//! Non-Windows placeholder. See `fs_stub` for why these exist.

pub fn apply_glass(_hwnd: isize, _dark: bool) {}

pub fn disable_glass(_hwnd: isize) {}

pub fn transparency_enabled() -> bool {
    true
}
