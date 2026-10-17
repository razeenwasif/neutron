//! Rendering the pane tree: cards, gutters, tab strips.
//!
//! Layout is computed geometrically rather than with egui's panel machinery.
//! Panels claim space from a parent in declaration order, which cannot express
//! an arbitrarily nested binary tree — a split inside a split inside a split has
//! no fixed declaration order. Recursively subdividing a rect does, and it makes
//! the divider hit-boxes exact.
//!
//! # Cards
//!
//! Each pane is drawn as an opaque card: rounded corners, a hairline, a soft
//! shadow, and a gutter of ground visible around it. Depth comes from
//! elevation rather than from translucency or from ruled borders.
//!
//! The gutter doubles as the divider's grab area, so resizing needs no
//! dedicated visual element. Nothing is painted there at rest — a permanent
//! divider line between panes is exactly the kind of chrome this design is
//! trying to shed — and a soft accent pill only appears on hover or drag.

use egui::{Rect, Sense, Ui, UiBuilder, pos2, vec2};
use neutron_core::layout::{Axis, GroupId, Layout, MIN_RATIO};
use neutron_ui::theme::{self, GUTTER, Palette, RADIUS_CARD, RADIUS_CONTROL};

/// Something the user did to the pane structure itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaneAction {
    Focus(GroupId),
    /// A divider was dragged. `target` is the first pane on its second side;
    /// see `Layout::set_divider_ratio`.
    SetRatio { target: GroupId, ratio: f32 },
}

/// Walks the tree, calling `draw_group` for each leaf inside its card.
pub fn show(
    ui: &mut Ui,
    layout: &Layout,
    rect: Rect,
    p: &Palette,
    focused: GroupId,
    actions: &mut Vec<PaneAction>,
    draw_group: &mut impl FnMut(&mut Ui, GroupId, bool),
) {
    match layout {
        Layout::Leaf(id) => {
            let is_focused = *id == focused;
            let card = rect.shrink(GUTTER / 2.0);
            if card.width() <= 1.0 || card.height() <= 1.0 {
                return;
            }

            draw_card(ui, card, p, is_focused);

            // Clicking anywhere in the card focuses that pane. Registered
            // before the contents so a click that also hits a row still moves
            // focus, and at the lowest layer so it never steals the click.
            let bg = ui.interact(card, ui.id().with(("pane-bg", id.0)), Sense::click());
            if bg.clicked() && !is_focused {
                actions.push(PaneAction::Focus(*id));
            }

            // Inset so contents never touch the rounded corners.
            let inner = card.shrink(1.0);
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(inner)
                    .id_salt(("pane", id.0)),
            );
            child.set_clip_rect(inner);
            draw_group(&mut child, *id, is_focused);
        }

        Layout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (first_rect, gutter, second_rect) = subdivide(rect, *axis, *ratio);

            show(ui, first, first_rect, p, focused, actions, draw_group);
            show(ui, second, second_rect, p, focused, actions, draw_group);

            // Named by the first pane of its second side, which no other
            // divider shares — see `Layout::set_divider_ratio`.
            if let Some(target) = second.groups().first().copied() {
                if let Some(new_ratio) = divider(ui, gutter, *axis, rect, p, target) {
                    actions.push(PaneAction::SetRatio {
                        target,
                        ratio: new_ratio,
                    });
                }
            }
        }
    }
}

/// Paints a pane card: shadow, translucent fill, glass highlight, and hairline.
fn draw_card(ui: &Ui, rect: Rect, p: &Palette, _focused: bool) {
    // The same surface in both states. Focus with several panes open is
    // carried by the active tab, which is small and unambiguous; a ring
    // around the largest thing on screen read as a selection box.
    let painter = ui.painter().with_clip_rect(ui.ctx().content_rect());
    theme::paint_pane(&painter, rect, RADIUS_CARD as f32, p);
}

/// Splits `rect` into two child rects with the gutter between them.
fn subdivide(rect: Rect, axis: Axis, ratio: f32) -> (Rect, Rect, Rect) {
    let ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);

    match axis {
        Axis::Horizontal => {
            // Reserve the gutter before apportioning, so the ratio describes
            // the split of usable space rather than of space-plus-gap.
            let usable = (rect.width() - GUTTER).max(0.0);
            let split_x = rect.left() + usable * ratio;
            // Clamped because a parent narrower than the gutter itself — which
            // happens for a frame while the window is resized to nothing —
            // would otherwise put the second pane's left edge past its right
            // edge, producing an inverted rect.
            let gutter_end = (split_x + GUTTER).min(rect.right());
            (
                Rect::from_min_max(rect.min, pos2(split_x, rect.bottom())),
                Rect::from_min_max(pos2(split_x, rect.top()), pos2(gutter_end, rect.bottom())),
                Rect::from_min_max(pos2(gutter_end, rect.top()), rect.max),
            )
        }
        Axis::Vertical => {
            let usable = (rect.height() - GUTTER).max(0.0);
            let split_y = rect.top() + usable * ratio;
            let gutter_end = (split_y + GUTTER).min(rect.bottom());
            (
                Rect::from_min_max(rect.min, pos2(rect.right(), split_y)),
                Rect::from_min_max(pos2(rect.left(), split_y), pos2(rect.right(), gutter_end)),
                Rect::from_min_max(pos2(rect.left(), gutter_end), rect.max),
            )
        }
    }
}

/// Handles the gutter as a resize grip. Returns a new ratio while dragging.
fn divider(
    ui: &mut Ui,
    rect: Rect,
    axis: Axis,
    parent: Rect,
    p: &Palette,
    split: GroupId,
) -> Option<f32> {
    // Keyed by the split, never by where the gutter is: the gutter moves as it
    // is dragged, and egui keeps a drag attached to the id it began on, so an
    // id built from the position lost the drag after a single frame.
    let id = ui.id().with(("divider", split.0));
    let response = ui.interact(rect, id, Sense::drag());
    let active = response.hovered() || response.dragged();

    if active {
        // A short pill centred in the gutter, not a full-length line: it reads
        // as a grip rather than as a border between two regions.
        let pill = match axis {
            Axis::Horizontal => {
                let h = (parent.height() * 0.12).clamp(18.0, 48.0);
                Rect::from_center_size(rect.center(), vec2(3.0, h))
            }
            Axis::Vertical => {
                let w = (parent.width() * 0.12).clamp(18.0, 48.0);
                Rect::from_center_size(rect.center(), vec2(w, 3.0))
            }
        };
        ui.painter().rect_filled(pill, 1.5, p.accent);

        ui.ctx().set_cursor_icon(match axis {
            Axis::Horizontal => egui::CursorIcon::ResizeHorizontal,
            Axis::Vertical => egui::CursorIcon::ResizeVertical,
        });
    }

    if !response.dragged() {
        return None;
    }

    // Derive the ratio from the pointer's absolute position rather than
    // accumulating drag deltas: accumulation drifts away from the cursor over a
    // long drag, and the divider visibly lags behind the mouse.
    let pointer = ui.ctx().pointer_interact_pos()?;
    let ratio = match axis {
        Axis::Horizontal => {
            let usable = (parent.width() - GUTTER).max(1.0);
            (pointer.x - parent.left()) / usable
        }
        Axis::Vertical => {
            let usable = (parent.height() - GUTTER).max(1.0);
            (pointer.y - parent.top()) / usable
        }
    };
    Some(ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO))
}

/// Height of a tab strip, in points.
pub const TAB_STRIP_HEIGHT: f32 = 44.0;

/// Whether more than one pane is open. Only then does the active tab need to
/// say which pane has focus. Recorded by the app each frame with
/// [`set_multi_pane`].
fn multi_pane(ui: &Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("neutron-multi-pane"))).unwrap_or(false)
}

pub fn set_multi_pane(ctx: &egui::Context, multi: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("neutron-multi-pane"), multi));
}

/// Narrowest a tab is squeezed to before tabs start going off the strip. Wide
/// enough for a few letters of the title and the close button.
const MIN_TAB: f32 = 64.0;
/// Width given to each of the arrows shown when tabs go off the strip.
const OVERFLOW_ARROW: f32 = 26.0;
/// Space between tabs.
const TAB_GAP: f32 = 4.0;

/// Each tab's width when `natural` widths have to share `room`: as they are if
/// they fit, otherwise shrunk evenly — wide tabs first — down to [`MIN_TAB`].
fn fit_tabs(natural: &[f32], room: f32) -> Vec<f32> {
    let gaps = TAB_GAP * natural.len().saturating_sub(1) as f32;
    let total: f32 = natural.iter().sum::<f32>() + gaps;
    if total <= room || natural.is_empty() {
        return natural.to_vec();
    }
    // The largest cap that makes them fit, so short titles keep their width
    // and only the long ones are cut.
    let available = (room - gaps).max(0.0);
    let mut sorted = natural.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mut remaining = available;
    let mut cap = MIN_TAB;
    for (k, w) in sorted.iter().enumerate() {
        let share = remaining / (sorted.len() - k) as f32;
        if *w <= share {
            remaining -= w;
        } else {
            cap = share;
            break;
        }
    }
    natural.iter().map(|w| w.min(cap).max(MIN_TAB)).collect()
}

/// The first and last tab to draw so that `active` is among them and they fit
/// in `room`. Everything, when everything fits.
fn visible_run(widths: &[f32], active: usize, room: f32) -> (usize, usize) {
    if widths.is_empty() {
        return (0, 0);
    }
    let all: f32 = widths.iter().sum::<f32>() + TAB_GAP * (widths.len() - 1) as f32;
    if all <= room + 2.0 * OVERFLOW_ARROW {
        return (0, widths.len() - 1);
    }
    let active = active.min(widths.len() - 1);
    let (mut first, mut last) = (active, active);
    let mut used = widths[active];
    // Grow to the right first, then the left, while there is room.
    loop {
        let grew_right = last + 1 < widths.len() && used + TAB_GAP + widths[last + 1] <= room;
        if grew_right {
            last += 1;
            used += TAB_GAP + widths[last];
        }
        let grew_left = first > 0 && used + TAB_GAP + widths[first - 1] <= room;
        if grew_left {
            first -= 1;
            used += TAB_GAP + widths[first];
        }
        if !grew_right && !grew_left {
            return (first, last);
        }
    }
}

/// Where a tab dragged from `from` lands when released at `x`: the index it
/// will have once moved.
fn landing(drawn: &[(usize, Rect)], x: f32, from: usize) -> usize {
    let before = drawn
        .iter()
        .filter(|(i, r)| *i != from && r.center().x < x)
        .count();
    let offset = drawn.first().map_or(0, |(i, _)| *i);
    // `before` counts drawn tabs left of the pointer other than the dragged
    // one; the dragged tab's own slot closes up when it moves.
    let target = offset + before;
    if offset > from { target - 1 } else { target }
}

/// One of the arrows at the ends of an overflowing strip. Shows how many tabs
/// are off that end; returns whether it was clicked.
fn overflow_arrow(ui: &mut Ui, p: &Palette, rect: Rect, group: GroupId, right: bool, hidden: usize) -> bool {
    let response = ui.interact(rect, ui.id().with(("tab-overflow", group.0, right)), Sense::click());
    if hidden > 0 && response.hovered() {
        ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }
    let colour = if hidden == 0 { p.text_faint } else { p.text_muted };
    let c = rect.center();
    let d = if right { 1.0 } else { -1.0 };
    ui.painter().add(egui::Shape::line(
        vec![pos2(c.x - 2.0 * d, c.y - 4.0), pos2(c.x + 2.0 * d, c.y), pos2(c.x - 2.0 * d, c.y + 4.0)],
        egui::Stroke::new(1.3, colour),
    ));
    if hidden > 0 {
        response.clone().on_hover_text(format!(
            "{hidden} more tab{} {}",
            if hidden == 1 { "" } else { "s" },
            if right { "to the right" } else { "to the left" }
        ));
    }
    response.clicked()
}

/// What the user did in a tab strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabAction {
    Select(usize),
    Close(usize),
    New,
    /// Dragging started on the tab at this index.
    BeginDrag(usize),
    /// A dragged tab was released over this strip.
    DropHere,
    /// A tab dragged within its own strip was released at a new position.
    Reorder { from: usize, to: usize },
}

/// A tab being dragged, tracked across frames by the app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabDrag {
    pub group: GroupId,
    pub index: usize,
}

/// Draws one pane's tab strip, inside its card.
///
/// The strip has no background of its own: it sits on the card, and the tabs
/// are pills. A separately-filled header bar would reintroduce the banded,
/// ruled look the card design replaces.
#[allow(clippy::too_many_arguments)]
pub fn tab_strip(
    ui: &mut Ui,
    group_id: GroupId,
    titles: &[String],
    folders: &[Option<std::path::PathBuf>],
    active: usize,
    focused: bool,
    drag: Option<TabDrag>,
    p: &Palette,
) -> Option<TabAction> {
    let mut action = None;

    let full = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(full.min, vec2(full.width(), TAB_STRIP_HEIGHT));
    ui.advance_cursor_after_rect(rect);

    // Every tab's natural width, then shrunk together to fit. Tabs that did
    // not fit used to be dropped from the strip altogether — still open, but
    // reachable only by Ctrl+1…9.
    // Measured in the active tab's heavier weight, so a tab is the same width
    // whichever is active and an active title is never the one cut short. The
    // padding is the text's inset on the left and the close button's slot.
    let font = egui::FontId::proportional(12.5);
    let measure = theme::strong(ui.ctx(), 12.5);
    let natural: Vec<f32> = titles
        .iter()
        .map(|t| (ui.painter().layout_no_wrap(t.clone(), measure.clone(), p.text).size().x + 16.0 + 30.0).clamp(88.0, 200.0))
        .collect();
    let room = (rect.width() - 8.0 - 34.0).max(0.0);
    let widths = fit_tabs(&natural, room);
    // When even the narrowest tabs do not fit, show a run of them around the
    // active one, with arrows to step to the ones either side.
    let (first, last) = visible_run(&widths, active, room - 2.0 * OVERFLOW_ARROW);
    let overflowing = first > 0 || last + 1 < titles.len();

    // Drop target: any strip other than the one the drag started in.
    if drag.is_some_and(|d| d.group != group_id) {
        let hovered = ui
            .ctx()
            .pointer_interact_pos()
            .is_some_and(|pos| rect.contains(pos));
        if hovered {
            ui.painter()
                .rect_filled(rect.shrink(3.0), RADIUS_CONTROL as f32, p.selection);
            if ui.input(|i| i.pointer.any_released()) {
                action = Some(TabAction::DropHere);
            }
        }
    }

    let mut x = rect.left() + 8.0;
    if overflowing {
        let arrow = Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(OVERFLOW_ARROW - 4.0, rect.height() - 12.0));
        if overflow_arrow(ui, p, arrow, group_id, false, first) && first > 0 {
            action = Some(TabAction::Select(first - 1));
        }
        x += OVERFLOW_ARROW;
    }

    // Where each drawn tab sits, for working out where a dragged one lands.
    let mut drawn: Vec<(usize, Rect)> = Vec::new();

    for (i, title) in titles.iter().enumerate().take(last + 1).skip(first) {
        let is_active = i == active;
        let width = widths[i];

        let tab_rect =
            Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(width, rect.height() - 12.0));
        drawn.push((i, tab_rect));

        let response = ui.interact(
            tab_rect,
            ui.id().with(("tab", group_id.0, i)),
            Sense::click_and_drag(),
        );
        // A tab on a folder takes files dropped on it, and shows itself when
        // a drag is held there.
        if let Some(Some(folder)) = folders.get(i) {
            neutron_ui::drop::offer_tab(ui, tab_rect, folder, tab_rect.height() / 2.0, group_id.0, i);
        }
        let dragging_this = drag == Some(TabDrag { group: group_id, index: i });

        // Tabs are pills; the active one is raised out of the strip. With
        // several panes open, a small accent dot marks the focused pane's
        // active tab, so two panes cannot both look current.
        let pill = theme::pill(tab_rect.height());
        if is_active {
            theme::paint_raised(ui.painter(), tab_rect, pill, p);
            if focused && multi_pane(ui) {
                ui.painter().circle_filled(
                    pos2(tab_rect.left() + 10.0, tab_rect.center().y),
                    2.5,
                    p.accent,
                );
            }
        } else if response.hovered() {
            ui.painter().rect_filled(tab_rect, pill, p.hover);
        }

        if dragging_this {
            // Ghost the source tab rather than removing it: reflowing the strip
            // mid-drag moves every other tab out from under the cursor.
            ui.painter()
                .rect_filled(tab_rect, theme::pill(tab_rect.height()), p.hover);
        }

        let (text_colour, text_font) = if is_active {
            (p.text, theme::strong(ui.ctx(), 12.5))
        } else {
            (p.text_muted, font.clone())
        };
        let close_w = 24.0;
        let text_rect = Rect::from_min_max(
            pos2(tab_rect.left() + 16.0, tab_rect.top()),
            pos2(tab_rect.right() - close_w, tab_rect.bottom()),
        );
        draw_clipped(ui, text_rect, title, &text_font, text_colour);

        // Close button: an X drawn as two strokes, for the same reason the row
        // icons are drawn — the obvious glyphs are missing from egui's fonts.
        let close_rect = Rect::from_center_size(
            pos2(tab_rect.right() - 13.0, tab_rect.center().y),
            vec2(16.0, 16.0),
        );
        let close = ui.interact(close_rect, ui.id().with(("tabclose", group_id.0, i)), Sense::click());
        if close.hovered() {
            ui.painter().circle_filled(close_rect.center(), 9.0, p.hover);
        }
        // Only shown on the active tab or on hover: an X on every tab is
        // clutter, and they are the smallest targets in the strip.
        if is_active || response.hovered() || close.hovered() {
            let c = close_rect.center();
            let s = 3.2;
            let stroke = egui::Stroke::new(
                1.2,
                if close.hovered() { p.text } else { p.text_faint },
            );
            ui.painter()
                .line_segment([pos2(c.x - s, c.y - s), pos2(c.x + s, c.y + s)], stroke);
            ui.painter()
                .line_segment([pos2(c.x - s, c.y + s), pos2(c.x + s, c.y - s)], stroke);
        }

        // Close is checked first: it sits inside the tab, so a click on it
        // would otherwise also register as selecting the tab.
        if close.clicked() {
            action = Some(TabAction::Close(i));
        } else if response.drag_started() {
            action = Some(TabAction::BeginDrag(i));
        } else if response.clicked() {
            action = Some(TabAction::Select(i));
        } else if response.middle_clicked() {
            action = Some(TabAction::Close(i));
        }

        x += width + 4.0;
    }

    if overflowing {
        let arrow = Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(OVERFLOW_ARROW - 4.0, rect.height() - 12.0));
        let hidden_after = titles.len() - 1 - last;
        if overflow_arrow(ui, p, arrow, group_id, true, hidden_after) && hidden_after > 0 {
            action = Some(TabAction::Select(last + 1));
        }
    }

    // A tab dragged within its own strip: show where it would land, and move
    // it there on release.
    if let Some(d) = drag.filter(|d| d.group == group_id) {
        if let Some(pointer) = ui.ctx().pointer_interact_pos().filter(|p| rect.contains(*p)) {
            let to = landing(&drawn, pointer.x, d.index);
            // Before the tab it will displace when moving left, after it when
            // moving right — the gap the tab will drop into.
            let marker_x = drawn
                .iter()
                .find(|(i, _)| *i == to)
                .map(|(_, r)| if to > d.index { r.right() + 2.0 } else { r.left() - 2.0 })
                .unwrap_or_else(|| drawn.last().map_or(rect.left() + 8.0, |(_, r)| r.right() + 2.0));
            if to != d.index {
                ui.painter().vline(marker_x, rect.top() + 8.0..=rect.bottom() - 8.0, egui::Stroke::new(2.0, p.accent));
            }
            if ui.input(|i| i.pointer.any_released()) && to != d.index {
                action = Some(TabAction::Reorder { from: d.index, to });
            }
        }
    }

    // New-tab button, pinned to the right.
    let plus_rect = Rect::from_center_size(
        pos2(rect.right() - 18.0, rect.center().y),
        vec2(24.0, 24.0),
    );
    let plus = ui.interact(plus_rect, ui.id().with(("newtab", group_id.0)), Sense::click());
    if plus.hovered() {
        ui.painter().circle_filled(plus_rect.center(), 16.0, p.hover);
    }
    let c = plus_rect.center();
    let stroke = egui::Stroke::new(
        1.3,
        if plus.hovered() { p.text } else { p.text_faint },
    );
    ui.painter()
        .line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
    ui.painter()
        .line_segment([pos2(c.x, c.y - 5.0), pos2(c.x, c.y + 5.0)], stroke);
    if plus.clicked() {
        action = Some(TabAction::New);
    }

    let _ = theme::RADIUS_CARD;
    action
}

fn draw_clipped(ui: &Ui, rect: Rect, text: &str, font: &egui::FontId, color: egui::Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font.clone(), color);
    job.wrap = egui::text::TextWrapping {
        max_width: rect.width().max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    #[test]
    fn tabs_that_fit_keep_their_width_and_the_rest_share_what_is_left() {
        assert_eq!(fit_tabs(&[100.0, 100.0], 500.0), [100.0, 100.0]);
        let fitted = fit_tabs(&[88.0, 200.0, 200.0], 400.0);
        assert_eq!(fitted[0], 88.0, "a short title was cut");
        let total: f32 = fitted.iter().sum::<f32>() + 2.0 * TAB_GAP;
        assert!(total <= 400.0 + 0.01, "still too wide: {total}");
        // Never below the minimum, however many there are.
        assert!(fit_tabs(&[150.0; 40], 300.0).iter().all(|w| *w >= MIN_TAB));
    }

    #[test]
    fn the_run_of_visible_tabs_always_holds_the_active_one() {
        let widths = [MIN_TAB; 30];
        for active in [0, 14, 29] {
            let (first, last) = visible_run(&widths, active, 400.0);
            assert!(first <= active && active <= last, "active {active} not in {first}..={last}");
            assert!(last - first < 30);
        }
        assert_eq!(visible_run(&[100.0, 100.0], 1, 400.0), (0, 1));
    }

    #[test]
    fn a_dragged_tab_lands_where_it_is_dropped() {
        let at = |i: usize| (i, Rect::from_min_size(pos2(i as f32 * 100.0, 0.0), vec2(96.0, 20.0)));
        let drawn: Vec<(usize, Rect)> = (0..4).map(at).collect();
        // Tab 0 dropped just past the middle of tab 2 lands in slot 2.
        assert_eq!(landing(&drawn, 260.0, 0), 2);
        // Tab 3 dropped left of everything becomes the first.
        assert_eq!(landing(&drawn, 5.0, 3), 0);
        // Dropped where it started, it stays.
        assert_eq!(landing(&drawn, 150.0, 1), 1);
    }

    #[test]
    fn a_divider_follows_the_pointer_for_the_whole_drag() {
        // The app applies each SetRatio and redraws, which moves the gutter;
        // the drag has to survive that move every frame, not just the first.
        let mut layout = Layout::leaf(GroupId(1));
        layout.split(GroupId(1), Axis::Horizontal, GroupId(2));
        let area = rect(816.0, 400.0);
        let ctx = egui::Context::default();
        let mut time = 0.0;

        let mut frame = |layout: &mut Layout, events: Vec<egui::Event>| {
            time += 1.0 / 60.0;
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(
                egui::RawInput { time: Some(time), events, ..Default::default() },
                |ui| {
                    show(ui, layout, area, &Palette::DARK, GroupId(1), &mut actions, &mut |_, _, _| {});
                },
            );
            output.textures_delta.clear();
            for action in actions {
                if let PaneAction::SetRatio { target, ratio } = action {
                    layout.set_divider_ratio(target, ratio);
                }
            }
        };
        let press = |at: egui::Pos2, pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        // The gutter's centre: 400 points of pane, then 8 of the 16 gutter.
        let start = pos2(408.0, 200.0);
        frame(&mut layout, vec![egui::Event::PointerMoved(start)]);
        frame(&mut layout, vec![press(start, true)]);
        for step in 1..=10 {
            frame(&mut layout, vec![egui::Event::PointerMoved(start + vec2(16.0 * step as f32, 0.0))]);
        }
        frame(&mut layout, vec![press(start + vec2(160.0, 0.0), false)]);

        let Layout::Split { ratio, .. } = layout else { panic!("not a split") };
        assert!(ratio > 0.65, "the divider stopped following at ratio {ratio}");
    }

    #[test]
    fn a_horizontal_split_apportions_width_around_the_gutter() {
        let parent = rect(200.0 + GUTTER, 100.0);
        let (first, gutter, second) = subdivide(parent, Axis::Horizontal, 0.5);

        assert!((first.width() - 100.0).abs() < 0.01);
        assert!((second.width() - 100.0).abs() < 0.01);
        assert!((gutter.width() - GUTTER).abs() < 0.01);
        // No gaps and no overlap.
        assert!((gutter.left() - first.right()).abs() < 0.01);
        assert!((second.left() - gutter.right()).abs() < 0.01);
    }

    #[test]
    fn a_vertical_split_apportions_height() {
        let parent = rect(100.0, 200.0 + GUTTER);
        let (first, gutter, second) = subdivide(parent, Axis::Vertical, 0.5);

        assert!((first.height() - 100.0).abs() < 0.01);
        assert!((second.height() - 100.0).abs() < 0.01);
        assert!((gutter.height() - GUTTER).abs() < 0.01);
    }

    #[test]
    fn children_exactly_tile_the_parent() {
        for ratio in [0.1, 0.25, 0.5, 0.75, 0.9] {
            let parent = rect(500.0, 300.0);
            let (a, g, b) = subdivide(parent, Axis::Horizontal, ratio);
            let total = a.width() + g.width() + b.width();
            assert!(
                (total - parent.width()).abs() < 0.01,
                "ratio {ratio}: widths sum to {total}, expected {}",
                parent.width()
            );
        }
    }

    #[test]
    fn an_extreme_ratio_is_clamped_so_neither_pane_vanishes() {
        let (first, _, second) = subdivide(rect(500.0, 300.0), Axis::Horizontal, 0.0);
        assert!(first.width() > 0.0, "first pane must remain grabbable");
        assert!(second.width() > 0.0);
    }

    #[test]
    fn a_degenerate_parent_does_not_produce_negative_extents() {
        // Happens for a frame while the window is resized to nothing.
        let (first, _, second) = subdivide(rect(2.0, 2.0), Axis::Horizontal, 0.5);
        assert!(first.width() >= 0.0);
        assert!(second.width() >= 0.0);
    }
}
