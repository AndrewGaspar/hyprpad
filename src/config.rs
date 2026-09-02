//! Declarative gesture -> action bindings.
//!
//! A [`Config`] maps recognized [`gesture::GestureEvent`]s to [`Action`]s. It
//! is loaded from a small, flat TOML dialect whose headline table is
//! `[bindings]`: keys name a gesture (`"guide+r1"`, `"guide+stick_right"`) and
//! values are action strings (`"workspace +1"`, `"exec walker"`, `"fullscreen"`).
//!
//! ## Two front-ends, one `Config`
//!
//! [`Config::load`] reads whichever of the two config files exists:
//!
//! | file | front-end |
//! |---|---|
//! | `~/.config/hyprpad/config.lua` | the Lua front-end ([`crate::lua_config`]) — preferred when present |
//! | `~/.config/hyprpad/config.toml` | the flat TOML dialect described below |
//!
//! Both produce *this* `Config`, so the rest of the daemon never learns which
//! one was used. The Lua front-end additionally populates the modality fields —
//! [`ModeDef`]s and per-binding [`Guard`]s — which the TOML front-end leaves
//! empty; an empty mode list puts [`crate::mode::ModeEngine`] into its
//! built-in game/desktop behaviour, which is exactly what the TOML config has
//! always had. See `docs/research/lua-config.md` for why the second front-end
//! exists at all.
//!
//! The other sections are flat `key = value` tables of the same shape:
//!
//! | Section | What it configures |
//! |---|---|
//! | `[bindings]` | guide chords / stick flicks -> actions (the default section) |
//! | `[buttons]` | bare buttons -> any action: a key or mouse button is held with the button, anything else fires on the press edge (D-pad = arrows, pad click / triggers = clicks by default) |
//! | `[osk_buttons]` | what buttons do while the on-screen keyboard is up — a key typed through it, or one of its own actions (`osk commit\|shift\|dismiss`) — layered over the built-in Deck map ([`osk_builtins`]) |
//! | `[daemon]` | daemon-wide switches (`own_lizard`, `rescan_on_title_change`, `process_rescan_ms`) |
//! | `[cursor]` (alias `[damping]`) | trackpad-cursor gain + smoothing ([`CursorConfig`]) |
//! | `[scroll]` | left-pad scroll mode and feel ([`ScrollConfig`]) |
//! | `[haptics]` | pad-actuator feedback: which events buzz, and how hard ([`HapticsConfig`]) |
//! | `[gamepad]` | the virtual pad fed to games under focus, and its rumble back-channel ([`GamepadConfig`]) |
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
use crate::output::{PointerButton, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::report;
use std::collections::HashMap;
use std::rc::Rc;

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
    /// shown. `reflow` picks the presentation: `false` (the default) floats the
    /// keyboard over the desktop; `true` claims an exclusive zone so workspace
    /// content is displaced around it. Driven by [`crate::osk::OskHandle`], not
    /// a Hyprland dispatch.
    ToggleKeyboard { mode: crate::osk::OskMode, reflow: bool },
    /// Emit a raw evdev code. Bound to a *bare* controller button in the
    /// `[buttons]` section (e.g. D-pad up -> `KEY_UP`); pressed while the
    /// button is held and released when it lifts, so the kernel auto-repeats.
    ///
    /// The code space is evdev's own, so it names a mouse button as readily as
    /// a key: a `KEY_*` code is typed through
    /// [`crate::keyboard::VirtualKeyboard`], while a `BTN_*` mouse code
    /// ([`BTN_LEFT`] / [`BTN_RIGHT`] / [`BTN_MIDDLE`] — spelled `mouse left`,
    /// or `h.mouse "left"` in Lua) is clicked through the virtual pointer
    /// ([`crate::output::VirtualPointer`]) instead. The daemon decides by the
    /// code ([`PointerButton::from_evdev`]); the binding tables, guards and
    /// mode engine never need to tell the two apart. Not a Hyprland dispatch.
    Key(u16),
    /// Force the named mode, overriding whatever the context rules resolve to
    /// ([`crate::mode::ModeEngine`]'s manual override — the top of the
    /// precedence, docs/13). Applied by the daemon loop, not dispatched.
    SetMode(String),
    /// Drop a manual override so the context rules decide again.
    ClearMode,
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
                // `osk commit|shift|dismiss` is one of the keyboard's OWN
                // actions — an `[osk_buttons]` binding, live only while it is
                // up — not a chord action. Say so, rather than reporting an
                // unknown keyboard option.
                if let Some(word) = rest.split_whitespace().next() {
                    if osk_verb(word).is_some() {
                        return Err(format!(
                            "'osk {word}' is an on-screen keyboard binding, not an action: \
                             it belongs in [osk_buttons] (h.osk_button)"
                        ));
                    }
                }
                // Grammar: `keyboard [bottom|split] [overlay|reflow]`. Both
                // words optional; overlay (float over the desktop) is the
                // default presentation, matching the OSK's own default.
                let mut mode = crate::osk::OskMode::Bottom;
                let mut reflow = false;
                for word in rest.to_ascii_lowercase().split_whitespace() {
                    match word {
                        "bottom" | "deck" => mode = crate::osk::OskMode::Bottom,
                        "split" | "side" => mode = crate::osk::OskMode::Split,
                        "overlay" | "float" => reflow = false,
                        "reflow" | "displace" | "push" => reflow = true,
                        other => {
                            return Err(format!(
                                "unknown keyboard option '{other}' (want bottom|split, overlay|reflow)"
                            ))
                        }
                    }
                }
                Ok(Action::ToggleKeyboard { mode, reflow })
            }
            "key" => {
                if rest.is_empty() {
                    Err("key needs a name".to_string())
                } else {
                    Ok(Action::Key(key_code(rest)?))
                }
            }
            // A mouse button is a key in evdev's code space (`BTN_LEFT` sits a
            // little past the last `KEY_*`), so it is the same action; only the
            // spelling — and the device the daemon routes it to — differs.
            "mouse" | "click" => {
                if rest.is_empty() {
                    Err("mouse needs a button: left|right|middle".to_string())
                } else {
                    Ok(Action::Key(mouse_code(rest)?))
                }
            }
            // The manual mode override (docs/13 "Owner decisions" #4). Spelled
            // in TOML too, so a `config.toml` user can bind a chord to force a
            // mode even though only the Lua front-end can *declare* modes.
            "set_mode" | "mode" => {
                if rest.is_empty() {
                    Err("set_mode needs a mode name".to_string())
                } else {
                    Ok(Action::SetMode(rest.to_string()))
                }
            }
            "clear_mode" | "unset_mode" => Ok(Action::ClearMode),
            other => Err(format!("unknown action '{other}'")),
        }
    }
}

/// What a bare button does: a held output that follows the button, or a
/// one-shot action on its press edge.
///
/// A bare button (`[buttons]` / `h.button`) takes any action a guide chord
/// takes, but a button is *held*, and only some actions mean something for as
/// long as it is:
///
/// * [`Hold`](Self::Hold) — a `KEY_*` or `BTN_*` evdev code ([`Action::Key`],
///   spelled `key up` / `mouse left`). Pressed on the down edge and released
///   on the up edge, so the kernel auto-repeats a held arrow and a held mouse
///   button drags. Today's behaviour for every bare button.
/// * [`Fire`](Self::Fire) — everything else (`exec`, `dispatch`, `workspace`,
///   `keyboard`, `fullscreen`, `set_mode`, `clear_mode`). Performed **once**
///   on the press edge, through the same path a guide chord takes after it
///   resolves, and never again while the button stays down.
///
/// [`Action::None`] is neither: a bare button with no action is a config
/// error — remove the line instead ([`ButtonAction::classify`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    /// An evdev code held down with the button.
    Hold(u16),
    /// An action fired once on the press edge.
    Fire(Action),
}

impl ButtonAction {
    /// Sort a parsed action into what a bare button does with it.
    pub fn classify(action: Action) -> Result<ButtonAction, String> {
        match action {
            Action::Key(code) => Ok(ButtonAction::Hold(code)),
            Action::None => Err("a bare button needs an action; to leave a button unbound, \
                                 remove its line instead of binding it to none"
                .to_string()),
            other => Ok(ButtonAction::Fire(other)),
        }
    }

    /// The action as a config would spell it — the inverse of
    /// [`classify`](Self::classify), for the cheat sheet.
    pub fn to_action(&self) -> Action {
        match self {
            ButtonAction::Hold(code) => Action::Key(*code),
            ButtonAction::Fire(a) => a.clone(),
        }
    }
}

/// What a button does while the on-screen keyboard is up (`[osk_buttons]` /
/// `h.osk_button`).
///
/// The keyboard has three verbs of its own, plus a key typed through it. Every
/// behaviour the daemon used to hardwire while the keyboard owned the pads is
/// one of these, so the built-in map ([`osk_builtins`]) and a config's entries
/// are the same kind of thing and layer cleanly ([`Config::osk_buttons_in`]).
/// None of them means anything on a chord or a bare button: the keyboard is
/// not up there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OskAction {
    /// Tap this raw evdev key through the keyboard's own virtual keyboard
    /// (`key space`, `h.key "space"`): the Deck's Y = Space, X = Backspace,
    /// R2 = Enter. Never a mouse button — there is no pointer in a keyboard.
    Key(u16),
    /// Type the key under the cursor of the pad on this button's side of the
    /// puck (`osk commit`, `h.osk "commit"`): the pad clicks.
    Commit,
    /// Hold Shift for as long as the button is down (`osk shift`,
    /// `h.osk "shift"`): the Deck's L2. Momentary, like the key itself — it
    /// forces the shifted level while held and leaves the keyboard's own
    /// one-shot/caps latch exactly as it was.
    Shift,
    /// Close the keyboard (`osk dismiss`, `h.osk "dismiss"`; `close` and
    /// `hide` are aliases): B and Menu.
    Dismiss,
    /// Nothing — how a config takes a built-in away from a button
    /// (`menu = "none"`, `h.osk_button("menu", h.none())`).
    None,
}

/// The error for a mouse button in the keyboard's table, worded for both
/// front-ends: the TOML section and the Lua call each get named.
const OSK_MOUSE_ERR: &str = "osk_buttons send keys through the on-screen keyboard; a mouse \
                             button makes no sense there — bind it in [buttons] / h.button instead";

impl OskAction {
    /// Parse an `[osk_buttons]` value: `key <name>`, `osk <commit|shift|dismiss>`,
    /// or `none`. The `key` half is [`Action::parse`]'s own grammar, so a key
    /// is spelled the same in every table.
    pub fn parse(s: &str) -> Result<OskAction, String> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("none") {
            return Ok(OskAction::None);
        }
        let (verb, rest) = match s.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim()),
            None => (s, ""),
        };
        match verb.to_ascii_lowercase().as_str() {
            "osk" => osk_verb(rest).ok_or_else(|| {
                format!("unknown on-screen keyboard action '{rest}' (want osk commit|shift|dismiss)")
            }),
            "key" | "mouse" | "click" => match Action::parse(s)? {
                Action::Key(code) if is_mouse_code(code) => Err(OSK_MOUSE_ERR.to_string()),
                Action::Key(code) => Ok(OskAction::Key(code)),
                other => Err(format!("'{s}' is not a key ({other:?})")),
            },
            _ => Err("osk_buttons values must be a 'key <name>', one of the keyboard's own \
                      actions ('osk commit|shift|dismiss'), or none"
                .to_string()),
        }
    }
}

/// The keyboard's own verbs by their config word (`osk <word>`).
fn osk_verb(word: &str) -> Option<OskAction> {
    match word.trim().to_ascii_lowercase().as_str() {
        "commit" | "type" => Some(OskAction::Commit),
        "shift" => Some(OskAction::Shift),
        "dismiss" | "close" | "hide" => Some(OskAction::Dismiss),
        _ => None,
    }
}

/// What the buttons do while the on-screen keyboard is up before a config says
/// otherwise — the Steam Deck's own keyboard map, so the puck reads like the
/// Deck: `(button, action, cheat-sheet label)`.
///
/// | button | does |
/// |---|---|
/// | left / right pad click | type the key under that pad's cursor |
/// | L2 (full pull) | hold Shift |
/// | R2 (full pull) | Enter |
/// | Y | Space |
/// | X | Backspace |
/// | B, Menu | close the keyboard |
///
/// These are always on: a config's `osk_buttons` are layered **over** them
/// ([`Config::osk_buttons_in`]), never in place of them, so a config that lists
/// only `y = "key space"` keeps the pad clicks committing and B closing without
/// saying so, and a built-in is taken away by binding its button to `none`.
pub fn osk_builtins() -> impl Iterator<Item = (report::Button, OskAction, &'static str)> {
    use report::Button::*;
    // KEY_ENTER / KEY_SPACE / KEY_BACKSPACE, as `key_code` spells them; a test
    // pins the two together.
    [
        (PadLeftClick, OskAction::Commit, "Type the key under the cursor"),
        (PadRightClick, OskAction::Commit, "Type the key under the cursor"),
        (TriggerL2Full, OskAction::Shift, "Shift (hold)"),
        (TriggerR2Full, OskAction::Key(28), "Enter"),
        (Y, OskAction::Key(57), "Space"),
        (X, OskAction::Key(14), "Backspace"),
        (B, OskAction::Dismiss, "Close the keyboard"),
        (Menu, OskAction::Dismiss, "Close the keyboard"),
    ]
    .into_iter()
}

/// The normalized binding key a gesture event resolves against.
///
/// `pub(crate)` so the Lua front-end ([`crate::lua_config`]) can build the same
/// binding table this module's TOML parser does; it is not part of the public
/// API and callers outside the crate go through [`Config::resolve`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum GestureKey {
    Chord(report::Button),
    Flick(Stick, StickDir),
    /// A bare guide tap (`GuideLeave { was_chorded: false }`).
    Tap,
    /// The guide crossing the hold threshold (`GuideHold`).
    Hold,
}

impl GestureKey {
    /// The binding key a gesture event resolves against, or `None` for the
    /// lifecycle events that carry no binding — `GuideEnter` and a *chorded*
    /// `GuideLeave`.
    pub(crate) fn of(ev: &gesture::GestureEvent) -> Option<GestureKey> {
        use gesture::GestureEvent as E;
        match ev {
            E::GuideChord(b) => Some(GestureKey::Chord(*b)),
            E::GuideStickFlick { stick, dir } => Some(GestureKey::Flick(*stick, *dir)),
            E::GuideLeave { was_chorded: false } => Some(GestureKey::Tap),
            E::GuideHold => Some(GestureKey::Hold),
            E::GuideEnter | E::GuideLeave { was_chorded: true } => None,
        }
    }

    /// Parse a binding key such as `"guide+r1"`, `"guide+stick_right"`,
    /// `"guide+lstick_up"`, `"guide_tap"`, or `"guide_hold"`.
    pub(crate) fn parse(raw: &str) -> Result<GestureKey, String> {
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

pub(crate) fn parse_button(s: &str) -> Result<report::Button, String> {
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
pub(crate) fn key_code(name: &str) -> Result<u16, String> {
    let code = match name.trim().to_ascii_lowercase().as_str() {
        "up" => 103,        // KEY_UP
        "down" => 108,      // KEY_DOWN
        "left" => 105,      // KEY_LEFT
        "right" => 106,     // KEY_RIGHT
        "enter" | "return" => 28, // KEY_ENTER
        "backspace" => 14,  // KEY_BACKSPACE
        "space" => 57,      // KEY_SPACE
        "tab" => 15,        // KEY_TAB
        "escape" | "esc" => 1,
        "back" => 158, // KEY_BACK -> XF86Back: the standard "back" key (Omarchy menu end state) // KEY_ESC
        "home" => 102,      // KEY_HOME
        "end" => 107,       // KEY_END
        "pageup" | "pgup" => 104, // KEY_PAGEUP
        "pagedown" | "pgdn" => 109, // KEY_PAGEDOWN
        // The mouse buttons, under their evdev names: `key btn_left` is the
        // same binding `mouse left` is. Routed to the pointer by the daemon.
        "btn_left" => BTN_LEFT,
        "btn_right" => BTN_RIGHT,
        "btn_middle" => BTN_MIDDLE,
        other => return Err(format!("unknown key '{other}'")),
    };
    Ok(code)
}

/// Map a mouse-button name (the argument of a `mouse <button>` action, or
/// `h.mouse "<button>"` in Lua) to its evdev `BTN_*` code, which
/// [`Action::Key`] carries like any keycode.
///
/// Accepts `left|right|middle`, the evdev names `btn_left|btn_right|btn_middle`,
/// the abbreviations `lmb|rmb|mmb`, and the X-style numbers `1|2|3`. Matched
/// case-insensitively; an unknown name is a reported error naming the choices.
pub(crate) fn mouse_code(name: &str) -> Result<u16, String> {
    let code = match name.trim().to_ascii_lowercase().as_str() {
        "left" | "btn_left" | "lmb" | "1" => BTN_LEFT,
        "right" | "btn_right" | "rmb" | "2" => BTN_RIGHT,
        "middle" | "btn_middle" | "mmb" | "3" => BTN_MIDDLE,
        other => {
            return Err(format!(
                "unknown mouse button '{other}' (want left|right|middle; \
                 also btn_left|btn_right|btn_middle, lmb|rmb|mmb, 1|2|3)"
            ))
        }
    };
    Ok(code)
}

/// Whether an [`Action::Key`] code is a mouse button — one the daemon clicks
/// through the virtual pointer rather than typing through the virtual keyboard.
pub(crate) fn is_mouse_code(code: u16) -> bool {
    PointerButton::from_evdev(code).is_some()
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

/// Which puck report the game-rumble back-channel drives (`[gamepad]
/// rumble_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RumbleMode {
    /// The puck's own force-feedback report, `0x80`
    /// ([`crate::haptics::Haptics::rumble`]) — a faithful replay of what
    /// `hid-steam` sends for an `FF_RUMBLE` effect. The default.
    #[default]
    Native,
    /// Approximate the rumble with trains of the `0x81` pulse instead. A hedge:
    /// the `0x81` pulse is the report hyprpad has actually exercised on this
    /// unit, whereas `0x80` has only ever been replayed from the kernel source
    /// (`hid-generic` binds the puck here, so the in-kernel rumble path has
    /// never run on it). If `native` turns out inert on-device, this still buzzes.
    Pulse,
}

impl RumbleMode {
    pub(crate) fn parse(s: &str) -> Result<RumbleMode, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "native" | "rumble" | "ff" => Ok(RumbleMode::Native),
            "pulse" | "pulses" | "approx" => Ok(RumbleMode::Pulse),
            other => Err(format!("unknown rumble mode '{other}' (native|pulse)")),
        }
    }
}

/// Virtual-gamepad knobs (the `[gamepad]` config section).
///
/// The Tier-1 keystone: hyprpad owns the real puck, so Steam and games are fed a
/// synthesized Xbox-360-class pad ([`crate::gamepad`]) whenever a game holds
/// focus. All of these are read *per frame* by the daemon, so `hyprpad reload`
/// takes effect on the next report with nothing to rebuild.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadConfig {
    /// Master switch. Default **`true`** — this is the point of the project. It
    /// costs a non-gamer nothing: the uinput device is created lazily, on the
    /// first frame a game-classed window actually holds focus.
    pub enabled: bool,
    /// Forward the guide (Steam) button to the virtual pad as `BTN_MODE`.
    /// Default **`false`**: docs/08's input contract makes `Guide` hyprpad's
    /// global modifier, and a chord must never also reach the game. Turn it on
    /// to hand Steam's overlay its own button back.
    pub forward_guide: bool,
    /// Forward a game's force-feedback rumble to the puck's actuators. Default
    /// `true`.
    pub rumble: bool,
    /// Which report carries it. Default [`RumbleMode::Native`].
    pub rumble_mode: RumbleMode,
    /// Scale applied to both `FF_RUMBLE` magnitudes before they reach the puck.
    /// `1.0` passes a game's request through unchanged (which is what
    /// `hid-steam` does); `0` or below silences rumble — use `rumble = false` to
    /// switch it off properly. Default `1.0`.
    pub rumble_intensity: f64,
}

impl Default for GamepadConfig {
    fn default() -> GamepadConfig {
        GamepadConfig {
            enabled: true,
            forward_guide: false,
            rumble: true,
            rumble_mode: RumbleMode::Native,
            rumble_intensity: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Modality: modes as named contexts, and per-binding guards (docs/13).
// ---------------------------------------------------------------------------

/// A declared **mode**: a named context the daemon can be in.
///
/// Per the owner's decision (docs/13 "Owner decisions" #1) a mode is *only* a
/// name plus the rule that selects it — it does not carry category switches.
/// What is live in a mode is decided per binding, by that binding's [`Guard`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeDef {
    /// The mode's name, as written in `h.mode("game")` and in every guard that
    /// refers to it.
    pub name: String,
    /// Index of this mode's selection predicate in the Lua predicate table, or
    /// `None` for a mode with no rule — reachable only as `default_mode` or via
    /// a manual override ([`Action::SetMode`]).
    pub rule: Option<usize>,
    /// Whether raw controller input is handed to the virtual gamepad while this
    /// mode is active (`h.mode("game", { forward = true })`). This is the mode
    /// model's replacement for "a game window is focused".
    pub forward: bool,
}

/// A **second** bare-button binding for a button that already carries one, with
/// its own guard — how a button means different things in different modes.
///
/// `buttons` holds one binding per button: all a `config.toml` can say, and all
/// the no-modes path can use. Once modes exist a button can be bound more than
/// once, each binding guarded into a different mode:
///
/// ```lua
/// h.button("b", h.key "backspace"):only_in("desktop")
/// h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
/// ```
///
/// Modes are exclusive, so at most one of a button's bindings is ever live;
/// [`Config::buttons_in`] takes the first whose guard passes, the base map's
/// binding first, so the file reads top-down. Two bindings that *are* live at
/// once (both unguarded — a config bug) resolve to the first declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonAlt {
    /// The button being bound again.
    pub button: report::Button,
    /// What it does where this binding is live.
    pub action: ButtonAction,
    /// Where it is live. Practically always a mode guard: an unguarded
    /// re-binding of an already-bound button can never win.
    pub guard: Guard,
    /// The optional human description, for `hyprpad bindings`.
    pub desc: Option<String>,
}

/// When a binding — or an ambient handler such as the cursor — is live.
///
/// Every `h.bind` / `h.button` / `h.osk_button`, and the `h.cursor` /
/// `h.scroll` "virtual bindings", carries one of these. [`Guard::Always`] (the
/// default, and everything the TOML front-end produces) means *live in every
/// mode*, which is why an unguarded config behaves exactly as it always has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Guard {
    /// Live in every mode. The default for an unguarded binding.
    #[default]
    Always,
    /// Live only while one of these modes is active (`:only_in("desktop")`).
    OnlyIn(Vec<String>),
    /// Live except while one of these modes is active (`:not_in("game")`).
    NotIn(Vec<String>),
    /// Live when this Lua predicate returns truthy (`:when(function(ctx) …
    /// end)`). The index is into the config's predicate table; the *result* is
    /// cached in [`ModeState`], recomputed only on a context change — never per
    /// input frame.
    When(usize),
}

/// The `Guard::Always` singleton, so [`Config::gesture_guard`] can hand back a
/// reference for an unguarded binding without allocating.
static ALWAYS: Guard = Guard::Always;

impl Guard {
    /// Whether this guard passes in the given resolved modality snapshot.
    ///
    /// A [`When`](Guard::When) guard whose predicate is missing from the
    /// snapshot (it errored or timed out) reads as **false** — the same
    /// "a broken predicate is not a match" rule the mode rules use, so a bad
    /// guard silences one binding rather than taking the daemon with it.
    pub fn allows(&self, st: &ModeState) -> bool {
        match self {
            Guard::Always => true,
            Guard::OnlyIn(modes) => modes.iter().any(|m| m == st.active()),
            Guard::NotIn(modes) => !modes.iter().any(|m| m == st.active()),
            Guard::When(i) => st.predicate(*i),
        }
    }

    /// Every mode name this guard mentions, for load-time validation (a typo in
    /// `:only_in("desktopp")` would otherwise silently disable a binding).
    pub fn mode_names(&self) -> &[String] {
        match self {
            Guard::OnlyIn(m) | Guard::NotIn(m) => m,
            Guard::Always | Guard::When(_) => &[],
        }
    }
}

/// The resolved modality snapshot every [`Guard`] is evaluated against: which
/// mode is active, and the cached truth of every `:when` predicate.
///
/// Built by [`crate::mode::ModeEngine`] on a **context change** (focus,
/// fullscreen, manual override, reload) and then read — never recomputed — by
/// the per-frame handlers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeState {
    active: String,
    predicates: Vec<bool>,
}

impl ModeState {
    /// A snapshot with `active` as the active mode and `predicates[i]` the
    /// cached result of predicate `i`.
    pub fn new(active: impl Into<String>, predicates: Vec<bool>) -> ModeState {
        ModeState { active: active.into(), predicates }
    }

    /// The active mode's name.
    pub fn active(&self) -> &str {
        &self.active
    }

    /// The cached result of predicate `i`; `false` when it is absent (errored,
    /// timed out, or out of range).
    pub fn predicate(&self, i: usize) -> bool {
        self.predicates.get(i).copied().unwrap_or(false)
    }
}

/// Default `process_rescan_ms`: how often the focused window's process tree is
/// re-walked when a mode rule actually asks about it. The walk is a handful of
/// `/proc` reads, so twice a second is imperceptible either way — fast enough
/// that a program which renames nothing is noticed while the user is still
/// reaching for the controller, cheap enough to leave on.
pub const DEFAULT_PROCESS_RESCAN_MS: u64 = 500;

/// A set of gesture bindings.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub(crate) bindings: HashMap<GestureKey, Action>,
    /// Bare-button bindings (the `[buttons]` section): what a controller button
    /// pressed *without* the guide modifier does — an evdev code held with it,
    /// or any other action fired on its press edge ([`ButtonAction`]). Distinct
    /// from `bindings`, which are guide chords. The default maps the D-pad to
    /// the arrow keys and the pad click / triggers to mouse clicks.
    pub(crate) buttons: HashMap<report::Button, ButtonAction>,
    /// What the config says a button does while the on-screen keyboard is up
    /// (the `[osk_buttons]` section) — a key typed THROUGH the OSK (its uinput
    /// types it) or one of the keyboard's own actions ([`OskAction`]). Only
    /// what the config wrote: the built-in Deck map these are layered over is
    /// [`osk_builtins`], and the result of the layering is
    /// [`Config::osk_buttons_in`]. Distinct from `buttons`, which are live only
    /// when the OSK is down.
    pub(crate) osk_buttons: HashMap<report::Button, OskAction>,
    /// Whether hyprpad should take ownership of the puck's lizard mode and keep
    /// the firmware keyboard/mouse emulation disabled ([`crate::lizard`]). Set
    /// via `own_lizard = true` in the `[daemon]` section. Default `false`, so we
    /// never fight an unmasked Steam that is managing lizard mode itself
    /// (docs/experiments/w12-device-denial.md).
    pub(crate) own_lizard: bool,
    /// Whether a `windowtitle` event on the **focused** window re-resolves the
    /// modes (`rescan_on_title_change` in `[daemon]` / `h.daemon`). `None` is
    /// the default, which is *on*: it is the event-driven half of noticing a
    /// program that starts inside an already-focused terminal, and it polls
    /// nothing. Read per event, so a reload retunes it.
    pub(crate) rescan_on_title_change: Option<bool>,
    /// How often the focused window's process tree may be re-walked, in
    /// milliseconds; `0` disables the sweep (`process_rescan_ms`). `None` is the
    /// default, [`DEFAULT_PROCESS_RESCAN_MS`]. Read per tick, so a reload
    /// retunes it.
    pub(crate) process_rescan_ms: Option<u64>,
    /// Trackpad-cursor smoothing knobs (`[cursor]`/`[damping]` section).
    pub(crate) cursor: CursorConfig,
    /// Left-trackpad scroll knobs (`[scroll]` section).
    pub(crate) scroll: ScrollConfig,
    /// Haptic-feedback knobs (`[haptics]` section).
    pub(crate) haptics: HapticsConfig,
    /// Virtual-gamepad knobs (`[gamepad]` section).
    pub(crate) gamepad: GamepadConfig,

    // --- Modality (Lua front-end only; empty from TOML) --------------------
    /// The declared modes, **in definition order** — the order the rules are
    /// evaluated in, first match wins. Empty for a TOML config, which puts
    /// [`crate::mode::ModeEngine`] into its built-in game/desktop behaviour.
    pub(crate) modes: Vec<ModeDef>,
    /// The mode chosen when no rule matches (`h.default_mode "desktop"`).
    pub(crate) default_mode: Option<String>,
    /// Per-binding guards, keyed exactly like `bindings`. A missing entry means
    /// [`Guard::Always`].
    pub(crate) binding_guards: HashMap<GestureKey, Guard>,
    /// Per-binding guards for the bare-button (`[buttons]`) map.
    pub(crate) button_guards: HashMap<report::Button, Guard>,
    /// Further bare-button bindings for buttons `buttons` already binds, in
    /// declaration order — the same button meaning different things in
    /// different modes ([`ButtonAlt`]). Empty for a TOML config, which has one
    /// binding per button and no modes to tell them apart.
    pub(crate) button_alts: Vec<ButtonAlt>,
    /// Per-binding guards for the OSK-helper (`[osk_buttons]`) map.
    pub(crate) osk_button_guards: HashMap<report::Button, Guard>,

    // --- Descriptions (Lua front-end only; empty from TOML) ----------------
    /// The optional human description a `config.lua` gave a binding —
    /// `h.bind("guide+r1", "Workspace right", …)`. Keyed exactly like
    /// `bindings`; a missing entry means the cheat sheet derives a label from
    /// the action instead ([`crate::bindings_sheet`]). Never consulted by the
    /// input path: this is documentation, not behaviour.
    pub(crate) binding_descs: HashMap<GestureKey, String>,
    /// Descriptions for the bare-button (`[buttons]`) map.
    pub(crate) button_descs: HashMap<report::Button, String>,
    /// Descriptions for the OSK-helper (`[osk_buttons]`) map.
    pub(crate) osk_button_descs: HashMap<report::Button, String>,
    /// The guard on the cursor "virtual binding" (`h.cursor { only_in = … }`).
    pub(crate) cursor_guard: Guard,
    /// The guard on the scroll "virtual binding" (`h.scroll { only_in = … }`).
    pub(crate) scroll_guard: Guard,
    /// The live Lua state behind a `config.lua`, holding the mode rules and
    /// `:when` predicates. `None` for a TOML config. Shared (`Rc`) because
    /// `Config` is `Clone` and the interpreter must not be duplicated;
    /// single-threaded by construction — only the daemon loop touches it.
    pub(crate) lua: Option<Rc<crate::lua_config::LuaRuntime>>,
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

# Bare buttons: pressed WITHOUT the guide modifier. A key or mouse button is
# held with the button: the D-pad acts as the arrow keys on the desktop (holding
# repeats, like a real keyboard); a hard right-pad click or a full right-trigger
# pull is a left click, a full left-trigger pull a right click. Any other action
# from the [bindings] grammar works here too (`l5 = "exec …"`, `r4 = "keyboard
# split"`) and fires once, on the press. These are suppressed while the guide
# layer or on-screen keyboard is up, and in a focused game — there the D-pad
# reaches the game as a controller. A config that lists its own [buttons]
# replaces this whole table, clicks included.
[buttons]
dpad_up = "key up"
dpad_down = "key down"
dpad_left = "key left"
dpad_right = "key right"
rpad_click = "mouse left"
r2 = "mouse left"
l2 = "mouse right"

# What buttons do while the on-screen keyboard is up. The Steam Deck's own map
# is built in and always on: a pad click types the key under that pad's cursor
# ("osk commit"), L2 holds Shift ("osk shift" — momentary, the keyboard's own
# one-shot/caps latch is untouched), R2 is Enter, Y is Space, X is Backspace,
# and B or Menu close it ("osk dismiss"). Entries here are layered OVER that
# map, one button at a time, never in place of it: rebind a button with a
# "key <name>" (typed THROUGH the OSK) or an "osk commit|shift|dismiss", or
# take a built-in away with "none" (menu = "none"). The two below restate the
# built-ins, as a worked example of the syntax.
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

# The virtual gamepad (docs/06 Tier 1). hyprpad owns the real controller, so
# Steam and games are fed a synthesized Xbox-360-class pad instead — but ONLY
# while a game-classed window holds focus, and never while the guide button is
# held or the on-screen keyboard is up. The device is created lazily on the first
# such frame, so a session that never plays a game never creates one.
[gamepad]
enabled = true              # master switch for the whole forwarding path
forward_guide = false       # send the guide button to the game as BTN_MODE (it is hyprpad's modifier)
rumble = true               # forward a game's force feedback to the puck's actuators
rumble_mode = native        # native (the puck's 0x80 rumble report) | pulse (approximate with 0x81 trains)
rumble_intensity = 1.0      # scale on both FF magnitudes; 1.0 passes the game's request through
"#;

impl Config {
    /// Parse a config from the TOML dialect described in the module docs.
    pub fn from_toml_str(s: &str) -> Result<Config, String> {
        let mut bindings = HashMap::new();
        let mut buttons = HashMap::new();
        let mut osk_buttons = HashMap::new();
        let mut own_lizard = false;
        let mut rescan_on_title_change = None;
        let mut process_rescan_ms = None;
        let mut cursor = CursorConfig::default();
        let mut scroll = ScrollConfig::default();
        let mut haptics = HapticsConfig::default();
        let mut gamepad = GamepadConfig::default();
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
                // modifier. The key uses the same button-name aliases as the
                // guide chords (`dpad_up`, `a`, `r1`, ...); the value is any
                // action in the `[bindings]` grammar, sorted into held-with-
                // the-button or fired-on-press by `ButtonAction::classify`.
                "buttons" => {
                    let button = parse_button(&unquote(k).trim().to_ascii_lowercase())
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    let action = Action::parse(&unquote(v))
                        .and_then(ButtonAction::classify)
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    buttons.insert(button, action);
                }
                // The on-screen keyboard's table: what a button does while it
                // is up. Same button names; the values are `OskAction`'s own
                // grammar (a key typed THROUGH the OSK, or `osk commit|shift|
                // dismiss`), layered over the built-in Deck map.
                "osk_buttons" => {
                    let button = parse_button(&unquote(k).trim().to_ascii_lowercase())
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    let action = OskAction::parse(&unquote(v))
                        .map_err(|e| format!("line {lineno}: {e}"))?;
                    osk_buttons.insert(button, action);
                }
                // Daemon-wide settings (not gesture bindings).
                "daemon" => {
                    let key = unquote(k).to_ascii_lowercase();
                    match key.as_str() {
                        "own_lizard" => {
                            own_lizard = parse_bool(&unquote(v))
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "rescan_on_title_change" => {
                            rescan_on_title_change = Some(
                                parse_bool(&unquote(v))
                                    .map_err(|e| format!("line {lineno}: {e}"))?,
                            );
                        }
                        "process_rescan_ms" => {
                            process_rescan_ms = Some(
                                parse_millis(&unquote(v))
                                    .map_err(|e| format!("line {lineno}: {e}"))?,
                            );
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
                // Virtual-gamepad knobs: the master switch, whether the guide
                // button reaches the game, and the rumble back-channel.
                "gamepad" => {
                    let key = unquote(k).to_ascii_lowercase();
                    let val = unquote(v);
                    let flag = |slot: &mut bool| -> Result<(), String> {
                        *slot = parse_bool(&val).map_err(|e| format!("line {lineno}: {e}"))?;
                        Ok(())
                    };
                    match key.as_str() {
                        "enabled" | "enable" | "on" => flag(&mut gamepad.enabled)?,
                        "forward_guide" | "guide" | "forward_steam" => {
                            flag(&mut gamepad.forward_guide)?;
                        }
                        "rumble" | "force_feedback" | "ff" => flag(&mut gamepad.rumble)?,
                        "rumble_mode" | "mode" => {
                            gamepad.rumble_mode = RumbleMode::parse(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        "rumble_intensity" | "rumble_strength" | "rumble_gain" => {
                            gamepad.rumble_intensity = parse_f64(&val)
                                .map_err(|e| format!("line {lineno}: {e}"))?;
                        }
                        other => {
                            return Err(format!(
                                "line {lineno}: unknown [gamepad] setting '{other}'"
                            ));
                        }
                    }
                }
                _ => return Err(format!("line {lineno}: unknown section [{section}]")),
            }
        }
        Ok(Config {
            bindings,
            buttons,
            osk_buttons,
            own_lizard,
            rescan_on_title_change,
            process_rescan_ms,
            cursor,
            scroll,
            haptics,
            gamepad,
            // The TOML dialect declares no modes and no guards: everything is
            // unguarded, and an empty mode list is the signal that
            // `ModeEngine` should keep its built-in game/desktop behaviour.
            ..Config::default()
        })
    }

    /// The built-in defaults encoding the vision's core gestures.
    pub fn load_default() -> Config {
        Config::from_toml_str(DEFAULT_TOML).expect("built-in default config is valid")
    }

    /// The config *home*: `$XDG_CONFIG_HOME`, or `~/.config` when it is unset.
    /// `None` only if neither `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_home() -> Option<std::path::PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(dir));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(std::path::PathBuf::from(home).join(".config"))
    }

    /// The config directory: `$XDG_CONFIG_HOME/hyprpad`, or
    /// `~/.config/hyprpad` when `XDG_CONFIG_HOME` is unset. `None` only if
    /// neither `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_dir() -> Option<std::path::PathBuf> {
        Some(Config::config_root()?.join("hyprpad"))
    }

    /// The user's config root: `$XDG_CONFIG_HOME`, or `~/.config`. Both config
    /// directories hyprpad reads from hang off it — `hypr/` (the hypr-ecosystem
    /// convention, where `hypridle.conf`/`hyprlock.conf` live) and `hyprpad/`.
    /// `None` only if neither `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_root() -> Option<std::path::PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(dir));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(std::path::PathBuf::from(home).join(".config"))
    }

    /// The path the TOML user config is read from:
    /// `$XDG_CONFIG_HOME/hyprpad/config.toml`, or `~/.config/hyprpad/config.toml`
    /// when `XDG_CONFIG_HOME` is unset. Returns `None` only if neither
    /// `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_path() -> Option<std::path::PathBuf> {
        Some(Config::config_dir()?.join("config.toml"))
    }

    /// The conventional path for the **Lua** user config:
    /// `$XDG_CONFIG_HOME/hypr/hyprpad.lua`, or `~/.config/hypr/hyprpad.lua` —
    /// alongside `hypridle.conf`/`hyprlock.conf`, per the hypr-ecosystem
    /// convention. (`hyprpad/config.lua` is still honoured as a fallback; see
    /// [`active_config_path`](Self::active_config_path) for the full precedence.)
    /// When a Lua file exists it wins over `config.toml`.
    pub fn lua_config_path() -> Option<std::path::PathBuf> {
        Some(Config::config_root()?.join("hypr").join("hyprpad.lua"))
    }

    /// The config file [`load`](Self::load) would actually read, and which
    /// front-end it would use — for the daemon's startup banner and for
    /// diagnostics. `None` when neither file exists (built-in defaults).
    pub fn active_config_path() -> Option<(std::path::PathBuf, ConfigFormat)> {
        pick_front_end(&Config::config_root()?)
    }

    /// Load the user config, preferring `config.lua` (the Lua front-end) over
    /// `config.toml` (the TOML front-end) and falling back to
    /// [`load_default`](Self::load_default) when neither exists (or when no
    /// config directory can be resolved).
    ///
    /// Returns an error only when the chosen file is *present but malformed*,
    /// so the caller can surface a real misconfiguration rather than silently
    /// ignoring it — and, crucially, so the reload path
    /// (`crate::run::resolve_reload`) can keep the last-good config instead of
    /// running blind.
    pub fn load() -> Result<Config, String> {
        match Config::active_config_path() {
            Some((path, ConfigFormat::Lua)) => crate::lua_config::load_file(&path),
            Some((path, ConfigFormat::Toml)) => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    Config::from_toml_str(&text).map_err(|e| format!("{}: {e}", path.display()))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::load_default()),
                Err(e) => Err(format!("reading {}: {e}", path.display())),
            },
            None => Ok(Config::load_default()),
        }
    }

    /// Resolve a gesture event to its bound action, or [`Action::None`].
    ///
    /// Lifecycle events that carry no binding — `GuideEnter` and a *chorded*
    /// `GuideLeave` — always resolve to `None`. A bare `GuideLeave` maps to the
    /// optional `guide_tap` binding, and `GuideHold` to `guide_hold`.
    pub fn resolve(&self, ev: &gesture::GestureEvent) -> Action {
        let Some(key) = GestureKey::of(ev) else { return Action::None };
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

    /// The bare-button bindings (`[buttons]` section): what each controller
    /// button pressed *without* the guide modifier does ([`ButtonAction`]). The
    /// default maps the D-pad to the arrow keys and the pad click / triggers to
    /// mouse clicks.
    pub fn buttons(&self) -> &HashMap<report::Button, ButtonAction> {
        &self.buttons
    }

    /// What the config itself says a button does while the on-screen keyboard
    /// is up (`[osk_buttons]` section) — its entries only, before the built-in
    /// Deck map is layered underneath. What the keyboard actually gets is
    /// [`osk_buttons_in`](Self::osk_buttons_in).
    pub fn osk_buttons(&self) -> &HashMap<report::Button, OskAction> {
        &self.osk_buttons
    }

    /// Whether hyprpad should take ownership of the puck's lizard mode
    /// ([`crate::lizard`]). Configured by `own_lizard` in the `[daemon]`
    /// section; default `false`.
    pub fn own_lizard(&self) -> bool {
        self.own_lizard
    }

    /// Whether a rename of the focused window re-resolves the modes
    /// (`rescan_on_title_change` in the `[daemon]` section); default `true`.
    pub fn rescan_on_title_change(&self) -> bool {
        self.rescan_on_title_change.unwrap_or(true)
    }

    /// How often the focused window's process tree may be re-walked, in
    /// milliseconds (`process_rescan_ms` in the `[daemon]` section); default
    /// [`DEFAULT_PROCESS_RESCAN_MS`], `0` = off.
    pub fn process_rescan_ms(&self) -> u64 {
        self.process_rescan_ms.unwrap_or(DEFAULT_PROCESS_RESCAN_MS)
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

    /// The virtual-gamepad knobs (`[gamepad]` section); defaults from
    /// [`GamepadConfig::default`]. Read per frame by the daemon so a reload
    /// retunes forwarding and rumble with no restart.
    pub fn gamepad(&self) -> &GamepadConfig {
        &self.gamepad
    }

    // --- Modality ---------------------------------------------------------

    /// The declared modes, in definition order (the rule evaluation order).
    /// Empty for a TOML config — see [`crate::mode::ModeEngine`].
    pub fn modes(&self) -> &[ModeDef] {
        &self.modes
    }

    /// The mode selected when no rule matches. `"desktop"` unless the config
    /// said otherwise with `h.default_mode`.
    pub fn default_mode(&self) -> &str {
        self.default_mode.as_deref().unwrap_or("desktop")
    }

    /// The live Lua state, when this config came from the Lua front-end.
    pub fn lua(&self) -> Option<&crate::lua_config::LuaRuntime> {
        self.lua.as_deref()
    }

    /// Whether resolving modes needs the focused window's **pid** — i.e.
    /// whether a `config.lua` with real rules is in play. The daemon only pays
    /// for the extra `j/activewindow` round trip on a focus change when this is
    /// true.
    pub fn needs_focus_pid(&self) -> bool {
        self.lua.is_some() && !self.modes.is_empty()
    }

    /// The guard on a gesture binding, or [`Guard::Always`] when it carries
    /// none. Lifecycle events that can never be bound also read `Always` — they
    /// resolve to [`Action::None`] anyway.
    pub fn gesture_guard(&self, ev: &gesture::GestureEvent) -> &Guard {
        match GestureKey::of(ev) {
            Some(k) => self.binding_guards.get(&k).unwrap_or(&ALWAYS),
            None => &ALWAYS,
        }
    }

    /// Resolve a gesture to its action **in a given mode**: exactly
    /// [`resolve`](Self::resolve), except that a binding whose guard does not
    /// pass yields [`Action::None`].
    pub fn resolve_in(&self, ev: &gesture::GestureEvent, st: &ModeState) -> Action {
        if self.gesture_guard(ev).allows(st) {
            self.resolve(ev)
        } else {
            Action::None
        }
    }

    /// The bare-button map filtered to the bindings live in `st`. Built on a
    /// **mode transition**, never per frame, and handed to
    /// `crate::run::drive_buttons` in place of [`buttons`](Self::buttons).
    ///
    /// A button bound once per mode ([`ButtonAlt`]) resolves here: the base
    /// binding first, then the alternates in declaration order, first guard
    /// that passes wins. Since modes are exclusive there is normally no
    /// competition — `b` is backspace on the desktop and escape under the cheat
    /// sheet, and never both.
    pub fn buttons_in(&self, st: &ModeState) -> HashMap<report::Button, ButtonAction> {
        let mut live = filter_buttons(&self.buttons, &self.button_guards, st);
        for alt in &self.button_alts {
            if alt.guard.allows(st) {
                live.entry(alt.button).or_insert_with(|| alt.action.clone());
            }
        }
        live
    }

    /// What the buttons do while the on-screen keyboard is up, in `st`: the
    /// built-in Deck map ([`osk_builtins`]) with the config's `osk_buttons`
    /// layered over it. A config entry replaces the built-in for its button —
    /// with nothing, when it is `none` — and an entry whose guard fails in
    /// `st` lets the built-in show through, so a helper guarded into one mode
    /// never leaves its button dead in the others.
    pub fn osk_buttons_in(&self, st: &ModeState) -> HashMap<report::Button, OskAction> {
        let mut live: HashMap<_, _> = osk_builtins().map(|(b, a, _)| (b, a)).collect();
        for (b, a) in filter_buttons(&self.osk_buttons, &self.osk_button_guards, st) {
            match a {
                OskAction::None => {
                    live.remove(&b);
                }
                a => {
                    live.insert(b, a);
                }
            }
        }
        live
    }

    /// Whether the right pad drives the desktop cursor in `st`
    /// (`h.cursor { only_in = { "desktop" } }`).
    pub fn cursor_enabled_in(&self, st: &ModeState) -> bool {
        self.cursor_guard.allows(st)
    }

    /// Whether the left pad scrolls in `st` (`h.scroll { only_in = … }`).
    pub fn scroll_enabled_in(&self, st: &ModeState) -> bool {
        self.scroll_guard.allows(st)
    }

    /// Every `:when` predicate index the config uses, so the mode engine knows
    /// how large a result vector to build. One past the highest index in use.
    pub fn predicate_slots(&self) -> usize {
        let guards = self
            .binding_guards
            .values()
            .chain(self.button_guards.values())
            .chain(self.osk_button_guards.values())
            .chain(self.button_alts.iter().map(|a| &a.guard))
            .chain([&self.cursor_guard, &self.scroll_guard]);
        let from_guards = guards.filter_map(|g| match g {
            Guard::When(i) => Some(*i + 1),
            _ => None,
        });
        let from_modes = self.modes.iter().filter_map(|m| m.rule.map(|i| i + 1));
        from_guards.chain(from_modes).max().unwrap_or(0)
    }
}

/// Which front-end [`Config::load`] used (or would use).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigFormat {
    /// `config.lua`, via [`crate::lua_config`].
    Lua,
    /// `config.toml`, via [`Config::from_toml_str`].
    Toml,
}

impl std::fmt::Display for ConfigFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ConfigFormat::Lua => "lua",
            ConfigFormat::Toml => "toml",
        })
    }
}

/// Which config file wins, and which front-end reads it.
///
/// `config.lua` beats `config.toml` when both exist, so migrating is "write the
/// Lua file", and rolling back is "rename it". Split out from
/// [`Config::active_config_path`] so the precedence is testable against a
/// scratch directory rather than the process environment.
fn pick_front_end(root: &std::path::Path) -> Option<(std::path::PathBuf, ConfigFormat)> {
    // Precedence, first existing file wins:
    //  1. `hypr/hyprpad.lua`     — the hypr-ecosystem convention (`hypridle.conf`,
    //                              `hyprlock.conf`, … all live in `~/.config/hypr/`)
    //  2. `hyprpad/config.lua`   — the pre-convention Lua location
    //  3. `hyprpad/config.toml`  — the TOML front-end
    let candidates = [
        (root.join("hypr").join("hyprpad.lua"), ConfigFormat::Lua),
        (root.join("hyprpad").join("config.lua"), ConfigFormat::Lua),
        (root.join("hyprpad").join("config.toml"), ConfigFormat::Toml),
    ];
    candidates.into_iter().find(|(p, _)| p.exists())
}

/// Drop the button bindings whose guard does not pass in `st`. Generic over the
/// binding — a bare button's [`ButtonAction`], an OSK helper's keycode — since
/// the guard is keyed by the button either way.
fn filter_buttons<V: Clone>(
    map: &HashMap<report::Button, V>,
    guards: &HashMap<report::Button, Guard>,
    st: &ModeState,
) -> HashMap<report::Button, V> {
    map.iter()
        .filter(|(b, _)| guards.get(b).unwrap_or(&ALWAYS).allows(st))
        .map(|(b, v)| (*b, v.clone()))
        .collect()
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

/// Parse a whole number of milliseconds (`0` = off), rejecting the negative and
/// fractional values a timer cannot mean.
fn parse_millis(s: &str) -> Result<u64, String> {
    let t = s.trim();
    t.parse::<u64>()
        .map_err(|_| format!("expected a whole number of milliseconds (0 = off), got '{t}'"))
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
        // The default keyboard chord is guide+y -> toggle the bottom deck,
        // floating over the desktop (overlay).
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::ToggleKeyboard { mode: OskMode::Bottom, reflow: false }
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
"guide+x" = "osk split reflow"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::ToggleKeyboard { mode: OskMode::Bottom, reflow: false }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleKeyboard { mode: OskMode::Bottom, reflow: false }
        );
        // Presentation defaults to overlay (float); `reflow` opts in.
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::B)),
            Action::ToggleKeyboard { mode: OskMode::Split, reflow: false }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::ToggleKeyboard { mode: OskMode::Split, reflow: true }
        );
        // An unknown option is a reported error, not a silent default.
        assert!(Config::from_toml_str("[bindings]\n\"guide+y\" = \"keyboard sideways\"\n")
            .unwrap_err()
            .contains("unknown keyboard option"));
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
            assert!(key_code(name).unwrap() <= 255);
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
        use ButtonAction::Hold;
        assert_eq!(b.get(&Button::DpadUp), Some(&Hold(103)));
        assert_eq!(b.get(&Button::DpadDown), Some(&Hold(108)));
        assert_eq!(b.get(&Button::DpadLeft), Some(&Hold(105)));
        assert_eq!(b.get(&Button::DpadRight), Some(&Hold(106)));
        // The mouse clicks that used to be hardwired in the cursor driver: pad
        // click and a full R2 pull are a left click, a full L2 pull a right one.
        assert_eq!(b.get(&Button::PadRightClick), Some(&Hold(BTN_LEFT)));
        assert_eq!(b.get(&Button::TriggerR2Full), Some(&Hold(BTN_LEFT)));
        assert_eq!(b.get(&Button::TriggerL2Full), Some(&Hold(BTN_RIGHT)));
        // Four arrows and three clicks, and nothing else, by default — and
        // every one of them is held with its button, not fired.
        assert_eq!(b.len(), 7);
        assert!(b.values().all(|a| matches!(a, Hold(_))));
    }

    #[test]
    fn a_config_with_its_own_buttons_replaces_the_default_clicks_too() {
        // The rule is unchanged: listing [buttons] replaces the whole default
        // table. A config that wants the clicks lists them (the shipped sample
        // does); one that leaves them out has no clicks, by choice.
        let c = Config::from_toml_str("[buttons]\ndpad_up = \"key up\"\n").expect("parse");
        assert_eq!(c.buttons().len(), 1);
        assert_eq!(c.buttons().get(&Button::PadRightClick), None);
    }

    #[test]
    fn mouse_actions_parse_to_key_actions_in_the_btn_code_space() {
        assert_eq!(Action::parse("mouse left"), Ok(Action::Key(272)));
        assert_eq!(Action::parse("click right"), Ok(Action::Key(273)));
        assert_eq!(Action::parse("MOUSE Middle"), Ok(Action::Key(274)));
        // The evdev spelling works through `key` too: same code, same action.
        assert_eq!(Action::parse("key btn_left"), Ok(Action::Key(272)));
        assert_eq!(Action::parse("key btn_left"), Action::parse("mouse left"));
        assert!(Action::parse("mouse").unwrap_err().contains("mouse needs a button"));
        let e = Action::parse("mouse side").unwrap_err();
        assert!(e.contains("unknown mouse button 'side'"), "{e}");
        assert!(e.contains("left|right|middle"), "should list the choices: {e}");
    }

    #[test]
    fn mouse_code_accepts_every_alias_case_insensitively() {
        for name in ["left", "LEFT", "btn_left", "lmb", "1", " Left "] {
            assert_eq!(mouse_code(name), Ok(BTN_LEFT), "{name:?}");
        }
        for name in ["right", "btn_right", "RMB", "2"] {
            assert_eq!(mouse_code(name), Ok(BTN_RIGHT), "{name:?}");
        }
        for name in ["middle", "btn_middle", "mmb", "3"] {
            assert_eq!(mouse_code(name), Ok(BTN_MIDDLE), "{name:?}");
        }
        assert!(mouse_code("4").is_err());
        assert!(mouse_code("").is_err());
        assert!(is_mouse_code(BTN_LEFT) && is_mouse_code(BTN_MIDDLE));
        assert!(!is_mouse_code(103) && !is_mouse_code(0));
    }

    #[test]
    fn buttons_accept_a_mouse_button_but_osk_buttons_reject_it() {
        let c = Config::from_toml_str("[buttons]\nr2 = \"mouse left\"\nl2 = \"click rmb\"\n")
            .expect("parse");
        assert_eq!(c.buttons().get(&Button::TriggerR2Full), Some(&ButtonAction::Hold(BTN_LEFT)));
        assert_eq!(c.buttons().get(&Button::TriggerL2Full), Some(&ButtonAction::Hold(BTN_RIGHT)));

        for value in ["mouse left", "key btn_right", "click 3"] {
            let e = Config::from_toml_str(&format!("[osk_buttons]\ny = \"{value}\"\n"))
                .unwrap_err();
            assert!(e.contains("line 2"), "{e}");
            assert!(e.contains("mouse button makes no sense there"), "{e}");
            assert!(e.contains("[buttons]"), "should point at the right section: {e}");
        }
        // A real key is still fine there.
        assert!(Config::from_toml_str("[osk_buttons]\ny = \"key space\"\n").is_ok());
    }

    #[test]
    fn osk_actions_parse_the_keyboards_own_verbs_and_keys() {
        use OskAction::*;
        assert_eq!(OskAction::parse("osk commit"), Ok(Commit));
        assert_eq!(OskAction::parse("osk type"), Ok(Commit));
        assert_eq!(OskAction::parse("OSK Shift"), Ok(Shift));
        assert_eq!(OskAction::parse("osk dismiss"), Ok(Dismiss));
        assert_eq!(OskAction::parse("osk close"), Ok(Dismiss));
        assert_eq!(OskAction::parse("osk hide"), Ok(Dismiss));
        assert_eq!(OskAction::parse("key space"), Ok(Key(57)));
        assert_eq!(OskAction::parse("  key enter "), Ok(Key(28)));
        assert_eq!(OskAction::parse("none"), Ok(None));
        assert_eq!(OskAction::parse(""), Ok(None));

        let e = OskAction::parse("osk frobnicate").unwrap_err();
        assert!(e.contains("commit|shift|dismiss"), "{e}");
        assert!(OskAction::parse("osk").is_err(), "a bare `osk` is not a binding");
        for not_a_binding in ["exec foo", "keyboard split", "workspace +1", "set_mode game"] {
            let e = OskAction::parse(not_a_binding).unwrap_err();
            assert!(e.contains("osk_buttons values must be"), "{not_a_binding}: {e}");
        }
        assert!(OskAction::parse("key frobnicate").unwrap_err().contains("unknown key"));
    }

    #[test]
    fn the_keyboards_own_verbs_are_not_chord_actions() {
        // `osk` alone still toggles the keyboard, and takes its layout words…
        assert_eq!(
            Action::parse("osk"),
            Ok(Action::ToggleKeyboard { mode: crate::osk::OskMode::Bottom, reflow: false })
        );
        assert_eq!(
            Action::parse("osk split reflow"),
            Ok(Action::ToggleKeyboard { mode: crate::osk::OskMode::Split, reflow: true })
        );
        // …but its own verbs are bindings for [osk_buttons], and the error
        // says where they go instead of calling `commit` a bad layout.
        for verb in ["commit", "shift", "dismiss"] {
            let e = Action::parse(&format!("osk {verb}")).unwrap_err();
            assert!(e.contains("[osk_buttons]"), "{e}");
            assert!(e.contains("not an action"), "{e}");
        }
        assert!(Action::parse("keyboard commit").is_err());
    }

    #[test]
    fn the_keyboards_built_in_map_is_the_decks_and_a_config_layers_over_it() {
        use OskAction::*;
        let anywhere = ModeState::new("whatever", vec![]);
        // Names and codes agree with the key table the config grammar uses.
        for (b, what, _) in osk_builtins() {
            match (b, what) {
                (Button::TriggerR2Full, Key(c)) => assert_eq!(c, key_code("enter").unwrap()),
                (Button::Y, Key(c)) => assert_eq!(c, key_code("space").unwrap()),
                (Button::X, Key(c)) => assert_eq!(c, key_code("backspace").unwrap()),
                (Button::PadLeftClick | Button::PadRightClick, Commit) => {}
                (Button::TriggerL2Full, Shift) => {}
                (Button::B | Button::Menu, Dismiss) => {}
                other => panic!("unexpected built-in {other:?}"),
            }
        }
        assert_eq!(osk_builtins().count(), 8);

        // No config at all: the built-ins are what the keyboard gets.
        let bare = Config::from_toml_str("").expect("empty config");
        assert!(bare.osk_buttons().is_empty());
        let live = bare.osk_buttons_in(&anywhere);
        assert_eq!(live.get(&Button::TriggerL2Full), Some(&Shift));
        assert_eq!(live.get(&Button::TriggerR2Full), Some(&Key(28)));
        assert_eq!(live.get(&Button::PadLeftClick), Some(&Commit));
        assert_eq!(live.get(&Button::B), Some(&Dismiss));
        assert_eq!(live.len(), 8);

        // The owner's config restates Y and X and lists nothing else: the pad
        // clicks, the triggers and B/Menu are all still there.
        let owner = Config::from_toml_str("[osk_buttons]\ny = \"key space\"\nx = \"key backspace\"\n")
            .expect("parse");
        assert_eq!(owner.osk_buttons_in(&anywhere), live);

        // A config rebinds a built-in, adds a button, and takes one away.
        let c = Config::from_toml_str(
            "[osk_buttons]\nr2 = \"osk commit\"\nl1 = \"key tab\"\nmenu = \"none\"\n",
        )
        .expect("parse");
        assert_eq!(c.osk_buttons().get(&Button::Menu), Some(&None), "the raw table keeps `none`");
        let live = c.osk_buttons_in(&anywhere);
        assert_eq!(live.get(&Button::TriggerR2Full), Some(&Commit), "rebound over the built-in");
        assert_eq!(live.get(&Button::BumperL1), Some(&Key(15)), "added");
        assert_eq!(live.get(&Button::Menu), Option::None, "taken away, nothing in its place");
        assert_eq!(live.get(&Button::B), Some(&Dismiss), "the other built-ins survive");
        assert_eq!(live.get(&Button::TriggerL2Full), Some(&Shift));
        assert_eq!(live.len(), 8, "one in, one out");
    }

    #[test]
    fn buttons_section_parses_aliases_and_reports_errors() {
        // Uses the same button-name aliases as the guide chords, plus a
        // non-D-pad button; a `key <name>` value is held with the button.
        let toml = r#"
[buttons]
dpad_up = "key up"
a = "key enter"
r1 = "key pageup"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        assert_eq!(c.buttons().get(&Button::DpadUp), Some(&ButtonAction::Hold(103)));
        assert_eq!(c.buttons().get(&Button::A), Some(&ButtonAction::Hold(28)));
        assert_eq!(c.buttons().get(&Button::BumperR1), Some(&ButtonAction::Hold(104)));
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
        // An unknown action is reported with the same message a chord gets.
        let e = Config::from_toml_str("[buttons]\ndpad_up = \"teleport home\"\n").unwrap_err();
        assert!(e.contains("line 2") && e.contains("unknown action 'teleport'"), "{e}");
    }

    #[test]
    fn buttons_take_any_action_held_or_fired() {
        use ButtonAction::{Fire, Hold};
        // The same grammar as [bindings]: a key or mouse button is held with
        // the button, everything else fires once on the press edge.
        let toml = r#"
[buttons]
a = "exec foo"
dpad_up = "key up"
x = "mouse left"
b = "keyboard split"
r1 = "workspace +1"
l1 = "fullscreen"
r4 = "set_mode edit"
l4 = "clear_mode"
r5 = "dispatch hl.dsp.window.close()"
"#;
        let c = Config::from_toml_str(toml).expect("parse");
        let b = c.buttons();
        assert_eq!(b.get(&Button::A), Some(&Fire(Action::Exec("foo".into()))));
        assert_eq!(b.get(&Button::DpadUp), Some(&Hold(103)));
        assert_eq!(b.get(&Button::X), Some(&Hold(BTN_LEFT)));
        assert_eq!(
            b.get(&Button::B),
            Some(&Fire(Action::ToggleKeyboard { mode: crate::osk::OskMode::Split, reflow: false }))
        );
        assert_eq!(
            b.get(&Button::BumperR1),
            Some(&Fire(Action::Workspace(WorkspaceTarget::Relative(1))))
        );
        assert_eq!(b.get(&Button::BumperL1), Some(&Fire(Action::ToggleFullscreen)));
        assert_eq!(b.get(&Button::GripR4), Some(&Fire(Action::SetMode("edit".into()))));
        assert_eq!(b.get(&Button::GripL4), Some(&Fire(Action::ClearMode)));
        assert_eq!(
            b.get(&Button::GripR5),
            Some(&Fire(Action::Dispatch("hl.dsp.window.close()".into())))
        );

        // `none` is not something a button can do: say so, at the line.
        let e = Config::from_toml_str("[buttons]\ny = \"none\"\n").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
        assert!(e.contains("a bare button needs an action"), "{e}");
        assert!(e.contains("remove its line"), "{e}");
        let e = Config::from_toml_str("[buttons]\ny = \"\"\n").unwrap_err();
        assert!(e.contains("a bare button needs an action"), "{e}");

        // The guard filter carries a fired action through unchanged.
        let anywhere = ModeState::new("whatever", vec![]);
        assert_eq!(c.buttons_in(&anywhere), *c.buttons());
    }

    #[test]
    fn button_action_classifies_keys_as_held_and_the_rest_as_fired() {
        use ButtonAction::{Fire, Hold};
        assert_eq!(ButtonAction::classify(Action::Key(103)), Ok(Hold(103)));
        assert_eq!(ButtonAction::classify(Action::Key(BTN_RIGHT)), Ok(Hold(BTN_RIGHT)));
        for a in [
            Action::Exec("x".into()),
            Action::Dispatch("y".into()),
            Action::Workspace(WorkspaceTarget::Number(3)),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Relative(-1)),
            Action::ToggleFullscreen,
            Action::ToggleKeyboard { mode: crate::osk::OskMode::Bottom, reflow: true },
            Action::SetMode("game".into()),
            Action::ClearMode,
        ] {
            assert_eq!(ButtonAction::classify(a.clone()), Ok(Fire(a)));
        }
        assert!(ButtonAction::classify(Action::None).is_err());
        // And back: what the sheet prints is what the config said.
        assert_eq!(Hold(103).to_action(), Action::Key(103));
        assert_eq!(Fire(Action::ClearMode).to_action(), Action::ClearMode);
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
    fn the_rescan_knobs_default_on_and_parse() {
        // Defaults: the event-driven title path on, the sweep at half a second.
        let d = Config::load_default();
        assert!(d.rescan_on_title_change());
        assert_eq!(d.process_rescan_ms(), DEFAULT_PROCESS_RESCAN_MS);
        assert_eq!(d.process_rescan_ms(), 500);

        let c = Config::from_toml_str(
            "[daemon]\nrescan_on_title_change = false\nprocess_rescan_ms = 250\n",
        )
        .unwrap();
        assert!(!c.rescan_on_title_change());
        assert_eq!(c.process_rescan_ms(), 250);

        // `0` is off, and the boolean's other spellings work here too.
        assert_eq!(
            Config::from_toml_str("[daemon]\nprocess_rescan_ms = 0\n").unwrap().process_rescan_ms(),
            0
        );
        assert!(Config::from_toml_str("[daemon]\nrescan_on_title_change = on\n")
            .unwrap()
            .rescan_on_title_change());

        // Values a timer cannot mean are reported, not rounded.
        for bad in ["-1", "0.5", "soon"] {
            assert!(
                Config::from_toml_str(&format!("[daemon]\nprocess_rescan_ms = {bad}\n"))
                    .unwrap_err()
                    .contains("whole number of milliseconds"),
                "{bad}"
            );
        }
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
    fn gamepad_defaults_and_parse() {
        // The built-in default config reproduces GamepadConfig::default exactly
        // (its `[gamepad]` block is living documentation of the defaults).
        let d = Config::load_default();
        assert_eq!(d.gamepad(), &GamepadConfig::default());
        // Forwarding is ON by default — it is the point of the project, and it
        // costs a non-gamer nothing because the device is created lazily. The
        // guide button is NOT forwarded: docs/08 makes it hyprpad's modifier.
        assert!(d.gamepad().enabled);
        assert!(!d.gamepad().forward_guide);
        assert!(d.gamepad().rumble);
        assert_eq!(d.gamepad().rumble_mode, RumbleMode::Native);
        assert_eq!(d.gamepad().rumble_intensity, 1.0);

        // A `[gamepad]` section overrides individual knobs; bindings still parse.
        let c = Config::from_toml_str(
            r#"
[gamepad]
enabled = true
forward_guide = true
rumble = false
rumble_mode = pulse
rumble_intensity = 0.25
[bindings]
"guide+a" = "fullscreen"
"#,
        )
        .expect("parse");
        assert!(c.gamepad().forward_guide);
        assert!(!c.gamepad().rumble);
        assert_eq!(c.gamepad().rumble_mode, RumbleMode::Pulse);
        assert_eq!(c.gamepad().rumble_intensity, 0.25);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::ToggleFullscreen
        );

        // The master switch and the key aliases; unspecified knobs keep their
        // defaults.
        let c = Config::from_toml_str("[gamepad]\nenabled = off\n").unwrap();
        assert!(!c.gamepad().enabled);
        assert!(c.gamepad().rumble, "untouched knobs keep the default");
        let c = Config::from_toml_str("[gamepad]\nguide = yes\nff = no\nrumble_gain = 2\n").unwrap();
        assert!(c.gamepad().forward_guide);
        assert!(!c.gamepad().rumble);
        assert_eq!(c.gamepad().rumble_intensity, 2.0);
        assert_eq!(
            Config::from_toml_str("[gamepad]\nmode = native\n").unwrap().gamepad().rumble_mode,
            RumbleMode::Native
        );

        // Bad values and unknown keys are reported, not silently ignored — a
        // typo must never leave the gamepad half-configured.
        assert!(Config::from_toml_str("[gamepad]\nenabled = maybe\n")
            .unwrap_err()
            .contains("boolean"));
        assert!(Config::from_toml_str("[gamepad]\nrumble_intensity = hard\n")
            .unwrap_err()
            .contains("expected a number"));
        assert!(Config::from_toml_str("[gamepad]\nrumble_mode = shake\n")
            .unwrap_err()
            .contains("unknown rumble mode"));
        assert!(Config::from_toml_str("[gamepad]\nturbo = true\n")
            .unwrap_err()
            .contains("unknown [gamepad] setting"));
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

    #[test]
    fn front_end_precedence_convention_then_legacy_then_toml() {
        // A scratch config ROOT (stands in for ~/.config) with both subdirs.
        let root = std::env::temp_dir().join(format!(
            "hyprpad-front-end-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let hypr = root.join("hypr");
        let pad = root.join("hyprpad");
        std::fs::create_dir_all(&hypr).unwrap();
        std::fs::create_dir_all(&pad).unwrap();

        // Nothing: built-in defaults.
        assert_eq!(pick_front_end(&root), None);

        // TOML only.
        std::fs::write(pad.join("config.toml"), "").unwrap();
        assert_eq!(
            pick_front_end(&root),
            Some((pad.join("config.toml"), ConfigFormat::Toml))
        );

        // The legacy Lua location beats TOML.
        std::fs::write(pad.join("config.lua"), "").unwrap();
        assert_eq!(
            pick_front_end(&root),
            Some((pad.join("config.lua"), ConfigFormat::Lua))
        );

        // The hypr-ecosystem convention (`~/.config/hypr/hyprpad.lua`, next to
        // hypridle.conf & co.) beats everything. Migrating is "write the file",
        // rolling back is "rename it".
        std::fs::write(hypr.join("hyprpad.lua"), "").unwrap();
        assert_eq!(
            pick_front_end(&root),
            Some((hypr.join("hyprpad.lua"), ConfigFormat::Lua))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_mode_and_clear_mode_parse_from_the_toml_grammar_too() {
        assert_eq!(Action::parse("set_mode game"), Ok(Action::SetMode("game".into())));
        assert_eq!(Action::parse("mode desktop"), Ok(Action::SetMode("desktop".into())));
        assert_eq!(Action::parse("clear_mode"), Ok(Action::ClearMode));
        assert!(Action::parse("set_mode").is_err());
    }

    #[test]
    fn guards_default_to_always_and_a_toml_config_declares_none() {
        let c = Config::load_default();
        assert!(c.modes().is_empty(), "TOML declares no modes");
        assert_eq!(c.default_mode(), "desktop");
        assert!(c.lua().is_none());
        assert!(!c.needs_focus_pid());
        assert_eq!(c.predicate_slots(), 0);

        // Everything is live in any mode, which is what makes adopting the mode
        // engine a no-op for an existing config.
        let anywhere = ModeState::new("whatever", vec![]);
        assert_eq!(c.buttons_in(&anywhere), *c.buttons());
        // The keyboard's table is the config's entries over the built-ins, and
        // with no guards every entry is in.
        let osk = c.osk_buttons_in(&anywhere);
        for (b, what) in c.osk_buttons() {
            assert_eq!(osk.get(b), Some(what));
        }
        assert!(osk.len() >= c.osk_buttons().len());
        assert!(c.cursor_enabled_in(&anywhere) && c.scroll_enabled_in(&anywhere));
        assert_eq!(c.gesture_guard(&GestureEvent::GuideChord(Button::A)), &Guard::Always);
    }

    #[test]
    fn guard_semantics() {
        let desktop = ModeState::new("desktop", vec![true, false]);
        assert!(Guard::Always.allows(&desktop));
        assert!(Guard::OnlyIn(vec!["desktop".into()]).allows(&desktop));
        assert!(!Guard::OnlyIn(vec!["game".into()]).allows(&desktop));
        assert!(Guard::NotIn(vec!["game".into()]).allows(&desktop));
        assert!(!Guard::NotIn(vec!["desktop".into()]).allows(&desktop));
        assert!(Guard::When(0).allows(&desktop));
        assert!(!Guard::When(1).allows(&desktop));
        // A predicate whose result never arrived (it errored or timed out) is
        // "no match", never "yes by default".
        assert!(!Guard::When(9).allows(&desktop));
    }
}
