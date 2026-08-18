//! Directory contents in struct-of-arrays layout.
//!
//! A folder with 500k entries is the design point. Storing entries as
//! `Vec<Entry>` would mean each sort comparison pulls a whole struct (name
//! pointer, size, timestamps, attrs) through cache to read one field. Splitting
//! into parallel arrays means a sort by size touches only the size array, which
//! is a dense `u64` run the prefetcher handles perfectly.
//!
//! Names live in one flat arena rather than 500k individual `String`
//! allocations — that alone is the difference between one allocation and half a
//! million.

use std::ops::Range;

/// Raw Win32 `FILE_ATTRIBUTE_*` bits, carried through without depending on the
/// `windows` crate so this crate still builds on Linux.
pub mod attr {
    pub const READONLY: u32 = 0x0000_0001;
    pub const HIDDEN: u32 = 0x0000_0002;
    pub const SYSTEM: u32 = 0x0000_0004;
    pub const DIRECTORY: u32 = 0x0000_0010;
    pub const ARCHIVE: u32 = 0x0000_0020;
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    pub const COMPRESSED: u32 = 0x0000_0800;
    pub const OFFLINE: u32 = 0x0000_1000;
    pub const ENCRYPTED: u32 = 0x0000_4000;
    /// Cloud placeholder: contents are not local and opening triggers a fetch.
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Directory,
    /// Reparse point that resolves to another location.
    Symlink,
    Junction,
    /// A volume root, e.g. `C:\`.
    Drive,
    /// Shell namespace item with no filesystem path — Control Panel, Network,
    /// a Drive API object. Cannot be handed to `FindFirstFileExW`.
    Virtual,
}

impl EntryKind {
    /// Whether navigating into this entry is meaningful.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            EntryKind::Directory | EntryKind::Drive | EntryKind::Junction | EntryKind::Virtual
        )
    }
}

/// Sync state for cloud-backed entries, surfaced as a sidebar/row badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    /// Not cloud-backed at all.
    #[default]
    None,
    /// Cloud-only: metadata is local, contents are not.
    CloudOnly,
    /// Contents cached locally but evictable.
    Local,
    /// Pinned: guaranteed to stay local.
    Pinned,
    Syncing,
}

/// A single entry, materialized out of the arrays for display or callers that
/// want one item. Not the storage format — see [`EntryList`].
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Milliseconds since the Unix epoch. Negative means pre-1970, which does
    /// occur on files with corrupt timestamps.
    pub modified: i64,
    pub created: i64,
    pub attrs: u32,
    pub sync: SyncState,
}

/// Columnar storage for one directory's contents.
///
/// Sorting permutes [`EntryList::order`] rather than the data arrays. Re-sorting
/// a large directory therefore moves 4 bytes per entry instead of ~60, and any
/// index a caller is holding into the data arrays stays valid across a re-sort.
#[derive(Debug, Clone)]
pub struct EntryList {
    /// All names concatenated, UTF-8. One allocation for the whole directory.
    name_arena: String,
    /// `name_offsets[i]..name_offsets[i + 1]` bounds entry `i`'s name. Always
    /// has `len() + 1` elements, so the final bound needs no special case.
    name_offsets: Vec<u32>,
    sizes: Vec<u64>,
    modified: Vec<i64>,
    created: Vec<i64>,
    attrs: Vec<u32>,
    kinds: Vec<EntryKind>,
    sync: Vec<SyncState>,
    /// Display order: indices into the arrays above.
    order: Vec<u32>,
    /// Inverse of `order`: `rank[storage_index]` is that entry's display
    /// position, or [`EntryList::HIDDEN`] if a filter excluded it.
    ///
    /// Exists so the selection cursor can be stored as a *storage* index and
    /// still be located on screen in O(1). Storing the cursor as a display
    /// position instead would silently move the selection to a different file
    /// every time the user re-sorts.
    rank: Vec<u32>,

    /// Where each entry leads, for listings whose children cannot be addressed
    /// by joining a name to the parent's path.
    ///
    /// `None` for ordinary filesystem listings, which is almost all of them —
    /// so this costs one null pointer's worth of struct, not a per-entry
    /// allocation. Populated by the shell backend, where "This PC" contains
    /// `C:\` and a Control Panel item is a CLSID with no path at all.
    targets: Option<Targets>,
}

/// Per-entry navigation targets, stored as an arena like the names.
#[derive(Debug, Default, Clone)]
struct Targets {
    arena: String,
    offsets: Vec<u32>,
    /// Whether each target is a filesystem path or a shell parsing name. Not
    /// inferable from the string: `C:\x.zip\inner` is a valid-looking path
    /// that only the shell can enumerate.
    is_path: Vec<bool>,
}

impl Default for EntryList {
    /// Not derived. `name_offsets` holds one more entry than there are names —
    /// `[i]..[i + 1]` bounds name `i` — so an empty list still needs its
    /// leading zero. A derived `Default` gives an empty vector, and the first
    /// `name()` after the first `push` reads one past the end of it.
    fn default() -> Self {
        Self::new()
    }
}

impl EntryList {
    /// Rank value for an entry excluded by the current filter.
    pub const HIDDEN: u32 = u32::MAX;

    pub fn new() -> Self {
        // Written out rather than `..Default::default()`: `Default` is now
        // this function, and the two would call each other until the stack ran
        // out — which is exactly what happened.
        Self::with_capacity(0)
    }


    /// Preallocates for `cap` entries assuming ~24 bytes per name, which is
    /// close to the real average and avoids most arena regrowth on big folders.
    pub fn with_capacity(cap: usize) -> Self {
        let mut name_offsets = Vec::with_capacity(cap + 1);
        name_offsets.push(0);
        Self {
            name_arena: String::with_capacity(cap * 24),
            name_offsets,
            sizes: Vec::with_capacity(cap),
            modified: Vec::with_capacity(cap),
            created: Vec::with_capacity(cap),
            attrs: Vec::with_capacity(cap),
            kinds: Vec::with_capacity(cap),
            sync: Vec::with_capacity(cap),
            order: Vec::with_capacity(cap),
            rank: Vec::with_capacity(cap),
            targets: None,
        }
    }

    /// Records where entry `i` leads. Must be called once per `push`, in order,
    /// for listings that need it.
    ///
    /// `is_path` distinguishes a real filesystem path from a shell parsing
    /// name, which the string alone cannot: the inside of a zip looks exactly
    /// like a directory path and is not one.
    pub fn push_target(&mut self, target: &str, is_path: bool) {
        let targets = self.targets.get_or_insert_with(|| Targets {
            arena: String::new(),
            offsets: vec![0],
            is_path: Vec::new(),
        });
        targets.arena.push_str(target);
        targets.offsets.push(targets.arena.len() as u32);
        targets.is_path.push(is_path);
    }

    /// Where entry `i` leads, as `(target, is_path)`.
    ///
    /// `None` for ordinary filesystem listings, where the caller joins the name
    /// to the parent path itself.
    pub fn target(&self, i: usize) -> Option<(&str, bool)> {
        let targets = self.targets.as_ref()?;
        // A partially populated target arena is a bug in the backend, but it
        // must degrade to "no target" rather than panic mid-listing.
        if i + 1 >= targets.offsets.len() {
            return None;
        }
        let range = targets.offsets[i] as usize..targets.offsets[i + 1] as usize;
        Some((&targets.arena[range], targets.is_path[i]))
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn push(&mut self, entry: &Entry) {
        self.name_arena.push_str(&entry.name);
        self.name_offsets.push(self.name_arena.len() as u32);
        self.sizes.push(entry.size);
        self.modified.push(entry.modified);
        self.created.push(entry.created);
        self.attrs.push(entry.attrs);
        self.kinds.push(entry.kind);
        self.sync.push(entry.sync);
        let idx = (self.kinds.len() - 1) as u32;
        self.rank.push(self.order.len() as u32);
        self.order.push(idx);
    }

    /// Name of entry `i` in *storage* order. Cheap: a slice of the arena, no
    /// allocation.
    pub fn name(&self, i: usize) -> &str {
        &self.name_arena[self.name_range(i)]
    }

    fn name_range(&self, i: usize) -> Range<usize> {
        self.name_offsets[i] as usize..self.name_offsets[i + 1] as usize
    }

    pub fn size(&self, i: usize) -> u64 {
        self.sizes[i]
    }

    pub fn modified(&self, i: usize) -> i64 {
        self.modified[i]
    }

    pub fn created(&self, i: usize) -> i64 {
        self.created[i]
    }

    pub fn attrs(&self, i: usize) -> u32 {
        self.attrs[i]
    }

    pub fn kind(&self, i: usize) -> EntryKind {
        self.kinds[i]
    }

    pub fn sync(&self, i: usize) -> SyncState {
        self.sync[i]
    }

    pub fn is_hidden(&self, i: usize) -> bool {
        self.attrs[i] & (attr::HIDDEN | attr::SYSTEM) != 0
    }

    /// Display order as storage indices. The UI walks this, slicing only the
    /// visible window.
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// Storage index of the entry at display position `pos`.
    pub fn at(&self, pos: usize) -> usize {
        self.order[pos] as usize
    }

    /// Materializes entry `i` (storage order) into an owned [`Entry`]. Allocates
    /// — for rendering, prefer the field accessors.
    pub fn get(&self, i: usize) -> Entry {
        Entry {
            name: self.name(i).to_owned(),
            kind: self.kinds[i],
            size: self.sizes[i],
            modified: self.modified[i],
            created: self.created[i],
            attrs: self.attrs[i],
            sync: self.sync[i],
        }
    }

    /// Display position of storage index `i`, or `None` when a filter has
    /// excluded it. O(1).
    pub fn rank(&self, i: usize) -> Option<usize> {
        match self.rank.get(i).copied() {
            Some(Self::HIDDEN) | None => None,
            Some(r) => Some(r as usize),
        }
    }

    /// Replaces the display order. Used by [`crate::sort`] and by filtering.
    pub fn set_order(&mut self, order: Vec<u32>) {
        debug_assert!(order.len() <= self.len());
        self.order = order;
        self.rebuild_rank();
    }

    /// Restores display order to storage order, dropping any filter.
    pub fn reset_order(&mut self) {
        self.order.clear();
        self.order.extend(0..self.len() as u32);
        self.rebuild_rank();
    }

    fn rebuild_rank(&mut self) {
        // Every entry starts hidden, so anything absent from `order` — which is
        // exactly the filtered-out set — is correctly reported as having no
        // display position.
        self.rank.clear();
        self.rank.resize(self.len(), Self::HIDDEN);
        for (pos, &idx) in self.order.iter().enumerate() {
            self.rank[idx as usize] = pos as u32;
        }
    }
}

impl FromIterator<Entry> for EntryList {
    fn from_iter<T: IntoIterator<Item = Entry>>(iter: T) -> Self {
        let it = iter.into_iter();
        let (lower, _) = it.size_hint();
        let mut list = EntryList::with_capacity(lower);
        for e in it {
            list.push(&e);
        }
        list
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    #[test]
    fn a_filesystem_listing_carries_no_targets() {
        // The overwhelmingly common case. Paying a per-entry allocation for
        // something only the shell backend needs would tax every directory.
        let mut list = EntryList::new();
        list.push(&Entry {
            name: "a.txt".into(),
            kind: EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            attrs: 0,
            sync: SyncState::None,
        });
        assert_eq!(list.target(0), None);
    }

    #[test]
    fn shell_targets_round_trip_with_their_kind() {
        // "This PC" contains `C:\`, which is a real path, and a Control Panel
        // item, which is a CLSID with no path at all. Getting the flag wrong
        // sends one down the other's backend.
        let mut list = EntryList::new();
        for (name, target, is_path) in [
            ("Local Disk (C:)", r"C:\", true),
            ("Control Panel", "::{26EE0668-A00A-44D7-9371-BEB064C98683}", false),
        ] {
            list.push(&Entry {
                name: name.into(),
                kind: EntryKind::Directory,
                size: 0,
                modified: 0,
                created: 0,
                attrs: attr::DIRECTORY,
                sync: SyncState::None,
            });
            list.push_target(target, is_path);
        }

        assert_eq!(list.target(0), Some((r"C:\", true)));
        assert_eq!(
            list.target(1),
            Some(("::{26EE0668-A00A-44D7-9371-BEB064C98683}", false))
        );
    }

    #[test]
    fn an_out_of_range_target_is_none_rather_than_a_panic() {
        // A backend that pushes entries without targets is buggy, but the
        // failure must be a missing link in one row, not a crash mid-listing.
        let mut list = EntryList::new();
        list.push(&Entry {
            name: "a".into(),
            kind: EntryKind::Directory,
            size: 0,
            modified: 0,
            created: 0,
            attrs: attr::DIRECTORY,
            sync: SyncState::None,
        });
        list.push_target("x", true);
        list.push(&Entry {
            name: "b".into(),
            kind: EntryKind::Directory,
            size: 0,
            modified: 0,
            created: 0,
            attrs: attr::DIRECTORY,
            sync: SyncState::None,
        });
        // Second entry has no target pushed.
        assert!(list.target(0).is_some());
        assert_eq!(list.target(1), None);
        assert_eq!(list.target(99), None);
    }

    #[test]
    fn targets_survive_sorting() {
        // Targets are indexed by *storage* position, like names, so re-sorting
        // the display order must not detach an entry from where it leads.
        let mut list = EntryList::new();
        for name in ["zeta", "alpha"] {
            list.push(&Entry {
                name: name.into(),
                kind: EntryKind::Directory,
                size: 0,
                modified: 0,
                created: 0,
                attrs: attr::DIRECTORY,
                sync: SyncState::None,
            });
            list.push_target(&format!("target-of-{name}"), false);
        }

        list.set_order(vec![1, 0]);
        let first = list.at(0);
        assert_eq!(list.name(first), "alpha");
        assert_eq!(list.target(first), Some(("target-of-alpha", false)));
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            size,
            modified: 0,
            created: 0,
            attrs: if kind == EntryKind::Directory {
                attr::DIRECTORY
            } else {
                0
            },
            sync: SyncState::None,
        }
    }

    #[test]
    fn arena_slices_names_back_correctly() {
        let list: EntryList = [
            entry("a.txt", EntryKind::File, 1),
            entry("bb.txt", EntryKind::File, 2),
            entry("ccc.txt", EntryKind::File, 3),
        ]
        .into_iter()
        .collect();

        assert_eq!(list.len(), 3);
        assert_eq!(list.name(0), "a.txt");
        assert_eq!(list.name(1), "bb.txt");
        assert_eq!(list.name(2), "ccc.txt");
        assert_eq!(list.size(2), 3);
    }

    #[test]
    fn non_ascii_names_survive_the_arena() {
        // Offsets are byte offsets, so multi-byte names must not corrupt
        // neighbouring slices. Emoji filenames are legal on NTFS.
        let list: EntryList = [
            entry("café.txt", EntryKind::File, 1),
            entry("🦀.rs", EntryKind::File, 2),
            entry("日本語.md", EntryKind::File, 3),
        ]
        .into_iter()
        .collect();

        assert_eq!(list.name(0), "café.txt");
        assert_eq!(list.name(1), "🦀.rs");
        assert_eq!(list.name(2), "日本語.md");
    }

    #[test]
    fn empty_list_has_valid_offset_sentinel() {
        let list = EntryList::new();
        assert!(list.is_empty());
        assert_eq!(list.order().len(), 0);
    }

    #[test]
    fn rank_inverts_the_display_order() {
        let mut list: EntryList = [
            entry("a", EntryKind::File, 0),
            entry("b", EntryKind::File, 0),
            entry("c", EntryKind::File, 0),
        ]
        .into_iter()
        .collect();

        // Reverse the display order; storage indices stay put.
        list.set_order(vec![2, 1, 0]);

        assert_eq!(list.rank(0), Some(2));
        assert_eq!(list.rank(1), Some(1));
        assert_eq!(list.rank(2), Some(0));
        assert_eq!(list.name(list.at(0)), "c");
    }

    #[test]
    fn filtered_entries_report_no_rank() {
        let mut list: EntryList = [
            entry("keep", EntryKind::File, 0),
            entry("drop", EntryKind::File, 0),
        ]
        .into_iter()
        .collect();

        list.set_order(vec![0]);

        assert_eq!(list.rank(0), Some(0));
        // Filtered out — a selection cursor on it must not resolve to a row.
        assert_eq!(list.rank(1), None);
    }

    #[test]
    fn a_default_list_is_a_usable_list() {
        // `Default` used to be derived, which left `name_offsets` empty — and
        // that vector holds one *more* entry than there are names, so the very
        // first `name()` after the first `push` read past the end.
        let mut list = EntryList::default();
        list.push(&entry("first.txt", EntryKind::File, 1));
        assert_eq!(list.name(0), "first.txt");
    }

    #[test]
    fn empty_names_do_not_break_indexing() {
        let list: EntryList = [
            entry("", EntryKind::File, 0),
            entry("after.txt", EntryKind::File, 1),
        ]
        .into_iter()
        .collect();

        assert_eq!(list.name(0), "");
        assert_eq!(list.name(1), "after.txt");
    }
}
