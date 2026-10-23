// SPDX-License-Identifier: Apache-2.0
//! Original desktop palettes.
//!
//! The palette is deliberately project-specific. It borrows the *idea* of a
//! calm, layered desktop from public design principles but uses independently
//! chosen colours. Every colour is `0x00RRGGBB`.

/// The active visual theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeKind {
    /// Deep indigo and aurora teal.
    #[default]
    Aurora,
    /// Black, white, and signal yellow for maximum legibility.
    HighContrast,
}

impl ThemeKind {
    /// Returns the palette for this theme.
    #[must_use]
    pub const fn palette(self) -> Palette {
        match self {
            Self::Aurora => AURORA,
            Self::HighContrast => HIGH_CONTRAST,
        }
    }

    /// Cycles to the other theme.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Aurora => Self::HighContrast,
            Self::HighContrast => Self::Aurora,
        }
    }
}

/// The intensity of the Liquid Glass-style translucency.
///
/// Golden Gate exposes a global transparency control. Aether mirrors the
/// *behaviour* with three readable steps: clear, balanced, and tinted. The
/// panels and window shells are alpha-composited over the procedural wallpaper;
/// no blur is claimed on hardware that has no GPU compositor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlassLevel {
    /// More background visible.
    Clear,
    /// Default diffusion.
    #[default]
    Balanced,
    /// More tint, maximum text isolation.
    Tinted,
}

impl GlassLevel {
    /// Cycles to the next level.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Clear => Self::Balanced,
            Self::Balanced => Self::Tinted,
            Self::Tinted => Self::Clear,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Balanced => "Balanced",
            Self::Tinted => "Tinted",
        }
    }

    /// Compact single-byte form for the persistable settings record.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::Clear => 0,
            Self::Balanced => 1,
            Self::Tinted => 2,
        }
    }

    /// Rebuilds a level from its single-byte form, rejecting unknown values.
    #[must_use]
    pub const fn from_index(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Clear),
            1 => Some(Self::Balanced),
            2 => Some(Self::Tinted),
            _ => None,
        }
    }

    /// Alpha for menu-bar and dock glass.
    #[must_use]
    pub const fn panel_alpha(self) -> u8 {
        match self {
            Self::Clear => 188,
            Self::Balanced => 218,
            Self::Tinted => 244,
        }
    }

    /// Slightly more opaque drop-down menus, without overflowing an 8-bit alpha.
    #[must_use]
    pub const fn menu_alpha(self) -> u8 {
        let alpha = self.panel_alpha().saturating_add(20);
        if alpha > 250 { 250 } else { alpha }
    }

    /// Alpha for window shells.
    #[must_use]
    pub const fn window_alpha(self) -> u8 {
        match self {
            Self::Clear => 224,
            Self::Balanced => 239,
            Self::Tinted => 252,
        }
    }

    /// Alpha for sidebar and toolbar fills.
    #[must_use]
    pub const fn chrome_alpha(self) -> u8 {
        match self {
            Self::Clear => 170,
            Self::Balanced => 205,
            Self::Tinted => 238,
        }
    }
}

/// Colours used by the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Top of the procedural wallpaper.
    pub wallpaper_top: u32,
    /// Bottom of the procedural wallpaper.
    pub wallpaper_bottom: u32,
    /// First soft glow in the wallpaper.
    pub wallpaper_glow: u32,
    /// Second soft glow in the wallpaper.
    pub wallpaper_glow_alt: u32,
    /// Menu bar background.
    pub menu_bar: u32,
    /// Menu bar divider and border.
    pub menu_border: u32,
    /// Dock background.
    pub dock: u32,
    /// Dock border and running indicator.
    pub dock_border: u32,
    /// Window body.
    pub window: u32,
    /// Inactive window body.
    pub window_inactive: u32,
    /// Active title bar.
    pub title: u32,
    /// Inactive title bar.
    pub title_inactive: u32,
    /// Primary text.
    pub text: u32,
    /// Secondary text.
    pub muted: u32,
    /// Tertiary text and disabled controls.
    pub faint: u32,
    /// Horizontal and vertical dividers.
    pub divider: u32,
    /// Focus accent.
    pub accent: u32,
    /// Muted accent used for fills.
    pub accent_soft: u32,
    /// Very dark accent used behind selected rows.
    pub accent_dim: u32,
    /// Warning text and indicators.
    pub warning: u32,
    /// Denied, destructive, or faulted state.
    pub danger: u32,
    /// Verified or healthy state.
    pub success: u32,
    /// Window shadow.
    pub shadow: u32,
    /// Full-screen dimming overlay.
    pub overlay: u32,
    /// Card surface.
    pub card: u32,
    /// Hovered card surface.
    pub card_hover: u32,
    /// Window border.
    pub border: u32,
    /// Close control.
    pub close: u32,
    /// Minimize control.
    pub minimize: u32,
    /// Zoom control.
    pub zoom: u32,
    /// Inactive traffic-light control.
    pub control_off: u32,
}

/// The default Aether palette: deep indigo, aurora teal, and warm signals.
pub const AURORA: Palette = Palette {
    wallpaper_top: 0x06101c,
    wallpaper_bottom: 0x1c2a35,
    wallpaper_glow: 0xb4763f,
    wallpaper_glow_alt: 0x1d5b66,
    menu_bar: 0x0d1724,
    menu_border: 0x33465e,
    dock: 0x111c2a,
    dock_border: 0x3a5068,
    window: 0x1c2a3c,
    window_inactive: 0x182334,
    title: 0x2b3c56,
    title_inactive: 0x202d40,
    text: 0xf4f7fb,
    muted: 0xa6b3c4,
    faint: 0x6a7d91,
    divider: 0x33465a,
    accent: 0x67e8d5,
    accent_soft: 0x2dd4bf,
    accent_dim: 0x12413f,
    warning: 0xf4c46a,
    danger: 0xfb7185,
    success: 0x5ee08a,
    shadow: 0x03060b,
    overlay: 0x03070d,
    card: 0x223247,
    card_hover: 0x2d4058,
    border: 0x415a78,
    close: 0xff6f6f,
    minimize: 0xf7c948,
    zoom: 0x5bd97a,
    control_off: 0x4a5b6e,
};

/// High-contrast palette for low-vision and glare-limited environments.
pub const HIGH_CONTRAST: Palette = Palette {
    wallpaper_top: 0x000000,
    wallpaper_bottom: 0x000000,
    wallpaper_glow: 0x000000,
    wallpaper_glow_alt: 0x000000,
    menu_bar: 0x000000,
    menu_border: 0xffffff,
    dock: 0x000000,
    dock_border: 0xffffff,
    window: 0x0a0a0a,
    window_inactive: 0x050505,
    title: 0x1c1c1c,
    title_inactive: 0x111111,
    text: 0xffffff,
    muted: 0xe0e0e0,
    faint: 0xc0c0c0,
    divider: 0xffffff,
    accent: 0xffff00,
    accent_soft: 0xffff66,
    accent_dim: 0x3a3a00,
    warning: 0xffb000,
    danger: 0xff6060,
    success: 0x60ff60,
    shadow: 0x000000,
    overlay: 0x000000,
    card: 0x141414,
    card_hover: 0x242424,
    border: 0xffffff,
    close: 0xff6060,
    minimize: 0xffd000,
    zoom: 0x60ff60,
    control_off: 0x808080,
};

impl Palette {
    /// Returns the contrast ratio between two colours, scaled by 100.
    ///
    /// This is a compact integer approximation of the WCAG relative-luminance
    /// formula. It is used by host tests to keep the high-contrast theme honest.
    #[must_use]
    pub fn contrast_ratio(first: u32, second: u32) -> u32 {
        let first = luminance(first);
        let second = luminance(second);
        let (lighter, darker) = if first > second {
            (first, second)
        } else {
            (second, first)
        };
        ((lighter + 13) * 100) / (darker + 13)
    }
}

fn luminance(rgb: u32) -> u32 {
    let channel = |shift: u32| -> u32 {
        let value = (rgb >> shift) & 0xff;
        value * value / 255
    };
    let red = channel(16);
    let green = channel(8);
    let blue = channel(0);
    (red * 2126 + green * 7152 + blue * 722) / 10_000
}
