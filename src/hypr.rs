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
        Ok(ActiveWindowInfo {
            pid: json_number(&json, "pid").map(|n| n as i32),
            fullscreen: json_number(&json, "fullscreen").is_some_and(|n| n != 0),
            title: json_string(&json, "title").unwrap_or_default(),
        })
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
    let rest = json[json.find(&needle)? + needle.len()..].trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
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
