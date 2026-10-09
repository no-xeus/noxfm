//! The noxfm window in GTK4, replacing the libcosmic one (docs/ui-rewrite.md).
//!
//! noxd runs it as `noxfm-gtk --window [OPTIONS] [PATH]` when started with
//! `NOXFM_WINDOW_BIN=noxfm-gtk`. Its stderr goes to
//! `~/.local/state/noxfm/windows.log`.

mod cells;
mod complete;
mod daemon;
mod grid;
mod list;
mod window;

use anyhow::Context;
use gtk::prelude::*;
use gtk::{gio, glib};

fn main() -> anyhow::Result<glib::ExitCode> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", noxfm_proto::version_line("noxfm-gtk"));
        return Ok(glib::ExitCode::SUCCESS);
    }
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("NOXFM_LOG", "warn")).init();

    let mut args = std::env::args_os().skip(1).peekable();
    args.next_if(|a| a == "--window");
    // `--recent`, `--trash` and the sidebar layout come with the sidebar (phase 4).
    while args.next_if(|a| a.to_str().is_some_and(|s| s.starts_with("--"))).is_some() {}
    let start = match args.next() {
        Some(p) => std::fs::canonicalize(&p).with_context(|| format!("{}", std::path::Path::new(&p).display()))?,
        None => std::env::current_dir()?,
    };

    let (daemon, conn) = daemon::Daemon::start(noxfm_proto::socket_path());
    // One window per process, as noxd starts them: no single-instance handoff.
    let app = gtk::Application::builder()
        .application_id("dev.noxfm.Browser")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    window::set_accels(&app);
    app.connect_activate(move |app| window::Browser::open(app, daemon.clone(), conn.clone(), start.clone()));
    // Arguments were handled above; GApplication would reject ours.
    Ok(app.run_with_args::<&str>(&[]))
}
