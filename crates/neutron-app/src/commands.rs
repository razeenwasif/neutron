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
    DeleteSelection,

    ToggleHidden,
    ToggleView,
    ToggleTheme,

    FindInFolder,
    SearchEverything,
    StopIndexer,

    GoHome,
    GoDesktop,
    GoDownloads,
    GoDocuments,
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
    CommandDef { id: CommandId::ToggleTheme, title: "Switch light / dark", hint: "Ctrl+D" },

    CommandDef { id: CommandId::FindInFolder, title: "Find file in this folder", hint: "Ctrl+P" },
    CommandDef {
        id: CommandId::SearchEverything,
        title: "Search every volume",
        hint: "Ctrl+Shift+F",
    },
    // The only way to stop the elevated helper from inside the application.
    // Without it the process outlives every window and has to be killed from
    // Task Manager — which is exactly what happened during development.
    CommandDef {
        id: CommandId::StopIndexer,
        title: "Stop the search indexer",
        hint: "",
    },
];

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
