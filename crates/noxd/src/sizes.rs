//! Recursive directory sizes, computed in the background and cached.
//!
//! Sizes are apparent sizes (sum of file lengths), stay on one filesystem and
//! don't follow symlinks. A cached value is trusted for [`TTL`] unless the
//! watcher saw a change underneath it; we only watch open directories, so
//! deep changes elsewhere are picked up when the TTL runs out.
//!
//! The cache is persisted to `~/.cache/noxfm/sizes`, so a restarted daemon
//! shows the last known sizes at once. Loaded values count as stale: they are
//! shown, and walked again.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Semaphore;

pub const TTL: Duration = Duration::from_secs(120);

/// Walks running at once; each one is disk-bound.
const PARALLEL_WALKS: usize = 2;

/// How often a walk checks whether it is still wanted.
const CANCEL_CHECK_EVERY: usize = 4096;

/// A walk also records the folders this deep below it, so opening one of
/// them needs no walk of its own.
const SUBDIR_DEPTH: usize = 2;

/// Saved sizes not measured again for this long are dropped on load.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

pub const SAVE_EVERY: Duration = Duration::from_secs(60);

struct Cached {
    bytes: u64,
    /// `None` for values loaded from disk: shown, but never fresh.
    measured: Option<Instant>,
    /// Unix time of the measure, for [`MAX_AGE`].
    at: u64,
}

pub struct Sizes {
    cache: Mutex<HashMap<PathBuf, Cached>>,
    inflight: Mutex<HashSet<PathBuf>>,
    pub permits: Semaphore,
    /// Where the cache is saved; `None` = not persisted (tests).
    file: Option<PathBuf>,
    dirty: AtomicBool,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            cache: Default::default(),
            inflight: Default::default(),
            permits: Semaphore::new(PARALLEL_WALKS),
            file: None,
            dirty: AtomicBool::new(false),
        }
    }
}

impl Sizes {
    /// The user's cache file.
    pub fn for_user() -> Self {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".cache"));
        Self::at(cache.join("noxfm/sizes"))
    }

    /// Loads `file` if it exists, and saves there.
    pub fn at(file: PathBuf) -> Self {
        let cache = std::fs::File::open(&file)
            .map(|f| load(std::io::BufReader::new(f), unix_now()))
            .unwrap_or_default();
        Sizes { cache: Mutex::new(cache), file: Some(file), ..Default::default() }
    }

    /// The cached size and whether it is still fresh.
    pub fn cached(&self, dir: &Path) -> Option<(u64, bool)> {
        let cache = self.cache.lock().unwrap();
        cache.get(dir).map(|c| (c.bytes, c.measured.is_some_and(|at| at.elapsed() < TTL)))
    }

    pub fn store(&self, dir: PathBuf, bytes: u64) {
        let c = Cached { bytes, measured: Some(Instant::now()), at: unix_now() };
        self.cache.lock().unwrap().insert(dir, c);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Something inside `path` changed: every ancestor's total is stale.
    pub fn invalidate(&self, path: &Path) {
        let mut cache = self.cache.lock().unwrap();
        for p in path.ancestors() {
            if cache.remove(p).is_some() {
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Returns false if a walk for `dir` is already running.
    pub fn begin(&self, dir: &Path) -> bool {
        self.inflight.lock().unwrap().insert(dir.to_path_buf())
    }

    pub fn end(&self, dir: &Path) {
        self.inflight.lock().unwrap().remove(dir);
    }

    /// Blocking. Writes the cache if it changed since the last save.
    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(file.parent().expect("cache file has a parent"))?;
            let tmp = file.with_extension("tmp");
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            save(&self.cache.lock().unwrap(), &mut f)?;
            f.flush()?;
            std::fs::rename(tmp, file)
        };
        if let Err(e) = write() {
            tracing::warn!(%e, "could not save folder sizes");
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `<unix time>\t<bytes>\t<path>` per line; paths are raw bytes, and those
/// holding a newline are not saved.
fn save(cache: &HashMap<PathBuf, Cached>, mut w: impl Write) -> std::io::Result<()> {
    for (path, c) in cache {
        let p = path.as_os_str().as_bytes();
        if p.contains(&b'\n') {
            continue;
        }
        write!(w, "{}\t{}\t", c.at, c.bytes)?;
        w.write_all(p)?;
        w.write_all(b"\n")?;
    }
    Ok(())
}

fn load(r: impl BufRead, now: u64) -> HashMap<PathBuf, Cached> {
    let parse = |line: &[u8]| -> Option<(PathBuf, Cached)> {
        let mut parts = line.splitn(3, |&b| b == b'\t');
        let at: u64 = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
        let bytes = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(parts.next()?));
        (now.saturating_sub(at) < MAX_AGE.as_secs()).then_some((path, Cached { bytes, measured: None, at }))
    };
    r.split(b'\n').map_while(Result::ok).filter_map(|l| parse(&l)).collect()
}

/// What a walk measured.
#[derive(Debug, Default, PartialEq)]
pub struct Walk {
    /// The walked folder's total; `None` if the walk was stopped.
    pub total: Option<u64>,
    /// Folders down to [`SUBDIR_DEPTH`] below it that were walked entirely
    /// (so also those finished before a stop), on the same filesystem.
    pub subdirs: Vec<(PathBuf, u64)>,
}

/// Blocking. Stops early if `keep_going` says so.
pub fn walk(dir: &Path, keep_going: impl Fn() -> bool) -> Walk {
    let mut out = Walk::default();
    let root_dev = std::fs::metadata(dir).map(|m| m.dev()).ok();
    // Contents first: a folder comes after everything in it, so its total is
    // complete when it shows up. `sums[d]` is what has been counted at depth
    // `d` inside the folder at depth `d - 1` being walked.
    let mut sums = vec![0u64; 2];
    let walker = walkdir::WalkDir::new(dir).follow_links(false).same_file_system(true).contents_first(true);
    for (i, entry) in walker.into_iter().enumerate() {
        if i % CANCEL_CHECK_EVERY == CANCEL_CHECK_EVERY - 1 && !keep_going() {
            return out;
        }
        // Unreadable subtrees are skipped; the total is then a lower bound.
        let Ok(entry) = entry else { continue };
        let depth = entry.depth();
        if sums.len() < depth + 2 {
            sums.resize(depth + 2, 0);
        }
        if entry.file_type().is_dir() {
            let inside = std::mem::take(&mut sums[depth + 1]);
            sums[depth] += inside;
            if depth == 0 {
                out.total = Some(inside);
                return out;
            }
            // A mount point is listed but not entered: its 0 isn't its size.
            if depth <= SUBDIR_DEPTH && entry.metadata().is_ok_and(|m| Some(m.dev()) == root_dev) {
                out.subdirs.push((entry.into_path(), inside));
            }
        } else if entry.file_type().is_file()
            && let Ok(md) = entry.metadata()
        {
            sums[depth] += md.len();
        }
    }
    // The folder itself couldn't be read.
    out.total = Some(sums[1]);
    out
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
        assert_eq!(walk(&r.join("a"), || true).total, Some(123));
    }

    #[test]
    fn walk_records_subdirs_down_to_the_depth_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        std::fs::create_dir_all(r.join("a/b/c/d")).unwrap();
        std::fs::create_dir_all(r.join("e")).unwrap();
        std::fs::write(r.join("top"), [0u8; 1]).unwrap();
        std::fs::write(r.join("a/x"), [0u8; 10]).unwrap();
        std::fs::write(r.join("a/b/y"), [0u8; 100]).unwrap();
        std::fs::write(r.join("a/b/c/d/z"), [0u8; 1000]).unwrap();
        let mut w = walk(r, || true);
        w.subdirs.sort();
        assert_eq!(w.total, Some(1111));
        assert_eq!(w.subdirs, [(r.join("a"), 1110), (r.join("a/b"), 1100), (r.join("e"), 0)]);
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

    #[test]
    fn saved_sizes_load_stale_and_old_ones_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("noxfm/sizes");
        let s = Sizes::at(file.clone());
        s.store("/a".into(), 1);
        s.store("/tab\there".into(), 2);
        s.store("/new\nline".into(), 3);
        s.save();
        let old = unix_now() - MAX_AGE.as_secs() - 10;
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, "{old}\t4\t/old").unwrap();

        let s = Sizes::at(file);
        assert_eq!(s.cached(Path::new("/a")), Some((1, false)));
        assert_eq!(s.cached(Path::new("/tab\there")), Some((2, false)));
        assert_eq!(s.cached(Path::new("/new\nline")), None);
        assert_eq!(s.cached(Path::new("/old")), None);
    }
}
