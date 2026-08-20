//! Turning an archive entry's name into a path it is safe to write.
//!
//! # This is the security boundary
//!
//! An archive is a file from somewhere else, and the names inside it are
//! attacker-controlled strings, not paths. An entry called `..\..\..\Windows\
//! System32\drivers\etc\hosts` is a valid zip entry, and an extractor that
//! joins entry names onto a destination writes exactly there. That bug has a
//! name — Zip Slip — and it has been found in extractors in every language,
//! repeatedly, because joining paths is the obvious thing to do and it is
//! wrong.
//!
//! So nothing here joins anything until the name has been taken apart and
//! rebuilt from components that are known to be ordinary. Anything else is
//! refused outright: a refused entry costs the user one file, and a wrong one
//! costs them their machine.

use std::path::{Component, Path, PathBuf};

/// Why an entry was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Empty, or nothing left after the parts were checked.
    Empty,
    /// Rooted at a drive, a share, or `/` — would ignore the destination.
    Absolute,
    /// Contains `..`, which climbs out of the destination.
    Traversal,
    /// A Windows device name, which cannot be created and is a trap besides.
    ReservedName,
}

/// The path `entry` should be written to under `root`, or why it must not be.
///
/// The returned path is always inside `root`.
pub fn safe_join(root: &Path, entry: &str) -> Result<PathBuf, Refusal> {
    if entry.trim().is_empty() {
        return Err(Refusal::Empty);
    }

    // Archives are specified to use forward slashes, and plenty of real ones
    // use backslashes anyway. Normalising first means the component walk below
    // sees the separators whichever was written, on whichever platform this
    // runs — `Path` on Linux does not treat `\` as a separator, and an
    // extractor that only splits on `/` would let `..\..` through as a single
    // innocent-looking component.
    let normalised = entry.replace('\\', "/");

    // Rejected before the component walk, because `Path::components` quietly
    // absorbs a leading separator and a drive prefix into a `RootDir`/`Prefix`
    // that is easy to forget to check.
    if normalised.starts_with('/') || has_drive_prefix(&normalised) {
        return Err(Refusal::Absolute);
    }

    let mut out = PathBuf::new();
    for component in Path::new(&normalised).components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if is_reserved(&name) {
                    return Err(Refusal::ReservedName);
                }
                out.push(part);
            }
            // `.` contributes nothing and is common in tar archives.
            Component::CurDir => {}
            Component::ParentDir => return Err(Refusal::Traversal),
            Component::RootDir | Component::Prefix(_) => return Err(Refusal::Absolute),
        }
    }

    if out.as_os_str().is_empty() {
        return Err(Refusal::Empty);
    }
    Ok(root.join(out))
}

/// Whether `s` starts with something like `C:` or `\\server`.
///
/// Checked textually rather than with `Path::components`, because this code
/// also runs on Linux — in tests, and in the pure half of the workspace — where
/// `C:\evil` is one perfectly ordinary relative filename.
fn has_drive_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    // `//server/share` — the separators are already normalised by the caller.
    if s.starts_with("//") {
        return true;
    }
    // `C:` in any position of the first component, since `C:foo` is also
    // drive-relative and resolves against that drive's current directory.
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Whether a single path component is a Windows device name.
///
/// These cannot be created as files whatever the extension, so an archive
/// containing one is either broken or trying something. Declining by name is
/// clearer than letting the write fail with "the parameter is incorrect".
fn is_reserved(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/dest")
    }

    #[test]
    fn an_ordinary_entry_lands_under_the_destination() {
        let out = safe_join(&root(), "docs/readme.md").unwrap();
        assert_eq!(out, PathBuf::from("/dest/docs/readme.md"));
    }

    #[test]
    fn backslash_separators_are_understood() {
        // Written by plenty of real archivers despite the specification.
        let out = safe_join(&root(), r"docs\readme.md").unwrap();
        assert_eq!(out, PathBuf::from("/dest/docs/readme.md"));
    }

    #[test]
    fn a_parent_component_is_refused() {
        assert_eq!(safe_join(&root(), "../escape").unwrap_err(), Refusal::Traversal);
        assert_eq!(safe_join(&root(), "a/../../escape").unwrap_err(), Refusal::Traversal);
    }

    #[test]
    fn a_parent_component_written_with_backslashes_is_refused() {
        // The one that gets through extractors which only split on `/`: on
        // Linux `..\..\x` is a single ordinary-looking component.
        assert_eq!(
            safe_join(&root(), r"..\..\Windows\System32\drivers\etc\hosts").unwrap_err(),
            Refusal::Traversal
        );
    }

    #[test]
    fn an_absolute_unix_path_is_refused() {
        assert_eq!(safe_join(&root(), "/etc/passwd").unwrap_err(), Refusal::Absolute);
    }

    #[test]
    fn a_drive_qualified_path_is_refused() {
        assert_eq!(safe_join(&root(), r"C:\Windows\evil.dll").unwrap_err(), Refusal::Absolute);
        assert_eq!(safe_join(&root(), "C:/Windows/evil.dll").unwrap_err(), Refusal::Absolute);
    }

    #[test]
    fn a_drive_relative_path_is_refused() {
        // `C:evil` is not absolute, but it resolves against C:'s current
        // directory rather than the destination — which is just as wrong.
        assert_eq!(safe_join(&root(), "C:evil").unwrap_err(), Refusal::Absolute);
    }

    #[test]
    fn a_unc_path_is_refused() {
        assert_eq!(
            safe_join(&root(), r"\\attacker\share\payload").unwrap_err(),
            Refusal::Absolute
        );
    }

    #[test]
    fn a_current_directory_component_is_ignored() {
        // Ordinary in tar archives, which often prefix everything with `./`.
        let out = safe_join(&root(), "./docs/./readme.md").unwrap();
        assert_eq!(out, PathBuf::from("/dest/docs/readme.md"));
    }

    #[test]
    fn an_empty_entry_is_refused() {
        assert_eq!(safe_join(&root(), "").unwrap_err(), Refusal::Empty);
        assert_eq!(safe_join(&root(), "   ").unwrap_err(), Refusal::Empty);
        assert_eq!(safe_join(&root(), "./").unwrap_err(), Refusal::Empty);
    }

    #[test]
    fn a_device_name_is_refused_with_or_without_an_extension() {
        assert_eq!(safe_join(&root(), "CON").unwrap_err(), Refusal::ReservedName);
        assert_eq!(safe_join(&root(), "sub/nul.txt").unwrap_err(), Refusal::ReservedName);
    }

    #[test]
    fn a_name_that_merely_starts_like_a_device_is_allowed() {
        assert!(safe_join(&root(), "console.log").is_ok());
        assert!(safe_join(&root(), "COM10.txt").is_ok());
    }

    #[test]
    fn every_accepted_path_stays_under_the_destination() {
        // The single property the whole module exists to guarantee, asserted
        // over everything above rather than trusted per case.
        let candidates = [
            "a", "a/b/c.txt", "./x", r"a\b", "docs/read me.txt", "ünïcode/файл.txt",
            "..", "../x", r"..\x", "/x", r"C:\x", "C:x", r"\\s\x", "", "CON", "a/../b",
        ];
        for entry in candidates {
            if let Ok(path) = safe_join(&root(), entry) {
                assert!(
                    path.starts_with(root()),
                    "{entry:?} produced {path:?}, which is outside the destination"
                );
                assert!(
                    !path.components().any(|c| c == Component::ParentDir),
                    "{entry:?} produced {path:?}, which still contains a parent component"
                );
            }
        }
    }
}
