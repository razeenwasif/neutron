//! Neutron's design system — purple glassmorphism.
//!
//! # The model: depth through translucency in three layers
//!
//! 1. A deep near-black purple ground with top radial illumination.
//! 2. Three slow-drifting colour fields (purple, fuchsia, electric indigo) on
//!    34–52s loops that give the glass something to refract.
//! 3. Translucent panels floating above them with soft shadows, subtle borders,
//!    and an inset top-edge highlight.
//!
//! # The load-bearing colour fields (orbs)
//!
//! The orbs are what make the glass panels read as glass: a blur with nothing
//! behind it is just a grey box. The panels read as translucent glass because
//! there is coloured light moving underneath for them to refract. On Windows 11
//! the acrylic backdrop extends this to sample the desktop behind the window.
//!
//! # Disciplined purple palette
//!
//! One purple→fuchsia/violet ramp does nearly all the work, with amber and rose
//! reserved strictly for warning and error states. Nothing else competes.

use egui::{Color32, CornerRadius, Stroke, Visuals, pos2};

/// Light is the default. Both of the concepts this design came from are light,
/// and a file manager is read far more than it is looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    pub fn toggled(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, ThemeMode::Dark)
    }
}

/// A soft static coloured wash on the ground behind the cards.
///
/// Position and radius are fractions of the window, so the arrangement holds
/// at any size. Static: nothing here moves, which is why the ground costs one
/// frame rather than a repaint forever.
#[derive(Debug, Clone, Copy)]
pub struct Orb {
    /// Base centre x, as a fraction of window width.
    pub x: f32,
    /// Base centre y, as a fraction of window height.
    pub y: f32,
    /// Radius, as a fraction of the window's larger dimension.
    pub radius: f32,
    /// Colour at the centre. Its alpha sets the peak intensity; the edge fades
    /// to fully transparent.
    pub colour: Color32,
}

/// Semantic colour tokens, named by role rather than appearance.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background, visible around and between cards.
    pub ground: Color32,
    /// Outer edge of the ground gradient.
    pub ground_deep: Color32,

    /// Card fill — panes, sidebar, toolbars.
    ///
    /// Translucent glass, so the coloured lights drifting beneath refract
    /// through and tint the surface dynamically.
    pub card: Color32,
    /// Card fill under the pointer.
    pub card_hover: Color32,
    /// Menus and popovers, one step above a card.
    pub elevated: Color32,
    /// A recessed well on a card: the filter field, a capacity bar's track.
    pub inset: Color32,

    /// Shadow colour under a card.
    pub shadow: Color32,

    /// Coloured lights behind the cards. See [`Orb`].
    pub orbs: [Orb; 2],

    /// Hairline around a card. Subtle purple-tinted glass border.
    pub border: Color32,
    /// Border for a focused or active surface.
    pub border_strong: Color32,

    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,

    /// File and folder glyphs. Neutral muted violet-grey.
    pub icon: Color32,

    /// The purple brand accent. Used for selection, focus, and the active tab.
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,

    /// Selected-row fill: the accent at low alpha over a card.
    pub selection: Color32,
    /// Neutral hover wash.
    pub hover: Color32,

    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub cloud: Color32,
}

impl Palette {
    /// The light theme, and the default.
    ///
    /// # Opaque cards on a still ground
    ///
    /// Neutron used to draw translucent glass over two colour fields drifting
    /// on 41- and 57-second loops. That is gone. A light palette is exactly
    /// where a moving ground stops reading as depth and starts reading as a
    /// smudge under the text, and the drift was the only continuous repaint in
    /// the program — 1.69% of a core, forever, for decoration. The ground keeps
    /// two *static* washes, which cost nothing and still stop the window
    /// reading as a flat sheet.
    ///
    /// Cards are opaque, so a card is the same colour wherever it sits. That
    /// is what makes a hairline border enough to separate it from the ground,
    /// and it is why the contrast figures below are single numbers rather than
    /// ranges.
    pub const LIGHT: Palette = Palette {
        ground: Color32::from_rgb(0xEE, 0xF0, 0xF4),
        ground_deep: Color32::from_rgb(0xE6, 0xE9, 0xEF),

        card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_hover: Color32::from_rgb(0xF5, 0xF6, 0xF9),
        elevated: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        inset: Color32::from_rgb(0xF2, 0xF3, 0xF7),

        // Barely there. Depth comes from the hairline and a whisper of shadow,
        // not from a dark edge under every card.
        shadow: Color32::from_black_alpha(18),

        // Two static washes, well outside the window's centre so no circle is
        // identifiable — what shows is the tint where they fall off.
        orbs: [
            Orb {
                x: 0.14,
                y: -0.06,
                radius: 0.95,
                colour: Color32::from_rgba_unmultiplied_const(0xA8, 0x8B, 0xF7, 26),
            },
            Orb {
                x: 0.94,
                y: 0.10,
                radius: 0.88,
                colour: Color32::from_rgba_unmultiplied_const(0x6E, 0xA8, 0xF0, 22),
            },
        ],

        border: Color32::from_rgb(0xE6, 0xE7, 0xEE),
        border_strong: Color32::from_rgb(0xD8, 0xD9, 0xE3),

        text: Color32::from_rgb(0x16, 0x16, 0x1D),
        text_muted: Color32::from_rgb(0x6B, 0x6C, 0x7B),
        // Deliberately darker than the 0x9A9BAB of the design draft, which
        // measured 2.9:1 on white and would have been the one value in the
        // palette below AA. Metadata is still quiet at this weight.
        text_faint: Color32::from_rgb(0x77, 0x78, 0x88),

        icon: Color32::from_rgb(0x6B, 0x6C, 0x7B),

        accent: Color32::from_rgb(0x72, 0x50, 0xE6),
        accent_hover: Color32::from_rgb(0x5F, 0x3D, 0xD6),
        accent_pressed: Color32::from_rgb(0x4E, 0x2F, 0xBC),

        // A tint, not a fill: the row keeps its own text colour and the left
        // edge carries the accent.
        selection: Color32::from_rgb(0xF0, 0xEC, 0xFE),
        hover: Color32::from_rgba_unmultiplied_const(0x16, 0x16, 0x1D, 10),

        success: Color32::from_rgb(0x07, 0x84, 0x5C),
        warning: Color32::from_rgb(0xA8, 0x5C, 0x12),
        danger: Color32::from_rgb(0xC0, 0x35, 0x4C),
        cloud: Color32::from_rgb(0x72, 0x50, 0xE6),
    };

    /// The same design language after dark: opaque cards, a still ground, one
    /// accent. Not a separate look — the light theme with the values inverted
    /// around the same structure, so a hairline still separates a card from the
    /// ground and nothing moves.
    pub const DARK: Palette = Palette {
        ground: Color32::from_rgb(0x14, 0x15, 0x1A),
        ground_deep: Color32::from_rgb(0x0E, 0x0F, 0x13),

        card: Color32::from_rgb(0x1C, 0x1D, 0x24),
        card_hover: Color32::from_rgb(0x24, 0x25, 0x2E),
        elevated: Color32::from_rgb(0x24, 0x25, 0x2E),
        inset: Color32::from_rgb(0x16, 0x17, 0x1D),

        shadow: Color32::from_black_alpha(90),

        orbs: [
            Orb {
                x: 0.14,
                y: -0.06,
                radius: 0.95,
                colour: Color32::from_rgba_unmultiplied_const(0x7C, 0x5C, 0xF0, 30),
            },
            Orb {
                x: 0.94,
                y: 0.10,
                radius: 0.88,
                colour: Color32::from_rgba_unmultiplied_const(0x3A, 0x6E, 0xB8, 24),
            },
        ],

        border: Color32::from_rgb(0x2C, 0x2D, 0x37),
        border_strong: Color32::from_rgb(0x3C, 0x3D, 0x4A),

        text: Color32::from_rgb(0xF2, 0xF2, 0xF6),
        text_muted: Color32::from_rgb(0xA3, 0xA4, 0xB4),
        text_faint: Color32::from_rgb(0x84, 0x85, 0x95),

        icon: Color32::from_rgb(0xA3, 0xA4, 0xB4),

        accent: Color32::from_rgb(0xA4, 0x8C, 0xFF),
        accent_hover: Color32::from_rgb(0xB9, 0xA7, 0xFF),
        accent_pressed: Color32::from_rgb(0x8B, 0x6B, 0xF5),

        selection: Color32::from_rgb(0x2A, 0x25, 0x45),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 12),

        success: Color32::from_rgb(0x3D, 0xD6, 0xA0),
        warning: Color32::from_rgb(0xF0, 0xB4, 0x3C),
        danger: Color32::from_rgb(0xF4, 0x76, 0x8C),
        cloud: Color32::from_rgb(0xA4, 0x8C, 0xFF),
    };

    pub const fn for_mode(mode: ThemeMode) -> Palette {
        match mode {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        }
    }

    /// The colour a card actually presents when drawn over `backdrop`.
    pub fn card_over(&self, backdrop: Color32) -> Color32 {
        composite(self.card, backdrop)
    }

    /// The darkest a card can get: over the deepest, unlit part of the ground.
    ///
    /// The worst case for *dark* text on a light card.
    pub fn card_worst_case(&self) -> Color32 {
        self.card_over(self.ground_deep)
    }

    /// The lightest a card can get: over the brightest colour field at its peak.
    ///
    /// The worst case for *light* text on a dark card, and the one that matters
    /// as the cards get more transparent — a contrast check against the deep
    /// ground alone is the *best* case for light text and would pass however
    /// washed-out the lit areas became.
    pub fn card_lit_case(&self) -> Color32 {
        let brightest = self
            .orbs
            .iter()
            .max_by_key(|o| o.colour.a())
            .map(|o| composite(o.colour, self.ground))
            .unwrap_or(self.ground);
        self.card_over(brightest)
    }
}

/// Alpha-composites `fg` over an opaque `bg`.
pub fn composite(fg: Color32, bg: Color32) -> Color32 {
    let a = fg.a() as f32 / 255.0;
    let blend = |f: u8, b: u8| (f as f32 + b as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(
        blend(fg.r(), bg.r()),
        blend(fg.g(), bg.g()),
        blend(fg.b(), bg.b()),
    )
}

// --- metrics ---------------------------------------------------------------

pub const ROW_HEIGHT: f32 = 36.0;
pub const NAV_HEIGHT: f32 = 32.0;
pub const GUTTER: f32 = 16.0;
pub const RADIUS_CARD: u8 = 14;
pub const RADIUS_CONTROL: u8 = 10;
pub const RADIUS_SMALL: u8 = 8;

/// The soft, wide glass shadow a card casts on the ground.
pub fn card_shadow(p: &Palette) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 8],
        blur: 32,
        spread: 0,
        color: p.shadow,
    }
}

/// A glass card: translucent fill, subtle glass border, rounded corners, soft shadow.
pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .shadow(card_shadow(p))
}

/// Draws an inset top-edge highlight stroke for glass panels and pills.
///
/// Mirrors Aero's `--glass-highlight: inset 0 1px 0 rgba(255, 255, 255, 0.08)`.
/// A lit top edge, for a surface that is meant to look like a lit object.
///
/// Cards no longer use this. They are opaque, and a white highlight on a white
/// card is a draw call that changes nothing. What is left is the brand mark,
/// which is a gradient chip and does catch light.
pub fn glass_highlight(painter: &egui::Painter, rect: egui::Rect, radius: egui::CornerRadius) {
    let r = radius.nw as f32;

    // Top edge: the bright line where a sheet of glass catches the light.
    painter.line_segment(
        [
            pos2(rect.left() + r, rect.top() + 0.5),
            pos2(rect.right() - r, rect.top() + 0.5),
        ],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 34)),
    );

    // Upper side edges, fading out. Real glass catches light down the sides of
    // its top corners too, and stopping the highlight dead at the corner is
    // what makes a "glass" panel read as a rectangle with a white line on it.
    // Drawn as short segments of decreasing alpha rather than a gradient mesh:
    // three primitives against a whole mesh, for something this faint.
    for (i, alpha) in [24u8, 14, 7].into_iter().enumerate() {
        let from = rect.top() + r + (i as f32) * r;
        let to = from + r;
        if to > rect.bottom() {
            break;
        }
        let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
        painter.line_segment(
            [pos2(rect.left() + 0.5, from), pos2(rect.left() + 0.5, to)],
            stroke,
        );
        painter.line_segment(
            [pos2(rect.right() - 0.5, from), pos2(rect.right() - 0.5, to)],
            stroke,
        );
    }

    // Bottom edge, darker. Glass has thickness: the far edge sits in its own
    // shadow, and without it the panel reads as a flat translucent rectangle.
    painter.line_segment(
        [
            pos2(rect.left() + r, rect.bottom() - 0.5),
            pos2(rect.right() - r, rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, Color32::from_black_alpha(46)),
    );
}

/// A popover: one step above a card, with a deeper shadow.
pub fn popover(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(RADIUS_CONTROL))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 16],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(72),
        })
}

/// A tiny letter-spaced uppercase label.
pub fn micro_caps(label: &str) -> String {
    let upper = label.to_uppercase();
    let mut out = String::with_capacity(upper.len() * 2);
    for (i, c) in upper.chars().enumerate() {
        if i > 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// Applies the palette to an egui context.
pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_visuals_of(egui::Theme::Dark, build_visuals(ThemeMode::Dark));
    ctx.set_visuals_of(egui::Theme::Light, build_visuals(ThemeMode::Light));

    ctx.set_theme(match mode {
        ThemeMode::Dark => egui::ThemePreference::Dark,
        ThemeMode::Light => egui::ThemePreference::Light,
    });

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.scroll.bar_width = 9.0;
        style.spacing.scroll.bar_inner_margin = 4.0;
        style.interaction.resize_grab_radius_side = 6.0;
    });
}

fn build_visuals(mode: ThemeMode) -> Visuals {
    let p = Palette::for_mode(mode);
    let mut visuals = match mode {
        ThemeMode::Dark => Visuals::dark(),
        ThemeMode::Light => Visuals::light(),
    };

    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = p.elevated;
    visuals.extreme_bg_color = p.ground;
    visuals.faint_bg_color = Color32::TRANSPARENT;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.selection.bg_fill = p.selection;
    visuals.selection.stroke = Stroke::new(1.0, p.accent);
    visuals.hyperlink_color = p.accent;
    visuals.window_corner_radius = CornerRadius::same(RADIUS_CONTROL);

    let radius = CornerRadius::same(RADIUS_CONTROL);

    visuals.widgets.noninteractive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_muted);
    visuals.widgets.noninteractive.corner_radius = radius;

    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text_muted);
    visuals.widgets.inactive.corner_radius = radius;

    visuals.widgets.hovered.bg_fill = p.hover;
    visuals.widgets.hovered.weak_bg_fill = p.hover;
    visuals.widgets.hovered.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.hovered.corner_radius = radius;

    visuals.widgets.active.bg_fill = p.card_hover;
    visuals.widgets.active.weak_bg_fill = p.card_hover;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, p.border_strong);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.active.corner_radius = radius;

    visuals.widgets.open.bg_fill = p.card_hover;
    visuals.widgets.open.weak_bg_fill = p.card_hover;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.open.corner_radius = radius;

    visuals
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn surfaces(p: &Palette) -> Vec<(&'static str, Color32)> {
        vec![
            ("ground", p.ground),
            ("card over ground", p.card_over(p.ground)),
            ("card over deep ground", p.card_worst_case()),
            ("card over a lit orb", p.card_lit_case()),
            ("card_hover", p.card_hover),
            ("elevated", p.elevated),
            ("inset", p.inset),
        ]
    }

    fn themes() -> [(&'static str, Palette); 2] {
        [("dark", Palette::DARK), ("light", Palette::LIGHT)]
    }

    #[test]
    fn primary_text_meets_wcag_aa_on_every_surface() {
        for (theme, p) in themes() {
            for (name, bg) in surfaces(&p) {
                let c = contrast(p.text, bg);
                assert!(c >= 4.5, "{theme}: text on {name} is only {c:.2}:1");
            }
        }
    }

    #[test]
    fn muted_text_clears_the_large_text_floor() {
        for (theme, p) in themes() {
            for (name, bg) in surfaces(&p) {
                let c = contrast(p.text_muted, bg);
                assert!(c >= 3.0, "{theme}: muted text on {name} is only {c:.2}:1");
            }
        }
    }

    #[test]
    fn text_stays_readable_on_a_selected_row() {
        for (theme, p) in themes() {
            // Both extremes: a selected row over dark ground and over a lit
            // orb are different colours, and text has to survive both.
            for (where_, under) in [
                ("deep ground", p.card_worst_case()),
                ("a lit orb", p.card_lit_case()),
            ] {
                let row = composite(p.selection, under);
                let c = contrast(p.text, row);
                assert!(
                    c >= 4.5,
                    "{theme}: text on a selected row over {where_} is only {c:.2}:1"
                );
            }
        }
    }

    #[test]
    fn the_accent_is_distinguishable_from_a_plain_row() {
        for (theme, p) in themes() {
            let card = p.card_worst_case();
            let selected = composite(p.selection, card);
            let ratio = contrast(selected, card);
            assert!(
                ratio >= 1.12,
                "{theme}: selection is too faint to see ({ratio:.2}:1 vs card)"
            );
            assert!(
                ratio <= 3.0,
                "{theme}: selection is shouting ({ratio:.2}:1 vs card)"
            );
        }
    }

    #[test]
    fn the_ground_is_opaque() {
        for (theme, p) in themes() {
            assert_eq!(p.ground.a(), 255, "{theme}: ground must be opaque");
            assert_eq!(p.ground_deep.a(), 255, "{theme}: deep ground must be opaque");
        }
    }

    /// Opaque, and that is the point. A card that changes colour with what is
    /// behind it needs every contrast figure quoted as a range; an opaque one
    /// has a single value, and a hairline is enough to separate it from the
    /// ground. This replaces a test that asserted the opposite.
    #[test]
    fn cards_are_opaque() {
        for (theme, p) in themes() {
            assert_eq!(
                p.card.a(),
                255,
                "{theme}: a translucent card puts the ground back inside every contrast check"
            );
            assert_eq!(p.elevated.a(), 255, "{theme}: popovers cover content");
            assert_eq!(p.inset.a(), 255, "{theme}: an inset well is a surface");
        }
    }

    #[test]
    fn a_card_still_reads_as_a_panel_against_the_ground() {
        // Transparency has a limit that contrast tests do not catch: a card
        // that composites to nearly the same colour as the ground beside it
        // stops being a panel, and the layout dissolves into floating text.
        for (theme, p) in themes() {
            for (where_, backdrop, card) in [
                ("deep ground", p.ground_deep, p.card_worst_case()),
                ("lit ground", p.ground, p.card_over(p.ground)),
            ] {
                let ratio = contrast(card, backdrop);
                assert!(
                    ratio >= 1.05,
                    "{theme}: card over {where_} is indistinguishable from it ({ratio:.3}:1)"
                );
            }
        }
    }

    #[test]
    fn light_is_the_default_theme() {
        assert_eq!(ThemeMode::default(), ThemeMode::Light);
    }

    #[test]
    fn micro_caps_uppercases_and_tracks() {
        assert_eq!(micro_caps("Name"), "N\u{2009}A\u{2009}M\u{2009}E");
        assert!(!micro_caps("Size").ends_with('\u{2009}'));
        assert_eq!(micro_caps(""), "");
    }

    #[test]
    fn a_card_shadow_is_wide_and_faint() {
        for (theme, p) in themes() {
            let s = card_shadow(&p);
            assert!(s.blur >= 16, "{theme}: shadow blur {} is too tight", s.blur);
        }
    }

    #[test]
    fn compositing_a_transparent_colour_is_a_no_op() {
        let bg = Color32::from_rgb(10, 20, 30);
        assert_eq!(composite(Color32::TRANSPARENT, bg), bg);
    }
}
