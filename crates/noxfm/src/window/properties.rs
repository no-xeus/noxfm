//! Properties, in a small window of their own: files and folders (sizes,
//! dates, owner, permissions, default app, git), several items at once,
//! partitions and disks.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use noxfm_core::devices::{self, Disk};
use noxfm_core::fmt;
use noxfm_proto::{Device, EntryKind, Props, Request, Response};

use super::Browser;

/// Label column width.
const LABEL_W: i32 = 120;

/// A titled window holding `content`, scrolled, beside `parent`.
fn props_window(parent: &gtk::ApplicationWindow, title: &str, icon_names: &[&str]) -> (gtk::Window, gtk::Box) {
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_start(16).margin_end(16).margin_top(12).margin_bottom(16).build();
    let head = gtk::Box::builder().spacing(12).build();
    head.append(&gtk::Image::builder().gicon(&gio::ThemedIcon::from_names(icon_names)).pixel_size(48).build());
    let name = gtk::Label::builder().label(title).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).selectable(true).build();
    name.add_css_class("title-3");
    head.append(&name);
    body.append(&head);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title(format!("{title} — Properties"))
        .default_width(440)
        .default_height(540)
        .child(&gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).build())
        .build();
    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string("Escape"), Some(gtk::NamedAction::new("window.close"))));
    window.add_controller(keys);
    (window, body)
}

/// "Label   value" rows.
struct Rows(gtk::Grid, Cell<i32>);

impl Rows {
    fn new() -> Rows {
        Rows(gtk::Grid::builder().column_spacing(12).row_spacing(6).build(), Cell::new(0))
    }

    fn widget(&self, label: &str, value: &impl IsA<gtk::Widget>) {
        let l = gtk::Label::builder().label(label).xalign(0.0).yalign(0.0).width_request(LABEL_W).build();
        l.add_css_class("dim-label");
        let row = self.1.get();
        self.0.attach(&l, 0, row, 1, 1);
        self.0.attach(value, 1, row, 1, 1);
        self.1.set(row + 1);
    }

    fn text(&self, label: &str, value: &str) {
        let v = gtk::Label::builder().label(value).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).selectable(true).hexpand(true).build();
        self.widget(label, &v);
    }

    fn divider(&self) {
        let row = self.1.get();
        let sep = gtk::Separator::builder().margin_top(4).margin_bottom(4).build();
        self.0.attach(&sep, 0, row, 2, 1);
        self.1.set(row + 1);
    }
}

fn size(bytes: u64) -> String {
    format!("{} ({} bytes)", fmt::size(bytes), fmt::group_digits(bytes))
}

fn contains(p: &Props) -> String {
    let plural = |n: u64, one: &str, many: &str| format!("{} {}", fmt::group_digits(n), if n == 1 { one } else { many });
    format!("{}, {}", plural(p.files, "file", "files"), plural(p.folders, "folder", "folders"))
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl Browser {
    /// Properties of `paths` (one item, several, or the shown folder).
    pub(super) fn show_properties(self: &Rc<Self>, paths: Vec<PathBuf>) -> Option<gtk::Window> {
        let (title, icon): (String, &[&str]) = match paths.as_slice() {
            [] => return None,
            [one] => (name_of(one), if one.is_dir() { &["folder"] } else { &["text-x-generic"] }),
            many => (format!("{} items", many.len()), &["edit-select-all-symbolic", "folder"]),
        };
        let (window, body) = props_window(&self.window, &title, icon);
        // Folder totals take a walk: show that something is happening.
        let waiting = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(24).build();
        waiting.append(&gtk::Spinner::builder().spinning(true).build());
        waiting.append(&gtk::Label::new(Some("Adding up sizes…")));
        body.append(&waiting);
        window.present();

        let (weak, daemon) = (Rc::downgrade(self), self.daemon.clone());
        let shown = window.clone();
        glib::spawn_future_local(async move {
            let reply = daemon.request(Request::Properties { paths }).await;
            let Some(b) = weak.upgrade() else { return };
            body.remove(&waiting);
            match reply {
                Ok(Response::Properties(props)) => body.append(&b.props_body(&props)),
                Ok(other) => body.append(&gtk::Label::new(Some(&format!("unexpected reply: {other:?}")))),
                Err(e) => body.append(&gtk::Label::builder().label(&e).wrap(true).build()),
            }
        });
        Some(shown)
    }

    fn props_body(self: &Rc<Self>, props: &Props) -> gtk::Widget {
        let rows = Rows::new();
        match &props.entry {
            Some(e) => {
                let mut kind = if e.kind == EntryKind::Dir { "Folder".to_owned() } else { e.mime.clone().unwrap_or_else(|| "unknown".into()) };
                if let Some(ext) = props.ext_mime.as_ref().filter(|_| e.mime_mismatch) {
                    kind += &format!("\n⚠ content is {}, the extension says {ext}", e.mime.as_deref().unwrap_or("?"));
                }
                rows.text("Type", &kind);
                if let (Some(app), EntryKind::File) = (&e.app, e.kind) {
                    let line = gtk::Box::builder().spacing(8).build();
                    line.append(&gtk::Label::builder().label(&app.name).xalign(0.0).hexpand(true).build());
                    let change = gtk::Button::with_label("Change…");
                    let (weak, mime) = (Rc::downgrade(self), e.mime.clone().unwrap_or_else(|| "application/octet-stream".into()));
                    change.connect_clicked(move |_| {
                        if let Some(b) = weak.upgrade() {
                            b.app_picker(mime.clone(), None);
                        }
                    });
                    line.append(&change);
                    rows.widget("Opens with", &line);
                }
                rows.text("Location", &e.path.parent().map(fmt::short_path).unwrap_or_default());
                if e.symlink {
                    rows.text("Link to", &std::fs::read_link(&e.path).map(|t| t.display().to_string()).unwrap_or_default());
                }
                rows.divider();
                if e.kind == EntryKind::Dir {
                    rows.text("Size", &size(props.bytes));
                    rows.text("Contains", &contains(props));
                } else {
                    rows.text("Size", &size(e.size.unwrap_or(props.bytes)));
                }
                rows.divider();
                rows.text("Created", &fmt::time(e.created));
                rows.text("Modified", &fmt::time(e.modified));
                rows.text("Accessed", &fmt::time(props.accessed));
                rows.divider();
                let owner = format!(
                    "{} / {}",
                    e.owner.clone().unwrap_or_else(|| e.uid.to_string()),
                    e.group.clone().unwrap_or_else(|| e.gid.to_string())
                );
                rows.text("Owner / group", &owner);
                rows.widget("Permissions", &self.permissions(e.path.clone(), e.mode));
                if let Some(fs) = &props.fs {
                    rows.text("Filesystem", fs);
                }
                if let Some(git) = &props.git {
                    let branch = git.branch.clone().unwrap_or_else(|| "detached HEAD".into());
                    rows.text("Git", &format!("{branch} · {}", if git.dirty { "uncommitted changes" } else { "clean" }));
                }
            }
            None => {
                rows.text("Items", &format!("{} selected", props.paths.len()));
                let common = props.paths.first().and_then(|p| p.parent()).filter(|d| props.paths.iter().all(|p| p.parent() == Some(*d)));
                if let Some(dir) = common {
                    rows.text("Location", &fmt::short_path(dir));
                }
                rows.text("Size", &size(props.bytes));
                rows.text("Contains", &contains(props));
            }
        }
        if props.partial {
            rows.divider();
            rows.text("", "Some folders couldn't be read; totals are a minimum.");
        }
        rows.0.upcast()
    }

    /// Owner / group / others × read / write / run, each a check box that
    /// changes the mode at once.
    fn permissions(self: &Rc<Self>, path: PathBuf, mode: u32) -> gtk::Widget {
        let grid = gtk::Grid::builder().column_spacing(16).row_spacing(2).build();
        for (col, head) in ["Read", "Write", "Run"].iter().enumerate() {
            let l = gtk::Label::new(Some(head));
            l.add_css_class("caption");
            grid.attach(&l, col as i32 + 1, 0, 1, 1);
        }
        let mode = Rc::new(Cell::new(mode & 0o7777));
        let summary = gtk::Label::builder().xalign(0.0).build();
        summary.add_css_class("caption");
        let show = |summary: &gtk::Label, m: u32| summary.set_text(&format!("{} ({})", noxfm_core::perms::symbolic(m), noxfm_core::perms::octal(m)));
        show(&summary, mode.get());
        // Set while a refused change is put back, so it isn't sent again.
        let reverting = Rc::new(Cell::new(false));
        for (row, (who, shift)) in [("Owner", 6), ("Group", 3), ("Others", 0)].into_iter().enumerate() {
            grid.attach(&gtk::Label::builder().label(who).xalign(0.0).build(), 0, row as i32 + 1, 1, 1);
            for (col, bit) in [4u32, 2, 1].into_iter().enumerate() {
                let mask = bit << shift;
                let check = gtk::CheckButton::builder().active(mode.get() & mask != 0).halign(gtk::Align::Center).build();
                let (weak, path, mode, summary, reverting) = (Rc::downgrade(self), path.clone(), mode.clone(), summary.clone(), reverting.clone());
                check.connect_toggled(move |check| {
                    if reverting.get() {
                        return;
                    }
                    let Some(b) = weak.upgrade() else { return };
                    let new = mode.get() ^ mask;
                    let (check, mode, summary, reverting, daemon) = (check.clone(), mode.clone(), summary.clone(), reverting.clone(), b.daemon.clone());
                    let path = path.clone();
                    glib::spawn_future_local(async move {
                        match daemon.request(Request::Chmod { path, mode: new }).await {
                            Ok(_) => {
                                mode.set(new);
                                show(&summary, new);
                            }
                            Err(e) => {
                                reverting.set(true);
                                check.set_active(!check.is_active());
                                reverting.set(false);
                                summary.set_text(&e);
                            }
                        }
                    });
                });
                grid.attach(&check, col as i32 + 1, row as i32 + 1, 1, 1);
            }
        }
        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        col.append(&grid);
        col.append(&summary);
        col.upcast()
    }

    pub(super) fn show_partition_properties(self: &Rc<Self>, d: &Device) {
        let (window, body) = props_window(&self.window, &devices::partition_name(d), devices::device_icon(d));
        let rows = Rows::new();
        rows.text("Device", &d.device);
        rows.text("Filesystem", d.fs_type.as_deref().unwrap_or("unknown"));
        if let Some(l) = &d.label {
            rows.text("Label", l);
        }
        if let Some(u) = &d.uuid {
            rows.text("UUID", u);
        }
        rows.divider();
        rows.text("Size", &size(d.size));
        match (&d.mount_point, d.free) {
            (Some(mp), free) => {
                if let Some(free) = free {
                    let used = d.size.saturating_sub(free);
                    rows.text("Used", &fmt::size(used));
                    rows.text("Free", &fmt::size(free));
                    let bar = gtk::LevelBar::builder().value(if d.size == 0 { 0.0 } else { used as f64 / d.size as f64 }).build();
                    rows.widget("", &bar);
                }
                rows.text("Mounted at", &fmt::short_path(mp));
            }
            (None, _) => rows.text("Mounted", "No"),
        }
        rows.divider();
        rows.text("When plugged in", devices::policy_label(d.policy));
        if let Some(model) = &d.drive {
            let n = d.partition.map(|n| format!(", partition {n}")).unwrap_or_default();
            rows.text("Disk", &format!("{model}{n}"));
        }
        rows.text("Connection", if d.internal { "Internal" } else { "Plugged in" });
        body.append(&rows.0);
        window.present();
    }

    pub(super) fn show_disk_properties(self: &Rc<Self>, disk: &Disk<'_>) {
        let (window, body) = props_window(&self.window, &disk.name, disk.icon());
        let rows = Rows::new();
        rows.text("Size", &size(disk.size));
        rows.text("Connection", if disk.image { "Disk image" } else if disk.external { "Plugged in" } else { "Internal" });
        rows.divider();
        for (_, d) in &disk.parts {
            let state = match &d.mount_point {
                Some(m) => format!("mounted at {}", fmt::short_path(m)),
                None => "not mounted".into(),
            };
            let fs = d.fs_type.as_deref().unwrap_or("?");
            rows.text(&devices::partition_name(d), &format!("{fs} · {} · {state}", fmt::size(d.size)));
        }
        body.append(&rows.0);
        window.present();
    }
}
