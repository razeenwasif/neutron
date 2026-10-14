//! Neutron's design system.
//!
//! # Velvet, after dark
//!
//! The dark theme is the primary one: matte, low-contrast surfaces that catch
//! a little light along their top edge, one lavender accent kept for what is
//! chosen, and every shape rounded. Separation comes from shifts in tone — a
//! pane a few levels lighter than the desktop showing through around it, a
//! well a few levels darker than the pane it is pressed into — rather than
//! from lines. [`paint_pane`] draws the surface; the tokens on [`Palette`]
//! name the rest.
//!
//! The notes below describe the earlier glass design, of which the light
//! theme is what remains.
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

/// Dark is the default: the velvet design is a dark one, and the light theme
/// is kept for whoever prefers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    Light,
    #[default]
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
    /// The top of a pane's vertical sheen; [`Palette::card`] is the bottom.
    /// The same as `card` where a theme has no sheen.
    pub card_top: Color32,
    /// Card fill under the pointer.
    pub card_hover: Color32,
    /// A control lifted off the surface: the chosen segment of a segmented
    /// control, a rail button under the pointer, the active tab.
    pub raised: Color32,
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

    /// One colour per kind of file, for the tile behind its glyph.
    pub tints: Tints,
}

/// Colours that tell kinds of file apart at a glance.
///
/// Muted and of a like lightness, so no kind shouts over the others; they
/// differ in hue for scanning, and the glyph inside each tile differs in shape
/// for anyone who cannot tell the hues apart.
#[derive(Debug, Clone, Copy)]
pub struct Tints {
    pub folder: Color32,
    pub code: Color32,
    pub document: Color32,
    pub image: Color32,
    pub archive: Color32,
    pub plain: Color32,
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
        card_top: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_hover: Color32::from_rgb(0xF5, 0xF6, 0xF9),
        raised: Color32::from_rgb(0xFF, 0xFF, 0xFF),
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

        tints: Tints {
            folder: Color32::from_rgb(0x72, 0x50, 0xE6),
            code: Color32::from_rgb(0x1F, 0x86, 0x78),
            document: Color32::from_rgb(0x3E, 0x5C, 0xB8),
            image: Color32::from_rgb(0xB8, 0x45, 0x70),
            archive: Color32::from_rgb(0x9A, 0x6A, 0x1E),
            plain: Color32::from_rgb(0x6B, 0x6C, 0x7B),
        },
    };

    /// The same design language after dark: opaque cards, a still ground, one
    /// accent. Not a separate look — the light theme with the values inverted
    /// around the same structure, so a hairline still separates a card from the
    /// ground and nothing moves.
    pub const DARK: Palette = Palette {
        // Shows only where the window is not transparent — elsewhere the
        // desktop is the ground.
        ground: Color32::from_rgb(0x0A, 0x09, 0x0F),
        ground_deep: Color32::from_rgb(0x0A, 0x09, 0x0F),

        // A pane runs from #1A1822 at the top to this at the bottom.
        card: Color32::from_rgb(0x14, 0x12, 0x1A),
        card_top: Color32::from_rgb(0x1A, 0x18, 0x22),
        card_hover: Color32::from_rgb(0x1F, 0x1C, 0x28),
        raised: Color32::from_rgb(0x2A, 0x26, 0x36),
        elevated: Color32::from_rgb(0x22, 0x1F, 0x2B),
        inset: Color32::from_rgb(0x0F, 0x0E, 0x15),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.5,
                y: -0.2,
                radius: 0.9,
                colour: Color32::from_rgba_unmultiplied_const(0x2A, 0x22, 0x40, 40),
            },
            Orb {
                x: 0.9,
                y: 0.1,
                radius: 0.6,
                colour: Color32::from_rgba_unmultiplied_const(0x22, 0x1E, 0x38, 24),
            },
        ],

        // Barely lines at all: a pane's edge is where its tone stops.
        border: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 9),
        border_strong: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 18),

        text: Color32::from_rgb(0xEE, 0xEA, 0xF6),
        text_muted: Color32::from_rgb(0xA3, 0x9C, 0xB4),
        text_faint: Color32::from_rgb(0x85, 0x7E, 0x97),

        icon: Color32::from_rgb(0xA3, 0x9C, 0xB4),

        accent: Color32::from_rgb(0xB9, 0xA6, 0xF5),
        accent_hover: Color32::from_rgb(0xCB, 0xBC, 0xF8),
        accent_pressed: Color32::from_rgb(0xA2, 0x8C, 0xEB),

        // The accent at 14%: a wash the row's own text stays readable on.
        selection: Color32::from_rgba_unmultiplied_const(0xB9, 0xA6, 0xF5, 36),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 9),

        success: Color32::from_rgb(0x8F, 0xCF, 0xC0),
        warning: Color32::from_rgb(0xE2, 0xC0, 0x8D),
        danger: Color32::from_rgb(0xE6, 0xA8, 0xBF),
        cloud: Color32::from_rgb(0x9D, 0xB4, 0xF2),

        tints: Tints {
            folder: Color32::from_rgb(0xB9, 0xA6, 0xF5),
            code: Color32::from_rgb(0x8F, 0xC9, 0xC0),
            document: Color32::from_rgb(0xA9, 0xB8, 0xE8),
            image: Color32::from_rgb(0xE6, 0xA8, 0xBF),
            archive: Color32::from_rgb(0xE2, 0xC0, 0x8D),
            plain: Color32::from_rgb(0xA3, 0x9C, 0xB4),
        },
    };

    /// Text a step quieter than [`Palette::text`], for the many rows that
    /// are not the one being looked at: sidebar destinations, unselected
    /// file names.
    pub fn text_soft(&self) -> Color32 {
        lerp_colour(self.text, self.text_muted, 0.45)
    }

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

pub const ROW_HEIGHT: f32 = 42.0;
pub const NAV_HEIGHT: f32 = 32.0;
pub const GUTTER: f32 = 14.0;
/// A pane: the sidebar, the rail, a file pane.
pub const RADIUS_CARD: u8 = 26;
/// A row, a button, a menu item.
pub const RADIUS_CONTROL: u8 = 13;
/// A file's icon tile, a key cap.
pub const RADIUS_SMALL: u8 = 9;
/// A recessed well, a card inside a pane.
pub const RADIUS_WELL: u8 = 16;
/// A panel set into a pane: the column view's columns.
pub const RADIUS_PANEL: u8 = 20;

/// Corner radius that makes a shape of `height` a pill.
pub fn pill(height: f32) -> CornerRadius {
    CornerRadius::same((height / 2.0).round().clamp(0.0, 255.0) as u8)
}

/// The soft, wide shadow a pane casts on whatever is behind it.
pub fn card_shadow(p: &Palette) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 14],
        blur: 40,
        spread: 0,
        color: p.shadow,
    }
}

/// A card as an egui frame: a solid fill, a faint edge, rounded corners and a
/// soft shadow. For surfaces drawn through a frame; a pane drawn by hand uses
/// [`paint_pane`], which adds the sheen a frame cannot.
pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .shadow(card_shadow(p))
}

/// The same frame with nothing painted: margins only, for a panel whose
/// surface is drawn by [`paint_pane`] inside it.
pub fn bare_card() -> egui::Frame {
    egui::Frame::new()
}

/// Paints a pane: its shadow, a surface that runs from [`Palette::card_top`]
/// down to [`Palette::card`], a faint edge, and a lit line along the top.
///
/// The sheen is what makes the surface read as velvet rather than flat paint,
/// and egui has no rounded gradient to draw it with. So the pane is filled
/// solid in its bottom colour first — that fill is the anti-aliased edge — and
/// the gradient is laid inside it as a mesh following the same rounded outline
/// a point in from the edge. The mesh's own edge is not smoothed, but it sits
/// on a colour a few levels away from its own, where no stair-step can be seen.
pub fn paint_pane(painter: &egui::Painter, rect: egui::Rect, radius: f32, p: &Palette) {
    let corners = CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
    painter.add(card_shadow(p).as_shape(rect, corners));
    painter.rect_filled(rect, corners, p.card);

    if p.card_top != p.card {
        let inner = rect.shrink(1.0);
        let mut outline = Vec::new();
        egui::epaint::tessellator::path::rounded_rectangle(
            &mut outline,
            inner,
            egui::epaint::CornerRadiusF32::same((radius - 1.0).max(0.0)),
        );
        let colour_at = |y: f32| {
            let t = ((y - inner.top()) / inner.height().max(1.0)).clamp(0.0, 1.0);
            lerp_colour(p.card_top, p.card, t)
        };
        let mut mesh = egui::Mesh::default();
        let centre = inner.center();
        mesh.colored_vertex(centre, colour_at(centre.y));
        for point in &outline {
            mesh.colored_vertex(*point, colour_at(point.y));
        }
        let n = outline.len() as u32;
        for i in 0..n {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    painter.rect_stroke(rect, corners, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    // The lit top edge, round the corners.
    let inner = CornerRadius::same(corners.nw.saturating_sub(1));
    paint_top_edge(painter, rect.shrink(1.0), inner, Color32::from_rgba_unmultiplied(255, 255, 255, 12));
}

/// Corner radius of the window itself, as the mockups' frame has it.
pub const RADIUS_WINDOW: u8 = 22;

/// The window's own surface: the ground the panes sit on, with rounded
/// corners and a faint glow down from the top, as the mockups' frame has.
///
/// The glow is a band across the top whose colour is a function of height
/// alone, so a fan of flat triangles reproduces it exactly, and it ends on the
/// ground's own colour, so there is no seam where it stops.
pub fn paint_ground(painter: &egui::Painter, rect: egui::Rect, p: &Palette) {
    let radius = RADIUS_WINDOW as f32;
    let corners = CornerRadius::same(RADIUS_WINDOW);
    painter.rect_filled(rect, corners, p.ground);

    let glow = lerp_colour(p.ground, p.card_top, 0.6);
    let band = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() * 0.4));
    let mut outline = Vec::new();
    egui::epaint::tessellator::path::rounded_rectangle(
        &mut outline,
        band,
        egui::epaint::CornerRadiusF32 { nw: radius, ne: radius, sw: 0.0, se: 0.0 },
    );
    let colour_at = |y: f32| {
        let t = ((y - band.top()) / band.height().max(1.0)).clamp(0.0, 1.0);
        lerp_colour(glow, p.ground, t)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(band.center(), colour_at(band.center().y));
    for point in &outline {
        mesh.colored_vertex(*point, colour_at(point.y));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));

    // A hairline edge, so the window still reads against a dark wallpaper.
    painter.rect_stroke(rect, corners, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
}

/// A recessed well: the filter field, the view switcher, a progress track.
/// A step darker than the surface it sits in, with a shadow along its inside
/// top edge where the surface would overhang it.
pub fn paint_well(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, p: &Palette) {
    painter.rect_filled(rect, corners, p.inset);
    painter.rect_stroke(
        rect,
        corners,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 9)),
        egui::StrokeKind::Inside,
    );
    // The overhang's shadow follows the outline round the top corners and
    // fades out down the sides, rather than being a straight rule that stops
    // short of them — on a pill, that rule was a hard edge of its own.
    let dark = p.ground.r() < 128;
    paint_top_edge(painter, rect.translate(egui::vec2(0.0, 1.0)), corners, Color32::from_black_alpha(if dark { 80 } else { 16 }));
}

/// A one-point line along the top of a rounded `rect`, round its corners, and
/// clipped before the straight sides.
fn paint_top_edge(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, colour: Color32) {
    let depth = (corners.nw.max(corners.ne) as f32).max(4.0) * 0.8;
    let band = egui::Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + depth));
    painter
        .with_clip_rect(band.intersect(painter.clip_rect()))
        .rect_stroke(rect, corners, Stroke::new(1.0, colour), egui::StrokeKind::Inside);
}

/// A control lifted off the surface — the chosen segment, the active tab, a
/// rail button under the pointer: [`Palette::raised`] with a lit top edge and
/// a small shadow under it.
pub fn paint_raised(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, p: &Palette) {
    painter.add(
        egui::epaint::Shadow {
            offset: [0, 2],
            blur: 6,
            spread: 0,
            color: Color32::from_black_alpha(90),
        }
        .as_shape(rect, corners),
    );
    painter.rect_filled(rect, corners, p.raised);
    paint_top_edge(painter, rect, corners, Color32::from_rgba_unmultiplied(255, 255, 255, 18));
}

/// `colour` at `alpha` out of 255, for a tint laid over a surface.
pub fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A faint vertical gradient across `rect`, from the card colour to a slightly
/// tinted version of it.
///
/// For a surface that needs to read as distinct from the card beside it without
/// a second border or a heavier fill. Two triangles, drawn once per frame.
///
/// The tint leans on the accent rather than on grey, because a grey wash on a
/// near-white card reads as dirt; a trace of the brand hue reads as a surface.
///
/// # Staying inside the card
///
/// `rect` is the card, and the mesh is clipped to it inset by `radius` at the
/// top and bottom. A rectangular mesh cannot have rounded corners, so without
/// the inset it paints over them and squares the card off — and any overshoot
/// beyond the card lands on the ground as a band down each side. The ramp is
/// still computed across the whole card, so what shows in the clipped band is
/// the right part of it.
pub fn vertical_wash(painter: &egui::Painter, rect: egui::Rect, radius: f32, p: &Palette) {
    let painter = painter.with_clip_rect(rect.shrink2(egui::vec2(0.0, radius)));
    let top = p.card;
    // Toward the accent, but barely: 6% at the bottom edge.
    let mix = |a: u8, b: u8| (a as f32 * 0.94 + b as f32 * 0.06).round() as u8;
    let bottom = Color32::from_rgb(
        mix(p.card.r(), p.accent.r()),
        mix(p.card.g(), p.accent.g()),
        mix(p.card.b(), p.accent.b()),
    );

    let mut mesh = egui::Mesh::default();
    for (i, (at, colour)) in [(rect.top(), top), (rect.bottom(), bottom)]
        .into_iter()
        .enumerate()
    {
        mesh.colored_vertex(pos2(rect.left(), at), colour);
        mesh.colored_vertex(pos2(rect.right(), at), colour);
        let _ = i;
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// An inset top-edge highlight stroke.
///
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
        .corner_radius(CornerRadius::same(RADIUS_WELL))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 18],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(140),
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

// --- type --------------------------------------------------------------------

/// The font family for headings and anything set heavier than body text.
///
/// egui draws one weight per family, so weight is a family of its own. Always
/// defined by [`install_fonts`] — falling back to the body face when no heavier
/// one is supplied — because naming a family egui does not have is a panic.
pub const STRONG: &str = "neutron-strong";

/// Font files for the interface.
pub struct Faces {
    /// Body text: names, menus, rows.
    pub regular: Vec<u8>,
    /// Headings and labels. The body face is used when absent.
    pub strong: Option<Vec<u8>>,
    /// Tried before egui's own faces for characters these lack: Manrope has
    /// no ⌘, ⇧ or ⌥, and a shortcut drawn as boxes is worse than one drawn
    /// in the system's face.
    pub fallback: Option<Vec<u8>>,
}

/// Installs the interface faces, keeping egui's own as fallbacks for any
/// character they lack — symbols, scripts the face does not cover.
pub fn install_fonts(ctx: &egui::Context, faces: Option<Faces>) {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;

    let mut fonts = FontDefinitions::default();
    let mut strong_first: Option<&str> = None;
    if let Some(faces) = faces {
        fonts
            .font_data
            .insert("neutron-regular".into(), Arc::new(FontData::from_owned(faces.regular)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "neutron-regular".into());
        if let Some(fallback) = faces.fallback {
            fonts
                .font_data
                .insert("neutron-fallback".into(), Arc::new(FontData::from_owned(fallback)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(1, "neutron-fallback".into());
        }
        if let Some(strong) = faces.strong {
            fonts
                .font_data
                .insert("neutron-strong".into(), Arc::new(FontData::from_owned(strong)));
            strong_first = Some("neutron-strong");
        }
    }
    let mut strong_family = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(first) = strong_first {
        strong_family.insert(0, first.into());
    }
    fonts.families.insert(FontFamily::Name(STRONG.into()), strong_family);
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(STRONG), true));
}

/// The heavier face at `size` — or the body face in a context that never had
/// [`install_fonts`] run, where asking for the family would be a panic.
pub fn strong(ctx: &egui::Context, size: f32) -> egui::FontId {
    let installed = ctx.data(|d| d.get_temp::<bool>(egui::Id::new(STRONG))).unwrap_or(false);
    if installed {
        egui::FontId::new(size, egui::FontFamily::Name(STRONG.into()))
    } else {
        egui::FontId::proportional(size)
    }
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
    // Also the focus ring of a stock text field: a soft one, as the app's
    // own fields draw.
    visuals.selection.stroke = Stroke::new(1.0, with_alpha(p.accent, 120));
    visuals.text_edit_bg_color = Some(p.inset);
    visuals.hyperlink_color = p.accent;
    visuals.window_corner_radius = CornerRadius::same(RADIUS_WELL);

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
