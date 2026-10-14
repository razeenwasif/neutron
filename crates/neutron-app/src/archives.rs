//! Browsing inside archives: listing a folder of one, and making real files of
//! the entries that something outside Neutron has to read.
//!
//! An archive is read once per visit, not once per folder: its index is kept
//! by the loader, keyed by the file and its date, so going in and out of
//! folders inside it costs nothing, and a changed archive is read afresh.
//!
//! Opening, previewing, copying out and Quick Look all need a file on disk.
//! Those entries are extracted into the cache, under a folder named for the
//! archive and its date, and reused while the archive is unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use neutron_archive::browse::Index;
use neutron_core::entry::{Entry, EntryKind, EntryList, SyncState};

/// Indexes read so far, kept by the loader's worker.
#[derive(Default)]
pub struct Archives {
    indexes: HashMap<PathBuf, (Option<SystemTime>, Arc<Index>)>,
}

impl Archives {
    /// The folder at `inner` inside `archive`.
    pub fn list(&mut self, archive: &Path, inner: &str) -> Result<EntryList, String> {
        let index = self.index(archive)?;
        let listed = index
            .list(inner)
            .ok_or_else(|| format!("{} has no folder called {inner}", display(archive)))?;
        let mut list = EntryList::with_capacity(listed.len());
        for item in listed {
            list.push(&Entry {
                name: item.name,
                kind: if item.is_dir { EntryKind::Directory } else { EntryKind::File },
                size: item.size,
                modified: item.modified,
                created: item.modified,
                attrs: 0,
                sync: SyncState::None,
            });
        }
        Ok(list)
    }

    fn index(&mut self, archive: &Path) -> Result<Arc<Index>, String> {
        let stamp = modified(archive);
        if let Some((seen, index)) = self.indexes.get(archive) {
            if *seen == stamp {
                return Ok(index.clone());
            }
        }
        let index = Arc::new(
            Index::read(archive).map_err(|e| format!("{} could not be read: {e}", display(archive)))?,
        );
        // A handful of archives at a time is all anyone browses; the oldest
        // reading goes rather than letting a long session keep them all.
        if self.indexes.len() >= 8 {
            if let Some(key) = self.indexes.keys().next().cloned() {
                self.indexes.remove(&key);
            }
        }
        self.indexes.insert(archive.to_path_buf(), (stamp, index.clone()));
        Ok(index)
    }
}

/// Real files for the entries at `inner` inside `archive`, extracted into the
/// cache if they are not there already. Blocks; call it from a worker.
pub fn materialise(archive: &Path, inner: &[String]) -> Result<Vec<PathBuf>, String> {
    materialise_in(&cache_root(archive)?, archive, inner)
}

fn materialise_in(root: &Path, archive: &Path, inner: &[String]) -> Result<Vec<PathBuf>, String> {
    let format = neutron_archive::format_of(archive).ok_or("that is not an archive Neutron can open")?;
    let root = root.to_path_buf();
    let mut out = Vec::with_capacity(inner.len());
    for entry in inner {
        let (path, _) = neutron_archive::browse::normalise(entry);
        let (parent, base) = match path.rsplit_once('/') {
            Some((parent, base)) => (root.join(parent), base.to_owned()),
            None => (root.clone(), path.clone()),
        };
        let target = parent.join(&base);
        if !target.exists() {
            let summary = neutron_archive::extract::extract_selected(
                archive,
                &parent,
                format,
                std::slice::from_ref(&path),
                |_| neutron_archive::Continue::Yes,
            )
            .map_err(|e| e.to_string())?;
            if let Some(refused) = summary.refused.first() {
                return Err(refused.clone());
            }
            if !target.exists() {
                return Err(format!("{base} is not in {}", display(archive)));
            }
        }
        out.push(target);
    }
    Ok(out)
}

/// `inner`, joined with `name`: the path of a row inside an archive folder.
pub fn join(inner: &str, name: &str) -> String {
    if inner.is_empty() { name.to_owned() } else { format!("{inner}/{name}") }
}

fn cache_root(archive: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("there is no home folder for the cache")?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(archive.as_os_str().as_encoded_bytes());
    let stamp = modified(archive)
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    feed(&stamp.to_le_bytes());
    Ok(PathBuf::from(home).join("Library/Caches/Neutron/archives").join(format!("{hash:016x}")))
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|m| m.modified()).ok()
}

fn display(archive: &Path) -> String {
    archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_listed_and_taken_out_once() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("neutron-archives-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            zip.start_file("docs/one.txt", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"one").unwrap();
            zip.finish().unwrap();
        }
        let mut archives = Archives::default();
        let top = archives.list(&archive, "").unwrap();
        assert_eq!((top.len(), top.name(0), top.kind(0)), (1, "docs", EntryKind::Directory));
        assert!(archives.list(&archive, "missing").is_err());

        let root = dir.join("cache");
        let paths = materialise_in(&root, &archive, &["docs/one.txt".to_owned()]).unwrap();
        assert_eq!(paths, vec![root.join("docs/one.txt")]);
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "one");
        // A second time finds it there rather than extracting again.
        std::fs::write(&paths[0], "kept").unwrap();
        let again = materialise_in(&root, &archive, &["docs/one.txt".to_owned()]).unwrap();
        assert_eq!(std::fs::read_to_string(&again[0]).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_inside_an_archive_is_joined_without_a_leading_slash() {
        assert_eq!(join("", "a.txt"), "a.txt");
        assert_eq!(join("docs", "a.txt"), "docs/a.txt");
    }
}
