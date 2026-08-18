//! Sorting for directory listings.
//!
//! Sorts permute the display-order index vector, never the data arrays. On a
//! 500k-entry directory that means shuffling 2MB instead of ~30MB.

use rayon::prelude::*;
use std::cmp::Ordering;

use crate::entry::EntryList;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
    Created,
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn flipped(self) -> Self {
        match self {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        }
    }

    fn apply(self, ord: Ordering) -> Ordering {
        match self {
            SortOrder::Ascending => ord,
            SortOrder::Descending => ord.reverse(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub column: SortColumn,
    pub order: SortOrder,
    /// Group containers above files regardless of column, as Explorer does.
    /// Applied before the column comparison, so it is never inverted by
    /// `Descending` — users expect folders to stay on top either way.
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            order: SortOrder::Ascending,
            dirs_first: true,
        }
    }
}

/// Case-insensitive comparison that reads digit runs as numbers, so `file2`
/// sorts before `file10`. Explorer does this and users notice immediately when
/// it is missing.
///
/// Operates on `char`s rather than bytes because case folding and digit
/// detection both need them; the common all-ASCII path is still fast because
/// `char_indices` over ASCII is a byte walk.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);

    loop {
        match (ab.get(i).copied(), bb.get(j).copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,

            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                // Compare the digit runs numerically, as slices of the original
                // names. An earlier version copied each run into a `String`,
                // which put a heap allocation inside the sort's innermost loop
                // — and directory names full of version numbers and hashes,
                // which is exactly what a big folder looks like, hit it on
                // nearly every comparison.
                let a_run = digits_from(ab, &mut i);
                let b_run = digits_from(bb, &mut j);

                // By significant-digit count first, so arbitrarily long runs
                // compare correctly without overflowing an integer.
                let a_sig = strip_zeros(a_run);
                let b_sig = strip_zeros(b_run);

                match a_sig.len().cmp(&b_sig.len()).then_with(|| a_sig.cmp(b_sig)) {
                    Ordering::Equal => {
                        // Numerically equal. Fewer leading zeros first, so
                        // `file01` and `file1` have a stable, total order
                        // rather than comparing equal.
                        match a_run.len().cmp(&b_run.len()) {
                            Ordering::Equal => continue,
                            other => return other,
                        }
                    }
                    other => return other,
                }
            }

            (Some(x), Some(y)) if x.is_ascii() && y.is_ascii() => {
                // The overwhelmingly common case, and the reason this walks
                // bytes rather than chars: no UTF-8 decoding at all.
                let (al, bl) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
                if al != bl {
                    return al.cmp(&bl);
                }
                // Same letter, different case: move on. Ties are broken by the
                // caller's total-order fallback.
                i += 1;
                j += 1;
            }

            (Some(_), Some(_)) => {
                // At least one side is non-ASCII. Decode a character from each
                // and fold properly. `i` and `j` only ever advance by whole
                // characters, so they are always on a boundary here.
                let ac = a[i..].chars().next().unwrap_or('\u{fffd}');
                let bc = b[j..].chars().next().unwrap_or('\u{fffd}');
                let (al, bl) = (lower(ac), lower(bc));
                if al != bl {
                    return al.cmp(&bl);
                }
                i += ac.len_utf8();
                j += bc.len_utf8();
            }
        }
    }
}

fn lower(c: char) -> char {
    // Fast path: ASCII is the overwhelming majority of filenames and
    // `to_lowercase` allocates an iterator we do not need.
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The run of digits starting at `*at`, advancing `*at` past it.
fn digits_from<'a>(bytes: &'a [u8], at: &mut usize) -> &'a [u8] {
    let start = *at;
    while *at < bytes.len() && bytes[*at].is_ascii_digit() {
        *at += 1;
    }
    &bytes[start..*at]
}

/// `digits` without its leading zeros.
fn strip_zeros(digits: &[u8]) -> &[u8] {
    let first = digits
        .iter()
        .position(|&d| d != b'0')
        .unwrap_or(digits.len());
    &digits[first..]
}

/// Rebuilds the display order from scratch: filters, then sorts.
///
/// Separate from [`sort`] because toggling hidden files must reconsider entries
/// the current order has already excluded — sorting alone can never bring a
/// filtered-out entry back.
pub fn apply(list: &mut EntryList, spec: SortSpec, show_hidden: bool) {
    apply_filtered(list, spec, show_hidden, "")
}

/// As [`apply`], but also keeping only entries whose name contains `needle`.
///
/// Case-insensitive substring, not fuzzy: this backs the filter field in the
/// pane header, where the user is narrowing a listing they can already see and
/// expects "doc" to hide everything without "doc" in it. Fuzzy matching belongs
/// to the finder overlay, where the user is searching for something they cannot
/// see and a loose match is a help rather than a surprise.
///
/// An empty needle matches everything, so clearing the field restores the full
/// listing without a reload.
pub fn apply_filtered(list: &mut EntryList, spec: SortSpec, show_hidden: bool, needle: &str) {
    // Lowercased once here rather than per entry — the comparison runs over
    // every name in the directory on each keystroke.
    let needle = needle.trim().to_lowercase();

    let order: Vec<u32> = (0..list.len() as u32)
        .filter(|&i| {
            let i = i as usize;
            if !show_hidden && list.is_hidden(i) {
                return false;
            }
            // Not `name.to_lowercase().contains(..)`: that allocates a
            // lowercased copy of every name in the directory, on every
            // keystroke. The shared matcher folds case as it compares.
            crate::text::contains_ignore_ascii_case(list.name(i), &needle)
        })
        .collect();

    list.set_order(order);
    sort(list, spec);
}

/// Sorts `list`'s current display order in place, preserving any filter.
///
/// Uses an unstable parallel sort with an explicit index tiebreak, which gives
/// a total order (so the result is deterministic) without paying for a stable
/// sort's allocation.
pub fn sort(list: &mut EntryList, spec: SortSpec) {
    let mut order: Vec<u32> = list.order().to_vec();

    order.par_sort_unstable_by(|&a, &b| {
        let (ai, bi) = (a as usize, b as usize);

        if spec.dirs_first {
            let (ad, bd) = (
                list.kind(ai).is_container(),
                list.kind(bi).is_container(),
            );
            if ad != bd {
                // Containers first, independent of sort direction.
                return if ad { Ordering::Less } else { Ordering::Greater };
            }
        }

        let ord = match spec.column {
            SortColumn::Name => natural_cmp(list.name(ai), list.name(bi)),
            SortColumn::Size => list.size(ai).cmp(&list.size(bi)),
            SortColumn::Modified => list.modified(ai).cmp(&list.modified(bi)),
            SortColumn::Created => list.created(ai).cmp(&list.created(bi)),
            SortColumn::Kind => extension(list.name(ai))
                .cmp(extension(list.name(bi)))
                .then_with(|| natural_cmp(list.name(ai), list.name(bi))),
        };

        spec.order.apply(ord).then_with(|| {
            // Total-order tiebreak: two entries can compare equal on every
            // sorted field (same size, same timestamp), and without this an
            // unstable sort would reorder them between frames.
            natural_cmp(list.name(ai), list.name(bi)).then(a.cmp(&b))
        })
    });

    list.set_order(order);
}

/// Lowercased extension, or `""` when there is none. Dotfiles like `.gitignore`
/// are treated as having no extension, matching Explorer.
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i + 1..],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, EntryKind, SyncState, attr};

    fn mk(name: &str, kind: EntryKind, size: u64, modified: i64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            size,
            modified,
            created: 0,
            attrs: if kind == EntryKind::Directory {
                attr::DIRECTORY
            } else {
                0
            },
            sync: SyncState::None,
        }
    }

    fn names(list: &EntryList) -> Vec<&str> {
        list.order().iter().map(|&i| list.name(i as usize)).collect()
    }

    #[test]
    fn digits_compare_numerically_not_lexically() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("file10", "file2"), Ordering::Greater);
        assert_eq!(natural_cmp("a1b2", "a1b10"), Ordering::Less);
    }

    #[test]
    fn comparison_is_case_insensitive() {
        assert_eq!(natural_cmp("Apple", "apple"), Ordering::Equal);
        assert_eq!(natural_cmp("Apple", "banana"), Ordering::Less);
        assert_eq!(natural_cmp("BANANA", "apple"), Ordering::Greater);
    }

    #[test]
    fn leading_zeros_give_a_total_order() {
        // Numerically equal, so they must not compare Equal or an unstable sort
        // will shuffle them between frames.
        assert_ne!(natural_cmp("file01", "file1"), Ordering::Equal);
        assert_eq!(natural_cmp("file1", "file01"), Ordering::Less);
    }

    #[test]
    fn very_long_digit_runs_do_not_overflow() {
        // 40 digits — far past u128. Comparing by significant length keeps this
        // correct where parse-to-integer would panic or saturate.
        let a = format!("f{}", "9".repeat(40));
        let b = format!("f{}", "9".repeat(39));
        assert_eq!(natural_cmp(&a, &b), Ordering::Greater);
    }

    #[test]
    fn directories_stay_first_when_descending() {
        let mut list: EntryList = [
            mk("b.txt", EntryKind::File, 10, 0),
            mk("adir", EntryKind::Directory, 0, 0),
            mk("a.txt", EntryKind::File, 20, 0),
            mk("zdir", EntryKind::Directory, 0, 0),
        ]
        .into_iter()
        .collect();

        sort(
            &mut list,
            SortSpec {
                column: SortColumn::Name,
                order: SortOrder::Descending,
                dirs_first: true,
            },
        );

        assert_eq!(names(&list), ["zdir", "adir", "b.txt", "a.txt"]);
    }

    #[test]
    fn sorting_by_size_is_deterministic_on_ties() {
        // Every file has the same size, so the name tiebreak decides.
        let mut list: EntryList = [
            mk("c.txt", EntryKind::File, 5, 0),
            mk("a.txt", EntryKind::File, 5, 0),
            mk("b.txt", EntryKind::File, 5, 0),
        ]
        .into_iter()
        .collect();

        sort(
            &mut list,
            SortSpec {
                column: SortColumn::Size,
                order: SortOrder::Ascending,
                dirs_first: true,
            },
        );

        assert_eq!(names(&list), ["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn natural_order_holds_across_a_full_sort() {
        let mut list: EntryList = [
            mk("img12.png", EntryKind::File, 0, 0),
            mk("img2.png", EntryKind::File, 0, 0),
            mk("img100.png", EntryKind::File, 0, 0),
            mk("img1.png", EntryKind::File, 0, 0),
        ]
        .into_iter()
        .collect();

        sort(&mut list, SortSpec::default());

        assert_eq!(
            names(&list),
            ["img1.png", "img2.png", "img12.png", "img100.png"]
        );
    }

    #[test]
    fn filtering_hides_and_restores_hidden_entries() {
        let hidden = Entry {
            attrs: attr::HIDDEN,
            ..mk("secret.txt", EntryKind::File, 0, 0)
        };
        let mut list: EntryList = [mk("visible.txt", EntryKind::File, 0, 0), hidden]
            .into_iter()
            .collect();

        apply(&mut list, SortSpec::default(), false);
        assert_eq!(names(&list), ["visible.txt"]);

        // The filtered entry must come back — this is what `apply` exists for,
        // since re-sorting the surviving order alone never could.
        apply(&mut list, SortSpec::default(), true);
        assert_eq!(names(&list), ["secret.txt", "visible.txt"]);
    }

    #[test]
    fn the_name_filter_is_a_case_insensitive_substring() {
        let mut list: EntryList = [
            mk("Report.docx", EntryKind::File, 0, 0),
            mk("notes.txt", EntryKind::File, 0, 0),
            mk("reports", EntryKind::Directory, 0, 0),
        ]
        .into_iter()
        .collect();

        apply_filtered(&mut list, SortSpec::default(), false, "REPORT");
        // Directory first, as everywhere else — the filter must not disturb
        // the ordering rules.
        assert_eq!(names(&list), ["reports", "Report.docx"]);

        // Matching anywhere in the name, not just at the start: this is a
        // narrowing filter, not type-ahead.
        apply_filtered(&mut list, SortSpec::default(), false, "ote");
        assert_eq!(names(&list), ["notes.txt"]);
    }

    #[test]
    fn an_empty_filter_restores_everything() {
        let mut list: EntryList = [
            mk("a.txt", EntryKind::File, 0, 0),
            mk("b.txt", EntryKind::File, 0, 0),
        ]
        .into_iter()
        .collect();

        apply_filtered(&mut list, SortSpec::default(), false, "a");
        assert_eq!(names(&list), ["a.txt"]);

        // Whitespace only is also empty: a stray space must not blank the pane.
        apply_filtered(&mut list, SortSpec::default(), false, "   ");
        assert_eq!(names(&list), ["a.txt", "b.txt"]);
    }

    #[test]
    fn the_filter_and_the_hidden_toggle_both_apply() {
        let hidden = Entry {
            attrs: attr::HIDDEN,
            ..mk("archive.hidden", EntryKind::File, 0, 0)
        };
        let mut list: EntryList = [mk("archive.txt", EntryKind::File, 0, 0), hidden]
            .into_iter()
            .collect();

        apply_filtered(&mut list, SortSpec::default(), false, "archive");
        assert_eq!(names(&list), ["archive.txt"]);

        apply_filtered(&mut list, SortSpec::default(), true, "archive");
        assert_eq!(names(&list), ["archive.hidden", "archive.txt"]);
    }

    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(extension(".gitignore"), "");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("noext"), "");
    }
}
