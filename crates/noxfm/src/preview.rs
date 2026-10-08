//! Contents for the preview panel, for files the view can't show inline.

use std::io::Read;
use std::path::Path;

/// Text beyond this many bytes or lines is cut off.
const TEXT_BYTES: u64 = 64 * 1024;
const TEXT_LINES: usize = 300;
const HEX_BYTES: u64 = 512;
/// Bytes per hex line; 8 keeps lines narrow enough for the side panel.
const HEX_ROW: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text { text: String, truncated: bool },
    Hex(String),
    Folder { items: usize },
    Empty,
    Error(String),
}

/// Blocking.
pub fn load(path: &Path) -> Content {
    match std::fs::metadata(path) {
        Err(e) => Content::Error(e.to_string()),
        Ok(m) if m.is_dir() => match std::fs::read_dir(path) {
            Ok(rd) => Content::Folder { items: rd.count() },
            Err(e) => Content::Error(e.to_string()),
        },
        Ok(m) if m.len() == 0 => Content::Empty,
        Ok(_) => {
            let mut head = Vec::new();
            match std::fs::File::open(path).and_then(|f| f.take(TEXT_BYTES).read_to_end(&mut head)) {
                Err(e) => Content::Error(e.to_string()),
                Ok(_) => from_bytes(&head),
            }
        }
    }
}

pub fn from_bytes(head: &[u8]) -> Content {
    if noxfm_core::filetype::looks_textual(head) {
        let text = String::from_utf8_lossy(head);
        let lines: Vec<&str> = text.lines().take(TEXT_LINES + 1).collect();
        let truncated = lines.len() > TEXT_LINES || head.len() as u64 >= TEXT_BYTES;
        Content::Text { text: lines[..lines.len().min(TEXT_LINES)].join("\n"), truncated }
    } else {
        Content::Hex(hex_dump(&head[..head.len().min(HEX_BYTES as usize)], HEX_ROW))
    }
}

/// `00000010  48 65 6c 6c 6f 0a …  |Hello.|`, `per_row` bytes per line.
pub fn hex_dump(bytes: &[u8], per_row: usize) -> String {
    let hex_w = per_row * 3 - 1;
    bytes
        .chunks(per_row)
        .enumerate()
        .map(|(i, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String =
                chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }).collect();
            format!("{:08x}  {:<hex_w$}  |{ascii}|", i * per_row, hex.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(from_bytes(b"fn main() {}\n"), Content::Text { text: "fn main() {}".into(), truncated: false });
        let many = "x\n".repeat(TEXT_LINES + 5);
        let Content::Text { text, truncated } = from_bytes(many.as_bytes()) else { panic!() };
        assert!(truncated);
        assert_eq!(text.lines().count(), TEXT_LINES);
        assert!(matches!(from_bytes(b"\x89PNG\0\0"), Content::Hex(_)));
    }

    #[test]
    fn hex_layout() {
        let dump = hex_dump(b"Hello, world!\n\0\x01\xffAB", 16);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines[0], "00000000  48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 00 01  |Hello, world!...|");
        // The ASCII column lines up with the full rows above.
        assert_eq!(lines[1], format!("00000010  {:<47}  |.AB|", "ff 41 42"));
        assert_eq!(lines[0].find('|'), lines[1].find('|'));
    }

    #[test]
    fn folders_and_errors() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a"), "").unwrap();
        assert_eq!(load(t.path()), Content::Folder { items: 1 });
        assert_eq!(load(&t.path().join("a")), Content::Empty);
        assert!(matches!(load(&t.path().join("nope")), Content::Error(_)));
    }
}
