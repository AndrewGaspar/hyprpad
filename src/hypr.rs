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
//!     | workspace, by name            | `hl.dsp.focus({ workspace = "name:foo" })`        |
//!     | move active window to ws      | `hl.dsp.window.move({ workspace = "e+1" })`       |
//!     | toggle fullscreen             | `hl.dsp.window.fullscreen({ mode = "fullscreen" })` |
//!     | focus a window by address     | `hl.dsp.focus({ window = "address:0x..." })`      |
//!
//!     Note the surprises: switching workspace is a `focus` verb (not a
//!     `workspace` one), and `hl.dsp.window.*` operations accept an optional
//!     `window = "address:0x..."` selector to target a specific window instead
//!     of the active one (proven by fullscreening a non-active window).
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

    /// Switch to the workspace `n` steps away on the active monitor
    /// (`+1` next, `-1` previous). Wraps `hl.dsp.focus({ workspace = "e±n" })`.
    pub fn workspace_relative(&self, n: i32) -> io::Result<()> {
        self.dispatch_ok(&format!(
            "hl.dsp.focus({{ workspace = \"{}\" }})",
            rel_arg(n)
        ))
    }

    /// Switch to a workspace by name. A bare `name` (no `:` selector) is taken
    /// as a named workspace and sent as `name:<name>`; an already-qualified
    /// selector such as `special:magic` is passed through unchanged.
    pub fn workspace_named(&self, name: &str) -> io::Result<()> {
        let selector = if name.contains(':') {
            name.to_string()
        } else {
            format!("name:{name}")
        };
        self.dispatch_ok(&format!(
            "hl.dsp.focus({{ workspace = \"{}\" }})",
            lua_escape(&selector)
        ))
    }

    /// Move the active window `n` workspaces away (it follows, as with the
    /// classic `movetoworkspace e±n`).
    pub fn move_window_to_workspace_relative(&self, n: i32) -> io::Result<()> {
        self.dispatch_ok(&format!(
            "hl.dsp.window.move({{ workspace = \"{}\" }})",
            rel_arg(n)
        ))
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

    /// Fire-and-forget exec of a shell command, detached from our stdio. A
    /// short-lived reaper thread waits on the child so it never lingers as a
    /// zombie; if the daemon exits first the child is simply reparented and
    /// keeps running. The command inherits our environment (so
    /// `WAYLAND_DISPLAY` etc. reach GUI children).
    pub fn spawn(&self, cmd: &str) {
        let cmd = cmd.to_string();
        thread::spawn(move || {
            let child = Command::new("/bin/sh")
                .arg("-c")
                .arg(&cmd)
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
    ActiveWindow { class: String, title: String },
    /// Keyboard focus moved, address form. `activewindowv2>>address`.
    ActiveWindowV2 { address: String },
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
            }
        }
        "activewindowv2" => HyprEvent::ActiveWindowV2 {
            address: norm_addr(data),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_window() {
        assert_eq!(
            parse_event("activewindow>>foot,ajg@framework:~/code/hyprsc"),
            Some(HyprEvent::ActiveWindow {
                class: "foot".into(),
                title: "ajg@framework:~/code/hyprsc".into(),
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
            parse_event("openwindow>>55d89d429360,7,foot,hyprsc-focus-probe"),
            Some(HyprEvent::OpenWindow {
                address: "0x55d89d429360".into(),
                workspace: "7".into(),
                class: "foot".into(),
                title: "hyprsc-focus-probe".into(),
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
