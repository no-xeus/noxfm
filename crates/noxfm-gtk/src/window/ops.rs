//! File operations on the selection or the shown folder, run by noxd.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use noxfm_proto::{Entry, EntryKind, NewKind, Request, Response, TransferOp};

use super::{Browser, Nav};
use crate::cells::entry_of;
use crate::clipboard;

/// Files opened at once by "Open" on a multi-selection, at most.
const OPEN_MAX: usize = 20;

impl Browser {
    pub(super) fn here(&self) -> PathBuf {
        self.state.borrow().path.clone()
    }

    /// Selected entries, in view order.
    pub(super) fn selected_entries(&self) -> Vec<Entry> {
        let set = self.selection.selection();
        (0..set.size()).filter_map(|i| self.selection.item(set.nth(i as u32))).map(|o| entry_of(&o).clone()).collect()
    }

    pub(super) fn selected_list(&self) -> Vec<PathBuf> {
        self.selected_entries().into_iter().map(|e| e.path).collect()
    }

    pub(super) fn position_of(&self, path: &Path) -> Option<u32> {
        (0..self.selection.n_items()).find(|&i| self.selection.item(i).is_some_and(|o| entry_of(&o).path == path))
    }

    pub(super) fn entry(&self, path: &Path) -> Option<Entry> {
        self.position_of(path).and_then(|i| self.selection.item(i)).map(|o| entry_of(&o).clone())
    }

    /// Selects `path` alone and scrolls to it.
    pub(super) fn select_only(&self, path: &Path) {
        let Some(pos) = self.position_of(path) else { return };
        self.selection.select_item(pos, true);
        let flags = gtk::ListScrollFlags::FOCUS;
        match self.mode.get() {
            super::ViewMode::List => self.list.view.scroll_to(pos, None::<&gtk::ColumnViewColumn>, flags, None),
            super::ViewMode::Grid => self.grid.scroll_to(pos, flags, None),
        }
    }

    /// A short note in the status line (until the next listing).
    pub(super) fn notify(&self, msg: impl Into<String>) {
        self.state.borrow_mut().notice = Some(msg.into());
        self.update_status();
    }

    /// Sends `req`; `then` gets the reply (errors are shown by default).
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

    /// Enter or "Open": one folder opens in place; files with their apps.
    pub(super) fn open_selection(self: &Rc<Self>) {
        let sel = self.selected_entries();
        if let [e] = sel.as_slice()
            && e.kind == EntryKind::Dir
        {
            self.load(e.path.clone(), Nav::New);
            return;
        }
        for e in sel.into_iter().filter(|e| e.kind != EntryKind::Dir).take(OPEN_MAX) {
            self.fire(Request::OpenWith { path: e.path, app: None });
        }
    }

    pub(super) fn open_with(self: &Rc<Self>, i: usize) {
        let app = self.open_with.borrow().1.get(i).map(|a| a.id.clone());
        if let (Some(app), [e]) = (app, self.selected_entries().as_slice()) {
            self.fire(Request::OpenWith { path: e.path.clone(), app: Some(app) });
        }
    }

    pub(super) fn to_clipboard(&self, op: TransferOp) {
        let paths = self.selected_list();
        if paths.is_empty() {
            return;
        }
        clipboard::write(&self.window.clipboard(), op, &paths);
        let cut = if op == TransferOp::Move { paths.iter().cloned().collect() } else { HashSet::new() };
        self.cells.set_cut(cut);
        let n = paths.len();
        let verb = if op == TransferOp::Move { "Cut" } else { "Copied" };
        self.notify(format!("{verb} {n} item{}", if n == 1 { "" } else { "s" }));
    }

    /// Another app (or window) owns the clipboard now: nothing is cut here.
    pub(super) fn clipboard_changed(&self, clipboard: &gdk::Clipboard) {
        if !clipboard.is_local() {
            self.cells.set_cut(HashSet::new());
        }
    }

    pub(super) fn paste(self: &Rc<Self>, as_link: bool) {
        let weak = Rc::downgrade(self);
        let clip = self.window.clipboard();
        glib::spawn_future_local(async move {
            let found = clipboard::read(&clip).await;
            let Some(this) = weak.upgrade() else { return };
            let Some((op, paths)) = found else {
                this.notify("Nothing to paste");
                return;
            };
            let here = this.here();
            if as_link {
                this.fire(Request::Symlink { targets: paths, dir: here });
                return;
            }
            if op == TransferOp::Move {
                // A cut is pasted once, as in other file managers.
                clip.set_content(None::<&gdk::ContentProvider>).ok();
                this.cells.set_cut(HashSet::new());
            }
            this.transfer(op, paths, here);
        });
    }

    pub(super) fn copy_paths(&self) {
        let paths = self.selected_list();
        let text = if paths.is_empty() {
            self.here().display().to_string()
        } else {
            paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")
        };
        self.window.clipboard().set_text(&text);
        self.notify(if paths.len() > 1 { "Paths copied" } else { "Path copied" });
    }

    /// Copy or move into `dest`; moving items already there does nothing.
    pub(super) fn transfer(self: &Rc<Self>, op: TransferOp, sources: Vec<PathBuf>, dest: PathBuf) {
        let sources: Vec<PathBuf> = sources.into_iter().filter(|s| *s != dest).collect();
        if sources.is_empty() || (op == TransferOp::Move && sources.iter().all(|s| s.parent() == Some(dest.as_path()))) {
            return;
        }
        self.fire(Request::Transfer { op, sources, dest });
    }

    /// Where "Send to ▸" copies: Desktop, Documents, mounted removable drives.
    pub(super) fn send_targets(&self) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = self
            .places
            .borrow()
            .iter()
            .filter(|p| !p.pinned && matches!(p.name.as_str(), "Desktop" | "Documents"))
            .map(|p| (p.name.clone(), p.path.clone()))
            .collect();
        for d in self.devices.borrow().iter().filter(|d| !d.internal) {
            if let Some(m) = &d.mount_point {
                let name = match (&d.label, d.partition) {
                    (Some(l), _) => l.clone(),
                    (None, Some(n)) => format!("Partition {n}"),
                    (None, None) => d.device.rsplit('/').next().unwrap_or(&d.device).to_owned(),
                };
                out.push((name, m.clone()));
            }
        }
        out
    }

    pub(super) fn send_to(self: &Rc<Self>, i: usize) {
        if let Some((_, dest)) = self.send_targets().into_iter().nth(i) {
            self.transfer(TransferOp::Copy, self.selected_list(), dest);
        }
    }

    pub(super) fn compress(self: &Rc<Self>) {
        let sources = self.selected_list();
        if !sources.is_empty() {
            self.fire(Request::Compress { sources, dest_dir: self.here() });
        }
    }

    pub(super) fn extract(self: &Rc<Self>) {
        if let [e] = self.selected_entries().as_slice() {
            self.fire(Request::Extract { zip: e.path.clone(), dest_dir: self.here() });
        }
    }

    pub(super) fn create_link(self: &Rc<Self>) {
        let targets = self.selected_list();
        if !targets.is_empty() {
            self.fire(Request::Symlink { targets, dir: self.here() });
        }
    }

    pub(super) fn trash(self: &Rc<Self>) {
        let paths = self.selected_list();
        if !paths.is_empty() {
            self.fire(Request::Trash { paths });
        }
    }

    /// After asking: there's no undo for this.
    pub(super) fn delete_forever(self: &Rc<Self>) {
        let paths = self.selected_list();
        let what = match paths.as_slice() {
            [] => return,
            [one] => format!("“{}”", one.file_name().unwrap_or_default().to_string_lossy()),
            many => format!("these {} items", many.len()),
        };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Delete {what} permanently?"))
            .detail("They won't go to the Trash, and this can't be undone.")
            .buttons(["Cancel", "Delete"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == Ok(1)
                && let Some(this) = weak.upgrade()
            {
                this.fire(Request::DeleteForever { paths });
            }
        });
    }

    pub(super) fn undo(self: &Rc<Self>) {
        self.request_then(Request::Undo, |this, r| {
            if let Response::Label(Some(l)) = r {
                this.notify(format!("Undid {l}"));
            }
        });
    }

    /// The new item is renamed in place as soon as it shows up.
    pub(super) fn create(self: &Rc<Self>, kind: NewKind) {
        self.request_then(Request::Create { dir: self.here(), kind }, |this, r| {
            if let Response::Path(p) = r {
                *this.pending_rename.borrow_mut() = Some(p);
                this.apply_pending();
            }
        });
    }

    /// Starts a rename waiting for its item to be listed.
    pub(super) fn apply_pending(self: &Rc<Self>) {
        let pending = self.pending_rename.borrow().clone();
        if let Some(p) = pending.filter(|p| self.position_of(p).is_some()) {
            self.pending_rename.take();
            self.select_only(&p);
            self.start_rename(p);
        }
    }

    /// The single selected folder, or the shown one.
    pub(super) fn target_dir(&self) -> PathBuf {
        match self.selected_entries().as_slice() {
            [e] if e.kind == EntryKind::Dir => e.path.clone(),
            _ => self.here(),
        }
    }

    pub(super) fn set_pinned(self: &Rc<Self>, pin: bool) {
        let path = self.target_dir();
        self.fire(if pin { Request::Pin { path } } else { Request::Unpin { path } });
    }

    pub(super) fn is_pinned(&self, path: &Path) -> bool {
        self.places.borrow().iter().any(|p| p.pinned && p.path == path)
    }

    pub(super) fn open_terminal(self: &Rc<Self>) {
        self.fire(Request::OpenTerminal { dir: self.target_dir() });
    }

    pub(super) fn open_new_window(self: &Rc<Self>) {
        self.fire(Request::OpenWindow { path: Some(self.target_dir()), view: None, layout: None });
    }

    /// Asks for "Open with ▸" apps of the selected file's type, then runs `then`.
    pub(super) fn with_apps(self: &Rc<Self>, then: impl FnOnce(&Rc<Self>) + 'static) {
        let mime = match self.selected_entries().as_slice() {
            [e] if e.kind == EntryKind::File => e.mime.clone(),
            _ => None,
        };
        let Some(mime) = mime.filter(|m| self.open_with.borrow().0 != *m) else {
            then(self);
            return;
        };
        self.request_then(Request::AppsFor { mime: mime.clone() }, move |this, r| {
            if let Response::Apps(apps) = r {
                *this.open_with.borrow_mut() = (mime, apps);
            }
            then(this);
        });
    }
}

/// Move within one filesystem, copy across filesystems; Ctrl forces a copy.
pub(super) fn drop_op(sources: &[PathBuf], dest: &Path, ctrl: bool) -> TransferOp {
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let same_fs = sources.first().and_then(|s| dev(s)).is_some_and(|d| Some(d) == dev(dest));
    if ctrl || !same_fs { TransferOp::Copy } else { TransferOp::Move }
}

/// Files in the templates folder, for "New ▸".
pub(super) fn load_templates() -> Vec<PathBuf> {
    let home = noxfm_core::complete::home_dir();
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok();
    let dir = noxfm_core::places::templates_dir(&home, user_dirs.as_deref());
    let mut v: Vec<PathBuf> =
        std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()).collect();
    v.sort();
    v
}
