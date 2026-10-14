use std::path::{Path, PathBuf};
use crate::fileops::Transfer;
use serde_json::json;

pub fn write(paths: &[PathBuf], how: Transfer) -> Result<(), String> {
    if paths.is_empty() { return Err("no files selected".into()); }
    let paths = super::macos::path_strs(paths)?;
    super::macos::call(json!({"action":"clipboard-write", "paths":paths, "move":how == Transfer::Move})).map(|_| ())
}
pub fn read() -> Option<(Vec<PathBuf>, Transfer)> {
    let result = super::macos::call(json!({"action":"clipboard-read"})).ok()?;
    let paths: Vec<PathBuf> = serde_json::from_value(result["paths"].clone()).ok()?;
    if paths.is_empty() { return None; }
    Some((paths, if result["move"] == true { Transfer::Move } else { Transfer::Copy }))
}
/// Sources that no longer exist (deleted, or a cut already pasted) are
/// ignored; the paste is refused only if a remaining source is the destination
/// or one of its ancestors, matching the helper's own check.
pub fn can_paste_into(paths: &[PathBuf], destination: &Path) -> bool {
    let Ok(destination) = destination.canonicalize() else { return false; };
    let existing: Vec<PathBuf> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    !existing.is_empty() && !existing.iter().any(|p| destination.starts_with(p))
}

/// Puts plain text on the clipboard: a path, as Copy Path writes it.
pub fn write_text(text: &str) -> Result<(), String> {
    super::macos::call(serde_json::json!({"action":"clipboard-text", "text":text})).map(|_| ())
}
