//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

pub fn watch(_hwnd: isize, _tx: crossbeam_channel::Sender<()>) -> Result<(), String> {
    Err("paste watching is Windows-only".to_owned())
}
