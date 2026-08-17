//! fzf-style fuzzy matching.
//!
//! Wraps `nucleo-matcher` (Helix's matcher — the fzf-v2 algorithm, already
//! SIMD-optimised) rather than hand-rolling scoring. The value this crate adds
//! is the match-position plumbing: the overlay highlights matched characters in
//! the accent color, which needs per-character indices, not just a score.
//!
//! Filled in at M5.

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
    needle_buf: Vec<char>,
}

impl FuzzyMatcher {
    pub fn new() -> Self {
        // `match_paths` biases scoring toward path separators and filename
        // segments, which is what we want for a file explorer.
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            haystack_buf: Vec::new(),
            needle_buf: Vec::new(),
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

        let hay = Utf32Str::new(haystack, &mut self.haystack_buf);
        let nee = Utf32Str::new(needle, &mut self.needle_buf);

        let mut positions = Vec::new();
        self.matcher
            .fuzzy_indices(hay, nee, &mut positions)
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
