//! Drives a real window against a headless noxd, on an offscreen GTK
//! display (Broadway). Input can't be injected there, so the test calls the
//! actions and methods that keys, menus and clicks call.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};
use noxfm_proto::{NewKind, Request, TransferOp};

use super::pane::{Loc, Nav, Pane};
use super::{Browser, ViewMode};
use crate::cells::{GRID_ZOOM, entry_of};
use crate::daemon::Daemon;

/// Runs the main loop until `cond` holds.
fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let ctx = glib::MainContext::default();
    let end = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn names(p: &Pane) -> Vec<String> {
    (0..p.selection.n_items()).filter_map(|i| p.selection.item(i)).map(|o| entry_of(&o).name.clone()).collect()
}

/// `w` and everything inside it.
fn descendants(w: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = vec![w.clone()];
    let mut child = w.first_child();
    while let Some(c) = child {
        out.extend(descendants(&c));
        child = c.next_sibling();
    }
    out
}

fn find<W: IsA<gtk::Widget>>(w: &impl IsA<gtk::Widget>) -> Vec<W> {
    descendants(w.upcast_ref()).into_iter().filter_map(|d| d.downcast::<W>().ok()).collect()
}

fn action(b: &Browser, name: &str, param: Option<&glib::Variant>) {
    WidgetExt::activate_action(&b.window, name, param).unwrap();
}

/// Stops the display server however the test ends.
struct Display(std::process::Child);

impl Drop for Display {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// An offscreen display, or `None` when Broadway isn't installed.
fn broadway(runtime: &Path) -> Option<Display> {
    let display = format!(":{}", 40 + std::process::id() % 50);
    let child = std::process::Command::new("gtk4-broadwayd")
        .arg(&display)
        .env("XDG_RUNTIME_DIR", runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // SAFETY: set before GTK and the daemon start; no other test reads these.
    unsafe {
        std::env::set_var("GDK_BACKEND", "broadway");
        std::env::set_var("BROADWAY_DISPLAY", &display);
    }
    let socket = runtime.join(format!("broadway{}.socket", &display[1..].parse::<u32>().unwrap() + 1));
    let end = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(Display(child))
}

#[test]
fn file_actions_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // SAFETY: as above. Trash, caches and state stay in the temp dir.
    unsafe {
        for (k, d) in [("HOME", ""), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config")] {
            std::env::set_var(k, home.join(d));
        }
        std::env::set_var("XDG_RUNTIME_DIR", home);
    }
    let Some(_display) = broadway(home) else {
        eprintln!("skipped: gtk4-broadwayd not available");
        return;
    };

    let dir = home.join("dir");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("b.txt"), "b").unwrap();

    let sock = home.join("noxd.sock");
    let listener_sock = sock.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::UnixListener::bind(&listener_sock).unwrap();
            noxd::Daemon::headless(listener_sock.clone()).serve(listener).await.unwrap();
        });
    });
    while !sock.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }

    gtk::init().unwrap();
    let ctx = glib::MainContext::default();
    let _owner = ctx.acquire().unwrap();
    let app = gtk::Application::builder().application_id("dev.noxfm.Test").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let (daemon, conn) = Daemon::start(sock);
    let b = Browser::open(&app, daemon, conn, Loc::Dir(dir.clone()), None);
    let p = b.pane();

    wait_for("first listing", || names(&p) == ["a", "sub", "b.txt"]);
    assert_eq!(b.path_bar.text(), format!("{}/", dir.display()));

    // Copy, then paste in another folder.
    p.select_only(&dir.join("b.txt"));
    p.to_clipboard(TransferOp::Copy);
    assert_eq!(p.state.borrow().notice.as_deref(), Some("Copied 1 item"));
    p.load(Loc::Dir(dir.join("sub")), Nav::New);
    wait_for("sub listed", || p.here() == dir.join("sub"));
    p.paste(false);
    wait_for("copy pasted", || names(&p) == ["b.txt"]);
    assert!(dir.join("b.txt").exists());

    // Cut: dimmed until pasted, then moved.
    p.go_back();
    wait_for("back", || p.here() == dir && names(&p).len() == 3);
    p.select_only(&dir.join("b.txt"));
    p.to_clipboard(TransferOp::Move);
    assert!(p.cells.is_cut(&dir.join("b.txt")));
    p.load(Loc::Dir(dir.join("a")), Nav::New);
    wait_for("a listed", || p.here() == dir.join("a"));
    p.paste(false);
    wait_for("cut pasted", || names(&p) == ["b.txt"]);
    assert!(!dir.join("b.txt").exists(), "moved, not copied");
    assert!(!p.cells.is_cut(&dir.join("b.txt")), "a cut is pasted once");

    // New folder: selected and waiting to be renamed; undo trashes it.
    p.create(NewKind::Folder);
    wait_for("new folder", || names(&p).len() == 2);
    wait_for("pending rename used", || p.pending_rename.borrow().is_none());
    assert_eq!(p.selected_entries().len(), 1);
    p.undo();
    wait_for("undone", || names(&p) == ["b.txt"]);

    // A second tab; renaming the folder the first one shows moves both.
    let t2 = b.open_tab(Loc::Dir(dir.clone()), true);
    assert!(Rc::ptr_eq(&b.pane(), &t2));
    assert!(b.notebook.shows_tabs());
    wait_for("tab listed", || names(&t2) == ["a", "sub"]);
    let a = dir.join("a");
    t2.request_then(Request::Rename { path: a.clone(), new_name: "a2".into() }, |_, _| {});
    wait_for("followed the rename", || p.here() == dir.join("a2") && names(&p) == ["b.txt"]);
    assert_eq!(p.state.borrow().back.last(), Some(&Loc::Dir(dir.clone())), "history kept, not extended");
    wait_for("other tab relisted", || names(&t2).contains(&"a2".to_owned()));
    b.close_tab(&t2);
    assert!(!b.notebook.shows_tabs());
    assert!(Rc::ptr_eq(&b.pane(), &p));

    // Trash: listed with where it came from, restored from there.
    p.select_only(&dir.join("a2/b.txt"));
    p.trash();
    wait_for("trashed", || names(&p).is_empty());
    p.load(Loc::Trash, Nav::New);
    // The new folder undone above is there too.
    wait_for("trash listed", || names(&p).contains(&"b.txt".to_owned()));
    assert_eq!(names(&p).len(), 2);
    assert_eq!(b.path_bar.text(), "Trash");
    let pos = names(&p).iter().position(|n| n == "b.txt").unwrap() as u32;
    let trashed = entry_of(&p.selection.item(pos).unwrap()).path.clone();
    assert!(p.cells.caption(&trashed).is_some_and(|c| c.starts_with("from ")));
    p.select_only(&trashed);
    assert!(b.item_menu(&p).n_items() == 2, "Restore / Delete permanently");
    p.restore();
    wait_for("restored", || names(&p) == ["New folder"] && dir.join("a2/b.txt").exists());
    p.go_up();
    wait_for("up from the Trash", || p.loc() == Loc::Dir(dir.join("a2")));

    // Recent keeps daemon order; leaving it restores the folder sort.
    p.load(Loc::Recent(None), Nav::New);
    wait_for("recent", || p.loc() == Loc::Recent(None));
    assert!(p.list.sorter.primary_sort_column().is_none());
    p.go_back();
    wait_for("back to the folder", || p.loc() == Loc::Dir(dir.join("a2")));
    assert!(p.list.sorter.primary_sort_column().is_some());

    // Hidden files.
    std::fs::write(dir.join("a2/.h"), "").unwrap();
    wait_for("hidden file listed", || p.store.n_items() == 2);
    assert_eq!(names(&p), ["b.txt"]);
    action(&b, "win.show-hidden", None);
    assert_eq!(names(&p).len(), 2);

    // Views and zoom.
    action(&b, "win.view", Some(&"grid".to_variant()));
    assert!(p.mode.get() == ViewMode::Grid);
    assert_eq!(p.root.visible_child_name().as_deref(), Some("grid"));
    action(&b, "win.zoom-in", None);
    assert_eq!(p.cells.grid_zoom.get(), GRID_ZOOM + 1);
    action(&b, "win.zoom-reset", None);
    assert_eq!(p.cells.grid_zoom.get(), GRID_ZOOM);

    // Menus for a file and for the background build.
    p.select_only(&dir.join("a2/b.txt"));
    assert!(b.item_menu(&p).n_items() >= 4);
    p.selection.unselect_all();
    assert!(b.background_menu(&p).n_items() >= 3);

    // Properties: totals arrive, permissions change the file.
    use std::os::unix::fs::PermissionsExt;
    let file = dir.join("a2/b.txt");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let props = b.show_properties(vec![file.clone()]).unwrap();
    wait_for("properties filled", || find::<gtk::CheckButton>(&props).len() == 9);
    let owner_write = &find::<gtk::CheckButton>(&props)[1];
    assert!(owner_write.is_active());
    owner_write.set_active(false);
    wait_for("chmod", || std::fs::metadata(&file).unwrap().permissions().mode() & 0o777 == 0o444);
    props.destroy();

    // Preview of a text file.
    std::fs::write(dir.join("a2/notes.txt"), "hello preview").unwrap();
    wait_for("notes listed", || names(&p).contains(&"notes.txt".to_owned()));
    action(&b, "win.preview", None);
    p.select_only(&dir.join("a2/notes.txt"));
    wait_for("preview text", || {
        find::<gtk::TextView>(&b.preview).first().is_some_and(|t| {
            let buf = t.buffer();
            buf.text(&buf.start_iter(), &buf.end_iter(), false) == "hello preview"
        })
    });
    action(&b, "win.preview", None);
    assert!(!b.preview.is_visible());

    // A copy shows in the transfers indicator, then goes.
    std::fs::write(dir.join("big.bin"), vec![7u8; 8 << 20]).unwrap();
    p.transfer(TransferOp::Copy, vec![dir.join("big.bin")], dir.join("sub"));
    wait_for("transfer shown", || b.transfers_button.is_visible());
    wait_for("transfer done", || dir.join("sub/big.bin").metadata().is_ok_and(|m| m.len() == 8 << 20));
    wait_for("transfer cleared", || !b.transfers_button.is_visible());

    // The app picker opens (its list comes from the system).
    let picker = b.app_picker("text/plain".into(), Some(dir.join("a2/notes.txt")));
    assert!(!find::<gtk::ListBox>(&picker).is_empty());
    picker.destroy();

    // Repositories get a "git" badge.
    std::fs::create_dir_all(dir.join("a2/repo/.git")).unwrap();
    action(&b, "win.view", Some(&"list".to_variant()));
    wait_for("git badge", || {
        find::<gtk::Label>(&p.list.view).iter().any(|l| l.text() == "git" && l.is_visible())
    });

    b.window.destroy();
}
