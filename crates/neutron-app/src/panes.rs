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

            if let Some(new_ratio) = divider(ui, gutter, *axis, rect, p) {
                // Ratio is addressed by the split's first leaf, matching how
                // `Layout::set_ratio` locates the node.
                if let Some(target) = first.groups().first().copied() {
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
fn draw_card(ui: &Ui, rect: Rect, p: &Palette, focused: bool) {
    let radius = egui::CornerRadius::same(RADIUS_CARD);

    let shadow = theme::card_shadow(p);
    ui.painter().add(shadow.as_shape(rect, radius));

    ui.painter().rect_filled(rect, radius, p.card);
    theme::glass_highlight(ui.painter(), rect, radius);

    // One hairline, the same in both states. An accent ring plus a glow around
    // the focused pane put the loudest colour on screen around the largest
    // element — it read as a selection box rather than as a pane, and with a
    // single pane open it framed the entire window.
    //
    // Focus still has to be visible with several panes open. It is carried by
    // the active tab's accent underline in the tab strip, which is small,
    // unambiguous, and already there.
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, if focused { p.border_strong } else { p.border }),
        egui::StrokeKind::Inside,
    );
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
fn divider(ui: &mut Ui, rect: Rect, axis: Axis, parent: Rect, p: &Palette) -> Option<f32> {
    let id = ui.id().with(("divider", rect.min.x as i32, rect.min.y as i32));
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
pub const TAB_STRIP_HEIGHT: f32 = 38.0;

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
pub fn tab_strip(
    ui: &mut Ui,
    group_id: GroupId,
    titles: &[String],
    active: usize,
    focused: bool,
    drag: Option<TabDrag>,
    p: &Palette,
) -> Option<TabAction> {
    let mut action = None;

    let full = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(full.min, vec2(full.width(), TAB_STRIP_HEIGHT));
    ui.advance_cursor_after_rect(rect);

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
    let font = egui::FontId::proportional(12.0);

    for (i, title) in titles.iter().enumerate() {
        let is_active = i == active;

        let galley = ui
            .painter()
            .layout_no_wrap(title.clone(), font.clone(), p.text);
        let width = (galley.size().x + 40.0).clamp(88.0, 200.0);

        let tab_rect =
            Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(width, rect.height() - 12.0));
        if tab_rect.right() > rect.right() - 34.0 {
            // Out of room. Overflow handling (scroll or menu) is a later
            // refinement; dropping tabs is better than drawing them over the
            // new-tab button.
            break;
        }

        let response = ui.interact(
            tab_rect,
            ui.id().with(("tab", group_id.0, i)),
            Sense::click_and_drag(),
        );
        let dragging_this = drag == Some(TabDrag { group: group_id, index: i });

        if is_active {
            ui.painter()
                .rect_filled(tab_rect, RADIUS_CONTROL as f32, p.card_hover);
            // A short accent underline marks the active tab of the *focused*
            // pane only, so two panes cannot both look current.
            if focused {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        pos2(tab_rect.left() + 12.0, tab_rect.bottom() - 2.0),
                        pos2(tab_rect.right() - 12.0, tab_rect.bottom()),
                    ),
                    1.0,
                    p.accent,
                );
            }
        } else if response.hovered() {
            ui.painter()
                .rect_filled(tab_rect, RADIUS_CONTROL as f32, p.hover);
        }

        if dragging_this {
            // Ghost the source tab rather than removing it: reflowing the strip
            // mid-drag moves every other tab out from under the cursor.
            ui.painter()
                .rect_filled(tab_rect, RADIUS_CONTROL as f32, p.hover);
        }

        let text_colour = if is_active { p.text } else { p.text_muted };
        let close_w = 22.0;
        let text_rect = Rect::from_min_max(
            pos2(tab_rect.left() + 12.0, tab_rect.top()),
            pos2(tab_rect.right() - close_w, tab_rect.bottom()),
        );
        draw_clipped(ui, text_rect, title, &font, text_colour);

        // Close button: an X drawn as two strokes, for the same reason the row
        // icons are drawn — the obvious glyphs are missing from egui's fonts.
        let close_rect = Rect::from_center_size(
            pos2(tab_rect.right() - 13.0, tab_rect.center().y),
            vec2(16.0, 16.0),
        );
        let close = ui.interact(close_rect, ui.id().with(("tabclose", group_id.0, i)), Sense::click());
        if close.hovered() {
            ui.painter().rect_filled(close_rect, 4.0, p.hover);
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

    // New-tab button, pinned to the right.
    let plus_rect = Rect::from_center_size(
        pos2(rect.right() - 18.0, rect.center().y),
        vec2(24.0, 24.0),
    );
    let plus = ui.interact(plus_rect, ui.id().with(("newtab", group_id.0)), Sense::click());
    if plus.hovered() {
        ui.painter()
            .rect_filled(plus_rect, RADIUS_CONTROL as f32, p.hover);
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
