//! Themes. LunarZero themes are 12-colour palettes (`assets/palettes.json`,
//! dark and/or light) from which every UI colour is derived, so a theme is a
//! dozen values instead of sixty. User theme files in the legacy JSON format
//! (`defs` + `theme`, hex / refs / ANSI / `{dark, light}`) are also accepted
//! from the config `themes/` directories.

use std::collections::BTreeMap;
use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use serde_json::Value;

static PALETTES_JSON: &str = include_str!("../../../../assets/palettes.json");

/// A LunarZero palette: the few colours a theme actually chooses.
#[derive(Debug, Clone, Deserialize)]
pub struct Palette {
    pub bg: String,
    pub fg: String,
    pub muted: String,
    pub surface: String,
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaletteSet {
    pub dark: Option<Palette>,
    pub light: Option<Palette>,
}

pub fn palettes() -> &'static BTreeMap<String, PaletteSet> {
    static P: std::sync::LazyLock<BTreeMap<String, PaletteSet>> =
        std::sync::LazyLock::new(|| serde_json::from_str(PALETTES_JSON).expect("embedded palettes"));
    &P
}

pub const DEFAULT_THEME: &str = "lunar";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub fn hex(s: &str) -> Option<Rgba> {
        let h = s.trim_start_matches('#');
        let v = u32::from_str_radix(h, 16).ok()?;
        match h.len() {
            6 => Some(Rgba {
                r: (v >> 16) as u8,
                g: (v >> 8) as u8,
                b: v as u8,
                a: 255,
            }),
            8 => Some(Rgba {
                r: (v >> 24) as u8,
                g: (v >> 16) as u8,
                b: (v >> 8) as u8,
                a: v as u8,
            }),
            3 => {
                let r = ((v >> 8) & 0xf) as u8;
                let g = ((v >> 4) & 0xf) as u8;
                let b = (v & 0xf) as u8;
                Some(Rgba {
                    r: r * 17,
                    g: g * 17,
                    b: b * 17,
                    a: 255,
                })
            }
            _ => None,
        }
    }
    pub fn color(self) -> Color {
        if self.a == 0 {
            Color::Reset
        } else {
            Color::Rgb(self.r, self.g, self.b)
        }
    }
    pub fn luminance(self) -> f64 {
        (0.2126 * self.r as f64 + 0.7152 * self.g as f64 + 0.0722 * self.b as f64) / 255.0
    }
    /// Blend `overlay` over `self` with `alpha` (0..1).
    pub fn tint(self, overlay: Rgba, alpha: f64) -> Rgba {
        let mix = |a: u8, b: u8| ((a as f64) * (1.0 - alpha) + (b as f64) * alpha).round() as u8;
        Rgba {
            r: mix(self.r, overlay.r),
            g: mix(self.g, overlay.g),
            b: mix(self.b, overlay.b),
            a: 255,
        }
    }
}

fn ansi_rgba(code: u8) -> Rgba {
    const BASE: [&str; 16] = [
        "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5", "#7f7f7f",
        "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if (code as usize) < 16 {
        return Rgba::hex(BASE[code as usize]).unwrap();
    }
    if code >= 232 {
        let v = (code - 232) * 10 + 8;
        return Rgba {
            r: v,
            g: v,
            b: v,
            a: 255,
        };
    }
    let c = code - 16;
    let val = |x: u8| if x == 0 { 0 } else { x * 40 + 55 };
    Rgba {
        r: val(c / 36),
        g: val((c / 6) % 6),
        b: val(c % 6),
        a: 255,
    }
}

#[derive(Debug, Deserialize)]
struct ThemeJson {
    #[serde(default)]
    defs: BTreeMap<String, Value>,
    #[serde(default)]
    theme: BTreeMap<String, Value>,
}

pub const KEYS: &[&str] = &[
    "primary",
    "secondary",
    "accent",
    "error",
    "warning",
    "success",
    "info",
    "text",
    "textMuted",
    "background",
    "backgroundPanel",
    "backgroundElement",
    "backgroundMenu",
    "border",
    "borderActive",
    "borderSubtle",
    "diffAdded",
    "diffRemoved",
    "diffContext",
    "diffHunkHeader",
    "diffHighlightAdded",
    "diffHighlightRemoved",
    "diffAddedBg",
    "diffRemovedBg",
    "diffContextBg",
    "diffLineNumber",
    "diffAddedLineNumberBg",
    "diffRemovedLineNumberBg",
    "markdownText",
    "markdownHeading",
    "markdownLink",
    "markdownLinkText",
    "markdownCode",
    "markdownBlockQuote",
    "markdownEmph",
    "markdownStrong",
    "markdownHorizontalRule",
    "markdownListItem",
    "markdownListEnumeration",
    "markdownImage",
    "markdownImageText",
    "markdownCodeBlock",
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    "selectedListItemText",
];

/// A fully-resolved theme: every key → RGBA.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub mode: Mode,
    colors: BTreeMap<String, Rgba>,
    pub thinking_opacity: f64,
}

fn resolve(json: &ThemeJson, key_value: &Value, mode: Mode, chain: &mut Vec<String>) -> Result<Rgba, String> {
    match key_value {
        Value::String(s) => {
            if s == "transparent" || s == "none" {
                return Ok(Rgba::TRANSPARENT);
            }
            if s.starts_with('#') {
                return Rgba::hex(s).ok_or_else(|| format!("bad hex {s}"));
            }
            if chain.contains(s) {
                return Err(format!("circular color reference: {}", chain.join(" -> ")));
            }
            let next = json
                .defs
                .get(s)
                .or_else(|| json.theme.get(s))
                .ok_or_else(|| format!("color reference \"{s}\" not found"))?;
            chain.push(s.clone());
            let out = resolve(json, next, mode, chain);
            chain.pop();
            out
        }
        Value::Number(n) => Ok(ansi_rgba(n.as_u64().unwrap_or(0) as u8)),
        Value::Object(o) => {
            let v = match mode {
                Mode::Dark => o.get("dark"),
                Mode::Light => o.get("light"),
            }
            .ok_or("missing dark/light variant")?;
            resolve(json, v, mode, chain)
        }
        _ => Err("unsupported color value".into()),
    }
}

impl Theme {
    pub fn parse(name: &str, text: &str, mode: Mode) -> Result<Theme, String> {
        let json: ThemeJson = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut colors = BTreeMap::new();
        for (k, v) in &json.theme {
            if k == "thinkingOpacity" {
                continue;
            }
            let c = resolve(&json, v, mode, &mut Vec::new())?;
            colors.insert(k.clone(), c);
        }
        if !colors.contains_key("selectedListItemText") {
            let bg = colors.get("background").copied().unwrap_or(Rgba::TRANSPARENT);
            colors.insert("selectedListItemText".into(), bg);
        }
        if !colors.contains_key("backgroundMenu") {
            let el = colors
                .get("backgroundElement")
                .copied()
                .unwrap_or(Rgba::TRANSPARENT);
            colors.insert("backgroundMenu".into(), el);
        }
        let thinking_opacity = json
            .theme
            .get("thinkingOpacity")
            .and_then(Value::as_f64)
            .unwrap_or(0.6);
        Ok(Theme {
            name: name.into(),
            mode,
            colors,
            thinking_opacity,
        })
    }

    pub fn rgba(&self, key: &str) -> Rgba {
        self.colors.get(key).copied().unwrap_or_else(|| {
            self.colors.get("text").copied().unwrap_or(Rgba {
                r: 200,
                g: 200,
                b: 200,
                a: 255,
            })
        })
    }
    pub fn color(&self, key: &str) -> Color {
        self.rgba(key).color()
    }
    pub fn fg(&self, key: &str) -> Style {
        Style::default().fg(self.color(key))
    }
    pub fn bg(&self, key: &str) -> Style {
        Style::default().bg(self.color(key))
    }
    pub fn bold(&self, key: &str) -> Style {
        self.fg(key).add_modifier(Modifier::BOLD)
    }
    pub fn muted(&self) -> Style {
        self.fg("textMuted")
    }
    pub fn text(&self) -> Style {
        self.fg("text")
    }
    /// A dimmed variant of `text` for reasoning blocks.
    pub fn thinking(&self) -> Style {
        let bg = self.rgba("background");
        let fg = self.rgba("text");
        let bg = if bg.a == 0 {
            if self.mode == Mode::Dark {
                Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }
            } else {
                Rgba {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                }
            }
        } else {
            bg
        };
        Style::default().fg(bg.tint(fg, self.thinking_opacity).color())
    }

    /// Derive every UI colour from a palette.
    pub fn from_palette(name: &str, mode: Mode, p: &Palette) -> Result<Theme, String> {
        let h = |v: &str| Rgba::hex(v).ok_or_else(|| format!("{name}: bad colour {v}"));
        let (bg, fg, muted, surface) = (h(&p.bg)?, h(&p.fg)?, h(&p.muted)?, h(&p.surface)?);
        let (primary, secondary, accent) = (h(&p.primary)?, h(&p.secondary)?, h(&p.accent)?);
        let (red, green, yellow, blue, magenta, cyan) = (
            h(&p.red)?,
            h(&p.green)?,
            h(&p.yellow)?,
            h(&p.blue)?,
            h(&p.magenta)?,
            h(&p.cyan)?,
        );
        Ok(Theme::derive(
            name,
            mode,
            DeriveInput {
                bg,
                fg,
                muted,
                surface,
                primary,
                secondary,
                accent,
                red,
                green,
                yellow,
                blue,
                magenta,
                cyan,
                transparent_bg: false,
            },
        ))
    }

    fn derive(name: &str, mode: Mode, i: DeriveInput) -> Theme {
        let DeriveInput {
            bg,
            fg,
            muted,
            surface,
            primary,
            secondary,
            accent,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            transparent_bg,
        } = i;
        let mut c: BTreeMap<String, Rgba> = BTreeMap::new();
        let mut set = |k: &str, v: Rgba| {
            c.insert(k.into(), v);
        };
        set("primary", primary);
        set("secondary", secondary);
        set("accent", accent);
        set("error", red);
        set("warning", yellow);
        set("success", green);
        set("info", cyan);
        set("text", fg);
        set("textMuted", muted);
        set("background", if transparent_bg { Rgba::TRANSPARENT } else { bg });
        set(
            "backgroundPanel",
            if transparent_bg {
                Rgba::TRANSPARENT
            } else {
                surface
            },
        );
        set("backgroundElement", bg.tint(fg, 0.10));
        set("backgroundMenu", surface);
        set("border", bg.tint(fg, 0.22));
        set("borderActive", primary);
        set("borderSubtle", bg.tint(fg, 0.12));
        set("diffAdded", green);
        set("diffRemoved", red);
        set("diffContext", muted);
        set("diffHunkHeader", muted);
        set("diffHighlightAdded", green);
        set("diffHighlightRemoved", red);
        set("diffAddedBg", bg.tint(green, 0.16));
        set("diffRemovedBg", bg.tint(red, 0.16));
        set("diffContextBg", bg.tint(fg, 0.04));
        set("diffLineNumber", muted);
        set("diffAddedLineNumberBg", bg.tint(green, 0.26));
        set("diffRemovedLineNumberBg", bg.tint(red, 0.26));
        set("markdownText", fg);
        set("markdownHeading", primary);
        set("markdownLink", cyan);
        set("markdownLinkText", cyan);
        set("markdownCode", accent);
        set("markdownBlockQuote", muted);
        set("markdownEmph", yellow);
        set("markdownStrong", fg);
        set("markdownHorizontalRule", muted);
        set("markdownListItem", primary);
        set("markdownListEnumeration", primary);
        set("markdownImage", cyan);
        set("markdownImageText", cyan);
        set("markdownCodeBlock", fg);
        set("syntaxComment", muted);
        set("syntaxKeyword", magenta);
        set("syntaxFunction", blue);
        set("syntaxVariable", fg);
        set("syntaxString", green);
        set("syntaxNumber", yellow);
        set("syntaxType", cyan);
        set("syntaxOperator", fg);
        set("syntaxPunctuation", muted);
        set("selectedListItemText", bg);
        Theme {
            name: name.into(),
            mode,
            colors: c,
            thinking_opacity: 0.6,
        }
    }

    /// The `system` theme: the terminal's own foreground/background and ANSI colours.
    pub fn system(mode: Mode, bg: Option<Rgba>, fg: Option<Rgba>) -> Theme {
        let dark = mode == Mode::Dark;
        let bg = bg.unwrap_or(if dark {
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }
        } else {
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }
        });
        let fg = fg.unwrap_or(if dark {
            Rgba {
                r: 229,
                g: 229,
                b: 229,
                a: 255,
            }
        } else {
            Rgba {
                r: 26,
                g: 26,
                b: 26,
                a: 255,
            }
        });
        let a = |i: u8| ansi_rgba(i);
        let (red, green, yellow, blue, magenta, cyan) = if dark {
            (a(9), a(10), a(11), a(12), a(13), a(14))
        } else {
            (a(1), a(2), a(3), a(4), a(5), a(6))
        };
        Theme::derive(
            "system",
            mode,
            DeriveInput {
                bg,
                fg,
                muted: bg.tint(fg, 0.55),
                surface: bg.tint(fg, 0.05),
                primary: cyan,
                secondary: magenta,
                accent: cyan,
                red,
                green,
                yellow,
                blue,
                magenta,
                cyan,
                transparent_bg: true,
            },
        )
    }
}

struct DeriveInput {
    bg: Rgba,
    fg: Rgba,
    muted: Rgba,
    surface: Rgba,
    primary: Rgba,
    secondary: Rgba,
    accent: Rgba,
    red: Rgba,
    green: Rgba,
    yellow: Rgba,
    blue: Rgba,
    magenta: Rgba,
    cyan: Rgba,
    transparent_bg: bool,
}

/// Names of every available theme (embedded + user), sorted.
pub fn available(extra_dirs: &[&Path]) -> Vec<String> {
    let mut names: Vec<String> = palettes().keys().cloned().collect();
    for dir in extra_dirs {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json")
                    && let Some(s) = p.file_stem().and_then(|s| s.to_str())
                {
                    names.push(s.to_string());
                }
            }
        }
    }
    names.push("system".into());
    names.sort();
    names.dedup();
    names
}

pub fn load(
    name: &str,
    mode: Mode,
    extra_dirs: &[&Path],
    system: (Option<Rgba>, Option<Rgba>),
) -> Result<Theme, String> {
    if name == "system" {
        return Ok(Theme::system(mode, system.0, system.1));
    }
    for dir in extra_dirs.iter().rev() {
        let p = dir.join(format!("{name}.json"));
        if let Ok(text) = std::fs::read_to_string(&p) {
            return Theme::parse(name, &text, mode);
        }
    }
    let set = palettes()
        .get(name)
        .ok_or_else(|| format!("theme {name} not found"))?;
    // a single-mode palette is used for both modes rather than failing
    let palette = match mode {
        Mode::Dark => set.dark.as_ref().or(set.light.as_ref()),
        Mode::Light => set.light.as_ref().or(set.dark.as_ref()),
    }
    .ok_or_else(|| format!("theme {name} has no palette"))?;
    Theme::from_palette(name, mode, palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_palettes_derive_full_themes() {
        for (name, set) in palettes() {
            for (mode, p) in [(Mode::Dark, set.dark.as_ref()), (Mode::Light, set.light.as_ref())] {
                let Some(p) = p else { continue };
                let t = Theme::from_palette(name, mode, p).unwrap_or_else(|e| panic!("{e}"));
                for k in KEYS {
                    assert!(t.colors.contains_key(*k), "{name} missing {k}");
                }
            }
        }
        assert!(palettes().contains_key(DEFAULT_THEME));
        assert!(available(&[]).contains(&"system".to_string()));
    }

    #[test]
    fn user_json_theme_format_still_parses() {
        let json = r##"{ "defs": { "blue": "#0000ff" }, "theme": { "primary": "blue", "text": { "dark": "#ffffff", "light": "#000000" }, "background": "transparent" } }"##;
        let t = Theme::parse("custom", json, Mode::Light).unwrap();
        assert_eq!(t.rgba("primary").b, 255);
        assert_eq!(t.rgba("text").r, 0);
    }

    #[test]
    fn hex_parse_and_ansi() {
        assert_eq!(
            Rgba::hex("#ff8000"),
            Some(Rgba {
                r: 255,
                g: 128,
                b: 0,
                a: 255
            })
        );
        assert_eq!(ansi_rgba(1).r, 0xcd);
        assert_eq!(
            ansi_rgba(16),
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
        assert_eq!(
            ansi_rgba(231),
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            }
        );
    }
}
