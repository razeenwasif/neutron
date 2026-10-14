//! Finder tags as the views draw them: a row of small overlapping dots in the
//! tags' colours, after a file's name.

use egui::{Color32, Painter, Pos2, Stroke, pos2};

/// The Finder's tag colours, softened to sit on the dark panes: by the colour
/// number a tag carries, 1 to 7.
pub fn color(number: u8) -> Color32 {
    match number {
        1 => Color32::from_rgb(0xA3, 0x9F, 0xAE), // gray
        2 => Color32::from_rgb(0x7F, 0xD1, 0x9E), // green
        3 => Color32::from_rgb(0xC0, 0x9C, 0xF0), // purple
        4 => Color32::from_rgb(0x82, 0xB4, 0xF2), // blue
        5 => Color32::from_rgb(0xEE, 0xD1, 0x7C), // yellow
        6 => Color32::from_rgb(0xF0, 0x8C, 0x8C), // red
        7 => Color32::from_rgb(0xF2, 0xB1, 0x7C), // orange
        _ => Color32::TRANSPARENT,
    }
}

/// The order the Finder lists its colours in, which is the order dots go.
const ORDER: [u8; 7] = [6, 7, 5, 2, 4, 3, 1];
/// At most this many dots; more read as noise, and the Finder stops at three.
const MOST: usize = 3;
const RADIUS: f32 = 4.5;
const STEP: f32 = 6.0;

/// The colours in `bits` (bit `n - 1` for colour `n`), in the Finder's order.
fn colours(bits: u8) -> impl Iterator<Item = u8> {
    ORDER.into_iter().filter(move |n| bits & (1 << (n - 1)) != 0).take(MOST)
}

/// How wide the dots for `bits` are; 0 for none.
pub fn width(bits: u8) -> f32 {
    match colours(bits).count() {
        0 => 0.0,
        n => RADIUS * 2.0 + STEP * (n - 1) as f32,
    }
}

/// Paints the dots for `bits` starting at `left` (the left edge, vertically
/// centred), each ringed in `ring` — the surface behind — so overlapping dots
/// stay distinct.
pub fn paint(painter: &Painter, left: Pos2, bits: u8, ring: Color32) {
    for (i, number) in colours(bits).enumerate() {
        let centre = pos2(left.x + RADIUS + STEP * i as f32, left.y);
        painter.circle(centre, RADIUS, color(number), Stroke::new(1.5, ring));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_follow_the_finders_order_and_stop_at_three() {
        let all = 0x7F;
        assert_eq!(colours(all).collect::<Vec<_>>(), vec![6, 7, 5]);
        assert_eq!(colours(1 << 3).collect::<Vec<_>>(), vec![4], "blue alone");
        assert_eq!(width(0), 0.0);
        assert_eq!(width(all), RADIUS * 2.0 + STEP * 2.0);
    }
}
