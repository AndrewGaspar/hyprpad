//! The control channel — how the hyprpad daemon drives the OSK.
//!
//! A line-based text protocol over either a unix socket at
//! `$XDG_RUNTIME_DIR/hyprpad-osk.sock` (default) or stdin (`--stdin`, handy for
//! the kickoff and for scripting the live test). The vocabulary is defined with
//! the **dual-trackpad future** in mind: the daemon will forward each pad's
//! absolute cursor and its click independently (osk-technology.md §4.1/§4.5),
//! so the wire protocol already speaks `cursor <L|R> …` and `commit <L|R>` even
//! though this kickoff only wires a subset of their behaviour.
//!
//! ## Grammar
//!
//! ```text
//! show <layout> [reflow|overlay]   # create the surface(s) and render
//!     layout ∈ { bottom, split }
//!     overlay (default) → zero exclusive zone, float over content
//!     reflow            → set an exclusive zone so workspace content reflows
//! hide                             # DESTROY the surface(s) — never just unmap
//! cursor <L|R> <nx> <ny>           # pad absolute position, each axis in [-1,1]
//! commit <L|R>                     # commit the key under that pad's cursor (click-down)
//! nav <left|right|up|down|home|end># move the snap-navigation highlight one key
//!                                  # (the padless modality; the first `nav` arms it)
//! shift <off|oneshot|stuck|on>     # set the latched shift/caps state (on = stuck/caps)
//! shift <down|up>                  # hold / release a physical Shift (momentary; the latch is untouched)
//! layer <base|symbols|toggle>      # switch the base QWERTY ↔ numeric/symbols page
//! reflow <on|off>                  # displace (on) vs overlay/float (off); recreates surfaces
//! key <keycode> [mod…]             # commit a raw evdev keycode directly (test/daemon),
//!                                  # optionally with modifier keycodes held around it
//! type <text…>                     # type an ASCII string (test helper)
//! candidate accept [n]             # accept the highlighted (or nth) suggestion — R1
//! candidate next|prev              # move the highlight along the strip — L1
//! context reset                    # forget the typed context (daemon sends it on focus change)
//! learn on|off                     # gate the personal word cache (deny-listed windows)
//! predict on|off                   # show / hide the suggestion strip entirely
//! forget                           # delete every learned word
//! quit                             # exit the process
//! ```
//!
//! Unknown or malformed lines are reported on the reply channel and ignored;
//! one bad line never desyncs the stream.

use std::io::{BufReader, Read};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::layout::{Layer, LayoutMode, NavDir, Pad, ShiftState};

/// A parsed control command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Create the surface(s) for `mode` and render. `reflow` chooses the
    /// exclusive-zone behaviour (see [`crate::surface`]); it defaults to `false`
    /// (overlay/float) when the `show` line does not say `reflow`.
    Show { mode: LayoutMode, reflow: bool },
    /// Destroy the surface(s) (osk-technology.md §2.3 — never hide).
    Hide,
    /// A pad's absolute normalized position, each axis in `[-1, 1]`.
    Cursor { pad: Pad, nx: f32, ny: f32 },
    /// Commit the key currently under `pad`'s cursor (trackpad click-down).
    Commit { pad: Pad },
    /// Move the snap-navigation highlight one key — the **padless** modality
    /// (`docs/research/xbox-elite.md` §3.5 option (ii)). A controller with no
    /// trackpads cannot point, so instead of a cursor it moves a single
    /// highlight from key to key, and `commit` takes whatever it is on. The
    /// first `nav` of a session arms the highlight without moving it, so the
    /// daemon can raise the keyboard and light up a starting key in one burst.
    Nav { dir: NavDir },
    /// Set the latched shift/caps state directly so the daemon can drive it.
    Shift { state: ShiftState },
    /// Hold (`down`) or release (`up`) a physical Shift: the daemon's held
    /// trigger. Momentary — forces the shifted level while down and leaves the
    /// latched state alone ([`crate::layout::ShiftModel`]).
    ShiftHeld { down: bool },
    /// Switch the active key layer. `None` means toggle to the other layer.
    Layer { target: Option<Layer> },
    /// Switch the surface policy: `true` = displace (exclusive zone), `false` =
    /// overlay/float. Recreates the live surface(s) with the flipped policy.
    Reflow { on: bool },
    /// Commit a raw evdev keycode directly, with `mods` (themselves raw
    /// keycodes) held around it — pressed before it in the order given and
    /// released after it in reverse, so `key 14 29` is Ctrl+Backspace. An empty
    /// `mods` is the one-code form the wire has always had.
    Key { keycode: u16, mods: Vec<u16> },
    /// Type an ASCII string.
    Type { text: String },
    /// Act on the prediction strip (osk-prediction.md §7.2 — the daemon binds
    /// `R1` to `candidate accept` and `L1` to `candidate next`; the OSK never
    /// reads controller input itself).
    Candidate { what: CandidateCmd },
    /// Forget the typed context. The daemon sends this on every focus change
    /// while the keyboard is up: whatever we knew about the field stopped being
    /// true (§8.1, `compose`).
    ContextReset,
    /// Gate the personal word cache. The daemon sends `learn off` for password
    /// managers, polkit agents and terminals (§5.3 rule 2).
    Learn { on: bool },
    /// Show or hide the suggestion strip entirely.
    Predict { on: bool },
    /// Delete every learned word (§5.3 rule 7, "delete learned words").
    Forget,
    /// Exit the process.
    Quit,
}

/// What a `candidate` command asks of the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateCmd {
    /// Accept the highlighted candidate, or the `n`th one when given.
    Accept(Option<usize>),
    /// Move the highlight one slot along, wrapping.
    Next,
    /// Move the highlight one slot back, wrapping.
    Prev,
}

/// Parse one protocol line into a [`Command`], or an error string describing
/// why it was rejected. Pure and unit-testable — no I/O.
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let mut it = line.splitn(2, char::is_whitespace);
    let verb = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("").trim();
    match verb {
        "show" => {
            let mut parts = rest.split_whitespace();
            let mode = match parts.next() {
                Some("bottom") => LayoutMode::BottomDeck,
                Some("split") => LayoutMode::SideSplit,
                Some(other) => return Err(format!("unknown layout '{other}' (want bottom|split)")),
                None => return Err("show needs a layout: bottom|split".into()),
            };
            // Overlay/float is the default now; `reflow` is the opt-in.
            let reflow = match parts.next() {
                None | Some("overlay") => false,
                Some("reflow") => true,
                Some(other) => return Err(format!("unknown presentation '{other}' (want reflow|overlay)")),
            };
            Ok(Command::Show { mode, reflow })
        }
        "hide" => Ok(Command::Hide),
        "quit" | "exit" => Ok(Command::Quit),
        "cursor" => {
            let mut p = rest.split_whitespace();
            let pad = parse_pad(p.next())?;
            let nx = parse_f32(p.next(), "nx")?;
            let ny = parse_f32(p.next(), "ny")?;
            Ok(Command::Cursor { pad, nx, ny })
        }
        "commit" => Ok(Command::Commit { pad: parse_pad(rest.split_whitespace().next())? }),
        // `focus` is the spelling `docs/research/xbox-elite.md` §3.5 sketched
        // this verb under; both are accepted so neither doc reads stale.
        "nav" | "focus" => Ok(Command::Nav { dir: parse_nav(rest.split_whitespace().next())? }),
        "shift" => {
            let state = match rest.split_whitespace().next() {
                Some("off") | None => ShiftState::Off,
                Some("oneshot") | Some("one") => ShiftState::OneShot,
                // `on`/`caps`/`stuck` all mean the locked level.
                Some("stuck") | Some("on") | Some("caps") => ShiftState::Stuck,
                // The physical, momentary shift is a separate bit from the latch.
                Some("down") | Some("hold") => return Ok(Command::ShiftHeld { down: true }),
                Some("up") | Some("release") => return Ok(Command::ShiftHeld { down: false }),
                Some(other) => {
                    return Err(format!("bad shift '{other}' (want off|oneshot|stuck|on, or down|up)"))
                }
            };
            Ok(Command::Shift { state })
        }
        "layer" => {
            let target = match rest.split_whitespace().next() {
                Some("base") | Some("abc") | Some("qwerty") => Some(Layer::Base),
                Some("symbols") | Some("sym") | Some("123") | Some("?123") => Some(Layer::Symbols),
                Some("toggle") | None => None,
                Some(other) => return Err(format!("bad layer '{other}' (want base|symbols|toggle)")),
            };
            Ok(Command::Layer { target })
        }
        "reflow" | "display" => {
            let on = match rest.split_whitespace().next() {
                Some("on") | Some("displace") => true,
                Some("off") | Some("overlay") | Some("float") => false,
                Some(other) => return Err(format!("bad reflow '{other}' (want on|off / displace|overlay)")),
                None => return Err("reflow needs on|off".into()),
            };
            Ok(Command::Reflow { on })
        }
        "key" => {
            let mut it = rest.split_whitespace();
            let head = it.next().unwrap_or("");
            let keycode: u16 = head.parse().map_err(|_| format!("bad keycode '{head}'"))?;
            let mut mods = Vec::new();
            for tok in it {
                mods.push(tok.parse::<u16>().map_err(|_| format!("bad modifier '{tok}'"))?);
            }
            Ok(Command::Key { keycode, mods })
        }
        "type" => Ok(Command::Type { text: rest.to_string() }),
        "candidate" | "suggest" => {
            let mut p = rest.split_whitespace();
            match p.next() {
                Some("accept") | Some("commit") | None => {
                    let n = match p.next() {
                        None => None,
                        Some(tok) => Some(
                            tok.parse::<usize>()
                                .map_err(|_| format!("bad candidate index '{tok}'"))?,
                        ),
                    };
                    Ok(Command::Candidate { what: CandidateCmd::Accept(n) })
                }
                Some("next") => Ok(Command::Candidate { what: CandidateCmd::Next }),
                Some("prev") | Some("previous") => {
                    Ok(Command::Candidate { what: CandidateCmd::Prev })
                }
                Some(other) => {
                    Err(format!("bad candidate action '{other}' (want accept [n]|next|prev)"))
                }
            }
        }
        "context" => match rest.split_whitespace().next() {
            Some("reset") | Some("clear") | None => Ok(Command::ContextReset),
            Some(other) => Err(format!("bad context action '{other}' (want reset)")),
        },
        "learn" => Ok(Command::Learn { on: parse_on_off(rest, "learn")? }),
        "predict" => Ok(Command::Predict { on: parse_on_off(rest, "predict")? }),
        "forget" => Ok(Command::Forget),
        "" => Err("empty line".into()),
        other => Err(format!("unknown command '{other}'")),
    }
}

fn parse_pad(tok: Option<&str>) -> Result<Pad, String> {
    match tok {
        Some("L") | Some("l") | Some("left") => Ok(Pad::Left),
        Some("R") | Some("r") | Some("right") => Ok(Pad::Right),
        Some(other) => Err(format!("bad pad '{other}' (want L|R)")),
        None => Err("missing pad (L|R)".into()),
    }
}

fn parse_nav(tok: Option<&str>) -> Result<NavDir, String> {
    match tok {
        Some("left") | Some("l") | Some("west") => Ok(NavDir::Left),
        Some("right") | Some("r") | Some("east") => Ok(NavDir::Right),
        Some("up") | Some("u") | Some("north") => Ok(NavDir::Up),
        Some("down") | Some("d") | Some("south") => Ok(NavDir::Down),
        Some("home") | Some("start") => Ok(NavDir::Home),
        Some("end") => Ok(NavDir::End),
        Some(other) => Err(format!("bad nav '{other}' (want left|right|up|down|home|end)")),
        None => Err("nav needs a direction: left|right|up|down|home|end".into()),
    }
}

/// `on|off` (with the usual synonyms) for the gate commands.
fn parse_on_off(rest: &str, verb: &str) -> Result<bool, String> {
    match rest.split_whitespace().next() {
        Some("on") | Some("yes") | Some("true") | Some("enable") => Ok(true),
        Some("off") | Some("no") | Some("false") | Some("disable") => Ok(false),
        Some(other) => Err(format!("bad {verb} '{other}' (want on|off)")),
        None => Err(format!("{verb} needs on|off")),
    }
}

fn parse_f32(tok: Option<&str>, name: &str) -> Result<f32, String> {
    tok.ok_or_else(|| format!("missing {name}"))
        .and_then(|s| s.parse().map_err(|_| format!("bad {name} '{s}'")))
}

/// The transport the control channel listens on.
pub enum Channel {
    /// Read commands from stdin (fd 0). The fd is put in non-blocking mode so a
    /// whole burst of lines is drained per poll wakeup; `leftover` carries a
    /// partial trailing line between reads.
    Stdin { leftover: String },
    /// Accept one client at a time on a unix socket; read lines from it.
    Socket { listener: UnixListener, path: PathBuf, client: Option<BufReader<UnixStream>> },
}

impl Channel {
    /// Open a stdin channel. stdin is switched to non-blocking so [`Self::drain`]
    /// can pull *every* buffered line each wakeup rather than one line per poll
    /// — the latter lags command bursts (e.g. two `cursor` lines in one write),
    /// which would leave the second pad's cursor a step behind.
    pub fn stdin() -> Channel {
        set_nonblocking(libc::STDIN_FILENO);
        Channel::Stdin { leftover: String::new() }
    }

    /// Bind the unix socket at `$XDG_RUNTIME_DIR/hyprpad-osk.sock`, removing any
    /// stale socket first.
    pub fn socket() -> Result<Channel, String> {
        let dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "XDG_RUNTIME_DIR unset".to_string())?;
        let path = PathBuf::from(dir).join("hyprpad-osk.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
        listener.set_nonblocking(true).map_err(|e| format!("set_nonblocking: {e}"))?;
        Ok(Channel::Socket { listener, path, client: None })
    }

    /// The set of raw fds a poll loop should watch for readability. For the
    /// socket channel this is the listener plus the current client, if any.
    pub fn poll_fds(&self) -> Vec<RawFd> {
        match self {
            Channel::Stdin { .. } => vec![libc::STDIN_FILENO],
            Channel::Socket { listener, client, .. } => {
                let mut v = vec![listener.as_raw_fd()];
                if let Some(c) = client {
                    v.push(c.get_ref().as_raw_fd());
                }
                v
            }
        }
    }

    /// Read whatever commands are currently available without blocking,
    /// invoking `sink` for each parsed [`Command`]. Returns `Ok(false)` when the
    /// input has reached EOF (stdin closed) and the loop should stop.
    ///
    /// Parse errors are written back to the client (socket) or stderr (stdin)
    /// and skipped.
    pub fn drain(&mut self, mut sink: impl FnMut(Command)) -> std::io::Result<bool> {
        match self {
            Channel::Stdin { leftover } => {
                // Drain every byte the kernel has for us (non-blocking), so a
                // multi-line burst is fully processed this wakeup instead of one
                // line per poll (extra lines would otherwise sit unseen in a
                // userspace buffer that `poll` cannot observe).
                let mut hung_up = false;
                let mut buf = [0u8; 4096];
                loop {
                    let n = unsafe {
                        libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                    };
                    if n < 0 {
                        let e = std::io::Error::last_os_error();
                        match e.kind() {
                            std::io::ErrorKind::WouldBlock => break,
                            std::io::ErrorKind::Interrupted => continue,
                            _ => return Err(e),
                        }
                    } else if n == 0 {
                        hung_up = true; // EOF
                        break;
                    } else {
                        leftover.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
                    }
                }
                // Dispatch each complete line; keep any partial remainder.
                while let Some(pos) = leftover.find('\n') {
                    let line: String = leftover.drain(..=pos).collect();
                    dispatch_line(&line, &mut sink, &mut |e| eprintln!("hyprpad-osk: {e}"));
                }
                if hung_up {
                    // Flush a final unterminated line, then signal EOF.
                    if !leftover.trim().is_empty() {
                        let last = std::mem::take(leftover);
                        dispatch_line(&last, &mut sink, &mut |e| eprintln!("hyprpad-osk: {e}"));
                    }
                    return Ok(false);
                }
                Ok(true)
            }
            Channel::Socket { listener, client, .. } => {
                // Accept a new client if one is waiting and we have no current one.
                if client.is_none() {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(true);
                            *client = Some(BufReader::new(stream));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => eprintln!("hyprpad-osk: accept: {e}"),
                    }
                }
                // Read whatever bytes are ready, then drop the reader borrow
                // before replying (so the error-reply closure can re-borrow the
                // client).
                let mut buf = Vec::new();
                let mut hung_up = false;
                if let Some(reader) = client.as_mut() {
                    let mut byte = [0u8; 1];
                    loop {
                        match reader.get_mut().read(&mut byte) {
                            Ok(0) => {
                                hung_up = true;
                                break;
                            }
                            Ok(_) => buf.push(byte[0]),
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                eprintln!("hyprpad-osk: socket read: {e}");
                                hung_up = true;
                                break;
                            }
                        }
                    }
                }
                if hung_up {
                    *client = None;
                }
                if !buf.is_empty() {
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    for line in text.lines() {
                        let mut on_err = |e: String| {
                            if let Some(c) = client.as_mut() {
                                use std::io::Write as _;
                                let _ = writeln!(c.get_mut(), "err: {e}");
                            }
                        };
                        dispatch_line(line, &mut sink, &mut on_err);
                    }
                }
                Ok(true)
            }
        }
    }
}

/// Put a raw fd into non-blocking mode (best-effort). Used for stdin so the
/// poll loop can drain it without blocking.
fn set_nonblocking(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// Clear non-blocking mode on `fd` (best-effort) — the inverse of
/// [`set_nonblocking`], used to leave stdin as we found it on exit.
fn clear_nonblocking(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

fn dispatch_line(line: &str, sink: &mut impl FnMut(Command), on_err: &mut impl FnMut(String)) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    match parse(line) {
        Ok(cmd) => sink(cmd),
        Err(e) => on_err(format!("bad command: {e}")),
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        match self {
            Channel::Socket { path, .. } => {
                let _ = std::fs::remove_file(path);
            }
            // Restore stdin to blocking so exiting doesn't leave a shared
            // terminal's fd 0 non-blocking (which would trip up the parent shell).
            Channel::Stdin { .. } => clear_nonblocking(libc::STDIN_FILENO),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_show_variants() {
        // Overlay/float is the default now; reflow is the explicit opt-in.
        assert_eq!(parse("show bottom"), Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: false }));
        assert_eq!(parse("show bottom overlay"), Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: false }));
        assert_eq!(parse("show bottom reflow"), Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: true }));
        assert_eq!(parse("show split reflow"), Ok(Command::Show { mode: LayoutMode::SideSplit, reflow: true }));
        assert!(parse("show sideways").is_err());
    }

    #[test]
    fn parses_shift_layer_reflow() {
        assert_eq!(parse("shift oneshot"), Ok(Command::Shift { state: ShiftState::OneShot }));
        assert_eq!(parse("shift on"), Ok(Command::Shift { state: ShiftState::Stuck }));
        assert_eq!(parse("shift off"), Ok(Command::Shift { state: ShiftState::Off }));
        assert!(parse("shift sideways").is_err());
        // The held shift is its own command, not a latch level.
        assert_eq!(parse("shift down"), Ok(Command::ShiftHeld { down: true }));
        assert_eq!(parse("shift up"), Ok(Command::ShiftHeld { down: false }));
        assert_eq!(parse("shift hold"), Ok(Command::ShiftHeld { down: true }));
        assert_eq!(parse("shift release"), Ok(Command::ShiftHeld { down: false }));
        assert_eq!(parse("layer symbols"), Ok(Command::Layer { target: Some(Layer::Symbols) }));
        assert_eq!(parse("layer base"), Ok(Command::Layer { target: Some(Layer::Base) }));
        assert_eq!(parse("layer toggle"), Ok(Command::Layer { target: None }));
        assert_eq!(parse("reflow on"), Ok(Command::Reflow { on: true }));
        assert_eq!(parse("reflow off"), Ok(Command::Reflow { on: false }));
        assert_eq!(parse("display overlay"), Ok(Command::Reflow { on: false }));
        assert!(parse("reflow maybe").is_err());
    }

    #[test]
    fn parses_dual_trackpad_vocab() {
        assert_eq!(parse("cursor L -0.5 0.25"), Ok(Command::Cursor { pad: Pad::Left, nx: -0.5, ny: 0.25 }));
        assert_eq!(parse("cursor R 1 -1"), Ok(Command::Cursor { pad: Pad::Right, nx: 1.0, ny: -1.0 }));
        assert_eq!(parse("commit L"), Ok(Command::Commit { pad: Pad::Left }));
        assert_eq!(parse("commit R"), Ok(Command::Commit { pad: Pad::Right }));
        assert!(parse("cursor X 0 0").is_err());
    }

    #[test]
    fn parses_the_padless_show_and_the_snap_navigation_vocabulary() {
        // The padless presentation: the FULL keyboard from the bottom edge,
        // with an exclusive zone so workspace content is displaced rather than
        // covered. This is the exact line the daemon sends for a controller
        // with no trackpads.
        assert_eq!(
            parse("show bottom reflow"),
            Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: true })
        );

        assert_eq!(parse("nav left"), Ok(Command::Nav { dir: NavDir::Left }));
        assert_eq!(parse("nav right"), Ok(Command::Nav { dir: NavDir::Right }));
        assert_eq!(parse("nav up"), Ok(Command::Nav { dir: NavDir::Up }));
        assert_eq!(parse("nav down"), Ok(Command::Nav { dir: NavDir::Down }));
        assert_eq!(parse("nav home"), Ok(Command::Nav { dir: NavDir::Home }));
        assert_eq!(parse("nav end"), Ok(Command::Nav { dir: NavDir::End }));
        // The research doc spelled this verb `focus`; both are accepted.
        assert_eq!(parse("focus up"), Ok(Command::Nav { dir: NavDir::Up }));
        assert!(parse("nav sideways").unwrap_err().contains("bad nav"));
        assert!(parse("nav").unwrap_err().contains("needs a direction"));
    }

    #[test]
    fn parses_key_type_hide_quit() {
        assert_eq!(parse("key 30"), Ok(Command::Key { keycode: 30, mods: vec![] }));
        // The extended form: modifiers follow the keycode, in press order.
        assert_eq!(
            parse("key 14 29"),
            Ok(Command::Key { keycode: 14, mods: vec![29] }),
            "ctrl+backspace"
        );
        assert_eq!(
            parse("key 15 29 42"),
            Ok(Command::Key { keycode: 15, mods: vec![29, 42] }),
            "ctrl+shift+tab"
        );
        // Extra whitespace is the splitter's problem, not the grammar's.
        assert_eq!(parse("key  14   29 "), Ok(Command::Key { keycode: 14, mods: vec![29] }));
        // A junk modifier is reported, not silently dropped: a keystroke that
        // half-lands is worse than one that does not.
        assert!(parse("key 14 nope").unwrap_err().contains("bad modifier"));
        assert!(parse("key nope").unwrap_err().contains("bad keycode"));
        assert!(parse("key").unwrap_err().contains("bad keycode"));
        assert_eq!(parse("type hello world"), Ok(Command::Type { text: "hello world".into() }));
        assert_eq!(parse("hide"), Ok(Command::Hide));
        assert_eq!(parse("quit"), Ok(Command::Quit));
        assert!(parse("").is_err());
        assert!(parse("frobnicate").is_err());
    }

    #[test]
    fn parses_the_prediction_vocabulary() {
        use CandidateCmd::*;
        assert_eq!(parse("candidate accept"), Ok(Command::Candidate { what: Accept(None) }));
        assert_eq!(parse("candidate accept 2"), Ok(Command::Candidate { what: Accept(Some(2)) }));
        assert_eq!(parse("candidate"), Ok(Command::Candidate { what: Accept(None) }));
        assert_eq!(parse("candidate next"), Ok(Command::Candidate { what: Next }));
        assert_eq!(parse("candidate prev"), Ok(Command::Candidate { what: Prev }));
        assert!(parse("candidate sideways").unwrap_err().contains("bad candidate action"));
        assert!(parse("candidate accept x").unwrap_err().contains("bad candidate index"));

        assert_eq!(parse("context reset"), Ok(Command::ContextReset));
        assert_eq!(parse("context"), Ok(Command::ContextReset));
        assert!(parse("context sideways").is_err());

        assert_eq!(parse("learn off"), Ok(Command::Learn { on: false }));
        assert_eq!(parse("learn on"), Ok(Command::Learn { on: true }));
        assert!(parse("learn").unwrap_err().contains("on|off"));
        assert!(parse("learn maybe").is_err());
        assert_eq!(parse("predict off"), Ok(Command::Predict { on: false }));
        assert_eq!(parse("predict on"), Ok(Command::Predict { on: true }));
        assert_eq!(parse("forget"), Ok(Command::Forget));
    }
}
