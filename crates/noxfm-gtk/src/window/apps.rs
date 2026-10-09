//! Choosing an application: "Open with ▸ Other application…" (opens the
//! file, and can make it the default) and Properties' "Change…" (sets the
//! default only).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use noxfm_proto::{AppRef, Request, Response};

use super::Browser;

impl Browser {
    pub(super) fn app_picker(self: &Rc<Self>, mime: String, path: Option<PathBuf>) -> gtk::Window {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_start(12).margin_end(12).margin_top(12).margin_bottom(12).build();
        let search = gtk::SearchEntry::builder().placeholder_text("Search applications").build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        content.append(&search);
        content.append(&gtk::ScrolledWindow::builder().child(&list).vexpand(true).build());
        let remember = gtk::CheckButton::with_label(&format!("Always use for {mime}"));
        if path.is_some() {
            content.append(&remember);
        }
        let title = if path.is_some() { "Open with" } else { "Default application" };
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(title)
            .default_width(420)
            .default_height(520)
            .child(&content)
            .build();
        let keys = gtk::ShortcutController::new();
        keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string("Escape"), Some(gtk::NamedAction::new("window.close"))));
        window.add_controller(keys);

        let apps: Rc<RefCell<Vec<AppRef>>> = Rc::default();
        let (shown, filter) = (apps.clone(), search.clone());
        list.set_filter_func(move |row| {
            let needle = filter.text().to_lowercase();
            needle.is_empty() || shown.borrow().get(row.index() as usize).is_some_and(|a| a.name.to_lowercase().contains(&needle))
        });
        let refilter = list.clone();
        search.connect_search_changed(move |_| refilter.invalidate_filter());

        let (weak, chosen, win) = (Rc::downgrade(self), apps.clone(), window.clone());
        list.connect_row_activated(move |_, row| {
            let (Some(b), Some(app)) = (weak.upgrade(), chosen.borrow().get(row.index() as usize).cloned()) else { return };
            win.close();
            b.choose_app(&mime, path.clone(), app.id, path.is_none() || remember.is_active());
        });

        let (daemon, list_) = (self.daemon.clone(), list.clone());
        glib::spawn_future_local(async move {
            let Ok(Response::Apps(found)) = daemon.request(Request::AllApps).await else { return };
            for a in &found {
                let row = gtk::Box::builder().spacing(10).margin_top(4).margin_bottom(4).margin_start(4).build();
                let icon: gio::Icon = match &a.icon {
                    Some(i) if i.starts_with('/') => gio::FileIcon::new(&gio::File::for_path(i)).upcast(),
                    Some(i) => gio::ThemedIcon::from_names(&[i.as_str(), "application-x-executable"]).upcast(),
                    None => gio::ThemedIcon::new("application-x-executable").upcast(),
                };
                row.append(&gtk::Image::builder().gicon(&icon).pixel_size(24).build());
                row.append(&gtk::Label::builder().label(&a.name).xalign(0.0).build());
                list_.append(&row);
            }
            *apps.borrow_mut() = found;
        });
        window.present();
        window
    }

    /// Opens `path` with `app`, and/or makes `app` the default for `mime`.
    fn choose_app(self: &Rc<Self>, mime: &str, path: Option<PathBuf>, app: String, make_default: bool) {
        let pane = self.pane();
        if make_default {
            let weak = Rc::downgrade(self);
            pane.request_then(Request::SetDefaultApp { mime: mime.to_owned(), app: app.clone() }, move |_, _| {
                // Entries carry their default app: relist to show the new one.
                if let Some(b) = weak.upgrade() {
                    for p in b.panes.borrow().iter() {
                        p.open_with.borrow_mut().0.clear();
                        p.reload();
                    }
                }
            });
        }
        if let Some(path) = path {
            pane.fire(Request::OpenWith { path, app: Some(app) });
        }
    }
}
