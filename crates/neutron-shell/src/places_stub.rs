//! Non-Windows placeholder. See `fs_stub` for why these exist.

use neutron_core::places::Place;
use std::path::PathBuf;

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn known_folders() -> Vec<Place> {
    Vec::new()
}

pub fn cloud_folders() -> Vec<Place> {
    Vec::new()
}

pub fn wsl_distributions() -> Vec<Place> {
    Vec::new()
}

pub fn drives() -> Vec<Place> {
    Vec::new()
}

pub fn drives_streaming(_emit: impl FnMut(Place)) {}

/// Nothing here can be ejected.
pub fn ejectable(_path: &std::path::Path) -> bool {
    false
}

pub fn eject(_path: &std::path::Path) -> Result<(), String> {
    Err("Ejecting is not supported here".into())
}
