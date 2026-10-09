//! The browser window: tabs (each a folder, Recent or the Trash), a
//! sidebar, the path bar and a status line.

mod apps;
mod dnd;
mod menu;
mod ops;
mod pane;
mod preview;
mod properties;
mod rename;
mod sidebar;
mod tabs;
mod transfers;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::jobs::Jobs;
use noxfm_proto::{Device, Event, HealthCheck, Place, Request, Response, StartView, WindowLayout};

use crate::complete::Completion;
use crate::daemon::{Conn, Daemon};
pub(crate) use pane::Loc;
use pane::{Nav, Pane, ViewOpts};

const SIDEBAR_W: i32 = 240;

/// Keyboard shortcuts of the window actions.
pub fn set_accels(app: &gtk::Application) {
    let accels: [(&str, &[&str]); 19] = [
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
        ("win.toggle-sidebar", &["F9"]),
        ("win.new-tab", &["<Ctrl>t"]),
        ("win.close-tab", &["<Ctrl>w"]),
        ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
        ("win.prev-tab", &["<Ctrl><Shift>Tab", "<Ctrl><Shift>ISO_Left_Tab", "<Ctrl>Page_Up"]),
        ("win.empty-trash", &[]),
        ("win.restore", &[]),
    ];
    for (action, keys) in accels {
        app.set_accels_for_action(action, keys);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    List,
    Grid,
}

pub(crate) struct Browser {
    daemon: Daemon,
    window: gtk::ApplicationWindow,
    notebook: gtk::Notebook,
    paned: gtk::Paned,
    /// Rebuilt whenever what it shows changes (see `sidebar.rs`).
    sidebar: gtk::Box,
    /// "Mount this?" questions, above the tabs.
    banners: gtk::Box,
    path_bar: gtk::Entry,
    completion: Rc<Completion>,
    status: gtk::Label,
    view_toggle: gtk::Button,
    panes: RefCell<Vec<Rc<Pane>>>,
    opts: Cell<ViewOpts>,
    /// Shown in the path bar; it isn't overwritten while typing unless the
    /// place changes.
    shown_loc: RefCell<Option<Loc>>,
    places: RefCell<Vec<Place>>,
    devices: RefCell<Vec<Device>>,
    /// Folded sidebar sections and disks (shared by all windows via noxd).
    collapsed: RefCell<HashSet<String>>,
    trash_count: Cell<u32>,
    templates: Vec<PathBuf>,
    undo_label: RefCell<Option<String>>,
    /// Items cut to the clipboard, dimmed in every tab.
    cut: RefCell<HashSet<PathBuf>>,
    /// Right-click menus of sidebar rows, unparented on each rebuild.
    sidebar_menus: RefCell<Vec<gtk::PopoverMenu>>,
    /// The preview panel (hidden unless asked for) and what it shows.
    preview: gtk::Box,
    preview_path: RefCell<Option<PathBuf>>,
    jobs: RefCell<Jobs>,
    /// The job list, its panel, and the status-line indicator opening it.
    transfers: gtk::Box,
    transfers_revealer: gtk::Revealer,
    transfers_button: gtk::Button,
    transfer_rows: RefCell<HashMap<u64, transfers::TransferRow>>,
    /// Removing finished jobs as they expire.
    ticking: Cell<bool>,
    /// Missing dependencies to tell about (empty once dismissed).
    missing: RefCell<Vec<HealthCheck>>,
}

const CSS: &str = "
.drop-target { background-color: alpha(@accent_bg_color, 0.25); border-radius: 6px; }
.sidebar-active { background-color: alpha(@accent_bg_color, 0.2); }
.ask-banner { padding: 8px; margin: 0 6px 6px 6px; border-radius: 8px; background-color: alpha(@accent_bg_color, 0.12); }
.badge { font-size: smaller; padding: 0 6px; border-radius: 6px; background-color: alpha(currentColor, 0.1); }
";

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
    show.append(Some("Sidebar"), Some("win.toggle-sidebar"));
    show.append(Some("Preview panel"), Some("win.preview"));
    menu.append_section(Some("Show"), &show);
    menu
}

fn header_button(icon: &str, action: &str, tip: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).build()
}

impl Browser {
    pub(crate) fn open(
        app: &gtk::Application,
        daemon: Daemon,
        conn: async_channel::Receiver<Conn>,
        start: Loc,
        layout: Option<WindowLayout>,
    ) -> Rc<Browser> {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }

        let path_bar = gtk::Entry::builder().hexpand(true).build();
        let completion = Completion::attach(&path_bar, daemon.clone());
        let status = gtk::Label::builder().xalign(0.0).margin_start(8).margin_end(8).margin_top(4).margin_bottom(4).build();
        status.add_css_class("caption");
        let notebook = gtk::Notebook::builder().show_border(false).scrollable(true).vexpand(true).hexpand(true).build();
        notebook.set_group_name(Some("noxfm-tabs"));
        let banners = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::builder().margin_start(6).margin_end(6).margin_top(6).margin_bottom(6).build();
        bar.append(&path_bar);
        let preview = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(380)
            .margin_start(8)
            .margin_end(8)
            .visible(false)
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&notebook);
        row.append(&preview);
        let transfers = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_start(12).margin_end(12).margin_top(8).margin_bottom(8).build();
        let transfers_revealer = gtk::Revealer::builder()
            .child(&gtk::ScrolledWindow::builder().child(&transfers).propagate_natural_height(true).max_content_height(240).build())
            .build();
        let transfers_button = gtk::Button::builder().visible(false).action_name("win.transfers").build();
        transfers_button.add_css_class("flat");
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status.set_hexpand(true);
        status_row.append(&status);
        status_row.append(&transfers_button);
        main.append(&bar);
        main.append(&banners);
        main.append(&row);
        main.append(&transfers_revealer);
        main.append(&status_row);

        let sidebar = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).margin_start(4).margin_end(4).build();
        let side_scroll = gtk::ScrolledWindow::builder().child(&sidebar).hscrollbar_policy(gtk::PolicyType::Never).build();
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&side_scroll)
            .end_child(&main)
            .resize_start_child(false)
            .shrink_start_child(false)
            .position(layout.map_or(SIDEBAR_W, |l| l.sidebar_width.round() as i32))
            .build();
        side_scroll.set_visible(layout.is_none_or(|l| l.sidebar_open));

        let header = gtk::HeaderBar::new();
        for (icon, action, tip) in [
            ("sidebar-show-symbolic", "win.toggle-sidebar", "Sidebar (F9)"),
            ("go-previous-symbolic", "win.back", "Back (Alt+Left)"),
            ("go-next-symbolic", "win.forward", "Forward (Alt+Right)"),
            ("go-up-symbolic", "win.up", "Up (Backspace)"),
            ("tab-new-symbolic", "win.new-tab", "New tab (Ctrl+T)"),
        ] {
            header.pack_start(&header_button(icon, action, tip));
        }
        let menu = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&view_menu()).tooltip_text("View").build();
        header.pack_end(&menu);
        let view_toggle = header_button("view-grid-symbolic", "win.toggle-view", "Show as icons (Ctrl+2)");
        header.pack_end(&view_toggle);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(1100)
            .default_height(720)
            .title("noxfm")
            .titlebar(&header)
            .child(&paned)
            .build();

        let this = Rc::new(Browser {
            daemon,
            window,
            notebook,
            paned,
            sidebar,
            banners,
            path_bar,
            completion,
            status,
            view_toggle,
            panes: RefCell::default(),
            opts: Cell::new(ViewOpts::default()),
            shown_loc: RefCell::default(),
            places: RefCell::default(),
            devices: RefCell::default(),
            collapsed: RefCell::default(),
            trash_count: Cell::new(0),
            templates: ops::load_templates(),
            undo_label: RefCell::default(),
            cut: RefCell::default(),
            sidebar_menus: RefCell::default(),
            preview,
            preview_path: RefCell::default(),
            jobs: RefCell::default(),
            transfers,
            transfers_revealer,
            transfers_button,
            transfer_rows: RefCell::default(),
            ticking: Cell::new(false),
            missing: RefCell::default(),
        });
        this.install_actions();
        this.install_file_actions();
        this.install_sidebar_actions();
        this.connect_signals();
        this.open_tab(start, true);
        this.status.set_text("Connecting to noxd…");
        this.refresh_sidebar();

        this.window.present();
        this.pane().current_view().grab_focus();
        // This loop owns the browser: it runs as long as the process (one
        // window per process; closing it quits the application).
        let owner = this.clone();
        glib::spawn_future_local(async move {
            while let Ok(c) = conn.recv().await {
                owner.on_conn(c);
            }
        });
        this
    }

    /// The active tab.
    fn pane(&self) -> Rc<Pane> {
        let page = self.notebook.current_page().and_then(|i| self.notebook.nth_page(Some(i)));
        let panes = self.panes.borrow();
        page.and_then(|w| panes.iter().find(|p| p.root.upcast_ref::<gtk::Widget>() == &w).cloned())
            .unwrap_or_else(|| panes[0].clone())
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

    fn install_actions(self: &Rc<Self>) {
        type Run = fn(&Rc<Browser>);
        let plain: [(&str, Run); 14] = [
            ("back", |b| b.pane().go_back()),
            ("forward", |b| b.pane().go_forward()),
            ("up", |b| b.pane().go_up()),
            ("reload", |b| b.pane().reload()),
            ("focus-path", |b| {
                b.path_bar.grab_focus();
            }),
            ("zoom-in", |b| b.zoom(1)),
            ("zoom-out", |b| b.zoom(-1)),
            ("zoom-reset", |b| b.zoom(0)),
            ("toggle-sidebar", |b| {
                let side = b.paned.start_child().expect("sidebar");
                side.set_visible(!side.is_visible());
            }),
            ("new-tab", |b| {
                b.open_tab(b.pane().loc(), true);
            }),
            ("close-tab", |b| b.close_tab(&b.pane())),
            ("next-tab", |b| b.cycle_tab(1)),
            ("prev-tab", |b| b.cycle_tab(-1)),
            ("transfers", |b| b.toggle_transfers()),
        ];
        for (name, run) in plain {
            self.add_action(&gio::SimpleAction::new(name, None), move |b, _, _| run(b));
        }

        let view = gio::SimpleAction::new_stateful("view", Some(glib::VariantTy::STRING), &"list".to_variant());
        self.add_action(&view, |b, a, param| {
            let Some(p) = param else { return };
            a.set_state(p);
            let grid = p.str() == Some("grid");
            b.set_opts(|o| o.mode = if grid { ViewMode::Grid } else { ViewMode::List });
            b.view_toggle.set_icon_name(if grid { "view-list-symbolic" } else { "view-grid-symbolic" });
            b.view_toggle.set_tooltip_text(Some(if grid { "Show as list (Ctrl+1)" } else { "Show as icons (Ctrl+2)" }));
            b.pane().current_view().grab_focus();
        });
        self.add_action(&gio::SimpleAction::new("toggle-view", None), |b, _, _| {
            let next = if b.opts.get().mode == ViewMode::List { "grid" } else { "list" };
            WidgetExt::activate_action(&b.window, "win.view", Some(&next.to_variant())).ok();
        });

        // On/off options, shown as check items in the View menu.
        let preview = gio::SimpleAction::new_stateful("preview", None, &false.to_variant());
        self.add_action(&preview, |b, a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            b.toggle_preview(on);
        });

        type Toggle = fn(&mut ViewOpts, bool);
        let toggles: [(&str, Toggle); 3] = [
            ("show-hidden", |o, on| o.show_hidden = on),
            ("show-created", |o, on| o.show_created = on),
            ("show-details", |o, on| o.show_details = on),
        ];
        for (name, apply) in toggles {
            let a = gio::SimpleAction::new_stateful(name, None, &false.to_variant());
            self.add_action(&a, move |b, a, _| {
                let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                a.set_state(&on.to_variant());
                b.set_opts(|o| apply(o, on));
            });
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.path_bar.connect_activate(move |entry| {
            let Some(b) = weak.upgrade() else { return };
            b.completion.close();
            let home = noxfm_core::complete::home_dir();
            let p = noxfm_core::complete::expand_tilde(entry.text().trim(), &home);
            b.pane().load(Loc::Dir(PathBuf::from(p)), Nav::New);
        });
        self.connect_tabs();
    }

    /// Changes view options in every tab.
    fn set_opts(self: &Rc<Self>, change: impl FnOnce(&mut ViewOpts)) {
        let mut o = self.opts.get();
        change(&mut o);
        self.opts.set(o);
        for p in self.panes.borrow().iter() {
            p.apply_opts(o);
        }
        self.sync_chrome();
    }

    /// One step bigger (`1`), smaller (`-1`), or back to the default (`0`),
    /// in the current view.
    fn zoom(self: &Rc<Self>, step: i32) {
        let o = self.opts.get();
        let (sizes, default) = pane::zoom_range(o.mode);
        let level = if o.mode == ViewMode::List { o.list_zoom } else { o.grid_zoom };
        let new = match step {
            0 => default,
            s => level.saturating_add_signed(s as isize).min(sizes - 1),
        };
        if new != level {
            self.set_opts(|o| match o.mode {
                ViewMode::List => o.list_zoom = new,
                ViewMode::Grid => o.grid_zoom = new,
            });
        }
    }

    fn set_cut(&self, paths: HashSet<PathBuf>) {
        for p in self.panes.borrow().iter() {
            p.cells.set_cut(paths.clone());
        }
        *self.cut.borrow_mut() = paths;
    }

    /// Another tab than `except` watches `path`.
    fn watched_by_other(&self, path: &Path, except: &Pane) -> bool {
        self.panes.borrow().iter().any(|p| !std::ptr::eq(p.as_ref(), except) && p.state.borrow().subscribed.as_deref() == Some(path))
    }

    /// What a window opened from this one starts with.
    fn layout(&self) -> WindowLayout {
        let side = self.paned.start_child().is_some_and(|s| s.is_visible());
        WindowLayout { sidebar_open: side, sidebar_width: self.paned.position() as f32 }
    }

    fn open_window(&self, loc: Loc) {
        let (path, view) = loc.to_start_view();
        let req = Request::OpenWindow { path, view, layout: Some(self.layout()) };
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = daemon.request(req).await {
                log::warn!("open window: {e}");
            }
        });
    }

    /// A tab's title, status or history changed.
    fn pane_changed(self: &Rc<Self>, pane: &Rc<Pane>) {
        self.update_tab_label(pane);
        if Rc::ptr_eq(pane, &self.pane()) {
            self.sync_chrome();
        }
    }

    /// Path bar, title, history buttons, status and sidebar follow the active tab.
    fn sync_chrome(self: &Rc<Self>) {
        if self.panes.borrow().is_empty() {
            return;
        }
        let p = self.pane();
        let loc = p.loc();
        let moved = self.shown_loc.borrow().as_ref() != Some(&loc);
        // Don't overwrite what the user is typing over a mere refresh.
        if moved || !self.path_bar.has_focus() {
            self.completion.set_text_quietly(&loc.display());
        }
        *self.shown_loc.borrow_mut() = Some(loc.clone());
        self.window.set_title(Some(&loc.title()));
        let st = p.state.borrow();
        let up = match &loc {
            Loc::Dir(d) => d.parent().is_some(),
            _ => true,
        };
        for (name, on) in [("back", !st.back.is_empty()), ("forward", !st.forward.is_empty()), ("up", up)] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        }
        drop(st);
        self.status.set_text(&p.status_text());
        self.sync_sort_actions(&p);
        self.refresh_preview();
        if moved {
            self.refresh_sidebar();
        }
    }

    fn on_conn(self: &Rc<Self>, c: Conn) {
        match c {
            Conn::Connected => {
                log::debug!("connected to noxd");
                for p in self.panes.borrow().iter() {
                    // The new connection watches nothing yet.
                    p.state.borrow_mut().subscribed = None;
                    p.reload();
                }
                self.load_places();
                self.load_devices();
                self.load_transfers();
                let p = self.pane();
                let weak = Rc::downgrade(self);
                p.request_then(Request::Health, move |_, r| {
                    if let (Some(b), Response::Health { checks, dismissed: false }) = (weak.upgrade(), r) {
                        *b.missing.borrow_mut() = checks.into_iter().filter(|c| !c.ok).collect();
                        b.refresh_banners();
                    }
                });
                let weak = Rc::downgrade(self);
                p.request_then(Request::UndoLabel, move |_, r| {
                    if let (Some(b), Response::Label(l)) = (weak.upgrade(), r) {
                        *b.undo_label.borrow_mut() = l;
                        b.update_undo();
                    }
                });
                let weak = Rc::downgrade(self);
                p.request_then(Request::ListTrash, move |_, r| {
                    if let (Some(b), Response::TrashItems(t)) = (weak.upgrade(), r) {
                        b.trash_count.set(t.len() as u32);
                        b.refresh_sidebar();
                    }
                });
            }
            Conn::Lost(why) => {
                log::debug!("noxd lost: {why}");
                for p in self.panes.borrow().iter() {
                    p.fail(format!("noxd unavailable: {why}"));
                }
            }
            Conn::Event(ev) => self.on_event(ev),
        }
    }

    fn on_event(self: &Rc<Self>, ev: Event) {
        let panes: Vec<Rc<Pane>> = self.panes.borrow().clone();
        match ev {
            Event::DirChanged { path } => {
                for p in panes.iter().filter(|p| p.loc() == Loc::Dir(path.clone())) {
                    p.reload();
                }
            }
            Event::SizeUpdated { path, bytes } => panes.iter().for_each(|p| p.size_updated(&path, bytes)),
            Event::Moved(moves) => panes.iter().for_each(|p| p.follow_moves(&moves)),
            Event::RecentChanged => {
                for p in panes.iter().filter(|p| matches!(p.loc(), Loc::Recent(_))) {
                    p.reload();
                }
            }
            Event::TrashChanged { items } => {
                self.trash_count.set(items);
                self.refresh_sidebar();
                for p in panes.iter().filter(|p| p.loc() == Loc::Trash) {
                    p.reload();
                }
            }
            Event::UndoChanged(label) => {
                *self.undo_label.borrow_mut() = label;
                self.update_undo();
            }
            Event::PlacesChanged => self.load_places(),
            Event::DevicesChanged => self.load_devices(),
            Event::TransferProgress(status) => self.transfer_progress(status),
            Event::TransferDone { id, error } => self.transfer_done(id, error),
            Event::ThumbnailReady { .. } => {}
        }
    }

    /// Where "Send to ▸" copies: Desktop, Documents, mounted removable drives.
    fn send_targets(&self) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = self
            .places
            .borrow()
            .iter()
            .filter(|p| !p.pinned && matches!(p.name.as_str(), "Desktop" | "Documents"))
            .map(|p| (p.name.clone(), p.path.clone()))
            .collect();
        for d in self.devices.borrow().iter().filter(|d| !d.internal) {
            if let Some(m) = &d.mount_point {
                out.push((noxfm_core::devices::partition_name(d), m.clone()));
            }
        }
        out
    }

    fn is_pinned(&self, path: &Path) -> bool {
        self.places.borrow().iter().any(|p| p.pinned && p.path == path)
    }
}

/// The start of a window: `--recent[=KIND]`, `--trash`, or a folder.
pub(crate) fn start_loc(view: Option<StartView>, path: PathBuf) -> Loc {
    match view {
        Some(StartView::Recent(k)) => Loc::Recent(k),
        Some(StartView::Trash) => Loc::Trash,
        None => Loc::Dir(path),
    }
}

#[cfg(test)]
mod tests;
