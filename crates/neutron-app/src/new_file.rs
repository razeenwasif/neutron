//! New File: an empty text file, or a copy of a template, made in the folder
//! being looked at under a name nothing there has yet.
//!
//! Templates are whatever files the user puts in Neutron's Templates folder,
//! `~/Library/Application Support/Neutron/Templates`; each new copy is named
//! after its template, as "Invoice.numbers", "Invoice 2.numbers".

use std::io;
use std::path::{Path, PathBuf};

/// The name an empty new file starts with.
pub const UNTITLED: &str = "untitled.txt";

/// Where templates are kept.
pub fn templates_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library/Application Support/Neutron/Templates"))
}

/// The templates' names, sorted, making the folder if it is not there so the
/// user can find it. Blocks on the disk: call it from a worker.
pub fn templates() -> Vec<String> {
    let Some(dir) = templates_dir() else { return Vec::new() };
    let _ = std::fs::create_dir_all(&dir);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// `name`, or `name` with a number before its extension, whichever of
/// "untitled.txt", "untitled 2.txt", "untitled 3.txt"… is first not taken
/// according to `taken`.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} {n}{ext}"))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range runs out of names")
}

/// Makes the file in `dir` — empty, or with `template`'s contents — and
/// returns its name. Never replaces anything: a name taken in the moment
/// between choosing it and creating the file moves on to the next one.
/// Blocks on the disk: call it from a worker.
pub fn create(dir: &Path, template: Option<&Path>) -> io::Result<String> {
    let wanted = template
        .and_then(|t| t.file_name())
        .map_or_else(|| UNTITLED.to_owned(), |n| n.to_string_lossy().into_owned());
    let mut source = template.map(std::fs::File::open).transpose()?;
    for _ in 0..100 {
        let name = unique_name(&wanted, |n| dir.join(n).symlink_metadata().is_ok());
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&name)) {
            Ok(mut file) => {
                if let Some(source) = source.as_mut() {
                    io::copy(source, &mut file)?;
                }
                return Ok(name);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no free name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_the_next_number_before_its_extension() {
        let taken = ["untitled.txt", "untitled 2.txt"];
        assert_eq!(unique_name("untitled.txt", |n| taken.contains(&n)), "untitled 3.txt");
        assert_eq!(unique_name("Makefile", |n| n == "Makefile"), "Makefile 2");
        assert_eq!(unique_name("notes.md", |_| false), "notes.md");
    }

    #[test]
    fn files_are_made_beside_what_is_there_and_copy_their_template() {
        let dir = std::env::temp_dir().join(format!("neutron-newfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("untitled.txt"), "keep").unwrap();
        assert_eq!(create(&dir, None).unwrap(), "untitled 2.txt");
        assert_eq!(std::fs::read_to_string(dir.join("untitled.txt")).unwrap(), "keep");

        let template = dir.join("t").join("Letter.md");
        std::fs::create_dir_all(template.parent().unwrap()).unwrap();
        std::fs::write(&template, "Dear").unwrap();
        assert_eq!(create(&dir, Some(&template)).unwrap(), "Letter.md");
        assert_eq!(std::fs::read_to_string(dir.join("Letter.md")).unwrap(), "Dear");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
