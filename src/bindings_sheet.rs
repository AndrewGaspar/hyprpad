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
//! That section is what the keyboard actually routes: its built-in Deck map
//! ([`crate::config::osk_builtins`] — pad clicks commit, L2 holds Shift, R2 is
//! Enter, Y Space, X Backspace, B/Menu close) with the config's `osk_buttons`
//! layered over it, plus the two pad cursors, which are not bindings at all.
//! A built-in row is guarded to the keyboard's context and carries an authored
//! label; a config row carries the config's own guard and description, like
//! any other binding.
//!
//! # The pads
//!
//! The two trackpads get `ambient` rows for what they do with nothing held —
//! the cursor and scroll, each carrying its guard — and, when `h.cursor {
//! guide_in = … }` lists a mode, the right pad gets one more row in the
//! `guide` section (`guide+rpad`, "Move the cursor (guide held)") carrying
//! *that* guard, so a game's tab shows the pad as a mouse under the Steam
//! glyph next to whatever `guide+rpad_click` is bound to.
//!
//! `h.scrub` puts the same kind of row on the LEFT pad (`guide+lpad`, "Scrub
//! the caret") plus one on whichever button it selects with (`guide+l5` by
//! default, "Select while scrubbing"), both carrying the scrub's guard — so a
//! tab where the caret jog wheel is live reads `Ⓢ + left pad: Scrub the caret`
//! above the pad's own `Scroll (circular)`.
//!
//! # Control ids
//!
//! Every entry carries a `control`: a stable id for the *physical* control it
//! lives on (`r1`, `dpad_up`, `rstick`, `rpad`, …). That is the join key the
//! QML widget anchors its callouts on, so the diagram and this module can be
//! changed independently as long as the vocabulary holds.

use std::fmt::Write as _;

use crate::config::{
    osk_builtins, Action, ButtonAction, Config, ConfigFormat, Guard, GuideTap, KeyChord, ModeState,
    OskAction, TransientExit, TransientSpec, WorkspaceTarget,
};
use crate::gesture::{Stick, StickDir};
use crate::report::{Button, Source};

/// Entry point for the `bindings` subcommand.
pub fn run(json: bool) -> Result<(), String> {
    let config = Config::load()?;
    let source = Config::active_config_path();
    let mut sheet = Sheet::build(&config, source.as_ref().map(|(p, f)| (p.as_path(), *f)));
    // The one thing the config cannot answer: which controller is in the
    // user's hands. The daemon publishes it and this reads that file — the
    // command still never opens a device.
    sheet.set_layout(&crate::status::active_layout(), &config);
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
    /// How this mode gives itself back, in words — `"transient · 3 presses ·
    /// exit on B, focus, title, click · 8s"` — or `None` for an ordinary mode
    /// that stays until something moves it.
    ///
    /// This is the *only* place a reader learns that entering `hints` is not
    /// a one-way door: the mode is a tab like any other, and nothing else on
    /// the sheet says it will let go on its own
    /// ([`crate::config::TransientSpec`]).
    pub transient: Option<String>,
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
/// pads, the buttons do what its own table says (`[osk_buttons]` over the
/// built-in Deck map), and every other binding is suppressed (`route_osk` in
/// `run.rs`). The sheet shows it as a mode because that is what it behaves
/// like.
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
    /// Which controller drawing the sheet should use — the id of a file in
    /// `shell/hyprpad.cheatsheet/layouts/`.
    ///
    /// [`Sheet::build`] always fills in the puck's, because this module reads
    /// only the config and must not depend on a running daemon. The `bindings`
    /// subcommand then overwrites it from `status.json`
    /// ([`crate::status::active_layout`]) when a daemon is publishing one, so
    /// `hyprpad bindings --json` names the controller actually in the user's
    /// hands. Still no device is touched: a file the daemon wrote is not the
    /// controller.
    pub layout: String,
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

        // A bare button's row reads exactly like a chord's: a held key or
        // click spells back as `key up` / `mouse left`, and a fired action as
        // whatever it is (`exec …`, `keyboard split`), label and kind alike.
        let button_entry = |btn: Button, what: &ButtonAction, desc, guard| {
            entry(
                Section::Button,
                button_name(btn).to_string(),
                button_name(btn).to_string(),
                button_label(btn),
                None,
                button_rank(btn),
                &what.to_action(),
                desc,
                guard,
            )
        };
        for (btn, what) in &c.buttons {
            let (desc, guard) = (c.button_descs.get(btn), c.button_guards.get(btn));
            entries.push(button_entry(*btn, what, desc, guard));
        }

        // A button bound once per mode contributes one row per binding, each
        // carrying its own guard: `b` reads as Backspace on the desktop and as
        // "Close cheat sheet" under the sheet, and the mode rows below sort the
        // two into the modes they are live in.
        for alt in &c.button_alts {
            let (desc, guard) = (alt.desc.as_ref(), Some(&alt.guard));
            entries.push(button_entry(alt.button, &alt.action, desc, guard));
        }

        // What the buttons do while the keyboard is up: the config's
        // `osk_buttons` layered over the built-in Deck map — the overlay
        // `route_osk` routes (`Config::osk_buttons_in`). A config entry takes
        // its button from the built-ins whatever its guard says (the sheet
        // shows what the author wrote, not what a mode would resolve), and
        // `none` takes the button away with nothing in its place.
        for (btn, what) in &c.osk_buttons {
            if *what == OskAction::None {
                continue;
            }
            let guard = c.osk_button_guards.get(btn).cloned().unwrap_or(Guard::Always);
            entries.push(osk_entry(*btn, *what, c.osk_button_descs.get(btn), guard));
        }
        for (btn, what, label) in osk_builtins() {
            if c.osk_buttons.contains_key(&btn) {
                continue;
            }
            let label = label.to_string();
            entries.push(osk_entry(btn, what, Some(&label), Guard::OnlyIn(vec![OSK_MODE.into()])));
        }

        // The trackpads' ambient behaviour. Not a binding you press, but the
        // diagram has two pads on it and a sheet that leaves them blank is
        // lying about the biggest two controls on the puck.
        let cursor_action = format!("cursor sens {:.2}", c.cursor().sens);
        entries.push(ambient(
            Section::Ambient,
            "rpad",
            "rpad",
            "Right trackpad",
            "Move the cursor",
            cursor_action.clone(),
            &c.cursor_guard,
            RANK_PAD + 1,
        ));
        // The same pad under a held guide (`h.cursor { guide_in = … }`): a
        // guide-layer row on the pad's callout, so the tab for a game reads
        // `Ⓢ + right pad: Move the cursor` beside a `Ⓢ + click` chord. Only
        // when the config lists somewhere for it — off is nothing, not a row
        // that is live nowhere.
        if let Some(guard) = &c.cursor_guide_guard {
            entries.push(ambient(
                Section::Guide,
                "guide+rpad",
                "rpad",
                "Right trackpad",
                "Move the cursor (guide held)",
                cursor_action,
                guard,
                RANK_PAD + 1,
            ));
        }
        // The LEFT pad under a held guide: the caret jog wheel (`h.scrub`).
        // Two rows, both carrying the scrub's own guard — the wheel itself, on
        // the pad's callout beside its ambient scroll, and the button that
        // turns it into a selection, on that button's callout. Only when the
        // config switched the scrub on: off is nothing, not a row that is live
        // nowhere.
        if c.scrub().enabled {
            let scrub = c.scrub();
            entries.push(ambient(
                Section::Guide,
                "guide+lpad",
                "lpad",
                "Left trackpad",
                "Scrub the caret",
                format!("scrub {:.0}°", scrub.detent_deg),
                &c.scrub_guard,
                RANK_PAD,
            ));
            let sel = scrub.select;
            entries.push(ambient(
                Section::Guide,
                &format!("guide+{}", button_name(sel)),
                button_name(sel),
                &button_label(sel),
                "Select while scrubbing",
                "scrub select (hold)".to_string(),
                &c.scrub_guard,
                button_rank(sel),
            ));
        }
        // The guide button's own row: a bare TAP of it, in a game, opens the
        // Steam overlay (`[gamepad] guide_tap`). Not a binding either — nothing
        // resolves against it, and a config cannot move it — but it is the one
        // thing the Steam button does that is not a chord, and a sheet whose
        // guide section says only "hold me" would be hiding it. Guarded to the
        // modes that actually forward, because that is exactly where it works.
        if c.gamepad().enabled && c.gamepad().guide_tap == GuideTap::Steam {
            let forwarding: Vec<String> = c
                .modes()
                .iter()
                .filter(|m| m.forward)
                .map(|m| m.name.clone())
                .collect();
            let guard = Guard::OnlyIn(if forwarding.is_empty() {
                // The built-in path: no modes are declared, and "a game holds
                // focus" is spelled `game` there.
                vec![crate::mode::BUILTIN_GAME.to_string()]
            } else {
                forwarding
            });
            entries.push(ambient(
                Section::Guide,
                "guide_tap",
                "steam",
                "Steam",
                "Steam overlay",
                "guide tap -> steam".to_string(),
                &guard,
                RANK_GUIDE,
            ));
        }
        let (scroll_label, scroll_mode) = match c.scroll().mode {
            crate::config::ScrollMode::Off => ("Scrolling off", "off"),
            crate::config::ScrollMode::Swipe => ("Scroll (swipe)", "swipe"),
            crate::config::ScrollMode::Circular => ("Scroll (circular)", "circular"),
        };
        entries.push(ambient(
            Section::Ambient,
            "lpad",
            "lpad",
            "Left trackpad",
            scroll_label,
            format!("scroll {scroll_mode}"),
            &c.scroll_guard,
            RANK_PAD,
        ));

        // The pads while the keyboard is up. Not a binding anyone can write —
        // `route_osk` owns both cursors itself — but a keyboard context whose
        // sheet left its two biggest controls blank would be lying.
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
                    transient: m.transient.as_ref().map(transient_note),
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
            transient: None,
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
            layout: crate::report::LAYOUT_PUCK.to_string(),
        }
    }

    /// Say which controller the reader is holding, and re-aim the rows that
    /// name a control the reader does not have.
    ///
    /// Everything on the sheet is a *binding*, and bindings are the same on
    /// every controller — that is the whole point of one vocabulary. Two rows
    /// are not: the ambient cursor and the ambient scroll say which physical
    /// thing does them, and on a pad with no trackpads the answer is the
    /// sticks. Those two are retargeted here; the caret scrub is dropped
    /// outright, because circling a spring-loaded stick is not the jog wheel
    /// it describes and there is nothing on this controller that does it.
    ///
    /// The alternative — teaching [`build`](Self::build) which controller is
    /// live — would make the sheet depend on a running daemon. This way it
    /// depends only on the layout id, which is a string.
    pub fn set_layout(&mut self, layout: &str, c: &Config) {
        self.layout = layout.to_string();
        if Source::from_layout(layout).has_pads() {
            return;
        }
        // The caret scrub is a jog wheel: a thumb circling an absolute
        // surface. There is nothing on this controller that does it.
        self.entries.retain(|e| !e.action.starts_with("scrub"));
        let sticks = c.sticks();
        if !sticks.enabled {
            // Nothing drives the pointer at all here. Saying nothing is more
            // honest than pointing at a stick that does not move it.
            self.entries.retain(|e| e.control != "rpad" && e.control != "lpad");
            return;
        }
        let scroll_off = c.scroll().mode == crate::config::ScrollMode::Off;
        for e in &mut self.entries {
            let (id, control_label) = match e.control.as_str() {
                "rpad" => ("rstick", "Right stick"),
                "lpad" => ("lstick", "Left stick"),
                _ => continue,
            };
            e.chord = e.chord.replace(&e.control, id);
            e.control = id.to_string();
            e.control_label = control_label.to_string();
            // The numbers change with the control. The pad's `sens` is
            // pixels-per-pad-count and its scroll `mode` is how a finger is
            // read; a stick has neither. Both are rates.
            if id == "rstick" {
                e.action = format!("cursor {:.0} px/s", sticks.cursor.max);
            } else if !scroll_off {
                e.label = "Scroll".to_string();
                e.action = format!("scroll {:.0} units/s", sticks.scroll.max);
            }
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

/// A row for one on-screen keyboard binding ([`OskAction`]): a config entry
/// (its own description and guard) or a built-in (an authored label, guarded to
/// [`OSK_MODE`] so it belongs to the keyboard's context and to no declared
/// mode). `action` is the config spelling either way — `key space`, `osk
/// shift` — so a built-in reads as exactly what a config would write to keep
/// it.
fn osk_entry(btn: Button, what: OskAction, desc: Option<&String>, guard: Guard) -> Entry {
    Entry {
        section: Section::OskButton,
        chord: button_name(btn).to_string(),
        control: button_name(btn).to_string(),
        direction: None,
        control_label: button_label(btn),
        label: desc.cloned().unwrap_or_else(|| derive_osk_label(what)),
        described: desc.is_some(),
        action: osk_action_string(what),
        action_kind: osk_action_kind(what),
        guard,
        rank: button_rank(btn),
    }
}

/// A row for something the daemon hardwires while the on-screen keyboard is up
/// and no binding can spell — the pad cursors ([`crate::run`]'s `route_osk`).
/// `action` describes the behaviour instead of naming an action, and the label
/// is authored here rather than derived. Guarded to [`OSK_MODE`], so it belongs
/// to the keyboard's context and to no declared mode.
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

/// A row for a pad's continuous behaviour — no press involved, so `action`
/// describes the handler rather than naming an action. `section` is
/// [`Section::Ambient`] for what the pad does on its own and
/// [`Section::Guide`] for what it does under a held guide (the guide-mouse),
/// which is what puts the guide glyph in front of it on the widget.
#[allow(clippy::too_many_arguments)]
fn ambient(
    section: Section,
    chord: &str,
    control: &str,
    control_label: &str,
    label: &str,
    action: String,
    guard: &Guard,
    rank: u32,
) -> Entry {
    Entry {
        section,
        chord: chord.to_string(),
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
        // A mouse button is a `Key` in evdev's code space; spell it back the
        // way a config would write it, so the round trip lands on `mouse`.
        // Only a *bare* one, though: `shift+btn_left` has modifiers to name and
        // there is no `mouse` spelling that carries them, so it prints as the
        // key combo it parsed from.
        Action::Key(chord) => match bare_mouse_name(chord) {
            Some(button) => format!("mouse {button}"),
            None => format!("key {}", chord_name(chord)),
        },
        Action::SetMode(m) => format!("set_mode {m}"),
        Action::ClearMode => "clear_mode".to_string(),
        Action::ControllerOff => "controller_off".to_string(),
        // The `seq: a; b` spelling `Action::parse` reads back, steps in order.
        Action::Seq(steps) => format!(
            "seq: {}",
            steps.iter().map(action_string).collect::<Vec<_>>().join("; ")
        ),
        Action::None => "none".to_string(),
    }
}

/// A transient mode's contract in words, for the mode row: `"transient · 3
/// presses · exit on B, focus, title, click · 8s"`.
///
/// Only the parts that are switched on appear, so `transient { max_presses =
/// 0, exit_on = { "b" }, timeout_ms = 0 }` reads `"transient · exit on B"` —
/// the reader should be able to tell at a glance which of the three ways out
/// actually exist.
pub fn transient_note(t: &TransientSpec) -> String {
    let mut parts = vec!["transient".to_string()];
    match t.max_presses {
        0 => {}
        1 => parts.push("1 press".to_string()),
        n => parts.push(format!("{n} presses")),
    }
    if !t.exit_on.is_empty() {
        let names: Vec<String> = t
            .exit_on
            .iter()
            .map(|e| match e {
                TransientExit::Button(b) => button_label(*b),
                TransientExit::Focus => "focus".to_string(),
                TransientExit::Title => "title".to_string(),
                TransientExit::Click => "click".to_string(),
            })
            .collect();
        parts.push(format!("exit on {}", names.join(", ")));
    }
    if t.timeout_ms > 0 {
        // Whole seconds where they are whole, so the common 8000 does not read
        // as "8.0s" and a 500 ms timer is still legible.
        let secs = t.timeout_ms as f64 / 1000.0;
        parts.push(if secs.fract() == 0.0 {
            format!("{secs:.0}s")
        } else {
            format!("{secs}s")
        });
    }
    parts.join(" · ")
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
        Action::Key(chord) if bare_mouse_name(chord).is_some() => "mouse",
        Action::Key(_) => "key",
        Action::SetMode(_) => "set_mode",
        Action::ClearMode => "clear_mode",
        // Its own kind, not `exec`: nothing else on the sheet acts on the
        // hardware in the reader's hand, and the widget colours by this.
        Action::ControllerOff => "controller",
        Action::Seq(_) => "seq",
        Action::None => "none",
    }
}

/// An on-screen keyboard binding in canonical config spelling — `key space`,
/// `osk shift` — what [`OskAction::parse`] reads back.
pub fn osk_action_string(a: OskAction) -> String {
    match a {
        OskAction::Key(chord) => action_string(&Action::Key(chord)),
        OskAction::Commit => "osk commit".to_string(),
        OskAction::Shift => "osk shift".to_string(),
        OskAction::Dismiss => "osk dismiss".to_string(),
        OskAction::CandidateAccept => "osk accept".to_string(),
        OskAction::CandidateNext => "osk next".to_string(),
        OskAction::None => "none".to_string(),
    }
}

/// An on-screen keyboard binding's verb, for the widget to colour by: `key`
/// for a key typed through the keyboard, `osk` for one of its own actions.
pub fn osk_action_kind(a: OskAction) -> &'static str {
    match a {
        OskAction::Key(_) => "key",
        OskAction::Commit
        | OskAction::Shift
        | OskAction::Dismiss
        | OskAction::CandidateAccept
        | OskAction::CandidateNext => "osk",
        OskAction::None => "none",
    }
}

/// The derived label for an on-screen keyboard binding without a description —
/// the same words the built-in map uses for the same actions, so a config that
/// moves `osk shift` to another button reads the same as the default.
pub fn derive_osk_label(a: OskAction) -> String {
    match a {
        OskAction::Key(chord) => derive_label(&Action::Key(chord)),
        OskAction::Commit => "Type the key under the cursor".to_string(),
        OskAction::Shift => "Shift (hold)".to_string(),
        OskAction::Dismiss => "Close the keyboard".to_string(),
        OskAction::CandidateAccept => "Accept suggestion".to_string(),
        OskAction::CandidateNext => "Next suggestion".to_string(),
        OskAction::None => "Unbound".to_string(),
    }
}

/// A workspace target in config spelling — what [`WorkspaceTarget::parse`]
/// reads back.
fn target(t: &WorkspaceTarget) -> String {
    match t {
        WorkspaceTarget::Relative(n) => format!("{n:+}"),
        WorkspaceTarget::Number(n) => n.to_string(),
        WorkspaceTarget::Selector(s) => s.clone(),
    }
}

/// The `KEY_*` name a code was bound under — the inverse of
/// [`crate::config::key_code`]. Unknown codes print numerically rather than
/// panicking, so a future key added to one table cannot break the sheet.
pub fn key_name(code: u16) -> String {
    // The letter and digit rows are computed from the same table `key_code`
    // parses them with, so the two directions cannot drift.
    if let Some(c) = crate::config::row_name(code) {
        return c.to_string();
    }
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
        111 => "delete",
        158 => "back",
        42 => "leftshift",
        54 => "rightshift",
        29 => "leftctrl",
        97 => "rightctrl",
        56 => "leftalt",
        100 => "rightalt",
        125 => "leftmeta",
        126 => "rightmeta",
        58 => "capslock",
        110 => "insert",
        59 => "f1",
        60 => "f2",
        61 => "f3",
        62 => "f4",
        63 => "f5",
        64 => "f6",
        65 => "f7",
        66 => "f8",
        67 => "f9",
        68 => "f10",
        87 => "f11",
        88 => "f12",
        12 => "minus",
        13 => "equal",
        26 => "leftbrace",
        27 => "rightbrace",
        39 => "semicolon",
        40 => "apostrophe",
        41 => "grave",
        43 => "backslash",
        51 => "comma",
        52 => "dot",
        53 => "slash",
        // The mouse buttons under their evdev names (`key btn_left` parses).
        // `action_string` prefers the `mouse left` spelling; this is the
        // fallback for anything that prints a code as a key name.
        0x110 => "btn_left",
        0x111 => "btn_right",
        0x112 => "btn_middle",
        _ => return format!("keycode {code}"),
    }
    .to_string()
}

/// A whole [`KeyChord`] in canonical config spelling — `tab`, `shift+tab`,
/// `ctrl+shift+tab` — the inverse of [`crate::config::KeyChord::parse`].
pub fn chord_name(chord: &KeyChord) -> String {
    if chord.is_plain() {
        return key_name(chord.code());
    }
    let mut out = String::new();
    for &m in chord.mods() {
        match mod_name(m) {
            "" => out.push_str(&key_name(m)),
            short => out.push_str(short),
        }
        out.push('+');
    }
    out.push_str(&key_name(chord.code()));
    out
}

/// The modifier's *config* name in the prefix of a combo: the short, unqualified
/// spelling (`shift`, `ctrl`, `alt`, `super`) for the left-hand keys people mean
/// when they do not say, and the explicit `right*` name for the others.
///
/// Both parse back to the same code ([`crate::config::modifier_code`]), so the
/// round trip holds either way; the short one is what a config writes and what
/// the cheat sheet should show. A modifier bound *alone* is still printed by
/// [`key_name`] as the exact key it is — there the left/right distinction is the
/// whole content of the binding.
fn mod_name(code: u16) -> &'static str {
    match code {
        42 => "shift",
        29 => "ctrl",
        56 => "alt",
        125 => "super",
        54 => "rightshift",
        97 => "rightctrl",
        100 => "rightalt",
        126 => "rightmeta",
        // Not a modifier at all; unreachable through parsing, and printing the
        // key's own name is the honest answer if it ever happens.
        _ => "",
    }
}

/// The mouse-button name of a chord that is *only* a mouse button — no
/// modifiers to lose. `mouse left` is the config spelling for exactly that, and
/// nothing else, so a chord with modifiers deliberately answers `None` and gets
/// printed as a key combo instead.
fn bare_mouse_name(chord: &KeyChord) -> Option<&'static str> {
    chord.is_plain().then(|| mouse_name(chord.code())).flatten()
}

/// The modifier's label in the *prefix* of a combo: the short, unqualified word
/// a person reads in "Shift+Tab". A modifier bound on its own is still labelled
/// by its full key name ("Leftshift"), because there the exact key is the whole
/// point; in front of a `+` it never is.
fn mod_label(code: u16) -> String {
    match code {
        42 => "Shift",
        54 => "Right Shift",
        29 => "Ctrl",
        97 => "Right Ctrl",
        56 => "Alt",
        100 => "Right Alt",
        125 => "Super",
        126 => "Right Super",
        other => return capitalize(&key_name(other)),
    }
    .to_string()
}

/// The mouse-button name (`left|right|middle`) a `Key` code stands for, or
/// `None` for a keyboard code — the sheet's side of the daemon's routing
/// decision ([`crate::output::PointerButton::from_evdev`]).
fn mouse_name(code: u16) -> Option<&'static str> {
    use crate::output::PointerButton;
    Some(match PointerButton::from_evdev(code)? {
        PointerButton::Left => "left",
        PointerButton::Right => "right",
        PointerButton::Middle => "middle",
    })
}

/// A readable label for a binding the config gave no description.
///
/// Deliberately conservative: it re-words the action rather than inventing
/// intent, so a derived label is never *wrong*, only less specific than one the
/// owner would write. `described: false` on the entry marks it as derived.
pub fn derive_label(a: &Action) -> String {
    match a {
        Action::Workspace(t) => workspace_label(t),
        Action::MoveWindowToWorkspace(t) => {
            format!("Move window to {}", uncapitalize(&workspace_label(t)))
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
        Action::Key(chord) => match bare_mouse_name(chord) {
            Some(button) => format!("{} click", capitalize(button)),
            None => chord_label(chord),
        },
        Action::SetMode(m) => format!("Force {m} mode"),
        Action::ClearMode => "Back to automatic mode".to_string(),
        Action::ControllerOff => "Turn the controller off".to_string(),
        // The steps in order, each in its short in-sequence form: "F, then
        // Hints" says what one press does far better than "Shift+F, then
        // Force hints mode" would.
        Action::Seq(steps) => steps
            .iter()
            .map(step_label)
            .collect::<Vec<_>>()
            .join(", then "),
        Action::None => "Unbound".to_string(),
    }
}

/// One step of a sequence, labelled for the middle of a list rather than for a
/// row of its own.
///
/// The mode verbs are the whole difference: alone, `set_mode hints` is "Force
/// hints mode", which is a sentence; as the tail of "type F, then …" the mode
/// name *is* the label. Everything else reads the same either way and goes
/// through [`derive_label`] unchanged.
fn step_label(a: &Action) -> String {
    match a {
        Action::SetMode(m) => capitalize(m),
        Action::ClearMode => "automatic".to_string(),
        other => derive_label(other),
    }
}

/// A combo in words: the modifiers by their short names, then the key, joined
/// the way the config joins them — "Shift+Tab", "Ctrl+Left", "Shift+F". A lone
/// key is just its own label, exactly as before combos existed.
fn chord_label(chord: &KeyChord) -> String {
    let key = match bare_mouse_name(&KeyChord::plain(chord.code())) {
        Some(button) => format!("{} click", capitalize(button)),
        None => capitalize(&key_name(chord.code()).replace("page", "page ")),
    };
    if chord.is_plain() {
        return key;
    }
    let mut out = String::new();
    for &m in chord.mods() {
        out.push_str(&mod_label(m));
        out.push('+');
    }
    out.push_str(&key);
    out
}

/// A standalone label for a workspace target: "next"/"previous" for the ±1
/// steps that make up almost every real config, an English phrase for the
/// Hyprland selectors that have one, and the literal target for the rest.
fn workspace_label(t: &WorkspaceTarget) -> String {
    match t {
        WorkspaceTarget::Relative(1) => "Workspace next".to_string(),
        WorkspaceTarget::Relative(-1) => "Workspace previous".to_string(),
        WorkspaceTarget::Selector(s) => selector_label(s),
        other => format!("Workspace {}", target(other)),
    }
}

/// English for the workspace selectors a controller config actually reaches
/// for. Anything else prints verbatim rather than being guessed at — a derived
/// label is allowed to be terse, never wrong.
fn selector_label(s: &str) -> String {
    // `empty` takes the flags `m` (this monitor only) and `n` (next empty
    // above the active id), in either order; `n` is the one that changes what
    // you land on enough to say out loud.
    if let Some(flags) = s.strip_prefix("empty") {
        if flags.chars().all(|c| c == 'm' || c == 'n') {
            return if flags.contains('n') {
                "Next empty workspace".to_string()
            } else {
                "Empty workspace".to_string()
            };
        }
    }
    if let Some(name) = s.strip_prefix("special:") {
        return format!("Special workspace {name}");
    }
    match s {
        "previous" | "previous_per_monitor" => "Previous workspace".to_string(),
        "special" => "Special workspace".to_string(),
        // Only reachable for a hand-built target: the parser turns a bare
        // integer into `Number`.
        _ if s.parse::<i32>().is_ok() => format!("Workspace {s}"),
        _ => s.to_string(),
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

/// Drop a standalone label into the middle of a sentence ("Next empty
/// workspace" -> "Move window to next empty workspace"). Only the first letter
/// moves, so a selector that carries its own case (`name:Foo`) keeps it.
fn uncapitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
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
        o.push_str(",\n  \"layout\": ");
        push_str(&mut o, &self.layout);
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
            o.push_str(", \"transient\": ");
            match &m.transient {
                Some(note) => push_str(&mut o, note),
                None => o.push_str("null"),
            }
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
            ("On-screen keyboard (while it is up)", Section::OskButton),
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
            if let Some(note) = &m.transient {
                notes.push(note.clone());
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

    /// The rows the daemon synthesizes rather than reading out of a config, so
    /// a test looking for what the CONFIG said can step over them.
    ///
    /// Two kinds. The keyboard's context — its built-in map and the pad
    /// cursors — shares its chord with the control it sits on (`b`, `rpad`) and
    /// is guarded to `osk`, which no config can write because it is not a mode
    /// a config declares. And the guide *tap*'s Steam-overlay row, guarded to
    /// whichever modes forward, which no config can write either: nothing
    /// resolves against it, which is exactly what `ambient` marks. A `guide_tap`
    /// **binding** shares the chord and is not built in — the two rows coexist
    /// on the Steam button, guarded to disjoint modes.
    fn is_builtin(e: &Entry) -> bool {
        e.guard == Guard::OnlyIn(vec![OSK_MODE.to_string()])
            || (e.chord == "guide_tap" && e.action_kind == "ambient")
    }

    /// The keyboard-context row on `chord` (see [`is_builtin`]).
    fn builtin<'a>(s: &'a Sheet, chord: &str) -> &'a Entry {
        s.entries
            .iter()
            .find(|e| e.chord == chord && is_builtin(e))
            .unwrap_or_else(|| panic!("no built-in {chord}"))
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
        // And the move variant reads as a sentence, whatever the target.
        assert_eq!(
            derive_label(&Action::MoveWindowToWorkspace(WorkspaceTarget::Relative(1))),
            "Move window to workspace next"
        );
    }

    #[test]
    fn workspace_selectors_get_english_labels() {
        let label = |s: &str| derive_label(&Action::Workspace(WorkspaceTarget::Selector(s.into())));
        assert_eq!(label("empty"), "Empty workspace");
        assert_eq!(label("emptym"), "Empty workspace");
        assert_eq!(label("emptyn"), "Next empty workspace");
        assert_eq!(label("emptynm"), "Next empty workspace");
        assert_eq!(label("previous"), "Previous workspace");
        assert_eq!(label("special"), "Special workspace");
        assert_eq!(label("special:term"), "Special workspace term");
        // No English for these: print what the config said rather than guess.
        assert_eq!(label("name:foo"), "name:foo");
        assert_eq!(label("e+1"), "e+1");
        assert_eq!(label("r-1"), "r-1");
        // Moving a window borrows the same phrase.
        assert_eq!(
            derive_label(&Action::MoveWindowToWorkspace(WorkspaceTarget::Selector(
                "emptyn".into()
            ))),
            "Move window to next empty workspace"
        );
        assert_eq!(
            derive_label(&Action::MoveWindowToWorkspace(WorkspaceTarget::Selector(
                "name:foo".into()
            ))),
            "Move window to name:foo"
        );
    }

    /// What the sheet prints as the machine-readable action — the same text
    /// the config front-ends accept.
    #[test]
    fn workspace_action_strings_are_the_config_spelling() {
        assert_eq!(
            action_string(&Action::Workspace(WorkspaceTarget::Selector("emptyn".into()))),
            "workspace emptyn"
        );
        assert_eq!(
            action_string(&Action::Workspace(WorkspaceTarget::Number(3))),
            "workspace 3"
        );
        assert_eq!(
            action_string(&Action::MoveWindowToWorkspace(WorkspaceTarget::Selector(
                "emptyn".into()
            ))),
            "movetoworkspace emptyn"
        );
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
    fn a_combo_reads_back_as_the_config_wrote_it_and_labels_in_words() {
        use crate::config::KeyChord;
        let k = |s: &str| KeyChord::parse(s).expect(s);

        // The config spelling, both ways: what a `config.toml` would say, and
        // what `Action::parse` reads back from it.
        for spelling in ["shift+tab", "ctrl+left", "ctrl+shift+tab", "super+1", "shift+f"] {
            let action = Action::Key(k(spelling));
            assert_eq!(action_string(&action), format!("key {spelling}"));
            assert_eq!(Action::parse(&action_string(&action)), Ok(action.clone()), "{spelling}");
            assert_eq!(action_kind(&action), "key");
        }
        assert_eq!(chord_name(&k("shift+tab")), "shift+tab");
        assert_eq!(chord_name(&k("leftshift+tab")), "shift+tab", "the short form is canonical");
        assert_eq!(chord_name(&k("rightshift+tab")), "rightshift+tab", "but not for the right one");
        assert_eq!(chord_name(&k("tab")), "tab", "a lone key keeps its bare name");
        // A lone modifier is still the exact key it was bound as.
        assert_eq!(action_string(&Action::Key(k("leftshift"))), "key leftshift");

        // The derived labels: modifiers by their short names, the key by its own.
        assert_eq!(derive_label(&Action::Key(k("shift+tab"))), "Shift+Tab");
        assert_eq!(derive_label(&Action::Key(k("ctrl+left"))), "Ctrl+Left");
        assert_eq!(derive_label(&Action::Key(k("shift+f"))), "Shift+F");
        assert_eq!(derive_label(&Action::Key(k("ctrl+shift+tab"))), "Ctrl+Shift+Tab");
        assert_eq!(derive_label(&Action::Key(k("super+1"))), "Super+1");
        assert_eq!(derive_label(&Action::Key(k("rightalt+home"))), "Right Alt+Home");
        // A modifier bound alone still names the exact key it is — there the
        // left/right distinction is the point.
        assert_eq!(derive_label(&Action::Key(k("leftshift"))), "Leftshift");

        // A combo whose key is a mouse button has modifiers to name, so it
        // prints as a key combo rather than losing them to the `mouse`
        // spelling — and its kind follows the spelling.
        let shift_click = Action::Key(k("shift+btn_left"));
        assert_eq!(action_string(&shift_click), "key shift+btn_left");
        assert_eq!(action_kind(&shift_click), "key");
        assert_eq!(derive_label(&shift_click), "Shift+Left click");
        assert_eq!(Action::parse("key shift+btn_left"), Ok(shift_click));
        // A bare one is still `mouse left`.
        assert_eq!(action_string(&Action::Key(k("btn_left"))), "mouse left");
        assert_eq!(action_kind(&Action::Key(k("btn_left"))), "mouse");

        // The OSK table renders a combo the same way.
        let osk = OskAction::Key(k("ctrl+backspace"));
        assert_eq!(osk_action_string(osk), "key ctrl+backspace");
        assert_eq!(osk_action_kind(osk), "key");
        assert_eq!(derive_osk_label(osk), "Ctrl+Backspace");
    }

    #[test]
    fn every_name_the_sheet_prints_is_a_name_the_config_parses() {
        // `key_name` is `key_code`'s inverse, and the letter/digit rows are
        // computed from one table on both sides. Walk the whole keyboard range
        // and pin the round trip, so a key added to one direction can never
        // print as something the other cannot read back.
        for code in 1u16..=255 {
            let name = key_name(code);
            if name.starts_with("keycode ") {
                continue; // not in the table at all — printed numerically
            }
            assert_eq!(
                Action::parse(&format!("key {name}")),
                Ok(Action::Key(KeyChord::plain(code))),
                "{code} printed as {name:?}"
            );
        }
        // The letters and digits, specifically.
        assert_eq!(key_name(30), "a");
        assert_eq!(key_name(50), "m");
        assert_eq!(key_name(2), "1");
        assert_eq!(key_name(11), "0");
        assert_eq!(key_name(88), "f12");
        assert_eq!(key_name(52), "dot");
    }

    #[test]
    fn key_and_mode_labels() {
        assert_eq!(derive_label(&Action::Key(103.into())), "Up");
        assert_eq!(derive_label(&Action::Key(14.into())), "Backspace");
        assert_eq!(derive_label(&Action::Key(104.into())), "Page up");
        // The modifiers print under the name they were bound by, and parse back.
        assert_eq!(derive_label(&Action::Key(42.into())), "Leftshift");
        assert_eq!(action_string(&Action::Key(125.into())), "key leftmeta");
        assert_eq!(Action::parse("key leftmeta"), Ok(Action::Key(125.into())));
        assert_eq!(derive_label(&Action::SetMode("desktop".into())), "Force desktop mode");
        assert_eq!(derive_label(&Action::ClearMode), "Back to automatic mode");
    }

    #[test]
    fn mouse_bindings_read_as_clicks() {
        // A mouse button is a `Key` in evdev's code space; the sheet tells it
        // apart by the code, exactly as the daemon does when routing it.
        assert_eq!(derive_label(&Action::Key(0x110.into())), "Left click");
        assert_eq!(derive_label(&Action::Key(0x111.into())), "Right click");
        assert_eq!(derive_label(&Action::Key(0x112.into())), "Middle click");
        assert_eq!(action_string(&Action::Key(0x110.into())), "mouse left");
        assert_eq!(action_string(&Action::Key(0x111.into())), "mouse right");
        assert_eq!(action_kind(&Action::Key(0x110.into())), "mouse");
        assert_eq!(action_kind(&Action::Key(0x112.into())), "mouse");
        // A keyboard code is untouched by this.
        assert_eq!(action_kind(&Action::Key(103.into())), "key");
        assert_eq!(action_string(&Action::Key(103.into())), "key up");
        assert_eq!(key_name(0x110), "btn_left");
        assert_eq!(key_name(0x112), "btn_middle");
        assert_eq!(mouse_name(103), None);

        // The default config's three clicks land on controls the diagram knows,
        // in the bare-button section, with the `mouse` kind.
        let sheet = Sheet::build(&Config::load_default(), None);
        let mut clicks: Vec<(String, String, &str, String)> = sheet
            .entries
            .iter()
            .filter(|e| e.action_kind == "mouse")
            .map(|e| (e.control.clone(), e.label.clone(), e.section.wire(), e.action.clone()))
            .collect();
        clicks.sort();
        assert_eq!(
            clicks,
            vec![
                ("l2".into(), "Right click".into(), "button", "mouse right".into()),
                ("r2".into(), "Left click".into(), "button", "mouse left".into()),
                ("rpad_click".into(), "Left click".into(), "button", "mouse left".into()),
            ]
        );
    }

    #[test]
    fn action_strings_round_trip_through_the_parser() {
        for a in [
            Action::Workspace(WorkspaceTarget::Relative(1)),
            Action::Workspace(WorkspaceTarget::Relative(-2)),
            Action::Workspace(WorkspaceTarget::Number(3)),
            Action::Workspace(WorkspaceTarget::Selector("emptyn".into())),
            Action::Workspace(WorkspaceTarget::Selector("name:foo".into())),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Selector("special".into())),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Selector("emptyn".into())),
            Action::ToggleFullscreen,
            Action::Exec("omarchy-menu".into()),
            Action::Dispatch("hl.dsp.window.close()".into()),
            Action::ToggleKeyboard { mode: crate::osk::OskMode::Split, reflow: false },
            Action::ToggleKeyboard { mode: crate::osk::OskMode::Bottom, reflow: true },
            Action::Key(103.into()),
            Action::Key(0x110.into()),
            Action::Key(0x112.into()),
            Action::SetMode("game".into()),
            Action::ClearMode,
            Action::Seq(vec![Action::Key(33.into()), Action::SetMode("hints".into())]),
            Action::Seq(vec![
                Action::Key(KeyChord::parse("shift+f").unwrap()),
                Action::SetMode("hints".into()),
            ]),
            Action::Seq(vec![Action::Exec("true".into()), Action::ClearMode]),
            Action::None,
        ] {
            let s = action_string(&a);
            assert_eq!(Action::parse(&s), Ok(a.clone()), "round trip of {s:?}");
        }
        // The keyboard's own table, through its own parser.
        for a in [
            OskAction::Key(57.into()),
            OskAction::Commit,
            OskAction::Shift,
            OskAction::Dismiss,
            OskAction::None,
        ] {
            let s = osk_action_string(a);
            assert_eq!(OskAction::parse(&s), Ok(a), "round trip of {s:?}");
        }
        assert_eq!(osk_action_kind(OskAction::Key(57.into())), "key");
        assert_eq!(osk_action_kind(OskAction::Shift), "osk");
        assert_eq!(derive_osk_label(OskAction::Key(57.into())), "Space");
        assert_eq!(derive_osk_label(OskAction::Shift), "Shift (hold)");
        assert_eq!(derive_osk_label(OskAction::Commit), "Type the key under the cursor");
        assert_eq!(derive_osk_label(OskAction::Dismiss), "Close the keyboard");
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
    fn a_fired_button_row_reads_exactly_like_a_chords() {
        // A bare button bound to something other than a key or click fires on
        // its press edge; on the sheet that is an ordinary action row — label,
        // spelling and kind all derived the way a chord's are — in the
        // bare-button section, with its guard.
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx) return false end)
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+r5", h.exec "voxtype record toggle")
            h.button("l5", h.exec "voxtype record toggle"):only_in("desktop")
            h.button("r4", h.keyboard { mode = "split" })
            h.button("l4", "Pause the game", h.set_mode "game")
            h.button("dpad_up", h.key "up")
            h.button("r2", h.mouse "left")
            "#,
        );
        let l5 = find(&s, "l5");
        let chord = find(&s, "guide+r5");
        assert_eq!(l5.section, Section::Button);
        assert_eq!(l5.action_kind, "exec");
        assert_eq!(l5.action, "exec voxtype record toggle");
        assert_eq!(l5.label, "Voxtype record toggle");
        assert!(!l5.described);
        assert_eq!(l5.guard, Guard::OnlyIn(vec!["desktop".into()]));
        // Same action, same row apart from the control it sits on.
        assert_eq!(
            (l5.label.as_str(), l5.action.as_str(), l5.action_kind),
            (chord.label.as_str(), chord.action.as_str(), chord.action_kind)
        );

        let r4 = find(&s, "r4");
        assert_eq!(r4.action_kind, "keyboard");
        assert_eq!(r4.action, "keyboard split");
        assert_eq!(r4.label, "On-screen keyboard (split)");
        let l4 = find(&s, "l4");
        assert_eq!(l4.action_kind, "set_mode");
        assert_eq!(l4.label, "Pause the game");
        assert!(l4.described);
        // The held rows are untouched by any of this.
        assert_eq!(find(&s, "dpad_up").action_kind, "key");
        assert_eq!(find(&s, "dpad_up").label, "Up");
        assert_eq!(find(&s, "r2").action_kind, "mouse");
        assert_eq!(find(&s, "r2").label, "Left click");

        // The text form prints the fired row in the bare-button table.
        let text = s.to_text();
        let table = text.split("Bare buttons (no modifier)").nth(1).expect("section");
        let table = table.split("OSK helpers").next().expect("section end");
        assert!(table.contains("l5") && table.contains("exec voxtype record toggle"), "{table}");
        assert!(table.contains("only in desktop"), "{table}");

        // And the TOML front-end lands in the same place.
        let t = sheet_from_toml("[buttons]\nl5 = \"exec foo\"\ndpad_up = \"key up\"\n");
        assert_eq!(find(&t, "l5").action_kind, "exec");
        assert_eq!(find(&t, "l5").label, "Foo");
        assert_eq!(find(&t, "l5").section, Section::Button);
    }

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

    /// The guide button's one non-chord row: a bare tap in a game opens the
    /// Steam overlay, and the sheet says so in the game's tab and nowhere else.
    #[test]
    fn the_guide_tap_row_appears_in_the_forwarding_modes_only() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+r1", h.workspace "+1")
            "#,
        );
        let tap = s.entries.iter().find(|e| e.chord == "guide_tap").expect("a guide_tap row");
        assert_eq!(tap.section, Section::Guide);
        assert_eq!(tap.control, "steam", "it sits on the Steam button's callout");
        assert_eq!(tap.label, "Steam overlay");
        assert_eq!(tap.action_kind, "ambient", "nothing resolves against it");
        assert_eq!(tap.guard, Guard::OnlyIn(vec!["game".to_string()]));

        // Switched off, the row goes with it — off is nothing, not a row that
        // is live nowhere.
        let off = sheet_from_toml("[gamepad]\nguide_tap = none\n");
        assert!(!off.entries.iter().any(|e| e.chord == "guide_tap"));
        let disabled = sheet_from_toml("[gamepad]\nenabled = false\n");
        assert!(!disabled.entries.iter().any(|e| e.chord == "guide_tap"));

        // With no modes declared, the built-in path's own name for "a game
        // holds focus" is what it is guarded to.
        let builtin = sheet_from_toml("[bindings]\n\"guide+r1\" = \"workspace +1\"\n");
        let tap = builtin.entries.iter().find(|e| e.chord == "guide_tap").expect("a row");
        assert_eq!(tap.guard, Guard::OnlyIn(vec![crate::mode::BUILTIN_GAME.to_string()]));
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
        // Its rows are the whole section: what the config wrote, and the
        // built-in Deck map under it — a context listing only `y` would be
        // unusable.
        assert!(osk.active.contains(&"y".to_string()));
        for chord in
            ["lpad", "rpad", "lpad_click", "rpad_click", "l2", "r2", "x", "b", "menu", "r1", "l1"]
        {
            assert!(osk.active.contains(&chord.to_string()), "no built-in row for {chord}");
        }
        // No declared mode claims them: while the keyboard is up, nothing else
        // is live, so they are not "live in desktop" in any useful sense.
        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(!desktop.active.contains(&"lpad_click".to_string()));
        assert!(!desktop.active.contains(&"menu".to_string()));

        // The Deck map, as the daemon routes it: L2 holds Shift, R2 is Enter,
        // the pad clicks type under their cursor, B and Menu close. Each row
        // spells the action a config would write to keep it.
        let l2 = builtin(&s, "l2");
        assert_eq!((l2.label.as_str(), l2.action.as_str(), l2.action_kind), ("Shift (hold)", "osk shift", "osk"));
        let r2 = builtin(&s, "r2");
        assert_eq!((r2.label.as_str(), r2.action.as_str(), r2.action_kind), ("Enter", "key enter", "key"));
        for pad in ["lpad_click", "rpad_click"] {
            let e = builtin(&s, pad);
            assert_eq!((e.label.as_str(), e.action.as_str(), e.action_kind), ("Type the key under the cursor", "osk commit", "osk"));
        }
        let close = builtin(&s, "menu");
        assert_eq!((close.label.as_str(), close.action.as_str()), ("Close the keyboard", "osk dismiss"));
        assert_eq!(builtin(&s, "b").action, "osk dismiss");
        assert_eq!(builtin(&s, "x").label, "Backspace");
        // The word-prediction strip's bumpers (osk-prediction.md §7.2).
        let r1 = builtin(&s, "r1");
        assert_eq!(
            (r1.label.as_str(), r1.action.as_str(), r1.action_kind),
            ("Accept suggestion", "osk accept", "osk")
        );
        let l1 = builtin(&s, "l1");
        assert_eq!(
            (l1.label.as_str(), l1.action.as_str(), l1.action_kind),
            ("Next suggestion", "osk next", "osk")
        );
        assert_eq!(close.section, Section::OskButton);
        assert!(close.described, "a built-in row is authored, not derived");
        assert_eq!(close.guard, Guard::OnlyIn(vec![OSK_MODE.to_string()]));
        assert_eq!(builtin(&s, "lpad").action_kind, "builtin", "the cursors are not bindings");
        // The config's `y` is the config's row, not a built-in: Space, derived.
        let y = find(&s, "y");
        assert_eq!((y.label.as_str(), y.action.as_str(), y.described), ("Space", "key space", false));
        assert!(s.entries.iter().filter(|e| e.chord == "y" && e.section == Section::OskButton).count() == 1);

        let j = s.to_json();
        assert!(j.contains("\"builtin\": true"), "modes carry `builtin` in\n{j}");
        assert!(j.contains("\"section\": \"osk_button\""), "and the section in\n{j}");
        check_json(&j);

        // …and the rendered tab a reader actually sees.
        let t = s.to_text();
        assert!(t.contains("Accept suggestion"), "{t}");
        assert!(t.contains("Next suggestion"), "{t}");
    }

    #[test]
    fn a_config_rebinds_or_drops_a_keyboard_built_in() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.default_mode "desktop"
            h.osk_button("r2", "Type here too", h.osk "commit")
            h.osk_button("menu", h.none())
            h.osk_button("l1", h.key "tab")
            "#,
        );
        let osk_rows = |chord: &str| -> Vec<&Entry> {
            s.entries.iter().filter(|e| e.chord == chord && e.section == Section::OskButton).collect()
        };
        // R2: one row, the config's, in place of the built-in Enter.
        let r2 = osk_rows("r2");
        assert_eq!(r2.len(), 1, "the built-in must not show through: {r2:?}");
        assert_eq!((r2[0].label.as_str(), r2[0].action.as_str(), r2[0].action_kind), ("Type here too", "osk commit", "osk"));
        assert!(r2[0].described && r2[0].guard == Guard::Always);
        // Menu: taken away, nothing in its place.
        assert!(osk_rows("menu").is_empty());
        // L1: added, with a derived label.
        let l1 = osk_rows("l1");
        assert_eq!((l1[0].label.as_str(), l1[0].action.as_str(), l1[0].described), ("Tab", "key tab", false));
        // The untouched built-ins are still there.
        assert_eq!(builtin(&s, "b").action, "osk dismiss");
        assert_eq!(builtin(&s, "l2").action, "osk shift");
        let osk = s.modes.iter().find(|m| m.name == OSK_MODE).unwrap();
        assert!(osk.active.contains(&"l1".to_string()) && !osk.active.contains(&"menu".to_string()));
        check_json(&s.to_json());
        // The text table has the section under its new title, and the row.
        let t = s.to_text();
        assert!(t.contains("On-screen keyboard (while it is up)"), "{t}");
        assert!(t.contains("osk shift"), "{t}");
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
        // Nothing listed for the guide-held pad: no row for it at all.
        assert!(!s.entries.iter().any(|e| e.chord == "guide+rpad"));
    }

    #[test]
    fn the_guide_held_cursor_is_a_guide_row_on_the_pad_beside_its_click_chord() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx) return false end)
            h.mode("desktop")
            h.default_mode "desktop"
            h.cursor { sens = 0.06, only_in = { "desktop" }, guide_in = { "game" } }
            h.button("rpad_click", h.mouse "left"):only_in("desktop")
            h.bind("guide+rpad_click", "Click (guide mouse)", h.mouse "left"):only_in("game")
            h.bind("guide+l5", h.key "leftshift")
            "#,
        );
        // The pad's own row is untouched...
        let rpad = find(&s, "rpad");
        assert_eq!(rpad.section, Section::Ambient);
        assert_eq!(rpad.guard, Guard::OnlyIn(vec!["desktop".into()]));
        // ...and the guide-held one sits on the same control, in the guide
        // section (so the widget draws the Steam glyph), carrying `guide_in`.
        let under = find(&s, "guide+rpad");
        assert_eq!(under.section, Section::Guide);
        assert_eq!(under.control, "rpad");
        assert_eq!(under.control_label, "Right trackpad");
        assert_eq!(under.label, "Move the cursor (guide held)");
        assert_eq!(under.action, "cursor sens 0.06");
        assert_eq!(under.action_kind, "ambient");
        assert_eq!(under.guard, Guard::OnlyIn(vec!["game".into()]));

        // A key or mouse button on a chord reads like a bare one: the sheet
        // does not know it is held, and does not need to.
        let click = find(&s, "guide+rpad_click");
        assert_eq!(click.section, Section::Guide);
        assert_eq!(click.action_kind, "mouse");
        assert_eq!(click.action, "mouse left");
        assert_eq!(click.label, "Click (guide mouse)");
        assert!(click.described);
        let shift = find(&s, "guide+l5");
        assert_eq!(shift.action_kind, "key");
        assert_eq!(shift.label, "Leftshift");

        // The game tab is where both live, and the desktop tab has neither.
        let game = s.modes.iter().find(|m| m.name == "game").unwrap();
        assert!(game.active.contains(&"guide+rpad".to_string()));
        assert!(game.active.contains(&"guide+rpad_click".to_string()));
        assert!(!game.active.contains(&"rpad".to_string()));
        assert!(!game.active.contains(&"rpad_click".to_string()));
        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(!desktop.active.contains(&"guide+rpad".to_string()));
        assert!(!desktop.active.contains(&"guide+rpad_click".to_string()));
        assert!(desktop.active.contains(&"rpad".to_string()));

        // Both renderings carry it: the guide-chord table in the text form,
        // the `guide_chords` array in the JSON.
        let text = s.to_text();
        let table = text.split("Guide chords (hold Steam, then press)").nth(1).unwrap();
        let table = table.split("Bare buttons").next().unwrap();
        assert!(table.contains("guide+rpad ") && table.contains("Move the cursor (guide held)"), "{table}");
        assert!(table.contains("only in game"), "{table}");
        let j = s.to_json();
        assert!(j.contains("\"chord\": \"guide+rpad\", \"control\": \"rpad\""), "{j}");
        check_json(&j);

        // The TOML front-end spells it too, against the built-in mode names.
        let t = sheet_from_toml(
            "[cursor]\nguide_in = [\"game\"]\n[bindings]\n\"guide+rpad_click\" = \"mouse left\"\n",
        );
        assert_eq!(find(&t, "guide+rpad").guard, Guard::OnlyIn(vec!["game".into()]));
        assert_eq!(find(&t, "guide+rpad_click").action_kind, "mouse");
    }

    #[test]
    fn controller_off_gets_its_own_words_and_its_own_kind() {
        // The label the sheet draws when a binding gives no description of its
        // own, the spelling a config would write back, and a kind of its own so
        // the widget does not colour it like an `exec`.
        assert_eq!(derive_label(&Action::ControllerOff), "Turn the controller off");
        assert_eq!(action_string(&Action::ControllerOff), "controller_off");
        assert_eq!(action_kind(&Action::ControllerOff), "controller");
        // Round trip: what the sheet prints is what the parser reads back.
        assert_eq!(
            Action::parse(&action_string(&Action::ControllerOff)),
            Ok(Action::ControllerOff)
        );

        // And on a real sheet, from both front-ends.
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+quickaccess", "Controller off", h.controller_off())
            h.button("l4", h.controller_off())
            "#,
        );
        let chord = find(&s, "guide+quickaccess");
        assert_eq!(
            (chord.label.as_str(), chord.action.as_str(), chord.action_kind),
            ("Controller off", "controller_off", "controller")
        );
        // No description: the derived words stand in.
        assert_eq!(find(&s, "l4").label, "Turn the controller off");
        let t = sheet_from_toml("[bindings]\n\"guide+quickaccess\" = \"controller_off\"\n");
        assert_eq!(find(&t, "guide+quickaccess").action_kind, "controller");
    }

    #[test]
    fn the_caret_scrub_is_two_guide_rows_carrying_the_scrub_guard() {
        let s = sheet_from_lua(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx) return false end)
            h.mode("desktop")
            h.default_mode "desktop"
            h.scroll { mode = "circular", only_in = { "desktop" } }
            h.scrub { detent_deg = 15, only_in = { "desktop" } }
            "#,
        );
        // The wheel sits on the left pad's callout in the guide section, so the
        // widget draws it under the Steam glyph above the pad's own scroll row.
        let wheel = find(&s, "guide+lpad");
        assert_eq!(wheel.section, Section::Guide);
        assert_eq!(wheel.control, "lpad");
        assert_eq!(wheel.control_label, "Left trackpad");
        assert_eq!(wheel.label, "Scrub the caret");
        assert_eq!(wheel.action, "scrub 15°");
        assert_eq!(wheel.action_kind, "ambient");
        assert_eq!(wheel.guard, Guard::OnlyIn(vec!["desktop".into()]));
        // The pad's ambient scroll is untouched beside it.
        let lpad = find(&s, "lpad");
        assert_eq!(lpad.section, Section::Ambient);
        assert_eq!(lpad.label, "Scroll (circular)");

        // The select button gets its own row, on its own control.
        let sel = find(&s, "guide+l5");
        assert_eq!(sel.section, Section::Guide);
        assert_eq!(sel.control, "l5");
        assert_eq!(sel.control_label, "L5 grip");
        assert_eq!(sel.label, "Select while scrubbing");
        assert_eq!(sel.action, "scrub select (hold)");
        assert_eq!(sel.guard, Guard::OnlyIn(vec!["desktop".into()]));

        // Both rows live on the tab the guard names, and nowhere else.
        let desktop = s.modes.iter().find(|m| m.name == "desktop").unwrap();
        assert!(desktop.active.contains(&"guide+lpad".to_string()));
        assert!(desktop.active.contains(&"guide+l5".to_string()));
        let game = s.modes.iter().find(|m| m.name == "game").unwrap();
        assert!(!game.active.contains(&"guide+lpad".to_string()));
        assert!(!game.active.contains(&"guide+l5".to_string()));

        // Both renderings carry them.
        let text = s.to_text();
        let table = text.split("Guide chords (hold Steam, then press)").nth(1).unwrap();
        let table = table.split("Bare buttons").next().unwrap();
        assert!(table.contains("guide+lpad") && table.contains("Scrub the caret"), "{table}");
        assert!(table.contains("Select while scrubbing"), "{table}");
        let j = s.to_json();
        assert!(j.contains("\"chord\": \"guide+lpad\", \"control\": \"lpad\""), "{j}");
        check_json(&j);

        // A configured `select` moves the second row to that button.
        let l4 = sheet_from_lua(r#"hyprpad.scrub { select = "l4" }"#);
        assert_eq!(find(&l4, "guide+l4").label, "Select while scrubbing");
        assert!(!l4.entries.iter().any(|e| e.chord == "guide+l5"));

        // Off — the default, and every config that predates the scrub — is no
        // rows at all, not rows that are live nowhere.
        let none = sheet_from_lua(r#"hyprpad.scroll { mode = "circular" }"#);
        assert!(!none
            .entries
            .iter()
            .any(|e| e.chord.starts_with("guide+lpad") || e.action.starts_with("scrub")));

        // And the TOML front-end spells the same two rows.
        let t = sheet_from_toml("[scrub]\ndetent_deg = 20\n");
        assert_eq!(find(&t, "guide+lpad").action, "scrub 20°");
        assert_eq!(find(&t, "guide+l5").label, "Select while scrubbing");
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
            "\"layout\":",
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

    /// The layout id is which controller drawing to show. `build` cannot know
    /// — it reads only the config — so it names the puck, and the `bindings`
    /// subcommand overwrites it from what the daemon published.
    #[test]
    fn the_json_names_the_controller_drawing_to_use() {
        let src = "[bindings]\n\"guide+r1\" = \"workspace +1\"\n";
        let c = Config::from_toml_str(src).unwrap();
        let mut s = sheet_from_toml(src);
        assert_eq!(s.layout, "steam-controller-2026");
        assert!(s.to_json().contains(r#""layout": "steam-controller-2026""#));
        s.set_layout("xbox-elite-2", &c);
        assert!(s.to_json().contains(r#""layout": "xbox-elite-2""#));
    }

    /// A controller with no trackpads: the *bindings* are identical — that is
    /// the point of one vocabulary — but the two rows that name a physical
    /// pad have to name the stick that stands in for it, and the caret scrub
    /// goes away because nothing on this pad does it.
    #[test]
    fn a_padless_layout_retargets_the_ambient_rows_onto_the_sticks() {
        let src = "[bindings]\n\"guide+r1\" = \"workspace +1\"\n[scrub]\ndetent_deg = 15\n";
        let c = Config::from_toml_str(src).unwrap();
        let puck = sheet_from_toml(src);
        let cursor = |s: &Sheet| {
            s.entries
                .iter()
                .find(|e| e.label == "Move the cursor")
                .expect("an ambient cursor row")
                .clone()
        };
        assert_eq!(cursor(&puck).control, "rpad");
        assert_eq!(cursor(&puck).control_label, "Right trackpad");
        assert!(puck.entries.iter().any(|e| e.action.starts_with("scrub")));

        let mut xbox = sheet_from_toml(src);
        xbox.set_layout("xbox-elite-2", &c);
        assert_eq!(cursor(&xbox).control, "rstick");
        assert_eq!(cursor(&xbox).control_label, "Right stick");
        // The numbers change with the control: the pad's `sens` is pixels per
        // pad count, the stick's is a top speed.
        assert_eq!(cursor(&xbox).action, "cursor 1500 px/s");
        let scroll = xbox
            .entries
            .iter()
            .find(|e| e.action.starts_with("scroll"))
            .expect("an ambient scroll row");
        assert_eq!(scroll.control, "lstick");
        assert_eq!(scroll.control_label, "Left stick");
        assert_eq!(scroll.label, "Scroll", "a stick has no circular mode");
        assert_eq!(scroll.action, "scroll 180 units/s");
        assert!(
            !xbox.entries.iter().any(|e| e.action.starts_with("scrub")),
            "there is no pad to circle"
        );

        // Turn the stick layer off and the two rows go away: nothing drives
        // the pointer, and pointing at a stick that does not move it would be
        // a lie.
        let off = Config::from_toml_str(&format!("{src}[sticks]\nenabled = false\n")).unwrap();
        let mut dead = sheet_from_toml(src);
        dead.set_layout("xbox-elite-2", &off);
        assert!(!dead.entries.iter().any(|e| e.label == "Move the cursor"));
        assert!(!dead.entries.iter().any(|e| e.action.starts_with("scroll")));
        // Everything that is a binding is untouched: same chord, same action.
        let bound = |s: &Sheet| {
            s.entries
                .iter()
                .filter(|e| e.section == Section::Guide && e.chord == "guide+r1")
                .map(|e| (e.chord.clone(), e.action.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(bound(&puck), bound(&xbox));
        // And an unknown layout id is read as the puck, so a config written
        // against it is never silently rewritten.
        let mut unknown = sheet_from_toml(src);
        unknown.set_layout("some-future-pad", &c);
        assert_eq!(cursor(&unknown).control, "rpad");
    }

    /// The two shipped layouts must anchor every control the sheet can emit
    /// for them, and must not invent ids the daemon never produces. Checked
    /// against the real files, so a layout and the vocabulary cannot drift.
    #[test]
    fn every_shipped_layout_anchors_only_real_control_ids() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("shell/hyprpad.cheatsheet/layouts");
        // Every id the daemon can put in an entry's `control` field: each
        // bindable button's config name, the two stick flicks, the guide, and
        // the group ids a layout may collapse into.
        use crate::report::Button::*;
        let mut known: Vec<String> = [
            A, B, X, Y, BumperL1, BumperR1, TriggerL2Full, TriggerR2Full, L3, R3, GripL4,
            GripL5, GripR4, GripR5, DpadUp, DpadDown, DpadLeft, DpadRight, Menu, View,
            QuickAccess, PadLeftClick, PadRightClick, Steam,
        ]
        .iter()
        .map(|&b| button_name(b).to_string())
        .collect();
        known.extend(["lstick", "rstick", "dpad", "lpad", "rpad"].map(str::to_string));

        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("layouts/") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            seen += 1;
            let text = std::fs::read_to_string(&path).expect("read layout");
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            // The file's own id must be its filename: that is how Panel.qml
            // finds it (`layouts/<layoutId>.json`).
            assert!(
                text.contains(&format!("\"id\": \"{stem}\"")),
                "{stem}: `id` must match the filename"
            );
            for key in ["viewBox", "art", "controls", "glyphs", "modifiers"] {
                assert!(text.contains(&format!("\"{key}\"")), "{stem}: missing `{key}`");
            }
            let controls = layout_keys(&text, "controls");
            assert!(controls.len() > 10, "{stem}: only found {} controls", controls.len());
            for id in &controls {
                assert!(known.contains(id), "{stem}: `{id}` is not a control the daemon emits");
            }
            // The four d-pad directions and the guide are what every config
            // reaches for first; a layout that cannot anchor them is broken.
            for must in ["a", "b", "steam", "dpad_up", "dpad_down", "dpad_left", "dpad_right"] {
                assert!(controls.iter().any(|c| c == must), "{stem}: no anchor for `{must}`");
            }
            // Every anchored control needs a chip, or its callout draws blank.
            let glyphs = layout_keys(&text, "glyphs");
            assert!(!glyphs.is_empty(), "{stem}: no glyphs parsed");
            for id in &controls {
                assert!(glyphs.contains(id), "{stem}: `{id}` is anchored but has no glyph");
            }
            // Bundled art has to exist; `steam`-sourced art is read from the
            // local Steam install and is allowed to be absent.
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("\"path\": \"") {
                    let rel = rest.split('"').next().unwrap();
                    if rel.starts_with("art/") {
                        let art = dir.parent().unwrap().join(rel);
                        assert!(art.exists(), "{stem}: missing bundled art {rel}");
                    }
                }
            }
        }
        assert_eq!(seen, 2, "the puck's layout and the Xbox one");
    }

    /// Pull the object keys out of one top-level block of a layout file.
    ///
    /// A five-line scanner rather than a JSON dependency: these files are
    /// ours, hand-formatted one `"key": { … }` per line, and the alternative
    /// is a crate for one test.
    fn layout_keys(text: &str, block: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut inside = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with(&format!("\"{block}\": {{")) {
                inside = true;
                continue;
            }
            if inside {
                if t == "}," || t == "}" {
                    break;
                }
                if let Some(rest) = t.strip_prefix('"') {
                    if let Some(key) = rest.split('"').next() {
                        if t.contains("{") {
                            out.push(key.to_string());
                        }
                    }
                }
            }
        }
        out
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
            "On-screen keyboard (while it is up)",
            "Trackpads (ambient)",
            "Modes (rules evaluated in order",
            "guide+r1",
            "Workspace right",
            "only in desktop",
        ] {
            assert!(t.contains(want), "missing {want:?} in\n{t}");
        }
    }

    // --- h.seq and transient modes (docs/research/browser-hints.md) --------

    #[test]
    fn a_sequence_reads_as_its_steps_joined() {
        // "F, then Hints" says what one press does; "Shift+F, then Force hints
        // mode" would be two sentences pretending to be a label.
        let hints = Action::Seq(vec![Action::Key(33.into()), Action::SetMode("hints".into())]);
        assert_eq!(derive_label(&hints), "F, then Hints");
        assert_eq!(action_string(&hints), "seq: key f; set_mode hints");
        assert_eq!(action_kind(&hints), "seq");

        let bg = Action::Seq(vec![
            Action::Key(KeyChord::parse("shift+f").unwrap()),
            Action::SetMode("hints".into()),
        ]);
        assert_eq!(derive_label(&bg), "Shift+F, then Hints");

        // `clear_mode` gets the same in-sequence shortening, and everything
        // that is not a mode verb reads exactly as it does on a row of its own.
        let out = Action::Seq(vec![Action::Key(1.into()), Action::ClearMode]);
        assert_eq!(derive_label(&out), "Escape, then automatic");
        let three = Action::Seq(vec![
            Action::Exec("omarchy-menu".into()),
            Action::Key(33.into()),
            Action::SetMode("hints".into()),
        ]);
        assert_eq!(derive_label(&three), "Omarchy menu, then F, then Hints");
    }

    #[test]
    fn a_transient_mode_says_so_in_its_row() {
        let c = crate::lua_config::load_str(
            r#"
            local h = hyprpad
            h.mode("hints"):transient { max_presses = 3,
                                        exit_on = { "b", "focus", "title", "click" },
                                        timeout_ms = 8000 }
            h.mode("browser").when(function(ctx) return ctx.focus.class == "google-chrome" end)
            h.mode("desktop")
            h.default_mode "desktop"
            h.bind("guide+l4", "Link hints", h.seq { h.key "f", h.set_mode "hints" })
                :only_in("browser")
            h.button("a", h.key "a"):only_in("hints")
            "#,
            "t.lua",
        )
        .expect("config should load");
        let s = Sheet::build(&c, None);

        let hints = s.modes.iter().find(|m| m.name == "hints").expect("a hints tab");
        assert_eq!(
            hints.transient.as_deref(),
            Some("transient · 3 presses · exit on B, focus, title, click · 8s")
        );
        assert!(!hints.has_rule, "a transient mode is entered, never resolved into");
        // The ordinary modes carry nothing, so the widget can tell them apart.
        assert!(s.modes.iter().find(|m| m.name == "browser").unwrap().transient.is_none());

        // The mode summary shows it beside the mode's other notes...
        let t = s.to_text();
        assert!(
            t.contains("hints (no rule, transient · 3 presses · exit on B, focus, title, click · 8s)"),
            "the mode row must carry the contract:\n{t}"
        );
        // ...and the chord that enters it reads as what it does.
        assert!(t.contains("Link hints"), "{t}");
        // The JSON carries it too, null where there is none.
        let j = s.to_json();
        assert!(j.contains(r#""transient": "transient · 3 presses"#), "{j}");
        assert!(j.contains(r#""transient": null"#), "{j}");
    }

    #[test]
    fn the_transient_note_shows_only_what_is_switched_on() {
        use crate::report::Button::B;
        // Off is the empty value, so a spec with two of the three ways out
        // turned off must not advertise them.
        let only_b = TransientSpec {
            max_presses: 0,
            exit_on: vec![TransientExit::Button(B)],
            timeout_ms: 0,
        };
        assert_eq!(transient_note(&only_b), "transient · exit on B");
        // One press is one press.
        let one = TransientSpec { max_presses: 1, exit_on: Vec::new(), timeout_ms: 500 };
        assert_eq!(transient_note(&one), "transient · 1 press · 0.5s");
        // And a mode with no way out at all still says it is transient — an
        // odd config, but not a silent one.
        let never = TransientSpec { max_presses: 0, exit_on: Vec::new(), timeout_ms: 0 };
        assert_eq!(transient_note(&never), "transient");
    }
}
