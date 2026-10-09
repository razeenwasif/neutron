//! Writing to a server: new folders, renames, deletes and uploads.
//!
//! # On their own connection
//!
//! Like a download, each operation opens a connection of its own on a worker
//! and closes it afterwards. The loader's connections serve every remote tab's
//! listings from one thread, and an upload of a large folder on that thread
//! would leave every server pane frozen until it finished.
//!
//! # Deleting a folder is a walk
//!
//! Neither protocol deletes a directory with anything in it, so removing one
//! means listing it, removing what is inside, and then the folder itself. The
//! walk never follows a symbolic link: deleting "through" a link to another
//! directory would empty a folder the user never selected. A link is removed
//! as the entry it is.
//!
//! # Uploads never replace
//!
//! The transport's upload overwrites whatever is at the path, and a server has
//! no bin to get the old file back from. So names that already exist in the
//! destination are refused before anything is sent — checked by the caller
//! against the listing on screen — and inside a folder being uploaded nothing
//! can collide, because the folder is new.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use neutron_core::EntryKind;
use neutron_remote::{Continue, Site, Transport, path};

/// One write, against the folder `dir` on a server.
#[derive(Debug, Clone)]
pub enum Op {
    CreateDir { name: String },
    Rename { from: String, to: String },
    /// Names in `dir`, each with whether it is a directory (and not a link).
    Delete { entries: Vec<(String, EntryKind)> },
    /// Local files and folders, copied into `dir` under their own names.
    Upload { sources: Vec<PathBuf> },
}

/// Progress a running operation reports, and how it is asked to stop.
#[derive(Default)]
pub struct Progress {
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

/// Runs `op` on its own connection. Returns a receipt for the status line.
///
/// **Worker thread only** — it connects to a server.
pub fn run(site: &Site, dir: &str, op: &Op, progress: &Progress) -> Result<String, String> {
    let mut transport = crate::remote::connect_checked(site)?;
    let t = transport.as_mut();
    match op {
        Op::CreateDir { name } => {
            t.create_dir(&join(dir, name)?).map_err(|e| e.to_string())?;
            Ok(format!("Created {name}"))
        }
        Op::Rename { from, to } => {
            t.rename(&join(dir, from)?, &join(dir, to)?).map_err(|e| e.to_string())?;
            Ok(format!("Renamed to {to}"))
        }
        Op::Delete { entries } => {
            let mut removed = 0;
            for (name, kind) in entries {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(format!("Stopped after deleting {removed} items"));
                }
                removed += delete(t, &join(dir, name)?, *kind, progress)?;
            }
            Ok(format!("Deleted {} from the server", count(entries.len(), "item")))
        }
        Op::Upload { sources } => {
            let mut sent = 0;
            for source in sources {
                sent += upload(t, source, dir, progress)?;
            }
            Ok(format!("Uploaded {}", count(sent, "file")))
        }
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn join(dir: &str, name: &str) -> Result<String, String> {
    path::join(dir, name).map_err(|e| format!("{name}: {e}"))
}

/// Removes one entry, and everything inside it if it is a directory. Returns
/// how many entries went.
fn delete(t: &mut dyn Transport, at: &str, kind: EntryKind, progress: &Progress) -> Result<usize, String> {
    // Only a real directory is walked. A link — to a directory or anything
    // else — is removed as itself.
    if kind != EntryKind::Directory {
        t.remove_file(at).map_err(|e| format!("{}: {e}", path::file_name(at)))?;
        return Ok(1);
    }

    let mut refused = Vec::new();
    let children = t.list(at, &mut refused).map_err(|e| e.to_string())?;
    if !refused.is_empty() {
        // The folder cannot be emptied if Neutron will not name what is in it,
        // and deleting the rest would leave it half gone for nothing.
        return Err(format!(
            "{} holds names Neutron will not touch, so it was left as it was",
            path::file_name(at)
        ));
    }

    let mut removed = 0;
    for child in children {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(format!("Stopped after deleting {removed} items"));
        }
        removed += delete(t, &join(at, &child.name)?, child.kind, progress)?;
    }
    t.remove_dir(at).map_err(|e| format!("{}: {e}", path::file_name(at)))?;
    Ok(removed + 1)
}

/// Sends one local file or folder into the remote directory `into`. Returns
/// how many files went.
fn upload(t: &mut dyn Transport, source: &Path, into: &str, progress: &Progress) -> Result<usize, String> {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has a name a server cannot be sent", source.display()))?;
    let target = join(into, name)?;

    // Links are followed for files, which is what copying one means, and not
    // for folders, where a link back up the tree would upload forever.
    let link = std::fs::symlink_metadata(source).map_err(|e| format!("{name}: {e}"))?;
    let meta = std::fs::metadata(source).map_err(|e| format!("{name}: {e}"))?;

    if meta.is_dir() {
        if link.file_type().is_symlink() {
            return Ok(0);
        }
        t.create_dir(&target).map_err(|e| format!("{name}: {e}"))?;
        let mut sent = 0;
        let mut children: Vec<PathBuf> = std::fs::read_dir(source)
            .map_err(|e| format!("{name}: {e}"))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        children.sort();
        for child in children {
            sent += upload(t, &child, &target, progress)?;
        }
        return Ok(sent);
    }

    let mut file = std::fs::File::open(source).map_err(|e| format!("{name}: {e}"))?;
    let start = progress.bytes.load(Ordering::Relaxed);
    let outcome = t.upload(&target, &mut file, &mut |done| {
        progress.bytes.store(start + done, Ordering::Relaxed);
        if progress.cancel.load(Ordering::Relaxed) { Continue::Stop } else { Continue::Yes }
    });
    match outcome {
        Ok(_) if progress.cancel.load(Ordering::Relaxed) => {
            let _ = t.remove_file(&target);
            Err(format!("Stopped while sending {name}; the partial copy was removed"))
        }
        Ok(_) => Ok(1),
        Err(e) => {
            // A partial file is worse than none: it looks like the real thing.
            let _ = t.remove_file(&target);
            Err(format!("{name}: {e}"))
        }
    }
}

/// The names in `wanted` that already exist in a listing — what an upload or
/// a new name would overwrite.
pub fn collisions<'a>(wanted: impl IntoIterator<Item = &'a str>, existing: &[&str]) -> Vec<String> {
    wanted
        .into_iter()
        .filter(|name| existing.contains(name))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use neutron_remote::{RemoteEntry, RemoteError};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};

    /// A server in memory: paths to entries, files holding their bytes.
    #[derive(Default)]
    struct Fake {
        entries: BTreeMap<String, (EntryKind, Vec<u8>)>,
        fail_uploads: bool,
    }

    impl Fake {
        fn with(paths: &[(&str, EntryKind)]) -> Self {
            let mut fake = Fake::default();
            for (p, kind) in paths {
                fake.entries.insert((*p).to_owned(), (*kind, Vec::new()));
            }
            fake
        }
        fn children(&self, dir: &str) -> Vec<String> {
            let prefix = if dir.ends_with('/') { dir.to_owned() } else { format!("{dir}/") };
            self.entries
                .keys()
                .filter(|p| p.starts_with(&prefix) && !p[prefix.len()..].contains('/'))
                .cloned()
                .collect()
        }
    }

    impl Transport for Fake {
        fn list(&mut self, path: &str, _: &mut Vec<String>) -> Result<Vec<RemoteEntry>, RemoteError> {
            Ok(self
                .children(path)
                .into_iter()
                .map(|p| RemoteEntry {
                    name: p.rsplit('/').next().unwrap().to_owned(),
                    kind: self.entries[&p].0,
                    size: 0,
                    modified: 0,
                })
                .collect())
        }
        fn download(&mut self, _: &str, _: &mut dyn Write, _: &mut dyn FnMut(u64) -> Continue) -> Result<u64, RemoteError> {
            unreachable!()
        }
        fn create_dir(&mut self, path: &str) -> Result<(), RemoteError> {
            self.entries.insert(path.to_owned(), (EntryKind::Directory, Vec::new()));
            Ok(())
        }
        fn rename(&mut self, _: &str, _: &str) -> Result<(), RemoteError> {
            unreachable!()
        }
        fn remove_file(&mut self, path: &str) -> Result<(), RemoteError> {
            self.entries.remove(path);
            Ok(())
        }
        fn remove_dir(&mut self, path: &str) -> Result<(), RemoteError> {
            if !self.children(path).is_empty() {
                return Err(RemoteError::Server { path: path.into(), reason: "not empty".into() });
            }
            self.entries.remove(path);
            Ok(())
        }
        fn upload(&mut self, path: &str, source: &mut dyn Read, _: &mut dyn FnMut(u64) -> Continue) -> Result<u64, RemoteError> {
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes).unwrap();
            self.entries.insert(path.to_owned(), (EntryKind::File, bytes.clone()));
            if self.fail_uploads {
                return Err(RemoteError::Server { path: path.into(), reason: "disk full".into() });
            }
            Ok(bytes.len() as u64)
        }
    }

    #[test]
    fn deleting_a_folder_empties_it_first_but_never_through_a_link() {
        let mut fake = Fake::with(&[
            ("/pub/old", EntryKind::Directory),
            ("/pub/old/a.txt", EntryKind::File),
            ("/pub/old/inner", EntryKind::Directory),
            ("/pub/old/inner/b.txt", EntryKind::File),
            // A link inside the folder, pointing somewhere that must survive.
            ("/pub/old/elsewhere", EntryKind::Symlink),
            ("/keep/precious.txt", EntryKind::File),
        ]);
        let removed = delete(&mut fake, "/pub/old", EntryKind::Directory, &Progress::default()).unwrap();
        assert_eq!(removed, 5);
        assert_eq!(fake.entries.keys().collect::<Vec<_>>(), ["/keep/precious.txt"]);
    }

    #[test]
    fn a_folder_uploads_with_everything_in_it() {
        let root = std::env::temp_dir().join(format!("neutron-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("project/src")).unwrap();
        std::fs::write(root.join("project/readme.md"), "hi").unwrap();
        std::fs::write(root.join("project/src/main.rs"), "fn main() {}").unwrap();

        let mut fake = Fake::default();
        let sent = upload(&mut fake, &root.join("project"), "/pub", &Progress::default()).unwrap();
        assert_eq!(sent, 2);
        assert_eq!(fake.entries["/pub/project/src/main.rs"].1, b"fn main() {}");
        assert_eq!(fake.entries["/pub/project"].0, EntryKind::Directory);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_upload_leaves_no_partial_file_behind() {
        let root = std::env::temp_dir().join(format!("neutron-upload-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("big.bin"), vec![0u8; 1000]).unwrap();

        let mut fake = Fake { fail_uploads: true, ..Default::default() };
        assert!(upload(&mut fake, &root.join("big.bin"), "/pub", &Progress::default()).is_err());
        assert!(!fake.entries.contains_key("/pub/big.bin"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_name_already_on_the_server_is_a_collision() {
        let existing = ["a.txt", "docs"];
        assert_eq!(collisions(["a.txt", "b.txt"], &existing), ["a.txt"]);
        assert!(collisions(["new"], &existing).is_empty());
    }

    #[test]
    fn counts_read_as_words() {
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(3, "item"), "3 items");
    }
}
