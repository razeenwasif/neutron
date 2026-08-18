//! Text matching shared by the filter field and the index.
//!
//! One implementation because there is one behaviour: "does this name contain
//! what I typed", case-insensitively, without allocating. Both callers run it
//! over every entry they hold on every keystroke, so an allocation here is an
//! allocation per file per character typed.

/// Whether `haystack` contains `needle_lower`, ignoring ASCII case.
///
/// `needle_lower` must already be lowercased; the caller does it once per query
/// rather than once per candidate.
///
/// # Case folding is ASCII-only on the haystack side
///
/// Filenames are overwhelmingly ASCII, and folding Unicode properly means
/// allocating a lowercased copy of every candidate — which is the entire
/// budget, spent on a case that almost never arises. A non-ASCII needle still
/// matches non-ASCII names exactly.
pub fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let (hay, nee) = (haystack.as_bytes(), needle_lower.as_bytes());
    if nee.len() > hay.len() {
        return false;
    }

    let first = nee[0];
    // Only the ASCII case needs both variants; a non-ASCII first byte is
    // matched exactly.
    let first_upper = first.to_ascii_uppercase();

    // The last position a match could still start at.
    let last_start = hay.len() - nee.len();

    // `memchr2` rather than a byte loop: this is overwhelmingly a rejection
    // test, and the rejection is the part worth vectorising.
    let mut from = 0;
    while from <= last_start {
        let Some(offset) = memchr::memchr2(first, first_upper, &hay[from..=last_start]) else {
            return false;
        };
        let start = from + offset;

        if hay[start..start + nee.len()]
            .iter()
            .zip(nee)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_substring_is_found_anywhere() {
        assert!(contains_ignore_ascii_case("report.txt", "port"));
        assert!(contains_ignore_ascii_case("report.txt", "report.txt"));
    }

    #[test]
    fn case_is_ignored_on_both_sides() {
        assert!(contains_ignore_ascii_case("README.MD", "readme"));
    }

    #[test]
    fn an_absent_substring_is_not_found() {
        assert!(!contains_ignore_ascii_case("report.txt", "zebra"));
    }

    #[test]
    fn an_empty_needle_matches_everything() {
        // Clearing the filter field must restore the whole listing.
        assert!(contains_ignore_ascii_case("anything", ""));
    }

    #[test]
    fn a_needle_longer_than_the_name_is_rejected() {
        assert!(!contains_ignore_ascii_case("a", "abc"));
    }

    #[test]
    fn a_near_miss_does_not_stop_the_search() {
        // The first candidate fails on its second byte; the real match is
        // later. Bailing out at the first candidate is the classic bug here.
        assert!(contains_ignore_ascii_case("abxabc", "abc"));
    }

    #[test]
    fn non_ascii_matches_exactly() {
        assert!(contains_ignore_ascii_case("caf\u{e9}.txt", "caf\u{e9}"));
    }
}
