//! Files opened lately.
//!
//! # Why this is not the workspace, and not the pins
//!
//! Three lists that all look like "places I have been", and they are not the
//! same list:
//!
//! * the **workspace** remembers which folders were *open* when the window
//!   closed, so a restart looks like it never happened;
//! * a **pin** is a folder chosen on purpose, and stays until it is unpinned;
//! * a **recent** is a file opened by accident of working, kept only because
//!   the next thing you want is usually the last thing you had.
//!
//! Recents are the only one of the three the user never curates, which is why
//! this is the one with a hard cap and an eviction rule.
//!
//! # Only local files
//!
//! Recording a path means it can be reopened from a string on disk, and only a
//! real file can be. A file fetched from a server or from Drive was opened as a
//! *copy* under `%TEMP%`, and pointing at that copy weeks later would either
//! open something stale or nothing at all.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many are kept. Enough that yesterday is still there, few enough that the
/// list stays readable without scrolling.
pub const MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub path: PathBuf,
    /// Unix milliseconds, matching how the rest of the application carries a
    /// time.
    pub at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default)]
    pub files: Vec<Visit>,
    /// Bumped by every change, so a stale save cannot overwrite a newer one.
    /// See `crate::persist`.
    #[serde(skip)]
    pub revision: u64,
}

impl Recent {
    /// Records `path` as just opened.
    ///
    /// Returns whether anything changed, so the caller can skip a write when
    /// nothing did — opening the same file twice in a row is common and should
    /// not touch the disk the second time.
    pub fn record(&mut self, path: &Path, at: i64) -> bool {
        if let Some(existing) = self.files.first() {
            if existing.path == path {
                // Already at the top. Rewriting the timestamp would churn the
                // file for no visible change.
                return false;
            }
        }
        // Moved rather than duplicated: a file opened twice is one entry, at
        // the position its most recent opening earns.
        self.files.retain(|v| v.path != path);
        self.files.insert(
            0,
            Visit {
                path: path.to_path_buf(),
                at,
            },
        );
        self.files.truncate(MAX);
        self.revision += 1;
        true
    }

    /// Drops one entry, for a file the user no longer wants listed.
    pub fn forget(&mut self, path: &Path) -> bool {
        let before = self.files.len();
        self.files.retain(|v| v.path != path);
        let changed = before != self.files.len();
        self.revision += u64::from(changed);
        changed
    }
}

/// `%APPDATA%\Neutron\recent.json`.
pub fn path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support").into_os_string();
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("recent.json"))
}

pub fn load() -> Result<Recent, String> {
    let Some(file) = path() else {
        return Ok(Recent::default());
    };
    match std::fs::read_to_string(&file) {
        Ok(text) => parse(&text).map_err(|e| match crate::persist::set_aside(&file) {
            Some(kept) => format!("{e}; the file was kept as {}", kept.display()),
            None => e,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Recent::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn parse(text: &str) -> Result<Recent, String> {
    if text.trim().is_empty() {
        return Ok(Recent::default());
    }
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// **Worker thread only** — a write to `%APPDATA%`.
pub fn save(recent: &Recent) -> Result<(), String> {
    let Some(file) = path() else {
        return Err("there is no %APPDATA% to save recent files in".to_owned());
    };
    let json = serde_json::to_string_pretty(recent).map_err(|e| e.to_string())?;
    crate::persist::write(&file, &json, recent.revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn the_newest_is_first() {
        let mut r = Recent::default();
        r.record(&p("/a"), 1);
        r.record(&p("/b"), 2);
        assert_eq!(r.files[0].path, p("/b"));
        assert_eq!(r.files[1].path, p("/a"));
    }

    /// A file opened twice is one entry. Duplicates would fill the list with
    /// the one file someone is working on and evict everything else.
    #[test]
    fn reopening_moves_an_entry_rather_than_repeating_it() {
        let mut r = Recent::default();
        r.record(&p("/a"), 1);
        r.record(&p("/b"), 2);
        assert!(r.record(&p("/a"), 3), "moving one counts as a change");
        assert_eq!(r.files.len(), 2);
        assert_eq!(r.files[0].path, p("/a"));
        assert_eq!(r.files[0].at, 3, "it carries the newer time");
    }

    /// Opening the same file twice in a row is common. Rewriting the file for
    /// a change nobody can see is a write nobody asked for.
    #[test]
    fn recording_what_is_already_at_the_top_changes_nothing() {
        let mut r = Recent::default();
        assert!(r.record(&p("/a"), 1));
        assert!(!r.record(&p("/a"), 2), "no change to save");
        assert_eq!(r.files[0].at, 1, "the original time is kept");
    }

    #[test]
    fn the_list_is_capped_and_drops_the_oldest() {
        let mut r = Recent::default();
        for i in 0..(MAX + 5) {
            r.record(&p(&format!("/f{i}")), i as i64);
        }
        assert_eq!(r.files.len(), MAX);
        assert_eq!(r.files[0].path, p(&format!("/f{}", MAX + 4)));
        // The first few are gone, which is the point of a recent list.
        assert!(!r.files.iter().any(|v| v.path == p("/f0")));
    }

    #[test]
    fn forgetting_removes_exactly_one() {
        let mut r = Recent::default();
        r.record(&p("/a"), 1);
        r.record(&p("/b"), 2);
        assert!(r.forget(&p("/a")));
        assert!(!r.forget(&p("/a")), "already gone");
        assert_eq!(r.files.len(), 1);
    }

    #[test]
    fn an_absent_file_is_not_an_error() {
        assert!(parse("").expect("empty is fine").files.is_empty());
    }

    #[test]
    fn a_broken_file_is_reported_rather_than_ignored() {
        assert!(parse("{ not json").is_err());
    }

    #[test]
    fn entries_round_trip_through_json() {
        let mut r = Recent::default();
        r.record(&p("/a"), 1);
        let text = serde_json::to_string(&r).expect("serialise");
        assert_eq!(parse(&text).expect("parse").files, r.files);
    }
}
