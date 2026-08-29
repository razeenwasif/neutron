//! One modal, for the things that cannot be undone.
//!
//! # Why anything gets one
//!
//! Almost nothing in Neutron needs confirming. A local delete goes to the
//! Recycle Bin, a rename can be renamed back, and a paste can be deleted — the
//! application is full of operations whose answer to "are you sure?" is "you
//! can just undo it". Putting a dialog in front of those trains people to
//! dismiss dialogs, which is how the one that mattered gets dismissed too.
//!
//! What is left is a short list where the undo does not exist:
//!
//! * **Deleting on a server.** There is no server-side Recycle Bin. The same
//!   keystroke that is reversible locally destroys the file remotely, and those
//!   two should not feel identical.
//! * **Forgetting a server.** Takes the password out of Credential Manager, and
//!   the user may not know it any more.
//!
//! # Shape
//!
//! Cancel is the default and Escape takes it. The dangerous button is on the
//! right, is the one carrying the accent, and says what it will *do* — "Delete
//! 3 files" rather than "OK", because the label is the last chance to notice
//! the count is wrong.

use egui::{Align2, Color32, Sense, Ui, vec2};
use neutron_ui::theme::{self, Palette, RADIUS_CARD, RADIUS_CONTROL};

/// A pending question.
pub struct Confirm {
    pub title: String,
    /// One or two sentences. What will happen, and what cannot be undone.
    pub body: String,
    /// The label on the dangerous button. Names the action and its scale.
    pub proceed: String,
}

/// What the user did with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Still open.
    Waiting,
    Cancelled,
    Confirmed,
}

/// Draws the dialog over everything else.
pub fn show(ui: &mut Ui, p: &Palette, confirm: &Confirm) -> Answer {
    let screen = ui.ctx().content_rect();

    // A scrim, and one that takes clicks: without it a click just outside the
    // card lands on the listing underneath, and the thing being confirmed is
    // usually about whatever is selected there.
    ui.painter()
        .rect_filled(screen, 0.0, Color32::from_black_alpha(90));
    let _ = ui.allocate_rect(screen, Sense::click());

    let mut answer = Answer::Waiting;

    // Escape cancels. Checked before the buttons so a held Escape cannot land
    // on a button that has just appeared under the pointer.
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Answer::Cancelled;
    }

    egui::Area::new(egui::Id::new("neutron-confirm"))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, -40.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            theme::popover(p)
                .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
                .inner_margin(egui::Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.set_width(360.0);

                    ui.label(
                        egui::RichText::new(&confirm.title)
                            .size(15.0)
                            .color(p.text)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(&confirm.body)
                            .size(12.5)
                            .color(p.text_muted),
                    );
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        // Pushed right, with Cancel first: the safe option is
                        // where the eye lands and the dangerous one is the
                        // deliberate reach.
                        let buttons = 200.0;
                        let pad = (ui.available_width() - buttons).max(0.0);
                        ui.add_space(pad);

                        if flat(ui, p, "Cancel", false) {
                            answer = Answer::Cancelled;
                        }
                        ui.add_space(8.0);
                        if flat(ui, p, &confirm.proceed, true) {
                            answer = Answer::Confirmed;
                        }
                    });
                });
        });

    answer
}

/// A button. Two states only: the quiet one and the one that does the thing.
fn flat(ui: &mut Ui, p: &Palette, label: &str, danger: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(12.5),
        p.text,
    );
    let size = vec2(galley.size().x + 24.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let (fill, ink) = match (danger, response.hovered()) {
        (true, true) => (p.danger, Color32::WHITE),
        (true, false) => (p.danger, Color32::WHITE),
        (false, true) => (p.card_hover, p.text),
        (false, false) => (p.inset, p.text_muted),
    };
    ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.5),
        ink,
    );
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The label is the last chance to notice the count is wrong, so it has to
    /// say what will happen rather than "OK".
    #[test]
    fn a_proceed_label_names_the_action() {
        let c = Confirm {
            title: "Delete from files.example.com?".into(),
            body: "3 files will be deleted. A server has no Recycle Bin.".into(),
            proceed: "Delete 3 files".into(),
        };
        assert!(c.proceed.len() > 2, "{:?} says nothing", c.proceed);
        assert!(
            !c.proceed.eq_ignore_ascii_case("ok"),
            "a confirmation labelled OK confirms nothing in particular"
        );
        // The body has to explain the *irreversibility*, which is the only
        // reason the dialog exists.
        assert!(c.body.contains("Recycle Bin") || c.body.contains("undone"));
    }
}
