//! Drag and drop of files, within noxfm and with other apps.
//!
//! A press on a selected item drags the selection; a press anywhere else
//! selects or draws a rubber band. Dropping on a folder puts the files in
//! it, elsewhere in the shown folder: moved within one filesystem, copied
//! across filesystems or with Ctrl held.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio};
use noxfm_proto::{EntryKind, TransferOp};

use super::Browser;
use super::ops::drop_op;

fn paths(files: &gdk::FileList) -> Vec<PathBuf> {
    files.files().iter().filter_map(|f| f.path()).collect()
}

impl Browser {
    pub(super) fn connect_dnd(self: &Rc<Self>, view: &gtk::Widget) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let (weak, v) = (Rc::downgrade(self), view.clone());
        source.connect_prepare(move |source, x, y| {
            let b = weak.upgrade()?;
            let pressed = b.cells.path_at(&v, x, y)?;
            let sel = b.selected_list();
            if !sel.contains(&pressed) {
                return None;
            }
            if let Some(e) = b.entry(&pressed) {
                let names = noxfm_core::fmt::icon_names(&e);
                let fallbacks: Vec<&str> = names[1..].iter().map(String::as_str).collect();
                let icon = gtk::IconTheme::for_display(&v.display()).lookup_icon(
                    &names[0],
                    &fallbacks,
                    32,
                    v.scale_factor(),
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                );
                source.set_icon(Some(&icon), 0, 0);
            }
            let files: Vec<gio::File> = sel.iter().map(gio::File::for_path).collect();
            Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()))
        });
        view.add_controller(source);

        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        // The files are known while hovering, to pick move or copy.
        target.set_preload(true);
        let (weak, v) = (Rc::downgrade(self), view.clone());
        target.connect_motion(move |t, x, y| {
            let Some(b) = weak.upgrade() else { return gdk::DragAction::empty() };
            let dest = b.drop_dest(&v, x, y);
            b.cells.set_drop_hover(Some(dest.clone()).filter(|d| *d != b.here()));
            let sources = t.value().and_then(|v| v.get::<gdk::FileList>().ok()).map(|f| paths(&f));
            match sources {
                Some(s) => action(&s, &dest, ctrl(t)),
                // Not read yet: offer both, the drop decides.
                None => gdk::DragAction::COPY,
            }
        });
        let weak = Rc::downgrade(self);
        target.connect_leave(move |_| {
            if let Some(b) = weak.upgrade() {
                b.cells.set_drop_hover(None);
            }
        });
        let (weak, v) = (Rc::downgrade(self), view.clone());
        target.connect_drop(move |t, value, x, y| {
            let Some(b) = weak.upgrade() else { return false };
            b.cells.set_drop_hover(None);
            let Ok(files) = value.get::<gdk::FileList>() else { return false };
            let sources = paths(&files);
            let dest = b.drop_dest(&v, x, y);
            let op = match action(&sources, &dest, ctrl(t)) {
                a if a == gdk::DragAction::MOVE => TransferOp::Move,
                a if a == gdk::DragAction::COPY => TransferOp::Copy,
                _ => return false,
            };
            log::debug!("drop {} item(s) on {}: {op:?}", sources.len(), dest.display());
            b.transfer(op, sources, dest);
            true
        });
        view.add_controller(target);
    }

    /// The folder under the pointer, or the shown one.
    fn drop_dest(&self, view: &gtk::Widget, x: f64, y: f64) -> PathBuf {
        self.cells
            .path_at(view, x, y)
            .filter(|p| self.entry(p).is_some_and(|e| e.kind == EntryKind::Dir))
            .unwrap_or_else(|| self.here())
    }
}

fn ctrl(t: &gtk::DropTarget) -> bool {
    t.current_event_state().contains(gdk::ModifierType::CONTROL_MASK)
}

/// Nothing when dropping a folder onto itself or items where they are.
fn action(sources: &[PathBuf], dest: &Path, ctrl: bool) -> gdk::DragAction {
    let into_itself = sources.iter().any(|s| dest.starts_with(s));
    let already_there = sources.iter().all(|s| s.parent() == Some(dest));
    if sources.is_empty() || into_itself || (already_there && !ctrl) {
        return gdk::DragAction::empty();
    }
    match drop_op(sources, dest, ctrl) {
        TransferOp::Move => gdk::DragAction::MOVE,
        TransferOp::Copy => gdk::DragAction::COPY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_actions() {
        let t = tempfile::tempdir().unwrap();
        let (a, sub) = (t.path().join("a"), t.path().join("sub"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&sub).unwrap();
        let none = gdk::DragAction::empty();
        assert_eq!(action(std::slice::from_ref(&a), &sub, false), gdk::DragAction::MOVE, "same filesystem");
        assert_eq!(action(std::slice::from_ref(&a), &sub, true), gdk::DragAction::COPY, "Ctrl copies");
        assert_eq!(action(std::slice::from_ref(&a), &a, false), none, "onto itself");
        assert_eq!(action(std::slice::from_ref(&a), &a.join("x"), false), none, "into itself");
        assert_eq!(action(std::slice::from_ref(&a), t.path(), false), none, "already there");
        assert_eq!(action(std::slice::from_ref(&a), t.path(), true), gdk::DragAction::COPY, "Ctrl duplicates");
    }
}
