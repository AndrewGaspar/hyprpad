//! Game-focus arbitration: decide whether the desktop-control layer should act.
//!
//! Gamepad input has no compositor focus routing (docs/02), so the daemon must
//! decide for itself whether a guide gesture drives the window manager or is
//! left alone for a focused game. The rule (docs/08 input contract): while a
//! game holds focus, desktop gestures are suppressed; guide chords still belong
//! to the desktop layer, but plain in-game controller use must not trigger
//! workspace flicks &c.
//!
//! This module is deliberately decoupled from the Hyprland event types: the
//! main loop translates compositor events into `focus_changed` / `fullscreen`
//! calls, and asks `suppressed()` before executing a desktop action.

/// Window classes that indicate a game/Steam surface currently has focus.
/// Steam launches native and Proton titles as `steam_app_<id>`; Proton-direct
/// and gamescope surfaces are matched by prefix too.
const GAME_CLASS_PREFIXES: &[&str] = &["steam_app_", "steam_proton", "gamescope"];

/// Tracks focus/fullscreen state and answers whether desktop gestures should be
/// suppressed right now.
#[derive(Debug, Default)]
pub struct Arbiter {
    focused_class: String,
    fullscreen: bool,
    /// When true, suppression is forced off regardless of focus (e.g. the user
    /// explicitly toggled the desktop layer on). Reserved for a future manual
    /// override; defaults to false.
    force_desktop: bool,
}

impl Arbiter {
    pub fn new() -> Self {
        Arbiter::default()
    }

    /// Update on a focus change. `class` is the Hyprland window class of the
    /// newly focused window (empty string when focus is lost to the desktop).
    pub fn focus_changed(&mut self, class: &str) {
        self.focused_class = class.to_string();
    }

    /// Update on a fullscreen state change of the focused window.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
    }

    /// Manual override: force the desktop layer active even over a game.
    pub fn set_force_desktop(&mut self, on: bool) {
        self.force_desktop = on;
    }

    /// Is the currently focused window a game/Steam surface?
    pub fn game_focused(&self) -> bool {
        let c = self.focused_class.to_ascii_lowercase();
        GAME_CLASS_PREFIXES.iter().any(|p| c.starts_with(p))
    }

    /// Should desktop-control gestures be suppressed right now?
    ///
    /// Suppressed when a game is focused and not overridden. Guide *chords* may
    /// still be honored by the caller even when this is true — this governs the
    /// ambient desktop gestures (bare stick flicks, non-guide bindings); the
    /// guide-held layer is the caller's policy. Kept conservative: a fullscreen
    /// game always suppresses; a windowed game also suppresses (the user is
    /// still playing) unless overridden.
    pub fn suppressed(&self) -> bool {
        if self.force_desktop {
            return false;
        }
        self.game_focused()
    }

    /// Convenience: is a game running fullscreen right now (the strongest
    /// "hands off" signal, useful for gating even guide chords if desired)?
    pub fn fullscreen_game(&self) -> bool {
        self.game_focused() && self.fullscreen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_not_suppressed_by_default() {
        let a = Arbiter::new();
        assert!(!a.suppressed());
        assert!(!a.game_focused());
    }

    #[test]
    fn steam_app_suppresses() {
        let mut a = Arbiter::new();
        a.focus_changed("steam_app_413080");
        assert!(a.game_focused());
        assert!(a.suppressed());
    }

    #[test]
    fn gamescope_suppresses() {
        let mut a = Arbiter::new();
        a.focus_changed("gamescope");
        assert!(a.suppressed());
    }

    #[test]
    fn ordinary_app_does_not_suppress() {
        let mut a = Arbiter::new();
        a.focus_changed("org.mozilla.firefox");
        assert!(!a.suppressed());
        a.focus_changed("Alacritty");
        assert!(!a.suppressed());
    }

    #[test]
    fn force_desktop_overrides_game() {
        let mut a = Arbiter::new();
        a.focus_changed("steam_app_413080");
        a.set_force_desktop(true);
        assert!(!a.suppressed());
        assert!(a.game_focused()); // still a game, just not suppressing
    }

    #[test]
    fn fullscreen_game_signal() {
        let mut a = Arbiter::new();
        a.focus_changed("steam_app_1");
        assert!(!a.fullscreen_game());
        a.set_fullscreen(true);
        assert!(a.fullscreen_game());
        a.focus_changed(""); // focus back to desktop
        assert!(!a.fullscreen_game());
    }

    #[test]
    fn case_insensitive_prefix() {
        let mut a = Arbiter::new();
        a.focus_changed("Steam_App_570");
        assert!(a.game_focused());
    }
}
