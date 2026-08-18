//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    Cancelled,
    Completed,
}

pub fn drag(_paths: &[PathBuf], _owner: isize) -> Result<Dropped, String> {
    Err("dragging out is Windows-only".to_owned())
}
