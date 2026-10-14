//! What the Mac's menu bar holds: File, Edit, View, Go and Window, laid out
//! as the Finder lays them out, with the shortcuts the keyboard handler also
//! answers.
//!
//! Only the description is here. Building the menus is
//! `neutron_shell::menubar`; what each item does, and when it is enabled, is
//! decided in `app.rs`, which knows the state.
//!
//! Shortcuts that need Shift with punctuation — Command-Shift-] for the next
//! tab, Command-Shift-. for hidden files — are left off the items: AppKit
//! matches them by the shifted character, which differs between keyboard
//! layouts, and the keyboard handler already answers them reliably.

use neutron_shell::menubar::{BACKSPACE, Entry, Key, Menu, UP};

/// A menu item, by what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Cmd {
    NewWindow = 1,
    NewTab,
    NewFolder,
    Open,
    CloseTab,
    GetInfo,
    Rename,
    Duplicate,
    MakeAlias,
    QuickLook,
    Compress,
    Extract,
    MoveToTrash,
    DeleteImmediately,
    Eject,
    AddToSidebar,

    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    CopyPath,
    Filter,
    SearchEverywhere,
    FindFile,
    CommandPalette,

    AsGrid,
    AsList,
    AsColumns,
    SortName,
    SortKind,
    SortModified,
    SortCreated,
    SortSize,
    ShowPreview,
    ShowHidden,
    SplitRight,
    SplitDown,
    AddressBar,
    ToggleTheme,

    Back,
    Forward,
    Enclosing,
    Dashboard,
    Home,
    Desktop,
    Documents,
    Downloads,
    Applications,
    Trash,
    GoToFolder,
    Connect,
    AirDrop,
    Terminal,

    NextTab,
    PreviousTab,
    NextPane,
}

impl Cmd {
    const ALL: &'static [Cmd] = &[
        Cmd::NewWindow, Cmd::NewTab, Cmd::NewFolder, Cmd::Open, Cmd::CloseTab, Cmd::GetInfo,
        Cmd::Rename, Cmd::Duplicate, Cmd::MakeAlias, Cmd::QuickLook, Cmd::Compress, Cmd::Extract,
        Cmd::MoveToTrash, Cmd::DeleteImmediately, Cmd::Eject, Cmd::AddToSidebar, Cmd::Undo,
        Cmd::Redo, Cmd::Cut, Cmd::Copy, Cmd::Paste, Cmd::SelectAll, Cmd::CopyPath, Cmd::Filter,
        Cmd::SearchEverywhere, Cmd::FindFile, Cmd::CommandPalette, Cmd::AsGrid, Cmd::AsList,
        Cmd::AsColumns, Cmd::SortName, Cmd::SortKind, Cmd::SortModified, Cmd::SortCreated,
        Cmd::SortSize, Cmd::ShowPreview, Cmd::ShowHidden, Cmd::SplitRight, Cmd::SplitDown,
        Cmd::AddressBar, Cmd::ToggleTheme, Cmd::Back, Cmd::Forward, Cmd::Enclosing,
        Cmd::Dashboard, Cmd::Home, Cmd::Desktop, Cmd::Documents, Cmd::Downloads,
        Cmd::Applications, Cmd::Trash, Cmd::GoToFolder, Cmd::Connect, Cmd::AirDrop,
        Cmd::Terminal, Cmd::NextTab, Cmd::PreviousTab, Cmd::NextPane,
    ];

    pub fn from_tag(tag: u32) -> Option<Cmd> {
        Cmd::ALL.iter().copied().find(|c| *c as u32 == tag)
    }

    pub fn tag(self) -> u32 {
        self as u32
    }

    /// Items whose shortcuts a text field uses. Disabled while one has focus,
    /// so the keys reach the field; see `neutron_shell::menubar`.
    pub fn edits_text(self) -> bool {
        matches!(
            self,
            Cmd::Undo
                | Cmd::Redo
                | Cmd::Cut
                | Cmd::Copy
                | Cmd::Paste
                | Cmd::SelectAll
                | Cmd::CopyPath
                | Cmd::MoveToTrash
                | Cmd::DeleteImmediately
                | Cmd::Enclosing
        )
    }

    /// Every item, for disabling the lot while a dialog is open.
    pub fn all() -> impl Iterator<Item = Cmd> {
        Cmd::ALL.iter().copied()
    }
}

fn item(cmd: Cmd, title: &str, key: Option<Key>) -> Entry {
    Entry::Item { tag: cmd.tag(), title: title.to_owned(), key }
}

fn cmd(key: &'static str) -> Option<Key> {
    Some(Key::cmd(key))
}

/// The menus, in the order they sit in the bar after the application menu.
pub fn menus() -> Vec<Menu> {
    use Entry::Separator;
    let file = vec![
        item(Cmd::NewWindow, "New Window", cmd("n")),
        item(Cmd::NewFolder, "New Folder", Some(Key::cmd("n").shift())),
        item(Cmd::NewTab, "New Tab", cmd("t")),
        item(Cmd::Open, "Open", cmd("o")),
        item(Cmd::CloseTab, "Close Tab", cmd("w")),
        Separator,
        item(Cmd::GetInfo, "Get Info", cmd("i")),
        item(Cmd::Rename, "Rename", None),
        item(Cmd::Duplicate, "Duplicate", cmd("d")),
        item(Cmd::MakeAlias, "Make Alias", None),
        item(Cmd::QuickLook, "Quick Look", cmd("y")),
        Separator,
        item(Cmd::Compress, "Compress", None),
        item(Cmd::Extract, "Extract", None),
        Separator,
        item(Cmd::MoveToTrash, "Move to Trash", cmd(BACKSPACE)),
        item(Cmd::DeleteImmediately, "Delete Immediately…", Some(Key::cmd(BACKSPACE).option())),
        Separator,
        item(Cmd::Eject, "Eject", cmd("e")),
        item(Cmd::AddToSidebar, "Add to Sidebar", Some(Key::cmd("t").control())),
    ];
    let edit = vec![
        item(Cmd::Undo, "Undo", cmd("z")),
        item(Cmd::Redo, "Redo", Some(Key::cmd("z").shift())),
        Separator,
        item(Cmd::Cut, "Cut", cmd("x")),
        item(Cmd::Copy, "Copy", cmd("c")),
        item(Cmd::Paste, "Paste", cmd("v")),
        item(Cmd::SelectAll, "Select All", cmd("a")),
        item(Cmd::CopyPath, "Copy Path", Some(Key::cmd("c").option())),
        Separator,
        item(Cmd::Filter, "Filter This Folder", cmd("f")),
        item(Cmd::SearchEverywhere, "Search Everywhere", Some(Key::cmd("f").shift())),
        item(Cmd::FindFile, "Find a File", cmd("p")),
        item(Cmd::CommandPalette, "Command Palette", Some(Key::cmd("p").shift())),
    ];
    let sort = vec![
        item(Cmd::SortName, "Name", None),
        item(Cmd::SortKind, "Kind", None),
        item(Cmd::SortModified, "Date Modified", None),
        item(Cmd::SortCreated, "Date Created", None),
        item(Cmd::SortSize, "Size", None),
    ];
    let view = vec![
        item(Cmd::AsGrid, "as Grid", cmd("1")),
        item(Cmd::AsList, "as List", cmd("2")),
        item(Cmd::AsColumns, "as Columns", cmd("3")),
        Separator,
        Entry::Submenu { title: "Sort By".into(), entries: sort },
        Separator,
        item(Cmd::ShowPreview, "Show Preview", None),
        item(Cmd::ShowHidden, "Show Hidden Files", None),
        Separator,
        item(Cmd::SplitRight, "Split Right", cmd("\\")),
        item(Cmd::SplitDown, "Split Down", None),
        item(Cmd::AddressBar, "Show Address Bar", cmd("l")),
        Separator,
        item(Cmd::ToggleTheme, "Switch Light / Dark", None),
    ];
    let go = vec![
        item(Cmd::Back, "Back", cmd("[")),
        item(Cmd::Forward, "Forward", cmd("]")),
        item(Cmd::Enclosing, "Enclosing Folder", cmd(UP)),
        Separator,
        item(Cmd::Dashboard, "Dashboard", None),
        item(Cmd::Home, "Home", Some(Key::cmd("h").shift())),
        item(Cmd::Desktop, "Desktop", Some(Key::cmd("d").shift())),
        item(Cmd::Documents, "Documents", Some(Key::cmd("o").shift())),
        item(Cmd::Downloads, "Downloads", Some(Key::cmd("l").option())),
        item(Cmd::Applications, "Applications", Some(Key::cmd("a").shift())),
        item(Cmd::Trash, "Trash", None),
        item(Cmd::AirDrop, "AirDrop", Some(Key::cmd("r").shift())),
        Separator,
        item(Cmd::GoToFolder, "Go to Folder…", Some(Key::cmd("g").shift())),
        item(Cmd::Connect, "Connect to Server…", cmd("k")),
        item(Cmd::Terminal, "Open in Terminal", None),
    ];
    let window = vec![
        Entry::System { title: "Minimize".into(), action: c"performMiniaturize:", key: cmd("m") },
        Entry::System { title: "Zoom".into(), action: c"performZoom:", key: None },
        Separator,
        item(Cmd::NextTab, "Show Next Tab", None),
        item(Cmd::PreviousTab, "Show Previous Tab", None),
        item(Cmd::NextPane, "Focus Next Pane", None),
        Separator,
        Entry::System { title: "Bring All to Front".into(), action: c"arrangeInFront:", key: None },
    ];
    vec![
        Menu { title: "File".into(), entries: file, windows: false },
        Menu { title: "Edit".into(), entries: edit, windows: false },
        Menu { title: "View".into(), entries: view, windows: false },
        Menu { title: "Go".into(), entries: go, windows: false },
        Menu { title: "Window".into(), entries: window, windows: true },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_distinct_tag_that_maps_back() {
        let mut seen = std::collections::HashSet::new();
        for cmd in Cmd::all() {
            assert!(seen.insert(cmd.tag()), "{cmd:?} shares a tag");
            assert_eq!(Cmd::from_tag(cmd.tag()), Some(cmd));
        }
        assert_eq!(Cmd::from_tag(0), None);
    }

    #[test]
    fn every_command_is_in_a_menu_once() {
        fn tags(entries: &[Entry], out: &mut Vec<u32>) {
            for entry in entries {
                match entry {
                    Entry::Item { tag, .. } => out.push(*tag),
                    Entry::Submenu { entries, .. } => tags(entries, out),
                    _ => {}
                }
            }
        }
        let mut found = Vec::new();
        for menu in menus() {
            tags(&menu.entries, &mut found);
        }
        for cmd in Cmd::all() {
            assert_eq!(found.iter().filter(|t| **t == cmd.tag()).count(), 1, "{cmd:?}");
        }
    }
}
