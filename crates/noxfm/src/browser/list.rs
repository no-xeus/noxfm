//! Details (list) view: one row per entry, with sortable columns.

use std::path::PathBuf;
use std::sync::Arc;

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget;
use noxfm_core::SortKey;
use noxfm_proto::{Entry, EntryKind};

use super::{App, Message, badge};
use crate::{fmt, icons};

/// Every row has exactly this height, which band selection and
/// scroll-to-cursor rely on.
pub const ROW_H: f32 = 32.0;
const ICON: u16 = 24;
const EXT_W: f32 = 64.0;
const SIZE_W: f32 = 90.0;
const DATE_W: f32 = 140.0;
const OWNER_W: f32 = 90.0;
const PERM_W: f32 = 120.0;

impl App {
    pub(super) fn column_headers(&self) -> Element<'_, Message> {
        let head = |label: &'static str, key: SortKey, width: Length| -> Element<'_, Message> {
            let arrow = match self.sort {
                (k, true) if k == key => " ▲",
                (k, false) if k == key => " ▼",
                _ => "",
            };
            widget::button::custom(widget::text::caption_heading(format!("{label}{arrow}")))
                .class(cosmic::theme::Button::Text)
                .padding([2, 0])
                .on_press(Message::Sort(key))
                .width(width)
                .into()
        };
        let mut row = widget::row::with_capacity(8)
            .spacing(12)
            .align_y(Alignment::Center)
            .push(widget::Space::new().width(ICON))
            .push(head("Name", SortKey::Name, Length::Fill))
            .push(head("Type", SortKey::Extension, Length::Fixed(EXT_W)))
            .push(head("Size", SortKey::Size, Length::Fixed(SIZE_W)))
            .push(head("Modified", SortKey::Modified, Length::Fixed(DATE_W)));
        if self.show_created {
            row = row.push(head("Created", SortKey::Created, Length::Fixed(DATE_W)));
        }
        if self.show_details {
            row = row
                .push(head("Owner", SortKey::Owner, Length::Fixed(OWNER_W)))
                .push(head("Permissions", SortKey::Permissions, Length::Fixed(PERM_W)));
        }
        widget::container(row).padding([0, 8]).into()
    }

    pub(super) fn list_view(&self, dragged: &Arc<Vec<PathBuf>>) -> Element<'_, Message> {
        self.visible()
            .enumerate()
            .fold(widget::column::with_capacity(self.entries.len()), |c, (i, e)| {
                let row = widget::container(self.row_content(e)).padding([4, 8]).width(Length::Fill).height(ROW_H);
                c.push(self.interactive(i, e, row, dragged))
            })
            .into()
    }

    fn row_content<'a>(&'a self, e: &'a Entry) -> Element<'a, Message> {
        let size = match (e.kind, e.size) {
            (_, Some(b)) => fmt::size(b),
            // Symlinked folders are never walked (their target is counted where it lives).
            (EntryKind::Dir, None) if !e.symlink && !self.trash_view => "…".into(),
            _ => String::new(),
        };

        let mut name = widget::row::with_capacity(4).spacing(6).align_y(Alignment::Center);
        if let Some(input) = self.rename_input(e) {
            return widget::row::with_capacity(2)
                .spacing(12)
                .align_y(Alignment::Center)
                .push(self.entry_icon(e, ICON))
                .push(input)
                .into();
        }
        name = name.push(widget::text::body(e.name.as_str()).wrapping(cosmic::iced::widget::text::Wrapping::None));
        if e.symlink {
            name = name.push(widget::text::caption("→ link"));
        }
        if let Some(t) = self.trash_entries.get(&e.path) {
            let from = t.original_path.parent().map(fmt::short_path).unwrap_or_default();
            let mut caption = format!("from {} · deleted {}", fmt::ellipsize_start(&from, 32), fmt::ago(t.deleted_at));
            if let Some(at) = t.purge_at {
                let days = ((at - chrono::Local::now().timestamp()) as f64 / 86_400.0).ceil().max(0.0) as i64;
                caption += &format!(" · gone in {days} day{}", if days == 1 { "" } else { "s" });
            }
            name = name.push(widget::text::caption(caption).wrapping(cosmic::iced::widget::text::Wrapping::None));
        }
        if let Some(r) = self.recent_items.get(&e.path) {
            let why = match r.kind {
                noxfm_proto::RecentKind::Downloaded => "downloaded",
                noxfm_proto::RecentKind::Modified => "edited",
                noxfm_proto::RecentKind::Created => "created",
            };
            let folder = e.path.parent().map(fmt::short_path).unwrap_or_default();
            let caption = format!("{why} {} · {}", fmt::ago(r.at), fmt::ellipsize_start(&folder, 40));
            name = name.push(widget::text::caption(caption).wrapping(cosmic::iced::widget::text::Wrapping::None));
        }
        if e.is_git {
            name = name.push(badge("git"));
        }
        if e.mime_mismatch {
            let tip = format!(
                "Content is {}, which doesn't match the .{} extension",
                e.mime.as_deref().unwrap_or("?"),
                e.extension().unwrap_or("")
            );
            name = name.push(widget::tooltip(
                icons::get(&["dialog-warning-symbolic"], 14),
                widget::text::body(tip),
                widget::tooltip::Position::Top,
            ));
        }

        let cell = |s: String, w: f32| widget::text::body(s).width(Length::Fixed(w));
        let mut row = widget::row::with_capacity(8)
            .spacing(12)
            .align_y(Alignment::Center)
            .push(self.entry_icon(e, ICON))
            // Rows have a fixed height: long names and captions are clipped, never wrapped.
            .push(widget::container(name).width(Length::Fill).clip(true))
            .push(cell(e.extension().map(str::to_lowercase).unwrap_or_default(), EXT_W))
            .push(cell(size, SIZE_W))
            .push(cell(fmt::time(e.modified), DATE_W));
        if self.show_created {
            row = row.push(cell(fmt::time(e.created), DATE_W));
        }
        if self.show_details {
            let owner = match (&e.owner, &e.group) {
                (Some(o), Some(g)) if o == g => o.clone(),
                (Some(o), Some(g)) => format!("{o}:{g}"),
                (o, _) => o.clone().unwrap_or_else(|| e.uid.to_string()),
            };
            row = row.push(cell(owner, OWNER_W)).push(cell(
                format!("{} {}", noxfm_core::perms::symbolic(e.mode), noxfm_core::perms::octal(e.mode)),
                PERM_W,
            ));
        }
        row.into()
    }
}
