//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal {
    Recycle,
    Permanent,
}

pub fn delete(_paths: &[PathBuf], _disposal: Disposal, _owner: isize) -> Result<(), String> {
    Err("file operations are Windows-only".to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

pub fn default_transfer(_source: &std::path::Path, _destination: &std::path::Path) -> Transfer {
    Transfer::Copy
}

pub fn transfer(
    _paths: &[PathBuf],
    _destination: &std::path::Path,
    _how: Transfer,
    _owner: isize,
) -> Result<(), String> {
    Err("file operations are Windows-only".to_owned())
}
