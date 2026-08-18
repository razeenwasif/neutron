//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::{Path, PathBuf};

use crate::fileops::Transfer;

pub fn write(_paths: &[PathBuf], _how: Transfer) -> Result<(), String> {
    Err("the clipboard is Windows-only".to_owned())
}

pub fn read() -> Option<(Vec<PathBuf>, Transfer)> {
    None
}

pub fn can_paste_into(_paths: &[PathBuf], _destination: &Path) -> bool {
    false
}
