//! The places sidebar.
//!
//! A pane holding the wordmark, a way into the finder, a few labelled groups of
//! destinations, and a storage card pinned to the bottom.
//!
//! # Rules
//!
//! * **Every row has an icon.** A column of bare text reads as a list of
//!   strings; the glyph is what makes it read as a place you can go.
//! * **Group labels are small spaced capitals.** Structure rather than
//!   content, which is what lets several groups share one column without the
//!   sidebar looking like an outline.
//! * **The place you are in is a soft pill.** An accent wash, a faint ring of
//!   the accent and a dot at the end — no bar down the side, no hard edge.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use neutron_core::NodeId;
use neutron_core::places::{Place, PlaceKind};
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, ThemeMode, micro_caps};

/// Height of a destination row.
const ROW: f32 = 36.0;
/// Room the footer needs: the storage card, a gap, the button row.
const FOOTER_HEIGHT: f32 = 122.0;

/// The wordmark: the app's own icon and its name.
pub fn brand(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());

    let mark = Rect::from_min_size(pos2(rect.left() + 6.0, rect.center().y - 16.0), vec2(32.0, 32.0));
    let texture: egui::TextureHandle = ui
        .ctx()
        .data(|data| data.get_temp(egui::Id::new("neutron-brand-icon")))
        .unwrap_or_else(|| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [256, 256],
                include_bytes!("../../../assets/icon_256.rgba"),
            );
            let texture = ui.ctx().load_texture("neutron-brand", image, egui::TextureOptions::LINEAR);
            ui.ctx()
                .data_mut(|data| data.insert_temp(egui::Id::new("neutron-brand-icon"), texture.clone()));
            texture
        });
    ui.painter()
        .image(texture.id(), mark, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

    ui.painter().text(
        pos2(mark.right() + 11.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Neutron",
        theme::strong(ui.ctx(), 16.0),
        p.text,
    );
}

/// A well that opens the finder: the way in to searching, for whoever has
/// not learned its shortcut yet. Returns whether it was clicked.
pub fn find_pill(ui: &mut Ui, p: &Palette) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let corners = CornerRadius::same(14);
    theme::paint_well(ui.painter(), rect, corners, p);
    if response.hovered() {
        ui.painter().rect_filled(rect, corners, p.hover);
    }

    icons::draw(ui.painter(), pos2(rect.left() + 20.0, rect.center().y), Glyph::Search, p.text_faint);
    ui.painter().text(
        pos2(rect.left() + 36.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Find anything",
        egui::FontId::proportional(13.0),
        if response.hovered() { p.text_muted } else { p.text_faint },
    );
    key_cap(ui, p, pos2(rect.right() - 10.0, rect.center().y), if cfg!(target_os = "macos") { "⌘P" } else { "Ctrl+P" });

    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Find anything"));
    response.clicked()
}

/// A key cap, right-aligned on `right_centre`. Shared with the pane footer.
pub fn key_cap(ui: &Ui, p: &Palette, right_centre: egui::Pos2, label: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::strong(ui.ctx(), 11.0), p.text_muted);
    let size = galley.size() + vec2(14.0, 4.0);
    let cap = Rect::from_min_size(pos2(right_centre.x - size.x, right_centre.y - size.y / 2.0), size);
    ui.painter().rect_filled(cap, CornerRadius::same(8), p.elevated);
    ui.painter().galley(
        pos2(cap.left() + 7.0, cap.center().y - galley.size().y / 2.0),
        galley,
        p.text_muted,
    );
}

/// A group label: small spaced capitals in the faint tone.
pub fn section(ui: &mut Ui, p: &Palette, label: &str) {
    ui.add_space(14.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        micro_caps(label),
        theme::strong(ui.ctx(), 10.0),
        p.text_faint,
    );
    ui.add_space(4.0);
}

/// What a click on a row asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowHit {
    None,
    Open,
    Menu(egui::Pos2),
    /// The eject button on a volume's row.
    Eject,
}

impl RowHit {
    fn of(response: &egui::Response, rect: Rect) -> Self {
        if response.clicked() {
            RowHit::Open
        } else if response.secondary_clicked() {
            RowHit::Menu(
                response
                    .interact_pointer_pos()
                    .unwrap_or_else(|| rect.center()),
            )
        } else {
            RowHit::None
        }
    }
}

/// One destination.
pub fn row(ui: &mut Ui, p: &Palette, place: &Place, current: Option<&NodeId>) -> RowHit {
    drive_row(ui, p, place, current, false)
}

/// One destination, with an eject button at its end when `ejectable`, as the
/// Finder puts one beside a disk that can be taken out.
pub fn drive_row(ui: &mut Ui, p: &Palette, place: &Place, current: Option<&NodeId>, ejectable: bool) -> RowHit {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if !ui.is_rect_visible(rect) {
        return RowHit::of(&response, rect);
    }

    let selected = current == Some(&place.id);
    let corners = CornerRadius::same(12);

    if selected {
        ui.painter().rect_filled(rect, corners, p.selection);
        ui.painter().rect_stroke(
            rect,
            corners,
            Stroke::new(1.0, theme::with_alpha(p.accent, 51)),
            egui::StrokeKind::Inside,
        );
        // The eject button takes the dot's place on a disk that has one.
        if !ejectable {
            ui.painter()
                .circle_filled(pos2(rect.right() - 14.0, rect.center().y), 3.0, p.accent);
        }
    } else if response.hovered() {
        ui.painter().rect_filled(rect, corners, p.hover);
    }

    // The Trash borrows the action rail's glyph, which is drawn larger.
    let glyph = glyph_for(place);
    icons::draw_scaled(
        ui.painter(),
        pos2(rect.left() + 19.0, rect.center().y),
        glyph,
        if selected { p.accent } else { p.icon },
        if glyph == Glyph::Trash { 0.8 } else { 1.0 },
    );

    let (font, colour) = if selected {
        (theme::strong(ui.ctx(), 13.5), p.text)
    } else {
        (egui::FontId::proportional(13.5), p.text_soft())
    };
    let mut job = egui::text::LayoutJob::simple_singleline(place.name.clone(), font, colour);
    job.wrap = egui::text::TextWrapping {
        max_width: (rect.width() - 62.0).max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left() + 39.0, rect.center().y - galley.size().y / 2.0),
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

    if ejectable {
        let button = Rect::from_center_size(pos2(rect.right() - 17.0, rect.center().y), vec2(24.0, 24.0));
        let eject = ui
            .interact(button, ui.id().with(("eject", &place.id)), Sense::click())
            .on_hover_text(format!("Eject {}", place.name));
        if eject.hovered() {
            ui.painter().circle_filled(button.center(), 12.0, p.hover);
        }
        icons::draw(
            ui.painter(),
            button.center(),
            Glyph::Eject,
            if eject.hovered() { p.text } else { p.text_faint },
        );
        if eject.clicked() {
            return RowHit::Eject;
        }
    }

    RowHit::of(&response, rect)
}

/// The well-known Windows shell roots, by their parsing names.
fn shell_glyph(place: &Place) -> Glyph {
    match place.id.parsing_name().unwrap_or_default() {
        "::{645FF040-5081-101B-9F08-00AA002F954E}" => Glyph::Trash,
        "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}" => Glyph::Drive,
        "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}" => Glyph::Server,
        _ => Glyph::Diamond,
    }
}

fn glyph_for(place: &Place) -> Glyph {
    if place.id == NodeId::Home {
        return Glyph::Dashboard;
    }
    match place.kind {
        PlaceKind::Drive => Glyph::Drive,
        PlaceKind::Cloud => Glyph::Cloud,
        PlaceKind::Wsl => Glyph::Terminal,
        PlaceKind::Shell => shell_glyph(place),
        PlaceKind::Server => Glyph::Server,
        PlaceKind::KnownFolder => match place.name.as_str() {
            "Home" => Glyph::Home,
            "Desktop" => Glyph::Desktop,
            "Downloads" => Glyph::Download,
            "Documents" => Glyph::Document,
            "Pictures" => Glyph::Image,
            "Music" => Glyph::Music,
            "Movies" | "Videos" => Glyph::Film,
            "Applications" => Glyph::Grid,
            "Trash" => Glyph::Trash,
            "AirDrop" => Glyph::AirDrop,
            _ => Glyph::Folder,
        },
    }
}

pub fn footer_height() -> f32 {
    FOOTER_HEIGHT
}

/// What the footer was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    None,
    Connect,
    ToggleTheme,
}

/// The storage card and, under it, Connect and the theme switch.
pub fn footer(ui: &mut Ui, p: &Palette, rect: Rect, drives: &[Place], mode: ThemeMode) -> FooterAction {
    let mut action = FooterAction::None;

    let card = Rect::from_min_size(rect.min, vec2(rect.width(), 76.0));
    storage_card(ui, p, card, drives);

    let buttons = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 34.0), rect.max);
    let toggle = Rect::from_min_size(pos2(buttons.right() - 34.0, buttons.top()), vec2(34.0, 34.0));
    let connect = Rect::from_min_max(buttons.min, pos2(toggle.left() - 6.0, buttons.bottom()));

    let response = ui.interact(connect, ui.id().with("sidebar-connect"), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(connect, CornerRadius::same(12), p.hover);
    }
    let label = ui.painter().layout_no_wrap(
        "Connect".to_owned(),
        egui::FontId::proportional(12.5),
        if response.hovered() { p.text } else { p.text_muted },
    );
    let width = 16.0 + 8.0 + label.size().x;
    let left = connect.center().x - width / 2.0;
    icons::draw(
        ui.painter(),
        pos2(left + 8.0, connect.center().y),
        Glyph::Plus,
        if response.hovered() { p.text } else { p.text_muted },
    );
    ui.painter()
        .galley(pos2(left + 24.0, connect.center().y - label.size().y / 2.0), label, p.text_muted);
    if response.clicked() {
        action = FooterAction::Connect;
    }
    response.on_hover_text("Connect to a server");

    let response = ui.interact(toggle, ui.id().with("theme"), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(toggle, CornerRadius::same(12), p.hover);
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
        action = FooterAction::ToggleTheme;
    }
    response.on_hover_text(neutron_ui::format::shortcut(tip));

    action
}

/// The main volume's name, how full it is, and how much is left.
///
/// The first volume that reported a capacity — the startup disk on a Mac,
/// listed first by the scan. The dashboard shows every volume; this is the
/// one figure worth keeping in sight everywhere.
fn storage_card(ui: &Ui, p: &Palette, rect: Rect, drives: &[Place]) {
    let painter = ui.painter();
    let corners = CornerRadius::same(theme::RADIUS_WELL);
    painter.rect_filled(rect, corners, Color32::from_white_alpha(6));
    painter.rect_stroke(rect, corners, Stroke::new(1.0, Color32::from_white_alpha(10)), egui::StrokeKind::Inside);

    let Some((name, cap)) = drives.iter().find_map(|d| d.capacity.map(|c| (d.name.as_str(), c))) else {
        painter.text(
            pos2(rect.left() + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Scanning volumes…",
            egui::FontId::proportional(11.5),
            p.text_faint,
        );
        return;
    };

    let used = cap.total.saturating_sub(cap.free);
    let fraction = if cap.total > 0 { used as f64 / cap.total as f64 } else { 0.0 };

    let mut job = egui::text::LayoutJob::simple_singleline(name.to_owned(), theme::strong(ui.ctx(), 12.0), p.text_soft());
    job.wrap = egui::text::TextWrapping {
        max_width: (rect.width() - 70.0).max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(pos2(rect.left() + 12.0, rect.top() + 12.0), galley, p.text_soft());
    painter.text(
        pos2(rect.right() - 12.0, rect.top() + 19.0),
        egui::Align2::RIGHT_CENTER,
        format!("{:.0}%", fraction * 100.0),
        theme::strong(ui.ctx(), 12.0),
        p.text_muted,
    );

    let track = Rect::from_min_max(
        pos2(rect.left() + 12.0, rect.top() + 34.0),
        pos2(rect.right() - 12.0, rect.top() + 40.0),
    );
    painter.rect_filled(track, theme::pill(track.height()), p.inset);
    let fill = Rect::from_min_max(
        track.min,
        pos2(track.left() + track.width() * fraction as f32, track.bottom()),
    );
    if fill.width() > 1.0 {
        painter.rect_filled(fill, theme::pill(track.height()), theme::with_alpha(p.accent, 220));
    }

    painter.text(
        pos2(rect.left() + 12.0, rect.bottom() - 15.0),
        egui::Align2::LEFT_CENTER,
        format!(
            "{} free of {}",
            neutron_ui::format::size(Some(cap.free)),
            neutron_ui::format::size(Some(cap.total))
        ),
        egui::FontId::proportional(11.5),
        p.text_faint,
    );
}

/// A row for something not available yet, saying why on hover.
pub fn placeholder_row(ui: &mut Ui, p: &Palette, label: &str, glyph: Glyph, why: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    icons::draw(ui.painter(), pos2(rect.left() + 19.0, rect.center().y), glyph, p.text_faint);
    ui.painter().text(
        pos2(rect.left() + 39.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        p.text_faint,
    );
    response.on_hover_text(why);
}

/// A row that does something rather than going somewhere: "Add a server…".
pub fn action_row(ui: &mut Ui, p: &Palette, label: &str, glyph: Glyph) -> bool {
    action_row_with_hint(ui, p, label, glyph, "")
}

/// A Finder tag: its colour as a dot, its name, and the same selection as a
/// place's row when its files are being shown.
pub fn tag_row(ui: &mut Ui, p: &Palette, name: &str, color: u8, selected: bool) -> RowHit {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if !ui.is_rect_visible(rect) {
        return RowHit::of(&response, rect);
    }
    let corners = CornerRadius::same(12);
    if selected {
        ui.painter().rect_filled(rect, corners, p.selection);
        ui.painter().rect_stroke(
            rect,
            corners,
            Stroke::new(1.0, theme::with_alpha(p.accent, 51)),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        ui.painter().rect_filled(rect, corners, p.hover);
    }
    ui.painter().circle_filled(pos2(rect.left() + 19.0, rect.center().y), 5.5, neutron_ui::tags::color(color));
    let (font, colour) = if selected {
        (theme::strong(ui.ctx(), 13.5), p.text)
    } else {
        (egui::FontId::proportional(13.5), p.text_soft())
    };
    ui.painter().text(pos2(rect.left() + 39.0, rect.center().y), egui::Align2::LEFT_CENTER, name, font, colour);
    RowHit::of(&response, rect)
}

pub fn action_row_with_hint(ui: &mut Ui, p: &Palette, label: &str, glyph: Glyph, hint: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());

    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(12), p.hover);
    }

    icons::draw(ui.painter(), pos2(rect.left() + 19.0, rect.center().y), glyph, p.accent);
    ui.painter().text(
        pos2(rect.left() + 39.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        if response.hovered() { p.text } else { p.text_muted },
    );

    let clicked = response.clicked();
    if !hint.is_empty() {
        response.on_hover_text(hint);
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use neutron_core::places::Capacity;

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
        assert_eq!(glyph_for(&place("Downloads", PlaceKind::KnownFolder)), Glyph::Download);
        assert_eq!(glyph_for(&place("Research", PlaceKind::KnownFolder)), Glyph::Folder);
        assert_eq!(glyph_for(&place("C:", PlaceKind::Drive)), Glyph::Drive);
        assert_eq!(glyph_for(&place("OneDrive", PlaceKind::Cloud)), Glyph::Cloud);
        assert_eq!(glyph_for(&place("Ubuntu-24.04", PlaceKind::Wsl)), Glyph::Terminal);
        let dashboard = Place { id: NodeId::Home, ..place("Dashboard", PlaceKind::KnownFolder) };
        assert_eq!(glyph_for(&dashboard), Glyph::Dashboard);
    }

    #[test]
    fn capacity_totals_ignore_volumes_that_did_not_report() {
        let drives = [
            Place {
                capacity: Some(Capacity { total: 1000, free: 400 }),
                ..place("C:", PlaceKind::Drive)
            },
            place("E:", PlaceKind::Drive),
        ];
        let with: Vec<_> = drives.iter().filter_map(|d| d.capacity).collect();
        assert_eq!(with.len(), 1);
        assert_eq!(with.iter().map(|c| c.total).sum::<u64>(), 1000);
    }
}
