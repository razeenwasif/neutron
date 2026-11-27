//! Split-pane directory comparison and smart sync helpers.

use std::collections::HashMap;

use neutron_core::EntryList;
use neutron_ui::file_list::DiffTag;

/// Two-second mtime tolerance absorbs FAT / SMB / cross-OS timestamp jitter.
pub const MTIME_TOLERANCE_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub left_only: usize,
    pub right_only: usize,
    pub left_newer: usize,
    pub right_newer: usize,
    pub different: usize,
}

#[derive(Debug, Clone, Default)]
pub struct PaneDiff {
    pub left_tags: HashMap<String, DiffTag>,
    pub right_tags: HashMap<String, DiffTag>,
    pub summary: DiffSummary,
}

pub fn compare_lists(left: &EntryList, right: &EntryList) -> PaneDiff {
    let mut left_map: HashMap<&str, usize> = HashMap::with_capacity(left.len());
    for i in 0..left.len() {
        left_map.insert(left.name(i), i);
    }
    let mut right_map: HashMap<&str, usize> = HashMap::with_capacity(right.len());
    for j in 0..right.len() {
        right_map.insert(right.name(j), j);
    }

    let mut out = PaneDiff::default();

    for (&name, &li) in &left_map {
        let Some(&ri) = right_map.get(name) else {
            out.left_tags.insert(name.to_owned(), DiffTag::OnlyHere);
            out.summary.left_only += 1;
            continue;
        };
        let l_dir = left.kind(li).is_container();
        let r_dir = right.kind(ri).is_container();
        if l_dir != r_dir {
            out.left_tags.insert(name.to_owned(), DiffTag::Different);
            out.right_tags.insert(name.to_owned(), DiffTag::Different);
            out.summary.different += 1;
            continue;
        }
        if l_dir {
            continue;
        }
        let l_mod = left.modified(li);
        let r_mod = right.modified(ri);
        if l_mod > 0 && r_mod > 0 && (l_mod - r_mod).abs() > MTIME_TOLERANCE_MS {
            if l_mod > r_mod {
                out.left_tags.insert(name.to_owned(), DiffTag::Newer);
                out.right_tags.insert(name.to_owned(), DiffTag::Older);
                out.summary.left_newer += 1;
            } else {
                out.left_tags.insert(name.to_owned(), DiffTag::Older);
                out.right_tags.insert(name.to_owned(), DiffTag::Newer);
                out.summary.right_newer += 1;
            }
        } else if left.size(li) != right.size(ri) {
            out.left_tags.insert(name.to_owned(), DiffTag::Different);
            out.right_tags.insert(name.to_owned(), DiffTag::Different);
            out.summary.different += 1;
        }
    }

    for &name in right_map.keys() {
        if !left_map.contains_key(name) {
            out.right_tags.insert(name.to_owned(), DiffTag::OnlyHere);
            out.summary.right_only += 1;
        }
    }

    out
}

pub fn sync_candidate_indices(list: &EntryList, tags: &HashMap<String, DiffTag>) -> Vec<usize> {
    (0..list.len())
        .filter(|&i| {
            matches!(
                tags.get(list.name(i)),
                Some(DiffTag::OnlyHere | DiffTag::Newer)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use neutron_core::{Entry, EntryKind, entry::SyncState};

    use super::*;

    fn file(name: &str, size: u64, modified: i64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind: EntryKind::File,
            size,
            modified,
            created: 0,
            attrs: 0,
            sync: SyncState::None,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            kind: EntryKind::Directory,
            size: 0,
            modified: 10_000,
            created: 0,
            attrs: 0,
            sync: SyncState::None,
        }
    }

    #[test]
    fn identical_lists_have_no_diffs() {
        let mut l = EntryList::default();
        let mut r = EntryList::default();
        l.push(&file("a.txt", 10, 50_000));
        l.push(&dir("src"));
        r.push(&file("a.txt", 10, 50_000));
        r.push(&dir("src"));

        let d = compare_lists(&l, &r);
        assert!(d.left_tags.is_empty());
        assert!(d.right_tags.is_empty());
        assert_eq!(d.summary, DiffSummary::default());
    }

    #[test]
    fn detects_only_left_and_only_right() {
        let mut l = EntryList::default();
        let mut r = EntryList::default();
        l.push(&file("left.txt", 10, 50_000));
        r.push(&file("right.txt", 20, 50_000));

        let d = compare_lists(&l, &r);
        assert_eq!(d.left_tags.get("left.txt"), Some(&DiffTag::OnlyHere));
        assert_eq!(d.right_tags.get("right.txt"), Some(&DiffTag::OnlyHere));
        assert_eq!(d.summary.left_only, 1);
        assert_eq!(d.summary.right_only, 1);
    }

    #[test]
    fn detects_newer_older_and_respects_two_second_tolerance() {
        let mut l = EntryList::default();
        let mut r = EntryList::default();
        l.push(&file("same_jitter.txt", 100, 51_500));
        r.push(&file("same_jitter.txt", 100, 50_000));
        l.push(&file("newer.txt", 100, 60_000));
        r.push(&file("newer.txt", 100, 50_000));

        let d = compare_lists(&l, &r);
        assert!(!d.left_tags.contains_key("same_jitter.txt"));
        assert_eq!(d.left_tags.get("newer.txt"), Some(&DiffTag::Newer));
        assert_eq!(d.right_tags.get("newer.txt"), Some(&DiffTag::Older));
        assert_eq!(d.summary.left_newer, 1);
    }

    #[test]
    fn detects_size_mismatch_and_kind_mismatch_as_different() {
        let mut l = EntryList::default();
        let mut r = EntryList::default();
        l.push(&file("data.bin", 100, 50_000));
        r.push(&file("data.bin", 200, 50_000));
        l.push(&dir("item"));
        r.push(&file("item", 12, 50_000));

        let d = compare_lists(&l, &r);
        assert_eq!(d.left_tags.get("data.bin"), Some(&DiffTag::Different));
        assert_eq!(d.left_tags.get("item"), Some(&DiffTag::Different));
        assert_eq!(d.summary.different, 2);
    }

    #[test]
    fn sync_candidates_selects_only_here_and_newer() {
        let mut l = EntryList::default();
        let mut r = EntryList::default();
        l.push(&file("only.txt", 1, 50_000));
        l.push(&file("newer.txt", 1, 60_000));
        l.push(&file("older.txt", 1, 40_000));
        r.push(&file("newer.txt", 1, 50_000));
        r.push(&file("older.txt", 1, 50_000));

        let d = compare_lists(&l, &r);
        let idxs = sync_candidate_indices(&l, &d.left_tags);
        let names: Vec<&str> = idxs.into_iter().map(|i| l.name(i)).collect();
        assert_eq!(names, vec!["only.txt", "newer.txt"]);
    }
}
