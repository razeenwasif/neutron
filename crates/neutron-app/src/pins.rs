//! Folders the user has pinned to Quick access.
//!
//! # Why a file and not the session
//!
//! The saved workspace already remembers which folders were *open*. Pins are a
//! different thing: a folder you go back to, whether or not it was open when
//! you last closed the window. Keeping them in the session would lose them the
//! first time a tab was closed, which is exactly backwards.
//!
//! `%APPDATA%\Neutron\pins.json`, beside `sites.json` and for the same reasons:
//! it is inspectable, editable by hand, and survives a reinstall.
//!
//! # Only real folders
//!
//! A pin is a path, so only a location that has one can be pinned. This PC, a
//! Drive folder and a server are all excluded — not because pinning them is a
//! bad idea, but because a pin has to survive a restart, and none of those
//! three can be reached from a string on disk without the provider being
//! connected first. The refusal says which.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most pins kept. Past this the sidebar section stops being a shortcut and
/// becomes a second file listing to read.
pub const MAX: usize = 24;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default)]
    pub folders: Vec<PathBuf>,
}

impl Pins {
    /// Adds `path`, or removes it if it is already pinned. Returns what
    /// happened, so the caller can say so.
    pub fn toggle(&mut self, path: &Path) -> Toggled {
        if let Some(at) = self.folders.iter().position(|p| p == path) {
            self.folders.remove(at);
            return Toggled::Unpinned;
        }
        if self.folders.len() >= MAX {
            return Toggled::Full;
        }
        // Newest first: a pin made just now is the one most likely to be
        // wanted next, and the list is read from the top.
        self.folders.insert(0, path.to_path_buf());
        Toggled::Pinned
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggled {
    Pinned,
    Unpinned,
    /// Already at [`MAX`]. Nothing changed.
    Full,
}

/// `%APPDATA%\Neutron\pins.json`.
///
/// The fallbacks match the rest of the application's config handling and are
/// what let this be exercised away from Windows.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("pins.json"))
}

/// Reads the pins.
///
/// A missing file is the normal case for someone who has never pinned
/// anything, and is not an error. A file that will not parse *is* — returned so
/// it can be shown, because a pin that silently fails to appear is
/// unanswerable.
pub fn load() -> Result<Pins, String> {
    let Some(file) = path() else {
        return Ok(Pins::default());
    };
    match std::fs::read_to_string(&file) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Pins::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn parse(text: &str) -> Result<Pins, String> {
    if text.trim().is_empty() {
        return Ok(Pins::default());
    }
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Writes the pins, creating the directory if it is not there.
///
/// **Worker thread only** — this is a write to `%APPDATA%`, which is an
/// ordinary local directory for most people and a redirected network share for
/// some, and this code cannot tell which it is looking at.
pub fn save(pins: &Pins) -> Result<PathBuf, String> {
    let Some(file) = path() else {
        return Err("there is no %APPDATA% to save pins in".to_owned());
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(pins).map_err(|e| e.to_string())?;
    std::fs::write(&file, json).map_err(|e| e.to_string())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn pinning_the_same_folder_twice_unpins_it() {
        let mut pins = Pins::default();
        assert_eq!(pins.toggle(&p("/notes")), Toggled::Pinned);
        assert!(pins.folders.contains(&p("/notes")));
        assert_eq!(pins.toggle(&p("/notes")), Toggled::Unpinned);
        assert!(!pins.folders.contains(&p("/notes")));
        assert!(pins.folders.is_empty());
    }

    /// The list is read from the top, and the one just pinned is the one most
    /// likely to be wanted next.
    #[test]
    fn the_newest_pin_comes_first() {
        let mut pins = Pins::default();
        pins.toggle(&p("/one"));
        pins.toggle(&p("/two"));
        assert_eq!(pins.folders, vec![p("/two"), p("/one")]);
    }

    /// Past the cap the section stops being a shortcut. Refused rather than
    /// silently dropping the oldest, which would lose a pin the user made on
    /// purpose without saying so.
    #[test]
    fn the_list_is_capped_and_says_so() {
        let mut pins = Pins::default();
        for i in 0..MAX {
            assert_eq!(pins.toggle(&p(&format!("/f{i}"))), Toggled::Pinned);
        }
        assert_eq!(pins.toggle(&p("/one-too-many")), Toggled::Full);
        assert_eq!(pins.folders.len(), MAX);
        assert!(!pins.folders.contains(&p("/one-too-many")));
    }

    /// Unpinning still works at the cap — otherwise a full list could never be
    /// emptied.
    #[test]
    fn a_full_list_can_still_be_unpinned_from() {
        let mut pins = Pins::default();
        for i in 0..MAX {
            pins.toggle(&p(&format!("/f{i}")));
        }
        assert_eq!(pins.toggle(&p("/f0")), Toggled::Unpinned);
        assert_eq!(pins.folders.len(), MAX - 1);
    }

    #[test]
    fn an_absent_file_is_not_an_error() {
        assert!(parse("").expect("empty is fine").folders.is_empty());
    }

    /// Hand-editable, so it has to survive being hand-edited badly.
    #[test]
    fn a_broken_file_is_reported_rather_than_ignored() {
        assert!(parse("{ not json").is_err());
    }

    #[test]
    fn pins_round_trip_through_json() {
        let mut pins = Pins::default();
        pins.toggle(&p("/a"));
        pins.toggle(&p("/b"));
        let text = serde_json::to_string(&pins).expect("serialise");
        let back = parse(&text).expect("parse");
        assert_eq!(back.folders, pins.folders);
    }
}
