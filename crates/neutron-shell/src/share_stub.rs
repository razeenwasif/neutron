//! Sharing through the system is wired up on the Mac only.

use std::path::PathBuf;

pub fn share(_paths: &[PathBuf], _at: (f32, f32)) -> Result<(), String> {
    Err("sharing is only available on macOS".into())
}

pub fn airdrop(_paths: &[PathBuf]) -> Result<(), String> {
    Err("AirDrop is only available on macOS".into())
}
