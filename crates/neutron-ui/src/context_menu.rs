//! Neutron's own context menu, drawn from a shell menu read as plain data.
//!
//! The commands are the shell's — see `neutron_shell::menu` — but the rows are
//! ours, so the menu is the same sheet of glass as everything else in the
//! window instead of a system-grey rectangle dropped on top of it.
//!
//! # What this is not
//!
//! It is not `egui`'s built-in `menu`/`popup`. Those are built around a
//! `Response` and a closure that declares its items inline; this menu's items
//! arrive at runtime from another process's shell extensions, nest to arbitrary
//! depth, and have to stay open across the frames it takes to answer. Driving
//! that through a retained [`MenuState`] is simpler than fighting the
//! immediate-mode API into holding it.

use egui::{
    Align2, Area, Color32, CornerRadius, FontId, Id, Order, Pos2, Rect, Sense, Stroke, Vec2, pos2,
    vec2,
};

use neutron_core::menu::MenuItem;

use crate::theme::{self, Palette};

/// Row metrics. A context menu row is tighter than a list row — it is read in
/// one glance, and Explorer's own is 22px at 100% scale.
const ROW_HEIGHT: f32 = 26.0;
const SEPARATOR_HEIGHT: f32 = 7.0;
const PAD_X: f32 = 12.0;
/// Space kept between a label and its shortcut, so "Rename F2" never runs
/// together on the widest row in the menu.
const ACCEL_GAP: f32 = 28.0;
const ARROW_WIDTH: f32 = 16.0;
/// Left gutter for the check mark, present on every row so labels line up
/// whether or not anything in the menu is checkable.
const GUTTER: f32 = 18.0;
const MIN_WIDTH: f32 = 168.0;
const MAX_WIDTH: f32 = 420.0;
const FONT: f32 = 13.0;

/// What the menu did this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    /// Still open. Show it again next frame.
    Open,
    /// The user picked the command with this shell id.
    Chosen(u32),
    /// Dismissed without choosing.
    Dismissed,
}

/// An open context menu.
///
/// Retained by the app between frames, because the menu outlives the click that
/// opened it and the shell is blocked on an apartment thread waiting for the
/// answer.
pub struct MenuState {
    pub items: Vec<MenuItem>,
    /// Where the user clicked, in screen coordinates.
    pub anchor: Pos2,
    /// Indices of the submenus currently expanded, outermost first.
    open: Vec<usize>,
    /// Path to the highlighted row. Empty when nothing is highlighted, which is
    /// the state a menu opens in — arrowing down should land on the first item,
    /// not the second.
    cursor: Vec<usize>,
    /// True until the menu has been drawn once.
    ///
    /// A menu must not act on the same click that opened it. Without this the
    /// press is still down on the first frame, and the release that follows
    /// lands on whichever row happens to be under the cursor.
    fresh: bool,
}

impl MenuState {
    pub fn new(items: Vec<MenuItem>, anchor: Pos2) -> Self {
        MenuState {
            items,
            anchor,
            open: Vec::new(),
            cursor: Vec::new(),
            fresh: true,
        }
    }
}

/// Draws the menu and reports what the user did.
pub fn show(ctx: &egui::Context, p: &Palette, state: &mut MenuState) -> MenuOutcome {
    if state.items.is_empty() {
        return MenuOutcome::Dismissed;
    }

    if let Some(outcome) = handle_keys(ctx, state) {
        return outcome;
    }

    let screen = ctx.input(|i| i.content_rect());
    let mut chosen = None;
    // Whether the pointer is over any level of the menu. Accumulated across
    // levels because the click-outside test cannot use a single panel's rect
    // once a submenu is showing beside it.
    let mut over_menu = false;
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    // The open path as this frame's hovering rewrites it, applied at the end so
    // that levels drawn later still see the path they were drawn from.
    let mut next_open = state.open.clone();

    let mut origin = state.anchor;
    let mut level = 0usize;
    while let Some(items) = items_at(&state.items, &state.open[..level]) {

        let size = panel_size(ctx, items);
        let rect = place(origin, size, screen, level > 0);

        draw_panel(ctx, p, state, level, items, rect, &mut chosen, &mut next_open);

        // The whole panel counts as "inside", not merely its rows. Testing row
        // hover instead left the padding, the separators and the 4px margin
        // down each side reading as outside, so a click a few pixels from the
        // edge dismissed the menu the user was aiming at.
        over_menu |= pointer.is_some_and(|pos| rect.contains(pos));

        // The next level opens off the row that owns it.
        match next_open.get(level) {
            Some(&index) if level < state.open.len() && state.open[level] == index => {
                origin = pos2(rect.right() - 4.0, rect.top() + row_offset(items, index) - 6.0);
                level += 1;
            }
            _ => break,
        }
    }

    state.open = next_open;

    if let Some(id) = chosen {
        return MenuOutcome::Chosen(id);
    }

    // A press anywhere else closes the menu. Checked on press rather than
    // release so the menu is gone before the click reaches what is underneath.
    let pressed_outside = ctx.input(|i| i.pointer.any_pressed()) && !over_menu;
    if !state.fresh && pressed_outside {
        return MenuOutcome::Dismissed;
    }

    state.fresh = false;
    MenuOutcome::Open
}

/// The item list reached by following `path` into nested submenus.
fn items_at<'a>(items: &'a [MenuItem], path: &[usize]) -> Option<&'a [MenuItem]> {
    let mut current = items;
    for &index in path {
        current = current.get(index)?.children.as_slice();
        if current.is_empty() {
            return None;
        }
    }
    Some(current)
}

/// Distance from the top of a panel to the top of row `index`.
fn row_offset(items: &[MenuItem], index: usize) -> f32 {
    theme::RADIUS_SMALL as f32 * 0.5
        + items[..index]
            .iter()
            .map(|i| if i.separator { SEPARATOR_HEIGHT } else { ROW_HEIGHT })
            .sum::<f32>()
        + 6.0
}

/// Measures a panel from its widest row.
///
/// Every row is laid out to the same width, so a menu whose entries range from
/// "Cut" to "Extract to a subfolder of the current one" is one rectangle rather
/// than a ragged stack.
fn panel_size(ctx: &egui::Context, items: &[MenuItem]) -> Vec2 {
    let font = FontId::proportional(FONT);
    let width = ctx.fonts_mut(|f| {
        items
            .iter()
            .filter(|i| !i.separator)
            .map(|item| {
                let label = f
                    .layout_no_wrap(item.label.clone(), font.clone(), Color32::WHITE)
                    .size()
                    .x;
                let extra = if item.accel.is_empty() {
                    0.0
                } else {
                    ACCEL_GAP
                        + f.layout_no_wrap(item.accel.clone(), font.clone(), Color32::WHITE)
                            .size()
                            .x
                };
                let arrow = if item.is_submenu() { ARROW_WIDTH } else { 0.0 };
                GUTTER + label + extra + arrow
            })
            .fold(0.0_f32, f32::max)
    });

    let height: f32 = items
        .iter()
        .map(|i| if i.separator { SEPARATOR_HEIGHT } else { ROW_HEIGHT })
        .sum();

    vec2(
        (width + PAD_X * 2.0).clamp(MIN_WIDTH, MAX_WIDTH),
        height + 12.0,
    )
}

/// Positions a panel so it stays on screen.
///
/// A submenu that will not fit to the right flips to the left of its parent,
/// which is what every menu on every platform does and what makes a menu near
/// the right edge usable at all.
fn place(origin: Pos2, size: Vec2, screen: Rect, is_submenu: bool) -> Rect {
    let mut x = origin.x;
    if x + size.x > screen.right() {
        x = if is_submenu {
            // Back across the parent panel, not merely clamped: clamping would
            // lay the submenu on top of the row that opened it.
            origin.x - size.x
        } else {
            origin.x - size.x
        };
    }
    let x = x.max(screen.left() + 4.0);

    let mut y = origin.y;
    if y + size.y > screen.bottom() {
        // Grow upwards from the click instead of downwards.
        y = origin.y - size.y;
    }
    let y = y.max(screen.top() + 4.0);

    Rect::from_min_size(pos2(x, y), size)
}

/// Draws one panel.
#[allow(clippy::too_many_arguments)]
fn draw_panel(
    ctx: &egui::Context,
    p: &Palette,
    state: &MenuState,
    level: usize,
    items: &[MenuItem],
    rect: Rect,
    chosen: &mut Option<u32>,
    next_open: &mut Vec<usize>,
) {
    Area::new(Id::new("neutron-context-menu").with(level))
        .order(Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            let painter = ui.painter();
            let radius = CornerRadius::same(theme::RADIUS_CONTROL);

            painter.rect_filled(rect, radius, Color32::from_black_alpha(64));
            painter.rect_filled(rect, radius, p.elevated);
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, p.border_strong),
                egui::StrokeKind::Inside,
            );
            theme::glass_highlight(painter, rect, radius);

            let mut y = rect.top() + 6.0;
            for (index, item) in items.iter().enumerate() {
                if item.separator {
                    let mid = y + SEPARATOR_HEIGHT * 0.5;
                    painter.line_segment(
                        [
                            pos2(rect.left() + PAD_X, mid.round() + 0.5),
                            pos2(rect.right() - PAD_X, mid.round() + 0.5),
                        ],
                        Stroke::new(1.0, p.border),
                    );
                    y += SEPARATOR_HEIGHT;
                    continue;
                }

                let row = Rect::from_min_size(
                    pos2(rect.left() + 4.0, y),
                    vec2(rect.width() - 8.0, ROW_HEIGHT),
                );
                let response = ui.interact(
                    row,
                    ui.id().with(index),
                    if item.enabled { Sense::click() } else { Sense::hover() },
                );

                if response.hovered() && item.enabled {
                    // Hovering a row is what steers the open path: descend into
                    // a submenu, and collapse anything deeper than a plain
                    // command.
                    next_open.truncate(level);
                    if item.is_submenu() {
                        next_open.push(index);
                    }
                }

                let active = response.hovered()
                    || state.cursor.len() == level + 1 && state.cursor[level] == index
                    || (item.is_submenu() && next_open.get(level) == Some(&index));

                draw_row(ui, p, item, row, active);

                if response.clicked() && item.enabled && !item.is_submenu() {
                    *chosen = Some(item.id);
                }
                y += ROW_HEIGHT;
            }
        });
}

fn draw_row(ui: &egui::Ui, p: &Palette, item: &MenuItem, row: Rect, active: bool) {
    let painter = ui.painter();

    if active && item.enabled {
        painter.rect_filled(row, CornerRadius::same(theme::RADIUS_SMALL), p.selection);
    }

    let colour = if !item.enabled {
        p.text_faint
    } else if item.default {
        // The command a double-click would run. Tinting it is how the menu
        // answers "what does opening this actually do?" for an unfamiliar file
        // type, without a second line of explanation.
        p.accent
    } else {
        p.text
    };

    if item.checked {
        painter.text(
            pos2(row.left() + PAD_X - 2.0, row.center().y),
            Align2::LEFT_CENTER,
            "\u{2713}",
            FontId::proportional(FONT),
            if item.enabled { p.accent } else { p.text_faint },
        );
    }

    painter.text(
        pos2(row.left() + PAD_X + GUTTER, row.center().y),
        Align2::LEFT_CENTER,
        &item.label,
        FontId::proportional(FONT),
        colour,
    );

    if !item.accel.is_empty() {
        painter.text(
            pos2(row.right() - PAD_X, row.center().y),
            Align2::RIGHT_CENTER,
            &item.accel,
            FontId::proportional(FONT - 1.0),
            p.text_faint,
        );
    }

    if item.is_submenu() {
        // Drawn rather than typeset: the glyph a font supplies for a menu arrow
        // varies with whichever fallback face the platform picks, and a
        // three-point triangle is the same shape everywhere.
        let x = row.right() - PAD_X;
        let y = row.center().y;
        painter.add(egui::Shape::convex_polygon(
            vec![pos2(x - 5.0, y - 4.0), pos2(x, y), pos2(x - 5.0, y + 4.0)],
            if item.enabled { p.icon } else { p.text_faint },
            Stroke::NONE,
        ));
    }
}

/// Keyboard navigation. Returns an outcome when a key ends the menu.
fn handle_keys(ctx: &egui::Context, state: &mut MenuState) -> Option<MenuOutcome> {
    use egui::Key;

    let (esc, up, down, left, right, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
            i.consume_key(egui::Modifiers::NONE, Key::Enter),
        )
    });

    if esc {
        // Escape backs out one level before closing, so a wrong turn into
        // "Send to" does not cost the whole menu.
        if state.open.pop().is_some() {
            state.cursor.truncate(state.open.len() + 1);
            return None;
        }
        return Some(MenuOutcome::Dismissed);
    }

    let level = state.open.len();
    let items = items_at(&state.items, &state.open)?;

    if up || down {
        let current = (state.cursor.len() == level + 1).then(|| state.cursor[level]);
        if let Some(next) = step(items, current, if down { 1 } else { -1 }) {
            state.cursor.truncate(level);
            state.cursor.push(next);
        }
    }

    if right {
        if let Some(&index) = state.cursor.get(level) {
            if items.get(index).is_some_and(|i| i.is_submenu()) {
                state.open.push(index);
                state.cursor.truncate(level + 1);
                if let Some(first) = items_at(&state.items, &state.open)
                    .and_then(|sub| step(sub, None, 1))
                {
                    state.cursor.push(first);
                }
            }
        }
    }

    if left && state.open.pop().is_some() {
        state.cursor.truncate(state.open.len() + 1);
    }

    if enter {
        if let Some(&index) = state.cursor.get(level) {
            if let Some(item) = items.get(index) {
                if item.is_submenu() {
                    state.open.push(index);
                    state.cursor.truncate(level + 1);
                } else if item.enabled {
                    return Some(MenuOutcome::Chosen(item.id));
                }
            }
        }
    }

    None
}

/// The next selectable row in `direction`, wrapping at the ends.
///
/// Wraps because a context menu is short: arrowing up from the first item to
/// reach the last is faster than the alternative, and there is no scrollback to
/// get lost in.
fn step(items: &[MenuItem], from: Option<usize>, direction: isize) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    let start = match from {
        Some(i) => i as isize,
        // Entering from nowhere should land on the first row going down and the
        // last going up, which is what stepping from just outside each end
        // gives.
        None if direction > 0 => -1,
        None => n as isize,
    };

    for hop in 1..=n as isize {
        let candidate = (start + direction * hop).rem_euclid(n as isize) as usize;
        if items[candidate].selectable() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(label: &str) -> MenuItem {
        MenuItem { id: 1, label: label.to_owned(), enabled: true, ..Default::default() }
    }

    #[test]
    fn stepping_down_from_nothing_lands_on_the_first_row() {
        let items = vec![cmd("Open"), cmd("Copy")];
        assert_eq!(step(&items, None, 1), Some(0));
    }

    #[test]
    fn stepping_up_from_nothing_lands_on_the_last_row() {
        let items = vec![cmd("Open"), cmd("Copy")];
        assert_eq!(step(&items, None, -1), Some(1));
    }

    #[test]
    fn stepping_skips_separators() {
        let items = vec![cmd("Open"), MenuItem::separator(), cmd("Copy")];
        assert_eq!(step(&items, Some(0), 1), Some(2));
    }

    #[test]
    fn stepping_skips_disabled_rows() {
        let mut disabled = cmd("Paste");
        disabled.enabled = false;
        let items = vec![cmd("Open"), disabled, cmd("Copy")];
        assert_eq!(step(&items, Some(0), 1), Some(2));
    }

    #[test]
    fn stepping_wraps_at_the_end() {
        let items = vec![cmd("Open"), cmd("Copy")];
        assert_eq!(step(&items, Some(1), 1), Some(0));
    }

    #[test]
    fn a_menu_with_nothing_selectable_has_nowhere_to_step() {
        // Every row disabled is a real state — a read-only location where the
        // shell offers only commands that do not apply.
        let items = vec![MenuItem::separator(), MenuItem::separator()];
        assert_eq!(step(&items, None, 1), None);
    }

    #[test]
    fn descending_a_path_reaches_the_submenu() {
        let mut parent = cmd("Send to");
        parent.children = vec![cmd("Desktop")];
        let items = vec![cmd("Open"), parent];
        assert_eq!(items_at(&items, &[1]).unwrap()[0].label, "Desktop");
    }

    #[test]
    fn descending_into_a_plain_command_reaches_nothing() {
        // The open path is rewritten by hovering, and a stale path pointing at
        // a row that is not a submenu must not index into an empty slice.
        let items = vec![cmd("Open")];
        assert!(items_at(&items, &[0]).is_none());
    }

    #[test]
    fn descending_past_the_end_reaches_nothing() {
        assert!(items_at(&[cmd("Open")], &[7]).is_none());
    }

    #[test]
    fn a_panel_near_the_right_edge_flips_to_the_left() {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
        let rect = place(pos2(950.0, 100.0), vec2(200.0, 300.0), screen, false);
        assert!(rect.right() <= screen.right());
    }

    #[test]
    fn a_panel_near_the_bottom_grows_upwards() {
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
        let rect = place(pos2(100.0, 700.0), vec2(200.0, 300.0), screen, false);
        assert!(rect.bottom() <= screen.bottom());
        assert!(rect.top() < 700.0);
    }

    #[test]
    fn a_panel_taller_than_the_screen_still_starts_on_it() {
        // Clamping the top is what keeps the first rows reachable; without it a
        // long "Open with" list would begin above the window.
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 400.0));
        let rect = place(pos2(100.0, 380.0), vec2(200.0, 900.0), screen, false);
        assert!(rect.top() >= screen.top());
    }

    #[test]
    fn row_offsets_account_for_separators_being_shorter() {
        let items = vec![cmd("Open"), MenuItem::separator(), cmd("Copy")];
        let gap = row_offset(&items, 2) - row_offset(&items, 1);
        assert_eq!(gap, SEPARATOR_HEIGHT);
    }
}
