//! Recursive directory sizes, computed in the background and cached.
//!
//! Sizes are apparent sizes (sum of file lengths), stay on one filesystem and
//! don't follow symlinks. A cached value is trusted for [`TTL`] unless the
//! watcher saw a change underneath it; we only watch open directories, so
//! deep changes elsewhere are picked up when the TTL runs out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

pub const TTL: Duration = Duration::from_secs(120);

/// Walks running at once; each one is disk-bound.
const PARALLEL_WALKS: usize = 2;

/// How often a walk checks whether it is still wanted.
const CANCEL_CHECK_EVERY: usize = 4096;

pub struct Sizes {
    cache: Mutex<HashMap<PathBuf, (u64, Instant)>>,
    inflight: Mutex<HashSet<PathBuf>>,
    pub permits: Semaphore,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            cache: Default::default(),
            inflight: Default::default(),
            permits: Semaphore::new(PARALLEL_WALKS),
        }
    }
}

impl Sizes {
    /// The cached size and whether it is still fresh.
    pub fn cached(&self, dir: &Path) -> Option<(u64, bool)> {
        self.cache.lock().unwrap().get(dir).map(|&(b, at)| (b, at.elapsed() < TTL))
    }

    pub fn store(&self, dir: PathBuf, bytes: u64) {
        self.cache.lock().unwrap().insert(dir, (bytes, Instant::now()));
    }

    /// Something inside `path` changed: every ancestor's total is stale.
    pub fn invalidate(&self, path: &Path) {
        let mut cache = self.cache.lock().unwrap();
        for p in path.ancestors() {
            cache.remove(p);
        }
    }

    /// Returns false if a walk for `dir` is already running.
    pub fn begin(&self, dir: &Path) -> bool {
        self.inflight.lock().unwrap().insert(dir.to_path_buf())
    }

    pub fn end(&self, dir: &Path) {
        self.inflight.lock().unwrap().remove(dir);
    }
}

/// Blocking. Returns `None` if `keep_going` said stop.
pub fn walk(dir: &Path, keep_going: impl Fn() -> bool) -> Option<u64> {
    let mut total = 0u64;
    let walker = walkdir::WalkDir::new(dir).follow_links(false).same_file_system(true);
    for (i, entry) in walker.into_iter().enumerate() {
        if i % CANCEL_CHECK_EVERY == CANCEL_CHECK_EVERY - 1 && !keep_going() {
            return None;
        }
        // Unreadable subtrees are skipped; the total is then a lower bound.
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file()
            && let Ok(md) = entry.metadata()
        {
            total += md.len();
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_sums_files_and_skips_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        std::fs::write(r.join("a/x"), [0u8; 100]).unwrap();
        std::fs::write(r.join("a/b/y"), [0u8; 23]).unwrap();
        std::os::unix::fs::symlink(r.join("a/x"), r.join("a/link")).unwrap();
        assert_eq!(walk(&r.join("a"), || true), Some(123));
    }

    #[test]
    fn invalidate_clears_ancestors_only() {
        let s = Sizes::default();
        s.store("/a".into(), 1);
        s.store("/a/b".into(), 2);
        s.store("/a/b/c".into(), 3);
        s.store("/a/z".into(), 4);
        s.invalidate(Path::new("/a/b"));
        assert_eq!(s.cached(Path::new("/a")), None);
        assert_eq!(s.cached(Path::new("/a/b")), None);
        assert_eq!(s.cached(Path::new("/a/b/c")), Some((3, true)));
        assert_eq!(s.cached(Path::new("/a/z")), Some((4, true)));
    }
}
