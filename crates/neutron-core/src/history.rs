//! Per-tab back/forward navigation.

use crate::namespace::NodeId;

/// Browser-style navigation history.
///
/// Bounded because a long-lived tab would otherwise accumulate PIDLs
/// indefinitely; when the cap is hit the oldest entry is dropped, which is what
/// browsers do and nobody notices.
#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<NodeId>,
    /// Index of the current location within `entries`.
    cursor: usize,
    cap: usize,
}

impl History {
    const DEFAULT_CAP: usize = 256;

    pub fn new(initial: NodeId) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
            cap: Self::DEFAULT_CAP,
        }
    }

    pub fn current(&self) -> &NodeId {
        &self.entries[self.cursor]
    }

    /// Navigates to `id`, discarding any forward history.
    ///
    /// Navigating to the location already shown is a no-op, so double-clicking
    /// or a redundant sidebar click does not fill history with duplicates.
    pub fn push(&mut self, id: NodeId) {
        if *self.current() == id {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(id);

        if self.entries.len() > self.cap {
            // Drop the oldest; the cursor shifts down with it.
            let overflow = self.entries.len() - self.cap;
            self.entries.drain(0..overflow);
            self.cursor = self.cursor.saturating_sub(overflow);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<&NodeId> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        Some(&self.entries[self.cursor])
    }

    pub fn forward(&mut self) -> Option<&NodeId> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        Some(&self.entries[self.cursor])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> NodeId {
        NodeId::path(s)
    }

    #[test]
    fn back_and_forward_walk_the_stack() {
        let mut h = History::new(p("/a"));
        h.push(p("/b"));
        h.push(p("/c"));

        assert_eq!(h.back(), Some(&p("/b")));
        assert_eq!(h.back(), Some(&p("/a")));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(&p("/b")));
        assert_eq!(h.current(), &p("/b"));
    }

    #[test]
    fn navigating_after_back_discards_forward_history() {
        let mut h = History::new(p("/a"));
        h.push(p("/b"));
        h.push(p("/c"));
        h.back();
        h.push(p("/d"));

        assert!(!h.can_go_forward());
        assert_eq!(h.current(), &p("/d"));
        assert_eq!(h.back(), Some(&p("/b")));
    }

    #[test]
    fn repeat_navigation_to_current_is_ignored() {
        let mut h = History::new(p("/a"));
        h.push(p("/b"));
        h.push(p("/b"));
        h.push(p("/b"));

        assert_eq!(h.back(), Some(&p("/a")));
        assert!(!h.can_go_back());
    }

    #[test]
    fn overflow_drops_oldest_and_keeps_cursor_valid() {
        let mut h = History::new(p("/0"));
        h.cap = 4;
        for i in 1..10 {
            h.push(p(&format!("/{i}")));
        }

        assert_eq!(h.entries.len(), 4);
        assert_eq!(h.current(), &p("/9"));
        // Cursor must still point at the last entry after draining.
        assert_eq!(h.cursor, 3);
        assert!(h.can_go_back());
    }
}
