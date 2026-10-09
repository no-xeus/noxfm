//! Tabs: several places in one window. The tab bar shows from two tabs on.
//! Tabs can be reordered by dragging; dragged out of the bar, a tab opens
//! in a window of its own.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use super::Browser;
use super::pane::{Loc, Nav, Pane};

impl Browser {
    pub(super) fn open_tab(self: &Rc<Self>, loc: Loc, focus: bool) -> Rc<Pane> {
        let pane = Pane::new(self, loc.clone(), self.opts.get());
        pane.cells.set_cut(self.cut.borrow().clone());
        self.connect_menus(&pane);
        pane.connect_dnd(&pane.list.view.clone().upcast());
        pane.connect_dnd(&pane.grid.clone().upcast());

        let label = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(24).build();
        let close = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text("Close tab (Ctrl+W)").build();
        close.add_css_class("flat");
        let tab = gtk::Box::builder().spacing(4).build();
        tab.append(&label);
        tab.append(&close);
        let weak = Rc::downgrade(&pane);
        close.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.browser().close_tab(&p);
            }
        });
        // Middle click closes, as in browsers.
        let middle = gtk::GestureClick::builder().button(gdk::BUTTON_MIDDLE).build();
        let weak = Rc::downgrade(&pane);
        middle.connect_pressed(move |_, _, _, _| {
            if let Some(p) = weak.upgrade() {
                p.browser().close_tab(&p);
            }
        });
        tab.add_controller(middle);

        self.panes.borrow_mut().push(pane.clone());
        let at = self.notebook.current_page().map_or(-1, |i| i as i32 + 1);
        let pos = self.notebook.insert_page(&pane.root, Some(&tab), if at < 0 { None } else { Some(at as u32) });
        self.notebook.set_tab_reorderable(&pane.root, true);
        self.notebook.set_tab_detachable(&pane.root, true);
        self.notebook.set_show_tabs(self.notebook.n_pages() > 1);
        self.update_tab_label(&pane);
        if focus {
            self.notebook.set_current_page(Some(pos));
        }
        pane.load(loc, Nav::Reload);
        pane
    }

    /// The last tab closes the window.
    pub(super) fn close_tab(self: &Rc<Self>, pane: &Rc<Pane>) {
        if self.notebook.n_pages() <= 1 {
            self.window.close();
            return;
        }
        if let Some(i) = self.notebook.page_num(&pane.root) {
            self.notebook.remove_page(Some(i));
        }
        pane.list_menu.unparent();
        pane.grid_menu.unparent();
        self.panes.borrow_mut().retain(|p| !Rc::ptr_eq(p, pane));
        let watch = pane.state.borrow_mut().subscribed.take();
        if let Some(path) = watch.filter(|w| !self.watched_by_other(w, pane)) {
            self.pane().fire(noxfm_proto::Request::Unsubscribe { path });
        }
        self.notebook.set_show_tabs(self.notebook.n_pages() > 1);
        self.sync_chrome();
    }

    pub(super) fn cycle_tab(&self, step: i32) {
        let n = self.notebook.n_pages() as i32;
        if n > 1 {
            let cur = self.notebook.current_page().unwrap_or(0) as i32;
            self.notebook.set_current_page(Some((cur + step).rem_euclid(n) as u32));
        }
    }

    pub(super) fn update_tab_label(&self, pane: &Pane) {
        let Some(tab) = self.notebook.tab_label(&pane.root) else { return };
        if let Some(label) = tab.first_child().and_downcast::<gtk::Label>() {
            let loc = pane.loc();
            label.set_text(&loc.title());
            tab.set_tooltip_text(Some(&loc.display()));
        }
    }

    pub(super) fn connect_tabs(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.notebook.connect_notify_local(Some("page"), move |_, _| {
            if let Some(b) = weak.upgrade() {
                b.sync_chrome();
                b.pane().current_view().grab_focus();
            }
        });
        // Dropped outside the bar: the tab becomes its own window (a process
        // of its own, like every window), and leaves this one.
        let weak = Rc::downgrade(self);
        self.notebook.connect_create_window(move |_, page| {
            let b = weak.upgrade()?;
            let pane = b.panes.borrow().iter().find(|p| p.root.upcast_ref::<gtk::Widget>() == page).cloned()?;
            b.open_window(pane.loc());
            glib::idle_add_local_once(move || pane.browser().close_tab(&pane));
            None
        });
    }
}
