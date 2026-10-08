use std::os::unix::fs::PermissionsExt;

use anyhow::Context;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", noxfm_proto::version_line("noxd"));
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "noxd=info".into()))
        .init();

    let path = noxfm_proto::socket_path();
    let dir = path.parent().context("socket path has no parent")?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            anyhow::bail!("noxd is already running on {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    tracing::info!(socket = %path.display(), "listening");

    let daemon = noxd::Daemon::new(path.clone());
    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        r = daemon.clone().serve(listener) => r?,
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    daemon.save_recent();
    let _ = std::fs::remove_file(&path);
    Ok(())
}
