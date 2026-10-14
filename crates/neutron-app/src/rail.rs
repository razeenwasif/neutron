//! The action rail: a fixed vertical strip of the verbs that act on a
//! selection.
//!
//! # Why a rail, in a keyboard-first application
//!
//! Every verb here already has a keystroke and a command-palette entry, and the
//! project's pitch lists "mouse-centric" as *Explorer's* problem. The rail is
//! here anyway, deliberately: not every user is keyboard-driven, and a palette
//! only helps someone who already knows the command exists. A visible row of
//! verbs is also how you find out what a file manager can do — a shortcut you
//! have never been told about is a feature you do not have.
//!
//! It is a strip beside the sidebar rather than a toolbar across the top for
//! the same reason navigation lives inside each pane: with split panes there is
//! no single current location, but there *is* a single focused selection, and
//! the rail acts on that.
//!
//! Enablement comes from the focused pane's capabilities. Disabled buttons
//! explain what is missing, and active buttons dispatch the same commands as
//! the palette. The rail scrolls when the window is short.

use egui::{Rect, Sense, Ui, pos2, vec2};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette};

/// Width of the strip, including its padding.
pub const WIDTH: f32 = 76.0;

/// Height of one button: glyph, then its label, with room to breathe.
const ITEM: f32 = 68.0;

/// Space between buttons within a group.
const SPACING: f32 = 2.0;

/// What the rail is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailAction {
    Copy,
    Cut,
    Paste,
    Delete,
    Rename,
    NewFolder,
    Extract,
    Compress,
}

/// A filesystem-free snapshot of the focused pane.
#[derive(Default)]
pub struct RailState {
    pub local_folder: bool,
    /// A folder on a saved server: uploads, new folders, renames and deletes
    /// work there; copying off it and archives do not yet.
    pub server_folder: bool,
    /// A folder inside an archive: read-only, but what is in it can be
    /// copied out.
    pub archive_folder: bool,
    pub selection_count: usize,
    pub has_cursor: bool,
    pub selected_archive: bool,
    pub archive_busy: bool,
}

impl RailAction {
    pub fn command(self) -> crate::commands::CommandId {
        use crate::commands::CommandId as C;
        match self {
            Self::Copy => C::Copy,
            Self::Cut => C::Cut,
            Self::Paste => C::Paste,
            Self::Rename => C::Rename,
            Self::Delete => C::DeleteSelection,
            Self::NewFolder => C::NewFolder,
            Self::Extract => C::Extract,
            Self::Compress => C::Compress,
        }
    }
    fn unavailable(self, state: &RailState) -> Option<&'static str> {
        if state.archive_folder {
            return match self {
                Self::Copy if state.selection_count == 0 => Some("Select files or folders first"),
                Self::Copy => None,
                _ => Some("An archive is read-only — copy things out of it to change them"),
            };
        }
        if state.server_folder {
            match self {
                Self::Copy | Self::Cut => return Some("Copying files off a server is not supported yet"),
                Self::Extract | Self::Compress => return Some("Archives work on local folders only"),
                _ => {}
            }
        } else if !state.local_folder {
            return Some("Open a local folder to use file actions");
        }
        match self {
            Self::Paste | Self::NewFolder => None,
            Self::Rename if state.selection_count != 1 || !state.has_cursor => {
                Some("Select one item to rename")
            }
            Self::Extract | Self::Compress if state.archive_busy => {
                Some("Wait for the current archive operation to finish")
            }
            Self::Extract if !state.selected_archive || state.selection_count != 1 => {
                Some("Select one ZIP or TAR archive to extract")
            }
            _ if state.selection_count == 0 => Some("Select files or folders first"),
            _ => None,
        }
    }
}

/// One row: glyph, label, the action it sends, and whether it needs a
/// selection.
struct Item {
    glyph: Glyph,
    label: &'static str,
    action: RailAction,
    hint: &'static str,
}

/// A gap between groups.
///
/// Space rather than a rule. A hairline across a 64px strip is a lot of
/// contrast for something whose only job is to say "these three go together",
/// and the rail already sits on its own card — one more line inside a bordered
/// box reads as clutter. Grouping by rhythm is quieter and says the same thing.
const GAP: Option<Item> = None;

fn items() -> Vec<Option<Item>> {
    vec![
        Some(Item {
            glyph: Glyph::Copy,
            label: "Copy",
            action: RailAction::Copy,
            hint: "Copy to clipboard (Ctrl+C)",
        }),
        Some(Item {
            glyph: Glyph::Cut,
            label: "Cut",
            action: RailAction::Cut,
            hint: "Cut to clipboard (Ctrl+X)",
        }),
        // Clipboard contents are read only when Paste is invoked. Whether the clipboard holds files is a question for
        // the shell on a worker thread, and asking it every frame to grey out a
        // button would be a COM call in the paint path.
        Some(Item {
            glyph: Glyph::Paste,
            label: "Paste",
            action: RailAction::Paste,
            hint: "Paste here (Ctrl+V)",
        }),
        GAP,
        Some(Item {
            glyph: Glyph::Rename,
            label: "Rename",
            action: RailAction::Rename,
            hint: "Rename this file (F2)",
        }),
        Some(Item {
            glyph: Glyph::Trash,
            label: if cfg!(target_os = "macos") { "Trash" } else { "Delete" },
            action: RailAction::Delete,
            hint: if cfg!(target_os = "macos") {
                "Move to Trash (Cmd+Backspace)"
            } else {
                "Send to the Recycle Bin (Delete)"
            },
        }),
        Some(Item {
            glyph: Glyph::NewFolder,
            label: "Folder",
            action: RailAction::NewFolder,
            hint: "New folder (Ctrl+Shift+N)",
        }),
        GAP,
        Some(Item {
            glyph: Glyph::Extract,
            label: "Extract",
            action: RailAction::Extract,
            hint: "Extract this archive",
        }),
        Some(Item {
            glyph: Glyph::Compress,
            label: "Zip",
            action: RailAction::Compress,
            hint: "Compress to a zip",
        }),
    ]
}

/// Draws the rail and returns whatever was clicked.
pub fn show(ui: &mut Ui, p: &Palette, state: &RailState) -> Option<RailAction> {
    let mut chosen = None;
    let full = ui.available_rect_before_wrap();

    let _ = full;
    egui::ScrollArea::vertical()
        .id_salt("action-rail-scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let mut y = full.top();

            for entry in items() {
                let Some(item) = entry else {
                    y += 12.0;
                    continue;
                };

                let rect = Rect::from_min_size(pos2(full.left(), y), vec2(full.width(), ITEM));
                y += ITEM + SPACING;

                let unavailable = item.action.unavailable(state);
                let enabled = unavailable.is_none();

                let response = ui.allocate_rect(
                    rect,
                    if enabled {
                        Sense::click()
                    } else {
                        Sense::hover()
                    },
                );

                // Each verb sits in a soft well. Under the pointer the well
                // lifts and its glyph takes the accent; an unavailable verb's
                // well all but disappears and its glyph and label go faint.
                let well = egui::Rect::from_center_size(
                    pos2(rect.center().x, rect.top() + 6.0 + 20.0),
                    vec2(40.0, 40.0),
                );
                let corners = egui::CornerRadius::same(14);
                let hovered = enabled && response.hovered();
                if hovered {
                    theme::paint_raised(ui.painter(), well, corners, p);
                } else {
                    ui.painter().rect_filled(
                        well,
                        corners,
                        egui::Color32::from_white_alpha(if enabled { 10 } else { 5 }),
                    );
                }

                let (glyph_colour, text_colour) = if !enabled {
                    (theme::with_alpha(p.text_faint, 130), theme::with_alpha(p.text_faint, 130))
                } else if hovered {
                    (p.accent, p.text)
                } else {
                    (p.text_soft(), p.text_soft())
                };

                icons::draw(ui.painter(), well.center(), item.glyph, glyph_colour);

                let galley = ui.painter().layout_no_wrap(
                    item.label.to_owned(),
                    theme::strong(ui.ctx(), 10.5),
                    text_colour,
                );
                ui.painter().galley(
                    pos2(
                        (rect.center().x - galley.size().x / 2.0).round(),
                        well.bottom() + 5.0,
                    ),
                    galley,
                    text_colour,
                );

                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, item.label)
                });
                let hint = unavailable.unwrap_or(item.hint);
                let hint = neutron_ui::format::shortcut(hint);
                let response = response.on_hover_text(hint);
                if enabled && response.clicked() {
                    chosen = Some(item.action);
                }
            }
        });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_appears_exactly_once() {
        let mut seen: Vec<RailAction> = items().into_iter().flatten().map(|i| i.action).collect();
        let before = seen.len();
        seen.sort_by_key(|action| *action as u8);
        seen.dedup();
        assert_eq!(before, seen.len(), "an action is listed twice");
        assert!(before >= 8, "only {before} actions on the rail");
    }

    /// The labels sit in a 54px strip at 9pt. A long one does not wrap, it
    /// overlaps its neighbours.
    #[test]
    fn labels_fit_the_strip() {
        for item in items().into_iter().flatten() {
            assert!(
                item.label.len() <= 9,
                "{:?}: {:?} is too long for the rail",
                item.action,
                item.label
            );
        }
    }

    /// Every row says its keystroke, because the rail is also how someone
    /// finds out the keystroke exists.
    #[test]
    fn the_hints_teach_the_shortcut() {
        for item in items().into_iter().flatten() {
            assert!(!item.hint.is_empty(), "{:?} has no hover text", item.action);
        }
    }

    #[test]
    fn dashboard_and_virtual_locations_offer_no_local_mutations() {
        for item in items().into_iter().flatten() {
            assert!(item.action.unavailable(&RailState::default()).is_some());
        }
    }
    #[test]
    fn an_empty_folder_can_create_and_paste_but_cannot_mutate_a_selection() {
        let state = RailState {
            local_folder: true,
            ..Default::default()
        };
        assert!(RailAction::Paste.unavailable(&state).is_none());
        assert!(RailAction::NewFolder.unavailable(&state).is_none());
        for action in [
            RailAction::Copy,
            RailAction::Cut,
            RailAction::Delete,
            RailAction::Rename,
            RailAction::Extract,
            RailAction::Compress,
        ] {
            assert!(action.unavailable(&state).is_some());
        }
    }
    #[test]
    fn a_server_folder_offers_the_writes_a_server_can_take() {
        let state = RailState { server_folder: true, selection_count: 1, has_cursor: true, ..Default::default() };
        for action in [RailAction::Paste, RailAction::NewFolder, RailAction::Rename, RailAction::Delete] {
            assert!(action.unavailable(&state).is_none(), "{action:?} should work on a server");
        }
        for action in [RailAction::Copy, RailAction::Cut, RailAction::Extract, RailAction::Compress] {
            assert!(action.unavailable(&state).is_some(), "{action:?} should explain itself");
        }
    }

    #[test]
    fn extracting_requires_one_archive_and_an_idle_worker() {
        let mut state = RailState {
            local_folder: true,
            selection_count: 1,
            has_cursor: true,
            ..Default::default()
        };
        assert!(RailAction::Extract.unavailable(&state).is_some());
        state.selected_archive = true;
        assert!(RailAction::Extract.unavailable(&state).is_none());
        state.archive_busy = true;
        assert!(RailAction::Extract.unavailable(&state).is_some());
        assert!(RailAction::Compress.unavailable(&state).is_some());
        assert!(RailAction::Copy.unavailable(&state).is_none());
    }

    #[test]
    fn the_rail_is_grouped() {
        assert!(
            items().iter().any(|i| i.is_none()),
            "a flat list of ten verbs is a menu, not a rail"
        );
    }

    /// The strip has to hold a 9pt label plus side padding. This is the number
    /// the label length test is derived from, so it is worth pinning.
    #[test]
    fn the_strip_is_wide_enough_for_its_labels() {
        // Narrower than this and the rail crops its own labels.
        const { assert!(WIDTH >= 60.0) };
    }

    #[test]
    fn inside_an_archive_only_copying_out_is_offered() {
        let state = RailState { archive_folder: true, selection_count: 1, has_cursor: true, ..Default::default() };
        assert!(RailAction::Copy.unavailable(&state).is_none());
        for action in [RailAction::Cut, RailAction::Paste, RailAction::Rename, RailAction::Delete, RailAction::NewFolder] {
            assert!(action.unavailable(&state).is_some(), "{action:?} would change an archive");
        }
    }
}
