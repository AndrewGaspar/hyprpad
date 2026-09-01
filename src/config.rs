//! Declarative gesture -> action bindings.
//!
//! A [`Config`] maps recognized [`gesture::GestureEvent`]s to [`Action`]s. It
//! is loaded from a small, flat TOML dialect whose headline table is
//! `[bindings]`: keys name a gesture (`"guide+r1"`, `"guide+stick_right"`) and
//! values are action strings (`"workspace +1"`, `"exec walker"`, `"fullscreen"`).
//!
//! The other sections are flat `key = value` tables of the same shape:
//!
//! | Section | What it configures |
//! |---|---|
//! | `[bindings]` | guide chords / stick flicks -> actions (the default section) |
//! | `[buttons]` | bare buttons -> raw keys (D-pad = arrows by default) |
//! | `[osk_buttons]` | buttons that type through the on-screen keyboard |
//! | `[daemon]` | daemon-wide switches (`own_lizard`) |
//! | `[cursor]` (alias `[damping]`) | trackpad-cursor gain + smoothing ([`CursorConfig`]) |
//! | `[scroll]` | left-pad scroll mode and feel ([`ScrollConfig`]) |
//! | `[haptics]` | pad-actuator feedback: which events buzz, and how hard ([`HapticsConfig`]) |
//!
//! [`DEFAULT_TOML`] carries the built-in defaults and doubles as living
//! documentation of every knob.
//!
//! **Why a hand-written parser instead of the `toml` crate.** The rest of the
//! crate is dependency-free (see Cargo.toml), and this schema is a flat table of
//! `string = string` — no nesting, arrays, or typed scalars. A ~40-line
//! line-based reader covers it exactly, keeps the dependency graph empty, and
//! keeps builds fast. If the schema ever grows structure, switch to `toml`.

use crate::gesture::{self, Stick, StickDir};
use crate::report;
use std::collections::HashMap;

/// Where a workspace action points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceTarget {
    /// Relative move, e.g. `+1` / `-1` from the current workspace.
    Relative(i32),
    /// A named (special) workspace.
    Named(String),
    /// An absolute workspace number.
    Number(i32),
}

impl WorkspaceTarget {
    /// Parse a target token: `+1`/`-2` -> [`Relative`](Self::Relative),
    /// a bare integer -> [`Number`](Self::Number), anything else ->
    /// [`Named`](Self::Named).
    pub fn parse(s: &str) -> Result<WorkspaceTarget, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("workspace needs a target".to_string());
        }
        if let Some(rest) = s.strip_prefix('+') {
            let n = rest
                .trim()
                .parse::<i32>()
                .map_err(|_| format!("bad relative workspace '{s}'"))?;
            return Ok(WorkspaceTarget::Relative(n));
        }
        if let Some(rest) = s.strip_prefix('-') {
            let n = rest
                .trim()
                .parse::<i32>()
                .map_err(|_| format!("bad relative workspace '{s}'"))?;
            return Ok(WorkspaceTarget::Relative(-n));
        }
        if let Ok(n) = s.parse::<i32>() {
            return Ok(WorkspaceTarget::Number(n));
        }
        Ok(WorkspaceTarget::Named(s.to_string()))
    }
}

/// An action the integration layer can carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Switch to a workspace.
    Workspace(WorkspaceTarget),
    /// Move the focused window to a workspace.
    MoveWindowToWorkspace(WorkspaceTarget),
    /// Toggle fullscreen on the focused window.
    ToggleFullscreen,
    /// Spawn a program (the launcher, a terminal, ...).
    Exec(String),
    /// A raw Hyprland dispatch payload; the integration layer decides delivery.
    Dispatch(String),
    /// Toggle the on-screen keyboard: show it (in `mode`) if hidden, hide it if
    /// shown. Driven by [`crate::osk::OskHandle`], not a Hyprland dispatch.
    ToggleKeyboard { mode: crate::osk::OskMode },
    /// Emit a raw evdev keycode (`KEY_*`). Bound to a *bare* controller button
    /// in the `[buttons]` section (e.g. D-pad up -> `KEY_UP`); pressed while the
    /// button is held and released when it lifts, so the kernel auto-repeats.
    /// Driven by [`crate::keyboard::VirtualKeyboard`], not a Hyprland dispatch.
    Key(u16),
    /// No action.
    None,
}

impl Action {
    /// Parse an action string such as `"workspace +1"` or `"exec walker"`.
    /// An empty string or `"none"` yields [`Action::None`].
    pub fn parse(s: &str) -> Result<Action, String> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("none") {
            return Ok(Action::None);
        }
        let (verb, rest) = match s.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim()),
            None => (s, ""),
        };
        match verb.to_ascii_lowercase().as_str() {
            "workspace" | "ws" => Ok(Action::Workspace(WorkspaceTarget::parse(rest)?)),
            "movetoworkspace" | "movewindow" => {
                Ok(Action::MoveWindowToWorkspace(WorkspaceTarget::parse(rest)?))
            }
            "fullscreen" => Ok(Action::ToggleFullscreen),
            "exec" => {
                if rest.is_empty() {
                    Err("exec needs a command".to_string())
                } else {
                    Ok(Action::Exec(rest.to_string()))
                }
            }
            "dispatch" => {
                if rest.is_empty() {
                    Err("dispatch needs a payload".to_string())
                } else {
                    Ok(Action::Dispatch(rest.to_string()))
                }
            }
            "keyboard" | "osk" => {
                let mode = match rest.to_ascii_lowercase().as_str() {
                    "" | "bottom" | "deck" => crate::osk::OskMode::Bottom,
                    "split" | "side" => crate::osk::OskMode::Split,
                    other => {
                        return Err(format!("unknown keyboard mode '{other}' (want bottom|split)"))
                    }
                };
                Ok(Action::ToggleKeyboard { mode })
            }
            "key" => {
                if rest.is_empty() {
                    Err("key needs a name".to_string())
                } else {
                    Ok(Action::Key(key_code(rest)?))
                }
            }
            other => Err(format!("unknown action '{other}'")),
        }
    }
}

/// The normalized binding key a gesture event resolves against.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum GestureKey {
    Chord(report::Button),
    Flick(Stick, StickDir),
    /// A bare guide tap (`GuideLeave { was_chorded: false }`).
    Tap,
    /// The guide crossing the hold threshold (`GuideHold`).
    Hold,
}

impl GestureKey {
    /// Parse a binding key such as `"guide+r1"`, `"guide+stick_right"`,
    /// `"guide+lstick_up"`, `"guide_tap"`, or `"guide_hold"`.
    fn parse(raw: &str) -> Result<GestureKey, String> {
        let k = raw.trim().to_ascii_lowercase();
        match k.as_str() {
            "guide" | "guide_tap" | "guide+tap" => return Ok(GestureKey::Tap),
            "guide_hold" | "guide+hold" => return Ok(GestureKey::Hold),
            _ => {}
        }
        let rest = k
            .strip_prefix("guide+")
            .ok_or_else(|| format!("binding key must start with 'guide+': '{raw}'"))?;
        // Stick flicks. `stick_` alone is an alias for the right stick, which is
        // the primary navigation stick in the vision.
        for (prefix, stick) in [
            ("rstick_", Stick::Right),
            ("right_stick_", Stick::Right),
            ("lstick_", Stick::Left),
            ("left_stick_", Stick::Left),
            ("stick_", Stick::Right),
        ] {
            if let Some(dir) = rest.strip_prefix(prefix) {
                return Ok(GestureKey::Flick(stick, parse_dir(dir)?));
            }
        }
        Ok(GestureKey::Chord(parse_button(rest)?))
    }
}

fn parse_dir(s: &str) -> Result<StickDir, String> {
    match s {
        "up" => Ok(StickDir::Up),
        "down" => Ok(StickDir::Down),
        "left" => Ok(StickDir::Left),
        "right" => Ok(StickDir::Right),
        other => Err(format!("unknown stick direction '{other}'")),
    }
}

fn parse_button(s: &str) -> Result<report::Button, String> {
    use report::Button::*;
    let b = match s {
        "a" => A,
        "b" => B,
        "x" => X,
        "y" => Y,
        "r1" => BumperR1,
        "l1" => BumperL1,
        "r2" | "r2_full" | "trigger_r2" => TriggerR2Full,
        "l2" | "l2_full" | "trigger_l2" => TriggerL2Full,
        "r3" => R3,
        "l3" => L3,
        "r4" => GripR4,
        "r5" => GripR5,
        "l4" => GripL4,
        "l5" => GripL5,
        "dpad_up" => DpadUp,
        "dpad_down" => DpadDown,
        "dpad_left" => DpadLeft,
        "dpad_right" => DpadRight,
        "menu" | "start" => Menu,
        "view" | "select" => View,
        "quickaccess" | "qam" => QuickAccess,
        "rpad_click" => PadRightClick,
        "lpad_click" => PadLeftClick,
        other => return Err(format!("unknown button '{other}'")),
    };
    Ok(b)
}

/// Map a key name (as used in a `[buttons]` binding's `key <name>` value) to
/// its raw evdev keycode (`input-event-codes.h`, the `KEY_*` constants).
///
/// Covers the arrow keys (the D-pad-to-arrows default) plus the common editing
/// and navigation keys, so bare buttons can be bound to more than the arrows
/// without extending this table. An unknown name is a reported error, never a
/// silent no-op. Names are matched case-insensitively by the caller.
fn key_code(name: &str) -> Result<u16, String> {
    let code = match name.trim().to_ascii_lowercase().as_str() {
        "up" => 103,        // KEY_UP
        "down" => 108,      // KEY_DOWN
        "left" => 105,      // KEY_LEFT
        "right" => 106,     // KEY_RIGHT
        "enter" | "return" => 28, // KEY_ENTER
        "backspace" => 14,  // KEY_BACKSPACE
        "space" => 57,      // KEY_SPACE
        "tab" => 15,        // KEY_TAB
        "escape" | "esc" => 1, // KEY_ESC
        "home" => 102,      // KEY_HOME
        "end" => 107,       // KEY_END
        "pageup" | "pgup" => 104, // KEY_PAGEUP
        "pagedown" | "pgdn" => 109, // KEY_PAGEDOWN
        other => return Err(format!("unknown key '{other}'")),
    };
    Ok(code)
}

/// Trackpad-cursor smoothing/damping knobs (the `[cursor]` — or its `[damping]`
/// alias — config section).
///
/// These configure [`crate::filter::PadDamper`], the per-pad One Euro Filter +
/// moving-center hysteresis + sub-pixel accumulation that keeps a held-still
/// finger from making the cursor swim (see `docs/research/pointer-damping.md`).
/// The same damper (and hence these same knobs) feeds *both* the desktop cursor
/// and the OSK on-screen cursors.
///
/// Defaults are the research doc's starting points (§4.1) — deliberately
/// *tunable*, not tuned: the pad's real noise floor should be measured
/// on-device and the margins sized to it.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorConfig {
    /// Desktop-cursor gain, in compositor pixels per pad count (the old
    /// `PAD_CURSOR_SENS`). Orthogonal to smoothing: it scales the *filtered*
    /// difference. Default `0.06`.
    pub sens: f64,
    /// One Euro `min_cutoff` (Hz): the cutoff floor at zero speed, i.e. how hard
    /// a still finger is smoothed. Lower = steadier at rest (more lag). Paper
    /// default `1.0`.
    pub one_euro_min_cutoff: f64,
    /// One Euro `beta`: how fast the cutoff opens up with speed, i.e. how little
    /// a fast flick lags. Higher = snappier flicks. Applied on the normalized
    /// `[-1, 1]` signal so the literature value transfers. Default `1.0`.
    pub one_euro_beta: f64,
    /// One Euro `d_cutoff` (Hz): fixed cutoff for the derivative low-pass. Paper
    /// default `1.0`.
    pub one_euro_d_cutoff: f64,
    /// Moving-center hysteresis margin, in normalized (`[-1, 1]`) units. Guards a
    /// *hard zero* for a truly motionless finger. `~2–4 × σ_noise` counts,
    /// normalized: default `0.0002` (≈ 6.5 counts out of ±32767). `0.0` disables.
    pub hysteresis: f64,
    /// Extra dead-band applied only to the desktop cursor's relative-delta path,
    /// on top of `hysteresis`, so fine desktop pointing can sit steadier than the
    /// OSK cursor. Normalized units. Default `0.0` (off).
    pub deadzone: f64,
}

impl Default for CursorConfig {
    fn default() -> CursorConfig {
        CursorConfig {
            sens: 0.06,
            one_euro_min_cutoff: 1.0,
            one_euro_beta: 1.0,
            one_euro_d_cutoff: 1.0,
            hysteresis: 0.0002,
            deadzone: 0.0,
        }
    }
}

/// Which left-trackpad scroll behaviour is active (the `[scroll] mode` knob).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMode {
    /// Left pad does not scroll (the right pad still drives the cursor).
    Off,
    /// Vertical (and optionally horizontal) finger movement maps to scroll:
    /// the smoothed left-pad position is differenced into a continuous scroll.
    Swipe,
    /// Steam-Deck-style radial scroll: the finger's angle around the pad centre
    /// is accumulated and emits one scroll tick every `circular_step_degrees`.
    Circular,
}

impl ScrollMode {
    /// Parse a `mode` value: `swipe`/`vertical`, `circular`/`radial`/`wheel`,
    /// or `off`/`none`/`disabled`.
    pub fn parse(s: &str) -> Result<ScrollMode, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Ok(ScrollMode::Off),
            "swipe" | "vertical" | "linear" => Ok(ScrollMode::Swipe),
            "circular" | "radial" | "wheel" => Ok(ScrollMode::Circular),
            other => Err(format!(
                "unknown scroll mode '{other}' (want swipe|circular|off)"
            )),
        }
    }
}

/// Left-trackpad scroll knobs (the `[scroll]` config section).
///
/// The LEFT pad drives scrolling on the ambient (desktop) layer while the RIGHT
/// pad keeps driving the cursor. Two modes are offered — a linear vertical
/// [`Swipe`](ScrollMode::Swipe) and a radial [`Circular`](ScrollMode::Circular)
/// (Steam-Deck-style). The left-pad position is smoothed by the same
/// [`crate::filter::PadDamper`] (One Euro + hysteresis) that steadies the
/// cursor — sharing the `[cursor]` smoothing knobs — before either mode reads
/// it, so scrolling is not jittery.
///
/// Like [`CursorConfig`], these defaults are deliberately *tunable starting
/// points*, not tuned values; the exact feel wants measuring on-device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollConfig {
    /// Which behaviour is active. Default [`ScrollMode::Circular`] — the radial
    /// scroll is the signature Steam-Deck feel and lets you scroll indefinitely
    /// with a continuous motion (no lift-and-repeat). Set `mode = swipe` for the
    /// more familiar linear swipe, or `mode = off` to disable.
    pub mode: ScrollMode,
    /// Scroll gain. Its unit depends on the mode (both sane near `1.0`):
    /// - swipe: scroll units per normalized unit of finger travel (a
    ///   centre-to-edge swipe is `1.0`), scaled by [`SWIPE_SCROLL_REF`].
    ///   [SWIPE_SCROLL_REF]: crate::run
    /// - circular: scroll units per *degree* of rotation, so each emitted tick
    ///   carries `sensitivity * circular_step_degrees` units (≈ one wheel notch
    ///   at the defaults). Default `1.0`.
    pub sensitivity: f64,
    /// Invert the scroll direction. Default `false` (traditional desktop-wheel
    /// direction; `true` gives the touch-screen "content follows the finger"
    /// feel). Applies to both modes.
    pub natural: bool,
    /// Swipe only: also map left/right finger motion to horizontal scroll.
    /// Default `false` (vertical scroll only, the least surprising behaviour).
    pub horizontal: bool,
    /// Circular only: degrees of rotation per emitted scroll tick. Smaller =
    /// finer/faster ticking. Default `15.0` (≈ the Deck's radial granularity).
    pub circular_step_degrees: f64,
    /// Circular only: minimum radius (normalized, pad edge ≈ `1.0` per axis) for
    /// the angle to count. Inside it the angle is ill-defined, so rotation is
    /// ignored — this is the dead centre. Default `0.35`.
    pub circular_min_radius: f64,
}

impl Default for ScrollConfig {
    fn default() -> ScrollConfig {
        ScrollConfig {
            mode: ScrollMode::Circular,
            sensitivity: 1.0,
            natural: false,
            horizontal: false,
            circular_step_degrees: 15.0,
            circular_min_radius: 0.35,
        }
    }
}

/// Haptic-feedback knobs (the `[haptics]` config section).
///
/// The puck has an actuator behind each trackpad ([`crate::haptics`]); firing a
/// short pulse on the pad a thumb is resting on is what makes the on-screen
/// keyboard feel physical. Each trigger point has its own toggle so the feel can
/// be dialled in one piece at a time, and `intensity` scales every pulse's width
/// (the only strength knob this device exposes).
///
/// All of these are read *per event* by the daemon, so `hyprpad reload` takes
/// effect on the next tick without a restart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HapticsConfig {
    /// Master switch. Default `true` — feedback is the point of the section, and
    /// a puck that can't be written to degrades to a silent no-op anyway.
    pub enabled: bool,
    /// Tick the pad whose OSK cursor crosses onto a **new** key (the Deck's
    /// signature keyboard feel). Reported by the OSK child over its stdout
    /// back-channel; suppressed when the cursor crosses onto a gap. Default
    /// `true`.
    pub crossing: bool,
    /// Click the pad that committed an OSK key (pad click, full trigger pull, or
    /// an `[osk_buttons]` helper). Default `true`.
    pub commit: bool,
    /// Buzz when a guide chord or stick flick resolves to an action. Fires on
    /// both actuators. Default `true`.
    pub gesture: bool,
    /// Tick the left pad on each emitted circular-scroll detent, so the radial
    /// scroll feels like a physical wheel. Default `true`.
    pub scroll: bool,
    /// Tick on a bare-button (`[buttons]`) key press — the press edge only,
    /// never the kernel's auto-repeat. Default **`false`**: the D-pad is held
    /// down for navigation and a buzz per arrow gets old fast.
    pub buttons: bool,
    /// Texture-tick the right pad as it drives the desktop cursor — one faint
    /// pulse per [`cursor_spacing_px`](Self::cursor_spacing_px) pixels of cursor
    /// travel, Steam Input's trackpad-friction feel. Default `true`.
    pub cursor: bool,
    /// Pixels of desktop-cursor travel per texture tick. Smaller = finer,
    /// busier texture. Default `64.0`.
    pub cursor_spacing_px: f64,
    /// Pulse-width scale, `1.0` = the kernel's calibrated widths. Clamped to a
    /// sane range by [`crate::haptics`]; `0` or below fires nothing (use
    /// `enabled = false` to switch off properly). Default `1.0`.
    pub intensity: f64,
}

impl Default for HapticsConfig {
    fn default() -> HapticsConfig {
        HapticsConfig {
            enabled: true,
            crossing: true,
            commit: true,
            gesture: true,
            scroll: true,
            buttons: false,
            cursor: true,
            cursor_spacing_px: 64.0,
            intensity: 1.0,
        }
    }
}

/// A set of gesture bindings.
#[derive(Clone, Debug, Default)]
pub struct Config {
    bindings: HashMap<GestureKey, Action>,
    /// Bare-button bindings (the `[buttons]` section): a controller button
    /// pressed *without* the guide modifier emits a raw evdev keycode. Distinct
    /// from `bindings`, which are guide chords. Maps the button to its `KEY_*`
    /// code; the default maps the D-pad to the arrow keys.
    buttons: HashMap<report::Button, u16>,
    /// OSK-helper buttons (the `[osk_buttons]` section): while the on-screen
    /// keyboard is up, these buttons send a raw key THROUGH the OSK (its uinput
    /// types it), Deck-style. Default: `Y` = Space, `X` = Backspace. Distinct
    /// from `buttons`, which fire only when the OSK is down.
    osk_buttons: HashMap<report::Button, u16>,
    /// Whether hyprpad should take ownership of the puck's lizard mode and keep
    /// the firmware keyboard/mouse emulation disabled ([`crate::lizard`]). Set
    /// via `own_lizard = true` in the `[daemon]` section. Default `false`, so we
    /// never fight an unmasked Steam that is managing lizard mode itself
    /// (docs/experiments/w12-device-denial.md).
    own_lizard: bool,
    /// Trackpad-cursor smoothing knobs (`[cursor]`/`[damping]` section).
    cursor: CursorConfig,
    /// Left-trackpad scroll knobs (`[scroll]` section).
    scroll: ScrollConfig,
    /// Haptic-feedback knobs (`[haptics]` section).
    haptics: HapticsConfig,
}

/// The built-in default bindings, in the config's own TOML dialect. Loaded by
/// [`Config::load_default`]; also exercises the parser round-trip.
pub const DEFAULT_TOML: &str = r#"
# hyprpad default gesture bindings (docs/08-living-room-vision.md).
[bindings]
"guide+r1" = "workspace +1"          # workspace right
"guide+l1" = "workspace -1"          # workspace left
"guide+stick_right" = "workspace +1" # right stick flicked right
"guide+stick_left"  = "workspace -1" # right stick flicked left
"guide+x" = "exec walker"            # launcher
"guide+y" = "keyboard"               # toggle the on-screen keyboard (bottom deck; configurable)

# Bare buttons: pressed WITHOUT the guide modifier, they emit a raw key. The
# D-pad acts as the arrow keys on the desktop (holding repeats, like a real
# keyboard). These are suppressed while the guide layer or on-screen keyboard is
# up, and in a focused game — there the D-pad reaches the game as a controller.
[buttons]
dpad_up = "key up"
dpad_down = "key down"
dpad_left = "key left"
dpad_right = "key right"

# OSK helper buttons: while the on-screen keyboard is up, these send a raw key
# THROUGH the OSK (its virtual keyboard types it), like the Steam Deck's
# keyboard chords. Y taps Space, X taps Backspace — no cursor hunting.
[osk_buttons]
y = "key space"
x = "key backspace"

# Left trackpad scrolls the desktop (ambient) layer; the right pad keeps driving
# the cursor. All knobs are tunable starting points.
[scroll]
mode = circular             # swipe | circular | off
sensitivity = 1.0           # circular: scroll units per degree; swipe: per normalized unit
natural = false             # invert scroll direction
horizontal = false          # swipe only: also map left/right finger motion to horizontal scroll
circular_step_degrees = 15  # circular only: rotation per emitted scroll tick
circular_min_radius = 0.35  # circular only: ignore rotation nearer than this to the pad centre

# Haptic feedback from the actuator behind each trackpad: a tick as an on-screen
# keyboard cursor crosses onto a new key, a click on commit, a buzz on a
# recognized gesture, a detent per circular-scroll tick. Every knob is read per
# event, so `hyprpad reload` retunes the feel live.
[haptics]
enabled = true              # master switch
crossing = true             # tick that pad when its OSK cursor crosses onto a new key
commit = true               # click that pad when it commits an OSK key
gesture = true              # buzz both pads when a guide chord/flick resolves
scroll = true               # tick the left pad on each circular-scroll detent
buttons = false             # tick on a bare-button ([buttons]) press edge
cursor = true               # texture-tick the right pad as it drives the desktop cursor
cursor_spacing_px = 64      # pixels of cursor travel per texture tick (smaller = finer)
intensity = 1.0             # pulse-width scale; 1.0 = the kernel's calibrated widths
"#;

impl Config {
    /// Parse a config from the TOML dialect described in the module docs.
    pub fn from_toml_str(s: &str) -> Result<Config, String> {
        let mut bindings = HashMap::new();
        let mut buttons = HashMap::new();
        let mut osk_buttons = HashMap::new();
        let mut own_lizard = false;
        let mut cursor = CursorConfig::default();
        let mut scroll = ScrollConfig::default();
        let mut haptics = HapticsConfig::default();
        let mut section = String::new();
        for (i, raw_line) in s.lines().enumerate() {
            let lineno = i + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(inner) = line.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                section = inner.trim().to_ascii_lowercase();
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {lineno}: expected 'key = value'"))?;
            match section.as_str() {
                // A leading, header-less block is treated as bindings, as before.
                "" | "bindings" => {
                    let key = GestureKey::parse(&unquote(k))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    let action = Action::parse(&unquote(v))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    bindings.insert(key, action);
                }
                // Bare-button bindings: a button pressed WITHOUT the guide
                // modifier emits a raw key. The key uses the same button-name
                // aliases as the guide chords (`dpad_up`, `a`, `r1`, ...); the
                // value must be a `key <name>` action.
                "buttons" => {
                    let button = parse_button(&unquote(k).trim().to_ascii_lowercase())
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    let action = Action::parse(&unquote(v))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    match action {
                        Action::Key(code) => {
                            buttons.insert(button, code);
                        }
                        _ => {
                            return Err(format!(
                                "line {lineno}: [buttons] values must be a 'key <name>' action"
                            ));
                        }
                    }
                }
                // OSK helper buttons: while the on-screen keyboard is up, the
                // button sends its key THROUGH the OSK (Deck-style Y=Space,
                // X=Backspace). Same button names and `key <name>` values.
                "osk_buttons" => {
                    let button = parse_button(&unquote(k).trim().to_ascii_lowercase())
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    let action = Action::parse(&unquote(v))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    match action {
                        Action::Key(code) => {
                            osk_buttons.insert(button, code);
                        }
                        _ => {
                            return Err(format!(
                                "line {lineno}: [osk_buttons] values must be a 'key <name>' action"
                            ));
                        }
                    }
                }
                // Daemon-wide settings (not gesture bindings).
                "daemon" => {
                    let key = unquote(k).to_ascii_lowercase();
                    match key.as_str() {
                        "own_lizard" => {
                            own_lizard = parse_bool(&unquote(v))
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        other => {
                            return Err(format!(
                                "line {lineno}: unknown [daemon] setting '{other}'"
                            ));
                        }
                    }
                }
                // Trackpad-cursor smoothing/damping knobs. `[damping]` is an
                // alias for `[cursor]`.
                "cursor" | "damping" => {
                    let key = unquote(k).to_ascii_lowercase();
                    let val = parse_f64(&unquote(v))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    match key.as_str() {
                        "sens" | "sensitivity" => cursor.sens = val,
                        "one_euro_min_cutoff" | "min_cutoff" => cursor.one_euro_min_cutoff = val,
                        "one_euro_beta" | "beta" => cursor.one_euro_beta = val,
                        "one_euro_d_cutoff" | "d_cutoff" => cursor.one_euro_d_cutoff = val,
                        "hysteresis" | "hysteresis_margin" => cursor.hysteresis = val,
                        "deadzone" | "dead_zone" => cursor.deadzone = val,
                        other => {
                            return Err(format!(
                                "line {lineno}: unknown [{section}] setting '{other}'"
                            ));
                        }
                    }
                }
                // Left-trackpad scroll knobs.
                "scroll" => {
                    let key = unquote(k).to_ascii_lowercase();
                    let val = unquote(v);
                    match key.as_str() {
                        "mode" => {
                            scroll.mode = ScrollMode::parse(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "sensitivity" | "sens" => {
                            scroll.sensitivity = parse_f64(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "natural" | "invert" => {
                            scroll.natural = parse_bool(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "horizontal" | "swipe_horizontal" => {
                            scroll.horizontal = parse_bool(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "circular_step_degrees" | "step_degrees" | "step" => {
                            scroll.circular_step_degrees = parse_f64(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "circular_min_radius" | "min_radius" => {
                            scroll.circular_min_radius = parse_f64(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        other => {
                            return Err(format!(
                                "line {lineno}: unknown [scroll] setting '{other}'"
                            ));
                        }
                    }
                }
                // Haptic-feedback knobs: a master switch, one toggle per trigger
                // point, and the pulse-width scale.
                "haptics" => {
                    let key = unquote(k).to_ascii_lowercase();
                    let val = unquote(v);
                    let flag = |slot: &mut bool| -> Result<(), String> {
                        *slot = parse_bool(&val).map_err(|e| format!("line {lineno}: {e}"))?;
                        Ok(())
                    };
                    match key.as_str() {
                        "enabled" | "enable" | "on" => flag(&mut haptics.enabled)?,
                        "crossing" | "key_crossing" | "crossings" => flag(&mut haptics.crossing)?,
                        "commit" | "commits" => flag(&mut haptics.commit)?,
                        "gesture" | "gestures" => flag(&mut haptics.gesture)?,
                        "scroll" | "scroll_ticks" => flag(&mut haptics.scroll)?,
                        "buttons" | "bare_buttons" => flag(&mut haptics.buttons)?,
                        "cursor" | "cursor_texture" => flag(&mut haptics.cursor)?,
                        "cursor_spacing_px" | "cursor_spacing" => {
                            haptics.cursor_spacing_px = parse_f64(&unquote(v))
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "intensity" | "strength" | "gain" => {
                            haptics.intensity = parse_f64(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        other => {
                            return Err(format!(
                                "line {lineno}: unknown [haptics] setting '{other}'"
                            ));
                        }
                    }
                }
                _ => return Err(format!("line {lineno}: unknown section [{section}]")),
            }
        }
        Ok(Config { bindings, buttons, osk_buttons, own_lizard, cursor, scroll, haptics })
    }

    /// The built-in defaults encoding the vision's core gestures.
    pub fn load_default() -> Config {
        Config::from_toml_str(DEFAULT_TOML).expect("built-in default config is valid")
    }

    /// The path the user config is read from:
    /// `$XDG_CONFIG_HOME/hyprpad/config.toml`, or `~/.config/hyprpad/config.toml`
    /// when `XDG_CONFIG_HOME` is unset. Returns `None` only if neither
    /// `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_path() -> Option<std::path::PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(dir).join("hyprpad/config.toml"));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(std::path::PathBuf::from(home).join(".config/hyprpad/config.toml"))
    }

    /// Load the user config from [`config_path`](Self::config_path), falling
    /// back to [`load_default`](Self::load_default) when the file is absent
    /// (or when no config directory can be resolved).
    ///
    /// Returns an error only when the file is *present but malformed*, so the
    /// caller can surface a real misconfiguration rather than silently ignoring
    /// it.
    pub fn load() -> Result<Config, String> {
        let Some(path) = Config::config_path() else {
            return Ok(Config::load_default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::from_toml_str(&text)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::load_default()),
            Err(e) => Err(format!("reading {}: {e}", path.display())),
        }
    }

    /// Resolve a gesture event to its bound action, or [`Action::None`].
    ///
    /// Lifecycle events that carry no binding — `GuideEnter` and a *chorded*
    /// `GuideLeave` — always resolve to `None`. A bare `GuideLeave` maps to the
    /// optional `guide_tap` binding, and `GuideHold` to `guide_hold`.
    pub fn resolve(&self, ev: &gesture::GestureEvent) -> Action {
        use gesture::GestureEvent as E;
        let key = match ev {
            E::GuideChord(b) => GestureKey::Chord(*b),
            E::GuideStickFlick { stick, dir } => GestureKey::Flick(*stick, *dir),
            E::GuideLeave { was_chorded: false } => GestureKey::Tap,
            E::GuideHold => GestureKey::Hold,
            E::GuideEnter | E::GuideLeave { was_chorded: true } => return Action::None,
        };
        self.bindings.get(&key).cloned().unwrap_or(Action::None)
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether there are no bindings.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The bare-button bindings (`[buttons]` section): each controller button
    /// pressed *without* the guide modifier maps to a raw evdev keycode. The
    /// default maps the D-pad to the arrow keys.
    pub fn buttons(&self) -> &HashMap<report::Button, u16> {
        &self.buttons
    }

    /// The OSK helper buttons (`[osk_buttons]` section): while the on-screen
    /// keyboard is up, each of these taps its key through the OSK's virtual
    /// keyboard (Deck-style; default `Y` = Space, `X` = Backspace).
    pub fn osk_buttons(&self) -> &HashMap<report::Button, u16> {
        &self.osk_buttons
    }

    /// Whether hyprpad should take ownership of the puck's lizard mode
    /// ([`crate::lizard`]). Configured by `own_lizard` in the `[daemon]`
    /// section; default `false`.
    pub fn own_lizard(&self) -> bool {
        self.own_lizard
    }

    /// The trackpad-cursor smoothing/damping knobs (`[cursor]`/`[damping]`
    /// section); defaults from [`CursorConfig::default`].
    pub fn cursor(&self) -> &CursorConfig {
        &self.cursor
    }

    /// The left-trackpad scroll knobs (`[scroll]` section); defaults from
    /// [`ScrollConfig::default`].
    pub fn scroll(&self) -> &ScrollConfig {
        &self.scroll
    }

    /// The haptic-feedback knobs (`[haptics]` section); defaults from
    /// [`HapticsConfig::default`]. Read per event by the daemon so a reload
    /// retunes the feel live.
    pub fn haptics(&self) -> &HapticsConfig {
        &self.haptics
    }
}

/// Parse a floating-point config value, rejecting non-finite results so a
/// bad knob is a reported error rather than a NaN/inf that silently breaks the
/// filter.
fn parse_f64(s: &str) -> Result<f64, String> {
    let t = s.trim();
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(format!("expected a number, got '{t}'")),
    }
}

/// Parse a boolean config value: `true`/`false`, `1`/`0`, `yes`/`no`,
/// `on`/`off` (case-insensitive).
fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!("expected a boolean (true/false), got '{other}'")),
    }
}

/// Truncate a line at the first `#` that is not inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut quote = '"';
    for (i, c) in line.char_indices() {
        if in_str {
            if c == quote {
                in_str = false;
            }
        } else if c == '"' || c == '\'' {
            in_str = true;
            quote = c;
        } else if c == '#' {
            return &line[..i];
        }
    }
    line
}

/// Strip a single pair of matching surrounding quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    if s.len() >= 2 {
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::GestureEvent;
    use crate::report::Button;

    #[test]
    fn defaults_resolve_to_vision_actions() {
        let c = Config::load_default();
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperL1)),
            Action::Workspace(WorkspaceTarget::Relative(-1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Right,
            }),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Left,
            }),
            Action::Workspace(WorkspaceTarget::Relative(-1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::Exec("walker".to_string())
        );
    }

    #[test]
    fn default_binds_keyboard_toggle() {
        use crate::osk::OskMode;
        let c = Config::load_default();
        // The default keyboard chord is guide+y -> toggle the bottom deck.
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::ToggleKeyboard { mode: OskMode::Bottom }
        );
    }

    #[test]
    fn parses_keyboard_action_modes() {
        use crate::osk::OskMode;
        let toml = r#"
[bindings]
"guide+y" = "keyboard"
"guide+a" = "keyboard bottom"
"guide+b" = "keyboard split"
"guide+x" = "osk split"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::ToggleKeyboard { mode: OskMode::Bottom }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleKeyboard { mode: OskMode::Bottom }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::B)),
            Action::ToggleKeyboard { mode: OskMode::Split }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::ToggleKeyboard { mode: OskMode::Split }
        );
        // An unknown mode is a reported error, not a silent default.
        assert!(Config::from_toml_str("[bindings]\n\"guide+y\" = \"keyboard sideways\"\n")
            .unwrap_err()
            .contains("unknown keyboard mode"));
    }

    #[test]
    fn unbound_events_resolve_to_none() {
        let c = Config::load_default();
        // Unbound chord (A carries no default binding).
        assert_eq!(c.resolve(&GestureEvent::GuideChord(Button::A)), Action::None);
        // Left stick is unbound in defaults.
        assert_eq!(
            c.resolve(&GestureEvent::GuideStickFlick {
                stick: Stick::Left,
                dir: StickDir::Right,
            }),
            Action::None
        );
        // Lifecycle events.
        assert_eq!(c.resolve(&GestureEvent::GuideEnter), Action::None);
        assert_eq!(
            c.resolve(&GestureEvent::GuideLeave { was_chorded: false }),
            Action::None
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideLeave { was_chorded: true }),
            Action::None
        );
    }

    #[test]
    fn parses_all_action_kinds() {
        let toml = r#"
[bindings]
"guide+r1" = "workspace +2"
"guide+l1" = "workspace -3"
"guide+a" = "workspace 5"
"guide+b" = "workspace steam"
"guide+y" = "movetoworkspace +1"
"guide+x" = "exec walker --theme dark"
"guide+menu" = "fullscreen"
"guide+dpad_up" = "dispatch togglespecialworkspace magic"
"guide+dpad_down" = "none"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(2))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperL1)),
            Action::Workspace(WorkspaceTarget::Relative(-3))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::Workspace(WorkspaceTarget::Number(5))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::B)),
            Action::Workspace(WorkspaceTarget::Named("steam".to_string()))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::Exec("walker --theme dark".to_string())
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Menu)),
            Action::ToggleFullscreen
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::DpadUp)),
            Action::Dispatch("togglespecialworkspace magic".to_string())
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::DpadDown)),
            Action::None
        );
    }

    #[test]
    fn parses_stick_and_tap_and_hold_keys() {
        let toml = r#"
[bindings]
"guide+lstick_up" = "fullscreen"
"guide+rstick_down" = "workspace +1"
"guide_tap" = "exec steam-bigpicture"
"guide_hold" = "dispatch overlay"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideStickFlick {
                stick: Stick::Left,
                dir: StickDir::Up,
            }),
            Action::ToggleFullscreen
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Down,
            }),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideLeave { was_chorded: false }),
            Action::Exec("steam-bigpicture".to_string())
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideHold),
            Action::Dispatch("overlay".to_string())
        );
    }

    #[test]
    fn comments_blank_lines_and_inline_comments_ignored() {
        let toml = r#"
# a leading comment
[bindings]

"guide+r1" = "workspace +1"   # inline comment after value
# "guide+l1" = "workspace -1" (commented out entirely)
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(c.len(), 1);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        // The commented-out binding did not register.
        assert_eq!(c.resolve(&GestureEvent::GuideChord(Button::BumperL1)), Action::None);
    }

    #[test]
    fn hash_inside_quotes_is_not_a_comment() {
        let toml = r#"
[bindings]
"guide+x" = "exec echo #tag"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::Exec("echo #tag".to_string())
        );
    }

    #[test]
    fn bare_unquoted_keys_and_values_work() {
        // Quotes are optional in this dialect.
        let toml = "[bindings]\nguide+r1 = workspace +1\n";
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
    }

    #[test]
    fn errors_are_reported_with_line_numbers() {
        assert!(Config::from_toml_str("[bindings]\n\"guide+r1\" = \"teleport 3\"\n")
            .unwrap_err()
            .contains("unknown action"));
        assert!(Config::from_toml_str("[bindings]\n\"guide+nope\" = \"fullscreen\"\n")
            .unwrap_err()
            .contains("unknown button"));
        assert!(Config::from_toml_str("[bindings]\n\"jump+a\" = \"fullscreen\"\n")
            .unwrap_err()
            .contains("must start with 'guide+'"));
        assert!(Config::from_toml_str("[other]\n\"guide+a\" = \"fullscreen\"\n")
            .unwrap_err()
            .contains("unknown section"));
        assert!(Config::from_toml_str("[bindings]\nthis line has no equals\n")
            .unwrap_err()
            .contains("expected 'key = value'"));
        assert!(Config::from_toml_str("[bindings]\n\"guide+stick_up\" = \"workspace\"\n")
            .unwrap_err()
            .contains("workspace needs a target"));
    }

    #[test]
    fn workspace_target_parse_cases() {
        assert_eq!(
            WorkspaceTarget::parse("+1").unwrap(),
            WorkspaceTarget::Relative(1)
        );
        assert_eq!(
            WorkspaceTarget::parse("-4").unwrap(),
            WorkspaceTarget::Relative(-4)
        );
        assert_eq!(
            WorkspaceTarget::parse("7").unwrap(),
            WorkspaceTarget::Number(7)
        );
        assert_eq!(
            WorkspaceTarget::parse("gaming").unwrap(),
            WorkspaceTarget::Named("gaming".to_string())
        );
        assert!(WorkspaceTarget::parse("+notanumber").is_err());
    }

    #[test]
    fn default_config_is_nonempty() {
        assert!(!Config::load_default().is_empty());
    }

    #[test]
    fn key_name_table_maps_arrows_and_extras() {
        // The arrows, which the default D-pad binding uses.
        assert_eq!(key_code("up"), Ok(103));
        assert_eq!(key_code("down"), Ok(108));
        assert_eq!(key_code("left"), Ok(105));
        assert_eq!(key_code("right"), Ok(106));
        // A few of the editing/nav extras and their aliases; case-insensitive.
        assert_eq!(key_code("enter"), Ok(28));
        assert_eq!(key_code("return"), Ok(28));
        assert_eq!(key_code("Backspace"), Ok(14));
        assert_eq!(key_code("space"), Ok(57));
        assert_eq!(key_code("tab"), Ok(15));
        assert_eq!(key_code("esc"), Ok(1));
        assert_eq!(key_code("escape"), Ok(1));
        assert_eq!(key_code("pageup"), Ok(104));
        assert_eq!(key_code("pgdn"), Ok(109));
        // Every arrow/nav code stays inside the range keyboard.rs registers.
        for name in ["up", "down", "left", "right", "home", "end", "pageup", "pagedown"] {
            assert!(key_code(name).unwrap() <= 127);
        }
        // An unknown name is a reported error, not a silent default.
        assert!(key_code("f13").unwrap_err().contains("unknown key"));
    }

    #[test]
    fn key_action_parses_and_reports_unknown() {
        assert_eq!(Action::parse("key up").unwrap(), Action::Key(103));
        assert_eq!(Action::parse("key ESC").unwrap(), Action::Key(1));
        assert!(Action::parse("key").unwrap_err().contains("key needs a name"));
        assert!(Action::parse("key nope").unwrap_err().contains("unknown key"));
    }

    #[test]
    fn default_buttons_map_dpad_to_arrows() {
        let c = Config::load_default();
        let b = c.buttons();
        assert_eq!(b.get(&Button::DpadUp), Some(&103));
        assert_eq!(b.get(&Button::DpadDown), Some(&108));
        assert_eq!(b.get(&Button::DpadLeft), Some(&105));
        assert_eq!(b.get(&Button::DpadRight), Some(&106));
        // Only the four D-pad buttons are bound by default.
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn buttons_section_parses_aliases_and_reports_errors() {
        // Uses the same button-name aliases as the guide chords, plus a
        // non-D-pad button, and the value must be a `key <name>` action.
        let toml = r#"
[buttons]
dpad_up = "key up"
a = "key enter"
r1 = "key pageup"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(c.buttons().get(&Button::DpadUp), Some(&103));
        assert_eq!(c.buttons().get(&Button::A), Some(&28));
        assert_eq!(c.buttons().get(&Button::BumperR1), Some(&104));
        // Bindings and buttons are independent sections.
        assert!(c.is_empty());

        // An unknown button name is reported.
        assert!(Config::from_toml_str("[buttons]\nnope = \"key up\"\n")
            .unwrap_err()
            .contains("unknown button"));
        // An unknown key name is reported.
        assert!(Config::from_toml_str("[buttons]\ndpad_up = \"key sideways\"\n")
            .unwrap_err()
            .contains("unknown key"));
        // A non-key action in [buttons] is rejected.
        assert!(Config::from_toml_str("[buttons]\ndpad_up = \"fullscreen\"\n")
            .unwrap_err()
            .contains("must be a 'key <name>' action"));
    }

    #[test]
    fn own_lizard_defaults_off_and_parses() {
        // Default (and built-in default config) leaves ownership off.
        assert!(!Config::load_default().own_lizard());
        assert!(!Config::from_toml_str("[bindings]\n\"guide+a\" = \"fullscreen\"\n")
            .unwrap()
            .own_lizard());

        // Explicit enable in the [daemon] section, bindings still parse.
        let c = Config::from_toml_str(
            "[daemon]\nown_lizard = true\n[bindings]\n\"guide+a\" = \"fullscreen\"\n",
        )
        .unwrap();
        assert!(c.own_lizard());
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleFullscreen
        );

        // Accepted spellings and explicit false.
        assert!(Config::from_toml_str("[daemon]\nown_lizard = on\n").unwrap().own_lizard());
        assert!(Config::from_toml_str("[daemon]\nown_lizard = 1\n").unwrap().own_lizard());
        assert!(!Config::from_toml_str("[daemon]\nown_lizard = false\n").unwrap().own_lizard());

        // Bad value and unknown setting are reported, not silently ignored.
        assert!(Config::from_toml_str("[daemon]\nown_lizard = maybe\n")
            .unwrap_err()
            .contains("boolean"));
        assert!(Config::from_toml_str("[daemon]\nnope = true\n")
            .unwrap_err()
            .contains("unknown [daemon] setting"));
    }

    #[test]
    fn cursor_damping_defaults_and_parse() {
        // Defaults match the research doc's starting points.
        let d = Config::load_default();
        assert_eq!(d.cursor(), &CursorConfig::default());
        assert_eq!(d.cursor().sens, 0.06);
        assert_eq!(d.cursor().one_euro_min_cutoff, 1.0);
        assert_eq!(d.cursor().one_euro_beta, 1.0);
        assert_eq!(d.cursor().one_euro_d_cutoff, 1.0);
        assert_eq!(d.cursor().deadzone, 0.0);

        // A `[cursor]` section overrides individual knobs; bindings still parse.
        let c = Config::from_toml_str(
            r#"
[cursor]
sens = 0.09
one_euro_min_cutoff = 0.5
one_euro_beta = 2.0
one_euro_d_cutoff = 1.5
hysteresis = 0.0005
deadzone = 0.001

[bindings]
"guide+a" = "fullscreen"
"#,
        )
        .expect("parse");
        assert_eq!(c.cursor().sens, 0.09);
        assert_eq!(c.cursor().one_euro_min_cutoff, 0.5);
        assert_eq!(c.cursor().one_euro_beta, 2.0);
        assert_eq!(c.cursor().one_euro_d_cutoff, 1.5);
        assert_eq!(c.cursor().hysteresis, 0.0005);
        assert_eq!(c.cursor().deadzone, 0.001);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleFullscreen
        );

        // `[damping]` is an accepted alias, with the shorter key spellings.
        let c = Config::from_toml_str("[damping]\nbeta = 3.0\nmin_cutoff = 0.8\n").unwrap();
        assert_eq!(c.cursor().one_euro_beta, 3.0);
        assert_eq!(c.cursor().one_euro_min_cutoff, 0.8);
        // Unspecified knobs keep their defaults.
        assert_eq!(c.cursor().sens, CursorConfig::default().sens);

        // A bad value and an unknown key are reported, not silently ignored.
        assert!(Config::from_toml_str("[cursor]\nsens = fast\n")
            .unwrap_err()
            .contains("expected a number"));
        assert!(Config::from_toml_str("[cursor]\nwiggle = 1.0\n")
            .unwrap_err()
            .contains("unknown [cursor] setting"));
    }

    #[test]
    fn scroll_defaults_and_parse() {
        // The built-in default config reproduces ScrollConfig::default exactly
        // (its `[scroll]` block is living documentation of the defaults).
        let d = Config::load_default();
        assert_eq!(d.scroll(), &ScrollConfig::default());
        assert_eq!(d.scroll().mode, ScrollMode::Circular);
        assert_eq!(d.scroll().sensitivity, 1.0);
        assert!(!d.scroll().natural);
        assert!(!d.scroll().horizontal);
        assert_eq!(d.scroll().circular_step_degrees, 15.0);
        assert_eq!(d.scroll().circular_min_radius, 0.35);

        // A `[scroll]` section overrides individual knobs; bindings still parse.
        let c = Config::from_toml_str(
            r#"
[scroll]
mode = swipe
sensitivity = 0.5
natural = true
horizontal = true
circular_step_degrees = 20
circular_min_radius = 0.25

[bindings]
"guide+a" = "fullscreen"
"#,
        )
        .expect("parse");
        assert_eq!(c.scroll().mode, ScrollMode::Swipe);
        assert_eq!(c.scroll().sensitivity, 0.5);
        assert!(c.scroll().natural);
        assert!(c.scroll().horizontal);
        assert_eq!(c.scroll().circular_step_degrees, 20.0);
        assert_eq!(c.scroll().circular_min_radius, 0.25);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleFullscreen
        );

        // Mode spellings and `off`; shorter key aliases; unspecified knobs keep
        // their defaults.
        assert_eq!(
            Config::from_toml_str("[scroll]\nmode = off\n").unwrap().scroll().mode,
            ScrollMode::Off
        );
        assert_eq!(
            Config::from_toml_str("[scroll]\nmode = radial\n").unwrap().scroll().mode,
            ScrollMode::Circular
        );
        let c = Config::from_toml_str("[scroll]\nsens = 2.0\nstep = 10\nmin_radius = 0.4\n").unwrap();
        assert_eq!(c.scroll().sensitivity, 2.0);
        assert_eq!(c.scroll().circular_step_degrees, 10.0);
        assert_eq!(c.scroll().circular_min_radius, 0.4);
        assert_eq!(c.scroll().mode, ScrollConfig::default().mode);

        // Bad values and unknown keys/modes are reported, not silently ignored.
        assert!(Config::from_toml_str("[scroll]\nmode = sideways\n")
            .unwrap_err()
            .contains("unknown scroll mode"));
        assert!(Config::from_toml_str("[scroll]\nsensitivity = fast\n")
            .unwrap_err()
            .contains("expected a number"));
        assert!(Config::from_toml_str("[scroll]\nnatural = maybe\n")
            .unwrap_err()
            .contains("boolean"));
        assert!(Config::from_toml_str("[scroll]\nwiggle = 1.0\n")
            .unwrap_err()
            .contains("unknown [scroll] setting"));
    }

    #[test]
    fn haptics_defaults_and_parse() {
        // The built-in default config reproduces HapticsConfig::default exactly
        // (its `[haptics]` block is living documentation of the defaults).
        let d = Config::load_default();
        assert_eq!(d.haptics(), &HapticsConfig::default());
        // Feedback is on by default — it is the point of the section — except
        // the bare-button tick, which would buzz on every held arrow.
        assert!(d.haptics().enabled);
        assert!(d.haptics().crossing);
        assert!(d.haptics().commit);
        assert!(d.haptics().gesture);
        assert!(d.haptics().scroll);
        assert!(!d.haptics().buttons);
        assert_eq!(d.haptics().intensity, 1.0);

        // A `[haptics]` section overrides individual knobs; bindings still parse.
        let c = Config::from_toml_str(
            r#"
[haptics]
enabled = true
crossing = false
commit = false
gesture = false
scroll = false
buttons = true
intensity = 0.5

[bindings]
"guide+a" = "fullscreen"
"#,
        )
        .expect("parse");
        assert!(c.haptics().enabled);
        assert!(!c.haptics().crossing);
        assert!(!c.haptics().commit);
        assert!(!c.haptics().gesture);
        assert!(!c.haptics().scroll);
        assert!(c.haptics().buttons);
        assert_eq!(c.haptics().intensity, 0.5);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleFullscreen
        );

        // The master switch and the key aliases; unspecified knobs keep their
        // defaults.
        assert!(!Config::from_toml_str("[haptics]\nenabled = off\n").unwrap().haptics().enabled);
        let c = Config::from_toml_str("[haptics]\nstrength = 2.0\nbare_buttons = yes\n").unwrap();
        assert_eq!(c.haptics().intensity, 2.0);
        assert!(c.haptics().buttons);
        assert!(c.haptics().crossing); // untouched knobs keep the default

        // Bad values and unknown keys are reported, not silently ignored.
        assert!(Config::from_toml_str("[haptics]\nenabled = maybe\n")
            .unwrap_err()
            .contains("boolean"));
        assert!(Config::from_toml_str("[haptics]\nintensity = strong\n")
            .unwrap_err()
            .contains("expected a number"));
        assert!(Config::from_toml_str("[haptics]\nwiggle = true\n")
            .unwrap_err()
            .contains("unknown [haptics] setting"));
    }

    #[test]
    fn load_reads_present_file_and_reports_malformed() {
        // Point XDG_CONFIG_HOME at a unique temp dir so `load()` reads our
        // file. No other test touches these vars, so the process-wide mutation
        // is safe here. Save/restore to leave the environment as we found it.
        let saved_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let dir = std::env::temp_dir().join(format!("hyprpad-cfg-test-{}", std::process::id()));
        let cfg_dir = dir.join("hyprpad");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        // Absent file -> defaults, no error.
        let _ = std::fs::remove_file(cfg_dir.join("config.toml"));
        assert_eq!(Config::load().unwrap().len(), Config::load_default().len());

        // Present, valid file -> parsed.
        std::fs::write(cfg_dir.join("config.toml"), "[bindings]\n\"guide+a\" = \"workspace 3\"\n")
            .unwrap();
        let c = Config::load().expect("valid file loads");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::Workspace(WorkspaceTarget::Number(3))
        );

        // Present, malformed file -> error (not a silent fallback).
        std::fs::write(cfg_dir.join("config.toml"), "[bindings]\n\"guide+nope\" = \"fullscreen\"\n")
            .unwrap();
        assert!(Config::load().unwrap_err().contains("unknown button"));

        // Restore environment and clean up.
        match saved_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
