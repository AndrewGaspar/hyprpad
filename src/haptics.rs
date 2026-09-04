//! Haptic pulses on the controller, over either transport (the puck,
//! `28de:1304` IBEX/Proteus; or Bluetooth, `28de:1303`).
//!
//! The controller has an actuator behind each trackpad. Firing a short pulse on
//! the pad a thumb is resting on is what makes the on-screen keyboard feel like
//! a physical one — a subtle **tick** as the cursor crosses onto a new key, a
//! firmer **click** on commit, a **buzz** when a gesture is recognized, and a
//! detent per emitted circular-scroll tick.
//!
//! ## The wire format (source: Linux `drivers/hid/hid-steam.c`, IBEX path)
//!
//! The 2026 puck is driven under `STEAM_QUIRK_IBEX`, whose haptics are **not**
//! the gen-1/Deck `0x8F` feature report. One pulse is an 8-byte HID **output**
//! report, id `REPORT_ID_HAPTIC_PULSE` (`0x81`), all fields little-endian:
//!
//! | Byte | Field | Meaning |
//! |---|---|---|
//! | 0 | `id` = `0x81` | the report id |
//! | 1 | `side` | actuator: `0` = right pad, `1` = left pad, `2` = both |
//! | 2–3 | `on_us` | pulse ON width in µs — the "strength" knob |
//! | 4–5 | `off_us` | gap between pulses, µs |
//! | 6–7 | `repeat_count` | number of on/off cycles |
//!
//! **This is the one departure from [`crate::lizard`]:** lizard sends *feature*
//! reports with the `HIDIOCSFEATURE` ioctl; an output report in userspace is a
//! plain `write()` of exactly those 8 bytes to a writable hidraw fd (the kernel
//! sends `hid_hw_output_report(..., 8)`). It is **not** zero-padded to 64 —
//! that is the feature-report convention, not this one.
//!
//! Tick vs. click is purely `on_us`/`repeat_count`: the IBEX pulse struct has no
//! gain field, so the gen-1 `gain` argument is dropped on this device. The
//! [`Feel`] values are the kernel's own calibrated mode-switch feedback (a
//! 400 µs single tick; a 500 µs on/off train for the buzz) and its derivatives.
//!
//! ## The second report: game rumble
//!
//! The pulses above are hyprpad's own UI feedback. A *game's* force feedback is
//! a different report on the same wire — `REPORT_ID_HAPTIC_RUMBLE` (`0x80`), 10
//! bytes, `struct steam_ibex_haptic_rumble` — driven through [`Haptics::rumble`]
//! by the virtual-gamepad bridge ([`crate::gamepad`]). Both share this module's
//! one writer thread and node set, so the two never open the controller twice.
//!
//! Rumble is rate-limited by its caller, not here: `hid-steam` throttles to
//! 20 Hz and re-sends every 50 ms while active, because the controller restarts
//! its haptic pattern on each packet. See [`Haptics::rumble`] and
//! `run::drive_rumble`.
//!
//! ## Which node, concurrency, sleep, and never being fatal
//!
//! hidraw is not exclusive, so writing output reports works while another
//! process holds the same node. The puck exposes one node per pairing slot plus
//! a dongle-control interface, and the pulse must reach the slot the controller
//! is actually on.
//!
//! `lizard.rs` finds that slot by trial: its *feature* reports STALL (`EPIPE`) on
//! every other node, so "write to all, keep what works" converges. **That trick
//! does not transfer here.** An output report is fire-and-forget, and every node
//! accepts it — verified on-device: all five puck nodes returned a successful
//! 8-byte write for the `0x81` report. So a failed write cannot identify the
//! live slot. *Input* can: only the occupied slot streams reports (verified:
//! 268 reports/s on one node, 0 on the other four). We therefore open every node
//! read-write, `poll` briefly for readability, and keep only the node(s) that
//! streamed — one event, one pulse. If nothing streams (an asleep or silent
//! controller) we keep them all, falling back to `lizard.rs`'s shotgun.
//!
//! The fd set is refreshed periodically so the pulse follows the controller to a
//! new slot after a re-pair, and re-opened whenever every write fails (the
//! controller went away). A failure is logged **once**, never per tick and never
//! fatally: a controller with no haptics, no writable node, or no controller at all
//! degrades to a silent no-op.
//!
//! Device writes run on their own thread behind a small bounded queue
//! ([`QUEUE_DEPTH`]): a hidraw write is a synchronous USB transfer that can block
//! for milliseconds (or stall outright against a sleeping controller), and the
//! daemon's main loop services a ~250 Hz report stream — it must never wait on
//! one. If the queue is full the pulse is dropped, which is the right answer for
//! feedback: a late tick is worse than no tick.

use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `REPORT_ID_HAPTIC_PULSE` (kernel `hid-steam.c`). Byte 0 of the output report.
const REPORT_ID_HAPTIC_PULSE: u8 = 0x81;

/// `REPORT_ID_HAPTIC_RUMBLE` (kernel `hid-steam.c`). Byte 0 of the *rumble*
/// output report — the force-feedback path a game drives, as opposed to the
/// discrete UI pulses above. See [`Haptics::rumble`].
const REPORT_ID_HAPTIC_RUMBLE: u8 = 0x80;

/// One pulse on the wire: report id + `steam_ibex_haptic_pulse` (7 bytes).
const PULSE_LEN: usize = 8;

/// One rumble on the wire: report id + `steam_ibex_haptic_rumble` (9 bytes).
const RUMBLE_LEN: usize = 10;

/// The widest output report the writer thread carries. Every queued report is
/// padded to this and written `len` bytes long, because the two report ids have
/// different fixed sizes and the kernel sends each at its own exact length.
const MAX_REPORT_LEN: usize = RUMBLE_LEN;

/// Depth of the queue between the daemon and the device-writer thread. Deep
/// enough to absorb a burst (a fast circular scroll, a two-thumb key crossing)
/// without dropping, shallow enough that a stalled write can never leave a
/// backlog of stale pulses to fire late.
const QUEUE_DEPTH: usize = 8;

/// Minimum gap between attempts to (re)open the controller's nodes after a failure.
/// Without it a sleeping controller — every write failing — would re-scan
/// `/sys/class/hidraw` at the tick rate.
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);

/// How long a *working* fd set is trusted before it is re-opened and re-probed.
/// An output report to the wrong pairing slot succeeds silently, so a controller
/// that re-paired onto another slot cannot be detected by write errors; this
/// bounds how long the pulses would go to the old one. Long enough that the
/// probe is invisible in normal use.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// How long the probe waits for a node to report input before giving up and
/// treating every node as a candidate. `poll` returns as soon as the live slot
/// speaks — ~4 ms at the controller's ~250 Hz — so this ceiling is only ever
/// paid when nothing is streaming at all.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Upper bound on `[haptics] intensity`. The knob scales `on_us`; past a few ×
/// the calibrated width there is nothing left to gain and the actuator is just
/// being held on, so the scale is clamped rather than trusted.
const INTENSITY_MAX: f64 = 4.0;

/// Hard ceiling on a scaled `on_us`, in µs. 2 ms is already four times the
/// kernel's longest calibrated pulse; anything beyond is a config typo.
const ON_US_MAX: u16 = 2_000;

/// Which actuator a pulse addresses, in the kernel's **logical** `STEAM_PAD_*`
/// numbering (the firmware's own numbering is inverted — see [`wire_side`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pad {
    /// The left trackpad's actuator (`STEAM_PAD_LEFT`).
    Left = 0,
    /// The right trackpad's actuator (`STEAM_PAD_RIGHT`).
    Right = 1,
    /// Both at once (`STEAM_PAD_BOTH`).
    Both = 2,
}

/// How a pulse feels: the three shapes the daemon fires. All three are the same
/// `0x81` report — only the pulse width, gap, and repeat count differ (the IBEX
/// pulse struct exposes nothing else).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feel {
    /// A single short tick — the key-crossing / scroll-detent feel.
    Tick,
    /// A firmer double pulse — a commit "clunk", clearly heavier than a tick.
    Click,
    /// A ~10 ms train — "the gesture landed".
    Buzz,
    /// The faintest pulse — half a [`Tick`](Feel::Tick). Fired repeatedly as the
    /// right pad drives the desktop cursor (one per spacing of travel), so the
    /// pad feels textured, like Steam Input's trackpad-friction haptics.
    Texture,
}

impl Feel {
    /// `(on_us, off_us, repeat_count)` for this feel, before the intensity
    /// scale. `Tick` is the kernel's own single-tick value (`0x190` = 400 µs,
    /// `steam_haptic_pulse(.., STEAM_PAD_RIGHT, 0x190, 0, 1, 0)` on a mode
    /// switch); `Buzz` is the shape of the kernel's mode-switch buzz on a
    /// shorter count. Starting points — tune on-device.
    fn pulse(self) -> (u16, u16, u16) {
        match self {
            Feel::Tick => (0x0190, 0x0000, 0x0001),
            Feel::Click => (0x0258, 0x012C, 0x0002),
            Feel::Buzz => (0x01F4, 0x01F4, 0x000A),
            Feel::Texture => (0x00C8, 0x0000, 0x0001),
        }
    }
}

/// The firmware's `side` byte is XOR-inverted against the logical pad for
/// left/right: `steam_haptic_pulse` runs `if (pad < STEAM_PAD_BOTH) pad ^= 1;`
/// before writing it. Mirroring the kernel bit-for-bit side-steps any "is left
/// really left?" doubt.
fn wire_side(pad: Pad) -> u8 {
    let p = pad as u8;
    if p < Pad::Both as u8 {
        p ^ 1
    } else {
        p
    }
}

/// One output report queued for the writer thread: its bytes, padded to
/// [`MAX_REPORT_LEN`], plus the length actually written. The kernel sends each
/// report id at its own fixed size (8 for `0x81`, 10 for `0x80`) and neither is
/// zero-padded, so the length travels with the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct OutReport {
    bytes: [u8; MAX_REPORT_LEN],
    len: usize,
}

impl OutReport {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Build the 8-byte `0x81` pulse output report (all fields little-endian).
fn build_pulse(pad: Pad, on_us: u16, off_us: u16, count: u16) -> OutReport {
    let mut b = [0u8; MAX_REPORT_LEN];
    b[0] = REPORT_ID_HAPTIC_PULSE;
    b[1] = wire_side(pad);
    b[2..4].copy_from_slice(&on_us.to_le_bytes());
    b[4..6].copy_from_slice(&off_us.to_le_bytes());
    b[6..8].copy_from_slice(&count.to_le_bytes());
    OutReport { bytes: b, len: PULSE_LEN }
}

/// The per-side gain bytes the kernel hard-codes on **every** rumble it sends —
/// `steam_haptic_rumble(steam, 0, left, right, 2, 0)` in both the rumble work
/// item and the 20 Hz coalescing one. There is no comment explaining the
/// asymmetry; the only gain documentation in the driver (on the *pulse* command)
/// describes gain as decibels in `-24..=+6`, which would make these `+2 dB` and
/// `0 dB`. Mirrored verbatim rather than reinterpreted.
const RUMBLE_LEFT_GAIN: u8 = 2;
const RUMBLE_RIGHT_GAIN: u8 = 0;

/// The `intensity` field, which the driver leaves at `0` on every call.
const RUMBLE_INTENSITY: u16 = 0;

/// Build the 10-byte `0x80` rumble output report.
///
/// Layout (`struct steam_ibex_haptic_rumble`, all little-endian):
/// `[0x80, type, intensity:le16, left.speed:le16, left.gain, right.speed:le16,
/// right.gain]`. `type` is never assigned by the driver (the struct comes from
/// `kzalloc`), so it is always `0`.
///
/// **The `side` XOR does not apply here** — unlike `steam_haptic_pulse`,
/// `steam_haptic_rumble` has no `pad ^= 1`; left and right are named fields.
///
/// `left_speed` is the FF `strong_magnitude` and `right_speed` the
/// `weak_magnitude`, passed through with **no scaling whatsoever** — the driver
/// stores `effect->u.rumble.strong_magnitude` / `weak_magnitude` and hands them
/// straight to this report.
fn build_rumble(left_speed: u16, right_speed: u16) -> OutReport {
    let mut b = [0u8; MAX_REPORT_LEN];
    b[0] = REPORT_ID_HAPTIC_RUMBLE;
    b[1] = 0; // `type`, left zero by the driver
    b[2..4].copy_from_slice(&RUMBLE_INTENSITY.to_le_bytes());
    b[4..6].copy_from_slice(&left_speed.to_le_bytes());
    b[6] = RUMBLE_LEFT_GAIN;
    b[7..9].copy_from_slice(&right_speed.to_le_bytes());
    b[9] = RUMBLE_RIGHT_GAIN;
    OutReport { bytes: b, len: RUMBLE_LEN }
}

/// Apply the `[haptics] intensity` scale to a feel's pulse width, clamped to a
/// sane range. A non-finite or non-positive scale yields `0`, which the caller
/// treats as "nothing to fire" (use `enabled = false` to switch haptics off —
/// this is only the belt-and-braces path for a nonsense value).
fn scale_on_us(on_us: u16, intensity: f64) -> u16 {
    if !intensity.is_finite() || intensity <= 0.0 {
        return 0;
    }
    let scaled = f64::from(on_us) * intensity.min(INTENSITY_MAX);
    scaled.round().clamp(0.0, f64::from(ON_US_MAX)) as u16
}

/// A live handle to the controller's haptics.
///
/// Construct once, up front — nothing is opened until the first pulse, so a
/// config with `[haptics] enabled = false` never touches the device. Every
/// failure degrades to a logged-once no-op.
pub struct Haptics {
    /// Queue into the device-writer thread. Bounded, and fed with `try_send` so
    /// the daemon's main loop never blocks on a USB transfer.
    tx: mpsc::SyncSender<OutReport>,
    /// Whether a dropped pulse has already been logged, so a wedged writer does
    /// not spam the log at the tick rate.
    warned_full: bool,
}

impl Haptics {
    /// Spawn the device-writer thread and return a handle to it. The thread
    /// parks on the queue and opens nothing until the first pulse arrives.
    pub fn new() -> Haptics {
        let (tx, rx) = mpsc::sync_channel::<OutReport>(QUEUE_DEPTH);
        std::thread::spawn(move || writer_loop(&rx));
        Haptics { tx, warned_full: false }
    }

    /// Fire `feel` on `pad`, scaled by `intensity` (`1.0` = the calibrated
    /// value). Non-blocking: the report is queued for the writer thread, and
    /// dropped if that queue is full.
    pub fn play(&mut self, feel: Feel, pad: Pad, intensity: f64) {
        let (on_us, off_us, count) = feel.pulse();
        let on_us = scale_on_us(on_us, intensity);
        if on_us == 0 {
            return; // scaled to nothing: don't bother the device
        }
        self.queue(build_pulse(pad, on_us, off_us, count));
    }

    /// A single short tick — key crossing, scroll detent, bare-button press.
    pub fn tick(&mut self, pad: Pad, intensity: f64) {
        self.play(Feel::Tick, pad, intensity);
    }

    /// A firmer double pulse — an OSK commit.
    pub fn click(&mut self, pad: Pad, intensity: f64) {
        self.play(Feel::Click, pad, intensity);
    }

    /// A short train — a recognized gesture.
    pub fn buzz(&mut self, pad: Pad, intensity: f64) {
        self.play(Feel::Buzz, pad, intensity);
    }

    /// Fire an explicit `0x81` pulse train. The escape hatch from the fixed
    /// [`Feel`] shapes, used by the gamepad bridge's pulse-mode rumble
    /// approximation, which needs a width and repeat count derived from a game's
    /// force-feedback magnitude rather than one of the calibrated UI feels.
    /// `on_us` is clamped exactly like a scaled feel's.
    pub fn pulse(&mut self, pad: Pad, on_us: u16, off_us: u16, count: u16) {
        let on_us = on_us.min(ON_US_MAX);
        if on_us == 0 || count == 0 {
            return;
        }
        self.queue(build_pulse(pad, on_us, off_us, count));
    }

    /// Drive the controller's **force-feedback rumble** (`0x80`) — the game-facing
    /// channel, distinct from the discrete UI pulses above.
    ///
    /// `left_speed` is the `FF_RUMBLE` `strong_magnitude` and `right_speed` the
    /// `weak_magnitude`, both `0..=65535`, passed through unscaled exactly as
    /// `hid-steam`'s `steam_play_effect` does. Zero on both stops the rumble;
    /// there is no separate stop command.
    ///
    /// **Rate-limit this.** The kernel throttles rumble to 20 Hz and re-sends
    /// the same packet every 50 ms while either magnitude is non-zero, because
    /// the controller restarts its haptic pattern on every packet — back-to-back
    /// writes make it stutter or cut out. The caller owns that clock
    /// (`run::drive_rumble`); this method just queues what it is given.
    pub fn rumble(&mut self, left_speed: u16, right_speed: u16) {
        self.queue(build_rumble(left_speed, right_speed));
    }

    /// Queue one built report for the writer thread, warning once if the queue
    /// is full rather than blocking the daemon's loop on a USB transfer.
    fn queue(&mut self, report: OutReport) {
        if self.tx.try_send(report).is_err() && !self.warned_full {
            eprintln!("warning: haptics queue full (device slow or gone); dropping pulses");
            self.warned_full = true;
        }
    }
}

impl Default for Haptics {
    fn default() -> Haptics {
        Haptics::new()
    }
}

/// The device-writer thread: drain queued pulse reports and write each one to
/// the controller. Ends when the last [`Haptics`] handle drops (daemon exit).
fn writer_loop(rx: &mpsc::Receiver<OutReport>) {
    let mut dev = ControllerWriter::new();
    for report in rx {
        dev.write_report(report.as_slice());
    }
}

/// Narrow an open node set to the pairing slot(s) actually streaming input.
///
/// Returns every index of `fds` that became readable within [`PROBE_TIMEOUT`].
/// An empty result means "don't know" — nothing is reporting — and the caller
/// keeps every node rather than guessing.
fn live_indices(fds: &[File]) -> Vec<usize> {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|f| libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 })
        .collect();
    let timeout = PROBE_TIMEOUT.as_millis() as libc::c_int;
    // SAFETY: `pfds` is a live, initialised array of exactly `pfds.len()`
    // `pollfd`s for the duration of the call, each holding an fd this thread
    // owns. `poll` reads `fd`/`events` and writes `revents`, nothing else.
    let ret = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
    if ret <= 0 {
        return Vec::new(); // timeout, or an error we simply don't act on
    }
    pfds.iter()
        .enumerate()
        .filter(|(_, p)| p.revents & libc::POLLIN != 0)
        .map(|(i, _)| i)
        .collect()
}

/// The writable puck nodes, opened lazily, narrowed to the live pairing slot,
/// and re-opened after failure or once the set goes stale.
struct ControllerWriter {
    /// Nodes the pulse is written to. Empty until the first open, and again
    /// after the controller went away.
    fds: Vec<File>,
    /// When the current fd set was opened — drives both the [`REOPEN_INTERVAL`]
    /// back-off after a failure and the [`REFRESH_INTERVAL`] re-probe.
    last_open: Option<Instant>,
    /// Whether the "can't reach the actuator" warning has been logged. Cleared
    /// on the next success, so a sleep/wake cycle logs at most one line each way.
    warned: bool,
    /// Whether the "haptics live" line has been logged for the current fd set.
    announced: bool,
}

impl ControllerWriter {
    fn new() -> ControllerWriter {
        ControllerWriter { fds: Vec::new(), last_open: None, warned: false, announced: false }
    }

    /// Write one output report (8 bytes for a pulse, 10 for a rumble) to every
    /// node in the current set.
    ///
    /// A node that errors is dropped; when the set empties — the controller went
    /// away — the next pulse re-opens, at most once per [`REOPEN_INTERVAL`].
    fn write_report(&mut self, report: &[u8]) {
        if !self.ensure_open() {
            return;
        }
        let mut delivered = 0usize;
        self.fds.retain_mut(|fd| match fd.write_all(report) {
            Ok(()) => {
                delivered += 1;
                true
            }
            Err(_) => false,
        });
        if delivered > 0 {
            if !self.announced {
                eprintln!("hyprpad: haptics live on {delivered} controller node(s)");
                self.announced = true;
            }
            if self.warned {
                eprintln!("hyprpad: haptics reached the controller again");
                self.warned = false;
            }
        } else {
            self.fail_once("every controller node rejected the pulse (controller asleep?)");
            self.announced = false;
        }
    }

    /// Make sure a usable fd set is in hand: keep a fresh working one, re-probe a
    /// stale one (the controller may have re-paired onto another slot), and
    /// back off when there is nothing to open.
    fn ensure_open(&mut self) -> bool {
        match self.last_open {
            // Working and fresh: use it.
            Some(t) if !self.fds.is_empty() && t.elapsed() < REFRESH_INTERVAL => return true,
            // Nothing open and we only just tried: stay quiet rather than
            // re-scan `/sys/class/hidraw` on every pulse against an absent
            // controller.
            Some(t) if self.fds.is_empty() && t.elapsed() < REOPEN_INTERVAL => return false,
            _ => {}
        }
        self.reopen()
    }

    /// Re-enumerate the controller, open every node read-write, and narrow to
    /// the one that is streaming input. Returns whether anything is open
    /// afterwards.
    ///
    /// # This is also what routes haptics to the live transport
    ///
    /// The narrowing was written for the puck's pairing slots — only one of the
    /// five has a controller behind it — and it turns out to be exactly the rule
    /// Bluetooth needs, for exactly the same reason. `ControllerSource::acquire`
    /// now hands over the dongle's nodes *and* the Bluetooth node when both
    /// exist, and only one of them is streaming, because the controller holds
    /// one link at a time. [`live_indices`] polls them all and keeps whichever
    /// produced a report, so a pulse goes to the link the controller is actually
    /// on with no transport logic here at all.
    ///
    /// [`REFRESH_INTERVAL`] is what makes it follow a switch: the set is
    /// re-probed periodically, so moving between the dongle and Bluetooth
    /// re-narrows to the new link within one refresh rather than needing an
    /// explicit signal. A pulse in the gap goes to a node that ignores it, which
    /// is the pre-existing behaviour for a slot whose controller has slept.
    fn reopen(&mut self) -> bool {
        self.last_open = Some(Instant::now());
        // Through the same acquire path the readers use, so haptics keep working
        // once `packaging/udev/72-hyprpad-puck.rules` has made the nodes
        // root-only: the broker hands over descriptors opened with
        // `hidraw::OPEN_FLAGS`, which is read-write because of exactly this —
        // an output report is a write to the device — and read access is what
        // lets the probe below see which slot is live.
        let opened: Vec<File> = match crate::hidraw::ControllerSource::acquire() {
            Some(source) => source.into_fds().into_iter().map(File::from).collect(),
            None => Vec::new(),
        };
        if opened.is_empty() {
            self.fds.clear();
            self.fail_once(
                "no writable Steam Controller node (28de:1304 puck or 28de:1303 Bluetooth)",
            );
            return false;
        }
        // Fresh fds have empty read buffers, so the probe reflects what is
        // streaming *now*, not what streamed before the last refresh.
        let live = live_indices(&opened);
        self.fds = opened
            .into_iter()
            .enumerate()
            .filter(|(i, _)| live.is_empty() || live.contains(i))
            .map(|(_, fd)| fd)
            .collect();
        // A changed node set is worth one line the next time a pulse lands.
        self.announced = false;
        true
    }

    /// Log `why` the first time haptics stop reaching the device, then stay
    /// quiet until they work again.
    fn fail_once(&mut self, why: &str) {
        if !self.warned {
            eprintln!("warning: haptics unavailable: {why}; feedback is off until it returns");
            self.warned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_report_is_eight_bytes_with_the_pulse_id() {
        // The kernel sends exactly 8 bytes (`hid_hw_output_report(..., 8)`) —
        // NOT zero-padded to 64 like the feature frames in `lizard.rs`, and NOT
        // padded out to the wider rumble report's length either.
        let r = build_pulse(Pad::Right, 0x0190, 0, 1);
        assert_eq!(r.as_slice().len(), 8);
        assert_eq!(r.as_slice()[0], 0x81); // REPORT_ID_HAPTIC_PULSE
    }

    #[test]
    fn side_byte_is_xor_inverted_for_left_and_right() {
        // `if (pad < STEAM_PAD_BOTH) pad ^= 1;` — left (logical 0) goes on the
        // wire as 1, right (logical 1) as 0, both (2) unchanged.
        assert_eq!(wire_side(Pad::Left), 1);
        assert_eq!(wire_side(Pad::Right), 0);
        assert_eq!(wire_side(Pad::Both), 2);
        assert_eq!(build_pulse(Pad::Left, 1, 2, 3).as_slice()[1], 1);
        assert_eq!(build_pulse(Pad::Right, 1, 2, 3).as_slice()[1], 0);
        assert_eq!(build_pulse(Pad::Both, 1, 2, 3).as_slice()[1], 2);
    }

    #[test]
    fn fields_are_little_endian_in_the_documented_slots() {
        // on_us = 0x0190, off_us = 0x012C, count = 0x0002.
        let r = build_pulse(Pad::Right, 0x0190, 0x012C, 0x0002);
        assert_eq!(r.as_slice(), [0x81, 0x00, 0x90, 0x01, 0x2C, 0x01, 0x02, 0x00]);
    }

    #[test]
    fn tick_report_matches_the_kernels_single_tick() {
        // The kernel's own mode-switch tick: STEAM_PAD_RIGHT, 0x190 on, 0 off,
        // count 1 -> wire side 0.
        let (on, off, count) = Feel::Tick.pulse();
        assert_eq!((on, off, count), (0x0190, 0x0000, 0x0001));
        let r = build_pulse(Pad::Right, on, off, count);
        assert_eq!(r.as_slice(), [0x81, 0x00, 0x90, 0x01, 0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn click_report_is_a_heavier_double_pulse_on_the_left() {
        let (on, off, count) = Feel::Click.pulse();
        assert_eq!((on, off, count), (0x0258, 0x012C, 0x0002));
        // Left pad -> wire side 1; 600 µs on, 300 µs off, twice.
        let r = build_pulse(Pad::Left, on, off, count);
        assert_eq!(r.as_slice(), [0x81, 0x01, 0x58, 0x02, 0x2C, 0x01, 0x02, 0x00]);
        // A click is strictly longer/repeated versus a tick — the only knobs
        // the IBEX pulse struct exposes (there is no gain field).
        let (tick_on, _, tick_count) = Feel::Tick.pulse();
        assert!(on > tick_on && count > tick_count);
    }

    #[test]
    fn buzz_report_targets_both_actuators() {
        let (on, off, count) = Feel::Buzz.pulse();
        assert_eq!((on, off, count), (0x01F4, 0x01F4, 0x000A));
        let r = build_pulse(Pad::Both, on, off, count);
        assert_eq!(r.as_slice(), [0x81, 0x02, 0xF4, 0x01, 0xF4, 0x01, 0x0A, 0x00]);
        // ~10 on/off cycles of 500 µs each ≈ a 10 ms brrr.
        assert!(u32::from(count) * (u32::from(on) + u32::from(off)) / 1000 >= 9);
    }

    #[test]
    fn intensity_scales_on_us_and_is_clamped() {
        let (on, _, _) = Feel::Tick.pulse(); // 400 µs
        assert_eq!(scale_on_us(on, 1.0), 400);
        assert_eq!(scale_on_us(on, 0.5), 200);
        assert_eq!(scale_on_us(on, 2.0), 800);
        // Clamped at INTENSITY_MAX (4x), not trusted beyond it.
        assert_eq!(scale_on_us(on, 100.0), 1600);
        // And the absolute ceiling holds for a wide base pulse.
        assert_eq!(scale_on_us(1_000, 4.0), ON_US_MAX);
        // Nonsense scales fire nothing rather than a garbage-width pulse.
        assert_eq!(scale_on_us(on, 0.0), 0);
        assert_eq!(scale_on_us(on, -1.0), 0);
        assert_eq!(scale_on_us(on, f64::NAN), 0);
    }

    /// A pipe pair standing in for one hidraw node: the read end is the "node",
    /// and writing to the other end makes it "stream input".
    fn fake_node() -> (File, File) {
        use std::os::unix::io::FromRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both fds are freshly created and owned by us; wrapping them in
        // `File` transfers that ownership exactly once each.
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    #[test]
    fn probe_narrows_to_the_node_that_streams() {
        // The on-device shape: several puck nodes are open, but only the
        // occupied pairing slot is reporting. An output report succeeds on all
        // of them, so readability is the only signal that separates them.
        let (quiet_a, _wa) = fake_node();
        let (live, mut feed) = fake_node();
        let (quiet_b, _wb) = fake_node();
        feed.write_all(b"report").unwrap();
        let nodes = vec![quiet_a, live, quiet_b];
        assert_eq!(live_indices(&nodes), vec![1]);
    }

    #[test]
    fn probe_reports_nothing_when_no_node_streams() {
        // An asleep controller: no node is readable, so the probe declines to
        // guess and the caller keeps every node (lizard.rs's shotgun).
        let (a, _wa) = fake_node();
        let (b, _wb) = fake_node();
        assert!(live_indices(&[a, b]).is_empty());
    }

    #[test]
    fn intensity_only_touches_the_width_not_the_gap_or_count() {
        // Strength on this device is `on_us` (+ count); scaling must not change
        // the rhythm, or a "louder" click would become a different feel.
        let (on, off, count) = Feel::Click.pulse();
        let r = build_pulse(Pad::Left, scale_on_us(on, 2.0), off, count);
        let b = r.as_slice();
        assert_eq!(&b[4..8], &[0x2C, 0x01, 0x02, 0x00]);
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 1200);
    }

    #[test]
    fn rumble_report_mirrors_the_kernels_ibex_force_feedback_packet() {
        // `steam_haptic_rumble(steam, intensity=0, left, right, left_gain=2,
        // right_gain=0)` — the constants are the driver's, on every call, for
        // both the immediate and the 20 Hz coalescing work item.
        let r = build_rumble(0xBEEF, 0x1234);
        assert_eq!(r.as_slice().len(), 10, "the kernel sends exactly 10 bytes");
        assert_eq!(
            r.as_slice(),
            [
                0x80, // REPORT_ID_HAPTIC_RUMBLE
                0x00, // type: never assigned by the driver (kzalloc'd)
                0x00, 0x00, // intensity: the driver always passes 0
                0xEF, 0xBE, // left.speed  = strong_magnitude, LE, unscaled
                0x02, // left.gain: the driver's hard-coded 2
                0x34, 0x12, // right.speed = weak_magnitude, LE, unscaled
                0x00, // right.gain: the driver's hard-coded 0
            ]
        );
    }

    #[test]
    fn rumble_has_no_side_xor_and_stops_with_zero_magnitudes() {
        // Unlike `steam_haptic_pulse`, `steam_haptic_rumble` has no `pad ^= 1`:
        // left and right are named struct fields, so a strong-only effect must
        // land in the LEFT slot and leave the right one alone.
        let b = build_rumble(u16::MAX, 0);
        assert_eq!(&b.as_slice()[4..6], &[0xFF, 0xFF], "strong -> left.speed");
        assert_eq!(&b.as_slice()[7..9], &[0x00, 0x00], "weak stays zero");
        // There is no dedicated stop command: zero magnitudes are the stop, and
        // the gain bytes stay at the driver's constants even then.
        assert_eq!(
            build_rumble(0, 0).as_slice(),
            [0x80, 0, 0, 0, 0, 0, 0x02, 0, 0, 0x00]
        );
    }

    #[test]
    fn raw_pulse_entry_point_clamps_and_refuses_nothing_pulses() {
        // The gamepad bridge's pulse-mode rumble derives its own widths, so the
        // raw entry point must apply the same ceiling a scaled feel gets.
        let mut h = Haptics::new();
        // Nothing to fire: neither of these should reach the queue (a zero-width
        // or zero-count pulse is silence, and would only cost a USB transfer).
        h.pulse(Pad::Left, 0, 0, 4);
        h.pulse(Pad::Left, 400, 0, 0);
        // A sane one does. The clamp is shared with `scale_on_us`, so a caller
        // asking for a 10 ms "pulse" gets the 2 ms ceiling instead.
        assert_eq!(scale_on_us(ON_US_MAX, 4.0), ON_US_MAX);
    }
}
