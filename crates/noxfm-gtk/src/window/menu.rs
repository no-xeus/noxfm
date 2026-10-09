//! Right-click menus for items and the folder background, modelled on the
//! Windows 10 Explorer menu, and the window actions behind them.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::SortKey;
use noxfm_proto::{EntryKind, NewKind, TransferOp};

use super::Browser;

/// Sort keys as action targets.
pub(super) const SORT_KEYS: [(&str, &str, SortKey); 5] = [
    ("name", "Name", SortKey::Name),
    ("type", "Type", SortKey::Extension),
    ("size", "Size", SortKey::Size),
    ("modified", "Date modified", SortKey::Modified),
    ("created", "Date created", SortKey::Created),
];

/// Shortcuts that act on the items, attached to the views only, so they
/// never take keys from the path bar. Shown next to the menu items.
const VIEW_KEYS: [(&str, &str); 11] = [
    ("<Control>x", "win.cut"),
    ("<Control>c", "win.copy"),
    ("<Control><Shift>c", "win.copy-path"),
    ("<Control>v", "win.paste"),
    ("<Control>z", "win.undo"),
    ("F2", "win.rename"),
    ("Delete", "win.trash"),
    ("KP_Delete", "win.trash"),
    ("<Shift>Delete", "win.delete"),
    ("<Control><Shift>n", "win.new-folder"),
    ("Menu", "win.context-menu"),
];

fn accel_of(action: &str) -> Option<&'static str> {
    VIEW_KEYS.iter().find(|(_, a)| *a == action).map(|(k, _)| *k)
}

fn item(label: &str, action: &str) -> gio::MenuItem {
    let i = gio::MenuItem::new(Some(label), Some(action));
    if let Some(accel) = accel_of(action) {
        i.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    i
}

fn indexed(label: &str, action: &str, i: usize) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
    item
}

fn section(items: impl IntoIterator<Item = gio::MenuItem>) -> gio::Menu {
    let m = gio::Menu::new();
    for i in items {
        m.append_item(&i);
    }
    m
}

fn submenu(label: &str, menu: &gio::Menu) -> gio::MenuItem {
    gio::MenuItem::new_submenu(Some(label), menu)
}

impl Browser {
    pub(super) fn install_file_actions(self: &Rc<Self>) {
        type Run = fn(&Rc<Browser>);
        let plain: [(&str, Run); 21] = [
            ("open", Self::open_selection),
            ("cut", |b| b.to_clipboard(TransferOp::Move)),
            ("copy", |b| b.to_clipboard(TransferOp::Copy)),
            ("copy-path", |b| b.copy_paths()),
            ("paste", |b| b.paste(false)),
            ("paste-link", |b| b.paste(true)),
            ("compress", Self::compress),
            ("extract", Self::extract),
            ("create-link", Self::create_link),
            ("rename", |b| {
                if let [e] = b.selected_entries().as_slice() {
                    b.start_rename(e.path.clone());
                }
            }),
            ("trash", Self::trash),
            ("delete", Self::delete_forever),
            ("undo", Self::undo),
            ("new-folder", |b| b.create(NewKind::Folder)),
            ("new-file", |b| b.create(NewKind::EmptyFile)),
            ("new-text", |b| b.create(NewKind::TextDocument)),
            ("open-terminal", Self::open_terminal),
            ("open-new-window", Self::open_new_window),
            ("pin", |b| b.set_pinned(true)),
            ("unpin", |b| b.set_pinned(false)),
            ("context-menu", |b| b.keyboard_menu()),
        ];
        for (name, run) in plain {
            self.add_action(&gio::SimpleAction::new(name, None), move |b, _, _| run(b));
        }

        type RunIndexed = fn(&Rc<Browser>, usize);
        let indexed: [(&str, RunIndexed); 3] = [
            ("open-with", Self::open_with),
            ("send-to", Self::send_to),
            ("new-template", |b, i| {
                if let Some(t) = b.templates.get(i).cloned() {
                    b.create(NewKind::Template(t));
                }
            }),
        ];
        for (name, run) in indexed {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::INT32));
            self.add_action(&a, move |b, _, p| {
                if let Some(i) = p.and_then(|p| p.get::<i32>()) {
                    run(b, i.max(0) as usize);
                }
            });
        }

        let sort = gio::SimpleAction::new_stateful("sort", Some(glib::VariantTy::STRING), &"name".to_variant());
        self.add_action(&sort, |b, _, p| {
            let key = p.and_then(|p| p.str()).and_then(|k| SORT_KEYS.iter().find(|(id, ..)| *id == k)).map(|(.., k)| *k);
            if let Some(key) = key {
                b.list.sort(key, b.list.sorting().1);
            }
        });
        let descending = gio::SimpleAction::new_stateful("sort-descending", None, &false.to_variant());
        self.add_action(&descending, |b, _, _| {
            let (key, asc) = b.list.sorting();
            b.list.sort(key, !asc);
        });
        // Clicking a column header changes the sort too: keep the menu in step.
        let weak = Rc::downgrade(self);
        self.list.sorter.connect_changed(move |_, _| {
            let Some(b) = weak.upgrade() else { return };
            let (key, asc) = b.list.sorting();
            let id = SORT_KEYS.iter().find(|(.., k)| *k == key).map_or("name", |(id, ..)| *id);
            sort.set_state(&id.to_variant());
            descending.set_state(&(!asc).to_variant());
        });

        // Item shortcuts, on the views only.
        for view in [self.list.view.upcast_ref::<gtk::Widget>(), self.grid.upcast_ref()] {
            let keys = gtk::ShortcutController::new();
            for (trigger, action) in VIEW_KEYS {
                keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(gtk::NamedAction::new(action))));
            }
            view.add_controller(keys);
        }

        let weak = Rc::downgrade(self);
        self.window.clipboard().connect_changed(move |c| {
            if let Some(b) = weak.upgrade() {
                b.clipboard_changed(c);
            }
        });
        self.update_undo();
    }

    /// Undo is offered while noxd has something to undo.
    pub(super) fn update_undo(&self) {
        if let Some(a) = self.window.lookup_action("undo").and_downcast::<gio::SimpleAction>() {
            a.set_enabled(self.undo_label.borrow().is_some());
        }
    }

    /// Right press on `view` at `(x, y)`: an item outside the selection
    /// becomes the selection (as in Explorer); the background clears it.
    pub(super) fn connect_context_menu(self: &Rc<Self>, view: &gtk::Widget, popover: &gtk::PopoverMenu) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let (weak, view_, popover) = (Rc::downgrade(self), view.clone(), popover.clone());
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(b) = weak.upgrade() else { return };
            match b.cells.path_at(&view_, x, y) {
                Some(p) => {
                    let selected = b.selected_list().contains(&p);
                    if !selected && let Some(pos) = b.position_of(&p) {
                        b.selection.select_item(pos, true);
                    }
                }
                None => {
                    b.selection.unselect_all();
                }
            }
            let popover = popover.clone();
            b.with_apps(move |b| b.show_menu(&popover, x, y));
        });
        view.add_controller(click);
    }

    /// The Menu key: the menu at the first selected item (or the top).
    fn keyboard_menu(self: &Rc<Self>) {
        let (view, popover) = self.current_view_and_menu();
        let at = self.selected_list().first().and_then(|p| self.cells.anchor(p)).and_then(|a| a.compute_point(&view, &gtk::graphene::Point::new(16.0, 16.0)));
        let (x, y) = at.map_or((16.0, 16.0), |p| (f64::from(p.x()), f64::from(p.y())));
        self.with_apps(move |b| b.show_menu(&popover, x, y));
    }

    fn show_menu(&self, popover: &gtk::PopoverMenu, x: f64, y: f64) {
        let menu = if self.selected_list().is_empty() { self.background_menu() } else { self.item_menu() };
        popover.set_menu_model(Some(&menu));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    pub(super) fn item_menu(&self) -> gio::Menu {
        let sel = self.selected_entries();
        let first = &sel[0];
        let single = sel.len() == 1;
        let folder = single && first.kind == EntryKind::Dir;
        let file = single && first.kind == EntryKind::File;
        let menu = gio::Menu::new();

        let mut open = vec![item(
            &match (&first.app, file) {
                (Some(app), true) => format!("Open with {}", app.name),
                _ => "Open".into(),
            },
            "win.open",
        )];
        if folder {
            open.push(item("Open in new window", "win.open-new-window"));
            open.push(item("Open in terminal", "win.open-terminal"));
        }
        let open = section(open);
        if file {
            let apps = gio::Menu::new();
            for (i, a) in self.open_with.borrow().1.iter().enumerate() {
                apps.append_item(&indexed(&a.name, "win.open-with", i));
            }
            if apps.n_items() > 0 {
                open.append_submenu(Some("Open with"), &apps);
            }
            if first.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                open.append_item(&item("Extract here", "win.extract"));
            }
        }
        menu.append_section(None, &open);

        menu.append_section(
            None,
            &section([
                item("Cut", "win.cut"),
                item("Copy", "win.copy"),
                item(if single { "Copy path" } else { "Copy paths" }, "win.copy-path"),
            ]),
        );

        let send = gio::Menu::new();
        let targets = section(self.send_targets().iter().enumerate().map(|(i, (name, _))| indexed(name, "win.send-to", i)));
        send.append_section(None, &targets);
        send.append_section(None, &section([item("Compressed archive (.zip)", "win.compress")]));
        let mut share = vec![submenu("Send to", &send), item("Create link", "win.create-link")];
        if folder {
            share.push(if self.is_pinned(&first.path) {
                item("Unpin from sidebar", "win.unpin")
            } else {
                item("Pin to sidebar", "win.pin")
            });
        }
        menu.append_section(None, &section(share));

        let mut change = Vec::new();
        if single {
            change.push(item("Rename", "win.rename"));
        }
        change.push(item("Move to Trash", "win.trash"));
        change.push(item("Delete permanently…", "win.delete"));
        menu.append_section(None, &section(change));
        menu
    }

    pub(super) fn background_menu(&self) -> gio::Menu {
        let menu = gio::Menu::new();

        let sort = gio::Menu::new();
        let keys = gio::Menu::new();
        for (id, label, _) in SORT_KEYS {
            let i = gio::MenuItem::new(Some(label), None);
            i.set_action_and_target_value(Some("win.sort"), Some(&id.to_variant()));
            keys.append_item(&i);
        }
        sort.append_section(None, &keys);
        sort.append_section(None, &section([item("Descending", "win.sort-descending")]));
        let mut view = vec![submenu("View", &super::view_menu()), submenu("Sort by", &sort)];
        let refresh = item("Refresh", "win.reload");
        refresh.set_attribute_value("accel", Some(&"F5".to_variant()));
        view.push(refresh);
        menu.append_section(None, &section(view));

        let undo = match self.undo_label.borrow().as_deref() {
            Some(l) => item(&format!("Undo {l}"), "win.undo"),
            None => item("Undo", "win.undo"),
        };
        menu.append_section(None, &section([item("Paste", "win.paste"), item("Paste as link", "win.paste-link"), undo]));

        let new = gio::Menu::new();
        new.append_section(
            None,
            &section([item("Folder", "win.new-folder"), item("Empty file", "win.new-file"), item("Text document", "win.new-text")]),
        );
        let templates = section(self.templates.iter().enumerate().map(|(i, t)| {
            let name = t.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            indexed(&name, "win.new-template", i)
        }));
        new.append_section(None, &templates);
        let pin = if self.is_pinned(&self.here()) { item("Unpin from sidebar", "win.unpin") } else { item("Pin to sidebar", "win.pin") };
        menu.append_section(None, &section([submenu("New", &new), item("Open in terminal", "win.open-terminal"), pin]));
        menu
    }
}
