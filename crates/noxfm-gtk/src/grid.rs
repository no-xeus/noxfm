//! Icon (grid) view: tiles big enough to see thumbnails.

use std::rc::Rc;

use gtk::prelude::*;

use crate::cells::{Cells, entry_of};

pub fn new(cells: &Rc<Cells>) -> gtk::GridView {
    let view = gtk::GridView::new(None::<gtk::MultiSelection>, Some(factory(cells)));
    view.set_enable_rubberband(true);
    view.set_max_columns(64);
    view
}

/// Tiles take the current zoom (rebuilt).
pub fn zoomed(view: &gtk::GridView, cells: &Rc<Cells>) {
    view.set_factory(Some(&factory(cells)));
}

fn factory(cells: &Rc<Cells>) -> gtk::SignalListItemFactory {
    let px = cells.grid_px();
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let tile = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .width_request(px + 40)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        tile.append(&gtk::Image::builder().pixel_size(px).build());
        // Long names wrap to two lines, then are cut in the middle of the second.
        tile.append(
            &gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .justify(gtk::Justification::Center)
                .max_width_chars(((px + 40) / 8).max(8))
                .build(),
        );
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&tile));
    });
    let c = cells.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (image, label) = parts(item);
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        c.bind_icon(&image, &e);
        label.set_text(&e.name);
    });
    let c = cells.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(obj) = item.item() {
            c.unbind_icon(&parts(item).0, &entry_of(&obj).path);
        }
    });
    f
}

fn parts(item: &gtk::ListItem) -> (gtk::Image, gtk::Label) {
    let image = item.child().and_then(|t| t.first_child()).and_downcast::<gtk::Image>().unwrap();
    let label = image.next_sibling().and_downcast::<gtk::Label>().unwrap();
    (image, label)
}
