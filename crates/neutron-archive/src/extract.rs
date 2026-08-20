//! Unpacking an archive.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

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
    fs::create_dir_all(destination)?;

    match format {
        Format::Zip => zip(archive, destination, on_progress),
        Format::Tar => tarball(
            tar::Archive::new(fs::File::open(archive)?),
            destination,
            on_progress,
        ),
        Format::TarGz => tarball(
            tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?)),
            destination,
            on_progress,
        ),
    }
}

fn zip(
    archive: &Path,
    destination: &Path,
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
        let target = match safe_join(destination, &name) {
            Ok(path) => path,
            Err(reason) => {
                summary.refused.push(describe(&name, reason));
                continue;
            }
        };

        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
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

        let mut out = fs::File::create(&target)?;
        // Copied in chunks rather than with `io::copy` so progress moves during
        // a single large member — a 4 GB disk image inside a zip would
        // otherwise sit at the same percentage for minutes.
        if copy_watched(&mut entry, &mut out, &mut summary, total, &mut on_progress)? == Continue::Stop
        {
            summary.cancelled = true;
            return Ok(summary);
        }
        // Restored after writing, since writing sets it. Best effort: a
        // filesystem that will not accept the time is not a reason to fail an
        // extraction that otherwise worked.
        if let Some(time) = modified {
            let _ = out.set_modified(time);
        }
        summary.files += 1;
    }

    Ok(summary)
}

fn tarball<R: Read>(
    mut archive: tar::Archive<R>,
    destination: &Path,
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

        let name = entry.path().map(|p| p.to_string_lossy().into_owned());
        let Ok(name) = name else {
            summary.refused.push("an entry with an unreadable name".to_owned());
            continue;
        };

        let target = match safe_join(destination, &name) {
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
                fs::create_dir_all(&target)?;
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

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // A tar stores seconds since the epoch directly, with none of the zip
        // format's range limits.
        let modified = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));

        let mut out = fs::File::create(&target)?;
        if copy_watched(&mut entry, &mut out, &mut summary, 0, &mut on_progress)? == Continue::Stop {
            summary.cancelled = true;
            return Ok(summary);
        }
        if let Some(time) = modified {
            let _ = out.set_modified(time);
        }
        summary.files += 1;
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
    };
    format!("{name}: {why}")
}
