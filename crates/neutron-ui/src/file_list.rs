//! The virtualized directory listing.
//!
//! # Why this is fast
//!
//! [`egui::ScrollArea::show_rows`] is given the total row count and a uniform
//! row height, and calls back only for the rows currently on screen. A 500k-entry
//! directory therefore costs the same per frame as a 50-entry one — roughly 30
//! rows of work either way. Nothing iterates the full list during painting.
//!
//! Rows are painted directly rather than composed from nested layouts: one
//! `allocate_exact_size` plus a handful of painter calls each.
//!
//! # Visual rules
//!
//! * **One rule, under the header.** Column labels are set as tiny tracked
//!   uppercase and separated from the rows by a single hairline — the shape the
//!   reference designs use. Beyond that there are no gridlines: fully ruled
//!   tables are the single most dated thing a file list can do.
//! * **No zebra striping.** Hover and selection identify a row; permanent
//!   banding is visual noise the rest of the time.
//! * **Metadata recedes.** The filename is the content; size, date, and type
//!   are reference information and are drawn faint so the eye skips them.
//! * **Icons are the system's.** Real shell icons, sampled from a texture atlas
//!   filled on worker threads; a row that has not got one yet draws a grey
//!   outline glyph instead and swaps it in when it arrives. Rows never wait for
//!   an icon — a listing that scrolls at the speed of its slowest icon handler
//!   is Explorer's most visible stall.
//!
//! Nothing here touches the filesystem. Rows render from the in-memory
//! [`EntryList`] snapshot only; anything that could block belongs on a worker.

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, TextStyle, Ui, pos2, vec2};
use neutron_core::entry::{EntryKind, SyncState};
use neutron_core::{EntryList, SelectMode, Selection, SortColumn, SortOrder, SortSpec};

use crate::format;
use crate::icons;
use crate::theme::{self, Palette, RADIUS_CONTROL, ROW_HEIGHT};

/// Fixed column widths, in points.
const W_SIZE: f32 = 92.0;
const W_MODIFIED: f32 = 140.0;
const W_KIND: f32 = 104.0;
/// Space reserved for the icon at the start of a row.
const ICON_SLOT: f32 = 32.0;
/// Padding inside the list, left and right.
const LIST_PAD: f32 = 16.0;
const CELL_PAD: f32 = 10.0;
/// Name column never shrinks below this, so it stays readable on narrow panes.
const MIN_NAME_WIDTH: f32 = 120.0;
/// On-screen size of a system icon, in points. Smaller than the 32px source it
/// is sampled from, which is what keeps it crisp on a high-DPI display.
const ICON_DRAW: f32 = 20.0;

/// How the listing is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// One row per entry, with size, date and type columns.
    #[default]
    List,
    /// A grid of tiles. Trades the metadata columns for a much larger icon,
    /// which is what makes a folder of images or executables recognisable at a
    /// glance rather than a column of near-identical filenames.
    Grid,
}

impl ViewMode {
    pub fn toggled(self) -> Self {
        match self {
            ViewMode::List => ViewMode::Grid,
            ViewMode::Grid => ViewMode::List,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileListState {
    pub sort: SortSpec,
    pub show_hidden: bool,
    pub view: ViewMode,
    /// Tiles per row as last laid out.
    ///
    /// Written during paint and read by the app's key handling: in a grid,
    /// Up and Down move by a whole row, and only the layout knows how wide a
    /// row is. Zero until the grid has been drawn once.
    pub columns: usize,
    /// Live narrowing filter from the pane header's field. Empty means no
    /// filter; see [`neutron_core::sort::apply_filtered`].
    pub filter: String,
    /// Storage index to bring into view on the next frame. Consumed once.
    pub scroll_to: Option<usize>,
    /// Last observed vertical scroll offset, carried between frames so
    /// scroll-into-view can move the minimum distance instead of always
    /// snapping the target row to the top of the viewport.
    scroll_offset: f32,
}

impl Default for FileListState {
    fn default() -> Self {
        Self {
            sort: SortSpec::default(),
            show_hidden: false,
            view: ViewMode::default(),
            columns: 0,
            filter: String::new(),
            scroll_to: None,
            scroll_offset: 0.0,
        }
    }
}

/// Something the user did that the app must act on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileListAction {
    /// Double-click or Enter — open the entry.
    Activate(usize),
    Select {
        idx: usize,
        mode: SelectMode,
    },
    /// A column header was clicked.
    SortBy(SortColumn),
    /// Right-click. `None` means the click landed on empty space below the rows.
    ContextMenu {
        idx: Option<usize>,
        pos: egui::Pos2,
    },
    /// Click on empty space — clears the selection.
    ClearSelection,
    /// A rubber band covered these display positions, inclusive.
    Marquee {
        from: usize,
        to: usize,
        /// Ctrl was held: add to the selection rather than replacing it.
        additive: bool,
    },
}

/// Supplies real system icons for rows, when they have been resolved yet.
///
/// A trait rather than a concrete type because the resolver lives in
/// `neutron-app` — it needs `neutron-shell`, and this crate deliberately does
/// not depend on Win32 so that it keeps building and testing on Linux.
///
/// `row` takes `&self`: it is called from the paint path, where the app holds
/// only a shared borrow of its own state.
pub trait IconSource {
    /// The atlas texture every row samples from.
    fn texture(&self) -> egui::TextureId;
    /// UV rect for this entry's icon, or `None` while it is still unresolved —
    /// in which case the row falls back to the drawn glyph.
    fn uv_for(&self, name: &str, kind: EntryKind) -> Option<egui::Rect>;
}

/// Draws the listing in whichever view mode is active.
pub fn show(
    ui: &mut Ui,
    state: &mut FileListState,
    list: &EntryList,
    selection: &Selection,
    p: &Palette,
    icons: Option<&dyn IconSource>,
) -> Option<FileListAction> {
    match state.view {
        ViewMode::List => show_list(ui, state, list, selection, p, icons),
        ViewMode::Grid => show_grid(ui, state, list, selection, p, icons),
    }
}

/// Draws the header and the virtualized rows.
fn show_list(
    ui: &mut Ui,
    state: &mut FileListState,
    list: &EntryList,
    selection: &Selection,
    p: &Palette,
    icons: Option<&dyn IconSource>,
) -> Option<FileListAction> {
    let mut action = None;

    let total_width = ui.available_width() - LIST_PAD * 2.0;
    let name_width =
        (total_width - ICON_SLOT - W_SIZE - W_MODIFIED - W_KIND).max(MIN_NAME_WIDTH);

    if let Some(a) = header(ui, state, p, name_width) {
        action = Some(a);
    }

    let row_count = list.order().len();
    let viewport_height = ui.available_height();

    let mut scroll = egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .id_salt("file_list_scroll");

    // Keyboard navigation moves the cursor off screen; bring it back. Resolved
    // through `rank` because the cursor is a storage index, not a row number.
    //
    // Scrolls the minimum distance: holding Down should walk the cursor to the
    // bottom edge and then scroll one row at a time, not re-centre the list on
    // every keystroke.
    if let Some(target) = state.scroll_to.take() {
        if let Some(row) = list.rank(target) {
            let top = row as f32 * ROW_HEIGHT;
            let bottom = top + ROW_HEIGHT;
            let current = state.scroll_offset;

            let desired = if top < current {
                Some(top)
            } else if bottom > current + viewport_height {
                Some(bottom - viewport_height)
            } else {
                None
            };

            if let Some(offset) = desired {
                scroll = scroll.vertical_scroll_offset(offset.max(0.0));
            }
        }
    }

    let viewport = ui.available_rect_before_wrap();


    let output = scroll.show_rows(ui, ROW_HEIGHT, row_count, |ui, visible| {
        // `visible` is the only range that costs anything, regardless of how
        // large the directory is.
        for row in visible {
            let idx = list.at(row);
            if let Some(a) = draw_row(ui, list, selection, p, idx, name_width, icons) {
                action = Some(a);
            }
        }


        // Clicking below the last row clears the selection, as in Explorer.
        let leftover = ui.available_size();
        if leftover.y > 0.0 {
            let (rect, response) = ui.allocate_exact_size(leftover, Sense::click());
            if response.clicked() {
                action = Some(FileListAction::ClearSelection);
            }
            if response.secondary_clicked() {
                action = Some(FileListAction::ContextMenu {
                    idx: None,
                    pos: response
                        .interact_pointer_pos()
                        .unwrap_or_else(|| rect.center()),
                });
            }
        }
    });

    state.scroll_offset = output.state.offset.y;

    // Rubber band, tracked after the rows so it paints over them.
    let start_area =
        band_start_area(ui.spacing().scroll, viewport, output.content_size.y > viewport.height());
    let band = track_band(ui, viewport, start_area, output.state.offset.y);
    if let Some(rect) = band.rect {
        draw_band(ui, p, rect, viewport, output.state.offset.y);

        // Rows are a uniform height, so the covered range is arithmetic rather
        // than a hit test — which is what makes this cover rows scrolled far
        // out of view, and what a per-row test could not.
        if row_count > 0 {
            let first = (rect.top() / ROW_HEIGHT).floor().max(0.0) as usize;
            let last = (rect.bottom() / ROW_HEIGHT).floor().max(0.0) as usize;
            action = Some(FileListAction::Marquee {
                from: first.min(row_count - 1),
                to: last.min(row_count - 1),
                additive: band.additive,
            });
        }
        // The band follows the pointer, so the frame has to keep coming even
        // while the mouse is held still.
        ui.ctx().request_repaint();
    }

    action
}

/// Tracks a rubber-band drag and reports what it covers.
///
/// # Why this works in content coordinates
///
/// The list scrolls while the band is being dragged — dragging past the bottom
/// edge auto-scrolls, which is the whole point of dragging past the bottom
/// edge. A band anchored in *screen* space would stay put while the content
/// moved underneath, so the selection would drift away from the rows the user
/// was pointing at. Anchoring in content space means the origin stays glued to
/// the row it started on.
struct Band {
    /// Rectangle in content coordinates, or `None` when no drag is in progress.
    rect: Option<Rect>,
    additive: bool,
}

/// Where this list's band drag started, in content coordinates.
///
/// Held in egui's own memory rather than in [`FileListState`], because the pane
/// is drawn from a *clone* of that state — anything written during paint is
/// discarded when the frame ends. A band origin stored there was reset to
/// `None` on every frame, so the drag never survived past the press and no band
/// ever appeared. Keyed by the pane's `Ui` id, so split panes each get their
/// own.
fn band_origin_id(ui: &Ui) -> egui::Id {
    ui.id().with("marquee-origin")
}

/// The width down the right edge of a scroll area that belongs to the scroll
/// bar, and where a rubber band must therefore not start.
///
/// egui senses the bar over `bar_width` inset by `bar_outer_margin`, and it
/// senses that full width even while the bar is drawn as a thin floating line —
/// so the strip is wider than what is on screen. Without excluding it, grabbing
/// the scroll bar swept a selection across everything the drag passed: the list
/// still scrolled, but it came back with eighty rows selected, which is not
/// what anyone means by dragging a scroll bar.
///
/// Zero when the content fits, because then there is no bar and the last few
/// pixels of the list should still start a band.
fn scrollbar_strip(scroll: egui::style::ScrollStyle, overflowing: bool) -> f32 {
    if !overflowing {
        return 0.0;
    }
    scroll.bar_width + scroll.bar_outer_margin
}

/// `viewport` with the scroll bar's strip taken off its right edge.
fn band_start_area(
    scroll: egui::style::ScrollStyle,
    viewport: Rect,
    overflowing: bool,
) -> Rect {
    Rect::from_min_max(
        viewport.min,
        pos2(
            viewport.right() - scrollbar_strip(scroll, overflowing),
            viewport.bottom(),
        ),
    )
}

/// Updates the band state from this frame's pointer.
///
/// `viewport` is the scroll area's rect, used to convert screen positions into
/// content ones. `start_area` is where a press may *begin* a band, which is
/// `viewport` minus the scroll bar's strip.
fn track_band(ui: &Ui, viewport: Rect, start_area: Rect, scroll_offset: f32) -> Band {
    let (pointer, primary_down, primary_released, ctrl) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.modifiers.ctrl || i.modifiers.command,
        )
    });

    let to_content = |p: egui::Pos2| pos2(p.x - viewport.left(), p.y - viewport.top() + scroll_offset);

    let id = band_origin_id(ui);
    let stored: Option<egui::Pos2> = ui.ctx().data(|d| d.get_temp(id));

    // A band starts anywhere in the list, including on top of a row.
    //
    // Explorer reserves a press on a row for dragging the file out, and an
    // earlier version copied that — which made the band nearly unreachable,
    // since rows occupy almost the whole list. Neutron has no outbound file
    // drag yet, so that reservation bought nothing and cost the feature.
    //
    // Safe alongside click-selection because egui only reports `clicked()` for
    // a press and release that did not travel, and the band needs a few pixels
    // of travel before it starts. When outbound drag lands, this is where the
    // two gestures will have to be told apart — most likely by whether the
    // press landed on an already-selected row.
    if primary_down && stored.is_none() {
        if let Some(p) = pointer.filter(|p| start_area.contains(*p)) {
            let origin = to_content(p);
            ui.ctx().data_mut(|d| d.insert_temp(id, origin));
        }
    }

    if primary_released {
        ui.ctx().data_mut(|d| d.remove::<egui::Pos2>(id));
        return Band { rect: None, additive: ctrl };
    }

    let Some(origin) = ui.ctx().data(|d| d.get_temp::<egui::Pos2>(id)) else {
        return Band { rect: None, additive: ctrl };
    };
    let Some(current) = pointer.map(to_content) else {
        return Band { rect: None, additive: ctrl };
    };

    // Below a few pixels this is a click with a shaky hand, and treating it as
    // a band would clear the selection on every imprecise click.
    let rect = Rect::from_two_pos(origin, current);
    if rect.width() < 3.0 && rect.height() < 3.0 {
        return Band { rect: None, additive: ctrl };
    }

    Band {
        rect: Some(rect),
        additive: ctrl,
    }
}

/// Paints the band over the viewport, converting back to screen coordinates.
fn draw_band(ui: &Ui, p: &Palette, band: Rect, viewport: Rect, scroll_offset: f32) {
    let screen = Rect::from_min_max(
        pos2(
            band.left() + viewport.left(),
            band.top() + viewport.top() - scroll_offset,
        ),
        pos2(
            band.right() + viewport.left(),
            band.bottom() + viewport.top() - scroll_offset,
        ),
    );

    ui.painter()
        .rect_filled(screen, RADIUS_SMALL_F, p.selection);
    ui.painter().rect_stroke(
        screen,
        RADIUS_SMALL_F,
        Stroke::new(1.0, p.accent),
        egui::StrokeKind::Inside,
    );
}

/// Column labels: tiny tracked uppercase over a single hairline.
///
/// The tracked micro-caps come straight from the reference design's table
/// ("FILE NAME", "DATE UPLOADED"). They read as a legend rather than as data,
/// which is exactly the job — and at 10pt they take up no visual weight at all,
/// so the one hairline beneath is enough to separate header from content
/// without ruling the rest of the table.
fn header(
    ui: &mut Ui,
    state: &FileListState,
    p: &Palette,
    name_width: f32,
) -> Option<FileListAction> {
    let mut action = None;
    let height = 32.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());

    let cols = [
        (
            SortColumn::Name,
            "Name",
            ICON_SLOT + name_width,
            Align2::LEFT_CENTER,
        ),
        (SortColumn::Size, "Size", W_SIZE, Align2::RIGHT_CENTER),
        (
            SortColumn::Modified,
            "Modified",
            W_MODIFIED,
            Align2::LEFT_CENTER,
        ),
        (SortColumn::Kind, "Type", W_KIND, Align2::LEFT_CENTER),
    ];

    let font = FontId::proportional(10.0);
    let mut x = rect.left() + LIST_PAD;

    for (col, label, width, align) in cols {
        let cell = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(width, height - 10.0));
        let response = ui.interact(cell, ui.id().with(("hdr", label)), Sense::click());

        if response.clicked() {
            action = Some(FileListAction::SortBy(col));
        }

        let active = state.sort.column == col;
        let colour = if active || response.hovered() {
            p.text_muted
        } else {
            p.text_faint
        };

        if response.hovered() {
            ui.painter().rect_filled(cell, RADIUS_SMALL_F, p.hover);
        }

        // Reserve room for the sort marker so it never overlaps the next label.
        let marker = if active { 14.0 } else { 0.0 };
        let text_rect = cell.shrink2(vec2(CELL_PAD * 0.6, 0.0));
        let anchor = match align {
            Align2::RIGHT_CENTER => pos2(text_rect.right() - marker, text_rect.center().y),
            _ => pos2(text_rect.left(), text_rect.center().y),
        };
        ui.painter()
            .text(anchor, align, theme::micro_caps(label), font.clone(), colour);

        if active {
            icons::sort_arrow(
                ui.painter(),
                pos2(text_rect.right() - 5.0, text_rect.center().y),
                state.sort.order == SortOrder::Ascending,
                p.text_muted,
            );
        }

        x += width;
    }

    // Inset to the same padding as the rows, so it lines up with the content
    // rather than running the full width of the card.
    ui.painter().hline(
        (rect.left() + LIST_PAD * 0.5)..=(rect.right() - LIST_PAD * 0.5),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.border),
    );

    action
}

const RADIUS_SMALL_F: f32 = 8.0;

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    list: &EntryList,
    selection: &Selection,
    p: &Palette,
    idx: usize,
    name_width: f32,
    icons: Option<&dyn IconSource>,
) -> Option<FileListAction> {
    let full = ui.available_width();
    let (outer, response) = ui.allocate_exact_size(vec2(full, ROW_HEIGHT), Sense::click());

    if !ui.is_rect_visible(outer) {
        return None;
    }

    // The interactive row spans the full width, but the painted highlight is
    // inset — a selection that runs edge to edge inside a rounded card looks
    // like a bug, and the inset is what makes it read as a pill.
    let rect = Rect::from_min_max(
        pos2(outer.left() + LIST_PAD * 0.5, outer.top() + 1.0),
        pos2(outer.right() - LIST_PAD * 0.5, outer.bottom() - 1.0),
    );

    let selected = selection.is_selected(idx);
    let is_cursor = selection.cursor() == Some(idx);

    if selected {
        ui.painter().rect(
            rect,
            RADIUS_CONTROL as f32,
            p.selection,
            Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }

    // Focus ring on the keyboard cursor, so it stays findable when it is not
    // part of the selection.
    if is_cursor && !selected {
        ui.painter().rect_stroke(
            rect,
            RADIUS_CONTROL as f32,
            Stroke::new(1.0, p.accent),
            egui::StrokeKind::Inside,
        );
    }

    let kind = list.kind(idx);
    let hidden = list.is_hidden(idx);

    // Hidden files are dimmed rather than being identical to normal ones.
    let name_colour = if hidden { p.text_muted } else { p.text };
    let meta_colour = p.text_faint;
    let body = TextStyle::Body.resolve(ui.style());
    let small = FontId::proportional(11.0);

    let mut x = outer.left() + LIST_PAD;

    // --- icon ---
    //
    // The real system icon when it has been resolved, the drawn outline
    // otherwise. Rows never wait: an unresolved icon is a normal state that
    // lasts a frame or two, not an error.
    let centre = pos2(x + ICON_SLOT / 2.0 - 5.0, outer.center().y);
    let uv = icons.and_then(|src| src.uv_for(list.name(idx), kind));

    match (icons, uv) {
        (Some(src), Some(uv)) => {
            let rect = Rect::from_center_size(centre, vec2(ICON_DRAW, ICON_DRAW));
            let mut mesh = egui::Mesh::with_texture(src.texture());
            // Hidden entries are dimmed by tinting the icon rather than by
            // drawing a different one, matching how their name is dimmed.
            let tint = if hidden {
                Color32::from_white_alpha(110)
            } else {
                Color32::WHITE
            };
            mesh.add_rect_with_uv(rect, uv, tint);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        _ => icons::draw(
            ui.painter(),
            centre,
            icons::for_kind(kind),
            if selected {
                p.accent
            } else if hidden {
                p.text_faint
            } else {
                p.icon
            },
        ),
    }
    x += ICON_SLOT;

    // --- name ---
    let name_rect = Rect::from_min_size(pos2(x, outer.top()), vec2(name_width, ROW_HEIGHT));
    draw_cell_text(
        ui,
        name_rect,
        list.name(idx),
        &body,
        name_colour,
        Align2::LEFT_CENTER,
    );

    // Cloud-only badge, right-aligned within the name column.
    if list.sync(idx) == SyncState::CloudOnly {
        ui.painter().text(
            pos2(name_rect.right() - 2.0, outer.center().y),
            Align2::RIGHT_CENTER,
            "☁",
            small.clone(),
            p.cloud,
        );
    }
    x += name_width;

    // --- size (blank for containers) ---
    let size_rect = Rect::from_min_size(pos2(x, outer.top()), vec2(W_SIZE, ROW_HEIGHT));
    let size_text = format::size((!kind.is_container()).then(|| list.size(idx)));
    draw_cell_text(
        ui,
        size_rect,
        &size_text,
        &small,
        meta_colour,
        Align2::RIGHT_CENTER,
    );
    x += W_SIZE;

    // --- modified ---
    let mod_rect = Rect::from_min_size(pos2(x, outer.top()), vec2(W_MODIFIED, ROW_HEIGHT));
    draw_cell_text(
        ui,
        mod_rect,
        &format::timestamp(list.modified(idx)),
        &small,
        meta_colour,
        Align2::LEFT_CENTER,
    );
    x += W_MODIFIED;

    // --- type ---
    let kind_rect = Rect::from_min_size(pos2(x, outer.top()), vec2(W_KIND, ROW_HEIGHT));
    draw_cell_text(
        ui,
        kind_rect,
        &kind_label(kind, list.name(idx)),
        &small,
        meta_colour,
        Align2::LEFT_CENTER,
    );

    // --- interaction ---
    if response.double_clicked() {
        return Some(FileListAction::Activate(idx));
    }
    if response.clicked() {
        let mods = ui.input(|i| i.modifiers);
        let mode = if mods.shift {
            SelectMode::Range
        } else if mods.ctrl || mods.command {
            SelectMode::Toggle
        } else {
            SelectMode::Replace
        };
        return Some(FileListAction::Select { idx, mode });
    }
    if response.secondary_clicked() {
        return Some(FileListAction::ContextMenu {
            idx: Some(idx),
            pos: response
                .interact_pointer_pos()
                .unwrap_or_else(|| outer.center()),
        });
    }

    None
}


// --- grid view ------------------------------------------------------------

/// Tile geometry, in points.
const TILE_W: f32 = 128.0;
const TILE_H: f32 = 116.0;
const TILE_GAP: f32 = 10.0;
/// Icon size inside a tile. Sampled from the 32px atlas cell, so this is a
/// modest upscale — soft rather than crisp, which is the price of one atlas
/// shared with the list view. A second atlas at 64px would double its memory
/// to sharpen an icon nobody reads at arm's length.
const TILE_ICON: f32 = 44.0;

/// Draws the grid, virtualized a row of tiles at a time.
///
/// Same trick as the list: `show_rows` is told the number of tile *rows*, so a
/// 500k-entry directory costs the same per frame as a 50-entry one. What
/// changes is that one "row" now holds several entries.
fn show_grid(
    ui: &mut Ui,
    state: &mut FileListState,
    list: &EntryList,
    selection: &Selection,
    p: &Palette,
    icons: Option<&dyn IconSource>,
) -> Option<FileListAction> {
    let mut action = None;

    let avail = (ui.available_width() - LIST_PAD * 2.0).max(TILE_W);
    // At least one column, however narrow the pane: a grid with zero columns
    // divides by zero and shows nothing.
    let columns = (((avail + TILE_GAP) / (TILE_W + TILE_GAP)).floor() as usize).max(1);
    state.columns = columns;

    let count = list.order().len();
    let rows = count.div_ceil(columns);
    let viewport_height = ui.available_height();

    let mut scroll = egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .id_salt("file_grid_scroll");

    // Scroll-into-view, in tile rows rather than list rows.
    if let Some(target) = state.scroll_to.take() {
        if let Some(index) = list.rank(target) {
            let row = index / columns;
            let top = row as f32 * (TILE_H + TILE_GAP);
            let bottom = top + TILE_H + TILE_GAP;
            let current = state.scroll_offset;

            let desired = if top < current {
                Some(top)
            } else if bottom > current + viewport_height {
                Some(bottom - viewport_height)
            } else {
                None
            };
            if let Some(offset) = desired {
                scroll = scroll.vertical_scroll_offset(offset.max(0.0));
            }
        }
    }

    let viewport = ui.available_rect_before_wrap();
    let row_pitch = TILE_H + TILE_GAP;


    let output = scroll.show_rows(ui, row_pitch, rows, |ui, visible| {
        let full = ui.available_width();
        for row in visible {
            let (band, response) =
                ui.allocate_exact_size(vec2(full, TILE_H + TILE_GAP), Sense::click());

            for column in 0..columns {
                let index = row * columns + column;
                if index >= count {
                    break;
                }
                let idx = list.at(index);
                let rect = Rect::from_min_size(
                    pos2(
                        band.left() + LIST_PAD + column as f32 * (TILE_W + TILE_GAP),
                        band.top() + TILE_GAP / 2.0,
                    ),
                    vec2(TILE_W, TILE_H),
                );
                if let Some(a) = draw_tile(ui, list, selection, p, idx, rect, icons) {
                    action = Some(a);
                }
            }

            // Clicking the empty space to the right of the last tile in a row
            // clears the selection, matching the list view's dead space.
            if response.clicked() && action.is_none() {
                action = Some(FileListAction::ClearSelection);
            }
        }

        let leftover = ui.available_size();
        if leftover.y > 0.0 {
            let (rect, response) = ui.allocate_exact_size(leftover, Sense::click());
            if response.clicked() {
                action = Some(FileListAction::ClearSelection);
            }
            if response.secondary_clicked() {
                action = Some(FileListAction::ContextMenu {
                    idx: None,
                    pos: response
                        .interact_pointer_pos()
                        .unwrap_or_else(|| rect.center()),
                });
            }
        }
    });

    state.scroll_offset = output.state.offset.y;

    let start_area =
        band_start_area(ui.spacing().scroll, viewport, output.content_size.y > viewport.height());
    let band = track_band(ui, viewport, start_area, output.state.offset.y);
    if let Some(rect) = band.rect {
        draw_band(ui, p, rect, viewport, output.state.offset.y);

        // A band over a grid covers a rectangle of tiles, so the covered range
        // is not contiguous in display order — the tiles in the columns either
        // side of the band, on the rows it spans, are *not* inside it. Reported
        // as the enclosing span rather than the exact set, which is what
        // Explorer does and what reads as predictable while dragging.
        if count > 0 {
            let first_row = (rect.top() / row_pitch).floor().max(0.0) as usize;
            let last_row = (rect.bottom() / row_pitch).floor().max(0.0) as usize;
            let column_of = |x: f32| {
                ((x - LIST_PAD).max(0.0) / (TILE_W + TILE_GAP)).floor() as usize
            };
            let first = first_row * columns + column_of(rect.left()).min(columns - 1);
            let last = last_row * columns + column_of(rect.right()).min(columns - 1);

            action = Some(FileListAction::Marquee {
                from: first.min(count - 1),
                to: last.min(count - 1),
                additive: band.additive,
            });
        }
        ui.ctx().request_repaint();
    }

    action
}

/// One tile: a large icon over a wrapped, centred name.
fn draw_tile(
    ui: &mut Ui,
    list: &EntryList,
    selection: &Selection,
    p: &Palette,
    idx: usize,
    rect: Rect,
    icons: Option<&dyn IconSource>,
) -> Option<FileListAction> {
    let response = ui.interact(rect, ui.id().with(("tile", idx)), Sense::click());

    let selected = selection.is_selected(idx);
    let is_cursor = selection.cursor() == Some(idx);

    if selected {
        ui.painter()
            .rect_filled(rect, RADIUS_CONTROL as f32, p.selection);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, RADIUS_CONTROL as f32, p.hover);
    }
    if is_cursor && !selected {
        ui.painter().rect_stroke(
            rect,
            RADIUS_CONTROL as f32,
            Stroke::new(1.0, p.accent),
            egui::StrokeKind::Inside,
        );
    }

    let kind = list.kind(idx);
    let hidden = list.is_hidden(idx);
    let name = list.name(idx);

    let icon_centre = pos2(rect.center().x, rect.top() + 14.0 + TILE_ICON / 2.0);
    let uv = icons.and_then(|src| src.uv_for(name, kind));

    match (icons, uv) {
        (Some(src), Some(uv)) => {
            let icon_rect = Rect::from_center_size(icon_centre, vec2(TILE_ICON, TILE_ICON));
            let mut mesh = egui::Mesh::with_texture(src.texture());
            let tint = if hidden {
                Color32::from_white_alpha(110)
            } else {
                Color32::WHITE
            };
            mesh.add_rect_with_uv(icon_rect, uv, tint);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        // The drawn glyphs are built for a 16pt slot, so they are scaled up by
        // drawing several concentric copies rather than stretched — simplest is
        // to draw the outline at its natural size, centred. A placeholder is
        // only on screen for a frame or two.
        _ => icons::draw(
            ui.painter(),
            icon_centre,
            icons::for_kind(kind),
            if hidden { p.text_faint } else { p.icon },
        ),
    }

    // Name: up to two lines, centred, ellipsised. Two rather than one because a
    // grid of truncated names is unusable — the distinguishing part of a
    // filename is very often at the end.
    let colour = if hidden { p.text_muted } else { p.text };
    let mut job = egui::text::LayoutJob::simple(
        name.to_owned(),
        FontId::proportional(11.5),
        colour,
        TILE_W - 12.0,
    );
    job.halign = Align2::CENTER_TOP.x();
    job.wrap = egui::text::TextWrapping {
        max_width: TILE_W - 12.0,
        max_rows: 2,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.center().x, rect.top() + 14.0 + TILE_ICON + 8.0),
        galley,
        colour,
    );

    if response.double_clicked() {
        return Some(FileListAction::Activate(idx));
    }
    if response.clicked() {
        let mods = ui.input(|i| i.modifiers);
        let mode = if mods.shift {
            SelectMode::Range
        } else if mods.ctrl || mods.command {
            SelectMode::Toggle
        } else {
            SelectMode::Replace
        };
        return Some(FileListAction::Select { idx, mode });
    }
    if response.secondary_clicked() {
        return Some(FileListAction::ContextMenu {
            idx: Some(idx),
            pos: response
                .interact_pointer_pos()
                .unwrap_or_else(|| rect.center()),
        });
    }
    None
}

/// Draws text clipped to its cell, with an ellipsis when it overflows.
fn draw_cell_text(ui: &Ui, cell: Rect, text: &str, font: &FontId, color: Color32, align: Align2) {
    if text.is_empty() {
        return;
    }

    let inner = cell.shrink2(vec2(CELL_PAD / 2.0, 0.0));

    // Truncating in the layout (rather than clipping the painted result) is
    // what produces the ellipsis, and it also means long names cost no more to
    // lay out than short ones.
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font.clone(), color);
    job.wrap = egui::text::TextWrapping {
        max_width: inner.width(),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };

    let galley = ui.painter().layout_job(job);
    let pos = match align {
        Align2::RIGHT_CENTER => pos2(
            inner.right() - galley.size().x,
            inner.center().y - galley.size().y / 2.0,
        ),
        _ => pos2(inner.left(), inner.center().y - galley.size().y / 2.0),
    };
    ui.painter().galley(pos, galley, color);
}

fn kind_label(kind: EntryKind, name: &str) -> String {
    match kind {
        EntryKind::Directory => "Folder".to_owned(),
        EntryKind::Drive => "Drive".to_owned(),
        EntryKind::Junction => "Junction".to_owned(),
        EntryKind::Symlink => "Shortcut".to_owned(),
        EntryKind::Virtual => "System".to_owned(),
        EntryKind::File => match name.rfind('.') {
            // A leading dot is part of the name (`.gitignore`), not an extension.
            Some(i) if i > 0 && i + 1 < name.len() => {
                format!("{} file", name[i + 1..].to_uppercase())
            }
            _ => "File".to_owned(),
        },
    }
}

/// Number of rows that fit in `height` — used to size PageUp/PageDown.
pub fn rows_per_page(height: f32) -> usize {
    ((height / ROW_HEIGHT).floor() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_labels_use_the_extension() {
        assert_eq!(kind_label(EntryKind::File, "notes.txt"), "TXT file");
        assert_eq!(kind_label(EntryKind::File, "archive.tar.gz"), "GZ file");
        assert_eq!(kind_label(EntryKind::Directory, "src"), "Folder");
    }

    #[test]
    fn dotfiles_and_extensionless_files_are_plain_files() {
        assert_eq!(kind_label(EntryKind::File, ".gitignore"), "File");
        assert_eq!(kind_label(EntryKind::File, "Makefile"), "File");
        // Trailing dot leaves an empty extension.
        assert_eq!(kind_label(EntryKind::File, "weird."), "File");
    }

    #[test]
    fn a_page_is_at_least_one_row() {
        assert_eq!(rows_per_page(0.0), 1);
        assert_eq!(rows_per_page(ROW_HEIGHT * 10.0), 10);
    }

    /// Guards the redesign: 26pt rows were part of what made the list read as a
    /// 2005 file manager. Checked at compile time, since it is a constant.
    const _: () = assert!(ROW_HEIGHT >= 30.0);

    #[test]
    fn a_band_may_not_start_on_the_scroll_bar() {
        // Grabbing the bar used to sweep a selection across everything the drag
        // passed. The list still scrolled, but came back with eighty rows
        // selected, which is not what dragging a scroll bar means.
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let area = band_start_area(egui::style::ScrollStyle::solid(), viewport, true);
        assert!(area.right() < viewport.right(), "the bar's strip is still band area");
        assert_eq!(area.left(), viewport.left());
        assert_eq!(area.height(), viewport.height());
    }

    #[test]
    fn with_nothing_to_scroll_the_band_reaches_the_right_edge() {
        // There is no bar to protect, and losing the last dozen pixels of a
        // short listing to a strip that is not drawn would be arbitrary.
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let area = band_start_area(egui::style::ScrollStyle::solid(), viewport, false);
        assert_eq!(area.right(), viewport.right());
    }

    #[test]
    fn the_reserved_strip_covers_what_egui_senses_not_what_it_draws() {
        // A floating bar is drawn as a thin line but sensed over its full
        // width, so reserving only the visible width leaves most of the grab
        // target inside the band area.
        let floating = egui::style::ScrollStyle::floating();
        assert!(scrollbar_strip(floating, true) >= floating.bar_width);
        assert!(floating.bar_width > floating.floating_width);
    }
}
