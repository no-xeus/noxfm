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

/// An item's icon, with room for its default app's icon in the corner.
pub fn icon_slot() -> gtk::Overlay {
    let slot = gtk::Overlay::new();
    slot.set_child(Some(&gtk::Image::new()));
    slot.add_overlay(&gtk::Image::builder().halign(gtk::Align::Start).valign(gtk::Align::End).visible(false).build());
    slot
}

/// The icon and the badge of an [`icon_slot`].
pub fn slot_parts(slot: &gtk::Widget) -> (gtk::Image, gtk::Image) {
    let slot = slot.downcast_ref::<gtk::Overlay>().expect("an icon slot");
    let icon = slot.child().and_downcast::<gtk::Image>().unwrap();
    // GTK keeps overlays before the main child: the badge is the other one.
    let badge = std::iter::successors(slot.first_child(), |w| w.next_sibling())
        .find(|w| w != icon.upcast_ref::<gtk::Widget>())
        .and_downcast::<gtk::Image>()
        .unwrap();
    (icon, badge)
}

fn app_icon(app: &noxfm_proto::AppRef) -> gio::Icon {
    match &app.icon {
        Some(i) if i.starts_with('/') => gio::FileIcon::new(&gio::File::for_path(i)).upcast(),
        Some(i) => gio::ThemedIcon::from_names(&[i.as_str(), "application-x-executable"]).upcast(),
        None => gio::ThemedIcon::new("application-x-executable").upcast(),
    }
}

/// "git" next to repositories (shown when the entry is one).
pub fn git_badge() -> gtk::Label {
    let l = gtk::Label::builder().label("git").visible(false).valign(gtk::Align::Center).build();
    l.add_css_class("badge");
    l
}

/// A warning when the content doesn't match the extension.
pub fn mismatch_mark() -> gtk::Image {
    gtk::Image::builder().icon_name("dialog-warning-symbolic").pixel_size(14).visible(false).build()
}

pub fn bind_marks(git: &gtk::Label, mismatch: Option<&gtk::Image>, e: &Entry) {
    git.set_visible(e.is_git);
    if let Some(m) = mismatch {
        m.set_visible(e.mime_mismatch);
        if e.mime_mismatch {
            let tip = format!(
                "Content is {}, which doesn't match the .{} extension",
                e.mime.as_deref().unwrap_or("?"),
                e.extension().unwrap_or("")
            );
            m.set_tooltip_text(Some(&tip));
        }
    }
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
    /// Every cell widget on screen and the item it shows, to find the item
    /// under the pointer (right-click, drag, drop).
    owners: RefCell<HashMap<gtk::Widget, PathBuf>>,
    /// Items cut to the clipboard: drawn dimmed until pasted or replaced.
    cut: RefCell<HashSet<PathBuf>>,
    /// Folder a drag hovers, highlighted as the drop target.
    drop_hover: RefCell<Option<PathBuf>>,
    /// A line under the name, by path (why an item is in Recent, where a
    /// trashed one came from).
    captions: RefCell<HashMap<PathBuf, String>>,
}

/// Icon opacity of cut items.
const CUT_OPACITY: f64 = 0.45;

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
            owners: RefCell::default(),
            cut: RefCell::default(),
            drop_hover: RefCell::default(),
            captions: RefCell::default(),
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
        image.set_opacity(if self.cut.borrow().contains(&e.path) { CUT_OPACITY } else { 1.0 });
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

    /// Icon (or thumbnail) at `px`, with the default app's icon in the
    /// bottom-left corner of files.
    pub fn bind_slot(self: &Rc<Self>, slot: &gtk::Widget, e: &Entry, px: i32) {
        let (icon, badge) = slot_parts(slot);
        icon.set_pixel_size(px);
        self.bind_icon(&icon, e);
        match e.app.as_ref().filter(|_| e.kind == EntryKind::File) {
            Some(app) => {
                badge.set_from_gicon(&app_icon(app));
                badge.set_pixel_size((px * 45 / 100).max(10));
                badge.set_visible(true);
                slot.set_tooltip_text(Some(&format!("Opens with {}", app.name)));
            }
            None => {
                badge.set_visible(false);
                slot.set_tooltip_text(None);
            }
        }
    }

    pub fn unbind_slot(&self, slot: &gtk::Widget, path: &Path) {
        self.unbind_icon(&slot_parts(slot).0, path);
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

    /// `w` shows `path` (until [`Cells::disown`]).
    pub fn own(&self, w: &impl IsA<gtk::Widget>, path: &Path) {
        let w = w.upcast_ref::<gtk::Widget>();
        if self.drop_hover.borrow().as_deref() == Some(path) {
            w.add_css_class("drop-target");
        } else {
            w.remove_css_class("drop-target");
        }
        self.owners.borrow_mut().insert(w.clone(), path.to_path_buf());
    }

    pub fn disown(&self, w: &impl IsA<gtk::Widget>) {
        self.owners.borrow_mut().remove(w.upcast_ref::<gtk::Widget>());
    }

    /// The item shown at `(x, y)` of `view`, if any.
    pub fn path_at(&self, view: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Option<PathBuf> {
        let view = view.upcast_ref::<gtk::Widget>();
        let owners = self.owners.borrow();
        let mut w = view.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(cur) = w {
            if let Some(p) = owners.get(&cur) {
                return Some(p.clone());
            }
            if cur == *view {
                break;
            }
            w = cur.parent();
        }
        None
    }

    /// A widget showing `path`, preferably the one registered first (the
    /// name cell or the tile), to anchor a popover to.
    pub fn anchor(&self, path: &Path) -> Option<gtk::Widget> {
        live(&self.icons, path).into_iter().next().and_then(|i| i.parent())
    }

    pub fn set_captions(&self, captions: HashMap<PathBuf, String>) {
        *self.captions.borrow_mut() = captions;
    }

    pub fn caption(&self, path: &Path) -> Option<String> {
        self.captions.borrow().get(path).cloned()
    }

    #[cfg(test)]
    pub fn is_cut(&self, path: &Path) -> bool {
        self.cut.borrow().contains(path)
    }

    pub fn set_cut(&self, paths: HashSet<PathBuf>) {
        *self.cut.borrow_mut() = paths;
        let cut = self.cut.borrow();
        for (path, images) in self.icons.borrow().iter() {
            let opacity = if cut.contains(path) { CUT_OPACITY } else { 1.0 };
            images.iter().filter_map(|w| w.upgrade()).for_each(|i| i.set_opacity(opacity));
        }
    }

    pub fn set_drop_hover(&self, path: Option<PathBuf>) {
        if *self.drop_hover.borrow() == path {
            return;
        }
        *self.drop_hover.borrow_mut() = path.clone();
        for (w, p) in self.owners.borrow().iter() {
            if Some(p) == path.as_ref() {
                w.add_css_class("drop-target");
            } else {
                w.remove_css_class("drop-target");
            }
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
