//! Previews of what is inside a file, drawn where its tile would be.
//!
//! The pixels come from the app, which knows how to ask the system for them;
//! this module decides which files are worth a preview and how one is laid
//! into the space a view gives it.

use egui::{Color32, Painter, Rect, Stroke, Vec2, pos2, vec2};
use neutron_core::EntryKind;

use crate::icons::FileKind;

/// A preview ready to draw: its texture, and its size in pixels, which is what
/// gives its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    pub texture: egui::TextureId,
    pub size: Vec2,
}

/// Whether a file is worth previewing rather than tiling.
///
/// Pictures, films, and documents whose first page is something to look at.
/// Text and code are not: a preview of a page of tiny text says less than the
/// tile tinted for its kind, and costs a request to say it.
pub fn wants_preview(name: &str, kind: EntryKind) -> bool {
    match FileKind::of(name, kind) {
        FileKind::Image | FileKind::Video => true,
        FileKind::Document => name.rsplit_once('.').is_some_and(|(_, ext)| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "pdf" | "doc" | "docx" | "rtf" | "pages" | "odt" | "epub" | "key" | "numbers"
                    | "xls" | "xlsx" | "ppt" | "pptx"
            )
        }),
        _ => false,
    }
}

/// Where a preview of `size` goes in `space`, and which part of it is shown.
///
/// `cover` fills the space and crops what overhangs, for a square slot in a
/// row; otherwise the whole preview is shown, as large as fits.
pub fn fit(space: Rect, size: Vec2, cover: bool) -> (Rect, Rect) {
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if size.x <= 0.0 || size.y <= 0.0 || space.width() <= 0.0 || space.height() <= 0.0 {
        return (space, full);
    }
    let aspect = size.x / size.y;
    let room = space.width() / space.height();
    if cover {
        // Crop the long side to the slot's shape, about the centre.
        let uv = if aspect > room {
            let w = room / aspect;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
        } else {
            let h = aspect / room;
            Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0))
        };
        (space, uv)
    } else {
        // Never larger than the preview itself: an upscaled thumbnail is soft
        // in a way the eye catches at once.
        let scale = (space.width() / size.x).min(space.height() / size.y).min(1.0);
        (Rect::from_center_size(space.center(), size * scale), full)
    }
}

/// Paints `thumb` into `space` with rounded corners and a faint ring, dimmed
/// for a hidden file as its tile would be.
pub fn paint(painter: &Painter, space: Rect, thumb: Thumb, radius: f32, cover: bool, dim: bool) -> Rect {
    let (rect, uv) = fit(space, thumb.size, cover);
    let corners = egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
    let tint = if dim { Color32::from_white_alpha(120) } else { Color32::WHITE };
    painter.add(
        egui::epaint::RectShape::filled(rect, corners, tint).with_texture(thumb.texture, uv),
    );
    painter.rect_stroke(
        rect,
        corners,
        Stroke::new(1.0, Color32::from_white_alpha(18)),
        egui::StrokeKind::Inside,
    );
    rect
}

/// The size a preview of `size` takes in `space`, for laying out what follows.
pub fn fitted_size(space: Vec2, size: Vec2) -> Vec2 {
    fit(Rect::from_min_size(pos2(0.0, 0.0), space), size, false).0.size().max(vec2(0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_films_and_page_documents_are_previewed_and_text_is_not() {
        assert!(wants_preview("Photo.HEIC", EntryKind::File));
        assert!(wants_preview("clip.mov", EntryKind::File));
        assert!(wants_preview("paper.pdf", EntryKind::File));
        assert!(!wants_preview("notes.md", EntryKind::File));
        assert!(!wants_preview("main.rs", EntryKind::File));
        assert!(!wants_preview("photos", EntryKind::Directory));
    }

    #[test]
    fn a_contained_preview_keeps_its_shape_and_never_grows() {
        let space = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
        let (wide, _) = fit(space, vec2(400.0, 100.0), false);
        assert_eq!(wide.size(), vec2(100.0, 25.0));
        let (small, _) = fit(space, vec2(20.0, 10.0), false);
        assert_eq!(small.size(), vec2(20.0, 10.0));
    }

    #[test]
    fn a_covering_preview_crops_its_long_side_about_the_centre() {
        let square = Rect::from_min_size(pos2(0.0, 0.0), vec2(28.0, 28.0));
        let (rect, uv) = fit(square, vec2(200.0, 100.0), true);
        assert_eq!(rect, square);
        assert!((uv.width() - 0.5).abs() < 1e-6 && (uv.center().x - 0.5).abs() < 1e-6);
        assert_eq!(uv.height(), 1.0);
    }
}
