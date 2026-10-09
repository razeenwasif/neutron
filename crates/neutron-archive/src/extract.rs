//! Unpacking an archive.
//!
//! # One bad entry costs one file
//!
//! An entry that cannot be written — a file `a` followed by `a/b`, a name the
//! filesystem will not take, a member whose data is corrupt — is recorded in
//! [`Summary::refused`] and the extraction moves on. Aborting on the first one
//! would leave the user with an arbitrary prefix of the archive and no way to
//! get the rest, which is worse than every other file plus a list of the ones
//! that did not make it. The exception is the disk filling up: every entry
//! after that would fail the same way, so that still ends the extraction.
//!
//! # Provenance is passed on
//!
//! A downloaded archive carries a mark saying it came from the internet —
//! `com.apple.quarantine` on macOS, the `Zone.Identifier` stream on Windows —
//! and that mark is what makes Gatekeeper and SmartScreen look at a program
//! before it first runs. Extracting without copying it launders a downloaded
//! executable into one the OS treats as locally made, so the mark is copied
//! onto everything written here. Best effort throughout: a filesystem that
//! cannot hold the mark is not a reason to fail an extraction.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::path::{Refusal, safe_join};
use crate::{ArchiveError, Continue, Format, Progress, Summary};

/// Extracts `archive` into `destination`, which is created if needed.
///
/// `on_progress` is called as entries are written and can stop the extraction;
/// what has already been written stays, because deleting a partial extraction
/// is a destructive act the caller did not ask for.
pub fn extract(
    archive: &Path,
    destination: &Path,
    format: Format,
    on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    let existed = destination.exists();
    fs::create_dir_all(destination)?;

    let mut out = Output {
        destination,
        provenance: mark::read(archive),
        marked: HashSet::new(),
    };
    // The destination is marked only if this made it. Extracting into a
    // folder that was already there must not quarantine the folder itself.
    if !existed {
        out.stamp(destination, true);
    }

    match format {
        Format::Zip => zip(archive, &mut out, on_progress),
        Format::Tar => tarball(tar::Archive::new(fs::File::open(archive)?), &mut out, on_progress),
        Format::TarGz => tarball(
            tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?)),
            &mut out,
            on_progress,
        ),
    }
}

/// Where entries are written, and what is passed on to each.
struct Output<'a> {
    destination: &'a Path,
    /// The archive's own provenance mark, if it had one.
    provenance: Option<Vec<u8>>,
    /// Directories already marked, so a folder of ten thousand files is
    /// marked once rather than ten thousand times.
    marked: HashSet<PathBuf>,
}

impl Output<'_> {
    /// Creates `dir` and any missing parents, marking each.
    fn directory(&mut self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        self.mark_tree(dir);
        Ok(())
    }

    /// Marks `dir` and every directory between it and the destination.
    ///
    /// Covers the folders an archive never lists but implies — a zip holding
    /// only `a/b/c.txt` still produces `a` and `a/b`, and they came from the
    /// same download as the file inside them.
    fn mark_tree(&mut self, dir: &Path) {
        if self.provenance.is_none() {
            return;
        }
        let mut current = Some(dir);
        while let Some(d) = current {
            if d == self.destination || !d.starts_with(self.destination) {
                break;
            }
            // Already marked means its ancestors are too: every chain is
            // walked all the way up the first time.
            if !self.marked.insert(d.to_path_buf()) {
                break;
            }
            self.stamp(d, true);
            current = d.parent();
        }
    }

    fn stamp(&self, path: &Path, is_dir: bool) {
        if let Some(value) = &self.provenance {
            mark::write(path, value, is_dir);
        }
    }

    /// Writes one file entry. `Ok(Stop)` when the caller asked to stop.
    ///
    /// On failure the partial file is removed: a file cut off partway looks
    /// like a whole one until someone opens it, and the entry is reported as
    /// refused anyway.
    fn file(
        &mut self,
        target: &Path,
        from: &mut impl Read,
        modified: Option<SystemTime>,
        summary: &mut Summary,
        total: u64,
        on_progress: &mut impl FnMut(Progress) -> Continue,
    ) -> io::Result<Continue> {
        if let Some(parent) = target.parent() {
            self.directory(parent)?;
        }
        let mut out = fs::File::create(target)?;
        // Copied in chunks rather than with `io::copy` so progress moves during
        // a single large member — a 4 GB disk image inside a zip would
        // otherwise sit at the same percentage for minutes.
        let copied = copy_watched(from, &mut out, summary, total, on_progress);
        match copied {
            Ok(Continue::Stop) => return Ok(Continue::Stop),
            Ok(Continue::Yes) => {}
            Err(e) => {
                drop(out);
                let _ = fs::remove_file(target);
                return Err(e);
            }
        }
        // Before the time is restored: on Windows the mark is a second data
        // stream, and writing it counts as modifying the file.
        self.stamp(target, false);
        // Restored after writing, since writing sets it. Best effort: a
        // filesystem that will not accept the time is not a reason to fail an
        // extraction that otherwise worked.
        if let Some(time) = modified {
            let _ = out.set_modified(time);
        }
        summary.files += 1;
        Ok(Continue::Yes)
    }
}

/// Records a write failure against the entry, or ends the extraction if no
/// later entry could succeed either.
fn record(summary: &mut Summary, name: &str, error: io::Error) -> Result<(), ArchiveError> {
    if matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) {
        return Err(ArchiveError::Io(error));
    }
    summary.refused.push(format!("{name}: could not be written: {error}"));
    Ok(())
}

fn zip(
    archive: &Path,
    out: &mut Output<'_>,
    mut on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;

    // Summed up front so progress is a percentage rather than a spinner. Read
    // from the central directory, which is metadata — no member is decompressed
    // to get it.
    let mut total = 0u64;
    for i in 0..zip.len() {
        if let Ok(entry) = zip.by_index_raw(i) {
            total += entry.size();
        }
    }

    let mut summary = Summary::default();

    for i in 0..zip.len() {
        let mut entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                // One unreadable entry does not condemn the rest: a zip with a
                // single member in an unsupported compression method should
                // still yield everything else.
                summary.refused.push(format!("entry {i}: {e}"));
                continue;
            }
        };

        // `mangled_name` is the zip crate's own sanitiser. It is not trusted in
        // place of ours: it silently rewrites hostile names into something
        // plausible, where the whole point here is to notice and report them.
        let name = entry.name().to_owned();
        let target = match safe_join(out.destination, &name) {
            Ok(path) => path,
            Err(reason) => {
                summary.refused.push(describe(&name, reason));
                continue;
            }
        };

        if entry.is_dir() {
            if let Err(e) = out.directory(&target) {
                record(&mut summary, &name, e)?;
            }
            continue;
        }

        let modified = entry.last_modified().and_then(|d| {
            crate::clock::unix_from_civil(crate::clock::Civil {
                year: d.year(),
                month: d.month(),
                day: d.day(),
                hour: d.hour(),
                minute: d.minute(),
                second: d.second(),
            })
        });

        match out.file(&target, &mut entry, modified, &mut summary, total, &mut on_progress) {
            Ok(Continue::Yes) => {}
            Ok(Continue::Stop) => {
                summary.cancelled = true;
                return Ok(summary);
            }
            Err(e) => record(&mut summary, &name, e)?,
        }
    }

    Ok(summary)
}

fn tarball<R: Read>(
    mut archive: tar::Archive<R>,
    out: &mut Output<'_>,
    mut on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    let mut summary = Summary::default();

    // A tar is a stream: its size is not known without reading it once, and
    // reading a gzipped one twice to show a percentage is not worth it. The
    // caller is told a total of zero and shows a count instead.
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;

    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                summary.refused.push(e.to_string());
                continue;
            }
        };

        // PAX headers are metadata about other entries, not entries of their
        // own. The `tar` crate folds the per-file kind (`x`) into the entry it
        // describes, but hands the global one (`g`) through — and every
        // `git archive` tarball starts with one, named `pax_global_header`.
        // Reporting it as a skipped file would put a warning on the most
        // ordinary tarball there is.
        if matches!(
            entry.header().entry_type(),
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }

        let name = entry.path().map(|p| p.to_string_lossy().into_owned());
        let Ok(name) = name else {
            summary.refused.push("an entry with an unreadable name".to_owned());
            continue;
        };

        let target = match safe_join(out.destination, &name) {
            Ok(path) => path,
            Err(reason) => {
                summary.refused.push(describe(&name, reason));
                continue;
            }
        };

        // Only ordinary files and directories are written. A tar can also carry
        // symlinks, hard links, devices and fifos; a link is another way to
        // point outside the destination, and the rest have no meaning here.
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if let Err(e) = out.directory(&target) {
                    record(&mut summary, &name, e)?;
                }
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            other => {
                summary
                    .refused
                    .push(format!("{name}: {other:?} entries are not extracted"));
                continue;
            }
        }

        // A tar stores seconds since the epoch directly, with none of the zip
        // format's range limits.
        let modified = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));

        match out.file(&target, &mut entry, modified, &mut summary, 0, &mut on_progress) {
            Ok(Continue::Yes) => {}
            Ok(Continue::Stop) => {
                summary.cancelled = true;
                return Ok(summary);
            }
            Err(e) => record(&mut summary, &name, e)?,
        }
    }

    Ok(summary)
}

/// Copies one entry, reporting progress and honouring a stop.
fn copy_watched(
    from: &mut impl Read,
    to: &mut fs::File,
    summary: &mut Summary,
    total: u64,
    on_progress: &mut impl FnMut(Progress) -> Continue,
) -> io::Result<Continue> {
    use std::io::Write;

    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = from.read(&mut buffer)?;
        if read == 0 {
            return Ok(Continue::Yes);
        }
        to.write_all(&buffer[..read])?;
        summary.bytes += read as u64;

        if on_progress(Progress {
            done: summary.bytes,
            total,
        }) == Continue::Stop
        {
            return Ok(Continue::Stop);
        }
    }
}

fn describe(name: &str, reason: Refusal) -> String {
    let why = match reason {
        Refusal::Empty => "has no usable name",
        Refusal::Absolute => "is an absolute path",
        Refusal::Traversal => "points outside the destination",
        Refusal::ReservedName => "uses a name Windows reserves",
        Refusal::Stream => "names an alternate data stream",
    };
    format!("{name}: {why}")
}

/// Reading and writing the provenance mark. See the module docs.
#[cfg(target_os = "macos")]
mod mark {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const QUARANTINE: &CStr = c"com.apple.quarantine";

    fn c_path(path: &Path) -> Option<CString> {
        CString::new(path.as_os_str().as_bytes()).ok()
    }

    /// The archive's quarantine value, verbatim, if it has one.
    pub(super) fn read(path: &Path) -> Option<Vec<u8>> {
        let path = c_path(path)?;
        // The value is a short `flags;timestamp;agent;uuid` string — well under
        // a hundred bytes in practice. Anything that does not fit this is not
        // a quarantine value worth copying.
        let mut buffer = vec![0u8; 4096];
        // SAFETY: both strings are NUL-terminated and outlive the call, and
        // the buffer pointer and length describe memory this owns.
        let got = unsafe {
            libc::getxattr(
                path.as_ptr(),
                QUARANTINE.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
                0,
            )
        };
        if got <= 0 {
            return None;
        }
        buffer.truncate(got as usize);
        Some(buffer)
    }

    /// Copies `value` onto `path`. Directories are marked too: Gatekeeper
    /// checks an app bundle, which is a directory, by its own attribute.
    pub(super) fn write(path: &Path, value: &[u8], _is_dir: bool) {
        let Some(c) = c_path(path) else { return };
        // SAFETY: as in `read`. `XATTR_NOFOLLOW` so a link planted in the
        // destination is not followed to mark something else.
        let rc = unsafe {
            libc::setxattr(
                c.as_ptr(),
                QUARANTINE.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if rc != 0 {
            tracing::debug!(
                path = %path.display(),
                "could not copy the quarantine flag: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Reading and writing the provenance mark. See the module docs.
#[cfg(windows)]
mod mark {
    use std::path::{Path, PathBuf};

    /// `path:Zone.Identifier` — the alternate data stream the mark lives in.
    fn stream(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(":Zone.Identifier");
        PathBuf::from(name)
    }

    /// The archive's zone information, verbatim, if it has any.
    pub(super) fn read(path: &Path) -> Option<Vec<u8>> {
        // A few lines of INI text in practice. Anything far larger is not a
        // zone identifier worth copying onto every file.
        let data = std::fs::read(stream(path)).ok()?;
        (!data.is_empty() && data.len() <= 64 * 1024).then_some(data)
    }

    /// Copies `value` onto `path`. Files only: SmartScreen consults the mark
    /// on what is run or opened, and that is never a directory.
    pub(super) fn write(path: &Path, value: &[u8], is_dir: bool) {
        if is_dir {
            return;
        }
        if let Err(e) = std::fs::write(stream(path), value) {
            tracing::debug!(path = %path.display(), "could not copy the zone identifier: {e}");
        }
    }
}

/// Nowhere else has a provenance mark this knows how to carry.
#[cfg(not(any(target_os = "macos", windows)))]
mod mark {
    use std::path::Path;

    pub(super) fn read(_: &Path) -> Option<Vec<u8>> {
        None
    }

    pub(super) fn write(_: &Path, _: &[u8], _: bool) {}
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("neutron-archive-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_quarantine_flag_follows_the_archive_onto_everything_extracted() {
        use std::io::Write;

        let root = scratch("quarantine");
        let archive = root.join("download.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.add_directory("app/", options).unwrap();
            // `nested` is never listed: it is implied by the file inside it,
            // and must be marked all the same.
            zip.start_file("app/nested/run.sh", options).unwrap();
            zip.write_all(b"echo hi").unwrap();
            zip.finish().unwrap();
        }
        let value = b"0083;66f00000;Safari;00000000-0000-0000-0000-000000000000".to_vec();
        mark::write(&archive, &value, false);
        assert_eq!(mark::read(&archive).as_deref(), Some(&value[..]), "test setup");

        let destination = root.join("download");
        let summary = extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();
        assert!(summary.refused.is_empty(), "{:?}", summary.refused);

        for path in [
            destination.clone(),
            destination.join("app"),
            destination.join("app/nested"),
            destination.join("app/nested/run.sh"),
        ] {
            assert_eq!(
                mark::read(&path).as_deref(),
                Some(&value[..]),
                "{} was not marked",
                path.display()
            );
        }
    }

    #[test]
    fn an_archive_without_the_flag_adds_none() {
        use std::io::Write;

        let root = scratch("unquarantined");
        let archive = root.join("local.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("a.txt", options).unwrap();
            zip.write_all(b"a").unwrap();
            zip.finish().unwrap();
        }

        let destination = root.join("local");
        extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();
        assert_eq!(mark::read(&destination.join("a.txt")), None);
    }
}
