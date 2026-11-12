//! UI palette. The only theming surface in v1; v2 adds named themes by
//! producing more `ThemePalette` values, with no changes to the renderer.

use crate::config::{PromptIcon, ThemeSection};

/// sRGB color, independent of the GUI crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Accepts `#rrggbb` and `rrggbb`.
    pub fn from_hex(hex: &str) -> Result<Self, ThemeError> {
        let s = hex.trim();
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.is_ascii() {
            return Err(ThemeError::InvalidHex(hex.to_string()));
        }
        let r = u8::from_str_radix(&s[0..2], 16)
            .map_err(|_| ThemeError::InvalidHex(hex.to_string()))?;
        let g = u8::from_str_radix(&s[2..4], 16)
            .map_err(|_| ThemeError::InvalidHex(hex.to_string()))?;
        let b = u8::from_str_radix(&s[4..6], 16)
            .map_err(|_| ThemeError::InvalidHex(hex.to_string()))?;
        Ok(Self { r, g, b })
    }

    /// Adapter conversion for egui; UI code never parses colors itself.
    pub fn to_color32(self) -> eframe::egui::Color32 {
        eframe::egui::Color32::from_rgb(self.r, self.g, self.b)
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ThemePreset {
    pub name: &'static str,
    pub background: &'static str,
    pub foreground: &'static str,
    pub accent: &'static str,
    pub warning: &'static str,
    pub danger: &'static str,
    pub selection: &'static str,
    pub overlay_bg: &'static str,
}

pub const THEME_PRESETS: &[ThemePreset] = &[
    ThemePreset {
        name: "Key Gray",
        background: "#252527",
        foreground: "#e4e4e4",
        accent: "#d4d4d4",
        warning: "#d9a05b",
        danger: "#e06c75",
        selection: "#363639",
        overlay_bg: "#2c2c2f",
    },
    ThemePreset {
        name: "Warm Stone",
        background: "#272523",
        foreground: "#e6e2dc",
        accent: "#d8cfc4",
        warning: "#d9a05b",
        danger: "#e06c75",
        selection: "#383430",
        overlay_bg: "#2e2b28",
    },
    ThemePreset {
        name: "Slate",
        background: "#242830",
        foreground: "#dde3ee",
        accent: "#c7d0e0",
        warning: "#e0af68",
        danger: "#f7768e",
        selection: "#333945",
        overlay_bg: "#2a2f3a",
    },
    ThemePreset {
        name: "Light",
        background: "#f4f4f2",
        foreground: "#252527",
        accent: "#444448",
        warning: "#b87628",
        danger: "#c94a53",
        selection: "#dcdcd9",
        overlay_bg: "#eaeae7",
    },
];

/// Resolved palette handed to every widget draw call.
#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub selection: Rgb,
    pub overlay_bg: Rgb,
    pub font_size: f32,
    pub prompt_icon: PromptIcon,
}

impl ThemePalette {
    /// Built-in adaptive neutral graphite fallback (`#252527` base) used when no config exists yet.
    #[allow(dead_code)] // Shortcut: фолбэк включается из config.rs инлайном.
    pub fn dark() -> Self {
        Self {
            background: Rgb {
                r: 0x25,
                g: 0x25,
                b: 0x27,
            },
            foreground: Rgb {
                r: 0xe4,
                g: 0xe4,
                b: 0xe4,
            },
            accent: Rgb {
                r: 0xd4,
                g: 0xd4,
                b: 0xd4,
            },
            warning: Rgb {
                r: 0xd9,
                g: 0xa0,
                b: 0x5b,
            },
            danger: Rgb {
                r: 0xe0,
                g: 0x6c,
                b: 0x75,
            },
            selection: Rgb {
                r: 0x36,
                g: 0x36,
                b: 0x39,
            },
            overlay_bg: Rgb {
                r: 0x2c,
                g: 0x2c,
                b: 0x2f,
            },
            font_size: 14.0,
            prompt_icon: PromptIcon::Arrow,
        }
    }

    /// Relative luminance (0..=255) of `background` for adaptive UI contrast.
    pub fn is_light(&self) -> bool {
        let lum = (self.background.r as u32 * 299
            + self.background.g as u32 * 587
            + self.background.b as u32 * 114)
            / 1000;
        lum >= 128
    }

    pub fn blend(a: Rgb, b: Rgb, a_pct: u16) -> eframe::egui::Color32 {
        let b_pct = 100u16.saturating_sub(a_pct);
        let ch = |x: u8, y: u8| -> u8 { ((x as u16 * a_pct + y as u16 * b_pct) / 100) as u8 };
        eframe::egui::Color32::from_rgb(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b))
    }

    /// Adaptive muted foreground color derived from `foreground` + `background`.
    pub fn muted_fg(&self, fg_pct: u16) -> eframe::egui::Color32 {
        Self::blend(self.foreground, self.background, fg_pct)
    }

    /// Adaptive elevated surface for bars and pills.
    pub fn surface(&self) -> eframe::egui::Color32 {
        Self::blend(
            self.foreground,
            self.background,
            if self.is_light() { 6 } else { 4 },
        )
    }

    /// Adaptive subtle hairline/border color.
    pub fn hairline(&self) -> eframe::egui::Color32 {
        Self::blend(self.foreground, self.background, 14)
    }

    /// Strict conversion; any bad hex string is a startup error.
    pub fn from_section(section: &ThemeSection) -> Result<Self, ThemeError> {
        if !section.font_size.is_finite() || section.font_size <= 0.0 {
            return Err(ThemeError::InvalidHex(format!(
                "invalid font_size: {}",
                section.font_size
            )));
        }
        Ok(Self {
            background: Rgb::from_hex(&section.background)?,
            foreground: Rgb::from_hex(&section.foreground)?,
            accent: Rgb::from_hex(&section.accent)?,
            warning: Rgb::from_hex(&section.warning)?,
            danger: Rgb::from_hex(&section.danger)?,
            selection: Rgb::from_hex(&section.selection)?,
            overlay_bg: Rgb::from_hex(&section.overlay_bg)?,
            font_size: section.font_size,
            prompt_icon: section.prompt_icon,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("invalid hex color: {0}")]
    InvalidHex(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_and_key_gray_replaces_1a1a1a() {
        let def = ThemePalette::dark();
        assert_eq!(def.background.to_hex(), "#252527");
        assert_eq!(def.accent.to_hex(), "#d4d4d4");
        for preset in THEME_PRESETS {
            let section = ThemeSection {
                background: preset.background.into(),
                foreground: preset.foreground.into(),
                accent: preset.accent.into(),
                warning: preset.warning.into(),
                danger: preset.danger.into(),
                selection: preset.selection.into(),
                overlay_bg: preset.overlay_bg.into(),
                font_size: 14.0,
                prompt_icon: PromptIcon::Arrow,
            };
            assert!(
                ThemePalette::from_section(&section).is_ok(),
                "{}",
                preset.name
            );
        }
    }
}
