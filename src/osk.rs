//! The bridge to the on-screen keyboard (`hyprpad-osk`).
//!
//! [`OskHandle`] owns the OSK as a **child process** and drives it over the
//! control channel described in `osk/src/control.rs`: a line-based text protocol
//! we speak on the child's **stdin** (`hyprpad-osk --stdin`). The vocabulary is
//! the dual-trackpad one the OSK already understands — `show`, `hide`,
//! `cursor <L|R> <nx> <ny>`, `commit <L|R>`, `key <code>`, `shift down|up`,
//! `quit` — so the daemon forwards each pad's absolute cursor and its click
//! independently, and holds Shift for exactly as long as a trigger is pulled.
//!
//! ## The back-channel (child stdout -> daemon)
//!
//! Responsibility for the OSK's haptic tick is split: the **child** knows when a
//! pad's cursor crosses onto a new key (it owns the layout and the hit-test),
//! but the **daemon** owns the puck's writable hidraw node ([`crate::haptics`]).
//! So the child announces the crossing and the daemon fires the pulse. The child
//! prints one machine-readable line per event on its **stdout**:
//!
//! ```text
//! event crossed <L|R>
//! ```
//!
//! and keeps its human-oriented logs on **stderr** (which stays inherited, so
//! the `hyprpad-osk:` lines still land in the daemon's log). We pipe stdout, read
//! it on a thread, and forward each parsed [`OskEvent`] to the daemon over an
//! [`mpsc`] channel. The parser is deliberately **version-tolerant**: any line
//! that is not a recognized `event …` is ignored, so a newer OSK can add events
//! (or print anything else) without breaking an older daemon.
//!
//! The handle is deliberately forgiving: the OSK binary may not be installed,
//! and the daemon must keep running without it. Spawn is **lazy** (on the first
//! [`show`](OskHandle::show)) and every failure degrades to a logged warning
//! plus a no-op — never a panic and never a daemon exit. The child is killed on
//! [`Drop`], i.e. when the daemon exits.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;

use crate::config::KeyChord;

/// Which OSK layout to show. Mirrors the `show <bottom|split>` grammar; the
/// two-region dual-trackpad model is the same in both modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OskMode {
    /// One bottom-docked full-width panel (the fully-implemented Deck mode).
    Bottom,
    /// Two edge-docked columns, one per hand.
    Split,
}

impl OskMode {
    /// The wire token used in a `show` command.
    fn wire(self) -> &'static str {
        match self {
            OskMode::Bottom => "bottom",
            OskMode::Split => "split",
        }
    }
}

/// Which trackpad an addressed command targets. Matches the OSK's `L`/`R`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OskPad {
    Left,
    Right,
}

impl OskPad {
    /// The wire token used in `cursor`/`commit` commands.
    fn wire(self) -> &'static str {
        match self {
            OskPad::Left => "L",
            OskPad::Right => "R",
        }
    }

    /// Parse a wire pad token (`L`/`R`, either case) from a back-channel event.
    fn parse_wire(tok: &str) -> Option<OskPad> {
        match tok {
            "L" | "l" => Some(OskPad::Left),
            "R" | "r" => Some(OskPad::Right),
            _ => None,
        }
    }
}

/// An event read back from the OSK child over its stdout back-channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OskEvent {
    /// That pad's cursor moved onto a **new** key (`event crossed <L|R>`). The
    /// child emits this only on an actual change of focused key, and never for a
    /// crossing onto a gap — so the daemon can fire a haptic tick per line with
    /// no further filtering.
    Crossed(OskPad),
}

/// Parse one line of the OSK's stdout back-channel into an [`OskEvent`].
///
/// The grammar is `event <name> [args…]`. Anything else — a log line, a blank
/// line, an event name or argument this daemon does not know — yields `None` and
/// is ignored, so the two binaries can be upgraded independently.
fn parse_event(line: &str) -> Option<OskEvent> {
    let mut it = line.split_whitespace();
    if it.next()? != "event" {
        return None;
    }
    match it.next()? {
        "crossed" => Some(OskEvent::Crossed(OskPad::parse_wire(it.next()?)?)),
        _ => None,
    }
}

/// Normalize a raw pad axis to the OSK's `[-1, 1]` range.
///
/// The trackpad reports absolute `i16` touch coordinates spanning roughly
/// ±32767 across its surface (0 when untouched). The OSK wants each axis in
/// `[-1, 1]`, and — crucially — the pad's `+Y` is up, which is exactly the
/// OSK's `+ny` convention, so the value passes straight through with no flip.
/// `i16::MIN` sits just past `-32767`, so the result is clamped.
pub fn normalize_pad_axis(v: i16) -> f32 {
    (f32::from(v) / 32767.0).clamp(-1.0, 1.0)
}

/// Format a `show` command line.
fn show_cmd(mode: OskMode, reflow: bool) -> String {
    let present = if reflow { "reflow" } else { "overlay" };
    format!("show {} {present}", mode.wire())
}

/// Format a `cursor` command line. Axes are emitted at fixed precision so
/// identical positions serialize identically (and stay short on the wire).
fn cursor_cmd(pad: OskPad, nx: f32, ny: f32) -> String {
    format!("cursor {} {nx:.4} {ny:.4}", pad.wire())
}

/// Format a `commit` command line.
fn commit_cmd(pad: OskPad) -> String {
    format!("commit {}", pad.wire())
}

/// Format a `shift down` / `shift up` command line: the OSK holds a momentary
/// Shift for as long as the daemon says it is down (the `osk shift` binding —
/// L2 by default), forcing the shifted legends and output without touching its
/// own one-shot/caps latch.
fn shift_cmd(down: bool) -> String {
    format!("shift {}", if down { "down" } else { "up" })
}

/// Format a `key` command line: the OSK taps this raw evdev keycode on its
/// virtual keyboard. Used by the Deck-style helper buttons (`[osk_buttons]`,
/// e.g. Y = Space) so common keys need no cursor hunting.
///
/// A [`KeyChord`]'s modifiers follow the keycode (`key 14 29` = Ctrl+Backspace),
/// which keeps the one-code form the wire has always had: a plain key still
/// serializes to `key 14` and an older keyboard binary reads it unchanged.
fn key_cmd(chord: &KeyChord) -> String {
    let mut line = format!("key {}", chord.code());
    for &m in chord.mods() {
        line.push(' ');
        line.push_str(&m.to_string());
    }
    line
}

/// A live handle to the on-screen keyboard child process.
///
/// Construct once, up front — nothing is spawned until the first
/// [`show`](Self::show). Across a [`hide`](Self::hide)/`show` cycle the same
/// child is reused: `hide` destroys the OSK's surface but leaves the process
/// running, ready to re-show cheaply.
#[derive(Default)]
pub struct OskHandle {
    /// The running OSK child, if spawned. Retained so [`Drop`] can reap it.
    child: Option<Child>,
    /// The child's stdin — our control channel. `None` before first spawn or
    /// after the pipe broke.
    stdin: Option<ChildStdin>,
    /// Whether the OSK surface is currently shown (between `show` and `hide`).
    active: bool,
    /// Set once spawning has failed, so we neither retry every frame nor
    /// re-log the warning.
    spawn_failed: bool,
    /// Where parsed back-channel events go. `None` means the caller does not
    /// want them, and the child's stdout is simply inherited as before. Cloned
    /// (not taken) on spawn, so a respawn after a broken pipe keeps reporting.
    events: Option<mpsc::Sender<OskEvent>>,
}

impl OskHandle {
    /// A handle that has not spawned anything yet, and does not read the child's
    /// back-channel.
    pub fn new() -> Self {
        Self::default()
    }

    /// A handle that forwards every [`OskEvent`] it reads from the child's
    /// stdout to `events`. The daemon uses this to turn a key crossing into a
    /// haptic tick on the device it (not the child) owns.
    pub fn with_events(events: mpsc::Sender<OskEvent>) -> Self {
        OskHandle {
            child: None,
            stdin: None,
            active: false,
            spawn_failed: false,
            events: Some(events),
        }
    }

    /// Whether the OSK surface is currently shown.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Show the keyboard in `mode`, spawning the child on first use. `reflow`
    /// chooses the exclusive-zone behaviour (`true` reflows workspace content,
    /// `false` floats over a fullscreen game). A no-op (with a one-time warning)
    /// if the OSK binary can't be started.
    pub fn show(&mut self, mode: OskMode, reflow: bool) {
        if !self.ensure_spawned() {
            return;
        }
        if self.send(&show_cmd(mode, reflow)) {
            self.active = true;
        }
    }

    /// Hide (destroy) the keyboard surface. The child process stays alive for a
    /// cheap re-show. Always leaves us out of the active state.
    pub fn hide(&mut self) {
        if self.stdin.is_some() {
            self.send("hide");
        }
        self.active = false;
    }

    /// Move `pad`'s cursor to the normalized position `(nx, ny)`, each axis in
    /// `[-1, 1]`. Ignored unless the keyboard is shown.
    pub fn cursor(&mut self, pad: OskPad, nx: f32, ny: f32) {
        if !self.active {
            return;
        }
        self.send(&cursor_cmd(pad, nx, ny));
    }

    /// Commit (click-down) the key currently under `pad`'s cursor. Ignored
    /// unless the keyboard is shown.
    pub fn commit(&mut self, pad: OskPad) {
        if !self.active {
            return;
        }
        self.send(&commit_cmd(pad));
    }

    /// Hold (`true`) or release (`false`) the keyboard's momentary Shift — the
    /// `osk shift` binding, a trigger by default. Ignored unless the keyboard
    /// is shown; the keyboard drops a held shift on `hide` itself, so a release
    /// that arrives after a dismiss has nothing to undo.
    pub fn hold_shift(&mut self, down: bool) {
        if !self.active {
            return;
        }
        self.send(&shift_cmd(down));
    }

    /// Tap a key — with any modifiers held around it — through the OSK's
    /// virtual keyboard (the `[osk_buttons]` Deck-style helpers, e.g. Y =
    /// Space, X = Backspace, and combos like `h.key "ctrl+backspace"`).
    /// Ignored unless the keyboard is shown.
    pub fn key(&mut self, chord: &KeyChord) {
        if !self.active {
            return;
        }
        self.send(&key_cmd(chord));
    }

    /// Ensure a child is running; returns whether we have a usable control
    /// channel. Spawns lazily and remembers a hard failure so we stop trying.
    fn ensure_spawned(&mut self) -> bool {
        if self.stdin.is_some() {
            return true;
        }
        if self.spawn_failed {
            return false;
        }
        let bin = resolve_osk_bin();
        // Only pipe the child's stdout when someone is listening; otherwise
        // inherit it, so a stdout-piped-but-never-read child could never block
        // on a full pipe.
        match spawn_osk(&bin, self.events.is_some()) {
            Ok((child, stdin, stdout)) => {
                eprintln!("hyprpad: on-screen keyboard ready ({})", bin.display());
                if let (Some(stdout), Some(events)) = (stdout, self.events.clone()) {
                    spawn_event_reader(stdout, events);
                }
                self.child = Some(child);
                self.stdin = Some(stdin);
                true
            }
            Err(e) => {
                eprintln!(
                    "warning: could not start on-screen keyboard '{}' ({e}); keyboard disabled",
                    bin.display()
                );
                self.spawn_failed = true;
                false
            }
        }
    }

    /// Write one command line to the child, flushing it. On any I/O error the
    /// child is torn down so a later [`show`](Self::show) can respawn it.
    /// Returns whether the line was delivered.
    fn send(&mut self, line: &str) -> bool {
        let ok = match self.stdin.as_mut() {
            Some(stdin) => writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_ok(),
            None => return false,
        };
        if !ok {
            eprintln!("warning: on-screen keyboard control pipe closed; will respawn on next show");
            self.teardown();
        }
        ok
    }

    /// Drop the control channel and reap the child. Leaves the handle ready to
    /// respawn on the next [`show`](Self::show).
    fn teardown(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.active = false;
    }
}

impl Drop for OskHandle {
    fn drop(&mut self) {
        // Best-effort graceful shutdown (destroys the layer surface), then make
        // sure the process is gone.
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        self.teardown();
    }
}

/// Spawn `hyprpad-osk --stdin` with a piped stdin (the control channel) and,
/// when `pipe_stdout` is set, a piped stdout (the event back-channel). **stderr
/// stays inherited** either way, so the child's human-oriented `hyprpad-osk:`
/// logs keep surfacing alongside the daemon's.
fn spawn_osk(bin: &Path, pipe_stdout: bool) -> std::io::Result<(Child, ChildStdin, Option<ChildStdout>)> {
    let mut cmd = Command::new(bin);
    cmd.arg("--stdin").stdin(Stdio::piped());
    if pipe_stdout {
        cmd.stdout(Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    let stdin = child
        .stdin
        .take()
        .expect("child spawned with Stdio::piped() has a stdin");
    let stdout = child.stdout.take();
    Ok((child, stdin, stdout))
}

/// Read the child's stdout back-channel on its own thread, forwarding every
/// recognized [`OskEvent`] to `events`.
///
/// Ends silently when the pipe closes — the child exited or was killed. This is
/// a best-effort feedback path, so an unreadable line or a broken pipe must
/// never be louder than a no-op.
///
/// When the *receiver* goes away (the daemon is shutting down) the thread keeps
/// **draining** the pipe instead of returning: a piped stdout nobody reads fills
/// up, and the child's next event line would then block on the write. Draining
/// costs nothing and guarantees the keyboard can never wedge on our account; the
/// thread still ends promptly, because a shutting-down daemon kills the child.
fn spawn_event_reader(stdout: ChildStdout, events: mpsc::Sender<OskEvent>) {
    std::thread::spawn(move || {
        let mut listening = true;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if !listening {
                continue;
            }
            if let Some(ev) = parse_event(&line) {
                listening = events.send(ev).is_ok();
            }
        }
    });
}

/// Resolve the OSK binary to run, in priority order:
/// 1. the `HYPRPAD_OSK_BIN` environment override,
/// 2. `hyprpad-osk` found on `PATH`,
/// 3. the in-tree dev build at `osk/target/release/hyprpad-osk`.
///
/// The last is returned unconditionally as a fallback; if it doesn't exist the
/// spawn simply fails and the handle degrades gracefully.
fn resolve_osk_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("HYPRPAD_OSK_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(p) = find_on_path("hyprpad-osk") {
        return p;
    }
    PathBuf::from("osk/target/release/hyprpad-osk")
}

/// Search `PATH` for an executable named `name`.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|cand| is_executable(cand))
}

/// Whether `path` is a regular file with any execute bit set.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_full_scale_to_unit() {
        assert!((normalize_pad_axis(0) - 0.0).abs() < 1e-6);
        assert!((normalize_pad_axis(32767) - 1.0).abs() < 1e-6);
        assert!((normalize_pad_axis(-32767) + 1.0).abs() < 1e-6);
        // Half scale.
        assert!((normalize_pad_axis(16384) - 0.5).abs() < 1e-3);
        assert!((normalize_pad_axis(-16384) + 0.5).abs() < 1e-3);
    }

    #[test]
    fn normalize_is_clamped_to_unit_interval() {
        // i16::MIN is one count past -32767, so the raw ratio dips below -1.
        assert_eq!(normalize_pad_axis(i16::MIN), -1.0);
        assert!(normalize_pad_axis(i16::MAX) <= 1.0);
        assert!(normalize_pad_axis(i16::MIN) >= -1.0);
    }

    #[test]
    fn show_cmd_formats_mode_and_presentation() {
        assert_eq!(show_cmd(OskMode::Bottom, true), "show bottom reflow");
        assert_eq!(show_cmd(OskMode::Bottom, false), "show bottom overlay");
        assert_eq!(show_cmd(OskMode::Split, true), "show split reflow");
        assert_eq!(show_cmd(OskMode::Split, false), "show split overlay");
    }

    #[test]
    fn cursor_cmd_formats_pad_and_axes() {
        assert_eq!(cursor_cmd(OskPad::Left, -1.0, 1.0), "cursor L -1.0000 1.0000");
        assert_eq!(cursor_cmd(OskPad::Right, 0.0, -0.5), "cursor R 0.0000 -0.5000");
    }

    #[test]
    fn commit_cmd_formats_pad() {
        assert_eq!(commit_cmd(OskPad::Left), "commit L");
        assert_eq!(commit_cmd(OskPad::Right), "commit R");
    }

    #[test]
    fn cursor_cmd_tokens_reparse_as_the_osk_grammar_expects() {
        // We can't link the osk crate, but the tokens must match its parser:
        // "cursor" <L|R> <f32> <f32>.
        let s = cursor_cmd(OskPad::Right, 0.25, -0.75);
        let mut it = s.split_whitespace();
        assert_eq!(it.next(), Some("cursor"));
        assert_eq!(it.next(), Some("R"));
        assert_eq!(it.next().unwrap().parse::<f32>().unwrap(), 0.25);
        assert_eq!(it.next().unwrap().parse::<f32>().unwrap(), -0.75);
        assert_eq!(it.next(), None);
    }

    #[test]
    fn shift_and_key_cmds_match_the_osk_grammar() {
        // `shift down|up` is the OSK's held-shift command (osk/src/control.rs),
        // distinct from the latch levels `off|oneshot|stuck|on`.
        assert_eq!(shift_cmd(true), "shift down");
        assert_eq!(shift_cmd(false), "shift up");
        assert_eq!(key_cmd(&KeyChord::plain(57)), "key 57");
        // A combo trails its modifiers, in press order, after the key.
        let ctrl_bs = KeyChord::parse("ctrl+backspace").expect("parse");
        assert_eq!(key_cmd(&ctrl_bs), "key 14 29");
        let ctrl_shift_tab = KeyChord::parse("ctrl+shift+tab").expect("parse");
        assert_eq!(key_cmd(&ctrl_shift_tab), "key 15 29 42");
    }

    #[test]
    fn full_pipeline_pad_to_cursor_command() {
        // A right-edge, top-of-pad touch -> nearly (+1, +1), passed straight
        // through (pad +Y up == OSK +ny up).
        let cmd = cursor_cmd(
            OskPad::Left,
            normalize_pad_axis(32767),
            normalize_pad_axis(32767),
        );
        assert_eq!(cmd, "cursor L 1.0000 1.0000");
    }

    #[test]
    fn new_handle_is_inactive_and_spawns_nothing() {
        let osk = OskHandle::new();
        assert!(!osk.is_active());
        assert!(osk.child.is_none());
        assert!(osk.stdin.is_none());
        assert!(osk.events.is_none(), "plain handle wants no back-channel");
    }

    #[test]
    fn with_events_handle_arms_the_back_channel_without_spawning() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let osk = OskHandle::with_events(tx);
        assert!(osk.events.is_some());
        assert!(osk.child.is_none());
        assert!(!osk.is_active());
    }

    #[test]
    fn parses_crossing_events_from_the_back_channel() {
        assert_eq!(parse_event("event crossed L"), Some(OskEvent::Crossed(OskPad::Left)));
        assert_eq!(parse_event("event crossed R"), Some(OskEvent::Crossed(OskPad::Right)));
        // Tolerant of surrounding whitespace, as a line read off a pipe may be.
        assert_eq!(
            parse_event("  event   crossed   R  "),
            Some(OskEvent::Crossed(OskPad::Right))
        );
    }

    #[test]
    fn back_channel_ignores_everything_it_does_not_know() {
        // Version tolerance: an unknown event, an unknown pad, a truncated line,
        // a plain log line, and noise all parse to None rather than erroring —
        // a newer OSK can add events without breaking an older daemon.
        assert_eq!(parse_event("event pressed L"), None);
        assert_eq!(parse_event("event crossed X"), None);
        assert_eq!(parse_event("event crossed"), None);
        assert_eq!(parse_event("event"), None);
        assert_eq!(parse_event("hyprpad-osk: show mode=BottomDeck"), None);
        assert_eq!(parse_event(""), None);
        // Extra trailing arguments are tolerated (a future event may add them).
        assert_eq!(
            parse_event("event crossed L 42"),
            Some(OskEvent::Crossed(OskPad::Left))
        );
    }

    #[test]
    fn event_reader_forwards_recognized_lines_off_a_real_pipe() {
        // End-to-end for the reader half: a child writing the back-channel to a
        // piped stdout, read on the thread, parsed, delivered in order. The log
        // line and the unknown event are dropped without desyncing the stream.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'hyprpad-osk: show mode=BottomDeck\\nevent crossed L\\nevent frobnicate\\nevent crossed R\\n'")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn /bin/sh");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_event_reader(stdout, tx);

        assert_eq!(rx.recv().unwrap(), OskEvent::Crossed(OskPad::Left));
        assert_eq!(rx.recv().unwrap(), OskEvent::Crossed(OskPad::Right));
        // EOF ends the reader thread, which drops the sender.
        assert!(rx.recv().is_err());
        let _ = child.wait();
    }

    #[test]
    fn back_channel_pad_tokens_match_the_command_wire_tokens() {
        // The event grammar reuses the `cursor`/`commit` L|R tokens, so the two
        // directions can never drift apart.
        for pad in [OskPad::Left, OskPad::Right] {
            assert_eq!(
                parse_event(&format!("event crossed {}", pad.wire())),
                Some(OskEvent::Crossed(pad))
            );
        }
    }

    #[test]
    fn override_env_resolves_binary_path() {
        // Save/restore: no other test reads this var.
        let saved = std::env::var_os("HYPRPAD_OSK_BIN");
        std::env::set_var("HYPRPAD_OSK_BIN", "/opt/custom/hyprpad-osk");
        assert_eq!(resolve_osk_bin(), PathBuf::from("/opt/custom/hyprpad-osk"));
        match saved {
            Some(v) => std::env::set_var("HYPRPAD_OSK_BIN", v),
            None => std::env::remove_var("HYPRPAD_OSK_BIN"),
        }
    }
}
