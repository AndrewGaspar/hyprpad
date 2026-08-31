//! Per-pad cursor smoothing: a One Euro Filter plus moving-center hysteresis and
//! sub-pixel accumulation.
//!
//! The 2026 Steam Controller trackpads report absolute capacitive touch position
//! (`i16`, roughly ±32767 per axis) at ~250 Hz. Turning that into a *relative*
//! cursor by differencing consecutive frames high-pass-amplifies the sensor's
//! per-frame noise, so a finger held perfectly still makes the cursor swim (see
//! `docs/research/pointer-damping.md` §1). The fix, per that doc, is to smooth
//! the *absolute position* before differencing.
//!
//! The pipeline here follows the doc's recommendation:
//!
//! 1. normalize the raw axis to roughly `[-1, 1]` (`coord / 32767`), so the
//!    literature `beta` transfers directly (doc §2.3);
//! 2. a per-axis [`OneEuroFilter`] — a speed-adaptive low-pass that crushes
//!    jitter when slow/still and gets out of the way when fast (doc §2.3);
//! 3. per-axis moving-center **hysteresis** (libinput's `evdev_hysteresis`,
//!    doc §2.6), which guarantees a *hard zero* for a truly motionless finger —
//!    the one thing a low-pass alone cannot promise;
//! 4. **sub-pixel accumulation** on the relative-delta path (doc §2.7), so slow,
//!    deliberate motion is never lost to integer rounding.
//!
//! [`PadDamper::update`] runs stages 1–3 and returns the smoothed normalized
//! position; that single output feeds *both* consumers — the desktop cursor
//! (which differences it via [`PadDamper::relative`], stage 4) and the OSK
//! cursor forwarding (which sends it straight through). [`PadDamper::reset`]
//! clears all state and is called on touch-lift, exactly where the old raw path
//! reset its `prev`.
//!
//! All numeric parameters are *tuning starting points*, not tuned values; the
//! defaults live in [`crate::config::CursorConfig`].

use std::f64::consts::PI;
use std::time::Instant;

/// The pad axis full-scale used to normalize raw counts to `[-1, 1]`. Matches
/// [`crate::osk::normalize_pad_axis`] so the desktop and OSK cursors share one
/// coordinate convention.
const PAD_FULL_SCALE: f64 = 32767.0;

/// The exponential-smoothing factor `alpha` for a one-pole low-pass at `cutoff`
/// Hz, sampled every `dt` seconds. `alpha = r / (r + 1)` with `r = 2*pi*fc*dt`
/// (Casiez et al. / gery.casiez.net/1euro).
fn smoothing_factor(dt: f64, cutoff: f64) -> f64 {
    let r = 2.0 * PI * cutoff * dt;
    r / (r + 1.0)
}

/// One exponential-smoothing step: `a*x + (1-a)*x_prev`.
fn exp_smooth(a: f64, x: f64, x_prev: f64) -> f64 {
    a * x + (1.0 - a) * x_prev
}

/// A single-axis One Euro Filter (Casiez, Roussel & Vogel, CHI 2012).
///
/// Two coupled low-passes: one on the signal, one on its derivative, where the
/// derivative drives an adaptive cutoff `min_cutoff + beta*|dx_hat|`. Slow/still
/// input → low cutoff → heavy smoothing; fast input → high cutoff → little lag.
///
/// Operate it on a signal normalized to roughly `[-1, 1]` so the published
/// `beta` values transfer (doc §2.3). `Te`/`dt` is the *measured* interval
/// between samples, passed per call, so dropped frames don't distort the cutoff.
#[derive(Clone, Debug)]
pub struct OneEuroFilter {
    /// Cutoff floor at zero speed (Hz); sets the held-still smoothing. Must be
    /// `> 0`.
    min_cutoff: f64,
    /// Speed coefficient (per normalized-unit/second); sets the lag during fast
    /// motion. `0.0` reduces the filter to a fixed low-pass at `min_cutoff`.
    beta: f64,
    /// Fixed cutoff for the derivative low-pass (Hz).
    d_cutoff: f64,
    /// Last smoothed value; `None` until the first sample seeds it.
    x_prev: Option<f64>,
    /// Last smoothed derivative (normalized units per second).
    dx_prev: f64,
}

impl OneEuroFilter {
    /// A filter with the given parameters, in the uninitialized state (the first
    /// [`filter`](Self::filter) call seeds it and returns the input unchanged).
    pub fn new(min_cutoff: f64, beta: f64, d_cutoff: f64) -> OneEuroFilter {
        OneEuroFilter {
            min_cutoff,
            beta,
            d_cutoff,
            x_prev: None,
            dx_prev: 0.0,
        }
    }

    /// Feed one sample taken `dt` seconds after the previous one and return the
    /// smoothed estimate.
    ///
    /// The first sample after construction or [`reset`](Self::reset) seeds the
    /// state and is returned unchanged (derivative 0), so there is no startup
    /// transient. A non-positive `dt` (duplicate/re-ordered timestamp) is treated
    /// as "no time elapsed": the current estimate is returned without an update,
    /// which keeps the derivative division safe.
    pub fn filter(&mut self, x: f64, dt: f64) -> f64 {
        let Some(x_prev) = self.x_prev else {
            self.x_prev = Some(x);
            self.dx_prev = 0.0;
            return x;
        };
        if dt <= 0.0 {
            return x_prev;
        }
        // 1. derivative of the signal, low-passed at the fixed d_cutoff.
        let dx = (x - x_prev) / dt;
        let a_d = smoothing_factor(dt, self.d_cutoff);
        let dx_hat = exp_smooth(a_d, dx, self.dx_prev);
        // 2. adaptive cutoff rises with |speed|.
        let cutoff = self.min_cutoff + self.beta * dx_hat.abs();
        // 3. low-pass the signal with that adaptive cutoff.
        let a = smoothing_factor(dt, cutoff);
        let x_hat = exp_smooth(a, x, x_prev);
        self.x_prev = Some(x_hat);
        self.dx_prev = dx_hat;
        x_hat
    }

    /// Drop all history so the next sample re-seeds the filter (used on lift).
    pub fn reset(&mut self) {
        self.x_prev = None;
        self.dx_prev = 0.0;
    }
}

/// Per-axis moving-center hysteresis (libinput `evdev_hysteresis`, doc §2.6).
///
/// If the input is within `±margin` of the center, the center is returned
/// unchanged (→ zero delta → no motion). Once the input leaves the margin, the
/// output is `input ∓ margin` — the center "drags" along the leading edge — and
/// that output becomes the center for the next frame. A `None` center seeds to
/// the first input. `margin == 0` is a pass-through (never clamps to a hard
/// zero, but still cheap and inert).
fn hysteresis(input: f64, center: &mut Option<f64>, margin: f64) -> f64 {
    match *center {
        None => {
            *center = Some(input);
            input
        }
        Some(c) => {
            let diff = input - c;
            let out = if diff.abs() <= margin {
                c
            } else if diff > 0.0 {
                input - margin
            } else {
                input + margin
            };
            *center = Some(out);
            out
        }
    }
}

/// A per-pad smoothing damper: two [`OneEuroFilter`]s (x, y) plus moving-center
/// hysteresis and, for the relative-delta path, sub-pixel accumulation.
///
/// [`update`](Self::update) produces the smoothed normalized position that feeds
/// *both* pad consumers. The desktop-cursor consumer additionally calls
/// [`relative`](Self::relative) to difference successive smoothed positions into
/// integer pixel deltas; the OSK consumer forwards the normalized position as
/// is. [`reset`](Self::reset) clears everything on touch-lift.
#[derive(Clone, Debug)]
pub struct PadDamper {
    fx: OneEuroFilter,
    fy: OneEuroFilter,
    /// Hysteresis centers for the shared smoothed position (one per axis).
    hcx: Option<f64>,
    hcy: Option<f64>,
    /// Shared hysteresis margin, in normalized units.
    hysteresis: f64,
    /// Extra dead-band margin applied *only* on the relative (desktop-cursor)
    /// path, on top of `hysteresis`; lets the desktop cursor sit steadier than
    /// the OSK cursor. `0.0` = no extra dead-band.
    deadzone: f64,
    /// Dead-band centers for the relative path's extra `deadzone` stage.
    dcx: Option<f64>,
    dcy: Option<f64>,
    /// Previous post-deadzone smoothed position, for differencing on the
    /// relative path. `None` clutches the first touched frame to zero motion.
    prev: Option<(f64, f64)>,
    /// Fractional-pixel remainders carried across frames (doc §2.7).
    acc_x: f64,
    acc_y: f64,
    /// Timestamp of the last accepted sample, for the measured `dt`.
    last_t: Option<Instant>,
}

impl PadDamper {
    /// Build a damper from its tuning parameters. `min_cutoff`, `beta` and
    /// `d_cutoff` configure both per-axis One Euro filters; `hysteresis` is the
    /// shared moving-center margin; `deadzone` is the relative-path extra margin.
    /// All margins are in normalized (`[-1, 1]`) units.
    pub fn new(
        min_cutoff: f64,
        beta: f64,
        d_cutoff: f64,
        hysteresis: f64,
        deadzone: f64,
    ) -> PadDamper {
        PadDamper {
            fx: OneEuroFilter::new(min_cutoff, beta, d_cutoff),
            fy: OneEuroFilter::new(min_cutoff, beta, d_cutoff),
            hcx: None,
            hcy: None,
            hysteresis,
            deadzone,
            dcx: None,
            dcy: None,
            prev: None,
            acc_x: 0.0,
            acc_y: 0.0,
            last_t: None,
        }
    }

    /// Smooth one raw touched frame and return the normalized `[-1, 1]` position
    /// (One Euro + hysteresis). This is the single smoothed-position source used
    /// by *both* the desktop cursor and the OSK cursor forwarding.
    pub fn update(&mut self, raw_x: i16, raw_y: i16, now: Instant) -> (f64, f64) {
        let dt = match self.last_t {
            Some(prev_t) => now.saturating_duration_since(prev_t).as_secs_f64(),
            None => 0.0,
        };
        self.last_t = Some(now);

        let nx = normalize(raw_x);
        let ny = normalize(raw_y);
        let sx = self.fx.filter(nx, dt);
        let sy = self.fy.filter(ny, dt);
        let hx = hysteresis(sx, &mut self.hcx, self.hysteresis);
        let hy = hysteresis(sy, &mut self.hcy, self.hysteresis);
        (hx, hy)
    }

    /// Difference the smoothed position `s` (as returned by [`update`](Self::update))
    /// into integer pixel deltas for `zwlr_virtual_pointer` relative motion.
    ///
    /// Applies the extra `deadzone` dead-band, differences against the previous
    /// frame, scales the normalized delta back to counts (`* 32767`) then to
    /// pixels (`* sens`) so `sens` keeps its px-per-count meaning, and carries
    /// the sub-pixel remainder across frames. `invert_y` negates the y delta
    /// (pad `+Y` is up, screen `+Y` is down). The first touched frame after a
    /// reset seeds `prev` and returns `(0, 0)` (the clutch).
    pub fn relative(&mut self, s: (f64, f64), sens: f64, invert_y: bool) -> (i32, i32) {
        let dx_axis = hysteresis(s.0, &mut self.dcx, self.deadzone);
        let dy_axis = hysteresis(s.1, &mut self.dcy, self.deadzone);

        let Some((px, py)) = self.prev else {
            self.prev = Some((dx_axis, dy_axis));
            return (0, 0);
        };
        self.prev = Some((dx_axis, dy_axis));

        let dx = (dx_axis - px) * PAD_FULL_SCALE * sens;
        let mut dy = (dy_axis - py) * PAD_FULL_SCALE * sens;
        if invert_y {
            dy = -dy;
        }

        self.acc_x += dx;
        self.acc_y += dy;
        let out_x = self.acc_x.trunc();
        let out_y = self.acc_y.trunc();
        self.acc_x -= out_x;
        self.acc_y -= out_y;
        (out_x as i32, out_y as i32)
    }

    /// Drop all filter, hysteresis, differencing and accumulator state. Called on
    /// touch-lift so a lift-and-retouch never carries stale state (no jump, no
    /// wrong initial cutoff), exactly like the old raw path's `prev = None`.
    pub fn reset(&mut self) {
        self.fx.reset();
        self.fy.reset();
        self.hcx = None;
        self.hcy = None;
        self.dcx = None;
        self.dcy = None;
        self.prev = None;
        self.acc_x = 0.0;
        self.acc_y = 0.0;
        self.last_t = None;
    }
}

/// Normalize a raw pad axis count to roughly `[-1, 1]` (`coord / 32767`,
/// clamped). Mirrors [`crate::osk::normalize_pad_axis`] in `f64`.
fn normalize(v: i16) -> f64 {
    (f64::from(v) / PAD_FULL_SCALE).clamp(-1.0, 1.0)
}

/// Accumulates a finger's angular travel around the pad centre and emits one
/// integer *tick* for every `step` radians of accumulated rotation. Positive
/// ticks are counter-clockwise (increasing `atan2` angle); negative ticks are
/// clockwise. This is the engine behind the circular ("radial") scroll mode; the
/// caller maps the signed tick count to a scroll direction.
///
/// It handles the two hazards of turning an absolute angle into a rotation
/// count:
///
/// * **the ±π wrap** — the per-frame angle delta is reduced to `(-π, π]`
///   ([`wrap_pi`]), so a finger crossing the `atan2` branch cut (the −x axis)
///   contributes its true small step, not a ~2π spurious jump;
/// * **the ill-defined centre** — within `min_radius` of the pad centre the
///   angle is meaningless, so those samples emit nothing *and* drop the
///   reference angle, so re-emerging on the far side does not inject a spurious
///   step. Sub-tick progress is preserved across such a dropout.
///
/// Feed it the *smoothed* normalized position from [`PadDamper::update`], so the
/// angle (and hence the tick rate) is not driven by sensor jitter.
/// [`reset`](Self::reset) clears it on touch-lift.
#[derive(Clone, Debug)]
pub struct AngleAccumulator {
    /// Radians of rotation per emitted tick. Kept strictly positive.
    step: f64,
    /// Minimum radius (same normalized units as the fed position) for the angle
    /// to be considered valid.
    min_radius: f64,
    /// Previous accepted frame's angle; `None` when unseeded or last seen near
    /// the centre.
    prev: Option<f64>,
    /// Signed accumulated rotation not yet emitted as whole ticks (radians).
    accum: f64,
}

impl AngleAccumulator {
    /// Build an accumulator emitting one tick per `step_radians` of rotation and
    /// ignoring samples within `min_radius` of the centre. A non-positive
    /// `step_radians` is floored to a tiny positive value so [`update`](Self::update)
    /// can never divide by zero (a degenerate config still behaves, just very
    /// finely); a negative `min_radius` is clamped to `0`.
    pub fn new(step_radians: f64, min_radius: f64) -> AngleAccumulator {
        AngleAccumulator {
            step: if step_radians > 0.0 { step_radians } else { f64::EPSILON },
            min_radius: min_radius.max(0.0),
            prev: None,
            accum: 0.0,
        }
    }

    /// Feed one smoothed normalized position and return the signed number of
    /// whole ticks its rotation completed since the last call: `0` when the
    /// finger is near the centre, unseeded, or has not yet turned a full `step`;
    /// `> 0` counter-clockwise, `< 0` clockwise.
    pub fn update(&mut self, x: f64, y: f64) -> i32 {
        // Near dead-centre the angle is ill-defined: emit nothing and drop the
        // reference so re-emergence re-seeds without a spurious delta. Keep the
        // sub-tick accumulator so a brief dip through the centre loses no
        // progress.
        if (x * x + y * y).sqrt() < self.min_radius {
            self.prev = None;
            return 0;
        }
        let angle = y.atan2(x);
        let Some(prev) = self.prev else {
            // First valid sample only seeds the reference: no rotation yet.
            self.prev = Some(angle);
            return 0;
        };
        self.prev = Some(angle);
        self.accum += wrap_pi(angle - prev);
        // Emit as many whole ticks as have accumulated, truncating toward zero
        // and carrying the sub-tick remainder to the next call.
        let ticks = (self.accum / self.step).trunc();
        self.accum -= ticks * self.step;
        ticks as i32
    }

    /// Drop all state (used on touch-lift), so a re-touch starts fresh.
    pub fn reset(&mut self) {
        self.prev = None;
        self.accum = 0.0;
    }
}

/// Reduce an angle difference to the equivalent value in `(-π, π]`, so a delta
/// that appears to leap across the `atan2` branch cut is read as the short way
/// round. At 250 Hz a real per-frame delta is tiny, so each loop runs at most
/// once; the loop form still does the right thing for an unusually large jump.
fn wrap_pi(mut a: f64) -> f64 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a <= -PI {
        a += 2.0 * PI;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A 250 Hz sample clock: frame `i` lands 4 ms after frame 0.
    fn at(t0: Instant, i: u32) -> Instant {
        t0 + Duration::from_millis(4 * u64::from(i))
    }

    /// Deterministic small "noise" in raw counts, bounded to `±amp`.
    fn noise(i: u32, amp: i16) -> i16 {
        // A cheap pseudo-random-ish wobble; sign and magnitude both vary.
        let n = ((i.wrapping_mul(2654435761)) >> 24) as i16;
        (n % (2 * amp + 1)) - amp
    }

    #[test]
    fn one_euro_first_sample_is_passthrough() {
        let mut f = OneEuroFilter::new(1.0, 1.0, 1.0);
        // First sample seeds state and is returned unchanged.
        assert_eq!(f.filter(0.5, 0.004), 0.5);
    }

    #[test]
    fn one_euro_constant_input_converges_and_holds() {
        let mut f = OneEuroFilter::new(1.0, 0.0, 1.0);
        let mut y = 0.0;
        for _ in 0..500 {
            y = f.filter(0.3, 0.004);
        }
        // A constant input must converge exactly to that constant (unit DC gain).
        assert!((y - 0.3).abs() < 1e-9, "converged to {y}, want 0.3");
    }

    #[test]
    fn one_euro_nonpositive_dt_is_safe() {
        let mut f = OneEuroFilter::new(1.0, 1.0, 1.0);
        assert_eq!(f.filter(0.2, 0.004), 0.2); // seed
        // Duplicate timestamp: returns the current estimate, no div-by-zero.
        let y = f.filter(0.9, 0.0);
        assert_eq!(y, 0.2);
    }

    #[test]
    fn one_euro_reset_clears_state() {
        let mut f = OneEuroFilter::new(1.0, 1.0, 1.0);
        for i in 0..50 {
            f.filter(0.5 + 0.01 * f64::from(i), 0.004);
        }
        f.reset();
        // After reset the next sample is a fresh seed (passthrough).
        assert_eq!(f.filter(-0.4, 0.004), -0.4);
    }

    #[test]
    fn damper_first_frame_emits_no_motion() {
        let t0 = Instant::now();
        let mut d = PadDamper::new(1.0, 1.0, 1.0, 0.0002, 0.0);
        let s = d.update(1000, -2000, at(t0, 0));
        assert_eq!(d.relative(s, 0.06, true), (0, 0), "first frame is the clutch");
    }

    #[test]
    fn damper_held_still_noise_stays_put_no_drift() {
        // A finger held at a fixed spot, with a few counts of per-frame noise,
        // must not accumulate net cursor motion (the core "swim" bug).
        let t0 = Instant::now();
        let mut d = PadDamper::new(1.0, 1.0, 1.0, 0.0004, 0.0);
        let (cx, cy) = (5000i16, -3000i16);
        let mut total_x = 0i64;
        let mut total_y = 0i64;
        for i in 0..2000 {
            let x = cx + noise(i, 3);
            let y = cy + noise(i.wrapping_add(101), 3);
            let s = d.update(x, y, at(t0, i));
            let (ox, oy) = d.relative(s, 0.06, true);
            total_x += i64::from(ox);
            total_y += i64::from(oy);
        }
        // Net drift over 2000 frames (~8 s) must be a handful of pixels at most,
        // not the relentless crawl of the raw differencing path.
        assert!(total_x.abs() <= 2, "x drifted {total_x} px");
        assert!(total_y.abs() <= 2, "y drifted {total_y} px");
    }

    #[test]
    fn damper_smoothed_position_settles_for_a_still_finger() {
        // The shared smoothed position (what the OSK cursor forwards) must stop
        // moving for a still-but-noisy finger, so the on-screen cursor is steady.
        let t0 = Instant::now();
        let mut d = PadDamper::new(1.0, 1.0, 1.0, 0.0004, 0.0);
        let mut last = (0.0, 0.0);
        for i in 0..1500 {
            let x = 8000 + noise(i, 3);
            let y = 8000 + noise(i.wrapping_add(7), 3);
            last = d.update(x, y, at(t0, i));
        }
        let n = 8000.0 / PAD_FULL_SCALE;
        // Converged near the true normalized position, within the noise/margin.
        assert!((last.0 - n).abs() < 0.01, "x settled at {}", last.0);
        assert!((last.1 - n).abs() < 0.01, "y settled at {}", last.1);
    }

    #[test]
    fn damper_tracks_a_step_with_bounded_lag() {
        // Hold at one spot, then jump to another and hold: the cursor must travel
        // (close to) the full geometric distance, and finish tracking (net motion
        // stops) within a bounded number of frames — "bounded lag", no overshoot.
        let t0 = Instant::now();
        let sens = 0.06;
        let mut d = PadDamper::new(1.0, 1.0, 1.0, 0.0, 0.0);
        let (a, b) = (0i16, 12000i16);

        // Settle at A.
        for i in 0..200 {
            let s = d.update(a, a, at(t0, i));
            d.relative(s, sens, true);
        }
        // Step to B and integrate the emitted motion.
        let mut moved_x = 0i64;
        let mut frames_to_settle = None;
        let start = 200u32;
        for i in start..start + 400 {
            let s = d.update(b, a, at(t0, i));
            let (ox, _oy) = d.relative(s, sens, true);
            moved_x += i64::from(ox);
            if ox == 0 && i > start + 5 && frames_to_settle.is_none() {
                frames_to_settle = Some(i - start);
            }
        }
        let settle_frame = frames_to_settle.expect("cursor should stop moving after the step");
        // Total travel ~= (B - A) counts * sens, within rounding/lag slack.
        let want = f64::from(b - a) * sens; // 720 px
        assert!(
            (moved_x as f64 - want).abs() < 3.0,
            "moved {moved_x} px, want ~{want}"
        );
        // Bounded lag: fully caught up well under a second at 250 Hz.
        assert!(settle_frame < 250, "took {settle_frame} frames to settle");
    }

    #[test]
    fn damper_reset_clears_state() {
        let t0 = Instant::now();
        let mut d = PadDamper::new(1.0, 1.0, 1.0, 0.0002, 0.0);
        for i in 0..100 {
            let s = d.update(4000 + i as i16, 4000, at(t0, i));
            d.relative(s, 0.06, true);
        }
        d.reset();
        // Post-reset: first frame re-seeds (passthrough position) and clutches.
        let s = d.update(-6000, 6000, at(t0, 200));
        assert!((s.0 - normalize(-6000)).abs() < 1e-9, "not re-seeded: {}", s.0);
        assert_eq!(d.relative(s, 0.06, true), (0, 0), "clutch not restored");
    }

    #[test]
    fn damper_sub_pixel_motion_is_not_lost() {
        // A slow drift smaller than a pixel per frame must still move the cursor
        // eventually, thanks to sub-pixel accumulation.
        let t0 = Instant::now();
        // No smoothing floor fighting us: use a light filter and no dead-band.
        let mut d = PadDamper::new(30.0, 0.0, 1.0, 0.0, 0.0);
        let sens = 0.06;
        let mut moved = 0i64;
        // Drift ~2 counts/frame → 0.12 px/frame: truncates to 0 every frame if
        // the remainder were discarded, but must accumulate into real motion.
        for i in 0..500 {
            let x = 2 * i as i16;
            let s = d.update(x, 0, at(t0, i));
            let (ox, _) = d.relative(s, sens, true);
            moved += i64::from(ox);
        }
        assert!(moved > 40, "slow drift lost to rounding: moved only {moved} px");
    }

    #[test]
    fn hysteresis_freezes_within_margin_and_drags_outside() {
        let mut c = None;
        assert_eq!(hysteresis(0.10, &mut c, 0.05), 0.10); // seed
        // Within margin: output pinned to center.
        assert_eq!(hysteresis(0.12, &mut c, 0.05), 0.10);
        assert_eq!(hysteresis(0.14, &mut c, 0.05), 0.10);
        // Leaves the margin upward: drags to input - margin.
        let out = hysteresis(0.20, &mut c, 0.05);
        assert!((out - 0.15).abs() < 1e-9, "dragged to {out}, want 0.15");
    }

    /// A point on a circle of radius `r` at `deg` degrees.
    fn on_circle(r: f64, deg: f64) -> (f64, f64) {
        let a = deg.to_radians();
        (r * a.cos(), r * a.sin())
    }

    #[test]
    fn wrap_pi_maps_into_range() {
        assert!((wrap_pi(0.0)).abs() < 1e-12);
        // A +350° apparent jump is really −10°.
        assert!((wrap_pi(350f64.to_radians()) - (-10f64).to_radians()).abs() < 1e-12);
        // A −350° apparent jump is really +10°.
        assert!((wrap_pi((-350f64).to_radians()) - 10f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn angle_accumulator_clockwise_emits_down_ticks() {
        // Trace a clockwise arc of 90.5° at a safe radius. With a 15° step that
        // is exactly 6 whole ticks, all negative (clockwise); never a positive.
        let step = 15f64.to_radians();
        let mut acc = AngleAccumulator::new(step, 0.35);
        let mut total = 0i32;
        let mut saw_positive = false;
        // 0° down to −90.5° in 0.5° clockwise increments (182 samples).
        for i in 0..=181 {
            let deg = -(f64::from(i)) * 0.5;
            let t = acc.update_pt(on_circle(0.5, deg));
            if t > 0 {
                saw_positive = true;
            }
            total += t;
        }
        assert!(!saw_positive, "clockwise motion must never emit an up-tick");
        assert_eq!(total, -6, "90.5° / 15° = 6 down-ticks");
    }

    #[test]
    fn angle_accumulator_handles_pi_wrap() {
        // A counter-clockwise sweep crossing the +π branch cut (135° → 226°)
        // must accumulate its true rotation, not a ~2π spurious jump: 91° / 15°
        // = 6 up-ticks. Without wrap handling the crossing frame would inject a
        // −360° step and wreck the count.
        let step = 15f64.to_radians();
        let mut acc = AngleAccumulator::new(step, 0.35);
        let mut total = 0i32;
        for i in 0..=182 {
            let deg = 135.0 + f64::from(i) * 0.5; // 135 .. 226, crossing 180
            total += acc.update_pt(on_circle(0.6, deg));
        }
        assert_eq!(total, 6, "91° CCW across the branch cut = 6 up-ticks");
    }

    #[test]
    fn angle_accumulator_ignores_near_center() {
        // A full rotation entirely inside min_radius emits nothing: near the
        // dead centre the angle is ill-defined.
        let step = 15f64.to_radians();
        let mut acc = AngleAccumulator::new(step, 0.35);
        let mut total = 0i32;
        for i in 0..=360 {
            total += acc.update_pt(on_circle(0.1, f64::from(i)));
        }
        assert_eq!(total, 0, "rotation near dead-centre must not scroll");
    }

    #[test]
    fn angle_accumulator_no_jump_leaving_center() {
        // Seeding at/near the centre then jumping out must not emit a spurious
        // tick from the meaningless centre angle.
        let step = 15f64.to_radians();
        let mut acc = AngleAccumulator::new(step, 0.35);
        assert_eq!(acc.update(0.0, 0.0), 0, "exact centre: no angle, no tick");
        assert_eq!(acc.update(0.01, -0.02), 0, "still inside min_radius");
        // First valid sample only seeds the reference angle: still no tick.
        assert_eq!(acc.update_pt(on_circle(0.9, 0.0)), 0, "re-emergence seeds only");
        // A following 16° CCW step (> the 15° step) now emits exactly one up-tick.
        assert_eq!(acc.update_pt(on_circle(0.9, 16.0)), 1);
    }

    #[test]
    fn angle_accumulator_reset_clears_progress() {
        let step = 15f64.to_radians();
        let mut acc = AngleAccumulator::new(step, 0.35);
        // Build up ~10° of clockwise progress (below one tick), then reset.
        acc.update_pt(on_circle(0.5, 0.0));
        acc.update_pt(on_circle(0.5, -10.0));
        acc.reset();
        // Post-reset the next sample only re-seeds; a fresh 16° CW step is one
        // down-tick, proving the pre-reset 10° did not carry over.
        assert_eq!(acc.update_pt(on_circle(0.5, 0.0)), 0);
        assert_eq!(acc.update_pt(on_circle(0.5, -16.0)), -1);
    }

    // Small convenience so the arc tests read as a stream of points.
    impl AngleAccumulator {
        fn update_pt(&mut self, p: (f64, f64)) -> i32 {
            self.update(p.0, p.1)
        }
    }
}
