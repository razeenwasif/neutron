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
