//! Renaming several items at once, as the Finder's Rename Items does: replace
//! some text in every name, add text to every name, or give them all one name
//! with a counter.
//!
//! The rules are pure — names in, names out — so the dialog can preview them
//! as they are typed, and so they can be tested without a disk. Only
//! `carry_out` touches files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How the new names are made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// Every occurrence of `find` in a name becomes `with`.
    Replace { find: String, with: String },
    /// `text` goes before the name, or after it and before its extension.
    Add { text: String, after: bool },
    /// `base` and a counter from `start`, each keeping its own extension:
    /// "Holiday 1.jpg", "Holiday 2.jpg".
    Format { base: String, start: u32 },
}

/// The name `name` becomes under `rule`, as the `index`th of the batch.
pub fn apply(rule: &Rule, name: &str, index: usize, is_dir: bool) -> String {
    let (stem, ext) = split(name, is_dir);
    match rule {
        Rule::Replace { find, with } if !find.is_empty() => name.replace(find.as_str(), with),
        Rule::Replace { .. } => name.to_owned(),
        Rule::Add { text, after: false } => format!("{text}{name}"),
        Rule::Add { text, after: true } => format!("{stem}{text}{ext}"),
        Rule::Format { base, start } => format!("{base} {}{ext}", *start as usize + index),
    }
}

/// Why the batch cannot be applied as it stands, if it cannot: a name the
/// system would refuse, or two items given the same name.
pub fn problem(new_names: &[String]) -> Option<String> {
    for name in new_names {
        if let Some(why) = neutron_core::naming::rejection(name) {
            return Some(format!("{name:?}: {why}"));
        }
    }
    let mut seen = HashSet::new();
    for name in new_names {
        // The default Mac volume does not tell `A.txt` from `a.txt`.
        if !seen.insert(name.to_lowercase()) {
            return Some(format!("Two items would both be called {name}"));
        }
    }
    None
}

/// Renames each `(old, new)` in `dir`, returning `(was, is)` for each one done
/// and a line for each that was not. Blocks on the disk: call it from a worker.
///
/// A new name already taken by something outside the batch is refused, never
/// overwritten. One taken by another item of the batch — swapping two names,
/// or shifting a numbered run — is fine: every item first steps aside to a
/// temporary name, so no rename lands on a name not yet given up.
pub fn carry_out(dir: &Path, pairs: &[(String, String)]) -> (Vec<(PathBuf, PathBuf)>, Vec<String>) {
    let pairs: Vec<&(String, String)> = pairs.iter().filter(|(old, new)| old != new).collect();
    let olds: HashSet<String> = pairs.iter().map(|(old, _)| old.to_lowercase()).collect();
    let mut failed = Vec::new();
    let mut ready = Vec::new();
    for pair @ (_, new) in pairs {
        if !olds.contains(&new.to_lowercase()) && dir.join(new).symlink_metadata().is_ok() {
            failed.push(format!("{new} is already here"));
        } else {
            ready.push(pair);
        }
    }
    let shuffled = ready.iter().any(|(_, new)| olds.contains(&new.to_lowercase()));

    let mut done = Vec::new();
    let mut aside = Vec::new();
    for (i, (old, new)) in ready.into_iter().enumerate() {
        let from = dir.join(old);
        if !shuffled {
            match std::fs::rename(&from, dir.join(new)) {
                Ok(()) => done.push((from, dir.join(new))),
                Err(e) => failed.push(format!("{old}: {e}")),
            }
            continue;
        }
        let temp = dir.join(format!(".neutron-rename-{}-{i}", std::process::id()));
        match std::fs::rename(&from, &temp) {
            Ok(()) => aside.push((from, temp, new)),
            Err(e) => failed.push(format!("{old}: {e}")),
        }
    }
    for (from, temp, new) in aside {
        let to = dir.join(new);
        // Something could have taken the name meanwhile; the item goes back
        // to its own name rather than over it.
        let target = if to.symlink_metadata().is_ok() { &from } else { &to };
        match std::fs::rename(&temp, target) {
            Ok(()) if target == &to => done.push((from, to)),
            Ok(()) => failed.push(format!("{new} was taken while renaming")),
            Err(e) => failed.push(format!("{new}: {e} (left as {})", temp.display())),
        }
    }
    (done, failed)
}

/// A name's stem and its extension, with the dot. A folder, and a name whose
/// only dot leads it, have no extension.
fn split(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_changes_every_occurrence_and_nothing_when_empty() {
        let rule = Rule::Replace { find: "IMG".into(), with: "Photo".into() };
        assert_eq!(apply(&rule, "IMG_001 IMG.jpg", 0, false), "Photo_001 Photo.jpg");
        let none = Rule::Replace { find: String::new(), with: "x".into() };
        assert_eq!(apply(&none, "a.txt", 0, false), "a.txt");
    }

    #[test]
    fn added_text_goes_before_the_name_or_before_its_extension() {
        assert_eq!(apply(&Rule::Add { text: "old ".into(), after: false }, "a.txt", 0, false), "old a.txt");
        assert_eq!(apply(&Rule::Add { text: " v2".into(), after: true }, "a.txt", 0, false), "a v2.txt");
        assert_eq!(apply(&Rule::Add { text: " v2".into(), after: true }, "dir.d", 0, true), "dir.d v2");
        assert_eq!(apply(&Rule::Add { text: "-x".into(), after: true }, ".env", 0, false), ".env-x");
    }

    #[test]
    fn a_format_counts_from_its_start_and_keeps_each_extension() {
        let rule = Rule::Format { base: "Holiday".into(), start: 3 };
        assert_eq!(apply(&rule, "IMG_9.JPG", 0, false), "Holiday 3.JPG");
        assert_eq!(apply(&rule, "clip.mov", 1, false), "Holiday 4.mov");
    }

    #[test]
    fn clashing_or_invalid_names_are_caught_before_anything_changes() {
        assert!(problem(&["a.txt".into(), "b.txt".into()]).is_none());
        assert!(problem(&["a.txt".into(), "A.TXT".into()]).is_some());
        assert!(problem(&["a/b".into()]).is_some());
    }

    #[test]
    fn names_are_swapped_and_never_overwrite_what_is_outside_the_batch() {
        let dir = std::env::temp_dir().join(format!("neutron-batch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in [("a", "A"), ("b", "B"), ("c", "C"), ("keep", "K")] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let pairs = [("a".into(), "b".into()), ("b".into(), "a".into()), ("c".into(), "keep".into())];
        let (done, failed) = carry_out(&dir, &pairs);
        assert_eq!(done.len(), 2);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(dir.join("keep")).unwrap(), "K");
        assert_eq!(std::fs::read_to_string(dir.join("c")).unwrap(), "C");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
