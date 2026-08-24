//! The command palette's catalogue.
//!
//! Every entry here is something the application can already do. A palette that
//! lists commands which do nothing is worse than no palette: it is the one
//! surface where a user goes looking for a capability, so an entry is a promise.
//!
//! # Why a table rather than a trait
//!
//! Commands are data — a title, a hint, and an identifier the app maps to an
//! [`crate::app::Action`]. Making each one a type would spread the list across
//! the codebase and make "what can this application do?" unanswerable by
//! reading one screen.

/// What a palette entry does. Mapped to an action by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    NewTab,
    CloseTab,
    SplitRight,
    SplitDown,
    FocusNextPane,

    GoUp,
    GoBack,
    GoForward,
    Refresh,

    SelectAll,
    ClearSelection,
    Rename,
    NewFolder,
    Extract,
    Compress,
    Copy,
    Cut,
    Paste,
    DeleteSelection,

    ToggleHidden,
    ToggleView,
    TogglePreview,
    ToggleTheme,

    FindInFolder,
    SearchEverything,
    RefreshIndex,
    StopIndexer,

    GoHome,
    GoDesktop,
    GoDownloads,
    GoDocuments,

    ReloadPlugins,
    OpenPluginsFolder,

    ConnectServer,
    ReloadSites,
}

pub struct CommandDef {
    pub id: CommandId,
    /// Imperative, and named for what the user recognises rather than for the
    /// code that runs — "Show hidden files", not "Toggle hidden attribute".
    pub title: &'static str,
    /// The keystroke, where one exists. The palette is also how people *learn*
    /// the shortcuts, so showing them is half the point.
    pub hint: &'static str,
}

/// Every command, in the order the palette shows them with an empty prompt.
///
/// Grouped by what they act on — pane, then location, then selection, then
/// view, then search — because with no query typed the list is read rather than
/// filtered, and alphabetical order would scatter related commands.
pub const ALL: &[CommandDef] = &[
    CommandDef { id: CommandId::NewTab, title: "New tab here", hint: "Ctrl+T" },
    CommandDef { id: CommandId::CloseTab, title: "Close tab", hint: "Ctrl+W" },
    CommandDef { id: CommandId::SplitRight, title: "Split pane right", hint: "Ctrl+\\" },
    CommandDef { id: CommandId::SplitDown, title: "Split pane down", hint: "Ctrl+Shift+\\" },
    CommandDef { id: CommandId::FocusNextPane, title: "Focus next pane", hint: "F6" },

    CommandDef { id: CommandId::GoUp, title: "Go up one folder", hint: "Backspace" },
    CommandDef { id: CommandId::GoBack, title: "Go back", hint: "Alt+Left" },
    CommandDef { id: CommandId::GoForward, title: "Go forward", hint: "Alt+Right" },
    CommandDef { id: CommandId::Refresh, title: "Refresh this folder", hint: "F5" },

    CommandDef { id: CommandId::GoHome, title: "Go to Home", hint: "" },
    CommandDef { id: CommandId::GoDesktop, title: "Go to Desktop", hint: "" },
    CommandDef { id: CommandId::GoDownloads, title: "Go to Downloads", hint: "" },
    CommandDef { id: CommandId::GoDocuments, title: "Go to Documents", hint: "" },

    CommandDef { id: CommandId::SelectAll, title: "Select all", hint: "Ctrl+A" },
    CommandDef { id: CommandId::ClearSelection, title: "Clear selection", hint: "Esc" },
    CommandDef { id: CommandId::Copy, title: "Copy to clipboard", hint: "Ctrl+C" },
    CommandDef { id: CommandId::Cut, title: "Cut to clipboard", hint: "Ctrl+X" },
    CommandDef { id: CommandId::Paste, title: "Paste here", hint: "Ctrl+V" },
    CommandDef { id: CommandId::Rename, title: "Rename this file", hint: "F2" },
    CommandDef { id: CommandId::NewFolder, title: "New folder", hint: "Ctrl+Shift+N" },
    CommandDef { id: CommandId::Extract, title: "Extract this archive", hint: "" },
    CommandDef { id: CommandId::Compress, title: "Compress to a zip", hint: "" },
    CommandDef {
        id: CommandId::DeleteSelection,
        title: "Delete to Recycle Bin",
        hint: "Delete",
    },

    CommandDef { id: CommandId::ToggleHidden, title: "Show hidden files", hint: "Ctrl+H" },
    CommandDef {
        id: CommandId::ToggleView,
        title: "Switch list / grid view",
        hint: "Ctrl+Shift+L",
    },
    CommandDef {
        id: CommandId::TogglePreview,
        title: "Show the preview pane",
        hint: "Alt+P",
    },
    CommandDef { id: CommandId::ToggleTheme, title: "Switch light / dark", hint: "Ctrl+D" },

    CommandDef { id: CommandId::FindInFolder, title: "Find file in this folder", hint: "Ctrl+P" },
    CommandDef {
        id: CommandId::SearchEverything,
        title: "Search every volume",
        hint: "Ctrl+Shift+F",
    },
    // Searching normally reuses the index the last elevated run wrote, which
    // costs no prompt. This is how a user asks for one that includes what has
    // changed since.
    CommandDef {
        id: CommandId::RefreshIndex,
        title: "Rebuild the search index",
        hint: "",
    },
    // The only way to stop the elevated helper from inside the application.
    // Without it the process outlives every window and has to be killed from
    // Task Manager — which is exactly what happened during development.
    CommandDef {
        id: CommandId::StopIndexer,
        title: "Stop the search indexer",
        hint: "",
    },

    // Plugin housekeeping. Both exist for the same reason: a plugin that does
    // not appear gives the user nothing to look at, and these are the two
    // things they would want to do about it.
    CommandDef {
        id: CommandId::OpenPluginsFolder,
        title: "Open the plugins folder",
        hint: "",
    },
    CommandDef {
        id: CommandId::ReloadPlugins,
        title: "Reload plugins",
        hint: "",
    },

    // Saved servers. Adding one has to be a dialog rather than a file, because
    // the password belongs in Credential Manager and something has to accept it
    // as typed input.
    CommandDef {
        id: CommandId::ConnectServer,
        title: "Connect to a server",
        hint: "",
    },
    CommandDef {
        id: CommandId::ReloadSites,
        title: "Reload saved servers",
        hint: "",
    },
];

/// Something the palette can run: one of Neutron's own commands, or one a
/// plugin declared.
///
/// Rows are chosen by position in the matched list and resolved through this,
/// rather than by matching the title back against a table. Titles are no longer
/// unique once plugins can contribute them — two plugins may reasonably both
/// offer "Copy hash" — and a lookup by title would run whichever came first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invoke {
    Builtin(CommandId),
    Plugin(crate::plugins::Ref),
}

/// One palette row, from either source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub invoke: Invoke,
    pub title: String,
    /// The shortcut for a built-in; the plugin's name for a plugin command, so
    /// it is always visible whose command it is.
    pub hint: String,
}

/// The palette's full list: everything built in, then everything installed.
///
/// Built-ins first because they are what a user expects to find, and their
/// order is deliberate. Plugin entries keep the order [`crate::plugins`]
/// produced, which is by plugin id and then by manifest.
pub fn catalogue(plugins: &[(crate::plugins::Ref, String, String)]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = ALL
        .iter()
        .map(|c| Entry {
            invoke: Invoke::Builtin(c.id),
            title: c.title.to_owned(),
            hint: c.hint.to_owned(),
        })
        .collect();

    entries.extend(plugins.iter().map(|(reference, title, plugin_name)| Entry {
        invoke: Invoke::Plugin(reference.clone()),
        title: title.clone(),
        hint: plugin_name.clone(),
    }));

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_title() {
        for command in ALL {
            assert!(!command.title.is_empty(), "{:?} has no title", command.id);
        }
    }

    #[test]
    fn command_ids_are_unique() {
        // A duplicate would make the palette run whichever entry sorted first,
        // which is silent and maddening to diagnose.
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.id, b.id, "duplicate command: {:?}", a.id);
            }
        }
    }

    #[test]
    fn titles_are_distinct() {
        // Two entries reading the same is indistinguishable to the user even
        // when the ids differ.
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.title, b.title, "duplicate title: {}", a.title);
            }
        }
    }

    #[test]
    fn the_catalogue_starts_with_the_builtins_and_appends_plugins() {
        let plugins = vec![(
            crate::plugins::Ref { plugin: "hash".into(), command: "sha256".into() },
            "Copy SHA-256".to_owned(),
            "Checksums".to_owned(),
        )];
        let entries = catalogue(&plugins);

        assert_eq!(entries.len(), ALL.len() + 1);
        assert_eq!(entries[0].invoke, Invoke::Builtin(ALL[0].id));
        let last = entries.last().unwrap();
        assert_eq!(last.title, "Copy SHA-256");
        // The plugin's name occupies the hint column, where a built-in shows
        // its shortcut — so a plugin row always says where it came from.
        assert_eq!(last.hint, "Checksums");
    }

    #[test]
    fn two_plugins_may_offer_the_same_title_without_becoming_one_entry() {
        // The reason rows resolve by position rather than by title.
        let plugins = vec![
            (
                crate::plugins::Ref { plugin: "a".into(), command: "go".into() },
                "Copy hash".to_owned(),
                "A".to_owned(),
            ),
            (
                crate::plugins::Ref { plugin: "b".into(), command: "go".into() },
                "Copy hash".to_owned(),
                "B".to_owned(),
            ),
        ];
        let entries = catalogue(&plugins);
        assert_eq!(entries.len(), ALL.len() + 2);
        assert_ne!(entries[ALL.len()].invoke, entries[ALL.len() + 1].invoke);
    }

    #[test]
    fn titles_start_with_a_verb_in_the_imperative() {
        // A palette entry is an instruction, and consistency is what lets the
        // list be scanned rather than read.
        for command in ALL {
            let first = command.title.split(' ').next().unwrap_or("");
            assert!(
                first.chars().next().is_some_and(|c| c.is_uppercase()),
                "{:?}: {:?} does not start with a capitalised verb",
                command.id,
                command.title
            );
            assert!(
                !command.title.ends_with('.'),
                "{:?}: titles are labels, not sentences",
                command.id
            );
        }
    }
}
