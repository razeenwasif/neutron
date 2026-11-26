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
use neutron_core::places::{Place, PlaceKind};
use neutron_core::NodeId;
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, micro_caps};

/// Height of a quick-destination tile.
const TILE: f32 = 64.0;
/// Height of a recent-file card: a tile, a name and a line under it.
const RECENT_H: f32 = 100.0;
/// Smallest tile width before the grid drops a column.
const TILE_MIN: f32 = 190.0;
/// Space between tiles, and between rows of them.
const GAP: f32 = 12.0;
/// Widest the dashboard's content is allowed to get.
///
/// A greeting and a row of tiles stretched across a 4K pane is a line of text
/// with a metre of nothing after it. The column is capped and centred, which is
/// what every dashboard does and what the concept's own layout implies.
const MAX_CONTENT: f32 = 980.0;

/// Draws the dashboard. Returns a destination if one was chosen.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    volumes: &[Place],
    remote_groups: &[(String, Vec<Place>)],
    known: &[Place],
    recent: &[crate::recent::Visit],
    elsewhere: &[Place],
) -> Option<Chosen> {
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let full = ui.available_width();
            let content = full.min(MAX_CONTENT);
            let margin = ((full - content) / 2.0).max(0.0);

            // A pane narrower than the cap gets the whole thing, with no
            // wrapper at all. The centring below forces a column of exactly
            // `content` wide, and in a split pane that is wider than the pane
            // — so the tiles were laid out for a width they never had and ran
            // off the edge. Nothing to centre, nothing to nest.
            if margin <= 0.5 {
                return body(ui, p, volumes, remote_groups, known, recent, elsewhere);
            }

            let mut chosen = None;
            ui.horizontal(|ui| {
                // egui puts `item_spacing.x` between the space and the column,
                // which lands entirely on the left and made the left gap wider
                // than the right by exactly one spacing unit.
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(content);
                    chosen = body(ui, p, volumes, remote_groups, known, recent, elsewhere);
                });
            });
            chosen
        })
        .inner
}

/// What the dashboard was asked to do.
pub enum Chosen {
    /// Go to a place.
    Go(NodeId),
    /// Open a file that was opened before.
    Open(std::path::PathBuf),
    /// Drop one from the recent list.
    Forget(std::path::PathBuf),
}

fn body(
    ui: &mut Ui,
    p: &Palette,
    volumes: &[Place],
    remote_groups: &[(String, Vec<Place>)],
    known: &[Place],
    recent: &[crate::recent::Visit],
    elsewhere: &[Place],
) -> Option<Chosen> {
    ui.add_space(28.0);
    greeting(ui, p);
    ui.add_space(24.0);

    let mut chosen = None;
    if let Some(volume) = volumes.iter().find(|v| v.capacity.is_some()) {
        chosen = storage_card(ui, p, volume).map(Chosen::Go).or(chosen);
        ui.add_space(14.0);
    }

    for (server_name, vols) in remote_groups {
        if vols.is_empty() {
            continue;
        }
        section(ui, p, &format!("{server_name} · Storage"));
        chosen = remote_storage_row(ui, p, vols).map(Chosen::Go).or(chosen);
        ui.add_space(12.0);
    }

    ui.add_space(8.0);

    // Two rows of four. Everything past that is a scroll away in the sidebar,
    // and a dashboard that lists every pin is a second sidebar.
    section(ui, p, "Quick access");
    chosen = tiles(ui, p, &known[..known.len().min(8)], 4)
        .map(Chosen::Go)
        .or(chosen);

    if !recent.is_empty() {
        ui.add_space(16.0);
        section(ui, p, "Recent files");
        chosen = recent_cards(ui, p, recent).or(chosen);
    }

    let places: Vec<Place> = volumes.iter().chain(elsewhere).cloned().collect();
    if !places.is_empty() {
        ui.add_space(16.0);
        section(ui, p, "Drives and servers");
        chosen = tiles(ui, p, &places, 3).map(Chosen::Go).or(chosen);
    }

    ui.add_space(16.0);
    chosen
}

/// "Good morning", by the clock where the user is.
fn greeting(ui: &mut Ui, p: &Palette) {
    let hello = match neutron_ui::format::local_hour() {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 4.0, rect.top() + 18.0),
        egui::Align2::LEFT_CENTER,
        hello,
        theme::strong(ui.ctx(), 28.0),
        p.text,
    );
    ui.painter().text(
        pos2(rect.left() + 4.0, rect.top() + 47.0),
        egui::Align2::LEFT_CENTER,
        "Everything on this Mac, and everywhere you have connected.",
        egui::FontId::proportional(13.0),
        p.text_muted,
    );
}

/// A group label, in the sidebar's small spaced capitals.
fn section(ui: &mut Ui, p: &Palette, label: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 4.0, rect.top() + 8.0),
        egui::Align2::LEFT_CENTER,
        micro_caps(label),
        theme::strong(ui.ctx(), 10.0),
        p.text_faint,
    );
}

/// The startup volume: how much is free in large type, how full in a bar.
fn storage_card(ui: &mut Ui, p: &Palette, volume: &Place) -> Option<NodeId> {
    let cap = volume.capacity?;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), Sense::click());
    card(ui, p, rect, response.hovered());

    let painter = ui.painter();
    let left = rect.left() + 22.0;
    painter.text(
        pos2(left, rect.top() + 24.0),
        egui::Align2::LEFT_CENTER,
        micro_caps(&volume.name),
        theme::strong(ui.ctx(), 10.0),
        p.text_faint,
    );
    let free = neutron_ui::format::size(Some(cap.free));
    let big = painter.layout_no_wrap(free, theme::strong(ui.ctx(), 30.0), p.text);
    let big_width = big.size().x;
    painter.galley(pos2(left, rect.top() + 38.0), big, p.text);
    let of = format!("free of {}", neutron_ui::format::size(Some(cap.total)));
    let of_galley = painter.layout_no_wrap(of, egui::FontId::proportional(13.0), p.text_muted);
    let of_width = of_galley.size().x;
    painter.galley(pos2(left + big_width + 8.0, rect.top() + 53.0), of_galley, p.text_muted);

    // The bar takes what the text leaves.
    let bar_left = (left + big_width + of_width + 40.0).max(rect.left() + rect.width() * 0.45);
    let track = Rect::from_min_max(
        pos2(bar_left, rect.center().y - 10.0),
        pos2(rect.right() - 22.0, rect.center().y - 4.0),
    );
    if track.width() > 60.0 {
        let used = cap.total.saturating_sub(cap.free);
        let fraction = if cap.total > 0 { used as f32 / cap.total as f32 } else { 0.0 };
        painter.rect_filled(track, theme::pill(track.height()), p.raised);
        let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * fraction, track.bottom()));
        if fill.width() > 2.0 {
            painter.rect_filled(fill, theme::pill(track.height()), p.accent);
        }
        let mut x = track.left();
        for (colour, text) in [
            (p.accent, format!("{} used", neutron_ui::format::size(Some(used)))),
            (p.text_faint, format!("{} free", neutron_ui::format::size(Some(cap.free)))),
        ] {
            let y = track.bottom() + 18.0;
            painter.circle_filled(pos2(x + 4.0, y), 3.5, colour);
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), p.text_muted);
            let width = galley.size().x;
            painter.galley(pos2(x + 12.0, y - galley.size().y / 2.0), galley, p.text_muted);
            x += 12.0 + width + 18.0;
        }
    }
    response.clicked().then(|| volume.id.clone())
}

/// Side-by-side storage cards for a remote host's internal and external SSDs.
fn remote_storage_row(ui: &mut Ui, p: &Palette, vols: &[Place]) -> Option<NodeId> {
    let mut chosen = None;
    let columns = vols.len().clamp(1, 3);
    let width = ((ui.available_width() - GAP * (columns - 1) as f32) / columns as f32).floor();
    for row in vols.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (n, vol) in row.iter().enumerate() {
                if n > 0 {
                    ui.add_space(GAP);
                }
                if remote_storage_card(ui, p, vol, width) {
                    chosen = Some(vol.id.clone());
                }
            }
        });
        ui.add_space(GAP);
    }
    chosen
}

fn remote_storage_card(ui: &mut Ui, p: &Palette, volume: &Place, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 84.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }
    card(ui, p, rect, response.hovered());

    let painter = ui.painter();
    let plate = Rect::from_min_size(pos2(rect.left() + 16.0, rect.top() + 14.0), vec2(32.0, 32.0));
    painter.rect_filled(plate, egui::CornerRadius::same(theme::RADIUS_WELL), p.raised);
    icons::draw_scaled(painter, plate.center(), Glyph::Drive, p.cloud, 0.9);

    let text_left = plate.right() + 12.0;
    let name_galley = painter.layout_no_wrap(
        volume.name.clone(),
        theme::strong(ui.ctx(), 13.5),
        p.text,
    );
    painter.galley(pos2(text_left, rect.top() + 13.0), name_galley, p.text);

    if let Some(cap) = volume.capacity {
        let used = cap.total.saturating_sub(cap.free);
        let fraction = if cap.total > 0 { used as f32 / cap.total as f32 } else { 0.0 };
        painter.text(
            pos2(rect.right() - 16.0, rect.top() + 21.0),
            egui::Align2::RIGHT_CENTER,
            format!("{:.0}%", fraction * 100.0),
            theme::strong(ui.ctx(), 12.0),
            p.text_muted,
        );
        let detail = format!(
            "{} free of {} · {} used",
            neutron_ui::format::size(Some(cap.free)),
            neutron_ui::format::size(Some(cap.total)),
            neutron_ui::format::size(Some(used)),
        );
        painter.text(
            pos2(text_left, rect.top() + 37.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::proportional(11.5),
            p.text_faint,
        );
        let track = Rect::from_min_max(
            pos2(rect.left() + 16.0, rect.bottom() - 17.0),
            pos2(rect.right() - 16.0, rect.bottom() - 12.0),
        );
        painter.rect_filled(track, theme::pill(track.height()), p.raised);
        let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * fraction, track.bottom()));
        if fill.width() > 1.0 {
            painter.rect_filled(fill, theme::pill(track.height()), p.accent);
        }
    }

    response.clicked()
}

/// Recent files, as a grid of cards.
fn recent_cards(ui: &mut Ui, p: &Palette, recent: &[crate::recent::Visit]) -> Option<Chosen> {
    let mut chosen = None;
    let shown = &recent[..recent.len().min(8)];
    // Columns by width alone, not by how many there are: one recent file is a
    // card, not a banner across the pane.
    let columns = columns_for(ui.available_width(), 4);
    let width = ((ui.available_width() - GAP * (columns - 1) as f32) / columns as f32).floor();
    for row in shown.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (n, visit) in row.iter().enumerate() {
                if n > 0 {
                    ui.add_space(GAP);
                }
                if let Some(hit) = recent_card(ui, p, visit, width) {
                    chosen = Some(hit);
                }
            }
        });
        ui.add_space(GAP);
    }
    chosen
}

fn recent_card(ui: &mut Ui, p: &Palette, visit: &crate::recent::Visit, width: f32) -> Option<Chosen> {
    let (rect, response) = ui.allocate_exact_size(vec2(width, RECENT_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return None;
    }
    card(ui, p, rect, response.hovered());

    let name = visit
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| visit.path.to_string_lossy().into_owned());

    let tile = Rect::from_min_size(pos2(rect.left() + 14.0, rect.top() + 14.0), vec2(36.0, 36.0));
    icons::tile(
        ui.painter(),
        tile,
        icons::FileKind::of(&name, neutron_core::EntryKind::File),
        p,
        false,
    );

    // Clipped through a layout job: a long filename would otherwise run past
    // the card and over the next one.
    let text_left = rect.left() + 14.0;
    let room = rect.width() - 28.0;
    let line = |text: String, font: egui::FontId, colour: egui::Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(text, font, colour);
        job.wrap = egui::text::TextWrapping {
            max_width: room.max(1.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        ui.painter().layout_job(job)
    };
    let title = line(name, theme::strong(ui.ctx(), 13.5), p.text);
    ui.painter().galley(pos2(text_left, rect.top() + 60.0), title, p.text);

    // The folder, not the whole path: the filename is above it, and what is
    // needed is which of the places called `notes.txt` this one is.
    let folder = visit
        .path
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "—".to_owned());
    let detail = line(
        format!("{folder} · {}", neutron_ui::format::timestamp(visit.at)),
        egui::FontId::proportional(12.0),
        p.text_faint,
    );
    ui.painter().galley(pos2(text_left, rect.top() + 80.0), detail, p.text_faint);

    let response = response.on_hover_text(visit.path.to_string_lossy());
    if response.clicked() {
        return Some(Chosen::Open(visit.path.clone()));
    }
    // Right-click forgets it. A recent list is the one list here the user never
    // curates, so the only control over it that matters is taking something
    // out — a file opened once by accident should not sit there for a week.
    if response.secondary_clicked() {
        return Some(Chosen::Forget(visit.path.clone()));
    }
    None
}

/// A card on the dashboard: a faint lift off the pane, a little more under
/// the pointer.
fn card(ui: &Ui, p: &Palette, rect: Rect, hovered: bool) {
    let corners = egui::CornerRadius::same(theme::RADIUS_PANEL);
    ui.painter()
        .rect_filled(rect, corners, if hovered { p.card_hover } else { p.inset });
}

/// Quick destinations, as a grid of tiles at most `max_columns` across.
fn tiles(ui: &mut Ui, p: &Palette, places: &[Place], max_columns: usize) -> Option<NodeId> {
    if places.is_empty() {
        return None;
    }
    let mut chosen = None;

    let columns = columns_for(ui.available_width(), places.len()).min(max_columns);
    let width = ((ui.available_width() - GAP * (columns - 1) as f32) / columns as f32).floor();

    for row in places.chunks(columns) {
        ui.horizontal(|ui| {
            // egui adds `item_spacing.x` between widgets of its own accord. With
            // the gap added by hand as well, every tile cost width + 8 + 12 and
            // the last one in each row fell off the end.
            ui.spacing_mut().item_spacing.x = 0.0;
            for (n, place) in row.iter().enumerate() {
                if n > 0 {
                    ui.add_space(GAP);
                }
                if tile(ui, p, place, width) {
                    chosen = Some(place.id.clone());
                }
            }
        });
        ui.add_space(GAP);
    }

    chosen
}

/// How many tiles fit across `available`.
///
/// Capped at the number there actually are, so four folders fill a wide pane
/// as four wide tiles rather than four narrow ones with a hole beside them.
fn columns_for(available: f32, count: usize) -> usize {
    let fits = ((available + GAP) / (TILE_MIN + GAP)).floor() as usize;
    fits.clamp(1, 5).min(count.max(1))
}

fn tile(ui: &mut Ui, p: &Palette, place: &Place, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }
    card(ui, p, rect, response.hovered());

    let plate = Rect::from_min_size(pos2(rect.left() + 14.0, rect.center().y - 19.0), vec2(38.0, 38.0));
    let folder_like = matches!(place.kind, PlaceKind::KnownFolder);
    let (plate_fill, glyph_colour) = if folder_like {
        (theme::with_alpha(p.tints.folder, 24), p.tints.folder)
    } else {
        (p.raised, p.text_soft())
    };
    ui.painter().rect_filled(plate, egui::CornerRadius::same(theme::RADIUS_WELL), plate_fill);
    icons::draw(ui.painter(), plate.center(), glyph_for(place), glyph_colour);

    let text_left = plate.right() + 12.0;
    let room = (rect.right() - text_left - 14.0).max(1.0);
    let line = |text: String, font: egui::FontId, colour: egui::Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(text, font, colour);
        job.wrap = egui::text::TextWrapping {
            max_width: room,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        ui.painter().layout_job(job)
    };

    match place.capacity {
        // A volume shows how full it is, as the storage card does in large.
        Some(cap) => {
            let used = cap.total.saturating_sub(cap.free);
            let fraction = if cap.total > 0 { used as f32 / cap.total as f32 } else { 0.0 };
            let name = line(place.name.clone(), theme::strong(ui.ctx(), 13.5), p.text);
            ui.painter().galley(pos2(text_left, rect.center().y - 15.0), name, p.text);
            ui.painter().text(
                pos2(rect.right() - 14.0, rect.center().y - 7.0),
                egui::Align2::RIGHT_CENTER,
                format!("{:.0}%", fraction * 100.0),
                egui::FontId::proportional(12.0),
                p.text_faint,
            );
            let track = Rect::from_min_max(
                pos2(text_left, rect.center().y + 6.0),
                pos2(rect.right() - 14.0, rect.center().y + 11.0),
            );
            ui.painter().rect_filled(track, theme::pill(5.0), p.raised);
            let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * fraction, track.bottom()));
            if fill.width() > 1.0 {
                ui.painter().rect_filled(fill, theme::pill(5.0), theme::with_alpha(p.accent, 220));
            }
        }
        None => {
            let name = line(place.name.clone(), theme::strong(ui.ctx(), 13.5), p.text);
            ui.painter().galley(pos2(text_left, rect.center().y - 17.0), name, p.text);
            let detail = match place.kind {
                PlaceKind::Server => "Server".to_owned(),
                PlaceKind::Cloud => "Cloud storage".to_owned(),
                _ => place
                    .id
                    .as_path()
                    .map(crate::app::tidy_location)
                    .unwrap_or_else(|| "Folder".to_owned()),
            };
            let detail = line(detail, egui::FontId::proportional(12.0), p.text_faint);
            ui.painter().galley(pos2(text_left, rect.center().y + 2.0), detail, p.text_faint);
        }
    }

    response.clicked()
}

fn glyph_for(place: &Place) -> Glyph {
    match place.kind {
        PlaceKind::Drive => Glyph::Drive,
        PlaceKind::Cloud => icons::cloud_glyph(&place.name),
        PlaceKind::Server => Glyph::Server,
        PlaceKind::Wsl => Glyph::Terminal,
        _ => match place.name.as_str() {
            "Home" => Glyph::Home,
            "Desktop" => Glyph::Desktop,
            "Downloads" => Glyph::Download,
            "Documents" => Glyph::Document,
            "Pictures" => Glyph::Image,
            "Music" => Glyph::Music,
            "Movies" | "Videos" => Glyph::Film,
            "Applications" => Glyph::Grid,
            _ => Glyph::Folder,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid has to stay usable in a narrow pane. A split four ways is not
    /// unusual, and a tile that never fits is a tile that never renders.
    #[test]
    fn the_grid_always_keeps_at_least_one_column() {
        for available in [0.0f32, 40.0, 149.0, 150.0, 600.0, 4000.0] {
            let columns = columns_for(available, 12);
            assert!((1..=5).contains(&columns), "{available}px gave {columns}");
        }
    }

    /// The row must fit. This is the arithmetic that was wrong: a trailing gap
    /// after the last tile pushed every row over the edge.
    #[test]
    fn a_row_of_tiles_fits_the_width_it_was_given() {
        for available in [150.0f32, 320.0, 700.0, 980.0] {
            let columns = columns_for(available, 12);
            let width = ((available - GAP * (columns - 1) as f32) / columns as f32).floor();
            let used = width * columns as f32 + GAP * (columns - 1) as f32;
            assert!(
                used <= available,
                "{columns} tiles of {width} used {used} of {available}"
            );
            assert!(width >= 1.0, "{available}px gave a {width}px tile");
        }
    }

    /// Four folders in a wide pane should be four wide tiles, not four narrow
    /// ones with a hole beside them.
    #[test]
    fn a_short_list_does_not_leave_empty_columns() {
        assert_eq!(columns_for(4000.0, 4), 4);
        assert_eq!(columns_for(4000.0, 1), 1);
        assert_eq!(columns_for(4000.0, 12), 5);
    }

    /// A pane narrower than one tile still gets one, rather than zero and a
    /// division by zero after it.
    #[test]
    fn a_pane_too_narrow_for_a_tile_still_draws_one() {
        assert_eq!(columns_for(10.0, 6), 1);
        assert_eq!(columns_for(0.0, 6), 1);
    }

    /// The content column is capped so a greeting does not sit alone at the
    /// left of a very wide pane.
    #[test]
    fn the_content_column_is_capped_and_centred() {
        let margin = |full: f32| ((full - full.min(MAX_CONTENT)) / 2.0).max(0.0);
        assert_eq!(margin(600.0), 0.0, "a narrow pane uses all of itself");
        assert!(margin(2000.0) > 0.0, "a wide pane centres its column");
        assert_eq!(margin(MAX_CONTENT), 0.0);
    }
}
