//! Icon (grid) view: tiles big enough to see thumbnails.

use std::rc::Rc;

use gtk::prelude::*;

use crate::cells::{self, Cells, entry_of};

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
        let slot = cells::icon_slot();
        slot.set_halign(gtk::Align::Center);
        tile.append(&slot);
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
        let git = cells::git_badge();
        git.set_halign(gtk::Align::Center);
        tile.append(&git);
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&tile));
    });
    let c = cells.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (slot, label, git) = parts(item);
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        c.bind_slot(&slot, &e, c.grid_px());
        label.set_text(&e.name);
        cells::bind_marks(&git, None, &e);
        c.own(&item.child().unwrap(), &e.path);
    });
    let c = cells.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(obj) = item.item() {
            c.unbind_slot(&parts(item).0, &entry_of(&obj).path);
        }
        if let Some(child) = item.child() {
            c.disown(&child);
        }
    });
    f
}

/// Icon slot, name, git badge.
fn parts(item: &gtk::ListItem) -> (gtk::Widget, gtk::Label, gtk::Label) {
    let slot = item.child().and_then(|t| t.first_child()).unwrap();
    let label = slot.next_sibling().and_downcast::<gtk::Label>().unwrap();
    let git = label.next_sibling().and_downcast::<gtk::Label>().unwrap();
    (slot, label, git)
}
