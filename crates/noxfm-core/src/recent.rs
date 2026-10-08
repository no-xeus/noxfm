//! The "Recent" log: files downloaded, edited or created in the listen
//! directories, newest first.
//!
//! The daemon turns filesystem events into calls to [`RecentLog::record`];
//! this module decides what counts and keeps the log bounded. It is
//! persisted as JSON lines.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use noxfm_proto::{RecentItem, RecentKind, Timestamp};

/// Folders not worth watching or reporting: build output, VCS internals,
/// dependency trees.
pub const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "__pycache__", ".cache", "build", "dist", ".venv"];

/// Suffixes of in-progress downloads.
const PARTIAL: &[&str] = &[".part", ".crdownload", ".download", ".partial", ".tmp", ".opdownload"];

/// A second event of the same kind for the same file within this many
/// seconds is folded into the first.
const COALESCE_SECS: i64 = 30;

pub fn is_partial(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    PARTIAL.iter().any(|s| name.ends_with(s))
}

/// Hidden files, editor swap/backup files, partial downloads, and anything
/// under a skipped or hidden folder below `root`.
pub fn is_ignored(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return true };
    let hidden_or_skipped = rel.components().any(|c| match c {
        Component::Normal(n) => {
            let n = n.to_string_lossy();
            n.starts_with('.') || SKIP_DIRS.contains(&n.as_ref())
        }
        _ => false,
    });
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    hidden_or_skipped
        || is_partial(path)
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.starts_with(".#")
        || name.starts_with('#') && name.ends_with('#')
        || name == "4913" // vim's write probe
}

#[derive(Debug)]
pub struct RecentLog {
    items: VecDeque<RecentItem>,
    cap: usize,
}

impl RecentLog {
    pub fn new(cap: usize) -> Self {
        RecentLog { items: VecDeque::new(), cap }
    }

    /// Returns false if the event was folded into an earlier one.
    pub fn record(&mut self, path: PathBuf, kind: RecentKind, at: Timestamp) -> bool {
        let recent_same = self.items.iter().rev().take(64).any(|i| {
            i.path == path && at - i.at < COALESCE_SECS && (i.kind == kind || i.kind != RecentKind::Modified && kind == RecentKind::Modified)
        });
        // "Created" or "Downloaded" moments ago already says more than "edited".
        if recent_same {
            return false;
        }
        self.items.push_back(RecentItem { path, kind, at });
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
        true
    }

    /// Newest first, one entry per file (its latest event), optionally of one
    /// kind only. Files that no longer exist are skipped.
    pub fn query(&self, kind: Option<RecentKind>, limit: usize, exists: impl Fn(&Path) -> bool) -> Vec<RecentItem> {
        let mut latest: HashMap<&Path, &RecentItem> = HashMap::new();
        for item in &self.items {
            // Later entries win ties: they were recorded after.
            match latest.get(item.path.as_path()) {
                Some(prev) if prev.at > item.at => {}
                _ => {
                    latest.insert(&item.path, item);
                }
            }
        }
        let mut out: Vec<RecentItem> = latest
            .into_values()
            .filter(|i| kind.is_none_or(|k| k == i.kind))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.path.cmp(&b.path)));
        out.retain(|i| exists(&i.path));
        out.truncate(limit);
        out
    }

    /// Forgets a path (deleted, or moved away).
    pub fn forget(&mut self, path: &Path) {
        self.items.retain(|i| i.path != path);
    }

    pub fn load(r: impl BufRead, cap: usize) -> Self {
        let mut log = RecentLog::new(cap);
        for line in r.lines().map_while(Result::ok) {
            if let Some(item) = parse_line(&line) {
                log.items.push_back(item);
            }
        }
        while log.items.len() > cap {
            log.items.pop_front();
        }
        log
    }

    pub fn save(&self, mut w: impl Write) -> io::Result<()> {
        for i in &self.items {
            writeln!(w, "{}", format_line(i))?;
        }
        Ok(())
    }
}

/// `<unix time>\t<C|M|D>\t<path>`: paths may hold any byte but a newline.
fn format_line(i: &RecentItem) -> String {
    let k = match i.kind {
        RecentKind::Created => 'C',
        RecentKind::Modified => 'M',
        RecentKind::Downloaded => 'D',
    };
    format!("{}\t{k}\t{}", i.at, i.path.display())
}

fn parse_line(line: &str) -> Option<RecentItem> {
    let mut parts = line.splitn(3, '\t');
    let at = parts.next()?.parse().ok()?;
    let kind = match parts.next()? {
        "C" => RecentKind::Created,
        "M" => RecentKind::Modified,
        "D" => RecentKind::Downloaded,
        _ => return None,
    };
    Some(RecentItem { path: PathBuf::from(parts.next()?), kind, at })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_noise() {
        let root = Path::new("/home/u/Projects");
        for p in [
            "/home/u/Projects/app/target/debug/x",
            "/home/u/Projects/app/.git/index",
            "/home/u/Projects/app/node_modules/a/b.js",
            "/home/u/Projects/app/.main.rs.swp",
            "/home/u/Projects/app/main.rs~",
            "/home/u/Projects/app/4913",
            "/home/u/Projects/file.iso.crdownload",
            "/elsewhere/file",
        ] {
            assert!(is_ignored(Path::new(p), root), "{p}");
        }
        assert!(!is_ignored(Path::new("/home/u/Projects/app/src/main.rs"), root));
        assert!(is_partial(Path::new("/d/x.zip.part")) && !is_partial(Path::new("/d/x.zip")));
    }

    #[test]
    fn records_coalesces_and_queries() {
        let mut log = RecentLog::new(100);
        let p = |s: &str| PathBuf::from(s);
        assert!(log.record(p("/a"), RecentKind::Created, 100));
        assert!(!log.record(p("/a"), RecentKind::Modified, 105), "edit right after creation folds in");
        assert!(log.record(p("/a"), RecentKind::Modified, 200));
        assert!(log.record(p("/b"), RecentKind::Downloaded, 150));
        assert!(log.record(p("/gone"), RecentKind::Created, 300));

        let all = log.query(None, 10, |path| path != Path::new("/gone"));
        let got: Vec<_> = all.iter().map(|i| (i.path.to_str().unwrap(), i.kind)).collect();
        assert_eq!(got, [("/a", RecentKind::Modified), ("/b", RecentKind::Downloaded)]);

        let downloads = log.query(Some(RecentKind::Downloaded), 10, |_| true);
        assert_eq!(downloads.len(), 1);
        // /a's latest event is an edit, so it isn't listed under "created".
        assert_eq!(log.query(Some(RecentKind::Created), 10, |_| true)[0].path, p("/gone"));
        log.forget(Path::new("/a"));
        assert!(log.query(None, 10, |_| true).iter().all(|i| i.path != p("/a")));
    }

    #[test]
    fn caps_and_round_trips() {
        let mut log = RecentLog::new(3);
        for i in 0..5 {
            log.record(PathBuf::from(format!("/f{i} with\ttab")), RecentKind::Created, i);
        }
        let mut buf = Vec::new();
        log.save(&mut buf).unwrap();
        let back = RecentLog::load(&buf[..], 3);
        let names: Vec<_> = back.query(None, 10, |_| true).into_iter().map(|i| i.path).collect();
        assert_eq!(names, [PathBuf::from("/f4 with\ttab"), "/f3 with\ttab".into(), "/f2 with\ttab".into()]);
        assert_eq!(RecentLog::load(&b"garbage\n1\tX\t/p\n"[..], 3).items.len(), 0);
    }
}
