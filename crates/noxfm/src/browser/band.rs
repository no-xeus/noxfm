//! Rubber-band selection and item geometry.
//!
//! Both views lay items out on exact fixed sizes, so where every item sits
//! can be computed here instead of asked from the layout.

use cosmic::iced::{Point, Rectangle};
use cosmic::prelude::*;
use cosmic::widget;

use super::grid::{TILE_H, TILE_W};
use super::list::ROW_H;
use super::{App, Message, ViewMode};
use crate::selection::Selection;

/// Pointer travel before a press turns into a rubber band.
pub const BAND_THRESHOLD: f32 = 4.0;

/// A rubber band in progress. Points are in content coordinates (viewport
/// position + scroll offset).
pub struct Band {
    pub origin: Point,
    pub current: Point,
    /// Selection the band adds to (empty unless Ctrl was held).
    pub base: Selection,
    /// Moved past the threshold; until then it's still a plain click.
    pub active: bool,
}

impl Band {
    fn rect(&self) -> Rectangle {
        let (x, y) = (self.origin.x.min(self.current.x), self.origin.y.min(self.current.y));
        Rectangle::new(
            Point::new(x, y),
            cosmic::iced::Size::new((self.origin.x - self.current.x).abs(), (self.origin.y - self.current.y).abs()),
        )
    }
}

impl App {
    /// Where item `i` sits, in content coordinates.
    pub(super) fn item_rect(&self, i: usize) -> Rectangle {
        match self.view_mode {
            // Rows span the full width; only their vertical extent matters.
            ViewMode::List => Rectangle::new(Point::new(0.0, i as f32 * ROW_H), cosmic::iced::Size::new(f32::MAX, ROW_H)),
            ViewMode::Grid => {
                let cols = self.grid_cols.get();
                let (r, c) = (i / cols, i % cols);
                Rectangle::new(
                    Point::new(c as f32 * TILE_W, r as f32 * TILE_H),
                    cosmic::iced::Size::new(TILE_W, TILE_H),
                )
            }
        }
    }

    pub(super) fn start_band(&mut self, base: Selection) {
        let Some(p) = self.pointer else { return };
        let origin = self.to_content(p);
        self.band = Some(Band { origin, current: origin, base, active: false });
    }

    pub(super) fn to_content(&self, viewport_point: Point) -> Point {
        let offset = self.viewport.map_or(0.0, |(o, _)| o);
        Point::new(viewport_point.x, viewport_point.y + offset)
    }

    /// Selects every item the band touches.
    pub(super) fn apply_band(&mut self) {
        let Some(band) = &self.band else { return };
        let rect = band.rect();
        let items = self.visible_paths();
        let hit: Vec<usize> = (0..items.len()).filter(|&i| touches(&self.item_rect(i), &rect)).collect();
        let base = band.base.clone();
        self.sel.band(&items, &base, hit);
    }

    /// The band, drawn over the items in viewport coordinates.
    pub(super) fn band_rect(&self) -> Option<Element<'_, Message>> {
        let band = self.band.as_ref().filter(|b| b.active)?;
        let (offset, height) = self.viewport.unwrap_or((0.0, f32::MAX));
        let r = band.rect();
        let left = r.x.max(0.0);
        let top = (r.y - offset).max(0.0);
        let bottom = (r.y + r.height - offset).min(height);
        let right = r.x + r.width;
        if right <= left || bottom <= top {
            return None;
        }
        let rect = widget::container(widget::Space::new())
            .width(right - left)
            .height(bottom - top)
            .class(cosmic::style::Container::custom(|t| {
                let c = t.cosmic();
                let mut fill: cosmic::iced::Color = c.accent_color().into();
                fill.a = 0.15;
                widget::container::Style {
                    background: Some(fill.into()),
                    border: cosmic::iced::Border { color: c.accent_color().into(), width: 1.0, radius: 2.0.into() },
                    ..Default::default()
                }
            }));
        Some(widget::container(rect).padding(cosmic::iced::Padding { top, left, right: 0.0, bottom: 0.0 }).into())
    }
}

/// Overlap test that also counts a zero-width band (a straight vertical drag).
fn touches(item: &Rectangle, band: &Rectangle) -> bool {
    band.x <= item.x + item.width && item.x <= band.x + band.width && band.y < item.y + item.height && item.y <= band.y + band.height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap() {
        let r = |x, y, w, h| Rectangle::new(Point::new(x, y), cosmic::iced::Size::new(w, h));
        let tile = r(120.0, 136.0, 120.0, 136.0);
        assert!(touches(&tile, &r(100.0, 100.0, 30.0, 50.0)));
        assert!(touches(&tile, &r(150.0, 0.0, 0.0, 200.0)), "vertical line through the tile");
        assert!(!touches(&tile, &r(0.0, 0.0, 100.0, 100.0)));
        assert!(!touches(&tile, &r(0.0, 272.0, 400.0, 10.0)), "starts exactly below");
    }
}
