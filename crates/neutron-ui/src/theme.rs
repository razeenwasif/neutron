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

/// Dark glass is the default, matching Aero's depth-through-translucency philosophy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
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

/// A soft drifting coloured light on the ground behind the glass panels.
///
/// Position and radius are fractions of the window, so the arrangement holds
/// at any size. Drifts slowly on 34s, 44s, and 52s animation cycles.
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
    /// Loop duration in seconds for the drift animation.
    pub period: f32,
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
    pub const DARK: Palette = Palette {
        // Deep near-black violet ground.
        ground: Color32::from_rgb(0x0C, 0x07, 0x14),
        ground_deep: Color32::from_rgb(0x05, 0x02, 0x0A),

        // Darker and more transparent than before: at 180 alpha over a lit orb
        // the big pane came out visibly lighter than the sidebar beside it,
        // because the pane covers the bright part of the ground and the sidebar
        // does not. Taking the fill down and the transparency up lets both read
        // as the same sheet of glass over different amounts of light — which is
        // what glass does, and what a flat fill cannot fake.
        // Lighter than the ground, not darker. While the cards were nearly
        // opaque a near-ground fill read fine, but transparency exposes the
        // problem: a panel whose fill matches what is behind it composites to
        // the same colour and its edge disappears in the unlit corners. Glass
        // over a dark ground catches light — it is the lighter thing.
        card: Color32::from_rgba_unmultiplied_const(0x24, 0x1B, 0x38, 112),
        card_hover: Color32::from_rgba_unmultiplied_const(0x35, 0x28, 0x50, 150),
        // Popovers stay near-opaque. They cover content rather than sitting on
        // the ground, and reading a menu through the listing underneath is
        // atmosphere at the cost of the thing actually being used.
        elevated: Color32::from_rgba_unmultiplied_const(0x14, 0x0D, 0x22, 244),
        inset: Color32::from_rgba_unmultiplied_const(0x04, 0x02, 0x08, 150),

        shadow: Color32::from_black_alpha(110),

        // Two large, soft colour fields rather than three tighter ones. At this
        // radius each one spans most of the window, so what shows is the
        // gradient where they overlap — no orb is identifiable as a circle,
        // which is what "subtle" has to mean for something the panels sit on.
        //
        // Placed on opposite diagonals and drifting on periods that do not
        // divide into each other, so the pair never settles into a visibly
        // repeating pattern.
        orbs: [
            Orb {
                x: 0.12,
                y: 0.06,
                radius: 0.92,
                colour: Color32::from_rgba_unmultiplied_const(0x93, 0x33, 0xEA, 150),
                period: 41.0,
            },
            Orb {
                x: 0.86,
                y: 0.88,
                radius: 0.85,
                colour: Color32::from_rgba_unmultiplied_const(0x63, 0x66, 0xF1, 125),
                period: 57.0,
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xD8, 0xB4, 0xFE, 28),
        border_strong: Color32::from_rgba_unmultiplied_const(0xD8, 0xB4, 0xFE, 50),

        text: Color32::from_rgb(0xFB, 0xF7, 0xFF),
        text_muted: Color32::from_rgb(0xB8, 0xAD, 0xCC),
        text_faint: Color32::from_rgb(0x7C, 0x72, 0x90),

        icon: Color32::from_rgb(0x9D, 0x93, 0xB0),

        accent: Color32::from_rgb(0xC0, 0x84, 0xFC),
        accent_hover: Color32::from_rgb(0xD8, 0xB4, 0xFE),
        accent_pressed: Color32::from_rgb(0xA8, 0x55, 0xF7),

        selection: Color32::from_rgba_unmultiplied_const(0xC0, 0x84, 0xFC, 44),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 18),

        success: Color32::from_rgb(0x34, 0xD3, 0x99),
        warning: Color32::from_rgb(0xFB, 0xBF, 0x24),
        danger: Color32::from_rgb(0xFB, 0x71, 0x85),
        cloud: Color32::from_rgb(0xA7, 0x8B, 0xFA),
    };

    pub const LIGHT: Palette = Palette {
        ground: Color32::from_rgb(0xF4, 0xEE, 0xFC),
        ground_deep: Color32::from_rgb(0xE9, 0xDE, 0xF8),

        card: Color32::from_rgba_unmultiplied_const(0xFC, 0xFA, 0xFF, 158),
        card_hover: Color32::from_rgba_unmultiplied_const(0xF1, 0xEB, 0xFC, 190),
        elevated: Color32::from_rgb(0xFC, 0xFA, 0xFF),
        inset: Color32::from_rgb(0xEB, 0xE4, 0xF8),

        shadow: Color32::from_black_alpha(22),

        orbs: [
            Orb {
                x: 0.05,
                y: -0.05,
                radius: 0.95,
                colour: Color32::from_rgba_unmultiplied_const(0xA8, 0x55, 0xF7, 84),
                period: 41.0,
            },
            Orb {
                x: 0.92,
                y: 0.92,
                radius: 0.88,
                colour: Color32::from_rgba_unmultiplied_const(0x81, 0x8C, 0xF8, 70),
                period: 57.0,
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0x3B, 0x1E, 0x5A, 24),
        border_strong: Color32::from_rgba_unmultiplied_const(0x3B, 0x1E, 0x5A, 48),

        text: Color32::from_rgb(0x16, 0x10, 0x22),
        text_muted: Color32::from_rgb(0x60, 0x55, 0x72),
        text_faint: Color32::from_rgb(0x94, 0x89, 0xA6),

        icon: Color32::from_rgb(0x76, 0x6B, 0x88),

        accent: Color32::from_rgb(0x93, 0x33, 0xEA),
        accent_hover: Color32::from_rgb(0x7E, 0x22, 0xCE),
        accent_pressed: Color32::from_rgb(0x6B, 0x21, 0xA8),

        selection: Color32::from_rgba_unmultiplied_const(0x93, 0x33, 0xEA, 40),
        hover: Color32::from_rgba_unmultiplied_const(0x20, 0x10, 0x30, 14),

        success: Color32::from_rgb(0x05, 0x96, 0x69),
        warning: Color32::from_rgb(0xD9, 0x77, 0x06),
        danger: Color32::from_rgb(0xE1, 0x1D, 0x48),
        cloud: Color32::from_rgb(0x7C, 0x3A, 0xED),
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
pub const RADIUS_CARD: u8 = 16;
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

    #[test]
    fn cards_are_translucent_glass() {
        for (theme, p) in themes() {
            assert!(
                p.card.a() < 255,
                "{theme}: an opaque card cannot pick up the ground's refraction"
            );
            // A low floor, and deliberately only a floor. This used to demand
            // alpha >= 160 as a stand-in for "text is still readable" — but
            // that is now measured directly against both extremes of the lit
            // ground, so the proxy only blocked a legitimate design choice
            // while claiming to protect something it never actually checked.
            // What remains guards against a card that is not a surface at all.
            assert!(
                p.card.a() >= 80,
                "{theme}: card alpha {} is not a surface, it is a tint",
                p.card.a()
            );
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
    fn dark_is_the_default_theme() {
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
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
