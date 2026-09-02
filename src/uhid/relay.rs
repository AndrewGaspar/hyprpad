//! The running relay: a 250 Hz input stream into the virtual device, and the
//! interpretation of what Steam writes back.
//!
//! This is `gamepad.rs`'s opposite number. Both are *game sinks*, both are fed
//! only when `run::gamepad_forwarding` says a game should get input, and both
//! guarantee the same thing on the falling edge of that gate: the sink is left
//! neutral, never holding a stuck stick. Only one is ever created — a config
//! with `kind = "steam"` does not also make the Xbox pad, because Steam would
//! then see two controllers and a game would double-count every press.
//!
//! # The streaming rule, and why it is unconditional
//!
//! **The device emits an input report every 4 ms from the moment it is created,
//! whether or not anything moved.** This is not an optimisation to be undone; it
//! is load-bearing, and the single reason the first (passive) probe was ignored
//! while the second was adopted:
//!
//! * InputPlumber's `SteamDeckUhidDevice::poll()` calls `write_state()`
//!   unconditionally on every 4 ms tick — the wire write happens on the timer,
//!   not on input change (`docs/research/uhid-steam-controller.md` A.2).
//! * SDL's Deck backend hard-gates detection on a live report:
//!   `SDL_hid_read_timeout(dev, data, sizeof(data), 16); if (size == 0) return
//!   false;` — no report within 16 ms and the device is rejected outright.
//! * The proven run streamed 250 Hz from create and Steam adopted it; the
//!   earlier passive fake streamed nothing and Steam never engaged.
//!
//! So the streamer is a thread with its own clock, and the daemon's frame path
//! only ever *updates what it streams*. That also gives two properties for free:
//!
//! * **A silent puck is invisible to Steam.** When the controller naps, hyprpad
//!   keeps the fake alive on neutral reports. Steam must never see a disconnect
//!   — a create/destroy cycle makes it re-detect, re-apply configs and toast.
//!   §5.3 is explicit: the device is created at daemon start and destroyed at
//!   exit, *never* torn down on a focus change.
//! * **Not-forwarding is just another state to stream.** Ranks 1, 2 and 4 of the
//!   precedence (desktop, OSK, guide held) stream neutral, so Steam sees a
//!   connected-but-idle controller rather than a vanished one.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::report::Frame;
use crate::uhid::profile::{Profile, ReportKind};
use crate::uhid::settings::{self, Action};
use crate::uhid::translate::{self, StripMask, DECK_REPORT_LEN, TRITON_REPORT_LEN};
use crate::uhid::{UhidDevice, UhidSender};

/// The stream period: 4 ms, i.e. 250 Hz.
///
/// InputPlumber's `deck-uhid` target driver is configured with
/// `poll_rate: Duration::from_millis(4)`, and the puck itself streams at about
/// the same rate, so a forwarded frame is never stale by more than one tick.
pub const STREAM_PERIOD: Duration = Duration::from_millis(4);

/// Longest a report is streamed unchanged before the streamer starts sending
/// neutral instead.
///
/// The relay's state is updated by the daemon's frame path; if the puck stops —
/// it sleeps, or its hidraw readers end — that path simply stops running, and
/// the last live frame would otherwise be repeated forever. A game must not be
/// left holding whatever was pressed at the moment the controller napped, so
/// after this long without an update the stream falls back to neutral, with the
/// device still very much alive.
///
/// Generous relative to the puck's ~250 Hz: this is a "the controller is gone"
/// timer, not a jitter budget.
pub const STALE_AFTER: Duration = Duration::from_millis(200);

/// The report currently being streamed.
#[derive(Clone, Copy)]
enum Streamed {
    /// Nothing to forward: stream this profile's neutral report.
    Neutral,
    /// A live frame, and when it was set (for [`STALE_AFTER`]).
    Live([u8; DECK_REPORT_LEN], usize, Instant),
}

/// A live virtual Valve controller fed from the real puck.
pub struct SteamRelay {
    device: UhidDevice,
    profile: &'static Profile,
    state: Arc<Mutex<Streamed>>,
    /// The stream's own sequence counter, bumped once per tick exactly as
    /// InputPlumber bumps `frame.wrapping_add(1)` on every write.
    seq: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl SteamRelay {
    /// Create the virtual device on `fd` and start streaming immediately.
    ///
    /// The fd is injected; see `uhid::acquire_uhid` for the boundary this sits
    /// behind. Streaming starts before `UHID_START` has necessarily arrived,
    /// which is correct and deliberate: that event is delivered asynchronously
    /// from a workqueue, and a driver doing I/O in `.probe` deadlocks for five
    /// seconds against anyone who waits for it.
    pub fn start(fd: OwnedFd, profile: &'static Profile) -> io::Result<SteamRelay> {
        let device = UhidDevice::create(fd, profile)?;
        let relay = SteamRelay {
            device,
            profile,
            state: Arc::new(Mutex::new(Streamed::Neutral)),
            seq: Arc::new(AtomicU32::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        relay.spawn_streamer()?;
        Ok(relay)
    }

    fn spawn_streamer(&self) -> io::Result<()> {
        let device = self.device.sender();
        let state = Arc::clone(&self.state);
        let seq = Arc::clone(&self.seq);
        let stop = Arc::clone(&self.stop);
        let profile = self.profile;
        std::thread::Builder::new()
            .name("uhid-stream".to_string())
            .spawn(move || stream_loop(&device, profile, &state, &seq, &stop))?;
        Ok(())
    }

    /// Hand the relay one puck frame to stream, because a game should have it.
    ///
    /// `raw` is the report exactly as it came off hidraw — the triton profile
    /// forwards those bytes untouched but for the strip mask, so the caller must
    /// not pre-process them. `frame` is the same report decoded, which the deck
    /// profile transcodes.
    pub fn forward(&self, raw: &[u8], frame: &Frame, strip: StripMask) {
        let next = match self.profile.kind {
            ReportKind::Triton => match translate::puck_to_triton(raw, strip) {
                Some(r) => {
                    let mut buf = [0u8; DECK_REPORT_LEN];
                    buf[..TRITON_REPORT_LEN].copy_from_slice(&r);
                    Streamed::Live(buf, TRITON_REPORT_LEN, Instant::now())
                }
                // Not a 0x42 — a battery or status report. Leave the stream as
                // it is rather than dropping the game to neutral.
                None => return,
            },
            ReportKind::Deck => {
                let mut r = translate::puck_to_deck(frame, self.seq.load(Ordering::Relaxed));
                translate::strip_deck(&mut r, strip);
                Streamed::Live(r, DECK_REPORT_LEN, Instant::now())
            }
        };
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = next;
    }

    /// Stop forwarding: stream neutral from the next tick.
    ///
    /// Called on the falling edge of the forwarding gate, when the controller
    /// disconnects, and on the way out — the same three moments
    /// `gamepad::VirtualGamepad::neutral` covers. The device stays created.
    pub fn release(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = Streamed::Neutral;
    }

    /// Take and interpret everything Steam has written since the last call.
    ///
    /// Called from the daemon's loop, which owns the `Haptics` handle these
    /// actions drive. Interpretation is deliberately *not* done on the event
    /// thread: that thread's one job is answering the kernel inside its five
    /// second budget.
    pub fn drain_actions(&self) -> Vec<Action> {
        self.device.drain_writes().iter().map(settings::classify).collect()
    }

    /// Whether Steam currently holds the virtual device open.
    pub fn is_open(&self) -> bool {
        self.device.is_open()
    }

    /// The identity being presented.
    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    /// `dev_flags` from `UHID_START`, once it has arrived. Exposed for the
    /// startup log: it is the empirical answer to "does this profile number its
    /// reports", which decides the prefixing `translate` applies.
    pub fn dev_flags(&self) -> Option<u64> {
        self.device.dev_flags()
    }
}

impl Drop for SteamRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Write one report every [`STREAM_PERIOD`], forever.
fn stream_loop(
    device: &UhidSender,
    profile: &'static Profile,
    state: &Arc<Mutex<Streamed>>,
    seq: &Arc<AtomicU32>,
    stop: &Arc<AtomicBool>,
) {
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
            continue;
        }
        let n = seq.fetch_add(1, Ordering::Relaxed);
        let current = *state.lock().unwrap_or_else(|e| e.into_inner());
        let report = tick_report(profile, current, n, now);
        if let Err(e) = device.send_input(&report) {
            // The device is gone — the fd closed, or the kernel tore it down.
            // Nothing is recoverable from in here; re-creating it needs a fresh
            // descriptor, which only the caller can obtain. Say so rather than
            // going quiet, because from Steam's side a stream that stops is
            // indistinguishable from a controller that died.
            //
            // The ordinary shutdown path sets `stop` first, so this only ever
            // logs when the device really did fail under us.
            if !stop.load(Ordering::Relaxed) {
                eprintln!(
                    "warning: virtual Steam Controller stream ended ({e}); \
                     Steam will see it stop responding"
                );
            }
            return;
        }
        next += STREAM_PERIOD;
        // Behind by more than a dozen ticks: resync rather than spin trying to
        // catch up, exactly as the proven probe's sender does.
        if now.saturating_duration_since(next) > Duration::from_millis(50) {
            next = now + STREAM_PERIOD;
        }
    }
}

/// The bytes for one tick: pure, so the whole streaming rule is testable.
///
/// The two profiles renumber differently, and both follow their own protocol:
///
/// * **deck** — the `frame` field is re-stamped on every write, because
///   InputPlumber bumps it per write rather than per input change. A repeated
///   report therefore still advances, which is what makes the stream read as
///   live rather than stuck.
/// * **triton** — a live report's byte-1 counter is **relayed untouched** (§4.4:
///   renumbering risks desync with the IMU timestamp path, and Steam tolerates
///   gaps). Only the synthesized neutral gets a counter of our own.
fn tick_report(
    profile: &'static Profile,
    current: Streamed,
    seq: u32,
    now: Instant,
) -> Vec<u8> {
    let live = match current {
        Streamed::Live(buf, len, at) if now.saturating_duration_since(at) < STALE_AFTER => {
            Some((buf, len))
        }
        _ => None,
    };
    match (profile.kind, live) {
        (ReportKind::Deck, Some((buf, len))) => {
            let mut out = buf[..len].to_vec();
            // Re-stamp bytes 4..8 with this tick's counter.
            out[4..8].copy_from_slice(&seq.to_le_bytes());
            out
        }
        (ReportKind::Deck, None) => translate::deck_neutral(seq).to_vec(),
        (ReportKind::Triton, Some((buf, len))) => buf[..len].to_vec(),
        (ReportKind::Triton, None) => translate::triton_neutral(seq as u8).to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uhid::profile;

    fn raw_0x42(counter: u8) -> Vec<u8> {
        let mut raw = vec![0u8; TRITON_REPORT_LEN];
        raw[0] = 0x42;
        raw[1] = counter;
        raw[2] |= 1 << 0; // Button::A
        raw
    }

    fn live(bytes: &[u8], at: Instant) -> Streamed {
        let mut buf = [0u8; DECK_REPORT_LEN];
        buf[..bytes.len()].copy_from_slice(bytes);
        Streamed::Live(buf, bytes.len(), at)
    }

    #[test]
    fn a_silent_relay_streams_neutral_rather_than_nothing() {
        let now = Instant::now();
        assert_eq!(
            tick_report(profile::deck(), Streamed::Neutral, 5, now),
            translate::deck_neutral(5).to_vec()
        );
        assert_eq!(
            tick_report(profile::triton(), Streamed::Neutral, 5, now),
            translate::triton_neutral(5).to_vec()
        );
    }

    #[test]
    fn the_deck_frame_counter_advances_on_every_tick_even_when_nothing_moved() {
        let now = Instant::now();
        let f = Frame::default();
        let held = live(&translate::puck_to_deck(&f, 0), now);
        let a = tick_report(profile::deck(), held, 100, now);
        let b = tick_report(profile::deck(), held, 101, now);
        assert_ne!(a, b, "a repeated report still advances");
        assert_eq!(&a[4..8], &100u32.to_le_bytes());
        assert_eq!(&b[4..8], &101u32.to_le_bytes());
        assert_eq!(&a[8..], &b[8..], "and nothing else changes");
    }

    #[test]
    fn a_triton_report_is_streamed_with_its_own_counter_untouched() {
        let now = Instant::now();
        let raw = translate::puck_to_triton(&raw_0x42(0xa7), StripMask::guide_only()).unwrap();
        let held = live(&raw, now);
        let out = tick_report(profile::triton(), held, 999, now);
        assert_eq!(out.len(), TRITON_REPORT_LEN);
        assert_eq!(out[1], 0xa7, "the puck's counter is relayed, never renumbered");
        assert_eq!(out, raw.to_vec());
        // Two ticks of the same frame are byte-identical.
        assert_eq!(tick_report(profile::triton(), held, 1000, now), out);
    }

    #[test]
    fn a_stale_frame_decays_to_neutral_so_no_game_is_left_holding_a_button() {
        let set_at = Instant::now();
        let raw = translate::puck_to_triton(&raw_0x42(3), StripMask::none()).unwrap();
        let held = live(&raw, set_at);
        // Fresh: relayed.
        let fresh = tick_report(profile::triton(), held, 0, set_at);
        assert_ne!(fresh[2] & 0x01, 0, "A is still pressed");
        // Long after the puck went quiet: neutral, and the device stays alive.
        let later = set_at + STALE_AFTER + Duration::from_millis(1);
        let stale = tick_report(profile::triton(), held, 7, later);
        assert_eq!(stale, translate::triton_neutral(7).to_vec());
        assert_eq!(stale[2] & 0x01, 0, "nothing is left pressed");

        let deck_held = live(&translate::puck_to_deck(&Frame::default(), 0), set_at);
        assert_eq!(
            tick_report(profile::deck(), deck_held, 7, later),
            translate::deck_neutral(7).to_vec()
        );
    }

    #[test]
    fn the_stream_period_is_the_reference_implementations_250_hz() {
        assert_eq!(STREAM_PERIOD, Duration::from_millis(4));
        assert!(STALE_AFTER > STREAM_PERIOD * 10, "a gone-away timer, not a jitter budget");
    }

    /// The relay's `forward` chooses a path by profile; check both land the
    /// right bytes in the streamed state without needing a device.
    #[test]
    fn forward_stores_the_profiles_own_report_shape() {
        let raw = raw_0x42(0x11);
        let frame = Frame::decode(&raw).unwrap();

        // triton: the raw bytes, masked.
        let masked = translate::puck_to_triton(&raw, StripMask::guide_only()).unwrap();
        let held = live(&masked, Instant::now());
        let out = tick_report(profile::triton(), held, 0, Instant::now());
        assert_eq!(out.len(), TRITON_REPORT_LEN);
        assert_eq!(out[0], 0x42);

        // deck: a transcode, 64 bytes with the reference header.
        let mut d = translate::puck_to_deck(&frame, 0);
        translate::strip_deck(&mut d, StripMask::guide_only());
        let held = live(&d, Instant::now());
        let out = tick_report(profile::deck(), held, 0, Instant::now());
        assert_eq!(out.len(), DECK_REPORT_LEN);
        assert_eq!(&out[..4], &[0x01, 0x00, 0x09, 0x40]);
        assert_eq!(out[8] & 0x80, 0x80, "A survived the transcode");
    }
}
