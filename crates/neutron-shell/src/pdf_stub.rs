//! Non-Mac placeholder. See `lib.rs` for why the stubs exist.

use std::path::Path;

pub fn pages(_path: &Path) -> Result<Vec<[f32; 2]>, String> {
    Err("PDFs are drawn only on the Mac".to_owned())
}

pub fn render(_path: &Path, _page: usize, _width: usize) -> Result<(usize, usize, Vec<u8>), String> {
    Err("PDFs are drawn only on the Mac".to_owned())
}
