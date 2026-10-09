//! Copy/move engine. Blocking; the daemon runs it on a worker thread.
//!
//! - A move on one filesystem is a `rename`. Across filesystems it is a
//!   copy, then the source is deleted.
//! - File copies try a reflink (`FICLONE`: instant and space-sharing on
//!   btrfs/xfs) before falling back to streaming the bytes.
//! - Name clashes never overwrite: the target becomes `name (1).ext`, and so on.
//! - Mode bits and mtime are preserved; symlinks are recreated, not followed.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use noxfm_proto::TransferOp;

const CHUNK: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("cancelled")]
    Cancelled,
    #[error("can't put “{0}” inside itself")]
    IntoItself(PathBuf),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

trait Ctx<T> {
    fn at(self, path: &Path) -> Result<T, TransferError>;
}

impl<T> Ctx<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T, TransferError> {
        self.map_err(|source| TransferError::Io { path: path.to_path_buf(), source })
    }
}

/// Bytes `run` will report in total for these sources.
pub fn total_bytes(sources: &[PathBuf], cancel: &AtomicBool) -> Result<u64, TransferError> {
    let mut total = 0;
    for src in sources {
        for entry in walkdir::WalkDir::new(src).follow_links(false) {
            if cancel.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }
            let entry = entry.map_err(|e| {
                let path = e.path().unwrap_or(src).to_path_buf();
                TransferError::Io { path, source: e.into() }
            })?;
            if entry.file_type().is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(total)
}

/// `on_progress(done_bytes, current_file)` is called after every chunk.
/// Returns each top-level `(source, target)` actually transferred (sources
/// already in `dest` are skipped for moves), which is what undo needs.
pub fn run(
    op: TransferOp,
    sources: &[PathBuf],
    dest: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, &Path),
) -> Result<Vec<(PathBuf, PathBuf)>, TransferError> {
    if !dest.is_dir() {
        return Err(TransferError::Io {
            path: dest.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotADirectory, "destination is not a directory"),
        });
    }
    let mut job = Job { cancel, on_progress, done: 0 };
    let mut done = Vec::with_capacity(sources.len());
    for src in sources {
        let meta = fs::symlink_metadata(src).at(src)?;
        if meta.is_dir() && dest.starts_with(src) {
            return Err(TransferError::IntoItself(src.clone()));
        }
        if op == TransferOp::Move && src.parent() == Some(dest) {
            continue; // already there
        }
        let name = src.file_name().ok_or_else(|| TransferError::IntoItself(src.clone()))?;
        let target = unique_target(dest, Path::new(name));
        match op {
            TransferOp::Copy => job.copy_tree(src, &target)?,
            TransferOp::Move => job.move_tree(src, &target)?,
        }
        done.push((src.clone(), target));
    }
    Ok(done)
}

/// Checks a new file name typed by the user.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    match name {
        "" => Err("The name can't be empty"),
        "." | ".." => Err("“.” and “..” are reserved"),
        n if n.contains('/') => Err("Names can't contain “/”"),
        n if n.contains('\0') => Err("Names can't contain NUL"),
        n if n.len() > 255 => Err("The name is too long"),
        _ => Ok(()),
    }
}

/// `dest/name`, or `dest/name (n).ext` for the first `n` that is free.
pub fn unique_target(dest: &Path, name: &Path) -> PathBuf {
    let first = dest.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let name = name.to_string_lossy();
    // Only treat the last dot as an extension separator if it isn't leading ("".bashrc").
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (&name[..], ""),
    };
    (1..)
        .map(|n| dest.join(format!("{stem} ({n}){ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .expect("unbounded")
}

struct Job<'a> {
    cancel: &'a AtomicBool,
    on_progress: &'a mut dyn FnMut(u64, &Path),
    done: u64,
}

impl Job<'_> {
    fn check(&self) -> Result<(), TransferError> {
        if self.cancel.load(Ordering::Relaxed) { Err(TransferError::Cancelled) } else { Ok(()) }
    }

    fn move_tree(&mut self, src: &Path, target: &Path) -> Result<(), TransferError> {
        self.check()?;
        match fs::rename(src, target) {
            Ok(()) => {
                let bytes = total_bytes(&[target.to_path_buf()], &AtomicBool::new(false)).unwrap_or(0);
                self.done += bytes;
                (self.on_progress)(self.done, target);
                Ok(())
            }
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                self.copy_tree(src, target)?;
                let meta = fs::symlink_metadata(src).at(src)?;
                if meta.is_dir() { fs::remove_dir_all(src) } else { fs::remove_file(src) }.at(src)
            }
            Err(e) => Err(e).at(src),
        }
    }

    fn copy_tree(&mut self, src: &Path, target: &Path) -> Result<(), TransferError> {
        self.check()?;
        let meta = fs::symlink_metadata(src).at(src)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let link = fs::read_link(src).at(src)?;
            std::os::unix::fs::symlink(link, target).at(target)?;
        } else if ft.is_dir() {
            fs::create_dir(target).at(target)?;
            let mut children: Vec<_> = fs::read_dir(src).at(src)?.collect::<Result<_, _>>().at(src)?;
            children.sort_by_key(|d| d.file_name());
            for child in children {
                self.copy_tree(&child.path(), &target.join(child.file_name()))?;
            }
            copy_attrs(&meta, target)?;
        } else if ft.is_file() {
            self.copy_file(src, target, &meta)?;
        }
        // Sockets, fifos and devices are skipped.
        Ok(())
    }

    fn copy_file(&mut self, src: &Path, target: &Path, meta: &fs::Metadata) -> Result<(), TransferError> {
        let mut input = File::open(src).at(src)?;
        let mut output = File::options().write(true).create_new(true).open(target).at(target)?;

        let result = if rustix::fs::ioctl_ficlone(&output, &input).is_ok() {
            self.done += meta.len();
            (self.on_progress)(self.done, src);
            // Same as `stream`, which checks once more before seeing EOF.
            self.check()
        } else {
            self.stream(&mut input, &mut output, src, target)
        };
        if let Err(e) = result {
            drop(output);
            let _ = fs::remove_file(target);
            return Err(e);
        }
        drop(output);
        copy_attrs(meta, target)
    }

    fn stream(&mut self, input: &mut File, output: &mut File, src: &Path, target: &Path) -> Result<(), TransferError> {
        let mut buf = vec![0u8; CHUNK];
        loop {
            self.check()?;
            let n = input.read(&mut buf).at(src)?;
            if n == 0 {
                return Ok(());
            }
            output.write_all(&buf[..n]).at(target)?;
            self.done += n as u64;
            (self.on_progress)(self.done, src);
        }
    }
}

fn copy_attrs(meta: &fs::Metadata, target: &Path) -> Result<(), TransferError> {
    fs::set_permissions(target, fs::Permissions::from_mode(meta.mode() & 0o7777)).at(target)?;
    let mtime = meta.modified().at(target)?;
    // Opening read-only is enough for futimens on files we own.
    File::open(target).and_then(|f| f.set_modified(mtime)).at(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{Duration, SystemTime};

    type Outcome = (Result<Vec<(PathBuf, PathBuf)>, TransferError>, Vec<u64>);

    fn go(op: TransferOp, srcs: &[PathBuf], dest: &Path) -> Outcome {
        let mut seen = Vec::new();
        let r = run(op, srcs, dest, &AtomicBool::new(false), &mut |d, _| seen.push(d));
        (r, seen)
    }

    #[test]
    fn copies_tree_with_attrs_and_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("src"), t.path().join("dst"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src.join("sub/a.sh"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        fs::set_permissions(src.join("sub/a.sh"), fs::Permissions::from_mode(0o751)).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::open(src.join("sub/a.sh")).unwrap().set_modified(old).unwrap();
        symlink("sub/a.sh", src.join("link")).unwrap();

        let total = total_bytes(std::slice::from_ref(&src), &AtomicBool::new(false)).unwrap();
        let (r, seen) = go(TransferOp::Copy, std::slice::from_ref(&src), &dst);
        r.unwrap();

        let copied = dst.join("src/sub/a.sh");
        assert_eq!(fs::read(&copied).unwrap(), fs::read(src.join("sub/a.sh")).unwrap());
        let m = fs::metadata(&copied).unwrap();
        assert_eq!(m.mode() & 0o7777, 0o751);
        assert_eq!(m.modified().unwrap(), old);
        assert_eq!(fs::read_link(dst.join("src/link")).unwrap(), Path::new("sub/a.sh"));
        assert_eq!(seen.last(), Some(&total));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress must be monotonic");
        assert!(src.exists(), "copy keeps the source");
    }

    #[test]
    fn names() {
        assert!(validate_name("ok name.txt").is_ok());
        for bad in ["", ".", "..", "a/b", &"x".repeat(256)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn never_overwrites() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("a.txt");
        fs::write(&f, "one").unwrap();
        go(TransferOp::Copy, std::slice::from_ref(&f), t.path()).0.unwrap();
        let pairs = go(TransferOp::Copy, std::slice::from_ref(&f), t.path()).0.unwrap();
        assert_eq!(pairs, [(f.clone(), t.path().join("a (2).txt"))], "reports the name actually used");
        assert_eq!(fs::read_to_string(t.path().join("a (1).txt")).unwrap(), "one");
        assert!(t.path().join("a (2).txt").exists());
        assert_eq!(unique_target(t.path(), Path::new(".bashrc")), t.path().join(".bashrc"));
        fs::write(t.path().join(".bashrc"), "").unwrap();
        assert_eq!(unique_target(t.path(), Path::new(".bashrc")), t.path().join(".bashrc (1)"));
    }

    #[test]
    fn move_is_rename_on_same_fs() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fs::write(&f, "data").unwrap();
        let ino = fs::metadata(&f).unwrap().ino();
        let dst = t.path().join("d");
        fs::create_dir(&dst).unwrap();
        let (r, seen) = go(TransferOp::Move, std::slice::from_ref(&f), &dst);
        r.unwrap();
        assert!(!f.exists());
        assert_eq!(fs::metadata(dst.join("f")).unwrap().ino(), ino);
        assert_eq!(seen, [4]);
    }

    /// Under `target/`, which is on the same filesystem as the checkout
    /// (btrfs on the dev box), unlike the tmpfs used by the other tests.
    fn on_target_fs() -> tempfile::TempDir {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
        fs::create_dir_all(&dir).unwrap();
        tempfile::tempdir_in(dir).unwrap()
    }

    #[test]
    fn reflinks_where_supported() {
        let t = on_target_fs();
        if !crate::fskind::FsKind::of(t.path()).unwrap().supports_reflink() {
            eprintln!("skipped: no reflink support here");
            return;
        }
        let f = t.path().join("big");
        fs::write(&f, vec![1u8; 8 * CHUNK]).unwrap();
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let (r, seen) = go(TransferOp::Copy, std::slice::from_ref(&f), &out);
        r.unwrap();
        assert_eq!(seen, [8 * CHUNK as u64], "a reflink reports the whole file in one step");
        assert_eq!(fs::read(out.join("big")).unwrap(), fs::read(&f).unwrap());
    }

    #[test]
    fn move_across_filesystems_copies_then_deletes() {
        let tmpfs = tempfile::tempdir().unwrap();
        let other = on_target_fs();
        if fs::metadata(tmpfs.path()).unwrap().dev() == fs::metadata(other.path()).unwrap().dev() {
            eprintln!("skipped: need two filesystems");
            return;
        }
        let d = tmpfs.path().join("dir");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("x"), "hello").unwrap();
        go(TransferOp::Move, std::slice::from_ref(&d), other.path()).0.unwrap();
        assert!(!d.exists());
        assert_eq!(fs::read_to_string(other.path().join("dir/x")).unwrap(), "hello");
    }

    #[test]
    fn refuses_into_itself_and_honours_cancel() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(d.join("inner")).unwrap();
        assert!(matches!(go(TransferOp::Copy, std::slice::from_ref(&d), &d.join("inner")).0, Err(TransferError::IntoItself(_))));

        fs::write(d.join("big"), vec![0u8; 4 * CHUNK]).unwrap();
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let cancel = AtomicBool::new(false);
        let r = run(TransferOp::Copy, &[d.join("big")], &out, &cancel, &mut |_, _| {
            cancel.store(true, Ordering::Relaxed)
        });
        assert!(matches!(r, Err(TransferError::Cancelled)), "{r:?}");
        assert!(!out.join("big").exists(), "partial file is removed");
    }
}
