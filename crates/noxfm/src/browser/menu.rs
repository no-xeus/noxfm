//! Right-click menus for items and the folder background, modelled on the
//! Windows 10 Explorer menu.

use std::collections::HashMap;
use std::path::PathBuf;

use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::prelude::*;
use cosmic::widget::menu::{self, KeyBind, key_bind::Modifier};
use noxfm_core::SortKey;
use noxfm_proto::{EntryKind, NewKind, Request, TransferOp};

use super::{App, Dialog, Message, ViewMode};

/// Menu actions. They must be `Copy`, so anything variable (which app,
/// which template, which destination) is an index into lists the `App`
/// keeps while the menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Open,
    OpenWith(usize),
    OpenWithOther,
    Preview,
    Cut,
    Copy,
    CopyPath,
    SendTo(usize),
    Compress,
    ExtractHere,
    CreateLink,
    Rename,
    Trash,
    DeleteForever,
    Properties,
    OpenNewWindow,
    OpenNewTab,
    OpenTerminal,
    Pin,
    Unpin,
    View(ViewMode),
    Sort(SortKey),
    SortAscending(bool),
    ToggleHidden,
    ToggleCreated,
    ToggleDetails,
    Refresh,
    Paste,
    PasteLink,
    Undo,
    NewFolder,
    NewFile,
    NewText,
    NewTemplate(usize),
    OpenLocation,
    RemoveFromRecent,
    Restore,
    EmptyTrash,
}

impl menu::Action for Ctx {
    type Message = Message;

    fn message(&self) -> Message {
        Message::Ctx(*self)
    }
}

/// Shortcuts shown next to menu items (handled in `App::key`).
fn key_binds() -> HashMap<KeyBind, Ctx> {
    let k = |mods: &[Modifier], key: Key, a: Ctx| (KeyBind { modifiers: mods.to_vec(), key }, a);
    let ch = |c: &str| Key::Character(c.into());
    use Modifier::{Alt, Ctrl, Shift};
    HashMap::from([
        k(&[], Key::Named(Named::Enter), Ctx::Open),
        k(&[], ch(" "), Ctx::Preview),
        k(&[Ctrl], ch("x"), Ctx::Cut),
        k(&[Ctrl], ch("c"), Ctx::Copy),
        k(&[Ctrl, Shift], ch("c"), Ctx::CopyPath),
        k(&[Ctrl], ch("v"), Ctx::Paste),
        k(&[Ctrl], ch("z"), Ctx::Undo),
        k(&[], Key::Named(Named::F2), Ctx::Rename),
        k(&[], Key::Named(Named::Delete), Ctx::Trash),
        k(&[Shift], Key::Named(Named::Delete), Ctx::DeleteForever),
        k(&[Alt], Key::Named(Named::Enter), Ctx::Properties),
        k(&[], Key::Named(Named::F5), Ctx::Refresh),
        k(&[Ctrl, Shift], ch("n"), Ctx::NewFolder),
        k(&[Ctrl], ch("h"), Ctx::ToggleHidden),
    ])
}

type Item = menu::Item<Ctx, String>;

fn button(label: impl Into<String>, a: Ctx) -> Item {
    menu::Item::Button(label.into(), None, a)
}

fn button_if(enabled: bool, label: impl Into<String>, a: Ctx) -> Item {
    if enabled { button(label, a) } else { menu::Item::ButtonDisabled(label.into(), None, a) }
}

fn check(label: impl Into<String>, on: bool, a: Ctx) -> Item {
    menu::Item::CheckBox(label.into(), None, on, a)
}

fn folder(label: impl Into<String>, items: Vec<Item>) -> Item {
    menu::Item::Folder(label.into(), items)
}

/// Which menu a right-click gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Background,
    File,
    Folder,
    Many,
}

impl App {
    fn target(&self) -> Target {
        let sel = self.selected_entries();
        match sel.as_slice() {
            [] => Target::Background,
            [e] if e.kind == EntryKind::Dir => Target::Folder,
            [_] => Target::File,
            _ => Target::Many,
        }
    }

    /// Where "Send to ▸" can copy: Desktop, Documents, mounted plugged-in drives.
    pub(super) fn send_targets(&self) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = self
            .places
            .iter()
            .filter(|p| !p.pinned && matches!(p.name.as_str(), "Desktop" | "Documents"))
            .map(|p| (p.name.clone(), p.path.clone()))
            .collect();
        for d in self.devices.iter().filter(|d| !d.internal) {
            if let Some(m) = &d.mount_point {
                out.push((super::sidebar::partition_name(d), m.clone()));
            }
        }
        out
    }

    pub(super) fn context_menu(&self) -> Vec<menu::Tree<Message>> {
        let items = if self.trash_view {
            self.trash_menu()
        } else {
            match self.target() {
                Target::Background => self.background_menu(),
                t => self.item_menu(t),
            }
        };
        menu::items(&key_binds(), items)
    }

    fn item_menu(&self, t: Target) -> Vec<Item> {
        let sel = self.selected_entries();
        let first = sel[0];
        let in_recent = self.recent.is_some();
        let mut v = Vec::new();

        // Open, with the app named, as Explorer shows it.
        let open_label = match (t, &first.app) {
            (Target::File, Some(app)) => format!("Open with {}", app.name),
            _ => "Open".into(),
        };
        v.push(button(open_label, Ctx::Open));
        if t == Target::Folder {
            v.push(button("Open in new tab", Ctx::OpenNewTab));
            v.push(button("Open in new window", Ctx::OpenNewWindow));
            v.push(button("Open in terminal", Ctx::OpenTerminal));
        }
        if t == Target::File {
            let mut apps: Vec<Item> = self
                .open_with_apps(first)
                .iter()
                .enumerate()
                .map(|(i, a)| button(a.name.clone(), Ctx::OpenWith(i)))
                .collect();
            if !apps.is_empty() {
                apps.push(menu::Item::Divider);
            }
            apps.push(button("Other application…", Ctx::OpenWithOther));
            v.push(folder("Open with", apps));
            if !self.thumbs.contains_key(&first.path) {
                v.push(button("Preview", Ctx::Preview));
            }
            if first.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                v.push(button("Extract here", Ctx::ExtractHere));
            }
        }
        if in_recent {
            v.push(button("Open file location", Ctx::OpenLocation));
            v.push(button("Remove from Recent", Ctx::RemoveFromRecent));
        }
        v.push(menu::Item::Divider);
        v.push(button("Cut", Ctx::Cut));
        v.push(button("Copy", Ctx::Copy));
        v.push(button(if t == Target::Many { "Copy paths" } else { "Copy path" }, Ctx::CopyPath));
        v.push(menu::Item::Divider);

        let mut send: Vec<Item> = self.send_targets().into_iter().enumerate().map(|(i, (n, _))| button(n, Ctx::SendTo(i))).collect();
        if !send.is_empty() {
            send.push(menu::Item::Divider);
        }
        send.push(button("Compressed archive (.zip)", Ctx::Compress));
        v.push(folder("Send to", send));
        v.push(button_if(!in_recent, "Create link", Ctx::CreateLink));
        if t == Target::Folder {
            let pinned = self.places.iter().any(|p| p.pinned && p.path == first.path);
            v.push(if pinned { button("Unpin from sidebar", Ctx::Unpin) } else { button("Pin to sidebar", Ctx::Pin) });
        }
        v.push(menu::Item::Divider);
        v.push(button_if(t != Target::Many, "Rename", Ctx::Rename));
        v.push(button("Move to Trash", Ctx::Trash));
        v.push(button("Delete permanently…", Ctx::DeleteForever));
        v.push(menu::Item::Divider);
        v.push(button("Properties", Ctx::Properties));
        v
    }

    fn view_items(&self) -> Vec<Item> {
        let (key, asc) = self.sort;
        let sort = |label: &str, k: SortKey| check(label, key == k, Ctx::Sort(k));
        vec![
            folder(
                "View",
                vec![
                    check("List", self.view_mode == ViewMode::List, Ctx::View(ViewMode::List)),
                    check("Icons", self.view_mode == ViewMode::Grid, Ctx::View(ViewMode::Grid)),
                ],
            ),
            folder(
                "Sort by",
                vec![
                    sort("Name", SortKey::Name),
                    sort("Type", SortKey::Extension),
                    sort("Size", SortKey::Size),
                    sort("Date modified", SortKey::Modified),
                    sort("Date created", SortKey::Created),
                    menu::Item::Divider,
                    check("Ascending", asc, Ctx::SortAscending(true)),
                    check("Descending", !asc, Ctx::SortAscending(false)),
                ],
            ),
            folder(
                "Show",
                vec![
                    check("Hidden files", self.show_hidden, Ctx::ToggleHidden),
                    check("Date created", self.show_created, Ctx::ToggleCreated),
                    check("Owner and permissions", self.show_details, Ctx::ToggleDetails),
                ],
            ),
            button("Refresh", Ctx::Refresh),
        ]
    }

    fn background_menu(&self) -> Vec<Item> {
        let mut v = self.view_items();
        if self.recent.is_some() {
            return v;
        }
        v.push(menu::Item::Divider);
        v.push(button("Paste", Ctx::Paste));
        v.push(button("Paste as link", Ctx::PasteLink));
        v.push(match &self.undo_label {
            Some(l) => button(format!("Undo {l}"), Ctx::Undo),
            None => menu::Item::ButtonDisabled("Undo".into(), None, Ctx::Undo),
        });
        v.push(menu::Item::Divider);
        let mut new = vec![
            button("Folder", Ctx::NewFolder),
            button("Empty file", Ctx::NewFile),
            button("Text document", Ctx::NewText),
        ];
        if !self.templates.is_empty() {
            new.push(menu::Item::Divider);
            for (i, t) in self.templates.iter().enumerate() {
                let name = t.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                new.push(button(name, Ctx::NewTemplate(i)));
            }
        }
        v.push(folder("New", new));
        v.push(button("Open in terminal", Ctx::OpenTerminal));
        let pinned = self.places.iter().any(|p| p.pinned && p.path == self.path);
        v.push(if pinned { button("Unpin from sidebar", Ctx::Unpin) } else { button("Pin to sidebar", Ctx::Pin) });
        v.push(menu::Item::Divider);
        v.push(button("Properties", Ctx::Properties));
        v
    }

    fn trash_menu(&self) -> Vec<Item> {
        if self.sel.is_empty() {
            let mut v = self.view_items();
            v.push(menu::Item::Divider);
            v.push(button_if(!self.entries.is_empty(), "Empty Trash…", Ctx::EmptyTrash));
            return v;
        }
        vec![
            button("Restore", Ctx::Restore),
            menu::Item::Divider,
            button("Delete permanently…", Ctx::DeleteForever),
            menu::Item::Divider,
            button("Properties", Ctx::Properties),
        ]
    }

    fn open_with_apps(&self, e: &noxfm_proto::Entry) -> &[noxfm_proto::AppRef] {
        match (&self.open_with, &e.mime) {
            (Some((m, apps)), Some(mime)) if m == mime => apps,
            _ => &[],
        }
    }

    /// Runs a menu action (or its keyboard shortcut).
    pub(super) fn run_ctx(&mut self, c: Ctx) -> cosmic::app::Task<Message> {
        let paths = self.sel.paths(&self.visible_paths());
        let single = self.selected_entries().first().map(|e| (*e).clone());
        let here = self.path.clone();
        match c {
            Ctx::Open => {
                let items = self.visible_paths();
                return match &single {
                    Some(e) if paths.len() == 1 => {
                        let i = items.iter().position(|p| *p == e.path).unwrap_or(0);
                        self.handle(Message::Activate(i))
                    }
                    _ => Task::batch(paths.into_iter().take(20).map(|p| self.request(Request::OpenWith { path: p, app: None }))),
                };
            }
            Ctx::OpenWith(i) => {
                let Some(e) = &single else { return Task::none() };
                let Some(app) = self.open_with_apps(e).get(i).cloned() else { return Task::none() };
                return self.request(Request::OpenWith { path: e.path.clone(), app: Some(app.id) });
            }
            Ctx::OpenWithOther => {
                let Some(e) = single else { return Task::none() };
                let mime = e.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
                return self.open_app_picker(mime, Some(e.path));
            }
            Ctx::Preview => return self.handle(Message::TogglePreview),
            Ctx::Cut => return self.handle(Message::ToClipboard(TransferOp::Move)),
            Ctx::Copy => return self.handle(Message::ToClipboard(TransferOp::Copy)),
            Ctx::CopyPath => {
                let text: Vec<String> = if paths.is_empty() { vec![here.display().to_string()] } else { paths.iter().map(|p| p.display().to_string()).collect() };
                self.status = Some("Path copied".into());
                return cosmic::iced::clipboard::write(text.join("\n"));
            }
            Ctx::SendTo(i) => {
                if let Some((_, dest)) = self.send_targets().get(i).cloned() {
                    return self.transfer(TransferOp::Copy, paths, dest);
                }
            }
            Ctx::Compress => return self.request(Request::Compress { sources: paths, dest_dir: here }),
            Ctx::ExtractHere => {
                if let Some(e) = single {
                    return self.request(Request::Extract { zip: e.path, dest_dir: here });
                }
            }
            Ctx::CreateLink => return self.request(Request::Symlink { targets: paths, dir: here }),
            Ctx::Rename => {
                if let Some(e) = single {
                    return self.start_rename(e.path, e.name);
                }
            }
            Ctx::Trash => {
                if !paths.is_empty() && !self.trash_view {
                    return self.request(Request::Trash { paths });
                }
            }
            Ctx::DeleteForever => {
                if self.trash_view {
                    let ids = self.selected_trash_ids();
                    if !ids.is_empty() {
                        self.dialog = Some(Dialog::PurgeTrash(ids));
                    }
                } else if !paths.is_empty() {
                    self.dialog = Some(Dialog::DeleteForever(paths));
                }
            }
            Ctx::Properties => {
                let targets = if paths.is_empty() { vec![here] } else { paths };
                return self.open_properties(targets);
            }
            Ctx::OpenNewTab => {
                let dir = single.map_or(here, |e| e.path);
                return self.open_tab(dir, None);
            }
            Ctx::OpenNewWindow => {
                let dir = single.map_or(here, |e| e.path);
                return self.request(Request::OpenWindow { path: Some(dir), view: None, layout: Some(self.layout()) });
            }
            Ctx::OpenTerminal => {
                let dir = single.filter(|e| e.kind == EntryKind::Dir).map_or(here, |e| e.path);
                return self.request(Request::OpenTerminal { dir });
            }
            Ctx::Pin | Ctx::Unpin => {
                let dir = single.filter(|e| e.kind == EntryKind::Dir).map_or(here, |e| e.path);
                let req = if c == Ctx::Pin { Request::Pin { path: dir } } else { Request::Unpin { path: dir } };
                return self.request(req);
            }
            Ctx::View(m) => self.view_mode = m,
            Ctx::Sort(k) => {
                self.sort.0 = k;
                self.recent_sorted = true;
                self.resort();
            }
            Ctx::SortAscending(asc) => {
                self.sort.1 = asc;
                self.recent_sorted = true;
                self.resort();
            }
            Ctx::ToggleHidden => return self.handle(Message::ToggleHidden),
            Ctx::ToggleCreated => self.show_created = !self.show_created,
            Ctx::ToggleDetails => self.show_details = !self.show_details,
            Ctx::Refresh => return self.reload(),
            Ctx::Paste => return self.handle(Message::Paste),
            Ctx::PasteLink => {
                return cosmic::iced::clipboard::read_data::<crate::clipboard::ClipboardFiles>()
                    .map(|c| cosmic::Action::App(Message::PastedLink(c)));
            }
            Ctx::Undo => return self.undo(),
            Ctx::NewFolder => return self.create(NewKind::Folder),
            Ctx::NewFile => return self.create(NewKind::EmptyFile),
            Ctx::NewText => return self.create(NewKind::TextDocument),
            Ctx::NewTemplate(i) => {
                if let Some(t) = self.templates.get(i).cloned() {
                    return self.create(NewKind::Template(t));
                }
            }
            Ctx::OpenLocation => {
                if let Some(e) = single
                    && let Some(parent) = e.path.parent()
                {
                    self.pending_select = Some(e.path.clone());
                    return self.load(parent.to_path_buf());
                }
            }
            Ctx::RemoveFromRecent => {
                return Task::batch(paths.into_iter().map(|path| self.request(Request::ForgetRecent { path })));
            }
            Ctx::Restore => {
                let ids = self.selected_trash_ids();
                return self.request(Request::RestoreTrash { ids });
            }
            Ctx::EmptyTrash => self.dialog = Some(Dialog::EmptyTrash),
        }
        Task::none()
    }
}
