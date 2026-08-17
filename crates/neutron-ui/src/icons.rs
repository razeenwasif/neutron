//! Painter-drawn glyphs.
//!
//! # Why these are shapes rather than text
//!
//! egui bundles Ubuntu-Light plus a small emoji subset. Most of the symbols a
//! file manager wants — arrows, chevrons, a magnifier, a disk — are simply not
//! in either, and a missing glyph renders as an empty box. That failure is
//! silent on the developer's machine if the OS happens to have a fallback font
//! and obvious on everyone else's, which is the worst way for it to fail.
//!
//! Shapes always draw, take a palette colour directly, and scale with the
//! window's DPI for free.
//!
//! # Why they are outlines
//!
//! Filled glyphs down the left edge of a list read as a solid stripe of colour
//! and pull the eye away from the filename, which is the content. Outlines at
//! a 1.4pt stroke carry the same information at a fraction of the visual
//! weight. The accent is reserved for selection; nothing here is ever accented.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use neutron_core::entry::EntryKind;

/// Nominal glyph box, in points. Every icon is drawn to fit inside this
/// centred on the position given, so callers can lay them out on a grid
/// without knowing which glyph they are about to draw.
pub const SIZE: f32 = 16.0;

const STROKE_WIDTH: f32 = 1.4;

/// What a sidebar entry depicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Home,
    Folder,
    Cloud,
    Drive,
    /// A WSL distribution — drawn as a terminal.
    Terminal,
    Search,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ChevronRight,
    Eye,
    Sun,
    Moon,
    File,
    Link,
    Diamond,
}

/// Draws `glyph` centred on `centre`.
pub fn draw(painter: &Painter, centre: Pos2, glyph: Glyph, colour: Color32) {
    let stroke = Stroke::new(STROKE_WIDTH, colour);
    match glyph {
        Glyph::Home => home(painter, centre, stroke),
        Glyph::Folder => folder(painter, centre, stroke),
        Glyph::Cloud => cloud(painter, centre, stroke),
        Glyph::Drive => drive(painter, centre, stroke, colour),
        Glyph::Terminal => terminal(painter, centre, stroke),
        Glyph::Search => search(painter, centre, stroke),
        Glyph::ArrowLeft => arrow(painter, centre, stroke, -1.0, false),
        Glyph::ArrowRight => arrow(painter, centre, stroke, 1.0, false),
        Glyph::ArrowUp => arrow(painter, centre, stroke, -1.0, true),
        Glyph::ChevronRight => chevron(painter, centre, stroke),
        Glyph::Eye => eye(painter, centre, stroke, colour),
        Glyph::Sun => sun(painter, centre, stroke),
        Glyph::Moon => moon(painter, centre, colour),
        Glyph::File => file(painter, centre, stroke),
        Glyph::Link => link(painter, centre, stroke, colour),
        Glyph::Diamond => diamond(painter, centre, stroke),
    }
}

/// The glyph for a directory entry.
pub fn for_kind(kind: EntryKind) -> Glyph {
    match kind {
        EntryKind::Directory => Glyph::Folder,
        EntryKind::Drive => Glyph::Drive,
        EntryKind::File => Glyph::File,
        EntryKind::Junction | EntryKind::Symlink => Glyph::Link,
        EntryKind::Virtual => Glyph::Diamond,
    }
}

// --- individual glyphs -----------------------------------------------------

fn folder(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Body with a small tab on its top-left, the way a manila folder reads.
    let body = Rect::from_min_max(pos2(c.x - 7.0, c.y - 4.0), pos2(c.x + 7.0, c.y + 6.0));
    painter.rect_stroke(body, 2.5, stroke, egui::StrokeKind::Inside);
    painter.add(Shape::line(
        vec![
            pos2(c.x - 7.0, c.y - 4.0),
            pos2(c.x - 3.5, c.y - 6.5),
            pos2(c.x - 0.5, c.y - 6.5),
            pos2(c.x + 1.5, c.y - 4.0),
        ],
        stroke,
    ));
}

fn file(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Page with a folded corner.
    let r = Rect::from_min_max(pos2(c.x - 5.0, c.y - 7.0), pos2(c.x + 5.0, c.y + 7.0));
    painter.rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
    painter.line_segment(
        [pos2(r.right() - 4.0, r.top()), pos2(r.right(), r.top() + 4.0)],
        stroke,
    );
}

fn home(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Roof as an open polyline, walls as a rect beneath it.
    painter.add(Shape::line(
        vec![
            pos2(c.x - 7.5, c.y - 0.5),
            pos2(c.x, c.y - 7.0),
            pos2(c.x + 7.5, c.y - 0.5),
        ],
        stroke,
    ));
    painter.add(Shape::line(
        vec![
            pos2(c.x - 5.5, c.y - 1.0),
            pos2(c.x - 5.5, c.y + 6.5),
            pos2(c.x + 5.5, c.y + 6.5),
            pos2(c.x + 5.5, c.y - 1.0),
        ],
        stroke,
    ));
}

fn cloud(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Three overlapping circles clipped by a flat base: drawing the true
    // outline needs arcs, which epaint has no primitive for, so the shape is
    // built from circle strokes plus a baseline over the seams.
    painter.circle_stroke(pos2(c.x - 3.5, c.y + 0.5), 4.0, stroke);
    painter.circle_stroke(pos2(c.x + 3.0, c.y + 1.0), 3.5, stroke);
    painter.circle_stroke(pos2(c.x, c.y - 1.5), 4.5, stroke);
    // Flat bottom, drawn in the surface's own colour would need the background;
    // instead the base line simply completes the silhouette.
    painter.line_segment(
        [pos2(c.x - 3.5, c.y + 4.5), pos2(c.x + 3.0, c.y + 4.5)],
        stroke,
    );
}

fn drive(painter: &Painter, c: Pos2, stroke: Stroke, colour: Color32) {
    // A stack of two platters: the shape people read as "disk" at 16pt.
    let top = Rect::from_min_max(pos2(c.x - 7.0, c.y - 6.0), pos2(c.x + 7.0, c.y - 0.5));
    let bottom = Rect::from_min_max(pos2(c.x - 7.0, c.y + 0.5), pos2(c.x + 7.0, c.y + 6.0));
    painter.rect_stroke(top, 2.0, stroke, egui::StrokeKind::Inside);
    painter.rect_stroke(bottom, 2.0, stroke, egui::StrokeKind::Inside);
    // Activity dot on each, at the right where a drive light would be.
    painter.circle_filled(pos2(c.x + 4.5, c.y - 3.25), 1.0, colour);
    painter.circle_filled(pos2(c.x + 4.5, c.y + 3.25), 1.0, colour);
}

fn terminal(painter: &Painter, c: Pos2, stroke: Stroke) {
    // A shell prompt: the least ambiguous way to say "Linux" without a logo.
    let r = Rect::from_min_max(pos2(c.x - 7.5, c.y - 6.0), pos2(c.x + 7.5, c.y + 6.0));
    painter.rect_stroke(r, 2.5, stroke, egui::StrokeKind::Inside);
    painter.add(Shape::line(
        vec![
            pos2(c.x - 4.0, c.y - 2.5),
            pos2(c.x - 1.0, c.y),
            pos2(c.x - 4.0, c.y + 2.5),
        ],
        stroke,
    ));
    painter.line_segment([pos2(c.x + 0.5, c.y + 3.0), pos2(c.x + 4.5, c.y + 3.0)], stroke);
}

fn search(painter: &Painter, c: Pos2, stroke: Stroke) {
    painter.circle_stroke(pos2(c.x - 1.5, c.y - 1.5), 5.0, stroke);
    painter.line_segment([pos2(c.x + 2.2, c.y + 2.2), pos2(c.x + 6.0, c.y + 6.0)], stroke);
}

/// Shaft plus a chevron head. `dir` is -1 for left/up, 1 for right/down;
/// `vertical` rotates the whole thing a quarter turn.
fn arrow(painter: &Painter, c: Pos2, stroke: Stroke, dir: f32, vertical: bool) {
    // Laid out along a nominal axis where -1 is the tip and +1 is the tail,
    // then mapped, so there is one set of coordinates to get right rather than
    // four. `along` is *subtracted* so that `dir = -1` puts the tip at
    // decreasing coordinates — that is, to the left, or upward.
    let point = |along: f32, across: f32| arrow_point(c, dir, vertical, along, across);

    painter.line_segment([point(-5.5, 0.0), point(5.5, 0.0)], stroke);
    painter.add(Shape::line(
        vec![point(-1.0, -4.0), point(-5.5, 0.0), point(-1.0, 4.0)],
        stroke,
    ));
}

/// Maps a point on the arrow's nominal axis into screen space.
///
/// Split out so the direction can be tested: the first version had the sign
/// inverted, and every arrow in the application pointed backwards — a mistake
/// that is glaring on screen and invisible in the source.
fn arrow_point(c: Pos2, dir: f32, vertical: bool, along: f32, across: f32) -> Pos2 {
    if vertical {
        pos2(c.x + across, c.y - along * dir)
    } else {
        pos2(c.x - along * dir, c.y + across)
    }
}

fn chevron(painter: &Painter, c: Pos2, stroke: Stroke) {
    painter.add(Shape::line(
        vec![
            pos2(c.x - 1.8, c.y - 3.6),
            pos2(c.x + 1.8, c.y),
            pos2(c.x - 1.8, c.y + 3.6),
        ],
        stroke,
    ));
}

fn eye(painter: &Painter, c: Pos2, stroke: Stroke, colour: Color32) {
    // Lens as two arcs approximated by polylines; a stroked circle would read
    // as a target rather than an eye.
    for sign in [-1.0f32, 1.0] {
        painter.add(Shape::line(
            vec![
                pos2(c.x - 7.0, c.y),
                pos2(c.x - 3.0, c.y + 3.6 * sign),
                pos2(c.x + 3.0, c.y + 3.6 * sign),
                pos2(c.x + 7.0, c.y),
            ],
            stroke,
        ));
    }
    painter.circle_filled(c, 1.8, colour);
}

fn sun(painter: &Painter, c: Pos2, stroke: Stroke) {
    painter.circle_stroke(c, 3.6, stroke);
    for i in 0..8 {
        let a = i as f32 / 8.0 * std::f32::consts::TAU;
        let (s, t) = (a.sin(), a.cos());
        painter.line_segment(
            [
                pos2(c.x + t * 5.6, c.y + s * 5.6),
                pos2(c.x + t * 7.4, c.y + s * 7.4),
            ],
            stroke,
        );
    }
}

fn moon(painter: &Painter, c: Pos2, colour: Color32) {
    // Stroked rather than filled. A crescent is concave, and epaint's polygon
    // fill only handles convex shapes — feeding it these points produced a
    // folded-over bowtie on screen rather than a moon. Two arcs joined into one
    // closed outline also matches the rest of the set, which is all outlines.
    const STEPS: usize = 18;
    let arc = |cx: f32, rev: bool| {
        (0..=STEPS).map(move |i| {
            let i = if rev { STEPS - i } else { i };
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / STEPS as f32 * std::f32::consts::PI;
            pos2(cx + a.cos() * 6.5, c.y + a.sin() * 6.5)
        })
    };

    // Outer limb sweeping the right half, then the inner terminator back.
    let points: Vec<Pos2> = arc(c.x, false).chain(arc(c.x + 3.6, true)).collect();
    painter.add(Shape::line(points, Stroke::new(STROKE_WIDTH, colour)));
}

fn link(painter: &Painter, c: Pos2, stroke: Stroke, colour: Color32) {
    painter.add(Shape::line(
        vec![
            pos2(c.x - 6.0, c.y + 5.0),
            pos2(c.x - 6.0, c.y - 2.0),
            pos2(c.x + 3.0, c.y - 2.0),
        ],
        stroke,
    ));
    painter.add(Shape::convex_polygon(
        vec![
            pos2(c.x + 7.0, c.y - 2.0),
            pos2(c.x + 2.0, c.y - 5.5),
            pos2(c.x + 2.0, c.y + 1.5),
        ],
        colour,
        Stroke::NONE,
    ));
}

fn diamond(painter: &Painter, c: Pos2, stroke: Stroke) {
    painter.add(Shape::convex_polygon(
        vec![
            pos2(c.x, c.y - 6.0),
            pos2(c.x + 6.0, c.y),
            pos2(c.x, c.y + 6.0),
            pos2(c.x - 6.0, c.y),
        ],
        Color32::TRANSPARENT,
        stroke,
    ));
}

/// Sort-direction triangle for the active column header. Solid rather than
/// outlined — at 6pt across, an outline is mud.
pub fn sort_arrow(painter: &Painter, centre: Pos2, ascending: bool, colour: Color32) {
    let (w, h) = (3.2, 2.2);
    let dir = if ascending { -1.0 } else { 1.0 };
    painter.add(Shape::convex_polygon(
        vec![
            pos2(centre.x, centre.y + h * dir),
            pos2(centre.x - w, centre.y - h * dir),
            pos2(centre.x + w, centre.y - h * dir),
        ],
        colour,
        Stroke::NONE,
    ));
}

/// A plus, for the new-tab button.
pub fn plus(painter: &Painter, c: Pos2, colour: Color32) {
    let stroke = Stroke::new(STROKE_WIDTH, colour);
    painter.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
    painter.line_segment([pos2(c.x, c.y - 5.0), pos2(c.x, c.y + 5.0)], stroke);
}

/// A cross, for the close-tab button. Smaller than [`plus`] on purpose: it is
/// destructive and should not advertise itself.
pub fn cross(painter: &Painter, c: Pos2, colour: Color32) {
    let stroke = Stroke::new(1.2, colour);
    let s = 3.2;
    painter.line_segment([pos2(c.x - s, c.y - s), pos2(c.x + s, c.y + s)], stroke);
    painter.line_segment([pos2(c.x - s, c.y + s), pos2(c.x + s, c.y - s)], stroke);
}

/// Two rectangles showing the arrangement a split would produce.
pub fn split(painter: &Painter, centre: Pos2, vertical: bool, colour: Color32) {
    let stroke = Stroke::new(1.2, colour);
    let icon = Rect::from_center_size(centre, vec2(13.0, 11.0));
    painter.rect_stroke(icon, 2.0, stroke, egui::StrokeKind::Inside);
    let divider = if vertical {
        [
            pos2(icon.left(), icon.center().y),
            pos2(icon.right(), icon.center().y),
        ]
    } else {
        [
            pos2(icon.center().x, icon.top()),
            pos2(icon.center().x, icon.bottom()),
        ]
    };
    painter.line_segment(divider, stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_kind_maps_to_a_glyph() {
        // A kind added without an icon would otherwise render as nothing at
        // all, which looks like a loading bug rather than a missing case.
        for kind in [
            EntryKind::Directory,
            EntryKind::Drive,
            EntryKind::File,
            EntryKind::Junction,
            EntryKind::Symlink,
            EntryKind::Virtual,
        ] {
            let _ = for_kind(kind);
        }
        assert_eq!(for_kind(EntryKind::Directory), Glyph::Folder);
        assert_eq!(for_kind(EntryKind::Symlink), Glyph::Link);
    }

    #[test]
    fn arrows_point_where_they_say_they_do() {
        let c = pos2(100.0, 100.0);
        // Tip of the head is at `along = -5.5`.
        let tip = |dir, vertical| arrow_point(c, dir, vertical, -5.5, 0.0);

        // Back arrow: tip to the left of centre, tail to the right.
        assert!(tip(-1.0, false).x < c.x, "back arrow points the wrong way");
        assert!(arrow_point(c, -1.0, false, 5.5, 0.0).x > c.x);

        assert!(tip(1.0, false).x > c.x, "forward arrow points the wrong way");

        // Up arrow: tip above centre. Screen y grows downward, so "up" is a
        // smaller y — the axis that is easiest to get backwards.
        assert!(tip(-1.0, true).y < c.y, "up arrow points the wrong way");
        assert!(arrow_point(c, -1.0, true, 5.5, 0.0).y > c.y);
    }

    #[test]
    fn an_arrow_head_sits_on_its_shaft() {
        // The two barbs must straddle the shaft, or the head renders as a
        // flag hanging off one side.
        let c = pos2(0.0, 0.0);
        let upper = arrow_point(c, -1.0, false, -1.0, -4.0);
        let lower = arrow_point(c, -1.0, false, -1.0, 4.0);
        assert!(upper.y < c.y && lower.y > c.y);
        assert_eq!(upper.x, lower.x);
    }

    #[test]
    fn folders_and_drives_are_told_apart() {
        // They were the same glyph before the sidebar needed to distinguish a
        // volume from a directory.
        assert_ne!(for_kind(EntryKind::Drive), for_kind(EntryKind::Directory));
    }
}
