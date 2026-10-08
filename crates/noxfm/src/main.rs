//! `noxfm [PATH]` asks the daemon to open a window (starting noxd if needed).
//! `noxfm --window [--recent[=KIND]|--trash] [PATH]` is the window itself;
//! only noxd runs it that way.

use noxfm::browser;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;
use noxfm_proto::{Client, Request, Role};

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", noxfm_proto::version_line("noxfm"));
        return Ok(());
    }
    let mut args = std::env::args_os().skip(1).peekable();
    let window = args.next_if(|a| a == "--window").is_some();
    // Window options, in any order, before the path.
    let (mut view, mut layout) = (None, None);
    while let Some(a) = args.next_if(|a| a.to_str().is_some_and(|s| s.starts_with("--"))) {
        let a = a.to_string_lossy();
        if let Some(v) = noxfm_proto::StartView::from_arg(&a) {
            view = Some(v);
        } else if let Some(l) = noxfm_proto::WindowLayout::from_arg(&a) {
            layout = Some(l);
        }
    }
    let path = match args.next() {
        Some(p) => std::fs::canonicalize(&p).with_context(|| format!("{}", Path::new(&p).display()))?,
        None => std::env::current_dir()?,
    };

    if window {
        // Windows run as children of noxd and inherit its environment; their
        // stderr goes to ~/.local/state/noxfm/windows.log.
        env_logger::Builder::from_env(env_logger::Env::new().filter_or("NOXFM_LOG", "warn")).init();
        let flags = browser::Flags { socket: noxfm_proto::socket_path(), start: path, view, layout };
        let settings = cosmic::app::Settings::default().size(cosmic::iced::Size::new(1000.0, 700.0));
        cosmic::app::run::<browser::App>(settings, flags)?;
        Ok(())
    } else {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(launch(path))
    }
}

async fn launch(path: PathBuf) -> anyhow::Result<()> {
    let socket = noxfm_proto::socket_path();
    let client = match Client::connect(&socket, Role::Launcher).await {
        Ok((c, _)) => c,
        Err(_) => {
            start_daemon()?;
            connect_retry(&socket).await?
        }
    };
    client.request(Request::OpenWindow { path: Some(path), view: None, layout: None }).await?;
    Ok(())
}

/// Prefer the systemd unit; fall back to running noxd from next to us.
fn start_daemon() -> anyhow::Result<()> {
    let via_systemd = Command::new("systemctl")
        .args(["--user", "start", "noxd.service"])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if via_systemd {
        return Ok(());
    }
    let sibling = std::env::current_exe()?.with_file_name("noxd");
    let bin = if sibling.is_file() { sibling } else { PathBuf::from("noxd") };
    Command::new(&bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    Ok(())
}

async fn connect_retry(socket: &Path) -> anyhow::Result<Client> {
    let mut last = None;
    for _ in 0..30 {
        match Client::connect(socket, Role::Launcher).await {
            Ok((c, _)) => return Ok(c),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last.map(Into::into).unwrap_or_else(|| anyhow::anyhow!("noxd did not start")))
}
