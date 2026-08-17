//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Open,
    Edit,
    Properties,
    RunAs,
}

pub fn shell_execute(_path: &Path, _verb: Verb, _owner: isize) -> Result<(), String> {
    Err("shell execute is Windows-only".to_owned())
}

pub fn shell_execute_with_args(
    _path: &Path,
    _args: &[String],
    _verb: Verb,
    _owner: isize,
) -> Result<(), String> {
    Err("shell execute is Windows-only".to_owned())
}
