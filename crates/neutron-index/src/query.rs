//! Searching the index.
//!
//! # Why this is fast enough to feel instant
//!
//! A full scan of five million names is tens of milliseconds — fine once, far
//! too slow to repeat on every keystroke. The trick, and it is the same one
//! Everything uses, is that **typing narrows**: `rep` can only match files that
//! already matched `re`. So a query that extends the previous one filters the
//! previous *result set* instead of rescanning the index.
//!
//! In practice the first character of a search costs a full scan and every
//! character after it is nearly free, because the candidate set collapses by an
//! order of magnitude per keystroke. Deleting a character, or typing something
//! unrelated, falls back to a full scan.
//!
//! # Matching
//!
//! Plain case-insensitive substring, not fuzzy. This is the "I know what the
//! file is called" search, and a fuzzy match over five million names returns a
//! wall of near-misses ranked above the exact hit. Fuzzy belongs in the finder
//! overlay, over a much smaller candidate set.
//!
//! Case folding is ASCII-only on the haystack side. Filenames are overwhelmingly
//! ASCII, and folding Unicode properly means allocating a lowercased copy of
//! every candidate — which is the entire budget, spent on a case that almost
//! never arises. A non-ASCII needle still matches non-ASCII names exactly.

use crate::volume::VolumeIndex;

/// A search hit, identifying the record it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Which volume's index, as a position in the searched slice.
    pub volume: u16,
    /// Record index within that volume.
    pub record: u32,
}

/// Results, plus what had to be given up to produce them.
#[derive(Debug, Clone, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Total matches found, which can exceed `hits.len()`.
    pub total: usize,
    /// True when `total` hit [`MAX_HITS`] and the scan stopped early.
    pub truncated: bool,
}

/// Hits collected before a search gives up.
///
/// A query like `e` matches most of a disk. Nobody scrolls through two million
/// rows, and collecting them costs both the memory and the time the cap exists
/// to save. The count shown to the user says "more than this many".
pub const MAX_HITS: usize = 4096;

/// A reusable searcher that remembers the last query, so an extending query can
/// narrow the previous results instead of rescanning.
#[derive(Default)]
pub struct Searcher {
    last: String,
    last_hits: Vec<Hit>,
    /// Whether `last_hits` is the complete match set for `last`. A truncated
    /// previous result cannot be narrowed — the hits it dropped might be the
    /// only ones matching the longer query.
    last_complete: bool,
}

impl Searcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `needle` against `volumes`.
    ///
    /// An empty needle returns nothing rather than everything: this backs a
    /// search box, where an empty box means "not searching", not "show me all
    /// five million files".
    pub fn search(&mut self, volumes: &[VolumeIndex], needle: &str) -> Results {
        let needle = needle.trim();
        if needle.is_empty() {
            self.last.clear();
            self.last_hits.clear();
            self.last_complete = true;
            return Results::default();
        }

        let lowered = needle.to_lowercase();

        let mut results = if self.can_narrow(needle) {
            self.narrow(volumes, &lowered)
        } else {
            self.full_scan(volumes, &lowered)
        };

        self.last = needle.to_owned();
        self.last_complete = !results.truncated;
        self.last_hits.clone_from(&results.hits);

        // Directories before files, then shortest path first. A shallow hit is
        // almost always the one meant — `C:\Windows\notepad.exe` before
        // `C:\Windows\WinSxS\amd64_…\notepad.exe`.
        results.hits.sort_by_cached_key(|h| {
            let v = &volumes[h.volume as usize];
            let i = h.record as usize;
            (!v.is_dir(i), v.name(i).len(), h.volume, h.record)
        });
        results
    }

    /// Whether the previous result set is a valid starting point.
    fn can_narrow(&self, needle: &str) -> bool {
        // Extending the previous needle can only ever shrink the match set —
        // but only if that set was complete. A truncated one is missing hits
        // the longer query might be the only match for.
        self.last_complete && !self.last.is_empty() && needle.starts_with(&self.last)
    }

    fn narrow(&self, volumes: &[VolumeIndex], lowered: &str) -> Results {
        let mut out = Results::default();
        for &hit in &self.last_hits {
            let name = volumes[hit.volume as usize].name(hit.record as usize);
            if contains_ignore_ascii_case(name, lowered) {
                out.total += 1;
                if out.hits.len() < MAX_HITS {
                    out.hits.push(hit);
                }
            }
        }
        // Narrowing from a complete set is itself complete: `total` is exact,
        // and it is only `hits` that is capped.
        out.truncated = false;
        out
    }

    /// Scans every record across every volume, in parallel.
    ///
    /// This is the expensive path — the first character of any search, and
    /// every keystroke of one whose results stay capped. The scanning itself
    /// is [`VolumeIndex::scan`], which sweeps the name arena as one contiguous
    /// buffer; see there for why that is not a loop over individual names.
    ///
    /// Every match is counted even once the hit list is full, so the total is
    /// exact. An earlier version stopped at the cap, which made a one-letter
    /// query look fast but reported "4097+" where the honest answer is a real
    /// number — and a search tool's count is one of the few things a user
    /// cannot verify for themselves.
    fn full_scan(&self, volumes: &[VolumeIndex], lowered: &str) -> Results {
        use rayon::prelude::*;

        // Chunked by record range rather than by volume: one volume routinely
        // holds ten times another, so per-volume tasks leave most cores idle
        // waiting for the largest.
        //
        // Large chunks, because each one is a contiguous sweep and the setup
        // cost of a vectorised search is paid per sweep. Small chunks would
        // reintroduce, at a coarser grain, exactly the overhead that scanning
        // the arena in one piece exists to avoid.
        const CHUNK: usize = 262_144;

        let tasks: Vec<(u16, usize, usize)> = volumes
            .iter()
            .enumerate()
            .flat_map(|(v, index)| {
                (0..index.len())
                    .step_by(CHUNK)
                    .map(move |start| (v as u16, start, (start + CHUNK).min(index.len())))
            })
            .collect();

        let (hits, total) = tasks
            .par_iter()
            .map(|&(volume, start, end)| {
                let index = &volumes[volume as usize];
                let mut local = Vec::new();
                let mut count = 0usize;

                index.scan(start..end, lowered, |record| {
                    count += 1;
                    // Each chunk keeps at most the cap, so the merged list is
                    // bounded by cores × cap rather than by the match count —
                    // which for a one-letter query is millions.
                    if local.len() < MAX_HITS {
                        local.push(Hit {
                            volume,
                            record: record as u32,
                        });
                    }
                });
                (local, count)
            })
            .reduce(
                || (Vec::new(), 0usize),
                |mut a, b| {
                    let room = MAX_HITS.saturating_sub(a.0.len());
                    a.0.extend(b.0.into_iter().take(room));
                    a.1 += b.1;
                    a
                },
            );

        Results {
            truncated: total > hits.len(),
            hits,
            total,
        }
    }
}

/// A fuzzy hit: where it is, how well it scored, and which characters matched.
#[derive(Debug, Clone)]
pub struct FuzzyHit {
    pub hit: Hit,
    pub score: u32,
    /// Character indices into the *name* that matched, ascending. Drives the
    /// per-character highlighting in the finder.
    pub positions: Vec<u32>,
}

/// Fuzzy-matches names beneath `scope`.
///
/// # Why this is scoped and the global search is not
///
/// Fuzzy matching is the right tool when the candidate set is small enough that
/// near-misses are still plausibly what you meant. Over five million names it is
/// the wrong tool twice over: it costs far more per candidate, and it buries the
/// exact hit under a wall of things that merely share some letters. So the
/// finder scopes to one subtree and matches fuzzily; the global search spans
/// everything and matches by substring.
///
/// # Cost
///
/// Three stages, each cheaper than the one it feeds:
///
/// 1. A subsequence test over every name — no allocation, rejects almost
///    everything.
/// 2. A path reconstruction and prefix test, only for survivors.
/// 3. Real fuzzy scoring, only for what is left.
///
/// Ordering matters: reconstructing a path costs a parent walk, so doing it
/// before the subsequence gate would pay that walk five million times.
pub fn fuzzy_in_scope(
    volumes: &[VolumeIndex],
    needle: &str,
    scope: &str,
    limit: usize,
) -> Vec<FuzzyHit> {
    use rayon::prelude::*;

    let lowered = needle.trim().to_lowercase();
    // Compared case-insensitively: the shell hands back paths in whatever case
    // the filesystem recorded, which is not the case the user navigated with.
    let scope_lower = scope.trim_end_matches(['\\', '/']).to_lowercase();

    let candidates: Vec<Hit> = volumes
        .par_iter()
        .enumerate()
        .flat_map(|(v, index)| {
            (0..index.len())
                .into_par_iter()
                .filter(|&record| is_subsequence_ignore_ascii_case(index.name(record), &lowered))
                .filter(|&record| {
                    // Under the scope, or the scope itself.
                    let parent = index.parent_path(record).to_lowercase();
                    parent == scope_lower
                        || parent.starts_with(&scope_lower)
                            && parent.as_bytes().get(scope_lower.len()) == Some(&b'\\')
                })
                .map(move |record| Hit {
                    volume: v as u16,
                    record: record as u32,
                })
                .collect::<Vec<_>>()
        })
        .collect();

    // Scoring is sequential: `FuzzyMatcher` owns reusable scratch buffers, and
    // the surviving set is small enough that a per-thread matcher would cost
    // more in setup than it saves.
    let mut matcher = neutron_fuzzy::FuzzyMatcher::new();
    let mut scored: Vec<FuzzyHit> = candidates
        .into_iter()
        .filter_map(|hit| {
            let name = volumes[hit.volume as usize].name(hit.record as usize);
            matcher.score(name, needle).map(|m| FuzzyHit {
                hit,
                score: m.score,
                positions: m.positions,
            })
        })
        .collect();

    // Best first; ties broken by the shorter name, which is almost always the
    // one meant — `main.rs` before `main_generated_bindings.rs`.
    scored.sort_by(|a, b| {
        let name = |h: &FuzzyHit| volumes[h.hit.volume as usize].name(h.hit.record as usize).len();
        b.score
            .cmp(&a.score)
            .then_with(|| name(a).cmp(&name(b)))
            .then_with(|| a.hit.volume.cmp(&b.hit.volume))
            .then_with(|| a.hit.record.cmp(&b.hit.record))
    });
    scored.truncate(limit);
    scored
}

/// Whether `needle`'s characters all appear in `haystack`, in order.
///
/// The cheap gate in front of fuzzy scoring, and the same one fzf uses. Real
/// fuzzy scoring — which weighs word boundaries, contiguity and position — costs
/// orders of magnitude more than this per candidate, and cannot match anything
/// this rejects. Running it over five million names directly is unaffordable;
/// running it over the few thousand that survive this is instant.
///
/// ASCII-folded on both sides, for the reason in the module note.
pub fn is_subsequence_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let mut wanted = needle_lower.bytes();
    let mut next = wanted.next();

    for byte in haystack.bytes() {
        let Some(target) = next else { break };
        if byte.eq_ignore_ascii_case(&target) {
            next = wanted.next();
        }
    }
    next.is_none()
}

/// Case-insensitive substring search, where `needle` is already lowercased.
///
/// Folds ASCII only — see the module note on why. Non-ASCII bytes compare
/// exactly, so a Unicode needle still matches its own case.
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

    // The last position a match could still start at. Searching past it wastes
    // work and, worse, would report a candidate that cannot be compared.
    let last_start = hay.len() - nee.len();

    // `memchr2` rather than a byte loop. Almost every name in an index does not
    // contain the first character at all, so this function is overwhelmingly a
    // rejection test, and the rejection is the part worth vectorising: a scalar
    // loop compares one byte per iteration where this compares a register full
    // at a time.
    let mut from = 0;
    while from <= last_start {
        let window = &hay[from..=last_start];
        let Some(offset) = memchr::memchr2(first, first_upper, window) else {
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
    use crate::VolumeId;
    use crate::volume::RawRecord;

    fn index(names: &[(&str, bool)]) -> VolumeIndex {
        let records = names
            .iter()
            .enumerate()
            .map(|(i, (name, is_dir))| RawRecord {
                frn: i as u64 + 10,
                parent: 5,
                name: (*name).to_owned(),
                is_dir: *is_dir,
            })
            .collect();
        VolumeIndex::build(VolumeId('C'), records, 0)
    }

    fn names(results: &Results, volumes: &[VolumeIndex]) -> Vec<String> {
        results
            .hits
            .iter()
            .map(|h| volumes[h.volume as usize].name(h.record as usize).to_owned())
            .collect()
    }

    #[test]
    fn matching_is_case_insensitive_in_both_directions() {
        assert!(contains_ignore_ascii_case("ReadMe.MD", "readme"));
        assert!(contains_ignore_ascii_case("readme.md", "readme"));
        assert!(contains_ignore_ascii_case("XYZreadmeXYZ", "readme"));
        assert!(!contains_ignore_ascii_case("read", "readme"));
    }

    #[test]
    fn matching_finds_a_substring_anywhere() {
        // Not a prefix search — "the file with `log` in the name" is the
        // overwhelmingly common way people search.
        assert!(contains_ignore_ascii_case("app.2026.log", "log"));
        assert!(contains_ignore_ascii_case("app.2026.log", "2026"));
    }

    #[test]
    fn an_empty_needle_matches_but_an_empty_search_returns_nothing() {
        // Different questions: the matcher says "no constraint", the search box
        // says "not searching". Conflating them shows five million rows.
        assert!(contains_ignore_ascii_case("anything", ""));

        let volumes = vec![index(&[("a.txt", false)])];
        let mut s = Searcher::new();
        assert!(s.search(&volumes, "  ").hits.is_empty());
    }

    #[test]
    fn non_ascii_needles_match_exactly() {
        assert!(contains_ignore_ascii_case("日本語.txt", "日本"));
        assert!(!contains_ignore_ascii_case("日本語.txt", "中文"));
    }

    #[test]
    fn a_needle_longer_than_the_name_cannot_match() {
        // Guards the subtraction in the loop bound, which underflows if the
        // length check is dropped.
        assert!(!contains_ignore_ascii_case("a", "abcdef"));
        assert!(!contains_ignore_ascii_case("", "a"));
    }

    #[test]
    fn results_put_directories_and_shallow_names_first() {
        let volumes = vec![index(&[
            ("report_final_v2.txt", false),
            ("report", true),
            ("report.txt", false),
        ])];
        let mut s = Searcher::new();
        let got = names(&s.search(&volumes, "report"), &volumes);
        assert_eq!(got, ["report", "report.txt", "report_final_v2.txt"]);
    }

    #[test]
    fn extending_a_query_narrows_the_previous_results() {
        let volumes = vec![index(&[
            ("report.txt", false),
            ("repair.txt", false),
            ("other.txt", false),
        ])];
        let mut s = Searcher::new();

        assert_eq!(s.search(&volumes, "rep").total, 2);
        // The narrowing path must give the same answer as a fresh scan would.
        let narrowed = s.search(&volumes, "repo");
        assert_eq!(names(&narrowed, &volumes), ["report.txt"]);

        let mut fresh = Searcher::new();
        assert_eq!(
            names(&fresh.search(&volumes, "repo"), &volumes),
            names(&narrowed, &volumes)
        );
    }

    #[test]
    fn shortening_a_query_rescans_rather_than_narrowing() {
        // Backspace widens the match set, which cannot come from the previous
        // results. Narrowing here would silently lose every hit the longer
        // query excluded.
        let volumes = vec![index(&[("report.txt", false), ("repair.txt", false)])];
        let mut s = Searcher::new();

        assert_eq!(s.search(&volumes, "repo").total, 1);
        assert_eq!(s.search(&volumes, "rep").total, 2);
    }

    #[test]
    fn an_unrelated_query_rescans() {
        let volumes = vec![index(&[("alpha.txt", false), ("beta.txt", false)])];
        let mut s = Searcher::new();

        assert_eq!(s.search(&volumes, "alpha").total, 1);
        assert_eq!(s.search(&volumes, "beta").total, 1);
    }

    #[test]
    fn a_truncated_result_set_is_never_narrowed() {
        // The cap drops hits. Narrowing from a capped set would hide files that
        // match the longer query — the worst possible failure for a search
        // tool, because it looks like the file does not exist.
        let many: Vec<(String, bool)> = (0..MAX_HITS + 50)
            .map(|i| (format!("match{i}_x.txt"), false))
            .collect();
        let refs: Vec<(&str, bool)> = many.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        let volumes = vec![index(&refs)];

        let mut s = Searcher::new();
        let broad = s.search(&volumes, "match");
        assert!(broad.truncated, "expected the cap to be hit");

        // `match9_x` exists but is well past the cap, so it is not in the
        // truncated hit list. A rescan must still find it.
        let narrowed = s.search(&volumes, "match9_x");
        assert_eq!(narrowed.total, 1, "a rescan was skipped and the hit was lost");
    }

    #[test]
    fn the_hit_list_is_capped_but_the_total_is_not() {
        let many: Vec<(String, bool)> = (0..200).map(|i| (format!("f{i}.log"), false)).collect();
        let refs: Vec<(&str, bool)> = many.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        let volumes = vec![index(&refs)];

        let mut s = Searcher::new();
        let r = s.search(&volumes, "log");
        assert_eq!(r.total, 200);
        assert!(!r.truncated);
        assert_eq!(r.hits.len(), 200);
    }

    #[test]
    fn the_subsequence_gate_accepts_what_fuzzy_could_match() {
        // It sits in front of the scorer, so anything it rejects can never be
        // found. Too strict is a silently missing file.
        assert!(is_subsequence_ignore_ascii_case("src/main.rs", "smain"));
        assert!(is_subsequence_ignore_ascii_case("MainWindow.xaml", "mwx"));
        assert!(is_subsequence_ignore_ascii_case("anything", ""));
        // Order matters — that is what makes it a subsequence rather than a bag
        // of characters.
        assert!(!is_subsequence_ignore_ascii_case("abc", "cba"));
        assert!(!is_subsequence_ignore_ascii_case("main.rs", "mainz"));
    }

    #[test]
    fn the_gate_does_not_run_off_the_end_of_a_short_name() {
        assert!(!is_subsequence_ignore_ascii_case("", "a"));
        assert!(is_subsequence_ignore_ascii_case("", ""));
        assert!(!is_subsequence_ignore_ascii_case("ab", "abc"));
    }

    /// An index shaped like `C:\work\{src\lib.rs, notes.md}` plus a decoy
    /// outside the scope and one in a sibling with a shared prefix.
    fn scoped_index() -> Vec<VolumeIndex> {
        let records = vec![
            RawRecord { frn: 1, parent: 1, name: String::new(), is_dir: true },
            RawRecord { frn: 2, parent: 1, name: "work".into(), is_dir: true },
            RawRecord { frn: 3, parent: 2, name: "src".into(), is_dir: true },
            RawRecord { frn: 4, parent: 3, name: "lib.rs".into(), is_dir: false },
            RawRecord { frn: 5, parent: 2, name: "notes.md".into(), is_dir: false },
            // Sibling directory whose name starts with the scope's name.
            RawRecord { frn: 6, parent: 1, name: "workshop".into(), is_dir: true },
            RawRecord { frn: 7, parent: 6, name: "lib.rs".into(), is_dir: false },
            // Entirely outside.
            RawRecord { frn: 8, parent: 1, name: "lib.rs".into(), is_dir: false },
        ];
        vec![VolumeIndex::build(VolumeId('C'), records, 0)]
    }

    fn found(hits: &[FuzzyHit], volumes: &[VolumeIndex]) -> Vec<String> {
        hits.iter()
            .map(|h| {
                let v = &volumes[h.hit.volume as usize];
                v.path(h.hit.record as usize)
            })
            .collect()
    }

    #[test]
    fn a_scoped_search_stays_inside_its_subtree() {
        let volumes = scoped_index();
        let hits = fuzzy_in_scope(&volumes, "lib", r"C:\work", 20);
        assert_eq!(found(&hits, &volumes), [r"C:\work\src\lib.rs"]);
    }

    #[test]
    fn a_sibling_sharing_the_scopes_prefix_is_excluded() {
        // `C:\workshop` starts with `C:\work`. A naive prefix test pulls the
        // whole sibling tree into every search of `work` — the classic
        // path-prefix bug, and it is silent because the results look plausible.
        let volumes = scoped_index();
        let hits = fuzzy_in_scope(&volumes, "lib", r"C:\work", 20);
        let paths = found(&hits, &volumes);
        assert!(
            !paths.iter().any(|p| p.contains("workshop")),
            "leaked a sibling directory: {paths:?}"
        );
    }

    #[test]
    fn a_trailing_separator_on_the_scope_makes_no_difference() {
        // The scope comes from a breadcrumb, and a drive root arrives as `C:\`
        // while a folder arrives without the trailing slash.
        let volumes = scoped_index();
        let with = fuzzy_in_scope(&volumes, "lib", r"C:\work\", 20);
        let without = fuzzy_in_scope(&volumes, "lib", r"C:\work", 20);
        assert_eq!(found(&with, &volumes), found(&without, &volumes));
    }

    #[test]
    fn scope_matching_ignores_case() {
        // The shell records whatever case the filesystem holds, which is not
        // necessarily the case the user navigated with.
        let volumes = scoped_index();
        let hits = fuzzy_in_scope(&volumes, "lib", r"c:\WORK", 20);
        assert_eq!(found(&hits, &volumes), [r"C:\work\src\lib.rs"]);
    }

    #[test]
    fn results_are_ranked_and_carry_highlight_positions() {
        let volumes = scoped_index();
        let hits = fuzzy_in_scope(&volumes, "lib", r"C:\work", 20);

        let first = hits.first().expect("a hit");
        assert!(!first.positions.is_empty(), "no highlight positions");
        // Ascending, which the highlighter relies on to walk the name once.
        assert!(first.positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_scoped_search_respects_its_limit() {
        let volumes = scoped_index();
        assert!(fuzzy_in_scope(&volumes, "", r"C:\work", 2).len() <= 2);
    }

    #[test]
    fn hits_carry_their_volume() {
        // With several volumes indexed, a record index alone is ambiguous —
        // resolving it against the wrong volume yields a plausible but entirely
        // wrong path.
        let volumes = vec![index(&[("only.txt", false)]), index(&[("only.txt", false)])];
        let mut s = Searcher::new();
        let r = s.search(&volumes, "only");
        assert_eq!(r.total, 2);
        assert_ne!(r.hits[0].volume, r.hits[1].volume);
    }
}
