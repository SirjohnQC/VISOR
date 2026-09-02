//! Colour tokens for the tuning window.
//!
//! Pure data plus one piece of arithmetic, so the whole palette is testable
//! without a window.
//!
//! Built as a **table selected at runtime** rather than constants baked into
//! the paint calls. There are three palettes already and the OLED one is the
//! reason the shape matters: adding or changing a palette has to be a data
//! change, never a sweep through every draw call.

/// Straight 8-bit sRGB. Direct2D wants floats, hence [`Rgb::to_f32`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Linear-ish 0..1 components for Direct2D brushes.
    pub fn to_f32(self) -> (f32, f32, f32) {
        (
            self.0 as f32 / 255.0,
            self.1 as f32 / 255.0,
            self.2 as f32 / 255.0,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
    /// True-black ground. Not a darker `Dark`: on an OLED, `#000000` switches
    /// the subpixels off entirely, where `Dark`'s `#0B0C0E` leaves every one
    /// of them lit and drawing power. A program whose whole purpose is
    /// protecting an OLED should not paint its own window on lit pixels.
    ///
    /// Deliberately not the default. Pure black with high-contrast text
    /// haloes badly on an LCD, so this is offered, not imposed.
    Oled,
}

impl Theme {
    /// Parse a config value. Unknown strings fall back to `Dark` rather than
    /// failing — a typo in the theme name must not stop VISOR starting.
    pub fn parse(s: &str) -> Theme {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Theme::Light,
            "oled" => Theme::Oled,
            _ => Theme::Dark,
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Theme::Dark | Theme::Oled)
    }
}

/// Every colour the window draws with.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Rgb,
    pub surface: Rgb,
    pub well: Rgb,
    pub hair: Rgb,
    pub strong: Rgb,

    pub t1: Rgb,
    pub t2: Rgb,
    pub t3: Rgb,
    pub t4: Rgb,

    /// Face-ratio states. These, the warning and the focus ring are the only
    /// places chroma is ever spent — see the design rule: the only saturated
    /// pixels in the window are the ones carrying the measurement.
    pub good: Rgb,
    pub marginal: Rgb,
    pub below: Rgb,
    pub no_signal: Rgb,
    pub dead: Rgb,

    /// The user's line. Neutral on purpose: it is not a measurement.
    pub threshold: Rgb,

    pub warn_text: Rgb,
    pub warn_fill: Rgb,
    pub warn_border: Rgb,
    pub danger: Rgb,
    pub focus: Rgb,

    /// The video plate stays dark in every theme — video on a light ground
    /// glares and the letterbox bars look like a bug.
    pub plate: Rgb,

    /// The two ends of the timing rail's brightness scale. The rail's fill
    /// literally is the screen brightness at that point on the timeline, so
    /// these are the screen's own extremes, not decoration.
    pub level_full: Rgb,
    pub level_black: Rgb,
}

const DARK: Palette = Palette {
    bg: Rgb(0x0B, 0x0C, 0x0E),
    surface: Rgb(0x14, 0x16, 0x1A),
    well: Rgb(0x06, 0x07, 0x08),
    hair: Rgb(0x1F, 0x23, 0x29),
    strong: Rgb(0x2C, 0x32, 0x3A),
    t1: Rgb(0xE8, 0xEA, 0xED),
    t2: Rgb(0x9A, 0xA1, 0xAC),
    t3: Rgb(0x76, 0x7E, 0x89),
    t4: Rgb(0x45, 0x4B, 0x54),
    good: Rgb(0x4A, 0xDE, 0x9E),
    marginal: Rgb(0xE8, 0xC5, 0x5A),
    below: Rgb(0xFF, 0x8A, 0x6B),
    no_signal: Rgb(0x7A, 0x82, 0x8D),
    dead: Rgb(0x3A, 0x3F, 0x47),
    threshold: Rgb(0xE8, 0xEA, 0xED),
    warn_text: Rgb(0xFF, 0xB8, 0x4D),
    warn_fill: Rgb(0x2A, 0x22, 0x14),
    warn_border: Rgb(0x7A, 0x5A, 0x24),
    danger: Rgb(0xFF, 0x5C, 0x5C),
    focus: Rgb(0x7F, 0xB4, 0xFF),
    plate: Rgb(0x0A, 0x0B, 0x0D),
    level_full: Rgb(0xC9, 0xCE, 0xD6),
    level_black: Rgb(0x00, 0x00, 0x00),
};

const LIGHT: Palette = Palette {
    bg: Rgb(0xF4, 0xF5, 0xF7),
    surface: Rgb(0xFF, 0xFF, 0xFF),
    well: Rgb(0xE7, 0xE9, 0xEC),
    hair: Rgb(0xDF, 0xE2, 0xE7),
    strong: Rgb(0xC6, 0xCB, 0xD3),
    t1: Rgb(0x16, 0x18, 0x1C),
    t2: Rgb(0x5A, 0x61, 0x6B),
    t3: Rgb(0x66, 0x6D, 0x77),
    t4: Rgb(0xA8, 0xAE, 0xB7),
    good: Rgb(0x0B, 0x7A, 0x4D),
    marginal: Rgb(0x8A, 0x63, 0x00),
    below: Rgb(0xB8, 0x43, 0x1A),
    no_signal: Rgb(0x6B, 0x72, 0x7C),
    dead: Rgb(0xB6, 0xBB, 0xC2),
    threshold: Rgb(0x16, 0x18, 0x1C),
    warn_text: Rgb(0x8A, 0x50, 0x00),
    warn_fill: Rgb(0xFF, 0xF3, 0xE0),
    warn_border: Rgb(0xE0, 0xA9, 0x57),
    danger: Rgb(0xC2, 0x2C, 0x2C),
    focus: Rgb(0x0F, 0x62, 0xD6),
    plate: Rgb(0x0A, 0x0B, 0x0D),
    level_full: Rgb(0xFF, 0xFF, 0xFF),
    level_black: Rgb(0x10, 0x12, 0x16),
};

/// True black, with everything else lifted just enough to separate from it.
///
/// The greys are warmer-neutral and a touch brighter than `DARK`'s: against
/// `#000000` a `#1F2329` hairline nearly vanishes, so the structure has to
/// carry itself with slightly more contrast rather than less.
const OLED: Palette = Palette {
    bg: Rgb(0x00, 0x00, 0x00),
    surface: Rgb(0x0A, 0x0A, 0x0B),
    well: Rgb(0x00, 0x00, 0x00),
    hair: Rgb(0x24, 0x26, 0x2A),
    strong: Rgb(0x3A, 0x3D, 0x43),
    t1: Rgb(0xF2, 0xF3, 0xF5),
    t2: Rgb(0xA6, 0xAC, 0xB6),
    t3: Rgb(0x80, 0x87, 0x91),
    t4: Rgb(0x4C, 0x52, 0x5A),
    good: Rgb(0x4A, 0xDE, 0x9E),
    marginal: Rgb(0xE8, 0xC5, 0x5A),
    below: Rgb(0xFF, 0x8A, 0x6B),
    no_signal: Rgb(0x7A, 0x82, 0x8D),
    dead: Rgb(0x3A, 0x3F, 0x47),
    threshold: Rgb(0xF2, 0xF3, 0xF5),
    warn_text: Rgb(0xFF, 0xB8, 0x4D),
    warn_fill: Rgb(0x24, 0x1C, 0x0E),
    warn_border: Rgb(0x7A, 0x5A, 0x24),
    danger: Rgb(0xFF, 0x5C, 0x5C),
    focus: Rgb(0x7F, 0xB4, 0xFF),
    plate: Rgb(0x00, 0x00, 0x00),
    level_full: Rgb(0xC9, 0xCE, 0xD6),
    level_black: Rgb(0x00, 0x00, 0x00),
};

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Light => LIGHT,
        Theme::Dark => DARK,
        Theme::Oled => OLED,
    }
}

/// The rail's fill colour at `percent` brightness.
///
/// Interpolated in sRGB rather than linear light, deliberately: sRGB tracks
/// *perceived* brightness closely enough that a 20% setting looks like 20%,
/// which is the point — the segment is a picture of what the screen will
/// actually look like, so it has to match the eye, not the photons.
pub fn dim_fill(p: &Palette, percent: u8) -> Rgb {
    let t = percent.min(100) as f32 / 100.0;
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Rgb(
        mix(p.level_black.0, p.level_full.0),
        mix(p.level_black.1, p.level_full.1),
        mix(p.level_black.2, p.level_full.2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oled_ground_is_actually_off_not_merely_dark() {
        // The whole reason this palette exists. `Dark` is a dim grey with
        // every subpixel lit; only #000000 switches them off.
        assert_eq!(palette(Theme::Oled).bg, Rgb(0, 0, 0));
        assert_eq!(palette(Theme::Oled).well, Rgb(0, 0, 0));
        assert_eq!(palette(Theme::Oled).plate, Rgb(0, 0, 0));
        assert_ne!(
            palette(Theme::Dark).bg,
            Rgb(0, 0, 0),
            "Dark is deliberately not true black -- it haloes less on an LCD"
        );
    }

    #[test]
    fn oled_structure_reads_against_true_black() {
        // A hairline tuned for a #0B0C0E ground disappears on #000000, so the
        // OLED palette has to separate its structure MORE, not less.
        let dark = palette(Theme::Dark);
        let oled = palette(Theme::Oled);
        let lum = |c: Rgb| c.0 as u32 + c.1 as u32 + c.2 as u32;
        assert!(
            lum(oled.hair) > lum(dark.hair),
            "OLED hairlines must be brighter than Dark's to survive the ground"
        );
        assert!(lum(oled.strong) > lum(dark.strong));
    }

    #[test]
    fn every_theme_keeps_the_video_plate_dark() {
        // Video on a light ground glares and the letterbox bars read as a bug.
        for t in [Theme::Light, Theme::Dark, Theme::Oled] {
            let p = palette(t);
            let lum = p.plate.0 as u32 + p.plate.1 as u32 + p.plate.2 as u32;
            assert!(lum < 60, "{t:?} plate should stay dark, got {:?}", p.plate);
        }
    }

    #[test]
    fn the_dim_fill_spans_the_screens_own_extremes() {
        let p = palette(Theme::Dark);
        assert_eq!(dim_fill(&p, 0), p.level_black, "0% is the screen off");
        assert_eq!(dim_fill(&p, 100), p.level_full, "100% is full brightness");
        // And an out-of-range config value cannot overshoot.
        assert_eq!(dim_fill(&p, 200), p.level_full);
    }

    #[test]
    fn the_dim_fill_is_monotonic() {
        // Dragging "Dim to" upward must never make the rail segment darker --
        // the segment is a picture of the setting, so a non-monotonic ramp
        // would be the window contradicting itself.
        let p = palette(Theme::Dark);
        let lum = |c: Rgb| c.0 as u32 + c.1 as u32 + c.2 as u32;
        let mut last = 0;
        for pct in 0..=100u8 {
            let l = lum(dim_fill(&p, pct));
            assert!(l >= last, "brightness dipped at {pct}%");
            last = l;
        }
    }

    #[test]
    fn an_unknown_theme_name_falls_back_rather_than_failing() {
        assert_eq!(Theme::parse("oled"), Theme::Oled);
        assert_eq!(Theme::parse("  LIGHT "), Theme::Light);
        assert_eq!(Theme::parse("dark"), Theme::Dark);
        // Spec §7's stance everywhere else in config: a bad value logs and
        // falls back, it never stops VISOR starting.
        assert_eq!(Theme::parse("marathon"), Theme::Dark);
        assert_eq!(Theme::parse(""), Theme::Dark);
    }

    #[test]
    fn colours_convert_to_the_unit_range_direct2d_wants() {
        assert_eq!(Rgb(0, 0, 0).to_f32(), (0.0, 0.0, 0.0));
        assert_eq!(Rgb(255, 255, 255).to_f32(), (1.0, 1.0, 1.0));
    }
}
