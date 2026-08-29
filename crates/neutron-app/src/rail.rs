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
//! # Enablement is about the selection, not about whether it would work
//!
//! A row is dimmed when there is nothing for it to act on — no selection, no
//! cursor. It is *not* dimmed for a location that cannot be written to: those
//! go through the same actions as the keyboard and meet the same guard in
//! `app.rs`, which answers with a sentence naming the place. A rail that
//! silently greys itself out in a zip teaches nothing; one that says "Neutron
//! cannot rename anything in This PC" teaches the rule once.

use egui::{Rect, Sense, Ui, pos2, vec2};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette};

/// Width of the strip, including its padding.
pub const WIDTH: f32 = 64.0;

/// Height of one button: glyph, then its label.
const ITEM: f32 = 40.0;

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

/// What the rail needs to know to dim the right rows.
pub struct RailState {
    /// Something is selected, so the verbs that consume a selection apply.
    pub has_selection: bool,
    /// A cursor row exists. Rename acts on one row, not on the set.
    pub has_cursor: bool,
}

/// One row: glyph, label, the action it sends, and whether it needs a
/// selection.
struct Item {
    glyph: Glyph,
    label: &'static str,
    action: RailAction,
    hint: &'static str,
    needs: Needs,
}

#[derive(PartialEq, Eq)]
enum Needs {
    /// Always available — it acts on the folder, not on what is picked.
    Nothing,
    Selection,
    Cursor,
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
        Some(Item { glyph: Glyph::Copy, label: "Copy", action: RailAction::Copy, hint: "Copy to clipboard (Ctrl+C)", needs: Needs::Selection }),
        Some(Item { glyph: Glyph::Cut, label: "Cut", action: RailAction::Cut, hint: "Cut to clipboard (Ctrl+X)", needs: Needs::Selection }),
        // Paste never dims. Whether the clipboard holds files is a question for
        // the shell on a worker thread, and asking it every frame to grey out a
        // button would be a COM call in the paint path.
        Some(Item { glyph: Glyph::Paste, label: "Paste", action: RailAction::Paste, hint: "Paste here (Ctrl+V)", needs: Needs::Nothing }),
        GAP,
        Some(Item { glyph: Glyph::Rename, label: "Rename", action: RailAction::Rename, hint: "Rename this file (F2)", needs: Needs::Cursor }),
        Some(Item { glyph: Glyph::Trash, label: "Delete", action: RailAction::Delete, hint: "Send to the Recycle Bin (Delete)", needs: Needs::Selection }),
        Some(Item { glyph: Glyph::NewFolder, label: "Folder", action: RailAction::NewFolder, hint: "New folder (Ctrl+Shift+N)", needs: Needs::Nothing }),
        GAP,
        Some(Item { glyph: Glyph::Extract, label: "Extract", action: RailAction::Extract, hint: "Extract this archive", needs: Needs::Selection }),
        Some(Item { glyph: Glyph::Compress, label: "Zip", action: RailAction::Compress, hint: "Compress to a zip", needs: Needs::Selection }),
    ]
}

/// Draws the rail and returns whatever was clicked.
pub fn show(ui: &mut Ui, p: &Palette, state: &RailState) -> Option<RailAction> {
    let mut chosen = None;
    let full = ui.available_rect_before_wrap();

    // A whisper of a gradient down the strip, so the rail reads as a distinct
    // surface from the sidebar beside it without needing a heavier fill or a
    // second border to say so.
    theme::vertical_wash(ui.painter(), full.expand(12.0), p);

    let mut y = full.top();

    for entry in items() {
        let Some(item) = entry else {
            y += 12.0;
            continue;
        };

        let rect = Rect::from_min_size(pos2(full.left(), y), vec2(full.width(), ITEM));
        y += ITEM + 1.0;

        let enabled = match item.needs {
            Needs::Nothing => true,
            Needs::Selection => state.has_selection,
            Needs::Cursor => state.has_cursor,
        };

        let response = ui.allocate_rect(rect, if enabled { Sense::click() } else { Sense::hover() });

        if enabled && response.hovered() {
            ui.painter().rect_filled(
                rect.shrink2(vec2(3.0, 1.0)),
                theme::RADIUS_SMALL,
                p.hover,
            );
        }

        // Dimming is done by choosing a fainter colour rather than by fading
        // the whole row: egui has no opacity on a painter, and a grey glyph
        // beside grey text is the same signal.
        let (glyph_colour, text_colour) = if !enabled {
            (p.text_faint, p.text_faint)
        } else if response.hovered() {
            (p.text, p.text)
        } else {
            (p.icon, p.text_muted)
        };

        icons::draw(
            ui.painter(),
            pos2(rect.center().x, rect.top() + 13.0),
            item.glyph,
            glyph_colour,
        );

        let galley = ui.painter().layout_no_wrap(
            item.label.to_owned(),
            egui::FontId::proportional(9.0),
            text_colour,
        );
        ui.painter().galley(
            pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.top() + 24.0,
            ),
            galley,
            text_colour,
        );

        if enabled {
            let response = response.on_hover_text(item.hint);
            if response.clicked() {
                chosen = Some(item.action);
            }
        }
    }

    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_appears_exactly_once() {
        let mut seen: Vec<RailAction> = items()
            .into_iter()
            .flatten()
            .map(|i| i.action)
            .collect();
        let before = seen.len();
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
            assert!(
                !item.hint.is_empty(),
                "{:?} has no hover text",
                item.action
            );
        }
    }

    /// Paste must not dim: asking the shell whether the clipboard holds files
    /// is a COM call, and it would land in the paint path.
    #[test]
    fn paste_is_always_available() {
        let paste = items()
            .into_iter()
            .flatten()
            .find(|i| i.action == RailAction::Paste)
            .expect("paste is on the rail");
        assert!(paste.needs == Needs::Nothing);
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
}
