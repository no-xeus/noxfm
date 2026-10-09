//! Drives a real window against a headless noxd, on an offscreen GTK
//! display (Broadway). Input can't be injected there, so the test calls the
//! actions and methods that keys, menus and clicks call.

use std::path::Path;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};
use noxfm_proto::{NewKind, Request, TransferOp};

use super::{Browser, Nav, ViewMode};
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

fn names(b: &Browser) -> Vec<String> {
    (0..b.selection.n_items()).filter_map(|i| b.selection.item(i)).map(|o| entry_of(&o).name.clone()).collect()
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
    let b = Browser::open(&app, daemon, conn, dir.clone());

    wait_for("first listing", || names(&b) == ["a", "sub", "b.txt"]);

    // Copy, then paste in another folder.
    b.select_only(&dir.join("b.txt"));
    b.to_clipboard(TransferOp::Copy);
    assert_eq!(b.state.borrow().notice.as_deref(), Some("Copied 1 item"));
    b.load(dir.join("sub"), Nav::New);
    wait_for("sub listed", || b.here() == dir.join("sub"));
    b.paste(false);
    wait_for("copy pasted", || names(&b) == ["b.txt"]);
    assert!(dir.join("b.txt").exists());

    // Cut: dimmed until pasted, then moved.
    b.go_back();
    wait_for("back", || b.here() == dir && names(&b).len() == 3);
    b.select_only(&dir.join("b.txt"));
    b.to_clipboard(TransferOp::Move);
    assert!(b.cells.is_cut(&dir.join("b.txt")));
    b.load(dir.join("a"), Nav::New);
    wait_for("a listed", || b.here() == dir.join("a"));
    b.paste(false);
    wait_for("cut pasted", || names(&b) == ["b.txt"]);
    assert!(!dir.join("b.txt").exists(), "moved, not copied");
    assert!(!b.cells.is_cut(&dir.join("b.txt")), "a cut is pasted once");

    // New folder: selected and waiting to be renamed; undo trashes it.
    b.create(NewKind::Folder);
    wait_for("new folder", || names(&b).len() == 2);
    wait_for("pending rename used", || b.pending_rename.borrow().is_none());
    assert_eq!(b.selected_entries().len(), 1);
    b.undo();
    wait_for("undone", || names(&b) == ["b.txt"]);

    // Renamed elsewhere while shown: the window follows.
    let a = dir.join("a");
    b.request_then(Request::Rename { path: a.clone(), new_name: "a2".into() }, |_, _| {});
    wait_for("followed the rename", || b.here() == dir.join("a2") && names(&b) == ["b.txt"]);
    assert_eq!(b.state.borrow().back.last(), Some(&dir), "history kept, not extended");

    // Hidden files.
    std::fs::write(dir.join("a2/.h"), "").unwrap();
    wait_for("hidden file listed", || b.store.n_items() == 2);
    assert_eq!(names(&b), ["b.txt"]);
    action(&b, "win.show-hidden", None);
    assert_eq!(names(&b).len(), 2);

    // Views and zoom.
    action(&b, "win.view", Some(&"grid".to_variant()));
    assert!(b.mode.get() == ViewMode::Grid);
    assert_eq!(b.views.visible_child_name().as_deref(), Some("grid"));
    action(&b, "win.zoom-in", None);
    assert_eq!(b.cells.grid_zoom.get(), GRID_ZOOM + 1);
    action(&b, "win.zoom-reset", None);
    assert_eq!(b.cells.grid_zoom.get(), GRID_ZOOM);

    // Menus for a file and for the background build.
    b.select_only(&dir.join("a2/b.txt"));
    assert!(b.item_menu().n_items() >= 4);
    b.selection.unselect_all();
    assert!(b.background_menu().n_items() >= 3);

    b.window.destroy();
}
