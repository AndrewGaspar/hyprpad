//! IPC with a running Hyprland compositor.
//!
//! Target is a HypXRland fork (Omarchy 4 "Quattro", Hyprland 0.56.2) that
//! drives its configuration from Lua. Two UNIX sockets live under
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//!
//!   - `.socket.sock`  — one-shot request/reply command socket
//!   - `.socket2.sock` — line-oriented event stream
//!
//! ## Dispatch grammar on this fork (determined empirically)
//!
//! Classic Hyprland dispatch is dead here. The command socket's
//! `dispatch <X>` line is routed through the Lua engine as
//! `return hl.dispatch(<X>)`, so `<X>` must be a Lua expression evaluating to
//! an `HL.Dispatcher`. Consequences that were verified against the live
//! compositor:
//!
//!   - `dispatch workspace e+1` (the classic string form) is a Lua *syntax
//!     error* — the socket replies `error: ... ')' expected near 'e'`.
//!   - `hyprctl keyword` refuses to run under the Lua config manager and
//!     exits 0 without effect.
//!   - Dispatchers come from the curated `hl.dsp.*` table. The ones this
//!     module needs, with the exact working form:
//!
//!     | intent                        | dispatcher                                        |
//!     |-------------------------------|---------------------------------------------------|
//!     | workspace, relative (+1/-1)   | `hl.dsp.focus({ workspace = "e+1" })`             |
//!     | workspace, by selector        | `hl.dsp.focus({ workspace = "emptyn" })`          |
//!     | move active window to ws      | `hl.dsp.window.move({ workspace = "e+1" })`       |
//!     | toggle fullscreen             | `hl.dsp.window.fullscreen({ mode = "fullscreen" })` |
//!     | focus a window by address     | `hl.dsp.focus({ window = "address:0x..." })`      |
//!
//!     Note the surprises: switching workspace is a `focus` verb (not a
//!     `workspace` one), and `hl.dsp.window.*` operations accept an optional
//!     `window = "address:0x..."` selector to target a specific window instead
//!     of the active one (proven by fullscreening a non-active window).
//!
//!     The `workspace = …` field is *not* validated at config time: the string
//!     reaches `getWorkspaceIDNameFromString` when the dispatcher fires, so
//!     the whole selector grammar (`empty`, `emptyn`, `previous`, `r+1`,
//!     `special:foo`, `name:foo`, a bare id, …) is legal there. hyprpad
//!     therefore hands selectors over untouched — see
//!     [`crate::config::WorkspaceTarget`].
//!
//! The command socket is one-shot: connect, write the request, read the whole
//! reply, the server closes the connection. A dispatch that lands replies the
//! literal `ok`; a failure replies a line beginning `error:` (surfaced here as
//! an [`io::Error`]). JSON queries use the `j/<command>` request form.
//!
//! `spawn` is *not* a dispatcher — it execs a shell command directly (see the
//! method) so the daemon can launch helpers (e.g. a PiP `mpv`).
//!
//! ## A note on focus
//!
//! `focus_window` issues the idiomatic `hl.dsp.focus({ window = "address:.." })`
//! and the socket accepts it (`ok`). During bring-up on the live session the
//! *visible* focus never moved for any focus verb issued over IPC — not
//! `focuswindow`, not `movefocus`, not `cyclenext`, and a freshly opened window
//! did not take focus either — because keyboard focus was externally pinned to
//! one (grouped) terminal. Workspace switching, fullscreen, and window moves
//! were all confirmed to take effect and emit events, so the command path
//! itself is sound; only the focus outcome could not be demonstrated in that
//! environment.

use crate::config::WorkspaceTarget;
use std::collections::BTreeSet;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// A handle to the running compositor's command socket.
///
/// Cheap to hold: it stores only the runtime socket directory. Each request
/// opens a fresh connection, matching the socket's one-shot protocol, so a
/// `Hypr` is `Send + Sync` and can be shared freely.
pub struct Hypr {
    /// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE`.
    dir: PathBuf,
}

impl Hypr {
    /// Locate the compositor from the environment. Fails if the instance
    /// signature is unset or the command socket is absent (no Hyprland).
    pub fn connect() -> io::Result<Hypr> {
        let dir = socket_dir()?;
        let sock = dir.join(".socket.sock");
        if !sock.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Hyprland command socket not found at {}", sock.display()),
            ));
        }
        Ok(Hypr { dir })
    }

    /// A handle that points at no compositor, for tests of code that carries a
    /// `Hypr` but takes a path that never talks to it (a mode override, say).
    /// Any request through it fails with a socket error rather than reaching a
    /// live session.
    #[cfg(test)]
    pub(crate) fn detached() -> Hypr {
        Hypr { dir: PathBuf::from("/nonexistent/hyprpad-detached") }
    }

    /// Switch to the workspace `t` points at.
    pub fn workspace(&self, t: &WorkspaceTarget) -> io::Result<()> {
        self.dispatch_ok(&focus_workspace_expr(t))
    }

    /// Move the active window to the workspace `t` points at; it follows, as
    /// with the classic `movetoworkspace`.
    pub fn move_window_to_workspace(&self, t: &WorkspaceTarget) -> io::Result<()> {
        self.dispatch_ok(&move_window_to_workspace_expr(t))
    }

    /// Toggle fullscreen on the active window. Repeated calls flip it on/off
    /// (verified: the window's `fullscreen` state cycled `0 -> 2 -> 0`).
    pub fn toggle_fullscreen(&self) -> io::Result<()> {
        self.dispatch_ok("hl.dsp.window.fullscreen({ mode = \"fullscreen\" })")
    }

    /// Focus the window with the given address. Accepts either the raw
    /// `0x…` form or an already-prefixed `address:0x…` and normalizes to the
    /// `address:0x…` selector Hyprland expects.
    pub fn focus_window(&self, address: &str) -> io::Result<()> {
        let selector = if address.starts_with("address:") {
            address.to_string()
        } else {
            format!("address:{}", norm_addr(address))
        };
        self.dispatch_ok(&format!(
            "hl.dsp.focus({{ window = \"{}\" }})",
            lua_escape(&selector)
        ))
    }

    /// Escape hatch: dispatch an arbitrary `hl.dsp.*` Lua expression. `payload`
    /// is the dispatcher expression only — this prepends the `dispatch ` verb.
    /// Returns the socket's raw reply (`ok`, JSON, or an `error:` line, which
    /// is turned into an [`io::Error`]).
    pub fn dispatch_raw(&self, payload: &str) -> io::Result<String> {
        let reply = self.request(&format!("dispatch {payload}"))?;
        if reply.starts_with("error:") {
            return Err(io::Error::other(reply.trim().to_string()));
        }
        Ok(reply)
    }

    /// Run a JSON query (`clients`, `activewindow`, `activeworkspace`, …) and
    /// return the raw JSON reply. Uses the socket's `j/<command>` form.
    pub fn query(&self, command: &str) -> io::Result<String> {
        self.request(&format!("j/{command}"))
    }

    /// The focused window's `pid`, fullscreen state and current title, from
    /// `j/activewindow`.
    ///
    /// The `activewindow` **event** carries only class and title, so this fills
    /// the gap for the modality layer: `pid` is what a mode rule's
    /// `ctx.focus:process_tree_has(..)` walks, and `fullscreen` is what an
    /// opt-in rule reads. Verified against the live compositor: the reply has
    /// `"pid": <n>` and `"fullscreen": <n>` (0 = not fullscreen).
    ///
    /// `title` is the *live* title, which is why the title-rescan path reads it
    /// too: a compositor that emits only the bare `windowtitle>>ADDRESS` form
    /// says a window was renamed without saying to what.
    ///
    /// Called on a focus change (and, on such a compositor, on a rename of the
    /// focused window) — never per frame — and only when a config actually
    /// declares modes ([`crate::config::Config::needs_focus_pid`]).
    pub fn active_window(&self) -> io::Result<ActiveWindowInfo> {
        let json = self.query("activewindow")?;
        Ok(parse_active_window(&json))
    }

    /// The namespaces of every layer-shell surface currently on screen, from
    /// `j/layers`.
    ///
    /// The startup counterpart of [`HyprEvent::Layer`]: the event socket
    /// announces only *changes*, so a daemon (re)started while the cheat sheet
    /// — or, in future, the OSK — is already up would otherwise never know it
    /// is there, and would sit in the wrong mode until the overlay was closed
    /// and reopened. Called once, at startup, and only when a config actually
    /// declares modes.
    ///
    /// Verified against the live compositor: the reply nests
    /// monitor → `levels` → level → an array of surfaces, each with a
    /// `"namespace"`, and the cheat sheet shows up as `hyprpad-cheatsheet` on
    /// level 3 alongside `omarchy-bar` and `omarchy-background`.
    pub fn open_layers(&self) -> io::Result<BTreeSet<String>> {
        Ok(parse_layer_namespaces(&self.query("layers")?))
    }

    /// Fire-and-forget exec of a shell command, detached from our stdio. A
    /// short-lived reaper thread waits on the child so it never lingers as a
    /// zombie; if the daemon exits first the child is simply reparented and
    /// keeps running. The command inherits our environment (so
    /// `WAYLAND_DISPLAY` etc. reach GUI children).
    pub fn spawn(&self, cmd: &str) {
        self.spawn_env(cmd, &[]);
    }

    /// [`spawn`](Self::spawn), with `env` set on the child on top of ours.
    ///
    /// This is how a child learns something about the moment it was launched
    /// that it could not ask for afterwards — `HYPRPAD_MODE`, the mode the
    /// chord fired in, which is already stale by the time an overlay the child
    /// summons has changed it.
    pub fn spawn_env(&self, cmd: &str, env: &[(&str, &str)]) {
        let cmd = cmd.to_string();
        let env: Vec<(String, String)> =
            env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        thread::spawn(move || {
            let child = Command::new("/bin/sh")
                .arg("-c")
                .arg(&cmd)
                .envs(env)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = child {
                let _ = child.wait();
            } else {
                eprintln!("hypr: failed to spawn: {cmd}");
            }
        });
    }

    /// Send a dispatcher and require an `ok`, discarding the reply.
    fn dispatch_ok(&self, payload: &str) -> io::Result<()> {
        self.dispatch_raw(payload).map(|_| ())
    }

    /// One request/reply round-trip on the command socket.
    fn request(&self, payload: &str) -> io::Result<String> {
        let mut stream = UnixStream::connect(self.dir.join(".socket.sock"))?;
        stream.write_all(payload.as_bytes())?;
        // The server reads the request, replies, and closes; reading to end
        // yields the whole reply. No half-close is needed (confirmed live).
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        Ok(reply)
    }
}

/// The fields of `j/activewindow` the modality layer needs, and which the
/// `activewindow` event does not carry (or, for `title`, does not carry on a
/// `windowtitle` rename).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveWindowInfo {
    /// The focused window's process id.
    pub pid: Option<i32>,
    /// Whether it is fullscreen (any non-zero Hyprland fullscreen state).
    pub fullscreen: bool,
    /// Its title as of this query. Empty when the reply carries none.
    pub title: String,
    /// Its window class. Empty when focus is on the desktop (no window).
    pub class: String,
    /// Its window address in the event-socket form (no `0x` prefix), so it
    /// compares directly against `activewindowv2` / `windowtitle` addresses.
    pub address: String,
}

/// Pull a top-level numeric field out of a Hyprland JSON reply.
///
/// A four-line scanner rather than a JSON crate, matching this crate's
/// hand-rolled ethos (see `config.rs`'s note on the TOML parser): the replies
/// are machine-generated, flat where we read them, and we want exactly two
/// integers out of one of them. It looks for `"<field>":` at the top level —
/// nesting is irrelevant here because `pid` and `fullscreen` appear once each
/// in an `activewindow` reply.
fn json_number(json: &str, field: &str) -> Option<i64> {
    let needle = format!("\"{field}\":");
    let rest = &json[json.find(&needle)? + needle.len()..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// Pull a top-level string field out of a Hyprland JSON reply, undoing the two
/// escapes its writer emits (`\"` and `\\`).
///
/// Same four-line-scanner ethos as [`json_number`], and safe against the
/// neighbouring `initialTitle` / `initialClass` fields because the needle
/// carries the opening quote: `"title":` is not a substring of
/// `"initialTitle":`.
fn json_string(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    json_string_at(&json[json.find(&needle)? + needle.len()..])
}

/// Read the JSON string value that starts at `rest` (leading whitespace
/// allowed), undoing the two escapes Hyprland's writer emits.
///
/// Split out of [`json_string`] because a `j/layers` reply carries the *same*
/// field many times over and [`parse_layer_namespaces`] wants every one of
/// them, not the first.
fn json_string_at(rest: &str) -> Option<String> {
    let mut chars = rest.trim_start().strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            _ => out.push(c),
        }
    }
    None // unterminated string: a truncated reply, not a title
}

/// Every `namespace` in a `j/layers` reply, deduplicated and sorted.
///
/// Same four-line-scanner ethos as [`json_string`], one `find` per hit: the
/// reply's nesting (monitor → `levels` → level → surfaces) carries no
/// information we want, because *every* object in it is a layer surface, so the
/// open namespaces are simply every `"namespace"` field in the document. A
/// nameless surface is dropped — there is nothing a rule could ask about it.
///
/// Crate-visible so [`crate::mode`]'s startup-seed test can drive the whole
/// path — captured reply in, resolved mode out — rather than hand-writing the
/// set this produces and hoping the two agree.
pub(crate) fn parse_layer_namespaces(json: &str) -> BTreeSet<String> {
    const NEEDLE: &str = "\"namespace\":";
    let mut out = BTreeSet::new();
    let mut rest = json;
    while let Some(at) = rest.find(NEEDLE) {
        rest = &rest[at + NEEDLE.len()..];
        if let Some(ns) = json_string_at(rest).filter(|ns| !ns.is_empty()) {
            out.insert(ns);
        }
    }
    out
}

/// A parsed line from the `.socket2.sock` event stream.
///
/// Addresses are normalized to the `0x…` form used by the JSON queries and by
/// [`Hypr::focus_window`], even though the wire format omits the `0x` prefix,
/// so an address from an event can be fed straight back to `focus_window`.
/// Any event this module does not model becomes [`HyprEvent::Other`] with its
/// name and unparsed data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HyprEvent {
    /// Keyboard focus moved to a window. `activewindow>>class,title`.
    ///
    /// `pid` is **not** on the wire — the event stream carries class and title
    /// only. It is `None` from [`subscribe`] and filled in by the daemon from
    /// [`Hypr::active_window`] when a config actually needs it (a `config.lua`
    /// mode rule calling `ctx.focus:process_tree_has(..)`), so the extra socket
    /// round trip is paid only by configs that use it.
    ActiveWindow { class: String, title: String, pid: Option<i32> },
    /// Keyboard focus moved, address form. `activewindowv2>>address`.
    ActiveWindowV2 { address: String },
    /// A window renamed itself. Both wire forms land here, distinguished by
    /// whether the new title was on the line:
    ///
    /// ```text
    /// windowtitle>>55d89beaf480              -> title: None
    /// windowtitlev2>>55d89beaf480,⠙ ds-decomp -> title: Some("⠙ ds-decomp")
    /// ```
    ///
    /// Captured from the live compositor (HypXRland, Hyprland 0.56.2), which
    /// emits **both**, in that order, for every rename. This is the event that
    /// makes a program started inside an already-focused terminal — `claude` in
    /// a shell — visible to the mode rules without a refocus, because starting
    /// it is what renames the terminal.
    WindowTitle { address: String, title: Option<String> },
    /// The active window's fullscreen state changed. `fullscreen>>0|1`
    /// (any non-zero state is `true`).
    Fullscreen(bool),
    /// A window opened. `openwindow>>address,workspace,class,title`.
    OpenWindow {
        address: String,
        workspace: String,
        class: String,
        title: String,
    },
    /// A window closed. `closewindow>>address`.
    CloseWindow { address: String },
    /// The active workspace changed. `workspace>>name`.
    Workspace { name: String },
    /// A layer-shell surface appeared or disappeared:
    ///
    /// ```text
    /// openlayer>>hyprpad-cheatsheet
    /// closelayer>>hyprpad-cheatsheet
    /// ```
    ///
    /// Captured verbatim from the live compositor by toggling the cheat sheet.
    /// The wire carries the **namespace only** — no address, nothing else —
    /// which is why [`crate::mode::Context`] tracks a set of names rather than
    /// a map of surfaces.
    ///
    /// This is what makes an overlay a *context*. The same capture shows the
    /// sheet taking keyboard focus with **no** `activewindow` line behind it
    /// (the focus event fires only on the way back out, when the sheet closes),
    /// so a focus-only engine is blind to it: hyprpad's own cheat sheet is up,
    /// modal, and swallowing the keyboard, and nothing else on the event stream
    /// says so.
    Layer { namespace: String, open: bool },
    /// Any event not modelled above, passed through verbatim.
    Other { name: String, data: String },
}

/// Subscribe to the compositor event stream.
///
/// Connects to `.socket2.sock` up front (so a missing compositor surfaces as an
/// error to the caller), then spawns a thread that parses each `EVENT>>data`
/// line into a [`HyprEvent`] and forwards it. The channel closes when the
/// socket closes or the receiver is dropped.
pub fn subscribe() -> io::Result<mpsc::Receiver<HyprEvent>> {
    let path = socket_dir()?.join(".socket2.sock");
    let stream = UnixStream::connect(&path)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = io::BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(event) = parse_event(&line) {
                if tx.send(event).is_err() {
                    break; // receiver gone
                }
            }
        }
    });
    Ok(rx)
}

/// Parse one `EVENT>>data` line. Returns `None` only for a line with no `>>`
/// delimiter; every delimited line yields an event (unknown names → `Other`).
fn parse_event(line: &str) -> Option<HyprEvent> {
    let (name, data) = line.split_once(">>")?;
    let event = match name {
        "activewindow" => {
            // class,title — a title may itself contain commas, so split once.
            let (class, title) = data.split_once(',').unwrap_or((data, ""));
            HyprEvent::ActiveWindow {
                class: class.to_string(),
                title: title.to_string(),
                pid: None,
            }
        }
        "activewindowv2" => HyprEvent::ActiveWindowV2 {
            address: norm_addr(data),
        },
        "windowtitle" => HyprEvent::WindowTitle {
            address: norm_addr(data.trim()),
            title: None,
        },
        "windowtitlev2" => {
            // address,title — a title may itself contain commas, so split once.
            let (address, title) = data.split_once(',').unwrap_or((data, ""));
            HyprEvent::WindowTitle {
                address: norm_addr(address),
                title: Some(title.to_string()),
            }
        }
        "fullscreen" => HyprEvent::Fullscreen(data.trim() != "0"),
        "openwindow" => {
            // address,workspace,class,title — title may contain commas.
            let mut it = data.splitn(4, ',');
            HyprEvent::OpenWindow {
                address: norm_addr(it.next().unwrap_or("")),
                workspace: it.next().unwrap_or("").to_string(),
                class: it.next().unwrap_or("").to_string(),
                title: it.next().unwrap_or("").to_string(),
            }
        }
        "closewindow" => HyprEvent::CloseWindow {
            address: norm_addr(data),
        },
        "workspace" => HyprEvent::Workspace {
            name: data.to_string(),
        },
        // The namespace is taken verbatim: it is matched against the name a
        // widget registered with (`hyprpad-cheatsheet`) and against the
        // `"namespace"` field of a `j/layers` reply, and neither trims.
        "openlayer" => HyprEvent::Layer {
            namespace: data.to_string(),
            open: true,
        },
        "closelayer" => HyprEvent::Layer {
            namespace: data.to_string(),
            open: false,
        },
        _ => HyprEvent::Other {
            name: name.to_string(),
            data: data.to_string(),
        },
    };
    Some(event)
}

/// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE`.
fn socket_dir() -> io::Result<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HYPRLAND_INSTANCE_SIGNATURE unset (is Hyprland running?)",
        )
    })?;
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR unset"))?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

/// Build the `e±n` relative-workspace argument.
fn rel_arg(n: i32) -> String {
    if n < 0 {
        format!("e{n}") // n already carries the '-'
    } else {
        format!("e+{n}")
    }
}

/// The Hyprland selector string a [`WorkspaceTarget`] stands for.
///
/// Only [`Relative`](WorkspaceTarget::Relative) is translated (to `e±n`);
/// a [`Number`](WorkspaceTarget::Number) becomes the bare id — never `name:N`,
/// which would allocate a *named* workspace with a negative id when id `N` does
/// not exist yet — and a [`Selector`](WorkspaceTarget::Selector) is handed over
/// exactly as written, because the compositor owns that grammar.
fn workspace_arg(t: &WorkspaceTarget) -> String {
    match t {
        WorkspaceTarget::Relative(n) => rel_arg(*n),
        WorkspaceTarget::Number(n) => n.to_string(),
        WorkspaceTarget::Selector(s) => s.clone(),
    }
}

/// The `hl.dsp.focus` expression that switches to `t`. Pure, so the wire form
/// is tested without a compositor.
fn focus_workspace_expr(t: &WorkspaceTarget) -> String {
    format!(
        "hl.dsp.focus({{ workspace = \"{}\" }})",
        lua_escape(&workspace_arg(t))
    )
}

/// The `hl.dsp.window.move` expression that takes the active window to `t`.
/// `follow` is left unset, which is the dispatcher's default (`silent` is only
/// true when `follow` is present and false — `LuaBindingsDispatchers.cpp:900-901`),
/// so the window is followed. Pure, like [`focus_workspace_expr`].
fn move_window_to_workspace_expr(t: &WorkspaceTarget) -> String {
    format!(
        "hl.dsp.window.move({{ workspace = \"{}\" }})",
        lua_escape(&workspace_arg(t))
    )
}

/// Parse a `j/activewindow` reply into the fields the modality layer needs.
/// Pure, so it is tested against a captured live reply.
fn parse_active_window(json: &str) -> ActiveWindowInfo {
    ActiveWindowInfo {
        pid: json_number(json, "pid").map(|n| n as i32),
        fullscreen: json_number(json, "fullscreen").is_some_and(|n| n != 0),
        title: json_string(json, "title").unwrap_or_default(),
        class: json_string(json, "class").unwrap_or_default(),
        // The JSON reply spells the address `0x55d…`; the event socket
        // (`activewindowv2>>55d…`, `windowtitle>>55d…`) omits the `0x`.
        // Normalize to the event form so the two compare directly.
        address: json_string(json, "address")
            .map(|a| a.trim_start_matches("0x").to_string())
            .unwrap_or_default(),
    }
}

/// Ensure an address carries the `0x` prefix (event wire form omits it).
fn norm_addr(s: &str) -> String {
    if s.is_empty() || s.starts_with("0x") {
        s.to_string()
    } else {
        format!("0x{s}")
    }
}

/// Escape a value for embedding inside a Lua double-quoted string.
fn lua_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A real `j/layers` reply captured from the live compositor with the cheat
/// sheet **up** — verbatim, including the empty levels and the compositor's own
/// indentation, so the shape (monitor → `levels` → level → surfaces) is exactly
/// what the daemon will meet at startup.
///
/// At module scope, and crate-visible, because [`crate::mode`]'s startup-seed
/// test drives the same capture through the same parser: one fixture, one
/// answer, no chance of the two drifting apart.
#[cfg(test)]
pub(crate) const CAPTURED_LAYERS_JSON: &str = r#"{
"eDP-2": {
    "levels": {

        "0": [
                {
                    "address": "0x55d89c104290",
                    "x": 0,
                    "y": 0,
                    "w": 2048,
                    "h": 1280,
                    "alpha": 1,
                    "depth": 0.000,
                    "namespace": "omarchy-background",
                    "pid": 3229091
                }
        ],
        "1": [
],
        "2": [
                {
                    "address": "0x55d89c41ff70",
                    "x": 0,
                    "y": 0,
                    "w": 2048,
                    "h": 26,
                    "alpha": 1,
                    "depth": 0.800,
                    "namespace": "omarchy-bar",
                    "pid": 3229091
                }
        ],
        "3": [
                {
                    "address": "0x55d89c436500",
                    "x": 0,
                    "y": 0,
                    "w": 2048,
                    "h": 1280,
                    "alpha": 1,
                    "depth": 0.800,
                    "namespace": "hyprpad-cheatsheet",
                    "pid": 3229091
                }
        ]
    }
}
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The startup focus seed needs `class` (what the game-mode predicate
    /// matches) and `address` in the event-socket form (what title-rescan
    /// attribution compares against) — both parsed from the live reply.
    #[test]
    fn active_window_parses_class_and_event_form_address() {
        let info = parse_active_window(ACTIVE_WINDOW_JSON);
        assert_eq!(info.class, "foot");
        assert_eq!(info.address, "55d89bdb6a70", "0x prefix must be stripped");
        assert_eq!(info.title, "◑ hyprpad");
        assert_eq!(info.pid, Some(3996259));
        assert!(!info.fullscreen);
        // No window focused: empty class, empty address, nothing to seed.
        let none = parse_active_window("{}");
        assert!(none.class.is_empty() && none.address.is_empty());
    }

    /// A real `j/activewindow` reply from the live compositor (Hyprland 0.56.2,
    /// HypXRland fork), trimmed to the fields this module reads plus enough
    /// neighbours to keep the scanner honest.
    const ACTIVE_WINDOW_JSON: &str = r#"{
    "address": "0x55d89bdb6a70",
    "at": [0, 54],
    "size": [2048, 1226],
    "class": "foot",
    "title": "◑ hyprpad",
    "pid": 3996259,
    "xwayland": false,
    "pinned": false,
    "pinFullscreened": false,
    "fullscreen": 0,
    "fullscreenClient": 0,
    "depth": 0.600
}"#;

    #[test]
    fn reads_pid_and_fullscreen_out_of_an_active_window_reply() {
        assert_eq!(json_number(ACTIVE_WINDOW_JSON, "pid"), Some(3996259));
        assert_eq!(json_number(ACTIVE_WINDOW_JSON, "fullscreen"), Some(0));
        // A field that isn't there, and one whose value isn't a number, are
        // `None` rather than a wrong answer.
        assert_eq!(json_number(ACTIVE_WINDOW_JSON, "nosuchfield"), None);
        assert_eq!(json_number(ACTIVE_WINDOW_JSON, "class"), None);
        // A fullscreened window: any non-zero state counts.
        let fs = ACTIVE_WINDOW_JSON.replace("\"fullscreen\": 0", "\"fullscreen\": 2");
        assert_eq!(json_number(&fs, "fullscreen"), Some(2));
    }

    #[test]
    fn parses_active_window() {
        assert_eq!(
            parse_event("activewindow>>foot,ajg@framework:~/code/hyprpad"),
            Some(HyprEvent::ActiveWindow {
                class: "foot".into(),
                title: "ajg@framework:~/code/hyprpad".into(),
                pid: None,
            })
        );
    }

    #[test]
    fn active_window_title_may_contain_commas() {
        assert_eq!(
            parse_event("activewindow>>google-chrome,Doc, v2, final — Chrome"),
            Some(HyprEvent::ActiveWindow {
                class: "google-chrome".into(),
                title: "Doc, v2, final — Chrome".into(),
                pid: None,
            })
        );
    }

    #[test]
    fn active_window_with_empty_title() {
        assert_eq!(
            parse_event("activewindow>>foot"),
            Some(HyprEvent::ActiveWindow {
                class: "foot".into(),
                title: String::new(),
                pid: None,
            })
        );
    }

    #[test]
    fn reads_the_title_out_of_an_active_window_reply() {
        assert_eq!(json_string(ACTIVE_WINDOW_JSON, "title").as_deref(), Some("◑ hyprpad"));
        // `"title":` must not be found inside `"initialTitle":`.
        let with_initial = ACTIVE_WINDOW_JSON
            .replace("\"title\":", "\"initialTitle\": \"foot\",\n    \"title\":");
        assert_eq!(json_string(&with_initial, "title").as_deref(), Some("◑ hyprpad"));
        // Escapes are undone; a missing or unterminated field is `None`.
        let quoted = ACTIVE_WINDOW_JSON.replace("◑ hyprpad", "a \\\"b\\\" c");
        assert_eq!(json_string(&quoted, "title").as_deref(), Some(r#"a "b" c"#));
        assert_eq!(json_string(ACTIVE_WINDOW_JSON, "nosuchfield"), None);
        assert_eq!(json_string(ACTIVE_WINDOW_JSON, "pid"), None);
    }

    /// Both wire forms, exactly as captured from the live compositor.
    #[test]
    fn parses_both_window_title_forms() {
        assert_eq!(
            parse_event("windowtitle>>55d89beaf480"),
            Some(HyprEvent::WindowTitle {
                address: "0x55d89beaf480".into(),
                title: None,
            })
        );
        assert_eq!(
            parse_event("windowtitlev2>>55d89beaf480,⠙ ds-decomp"),
            Some(HyprEvent::WindowTitle {
                address: "0x55d89beaf480".into(),
                title: Some("⠙ ds-decomp".into()),
            })
        );
        // A renamed-to-nothing window, and a title carrying commas.
        assert_eq!(
            parse_event("windowtitlev2>>abc,"),
            Some(HyprEvent::WindowTitle {
                address: "0xabc".into(),
                title: Some(String::new()),
            })
        );
        assert_eq!(
            parse_event("windowtitlev2>>abc,Doc, v2, final — Chrome"),
            Some(HyprEvent::WindowTitle {
                address: "0xabc".into(),
                title: Some("Doc, v2, final — Chrome".into()),
            })
        );
    }

    #[test]
    fn parses_active_window_v2_and_adds_0x() {
        assert_eq!(
            parse_event("activewindowv2>>55d89beaf480"),
            Some(HyprEvent::ActiveWindowV2 {
                address: "0x55d89beaf480".into(),
            })
        );
    }

    #[test]
    fn parses_fullscreen_states() {
        assert_eq!(parse_event("fullscreen>>1"), Some(HyprEvent::Fullscreen(true)));
        assert_eq!(parse_event("fullscreen>>0"), Some(HyprEvent::Fullscreen(false)));
        // A non-zero fullscreen mode (2 == real fullscreen) still reads true.
        assert_eq!(parse_event("fullscreen>>2"), Some(HyprEvent::Fullscreen(true)));
    }

    #[test]
    fn parses_open_window() {
        assert_eq!(
            parse_event("openwindow>>55d89d429360,7,foot,hyprpad-focus-probe"),
            Some(HyprEvent::OpenWindow {
                address: "0x55d89d429360".into(),
                workspace: "7".into(),
                class: "foot".into(),
                title: "hyprpad-focus-probe".into(),
            })
        );
    }

    #[test]
    fn open_window_title_may_contain_commas() {
        assert_eq!(
            parse_event("openwindow>>abc,special:scratch,mpv,Big, Buck, Bunny"),
            Some(HyprEvent::OpenWindow {
                address: "0xabc".into(),
                workspace: "special:scratch".into(),
                class: "mpv".into(),
                title: "Big, Buck, Bunny".into(),
            })
        );
    }

    #[test]
    fn parses_close_window() {
        assert_eq!(
            parse_event("closewindow>>55d89d429360"),
            Some(HyprEvent::CloseWindow {
                address: "0x55d89d429360".into(),
            })
        );
    }

    #[test]
    fn parses_workspace() {
        assert_eq!(
            parse_event("workspace>>7"),
            Some(HyprEvent::Workspace { name: "7".into() })
        );
    }

    /// Both lines exactly as the live compositor emitted them while the cheat
    /// sheet was toggled on and off (`socat` on `.socket2.sock`, HypXRland /
    /// Hyprland 0.56.2).
    #[test]
    fn parses_both_layer_lines() {
        assert_eq!(
            parse_event("openlayer>>hyprpad-cheatsheet"),
            Some(HyprEvent::Layer {
                namespace: "hyprpad-cheatsheet".into(),
                open: true,
            })
        );
        assert_eq!(
            parse_event("closelayer>>hyprpad-cheatsheet"),
            Some(HyprEvent::Layer {
                namespace: "hyprpad-cheatsheet".into(),
                open: false,
            })
        );
        // The namespace is the whole payload, verbatim — no address follows it,
        // and nothing is trimmed off it.
        assert_eq!(
            parse_event("openlayer>>hyprpad-osk"),
            Some(HyprEvent::Layer {
                namespace: "hyprpad-osk".into(),
                open: true,
            })
        );
        assert_eq!(
            parse_event("closelayer>>"),
            Some(HyprEvent::Layer {
                namespace: String::new(),
                open: false,
            })
        );
    }


    #[test]
    fn reads_every_open_namespace_out_of_a_layers_reply() {
        let open = parse_layer_namespaces(CAPTURED_LAYERS_JSON);
        assert_eq!(
            open.iter().map(String::as_str).collect::<Vec<_>>(),
            ["hyprpad-cheatsheet", "omarchy-background", "omarchy-bar"],
            "every surface in the reply, whatever level it sits on"
        );
        assert!(open.contains("hyprpad-cheatsheet"), "the seed a mode rule asks about");

        // The same session with the sheet closed: the third level is empty, so
        // the sheet is simply absent.
        let closed = CAPTURED_LAYERS_JSON.replace(
            r#"                {
                    "address": "0x55d89c436500",
                    "x": 0,
                    "y": 0,
                    "w": 2048,
                    "h": 1280,
                    "alpha": 1,
                    "depth": 0.800,
                    "namespace": "hyprpad-cheatsheet",
                    "pid": 3229091
                }
"#,
            "",
        );
        assert!(!parse_layer_namespaces(&closed).contains("hyprpad-cheatsheet"));
        assert_eq!(parse_layer_namespaces(&closed).len(), 2);

        // A monitorless session, and a nameless surface, seed nothing.
        assert!(parse_layer_namespaces("{}").is_empty());
        assert!(parse_layer_namespaces(r#"{"namespace": ""}"#).is_empty());
    }

    #[test]
    fn unknown_event_is_other() {
        assert_eq!(
            parse_event("movewindowv2>>55d89bdb6a70,7,7"),
            Some(HyprEvent::Other {
                name: "movewindowv2".into(),
                data: "55d89bdb6a70,7,7".into(),
            })
        );
    }

    #[test]
    fn line_without_delimiter_is_none() {
        assert_eq!(parse_event("garbage line"), None);
    }

    #[test]
    fn empty_data_is_preserved() {
        assert_eq!(
            parse_event("workspace>>"),
            Some(HyprEvent::Workspace { name: String::new() })
        );
    }

    #[test]
    fn relative_workspace_argument() {
        assert_eq!(rel_arg(1), "e+1");
        assert_eq!(rel_arg(-1), "e-1");
        assert_eq!(rel_arg(0), "e+0");
        assert_eq!(rel_arg(3), "e+3");
    }

    /// The selector each target turns into on the wire. Two of these are the
    /// bug this module used to have: a `Number` went out as `name:N` and any
    /// other word as `name:<word>`, both of which allocate a hidden named
    /// workspace with a negative id when nothing by that name exists.
    #[test]
    fn workspace_targets_map_to_hyprland_selectors() {
        assert_eq!(workspace_arg(&WorkspaceTarget::Relative(1)), "e+1");
        assert_eq!(workspace_arg(&WorkspaceTarget::Relative(-2)), "e-2");
        assert_eq!(workspace_arg(&WorkspaceTarget::Number(3)), "3");
        for s in ["emptyn", "empty", "previous", "special:term", "name:foo", "e+1"] {
            assert_eq!(
                workspace_arg(&WorkspaceTarget::Selector(s.to_string())),
                s,
                "a selector is the compositor's to interpret"
            );
        }
    }

    #[test]
    fn focus_dispatch_expressions() {
        assert_eq!(
            focus_workspace_expr(&WorkspaceTarget::Number(3)),
            r#"hl.dsp.focus({ workspace = "3" })"#
        );
        assert_eq!(
            focus_workspace_expr(&WorkspaceTarget::Selector("emptyn".into())),
            r#"hl.dsp.focus({ workspace = "emptyn" })"#
        );
        assert_eq!(
            focus_workspace_expr(&WorkspaceTarget::Selector("name:foo".into())),
            r#"hl.dsp.focus({ workspace = "name:foo" })"#
        );
        assert_eq!(
            focus_workspace_expr(&WorkspaceTarget::Relative(1)),
            r#"hl.dsp.focus({ workspace = "e+1" })"#
        );
    }

    #[test]
    fn move_dispatch_expressions() {
        assert_eq!(
            move_window_to_workspace_expr(&WorkspaceTarget::Number(3)),
            r#"hl.dsp.window.move({ workspace = "3" })"#
        );
        assert_eq!(
            move_window_to_workspace_expr(&WorkspaceTarget::Selector("emptyn".into())),
            r#"hl.dsp.window.move({ workspace = "emptyn" })"#
        );
        assert_eq!(
            move_window_to_workspace_expr(&WorkspaceTarget::Selector("name:foo".into())),
            r#"hl.dsp.window.move({ workspace = "name:foo" })"#
        );
        assert_eq!(
            move_window_to_workspace_expr(&WorkspaceTarget::Relative(1)),
            r#"hl.dsp.window.move({ workspace = "e+1" })"#
        );
    }

    /// A selector reaches the compositor inside a Lua string literal, so a
    /// quote in a workspace name cannot break out of it.
    #[test]
    fn a_selector_cannot_escape_its_lua_string() {
        assert_eq!(
            focus_workspace_expr(&WorkspaceTarget::Selector(r#"name:a"b"#.into())),
            r#"hl.dsp.focus({ workspace = "name:a\"b" })"#
        );
    }

    #[test]
    fn address_normalization() {
        assert_eq!(norm_addr("55d89"), "0x55d89");
        assert_eq!(norm_addr("0x55d89"), "0x55d89");
        assert_eq!(norm_addr(""), "");
    }

    #[test]
    fn lua_escaping() {
        assert_eq!(lua_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(lua_escape("plain"), "plain");
    }
}
