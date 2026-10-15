//! Finder tags are a Mac thing. Elsewhere there are none, and none to find.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub color: u8,
}

pub const COLORS: [(&str, u8); 7] = [
    ("Red", 6),
    ("Orange", 7),
    ("Yellow", 5),
    ("Green", 2),
    ("Blue", 4),
    ("Purple", 3),
    ("Gray", 1),
];

pub fn read(_path: &Path) -> Vec<Tag> {
    Vec::new()
}
pub fn colors(_path: &Path) -> u8 {
    0
}
pub fn toggle(_paths: &[PathBuf], _name: &str, _on: bool) -> Result<(), String> {
    Err("tags are only on macOS".into())
}
pub fn color_name(color: u8) -> String {
    COLORS.iter().find(|(_, c)| *c == color).map(|(n, _)| (*n).to_owned()).unwrap_or_default()
}
pub fn toggle_color(_paths: &[PathBuf], _color: u8, _on: bool) -> Result<(), String> {
    Err("tags are only on macOS".into())
}
pub fn set_names(_paths: &[PathBuf], _names: &[String]) -> Result<(), String> {
    Err("tags are only on macOS".into())
}
pub fn search(_name: &str) -> Result<Vec<PathBuf>, String> {
    Err("tags are only on macOS".into())
}
