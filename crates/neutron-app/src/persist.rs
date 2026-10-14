//! Writing the small JSON files Neutron keeps beside its session — pins and
//! recent files — so that a write can neither tear nor go backwards.
//!
//! # Whole or not at all
//!
//! `fs::write` truncates the file and then fills it, so a crash, a full disk or
//! a second writer in between leaves half a file — which then fails to parse,
//! and used to read as "no pins at all". Writing beside it and renaming over
//! it means the file on disk is always one complete version or another.
//!
//! # Never backwards
//!
//! Saves run on the worker pool, four threads wide, so two saves in quick
//! succession — pin then unpin — can finish in either order. Each snapshot
//! carries the revision it was taken at, and a write older than the one
//! already on disk is skipped, so the last change made is the one that stays.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Writes `contents` to `file` atomically, unless a newer revision of it has
/// already been written this session.
///
/// **Worker thread only** — a write to the config directory.
pub fn write(file: &Path, contents: &str, revision: u64) -> Result<(), String> {
    static WRITTEN: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);
    // Held for the whole write, so two saves of one file cannot interleave
    // their temporary files either.
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    let written = written.get_or_insert_with(HashMap::new);
    if written.get(file).is_some_and(|&on_disk| on_disk > revision) {
        return Ok(());
    }

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut temporary = file.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, file).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        e.to_string()
    })?;

    written.insert(file.to_path_buf(), revision);
    Ok(())
}

/// Moves a file that will not parse out of the way, and says where to.
///
/// So that the next save, which starts from empty, writes a new file rather
/// than over the old one: a typo made editing it by hand should cost the user
/// a fix, not every entry in it.
pub fn set_aside(file: &Path) -> Option<PathBuf> {
    let mut kept = file.as_os_str().to_owned();
    kept.push(".unreadable");
    let kept = PathBuf::from(kept);
    std::fs::rename(file, &kept).ok()?;
    Some(kept)
}

/// How the folder Neutron keeps its settings in is named to the user, in a
/// message about it being missing.
pub fn settings_folder_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Application Support folder"
    } else {
        "%APPDATA%"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neutron-persist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("file.json")
    }

    #[test]
    fn an_older_revision_does_not_overwrite_a_newer_one() {
        let file = scratch("order");
        write(&file, "two", 2).unwrap();
        write(&file, "one", 1).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        write(&file, "three", 3).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "three");
    }

    #[test]
    fn an_unreadable_file_is_kept_rather_than_replaced() {
        let file = scratch("aside");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        let kept = set_aside(&file).expect("moved aside");
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(kept).unwrap(), "{ not json");
    }
}
