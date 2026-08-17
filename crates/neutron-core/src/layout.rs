//! Recursive pane layout.
//!
//! A binary tree: leaves are tab groups, internal nodes are splits. Arbitrary
//! nesting comes free — splitting a pane replaces that leaf with a split node
//! containing the original and a new sibling, so "split the right pane
//! vertically, then split its bottom half horizontally" needs no special case.
//!
//! Closing a pane **collapses** its parent: the surviving sibling takes the
//! parent's place. Without that, closing panes would leave a tree full of
//! single-child splits, each still consuming a divider and a share of the
//! available width.
//!
//! Deliberately free of any UI type so the tree operations — which are where
//! the fiddly cases live — can be tested without a window.

use std::fmt;

/// Identifies a tab group (one leaf of the layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub u64);

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "g{}", self.0)
    }
}

/// Direction a split divides space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side, divider runs vertically.
    Horizontal,
    /// Children stacked, divider runs horizontally.
    Vertical,
}

/// Smallest fraction of a split either child may be squeezed to. Prevents a
/// divider being dragged so far that the pane it belongs to becomes
/// unrecoverable.
pub const MIN_RATIO: f32 = 0.1;

#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Leaf(GroupId),
    Split {
        axis: Axis,
        /// Fraction of the available extent given to `first`, in
        /// `MIN_RATIO..=1.0 - MIN_RATIO`.
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn leaf(id: GroupId) -> Self {
        Layout::Leaf(id)
    }

    /// Splits the pane holding `target`, placing `new_group` after it.
    ///
    /// Returns `false` if `target` is not in the tree, leaving it unchanged.
    pub fn split(&mut self, target: GroupId, axis: Axis, new_group: GroupId) -> bool {
        match self {
            Layout::Leaf(id) if *id == target => {
                let original = Layout::Leaf(*id);
                *self = Layout::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(original),
                    second: Box::new(Layout::Leaf(new_group)),
                };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { first, second, .. } => {
                first.split(target, axis, new_group) || second.split(target, axis, new_group)
            }
        }
    }

    /// Removes `target`, collapsing its parent so the sibling takes its place.
    ///
    /// Returns `None` when the whole tree is removed — the caller decides what
    /// an empty layout means (in Neutron, the window closes).
    #[must_use]
    pub fn remove(self, target: GroupId) -> Option<Layout> {
        match self {
            Layout::Leaf(id) if id == target => None,
            leaf @ Layout::Leaf(_) => Some(leaf),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (None, None) => None,
                // The collapse: a split with one surviving child *becomes* that
                // child rather than staying a split with a hole in it.
                (Some(only), None) | (None, Some(only)) => Some(only),
                (Some(a), Some(b)) => Some(Layout::Split {
                    axis,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
            },
        }
    }

    /// Sets the ratio of the split that directly contains `target` as a child.
    ///
    /// Clamped so neither side can be collapsed to nothing by a divider drag.
    pub fn set_ratio(&mut self, target: GroupId, ratio: f32) -> bool {
        if let Layout::Split {
            ratio: r,
            first,
            second,
            ..
        } = self
        {
            if matches!(**first, Layout::Leaf(id) if id == target)
                || matches!(**second, Layout::Leaf(id) if id == target)
            {
                *r = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                return true;
            }
            return first.set_ratio(target, ratio) || second.set_ratio(target, ratio);
        }
        false
    }

    /// All groups, left-to-right / top-to-bottom in visual order.
    pub fn groups(&self) -> Vec<GroupId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<GroupId>) {
        match self {
            Layout::Leaf(id) => out.push(*id),
            Layout::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, target: GroupId) -> bool {
        match self {
            Layout::Leaf(id) => *id == target,
            Layout::Split { first, second, .. } => {
                first.contains(target) || second.contains(target)
            }
        }
    }

    /// Number of leaves.
    pub fn len(&self) -> usize {
        match self {
            Layout::Leaf(_) => 1,
            Layout::Split { first, second, .. } => first.len() + second.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// The group visually after `from`, wrapping — drives "focus next pane".
    pub fn next_group(&self, from: GroupId) -> Option<GroupId> {
        let groups = self.groups();
        let pos = groups.iter().position(|g| *g == from)?;
        Some(groups[(pos + 1) % groups.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(n: u64) -> GroupId {
        GroupId(n)
    }

    #[test]
    fn splitting_a_leaf_produces_two_panes() {
        let mut l = Layout::leaf(g(1));
        assert!(l.split(g(1), Axis::Horizontal, g(2)));

        assert_eq!(l.len(), 2);
        assert_eq!(l.groups(), vec![g(1), g(2)]);
    }

    #[test]
    fn splitting_an_unknown_group_changes_nothing() {
        let mut l = Layout::leaf(g(1));
        let before = l.clone();
        assert!(!l.split(g(99), Axis::Horizontal, g(2)));
        assert_eq!(l, before);
    }

    #[test]
    fn splits_nest_arbitrarily() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));
        l.split(g(2), Axis::Vertical, g(3));
        l.split(g(3), Axis::Horizontal, g(4));

        assert_eq!(l.len(), 4);
        assert_eq!(l.groups(), vec![g(1), g(2), g(3), g(4)]);
    }

    #[test]
    fn removing_a_pane_collapses_its_parent() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));

        let l = l.remove(g(2)).expect("one pane should survive");

        // Must become a bare leaf, not a split with a single child — otherwise
        // the divider and its space allocation would persist.
        assert_eq!(l, Layout::Leaf(g(1)));
    }

    #[test]
    fn removing_from_a_nested_tree_keeps_the_rest_intact() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));
        l.split(g(2), Axis::Vertical, g(3));

        let l = l.remove(g(2)).unwrap();

        assert_eq!(l.groups(), vec![g(1), g(3)]);
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn removing_the_last_pane_empties_the_tree() {
        let l = Layout::leaf(g(1));
        assert_eq!(l.remove(g(1)), None);
    }

    #[test]
    fn removing_an_absent_group_is_a_no_op() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));
        let before = l.clone();

        assert_eq!(l.remove(g(99)), Some(before));
    }

    #[test]
    fn ratio_is_clamped_so_a_pane_cannot_vanish() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));

        l.set_ratio(g(1), 0.0);
        let Layout::Split { ratio, .. } = &l else {
            panic!("expected a split")
        };
        assert!((*ratio - MIN_RATIO).abs() < 1e-6);

        l.set_ratio(g(1), 5.0);
        let Layout::Split { ratio, .. } = &l else {
            panic!("expected a split")
        };
        assert!((*ratio - (1.0 - MIN_RATIO)).abs() < 1e-6);
    }

    #[test]
    fn focus_cycles_through_panes_and_wraps() {
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));
        l.split(g(2), Axis::Vertical, g(3));

        assert_eq!(l.next_group(g(1)), Some(g(2)));
        assert_eq!(l.next_group(g(2)), Some(g(3)));
        assert_eq!(l.next_group(g(3)), Some(g(1)), "should wrap");
        assert_eq!(l.next_group(g(99)), None);
    }

    #[test]
    fn removing_panes_one_by_one_never_leaves_a_stale_split() {
        // Build four panes, then tear them all down. Every intermediate tree
        // must have exactly as many leaves as remain.
        let mut l = Layout::leaf(g(1));
        l.split(g(1), Axis::Horizontal, g(2));
        l.split(g(2), Axis::Vertical, g(3));
        l.split(g(1), Axis::Vertical, g(4));

        let mut current = Some(l);
        for (removed, expected_len) in [(g(4), 3), (g(1), 2), (g(3), 1)] {
            current = current.unwrap().remove(removed);
            let tree = current.as_ref().expect("panes remain");
            assert_eq!(tree.len(), expected_len);
            assert!(!tree.contains(removed));
        }

        assert_eq!(current.unwrap().remove(g(2)), None);
    }
}
