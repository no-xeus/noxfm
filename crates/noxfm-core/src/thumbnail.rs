//! Thumbnails per the freedesktop Thumbnail Managing Standard, shared with
//! other apps through `~/.cache/thumbnails`.
//!
//! - A thumbnail is `<cache>/<size>/<md5 of the file URI>.png`.
//! - It is valid while its `Thumb::MTime` tag equals the file's mtime.
//! - New thumbnails are written to a temp file and renamed into place.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use md5::{Digest, Md5};

use crate::uri;

/// Images bigger than this aren't decoded just for a thumbnail.
const MAX_IMAGE_BYTES: u64 = 256 << 20;
const VIDEO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 128 px
    Normal,
    /// 256 px
    Large,
}

impl Size {
    fn px(self) -> u32 {
        match self {
            Size::Normal => 128,
            Size::Large => 256,
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Size::Normal => "normal",
            Size::Large => "large",
        }
    }
}

/// Which files we can thumbnail, by MIME type.
pub fn supported(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff" | "image/x-icon"
            | "image/vnd.microsoft.icon"
    ) || mime.starts_with("video/")
}

pub fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::complete::home_dir().join(".cache"))
        .join("thumbnails")
}

pub fn cache_path(root: &Path, file: &Path, size: Size) -> PathBuf {
    let hash = Md5::digest(uri::to_uri(file).as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    root.join(size.dir()).join(format!("{hex}.png"))
}

/// Returns a valid thumbnail for `file`, generating it if needed.
pub fn get_or_create(root: &Path, file: &Path, mime: &str, size: Size) -> io::Result<PathBuf> {
    let meta = fs::metadata(file)?;
    let mtime = meta.modified()?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let out = cache_path(root, file, size);
    if stored_mtime(&out) == Some(mtime) {
        return Ok(out);
    }

    let img = if mime.starts_with("video/") {
        video_frame(file)?
    } else {
        if meta.len() > MAX_IMAGE_BYTES {
            return Err(io::Error::other("image too large to thumbnail"));
        }
        image::ImageReader::open(file)?.with_guessed_format()?.decode().map_err(io::Error::other)?
    };
    let thumb = img.thumbnail(size.px(), size.px()).to_rgba8();

    let dir = out.parent().expect("cache path has a parent");
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".noxfm-{}-{}.png", std::process::id(), rand_suffix()));
    let written = write_png(&tmp, &thumb, &uri::to_uri(file), mtime, meta.len());
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, &out)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(out)
}

fn stored_mtime(thumb: &Path) -> Option<u64> {
    let reader = png::Decoder::new(BufReader::new(File::open(thumb).ok()?)).read_info().ok()?;
    let info = reader.info();
    info.uncompressed_latin1_text.iter().find(|t| t.keyword == "Thumb::MTime")?.text.parse().ok()
}

fn write_png(path: &Path, img: &image::RgbaImage, uri: &str, mtime: u64, size: u64) -> io::Result<()> {
    let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let text = |k: &str, v: String| (k.to_owned(), v);
    for (k, v) in [
        text("Thumb::URI", uri.to_owned()),
        text("Thumb::MTime", mtime.to_string()),
        text("Thumb::Size", size.to_string()),
        text("Software", "noxfm".into()),
    ] {
        enc.add_text_chunk(k, v).map_err(io::Error::other)?;
    }
    let mut w = enc.write_header().map_err(io::Error::other)?;
    w.write_image_data(img.as_raw()).map_err(io::Error::other)?;
    w.finish().map_err(io::Error::other)
}

/// First frame via ffmpeg, which must be on `$PATH`.
fn video_frame(file: &Path) -> io::Result<image::DynamicImage> {
    let tmp = std::env::temp_dir().join(format!("noxfm-frame-{}-{}.png", std::process::id(), rand_suffix()));
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-vf", "scale=512:-2", "-f", "image2", "-c:v", "png"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if started.elapsed() > VIDEO_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&tmp);
            return Err(io::Error::new(io::ErrorKind::TimedOut, "ffmpeg took too long"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let img = if status.success() {
        image::open(&tmp).map_err(io::Error::other)
    } else {
        Err(io::Error::other(format!("ffmpeg failed: {status}")))
    };
    let _ = fs::remove_file(&tmp);
    img
}

/// Unique within this process; combined with the pid in temp file names.
fn rand_suffix() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_matches_spec_example() {
        // Hash of "file:///home/jens/photos/me.png" as given in the spec.
        let p = cache_path(Path::new("/c"), Path::new("/home/jens/photos/me.png"), Size::Normal);
        assert_eq!(p, Path::new("/c/normal/c6ee772d9e49320e97ec29a7eb5b1697.png"));
    }

    #[test]
    fn creates_reuses_and_refreshes() {
        let t = tempfile::tempdir().unwrap();
        let (root, img) = (t.path().join("cache"), t.path().join("big.png"));
        image::RgbaImage::from_pixel(400, 200, image::Rgba([255, 0, 0, 255])).save(&img).unwrap();

        let thumb = get_or_create(&root, &img, "image/png", Size::Normal).unwrap();
        let decoded = image::open(&thumb).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (128, 64), "aspect ratio kept");
        let mtime = fs::metadata(&img).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(stored_mtime(&thumb), Some(mtime));

        // Valid cache entry: not rewritten.
        let before = fs::metadata(&thumb).unwrap().modified().unwrap();
        get_or_create(&root, &img, "image/png", Size::Normal).unwrap();
        assert_eq!(fs::metadata(&thumb).unwrap().modified().unwrap(), before);

        // Source changed: regenerated with the new mtime.
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        File::options().write(true).open(&img).unwrap().set_modified(later).unwrap();
        get_or_create(&root, &img, "image/png", Size::Normal).unwrap();
        assert_ne!(stored_mtime(&thumb), Some(mtime));

        assert!(get_or_create(&root, &t.path().join("missing.png"), "image/png", Size::Normal).is_err());
        let leftovers = fs::read_dir(root.join("normal")).unwrap().count();
        assert_eq!(leftovers, 1, "no temp files left behind");
    }

    #[test]
    fn video_first_frame() {
        let t = tempfile::tempdir().unwrap();
        let clip = t.path().join("clip.mp4");
        let made = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10", "-t", "1"])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipped: ffmpeg unavailable");
            return;
        }
        let thumb = get_or_create(&t.path().join("cache"), &clip, "video/mp4", Size::Large).unwrap();
        let img = image::open(thumb).unwrap();
        assert_eq!((img.width(), img.height()), (256, 192));
    }
}
