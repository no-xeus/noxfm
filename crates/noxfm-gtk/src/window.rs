//! The browser window: one folder, as a list or a grid of icons.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_proto::{Entry, EntryKind, Event, Request, Response};

use crate::cells::{Cells, GRID_ICONS, GRID_ZOOM, LIST_ICONS, LIST_ZOOM, entry_of};
use crate::complete::Completion;
use crate::daemon::{Conn, Daemon};
use crate::grid;
use crate::list::ListView;

/// Back/forward entries kept.
const HISTORY_DEPTH: usize = 50;

/// Keyboard shortcuts of the window actions.
pub fn set_accels(app: &gtk::Application) {
    let accels: [(&str, &[&str]); 12] = [
        ("win.back", &["<Alt>Left"]),
        ("win.forward", &["<Alt>Right"]),
        ("win.up", &["<Alt>Up"]),
        ("win.reload", &["F5"]),
        ("win.focus-path", &["<Ctrl>l"]),
        ("win.view::list", &["<Ctrl>1"]),
        ("win.view::grid", &["<Ctrl>2"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
        ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
        ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
        ("win.show-hidden", &["<Ctrl>h"]),
        ("win.toggle-view", &[]),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    List,
    Grid,
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
    completion: Rc<Completion>,
    /// Entries (as `BoxedAnyObject`s) in daemon order; hidden ones are
    /// filtered out and the rest sorted before `selection`.
    store: gio::ListStore,
    hidden_filter: gtk::CustomFilter,
    show_hidden: Rc<Cell<bool>>,
    selection: gtk::MultiSelection,
    list: ListView,
    grid: gtk::GridView,
    views: gtk::Stack,
    mode: Cell<ViewMode>,
    cells: Rc<Cells>,
    status: gtk::Label,
    state: RefCell<State>,
}

/// `/a/b/`, as typed in the path bar.
fn display(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder().child(child).vexpand(true).build()
}

fn view_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let views = gio::Menu::new();
    views.append(Some("List"), Some("win.view::list"));
    views.append(Some("Icons"), Some("win.view::grid"));
    menu.append_section(None, &views);
    let zoom = gio::Menu::new();
    zoom.append(Some("Larger items"), Some("win.zoom-in"));
    zoom.append(Some("Smaller items"), Some("win.zoom-out"));
    zoom.append(Some("Default size"), Some("win.zoom-reset"));
    menu.append_section(None, &zoom);
    let show = gio::Menu::new();
    show.append(Some("Hidden files"), Some("win.show-hidden"));
    show.append(Some("Date created"), Some("win.show-created"));
    show.append(Some("Owner and permissions"), Some("win.show-details"));
    menu.append_section(Some("Show"), &show);
    menu
}

impl Browser {
    pub fn open(app: &gtk::Application, daemon: Daemon, conn: async_channel::Receiver<Conn>, start: PathBuf) {
        let cells = Cells::new(daemon.clone());
        let list = ListView::new(&cells);
        let grid = grid::new(&cells);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let show_hidden = Rc::new(Cell::new(false));
        let shown = show_hidden.clone();
        let hidden_filter = gtk::CustomFilter::new(move |o| shown.get() || !entry_of(o).hidden);
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(hidden_filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(list.sorter.clone()));
        let selection = gtk::MultiSelection::new(Some(sorted));
        // Only the shown view holds the model, so the other builds no items.
        list.view.set_model(Some(&selection));

        let views = gtk::Stack::new();
        views.add_named(&scrolled(&list.view), Some("list"));
        views.add_named(&scrolled(&grid), Some("grid"));

        let path_bar = gtk::Entry::builder().hexpand(true).build();
        let completion = Completion::attach(&path_bar, daemon.clone());
        let status = gtk::Label::builder().xalign(0.0).margin_start(8).margin_end(8).margin_top(4).margin_bottom(4).build();
        status.add_css_class("caption");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::builder().margin_start(6).margin_end(6).margin_top(6).margin_bottom(6).build();
        bar.append(&path_bar);
        content.append(&bar);
        content.append(&views);
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
        let menu = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&view_menu()).tooltip_text("View").build();
        header.pack_end(&menu);
        let toggle = gtk::Button::builder()
            .icon_name("view-grid-symbolic")
            .action_name("win.toggle-view")
            .tooltip_text("Show as icons (Ctrl+2)")
            .build();
        header.pack_end(&toggle);

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
            completion,
            store,
            hidden_filter,
            show_hidden,
            selection,
            list,
            grid,
            views,
            mode: Cell::new(ViewMode::List),
            cells,
            status,
            state: RefCell::new(State { path: start.clone(), ..Default::default() }),
        });
        this.completion.set_text_quietly(&display(&start));
        this.status.set_text("Connecting to noxd…");
        this.install_actions(&toggle);
        this.connect_signals();

        this.window.present();
        this.list.view.grab_focus();
        // This loop owns the browser: it runs as long as the process (one
        // window per process; closing it quits the application).
        glib::spawn_future_local(async move {
            while let Ok(c) = conn.recv().await {
                this.on_conn(c);
            }
        });
    }

    fn add_action(self: &Rc<Self>, a: &gio::SimpleAction, run: impl Fn(&Rc<Self>, &gio::SimpleAction, Option<&glib::Variant>) + 'static) {
        let weak = Rc::downgrade(self);
        a.connect_activate(move |a, param| {
            if let Some(b) = weak.upgrade() {
                run(&b, a, param);
            }
        });
        self.window.add_action(a);
    }

    fn install_actions(self: &Rc<Self>, toggle: &gtk::Button) {
        type Run = fn(&Rc<Browser>);
        let plain: [(&str, Run); 8] = [
            ("back", Self::go_back),
            ("forward", Self::go_forward),
            ("up", Self::go_up),
            ("reload", Self::reload),
            ("focus-path", |b| {
                b.path_bar.grab_focus();
            }),
            ("zoom-in", |b| b.zoom(1)),
            ("zoom-out", |b| b.zoom(-1)),
            ("zoom-reset", |b| b.zoom(0)),
        ];
        for (name, run) in plain {
            self.add_action(&gio::SimpleAction::new(name, None), move |b, _, _| run(b));
        }

        let view = gio::SimpleAction::new_stateful("view", Some(glib::VariantTy::STRING), &"list".to_variant());
        let toggle = toggle.clone();
        self.add_action(&view, move |b, a, param| {
            let Some(p) = param else { return };
            a.set_state(p);
            let grid = p.str() == Some("grid");
            b.set_view(if grid { ViewMode::Grid } else { ViewMode::List });
            toggle.set_icon_name(if grid { "view-list-symbolic" } else { "view-grid-symbolic" });
            toggle.set_tooltip_text(Some(if grid { "Show as list (Ctrl+1)" } else { "Show as icons (Ctrl+2)" }));
        });
        self.add_action(&gio::SimpleAction::new("toggle-view", None), |b, _, _| {
            let next = if b.mode.get() == ViewMode::List { "grid" } else { "list" };
            WidgetExt::activate_action(&b.window, "win.view", Some(&next.to_variant())).ok();
        });

        // On/off options, shown as check items in the View menu.
        type Toggle = fn(&Browser, bool);
        let toggles: [(&str, Toggle); 3] = [
            ("show-hidden", |b, on| {
                b.show_hidden.set(on);
                b.hidden_filter.changed(if on { gtk::FilterChange::LessStrict } else { gtk::FilterChange::MoreStrict });
                b.update_status();
            }),
            ("show-created", |b, on| b.list.created.set_visible(on)),
            ("show-details", |b, on| {
                b.list.owner.set_visible(on);
                b.list.permissions.set_visible(on);
            }),
        ];
        for (name, apply) in toggles {
            let a = gio::SimpleAction::new_stateful(name, None, &false.to_variant());
            self.add_action(&a, move |b, a, _| {
                let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                a.set_state(&on.to_variant());
                apply(b, on);
            });
        }
        self.update_history_actions();
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.view.connect_activate(move |_, pos| {
            if let Some(b) = weak.upgrade() {
                b.activate(pos);
            }
        });
        let weak = Rc::downgrade(self);
        self.grid.connect_activate(move |_, pos| {
            if let Some(b) = weak.upgrade() {
                b.activate(pos);
            }
        });

        for view in [self.list.view.upcast_ref::<gtk::Widget>(), self.grid.upcast_ref()] {
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

            // Ctrl + wheel zooms, before the scrolled window scrolls.
            let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(self);
            wheel.connect_scroll(move |c, _, dy| {
                if !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) || dy == 0.0 {
                    return glib::Propagation::Proceed;
                }
                if let Some(b) = weak.upgrade() {
                    b.zoom(if dy < 0.0 { 1 } else { -1 });
                }
                glib::Propagation::Stop
            });
            view.parent().expect("views are in scrolled windows").add_controller(wheel);
        }

        let weak = Rc::downgrade(self);
        self.path_bar.connect_activate(move |entry| {
            let Some(b) = weak.upgrade() else { return };
            b.completion.close();
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

    fn set_view(&self, mode: ViewMode) {
        if self.mode.replace(mode) == mode {
            return;
        }
        let (name, focus): (&str, &gtk::Widget) = match mode {
            ViewMode::List => {
                self.grid.set_model(None::<&gtk::MultiSelection>);
                self.list.view.set_model(Some(&self.selection));
                ("list", self.list.view.upcast_ref())
            }
            ViewMode::Grid => {
                self.list.view.set_model(None::<&gtk::MultiSelection>);
                self.grid.set_model(Some(&self.selection));
                ("grid", self.grid.upcast_ref())
            }
        };
        self.views.set_visible_child_name(name);
        focus.grab_focus();
    }

    /// One step bigger (`1`), smaller (`-1`), or back to the default (`0`),
    /// in the current view.
    fn zoom(&self, step: i32) {
        let (level, sizes, default) = match self.mode.get() {
            ViewMode::List => (&self.cells.list_zoom, LIST_ICONS.len(), LIST_ZOOM),
            ViewMode::Grid => (&self.cells.grid_zoom, GRID_ICONS.len(), GRID_ZOOM),
        };
        let new = match step {
            0 => default,
            s => level.get().saturating_add_signed(s as isize).min(sizes - 1),
        };
        if level.replace(new) == new {
            return;
        }
        match self.mode.get() {
            ViewMode::List => self.list.zoomed(),
            ViewMode::Grid => grid::zoomed(&self.grid, &self.cells),
        }
    }

    fn on_conn(self: &Rc<Self>, c: Conn) {
        match c {
            Conn::Connected => {
                log::debug!("connected to noxd");
                let path = {
                    let mut st = self.state.borrow_mut();
                    // The new connection watches nothing yet.
                    st.subscribed = None;
                    st.path.clone()
                };
                self.load(path, Nav::Reload);
            }
            Conn::Lost(why) => {
                log::debug!("noxd lost: {why}");
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
        self.cells.size_updated(path, bytes);
        if self.list.sorted_by_size()
            && let Some(s) = self.list.size.sorter()
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
        if !same {
            self.cells.forget_thumbnails();
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

        // Don't overwrite what the user is typing over a mere refresh.
        if !same || !self.path_bar.has_focus() {
            self.completion.set_text_quietly(&display(&path));
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
        let n = self.selection.n_items();
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
