//! `hyprpad broker` — the tiny root helper that hands the daemon descriptors.
//!
//! # The problem
//!
//! `/dev/uhid` is `crw------- root root`, and once
//! `packaging/udev/72-hyprpad-puck.rules` is installed the real puck's hidraw
//! nodes are `crw------- root root` too. The daemon must be able to open both;
//! Steam — running as the *same user* — must not be able to open the puck. No
//! group and no ACL can separate two processes with the same uid, so the
//! privilege has to live somewhere else.
//!
//! It lives here, in about as little code as the job admits.
//!
//! # The protocol
//!
//! One request per connection, then the connection closes. The client writes a
//! single line; the broker answers with one `sendmsg` carrying a short text
//! status line as the payload and the descriptors as `SCM_RIGHTS` ancillary
//! data.
//!
//! ```text
//! ->  "uhid\n"          <-  "ok 1\n"   + 1 fd   (/dev/uhid,  O_RDWR|O_CLOEXEC)
//! ->  "puck\n"          <-  "ok 5\n"   + 5 fds  (every 28de:1304 hidraw node)
//! ->  anything else     <-  "err unknown request\n"        + 0 fds
//! ```
//!
//! That is the whole vocabulary. The broker takes no path, no flags and no
//! numbers from the client: the *only* thing a client influences is which of two
//! hard-coded verbs runs. It never reads from, writes to or ioctls a device — it
//! opens and hands over, nothing else.
//!
//! `puck` **rescans `/sys/class/hidraw` on every request** and caches nothing, so
//! hotplug needs no special handling anywhere: the puck sleeping and coming back
//! on different node numbers is just a later request returning different fds.
//!
//! # Why passing fds works at all
//!
//! `UHID_CREATE2` — unlike the legacy `UHID_CREATE` — has no
//! `f_cred != current_cred()` check, so a `/dev/uhid` descriptor opened by root
//! and used by an unprivileged process is fine (see [`crate::uhid`]). hidraw has
//! no such check either, and no exclusive mode.
//!
//! # Who may ask
//!
//! Two gates, and the first is the one that normally matters:
//!
//! 1. **The socket's mode.** `hyprpad-broker.socket` creates
//!    `/run/hyprpad/broker.sock` as `0660 root:hyprpad`, so only members of the
//!    `hyprpad` group can connect at all. This is the default and the documented
//!    control.
//! 2. **`SO_PEERCRED`.** With `--uid N` (or `HYPRPAD_UID=N`) the broker
//!    additionally refuses any peer whose uid is neither `N` nor 0. Use it when
//!    more than one human shares the machine and the group is too coarse. uid 0
//!    is always allowed: root can open the nodes without asking.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::mem;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::hidraw;

/// The well-known socket. Must match `ListenStream=` in
/// `packaging/systemd/hyprpad-broker.socket`.
pub const DEFAULT_SOCKET: &str = "/run/hyprpad/broker.sock";

/// Overrides [`DEFAULT_SOCKET`] on both sides. Exists for tests and for running
/// a broker by hand; the shipped units never set it.
pub const SOCKET_ENV: &str = "HYPRPAD_BROKER_SOCKET";

/// Restricts the broker to one peer uid. See the module docs' second gate.
pub const UID_ENV: &str = "HYPRPAD_UID";

/// Longest request line the broker will read. A verb is four bytes; anything
/// approaching this is a client that has lost its mind, or is not a client.
const MAX_REQUEST: usize = 32;

/// Ceiling on descriptors in one reply. The puck has five interfaces today; the
/// cap exists so a malfunctioning `/sys` cannot make either side allocate an
/// unbounded control buffer.
pub const MAX_FDS: usize = 16;

/// How long either side waits on a single exchange. Every request is an open or
/// two, so this is generous by three orders of magnitude; it is here so a wedged
/// peer cannot hold the single-threaded broker (or block the daemon's startup).
const IO_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// The wire vocabulary
// ---------------------------------------------------------------------------

/// The two things a client may ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `/dev/uhid`, opened `O_RDWR | O_CLOEXEC`. Exactly one descriptor.
    Uhid,
    /// Every hidraw node belonging to a `28de:1304` puck, opened with
    /// [`hidraw::OPEN_FLAGS`]. One or more descriptors, freshly enumerated.
    Puck,
}

impl Request {
    /// The wire spelling, without the newline.
    pub fn as_str(self) -> &'static str {
        match self {
            Request::Uhid => "uhid",
            Request::Puck => "puck",
        }
    }

    /// The exact bytes a client sends, newline included.
    pub fn line(self) -> String {
        format!("{}\n", self.as_str())
    }
}

/// Parse one request line.
///
/// Deliberately strict — an allowlist of two exact words. A trailing `\n` (and a
/// `\r` before it, for a client that came through something line-oriented) is
/// the only slack. Leading or trailing spaces, a different case, extra
/// arguments, an embedded NUL, an empty line: all refused, with the reason that
/// goes back on the wire.
pub fn parse_request(raw: &[u8]) -> Result<Request, String> {
    if raw.len() > MAX_REQUEST {
        return Err("request too long".to_string());
    }
    let line = raw.strip_suffix(b"\n").unwrap_or(raw);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.is_empty() {
        return Err("empty request".to_string());
    }
    match line {
        b"uhid" => Ok(Request::Uhid),
        b"puck" => Ok(Request::Puck),
        _ => Err("unknown request".to_string()),
    }
}

/// The success status line: `ok <n>\n`, where `n` is how many descriptors ride
/// along in the same message.
pub fn ok_line(n: usize) -> String {
    format!("ok {n}\n")
}

/// The failure status line. `reason` is normalised to one line so a reply is
/// always exactly one.
pub fn err_line(reason: &str) -> String {
    let flat: String =
        reason.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    format!("err {}\n", flat.trim())
}

/// What a status line meant: `Ok(n)` for `ok <n>`, `Err(reason)` for `err …`.
pub fn parse_reply(raw: &[u8]) -> Result<usize, String> {
    let text = String::from_utf8_lossy(raw);
    let line = text.trim_end_matches(['\n', '\r']);
    if let Some(rest) = line.strip_prefix("ok ") {
        return rest.trim().parse::<usize>().map_err(|_| format!("malformed count in {line:?}"));
    }
    if let Some(rest) = line.strip_prefix("err ") {
        return Err(rest.trim().to_string());
    }
    Err(format!("malformed reply {line:?}"))
}

// ---------------------------------------------------------------------------
// SCM_RIGHTS, hand-rolled
// ---------------------------------------------------------------------------
//
// The `CMSG_*` macros are macros, so there is nothing to call from Rust without
// reimplementing them. These three are `include/linux/socket.h` verbatim; the
// tests below check every one against `libc`'s own implementations across the
// whole range of payload sizes a reply can have, so a wrong `sizeof` assumption
// fails `cargo test` rather than silently truncating a descriptor array.

/// `CMSG_ALIGN(len)` — round up to the natural alignment of a `cmsghdr`, which
/// on Linux is `sizeof(long)`.
const fn cmsg_align(len: usize) -> usize {
    let a = mem::size_of::<libc::c_long>();
    (len + a - 1) & !(a - 1)
}

/// `CMSG_LEN(len)` — what goes in `cmsg_len`: the header plus the payload, with
/// only the *header* aligned.
const fn cmsg_len(payload: usize) -> usize {
    cmsg_align(mem::size_of::<libc::cmsghdr>()) + payload
}

/// `CMSG_SPACE(len)` — how much control buffer one such cmsg occupies, with the
/// payload aligned too so a following cmsg would start correctly.
const fn cmsg_space(payload: usize) -> usize {
    cmsg_align(mem::size_of::<libc::cmsghdr>()) + cmsg_align(payload)
}

/// A control buffer with `cmsghdr` alignment.
///
/// A plain `Vec<u8>` is 1-aligned and the kernel reads a `struct cmsghdr`
/// straight out of it. Backing it with `u64` gives 8-byte alignment on every
/// Linux target hyprpad builds for.
struct ControlBuf(Vec<u64>);

impl ControlBuf {
    fn with_capacity(bytes: usize) -> ControlBuf {
        ControlBuf(vec![0u64; bytes.div_ceil(mem::size_of::<u64>()).max(1)])
    }
    fn as_mut_ptr(&mut self) -> *mut libc::c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// Send `payload` together with `fds` over `sock`, in one message.
///
/// The payload is never empty: a `sendmsg` with no data can legally transfer
/// nothing at all, taking the ancillary data with it.
pub fn send_with_fds(sock: &UnixStream, payload: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    assert!(!payload.is_empty(), "an SCM_RIGHTS message needs at least one payload byte");
    assert!(fds.len() <= MAX_FDS, "too many fds for one message");

    let raw: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();
    let fd_bytes = mem::size_of_val(raw.as_slice());
    let space = if raw.is_empty() { 0 } else { cmsg_space(fd_bytes) };
    let mut control = ControlBuf::with_capacity(space);

    let mut iov =
        libc::iovec { iov_base: payload.as_ptr() as *mut libc::c_void, iov_len: payload.len() };
    // SAFETY: `msghdr` is plain data; zeroing it is the documented way to start,
    // and every field that matters is set below.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if space > 0 {
        msg.msg_control = control.as_mut_ptr();
        msg.msg_controllen = space as _;
        // SAFETY: `msg_control` points at `space` writable, correctly aligned
        // bytes, and `space == cmsg_space(fd_bytes)` is by construction big
        // enough for one header plus `raw`.
        unsafe {
            let hdr = libc::CMSG_FIRSTHDR(&msg);
            assert!(!hdr.is_null(), "control buffer too small for one cmsghdr");
            (*hdr).cmsg_level = libc::SOL_SOCKET;
            (*hdr).cmsg_type = libc::SCM_RIGHTS;
            (*hdr).cmsg_len = cmsg_len(fd_bytes) as _;
            std::ptr::copy_nonoverlapping(
                raw.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(hdr),
                fd_bytes,
            );
        }
    }

    // SAFETY: `msg` describes buffers that outlive the call.
    let n = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if (n as usize) < payload.len() {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "short sendmsg"));
    }
    Ok(())
}

/// Receive one message: up to `buf.len()` payload bytes and up to `max_fds`
/// descriptors.
///
/// Returns `(payload_len, fds)`. `MSG_CMSG_CLOEXEC` is set, so a received
/// descriptor is never leaked into a child the daemon spawns (the OSK) — the
/// same property the direct opens get from `O_CLOEXEC`.
pub fn recv_with_fds(
    sock: &UnixStream,
    buf: &mut [u8],
    max_fds: usize,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let max_fds = max_fds.min(MAX_FDS);
    let space = cmsg_space(max_fds * mem::size_of::<RawFd>());
    let mut control = ControlBuf::with_capacity(space);

    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // SAFETY: as in `send_with_fds`.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr();
    msg.msg_controllen = space as _;

    // SAFETY: `msg` describes writable buffers that outlive the call.
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut fds = Vec::new();
    // SAFETY: `msg` was filled in by the kernel; walking it with the kernel's own
    // iteration helpers is the only correct way to read variable-length cmsgs.
    // Every descriptor is taken into an `OwnedFd` exactly once, so none leaks and
    // none is closed twice.
    unsafe {
        let mut hdr = libc::CMSG_FIRSTHDR(&msg);
        while !hdr.is_null() {
            if (*hdr).cmsg_level == libc::SOL_SOCKET && (*hdr).cmsg_type == libc::SCM_RIGHTS {
                let bytes = (*hdr).cmsg_len as usize - cmsg_len(0);
                let count = bytes / mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(hdr).cast::<RawFd>();
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            hdr = libc::CMSG_NXTHDR(&msg, hdr);
        }
    }
    // A truncated control message means descriptors the kernel dropped on the
    // floor; better to say so than to hand back a short set that looks complete.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::other("ancillary data truncated (too many descriptors)"));
    }
    Ok((n as usize, fds))
}

// ---------------------------------------------------------------------------
// Who may ask
// ---------------------------------------------------------------------------

/// Which peers the broker serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UidPolicy {
    /// Anyone who can connect. The socket's `0660 root:hyprpad` mode is the
    /// control; this is the default and what the shipped units use.
    #[default]
    SocketMode,
    /// Only this uid (and root). Set with `--uid N` or `HYPRPAD_UID=N`.
    Only(u32),
}

/// Whether a peer with `uid` may be served under `policy`.
///
/// root is always allowed: a root peer can open `/dev/uhid` and every hidraw node
/// directly, so refusing it would restrict nothing and only make debugging with
/// `sudo` confusing.
pub fn uid_allowed(policy: UidPolicy, uid: u32) -> bool {
    match policy {
        UidPolicy::SocketMode => true,
        UidPolicy::Only(_) if uid == 0 => true,
        UidPolicy::Only(allowed) => uid == allowed,
    }
}

/// The peer's uid, from `SO_PEERCRED`.
fn peer_uid(sock: &UnixStream) -> io::Result<u32> {
    // SAFETY: `ucred` is plain data; the kernel fills it in entirely.
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred`/`len` are a correctly sized, writable `ucred` and its length.
    let r = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Where the broker's socket is: [`SOCKET_ENV`] if set, else [`DEFAULT_SOCKET`].
pub fn socket_path() -> PathBuf {
    match std::env::var_os(SOCKET_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(DEFAULT_SOCKET),
    }
}

/// Ask the broker for `verb`'s descriptors.
///
/// Every failure — no socket, no permission to connect, a refusal, a short read —
/// comes back as an `Err`, and every caller in the daemon treats an `Err` the
/// same way: log once, fall back to opening by path.
pub fn request(verb: Request) -> io::Result<Vec<OwnedFd>> {
    request_at(&socket_path(), verb)
}

/// [`request`], against an explicit socket path.
pub fn request_at(path: &Path, verb: Request) -> io::Result<Vec<OwnedFd>> {
    let sock = UnixStream::connect(path)?;
    sock.set_read_timeout(Some(IO_TIMEOUT))?;
    sock.set_write_timeout(Some(IO_TIMEOUT))?;
    (&sock).write_all(verb.line().as_bytes())?;
    // Half-close so the broker's read sees EOF and never waits for more.
    sock.shutdown(std::net::Shutdown::Write)?;

    let mut buf = [0u8; 128];
    let (n, fds) = recv_with_fds(&sock, &mut buf, MAX_FDS)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "broker closed without answering",
        ));
    }
    match parse_reply(&buf[..n]) {
        Ok(count) if count == fds.len() => Ok(fds),
        Ok(count) => Err(io::Error::other(format!(
            "broker promised {count} descriptor(s) and sent {}",
            fds.len()
        ))),
        Err(reason) => Err(io::Error::other(format!("broker refused: {reason}"))),
    }
}

/// What the broker had to say, as the daemon sees it.
///
/// Exists so the fallback rule is a pure function over a three-valued input
/// rather than a chain of `if let` in the middle of the reconnect loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// No socket at [`socket_path`]: the broker is not installed. The ordinary
    /// state on a machine that has not run `hyprpad setup`.
    Absent,
    /// The socket was there and the exchange failed: refused, wrong uid, the
    /// broker mid-restart, a reply that did not parse.
    Failed,
    /// The broker handed over this many descriptors.
    Fds(usize),
}

/// Classify a `request` outcome into an [`Answer`], so the caller can apply
/// [`use_broker`] to it.
pub fn classify(path: &Path, outcome: &io::Result<Vec<OwnedFd>>) -> Answer {
    match outcome {
        Ok(fds) => Answer::Fds(fds.len()),
        Err(e) if e.kind() == io::ErrorKind::NotFound && !path.exists() => Answer::Absent,
        Err(_) => Answer::Failed,
    }
}

/// Which side of the fallback an [`Answer`] lands on.
///
/// The whole decision table, in one place:
///
/// | Answer | Source |
/// |---|---|
/// | broker absent | `Direct` |
/// | broker refused / failed | `Direct` |
/// | broker returned 0 descriptors | `Direct` — nothing usable, and the puck may simply be away |
/// | broker returned ≥1 descriptor | `Broker` |
pub fn use_broker(answer: Answer) -> bool {
    matches!(answer, Answer::Fds(n) if n > 0)
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

/// Parsed `hyprpad broker` arguments.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Options {
    /// `--socket PATH`. `None` means: use the socket systemd passed, or
    /// [`socket_path`] if there is none.
    pub socket: Option<PathBuf>,
    /// `--uid N` / [`UID_ENV`].
    pub uid: UidPolicy,
}

/// `hyprpad broker`'s usage, for the error path in `main`.
pub const USAGE: &str = "usage: hyprpad broker [--socket PATH] [--uid N]";

/// Parse `hyprpad broker`'s arguments (everything after the subcommand).
///
/// `env_uid` is [`UID_ENV`]'s value, threaded in rather than read here so the
/// precedence — flag beats environment — is testable.
pub fn parse_options(args: &[OsString], env_uid: Option<&str>) -> Result<Options, String> {
    let mut opts = Options::default();
    if let Some(v) = env_uid {
        let v = v.trim();
        if !v.is_empty() {
            let uid: u32 = v.parse().map_err(|_| format!("{UID_ENV}: not a uid: {v:?}"))?;
            opts.uid = UidPolicy::Only(uid);
        }
    }
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_string_lossy().as_ref() {
            "--socket" => {
                let p = it.next().ok_or_else(|| "--socket needs a path".to_string())?;
                opts.socket = Some(PathBuf::from(p));
            }
            "--uid" => {
                let v = it.next().ok_or_else(|| "--uid needs a number".to_string())?;
                let s = v.to_string_lossy();
                let uid: u32 =
                    s.trim().parse().map_err(|_| format!("--uid: not a uid: {s:?}"))?;
                opts.uid = UidPolicy::Only(uid);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(opts)
}

/// Where the broker would put its socket when it has to bind one itself:
/// `--socket`, else `$RUNTIME_DIRECTORY/broker.sock` (systemd's
/// `RuntimeDirectory=`), else [`DEFAULT_SOCKET`].
///
/// Pure, so the precedence is testable without an environment.
pub fn bind_path(
    flag: Option<&Path>,
    runtime_dir: Option<&std::ffi::OsStr>,
    env_socket: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(dir) = runtime_dir.filter(|d| !d.is_empty()) {
        // systemd allows a colon-separated list; the first entry is ours.
        let first = dir.to_string_lossy();
        let first = first.split(':').next().unwrap_or("");
        if !first.is_empty() {
            return PathBuf::from(first).join("broker.sock");
        }
    }
    match env_socket.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(DEFAULT_SOCKET),
    }
}

/// The listening socket systemd passed us, if we were socket-activated.
///
/// `sd_listen_fds` by hand: systemd sets `LISTEN_PID` to the service's pid and
/// `LISTEN_FDS` to how many descriptors start at fd 3. We accept exactly one.
fn inherited_listener() -> Option<UnixListener> {
    let pid: u32 = std::env::var("LISTEN_PID").ok()?.trim().parse().ok()?;
    if pid != std::process::id() {
        return None;
    }
    let count: u32 = std::env::var("LISTEN_FDS").ok()?.trim().parse().ok()?;
    if count != 1 {
        eprintln!("hyprpad broker: LISTEN_FDS={count}, expected exactly 1; ignoring");
        return None;
    }
    const SD_LISTEN_FDS_START: RawFd = 3;
    // SAFETY: fd 3 is ours by the LISTEN_PID check above, systemd guarantees it
    // is a listening socket of the type the unit declared, and nothing else in
    // the process has claimed it — this runs before any other descriptor work.
    let fd = unsafe { OwnedFd::from_raw_fd(SD_LISTEN_FDS_START) };
    let listener = UnixListener::from(fd);
    // **systemd creates its listening sockets `SOCK_NONBLOCK`.** Inherited as-is,
    // `incoming()` would return `EAGAIN` immediately and spin at full tilt. The
    // broker wants exactly the opposite: block until someone asks.
    if let Err(e) = listener.set_nonblocking(false) {
        eprintln!("hyprpad broker: could not make the inherited socket blocking: {e}");
    }
    Some(listener)
}

/// How many consecutive `accept` failures the broker tolerates before giving up.
///
/// A single failure is a blip (EMFILE under pressure, a peer that vanished
/// mid-handshake) and must not take the broker down; an unbroken run of them is
/// a listener that will never work again, and spinning on it forever is worse
/// than exiting and letting systemd restart us on the next connection.
const ACCEPT_FAILURE_LIMIT: u32 = 16;

/// Entry point for `hyprpad broker`. Runs until it is killed or the listener
/// breaks; every per-connection failure is logged and dropped.
pub fn run(opts: &Options) -> io::Result<()> {
    let (listener, how) = match inherited_listener() {
        Some(l) => (l, "socket-activated (LISTEN_FDS)".to_string()),
        None => {
            let path = bind_path(
                opts.socket.as_deref(),
                std::env::var_os("RUNTIME_DIRECTORY").as_deref(),
                std::env::var_os(SOCKET_ENV).as_deref(),
            );
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // A leftover socket from a killed broker would make `bind` fail with
            // EADDRINUSE forever. Only ever unlink a socket, never anything else.
            if let Ok(md) = std::fs::symlink_metadata(&path) {
                use std::os::unix::fs::FileTypeExt;
                if md.file_type().is_socket() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            let l = UnixListener::bind(&path)?;
            (l, format!("listening on {}", path.display()))
        }
    };
    match opts.uid {
        UidPolicy::SocketMode => {
            eprintln!("hyprpad broker: {how}; peers gated by the socket's mode/group")
        }
        UidPolicy::Only(uid) => eprintln!("hyprpad broker: {how}; serving uid {uid} (and root)"),
    }

    let mut failures = 0u32;
    for conn in listener.incoming() {
        match conn {
            Ok(sock) => {
                failures = 0;
                serve(&sock, opts.uid);
            }
            Err(e) => {
                failures += 1;
                eprintln!("hyprpad broker: accept: {e}");
                if failures >= ACCEPT_FAILURE_LIMIT {
                    return Err(io::Error::other(format!(
                        "giving up after {failures} consecutive accept failures ({e})"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Handle exactly one connection: check the peer, read one verb, answer, close.
fn serve(sock: &UnixStream, policy: UidPolicy) {
    let _ = sock.set_read_timeout(Some(IO_TIMEOUT));
    let _ = sock.set_write_timeout(Some(IO_TIMEOUT));

    let uid = match peer_uid(sock) {
        Ok(uid) => uid,
        Err(e) => {
            eprintln!("hyprpad broker: SO_PEERCRED: {e}; refusing");
            let _ = refuse(sock, "cannot identify peer");
            return;
        }
    };
    if !uid_allowed(policy, uid) {
        eprintln!("hyprpad broker: refusing uid {uid}");
        let _ = refuse(sock, "not permitted");
        return;
    }

    let mut raw = [0u8; MAX_REQUEST + 1];
    let mut used = 0usize;
    // Read until a newline, EOF or the cap. `read` can return a partial line on
    // a stream socket, so this cannot be a single call.
    let parsed = loop {
        match (&mut &*sock).read(&mut raw[used..]) {
            Ok(0) => break parse_request(&raw[..used]),
            Ok(n) => {
                used += n;
                if raw[..used].contains(&b'\n') || used >= raw.len() {
                    break parse_request(&raw[..used]);
                }
            }
            Err(e) => {
                eprintln!("hyprpad broker: read from uid {uid}: {e}");
                return;
            }
        }
    };

    let verb = match parsed {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("hyprpad broker: uid {uid}: {reason}");
            let _ = refuse(sock, &reason);
            return;
        }
    };

    match open_for(verb) {
        Ok(fds) => {
            let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
            let line = ok_line(fds.len());
            match send_with_fds(sock, line.as_bytes(), &borrowed) {
                Ok(()) => eprintln!(
                    "hyprpad broker: uid {uid}: {} -> {} descriptor(s)",
                    verb.as_str(),
                    fds.len()
                ),
                Err(e) => eprintln!("hyprpad broker: uid {uid}: sendmsg: {e}"),
            }
        }
        Err(e) => {
            eprintln!("hyprpad broker: uid {uid}: {}: {e}", verb.as_str());
            let _ = refuse(sock, &e.to_string());
        }
    }
}

/// Answer a request we will not serve. Always a well-formed reply with zero
/// descriptors, never a silent close: the client should learn *why*.
fn refuse(sock: &UnixStream, reason: &str) -> io::Result<()> {
    let line = err_line(reason);
    send_with_fds(sock, line.as_bytes(), &[])
}

/// Do the one privileged thing: open what `verb` names.
///
/// The only two things in the program the broker will ever open, and neither
/// comes from the client.
fn open_for(verb: Request) -> io::Result<Vec<OwnedFd>> {
    match verb {
        Request::Uhid => Ok(vec![open_uhid()?]),
        Request::Puck => {
            let nodes = hidraw::puck_nodes()?;
            if nodes.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no Steam Controller puck present (28de:1304)",
                ));
            }
            let mut fds = Vec::new();
            let mut errors = Vec::new();
            for node in nodes.iter().take(MAX_FDS) {
                match hidraw::open_node(node) {
                    Ok(fd) => fds.push(fd),
                    Err(e) => errors.push(format!("{}: {e}", node.display())),
                }
            }
            if fds.is_empty() {
                return Err(io::Error::other(format!(
                    "no puck node could be opened ({})",
                    errors.join("; ")
                )));
            }
            Ok(fds)
        }
    }
}

/// `/dev/uhid`, `O_RDWR | O_CLOEXEC`. The one node the relay needs.
///
/// Shared with [`crate::uhid::acquire_uhid`]'s fallback so the broker and the
/// direct path open it identically.
pub fn open_uhid() -> io::Result<OwnedFd> {
    let file = std::fs::OpenOptions::new().read(true).write(true).open("/dev/uhid")?;
    Ok(OwnedFd::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the vocabulary ----------------------------------------------------

    #[test]
    fn the_two_verbs_parse() {
        assert_eq!(parse_request(b"uhid\n"), Ok(Request::Uhid));
        assert_eq!(parse_request(b"puck\n"), Ok(Request::Puck));
        // A bare word with no newline is still a request: a client that shuts
        // down its write half instead of sending "\n" is well-behaved.
        assert_eq!(parse_request(b"uhid"), Ok(Request::Uhid));
        assert_eq!(parse_request(b"puck\r\n"), Ok(Request::Puck));
    }

    #[test]
    fn a_verb_round_trips_through_its_wire_form() {
        for verb in [Request::Uhid, Request::Puck] {
            assert_eq!(parse_request(verb.line().as_bytes()), Ok(verb));
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            &b""[..],
            b"\n",
            b"UHID\n",
            b" uhid\n",
            b"uhid \n",
            b"uhid puck\n",
            b"uhid\0\n",
            b"open /dev/hidraw7\n",
            b"../../etc/shadow\n",
            b"pucks\n",
            b"uhi\n",
        ] {
            assert!(
                parse_request(bad).is_err(),
                "must refuse {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn an_overlong_request_is_refused_without_looking_at_it() {
        let flood = vec![b'u'; MAX_REQUEST + 1];
        assert_eq!(parse_request(&flood), Err("request too long".to_string()));
        // Even one that starts with a valid verb.
        let mut sneaky = b"uhid".to_vec();
        sneaky.resize(MAX_REQUEST + 8, b'x');
        assert!(parse_request(&sneaky).is_err());
    }

    #[test]
    fn status_lines_round_trip() {
        assert_eq!(ok_line(0), "ok 0\n");
        assert_eq!(ok_line(5), "ok 5\n");
        assert_eq!(parse_reply(b"ok 5\n"), Ok(5));
        assert_eq!(parse_reply(b"ok 0\n"), Ok(0));
        assert_eq!(parse_reply(b"err not permitted\n"), Err("not permitted".to_string()));
        assert!(parse_reply(b"yes\n").is_err());
        assert!(parse_reply(b"ok many\n").is_err());
        assert!(parse_reply(b"").is_err());
    }

    #[test]
    fn an_error_reason_is_flattened_to_one_line() {
        let line = err_line("open read-write:\nEACCES\r\n");
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
        assert_eq!(parse_reply(line.as_bytes()), Err("open read-write: EACCES".to_string()));
    }

    // --- the cmsg arithmetic ----------------------------------------------

    /// Our three `const fn`s against `libc`'s own `CMSG_*`, for every payload
    /// size a real reply can have plus the awkward ones around the alignment
    /// boundary. This is the test that catches a wrong `sizeof` assumption
    /// before it silently truncates a descriptor array.
    #[test]
    fn cmsg_arithmetic_matches_libc() {
        for n in 0..=MAX_FDS {
            let payload = n * mem::size_of::<RawFd>();
            // SAFETY: `CMSG_LEN`/`CMSG_SPACE` are pure arithmetic on a length.
            assert_eq!(cmsg_len(payload) as u32, unsafe { libc::CMSG_LEN(payload as u32) });
            // SAFETY: as above.
            assert_eq!(cmsg_space(payload) as u32, unsafe { libc::CMSG_SPACE(payload as u32) });
        }
        for payload in 0..64usize {
            // SAFETY: as above.
            assert_eq!(cmsg_len(payload) as u32, unsafe { libc::CMSG_LEN(payload as u32) });
            // SAFETY: as above.
            assert_eq!(cmsg_space(payload) as u32, unsafe { libc::CMSG_SPACE(payload as u32) });
        }
    }

    #[test]
    fn cmsg_align_rounds_up_to_a_long() {
        let a = mem::size_of::<libc::c_long>();
        assert_eq!(cmsg_align(0), 0);
        assert_eq!(cmsg_align(1), a);
        assert_eq!(cmsg_align(a), a);
        assert_eq!(cmsg_align(a + 1), 2 * a);
        // The header is already aligned on every target we build for, which is
        // what makes `cmsg_len(0) == cmsg_space(0)`.
        assert_eq!(cmsg_len(0), cmsg_space(0));
        assert_eq!(cmsg_len(0), mem::size_of::<libc::cmsghdr>());
    }

    #[test]
    fn a_control_buffer_is_cmsghdr_aligned() {
        let mut buf = ControlBuf::with_capacity(cmsg_space(4 * mem::size_of::<RawFd>()));
        let addr = buf.as_mut_ptr() as usize;
        assert_eq!(addr % mem::align_of::<libc::cmsghdr>(), 0);
        // Even a zero request allocates something, so `as_mut_ptr` is never
        // dangling.
        let mut empty = ControlBuf::with_capacity(0);
        assert!(!empty.as_mut_ptr().is_null());
    }

    // --- SCM_RIGHTS over a socketpair --------------------------------------

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().expect("socketpair")
    }

    /// The round trip that matters: a descriptor sent over a socket is a
    /// *working* descriptor on the other side. Uses a pipe, so it needs no
    /// privilege and no device.
    #[test]
    fn a_passed_descriptor_still_works() {
        let (a, b) = pair();
        let (read_end, write_end) = std::io::pipe().expect("pipe");

        send_with_fds(&a, b"ok 1\n", &[write_end.as_fd()]).expect("send");
        drop(write_end); // only the passed copy survives

        let mut buf = [0u8; 64];
        let (n, fds) = recv_with_fds(&b, &mut buf, MAX_FDS).expect("recv");
        assert_eq!(parse_reply(&buf[..n]), Ok(1));
        assert_eq!(fds.len(), 1);

        // Write through the received copy, read out of the original pipe.
        let mut received = std::fs::File::from(fds.into_iter().next().unwrap());
        received.write_all(b"hello from the broker").expect("write via passed fd");
        drop(received);

        let mut got = String::new();
        let mut read_end = read_end;
        read_end.read_to_string(&mut got).expect("read");
        assert_eq!(got, "hello from the broker");
    }

    /// Several descriptors in one message, each landing where it should: the
    /// `puck` reply's shape.
    #[test]
    fn several_descriptors_arrive_in_one_message() {
        let (a, b) = pair();
        let pipes: Vec<(std::io::PipeReader, std::io::PipeWriter)> =
            (0..5).map(|_| std::io::pipe().expect("pipe")).collect();
        let writers: Vec<BorrowedFd<'_>> = pipes.iter().map(|(_, w)| w.as_fd()).collect();

        send_with_fds(&a, ok_line(writers.len()).as_bytes(), &writers).expect("send");

        let mut buf = [0u8; 64];
        let (n, fds) = recv_with_fds(&b, &mut buf, MAX_FDS).expect("recv");
        assert_eq!(parse_reply(&buf[..n]), Ok(5));
        assert_eq!(fds.len(), 5);

        for (i, fd) in fds.into_iter().enumerate() {
            let mut w = std::fs::File::from(fd);
            write!(w, "{i}").unwrap();
        }
        for (i, (r, w)) in pipes.into_iter().enumerate() {
            drop(w);
            let mut got = String::new();
            let mut r = r;
            r.read_to_string(&mut got).unwrap();
            assert_eq!(got, i.to_string(), "descriptor {i} landed on the wrong pipe");
        }
    }

    /// A refusal is a real message with a real reason and no descriptors.
    #[test]
    fn a_refusal_carries_no_descriptors() {
        let (a, b) = pair();
        refuse(&a, "unknown request").expect("send");
        let mut buf = [0u8; 64];
        let (n, fds) = recv_with_fds(&b, &mut buf, MAX_FDS).expect("recv");
        assert!(fds.is_empty());
        assert_eq!(parse_reply(&buf[..n]), Err("unknown request".to_string()));
    }

    /// Asking for fewer descriptors than arrive must not quietly hand back a
    /// short set — the kernel sets `MSG_CTRUNC` and we turn that into an error.
    #[test]
    fn a_truncated_control_message_is_an_error_not_a_short_set() {
        let (a, b) = pair();
        let pipes: Vec<_> = (0..4).map(|_| std::io::pipe().expect("pipe")).collect();
        let writers: Vec<BorrowedFd<'_>> = pipes.iter().map(|(_, w)| w.as_fd()).collect();
        send_with_fds(&a, b"ok 4\n", &writers).expect("send");

        let mut buf = [0u8; 64];
        let err = recv_with_fds(&b, &mut buf, 1).expect_err("must not silently truncate");
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    // --- who may ask -------------------------------------------------------

    #[test]
    fn the_uid_gate_decision_table() {
        // Default: the socket's group is the whole control.
        assert!(uid_allowed(UidPolicy::SocketMode, 1000));
        assert!(uid_allowed(UidPolicy::SocketMode, 1001));
        assert!(uid_allowed(UidPolicy::SocketMode, 0));
        // Pinned: that uid, and root.
        assert!(uid_allowed(UidPolicy::Only(1000), 1000));
        assert!(uid_allowed(UidPolicy::Only(1000), 0));
        assert!(!uid_allowed(UidPolicy::Only(1000), 1001));
        assert!(!uid_allowed(UidPolicy::Only(1000), 65534));
    }

    #[test]
    fn peercred_reads_our_own_uid_over_a_socketpair() {
        let (a, _b) = pair();
        // SAFETY: `getuid` is always safe.
        assert_eq!(peer_uid(&a).unwrap(), unsafe { libc::getuid() });
    }

    // --- argument parsing --------------------------------------------------

    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn broker_options_default_to_the_socket_gate() {
        let o = parse_options(&args(&[]), None).unwrap();
        assert_eq!(o, Options { socket: None, uid: UidPolicy::SocketMode });
    }

    #[test]
    fn a_flag_beats_the_environment() {
        let o = parse_options(&args(&["--uid", "1000"]), Some("42")).unwrap();
        assert_eq!(o.uid, UidPolicy::Only(1000));
        let o = parse_options(&args(&[]), Some("42")).unwrap();
        assert_eq!(o.uid, UidPolicy::Only(42));
        // An empty HYPRPAD_UID is "unset", not "uid 0".
        let o = parse_options(&args(&[]), Some("  ")).unwrap();
        assert_eq!(o.uid, UidPolicy::SocketMode);
    }

    #[test]
    fn bad_broker_options_are_errors_not_defaults() {
        assert!(parse_options(&args(&["--uid"]), None).is_err());
        assert!(parse_options(&args(&["--uid", "root"]), None).is_err());
        assert!(parse_options(&args(&["--socket"]), None).is_err());
        assert!(parse_options(&args(&["--help"]), None).is_err());
        assert!(parse_options(&args(&["/tmp/x"]), None).is_err());
        assert!(parse_options(&args(&[]), Some("root")).is_err());
    }

    #[test]
    fn socket_override_is_taken_verbatim() {
        let o = parse_options(&args(&["--socket", "/run/other.sock"]), None).unwrap();
        assert_eq!(o.socket.as_deref(), Some(Path::new("/run/other.sock")));
    }

    /// The bind path's precedence: flag, then systemd's `RUNTIME_DIRECTORY`,
    /// then the environment override, then the compiled-in default.
    #[test]
    fn the_bind_path_precedence() {
        use std::ffi::OsStr;
        assert_eq!(bind_path(None, None, None), Path::new(DEFAULT_SOCKET));
        assert_eq!(
            bind_path(None, Some(OsStr::new("/run/hyprpad")), None),
            Path::new("/run/hyprpad/broker.sock")
        );
        // systemd may hand over a colon-separated list; the first is ours.
        assert_eq!(
            bind_path(None, Some(OsStr::new("/run/hyprpad:/run/other")), None),
            Path::new("/run/hyprpad/broker.sock")
        );
        assert_eq!(
            bind_path(None, None, Some(OsStr::new("/tmp/b.sock"))),
            Path::new("/tmp/b.sock")
        );
        // The flag wins over everything.
        assert_eq!(
            bind_path(
                Some(Path::new("/tmp/flag.sock")),
                Some(OsStr::new("/run/hyprpad")),
                Some(OsStr::new("/tmp/env.sock"))
            ),
            Path::new("/tmp/flag.sock")
        );
        // An empty RUNTIME_DIRECTORY is not a directory.
        assert_eq!(bind_path(None, Some(OsStr::new("")), None), Path::new(DEFAULT_SOCKET));
    }

    // --- the fallback decision table ---------------------------------------

    #[test]
    fn the_daemon_falls_back_unless_the_broker_actually_delivered() {
        assert!(!use_broker(Answer::Absent), "no broker installed -> direct");
        assert!(!use_broker(Answer::Failed), "broker refused -> direct");
        assert!(!use_broker(Answer::Fds(0)), "broker delivered nothing -> direct");
        assert!(use_broker(Answer::Fds(1)));
        assert!(use_broker(Answer::Fds(5)));
    }

    #[test]
    fn an_outcome_classifies_by_whether_the_socket_is_even_there() {
        let missing = std::env::temp_dir().join("hyprpad-no-such-broker.sock");
        let _ = std::fs::remove_file(&missing);
        let not_found: io::Result<Vec<OwnedFd>> =
            Err(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        assert_eq!(classify(&missing, &not_found), Answer::Absent);
        // The same error against a socket that *does* exist is a real failure —
        // "/dev/uhid is missing", say — not "no broker".
        let present = Path::new("/dev/null");
        assert_eq!(classify(present, &not_found), Answer::Failed);
        assert_eq!(classify(present, &Err(io::Error::other("refused"))), Answer::Failed);
        assert_eq!(classify(present, &Ok(Vec::new())), Answer::Fds(0));
    }

    // --- the whole exchange, in process ------------------------------------

    /// `serve` against a real socketpair, with a real request on the wire. The
    /// `uhid`/`puck` paths need root and a device, so this drives the two
    /// outcomes that do not: a refused verb and a refused uid.
    #[test]
    fn serve_refuses_an_unknown_verb_over_a_real_socket() {
        let (client, server) = pair();
        std::thread::spawn(move || serve(&server, UidPolicy::SocketMode));
        (&client).write_all(b"open /dev/hidraw7\n").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut buf = [0u8; 128];
        let (n, fds) = recv_with_fds(&client, &mut buf, MAX_FDS).unwrap();
        assert!(fds.is_empty(), "a refusal must hand over nothing");
        assert_eq!(parse_reply(&buf[..n]), Err("unknown request".to_string()));
    }

    #[test]
    fn serve_refuses_a_peer_the_policy_excludes() {
        let (client, server) = pair();
        // SAFETY: `getuid` is always safe.
        let me = unsafe { libc::getuid() };
        let not_me = if me == 0 { 1 } else { me + 1 };
        std::thread::spawn(move || serve(&server, UidPolicy::Only(not_me)));
        // The verb is valid; the peer is not, so it is never even read.
        (&client).write_all(b"puck\n").unwrap();

        let mut buf = [0u8; 128];
        let (n, fds) = recv_with_fds(&client, &mut buf, MAX_FDS).unwrap();
        assert!(fds.is_empty());
        assert_eq!(parse_reply(&buf[..n]), Err("not permitted".to_string()));
    }

    /// A request split across two writes still parses: the read loop must not
    /// assume one `read` is one line.
    #[test]
    fn serve_reassembles_a_split_request() {
        let (client, server) = pair();
        std::thread::spawn(move || serve(&server, UidPolicy::SocketMode));
        (&client).write_all(b"pu").unwrap();
        (&client).write_all(b"ckle\n").unwrap(); // still bogus, but reassembled
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut buf = [0u8; 128];
        let (n, _) = recv_with_fds(&client, &mut buf, MAX_FDS).unwrap();
        // "puckle" — not "puck": proof both writes reached one parse.
        assert_eq!(parse_reply(&buf[..n]), Err("unknown request".to_string()));
    }

    #[test]
    fn request_at_a_missing_socket_is_an_ordinary_error() {
        let path = std::env::temp_dir().join("hyprpad-there-is-no-broker-here.sock");
        let _ = std::fs::remove_file(&path);
        let err = request_at(&path, Request::Uhid).expect_err("no socket");
        assert!(
            matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused),
            "{err:?}"
        );
        assert_eq!(classify(&path, &Err(err)), Answer::Absent);
    }

    /// The client half against a real listener: `request_at` must surface a
    /// refusal as an error rather than as an empty success.
    #[test]
    fn request_at_reports_a_refusal_as_an_error() {
        let dir = std::env::temp_dir().join(format!("hyprpad-broker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("refuse.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        // SAFETY: `getuid` is always safe.
        let me = unsafe { libc::getuid() };
        let not_me = if me == 0 { 1 } else { me + 1 };
        let handle = std::thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                serve(&sock, UidPolicy::Only(not_me));
            }
        });
        let err = request_at(&path, Request::Puck).expect_err("the policy excludes us");
        assert!(err.to_string().contains("not permitted"), "{err}");
        handle.join().unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn the_default_socket_matches_the_shipped_unit() {
        // packaging/systemd/hyprpad-broker.socket's ListenStream=.
        assert_eq!(DEFAULT_SOCKET, "/run/hyprpad/broker.sock");
        assert!(socket_path().is_absolute());
    }
}
