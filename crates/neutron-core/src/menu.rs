//! A shell context menu as plain data.
//!
//! Neutron draws its own context menu rather than letting the shell draw one.
//! The commands still come from the shell — 7-Zip, Git, "Open in Terminal" are
//! the reason a file manager is usable — but a native `TrackPopupMenuEx` popup
//! is drawn by the system in the system's colours, and a stark grey Win32 menu
//! on top of a glass panel is the single most jarring thing in the window.
//!
//! So the shell builds its `HMENU` as usual, we *read* it into this type, and
//! the UI renders it in the palette. Choosing an item sends its id back to the
//! same apartment thread, which invokes it through `IContextMenu` exactly as
//! before.
//!
//! This type lives in `neutron-core` because it is the seam: `neutron-shell`
//! produces it and `neutron-ui` consumes it, and neither may depend on the
//! other. Being pure data it is also testable on Linux.

/// One row of a context menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuItem {
    /// The shell command id, used to invoke the item. Zero for separators and
    /// for submenu parents, which are not themselves commands.
    pub id: u32,
    /// What the user reads. Win32 accelerator markers are already stripped.
    pub label: String,
    /// The shortcut column, where the shell supplied one — the text after the
    /// tab character in a menu string.
    pub accel: String,
    pub enabled: bool,
    /// Rendered emphasised. This is the item a double-click would run, and
    /// showing it is how the menu tells you what "just opening" this file does.
    pub default: bool,
    pub checked: bool,
    pub separator: bool,
    /// Non-empty for a submenu.
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn separator() -> Self {
        MenuItem { separator: true, ..Default::default() }
    }

    pub fn is_submenu(&self) -> bool {
        !self.children.is_empty()
    }

    /// Whether this row can be chosen. Separators and disabled items cannot;
    /// submenus can be *opened* but not invoked.
    pub fn selectable(&self) -> bool {
        !self.separator && self.enabled
    }
}

/// Removes the Win32 accelerator markers from a menu string and splits off the
/// shortcut column.
///
/// Menu strings carry two pieces of formatting the shell expects the system
/// menu renderer to interpret: `&` before the underlined access key (and `&&`
/// for a literal ampersand), and a tab separating the label from its keyboard
/// shortcut. Drawn verbatim they come out as `E&xtract files...` and
/// `Rename\tF2`.
pub fn parse_label(raw: &str) -> (String, String) {
    let (text, accel) = match raw.split_once('\t') {
        Some((t, a)) => (t, a.trim()),
        None => (raw, ""),
    };

    let mut label = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // `&&` is an escaped literal; a lone `&` marks the next character
            // as the access key and is not itself drawn.
            if chars.peek() == Some(&'&') {
                chars.next();
                label.push('&');
            }
        } else {
            label.push(c);
        }
    }

    (label.trim_end().to_owned(), accel.to_owned())
}

/// Drops separators that cannot be seen: leading, trailing, and runs.
///
/// The shell's menu is assembled by many independent handlers, each adding its
/// own trailing separator. Whichever ones end up adjacent leave a double rule,
/// and a handler that contributes nothing on this selection leaves a rule with
/// nothing under it.
pub fn tidy(items: Vec<MenuItem>) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::with_capacity(items.len());
    for mut item in items {
        if item.separator {
            if out.last().is_none_or(|prev| prev.separator) {
                continue;
            }
        } else {
            item.children = tidy(std::mem::take(&mut item.children));
        }
        out.push(item);
    }
    while out.last().is_some_and(|i| i.separator) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(label: &str) -> MenuItem {
        MenuItem { id: 1, label: label.to_owned(), enabled: true, ..Default::default() }
    }

    #[test]
    fn accelerator_markers_are_stripped() {
        assert_eq!(parse_label("E&xtract files...").0, "Extract files...");
    }

    #[test]
    fn a_doubled_ampersand_is_a_literal_one() {
        // "Search && Replace" is a real menu label; drawn naively it loses the
        // ampersand entirely, because the first & eats the second.
        assert_eq!(parse_label("Search && Replace").0, "Search & Replace");
    }

    #[test]
    fn the_shortcut_column_is_separated_from_the_label() {
        let (label, accel) = parse_label("&Rename\tF2");
        assert_eq!(label, "Rename");
        assert_eq!(accel, "F2");
    }

    #[test]
    fn a_label_without_a_shortcut_has_an_empty_one() {
        assert_eq!(parse_label("Properties"), ("Properties".to_owned(), String::new()));
    }

    #[test]
    fn runs_of_separators_collapse_to_one() {
        let items = vec![cmd("Open"), MenuItem::separator(), MenuItem::separator(), cmd("Copy")];
        let out = tidy(items);
        assert_eq!(out.len(), 3);
        assert!(out[1].separator);
    }

    #[test]
    fn separators_at_the_edges_are_dropped() {
        // A handler that contributed no commands for this selection still
        // added its trailing rule.
        let items = vec![MenuItem::separator(), cmd("Open"), MenuItem::separator()];
        let out = tidy(items);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "Open");
    }

    #[test]
    fn a_menu_of_only_separators_tidies_to_nothing() {
        assert!(tidy(vec![MenuItem::separator(), MenuItem::separator()]).is_empty());
    }

    #[test]
    fn tidying_reaches_into_submenus() {
        let mut parent = cmd("Send to");
        parent.children = vec![MenuItem::separator(), cmd("Desktop"), MenuItem::separator()];
        let out = tidy(vec![parent]);
        assert_eq!(out[0].children.len(), 1);
    }

    #[test]
    fn a_separator_is_never_selectable() {
        assert!(!MenuItem::separator().selectable());
        assert!(cmd("Open").selectable());
    }

    #[test]
    fn a_disabled_command_is_not_selectable() {
        let mut item = cmd("Paste");
        item.enabled = false;
        assert!(!item.selectable());
    }
}
