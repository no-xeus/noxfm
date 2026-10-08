//! `file://` URIs and the clipboard / drag-and-drop formats built on them.
//!
//! - `text/uri-list` (RFC 2483): one URI per line, CRLF, `#` lines are comments.
//! - `x-special/gnome-copied-files`: first line `copy` or `cut`, then one URI
//!   per line. Nautilus, Thunar, Dolphin and cosmic-files read it for cut/paste.

use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use noxfm_proto::TransferOp;

pub const URI_LIST: &str = "text/uri-list";
pub const GNOME_COPIED: &str = "x-special/gnome-copied-files";
pub const PLAIN: &str = "text/plain;charset=utf-8";

pub fn to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Accepts `file:///p` and `file://localhost/p`; other hosts and schemes are rejected.
pub fn from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

pub fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| to_uri(p) + "\r\n").collect()
}

pub fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(from_uri)
        .collect()
}

pub fn gnome_copied(op: TransferOp, paths: &[PathBuf]) -> String {
    let verb = match op {
        TransferOp::Copy => "copy",
        TransferOp::Move => "cut",
    };
    std::iter::once(verb.to_owned()).chain(paths.iter().map(|p| to_uri(p))).collect::<Vec<_>>().join("\n")
}

pub fn parse_gnome_copied(data: &[u8]) -> Option<(TransferOp, Vec<PathBuf>)> {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.lines();
    let op = match lines.next()?.trim() {
        "copy" => TransferOp::Copy,
        "cut" => TransferOp::Move,
        _ => return None,
    };
    let paths: Vec<_> = lines.filter_map(|l| from_uri(l.trim())).collect();
    (!paths.is_empty()).then_some((op, paths))
}

/// Newline-separated paths, for pasting into terminals and editors.
pub fn plain(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.as_os_str()).map(OsStr::to_string_lossy).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_odd_names() {
        for p in ["/home/me/My Files/été #1 100%.txt", "/a/b", "/x/[weird]?name"] {
            let uri = to_uri(Path::new(p));
            assert!(!uri.contains(' ') && !uri.contains('#') && !uri.contains('?'), "{uri}");
            assert_eq!(from_uri(&uri).unwrap(), Path::new(p));
        }
        let raw = PathBuf::from(std::ffi::OsString::from_vec(b"/bad\xffbyte".to_vec()));
        assert_eq!(from_uri(&to_uri(&raw)).unwrap(), raw);
        assert_eq!(to_uri(Path::new("/a b")), "file:///a%20b");
    }

    #[test]
    fn rejects_foreign_uris() {
        assert_eq!(from_uri("https://x/y"), None);
        assert_eq!(from_uri("file://otherhost/y"), None);
        assert_eq!(from_uri("file:///bad%2"), None);
        assert_eq!(from_uri("file://localhost/ok").unwrap(), Path::new("/ok"));
    }

    #[test]
    fn clipboard_formats() {
        let ps = vec![PathBuf::from("/a b"), PathBuf::from("/c")];
        assert_eq!(parse_uri_list(uri_list(&ps).as_bytes()), ps);
        assert_eq!(parse_uri_list(b"# comment\r\nfile:///x\r\n\r\n"), [PathBuf::from("/x")]);
        let g = gnome_copied(TransferOp::Move, &ps);
        assert_eq!(g, "cut\nfile:///a%20b\nfile:///c");
        assert_eq!(parse_gnome_copied(g.as_bytes()), Some((TransferOp::Move, ps)));
        assert_eq!(parse_gnome_copied(b"copy\n"), None);
    }
}
