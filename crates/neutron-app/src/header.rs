//! The header at the top of every pane: navigation, breadcrumb, title, filter.
//!
//! # Why this lives in the pane and not in a window-wide toolbar
//!
//! An earlier version put back/forward/up and the breadcrumb in a strip across
//! the top of the window. That is wrong twice over.
//!
//! Structurally, it is a lie: with split panes there is no single "current
//! location", so a window-wide breadcrumb has to pick one pane and silently
//! ignore the other. Every control it held was really a control on the focused
//! pane wearing a costume.
//!
//! Visually, a full-width filled toolbar strip is the most dated element a
//! desktop application can have, and both designs Neutron is modelled on put
//! exactly these controls — arrows, breadcrumb, page title — *inside* the
//! content card instead. Each pane now carries its own, which is both what the
//! reference designs look like and what a split-pane file manager actually
//! means.
//!
//! # Layout
//!
//! ```text
//!   ← → ↑   C: › Users › Razeen › Documents          [ search ]  👁 ⊞ ⊟
//!
//!   Documents
//!   ────────────────────────────────────────────────────────────────────
//! ```
//!
//! The large title is dropped on short panes, where the listing needs the
//! rows more than the pane needs a heading.

use std::path::PathBuf;

use egui::{Rect, Sense, Ui, pos2, vec2};
use neutron_core::{Axis, NodeId};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, RADIUS_CONTROL, RADIUS_SMALL};

/// Height of the navigation row.
const NAV_ROW: f32 = 40.0;
/// Height of the large title block, when it is shown.
const TITLE_ROW: f32 = 40.0;
/// Below this pane height the title is dropped in favour of more rows.
const TITLE_MIN_PANE_HEIGHT: f32 = 300.0;
/// Below this pane width the filter field is dropped; the breadcrumb and the
/// navigation arrows matter more on a narrow pane.
const FILTER_MIN_PANE_WIDTH: f32 = 460.0;
const FILTER_WIDTH: f32 = 180.0;

/// A control in the header was used.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderAction {
    Back,
    Forward,
    Up,
    Navigate(NodeId),
    SetFilter(String),
    ToggleHidden,
    Split(Axis),
}

/// Everything the header needs to draw itself. Passed as a struct rather than
/// eight positional arguments, all of which would be `bool`.
pub struct Header<'a> {
    pub location: &'a NodeId,
    pub can_back: bool,
    pub can_forward: bool,
    pub has_parent: bool,
    pub show_hidden: bool,
    pub filter: &'a str,
    /// Number of entries currently listed, shown beside the title.
    pub shown: usize,
    /// Total before filtering, so a narrowed listing can say so.
    pub total: usize,
}

/// The id of a pane's filter field.
///
/// Absolute rather than derived from the enclosing `Ui`, so `Ctrl+F` can focus
/// it from the app's key handler — which runs before any pane `Ui` exists and
/// therefore has nothing to derive from.
pub fn filter_id(group: neutron_core::GroupId) -> egui::Id {
    egui::Id::new(("neutron-filter", group.0))
}

/// Draws the header. Returns the first action taken this frame.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    group: neutron_core::GroupId,
    h: &Header<'_>,
) -> Option<HeaderAction> {
    let mut action = None;

    let full = ui.available_rect_before_wrap();
    let show_title = full.height() >= TITLE_MIN_PANE_HEIGHT;
    let show_filter = full.width() >= FILTER_MIN_PANE_WIDTH;

    let nav = Rect::from_min_size(full.min, vec2(full.width(), NAV_ROW));
    ui.advance_cursor_after_rect(nav);

    // --- navigation arrows ---
    //
    // Shortcuts are spelled out rather than drawn with arrow characters:
    // U+2190…U+2192 are not in the font egui bundles and render as empty boxes,
    // so "Alt+↑" reached the screen as "Alt+ □".
    let mut x = nav.left() + 10.0;
    for (glyph, enabled, act, tip) in [
        (
            Glyph::ArrowLeft,
            h.can_back,
            HeaderAction::Back,
            "Back (Alt+Left)",
        ),
        (
            Glyph::ArrowRight,
            h.can_forward,
            HeaderAction::Forward,
            "Forward (Alt+Right)",
        ),
        (
            Glyph::ArrowUp,
            h.has_parent,
            HeaderAction::Up,
            "Up (Alt+Up, Backspace)",
        ),
    ] {
        let r = Rect::from_center_size(pos2(x + 14.0, nav.center().y), vec2(28.0, 28.0));
        if icon_button(ui, p, r, glyph, enabled, false, tip) {
            action = Some(act);
        }
        x += 30.0;
    }

    // --- right-hand controls, laid out from the right edge inward ---
    let mut right = nav.right() - 8.0;

    for (vertical, tip) in [
        (true, "Split down (Ctrl+Shift+\\)"),
        (false, "Split right (Ctrl+\\)"),
    ] {
        let r = Rect::from_center_size(pos2(right - 14.0, nav.center().y), vec2(28.0, 28.0));
        if split_button(ui, p, r, vertical, tip) {
            action = Some(HeaderAction::Split(if vertical {
                Axis::Vertical
            } else {
                Axis::Horizontal
            }));
        }
        right -= 30.0;
    }

    let eye = Rect::from_center_size(pos2(right - 14.0, nav.center().y), vec2(28.0, 28.0));
    if icon_button(
        ui,
        p,
        eye,
        Glyph::Eye,
        true,
        h.show_hidden,
        "Show hidden files (Ctrl+H)",
    ) {
        action = Some(HeaderAction::ToggleHidden);
    }
    right -= 36.0;

    if show_filter {
        let field = Rect::from_min_max(
            pos2(right - FILTER_WIDTH, nav.center().y - 14.0),
            pos2(right, nav.center().y + 14.0),
        );
        if let Some(text) = filter_field(ui, p, group, field, h.filter) {
            action = Some(HeaderAction::SetFilter(text));
        }
        right -= FILTER_WIDTH + 8.0;
    }

    // --- breadcrumb, in whatever is left between the arrows and the controls ---
    let trail = Rect::from_min_max(pos2(x + 8.0, nav.top()), pos2(right, nav.bottom()));
    if trail.width() > 40.0 {
        if let Some(target) = breadcrumb(ui, p, trail, h.location) {
            action = Some(HeaderAction::Navigate(target));
        }
    }

    // --- large title ---
    if show_title {
        let row = Rect::from_min_size(
            pos2(full.left(), nav.bottom()),
            vec2(full.width(), TITLE_ROW),
        );
        ui.advance_cursor_after_rect(row);
        draw_title(ui, p, row, h);
    }

    action
}

/// The location's own name, set large, with a count beside it.
///
/// Straight from the reference designs, which both lead the content area with
/// the folder name at heading size. It is not redundant with the breadcrumb: the
/// breadcrumb is a path you navigate, this is a label that tells you at a glance
/// which pane you are looking at.
fn draw_title(ui: &Ui, p: &Palette, row: Rect, h: &Header<'_>) {
    let title = h.location.display_name();
    let painter = ui.painter();

    let galley = painter.layout_no_wrap(
        title,
        egui::FontId::proportional(19.0),
        p.text,
    );
    let baseline = pos2(row.left() + 16.0, row.center().y - galley.size().y / 2.0 + 2.0);
    let width = galley.size().x;
    painter.galley(baseline, galley, p.text);

    // Filtered listings say so; an unfiltered one just states the count, and a
    // silent "12 items" on a 4000-entry folder would be alarming.
    let count = if h.shown == h.total {
        format!("{} items", h.total)
    } else {
        format!("{} of {}", h.shown, h.total)
    };
    painter.text(
        pos2(baseline.x + width + 12.0, row.center().y + 2.0),
        egui::Align2::LEFT_CENTER,
        count,
        egui::FontId::proportional(11.0),
        p.text_faint,
    );
}

/// A square icon button. `active` gives it the held-down look used by the
/// hidden-files toggle.
fn icon_button(
    ui: &mut Ui,
    p: &Palette,
    rect: Rect,
    glyph: Glyph,
    enabled: bool,
    active: bool,
    tip: &str,
) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with(("iconbtn", glyph as u8, rect.left() as i32, rect.top() as i32)),
        if enabled { Sense::click() } else { Sense::hover() },
    );

    if active {
        ui.painter()
            .rect_filled(rect, RADIUS_CONTROL as f32, p.selection);
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }

    // Disabled controls are dimmed rather than hidden: a back arrow that
    // vanishes at the start of history makes the row's contents jump.
    let colour = if !enabled {
        p.text_faint
    } else if active || response.hovered() {
        p.text
    } else {
        p.text_muted
    };
    icons::draw(ui.painter(), rect.center(), glyph, colour);

    if enabled {
        response.clone().on_hover_text(tip);
    }
    enabled && response.clicked()
}

fn split_button(ui: &mut Ui, p: &Palette, rect: Rect, vertical: bool, tip: &str) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with(("split", vertical, rect.left() as i32, rect.top() as i32)),
        Sense::click(),
    );
    if response.hovered() {
        ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }
    let colour = if response.hovered() {
        p.text
    } else {
        p.text_muted
    };
    icons::split(ui.painter(), rect.center(), vertical, colour);
    response.clone().on_hover_text(tip);
    response.clicked()
}

/// The narrowing filter, drawn as a pill with a magnifier — the search control
/// from the reference design.
///
/// Returns the new text when it changed. The caller owns the string; this takes
/// a copy each frame because the pane is drawn from `&self`.
fn filter_field(
    ui: &mut Ui,
    p: &Palette,
    group: neutron_core::GroupId,
    rect: Rect,
    current: &str,
) -> Option<String> {
    let id = filter_id(group);
    let mut text = current.to_owned();

    ui.painter().rect(
        rect,
        RADIUS_CONTROL as f32,
        p.inset,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    icons::draw(
        ui.painter(),
        pos2(rect.left() + 15.0, rect.center().y),
        Glyph::Search,
        p.text_faint,
    );

    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 28.0, rect.top()),
        pos2(rect.right() - 8.0, rect.bottom()),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    child.set_clip_rect(text_rect);

    // No frame of its own: the pill behind it is the frame.
    let output = egui::TextEdit::singleline(&mut text)
        .id(id)
        .background_color(egui::Color32::TRANSPARENT)
        .desired_width(text_rect.width())
        .font(egui::FontId::proportional(12.0))
        .text_color(p.text)
        // Names the control, so an empty field is not an unexplained blank pill.
        .hint_text(egui::RichText::new("Filter").color(p.text_faint).size(12.0))
        .vertical_align(egui::Align::Center)
        .show(&mut child);

    // Escape leaves the field and clears it. Handled here rather than in the
    // app's key table because that table is skipped entirely while a text field
    // holds focus — which is exactly when this binding is wanted.
    if output.response.has_focus() && child.input(|i| i.key_pressed(egui::Key::Escape)) {
        child.memory_mut(|m| m.surrender_focus(id));
        return (!current.is_empty()).then(String::new);
    }

    (text != current).then_some(text)
}

/// Clickable path segments, elided from the left when they do not fit.
///
/// Elides the *left* because the tail is what identifies the location: given
/// `C:\Users\Razeen\Projects\Neutron\crates`, dropping `C:\Users` costs nothing
/// and dropping `Neutron\crates` costs everything.
fn breadcrumb(ui: &mut Ui, p: &Palette, rect: Rect, location: &NodeId) -> Option<NodeId> {
    let Some(path) = location.as_path() else {
        // A shell location has no path to split into ancestors — "This PC" is
        // one item, and the shell's own parent chain needs COM to walk, which
        // cannot happen while painting. The name alone is honest; the sidebar
        // is how you leave.
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            location.display_name(),
            egui::FontId::proportional(12.0),
            p.text,
        );
        return None;
    };

    // (label, path) root-first, accumulating components.
    let mut segments: Vec<(String, PathBuf)> = Vec::new();
    let mut acc = PathBuf::new();
    for component in path.components() {
        acc.push(component.as_os_str());
        // The root prints as `C:\`; trim the separator so it does not read
        // "C:\ › Users".
        let label = component
            .as_os_str()
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned();
        if !label.is_empty() {
            segments.push((label, acc.clone()));
        }
    }
    if segments.is_empty() {
        return None;
    }

    let font = egui::FontId::proportional(12.0);
    let chevron = 16.0;

    // Measure from the right, keeping segments while they fit.
    let widths: Vec<f32> = segments
        .iter()
        .map(|(label, _)| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), p.text)
                .size()
                .x
        })
        .collect();

    let mut first = 0;
    let mut used: f32 = widths.iter().sum::<f32>() + chevron * (segments.len() - 1) as f32;
    // The last segment always stays, however long it is — better to clip one
    // name than to render a breadcrumb of nothing but ellipses.
    while used > rect.width() && first + 1 < segments.len() {
        used -= widths[first] + chevron;
        first += 1;
    }
    let elided = first > 0;

    let mut action = None;
    let mut x = rect.left();

    if elided {
        ui.painter().text(
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "…",
            font.clone(),
            p.text_faint,
        );
        x += 12.0;
        icons::draw(
            ui.painter(),
            pos2(x + chevron / 2.0, rect.center().y),
            Glyph::ChevronRight,
            p.text_faint,
        );
        x += chevron;
    }

    let last = segments.len() - 1;
    for (i, (label, target)) in segments.iter().enumerate().skip(first) {
        // Separator before every segment but the first drawn. When the trail is
        // elided the leading chevron has already been drawn after the ellipsis.
        if i > first {
            icons::draw(
                ui.painter(),
                pos2(x + chevron / 2.0, rect.center().y),
                Glyph::ChevronRight,
                p.text_faint,
            );
            x += chevron;
        }

        let w = widths[i];
        let hit = Rect::from_min_max(
            pos2(x - 4.0, rect.center().y - 11.0),
            pos2(x + w + 4.0, rect.center().y + 11.0),
        );

        if i == last {
            // The current location is a label, not a link: clicking it would
            // reload the pane, which is what F5 is for.
            ui.painter().text(
                pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                font.clone(),
                p.text,
            );
        } else {
            let response = ui.interact(
                hit,
                ui.id().with(("crumb", i, x as i32)),
                Sense::click(),
            );
            if response.hovered() {
                ui.painter()
                    .rect_filled(hit, RADIUS_SMALL as f32, p.hover);
            }
            ui.painter().text(
                pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                font.clone(),
                if response.hovered() { p.text } else { p.text_muted },
            );
            if response.clicked() {
                action = Some(NodeId::Path(target.clone()));
            }
        }
        x += w;
    }

    let _ = theme::RADIUS_CARD;
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_pane_spends_its_height_on_rows() {
        // Four-way vertical splits leave very little height, and a 40pt
        // heading there would cost most of the visible listing.
        assert!(TITLE_MIN_PANE_HEIGHT > NAV_ROW + TITLE_ROW * 2.0);
    }

    #[test]
    fn the_header_never_eats_a_whole_pane() {
        // Even at the threshold where the title first appears, the header must
        // be a minority of the pane.
        let tallest = NAV_ROW + TITLE_ROW;
        assert!(
            tallest < TITLE_MIN_PANE_HEIGHT * 0.5,
            "header takes {tallest} of a {TITLE_MIN_PANE_HEIGHT}pt pane"
        );
    }

    #[test]
    fn every_pane_gets_a_distinct_filter_field() {
        // A shared id would make two panes' filter boxes the same widget:
        // typing in one would move the caret in the other.
        assert_ne!(
            filter_id(neutron_core::GroupId(1)),
            filter_id(neutron_core::GroupId(2))
        );
    }
}
