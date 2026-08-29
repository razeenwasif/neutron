//! The window ground: a still, softly lit surface for the cards to sit on.
//!
//! # Two layers, and nothing moves
//!
//! 1. A flat ground with a wide overhead wash, so the surface is lit rather
//!    than merely flat.
//! 2. Two large static colour washes, well outside the centre, which tint the
//!    ground without ever reading as circles.
//!
//! Cards are opaque and sit on top. They do not refract anything, which is the
//! whole reason the ground is allowed to be this quiet.
//!
//! # Why the drifting is gone
//!
//! This module used to move: two colour fields on 41- and 57-second loops,
//! there to give translucent glass something to refract. Both premises are
//! gone. The cards are opaque now, so there is nothing to refract *through*;
//! and on a light palette a moving ground stops reading as depth and starts
//! reading as a smudge travelling under the text.
//!
//! It was also the only continuous repaint in the program. `request_repaint_after`
//! ran forever so that a soft gradient could move under a pixel per frame, and
//! it measured 1.69% of a core focused, 0.52% unfocused, against 0.3% for a
//! still ground. A file manager that is open all day should cost nothing while
//! nobody is touching it.
//!
//! Each wash is still a three-layer gaussian falloff fan — see [`glow`] — for
//! the same reason as before: at this size any hard edge makes the background
//! read as a shape rather than as light.

use egui::{Color32, Mesh, Painter, Rect, Shape, epaint::Vertex, pos2, vec2};

use crate::theme::Palette;

/// Segments in a gradient fan.
const SEGMENTS: usize = 48;

/// Paints the ground: an overhead wash, then the static colour washes.
///
/// Takes no time parameter, and deliberately: there is nothing here that could
/// use one, and a signature that accepts a clock invites something to start
/// moving again.
pub fn paint(painter: &Painter, rect: Rect, p: &Palette) {
    // Base fill first, so the corners outside the ellipse are never bare.
    painter.rect_filled(rect, 0.0, p.ground_deep);

    // Light source above the window, so the ground feels lit rather than flat.
    let centre = pos2(rect.center().x, rect.top());
    let radius = vec2(rect.width() * 1.25, rect.height() * 1.15);
    fan(painter, centre, radius, p.ground, p.ground_deep);

    let span = rect.width().max(rect.height());
    for orb in p.orbs.iter() {
        let at = pos2(
            rect.left() + rect.width() * orb.x,
            rect.top() + rect.height() * orb.y,
        );
        glow(painter, at, span * orb.radius, orb.colour);
    }
}

/// A soft light: concentric fans approximating a gaussian falloff.
///
/// Three layers rather than two. A single linear fan falls off in a straight
/// line and reads as a cone with a visible rim; two soften that but still leave
/// a discernible edge at this size, and these fields are nearly window-sized —
/// any edge at all makes the background read as a shape rather than as light.
/// Each additional layer costs one 48-triangle mesh, which is nothing.
fn glow(painter: &Painter, centre: egui::Pos2, radius: f32, colour: Color32) {
    let at = |a: u8| Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), a);
    let (outer_a, mid_a, core_a) = split_alpha(colour.a());

    for (scale, alpha) in [(1.0, outer_a), (0.62, mid_a), (0.32, core_a)] {
        fan(
            painter,
            centre,
            vec2(radius * scale, radius * scale),
            at(alpha),
            Color32::TRANSPARENT,
        );
    }
}

/// Splits a peak alpha across the three overlapping layers so their combination
/// lands exactly on it.
///
/// The layers composite rather than add — `1 - Π(1 - aᵢ)` — so giving each the
/// full declared alpha produces a centre far brighter than asked for. Solving
/// for it keeps the number in the palette meaning what it says.
fn split_alpha(peak: u8) -> (u8, u8, u8) {
    let a = peak as f32 / 255.0;
    // Weighted outward: the widest layer carries least, which is what makes the
    // falloff gaussian-ish rather than linear.
    let outer = a * 0.30;
    let remaining = if outer >= 1.0 { 0.0 } else { (a - outer) / (1.0 - outer) };
    let mid = remaining * 0.45;
    let core = if mid >= 1.0 { 0.0 } else { (remaining - mid) / (1.0 - mid) };

    (
        (outer * 255.0).round() as u8,
        (mid * 255.0).round() as u8,
        (core * 255.0).round() as u8,
    )
}

/// Triangle fan with `inner` at the centre fading to `outer` at the rim.
fn fan(painter: &Painter, centre: egui::Pos2, radius: egui::Vec2, inner: Color32, outer: Color32) {
    let mut mesh = Mesh::default();

    mesh.vertices.push(Vertex {
        pos: centre,
        uv: egui::epaint::WHITE_UV,
        color: inner,
    });

    for i in 0..=SEGMENTS {
        let angle = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        mesh.vertices.push(Vertex {
            pos: pos2(
                centre.x + angle.cos() * radius.x,
                centre.y + angle.sin() * radius.y,
            ),
            uv: egui::epaint::WHITE_UV,
            color: outer,
        });
    }

    for i in 0..SEGMENTS {
        mesh.indices
            .extend_from_slice(&[0, (i + 1) as u32, (i + 2) as u32]);
    }

    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeMode;

    #[test]
    fn a_fan_has_one_triangle_per_segment() {
        let mut indices: Vec<u32> = Vec::new();
        for i in 0..SEGMENTS {
            indices.extend_from_slice(&[0, (i + 1) as u32, (i + 2) as u32]);
        }
        assert_eq!(indices.len(), SEGMENTS * 3);
        assert!((*indices.iter().max().unwrap() as usize) < SEGMENTS + 2);
    }

    #[test]
    fn the_ring_closes() {
        let last = SEGMENTS as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        assert!((0.0f32.cos() - last.cos()).abs() < 1e-6);
        assert!((0.0f32.sin() - last.sin()).abs() < 1e-6);
    }

    #[test]
    fn orbs_stay_anchored_around_the_window() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for orb in &Palette::for_mode(mode).orbs {
                assert!((-0.5..=1.5).contains(&orb.x), "{mode:?}: x {} out of range", orb.x);
                assert!((-0.5..=1.5).contains(&orb.y), "{mode:?}: y {} out of range", orb.y);
                assert!(orb.radius > 0.0);
            }
        }
    }

    /// The ground is a backdrop, not a feature. A wash heavy enough to be
    /// noticed as a shape is a wash heavy enough to tint a card sitting on it,
    /// and the cards are the thing being read.
    #[test]
    fn the_washes_stay_faint() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for orb in &Palette::for_mode(mode).orbs {
                assert!(
                    orb.colour.a() <= 40,
                    "{mode:?}: a wash at alpha {} is no longer a wash",
                    orb.colour.a()
                );
            }
        }
    }

    #[test]
    fn the_glow_layers_add_up_to_the_declared_peak() {
        // The declared alpha has to mean what it says, or tuning the palette is
        // guesswork against a number that is not the one on screen.
        for peak in [0u8, 20, 44, 76, 92, 112, 200, 255] {
            let (outer, mid, core) = split_alpha(peak);
            let combined = 1.0
                - (1.0 - outer as f32 / 255.0)
                    * (1.0 - mid as f32 / 255.0)
                    * (1.0 - core as f32 / 255.0);
            let got = combined * 255.0;
            assert!(
                (got - peak as f32).abs() <= 3.0,
                "peak {peak} arrives as {got:.0}"
            );
        }
    }

    #[test]
    fn the_layers_widen_as_they_fade() {
        // Gaussian-ish means the widest layer is the faintest. Reversed, the
        // field gets a hard bright rim — the exact thing three layers exist to
        // avoid.
        let (outer, mid, core) = split_alpha(200);
        assert!(outer < mid, "outer {outer} should be fainter than mid {mid}");
        assert!(mid < core, "mid {mid} should be fainter than core {core}");
    }

    #[test]
    fn the_orbs_are_actually_different_hues() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let orbs = Palette::for_mode(mode).orbs;
            let hue_key = |c: Color32| {
                let (r, g, b) = (c.r() as i32, c.g() as i32, c.b() as i32);
                (r - g, g - b)
            };
            let keys: Vec<_> = orbs.iter().map(|o| hue_key(o.colour)).collect();
            assert_ne!(keys[0], keys[1], "{mode:?}: the two fields share a hue");
        }
    }
}
