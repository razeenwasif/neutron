//! The wire protocol between `neutron.exe` and the elevated indexer.
//!
//! # Shape
//!
//! Newline-delimited JSON, one message per line, request and response strictly
//! alternating on a single connection.
//!
//! JSON rather than a compact binary encoding because the traffic is tiny and
//! infrequent — one query per keystroke, capped at a few thousand results — and
//! being able to read a session with any text tool is worth far more than the
//! bytes saved. The heavy data never crosses this boundary: the index itself
//! stays in the helper.
//!
//! # Trust
//!
//! The client is *unelevated* and the server is *elevated*, so every request is
//! a message from a lower-privilege process to a higher-privilege one. That
//! makes the server's parsing an attack surface, and the protocol is
//! deliberately shaped so it cannot be much of one: the only inputs are a
//! string to search for and a result cap. Nothing here names a path to read, a
//! command to run, or a file to write.

use serde::{Deserialize, Serialize};

/// Default pipe name. The full path is `\\.\pipe\<this>`.
pub const DEFAULT_PIPE: &str = "neutron-index";

/// Longest search string accepted.
///
/// A needle longer than any possible filename cannot match anything, so this
/// costs nothing real and bounds what an unelevated caller can make the
/// elevated process allocate.
pub const MAX_NEEDLE: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// How far along indexing is.
    Status,
    /// Search every indexed volume by substring — the global search.
    Search { needle: String, limit: usize },
    /// Fuzzy-match names beneath one directory — the finder.
    ///
    /// Separate from [`Request::Search`] rather than a flag on it, because the
    /// two answer different questions and are tuned differently: substring over
    /// everything, fuzzy over a subtree. A single request with a mode flag
    /// would invite calling for fuzzy over five million names, which is the one
    /// combination that is neither fast nor useful.
    Find {
        needle: String,
        /// Directory to search under, inclusive of its own children.
        scope: String,
        limit: usize,
    },
    /// Stop serving and exit.
    ///
    /// **Nothing sends this yet.** The UI deliberately leaves the helper
    /// running when it closes, so the next session reconnects without another
    /// UAC prompt. That leaves no way to stop it from inside the application —
    /// an elevated process an unelevated UI cannot kill — which is a real gap
    /// rather than a hypothetical one: it had to be killed with `taskkill`
    /// during development. The command that sends this arrives with the
    /// palette at M5.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Status(IndexStatus),
    Results {
        hits: Vec<SearchHit>,
        /// Matches found, which can exceed `hits.len()`.
        total: usize,
        truncated: bool,
        /// Server-side search time, so the UI can report where latency went.
        elapsed_micros: u64,
    },
    Error(String),
}

/// Progress, so the UI can say "indexing 3 of 6" rather than looking broken.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStatus {
    pub volumes_done: usize,
    pub volumes_total: usize,
    pub records: usize,
    pub memory_bytes: usize,
    /// Volumes that could not be indexed, with the reason — a FAT32 stick, a
    /// volume with no journal. Shown once rather than retried.
    pub skipped: Vec<String>,
    /// Whether this index was read from the volumes just now, or loaded from
    /// the last run's cache.
    ///
    /// A cached index is missing everything created since it was written, and
    /// a search tool that quietly omits results is worse than one that says it
    /// might. The UI shows the difference and offers to refresh.
    #[serde(default)]
    pub fresh: bool,
    /// How many seconds ago a cached index was written, when it was cached.
    ///
    /// "From the last index" is not actionable; "from an index six days old"
    /// is. It is the difference between a note the user reads past and one
    /// they do something about.
    #[serde(default)]
    pub cached_age_secs: Option<u64>,
}

impl IndexStatus {
    pub fn is_ready(&self) -> bool {
        self.volumes_total > 0 && self.volumes_done >= self.volumes_total
    }
}

/// One search result, already resolved to a path by the server.
///
/// Paths are reconstructed server-side because that is where the parent chains
/// live. Sending the client a record index would mean shipping it the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub name: String,
    /// Containing directory — what opening the result navigates to.
    pub parent: String,
    pub is_dir: bool,
    /// Character indices into `name` that the query matched, ascending.
    ///
    /// Empty for substring results, where the whole needle matches contiguously
    /// and highlighting adds nothing. Populated for fuzzy results, where seeing
    /// *why* something matched is most of what makes a ranked list readable.
    #[serde(default)]
    pub matched: Vec<u32>,
}

/// Clamps a needle to something the server will accept.
pub fn sanitize_needle(needle: &str) -> String {
    let trimmed = needle.trim();
    if trimmed.len() <= MAX_NEEDLE {
        return trimmed.to_owned();
    }
    // Truncate on a character boundary; slicing mid-character panics.
    let mut end = MAX_NEEDLE;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_one_line_each() {
        // The framing is newline-delimited, so an encoded message containing a
        // literal newline would desynchronise the stream permanently.
        let requests = [
            Request::Status,
            Request::Search {
                needle: "a\nb\tc\"d".to_owned(),
                limit: 100,
            },
            Request::Find {
                needle: "a\nb".to_owned(),
                scope: "C:\\a b\\c".to_owned(),
                limit: 50,
            },
            Request::Shutdown,
        ];

        for request in requests {
            let line = serde_json::to_string(&request).expect("encodes");
            assert!(!line.contains('\n'), "encoded message contains a newline");
            let _: Request = serde_json::from_str(&line).expect("decodes");
        }
    }

    #[test]
    fn a_response_with_awkward_paths_round_trips() {
        let response = Response::Results {
            hits: vec![SearchHit {
                name: "日本 🦀.txt".to_owned(),
                parent: "C:\\Users\\Ra zeen\\Do\"cs".to_owned(),
                is_dir: false,
                matched: vec![0, 2, 5],
            }],
            total: 1,
            truncated: false,
            elapsed_micros: 42,
        };
        let line = serde_json::to_string(&response).expect("encodes");
        assert!(!line.contains('\n'));

        let back: Response = serde_json::from_str(&line).expect("decodes");
        match back {
            Response::Results { hits, .. } => {
                assert_eq!(hits[0].name, "日本 🦀.txt");
                assert!(hits[0].parent.contains('"'));
                assert_eq!(hits[0].matched, vec![0, 2, 5]);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn an_over_long_needle_is_clamped_on_a_character_boundary() {
        // The server is elevated and the client is not, so what the client can
        // make it allocate has to be bounded — and clamping must not panic on
        // multi-byte input.
        let long = "🦀".repeat(MAX_NEEDLE);
        let clamped = sanitize_needle(&long);
        assert!(clamped.len() <= MAX_NEEDLE);
        assert!(clamped.chars().all(|c| c == '🦀'));
    }

    #[test]
    fn needles_are_trimmed() {
        assert_eq!(sanitize_needle("  report  "), "report");
        assert_eq!(sanitize_needle("   "), "");
    }

    #[test]
    fn status_is_only_ready_once_every_volume_is_done() {
        assert!(!IndexStatus::default().is_ready());
        assert!(
            !IndexStatus {
                volumes_done: 2,
                volumes_total: 6,
                ..Default::default()
            }
            .is_ready()
        );
        assert!(
            IndexStatus {
                volumes_done: 6,
                volumes_total: 6,
                ..Default::default()
            }
            .is_ready()
        );
    }
}
