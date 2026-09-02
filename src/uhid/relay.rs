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

    // -----------------------------------------------------------------------
    // Neutral-frame stability — issue #35, and what it turned out to be
    // -----------------------------------------------------------------------
    //
    // With the fake adopted, `~/.local/share/Steam/logs/controller.txt` churns,
    // and the first suspicion was that something in *our* stream was toggling
    // under Steam. It is not, and these tests are what pins that down; the
    // measurement that settled it is recorded in `docs/design/uhid-relay.md`
    // §4.1. The rule they enforce is one sentence:
    //
    //   **Between two neutral ticks, the sequence counter is the only byte that
    //   may differ, and it may only advance by one.**
    //
    // Everything a host could read as an event — a battery level, a touch bit,
    // a "connected" flag, a timestamp — must be *absent*, not merely constant,
    // because a byte that is constant today is a byte someone can make toggle
    // tomorrow without noticing what it costs.

    /// How many ticks the stability tests walk. Long enough to wrap the
    /// triton profile's `u8` counter more than once, which is the only place
    /// the two profiles' counters can behave differently.
    const STABILITY_TICKS: u32 = 600;

    /// The whole of issue #35's "is it us?", for the profile Steam actually
    /// adopted: 600 consecutive neutral ticks, and byte 1 is the only thing
    /// that moves.
    #[test]
    fn six_hundred_neutral_triton_ticks_differ_only_in_the_sequence_counter() {
        let now = Instant::now();
        let mut prev: Option<Vec<u8>> = None;
        for n in 0..STABILITY_TICKS {
            let r = tick_report(profile::triton(), Streamed::Neutral, n, now);
            assert_eq!(r.len(), TRITON_REPORT_LEN);
            assert_eq!(r[0], 0x42, "the report id never changes");
            assert_eq!(r[1], (n & 0xff) as u8, "byte 1 is the counter, and only that");
            assert!(
                r[2..].iter().all(|&b| b == 0),
                "tick {n}: everything but the id and the counter is zero"
            );
            if let Some(p) = &prev {
                let differing: Vec<usize> =
                    (0..r.len()).filter(|&i| p[i] != r[i]).collect();
                assert_eq!(differing, vec![1], "tick {n}: exactly one byte may differ");
                assert_eq!(
                    r[1].wrapping_sub(p[1]),
                    1,
                    "tick {n}: the counter advances by exactly one, wrapping"
                );
            }
            prev = Some(r);
        }
    }

    /// The same rule for the deck profile, whose counter is a `u32` at 4..8 and
    /// whose header must likewise never move.
    #[test]
    fn six_hundred_neutral_deck_ticks_differ_only_in_the_frame_counter() {
        let now = Instant::now();
        let mut prev: Option<Vec<u8>> = None;
        for n in 0..STABILITY_TICKS {
            let r = tick_report(profile::deck(), Streamed::Neutral, n, now);
            assert_eq!(r.len(), DECK_REPORT_LEN);
            assert_eq!(&r[..4], &[0x01, 0x00, 0x09, 0x40], "the header never changes");
            assert_eq!(&r[4..8], &n.to_le_bytes());
            assert!(r[8..].iter().all(|&b| b == 0), "tick {n}: nothing else is set");
            if let Some(p) = &prev {
                let differing: Vec<usize> =
                    (0..r.len()).filter(|&i| p[i] != r[i]).collect();
                assert!(
                    differing.iter().all(|&i| (4..8).contains(&i)),
                    "tick {n}: only the frame counter may differ, saw {differing:?}"
                );
            }
            prev = Some(r);
        }
    }

    /// A neutral report carries **no state at all** beyond its counter — which
    /// is the structural reason the stability above holds rather than a
    /// coincidence of today's field assignments.
    ///
    /// Named individually so a future field lands on a failing assertion rather
    /// than quietly becoming a thing that toggles under Steam.
    #[test]
    fn a_neutral_report_has_no_flag_or_reading_that_could_toggle() {
        let r = translate::triton_neutral(0x5a);

        // Every button/touch byte of the puck's 0x42 (report.rs BUTTON_BITS
        // spans bytes 2..6) is clear: no pad-touch bit, no capacitive bit, no
        // trigger-full bit.
        assert!(r[2..6].iter().all(|&b| b == 0), "no button or touch bit is set");
        // Analog channels: triggers, sticks, both pads and both pad forces.
        assert!(r[6..30].iter().all(|&b| b == 0), "every analog channel is centred at zero");
        // The IMU window and the undecoded tail. This is also what makes
        // "not forwarding parks the gyro" true from Steam's side (§3.1): while
        // hyprpad owns the controller, the gyro Steam sees is stationary
        // whatever the firmware is doing.
        assert!(r[30..].iter().all(|&b| b == 0), "the IMU window and tail are zero");
        // And there is no battery, timestamp or "connected" field anywhere,
        // because there is nothing non-zero left to be one.
        assert_eq!(r.iter().filter(|&&b| b != 0).count(), 2, "only the id and the counter");
    }

    /// The counter wraps cleanly rather than sticking or jumping at the `u8`
    /// boundary — the one arithmetic edge in the neutral path.
    #[test]
    fn the_triton_neutral_counter_wraps_without_a_stutter() {
        let now = Instant::now();
        let at = |n: u32| tick_report(profile::triton(), Streamed::Neutral, n, now)[1];
        assert_eq!(at(254), 254);
        assert_eq!(at(255), 255);
        assert_eq!(at(256), 0, "wraps to zero");
        assert_eq!(at(257), 1);
        // Never repeats within one lap: 256 ticks, 256 distinct values.
        let lap: std::collections::HashSet<u8> = (0..256).map(at).collect();
        assert_eq!(lap.len(), 256);
    }

    /// **Deliberate, and the one place a repeated counter can occur.**
    ///
    /// A live triton frame is relayed byte for byte, counter included (§4.4:
    /// renumbering risks desync with the IMU timestamp path — which now matters
    /// *more*, not less, since the gyro rides in the same report). So while the
    /// puck's frame path is between updates, the identical report is re-sent,
    /// counter and all, for up to [`STALE_AFTER`].
    ///
    /// That is not the churn: it is bounded at 200 ms, and Steam tolerates a
    /// repeated counter exactly as it tolerates a gap. This test exists so the
    /// behaviour is a decision on the record rather than an accident.
    #[test]
    fn a_held_live_frame_repeats_its_own_counter_rather_than_being_renumbered() {
        let now = Instant::now();
        let raw = translate::puck_to_triton(&raw_0x42(0x77), StripMask::guide_only()).unwrap();
        let held = live(&raw, now);
        let a = tick_report(profile::triton(), held, 10, now);
        let b = tick_report(profile::triton(), held, 11, now);
        let c = tick_report(profile::triton(), held, 12, now);
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a[1], 0x77, "the puck's counter, not the streamer's tick");
    }

    /// Crossing from a live frame to neutral changes the counter's *source*,
    /// which shows up as one discontinuity — and nothing else moves with it.
    ///
    /// Recorded because a jump there is the one thing in the stream that could
    /// look like an event to a host, and it is bounded to one per transition
    /// (a handful per session), not the tens per second the log churns at.
    #[test]
    fn the_live_to_neutral_transition_is_one_counter_discontinuity_and_nothing_else() {
        let set_at = Instant::now();
        let raw = translate::puck_to_triton(&raw_0x42(0x77), StripMask::none()).unwrap();
        let held = live(&raw, set_at);
        let last_live = tick_report(profile::triton(), held, 40, set_at);
        let after = set_at + STALE_AFTER + Duration::from_millis(1);
        let first_neutral = tick_report(profile::triton(), held, 41, after);

        assert_eq!(last_live[1], 0x77);
        assert_eq!(first_neutral[1], 41, "the streamer's own counter takes over");
        assert_eq!(first_neutral, translate::triton_neutral(41).to_vec());
        // From there it is stable again: the next tick differs in byte 1 alone.
        let second = tick_report(profile::triton(), held, 42, after);
        let differing: Vec<usize> =
            (0..second.len()).filter(|&i| first_neutral[i] != second[i]).collect();
        assert_eq!(differing, vec![1]);
    }
}
