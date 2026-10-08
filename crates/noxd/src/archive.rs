//! "Send to ▸ Compressed archive (.zip)" and "Extract here". Blocking; run
//! as jobs so they show progress in the transfers tab.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use noxfm_core::transfer::unique_target;
use zip::write::SimpleFileOptions;

const CHUNK: usize = 256 * 1024;

fn cancelled() -> anyhow::Error {
    anyhow::anyhow!("cancelled")
}

/// Zips `sources` (each under its own name) into `dest_zip`. On error or
/// cancel the partial archive is removed.
pub fn compress(
    sources: &[PathBuf],
    dest_zip: &Path,
    cancel: &AtomicBool,
    total: &mut dyn FnMut(u64),
    progress: &mut dyn FnMut(u64, &Path),
) -> anyhow::Result<()> {
    total(noxfm_core::transfer::total_bytes(sources, cancel)?);
    let result = write_zip(sources, dest_zip, cancel, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(dest_zip);
    }
    result
}

fn write_zip(
    sources: &[PathBuf],
    dest_zip: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, &Path),
) -> anyhow::Result<()> {
    let out = File::options().write(true).create_new(true).open(dest_zip)?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(out));
    let base = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let mut done = 0u64;
    let mut buf = vec![0u8; CHUNK];
    for src in sources {
        let root = src.parent().unwrap_or(Path::new("/"));
        for entry in walkdir::WalkDir::new(src).follow_links(false).sort_by_file_name() {
            let entry = entry?;
            let rel = entry.path().strip_prefix(root)?.to_string_lossy().into_owned();
            let mode = std::os::unix::fs::PermissionsExt::mode(&entry.metadata()?.permissions());
            let opts = base.unix_permissions(mode & 0o7777);
            let ft = entry.file_type();
            if ft.is_dir() {
                zip.add_directory(format!("{rel}/"), opts)?;
            } else if ft.is_symlink() {
                let target = std::fs::read_link(entry.path())?;
                zip.add_symlink(rel, target.to_string_lossy(), opts)?;
            } else if ft.is_file() {
                zip.start_file(rel, opts)?;
                let mut f = File::open(entry.path())?;
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(cancelled());
                    }
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    zip.write_all(&buf[..n])?;
                    done += n as u64;
                    progress(done, entry.path());
                }
            }
        }
    }
    zip.finish()?.flush()?;
    Ok(())
}

/// Extracts into a new folder named after the archive, next to it
/// (`photos.zip` -> `photos/`, or `photos (1)/`). Entries that would escape
/// the folder are refused. Returns the folder.
pub fn extract(
    zip_path: &Path,
    cancel: &AtomicBool,
    total: &mut dyn FnMut(u64),
    progress: &mut dyn FnMut(u64, &Path),
) -> anyhow::Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(io::BufReader::new(File::open(zip_path)?))?;
    let parent = zip_path.parent().ok_or_else(|| anyhow::anyhow!("archive has no folder"))?;
    let stem = zip_path.file_stem().map_or_else(|| "archive".into(), |s| s.to_string_lossy().into_owned());
    let dest = unique_target(parent, Path::new(&stem));

    let mut sum = 0u64;
    for i in 0..archive.len() {
        sum += archive.by_index(i)?.size();
    }
    total(sum);

    std::fs::create_dir(&dest)?;
    let result = (|| -> anyhow::Result<()> {
        let mut done = 0u64;
        let mut buf = vec![0u8; CHUNK];
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let rel = file.enclosed_name().ok_or_else(|| anyhow::anyhow!("unsafe path in archive: {}", file.name()))?;
            let out = dest.join(&rel);
            if file.is_dir() {
                std::fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            if file.is_symlink() {
                let mut target = String::new();
                file.read_to_string(&mut target)?;
                std::os::unix::fs::symlink(target, &out)?;
                continue;
            }
            let mut w = File::options().write(true).create_new(true).open(&out)?;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(cancelled());
                }
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n])?;
                done += n as u64;
                progress(done, &out);
            }
            if let Some(mode) = file.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o7777))?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// A free `<name>.zip` in `dir`: `photos/` -> `photos.zip`, `report.txt` ->
/// `report.zip`, several items -> `Archive.zip`.
pub fn zip_name_for(sources: &[PathBuf], dir: &Path) -> PathBuf {
    let base = match sources {
        [one] if one.is_dir() => one.file_name().map(|n| n.to_string_lossy().into_owned()),
        [one] => one.file_stem().map(|n| n.to_string_lossy().into_owned()),
        _ => None,
    }
    .unwrap_or_else(|| "Archive".into());
    unique_target(dir, Path::new(&format!("{base}.zip")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("photos");
        std::fs::create_dir_all(src.join("2024")).unwrap();
        std::fs::write(src.join("2024/a.jpg"), vec![7u8; 300_000]).unwrap();
        std::fs::write(src.join("notes.txt"), "hello").unwrap();
        std::os::unix::fs::symlink("notes.txt", src.join("link")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(src.join("notes.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();

        let zip = zip_name_for(std::slice::from_ref(&src), t.path());
        assert_eq!(zip, t.path().join("photos.zip"));
        assert_eq!(zip_name_for(&[src.join("notes.txt")], t.path()), t.path().join("notes.zip"));
        assert_eq!(zip_name_for(&[src.clone(), src.join("notes.txt")], t.path()), t.path().join("Archive.zip"));
        let no = AtomicBool::new(false);
        let (mut tot, mut last) = (0, 0);
        compress(std::slice::from_ref(&src), &zip, &no, &mut |n| tot = n, &mut |d, _| last = d).unwrap();
        assert_eq!((tot, last), (300_005, 300_005));

        // Extracting next to the source folder: "photos" exists, so "photos (1)".
        let out = extract(&zip, &no, &mut |_| {}, &mut |_, _| {}).unwrap();
        assert_eq!(out, t.path().join("photos (1)"));
        let root = out.join("photos");
        assert_eq!(std::fs::read(root.join("2024/a.jpg")).unwrap(), vec![7u8; 300_000]);
        assert_eq!(std::fs::read_link(root.join("link")).unwrap(), Path::new("notes.txt"));
        assert_eq!(std::fs::metadata(root.join("notes.txt")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn cancel_removes_partial_archive() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("big");
        std::fs::write(&f, vec![0u8; 3 * CHUNK]).unwrap();
        let zip = t.path().join("big.zip");
        let cancel = AtomicBool::new(false);
        let r = compress(std::slice::from_ref(&f), &zip, &cancel, &mut |_| {}, &mut |_, _| cancel.store(true, Ordering::Relaxed));
        assert!(r.is_err());
        assert!(!zip.exists());
    }
}
