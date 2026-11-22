//! Which items a selection rectangle covers. Only tiles on screen exist as widgets, so what
//! was covered is remembered while scrolling, and rows skipped by a fast scroll are filled in.

use std::collections::{BTreeSet, HashMap};

#[derive(Default)]
pub struct Rubber {
    /// Selected before the drag (kept when Ctrl or Shift is held)
    base: BTreeSet<u32>,
    covered: HashMap<u32, bool>,
}

impl Rubber {
    pub fn new(base: impl IntoIterator<Item = u32>) -> Rubber {
        Rubber { base: base.into_iter().collect(), covered: HashMap::new() }
    }

    /// What's on screen right now, and whether the rectangle touches it.
    pub fn see(&mut self, items: impl IntoIterator<Item = (u32, bool)>) {
        self.covered.extend(items);
    }

    /// The list changed under us: positions mean something else now.
    pub fn forget(&mut self) {
        self.base.clear();
        self.covered.clear();
    }

    pub fn selected(&self, cols: u32, n: u32) -> BTreeSet<u32> {
        let cols = cols.max(1);
        let hit: BTreeSet<u32> = self.covered.iter().filter(|(p, c)| **c && **p < n).map(|(p, _)| *p).collect();
        let mut out: BTreeSet<u32> = self.base.iter().copied().filter(|p| *p < n).collect();
        out.extend(&hit);
        // a rectangle covers the same columns in every row between its first and last
        if let (Some(a), Some(b)) = (hit.first(), hit.last()) {
            let columns: BTreeSet<u32> = hit.iter().map(|p| p % cols).collect();
            for row in a / cols..=b / cols {
                out.extend(columns.iter().map(|c| row * cols + c).filter(|p| *p < n && self.covered.get(p) != Some(&false)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(r: &Rubber, cols: u32, n: u32) -> Vec<u32> {
        r.selected(cols, n).into_iter().collect()
    }

    #[test]
    fn grows_and_shrinks_live() {
        let mut r = Rubber::new([]);
        r.see([(0, false), (1, true), (4, false), (5, true)]);
        assert_eq!(v(&r, 4, 20), [1, 5]);
        r.see([(1, true), (2, true), (5, true), (6, true)]);
        assert_eq!(v(&r, 4, 20), [1, 2, 5, 6]);
        r.see([(5, false), (6, false)]);
        assert_eq!(v(&r, 4, 20), [1, 2]);
    }

    #[test]
    fn remembers_scrolled_away_and_fills_gaps() {
        let mut r = Rubber::new([]);
        r.see([(1, true), (2, true)]);
        // scrolled so fast that rows 1..4 were never on screen
        r.see([(21, true), (22, true)]);
        assert_eq!(v(&r, 4, 100), [1, 2, 5, 6, 9, 10, 13, 14, 17, 18, 21, 22]);
        assert_eq!(v(&r, 4, 6), [1, 2]); // never past the end
        let mut l = Rubber::new([]);
        l.see([(3, true), (6, true)]);
        assert_eq!(v(&l, 1, 9), [3, 4, 5, 6]);
    }

    #[test]
    fn keeps_the_old_selection_when_adding() {
        let mut r = Rubber::new([9]);
        r.see([(0, true), (9, false)]);
        assert_eq!(v(&r, 4, 20), [0, 9]);
        r.forget();
        assert!(v(&r, 4, 20).is_empty());
    }
}
