//! Icon (grid) view: fixed-size tiles, so thumbnails are big enough to see.

use std::path::PathBuf;
use std::sync::Arc;

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget;
use noxfm_proto::Entry;

use super::{App, Message, badge};

/// Tiles are laid out on an exact grid, which band selection relies on.
pub const TILE_W: f32 = 120.0;
pub const TILE_H: f32 = 136.0;
const ICON: u16 = 80;

pub fn columns(width: f32) -> usize {
    ((width / TILE_W).floor() as usize).max(1)
}

impl App {
    pub(super) fn grid_view(&self, dragged: &Arc<Vec<PathBuf>>) -> Element<'_, Message> {
        let cols = self.grid_cols.get();
        let entries: Vec<(usize, &Entry)> = self.visible().enumerate().collect();
        entries
            .chunks(cols)
            .fold(widget::column::with_capacity(entries.len() / cols + 1), |c, chunk| {
                let row = chunk.iter().fold(widget::row::with_capacity(cols), |r, &(i, e)| {
                    r.push(self.interactive(i, e, self.tile(e), dragged))
                });
                c.push(row)
            })
            .into()
    }

    fn tile<'a>(&'a self, e: &'a Entry) -> Element<'a, Message> {
        let label: Element<'a, Message> = match self.rename_input(e) {
            Some(input) => input,
            None => widget::text::body(e.name.as_str()).align_x(Alignment::Center).width(Length::Fill).into(),
        };
        let mut caption = widget::column::with_capacity(2).align_x(Alignment::Center).push(label);
        if e.is_git {
            caption = caption.push(badge("git"));
        }
        let content = widget::column::with_capacity(2)
            .spacing(4)
            .align_x(Alignment::Center)
            .push(
                widget::container(self.entry_icon(e, ICON))
                    .width(Length::Fill)
                    .height(ICON as f32 + 4.0)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            // Long names are clipped to about two lines.
            .push(widget::container(caption).height(Length::Fixed(40.0)).clip(true));
        widget::container(content).padding(6).width(TILE_W).height(TILE_H).into()
    }
}
