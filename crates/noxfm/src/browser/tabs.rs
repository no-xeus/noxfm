//! Tabs: several locations in one window.
//!
//! The active tab's state lives in the `App` fields; switching tabs swaps
//! those fields with a stored [`TabState`]. Async results carry the id of
//! the tab that asked, so a late answer never lands in another tab.
//!
//! A tab dragged sideways moves along the bar. Dragged well away from the
//! bar (or out of the window), it opens in a window of its own. This uses
//! plain pointer tracking, not Wayland drag-and-drop: while a button is held,
//! the compositor keeps sending pointer events to the window that got the
//! press, even outside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cosmic::iced::{Alignment, Length, Point};
use cosmic::prelude::*;
use cosmic::widget::{self, menu};
use noxfm_core::SortKey;
use noxfm_proto::{Entry, RecentItem, RecentKind, Request, StartView, TrashEntry};

use super::{App, Message, Thumb, ViewMode};
use crate::icons;
use crate::preview::Content;
use crate::selection::Selection;

/// Width of one tab; dragging by this much moves a tab one place.
pub const TAB_W: f32 = 190.0;
/// Pulling a tab this far up or down from the bar detaches it.
const DETACH_DISTANCE: f32 = 70.0;

pub type TabId = u64;

/// Where a tab is, for back/forward.
#[derive(Debug, Clone, PartialEq)]
pub enum Loc {
    Dir(PathBuf),
    Recent(Option<RecentKind>),
    Trash,
}

/// Back/forward entries kept per tab.
pub const HISTORY_DEPTH: usize = 50;

/// Everything that belongs to one tab rather than to the window.
#[derive(Default)]
pub struct TabState {
    pub back: Vec<Loc>,
    pub forward: Vec<Loc>,
    pub subscribed: Option<PathBuf>,
    pub path: PathBuf,
    pub recent: Option<Option<RecentKind>>,
    pub recent_items: HashMap<PathBuf, RecentItem>,
    pub recent_sorted: bool,
    pub trash_view: bool,
    pub trash_entries: HashMap<PathBuf, TrashEntry>,
    pub renaming: Option<(PathBuf, String)>,
    pub pending_rename: Option<PathBuf>,
    pub pending_select: Option<PathBuf>,
    pub path_input: String,
    pub completions: Vec<String>,
    pub entries: Vec<Entry>,
    pub fs: Option<String>,
    pub view_mode: Option<ViewMode>,
    pub thumbs: HashMap<PathBuf, Thumb>,
    pub sel: Selection,
    pub viewport: Option<(f32, f32)>,
    pub show_preview: bool,
    pub preview: Option<(PathBuf, Content)>,
    pub sort: Option<(SortKey, bool)>,
    pub show_hidden: bool,
    pub show_created: bool,
    pub show_details: bool,
}

pub struct TabSlot {
    pub id: TabId,
    /// `None` for the active tab: its state is in the `App` fields.
    pub state: Option<TabState>,
}

/// A tab press that may become a drag.
pub struct TabDrag {
    pub index: usize,
    /// Latest pointer position over the content (content coordinates, unlike
    /// the window coordinates the drag logic uses), for the floating preview.
    pub pointer: Option<Point>,
    /// Moved enough to count as a drag (shows the preview).
    pub moved: bool,
    /// Where the pointer was when the tab last moved (window coordinates);
    /// set on the first move after the press.
    pub origin: Option<Point>,
    pub detaching: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    New,
    Duplicate(usize),
    Detach(usize),
    Close(usize),
    CloseOthers(usize),
}

impl menu::Action for TabAction {
    type Message = Message;

    fn message(&self) -> Message {
        Message::TabMenu(*self)
    }
}

fn tab_menu(i: usize, count: usize) -> Vec<menu::Tree<Message>> {
    let alone = count < 2;
    let item = |label: &'static str, enabled: bool, a: TabAction| {
        if enabled { menu::Item::Button(label, None, a) } else { menu::Item::ButtonDisabled(label, None, a) }
    };
    menu::items(
        &HashMap::new(),
        vec![
            item("New tab", true, TabAction::New),
            item("Duplicate tab", true, TabAction::Duplicate(i)),
            item("Move to new window", !alone, TabAction::Detach(i)),
            menu::Item::Divider,
            item("Close other tabs", !alone, TabAction::CloseOthers(i)),
            item("Close tab", true, TabAction::Close(i)),
        ],
    )
}

/// Where the active tab ends up when the tab at `from` moves to `to`.
fn active_after_move(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && to >= active {
        active - 1
    } else if from > active && to <= active {
        active + 1
    } else {
        active
    }
}

fn title_of(path: &Path, recent: Option<Option<RecentKind>>, trash: bool) -> String {
    if trash {
        return "Trash".into();
    }
    if let Some(kind) = recent {
        return super::recent_title(kind);
    }
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl App {
    pub(super) fn active_tab(&self) -> TabId {
        self.tabs[self.active].id
    }

    /// Moves the active tab's state out of the `App` fields.
    fn take_tab(&mut self) -> TabState {
        TabState {
            back: std::mem::take(&mut self.back),
            forward: std::mem::take(&mut self.forward),
            subscribed: self.subscribed.take(),
            path: std::mem::take(&mut self.path),
            recent: self.recent.take(),
            recent_items: std::mem::take(&mut self.recent_items),
            recent_sorted: self.recent_sorted,
            trash_view: self.trash_view,
            trash_entries: std::mem::take(&mut self.trash_entries),
            renaming: self.renaming.take(),
            pending_rename: self.pending_rename.take(),
            pending_select: self.pending_select.take(),
            path_input: std::mem::take(&mut self.path_input),
            completions: std::mem::take(&mut self.completions),
            entries: std::mem::take(&mut self.entries),
            fs: self.fs.take(),
            view_mode: Some(self.view_mode),
            thumbs: std::mem::take(&mut self.thumbs),
            sel: std::mem::take(&mut self.sel),
            viewport: self.viewport.take(),
            show_preview: self.show_preview,
            preview: self.preview.take(),
            sort: Some(self.sort),
            show_hidden: self.show_hidden,
            show_created: self.show_created,
            show_details: self.show_details,
        }
    }

    /// Makes `t` the active tab's state.
    fn put_tab(&mut self, t: TabState) {
        self.back = t.back;
        self.forward = t.forward;
        self.history_move = false;
        self.subscribed = t.subscribed;
        self.path = t.path;
        self.recent = t.recent;
        self.recent_items = t.recent_items;
        self.recent_sorted = t.recent_sorted;
        self.trash_view = t.trash_view;
        self.trash_entries = t.trash_entries;
        self.renaming = t.renaming;
        self.pending_rename = t.pending_rename;
        self.pending_select = t.pending_select;
        self.path_input = t.path_input;
        self.completions = t.completions;
        self.entries = t.entries;
        self.fs = t.fs;
        self.view_mode = t.view_mode.unwrap_or(self.view_mode);
        self.thumbs = t.thumbs;
        self.sel = t.sel;
        self.viewport = t.viewport;
        self.show_preview = t.show_preview;
        self.preview = t.preview;
        self.sort = t.sort.unwrap_or(self.sort);
        self.show_hidden = t.show_hidden;
        self.show_created = t.show_created;
        self.show_details = t.show_details;
        // Window-wide per-interaction state doesn't carry over.
        self.band = None;
        self.drop_hover = None;
        self.pending_click = None;
    }

    /// A new tab's state: the location, with this tab's view settings.
    fn fresh_tab(&self, path: PathBuf, view: Option<StartView>) -> TabState {
        TabState {
            path_input: super::display(&path),
            path,
            recent: match view {
                Some(StartView::Recent(k)) => Some(k),
                _ => None,
            },
            trash_view: view == Some(StartView::Trash),
            view_mode: Some(self.view_mode),
            sort: Some(self.sort),
            show_hidden: self.show_hidden,
            show_created: self.show_created,
            show_details: self.show_details,
            ..Default::default()
        }
    }

    pub(super) fn tab_title(&self, i: usize) -> String {
        match &self.tabs[i].state {
            None => title_of(&self.path, self.recent, self.trash_view),
            Some(t) => title_of(&t.path, t.recent, t.trash_view),
        }
    }

    fn tab_location(&self, i: usize) -> (PathBuf, Option<StartView>) {
        let (path, recent, trash) = match &self.tabs[i].state {
            None => (self.path.clone(), self.recent, self.trash_view),
            Some(t) => (t.path.clone(), t.recent, t.trash_view),
        };
        let view = if trash { Some(StartView::Trash) } else { recent.map(StartView::Recent) };
        (path, view)
    }

    /// Shows tab `i`, re-listing its location (it may have changed meanwhile).
    pub(super) fn switch_tab(&mut self, i: usize) -> cosmic::app::Task<Message> {
        if i == self.active || i >= self.tabs.len() {
            return Task::none();
        }
        let current = self.take_tab();
        self.tabs[self.active].state = Some(current);
        let next = self.tabs[i].state.take().unwrap_or_default();
        self.active = i;
        self.put_tab(next);
        let offset = self.viewport.map_or(0.0, |(o, _)| o);
        Task::batch([
            self.reload(),
            cosmic::iced::widget::scrollable::scroll_to(
                super::LIST_ID.clone(),
                cosmic::iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(offset) },
            ),
        ])
    }

    /// Opens a tab after the active one and switches to it.
    pub(super) fn open_tab(&mut self, path: PathBuf, view: Option<StartView>) -> cosmic::app::Task<Message> {
        let fresh = self.fresh_tab(path, view);
        self.next_tab += 1;
        let at = self.active + 1;
        self.tabs.insert(at, TabSlot { id: self.next_tab, state: Some(fresh) });
        // `switch_tab` needs `active` to still point at the current tab.
        self.switch_tab(at)
    }

    pub(super) fn close_tab(&mut self, i: usize) -> cosmic::app::Task<Message> {
        if i >= self.tabs.len() {
            return Task::none();
        }
        if self.tabs.len() == 1 {
            // The last tab: close the window.
            return match self.core.main_window_id() {
                Some(id) => cosmic::iced::window::close(id),
                None => Task::none(),
            };
        }
        let mut tasks = Vec::new();
        if i == self.active {
            let neighbour = if i + 1 < self.tabs.len() { i + 1 } else { i - 1 };
            tasks.push(self.switch_tab(neighbour));
        }
        let slot = self.tabs.remove(i);
        if i < self.active {
            self.active -= 1;
        }
        if let Some(path) = slot.state.and_then(|s| s.subscribed).filter(|p| !self.watching(p)) {
            tasks.push(self.fire(Request::Unsubscribe { path }));
        }
        Task::batch(tasks)
    }

    /// Some other tab still watches `path`.
    pub(super) fn watching(&self, path: &Path) -> bool {
        self.subscribed.as_deref() == Some(path)
            || self.tabs.iter().filter_map(|t| t.state.as_ref()).any(|s| s.subscribed.as_deref() == Some(path))
    }

    /// Opens tab `i` in a new window and closes it here.
    pub(super) fn detach_tab(&mut self, i: usize) -> cosmic::app::Task<Message> {
        if self.tabs.len() < 2 || i >= self.tabs.len() {
            return Task::none();
        }
        let (path, view) = self.tab_location(i);
        let open = self.request(Request::OpenWindow { path: Some(path), view, layout: Some(self.layout()) });
        Task::batch([open, self.close_tab(i)])
    }

    pub(super) fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || to >= self.tabs.len() {
            return;
        }
        let slot = self.tabs.remove(from);
        self.tabs.insert(to, slot);
        self.active = active_after_move(self.active, from, to);
    }

    pub(super) fn tab_menu_action(&mut self, a: TabAction) -> cosmic::app::Task<Message> {
        match a {
            TabAction::New => {
                let (path, view) = self.tab_location(self.active);
                self.open_tab(path, view)
            }
            TabAction::Duplicate(i) => {
                let (path, view) = self.tab_location(i);
                self.open_tab(path, view)
            }
            TabAction::Detach(i) => self.detach_tab(i),
            TabAction::Close(i) => self.close_tab(i),
            TabAction::CloseOthers(i) => {
                let keep = self.tabs[i].id;
                let mut tasks = vec![self.switch_tab(i)];
                while let Some(j) = self.tabs.iter().position(|t| t.id != keep) {
                    tasks.push(self.close_tab(j));
                }
                Task::batch(tasks)
            }
        }
    }

    /// Pointer moved while a tab is held: reorder or arm detaching.
    pub(super) fn tab_drag_move(&mut self, p: Point) {
        let Some(drag) = &mut self.tab_drag else { return };
        let Some(origin) = drag.origin else {
            drag.origin = Some(p);
            return;
        };
        let (dx, dy) = (p.x - origin.x, p.y - origin.y);
        drag.moved |= dx.hypot(dy) > 6.0;
        drag.detaching = dy.abs() > DETACH_DISTANCE && self.tabs.len() > 1;
        if drag.detaching {
            return;
        }
        let shift = (dx / TAB_W).round() as isize;
        if shift != 0 {
            let from = drag.index;
            let to = (from as isize + shift).clamp(0, self.tabs.len() as isize - 1) as usize;
            if let Some(d) = &mut self.tab_drag {
                d.index = to;
                d.origin = Some(Point::new(origin.x + (to as f32 - from as f32) * TAB_W, origin.y));
            }
            self.move_tab(from, to);
        }
    }

    /// Button released: a tab pulled away from the bar gets its own window.
    pub(super) fn tab_drag_end(&mut self) -> cosmic::app::Task<Message> {
        match self.tab_drag.take() {
            Some(TabDrag { index, detaching: true, .. }) => self.detach_tab(index),
            _ => Task::none(),
        }
    }

    pub(super) fn current_loc(&self) -> Loc {
        if self.trash_view {
            Loc::Trash
        } else if let Some(k) = self.recent {
            Loc::Recent(k)
        } else {
            Loc::Dir(self.path.clone())
        }
    }

    /// The shown location is about to become `new`: remember where we were,
    /// unless this move is itself a step back or forward.
    pub(super) fn record_move(&mut self, new: &Loc) {
        let moving_in_history = std::mem::take(&mut self.history_move);
        let cur = self.current_loc();
        if cur == *new || moving_in_history {
            return;
        }
        self.back.push(cur);
        if self.back.len() > HISTORY_DEPTH {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    fn go(&mut self, loc: Loc) -> cosmic::app::Task<Message> {
        self.history_move = true;
        match loc {
            Loc::Dir(p) => self.load(p),
            Loc::Recent(k) => self.handle(Message::OpenRecent(k)),
            Loc::Trash => self.handle(Message::OpenTrash),
        }
    }

    pub(super) fn go_back(&mut self) -> cosmic::app::Task<Message> {
        let Some(loc) = self.back.pop() else { return Task::none() };
        self.forward.push(self.current_loc());
        self.go(loc)
    }

    pub(super) fn go_forward(&mut self) -> cosmic::app::Task<Message> {
        let Some(loc) = self.forward.pop() else { return Task::none() };
        self.back.push(self.current_loc());
        self.go(loc)
    }

    /// The tab being dragged, drawn small under the pointer.
    pub(super) fn tab_drag_preview(&self) -> Option<Element<'_, Message>> {
        let drag = self.tab_drag.as_ref().filter(|d| d.moved)?;
        let p = drag.pointer?;
        // The dragged tab is the active one (pressing it switched to it).
        let icon: &[&str] = if self.trash_view {
            &["user-trash"]
        } else if self.recent.is_some() {
            &["document-open-recent"]
        } else {
            &["folder"]
        };
        let header = widget::row::with_capacity(2)
            .spacing(6)
            .align_y(Alignment::Center)
            .push(icons::get(icon, 16))
            .push(widget::text::heading(self.tab_title(self.active)).wrapping(cosmic::iced::widget::text::Wrapping::None));
        let list = self.visible().take(7).fold(widget::column::with_capacity(7).spacing(2), |c, e| {
            c.push(
                widget::row::with_capacity(2)
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .push(icons::get(&crate::fmt::icon_names(e), 12))
                    .push(widget::text::caption(e.name.as_str()).wrapping(cosmic::iced::widget::text::Wrapping::None)),
            )
        });
        let mut body = widget::column::with_capacity(4)
            .spacing(6)
            .push(header)
            .push(widget::divider::horizontal::light())
            .push(widget::container(list).height(Length::Fill).clip(true));
        if drag.detaching {
            body = body.push(widget::text::caption("Release: new window"));
        }
        let card = widget::container(body)
            .padding(8)
            .width(220)
            .height(160)
            .clip(true)
            .class(cosmic::style::Container::Dialog(true));
        // Just below-right of the pointer, so it doesn't hide what's under it.
        Some(
            widget::container(card)
                .padding(cosmic::iced::Padding { top: p.y + 14.0, left: p.x + 14.0, right: 0.0, bottom: 0.0 })
                .into(),
        )
    }

    pub(super) fn tab_bar(&self) -> Option<Element<'_, Message>> {
        if self.tabs.len() < 2 {
            return None;
        }
        let detaching = self.tab_drag.as_ref().filter(|d| d.detaching).map(|d| d.index);
        let mut row = widget::row::with_capacity(self.tabs.len() + 1).spacing(4).align_y(Alignment::Center);
        for i in 0..self.tabs.len() {
            let active = i == self.active;
            let icon: &[&str] = match &self.tabs[i].state {
                None if self.trash_view => &["user-trash"],
                None if self.recent.is_some() => &["document-open-recent"],
                Some(t) if t.trash_view => &["user-trash"],
                Some(t) if t.recent.is_some() => &["document-open-recent"],
                _ => &["folder"],
            };
            let close = widget::button::icon(icons::handle(&["window-close-symbolic"], 12))
                .padding(2)
                .on_press(Message::CloseTab(i));
            let content = widget::row::with_capacity(3)
                .spacing(6)
                .align_y(Alignment::Center)
                .push(icons::get(icon, 16))
                .push(
                    widget::container(widget::text::body(self.tab_title(i)).wrapping(cosmic::iced::widget::text::Wrapping::None))
                        .width(Length::Fill)
                        .clip(true),
                )
                .push(close);
            let tab = widget::container(content)
                .padding([4, 8])
                .width(TAB_W - 4.0)
                .class(if detaching == Some(i) {
                    cosmic::style::Container::custom(|t| {
                        let c = t.cosmic();
                        widget::container::Style {
                            border: cosmic::iced::Border { color: c.accent_color().into(), width: 2.0, radius: c.radius_s().into() },
                            ..Default::default()
                        }
                    })
                } else if active {
                    // Accent-tinted, so the current tab is obvious.
                    cosmic::style::Container::custom(|t| {
                        let c = t.cosmic();
                        let mut bg: cosmic::iced::Color = c.accent_color().into();
                        bg.a = 0.22;
                        widget::container::Style {
                            background: Some(bg.into()),
                            border: cosmic::iced::Border { color: c.accent_color().into(), width: 1.0, radius: c.radius_s().into() },
                            ..Default::default()
                        }
                    })
                } else {
                    cosmic::style::Container::Card
                });
            let tab = widget::mouse_area(tab)
                .on_press(Message::TabPress(i))
                .on_middle_press(Message::CloseTab(i));
            let mut m = widget::context_menu(tab, Some(tab_menu(i, self.tabs.len()))).on_surface_action(Message::Surface);
            if let Some(id) = self.core.main_window_id() {
                m = m.window_id(id);
            }
            row = row.push(m);
        }
        row = row.push(
            widget::tooltip(
                widget::button::icon(icons::handle(&["tab-new-symbolic", "list-add-symbolic"], 16)).on_press(Message::TabMenu(TabAction::New)),
                widget::text::body("New tab (Ctrl+T)"),
                widget::tooltip::Position::Bottom,
            ),
        );
        let mut col = widget::column::with_capacity(2).push(widget::scrollable(row).direction(
            cosmic::iced::widget::scrollable::Direction::Horizontal(cosmic::iced::widget::scrollable::Scrollbar::new().width(4).scroller_width(4)),
        ));
        if detaching.is_some() {
            col = col.push(widget::text::caption("Release to open this tab in a new window"));
        }
        Some(col.into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn moving_keeps_the_active_tab() {
        // Simulate on a list of ids and check the active id is unchanged.
        for n in 1..6usize {
            for active in 0..n {
                for from in 0..n {
                    for to in 0..n {
                        let mut tabs: Vec<usize> = (0..n).collect();
                        let id = tabs[active];
                        let t = tabs.remove(from);
                        tabs.insert(to, t);
                        assert_eq!(tabs[super::active_after_move(active, from, to)], id, "n={n} a={active} {from}->{to}");
                    }
                }
            }
        }
    }

    #[test]
    fn titles() {
        use std::path::Path;
        assert_eq!(super::title_of(Path::new("/home/u/Music"), None, false), "Music");
        assert_eq!(super::title_of(Path::new("/"), None, false), "/");
        assert_eq!(super::title_of(Path::new("/x"), None, true), "Trash");
        assert_eq!(super::title_of(Path::new("/x"), Some(None), false), "Recent");
    }
}
