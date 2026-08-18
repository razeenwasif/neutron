//! The finder overlay: one widget, three sources.
//!
//! A centred card over a dimmed window — prompt at the top, ranked results
//! beneath, entirely keyboard-driven. The same widget serves:
//!
//! | Key | Mode | Corpus | Matching |
//! |---|---|---|---|
//! | `Ctrl+P` | [`Mode::Files`] | this folder and everything under it | fuzzy |
//! | `Ctrl+Shift+P` | [`Mode::Commands`] | what the app can do | fuzzy |
//! | `Ctrl+Shift+F` | [`Mode::Everything`] | every indexed volume | substring |
//!
//! # Why the matching differs by mode
//!
//! Fuzzy matching is right when the candidate set is small enough that a
//! near-miss is plausibly what you meant. Over three million names it is wrong
//! twice: it costs far more per candidate, and it buries the exact hit under
//! everything that merely shares some letters. So the scoped modes match
//! fuzzily and rank; the global one matches by substring. That is a property of
//! the corpus, not a preference, which is why it is fixed per mode rather than
//! exposed as a toggle.
//!
//! # Highlighting
//!
//! Fuzzy results carry the character positions that matched, and the row draws
//! those characters in the accent. Without it a ranked fuzzy list is unreadable
//! — the third result looks arbitrary until you can see *why* it matched.

use egui::{Align2, Color32, Rect, Sense, Ui, pos2, vec2};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, RADIUS_CARD, RADIUS_CONTROL};

/// Which corpus the overlay is searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Files under the focused pane's folder.
    Files,
    /// What the application can do.
    Commands,
    /// Every indexed volume.
    Everything,
}

impl Mode {
    pub fn placeholder(self) -> &'static str {
        match self {
            Mode::Files => "Find a file in this folder",
            Mode::Commands => "Run a command",
            Mode::Everything => "Search every volume",
        }
    }

    /// Shown as a chip beside the prompt, so the mode is never in doubt — the
    /// three look identical otherwise, and running a command when you meant to
    /// open a file is not a recoverable mistake.
    pub fn chip(self) -> &'static str {
        match self {
            Mode::Files => "FILES",
            Mode::Commands => "COMMANDS",
            Mode::Everything => "EVERYTHING",
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Mode::Files => Glyph::File,
            Mode::Commands => Glyph::Terminal,
            Mode::Everything => Glyph::Search,
        }
    }
}

/// One row, whatever produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Filename, or command title.
    pub primary: String,
    /// Containing folder, or the command's shortcut.
    pub secondary: String,
    /// Character indices into `primary` that matched, ascending. Empty for
    /// substring results.
    pub matched: Vec<u32>,
    pub is_dir: bool,
}

/// Overlay state, owned by the app.
pub struct Finder {
    pub open: bool,
    pub mode: Mode,
    pub needle: String,
    pub cursor: usize,
    /// Rows the current source produced. Commands are matched in-process, so
    /// they land here directly; the two file modes fill it from the indexer.
    pub rows: Vec<Row>,
    /// Shown under the prompt — result counts, timings, or why there is
    /// nothing to show.
    pub status: String,
}

impl Default for Finder {
    fn default() -> Self {
        Self {
            open: false,
            mode: Mode::Everything,
            needle: String::new(),
            cursor: 0,
            rows: Vec::new(),
            status: String::new(),
        }
    }
}

impl Finder {
    /// Opens in `mode`, or closes if that mode is already showing.
    ///
    /// Pressing the same key again closes, but pressing a *different* finder
    /// key switches mode rather than closing — otherwise `Ctrl+P` while the
    /// command palette is open would dismiss it and require a second press.
    pub fn toggle(&mut self, mode: Mode) {
        if self.open && self.mode == mode {
            self.close();
            return;
        }
        self.open = true;
        self.mode = mode;
        self.needle.clear();
        self.cursor = 0;
        self.rows.clear();
        self.status.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.rows.clear();
        self.needle.clear();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.rows.len() as isize - 1;
        // Wraps, because the list is short and keyboard-driven: holding Down at
        // the end should return to the top rather than stick.
        let next = self.cursor as isize + delta;
        self.cursor = next.rem_euclid(last + 1) as usize;
    }

    /// Replaces the rows, keeping the cursor in range.
    pub fn set_rows(&mut self, rows: Vec<Row>, status: String) {
        self.rows = rows;
        self.status = status;
        if self.cursor >= self.rows.len() {
            self.cursor = 0;
        }
    }
}

/// Geometry, shared with the app so it can place the text field.
const WIDTH: f32 = 780.0;
const PROMPT_H: f32 = 54.0;
const STATUS_H: f32 = 26.0;
const ROW_H: f32 = 46.0;
const MAX_H: f32 = 600.0;
const TOP: f32 = 72.0;

pub fn card_rect(screen: Rect, rows: usize) -> Rect {
    let width = WIDTH.min(screen.width() - 64.0);
    let visible = rows.min(visible_rows(screen));
    let height = (PROMPT_H + STATUS_H + visible as f32 * ROW_H + 8.0).min(MAX_H);
    Rect::from_min_size(
        pos2(screen.center().x - width / 2.0, screen.top() + TOP),
        vec2(width, height),
    )
}

pub fn visible_rows(screen: Rect) -> usize {
    let room = (MAX_H.min(screen.height() - TOP - 40.0) - PROMPT_H - STATUS_H - 8.0) / ROW_H;
    (room.floor() as usize).max(1)
}

/// Where the app draws the text field.
pub fn field_rect(screen: Rect, rows: usize) -> Rect {
    let card = card_rect(screen, rows);
    Rect::from_min_max(
        pos2(card.left() + 50.0, card.top() + 11.0),
        pos2(card.right() - 110.0, card.top() + PROMPT_H - 9.0),
    )
}

/// Something the user did in the overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum FinderAction {
    Close,
    /// Activate the row at this index.
    Activate(usize),
}

/// Draws the overlay. The text field itself is drawn by the app, which owns the
/// string being edited.
pub fn show(ui: &mut Ui, p: &Palette, finder: &Finder) -> Option<FinderAction> {
    if !finder.open {
        return None;
    }

    let screen = ui.ctx().content_rect();
    let card = card_rect(screen, finder.rows.len());
    let layer = egui::LayerId::new(egui::Order::Foreground, "finder".into());
    let painter = ui.painter().clone().with_layer_id(layer);

    // Scrim. Dark in both themes — it dims the window behind, and a light scrim
    // over a light UI reads as fog rather than depth.
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(104));
    let shadow = egui::epaint::Shadow {
        offset: [0, 16],
        blur: 48,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    painter.add(shadow.as_shape(card, egui::CornerRadius::same(RADIUS_CARD)));
    painter.rect(
        card,
        RADIUS_CARD as f32,
        p.elevated,
        egui::Stroke::new(1.0, p.border_strong),
        egui::StrokeKind::Inside,
    );
    theme::glass_highlight(&painter, card, egui::CornerRadius::same(RADIUS_CARD));

    let scrim = ui.interact(screen, ui.id().with("finder-scrim"), Sense::click());

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card).layer_id(layer));
    let mut action = None;

    prompt(&child, p, card, finder);
    if let Some(a) = rows(&mut child, p, card, finder, screen) {
        action = Some(a);
    }

    // Clicking away closes, as every overlay of this shape does.
    if action.is_none()
        && scrim.clicked()
        && !scrim.interact_pointer_pos().is_some_and(|q| card.contains(q))
    {
        action = Some(FinderAction::Close);
    }
    action
}

fn prompt(ui: &Ui, p: &Palette, card: Rect, finder: &Finder) {
    let painter = ui.painter();

    icons::draw(
        painter,
        pos2(card.left() + 28.0, card.top() + PROMPT_H / 2.0),
        finder.mode.glyph(),
        p.text_faint,
    );

    // Mode chip, right-aligned in the prompt row.
    let chip_text = finder.mode.chip();
    let galley = painter.layout_no_wrap(
        chip_text.to_owned(),
        egui::FontId::proportional(9.5),
        p.accent,
    );
    let chip = Rect::from_center_size(
        pos2(card.right() - 24.0 - galley.size().x / 2.0, card.top() + PROMPT_H / 2.0),
        vec2(galley.size().x + 16.0, 20.0),
    );
    painter.rect_filled(chip, RADIUS_CONTROL as f32, p.selection);
    painter.galley(
        pos2(chip.center().x - galley.size().x / 2.0, chip.center().y - galley.size().y / 2.0),
        galley,
        p.accent,
    );

    painter.hline(
        (card.left() + 14.0)..=(card.right() - 14.0),
        card.top() + PROMPT_H,
        egui::Stroke::new(1.0, p.border),
    );

    painter.text(
        pos2(card.left() + 50.0, card.top() + PROMPT_H + STATUS_H / 2.0),
        Align2::LEFT_CENTER,
        &finder.status,
        egui::FontId::proportional(11.0),
        p.text_faint,
    );
}

fn rows(
    ui: &mut Ui,
    p: &Palette,
    card: Rect,
    finder: &Finder,
    screen: Rect,
) -> Option<FinderAction> {
    if finder.rows.is_empty() {
        return None;
    }

    let top = card.top() + PROMPT_H + STATUS_H;
    let visible = visible_rows(screen).min(finder.rows.len());

    // Scroll the window so the cursor stays on screen. No ScrollArea: the list
    // is capped and keyboard-driven, so a scrollbar would be chrome that never
    // gets used.
    let first = finder.cursor.saturating_sub(visible.saturating_sub(1));
    let mut action = None;

    for offset in 0..visible {
        let index = first + offset;
        let Some(row) = finder.rows.get(index) else {
            break;
        };

        let rect = Rect::from_min_size(
            pos2(card.left() + 8.0, top + offset as f32 * ROW_H),
            vec2(card.width() - 16.0, ROW_H),
        );

        let response = ui.interact(rect, ui.id().with(("finder-row", index)), Sense::click());
        let selected = index == finder.cursor;

        if selected {
            ui.painter().rect_filled(
                rect.shrink2(vec2(4.0, 3.0)),
                RADIUS_CONTROL as f32,
                p.selection,
            );
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect.shrink2(vec2(4.0, 3.0)), RADIUS_CONTROL as f32, p.hover);
        }

        icons::draw(
            ui.painter(),
            pos2(rect.left() + 26.0, rect.center().y),
            if finder.mode == Mode::Commands {
                Glyph::ChevronRight
            } else if row.is_dir {
                Glyph::Folder
            } else {
                Glyph::File
            },
            if selected { p.accent } else { p.icon },
        );

        draw_highlighted(
            ui,
            pos2(rect.left() + 48.0, rect.center().y - 9.0),
            rect.right() - 60.0,
            &row.primary,
            &row.matched,
            p,
        );

        // Secondary is a path for files and a shortcut for commands, so it is
        // right-aligned for commands — a keystroke belongs at the edge where
        // the eye expects it, not trailing the title.
        if !row.secondary.is_empty() {
            if finder.mode == Mode::Commands {
                ui.painter().text(
                    pos2(rect.right() - 18.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    &row.secondary,
                    egui::FontId::monospace(11.0),
                    p.text_faint,
                );
            } else {
                clipped(
                    ui,
                    Rect::from_min_max(
                        pos2(rect.left() + 48.0, rect.center().y + 3.0),
                        pos2(rect.right() - 16.0, rect.bottom()),
                    ),
                    &row.secondary,
                    p.text_faint,
                );
            }
        }

        if response.clicked() {
            action = Some(FinderAction::Activate(index));
        }
    }

    action
}

/// Draws `text` with `matched` character positions in the accent.
///
/// Laid out as a single job with per-range colour rather than as separate
/// galleys: separate ones would need manual advance-width accumulation, which
/// drifts on any font with kerning and puts visible gaps between highlighted
/// and plain characters.
fn draw_highlighted(
    ui: &Ui,
    at: egui::Pos2,
    max_width: f32,
    text: &str,
    matched: &[u32],
    p: &Palette,
) {
    let font = egui::FontId::proportional(13.5);
    let mut job = egui::text::LayoutJob::default();

    if matched.is_empty() {
        job.append(text, 0.0, egui::TextFormat { font_id: font, color: p.text, ..Default::default() });
    } else {
        // `matched` indexes *characters*; the string is bytes. Walking with
        // `char_indices` keeps the two in step on non-ASCII names, where a
        // byte-indexed slice would panic or split a character.
        for (i, ch) in text.chars().enumerate() {
            let hit = matched.binary_search(&(i as u32)).is_ok();
            let mut buf = [0u8; 4];
            job.append(
                ch.encode_utf8(&mut buf),
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: if hit { p.accent } else { p.text },
                    ..Default::default()
                },
            );
        }
    }

    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(at, galley, p.text);
}

fn clipped(ui: &Ui, rect: Rect, text: &str, colour: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        egui::FontId::proportional(11.0),
        colour,
    );
    job.wrap = egui::text::TextWrapping {
        max_width: rect.width().max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter()
        .galley(pos2(rect.left(), rect.top()), galley, colour);
}

/// Fuzzy-matches the command catalogue in-process.
///
/// Commands never leave this process — there are a few dozen of them, so the
/// round trip to the indexer would cost more than the match.
pub fn match_commands(needle: &str) -> Vec<(usize, Row)> {
    let mut matcher = neutron_fuzzy::FuzzyMatcher::new();
    let mut scored: Vec<(u32, usize, Row)> = crate::commands::ALL
        .iter()
        .enumerate()
        .filter_map(|(i, command)| {
            matcher.score(command.title, needle).map(|m| {
                (
                    m.score,
                    i,
                    Row {
                        primary: command.title.to_owned(),
                        secondary: command.hint.to_owned(),
                        matched: m.positions,
                        is_dir: false,
                    },
                )
            })
        })
        .collect();

    // An empty needle scores everything zero, so the catalogue's own order
    // survives — which is deliberate, since that order groups related commands.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i, row)| (i, row)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row {
                primary: format!("row{i}"),
                secondary: String::new(),
                matched: Vec::new(),
                is_dir: false,
            })
            .collect()
    }

    #[test]
    fn the_same_key_closes_but_a_different_one_switches_mode() {
        // Ctrl+P while the command palette is open should show files, not
        // dismiss the overlay and make the user press it again.
        let mut f = Finder::default();
        f.toggle(Mode::Commands);
        assert!(f.open && f.mode == Mode::Commands);

        f.toggle(Mode::Files);
        assert!(f.open, "switching mode closed the overlay");
        assert_eq!(f.mode, Mode::Files);

        f.toggle(Mode::Files);
        assert!(!f.open, "the same key did not close it");
    }

    #[test]
    fn opening_clears_the_previous_query_and_results() {
        // Reopening with stale rows would let Enter act on a result from a
        // search the user has already left.
        let mut f = Finder::default();
        f.toggle(Mode::Files);
        f.needle = "old".into();
        f.set_rows(rows(3), String::new());
        f.cursor = 2;

        f.toggle(Mode::Commands);
        assert!(f.needle.is_empty());
        assert!(f.rows.is_empty());
        assert_eq!(f.cursor, 0);
    }

    #[test]
    fn the_cursor_wraps_at_both_ends() {
        let mut f = Finder::default();
        f.set_rows(rows(3), String::new());

        f.move_cursor(-1);
        assert_eq!(f.cursor, 2, "up from the top should wrap to the bottom");
        f.move_cursor(1);
        assert_eq!(f.cursor, 0, "down from the bottom should wrap to the top");
    }

    #[test]
    fn an_empty_list_pins_the_cursor() {
        let mut f = Finder::default();
        f.cursor = 5;
        f.move_cursor(1);
        assert_eq!(f.cursor, 0);
        assert!(f.rows.is_empty());
    }

    #[test]
    fn replacing_rows_pulls_a_stale_cursor_back_in_range() {
        // Results are replaced under the cursor on every keystroke. A cursor
        // left past the end makes the next Enter act on nothing, or on the
        // wrong row.
        let mut f = Finder::default();
        f.set_rows(rows(10), String::new());
        f.cursor = 9;

        f.set_rows(rows(2), String::new());
        assert!(f.cursor < 2, "cursor {} is past the end", f.cursor);
    }

    #[test]
    fn commands_match_fuzzily_and_report_positions() {
        let hits = match_commands("sptr");
        let first = &hits.first().expect("a match").1;
        assert_eq!(first.primary, "Split pane right");
        assert!(!first.matched.is_empty());
        assert!(first.matched.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn an_empty_command_query_keeps_the_catalogue_order() {
        // With nothing typed the list is read, not filtered, and the catalogue
        // is grouped by what each command acts on.
        let hits = match_commands("");
        assert_eq!(hits.len(), crate::commands::ALL.len());
        assert_eq!(hits[0].1.primary, crate::commands::ALL[0].title);
    }

    #[test]
    fn a_command_query_that_matches_nothing_returns_nothing() {
        assert!(match_commands("zzzzqqq").is_empty());
    }

    #[test]
    fn the_card_grows_with_its_results_but_stays_on_screen() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0));
        let empty = card_rect(screen, 0);
        let some = card_rect(screen, 5);
        let many = card_rect(screen, 5000);

        assert!(some.height() > empty.height());
        assert!(many.height() <= MAX_H);
        assert!(many.bottom() <= screen.bottom(), "the card ran off screen");
    }

    #[test]
    fn a_short_window_still_shows_a_row() {
        // Splitting the window down to nothing must not produce a card with
        // room for zero results and no explanation.
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 200.0));
        assert!(visible_rows(screen) >= 1);
    }
}
