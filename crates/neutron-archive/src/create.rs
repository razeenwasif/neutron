//! Building a zip.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::{ArchiveError, Continue, Progress, Summary};

/// Writes `sources` into a new zip at `output`.
///
/// Names inside the archive are relative to `base`, so zipping a folder from
/// its parent produces `folder/file` rather than the absolute path it came
/// from — which is both what every other archiver does and what stops the
/// archive leaking the directory layout of the machine that made it.
///
/// Directories are walked; the walk does not follow reparse points, so a
/// junction pointing at `C:\` cannot turn a small folder into the whole disk.
///
/// On failure the partly written archive is removed. A zip is only readable
/// once its central directory is written at the very end, so whatever was on
/// disk at the moment of failure is a file with the right name and size that
/// no tool can open — the worst kind of leftover, because it looks like the
/// thing that was asked for.
pub fn zip(
    sources: &[PathBuf],
    base: &Path,
    output: &Path,
    on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    let file = fs::File::create(output)?;
    let written = write_zip(file, sources, base, on_progress);
    if written.is_err() {
        let _ = fs::remove_file(output);
    }
    written
}

/// Whether a member of `len` bytes needs the ZIP64 extension.
///
/// The classic zip header holds sizes in 32 bits, and the `zip` crate does not
/// switch to ZIP64 by itself: a member that crosses 4 GiB without
/// `large_file` set fails the write partway through. So it is set from the
/// size on disk — with headroom, since the *compressed* size is what has to
/// fit too, and deflate makes incompressible data very slightly larger. Not
/// set for everything, because ZIP64 members cost a few bytes each and, more
/// to the point, some old extractors refuse them outright.
fn needs_zip64(len: u64) -> bool {
    const HEADROOM: u64 = 64 * 1024 * 1024;
    len >= u64::from(u32::MAX) - HEADROOM
}

fn write_zip(
    file: fs::File,
    sources: &[PathBuf],
    base: &Path,
    mut on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    let mut writer = zip::ZipWriter::new(io::BufWriter::new(file));
    let base_options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut summary = Summary::default();
    let mut queue: Vec<PathBuf> = sources.to_vec();

    while let Some(path) = queue.pop() {
        let Ok(relative) = path.strip_prefix(base) else {
            summary
                .refused
                .push(format!("{}: is not inside the folder being zipped", path.display()));
            continue;
        };
        // Zip names use forward slashes whatever the platform wrote them.
        let name = relative.to_string_lossy().replace('\\', "/");
        if name.is_empty() {
            continue;
        }

        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                summary.refused.push(format!("{name}: {e}"));
                continue;
            }
        };

        // Not `metadata`: that follows links, and following one is how a
        // junction to C:\ ends up inside the archive.
        if meta.file_type().is_symlink() {
            summary
                .refused
                .push(format!("{name}: links are not followed"));
            continue;
        }

        // Carried through, so a file does not come out of the archive dated
        // 1980 — which is what the format's zero value means and what an
        // extractor then writes to disk.
        let options = with_time(base_options, &meta);

        if meta.is_dir() {
            writer
                .add_directory(format!("{name}/"), options)
                .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
            match fs::read_dir(&path) {
                Ok(entries) => queue.extend(entries.filter_map(|e| e.ok()).map(|e| e.path())),
                Err(e) => summary.refused.push(format!("{name}: {e}")),
            }
            continue;
        }

        writer
            .start_file(&name, options.large_file(needs_zip64(meta.len())))
            .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;

        let mut source = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                // The entry has been started, so it stays in the archive as an
                // empty member rather than being left half-written.
                summary.refused.push(format!("{name}: {e}"));
                continue;
            }
        };

        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
            summary.bytes += read as u64;

            if on_progress(Progress {
                done: summary.bytes,
                total: 0,
            }) == Continue::Stop
            {
                summary.cancelled = true;
                // Finished rather than abandoned: a truncated zip on disk looks
                // like a real one until someone tries to open it.
                writer
                    .finish()
                    .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
                return Ok(summary);
            }
        }
        summary.files += 1;
    }

    writer
        .finish()
        .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
    Ok(summary)
}

/// `options` carrying the file's modification time, where it has one that a
/// zip can represent.
///
/// A zip stores times as MS-DOS fields, which start in 1980 and end in 2107.
/// Anything outside that — a corrupt timestamp, a file dated 1970 by a build
/// system — keeps the crate's default rather than being clamped to a date that
/// is equally wrong but looks deliberate.
fn with_time<'a>(
    options: zip::write::FileOptions<'a, ()>,
    meta: &fs::Metadata,
) -> zip::write::FileOptions<'a, ()> {
    let Some(civil) = meta.modified().ok().and_then(crate::clock::civil_from) else {
        return options;
    };
    match zip::DateTime::from_date_and_time(
        civil.year,
        civil.month,
        civil.day,
        civil.hour,
        civil.minute,
        civil.second,
    ) {
        Ok(stamp) => options.last_modified_time(stamp),
        Err(_) => options,
    }
}

/// A name for a zip made from `sources`.
///
/// One item gives its own name; several give the folder's, which is what
/// Explorer does and what makes the result identifiable later.
pub fn suggested_name(sources: &[PathBuf], base: &Path) -> String {
    let stem = match sources {
        [only] => only
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Archive".to_owned()),
        _ => base
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Archive".to_owned()),
    };
    format!("{stem}.zip")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_near_or_over_four_gigabytes_is_written_as_zip64() {
        assert!(!needs_zip64(0));
        assert!(!needs_zip64(1024 * 1024 * 1024));
        assert!(needs_zip64(u64::from(u32::MAX)));
        assert!(needs_zip64(5 * 1024 * 1024 * 1024));
        // Inside the headroom: small enough to fit uncompressed, close enough
        // that deflate's overhead on incompressible data could push it over.
        assert!(needs_zip64(u64::from(u32::MAX) - 1024));
    }

    #[test]
    fn a_failed_zip_leaves_no_partial_archive_behind() {
        // The same file named twice is a duplicate member, which the zip
        // writer refuses partway through — after the first copy has already
        // been written to disk. Any mid-write failure takes the same path;
        // this is simply the one a test can cause on demand.
        let dir = std::env::temp_dir()
            .join(format!("neutron-archive-create-{}-partial", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.txt");
        fs::write(&source, vec![b'a'; 100_000]).unwrap();
        let output = dir.join("out.zip");

        let outcome = zip(&[source.clone(), source], &dir, &output, |_| Continue::Yes);

        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!output.exists(), "the partial archive was left behind");
    }

    #[test]
    fn one_file_names_the_archive_after_it_without_its_extension() {
        // `notes.txt` gives `notes.zip`, not `notes.txt.zip` — which is what
        // Explorer's own "Compressed (zipped) folder" produces.
        let sources = vec![PathBuf::from("/d/notes.txt")];
        assert_eq!(suggested_name(&sources, Path::new("/d")), "notes.zip");
    }

    #[test]
    fn several_files_name_it_after_the_folder() {
        let sources = vec![PathBuf::from("/d/a.txt"), PathBuf::from("/d/b.txt")];
        assert_eq!(suggested_name(&sources, Path::new("/d/project")), "project.zip");
    }

    #[test]
    fn a_folder_with_no_name_still_produces_an_archive_name() {
        let sources = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert_eq!(suggested_name(&sources, Path::new("/")), "Archive.zip");
    }
}
