//! Neutron's design system.
//!
//! # Unified obsidian shell (Fairy, Flux, and Proton)
//!
//! One dark window with crisp 1px hairlines separating functional regions:
//! sidebar, action rail, pane tree, preview pane, title bar. No floating
//! 26px cards or wide gutters; neutral dark bases (zero muddy purple tint in
//! Obsidian, Flux, and Fairy) paired with a single crisp accent per theme.

use egui::{Color32, CornerRadius, Stroke, Visuals, pos2};

/// The active colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Obsidian,
    Flux,
    Fairy,
    Aura,
    Gruvbox,
    Light,
}

impl ThemeMode {
    pub const ALL: &'static [ThemeMode] = &[
        ThemeMode::Obsidian,
        ThemeMode::Flux,
        ThemeMode::Fairy,
        ThemeMode::Aura,
        ThemeMode::Gruvbox,
        ThemeMode::Light,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            ThemeMode::Obsidian => "Obsidian",
            ThemeMode::Flux => "Flux",
            ThemeMode::Fairy => "Fairy",
            ThemeMode::Aura => "Aura Dark",
            ThemeMode::Gruvbox => "Gruvbox",
            ThemeMode::Light => "Light",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            ThemeMode::Obsidian => "obsidian",
            ThemeMode::Flux => "flux",
            ThemeMode::Fairy => "fairy",
            ThemeMode::Aura => "aura",
            ThemeMode::Gruvbox => "gruvbox",
            ThemeMode::Light => "light",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "obsidian" | "dark" => Some(ThemeMode::Obsidian),
            "flux" => Some(ThemeMode::Flux),
            "fairy" => Some(ThemeMode::Fairy),
            "aura" | "aura-dark" => Some(ThemeMode::Aura),
            "gruvbox" | "gruvbox-dark" => Some(ThemeMode::Gruvbox),
            "light" => Some(ThemeMode::Light),
            _ => None,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            ThemeMode::Obsidian => ThemeMode::Flux,
            ThemeMode::Flux => ThemeMode::Fairy,
            ThemeMode::Fairy => ThemeMode::Aura,
            ThemeMode::Aura => ThemeMode::Gruvbox,
            ThemeMode::Gruvbox => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Obsidian,
        }
    }

    pub const fn toggled(self) -> Self {
        self.next()
    }

    pub const fn is_dark(self) -> bool {
        !matches!(self, ThemeMode::Light)
    }
}

/// A soft static coloured wash on the ground behind the panes.
#[derive(Debug, Clone, Copy)]
pub struct Orb {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub colour: Color32,
}

/// Semantic colour tokens, named by role rather than appearance.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window outer frame / deepest background.
    pub ground: Color32,
    /// Outer edge of the ground gradient.
    pub ground_deep: Color32,
    /// Sidebar, title bar, action rail, and preview chrome surface.
    pub sidebar: Color32,

    /// Primary content pane fill.
    pub card: Color32,
    /// The top of a floating pane's vertical sheen; same as `card` when flat.
    pub card_top: Color32,
    /// Interactive surface hover fill.
    pub card_hover: Color32,
    /// Raised control: active tab, selected segment, button hover well.
    pub raised: Color32,
    /// Menus, dialogs, and popovers, one step above the content pane.
    pub elevated: Color32,
    /// Recessed well: filter input, address bar, capacity bar track.
    pub inset: Color32,

    /// Shadow colour under floating overlays/popovers.
    pub shadow: Color32,

    /// Subtle colour washes behind non-macOS ambient grounds.
    pub orbs: [Orb; 2],

    /// 1px hairline separating chrome and split panes.
    pub border: Color32,
    /// Stronger hairline for active inputs, dialogs, and popovers.
    pub border_strong: Color32,

    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,

    /// File and folder glyphs.
    pub icon: Color32,

    /// Primary interactive accent.
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,

    /// Selected-row fill.
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
    /// Default dark theme: Fairy/Flux Obsidian base (#08080a-#121318) with
    /// Proton Aura Violet accent (#a277ff).
    pub const OBSIDIAN: Palette = Palette {
        ground: Color32::from_rgb(0x08, 0x08, 0x0A),
        ground_deep: Color32::from_rgb(0x05, 0x05, 0x07),
        sidebar: Color32::from_rgb(0x0D, 0x0E, 0x11),

        card: Color32::from_rgb(0x12, 0x13, 0x18),
        card_top: Color32::from_rgb(0x16, 0x17, 0x1D),
        card_hover: Color32::from_rgb(0x1B, 0x1D, 0x24),
        raised: Color32::from_rgb(0x1F, 0x21, 0x2A),
        elevated: Color32::from_rgb(0x18, 0x19, 0x20),
        inset: Color32::from_rgb(0x0A, 0x0B, 0x0E),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.20,
                y: -0.10,
                radius: 0.90,
                colour: Color32::from_rgba_unmultiplied_const(0xA2, 0x77, 0xFF, 20),
            },
            Orb {
                x: 0.88,
                y: 0.12,
                radius: 0.70,
                colour: Color32::from_rgba_unmultiplied_const(0x3E, 0x5A, 0xEC, 16),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 18),
        border_strong: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 32),

        text: Color32::from_rgb(0xEC, 0xEE, 0xF4),
        text_muted: Color32::from_rgb(0x9D, 0xA2, 0xB0),
        text_faint: Color32::from_rgb(0x78, 0x7D, 0x8C),

        icon: Color32::from_rgb(0x9D, 0xA2, 0xB0),

        accent: Color32::from_rgb(0xA2, 0x77, 0xFF),
        accent_hover: Color32::from_rgb(0xB8, 0x96, 0xFF),
        accent_pressed: Color32::from_rgb(0x8C, 0x5E, 0xF2),

        selection: Color32::from_rgba_unmultiplied_const(0xA2, 0x77, 0xFF, 44),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 12),

        success: Color32::from_rgb(0x61, 0xFF, 0xCA),
        warning: Color32::from_rgb(0xFF, 0xCA, 0x85),
        danger: Color32::from_rgb(0xFF, 0x67, 0x67),
        cloud: Color32::from_rgb(0x82, 0xE2, 0xFF),

        tints: Tints {
            folder: Color32::from_rgb(0xA2, 0x77, 0xFF),
            code: Color32::from_rgb(0x61, 0xFF, 0xCA),
            document: Color32::from_rgb(0x82, 0xE2, 0xFF),
            image: Color32::from_rgb(0xF6, 0x94, 0xFF),
            archive: Color32::from_rgb(0xFF, 0xCA, 0x85),
            plain: Color32::from_rgb(0x9D, 0xA2, 0xB0),
        },
    };

    /// Alias kept for compatibility with existing callers/tests.
    pub const DARK: Palette = Palette::OBSIDIAN;

    /// Flux Velvet: neutral dark slate with Neon Teal primary + Royal Violet secondary.
    pub const FLUX: Palette = Palette {
        ground: Color32::from_rgb(0x09, 0x09, 0x0B),
        ground_deep: Color32::from_rgb(0x06, 0x06, 0x08),
        sidebar: Color32::from_rgb(0x0F, 0x0F, 0x12),

        card: Color32::from_rgb(0x14, 0x14, 0x19),
        card_top: Color32::from_rgb(0x18, 0x18, 0x1F),
        card_hover: Color32::from_rgb(0x1E, 0x1E, 0x24),
        raised: Color32::from_rgb(0x27, 0x27, 0x2E),
        elevated: Color32::from_rgb(0x1A, 0x1A, 0x20),
        inset: Color32::from_rgb(0x0B, 0x0B, 0x0E),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.20,
                y: -0.10,
                radius: 0.90,
                colour: Color32::from_rgba_unmultiplied_const(0x2F, 0xF3, 0xFF, 16),
            },
            Orb {
                x: 0.88,
                y: 0.14,
                radius: 0.72,
                colour: Color32::from_rgba_unmultiplied_const(0x7B, 0x61, 0xFF, 20),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0x96, 0xA0, 0xDC, 34),
        border_strong: Color32::from_rgba_unmultiplied_const(0xB4, 0xBE, 0xFF, 52),

        text: Color32::from_rgb(0xF0, 0xF0, 0xF4),
        text_muted: Color32::from_rgb(0xA0, 0xA4, 0xB8),
        text_faint: Color32::from_rgb(0x74, 0x78, 0x8E),

        icon: Color32::from_rgb(0xA0, 0xA4, 0xB8),

        accent: Color32::from_rgb(0x2F, 0xF3, 0xFF),
        accent_hover: Color32::from_rgb(0x6B, 0xF7, 0xFF),
        accent_pressed: Color32::from_rgb(0x1A, 0xD4, 0xE2),

        selection: Color32::from_rgba_unmultiplied_const(0x2F, 0xF3, 0xFF, 32),
        hover: Color32::from_rgba_unmultiplied_const(0x96, 0xA0, 0xDC, 18),

        success: Color32::from_rgb(0x7C, 0xF5, 0xB0),
        warning: Color32::from_rgb(0xFF, 0xCA, 0x85),
        danger: Color32::from_rgb(0xEC, 0x4B, 0xE0),
        cloud: Color32::from_rgb(0x7B, 0x61, 0xFF),

        tints: Tints {
            folder: Color32::from_rgb(0x2F, 0xF3, 0xFF),
            code: Color32::from_rgb(0x7C, 0xF5, 0xB0),
            document: Color32::from_rgb(0x9D, 0x8D, 0xF1),
            image: Color32::from_rgb(0xEC, 0x4B, 0xE0),
            archive: Color32::from_rgb(0xFF, 0xCA, 0x85),
            plain: Color32::from_rgb(0xA0, 0xA4, 0xB8),
        },
    };

    /// Fairy Obsidian: deep neutral obsidian with Fairy Cobalt Blue accent.
    pub const FAIRY: Palette = Palette {
        ground: Color32::from_rgb(0x08, 0x08, 0x0A),
        ground_deep: Color32::from_rgb(0x05, 0x05, 0x07),
        sidebar: Color32::from_rgb(0x0D, 0x0E, 0x11),

        card: Color32::from_rgb(0x12, 0x13, 0x18),
        card_top: Color32::from_rgb(0x16, 0x17, 0x1C),
        card_hover: Color32::from_rgb(0x1B, 0x1D, 0x24),
        raised: Color32::from_rgb(0x20, 0x22, 0x2B),
        elevated: Color32::from_rgb(0x16, 0x17, 0x1D),
        inset: Color32::from_rgb(0x09, 0x0A, 0x0D),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.25,
                y: -0.08,
                radius: 0.85,
                colour: Color32::from_rgba_unmultiplied_const(0x3E, 0x5A, 0xEC, 22),
            },
            Orb {
                x: 0.90,
                y: 0.16,
                radius: 0.65,
                colour: Color32::from_rgba_unmultiplied_const(0xA9, 0xC4, 0xFF, 16),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 19),
        border_strong: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 34),

        text: Color32::from_rgb(0xEC, 0xEE, 0xF4),
        text_muted: Color32::from_rgb(0x9D, 0xA2, 0xB0),
        text_faint: Color32::from_rgb(0x78, 0x7D, 0x8C),

        icon: Color32::from_rgb(0x9D, 0xA2, 0xB0),

        accent: Color32::from_rgb(0x5B, 0x76, 0xF7),
        accent_hover: Color32::from_rgb(0x7A, 0x91, 0xFA),
        accent_pressed: Color32::from_rgb(0x3E, 0x5A, 0xEC),

        selection: Color32::from_rgba_unmultiplied_const(0x5B, 0x76, 0xF7, 56),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 14),

        success: Color32::from_rgb(0x8F, 0xE0, 0xA6),
        warning: Color32::from_rgb(0xFF, 0xD2, 0x7A),
        danger: Color32::from_rgb(0xFF, 0xB4, 0xBD),
        cloud: Color32::from_rgb(0xA9, 0xC4, 0xFF),

        tints: Tints {
            folder: Color32::from_rgb(0x5B, 0x76, 0xF7),
            code: Color32::from_rgb(0xA9, 0xC4, 0xFF),
            document: Color32::from_rgb(0x8F, 0xE0, 0xA6),
            image: Color32::from_rgb(0xFF, 0xB4, 0xBD),
            archive: Color32::from_rgb(0xFF, 0xD2, 0x7A),
            plain: Color32::from_rgb(0x9D, 0xA2, 0xB0),
        },
    };

    /// Proton Aura Dark: deep plum-slate (#110f18-#17161f) with Aura Violet & Mint.
    pub const AURA: Palette = Palette {
        ground: Color32::from_rgb(0x0F, 0x0E, 0x15),
        ground_deep: Color32::from_rgb(0x0B, 0x0A, 0x10),
        sidebar: Color32::from_rgb(0x11, 0x0F, 0x18),

        card: Color32::from_rgb(0x17, 0x16, 0x1F),
        card_top: Color32::from_rgb(0x1B, 0x19, 0x25),
        card_hover: Color32::from_rgb(0x21, 0x1F, 0x2C),
        raised: Color32::from_rgb(0x29, 0x25, 0x3D),
        elevated: Color32::from_rgb(0x1E, 0x1C, 0x29),
        inset: Color32::from_rgb(0x10, 0x0F, 0x18),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.22,
                y: -0.10,
                radius: 0.90,
                colour: Color32::from_rgba_unmultiplied_const(0xA2, 0x77, 0xFF, 22),
            },
            Orb {
                x: 0.86,
                y: 0.14,
                radius: 0.72,
                colour: Color32::from_rgba_unmultiplied_const(0x61, 0xFF, 0xCA, 16),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 18),
        border_strong: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 33),

        text: Color32::from_rgb(0xED, 0xEC, 0xEE),
        text_muted: Color32::from_rgb(0x9E, 0x9A, 0xB0),
        text_faint: Color32::from_rgb(0x78, 0x74, 0x8A),

        icon: Color32::from_rgb(0x9E, 0x9A, 0xB0),

        accent: Color32::from_rgb(0xA2, 0x77, 0xFF),
        accent_hover: Color32::from_rgb(0xBD, 0x9D, 0xFF),
        accent_pressed: Color32::from_rgb(0x88, 0x5B, 0xEC),

        selection: Color32::from_rgba_unmultiplied_const(0xA2, 0x77, 0xFF, 44),
        hover: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 14),

        success: Color32::from_rgb(0x61, 0xFF, 0xCA),
        warning: Color32::from_rgb(0xFF, 0xCA, 0x85),
        danger: Color32::from_rgb(0xFF, 0x67, 0x67),
        cloud: Color32::from_rgb(0x82, 0xE2, 0xFF),

        tints: Tints {
            folder: Color32::from_rgb(0xA2, 0x77, 0xFF),
            code: Color32::from_rgb(0x61, 0xFF, 0xCA),
            document: Color32::from_rgb(0x82, 0xE2, 0xFF),
            image: Color32::from_rgb(0xF6, 0x94, 0xFF),
            archive: Color32::from_rgb(0xFF, 0xCA, 0x85),
            plain: Color32::from_rgb(0x9E, 0x9A, 0xB0),
        },
    };

    /// Gruvbox Dark: warm retro dark surfaces with Gruvbox Aqua & Yellow.
    pub const GRUVBOX: Palette = Palette {
        ground: Color32::from_rgb(0x1D, 0x20, 0x21),
        ground_deep: Color32::from_rgb(0x17, 0x19, 0x1A),
        sidebar: Color32::from_rgb(0x23, 0x22, 0x21),

        card: Color32::from_rgb(0x28, 0x28, 0x28),
        card_top: Color32::from_rgb(0x2D, 0x2C, 0x2A),
        card_hover: Color32::from_rgb(0x32, 0x30, 0x2F),
        raised: Color32::from_rgb(0x3C, 0x38, 0x36),
        elevated: Color32::from_rgb(0x32, 0x30, 0x2F),
        inset: Color32::from_rgb(0x1E, 0x20, 0x21),

        shadow: Color32::from_black_alpha(150),

        orbs: [
            Orb {
                x: 0.20,
                y: -0.08,
                radius: 0.90,
                colour: Color32::from_rgba_unmultiplied_const(0x83, 0xA5, 0x98, 20),
            },
            Orb {
                x: 0.88,
                y: 0.14,
                radius: 0.72,
                colour: Color32::from_rgba_unmultiplied_const(0xFA, 0xBD, 0x2F, 16),
            },
        ],

        border: Color32::from_rgba_unmultiplied_const(0xEB, 0xDB, 0xB2, 22),
        border_strong: Color32::from_rgba_unmultiplied_const(0xEB, 0xDB, 0xB2, 40),

        text: Color32::from_rgb(0xEB, 0xDB, 0xB2),
        text_muted: Color32::from_rgb(0xBD, 0xAE, 0x93),
        text_faint: Color32::from_rgb(0x92, 0x83, 0x74),

        icon: Color32::from_rgb(0xBD, 0xAE, 0x93),

        accent: Color32::from_rgb(0x83, 0xA5, 0x98),
        accent_hover: Color32::from_rgb(0x8E, 0xC0, 0x7C),
        accent_pressed: Color32::from_rgb(0x68, 0x9D, 0x6A),

        selection: Color32::from_rgba_unmultiplied_const(0x83, 0xA5, 0x98, 52),
        hover: Color32::from_rgba_unmultiplied_const(0xEB, 0xDB, 0xB2, 14),

        success: Color32::from_rgb(0xB8, 0xBB, 0x26),
        warning: Color32::from_rgb(0xFA, 0xBD, 0x2F),
        danger: Color32::from_rgb(0xFB, 0x49, 0x34),
        cloud: Color32::from_rgb(0x8E, 0xC0, 0x7C),

        tints: Tints {
            folder: Color32::from_rgb(0x83, 0xA5, 0x98),
            code: Color32::from_rgb(0xB8, 0xBB, 0x26),
            document: Color32::from_rgb(0x8E, 0xC0, 0x7C),
            image: Color32::from_rgb(0xD3, 0x86, 0x9B),
            archive: Color32::from_rgb(0xFE, 0x80, 0x19),
            plain: Color32::from_rgb(0xBD, 0xAE, 0x93),
        },
    };

    /// Clean Light theme.
    pub const LIGHT: Palette = Palette {
        ground: Color32::from_rgb(0xEC, 0xEE, 0xF2),
        ground_deep: Color32::from_rgb(0xE3, 0xE6, 0xEC),
        sidebar: Color32::from_rgb(0xF5, 0xF6, 0xF9),

        card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_top: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_hover: Color32::from_rgb(0xF3, 0xF4, 0xF8),
        raised: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        elevated: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        inset: Color32::from_rgb(0xEE, 0xF0, 0xF5),

        shadow: Color32::from_black_alpha(18),

        orbs: [
            Orb {
                x: 0.14,
                y: -0.06,
                radius: 0.95,
                colour: Color32::from_rgba_unmultiplied_const(0xA8, 0x8B, 0xF7, 24),
            },
            Orb {
                x: 0.94,
                y: 0.10,
                radius: 0.88,
                colour: Color32::from_rgba_unmultiplied_const(0x6E, 0xA8, 0xF0, 20),
            },
        ],

        border: Color32::from_rgb(0xE2, 0xE4, 0xEC),
        border_strong: Color32::from_rgb(0xD0, 0xD3, 0xDF),

        text: Color32::from_rgb(0x16, 0x16, 0x1D),
        text_muted: Color32::from_rgb(0x5F, 0x61, 0x70),
        text_faint: Color32::from_rgb(0x73, 0x75, 0x85),

        icon: Color32::from_rgb(0x5F, 0x61, 0x70),

        accent: Color32::from_rgb(0x6D, 0x4A, 0xFF),
        accent_hover: Color32::from_rgb(0x5A, 0x38, 0xE5),
        accent_pressed: Color32::from_rgb(0x49, 0x2B, 0xC8),

        selection: Color32::from_rgb(0xEC, 0xE7, 0xFE),
        hover: Color32::from_rgba_unmultiplied_const(0x16, 0x16, 0x1D, 10),

        success: Color32::from_rgb(0x07, 0x84, 0x5C),
        warning: Color32::from_rgb(0xA8, 0x5C, 0x12),
        danger: Color32::from_rgb(0xC0, 0x35, 0x4C),
        cloud: Color32::from_rgb(0x6D, 0x4A, 0xFF),

        tints: Tints {
            folder: Color32::from_rgb(0x6D, 0x4A, 0xFF),
            code: Color32::from_rgb(0x1F, 0x86, 0x78),
            document: Color32::from_rgb(0x3E, 0x5C, 0xB8),
            image: Color32::from_rgb(0xB8, 0x45, 0x70),
            archive: Color32::from_rgb(0x9A, 0x6A, 0x1E),
            plain: Color32::from_rgb(0x5F, 0x61, 0x70),
        },
    };

    pub fn text_soft(&self) -> Color32 {
        lerp_colour(self.text, self.text_muted, 0.45)
    }

    pub const fn for_mode(mode: ThemeMode) -> Palette {
        match mode {
            ThemeMode::Obsidian => Palette::OBSIDIAN,
            ThemeMode::Flux => Palette::FLUX,
            ThemeMode::Fairy => Palette::FAIRY,
            ThemeMode::Aura => Palette::AURA,
            ThemeMode::Gruvbox => Palette::GRUVBOX,
            ThemeMode::Light => Palette::LIGHT,
        }
    }

    pub fn card_over(&self, backdrop: Color32) -> Color32 {
        composite(self.card, backdrop)
    }

    pub fn card_worst_case(&self) -> Color32 {
        self.card_over(self.ground_deep)
    }

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

pub const ROW_HEIGHT: f32 = 32.0;
pub const NAV_HEIGHT: f32 = 28.0;
pub const GUTTER: f32 = 1.0;
/// Floating overlays / dialogs (Finder, Connect, Confirm).
pub const RADIUS_CARD: u8 = 12;
/// A row, button, or menu item.
pub const RADIUS_CONTROL: u8 = 7;
/// A file's icon tile, a key cap.
pub const RADIUS_SMALL: u8 = 6;
/// A recessed well, a card inside a pane.
pub const RADIUS_WELL: u8 = 8;
/// A panel set into a pane: the column view's columns, terminal surface.
pub const RADIUS_PANEL: u8 = 10;

/// Corner radius that makes a shape of `height` a pill.
pub fn pill(height: f32) -> CornerRadius {
    CornerRadius::same((height / 2.0).round().clamp(0.0, 255.0) as u8)
}

/// Shadow under floating overlays / dialogs.
pub fn card_shadow(p: &Palette) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 12],
        blur: 32,
        spread: 0,
        color: p.shadow,
    }
}

pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .shadow(card_shadow(p))
}

pub fn bare_card() -> egui::Frame {
    egui::Frame::new()
}

/// Paints a floating dialog / overlay surface: elevated fill, crisp hairline,
/// specular top rim, and soft shadow.
pub fn paint_pane(painter: &egui::Painter, rect: egui::Rect, radius: f32, p: &Palette) {
    let corners = CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
    painter.add(card_shadow(p).as_shape(rect, corners));
    painter.rect_filled(rect, corners, p.elevated);
    painter.rect_stroke(rect, corners, Stroke::new(1.0, p.border_strong), egui::StrokeKind::Inside);
    let inner = CornerRadius::same(corners.nw.saturating_sub(1));
    let dark = p.ground.r() < 128;
    if dark {
        paint_top_edge(painter, rect.shrink(1.0), inner, Color32::from_rgba_unmultiplied(255, 255, 255, 28));
    }
}

/// Corner radius of the window itself (matches Proton `--r-win: 13px`).
pub const RADIUS_WINDOW: u8 = 13;

/// The window's own surface: solid ground with rounded corners and a crisp
/// hairline edge.
pub fn paint_ground(painter: &egui::Painter, rect: egui::Rect, p: &Palette) {
    let corners = CornerRadius::same(RADIUS_WINDOW);
    painter.rect_filled(rect, corners, p.ground);
    painter.rect_stroke(rect, corners, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
}

/// A recessed well: the filter field, view switcher, capacity bar track.
pub fn paint_well(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, p: &Palette) {
    painter.rect_filled(rect, corners, p.inset);
    painter.rect_stroke(
        rect,
        corners,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
}

fn paint_top_edge(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, colour: Color32) {
    let depth = (corners.nw.max(corners.ne) as f32).max(4.0) * 0.8;
    let band = egui::Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + depth));
    painter
        .with_clip_rect(band.intersect(painter.clip_rect()))
        .rect_stroke(rect, corners, Stroke::new(1.0, colour), egui::StrokeKind::Inside);
}

/// A control lifted off the surface — the chosen segment, the active tab, a
/// rail button under the pointer: `p.raised` with a hairline edge and top specular.
pub fn paint_raised(painter: &egui::Painter, rect: egui::Rect, corners: CornerRadius, p: &Palette) {
    let dark = p.ground.r() < 128;
    painter.add(
        egui::epaint::Shadow {
            offset: [0, 1],
            blur: 4,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 80 } else { 18 }),
        }
        .as_shape(rect, corners),
    );
    painter.rect_filled(rect, corners, p.raised);
    painter.rect_stroke(rect, corners, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    if dark {
        paint_top_edge(painter, rect.shrink(1.0), corners, Color32::from_rgba_unmultiplied(255, 255, 255, 22));
    }
}

pub fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

pub fn vertical_wash(painter: &egui::Painter, rect: egui::Rect, radius: f32, p: &Palette) {
    let painter = painter.with_clip_rect(rect.shrink2(egui::vec2(0.0, radius)));
    let top = p.card;
    let mix = |a: u8, b: u8| (a as f32 * 0.94 + b as f32 * 0.06).round() as u8;
    let bottom = Color32::from_rgb(
        mix(p.card.r(), p.accent.r()),
        mix(p.card.g(), p.accent.g()),
        mix(p.card.b(), p.accent.b()),
    );

    let mut mesh = egui::Mesh::default();
    for (at, colour) in [(rect.top(), top), (rect.bottom(), bottom)] {
        mesh.colored_vertex(pos2(rect.left(), at), colour);
        mesh.colored_vertex(pos2(rect.right(), at), colour);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

pub fn glass_highlight(painter: &egui::Painter, rect: egui::Rect, radius: egui::CornerRadius) {
    let r = radius.nw as f32;
    painter.line_segment(
        [
            pos2(rect.left() + r, rect.top() + 0.5),
            pos2(rect.right() - r, rect.top() + 0.5),
        ],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 34)),
    );
    for (i, alpha) in [24u8, 14, 7].into_iter().enumerate() {
        let from = rect.top() + r + (i as f32) * r;
        let to = from + r;
        if to > rect.bottom() {
            break;
        }
        let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
        painter.line_segment([pos2(rect.left() + 0.5, from), pos2(rect.left() + 0.5, to)], stroke);
        painter.line_segment([pos2(rect.right() - 0.5, from), pos2(rect.right() - 0.5, to)], stroke);
    }
    painter.line_segment(
        [
            pos2(rect.left() + r, rect.bottom() - 0.5),
            pos2(rect.right() - r, rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, Color32::from_black_alpha(46)),
    );
}

pub fn popover(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(RADIUS_WELL))
        .inner_margin(egui::Margin::same(6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 14],
            blur: 36,
            spread: 0,
            color: Color32::from_black_alpha(140),
        })
}

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

pub const STRONG: &str = "neutron-strong";

pub struct Faces {
    pub regular: Vec<u8>,
    pub strong: Option<Vec<u8>>,
    pub fallback: Option<Vec<u8>>,
}

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
    if mode.is_dark() {
        ctx.set_visuals_of(egui::Theme::Dark, build_visuals(mode));
        ctx.set_visuals_of(egui::Theme::Light, build_visuals(ThemeMode::Light));
        ctx.set_theme(egui::ThemePreference::Dark);
    } else {
        ctx.set_visuals_of(egui::Theme::Dark, build_visuals(ThemeMode::Obsidian));
        ctx.set_visuals_of(egui::Theme::Light, build_visuals(ThemeMode::Light));
        ctx.set_theme(egui::ThemePreference::Light);
    }

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 5.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.scroll.bar_width = 7.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
        style.interaction.resize_grab_radius_side = 6.0;
    });
}

fn build_visuals(mode: ThemeMode) -> Visuals {
    let p = Palette::for_mode(mode);
    let mut visuals = if mode.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = p.elevated;
    visuals.extreme_bg_color = p.ground;
    visuals.faint_bg_color = Color32::TRANSPARENT;
    visuals.window_stroke = Stroke::new(1.0, p.border_strong);
    visuals.selection.bg_fill = p.selection;
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
            ("sidebar", p.sidebar),
            ("card over ground", p.card_over(p.ground)),
            ("card over deep ground", p.card_worst_case()),
            ("card over a lit orb", p.card_lit_case()),
            ("card_hover", p.card_hover),
            ("elevated", p.elevated),
            ("inset", p.inset),
        ]
    }

    fn themes() -> Vec<(&'static str, Palette)> {
        ThemeMode::ALL
            .iter()
            .map(|&m| (m.name(), Palette::for_mode(m)))
            .collect()
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
            assert_eq!(p.sidebar.a(), 255, "{theme}: sidebar must be opaque");
        }
    }

    #[test]
    fn cards_are_opaque() {
        for (theme, p) in themes() {
            assert_eq!(p.card.a(), 255, "{theme}: card must be opaque");
            assert_eq!(p.elevated.a(), 255, "{theme}: popovers cover content");
            assert_eq!(p.inset.a(), 255, "{theme}: inset well must be opaque");
        }
    }

    #[test]
    fn a_card_still_reads_as_a_panel_against_the_ground() {
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
        assert_eq!(ThemeMode::default(), ThemeMode::Obsidian);
        assert!(ThemeMode::default().is_dark());
    }

    #[test]
    fn theme_ids_round_trip_and_cycle() {
        for &mode in ThemeMode::ALL {
            assert_eq!(ThemeMode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(ThemeMode::from_id("dark"), Some(ThemeMode::Obsidian));
        let mut cur = ThemeMode::Obsidian;
        for _ in 0..ThemeMode::ALL.len() {
            cur = cur.next();
        }
        assert_eq!(cur, ThemeMode::Obsidian);
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
