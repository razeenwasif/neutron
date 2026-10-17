//! The formats read through the system's libarchive — 7-Zip, RAR, and tar
//! compressed with xz or bzip2 — and plain gzip, which is one file squeezed.
//!
//! libarchive is reached through `bsdtar` (`/usr/bin/tar`), which every Mac
//! has: it reads RAR, which no maintained Rust crate does, and it refuses
//! absolute paths and `..` in names unless told otherwise, the same entries
//! `safe_join` refuses for the formats read here.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{ArchiveError, Continue, Progress, Summary};

const BSDTAR: &str = "/usr/bin/tar";

/// Every entry: its path, whether it is a folder, and its size.
pub fn list(archive: &Path) -> Result<Vec<(String, bool, u64)>, ArchiveError> {
    let out = Command::new(BSDTAR)
        .arg("-tvf")
        .arg(archive)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(ArchiveError::Unreadable(if why.is_empty() { "the archive could not be read".into() } else { why }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_line).collect())
}

/// One line of `bsdtar -tv`: mode, links, owner, group, size, month, day,
/// time or year, then the name — which may have spaces in it.
fn parse_line(line: &str) -> Option<(String, bool, u64)> {
    let mut rest = line;
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let mut name = rest.strip_prefix(' ').unwrap_or(rest).to_owned();
    let mode = fields[0];
    if mode.starts_with('l') {
        if let Some(at) = name.find(" -> ") {
            name.truncate(at);
        }
    }
    if name.is_empty() {
        return None;
    }
    let size = fields[4].parse().unwrap_or(0);
    Some((name, mode.starts_with('d'), size))
}

/// Extracts everything, or only the entries `selected`, each under its own
/// name in `destination`.
pub fn extract(
    archive: &Path,
    destination: &Path,
    selected: Option<&[String]>,
    mut on_progress: impl FnMut(Progress) -> Continue,
) -> Result<Summary, ArchiveError> {
    fs::create_dir_all(destination)?;
    let entries = list(archive)?;
    let mut summary = Summary::default();
    let total: u64 = entries.iter().map(|e| e.2).sum();
    if on_progress(Progress { done: 0, total }) == Continue::Stop {
        summary.cancelled = true;
        return Ok(summary);
    }
    let Some(selected) = selected else {
        run(archive, destination, &[])?;
        summary.files = entries.iter().filter(|e| !e.1).count() as u64;
        summary.bytes = total;
        on_progress(Progress { done: total, total });
        return Ok(summary);
    };
    // Out into a scratch folder with their paths, then each moved up to sit
    // in `destination` under its own name.
    let scratch = destination.join(format!(".neutron-extract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let result = run(archive, &scratch, selected);
    for wanted in selected {
        let wanted = wanted.trim_matches('/');
        let base = wanted.rsplit('/').next().unwrap_or(wanted);
        let from = scratch.join(wanted);
        let to = destination.join(base);
        if from.symlink_metadata().is_err() {
            summary.refused.push(format!("{wanted}: not in the archive"));
            continue;
        }
        if to.symlink_metadata().is_ok() {
            summary.refused.push(format!("{base}: already there"));
            continue;
        }
        match fs::rename(&from, &to) {
            Ok(()) => summary.files += 1,
            Err(e) => summary.refused.push(format!("{base}: {e}")),
        }
    }
    let _ = fs::remove_dir_all(&scratch);
    result?;
    summary.bytes = entries.iter().filter(|e| selected.iter().any(|s| e.0.trim_end_matches('/').starts_with(s.trim_matches('/')))).map(|e| e.2).sum();
    on_progress(Progress { done: total, total });
    Ok(summary)
}

fn run(archive: &Path, into: &Path, only: &[String]) -> Result<(), ArchiveError> {
    let out = Command::new(BSDTAR)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .args(only.iter().map(|s| s.trim_end_matches('/')))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(ArchiveError::Unreadable(String::from_utf8_lossy(&out.stderr).trim().to_owned()))
    }
}

/// The name a gzipped file has once unpacked: its own, less `.gz`.
pub fn gunzipped_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.len().checked_sub(3) {
        Some(cut) if name[cut..].eq_ignore_ascii_case(".gz") && cut > 0 => name[..cut].to_owned(),
        _ => format!("{name}.out"),
    }
}

/// A gzipped file's unpacked size, from the four bytes it ends with: exact
/// below four gigabytes, which is where gzip stops counting.
pub fn gunzipped_size(archive: &Path) -> u64 {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = fs::File::open(archive) else { return 0 };
    let mut tail = [0u8; 4];
    if file.seek(SeekFrom::End(-4)).is_err() || file.read_exact(&mut tail).is_err() {
        return 0;
    }
    u32::from_le_bytes(tail) as u64
}

/// Unpacks a gzipped file into `destination`.
pub fn gunzip(archive: &Path, destination: &Path) -> Result<(PathBuf, Summary), ArchiveError> {
    fs::create_dir_all(destination)?;
    let target = destination.join(gunzipped_name(archive));
    if target.symlink_metadata().is_ok() {
        return Err(ArchiveError::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} is already there", target.display()))));
    }
    let mut from = flate2::read::GzDecoder::new(fs::File::open(archive)?);
    let mut to = fs::File::create(&target)?;
    match io::copy(&mut from, &mut to) {
        Ok(bytes) => Ok((target, Summary { files: 1, bytes, ..Default::default() })),
        Err(e) => {
            drop(to);
            let _ = fs::remove_file(&target);
            Err(ArchiveError::Unreadable(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_lines_are_read_whatever_the_name_holds() {
        assert_eq!(
            parse_line("-rw-r--r--  0 me     staff       5 Oct 17 18:00 docs/a b.txt"),
            Some(("docs/a b.txt".into(), false, 5))
        );
        assert_eq!(parse_line("drwxr-xr-x  0 me     staff       0 Oct 17  2024 docs/"), Some(("docs/".into(), true, 0)));
        assert_eq!(parse_line("lrwxr-xr-x  0 me     staff       0 Oct 17 18:00 ln -> a b"), Some(("ln".into(), false, 0)));
        assert_eq!(parse_line("garbage"), None);
    }

    #[test]
    fn a_7z_made_by_bsdtar_is_listed_and_extracted() {
        let dir = std::env::temp_dir().join(format!("neutron-7z-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/docs")).unwrap();
        fs::write(dir.join("src/docs/a.txt"), "hello").unwrap();
        fs::write(dir.join("src/b.txt"), "x").unwrap();
        let archive = dir.join("pack.7z");
        let made = Command::new(BSDTAR)
            .args(["--format", "7zip", "-cf"])
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("src"))
            .args(["docs", "b.txt"])
            .status()
            .unwrap();
        assert!(made.success());

        let names: Vec<String> = list(&archive).unwrap().into_iter().map(|e| e.0).collect();
        assert!(names.iter().any(|n| n.trim_end_matches('/') == "docs/a.txt"), "{names:?}");

        let all = extract(&archive, &dir.join("all"), None, |_| Continue::Yes).unwrap();
        assert_eq!(fs::read_to_string(dir.join("all/docs/a.txt")).unwrap(), "hello");
        assert_eq!(all.files, 2);

        let some = extract(&archive, &dir.join("some"), Some(&["docs/a.txt".to_owned()]), |_| Continue::Yes).unwrap();
        assert!(some.refused.is_empty(), "{:?}", some.refused);
        assert_eq!(fs::read_to_string(dir.join("some/a.txt")).unwrap(), "hello", "under its own name");
        assert!(!dir.join("some/docs").exists(), "and nothing else");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gzipped_file_unpacks_beside_under_its_own_name() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("neutron-gz-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("notes.txt.gz");
        let mut gz = flate2::write::GzEncoder::new(fs::File::create(&archive).unwrap(), flate2::Compression::default());
        gz.write_all(b"squeezed").unwrap();
        gz.finish().unwrap();
        assert_eq!(gunzipped_name(&archive), "notes.txt");
        assert_eq!(gunzipped_size(&archive), 8);
        let (target, summary) = gunzip(&archive, &dir.join("out")).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "squeezed");
        assert_eq!(summary.files, 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
