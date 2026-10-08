//! Multi-selection over a listing, keyed by path so it survives re-sorts and
//! live reloads.
//!
//! - click: select only that item
//! - Ctrl+click: toggle it
//! - Shift+click: select from the anchor to it
//! - Ctrl+Shift+click: add that range to the selection
//! - arrows: move the cursor (with Shift, extend from the anchor)
//! - rubber band: the rows a dragged rectangle covers (with Ctrl, added to
//!   what was selected when the drag began)

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct Selection {
    set: HashSet<PathBuf>,
    /// Fixed end of shift-ranges.
    anchor: Option<PathBuf>,
    /// Where the keyboard is; moves with arrows.
    cursor: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

pub enum Move {
    By(isize),
    First,
    Last,
}

impl Selection {
    pub fn contains(&self, p: &Path) -> bool {
        self.set.contains(p)
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn cursor(&self) -> Option<&Path> {
        self.cursor.as_deref()
    }

    /// Selected paths in listing order.
    pub fn paths(&self, items: &[impl AsRef<Path>]) -> Vec<PathBuf> {
        items.iter().map(AsRef::as_ref).filter(|p| self.set.contains(*p)).map(Path::to_path_buf).collect()
    }

    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    pub fn click(&mut self, items: &[impl AsRef<Path>], i: usize, mods: Mods) {
        let Some(p) = items.get(i).map(AsRef::as_ref) else { return };
        match (mods.ctrl, mods.shift, self.anchor_index(items)) {
            (_, true, Some(a)) => {
                if !mods.ctrl {
                    self.set.clear();
                }
                self.set.extend(range(items, a, i));
                self.cursor = Some(p.to_path_buf());
            }
            (true, _, _) => {
                if !self.set.remove(p) {
                    self.set.insert(p.to_path_buf());
                }
                self.anchor = Some(p.to_path_buf());
                self.cursor = Some(p.to_path_buf());
            }
            _ => self.only(p),
        }
    }

    pub fn move_cursor(&mut self, items: &[impl AsRef<Path>], how: Move, extend: bool) {
        if items.is_empty() {
            return;
        }
        let last = items.len() - 1;
        let at = self.cursor.as_deref().and_then(|c| items.iter().position(|p| p.as_ref() == c));
        let to = match (how, at) {
            (Move::First, _) => 0,
            (Move::Last, _) => last,
            (Move::By(d), Some(i)) => i.saturating_add_signed(d).min(last),
            // Nothing focused yet: Down starts at the top, Up at the bottom.
            (Move::By(d), None) => if d < 0 { last } else { 0 },
        };
        match (extend, self.anchor_index(items)) {
            (true, Some(a)) => {
                self.set = range(items, a, to).collect();
                self.cursor = Some(items[to].as_ref().to_path_buf());
            }
            _ => self.only(items[to].as_ref()),
        }
    }

    pub fn select_all(&mut self, items: &[impl AsRef<Path>]) {
        self.set = items.iter().map(|p| p.as_ref().to_path_buf()).collect();
    }

    /// Items swept by a rubber band (indices into `items`; out-of-range ones
    /// are ignored), on top of `base`: the selection when the band started,
    /// empty unless Ctrl was held.
    pub fn band(&mut self, items: &[impl AsRef<Path>], base: &Selection, picked: impl IntoIterator<Item = usize>) {
        *self = base.clone();
        let picked: Vec<usize> = picked.into_iter().filter(|&i| i < items.len()).collect();
        let (Some(&first), Some(&last)) = (picked.iter().min(), picked.iter().max()) else { return };
        self.set.extend(picked.iter().map(|&i| items[i].as_ref().to_path_buf()));
        self.anchor = Some(items[first].as_ref().to_path_buf());
        self.cursor = Some(items[last].as_ref().to_path_buf());
    }

    /// Forget paths that are no longer listed.
    pub fn retain(&mut self, items: &[impl AsRef<Path>]) {
        let present: HashSet<&Path> = items.iter().map(AsRef::as_ref).collect();
        self.set.retain(|p| present.contains(p.as_path()));
        for slot in [&mut self.anchor, &mut self.cursor] {
            if slot.as_deref().is_some_and(|p| !present.contains(p)) {
                *slot = None;
            }
        }
    }

    fn only(&mut self, p: &Path) {
        self.set = HashSet::from([p.to_path_buf()]);
        self.anchor = Some(p.to_path_buf());
        self.cursor = Some(p.to_path_buf());
    }

    fn anchor_index(&self, items: &[impl AsRef<Path>]) -> Option<usize> {
        let a = self.anchor.as_deref()?;
        items.iter().position(|p| p.as_ref() == a)
    }
}

fn range(items: &[impl AsRef<Path>], a: usize, b: usize) -> impl Iterator<Item = PathBuf> + '_ {
    items[a.min(b)..=a.max(b)].iter().map(|p| p.as_ref().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Mods = Mods { ctrl: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true };
    const BOTH: Mods = Mods { ctrl: true, shift: true };

    fn names(s: &Selection, items: &[&Path]) -> Vec<String> {
        s.paths(items).iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn mouse() {
        let items: Vec<&Path> = ["a", "b", "c", "d", "e"].map(Path::new).to_vec();
        let mut s = Selection::default();
        s.click(&items, 1, Mods::default());
        s.click(&items, 3, SHIFT);
        assert_eq!(names(&s, &items), ["b", "c", "d"]);
        s.click(&items, 0, SHIFT); // range re-pivots on the same anchor
        assert_eq!(names(&s, &items), ["a", "b"]);
        s.click(&items, 4, CTRL);
        assert_eq!(names(&s, &items), ["a", "b", "e"]);
        s.click(&items, 4, CTRL);
        assert_eq!(names(&s, &items), ["a", "b"]);
        s.click(&items, 2, Mods::default());
        s.click(&items, 4, BOTH);
        s.click(&items, 0, CTRL);
        assert_eq!(names(&s, &items), ["a", "c", "d", "e"]);
    }

    #[test]
    fn keyboard() {
        let items: Vec<&Path> = ["a", "b", "c", "d"].map(Path::new).to_vec();
        let mut s = Selection::default();
        s.move_cursor(&items, Move::By(1), false);
        assert_eq!(names(&s, &items), ["a"]);
        s.move_cursor(&items, Move::By(1), true);
        s.move_cursor(&items, Move::By(1), true);
        assert_eq!(names(&s, &items), ["a", "b", "c"]);
        s.move_cursor(&items, Move::By(-1), true);
        assert_eq!(names(&s, &items), ["a", "b"]);
        s.move_cursor(&items, Move::By(10), false);
        assert_eq!(names(&s, &items), ["d"]);
        s.move_cursor(&items, Move::First, true);
        assert_eq!(names(&s, &items), ["a", "b", "c", "d"]);
    }

    #[test]
    fn rubber_band() {
        let items: Vec<&Path> = ["a", "b", "c", "d", "e"].map(Path::new).to_vec();
        let mut base = Selection::default();
        base.click(&items, 4, Mods::default());

        let mut s = Selection::default();
        s.band(&items, &Selection::default(), 1..=2);
        assert_eq!(names(&s, &items), ["b", "c"]);
        // Shrinking the band un-selects rows it no longer covers.
        s.band(&items, &Selection::default(), [1]);
        assert_eq!(names(&s, &items), ["b"]);
        // With Ctrl the band adds to the earlier selection.
        s.band(&items, &base, 0..=1);
        assert_eq!(names(&s, &items), ["a", "b", "e"]);
        // Past the last item: ignored. Empty space only: nothing new.
        s.band(&items, &Selection::default(), 3..=99);
        assert_eq!(names(&s, &items), ["d", "e"]);
        s.band(&items, &Selection::default(), []);
        assert!(s.is_empty());
        // Shift+arrows continue from the band.
        // Non-contiguous, as a grid band produces.
        s.band(&items, &Selection::default(), [0, 2, 4]);
        assert_eq!(names(&s, &items), ["a", "c", "e"]);
        s.band(&items, &Selection::default(), 1..=2);
        s.move_cursor(&items, Move::By(1), true);
        assert_eq!(names(&s, &items), ["b", "c", "d"]);
    }

    #[test]
    fn survives_reload() {
        let items: Vec<&Path> = ["a", "b", "c"].map(Path::new).to_vec();
        let mut s = Selection::default();
        s.select_all(&items);
        s.click(&items, 1, CTRL);
        let after: Vec<&Path> = ["c", "a", "z"].map(Path::new).to_vec();
        s.retain(&after);
        assert_eq!(names(&s, &after), ["c", "a"]);
        assert_eq!(s.cursor(), None, "b was the cursor and is gone");
    }
}
