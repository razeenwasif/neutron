//! Put Back: where something in the Trash came from.
//!
//! Two records answer it. The Finder's: when it moves an item to the Trash it
//! writes the folder and name it came from into the Trash's `.DS_Store`, as
//! `ptbL` and `ptbN`. And Neutron's own, kept in `trashed.json`, because what
//! Neutron trashes goes through the system's move to the Trash, which writes
//! nothing there.
//!
//! Reading the Trash at all needs Full Disk Access: macOS keeps it from
//! other apps. Without it, Put Back says so and how to grant it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn ledger_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Neutron/trashed.json"))
}

fn read_ledger() -> HashMap<PathBuf, PathBuf> {
    ledger_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_ledger(ledger: &HashMap<PathBuf, PathBuf>) {
    let Some(path) = ledger_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(ledger) {
        let _ = std::fs::write(path, text);
    }
}

/// Notes where items Neutron moved to the Trash came from: `(was, in_trash)`.
/// Blocks on the disk; called on the worker that trashed them.
pub fn remember(moved: &[(PathBuf, PathBuf)]) {
    if moved.is_empty() {
        return;
    }
    let mut ledger = read_ledger();
    // What has left the Trash since is forgotten, so the file stays small.
    ledger.retain(|in_trash, _| in_trash.symlink_metadata().is_ok());
    for (was, in_trash) in moved {
        ledger.insert(in_trash.clone(), was.clone());
    }
    write_ledger(&ledger);
}

fn forget(in_trash: &Path) {
    let mut ledger = read_ledger();
    if ledger.remove(in_trash).is_some() {
        write_ledger(&ledger);
    }
}

/// Where `in_trash` came from, if either record knows.
pub fn origin(in_trash: &Path) -> Option<PathBuf> {
    if let Some(was) = read_ledger().get(in_trash) {
        return Some(was.clone());
    }
    let trash = in_trash.parent()?;
    let name = in_trash.file_name()?.to_string_lossy().into_owned();
    let store = std::fs::read(trash.join(".DS_Store")).ok()?;
    let records = ds_store_records(&store)?;
    let folder = records.iter().find(|r| r.0 == name && r.1 == *b"ptbL").and_then(|r| r.2.clone())?;
    let original = records.iter().find(|r| r.0 == name && r.1 == *b"ptbN").and_then(|r| r.2.clone()).unwrap_or(name);
    // The folder is written from the root of the volume the Trash is on.
    let root = volume_root(trash);
    Some(root.join(folder.trim_start_matches('/')).join(original))
}

/// The root of the volume `path` is on: `/` for the startup disk's Trash,
/// the volume for a `.Trashes` folder on another.
fn volume_root(path: &Path) -> PathBuf {
    let mut current = path;
    while let Some(parent) = current.parent() {
        if current.file_name().is_some_and(|n| n == ".Trashes") {
            return parent.to_path_buf();
        }
        current = parent;
    }
    PathBuf::from("/")
}

/// What putting an item back did.
pub enum PutBack {
    Done(PathBuf, PathBuf),
    /// macOS would not let Neutron take it out of the Trash.
    Blocked,
    Failed(String),
}

/// Moves `in_trash` back where it came from. Blocks on the disk.
pub fn put_back(in_trash: &Path) -> PutBack {
    let Some(was) = origin(in_trash) else {
        let name = in_trash.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return PutBack::Failed(format!("{name}: where it came from is not recorded"));
    };
    if let Some(dir) = was.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match neutron_shell::transfer::relocate(in_trash, &was) {
        Ok(()) => {
            forget(in_trash);
            PutBack::Done(in_trash.to_path_buf(), was)
        }
        Err(neutron_shell::transfer::RelocateError::NotPermitted(_)) => PutBack::Blocked,
        Err(e) => PutBack::Failed(format!("{}: {e}", was.display())),
    }
}

/// System Settings, at Full Disk Access.
pub const FULL_DISK_ACCESS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

// --- .DS_Store ---------------------------------------------------------------

/// A record: the item it is about, its four-letter code, and its value when
/// it is text.
type Record = (String, [u8; 4], Option<String>);

/// Every record in a `.DS_Store`: the B-tree of the `DSDB` directory, walked.
fn ds_store_records(data: &[u8]) -> Option<Vec<Record>> {
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    // Everything after the first four bytes is addressed from there.
    if data.get(4..8)? != b"Bud1" {
        return None;
    }
    let root = u32_at(8)? as usize + 4;
    let blocks = u32_at(root)? as usize;
    let addresses: Vec<u32> = (0..blocks).map(|i| u32_at(root + 8 + i * 4)).collect::<Option<_>>()?;
    // The address table is padded to a multiple of 256 entries.
    let mut at = root + 8 + blocks.div_ceil(256).max(1) * 256 * 4;
    let dirs = u32_at(at)? as usize;
    at += 4;
    let mut dsdb = None;
    for _ in 0..dirs {
        let len = *data.get(at)? as usize;
        let name = data.get(at + 1..at + 1 + len)?;
        let block = u32_at(at + 1 + len)?;
        if name == b"DSDB" {
            dsdb = Some(block);
        }
        at += 1 + len + 4;
    }
    let block = |id: u32| -> Option<usize> { Some((*addresses.get(id as usize)? & !0x1f) as usize + 4) };
    let header = block(dsdb?)?;
    let mut records = Vec::new();
    let mut pending = vec![u32_at(header)?];
    let mut guard = 0;
    while let Some(node) = pending.pop() {
        guard += 1;
        if guard > 100_000 {
            return None;
        }
        let mut at = block(node)?;
        let rightmost = u32_at(at)?;
        let count = u32_at(at + 4)? as usize;
        at += 8;
        for _ in 0..count {
            if rightmost != 0 {
                pending.push(u32_at(at)?);
                at += 4;
            }
            let (record, next) = read_record(data, at)?;
            records.push(record);
            at = next;
        }
        if rightmost != 0 {
            pending.push(rightmost);
        }
    }
    Some(records)
}

fn read_record(data: &[u8], mut at: usize) -> Option<(Record, usize)> {
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let utf16 = |at: usize, chars: usize| -> Option<String> {
        let bytes = data.get(at..at + chars * 2)?;
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    };
    let len = u32_at(at)? as usize;
    let name = utf16(at + 4, len)?;
    at += 4 + len * 2;
    let code: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
    let kind = data.get(at + 4..at + 8)?;
    at += 8;
    let (value, next) = match kind {
        b"bool" => (None, at + 1),
        b"long" | b"shor" | b"type" => (None, at + 4),
        b"comp" | b"dutc" => (None, at + 8),
        b"blob" => (None, at + 4 + u32_at(at)? as usize),
        b"ustr" => {
            let chars = u32_at(at)? as usize;
            (Some(utf16(at + 4, chars)?), at + 4 + chars * 2)
        }
        _ => return None,
    };
    Some(((name, code, value), next))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.DS_Store` with one leaf of records, laid out as the Finder lays
    /// one out: header, the root block, the `DSDB` header, the leaf.
    fn ds_store(records: &[(&str, &[u8; 4], &str)]) -> Vec<u8> {
        let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect() };
        let mut leaf = Vec::new();
        leaf.extend(0u32.to_be_bytes()); // a leaf
        leaf.extend((records.len() as u32).to_be_bytes());
        for (name, code, value) in records {
            leaf.extend((name.encode_utf16().count() as u32).to_be_bytes());
            leaf.extend(utf16(name));
            leaf.extend(code.iter());
            leaf.extend(b"ustr");
            leaf.extend((value.encode_utf16().count() as u32).to_be_bytes());
            leaf.extend(utf16(value));
        }
        // Blocks, each at an offset (from byte 4) that is a multiple of 32,
        // their size as a power of two in the address's low bits.
        let place = |offset: usize, size: usize| (offset as u32) | size.next_power_of_two().trailing_zeros();
        let (root_at, dsdb_at, leaf_at) = (0x20usize, 0x1000usize, 0x1100usize);
        let mut root = Vec::new();
        root.extend(3u32.to_be_bytes());
        root.extend(0u32.to_be_bytes());
        root.extend(place(root_at, 0x1000).to_be_bytes());
        root.extend(place(dsdb_at, 32).to_be_bytes());
        root.extend(place(leaf_at, leaf.len()).to_be_bytes());
        root.resize(8 + 256 * 4, 0);
        root.extend(1u32.to_be_bytes());
        root.push(4);
        root.extend(b"DSDB");
        root.extend(1u32.to_be_bytes());
        let mut dsdb = Vec::new();
        dsdb.extend(2u32.to_be_bytes()); // the root node is block 2
        dsdb.extend([0u8; 16]);
        let mut file = vec![0u8; 4 + leaf_at + leaf.len()];
        file[0..4].copy_from_slice(&1u32.to_be_bytes());
        file[4..8].copy_from_slice(b"Bud1");
        file[8..12].copy_from_slice(&(root_at as u32).to_be_bytes());
        file[4 + root_at..4 + root_at + root.len()].copy_from_slice(&root);
        file[4 + dsdb_at..4 + dsdb_at + dsdb.len()].copy_from_slice(&dsdb);
        file[4 + leaf_at..].copy_from_slice(&leaf);
        file
    }

    #[test]
    fn the_finders_put_back_record_is_read() {
        let store = ds_store(&[("report 2.pdf", b"ptbL", "Users/me/Documents/"), ("report 2.pdf", b"ptbN", "report.pdf")]);
        let records = ds_store_records(&store).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], ("report 2.pdf".into(), *b"ptbL", Some("Users/me/Documents/".into())));

        let dir = std::env::temp_dir().join(format!("neutron-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".DS_Store"), &store).unwrap();
        assert_eq!(origin(&dir.join("report 2.pdf")), Some(PathBuf::from("/Users/me/Documents/report.pdf")));
        assert_eq!(origin(&dir.join("other.txt")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trash_on_another_disk_is_reckoned_from_that_disk() {
        assert_eq!(volume_root(Path::new("/Volumes/Stick/.Trashes/501")), PathBuf::from("/Volumes/Stick"));
        assert_eq!(volume_root(Path::new("/Users/me/.Trash")), PathBuf::from("/"));
        assert!(ds_store_records(b"not a store at all").is_none());
    }
}
