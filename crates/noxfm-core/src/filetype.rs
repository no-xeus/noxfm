//! MIME detection: magic number first, extension second.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Enough for nearly every signature `infer` knows (ISO 9660 is the exception).
const SNIFF_LEN: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub mime: String,
    /// The magic number contradicts the extension at the top-level type
    /// (e.g. `photo.txt` that is really a PNG).
    pub mismatch: bool,
}

pub fn detect(path: &Path) -> Detected {
    let head = read_head(path).unwrap_or_default();
    detect_from(path, &head)
}

pub fn detect_from(path: &Path, head: &[u8]) -> Detected {
    let by_ext = mime_guess::from_path(path);
    let magic = infer::get(head).map(|t| t.mime_type().to_owned());

    match magic {
        Some(m) => {
            let mismatch = !by_ext.is_empty()
                && !by_ext.iter().any(|g| top_level(g.essence_str()) == top_level(&m));
            Detected { mime: m, mismatch }
        }
        None => {
            let mime = match by_ext.first() {
                Some(g) => g.essence_str().to_owned(),
                None if head.is_empty() => "application/x-zerosize".into(),
                None if looks_textual(head) => "text/plain".into(),
                None => "application/octet-stream".into(),
            };
            Detected { mime, mismatch: false }
        }
    }
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(SNIFF_LEN);
    File::open(path)?.take(SNIFF_LEN as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn top_level(mime: &str) -> &str {
    mime.split('/').next().unwrap_or(mime)
}

/// No NUL bytes and valid UTF-8 (allowing a cut-off last character).
pub fn looks_textual(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    // A multi-byte char may be cut at the end of the sniff window.
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn magic_wins_and_flags_mismatch() {
        let d = detect_from(Path::new("photo.txt"), PNG);
        assert_eq!(d.mime, "image/png");
        assert!(d.mismatch);
    }

    #[test]
    fn matching_extension_is_not_flagged() {
        let d = detect_from(Path::new("photo.png"), PNG);
        assert_eq!(d, Detected { mime: "image/png".into(), mismatch: false });
    }

    #[test]
    fn extension_fallback_and_text_sniff() {
        assert_eq!(detect_from(Path::new("a.rs"), b"fn main() {}").mime, "text/x-rust");
        assert_eq!(detect_from(Path::new("README"), b"hello").mime, "text/plain");
        assert_eq!(detect_from(Path::new("blob"), b"\0\x01\x02").mime, "application/octet-stream");
        assert_eq!(detect_from(Path::new("empty"), b"").mime, "application/x-zerosize");
    }
}
