//! Rate control: turning a stick's deflection into cursor motion, scrolling,
//! and on-screen-keyboard cursors.
//!
//! # Why this is not the pad's pipeline
//!
//! The controller's cursor is **position** control. [`crate::run::drive_cursor`]
//! differences the smoothed absolute pad coordinate, and the One Euro filter in
//! [`crate::filter::PadDamper`] exists because differencing amplifies sensor
//! noise (`docs/research/pointer-damping.md` §1.2). A stick is **rate**
//! control: the deflection *is* a velocity command, it returns to a mechanical
//! centre, and the kernel has already applied the axis's `fuzz`/`flat`. There
//! is nothing to difference and therefore nothing for a One Euro filter to fix
//! — running one here would only add lag. The only smoothing worth having is on
//! the velocity itself, and a short EMA is enough.
//!
//! The pipeline, which is the one Steam Input, AntiMicroX and xpadneo all
//! implement (`docs/research/xbox-elite.md` §3.1):
//!
//! ```text
//! r  = |(x, y)|                                    radial deflection, 0..1
//! d  = clamp((r - inner) / (outer - inner), 0, 1)  deadzone rescale
//! g  = d^curve                                     response curve
//! v  = max * g                                     output units per second
//! (vx, vy) = v * (x, y) / r                        the stick's exact direction
//! ```
//!
//! Radial, not per-axis: a per-axis deadzone makes the diagonals reachable at a
//! smaller deflection than the cardinals, which is the classic "the cursor
//! drifts diagonally" bug.
//!
//! # The clock, and why it is a deadline and not a tick
//!
//! A gamepad reports **only on change**. Hold a stick at 60 % and the device
//! goes silent, so an integrator driven by frames would move the cursor once
//! and stop. It needs its own clock — but not a free-running one: the daemon's
//! loop already blocks on its input channel with a *deadline*
//! ([`crate::run`]'s reconnect scan, process rescan and transient-mode timer),
//! and this is a fourth deadline on the same mechanism.
//!
//! [`StickDrive::deadline`] returns `Some(when)` **only while something is
//! actually moving** — a stick outside its deadzone, or a velocity still
//! decaying through the EMA after one was released — and `None` otherwise.
//! Centred, the loop blocks indefinitely exactly as it does today with the controller
//! asleep. Nothing spins, nothing polls, and an idle pad costs zero wakeups.
//!
//! Everything in this module is pure over `Instant`, so the whole decision
//! table is unit-testable with no device and no clock.

use std::time::{Duration, Instant};

use crate::config::{StickAxisConfig, SticksConfig};

/// Longest step the integrator will honour.
///
/// The deadline disarms when the sticks centre, so the gap before the next
/// engaged step can be arbitrarily long — minutes, if the pad was put down.
/// Without a clamp the first step after such a gap would move the cursor across
/// the screen. 50 ms is generous next to the 4 ms cadence and short enough that
/// a hitch cannot produce a jump.
const MAX_STEP: Duration = Duration::from_millis(50);

/// Below this speed (in output units per second) a decaying velocity counts as
/// stopped, so the deadline disarms instead of chasing an exponential tail
/// forever.
const SETTLED: f64 = 0.5;

/// The response curve and deadzone for one rate-controlled axis pair.
///
/// Split out from [`StickAxisConfig`] so the shaping can be exercised on its
/// own; `StickAxisConfig` is the config-facing struct and carries the speed and
/// smoothing beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub deadzone: f64,
    pub outer: f64,
    pub curve: f64,
}

impl Shape {
    /// The shape of one config section.
    pub fn of(cfg: &StickAxisConfig) -> Shape {
        Shape { deadzone: cfg.deadzone, outer: cfg.outer, curve: cfg.curve }
    }

    /// Rescale a radial deflection through the inner/outer deadzone and the
    /// response curve, to `0..=1`.
    ///
    /// `0` at or inside the deadzone, `1` at or outside `outer`. A `curve`
    /// exponent of `1.0` is linear; the default `2.0` is AntiMicroX's
    /// "Quadratic" — "slowed down slightly to allow better control on the low
    /// end", which is the precision a desktop pointer wants.
    pub fn shaped(&self, r: f64) -> f64 {
        let inner = self.deadzone.clamp(0.0, 0.99);
        let outer = self.outer.clamp(inner + 1e-6, 1.0);
        if r <= inner {
            return 0.0;
        }
        let d = ((r - inner) / (outer - inner)).clamp(0.0, 1.0);
        if self.curve <= 0.0 {
            d
        } else {
            d.powf(self.curve)
        }
    }

    /// Whether a stick at `(x, y)` (normalised to `[-1, 1]`) is outside its
    /// deadzone — the arming question.
    pub fn engaged(&self, (x, y): (f64, f64)) -> bool {
        x.hypot(y) > self.deadzone.clamp(0.0, 0.99)
    }

    /// The velocity command for a stick at `(x, y)`, in output units per
    /// second, at full-scale speed `max`.
    ///
    /// The direction is the stick's exactly: the shaped magnitude is applied
    /// along the unit vector, never per axis.
    pub fn velocity(&self, (x, y): (f64, f64), max: f64) -> (f64, f64) {
        let r = x.hypot(y);
        if r <= 0.0 {
            return (0.0, 0.0);
        }
        let v = max * self.shaped(r.min(1.0));
        (v * x / r, v * y / r)
    }
}

/// One axis pair's rate integrator: smoothed velocity plus a sub-unit
/// remainder.
///
/// Pure and clock-driven: `step` is given the stick position and the time, and
/// answers with how far to move. Nothing here knows about pointers, scrolling
/// or keyboards.
#[derive(Clone, Copy, Debug, Default)]
pub struct StickIntegrator {
    /// Smoothed velocity, output units per second.
    vel: (f64, f64),
    /// Sub-unit remainder, carried between steps so a slow, deliberate drift
    /// still moves — the same trick [`crate::filter::PadDamper`] plays with
    /// `acc_x`/`acc_y`.
    acc: (f64, f64),
    /// When the last step ran. `None` clutches the next one to zero, so a
    /// re-armed integrator never integrates the gap it was asleep for.
    last: Option<Instant>,
}

impl StickIntegrator {
    pub fn new() -> StickIntegrator {
        StickIntegrator::default()
    }

    /// Forget everything: velocity, remainder and the clock. Called whenever
    /// the layer this feeds loses its claim (a mode handoff, the guide layer
    /// taking the sticks, a disconnect), so re-entry never integrates across
    /// the gap.
    pub fn reset(&mut self) {
        *self = StickIntegrator::default();
    }

    /// Whether the velocity is still large enough to be worth a wakeup. This is
    /// the "or a scroll rate is non-zero" half of the arming rule: with
    /// smoothing on, releasing a stick leaves a decaying velocity that must
    /// still be integrated for a few milliseconds.
    pub fn settling(&self) -> bool {
        self.vel.0.abs() > SETTLED || self.vel.1.abs() > SETTLED
    }

    /// The smoothed velocity, output units per second. Diagnostics and tests.
    pub fn velocity(&self) -> (f64, f64) {
        self.vel
    }

    /// Integrate one step and return the **fractional** distance moved.
    ///
    /// The remainder is not used on this path: a caller that can emit
    /// fractional units (scrolling, a normalised OSK cursor) wants the whole
    /// value.
    pub fn step(&mut self, stick: (f64, f64), cfg: &StickAxisConfig, now: Instant) -> (f64, f64) {
        let dt = self.advance(now);
        let target = Shape::of(cfg).velocity(stick, cfg.max);
        self.smooth(target, dt, cfg.smoothing_ms);
        (self.vel.0 * dt, self.vel.1 * dt)
    }

    /// Integrate one step and return **whole** units, carrying the remainder.
    ///
    /// What the desktop cursor wants: the pointer takes integer pixels, and
    /// throwing the sub-pixel part away would make a slow, deliberate nudge
    /// move nothing at all.
    pub fn step_whole(
        &mut self,
        stick: (f64, f64),
        cfg: &StickAxisConfig,
        now: Instant,
    ) -> (i32, i32) {
        let (dx, dy) = self.step(stick, cfg, now);
        self.acc.0 += dx;
        self.acc.1 += dy;
        let (wx, wy) = (self.acc.0.trunc(), self.acc.1.trunc());
        self.acc.0 -= wx;
        self.acc.1 -= wy;
        (wx as i32, wy as i32)
    }

    /// Elapsed seconds since the last step, clamped, seeding the clock on the
    /// first call after a reset (which therefore integrates nothing).
    fn advance(&mut self, now: Instant) -> f64 {
        let prev = self.last.replace(now);
        match prev {
            None => 0.0,
            Some(prev) => now.saturating_duration_since(prev).min(MAX_STEP).as_secs_f64(),
        }
    }

    /// One exponential step of the velocity toward `target`.
    ///
    /// `tau_ms <= 0` is no smoothing at all (the stick's own mechanics are the
    /// filter, which is what Steam Input does). Otherwise the standard
    /// frame-rate-independent EMA, so the feel does not change with the step
    /// size.
    fn smooth(&mut self, target: (f64, f64), dt: f64, tau_ms: f64) {
        // No time passed — the clutch step after a reset — is no filter step
        // either: jumping the velocity to the target here would make the very
        // first step of every re-arm unsmoothed.
        if dt <= 0.0 {
            return;
        }
        if tau_ms <= 0.0 {
            self.vel = target;
            return;
        }
        let alpha = 1.0 - (-dt / (tau_ms / 1000.0)).exp();
        self.vel.0 += alpha * (target.0 - self.vel.0);
        self.vel.1 += alpha * (target.1 - self.vel.1);
    }
}

/// A normalised `[-1, 1]` position driven by a stick — one OSK cursor.
///
/// Phase 1 of the on-screen keyboard on a padless controller (option (i) of
/// `docs/research/xbox-elite.md` §3.5): each stick integrates into a per-hand
/// position and the daemon sends it over the **existing** `cursor L|R` wire, so
/// the OSK child is completely unchanged. Phase 2's snap navigation is a
/// change to `osk/`, not to this.
#[derive(Clone, Copy, Debug)]
pub struct StickCursor {
    integrator: StickIntegrator,
    pos: (f64, f64),
    /// The last position actually put on the wire, at the wire's own
    /// precision. See [`should_send`](Self::should_send).
    last_sent: Option<(f32, f32)>,
}

impl Default for StickCursor {
    fn default() -> StickCursor {
        StickCursor::new()
    }
}

impl StickCursor {
    /// A cursor parked at the centre.
    pub fn new() -> StickCursor {
        StickCursor { integrator: StickIntegrator::new(), pos: (0.0, 0.0), last_sent: None }
    }

    /// Recentre and forget the velocity — a fresh keyboard starts in the
    /// middle rather than wherever the last one was left.
    pub fn reset(&mut self) {
        self.integrator.reset();
        self.pos = (0.0, 0.0);
        self.last_sent = None;
    }

    /// Whether `rounded` is somewhere this cursor has not already been sent to,
    /// remembering it if so.
    ///
    /// The OSK's `cursor` command serialises at four decimals, so a cursor
    /// clamped against an edge — a stick held hard over — stops re-sending
    /// instead of writing the same line into the child's stdin every step.
    /// Exactly what the pad router's `last_sent` does, for exactly the same
    /// reason.
    pub fn should_send(&mut self, rounded: (f32, f32)) -> bool {
        if self.last_sent == Some(rounded) {
            return false;
        }
        self.last_sent = Some(rounded);
        true
    }

    /// Integrate one step and return the new position, clamped to the
    /// keyboard's `[-1, 1]` box.
    pub fn step(
        &mut self,
        stick: (f64, f64),
        cfg: &StickAxisConfig,
        now: Instant,
    ) -> (f64, f64) {
        let (dx, dy) = self.integrator.step(stick, cfg, now);
        self.pos.0 = (self.pos.0 + dx).clamp(-1.0, 1.0);
        self.pos.1 = (self.pos.1 + dy).clamp(-1.0, 1.0);
        self.pos
    }

    pub fn position(&self) -> (f64, f64) {
        self.pos
    }

    pub fn settling(&self) -> bool {
        self.integrator.settling()
    }
}

/// Both sticks' normalised deflection, as last seen on a frame.
///
/// `+y` is **up**, the controller's convention, which [`crate::evdev`] has already
/// converted to. The cursor and scroll paths flip it themselves where the
/// output wants screen coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Deflection {
    pub left: (f64, f64),
    pub right: (f64, f64),
}

impl Deflection {
    /// Read both sticks off a frame, normalised to `[-1, 1]`.
    pub fn of(frame: &crate::report::Frame) -> Deflection {
        let n = |(x, y): (i16, i16)| {
            (
                (f64::from(x) / 32767.0).clamp(-1.0, 1.0),
                (f64::from(y) / 32767.0).clamp(-1.0, 1.0),
            )
        };
        Deflection { left: n(frame.left_stick), right: n(frame.right_stick) }
    }
}

/// Everything the daemon integrates off the sticks, and the one place the
/// conditional deadline is decided.
///
/// The daemon owns one of these. Frames update [`observe`](Self::observe);
/// the loop asks [`deadline`](Self::deadline) every iteration and, when it has
/// expired, steps whichever integrators are live.
pub struct StickDrive {
    /// The last deflection seen from a rate-controlled source. Zeroed the
    /// moment a position-controlled source (the controller) becomes active, so a
    /// stale Xbox frame cannot keep the deadline armed.
    pub sticks: Deflection,
    /// Whether the active source is rate-controlled at all.
    rate: bool,
    /// The desktop cursor, driven by the RIGHT stick.
    pub cursor: StickIntegrator,
    /// Scrolling, driven by the LEFT stick.
    pub scroll: StickIntegrator,
    /// Sub-notch scroll travel, for the detent haptic. Carried, not reset per
    /// step, so a slow scroll still ticks once per notch.
    pub scroll_detent: f64,
    /// The OSK's two cursors, one per hand.
    pub osk_left: StickCursor,
    pub osk_right: StickCursor,
    /// When the next step is due. `None` means nothing is moving and the loop
    /// may block indefinitely.
    due: Option<Instant>,
}

impl Default for StickDrive {
    fn default() -> StickDrive {
        StickDrive::new()
    }
}

impl StickDrive {
    pub fn new() -> StickDrive {
        StickDrive {
            sticks: Deflection::default(),
            rate: false,
            cursor: StickIntegrator::new(),
            scroll: StickIntegrator::new(),
            scroll_detent: 0.0,
            osk_left: StickCursor::new(),
            osk_right: StickCursor::new(),
            due: None,
        }
    }

    /// Record one frame's sticks and re-arm (or disarm) the deadline.
    ///
    /// A frame from a source with pads parks everything: the controller drives the
    /// cursor from its trackpads and its sticks are for flicks only, so the
    /// rate integrators must be silent and, above all, must not hold the
    /// deadline open.
    pub fn observe(&mut self, frame: &crate::report::Frame, cfg: &SticksConfig, now: Instant) {
        self.rate = cfg.enabled && frame.source.cursor_is_rate();
        self.sticks = if self.rate { Deflection::of(frame) } else { Deflection::default() };
        self.rearm(cfg, now);
    }

    /// Drop every claim: velocities, remainders, the OSK positions and the
    /// deadline. Run beside [`crate::run`]'s existing release paths — a mode
    /// handoff, a disconnect, a source switch — so nothing is integrated across
    /// a gap.
    pub fn release(&mut self) {
        self.cursor.reset();
        self.scroll.reset();
        self.scroll_detent = 0.0;
        self.osk_left.reset();
        self.osk_right.reset();
        self.sticks = Deflection::default();
        self.due = None;
    }

    /// Whether anything is moving: a stick outside its deadzone, or a velocity
    /// still decaying after one was let go.
    ///
    /// This is the arming rule in one place. Note the second half: with the EMA
    /// on, disarming the instant a stick centres would leave the cursor's last
    /// few pixels un-integrated and the motion would end with a visible clip.
    pub fn armed(&self, cfg: &SticksConfig) -> bool {
        if !self.rate || !cfg.enabled {
            return false;
        }
        Shape::of(&cfg.cursor).engaged(self.sticks.right)
            || Shape::of(&cfg.scroll).engaged(self.sticks.left)
            || Shape::of(&cfg.osk).engaged(self.sticks.left)
            || Shape::of(&cfg.osk).engaged(self.sticks.right)
            || self.cursor.settling()
            || self.scroll.settling()
            || self.osk_left.settling()
            || self.osk_right.settling()
    }

    /// When the loop must next wake for the integrators — `None` while
    /// everything is centred and settled, which is the whole point.
    pub fn deadline(&self) -> Option<Instant> {
        self.due
    }

    /// Note that a step has just run, and schedule the next one if anything is
    /// still moving.
    pub fn stepped(&mut self, cfg: &SticksConfig, now: Instant) {
        self.due = self
            .armed(cfg)
            .then(|| now + Duration::from_millis(cfg.tick_ms.max(1)));
    }

    /// Arm the deadline for `now` if something is moving and it is not armed
    /// already. Keeping an existing deadline matters: a stream of frames from a
    /// moving stick must not keep pushing the step into the future.
    fn rearm(&mut self, cfg: &SticksConfig, now: Instant) {
        if !self.armed(cfg) {
            self.due = None;
        } else if self.due.is_none() {
            self.due = Some(now + Duration::from_millis(cfg.tick_ms.max(1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SticksConfig;

    fn cfg() -> SticksConfig {
        SticksConfig::default()
    }

    fn at(ms: u64) -> Instant {
        // A fixed origin so every test's clock is deterministic.
        base() + Duration::from_millis(ms)
    }

    fn base() -> Instant {
        use std::sync::OnceLock;
        static T0: OnceLock<Instant> = OnceLock::new();
        *T0.get_or_init(Instant::now)
    }

    // --- the curve ---------------------------------------------------------

    #[test]
    fn the_deadzone_is_radial_and_rescales_to_the_outer_edge() {
        let s = Shape { deadzone: 0.12, outer: 0.95, curve: 1.0 };
        assert_eq!(s.shaped(0.0), 0.0);
        assert_eq!(s.shaped(0.12), 0.0, "at the deadzone is still nothing");
        assert!(s.shaped(0.13) > 0.0);
        assert!((s.shaped(0.95) - 1.0).abs() < 1e-12, "the outer edge is full scale");
        assert!((s.shaped(1.0) - 1.0).abs() < 1e-12, "and past it is clamped");
        // Halfway through the live range is half output on a linear curve.
        let mid = 0.12 + (0.95 - 0.12) / 2.0;
        assert!((s.shaped(mid) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn the_curve_exponent_slows_the_low_end() {
        let lin = Shape { deadzone: 0.0, outer: 1.0, curve: 1.0 };
        let quad = Shape { deadzone: 0.0, outer: 1.0, curve: 2.0 };
        assert!(quad.shaped(0.5) < lin.shaped(0.5), "quadratic is gentler in the middle");
        assert!((quad.shaped(0.5) - 0.25).abs() < 1e-12);
        // Both still reach full scale at full deflection: the curve changes the
        // feel, never the top speed.
        assert!((quad.shaped(1.0) - lin.shaped(1.0)).abs() < 1e-12);
    }

    #[test]
    fn the_direction_is_the_sticks_and_the_speed_is_capped() {
        let s = Shape { deadzone: 0.12, outer: 0.95, curve: 2.0 };
        // Full deflection on the diagonal: the magnitude is the max, split
        // along the unit vector — not `max` on each axis.
        let d = std::f64::consts::FRAC_1_SQRT_2;
        let (vx, vy) = s.velocity((d, d), 1500.0);
        assert!((vx.hypot(vy) - 1500.0).abs() < 1.0, "got {}", vx.hypot(vy));
        assert!((vx - vy).abs() < 1e-6, "the diagonal stays a diagonal");
        // Inside the deadzone is exactly zero, both axes.
        assert_eq!(s.velocity((0.05, -0.05), 1500.0), (0.0, 0.0));
        // A cardinal push at full deflection is the full speed on that axis.
        let (vx, vy) = s.velocity((1.0, 0.0), 1500.0);
        assert!((vx - 1500.0).abs() < 1.0 && vy.abs() < 1e-9);
    }

    // --- the integrator ----------------------------------------------------

    #[test]
    fn the_first_step_after_a_reset_moves_nothing() {
        let c = cfg();
        let mut i = StickIntegrator::new();
        assert_eq!(i.step_whole((1.0, 0.0), &c.cursor, at(0)), (0, 0), "the clutch");
        // ...and the second one does.
        let (dx, _) = i.step_whole((1.0, 0.0), &c.cursor, at(4));
        assert!(dx > 0);
    }

    #[test]
    fn a_held_stick_converges_on_the_configured_top_speed() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 15.0;
        let mut i = StickIntegrator::new();
        let mut t = 0;
        i.step((1.0, 0.0), &c.cursor, at(t));
        // Integrate a second's worth of 4 ms steps and count the pixels.
        let mut px = 0.0;
        while t < 1000 {
            t += 4;
            px += i.step((1.0, 0.0), &c.cursor, at(t)).0;
        }
        // 1500 px/s minus the EMA's start-up cost (a few tens of ms).
        assert!(px > 1400.0 && px <= 1500.0, "one second at full tilt moved {px} px");
        assert!((i.velocity().0 - 1500.0).abs() < 1.0, "settled at the max");
    }

    #[test]
    fn smoothing_ramps_the_velocity_and_zero_tau_does_not() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 15.0;
        let mut smoothed = StickIntegrator::new();
        smoothed.step((1.0, 0.0), &c.cursor, at(0));
        smoothed.step((1.0, 0.0), &c.cursor, at(4));
        assert!(smoothed.velocity().0 < 1500.0, "the EMA has not arrived yet");

        c.cursor.smoothing_ms = 0.0;
        let mut instant = StickIntegrator::new();
        instant.step((1.0, 0.0), &c.cursor, at(0));
        instant.step((1.0, 0.0), &c.cursor, at(4));
        assert!((instant.velocity().0 - 1500.0).abs() < 1e-9, "no smoothing = no ramp");
    }

    #[test]
    fn a_long_gap_is_clamped_so_a_re_armed_stick_cannot_jump() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 0.0;
        let mut i = StickIntegrator::new();
        i.step((1.0, 0.0), &c.cursor, at(0));
        // Ten seconds later: 1500 px/s * 10 s would be 15000 px.
        let (dx, _) = i.step_whole((1.0, 0.0), &c.cursor, at(10_000));
        assert!(dx <= 75, "clamped to MAX_STEP, got {dx}");
    }

    #[test]
    fn sub_pixel_remainders_carry_so_a_slow_drift_still_moves() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 0.0;
        // Just past the deadzone on a quadratic curve: a fraction of a pixel
        // per 4 ms step.
        let mut i = StickIntegrator::new();
        let stick = (0.2, 0.0);
        let per_step = Shape::of(&c.cursor).velocity(stick, c.cursor.max).0 * 0.004;
        assert!(per_step < 1.0, "the test needs a sub-pixel step, got {per_step}");
        let mut total = 0;
        for k in 0..250 {
            total += i.step_whole(stick, &c.cursor, at(k * 4)).0;
        }
        assert!(total > 0, "a sub-pixel rate must still move the cursor");
    }

    // --- the OSK cursor ----------------------------------------------------

    #[test]
    fn an_osk_cursor_integrates_into_the_normalised_box_and_clamps() {
        let mut c = cfg();
        c.osk.smoothing_ms = 0.0;
        let mut cur = StickCursor::new();
        cur.step((1.0, 0.0), &c.osk, at(0));
        for k in 1..500 {
            cur.step((1.0, 0.0), &c.osk, at(k * 4));
        }
        assert!((cur.position().0 - 1.0).abs() < 1e-9, "clamped at the right edge");
        assert_eq!(cur.position().1, 0.0);
        cur.reset();
        assert_eq!(cur.position(), (0.0, 0.0), "a fresh keyboard starts centred");
    }

    /// A cursor clamped against an edge stops re-sending: the deadline is
    /// still armed (the stick is held over) but there is nothing new to say,
    /// and writing the same line into the OSK child's stdin 250 times a second
    /// would be a busy loop by another name.
    #[test]
    fn an_osk_cursor_stops_re_sending_once_it_stops_moving() {
        let mut c = cfg();
        c.osk.smoothing_ms = 0.0;
        let mut cur = StickCursor::new();
        let mut sent = 0;
        for k in 0..500 {
            let p = cur.step((1.0, 0.0), &c.osk, at(k * 4));
            let rounded = (((p.0 * 10_000.0).round() / 10_000.0) as f32, p.1 as f32);
            if cur.should_send(rounded) {
                sent += 1;
            }
        }
        assert!(sent > 10, "it moved, so it was sent while it moved ({sent})");
        assert!(sent < 400, "and stopped once clamped at the edge ({sent})");
        // A reset forgets where it was, so the next show re-sends the centre.
        cur.reset();
        assert!(cur.should_send((0.0, 0.0)));
        assert!(!cur.should_send((0.0, 0.0)));
    }

    // --- the arm / disarm decision table -----------------------------------

    fn evdev_frame(left: (i16, i16), right: (i16, i16)) -> crate::report::Frame {
        crate::report::Frame {
            source: crate::report::Source::Evdev,
            left_stick: left,
            right_stick: right,
            ..Default::default()
        }
    }

    #[test]
    fn centred_sticks_arm_no_deadline_at_all() {
        let c = cfg();
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((0, 0), (0, 0)), &c, at(0));
        assert!(!d.armed(&c));
        assert_eq!(d.deadline(), None, "the loop blocks indefinitely");
        // An idle offset inside the deadzone is still centred.
        let idle = (0.10 * 32767.0) as i16;
        d.observe(&evdev_frame((idle, 0), (0, idle)), &c, at(4));
        assert!(!d.armed(&c), "0.10 is inside the 0.12 deadzone");
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn a_deflected_stick_arms_the_deadline_and_centring_drops_it() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 0.0;
        c.scroll.smoothing_ms = 0.0;
        c.osk.smoothing_ms = 0.0;
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((0, 0), (20_000, 0)), &c, at(0));
        assert!(d.armed(&c), "the right stick is out of the deadzone");
        let due = d.deadline().expect("armed");
        assert_eq!(due, at(0) + Duration::from_millis(c.tick_ms));

        // More frames from the same still-deflected stick must NOT push the
        // deadline out — otherwise a busy report stream starves the integrator.
        d.observe(&evdev_frame((0, 0), (20_000, 0)), &c, at(2));
        assert_eq!(d.deadline(), Some(due), "an existing deadline is kept");

        // Centred again: disarmed, with no velocity left to settle.
        d.observe(&evdev_frame((0, 0), (0, 0)), &c, at(8));
        assert!(!d.armed(&c));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn the_left_stick_arms_it_too_and_so_does_a_settling_velocity() {
        let mut c = cfg();
        c.scroll.smoothing_ms = 15.0;
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((-25_000, 0), (0, 0)), &c, at(0));
        assert!(d.armed(&c), "the left stick scrolls");
        // Wind the scroll integrator up, then centre the stick: the EMA still
        // has a velocity to spend, so the deadline stays armed.
        d.scroll.step(d.sticks.left, &c.scroll, at(0));
        for k in 1..20 {
            d.scroll.step(d.sticks.left, &c.scroll, at(k * 4));
        }
        assert!(d.scroll.settling());
        d.observe(&evdev_frame((0, 0), (0, 0)), &c, at(80));
        assert!(d.armed(&c), "a decaying velocity keeps the deadline");
        assert!(d.deadline().is_some());
        // Spend it, and it disarms on its own.
        for k in 20..60 {
            d.scroll.step((0.0, 0.0), &c.scroll, at(k * 4));
        }
        assert!(!d.scroll.settling());
        assert!(!d.armed(&c));
    }

    #[test]
    fn a_controller_frame_never_arms_the_deadline() {
        let c = cfg();
        let mut d = StickDrive::new();
        // The controller's sticks are for flicks; its pads drive the cursor. A stick
        // held right over on a controller frame must not start integrating.
        let controller = crate::report::Frame {
            source: crate::report::Source::SteamController,
            right_stick: (30_000, 0),
            ..Default::default()
        };
        d.observe(&controller, &c, at(0));
        assert!(!d.armed(&c));
        assert_eq!(d.deadline(), None);
        assert_eq!(d.sticks, Deflection::default(), "a padded source parks the sticks");
    }

    #[test]
    fn disabling_the_section_disarms_everything() {
        let mut c = cfg();
        c.enabled = false;
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((0, 0), (30_000, 0)), &c, at(0));
        assert!(!d.armed(&c));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn stepping_reschedules_only_while_something_moves() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 0.0;
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((0, 0), (30_000, 0)), &c, at(0));
        d.stepped(&c, at(4));
        assert_eq!(d.deadline(), Some(at(4) + Duration::from_millis(c.tick_ms)));
        d.observe(&evdev_frame((0, 0), (0, 0)), &c, at(8));
        d.stepped(&c, at(8));
        assert_eq!(d.deadline(), None, "nothing moving, nothing scheduled");
    }

    #[test]
    fn release_drops_every_claim() {
        let mut c = cfg();
        c.cursor.smoothing_ms = 0.0;
        let mut d = StickDrive::new();
        d.observe(&evdev_frame((0, 0), (30_000, 0)), &c, at(0));
        d.cursor.step((1.0, 0.0), &c.cursor, at(0));
        d.cursor.step((1.0, 0.0), &c.cursor, at(4));
        d.osk_left.step((1.0, 0.0), &c.osk, at(0));
        d.release();
        assert_eq!(d.deadline(), None);
        assert_eq!(d.sticks, Deflection::default());
        assert_eq!(d.osk_left.position(), (0.0, 0.0));
        assert!(!d.cursor.settling());
        assert!(!d.armed(&c));
    }

    #[test]
    fn deflection_normalises_and_keeps_the_controllers_y_up_convention() {
        let f = evdev_frame((-32767, 32767), (16383, -16383));
        let d = Deflection::of(&f);
        assert!((d.left.0 + 1.0).abs() < 1e-9 && (d.left.1 - 1.0).abs() < 1e-9);
        assert!((d.right.0 - 0.5).abs() < 1e-3 && (d.right.1 + 0.5).abs() < 1e-3);
    }
}
