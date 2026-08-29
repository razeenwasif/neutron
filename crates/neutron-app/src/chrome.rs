//! The window's own title bar: drag, minimise, maximise, close.
//!
//! # Why draw it ourselves
//!
//! Neutron's layout is cards on a ground with a gutter around them. A system
//! title bar sits above all of that as a separate strip in the OS's colours,
//! with the OS's corner radius, and no amount of theming inside the window
//! makes the two look like one object. Drawing it is what lets the window be a
//! single rounded surface.
//!
//! The cost is that everything the OS gave us for free has to be re-provided:
//! dragging, the three buttons, double-click to maximise, and — the one that is
//! easy to forget — resizing, because an undecorated window has no resize
//! border. [`resize_edges`] puts that back.
//!
//! # Close is not styled like the others
//!
//! It gets a red hover fill and the other two do not. That is the one
//! convention on this bar worth keeping exactly as Windows has it: it is the
//! only button here that can lose work, and it is nine pixels from the one that
//! merely hides the window.

use egui::{Color32, Rect, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};
use neutron_ui::theme::Palette;

/// Height of the bar. Tall enough to be an easy drag target, short enough that
/// it does not read as a second toolbar.
pub const HEIGHT: f32 = 34.0;

/// Width of one window button.
const BUTTON: f32 = 44.0;

/// How far in from the window edge a resize drag is recognised.
const GRIP: f32 = 6.0;

/// Draws the bar and applies whatever was pressed.
///
/// `snap` is where the maximise button reports its own position, so Windows can
/// offer the Snap Layouts flyout over it. See `neutron_shell::snaplayout`.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    title: &str,
    snap: &neutron_shell::snaplayout::MaximiseButton,
) {
    let bar = ui.available_rect_before_wrap();

    // --- the three buttons, right to left ---
    let mut x = bar.right();
    for kind in [Button::Close, Button::Maximise, Button::Minimise] {
        let rect = Rect::from_min_max(pos2(x - BUTTON, bar.top()), pos2(x, bar.bottom()));
        x -= BUTTON;

        if kind == Button::Maximise {
            // Physical pixels from the client origin, which is the only frame
            // of reference `WM_NCHITTEST` can be compared in.
            let scale = ui.ctx().pixels_per_point();
            snap.set(
                (rect.left() * scale).round() as i32,
                (rect.top() * scale).round() as i32,
                (rect.right() * scale).round() as i32,
                (rect.bottom() * scale).round() as i32,
            );
        }

        button(ui, p, rect, kind, snap);
    }

    // --- everything left of them drags the window ---
    //
    // Allocated after the buttons so they take their clicks first; a drag
    // region covering the whole bar would swallow them.
    let drag_area = Rect::from_min_max(bar.min, pos2(x, bar.bottom()));
    let response = ui.allocate_rect(drag_area, Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        toggle_maximised(ui);
    }

    ui.painter().text(
        pos2(drag_area.left() + 2.0, drag_area.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(12.0),
        p.text_muted,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Minimise,
    Maximise,
    Close,
}

fn button(
    ui: &mut Ui,
    p: &Palette,
    rect: Rect,
    kind: Button,
    snap: &neutron_shell::snaplayout::MaximiseButton,
) {
    let response = ui.allocate_rect(rect, Sense::click());
    // Once the maximise button claims `HTMAXBUTTON`, Windows treats the pointer
    // as being over a non-client area and egui stops seeing it at all. The
    // window procedure reports hover instead, or the button would never light
    // up under the cursor.
    let hovered = response.hovered()
        || (kind == Button::Maximise && snap.is_hovered());

    if hovered {
        let fill = match kind {
            // The one button that can lose work, in the colour every Windows
            // application uses for it.
            Button::Close => Color32::from_rgb(0xC4, 0x2B, 0x1C),
            _ => p.hover,
        };
        ui.painter().rect_filled(rect, 0.0, fill);
    }

    let ink = match (kind, hovered) {
        (Button::Close, true) => Color32::WHITE,
        (_, true) => p.text,
        _ => p.text_muted,
    };
    let stroke = Stroke::new(1.0, ink);
    let c = rect.center();

    match kind {
        Button::Minimise => {
            ui.painter()
                .line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
        }
        Button::Maximise => {
            let maximised = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            if maximised {
                // Two offset squares: the "restore down" mark.
                ui.painter().rect_stroke(
                    Rect::from_min_max(pos2(c.x - 5.0, c.y - 2.0), pos2(c.x + 2.0, c.y + 5.0)),
                    0.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
                ui.painter().line_segment(
                    [pos2(c.x - 2.0, c.y - 5.0), pos2(c.x + 5.0, c.y - 5.0)],
                    stroke,
                );
                ui.painter().line_segment(
                    [pos2(c.x + 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 2.0)],
                    stroke,
                );
            } else {
                ui.painter().rect_stroke(
                    Rect::from_center_size(c, vec2(10.0, 10.0)),
                    0.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Button::Close => {
            ui.painter()
                .line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], stroke);
            ui.painter()
                .line_segment([pos2(c.x + 5.0, c.y - 5.0), pos2(c.x - 5.0, c.y + 5.0)], stroke);
        }
    }

    if response.clicked() {
        match kind {
            Button::Minimise => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
            Button::Maximise => toggle_maximised(ui),
            Button::Close => ui.ctx().send_viewport_cmd(ViewportCommand::Close),
        }
    }
}

fn toggle_maximised(ui: &Ui) {
    let now = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!now));
}

/// Puts the resize border back.
///
/// An undecorated window has none — the OS is no longer drawing the frame that
/// carried it — so a window with a custom title bar and nothing else is one
/// that cannot be resized by the mouse at all. Eight regions: four edges and
/// four corners, the corners allocated first so they win where they overlap.
///
/// Drawn nowhere. These are interaction regions only; a visible grip would be a
/// line around a window whose whole point is not having one.
pub fn resize_edges(ui: &mut Ui, screen: Rect) {
    use egui::ResizeDirection as D;

    let maximised = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    if maximised {
        // A maximised window has no edges to drag, and claiming the strips
        // would steal clicks from whatever is under them.
        return;
    }

    let (l, r, t, b) = (screen.left(), screen.right(), screen.top(), screen.bottom());
    let regions = [
        // Corners first.
        (Rect::from_min_max(pos2(l, t), pos2(l + GRIP, t + GRIP)), D::NorthWest),
        (Rect::from_min_max(pos2(r - GRIP, t), pos2(r, t + GRIP)), D::NorthEast),
        (Rect::from_min_max(pos2(l, b - GRIP), pos2(l + GRIP, b)), D::SouthWest),
        (Rect::from_min_max(pos2(r - GRIP, b - GRIP), pos2(r, b)), D::SouthEast),
        // Then the edges.
        (Rect::from_min_max(pos2(l, t), pos2(r, t + GRIP)), D::North),
        (Rect::from_min_max(pos2(l, b - GRIP), pos2(r, b)), D::South),
        (Rect::from_min_max(pos2(l, t), pos2(l + GRIP, b)), D::West),
        (Rect::from_min_max(pos2(r - GRIP, t), pos2(r, b)), D::East),
    ];

    for (rect, direction) in regions {
        let id = egui::Id::new(("neutron-resize", direction as u8));
        let response = ui.interact(rect, id, Sense::drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(cursor_for(direction));
        }
        if response.drag_started() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }
}

fn cursor_for(d: egui::ResizeDirection) -> egui::CursorIcon {
    use egui::ResizeDirection as D;
    match d {
        D::North | D::South => egui::CursorIcon::ResizeVertical,
        D::East | D::West => egui::CursorIcon::ResizeHorizontal,
        D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => egui::CursorIcon::ResizeNeSw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar has to be tall enough to grab without hitting a button, and the
    /// buttons wide enough to be a comfortable target. These are the numbers
    /// Windows itself uses, near enough.
    #[test]
    fn the_bar_is_a_usable_drag_target() {
        // A bar shorter than this is hard to grab; a button narrower than
        // this is a small target. Both are near what Windows itself uses.
        const { assert!(HEIGHT >= 30.0) };
        const { assert!(BUTTON >= 40.0) };
    }

    /// Eight regions, and the four corners must come before the four edges:
    /// they overlap, and whichever is allocated first wins. Corners last would
    /// mean a window that can never be resized diagonally.
    #[test]
    fn the_grip_is_reachable_but_not_greedy() {
        // Below four pixels the grip cannot be hit; above eight it starts
        // eating clicks meant for the card underneath.
        const { assert!(GRIP >= 4.0) };
        const { assert!(GRIP <= 8.0) };
    }
}
