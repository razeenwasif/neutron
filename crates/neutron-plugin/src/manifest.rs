//! `plugin.json`: what a plugin declares about itself.
//!
//! The manifest is the whole of a plugin's public face. Everything Neutron
//! shows before a command is chosen — the titles, where they appear, whether
//! they need a selection — is read from here, because reading a file is
//! something the host can do without running anybody's code.
//!
//! ```json
//! {
//!   "protocol": 1,
//!   "id": "checksums",
//!   "name": "Checksums",
//!   "description": "Hashes for the selected files",
//!   "run": "checksums.exe",
//!   "args": [],
//!   "commands": [
//!     {
//!       "id": "sha256",
//!       "title": "Copy SHA-256 of the selection",
//!       "needs": "selection",
//!       "menu": true
//!     }
//!   ]
//! }
//! ```
//!
//! # Why validation is strict and loud
//!
//! Every rule below rejects the whole manifest and says why, and the reason
//! reaches the user rather than only the log. A plugin system that silently
//! skips what it cannot parse is unusable to write plugins for: the symptom is
//! an entry that never appears, with nothing anywhere to explain it. Failing
//! with a sentence costs a line of code and saves an afternoon.
//!
//! # The program is a file name, not a path
//!
//! `run` must name a file sitting directly beside the manifest. Not a path, not
//! an absolute location, not a name containing `..`. The plugin folder is what
//! the user installed and can inspect; a manifest that could point `run` at
//! `C:\Windows\System32\cmd.exe` — or, more quietly, at
//! `..\..\something.exe` — would mean the thing that runs is not the thing
//! they installed.
//!
//! This overlaps with [`neutron_archive::path::safe_join`] in spirit, and
//! deliberately does not share code with it: that function rebuilds a
//! *multi-component relative path* from a hostile archive, while this one
//! accepts a *single file name* and nothing else. Collapsing them would loosen
//! this rule to the weaker of the two.
//!
//! [`neutron_archive::path::safe_join`]: https://github.com/razeenwasif/neutron

use serde::{Deserialize, Serialize};

use crate::sanitize;

/// The wire and manifest version this build speaks.
pub const PROTOCOL: u32 = 1;

/// Most commands one plugin may declare.
///
/// A plugin with fifty entries has taken over the palette. The cap is a limit
/// on damage from a manifest built by a generator gone wrong, not a design
/// budget.
pub const MAX_COMMANDS: usize = 32;

/// The file inside a plugin folder that describes it.
pub const MANIFEST_NAME: &str = "plugin.json";

/// What a command needs in order to make sense.
///
/// This decides two things: what the request carries, and whether the command
/// is offered at all. A "hash the selection" command with nothing selected is
/// not an error to report — it is an entry that should not have been there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Needs {
    /// One or more selected files. Hidden when the selection is empty.
    Selection,
    /// Only the folder on screen. Always offered.
    Folder,
}

/// One declared command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDef {
    /// Stable identifier, sent back to the plugin as the command to run.
    /// Unique within the plugin.
    pub id: String,
    /// What the user reads, in the palette and the menu.
    pub title: String,
    pub needs: Needs,
    /// Also show this in the right-click menu.
    ///
    /// Defaults to off. The palette is a place people go looking; the context
    /// menu is a place they end up, and it is already long enough that every
    /// installed shell extension makes it worse.
    #[serde(default)]
    pub menu: bool,
    /// How long to wait for this command, in milliseconds. Clamped to
    /// [`MIN_TIMEOUT_MS`]..=[`MAX_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Floor on a declared timeout. Below this, process startup alone would make
/// the command fail intermittently on a busy machine.
pub const MIN_TIMEOUT_MS: u64 = 250;
/// Default when a command declares nothing.
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;
/// Ceiling on a declared timeout. A plugin that wants longer wants a job with
/// progress, which this protocol does not have.
pub const MAX_TIMEOUT_MS: u64 = 120_000;

impl CommandDef {
    pub fn timeout(&self) -> std::time::Duration {
        let ms = self
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        std::time::Duration::from_millis(ms)
    }
}

/// A parsed, validated `plugin.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Wire version the plugin was written against.
    pub protocol: u32,
    /// Slug, unique across installed plugins. Used to name the plugin in logs
    /// and to refer to its commands across a rescan.
    pub id: String,
    /// Shown beside the plugin's commands so two plugins contributing similar
    /// titles can be told apart.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// File name of the program to run, beside this manifest.
    pub run: String,
    /// Fixed arguments, before anything the host adds.
    #[serde(default)]
    pub args: Vec<String>,
    pub commands: Vec<CommandDef>,
}

/// Why a manifest was rejected.
///
/// Every variant renders to a sentence a plugin author can act on, which is the
/// only reason this is an enum rather than a `String`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("written for plugin protocol {found}, but this build speaks {PROTOCOL}")]
    Protocol { found: u32 },
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} may only contain a-z, 0-9, '-', '_' and '.'")]
    NotASlug { field: &'static str },
    #[error("{field} is longer than {max} characters")]
    TooLong { field: &'static str, max: usize },
    #[error("run must be a file name beside plugin.json, not a path: {found:?}")]
    RunIsAPath { found: String },
    #[error("run names a reserved device, not a file: {found:?}")]
    RunIsADevice { found: String },
    #[error("declares no commands")]
    NoCommands,
    #[error("declares more than {MAX_COMMANDS} commands")]
    TooManyCommands,
    #[error("two commands share the id {id:?}")]
    DuplicateCommand { id: String },
}

/// Longest id or name accepted. Both end up in log lines and in the UI.
const MAX_SLUG: usize = 40;

/// Parses and validates a manifest.
///
/// Titles and names are sanitised in place rather than rejected: a stray
/// newline in a description is not worth refusing a working plugin over, and
/// the only thing that matters is that it cannot reach the layout. A title that
/// sanitises to *nothing*, though, is an unreadable menu row, so that is an
/// error.
pub fn parse(text: &str) -> Result<Manifest, String> {
    let mut manifest: Manifest = serde_json::from_str(text).map_err(|e| e.to_string())?;
    validate(&mut manifest).map_err(|e| e.to_string())?;
    Ok(manifest)
}

/// Checks and normalises a deserialized manifest.
pub fn validate(manifest: &mut Manifest) -> Result<(), Invalid> {
    if manifest.protocol != PROTOCOL {
        return Err(Invalid::Protocol { found: manifest.protocol });
    }

    slug("id", &manifest.id)?;

    manifest.name = sanitize::title(&manifest.name);
    if manifest.name.is_empty() {
        return Err(Invalid::Empty { field: "name" });
    }
    manifest.description = sanitize::status(&manifest.description);

    program_name(&manifest.run)?;

    if manifest.commands.is_empty() {
        return Err(Invalid::NoCommands);
    }
    if manifest.commands.len() > MAX_COMMANDS {
        return Err(Invalid::TooManyCommands);
    }

    for i in 0..manifest.commands.len() {
        slug("command id", &manifest.commands[i].id)?;
        manifest.commands[i].title = sanitize::title(&manifest.commands[i].title);
        if manifest.commands[i].title.is_empty() {
            return Err(Invalid::Empty { field: "command title" });
        }
        // Quadratic, over at most 32 entries, once per plugin at startup.
        if let Some(dup) = manifest.commands[..i]
            .iter()
            .find(|c| c.id == manifest.commands[i].id)
        {
            return Err(Invalid::DuplicateCommand { id: dup.id.clone() });
        }
    }

    Ok(())
}

/// An identifier: lowercase, no spaces, safe in a log line and in a file name.
fn slug(field: &'static str, value: &str) -> Result<(), Invalid> {
    if value.is_empty() {
        return Err(Invalid::Empty { field });
    }
    if value.len() > MAX_SLUG {
        return Err(Invalid::TooLong { field, max: MAX_SLUG });
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(Invalid::NotASlug { field });
    }
    Ok(())
}

/// Names Windows resolves to a device however the path is written.
///
/// `run: "CON"` would open the console device rather than a file. These names
/// are reserved with any extension and in any case, so the check is on the stem.
const DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Checks that `run` names a file beside the manifest and nothing else.
///
/// Rejects, in order of how easy each is to miss:
///
/// - separators of either kind, so no subdirectory and no parent
/// - `.` and `..`, which are directories
/// - a colon anywhere, which covers both `C:\...` and the quieter
///   `plugin.exe:hidden` NTFS alternate data stream
/// - a leading `-`, which many programs would read as an option rather than a
///   file if the name were ever passed on to something else
/// - reserved device names
pub fn program_name(run: &str) -> Result<(), Invalid> {
    if run.is_empty() {
        return Err(Invalid::Empty { field: "run" });
    }
    let bad_shape = run.contains('/')
        || run.contains('\\')
        || run.contains(':')
        || run.starts_with('-')
        || run == "."
        || run == ".."
        || run.chars().any(|c| c.is_control());
    if bad_shape {
        return Err(Invalid::RunIsAPath { found: run.to_owned() });
    }

    let stem = run.split('.').next().unwrap_or(run).to_ascii_lowercase();
    if DEVICES.contains(&stem.as_str()) {
        return Err(Invalid::RunIsADevice { found: run.to_owned() });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> String {
        r#"{
            "protocol": 1,
            "id": "checksums",
            "name": "Checksums",
            "run": "checksums.exe",
            "commands": [
                { "id": "sha256", "title": "Copy SHA-256", "needs": "selection", "menu": true }
            ]
        }"#
        .to_owned()
    }

    #[test]
    fn a_well_formed_manifest_parses() {
        let m = parse(&good()).expect("should parse");
        assert_eq!(m.id, "checksums");
        assert_eq!(m.commands.len(), 1);
        assert_eq!(m.commands[0].needs, Needs::Selection);
        assert!(m.commands[0].menu);
    }

    #[test]
    fn optional_fields_may_be_absent() {
        // args, description, menu and timeout_ms all have defaults, because a
        // manifest for a one-command plugin should be four lines long.
        let m = parse(
            r#"{"protocol":1,"id":"a","name":"A","run":"a.exe",
                "commands":[{"id":"x","title":"X","needs":"folder"}]}"#,
        )
        .expect("should parse");
        assert!(m.args.is_empty());
        assert!(m.description.is_empty());
        assert!(!m.commands[0].menu);
        assert_eq!(m.commands[0].timeout(), std::time::Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn a_future_protocol_is_refused_by_name() {
        // The point of the version is that a plugin written for a later
        // Neutron fails with an explanation instead of half-working.
        let text = good().replace("\"protocol\": 1", "\"protocol\": 7");
        let err = parse(&text).expect_err("should refuse");
        assert!(err.contains('7'), "{err}");
    }

    #[test]
    fn run_may_not_escape_the_plugin_folder() {
        for attempt in [
            "..\\..\\Windows\\System32\\cmd.exe",
            "../../bin/sh",
            "C:\\Windows\\System32\\cmd.exe",
            "sub\\prog.exe",
            "..",
            ".",
        ] {
            assert!(
                program_name(attempt).is_err(),
                "{attempt:?} was accepted as a program name"
            );
        }
    }

    #[test]
    fn run_may_not_name_an_alternate_data_stream() {
        // `plugin.exe:payload` is a file name to a careless check and a
        // different file entirely to Windows.
        assert!(program_name("plugin.exe:payload").is_err());
    }

    #[test]
    fn run_may_not_name_a_device() {
        assert!(program_name("CON").is_err());
        assert!(program_name("nul.exe").is_err());
        assert!(program_name("com1").is_err());
    }

    #[test]
    fn an_ordinary_program_name_is_accepted() {
        for ok in ["checksums.exe", "run.cmd", "a-b_c.1.exe", "plugin"] {
            assert!(program_name(ok).is_ok(), "{ok:?} was refused");
        }
    }

    #[test]
    fn ids_must_be_slugs() {
        let text = good().replace("\"checksums\"", "\"Check Sums\"");
        assert!(parse(&text).is_err());
    }

    #[test]
    fn a_command_title_is_sanitised_rather_than_refused() {
        let text = good().replace("Copy SHA-256", "Copy\\nSHA-256");
        let m = parse(&text).expect("a newline in a title is not fatal");
        assert_eq!(m.commands[0].title, "Copy SHA-256");
    }

    #[test]
    fn a_title_that_sanitises_to_nothing_is_refused() {
        // An empty row in the menu is unclickable and unexplainable.
        let text = good().replace("Copy SHA-256", " \\t ");
        assert!(parse(&text).is_err());
    }

    #[test]
    fn duplicate_command_ids_are_refused() {
        let text = r#"{"protocol":1,"id":"a","name":"A","run":"a.exe","commands":[
            {"id":"x","title":"One","needs":"folder"},
            {"id":"x","title":"Two","needs":"folder"}]}"#;
        let err = parse(text).expect_err("should refuse");
        assert!(err.contains("x"), "{err}");
    }

    #[test]
    fn a_manifest_with_no_commands_is_refused() {
        let text = r#"{"protocol":1,"id":"a","name":"A","run":"a.exe","commands":[]}"#;
        assert!(parse(text).is_err());
    }

    #[test]
    fn too_many_commands_is_refused() {
        let commands: Vec<String> = (0..MAX_COMMANDS + 1)
            .map(|i| format!(r#"{{"id":"c{i}","title":"C{i}","needs":"folder"}}"#))
            .collect();
        let text = format!(
            r#"{{"protocol":1,"id":"a","name":"A","run":"a.exe","commands":[{}]}}"#,
            commands.join(",")
        );
        assert!(parse(&text).is_err());
    }

    #[test]
    fn a_declared_timeout_is_clamped_both_ways() {
        let mut c = CommandDef {
            id: "x".into(),
            title: "X".into(),
            needs: Needs::Folder,
            menu: false,
            timeout_ms: Some(0),
        };
        assert_eq!(c.timeout(), std::time::Duration::from_millis(MIN_TIMEOUT_MS));
        c.timeout_ms = Some(u64::MAX);
        assert_eq!(c.timeout(), std::time::Duration::from_millis(MAX_TIMEOUT_MS));
    }

    #[test]
    fn an_unknown_needs_value_is_refused() {
        // Rather than defaulting to something, because "folder" and
        // "selection" differ in whether the command is offered at all.
        let text = good().replace("\"selection\"", "\"everything\"");
        assert!(parse(&text).is_err());
    }
}
