//! Right-click menus for items and the background, modelled on the Windows
//! 10 Explorer menu, and the window actions behind them. Actions act on the
//! active tab.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::SortKey;
use noxfm_proto::{EntryKind, NewKind, TransferOp};

use super::Browser;
use super::pane::{Loc, Pane};

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

pub(super) fn section(items: impl IntoIterator<Item = gio::MenuItem>) -> gio::Menu {
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
        type Run = fn(&Rc<Pane>);
        let on_pane: [(&str, Run); 26] = [
            ("open", Pane::open_selection),
            ("cut", |p| p.to_clipboard(TransferOp::Move)),
            ("copy", |p| p.to_clipboard(TransferOp::Copy)),
            ("copy-path", Pane::copy_paths),
            ("paste", |p| p.paste(false)),
            ("paste-link", |p| p.paste(true)),
            ("compress", Pane::compress),
            ("extract", Pane::extract),
            ("create-link", Pane::create_link),
            ("rename", |p| {
                if let [e] = p.selected_entries().as_slice() {
                    p.start_rename(e.path.clone());
                }
            }),
            ("trash", Pane::trash),
            ("delete", Pane::delete_forever),
            ("undo", Pane::undo),
            ("new-folder", |p| p.create(NewKind::Folder)),
            ("new-file", |p| p.create(NewKind::EmptyFile)),
            ("new-text", |p| p.create(NewKind::TextDocument)),
            ("open-terminal", Pane::open_terminal),
            ("open-new-window", |p| p.browser().open_window(p.target_loc())),
            ("open-new-tab", |p| {
                p.browser().open_tab(p.target_loc(), false);
            }),
            ("pin", |p| p.set_pinned(true)),
            ("unpin", |p| p.set_pinned(false)),
            ("restore", Pane::restore),
            ("empty-trash", Pane::empty_trash),
            ("forget-recent", Pane::forget_recent),
            ("open-location", Pane::open_location),
            ("context-menu", |p| p.browser().keyboard_menu(p)),
        ];
        for (name, run) in on_pane {
            self.add_action(&gio::SimpleAction::new(name, None), move |b, _, _| run(&b.pane()));
        }

        type RunIndexed = fn(&Rc<Pane>, usize);
        let indexed: [(&str, RunIndexed); 3] = [
            ("open-with", Pane::open_with),
            ("send-to", Pane::send_to),
            ("new-template", |p, i| {
                if let Some(t) = p.browser().templates.get(i).cloned() {
                    p.create(NewKind::Template(t));
                }
            }),
        ];
        for (name, run) in indexed {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::INT32));
            self.add_action(&a, move |b, _, param| {
                if let Some(i) = param.and_then(|p| p.get::<i32>()) {
                    run(&b.pane(), i.max(0) as usize);
                }
            });
        }

        let sort = gio::SimpleAction::new_stateful("sort", Some(glib::VariantTy::STRING), &"name".to_variant());
        self.add_action(&sort, |b, _, param| {
            let key = param.and_then(|p| p.str()).and_then(|k| SORT_KEYS.iter().find(|(id, ..)| *id == k)).map(|(.., k)| *k);
            if let Some(key) = key {
                let p = b.pane();
                p.list.sort(key, p.list.sorting().1);
            }
        });
        self.add_action(&gio::SimpleAction::new_stateful("sort-descending", None, &false.to_variant()), |b, _, _| {
            let p = b.pane();
            let (key, asc) = p.list.sorting();
            p.list.sort(key, !asc);
        });

        let weak = Rc::downgrade(self);
        self.window.clipboard().connect_changed(move |c| {
            // Another app (or window) owns the clipboard now: nothing is cut here.
            if let Some(b) = weak.upgrade()
                && !c.is_local()
            {
                b.set_cut(Default::default());
            }
        });
        self.update_undo();
    }

    /// The sort menu shows the active tab's sort.
    pub(super) fn sync_sort_actions(&self, p: &Pane) {
        let (key, asc) = p.list.sorting();
        let id = SORT_KEYS.iter().find(|(.., k)| *k == key).map_or("name", |(id, ..)| *id);
        for (name, state) in [("sort", id.to_variant()), ("sort-descending", (!asc).to_variant())] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_state(&state);
            }
        }
    }

    /// Undo is offered while noxd has something to undo.
    pub(super) fn update_undo(&self) {
        if let Some(a) = self.window.lookup_action("undo").and_downcast::<gio::SimpleAction>() {
            a.set_enabled(self.undo_label.borrow().is_some());
        }
    }

    /// Right-click menus and item shortcuts on a tab's views.
    pub(super) fn connect_menus(self: &Rc<Self>, pane: &Rc<Pane>) {
        let views: [(gtk::Widget, gtk::PopoverMenu); 2] =
            [(pane.list.view.clone().upcast(), pane.list_menu.clone()), (pane.grid.clone().upcast(), pane.grid_menu.clone())];
        for (view, popover) in views {
            let keys = gtk::ShortcutController::new();
            for (trigger, action) in VIEW_KEYS {
                keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(gtk::NamedAction::new(action))));
            }
            view.add_controller(keys);

            // Right press: an item outside the selection becomes the
            // selection (as in Explorer); the background clears it.
            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            let (weak, v) = (Rc::downgrade(pane), view.clone());
            click.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                let Some(p) = weak.upgrade() else { return };
                match p.cells.path_at(&v, x, y) {
                    Some(path) => {
                        if !p.selected_list().contains(&path)
                            && let Some(pos) = p.position_of(&path)
                        {
                            p.selection.select_item(pos, true);
                        }
                    }
                    None => {
                        p.selection.unselect_all();
                    }
                }
                let popover = popover.clone();
                p.with_apps(move |p| p.browser().show_menu(p, &popover, x, y));
            });
            view.add_controller(click);
        }
    }

    /// The Menu key: the menu at the first selected item (or the top).
    fn keyboard_menu(&self, p: &Rc<Pane>) {
        let (view, popover) = (p.current_view(), p.current_menu());
        let at = p
            .selected_list()
            .first()
            .and_then(|path| p.cells.anchor(path))
            .and_then(|a| a.compute_point(&view, &gtk::graphene::Point::new(16.0, 16.0)));
        let (x, y) = at.map_or((16.0, 16.0), |pt| (f64::from(pt.x()), f64::from(pt.y())));
        p.with_apps(move |p| p.browser().show_menu(p, &popover, x, y));
    }

    fn show_menu(&self, p: &Pane, popover: &gtk::PopoverMenu, x: f64, y: f64) {
        let menu = if p.selected_list().is_empty() { self.background_menu(p) } else { self.item_menu(p) };
        popover.set_menu_model(Some(&menu));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    pub(super) fn item_menu(&self, p: &Pane) -> gio::Menu {
        let sel = p.selected_entries();
        let first = &sel[0];
        let menu = gio::Menu::new();
        if p.loc() == Loc::Trash {
            menu.append_section(None, &section([item("Restore", "win.restore")]));
            menu.append_section(None, &section([item("Delete permanently…", "win.delete")]));
            return menu;
        }
        let in_recent = matches!(p.loc(), Loc::Recent(_));
        let single = sel.len() == 1;
        let folder = single && first.kind == EntryKind::Dir;
        let file = single && first.kind == EntryKind::File;

        let mut open = vec![item(
            &match (&first.app, file) {
                (Some(app), true) => format!("Open with {}", app.name),
                _ => "Open".into(),
            },
            "win.open",
        )];
        if folder {
            open.push(item("Open in new tab", "win.open-new-tab"));
            open.push(item("Open in new window", "win.open-new-window"));
            open.push(item("Open in terminal", "win.open-terminal"));
        }
        let open = section(open);
        if file {
            let apps = gio::Menu::new();
            for (i, a) in p.open_with.borrow().1.iter().enumerate() {
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
        if in_recent {
            let mut recent = vec![item("Remove from Recent", "win.forget-recent")];
            if single {
                recent.insert(0, item("Open file location", "win.open-location"));
            }
            menu.append_section(None, &section(recent));
        }

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
        if !in_recent {
            send.append_section(None, &section([item("Compressed archive (.zip)", "win.compress")]));
        }
        let mut share = vec![submenu("Send to", &send)];
        if !in_recent {
            share.push(item("Create link", "win.create-link"));
        }
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

    pub(super) fn background_menu(&self, p: &Pane) -> gio::Menu {
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
        let refresh = item("Refresh", "win.reload");
        refresh.set_attribute_value("accel", Some(&"F5".to_variant()));
        menu.append_section(None, &section([submenu("View", &super::view_menu()), submenu("Sort by", &sort), refresh]));

        match p.loc() {
            Loc::Recent(_) => return menu,
            Loc::Trash => {
                menu.append_section(None, &section([item("Empty Trash…", "win.empty-trash")]));
                return menu;
            }
            Loc::Dir(_) => {}
        }

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
        let pin = if self.is_pinned(&p.here()) { item("Unpin from sidebar", "win.unpin") } else { item("Pin to sidebar", "win.pin") };
        menu.append_section(None, &section([submenu("New", &new), item("Open in terminal", "win.open-terminal"), pin]));
        menu
    }
}
