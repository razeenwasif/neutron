//! Turning something typed into somewhere to go.
//!
//! `Ctrl+L` replaces the breadcrumb with a field holding the current location's
//! path. Enter goes there, Escape puts the breadcrumb back. The breadcrumb is
//! for walking a path you can see; this is for pasting one you already have,
//! and those are different enough jobs that neither should be the other's
//! secondary mode.
//!
//! # What gets resolved here, and what does not
//!
//! Everything below is a *guess at intent* from a string, so it must not do
//! anything a user would not predict. It expands what Windows itself expands
//! and nothing more: environment variables, `~`, and a relative path against
//! wherever the pane is.
//!
//! It deliberately does **not** check that the destination exists. That is a
//! filesystem call, this runs on the thread that paints, and the loader already
//! answers a bad location with "Could not open this location" — one honest
//! error in one place beats a second, differently-worded one here.

use std::path::{Path, PathBuf};

use neutron_core::NodeId;

/// What the field should hold when it opens on `location`.
///
/// A path for a real folder; the parsing name for a shell location, because
/// that is the string that would get you back there. Somewhere with neither —
/// the dashboard, a Drive folder — has nothing to put in the field.
pub fn text_for(location: &NodeId) -> Option<String> {
    match location {
        NodeId::Path(p) => Some(p.to_string_lossy().into_owned()),
        NodeId::Shell { parsing, .. } => Some(parsing.to_string()),
        NodeId::Remote { site, path } => Some(format!("{site}:{path}")),
        NodeId::Archive { .. } => Some(location.to_string()),
        NodeId::Home | NodeId::Cloud { .. } | NodeId::Tag { .. } | NodeId::Terminal { .. } | NodeId::Search { .. } => None,
        NodeId::Viewer { path } => Some(path.to_string_lossy().into_owned()),
    }
}

/// Resolves typed text to a location, relative to `current` where it has to be.
///
/// `home` is passed rather than looked up so this stays a pure function: it is
/// the one piece of the address bar worth testing, and it should not need a
/// real profile directory to run.
pub fn resolve(input: &str, current: &NodeId, home: Option<&Path>) -> Option<NodeId> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }

    // A shell parsing name, pasted from somewhere or typed by someone who knows
    // what `::{20D04FE0-…}` is. Passed through untouched: the shell is the only
    // thing that can make sense of it.
    if text.starts_with("::") {
        return Some(NodeId::shell(text.to_owned(), text.to_owned()));
    }

    // `site:/path`, the form the breadcrumb shows for a server. Checked before
    // the drive-letter test below, which a single letter would also match.
    if let Some((site, path)) = text.split_once(":/") {
        if site.len() > 1 && !site.contains(['\\', '/']) {
            return Some(NodeId::remote(site, format!("/{path}").as_str()));
        }
    }

    let expanded = expand(text, home)?;

    // Absolute already, or relative to wherever the pane is. A relative path is
    // what makes typing `docs` useful, and it only means anything when the pane
    // has a path of its own.
    let path = if is_absolute(&expanded) {
        PathBuf::from(expanded)
    } else {
        current.as_path()?.join(expanded)
    };
    let path = normalize(&path);
    Some(into_archive(&path).unwrap_or(NodeId::Path(path)))
}

/// A path that runs through an archive — `…/papers.zip` or
/// `…/papers.zip/2024` — as the folder inside it. Read from the names alone,
/// like the rest of this, so a folder that is called `x.zip` is taken for an
/// archive; such folders are rare enough to accept that.
fn into_archive(path: &Path) -> Option<NodeId> {
    let mut archive = PathBuf::new();
    let mut components = path.components();
    for component in components.by_ref() {
        archive.push(component.as_os_str());
        if neutron_archive::format_of(&archive).is_some() && component.as_os_str() != archive.as_os_str() {
            let inner: Vec<String> = components.map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            return Some(NodeId::archive(archive, inner.join("/")));
        }
    }
    None
}

/// Folds `.` and `..` away and rebuilds the path with the platform's own
/// separator, without touching the disk.
///
/// The loader hands Windows a `\\?\` path, and that form turns off Windows'
/// own handling of both: `C:/Users/me`, `docs/sub` or `..` typed into the field
/// reached the filesystem literally and failed to open. On the Mac `..` did
/// open, but the location kept it — the tab was titled `..` and Up went back
/// down into the folder just left.
///
/// Lexical, so `..` after a symlink goes to the parent of the link rather than
/// of its target — the same answer a shell's `cd ..` gives.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only a real folder name is undone. At the root there is
                // nowhere further up, and a leading `..` on a path that is
                // still relative has nothing to cancel.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Expands `~` and `%VAR%`.
///
/// Returns `None` when a variable does not exist, rather than leaving the
/// literal `%FOO%` in the path: navigating to a folder with a percent sign in
/// its name is not what someone typing `%FOO%` meant, and the error it produces
/// would name a path they never typed.
fn expand(text: &str, home: Option<&Path>) -> Option<String> {
    // `~` alone or followed by a separator. `~bob` is a name like any other —
    // Neutron does not look up other users' homes, and quietly turning it into
    // a folder called `bob` inside yours would be worse than not expanding.
    let tilde = text
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']));
    let text = if let Some(rest) = tilde {
        let home = home?;
        let rest = rest.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            home.to_string_lossy().into_owned()
        } else {
            home.join(rest).to_string_lossy().into_owned()
        }
    } else {
        text.to_owned()
    };

    // `%VAR%` is Windows syntax. Elsewhere a percent sign is just a character,
    // and a folder like `50% off 100%` has to survive the round trip from the
    // field's own text and back.
    if !cfg!(windows) || !text.contains('%') {
        return Some(text);
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            // An unpaired `%` is a literal one. `100%` is a legal folder name.
            out.push_str(&rest[start..]);
            return Some(out);
        };
        let name = &after[..end];
        if name.is_empty() {
            // `%%` is an escaped percent sign.
            out.push('%');
        } else {
            out.push_str(&std::env::var(name).ok()?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Whether `text` is already anchored somewhere.
///
/// `Path::is_absolute` is not enough on Windows: `\\server\share` is absolute
/// and `C:\x` is absolute, but a bare `\x` is *root-relative* — anchored to the
/// current drive rather than to nothing — and treating it as relative to the
/// pane would silently produce a different path.
fn is_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    if text.starts_with('\\') || text.starts_with('/') {
        return true;
    }
    // `C:` or `C:\`.
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> NodeId {
        NodeId::path(r"C:\Users\Razeen\Projects")
    }

    fn home() -> PathBuf {
        PathBuf::from(r"C:\Users\Razeen")
    }

    #[test]
    fn an_absolute_path_is_taken_as_it_is() {
        assert_eq!(
            resolve(r"C:\Windows\System32", &here(), None),
            Some(NodeId::path(r"C:\Windows\System32"))
        );
    }

    /// Typing a bare name is the reason relative paths are resolved at all.
    #[test]
    fn a_relative_path_hangs_off_the_current_folder() {
        assert_eq!(
            resolve("Neutron", &here(), None),
            Some(NodeId::path(here().as_path().unwrap().join("Neutron")))
        );
    }

    /// A leading backslash is anchored to the drive, not to the pane. Treating
    /// it as relative would turn `\Windows` into `…\Projects\Windows`, which is
    /// a different folder that may well exist.
    #[test]
    fn a_root_relative_path_is_not_treated_as_relative() {
        assert_eq!(
            resolve(r"\Windows", &here(), None),
            Some(NodeId::path(r"\Windows"))
        );
    }

    #[test]
    fn a_unc_path_is_absolute() {
        assert_eq!(
            resolve(r"\\server\share", &here(), None),
            Some(NodeId::path(r"\\server\share"))
        );
    }

    #[test]
    fn tilde_expands_to_home_with_and_without_a_tail() {
        assert_eq!(
            resolve("~", &here(), Some(&home())),
            Some(NodeId::path(r"C:\Users\Razeen"))
        );
        assert_eq!(
            resolve(r"~\Downloads", &here(), Some(&home())),
            Some(NodeId::path(home().join("Downloads")))
        );
    }

    /// Without a home directory there is nothing to expand to, and guessing
    /// would send someone somewhere they did not ask for.
    #[test]
    fn tilde_with_no_home_resolves_to_nothing() {
        assert_eq!(resolve("~", &here(), None), None);
    }

    #[cfg(windows)]
    #[test]
    fn an_environment_variable_expands() {
        unsafe { std::env::set_var("NEUTRON_TEST_DIR", r"C:\somewhere") };
        assert_eq!(
            resolve(r"%NEUTRON_TEST_DIR%\inner", &here(), None),
            Some(NodeId::path(r"C:\somewhere\inner"))
        );
    }

    /// Refused rather than left literal: navigating to a folder called
    /// `%NOPE%` is not what someone typing that meant, and the error would name
    /// a path they never typed.
    #[cfg(windows)]
    #[test]
    fn an_unset_variable_resolves_to_nothing() {
        assert_eq!(resolve("%NEUTRON_NOT_SET_ANYWHERE%", &here(), None), None);
    }

    /// A percent sign is a legal character in a folder name, so an unpaired one
    /// has to survive.
    #[cfg(windows)]
    #[test]
    fn a_lone_percent_is_a_literal() {
        assert_eq!(
            resolve(r"C:\100% done", &here(), None),
            Some(NodeId::path(r"C:\100% done"))
        );
        assert_eq!(
            resolve(r"C:\50%%off", &here(), None),
            Some(NodeId::path(r"C:\50%off"))
        );
    }

    #[test]
    fn a_shell_parsing_name_passes_straight_through() {
        let typed = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
        assert_eq!(
            resolve(typed, &here(), None).and_then(|n| n.parsing_name().map(str::to_owned)),
            Some(typed.to_owned())
        );
    }

    /// The form the breadcrumb shows for a server, so what is displayed can be
    /// pasted back.
    #[test]
    fn a_site_and_path_becomes_a_remote_location() {
        assert_eq!(
            resolve("prod:/pub/incoming", &here(), None),
            Some(NodeId::remote("prod", "/pub/incoming"))
        );
    }

    /// `C:/x` is a drive letter with a forward slash, not a site called "C".
    #[test]
    fn a_drive_letter_is_not_mistaken_for_a_site() {
        assert_eq!(
            resolve("C:/Windows", &here(), None),
            Some(NodeId::path("C:/Windows"))
        );
    }

    #[test]
    fn nothing_typed_goes_nowhere() {
        assert_eq!(resolve("", &here(), None), None);
        assert_eq!(resolve("   ", &here(), None), None);
    }

    /// Whitespace around a pasted path is normal and should not stop it.
    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            resolve("  C:\\Windows  ", &here(), None),
            Some(NodeId::path(r"C:\Windows"))
        );
    }

    /// A relative path needs somewhere to be relative *to*. The dashboard has
    /// no path, so a bare name there resolves to nothing rather than to the
    /// wrong folder.
    #[test]
    fn a_relative_path_from_a_location_without_one_resolves_to_nothing() {
        assert_eq!(resolve("docs", &NodeId::Home, None), None);
    }

    #[test]
    fn the_field_opens_holding_where_you_are() {
        assert_eq!(
            text_for(&NodeId::path(r"C:\Windows")).as_deref(),
            Some(r"C:\Windows")
        );
        assert_eq!(
            text_for(&NodeId::remote("prod", "/pub")).as_deref(),
            Some("prod:/pub")
        );
        // Nothing sensible to put in the field, so it does not open.
        assert_eq!(text_for(&NodeId::Home), None);
    }

    /// What the field shows has to be what `resolve` accepts, or `Ctrl+L`
    /// followed by Enter would move you.
    #[test]
    fn what_the_field_shows_round_trips() {
        for place in [
            NodeId::path(r"C:\Windows\System32"),
            NodeId::remote("prod", "/pub"),
            NodeId::shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC"),
        ] {
            let text = text_for(&place).expect("has a text form");
            let back = resolve(&text, &here(), None).expect("resolves back");
            assert_eq!(
                text_for(&back),
                text_for(&place),
                "{text} did not round trip"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;

    fn here() -> NodeId {
        NodeId::path("/Users/test/Documents")
    }

    #[test]
    fn dots_are_folded_away() {
        assert_eq!(resolve("..", &here(), None), Some(NodeId::path("/Users/test")));
        assert_eq!(resolve("../Music/./x", &here(), None), Some(NodeId::path("/Users/test/Music/x")));
        assert_eq!(resolve("a/b/../c", &here(), None), Some(NodeId::path("/Users/test/Documents/a/c")));
        // Nothing above the root.
        assert_eq!(resolve("/../..", &here(), None), Some(NodeId::path("/")));
    }

    #[test]
    fn percent_signs_are_ordinary_characters() {
        assert_eq!(
            resolve("/Users/test/50% off 100%", &here(), None),
            Some(NodeId::path("/Users/test/50% off 100%"))
        );
    }

    #[test]
    fn a_tilde_that_starts_a_name_is_left_alone() {
        let home = Path::new("/Users/test");
        assert_eq!(resolve("~bob", &here(), Some(home)), Some(NodeId::path("/Users/test/Documents/~bob")));
        assert_eq!(resolve("~/x", &here(), Some(home)), Some(NodeId::path("/Users/test/x")));
    }

    #[test]
    fn unix_absolute_relative_and_home_paths_resolve() {
        let current = NodeId::path("/Users/test/Documents");
        assert_eq!(resolve("/Applications", &current, None), Some(NodeId::path("/Applications")));
        assert_eq!(resolve("notes", &current, None), Some(NodeId::path("/Users/test/Documents/notes")));
        assert_eq!(resolve("~/Downloads", &current, Some(Path::new("/Users/test"))), Some(NodeId::path("/Users/test/Downloads")));
    }

    #[test]
    #[cfg(unix)]
    fn a_path_through_an_archive_opens_inside_it() {
        let here = NodeId::path("/x");
        assert_eq!(resolve("/x/a.zip/docs/2024", &here, None), Some(NodeId::archive("/x/a.zip", "docs/2024")));
        assert_eq!(resolve("/x/a.ZIP", &here, None), Some(NodeId::archive("/x/a.ZIP", "")));
        assert_eq!(resolve("b.tar.gz/src", &here, None), Some(NodeId::archive("/x/b.tar.gz", "src")));
        assert_eq!(resolve("/x/notes.txt", &here, None), Some(NodeId::path("/x/notes.txt")));
        assert_eq!(text_for(&NodeId::archive("/x/a.zip", "docs")).as_deref(), Some("/x/a.zip/docs"));
    }
}
