//! The wire: one JSON request in, newline-delimited JSON responses out.
//!
//! ```text
//! host → plugin (stdin, one line, then EOF)
//!   {"protocol":1,"command":"sha256","folder":"C:\\notes",
//!    "selection":["C:\\notes\\a.txt","C:\\notes\\b.txt"]}
//!
//! plugin → host (stdout, any number of lines)
//!   {"type":"status","text":"3 files hashed"}
//!   {"type":"refresh"}
//! ```
//!
//! # Why newline-delimited JSON
//!
//! It is the format a plugin can produce with `print` in any language, which is
//! the property that decides whether plugins actually get written. A binary
//! framing would be marginally faster to parse and would restrict authorship to
//! whoever is willing to implement it.
//!
//! Everything else follows from being able to write it by hand: one request per
//! run rather than a session, `stdin` closed so a plugin that reads to EOF
//! works, and a tagged response so an unknown `type` from a newer plugin can be
//! ignored by name rather than guessed at.
//!
//! # Why stdout carries responses and stderr does not
//!
//! `stderr` is where a plugin's own diagnostics go — panics, tracebacks, the
//! line a language runtime prints when it cannot find its interpreter. The host
//! logs it and, when a plugin fails, shows the tail of it. Keeping the two
//! streams apart is what makes a broken plugin diagnosable instead of a parse
//! error.

use serde::{Deserialize, Serialize};

pub use crate::manifest::PROTOCOL;

/// What the host sends, as one line on the plugin's standard input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Wire version, so a plugin can refuse a host it does not understand — the
    /// mirror of the host checking the manifest.
    pub protocol: u32,
    /// The `id` of the command the user chose, from the manifest.
    pub command: String,
    /// The folder on screen.
    pub folder: String,
    /// The selected files, as absolute paths. Empty for a
    /// [`Needs::Folder`](crate::manifest::Needs::Folder) command.
    pub selection: Vec<String>,
}

impl Request {
    pub fn new(command: &str, folder: String, selection: Vec<String>) -> Self {
        Request {
            protocol: PROTOCOL,
            command: command.to_owned(),
            folder,
            selection,
        }
    }
}

/// What a plugin writes back, one per line.
///
/// Every variant is something Neutron can already do. That is the rule the list
/// is built on: a response type is a promise, and a plugin that gets an
/// acknowledgement for something the host then does not do is worse than one
/// that got an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Response {
    /// Say something in the status bar. The last one wins.
    Status { text: String },
    /// Show this folder in the pane the command was run from.
    Navigate { path: String },
    /// Hand this path to the shell, as a double-click would.
    ///
    /// This grants a plugin nothing it did not have: it is already a program
    /// running with the user's rights and could launch anything itself. It
    /// exists so the *common* case — "I made a file, show it to them" — does
    /// not require a plugin to link a Windows API.
    Open { path: String },
    /// Re-list the folder, because the command changed what is in it.
    Refresh,
    /// The command failed. Shown to the user as the plugin's own words.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_line_of_json() {
        let r = Request::new("sha256", "C:\\notes".into(), vec!["C:\\notes\\a.txt".into()]);
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains('\n'), "a request must fit one line: {line}");
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), r);
    }

    #[test]
    fn responses_are_tagged_by_type() {
        let cases = [
            (r#"{"type":"status","text":"done"}"#, Response::Status { text: "done".into() }),
            (r#"{"type":"refresh"}"#, Response::Refresh),
            (r#"{"type":"navigate","path":"C:\\x"}"#, Response::Navigate { path: "C:\\x".into() }),
            (r#"{"type":"open","path":"C:\\x"}"#, Response::Open { path: "C:\\x".into() }),
            (r#"{"type":"error","message":"no"}"#, Response::Error { message: "no".into() }),
        ];
        for (line, want) in cases {
            assert_eq!(serde_json::from_str::<Response>(line).unwrap(), want, "{line}");
        }
    }

    #[test]
    fn the_tag_may_come_after_the_fields() {
        // Not a hypothetical: PowerShell's ConvertTo-Json emits hashtable keys
        // in whatever order it likes, and the example plugin produces exactly
        // this. A parser that required the tag first would reject it.
        let line = r#"{"text":"2 files hashed","type":"status"}"#;
        assert_eq!(
            serde_json::from_str::<Response>(line).unwrap(),
            Response::Status { text: "2 files hashed".into() }
        );
    }

    #[test]
    fn an_unknown_response_type_fails_to_parse_rather_than_being_guessed() {
        // The host skips these with a warning. What must not happen is a new
        // type being silently read as an existing one.
        assert!(serde_json::from_str::<Response>(r#"{"type":"reboot"}"#).is_err());
    }

    #[test]
    fn a_response_missing_its_field_is_refused() {
        assert!(serde_json::from_str::<Response>(r#"{"type":"status"}"#).is_err());
    }

    #[test]
    fn the_protocol_number_is_carried_in_every_request() {
        let line = serde_json::to_string(&Request::new("x", "/".into(), vec![])).unwrap();
        assert!(line.contains("\"protocol\":1"), "{line}");
    }
}
