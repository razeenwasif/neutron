//! Reading and writing archives.
//!
//! # Why not just use the shell
//!
//! Windows can extract a zip, and any installed archiver puts its own entries
//! in the context menu Neutron already shows. What that cannot do is tell you
//! how far along it is inside Neutron, let you stop it, or work the same way on
//! a machine with nothing installed. This does.
//!
//! # What is supported
//!
//! Zip, tar, and gzipped tar for reading; zip for writing. Those cover
//! essentially everything that arrives by download or lands in a source tree.
//! `7z` and `rar` are deliberately absent: both need a real implementation or a
//! bundled binary, and the installed tools already appear in the context menu,
//! which is a better answer than a second-rate decoder.
//!
//! # Nothing in an archive is trusted
//!
//! Entry names are attacker-controlled strings. See [`path::safe_join`], which
//! is where that is dealt with, and which is the most important code here.

pub mod clock;
pub mod create;
pub mod extract;
pub mod path;

use std::path::Path;

/// An archive format this crate can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
}

impl Format {
    /// The suffix stripped to suggest a destination folder name.
    fn suffix(self) -> &'static [&'static str] {
        match self {
            Format::Zip => &[".zip"],
            Format::Tar => &[".tar"],
            Format::TarGz => &[".tar.gz", ".tgz"],
        }
    }
}

/// What kind of archive `path` is, judged by its name.
///
/// By extension rather than by reading the file: this answers "should the
/// Extract command be offered for this?", which is a question about what the
/// file claims to be. The reader checks the actual contents, and says so if
/// they disagree.
pub fn format_of(path: &Path) -> Option<Format> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

    // Longest first: `.tar.gz` also ends with `.gz`, and a plain `.gz` of
    // something that is not a tar is not something this extracts.
    [Format::TarGz, Format::Tar, Format::Zip]
        .into_iter()
        .find(|format| format.suffix().iter().any(|s| name.ends_with(s)))
}

/// The folder an archive should extract into, beside the archive itself.
///
/// `photos.zip` becomes `photos`, `src.tar.gz` becomes `src`. A name that
/// collides with something already there gets a number, the same shape as a new
/// folder — extracting the same download twice should not merge the two.
pub fn destination_for(archive: &Path, taken: impl Fn(&Path) -> bool) -> Option<std::path::PathBuf> {
    let parent = archive.parent()?;
    let name = archive.file_name()?.to_string_lossy().to_string();
    let lower = name.to_ascii_lowercase();

    let stem = format_of(archive)
        .and_then(|f| {
            f.suffix()
                .iter()
                .find(|s| lower.ends_with(**s))
                .map(|s| name[..name.len() - s.len()].to_owned())
        })
        .unwrap_or_else(|| name.clone());

    let stem = if stem.is_empty() { name } else { stem };

    let mut candidate = parent.join(&stem);
    for n in 2..=9999 {
        if !taken(&candidate) {
            return Some(candidate);
        }
        candidate = parent.join(format!("{stem} ({n})"));
    }
    Some(candidate)
}

/// How far along an operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub done: u64,
    /// Zero when the total is not known ahead of time, which is the normal case
    /// for a streamed tar.
    pub total: u64,
}

/// What a caller returns to keep going or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Yes,
    Stop,
}

/// What an operation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub files: u64,
    pub bytes: u64,
    /// Entries that were refused, with the reason. Reported rather than
    /// swallowed: an extraction that silently drops files is worse than one
    /// that says which and why.
    pub refused: Vec<String>,
    /// True when the caller asked to stop before the end.
    pub cancelled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("this is not a format Neutron can open")]
    UnknownFormat,
    #[error("the archive is damaged or uses a feature Neutron does not support: {0}")]
    Unreadable(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn formats_are_recognised_by_extension() {
        assert_eq!(format_of(Path::new("a.zip")), Some(Format::Zip));
        assert_eq!(format_of(Path::new("a.TAR")), Some(Format::Tar));
        assert_eq!(format_of(Path::new("a.tar.gz")), Some(Format::TarGz));
        assert_eq!(format_of(Path::new("a.tgz")), Some(Format::TarGz));
        assert_eq!(format_of(Path::new("a.txt")), None);
        assert_eq!(format_of(Path::new("a.7z")), None);
    }

    #[test]
    fn a_tar_gz_is_not_mistaken_for_a_tar() {
        // `.tar.gz` ends with `.gz` and contains `.tar`; matching the shorter
        // suffix first would try to read a gzip stream as a tar.
        assert_eq!(format_of(Path::new("src.tar.gz")), Some(Format::TarGz));
    }

    #[test]
    fn the_destination_drops_the_extension() {
        let free = |_: &Path| false;
        assert_eq!(
            destination_for(Path::new("/d/photos.zip"), free),
            Some(PathBuf::from("/d/photos"))
        );
        assert_eq!(
            destination_for(Path::new("/d/src.tar.gz"), free),
            Some(PathBuf::from("/d/src"))
        );
    }

    #[test]
    fn a_taken_destination_gets_a_number() {
        // Extracting the same download twice should not merge into one folder.
        let taken = |p: &Path| p == Path::new("/d/photos");
        assert_eq!(
            destination_for(Path::new("/d/photos.zip"), taken),
            Some(PathBuf::from("/d/photos (2)"))
        );
    }

    #[test]
    fn an_archive_named_only_by_its_extension_still_gets_a_folder() {
        // `.zip` has no stem; using the empty string would put the contents in
        // the parent directory itself.
        let free = |_: &Path| false;
        assert_eq!(
            destination_for(Path::new("/d/.zip"), free),
            Some(PathBuf::from("/d/.zip"))
        );
    }
}
