//! Spawns and reaps the GUI child processes.

use std::path::{Path, PathBuf};

use tokio::process::Command;

pub struct Supervisor {
    socket: PathBuf,
    /// Display variables last reported by a client (see `Hello::display_env`).
    display_env: std::sync::Mutex<Vec<(String, String)>>,
    /// Children this process started and waits for.
    spawned: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<i32>>>,
    bin_dir: Option<PathBuf>,
    /// Off in tests, so they don't open windows.
    enabled: bool,
}

impl Supervisor {
    /// Child binaries are looked up next to the running `noxd` first, then on `$PATH`.
    pub fn new(socket: PathBuf) -> Self {
        let bin_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        Supervisor { socket, bin_dir, enabled: true, display_env: Default::default(), spawned: Default::default() }
    }

    pub fn headless(socket: PathBuf) -> Self {
        Supervisor { enabled: false, ..Self::new(socket) }
    }

    fn resolve(&self, name: &str) -> PathBuf {
        self.bin_dir
            .as_ref()
            .map(|d| d.join(name))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from(name))
    }

    pub fn spawn(&self, bin: &str, args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
        if !self.enabled {
            tracing::debug!(bin, "headless: not spawning");
            return Ok(());
        }
        let path = self.resolve(bin);
        let env = self.display_env.lock().unwrap().clone();
        let mut child = Command::new(&path)
            .args(args)
            .envs(env)
            .env(noxfm_proto::SOCKET_ENV, &self.socket)
            .stdin(std::process::Stdio::null())
            .stderr(child_log().map_or_else(std::process::Stdio::inherit, Into::into))
            .spawn()?;
        let pid = child.id();
        tracing::info!(?path, ?pid, "spawned child");
        let spawned = self.spawned.clone();
        if let Some(p) = pid {
            spawned.lock().unwrap().insert(p as i32);
        }
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) if !status.success() => tracing::warn!(?pid, %status, "child exited"),
                Ok(_) => tracing::debug!(?pid, "child exited"),
                Err(e) => tracing::warn!(?pid, %e, "wait failed"),
            }
            if let Some(p) = pid {
                spawned.lock().unwrap().remove(&(p as i32));
            }
        });
        Ok(())
    }

    /// Whether this process started `pid` (and so already waits for it).
    pub fn owns(&self, pid: i32) -> bool {
        self.spawned.lock().unwrap().contains(&pid)
    }

    /// Remembers a client's display variables for the windows we open.
    pub fn note_display_env(&self, env: &[(String, String)]) {
        if env.iter().any(|(k, _)| k == "WAYLAND_DISPLAY" || k == "DISPLAY") {
            *self.display_env.lock().unwrap() = env.to_vec();
        }
    }

    pub fn log_path() -> PathBuf {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".local/state"));
        state.join("noxfm").join("windows.log")
    }

    pub fn open_browser(
        &self,
        path: Option<&Path>,
        view: Option<noxfm_proto::StartView>,
        layout: Option<noxfm_proto::WindowLayout>,
    ) -> std::io::Result<()> {
        let view_arg = view.map(|v| v.to_arg());
        let layout_arg = layout.map(|l| l.to_arg());
        let mut args: Vec<&std::ffi::OsStr> = vec!["--window".as_ref()];
        for a in view_arg.iter().chain(&layout_arg) {
            args.push(a.as_ref());
        }
        if let Some(p) = path {
            args.push(p.as_os_str());
        }
        self.spawn("noxfm", &args)
    }
}

/// Window processes append their stderr (including panics) here.
fn child_log() -> Option<std::fs::File> {
    let path = Supervisor::log_path();
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}
