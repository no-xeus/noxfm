//! The browser window: one directory, shown as a list or a grid.

mod band;
mod grid;
mod list;
mod menu;
mod properties;
mod sidebar;
mod tabs;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use cosmic::iced::advanced::widget::tree;
use cosmic::iced::clipboard::dnd::DndAction;
use cosmic::iced::keyboard::{self, Key, Modifiers, key::Named};
use cosmic::iced::widget::scrollable::{AbsoluteOffset, Viewport};
use cosmic::iced::{Alignment, Length, Point, Subscription, event, mouse};
use cosmic::prelude::*;
use cosmic::{Core, widget};
use noxfm_core::SortKey;
use noxfm_proto::{
    AppRef, Client, Device, Entry, EntryKind, Event, MountPolicy, NewKind, Place, RecentItem, RecentKind, Request, Response,
    Role, TransferOp, TrashEntry,
};

use crate::clipboard::{ClipboardFiles, DragFiles};
use crate::daemon::{self, Conn};
use crate::preview::{self, Content};
use crate::selection::{Mods, Move, Selection};
use crate::transfers::Jobs;
use crate::{fmt, icons};
use band::{BAND_THRESHOLD, Band};
use properties::{PanelDrag, PropsMsg, PropsPanel};
use tabs::{TabAction, TabDrag, TabId, TabSlot};

/// Modal questions, shown by libcosmic over the window.
pub enum Dialog {
    DeleteForever(Vec<PathBuf>),
    PurgeTrash(Vec<String>),
    EmptyTrash,
    /// "Open with ▸ Other application…" and Properties' "Change…".
    AppPicker { mime: String, path: Option<PathBuf>, filter: String, apps: Vec<AppRef>, remember: bool },
}

// Unique (not named) ids, created once: stable for focus/scroll commands.
// Named ids (`Id::new("…")`) make libcosmic's iced move widget state around
// by name, which loses the path bar's state when the tab bar appears.
static LIST_ID: LazyLock<widget::Id> = LazyLock::new(widget::Id::unique);
static PATH_ID: LazyLock<widget::Id> = LazyLock::new(widget::Id::unique);
static RENAME_ID: LazyLock<widget::Id> = LazyLock::new(widget::Id::unique);

/// Rows moved by PageUp/PageDown in the list.
const PAGE: isize = 10;
const PREVIEW_W: f32 = 420.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

pub struct Flags {
    pub socket: PathBuf,
    pub start: PathBuf,
    pub view: Option<noxfm_proto::StartView>,
    /// Inherited from the window that opened this one.
    pub layout: Option<noxfm_proto::WindowLayout>,
}

const SIDEBAR_MIN: f32 = 160.0;
const SIDEBAR_MAX: f32 = 480.0;

/// A generated thumbnail, and the mtime of the file it was made from.
struct Thumb {
    handle: widget::image::Handle,
    modified: Option<i64>,
}

pub struct App {
    core: Core,
    /// All tabs of this window, in bar order. The active one's state lives
    /// in the fields below (see `tabs.rs`).
    tabs: Vec<TabSlot>,
    active: usize,
    next_tab: TabId,
    tab_drag: Option<TabDrag>,
    /// Back/forward of the active tab (see `tabs::Loc`).
    back: Vec<tabs::Loc>,
    forward: Vec<tabs::Loc>,
    /// The location change in flight is a back/forward step: don't record it.
    history_move: bool,
    sidebar_open: bool,
    sidebar_width: f32,
    /// Dragging the sidebar's edge.
    sidebar_resizing: bool,
    /// Missing dependencies to tell the user about (empty once dismissed).
    missing: Vec<noxfm_proto::HealthCheck>,
    socket: PathBuf,
    client: Option<Client>,
    /// Directory the daemon is currently watching for us.
    subscribed: Option<PathBuf>,
    /// The folder shown, or the last one before switching to Recent.
    path: PathBuf,
    /// Showing the Recent view (of one kind, or all) instead of `path`.
    recent: Option<Option<RecentKind>>,
    /// Why and when each entry of the Recent view is there.
    recent_items: HashMap<PathBuf, RecentItem>,
    /// The user re-sorted the Recent view; otherwise it stays newest-first.
    recent_sorted: bool,
    places: Vec<Place>,
    devices: Vec<Device>,
    /// Folded sidebar sections and disks (`section:<name>`, `sidebar::disk_key`).
    collapsed: HashSet<String>,
    /// Showing the Trash instead of `path`.
    trash_view: bool,
    /// Trash details by the trashed file's path (the entry's path).
    trash_entries: HashMap<PathBuf, TrashEntry>,
    trash_count: u32,
    /// Apps for "Open with ▸", for one MIME type (that of the selection).
    open_with: Option<(String, Vec<AppRef>)>,
    templates: Vec<PathBuf>,
    undo_label: Option<String>,
    /// Inline rename in progress: the item and the text typed so far.
    renaming: Option<(PathBuf, String)>,
    /// Start renaming this as soon as it shows up (after "New ▸").
    pending_rename: Option<PathBuf>,
    /// Select this as soon as it shows up (after "Open file location").
    pending_select: Option<PathBuf>,
    dialog: Option<Dialog>,
    props: Vec<PropsPanel>,
    next_panel: u64,
    panel_drag: Option<PanelDrag>,
    /// Size of the items area, updated while laying out (panel placement).
    items_size: Cell<cosmic::iced::Size>,
    path_input: String,
    completions: Vec<String>,
    entries: Vec<Entry>,
    fs: Option<String>,
    view_mode: ViewMode,
    /// Grid columns at the current width, updated while laying out.
    grid_cols: Cell<usize>,
    thumbs: HashMap<PathBuf, Thumb>,
    sel: Selection,
    mods: Modifiers,
    /// Last known scroll position of the items (offset, visible height).
    viewport: Option<(f32, f32)>,
    /// Folder a drag is currently over, highlighted as the drop target.
    drop_hover: Option<PathBuf>,
    /// Plain press on an already-selected item of a multi-selection: collapse
    /// the selection to it on release, unless a drag started in between.
    pending_click: Option<usize>,
    /// Last pointer position over the items, relative to their viewport.
    pointer: Option<Point>,
    band: Option<Band>,
    /// A right press was already handled by an item or panel. iced doesn't
    /// stop a right press at the first mouse area, so the background's
    /// `RightClick(None)` for the same press must be ignored.
    right_press_handled: bool,
    /// The current press began on an already-selected item, so moving the
    /// pointer drags files rather than drawing a band.
    drag_armed: bool,
    show_preview: bool,
    preview: Option<(PathBuf, Content)>,
    jobs: Jobs,
    /// The transfers tab; hidden unless asked for.
    show_transfers: bool,
    sort: (SortKey, bool),
    show_hidden: bool,
    show_created: bool,
    show_details: bool,
    status: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Daemon(Conn),
    Listed(Result<(PathBuf, Option<String>, Vec<Entry>), String>),
    /// An answer for a tab; dropped if that tab isn't the active one anymore
    /// (it lists itself again when shown).
    ForTab(TabId, Box<Message>),
    TabPress(usize),
    Back,
    Forward,
    ToggleSidebar,
    SidebarResizeStart,
    SidebarResize(f32),
    /// Pointer over the content while a tab is held (places the preview).
    TabPreviewAt(Point),
    Health(Vec<noxfm_proto::HealthCheck>),
    DismissHealth,
    TabDragMove(Point),
    CloseTab(usize),
    TabMenu(TabAction),
    /// Middle click on an item: folders open in a new tab.
    MiddleClick(usize),
    PathInput(String),
    PathSubmit,
    AcceptCompletion(Option<String>),
    Completions(String, Vec<String>),
    Up,
    Click(usize),
    /// Press on the background (not on an item).
    BackgroundPress,
    PointerMoved(Point),
    MouseUp,
    Release(usize),
    DragStarted,
    DragEnded,
    Activate(usize),
    Opened(Result<(), String>),
    Key(Key, Modifiers),
    Mods(Modifiers),
    Scrolled(Viewport),
    ToClipboard(TransferOp),
    Paste,
    Pasted(Option<ClipboardFiles>),
    TransferResult(Result<(), String>),
    /// Files dropped on the background (`None` target) or on a folder.
    Dropped(Option<PathBuf>, Option<DragFiles>, DndAction),
    DropEnter(PathBuf),
    DropLeave(PathBuf),
    Thumbnail(PathBuf, Option<i64>, Option<Vec<u8>>),
    Navigate(PathBuf),
    Places(Vec<Place>),
    Devices(Vec<Device>),
    OpenRecent(Option<RecentKind>),
    RecentListed(Option<RecentKind>, Result<Vec<(RecentItem, Entry)>, String>),
    MountDevice(String),
    Mounted(Result<PathBuf, String>),
    UnmountDevice(String),
    SetPolicy(String, MountPolicy),
    DeviceMenu(sidebar::DeviceAction),
    PlaceMenu(sidebar::PlaceAction),
    DiskMenu(sidebar::DiskAction),
    SidebarCollapsed(Vec<String>),
    ToggleCollapsed(String),
    Ctx(menu::Ctx),
    /// Right press on an item (`Some`) or the background (`None`).
    RightClick(Option<usize>),
    /// Right press swallowed by a Properties panel.
    RightPressConsumed,
    Props(PropsMsg),
    PastedLink(Option<ClipboardFiles>),
    Created(Result<PathBuf, String>),
    RenameInput(String),
    RenameCommit,
    /// Escape, even when a text field used it (to cancel a rename).
    Escape,
    Renamed(Result<PathBuf, String>),
    OpenWithApps(String, Vec<AppRef>),
    OpenTrash,
    TrashListed(Result<Vec<(TrashEntry, Entry)>, String>),
    Undone(Result<Option<String>, String>),
    DialogConfirm,
    DialogCancel,
    PickerFilter(String),
    PickerRemember(bool),
    PickerChoose(String),
    PickerApps(Vec<AppRef>),
    /// Popup surfaces (context menus) are managed by libcosmic.
    Surface(cosmic::surface::Action<Message>),
    DismissAsk(String),
    TogglePreview,
    PreviewLoaded(PathBuf, Content),
    ToggleTransfers,
    TransfersSnapshot(Vec<noxfm_proto::TransferStatus>),
    CancelTransfer(u64),
    DismissTransfer(u64),
    Tick,
    ToggleView,
    ToggleHidden,
    ToggleCreated,
    ToggleDetails,
    Sort(SortKey),
    Noop,
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "dev.noxfm.Browser";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Flags) -> (Self, cosmic::app::Task<Message>) {
        let app = App {
            core,
            tabs: vec![TabSlot { id: 1, state: None }],
            active: 0,
            next_tab: 1,
            tab_drag: None,
            back: Vec::new(),
            forward: Vec::new(),
            history_move: false,
            sidebar_open: flags.layout.is_none_or(|l| l.sidebar_open),
            sidebar_width: flags.layout.map_or(sidebar::SIDEBAR_W, |l| l.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)),
            sidebar_resizing: false,
            missing: Vec::new(),
            socket: flags.socket,
            client: None,
            subscribed: None,
            path_input: display(&flags.start),
            path: flags.start,
            recent: match flags.view {
                Some(noxfm_proto::StartView::Recent(k)) => Some(k),
                _ => None,
            },
            recent_items: HashMap::new(),
            recent_sorted: false,
            places: Vec::new(),
            devices: Vec::new(),
            collapsed: HashSet::new(),
            trash_view: flags.view == Some(noxfm_proto::StartView::Trash),
            trash_entries: HashMap::new(),
            trash_count: 0,
            open_with: None,
            templates: load_templates(),
            undo_label: None,
            renaming: None,
            pending_rename: None,
            pending_select: None,
            dialog: None,
            props: Vec::new(),
            next_panel: 0,
            panel_drag: None,
            items_size: Cell::new(cosmic::iced::Size::ZERO),
            completions: Vec::new(),
            entries: Vec::new(),
            fs: None,
            view_mode: ViewMode::List,
            grid_cols: Cell::new(1),
            thumbs: HashMap::new(),
            sel: Selection::default(),
            mods: Modifiers::empty(),
            viewport: None,
            drop_hover: None,
            pending_click: None,
            pointer: None,
            band: None,
            drag_armed: false,
            right_press_handled: false,
            show_preview: false,
            preview: None,
            jobs: Jobs::default(),
            show_transfers: false,
            sort: (SortKey::Name, true),
            show_hidden: false,
            show_created: false,
            show_details: false,
            status: Some("Connecting to noxd…".into()),
        };
        (app, Task::none())
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            daemon::subscription(self.socket.clone(), Role::Browser).map(Message::Daemon),
            // While a tab or the sidebar edge is held, follow the pointer anywhere
            // (even outside the window).
            if self.sidebar_resizing {
                event::listen_with(|ev, _, _| match ev {
                    cosmic::iced::Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Message::SidebarResize(position.x)),
                    _ => None,
                })
            } else if self.tab_drag.is_some() {
                event::listen_with(|ev, _, _| match ev {
                    cosmic::iced::Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Message::TabDragMove(position)),
                    _ => None,
                })
            } else {
                Subscription::none()
            },
            if self.jobs.is_empty() {
                Subscription::none()
            } else {
                cosmic::iced::time::every(std::time::Duration::from_millis(500)).map(|_| Message::Tick)
            },
            event::listen_with(|ev, status, _window| match ev {
                cosmic::iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => Some(Message::Mods(m)),
                cosmic::iced::Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(Named::Escape), .. }) => {
                    Some(Message::Escape)
                }
                cosmic::iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Back)) => Some(Message::Back),
                cosmic::iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Forward)) => Some(Message::Forward),
                // Wherever the button comes up, a rubber band ends.
                cosmic::iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(Message::MouseUp),
                // Keys a focused widget (the path bar) already used aren't ours.
                cosmic::iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                    if status == event::Status::Ignored =>
                {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
        ])
    }

    fn update(&mut self, message: Message) -> cosmic::app::Task<Message> {
        // Clicking elsewhere finishes a rename, as in Explorer.
        let commit = match &message {
            Message::Click(_) | Message::BackgroundPress | Message::RightClick(_) if self.renaming.is_some() => {
                self.handle(Message::RenameCommit)
            }
            _ => Task::none(),
        };
        let task = Task::batch([commit, self.handle(message)]);
        // Whatever changed the selection, keep the preview and "Open with" in step.
        Task::batch([task, self.refresh_preview(), self.refresh_open_with()])
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        self.dialog_view()
    }

    fn header_start(&self) -> Vec<Element<'_, Message>> {
        let button = |names: &[&str], msg: Option<Message>, tip: &'static str| -> Element<'_, Message> {
            // No message: greyed out (nothing to go back to, …). libcosmic paints
            // symbolic icons on disabled buttons brighter than enabled ones, so
            // draw those in the icon's own (dim) colour instead.
            let mut handle = icons::handle(names, 16);
            handle.symbolic &= msg.is_some();
            let b = widget::button::icon(handle).on_press_maybe(msg);
            widget::tooltip(b, widget::text::body(tip), widget::tooltip::Position::Bottom).into()
        };
        vec![
            button(
                &["sidebar-show-symbolic", "view-dual-symbolic", "open-menu-symbolic"],
                Some(Message::ToggleSidebar),
                if self.sidebar_open { "Hide sidebar (F9)" } else { "Show sidebar (F9)" },
            ),
            button(&["go-previous-symbolic"], (!self.back.is_empty()).then_some(Message::Back), "Back (Alt+Left)"),
            button(&["go-next-symbolic"], (!self.forward.is_empty()).then_some(Message::Forward), "Forward (Alt+Right)"),
            button(&["go-up-symbolic"], Some(Message::Up), "Up (Backspace)"),
            button(&["tab-new-symbolic", "list-add-symbolic"], Some(Message::TabMenu(TabAction::New)), "New tab (Ctrl+T)"),
        ]
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        let toggle = |names: &[&str], on: bool, msg: Message, tip: &'static str| -> Element<'_, Message> {
            // `.selected()` doesn't render on header-bar icon buttons, so fill "on" toggles explicitly.
            let class = if on { cosmic::theme::Button::Standard } else { cosmic::theme::Button::Icon };
            let button = widget::button::icon(icons::handle(names, 16)).class(class).on_press(msg);
            widget::tooltip(button, widget::text::body(tip), widget::tooltip::Position::Bottom).into()
        };
        let mut buttons = Vec::with_capacity(6);
        if !self.jobs.is_empty() {
            buttons.push(toggle(
                &["folder-download-symbolic", "emblem-synchronizing-symbolic", "document-send-symbolic"],
                self.show_transfers,
                Message::ToggleTransfers,
                "Transfers",
            ));
        }
        if self.preview_target().is_some() {
            buttons.push(toggle(
                &["view-paged-symbolic", "document-print-preview-symbolic", "x-office-document-symbolic"],
                self.show_preview,
                Message::TogglePreview,
                "Preview",
            ));
        }
        let (view_icon, view_tip) = match self.view_mode {
            ViewMode::List => ("view-grid-symbolic", "Show as icons"),
            ViewMode::Grid => ("view-list-symbolic", "Show as list"),
        };
        buttons.extend([
            toggle(&[view_icon], false, Message::ToggleView, view_tip),
            toggle(
                &["x-office-calendar-symbolic", "office-calendar-symbolic"],
                self.show_created,
                Message::ToggleCreated,
                "Show creation date",
            ),
            toggle(
                &["document-properties-symbolic", "dialog-information-symbolic"],
                self.show_details,
                Message::ToggleDetails,
                "Show owner and permissions",
            ),
            toggle(&["view-reveal-symbolic"], self.show_hidden, Message::ToggleHidden, "Show hidden files"),
        ]);
        buttons
    }

    fn view(&self) -> Element<'_, Message> {
        let space = cosmic::theme::spacing();

        let path_bar = widget::text_input("Path", &self.path_input)
            .id(PATH_ID.clone())
            .on_input(Message::PathInput)
            .on_submit(|_| Message::PathSubmit)
            .on_tab(Message::AcceptCompletion(None))
            .width(Length::Fill);

        // The tab bar's slot always exists (empty with one tab): widgets after
        // it must keep their position, or libcosmic's text input picks up
        // another widget's state and panics.
        let tab_bar: Element<'_, Message> = self.tab_bar().unwrap_or_else(|| widget::Space::new().height(0).into());
        let mut col = widget::column::with_capacity(9).spacing(space.space_xxs).push(tab_bar).push(path_bar);

        if !self.completions.is_empty() {
            let list = self.completions.iter().take(8).fold(widget::column::with_capacity(8), |c, s| {
                c.push(
                    widget::button::text(s.as_str())
                        .on_press(Message::AcceptCompletion(Some(s.clone())))
                        .width(Length::Fill),
                )
            });
            col = col.push(widget::container(list).class(cosmic::style::Container::Card));
        }

        if let Some(banners) = self.ask_banners() {
            col = col.push(banners);
        }
        if let Some(banner) = self.health_banner() {
            col = col.push(banner);
        }

        // Laid out responsively: the grid's column count depends on the width.
        let items = cosmic::iced::widget::responsive(move |size| {
            self.grid_cols.set(grid::columns(size.width));
            self.items_size.set(size);
            // Dragging a selected item drags the whole selection.
            let dragged = Arc::new(self.sel.paths(&self.visible_paths()));
            let content = match self.view_mode {
                ViewMode::List => self.list_view(&dragged),
                ViewMode::Grid => self.grid_view(&dragged),
            };
            widget::scrollable(content)
                .id(LIST_ID.clone())
                .on_scroll(Message::Scrolled)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        });
        let items = widget::dnd_destination::dnd_destination_for_data(items, |files, action| {
            Message::Dropped(None, files, action)
        })
        .on_leave(|| Message::DragEnded);
        // One menu for the whole area; what it offers follows the selection,
        // which a right press on an item has just set.
        let mut items = widget::context_menu(items, Some(self.context_menu())).on_surface_action(Message::Surface);
        if let Some(id) = self.core.main_window_id() {
            items = items.window_id(id);
        }
        // Rubber band and Properties panels float above the items.
        let mut layers: Vec<Element<'_, Message>> = vec![items.into()];
        layers.extend(self.band_rect());
        layers.extend(self.panels());
        let items = cosmic::iced::widget::Stack::with_children(layers);
        // Items capture their own presses, so `on_press` here means the background.
        let items = widget::mouse_area(items)
            .on_press(Message::BackgroundPress)
            .on_right_press(Message::RightClick(None))
            .on_move(Message::PointerMoved);

        // Column headers belong to the items, so they stay aligned when the
        // preview panel takes space on the right.
        let mut items_col = widget::column::with_capacity(3).spacing(space.space_xxs);
        if self.view_mode == ViewMode::List {
            items_col = items_col.push(self.column_headers());
        }
        let items_col = items_col.push(widget::divider::horizontal::default()).push(items);
        let mut body = widget::row::with_capacity(2).spacing(space.space_xs).push(items_col);
        if let Some(e) = self.preview_target().filter(|_| self.show_preview) {
            body = body.push(self.preview_panel(e));
        }

        col = col.push(body.height(Length::Fill));
        if self.show_transfers && !self.jobs.is_empty() {
            let tab = self.jobs.view(Message::CancelTransfer, Message::DismissTransfer);
            col = col.push(
                widget::container(tab)
                    .padding(space.space_xs)
                    .max_height(240)
                    .width(Length::Fill)
                    .class(cosmic::style::Container::Card),
            );
        }
        col = col.push(self.footer());

        let main = widget::container(col).padding(space.space_xs).width(Length::Fill);
        // Always the same children (spacers when hidden), so widget state
        // doesn't get shuffled when the sidebar or the tab preview toggles.
        let (side, edge): (Element<'_, Message>, Element<'_, Message>) = if self.sidebar_open {
            let edge = widget::mouse_area(
                widget::container(widget::divider::vertical::default())
                    .width(6)
                    .height(Length::Fill)
                    .align_x(Alignment::Center),
            )
            .on_press(Message::SidebarResizeStart)
            .interaction(cosmic::iced::mouse::Interaction::ResizingHorizontally);
            (self.sidebar(), edge.into())
        } else {
            (widget::Space::new().width(0).into(), widget::Space::new().width(0).into())
        };
        let window = widget::row::with_capacity(3).push(side).push(edge).push(main);
        let preview: Element<'_, Message> = self.tab_drag_preview().unwrap_or_else(|| widget::Space::new().width(0).into());
        let root = cosmic::iced::widget::Stack::with_children(vec![window.into(), preview]);
        // Content coordinates for the preview; only listened to during a tab drag.
        let mut root = widget::mouse_area(root);
        if self.tab_drag.is_some() {
            root = root.on_move(Message::TabPreviewAt);
        }
        root.into()
    }
}

impl App {
    fn handle(&mut self, message: Message) -> cosmic::app::Task<Message> {
        match message {
            Message::Daemon(Conn::Connected(client)) => {
                self.client = Some(client.clone());
                self.subscribed = None;
                self.status = None;
                let snapshot = cosmic::task::future(async move {
                    match client.request(Request::ListTransfers).await {
                        Ok(Response::Transfers(list)) => Message::TransfersSnapshot(list),
                        _ => Message::Noop,
                    }
                });
                let undo_label = self.request_map(Request::UndoLabel, |r| match r {
                    Ok(Response::Label(l)) => Message::Daemon(Conn::Event(Event::UndoChanged(l))),
                    _ => Message::Noop,
                });
                let trash = self.request_map(Request::ListTrash, |r| match r {
                    Ok(Response::TrashItems(t)) => Message::Daemon(Conn::Event(Event::TrashChanged { items: t.len() as u32 })),
                    _ => Message::Noop,
                });
                let health = self.request_map(Request::Health, |r| match r {
                    Ok(Response::Health { checks, dismissed: false }) => {
                        Message::Health(checks.into_iter().filter(|c| !c.ok).collect())
                    }
                    _ => Message::Noop,
                });
                return Task::batch([self.reload(), snapshot, self.load_places(), self.load_devices(), undo_label, trash, health]);
            }
            Message::Daemon(Conn::Lost(why)) => {
                self.client = None;
                self.status = Some(format!("noxd unavailable: {why}"));
            }
            Message::Daemon(Conn::Event(ev)) => match ev {
                Event::DirChanged { path } if path == self.path && self.recent.is_none() && !self.trash_view => {
                    return self.load(path);
                }
                Event::RecentChanged => {
                    if let Some(kind) = self.recent {
                        return self.load_recent(kind);
                    }
                }
                Event::Moved(moves) => return self.follow_moves(&moves),
                Event::DevicesChanged => return self.load_devices(),
                Event::PlacesChanged => return self.load_places(),
                Event::UndoChanged(label) => self.undo_label = label,
                Event::TrashChanged { items } => {
                    self.trash_count = items;
                    if self.trash_view {
                        return self.load_trash();
                    }
                }
                Event::TransferProgress(status) => self.jobs.progress(status),
                Event::TransferDone { id, error } => self.jobs.done(id, error),
                Event::SizeUpdated { path, bytes } => {
                    if let Some(e) = self.entries.iter_mut().find(|e| e.path == path) {
                        e.size = Some(bytes);
                        if self.sort.0 == SortKey::Size {
                            self.resort();
                        }
                    }
                }
                _ => {}
            },
            Message::Listed(Ok((path, fs, entries))) => {
                self.record_move(&tabs::Loc::Dir(path.clone()));
                if path != self.path || self.recent.is_some() || self.trash_view {
                    self.trash_view = false;
                    self.trash_entries.clear();
                    self.recent = None;
                    self.recent_items.clear();
                    self.path = path.clone();
                    self.sel.clear();
                    self.viewport = None;
                    self.thumbs.clear();
                }
                let mut tasks = Vec::new();
                if let Some(win) = self.core.main_window_id() {
                    tasks.push(self.set_window_title(display(&path), win));
                }
                // The listing subscribed us to `path`; drop the previous watch.
                // Another tab may still show the old folder: then keep watching it.
                let old = self.subscribed.replace(path.clone()).filter(|old| *old != path);
                if let Some(old) = old.filter(|o| !self.watching(o)) {
                    tasks.push(self.fire(Request::Unsubscribe { path: old }));
                }
                self.path_input = display(&path);
                self.completions.clear();
                self.entries = entries;
                self.fs = fs;
                self.resort();
                self.status = None;
                let items = self.visible_paths();
                self.sel.retain(&items);
                tasks.extend(self.request_thumbnails());
                tasks.push(self.apply_pending());
                return Task::batch(tasks);
            }
            Message::Listed(Err(e)) => {
                self.history_move = false;
                self.status = Some(e);
            }
            Message::PathInput(s) => {
                self.path_input = s.clone();
                if let Some(client) = self.client.clone() {
                    return cosmic::task::future(async move {
                        let found = match client.request(Request::Complete { prefix: s.clone() }).await {
                            Ok(Response::Completions(c)) => c,
                            _ => Vec::new(),
                        };
                        Message::Completions(s, found)
                    });
                }
            }
            Message::Completions(prefix, found) => {
                // Drop stale answers for text the user has already changed.
                if prefix == self.path_input {
                    self.completions = found;
                }
            }
            Message::AcceptCompletion(choice) => {
                if let Some(c) = choice.or_else(|| self.completions.first().cloned()) {
                    return self.handle(Message::PathInput(c));
                }
            }
            Message::PathSubmit => {
                let home = noxfm_core::complete::home_dir();
                let p = noxfm_core::complete::expand_tilde(self.path_input.trim(), &home);
                return self.load(PathBuf::from(p));
            }
            Message::Up => {
                // From Recent or Trash, "up" goes back to the folder we came from.
                if self.recent.is_some() || self.trash_view {
                    return self.load(self.path.clone());
                }
                if let Some(parent) = self.path.parent() {
                    return self.load(parent.to_path_buf());
                }
            }
            Message::Click(i) => {
                let items = self.visible_paths();
                let mods = Mods { ctrl: self.mods.control(), shift: self.mods.shift() };
                let on_selected = items.get(i).is_some_and(|p| self.sel.contains(p));
                self.drag_armed = on_selected && !mods.ctrl && !mods.shift;
                if on_selected && mods == Mods::default() && self.sel.len() > 1 {
                    // Might be the start of dragging the whole selection.
                    self.pending_click = Some(i);
                } else {
                    let before = self.sel.clone();
                    self.sel.click(&items, i, mods);
                    // Pressing a selected item may start a drag; any other may start a band.
                    if !on_selected && !mods.shift {
                        self.start_band(if mods.ctrl { before } else { Selection::default() });
                    }
                }
            }
            Message::BackgroundPress => {
                let base = if self.mods.control() { self.sel.clone() } else { Selection::default() };
                self.sel = base.clone();
                self.start_band(base);
            }
            Message::PointerMoved(p) => {
                self.pointer = Some(p);
                if self.panel_drag.is_some() {
                    self.drag_panel(p);
                    return Task::none();
                }
                let content = self.to_content(p);
                if let Some(band) = &mut self.band {
                    band.current = content;
                    let (dx, dy) = (content.x - band.origin.x, content.y - band.origin.y);
                    band.active |= dx.hypot(dy) > BAND_THRESHOLD;
                }
                if self.band.as_ref().is_some_and(|b| b.active) {
                    self.pending_click = None;
                    self.apply_band();
                }
            }
            Message::MouseUp => {
                self.band = None;
                self.panel_drag = None;
                self.sidebar_resizing = false;
                if self.tab_drag.is_some() {
                    return self.tab_drag_end();
                }
                self.drag_armed = false;
            }
            Message::Release(i) => {
                if self.pending_click.take() == Some(i) {
                    self.sel.click(&self.visible_paths(), i, Mods::default());
                }
            }
            Message::DragStarted => {
                log::debug!("drag started with {} item(s)", self.sel.len());
                self.pending_click = None;
            }
            Message::DragEnded => {
                log::debug!("drag ended");
                self.pending_click = None;
                self.drop_hover = None;
            }
            Message::Activate(i) => return self.activate(i),
            Message::Opened(Ok(())) => {}
            Message::Opened(Err(e)) => self.status = Some(e),
            Message::Mods(m) => self.mods = m,
            Message::Key(key, m) => return self.key(key, m),
            Message::Scrolled(v) => {
                self.viewport = Some((v.absolute_offset().y, v.bounds().height));
            }
            Message::ToClipboard(op) => {
                let paths = self.sel.paths(&self.visible_paths());
                if !paths.is_empty() {
                    let n = paths.len();
                    let verb = if op == TransferOp::Move { "Cut" } else { "Copied" };
                    self.status = Some(format!("{verb} {n} item{}", if n == 1 { "" } else { "s" }));
                    return cosmic::iced::clipboard::write_data(ClipboardFiles { op, paths });
                }
            }
            Message::Paste => {
                return cosmic::iced::clipboard::read_data::<ClipboardFiles>()
                    .map(|c| cosmic::Action::App(Message::Pasted(c)));
            }
            Message::Pasted(None) => self.status = Some("Nothing to paste".into()),
            Message::Pasted(Some(_)) if self.recent.is_some() || self.trash_view => {
                self.status = Some("Open a folder to paste into".into());
            }
            Message::Pasted(Some(ClipboardFiles { op, paths })) => {
                return self.transfer(op, paths, self.path.clone());
            }
            Message::Dropped(target, files, action) => {
                log::debug!("drop on {target:?}: {files:?} action={action:?}");
                self.drop_hover = None;
                let Some(DragFiles(sources)) = files else { return Task::none() };
                if target.is_none() && (self.recent.is_some() || self.trash_view) {
                    return Task::none(); // Recent isn't a folder
                }
                let dest = target.unwrap_or_else(|| self.path.clone());
                if sources.contains(&dest) {
                    return Task::none(); // a folder dropped onto itself
                }
                let op = drop_op(&sources, &dest, action, self.mods.control());
                return self.transfer(op, sources, dest);
            }
            Message::DropEnter(p) => {
                log::debug!("drag entered {}", p.display());
                self.drop_hover = Some(p);
            }
            // Leaving one item may arrive after entering the next; only clear our own.
            Message::DropLeave(p) => {
                if self.drop_hover.as_ref() == Some(&p) {
                    self.drop_hover = None;
                }
            }
            Message::Thumbnail(path, modified, bytes) => match bytes {
                Some(b) => {
                    self.thumbs.insert(path, Thumb { handle: widget::image::Handle::from_bytes(b), modified });
                }
                None => {
                    self.thumbs.remove(&path);
                }
            },
            Message::Navigate(path) => return self.load(path),
            Message::Places(places) => self.places = places,
            Message::Devices(devices) => self.devices = devices,
            Message::OpenRecent(kind) => {
                let mut tasks = vec![self.load_recent(kind)];
                if let Some(old) = self.subscribed.take().filter(|o| !self.watching(o)) {
                    tasks.push(self.fire(Request::Unsubscribe { path: old }));
                }
                return Task::batch(tasks);
            }
            Message::RecentListed(kind, Ok(items)) => {
                self.record_move(&tabs::Loc::Recent(kind));
                if self.recent != Some(kind) || self.trash_view {
                    self.trash_view = false;
                    self.trash_entries.clear();
                    self.recent = Some(kind);
                    self.recent_sorted = false;
                    self.sel.clear();
                    self.viewport = None;
                    self.thumbs.clear();
                }
                self.recent_items = items.iter().map(|(r, e)| (e.path.clone(), r.clone())).collect();
                self.entries = items.into_iter().map(|(_, e)| e).collect();
                self.fs = None;
                self.completions.clear();
                self.path_input = recent_title(kind);
                self.status = None;
                self.resort();
                let items = self.visible_paths();
                self.sel.retain(&items);
                let mut tasks = self.request_thumbnails();
                if let Some(win) = self.core.main_window_id() {
                    tasks.push(self.set_window_title(recent_title(kind), win));
                }
                return Task::batch(tasks);
            }
            Message::RecentListed(_, Err(e)) => self.status = Some(e),
            Message::MountDevice(id) => {
                let Some(client) = self.client.clone() else { return Task::none() };
                self.status = Some("Mounting…".into());
                return cosmic::task::future(async move {
                    let r = match client.request(Request::Mount { device: id }).await {
                        Ok(Response::Mounted(p)) => Ok(p),
                        Ok(other) => Err(format!("unexpected reply: {other:?}")),
                        Err(e) => Err(e.to_string()),
                    };
                    Message::Mounted(r)
                });
            }
            Message::Mounted(Ok(path)) => {
                self.status = None;
                return self.load(path);
            }
            Message::Mounted(Err(e)) => self.status = Some(e),
            Message::UnmountDevice(id) => {
                let mut tasks = Vec::new();
                // Step out of the volume first.
                let inside = self.devices.iter().find(|d| d.id == id).and_then(|d| d.mount_point.as_ref()).is_some_and(|m| self.path.starts_with(m));
                if inside && self.recent.is_none() {
                    tasks.push(self.load(noxfm_core::complete::home_dir()));
                }
                tasks.push(self.request(Request::Unmount { device: id }));
                return Task::batch(tasks);
            }
            Message::SetPolicy(uuid, policy) => return self.request(Request::SetMountPolicy { uuid, policy }),
            Message::DismissAsk(id) => return self.fire(Request::DismissAsk { device: id }),
            Message::Ctx(c) => return self.run_ctx(c),
            Message::ForTab(id, m) => {
                if id == self.active_tab() {
                    return self.handle(*m);
                }
            }
            Message::Back => return self.go_back(),
            Message::Forward => return self.go_forward(),
            Message::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Message::SidebarResizeStart => self.sidebar_resizing = true,
            Message::SidebarResize(x) => self.sidebar_width = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX),
            Message::Health(missing) => self.missing = missing,
            Message::DismissHealth => {
                self.missing.clear();
                return self.request(Request::DismissHealth);
            }
            Message::TabPreviewAt(p) => {
                if let Some(d) = &mut self.tab_drag {
                    d.pointer = Some(p);
                }
            }
            Message::TabPress(i) => {
                self.tab_drag = Some(TabDrag { index: i, pointer: None, moved: false, origin: None, detaching: false });
                return self.switch_tab(i);
            }
            Message::TabDragMove(p) => self.tab_drag_move(p),
            Message::CloseTab(i) => {
                self.tab_drag = None;
                return self.close_tab(i);
            }
            Message::TabMenu(a) => return self.tab_menu_action(a),
            Message::MiddleClick(i) => {
                let dir = self.visible().nth(i).filter(|e| e.kind == EntryKind::Dir).map(|e| e.path.clone());
                if let Some(path) = dir {
                    return self.open_tab(path, None);
                }
            }
            Message::PlaceMenu(a) => {
                use sidebar::PlaceAction as A;
                let (A::OpenNewWindow(i) | A::Unpin(i)) = a;
                let Some(place) = self.places.get(i).cloned() else { return Task::none() };
                return match a {
                    A::OpenNewWindow(_) => self.request(Request::OpenWindow { path: Some(place.path), view: None, layout: Some(self.layout()) }),
                    A::Unpin(_) => self.request(Request::Unpin { path: place.path }),
                };
            }
            Message::DiskMenu(a) => {
                use sidebar::DiskAction as A;
                let A::Properties(i) = a;
                if let Some(key) = self.devices.get(i).map(sidebar::disk_key) {
                    self.open_device_properties(properties::PanelKind::Disk(key));
                }
            }
            Message::SidebarCollapsed(keys) => self.collapsed = keys.into_iter().collect(),
            Message::ToggleCollapsed(key) => {
                // Fold at once; the daemon remembers it and tells the other windows.
                let collapsed = !self.collapsed.contains(&key);
                if collapsed {
                    self.collapsed.insert(key.clone());
                } else {
                    self.collapsed.remove(&key);
                }
                return self.request(Request::SetSidebarCollapsed { key, collapsed });
            }
            Message::RightPressConsumed => self.right_press_handled = true,
            Message::RightClick(i) => {
                self.band = None;
                // The background hears every right press after the item under
                // the pointer has: only act on it when no item did.
                if i.is_none() && std::mem::take(&mut self.right_press_handled) {
                    return Task::none();
                }
                let items = self.visible_paths();
                match i.and_then(|i| items.get(i).map(|p| (i, p))) {
                    // Like Explorer: right-clicking outside the selection selects just that.
                    Some((i, p)) => {
                        self.right_press_handled = true;
                        if !self.sel.contains(p) {
                            self.sel.click(&items, i, Mods::default());
                        }
                    }
                    None => self.sel.clear(),
                }
            }
            Message::Props(m) => return self.props_update(m),
            Message::PastedLink(None) => self.status = Some("Nothing to paste".into()),
            Message::PastedLink(Some(ClipboardFiles { paths, .. })) => {
                if self.recent.is_none() && !self.trash_view {
                    return self.request(Request::Symlink { targets: paths, dir: self.path.clone() });
                }
            }
            Message::Created(Ok(path)) => {
                self.pending_rename = Some(path);
                return self.apply_pending();
            }
            Message::Created(Err(e)) | Message::Renamed(Err(e)) => self.status = Some(e),
            Message::RenameInput(s) => {
                if let Some((_, text)) = &mut self.renaming {
                    *text = s;
                }
            }
            Message::RenameCommit => {
                let Some((path, name)) = self.renaming.take() else { return Task::none() };
                let unchanged = path.file_name().is_some_and(|n| n.to_string_lossy() == name);
                if unchanged {
                    return Task::none();
                }
                let Some(client) = self.client.clone() else { return Task::none() };
                return cosmic::task::future(async move {
                    let r = match client.request(Request::Rename { path, new_name: name }).await {
                        Ok(Response::Path(p)) => Ok(p),
                        Ok(other) => Err(format!("unexpected reply: {other:?}")),
                        Err(e) => Err(e.to_string()),
                    };
                    Message::Renamed(r)
                });
            }
            Message::Escape => {
                // Innermost first: rename, dialog, top Properties panel, selection.
                let closed = self.renaming.take().is_some() || self.dialog.take().is_some() || self.props.pop().is_some();
                if !closed {
                    self.sel.clear();
                    self.completions.clear();
                }
            }
            Message::Renamed(Ok(path)) => {
                self.pending_select = Some(path);
                return self.apply_pending();
            }
            Message::OpenWithApps(mime, apps) => self.open_with = Some((mime, apps)),
            Message::OpenTrash => {
                let mut tasks = vec![self.load_trash()];
                if let Some(old) = self.subscribed.take().filter(|o| !self.watching(o)) {
                    tasks.push(self.fire(Request::Unsubscribe { path: old }));
                }
                return Task::batch(tasks);
            }
            Message::TrashListed(Ok(items)) => {
                self.record_move(&tabs::Loc::Trash);
                self.trash_count = items.len() as u32;
                if !self.trash_view {
                    self.trash_view = true;
                    self.recent = None;
                    self.recent_items.clear();
                    self.sel.clear();
                    self.viewport = None;
                    self.thumbs.clear();
                }
                self.trash_entries = items.iter().map(|(t, e)| (e.path.clone(), t.clone())).collect();
                self.entries = items.into_iter().map(|(_, e)| e).collect();
                self.fs = None;
                self.completions.clear();
                self.path_input = "Trash".into();
                self.status = None;
                let items = self.visible_paths();
                self.sel.retain(&items);
                let mut tasks = self.request_thumbnails();
                if let Some(win) = self.core.main_window_id() {
                    tasks.push(self.set_window_title("Trash".into(), win));
                }
                return Task::batch(tasks);
            }
            Message::TrashListed(Err(e)) => self.status = Some(e),
            Message::Undone(Ok(label)) => self.status = label.map(|l| format!("Undid {l}")),
            Message::Undone(Err(e)) => self.status = Some(e),
            Message::DialogCancel => self.dialog = None,
            Message::DialogConfirm => {
                let req = match self.dialog.take() {
                    Some(Dialog::DeleteForever(paths)) => Request::DeleteForever { paths },
                    Some(Dialog::PurgeTrash(ids)) => Request::PurgeTrash { ids },
                    Some(Dialog::EmptyTrash) => Request::EmptyTrash,
                    _ => return Task::none(),
                };
                return self.request(req);
            }
            Message::PickerFilter(f) => {
                if let Some(Dialog::AppPicker { filter, .. }) = &mut self.dialog {
                    *filter = f;
                }
            }
            Message::PickerRemember(r) => {
                if let Some(Dialog::AppPicker { remember, .. }) = &mut self.dialog {
                    *remember = r;
                }
            }
            Message::PickerApps(list) => {
                if let Some(Dialog::AppPicker { apps, .. }) = &mut self.dialog {
                    *apps = list;
                }
            }
            Message::PickerChoose(id) => {
                let Some(Dialog::AppPicker { mime, path, remember, .. }) = self.dialog.take() else { return Task::none() };
                let mut tasks = Vec::new();
                // Properties' "Change…" has no file to open: it only sets the default.
                if remember || path.is_none() {
                    tasks.push(self.request(Request::SetDefaultApp { mime, app: id.clone() }));
                    self.open_with = None;
                    tasks.push(self.reload());
                }
                if let Some(path) = path {
                    tasks.push(self.request(Request::OpenWith { path, app: Some(id) }));
                }
                return Task::batch(tasks);
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
            Message::DeviceMenu(action) => {
                use sidebar::DeviceAction;
                let i = match action {
                    DeviceAction::Open(i)
                    | DeviceAction::Unmount(i)
                    | DeviceAction::Policy(i, _)
                    | DeviceAction::CopyPath(i)
                    | DeviceAction::Properties(i) => i,
                };
                let Some(d) = self.devices.get(i).cloned() else { return Task::none() };
                return match action {
                    DeviceAction::Open(_) => match d.mount_point {
                        Some(mp) => self.load(mp),
                        None => self.handle(Message::MountDevice(d.id)),
                    },
                    DeviceAction::Unmount(_) => self.handle(Message::UnmountDevice(d.id)),
                    DeviceAction::Policy(_, p) => match d.uuid {
                        Some(uuid) => self.handle(Message::SetPolicy(uuid, p)),
                        None => Task::none(),
                    },
                    DeviceAction::CopyPath(_) => cosmic::iced::clipboard::write(d.device),
                    DeviceAction::Properties(_) => {
                        self.open_device_properties(properties::PanelKind::Partition(d.id));
                        Task::none()
                    }
                };
            }
            Message::TogglePreview => self.show_preview = !self.show_preview,
            Message::PreviewLoaded(path, content) => self.preview = Some((path, content)),
            Message::ToggleTransfers => self.show_transfers = !self.show_transfers,
            Message::TransfersSnapshot(list) => self.jobs.snapshot(list),
            Message::CancelTransfer(id) => return self.fire(Request::CancelTransfer { id }),
            Message::DismissTransfer(id) => {
                self.jobs.dismiss(id);
                self.show_transfers &= !self.jobs.is_empty();
            }
            Message::Tick => {
                self.jobs.tick();
                self.show_transfers &= !self.jobs.is_empty();
            }
            Message::TransferResult(Ok(())) => {}
            Message::TransferResult(Err(e)) => self.status = Some(e),
            Message::ToggleView => {
                self.view_mode = match self.view_mode {
                    ViewMode::List => ViewMode::Grid,
                    ViewMode::Grid => ViewMode::List,
                };
                self.viewport = None;
                return self.scroll_to_cursor();
            }
            Message::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                let items = self.visible_paths();
                self.sel.retain(&items);
            }
            Message::ToggleCreated => self.show_created = !self.show_created,
            Message::ToggleDetails => self.show_details = !self.show_details,
            Message::Sort(key) => {
                self.sort = if self.sort.0 == key { (key, !self.sort.1) } else { (key, true) };
                self.recent_sorted = true;
                self.resort();
            }
            Message::Noop => {}
        }
        Task::none()
    }

    fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| self.show_hidden || !e.hidden)
    }

    fn visible_paths(&self) -> Vec<PathBuf> {
        self.visible().map(|e| e.path.clone()).collect()
    }

    /// The file or thumbnail icon, with the default app's icon in the
    /// bottom-left corner of files (FastOpen).
    fn entry_icon<'a>(&'a self, e: &'a Entry, size: u16) -> Element<'a, Message> {
        let base: Element<'a, Message> = match self.thumbs.get(&e.path) {
            Some(t) => widget::image(t.handle.clone())
                .width(size)
                .height(size)
                .content_fit(cosmic::iced::ContentFit::Contain)
                .into(),
            None => icons::get(&fmt::icon_names(e), size).into(),
        };
        let Some(app) = e.app.as_ref().filter(|_| e.kind == EntryKind::File) else { return base };
        let badge_px = (size as f32 * 0.45).round().max(10.0) as u16;
        let app_icon: Element<'a, Message> = match &app.icon {
            Some(i) if i.starts_with('/') => widget::icon(widget::icon::from_path(PathBuf::from(i))).size(badge_px).into(),
            Some(i) => icons::get(&[i.as_str(), "application-x-executable"], badge_px).into(),
            None => icons::get(&["application-x-executable"], badge_px).into(),
        };
        let overlay = widget::container(app_icon)
            .width(size)
            .height(size)
            .align_x(Alignment::Start)
            .align_y(Alignment::End);
        widget::tooltip(
            cosmic::iced::widget::stack![base, overlay],
            widget::text::body(format!("Opens with {}", app.name)),
            widget::tooltip::Position::Top,
        )
        .into()
    }

    /// Selection highlight, drop-target outline, click handling, drag source
    /// and (for folders) drop target around one item of either view.
    fn interactive<'a>(
        &'a self,
        i: usize,
        e: &'a Entry,
        content: impl Into<Element<'a, Message>>,
        dragged: &Arc<Vec<PathBuf>>,
    ) -> Element<'a, Message> {
        let selected = self.sel.contains(&e.path);
        let hovered = self.drop_hover.as_deref() == Some(e.path.as_path());
        let mut item = widget::container(content).class(if selected {
            cosmic::style::Container::Primary
        } else {
            cosmic::style::Container::Transparent
        });
        if hovered {
            // Drop target: an accent outline, distinct from selection.
            item = item.class(cosmic::style::Container::custom(|t| {
                let c = t.cosmic();
                widget::container::Style {
                    border: cosmic::iced::Border { color: c.accent_color().into(), width: 2.0, radius: c.radius_s().into() },
                    ..Default::default()
                }
            }));
        }

        let item = widget::mouse_area(item)
            .on_press(Message::Click(i))
            .on_right_press(Message::RightClick(Some(i)))
            .on_middle_press(Message::MiddleClick(i))
            .on_release(Message::Release(i))
            .on_double_click(Message::Activate(i));

        // Only a press on an already-selected item drags (once the pointer has
        // moved a few pixels); a press anywhere else starts a rubber band.
        let id = widget::Id::new(format!("drag:{}", e.path.display()));
        let mut item = widget::DndSource::<Message, DragFiles>::with_id(item, id)
            .on_start(Some(Message::DragStarted))
            .on_cancel(Some(Message::DragEnded))
            .on_finish(Some(Message::DragEnded));
        if selected && self.drag_armed {
            let payload = dragged.clone();
            let icon_names = fmt::icon_names(e);
            let count = payload.len();
            item = item
                .drag_content(move || DragFiles(payload.to_vec()))
                .drag_icon(move |offset| (drag_icon(&icon_names, count), tree::State::None, offset));
        }

        if e.kind == EntryKind::Dir {
            let (drop_to, hover_to, leave_from) = (e.path.clone(), e.path.clone(), e.path.clone());
            widget::dnd_destination::dnd_destination_for_data(item, move |files, action| {
                Message::Dropped(Some(drop_to.clone()), files, action)
            })
            .on_enter(move |_, _, _| Message::DropEnter(hover_to.clone()))
            .on_leave(move || Message::DropLeave(leave_from.clone()))
            .into()
        } else {
            item.into()
        }
    }

    /// The single selected item, if it has no inline preview (thumbnail).
    fn preview_target(&self) -> Option<&Entry> {
        if self.sel.len() != 1 {
            return None;
        }
        self.visible().find(|e| self.sel.contains(&e.path)).filter(|e| !self.thumbs.contains_key(&e.path))
    }

    fn refresh_preview(&mut self) -> cosmic::app::Task<Message> {
        if !self.show_preview {
            return Task::none();
        }
        let Some(path) = self.preview_target().map(|e| e.path.clone()) else { return Task::none() };
        if self.preview.as_ref().is_some_and(|(p, _)| *p == path) {
            return Task::none();
        }
        self.preview = Some((path.clone(), Content::Empty));
        cosmic::task::future(async move {
            let p = path.clone();
            let content = tokio::task::spawn_blocking(move || preview::load(&p))
                .await
                .unwrap_or_else(|e| Content::Error(e.to_string()));
            Message::PreviewLoaded(path, content)
        })
    }

    fn preview_panel<'a>(&'a self, e: &'a Entry) -> Element<'a, Message> {
        let space = cosmic::theme::spacing();
        let mut facts = vec![e.mime.clone().unwrap_or_default()];
        if let Some(s) = e.size {
            facts.push(fmt::size(s));
        }
        facts.push(format!("modified {}", fmt::time(e.modified)));
        if let Some(app) = &e.app {
            facts.push(format!("opens with {}", app.name));
        }
        let header = widget::column::with_capacity(2)
            .spacing(2)
            .push(widget::text::heading(e.name.as_str()))
            .push(widget::text::caption(facts.join(" · ")));

        let content = self.preview.as_ref().filter(|(p, _)| *p == e.path).map(|(_, c)| c);
        // Code and hex dumps keep their lines; the panel scrolls sideways instead.
        let mono = |t: &'a str| widget::text::monotext(t).wrapping(cosmic::iced::widget::text::Wrapping::None);
        let scroll_both = |c: Element<'a, Message>| -> Element<'a, Message> {
            use cosmic::iced::widget::scrollable::{Direction, Scrollbar};
            widget::scrollable(c)
                .direction(Direction::Both { vertical: Scrollbar::default(), horizontal: Scrollbar::default() })
                .into()
        };
        let body: Element<'a, Message> = match content {
            None => widget::text::body("Loading…").into(),
            Some(Content::Text { text, truncated }) => {
                let mut c = widget::column::with_capacity(2).push(mono(text));
                if *truncated {
                    c = c.push(widget::text::caption("… (truncated)"));
                }
                scroll_both(c.into())
            }
            Some(Content::Hex(dump)) => scroll_both(mono(dump).into()),
            Some(Content::Folder { items }) => widget::text::body(format!("Folder with {items} items")).into(),
            Some(Content::Empty) => widget::text::body("Empty file").into(),
            Some(Content::Error(err)) => widget::text::body(err.as_str()).into(),
        };

        widget::container(widget::column::with_capacity(3).spacing(space.space_xs).push(header).push(widget::divider::horizontal::default()).push(body))
            .padding(space.space_xs)
            .width(PREVIEW_W)
            .height(Length::Fill)
            .class(cosmic::style::Container::Card)
            .into()
    }

    /// Thumbnails for image and video files that don't have a current one.
    fn request_thumbnails(&self) -> Vec<cosmic::app::Task<Message>> {
        let Some(client) = self.client.clone() else { return Vec::new() };
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .filter(|e| e.mime.as_deref().is_some_and(noxfm_core::thumbnail::supported))
            .filter(|e| self.thumbs.get(&e.path).is_none_or(|t| t.modified != e.modified))
            .map(|e| {
                let (client, path, modified) = (client.clone(), e.path.clone(), e.modified);
                let tab = self.active_tab();
                cosmic::task::future(async move {
                    let bytes = match client.request(Request::Thumbnail { path: path.clone() }).await {
                        // Read now: the image cache keys on content, so a regenerated
                        // thumbnail at the same cache path is picked up.
                        Ok(Response::Thumbnail { cache_path: Some(p) }) => tokio::fs::read(p).await.ok(),
                        _ => None,
                    };
                    Message::ForTab(tab, Box::new(Message::Thumbnail(path, modified, bytes)))
                })
            })
            .collect()
    }

    fn footer(&self) -> Element<'_, Message> {
        let n = self.visible().count();
        let mut parts = vec![format!("{n} item{}", if n == 1 { "" } else { "s" })];
        match self.sel.len() {
            0 => {}
            1 => {
                if let Some(e) = self.visible().find(|e| self.sel.contains(&e.path)) {
                    parts.push(format!("“{}” selected", e.name));
                    parts.extend(e.mime.clone());
                }
            }
            n => {
                let bytes: u64 = self.visible().filter(|e| self.sel.contains(&e.path)).filter_map(|e| e.size).sum();
                parts.push(format!("{n} selected ({})", fmt::size(bytes)));
            }
        }
        if let Some(s) = &self.status {
            parts.push(s.clone());
        }
        if let Some(fs) = &self.fs {
            parts.push(fs.clone());
        }
        let text = widget::text::caption(parts.join("  ·  ")).width(Length::Fill);
        let mut row = widget::row::with_capacity(2).spacing(12).align_y(Alignment::Center).push(text);

        // While the transfers tab is hidden, a compact indicator opens it.
        let sum = self.jobs.summary();
        if !self.show_transfers && (sum.running > 0 || sum.failed > 0) {
            let mut ind = widget::row::with_capacity(2).spacing(8).align_y(Alignment::Center);
            if sum.running > 0 {
                let label = format!(
                    "{} transfer{} · {:.0}%",
                    sum.running,
                    if sum.running == 1 { "" } else { "s" },
                    sum.fraction * 100.0
                );
                ind = ind.push(widget::text::caption(label)).push(
                    widget::progress_bar::linear::Linear::new().progress(sum.fraction).girth(4.0).width(Length::Fixed(120.0)),
                );
            } else {
                ind = ind.push(widget::text::caption(format!(
                    "{} transfer{} failed",
                    sum.failed,
                    if sum.failed == 1 { "" } else { "s" }
                )));
            }
            row = row.push(
                widget::button::custom(ind).class(cosmic::theme::Button::Text).on_press(Message::ToggleTransfers),
            );
        }
        widget::container(row).padding([4, 8]).into()
    }

    /// Double-click or Enter: folders open in place, files with their app.
    fn activate(&mut self, i: usize) -> cosmic::app::Task<Message> {
        let Some(e) = self.visible().nth(i) else { return Task::none() };
        // Trashed items can't be opened (as in Explorer): show their properties.
        if self.trash_view {
            let path = e.path.clone();
            return self.open_properties(vec![path]);
        }
        if e.kind == EntryKind::Dir {
            return self.load(e.path.clone());
        }
        let Some(client) = self.client.clone() else { return Task::none() };
        let path = e.path.clone();
        cosmic::task::future(async move {
            let r = client.request(Request::OpenWith { path, app: None }).await;
            Message::Opened(r.map(|_| ()).map_err(|e| e.to_string()))
        })
    }

    /// The name field while `e` is being renamed: Enter or clicking away
    /// commits, Escape cancels.
    fn rename_input<'a>(&'a self, e: &Entry) -> Option<Element<'a, Message>> {
        let (path, text) = self.renaming.as_ref()?;
        if *path != e.path {
            return None;
        }
        Some(
            widget::text_input("", text.as_str())
                .id(RENAME_ID.clone())
                .on_input(Message::RenameInput)
                .on_submit(|_| Message::RenameCommit)
                .width(Length::Fill)
                .into(),
        )
    }

    /// "Some features are unavailable", with what to install.
    fn health_banner(&self) -> Option<Element<'_, Message>> {
        if self.missing.is_empty() {
            return None;
        }
        let lines = self.missing.iter().fold(widget::column::with_capacity(self.missing.len()).spacing(2), |c, m| {
            c.push(widget::text::caption(format!("• {} — {}", m.feature, m.detail)))
        });
        let body = widget::column::with_capacity(2)
            .spacing(4)
            .push(widget::text::heading("Some features are unavailable"))
            .push(lines)
            .width(Length::Fill);
        let row = widget::row::with_capacity(3)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(icons::get(&["dialog-warning", "dialog-warning-symbolic"], 24))
            .push(body)
            .push(widget::button::standard("Dismiss").on_press(Message::DismissHealth));
        Some(widget::container(row).padding(8).class(cosmic::style::Container::Card).into())
    }

    /// What a window opened from this one starts with.
    fn layout(&self) -> noxfm_proto::WindowLayout {
        noxfm_proto::WindowLayout { sidebar_open: self.sidebar_open, sidebar_width: self.sidebar_width }
    }

    fn selected_entries(&self) -> Vec<&Entry> {
        self.visible().filter(|e| self.sel.contains(&e.path)).collect()
    }

    fn selected_trash_ids(&self) -> Vec<String> {
        self.selected_entries().iter().filter_map(|e| self.trash_entries.get(&e.path)).map(|t| t.id.clone()).collect()
    }

    /// Re-lists whatever is shown.
    fn reload(&self) -> cosmic::app::Task<Message> {
        if self.trash_view {
            self.load_trash()
        } else if let Some(kind) = self.recent {
            self.load_recent(kind)
        } else {
            self.load(self.path.clone())
        }
    }

    fn load_trash(&self) -> cosmic::app::Task<Message> {
        let tab = self.active_tab();
        self.request_map(Request::ListTrash, move |r| {
            let m = Message::TrashListed(match r {
                Ok(Response::TrashItems(items)) => Ok(items),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e),
            });
            Message::ForTab(tab, Box::new(m))
        })
    }

    fn undo(&self) -> cosmic::app::Task<Message> {
        self.request_map(Request::Undo, |r| {
            Message::Undone(match r {
                Ok(Response::Label(l)) => Ok(l),
                Ok(_) => Ok(None),
                Err(e) => Err(e),
            })
        })
    }

    fn create(&self, kind: NewKind) -> cosmic::app::Task<Message> {
        if self.recent.is_some() || self.trash_view {
            return Task::none();
        }
        self.request_map(Request::Create { dir: self.path.clone(), kind }, |r| {
            Message::Created(match r {
                Ok(Response::Path(p)) => Ok(p),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e),
            })
        })
    }

    fn start_rename(&mut self, path: PathBuf, name: String) -> cosmic::app::Task<Message> {
        if self.trash_view {
            return Task::none();
        }
        self.renaming = Some((path, name));
        Task::batch([widget::text_input::focus(RENAME_ID.clone()), widget::text_input::select_all(RENAME_ID.clone())])
    }

    /// Selects / starts renaming items that were waiting to appear.
    fn apply_pending(&mut self) -> cosmic::app::Task<Message> {
        let items = self.visible_paths();
        let mut task = Task::none();
        if let Some(p) = self.pending_rename.clone()
            && let Some(i) = items.iter().position(|x| *x == p)
        {
            self.pending_rename = None;
            self.sel.click(&items, i, Mods::default());
            let name = self.visible().nth(i).map(|e| e.name.clone()).unwrap_or_default();
            task = Task::batch([self.start_rename(p, name), self.scroll_to_cursor()]);
        }
        if let Some(p) = self.pending_select.clone()
            && let Some(i) = items.iter().position(|x| *x == p)
        {
            self.pending_select = None;
            self.sel.click(&items, i, Mods::default());
            task = Task::batch([task, self.scroll_to_cursor()]);
        }
        task
    }

    fn open_app_picker(&mut self, mime: String, path: Option<PathBuf>) -> cosmic::app::Task<Message> {
        self.dialog = Some(Dialog::AppPicker { mime, path, filter: String::new(), apps: Vec::new(), remember: false });
        self.request_map(Request::AllApps, |r| match r {
            Ok(Response::Apps(apps)) => Message::PickerApps(apps),
            _ => Message::Noop,
        })
    }

    /// Fetches "Open with ▸" apps when a single file of a new type is selected.
    fn refresh_open_with(&mut self) -> cosmic::app::Task<Message> {
        let mime = match self.selected_entries().as_slice() {
            [e] if e.kind == EntryKind::File => e.mime.clone(),
            _ => None,
        };
        let Some(mime) = mime else { return Task::none() };
        if self.open_with.as_ref().is_some_and(|(m, _)| *m == mime) {
            return Task::none();
        }
        self.open_with = Some((mime.clone(), Vec::new()));
        self.request_map(Request::AppsFor { mime: mime.clone() }, move |r| match r {
            Ok(Response::Apps(apps)) => Message::OpenWithApps(mime, apps),
            _ => Message::Noop,
        })
    }

    fn dialog_view(&self) -> Option<Element<'_, Message>> {
        let d = self.dialog.as_ref()?;
        let cancel = widget::button::standard("Cancel").on_press(Message::DialogCancel);
        let confirm = |title: String, body: String, action: &'static str| -> Element<'_, Message> {
            widget::dialog()
                .title(title)
                .body(body)
                .icon(icons::get(&["dialog-warning", "dialog-warning-symbolic"], 48))
                .primary_action(widget::button::destructive(action).on_press(Message::DialogConfirm))
                .secondary_action(widget::button::standard("Cancel").on_press(Message::DialogCancel))
                .into()
        };
        Some(match d {
            Dialog::DeleteForever(paths) => {
                let what = match paths.as_slice() {
                    [one] => format!("“{}”", one.file_name().unwrap_or_default().to_string_lossy()),
                    many => format!("these {} items", many.len()),
                };
                confirm(format!("Delete {what} permanently?"), "They won't go to the Trash, and this can't be undone.".into(), "Delete")
            }
            Dialog::PurgeTrash(ids) => confirm(
                format!("Delete {} permanently?", if ids.len() == 1 { "this item".to_owned() } else { format!("{} items", ids.len()) }),
                "This can't be undone.".into(),
                "Delete",
            ),
            Dialog::EmptyTrash => confirm(
                "Empty the Trash?".into(),
                format!("All {} items in the Trash will be deleted for good.", self.trash_count),
                "Empty Trash",
            ),
            Dialog::AppPicker { mime, path, filter, apps, remember } => {
                let needle = filter.to_lowercase();
                let list = apps
                    .iter()
                    .filter(|a| needle.is_empty() || a.name.to_lowercase().contains(&needle))
                    .take(300)
                    .fold(widget::column::with_capacity(apps.len().min(300)), |c, a| {
                        let icon: Element<'_, Message> = match &a.icon {
                            Some(i) if i.starts_with('/') => widget::icon(widget::icon::from_path(PathBuf::from(i))).size(24).into(),
                            Some(i) => icons::get(&[i.as_str(), "application-x-executable"], 24).into(),
                            None => icons::get(&["application-x-executable"], 24).into(),
                        };
                        let row = widget::row::with_capacity(2).spacing(10).align_y(Alignment::Center).push(icon).push(widget::text::body(a.name.as_str()));
                        c.push(widget::button::custom(row).class(cosmic::theme::Button::Text).width(Length::Fill).on_press(Message::PickerChoose(a.id.clone())))
                    });
                let mut control = widget::column::with_capacity(3)
                    .spacing(8)
                    .push(widget::search_input("Search applications", filter.as_str()).on_input(Message::PickerFilter))
                    .push(widget::container(widget::scrollable(list)).height(320));
                if path.is_some() {
                    control = control.push(
                        widget::checkbox(*remember).label(format!("Always use for {mime}")).on_toggle(Message::PickerRemember),
                    );
                }
                let title = if path.is_some() { "Open with" } else { "Default application" };
                widget::dialog().title(title).control(control).secondary_action(cancel).into()
            }
        })
    }

    fn key(&mut self, key: Key, m: Modifiers) -> cosmic::app::Task<Message> {
        let items = self.visible_paths();
        let mv = |how| (how, m.shift());
        let (row_step, page) = match self.view_mode {
            ViewMode::List => (1, PAGE),
            ViewMode::Grid => {
                let cols = self.grid_cols.get() as isize;
                (cols, cols * 3)
            }
        };
        let movement = match key.as_ref() {
            Key::Named(Named::ArrowDown) => Some(mv(Move::By(row_step))),
            Key::Named(Named::ArrowUp) => Some(mv(Move::By(-row_step))),
            Key::Named(Named::ArrowRight) if self.view_mode == ViewMode::Grid => Some(mv(Move::By(1))),
            Key::Named(Named::ArrowLeft) if self.view_mode == ViewMode::Grid => Some(mv(Move::By(-1))),
            Key::Named(Named::PageDown) => Some(mv(Move::By(page))),
            Key::Named(Named::PageUp) => Some(mv(Move::By(-page))),
            Key::Named(Named::Home) => Some(mv(Move::First)),
            Key::Named(Named::End) => Some(mv(Move::Last)),
            _ => None,
        };
        if let Some((how, extend)) = movement {
            self.sel.move_cursor(&items, how, extend);
            return self.scroll_to_cursor();
        }
        // History, sidebar.
        match key.as_ref() {
            Key::Named(Named::ArrowLeft) if m.alt() => return self.go_back(),
            Key::Named(Named::ArrowRight) if m.alt() => return self.go_forward(),
            Key::Named(Named::F9) => {
                self.sidebar_open = !self.sidebar_open;
                return Task::none();
            }
            _ => {}
        }
        // Tabs.
        match key.as_ref() {
            Key::Character(c) if m.control() && c.eq_ignore_ascii_case("t") => return self.tab_menu_action(TabAction::New),
            Key::Character(c) if m.control() && c.eq_ignore_ascii_case("w") => return self.close_tab(self.active),
            Key::Named(Named::Tab) | Key::Named(Named::PageDown) | Key::Named(Named::PageUp) if m.control() => {
                let back = m.shift() || key == Key::Named(Named::PageUp);
                let n = self.tabs.len();
                let i = if back { (self.active + n - 1) % n } else { (self.active + 1) % n };
                return self.switch_tab(i);
            }
            _ => {}
        }
        // Shortcuts shown in the context menus.
        let ctx = match key.as_ref() {
            Key::Named(Named::Enter) if m.alt() => Some(menu::Ctx::Properties),
            Key::Named(Named::F2) => Some(menu::Ctx::Rename),
            Key::Named(Named::F5) => Some(menu::Ctx::Refresh),
            Key::Named(Named::Delete) if m.shift() => Some(menu::Ctx::DeleteForever),
            Key::Named(Named::Delete) => Some(menu::Ctx::Trash),
            Key::Character(c) if m.control() && m.shift() && c.eq_ignore_ascii_case("n") => Some(menu::Ctx::NewFolder),
            Key::Character(c) if m.control() && m.shift() && c.eq_ignore_ascii_case("c") => Some(menu::Ctx::CopyPath),
            Key::Character(c) if m.control() && c.eq_ignore_ascii_case("z") => Some(menu::Ctx::Undo),
            _ => None,
        };
        if let Some(c) = ctx {
            return self.run_ctx(c);
        }
        match key.as_ref() {
            Key::Named(Named::Enter) => {
                let at = self.sel.cursor().and_then(|c| items.iter().position(|p| p == c));
                if let Some(i) = at {
                    return self.activate(i);
                }
            }
            Key::Named(Named::Backspace) => return self.handle(Message::Up),
            // Space: quick look, as in other file managers.
            Key::Character(c) if c == " " && !m.control() => return self.handle(Message::TogglePreview),
            Key::Character(c) if m.control() => match c.to_lowercase().as_str() {
                "a" => self.sel.select_all(&items),
                "c" => return self.handle(Message::ToClipboard(TransferOp::Copy)),
                "x" => return self.handle(Message::ToClipboard(TransferOp::Move)),
                "v" => return self.handle(Message::Paste),
                "h" => return self.handle(Message::ToggleHidden),
                "l" => return widget::text_input::focus(PATH_ID.clone()),
                "1" => self.view_mode = ViewMode::List,
                "2" => self.view_mode = ViewMode::Grid,
                _ => {}
            },
            _ => {}
        }
        Task::none()
    }

    fn scroll_to_cursor(&self) -> cosmic::app::Task<Message> {
        let items = self.visible_paths();
        let Some(i) = self.sel.cursor().and_then(|c| items.iter().position(|p| p == c)) else {
            return Task::none();
        };
        let r = self.item_rect(i);
        let (top, bottom) = (r.y, r.y + r.height);
        let (offset, height) = self.viewport.unwrap_or((0.0, 320.0));
        let y = if top < offset {
            top
        } else if bottom > offset + height {
            bottom - height
        } else {
            return Task::none();
        };
        cosmic::iced::widget::scrollable::scroll_to(LIST_ID.clone(), AbsoluteOffset { x: None, y: Some(y) })
    }

    fn transfer(&self, op: TransferOp, sources: Vec<PathBuf>, dest: PathBuf) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move {
            let r = client.request(Request::Transfer { op, sources, dest }).await;
            Message::TransferResult(r.map(|_| ()).map_err(|e| e.to_string()))
        })
    }

    fn resort(&mut self) {
        // Recent stays newest-first until the user picks a column.
        if self.recent.is_some() && !self.recent_sorted {
            return;
        }
        noxfm_core::sort_by(&mut self.entries, self.sort.0, self.sort.1);
    }

    fn load_recent(&self, kind: Option<RecentKind>) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        let tab = self.active_tab();
        cosmic::task::future(async move {
            let r = match client.request(Request::Recent { kind, limit: 500 }).await {
                Ok(Response::Recent(items)) => Ok(items),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            Message::ForTab(tab, Box::new(Message::RecentListed(kind, r)))
        })
    }

    fn load_places(&self) -> cosmic::app::Task<Message> {
        let collapsed = self.request_map(Request::SidebarCollapsed, |r| match r {
            Ok(Response::Keys(k)) => Message::SidebarCollapsed(k),
            _ => Message::Noop,
        });
        Task::batch([collapsed, self.load_places_only()])
    }

    fn load_places_only(&self) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move {
            match client.request(Request::Places).await {
                Ok(Response::Places(p)) => Message::Places(p),
                _ => Message::Noop,
            }
        })
    }

    fn load_devices(&self) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move {
            match client.request(Request::ListDevices).await {
                Ok(Response::Devices(d)) => Message::Devices(d),
                _ => Message::Noop,
            }
        })
    }

    /// A request whose only interesting outcome is an error to show.
    fn request(&self, req: Request) -> cosmic::app::Task<Message> {
        self.request_map(req, |r| match r {
            Ok(_) => Message::Noop,
            Err(e) => Message::Opened(Err(e)),
        })
    }

    fn request_map(
        &self,
        req: Request,
        to_msg: impl FnOnce(Result<Response, String>) -> Message + Send + 'static,
    ) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move { to_msg(client.request(req).await.map_err(|e| e.to_string())) })
    }

    fn load(&self, path: PathBuf) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        let tab = self.active_tab();
        cosmic::task::future(async move {
            let r = match client.request(Request::ListDir { path, watch: true }).await {
                Ok(Response::Dir { path, fs, entries }) => Ok((path, fs, entries)),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            Message::ForTab(tab, Box::new(Message::Listed(r)))
        })
    }

    /// Request whose reply we don't care about.
    fn fire(&self, req: Request) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move {
            let _ = client.request(req).await;
            Message::Noop
        })
    }
}

fn recent_title(kind: Option<RecentKind>) -> String {
    match kind {
        None => "Recent".into(),
        Some(RecentKind::Downloaded) => "Recent · Downloaded".into(),
        Some(RecentKind::Modified) => "Recent · Edited".into(),
        Some(RecentKind::Created) => "Recent · Created".into(),
    }
}

fn display(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

/// Move within one filesystem, copy across filesystems; Ctrl forces a copy,
/// and so does a drop the source only allows as a copy.
fn drop_op(sources: &[PathBuf], dest: &Path, action: DndAction, ctrl: bool) -> TransferOp {
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let same_fs = sources.first().and_then(|s| dev(s)).is_some_and(|d| Some(d) == dev(dest));
    if ctrl || !action.contains(DndAction::Move) || !same_fs { TransferOp::Copy } else { TransferOp::Move }
}

fn drag_icon(names: &[String], count: usize) -> Element<'static, ()> {
    let icon = icons::get(names, 32);
    if count <= 1 {
        return icon.into();
    }
    widget::row::with_capacity(2)
        .spacing(4)
        .align_y(Alignment::Center)
        .push(icon)
        .push(badge(count.to_string()))
        .into()
}

fn badge<'a, M: 'a>(label: impl Into<std::borrow::Cow<'a, str>> + 'a) -> Element<'a, M> {
    widget::container(widget::text::caption(label))
        .padding([0, 6])
        .class(cosmic::style::Container::Card)
        .into()
}

/// Files in the templates folder, for "New ▸".
fn load_templates() -> Vec<PathBuf> {
    let home = noxfm_core::complete::home_dir();
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok();
    let dir = noxfm_core::places::templates_dir(&home, user_dirs.as_deref());
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    v.sort();
    v
}
