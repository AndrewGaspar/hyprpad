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
//!     reflow  (default) → set an exclusive zone so workspace content reflows
//!     overlay           → zero exclusive zone, for typing over a fullscreen game
//! hide                             # DESTROY the surface(s) — never just unmap
//! cursor <L|R> <nx> <ny>           # pad absolute position, each axis in [-1,1]
//! commit <L|R>                     # commit the key under that pad's cursor (click-down)
//! key <keycode>                    # commit a raw evdev keycode directly (test/daemon)
//! type <text…>                     # type an ASCII string (test helper)
//! quit                             # exit the process
//! ```
//!
//! Unknown or malformed lines are reported on the reply channel and ignored;
//! one bad line never desyncs the stream.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::layout::{LayoutMode, Pad};

/// A parsed control command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Create the surface(s) for `mode` and render. `reflow` chooses the
    /// exclusive-zone behaviour (see [`crate::surface`]).
    Show { mode: LayoutMode, reflow: bool },
    /// Destroy the surface(s) (osk-technology.md §2.3 — never hide).
    Hide,
    /// A pad's absolute normalized position, each axis in `[-1, 1]`.
    Cursor { pad: Pad, nx: f32, ny: f32 },
    /// Commit the key currently under `pad`'s cursor (trackpad click-down).
    Commit { pad: Pad },
    /// Commit a raw evdev keycode directly.
    Key { keycode: u16 },
    /// Type an ASCII string.
    Type { text: String },
    /// Exit the process.
    Quit,
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
            let reflow = match parts.next() {
                None | Some("reflow") => true,
                Some("overlay") => false,
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
        "key" => {
            let kc: u16 = rest.parse().map_err(|_| format!("bad keycode '{rest}'"))?;
            Ok(Command::Key { keycode: kc })
        }
        "type" => Ok(Command::Type { text: rest.to_string() }),
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

fn parse_f32(tok: Option<&str>, name: &str) -> Result<f32, String> {
    tok.ok_or_else(|| format!("missing {name}"))
        .and_then(|s| s.parse().map_err(|_| format!("bad {name} '{s}'")))
}

/// The transport the control channel listens on.
pub enum Channel {
    /// Read commands line-by-line from stdin.
    Stdin(BufReader<std::io::Stdin>),
    /// Accept one client at a time on a unix socket; read lines from it.
    Socket { listener: UnixListener, path: PathBuf, client: Option<BufReader<UnixStream>> },
}

impl Channel {
    /// Open a stdin channel.
    pub fn stdin() -> Channel {
        Channel::Stdin(BufReader::new(std::io::stdin()))
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
            Channel::Stdin(_) => vec![libc::STDIN_FILENO],
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
            Channel::Stdin(reader) => {
                // stdin is line-buffered here; read all currently-ready lines.
                // A single read_line may block only if the fd was reported
                // readable, which the caller guarantees before calling.
                let mut line = String::new();
                let n = reader.read_line(&mut line)?;
                if n == 0 {
                    return Ok(false); // EOF
                }
                dispatch_line(&line, &mut sink, &mut |e| eprintln!("hyprpad-osk: {e}"));
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
        if let Channel::Socket { path, .. } = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_show_variants() {
        assert_eq!(parse("show bottom"), Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: true }));
        assert_eq!(parse("show bottom overlay"), Ok(Command::Show { mode: LayoutMode::BottomDeck, reflow: false }));
        assert_eq!(parse("show split reflow"), Ok(Command::Show { mode: LayoutMode::SideSplit, reflow: true }));
        assert!(parse("show sideways").is_err());
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
    fn parses_key_type_hide_quit() {
        assert_eq!(parse("key 30"), Ok(Command::Key { keycode: 30 }));
        assert_eq!(parse("type hello world"), Ok(Command::Type { text: "hello world".into() }));
        assert_eq!(parse("hide"), Ok(Command::Hide));
        assert_eq!(parse("quit"), Ok(Command::Quit));
        assert!(parse("").is_err());
        assert!(parse("frobnicate").is_err());
    }
}
