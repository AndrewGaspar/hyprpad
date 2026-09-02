//! A daemon-owned **virtual Valve controller** on `/dev/uhid`, so Steam sees a
//! real Steam Controller while hyprpad keeps owning the physical puck.
//!
//! # Why this exists
//!
//! `src/gamepad.rs` gives games a synthesized Xbox-360 pad. That works, but it
//! throws away everything Steam Input adds on a *Valve* controller: trackpads as
//! trackpads, gyro, per-game configs, Steam-driven haptics, back grips. A HID
//! device created through `/dev/uhid` with a Valve VID/PID is adopted by Steam
//! as the real thing — Valve ship this pattern themselves (SteamOS switches
//! InputPlumber to its `deck-uhid` target), and it is **proven on this machine**:
//! see the "PROVEN on-device (2026-09-01)" section of
//! `docs/research/uhid-steam-controller.md`, where Steam opened the fake, loaded
//! its `neptune` config, and sent 39 `SetSettingsValues` writes to it.
//!
//! # What this module is, and is not
//!
//! This is the **device-independent core**. It never opens a device node. Both
//! file descriptors it needs — the `/dev/uhid` one, and (later) the puck's — are
//! handed in as already-open [`OwnedFd`]s by the caller, so the privileged half
//! (how the daemon comes by a writable `/dev/uhid`, and how the real puck is
//! hidden from Steam) is a separate, swappable concern. [`acquire_uhid`] is the
//! one-line placeholder for it.
//!
//! # Layout of the module
//!
//! * this file — the `linux/uhid.h` UAPI: event encoding by hand, and
//!   [`UhidDevice`], which owns the fd and runs the read/reply loop on a thread;
//! * [`profile`] — the two identities hyprpad can present (`triton`, `deck`),
//!   as **data**: descriptor, VID/PID, canned `GET_REPORT` answers;
//! * [`translate`] — pure puck-frame → wire-report conversion for both profiles;
//! * [`settings`] — decoding what Steam writes *back* (`0x87 SetSettingsValues`,
//!   rumble, haptics) and deciding what hyprpad does about it;
//! * [`relay`] — the running thing: a 250 Hz stream and a write-interpretation
//!   loop, wired into `run.rs` as a second "game sink" beside `gamepad.rs`.
//!
//! # The UAPI, and the three rules that matter
//!
//! Sources, all cited where used: `include/uapi/linux/uhid.h` and
//! `drivers/hid/uhid.c` (Linux master), `Documentation/hid/uhid.rst`, and the
//! proven probe `scripts/research/uhid_active_probe.py`, whose exact bytes this
//! module reproduces.
//!
//! 1. **Never let a report request time out.** `uhid.c` waits `5 * HZ` for a
//!    `GET_REPORT`/`SET_REPORT` reply and then returns `-EIO` to the caller
//!    (i.e. to Steam's `ioctl`). Every request is answered on the same loop
//!    iteration it arrives on, always with a reply, never with nothing.
//! 2. **Drain reads every iteration.** The kernel's output queue is 32 events
//!    deep (`UHID_BUFSIZE`) and drops silently on overflow, so a slow reader
//!    shows up as five-second stalls inside Steam.
//! 3. **Read into a full-size event buffer.** `read()` returns exactly one
//!    event and *truncates* to the buffer, so short buffers silently lose data.
//!
//! `write()` never blocks and the kernel zero-extends short writes
//! (`uhid_char_write` does `memset` then `min(count, sizeof(input_buf))`), so
//! this module writes only the populated prefix of each event.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub mod profile;
pub mod relay;
pub mod settings;
pub mod translate;

pub use profile::{Identity, Profile};
pub use relay::SteamRelay;

// ---------------------------------------------------------------------------
// linux/uhid.h — event types, report types, and the struct offsets.
// ---------------------------------------------------------------------------

/// `enum uhid_event_type` (`include/uapi/linux/uhid.h`). Only the discriminants
/// this module uses are named; the numbering is the header's, verbatim.
pub mod ev {
    /// `UHID_DESTROY` — tear the device down (no payload).
    pub const DESTROY: u32 = 1;
    /// `UHID_START` — always the first event; carries `dev_flags`.
    pub const START: u32 = 2;
    /// `UHID_STOP` — the kernel is done with the device.
    pub const STOP: u32 = 3;
    /// `UHID_OPEN` — someone opened the hidraw node. This is Steam attaching.
    pub const OPEN: u32 = 4;
    /// `UHID_CLOSE` — the last reader went away.
    pub const CLOSE: u32 = 5;
    /// `UHID_OUTPUT` — the host wrote on the interrupt (output) channel.
    pub const OUTPUT: u32 = 6;
    /// `UHID_GET_REPORT` — control-channel GET_REPORT; must be answered.
    pub const GET_REPORT: u32 = 9;
    /// `UHID_GET_REPORT_REPLY` — the answer to one `GET_REPORT`.
    pub const GET_REPORT_REPLY: u32 = 10;
    /// `UHID_CREATE2` — create the device. Note this is *not* the legacy
    /// `UHID_CREATE` (0), which is the only path with an
    /// `f_cred != current_cred()` check — one more reason to use CREATE2, since
    /// hyprpad's fd may well have been opened by somebody else.
    pub const CREATE2: u32 = 11;
    /// `UHID_INPUT2` — one input report, device → host.
    pub const INPUT2: u32 = 12;
    /// `UHID_SET_REPORT` — control-channel SET_REPORT; must be answered.
    pub const SET_REPORT: u32 = 13;
    /// `UHID_SET_REPORT_REPLY` — the answer to one `SET_REPORT`.
    pub const SET_REPORT_REPLY: u32 = 14;
}

/// `enum uhid_report_type` — the `rtype` field of the report events.
pub mod rtype {
    /// `UHID_FEATURE_REPORT`.
    pub const FEATURE: u8 = 0;
    /// `UHID_OUTPUT_REPORT`.
    pub const OUTPUT: u8 = 1;
    /// `UHID_INPUT_REPORT`.
    pub const INPUT: u8 = 2;
}

/// `UHID_DATA_MAX` == `HID_MAX_DESCRIPTOR_SIZE` (`linux/uhid.h`).
pub const UHID_DATA_MAX: usize = 4096;

/// `sizeof(struct uhid_event)` on x86-64.
///
/// `struct uhid_event` is `__packed`, so the union sits at offset 4; the union's
/// largest member is `uhid_create2_req` at 4372 bytes, and `uhid_start_req` —
/// the one *unpacked* member, holding a `__u64` — gives the union 8-byte
/// alignment, rounding it to 4376. `4 + 4376 == 4380`. (On i386 the `__u64`
/// aligns to 4, the union is 4372, and the total is 4376; every field offset is
/// identical, only the trailing padding differs.) Reads must use a buffer at
/// least this large: `uhid_char_read` truncates to the caller's length.
pub const EVENT_SIZE: usize = 4380;

/// `BUS_USB` (`linux/input.h`).
///
/// **Not `BUS_VIRTUAL` (0x06).** SDL's vendored hidraw backend filters on bus
/// type and drops anything that is not USB/Bluetooth/I2C/SPI, so a `BUS_VIRTUAL`
/// device is never enumerated by SDL at all. InputPlumber and hhd both declare
/// `BUS_USB`, and the proven probe confirmed it on-device
/// (`docs/research/uhid-steam-controller.md`, "PROVEN on-device").
pub const BUS_USB: u16 = 0x03;

// Field offsets inside `struct uhid_event`, from `include/uapi/linux/uhid.h`.
// The event type is a `__u32` at 0 and the union `u` begins at 4; everything
// below is `4 + offsetof(member)`. Asserted in `layout_matches_linux_uhid_h`.

/// `u.create2.name` — `__u8 name[128]`.
const O_CREATE2_NAME: usize = 4;
/// `u.create2.phys` — `__u8 phys[64]`.
const O_CREATE2_PHYS: usize = 132;
/// `u.create2.uniq` — `__u8 uniq[64]`.
const O_CREATE2_UNIQ: usize = 196;
/// `u.create2.rd_size` — `__u16`.
const O_CREATE2_RD_SIZE: usize = 260;
/// `u.create2.bus` — `__u16`.
const O_CREATE2_BUS: usize = 262;
/// `u.create2.vendor` — `__u32`.
const O_CREATE2_VENDOR: usize = 264;
/// `u.create2.product` — `__u32`.
const O_CREATE2_PRODUCT: usize = 268;
/// `u.create2.version` — `__u32`.
const O_CREATE2_VERSION: usize = 272;
/// `u.create2.country` — `__u32`.
const O_CREATE2_COUNTRY: usize = 276;
/// `u.create2.rd_data` — `__u8 rd_data[HID_MAX_DESCRIPTOR_SIZE]`.
const O_CREATE2_RD_DATA: usize = 280;
/// Total populated length of a `UHID_CREATE2` event with a `len`-byte descriptor.
const fn create2_len(rd_len: usize) -> usize {
    O_CREATE2_RD_DATA + rd_len
}

/// `u.start.dev_flags` — `__u64`.
const O_START_FLAGS: usize = 4;

/// `u.input2.size` — `__u16`.
const O_INPUT2_SIZE: usize = 4;
/// `u.input2.data` — `__u8 data[UHID_DATA_MAX]`.
const O_INPUT2_DATA: usize = 6;

/// `u.output.data` — `__u8 data[UHID_DATA_MAX]`. Note the odd field order:
/// `data` comes *first* in `uhid_output_req`, with `size` and `rtype` after it.
const O_OUTPUT_DATA: usize = 4;
/// `u.output.size` — `__u16`, after the 4096-byte `data`.
const O_OUTPUT_SIZE: usize = 4100;
/// `u.output.rtype` — `__u8`.
const O_OUTPUT_RTYPE: usize = 4102;

/// `u.get_report.id` — `__u32`, the request id to echo back.
const O_GET_REPORT_ID: usize = 4;
/// `u.get_report.rnum` — `__u8`.
const O_GET_REPORT_RNUM: usize = 8;
/// `u.get_report.rtype` — `__u8`.
const O_GET_REPORT_RTYPE: usize = 9;

/// `u.get_report_reply.id` — `__u32`.
const O_GRR_ID: usize = 4;
/// `u.get_report_reply.err` — `__u16` (0 or an errno).
const O_GRR_ERR: usize = 8;
/// `u.get_report_reply.size` — `__u16`.
const O_GRR_SIZE: usize = 10;
/// `u.get_report_reply.data` — `__u8 data[UHID_DATA_MAX]`.
const O_GRR_DATA: usize = 12;

/// `u.set_report.id` — `__u32`.
const O_SET_REPORT_ID: usize = 4;
/// `u.set_report.rnum` — `__u8`. Not read: hidraw passes the whole buffer
/// through, so `data[0]` already carries the same report number. Kept so the
/// struct map above is complete and the layout test can check it.
#[allow(dead_code)]
const O_SET_REPORT_RNUM: usize = 8;
/// `u.set_report.rtype` — `__u8`.
const O_SET_REPORT_RTYPE: usize = 9;
/// `u.set_report.size` — `__u16`.
const O_SET_REPORT_SIZE: usize = 10;
/// `u.set_report.data` — `__u8 data[UHID_DATA_MAX]`.
const O_SET_REPORT_DATA: usize = 12;

/// `u.set_report_reply.id` — `__u32`.
const O_SRR_ID: usize = 4;
/// `u.set_report_reply.err` — `__u16`.
const O_SRR_ERR: usize = 8;
/// Total populated length of a `UHID_SET_REPORT_REPLY` event.
const SRR_LEN: usize = 10;

/// `UHID_DEV_NUMBERED_FEATURE_REPORTS` (`linux/uhid.h`).
///
/// These three are **kernel → user space only**. They live in
/// `struct uhid_start_req`, which is the payload of `UHID_START`;
/// `struct uhid_create2_req` has no flags field, and `uhid_hid_start` computes
/// them by walking the descriptor we published
/// (`hid->report_enum[HID_FEATURE_REPORT].numbered` and friends). So a profile
/// asks for numbered framing by *shipping a numbered descriptor*, and these
/// constants exist to check the answer — see `profile::Framing`.
pub const DEV_NUMBERED_FEATURE_REPORTS: u64 = 1;
/// `UHID_DEV_NUMBERED_OUTPUT_REPORTS`. Kernel → user space; see above.
pub const DEV_NUMBERED_OUTPUT_REPORTS: u64 = 2;
/// `UHID_DEV_NUMBERED_INPUT_REPORTS`. Kernel → user space; see above.
pub const DEV_NUMBERED_INPUT_REPORTS: u64 = 4;

// ---------------------------------------------------------------------------
// Event encoding
// ---------------------------------------------------------------------------

/// Build the `UHID_CREATE2` event for `profile`.
///
/// Byte-for-byte what `scripts/research/uhid_active_probe.py::ev_create2` writes
/// for the `deck` profile — the exact event Steam adopted on this machine. Only
/// the populated prefix is returned; the kernel zero-extends the rest.
///
/// Note what is *not* here: report-ID framing. `struct uhid_create2_req` ends at
/// `country` and `rd_data`, with no `dev_flags` and no per-channel switch. The
/// descriptor is the whole request — publish a numbered one and the kernel
/// numbers the device, then says so in `UHID_START`.
pub fn create2_event(profile: &Profile) -> Vec<u8> {
    let rd = profile.descriptor;
    let mut buf = vec![0u8; create2_len(rd.len())];
    buf[0..4].copy_from_slice(&ev::CREATE2.to_le_bytes());
    put_cstr(&mut buf[O_CREATE2_NAME..O_CREATE2_PHYS], profile.name);
    put_cstr(&mut buf[O_CREATE2_PHYS..O_CREATE2_UNIQ], profile.phys);
    put_cstr(&mut buf[O_CREATE2_UNIQ..O_CREATE2_RD_SIZE], profile.uniq);
    let rd_size = u16::try_from(rd.len()).unwrap_or(u16::MAX);
    buf[O_CREATE2_RD_SIZE..O_CREATE2_BUS].copy_from_slice(&rd_size.to_le_bytes());
    buf[O_CREATE2_BUS..O_CREATE2_VENDOR].copy_from_slice(&profile.bus.to_le_bytes());
    buf[O_CREATE2_VENDOR..O_CREATE2_PRODUCT].copy_from_slice(&profile.vendor.to_le_bytes());
    buf[O_CREATE2_PRODUCT..O_CREATE2_VERSION].copy_from_slice(&profile.product.to_le_bytes());
    buf[O_CREATE2_VERSION..O_CREATE2_COUNTRY].copy_from_slice(&profile.version.to_le_bytes());
    buf[O_CREATE2_COUNTRY..O_CREATE2_RD_DATA].copy_from_slice(&profile.country.to_le_bytes());
    buf[O_CREATE2_RD_DATA..].copy_from_slice(rd);
    buf
}

/// Copy `s` into a fixed-width, NUL-padded field. Truncated (on a char
/// boundary) rather than panicking if it does not fit — a long name is a
/// cosmetic problem, a panic in the daemon is not.
fn put_cstr(field: &mut [u8], s: &str) {
    let room = field.len().saturating_sub(1); // always leave the NUL
    let mut end = s.len().min(room);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    field[..end].copy_from_slice(&s.as_bytes()[..end]);
}

/// Build a `UHID_INPUT2` event carrying one input report.
pub fn input2_event(report: &[u8]) -> Vec<u8> {
    let n = report.len().min(UHID_DATA_MAX);
    let mut buf = vec![0u8; O_INPUT2_DATA + n];
    buf[0..4].copy_from_slice(&ev::INPUT2.to_le_bytes());
    buf[O_INPUT2_SIZE..O_INPUT2_DATA].copy_from_slice(&(n as u16).to_le_bytes());
    buf[O_INPUT2_DATA..].copy_from_slice(&report[..n]);
    buf
}

/// Build a `UHID_GET_REPORT_REPLY` for request `id`, with `err = 0`.
///
/// The kernel matches a reply on **both** the id and the event type, and ids are
/// never reused — so `id` is echoed, never generated.
pub fn get_report_reply_event(id: u32, data: &[u8]) -> Vec<u8> {
    let n = data.len().min(UHID_DATA_MAX);
    let mut buf = vec![0u8; O_GRR_DATA + n];
    buf[0..4].copy_from_slice(&ev::GET_REPORT_REPLY.to_le_bytes());
    buf[O_GRR_ID..O_GRR_ERR].copy_from_slice(&id.to_le_bytes());
    buf[O_GRR_ERR..O_GRR_SIZE].copy_from_slice(&0u16.to_le_bytes());
    buf[O_GRR_SIZE..O_GRR_DATA].copy_from_slice(&(n as u16).to_le_bytes());
    buf[O_GRR_DATA..].copy_from_slice(&data[..n]);
    buf
}

/// Build a `UHID_SET_REPORT_REPLY` for request `id`, with `err = 0`. A
/// set-report reply never carries data.
pub fn set_report_reply_event(id: u32) -> Vec<u8> {
    let mut buf = vec![0u8; SRR_LEN];
    buf[0..4].copy_from_slice(&ev::SET_REPORT_REPLY.to_le_bytes());
    buf[O_SRR_ID..O_SRR_ERR].copy_from_slice(&id.to_le_bytes());
    buf[O_SRR_ERR..SRR_LEN].copy_from_slice(&0u16.to_le_bytes());
    buf
}

/// Build a `UHID_DESTROY` event.
pub fn destroy_event() -> Vec<u8> {
    ev::DESTROY.to_le_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Event decoding
// ---------------------------------------------------------------------------

/// Which channel a host → device write arrived on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteChannel {
    /// `UHID_SET_REPORT` — the control channel (a `SET_FEATURE` ioctl, or a
    /// `SET_REPORT` on the control endpoint). Must be replied to.
    SetReport,
    /// `UHID_OUTPUT` — the interrupt out channel (a plain `write()` to the
    /// hidraw node). Fire-and-forget; there is nothing to reply to.
    Output,
}

/// One write Steam made to the virtual device, handed to the interpreter.
///
/// `data` is the buffer **exactly as hidraw passed it**, leading report-number
/// byte included: `hidraw_send_report` does not strip it, it takes
/// `report_number = buf[0]` and forwards the whole buffer unchanged. So for the
/// `triton` profile `data[0] == 0x01` (its feature report id) and for `deck`
/// `data[0] == 0x00` (an unnumbered device still carries hidapi's leading zero);
/// in both cases the Valve command id is `data[1]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostWrite {
    /// Which channel it came in on.
    pub channel: WriteChannel,
    /// The kernel's `rtype` (see [`rtype`]).
    pub rtype: u8,
    /// The report buffer, report-number byte first.
    pub data: Vec<u8>,
}

impl HostWrite {
    /// The Valve command frame: byte 0 is the command id, byte 1 its payload
    /// length. This is the shape `settings::classify` and `src/lizard.rs` speak.
    ///
    /// The leading byte is stripped for a `SET_REPORT` and kept for an `OUTPUT`,
    /// because the two channels frame differently:
    ///
    /// * a **feature** write is `[report_id][command][len][payload…]` — Valve
    ///   multiplexes the whole command set through one feature report (`0x01` on
    ///   the triton descriptor, an unnumbered `0x00` on the deck one), so the
    ///   report id says nothing and the command id is the *second* byte. This is
    ///   exactly the frame `src/lizard.rs::disable_lizard_settings_report`
    ///   builds, and what the proven probe read `data[1]` for.
    /// * an **output** write is `[report_id][payload…]` where the report id *is*
    ///   the command: `0x80` rumble and `0x81` pulse are distinct output reports
    ///   in the triton descriptor, and `src/haptics.rs` writes them that way.
    ///   (The deck descriptor declares no output reports at all, so this arm is
    ///   only ever reached on the triton profile.)
    pub fn command(&self) -> &[u8] {
        match self.channel {
            WriteChannel::SetReport => self.data.get(1..).unwrap_or(&[]),
            WriteChannel::Output => &self.data,
        }
    }

    /// Whether this arrived on the interrupt-out channel, where the command id
    /// and the report id are the same byte.
    pub fn is_output(&self) -> bool {
        self.channel == WriteChannel::Output
    }
}

/// One decoded event from the kernel, for the loop to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Decoded {
    /// `UHID_START` with its `dev_flags`.
    Start(u64),
    /// `UHID_OPEN` — a reader attached.
    Open,
    /// `UHID_CLOSE` — the last reader detached.
    Close,
    /// `UHID_STOP`.
    Stop,
    /// A `GET_REPORT` needing an answer: `(id, rnum, rtype)`.
    GetReport(u32, u8, u8),
    /// A `SET_REPORT` needing an ack, plus the payload: `(id, write)`.
    SetReport(u32, HostWrite),
    /// An `OUTPUT` write. Nothing to reply to.
    Output(HostWrite),
    /// Something this module does not handle, by numeric type.
    Other(u32),
}

/// Decode one event read from the uhid fd. `None` for a runt.
fn decode(ev: &[u8]) -> Option<Decoded> {
    let etype = u32::from_le_bytes(ev.get(0..4)?.try_into().ok()?);
    Some(match etype {
        ev::START => {
            let flags = ev
                .get(O_START_FLAGS..O_START_FLAGS + 8)
                .and_then(|b| b.try_into().ok())
                .map_or(0, u64::from_le_bytes);
            Decoded::Start(flags)
        }
        ev::OPEN => Decoded::Open,
        ev::CLOSE => Decoded::Close,
        ev::STOP => Decoded::Stop,
        ev::GET_REPORT => Decoded::GetReport(
            le_u32(ev, O_GET_REPORT_ID)?,
            *ev.get(O_GET_REPORT_RNUM)?,
            *ev.get(O_GET_REPORT_RTYPE)?,
        ),
        ev::SET_REPORT => {
            let id = le_u32(ev, O_SET_REPORT_ID)?;
            let rtype = *ev.get(O_SET_REPORT_RTYPE)?;
            let size = le_u16(ev, O_SET_REPORT_SIZE)? as usize;
            let end = O_SET_REPORT_DATA + size.min(UHID_DATA_MAX);
            let data = ev.get(O_SET_REPORT_DATA..end).unwrap_or(&[]).to_vec();
            Decoded::SetReport(id, HostWrite { channel: WriteChannel::SetReport, rtype, data })
        }
        ev::OUTPUT => {
            let rtype = *ev.get(O_OUTPUT_RTYPE)?;
            let size = le_u16(ev, O_OUTPUT_SIZE)? as usize;
            let end = O_OUTPUT_DATA + size.min(UHID_DATA_MAX);
            let data = ev.get(O_OUTPUT_DATA..end).unwrap_or(&[]).to_vec();
            Decoded::Output(HostWrite { channel: WriteChannel::Output, rtype, data })
        }
        other => Decoded::Other(other),
    })
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

/// How long the event loop parks in `poll()` between checks of the stop flag.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// State shared between the event-loop thread and the owner's handle.
struct Shared {
    fd: OwnedFd,
    /// Serialises `write()`s. One `write` is one event and `uhid_char_write`
    /// takes `uhid->devlock` anyway, so this is belt and braces — but it keeps
    /// the 250 Hz streamer and the reply path provably non-interleaving.
    write_lock: Mutex<()>,
    /// `dev_flags` from `UHID_START`, or `u64::MAX` while it has not arrived.
    /// The kernel reports report-ID framing **per report type** here; the docs
    /// are explicit that user space must adjust prefixes to match rather than
    /// hardcoding from its own reading of the descriptor.
    dev_flags: AtomicU64,
    /// Whether a reader currently holds the hidraw node open (Steam attached).
    opened: AtomicBool,
    /// Which canned answer the next `GET_REPORT` gets.
    ///
    /// The Valve control protocol is stateful in one small way: a `SET_REPORT`
    /// naming a *query* command selects what the following `GET_REPORT` reads
    /// back. InputPlumber models it with a `current_report` field and so do we
    /// — the state belongs to the live device, not to the (`'static`) profile.
    selector: AtomicU8,
    /// Set to stop the event loop.
    stop: AtomicBool,
}

impl Shared {
    /// Wrap an already-open uhid descriptor, forcing it non-blocking.
    ///
    /// **`O_NONBLOCK` is required, not a preference.** The event loop drains
    /// every available event per wakeup (the kernel's queue is 32 deep and drops
    /// silently); on a blocking fd the read that finds the queue empty would
    /// park forever with a `GET_REPORT` possibly already queued behind it, and
    /// Steam would see the 5 s timeout. The proven probe sets the same flag.
    fn new(fd: OwnedFd, initial_selector: u8) -> io::Result<Shared> {
        // SAFETY: `fd` is owned and open; both calls are plain fcntl queries.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above; adding O_NONBLOCK to the file status flags.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Shared {
            fd,
            write_lock: Mutex::new(()),
            dev_flags: AtomicU64::new(u64::MAX),
            opened: AtomicBool::new(false),
            selector: AtomicU8::new(initial_selector),
            stop: AtomicBool::new(false),
        })
    }

    /// Write one whole event. Never blocks (`uhid_char_write` cannot).
    fn write_event(&self, buf: &[u8]) -> io::Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `buf` is a valid initialised slice and `fd` is owned and open
        // for writing for the lifetime of `self`.
        let n = unsafe {
            libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast::<libc::c_void>(), buf.len())
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A live virtual HID device: the `/dev/uhid` fd, the device created on it, and
/// the thread servicing the kernel's requests.
///
/// Dropping it writes `UHID_DESTROY` and stops the thread; closing the fd would
/// destroy the device anyway, but the explicit event is what the docs prescribe.
pub struct UhidDevice {
    shared: Arc<Shared>,
    /// Writes Steam made, for the caller to interpret. Bounded and lossy on
    /// purpose: a wedged consumer must never make the event loop block, because
    /// a blocked loop means five-second stalls inside Steam.
    writes: mpsc::Receiver<HostWrite>,
    profile: &'static Profile,
}

/// Depth of the host-write queue. Steam's traffic is a handful of feature writes
/// at connect plus rumble; anything beyond this means the consumer is wedged and
/// dropping is the right answer.
const WRITE_QUEUE_DEPTH: usize = 64;

impl UhidDevice {
    /// Create the virtual device on an **already-open** `/dev/uhid` descriptor
    /// and start servicing it.
    ///
    /// The fd is injected rather than opened here: on an ordinary desktop
    /// `/dev/uhid` is `crw------- root root`, so obtaining it is the privileged
    /// half of this feature and deliberately somebody else's problem. See
    /// [`acquire_uhid`].
    pub fn create(fd: OwnedFd, profile: &'static Profile) -> io::Result<UhidDevice> {
        let shared = Arc::new(Shared::new(fd, profile.default_selector)?);
        shared.write_event(&create2_event(profile))?;
        let (tx, writes) = mpsc::sync_channel(WRITE_QUEUE_DEPTH);
        let loop_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("uhid-events".to_string())
            .spawn(move || event_loop(&loop_shared, profile, &tx))?;
        Ok(UhidDevice { shared, writes, profile })
    }

    /// Send one input report (`UHID_INPUT2`).
    ///
    /// The caller supplies the report **already framed for the profile**: with
    /// its report-id prefix for `triton` (whose descriptor numbers every report
    /// type) and without one for `deck` (whose 38-byte descriptor has no
    /// `REPORT_ID` items at all). [`translate`] produces exactly that.
    pub fn send_input(&self, report: &[u8]) -> io::Result<()> {
        self.shared.write_event(&input2_event(report))
    }

    /// A cloneable write-only handle, for the 250 Hz streamer thread.
    ///
    /// The device itself is deliberately **not** `Sync`: it owns the host-write
    /// receiver, which is single-consumer by construction so that exactly one
    /// place in the daemon interprets Steam's writes. Streaming needs none of
    /// that — only the fd — so it gets this instead.
    pub fn sender(&self) -> UhidSender {
        UhidSender { shared: Arc::clone(&self.shared) }
    }

    /// The profile this device presents.
    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    /// Whether a reader currently holds the device open — i.e. Steam has it.
    pub fn is_open(&self) -> bool {
        self.shared.opened.load(Ordering::Relaxed)
    }

    /// `dev_flags` as reported by `UHID_START`, or `None` before it arrives.
    /// `UHID_START` is delivered asynchronously (`hid_add_device` is deferred to
    /// a workqueue), so this is `None` for the first milliseconds of a device's
    /// life — which is exactly why the streamer must not wait for it.
    pub fn dev_flags(&self) -> Option<u64> {
        match self.shared.dev_flags.load(Ordering::Relaxed) {
            u64::MAX => None,
            flags => Some(flags),
        }
    }

    /// Take every host write received since the last call, oldest first.
    pub fn drain_writes(&self) -> Vec<HostWrite> {
        self.writes.try_iter().collect()
    }
}

impl Drop for UhidDevice {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self.shared.write_event(&destroy_event());
    }
}

/// A write-only handle to a created device, safe to hand to another thread.
///
/// After the owning [`UhidDevice`] is dropped the device is destroyed and every
/// `send_input` fails, which is how the streamer thread learns to stop.
#[derive(Clone)]
pub struct UhidSender {
    shared: Arc<Shared>,
}

impl UhidSender {
    /// Send one input report. See [`UhidDevice::send_input`].
    pub fn send_input(&self, report: &[u8]) -> io::Result<()> {
        self.shared.write_event(&input2_event(report))
    }
}

/// Service the kernel's requests until the stop flag is set or the fd ends.
///
/// The three rules from the module docs live here: every `GET_REPORT` and
/// `SET_REPORT` is answered before the next event is looked at, all available
/// events are drained per wakeup, and every read uses a full-size buffer.
fn event_loop(shared: &Arc<Shared>, profile: &'static Profile, tx: &mpsc::SyncSender<HostWrite>) {
    let mut buf = vec![0u8; EVENT_SIZE];
    while !shared.stop.load(Ordering::Relaxed) {
        if !poll_readable(shared.fd.as_raw_fd(), POLL_INTERVAL) {
            continue;
        }
        // Drain: the kernel's output queue is 32 deep and drops silently.
        loop {
            // SAFETY: `buf` is a valid, initialised, EVENT_SIZE-byte buffer and
            // `fd` is owned and open for reading.
            let n = unsafe {
                libc::read(
                    shared.fd.as_raw_fd(),
                    buf.as_mut_ptr().cast::<libc::c_void>(),
                    buf.len(),
                )
            };
            if n <= 0 {
                let err = io::Error::last_os_error();
                if n < 0 && err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                // EAGAIN on a non-blocking fd, or EOF: back to the poll.
                note_read_end(n, &err, shared);
                break;
            }
            let Some(event) = decode(&buf[..n as usize]) else { continue };
            handle(shared, profile, tx, event);
        }
    }
}

/// Distinguish "nothing more to read right now" from "the fd is gone". A real
/// error stops the loop; `EAGAIN`/`EWOULDBLOCK` just ends this drain.
fn note_read_end(n: isize, err: &io::Error, shared: &Arc<Shared>) {
    if n == 0 {
        shared.stop.store(true, Ordering::Relaxed); // EOF: the device is gone.
        return;
    }
    match err.kind() {
        io::ErrorKind::WouldBlock => {}
        _ => shared.stop.store(true, Ordering::Relaxed),
    }
}

/// Act on one decoded event.
fn handle(
    shared: &Arc<Shared>,
    profile: &'static Profile,
    tx: &mpsc::SyncSender<HostWrite>,
    event: Decoded,
) {
    match event {
        Decoded::Start(flags) => {
            shared.dev_flags.store(flags, Ordering::Relaxed);
            // The kernel derives these from the descriptor we published
            // (`uhid_hid_start` in `drivers/hid/uhid.c`), so a mismatch means
            // the profile's `Framing` and its descriptor disagree — and every
            // report on every channel is then framed wrong. Nothing can be
            // done about it from here, but going quiet would leave the owner
            // debugging Steam instead of debugging us.
            let want = profile.framing.expected_dev_flags;
            if flags != want {
                eprintln!(
                    "warning: the kernel reports dev_flags {flags:#x} for the {} profile, \
                     not the {want:#x} its descriptor implies; report framing may be wrong",
                    profile.identity.as_str()
                );
            }
        }
        Decoded::Open => shared.opened.store(true, Ordering::Relaxed),
        Decoded::Close => shared.opened.store(false, Ordering::Relaxed),
        // "You can usually ignore any UHID_STOP events safely."
        Decoded::Stop | Decoded::Other(_) => {}
        Decoded::GetReport(id, rnum, _rtype) => {
            // Never empty, and never late. A device that answers every
            // GET_REPORT with `size = 0` hands Steam an empty attributes /
            // serial / chip-id blob; the reference implementation never does
            // that, and the passive probe that did was ignored.
            //
            // `rnum` is Steam's own byte 0, carried through `hidraw_get_report`
            // unchanged; `get_report_reply` echoes it back the way
            // `usbhid_get_raw_report` would on real hardware, and sizes the
            // answer as that profile's descriptor promises.
            let data = profile.get_report_reply(rnum, shared.selector.load(Ordering::Relaxed));
            let _ = shared.write_event(&get_report_reply_event(id, &data));
        }
        Decoded::SetReport(id, write) => {
            // Ack first, interpret later: the 5 s timeout is measured from the
            // request, and the consumer of `tx` runs on the daemon's own loop.
            let _ = shared.write_event(&set_report_reply_event(id));
            if let Some(&cmd) = write.command().first() {
                shared.selector.store(cmd, Ordering::Relaxed);
            }
            let _ = tx.try_send(write);
        }
        Decoded::Output(write) => {
            let _ = tx.try_send(write);
        }
    }
}

/// Block until `fd` is readable or `timeout` elapses. Mirrors the same helper in
/// `src/gamepad.rs`; `uhid.c` implements `poll` and has no ioctl at all.
fn poll_readable(fd: libc::c_int, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `pfd` is a single valid pollfd and `fd` is owned by the caller.
    let n = unsafe { libc::poll(&mut pfd, 1, ms) };
    n > 0 && pfd.revents & libc::POLLIN != 0
}

// ---------------------------------------------------------------------------
// The fd-injection boundary
// ---------------------------------------------------------------------------

/// Obtain a read-write descriptor for `/dev/uhid`, and say where it came from.
///
/// **This is the seam the host-integration half fills**, and it is now filled:
/// the descriptor comes from the root broker ([`crate::broker`]) when one is
/// installed, and from a plain `open()` when one is not.
///
/// The direct open is still tried, and on an un-set-up machine still fails with
/// `EACCES`: the node is `crw------- 1 root root 10, 239` and no shipped udev
/// rule opens it (verified locally; see
/// `docs/research/uhid-steam-controller.md` §1.4). That failure is expected, is
/// logged once by [`crate::run`], and leaves the daemon running with no relay.
///
/// Nothing else in this module knows where the descriptor came from, and
/// `UHID_CREATE2` (unlike the legacy `UHID_CREATE`) has no
/// `f_cred != current_cred()` check, so a descriptor opened by root and passed
/// over `SCM_RIGHTS` is usable as-is — which is the whole reason this design
/// works at all.
pub fn acquire_uhid_from() -> io::Result<(OwnedFd, &'static str)> {
    let path = crate::broker::socket_path();
    match crate::broker::request_at(&path, crate::broker::Request::Uhid) {
        Ok(fds) => match fds.into_iter().next() {
            Some(fd) => return Ok((fd, "broker")),
            // `ok 0` for `uhid` would be a broker bug rather than an ordinary
            // fallback condition, so it gets a line of its own — but falling
            // through to the direct open is still the right thing to do.
            None => eprintln!("warning: the fd broker answered `uhid` with no descriptor"),
        },
        // A missing socket is the ordinary state on a machine that has not run
        // `hyprpad setup` and is not worth a word here; a broker that is there
        // and said no is.
        Err(e) if path.exists() => {
            eprintln!("warning: the fd broker refused /dev/uhid ({e}); opening it directly")
        }
        Err(_) => {}
    }
    let file = OpenOptions::new().read(true).write(true).open("/dev/uhid")?;
    Ok((OwnedFd::from(file), "direct"))
}

/// [`acquire_uhid_from`] without the provenance, for callers that only want the
/// descriptor.
pub fn acquire_uhid() -> io::Result<OwnedFd> {
    acquire_uhid_from().map(|(fd, _)| fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;

    /// Every offset above, restated against `include/uapi/linux/uhid.h`.
    ///
    /// The header's structs are `__packed` (all but `uhid_start_req`) and
    /// `struct uhid_event` is itself `__packed`, so each field sits at
    /// `4 + offsetof(member)` with no padding anywhere. Writing them out twice
    /// is the point: this test fails loudly if anyone "tidies" a constant.
    #[test]
    fn layout_matches_linux_uhid_h() {
        // struct uhid_create2_req {
        //     __u8 name[128]; __u8 phys[64]; __u8 uniq[64];
        //     __u16 rd_size; __u16 bus;
        //     __u32 vendor; __u32 product; __u32 version; __u32 country;
        //     __u8 rd_data[HID_MAX_DESCRIPTOR_SIZE]; } __packed;
        assert_eq!(O_CREATE2_NAME, 4);
        assert_eq!(O_CREATE2_PHYS, 4 + 128);
        assert_eq!(O_CREATE2_UNIQ, 4 + 128 + 64);
        assert_eq!(O_CREATE2_RD_SIZE, 4 + 128 + 64 + 64);
        assert_eq!(O_CREATE2_BUS, O_CREATE2_RD_SIZE + 2);
        assert_eq!(O_CREATE2_VENDOR, O_CREATE2_BUS + 2);
        assert_eq!(O_CREATE2_PRODUCT, O_CREATE2_VENDOR + 4);
        assert_eq!(O_CREATE2_VERSION, O_CREATE2_PRODUCT + 4);
        assert_eq!(O_CREATE2_COUNTRY, O_CREATE2_VERSION + 4);
        assert_eq!(O_CREATE2_RD_DATA, O_CREATE2_COUNTRY + 4);
        assert_eq!(O_CREATE2_RD_DATA, 280, "the header's documented offset");
        assert_eq!(create2_len(UHID_DATA_MAX), 4376);

        // struct uhid_start_req { __u64 dev_flags; };  /* NOT packed */
        assert_eq!(O_START_FLAGS, 4);

        // struct uhid_input2_req { __u16 size; __u8 data[UHID_DATA_MAX]; } __packed;
        assert_eq!((O_INPUT2_SIZE, O_INPUT2_DATA), (4, 6));

        // struct uhid_output_req { __u8 data[UHID_DATA_MAX]; __u16 size; __u8 rtype; } __packed;
        assert_eq!(O_OUTPUT_DATA, 4);
        assert_eq!(O_OUTPUT_SIZE, 4 + UHID_DATA_MAX);
        assert_eq!(O_OUTPUT_RTYPE, O_OUTPUT_SIZE + 2);
        assert_eq!((O_OUTPUT_SIZE, O_OUTPUT_RTYPE), (4100, 4102));

        // struct uhid_get_report_req { __u32 id; __u8 rnum; __u8 rtype; } __packed;
        assert_eq!((O_GET_REPORT_ID, O_GET_REPORT_RNUM, O_GET_REPORT_RTYPE), (4, 8, 9));

        // struct uhid_get_report_reply_req {
        //     __u32 id; __u16 err; __u16 size; __u8 data[UHID_DATA_MAX]; } __packed;
        assert_eq!((O_GRR_ID, O_GRR_ERR, O_GRR_SIZE, O_GRR_DATA), (4, 8, 10, 12));

        // struct uhid_set_report_req {
        //     __u32 id; __u8 rnum; __u8 rtype; __u16 size; __u8 data[…]; } __packed;
        assert_eq!(
            (
                O_SET_REPORT_ID,
                O_SET_REPORT_RNUM,
                O_SET_REPORT_RTYPE,
                O_SET_REPORT_SIZE,
                O_SET_REPORT_DATA
            ),
            (4, 8, 9, 10, 12)
        );

        // struct uhid_set_report_reply_req { __u32 id; __u16 err; } __packed;
        assert_eq!((O_SRR_ID, O_SRR_ERR, SRR_LEN), (4, 8, 10));

        // sizeof(struct uhid_event) on x86-64, and the header's own constants.
        assert_eq!(EVENT_SIZE, 4380);
        assert_eq!(UHID_DATA_MAX, 4096);
        assert_eq!(BUS_USB, 0x03);
        assert_eq!(
            (
                DEV_NUMBERED_FEATURE_REPORTS,
                DEV_NUMBERED_OUTPUT_REPORTS,
                DEV_NUMBERED_INPUT_REPORTS
            ),
            (1, 2, 4)
        );

        // enum uhid_event_type, verbatim.
        assert_eq!(ev::DESTROY, 1);
        assert_eq!(ev::START, 2);
        assert_eq!(ev::STOP, 3);
        assert_eq!(ev::OPEN, 4);
        assert_eq!(ev::CLOSE, 5);
        assert_eq!(ev::OUTPUT, 6);
        assert_eq!(ev::GET_REPORT, 9);
        assert_eq!(ev::GET_REPORT_REPLY, 10);
        assert_eq!(ev::CREATE2, 11);
        assert_eq!(ev::INPUT2, 12);
        assert_eq!(ev::SET_REPORT, 13);
        assert_eq!(ev::SET_REPORT_REPLY, 14);
        assert_eq!((rtype::FEATURE, rtype::OUTPUT, rtype::INPUT), (0, 1, 2));
    }

    /// The `deck` profile's `UHID_CREATE2` event, rebuilt exactly the way
    /// `scripts/research/uhid_active_probe.py::ev_create2` builds it, must equal
    /// what [`create2_event`] produces. That probe is the run Steam adopted.
    #[test]
    fn create2_matches_the_proven_probe_byte_for_byte() {
        let p = profile::deck();
        // The probe, transcribed:
        //   buf  = struct.pack("<I", UHID_CREATE2)
        //   buf += DEV_NAME.ljust(128, b"\0")
        //   buf += DEV_PHYS.ljust(64, b"\0")
        //   buf += DEV_UNIQ.ljust(64, b"\0")
        //   buf += struct.pack("<HH", len(CONTROLLER_DESCRIPTOR), BUS_USB)
        //   buf += struct.pack("<IIII", VENDOR, PRODUCT, VERSION, COUNTRY)
        //   buf += CONTROLLER_DESCRIPTOR.ljust(UHID_DATA_MAX, b"\0")
        let mut want = Vec::new();
        want.extend_from_slice(&11u32.to_le_bytes());
        want.extend_from_slice(&ljust(b"Steam Controller", 128));
        want.extend_from_slice(&ljust(b"", 64));
        want.extend_from_slice(&ljust(b"", 64));
        want.extend_from_slice(&38u16.to_le_bytes());
        want.extend_from_slice(&0x03u16.to_le_bytes());
        want.extend_from_slice(&0x28deu32.to_le_bytes());
        want.extend_from_slice(&0x12f0u32.to_le_bytes());
        want.extend_from_slice(&0x1000u32.to_le_bytes());
        want.extend_from_slice(&0u32.to_le_bytes());
        want.extend_from_slice(p.descriptor);

        let got = create2_event(p);
        assert_eq!(got.len(), 280 + 38);
        assert_eq!(got, want, "CREATE2 must be byte-identical to the proven probe");
    }

    fn ljust(s: &[u8], n: usize) -> Vec<u8> {
        let mut v = s.to_vec();
        v.resize(n, 0);
        v
    }

    #[test]
    fn create2_for_triton_carries_the_captured_identity() {
        let p = profile::triton();
        let ev = create2_event(p);
        assert_eq!(u32::from_le_bytes(ev[0..4].try_into().unwrap()), ev::CREATE2);
        assert_eq!(&ev[O_CREATE2_NAME..O_CREATE2_NAME + 31], b"Valve Software Steam Controller");
        assert_eq!(&ev[O_CREATE2_UNIQ..O_CREATE2_UNIQ + 13], b"FXA9961402A6C");
        assert_eq!(le_u16(&ev, O_CREATE2_RD_SIZE), Some(372));
        assert_eq!(le_u16(&ev, O_CREATE2_BUS), Some(BUS_USB));
        assert_eq!(le_u32(&ev, O_CREATE2_VENDOR), Some(0x28de));
        assert_eq!(le_u32(&ev, O_CREATE2_PRODUCT), Some(0x1302));
        assert_eq!(le_u32(&ev, O_CREATE2_VERSION), Some(0x0307));
        assert_eq!(&ev[O_CREATE2_RD_DATA..], p.descriptor);
    }

    #[test]
    fn a_name_too_long_for_the_field_truncates_instead_of_panicking() {
        let mut field = [0xffu8; 8];
        put_cstr(&mut field, "abcdefghijkl");
        assert_eq!(&field[..7], b"abcdefg");
        assert_eq!(field[7], 0xff, "only the copied prefix is touched");
        // A multi-byte char is never split.
        let mut field = [0u8; 4];
        put_cstr(&mut field, "aé");
        assert_eq!(&field, b"a\xc3\xa9\0");
    }

    #[test]
    fn input2_carries_only_the_populated_prefix() {
        let ev = input2_event(&[0x42, 0x01, 0x02]);
        assert_eq!(ev.len(), 6 + 3, "the kernel zero-extends the rest");
        assert_eq!(u32::from_le_bytes(ev[0..4].try_into().unwrap()), ev::INPUT2);
        assert_eq!(le_u16(&ev, O_INPUT2_SIZE), Some(3));
        assert_eq!(&ev[O_INPUT2_DATA..], &[0x42, 0x01, 0x02]);
    }

    #[test]
    fn replies_echo_the_id_and_report_success() {
        // The probe: struct.pack("<I", 10) + struct.pack("<IHH", rid, 0, len) + data
        let ev = get_report_reply_event(7, &[0xaa, 0xbb]);
        assert_eq!(u32::from_le_bytes(ev[0..4].try_into().unwrap()), ev::GET_REPORT_REPLY);
        assert_eq!(le_u32(&ev, O_GRR_ID), Some(7));
        assert_eq!(le_u16(&ev, O_GRR_ERR), Some(0));
        assert_eq!(le_u16(&ev, O_GRR_SIZE), Some(2));
        assert_eq!(&ev[O_GRR_DATA..], &[0xaa, 0xbb]);

        // The probe: struct.pack("<IIH", 14, rid, 0)
        let ev = set_report_reply_event(9);
        assert_eq!(ev, [14u8, 0, 0, 0, 9, 0, 0, 0, 0, 0]);
    }

    /// Build a `UHID_GET_REPORT` event the way the kernel would.
    fn kernel_get_report(id: u32, rnum: u8, rt: u8) -> Vec<u8> {
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&ev::GET_REPORT.to_le_bytes());
        ev[O_GET_REPORT_ID..O_GET_REPORT_ID + 4].copy_from_slice(&id.to_le_bytes());
        ev[O_GET_REPORT_RNUM] = rnum;
        ev[O_GET_REPORT_RTYPE] = rt;
        ev
    }

    /// Build a `UHID_SET_REPORT` event the way the kernel would.
    fn kernel_set_report(id: u32, rnum: u8, rt: u8, data: &[u8]) -> Vec<u8> {
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&ev::SET_REPORT.to_le_bytes());
        ev[O_SET_REPORT_ID..O_SET_REPORT_ID + 4].copy_from_slice(&id.to_le_bytes());
        ev[O_SET_REPORT_RNUM] = rnum;
        ev[O_SET_REPORT_RTYPE] = rt;
        ev[O_SET_REPORT_SIZE..O_SET_REPORT_SIZE + 2]
            .copy_from_slice(&(data.len() as u16).to_le_bytes());
        ev[O_SET_REPORT_DATA..O_SET_REPORT_DATA + data.len()].copy_from_slice(data);
        ev
    }

    /// Build a `UHID_OUTPUT` event the way the kernel would.
    fn kernel_output(rt: u8, data: &[u8]) -> Vec<u8> {
        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&ev::OUTPUT.to_le_bytes());
        ev[O_OUTPUT_DATA..O_OUTPUT_DATA + data.len()].copy_from_slice(data);
        ev[O_OUTPUT_SIZE..O_OUTPUT_SIZE + 2].copy_from_slice(&(data.len() as u16).to_le_bytes());
        ev[O_OUTPUT_RTYPE] = rt;
        ev
    }

    #[test]
    fn decodes_every_event_the_kernel_can_send() {
        let mut start = vec![0u8; EVENT_SIZE];
        start[0..4].copy_from_slice(&ev::START.to_le_bytes());
        start[O_START_FLAGS..O_START_FLAGS + 8].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(decode(&start), Some(Decoded::Start(7)));

        assert_eq!(decode(&ev::OPEN.to_le_bytes()), Some(Decoded::Open));
        assert_eq!(decode(&ev::CLOSE.to_le_bytes()), Some(Decoded::Close));
        assert_eq!(decode(&ev::STOP.to_le_bytes()), Some(Decoded::Stop));
        assert_eq!(decode(&42u32.to_le_bytes()), Some(Decoded::Other(42)));
        assert_eq!(decode(&[1, 2]), None, "a runt decodes to nothing");

        assert_eq!(
            decode(&kernel_get_report(5, 1, rtype::FEATURE)),
            Some(Decoded::GetReport(5, 1, rtype::FEATURE))
        );
        let want = HostWrite {
            channel: WriteChannel::SetReport,
            rtype: rtype::FEATURE,
            data: vec![0x00, 0x87, 0x03],
        };
        assert_eq!(
            decode(&kernel_set_report(6, 0, rtype::FEATURE, &[0x00, 0x87, 0x03])),
            Some(Decoded::SetReport(6, want))
        );
        let want = HostWrite {
            channel: WriteChannel::Output,
            rtype: rtype::OUTPUT,
            data: vec![0x81, 0x01],
        };
        assert_eq!(decode(&kernel_output(rtype::OUTPUT, &[0x81, 0x01])), Some(Decoded::Output(want)));
    }

    #[test]
    fn a_host_write_strips_the_report_number_byte_to_reach_the_command() {
        let w = HostWrite {
            channel: WriteChannel::SetReport,
            rtype: rtype::FEATURE,
            data: vec![0x01, 0x87, 0x03, 0x09, 0x00, 0x00],
        };
        assert_eq!(w.command(), &[0x87, 0x03, 0x09, 0x00, 0x00]);

        // An output write keeps its leading byte: there the report id *is* the
        // command (`0x81` = the haptic pulse `src/haptics.rs` writes).
        let out = HostWrite {
            channel: WriteChannel::Output,
            rtype: rtype::OUTPUT,
            data: vec![0x81, 0x01, 0x90, 0x01],
        };
        assert_eq!(out.command(), &[0x81, 0x01, 0x90, 0x01]);

        let empty =
            HostWrite { channel: WriteChannel::SetReport, rtype: rtype::FEATURE, data: Vec::new() };
        assert_eq!(empty.command(), &[] as &[u8]);
    }

    /// A `SOCK_SEQPACKET` socket pair, standing in for `/dev/uhid`.
    ///
    /// The datagram semantics are the point: one `write` is one message and a
    /// `read` returns exactly one, truncated to the caller's buffer — which is
    /// precisely `uhid_char_write`/`uhid_char_read`. A `SOCK_STREAM` pair
    /// (`UnixStream::pair`) would coalesce two events into one read and quietly
    /// lose the second, so it cannot stand in for a uhid fd.
    fn seqpacket_pair() -> (UnixStream, OwnedFd) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid two-element array for the kernel to fill.
        let rc =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair: {}", io::Error::last_os_error());
        // SAFETY: both descriptors are fresh, owned, and taken exactly once.
        let (a, b) = unsafe {
            (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
        };
        (UnixStream::from(a), b)
    }

    /// Drive the real event loop against a socketpair standing in for
    /// `/dev/uhid`, for whichever profile is under test.
    fn loop_for(
        profile: &'static Profile,
    ) -> (UnixStream, Arc<Shared>, mpsc::Receiver<HostWrite>) {
        let (kernel, device) = seqpacket_pair();
        let shared = Arc::new(Shared::new(device, profile.default_selector).expect("nonblock"));
        let (tx, rx) = mpsc::sync_channel(WRITE_QUEUE_DEPTH);
        let s = Arc::clone(&shared);
        std::thread::spawn(move || event_loop(&s, profile, &tx));
        (kernel, shared, rx)
    }

    /// The proven profile, which most of these tests exercise.
    fn loop_on_a_socketpair() -> (UnixStream, Arc<Shared>, mpsc::Receiver<HostWrite>) {
        loop_for(profile::deck())
    }

    /// Read one event off the kernel end of the pair.
    fn read_event(kernel: &mut UnixStream) -> Vec<u8> {
        let mut buf = vec![0u8; EVENT_SIZE];
        let n = kernel.read(&mut buf).expect("one event");
        buf.truncate(n);
        buf
    }

    #[test]
    fn the_event_loop_answers_a_get_report_with_the_canned_data() {
        let (mut kernel, shared, _rx) = loop_on_a_socketpair();
        kernel.write_all(&kernel_get_report(11, 0, rtype::FEATURE)).unwrap();

        let mut reply = vec![0u8; EVENT_SIZE];
        let n = kernel.read(&mut reply).unwrap();
        let reply = &reply[..n];
        assert_eq!(u32::from_le_bytes(reply[0..4].try_into().unwrap()), ev::GET_REPORT_REPLY);
        assert_eq!(le_u32(reply, O_GRR_ID), Some(11), "the request id is echoed");
        assert_eq!(le_u16(reply, O_GRR_ERR), Some(0), "err = 0, success");
        let size = le_u16(reply, O_GRR_SIZE).unwrap() as usize;
        assert_eq!(size, 64, "never empty — an empty answer is what the passive probe did");
        assert_eq!(&reply[O_GRR_DATA..O_GRR_DATA + size], &profile::deck().canned_reply(profile::deck().default_selector)[..]);

        shared.stop.store(true, Ordering::Relaxed);
    }

    /// The triton round trip Steam actually performs, end to end through the
    /// real event loop: a `SET_REPORT` naming `GetAttributesValues` selects the
    /// answer, and the `GET_REPORT` that follows gets it — **65 bytes, report
    /// number first, command id second**, the frame
    /// `usbhid_get_raw_report` would have produced on a real `1302`.
    #[test]
    fn a_triton_get_report_round_trip_returns_65_framed_bytes() {
        let t = profile::triton();
        let (mut kernel, shared, rx) = loop_for(t);

        // Steam: `buf[0] = 0; buf[1] = ID_GET_ATTRIBUTES_VALUES;`
        // `SDL_hid_send_feature_report(dev, buf, 65)` — hidraw hands the whole
        // 65-byte buffer through, report-number byte included.
        let mut request = vec![0u8; 65];
        request[1] = profile::cmd::GET_ATTRIBUTES_VALUES;
        kernel.write_all(&kernel_set_report(1, 0, rtype::FEATURE, &request)).unwrap();
        assert_eq!(read_event(&mut kernel), set_report_reply_event(1), "acked first");
        let got = rx.recv_timeout(Duration::from_secs(2)).expect("the write is handed over");
        assert_eq!(got.command()[0], profile::cmd::GET_ATTRIBUTES_VALUES, "the id is data[1]");

        // Steam: `SDL_hid_get_feature_report(dev, uBuffer, 65)`.
        kernel.write_all(&kernel_get_report(2, 0, rtype::FEATURE)).unwrap();
        let reply = read_event(&mut kernel);
        assert_eq!(le_u32(&reply, O_GRR_ID), Some(2));
        assert_eq!(le_u16(&reply, O_GRR_ERR), Some(0));
        let size = le_u16(&reply, O_GRR_SIZE).unwrap() as usize;
        assert_eq!(size, 65, "a real 1302 answers a 65-byte request with 65 bytes");
        let data = &reply[O_GRR_DATA..O_GRR_DATA + size];
        assert_eq!(data[0], 0x00, "the report number Steam asked with");
        assert_eq!(data[1], profile::cmd::GET_ATTRIBUTES_VALUES, "SDL checks uBuffer[1]");
        assert_eq!(data[2], 0x2d, "…then the payload length, which SDL bounds-checks");
        assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 0x1302);
        assert_eq!(data, t.get_report_reply(0, profile::cmd::GET_ATTRIBUTES_VALUES).as_slice());

        shared.stop.store(true, Ordering::Relaxed);
    }

    /// The same exchange on the deck profile is byte-for-byte what it was: 64
    /// bytes, unnumbered, the proven bytes.
    #[test]
    fn the_deck_round_trip_is_unchanged_at_64_unnumbered_bytes() {
        let d = profile::deck();
        let (mut kernel, shared, _rx) = loop_for(d);
        let mut request = vec![0u8; 65];
        request[1] = profile::cmd::GET_STRING_ATTRIBUTE;
        kernel.write_all(&kernel_set_report(1, 0, rtype::FEATURE, &request)).unwrap();
        let _ = read_event(&mut kernel);
        kernel.write_all(&kernel_get_report(2, 0, rtype::FEATURE)).unwrap();
        let reply = read_event(&mut kernel);
        let size = le_u16(&reply, O_GRR_SIZE).unwrap() as usize;
        assert_eq!(size, 64);
        assert_eq!(
            &reply[O_GRR_DATA..O_GRR_DATA + size],
            &d.canned_reply(profile::cmd::GET_STRING_ATTRIBUTE)[..]
        );
        shared.stop.store(true, Ordering::Relaxed);
    }

    /// The triton input stream on the wire: exactly what the descriptor
    /// promises, because `uhid_dev_input2` hands the buffer straight to
    /// `hid_report_raw_event`, which reads `data[0]` as the report id.
    #[test]
    fn a_triton_input_event_carries_the_id_byte_and_the_declared_length() {
        use crate::uhid::translate::{self, StripMask};
        let f = profile::triton().framing;

        // A captured-shape puck report: id 0x42, counter, A pressed.
        let mut raw = vec![0u8; f.input_len];
        raw[0] = 0x42;
        raw[1] = 0x5a;
        raw[2] = 0x01;
        let report = translate::puck_to_triton(&raw, StripMask::guide_only()).expect("a 0x42");
        let ev = input2_event(&report);
        assert_eq!(le_u16(&ev, O_INPUT2_SIZE), Some(54));
        assert_eq!(usize::from(le_u16(&ev, O_INPUT2_SIZE).unwrap()), f.input_len);
        assert_eq!(ev[O_INPUT2_DATA], f.input_report_id.unwrap(), "byte 0 is the report id");
        assert_eq!(ev[O_INPUT2_DATA + 1], 0x5a, "the puck's own counter, relayed");
        assert_eq!(ev.len(), O_INPUT2_DATA + 54);

        // Neutral is the same shape — a silent puck must not change the framing.
        let ev = input2_event(&translate::triton_neutral(3));
        assert_eq!(le_u16(&ev, O_INPUT2_SIZE), Some(54));
        assert_eq!(ev[O_INPUT2_DATA], 0x42);

        // And deck's stays bare 64 bytes with no id byte at all.
        let d = profile::deck().framing;
        let ev = input2_event(&translate::deck_neutral(3));
        assert_eq!(le_u16(&ev, O_INPUT2_SIZE), Some(64));
        assert_eq!(usize::from(le_u16(&ev, O_INPUT2_SIZE).unwrap()), d.input_len);
        assert_eq!(d.input_report_id, None);
    }

    /// `UHID_CREATE2` has nowhere to ask for numbered framing, and that is the
    /// point: `struct uhid_create2_req` ends at `country` + `rd_data`. The
    /// descriptor is the request; `dev_flags` comes back the other way.
    #[test]
    fn create2_has_no_dev_flags_field_the_descriptor_is_the_request() {
        for p in [profile::triton(), profile::deck()] {
            let ev = create2_event(p);
            assert_eq!(
                ev.len(),
                O_CREATE2_RD_DATA + p.descriptor.len(),
                "nothing sits between country and rd_data"
            );
            assert_eq!(&ev[O_CREATE2_RD_DATA..], p.descriptor);
            assert_eq!(le_u32(&ev, O_CREATE2_COUNTRY), Some(0));
            // The flags are a *reply*: they only ever arrive in UHID_START.
            let mut start = vec![0u8; 12];
            start[0..4].copy_from_slice(&ev::START.to_le_bytes());
            start[O_START_FLAGS..O_START_FLAGS + 8]
                .copy_from_slice(&p.framing.expected_dev_flags.to_le_bytes());
            assert_eq!(decode(&start), Some(Decoded::Start(p.framing.expected_dev_flags)));
        }
        assert_eq!(profile::triton().framing.expected_dev_flags, 1 | 2 | 4);
        assert_eq!(profile::deck().framing.expected_dev_flags, 0);
    }

    #[test]
    fn the_event_loop_acks_a_set_report_and_hands_the_payload_over() {
        let (mut kernel, shared, rx) = loop_on_a_socketpair();
        // Steam's lizard-disable, as it arrives on an unnumbered Deck device.
        let payload = [0x00, 0x87, 0x03, 0x09, 0x00, 0x00];
        kernel.write_all(&kernel_set_report(3, 0, rtype::FEATURE, &payload)).unwrap();

        let mut reply = vec![0u8; EVENT_SIZE];
        let n = kernel.read(&mut reply).unwrap();
        assert_eq!(&reply[..n], set_report_reply_event(3).as_slice());

        let got = rx.recv_timeout(Duration::from_secs(2)).expect("payload on the channel");
        assert_eq!(got.channel, WriteChannel::SetReport);
        assert_eq!(got.rtype, rtype::FEATURE);
        assert_eq!(got.data, payload);
        assert_eq!(got.command(), &[0x87, 0x03, 0x09, 0x00, 0x00]);

        shared.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn the_event_loop_forwards_an_output_write_without_replying() {
        let (mut kernel, shared, rx) = loop_on_a_socketpair();
        kernel.write_all(&kernel_output(rtype::OUTPUT, &[0x00, 0xeb, 0x09])).unwrap();

        let got = rx.recv_timeout(Duration::from_secs(2)).expect("payload on the channel");
        assert_eq!(got.channel, WriteChannel::Output);
        assert_eq!(got.data, [0x00, 0xeb, 0x09]);

        // There is nothing to reply to on the interrupt channel, so the kernel
        // end must see no traffic at all.
        kernel.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 64];
        assert!(kernel.read(&mut buf).is_err(), "an OUTPUT is fire-and-forget");

        shared.stop.store(true, Ordering::Relaxed);
    }

    /// Whatever `UHID_START` says is recorded, even when it disagrees with the
    /// profile — the flags it carries are the kernel's *answer*, and the only
    /// honest thing to do with a surprising one is keep it and say so. (This
    /// case does warn: the deck profile expects `0` and gets `1 | 4`.)
    #[test]
    fn start_and_open_are_recorded_for_the_streamer_to_see() {
        let (mut kernel, shared, _rx) = loop_on_a_socketpair();
        let mut start = vec![0u8; 12];
        start[0..4].copy_from_slice(&ev::START.to_le_bytes());
        start[O_START_FLAGS..O_START_FLAGS + 8].copy_from_slice(
            &(DEV_NUMBERED_FEATURE_REPORTS | DEV_NUMBERED_INPUT_REPORTS).to_le_bytes(),
        );
        kernel.write_all(&start).unwrap();
        kernel.write_all(&ev::OPEN.to_le_bytes()).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if shared.opened.load(Ordering::Relaxed) {
                break;
            }
            std::thread::yield_now();
        }
        assert!(shared.opened.load(Ordering::Relaxed), "UHID_OPEN sets the attached flag");
        assert_eq!(shared.dev_flags.load(Ordering::Relaxed), 1 | 4);

        kernel.write_all(&ev::CLOSE.to_le_bytes()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if !shared.opened.load(Ordering::Relaxed) {
                break;
            }
            std::thread::yield_now();
        }
        assert!(!shared.opened.load(Ordering::Relaxed));

        shared.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn acquire_uhid_reports_the_real_error_rather_than_pretending() {
        // On this machine `/dev/uhid` is `crw------- root root`, so this is
        // `PermissionDenied` — the expected, documented, non-fatal outcome. In a
        // container without the node at all it is `NotFound`. Either way the
        // function must surface the OS error, never a fabricated success.
        match acquire_uhid() {
            Ok(_) => {} // a machine that has already installed the udev rule
            Err(e) => assert!(
                matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound),
                "unexpected error from /dev/uhid: {e}"
            ),
        }
    }
}
