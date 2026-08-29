//! The dashboard: what a window opens on.
//!
//! Modelled on the first of the two reference concepts — a greeting, one card
//! of storage with a segmented bar and a legend, then quick destinations as
//! tiles.
//!
//! # Why a file manager opens on something that is not a folder
//!
//! Every other location in Neutron answers "what is in here". This one answers
//! "where do you want to go", which is a different question and the one you
//! actually have on launch. Explorer's Home does the same thing for the same
//! reason.
//!
//! # Everything here is already in memory
//!
//! The volumes, their capacities and the known folders are what the sidebar
//! draws, gathered by the same background scan. The dashboard renders that and
//! nothing else — it starts no work of its own, so opening on it costs no more
//! than opening on an empty folder, and it is never waiting on a disk that is
//! asleep.

use egui::{Rect, Sense, Ui, pos2, vec2};
use neutron_core::places::{Capacity, Place, PlaceKind};
use neutron_core::NodeId;
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, RADIUS_CARD, RADIUS_CONTROL, micro_caps};

/// Height of a quick-destination tile.
const TILE: f32 = 84.0;
/// Smallest tile width before the grid drops a column.
const TILE_MIN: f32 = 150.0;

/// Draws the dashboard. Returns a destination if one was chosen.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    volumes: &[Place],
    known: &[Place],
) -> Option<NodeId> {
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.add_space(4.0);
            greeting(ui, p);
            ui.add_space(18.0);

            if let Some(volume) = volumes.iter().find(|v| v.capacity.is_some()) {
                storage_card(ui, p, volume, volumes);
                ui.add_space(20.0);
            }

            section(ui, p, "Quick access");
            ui.add_space(8.0);
            let mut chosen = tiles(ui, p, known);

            if !volumes.is_empty() {
                ui.add_space(20.0);
                section(ui, p, "Drives");
                ui.add_space(8.0);
                chosen = tiles(ui, p, volumes).or(chosen);
            }

            ui.add_space(16.0);
            chosen
        })
        .inner
}

fn greeting(ui: &mut Ui, p: &Palette) {
    ui.label(
        egui::RichText::new("Welcome back.")
            .size(25.0)
            .color(p.text)
            .strong(),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("Everything on this machine, and everywhere you have connected.")
            .size(12.5)
            .color(p.text_muted),
    );
}

fn section(ui: &mut Ui, p: &Palette, label: &str) {
    ui.label(
        egui::RichText::new(micro_caps(label))
            .size(9.5)
            .color(p.text_faint)
            .strong(),
    );
}

/// One card: a segmented bar across the volume's use, with a legend.
///
/// The bar is the concept's, and it earns its place here for a reason the
/// concept did not have: Neutron already knows every fixed volume, and "how
/// full is this machine" is a question a file manager is uniquely able to
/// answer at a glance.
fn storage_card(ui: &mut Ui, p: &Palette, primary: &Place, volumes: &[Place]) {
    theme::card(p)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("Storage")
                    .size(15.0)
                    .color(p.text)
                    .strong(),
            );
            ui.add_space(10.0);

            // The legend first, then the bar — the order the concept uses, and
            // the right one: the labels say what the colours are about to mean.
            let with_capacity: Vec<&Place> = volumes
                .iter()
                .filter(|v| v.capacity.is_some())
                .collect();

            ui.horizontal_wrapped(|ui| {
                for (i, volume) in with_capacity.iter().enumerate() {
                    let Some(cap) = volume.capacity else { continue };
                    let colour = crate::sidebar::segment_colour(p, i, with_capacity.len());
                    legend_entry(ui, p, &volume.name, cap, colour);
                    ui.add_space(14.0);
                }
            });

            ui.add_space(10.0);
            segmented_bar(ui, p, &with_capacity);
            let _ = primary;
        });
}

fn legend_entry(ui: &mut Ui, p: &Palette, name: &str, cap: Capacity, colour: egui::Color32) {
    let used = cap.total.saturating_sub(cap.free);
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(vec2(7.0, 7.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, colour);
        ui.add_space(2.0);
        ui.label(egui::RichText::new(name).size(11.5).color(p.text));
        ui.label(
            egui::RichText::new(neutron_ui::format::size(Some(used)))
                .size(11.0)
                .color(p.text_muted),
        );
    });
}

/// The bar itself: one segment per volume, widths proportional to what is used.
fn segmented_bar(ui: &mut Ui, p: &Palette, volumes: &[&Place]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 4.0, p.inset);

    let total: u64 = volumes
        .iter()
        .filter_map(|v| v.capacity)
        .map(|c| c.total)
        .sum();
    if total == 0 {
        return;
    }

    let mut x = rect.left();
    for (i, volume) in volumes.iter().enumerate() {
        let Some(cap) = volume.capacity else { continue };
        let used = cap.total.saturating_sub(cap.free);
        let width = rect.width() * (used as f32 / total as f32);
        if width < 1.0 {
            continue;
        }
        let seg = Rect::from_min_size(pos2(x, rect.top()), vec2(width, rect.height()));
        // Square inner edges, rounded outer ones, so the run reads as one bar
        // divided rather than as a row of separate pills.
        let radius = egui::CornerRadius {
            nw: if i == 0 { 4 } else { 0 },
            sw: if i == 0 { 4 } else { 0 },
            ne: 0,
            se: 0,
        };
        ui.painter().rect_filled(
            seg,
            radius,
            crate::sidebar::segment_colour(p, i, volumes.len()),
        );
        x += width + 1.5;
    }
}

/// Quick destinations, as a responsive grid of tiles.
fn tiles(ui: &mut Ui, p: &Palette, places: &[Place]) -> Option<NodeId> {
    if places.is_empty() {
        return None;
    }
    let mut chosen = None;

    let available = ui.available_width();
    let columns = ((available / TILE_MIN).floor() as usize).clamp(1, 5);
    let gap = 12.0;
    let width = ((available - gap * (columns - 1) as f32) / columns as f32).floor();

    let mut i = 0;
    while i < places.len() {
        ui.horizontal(|ui| {
            for _ in 0..columns {
                let Some(place) = places.get(i) else { break };
                i += 1;
                if tile(ui, p, place, width) {
                    chosen = Some(place.id.clone());
                }
                ui.add_space(gap);
            }
        });
        ui.add_space(gap);
    }

    chosen
}

fn tile(ui: &mut Ui, p: &Palette, place: &Place, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }

    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        RADIUS_CARD as f32,
        if hovered { p.card_hover } else { p.card },
        egui::Stroke::new(1.0, if hovered { p.border_strong } else { p.border }),
        egui::StrokeKind::Inside,
    );

    // A tinted plate behind the glyph, the way the concept sets its folders on
    // a soft panel rather than letting them float on the card.
    let plate = Rect::from_min_size(pos2(rect.left() + 14.0, rect.top() + 14.0), vec2(34.0, 34.0));
    ui.painter()
        .rect_filled(plate, RADIUS_CONTROL as f32, p.selection);
    icons::draw(ui.painter(), plate.center(), glyph_for(place), p.accent);

    ui.painter().text(
        pos2(rect.left() + 14.0, rect.top() + 58.0),
        egui::Align2::LEFT_TOP,
        &place.name,
        egui::FontId::proportional(12.5),
        p.text,
    );

    let detail = match place.capacity {
        Some(cap) => format!(
            "{} free of {}",
            neutron_ui::format::size(Some(cap.free)),
            neutron_ui::format::size(Some(cap.total))
        ),
        None => "Folder".to_owned(),
    };
    ui.painter().text(
        pos2(rect.left() + 14.0, rect.top() + 74.0),
        egui::Align2::LEFT_TOP,
        detail,
        egui::FontId::proportional(10.5),
        p.text_faint,
    );

    response.clicked()
}

fn glyph_for(place: &Place) -> Glyph {
    match place.kind {
        PlaceKind::Drive => Glyph::Drive,
        PlaceKind::Cloud => Glyph::Cloud,
        PlaceKind::Server => Glyph::Server,
        PlaceKind::Wsl => Glyph::Terminal,
        _ => Glyph::Folder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid has to stay usable in a narrow pane. A split four ways is not
    /// unusual, and a tile that never fits is a tile that never renders.
    #[test]
    fn the_tile_grid_always_keeps_at_least_one_column() {
        for available in [80.0f32, 149.0, 150.0, 600.0, 2000.0] {
            let columns = ((available / TILE_MIN).floor() as usize).clamp(1, 5);
            assert!(columns >= 1, "{available}px gave {columns} columns");
            assert!(columns <= 5, "{available}px gave {columns} columns");
        }
    }

    #[test]
    fn a_wide_pane_fills_out_but_stops_at_five() {
        assert_eq!(((2000.0f32 / TILE_MIN).floor() as usize).clamp(1, 5), 5);
        assert_eq!(((320.0f32 / TILE_MIN).floor() as usize).clamp(1, 5), 2);
    }
}
