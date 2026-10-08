//! Single-shot file operations behind the context menus: rename, new
//! items, links, permissions, properties, terminal, sidebar pins.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use noxfm_core::transfer::{unique_target, validate_name};
use noxfm_proto::{GitInfo, NewKind, Props};

/// Renames in place. Refuses to overwrite.
pub fn rename(path: &Path, new_name: &str) -> anyhow::Result<PathBuf> {
    validate_name(new_name).map_err(|e| anyhow::anyhow!(e))?;
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("can't rename the root"))?;
    let to = parent.join(new_name);
    if to == path {
        return Ok(to);
    }
    // A case-only rename on a case-insensitive fs reports the target as existing.
    let same_file = to.symlink_metadata().is_ok_and(|_| same_inode(path, &to));
    anyhow::ensure!(to.symlink_metadata().is_err() || same_file, "“{new_name}” already exists");
    std::fs::rename(path, &to)?;
    Ok(to)
}

fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.symlink_metadata(), b.symlink_metadata()) {
        (Ok(x), Ok(y)) => x.ino() == y.ino() && x.dev() == y.dev(),
        _ => false,
    }
}

pub fn create(dir: &Path, kind: &NewKind) -> anyhow::Result<PathBuf> {
    let (name, template): (String, Option<&Path>) = match kind {
        NewKind::Folder => ("New folder".into(), None),
        NewKind::EmptyFile => ("New file".into(), None),
        NewKind::TextDocument => ("New document.txt".into(), None),
        NewKind::Template(t) => (t.file_name().map_or_else(|| "New file".into(), |n| n.to_string_lossy().into_owned()), Some(t)),
    };
    let path = unique_target(dir, Path::new(&name));
    match (kind, template) {
        (NewKind::Folder, _) => std::fs::create_dir(&path)?,
        (_, Some(t)) => {
            std::fs::copy(t, &path)?;
        }
        _ => {
            std::fs::File::options().write(true).create_new(true).open(&path)?;
        }
    }
    Ok(path)
}

/// `dir/<name> (link)` pointing at each target (absolute paths).
pub fn symlink(targets: &[PathBuf], dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    targets
        .iter()
        .map(|t| {
            let name = t.file_name().map_or_else(|| "link".into(), |n| n.to_string_lossy().into_owned());
            let link = unique_target(dir, Path::new(&format!("{name} (link)")));
            std::os::unix::fs::symlink(t, &link)?;
            Ok(link)
        })
        .collect()
}

pub fn chmod(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))?;
    Ok(())
}

/// Totals and details for the Properties panel. Blocking: walks folders.
pub fn properties(paths: &[PathBuf]) -> anyhow::Result<Props> {
    let mut props = Props { paths: paths.to_vec(), ..Default::default() };
    if let [one] = paths {
        props.entry = Some(noxfm_core::stat_entry(one)?);
        props.accessed = rustix::fs::statx(rustix::fs::CWD, one, rustix::fs::AtFlags::empty(), rustix::fs::StatxFlags::ATIME)
            .ok()
            .map(|s| s.stx_atime.tv_sec);
        props.fs = noxfm_core::fskind::FsKind::of(one).ok().map(|k| k.name().to_owned());
        props.ext_mime = mime_guess::from_path(one).first().map(|m| m.essence_str().to_owned());
        if one.is_dir() {
            props.git = git_info(one);
        }
    }
    for p in paths {
        for entry in walkdir::WalkDir::new(p).follow_links(false).same_file_system(true) {
            let Ok(entry) = entry else {
                props.partial = true;
                continue;
            };
            // The selected items themselves don't count as "contents".
            if entry.depth() == 0 && entry.file_type().is_dir() {
                continue;
            }
            if entry.file_type().is_dir() {
                props.folders += 1;
            } else {
                props.files += 1;
                if entry.file_type().is_file() {
                    props.bytes += entry.metadata().map_or(0, |m| m.len());
                }
            }
        }
    }
    Ok(props)
}

/// Branch and dirty state, if `dir` is inside a git work tree. Gives up
/// after a couple of seconds (huge repos, network filesystems).
fn git_info(dir: &Path) -> Option<GitInfo> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain=v2", "--branch", "--untracked-files=normal"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if started.elapsed() > Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout?, &mut out).ok()?;
    Some(parse_git_status(&out))
}

fn parse_git_status(out: &str) -> GitInfo {
    let branch = out
        .lines()
        .find_map(|l| l.strip_prefix("# branch.head "))
        .filter(|b| *b != "(detached)")
        .map(str::to_owned);
    let dirty = out.lines().any(|l| !l.starts_with('#'));
    GitInfo { branch, dirty }
}

/// A terminal in `dir`, detached from noxd.
pub fn open_terminal(dir: &Path) -> anyhow::Result<()> {
    let env = std::env::var("TERMINAL").ok();
    let mut argv = noxfm_core::apps::terminal_prefix(noxfm_core::apps::on_path, env.as_deref())
        .ok_or_else(|| anyhow::anyhow!("no terminal emulator found"))?;
    // The prefix ends in "run this:" (`-e`, `--`); with no program, drop it.
    while argv.len() > 1 && matches!(argv.last().map(String::as_str), Some("-e" | "--")) {
        argv.pop();
    }
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// Sidebar pins: `~/.config/noxfm/pins`, one path per line.
pub struct Pins {
    file: PathBuf,
}

impl Pins {
    pub fn for_user() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".config"));
        Pins { file: config.join("noxfm/pins") }
    }

    pub fn at(file: PathBuf) -> Self {
        Pins { file }
    }

    pub fn list(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(&self.file)
            .map(|t| t.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    pub fn set(&self, path: &Path, pinned: bool) -> anyhow::Result<()> {
        let mut pins = self.list();
        pins.retain(|p| p != path);
        if pinned {
            pins.push(path.to_path_buf());
        }
        std::fs::create_dir_all(self.file.parent().expect("has parent"))?;
        let text: String = pins.iter().map(|p| format!("{}\n", p.display())).collect();
        std::fs::write(&self.file, text)?;
        Ok(())
    }
}

/// Sidebar sections and disks the user folded (`~/.config/noxfm/sidebar-collapsed`,
/// one key per line).
pub struct CollapsedItems {
    file: PathBuf,
}

impl CollapsedItems {
    pub fn for_user() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".config"));
        CollapsedItems { file: config.join("noxfm/sidebar-collapsed") }
    }

    pub fn at(file: PathBuf) -> Self {
        CollapsedItems { file }
    }

    pub fn list(&self) -> Vec<String> {
        std::fs::read_to_string(&self.file)
            .map(|t| t.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default()
    }

    pub fn set(&self, key: &str, collapsed: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!key.contains('\n'), "bad key");
        let mut keys = self.list();
        keys.retain(|k| k != key);
        if collapsed {
            keys.push(key.to_owned());
        }
        std::fs::create_dir_all(self.file.parent().expect("has parent"))?;
        std::fs::write(&self.file, keys.iter().map(|k| format!("{k}\n")).collect::<String>())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_rules() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        std::fs::write(&a, "").unwrap();
        std::fs::write(t.path().join("taken"), "").unwrap();
        assert!(rename(&a, "taken").is_err());
        assert!(rename(&a, "x/y").is_err());
        assert!(rename(&a, "").is_err());
        assert_eq!(rename(&a, "b").unwrap(), t.path().join("b"));
        assert!(!a.exists());
    }

    #[test]
    fn creates_unique_items_and_links() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(create(d, &NewKind::Folder).unwrap(), d.join("New folder"));
        assert_eq!(create(d, &NewKind::Folder).unwrap(), d.join("New folder (1)"));
        assert_eq!(create(d, &NewKind::TextDocument).unwrap(), d.join("New document.txt"));
        let tpl = d.join("Letter.odt");
        std::fs::write(&tpl, "template").unwrap();
        let sub = d.join("New folder");
        let made = create(&sub, &NewKind::Template(tpl.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(made).unwrap(), "template");

        let links = symlink(std::slice::from_ref(&tpl), d).unwrap();
        assert_eq!(links, [d.join("Letter.odt (link)")]);
        assert_eq!(std::fs::read_link(&links[0]).unwrap(), tpl);
    }

    #[test]
    fn properties_totals() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        std::fs::create_dir_all(d.join("x/y")).unwrap();
        std::fs::write(d.join("x/f1"), [0u8; 10]).unwrap();
        std::fs::write(d.join("x/y/f2"), [0u8; 5]).unwrap();
        let p = properties(std::slice::from_ref(&d)).unwrap();
        assert_eq!((p.files, p.folders, p.bytes), (2, 2, 15));
        assert!(p.entry.is_some() && p.accessed.is_some());
        let multi = properties(&[d.join("x/f1"), d.join("x/y")]).unwrap();
        assert_eq!((multi.files, multi.folders, multi.bytes), (2, 0, 15));
        assert!(multi.entry.is_none());
    }

    #[test]
    fn git_status_parsing() {
        let clean = "# branch.oid abc\n# branch.head main\n";
        assert_eq!(parse_git_status(clean), GitInfo { branch: Some("main".into()), dirty: false });
        let dirty = "# branch.head (detached)\n1 .M N... 100644 100644 100644 a b src/x.rs\n";
        assert_eq!(parse_git_status(dirty), GitInfo { branch: None, dirty: true });
    }

    #[test]
    fn collapsed_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let c = CollapsedItems::at(t.path().join("cfg/collapsed"));
        c.set("disk:/org/x", true).unwrap();
        c.set("section:places", true).unwrap();
        c.set("disk:/org/x", false).unwrap();
        assert_eq!(c.list(), ["section:places"]);
    }

    #[test]
    fn pins_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let pins = Pins::at(t.path().join("cfg/pins"));
        pins.set(Path::new("/a b"), true).unwrap();
        pins.set(Path::new("/c"), true).unwrap();
        pins.set(Path::new("/a b"), false).unwrap();
        assert_eq!(pins.list(), [PathBuf::from("/c")]);
    }
}
