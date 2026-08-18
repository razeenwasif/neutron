//! The places sidebar.
//!
//! Modelled on the first reference design: a white card holding a wordmark, a
//! few short labelled groups of destinations, and a storage summary pinned to
//! the bottom.
//!
//! # Rules
//!
//! * **Every row has an icon.** A column of bare text reads as a list of
//!   strings; the glyph is what makes it read as a place you can go.
//! * **Group labels are tracked micro-caps.** Small enough to be structure
//!   rather than content, which is what lets four groups sit in one column
//!   without the sidebar looking like an outline.
//! * **The selected row is a pill, not a highlight bar.** A full-bleed
//!   highlight inside a rounded card fights the card's own shape.
//!
//! The storage panel at the bottom is the design's segmented capacity bar,
//! reinterpreted: one segment per fixed volume, sized by bytes used, over a
//! track that represents the combined capacity of the machine. It is the single
//! place in Neutron where colour is spent on decoration, and it earns it by
//! also being the only view that answers "how full is this machine".

use egui::{Color32, Rect, Sense, Ui, pos2, vec2};
use neutron_core::NodeId;
use neutron_core::places::{Place, PlaceKind};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, NAV_HEIGHT, RADIUS_CONTROL, RADIUS_SMALL, ThemeMode, micro_caps};

/// Space reserved at the bottom for the storage panel and the theme toggle.
const FOOTER_HEIGHT: f32 = 96.0;

/// The wordmark at the top of the sidebar.
pub fn brand(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());

    // The brand mark: rounded square with glowing purple gradient, folder glyph, and subtle glass highlight.
    let mark = Rect::from_min_size(pos2(rect.left() + 2.0, rect.center().y - 13.0), vec2(26.0, 26.0));
    let mark_shadow = egui::epaint::Shadow {
        offset: [0, 2],
        blur: 14,
        spread: 0,
        color: p.selection,
    };
    ui.painter().add(mark_shadow.as_shape(mark, egui::CornerRadius::same(7)));
    ui.painter().rect_filled(mark, 7.0, p.accent_pressed);
    theme::glass_highlight(ui.painter(), mark, egui::CornerRadius::same(7));
    icons::draw(ui.painter(), mark.center(), Glyph::Folder, Color32::WHITE);

    ui.painter().text(
        pos2(mark.right() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Neutron",
        egui::FontId::proportional(15.0),
        p.text,
    );
}

/// A group heading.
pub fn section(ui: &mut Ui, p: &Palette, label: &str) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        micro_caps(label),
        egui::FontId::proportional(9.5),
        p.text_faint,
    );
    ui.add_space(2.0);
}

/// One destination. Returns true when it was clicked.
pub fn row(ui: &mut Ui, p: &Palette, place: &Place, current: Option<&NodeId>) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }

    let selected = current == Some(&place.id);
    let pill = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.right(), rect.bottom()));

    if selected {
        // Translucent selection wash plus subtle border
        ui.painter().rect(
            pill,
            RADIUS_CONTROL as f32,
            p.selection,
            egui::Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
        // Active indicator: glowing vertical accent bar on the left edge (matches Aero)
        let bar_h = (pill.height() * 0.58).round();
        let bar = Rect::from_min_size(
            pos2(pill.left() + 1.0, (pill.center().y - bar_h / 2.0).round()),
            vec2(3.0, bar_h),
        );
        ui.painter().rect_filled(bar, 1.5, p.accent);
    } else if response.hovered() {
        ui.painter().rect_filled(pill, RADIUS_CONTROL as f32, p.hover);
    }

    let colour = if selected { p.text } else { p.text_muted };
    icons::draw(
        ui.painter(),
        pos2(rect.left() + 18.0, rect.center().y),
        glyph_for(place),
        if selected { p.accent } else { p.icon },
    );

    // Clipped through a layout job so a long volume label ellipsises rather
    // than running under the sidebar's right edge.
    let mut job = egui::text::LayoutJob::simple_singleline(
        place.name.clone(),
        egui::FontId::proportional(12.5),
        colour,
    );
    job.wrap = egui::text::TextWrapping {
        max_width: (rect.width() - 44.0).max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );

    if let Some(cap) = place.capacity {
        let free = neutron_ui::format::size(Some(cap.free));
        let total = neutron_ui::format::size(Some(cap.total));
        response
            .clone()
            .on_hover_text(format!("{free} free of {total}"));
    }

    response.clicked()
}

fn glyph_for(place: &Place) -> Glyph {
    match place.kind {
        PlaceKind::Drive => Glyph::Drive,
        PlaceKind::Cloud => Glyph::Cloud,
        PlaceKind::Wsl => Glyph::Terminal,
        PlaceKind::Shell => Glyph::Diamond,
        // Home gets its own glyph; the other known folders are folders.
        PlaceKind::KnownFolder if place.name == "Home" => Glyph::Home,
        PlaceKind::KnownFolder => Glyph::Folder,
    }
}

/// Height the footer needs, so the scrolling area above it can be sized.
pub fn footer_height() -> f32 {
    FOOTER_HEIGHT
}

/// The storage panel and the theme toggle, pinned to the bottom of the card.
///
/// Returns true when the theme toggle was clicked. The theme lives here rather
/// than in a pane header because it is the one control in the application that
/// is not about a location — putting it beside the storage summary keeps every
/// pane header purely about the folder it is showing.
pub fn footer(ui: &mut Ui, p: &Palette, rect: Rect, drives: &[Place], mode: ThemeMode) -> bool {
    let mut toggled = false;

    // --- theme toggle, on its own row under the panel ---
    let toggle = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 28.0), vec2(28.0, 28.0));
    let response = ui.interact(toggle, ui.id().with("theme"), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(toggle, RADIUS_CONTROL as f32, p.hover);
    }
    let (glyph, tip) = match mode {
        ThemeMode::Dark => (Glyph::Sun, "Switch to light (Ctrl+D)"),
        ThemeMode::Light => (Glyph::Moon, "Switch to dark (Ctrl+D)"),
    };
    icons::draw(
        ui.painter(),
        toggle.center(),
        glyph,
        if response.hovered() { p.text } else { p.icon },
    );
    if response.clicked() {
        toggled = true;
    }
    response.on_hover_text(tip);

    let panel = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 36.0));
    storage_panel(ui, p, panel, drives);

    toggled
}

/// Combined capacity across the fixed volumes, as a segmented bar.
fn storage_panel(ui: &Ui, p: &Palette, rect: Rect, drives: &[Place]) {
    let painter = ui.painter();

    painter.rect(
        rect,
        RADIUS_CONTROL as f32,
        p.inset,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    painter.text(
        pos2(rect.left() + 12.0, rect.top() + 13.0),
        egui::Align2::LEFT_CENTER,
        micro_caps("Storage"),
        egui::FontId::proportional(9.5),
        p.text_faint,
    );

    // Only volumes that reported a capacity: a card reader with no media has
    // none, and counting it as zero would drag the total down for no reason.
    let with_capacity: Vec<_> = drives.iter().filter_map(|d| d.capacity).collect();
    let total: u64 = with_capacity.iter().map(|c| c.total).sum();
    let free: u64 = with_capacity.iter().map(|c| c.free).sum();

    let track = Rect::from_min_max(
        pos2(rect.left() + 12.0, rect.top() + 24.0),
        pos2(rect.right() - 12.0, rect.top() + 32.0),
    );
    painter.rect_filled(track, 4.0, p.hover);

    if total > 0 {
        // One segment per volume, laid left to right in the order the sidebar
        // lists them, so a segment can be matched to a drive by eye.
        let mut x = track.left();
        for (i, cap) in with_capacity.iter().enumerate() {
            let used = cap.total.saturating_sub(cap.free);
            let w = track.width() * (used as f64 / total as f64) as f32;
            if w <= 0.5 {
                continue;
            }
            let seg = Rect::from_min_max(pos2(x, track.top()), pos2(x + w, track.bottom()));
            painter.rect_filled(seg, 4.0, segment_colour(p, i, with_capacity.len()));
            x += w;
        }

        painter.text(
            pos2(rect.left() + 12.0, rect.bottom() - 12.0),
            egui::Align2::LEFT_CENTER,
            format!(
                "{} free of {}",
                neutron_ui::format::size(Some(free)),
                neutron_ui::format::size(Some(total))
            ),
            egui::FontId::proportional(11.0),
            p.text_muted,
        );
    } else {
        painter.text(
            pos2(rect.left() + 12.0, rect.bottom() - 12.0),
            egui::Align2::LEFT_CENTER,
            "Scanning volumes…",
            egui::FontId::proportional(11.0),
            p.text_faint,
        );
    }
}

/// A ramp across the accent's hue, light to dark.
///
/// Deliberately a ramp of one colour rather than the reference design's four
/// unrelated hues: those are categories of one store and read as a legend,
/// whereas these are volumes of one machine and reading them as a single
/// quantity split into parts is more truthful — and it keeps the sidebar from
/// becoming the most colourful thing on screen.
fn segment_colour(p: &Palette, index: usize, count: usize) -> Color32 {
    if count <= 1 {
        return p.accent;
    }
    // 0.0 at the first segment, 1.0 at the last.
    let t = index as f32 / (count - 1) as f32;
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let (from, to) = (p.accent, p.accent_pressed);
    Color32::from_rgb(
        lerp(from.r(), to.r()),
        lerp(from.g(), to.g()),
        lerp(from.b(), to.b()),
    )
}

/// A disabled row, for providers that are not connected yet.
pub fn placeholder_row(ui: &mut Ui, p: &Palette, label: &str, glyph: Glyph, why: &str) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_HEIGHT), Sense::hover());
    icons::draw(
        ui.painter(),
        pos2(rect.left() + 18.0, rect.center().y),
        glyph,
        p.text_faint,
    );
    ui.painter().text(
        pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        p.text_faint,
    );
    let _ = RADIUS_SMALL;
    response.on_hover_text(why);
}

#[cfg(test)]
mod tests {
    use super::*;
    use neutron_core::places::Capacity;
    use neutron_ui::theme::Palette;

    fn place(name: &str, kind: PlaceKind) -> Place {
        Place {
            name: name.to_owned(),
            id: NodeId::Path(std::path::PathBuf::from("/x")),
            kind,
            capacity: None,
        }
    }

    #[test]
    fn each_place_kind_gets_its_own_glyph() {
        assert_eq!(glyph_for(&place("Home", PlaceKind::KnownFolder)), Glyph::Home);
        assert_eq!(
            glyph_for(&place("Downloads", PlaceKind::KnownFolder)),
            Glyph::Folder
        );
        assert_eq!(glyph_for(&place("C:", PlaceKind::Drive)), Glyph::Drive);
        assert_eq!(glyph_for(&place("OneDrive", PlaceKind::Cloud)), Glyph::Cloud);
        // The whole point of pinning WSL is that it is recognisable at a
        // glance; sharing the folder glyph would defeat that.
        assert_eq!(
            glyph_for(&place("Ubuntu-24.04", PlaceKind::Wsl)),
            Glyph::Terminal
        );
    }

    #[test]
    fn a_single_volume_uses_the_plain_accent() {
        // Guards the divide-by-zero in the ramp.
        assert_eq!(segment_colour(&Palette::LIGHT, 0, 1), Palette::LIGHT.accent);
    }

    #[test]
    fn the_segment_ramp_spans_the_accent_range() {
        let p = Palette::LIGHT;
        assert_eq!(segment_colour(&p, 0, 4), p.accent);
        assert_eq!(segment_colour(&p, 3, 4), p.accent_pressed);
        // Intermediate segments must actually differ, or the bar reads as one
        // block and the per-volume split is lost.
        assert_ne!(segment_colour(&p, 1, 4), segment_colour(&p, 2, 4));
    }

    #[test]
    fn capacity_totals_ignore_volumes_that_did_not_report() {
        // An empty card reader has no capacity; treating it as zero total
        // would be harmless, but treating it as a zero-free volume would show
        // the machine as full.
        let drives = [
            Place {
                capacity: Some(Capacity {
                    total: 1000,
                    free: 400,
                }),
                ..place("C:", PlaceKind::Drive)
            },
            place("E:", PlaceKind::Drive),
        ];
        let with: Vec<_> = drives.iter().filter_map(|d| d.capacity).collect();
        assert_eq!(with.len(), 1);
        assert_eq!(with.iter().map(|c| c.total).sum::<u64>(), 1000);
    }
}

/// A row that starts something rather than navigating somewhere.
///
/// Visually distinct from a destination: the accent glyph says this is a
/// control, not a place you can already go.
pub fn action_row(ui: &mut Ui, p: &Palette, label: &str, glyph: Glyph) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_HEIGHT), Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }

    icons::draw(
        ui.painter(),
        pos2(rect.left() + 18.0, rect.center().y),
        glyph,
        p.accent,
    );
    ui.painter().text(
        pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        if response.hovered() { p.text } else { p.text_muted },
    );

    response.clicked()
}
