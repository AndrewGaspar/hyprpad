//! The daemon's live status file: `$XDG_RUNTIME_DIR/hyprpad/status.json`.
//!
//! One small JSON object carrying what a status-bar widget needs and nothing
//! else — is the controller here, and which mode is the pad in — so a widget
//! never has to talk to the daemon, only watch a file.
//!
//! ```json
//! {"connected": true, "mode": "desktop", "controller": "Steam Controller Puck",
//!  "relay": "xbox", "pid": 12345,
//!  "modes": ["cheatsheet", "omarchy-ui", "game", "desktop", "osk"],
//!  "updated": 1725230000}
//! ```
//!
//! Two properties make it safe to watch:
//!
//! * **Atomic.** Every publish writes `status.json.tmp` and renames it over
//!   `status.json`, so a reader woken by the write never parses half an object.
//! * **Present only while the daemon is.** The writer is an RAII guard, like
//!   [`crate::run`]'s pidfile: the file appears at startup and is removed on the
//!   clean-return and panic paths. A signal-driven exit can still leave it, which
//!   is why the object carries `pid` — a widget confirms `/proc/<pid>` exists and
//!   treats a stale file as "no daemon".
//!
//! The JSON is emitted by hand, as in [`crate::bindings_sheet`]: the daemon
//! carries no serialization dependency.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bindings_sheet::OSK_MODE;
use crate::config::Config;
use crate::mode::{BUILTIN_DESKTOP, BUILTIN_GAME};

/// Reported as the controller's name when its sysfs `HID_NAME` cannot be read —
/// which is the ordinary case while the puck is away.
pub const DEFAULT_CONTROLLER: &str = "Steam Controller";

/// The daemon's runtime directory, `$XDG_RUNTIME_DIR/hyprpad`. `None` when
/// `XDG_RUNTIME_DIR` is unset or empty, which simply means no status file.
pub fn status_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(dir).join("hyprpad"))
}

/// Path to the status file itself, `$XDG_RUNTIME_DIR/hyprpad/status.json`.
pub fn status_file_path() -> Option<PathBuf> {
    Some(status_dir()?.join("status.json"))
}

/// Which game sink is live — the `"relay"` field of `status.json`.
///
/// A widget uses it to say what a game would actually receive, which is not the
/// same question as which one the config asked for: `[gamepad] kind = "steam"`
/// with no writable `/dev/uhid` yields [`RelayKind::None`], not
/// [`RelayKind::Steam`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RelayKind {
    /// No game sink at all: `[gamepad] enabled = false`, or `kind = "steam"`
    /// with no `/dev/uhid` the daemon may open.
    #[default]
    None,
    /// The uinput Xbox-360 pad ([`crate::gamepad`]).
    Xbox,
    /// The virtual Valve controller on `/dev/uhid` ([`crate::uhid`]).
    Steam,
}

impl RelayKind {
    /// The wire spelling, which is also the `[gamepad] kind` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RelayKind::None => "none",
            RelayKind::Xbox => "xbox",
            RelayKind::Steam => "steam",
        }
    }
}

/// The published state. One value, so a publish is always a whole consistent
/// object rather than a field at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Whether the puck is present *right now*. False during the startup wait
    /// and during the reconnect wait, both of which the daemon sits through
    /// rather than exiting.
    pub connected: bool,
    /// The mode the pad is in: [`crate::mode::ModeEngine::active`]'s name, or
    /// [`OSK_MODE`] while the on-screen keyboard owns the pads.
    pub mode: String,
    /// A human name for the controller, from the hidraw node's `HID_NAME`.
    pub controller: String,
    /// Which virtual controller a focused game is actually being given.
    pub relay: RelayKind,
    /// The daemon's pid, so a reader can tell a live file from one a killed
    /// daemon left behind.
    pub pid: u32,
    /// Every mode that can become active, in resolution order — for a widget
    /// that wants to show more than the current one.
    pub modes: Vec<String>,
    /// Seconds since the epoch at the last publish.
    pub updated: u64,
}

impl Status {
    /// The wire format: one line, the shape documented at the module top.
    pub fn to_json(&self) -> String {
        let mut o = String::with_capacity(256);
        let _ = write!(o, "{{\"connected\": {}, \"mode\": ", self.connected);
        push_str(&mut o, &self.mode);
        o.push_str(", \"controller\": ");
        push_str(&mut o, &self.controller);
        o.push_str(", \"relay\": ");
        push_str(&mut o, self.relay.as_str());
        let _ = write!(o, ", \"pid\": {}, \"modes\": ", self.pid);
        push_strs(&mut o, &self.modes);
        let _ = write!(o, ", \"updated\": {}}}", self.updated);
        o.push('\n');
        o
    }
}

/// Owns the status file for the life of [`crate::run::run`]: publishes on every
/// state change and removes the file on drop.
///
/// Every method is best-effort and infallible from the caller's side. A daemon
/// whose status file cannot be written is a daemon with no bar widget, not a
/// daemon that fails to start, so a failure warns once and the writer goes quiet
/// rather than repeating itself on every mode change.
pub struct StatusWriter {
    /// `None` when there is nowhere to write (no `XDG_RUNTIME_DIR`, or the
    /// directory could not be created); every method is then a no-op.
    paths: Option<Paths>,
    status: Status,
    /// The mode engine's mode, as last given to [`set_mode`](Self::set_mode).
    /// Published as-is unless the keyboard is up.
    base_mode: String,
    /// Whether the on-screen keyboard owns the pads. While it does, the
    /// published mode is [`OSK_MODE`], whatever the engine says.
    osk: bool,
    /// Set once the first write fails, so the warning is not repeated per
    /// transition for the rest of the session.
    warned: bool,
}

/// The final path and the temp file it is renamed from — both held so the
/// atomic publish never has to rebuild them.
struct Paths {
    file: PathBuf,
    tmp: PathBuf,
}

impl StatusWriter {
    /// Start publishing to `$XDG_RUNTIME_DIR/hyprpad/status.json`, creating the
    /// directory if it is not there yet.
    ///
    /// The initial object says "not connected, desktop": the daemon starts
    /// before its controller does, and the startup wait is exactly the state
    /// this describes. The caller corrects it with [`set_connected`] and
    /// [`set_mode`] as soon as it knows better.
    ///
    /// [`set_connected`]: Self::set_connected
    /// [`set_mode`]: Self::set_mode
    pub fn create() -> StatusWriter {
        match status_dir() {
            Some(dir) => StatusWriter::in_dir(&dir),
            None => StatusWriter::disabled(),
        }
    }

    /// [`create`](Self::create), but writing into an explicit directory. The
    /// tests use it; `create` is this with the runtime dir filled in.
    pub fn in_dir(dir: &Path) -> StatusWriter {
        let mut w = StatusWriter::disabled();
        match std::fs::create_dir_all(dir) {
            Ok(()) => {
                w.paths = Some(Paths {
                    file: dir.join("status.json"),
                    tmp: dir.join("status.json.tmp"),
                });
                w.publish();
            }
            Err(e) => eprintln!("warning: could not create {}: {e}", dir.display()),
        }
        w
    }

    /// A writer with nowhere to write. Keeps the call sites in
    /// [`crate::run::run`] free of `Option` handling.
    pub fn disabled() -> StatusWriter {
        StatusWriter {
            paths: None,
            status: Status {
                connected: false,
                mode: BUILTIN_DESKTOP.to_string(),
                controller: DEFAULT_CONTROLLER.to_string(),
                relay: RelayKind::None,
                pid: std::process::id(),
                modes: Vec::new(),
                updated: now_secs(),
            },
            base_mode: BUILTIN_DESKTOP.to_string(),
            osk: false,
            warned: false,
        }
    }

    /// The state as last published. Mostly for tests, and for a caller that
    /// wants to know whether a set would change anything.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Record whether the puck is present. Publishes only on a real change, so
    /// the reconnect scan's repeated "still gone" costs nothing.
    ///
    /// A transition to *connected* also refreshes the controller's name: the
    /// daemon may well have started before the device existed, and the name is
    /// only readable once it does.
    pub fn set_connected(&mut self, connected: bool) {
        if self.status.connected == connected {
            return;
        }
        self.status.connected = connected;
        if connected {
            self.status.controller = controller_name();
        }
        self.publish();
    }

    /// Record the active mode. Publishes only on a real change — the daemon
    /// calls this after every transition, and a transition can land back on the
    /// mode that was already active.
    pub fn set_mode(&mut self, mode: &str) {
        if self.base_mode == mode {
            return;
        }
        self.base_mode = mode.to_string();
        self.refresh_mode();
    }

    /// Record whether the on-screen keyboard is up. The engine does not call
    /// that a mode — it is a routing state that owns both pads — but to a
    /// reader it is the mode the pad is in, and the cheat sheet already draws
    /// it as one (its `osk` tab). So while the keyboard is up the published
    /// mode is [`OSK_MODE`], and the engine's mode is kept aside to come back
    /// the moment it goes down. Publishes only on a real flip, so the daemon
    /// can call this every frame.
    pub fn set_osk(&mut self, active: bool) {
        if self.osk == active {
            return;
        }
        self.osk = active;
        self.refresh_mode();
    }

    /// Publish the effective mode — keyboard first, engine otherwise — if it
    /// moved.
    fn refresh_mode(&mut self) {
        let effective = if self.osk { OSK_MODE } else { self.base_mode.as_str() };
        if self.status.mode == effective {
            return;
        }
        self.status.mode = effective.to_string();
        self.publish();
    }

    /// Record which game sink is live.
    ///
    /// Set once at startup, when the daemon has learned whether the configured
    /// sink could actually be created — for the Steam relay that means "was
    /// `/dev/uhid` openable", which is not knowable from the config alone — and
    /// again on a reload that turns the whole forwarding path off. Publishes
    /// only on a real change.
    pub fn set_relay(&mut self, relay: RelayKind) {
        if self.status.relay == relay {
            return;
        }
        self.status.relay = relay;
        self.publish();
    }

    /// Record the set of modes that can become active. Changes only at startup
    /// and on a config reload.
    pub fn set_modes(&mut self, modes: Vec<String>) {
        if self.status.modes == modes {
            return;
        }
        self.status.modes = modes;
        self.publish();
    }

    /// Write the current state atomically: temp file, then rename over the real
    /// one. The rename is what makes a concurrent reader safe — it either sees
    /// the whole old object or the whole new one, never a partial write.
    fn publish(&mut self) {
        let Some(paths) = &self.paths else { return };
        self.status.updated = now_secs();
        let json = self.status.to_json();
        let outcome = std::fs::write(&paths.tmp, &json)
            .and_then(|()| std::fs::rename(&paths.tmp, &paths.file));
        if let Err(e) = outcome {
            // Leave no half-written temp file for the next publish to trip
            // over, then say so once and stay quiet.
            let _ = std::fs::remove_file(&paths.tmp);
            if !self.warned {
                eprintln!("warning: could not write {}: {e}", paths.file.display());
                self.warned = true;
            }
        }
    }
}

impl Drop for StatusWriter {
    fn drop(&mut self) {
        let Some(paths) = &self.paths else { return };
        let _ = std::fs::remove_file(&paths.file);
        let _ = std::fs::remove_file(&paths.tmp);
    }
}

/// A human name for the puck, from the `HID_NAME` its hidraw node advertises in
/// sysfs (`Valve Software Steam Controller Puck`), or [`DEFAULT_CONTROLLER`]
/// when it cannot be read — normally because the controller is not here.
pub fn controller_name() -> String {
    let Ok(nodes) = crate::hidraw::puck_nodes() else {
        return DEFAULT_CONTROLLER.to_string();
    };
    for node in nodes {
        // `/dev/hidrawN` -> `/sys/class/hidraw/hidrawN/device/uevent`, the same
        // file `puck_nodes` matched the vendor and product against.
        let Some(name) = node.file_name() else { continue };
        let uevent = Path::new("/sys/class/hidraw").join(name).join("device/uevent");
        let Ok(text) = std::fs::read_to_string(uevent) else { continue };
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("HID_NAME=") {
                let v = v.trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    DEFAULT_CONTROLLER.to_string()
}

/// Every mode that can become active, in the order the engine resolves them:
/// the declared modes in definition order, then the built-in `osk` context.
///
/// A config that declares no modes still resolves the two built-ins the
/// `Arbiter` has always had, so name those rather than publishing an empty list.
///
/// Nothing stops a config declaring a mode of its own called `osk` — a
/// reasonable thing to do, since that is the name of the context — so the
/// built-in is appended only when the declarations left room for it.
pub fn mode_names(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = if config.modes().is_empty() {
        vec![BUILTIN_DESKTOP.to_string(), BUILTIN_GAME.to_string()]
    } else {
        config.modes().iter().map(|m| m.name.clone()).collect()
    };
    if !names.iter().any(|n| n == OSK_MODE) {
        names.push(OSK_MODE.to_string());
    }
    names
}

/// Seconds since the epoch. A clock set before 1970 is not worth a branch in
/// the caller, so it reads as 0.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Append `s` as an escaped JSON string literal.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up, so a test never leaves a
    /// status file behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = std::env::temp_dir()
                .join(format!("hyprpad-status-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("scratch dir");
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Status {
        Status {
            connected: true,
            mode: "desktop".to_string(),
            controller: "Steam Controller Puck".to_string(),
            relay: RelayKind::Xbox,
            pid: 12345,
            modes: vec!["cheatsheet".to_string(), "game".to_string(), "desktop".to_string()],
            updated: 1_725_230_000,
        }
    }

    #[test]
    fn json_has_the_documented_shape() {
        assert_eq!(
            sample().to_json(),
            "{\"connected\": true, \"mode\": \"desktop\", \
             \"controller\": \"Steam Controller Puck\", \"relay\": \"xbox\", \
             \"pid\": 12345, \
             \"modes\": [\"cheatsheet\", \"game\", \"desktop\"], \
             \"updated\": 1725230000}\n"
        );
    }

    /// The `"relay"` field: which sink a focused game is actually given.
    #[test]
    fn json_carries_the_live_relay() {
        let mut s = sample();
        assert!(s.to_json().contains(r#""relay": "xbox""#), "{}", s.to_json());
        s.relay = RelayKind::Steam;
        assert!(s.to_json().contains(r#""relay": "steam""#), "{}", s.to_json());
        s.relay = RelayKind::None;
        assert!(s.to_json().contains(r#""relay": "none""#), "{}", s.to_json());
        assert_eq!(RelayKind::default(), RelayKind::None);
        assert_eq!(
            (RelayKind::None.as_str(), RelayKind::Xbox.as_str(), RelayKind::Steam.as_str()),
            ("none", "xbox", "steam")
        );
    }

    #[test]
    fn set_relay_publishes_once_per_real_change() {
        let dir = TempDir::new("relay");
        let file = dir.path().join("status.json");
        let mut w = StatusWriter::in_dir(dir.path());
        assert_eq!(w.status().relay, RelayKind::None, "nothing live until told");

        w.set_relay(RelayKind::Steam);
        let json = std::fs::read_to_string(&file).unwrap();
        assert!(json.contains(r#""relay": "steam""#), "{json}");

        // Idempotent: setting the same value again publishes nothing new.
        let before = std::fs::read_to_string(&file).unwrap();
        w.set_relay(RelayKind::Steam);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

        w.set_relay(RelayKind::None);
        assert!(std::fs::read_to_string(&file).unwrap().contains(r#""relay": "none""#));
    }

    #[test]
    fn json_spells_a_missing_controller_as_false() {
        let s = Status { connected: false, ..sample() };
        assert!(s.to_json().starts_with("{\"connected\": false, "), "{}", s.to_json());
    }

    #[test]
    fn json_escapes_strings_that_would_break_the_parse() {
        let s = Status {
            controller: "a \"quoted\"\\ name\n".to_string(),
            mode: "tab\there".to_string(),
            ..sample()
        };
        let json = s.to_json();
        assert!(json.contains(r#""controller": "a \"quoted\"\\ name\n""#), "{json}");
        assert!(json.contains(r#""mode": "tab\there""#), "{json}");
    }

    #[test]
    fn json_escapes_other_control_characters_as_unicode_escapes() {
        let s = Status { mode: "a\u{1}b".to_string(), ..sample() };
        assert!(s.to_json().contains(r#""mode": "a\u0001b""#), "{}", s.to_json());
    }

    #[test]
    fn the_writer_publishes_on_creation_and_removes_the_file_on_drop() {
        let dir = TempDir::new("lifecycle");
        let file = dir.path().join("status.json");
        {
            let w = StatusWriter::in_dir(dir.path());
            assert!(file.exists(), "the file appears as soon as the daemon starts");
            assert_eq!(w.status().pid, std::process::id());
            let text = std::fs::read_to_string(&file).expect("read back");
            assert!(text.contains("\"connected\": false"), "{text}");
            assert!(text.contains("\"mode\": \"desktop\""), "{text}");
        }
        assert!(!file.exists(), "and goes away with the daemon");
    }

    #[test]
    fn the_writer_leaves_no_temp_file_behind() {
        let dir = TempDir::new("tmpfile");
        let mut w = StatusWriter::in_dir(dir.path());
        w.set_connected(true);
        w.set_mode("game");
        assert!(!dir.path().join("status.json.tmp").exists(), "the temp file is renamed away");
    }

    #[test]
    fn setting_the_mode_republishes_it() {
        let dir = TempDir::new("setmode");
        let file = dir.path().join("status.json");
        let mut w = StatusWriter::in_dir(dir.path());
        w.set_mode("omarchy-ui");
        assert_eq!(w.status().mode, "omarchy-ui");
        let text = std::fs::read_to_string(&file).expect("read back");
        assert!(text.contains("\"mode\": \"omarchy-ui\""), "{text}");
    }

    #[test]
    fn a_set_that_changes_nothing_does_not_republish() {
        let dir = TempDir::new("noop");
        let mut w = StatusWriter::in_dir(dir.path());
        w.set_mode("game");
        let published = w.status().clone();
        // The mode the daemon re-announces after a transition that landed back
        // on the mode already active: nothing about the object may move.
        w.set_mode("game");
        w.set_connected(false);
        w.set_modes(Vec::new());
        assert_eq!(w.status(), &published);
    }

    #[test]
    fn the_published_file_is_always_a_whole_object() {
        let dir = TempDir::new("atomic");
        let file = dir.path().join("status.json");
        let mut w = StatusWriter::in_dir(dir.path());
        for (i, mode) in ["game", "desktop", "cheatsheet", "osk"].iter().enumerate() {
            w.set_mode(mode);
            let text = std::fs::read_to_string(&file).expect("read back");
            assert!(text.starts_with('{'), "publish {i}: {text}");
            assert!(text.ends_with("}\n"), "publish {i}: {text}");
            assert!(text.contains(&format!("\"mode\": \"{mode}\"")), "publish {i}: {text}");
        }
    }

    #[test]
    fn the_keyboard_overrides_the_published_mode_while_it_is_up() {
        let dir = TempDir::new("osk");
        let file = dir.path().join("status.json");
        let read = || std::fs::read_to_string(&file).expect("read back");
        let mut w = StatusWriter::in_dir(dir.path());
        w.set_mode("game");
        w.set_osk(true);
        assert_eq!(w.status().mode, "osk");
        assert!(read().contains("\"mode\": \"osk\""), "{}", read());
        // The engine can move underneath the keyboard; the reader still sees
        // the keyboard...
        w.set_mode("desktop");
        assert_eq!(w.status().mode, "osk");
        // ...until it goes down, when the engine's *current* mode comes back,
        // not the one it had when the keyboard came up.
        w.set_osk(false);
        assert_eq!(w.status().mode, "desktop");
        assert!(read().contains("\"mode\": \"desktop\""), "{}", read());
    }

    #[test]
    fn keyboard_state_that_does_not_flip_does_not_republish() {
        let dir = TempDir::new("osk-noop");
        let mut w = StatusWriter::in_dir(dir.path());
        w.set_osk(true);
        let published = w.status().clone();
        // What the per-frame call sees on every frame the keyboard stays up.
        w.set_osk(true);
        assert_eq!(w.status(), &published);
    }

    #[test]
    fn a_writer_with_nowhere_to_write_is_inert() {
        let mut w = StatusWriter::disabled();
        w.set_connected(true);
        w.set_mode("game");
        // No panic, and the in-memory state still tracks, so the daemon's call
        // sites need no `Option` handling.
        assert_eq!(w.status().mode, "game");
        assert!(w.status().connected);
    }

    #[test]
    fn the_mode_list_ends_with_the_keyboards_builtin_context() {
        let config = Config::load_default();
        let names = mode_names(&config);
        assert_eq!(names.last().map(String::as_str), Some(OSK_MODE));
    }

    #[test]
    fn a_config_that_declares_no_modes_still_names_the_two_builtins() {
        let config = Config::load_default();
        assert!(config.modes().is_empty(), "the built-in default config declares no modes");
        assert_eq!(mode_names(&config), vec!["desktop", "game", "osk"]);
    }

    #[test]
    fn declared_modes_are_listed_in_definition_order() {
        let config = crate::lua_config::load_str(
            r#"
            hyprpad.mode("cheatsheet").when(function(ctx) return false end)
            hyprpad.mode("game", { forward = true }).when(function(ctx) return false end)
            hyprpad.mode("desktop")
            "#,
            "test.lua",
        )
        .expect("config should load");
        assert_eq!(mode_names(&config), vec!["cheatsheet", "game", "desktop", "osk"]);
    }

    #[test]
    fn a_config_that_declares_its_own_osk_mode_gets_one_entry_not_two() {
        let config = crate::lua_config::load_str(
            r#"
            hyprpad.mode("osk").when(function(ctx) return false end)
            hyprpad.mode("desktop")
            "#,
            "test.lua",
        )
        .expect("config should load");
        assert_eq!(mode_names(&config), vec!["osk", "desktop"]);
    }
}
