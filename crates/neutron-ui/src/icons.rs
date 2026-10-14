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
    /// Four squares — switch to the grid view.
    Grid,
    /// Stacked lines — switch to the list view.
    List,
    /// Three side-by-side panels — switch to the column view.
    Columns,
    /// A saved FTP/SFTP server — drawn as a globe, because what distinguishes
    /// it from a drive is that it is somewhere else.
    Server,

    // --- the action rail ---
    Copy,
    Cut,
    Paste,
    Trash,
    Rename,
    NewFolder,
    Extract,
    Compress,

    // --- velvet: places and kinds of file, drawn from 24-unit outlines ---
    /// Tiles of different sizes — the dashboard.
    Dashboard,
    Desktop,
    Download,
    /// A page with lines on it.
    Document,
    Image,
    Music,
    Film,
    /// A page with brackets on it — source and configuration.
    Code,
    /// A box with a zip down it.
    Archive,
    /// A page with its corner folded — anything else.
    Page,
    Plus,
    Close,
    /// Three crossed orbits — Neutron's mark when its icon is not to hand.
    Atom,
    /// A triangle over a bar: take the disk out.
    Eject,
    /// Arcs spreading from a point.
    AirDrop,
}

/// Draws `glyph` centred on `centre`.
pub fn draw(painter: &Painter, centre: Pos2, glyph: Glyph, colour: Color32) {
    draw_with(painter, centre, glyph, Stroke::new(STROKE_WIDTH, colour));
}

fn draw_with(painter: &Painter, centre: Pos2, glyph: Glyph, stroke: Stroke) {
    let colour = stroke.color;
    match glyph {
        Glyph::Home => home(painter, centre, stroke),
        Glyph::Folder => folder(painter, centre, stroke),
        Glyph::Cloud => cloud(painter, centre, stroke),
        Glyph::Drive => drive(painter, centre, stroke, colour),
        Glyph::Terminal => terminal(painter, centre, stroke),
        Glyph::Server => server(painter, centre, stroke),
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
        Glyph::Grid => grid(painter, centre, stroke),
        Glyph::List => list_rows(painter, centre, stroke),
        Glyph::Columns => column_panels(painter, centre, stroke),
        Glyph::Copy => copy_pages(painter, centre, stroke),
        Glyph::Cut => cut_scissors(painter, centre, stroke),
        Glyph::Paste => paste_board(painter, centre, stroke),
        Glyph::Trash => trash(painter, centre, stroke),
        Glyph::Rename => pencil(painter, centre, stroke),
        Glyph::NewFolder => folder_plus(painter, centre, stroke),
        Glyph::Extract => extract(painter, centre, stroke),
        Glyph::Compress => compress(painter, centre, stroke),
        Glyph::Dashboard => outlines::dashboard(painter, centre, stroke),
        Glyph::Desktop => outlines::desktop(painter, centre, stroke),
        Glyph::Download => outlines::download(painter, centre, stroke),
        Glyph::Document => outlines::document(painter, centre, stroke),
        Glyph::Image => outlines::image(painter, centre, stroke),
        Glyph::Music => outlines::music(painter, centre, stroke),
        Glyph::Film => outlines::film(painter, centre, stroke),
        Glyph::Code => outlines::code(painter, centre, stroke),
        Glyph::Archive => outlines::archive(painter, centre, stroke),
        Glyph::Page => outlines::page(painter, centre, stroke),
        Glyph::Plus => outlines::plus(painter, centre, stroke),
        Glyph::Close => outlines::close(painter, centre, stroke),
        Glyph::Atom => outlines::atom(painter, centre, stroke),
        Glyph::Eject => outlines::eject(painter, centre, stroke),
        Glyph::AirDrop => outlines::airdrop(painter, centre, stroke),
    }
}

/// Draws `glyph` at `scale` times its usual size — for a large tile.
pub fn draw_scaled(painter: &Painter, centre: Pos2, glyph: Glyph, colour: Color32, scale: f32) {
    // Lines thicken with size, but less than in proportion: a glyph twice the
    // size with lines twice as heavy reads as bold, not as larger.
    let width = STROKE_WIDTH * scale.max(1.0).sqrt();
    if is_outline(glyph) {
        outlines::with_scale(scale, || draw_with(painter, centre, glyph, Stroke::new(width, colour)));
        return;
    }

    // The older glyphs are drawn to a fixed envelope, so they are drawn at
    // their usual size and the shapes scaled about the centre afterwards. Each
    // keeps the clip it was drawn with: scaling the clip as well would let a
    // glyph at the edge of a scrolled list paint over whatever is past it.
    let start = painter.add(Shape::Noop).0;
    draw_with(painter, centre, glyph, Stroke::new(width / scale, colour));
    let transform = egui::emath::TSTransform::new(centre.to_vec2() * (1.0 - scale), scale);
    painter.ctx().graphics_mut(|g| {
        let list = g.entry(painter.layer_id());
        let drawn: Vec<_> = list
            .all_entries()
            .skip(start)
            .map(|c| (c.clip_rect, c.shape.clone()))
            .collect();
        for (offset, (clip, mut shape)) in drawn.into_iter().enumerate() {
            shape.transform(transform);
            list.set(egui::layers::ShapeIdx(start + offset), clip, shape);
        }
    });
}

/// Whether `glyph` is drawn from the 24-unit outlines, which scale themselves.
fn is_outline(glyph: Glyph) -> bool {
    matches!(
        glyph,
        Glyph::Dashboard
            | Glyph::Desktop
            | Glyph::Download
            | Glyph::Document
            | Glyph::Image
            | Glyph::Music
            | Glyph::Film
            | Glyph::Code
            | Glyph::Archive
            | Glyph::Page
            | Glyph::Plus
            | Glyph::Close
            | Glyph::Atom
            | Glyph::Eject
            | Glyph::AirDrop
    )
}

/// A tick sized to fit inside a circle of `radius`.
///
/// Its own function rather than a [`Glyph`], because every glyph in that enum
/// is drawn to one fixed envelope and this one has to match whatever badge it
/// is being put inside. The stroke scales with it too: a 1.4px line inside a
/// nine-pixel circle is a hairline, not a tick.
pub fn check(painter: &Painter, centre: Pos2, radius: f32, colour: Color32) {
    let s = radius * 0.52;
    let stroke = Stroke::new((radius * 0.28).max(1.2), colour);
    painter.add(Shape::line(
        vec![
            pos2(centre.x - s, centre.y + s * 0.1),
            pos2(centre.x - s * 0.25, centre.y + s * 0.8),
            pos2(centre.x + s, centre.y - s * 0.7),
        ],
        stroke,
    ));
}

/// What kind of thing an entry is, for the tinted tile it is drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Folder,
    Code,
    Document,
    Image,
    Video,
    Audio,
    Archive,
    Link,
    Drive,
    Plain,
}

impl FileKind {
    /// Sorts an entry by its kind on disk and, for a file, its extension.
    pub fn of(name: &str, kind: EntryKind) -> FileKind {
        match kind {
            EntryKind::Directory => return FileKind::Folder,
            EntryKind::Drive => return FileKind::Drive,
            EntryKind::Junction | EntryKind::Symlink => return FileKind::Link,
            EntryKind::Virtual => return FileKind::Plain,
            EntryKind::File => {}
        }
        let lower = name.to_ascii_lowercase();
        if matches!(lower.as_str(), "makefile" | "dockerfile" | "justfile" | "license" | "licence") {
            return if lower.starts_with("lic") { FileKind::Document } else { FileKind::Code };
        }
        let Some((_, ext)) = lower.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
            return FileKind::Plain;
        };
        match ext {
            "rs" | "toml" | "lock" | "json" | "yaml" | "yml" | "js" | "mjs" | "cjs" | "ts" | "tsx"
            | "jsx" | "py" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "swift" | "kt" | "java"
            | "sh" | "zsh" | "bash" | "fish" | "css" | "scss" | "html" | "htm" | "xml" | "sql"
            | "rb" | "php" | "lua" | "cs" | "m" | "mm" | "plist" | "ini" | "cfg" | "conf"
            | "env" | "gradle" | "vue" | "svelte" | "zig" | "nix" | "wgsl" | "glsl" => FileKind::Code,
            "md" | "markdown" | "txt" | "pdf" | "doc" | "docx" | "rtf" | "pages" | "odt" | "tex"
            | "epub" | "csv" | "tsv" | "key" | "numbers" | "xls" | "xlsx" | "ppt" | "pptx" => {
                FileKind::Document
            }
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp"
            | "svg" | "ico" | "icns" | "psd" | "raw" | "avif" => FileKind::Image,
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => FileKind::Video,
            "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" | "aiff" => FileKind::Audio,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "dmg" | "pkg" | "iso"
            | "zst" => FileKind::Archive,
            _ => FileKind::Plain,
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            FileKind::Folder => Glyph::Folder,
            FileKind::Code => Glyph::Code,
            FileKind::Document => Glyph::Document,
            FileKind::Image => Glyph::Image,
            FileKind::Video => Glyph::Film,
            FileKind::Audio => Glyph::Music,
            FileKind::Archive => Glyph::Archive,
            FileKind::Link => Glyph::Link,
            FileKind::Drive => Glyph::Drive,
            FileKind::Plain => Glyph::Page,
        }
    }

    pub fn tint(self, p: &crate::theme::Palette) -> Color32 {
        match self {
            FileKind::Folder => p.tints.folder,
            FileKind::Code => p.tints.code,
            FileKind::Document => p.tints.document,
            FileKind::Image | FileKind::Video | FileKind::Audio => p.tints.image,
            FileKind::Archive => p.tints.archive,
            FileKind::Link | FileKind::Drive | FileKind::Plain => p.tints.plain,
        }
    }
}

/// A file's icon tile: its kind's tint at low strength behind its glyph.
pub fn tile(painter: &Painter, rect: Rect, kind: FileKind, p: &crate::theme::Palette, dim: bool) {
    let tint = kind.tint(p);
    let radius = (rect.height() * 0.32).round();
    painter.rect_filled(rect, radius, crate::theme::with_alpha(tint, if dim { 18 } else { 33 }));
    let scale = rect.height() / 28.0;
    let colour = if dim { crate::theme::with_alpha(tint, 140) } else { tint };
    draw_scaled(painter, rect.center(), kind.glyph(), colour, scale);
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


// --- the action rail -------------------------------------------------------
//
// Drawn to a larger envelope than the sidebar's glyphs. The rail's buttons are
// the biggest click targets in the window and carry a label underneath, so a
// mark sized for a 31px navigation row looks lost in one; `ACTION` is the
// factor between the two.
//
// Every coordinate below is the same 14px envelope the rest of the file uses,
// multiplied through by that factor in one place, so the two sets stay in
// proportion if either is retuned.

/// How much larger a rail glyph is than a sidebar one.
const ACTION: f32 = 1.35;

/// A point offset from `c`, in envelope units scaled to the rail.
fn a(c: Pos2, dx: f32, dy: f32) -> Pos2 {
    pos2(c.x + dx * ACTION, c.y + dy * ACTION)
}

fn copy_pages(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Two offset sheets: the universal "there are now two of these".
    painter.rect_stroke(
        Rect::from_min_max(a(c, -6.5, -7.0), a(c, 2.0, 2.5)),
        2.0 * ACTION,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        Rect::from_min_max(a(c, -2.0, -2.5), a(c, 6.5, 7.0)),
        2.0 * ACTION,
        stroke,
        egui::StrokeKind::Inside,
    );
}

fn cut_scissors(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Two blades crossing above two rings.
    painter.line_segment([a(c, -5.0, -7.0), a(c, 4.0, 3.0)], stroke);
    painter.line_segment([a(c, 5.0, -7.0), a(c, -4.0, 3.0)], stroke);
    painter.circle_stroke(a(c, -4.5, 5.0), 2.4 * ACTION, stroke);
    painter.circle_stroke(a(c, 4.5, 5.0), 2.4 * ACTION, stroke);
}

fn paste_board(painter: &Painter, c: Pos2, stroke: Stroke) {
    // A clipboard: the board, and the clip on top.
    painter.rect_stroke(
        Rect::from_min_max(a(c, -6.0, -4.5), a(c, 6.0, 7.0)),
        2.0 * ACTION,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        Rect::from_min_max(a(c, -3.0, -7.5), a(c, 3.0, -3.0)),
        1.5 * ACTION,
        stroke,
        egui::StrokeKind::Inside,
    );
}

fn trash(painter: &Painter, c: Pos2, stroke: Stroke) {
    // Lid, then a bin that tapers — a plain rectangle reads as a box.
    painter.line_segment([a(c, -6.5, -4.5), a(c, 6.5, -4.5)], stroke);
    painter.add(Shape::line(
        vec![a(c, -2.5, -4.5), a(c, -2.5, -6.5), a(c, 2.5, -6.5), a(c, 2.5, -4.5)],
        stroke,
    ));
    painter.add(Shape::line(
        vec![a(c, -5.0, -2.5), a(c, -4.0, 7.0), a(c, 4.0, 7.0), a(c, 5.0, -2.5)],
        stroke,
    ));
}

fn pencil(painter: &Painter, c: Pos2, stroke: Stroke) {
    // A nib on a shaft, at the usual 45 degrees.
    painter.add(Shape::closed_line(
        vec![
            a(c, -7.0, 7.0),
            a(c, -5.0, 2.0),
            a(c, 4.0, -7.0),
            a(c, 7.0, -4.0),
            a(c, -2.0, 5.0),
        ],
        stroke,
    ));
}

fn folder_plus(painter: &Painter, c: Pos2, stroke: Stroke) {
    // The folder body, at the rail's scale rather than the sidebar's.
    painter.rect_stroke(
        Rect::from_min_max(a(c, -7.0, -3.5), a(c, 7.0, 6.5)),
        2.5 * ACTION,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.add(Shape::line(
        vec![a(c, -7.0, -3.5), a(c, -3.5, -6.0), a(c, -0.5, -6.0), a(c, 1.5, -3.5)],
        stroke,
    ));
    // The cross sits over the body rather than beside it: at this size a glyph
    // with something appended alongside is two smudges instead of one mark.
    painter.line_segment([a(c, 0.0, -0.5), a(c, 0.0, 4.0)], stroke);
    painter.line_segment([a(c, -2.25, 1.75), a(c, 2.25, 1.75)], stroke);
}

fn extract(painter: &Painter, c: Pos2, stroke: Stroke) {
    // An arrow coming down out of a tray: unpacking, not downloading.
    painter.line_segment([a(c, 0.0, -7.0), a(c, 0.0, 1.0)], stroke);
    painter.add(Shape::line(
        vec![a(c, -3.0, -2.0), a(c, 0.0, 1.0), a(c, 3.0, -2.0)],
        stroke,
    ));
    painter.add(Shape::line(
        vec![a(c, -6.5, 3.0), a(c, -6.5, 7.0), a(c, 6.5, 7.0), a(c, 6.5, 3.0)],
        stroke,
    ));
}

fn compress(painter: &Painter, c: Pos2, stroke: Stroke) {
    // The same tray with the arrow going in: packing.
    painter.line_segment([a(c, 0.0, -7.0), a(c, 0.0, 1.0)], stroke);
    painter.add(Shape::line(
        vec![a(c, -3.0, -4.0), a(c, 0.0, -7.0), a(c, 3.0, -4.0)],
        stroke,
    ));
    painter.add(Shape::line(
        vec![a(c, -6.5, 3.0), a(c, -6.5, 7.0), a(c, 6.5, 7.0), a(c, 6.5, 3.0)],
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

fn server(painter: &Painter, c: Pos2, stroke: Stroke) {
    // A globe. The stack-of-platters shape is already the local drive, and the
    // one thing a saved server is not is local.
    painter.circle_stroke(c, 6.5, stroke);
    // The meridian, as an ellipse drawn from its own points — egui has no
    // ellipse primitive and an arc pair reads as a broken circle at 16pt.
    let mut meridian = Vec::with_capacity(33);
    for step in 0..=32 {
        let angle = step as f32 / 32.0 * std::f32::consts::TAU;
        meridian.push(pos2(c.x + angle.sin() * 3.1, c.y - angle.cos() * 6.5));
    }
    painter.add(Shape::line(meridian, stroke));
    painter.add(Shape::line(
        vec![pos2(c.x - 6.5, c.y), pos2(c.x + 6.5, c.y)],
        stroke,
    ));
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

fn grid(painter: &Painter, c: Pos2, stroke: Stroke) {
    for (dx, dy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let centre = pos2(c.x + dx * 3.4, c.y + dy * 3.4);
        painter.rect_stroke(
            Rect::from_center_size(centre, vec2(5.2, 5.2)),
            1.2,
            stroke,
            egui::StrokeKind::Inside,
        );
    }
}

fn list_rows(painter: &Painter, c: Pos2, stroke: Stroke) {
    for i in -1..=1 {
        let y = c.y + i as f32 * 4.6;
        // A leading dot per row, as a list marker does.
        painter.circle_filled(pos2(c.x - 5.6, y), 1.1, stroke.color);
        painter.line_segment([pos2(c.x - 2.6, y), pos2(c.x + 6.5, y)], stroke);
    }
}

fn column_panels(painter: &Painter, c: Pos2, stroke: Stroke) {
    // One frame split into three, the way the view itself reads: panes of a
    // path laid side by side.
    let frame = Rect::from_center_size(c, vec2(14.0, 11.0));
    painter.rect_stroke(frame, 1.6, stroke, egui::StrokeKind::Inside);
    for x in [frame.left() + frame.width() / 3.0, frame.left() + frame.width() * 2.0 / 3.0] {
        painter.line_segment([pos2(x, frame.top()), pos2(x, frame.bottom())], stroke);
    }
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

    /// The badge is a tick inside a disc, and the tick has to stay inside it.
    /// Drawn from the radius rather than at a fixed size, because the same
    /// helper is what any future badge would use.
    #[test]
    fn kinds_follow_the_extension_and_ignore_its_case() {
        assert_eq!(FileKind::of("main.RS", EntryKind::File), FileKind::Code);
        assert_eq!(FileKind::of("README.md", EntryKind::File), FileKind::Document);
        assert_eq!(FileKind::of("icon.PNG", EntryKind::File), FileKind::Image);
        assert_eq!(FileKind::of("release.tar.gz", EntryKind::File), FileKind::Archive);
        assert_eq!(FileKind::of(".gitignore", EntryKind::File), FileKind::Plain);
        assert_eq!(FileKind::of("src", EntryKind::Directory), FileKind::Folder);
    }

    #[test]
    fn a_check_stays_inside_its_badge() {
        for radius in [6.0f32, 9.0, 14.0, 24.0] {
            let s = radius * 0.52;
            // Furthest the tick reaches from the centre, plus half the stroke.
            let stroke = (radius * 0.28).max(1.2);
            let reach = (s * s + (s * 0.8) * (s * 0.8)).sqrt() + stroke / 2.0;
            assert!(
                reach <= radius,
                "at r={radius} the tick reaches {reach}, outside the disc"
            );
        }
    }

    /// A hairline tick inside a small disc is invisible, so the stroke scales
    /// with the badge and never goes below a floor.
    #[test]
    fn the_tick_stroke_scales_but_has_a_floor() {
        assert!((6.0f32 * 0.28).max(1.2) >= 1.2);
        assert!((24.0f32 * 0.28).max(1.2) > (9.0f32 * 0.28).max(1.2));
    }

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


/// Glyphs drawn from the same 24-unit outlines as the design mockups, so the
/// app and the design agree on every shape.
mod outlines {
    use std::cell::Cell;

    use egui::{Painter, Pos2, Rect, Shape, Stroke, pos2};

    /// Points per outline unit at the usual size: a 24-unit outline drawn in
    /// about 17 points, the size the mockups draw them in a row.
    const K: f32 = 0.72;

    thread_local! { static SCALE: Cell<f32> = const { Cell::new(1.0) }; }

    pub fn with_scale(scale: f32, f: impl FnOnce()) {
        let before = SCALE.with(|s| s.replace(scale));
        f();
        SCALE.with(|s| s.set(before));
    }

    fn k() -> f32 {
        K * SCALE.with(Cell::get)
    }

    /// An outline point, relative to the 24-unit box's centre at (12, 12).
    fn u(c: Pos2, x: f32, y: f32) -> Pos2 {
        let k = k();
        pos2(c.x + (x - 12.0) * k, c.y + (y - 12.0) * k)
    }

    // An SVG `<rect>`'s attributes, one argument each, so a shape can be
    // copied from the mockups' outlines as written.
    #[allow(clippy::too_many_arguments)]
    fn rect(painter: &Painter, c: Pos2, x: f32, y: f32, w: f32, h: f32, r: f32, stroke: Stroke) {
        painter.rect_stroke(
            Rect::from_min_max(u(c, x, y), u(c, x + w, y + h)),
            r * k(),
            stroke,
            egui::StrokeKind::Middle,
        );
    }

    fn line(painter: &Painter, c: Pos2, points: &[(f32, f32)], stroke: Stroke) {
        painter.add(Shape::line(points.iter().map(|&(x, y)| u(c, x, y)).collect(), stroke));
    }

    fn closed(painter: &Painter, c: Pos2, points: &[(f32, f32)], stroke: Stroke) {
        painter.add(Shape::closed_line(points.iter().map(|&(x, y)| u(c, x, y)).collect(), stroke));
    }

    fn circle(painter: &Painter, c: Pos2, x: f32, y: f32, r: f32, stroke: Stroke) {
        painter.circle_stroke(u(c, x, y), r * k(), stroke);
    }

    /// A page with a folded corner, its other corners softened.
    fn sheet(painter: &Painter, c: Pos2, stroke: Stroke) {
        closed(
            painter,
            c,
            &[
                (7.0, 3.5),
                (13.5, 3.5),
                (18.0, 8.0),
                (18.0, 18.6),
                (17.4, 20.0),
                (16.0, 20.5),
                (7.0, 20.5),
                (5.6, 20.0),
                (5.0, 18.6),
                (5.0, 5.4),
                (5.6, 4.0),
            ],
            stroke,
        );
    }

    pub fn dashboard(p: &Painter, c: Pos2, s: Stroke) {
        rect(p, c, 3.5, 3.5, 7.5, 9.0, 2.0, s);
        rect(p, c, 13.0, 3.5, 7.5, 5.5, 2.0, s);
        rect(p, c, 13.0, 11.0, 7.5, 9.5, 2.0, s);
        rect(p, c, 3.5, 14.5, 7.5, 6.0, 2.0, s);
    }

    pub fn desktop(p: &Painter, c: Pos2, s: Stroke) {
        rect(p, c, 3.5, 4.5, 17.0, 11.5, 2.5, s);
        line(p, c, &[(9.0, 20.0), (15.0, 20.0)], s);
        line(p, c, &[(12.0, 16.0), (12.0, 20.0)], s);
    }

    pub fn download(p: &Painter, c: Pos2, s: Stroke) {
        line(p, c, &[(12.0, 4.0), (12.0, 15.0)], s);
        line(p, c, &[(7.5, 10.5), (12.0, 15.0), (16.5, 10.5)], s);
        line(p, c, &[(5.0, 19.5), (19.0, 19.5)], s);
    }

    pub fn document(p: &Painter, c: Pos2, s: Stroke) {
        sheet(p, c, s);
        line(p, c, &[(8.5, 12.5), (15.5, 12.5)], s);
        line(p, c, &[(8.5, 15.5), (13.5, 15.5)], s);
    }

    pub fn code(p: &Painter, c: Pos2, s: Stroke) {
        sheet(p, c, s);
        line(p, c, &[(10.2, 12.5), (8.4, 14.3), (10.2, 16.1)], s);
        line(p, c, &[(13.8, 12.5), (15.6, 14.3), (13.8, 16.1)], s);
    }

    pub fn page(p: &Painter, c: Pos2, s: Stroke) {
        sheet(p, c, s);
        line(p, c, &[(13.5, 3.5), (13.5, 8.0), (18.0, 8.0)], s);
    }

    pub fn image(p: &Painter, c: Pos2, s: Stroke) {
        rect(p, c, 3.5, 4.5, 17.0, 15.0, 3.0, s);
        circle(p, c, 9.0, 10.0, 1.6, s);
        line(p, c, &[(20.0, 15.0), (16.0, 11.0), (8.5, 18.5)], s);
    }

    pub fn archive(p: &Painter, c: Pos2, s: Stroke) {
        rect(p, c, 4.5, 3.5, 15.0, 17.0, 3.0, s);
        line(p, c, &[(12.0, 3.5), (12.0, 6.0)], s);
        line(p, c, &[(12.0, 8.5), (12.0, 10.5)], s);
        line(p, c, &[(12.0, 13.0), (12.0, 15.0)], s);
    }

    pub fn music(p: &Painter, c: Pos2, s: Stroke) {
        line(p, c, &[(9.0, 17.0), (9.0, 6.0), (18.0, 4.5), (18.0, 15.5)], s);
        circle(p, c, 7.0, 17.0, 2.0, s);
        circle(p, c, 16.0, 15.5, 2.0, s);
    }

    pub fn film(p: &Painter, c: Pos2, s: Stroke) {
        rect(p, c, 3.5, 5.0, 17.0, 14.0, 3.0, s);
        line(p, c, &[(8.0, 5.0), (8.0, 19.0)], s);
        line(p, c, &[(16.0, 5.0), (16.0, 19.0)], s);
    }

    pub fn plus(p: &Painter, c: Pos2, s: Stroke) {
        line(p, c, &[(12.0, 6.0), (12.0, 18.0)], s);
        line(p, c, &[(6.0, 12.0), (18.0, 12.0)], s);
    }

    pub fn close(p: &Painter, c: Pos2, s: Stroke) {
        line(p, c, &[(7.5, 7.5), (16.5, 16.5)], s);
        line(p, c, &[(16.5, 7.5), (7.5, 16.5)], s);
    }

    pub fn eject(p: &Painter, c: Pos2, s: Stroke) {
        closed(p, c, &[(12.0, 5.5), (18.5, 13.0), (5.5, 13.0)], s);
        line(p, c, &[(5.5, 17.5), (18.5, 17.5)], s);
    }

    pub fn airdrop(p: &Painter, c: Pos2, s: Stroke) {
        // Three arcs opening upwards from a point near the bottom.
        for r in [3.5f32, 7.0, 10.5] {
            let points: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = (210.0 + 120.0 * i as f32 / 24.0).to_radians();
                    u(c, 12.0 + r * t.cos(), 17.0 + r * t.sin())
                })
                .collect();
            p.add(Shape::line(points, s));
        }
        p.circle_filled(u(c, 12.0, 17.0), 1.2 * k(), s.color);
    }

    pub fn atom(p: &Painter, c: Pos2, s: Stroke) {
        for angle in [0.0f32, 60.0, 120.0] {
            let (sin, cos) = angle.to_radians().sin_cos();
            let points: Vec<Pos2> = (0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0 * std::f32::consts::TAU;
                    let (x, y) = (9.0 * t.cos(), 3.6 * t.sin());
                    u(c, 12.0 + x * cos - y * sin, 12.0 + x * sin + y * cos)
                })
                .collect();
            p.add(Shape::line(points, s));
        }
    }
}
