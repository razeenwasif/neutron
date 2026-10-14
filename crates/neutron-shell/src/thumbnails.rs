//! Previews of what is inside a file — the picture, the first page of a PDF, a
//! frame of a film — as the system's own preview service draws them.
//!
//! The previews are written as PNG files to paths the caller chooses, which is
//! what makes them a disk cache for free: the app names each file after what
//! it is a preview of, and a preview already on disk is never asked for again.

use std::path::PathBuf;

/// Asks for a preview of each of `paths`, at most `px` pixels on its longer
/// side, written as a PNG to the matching entry of `outputs`.
///
/// Blocks for as long as the previews take, so it belongs on a worker. The
/// answer is, per path, whether a preview was written: false where the system
/// has nothing better than the file's icon.
#[cfg(target_os = "macos")]
pub fn render(paths: &[PathBuf], outputs: &[PathBuf], px: u32) -> Result<Vec<bool>, String> {
    let result = crate::macos::call(serde_json::json!({
        "action": "thumbnails",
        "paths": crate::macos::path_strs(paths)?,
        "outputs": crate::macos::path_strs(outputs)?,
        "size": px,
    }))?;
    let made: Vec<bool> = result
        .as_array()
        .ok_or("macOS services returned no previews")?
        .iter()
        .map(|v| v.as_bool().unwrap_or(false))
        .collect();
    if made.len() != paths.len() {
        return Err("macOS services returned the wrong number of previews".into());
    }
    Ok(made)
}

/// Nowhere else has a preview service wired up yet; every file keeps its tile.
#[cfg(not(target_os = "macos"))]
pub fn render(paths: &[PathBuf], _outputs: &[PathBuf], _px: u32) -> Result<Vec<bool>, String> {
    Ok(vec![false; paths.len()])
}
