//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::PathBuf;

pub fn show(_paths: &[PathBuf], _x: i32, _y: i32) -> Result<(), String> {
    Err("context menus are Windows-only".to_owned())
}
