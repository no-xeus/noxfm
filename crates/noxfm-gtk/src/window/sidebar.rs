//! Left sidebar (Recent, Places, Pinned, disks) and the "mount this?"
//! banners.
//!
//! Every section and every disk folds by clicking its header. What's folded
//! is remembered by noxd (shared by all windows) under a stable key:
//! `section:<name>` or the disk's key. The sidebar is rebuilt whenever what
//! it shows changes; it's a few dozen rows.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::devices::{self, Disk};
use noxfm_core::fmt;
use noxfm_proto::{Device, MountPolicy, RecentKind, Request, Response};

use super::Browser;
use super::menu::section;
use super::pane::{Loc, Nav};

const ICON_PX: i32 = 16;

fn icon(names: &[&str], px: i32) -> gtk::Image {
    gtk::Image::builder().gicon(&gio::ThemedIcon::from_names(names)).pixel_size(px).build()
}

fn policy_id(p: MountPolicy) -> &'static str {
    match p {
        MountPolicy::Ask => "ask",
        MountPolicy::Auto => "auto",
        MountPolicy::Never => "never",
    }
}

impl Browser {
    pub(super) fn load_places(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.pane().request_then(Request::Places, move |_, r| {
            if let (Some(b), Response::Places(p)) = (weak.upgrade(), r) {
                *b.places.borrow_mut() = p;
                b.refresh_sidebar();
            }
        });
        let weak = Rc::downgrade(self);
        self.pane().request_then(Request::SidebarCollapsed, move |_, r| {
            if let (Some(b), Response::Keys(k)) = (weak.upgrade(), r) {
                *b.collapsed.borrow_mut() = k.into_iter().collect();
                b.refresh_sidebar();
            }
        });
    }

    pub(super) fn load_devices(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.pane().request_then(Request::ListDevices, move |_, r| {
            if let (Some(b), Response::Devices(d)) = (weak.upgrade(), r) {
                *b.devices.borrow_mut() = d;
                b.refresh_sidebar();
                b.refresh_banners();
            }
        });
    }

    pub(super) fn install_sidebar_actions(self: &Rc<Self>) {
        type Run = fn(&Rc<Browser>, usize);
        let indexed: [(&str, Run); 6] = [
            ("place-new-window", |b, i| {
                if let Some(p) = b.places.borrow().get(i) {
                    b.open_window(Loc::Dir(p.path.clone()));
                }
            }),
            ("place-new-tab", |b, i| {
                let path = b.places.borrow().get(i).map(|p| p.path.clone());
                if let Some(path) = path {
                    b.open_tab(Loc::Dir(path), true);
                }
            }),
            ("place-unpin", |b, i| {
                let path = b.places.borrow().get(i).map(|p| p.path.clone());
                if let Some(path) = path {
                    b.pane().fire(Request::Unpin { path });
                }
            }),
            ("device-open", |b, i| b.open_device(i)),
            ("device-unmount", |b, i| b.unmount(i)),
            ("device-copy-path", |b, i| {
                if let Some(d) = b.devices.borrow().get(i) {
                    b.window.clipboard().set_text(&d.device);
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
        // "(is)": device index and policy.
        let policy = gio::SimpleAction::new("device-policy", Some(glib::VariantTy::new("(is)").unwrap()));
        self.add_action(&policy, |b, _, p| {
            let Some((i, id)) = p.and_then(|p| p.get::<(i32, String)>()) else { return };
            let policy = [MountPolicy::Ask, MountPolicy::Auto, MountPolicy::Never].into_iter().find(|p| policy_id(*p) == id);
            let uuid = b.devices.borrow().get(i.max(0) as usize).and_then(|d| d.uuid.clone());
            if let (Some(policy), Some(uuid)) = (policy, uuid) {
                b.pane().fire(Request::SetMountPolicy { uuid, policy });
            }
        });
    }

    /// Opens a partition, mounting it first if needed.
    fn open_device(self: &Rc<Self>, i: usize) {
        let Some(d) = self.devices.borrow().get(i).cloned() else { return };
        let pane = self.pane();
        match d.mount_point {
            Some(mp) => pane.load(Loc::Dir(mp), Nav::New),
            None => {
                pane.notify("Mounting…");
                pane.request_then(Request::Mount { device: d.id }, |p, r| {
                    if let Response::Mounted(path) = r {
                        p.load(Loc::Dir(path), Nav::New);
                    }
                });
            }
        }
    }

    /// Tabs inside the volume step out of it first.
    fn unmount(self: &Rc<Self>, i: usize) {
        let Some(d) = self.devices.borrow().get(i).cloned() else { return };
        if let Some(mp) = &d.mount_point {
            for p in self.panes.borrow().iter() {
                if matches!(p.loc(), Loc::Dir(dir) if dir.starts_with(mp)) {
                    p.load(Loc::Dir(noxfm_core::complete::home_dir()), Nav::New);
                }
            }
        }
        self.pane().fire(Request::Unmount { device: d.id });
    }

    fn toggle_collapsed(self: &Rc<Self>, key: &str) {
        // Fold at once; noxd remembers it for the other windows.
        let collapsed = !self.collapsed.borrow().contains(key);
        if collapsed {
            self.collapsed.borrow_mut().insert(key.to_owned());
        } else {
            self.collapsed.borrow_mut().remove(key);
        }
        self.refresh_sidebar();
        self.pane().fire(Request::SetSidebarCollapsed { key: key.to_owned(), collapsed });
    }

    pub(super) fn refresh_sidebar(self: &Rc<Self>) {
        // Popovers leave their rows before the rows go.
        for p in self.sidebar_menus.borrow_mut().drain(..) {
            p.unparent();
        }
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let current = (!self.panes.borrow().is_empty()).then(|| self.pane().loc());
        let folded = |key: &str| self.collapsed.borrow().contains(key);

        if self.section("Recent", "section:recent") {
            for (label, kind, name) in [
                ("All recent", None, "document-open-recent"),
                ("Downloaded", Some(RecentKind::Downloaded), "folder-download"),
                ("Edited", Some(RecentKind::Modified), "document-edit"),
                ("Created", Some(RecentKind::Created), "document-new"),
            ] {
                let loc = Loc::Recent(kind);
                let row = self.row(&[name, "folder"], label, None, current.as_ref() == Some(&loc), move |b| b.pane().load(loc.clone(), Nav::New));
                self.sidebar.append(&row);
            }
        }

        let places: Vec<(usize, noxfm_proto::Place)> = self.places.borrow().iter().cloned().enumerate().collect();
        let place_row = |i: usize, p: &noxfm_proto::Place| {
            let loc = Loc::Dir(p.path.clone());
            let names = if p.pinned { ["folder"; 2] } else { [devices::place_icon(&p.name), "folder"] };
            let row = self.row(&names, &p.name, None, current.as_ref() == Some(&loc), move |b| b.pane().load(loc.clone(), Nav::New));
            let mut items = vec![idx_item("Open in new tab", "win.place-new-tab", i), idx_item("Open in new window", "win.place-new-window", i)];
            if p.pinned {
                items.push(idx_item("Unpin from sidebar", "win.place-unpin", i));
            }
            self.attach_menu(&row, &section(items));
            row
        };
        if self.section("Places", "section:places") {
            for (i, p) in places.iter().filter(|(_, p)| !p.pinned) {
                self.sidebar.append(&place_row(*i, p));
            }
            let n = self.trash_count.get();
            let (names, label) = if n > 0 { (["user-trash-full", "user-trash"], format!("Trash ({n})")) } else { (["user-trash"; 2], "Trash".into()) };
            let trash = self.row(&names, &label, None, current.as_ref() == Some(&Loc::Trash), |b| b.pane().load(Loc::Trash, Nav::New));
            self.sidebar.append(&trash);
        }
        if places.iter().any(|(_, p)| p.pinned) && self.section("Pinned", "section:pinned") {
            for (i, p) in places.iter().filter(|(_, p)| p.pinned) {
                self.sidebar.append(&place_row(*i, p));
            }
        }

        let all = self.devices.borrow().clone();
        for (external, title, key) in [(true, "Plugged in", "section:plugged"), (false, "Internal disks", "section:internal")] {
            let disks: Vec<Disk<'_>> = devices::group_disks(&all).into_iter().filter(|d| d.external == external).collect();
            if disks.is_empty() || !self.section(title, key) {
                continue;
            }
            for disk in disks {
                let disk_folded = folded(&disk.key);
                self.sidebar.append(&self.disk_header(&disk, disk_folded));
                if !disk_folded {
                    for (i, d) in &disk.parts {
                        self.sidebar.append(&self.partition_row(*i, d, current.as_ref()));
                    }
                }
            }
        }
    }

    /// Appends a section header; true when the section is unfolded.
    fn section(self: &Rc<Self>, title: &str, key: &'static str) -> bool {
        let open = !self.collapsed.borrow().contains(key);
        let head = gtk::Box::builder().spacing(4).margin_top(8).build();
        head.append(&icon(&[if open { "pan-down-symbolic" } else { "pan-end-symbolic" }, "go-next-symbolic"], 12));
        let label = gtk::Label::builder().label(title).xalign(0.0).build();
        label.add_css_class("caption-heading");
        label.add_css_class("dim-label");
        head.append(&label);
        let button = gtk::Button::builder().child(&head).build();
        button.add_css_class("flat");
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.toggle_collapsed(key);
            }
        });
        self.sidebar.append(&button);
        open
    }

    fn row(self: &Rc<Self>, names: &[&str], label: &str, caption: Option<&str>, active: bool, go: impl Fn(&Rc<Browser>) + 'static) -> gtk::Button {
        let content = gtk::Box::builder().spacing(8).build();
        content.append(&icon(names, ICON_PX));
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.append(&gtk::Label::builder().label(label).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build());
        if let Some(c) = caption {
            let c = gtk::Label::builder().label(c).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            c.add_css_class("caption");
            c.add_css_class("dim-label");
            text.append(&c);
        }
        content.append(&text);
        let button = gtk::Button::builder().child(&content).hexpand(true).build();
        button.add_css_class("flat");
        if active {
            button.add_css_class("sidebar-active");
        }
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                go(&b);
            }
        });
        button
    }

    /// The disk's name; clicking it folds or unfolds its partitions.
    fn disk_header(self: &Rc<Self>, disk: &Disk<'_>, folded: bool) -> gtk::Button {
        let head = gtk::Box::builder().spacing(6).build();
        head.append(&icon(&[if folded { "pan-end-symbolic" } else { "pan-down-symbolic" }, "go-next-symbolic"], 12));
        head.append(&icon(disk.icon(), 20));
        let name = gtk::Label::builder().label(&disk.name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        name.add_css_class("heading");
        head.append(&name);
        let button = gtk::Button::builder().child(&head).tooltip_text(fmt::size(disk.size)).build();
        button.add_css_class("flat");
        let (weak, key) = (Rc::downgrade(self), disk.key.clone());
        button.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.toggle_collapsed(&key);
            }
        });
        button
    }

    /// One partition: click opens it, mounting first if needed; right-click
    /// shows actions and the mount rule.
    fn partition_row(self: &Rc<Self>, i: usize, d: &Device, current: Option<&Loc>) -> gtk::Box {
        let mounted = d.mount_point.as_ref();
        let active = matches!((current, mounted), (Some(Loc::Dir(p)), Some(m)) if p.starts_with(m));
        let caption = format!(
            "{} · {}",
            d.fs_type.as_deref().unwrap_or("volume"),
            if mounted.is_some() { "mounted" } else { devices::policy_short(d.policy) }
        );
        let main = self.row(devices::device_icon(d), &devices::partition_name(d), Some(&caption), active, move |b| b.open_device(i));
        main.set_tooltip_text(Some(&format!("{} · right-click for options", d.device)));
        let row = gtk::Box::builder().margin_start(20).build();
        row.append(&main);
        if mounted.is_some() {
            let eject = gtk::Button::builder().icon_name("media-eject-symbolic").tooltip_text("Unmount").valign(gtk::Align::Center).build();
            eject.add_css_class("flat");
            eject.set_action_name(Some("win.device-unmount"));
            eject.set_action_target_value(Some(&(i as i32).to_variant()));
            row.append(&eject);
        }

        let menu = gio::Menu::new();
        let open = if mounted.is_some() { "Open" } else { "Mount and open" };
        let mut first = vec![idx_item(open, "win.device-open", i)];
        if mounted.is_some() {
            first.push(idx_item("Unmount", "win.device-unmount", i));
        }
        menu.append_section(None, &section(first));
        // The rule is stored per filesystem UUID; without one there's nothing to remember.
        if d.uuid.is_some() {
            let rules = section([MountPolicy::Ask, MountPolicy::Auto, MountPolicy::Never].map(|p| {
                let label = if p == d.policy { format!("✓ {}", devices::policy_label(p)) } else { devices::policy_label(p).to_owned() };
                let item = gio::MenuItem::new(Some(&label), None);
                item.set_action_and_target_value(Some("win.device-policy"), Some(&(i as i32, policy_id(p).to_owned()).to_variant()));
                item
            }));
            menu.append_section(None, &rules);
        }
        menu.append_section(None, &section([idx_item("Copy device path", "win.device-copy-path", i)]));
        self.attach_menu(&row, &menu);
        row
    }

    /// One banner per device waiting for "mount it?".
    pub(super) fn refresh_banners(self: &Rc<Self>) {
        while let Some(child) = self.banners.first_child() {
            self.banners.remove(&child);
        }
        let asking: Vec<(usize, Device)> = self.devices.borrow().iter().cloned().enumerate().filter(|(_, d)| d.asking).collect();
        for (i, d) in asking {
            let row = gtk::Box::builder().spacing(8).build();
            row.add_css_class("ask-banner");
            row.append(&icon(devices::device_icon(&d), 24));
            let text = format!("“{}” ({}) was plugged in. Mount it?", devices::partition_name(&d), fmt::size(d.size));
            row.append(&gtk::Label::builder().label(&text).xalign(0.0).hexpand(true).wrap(true).build());
            type Run = Box<dyn Fn(&Rc<Browser>)>;
            let button = |label: &str, run: Run| {
                let b = gtk::Button::with_label(label);
                let weak = Rc::downgrade(self);
                b.connect_clicked(move |_| {
                    if let Some(browser) = weak.upgrade() {
                        run(&browser);
                    }
                });
                b
            };
            let mount = button("Mount", Box::new(move |b| b.open_device(i)));
            mount.add_css_class("suggested-action");
            row.append(&mount);
            if let Some(uuid) = d.uuid.clone() {
                for (label, policy) in [("Always", MountPolicy::Auto), ("Never", MountPolicy::Never)] {
                    let uuid = uuid.clone();
                    row.append(&button(label, Box::new(move |b| b.pane().fire(Request::SetMountPolicy { uuid: uuid.clone(), policy }))));
                }
            }
            let id = d.id.clone();
            let later = button("Not now", Box::new(move |b| b.pane().fire(Request::DismissAsk { device: id.clone() })));
            later.add_css_class("flat");
            row.append(&later);
            self.banners.append(&row);
        }
    }
}

fn idx_item(label: &str, action: &str, i: usize) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
    item
}

impl Browser {
    /// Right-click on `widget` shows `menu`.
    fn attach_menu(&self, widget: &impl IsA<gtk::Widget>, menu: &gio::Menu) {
        let popover = gtk::PopoverMenu::from_model(Some(menu));
        popover.set_has_arrow(false);
        popover.set_parent(widget);
        self.sidebar_menus.borrow_mut().push(popover.clone());
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        });
        widget.add_controller(click);
    }
}
