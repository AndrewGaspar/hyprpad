//! Declarative gesture -> action bindings.
//!
//! A [`Config`] maps recognized [`gesture::GestureEvent`]s to [`Action`]s. It
//! is loaded from a small, flat TOML dialect: a single `[bindings]` table whose
//! keys name a gesture (`"guide+r1"`, `"guide+stick_right"`) and whose values
//! are action strings (`"workspace +1"`, `"exec walker"`, `"fullscreen"`).
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

/// A set of gesture bindings.
#[derive(Clone, Debug, Default)]
pub struct Config {
    bindings: HashMap<GestureKey, Action>,
}

/// The built-in default bindings, in the config's own TOML dialect. Loaded by
/// [`Config::load_default`]; also exercises the parser round-trip.
pub const DEFAULT_TOML: &str = r#"
# hyprsc default gesture bindings (docs/08-living-room-vision.md).
[bindings]
"guide+r1" = "workspace +1"          # workspace right
"guide+l1" = "workspace -1"          # workspace left
"guide+stick_right" = "workspace +1" # right stick flicked right
"guide+stick_left"  = "workspace -1" # right stick flicked left
"guide+x" = "exec walker"            # launcher
"#;

impl Config {
    /// Parse a config from the TOML dialect described in the module docs.
    pub fn from_toml_str(s: &str) -> Result<Config, String> {
        let mut bindings = HashMap::new();
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
            if !section.is_empty() && section != "bindings" {
                return Err(format!("line {lineno}: unknown section [{section}]"));
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {lineno}: expected 'key = value'"))?;
            let key = GestureKey::parse(&unquote(k))
                .map_err(|e| format!("line {lineno}: {e}"))?;
            let action = Action::parse(&unquote(v))
                .map_err(|e| format!("line {lineno}: {e}"))?;
            bindings.insert(key, action);
        }
        Ok(Config { bindings })
    }

    /// The built-in defaults encoding the vision's core gestures.
    pub fn load_default() -> Config {
        Config::from_toml_str(DEFAULT_TOML).expect("built-in default config is valid")
    }

    /// The path the user config is read from:
    /// `$XDG_CONFIG_HOME/hyprsc/config.toml`, or `~/.config/hyprsc/config.toml`
    /// when `XDG_CONFIG_HOME` is unset. Returns `None` only if neither
    /// `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn config_path() -> Option<std::path::PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(dir).join("hyprsc/config.toml"));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(std::path::PathBuf::from(home).join(".config/hyprsc/config.toml"))
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
    fn unbound_events_resolve_to_none() {
        let c = Config::load_default();
        // Unbound chord.
        assert_eq!(c.resolve(&GestureEvent::GuideChord(Button::Y)), Action::None);
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
    fn load_reads_present_file_and_reports_malformed() {
        // Point XDG_CONFIG_HOME at a unique temp dir so `load()` reads our
        // file. No other test touches these vars, so the process-wide mutation
        // is safe here. Save/restore to leave the environment as we found it.
        let saved_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let dir = std::env::temp_dir().join(format!("hyprsc-cfg-test-{}", std::process::id()));
        let cfg_dir = dir.join("hyprsc");
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
