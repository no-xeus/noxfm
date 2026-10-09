//! Details (list) view: one row per entry, with sortable columns.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use noxfm_core::{SortKey, fmt};
use noxfm_proto::Entry;

use crate::cells::{Cells, entry_of};

pub struct ListView {
    pub view: gtk::ColumnView,
    pub sorter: gtk::ColumnViewSorter,
    pub size: gtk::ColumnViewColumn,
    pub created: gtk::ColumnViewColumn,
    pub owner: gtk::ColumnViewColumn,
    pub permissions: gtk::ColumnViewColumn,
    name: gtk::ColumnViewColumn,
    /// Every column and the key it sorts by.
    keyed: Vec<(SortKey, gtk::ColumnViewColumn)>,
    cells: Rc<Cells>,
}

impl ListView {
    /// The columns; the caller sorts its model with [`ListView::sorter`].
    pub fn new(cells: &Rc<Cells>) -> ListView {
        let view = gtk::ColumnView::new(None::<gtk::MultiSelection>);
        view.set_enable_rubberband(true);
        let ascending = Rc::new(Cell::new(true));
        let col = |title, key, factory| column(title, key, &ascending, factory);

        let name = col("Name", SortKey::Name, name_factory(cells));
        name.set_expand(true);
        let ext = col("Type", SortKey::Extension, label_factory(cells, |e| e.extension().map(str::to_lowercase).unwrap_or_default()));
        ext.set_fixed_width(80);
        let size = col("Size", SortKey::Size, size_factory(cells));
        size.set_fixed_width(100);
        let modified = col("Modified", SortKey::Modified, label_factory(cells, |e| fmt::time(e.modified)));
        modified.set_fixed_width(150);
        let created = col("Created", SortKey::Created, label_factory(cells, |e| fmt::time(e.created)));
        created.set_fixed_width(150);
        let owner = col("Owner", SortKey::Owner, label_factory(cells, owner_text));
        owner.set_fixed_width(110);
        let permissions = col(
            "Permissions",
            SortKey::Permissions,
            label_factory(cells, |e| format!("{} {}", noxfm_core::perms::symbolic(e.mode), noxfm_core::perms::octal(e.mode))),
        );
        permissions.set_fixed_width(140);
        for c in [&name, &ext, &size, &modified, &created, &owner, &permissions] {
            view.append_column(c);
        }
        for c in [&created, &owner, &permissions] {
            c.set_visible(false);
        }

        let sorter = view.sorter().and_downcast::<gtk::ColumnViewSorter>().expect("column view sorter");
        // Connected before any sort model, so the direction is current when it re-sorts.
        sorter.connect_changed(move |s, _| ascending.set(s.primary_sort_order() == gtk::SortType::Ascending));
        view.sort_by_column(Some(&name), gtk::SortType::Ascending);
        let keyed = vec![
            (SortKey::Name, name.clone()),
            (SortKey::Extension, ext),
            (SortKey::Size, size.clone()),
            (SortKey::Modified, modified),
            (SortKey::Created, created.clone()),
            (SortKey::Owner, owner.clone()),
            (SortKey::Permissions, permissions.clone()),
        ];
        ListView { view, sorter, size, created, owner, permissions, name, keyed, cells: cells.clone() }
    }

    /// Icons take the current zoom (rows are rebuilt).
    pub fn zoomed(&self) {
        self.name.set_factory(Some(&name_factory(&self.cells)));
    }

    pub fn sort(&self, key: SortKey, ascending: bool) {
        let col = self.keyed.iter().find(|(k, _)| *k == key).map(|(_, c)| c);
        let order = if ascending { gtk::SortType::Ascending } else { gtk::SortType::Descending };
        self.view.sort_by_column(col, order);
    }

    /// The sort key and direction in effect.
    pub fn sorting(&self) -> (SortKey, bool) {
        let col = self.sorter.primary_sort_column();
        let key = self.keyed.iter().find(|(_, c)| Some(c) == col.as_ref()).map_or(SortKey::Name, |(k, _)| *k);
        (key, self.sorter.primary_sort_order() == gtk::SortType::Ascending)
    }

    /// Daemon order (Recent: newest first) until a column is clicked.
    pub fn unsort(&self) {
        self.view.sort_by_column(None, gtk::SortType::Ascending);
    }

    pub fn sorted_by_size(&self) -> bool {
        self.sorter.primary_sort_column().as_ref() == Some(&self.size)
    }
}

fn owner_text(e: &Entry) -> String {
    match (&e.owner, &e.group) {
        (Some(o), Some(g)) if o == g => o.clone(),
        (Some(o), Some(g)) => format!("{o}:{g}"),
        (o, _) => o.clone().unwrap_or_else(|| e.uid.to_string()),
    }
}

/// A column sorted in noxfm's order: folders first and unknown values last
/// in both directions.
fn column(title: &str, key: SortKey, ascending: &Rc<Cell<bool>>, factory: gtk::SignalListItemFactory) -> gtk::ColumnViewColumn {
    let asc = ascending.clone();
    let sorter = gtk::CustomSorter::new(move |a, b| {
        let asc = asc.get();
        let order = noxfm_core::compare(&entry_of(a), &entry_of(b), key, asc);
        // GTK reverses a descending column's result; noxfm's order isn't a
        // plain reverse, so undo that.
        (if asc { order } else { order.reverse() }).into()
    });
    let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    col.set_sorter(Some(&sorter));
    col.set_resizable(true);
    col
}

fn label() -> gtk::Label {
    gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build()
}

fn label_factory(cells: &Rc<Cells>, text: impl Fn(&Entry) -> String + 'static) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, item| item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&label())));
    let c = cells.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        label.set_text(&text(&e));
        c.own(&label, &e.path);
    });
    let c = cells.clone();
    f.connect_unbind(move |_, item| {
        if let Some(child) = item.downcast_ref::<gtk::ListItem>().unwrap().child() {
            c.disown(&child);
        }
    });
    f
}

fn name_factory(cells: &Rc<Cells>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, item| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Image::new());
        row.append(&label());
        let caption = label();
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        row.append(&caption);
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&row));
    });
    let c = cells.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (image, label) = parts(item);
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        image.set_pixel_size(c.list_px());
        c.bind_icon(&image, &e);
        label.set_text(&e.name);
        let caption = label.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let text = c.caption(&e.path);
        caption.set_visible(text.is_some());
        caption.set_text(text.as_deref().unwrap_or_default());
        c.own(&item.child().unwrap(), &e.path);
    });
    let c = cells.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(obj) = item.item() {
            c.unbind_icon(&parts(item).0, &entry_of(&obj).path);
        }
        if let Some(child) = item.child() {
            c.disown(&child);
        }
    });
    f
}

fn parts(item: &gtk::ListItem) -> (gtk::Image, gtk::Label) {
    let image = item.child().and_then(|r| r.first_child()).and_downcast::<gtk::Image>().unwrap();
    let label = image.next_sibling().and_downcast::<gtk::Label>().unwrap();
    (image, label)
}

/// Size cells change in place when a folder's size arrives.
fn size_factory(cells: &Rc<Cells>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, item| item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&label())));
    let c = cells.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let obj = item.item().unwrap();
        let e = entry_of(&obj);
        c.bind_size(&label, &e);
        c.own(&label, &e.path);
    });
    let c = cells.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        if let Some(obj) = item.item() {
            c.unbind_size(&label, &entry_of(&obj).path);
        }
        c.disown(&label);
    });
    f
}
