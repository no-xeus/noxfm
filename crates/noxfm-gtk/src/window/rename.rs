//! Inline rename: a small popover with the name, on the item itself.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use noxfm_proto::{Request, Response};

use super::pane::Pane;

impl Pane {
    /// F2 / "Rename": Enter or clicking away commits, Escape cancels.
    pub(super) fn start_rename(self: &Rc<Self>, path: PathBuf) {
        let Some(e) = self.entry(&path) else { return };
        // The item may have just been scrolled to: wait for its widgets.
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(this) = weak.upgrade() else { return };
            let Some(anchor) = this.cells.anchor(&path) else { return };
            this.rename_popover(&anchor, path, &e.name, e.kind == noxfm_proto::EntryKind::Dir);
        });
    }

    fn rename_popover(self: &Rc<Self>, anchor: &gtk::Widget, path: PathBuf, name: &str, is_dir: bool) {
        let entry = gtk::Entry::builder().text(name).width_chars(name.chars().count().clamp(16, 48) as i32).build();
        let popover = gtk::Popover::builder().child(&entry).position(gtk::PositionType::Bottom).build();
        popover.set_parent(anchor);

        let committed = Rc::new(std::cell::Cell::new(false));
        let commit = {
            let (weak, committed, popover) = (Rc::downgrade(self), committed.clone(), popover.clone());
            let old = name.to_owned();
            move |entry: &gtk::Entry| {
                if committed.replace(true) {
                    return;
                }
                let new = entry.text().to_string();
                popover.popdown();
                let Some(this) = weak.upgrade() else { return };
                if new.is_empty() || new == old {
                    return;
                }
                let path = path.clone();
                this.request_then(Request::Rename { path, new_name: new }, |this, r| {
                    if let Response::Path(p) = r {
                        // Selected once the listing shows it.
                        *this.pending_select.borrow_mut() = Some(p);
                        this.apply_pending_select();
                    }
                });
            }
        };
        let on_enter = commit.clone();
        entry.connect_activate(move |e| on_enter(e));

        // Escape closes without committing; clicking away commits.
        let keys = gtk::EventControllerKey::new();
        let cancelled = committed.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                cancelled.set(true);
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(keys);
        let away = entry.clone();
        popover.connect_closed(move |p| {
            commit(&away);
            // Unparent once GTK is done with the closing popover.
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });

        popover.popup();
        entry.grab_focus();
        // The name without its extension, as other file managers do.
        let stem = if is_dir { name.len() } else { name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len()) };
        entry.select_region(0, name[..stem].chars().count() as i32);
    }
}
