//! Modality: which named context the controller is in, and what that makes live.
//!
//! This subsumes [`crate::arbitrate`]'s binary "is a game focused" gate and
//! generalizes it per docs/13, with the owner's decisions of 2026-09-01 baked
//! in:
//!
//! * **A mode is a named context, nothing more.** It carries a selection rule
//!   and a `forward` flag; it does *not* carry category switches.
//! * **Guards are per binding, not per category.** Every `h.bind` / `h.button`
//!   / `h.osk_button`, and the `h.cursor` / `h.scroll` "virtual bindings",
//!   decide for themselves where they are live ([`crate::config::Guard`]).
//!   "Game passthrough" is then *"nothing but the guide chords is guarded into
//!   `game`"*, expressed one binding at a time.
//! * **Fullscreen is not a game trigger.** `ctx.focus.fullscreen` is available
//!   to a rule that explicitly wants it; nothing ships using it.
//! * **A manual override is first class** and beats the rules.
//!
//! ## Resolution
//!
//! ```text
//! manual override  >  first matching mode rule (definition order)  >  default_mode
//! ```
//!
//! Rules and `:when` guards are Lua predicates, and they run **on a context
//! change only** — a focus change, a rename of the focused window, a fullscreen
//! change, an overlay opening or closing, a manual override, a config reload,
//! or the periodic process-tree rescan. Never per input frame: the resolved
//! mode, the guard results, and the filtered button maps are all cached in this
//! struct, and the per-frame handlers only read them.
//!
//! ## The built-in behaviour
//!
//! A config that declares no modes at all — every `config.toml`, and a
//! `config.lua` that never calls `h.mode` — gets the daemon's original
//! behaviour: a `game` mode selected by [`Arbiter`]'s window-class match, with
//! the cursor, scroll and bare buttons live only on the desktop and the guide
//! chords live everywhere. So adopting the Lua front-end is opt-in twice over:
//! once for the file, once for the modes.

use crate::arbitrate::Arbiter;
use crate::config::{ButtonAction, Config, ModeState, OskAction};
use crate::gesture::GestureEvent;
use crate::report;
use std::collections::{BTreeSet, HashMap};

/// The name the built-in (no modes declared) behaviour uses for "a game window
/// holds focus", and the name a `config.lua` conventionally gives the same
/// mode. Only the built-in path depends on the spelling.
pub const BUILTIN_GAME: &str = "game";

/// The name the built-in behaviour uses for "ordinary desktop use", and the
/// default `default_mode`.
pub const BUILTIN_DESKTOP: &str = "desktop";

/// The observable world a mode rule selects on, whole.
///
/// Two sources, both from the compositor and both cheap: the focused *window*
/// ([`Focus`]) and the *overlays* on screen. The second exists because an
/// overlay is a context a focus-only engine cannot see — a layer-shell surface
/// takes the keyboard without any `activewindow` event behind it, so nothing
/// but `openlayer`/`closelayer` says hyprpad's own cheat sheet is up and modal.
///
/// Reaches Lua as `ctx`, one table per re-resolve.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The focused window. `ctx.focus`.
    pub focus: Focus,
    /// The namespaces of the layer-shell surfaces currently on screen —
    /// hyprpad's own overlays (`hyprpad-cheatsheet`, `hyprpad-osk`) and
    /// everyone else's (`omarchy-bar`, `omarchy-background`). `ctx.layers`.
    ///
    /// A namespace is all the wire gives us: `openlayer>>hyprpad-cheatsheet`
    /// carries no address and nothing else, so this is a set of names and not a
    /// map of surfaces. Ordered (a `BTreeSet`) so `ipairs(ctx.layers)` and
    /// `ctx.layers:list()` read the same way twice running — a rule that
    /// renders the set must not watch it reshuffle.
    pub layers: BTreeSet<String>,
}

/// The focused window a mode rule selects on.
///
/// Fed by `activewindow` from the compositor's event socket (class, title) plus
/// a one-shot `j/activewindow` query for the fields the event stream does not
/// carry (pid, fullscreen), and kept current under a *still-focused* window by
/// `windowtitle` ([`ModeEngine::title_changed`]). Reaches Lua as `ctx.focus`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    /// The focused window's class, or empty when focus is on the desktop.
    pub class: String,
    /// The focused window's title.
    pub title: String,
    /// The focused window's pid, when known. `None` means the daemon did not
    /// (or could not) ask — `ctx.focus:process_tree_has(..)` then answers
    /// `false` rather than guessing.
    pub pid: Option<i32>,
    /// Whether the focused window is fullscreen. **Not** a game signal: it is
    /// available to a rule that asks for it and nothing more (docs/13 decision
    /// #2 — a fullscreen video is not a game).
    pub fullscreen: bool,
}

/// Tracks context, resolves the active mode, and caches everything the
/// per-frame handlers need to know about it.
#[derive(Debug)]
pub struct ModeEngine {
    /// Everything the rules select on — the focused window and the open
    /// overlays — kept whole so a predicate never sees half of a change.
    ctx: Context,
    /// The manual override, top of the precedence. `Some` until a
    /// [`crate::config::Action::ClearMode`].
    manual: Option<String>,
    /// The built-in game-class matcher, used only when the config declares no
    /// modes. Keeping the original type here is deliberate: the no-modes path
    /// is *literally* the behaviour it always had, not a re-implementation.
    arbiter: Arbiter,
    /// The resolved snapshot every guard is evaluated against.
    state: ModeState,
    /// The active mode hands raw input to the virtual gamepad.
    forwards: bool,
    cursor: bool,
    scroll: bool,
    buttons: HashMap<report::Button, ButtonAction>,
    osk_buttons: HashMap<report::Button, OskAction>,
}

impl ModeEngine {
    /// Build an engine for `config` and resolve the initial mode (no window
    /// focused yet, so the default mode unless a rule matches an empty focus).
    pub fn new(config: &Config) -> ModeEngine {
        let mut me = ModeEngine {
            ctx: Context::default(),
            manual: None,
            arbiter: Arbiter::new(),
            state: ModeState::default(),
            forwards: false,
            cursor: true,
            scroll: true,
            buttons: HashMap::new(),
            osk_buttons: HashMap::new(),
        };
        me.refresh(config);
        me
    }

    // --- context changes (each returns "did the active mode change?") -----

    /// The focused window changed. Returns whether that moved the active mode,
    /// which the caller answers with the clean handoff.
    ///
    /// The focused window's fullscreen state is carried over; use
    /// [`context_changed`](Self::context_changed) when the caller knows it, so
    /// the rules see one complete new context rather than two partial ones.
    pub fn focus_changed(
        &mut self,
        config: &Config,
        class: &str,
        title: &str,
        pid: Option<i32>,
    ) -> bool {
        self.context_changed(
            config,
            Focus {
                class: class.to_string(),
                title: title.to_string(),
                pid,
                fullscreen: self.ctx.focus.fullscreen,
            },
        )
    }

    /// Replace the whole focus context and re-resolve **once**.
    ///
    /// One call, one re-resolve: a predicate must never see a half-updated
    /// context (the new window's pid against the old window's class), and a
    /// transient mismatch must never be reported as a mode transition and
    /// trigger a spurious handoff.
    pub fn context_changed(&mut self, config: &Config, focus: Focus) -> bool {
        self.arbiter.focus_changed(&focus.class);
        self.arbiter.set_fullscreen(focus.fullscreen);
        self.ctx.focus = focus;
        self.refresh(config)
    }

    /// The focused window **renamed itself** (Hyprland's `windowtitle` event)
    /// without focus moving.
    ///
    /// This is the case a focus-only engine cannot see: starting `claude` in an
    /// already-focused terminal changes nothing about *which* window is focused,
    /// only what it is called and what runs under it. So the new title is
    /// adopted and the focused window's process-tree cache is dropped — the
    /// rename is the compositor telling us the tree probably moved — and the
    /// rules run again exactly as they do on a focus change.
    ///
    /// A rename to the title we already hold is not a context change and
    /// re-resolves nothing, which is what makes the compositor's two events per
    /// rename (`windowtitle` then `windowtitlev2`) cost one re-resolve.
    pub fn title_changed(&mut self, config: &Config, title: &str) -> bool {
        if self.ctx.focus.title == title {
            return false;
        }
        self.ctx.focus.title = title.to_string();
        self.forget_process_tree(config);
        self.refresh(config)
    }

    /// Re-walk the focused window's process tree and re-resolve — the periodic
    /// `process_rescan_ms` sweep, for the programs that start without renaming
    /// anything.
    ///
    /// A no-op (and no `/proc` read at all) unless [`process_rescan_useful`]
    /// says a walk could change an answer.
    ///
    /// [`process_rescan_useful`]: Self::process_rescan_useful
    pub fn rescan_processes(&mut self, config: &Config) -> bool {
        if !self.process_rescan_useful(config) {
            return false;
        }
        self.forget_process_tree(config);
        self.refresh(config)
    }

    /// Whether re-walking the focused window's process tree could change the
    /// resolution — the gate the daemon's rescan timer is armed behind.
    ///
    /// Three ways to answer no, all of them free: no declared modes (the
    /// built-in path selects on window class alone), no predicate anywhere in
    /// the config that mentions `process_tree_has`, or no focused pid to walk
    /// from. The owner's rule is that a config which never asks about processes
    /// never polls for them.
    pub fn process_rescan_useful(&self, config: &Config) -> bool {
        self.ctx.focus.pid.is_some()
            && !config.modes().is_empty()
            && config.lua().is_some_and(|rt| rt.walks_process_tree())
    }

    /// Drop the focused window's cached `/proc` walk, so the next predicate that
    /// asks re-reads it. No-op for a TOML config (no Lua, no walk).
    fn forget_process_tree(&self, config: &Config) {
        if let Some(rt) = config.lua() {
            rt.forget_process_tree();
        }
    }

    /// The focused window's fullscreen state changed.
    pub fn set_fullscreen(&mut self, config: &Config, on: bool) -> bool {
        self.arbiter.set_fullscreen(on);
        self.ctx.focus.fullscreen = on;
        self.refresh(config)
    }

    /// A layer-shell overlay appeared or disappeared (Hyprland's `openlayer` /
    /// `closelayer`, [`crate::hypr::HyprEvent::Layer`]).
    ///
    /// The context source a focus-only engine is blind to. The cheat sheet
    /// takes the keyboard the moment it is drawn and no `activewindow` event
    /// fires for it, so without this the daemon has no way of knowing that its
    /// own modal overlay is up — which is what makes "B closes the sheet"
    /// expressible as an ordinary guarded binding instead of a special case
    /// wired into the input path.
    ///
    /// An event that does not move the set — a repeat `openlayer` for a
    /// namespace already open, a `closelayer` for one we never saw — is not a
    /// context change and re-resolves nothing, exactly as a rename to the title
    /// we already hold does not.
    ///
    /// A config that declares no modes — the TOML front-end, which is the only
    /// one that can have none — never tracks layers at all. It resolves on
    /// window class alone, so no overlay can change its answer, and the set
    /// stays empty however many come and go: the same "never pay for what you
    /// did not ask for" rule as the focused-pid query.
    pub fn layer_changed(&mut self, config: &Config, namespace: &str, open: bool) -> bool {
        if config.modes().is_empty() {
            return false;
        }
        let moved = if open {
            self.ctx.layers.insert(namespace.to_string())
        } else {
            self.ctx.layers.remove(namespace)
        };
        moved && self.refresh(config)
    }

    /// Adopt the overlays already on screen (the daemon's startup seed from
    /// `j/layers`, [`crate::hypr::Hypr::open_layers`]).
    ///
    /// The event socket announces only changes, so a daemon started — or
    /// restarted, mid-session — while the cheat sheet is up would otherwise
    /// believe nothing is showing until the sheet was closed and reopened.
    /// Same gating as [`layer_changed`](Self::layer_changed), and the same
    /// "seeding is one complete context change" contract as the focus seed.
    pub fn seed_layers(&mut self, config: &Config, layers: BTreeSet<String>) -> bool {
        if config.modes().is_empty() || self.ctx.layers == layers {
            return false;
        }
        self.ctx.layers = layers;
        self.refresh(config)
    }

    /// Force `name` as the active mode, overriding the rules
    /// ([`crate::config::Action::SetMode`]).
    pub fn set_mode(&mut self, config: &Config, name: &str) -> bool {
        self.manual = Some(name.to_string());
        // The built-in path has no mode table to consult, so tell the arbiter
        // directly: forcing anything but `game` is the "force desktop" override
        // it already models.
        self.arbiter.set_force_desktop(name != BUILTIN_GAME);
        self.refresh(config)
    }

    /// Drop the manual override so the rules decide again
    /// ([`crate::config::Action::ClearMode`]).
    pub fn clear_mode(&mut self, config: &Config) -> bool {
        self.manual = None;
        self.arbiter.set_force_desktop(false);
        self.refresh(config)
    }

    /// Re-resolve against a freshly loaded config (the `hyprpad reload` path).
    ///
    /// The context is kept — the same window is still focused — but every rule
    /// and guard is re-evaluated against the new config, and a manual override
    /// naming a mode the new config does not declare is dropped rather than
    /// stranding the daemon in a mode that no longer exists.
    pub fn reconfigure(&mut self, config: &Config) -> bool {
        if let Some(name) = self.manual.clone() {
            if !config.modes().is_empty() && !config.modes().iter().any(|m| m.name == name) {
                eprintln!(
                    "hyprpad: manual mode override '{name}' is not declared by the new config; \
                     dropping it"
                );
                self.manual = None;
                self.arbiter.set_force_desktop(false);
            }
        }
        self.refresh(config)
    }

    // --- reads (per frame; all cached) ------------------------------------

    /// The active mode's name.
    pub fn active(&self) -> &str {
        self.state.active()
    }

    /// The resolved snapshot, for `Config::resolve_in` and friends.
    pub fn state(&self) -> &ModeState {
        &self.state
    }

    /// The focused window's title as the rules last saw it. The daemon compares
    /// against it to tell a real rename from a repeat of the one it already
    /// holds.
    pub fn focus_title(&self) -> &str {
        &self.ctx.focus.title
    }

    /// The layer-shell namespaces the rules currently see. Always empty for a
    /// config that declares no modes, which never tracks them.
    pub fn layers(&self) -> &BTreeSet<String> {
        &self.ctx.layers
    }

    /// Whether the active mode hands raw input to the game
    /// (`h.mode("game", { forward = true })`, or the built-in game match).
    pub fn forwards(&self) -> bool {
        self.forwards
    }

    /// Whether the ambient desktop layer has actually let go of the pads in
    /// this mode — i.e. neither the cursor nor scroll is live.
    ///
    /// This is the mode model's spelling of the old `Arbiter::suppressed()`,
    /// and the second half of the forwarding gate: forwarding requires both
    /// that the mode *wants* it and that the desktop layer has yielded, which
    /// is what keeps ranks 3 and 4 of the precedence mutually exclusive
    /// (see `crate::run::gamepad_forwarding`).
    pub fn desktop_yielded(&self) -> bool {
        !self.cursor && !self.scroll
    }

    /// Whether the right pad drives the desktop cursor in this mode.
    pub fn cursor_enabled(&self) -> bool {
        self.cursor
    }

    /// Whether the left pad scrolls in this mode.
    pub fn scroll_enabled(&self) -> bool {
        self.scroll
    }

    /// The bare-button bindings live in this mode: what each button does
    /// ([`ButtonAction`]), held with it or fired on its press edge.
    pub fn buttons(&self) -> &HashMap<report::Button, ButtonAction> {
        &self.buttons
    }

    /// What the buttons do while the on-screen keyboard is up, in this mode:
    /// the built-in Deck map with the config's `osk_buttons` layered over it
    /// ([`Config::osk_buttons_in`]).
    pub fn osk_buttons(&self) -> &HashMap<report::Button, OskAction> {
        &self.osk_buttons
    }

    /// Whether the binding on `ev` is live in this mode.
    ///
    /// `guide_scoped` is the caller's "this gesture carries the guide
    /// modifier" judgement, which the built-in path honours exactly as the
    /// original `arbiter.suppressed() && !is_guide_scoped(..)` line did. With
    /// declared modes the per-binding guard decides instead — including for
    /// guide chords, which is the whole point of per-binding granularity.
    pub fn allows_gesture(&self, config: &Config, ev: &GestureEvent, guide_scoped: bool) -> bool {
        if config.modes().is_empty() {
            return guide_scoped || !self.arbiter.suppressed();
        }
        config.gesture_guard(ev).allows(&self.state)
    }

    // --- resolution -------------------------------------------------------

    /// Re-resolve the active mode and rebuild every cached view. Returns
    /// whether the active mode actually changed.
    fn refresh(&mut self, config: &Config) -> bool {
        let before = self.state.active().to_string();

        if config.modes().is_empty() {
            self.refresh_builtin(config);
        } else {
            self.refresh_declared(config);
        }

        let changed = self.state.active() != before;
        if changed && self.forwards && !self.desktop_yielded() {
            // A mode that forwards while the cursor or scroll is still live
            // would have two layers driving at once, so the forwarding gate
            // holds off — silently, which would be a mystery. Say so.
            eprintln!(
                "warning: mode '{}' sets forward = true but leaves the cursor/scroll live; \
                 guard them (h.cursor {{ only_in = {{ \"desktop\" }} }}) or the virtual \
                 gamepad will not engage",
                self.state.active()
            );
        }
        changed
    }

    /// The no-modes-declared path: today's `Arbiter` behaviour, expressed in
    /// the mode vocabulary.
    fn refresh_builtin(&mut self, config: &Config) {
        let game = match self.manual.as_deref() {
            Some(name) => name == BUILTIN_GAME,
            None => self.arbiter.suppressed(),
        };
        let active = match (&self.manual, game) {
            (Some(name), _) => name.clone(),
            (None, true) => BUILTIN_GAME.to_string(),
            (None, false) => BUILTIN_DESKTOP.to_string(),
        };
        self.state = ModeState::new(active, Vec::new());
        self.cursor = !game;
        self.scroll = !game;
        self.forwards = game;
        self.buttons = if game { HashMap::new() } else { config.buttons().clone() };
        // The keyboard's table is the same in both modes (a TOML config has
        // no guards), and it is the layered map, not the config's raw entries.
        self.osk_buttons = config.osk_buttons_in(&self.state);
    }

    /// The declared-modes path: evaluate every predicate once, pick a mode,
    /// then filter every binding through its guard.
    fn refresh_declared(&mut self, config: &Config) {
        // One pass over the predicates, so a predicate shared by a rule and a
        // guard runs exactly once per context change.
        let slots = config.predicate_slots();
        let mut results = vec![false; slots];
        if let Some(rt) = config.lua() {
            for (i, slot) in results.iter_mut().enumerate() {
                *slot = rt.eval_predicate(i, &self.ctx);
            }
        }

        // Precedence: manual override > first matching rule > default_mode.
        let active = match &self.manual {
            Some(name) => name.clone(),
            None => config
                .modes()
                .iter()
                .find(|m| m.rule.is_some_and(|i| results.get(i).copied().unwrap_or(false)))
                .map(|m| m.name.clone())
                .unwrap_or_else(|| config.default_mode().to_string()),
        };

        self.forwards = config
            .modes()
            .iter()
            .find(|m| m.name == active)
            .is_some_and(|m| m.forward);

        self.state = ModeState::new(active, results);
        self.cursor = config.cursor_enabled_in(&self.state);
        self.scroll = config.scroll_enabled_in(&self.state);
        self.buttons = config.buttons_in(&self.state);
        self.osk_buttons = config.osk_buttons_in(&self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Action;
    use crate::config::ButtonAction::Hold;
    use crate::lua_config::load_str;
    use crate::report::Button;

    fn lua(src: &str) -> Config {
        load_str(src, "test.lua").expect("config should load")
    }

    /// The file the owner will actually drop in, loaded from the repo. Several
    /// tests run end to end against it, so it is worth exactly one reader.
    fn sample_config() -> Config {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/hyprpad.lua"
        ))
        .expect("config/hyprpad.lua");
        load_str(&src, "config/hyprpad.lua").expect("sample config should load")
    }

    /// The shape the shipped sample uses: a game mode selected by class, with
    /// the ambient handlers guarded onto the desktop and the chords unguarded.
    const GAME_CONFIG: &str = r#"
        local h = hyprpad
        h.mode("game", { forward = true }).when(function(ctx)
          return ctx.focus.class:match("^steam_app_") ~= nil
              or ctx.focus.class == "steam"
        end)
        h.mode("claude").when(function(ctx)
          return ctx.focus:process_tree_has("definitely-not-running-anywhere")
        end)
        h.mode("desktop")
        h.default_mode "desktop"

        h.cursor { only_in = { "desktop" } }
        h.scroll { only_in = { "desktop" } }
        h.button("dpad_up", h.key "up"):only_in("desktop")
        h.osk_button("y", h.key "space")
        h.bind("guide+r1", h.workspace "+1")
        h.bind("guide+l1", h.workspace "-1"):not_in("game")
    "#;

    #[test]
    fn no_modes_declared_keeps_the_original_arbiter_behaviour() {
        let c = Config::load_default();
        let mut m = ModeEngine::new(&c);
        assert_eq!(m.active(), BUILTIN_DESKTOP);
        assert!(m.cursor_enabled() && m.scroll_enabled() && !m.forwards());
        assert_eq!(m.buttons().len(), c.buttons().len());

        assert!(m.focus_changed(&c, "steam_app_413080", "Portal 2", None));
        assert_eq!(m.active(), BUILTIN_GAME);
        assert!(!m.cursor_enabled() && !m.scroll_enabled());
        assert!(m.forwards() && m.desktop_yielded());
        assert!(m.buttons().is_empty());

        assert!(m.focus_changed(&c, "foot", "shell", None));
        assert_eq!(m.active(), BUILTIN_DESKTOP);
        assert!(!m.forwards());
    }

    #[test]
    fn the_builtin_path_still_lets_guide_chords_through_a_game() {
        let c = Config::load_default();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "gamescope", "", None);
        let chord = GestureEvent::GuideChord(Button::BumperR1);
        assert!(m.allows_gesture(&c, &chord, true), "guide chords survive a game");
        assert!(!m.allows_gesture(&c, &chord, false), "ambient gestures do not");
    }

    #[test]
    fn declared_modes_resolve_first_match_in_definition_order() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        assert_eq!(m.active(), "desktop", "no rule matches an empty focus");

        assert!(m.focus_changed(&c, "steam_app_413080", "Portal 2", None));
        assert_eq!(m.active(), "game");

        assert!(m.focus_changed(&c, "org.mozilla.firefox", "web", None));
        assert_eq!(m.active(), "desktop");

        assert!(!m.focus_changed(&c, "foot", "shell", None), "desktop -> desktop is no change");
    }

    #[test]
    fn a_matching_rule_beats_the_default_and_the_default_catches_the_rest() {
        let c = lua(
            r#"
            hyprpad.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class == "steam"
            end)
            hyprpad.mode("desktop")
            hyprpad.default_mode "desktop"
            "#,
        );
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam", "Steam", None);
        assert_eq!(m.active(), "game");
        m.focus_changed(&c, "anything-else", "", None);
        assert_eq!(m.active(), "desktop");
    }

    #[test]
    fn the_manual_override_beats_a_matching_rule() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam_app_1", "", None);
        assert_eq!(m.active(), "game");

        // Force the desktop back over a running game...
        assert!(m.set_mode(&c, "desktop"));
        assert_eq!(m.active(), "desktop");
        assert!(m.cursor_enabled() && !m.forwards());

        // ...and it sticks across a focus change, because it is an override.
        m.focus_changed(&c, "steam_app_2", "", None);
        assert_eq!(m.active(), "desktop");

        // Clearing it hands control back to the rules, which still match.
        assert!(m.clear_mode(&c));
        assert_eq!(m.active(), "game");
        assert!(m.forwards());
    }

    #[test]
    fn the_manual_override_works_on_the_builtin_path_too() {
        let c = Config::load_default();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam_app_1", "", None);
        assert!(m.forwards());
        m.set_mode(&c, BUILTIN_DESKTOP);
        assert_eq!(m.active(), BUILTIN_DESKTOP);
        assert!(m.cursor_enabled() && !m.forwards());
        m.clear_mode(&c);
        assert!(m.forwards());
    }

    #[test]
    fn guards_gate_bindings_per_thing_not_per_category() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "shell", None);

        // Desktop: everything live.
        assert!(m.cursor_enabled() && m.scroll_enabled());
        assert_eq!(m.buttons().get(&Button::DpadUp), Some(&Hold(103)));
        assert!(m.allows_gesture(&c, &GestureEvent::GuideChord(Button::BumperL1), true));

        m.focus_changed(&c, "steam_app_1", "", None);
        // Game: the ambient handlers are off, the guarded chord is off, and the
        // unguarded chord — the escape hatch — is still live.
        assert!(!m.cursor_enabled() && !m.scroll_enabled());
        assert!(m.buttons().is_empty());
        assert!(!m.allows_gesture(&c, &GestureEvent::GuideChord(Button::BumperL1), true));
        assert!(m.allows_gesture(&c, &GestureEvent::GuideChord(Button::BumperR1), true));
        // An unguarded osk_button is live everywhere, which is the point of
        // per-binding granularity: `osk` is not a category that switches.
        assert_eq!(m.osk_buttons().get(&Button::Y), Some(&OskAction::Key(57)));
    }

    #[test]
    fn a_guarded_binding_resolves_to_none_rather_than_firing() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam_app_1", "", None);
        assert_eq!(
            c.resolve_in(&GestureEvent::GuideChord(Button::BumperL1), m.state()),
            Action::None
        );
        assert_ne!(
            c.resolve_in(&GestureEvent::GuideChord(Button::BumperR1), m.state()),
            Action::None
        );
    }

    #[test]
    fn a_runaway_rule_does_not_wedge_resolution() {
        let c = lua(
            r#"
            hyprpad.mode("spin").when(function(ctx) while true do end end)
            hyprpad.mode("desktop")
            hyprpad.default_mode "desktop"
            "#,
        );
        let t0 = std::time::Instant::now();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "", None);
        // Cut by the watchdog, counted as no match, so the default wins.
        assert_eq!(m.active(), "desktop");
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn reconfigure_re_resolves_against_the_new_rules() {
        let old = lua(
            r#"
            hyprpad.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class == "steam"
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&old);
        m.focus_changed(&old, "steam", "Steam", None);
        assert_eq!(m.active(), "game");

        // A reload whose rules no longer match this window moves us out.
        let new = lua(
            r#"
            hyprpad.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class == "gamescope"
            end)
            hyprpad.mode("desktop")
            "#,
        );
        assert!(m.reconfigure(&new));
        assert_eq!(m.active(), "desktop");
    }

    #[test]
    fn reconfigure_drops_an_override_the_new_config_no_longer_declares() {
        let old = lua(r#"hyprpad.mode("cinema") hyprpad.mode("desktop")"#);
        let mut m = ModeEngine::new(&old);
        m.set_mode(&old, "cinema");
        assert_eq!(m.active(), "cinema");

        let new = lua(r#"hyprpad.mode("desktop")"#);
        m.reconfigure(&new);
        assert_eq!(m.active(), "desktop");
    }

    /// A reload must not lose sight of an overlay that is still on screen: the
    /// sheet does not close because the config was re-read, so the new rules
    /// have to be resolved against the same overlays the old ones saw.
    #[test]
    fn a_reload_keeps_the_overlays_that_are_still_up() {
        let old = lua(
            r#"
            hyprpad.mode("cheatsheet").when(function(ctx)
              return ctx.layers:has("hyprpad-cheatsheet")
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&old);
        m.layer_changed(&old, "hyprpad-cheatsheet", true);
        assert_eq!(m.active(), "cheatsheet");

        // The same rule under a new name: the sheet is still up, so we land in
        // it again without waiting for the overlay to be closed and reopened.
        let new = lua(
            r#"
            hyprpad.mode("sheet").when(function(ctx)
              return ctx.layers:has("hyprpad-cheatsheet")
            end)
            hyprpad.mode("desktop")
            "#,
        );
        assert!(m.reconfigure(&new));
        assert_eq!(m.active(), "sheet");
        assert!(m.layers().contains("hyprpad-cheatsheet"));
    }

    #[test]
    fn fullscreen_is_available_to_a_rule_but_triggers_nothing_by_itself() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "mpv", "Big Buck Bunny", None);
        assert!(!m.set_fullscreen(&c, true), "fullscreen alone is not a game");
        assert_eq!(m.active(), "desktop");

        let opt_in = lua(
            r#"
            hyprpad.mode("cinema").when(function(ctx) return ctx.focus.fullscreen end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&opt_in);
        m.focus_changed(&opt_in, "mpv", "", None);
        assert_eq!(m.active(), "desktop");
        assert!(m.set_fullscreen(&opt_in, true));
        assert_eq!(m.active(), "cinema");
    }

    #[test]
    fn the_shipped_sample_config_delivers_the_game_passthrough() {
        // End to end on the file the owner will actually drop in: the game
        // classes select `game`, everything ambient goes quiet, the guide
        // chords survive, and the pad is handed to the game.
        let c = sample_config();
        let mut m = ModeEngine::new(&c);

        m.focus_changed(&c, "foot", "ajg@framework", None);
        assert_eq!(m.active(), "desktop");
        assert!(m.cursor_enabled() && m.scroll_enabled() && !m.forwards());
        assert_eq!(m.buttons().len(), 9, "d-pad + A + B + the three mouse clicks");
        // The clicks are bindings now, and on the desktop they are live: pad
        // click and R2 are a left click, L2 a right click.
        assert_eq!(m.buttons().get(&report::Button::PadRightClick), Some(&Hold(0x110)));
        assert_eq!(m.buttons().get(&report::Button::TriggerR2Full), Some(&Hold(0x110)));
        assert_eq!(m.buttons().get(&report::Button::TriggerL2Full), Some(&Hold(0x111)));

        for class in [
            "steam_app_413080",
            "steam_proton_x",
            "gamescope",
            "steam",
            "steamwebhelper",
            "Steam_App_570", // the class match is case-insensitive
        ] {
            m.focus_changed(&c, class, "", None);
            assert_eq!(m.active(), "game", "class {class} should select game mode");
            assert!(!m.cursor_enabled() && !m.scroll_enabled(), "{class}");
            assert!(m.buttons().is_empty(), "{class}");
            assert!(m.forwards() && m.desktop_yielded(), "{class}");
            // The escape hatch: every guide chord still resolves.
            assert!(m.allows_gesture(&c, &GestureEvent::GuideChord(Button::BumperR1), true));
            assert_ne!(
                c.resolve_in(&GestureEvent::GuideChord(Button::Y), m.state()),
                Action::None,
                "guide+Y must still raise the keyboard over a game"
            );
            // ...and so do the OSK helpers it needs once it is up.
            assert_eq!(m.osk_buttons().get(&Button::Y), Some(&OskAction::Key(57)));
        }

        // A fullscreen video is not a game (docs/13 decision #2).
        m.focus_changed(&c, "mpv", "Big Buck Bunny", None);
        m.set_fullscreen(&c, true);
        assert_eq!(m.active(), "desktop");
        assert!(m.cursor_enabled() && !m.forwards());
    }

    /// The overlay path end to end, on the file the owner will actually drop
    /// in: raising the cheat sheet is a context change all by itself, it wins
    /// over every other rule, and it re-points one button without disturbing
    /// anything else.
    #[test]
    fn the_cheat_sheet_is_a_mode_and_b_closes_it() {
        let c = sample_config();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "ajg@framework", None);
        assert_eq!(m.active(), "desktop");
        assert_eq!(m.buttons().get(&Button::B), Some(&Hold(14)), "B is Backspace here");

        // The sheet comes up. No focus event fires for a layer surface, so this
        // is the only thing that says so — and it is one transition.
        assert!(m.layer_changed(&c, "hyprpad-cheatsheet", true));
        assert_eq!(m.active(), "cheatsheet");
        assert_eq!(m.buttons().get(&Button::B), Some(&Hold(1)), "B is Escape under the sheet");
        // The sheet's own controls, and nothing else: B closes it, and the
        // bumpers page its tabs (Left/Right, which is what the widget listens
        // for). The desktop's arrows and Enter are gone, as they must be.
        assert_eq!(m.buttons().get(&Button::BumperL1), Some(&Hold(105)), "L1 pages back");
        assert_eq!(m.buttons().get(&Button::BumperR1), Some(&Hold(106)), "R1 pages on");
        assert_eq!(m.buttons().len(), 3, "nothing else is guarded into cheatsheet");

        // A repeat `openlayer` is not a context change: no re-resolve, and the
        // caller runs no second handoff.
        assert!(!m.layer_changed(&c, "hyprpad-cheatsheet", true));
        // Nor is somebody else's overlay coming and going.
        assert!(!m.layer_changed(&c, "omarchy-bar", true));
        assert_eq!(m.active(), "cheatsheet");
        assert!(!m.layer_changed(&c, "omarchy-bar", false));
        assert_eq!(m.active(), "cheatsheet");

        // Escape lands, the sheet closes, and the desktop takes B back.
        assert!(m.layer_changed(&c, "hyprpad-cheatsheet", false));
        assert_eq!(m.active(), "desktop");
        assert_eq!(m.buttons().get(&Button::B), Some(&Hold(14)));
        // Closing what is not open re-resolves nothing.
        assert!(!m.layer_changed(&c, "hyprpad-cheatsheet", false));
    }

    /// The sheet is *modal*: it is drawn over whatever is focused, so it has to
    /// beat the game rule — the sheet has the keyboard, the game does not.
    #[test]
    fn the_cheat_sheet_wins_over_a_game() {
        let c = sample_config();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam_app_413080", "Portal 2", None);
        assert_eq!(m.active(), "game");
        assert!(m.forwards() && m.buttons().is_empty());

        assert!(m.layer_changed(&c, "hyprpad-cheatsheet", true));
        assert_eq!(m.active(), "cheatsheet");
        assert!(!m.forwards(), "the sheet has the keyboard, not the game");
        assert_eq!(m.buttons().get(&Button::B), Some(&Hold(1)));

        // ...and the game gets everything back when the sheet goes away. The
        // focused window never moved.
        assert!(m.layer_changed(&c, "hyprpad-cheatsheet", false));
        assert_eq!(m.active(), "game");
        assert!(m.forwards() && m.desktop_yielded());
    }

    #[test]
    fn ctx_layers_answers_has_for_what_is_open_and_only_that() {
        let c = lua(
            r#"
            hyprpad.mode("sheet").when(function(ctx)
              return ctx.layers:has("hyprpad-cheatsheet")
            end)
            hyprpad.mode("osk").when(function(ctx)
              return ctx.layers:has("hyprpad-osk")
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&c);
        assert_eq!(m.active(), "desktop", "nothing is showing yet");

        m.layer_changed(&c, "hyprpad-osk", true);
        assert_eq!(m.active(), "osk", "`has` is true for the one that is open");
        // A near-miss is a miss: no prefix or substring matching.
        m.layer_changed(&c, "hyprpad-cheatsheet-preview", true);
        assert_eq!(m.active(), "osk");

        // Both open: definition order decides, as for every other rule.
        m.layer_changed(&c, "hyprpad-cheatsheet", true);
        assert_eq!(m.active(), "sheet");
        m.layer_changed(&c, "hyprpad-cheatsheet", false);
        assert_eq!(m.active(), "osk");
        m.layer_changed(&c, "hyprpad-osk", false);
        assert_eq!(m.active(), "desktop");
    }

    /// The startup seed, driven off the real `j/layers` reply captured from
    /// this machine with the sheet up: a daemon restarted under the sheet knows
    /// it is there.
    #[test]
    fn a_daemon_started_under_the_sheet_seeds_the_mode_from_the_layers_reply() {
        let c = sample_config();
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "ajg@framework", None);
        assert_eq!(m.active(), "desktop");

        let open = crate::hypr::parse_layer_namespaces(crate::hypr::CAPTURED_LAYERS_JSON);
        assert!(m.seed_layers(&c, open.clone()));
        assert_eq!(m.active(), "cheatsheet");
        assert_eq!(m.buttons().get(&Button::B), Some(&Hold(1)));
        // Everyone else's overlays came along and changed nothing.
        assert!(m.layers().contains("omarchy-bar"));
        // Seeding the same set again is not a context change.
        assert!(!m.seed_layers(&c, open));

        // And the ordinary close event still gets us out of it, so the seed
        // leaves the engine in the same state the events would have.
        assert!(m.layer_changed(&c, "hyprpad-cheatsheet", false));
        assert_eq!(m.active(), "desktop");
    }

    #[test]
    fn a_config_with_no_modes_never_tracks_layers() {
        // Nothing a layer could change, so nothing is remembered — the same
        // "never pay for what you did not ask for" rule as the pid query.
        let toml = Config::load_default();
        let mut m = ModeEngine::new(&toml);
        assert!(!m.layer_changed(&toml, "hyprpad-cheatsheet", true));
        assert!(m.layers().is_empty(), "the set must stay empty");
        assert_eq!(m.active(), BUILTIN_DESKTOP);

        let open = crate::hypr::parse_layer_namespaces(crate::hypr::CAPTURED_LAYERS_JSON);
        assert!(!m.seed_layers(&toml, open));
        assert!(m.layers().is_empty(), "not even the startup seed lands");
        // Which is the whole point: the built-in path resolves on window class,
        // so an overlay cannot change its answer and the daemon never asks.
        assert!(!m.layer_changed(&toml, "hyprpad-cheatsheet", false));
        assert_eq!(m.active(), BUILTIN_DESKTOP);
    }

    /// The gap this whole path exists to close: `claude` starts in the terminal
    /// that already has focus, so no focus event ever fires — only a rename.
    #[test]
    fn a_title_change_on_the_focused_window_re_resolves() {
        let c = lua(
            r#"
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus.title:match("claude") ~= nil
            end)
            hyprpad.mode("desktop")
            hyprpad.default_mode "desktop"
            "#,
        );
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "ajg@framework:~/code/hyprpad", None);
        assert_eq!(m.active(), "desktop");

        // The terminal renames itself the moment the program starts.
        assert!(m.title_changed(&c, "claude — hyprpad"));
        assert_eq!(m.active(), "claude");

        // The compositor sends two events per rename; the second is not a
        // context change and must not re-resolve (or re-run the handoff).
        assert!(!m.title_changed(&c, "claude — hyprpad"));

        // And the way back out is the same event.
        assert!(m.title_changed(&c, "ajg@framework:~/code/hyprpad"));
        assert_eq!(m.active(), "desktop");
    }

    #[test]
    fn a_title_change_that_matches_nothing_moves_no_mode() {
        let c = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "foot", "shell", None);
        assert!(!m.title_changed(&c, "a different shell"));
        assert_eq!(m.active(), "desktop");
    }

    /// The title-less case: a program that starts under the focused window
    /// without renaming it. The periodic sweep re-walks `/proc` and the rule
    /// flips — the same transition a focus change would have produced.
    #[test]
    fn the_periodic_rescan_sees_a_child_process_appear() {
        let c = lua(
            r#"
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus:process_tree_has("hyprpad-rescan-probe")
            end)
            hyprpad.mode("desktop")
            hyprpad.default_mode "desktop"
            "#,
        );
        let mut m = ModeEngine::new(&c);
        // Pretend this test process is the focused window.
        let me = std::process::id() as i32;
        m.focus_changed(&c, "foot", "shell", Some(me));
        assert_eq!(m.active(), "desktop", "nothing is running under us yet");
        assert!(m.process_rescan_useful(&c), "the rule walks the tree, so the sweep is armed");

        // The walk matches on `"<comm> <cmdline>"`, so a `sleep` wearing the
        // probe's name as its `argv[0]` is a distinguishable stand-in for the
        // program the owner actually cares about — no such binary required.
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg0("hyprpad-rescan-probe")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        // `spawn` returns as soon as the fork happens; the child may not have
        // exec'd yet, so give the rescan a few tries rather than racing it.
        let mut flipped = false;
        for _ in 0..50 {
            flipped = m.rescan_processes(&c);
            if flipped {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(flipped, "the rescan must see the new child");
        assert_eq!(m.active(), "claude");
        // A second rescan with nothing new is not a transition.
        assert!(!m.rescan_processes(&c));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_rescan_never_walks_for_a_config_that_never_asks() {
        // No mention of `process_tree_has` anywhere: nothing a walk could
        // change, so the daemon must never arm the timer.
        let title_only = lua(
            r#"
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus.title:match("claude") ~= nil
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&title_only);
        m.focus_changed(&title_only, "foot", "shell", Some(std::process::id() as i32));
        assert!(!m.process_rescan_useful(&title_only));
        assert!(!m.rescan_processes(&title_only));

        // A TOML config (no Lua at all, built-in class matching) likewise.
        let toml = Config::load_default();
        let mut m = ModeEngine::new(&toml);
        m.focus_changed(&toml, "foot", "shell", Some(std::process::id() as i32));
        assert!(!m.process_rescan_useful(&toml));

        // And a config that *does* ask still needs a pid to walk from.
        let walker = lua(GAME_CONFIG);
        let mut m = ModeEngine::new(&walker);
        m.focus_changed(&walker, "foot", "shell", None);
        assert!(!m.process_rescan_useful(&walker), "no focused pid, nothing to walk");
        m.focus_changed(&walker, "foot", "shell", Some(std::process::id() as i32));
        assert!(m.process_rescan_useful(&walker));
    }

    #[test]
    fn a_forwarding_mode_that_leaves_the_cursor_live_does_not_claim_the_pad() {
        // The gate needs both halves; this is the case the warning is about.
        let c = lua(
            r#"
            hyprpad.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class == "steam"
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let mut m = ModeEngine::new(&c);
        m.focus_changed(&c, "steam", "", None);
        assert!(m.forwards());
        assert!(!m.desktop_yielded(), "cursor is unguarded, so the desktop still holds the pads");
    }
}
