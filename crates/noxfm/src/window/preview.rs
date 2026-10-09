//! The preview panel (Space): the single selected item of the active tab.
//! Images are shown whole; text and other files as their first lines or
//! bytes (`noxfm_core::preview`).

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::fmt;
use noxfm_core::preview::{self, Content};
use noxfm_proto::{Entry, EntryKind};

use super::Browser;

/// Images bigger than this aren't loaded for preview.
const IMAGE_MAX: u64 = 64 << 20;

fn mono(text: &str) -> gtk::Widget {
    let view = gtk::TextView::builder().editable(false).cursor_visible(false).monospace(true).wrap_mode(gtk::WrapMode::None).build();
    view.buffer().set_text(text);
    gtk::ScrolledWindow::builder().child(&view).vexpand(true).build().upcast()
}

fn note(text: &str) -> gtk::Widget {
    let l = gtk::Label::builder().label(text).wrap(true).vexpand(true).valign(gtk::Align::Start).xalign(0.0).build();
    l.add_css_class("dim-label");
    l.upcast()
}

impl Browser {
    pub(super) fn toggle_preview(self: &Rc<Self>, on: bool) {
        self.preview.set_visible(on);
        self.preview_path.borrow_mut().take();
        self.refresh_preview();
    }

    /// Follows the selection of the active tab (called on every change).
    pub(super) fn refresh_preview(self: &Rc<Self>) {
        if !self.preview.is_visible() || self.panes.borrow().is_empty() {
            return;
        }
        let target = match self.pane().selected_entries().as_slice() {
            [e] => Some(e.clone()),
            _ => None,
        };
        if target.as_ref().map(|e| &e.path) == self.preview_path.borrow().as_ref() {
            return;
        }
        *self.preview_path.borrow_mut() = target.as_ref().map(|e| e.path.clone());
        while let Some(c) = self.preview.first_child() {
            self.preview.remove(&c);
        }
        let Some(e) = target else {
            self.preview.append(&note("Select one item to preview it."));
            return;
        };

        let name = gtk::Label::builder().label(&e.name).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).build();
        name.add_css_class("heading");
        self.preview.append(&name);
        let mut facts = vec![if e.kind == EntryKind::Dir { "Folder".into() } else { e.mime.clone().unwrap_or_default() }];
        facts.extend(e.size.map(fmt::size));
        facts.push(format!("modified {}", fmt::time(e.modified)));
        if let Some(app) = &e.app {
            facts.push(format!("opens with {}", app.name));
        }
        let facts = gtk::Label::builder().label(facts.join(" · ")).xalign(0.0).wrap(true).build();
        facts.add_css_class("caption");
        facts.add_css_class("dim-label");
        self.preview.append(&facts);
        self.preview.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let loading = note("Loading…");
        self.preview.append(&loading);
        self.load_preview(e, loading);
    }

    fn load_preview(self: &Rc<Self>, e: Entry, placeholder: gtk::Widget) {
        let weak = Rc::downgrade(self);
        let is_image = e.kind == EntryKind::File && e.mime.as_deref().is_some_and(|m| m.starts_with("image/")) && e.size.unwrap_or(0) <= IMAGE_MAX;
        glib::spawn_future_local(async move {
            let path = e.path.clone();
            let body: gtk::Widget = if is_image {
                let p = path.clone();
                let bytes = gio::spawn_blocking(move || std::fs::read(p).ok()).await.ok().flatten();
                match bytes.and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok()) {
                    Some(t) => gtk::Picture::builder().paintable(&t).content_fit(gtk::ContentFit::Contain).vexpand(true).build().upcast(),
                    None => note("This image can't be shown."),
                }
            } else {
                let p = path.clone();
                let content = gio::spawn_blocking(move || preview::load(&p)).await.unwrap_or_else(|_| Content::Error("preview failed".into()));
                match content {
                    Content::Text { text, truncated } => {
                        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
                        col.append(&mono(&text));
                        if truncated {
                            col.append(&note("… (truncated)"));
                        }
                        col.upcast()
                    }
                    Content::Hex(dump) => mono(&dump),
                    Content::Folder { items } => note(&format!("Folder with {items} item{}", if items == 1 { "" } else { "s" })),
                    Content::Empty => note("Empty file"),
                    Content::Error(err) => note(&err),
                }
            };
            // Still the item shown?
            let Some(b) = weak.upgrade() else { return };
            if b.preview_path.borrow().as_ref() == Some(&path) && placeholder.parent().is_some() {
                b.preview.remove(&placeholder);
                b.preview.append(&body);
            }
        });
    }
}
