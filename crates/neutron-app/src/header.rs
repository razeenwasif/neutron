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
use neutron_ui::file_list::ViewMode;
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, RADIUS_CONTROL};

/// Height of the navigation row.
const NAV_ROW: f32 = 48.0;
/// Height of the large title block, when it is shown.
const TITLE_ROW: f32 = 44.0;
/// Below this pane height the title is dropped in favour of more rows.
const TITLE_MIN_PANE_HEIGHT: f32 = 300.0;
/// Below this pane width the filter field is dropped; the breadcrumb and the
/// navigation arrows matter more on a narrow pane.
const FILTER_MIN_PANE_WIDTH: f32 = 460.0;
const FILTER_WIDTH: f32 = 220.0;
/// Narrowest the filter gets in a narrow pane.
const FILTER_MIN: f32 = 120.0;

/// A control in the header was used.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderAction {
    Back,
    /// The address field's text changed.
    SetAddress(String),
    /// Enter in the address field.
    SubmitAddress,
    /// Escape, or a click somewhere else.
    CancelAddress,
    Forward,
    Up,
    Navigate(NodeId),
    SetFilter(String),
    ToggleHidden,
    Split(Axis),
    /// One of the view buttons.
    SetView(ViewMode),
}

/// Everything the header needs to draw itself. Passed as a struct rather than
/// eight positional arguments, all of which would be `bool`.
pub struct Header<'a> {
    pub location: &'a NodeId,
    pub can_back: bool,
    pub can_forward: bool,
    pub has_parent: bool,
    pub show_hidden: bool,
    pub view: ViewMode,
    pub filter: &'a str,
    /// The address field's contents while it is open. `None` draws the
    /// breadcrumb instead: the two share a strip because they answer the same
    /// question, and only one of them is ever the one wanted.
    pub address: Option<&'a str>,
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
    // The dashboard is not a folder: nothing to filter, no views to switch
    // between, no hidden files, and it titles itself.
    let dashboard = matches!(h.location, NodeId::Home);
    let show_title = full.height() >= TITLE_MIN_PANE_HEIGHT && !dashboard;
    let show_filter = full.width() >= FILTER_MIN_PANE_WIDTH && !dashboard;

    let nav = Rect::from_min_size(full.min, vec2(full.width(), NAV_ROW));
    ui.advance_cursor_after_rect(nav);

    // --- navigation arrows ---
    //
    // Shortcuts are spelled out rather than drawn with arrow characters:
    // U+2190…U+2192 are not in the font egui bundles and render as empty boxes,
    // so "Alt+↑" reached the screen as "Alt+ □".
    // Back, forward and up share a well: one control for moving about.
    let group_rect = Rect::from_min_size(pos2(nav.left() + 4.0, nav.center().y - 18.0), vec2(102.0, 36.0));
    theme::paint_well(ui.painter(), group_rect, egui::CornerRadius::same(14), p);
    let mut x = group_rect.left() + 3.0;
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
        let r = Rect::from_min_size(pos2(x, group_rect.top() + 3.0), vec2(32.0, 30.0));
        if icon_button(ui, p, r, glyph, enabled, false, tip) {
            action = Some(act);
        }
        x += 32.0;
    }
    x = group_rect.right() + 4.0;

    // --- right-hand controls, laid out from the right edge inward ---
    let mut right = nav.right() - 4.0;

    for (vertical, tip) in [
        (true, "Split down (Ctrl+Shift+\\)"),
        (false, "Split right (Ctrl+\\)"),
    ] {
        let r = Rect::from_center_size(pos2(right - 18.0, nav.center().y), vec2(36.0, 36.0));
        if split_button(ui, p, r, vertical, tip) {
            action = Some(HeaderAction::Split(if vertical {
                Axis::Vertical
            } else {
                Axis::Horizontal
            }));
        }
        right -= 38.0;
    }
    right -= 2.0;

    if !dashboard {
        let eye = Rect::from_center_size(pos2(right - 18.0, nav.center().y), vec2(36.0, 36.0));
        if icon_button(
            ui,
            p,
            eye,
            Glyph::Eye,
            true,
            h.show_hidden,
            if cfg!(target_os = "macos") {
                "Show hidden files (Cmd+Shift+.)"
            } else {
                "Show hidden files (Ctrl+H)"
            },
        ) {
            action = Some(HeaderAction::ToggleHidden);
        }
        right -= 44.0;

        // The views, as a segmented control: a pill-shaped well with the chosen
        // view raised out of it. Three states no longer fit one toggle, and a
        // button showing "the next view" makes the user cycle through one they do
        // not want. Laid out from the right, so listed in reverse.
        let segments = Rect::from_min_size(pos2(right - 117.0, nav.center().y - 18.0), vec2(117.0, 36.0));
        theme::paint_well(ui.painter(), segments, theme::pill(36.0), p);
        let mut seg_x = segments.right() - 3.0 - 36.0;
        for (mode, glyph, tip) in [
            (ViewMode::Columns, Glyph::Columns, if cfg!(target_os = "macos") { "Column view (⌘3)" } else { "Column view (Ctrl+Shift+L cycles views)" }),
            (ViewMode::Grid, Glyph::Grid, if cfg!(target_os = "macos") { "Grid view (⌘1)" } else { "Grid view (Ctrl+Shift+L cycles views)" }),
            (ViewMode::List, Glyph::List, if cfg!(target_os = "macos") { "List view (⌘2)" } else { "List view (Ctrl+Shift+L cycles views)" }),
        ] {
            let r = Rect::from_min_size(pos2(seg_x, segments.top() + 3.0), vec2(36.0, 30.0));
            if segment(ui, p, r, glyph, h.view == mode, tip) && h.view != mode {
                action = Some(HeaderAction::SetView(mode));
            }
            seg_x -= 36.0;
        }
        right = segments.left() - 10.0;
    }

    if show_filter {
        // Gives up width in a narrow pane before the breadcrumb does: the
        // trail says where you are, and a filter is still usable at half size.
        let width = FILTER_WIDTH.min(((right - x) * 0.4).max(FILTER_MIN));
        let field = Rect::from_min_max(
            pos2(right - width, nav.center().y - 18.0),
            pos2(right, nav.center().y + 18.0),
        );
        if let Some(text) = filter_field(ui, p, group, field, h.filter) {
            action = Some(HeaderAction::SetFilter(text));
        }
        right -= width + 10.0;
    }

    // --- breadcrumb, in whatever is left between the arrows and the controls ---
    let trail = Rect::from_min_max(pos2(x + 6.0, nav.top()), pos2(right, nav.bottom()));
    if trail.width() > 40.0 {
        match h.address {
            Some(text) => {
                if let Some(a) = address_field(ui, p, group, trail, text) {
                    action = Some(a);
                }
            }
            None => {
                if let Some(target) = breadcrumb(ui, p, trail, h.location) {
                    action = Some(HeaderAction::Navigate(target));
                }
            }
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

    let galley = painter.layout_no_wrap(title, theme::strong(ui.ctx(), 22.0), p.text);
    let baseline = pos2(row.left() + 12.0, row.center().y - galley.size().y / 2.0);
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
        pos2(baseline.x + width + 12.0, row.center().y + 3.0),
        egui::Align2::LEFT_CENTER,
        count,
        egui::FontId::proportional(13.0),
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

    let corners = egui::CornerRadius::same(11);
    if active {
        ui.painter().rect_filled(rect, corners, p.selection);
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(rect, corners, p.hover);
    }

    // Disabled controls are dimmed rather than hidden: a back arrow that
    // vanishes at the start of history makes the row's contents jump.
    let colour = if !enabled {
        theme::with_alpha(p.text_faint, 140)
    } else if active {
        p.accent
    } else if response.hovered() {
        p.text
    } else {
        p.text_soft()
    };
    icons::draw(ui.painter(), rect.center(), glyph, colour);

    if enabled {
        response.clone().on_hover_text(tip);
    }
    enabled && response.clicked()
}

/// One segment of the view switcher: raised out of the well when chosen.
fn segment(ui: &mut Ui, p: &Palette, rect: Rect, glyph: Glyph, chosen: bool, tip: &str) -> bool {
    let response = ui.interact(rect, ui.id().with(("segment", glyph as u8)), Sense::click());
    if chosen {
        theme::paint_raised(ui.painter(), rect, theme::pill(rect.height()), p);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, theme::pill(rect.height()), p.hover);
    }
    let colour = if chosen {
        p.accent
    } else if response.hovered() {
        p.text
    } else {
        p.text_muted
    };
    icons::draw(ui.painter(), rect.center(), glyph, colour);
    response.clone().on_hover_text(tip);
    response.clicked()
}

fn split_button(ui: &mut Ui, p: &Palette, rect: Rect, vertical: bool, tip: &str) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with(("split", vertical, rect.left() as i32, rect.top() as i32)),
        Sense::click(),
    );
    if response.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(11), p.hover);
    }
    let colour = if response.hovered() {
        p.text
    } else {
        p.text_soft()
    };
    icons::split(ui.painter(), rect.center(), vertical, colour);
    response.clone().on_hover_text(neutron_ui::format::shortcut(tip));
    response.clicked()
}

/// The id of a pane's address field.
///
/// Absolute for the same reason the filter's is: `Ctrl+L` focuses it from the
/// app's key handler, which runs before any pane `Ui` exists.
pub fn address_id(group: neutron_core::GroupId) -> egui::Id {
    egui::Id::new(("neutron-address", group))
}

/// A single-line path field where the breadcrumb usually is.
///
/// Full width of the strip, monospaced-ish and unadorned. A path is read for
/// its structure and pasted in one piece, so it gets the whole trail rather
/// than sharing it with the crumbs it replaces.
fn address_field(
    ui: &mut Ui,
    p: &Palette,
    group: neutron_core::GroupId,
    rect: Rect,
    current: &str,
) -> Option<HeaderAction> {
    let id = address_id(group);
    let mut text = current.to_owned();

    ui.painter().rect(
        rect,
        RADIUS_CONTROL as f32,
        p.inset,
        egui::Stroke::new(1.0, p.accent),
        egui::StrokeKind::Inside,
    );

    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top()),
        pos2(rect.right() - 8.0, rect.bottom()),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    child.set_clip_rect(text_rect);

    let response = child.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .background_color(egui::Color32::TRANSPARENT)
            .desired_width(text_rect.width())
            .font(egui::FontId::proportional(12.5))
            .text_color(p.text)
            .vertical_align(egui::Align::Center),
    );

    // Escape closes it. egui does not surrender focus on Escape by itself, so
    // without this the field would keep the keyboard and every shortcut in the
    // application would stay switched off.
    if response.has_focus() && child.input(|i| i.key_pressed(egui::Key::Escape)) {
        child.memory_mut(|m| m.surrender_focus(id));
        return Some(HeaderAction::CancelAddress);
    }

    // Enter before the plain lost-focus case: a submit that also reported a
    // text change would navigate and then re-open the field with the same text.
    if response.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter)) {
        return Some(HeaderAction::SubmitAddress);
    }
    // Clicking away is a cancel. Leaving the field open but unfocused would put
    // a path bar over the breadcrumb of a pane nobody is typing in.
    if response.lost_focus() {
        return Some(HeaderAction::CancelAddress);
    }
    if response.changed() {
        return Some(HeaderAction::SetAddress(text));
    }
    None
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
    let has_focus = ui.memory(|m| m.has_focus(id));

    // A pill-shaped well; focused, a soft ring of the accent around it.
    let corners = theme::pill(rect.height());
    if has_focus {
        ui.painter().rect_filled(rect.expand(4.0), theme::pill(rect.height() + 8.0), theme::with_alpha(p.accent, 20));
    }
    theme::paint_well(ui.painter(), rect, corners, p);
    if has_focus {
        ui.painter().rect_stroke(
            rect,
            corners,
            egui::Stroke::new(1.0, theme::with_alpha(p.accent, 56)),
            egui::StrokeKind::Inside,
        );
    }
    icons::draw(
        ui.painter(),
        pos2(rect.left() + 18.0, rect.center().y),
        Glyph::Search,
        if has_focus { p.accent } else { p.text_faint },
    );

    // A line's height, centred: the field otherwise sits its text at the top
    // of whatever height it is given.
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 34.0, rect.center().y - 10.0),
        pos2(rect.right() - 14.0, rect.center().y + 10.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    child.set_clip_rect(text_rect);

    // No frame of its own: the pill behind it is the frame.
    let output = egui::TextEdit::singleline(&mut text)
        .id(id)
        .background_color(egui::Color32::TRANSPARENT)
        .desired_width(text_rect.width())
        .font(egui::FontId::proportional(13.0))
        .text_color(p.text)
        // Names the control, so an empty field is not an unexplained blank pill.
        .hint_text(egui::RichText::new("Filter this folder").color(p.text_faint).size(13.0))
        .vertical_align(egui::Align::Center)
        .show(&mut child);

    // Escape leaves the field and clears it. Handled here rather than in the
    // app's key table because that table is skipped entirely while a text field
    // holds focus — which is exactly when this binding is wanted.
    //
    // `lost_focus` as well as `has_focus`: egui takes focus away on Escape at
    // the start of the frame, before this field runs, so on the Escape frame
    // the field no longer has focus — it has just lost it. Checking focus
    // alone meant Escape only ever left the field and kept the text.
    let response = &output.response;
    if (response.has_focus() || response.lost_focus())
        && child.input(|i| i.key_pressed(egui::Key::Escape))
    {
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
/// The clickable trail for a location, root-first, or `None` for a location
/// that has no ancestors this side of a COM call.
///
/// Each segment carries the whole [`NodeId`] rather than a path, because a
/// remote trail's segments are only meaningful with their site attached — half
/// a remote crumb is a POSIX path on nothing in particular.
fn segments(location: &NodeId) -> Option<Vec<(String, NodeId)>> {
    match location {
        NodeId::Path(path) => {
            use std::path::Component;

            // Inside the home folder the trail starts there, as the column
            // view's columns do: "/ › Users ›" in front of every path is the
            // same two crumbs on almost every folder anyone opens.
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                if let Ok(rest) = path.strip_prefix(&home) {
                    if home.parent().is_some() {
                        let mut acc = home.clone();
                        let label = home
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "~".to_owned());
                        let mut segments = vec![(label, NodeId::Path(acc.clone()))];
                        for component in rest.components() {
                            acc.push(component.as_os_str());
                            segments.push((
                                component.as_os_str().to_string_lossy().into_owned(),
                                NodeId::Path(acc.clone()),
                            ));
                        }
                        return Some(segments);
                    }
                }
            }

            let mut segments = Vec::new();
            let mut acc = PathBuf::new();
            // A Windows root is two components — `C:` and then `\` — and is one
            // crumb. Held until the separator arrives, so the crumb leads to
            // `C:\` and not to `C:`, which means "the current directory on
            // drive C" and made every path built from it relative.
            let mut drive: Option<String> = None;
            for component in path.components() {
                acc.push(component.as_os_str());
                let label = match component {
                    Component::Prefix(prefix) => {
                        drive = Some(prefix.as_os_str().to_string_lossy().into_owned());
                        continue;
                    }
                    // `/` on its own reads as a stray mark, but it is the only
                    // name the root has, and without a crumb for it the top of
                    // the disk could not be reached from the trail at all.
                    Component::RootDir => drive.take().unwrap_or_else(|| "/".to_owned()),
                    other => other.as_os_str().to_string_lossy().into_owned(),
                };
                segments.push((label, NodeId::Path(acc.clone())));
            }
            // A bare `C:` with no separator after it still gets its crumb.
            if let Some(drive) = drive {
                segments.push((drive, NodeId::Path(acc.clone())));
            }
            (!segments.is_empty()).then_some(segments)
        }

        // The site first, then the POSIX path under it — so a remote pane says
        // *which* server it is on, which "docs" alone does not.
        NodeId::Remote { site, path } => {
            let mut segments = vec![(site.to_string(), NodeId::remote(site.clone(), "/"))];
            let mut acc = String::new();
            for part in path.split('/').filter(|p| !p.is_empty()) {
                acc.push('/');
                acc.push_str(part);
                segments.push((part.to_owned(), NodeId::remote(site.clone(), acc.as_str())));
            }
            Some(segments)
        }

        _ => None,
    }
}

fn breadcrumb(ui: &mut Ui, p: &Palette, rect: Rect, location: &NodeId) -> Option<NodeId> {
    let Some(segments) = segments(location) else {
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

    let font = egui::FontId::proportional(13.0);
    let strong = theme::strong(ui.ctx(), 13.0);
    let chevron = 18.0;
    // The "…" and chevron drawn in place of what was dropped.
    let lead = 12.0 + chevron;
    let last = segments.len() - 1;

    // Measure from the right, keeping segments while they fit. The last is
    // measured in the weight it is drawn in.
    let widths: Vec<f32> = segments
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let font = if i == last { strong.clone() } else { font.clone() };
            ui.painter().layout_no_wrap(label.clone(), font, p.text).size().x
        })
        .collect();

    let mut first = 0;
    let mut used: f32 = widths.iter().sum::<f32>() + chevron * last as f32;
    // The last segment always stays, however long it is — better to clip one
    // name than to render a breadcrumb of nothing but ellipses.
    while used > rect.width() && first < last {
        used -= widths[first] + chevron;
        if first == 0 {
            used += lead;
        }
        first += 1;
    }
    let elided = first > 0;

    // Nothing paints past the trail's end, whatever a long name measures.
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));

    let mut action = None;
    let mut x = rect.left();

    if elided {
        painter.text(
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "…",
            font.clone(),
            p.text_faint,
        );
        x += 12.0;
        icons::draw(
            &painter,
            pos2(x + chevron / 2.0, rect.center().y),
            Glyph::ChevronRight,
            p.text_faint,
        );
        x += chevron;
    }

    for (i, (label, target)) in segments.iter().enumerate().skip(first) {
        // Separator before every segment but the first drawn. When the trail is
        // elided the leading chevron has already been drawn after the ellipsis.
        if i > first {
            icons::draw(
                &painter,
                pos2(x + chevron / 2.0, rect.center().y),
                Glyph::ChevronRight,
                theme::with_alpha(p.text_faint, 170),
            );
            x += chevron;
        }

        let w = widths[i];
        let hit = Rect::from_min_max(
            pos2(x - 7.0, rect.center().y - 15.0),
            pos2(x + w + 7.0, rect.center().y + 15.0),
        );

        if i == last {
            // The current location is a label, not a link: clicking it would
            // reload the pane, which is what F5 is for.
            // Ellipsised rather than cut off when even it alone does not fit.
            let mut job = egui::text::LayoutJob::simple_singleline(label.clone(), strong.clone(), p.text);
            job.wrap = egui::text::TextWrapping {
                max_width: (rect.right() - x).max(1.0),
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = painter.layout_job(job);
            painter.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, p.text);
        } else {
            let response = ui.interact(
                hit,
                ui.id().with(("crumb", i, x as i32)),
                Sense::click(),
            );
            if response.hovered() {
                painter
                    .rect_filled(hit, egui::CornerRadius::same(10), p.hover);
            }
            painter.text(
                pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                font.clone(),
                if response.hovered() { p.text } else { p.text_muted },
            );
            if response.clicked() {
                action = Some(target.clone());
            }
        }
        x += w;
    }

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_trail_starts_with_the_site() {
        // "docs" alone does not say which server it is on, and a remote pane
        // looks exactly like a local one otherwise.
        let trail = segments(&NodeId::remote("nas", "/volume1/docs")).expect("a trail");
        let labels: Vec<&str> = trail.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["nas", "volume1", "docs"]);
    }

    #[test]
    fn every_remote_crumb_keeps_its_site() {
        // A crumb that lost it would navigate to a POSIX path on nothing in
        // particular.
        let trail = segments(&NodeId::remote("nas", "/a/b")).expect("a trail");
        for (_, target) in &trail {
            assert_eq!(target.as_remote().map(|(site, _)| site), Some("nas"));
        }
        assert_eq!(trail[0].1, NodeId::remote("nas", "/"));
        assert_eq!(trail[1].1, NodeId::remote("nas", "/a"));
        assert_eq!(trail[2].1, NodeId::remote("nas", "/a/b"));
    }

    #[test]
    fn a_site_root_is_a_trail_of_one() {
        let trail = segments(&NodeId::remote("nas", "/")).expect("a trail");
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].0, "nas");
    }

    #[test]
    fn a_shell_location_has_no_trail_to_walk() {
        // Its parent chain needs COM, which cannot happen while painting.
        assert!(segments(&NodeId::shell("::{GUID}", "This PC")).is_none());
    }

    #[test]
    fn escape_in_the_filter_clears_it() {
        let ctx = egui::Context::default();
        let group = neutron_core::GroupId(7);
        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(200.0, 28.0));
        let run = |events: Vec<egui::Event>| {
            let mut result = None;
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                result = filter_field(ui, &Palette::DARK, group, rect, "doc");
            });
            output.textures_delta.clear();
            result
        };

        run(Vec::new());
        ctx.memory_mut(|m| m.request_focus(filter_id(group)));
        run(Vec::new());
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(run(vec![escape]), Some(String::new()));
    }

    #[cfg(unix)]
    #[test]
    fn the_root_of_a_unix_path_has_a_crumb_of_its_own() {
        let trail = segments(&NodeId::path("/Users/me")).expect("a trail");
        let labels: Vec<&str> = trail.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["/", "Users", "me"]);
        assert_eq!(trail[0].1, NodeId::path("/"));
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_crumb_leads_to_the_root_of_the_drive() {
        let trail = segments(&NodeId::path(r"C:\Users")).expect("a trail");
        let labels: Vec<&str> = trail.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["C:", "Users"]);
        assert_eq!(trail[0].1, NodeId::path(r"C:\"));
    }

    #[test]
    fn a_short_pane_spends_its_height_on_rows() {
        // Four-way vertical splits leave very little height, and a 40pt
        // heading there would cost most of the visible listing.
        const { assert!(TITLE_MIN_PANE_HEIGHT > NAV_ROW + TITLE_ROW * 2.0) };
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
