//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use std::path::PathBuf;

use neutron_core::menu::MenuItem;

pub fn open<F>(_paths: &[PathBuf], _choose: F) -> Result<(), String>
where
    F: FnOnce(Vec<MenuItem>) -> Option<u32>,
{
    Err("context menus are Windows-only".to_owned())
}
