//! The bridge to the on-screen keyboard (`hyprpad-osk`).
//!
//! [`OskHandle`] owns the OSK as a **child process** and drives it over the
//! control channel described in `osk/src/control.rs`: a line-based text protocol
//! we speak on the child's **stdin** (`hyprpad-osk --stdin`). The vocabulary is
//! the dual-trackpad one the OSK already understands — `show`, `hide`,
//! `cursor <L|R> <nx> <ny>`, `commit <L|R>`, `quit` — so the daemon forwards
//! each pad's absolute cursor and its click independently.
//!
//! The handle is deliberately forgiving: the OSK binary may not be installed,
//! and the daemon must keep running without it. Spawn is **lazy** (on the first
//! [`show`](OskHandle::show)) and every failure degrades to a logged warning
//! plus a no-op — never a panic and never a daemon exit. The child is killed on
//! [`Drop`], i.e. when the daemon exits.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

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

/// Format a `key` command line: the OSK taps this raw evdev keycode on its
/// virtual keyboard. Used by the Deck-style helper buttons (`[osk_buttons]`,
/// e.g. Y = Space) so common keys need no cursor hunting.
fn key_cmd(keycode: u16) -> String {
    format!("key {keycode}")
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
}

impl OskHandle {
    /// A handle that has not spawned anything yet.
    pub fn new() -> Self {
        Self::default()
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

    /// Tap a raw evdev keycode through the OSK's virtual keyboard (the
    /// `[osk_buttons]` Deck-style helpers, e.g. Y = Space, X = Backspace).
    /// Ignored unless the keyboard is shown.
    pub fn key(&mut self, keycode: u16) {
        if !self.active {
            return;
        }
        self.send(&key_cmd(keycode));
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
        match spawn_osk(&bin) {
            Ok((child, stdin)) => {
                eprintln!("hyprpad: on-screen keyboard ready ({})", bin.display());
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

/// Spawn `hyprpad-osk --stdin` with a piped stdin, inheriting stdout/stderr so
/// its logs surface alongside the daemon's.
fn spawn_osk(bin: &Path) -> std::io::Result<(Child, ChildStdin)> {
    let mut child = Command::new(bin)
        .arg("--stdin")
        .stdin(Stdio::piped())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .expect("child spawned with Stdio::piped() has a stdin");
    Ok((child, stdin))
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
