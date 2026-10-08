//! Seamless updates: when the noxd program on disk is replaced (by pacman, or
//! `cargo build`), the running daemon re-executes itself into the new one.
//!
//! - It waits for running transfers to finish and saves state first.
//! - `exec` keeps the PID, so systemd sees nothing happen.
//! - Open windows lose their connection and reconnect within a second.
//! - A window from a newer build connecting to us (protocol mismatch) is
//!   what usually reveals an update; that triggers the same check at once.

use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::Daemon;

/// How often the program file is looked at.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// While transfers run, how often to check whether they're done.
const DRAIN_EVERY: Duration = Duration::from_secs(2);

/// Identity of a program file: replaced files get a new inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    dev: u64,
    ino: u64,
    mtime: i64,
}

impl FileId {
    pub fn of(path: &std::path::Path) -> Option<FileId> {
        let m = std::fs::metadata(path).ok()?;
        Some(FileId { dev: m.dev(), ino: m.ino(), mtime: m.mtime() })
    }
}

pub struct SelfUpdate {
    exe: Option<PathBuf>,
    started_as: Option<FileId>,
    /// Lets a version mismatch on connect ask for an immediate check.
    pub check_now: tokio::sync::Notify,
}

impl SelfUpdate {
    pub fn new() -> Self {
        // /proc/self/exe reads "… (deleted)" once replaced; resolve it now.
        let exe = std::env::current_exe().ok();
        let started_as = exe.as_deref().and_then(FileId::of);
        SelfUpdate { exe, started_as, check_now: tokio::sync::Notify::new() }
    }

    /// The program file was replaced by a new, executable one.
    pub fn replaced(&self) -> bool {
        let (Some(exe), Some(old)) = (&self.exe, self.started_as) else { return false };
        let executable = std::fs::metadata(exe).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0);
        executable && FileId::of(exe).is_some_and(|now| now != old)
    }
}

impl Default for SelfUpdate {
    fn default() -> Self {
        Self::new()
    }
}

impl Daemon {
    /// Watches for a new noxd and switches to it.
    pub fn start_self_update(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(CHECK_EVERY) => {}
                    _ = me.update.check_now.notified() => {}
                }
                if !me.update.replaced() {
                    continue;
                }
                // Give the installer a moment to finish writing the file.
                tokio::time::sleep(Duration::from_secs(1)).await;
                tracing::info!("a new noxd was installed; restarting into it once transfers are done");
                while !me.transfers.list().is_empty() {
                    tokio::time::sleep(DRAIN_EVERY).await;
                }
                me.save_recent();
                let err = me.reexec();
                tracing::error!(%err, "could not restart into the new noxd; keeping the old one");
                return;
            }
        });
        self.start_reaper();
    }

    /// Replaces this process with the program on disk (same PID, same args).
    /// Only returns on failure.
    fn reexec(&self) -> std::io::Error {
        let Some(exe) = &self.update.exe else { return std::io::Error::other("unknown executable") };
        // The new process binds the socket again; the stale file is cleaned up
        // there (connect fails, so it's removed).
        std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec()
    }

    /// Windows started before a restart are still our children but nobody
    /// waits for them anymore: reap them when they exit, leaving the ones this
    /// process started (and waits for) alone.
    fn start_reaper(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(30));
            loop {
                every.tick().await;
                for pid in zombie_children().into_iter().filter(|p| !me.supervisor.owns(*p)) {
                    if let Some(p) = rustix::process::Pid::from_raw(pid) {
                        let _ = rustix::process::waitpid(Some(p), rustix::process::WaitOptions::NOHANG);
                    }
                }
            }
        });
    }
}

/// PIDs of our children that have exited but weren't waited for.
fn zombie_children() -> Vec<i32> {
    let me = std::process::id().to_string();
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            // /proc/<pid>/stat: "pid (comm) state ppid …"; comm may contain spaces.
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
            let Some(rest) = stat.rsplit_once(") ").map(|(_, r)| r) else { return false };
            let mut f = rest.split_whitespace();
            f.next() == Some("Z") && f.next() == Some(me.as_str())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_a_replaced_program() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("noxd");
        std::fs::write(&exe, "old").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let u = SelfUpdate { exe: Some(exe.clone()), started_as: FileId::of(&exe), check_now: Default::default() };
        assert!(!u.replaced());

        // How package managers install: write elsewhere, rename over.
        let new = t.path().join("noxd.new");
        std::fs::write(&new, "new").unwrap();
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&new, &exe).unwrap();
        assert!(u.replaced());

        // Half-installed (not executable yet): wait.
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!u.replaced());
    }

    #[test]
    fn finds_no_zombies_normally() {
        assert!(zombie_children().is_empty());
    }
}
