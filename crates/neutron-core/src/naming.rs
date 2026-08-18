//! What a file may be called, and how to pick a name that is free.
//!
//! Pure rules, so they can be checked as the user types rather than after the
//! shell refuses. A rename box that goes red on `?` is a better explanation
//! than a modal dialog after the fact.

/// Characters Windows will not accept in a name.
///
/// `:` and `\` are the two people actually try — pasting a path into a rename
/// box, or typing a time as `12:30`.
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Names reserved by the device namespace, which cannot be used even with an
/// extension: `CON.txt` is as rejected as `CON`.
///
/// A holdover from DOS device files that Windows still honours, and one of the
/// few ways a perfectly reasonable name — `AUX`, `PRN` — fails for reasons that
/// look arbitrary from the outside.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why `name` cannot be used, or `None` if it can.
///
/// The message is shown to the user, so it says what to do rather than what
/// went wrong.
pub fn rejection(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("A name cannot be empty");
    }
    if name.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Some(r#"A name cannot contain \ / : * ? " < > |"#);
    }
    // Control characters are legal in the API and a disaster everywhere else —
    // they render as nothing, which makes two different files look identical.
    if name.chars().any(|c| c.is_control()) {
        return Some("A name cannot contain control characters");
    }
    // Windows silently strips these, so the file ends up with a name the user
    // did not type. Refusing is more honest than surprising them.
    if name.ends_with(' ') || name.ends_with('.') {
        return Some("A name cannot end with a space or a full stop");
    }
    if name == "." || name == ".." {
        return Some("That name is reserved");
    }

    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return Some("That name is reserved by Windows");
    }

    // The shell's own limit for a single component. Long *paths* are handled
    // with `\\?\` elsewhere; a single name has no such escape.
    if name.chars().count() > 255 {
        return Some("That name is too long");
    }

    None
}

pub fn is_valid(name: &str) -> bool {
    rejection(name).is_none()
}

/// The part of a file name a rename box should select: everything before the
/// last dot.
///
/// Renaming usually means changing the name, not the extension, and having to
/// unselect `.txt` every time is the kind of small friction that makes a file
/// manager feel unfinished. A dotfile has no stem to speak of — `.gitignore` is
/// all name — so the whole thing is selected.
pub fn stem_len(name: &str) -> usize {
    match name.rfind('.') {
        Some(0) | None => name.chars().count(),
        Some(byte) => name[..byte].chars().count(),
    }
}

/// The first of `base`, `base (2)`, `base (3)`… that `taken` says is free.
///
/// Matches Explorer's shape for a new folder. Gives up after a bounded search
/// rather than looping forever, because `taken` reads a real directory and a
/// pathological one should not hang the worker.
pub fn next_available(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    for n in 2..=9999 {
        let candidate = format!("{base} ({n})");
        if !taken(&candidate) {
            return candidate;
        }
    }
    // Nothing sensible is left; hand back something unique enough to land.
    format!("{base} ({})", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_is_accepted() {
        assert!(is_valid("notes.txt"));
        assert!(is_valid("A folder"));
        assert!(is_valid("émoji 🎉.png"));
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert!(rejection("").is_some());
    }

    #[test]
    fn path_separators_are_refused() {
        // The commonest mistake is pasting a whole path into the box.
        assert!(rejection(r"C:\Users\thing").is_some());
        assert!(rejection("a/b").is_some());
    }

    #[test]
    fn a_colon_is_refused() {
        // Typing a time into a name is the other common one.
        assert!(rejection("meeting 12:30").is_some());
    }

    #[test]
    fn a_trailing_space_or_dot_is_refused() {
        // Windows strips these silently, so the saved name is not the typed one.
        assert!(rejection("notes ").is_some());
        assert!(rejection("notes.").is_some());
    }

    #[test]
    fn device_names_are_refused_with_or_without_an_extension() {
        assert!(rejection("CON").is_some());
        assert!(rejection("con.txt").is_some());
        assert!(rejection("LPT1.log").is_some());
    }

    #[test]
    fn a_name_that_merely_starts_like_a_device_is_fine() {
        // "CONTENTS" is not "CON", and refusing it would be maddening.
        assert!(is_valid("CONTENTS"));
        assert!(is_valid("COM10"));
    }

    #[test]
    fn the_rejection_says_what_to_do() {
        let message = rejection("a/b").unwrap();
        assert!(message.contains('/'), "{message:?} does not name the problem");
    }

    #[test]
    fn the_stem_stops_before_the_extension() {
        assert_eq!(stem_len("notes.txt"), 5);
    }

    #[test]
    fn a_dotfile_is_all_stem() {
        // Selecting "" and leaving ".gitignore" would be useless.
        assert_eq!(stem_len(".gitignore"), 10);
    }

    #[test]
    fn a_name_without_a_dot_is_all_stem() {
        assert_eq!(stem_len("Makefile"), 8);
    }

    #[test]
    fn the_stem_stops_at_the_last_dot() {
        assert_eq!(stem_len("archive.tar.gz"), 11);
    }

    #[test]
    fn the_stem_is_counted_in_characters_not_bytes() {
        // A byte length would slice a multi-byte character in half when the
        // rename box set its selection.
        assert_eq!(stem_len("café.txt"), 4);
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        assert_eq!(next_available("New folder", |_| false), "New folder");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        assert_eq!(
            next_available("New folder", |n| n == "New folder"),
            "New folder (2)"
        );
    }

    #[test]
    fn numbering_continues_past_the_first_gap() {
        let taken = |n: &str| matches!(n, "New folder" | "New folder (2)" | "New folder (3)");
        assert_eq!(next_available("New folder", taken), "New folder (4)");
    }

    #[test]
    fn an_exhausted_search_still_returns_something() {
        // Better a long odd name than a hung worker thread.
        let name = next_available("x", |_| true);
        assert!(name.starts_with("x ("));
    }
}
