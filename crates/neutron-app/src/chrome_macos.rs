//! macOS owns the rounded window, resize border, shadow, and traffic lights.
//! This reserved title strip keeps file controls clear of the native buttons.
use egui::{Rect, Sense, Ui, ViewportCommand, pos2};
use neutron_ui::theme::Palette;

/// Just enough to clear the traffic lights, which end about 22 points down:
/// with the gutter under it, the cards start a little below them rather than
/// leaving a band of empty ground across the top of the window.
pub const HEIGHT: f32 = 20.0;

/// The strip above the cards: somewhere to drag the window by, and nothing
/// else. No title is drawn — the window is transparent, so text here would sit
/// on whatever wallpaper is behind it. The OS still has the title, for the
/// Window menu and Mission Control.
pub fn show(
    ui: &mut Ui,
    _p: &Palette,
    _title: &str,
    _snap: &neutron_shell::snaplayout::MaximiseButton,
) {
    let bar = ui.available_rect_before_wrap();
    let drag_area = Rect::from_min_max(pos2(bar.left() + 78.0, bar.top()), bar.max);
    let response = ui.allocate_rect(drag_area, Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

// Native NSWindow resizing must receive the edge events, especially near the
// traffic lights. A transparent egui resize region would steal those clicks.
pub fn resize_edges(_ui: &mut Ui, _screen: Rect) {}
