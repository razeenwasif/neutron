//! The in-memory index of one volume.
//!
//! # Shape
//!
//! Struct-of-arrays over a flat name arena, exactly like
//! [`neutron_core::EntryList`] and for the same reason: a query touches only
//! the name bytes, so keeping them contiguous means a scan is a linear walk
//! through cache rather than a pointer chase through five million allocations.
//!
//! # Paths are not stored
//!
//! Each record keeps its parent's *record index*, and a full path is rebuilt by
//! walking that chain — but only for results actually on screen. Storing
//! materialised paths for five million files would cost several hundred
//! megabytes to save a microsecond on the fifty rows a user can see.
//!
//! Parents are stored as record indices rather than as file reference numbers,
//! resolved once at build time. An FRN would need a binary search per level of
//! every path walk; an index is a direct array read.
//!
//! # Memory
//!
//! For a volume of 5M files, roughly: names ~100MB, FRNs 40MB, offsets 20MB,
//! parents 20MB, lengths 10MB — call it 190MB. That is the honest cost of
//! answering in milliseconds without touching the disk, and it is why the index
//! lives in the helper process rather than being duplicated per window.

use std::collections::HashMap;

use crate::{Frn, VolumeId};

/// Sentinel parent for a record whose parent is not in the index — the volume
/// root, or a record whose parent was filtered out.
const NO_PARENT: u32 = u32::MAX;

/// Directory flag, packed into the high bit of the length field.
const DIR_BIT: u16 = 0x8000;
const LEN_MASK: u16 = 0x7FFF;

/// NTFS caps a filename at 255 UTF-16 code units, which is at most 765 bytes of
/// UTF-8 — comfortably inside the 15 bits left after the directory flag.
pub const MAX_NAME_BYTES: usize = LEN_MASK as usize;

/// One record as it comes off the journal, before indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    pub frn: Frn,
    pub parent: Frn,
    pub name: String,
    pub is_dir: bool,
}

/// A finished, queryable index of one volume.
pub struct VolumeIndex {
    volume: VolumeId,

    /// All names concatenated, in original case. Lowercasing a second copy
    /// would double the largest array here to save a case-fold that the
    /// matcher does for free on ASCII.
    names: String,
    name_start: Vec<u32>,
    /// Length in the low 15 bits, directory flag in the high bit.
    name_meta: Vec<u16>,

    /// File reference numbers, ascending. Kept so journal deltas can find the
    /// record they refer to, and sorted so that is a binary search.
    frn: Vec<Frn>,
    /// Parent's record index, or [`NO_PARENT`].
    parent: Vec<u32>,

    /// Journal position this index is current as of. Deltas replay from here.
    pub next_usn: i64,
}

impl VolumeIndex {
    /// Builds an index from raw journal records.
    ///
    /// Records may arrive in any order and may reference parents that appear
    /// later, so parent resolution is a second pass. Anything whose parent is
    /// genuinely absent — the volume root, or a record the filter dropped —
    /// becomes a path root rather than being discarded: a file that cannot be
    /// fully qualified is still worth finding.
    pub fn build(volume: VolumeId, mut records: Vec<RawRecord>, next_usn: i64) -> Self {
        // Ascending FRN, so `frn` supports binary search.
        //
        // Checked before sorting rather than sorting unconditionally. The
        // journal enumerates in FRN order, so the check passes and the sort is
        // skipped — which measured as most of a second per million records,
        // because `sort_unstable_by_key` on `RawRecord` moves a 48-byte struct
        // containing an owned `String` on every swap. Relying on the ordering
        // without verifying it would be trusting an undocumented guarantee;
        // verifying it costs one pass.
        if !records.is_sorted_by_key(|r| r.frn) {
            tracing::debug!("journal records were not in FRN order; sorting");
            records.sort_unstable_by_key(|r| r.frn);
        }
        records.dedup_by_key(|r| r.frn);

        let count = records.len();
        let mut index = Self {
            volume,
            names: String::with_capacity(count * 24),
            name_start: Vec::with_capacity(count),
            name_meta: Vec::with_capacity(count),
            frn: Vec::with_capacity(count),
            parent: Vec::with_capacity(count),
            next_usn,
        };

        // FRN to record index, used only during this build. Dropped afterwards
        // — the resolved `parent` array replaces it, at a fifth of the size.
        //
        // With a trivial integer hasher rather than the default. FRNs are
        // already well-distributed 64-bit values, so SipHash's avalanche is
        // work done twice; over three million inserts that is measurable and
        // buys nothing, since the keys are not attacker-chosen.
        let mut by_frn: HashMap<Frn, u32, BuildFrnHasher> =
            HashMap::with_capacity_and_hasher(count, BuildFrnHasher);

        for (i, r) in records.iter().enumerate() {
            by_frn.insert(r.frn, i as u32);
            index.frn.push(r.frn);
            index.push_name(&r.name, r.is_dir);
        }

        for r in &records {
            index
                .parent
                .push(by_frn.get(&r.parent).copied().unwrap_or(NO_PARENT));
        }

        index
    }

    fn push_name(&mut self, name: &str, is_dir: bool) {
        let truncated = clamp_name(name);
        self.name_start.push(self.names.len() as u32);
        self.name_meta
            .push(truncated.len() as u16 | if is_dir { DIR_BIT } else { 0 });
        self.names.push_str(truncated);
    }

    pub fn volume(&self) -> VolumeId {
        self.volume
    }

    pub fn len(&self) -> usize {
        self.frn.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frn.is_empty()
    }

    /// Bytes held by this index, for the diagnostics the status bar shows.
    pub fn memory_bytes(&self) -> usize {
        self.names.capacity()
            + self.name_start.capacity() * 4
            + self.name_meta.capacity() * 2
            + self.frn.capacity() * 8
            + self.parent.capacity() * 4
    }

    pub fn name(&self, i: usize) -> &str {
        let start = self.name_start[i] as usize;
        let len = (self.name_meta[i] & LEN_MASK) as usize;
        &self.names[start..start + len]
    }

    pub fn is_dir(&self, i: usize) -> bool {
        self.name_meta[i] & DIR_BIT != 0
    }

    /// The record for `frn`, if the index has one.
    pub fn find(&self, frn: Frn) -> Option<usize> {
        self.frn.binary_search(&frn).ok()
    }

    /// Rebuilds the full path of record `i`.
    ///
    /// Walks the parent chain, so cost is proportional to depth rather than to
    /// index size. Called only for rows about to be displayed.
    pub fn path(&self, i: usize) -> String {
        if i >= self.len() {
            return String::new();
        }

        // Collected leaf-first then reversed, since the chain only runs upward.
        let mut parts: Vec<&str> = Vec::with_capacity(8);
        let mut current = i;

        // A corrupt journal — or a bug in delta application — can produce a
        // parent cycle. Without a bound this walk never returns, which is a
        // hang in the UI rather than a wrong path.
        for _ in 0..MAX_DEPTH {
            let name = self.name(current);
            // The volume root has an empty name. Including it would emit a
            // leading separator per level, and it contributes nothing to the
            // path — `C:` already names it.
            if !name.is_empty() {
                parts.push(name);
            }

            let next = self.parent[current];
            // NTFS records the root as its own parent. Treated as an ordinary
            // link that is an infinite loop, caught only by the depth cap —
            // which produced a path of 128 empty components.
            if next == NO_PARENT || next as usize == current {
                break;
            }
            current = next as usize;
        }

        let mut path = String::with_capacity(parts.iter().map(|p| p.len() + 1).sum::<usize>() + 3);
        path.push(self.volume.0);
        path.push(':');
        for part in parts.iter().rev() {
            path.push('\\');
            path.push_str(part);
        }
        path
    }

    /// The containing directory of record `i` — what a search result opens.
    pub fn parent_path(&self, i: usize) -> String {
        match self.parent.get(i).copied() {
            Some(p) if p != NO_PARENT => self.path(p as usize),
            _ => format!("{}:\\", self.volume.0),
        }
    }
}

/// Hasher for file reference numbers.
///
/// FRNs are sequence numbers packed with a record index — dense, distinct, and
/// entirely under the filesystem's control rather than an attacker's. All that
/// is needed is to spread the low bits across the bucket space, which one
/// multiply does. The default `SipHash` is a cryptographic mixer sized for
/// hostile keys, and at three million inserts its cost is real.
#[derive(Clone, Copy, Default)]
struct BuildFrnHasher;

impl std::hash::BuildHasher for BuildFrnHasher {
    type Hasher = FrnHasher;
    fn build_hasher(&self) -> FrnHasher {
        FrnHasher(0)
    }
}

#[derive(Default)]
struct FrnHasher(u64);

impl std::hash::Hasher for FrnHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only ever fed `u64` keys through `write_u64`. A byte-slice key would
        // silently hash to a constant, so this refuses rather than degrading
        // every lookup to a linear probe.
        debug_assert!(bytes.is_empty(), "FrnHasher only hashes u64 keys");
    }

    fn write_u64(&mut self, value: u64) {
        // Fibonacci hashing: multiply by 2^64 / φ and let the high bits fall
        // into the bucket index.
        self.0 = value.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// Deepest parent chain a path walk will follow before giving up.
///
/// NTFS has no hard depth limit, but 128 is far past anything real; the number
/// exists to bound a cycle, not to describe filesystems.
const MAX_DEPTH: usize = 128;

/// Truncates to [`MAX_NAME_BYTES`] on a character boundary.
fn clamp_name(name: &str) -> &str {
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let mut end = MAX_NAME_BYTES;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, parent: u64, name: &str, is_dir: bool) -> RawRecord {
        RawRecord {
            frn,
            parent,
            name: name.to_owned(),
            is_dir,
        }
    }

    /// `C:\Users\Razeen\notes.txt`, plus a stray file at the root.
    fn sample() -> VolumeIndex {
        VolumeIndex::build(
            VolumeId('C'),
            vec![
                rec(5, 5, "", true), // the volume root refers to itself
                rec(10, 5, "Users", true),
                rec(20, 10, "Razeen", true),
                rec(30, 20, "notes.txt", false),
                rec(40, 5, "pagefile.sys", false),
            ],
            0,
        )
    }

    #[test]
    fn paths_are_rebuilt_from_the_parent_chain() {
        let idx = sample();
        let notes = idx.find(30).expect("record present");
        assert_eq!(idx.path(notes), r"C:\Users\Razeen\notes.txt");

        let root_file = idx.find(40).expect("record present");
        assert_eq!(idx.path(root_file), r"C:\pagefile.sys");
    }

    #[test]
    fn the_parent_path_is_what_a_result_opens() {
        let idx = sample();
        let notes = idx.find(30).unwrap();
        assert_eq!(idx.parent_path(notes), r"C:\Users\Razeen");
    }

    #[test]
    fn records_are_findable_by_reference_number() {
        let idx = sample();
        assert!(idx.find(20).is_some());
        // Deleted or never-seen FRNs must miss rather than return a neighbour,
        // which is the failure mode of a bad binary search.
        assert!(idx.find(21).is_none());
        assert!(idx.find(u64::MAX).is_none());
    }

    #[test]
    fn a_self_referential_root_terminates() {
        // The NTFS root's parent is itself. Walked as an ordinary link that is
        // an infinite loop, bounded only by the depth cap — which produced
        // `C:` followed by 128 separators, and passed an earlier version of
        // this test that only checked the length was not absurd.
        let idx = sample();
        let root = idx.find(5).unwrap();
        assert_eq!(idx.path(root), "C:");
    }

    #[test]
    fn a_parent_cycle_terminates() {
        // Not reachable from a healthy journal, but a delta applied wrongly
        // could produce one, and a hang in the UI is far worse than a wrong
        // path.
        let idx = VolumeIndex::build(
            VolumeId('C'),
            vec![rec(1, 2, "a", true), rec(2, 1, "b", true)],
            0,
        );
        let path = idx.path(0);
        assert!(path.len() < 1000, "cycle was not bounded");
    }

    #[test]
    fn an_orphan_becomes_a_root_rather_than_disappearing() {
        // A file whose parent is missing is still worth finding — losing it
        // silently is worse than showing a short path.
        let idx = VolumeIndex::build(VolumeId('D'), vec![rec(9, 999, "lost.txt", false)], 0);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.path(0), r"D:\lost.txt");
    }

    #[test]
    fn directories_are_distinguished_from_files() {
        let idx = sample();
        assert!(idx.is_dir(idx.find(10).unwrap()));
        assert!(!idx.is_dir(idx.find(30).unwrap()));
    }

    #[test]
    fn records_are_deduplicated_by_reference_number() {
        // The journal can hand back the same FRN twice across a resumed
        // enumeration. Two records for one file means duplicate search hits.
        let idx = VolumeIndex::build(
            VolumeId('C'),
            vec![rec(7, 5, "a.txt", false), rec(7, 5, "a.txt", false)],
            0,
        );
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn unsorted_input_is_still_searchable() {
        // `find` is a binary search, so build must not depend on the journal's
        // ordering being what the documentation implies.
        let idx = VolumeIndex::build(
            VolumeId('C'),
            vec![
                rec(30, 10, "z.txt", false),
                rec(10, 5, "Users", true),
                rec(5, 5, "", true),
            ],
            0,
        );
        assert_eq!(idx.path(idx.find(30).unwrap()), r"C:\Users\z.txt");
    }

    #[test]
    fn names_are_stored_in_their_original_case() {
        // Search folds case, but results are displayed — showing a lowercased
        // filename would be wrong on every screen it appears.
        let idx = VolumeIndex::build(
            VolumeId('C'),
            vec![rec(1, 1, "ReadMe.MD", false)],
            0,
        );
        assert_eq!(idx.name(0), "ReadMe.MD");
    }

    #[test]
    fn non_ascii_names_survive() {
        let idx = VolumeIndex::build(
            VolumeId('C'),
            vec![rec(1, 1, "日本語 🦀.txt", false)],
            0,
        );
        assert_eq!(idx.name(0), "日本語 🦀.txt");
    }

    #[test]
    fn an_over_long_name_is_truncated_on_a_character_boundary() {
        // The length field has 15 bits. Truncating mid-character would panic on
        // the next slice; this is the guard for that.
        let long = "🦀".repeat(MAX_NAME_BYTES);
        let idx = VolumeIndex::build(VolumeId('C'), vec![rec(1, 1, &long, false)], 0);
        // Reading it back must not panic, which is the real assertion.
        assert!(idx.name(0).len() <= MAX_NAME_BYTES);
        assert!(idx.name(0).chars().all(|c| c == '🦀'));
    }

    #[test]
    fn an_empty_index_answers_rather_than_panicking() {
        let idx = VolumeIndex::build(VolumeId('C'), Vec::new(), 0);
        assert!(idx.is_empty());
        assert_eq!(idx.path(0), "");
        assert_eq!(idx.find(1), None);
    }
}
