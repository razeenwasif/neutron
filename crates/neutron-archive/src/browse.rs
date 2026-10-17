//! Looking inside an archive without unpacking it.
//!
//! A zip says what it holds in a directory at its end, which is read without
//! decompressing anything. A tar has no directory and is read through once;
//! the [`Index`] built from either is kept by the caller, so moving between
//! folders inside an archive reads it once, not once per folder.
//!
//! Names are only shown here, never written to disk — extracting goes through
//! [`crate::extract::extract_selected`] and its checks. They are still
//! normalised, so a name with `..` or a leading `/` cannot make a folder appear
//! somewhere it is not.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::{ArchiveError, Format};

/// One thing in a folder of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub is_dir: bool,
    /// Uncompressed, for a file.
    pub size: u64,
    /// Milliseconds since the Unix epoch; 0 when the archive does not say.
    pub modified: i64,
}

/// Every entry of an archive, by its path inside it.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// Normalised path, whether it is a folder, size, modified.
    entries: Vec<(String, bool, u64, i64)>,
}

impl Index {
    /// Reads what `archive` holds.
    pub fn read(archive: &Path) -> Result<Index, ArchiveError> {
        let format = crate::format_of(archive).ok_or(ArchiveError::UnknownFormat)?;
        let mut index = Index::default();
        match format {
            Format::Zip => {
                let file = fs::File::open(archive)?;
                let mut zip = zip::ZipArchive::new(io::BufReader::new(file))
                    .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
                for i in 0..zip.len() {
                    let Ok(entry) = zip.by_index_raw(i) else { continue };
                    let modified = entry
                        .last_modified()
                        .and_then(|d| {
                            crate::clock::unix_from_civil(crate::clock::Civil {
                                year: d.year(),
                                month: d.month(),
                                day: d.day(),
                                hour: d.hour(),
                                minute: d.minute(),
                                second: d.second(),
                            })
                        })
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_millis() as i64);
                    index.add(entry.name(), entry.is_dir(), entry.size(), modified);
                }
            }
            Format::Tar => index.tar(tar::Archive::new(fs::File::open(archive)?))?,
            Format::TarGz => {
                index.tar(tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?)))?
            }
            Format::TarXz | Format::TarBz2 | Format::SevenZ | Format::Rar => {
                for (name, dir, size) in crate::external::list(archive)? {
                    index.add(&name, dir, size, 0);
                }
            }
            // One file: the archive's own name less `.gz`, from when it was made.
            Format::Gz => {
                let modified = fs::metadata(archive)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                let name = crate::external::gunzipped_name(archive);
                index.add(&name, false, crate::external::gunzipped_size(archive), modified);
            }
        }
        Ok(index)
    }

    fn tar<R: Read>(&mut self, mut archive: tar::Archive<R>) -> Result<(), ArchiveError> {
        let entries = archive.entries().map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let kind = entry.header().entry_type();
            if matches!(kind, tar::EntryType::XGlobalHeader | tar::EntryType::XHeader) {
                continue;
            }
            let Ok(name) = entry.path().map(|p| p.to_string_lossy().into_owned()) else { continue };
            let modified = entry.header().mtime().map_or(0, |s| (s as i64).saturating_mul(1000));
            let size = entry.header().size().unwrap_or(0);
            self.add(&name, kind == tar::EntryType::Directory, size, modified);
        }
        Ok(())
    }

    fn add(&mut self, name: &str, dir: bool, size: u64, modified: i64) {
        let (path, slash) = normalise(name);
        if path.is_empty() {
            return;
        }
        self.entries.push((path, dir || slash, size, modified));
    }

    /// Whether `inner` is a folder in the archive; the root always is.
    pub fn is_dir(&self, inner: &str) -> bool {
        let inner = normalise(inner).0;
        inner.is_empty()
            || self.entries.iter().any(|(path, dir, ..)| {
                (*dir && *path == inner) || path.strip_prefix(inner.as_str()).is_some_and(|r| r.starts_with('/'))
            })
    }

    /// The folder at `inner` — `""` for the top — as files and folders.
    ///
    /// Folders an archive implies but never lists, as most zips do, appear
    /// all the same: `a/b/c.txt` alone makes `a` and `a/b`.
    pub fn list(&self, inner: &str) -> Option<Vec<Listed>> {
        let inner = normalise(inner).0;
        if !self.is_dir(&inner) {
            return None;
        }
        let mut found: HashMap<&str, Listed> = HashMap::new();
        for (path, dir, size, modified) in &self.entries {
            let rest = if inner.is_empty() {
                path.as_str()
            } else {
                match path.strip_prefix(inner.as_str()).and_then(|r| r.strip_prefix('/')) {
                    Some(rest) => rest,
                    None => continue,
                }
            };
            let (name, deeper) = match rest.split_once('/') {
                Some((name, _)) => (name, true),
                None => (rest, false),
            };
            if name.is_empty() {
                continue;
            }
            let is_dir = deeper || *dir;
            let listed = found.entry(name).or_insert_with(|| Listed {
                name: name.to_owned(),
                is_dir,
                size: 0,
                modified: 0,
            });
            if is_dir {
                // A folder's date is its newest content's, when it has no
                // entry of its own to say.
                listed.is_dir = true;
                listed.size = 0;
                listed.modified = listed.modified.max(*modified);
            } else if !listed.is_dir {
                listed.size = *size;
                listed.modified = *modified;
            }
        }
        Some(found.into_values().collect())
    }
}

/// A name from an archive as a `/`-separated path with no leading `./` or `/`,
/// no `..` or `.` parts, and whether it ended in `/` (which marks a folder).
pub fn normalise(name: &str) -> (String, bool) {
    let name = name.replace('\\', "/");
    let slash = name.ends_with('/');
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
    (parts.join("/"), slash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[(&str, bool, u64)]) -> Index {
        let mut index = Index::default();
        for (name, dir, size) in entries {
            index.add(name, *dir, *size, 1000);
        }
        index
    }

    fn names(listed: Vec<Listed>) -> Vec<(String, bool)> {
        let mut names: Vec<_> = listed.into_iter().map(|l| (l.name, l.is_dir)).collect();
        names.sort();
        names
    }

    #[test]
    fn folders_a_zip_only_implies_are_listed() {
        let index = index(&[("a/b/c.txt", false, 3), ("top.md", false, 1)]);
        assert_eq!(names(index.list("").unwrap()), vec![("a".into(), true), ("top.md".into(), false)]);
        assert_eq!(names(index.list("a").unwrap()), vec![("b".into(), true)]);
        assert_eq!(names(index.list("a/b/").unwrap()), vec![("c.txt".into(), false)]);
        assert!(index.list("a/b/c.txt").is_none(), "a file is not a folder");
        assert!(index.list("nowhere").is_none());
    }

    #[test]
    fn explicit_folders_and_their_contents_are_one_entry() {
        let index = index(&[("docs/", true, 0), ("docs/a.txt", false, 5), ("empty/", true, 0)]);
        let top = index.list("").unwrap();
        assert_eq!(names(top), vec![("docs".into(), true), ("empty".into(), true)]);
        assert!(index.list("empty").unwrap().is_empty());
    }

    #[test]
    fn names_cannot_climb_out_or_start_at_the_root() {
        assert_eq!(normalise("../../etc/passwd").0, "etc/passwd");
        assert_eq!(normalise("/abs/x").0, "abs/x");
        assert_eq!(normalise("./a\\b/").0, "a/b");
        assert!(normalise("dir/").1);
    }

    #[test]
    fn a_real_zip_is_read_and_one_entry_comes_out_under_its_own_name() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("neutron-browse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("t.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("docs/inner/a.txt", options).unwrap();
            zip.write_all(b"hello").unwrap();
            zip.start_file("docs/b.txt", options).unwrap();
            zip.write_all(b"bee").unwrap();
            zip.finish().unwrap();
        }
        let index = Index::read(&archive).unwrap();
        assert_eq!(names(index.list("docs").unwrap()), vec![("b.txt".into(), false), ("inner".into(), true)]);
        let a = index.list("docs/inner").unwrap().remove(0);
        assert_eq!((a.name.as_str(), a.size), ("a.txt", 5));

        let out = dir.join("out");
        let summary = crate::extract::extract_selected(
            &archive,
            &out,
            Format::Zip,
            &["docs/inner".to_owned(), "docs/b.txt".to_owned()],
            |_| crate::Continue::Yes,
        )
        .unwrap();
        assert_eq!(summary.files, 2);
        assert_eq!(fs::read_to_string(out.join("inner/a.txt")).unwrap(), "hello");
        assert_eq!(fs::read_to_string(out.join("b.txt")).unwrap(), "bee");
        assert!(!out.join("docs").exists(), "came out under its path rather than its name");
        let _ = fs::remove_dir_all(&dir);
    }
}
