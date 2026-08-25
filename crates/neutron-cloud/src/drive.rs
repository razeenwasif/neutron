//! Google Drive API v3: listing folders and mapping them to Neutron entries.
//!
//! # Drive is not a filesystem
//!
//! Three differences matter, and each one breaks an assumption a file manager
//! normally gets to make:
//!
//! * **Objects have ids, not paths.** A file can sit in several folders at
//!   once, so there is no canonical path to show or to navigate by. Everything
//!   here is addressed by id.
//! * **Names are not unique.** Two files in one folder can share a name
//!   exactly. Nothing may key off the name.
//! * **Some files have no bytes.** A Google Doc is a server-side object with no
//!   size and no download without an export format. Reporting it as a zero-byte
//!   file would be a lie a user acts on.
//!
//! # Field masks
//!
//! `files.list` returns a large object per file by default. The mask here asks
//! for the eight fields actually rendered, which is roughly a tenth of the
//! payload — over a folder of a few thousand items that is the difference
//! between a listing and a wait.

use neutron_core::entry::{Entry, EntryKind, SyncState, attr};
use serde::Deserialize;

/// The id Drive uses for "the top of my drive".
pub const ROOT_ID: &str = "root";

/// MIME type Drive gives folders.
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Fields requested per file. Anything not here is not rendered, and asking for
/// it would be paid for on every page of every listing.
pub const FIELDS: &str =
    "nextPageToken,files(id,name,mimeType,size,modifiedTime,trashed,shortcutDetails/targetId)";

/// Page size. Drive's maximum is 1000; smaller pages mean more round trips, and
/// the round trip dominates.
pub const PAGE_SIZE: u32 = 1000;

#[derive(Debug, Clone, Deserialize)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
    /// Absent for folders and for native Google formats. A *string* in the
    /// API, because Drive sizes exceed what JSON numbers represent exactly.
    #[serde(default)]
    pub size: Option<String>,
    #[serde(rename = "modifiedTime", default)]
    pub modified_time: Option<String>,
    #[serde(default)]
    pub trashed: bool,
    #[serde(rename = "shortcutDetails", default)]
    pub shortcut: Option<Shortcut>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Shortcut {
    #[serde(rename = "targetId")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileList {
    #[serde(default)]
    pub files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

impl DriveFile {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }

    /// Whether this is a native Google format — Doc, Sheet, Slide, Form.
    ///
    /// These have no bytes to download without choosing an export format, and
    /// no size. Worth knowing about rather than rendering as an empty file.
    pub fn is_native_google_format(&self) -> bool {
        self.mime_type.starts_with("application/vnd.google-apps.") && !self.is_folder()
    }

    /// Where activating this leads. A shortcut resolves to its target, so
    /// opening one behaves like opening the thing it points at.
    pub fn target_id(&self) -> &str {
        self.shortcut
            .as_ref()
            .and_then(|s| s.target_id.as_deref())
            .unwrap_or(&self.id)
    }

    /// Converts to a Neutron entry.
    pub fn to_entry(&self) -> Entry {
        let kind = if self.is_folder() {
            EntryKind::Directory
        } else if self.shortcut.is_some() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };

        Entry {
            name: self.name.clone(),
            kind,
            // Parsed rather than defaulted: a size Drive did not give is 0
            // here, and the list already blanks sizes for containers. Native
            // Google formats are the case where 0 is misleading, which is why
            // `is_native_google_format` exists for the caller to badge.
            size: self.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
            modified: self
                .modified_time
                .as_deref()
                .and_then(parse_rfc3339_millis)
                .unwrap_or(0),
            created: 0,
            attrs: if self.is_folder() { attr::DIRECTORY } else { 0 },
            // Everything in Drive is remote until downloaded, which is exactly
            // what the cloud-only badge means.
            sync: SyncState::CloudOnly,
        }
    }
}

/// Builds the query for one page of a folder's contents.
///
/// The `q` filter excludes trashed files: Drive keeps them listed under their
/// original parent, so without this the bin's contents appear inline with live
/// files and deleting something makes it look like nothing happened.
pub fn list_query(folder_id: &str, page_token: Option<&str>) -> Vec<(String, String)> {
    let mut params = vec![
        (
            "q".to_owned(),
            format!("'{}' in parents and trashed = false", escape_query(folder_id)),
        ),
        ("fields".to_owned(), FIELDS.to_owned()),
        ("pageSize".to_owned(), PAGE_SIZE.to_string()),
        // Folders first, then by name — the same order the local listing uses,
        // so switching between a Drive folder and a real one does not reshuffle
        // under the eye.
        ("orderBy".to_owned(), "folder,name".to_owned()),
        // Shared drives are ordinary folders to the user and invisible without
        // these two.
        ("supportsAllDrives".to_owned(), "true".to_owned()),
        ("includeItemsFromAllDrives".to_owned(), "true".to_owned()),
    ];
    if let Some(token) = page_token {
        params.push(("pageToken".to_owned(), token.to_owned()));
    }
    params
}

/// Escapes a value being interpolated into a Drive `q` expression.
///
/// Folder ids are opaque strings from the API and in practice contain nothing
/// dangerous — but this string is a query language, the id reaches it from
/// outside, and quoting it is one line against a class of bug that is invisible
/// until it is not.
fn escape_query(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Percent-encodes a value being interpolated into a URL *path* segment.
///
/// Drive ids are opaque strings from the API and in practice are alphanumeric
/// with `-` and `_`. This exists for the same reason [`escape_query`] does: the
/// id reaches a structured string from outside, and an id containing `/` or `?`
/// would silently address a different endpoint. Unreserved characters are
/// passed through, everything else is escaped.
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Parses an RFC 3339 timestamp to Unix milliseconds.
///
/// Hand-rolled rather than pulling in a date library: Drive emits exactly one
/// shape (`2026-08-18T09:41:03.123Z`), this is the only place a date is parsed,
/// and the alternative is a dependency tree for one format.
pub fn parse_rfc3339_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }

    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let millis = text
        .split_once('.')
        .and_then(|(_, rest)| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            // Drive sends three, but pad or truncate rather than trusting it.
            let padded = format!("{digits:0<3}");
            padded.get(..3)?.parse::<i64>().ok()
        })
        .unwrap_or(0);

    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000) + millis)
}

/// Days since the Unix epoch, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: &str) -> DriveFile {
        serde_json::from_str(json).expect("parses")
    }

    #[test]
    fn a_folder_maps_to_a_directory() {
        let f = file(r#"{"id":"1a","name":"Photos","mimeType":"application/vnd.google-apps.folder"}"#);
        assert!(f.is_folder());
        let e = f.to_entry();
        assert_eq!(e.kind, EntryKind::Directory);
        assert_eq!(e.attrs & attr::DIRECTORY, attr::DIRECTORY);
    }

    #[test]
    fn sizes_arrive_as_strings_and_survive_being_large() {
        // Drive sends size as a string precisely because a large file exceeds
        // what a JSON number holds exactly. Parsing it as f64 would round.
        let f = file(r#"{"id":"1","name":"big.iso","mimeType":"application/x-iso","size":"9007199254740993"}"#);
        assert_eq!(f.to_entry().size, 9_007_199_254_740_993);
    }

    #[test]
    fn a_missing_size_is_zero_rather_than_a_parse_failure() {
        let f = file(r#"{"id":"1","name":"Notes","mimeType":"application/vnd.google-apps.document"}"#);
        assert_eq!(f.to_entry().size, 0);
        // …and the caller can tell *why* it is zero.
        assert!(f.is_native_google_format());
    }

    #[test]
    fn a_folder_is_not_a_native_google_format() {
        // Both share the `vnd.google-apps.` prefix; treating a folder as a
        // document would badge every folder as un-downloadable.
        let f = file(r#"{"id":"1","name":"F","mimeType":"application/vnd.google-apps.folder"}"#);
        assert!(!f.is_native_google_format());
    }

    #[test]
    fn everything_in_drive_is_cloud_only() {
        let f = file(r#"{"id":"1","name":"a.txt","mimeType":"text/plain","size":"12"}"#);
        assert_eq!(f.to_entry().sync, SyncState::CloudOnly);
    }

    #[test]
    fn a_shortcut_resolves_to_its_target() {
        let f = file(
            r#"{"id":"short1","name":"Link","mimeType":"application/vnd.google-apps.shortcut",
                "shortcutDetails":{"targetId":"real42"}}"#,
        );
        assert_eq!(f.target_id(), "real42");
        assert_eq!(f.to_entry().kind, EntryKind::Symlink);
    }

    #[test]
    fn a_plain_file_targets_itself() {
        let f = file(r#"{"id":"1","name":"a.txt","mimeType":"text/plain"}"#);
        assert_eq!(f.target_id(), "1");
    }

    #[test]
    fn timestamps_parse_to_unix_millis() {
        // 2026-08-18T09:41:03.123Z
        let got = parse_rfc3339_millis("2026-08-18T09:41:03.123Z").expect("parses");
        assert_eq!(got, 1_787_046_063_123);
    }

    #[test]
    fn the_epoch_itself_parses() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn a_timestamp_without_fractional_seconds_still_parses() {
        // The field mask does not guarantee milliseconds, and a `None` here
        // renders as a blank date column on every row.
        assert_eq!(
            parse_rfc3339_millis("2026-08-18T09:41:03Z"),
            Some(1_787_046_063_000)
        );
    }

    #[test]
    fn a_leap_day_parses() {
        // The civil-days arithmetic is the part most likely to be subtly wrong.
        let got = parse_rfc3339_millis("2024-02-29T00:00:00.000Z").expect("parses");
        assert_eq!(got, 1_709_164_800_000);
    }

    #[test]
    fn malformed_timestamps_return_none_rather_than_panicking() {
        for bad in ["", "not a date", "2026-08", "20260818T094103Z", "2026-13-01T00:00:00Z", "2026-08-99T00:00:00Z"] {
            assert_eq!(parse_rfc3339_millis(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn the_listing_query_excludes_trashed_files() {
        // Drive keeps trashed files listed under their original parent, so
        // without this deleting something looks like it did nothing.
        let q = list_query("root", None);
        let filter = &q.iter().find(|(k, _)| k == "q").unwrap().1;
        assert!(filter.contains("trashed = false"), "{filter}");
        assert!(filter.contains("'root' in parents"), "{filter}");
    }

    #[test]
    fn the_query_asks_for_shared_drives() {
        // Without both flags a shared drive is invisible, which to the user
        // looks like their files are missing.
        let q = list_query("root", None);
        let has = |k: &str| q.iter().any(|(key, v)| key == k && v == "true");
        assert!(has("supportsAllDrives"));
        assert!(has("includeItemsFromAllDrives"));
    }

    #[test]
    fn a_page_token_is_only_sent_when_there_is_one() {
        assert!(!list_query("root", None).iter().any(|(k, _)| k == "pageToken"));
        assert!(
            list_query("root", Some("tok"))
                .iter()
                .any(|(k, v)| k == "pageToken" && v == "tok")
        );
    }

    #[test]
    fn quotes_in_an_id_cannot_break_out_of_the_query() {
        let q = list_query("a'b", None);
        let filter = &q.iter().find(|(k, _)| k == "q").unwrap().1;
        assert!(filter.contains("a\\'b"), "{filter}");
    }

    #[test]
    fn a_page_of_results_deserialises() {
        let page: FileList = serde_json::from_str(
            r#"{"nextPageToken":"abc","files":[
                {"id":"1","name":"a","mimeType":"text/plain","size":"3"},
                {"id":"2","name":"b","mimeType":"application/vnd.google-apps.folder"}]}"#,
        )
        .expect("parses");
        assert_eq!(page.files.len(), 2);
        assert_eq!(page.next_page_token.as_deref(), Some("abc"));
    }

    #[test]
    fn an_empty_folder_deserialises() {
        // Drive omits `files` entirely for an empty folder rather than sending
        // an empty array.
        let page: FileList = serde_json::from_str(r#"{}"#).expect("parses");
        assert!(page.files.is_empty());
        assert!(page.next_page_token.is_none());
    }
}

/// Whether a transfer should keep going. Checked between chunks so a download
/// the user has stopped does not run to completion in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Yes,
    Stop,
}

/// How a Drive object's bytes are obtained.
///
/// Ordinary uploads have bytes and are fetched with `alt=media`. A Google Doc,
/// Sheet or Slide **has none** — it is a server-side object, and the only way
/// to get a file out of it is to ask Drive to render one, which means choosing
/// a format the user did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// `files/{id}?alt=media`.
    Bytes,
    /// `files/{id}/export?mimeType=…`, saved with `extension` appended.
    Export { mime: &'static str, extension: &'static str },
    /// A native format with no sensible export — a Form, a Site, a Map.
    /// Refused with a sentence rather than downloading an error page.
    Impossible,
}

/// Chooses how to fetch an object of `mime_type`.
///
/// The export targets are the Office formats rather than PDF: a spreadsheet
/// downloaded as PDF is a picture of a spreadsheet, and the whole reason to
/// pull a Sheet out of Drive is usually to work on it. A Drawing has no Office
/// equivalent and becomes a PNG.
///
/// Note that Drive caps an export at 10MB and answers a larger one with an
/// error, which surfaces as a failed download rather than a truncated file.
pub fn fetch_for(mime_type: &str) -> Fetch {
    match mime_type {
        "application/vnd.google-apps.document" => Fetch::Export {
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            extension: ".docx",
        },
        "application/vnd.google-apps.spreadsheet" => Fetch::Export {
            mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            extension: ".xlsx",
        },
        "application/vnd.google-apps.presentation" => Fetch::Export {
            mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            extension: ".pptx",
        },
        "application/vnd.google-apps.drawing" => Fetch::Export {
            mime: "image/png",
            extension: ".png",
        },
        "application/vnd.google-apps.script" => Fetch::Export {
            mime: "application/vnd.google-apps.script+json",
            extension: ".json",
        },
        // A Form, a Site, a Map, a shortcut that resolved to nothing. Drive
        // exports none of these, and asking produces an error document that
        // would be saved as though it were the file.
        other if other.starts_with("application/vnd.google-apps.") => Fetch::Impossible,
        _ => Fetch::Bytes,
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::*;

    #[test]
    fn an_uploaded_file_is_fetched_as_bytes() {
        assert_eq!(fetch_for("image/png"), Fetch::Bytes);
        assert_eq!(fetch_for("application/pdf"), Fetch::Bytes);
        // Empty is what Drive gives when the field is missing. Treating it as
        // an ordinary file is right: `alt=media` on something with no bytes
        // fails with a message, which is better than refusing a real file.
        assert_eq!(fetch_for(""), Fetch::Bytes);
    }

    #[test]
    fn a_doc_exports_to_an_editable_format_not_a_pdf() {
        let Fetch::Export { mime, extension } =
            fetch_for("application/vnd.google-apps.document")
        else {
            panic!("a Doc must export");
        };
        assert!(mime.contains("wordprocessingml"), "{mime}");
        assert_eq!(extension, ".docx");
    }

    #[test]
    fn a_sheet_exports_to_a_spreadsheet() {
        let Fetch::Export { extension, .. } =
            fetch_for("application/vnd.google-apps.spreadsheet")
        else {
            panic!("a Sheet must export");
        };
        assert_eq!(extension, ".xlsx");
    }

    /// The case that must not silently produce a file: Drive answers an export
    /// request for these with an error document, which a client that does not
    /// check would write out under the file's own name.
    #[test]
    fn a_form_is_refused_rather_than_exported() {
        assert_eq!(fetch_for("application/vnd.google-apps.form"), Fetch::Impossible);
        assert_eq!(fetch_for("application/vnd.google-apps.site"), Fetch::Impossible);
    }

    /// A folder is never downloaded, but `fetch_for` must not claim it is
    /// exportable if it is ever asked.
    #[test]
    fn a_folder_is_not_exportable() {
        assert_eq!(fetch_for(FOLDER_MIME), Fetch::Impossible);
    }
}

#[cfg(test)]
mod urlencode_tests {
    use super::*;

    #[test]
    fn an_ordinary_drive_id_is_unchanged() {
        let id = "1AbC-dEfGh_iJkLmNoPqRsTuVwXyZ0123456789";
        assert_eq!(urlencode(id), id);
    }

    /// The reason this function exists: a value carrying path structure must
    /// not be able to address a different endpoint.
    #[test]
    fn separators_and_query_characters_are_escaped() {
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("a?alt=media"), "a%3Falt%3Dmedia");
        assert_eq!(urlencode("../../files"), "..%2F..%2Ffiles");
    }

    #[test]
    fn non_ascii_is_escaped_per_utf8_byte() {
        assert_eq!(urlencode("é"), "%C3%A9");
    }
}

/// Explains a failed Drive API call.
///
/// The Drive API and the OAuth token endpoint do not share an error shape. A
/// token error is flat — `{"error":"invalid_grant", …}` — while an API error
/// nests: `{"error":{"code":403,"message":"…"}}`. Handing the second to
/// [`crate::flow::describe_token_error`] fails to deserialize and falls through
/// to a bare "HTTP 403", which throws away the one part the user needs. The
/// interesting failures here all live in that message: an export over Drive's
/// 10MB limit, a daily quota, a file whose owner revoked sharing.
pub fn describe_api_error(body: &str, status: u16) -> String {
    #[derive(serde::Deserialize)]
    struct Envelope {
        error: Option<Inner>,
    }
    #[derive(serde::Deserialize)]
    struct Inner {
        message: Option<String>,
    }

    if let Ok(Envelope { error: Some(inner) }) = serde_json::from_str::<Envelope>(body) {
        if let Some(message) = inner.message.filter(|m| !m.trim().is_empty()) {
            return message;
        }
    }
    // Not a Drive-shaped error. It may still be a token-shaped one — an expired
    // credential surfaces on an API call, not only at the token endpoint.
    crate::flow::describe_token_error(body, status)
}

#[cfg(test)]
mod api_error_tests {
    use super::*;

    #[test]
    fn the_message_is_what_comes_back() {
        let body = r#"{"error":{"code":403,"message":"This file is too large to be exported.","errors":[]}}"#;
        assert_eq!(
            describe_api_error(body, 403),
            "This file is too large to be exported."
        );
    }

    #[test]
    fn a_not_found_reads_as_itself() {
        let body = r#"{"error":{"code":404,"message":"File not found: abc123."}}"#;
        assert_eq!(describe_api_error(body, 404), "File not found: abc123.");
    }

    /// An expired credential can surface on an API call rather than at the
    /// token endpoint, and it has the flat shape. It must not be lost.
    #[test]
    fn a_token_shaped_error_still_falls_through() {
        let body = r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        assert!(
            describe_api_error(body, 401).contains("sign in again"),
            "{}",
            describe_api_error(body, 401)
        );
    }

    /// Google sometimes answers with an HTML error page — from a proxy, or a
    /// 502. There is nothing to parse and the status is all there is.
    #[test]
    fn an_unparseable_body_reports_the_status() {
        assert_eq!(describe_api_error("<html>502</html>", 502), "HTTP 502");
        assert_eq!(describe_api_error("", 500), "HTTP 500");
    }

    /// An envelope with no message must not report an empty string, which
    /// would reach the status bar as a blank error.
    #[test]
    fn an_empty_message_falls_back_rather_than_showing_nothing() {
        assert_eq!(describe_api_error(r#"{"error":{"code":500}}"#, 500), "HTTP 500");
        assert_eq!(
            describe_api_error(r#"{"error":{"message":"   "}}"#, 500),
            "HTTP 500"
        );
    }
}
