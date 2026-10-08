//! inotify watches on the directories that windows are showing.
//!
//! Watches are non-recursive: a window only needs to know when its own
//! listing changes. Events are coalesced per directory before going out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// How long a burst of events on one directory is collected before notifying.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct FsWatcher {
    inner: Mutex<RecommendedWatcher>,
}

impl FsWatcher {
    /// The receiver yields directories whose contents (may have) changed.
    pub fn new() -> notify::Result<(FsWatcher, mpsc::UnboundedReceiver<PathBuf>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                // The event may be about the watched dir itself or one of its children.
                if let Some(parent) = p.parent() {
                    let _ = tx.send(parent.to_path_buf());
                }
                let _ = tx.send(p);
            }
        })?;
        Ok((FsWatcher { inner: Mutex::new(w) }, rx))
    }

    pub fn watch(&self, dir: &Path) {
        if let Err(e) = self.inner.lock().unwrap().watch(dir, RecursiveMode::NonRecursive) {
            tracing::warn!(dir = %dir.display(), %e, "watch failed");
        }
    }

    pub fn unwatch(&self, dir: &Path) {
        let _ = self.inner.lock().unwrap().unwatch(dir);
    }
}

/// Collects paths from `rx` and hands them to `flush` in batches, at most
/// once per [`DEBOUNCE`] window.
pub async fn debounce(mut rx: mpsc::UnboundedReceiver<PathBuf>, mut flush: impl FnMut(HashSet<PathBuf>)) {
    loop {
        let Some(first) = rx.recv().await else { return };
        let mut pending = HashSet::from([first]);
        let deadline = tokio::time::sleep(DEBOUNCE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                p = rx.recv() => match p {
                    Some(p) => { pending.insert(p); }
                    None => break,
                },
            }
        }
        flush(pending);
    }
}
