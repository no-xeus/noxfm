//! One tab: a location (a folder, Recent or the Trash) shown as a list or a
//! grid, with its own history, selection and folder watch.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::{SortKey, fmt};
use noxfm_proto::{AppRef, Entry, EntryKind, RecentItem, RecentKind, Request, Response, TrashEntry};

use super::{Browser, ViewMode};
use crate::cells::{Cells, GRID_ICONS, GRID_ZOOM, LIST_ICONS, LIST_ZOOM, entry_of};
use crate::daemon::Daemon;
use crate::grid;
use crate::list::ListView;

/// Back/forward entries kept per tab.
const HISTORY_DEPTH: usize = 50;
/// Recent items listed at most.
const RECENT_LIMIT: u32 = 500;

/// Where a tab is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Loc {
    Dir(PathBuf),
    Recent(Option<RecentKind>),
    Trash,
}

impl Loc {
    pub(super) fn title(&self) -> String {
        match self {
            Loc::Dir(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
            Loc::Recent(None) => "Recent".into(),
            Loc::Recent(Some(RecentKind::Downloaded)) => "Recent · Downloaded".into(),
            Loc::Recent(Some(RecentKind::Modified)) => "Recent · Edited".into(),
            Loc::Recent(Some(RecentKind::Created)) => "Recent · Created".into(),
            Loc::Trash => "Trash".into(),
        }
    }

    /// For the path bar: the folder with a trailing slash, or the title.
    pub(super) fn display(&self) -> String {
        match self {
            Loc::Dir(p) => {
                let s = p.display().to_string();
                if s.ends_with('/') { s } else { format!("{s}/") }
            }
            other => other.title(),
        }
    }

    pub(super) fn to_start_view(&self) -> (Option<PathBuf>, Option<noxfm_proto::StartView>) {
        match self {
            Loc::Dir(p) => (Some(p.clone()), None),
            Loc::Recent(k) => (None, Some(noxfm_proto::StartView::Recent(*k))),
            Loc::Trash => (None, Some(noxfm_proto::StartView::Trash)),
        }
    }
}

/// How a listing came to be shown, for the history.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Nav {
    New,
    Back,
    Forward,
    /// The same place again (changed, reconnected, followed a move).
    Reload,
}

/// View options, the same in every tab of a window.
#[derive(Clone, Copy)]
pub(super) struct ViewOpts {
    pub mode: ViewMode,
    pub show_hidden: bool,
    pub show_created: bool,
    pub show_details: bool,
    pub list_zoom: usize,
    pub grid_zoom: usize,
}

impl Default for ViewOpts {
    fn default() -> Self {
        ViewOpts { mode: ViewMode::List, show_hidden: false, show_created: false, show_details: false, list_zoom: LIST_ZOOM, grid_zoom: GRID_ZOOM }
    }
}

enum Listing {
    Dir { path: PathBuf, fs: Option<String>, entries: Vec<Entry> },
    Recent(Option<RecentKind>, Vec<(RecentItem, Entry)>),
    Trash(Vec<(TrashEntry, Entry)>),
}

pub(super) struct State {
    pub loc: Loc,
    /// The folder shown, or the last one before Recent or the Trash.
    pub path: PathBuf,
    pub back: Vec<Loc>,
    pub forward: Vec<Loc>,
    /// Folder the daemon watches for this tab.
    pub subscribed: Option<PathBuf>,
    pub fs: Option<String>,
    pub error: Option<String>,
    /// What just happened ("Copied 3 items"), until another place is shown.
    pub notice: Option<String>,
    /// Trash details by the trashed item's path.
    pub trash: HashMap<PathBuf, TrashEntry>,
    /// The folder sort, while Recent shows newest first.
    sort_before_recent: Option<(SortKey, bool)>,
    /// Shown once already (the first listing is never "the same place").
    listed: bool,
}

pub(super) struct Pane {
    owner: Weak<Browser>,
    pub(super) daemon: Daemon,
    /// The tab's page in the notebook.
    pub(super) root: gtk::Stack,
    /// Entries (as `BoxedAnyObject`s) in daemon order; hidden ones are
    /// filtered out and the rest sorted before `selection`.
    pub(super) store: gio::ListStore,
    hidden_filter: gtk::CustomFilter,
    show_hidden: Rc<Cell<bool>>,
    pub(super) selection: gtk::MultiSelection,
    pub(super) list: ListView,
    pub(super) grid: gtk::GridView,
    pub(super) mode: Cell<ViewMode>,
    pub(super) cells: Rc<Cells>,
    pub(super) state: RefCell<State>,
    /// Context menus, one per view (popovers are parented to it).
    pub(super) list_menu: gtk::PopoverMenu,
    pub(super) grid_menu: gtk::PopoverMenu,
    /// Apps for "Open with ▸", for one MIME type (the selected file's).
    pub(super) open_with: RefCell<(String, Vec<AppRef>)>,
    /// Rename / select this as soon as the listing shows it.
    pub(super) pending_rename: RefCell<Option<PathBuf>>,
    pub(super) pending_select: RefCell<Option<PathBuf>>,
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder().child(child).vexpand(true).hexpand(true).build()
}

fn context_popover(parent: &impl IsA<gtk::Widget>) -> gtk::PopoverMenu {
    let p = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
    p.set_has_arrow(false);
    p.set_halign(gtk::Align::Start);
    p.set_parent(parent);
    p
}

impl Pane {
    pub(super) fn new(owner: &Rc<Browser>, loc: Loc, opts: ViewOpts) -> Rc<Pane> {
        let daemon = owner.daemon.clone();
        let cells = Cells::new(daemon.clone());
        cells.list_zoom.set(opts.list_zoom);
        cells.grid_zoom.set(opts.grid_zoom);
        let list = ListView::new(&cells);
        list.created.set_visible(opts.show_created);
        list.owner.set_visible(opts.show_details);
        list.permissions.set_visible(opts.show_details);
        let grid = grid::new(&cells);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let show_hidden = Rc::new(Cell::new(opts.show_hidden));
        let shown = show_hidden.clone();
        let hidden_filter = gtk::CustomFilter::new(move |o| shown.get() || !entry_of(o).hidden);
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(hidden_filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(list.sorter.clone()));
        let selection = gtk::MultiSelection::new(Some(sorted));

        let root = gtk::Stack::new();
        root.add_named(&scrolled(&list.view), Some("list"));
        root.add_named(&scrolled(&grid), Some("grid"));

        let path = match &loc {
            Loc::Dir(p) => p.clone(),
            _ => noxfm_core::complete::home_dir(),
        };
        let (list_menu, grid_menu) = (context_popover(&list.view), context_popover(&grid));
        let pane = Rc::new(Pane {
            owner: Rc::downgrade(owner),
            daemon,
            root,
            store,
            hidden_filter,
            show_hidden,
            selection,
            list,
            grid,
            // Set by `set_view` below.
            mode: Cell::new(if opts.mode == ViewMode::List { ViewMode::Grid } else { ViewMode::List }),
            cells,
            state: RefCell::new(State {
                loc,
                path,
                back: Vec::new(),
                forward: Vec::new(),
                subscribed: None,
                fs: None,
                error: None,
                notice: None,
                trash: HashMap::new(),
                sort_before_recent: None,
                listed: false,
            }),
            list_menu,
            grid_menu,
            open_with: RefCell::default(),
            pending_rename: RefCell::default(),
            pending_select: RefCell::default(),
        });
        pane.set_view(opts.mode);
        pane.connect_signals();
        pane
    }

    pub(super) fn browser(&self) -> Rc<Browser> {
        self.owner.upgrade().expect("panes live in their window")
    }

    fn connect_signals(self: &Rc<Self>) {
        for view in [self.list.view.upcast_ref::<gtk::Widget>(), self.grid.upcast_ref()] {
            // Backspace goes up, except while typing in the path bar.
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::BackSpace
                    && let Some(p) = weak.upgrade()
                {
                    p.go_up();
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
                if let Some(p) = weak.upgrade() {
                    p.browser().zoom(if dy < 0.0 { 1 } else { -1 });
                }
                glib::Propagation::Stop
            });
            view.parent().expect("views are in scrolled windows").add_controller(wheel);

            // Middle click on a folder opens it in a new tab.
            let middle = gtk::GestureClick::new();
            middle.set_button(gdk::BUTTON_MIDDLE);
            let (weak, v) = (Rc::downgrade(self), view.clone());
            middle.connect_pressed(move |_, _, x, y| {
                let Some(p) = weak.upgrade() else { return };
                if let Some(path) = p.cells.path_at(&v, x, y).filter(|d| p.entry(d).is_some_and(|e| e.kind == EntryKind::Dir)) {
                    p.browser().open_tab(Loc::Dir(path), false);
                }
            });
            view.add_controller(middle);
        }

        let weak = Rc::downgrade(self);
        self.list.view.connect_activate(move |_, pos| {
            if let Some(p) = weak.upgrade() {
                p.activate(pos);
            }
        });
        let weak = Rc::downgrade(self);
        self.grid.connect_activate(move |_, pos| {
            if let Some(p) = weak.upgrade() {
                p.activate(pos);
            }
        });
        let weak = Rc::downgrade(self);
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(p) = weak.upgrade() {
                p.changed();
            }
        });
    }

    /// Tells the window this tab's title, status or history changed.
    pub(super) fn changed(self: &Rc<Self>) {
        if let Some(b) = self.owner.upgrade() {
            b.pane_changed(self);
        }
    }

    pub(super) fn loc(&self) -> Loc {
        self.state.borrow().loc.clone()
    }

    /// The folder shown, or the last one (Recent and Trash aren't folders).
    pub(super) fn here(&self) -> PathBuf {
        self.state.borrow().path.clone()
    }

    pub(super) fn in_folder(&self) -> bool {
        matches!(self.state.borrow().loc, Loc::Dir(_))
    }

    pub(super) fn current_view(&self) -> gtk::Widget {
        match self.mode.get() {
            ViewMode::List => self.list.view.clone().upcast(),
            ViewMode::Grid => self.grid.clone().upcast(),
        }
    }

    pub(super) fn current_menu(&self) -> gtk::PopoverMenu {
        match self.mode.get() {
            ViewMode::List => self.list_menu.clone(),
            ViewMode::Grid => self.grid_menu.clone(),
        }
    }

    pub(super) fn set_view(&self, mode: ViewMode) {
        if self.mode.replace(mode) == mode {
            return;
        }
        // Only the shown view holds the model, so the other builds no items.
        let name = match mode {
            ViewMode::List => {
                self.grid.set_model(None::<&gtk::MultiSelection>);
                self.list.view.set_model(Some(&self.selection));
                "list"
            }
            ViewMode::Grid => {
                self.list.view.set_model(None::<&gtk::MultiSelection>);
                self.grid.set_model(Some(&self.selection));
                "grid"
            }
        };
        self.root.set_visible_child_name(name);
    }

    pub(super) fn apply_opts(&self, o: ViewOpts) {
        self.set_view(o.mode);
        if self.show_hidden.replace(o.show_hidden) != o.show_hidden {
            self.hidden_filter.changed(if o.show_hidden { gtk::FilterChange::LessStrict } else { gtk::FilterChange::MoreStrict });
        }
        self.list.created.set_visible(o.show_created);
        self.list.owner.set_visible(o.show_details);
        self.list.permissions.set_visible(o.show_details);
        if self.cells.list_zoom.replace(o.list_zoom) != o.list_zoom {
            self.list.zoomed();
        }
        if self.cells.grid_zoom.replace(o.grid_zoom) != o.grid_zoom {
            grid::zoomed(&self.grid, &self.cells);
        }
    }

    /// Lists `loc` (watching folders); shown when the answer arrives.
    pub(super) fn load(self: &Rc<Self>, loc: Loc, nav: Nav) {
        let req = match &loc {
            Loc::Dir(path) => Request::ListDir { path: path.clone(), watch: true },
            Loc::Recent(kind) => Request::Recent { kind: *kind, limit: RECENT_LIMIT },
            Loc::Trash => Request::ListTrash,
        };
        let weak: Weak<Self> = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            let reply = daemon.request(req).await;
            let Some(this) = weak.upgrade() else { return };
            let listing = match (reply, &loc) {
                (Ok(Response::Dir { path, fs, entries }), _) => Listing::Dir { path, fs, entries },
                (Ok(Response::Recent(items)), Loc::Recent(k)) => Listing::Recent(*k, items),
                (Ok(Response::TrashItems(items)), _) => Listing::Trash(items),
                (Ok(other), _) => return this.fail(format!("unexpected reply: {other:?}")),
                (Err(e), _) => return this.fail(e),
            };
            this.show(listing, nav);
        });
    }

    fn show(self: &Rc<Self>, listing: Listing, nav: Nav) {
        let (loc, fs, entries, captions, trash) = match listing {
            Listing::Dir { path, fs, entries } => (Loc::Dir(path), fs, entries, HashMap::new(), HashMap::new()),
            Listing::Recent(kind, items) => {
                let captions = items.iter().map(|(r, e)| (e.path.clone(), recent_caption(r))).collect();
                (Loc::Recent(kind), None, items.into_iter().map(|(_, e)| e).collect(), captions, HashMap::new())
            }
            Listing::Trash(items) => {
                let captions = items.iter().map(|(t, e)| (e.path.clone(), trash_caption(t))).collect();
                let trash = items.iter().map(|(t, e)| (e.path.clone(), t.clone())).collect();
                (Loc::Trash, None, items.into_iter().map(|(_, e)| e).collect(), captions, trash)
            }
        };
        log::debug!("listed {loc:?} ({} entries)", entries.len());

        let (same, old_watch, sort) = {
            let mut st = self.state.borrow_mut();
            let st = &mut *st;
            let first = !std::mem::replace(&mut st.listed, true);
            let same = !first && loc == st.loc;
            let was_recent = !first && matches!(st.loc, Loc::Recent(_));
            if !same {
                let old = std::mem::replace(&mut st.loc, loc.clone());
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
                st.notice = None;
            }
            st.fs = fs;
            st.error = None;
            st.trash = trash;
            // A folder listing subscribed us to it; drop the previous watch.
            let old_watch = match &loc {
                Loc::Dir(path) => {
                    st.path = path.clone();
                    st.subscribed.replace(path.clone()).filter(|o| o != path)
                }
                _ => st.subscribed.take(),
            };
            // Recent stays newest-first (daemon order) until a column is clicked.
            let is_recent = matches!(loc, Loc::Recent(_));
            let sort = match (was_recent, is_recent) {
                (false, true) => {
                    st.sort_before_recent = Some(self.list.sorting());
                    Some(None)
                }
                (true, false) => Some(st.sort_before_recent.take()),
                _ => None,
            };
            (same, old_watch, sort)
        };
        if let Some(old) = old_watch.filter(|o| !self.browser().watched_by_other(o, self)) {
            self.fire(Request::Unsubscribe { path: old });
        }
        match sort {
            Some(None) => self.list.unsort(),
            Some(Some((key, asc))) => self.list.sort(key, asc),
            None => {}
        }
        if !same {
            self.cells.forget_thumbnails();
        }
        self.cells.set_captions(captions);

        // A relisting of the same place keeps the selection.
        let keep = if same { self.selected_list().into_iter().collect() } else { HashSet::new() };
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
        self.apply_pending();
        self.apply_pending_select();
        self.changed();
    }

    pub(super) fn fail(self: &Rc<Self>, error: String) {
        self.state.borrow_mut().error = Some(error);
        self.changed();
    }

    /// A short note in the status line (until another place is shown).
    pub(super) fn notify(self: &Rc<Self>, msg: impl Into<String>) {
        self.state.borrow_mut().notice = Some(msg.into());
        self.changed();
    }

    pub(super) fn reload(self: &Rc<Self>) {
        self.load(self.loc(), Nav::Reload);
    }

    pub(super) fn go_back(self: &Rc<Self>) {
        let target = self.state.borrow().back.last().cloned();
        if let Some(l) = target {
            self.load(l, Nav::Back);
        }
    }

    pub(super) fn go_forward(self: &Rc<Self>) {
        let target = self.state.borrow().forward.last().cloned();
        if let Some(l) = target {
            self.load(l, Nav::Forward);
        }
    }

    /// From Recent or the Trash, "up" goes back to the folder before.
    pub(super) fn go_up(self: &Rc<Self>) {
        let target = match self.loc() {
            Loc::Dir(p) => p.parent().map(|p| Loc::Dir(p.to_path_buf())),
            _ => Some(Loc::Dir(self.here())),
        };
        if let Some(l) = target {
            self.load(l, Nav::New);
        }
    }

    /// Double-click or Enter: folders open in place, files with their app.
    fn activate(self: &Rc<Self>, pos: u32) {
        let Some(obj) = self.selection.item(pos) else { return };
        let (path, is_dir) = {
            let e = entry_of(&obj);
            (e.path.clone(), e.kind == EntryKind::Dir)
        };
        if self.loc() == Loc::Trash {
            self.notify("Restore it to open it");
        } else if is_dir {
            self.load(Loc::Dir(path), Nav::New);
        } else {
            self.fire(Request::OpenWith { path, app: None });
        }
    }

    pub(super) fn size_updated(&self, path: &Path, bytes: u64) {
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

    /// Items were renamed or moved: follow the shown folder and history.
    pub(super) fn follow_moves(self: &Rc<Self>, moves: &[(PathBuf, PathBuf)]) {
        let relocate = |p: &Path| noxfm_core::moves::relocated(p, moves);
        let new = {
            let mut st = self.state.borrow_mut();
            let st = &mut *st;
            for l in st.back.iter_mut().chain(st.forward.iter_mut()) {
                if let Loc::Dir(p) = l
                    && let Some(n) = relocate(p)
                {
                    *p = n;
                }
            }
            if let Some(n) = relocate(&st.path) {
                st.path = n;
            }
            let Loc::Dir(p) = &st.loc else { return };
            let Some(new) = relocate(p) else { return };
            // Already the current place, so the listing isn't recorded as a move.
            st.loc = Loc::Dir(new.clone());
            new
        };
        self.load(Loc::Dir(new), Nav::Reload);
    }

    pub(super) fn status_text(&self) -> String {
        let st = self.state.borrow();
        let n = self.selection.n_items();
        let mut parts = vec![format!("{n} item{}", if n == 1 { "" } else { "s" })];
        let selected = self.selected_entries();
        match selected.as_slice() {
            [] => {}
            [e] => parts.push(format!("“{}” selected", e.name)),
            many => {
                let bytes: u64 = many.iter().filter_map(|e| e.size).sum();
                parts.push(format!("{} selected ({})", many.len(), fmt::size(bytes)));
            }
        }
        parts.extend(st.notice.clone());
        parts.extend(st.error.clone());
        parts.extend(st.fs.clone());
        parts.join("  ·  ")
    }

    /// Sends `req`; `then` gets the reply (errors go to the status line).
    pub(super) fn request_then(self: &Rc<Self>, req: Request, then: impl FnOnce(&Rc<Self>, Response) + 'static) {
        let weak = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            let reply = daemon.request(req).await;
            let Some(this) = weak.upgrade() else { return };
            match reply {
                Ok(r) => then(&this, r),
                Err(e) => this.fail(e),
            }
        });
    }

    /// A request whose only interesting outcome is an error to show.
    pub(super) fn fire(self: &Rc<Self>, req: Request) {
        self.request_then(req, |_, _| {});
    }
}

fn recent_caption(r: &RecentItem) -> String {
    let why = match r.kind {
        RecentKind::Downloaded => "downloaded",
        RecentKind::Modified => "edited",
        RecentKind::Created => "created",
    };
    let folder = r.path.parent().map(fmt::short_path).unwrap_or_default();
    format!("{why} {} · {}", fmt::ago(r.at), fmt::ellipsize_start(&folder, 40))
}

fn trash_caption(t: &TrashEntry) -> String {
    let from = t.original_path.parent().map(fmt::short_path).unwrap_or_default();
    let mut s = format!("from {} · deleted {}", fmt::ellipsize_start(&from, 32), fmt::ago(t.deleted_at));
    if let Some(at) = t.purge_at {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let days = ((at - now) as f64 / 86_400.0).ceil().max(0.0) as i64;
        s += &format!(" · gone in {days} day{}", if days == 1 { "" } else { "s" });
    }
    s
}

/// Zoom steps per view, for `Browser::zoom`.
pub(super) fn zoom_range(mode: ViewMode) -> (usize, usize) {
    match mode {
        ViewMode::List => (LIST_ICONS.len(), LIST_ZOOM),
        ViewMode::Grid => (GRID_ICONS.len(), GRID_ZOOM),
    }
}
