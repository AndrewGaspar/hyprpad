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
//! but the **daemon** owns the controller's writable hidraw node ([`crate::haptics`]).
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

/// Format a `candidate accept` / `candidate next` line — the prediction strip's
/// two verbs (`docs/research/osk-prediction.md` §7.2: R1 accepts the highlighted
/// suggestion, L1 moves the highlight along).
fn candidate_cmd(accept: bool) -> String {
    format!("candidate {}", if accept { "accept" } else { "next" })
}

/// Format a `learn on` / `learn off` line: the keyboard's personal word cache
/// gate. The daemon turns it off for windows on [`LEARN_DENY`] (§5.3 rule 2).
fn learn_cmd(on: bool) -> String {
    format!("learn {}", if on { "on" } else { "off" })
}

/// Window classes the keyboard must never learn typed words from, before a
/// config says otherwise: password managers, the polkit agents, and — following
/// ibus-typing-booster's precedent — terminals, where a typed secret is a
/// routine occurrence (`docs/research/osk-prediction.md` §5.3 rule 2).
///
/// Matched case-insensitively against the focused window's class, as a
/// substring, so `1password` catches `1Password` and `com.1password.desktop`
/// alike.
pub const LEARN_DENY: &[&str] = &[
    "1password",
    "keepassxc",
    "bitwarden",
    "polkit",
    "org.kde.polkit-kde-authentication-agent-1",
    "gnome-keyring",
    "hyprlock",
    "foot",
    "kitty",
    "alacritty",
    "ghostty",
];

/// Whether the personal word cache must be switched off for a focused window of
/// this class, given a deny list.
pub fn learn_denied(class: &str, deny: &[String]) -> bool {
    let class = class.to_ascii_lowercase();
    deny.iter().any(|d| !d.is_empty() && class.contains(&d.to_ascii_lowercase()))
}

/// Which window the keyboard's typed prediction context belongs to.
///
/// Hyprland announces one focus change **twice** — `activewindow` (class and
/// title) and then `activewindowv2` (the window's address) — and it re-announces
/// both for the window that *already* has focus, so an announcement is not a
/// change. The address is the window's identity, so it decides; the class is
/// only the fallback for a compositor that has sent no address.
#[derive(Debug, PartialEq, Eq)]
enum FocusedWindow {
    /// The focused window's address, from `activewindowv2`.
    Address(String),
    /// Its class, from `activewindow`, on a compositor that has yet to send an
    /// address. Never reached once one has been seen.
    Class(String),
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
    /// What the last `learn on|off` told the keyboard. Tracked so a focus
    /// change that does not cross the deny list sends nothing, and — more
    /// importantly — so a keyboard *raised inside* a denied window starts
    /// gated, with no focus change to trigger it. `None` until a focus is seen.
    learn: Option<bool>,
    /// The window the prediction context belongs to, as of the last reset.
    /// `None` until the first focus is known (a startup seed, or the first
    /// announcement off the event socket). Compared — never assumed — so
    /// Hyprland re-announcing the window that already has focus costs nothing.
    focus: Option<FocusedWindow>,
    /// Every line `send` was asked to write, in order. Tests assert on it; a
    /// real daemon writes to the child's stdin and keeps nothing.
    #[cfg(test)]
    sent: Vec<String>,
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
            learn: None,
            focus: None,
            #[cfg(test)]
            sent: Vec::new(),
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
            // A `show` resets the keyboard's typed context by itself, but the
            // learning gate is ours: re-state it, so a keyboard raised inside a
            // password manager starts gated without waiting for a focus change
            // that may never come.
            if let Some(on) = self.learn {
                self.send(&learn_cmd(on));
            }
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

    /// Accept the keyboard's highlighted suggestion — the `osk accept` binding,
    /// R1 by default. Ignored unless the keyboard is shown.
    pub fn candidate_accept(&mut self) {
        if !self.active {
            return;
        }
        self.send(&candidate_cmd(true));
    }

    /// Move the suggestion strip's highlight along — the `osk next` binding, L1
    /// by default. Ignored unless the keyboard is shown.
    pub fn candidate_next(&mut self) {
        if !self.active {
            return;
        }
        self.send(&candidate_cmd(false));
    }

    /// The compositor announced the focused window's **class**
    /// (`activewindow`). Two things follow from a focus change, and this is one
    /// of the two calls the frame loop makes for them
    /// (`docs/research/osk-prediction.md` §5.3 rule 2 / §8.1):
    ///
    /// * `class` decides whether the personal word cache may learn here at all.
    ///   Re-evaluated on every announcement, so the gate can never be missed;
    ///   nothing goes down the wire unless the answer actually flipped.
    /// * whatever the keyboard had typed belongs to the *previous* field, so a
    ///   real focus change resets its prediction context — but this line only
    ///   gets to decide that on a compositor that has sent no address.
    ///   [`focus_window_changed`](Self::focus_window_changed) decides once one
    ///   has, because **an `activewindow` line is an announcement, not a
    ///   change**: Hyprland re-emits it for the window that already has focus
    ///   (measured behind a terminal whose title carries a spinner: 3 in 3 s,
    ///   with focus never moving), and resetting on each of those wiped what the
    ///   user had typed about once a second.
    ///
    /// The learning gate is remembered even while the keyboard is down, so the
    /// next `show` starts correctly gated. Everything else is a no-op unless the
    /// keyboard is up.
    pub fn focus_changed(&mut self, class: &str, deny: &[String]) {
        let allow = !learn_denied(class, deny);
        let gate_flipped = self.learn != Some(allow);
        self.learn = Some(allow);
        let moved = match &self.focus {
            // An addressed compositor: the `activewindowv2` line that follows
            // this one carries the identity, and it decides.
            Some(FocusedWindow::Address(_)) => false,
            Some(FocusedWindow::Class(known)) => known.as_str() != class,
            None => true,
        };
        if moved {
            self.focus = Some(FocusedWindow::Class(class.to_string()));
        }
        if !self.active {
            return;
        }
        if moved {
            self.send("context reset");
        }
        if gate_flipped {
            self.send(&learn_cmd(allow));
        }
    }

    /// The compositor announced the focused window's **address**
    /// (`activewindowv2`) — the window's identity, and the closest thing
    /// hyprpad can see to the identity of the text *field* the typed context
    /// really belongs to.
    ///
    /// The prediction context is reset only when that address is not the one it
    /// was already gathered against, so a re-announcement of the focused window
    /// (and the `windowtitle` churn that comes with it) leaves what the user
    /// typed alone. The learning gate is not this line's business: the
    /// `activewindow` line immediately before it carried the class and has
    /// already re-evaluated it.
    pub fn focus_window_changed(&mut self, address: &str) {
        let moved = match &self.focus {
            Some(FocusedWindow::Address(known)) => known.as_str() != address,
            // The class half of *this* announcement, which has already decided:
            // adopting its address is not a second focus change.
            Some(FocusedWindow::Class(_)) => false,
            None => true,
        };
        self.focus = Some(FocusedWindow::Address(address.to_string()));
        if moved && self.active {
            self.send("context reset");
        }
    }

    /// Prime the focused window at startup, from `j/activewindow`, *without*
    /// treating it as a change: the event socket announces only changes, so
    /// without this the first re-announcement of the window that was already
    /// focused when the daemon started would look like a move and wipe the
    /// context. `class` still sets the learning gate, so a keyboard raised
    /// before any focus event starts correctly gated.
    ///
    /// Only an address is an identity worth priming; a reply that carries none
    /// leaves the first announcement to decide, which costs at most one reset
    /// of a context that is still empty.
    pub fn seed_focus(&mut self, class: &str, address: &str, deny: &[String]) {
        self.learn = Some(!learn_denied(class, deny));
        if !address.is_empty() {
            self.focus = Some(FocusedWindow::Address(address.to_string()));
        }
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
        #[cfg(test)]
        self.sent.push(line.to_string());
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
    use crate::hypr::HyprEvent;

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
    fn prediction_cmds_match_the_osk_grammar() {
        assert_eq!(candidate_cmd(true), "candidate accept");
        assert_eq!(candidate_cmd(false), "candidate next");
        assert_eq!(learn_cmd(true), "learn on");
        assert_eq!(learn_cmd(false), "learn off");
    }

    #[test]
    fn the_learn_deny_list_matches_a_window_class_case_insensitively() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        // The shipped list: password managers, polkit, the lock screen, terminals.
        for class in [
            "1Password",
            "com.1password.desktop",
            "KeePassXC",
            "Bitwarden",
            "org.kde.polkit-kde-authentication-agent-1",
            "polkit-gnome-authentication-agent-1",
            "hyprlock",
            "foot",
            "kitty",
            "Alacritty",
            "com.mitchellh.ghostty",
        ] {
            assert!(learn_denied(class, &deny), "{class} must gate learning off");
        }
        // Ordinary windows are not gated.
        for class in ["firefox", "steam", "org.gnome.Nautilus", "", "code-oss"] {
            assert!(!learn_denied(class, &deny), "{class} should be allowed to learn");
        }
        // An empty deny list turns the window gate off entirely, and an empty
        // entry never matches everything by accident.
        assert!(!learn_denied("1Password", &[]));
        assert!(!learn_denied("1Password", &[String::new()]));
        // A config's own list replaces the default.
        assert!(learn_denied("Obsidian", &["obsidian".to_string()]));
        assert!(!learn_denied("1Password", &["obsidian".to_string()]));
    }

    #[test]
    fn a_focus_change_gates_learning_even_before_the_keyboard_is_up() {
        // The gate is remembered while the keyboard is down, so a keyboard
        // raised *inside* a password manager starts gated — there is no focus
        // change after the raise to do it.
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = OskHandle::new();
        assert_eq!(osk.learn, None);
        osk.focus_changed("firefox", &deny);
        assert_eq!(osk.learn, Some(true));
        osk.focus_changed("1Password", &deny);
        assert_eq!(osk.learn, Some(false));
        osk.focus_changed("com.1password.desktop", &deny);
        assert_eq!(osk.learn, Some(false), "still denied, and nothing was re-sent");
        osk.focus_changed("firefox", &deny);
        assert_eq!(osk.learn, Some(true));
        // Nothing was spawned to do any of it.
        assert!(osk.child.is_none() && osk.stdin.is_none());
        assert!(!osk.is_active());
    }

    /// Feed the keyboard exactly what the daemon's compositor arm feeds it
    /// (`src/run.rs`, `Input::Compositor`): the class off an `activewindow`
    /// line, the address off the `activewindowv2` line that follows it, and
    /// nothing at all off a rename.
    fn feed(osk: &mut OskHandle, ev: &HyprEvent, deny: &[String]) {
        match ev {
            HyprEvent::ActiveWindow { class, .. } => osk.focus_changed(class, deny),
            HyprEvent::ActiveWindowV2 { address } => osk.focus_window_changed(address),
            _ => {}
        }
    }

    /// Hyprland's two lines for one focus change, in the order it sends them.
    fn announce(class: &str, address: &str) -> [HyprEvent; 2] {
        [
            HyprEvent::ActiveWindow {
                class: class.to_string(),
                title: String::new(),
                pid: None,
            },
            HyprEvent::ActiveWindowV2 { address: address.to_string() },
        ]
    }

    /// A handle that behaves as if `show` had succeeded, with no child behind
    /// it: `send` records the line and then reports the missing pipe, which
    /// (unlike a broken one) leaves `active` alone.
    fn shown() -> OskHandle {
        let mut osk = OskHandle::new();
        osk.active = true;
        osk
    }

    /// How many times the keyboard was told to throw its typed context away.
    fn resets(osk: &OskHandle) -> usize {
        osk.sent.iter().filter(|l| *l == "context reset").count()
    }

    #[test]
    fn the_context_resets_once_per_focus_change_not_once_per_announcement() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = shown();

        // One focus change, announced the way Hyprland announces it.
        for ev in announce("firefox", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(resets(&osk), 1, "a real focus change resets the context, once");

        // The same window announced again and again — what a terminal with a
        // spinner in its title produces (3 `activewindow` + 3 `activewindowv2`
        // in 3 s, with focus never moving). Before this rule each of those
        // wiped the suggestions the user was reading.
        for _ in 0..3 {
            for ev in announce("firefox", "55d89beaf480") {
                feed(&mut osk, &ev, &deny);
            }
        }
        assert_eq!(resets(&osk), 1, "focus never moved: the typed context must survive");

        // Another window of the *same class* is still another window, and its
        // field starts empty — the address is what says so.
        for ev in announce("firefox", "55d89beaf999") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(resets(&osk), 2, "a second window is a second context");
    }

    #[test]
    fn title_churn_behind_an_unmoved_focus_never_resets_the_context() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = shown();
        for ev in announce("foot", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(resets(&osk), 1);

        // 3 s of the event socket, measured with Claude Code spinning in the
        // focused terminal: 34 `windowtitle` + 33 `windowtitlev2` renames, and
        // 3 re-announcements of a focus that never moved. The renames are the
        // mode engine's business; the keyboard is handed none of them, and the
        // re-announcements carry the address it is already gathering against.
        let mut stream: Vec<HyprEvent> = Vec::new();
        for i in 0..34 {
            stream.push(HyprEvent::WindowTitle {
                address: "55d89beaf480".to_string(),
                title: None,
            });
            if i < 33 {
                stream.push(HyprEvent::WindowTitle {
                    address: "55d89beaf480".to_string(),
                    title: Some(format!("claude · {i}s")),
                });
            }
            if i % 11 == 10 {
                stream.extend(announce("foot", "55d89beaf480"));
            }
        }
        for ev in &stream {
            feed(&mut osk, ev, &deny);
        }
        assert_eq!(resets(&osk), 1, "a spinning title is not a focus change");
    }

    #[test]
    fn a_re_announcement_leaves_the_learning_gate_where_it_was() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = shown();
        for ev in announce("firefox", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(osk.learn, Some(true));
        // Moving into a terminal gates learning — once, however often the move
        // is announced.
        for _ in 0..3 {
            for ev in announce("foot", "55d89beaf999") {
                feed(&mut osk, &ev, &deny);
            }
        }
        assert_eq!(osk.learn, Some(false), "the gate still follows a real focus change");
        assert_eq!(resets(&osk), 2);
        assert_eq!(
            osk.sent.iter().filter(|l| l.starts_with("learn ")).count(),
            2,
            "`learn on` then `learn off`, and nothing for the repeats"
        );
    }

    #[test]
    fn the_startup_seed_primes_the_focus_without_resetting_anything() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = shown();
        osk.seed_focus("foot", "55d89beaf480", &deny);
        assert_eq!(osk.learn, Some(false), "a terminal is seeded gated");
        // The first announcement after a restart is the compositor repeating
        // what the seed already knows, not a focus change.
        for ev in announce("foot", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert!(osk.sent.is_empty(), "nothing at all went down the wire");
        // Moving off the seeded window still does.
        for ev in announce("firefox", "55d89beaf999") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(resets(&osk), 1);

        // A seed with no address to prime still gates learning, and still
        // costs at most the one reset of an empty context.
        let mut bare = shown();
        bare.seed_focus("firefox", "", &deny);
        assert_eq!(bare.learn, Some(true));
        for ev in announce("firefox", "55d89beaf480") {
            feed(&mut bare, &ev, &deny);
        }
        assert_eq!(resets(&bare), 1);
    }

    #[test]
    fn without_addresses_the_focus_falls_back_to_the_window_class() {
        // A compositor that sends no `activewindowv2` keeps exactly the old
        // behaviour, minus the re-announcements: the class is the identity, so
        // two windows of one class read as one field. That is the best an
        // address-less event stream allows.
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = shown();
        osk.focus_changed("firefox", &deny);
        assert_eq!(resets(&osk), 1, "the first focus of the session");
        osk.focus_changed("firefox", &deny);
        assert_eq!(resets(&osk), 1, "re-announced, same window");
        osk.focus_changed("foot", &deny);
        assert_eq!(resets(&osk), 2);
    }

    #[test]
    fn a_focus_change_with_the_keyboard_down_sends_nothing_but_is_remembered() {
        let deny: Vec<String> = LEARN_DENY.iter().map(|s| s.to_string()).collect();
        let mut osk = OskHandle::new();
        for ev in announce("firefox", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert!(osk.sent.is_empty() && !osk.is_active());
        // Raising the keyboard here must not be followed by a reset the next
        // time the compositor re-announces this same window.
        osk.active = true;
        for ev in announce("firefox", "55d89beaf480") {
            feed(&mut osk, &ev, &deny);
        }
        assert_eq!(resets(&osk), 0);
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
