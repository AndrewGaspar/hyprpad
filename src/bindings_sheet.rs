//! `hyprpad bindings` — the cheat sheet's data source.
//!
//! Loads the config through exactly the path the daemon uses
//! ([`Config::load`], so both the Lua and the TOML front-end are covered) and
//! renders it two ways:
//!
//!   * `hyprpad bindings` — an aligned text table, for a terminal.
//!   * `hyprpad bindings --json` — the same data as JSON, for the Quickshell
//!     cheat-sheet widget (`shell/hyprpad-cheatsheet.qml`), which re-runs this
//!     on every show so the sheet always mirrors the live config.
//!
//! Nothing here touches the controller or the running daemon: it re-reads the
//! config file, which is what makes it safe to run while `hyprpad run` owns the
//! device.
//!
//! # Labels
//!
//! A Lua binding can carry a description — `h.bind("guide+r1", "Workspace
//! right", h.workspace "+1")` — and that is what the sheet shows. The TOML
//! front-end has no way to spell one, so an undescribed binding gets a label
//! *derived* from its action ([`derive_label`]): `workspace +1` reads as
//! "Workspace next", `exec omarchy-menu` as "Omarchy menu". Every entry says
//! which it got, via `described`, so the widget can style an authored label
//! differently from a guessed one.
//!
//! # Modes, and the one the daemon hardwires
//!
//! `modes` reports every declared mode with what is live in it, and then one
//! more the config never wrote: [`OSK_MODE`], the context the on-screen
//! keyboard puts the pad in. It carries `builtin: true` and names the `section`
//! whose rows ARE that context, so a reader — or the widget's tab strip — can
//! show the keyboard's bindings as a mode without knowing what a keyboard is.
//! The rows the daemon hardwires there (the pads' cursors, the commit clicks,
//! the dismissal) are synthesized into that section, because a context whose
//! only listed bindings are two helper keys is not one you could use.
//!
//! # Control ids
//!
//! Every entry carries a `control`: a stable id for the *physical* control it
//! lives on (`r1`, `dpad_up`, `rstick`, `rpad`, …). That is the join key the
//! QML widget anchors its callouts on, so the diagram and this module can be
//! changed independently as long as the vocabulary holds.

use std::fmt::Write as _;

use crate::config::{Action, Config, ConfigFormat, Guard, ModeState, WorkspaceTarget};
use crate::gesture::{Stick, StickDir};
use crate::report::Button;

/// Entry point for the `bindings` subcommand.
pub fn run(json: bool) -> Result<(), String> {
    let config = Config::load()?;
    let source = Config::active_config_path();
    let sheet = Sheet::build(&config, source.as_ref().map(|(p, f)| (p.as_path(), *f)));
    print!("{}", if json { sheet.to_json() } else { sheet.to_text() });
    Ok(())
}

// ---------------------------------------------------------------------------
// The sheet model
// ---------------------------------------------------------------------------

/// Which family a binding belongs to — the sheet's four sections.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// A guide chord: hold Steam, then press.
    Guide,
    /// A bare button, pressed with no modifier.
    Button,
    /// An OSK helper, live only while the on-screen keyboard is up.
    OskButton,
    /// A trackpad's ambient behaviour (cursor, scroll) — no press involved.
    Ambient,
}

impl Section {
    fn wire(self) -> &'static str {
        match self {
            Section::Guide => "guide",
            Section::Button => "button",
            Section::OskButton => "osk_button",
            Section::Ambient => "ambient",
        }
    }
}

/// One row of the cheat sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Which section this row belongs to.
    pub section: Section,
    /// What you actually press, in config spelling: `guide+r1`, `dpad_up`.
    /// Empty-ish concepts (the ambient pads) use the control id.
    pub chord: String,
    /// The physical control the callout anchors on: `r1`, `dpad_up`, `rstick`.
    pub control: String,
    /// A stick flick's direction, `None` for everything else.
    pub direction: Option<&'static str>,
    /// The control's human name: "R1", "D-pad up", "Right stick".
    pub control_label: String,
    /// The human label for what the binding does.
    pub label: String,
    /// Whether `label` came from the config (`true`) or was derived from the
    /// action (`false`).
    pub described: bool,
    /// The action in canonical config spelling: `workspace +1`, `key up`.
    pub action: String,
    /// The action's verb, for styling: `workspace`, `exec`, `key`, …
    pub action_kind: &'static str,
    /// When this binding is live.
    pub guard: Guard,
    /// Sort rank, following the physical layout.
    rank: u32,
}

impl Entry {
    /// A one-line rendering of the guard, for the text table and the widget.
    pub fn guard_label(&self) -> String {
        guard_label(&self.guard)
    }
}

/// A declared mode, plus what is live in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeRow {
    pub name: String,
    /// Whether raw input is forwarded to the game in this mode.
    pub forward: bool,
    /// Whether the mode has a selection rule (a `:when` predicate).
    pub has_rule: bool,
    /// Whether this is the fallback mode.
    pub default: bool,
    /// Whether the daemon hardwires this context rather than the config
    /// declaring it — see [`OSK_MODE`].
    pub builtin: bool,
    /// The section whose rows ARE this context, for a built-in one: the reader
    /// is in it exactly when those bindings are the ones that work. `None` for
    /// a declared mode, whose rows are decided by each binding's guard.
    pub section: Option<&'static str>,
    /// The chords that are unconditionally live in this mode.
    pub active: Vec<String>,
    /// The chords whose guard is a `:when` predicate, so liveness cannot be
    /// decided without a live context.
    pub conditional: Vec<String>,
}

/// The name of the context the on-screen keyboard puts the pad in.
///
/// Not a declared mode — the daemon does not switch modes for the keyboard —
/// but from the reader's side it is one: while the keyboard is up it owns both
/// pads, the `[osk_buttons]` helpers type through it, and every other binding
/// is suppressed (`route_osk` in `run.rs`). The sheet shows it as a mode
/// because that is what it behaves like.
pub const OSK_MODE: &str = "osk";

/// Everything `hyprpad bindings` prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheet {
    /// The config file the data came from, and its front-end. `None` when
    /// neither config file exists and the built-in defaults are in play.
    pub source: Option<(String, ConfigFormat)>,
    pub entries: Vec<Entry>,
    pub modes: Vec<ModeRow>,
    pub default_mode: String,
}

impl Sheet {
    /// Read a loaded [`Config`] into the sheet model.
    pub fn build(c: &Config, source: Option<(&std::path::Path, ConfigFormat)>) -> Sheet {
        let mut entries = Vec::new();

        for (key, action) in &c.bindings {
            let (control, control_label, direction, rank) = gesture_control(key);
            entries.push(entry(
                Section::Guide,
                gesture_chord(key),
                control,
                control_label,
                direction,
                rank,
                action,
                c.binding_descs.get(key),
                c.binding_guards.get(key),
            ));
        }

        for (section, map, descs, guards) in [
            (Section::Button, &c.buttons, &c.button_descs, &c.button_guards),
            (Section::OskButton, &c.osk_buttons, &c.osk_button_descs, &c.osk_button_guards),
        ] {
            for (btn, code) in map {
                entries.push(entry(
                    section,
                    button_name(*btn).to_string(),
                    button_name(*btn).to_string(),
                    button_label(*btn),
                    None,
                    button_rank(*btn),
                    &Action::Key(*code),
                    descs.get(btn),
                    guards.get(btn),
                ));
            }
        }

        // A button bound once per mode contributes one row per binding, each
        // carrying its own guard: `b` reads as Backspace on the desktop and as
        // "Close cheat sheet" under the sheet, and the mode rows below sort the
        // two into the modes they are live in.
        for alt in &c.button_alts {
            entries.push(entry(
                Section::Button,
                button_name(alt.button).to_string(),
                button_name(alt.button).to_string(),
                button_label(alt.button),
                None,
                button_rank(alt.button),
                &Action::Key(alt.code),
                alt.desc.as_ref(),
                Some(&alt.guard),
            ));
        }

        // The trackpads' ambient behaviour. Not a binding you press, but the
        // diagram has two pads on it and a sheet that leaves them blank is
        // lying about the biggest two controls on the puck.
        entries.push(ambient(
            "rpad",
            "Right trackpad",
            "Move the cursor",
            format!("cursor sens {:.2}", c.cursor().sens),
            &c.cursor_guard,
            RANK_PAD + 1,
        ));
        let (scroll_label, scroll_mode) = match c.scroll().mode {
            crate::config::ScrollMode::Off => ("Scrolling off", "off"),
            crate::config::ScrollMode::Swipe => ("Scroll (swipe)", "swipe"),
            crate::config::ScrollMode::Circular => ("Scroll (circular)", "circular"),
        };
        entries.push(ambient(
            "lpad",
            "Left trackpad",
            scroll_label,
            format!("scroll {scroll_mode}"),
            &c.scroll_guard,
            RANK_PAD,
        ));

        // What the on-screen keyboard hardwires. None of this is a binding
        // anyone can write — `route_osk` owns the pads, the clicks and the
        // dismissal itself — so a sheet that listed only `[osk_buttons]` would
        // describe the keyboard's context as two helper keys and nothing else,
        // which is not a context you could actually use.
        for (control, control_label, rank) in [
            ("lpad", "Left trackpad", RANK_PAD),
            ("rpad", "Right trackpad", RANK_PAD + 1),
        ] {
            entries.push(osk_builtin(
                control,
                control_label.to_string(),
                "Move the keyboard's cursor",
                "osk cursor",
                rank,
            ));
        }
        for (b, label, action) in [
            (Button::PadLeftClick, "Type the key under the cursor", "osk commit"),
            (Button::PadRightClick, "Type the key under the cursor", "osk commit"),
            (Button::TriggerL2Full, "Type the key under the cursor", "osk commit"),
            (Button::TriggerR2Full, "Type the key under the cursor", "osk commit"),
            (Button::B, "Close the keyboard", "osk close"),
            (Button::Menu, "Close the keyboard", "osk close"),
        ] {
            entries.push(osk_builtin(
                button_name(b),
                button_label(b),
                label,
                action,
                button_rank(b),
            ));
        }

        entries.sort_by(|a, b| {
            (section_rank(a.section), a.rank, &a.chord).cmp(&(
                section_rank(b.section),
                b.rank,
                &b.chord,
            ))
        });

        let default_mode = c.default_mode().to_string();
        let mut modes: Vec<ModeRow> = c
            .modes()
            .iter()
            .map(|m| {
                // A snapshot with no predicate results: `Guard::When` reads as
                // false there (config.rs's "a broken predicate is not a match"
                // rule), which is exactly the "cannot decide statically"
                // answer we want — those chords go in `conditional` instead.
                let st = ModeState::new(m.name.clone(), Vec::new());
                let mut active = Vec::new();
                let mut conditional = Vec::new();
                for e in &entries {
                    match e.guard {
                        Guard::When(_) => conditional.push(e.chord.clone()),
                        _ if e.guard.allows(&st) => active.push(e.chord.clone()),
                        _ => {}
                    }
                }
                ModeRow {
                    name: m.name.clone(),
                    forward: m.forward,
                    has_rule: m.rule.is_some(),
                    default: m.name == default_mode,
                    builtin: false,
                    section: None,
                    active,
                    conditional,
                }
            })
            .collect();

        // The keyboard's context, as a mode the sheet can show. Its rows are a
        // whole SECTION rather than a guard evaluation: the reader is in this
        // context exactly when the OSK helpers are the bindings that work, and
        // saying so in the data is what keeps the widget from having to know
        // what an on-screen keyboard is.
        let osk: Vec<&Entry> =
            entries.iter().filter(|e| e.section == Section::OskButton).collect();
        modes.push(ModeRow {
            name: OSK_MODE.to_string(),
            forward: false,
            has_rule: false,
            default: false,
            builtin: true,
            section: Some(Section::OskButton.wire()),
            active: osk
                .iter()
                .filter(|e| !matches!(e.guard, Guard::When(_)))
                .map(|e| e.chord.clone())
                .collect(),
            conditional: osk
                .iter()
                .filter(|e| matches!(e.guard, Guard::When(_)))
                .map(|e| e.chord.clone())
                .collect(),
        });

        Sheet {
            source: source.map(|(p, f)| (p.display().to_string(), f)),
            entries,
            modes,
            default_mode,
        }
    }

    /// The rows of one section, in layout order.
    pub fn section(&self, s: Section) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.section == s)
    }
}

/// Sections print in this order.
fn section_rank(s: Section) -> u32 {
    match s {
        Section::Guide => 0,
        Section::Button => 1,
        Section::OskButton => 2,
        Section::Ambient => 3,
    }
}

#[allow(clippy::too_many_arguments)]
fn entry(
    section: Section,
    chord: String,
    control: String,
    control_label: String,
    direction: Option<&'static str>,
    rank: u32,
    action: &Action,
    desc: Option<&String>,
    guard: Option<&Guard>,
) -> Entry {
    Entry {
        section,
        chord,
        control,
        direction,
        control_label,
        label: desc.cloned().unwrap_or_else(|| derive_label(action)),
        described: desc.is_some(),
        action: action_string(action),
        action_kind: action_kind(action),
        guard: guard.cloned().unwrap_or(Guard::Always),
        rank,
    }
}

/// A row for something the daemon hardwires while the on-screen keyboard is up
/// ([`crate::run`]'s `route_osk`). Not a binding: there is no config spelling
/// for it, so `action` describes the behaviour instead of naming an action, and
/// the label is authored here rather than derived. Guarded to [`OSK_MODE`], so
/// it belongs to the keyboard's context and to no declared mode.
fn osk_builtin(
    control: &str,
    control_label: String,
    label: &str,
    action: &str,
    rank: u32,
) -> Entry {
    Entry {
        section: Section::OskButton,
        chord: control.to_string(),
        control: control.to_string(),
        direction: None,
        control_label,
        label: label.to_string(),
        described: true,
        action: action.to_string(),
        action_kind: "builtin",
        guard: Guard::OnlyIn(vec![OSK_MODE.to_string()]),
        rank,
    }
}

fn ambient(
    control: &str,
    control_label: &str,
    label: &str,
    action: String,
    guard: &Guard,
    rank: u32,
) -> Entry {
    Entry {
        section: Section::Ambient,
        chord: control.to_string(),
        control: control.to_string(),
        direction: None,
        control_label: control_label.to_string(),
        label: label.to_string(),
        described: false,
        action,
        action_kind: "ambient",
        guard: guard.clone(),
        rank,
    }
}

// ---------------------------------------------------------------------------
// Control vocabulary — the ids the QML diagram anchors callouts on
// ---------------------------------------------------------------------------

const RANK_FACE: u32 = 0;
const RANK_DPAD: u32 = 10;
const RANK_SHOULDER: u32 = 20;
const RANK_GRIP: u32 = 30;
const RANK_STICK: u32 = 40;
const RANK_PAD: u32 = 50;
const RANK_SYSTEM: u32 = 60;
const RANK_GUIDE: u32 = 70;

/// The canonical config spelling of a button — the inverse of
/// [`crate::config::parse_button`], so a chord printed here parses back.
pub fn button_name(b: Button) -> &'static str {
    use Button::*;
    match b {
        A => "a",
        B => "b",
        X => "x",
        Y => "y",
        BumperR1 => "r1",
        BumperL1 => "l1",
        TriggerR2Full => "r2",
        TriggerL2Full => "l2",
        R3 => "r3",
        L3 => "l3",
        GripR4 => "r4",
        GripR5 => "r5",
        GripL4 => "l4",
        GripL5 => "l5",
        DpadUp => "dpad_up",
        DpadDown => "dpad_down",
        DpadLeft => "dpad_left",
        DpadRight => "dpad_right",
        Menu => "menu",
        View => "view",
        QuickAccess => "quickaccess",
        PadRightClick => "rpad_click",
        PadLeftClick => "lpad_click",
        Steam => "steam",
        // Touch and capacitive flags are not bindable; they have no config
        // spelling. Named anyway so this function stays total.
        PadRightTouch => "rpad_touch",
        PadLeftTouch => "lpad_touch",
        Cap0 => "cap0",
        Cap1 => "cap1",
        Cap2 => "cap2",
        Cap3 => "cap3",
    }
}

/// The button's human name, as it reads on the sheet.
pub fn button_label(b: Button) -> String {
    use Button::*;
    match b {
        A => "A",
        B => "B",
        X => "X",
        Y => "Y",
        BumperR1 => "R1 bumper",
        BumperL1 => "L1 bumper",
        TriggerR2Full => "R2 trigger (full pull)",
        TriggerL2Full => "L2 trigger (full pull)",
        R3 => "Right stick click",
        L3 => "Left stick click",
        GripR4 => "R4 grip",
        GripR5 => "R5 grip",
        GripL4 => "L4 grip",
        GripL5 => "L5 grip",
        DpadUp => "D-pad up",
        DpadDown => "D-pad down",
        DpadLeft => "D-pad left",
        DpadRight => "D-pad right",
        Menu => "Menu",
        View => "View",
        QuickAccess => "Quick Access",
        PadRightClick => "Right pad click",
        PadLeftClick => "Left pad click",
        Steam => "Steam",
        PadRightTouch => "Right pad touch",
        PadLeftTouch => "Left pad touch",
        Cap0 | Cap1 | Cap2 | Cap3 => "Capacitive sensor",
    }
    .to_string()
}

fn button_rank(b: Button) -> u32 {
    use Button::*;
    match b {
        A => RANK_FACE,
        B => RANK_FACE + 1,
        X => RANK_FACE + 2,
        Y => RANK_FACE + 3,
        DpadUp => RANK_DPAD,
        DpadDown => RANK_DPAD + 1,
        DpadLeft => RANK_DPAD + 2,
        DpadRight => RANK_DPAD + 3,
        BumperL1 => RANK_SHOULDER,
        BumperR1 => RANK_SHOULDER + 1,
        TriggerL2Full => RANK_SHOULDER + 2,
        TriggerR2Full => RANK_SHOULDER + 3,
        GripL4 => RANK_GRIP,
        GripL5 => RANK_GRIP + 1,
        GripR4 => RANK_GRIP + 2,
        GripR5 => RANK_GRIP + 3,
        L3 => RANK_STICK,
        R3 => RANK_STICK + 1,
        PadLeftClick => RANK_PAD + 2,
        PadRightClick => RANK_PAD + 3,
        Menu => RANK_SYSTEM,
        View => RANK_SYSTEM + 1,
        QuickAccess => RANK_SYSTEM + 2,
        Steam => RANK_GUIDE,
        _ => RANK_GUIDE + 1,
    }
}

/// The config spelling of a gesture key: `guide+r1`, `guide+stick_right`,
/// `guide_tap`, `guide_hold`.
fn gesture_chord(k: &crate::config::GestureKey) -> String {
    use crate::config::GestureKey as K;
    match k {
        K::Chord(b) => format!("guide+{}", button_name(*b)),
        K::Flick(stick, dir) => format!(
            "guide+{}stick_{}",
            match stick {
                Stick::Left => "l",
                Stick::Right => "r",
            },
            dir_name(*dir)
        ),
        K::Tap => "guide_tap".to_string(),
        K::Hold => "guide_hold".to_string(),
    }
}

/// `(control id, human name, flick direction, sort rank)` for a gesture key.
fn gesture_control(k: &crate::config::GestureKey) -> (String, String, Option<&'static str>, u32) {
    use crate::config::GestureKey as K;
    match k {
        K::Chord(b) => (button_name(*b).to_string(), button_label(*b), None, button_rank(*b)),
        K::Flick(stick, dir) => {
            let (id, name, base) = match stick {
                Stick::Left => ("lstick", "Left stick", RANK_STICK + 2),
                Stick::Right => ("rstick", "Right stick", RANK_STICK + 6),
            };
            let d = dir_name(*dir);
            (
                id.to_string(),
                format!("{name} {}", arrow(*dir)),
                Some(d),
                base + dir_rank(*dir),
            )
        }
        K::Tap => ("steam".to_string(), "Steam tap".to_string(), None, RANK_GUIDE),
        K::Hold => ("steam".to_string(), "Steam hold".to_string(), None, RANK_GUIDE + 1),
    }
}

fn dir_name(d: StickDir) -> &'static str {
    match d {
        StickDir::Up => "up",
        StickDir::Down => "down",
        StickDir::Left => "left",
        StickDir::Right => "right",
    }
}

fn dir_rank(d: StickDir) -> u32 {
    match d {
        StickDir::Up => 0,
        StickDir::Down => 1,
        StickDir::Left => 2,
        StickDir::Right => 3,
    }
}

fn arrow(d: StickDir) -> char {
    match d {
        StickDir::Up => '↑',
        StickDir::Down => '↓',
        StickDir::Left => '←',
        StickDir::Right => '→',
    }
}

// ---------------------------------------------------------------------------
// Action rendering
// ---------------------------------------------------------------------------

/// The action in canonical config spelling — what you would write in a
/// `config.toml`, and what [`Action::parse`] reads back.
pub fn action_string(a: &Action) -> String {
    match a {
        Action::Workspace(t) => format!("workspace {}", target(t)),
        Action::MoveWindowToWorkspace(t) => format!("movetoworkspace {}", target(t)),
        Action::ToggleFullscreen => "fullscreen".to_string(),
        Action::Exec(c) => format!("exec {c}"),
        Action::Dispatch(p) => format!("dispatch {p}"),
        Action::ToggleKeyboard { mode, reflow } => {
            let m = match mode {
                crate::osk::OskMode::Bottom => "bottom",
                crate::osk::OskMode::Split => "split",
            };
            if *reflow {
                format!("keyboard {m} reflow")
            } else {
                format!("keyboard {m}")
            }
        }
        Action::Key(code) => format!("key {}", key_name(*code)),
        Action::SetMode(m) => format!("set_mode {m}"),
        Action::ClearMode => "clear_mode".to_string(),
        Action::None => "none".to_string(),
    }
}

/// The action's verb, for the widget to colour by.
pub fn action_kind(a: &Action) -> &'static str {
    match a {
        Action::Workspace(_) => "workspace",
        Action::MoveWindowToWorkspace(_) => "movetoworkspace",
        Action::ToggleFullscreen => "fullscreen",
        Action::Exec(_) => "exec",
        Action::Dispatch(_) => "dispatch",
        Action::ToggleKeyboard { .. } => "keyboard",
        Action::Key(_) => "key",
        Action::SetMode(_) => "set_mode",
        Action::ClearMode => "clear_mode",
        Action::None => "none",
    }
}

fn target(t: &WorkspaceTarget) -> String {
    match t {
        WorkspaceTarget::Relative(n) => format!("{n:+}"),
        WorkspaceTarget::Number(n) => n.to_string(),
        WorkspaceTarget::Named(s) => s.clone(),
    }
}

/// The `KEY_*` name a code was bound under — the inverse of
/// [`crate::config::key_code`]. Unknown codes print numerically rather than
/// panicking, so a future key added to one table cannot break the sheet.
pub fn key_name(code: u16) -> String {
    match code {
        103 => "up",
        108 => "down",
        105 => "left",
        106 => "right",
        28 => "enter",
        14 => "backspace",
        57 => "space",
        15 => "tab",
        1 => "escape",
        102 => "home",
        107 => "end",
        104 => "pageup",
        109 => "pagedown",
        _ => return format!("keycode {code}"),
    }
    .to_string()
}

/// A readable label for a binding the config gave no description.
///
/// Deliberately conservative: it re-words the action rather than inventing
/// intent, so a derived label is never *wrong*, only less specific than one the
/// owner would write. `described: false` on the entry marks it as derived.
pub fn derive_label(a: &Action) -> String {
    match a {
        Action::Workspace(t) => format!("Workspace {}", relative_words(t)),
        Action::MoveWindowToWorkspace(t) => {
            format!("Move window to workspace {}", relative_words(t))
        }
        Action::ToggleFullscreen => "Toggle fullscreen".to_string(),
        Action::Exec(cmd) => humanize_command(cmd),
        Action::Dispatch(p) => humanize_dispatch(p),
        Action::ToggleKeyboard { mode, reflow } => {
            let m = match mode {
                crate::osk::OskMode::Bottom => "bottom",
                crate::osk::OskMode::Split => "split",
            };
            if *reflow {
                format!("On-screen keyboard ({m}, reflow)")
            } else {
                format!("On-screen keyboard ({m})")
            }
        }
        Action::Key(code) => capitalize(&key_name(*code).replace("page", "page ")),
        Action::SetMode(m) => format!("Force {m} mode"),
        Action::ClearMode => "Back to automatic mode".to_string(),
        Action::None => "Unbound".to_string(),
    }
}

/// "next"/"previous" for the ±1 steps that make up almost every real config,
/// and a literal target for everything else.
fn relative_words(t: &WorkspaceTarget) -> String {
    match t {
        WorkspaceTarget::Relative(1) => "next".to_string(),
        WorkspaceTarget::Relative(-1) => "previous".to_string(),
        other => target(other),
    }
}

/// `omarchy-menu` -> "Omarchy menu"; `voxtype record toggle` -> "Voxtype record
/// toggle". Only the program name is de-dashed — an argument like `play-pause`
/// is quoted as written, because rewriting it would misrepresent the command.
fn humanize_command(cmd: &str) -> String {
    let cmd = cmd.trim();
    let (prog, args) = match cmd.split_once(char::is_whitespace) {
        Some((p, a)) => (p, a.trim()),
        None => (cmd, ""),
    };
    let base = prog.rsplit('/').next().unwrap_or(prog);
    let pretty = capitalize(&base.replace(['-', '_'], " "));
    if args.is_empty() {
        pretty
    } else {
        format!("{pretty} {args}")
    }
}

/// `hl.dsp.window.close()` -> "Window close". The HypXRland `hl.dsp.` prefix
/// and the trailing call parens are noise on a cheat sheet; what is left is the
/// dispatcher path, which reads as English already.
fn humanize_dispatch(payload: &str) -> String {
    let p = payload.trim();
    let p = p.strip_suffix("()").unwrap_or(p);
    let p = p.strip_prefix("hl.dsp.").or_else(|| p.strip_prefix("hl.")).unwrap_or(p);
    let words = p.replace(['.', '_'], " ");
    let words = words.trim();
    if words.is_empty() {
        format!("Dispatch {payload}")
    } else {
        capitalize(words)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A one-line rendering of a guard.
pub fn guard_label(g: &Guard) -> String {
    match g {
        Guard::Always => "always".to_string(),
        Guard::OnlyIn(m) => format!("only in {}", m.join(", ")),
        Guard::NotIn(m) => format!("except in {}", m.join(", ")),
        Guard::When(i) => format!("when (lua predicate #{i})"),
    }
}

// ---------------------------------------------------------------------------
// JSON — emitted by hand; the daemon carries no serialization dependency
// ---------------------------------------------------------------------------

impl Sheet {
    /// The widget's wire format. Schema version 1; see the module docs and
    /// `shell/README.md`.
    pub fn to_json(&self) -> String {
        let mut o = String::with_capacity(4096);
        o.push_str("{\n  \"version\": 1,\n  \"source\": ");
        match &self.source {
            Some((path, format)) => {
                o.push_str("{\"path\": ");
                push_str(&mut o, path);
                o.push_str(", \"format\": ");
                push_str(&mut o, &format.to_string());
                o.push('}');
            }
            None => o.push_str("null"),
        }
        o.push_str(",\n  \"default_mode\": ");
        push_str(&mut o, &self.default_mode);
        o.push_str(",\n");

        for (name, section) in [
            ("guide_chords", Section::Guide),
            ("buttons", Section::Button),
            ("osk_buttons", Section::OskButton),
            ("ambient", Section::Ambient),
        ] {
            let _ = write!(o, "  \"{name}\": [");
            let mut first = true;
            for e in self.section(section) {
                o.push_str(if first { "\n" } else { ",\n" });
                first = false;
                e.push_json(&mut o);
            }
            o.push_str(if first { "],\n" } else { "\n  ],\n" });
        }

        o.push_str("  \"modes\": [");
        let mut first = true;
        for m in &self.modes {
            o.push_str(if first { "\n" } else { ",\n" });
            first = false;
            o.push_str("    {\"name\": ");
            push_str(&mut o, &m.name);
            let _ = write!(
                o,
                ", \"forward\": {}, \"has_rule\": {}, \"default\": {}, \"builtin\": {}",
                m.forward, m.has_rule, m.default, m.builtin
            );
            o.push_str(", \"section\": ");
            match m.section {
                Some(s) => push_str(&mut o, s),
                None => o.push_str("null"),
            }
            o.push_str(", \"active\": ");
            push_strs(&mut o, &m.active);
            o.push_str(", \"conditional\": ");
            push_strs(&mut o, &m.conditional);
            o.push('}');
        }
        o.push_str(if first { "]\n}\n" } else { "\n  ]\n}\n" });
        o
    }

    /// The terminal rendering: four aligned tables and a mode summary.
    pub fn to_text(&self) -> String {
        let mut o = String::with_capacity(4096);
        match &self.source {
            Some((path, format)) => {
                let _ = writeln!(o, "hyprpad bindings — {path} ({format})");
            }
            None => o.push_str("hyprpad bindings — built-in defaults (no config file)\n"),
        }

        for (title, section) in [
            ("Guide chords (hold Steam, then press)", Section::Guide),
            ("Bare buttons (no modifier)", Section::Button),
            ("OSK helpers (while the on-screen keyboard is up)", Section::OskButton),
            ("Trackpads (ambient)", Section::Ambient),
        ] {
            let rows: Vec<&Entry> = self.section(section).collect();
            let _ = write!(o, "\n{title}\n");
            if rows.is_empty() {
                o.push_str("  (none)\n");
                continue;
            }
            let w_chord = rows.iter().map(|e| e.chord.chars().count()).max().unwrap_or(0);
            let w_control =
                rows.iter().map(|e| e.control_label.chars().count()).max().unwrap_or(0);
            let w_label = rows.iter().map(|e| e.label.chars().count()).max().unwrap_or(0);
            let w_action = rows.iter().map(|e| e.action.chars().count()).max().unwrap_or(0);
            for e in rows {
                let guard = e.guard_label();
                let line = format!(
                    "  {:<w_chord$}  {:<w_control$}  {:<w_label$}  {:<w_action$}  {}",
                    e.chord,
                    e.control_label,
                    e.label,
                    e.action,
                    if guard == "always" { "" } else { &guard },
                );
                let _ = writeln!(o, "{}", line.trim_end());
            }
        }

        let _ = write!(
            o,
            "\nModes (rules evaluated in order, first match wins; default: {})\n",
            self.default_mode
        );
        if self.modes.iter().all(|m| m.builtin) {
            o.push_str("  (none declared — built-in game/desktop behaviour)\n");
        }
        let total = self.entries.len();
        for m in &self.modes {
            let mut notes = Vec::new();
            if m.builtin {
                notes.push("built-in".to_string());
            }
            if m.default {
                notes.push("default".to_string());
            }
            notes.push(if m.has_rule { "rule".to_string() } else { "no rule".to_string() });
            if m.forward {
                notes.push("forwards raw input".to_string());
            }
            let _ = writeln!(o, "  {} ({})", m.name, notes.join(", "));
            let _ = writeln!(o, "    live {}/{}: {}", m.active.len(), total, wrap(&m.active, 6));
            if !m.conditional.is_empty() {
                let _ = writeln!(o, "    conditional: {}", wrap(&m.conditional, 6));
            }
        }
        o
    }
}

impl Entry {
    fn push_json(&self, o: &mut String) {
        o.push_str("    {\"section\": ");
        push_str(o, self.section.wire());
        o.push_str(", \"chord\": ");
        push_str(o, &self.chord);
        o.push_str(", \"control\": ");
        push_str(o, &self.control);
        o.push_str(", \"control_label\": ");
        push_str(o, &self.control_label);
        o.push_str(", \"direction\": ");
        match self.direction {
            Some(d) => push_str(o, d),
            None => o.push_str("null"),
        }
        o.push_str(", \"label\": ");
        push_str(o, &self.label);
        let _ = write!(o, ", \"described\": {}", self.described);
        o.push_str(", \"action\": ");
        push_str(o, &self.action);
        o.push_str(", \"action_kind\": ");
        push_str(o, self.action_kind);
        o.push_str(", \"guard\": {\"kind\": ");
        match &self.guard {
            Guard::Always => {
                push_str(o, "always");
                o.push_str(", \"modes\": []");
            }
            Guard::OnlyIn(m) => {
                push_str(o, "only_in");
                o.push_str(", \"modes\": ");
                push_strs(o, m);
            }
            Guard::NotIn(m) => {
                push_str(o, "not_in");
                o.push_str(", \"modes\": ");
                push_strs(o, m);
            }
            Guard::When(i) => {
                push_str(o, "when");
                let _ = write!(o, ", \"modes\": [], \"predicate\": {i}");
            }
        }
        o.push_str("}, \"guard_label\": ");
        push_str(o, &self.guard_label());
        o.push('}');
    }
}

/// Append a JSON string literal.
fn push_str(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
}

/// Append a JSON array of strings.
fn push_strs<S: AsRef<str>>(o: &mut String, xs: &[S]) {
    o.push('[');
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            o.push_str(", ");
        }
        push_str(o, x.as_ref());
    }
    o.push(']');
}

/// Join `items` with commas, wrapping onto continuation lines indented by
/// `indent` so a long list stays readable in a terminal.
fn wrap<S: AsRef<str>>(items: &[S], indent: usize) -> String {
    const WIDTH: usize = 72;
    let mut out = String::new();
    let mut col = indent;
    for (i, s) in items.iter().enumerate() {
        let s = s.as_ref();
        if i > 0 {
            // The separator goes down *before* the break decision, so a wrapped
            // line ends with the comma rather than with trailing blanks.
            out.push(',');
            col += 1;
            if col + 1 + s.len() > WIDTH {
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                col = indent;
            } else {
                out.push(' ');
                col += 1;
            }
        }
        out.push_str(s);
        col += s.len();
    }
    if out.is_empty() {
        "(nothing)".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_from_toml(src: &str) -> Sheet {
        let c = Config::from_toml_str(src).expect("test config parses");
        Sheet::build(&c, None)
    }

    fn sheet_from_lua(src: &str) -> Sheet {
        let c = crate::lua_config::load_str(src, "test.lua").expect("test config loads");
        Sheet::build(&c, None)
    }

    fn find<'a>(s: &'a Sheet, chord: &str) -> &'a Entry {
        s.entries
            .iter()
            .find(|e| e.chord == chord && !is_builtin(e))
            .unwrap_or_else(|| panic!("no {chord}"))
    }

    /// The rows the daemon synthesizes for the keyboard's context share their
    /// chord with the control they sit on (`b`, `rpad`), so a test looking for
    /// what the CONFIG said has to step over them.
    fn is_builtin(e: &Entry) -> bool {
        e.action_kind == "builtin"
    }

    // --- derived labels ----------------------------------------------------

    #[test]
    fn relative_workspaces_read_as_next_and_previous() {
        assert_eq!(derive_label(&Action::Workspace(WorkspaceTarget::Relative(1))), "Workspace next");
        assert_eq!(
            derive_label(&Action::Workspace(WorkspaceTarget::Relative(-1))),
            "Workspace previous"
        );
        // A bigger jump has no English name; show the literal target.
        assert_eq!(derive_label(&Action::Workspace(WorkspaceTarget::Relative(3))), "Workspace +3");
        assert_eq!(derive_label(&Action::Workspace(WorkspaceTarget::Number(4))), "Workspace 4");
    }

    #[test]
    fn exec_labels_humanize_the_program_but_not_its_arguments() {
        assert_eq!(derive_label(&Action::Exec("omarchy-menu".into())), "Omarchy menu");
        assert_eq!(derive_label(&Action::Exec("/usr/bin/omarchy_menu".into())), "Omarchy menu");
        assert_eq!(
            derive_label(&Action::Exec("playerctl play-pause".into())),
            "Playerctl play-pause"
        );
        assert_eq!(
            derive_label(&Action::Exec("voxtype record toggle".into())),
            "Voxtype record toggle"
        );
    }

    #[test]
    fn dispatch_labels_drop_the_hyprland_lua_prefix() {
        assert_eq!(derive_label(&Action::Dispatch("hl.dsp.window.close()".into())), "Window close");
        assert_eq!(derive_label(&Action::Dispatch("hl.dsp.group.next()".into())), "Group next");
        // Anything that is not a `hl.dsp.` path is left recognizable.
        assert_eq!(derive_label(&Action::Dispatch("togglefloating".into())), "Togglefloating");
    }

    #[test]
    fn key_and_mode_labels() {
        assert_eq!(derive_label(&Action::Key(103)), "Up");
        assert_eq!(derive_label(&Action::Key(14)), "Backspace");
        assert_eq!(derive_label(&Action::Key(104)), "Page up");
        assert_eq!(derive_label(&Action::SetMode("desktop".into())), "Force desktop mode");
        assert_eq!(derive_label(&Action::ClearMode), "Back to automatic mode");
    }

    #[test]
    fn action_strings_round_trip_through_the_parser() {
        for a in [
            Action::Workspace(WorkspaceTarget::Relative(1)),
            Action::Workspace(WorkspaceTarget::Relative(-2)),
            Action::Workspace(WorkspaceTarget::Number(3)),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Named("special".into())),
            Action::ToggleFullscreen,
            Action::Exec("omarchy-menu".into()),
            Action::Dispatch("hl.dsp.window.close()".into()),
            Action::ToggleKeyboard { mode: crate::osk::OskMode::Split, reflow: false },
            Action::ToggleKeyboard { mode: crate::osk::OskMode::Bottom, reflow: true },
            Action::Key(103),
            Action::SetMode("game".into()),
            Action::ClearMode,
            Action::None,
        ] {
            let s = action_string(&a);
            assert_eq!(Action::parse(&s), Ok(a.clone()), "round trip of {s:?}");
        }
    }

    #[test]
    fn button_names_round_trip_through_the_parser() {
        use crate::report::Button::*;
        for b in [
            A, B, X, Y, BumperL1, BumperR1, TriggerL2Full, TriggerR2Full, L3, R3, GripL4, GripL5,
            GripR4, GripR5, DpadUp, DpadDown, DpadLeft, DpadRight, Menu, View, QuickAccess,
            PadLeftClick, PadRightClick,
        ] {
            assert_eq!(crate::config::parse_button(button_name(b)), Ok(b));
        }
    }

    // --- the TOML front-end ------------------------------------------------

    #[test]
    fn a_toml_config_gets_derived_labels() {
        let s = sheet_from_toml(
            r#"
            [bindings]
            "guide+r1" = "workspace +1"
            "guide+x" = "exec omarchy-menu"
            [buttons]
            dpad_up = "key up"
            [osk_buttons]
            y = "key space"
            "#,
        );
        let r1 = find(&s, "guide+r1");
        assert_eq!(r1.label, "Workspace next");
        assert!(!r1.described, "TOML cannot carry a description");
        assert_eq!(r1.control, "r1");
        assert_eq!(r1.control_label, "R1 bumper");
        assert_eq!(r1.section, Section::Guide);
        assert_eq!(find(&s, "guide+x").label, "Omarchy menu");
        assert_eq!(find(&s, "dpad_up").section, Section::Button);
        assert_eq!(find(&s, "y").section, Section::OskButton);
        // Every TOML binding is unguarded. (The keyboard's built-in rows are
        // not bindings and carry their own context's guard.)
        assert!(s.entries.iter().filter(|e| !is_builtin(e)).all(|e| e.guard == Guard::Always));
    }

    // --- the Lua front-end -------------------------------------------------

    #[test]
    fn a_lua_description_beats_the_derived_label() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+r1", "Workspace right", h.workspace "+1")
            h.bind("guide+l1", h.workspace "-1")
            h.button("dpad_up", "Arrow up", h.key "up"):only_in("desktop")
            h.osk_button("y", "Space bar", h.key "space")
            "#,
        );
        let r1 = find(&s, "guide+r1");
        assert_eq!(r1.label, "Workspace right");
        assert!(r1.described);
        // No description -> derived, and flagged as such.
        let l1 = find(&s, "guide+l1");
        assert_eq!(l1.label, "Workspace previous");
        assert!(!l1.described);
        // h.button / h.osk_button take a description too.
        let up = find(&s, "dpad_up");
        assert_eq!(up.label, "Arrow up");
        assert!(up.described);
        assert_eq!(up.guard, Guard::OnlyIn(vec!["desktop".into()]));
        assert_eq!(up.guard_label(), "only in desktop");
        assert_eq!(find(&s, "y").label, "Space bar");
    }

    #[test]
    fn modes_report_what_is_live_in_them() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx) return false end)
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+r1", h.workspace "+1")
            h.button("a", h.key "enter"):only_in("desktop")
            h.button("b", h.key "backspace"):not_in("game")
            h.osk_button("y", h.key "space"):when(function(ctx) return true end)
            "#,
        );
        let game = s.modes.iter().find(|m| m.name == "game").unwrap();
        assert!(game.forward);
        assert!(game.has_rule);
        assert!(!game.default);
        // The unguarded chord survives; the two desktop-guarded buttons do not.
        assert!(game.active.contains(&"guide+r1".to_string()));
        assert!(!game.active.contains(&"a".to_string()));
        assert!(!game.active.contains(&"b".to_string()));
        // A `:when` guard cannot be decided without a live context.
        assert_eq!(game.conditional, vec!["y".to_string()]);

        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(desktop.default);
        assert!(!desktop.has_rule);
        assert!(desktop.active.contains(&"a".to_string()));
        assert!(desktop.active.contains(&"b".to_string()));
    }

    #[test]
    fn the_keyboards_context_is_a_builtin_mode_carrying_the_daemons_own_rows() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.default_mode "desktop"
            h.osk_button("y", h.key "space")
            "#,
        );
        let osk = s.modes.iter().find(|m| m.name == OSK_MODE).expect("an osk mode row");
        assert!(osk.builtin);
        assert_eq!(osk.section, Some("osk_button"), "the section that IS this context");
        // Its rows are the whole section: what the config wrote, and what the
        // daemon hardwires — a context listing only `y` would be unusable.
        assert!(osk.active.contains(&"y".to_string()));
        for chord in ["lpad", "rpad", "lpad_click", "rpad_click", "l2", "r2", "b", "menu"] {
            assert!(osk.active.contains(&chord.to_string()), "no built-in row for {chord}");
        }
        // No declared mode claims them: while the keyboard is up, nothing else
        // is live, so they are not "live in desktop" in any useful sense.
        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(!desktop.active.contains(&"lpad_click".to_string()));
        assert!(!desktop.active.contains(&"menu".to_string()));

        let close = s
            .entries
            .iter()
            .find(|e| e.chord == "menu" && is_builtin(e))
            .expect("Menu closes the keyboard");
        assert_eq!(close.label, "Close the keyboard");
        assert_eq!(close.section, Section::OskButton);
        assert!(close.described, "a built-in row is authored, not derived");
        assert_eq!(close.guard, Guard::OnlyIn(vec![OSK_MODE.to_string()]));

        let j = s.to_json();
        assert!(j.contains("\"builtin\": true"), "modes carry `builtin` in\n{j}");
        assert!(j.contains("\"section\": \"osk_button\""), "and the section in\n{j}");
        check_json(&j);
    }

    #[test]
    fn the_trackpads_appear_as_ambient_rows_carrying_their_guards() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.default_mode "desktop"
            h.cursor { sens = 0.06, only_in = { "desktop" } }
            h.scroll { mode = "circular", only_in = { "desktop" } }
            "#,
        );
        let rpad = find(&s, "rpad");
        assert_eq!(rpad.section, Section::Ambient);
        assert_eq!(rpad.control_label, "Right trackpad");
        assert_eq!(rpad.label, "Move the cursor");
        assert_eq!(rpad.guard, Guard::OnlyIn(vec!["desktop".into()]));
        let lpad = find(&s, "lpad");
        assert_eq!(lpad.label, "Scroll (circular)");
        assert_eq!(lpad.guard, Guard::OnlyIn(vec!["desktop".into()]));
    }

    // --- JSON shape --------------------------------------------------------

    #[test]
    fn json_has_the_documented_top_level_shape() {
        let s = sheet_from_toml(r#"[bindings]
"guide+r1" = "workspace +1""#);
        let j = s.to_json();
        assert!(j.starts_with("{\n  \"version\": 1,"), "got {j}");
        for key in [
            "\"source\":",
            "\"default_mode\":",
            "\"guide_chords\": [",
            "\"buttons\": [",
            "\"osk_buttons\": [",
            "\"ambient\": [",
            "\"modes\": [",
        ] {
            assert!(j.contains(key), "missing {key} in\n{j}");
        }
        assert!(j.ends_with("}\n"));
    }

    #[test]
    fn every_entry_carries_the_full_key_set() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+r1", "Workspace right", h.workspace "+1")
            h.bind("guide+stick_right", h.workspace "+1")
            "#,
        );
        let j = s.to_json();
        for key in [
            "\"section\":",
            "\"chord\":",
            "\"control\":",
            "\"control_label\":",
            "\"direction\":",
            "\"label\":",
            "\"described\":",
            "\"action\":",
            "\"action_kind\":",
            "\"guard\":",
            "\"guard_label\":",
        ] {
            assert!(j.contains(key), "missing {key} in\n{j}");
        }
        // The flick entry names its stick and its direction.
        let flick = find(&s, "guide+rstick_right");
        assert_eq!(flick.control, "rstick");
        assert_eq!(flick.direction, Some("right"));
        assert_eq!(flick.control_label, "Right stick →");
        assert!(j.contains("\"direction\": \"right\""));
        assert!(j.contains("\"direction\": null"));
    }

    #[test]
    fn json_escapes_strings_that_would_break_the_parse() {
        let s = sheet_from_lua(
            "local h = hyprpad\n\
             h.mode(\"desktop\")\n\
             h.default_mode \"desktop\"\n\
             h.bind(\"guide+r1\", 'a \"quoted\" \\\\ label', h.workspace \"+1\")\n",
        );
        let j = s.to_json();
        assert!(j.contains(r#""label": "a \"quoted\" \\ label""#), "got\n{j}");
    }

    #[test]
    fn json_is_valid_for_the_shipped_sample_config() {
        // The sample the repo ships is the closest thing to the owner's live
        // config; if the widget can parse this it can parse theirs.
        let c = crate::lua_config::load_str(
            include_str!("../config/hyprpad.lua"),
            "config.lua",
        )
        .expect("shipped sample config loads");
        let s = Sheet::build(&c, None);
        check_json(&s.to_json());
        // Spot-check that the owner's own descriptions survive the trip.
        assert_eq!(find(&s, "guide+r1").label, "Workspace right");
        assert!(find(&s, "guide+r1").described);
        // The sheet's own tab paging is live under the sheet and nowhere else:
        // the bumpers keep their guide chords everywhere.
        let sheet_mode = s.modes.iter().find(|m| m.name == "cheatsheet").unwrap();
        assert!(sheet_mode.active.contains(&"l1".to_string()));
        assert!(sheet_mode.active.contains(&"r1".to_string()));
        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(!desktop.active.contains(&"l1".to_string()));
        assert!(desktop.active.contains(&"guide+l1".to_string()));
    }

    /// A minimal JSON well-formedness check: balanced braces/brackets outside
    /// string literals, and no unterminated string. Enough to catch a missing
    /// comma or a stray brace in the hand-rolled emitter.
    fn check_json(j: &str) {
        let mut depth: i32 = 0;
        let mut in_str = false;
        let mut escaped = false;
        let mut stack = Vec::new();
        for c in j.chars() {
            if in_str {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => {
                    stack.push(c);
                    depth += 1;
                }
                '}' => {
                    assert_eq!(stack.pop(), Some('{'), "unbalanced }} in\n{j}");
                    depth -= 1;
                }
                ']' => {
                    assert_eq!(stack.pop(), Some('['), "unbalanced ] in\n{j}");
                    depth -= 1;
                }
                _ => {}
            }
            assert!(depth >= 0, "closed too many in\n{j}");
        }
        assert!(!in_str, "unterminated string in\n{j}");
        assert_eq!(depth, 0, "unclosed containers in\n{j}");
        // A trailing comma before a close is the emitter bug most likely to
        // slip past the brace check.
        for pair in [",\n  ]", ",\n}", ", ]", ",}", ",]"] {
            assert!(!j.contains(pair), "trailing comma before {pair:?} in\n{j}");
        }
    }

    // --- text rendering ----------------------------------------------------

    #[test]
    fn text_output_lists_every_section_and_mode() {
        let c = crate::lua_config::load_str(
            include_str!("../config/hyprpad.lua"),
            "config.lua",
        )
        .expect("shipped sample config loads");
        let t = Sheet::build(&c, None).to_text();
        for want in [
            "Guide chords (hold Steam, then press)",
            "Bare buttons (no modifier)",
            "OSK helpers (while the on-screen keyboard is up)",
            "Trackpads (ambient)",
            "Modes (rules evaluated in order",
            "guide+r1",
            "Workspace right",
            "only in desktop",
        ] {
            assert!(t.contains(want), "missing {want:?} in\n{t}");
        }
    }
}
