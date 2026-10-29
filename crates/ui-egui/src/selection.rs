//! Grid selection: click, Ctrl+click, Shift+click and keyboard movement, over result indices.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub items: BTreeSet<usize>,
    /// The keyboard position (also the last clicked item).
    pub cursor: Option<usize>,
    /// Where a Shift range starts.
    anchor: Option<usize>,
}

impl Selection {
    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn only(&mut self, i: usize) {
        self.items = [i].into();
        self.cursor = Some(i);
        self.anchor = Some(i);
    }

    /// A click on item `i` with the given modifiers.
    pub fn click(&mut self, i: usize, ctrl: bool, shift: bool) {
        if shift {
            let a = self.anchor.unwrap_or(i);
            let range = a.min(i)..=a.max(i);
            if !ctrl {
                self.items.clear();
            }
            self.items.extend(range);
            self.cursor = Some(i);
        } else if ctrl {
            if !self.items.remove(&i) {
                self.items.insert(i);
            }
            self.cursor = Some(i);
            self.anchor = Some(i);
        } else {
            self.only(i);
        }
    }

    /// Moves the cursor by `delta` over `n` items; Shift extends the selection.
    pub fn move_by(&mut self, delta: isize, n: usize, shift: bool) {
        if n == 0 {
            return;
        }
        let last = n - 1;
        let target = match self.cursor {
            None => 0,
            Some(c) => (c as isize + delta).clamp(0, last as isize) as usize,
        };
        self.move_to(target, shift);
    }

    pub fn move_to(&mut self, target: usize, shift: bool) {
        if shift {
            if self.anchor.is_none() {
                self.anchor = self.cursor.or(Some(target));
            }
            let a = self.anchor.unwrap_or(target);
            self.items = (a.min(target)..=a.max(target)).collect();
            self.cursor = Some(target);
        } else {
            self.only(target);
        }
    }

    pub fn select_all(&mut self, n: usize) {
        self.items = (0..n).collect();
        if self.cursor.is_none() && n > 0 {
            self.cursor = Some(0);
        }
    }

    /// Drops indices past the end (after the results shrank).
    pub fn clamp(&mut self, n: usize) {
        self.items.retain(|&i| i < n);
        if self.cursor.is_some_and(|c| c >= n) {
            self.cursor = None;
        }
        if self.anchor.is_some_and(|a| a >= n) {
            self.anchor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(s: &Selection) -> Vec<usize> {
        s.items.iter().copied().collect()
    }

    #[test]
    fn clicks() {
        let mut s = Selection::default();
        s.click(3, false, false);
        assert_eq!(items(&s), vec![3]);
        s.click(6, false, true);
        assert_eq!(items(&s), vec![3, 4, 5, 6]);
        s.click(1, false, true);
        assert_eq!(items(&s), vec![1, 2, 3], "shift range is from the anchor");
        s.click(9, true, false);
        assert_eq!(items(&s), vec![1, 2, 3, 9]);
        s.click(2, true, false);
        assert_eq!(items(&s), vec![1, 3, 9]);
        s.click(7, false, false);
        assert_eq!(items(&s), vec![7]);
    }

    #[test]
    fn keyboard() {
        let mut s = Selection::default();
        s.move_by(1, 10, false);
        assert_eq!(s.cursor, Some(0), "first move selects the first item");
        s.move_by(4, 10, false);
        assert_eq!(items(&s), vec![4]);
        s.move_by(2, 10, true);
        assert_eq!(items(&s), vec![4, 5, 6]);
        s.move_by(-4, 10, true);
        assert_eq!(items(&s), vec![2, 3, 4]);
        s.move_by(100, 10, false);
        assert_eq!(s.cursor, Some(9));
        s.move_by(-100, 10, false);
        assert_eq!(s.cursor, Some(0));
        s.select_all(5);
        assert_eq!(s.len(), 5);
        s.clamp(3);
        assert_eq!(items(&s), vec![0, 1, 2]);
        s.move_by(1, 0, false);
    }
}
