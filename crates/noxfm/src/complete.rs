//! Path completion: suggestions in a popover under the path bar, above the
//! window's content. Tab or a click takes one; typing goes on in the bar.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use noxfm_proto::{Request, Response};

use crate::daemon::Daemon;

/// Suggestions shown at once.
const SHOWN: usize = 8;

pub struct Completion {
    entry: gtk::Entry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    found: RefCell<Vec<String>>,
    /// The text is being set by the window (not typed): don't suggest.
    quiet: Cell<bool>,
    daemon: Daemon,
}

impl Completion {
    pub fn attach(entry: &gtk::Entry, daemon: Daemon) -> Rc<Self> {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).focusable(false).build();
        let popover = gtk::Popover::builder()
            .child(&list)
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .focusable(false)
            .build();
        popover.set_parent(entry);
        let this = Rc::new(Completion {
            entry: entry.clone(),
            popover,
            list,
            found: RefCell::default(),
            quiet: Cell::new(false),
            daemon,
        });

        let weak = Rc::downgrade(&this);
        entry.connect_changed(move |e| {
            if let Some(c) = weak.upgrade()
                && !c.quiet.get()
                && e.has_focus()
            {
                c.suggest(e.text().to_string());
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            if let Some(c) = weak.upgrade() {
                c.accept(row.index() as usize);
            }
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(c) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !c.popover.is_visible() {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::Tab => c.accept(0),
                gdk::Key::Escape => c.close(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        // Before the entry, which would move focus on Tab.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&this);
        focus.connect_leave(move |_| {
            if let Some(c) = weak.upgrade() {
                c.close();
            }
        });
        entry.add_controller(focus);
        this
    }

    /// Sets the bar's text without suggesting anything.
    pub fn set_text_quietly(&self, text: &str) {
        self.quiet.set(true);
        self.entry.set_text(text);
        self.quiet.set(false);
        self.close();
    }

    pub fn close(&self) {
        self.popover.popdown();
        self.found.borrow_mut().clear();
    }

    fn suggest(self: &Rc<Self>, prefix: String) {
        let weak = Rc::downgrade(self);
        let daemon = self.daemon.clone();
        glib::spawn_future_local(async move {
            let found = match daemon.request(Request::Complete { prefix: prefix.clone() }).await {
                Ok(Response::Completions(c)) => c,
                _ => Vec::new(),
            };
            // Drop answers for text the user has changed since.
            if let Some(c) = weak.upgrade()
                && c.entry.text() == prefix
                && c.entry.has_focus()
            {
                c.show(found);
            }
        });
    }

    fn show(&self, found: Vec<String>) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        if found.is_empty() {
            self.close();
            return;
        }
        for s in found.iter().take(SHOWN) {
            let label = gtk::Label::builder().label(s).xalign(0.0).margin_start(6).margin_end(6).build();
            let row = gtk::ListBoxRow::builder().child(&label).focusable(false).build();
            self.list.append(&row);
        }
        self.list.set_size_request(self.entry.width(), -1);
        *self.found.borrow_mut() = found;
        self.popover.popup();
    }

    /// Puts suggestion `i` in the bar; suggestions for it follow.
    fn accept(&self, i: usize) {
        let Some(s) = self.found.borrow().get(i).cloned() else { return };
        self.entry.set_text(&s);
        self.entry.set_position(-1);
    }
}
