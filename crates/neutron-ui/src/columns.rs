//! The column view, as in the Finder.
//!
//! # Shape
//!
//! One column per folder on the way to the current one, left to right, each
//! with the next step of the path highlighted. Then the current folder, which
//! carries the tab's real selection. Then, when exactly one folder is selected,
//! a column showing what is inside it. The columns sit in a horizontal scroll
//! area that keeps the deepest one in sight whenever the path changes.
//!
//! # Who owns what
//!
//! Like the list, this draws snapshots and reports what happened. The listings
//! for the other columns are loaded by the app on a worker; a column whose
//! listing has not arrived draws as loading rather than waiting for it.
//!
//! Only the current column speaks in storage indices, because only its listing
//! is the tab's own. A click anywhere else is reported by *name*, with the
//! folder it was in: the app goes there and selects it, and a name is the
//! identity that survives the listing being read again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, TextStyle, Ui, pos2, vec2};
use neutron_core::entry::EntryKind;
use neutron_core::{EntryList, SelectMode, Selection};

use crate::file_list::{self, FileListAction, IconSource};
use crate::icons::{self, Glyph};
use crate::theme::Palette;

/// Width of a column nobody has resized, in points.
pub const COLUMN_WIDTH: f32 = 240.0;
/// How narrow or wide a column can be dragged. Narrower than the minimum and
/// an icon and three letters do not fit; wider than the maximum and one column
/// is the whole pane.
/// Tallest a preview gets in the info column.
const PREVIEW_MAX_H: f32 = 220.0;
/// Narrowest the info column gets: enough to read a name and its facts.
const INFO_MIN: f32 = 280.0;
const MIN_WIDTH: f32 = 140.0;
const MAX_WIDTH: f32 = 720.0;
/// Width of the grab zone over a column's right edge.
const GRIP: f32 = 9.0;
/// How far a column's rows stop short of its right edge, so its scroll bar is
/// not under the grip.
const GRIP_INSET: f32 = 4.0;
/// Names measured when a column is fitted to its contents. A folder of half a
/// million files is fitted to its first few thousand, which is plenty to find
/// the long ones and costs a few milliseconds instead of seconds.
const FIT_SAMPLE: usize = 5_000;
/// Row height. Tighter than the list's, because a column carries no metadata
/// and a path five folders deep has to fit on one screen.
const ROW: f32 = 34.0;
/// Inset of the row highlight from the column edges.
const PAD: f32 = 6.0;
const ICON_SLOT: f32 = 26.0;
const ICON_DRAW: f32 = 18.0;
/// Room kept at the right of a row for the chevron that marks a folder.
const CHEVRON_SLOT: f32 = 18.0;

/// What a column's listing is doing.
pub enum Contents<'a> {
    Ready(&'a EntryList),
    Loading,
    Failed(&'a str),
}

/// The part a column plays.
pub enum Role<'a> {
    /// The folder the tab is in. Its rows carry the tab's real selection, and
    /// what happens to them is reported in the list's own terms.
    Current {
        selection: &'a Selection,
        /// The rename in progress, if it is in this column: row, text, and
        /// which rename it is.
        rename: Option<(usize, &'a str, u64)>,
        /// Storage index to bring into view. Applied when it changes, so a
        /// value the app leaves standing does not pin the column in place.
        scroll_to: Option<usize>,
    },
    /// Any other column: a folder on the way here, the current folder while
    /// it is still loading, or the contents of the selected folder.
    /// `highlight` names the entry drawn as chosen — the next step on the path,
    /// or what will be selected once the current folder lands.
    Named { highlight: Option<&'a str> },
}

/// One column, ready to draw.
pub struct Column<'a> {
    pub dir: &'a Path,
    pub contents: Contents<'a>,
    pub role: Role<'a>,
    /// Names in this folder that sit on the clipboard as a cut, drawn faded.
    pub cut: Option<&'a HashSet<String>>,
    pub icons: Option<&'a dyn IconSource>,
}

/// The file selected in the current column, described in a column of its own
/// at the end — the Finder's preview column.
pub struct Info<'a> {
    pub name: &'a str,
    pub kind: EntryKind,
    /// "Markdown", "PNG image" — the list's Kind column.
    pub kind_label: String,
    pub size: Option<u64>,
    /// Milliseconds since the epoch, as on an entry.
    pub modified: i64,
    /// Where the file is, for the "Where" line.
    pub location: String,
    /// Offer Quick Look: only where the system has it.
    pub quick_look: bool,
    /// What is inside the file, shown in place of its tile when there is one.
    pub preview: Option<crate::thumb::Thumb>,
}

/// Something the user did that the app must act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnAction {
    /// Open the file the info column describes.
    Open,
    /// Show it in Quick Look.
    QuickLook,
    /// In the current column, in the list's own terms.
    Current(FileListAction),
    /// A click in any other column. Go to `dir` and select `entry`, or nothing
    /// for a click on empty space. `double` is a double-click: open it.
    Pick {
        dir: PathBuf,
        entry: Option<(String, EntryKind)>,
        double: bool,
    },
}

/// Draws the columns side by side. Returns the first action taken.
pub fn show(ui: &mut Ui, p: &Palette, columns: &[Column<'_>], info: Option<&Info<'_>>) -> Option<ColumnAction> {
    let mut action = None;
    let viewport = ui.available_rect_before_wrap();
    let widths = widths_of(ui.ctx(), columns);
    let total: f32 = widths.iter().sum();

    // The columns sit in a panel pressed into the pane: a well, so the
    // dividers between them are shifts of tone inside it rather than lines
    // across the pane.
    let corners = egui::CornerRadius::same(crate::theme::RADIUS_PANEL);
    crate::theme::paint_well(ui.painter(), viewport, corners, p);
    let viewport_clip = viewport.shrink(1.0);
    ui.set_clip_rect(ui.clip_rect().intersect(viewport_clip));

    // The deepest column is the one being worked in, so it is brought into
    // view whenever the set of columns changes. Not every frame: the user may
    // have scrolled left to look at an earlier folder, and snapping back while
    // they read would be the view fighting them.
    // The info column counts: a file just picked is described past the last
    // column, and scrolling to the column but not to the description of what
    // was picked leaves it cut off at the pane's edge.
    let info_width = info.map_or(0.0, |_| (viewport.width() - total).max(INFO_MIN));
    let shape = egui::Id::new((columns.iter().map(|c| c.dir).collect::<Vec<_>>(), info.is_some()));
    let memory = ui.id().with("neutron-columns-shape");
    let mut outer = egui::ScrollArea::horizontal()
        .id_salt("neutron-columns")
        .auto_shrink([false; 2]);
    if ui.data(|d| d.get_temp::<egui::Id>(memory)) != Some(shape) {
        ui.data_mut(|d| d.insert_temp(memory, shape));
        outer = outer.horizontal_scroll_offset((total + info_width - viewport.width()).max(0.0));
    }

    outer.show(ui, |ui| {
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut rects = Vec::with_capacity(columns.len());
            for (column, &width) in columns.iter().zip(&widths) {
                let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                rects.push(rect);
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let content =
                    Rect::from_min_max(rect.min, pos2(rect.right() - GRIP_INSET, rect.bottom()));
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content)
                        .id_salt(("neutron-column", column.dir))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(content.intersect(ui.clip_rect()));
                if let Some(a) = draw_column(&mut child, p, column) {
                    action = Some(a);
                }
            }

            // The selected file, described at the end: whatever room the
            // columns leave, and never less than enough to read it.
            if let Some(info) = info {
                let (rect, _) = ui.allocate_exact_size(vec2(info_width, height), Sense::hover());
                if let Some(a) = draw_info(ui, p, rect, info) {
                    action = Some(a);
                }
            }

            // After every column, so each grip wins the pointer over the rows
            // on both sides of the edge it sits on.
            let origin = rects.first().map_or(0.0, |r| r.left());
            for (index, (column, rect)) in columns.iter().zip(&rects).enumerate() {
                if ui.is_rect_visible(rect.expand2(vec2(GRIP, 0.0))) {
                    grip(ui, p, column, *rect, index, origin);
                }
            }
        });
    });

    action
}

/// The info column: a large tile for the file's kind, its name and size, a
/// few facts, and Open and Quick Look.
fn draw_info(ui: &mut Ui, p: &Palette, rect: Rect, info: &Info<'_>) -> Option<ColumnAction> {
    let mut action = None;
    let painter = ui.painter().clone();
    let inner = rect.shrink2(vec2(26.0, 26.0));
    let mut y = inner.top();

    match info.preview {
        // As large as the column allows, up to a height that still leaves the
        // facts and the buttons on screen.
        Some(thumb) => {
            let space = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), PREVIEW_MAX_H));
            let drawn = crate::thumb::fit(space, thumb.size, false).0;
            let space = space.translate(vec2(0.0, -(space.height() - drawn.height()) / 2.0));
            let drawn = crate::thumb::paint(&painter, space, thumb, 12.0, false, false);
            y = drawn.bottom() + 18.0;
        }
        None => {
            let kind = icons::FileKind::of(info.name, info.kind);
            let tile = Rect::from_center_size(pos2(inner.center().x, y + 42.0), vec2(84.0, 84.0));
            icons::tile(&painter, tile, kind, p, false);
            y = tile.bottom() + 18.0;
        }
    }

    let name = painter.layout(
        info.name.to_owned(),
        crate::theme::strong(ui.ctx(), 17.0),
        p.text,
        inner.width(),
    );
    painter.galley(pos2(inner.center().x - name.size().x / 2.0, y), name.clone(), p.text);
    y += name.size().y + 4.0;

    let detail = match info.size {
        Some(bytes) => format!("{} · {}", info.kind_label, crate::format::size(Some(bytes))),
        None => info.kind_label.clone(),
    };
    painter.text(pos2(inner.center().x, y + 8.0), Align2::CENTER_CENTER, detail, FontId::proportional(12.5), p.text_muted);
    y += 36.0;

    for (label, value) in [
        ("Modified", crate::format::timestamp(info.modified)),
        ("Where", info.location.clone()),
    ] {
        painter.text(pos2(inner.left(), y + 8.0), Align2::LEFT_CENTER, label, FontId::proportional(12.5), p.text_faint);
        let room = inner.width() - 80.0;
        let mut job = egui::text::LayoutJob::simple_singleline(value, FontId::proportional(12.5), p.text_soft());
        job.wrap = egui::text::TextWrapping {
            max_width: room.max(1.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = painter.layout_job(job);
        painter.galley(pos2(inner.right() - galley.size().x, y + 8.0 - galley.size().y / 2.0), galley, p.text_soft());
        y += 26.0;
    }

    // The buttons sit at the foot of the column.
    let buttons = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 38.0), inner.max);
    let halves = if info.quick_look {
        let mid = buttons.center().x;
        vec![
            (Rect::from_min_max(buttons.min, pos2(mid - 4.0, buttons.bottom())), "Open", true),
            (Rect::from_min_max(pos2(mid + 4.0, buttons.top()), buttons.max), "Quick Look", false),
        ]
    } else {
        vec![(buttons, "Open", true)]
    };
    for (r, label, primary) in halves {
        let response = ui.interact(r, ui.id().with(("info-button", label)), Sense::click());
        let corners = crate::theme::pill(r.height());
        if primary {
            ui.painter().rect_filled(r, corners, if response.hovered() { p.accent_hover } else { p.accent });
        } else {
            ui.painter().rect_filled(r, corners, Color32::from_white_alpha(if response.hovered() { 20 } else { 13 }));
        }
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            crate::theme::strong(ui.ctx(), 13.0),
            if primary { p.inset } else { p.text },
        );
        if response.clicked() {
            action = Some(if primary { ColumnAction::Open } else { ColumnAction::QuickLook });
        }
    }
    action
}

// --- widths -----------------------------------------------------------------
//
// Kept in egui's memory, like a scroll offset, because they are a property of
// how the view is laid out rather than of anything the app decides. The width
// a column has not been given is shared by every column and kept across
// launches; a width given to one folder lasts the session.

type Widths = HashMap<PathBuf, f32>;

fn default_width_id() -> egui::Id {
    egui::Id::new("neutron-columns-default-width")
}

fn widths_id() -> egui::Id {
    egui::Id::new("neutron-columns-widths")
}

fn widths_of(ctx: &egui::Context, columns: &[Column<'_>]) -> Vec<f32> {
    ctx.data_mut(|d| {
        let default = d.get_persisted::<f32>(default_width_id()).unwrap_or(COLUMN_WIDTH);
        let given = d.get_temp_mut_or_default::<Widths>(widths_id());
        columns
            .iter()
            .map(|c| given.get(c.dir).copied().unwrap_or(default))
            .collect()
    })
}

/// Gives the column showing `dir` a width — or, with `all`, makes it the width
/// of every column, forgetting any one folder was given.
fn set_width(ctx: &egui::Context, dir: &Path, width: f32, all: bool) {
    let width = width.clamp(MIN_WIDTH, MAX_WIDTH);
    ctx.data_mut(|d| {
        if all {
            d.insert_persisted(default_width_id(), width);
            d.get_temp_mut_or_default::<Widths>(widths_id()).clear();
        } else {
            d.get_temp_mut_or_default::<Widths>(widths_id())
                .insert(dir.to_path_buf(), width);
        }
    });
}

/// The divider on a column's right edge, which resizes it.
///
/// Drag to set this column's width; with Alt (Option on a Mac) held, every
/// column's. Double-click to fit the column to its longest name. The same
/// gestures as the Finder's.
fn grip(ui: &mut Ui, p: &Palette, column: &Column<'_>, rect: Rect, index: usize, origin: f32) {
    let zone = Rect::from_x_y_ranges(
        rect.right() - GRIP_INSET - 1.0..=rect.right() + GRIP - GRIP_INSET - 1.0,
        rect.y_range(),
    );
    // Keyed by folder, never by position: the edge moves while it is being
    // dragged, and egui keeps a drag attached to the id it began on, so an id
    // that moved with the edge would lose the drag after one frame.
    let id = ui.id().with(("neutron-column-grip", column.dir));
    let response = ui.interact(zone, id, Sense::click_and_drag());
    let active = response.hovered() || response.dragged();
    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
    }

    let colour = if active { p.accent } else { Color32::from_white_alpha(11) };
    ui.painter().vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(if active { 1.5 } else { 1.0 }, colour),
    );
    // A small handle at the foot of the edge, as the Finder draws one, so a
    // hairline reads as something that can be taken hold of.
    let foot = pos2(rect.right() - 0.5, rect.bottom() - 18.0);
    for dx in [-2.0, 2.0] {
        ui.painter().line_segment(
            [pos2(foot.x + dx, foot.y - 5.0), pos2(foot.x + dx, foot.y + 5.0)],
            Stroke::new(1.0, if active { p.accent } else { p.text_faint }),
        );
    }

    let all = ui.input(|i| i.modifiers.alt);
    if response.double_clicked() {
        if let Some(width) = fitted_width(ui, column) {
            set_width(ui.ctx(), column.dir, width, all);
        }
        return;
    }

    // Where on the grip it was taken hold of, so the edge stays under the
    // pointer rather than jumping to it by the few points between the two.
    // Measured from where the button went down, not from where the pointer is
    // when egui decides a drag has begun — by then it has already travelled
    // past the drag threshold.
    let grab = id.with("grab");
    if response.drag_started() {
        if let Some(pressed) = ui.input(|i| i.pointer.press_origin()) {
            ui.data_mut(|d| d.insert_temp(grab, rect.right() - pressed.x));
        }
    }
    if response.dragged() && response.drag_delta().x != 0.0 {
        let Some(pointer) = response.interact_pointer_pos() else {
            return;
        };
        let edge = pointer.x + ui.data(|d| d.get_temp::<f32>(grab)).unwrap_or(0.0);
        let width = if all {
            // Every column takes the same width, so this edge sits that many
            // widths from the first column's left — solved for the width, the
            // edge follows the pointer.
            (edge - origin) / (index + 1) as f32
        } else {
            edge - rect.left()
        };
        set_width(ui.ctx(), column.dir, width, all);
    }
}

/// The width that shows every name in the column without an ellipsis.
fn fitted_width(ui: &Ui, column: &Column<'_>) -> Option<f32> {
    let Contents::Ready(list) = column.contents else {
        return None;
    };
    let font = TextStyle::Body.resolve(ui.style());
    let widest = list
        .order()
        .iter()
        .take(FIT_SAMPLE)
        .map(|&i| {
            // The colour is irrelevant to a measurement.
            ui.painter()
                .layout_no_wrap(list.name(i as usize).to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    // Everything in a row that is not the name, as `draw_entry` lays it out,
    // plus the name cell's own padding and a little air.
    let chrome = PAD * 2.0 + 6.0 + ICON_SLOT + CHEVRON_SLOT + GRIP_INSET + 10.0 + 8.0;
    Some(widest + chrome)
}

fn draw_column(ui: &mut Ui, p: &Palette, column: &Column<'_>) -> Option<ColumnAction> {
    let rect = ui.max_rect();
    let list = match column.contents {
        Contents::Ready(list) => list,
        Contents::Loading => {
            note(ui, rect, "Loading…", p.text_faint);
            return None;
        }
        Contents::Failed(error) => {
            note(ui, rect, "Could not open this folder", p.danger);
            ui.painter().text(
                pos2(rect.center().x, rect.center().y + 18.0),
                Align2::CENTER_CENTER,
                error,
                FontId::proportional(11.0),
                p.text_muted,
            );
            return None;
        }
    };

    let rows = list.order().len();
    let scroll = egui::ScrollArea::vertical()
        .id_salt("rows")
        .auto_shrink([false; 2]);
    let scroll = match bring_into_view(ui, column, list) {
        Some(offset) => scroll.vertical_scroll_offset(offset),
        None => scroll,
    };

    let mut action = None;
    let output = scroll.show_rows(ui, ROW, rows, |ui, visible| {
        for position in visible {
            let idx = list.at(position);
            if let Some(a) = draw_entry(ui, p, column, list, idx) {
                action = Some(a);
            }
        }
    });
    // Remembered rather than derived: the scroll area hashes its salt into an
    // id of its own, and reproducing that here would be guessing at egui's
    // internals. See `bring_into_view`.
    ui.data_mut(|d| d.insert_temp(scroll_id_key(ui), output.id));

    if rows == 0 {
        note(ui, rect, "Empty folder", p.text_faint);
    }

    // The space under the last row. `show_rows` sizes its content to exactly
    // the rows it was given, so this has to be interacted with directly.
    let inner = output.inner_rect;
    let bottom = inner.top() + rows as f32 * ROW - output.state.offset.y;
    if bottom < inner.bottom() {
        let dead = Rect::from_min_max(pos2(inner.left(), bottom.max(inner.top())), inner.max);
        let response = ui.interact(dead, ui.id().with("empty"), Sense::click());
        let pointer = response.interact_pointer_pos().unwrap_or_else(|| dead.center());
        match column.role {
            Role::Current { .. } => {
                if response.clicked() {
                    action = Some(ColumnAction::Current(FileListAction::ClearSelection));
                }
                if response.secondary_clicked() {
                    action = Some(ColumnAction::Current(FileListAction::ContextMenu {
                        idx: None,
                        pos: pointer,
                    }));
                }
            }
            // Clicking the empty part of another column makes that folder the
            // current one with nothing selected, as the Finder does.
            Role::Named { .. } => {
                if response.clicked() {
                    action = Some(ColumnAction::Pick {
                        dir: column.dir.to_path_buf(),
                        entry: None,
                        double: false,
                    });
                }
            }
        }
    }

    action
}

/// The vertical offset to jump to this frame, if any.
///
/// Two cases. A column appearing for the first time opens with its chosen row
/// in the middle, so the folder that leads onward is visible without hunting
/// for it. And the current column follows the keyboard cursor, moving the
/// least distance that brings it into view — but only when the requested row
/// changes, so the user can still scroll away from it with the wheel.
fn bring_into_view(ui: &Ui, column: &Column<'_>, list: &EntryList) -> Option<f32> {
    let state = ui
        .data(|d| d.get_temp::<egui::Id>(scroll_id_key(ui)))
        .and_then(|id| egui::scroll_area::State::load(ui.ctx(), id));
    let height = ui.available_height();
    let row_top = |idx: usize| list.rank(idx).map(|row| row as f32 * ROW);
    let applied = ui.id().with("applied-scroll");

    if state.is_none() {
        let chosen = match &column.role {
            // The cursor, not `scroll_to`: the app leaves that standing across
            // a navigation, so in a folder just entered it is an index into
            // the folder that was left. Recorded as applied for the same
            // reason, so the next frame does not go and act on it.
            Role::Current { selection, scroll_to, .. } => {
                ui.data_mut(|d| d.insert_temp(applied, *scroll_to));
                selection.cursor()
            }
            Role::Named { highlight } => highlight.and_then(|name| find(list, name)),
        };
        let top = chosen.and_then(row_top)?;
        return Some((top - (height - ROW) / 2.0).max(0.0));
    }

    let Role::Current { scroll_to, .. } = &column.role else {
        return None;
    };
    if ui.data(|d| d.get_temp::<Option<usize>>(applied)) == Some(*scroll_to) {
        return None;
    }
    ui.data_mut(|d| d.insert_temp(applied, *scroll_to));

    let top = scroll_to.and_then(row_top)?;
    let current = state.map_or(0.0, |s| s.offset.y);
    if top < current {
        Some(top)
    } else if top + ROW > current + height {
        Some(top + ROW - height)
    } else {
        None
    }
}

/// Where a column remembers its scroll area's id.
fn scroll_id_key(ui: &Ui) -> egui::Id {
    ui.id().with("rows-scroll-id")
}

/// The storage index of the entry called `name`. A scan, but only run when a
/// column first appears.
fn find(list: &EntryList, name: &str) -> Option<usize> {
    (0..list.len()).find(|&i| list.name(i) == name)
}

/// One line of centred text in an otherwise empty column.
fn note(ui: &Ui, rect: Rect, text: &str, colour: Color32) {
    ui.painter().text(
        pos2(rect.center().x, rect.top() + 48.0),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(12.0),
        colour,
    );
}

fn draw_entry(
    ui: &mut Ui,
    p: &Palette,
    column: &Column<'_>,
    list: &EntryList,
    idx: usize,
) -> Option<ColumnAction> {
    let (outer, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if !ui.is_rect_visible(outer) {
        return None;
    }
    let rect = outer.shrink2(vec2(PAD, 1.0));

    let name = list.name(idx);
    let kind = list.kind(idx);

    let (selected, is_cursor, rename) = match &column.role {
        Role::Current { selection, rename, .. } => (
            selection.is_selected(idx),
            selection.cursor() == Some(idx),
            rename
                .filter(|(row, ..)| *row == idx)
                .map(|(_, text, generation)| (text, generation)),
        ),
        Role::Named { .. } => (false, false, None),
    };
    let chosen = matches!(column.role, Role::Named { highlight: Some(h) } if h == name);

    // The current column's selection looks like the list's: the accent wash
    // and a faint ring of it. A chosen row elsewhere — the way here — is a
    // neutral lift instead, because only one column is the one keys act on.
    let corners = egui::CornerRadius::same(11);
    if selected {
        ui.painter().rect_filled(rect, corners, p.selection);
        ui.painter().rect_stroke(
            rect,
            corners,
            Stroke::new(1.0, crate::theme::with_alpha(p.accent, 60)),
            egui::StrokeKind::Inside,
        );
    } else if chosen {
        ui.painter().rect_filled(rect, corners, Color32::from_white_alpha(14));
    } else if response.hovered() {
        ui.painter().rect_filled(rect, corners, p.hover);
    }
    if is_cursor && !selected {
        ui.painter().rect_stroke(
            rect,
            corners,
            Stroke::new(1.5, crate::theme::with_alpha(p.accent, 128)),
            egui::StrokeKind::Inside,
        );
    }

    let faded = list.is_hidden(idx) || column.cut.is_some_and(|names| names.contains(name));

    // --- icon ---
    let centre = pos2(rect.left() + 6.0 + ICON_SLOT / 2.0, outer.center().y);
    // Applications keep their own icon; everything else is a glyph tinted for
    // its kind, as in the list.
    let application = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("app"));
    let uv = column
        .icons
        .filter(|_| application)
        .and_then(|src| src.uv_for(name, kind));
    match (column.icons, uv) {
        (Some(src), Some(uv)) => {
            let icon = Rect::from_center_size(centre, vec2(ICON_DRAW, ICON_DRAW));
            let mut mesh = egui::Mesh::with_texture(src.texture());
            let tint = if faded {
                Color32::from_white_alpha(110)
            } else {
                Color32::WHITE
            };
            mesh.add_rect_with_uv(icon, uv, tint);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        _ => {
            let kind_of = icons::FileKind::of(name, kind);
            let tint = kind_of.tint(p);
            icons::draw(
                ui.painter(),
                centre,
                kind_of.glyph(),
                if faded { crate::theme::with_alpha(tint, 130) } else { tint },
            );
        }
    }

    // --- name ---
    let name_left = rect.left() + 6.0 + ICON_SLOT;
    let name_rect = Rect::from_min_max(
        pos2(name_left, outer.top()),
        pos2(rect.right() - CHEVRON_SLOT, outer.bottom()),
    );
    let mut rename_action = None;
    if let Some((text, generation)) = rename {
        rename_action = file_list::rename_box(ui, p, name_rect, text, generation);
    } else {
        let body = TextStyle::Body.resolve(ui.style());
        file_list::draw_name_text(
            ui,
            name_rect,
            name,
            kind,
            &body,
            if faded {
                p.text_muted
            } else if selected || chosen {
                p.text
            } else {
                p.text_soft()
            },
            p.text_faint,
            list.tag_colors(idx),
            p.inset,
        );
    }

    // --- chevron: this leads somewhere ---
    if kind.is_container() {
        icons::draw(
            ui.painter(),
            pos2(rect.right() - CHEVRON_SLOT / 2.0, outer.center().y),
            Glyph::ChevronRight,
            if selected { p.accent } else { crate::theme::with_alpha(p.text_faint, 170) },
        );
    }

    // --- interaction ---
    //
    // The rename box wins outright, for the reason given in the list: a click
    // inside it must place the caret, not re-select the row.
    if let Some(a) = rename_action {
        return Some(ColumnAction::Current(a));
    }
    if rename.is_some() {
        return None;
    }

    match column.role {
        Role::Current { .. } => {
            if response.double_clicked() {
                return Some(ColumnAction::Current(FileListAction::Activate(idx)));
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
                return Some(ColumnAction::Current(FileListAction::Select { idx, mode }));
            }
            if response.secondary_clicked() {
                return Some(ColumnAction::Current(FileListAction::ContextMenu {
                    idx: Some(idx),
                    pos: response
                        .interact_pointer_pos()
                        .unwrap_or_else(|| outer.center()),
                }));
            }
        }
        Role::Named { .. } => {
            // A right-click picks the row too: what it is about has to be the
            // selection before any menu can act on it.
            let double = response.double_clicked();
            if double || response.clicked() || response.secondary_clicked() {
                return Some(ColumnAction::Pick {
                    dir: column.dir.to_path_buf(),
                    entry: Some((name.to_owned(), kind)),
                    double,
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use neutron_core::entry::{Entry, SyncState};

    fn listing(names: &[(&str, EntryKind)]) -> EntryList {
        let mut list: EntryList = names
            .iter()
            .map(|(name, kind)| Entry {
                name: (*name).to_owned(),
                kind: *kind,
                size: 0,
                modified: 0,
                created: 0,
                attrs: 0,
                sync: SyncState::None,
            })
            .collect();
        neutron_core::sort::apply(&mut list, Default::default(), true);
        list
    }

    fn run(columns: &[Column<'_>], events: Vec<egui::Event>) -> Option<ColumnAction> {
        let ctx = egui::Context::default();
        let mut action = None;
        let input = |events| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        // A first frame to lay out, so the second has something to hit.
        let mut output = ctx.run_ui(input(Vec::new()), |ui| {
            show(ui, &Palette::LIGHT, columns, None);
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(input(events), |ui| {
            action = show(ui, &Palette::LIGHT, columns, None);
        });
        output.textures_delta.clear();
        action
    }

    fn click_at(x: f32, y: f32) -> Vec<egui::Event> {
        let at = pos2(x, y);
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn a_click_in_another_column_names_the_folder_and_the_entry() {
        let parent = listing(&[("docs", EntryKind::Directory), ("src", EntryKind::Directory)]);
        let here = listing(&[("main.rs", EntryKind::File)]);
        let selection = Selection::new();
        let columns = [
            Column {
                dir: Path::new("/project"),
                contents: Contents::Ready(&parent),
                role: Role::Named { highlight: Some("src") },
                cut: None,
                icons: None,
            },
            Column {
                dir: Path::new("/project/src"),
                contents: Contents::Ready(&here),
                role: Role::Current { selection: &selection, rename: None, scroll_to: None },
                cut: None,
                icons: None,
            },
        ];

        // The first row of the first column: "docs".
        let action = run(&columns, click_at(100.0, ROW / 2.0));
        assert_eq!(
            action,
            Some(ColumnAction::Pick {
                dir: PathBuf::from("/project"),
                entry: Some(("docs".to_owned(), EntryKind::Directory)),
                double: false,
            })
        );
    }

    #[test]
    fn a_click_in_the_current_column_selects_by_index() {
        let parent = listing(&[("src", EntryKind::Directory)]);
        let here = listing(&[("a.rs", EntryKind::File), ("b.rs", EntryKind::File)]);
        let selection = Selection::new();
        let columns = [
            Column {
                dir: Path::new("/project"),
                contents: Contents::Ready(&parent),
                role: Role::Named { highlight: Some("src") },
                cut: None,
                icons: None,
            },
            Column {
                dir: Path::new("/project/src"),
                contents: Contents::Ready(&here),
                role: Role::Current { selection: &selection, rename: None, scroll_to: None },
                cut: None,
                icons: None,
            },
        ];

        // Second row of the second column.
        let action = run(&columns, click_at(COLUMN_WIDTH + 100.0, ROW * 1.5));
        let b = here.at(1);
        assert_eq!(
            action,
            Some(ColumnAction::Current(FileListAction::Select {
                idx: b,
                mode: SelectMode::Replace,
            }))
        );
    }

    #[test]
    fn a_scrolled_column_is_not_pulled_back_to_its_highlight() {
        // The chosen row is centred when a column first appears, and only
        // then. Re-centring on every frame would make a long column impossible
        // to scroll.
        let names: Vec<String> = (0..200).map(|i| format!("f{i:03}")).collect();
        let entries: Vec<(&str, EntryKind)> =
            names.iter().map(|n| (n.as_str(), EntryKind::Directory)).collect();
        let parent = listing(&entries);
        let columns = [Column {
            dir: Path::new("/many"),
            contents: Contents::Ready(&parent),
            role: Role::Named { highlight: Some("f100") },
            cut: None,
            icons: None,
        }];

        let ctx = egui::Context::default();
        let frame = |events: Vec<egui::Event>| {
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ui| action = show(ui, &Palette::LIGHT, &columns, None),
            );
            output.textures_delta.clear();
            action
        };
        let top_row = |action: Option<ColumnAction>| match action {
            Some(ColumnAction::Pick { entry: Some((name, _)), .. }) => name,
            other => panic!("expected a pick, got {other:?}"),
        };

        frame(Vec::new());
        frame(Vec::new());
        let first = top_row(frame(click_at(100.0, ROW / 2.0)));
        assert_ne!(first, "f000", "the highlighted row was not brought into view");

        // Wheel all the way up, and give any animation time to finish.
        frame(vec![egui::Event::PointerMoved(pos2(100.0, 300.0))]);
        frame(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 100_000.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }]);
        for _ in 0..60 {
            frame(Vec::new());
        }

        assert_eq!(top_row(frame(click_at(100.0, ROW / 2.0))), "f000", "the column was pulled back");
    }

    /// Frames with a clock, for gestures that span several of them.
    struct Frames {
        ctx: egui::Context,
        time: f64,
    }

    impl Frames {
        fn new() -> Self {
            Frames { ctx: egui::Context::default(), time: 0.0 }
        }

        fn frame(
            &mut self,
            columns: &[Column<'_>],
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> Option<ColumnAction> {
            self.time += 1.0 / 60.0;
            let mut action = None;
            let events = std::iter::once(egui::Event::ModifiersChanged(modifiers))
                .chain(events)
                .collect();
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 600.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| action = show(ui, &Palette::LIGHT, columns, None),
            );
            output.textures_delta.clear();
            action
        }

        fn press(&mut self, columns: &[Column<'_>], at: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) {
            self.frame(
                columns,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers,
                    },
                ],
                modifiers,
            );
        }

        /// Drags from `from` to `to` in a few steps.
        fn drag(&mut self, columns: &[Column<'_>], from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
            self.frame(columns, vec![egui::Event::PointerMoved(from)], modifiers);
            self.press(columns, from, true, modifiers);
            for step in 1..=4 {
                let at = from + (to - from) * (step as f32 / 4.0);
                self.frame(columns, vec![egui::Event::PointerMoved(at)], modifiers);
            }
            self.press(columns, to, false, modifiers);
        }

        /// Which folder a plain click at `x` lands in, a second after anything
        /// before it so it cannot be read as part of a double-click.
        fn folder_at(&mut self, columns: &[Column<'_>], x: f32) -> PathBuf {
            self.time += 1.0;
            let none = egui::Modifiers::NONE;
            let at = pos2(x, ROW / 2.0);
            self.frame(columns, vec![egui::Event::PointerMoved(at)], none);
            let events = vec![
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: none },
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: none },
            ];
            match self.frame(columns, events, none) {
                Some(ColumnAction::Pick { dir, .. }) => dir,
                other => panic!("expected a pick at x={x}, got {other:?}"),
            }
        }
    }

    fn named<'a>(dir: &'a str, list: &'a EntryList) -> Column<'a> {
        Column {
            dir: Path::new(dir),
            contents: Contents::Ready(list),
            role: Role::Named { highlight: None },
            cut: None,
            icons: None,
        }
    }

    #[test]
    fn dragging_an_edge_resizes_that_column_alone() {
        let list = listing(&[("entry", EntryKind::File)]);
        let columns = [named("/a", &list), named("/b", &list), named("/c", &list)];
        let mut f = Frames::new();
        f.frame(&columns, Vec::new(), egui::Modifiers::NONE);
        assert_eq!(f.folder_at(&columns, COLUMN_WIDTH + 60.0), PathBuf::from("/b"));

        let edge = COLUMN_WIDTH - 2.0;
        f.drag(&columns, pos2(edge, 200.0), pos2(edge + 100.0, 200.0), egui::Modifiers::NONE);

        // The first column now reaches past where the second used to start...
        assert_eq!(f.folder_at(&columns, COLUMN_WIDTH + 60.0), PathBuf::from("/a"));
        // ...and the second kept its width, moved along by the difference.
        assert_eq!(f.folder_at(&columns, COLUMN_WIDTH * 2.0 + 80.0), PathBuf::from("/b"));
        assert_eq!(f.folder_at(&columns, COLUMN_WIDTH * 2.0 + 120.0), PathBuf::from("/c"));
    }

    #[test]
    fn option_dragging_an_edge_resizes_every_column() {
        let list = listing(&[("entry", EntryKind::File)]);
        let columns = [named("/a", &list), named("/b", &list), named("/c", &list)];
        let mut f = Frames::new();
        f.frame(&columns, Vec::new(), egui::Modifiers::NONE);

        // The second column's edge, from 480 to 400: two columns in 400 points
        // makes every column 200 wide.
        let edge = COLUMN_WIDTH * 2.0 - 2.0;
        f.drag(&columns, pos2(edge, 200.0), pos2(398.0, 200.0), egui::Modifiers::ALT);

        assert_eq!(f.folder_at(&columns, 190.0), PathBuf::from("/a"));
        assert_eq!(f.folder_at(&columns, 215.0), PathBuf::from("/b"));
        assert_eq!(f.folder_at(&columns, 415.0), PathBuf::from("/c"));
    }

    #[test]
    fn double_clicking_an_edge_fits_the_column_to_its_longest_name() {
        let long = listing(&[(
            "a name far too long for a column of the usual width to show.txt",
            EntryKind::File,
        )]);
        let short = listing(&[("entry", EntryKind::File)]);
        let columns = [named("/a", &long), named("/b", &short)];
        let mut f = Frames::new();
        f.frame(&columns, Vec::new(), egui::Modifiers::NONE);

        let edge = pos2(COLUMN_WIDTH - 2.0, 200.0);
        let none = egui::Modifiers::NONE;
        f.frame(&columns, vec![egui::Event::PointerMoved(edge)], none);
        for _ in 0..2 {
            f.press(&columns, edge, true, none);
            f.press(&columns, edge, false, none);
        }

        assert_eq!(f.folder_at(&columns, COLUMN_WIDTH + 40.0), PathBuf::from("/a"));
    }

    #[test]
    fn a_column_still_loading_reports_nothing() {
        let columns = [Column {
            dir: Path::new("/slow"),
            contents: Contents::Loading,
            role: Role::Named { highlight: None },
            cut: None,
            icons: None,
        }];
        assert_eq!(run(&columns, click_at(100.0, ROW / 2.0)), None);
    }
}
