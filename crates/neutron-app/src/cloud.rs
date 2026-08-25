//! Fetching a cloud object to a local copy, so the shell can open it.
//!
//! The counterpart to [`crate::remote`]'s download, and deliberately the same
//! shape: an object with no local presence is fetched under `%TEMP%\Neutron`,
//! and opening it opens the copy. The status bar says so, because editing that
//! copy does not write back.
//!
//! # A Drive name is not a filename
//!
//! This is the part that needs care. Drive names are not paths, so Drive
//! imposes almost no rules on them: `Q3/Q4 forecast`, `..`, `report<final>` and
//! a name with a newline in it are all names Drive will hand back, and joining
//! any of them onto a directory is how a download writes somewhere it was never
//! meant to. It is Zip Slip with a third transport, after the archive extractor
//! and the FTP listing.
//!
//! The listing side refuses such names outright, because a *server* sending one
//! is misbehaving. Here refusing would be wrong: a slash in a Drive name is
//! ordinary and legal, and a user whose file is called `Q3/Q4 forecast` would
//! simply be unable to open it. So the name is rewritten into something that is
//! certainly a single safe component — and then handed to the same rejector the
//! listing side uses, as a proof rather than a second opinion.

use std::path::{Path, PathBuf};

use neutron_cloud::drive::{self, Continue, Fetch};

/// Longest local name produced. Windows caps a path component at 255, and a
/// Drive name has no limit worth relying on.
const MAX_NAME: usize = 120;

/// Where a fetched cloud object is put before it is opened.
///
/// Beside the server cache and for the same reason: these are copies made to
/// satisfy a double-click, and scattering them through the user's own folders
/// makes them the user's problem to clean up.
pub fn cache_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir().join("Neutron").join("GoogleDrive"))
}

/// Rewrites a Drive name into one safe local path component.
///
/// `extension` is appended when the object is being exported rather than
/// downloaded — a Google Doc has no extension of its own, and a `.docx` saved
/// without one is a file Windows has nothing to open.
pub fn local_name(drive_name: &str, extension: Option<&str>) -> String {
    let mut cleaned: String = drive_name
        .chars()
        .map(|c| match c {
            // Path structure, stream separators, and the characters Windows
            // refuses in a name. The colon matters most: `notes.txt:hidden` is
            // an NTFS alternate data stream, a different file to the one the
            // name appears to describe.
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    // Windows silently strips trailing dots and spaces, so `evil. ` and `evil`
    // are the same file — and `..` survives the mapping above untouched,
    // because every character in it is legal.
    cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start().to_owned();

    if cleaned.is_empty() {
        cleaned = "download".to_owned();
    }

    // Truncated on a character boundary: a byte-wise cut would leave invalid
    // UTF-8 in a path.
    if cleaned.chars().count() > MAX_NAME {
        cleaned = cleaned.chars().take(MAX_NAME).collect();
        // Trimmed again, because the cut may have landed just after a dot.
        cleaned = cleaned.trim_end_matches(['.', ' ']).to_owned();
    }

    // `CON`, `PRN`, `AUX`, `NUL`, `COM1` and friends: naming a file one of
    // these opens a device rather than creating a file, whatever the extension.
    if neutron_remote::path::safe_name(&cleaned).is_err() {
        cleaned.insert(0, '_');
    }

    if let Some(extension) = extension {
        if !cleaned
            .to_ascii_lowercase()
            .ends_with(&extension.to_ascii_lowercase())
        {
            cleaned.push_str(extension);
        }
    }

    debug_assert!(
        neutron_remote::path::safe_name(&cleaned).is_ok(),
        "local_name produced an unsafe component: {cleaned:?}"
    );
    cleaned
}

/// Fetches one Drive object into `cache`, returning where it landed.
///
/// **Worker thread only** — this blocks for the whole transfer.
pub fn fetch(
    file_id: &str,
    cache: &Path,
    on_progress: &mut dyn FnMut(u64) -> Continue,
) -> Result<PathBuf, String> {
    let drive = neutron_cloud::google::GoogleDrive::new();

    // What the object *is* decides how it is fetched, and the listing does not
    // keep its MIME type — one field no row renders. One request before a
    // transfer costs nothing next to the transfer.
    let file = drive.stat(file_id)?;
    let how = drive::fetch_for(&file.mime_type);

    let extension = match &how {
        Fetch::Export { extension, .. } => Some(*extension),
        Fetch::Bytes => None,
        // Said plainly rather than attempted: Drive answers an export request
        // for a Form with an error document, and a client that streamed first
        // would save that document under the file's own name.
        Fetch::Impossible => {
            return Err(format!(
                "{} is a Google file with no downloadable form",
                file.name
            ));
        }
    };

    let destination = cache.join(local_name(&file.name, extension));
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let handle = std::fs::File::create(&destination).map_err(|e| e.to_string())?;
    let mut sink = std::io::BufWriter::new(handle);

    let outcome = drive.download(file_id, &how, &mut sink, on_progress);

    // Flushed before the result is read, so a write error on the last buffer is
    // not reported as a successful download.
    use std::io::Write;
    let flushed = sink.flush();

    match outcome {
        Ok(_) => {
            flushed.map_err(|e| e.to_string())?;
            Ok(destination)
        }
        Err(e) => {
            // A partial file is worse than none: it is about to be handed to
            // whatever application opens that type, and half a .docx is a
            // repair dialog rather than a document.
            drop(sink);
            let _ = std::fs::remove_file(&destination);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of this module. Each of these is a name Drive will
    /// accept, and each one joined onto a directory writes outside it.
    #[test]
    fn a_name_carrying_path_structure_becomes_one_component() {
        for hostile in [
            "Q3/Q4 forecast",
            "..\\..\\Windows\\System32\\drivers\\etc\\hosts",
            "../../../secrets",
            "C:\\Windows\\notes.txt",
            "notes.txt:hidden",
        ] {
            let safe = local_name(hostile, None);
            assert!(!safe.contains('/'), "{hostile:?} -> {safe:?}");
            assert!(!safe.contains('\\'), "{hostile:?} -> {safe:?}");
            assert!(!safe.contains(':'), "{hostile:?} -> {safe:?}");
            assert!(
                neutron_remote::path::safe_name(&safe).is_ok(),
                "{hostile:?} -> {safe:?}"
            );
        }
    }

    /// `..` survives character mapping untouched, so it needs the
    /// trailing-dot trim to be caught.
    #[test]
    fn dots_alone_do_not_survive() {
        assert_ne!(local_name("..", None), "..");
        assert_ne!(local_name(".", None), ".");
        assert!(neutron_remote::path::safe_name(&local_name("..", None)).is_ok());
    }

    /// Windows strips these silently, so `evil. ` and `evil` are one file.
    #[test]
    fn trailing_dots_and_spaces_are_removed() {
        assert_eq!(local_name("report. ", None), "report");
        assert_eq!(local_name("  report  ", None), "report");
    }

    #[test]
    fn control_characters_are_replaced() {
        let safe = local_name("clean\r\nname\u{7}", None);
        assert!(!safe.chars().any(char::is_control), "{safe:?}");
    }

    /// Naming a file `NUL` opens a device instead of creating a file.
    #[test]
    fn a_device_name_is_defused() {
        for device in ["NUL", "con", "COM1", "LPT9.txt"] {
            let safe = local_name(device, None);
            assert!(
                neutron_remote::path::safe_name(&safe).is_ok(),
                "{device:?} -> {safe:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_name_is_left_alone() {
        assert_eq!(local_name("Budget 2026.xlsx", None), "Budget 2026.xlsx");
        assert_eq!(
            local_name("notes-v2 (final).pdf", None),
            "notes-v2 (final).pdf"
        );
    }

    /// A Google Doc has no extension of its own; without one Windows has
    /// nothing to open the export with.
    #[test]
    fn an_export_gets_its_extension() {
        assert_eq!(
            local_name("Quarterly plan", Some(".docx")),
            "Quarterly plan.docx"
        );
    }

    /// And not twice, for a Doc whose owner named it "plan.docx".
    #[test]
    fn an_extension_already_present_is_not_repeated() {
        assert_eq!(local_name("plan.docx", Some(".docx")), "plan.docx");
        assert_eq!(local_name("PLAN.DOCX", Some(".docx")), "PLAN.DOCX");
    }

    #[test]
    fn an_empty_name_still_produces_a_file() {
        assert_eq!(local_name("", None), "download");
        assert_eq!(local_name("   ", None), "download");
        assert_eq!(local_name("///", None), "___");
    }

    #[test]
    fn a_very_long_name_is_capped_and_still_safe() {
        let long = "a".repeat(5000);
        let safe = local_name(&long, Some(".docx"));
        assert!(safe.chars().count() <= MAX_NAME + ".docx".len(), "{safe:?}");
        assert!(safe.ends_with(".docx"));
        assert!(neutron_remote::path::safe_name(&safe).is_ok());
    }

    /// A name that is nothing but dots must still end up openable, and must
    /// still carry its export extension.
    #[test]
    fn a_name_of_nothing_but_dots_is_still_openable() {
        let safe = local_name("....", Some(".pdf"));
        assert!(neutron_remote::path::safe_name(&safe).is_ok(), "{safe:?}");
        assert!(safe.ends_with(".pdf"), "{safe:?}");
    }
}
