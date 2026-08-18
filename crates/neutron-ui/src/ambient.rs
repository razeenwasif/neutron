//! The window ground: depth through translucency with slow-drifting colour fields.
//!
//! # The 3-layer visual model
//!
//! 1. A deep near-black purple ground with top radial illumination.
//! 2. Two large slow-drifting colour fields (purple, electric indigo) on 41s
//!    and 57s loops that give the glass something to refract.
//! 3. Translucent panels floating above them.
//!
//! # Why the drifting colour fields matter
//!
//! The orbs are the load-bearing element of the glassmorphism system. A blur
//! with nothing behind it is just a grey box — translucent panels only read as
//! glass because there is coloured light moving underneath for them to refract.
//!
//! # Geometry and animation
//!
//! Two oversized colour fields drift on smooth sinusoidal ease-in-out loops:
//! - **Field A (41s)**: purple `#9333ea`, top-left, drifting `+7vw, +6vh`, scaling `1.0 → 1.10`.
//! - **Field B (57s)**: electric indigo `#6366f1`, bottom-right, drifting `-8vw, -5vh`, scaling `1.08 → 0.98`.
//!
//! Two rather than three, each near window-sized: what should show is the
//! gradient where they overlap, not a recognisable circle. The periods share no
//! common factor, so the pair never settles into a visible repeat.
//!
//! Each field is a three-layer gaussian falloff fan — see [`glow`].

use egui::{Color32, Mesh, Painter, Rect, Shape, epaint::Vertex, pos2, vec2};

use crate::theme::Palette;

/// Segments in a gradient fan.
const SEGMENTS: usize = 48;

/// Paints the animated ground with slow-drifting colour fields.
pub fn paint(painter: &Painter, rect: Rect, p: &Palette, time: f64) {
    // Base fill first, so the corners outside the ellipse are never bare.
    painter.rect_filled(rect, 0.0, p.ground_deep);

    // Light source above the window, so the ground feels lit rather than merely flat.
    let centre = pos2(rect.center().x, rect.top());
    let radius = vec2(rect.width() * 1.25, rect.height() * 1.15);
    fan(painter, centre, radius, p.ground, p.ground_deep);

    // Drifting colour fields on top of the wash.
    let span = rect.width().max(rect.height());
    for (i, orb) in p.orbs.iter().enumerate() {
        let period = if orb.period > 0.0 { orb.period as f64 } else { 34.0 };
        let progress = (time / period).fract() as f32;
        // Smooth sinusoidal ease-in-out: 0 at 0% and 100%, 1.0 at 50%
        let k = (1.0 - (progress * std::f32::consts::TAU).cos()) * 0.5;

        // Opposed drift, so the two fields breathe against each other rather
        // than sliding the whole background one way.
        let (dx, dy, scale) = match i {
            0 => (0.07 * k, 0.06 * k, 1.0 + 0.10 * k),
            1 => (-0.08 * k, -0.05 * k, 1.08 - 0.10 * k),
            _ => (0.0, 0.0, 1.0),
        };

        let at = pos2(
            rect.left() + rect.width() * (orb.x + dx),
            rect.top() + rect.height() * (orb.y + dy),
        );
        glow(painter, at, span * orb.radius * scale, orb.colour);
    }
}

/// Static fallback for painting the ground without time progression.
pub fn paint_static(painter: &Painter, rect: Rect, p: &Palette) {
    paint(painter, rect, p, 0.0);
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
                assert!(orb.period > 0.0);
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
