//! Files on the Wayland clipboard, in the formats other file managers use,
//! so copy/paste works between noxfm and Nautilus, Dolphin, terminals, etc.

use std::borrow::Cow;
use std::path::PathBuf;

use cosmic::iced::clipboard::mime::{AllowedMimeTypes, AsMimeTypes};
use noxfm_core::uri;
use noxfm_proto::TransferOp;

#[derive(Debug, Clone, PartialEq)]
pub struct ClipboardFiles {
    pub op: TransferOp,
    pub paths: Vec<PathBuf>,
}

impl AsMimeTypes for ClipboardFiles {
    fn available(&self) -> Cow<'static, [String]> {
        Cow::Owned(vec![uri::GNOME_COPIED.into(), uri::URI_LIST.into(), uri::PLAIN.into(), "text/plain".into()])
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        let s = match mime {
            uri::GNOME_COPIED => uri::gnome_copied(self.op, &self.paths),
            uri::URI_LIST => uri::uri_list(&self.paths),
            m if m.starts_with("text/plain") => uri::plain(&self.paths),
            _ => return None,
        };
        Some(Cow::Owned(s.into_bytes()))
    }
}

impl AllowedMimeTypes for ClipboardFiles {
    /// Most preferred first: only gnome-copied-files can say "cut".
    fn allowed() -> Cow<'static, [String]> {
        Cow::Owned(vec![uri::GNOME_COPIED.into(), uri::URI_LIST.into()])
    }
}

impl TryFrom<(Vec<u8>, String)> for ClipboardFiles {
    type Error = ();

    fn try_from((data, mime): (Vec<u8>, String)) -> Result<Self, ()> {
        let (op, paths) = match mime.as_str() {
            uri::GNOME_COPIED => uri::parse_gnome_copied(&data).ok_or(())?,
            uri::URI_LIST => (TransferOp::Copy, uri::parse_uri_list(&data)),
            _ => return Err(()),
        };
        if paths.is_empty() { Err(()) } else { Ok(ClipboardFiles { op, paths }) }
    }
}

/// Files being dragged. Drag-and-drop between apps speaks `text/uri-list`;
/// whether it's a move or a copy is negotiated separately.
#[derive(Debug, Clone, PartialEq)]
pub struct DragFiles(pub Vec<PathBuf>);

impl AsMimeTypes for DragFiles {
    fn available(&self) -> Cow<'static, [String]> {
        Cow::Owned(vec![uri::URI_LIST.into(), uri::PLAIN.into(), "text/plain".into()])
    }

    fn as_bytes(&self, mime: &str) -> Option<Cow<'static, [u8]>> {
        let s = match mime {
            uri::URI_LIST => uri::uri_list(&self.0),
            m if m.starts_with("text/plain") => uri::plain(&self.0),
            _ => return None,
        };
        Some(Cow::Owned(s.into_bytes()))
    }
}

impl AllowedMimeTypes for DragFiles {
    fn allowed() -> Cow<'static, [String]> {
        Cow::Owned(vec![uri::URI_LIST.into()])
    }
}

impl TryFrom<(Vec<u8>, String)> for DragFiles {
    type Error = ();

    fn try_from((data, mime): (Vec<u8>, String)) -> Result<Self, ()> {
        if mime != uri::URI_LIST {
            return Err(());
        }
        let paths = uri::parse_uri_list(&data);
        if paths.is_empty() { Err(()) } else { Ok(DragFiles(paths)) }
    }
}
