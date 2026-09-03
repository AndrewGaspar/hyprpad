//! High-level gesture recognition over the decoded controller stream.
//!
//! [`GestureEngine`] consumes `(Frame, Instant)` pairs — one per input report,
//! at the ~4 ms cadence measured in docs/03-hardware-findings.md — and emits
//! [`GestureEvent`]s describing the *guide layer* the living-room vision is
//! built on (docs/08-living-room-vision.md):
//!
//! - the guide (Steam) button opens a desktop layer while it is held;
//! - buttons and stick flicks during that hold are chords, owned by the WM;
//!   a chord button's lift is reported too ([`GestureEvent::GuideChordRelease`])
//!   so a chord can hold an output for as long as it is down;
//! - a *bare* guide tap (no chord during the hold, and nothing the daemon
//!   chose to spend the hold on — [`GestureEngine::consume_hold`]) is reported
//!   as such so the daemon can pass it through to Steam, whose own guide
//!   button acts on release. [`GestureEngine::guide_tap`] narrows that to the
//!   *quick* bare tap — the one thing the daemon either synthesizes onto the
//!   game sink or resolves as the `guide_tap` binding, never both.
//!
//! Timing uses the monotonic [`Instant`] passed by the caller, never the frame
//! counter (which is a wrapping `u8`): the engine is therefore robust to
//! dropped and duplicate frames. All thresholds are `pub const` so they can be
//! tuned without touching the logic.

use crate::report;
use std::time::{Duration, Instant};

/// A cardinal stick-flick direction. `+Y` is up, matching [`report::Frame`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum StickDir {
    Up,
    Down,
    Left,
    Right,
}

/// Which analog stick a flick came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Stick {
    Left,
    Right,
}

/// A recognized high-level gesture.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum GestureEvent {
    /// Guide pressed: the desktop layer is now active.
    GuideEnter,
    /// A button was pressed while the guide was held.
    GuideChord(report::Button),
    /// A button whose press was reported as a [`GuideChord`](Self::GuideChord)
    /// lifted while the guide is **still** held. The other edge of a chord, for
    /// a binding that holds an output for as long as the chord does (a mouse
    /// button on `guide+rpad_click`, a modifier on `guide+l5`). Not a binding
    /// key: nothing resolves against it.
    ///
    /// Reported only for buttons that chorded during *this* hold — a button
    /// already down when the guide went down never chorded, so its lift is
    /// nobody's business — and never for a button that lifts in the same
    /// report as the guide: that frame is a [`GuideLeave`](Self::GuideLeave),
    /// which ends every chord at once.
    GuideChordRelease(report::Button),
    /// A stick was flicked past the flick threshold while the guide was held.
    GuideStickFlick { stick: Stick, dir: StickDir },
    /// The guide crossed [`HOLD_THRESHOLD`] while still held. Emitted once per
    /// hold; a signal that this is a deliberate hold, not a tap in progress.
    ///
    /// Not part of the minimal required set, but cheap and useful (e.g. to
    /// reveal an on-screen layer overlay). A bare hold on its own does *not*
    /// set `was_chorded`.
    GuideHold,
    /// Guide released. Two bits, and they answer different questions.
    ///
    /// `was_chorded == true` means the hold carried at least one chord or
    /// flick — or the daemon spent it on something of its own
    /// ([`GestureEngine::consume_hold`]) — and was consumed by the desktop
    /// layer. Nothing resolves against such a leave.
    ///
    /// `bare_tap` is the *narrow* half: the whole gesture was a quick
    /// press-and-release with nothing in it, the exact decision
    /// [`GestureEngine::guide_tap`] documents (and the same value it returns
    /// for this frame), carried on the event so every consumer asks the same
    /// question. It is what the `guide_tap` binding resolves against
    /// ([`crate::config::GestureKey::Tap`]) and what the daemon replays onto
    /// the game sink as a Steam press. `was_chorded == false` alone is *not*
    /// enough for either: a three-second deliberating hold that ended in
    /// nothing is `was_chorded == false, bare_tap == false`.
    GuideLeave { was_chorded: bool, bare_tap: bool },
}

/// How long the guide must be held to count as a deliberate hold rather than a
/// tap. Measured taps sit under ~200 ms and deliberate holds near ~2300 ms
/// (docs/03), so 300 ms separates them comfortably.
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(300);

/// Longest a bare hold may last and still count as a **tap** for
/// [`GestureEngine::guide_tap`] — the signal the daemon turns into a
/// synthesized guide press on the game sink (`[gamepad] guide_tap`).
///
/// Deliberately looser than [`HOLD_THRESHOLD`], and the two answer different
/// questions. `HOLD_THRESHOLD` is "is this hold deliberate *yet*", asked while
/// the button is still down so an overlay can appear; this one is asked at the
/// release, about the whole gesture, and its job is to keep the owner's
/// *deliberating* case — guide down while deciding which chord to press, then
/// thinking better of it — from opening the Steam overlay. 400 ms leaves room
/// for a slow-but-still-single press without covering a pause for thought.
///
/// The default; `[gamepad] guide_tap_max_ms` overrides it per config
/// ([`GestureEngine::set_guide_tap_max`]).
pub const GUIDE_TAP_MAX: Duration = Duration::from_millis(400);

/// Dominant-axis deflection required to fire a flick: ~60% of the ±32767 range.
pub const FLICK_THRESHOLD: i32 = 19_660;

/// A flicked stick must fall back within this dominant-axis magnitude before
/// another flick can fire (hysteresis). Comfortably above the idle noise floor
/// and below [`FLICK_THRESHOLD`].
pub const RECENTER_THRESHOLD: i32 = 8_000;

/// Idle deadzone: sticks rest within ~±400 counts, so anything inside this is
/// treated as centered (docs/03).
pub const DEADZONE: i32 = 4_000;

// The recenter band must sit at or above the idle deadzone, or a stick resting
// at its idle offset could never re-arm. Checked at compile time.
const _: () = assert!(RECENTER_THRESHOLD >= DEADZONE);

/// Stateful recognizer. Feed every decoded frame to [`update`](Self::update).
pub struct GestureEngine {
    prev: report::Frame,
    guide_active: bool,
    guide_since: Instant,
    was_chorded: bool,
    hold_fired: bool,
    /// The buttons that chorded during the current hold and are still down,
    /// one bit per [`report::Button`] discriminant. A lift reports a
    /// [`GestureEvent::GuideChordRelease`] only for a button in this set.
    chorded: u32,
    /// Per-stick hysteresis: `true` means a fresh flick may fire; `false` means
    /// the stick is still deflected from the last flick and must recenter.
    left_armed: bool,
    right_armed: bool,
    /// A chordable button was **already down** when the guide went down. Such a
    /// button never chords (only a press edge under the guide does), so without
    /// this the hold would end as a bare tap even though the hand was plainly
    /// doing something else. Blocks [`Self::guide_tap`] only — `was_chorded`
    /// keeps its existing meaning.
    tap_blocked: bool,
    /// Whether the frame last handed to [`Self::update`] ended a quick bare
    /// tap. Recomputed every call; see [`Self::guide_tap`].
    guide_tap: bool,
    /// The cap `guide_tap` measures against; [`GUIDE_TAP_MAX`] until the daemon
    /// sets the config's value.
    guide_tap_max: Duration,
}

impl GestureEngine {
    /// Create an engine with no guide held and both sticks armed.
    pub fn new() -> Self {
        Self {
            prev: report::Frame::default(),
            guide_active: false,
            guide_since: Instant::now(),
            was_chorded: false,
            hold_fired: false,
            chorded: 0,
            left_armed: true,
            right_armed: true,
            tap_blocked: false,
            guide_tap: false,
            guide_tap_max: GUIDE_TAP_MAX,
        }
    }

    /// Set the cap a bare hold must fall under to count as a tap
    /// ([`Self::guide_tap`]). The daemon calls this from the live config, so
    /// `hyprpad reload` retunes it on the next report.
    pub fn set_guide_tap_max(&mut self, max: Duration) {
        self.guide_tap_max = max;
    }

    /// Consume one frame at time `now` and return the gestures it produced
    /// (usually none). `now` must be monotonic across calls.
    pub fn update(&mut self, frame: &report::Frame, now: Instant) -> Vec<GestureEvent> {
        let mut events = Vec::new();
        let guide_now = frame.pressed(report::Button::Steam);
        // One frame's worth of signal: true only for the call that saw the
        // release, never carried over.
        self.guide_tap = false;

        // Entering the guide layer.
        if guide_now && !self.guide_active {
            self.guide_active = true;
            self.guide_since = now;
            self.was_chorded = false;
            self.hold_fired = false;
            self.chorded = 0;
            // Only arm a stick that is already centered, so pressing guide with
            // a stick already deflected does not fire a spurious flick.
            self.left_armed = Self::centered(frame.left_stick);
            self.right_armed = Self::centered(frame.right_stick);
            // Whatever the hand was already holding. `edges_down` against an
            // empty frame is "every button down right now", using the same
            // public accessor a real edge goes through.
            self.tap_blocked = frame
                .edges_down(&report::Frame::default())
                .any(Self::chordable);
            events.push(GestureEvent::GuideEnter);
        }

        // While the guide is held: chords, their releases, flicks, and the hold
        // threshold.
        if guide_now {
            // Releases before presses, so a button that is (impossibly) both
            // released and pressed in one report ends the old chord first.
            for b in frame.edges_up(&self.prev) {
                if self.chorded & Self::bit(b) != 0 {
                    self.chorded &= !Self::bit(b);
                    events.push(GestureEvent::GuideChordRelease(b));
                }
            }
            for b in frame.edges_down(&self.prev) {
                if Self::chordable(b) {
                    events.push(GestureEvent::GuideChord(b));
                    self.was_chorded = true;
                    self.chorded |= Self::bit(b);
                }
            }
            for (stick, axis) in [
                (Stick::Left, frame.left_stick),
                (Stick::Right, frame.right_stick),
            ] {
                if let Some(dir) = self.flick(stick, axis) {
                    events.push(GestureEvent::GuideStickFlick { stick, dir });
                    self.was_chorded = true;
                }
            }
            if !self.hold_fired
                && now.saturating_duration_since(self.guide_since) >= HOLD_THRESHOLD
            {
                self.hold_fired = true;
                events.push(GestureEvent::GuideHold);
            }
        }

        // Leaving the guide layer. Every chord ends here, whether or not its
        // button is still down: the leave is the release for all of them, so
        // no per-button release is reported for this frame.
        if !guide_now && self.guide_active {
            self.guide_active = false;
            self.chorded = 0;
            // The quick-bare-tap signal, decided here and nowhere else: nothing
            // recognised, nothing the daemon spent the hold on, nothing already
            // held when it started, and short enough not to be a deliberation.
            self.guide_tap = !self.was_chorded
                && !self.tap_blocked
                && now.saturating_duration_since(self.guide_since) <= self.guide_tap_max;
            events.push(GestureEvent::GuideLeave {
                was_chorded: self.was_chorded,
                // The same bit, on the event: a consumer that only ever sees
                // the event (`Config::resolve`) must not have to ask the
                // engine, and must not be able to mistake a *broad* bare leave
                // for a tap.
                bare_tap: self.guide_tap,
            });
        }

        self.prev = *frame;
        events
    }

    /// Whether the guide layer is currently active (guide held).
    pub fn guide_active(&self) -> bool {
        self.guide_active
    }

    /// Whether the frame just handed to [`Self::update`] ended a **quick bare
    /// tap** of the guide: press, release, and nothing in between.
    ///
    /// The narrow half of a bare `GuideLeave`, and the value its
    /// [`bare_tap`](GestureEvent::GuideLeave) field carries. All four of
    /// these must hold, and each rules out a hold the owner meant as something
    /// else:
    ///
    /// * no chord or flick was recognised during the hold (`was_chorded`);
    /// * the daemon did not spend the hold itself ([`Self::consume_hold`] —
    ///   the guide-mouse and the caret scrub);
    /// * no chordable button was already down when the guide went down (such a
    ///   button cannot chord, having no press edge under the guide, but the
    ///   hand is plainly mid-something);
    /// * the whole hold fits inside [`GUIDE_TAP_MAX`] (or whatever
    ///   [`Self::set_guide_tap_max`] was given), which is what keeps a long
    ///   "what shall I press" hold from counting.
    ///
    /// True for exactly the one `update` call that saw the release, and false
    /// again on the next. It has exactly one consumer per release, and the
    /// forwarding gate picks which: in a game the daemon turns it into a
    /// synthesized guide press on the virtual pad (`run::guide_tap_pulse` — in
    /// a game the Steam button is Steam's), and everywhere else it resolves the
    /// `guide_tap` binding (`run::guide_tap_binding`).
    pub fn guide_tap(&self) -> bool {
        self.guide_tap
    }

    /// Mark the current hold as spent by the desktop layer, so its release is
    /// reported as `GuideLeave { was_chorded: true, bare_tap: false }` — never
    /// as the bare tap Steam or a `guide_tap` binding would act on — exactly as
    /// recognising a chord does.
    ///
    /// For the things the engine cannot see as chords — a pad *touch* is not
    /// chordable ([`Self::chordable`] excludes both pads, because a thumb rests
    /// on one), so a hold spent on the pads would otherwise end in
    /// a bare leave. The daemon calls this:
    ///
    /// * when the right pad moves the cursor under a held guide (`h.cursor {
    ///   guide_in = … }`);
    /// * on the first detent of a caret scrub, when circling the left pad has
    ///   started tapping the arrow keys (`h.scrub`, `run::drive_scrub`) — a
    ///   caret fix is a *long* hold by chord standards, and it must not end
    ///   with Steam — or the launcher on `guide_tap` — acting on the release.
    ///
    /// The decision is the daemon's, not the engine's, because on a mode where
    /// the pads do nothing under the guide a thumb resting on one must *not*
    /// eat the tap. A no-op while no guide is held.
    pub fn consume_hold(&mut self) {
        if self.guide_active {
            self.was_chorded = true;
        }
    }

    /// The `chorded` bit for a button: the enum is `#[repr(u8)]` with fewer
    /// than 32 variants, so the discriminant is the bit.
    fn bit(b: report::Button) -> u32 {
        1 << (b as u32)
    }

    /// Edge-triggered flick detector with hysteresis, run per stick per frame.
    /// Returns `Some(dir)` on the frame a fresh flick crosses the threshold.
    fn flick(&mut self, stick: Stick, (x, y): (i16, i16)) -> Option<StickDir> {
        let ax = (x as i32).abs();
        let ay = (y as i32).abs();
        let dom = ax.max(ay);
        let armed = match stick {
            Stick::Left => &mut self.left_armed,
            Stick::Right => &mut self.right_armed,
        };
        if *armed {
            if dom >= FLICK_THRESHOLD {
                *armed = false;
                let dir = if ax >= ay {
                    if x >= 0 {
                        StickDir::Right
                    } else {
                        StickDir::Left
                    }
                } else if y >= 0 {
                    StickDir::Up
                } else {
                    StickDir::Down
                };
                return Some(dir);
            }
        } else if dom <= RECENTER_THRESHOLD {
            *armed = true;
        }
        None
    }

    /// A stick position counts as centered when its dominant axis is within the
    /// recenter threshold.
    fn centered((x, y): (i16, i16)) -> bool {
        (x as i32).abs().max((y as i32).abs()) <= RECENTER_THRESHOLD
    }

    /// Whether a button counts as a deliberate chord. The purely capacitive
    /// "hands on controller" bits and the pad *touch* (not click) flags fire on
    /// contact, so they are excluded to keep `was_chorded` meaningful. `Steam`
    /// itself is excluded so the guide press is never its own chord.
    fn chordable(b: report::Button) -> bool {
        use report::Button::*;
        !matches!(
            b,
            Steam | Cap0 | Cap1 | Cap2 | Cap3 | PadLeftTouch | PadRightTouch
        )
    }
}

impl Default for GestureEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Button, Frame};
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Find the `buttons` bit index for a `Button` using only the public
    /// `Frame::pressed` API, so tests stay correct if the bit order changes.
    fn bit(b: Button) -> u32 {
        (0..32)
            .find(|&i| {
                Frame {
                    buttons: 1 << i,
                    ..Frame::default()
                }
                .pressed(b)
            })
            .expect("button has a bit")
    }

    fn frame(buttons: &[Button]) -> Frame {
        let mut bits = 0u32;
        for &b in buttons {
            bits |= 1 << bit(b);
        }
        Frame {
            buttons: bits,
            ..Frame::default()
        }
    }

    fn frame_sticks(buttons: &[Button], left: (i16, i16), right: (i16, i16)) -> Frame {
        Frame {
            left_stick: left,
            right_stick: right,
            ..frame(buttons)
        }
    }

    #[test]
    fn bare_tap_enters_then_leaves_unchorded() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        assert!(g.update(&frame(&[]), t).is_empty());
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(4)),
            vec![GestureEvent::GuideEnter]
        );
        // Released well under the hold threshold, nothing pressed meanwhile.
        assert_eq!(
            g.update(&frame(&[]), t + ms(120)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
        assert!(!g.guide_active());
    }

    #[test]
    fn idle_stream_is_silent() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        for k in 0..10 {
            assert!(g.update(&frame(&[]), t + ms(4 * k)).is_empty());
        }
    }

    #[test]
    fn guide_button_chord() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(4)),
            vec![GestureEvent::GuideEnter]
        );
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::BumperR1]), t + ms(8)),
            vec![GestureEvent::GuideChord(Button::BumperR1)]
        );
        // Holding the same chord must not re-fire (level held, no new edge).
        assert!(g
            .update(&frame(&[Button::Steam, Button::BumperR1]), t + ms(12))
            .is_empty());
        assert_eq!(
            g.update(&frame(&[]), t + ms(60)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
    }

    #[test]
    fn guide_and_button_pressed_in_same_frame() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        // Guide and X arrive together in one report.
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::X]), t + ms(4)),
            vec![
                GestureEvent::GuideEnter,
                GestureEvent::GuideChord(Button::X)
            ]
        );
    }

    #[test]
    fn duplicate_frames_do_not_repeat_a_chord() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        let f = frame(&[Button::Steam, Button::A]);
        assert_eq!(
            g.update(&f, t + ms(8)),
            vec![GestureEvent::GuideChord(Button::A)]
        );
        // Exact duplicate report delivered again.
        assert!(g.update(&f, t + ms(12)).is_empty());
    }

    #[test]
    fn capacitive_and_pad_touch_are_not_chords() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        // Grabbing the controller / resting a thumb: contact-sense bits only.
        assert!(g
            .update(
                &frame(&[Button::Steam, Button::Cap0, Button::PadLeftTouch]),
                t + ms(8)
            )
            .is_empty());
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
    }

    #[test]
    fn pad_click_and_trigger_full_are_chords() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::TriggerR2Full]), t + ms(8)),
            vec![GestureEvent::GuideChord(Button::TriggerR2Full)]
        );
    }

    #[test]
    fn stick_flick_fires_exactly_once_while_sustained() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(4)),
            vec![GestureEvent::GuideEnter]
        );
        // Right stick flicked hard right.
        assert_eq!(
            g.update(&frame_sticks(&[Button::Steam], (0, 0), (25_000, 0)), t + ms(8)),
            vec![GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Right,
            }]
        );
        // Held out for many frames: no repeat.
        for k in 3..12 {
            assert!(g
                .update(
                    &frame_sticks(&[Button::Steam], (0, 0), (25_000, 0)),
                    t + ms(4 * k),
                )
                .is_empty());
        }
    }

    #[test]
    fn flick_hysteresis_requires_recenter_before_refire() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        // First flick.
        assert_eq!(
            g.update(&frame_sticks(&[Button::Steam], (0, 0), (30_000, 0)), t + ms(8)),
            vec![GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Right,
            }]
        );
        // Ease off but stay outside the recenter band: still no re-fire.
        assert!(g
            .update(&frame_sticks(&[Button::Steam], (0, 0), (12_000, 0)), t + ms(12))
            .is_empty());
        // Return near center: re-arms, but re-arming itself emits nothing.
        assert!(g
            .update(&frame_sticks(&[Button::Steam], (0, 0), (0, 0)), t + ms(16))
            .is_empty());
        // Now a fresh flick fires again.
        assert_eq!(
            g.update(&frame_sticks(&[Button::Steam], (0, 0), (30_000, 0)), t + ms(20)),
            vec![GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Right,
            }]
        );
    }

    #[test]
    fn flick_directions_pick_dominant_axis() {
        let t = Instant::now();
        let cases = [
            ((30_000i16, 0i16), Stick::Right, StickDir::Right),
            ((-30_000, 0), Stick::Right, StickDir::Left),
            ((5_000, 25_000), Stick::Right, StickDir::Up),
            ((5_000, -25_000), Stick::Right, StickDir::Down),
        ];
        for (right, stick, dir) in cases {
            let mut g = GestureEngine::new();
            g.update(&frame(&[]), t);
            g.update(&frame(&[Button::Steam]), t + ms(4));
            assert_eq!(
                g.update(&frame_sticks(&[Button::Steam], (0, 0), right), t + ms(8)),
                vec![GestureEvent::GuideStickFlick { stick, dir }],
                "flick {right:?} should map to {dir:?}",
            );
        }
    }

    #[test]
    fn left_stick_flick_is_distinguished_from_right() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        assert_eq!(
            g.update(
                &frame_sticks(&[Button::Steam], (-30_000, 0), (0, 0)),
                t + ms(8)
            ),
            vec![GestureEvent::GuideStickFlick {
                stick: Stick::Left,
                dir: StickDir::Left,
            }]
        );
    }

    #[test]
    fn flick_ignored_without_guide() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        // Stick slammed right, but guide not held: pure gameplay, no events.
        assert!(g
            .update(&frame_sticks(&[], (0, 0), (32_000, 0)), t)
            .is_empty());
    }

    #[test]
    fn idle_offset_does_not_flick() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        // Within the idle noise floor (~±400): no flick.
        assert!(g
            .update(&frame_sticks(&[Button::Steam], (400, -350), (300, 380)), t + ms(8))
            .is_empty());
    }

    #[test]
    fn deflected_at_enter_needs_recenter_first() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        // Stick already right, then guide pressed in that frame.
        g.update(&frame_sticks(&[], (0, 0), (30_000, 0)), t);
        assert_eq!(
            g.update(&frame_sticks(&[Button::Steam], (0, 0), (30_000, 0)), t + ms(4)),
            vec![GestureEvent::GuideEnter],
            "no flick should fire from a stick already deflected at guide press"
        );
        // Recenter, then flick counts.
        assert!(g
            .update(&frame_sticks(&[Button::Steam], (0, 0), (0, 0)), t + ms(8))
            .is_empty());
        assert_eq!(
            g.update(&frame_sticks(&[Button::Steam], (0, 0), (30_000, 0)), t + ms(12)),
            vec![GestureEvent::GuideStickFlick {
                stick: Stick::Right,
                dir: StickDir::Right,
            }]
        );
    }

    #[test]
    fn hold_past_threshold_fires_once() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t),
            vec![GestureEvent::GuideEnter]
        );
        // Just under the threshold: nothing yet.
        assert!(g.update(&frame(&[Button::Steam]), t + ms(299)).is_empty());
        // Crossing it emits GuideHold once.
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(300)),
            vec![GestureEvent::GuideHold]
        );
        assert!(g.update(&frame(&[Button::Steam]), t + ms(600)).is_empty());
        // A bare long hold is still not a chord.
        assert_eq!(
            g.update(&frame(&[]), t + ms(2300)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: false }]
        );
    }

    #[test]
    fn hold_then_chord_reports_chorded() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t);
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(300)),
            vec![GestureEvent::GuideHold]
        );
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::Y]), t + ms(320)),
            vec![GestureEvent::GuideChord(Button::Y)]
        );
        assert_eq!(
            g.update(&frame(&[]), t + ms(400)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
    }

    #[test]
    fn dropped_frames_do_not_break_timing_or_chords() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(4)),
            vec![GestureEvent::GuideEnter]
        );
        // Simulate a long gap in delivered frames (dropped reports). The next
        // frame carries a new chord and a time far past the hold threshold:
        // both the chord and the (single) hold should surface together.
        let e = g.update(&frame(&[Button::Steam, Button::DpadUp]), t + ms(900));
        assert!(e.contains(&GestureEvent::GuideChord(Button::DpadUp)));
        assert!(e.contains(&GestureEvent::GuideHold));
    }

    #[test]
    fn a_chord_buttons_lift_is_reported_while_the_guide_is_held() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::PadRightClick]), t + ms(8)),
            vec![GestureEvent::GuideChord(Button::PadRightClick)]
        );
        // Held: nothing. Lifted with the guide still down: the release, once.
        assert!(g.update(&frame(&[Button::Steam, Button::PadRightClick]), t + ms(12)).is_empty());
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(16)),
            vec![GestureEvent::GuideChordRelease(Button::PadRightClick)]
        );
        assert!(g.update(&frame(&[Button::Steam]), t + ms(20)).is_empty());
        // Pressed again during the same hold: a fresh chord and a fresh release.
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::PadRightClick]), t + ms(24)),
            vec![GestureEvent::GuideChord(Button::PadRightClick)]
        );
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(28)),
            vec![GestureEvent::GuideChordRelease(Button::PadRightClick)]
        );
        // The hold was chorded, so its end is not a bare tap.
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
    }

    #[test]
    fn two_chords_release_independently() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        g.update(&frame(&[Button::Steam, Button::GripL5]), t + ms(8));
        g.update(&frame(&[Button::Steam, Button::GripL5, Button::A]), t + ms(12));
        // L5 lifts, A stays: only L5's release.
        assert_eq!(
            g.update(&frame(&[Button::Steam, Button::A]), t + ms(16)),
            vec![GestureEvent::GuideChordRelease(Button::GripL5)]
        );
        assert_eq!(
            g.update(&frame(&[Button::Steam]), t + ms(20)),
            vec![GestureEvent::GuideChordRelease(Button::A)]
        );
    }

    #[test]
    fn a_button_down_before_the_guide_never_reports_a_release() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        // A is already down when the guide arrives: it never chorded, so its
        // lift under the guide is not a chord release either.
        g.update(&frame(&[Button::A]), t);
        assert_eq!(
            g.update(&frame(&[Button::A, Button::Steam]), t + ms(4)),
            vec![GestureEvent::GuideEnter]
        );
        assert!(g.update(&frame(&[Button::Steam]), t + ms(8)).is_empty());
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: false }]
        );
    }

    #[test]
    fn lifting_the_chord_with_the_guide_is_a_leave_not_a_release() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        g.update(&frame(&[Button::Steam, Button::BumperR1]), t + ms(8));
        // Both lift in one report: the leave ends every chord, so no separate
        // release is reported — the consumer releases everything on the leave.
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
        // And the chord is forgotten with the hold: the next hold starts clean,
        // so a button still down from before it cannot report a stale release.
        g.update(&frame(&[Button::BumperR1]), t + ms(44));
        g.update(&frame(&[Button::BumperR1, Button::Steam]), t + ms(48));
        assert!(g.update(&frame(&[Button::Steam]), t + ms(52)).is_empty());
    }

    #[test]
    fn consume_hold_marks_the_hold_chorded() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        // The daemon spent the hold on something the engine cannot see (the
        // pad moved the cursor): the release must not read as a bare tap.
        g.consume_hold();
        assert!(g.update(&frame(&[Button::Steam]), t + ms(8)).is_empty());
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
        // It does not carry over: the next hold is a fresh, bare one.
        g.update(&frame(&[Button::Steam]), t + ms(100));
        assert_eq!(
            g.update(&frame(&[]), t + ms(140)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
    }

    #[test]
    fn a_scrubbing_thumb_spends_the_hold_and_a_resting_one_does_not() {
        // The caret scrub's half of the rule (`run::drive_scrub`): a thumb on
        // the LEFT pad is a touch, not a chord, so holding the guide while it
        // rests there must still read as a bare tap — and the daemon's call on
        // the first detent is the only thing that changes that.
        let t = Instant::now();
        let touching = frame(&[Button::Steam, Button::PadLeftTouch]);

        let mut resting = GestureEngine::new();
        resting.update(&frame(&[]), t);
        resting.update(&frame(&[Button::Steam]), t + ms(4));
        assert!(resting.update(&touching, t + ms(8)).is_empty());
        assert_eq!(
            resting.update(&frame(&[]), t + ms(400)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }],
            "a thumb that only rested there leaves the tap for Steam"
        );

        let mut scrubbing = GestureEngine::new();
        scrubbing.update(&frame(&[]), t);
        scrubbing.update(&frame(&[Button::Steam]), t + ms(4));
        scrubbing.update(&touching, t + ms(8));
        // The first detent fired: the daemon spends the hold.
        scrubbing.consume_hold();
        // The rest of the spin has nothing left to spend, and says so.
        scrubbing.consume_hold();
        assert!(scrubbing.update(&touching, t + ms(12)).is_empty());
        assert_eq!(
            scrubbing.update(&frame(&[]), t + ms(400)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }],
            "a caret fix must not end as a guide tap"
        );
    }

    #[test]
    fn consume_hold_without_a_guide_is_a_no_op() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        g.consume_hold();
        // Nothing to consume: the next hold is still a bare tap.
        g.update(&frame(&[Button::Steam]), t + ms(4));
        assert_eq!(
            g.update(&frame(&[]), t + ms(40)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
    }

    // -----------------------------------------------------------------------
    // The quick bare tap: what `guide_tap()` answers, and what it refuses.
    // -----------------------------------------------------------------------

    /// Press, release, nothing in between — the gesture that must reach Steam.
    #[test]
    fn a_quick_bare_press_and_release_is_a_guide_tap() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[]), t);
        assert!(!g.update(&frame(&[Button::Steam]), t + ms(4)).is_empty());
        assert!(!g.guide_tap(), "the press alone is not a tap; Steam acts on release");
        assert_eq!(
            g.update(&frame(&[]), t + ms(120)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
        assert!(g.guide_tap(), "a 116 ms bare hold is the tap");
        // One frame's worth of signal, and no more.
        g.update(&frame(&[]), t + ms(124));
        assert!(!g.guide_tap(), "it does not linger past the frame that produced it");
    }

    /// A hold the owner spent deliberating is not a tap, however it ends.
    #[test]
    fn a_hold_past_the_cap_is_not_a_tap() {
        let t = Instant::now();
        let at = |release: u64| {
            let mut g = GestureEngine::new();
            g.update(&frame(&[]), t);
            g.update(&frame(&[Button::Steam]), t + ms(4));
            g.update(&frame(&[]), t + ms(4 + release));
            g.guide_tap()
        };
        assert!(at(399), "just under the cap");
        assert!(at(400), "exactly the cap still counts");
        assert!(!at(401), "one millisecond over and it is a deliberation");
        assert!(!at(3_000));
        assert_eq!(GUIDE_TAP_MAX, Duration::from_millis(400));
    }

    /// The cap is the config's to set, live.
    #[test]
    fn the_tap_cap_is_settable() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.set_guide_tap_max(ms(150));
        g.update(&frame(&[]), t);
        g.update(&frame(&[Button::Steam]), t + ms(4));
        g.update(&frame(&[]), t + ms(204));
        assert!(!g.guide_tap(), "200 ms is over a 150 ms cap");
        // And it applies from the next hold with no rebuild.
        g.set_guide_tap_max(ms(1_000));
        g.update(&frame(&[Button::Steam]), t + ms(300));
        g.update(&frame(&[]), t + ms(900));
        assert!(g.guide_tap(), "600 ms is under a 1 s cap");
    }

    /// Everything the desktop layer claims, it keeps: a chord, a flick and a
    /// hold the daemon spent are all *not* taps.
    #[test]
    fn a_chord_a_flick_and_a_consumed_hold_are_not_taps() {
        let t = Instant::now();

        let mut chord = GestureEngine::new();
        chord.update(&frame(&[]), t);
        chord.update(&frame(&[Button::Steam]), t + ms(4));
        chord.update(&frame(&[Button::Steam, Button::BumperR1]), t + ms(8));
        chord.update(&frame(&[Button::Steam]), t + ms(12));
        chord.update(&frame(&[]), t + ms(60));
        assert!(!chord.guide_tap(), "guide+r1 belongs to the window manager");

        let mut flick = GestureEngine::new();
        flick.update(&frame(&[]), t);
        flick.update(&frame(&[Button::Steam]), t + ms(4));
        flick.update(&frame_sticks(&[Button::Steam], (0, 0), (30_000, 0)), t + ms(8));
        flick.update(&frame(&[]), t + ms(60));
        assert!(!flick.guide_tap());

        let mut spent = GestureEngine::new();
        spent.update(&frame(&[]), t);
        spent.update(&frame(&[Button::Steam]), t + ms(4));
        spent.consume_hold(); // the guide-mouse, or the caret scrub's first detent
        spent.update(&frame(&[]), t + ms(60));
        assert!(!spent.guide_tap(), "a hold the daemon spent is never Steam's");
    }

    /// A button already down when the guide arrives has no press edge under the
    /// guide, so it never chords — but the hand is mid-something, and the
    /// release must not open an overlay over it.
    #[test]
    fn a_button_already_held_when_the_guide_arrives_blocks_the_tap() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        g.update(&frame(&[Button::BumperR1]), t);
        g.update(&frame(&[Button::BumperR1, Button::Steam]), t + ms(4));
        g.update(&frame(&[Button::BumperR1]), t + ms(60));
        assert!(!g.guide_tap());
        // It is not sticky: the next hold, with the hand off the bumper, taps.
        g.update(&frame(&[]), t + ms(64));
        g.update(&frame(&[Button::Steam]), t + ms(68));
        g.update(&frame(&[]), t + ms(120));
        assert!(g.guide_tap());
    }

    /// The touch bits are not "already held": a thumb resting on a pad, or a
    /// deflected stick, is the ordinary state of a hand in a game, and neither
    /// may cost the owner the tap.
    #[test]
    fn a_resting_thumb_or_a_deflected_stick_still_taps() {
        let t = Instant::now();
        let mut g = GestureEngine::new();
        let holding = frame_sticks(
            &[Button::PadLeftTouch, Button::PadRightTouch],
            (28_000, 0),
            (0, -28_000),
        );
        g.update(&holding, t);
        let mut with_guide = holding;
        with_guide.set(Button::Steam, true);
        g.update(&with_guide, t + ms(4));
        g.update(&holding, t + ms(80));
        assert!(g.guide_tap(), "a hand on the controller is not a chord");
    }
}
