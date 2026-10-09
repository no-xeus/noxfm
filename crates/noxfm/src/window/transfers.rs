//! Copy/move/zip jobs: a compact indicator in the status line, which opens
//! a list with progress, speed, time left and Cancel. Every window shows
//! every job (noxd broadcasts them).

use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use noxfm_proto::{Request, Response, TransferStatus};

use super::Browser;

/// The widgets of one job in the list.
pub(super) struct TransferRow {
    row: gtk::Box,
    title: gtk::Label,
    bar: gtk::ProgressBar,
    detail: gtk::Label,
    button: gtk::Button,
}

/// How often finished jobs are checked for removal.
const TICK: Duration = Duration::from_millis(500);

impl Browser {
    pub(super) fn load_transfers(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.pane().request_then(Request::ListTransfers, move |_, r| {
            if let (Some(b), Response::Transfers(list)) = (weak.upgrade(), r) {
                b.jobs.borrow_mut().snapshot(list);
                b.refresh_transfers();
            }
        });
    }

    pub(super) fn transfer_progress(self: &Rc<Self>, status: TransferStatus) {
        self.jobs.borrow_mut().progress(status);
        self.refresh_transfers();
    }

    pub(super) fn transfer_done(self: &Rc<Self>, id: u64, error: Option<String>) {
        self.jobs.borrow_mut().done(id, error);
        self.refresh_transfers();
    }

    pub(super) fn toggle_transfers(&self) {
        let open = !self.transfers_revealer.reveals_child();
        self.transfers_revealer.set_reveal_child(open && !self.jobs.borrow().is_empty());
    }

    fn refresh_transfers(self: &Rc<Self>) {
        let jobs = self.jobs.borrow();
        // Rows stay while their job does, so a click never lands on a
        // button that was just replaced.
        self.transfer_rows.borrow_mut().retain(|id, row| {
            let keep = jobs.iter().any(|j| j.status.id == *id);
            if !keep {
                self.transfers.remove(&row.row);
            }
            keep
        });
        for j in jobs.iter() {
            let id = j.status.id;
            let mut rows = self.transfer_rows.borrow_mut();
            let row = rows.entry(id).or_insert_with(|| {
                let row = self.transfer_row(id);
                self.transfers.append(&row.row);
                row
            });
            row.title.set_text(&j.title());
            row.bar.set_fraction(f64::from(j.fraction()));
            row.detail.set_text(&j.detail());
            row.button.set_label(if j.finished.is_none() { "Cancel" } else { "Dismiss" });
        }

        let sum = jobs.summary();
        let empty = jobs.is_empty();
        drop(jobs);
        self.transfers_button.set_visible(!empty);
        if empty {
            self.transfers_revealer.set_reveal_child(false);
            return;
        }
        let label = if sum.running > 0 {
            format!("{} transfer{} · {:.0}%", sum.running, if sum.running == 1 { "" } else { "s" }, sum.fraction * 100.0)
        } else if sum.failed > 0 {
            format!("{} transfer{} failed", sum.failed, if sum.failed == 1 { "" } else { "s" })
        } else {
            "Transfers done".into()
        };
        self.transfers_button.set_label(&label);
        self.start_ticking();
    }

    fn transfer_row(self: &Rc<Self>, id: u64) -> TransferRow {
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
        let title = gtk::Label::builder().xalign(0.0).build();
        title.add_css_class("heading");
        let bar = gtk::ProgressBar::new();
        let detail = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        detail.add_css_class("caption");
        text.append(&title);
        text.append(&bar);
        text.append(&detail);
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&text);
        let button = gtk::Button::builder().valign(gtk::Align::Center).build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            let Some(b) = weak.upgrade() else { return };
            let running = b.jobs.borrow().iter().any(|j| j.status.id == id && j.finished.is_none());
            if running {
                b.pane().fire(Request::CancelTransfer { id });
            } else {
                b.jobs.borrow_mut().dismiss(id);
                b.refresh_transfers();
            }
        });
        row.append(&button);
        TransferRow { row, title, bar, detail, button }
    }

    /// Finished jobs linger a moment, then go: check while there are jobs.
    fn start_ticking(self: &Rc<Self>) {
        if self.ticking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(TICK, move || {
            let Some(b) = weak.upgrade() else { return glib::ControlFlow::Break };
            b.jobs.borrow_mut().tick();
            b.refresh_transfers();
            if b.jobs.borrow().is_empty() {
                b.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
}
