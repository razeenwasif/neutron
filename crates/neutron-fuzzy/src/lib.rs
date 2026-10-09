//! fzf-style fuzzy matching.
//!
//! Wraps `nucleo-matcher` (Helix's matcher — the fzf-v2 algorithm, already
//! SIMD-optimised) rather than hand-rolling scoring. The value this crate adds
//! is the match-position plumbing: the overlay highlights matched characters in
//! the accent color, which needs per-character indices, not just a score.
//!
//! Filled in at M5.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A scored match with the character positions that matched, in ascending
/// order, as indices into the *haystack*.
#[derive(Debug, Clone)]
pub struct Match {
    pub score: u32,
    pub positions: Vec<u32>,
}

/// Reusable matcher. Holds internal scratch buffers, so keep one per worker
/// thread rather than constructing per query.
pub struct FuzzyMatcher {
    matcher: Matcher,
    /// Scratch for UTF-32 conversion, reused across calls to avoid a per-item
    /// allocation when scoring hundreds of thousands of candidates.
    haystack_buf: Vec<char>,
    /// The needle as nucleo wants it, and the text it was built from.
    ///
    /// nucleo folds case and strips accents from the haystack but expects the
    /// needle prepared the same way by the caller. Passing it raw made any
    /// capital in the query match only that same capital — "Readme" found
    /// nothing in `readme.md` — and an accented letter match nothing at all.
    /// An `Atom` does that preparation; it is kept between calls because the
    /// same needle is scored against every candidate.
    atom: Option<(String, Atom)>,
}

impl FuzzyMatcher {
    pub fn new() -> Self {
        // `match_paths` biases scoring toward path separators and filename
        // segments, which is what we want for a file explorer.
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            haystack_buf: Vec::new(),
            atom: None,
        }
    }

    /// Scores `needle` against `haystack`, returning `None` when there is no
    /// match. An empty needle matches everything with score 0, so an empty
    /// prompt shows the unfiltered list.
    pub fn score(&mut self, haystack: &str, needle: &str) -> Option<Match> {
        if needle.is_empty() {
            return Some(Match {
                score: 0,
                positions: Vec::new(),
            });
        }

        if self.atom.as_ref().is_none_or(|(text, _)| text != needle) {
            let atom = Atom::new(
                needle,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            );
            self.atom = Some((needle.to_owned(), atom));
        }
        let Some((_, atom)) = &self.atom else {
            return None;
        };

        let hay = Utf32Str::new(haystack, &mut self.haystack_buf);
        let mut positions = Vec::new();
        atom.indices(hay, &mut self.matcher, &mut positions)
            // nucleo scores are u16; widened here so callers can sum or scale
            // them (blending index rank with match quality) without overflow.
            .map(|score| Match {
                score: u32::from(score),
                positions,
            })
    }
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches_and_reports_positions() {
        let mut m = FuzzyMatcher::new();
        let hit = m.score("src/main.rs", "smain").expect("should match");
        assert!(!hit.positions.is_empty());
        // Positions must be ascending; the highlighter relies on it.
        assert!(hit.positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn non_match_returns_none() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("src/main.rs", "zzzz").is_none());
    }

    #[test]
    fn empty_needle_matches_everything() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("anything", "").is_some());
    }

    #[test]
    fn a_capital_in_the_query_matches_either_case() {
        let mut m = FuzzyMatcher::new();
        assert!(m.score("readme.md", "Readme").is_some());
        assert!(m.score("New folder", "new").is_some());
        assert!(m.score("new folder", "New").is_some());
        assert!(m.score("Öl.txt", "öL").is_some());
    }

    #[test]
    fn closer_match_scores_higher() {
        let mut m = FuzzyMatcher::new();
        let exact = m.score("main.rs", "main").unwrap().score;
        let scattered = m.score("m_a_i_n_x.rs", "main").unwrap().score;
        assert!(
            exact > scattered,
            "contiguous match {exact} should beat scattered {scattered}"
        );
    }
}
