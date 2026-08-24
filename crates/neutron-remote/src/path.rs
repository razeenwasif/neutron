//! Remote paths, and the boundary where a remote name becomes a local one.
//!
//! Two jobs that look similar and are not.
//!
//! # Remote paths are POSIX, and ours to keep tidy
//!
//! Both protocols address files with `/`-separated paths, so Neutron tracks the
//! current remote directory as an absolute POSIX path and does its own
//! normalisation: collapse repeated slashes, drop `.`, and resolve `..`
//! lexically against what came before.
//!
//! Resolving `..` lexically is wrong in the presence of symlinks — `/a/b/..` is
//! not `/a` if `b` is a link elsewhere. It is still the right thing here,
//! because this is only ever used to build the path Neutron *asks for*, and the
//! server resolves it properly. What it buys is that the breadcrumb and the
//! parent button agree with each other and never accumulate `../..` segments.
//!
//! # A name from a listing is hostile input
//!
//! This is the part that matters. A directory listing is data the server
//! chooses, and downloading writes those names under a local folder. A server —
//! compromised, malicious, or merely running software with a bug — can answer
//! `LIST` with:
//!
//! ```text
//! -rw-r--r-- 1 u g 42 Nov 5 13:46 ../../../../Users/Razeen/AppData/Roaming/…
//! ```
//!
//! and an FTP client that joins that name onto a download directory writes
//! exactly there. This is Zip Slip with a different transport, and FTP clients
//! have shipped it repeatedly.
//!
//! [`safe_name`] is therefore strict in a way [`neutron_archive::path`] cannot
//! be: an archive entry is a *relative path* and has to keep its directory
//! structure, but a listing entry is a *single name in one directory*. So the
//! rule here is simply that it must contain no path structure at all, which is
//! both easier to be sure of and easier to test.
//!
//! [`neutron_archive::path`]: https://github.com/razeenwasif/neutron

/// Why a name from a listing was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the server sent an entry with no name")]
    Empty,
    #[error("the server sent a name containing a path separator: {name:?}")]
    Separator { name: String },
    #[error("the server sent {name:?}, which is a directory rather than a name")]
    Dots { name: String },
    #[error("the server sent a name containing a drive or stream marker: {name:?}")]
    Colon { name: String },
    #[error("the server sent a name containing a control character: {name:?}")]
    Control { name: String },
    #[error("the server sent {name:?}, which names a device on Windows")]
    Device { name: String },
}

/// Names Windows resolves to a device however they are written, with or without
/// an extension and in any case.
const DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Checks a single name from a directory listing.
///
/// Accepts a name that is safe both to append to a remote path and — the part
/// that matters — to create as a file in a local folder. Everything with any
/// path structure in it is refused, because a listing entry has none.
///
/// Backslash is refused as firmly as forward slash: on Linux `..\..\x` is one
/// perfectly ordinary-looking name, which is how this slips past a check
/// written on a machine that only knows about `/`.
pub fn safe_name(name: &str) -> Result<&str, Refusal> {
    if name.is_empty() {
        return Err(Refusal::Empty);
    }
    if name.contains('/') || name.contains('\\') {
        return Err(Refusal::Separator { name: name.to_owned() });
    }
    if name == "." || name == ".." {
        return Err(Refusal::Dots { name: name.to_owned() });
    }
    // Covers `C:` and, more quietly, `notes.txt:hidden` — an NTFS alternate
    // data stream, which is a different file to the one the name appears to
    // describe.
    if name.contains(':') {
        return Err(Refusal::Colon { name: name.to_owned() });
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(Refusal::Control { name: name.to_owned() });
    }

    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if DEVICES.contains(&stem.as_str()) {
        return Err(Refusal::Device { name: name.to_owned() });
    }

    Ok(name)
}

/// Normalises an absolute remote path.
///
/// Always returns a path that starts with `/`, has no repeated or trailing
/// slashes (except the root itself), and no `.` or `..` segments left in it.
/// A `..` at the root stays at the root rather than escaping, which is what
/// every server does with it.
pub fn normalize(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();

    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    if segments.is_empty() {
        return "/".to_owned();
    }

    let mut out = String::with_capacity(path.len());
    for segment in segments {
        out.push('/');
        out.push_str(segment);
    }
    out
}

/// Appends one listing entry to a directory path.
///
/// The name is checked first, so a server cannot use a listing to move the
/// user somewhere they did not ask to go.
pub fn join(dir: &str, name: &str) -> Result<String, Refusal> {
    let name = safe_name(name)?;
    let dir = normalize(dir);
    Ok(if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    })
}

/// The containing directory, or `None` at the root.
pub fn parent(path: &str) -> Option<String> {
    let path = normalize(path);
    if path == "/" {
        return None;
    }
    Some(match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(cut) => path[..cut].to_owned(),
    })
}

/// The last segment of a path — what to put in a tab title or breadcrumb.
///
/// The root has no name of its own, so it comes back as `/`.
pub fn file_name(path: &str) -> String {
    let path = normalize(path);
    if path == "/" {
        return "/".to_owned();
    }
    match path.rfind('/') {
        Some(cut) => path[cut + 1..].to_owned(),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_is_accepted() {
        for name in ["notes.txt", "a b c.tar.gz", "Ünïcødé", "..hidden", "...", "-rf"] {
            assert!(safe_name(name).is_ok(), "{name:?} was refused");
        }
    }

    #[test]
    fn a_name_that_climbs_out_of_the_download_folder_is_refused() {
        // The whole reason this module exists. A server answering LIST with
        // this name gets a client to write outside the folder the user chose.
        for name in [
            "../../../../Windows/System32/drivers/etc/hosts",
            "..\\..\\..\\Windows\\System32\\drivers\\etc\\hosts",
            "/etc/passwd",
            "sub/file.txt",
            "sub\\file.txt",
            "..",
            ".",
        ] {
            assert!(safe_name(name).is_err(), "{name:?} was accepted");
        }
    }

    #[test]
    fn a_backslash_is_refused_even_though_it_is_legal_on_a_unix_server() {
        // On Linux this is one ordinary filename, which is exactly how the
        // check gets written wrongly — and the file still lands somewhere else
        // entirely once the name reaches Windows.
        let refused = safe_name("..\\evil.exe").unwrap_err();
        assert!(matches!(refused, Refusal::Separator { .. }), "{refused:?}");
    }

    #[test]
    fn a_drive_letter_or_data_stream_is_refused() {
        assert!(safe_name("C:").is_err());
        assert!(safe_name("C:\\Windows").is_err());
        // The quiet one: this is a different file to `notes.txt`.
        assert!(safe_name("notes.txt:payload").is_err());
    }

    #[test]
    fn a_device_name_is_refused() {
        // Creating `NUL` locally does not make a file; it opens a device.
        assert!(safe_name("NUL").is_err());
        assert!(safe_name("con.txt").is_err());
        assert!(safe_name("LPT1").is_err());
        assert!(safe_name("console.txt").is_ok(), "only the reserved stems count");
    }

    #[test]
    fn a_control_character_is_refused() {
        // A newline in a name breaks the listing display, and a CR overprints
        // whatever is drawn beside it.
        assert!(safe_name("two\nlines").is_err());
        assert!(safe_name("carriage\rreturn").is_err());
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert!(safe_name("").is_err());
    }

    #[test]
    fn normalising_produces_one_canonical_form() {
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("//a///b//"), "/a/b");
        assert_eq!(normalize("/a/./b"), "/a/b");
        assert_eq!(normalize("a/b"), "/a/b");
        assert_eq!(normalize("/a/b/"), "/a/b");
    }

    #[test]
    fn dot_dot_resolves_and_stops_at_the_root() {
        assert_eq!(normalize("/a/b/.."), "/a");
        assert_eq!(normalize("/a/../b"), "/b");
        // Climbing past the root stays at the root rather than producing
        // something the server would reject or, worse, interpret.
        assert_eq!(normalize("/../../.."), "/");
        assert_eq!(normalize("/a/../../.."), "/");
    }

    #[test]
    fn joining_checks_the_name() {
        assert_eq!(join("/a", "b.txt").unwrap(), "/a/b.txt");
        assert_eq!(join("/", "b.txt").unwrap(), "/b.txt");
        assert_eq!(join("/a/", "b.txt").unwrap(), "/a/b.txt");
        assert!(join("/a", "../../etc").is_err());
    }

    #[test]
    fn a_join_can_never_leave_the_directory_it_started_in() {
        // The property the callers rely on, stated directly.
        for name in ["ok.txt", "..", "../x", "/x", "a/b", "..\\x", "C:x"] {
            if let Ok(joined) = join("/home/user", name) {
                assert_eq!(joined, format!("/home/user/{name}"));
                assert_eq!(parent(&joined).as_deref(), Some("/home/user"));
            }
        }
    }

    #[test]
    fn parents_walk_up_to_the_root_and_stop() {
        assert_eq!(parent("/a/b/c").as_deref(), Some("/a/b"));
        assert_eq!(parent("/a").as_deref(), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(parent("/a/b/").as_deref(), Some("/a"));
    }

    #[test]
    fn the_name_of_a_path_is_its_last_segment() {
        assert_eq!(file_name("/a/b/c.txt"), "c.txt");
        assert_eq!(file_name("/a"), "a");
        // The root has no name of its own, and a tab titled "" is worse than
        // one titled "/".
        assert_eq!(file_name("/"), "/");
    }
}
