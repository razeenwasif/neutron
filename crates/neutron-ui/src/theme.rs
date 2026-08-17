//! Neutron's design system.
//!
//! # The model: cards on a tinted ground
//!
//! Panes are **cards** with rounded corners, a soft shadow, and real margin
//! from the window edge. The **ground** shows between and around them, carrying
//! a faint gradient and three coloured lights. Depth comes from elevation — a
//! card sits above the ground — rather than from heavy borders.
//!
//! Cards are very slightly translucent (~88%), which is what stops them reading
//! as flat white cutouts: the lit ground tints each one, and differently in
//! different parts of the window. It is a cast, not glass — at this alpha the
//! surface under text is still effectively determined.
//!
//! # Colour is scarce on purpose — except on the ground
//!
//! Every surface that carries content is near-neutral: a charcoal that leans
//! very slightly violet, or an off-white that does. The accent appears in
//! roughly three places — the selected row, the focus ring, the active tab's
//! underline — and nowhere else. File icons are grey, not purple.
//!
//! The **ground** is the exception, and it is exempt for a specific reason: it
//! is almost entirely covered by cards, and no text ever lands on it. It
//! carries a pale lavender tint and three faint coloured lights ([`Orb`]) whose
//! overlap gives the window a prismatic cast. Colour there costs nothing in
//! legibility, which is precisely why it is the one place it can be spent
//! freely.
//!
//! This is a deliberate correction. An earlier version used saturated purple on
//! near-black with animated glow, which read as neon rather than as a tool.
//! Saturated colour spread across every row also destroys its usefulness as a
//! signal: if everything is purple, purple stops meaning "this is selected".
//!
//! Two rules keep it honest, both enforced by tests at the bottom of this file:
//! text must clear WCAG AA on every surface it can land on, and the accent must
//! not be used for more than a handful of roles.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// Light is the default. Both reference designs Neutron is modelled on are
/// light — off-white ground, white cards, near-black text — and a file manager
/// spends its day next to Explorer and a browser, which are light too. Dark
/// ships alongside and is a keystroke away.
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

/// A soft coloured light on the ground, behind the cards.
///
/// Three of them at slightly different hues give the ground a prismatic cast —
/// the colour shifts across the window instead of sitting flat — without any of
/// it landing on content, because the cards over them are all but opaque.
///
/// Position and radius are fractions of the window, so the arrangement holds at
/// any size instead of clustering in one corner on a wide monitor.
///
/// **Static.** These do not move. An earlier version drifted them on a timer,
/// which forced a repaint every 33ms purely for decoration and held the app at
/// ~9% of a core while nobody was touching it. A background effect has no
/// business costing frames in a file manager.
#[derive(Debug, Clone, Copy)]
pub struct Orb {
    /// Centre, as a fraction of window width and height.
    pub x: f32,
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
    /// Very slightly translucent, so the coloured ground beneath tints it and
    /// the tint varies across the window. It is deliberately *near* opaque:
    /// text sits on this surface, so the backdrop it composites over must never
    /// be in real doubt. See [`Palette::card_over`].
    pub card: Color32,
    /// Card fill under the pointer.
    pub card_hover: Color32,
    /// Menus and popovers, one step above a card.
    pub elevated: Color32,
    /// A recessed well on a card: the filter field, a capacity bar's track.
    pub inset: Color32,

    /// Shadow colour under a card. Dark themes need a denser one than light
    /// themes, because the same alpha over a charcoal ground is invisible.
    pub shadow: Color32,

    /// Coloured lights behind the cards. See [`Orb`].
    pub orbs: [Orb; 3],

    /// Hairline around a card. Neutral, very low contrast.
    pub border: Color32,
    /// Border for a focused or active surface.
    pub border_strong: Color32,

    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,

    /// File and folder glyphs. Grey on purpose — colour here would drown out
    /// the accent everywhere it actually matters.
    pub icon: Color32,

    /// The one accent. Used for selection, focus, and the active tab.
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
        // Charcoal, not near-black. True black plus a saturated accent is the
        // combination that reads as neon.
        ground: Color32::from_rgb(0x18, 0x17, 0x1D),
        ground_deep: Color32::from_rgb(0x13, 0x12, 0x18),

        // Lifted deliberately: at #1F1E26 the card was within 1.08:1 of the
        // ground, which is not enough separation for it to read as floating.
        card: Color32::from_rgba_unmultiplied_const(0x2A, 0x27, 0x35, 232),
        card_hover: Color32::from_rgb(0x34, 0x30, 0x3F),
        elevated: Color32::from_rgb(0x3A, 0x36, 0x48),
        // Darker than the card, not lighter: a well is a hole in the surface.
        inset: Color32::from_rgb(0x1E, 0x1C, 0x26),

        shadow: Color32::from_black_alpha(64),

        // Fainter than the light theme's. On a charcoal ground a coloured
        // light reads at roughly half the alpha before it starts to glow.
        orbs: [
            Orb {
                x: 0.16,
                y: 0.12,
                radius: 0.62,
                colour: Color32::from_rgba_unmultiplied_const(0x8B, 0x6D, 0xF0, 64),
            },
            Orb {
                x: 0.86,
                y: 0.30,
                radius: 0.55,
                colour: Color32::from_rgba_unmultiplied_const(0x5A, 0x7B, 0xD8, 52),
            },
            Orb {
                x: 0.62,
                y: 0.94,
                radius: 0.50,
                colour: Color32::from_rgba_unmultiplied_const(0xC0, 0x6C, 0xC8, 44),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 22),
        border_strong: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 48),

        text: Color32::from_rgb(0xE9, 0xE8, 0xEE),
        text_muted: Color32::from_rgb(0x9D, 0x9B, 0xA8),
        text_faint: Color32::from_rgb(0x70, 0x6E, 0x7B),

        icon: Color32::from_rgb(0x8B, 0x89, 0x98),

        accent: Color32::from_rgb(0xA7, 0x8B, 0xFA),
        accent_hover: Color32::from_rgb(0xB9, 0xA3, 0xFC),
        accent_pressed: Color32::from_rgb(0x8B, 0x6D, 0xF0),

        selection: Color32::from_rgba_unmultiplied_const(0xA7, 0x8B, 0xFA, 56),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 16),

        success: Color32::from_rgb(0x6B, 0xC7, 0x9B),
        warning: Color32::from_rgb(0xDF, 0xAE, 0x5E),
        danger: Color32::from_rgb(0xE5, 0x75, 0x7F),
        cloud: Color32::from_rgb(0x8F, 0xAE, 0xE0),
    };

    pub const LIGHT: Palette = Palette {
        // Pale lavender, not grey. The ground is the one surface with room for
        // colour: no text ever lands on it, so tinting it costs nothing in
        // legibility.
        //
        // It stays low-saturation despite reading as clearly purple — at this
        // lightness a 7% saturation is plenty, and going further turns the
        // shadow under each card muddy.
        ground: Color32::from_rgb(0xED, 0xE7, 0xFA),
        ground_deep: Color32::from_rgb(0xE2, 0xDA, 0xF6),

        // Lavender-white and slightly translucent, not white. A stark white
        // card on a coloured ground looks like a dialog pasted over a page;
        // letting the ground show faintly through makes card and ground read as
        // one surface at two heights, and means the cast varies across the
        // window as the orbs behind it do.
        card: Color32::from_rgba_unmultiplied_const(0xFA, 0xF8, 0xFF, 224),
        card_hover: Color32::from_rgb(0xF1, 0xEC, 0xFC),
        // Popovers stay opaque: they cover content, and letting a listing show
        // through a menu is unreadable rather than atmospheric.
        elevated: Color32::from_rgb(0xFC, 0xFA, 0xFF),
        inset: Color32::from_rgb(0xED, 0xE8, 0xFB),

        // Light shadows are large, soft, and almost not there. Anything denser
        // reads as a drop-shadow effect rather than as a sheet of paper.
        shadow: Color32::from_black_alpha(20),

        // Three hues rather than three tints of one: violet, cyan-blue and
        // magenta shifting across the window is what makes it prismatic. The
        // alphas are low enough that no single orb is identifiable as a circle
        // — what shows is the gradient where they overlap.
        orbs: [
            Orb {
                x: 0.14,
                y: 0.10,
                radius: 0.62,
                colour: Color32::from_rgba_unmultiplied_const(0x9B, 0x7C, 0xF0, 112),
            },
            Orb {
                x: 0.88,
                y: 0.26,
                radius: 0.58,
                colour: Color32::from_rgba_unmultiplied_const(0x6C, 0xA8, 0xEA, 92),
            },
            Orb {
                x: 0.58,
                y: 0.96,
                radius: 0.54,
                colour: Color32::from_rgba_unmultiplied_const(0xE0, 0x8A, 0xD8, 84),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0x14, 0x12, 0x1E, 22),
        border_strong: Color32::from_rgba_unmultiplied_const(0x14, 0x12, 0x1E, 46),

        text: Color32::from_rgb(0x17, 0x16, 0x1C),
        text_muted: Color32::from_rgb(0x63, 0x61, 0x6E),
        text_faint: Color32::from_rgb(0x9A, 0x98, 0xA4),

        icon: Color32::from_rgb(0x7C, 0x7A, 0x88),

        accent: Color32::from_rgb(0x7C, 0x5C, 0xD6),
        accent_hover: Color32::from_rgb(0x6B, 0x4A, 0xC7),
        accent_pressed: Color32::from_rgb(0x5C, 0x3B, 0xB5),

        selection: Color32::from_rgba_unmultiplied_const(0x7C, 0x5C, 0xD6, 44),
        hover: Color32::from_rgba_unmultiplied_const(0x14, 0x12, 0x1E, 14),

        success: Color32::from_rgb(0x18, 0x7A, 0x55),
        warning: Color32::from_rgb(0x92, 0x66, 0x1A),
        danger: Color32::from_rgb(0xC0, 0x3A, 0x4C),
        cloud: Color32::from_rgb(0x36, 0x64, 0xB0),
    };

    pub const fn for_mode(mode: ThemeMode) -> Palette {
        match mode {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        }
    }

    /// The colour a card actually presents when drawn over `backdrop`.
    ///
    /// Cards are translucent, so "the card colour" is not what reaches the eye
    /// — contrast has to be measured against this, not against [`Palette::card`].
    /// The backdrop varies a little across the window as the orbs tint the
    /// ground, so the honest worst case is [`Palette::ground_deep`].
    pub fn card_over(&self, backdrop: Color32) -> Color32 {
        composite(self.card, backdrop)
    }

    /// The darkest a card can get, over the deepest part of the ground.
    pub fn card_worst_case(&self) -> Color32 {
        self.card_over(self.ground_deep)
    }
}

/// Alpha-composites `fg` over an opaque `bg`.
///
/// `Color32` stores premultiplied alpha, so the source term is already scaled
/// and only the destination is attenuated.
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

/// Row height. Comfortable rather than dense: 26pt rows with hairline gridlines
/// were most of what made the first design read as a 2005 file manager. The
/// reference designs are airier still, hence 36 rather than 32.
pub const ROW_HEIGHT: f32 = 36.0;

/// Height of a sidebar entry. Shorter than a file row — the sidebar is a menu,
/// not a data table, and matching the two makes the sidebar look padded out.
pub const NAV_HEIGHT: f32 = 32.0;

/// Margin between the window edge and the cards, and between adjacent cards.
pub const GUTTER: f32 = 16.0;

/// Corner radii. Generous: both reference designs round their cards hard enough
/// that the curve is a visible part of the shape rather than a softened corner.
pub const RADIUS_CARD: u8 = 16;
pub const RADIUS_CONTROL: u8 = 10;
pub const RADIUS_SMALL: u8 = 8;

/// The soft, wide shadow a card casts on the ground.
pub fn card_shadow(p: &Palette) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 4],
        // Wide and faint rather than tight and dark. A large blur radius at low
        // alpha is what makes a surface read as lifted instead of outlined.
        blur: 24,
        spread: 0,
        color: p.shadow,
    }
}

/// A card: opaque fill, hairline, rounded corners, soft shadow.
pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .shadow(card_shadow(p))
}

/// A popover: one step above a card, with a deeper shadow to separate it from
/// whatever it covers.
pub fn popover(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(RADIUS_CONTROL))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 36,
            spread: 0,
            color: Color32::from_black_alpha(56),
        })
}

/// A tiny letter-spaced uppercase label: sidebar section headings and the
/// file list's column names.
///
/// egui has no letter-spacing, so the tracking is faked by joining the
/// characters with hair spaces. Cheap, and at 10pt over a handful of words it
/// is indistinguishable from real tracking.
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
///
/// Registers visuals for *both* themes and then selects one, so toggling is a
/// preference switch rather than a rebuild — and so egui never falls back to
/// its stock palette for the inactive theme.
pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_visuals_of(egui::Theme::Dark, build_visuals(ThemeMode::Dark));
    ctx.set_visuals_of(egui::Theme::Light, build_visuals(ThemeMode::Light));

    ctx.set_theme(match mode {
        ThemeMode::Dark => egui::ThemePreference::Dark,
        ThemeMode::Light => egui::ThemePreference::Light,
    });

    ctx.all_styles_mut(|style| {
        // Roomier than the defaults throughout. Cramped spacing was as much a
        // part of the dated feel as the colours were.
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

    // The app paints its own ground, and panels are drawn as cards.
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

    // Resting state has no fill and no outline: a toolbar full of outlined
    // buttons is exactly the old-fashioned look being removed here. Chrome
    // appears on interaction only.
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

    // Pressed state uses a neutral wash rather than a saturated accent fill:
    // the accent is reserved for selection and focus.
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

    /// Saturation in HSV terms, 0.0..=1.0.
    fn saturation(c: Color32) -> f32 {
        let (r, g, b) = (c.r() as f32, c.g() as f32, c.b() as f32);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max == 0.0 { 0.0 } else { (max - min) / max }
    }

    /// Every surface text can land on, as the colour that actually reaches the
    /// eye. Cards are translucent, so they are composited over both extremes of
    /// the ground rather than measured as declared.
    fn surfaces(p: &Palette) -> Vec<(&'static str, Color32)> {
        vec![
            ("ground", p.ground),
            ("card over ground", p.card_over(p.ground)),
            ("card over deep ground", p.card_worst_case()),
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
    fn icons_are_visible_without_being_loud() {
        for (theme, p) in themes() {
            let c = contrast(p.icon, p.card_worst_case());
            assert!(c >= 3.0, "{theme}: icons on card are only {c:.2}:1");
            // The point of grey icons is that they do not compete with the
            // accent. If someone re-saturates them, this fails.
            assert!(
                saturation(p.icon) < 0.25,
                "{theme}: icons must stay near-neutral, saturation is {:.2}",
                saturation(p.icon)
            );
        }
    }

    #[test]
    fn structural_colours_stay_near_neutral() {
        // Guards the whole direction of the redesign: surfaces that carry
        // content must read as charcoal or off-white, not as a colour.
        //
        // The ground is included even though it is deliberately tinted — at the
        // lightness these sit at, a tint that reads as clearly lavender still
        // scores well under the bound, so the test keeps its teeth against a
        // genuinely saturated ground while permitting the cast.
        for (theme, p) in themes() {
            for (name, c) in [
                ("ground", p.ground),
                ("card", p.card_worst_case()),
                ("card_hover", p.card_hover),
                ("elevated", p.elevated),
                ("inset", p.inset),
                ("text", p.text),
                ("text_muted", p.text_muted),
            ] {
                // 0.30 rather than something tighter: HSV saturation inflates
                // at very low values, so a near-black with a barely-there
                // violet cast scores higher than it looks. The bound still
                // separates surfaces from the accent by a wide margin — the
                // previous design's ground scored 0.33 and its accent 0.72.
                assert!(
                    saturation(c) < 0.30,
                    "{theme}: {name} is too saturated ({:.2}) for a neutral surface",
                    saturation(c)
                );
            }
        }
    }

    #[test]
    fn text_stays_readable_on_a_selected_row() {
        for (theme, p) in themes() {
            let row = composite(p.selection, p.card_worst_case());
            let c = contrast(p.text, row);
            assert!(c >= 4.5, "{theme}: text on a selected row is only {c:.2}:1");
        }
    }

    #[test]
    fn the_accent_is_distinguishable_from_a_plain_row() {
        // Selection has to be obvious at a glance without being loud. Too low
        // and it disappears; the upper bound catches a "fix" that just cranks
        // the alpha back up to a saturated block of colour.
        for (theme, p) in themes() {
            let card = p.card_worst_case();
            let selected = composite(p.selection, card);
            let ratio = contrast(selected, card);
            assert!(
                ratio >= 1.15,
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
        // Nothing is behind it. A translucent ground would composite against
        // whatever the GPU surface happens to hold, which is undefined.
        for (theme, p) in themes() {
            assert_eq!(p.ground.a(), 255, "{theme}: ground must be opaque");
            assert_eq!(p.ground_deep.a(), 255, "{theme}: deep ground must be opaque");
        }
    }

    #[test]
    fn cards_are_translucent_but_only_just() {
        // The tint from the ground is the point — it is what stops the cards
        // reading as flat white cutouts. But text sits on this surface, so the
        // backdrop must never be in real doubt: at 85% the composite moves by a
        // few units, which shifts the hue without moving the contrast.
        for (theme, p) in themes() {
            assert!(
                p.card.a() < 255,
                "{theme}: an opaque card cannot pick up the ground's cast"
            );
            assert!(
                p.card.a() >= 216,
                "{theme}: card alpha {} lets too much through to be a reading surface",
                p.card.a()
            );
        }
    }

    #[test]
    fn a_card_is_distinguishable_from_the_ground() {
        // Cards only read as floating if they differ from what is behind them —
        // measured after compositing, since a translucent card is partly made
        // of the ground it has to be told apart from.
        for (theme, p) in themes() {
            let ratio = contrast(p.card_over(p.ground), p.ground);
            assert!(
                ratio >= 1.08,
                "{theme}: card is indistinguishable from ground ({ratio:.2}:1)"
            );
        }
    }

    #[test]
    fn the_ground_shows_through_enough_to_tint_the_card() {
        // Guards the effect itself rather than the safety margin: if a future
        // tweak raises the alpha until a card over a lit part of the ground
        // matches a card over a dark part, the prismatic cast is gone and only
        // the risk of translucency remains.
        //
        // The range compared is the real one on screen — the brightest orb
        // against the deepest ground — not ground versus ground_deep, which in
        // the dark theme are close enough that the comparison proved nothing.
        for (theme, p) in themes() {
            let brightest = p
                .orbs
                .iter()
                .max_by_key(|o| o.colour.a())
                .expect("a palette has orbs");
            let lit = p.card_over(composite(brightest.colour, p.ground));
            let deep = p.card_worst_case();

            let delta = (lit.r() as i32 - deep.r() as i32).abs()
                + (lit.g() as i32 - deep.g() as i32).abs()
                + (lit.b() as i32 - deep.b() as i32).abs();
            assert!(
                delta >= 6,
                "{theme}: card looks the same over a lit orb as over bare ground (delta {delta})"
            );
        }
    }

    #[test]
    fn light_is_the_default_theme() {
        // Both reference designs are light. If someone flips this back, the
        // app stops matching them on the very first frame.
        assert_eq!(ThemeMode::default(), ThemeMode::Light);
    }

    #[test]
    fn micro_caps_uppercases_and_tracks() {
        assert_eq!(micro_caps("Name"), "N\u{2009}A\u{2009}M\u{2009}E");
        // No trailing separator, or right-aligned labels sit off their edge.
        assert!(!micro_caps("Size").ends_with('\u{2009}'));
        assert_eq!(micro_caps(""), "");
    }

    #[test]
    fn a_card_shadow_is_wide_and_faint() {
        // The whole point of the shape: a tight, dark shadow is the dated
        // "drop shadow" look, a wide faint one reads as elevation.
        for (theme, p) in themes() {
            let s = card_shadow(&p);
            assert!(s.blur >= 16, "{theme}: shadow blur {} is too tight", s.blur);
            assert!(
                p.shadow.a() <= 96,
                "{theme}: shadow alpha {} is too heavy",
                p.shadow.a()
            );
        }
    }

    #[test]
    fn compositing_a_transparent_colour_is_a_no_op() {
        let bg = Color32::from_rgb(10, 20, 30);
        assert_eq!(composite(Color32::TRANSPARENT, bg), bg);
    }
}
