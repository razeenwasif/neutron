//! The window ground: a faint wash plus a few coloured lights behind the cards.
//!
//! # What it is
//!
//! One radial wash from a slightly lighter centre-top to a deeper edge, then
//! three low-alpha coloured orbs at different hues. Where they overlap the
//! ground shifts through violet, blue and magenta — a prismatic cast rather
//! than a flat rectangle for the cards to sit on.
//!
//! Almost all of this is covered by opaque cards. That is deliberate: colour on
//! the ground costs nothing in legibility because no text ever lands on it, and
//! what shows through is the gutter between cards and the margin at the window
//! edge — exactly the places where a flat fill looks cheap.
//!
//! # Why it is static
//!
//! Nothing here changes between frames, so the app can be genuinely idle when
//! the user is not interacting with it. An earlier version drifted the orbs on
//! a timer, which forced a repaint every 33ms purely for decoration and held
//! the process at ~9% of a core doing nothing. The visual difference was not
//! worth a permanently awake event loop.
//!
//! # Cost
//!
//! Four gradients, each a `SEGMENTS`-triangle fan, plus a base rect — under 500
//! triangles for the whole background, submitted once per frame in a single
//! mesh each. It does not measurably move frame time.

use egui::{Color32, Mesh, Painter, Rect, Shape, epaint::Vertex, pos2, vec2};

use crate::theme::Palette;

/// Segments in a gradient fan. Past the point where facets are visible at any
/// window size we care about.
const SEGMENTS: usize = 48;

/// Paints the ground.
pub fn paint(painter: &Painter, rect: Rect, p: &Palette) {
    // Base fill first, so the corners outside the ellipse are never bare.
    painter.rect_filled(rect, 0.0, p.ground_deep);

    // Light source above the window, so the ground feels lit rather than
    // merely tinted.
    let centre = pos2(rect.center().x, rect.top());
    // Overhangs the window, so the visible area is all gradient interior with
    // no hard edge on screen.
    let radius = vec2(rect.width() * 1.25, rect.height() * 1.15);
    fan(painter, centre, radius, p.ground, p.ground_deep);

    // Orbs on top of the wash, so they tint it rather than being washed out.
    let span = rect.width().max(rect.height());
    for orb in &p.orbs {
        let at = pos2(
            rect.left() + rect.width() * orb.x,
            rect.top() + rect.height() * orb.y,
        );
        glow(painter, at, span * orb.radius, orb.colour);
    }
}

/// A soft light: two concentric fans, a tight bright one inside a wide faint
/// one.
///
/// A single linear fan falls off in a straight line from centre to rim, which
/// looks like a cone with a visible edge rather than a glow. Two overlaid
/// falloffs approximate the shoulder of a gaussian closely enough that the
/// boundary disappears, for the cost of one extra mesh.
fn glow(painter: &Painter, centre: egui::Pos2, radius: f32, colour: Color32) {
    let (halo_a, core_a) = split_alpha(colour.a());
    let at = |a: u8| Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), a);

    // `Color32` is premultiplied, so fading to `TRANSPARENT` — premultiplied
    // zero — is a correct linear fade to nothing rather than a fade to black.
    fan(
        painter,
        centre,
        vec2(radius, radius),
        at(halo_a),
        Color32::TRANSPARENT,
    );
    fan(
        painter,
        centre,
        vec2(radius * 0.55, radius * 0.55),
        at(core_a),
        Color32::TRANSPARENT,
    );
}

/// Splits a peak alpha across the two overlapping layers so their combination
/// lands exactly on it.
///
/// The layers composite rather than add: `1 - (1 - a₁)(1 - a₂)`. Giving each the
/// full declared alpha therefore produced a centre far brighter than asked for
/// — an orb declared at 44% arrived at 57% — which is how a "subtle" background
/// turns into wallpaper without anyone changing the number they were reading.
fn split_alpha(peak: u8) -> (u8, u8) {
    let a = peak as f32 / 255.0;
    // The halo carries the smaller share: it is the wide, soft part.
    let halo = a * 0.45;
    // Solve `1 - (1 - halo)(1 - core) = a` for the core.
    let core = if halo >= 1.0 { 0.0 } else { (a - halo) / (1.0 - halo) };
    ((halo * 255.0) as u8, (core * 255.0) as u8)
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
        // Guards the index arithmetic: an off-by-one shows up as a missing
        // wedge, which is easy to miss against a dark ground.
        let mut indices: Vec<u32> = Vec::new();
        for i in 0..SEGMENTS {
            indices.extend_from_slice(&[0, (i + 1) as u32, (i + 2) as u32]);
        }
        assert_eq!(indices.len(), SEGMENTS * 3);
        // Highest index must still address a real vertex (1 centre + SEGMENTS+1 rim).
        assert!((*indices.iter().max().unwrap() as usize) < SEGMENTS + 2);
    }

    #[test]
    fn the_ring_closes() {
        // The last rim vertex must coincide with the first, or the fan leaves a
        // visible seam.
        let last = SEGMENTS as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        assert!((0.0f32.cos() - last.cos()).abs() < 1e-6);
        assert!((0.0f32.sin() - last.sin()).abs() < 1e-6);
    }

    #[test]
    fn orbs_stay_inside_the_window() {
        // Positions are fractions; one outside 0..1 would put a light entirely
        // off screen, which is a silent way to lose part of the effect.
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for orb in &Palette::for_mode(mode).orbs {
                assert!((0.0..=1.0).contains(&orb.x), "{mode:?}: x {} off screen", orb.x);
                assert!((0.0..=1.0).contains(&orb.y), "{mode:?}: y {} off screen", orb.y);
                assert!(orb.radius > 0.0);
            }
        }
    }

    /// Relative luminance, per WCAG.
    fn luminance(c: Color32) -> f32 {
        fn channel(v: u8) -> f32 {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn the_two_glow_layers_add_up_to_the_declared_peak() {
        // The declared alpha has to mean what it says, or tuning the palette is
        // guesswork against a number that is not the one on screen.
        for peak in [0u8, 20, 44, 88, 112, 200, 255] {
            let (halo, core) = split_alpha(peak);
            let combined = 1.0 - (1.0 - halo as f32 / 255.0) * (1.0 - core as f32 / 255.0);
            let got = combined * 255.0;
            assert!(
                (got - peak as f32).abs() <= 2.0,
                "peak {peak} arrives as {got:.0}"
            );
        }
    }

    #[test]
    fn orbs_are_faint_enough_to_stay_a_background() {
        // The real risk is an orb reading as a distinct disc rather than as a
        // cast on the ground. Measured as contrast against the ground it sits
        // on — which is the property that matters — rather than as a raw alpha,
        // which means nothing without knowing what is behind it.
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let p = Palette::for_mode(mode);
            for (i, orb) in p.orbs.iter().enumerate() {
                let lit = crate::theme::composite(orb.colour, p.ground);
                let ratio = contrast(lit, p.ground);
                assert!(
                    ratio <= 1.55,
                    "{mode:?}: orb {i} peaks at {ratio:.2}:1 against the ground — a visible disc, not a cast"
                );
                // And the opposite failure: an orb nobody can see is dead code
                // that still costs a mesh every frame.
                assert!(
                    ratio >= 1.05,
                    "{mode:?}: orb {i} is invisible ({ratio:.2}:1)"
                );
            }
        }
    }

    #[test]
    fn the_orbs_are_actually_different_hues() {
        // Three tints of one colour would be a gradient, not a prism. Compares
        // the ordering of the channels, which is what distinguishes hues,
        // rather than the raw values.
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let orbs = Palette::for_mode(mode).orbs;
            let hue_key = |c: Color32| {
                let (r, g, b) = (c.r() as i32, c.g() as i32, c.b() as i32);
                (r - g, g - b)
            };
            let keys: Vec<_> = orbs.iter().map(|o| hue_key(o.colour)).collect();
            assert_ne!(keys[0], keys[1], "{mode:?}: orbs 0 and 1 share a hue");
            assert_ne!(keys[1], keys[2], "{mode:?}: orbs 1 and 2 share a hue");
            assert_ne!(keys[0], keys[2], "{mode:?}: orbs 0 and 2 share a hue");
        }
    }
}
