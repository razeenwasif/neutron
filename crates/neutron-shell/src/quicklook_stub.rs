//! Quick Look exists only on the Mac. Elsewhere these do nothing, so the app
//! can call them without asking where it is running.

use std::path::PathBuf;

pub fn set_waker(_wake: impl Fn() + Send + Sync + 'static) {}
pub fn request(_paths: Vec<PathBuf>) {}
pub fn pump() {}
pub fn take_moves() -> Vec<isize> {
    Vec::new()
}
pub fn toggle(_paths: &[PathBuf], _index: usize) -> Result<(), String> {
    Err("Quick Look is only on macOS".into())
}
pub fn is_open() -> bool {
    false
}
pub fn follow(_paths: &[PathBuf], _index: usize) {}
