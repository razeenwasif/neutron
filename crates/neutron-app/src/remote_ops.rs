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
    CreateFile { name: String, contents: Vec<u8> },
    Rename { from: String, to: String },
    /// Names in `dir`, each with whether it is a directory (and not a link).
    Delete { entries: Vec<(String, EntryKind)> },
    /// Local files and folders, copied into `dir` under their own names.
    Upload { sources: Vec<PathBuf> },
    /// Same-server move from `from_dir` into `dir` via remote rename.
    MoveRemote { from_dir: String, entries: Vec<(String, EntryKind)> },
    /// Same-server copy from `from_dir` into `dir` via SSH `cp -a` / `Copy-Item`.
    CopyRemote { from_dir: String, entries: Vec<(String, EntryKind)> },
}

/// Progress a running operation reports, and how it is asked to stop.
pub struct Progress {
    pub bytes: AtomicU64,
    pub items_done: AtomicU64,
    pub items_total: AtomicU64,
    pub started: std::time::Instant,
    pub cancel: AtomicBool,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            items_done: AtomicU64::new(0),
            items_total: AtomicU64::new(0),
            started: std::time::Instant::now(),
            cancel: AtomicBool::new(false),
        }
    }
}

impl Progress {
    /// Average throughput in bytes/sec once the transfer has run briefly.
    pub fn throughput_bps(&self) -> f64 {
        let elapsed = self.started.elapsed().as_secs_f64();
        if elapsed < 0.15 {
            0.0
        } else {
            self.bytes.load(Ordering::Relaxed) as f64 / elapsed
        }
    }
}

/// Runs `op` on its own connection. Returns a receipt for the status line.
///
/// **Worker thread only** — it connects to a server.
pub fn run(site: &Site, dir: &str, op: &Op, progress: &Progress) -> Result<String, String> {
    let mut transport = crate::remote::connect_checked(site)?;
    run_on(transport.as_mut(), dir, op, progress)
}

/// Uploads a single local file to `remote_path` on its own connection,
/// overwriting what is there (used to sync an edited local copy back).
///
/// **Worker thread only** — it connects to a server.
pub fn upload_file(site: &Site, remote_path: &str, local_path: &Path) -> Result<(), String> {
    let mut file = std::fs::File::open(local_path).map_err(|e| e.to_string())?;
    let mut transport = crate::remote::connect_checked(site)?;
    transport
        .upload(remote_path, &mut file, &mut |_| Continue::Yes)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn run_on(t: &mut dyn Transport, dir: &str, op: &Op, progress: &Progress) -> Result<String, String> {
    match op {
        Op::CreateDir { name } => {
            t.create_dir(&join(dir, name)?).map_err(|e| e.to_string())?;
            Ok(format!("Created {name}"))
        }
        Op::CreateFile { name, contents } => {
            let target = join(dir, name)?;
            let mut cursor = std::io::Cursor::new(contents.as_slice());
            match t.upload(&target, &mut cursor, &mut |_| Continue::Yes) {
                Ok(_) => Ok(format!("Created {name}")),
                Err(e) => {
                    let _ = t.remove_file(&target);
                    Err(e.to_string())
                }
            }
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
        Op::MoveRemote { from_dir, entries } => {
            let mut moved = 0;
            progress.items_total.store(entries.len() as u64, Ordering::Relaxed);
            for (name, _) in entries {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(format!("Stopped after moving {moved} items"));
                }
                let src = join(from_dir, name)?;
                let dst = join(dir, name)?;
                t.rename(&src, &dst).map_err(|e| format!("{name}: {e}"))?;
                moved += 1;
                progress.items_done.store(moved as u64, Ordering::Relaxed);
            }
            Ok(format!("Moved {} on the server", count(moved, "item")))
        }
        Op::CopyRemote { from_dir, entries } => {
            let mut copied = 0;
            progress.items_total.store(entries.len() as u64, Ordering::Relaxed);
            for (name, _) in entries {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(format!("Stopped after copying {copied} items"));
                }
                let src = join(from_dir, name)?;
                let dst = join(dir, name)?;
                t.copy_remote(&src, &dst).map_err(|e| format!("{name}: {e}"))?;
                copied += 1;
                progress.items_done.store(copied as u64, Ordering::Relaxed);
            }
            Ok(format!("Copied {} on the server", count(copied, "item")))
        }
    }
}

/// Downloads selected files and folders from `site` into `local_dest`,
/// optionally deleting the remote originals (`cut`) once every item lands.
///
/// **Worker thread only.**
pub fn download_entries(
    site: &Site,
    remote_dir: &str,
    entries: &[(String, EntryKind)],
    local_dest: &Path,
    cut: bool,
    progress: &Progress,
) -> Result<String, String> {
    download_entries_opts(site, remote_dir, entries, local_dest, cut, false, progress)
}

pub fn download_entries_sync(
    site: &Site,
    remote_dir: &str,
    entries: &[(String, EntryKind)],
    local_dest: &Path,
    progress: &Progress,
) -> Result<String, String> {
    download_entries_opts(site, remote_dir, entries, local_dest, false, true, progress)
}

fn download_entries_opts(
    site: &Site,
    remote_dir: &str,
    entries: &[(String, EntryKind)],
    local_dest: &Path,
    cut: bool,
    overwrite: bool,
    progress: &Progress,
) -> Result<String, String> {
    let mut transport = crate::remote::connect_checked(site)?;
    download_entries_on_opts(transport.as_mut(), remote_dir, entries, local_dest, cut, overwrite, progress)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn download_entries_on(
    t: &mut dyn Transport,
    remote_dir: &str,
    entries: &[(String, EntryKind)],
    local_dest: &Path,
    cut: bool,
    progress: &Progress,
) -> Result<String, String> {
    download_entries_on_opts(t, remote_dir, entries, local_dest, cut, false, progress)
}

pub(crate) fn download_entries_on_opts(
    t: &mut dyn Transport,
    remote_dir: &str,
    entries: &[(String, EntryKind)],
    local_dest: &Path,
    cut: bool,
    overwrite: bool,
    progress: &Progress,
) -> Result<String, String> {
    let mut clashes = Vec::new();
    for (name, _) in entries {
        path::safe_name(name).map_err(|e| format!("{name}: {e}"))?;
        if !overwrite && local_dest.join(name).exists() {
            clashes.push(name.as_str());
        }
    }
    if !clashes.is_empty() {
        return Err(format!(
            "{} already here, so nothing was downloaded: {}",
            if clashes.len() == 1 { "this name is" } else { "these names are" },
            clashes.join(", ")
        ));
    }

    progress.items_total.store(entries.len() as u64, Ordering::Relaxed);
    let mut files = 0usize;
    for (idx, (name, kind)) in entries.iter().enumerate() {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(format!("Stopped after downloading {}", count(files, "file")));
        }
        let remote_path = join(remote_dir, name)?;
        let target = local_dest.join(name);
        files += download_one(t, &remote_path, *kind, &target, progress)?;
        progress.items_done.store((idx + 1) as u64, Ordering::Relaxed);
    }

    if cut {
        for (name, kind) in entries {
            let remote_path = join(remote_dir, name)?;
            delete(t, &remote_path, *kind, progress)?;
        }
    }

    let verb = if cut { "Moved" } else { "Downloaded" };
    Ok(format!("{verb} {}", count(files, "file")))
}

fn download_one(
    t: &mut dyn Transport,
    remote_path: &str,
    kind: EntryKind,
    local_target: &Path,
    progress: &Progress,
) -> Result<usize, String> {
    match kind {
        EntryKind::Symlink | EntryKind::Junction | EntryKind::Drive | EntryKind::Virtual => Ok(0),
        EntryKind::Directory => {
            std::fs::create_dir_all(local_target)
                .map_err(|e| format!("{}: {e}", local_target.display()))?;
            let mut refused = Vec::new();
            let children = t.list(remote_path, &mut refused).map_err(|e| e.to_string())?;
            if !refused.is_empty() {
                return Err(format!(
                    "{} holds names Neutron will not write locally, so download stopped",
                    path::file_name(remote_path)
                ));
            }
            let mut got = 0;
            for child in children {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(format!("Stopped while downloading {}", path::file_name(remote_path)));
                }
                got += download_one(
                    t,
                    &join(remote_path, &child.name)?,
                    child.kind,
                    &local_target.join(&child.name),
                    progress,
                )?;
            }
            Ok(got)
        }
        EntryKind::File => {
            let name = path::file_name(remote_path);
            let mut file = std::fs::File::create(local_target)
                .map_err(|e| format!("{name}: {e}"))?;
            let start = progress.bytes.load(Ordering::Relaxed);
            let outcome = t.download(remote_path, &mut file, &mut |done| {
                progress.bytes.store(start + done, Ordering::Relaxed);
                if progress.cancel.load(Ordering::Relaxed) { Continue::Stop } else { Continue::Yes }
            });
            match outcome {
                Ok(_) if progress.cancel.load(Ordering::Relaxed) => {
                    let _ = std::fs::remove_file(local_target);
                    Err(format!("Stopped while fetching {name}; partial copy removed"))
                }
                Ok(_) => Ok(1),
                Err(e) => {
                    let _ = std::fs::remove_file(local_target);
                    Err(format!("{name}: {e}"))
                }
            }
        }
    }
}

/// Transfers files and folders directly between two saved servers via pooled
/// transports (using a short-lived local temp file per item).
///
/// **Worker thread only.**
#[allow(clippy::too_many_arguments)]
pub fn transfer_between_servers(
    src_site: &Site,
    src_dir: &str,
    entries: &[(String, EntryKind)],
    dst_site: &Site,
    dst_dir: &str,
    dst_taken: &[String],
    cut: bool,
    progress: &Progress,
) -> Result<String, String> {
    let mut src = crate::remote::connect_checked(src_site)?;
    let mut dst = crate::remote::connect_checked(dst_site)?;
    transfer_between_on(
        src.as_mut(),
        src_dir,
        entries,
        dst.as_mut(),
        dst_dir,
        dst_taken,
        cut,
        progress,
    )
}

pub fn transfer_between_servers_sync(
    src_site: &Site,
    src_dir: &str,
    entries: &[(String, EntryKind)],
    dst_site: &Site,
    dst_dir: &str,
    progress: &Progress,
) -> Result<String, String> {
    let mut src = crate::remote::connect_checked(src_site)?;
    let mut dst = crate::remote::connect_checked(dst_site)?;
    transfer_between_on(
        src.as_mut(),
        src_dir,
        entries,
        dst.as_mut(),
        dst_dir,
        &[],
        false,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn transfer_between_on(
    src: &mut dyn Transport,
    src_dir: &str,
    entries: &[(String, EntryKind)],
    dst: &mut dyn Transport,
    dst_dir: &str,
    dst_taken: &[String],
    cut: bool,
    progress: &Progress,
) -> Result<String, String> {
    let taken_refs: Vec<&str> = dst_taken.iter().map(String::as_str).collect();
    let clashes = collisions(entries.iter().map(|(n, _)| n.as_str()), &taken_refs);
    if !clashes.is_empty() {
        return Err(format!(
            "{} already on the destination server: {}",
            if clashes.len() == 1 { "this name is" } else { "these names are" },
            clashes.join(", ")
        ));
    }
    for (name, _) in entries {
        path::safe_name(name).map_err(|e| format!("{name}: {e}"))?;
    }

    let relay_dir = std::env::temp_dir().join("neutron-relay");
    std::fs::create_dir_all(&relay_dir).map_err(|e| e.to_string())?;

    progress.items_total.store(entries.len() as u64, Ordering::Relaxed);
    let mut files = 0usize;
    for (idx, (name, kind)) in entries.iter().enumerate() {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(format!("Stopped after transferring {}", count(files, "file")));
        }
        let s_path = join(src_dir, name)?;
        let d_path = join(dst_dir, name)?;
        files += relay_one(src, &s_path, *kind, dst, &d_path, &relay_dir, idx, progress)?;
        progress.items_done.store((idx + 1) as u64, Ordering::Relaxed);
    }

    if cut {
        for (name, kind) in entries {
            let s_path = join(src_dir, name)?;
            delete(src, &s_path, *kind, progress)?;
        }
    }

    let verb = if cut { "Moved" } else { "Copied" };
    Ok(format!("{verb} {} between servers", count(files, "file")))
}

#[allow(clippy::too_many_arguments)]
fn relay_one(
    src: &mut dyn Transport,
    s_path: &str,
    kind: EntryKind,
    dst: &mut dyn Transport,
    d_path: &str,
    relay_dir: &Path,
    seq: usize,
    progress: &Progress,
) -> Result<usize, String> {
    match kind {
        EntryKind::Symlink | EntryKind::Junction | EntryKind::Drive | EntryKind::Virtual => Ok(0),
        EntryKind::Directory => {
            dst.create_dir(d_path).map_err(|e| format!("{}: {e}", path::file_name(d_path)))?;
            let mut refused = Vec::new();
            let children = src.list(s_path, &mut refused).map_err(|e| e.to_string())?;
            if !refused.is_empty() {
                return Err(format!(
                    "{} holds unsafe names, so transfer stopped",
                    path::file_name(s_path)
                ));
            }
            let mut sent = 0;
            for (c_idx, child) in children.into_iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err(format!("Stopped while transferring {}", path::file_name(s_path)));
                }
                sent += relay_one(
                    src,
                    &join(s_path, &child.name)?,
                    child.kind,
                    dst,
                    &join(d_path, &child.name)?,
                    relay_dir,
                    seq.wrapping_mul(31).wrapping_add(c_idx + 1),
                    progress,
                )?;
            }
            Ok(sent)
        }
        EntryKind::File => {
            let name = path::file_name(s_path);
            let tmp = relay_dir.join(format!("relay-{}-{seq}.part", std::process::id()));
            let outcome = (|| -> Result<(), String> {
                {
                    let mut out = std::fs::File::create(&tmp).map_err(|e| format!("{name}: {e}"))?;
                    let base = progress.bytes.load(Ordering::Relaxed);
                    src.download(s_path, &mut out, &mut |done| {
                        progress.bytes.store(base + done, Ordering::Relaxed);
                        if progress.cancel.load(Ordering::Relaxed) { Continue::Stop } else { Continue::Yes }
                    })
                    .map_err(|e| format!("{name}: {e}"))?;
                }
                let mut inp = std::fs::File::open(&tmp).map_err(|e| format!("{name}: {e}"))?;
                let base = progress.bytes.load(Ordering::Relaxed);
                dst.upload(d_path, &mut inp, &mut |done| {
                    progress.bytes.store(base + done, Ordering::Relaxed);
                    if progress.cancel.load(Ordering::Relaxed) { Continue::Stop } else { Continue::Yes }
                })
                .map_err(|e| format!("{name}: {e}"))?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&tmp);
            if let Err(err) = outcome {
                let _ = dst.remove_file(d_path);
                return Err(err);
            }
            Ok(1)
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
        fn download(&mut self, path: &str, sink: &mut dyn Write, on_progress: &mut dyn FnMut(u64) -> Continue) -> Result<u64, RemoteError> {
            let (_, bytes) = self.entries.get(path).ok_or_else(|| RemoteError::Server {
                path: path.to_owned(),
                reason: "not found".into(),
            })?.clone();
            sink.write_all(&bytes).map_err(RemoteError::Io)?;
            let _ = on_progress(bytes.len() as u64);
            Ok(bytes.len() as u64)
        }
        fn create_dir(&mut self, path: &str) -> Result<(), RemoteError> {
            self.entries.insert(path.to_owned(), (EntryKind::Directory, Vec::new()));
            Ok(())
        }
        fn rename(&mut self, from: &str, to: &str) -> Result<(), RemoteError> {
            let val = self.entries.remove(from).ok_or_else(|| RemoteError::Server {
                path: from.to_owned(),
                reason: "not found".into(),
            })?;
            self.entries.insert(to.to_owned(), val);
            Ok(())
        }
        fn copy_remote(&mut self, from: &str, to: &str) -> Result<(), RemoteError> {
            let val = self.entries.get(from).cloned().ok_or_else(|| RemoteError::Server {
                path: from.to_owned(),
                reason: "not found".into(),
            })?;
            self.entries.insert(to.to_owned(), val);
            Ok(())
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

    #[test]
    fn creating_a_file_uploads_its_bytes() {
        let mut fake = Fake::default();
        let receipt = run_on(
            &mut fake,
            "/pub",
            &Op::CreateFile {
                name: "notes.txt".into(),
                contents: b"hello world".to_vec(),
            },
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(receipt, "Created notes.txt");
        assert_eq!(fake.entries["/pub/notes.txt"], (EntryKind::File, b"hello world".to_vec()));
    }

    #[test]
    fn move_and_copy_remote_act_without_downloading() {
        let mut fake = Fake::default();
        fake.entries.insert("/a/note.txt".into(), (EntryKind::File, b"abc".to_vec()));
        run_on(
            &mut fake,
            "/b",
            &Op::CopyRemote {
                from_dir: "/a".into(),
                entries: vec![("note.txt".into(), EntryKind::File)],
            },
            &Progress::default(),
        )
        .unwrap();
        assert!(fake.entries.contains_key("/a/note.txt"));
        assert_eq!(fake.entries["/b/note.txt"].1, b"abc");

        run_on(
            &mut fake,
            "/c",
            &Op::MoveRemote {
                from_dir: "/a".into(),
                entries: vec![("note.txt".into(), EntryKind::File)],
            },
            &Progress::default(),
        )
        .unwrap();
        assert!(!fake.entries.contains_key("/a/note.txt"));
        assert!(fake.entries.contains_key("/c/note.txt"));
    }

    #[test]
    fn download_entries_fetches_folders_checks_collisions_and_removes_on_cut() {
        let dest = std::env::temp_dir().join(format!("neutron-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let mut fake = Fake::with(&[("/srv/pack", EntryKind::Directory)]);
        fake.entries.insert("/srv/pack/one.txt".into(), (EntryKind::File, b"one".to_vec()));
        fake.entries.insert("/srv/top.txt".into(), (EntryKind::File, b"top".to_vec()));

        let receipt = download_entries_on(
            &mut fake,
            "/srv",
            &[
                ("pack".into(), EntryKind::Directory),
                ("top.txt".into(), EntryKind::File),
            ],
            &dest,
            true,
            &Progress::default(),
        )
        .unwrap();
        assert!(receipt.contains("Moved 2 files"));
        assert_eq!(std::fs::read(dest.join("pack/one.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("top.txt")).unwrap(), b"top");
        assert!(fake.entries.is_empty(), "cut should delete downloaded sources");

        fake.entries.insert("/srv/top.txt".into(), (EntryKind::File, b"new".to_vec()));
        assert!(download_entries_on(
            &mut fake,
            "/srv",
            &[("top.txt".into(), EntryKind::File)],
            &dest,
            false,
            &Progress::default(),
        ).is_err());

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn transfer_between_servers_streams_files_and_folders() {
        let mut src = Fake::with(&[("/src/folder", EntryKind::Directory)]);
        src.entries.insert("/src/folder/model.bin".into(), (EntryKind::File, vec![7u8; 64]));
        let mut dst = Fake::default();

        let receipt = transfer_between_on(
            &mut src,
            "/src",
            &[("folder".into(), EntryKind::Directory)],
            &mut dst,
            "/dst",
            &[],
            false,
            &Progress::default(),
        )
        .unwrap();
        assert!(receipt.contains("Copied 1 file between servers"));
        assert_eq!(dst.entries["/dst/folder/model.bin"].1, vec![7u8; 64]);
        assert!(src.entries.contains_key("/src/folder/model.bin"));
    }
}
