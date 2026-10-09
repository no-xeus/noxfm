use std::collections::HashMap;
use std::io;
use std::path::Path;

use noxfm_proto::{Entry, EntryKind};
use rustix::fs::{AtFlags, CWD, FileType, Statx, StatxFlags, statx};

use crate::filetype;

const MASK: StatxFlags = StatxFlags::BASIC_STATS.union(StatxFlags::BTIME);

/// Resolves uid/gid to names once per listing.
#[derive(Default)]
struct Names {
    users: HashMap<u32, Option<String>>,
    groups: HashMap<u32, Option<String>>,
}

impl Names {
    fn user(&mut self, uid: u32) -> Option<String> {
        self.users
            .entry(uid)
            .or_insert_with(|| uzers::get_user_by_uid(uid).map(|u| u.name().to_string_lossy().into_owned()))
            .clone()
    }

    fn group(&mut self, gid: u32) -> Option<String> {
        self.groups
            .entry(gid)
            .or_insert_with(|| uzers::get_group_by_gid(gid).map(|g| g.name().to_string_lossy().into_owned()))
            .clone()
    }
}

/// Lists `dir` sorted directories-first, then by case-insensitive name.
/// Entries that vanish mid-listing are skipped.
pub fn list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut names = Names::default();
    let mut out = Vec::new();
    for dent in std::fs::read_dir(dir)? {
        let Ok(dent) = dent else { continue };
        if let Ok(e) = build(&dent.path(), &mut names) {
            out.push(e);
        }
    }
    sort(&mut out);
    Ok(out)
}

pub fn stat_entry(path: &Path) -> io::Result<Entry> {
    build(path, &mut Names::default())
}

pub fn sort(entries: &mut [Entry]) {
    sort_by(entries, SortKey::Name, true);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    Extension,
    Size,
    Modified,
    Created,
    Owner,
    Permissions,
}

/// Directories always come first; ties fall back to case-insensitive name.
/// Unknown values (no size yet, no btime) sort last in both directions.
pub fn sort_by(entries: &mut [Entry], key: SortKey, ascending: bool) {
    entries.sort_by(|a, b| compare(a, b, key, ascending));
}

/// The order of [`sort_by`], for one pair.
pub fn compare(a: &Entry, b: &Entry, key: SortKey, ascending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    fn opt<T: Ord>(a: Option<T>, b: Option<T>, asc: bool) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) if asc => a.cmp(&b),
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let dir = |o: Ordering| if ascending { o } else { o.reverse() };

    let dirs_first = (a.kind != EntryKind::Dir).cmp(&(b.kind != EntryKind::Dir));
    let name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
    let by_key = match key {
        SortKey::Name => dir(name()),
        SortKey::Extension => dir(a.extension().map(str::to_lowercase).cmp(&b.extension().map(str::to_lowercase))),
        SortKey::Size => opt(a.size, b.size, ascending),
        SortKey::Modified => opt(a.modified, b.modified, ascending),
        SortKey::Created => opt(a.created, b.created, ascending),
        SortKey::Owner => dir(a.owner.cmp(&b.owner)),
        SortKey::Permissions => dir((a.mode & 0o7777).cmp(&(b.mode & 0o7777))),
    };
    dirs_first.then(by_key).then_with(name)
}

fn build(path: &Path, names: &mut Names) -> io::Result<Entry> {
    let lst = statx(CWD, path, AtFlags::SYMLINK_NOFOLLOW, MASK)?;
    let symlink = FileType::from_raw_mode(lst.stx_mode.into()) == FileType::Symlink;
    let (st, kind) = if symlink {
        match statx(CWD, path, AtFlags::empty(), MASK) {
            Ok(t) => {
                let k = kind_of(&t);
                (t, k)
            }
            Err(_) => (lst, EntryKind::BrokenLink),
        }
    } else {
        let k = kind_of(&lst);
        (lst, k)
    };

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let (mime, mime_mismatch) = match kind {
        EntryKind::File => {
            let d = filetype::detect(path);
            (Some(d.mime), d.mismatch)
        }
        EntryKind::Dir => (Some("inode/directory".into()), false),
        _ => (None, false),
    };

    let has_btime = StatxFlags::from_bits_retain(st.stx_mask).contains(StatxFlags::BTIME);
    Ok(Entry {
        hidden: name.starts_with('.'),
        is_git: kind == EntryKind::Dir && path.join(".git").exists(),
        app: None,
        size: (kind == EntryKind::File).then_some(st.stx_size),
        created: has_btime.then_some(st.stx_btime.tv_sec),
        modified: Some(st.stx_mtime.tv_sec),
        uid: st.stx_uid,
        gid: st.stx_gid,
        owner: names.user(st.stx_uid),
        group: names.group(st.stx_gid),
        mode: st.stx_mode.into(),
        path: path.to_path_buf(),
        name,
        kind,
        symlink,
        mime,
        mime_mismatch,
    })
}

fn kind_of(st: &Statx) -> EntryKind {
    match FileType::from_raw_mode(st.stx_mode.into()) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        _ => EntryKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_with_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("b.txt"), "hi").unwrap();
        std::fs::create_dir(root.join("A")).unwrap();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::write(root.join(".hidden"), "").unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
        std::os::unix::fs::symlink("A", root.join("linkdir")).unwrap();

        let es = list_dir(root).unwrap();
        let names: Vec<_> = es.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A", "linkdir", "repo", ".hidden", "b.txt", "dangling"]);

        let by = |n: &str| es.iter().find(|e| e.name == n).unwrap();
        assert_eq!(by("b.txt").size, Some(2));
        assert_eq!(by("b.txt").mime.as_deref(), Some("text/plain"));
        assert_eq!(by("b.txt").extension(), Some("txt"));
        assert!(by("b.txt").modified.is_some());
        assert!(by("repo").is_git && !by("A").is_git);
        assert!(by(".hidden").hidden);
        assert_eq!(by("dangling").kind, EntryKind::BrokenLink);
        assert!(by("linkdir").symlink && by("linkdir").kind == EntryKind::Dir);
    }

    #[test]
    fn sort_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("big.txt"), [0u8; 100]).unwrap();
        std::fs::write(root.join("small.rs"), [0u8; 1]).unwrap();
        std::fs::write(root.join("mid.md"), [0u8; 10]).unwrap();
        std::fs::create_dir(root.join("zdir")).unwrap();
        let mut es = list_dir(root).unwrap();
        let names = |es: &[Entry]| es.iter().map(|e| e.name.clone()).collect::<Vec<_>>();

        sort_by(&mut es, SortKey::Size, false);
        assert_eq!(names(&es), ["zdir", "big.txt", "mid.md", "small.rs"]);
        sort_by(&mut es, SortKey::Size, true);
        assert_eq!(names(&es), ["zdir", "small.rs", "mid.md", "big.txt"]);
        sort_by(&mut es, SortKey::Extension, true);
        assert_eq!(names(&es), ["zdir", "mid.md", "small.rs", "big.txt"]);
        sort_by(&mut es, SortKey::Name, false);
        assert_eq!(names(&es), ["zdir", "small.rs", "mid.md", "big.txt"]);
    }
}
