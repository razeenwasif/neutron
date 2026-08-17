//! Multi-selection with a keyboard cursor.
//!
//! Everything here is stored as **storage indices**, never display positions.
//! A user who selects three files, then clicks the Size column header, expects
//! the same three files to stay selected — storing display positions would
//! silently reassign the selection to whichever files landed in those rows.

use std::collections::HashSet;

use crate::entry::EntryList;

/// How a click or key press combines with the existing selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Plain click: this item becomes the entire selection.
    Replace,
    /// Ctrl: toggle this item, leave the rest alone.
    Toggle,
    /// Shift: select the contiguous run from the anchor to here.
    Range,
}

#[derive(Debug, Default, Clone)]
pub struct Selection {
    /// Storage indices of selected entries.
    items: HashSet<u32>,
    /// The focused entry — where the keyboard cursor sits and what the status
    /// bar describes. Not necessarily selected (Ctrl+arrow moves it alone).
    cursor: Option<u32>,
    /// Fixed end of a Shift-range. Set by any non-range selection.
    anchor: Option<u32>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_selected(&self, storage_idx: usize) -> bool {
        self.items.contains(&(storage_idx as u32))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor.map(|c| c as usize)
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.items.iter().map(|&i| i as usize)
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// Applies a selection gesture at `storage_idx`.
    ///
    /// `list` is needed for [`SelectMode::Range`], which walks *display* order
    /// between the anchor and the target — a Shift-range must select what the
    /// user sees between two rows, which is not the storage-index range.
    pub fn apply(&mut self, list: &EntryList, storage_idx: usize, mode: SelectMode) {
        let idx = storage_idx as u32;

        match mode {
            SelectMode::Replace => {
                self.items.clear();
                self.items.insert(idx);
                self.anchor = Some(idx);
            }
            SelectMode::Toggle => {
                if !self.items.remove(&idx) {
                    self.items.insert(idx);
                }
                self.anchor = Some(idx);
            }
            SelectMode::Range => {
                let anchor = self.anchor.unwrap_or(idx);
                // A range with no resolvable endpoint (either side filtered out)
                // degrades to a plain click rather than selecting nothing.
                match (list.rank(anchor as usize), list.rank(storage_idx)) {
                    (Some(a), Some(b)) => {
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        self.items.clear();
                        for pos in lo..=hi {
                            self.items.insert(list.at(pos) as u32);
                        }
                    }
                    _ => {
                        self.items.clear();
                        self.items.insert(idx);
                        self.anchor = Some(idx);
                    }
                }
            }
        }

        self.cursor = Some(idx);
    }

    /// Moves the cursor by `delta` rows in display order and selects the target.
    ///
    /// Returns the new cursor's storage index, or `None` if the list is empty.
    /// Clamps at both ends rather than wrapping — wrapping from the last file
    /// back to the first is disorienting when holding an arrow key down.
    pub fn move_cursor(&mut self, list: &EntryList, delta: isize, extend: bool) -> Option<usize> {
        let visible = list.order().len();
        if visible == 0 {
            return None;
        }

        let current_pos = self
            .cursor
            .and_then(|c| list.rank(c as usize))
            .map(|p| p as isize);

        let next_pos = match current_pos {
            Some(p) => (p + delta).clamp(0, visible as isize - 1),
            // No cursor yet: an initial Down starts at the top, an initial Up at
            // the bottom, which is what every list control does.
            None if delta >= 0 => 0,
            None => visible as isize - 1,
        } as usize;

        let target = list.at(next_pos);
        self.apply(
            list,
            target,
            if extend {
                SelectMode::Range
            } else {
                SelectMode::Replace
            },
        );
        Some(target)
    }

    /// Moves the cursor to an absolute display position (Home / End / type-ahead).
    pub fn move_to(&mut self, list: &EntryList, pos: usize, extend: bool) -> Option<usize> {
        if pos >= list.order().len() {
            return None;
        }
        let target = list.at(pos);
        self.apply(
            list,
            target,
            if extend {
                SelectMode::Range
            } else {
                SelectMode::Replace
            },
        );
        Some(target)
    }

    pub fn select_all(&mut self, list: &EntryList) {
        self.items = list.order().iter().copied().collect();
    }

    /// Total size of the selected entries, for the status bar.
    pub fn total_size(&self, list: &EntryList) -> u64 {
        self.items
            .iter()
            .filter(|&&i| !list.kind(i as usize).is_container())
            .map(|&i| list.size(i as usize))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, EntryKind, SyncState};
    use crate::sort::{SortColumn, SortOrder, SortSpec, sort};

    fn mk(name: &str, size: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind: EntryKind::File,
            size,
            modified: 0,
            created: 0,
            attrs: 0,
            sync: SyncState::None,
        }
    }

    fn list() -> EntryList {
        ["a", "b", "c", "d", "e"]
            .iter()
            .enumerate()
            .map(|(i, n)| mk(n, (i as u64 + 1) * 10))
            .collect()
    }

    #[test]
    fn replace_collapses_to_one_item() {
        let l = list();
        let mut s = Selection::new();
        s.apply(&l, 1, SelectMode::Replace);
        s.apply(&l, 3, SelectMode::Replace);

        assert_eq!(s.len(), 1);
        assert!(s.is_selected(3));
        assert_eq!(s.cursor(), Some(3));
    }

    #[test]
    fn toggle_adds_and_removes() {
        let l = list();
        let mut s = Selection::new();
        s.apply(&l, 1, SelectMode::Toggle);
        s.apply(&l, 3, SelectMode::Toggle);
        assert_eq!(s.len(), 2);

        s.apply(&l, 1, SelectMode::Toggle);
        assert_eq!(s.len(), 1);
        assert!(!s.is_selected(1));
    }

    #[test]
    fn range_selects_contiguous_display_rows() {
        let l = list();
        let mut s = Selection::new();
        s.apply(&l, 1, SelectMode::Replace);
        s.apply(&l, 3, SelectMode::Range);

        assert_eq!(s.len(), 3);
        for i in 1..=3 {
            assert!(s.is_selected(i), "{i} should be selected");
        }
    }

    #[test]
    fn range_follows_display_order_not_storage_order() {
        // This is the whole reason ranges consult `rank`. After a descending
        // sort the on-screen run between two rows is a different set of files
        // than the storage-index run between them.
        let mut l = list();
        sort(
            &mut l,
            SortSpec {
                column: SortColumn::Name,
                order: SortOrder::Descending,
                dirs_first: false,
            },
        );
        // Display order is now e, d, c, b, a — storage 4, 3, 2, 1, 0.

        let mut s = Selection::new();
        s.apply(&l, 4, SelectMode::Replace); // "e", display row 0
        s.apply(&l, 2, SelectMode::Range); // "c", display row 2

        assert_eq!(s.len(), 3);
        for i in [4, 3, 2] {
            assert!(s.is_selected(i), "storage {i} should be selected");
        }
        assert!(!s.is_selected(0), "'a' is not between 'e' and 'c' on screen");
    }

    #[test]
    fn selection_survives_a_resort() {
        let mut l = list();
        let mut s = Selection::new();
        s.apply(&l, 0, SelectMode::Replace); // "a"

        sort(
            &mut l,
            SortSpec {
                column: SortColumn::Size,
                order: SortOrder::Descending,
                dirs_first: false,
            },
        );

        // "a" is still selected and is now the last row rather than the first.
        assert!(s.is_selected(0));
        assert_eq!(l.rank(0), Some(4));
    }

    #[test]
    fn cursor_clamps_instead_of_wrapping() {
        let l = list();
        let mut s = Selection::new();
        s.apply(&l, 0, SelectMode::Replace);

        s.move_cursor(&l, -1, false);
        assert_eq!(s.cursor(), Some(0), "must not wrap past the top");

        s.move_to(&l, 4, false);
        s.move_cursor(&l, 1, false);
        assert_eq!(s.cursor(), Some(4), "must not wrap past the bottom");
    }

    #[test]
    fn first_arrow_press_with_no_cursor_enters_from_the_right_end() {
        let l = list();

        let mut down = Selection::new();
        down.move_cursor(&l, 1, false);
        assert_eq!(down.cursor(), Some(l.at(0)));

        let mut up = Selection::new();
        up.move_cursor(&l, -1, false);
        assert_eq!(up.cursor(), Some(l.at(4)));
    }

    #[test]
    fn cursor_movement_on_an_empty_list_is_a_no_op() {
        let empty = EntryList::new();
        let mut s = Selection::new();
        assert_eq!(s.move_cursor(&empty, 1, false), None);
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn total_size_ignores_directories() {
        let l: EntryList = [
            mk("f", 100),
            Entry {
                kind: EntryKind::Directory,
                ..mk("d", 999)
            },
        ]
        .into_iter()
        .collect();

        let mut s = Selection::new();
        s.apply(&l, 0, SelectMode::Toggle);
        s.apply(&l, 1, SelectMode::Toggle);

        assert_eq!(s.total_size(&l), 100);
    }
}
