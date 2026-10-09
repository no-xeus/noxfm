//! The browser window: one folder, listed with sortable columns.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::{SortKey, fmt};
use noxfm_proto::{Entry, EntryKind, Event, Request, Response};

use crate::daemon::{Conn, Daemon};

/// Back/forward entries kept.
const HISTORY_DEPTH: usize = 50;
const ICON_PX: i32 = 24;

/// Keyboard shortcuts of the window actions.
pub fn set_accels(app: &gtk::Application) {
    let accels: [(&str, &[&str]); 5] = [
        ("win.back", &["<Alt>Left"]),
        ("win.forward", &["<Alt>Right"]),
        ("win.up", &["<Alt>Up"]),
        ("win.reload", &["F5"]),
        ("win.focus-path", &["<Ctrl>l"]),
    ];
    for (action, keys) in accels {
        app.set_accels_for_action(action, keys);
    }
}

/// How a listing came to be shown, for the history.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    New,
    Back,
    Forward,
    /// The same folder again (changed on disk, reconnected, followed a move).
    Reload,
}

#[derive(Default)]
struct State {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Folder the daemon watches for us.
    subscribed: Option<PathBuf>,
    fs: Option<String>,
    error: Option<String>,
}

pub struct Browser {
    daemon: Daemon,
    window: gtk::ApplicationWindow,
    path_bar: gtk::Entry,
    /// Entries (as `BoxedAnyObject`s) in daemon order; `selection` sorts them.
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    status: gtk::Label,
    sorter: gtk::ColumnViewSorter,
    size_column: gtk::ColumnViewColumn,
    /// Size cells on screen, updated in place when a folder size arrives.
    size_labels: Rc<RefCell<HashMap<PathBuf, glib::WeakRef<gtk::Label>>>>,
    state: RefCell<State>,
}

fn entry_of(obj: &glib::Object) -> Ref<'_, Entry> {
    obj.downcast_ref::<glib::BoxedAnyObject>().expect("list items are entries").borrow::<Entry>()
}

/// `/a/b/`, as typed in the path bar.
fn display(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

fn size_text(e: &Entry) -> String {
    match (e.kind, e.size) {
        (_, Some(b)) => fmt::size(b),
        // Being measured. Symlinked folders are never walked.
        (EntryKind::Dir, None) if !e.symlink => "…".into(),
        _ => String::new(),
    }
}

fn label_factory(text: impl Fn(&Entry) -> String + 'static) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(&gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build()));
    });
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        label.set_text(&text(&entry_of(&item.item().unwrap())));
    });
    f
}

fn name_factory() -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Image::builder().pixel_size(ICON_PX).build());
        row.append(&gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build());
        item.set_child(Some(&row));
    });
    f.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().unwrap();
        let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
        let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        let names = fmt::icon_names(&e);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        icon.set_from_gicon(&gio::ThemedIcon::from_names(&names));
        label.set_text(&e.name);
    });
    f
}

/// Size cells register themselves so a size update can reach them.
fn size_factory(labels: &Rc<RefCell<HashMap<PathBuf, glib::WeakRef<gtk::Label>>>>) -> gtk::SignalListItemFactory {
    let f = label_factory(size_text);
    let bound = labels.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        bound.borrow_mut().insert(entry_of(&item.item().unwrap()).path.clone(), label.downgrade());
    });
    let unbound = labels.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(obj) = item.item() {
            unbound.borrow_mut().remove(&entry_of(&obj).path);
        }
    });
    f
}

/// A column sorted in noxfm's order: folders first and unknown values last
/// in both directions.
fn column(
    title: &str,
    key: SortKey,
    ascending: &Rc<Cell<bool>>,
    factory: gtk::SignalListItemFactory,
) -> gtk::ColumnViewColumn {
    let asc = ascending.clone();
    let sorter = gtk::CustomSorter::new(move |a, b| {
        let asc = asc.get();
        let order = noxfm_core::compare(&entry_of(a), &entry_of(b), key, asc);
        // GTK reverses a descending column's result; noxfm's order isn't a
        // plain reverse, so undo that.
        (if asc { order } else { order.reverse() }).into()
    });
    let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    col.set_sorter(Some(&sorter));
    col.set_resizable(true);
    col
}

impl Browser {
    pub fn open(app: &gtk::Application, daemon: Daemon, conn: async_channel::Receiver<Conn>, start: PathBuf) {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let view = gtk::ColumnView::new(None::<gtk::MultiSelection>);
        let ascending = Rc::new(Cell::new(true));
        let size_labels = Rc::default();

        let name_col = column("Name", SortKey::Name, &ascending, name_factory());
        name_col.set_expand(true);
        let type_col = column(
            "Type",
            SortKey::Extension,
            &ascending,
            label_factory(|e| e.extension().map(str::to_lowercase).unwrap_or_default()),
        );
        type_col.set_fixed_width(80);
        let size_column = column("Size", SortKey::Size, &ascending, size_factory(&size_labels));
        size_column.set_fixed_width(100);
        let modified_col = column("Modified", SortKey::Modified, &ascending, label_factory(|e| fmt::time(e.modified)));
        modified_col.set_fixed_width(150);
        for c in [&name_col, &type_col, &size_column, &modified_col] {
            view.append_column(c);
        }

        let sorter = view.sorter().and_downcast::<gtk::ColumnViewSorter>().expect("column view sorter");
        // Before the sort model connects, so the direction is current when it re-sorts.
        let asc = ascending.clone();
        sorter.connect_changed(move |s, _| asc.set(s.primary_sort_order() == gtk::SortType::Ascending));
        let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter.clone()));
        let selection = gtk::MultiSelection::new(Some(sorted));
        view.set_model(Some(&selection));
        view.sort_by_column(Some(&name_col), gtk::SortType::Ascending);

        let path_bar = gtk::Entry::builder().hexpand(true).build();
        let status = gtk::Label::builder().xalign(0.0).margin_start(8).margin_end(8).margin_top(4).margin_bottom(4).build();
        status.add_css_class("caption");
        let scroller = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::builder().margin_start(6).margin_end(6).margin_top(6).margin_bottom(6).build();
        bar.append(&path_bar);
        content.append(&bar);
        content.append(&scroller);
        content.append(&status);

        let header = gtk::HeaderBar::new();
        for (icon, action, tip) in [
            ("go-previous-symbolic", "win.back", "Back (Alt+Left)"),
            ("go-next-symbolic", "win.forward", "Forward (Alt+Right)"),
            ("go-up-symbolic", "win.up", "Up (Backspace)"),
        ] {
            let b = gtk::Button::from_icon_name(icon);
            b.set_action_name(Some(action));
            b.set_tooltip_text(Some(tip));
            header.pack_start(&b);
        }

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(1000)
            .default_height(700)
            .title("noxfm")
            .titlebar(&header)
            .child(&content)
            .build();

        let this = Rc::new(Browser {
            daemon,
            window,
            path_bar,
            store,
            selection,
            status,
            sorter,
            size_column,
            size_labels,
            state: RefCell::new(State { path: start.clone(), ..Default::default() }),
        });
        this.path_bar.set_text(&display(&start));
        this.status.set_text("Connecting to noxd…");
        this.install_actions();
        this.connect_signals(&view);

        this.window.present();
        // This loop owns the browser: it runs as long as the process (one
        // window per process; closing it quits the application).
        glib::spawn_future_local(async move {
            while let Ok(c) = conn.recv().await {
                this.on_conn(c);
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        type Run = fn(&Rc<Browser>);
        let actions: [(&str, Run); 5] = [
            ("back", Self::go_back),
            ("forward", Self::go_forward),
            ("up", Self::go_up),
            ("reload", Self::reload),
            ("focus-path", |b| {
                b.path_bar.grab_focus();
            }),
        ];
        for (name, run) in actions {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(b) = weak.upgrade() {
                    run(&b);
                }
            });
            self.window.add_action(&a);
        }
        self.update_history_actions();
    }

    fn connect_signals(self: &Rc<Self>, view: &gtk::ColumnView) {
        let weak = Rc::downgrade(self);
        view.connect_activate(move |_, pos| {
            if let Some(b) = weak.upgrade() {
                b.activate(pos);
            }
        });

        // Backspace goes up, except while typing in the path bar.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::BackSpace
                && let Some(b) = weak.upgrade()
            {
                b.go_up();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.path_bar.connect_activate(move |entry| {
            let Some(b) = weak.upgrade() else { return };
            let home = noxfm_core::complete::home_dir();
            let p = noxfm_core::complete::expand_tilde(entry.text().trim(), &home);
            b.load(PathBuf::from(p), Nav::New);
        });

        let weak = Rc::downgrade(self);
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(b) = weak.upgrade() {
                b.update_status();
            }
        });
    }

    fn on_conn(self: &Rc<Self>, c: Conn) {
        match &c {
            Conn::Connected => log::debug!("connected to noxd"),
            Conn::Lost(why) => log::debug!("noxd lost: {why}"),
            Conn::Event(ev) => log::debug!("event {ev:?}"),
        }
        match c {
            Conn::Connected => {
                let path = {
                    let mut st = self.state.borrow_mut();
                    // The new connection watches nothing yet.
                    st.subscribed = None;
                    st.path.clone()
                };
                self.load(path, Nav::Reload);
            }
            Conn::Lost(why) => {
                self.state.borrow_mut().error = Some(format!("noxd unavailable: {why}"));
                self.update_status();
            }
            Conn::Event(ev) => self.on_event(ev),
        }
    }

    fn on_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::DirChanged { path } if path == self.state.borrow().path => self.load(path, Nav::Reload),
            Event::SizeUpdated { path, bytes } => self.size_updated(&path, bytes),
            Event::Moved(moves) => {
                let relocate = |p: &Path| noxfm_core::moves::relocated(p, &moves);
                let new = {
                    let mut st = self.state.borrow_mut();
                    let st = &mut *st;
                    for p in st.back.iter_mut().chain(st.forward.iter_mut()) {
                        if let Some(n) = relocate(p) {
                            *p = n;
                        }
                    }
                    let Some(new) = relocate(&st.path) else { return };
                    st.path = new.clone();
                    new
                };
                self.load(new, Nav::Reload);
            }
            _ => {}
        }
    }

    fn size_updated(&self, path: &Path, bytes: u64) {
        let Some(obj) = self.store.iter::<glib::BoxedAnyObject>().flatten().find(|o| o.borrow::<Entry>().path == path) else {
            return;
        };
        obj.borrow_mut::<Entry>().size = Some(bytes);
        let label = self.size_labels.borrow().get(path).and_then(|w| w.upgrade());
        if let Some(label) = label {
            label.set_text(&fmt::size(bytes));
        }
        if self.sorter.primary_sort_column().as_ref() == Some(&self.size_column)
            && let Some(s) = self.size_column.sorter()
        {
            s.changed(gtk::SorterChange::Different);
        }
    }

    /// Lists `path` (and watches it); shown when the answer arrives.
    fn load(self: &Rc<Self>, path: PathBuf, nav: Nav) {
        let weak: Weak<Self> = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            let reply = daemon.request(Request::ListDir { path, watch: true }).await;
            log::debug!("listing reply: {:?}", reply.as_ref().map(|_| ()));
            let Some(this) = weak.upgrade() else { return };
            match reply {
                Ok(Response::Dir { path, fs, entries }) => this.show(path, fs, entries, nav),
                Ok(other) => this.fail(format!("unexpected reply: {other:?}")),
                Err(e) => this.fail(e),
            }
        });
    }

    fn show(self: &Rc<Self>, path: PathBuf, fs: Option<String>, entries: Vec<Entry>, nav: Nav) {
        log::debug!("listed {} ({} entries)", path.display(), entries.len());
        let (same, old_watch) = {
            let mut st = self.state.borrow_mut();
            let same = path == st.path;
            if !same {
                let old = std::mem::replace(&mut st.path, path.clone());
                match nav {
                    Nav::New => {
                        st.back.push(old);
                        if st.back.len() > HISTORY_DEPTH {
                            st.back.remove(0);
                        }
                        st.forward.clear();
                    }
                    Nav::Back => {
                        st.back.pop();
                        st.forward.push(old);
                    }
                    Nav::Forward => {
                        st.forward.pop();
                        st.back.push(old);
                    }
                    Nav::Reload => {}
                }
            }
            st.fs = fs;
            st.error = None;
            // The listing subscribed us to `path`; drop the previous watch.
            let old_watch = st.subscribed.replace(path.clone()).filter(|o| *o != path);
            (same, old_watch)
        };
        if let Some(old) = old_watch {
            self.fire(Request::Unsubscribe { path: old });
        }

        // A relisting of the same folder keeps the selection.
        let keep = if same { self.selected_paths() } else { HashSet::new() };
        let objs: Vec<glib::BoxedAnyObject> = entries.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objs);
        if !keep.is_empty() {
            for pos in 0..self.selection.n_items() {
                let obj = self.selection.item(pos).unwrap();
                if keep.contains(&entry_of(&obj).path) {
                    self.selection.select_item(pos, false);
                }
            }
        }

        if !self.path_bar.has_focus() || !same {
            self.path_bar.set_text(&display(&path));
        }
        let title = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.window.set_title(Some(&title));
        self.update_history_actions();
        self.update_status();
    }

    fn fail(&self, error: String) {
        self.state.borrow_mut().error = Some(error);
        self.update_status();
    }

    fn selected_paths(&self) -> HashSet<PathBuf> {
        let set = self.selection.selection();
        (0..set.size())
            .filter_map(|i| self.selection.item(set.nth(i as u32)))
            .map(|o| entry_of(&o).path.clone())
            .collect()
    }

    /// Double-click or Enter: folders open in place, files with their app.
    fn activate(self: &Rc<Self>, pos: u32) {
        let Some(obj) = self.selection.item(pos) else { return };
        let (path, is_dir) = {
            let e = entry_of(&obj);
            (e.path.clone(), e.kind == EntryKind::Dir)
        };
        if is_dir {
            self.load(path, Nav::New);
        } else {
            self.fire(Request::OpenWith { path, app: None });
        }
    }

    fn go_back(self: &Rc<Self>) {
        let target = self.state.borrow().back.last().cloned();
        if let Some(p) = target {
            self.load(p, Nav::Back);
        }
    }

    fn go_forward(self: &Rc<Self>) {
        let target = self.state.borrow().forward.last().cloned();
        if let Some(p) = target {
            self.load(p, Nav::Forward);
        }
    }

    fn go_up(self: &Rc<Self>) {
        let parent = self.state.borrow().path.parent().map(Path::to_path_buf);
        if let Some(p) = parent {
            self.load(p, Nav::New);
        }
    }

    fn reload(self: &Rc<Self>) {
        let path = self.state.borrow().path.clone();
        self.load(path, Nav::Reload);
    }

    fn update_history_actions(&self) {
        let st = self.state.borrow();
        for (name, on) in [("back", !st.back.is_empty()), ("forward", !st.forward.is_empty()), ("up", st.path.parent().is_some())] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        }
    }

    fn update_status(&self) {
        let st = self.state.borrow();
        let n = self.store.n_items();
        let mut parts = vec![format!("{n} item{}", if n == 1 { "" } else { "s" })];
        let selected = self.selection.selection().size();
        if selected > 0 {
            parts.push(format!("{selected} selected"));
        }
        parts.extend(st.error.clone());
        parts.extend(st.fs.clone());
        self.status.set_text(&parts.join("  ·  "));
    }

    /// A request whose only interesting outcome is an error to show.
    fn fire(self: &Rc<Self>, req: Request) {
        let weak = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = daemon.request(req).await
                && let Some(this) = weak.upgrade()
            {
                this.fail(e);
            }
        });
    }
}
