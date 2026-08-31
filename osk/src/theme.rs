//! The **theming foundation** — the token table every colour and every geometric
//! dimension in the draw path reads from, plus the [`ThemeSource`] seam that
//! decides where those tokens come from.
//!
//! # Why this module exists
//!
//! The Steam Deck OSK is themed by exactly one mechanism: a CSS class on the
//! keyboard root plus a table of ~53 custom properties — colours for every key
//! class and the trackpad pointer, and a handful of geometry hooks
//! (osk-technology.md §4.7). Nothing about the *layout* is themeable there;
//! only appearance. This module is the directly-copyable equivalent for a
//! native renderer: a flat [`Theme`] of colour + geometry + font tokens that
//! [`crate::render`] and [`crate::layout`] consume, with **no hardcoded colours
//! or sizes left in the draw path**.
//!
//! # The source seam
//!
//! [`ThemeSource`] is the abstraction that will let the OSK eventually inherit
//! the active **Omarchy** desktop theme without a rewrite. Today it has three
//! variants:
//!
//! * [`ThemeSource::BuiltIn`] — the sensible dark default ([`Theme::default`]).
//! * [`ThemeSource::TomlFile`] — load a user token file (default
//!   `~/.config/hyprpad-osk/theme.toml`), with **graceful fallback to the
//!   built-in default** on a missing or malformed file.
//! * [`ThemeSource::Omarchy`] — a **STUB** (see [`load_omarchy`]) that will map
//!   the active Omarchy palette onto these tokens. The parsing is intentionally
//!   not implemented yet; the seam is defined and the on-disk location is
//!   documented so it is a clean drop-in.

use std::path::{Path, PathBuf};

/// ARGB colour, non-premultiplied `0xAARRGGBB`. The renderer premultiplies on
/// write so translucent fills composite correctly on the layer surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Split into `(a, r, g, b)`, each `0..=255`.
    pub fn parts(self) -> (u32, u32, u32, u32) {
        let a = (self.0 >> 24) & 0xff;
        let r = (self.0 >> 16) & 0xff;
        let g = (self.0 >> 8) & 0xff;
        let b = self.0 & 0xff;
        (a, r, g, b)
    }
}

/// Every colour token the draw path can reference. Mirrors the meaningful
/// subset of the Deck's §4.7 custom-property table: a surface background, the
/// two key fills (character vs modifier/meta), border + label, the two
/// per-pad pointer/highlight colours, the d-pad focus colour, and the
/// pressed/committed accent.
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    /// Surface backdrop behind all keys (`--background-color`).
    pub surface_bg: Color,
    /// Character keycap fill (`--key-background-color`).
    pub key_fill: Color,
    /// Modifier / whitespace / meta keycap fill (`--key-*` variants).
    pub key_mod_fill: Color,
    /// Keycap border stroke.
    pub key_border: Color,
    /// Keycap legend / glyph colour (`--key-action-button-glyph-color`).
    pub key_label: Color,
    /// Left trackpad pointer + its hovered-key highlight
    /// (`--key-pointer-*` for the left pad).
    pub pointer_left: Color,
    /// Right trackpad pointer + its hovered-key highlight.
    pub pointer_right: Color,
    /// D-pad focus highlight (`.Focused` accent).
    pub focus: Color,
    /// Pressed / just-committed key accent (`.Touched`).
    pub pressed: Color,
    /// The contrasting outline/halo of the per-pad trackpad **cursor sprite**
    /// (the Deck's `--key-pointer-stroke-color`, §4.1). The sprite's *body* is
    /// the per-pad `pointer_*` colour; this stroke keeps it legible even when it
    /// sits over a same-hued hover highlight.
    pub cursor_stroke: Color,
    /// Active Shift / Caps indicator — the fill a Shift or Caps key takes while
    /// its state is engaged (the Deck's `.ShiftActive` / `.ToggleOn`, §4.6).
    pub shift_active: Color,
}

/// Geometry tokens. These are what make the surface **size-to-content**: the
/// panel dimensions are a pure function of these plus the key grid, computed by
/// [`crate::layout::LayoutEngine`] — never the screen size.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    /// Edge length of a 1-unit key, in px. Keys are laid out ~square: a 1-unit
    /// key is `key_size` x `key_size`. Comfortable default ~72 px.
    pub key_size: f32,
    /// Empty gap between adjacent keys, in px.
    pub gap: f32,
    /// Outer margin between the key cluster and the panel edge, in px.
    pub margin: f32,
    /// Corner rounding radius for keycaps, in px (consumed by the renderer's
    /// rounded-rect fill).
    pub corner: f32,
    /// Keycap border stroke width, in px.
    pub border_width: f32,
    /// Diameter, in px, of the per-pad trackpad cursor sprite (the Deck's
    /// ~30x30 pointer, §4.1). The renderer draws a filled disc of this size.
    pub cursor_size: f32,
}

/// Font tokens. The kickoff renderer uses an embedded 8x8 bitmap font, so the
/// only knob today is the maximum integer glyph scale; a real font family/size
/// lands with the GPU/pango path (osk-technology.md §8, deferred). Kept as a
/// struct so that growth does not churn the [`Theme`] shape.
#[derive(Clone, Copy, Debug)]
pub struct FontSpec {
    /// Largest integer glyph scale a single-char keycap may use.
    pub scale_max: i32,
}

/// The complete active theme: a name (the Deck's root class equivalent) plus
/// the colour, geometry, and font token tables.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Human-readable theme name (analogous to the Deck's root theme class).
    pub name: String,
    pub colors: Colors,
    pub geom: Geom,
    pub font: FontSpec,
}

impl Default for Theme {
    /// The built-in dark default. A restrained, legible palette with keys sized
    /// ~72 px square — the sensible starting point every other source falls
    /// back to.
    fn default() -> Theme {
        Theme {
            name: "hyprpad-dark".to_string(),
            colors: Colors {
                surface_bg: Color(0xE6_11_14_1A),   // translucent dark backdrop
                key_fill: Color(0xFF_23_28_33),     // character keycap
                key_mod_fill: Color(0xFF_2E_25_33), // modifier/meta tint
                key_border: Color(0xFF_3C_44_50),
                key_label: Color(0xFF_E6_EA_F0),
                pointer_left: Color(0xFF_2E_6C_C8),  // left pad (blue)
                pointer_right: Color(0xFF_C8_5A_2E), // right pad (orange)
                focus: Color(0xFF_3C_A0_5A),         // d-pad focus (green)
                pressed: Color(0xFF_D0_B0_50),       // committed accent (amber)
                cursor_stroke: Color(0xF0_F2_F4_F8), // near-white cursor halo
                shift_active: Color(0xFF_D8_5A_9E),  // shift/caps engaged (magenta)
            },
            geom: Geom {
                key_size: 72.0,
                gap: 8.0,
                margin: 14.0,
                corner: 8.0,
                border_width: 1.0,
                cursor_size: 30.0,
            },
            font: FontSpec { scale_max: 3 },
        }
    }
}

/// Where the active [`Theme`] is loaded from. The selection seam: the daemon /
/// CLI picks a source, [`ThemeSource::load`] resolves it to concrete tokens.
#[derive(Clone, Debug)]
pub enum ThemeSource {
    /// The compiled-in [`Theme::default`].
    BuiltIn,
    /// A user token file (see [`load_toml`]). Falls back to the default on any
    /// error.
    TomlFile(PathBuf),
    /// The active Omarchy desktop theme — **STUB** (see [`load_omarchy`]).
    Omarchy,
}

impl ThemeSource {
    /// The conventional path for [`ThemeSource::TomlFile`]:
    /// `~/.config/hyprpad-osk/theme.toml` (honours `$XDG_CONFIG_HOME`).
    pub fn default_toml_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        base.join("hyprpad-osk").join("theme.toml")
    }

    /// Resolve the source to a concrete [`Theme`]. Never fails: every fallible
    /// source degrades to [`Theme::default`] with a note on stderr, so the OSK
    /// always renders.
    pub fn load(&self) -> Theme {
        match self {
            ThemeSource::BuiltIn => Theme::default(),
            ThemeSource::TomlFile(path) => match load_toml(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!(
                        "hyprpad-osk: theme file {} not used ({e}); falling back to built-in default",
                        path.display()
                    );
                    Theme::default()
                }
            },
            ThemeSource::Omarchy => load_omarchy(),
        }
    }
}

// ---------------------------------------------------------------------------
// TOML token file (hand-parsed — no serde/toml dependency)
// ---------------------------------------------------------------------------

/// Load and apply a `theme.toml` on top of [`Theme::default`]. Every token is
/// optional — an absent key keeps the default — so a partial file is valid and
/// only overrides what it names.
///
/// The format is a tiny INI/TOML subset: `[section]` headers and `key = value`
/// lines, `#` line comments, blank lines ignored. Sections `colors`,
/// `geometry`, `font`. Colours are quoted hex strings (`"#RRGGBB"`,
/// `"#RRGGBBAA"`, or `"0xAARRGGBB"`); geometry/font values are plain numbers.
///
/// This is deliberately hand-parsed rather than pulling the `toml` + `serde`
/// stack: this crate's whole ethos is staying dependency-light and
/// C-dependency-free (font8x8 over pango, default SCTK features off), and the
/// token file is a flat handful of `key = value` lines that a ~40-line parser
/// covers completely with graceful per-line fallback.
pub fn load_toml(path: &Path) -> Result<Theme, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let mut theme = Theme::default();
    let mut section = String::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            eprintln!("hyprpad-osk: theme.toml:{}: not a key=value line, ignored", lineno + 1);
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = unquote(val.trim());
        apply_token(&mut theme, &section, &key, val, lineno + 1);
    }
    Ok(theme)
}

fn strip_comment(s: &str) -> &str {
    // Comments only outside quotes; the only quoted values here are colours,
    // whose `#` is inside the quotes, so a quote-aware scan suffices.
    let mut in_quote = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            '#' if !in_quote => return &s[..i],
            _ => {}
        }
    }
    s
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

fn apply_token(theme: &mut Theme, section: &str, key: &str, val: &str, lineno: usize) {
    let warn = || eprintln!("hyprpad-osk: theme.toml:{lineno}: bad value '{val}' for {section}.{key}, kept default");
    match (section, key) {
        ("colors", _) => {
            let Some(c) = parse_color(val) else { warn(); return };
            match key {
                "surface_bg" => theme.colors.surface_bg = c,
                "key_fill" => theme.colors.key_fill = c,
                "key_mod_fill" => theme.colors.key_mod_fill = c,
                "key_border" => theme.colors.key_border = c,
                "key_label" => theme.colors.key_label = c,
                "pointer_left" => theme.colors.pointer_left = c,
                "pointer_right" => theme.colors.pointer_right = c,
                "focus" => theme.colors.focus = c,
                "pressed" => theme.colors.pressed = c,
                "cursor_stroke" => theme.colors.cursor_stroke = c,
                "shift_active" => theme.colors.shift_active = c,
                _ => eprintln!("hyprpad-osk: theme.toml:{lineno}: unknown colors.{key}, ignored"),
            }
        }
        ("geometry", _) => {
            let Ok(n) = val.parse::<f32>() else { warn(); return };
            match key {
                "key_size" => theme.geom.key_size = n.max(8.0),
                "gap" => theme.geom.gap = n.max(0.0),
                "margin" => theme.geom.margin = n.max(0.0),
                "corner" => theme.geom.corner = n.max(0.0),
                "border_width" => theme.geom.border_width = n.max(0.0),
                "cursor_size" => theme.geom.cursor_size = n.max(4.0),
                _ => eprintln!("hyprpad-osk: theme.toml:{lineno}: unknown geometry.{key}, ignored"),
            }
        }
        ("font", "scale_max") => match val.parse::<i32>() {
            Ok(n) => theme.font.scale_max = n.clamp(1, 8),
            Err(_) => warn(),
        },
        ("", "name") | ("theme", "name") => theme.name = val.to_string(),
        _ => eprintln!("hyprpad-osk: theme.toml:{lineno}: unknown token {section}.{key}, ignored"),
    }
}

/// Parse a colour literal into ARGB `0xAARRGGBB`. Accepts `#RRGGBB`,
/// `#RRGGBBAA` (web order, alpha last), `0xAARRGGBB`, and `0xRRGGBB`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        match hex.len() {
            6 => {
                let rgb = u32::from_str_radix(hex, 16).ok()?;
                Some(Color(0xFF00_0000 | rgb))
            }
            8 => {
                // #RRGGBBAA -> 0xAARRGGBB
                let rgba = u32::from_str_radix(hex, 16).ok()?;
                let a = rgba & 0xff;
                let rgb = rgba >> 8;
                Some(Color((a << 24) | rgb))
            }
            _ => None,
        }
    } else if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        match hex.len() {
            6 => Some(Color(0xFF00_0000 | u32::from_str_radix(hex, 16).ok()?)),
            8 => Some(Color(u32::from_str_radix(hex, 16).ok()?)),
            _ => None,
        }
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Omarchy source — STUB (the clean drop-in seam)
// ---------------------------------------------------------------------------

/// **STUB.** Eventually maps the active **Omarchy** desktop theme's palette onto
/// [`Theme`] so the OSK matches the rest of the desktop. Today it logs the plan
/// and returns [`Theme::default`], so `--theme omarchy` is safe to select now.
///
/// # Where Omarchy's colours live (documented so this is a drop-in later)
///
/// Omarchy keeps the *currently selected* theme as a symlink:
///
/// ```text
/// ~/.config/omarchy/current/theme   ->   ~/.config/omarchy/themes/<name>/
/// ```
///
/// Inside that directory, `colors.toml` holds the palette as `#RRGGBB` hex
/// (exactly the format [`parse_color`] already accepts):
///
/// ```toml
/// accent      = "#D4A24C"
/// foreground  = "#D9CDB4"
/// background  = "#0B1418"
/// color0 = "#16232A"   # ... color1..color15 (the 16-colour terminal palette)
/// ```
///
/// # TODO — the mapping to implement here
///
/// Read `~/.config/omarchy/current/theme/colors.toml` (fall back to the default
/// on missing/unreadable), parse it with [`parse_color`], then map:
///
/// | Omarchy key            | [`Theme`] field                    |
/// |------------------------|------------------------------------|
/// | `background`           | `colors.surface_bg` (add ~0.9 alpha)|
/// | `color0`               | `colors.key_fill`                  |
/// | `color8` (bright black)| `colors.key_mod_fill`              |
/// | `foreground`           | `colors.key_label`                 |
/// | `color8` / `accent`    | `colors.key_border`                |
/// | `color4` (blue)        | `colors.pointer_left`              |
/// | `color3`/`color1`      | `colors.pointer_right`             |
/// | `accent`               | `colors.focus`                     |
/// | `accent` (brightened)  | `colors.pressed`                   |
///
/// Geometry/font tokens stay from the built-in default (Omarchy defines colours,
/// not key sizing) unless a user `theme.toml` overrides them. The parsing is
/// intentionally deferred; only the seam and the location are pinned down now.
pub fn load_omarchy() -> Theme {
    eprintln!(
        "hyprpad-osk: --theme omarchy is a stub; using built-in default. \
         TODO: map ~/.config/omarchy/current/theme/colors.toml onto the Theme tokens \
         (see theme::load_omarchy docs)."
    );
    Theme { name: "omarchy (stub -> default)".to_string(), ..Theme::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_forms() {
        assert_eq!(parse_color("#112233"), Some(Color(0xFF11_2233)));
        // web #RRGGBBAA, alpha last -> AARRGGBB
        assert_eq!(parse_color("#11223380"), Some(Color(0x8011_2233)));
        assert_eq!(parse_color("0xE6111418"), Some(Color(0xE611_1418)));
        assert_eq!(parse_color("0x112233"), Some(Color(0xFF11_2233)));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12"), None);
    }

    #[test]
    fn toml_overrides_only_named_tokens_and_keeps_defaults() {
        let dir = std::env::temp_dir().join(format!("hyprpad-osk-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.toml");
        std::fs::write(
            &path,
            "# a partial theme\n\
             name = \"unit-test\"\n\
             [colors]\n\
             key_fill = \"#010203\"  # inline comment\n\
             pointer_left = \"#0A0B0C0D\"\n\
             [geometry]\n\
             key_size = 96\n\
             gap = 10\n",
        )
        .unwrap();

        let t = load_toml(&path).unwrap();
        let def = Theme::default();
        assert_eq!(t.name, "unit-test");
        assert_eq!(t.colors.key_fill, Color(0xFF01_0203));
        assert_eq!(t.colors.pointer_left, Color(0x0D0A_0B0C));
        assert_eq!(t.geom.key_size, 96.0);
        assert_eq!(t.geom.gap, 10.0);
        // Untouched tokens keep the built-in default.
        assert_eq!(t.colors.key_label, def.colors.key_label);
        assert_eq!(t.geom.margin, def.geom.margin);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        let src = ThemeSource::TomlFile(PathBuf::from("/nonexistent/hyprpad-osk/theme.toml"));
        let t = src.load();
        assert_eq!(t.name, Theme::default().name);
    }
}
