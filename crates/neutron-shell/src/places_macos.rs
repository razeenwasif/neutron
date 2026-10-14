use neutron_core::{NodeId, places::{Capacity, Place, PlaceKind}};
use std::path::{Path, PathBuf};
pub fn home() -> Option<PathBuf> { std::env::var_os("HOME").map(PathBuf::from) }
fn place(name: &str, path: &Path, kind: PlaceKind) -> Place {
    let capacity = if kind == PlaceKind::Drive { volume(path).and_then(|(c, _)| c) } else { None };
    Place { name: name.into(), id: NodeId::path(path), kind, capacity }
}
/// Capacity and whether the mount is browsable (not MNT_DONTBROWSE). `None`
/// when the helper failed or the path cannot be passed to it.
fn volume(path: &Path) -> Option<(Option<Capacity>, bool)> {
    let path = crate::macos::path_str(path).ok()?;
    let v = crate::macos::call(serde_json::json!({"action":"capacity", "destination":path})).ok()?;
    let capacity = Some(Capacity { total: v["total"].as_u64()?, free: v["free"].as_u64()? });
    Some((capacity, v["browsable"] != false))
}
/// Whether the volume at `path` can be ejected: removable media, a disk image,
/// a network volume. Asks the system, so it belongs on a worker.
pub fn ejectable(path: &Path) -> bool {
    let Ok(path) = crate::macos::path_str(path) else { return false };
    crate::macos::call(serde_json::json!({"action":"capacity", "destination":path}))
        .ok()
        .is_some_and(|v| v["ejectable"] == true)
}

/// Ejects the volume at `path`, or unmounts it if it is on the network.
/// Blocks until macOS answers — a busy disk can take seconds — so run it on a
/// worker.
pub fn eject(path: &Path) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    let text = crate::macos::path_str(path)?;
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        // Documented as safe off the main thread, like the rest of
        // NSWorkspace's file operations.
        NSWorkspace::sharedWorkspace()
            .unmountAndEjectDeviceAtURL_error(&url)
            .map_err(|e| e.localizedDescription().to_string())
    })
}

pub fn known_folders() -> Vec<Place> {
    let Some(home) = home() else { return vec![]; };
    let mut out = vec![place("Home", &home, PlaceKind::KnownFolder)];
    for name in ["Desktop", "Downloads", "Documents", "Pictures", "Music", "Movies"] {
        let path = home.join(name);
        if path.is_dir() { out.push(place(name, &path, PlaceKind::KnownFolder)); }
    }
    out.push(place("Applications", Path::new("/Applications"), PlaceKind::KnownFolder));
    out
}
pub fn cloud_folders() -> Vec<Place> {
    let Some(home) = home() else { return vec![]; };
    let mut out = vec![];
    let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    if icloud.is_dir() { out.push(place("iCloud Drive", &icloud, PlaceKind::Cloud)); }
    if let Ok(entries) = std::fs::read_dir(home.join("Library/CloudStorage")) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                out.push(place(&entry.file_name().to_string_lossy(), &entry.path(), PlaceKind::Cloud));
            }
        }
    }
    out
}
pub fn wsl_distributions() -> Vec<Place> { vec![] }
pub fn drives() -> Vec<Place> {
    let mut out = vec![];
    drives_streaming(|p| out.push(p));
    out
}
pub fn drives_streaming(mut emit: impl FnMut(Place)) {
    emit(place("Macintosh HD", Path::new("/"), PlaceKind::Drive));
    let Ok(entries) = std::fs::read_dir("/Volumes") else { return };
    for entry in entries.flatten() {
        let (name, path) = (entry.file_name(), entry.path());
        // Dot-entries and hidden system mounts (Recovery, VM) are not drives.
        if name.as_encoded_bytes().starts_with(b".") || !path.is_dir()
            || path.canonicalize().ok().as_deref() == Some(Path::new("/")) { continue; }
        let (capacity, browsable) = volume(&path).unwrap_or((None, true));
        if browsable {
            emit(Place { name: name.to_string_lossy().into(), id: NodeId::path(&path), kind: PlaceKind::Drive, capacity });
        }
    }
}
