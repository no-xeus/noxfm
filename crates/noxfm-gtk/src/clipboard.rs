//! Files on the clipboard, in the formats other file managers use, so
//! copy/paste works between noxfm and Nautilus, Dolphin, terminals, etc.

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::uri;
use noxfm_proto::TransferOp;

pub fn write(clipboard: &gdk::Clipboard, op: TransferOp, paths: &[PathBuf]) {
    let bytes = |mime: &str, s: String| gdk::ContentProvider::for_bytes(mime, &glib::Bytes::from_owned(s.into_bytes()));
    let provider = gdk::ContentProvider::new_union(&[
        // Only gnome-copied-files can say "cut".
        bytes(uri::GNOME_COPIED, uri::gnome_copied(op, paths)),
        bytes(uri::URI_LIST, uri::uri_list(paths)),
        bytes(uri::PLAIN, uri::plain(paths)),
        bytes("text/plain", uri::plain(paths)),
    ]);
    if let Err(e) = clipboard.set_content(Some(&provider)) {
        log::warn!("clipboard: {e}");
    }
}

/// The files on the clipboard and whether they were cut, if any.
pub async fn read(clipboard: &gdk::Clipboard) -> Option<(TransferOp, Vec<PathBuf>)> {
    let (stream, mime) = clipboard.read_future(&[uri::GNOME_COPIED, uri::URI_LIST], glib::Priority::DEFAULT).await.ok()?;
    let out = gio::MemoryOutputStream::new_resizable();
    out.splice_future(
        &stream,
        gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
        glib::Priority::DEFAULT,
    )
    .await
    .ok()?;
    let data = out.steal_as_bytes();
    let (op, paths) = match mime.as_str() {
        uri::GNOME_COPIED => uri::parse_gnome_copied(&data)?,
        _ => (TransferOp::Copy, uri::parse_uri_list(&data)),
    };
    (!paths.is_empty()).then_some((op, paths))
}
