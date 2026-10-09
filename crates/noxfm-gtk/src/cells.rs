//! What item cells show beyond the entry itself, shared by the list and the
//! grid: icons and thumbnails, folder sizes that arrive later, the zoom.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use noxfm_core::fmt;
use noxfm_proto::{Entry, EntryKind, Request, Response};

use crate::daemon::Daemon;

/// Icon sizes Ctrl + / Ctrl − step through, per view.
pub const LIST_ICONS: [i32; 4] = [16, 24, 32, 48];
pub const GRID_ICONS: [i32; 6] = [48, 64, 80, 96, 128, 192];
pub const LIST_ZOOM: usize = 1;
pub const GRID_ZOOM: usize = 2;

pub fn entry_of(obj: &glib::Object) -> Ref<'_, Entry> {
    obj.downcast_ref::<glib::BoxedAnyObject>().expect("list items are entries").borrow::<Entry>()
}

pub fn size_text(e: &Entry) -> String {
    match (e.kind, e.size) {
        (_, Some(b)) => fmt::size(b),
        // Being measured. Symlinked folders are never walked.
        (EntryKind::Dir, None) if !e.symlink => "…".into(),
        _ => String::new(),
    }
}

struct Thumb {
    texture: gdk::Texture,
    /// mtime of the file it was made from.
    modified: Option<i64>,
}

/// Widgets on screen for each path, so late data can reach them.
type Live<W> = RefCell<HashMap<PathBuf, Vec<glib::WeakRef<W>>>>;

pub struct Cells {
    daemon: Daemon,
    pub list_zoom: Cell<usize>,
    pub grid_zoom: Cell<usize>,
    thumbs: RefCell<HashMap<PathBuf, Thumb>>,
    /// Asked for, or not available: don't ask again.
    requested: RefCell<HashSet<PathBuf>>,
    icons: Live<gtk::Image>,
    sizes: Live<gtk::Label>,
}

impl Cells {
    pub fn new(daemon: Daemon) -> Rc<Self> {
        Rc::new(Cells {
            daemon,
            list_zoom: Cell::new(LIST_ZOOM),
            grid_zoom: Cell::new(GRID_ZOOM),
            thumbs: RefCell::default(),
            requested: RefCell::default(),
            icons: RefCell::default(),
            sizes: RefCell::default(),
        })
    }

    pub fn list_px(&self) -> i32 {
        LIST_ICONS[self.list_zoom.get()]
    }

    pub fn grid_px(&self) -> i32 {
        GRID_ICONS[self.grid_zoom.get()]
    }

    /// The thumbnail if there is a current one, else the themed icon (and a
    /// thumbnail is asked for, once, if the type has them).
    pub fn bind_icon(self: &Rc<Self>, image: &gtk::Image, e: &Entry) {
        register(&self.icons, &e.path, image);
        if let Some(t) = self.thumbs.borrow().get(&e.path).filter(|t| t.modified == e.modified) {
            image.set_paintable(Some(&t.texture));
            return;
        }
        let names = fmt::icon_names(e);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        image.set_from_gicon(&gio::ThemedIcon::from_names(&names));
        if e.kind == EntryKind::File && e.mime.as_deref().is_some_and(noxfm_core::thumbnail::supported) {
            self.request_thumbnail(e.path.clone(), e.modified);
        }
    }

    pub fn unbind_icon(&self, image: &gtk::Image, path: &Path) {
        unregister(&self.icons, path, image);
    }

    pub fn bind_size(&self, label: &gtk::Label, e: &Entry) {
        label.set_text(&size_text(e));
        register(&self.sizes, &e.path, label);
    }

    pub fn unbind_size(&self, label: &gtk::Label, path: &Path) {
        unregister(&self.sizes, path, label);
    }

    pub fn size_updated(&self, path: &Path, bytes: u64) {
        for label in live(&self.sizes, path) {
            label.set_text(&fmt::size(bytes));
        }
    }

    /// Another folder is shown: its thumbnails are asked for afresh.
    pub fn forget_thumbnails(&self) {
        self.thumbs.borrow_mut().clear();
        self.requested.borrow_mut().clear();
    }

    fn request_thumbnail(self: &Rc<Self>, path: PathBuf, modified: Option<i64>) {
        let fresh = self.thumbs.borrow().get(&path).is_none_or(|t| t.modified != modified);
        if !fresh || !self.requested.borrow_mut().insert(path.clone()) {
            return;
        }
        let weak = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            let bytes = match daemon.request(Request::Thumbnail { path: path.clone() }).await {
                Ok(Response::Thumbnail { cache_path: Some(p) }) => {
                    gio::spawn_blocking(move || std::fs::read(p).ok()).await.ok().flatten()
                }
                _ => None,
            };
            let Some(this) = weak.upgrade() else { return };
            let Some(texture) = bytes.and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok()) else {
                return;
            };
            let shown = live(&this.icons, &path);
            log::debug!("thumbnail for {} ({}x{}, {} on screen)", path.display(), texture.width(), texture.height(), shown.len());
            for image in shown {
                image.set_paintable(Some(&texture));
            }
            this.thumbs.borrow_mut().insert(path, Thumb { texture, modified });
        });
    }
}

fn register<W: IsA<glib::Object>>(map: &Live<W>, path: &Path, w: &W) {
    let mut map = map.borrow_mut();
    let v = map.entry(path.to_path_buf()).or_default();
    v.retain(|x| x.upgrade().is_some());
    v.push(w.downgrade());
}

fn unregister<W: IsA<glib::Object>>(map: &Live<W>, path: &Path, w: &W) {
    let mut map = map.borrow_mut();
    if let Some(v) = map.get_mut(path) {
        v.retain(|x| x.upgrade().is_some_and(|x| x != *w));
        if v.is_empty() {
            map.remove(path);
        }
    }
}

fn live<W: IsA<glib::Object>>(map: &Live<W>, path: &Path) -> Vec<W> {
    map.borrow().get(path).map(|v| v.iter().filter_map(|w| w.upgrade()).collect()).unwrap_or_default()
}
