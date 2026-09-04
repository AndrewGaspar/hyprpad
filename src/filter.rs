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
use std::time::{Duration, Instant};

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
    /// The wrapped angle delta the most recent [`update`](Self::update)
    /// contributed (radians), for a consumer that wants the *rate* as well as
    /// the count ([`JogPacer`]). `0.0` whenever the sample contributed nothing
    /// — near the centre, unseeded, or after a [`reset`](Self::reset).
    last_delta: f64,
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
            last_delta: 0.0,
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
            self.last_delta = 0.0;
            return 0;
        }
        let angle = y.atan2(x);
        let Some(prev) = self.prev else {
            // First valid sample only seeds the reference: no rotation yet.
            self.prev = Some(angle);
            self.last_delta = 0.0;
            return 0;
        };
        self.prev = Some(angle);
        let delta = wrap_pi(angle - prev);
        self.last_delta = delta;
        self.accum += delta;
        // Emit as many whole ticks as have accumulated, truncating toward zero
        // and carrying the sub-tick remainder to the next call.
        let ticks = (self.accum / self.step).trunc();
        self.accum -= ticks * self.step;
        ticks as i32
    }

    /// The rotation the most recent [`update`](Self::update) contributed, in
    /// radians, signed the same way its return value is (`> 0` counter-
    /// clockwise). `0.0` when that sample emitted nothing *and* moved nothing —
    /// near the dead centre, on the seeding sample, and after a
    /// [`reset`](Self::reset).
    ///
    /// The tick count alone cannot tell a slow quarter-turn from a fast one:
    /// this is how [`JogPacer`] gets the angular *rate* out of the same pass,
    /// without a second `atan2` or a second copy of the wrap handling.
    pub fn last_delta(&self) -> f64 {
        self.last_delta
    }

    /// Drop all state (used on touch-lift), so a re-touch starts fresh.
    pub fn reset(&mut self) {
        self.prev = None;
        self.accum = 0.0;
        self.last_delta = 0.0;
    }
}

/// The unit one jog detent moves the caret by ([`JogPacer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JogUnit {
    /// One or more plain arrow taps — `count` characters.
    Char,
    /// One `ctrl`+arrow tap: the caret jumps a whole word, and how far that is
    /// is the text's business, not ours.
    Word,
}

/// What one detent emits: a unit and how many of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JogStep {
    pub unit: JogUnit,
    /// Taps to send for this detent. Always `1` for [`JogUnit::Word`].
    pub count: u32,
}

/// The rung the pacer is on. Not public: the caller reads [`JogStep`], which is
/// the rung's *effect*, so the ladder can grow one without a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    /// One character per detent — the landing tier, and where every spin starts.
    One,
    /// Two characters per detent.
    Two,
    /// The top rung: four characters, or one word when the word tier is on.
    Top,
}

/// The cutoff (Hz) of the low-pass on the angular-rate estimate.
///
/// The rate is differenced from one 4 ms frame to the next, so the raw estimate
/// is as noisy as the smoothed angle's last digit; ~5 Hz is slow enough to be
/// steady under a thumb and fast enough that a deliberate acceleration reaches
/// the threshold within a detent or two (`docs/research/pointer-damping.md`
/// §2.2 — α ≈ 0.11 at 250 Hz).
const RATE_CUTOFF_HZ: f64 = 5.0;

/// Where the top rung sits relative to the configured `fast`/`slow` pair, as a
/// multiplier on **both** — so one 2:1 hysteresis gap is configured and the
/// whole ladder keeps its shape however it is tuned.
///
/// The character ladder doubles (Apple's staged ×1 → ×2 → ×4, US 7,312,785), so
/// ×4 wants twice ×2's speed: 720 °/s in, 360 °/s out at the defaults. The word
/// tier replaces it lower down (540 / 270), because a word is worth ≈ ×6 and
/// should be reachable without spinning twice as fast as ×2 asks for —
/// `docs/research/text-scrub.md` §3.5.
const TOP_RATIO_CHAR: f64 = 2.0;
/// The word tier's multiplier on `fast`/`slow`; see [`TOP_RATIO_CHAR`].
const TOP_RATIO_WORD: f64 = 1.5;

/// Angular velocity → how far one detent moves the caret: the speed ladder
/// behind the guide-layer text scrub (`docs/research/text-scrub.md` §3.5).
///
/// A jog wheel is a *position* control — the count is exact and the rate follows
/// the hand — so acceleration must multiply the **count**, never the rate
/// (Apple's accelerated-scrolling patent, §2.4 of that doc). This is the state
/// machine that picks the multiplier:
///
/// | tier | enters at | exits at | one detent emits |
/// |---|---|---|---|
/// | ×1 | — | — | 1 × `Left`/`Right` |
/// | ×2 | `fast` (360 °/s) for `arm` detents | `slow` (180 °/s) | 2 taps |
/// | top | `fast × ratio` for `arm` (+1 for words) detents | `slow × ratio` | 4 taps, or one `ctrl`+arrow |
///
/// Three properties, each of them a lesson from the prior art:
///
/// * **a 2:1 enter/exit gap** (Logitech's SmartShift ratchet/free-spin
///   threshold): a thumb hovering at the threshold cannot chatter between units;
/// * **arming over consecutive detents**: a single fast flick never jumps a
///   tier, so the tier is a property of the *spin*, not of one frame;
/// * **one rung at a time on the way down**: dropping below the top rung's exit
///   speed lands on ×2, not on ×1, so slowing to a stop walks the ladder down
///   and the last detents before the target are always ×1.
///
/// Pure and device-free: [`observe`](Self::observe) takes the angle delta of
/// each frame as it arrives and [`detent`](Self::detent) is called once per
/// detent the [`AngleAccumulator`] emitted, in order.
#[derive(Clone, Debug)]
pub struct JogPacer {
    /// Enter-×2 speed, °/s.
    fast: f64,
    /// Exit-×2 speed, °/s. The gap to `fast` is the hysteresis.
    slow: f64,
    /// Consecutive detents at speed required to climb to ×2.
    arm: u32,
    /// Whether the top rung is the word tier (`ctrl`+arrow) rather than ×4.
    word: bool,
    /// Low-passed |ω| in °/s.
    speed: f64,
    /// Which rung we are on.
    tier: Tier,
    /// Consecutive detents seen at the *next* rung's enter speed.
    run: u32,
}

impl JogPacer {
    /// Build a pacer. `fast`/`slow` are the ×2 enter/exit speeds in °/s (the top
    /// rung derives its own pair from them); `arm` is how many consecutive
    /// detents at speed it takes to climb, floored at 1 so a zero in a config
    /// cannot make the ladder jump on a single frame.
    pub fn new(fast_deg_per_s: f64, slow_deg_per_s: f64, arm: u32, word_tier: bool) -> JogPacer {
        let fast = fast_deg_per_s.max(0.0);
        JogPacer {
            fast,
            // Never above `fast`: an exit at or over the enter speed is not a
            // hysteresis band at all, it is a coin toss per detent.
            slow: slow_deg_per_s.clamp(0.0, fast),
            arm: arm.max(1),
            word: word_tier,
            speed: 0.0,
            tier: Tier::One,
            run: 0,
        }
    }

    /// Feed one frame: `d_theta` radians of rotation (signed;
    /// [`AngleAccumulator::last_delta`]) over `dt` seconds.
    ///
    /// Only the magnitude matters — a reversal is a change of direction, not a
    /// slow-down, and the accumulator already makes a reversal cost a full
    /// detent. A non-positive `dt` is ignored rather than divided by.
    pub fn observe(&mut self, d_theta: f64, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        let instant = d_theta.abs().to_degrees() / dt;
        let alpha = smoothing_factor(dt, RATE_CUTOFF_HZ);
        self.speed = exp_smooth(alpha, instant, self.speed);
    }

    /// The low-passed angular speed, °/s. Diagnostic: the ladder reads it itself.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Consume one detent: advance the ladder at the current speed and return
    /// what this detent emits.
    ///
    /// Climbing takes effect on the detent that completes the arming, so a
    /// transition never emits a burst of its own — the run simply gets longer
    /// steps from here on.
    pub fn detent(&mut self) -> JogStep {
        let (top_enter, top_exit) = self.top_band();
        match self.tier {
            Tier::One => {
                if self.speed > self.fast {
                    self.run += 1;
                    if self.run >= self.arm {
                        self.tier = Tier::Two;
                        self.run = 0;
                    }
                } else {
                    self.run = 0;
                }
            }
            Tier::Two => {
                if self.speed < self.slow {
                    self.tier = Tier::One;
                    self.run = 0;
                } else if self.speed > top_enter {
                    self.run += 1;
                    if self.run >= self.top_arm() {
                        self.tier = Tier::Top;
                        self.run = 0;
                    }
                } else {
                    self.run = 0;
                }
            }
            Tier::Top => {
                if self.speed < top_exit {
                    // One rung at a time: the tail of a spin is always ×1.
                    self.tier = Tier::Two;
                    self.run = 0;
                }
            }
        }
        self.step()
    }

    /// What the current rung emits per detent.
    fn step(&self) -> JogStep {
        match self.tier {
            Tier::One => JogStep { unit: JogUnit::Char, count: 1 },
            Tier::Two => JogStep { unit: JogUnit::Char, count: 2 },
            Tier::Top if self.word => JogStep { unit: JogUnit::Word, count: 1 },
            Tier::Top => JogStep { unit: JogUnit::Char, count: 4 },
        }
    }

    /// The top rung's (enter, exit) speeds, °/s.
    fn top_band(&self) -> (f64, f64) {
        let ratio = if self.word { TOP_RATIO_WORD } else { TOP_RATIO_CHAR };
        (self.fast * ratio, self.slow * ratio)
    }

    /// Detents of arming for the top rung. The word tier asks for one more than
    /// the character ladder does: changing the *unit* mid-spin is a bigger
    /// surprise than lengthening the step, so it wants more of a commitment
    /// (`docs/research/text-scrub.md` §3.5 — three detents at the defaults).
    fn top_arm(&self) -> u32 {
        if self.word {
            self.arm + 1
        } else {
            self.arm
        }
    }

    /// Drop the speed estimate and fall back to ×1 — on lift, on guide release,
    /// and wherever the accumulator is reset, so no spin inherits the last
    /// one's tier.
    pub fn reset(&mut self) {
        self.speed = 0.0;
        self.tier = Tier::One;
        self.run = 0;
    }
}

/// One shuttle tap: which way, and whether it moves a word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShuttleTap {
    /// Direction of travel. `true` taps `Right`.
    pub right: bool,
    /// A word jump (`ctrl`+arrow) rather than a character.
    pub word: bool,
}

/// Stick deflection → *when the next caret tap is due*: the shuttle behind the
/// text scrub on a controller with no trackpads
/// (`docs/research/text-scrub.md` §3.1, option (d)).
///
/// The [`JogPacer`]'s opposite number, and deliberately so. A jog wheel is a
/// **position** control, so acceleration there multiplies the *count* of a
/// detent that the hand has already earned. A stick has no position to spend —
/// it springs back — so it is a **rate** control: the deflection *is* the
/// speed, and this holds the one piece of state that follows from that, the
/// clock. Deflection in, at most one tap out per call, and the schedule for the
/// next one.
///
/// The curve, over |x| (the horizontal deflection, `0..=1`):
///
/// | deflection | emits |
/// |---|---|
/// | ≤ `deadzone` | nothing at all, and the run is parked |
/// | `deadzone` … `word_above` | arrows, `slow_per_s` rising linearly to `fast_per_s` |
/// | ≥ `word_above` | `ctrl`+arrows at `fast_per_s` |
///
/// Three properties worth naming:
///
/// * **the first tap is immediate.** Engaging (or reversing) taps on the spot
///   and schedules the *next* one, so a flick of the stick moves the caret one
///   character — the fine adjustment — without waiting out an interval;
/// * **a reversal is a fresh run.** Push the other way and the pending tap is
///   abandoned, not inherited: nothing is ever emitted in the direction the
///   stick has already left;
/// * **it never catches up.** A schedule missed by more than one interval (the
///   loop was busy, or the deadline was disarmed) re-bases on `now` instead of
///   firing a burst of arrows into the document.
///
/// Pure over [`Instant`]: no device, no clock of its own, so the whole feel is
/// unit-testable.
#[derive(Clone, Debug)]
pub struct ShuttlePacer {
    /// Deflection below which nothing is commanded.
    deadzone: f64,
    /// Taps/s at the deadzone edge.
    slow: f64,
    /// Taps/s at `word_above` and beyond.
    fast: f64,
    /// Deflection at or past which a tap is a word.
    word_above: f64,
    /// The direction of the run in progress. `None` while parked.
    dir: Option<bool>,
    /// When the next tap is due. `Some` exactly while `dir` is.
    due: Option<Instant>,
}

impl ShuttlePacer {
    /// Build a shuttle from the four config knobs, each clamped into the range
    /// that makes the curve a curve: a deadzone short of full scale, a `fast`
    /// no slower than `slow`, and a word threshold above the deadzone (a
    /// config that puts it at or below one means "every tap is a word", which
    /// is a legitimate thing to ask for and must not divide by zero).
    pub fn new(deadzone: f64, slow_per_s: f64, fast_per_s: f64, word_above: f64) -> ShuttlePacer {
        let deadzone = deadzone.clamp(0.0, 0.99);
        let slow = slow_per_s.max(0.0);
        ShuttlePacer {
            deadzone,
            slow,
            fast: fast_per_s.max(slow),
            word_above,
            dir: None,
            due: None,
        }
    }

    /// Taps per second at deflection `x` (signed; only the magnitude counts).
    /// `0.0` inside the deadzone, which is what parks the run.
    pub fn rate(&self, x: f64) -> f64 {
        let m = x.abs().min(1.0);
        if m <= self.deadzone {
            return 0.0;
        }
        // The live band is deadzone → word threshold; past it the rate is
        // simply the top, and it is the *unit* that changes.
        let span = (self.word_above - self.deadzone).max(1e-6);
        let u = ((m - self.deadzone) / span).clamp(0.0, 1.0);
        self.slow + (self.fast - self.slow) * u
    }

    /// Whether deflection `x` is past the deadzone — the arming question the
    /// daemon's receive deadline asks ([`crate::sticks::StickDrive`]).
    pub fn engaged(&self, x: f64) -> bool {
        x.abs().min(1.0) > self.deadzone
    }

    /// Whether a run is in progress, i.e. the last [`update`](Self::update) saw
    /// an engaged stick. The loop reads this to keep its 4 ms deadline armed: a
    /// gamepad reports only on change, so a stick *held* over says nothing at
    /// all and the taps have to come from the clock.
    pub fn running(&self) -> bool {
        self.dir.is_some()
    }

    /// When the next tap is due, or `None` while parked. Diagnostic — the
    /// daemon steps this on the stick clock, which is faster than any rate this
    /// can ask for.
    pub fn deadline(&self) -> Option<Instant> {
        self.due
    }

    /// Park the run: no direction, no schedule. On guide release, on a gate
    /// close, and wherever the scrub's other state is dropped.
    pub fn reset(&mut self) {
        self.dir = None;
        self.due = None;
    }

    /// Feed the current horizontal deflection and return the tap it earns, if
    /// any. At most one per call — the caller's clock (4 ms) is an order of
    /// magnitude faster than the top rate, so a second tap in one call would
    /// mean a rate over 250/s, which no arrow key is worth.
    pub fn update(&mut self, x: f64, now: Instant) -> Option<ShuttleTap> {
        let rate = self.rate(x);
        if rate <= 0.0 {
            self.reset();
            return None;
        }
        let right = x > 0.0;
        let step = Duration::from_secs_f64(1.0 / rate);
        let tap = ShuttleTap { right, word: x.abs().min(1.0) >= self.word_above };
        // A new run, or the stick crossing to the other side: tap now and
        // schedule from here. The pending tap of the old direction dies with it.
        if self.dir != Some(right) {
            self.dir = Some(right);
            self.due = Some(now + step);
            return Some(tap);
        }
        match self.due {
            Some(due) if now >= due => {
                // Re-base rather than catch up when we are more than one
                // interval late: a hitch must not become a burst of arrows.
                let next = due + step;
                self.due = Some(if next <= now { now + step } else { next });
                Some(tap)
            }
            Some(_) => None,
            // `dir` and `due` are set together, so this is unreachable; treat
            // it as the start of a run rather than panicking on a live daemon.
            None => {
                self.due = Some(now + step);
                Some(tap)
            }
        }
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

    #[test]
    fn angle_accumulator_reports_the_delta_it_accumulated() {
        // The rate estimator reads the same pass the count comes from, so the
        // delta has to be there even on frames that emit no tick — and gone on
        // the frames that contribute nothing.
        let mut acc = AngleAccumulator::new(15f64.to_radians(), 0.35);
        assert_eq!(acc.update_pt(on_circle(0.6, 0.0)), 0, "seeding frame");
        assert_eq!(acc.last_delta(), 0.0, "a seed moved nothing");
        assert_eq!(acc.update_pt(on_circle(0.6, 5.0)), 0, "below one detent");
        assert!(
            (acc.last_delta() - 5f64.to_radians()).abs() < 1e-9,
            "5° CCW: {}",
            acc.last_delta().to_degrees()
        );
        // Clockwise is negative, on the delta as on the count.
        acc.update_pt(on_circle(0.6, -5.0));
        assert!(acc.last_delta() < 0.0);
        // A sample inside the dead centre contributes nothing at all.
        acc.update(0.0, 0.0);
        assert_eq!(acc.last_delta(), 0.0);
        // And a reset clears it with everything else.
        acc.update_pt(on_circle(0.6, 0.0));
        acc.update_pt(on_circle(0.6, 4.0));
        acc.reset();
        assert_eq!(acc.last_delta(), 0.0);
    }

    /// Run the pacer's rate estimator to steady state at `deg_per_s`, at the
    /// puck's 250 Hz. 0.8 s is many time constants of a 5 Hz low-pass, so the
    /// tier tests can talk about speeds rather than about transients.
    fn settle(p: &mut JogPacer, deg_per_s: f64) {
        let dt = 0.004;
        for _ in 0..200 {
            p.observe((deg_per_s * dt).to_radians(), dt);
        }
    }

    fn chars(n: u32) -> JogStep {
        JogStep { unit: JogUnit::Char, count: n }
    }

    #[test]
    fn jog_pacer_starts_at_one_character_per_detent() {
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        // Nothing observed yet, and a slow spin: the landing tier.
        assert_eq!(p.detent(), chars(1));
        settle(&mut p, 120.0);
        for _ in 0..10 {
            assert_eq!(p.detent(), chars(1), "120°/s is below the 360°/s threshold");
        }
    }

    #[test]
    fn jog_pacer_arms_over_consecutive_detents_and_never_bursts() {
        // Above `fast`, but the climb waits for the arming count — a single
        // fast detent must not change the unit, and the detent that completes
        // the arming emits the NEW tier once, not a burst.
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        settle(&mut p, 500.0);
        assert_eq!(p.detent(), chars(1), "first fast detent still ×1");
        assert_eq!(p.detent(), chars(2), "second one completes the arming");
        assert_eq!(p.detent(), chars(2), "and stays there, one step per detent");

        // With a longer arming count the climb takes correspondingly longer.
        let mut slow_to_arm = JogPacer::new(360.0, 180.0, 4, true);
        settle(&mut slow_to_arm, 500.0);
        for i in 0..3 {
            assert_eq!(slow_to_arm.detent(), chars(1), "detent {i} of the arming run");
        }
        assert_eq!(slow_to_arm.detent(), chars(2));
    }

    #[test]
    fn jog_pacer_needs_the_run_to_be_consecutive() {
        // One fast detent, then a slow one, then a fast one: the run restarts,
        // so a wobble at the threshold never climbs.
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        settle(&mut p, 500.0);
        assert_eq!(p.detent(), chars(1));
        settle(&mut p, 100.0);
        assert_eq!(p.detent(), chars(1));
        settle(&mut p, 500.0);
        assert_eq!(p.detent(), chars(1), "the earlier fast detent does not count");
        assert_eq!(p.detent(), chars(2));
    }

    #[test]
    fn jog_pacer_holds_its_tier_across_the_hysteresis_band() {
        // The 2:1 gap: ×2 is entered above 360°/s and left below 180°/s, so a
        // thumb sitting anywhere between the two keeps the tier it has.
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        settle(&mut p, 500.0);
        p.detent();
        assert_eq!(p.detent(), chars(2));
        for speed in [340.0, 260.0, 200.0] {
            settle(&mut p, speed);
            assert_eq!(p.detent(), chars(2), "{speed}°/s is inside the band");
        }
        // Below the exit speed it drops back, and stays down until the ENTER
        // speed is met again for a whole arming run.
        settle(&mut p, 150.0);
        assert_eq!(p.detent(), chars(1));
        settle(&mut p, 300.0);
        assert_eq!(p.detent(), chars(1), "300°/s is under the 360°/s enter speed");
    }

    #[test]
    fn jog_pacer_word_tier_replaces_the_top_rung() {
        // With words on, the top rung is one ctrl+arrow at 1.5 × the ×2 pair
        // (540 in, 270 out) and wants one more detent of arming.
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        settle(&mut p, 600.0);
        assert_eq!(p.detent(), chars(1));
        assert_eq!(p.detent(), chars(2), "climbs to ×2 first");
        assert_eq!(p.detent(), chars(2), "arming the word tier, detent 1");
        assert_eq!(p.detent(), chars(2), "arming the word tier, detent 2");
        let word = JogStep { unit: JogUnit::Word, count: 1 };
        assert_eq!(p.detent(), word, "three detents above 540°/s = words");
        assert_eq!(p.detent(), word);
        // Between the word tier's 540 °/s enter and its 270 °/s exit the unit
        // holds: that gap is the whole point of the hysteresis.
        settle(&mut p, 400.0);
        assert_eq!(p.detent(), word, "still above the word tier's exit speed");
        // Slowing past the exit drops ONE rung — back to ×2, not to ×1, so the
        // wheel walks down instead of falling off.
        settle(&mut p, 250.0);
        assert_eq!(p.detent(), chars(2));
        settle(&mut p, 100.0);
        assert_eq!(p.detent(), chars(1), "and the landing is always ×1");
    }

    #[test]
    fn jog_pacer_caps_at_four_without_the_word_tier() {
        // Words off: the character ladder tops out at ×4, entered at twice the
        // ×2 speeds (720 in, 360 out), and nothing goes past it however fast
        // the thumb spins.
        let mut p = JogPacer::new(360.0, 180.0, 2, false);
        settle(&mut p, 3000.0);
        assert_eq!(p.detent(), chars(1));
        assert_eq!(p.detent(), chars(2));
        assert_eq!(p.detent(), chars(2), "arming the top rung");
        assert_eq!(p.detent(), chars(4));
        for _ in 0..20 {
            assert_eq!(p.detent(), chars(4), "×4 is the cap");
        }
        // 500°/s is below the ×4 enter speed but above its exit: it holds.
        settle(&mut p, 500.0);
        assert_eq!(p.detent(), chars(4));
        settle(&mut p, 300.0);
        assert_eq!(p.detent(), chars(2), "one rung at a time on the way down");
    }

    #[test]
    fn jog_pacer_reset_forgets_the_spin() {
        // Guide release / lift: the next spin starts at ×1 with no speed
        // inherited, so a fresh touch can never begin mid-ladder.
        let mut p = JogPacer::new(360.0, 180.0, 2, true);
        settle(&mut p, 900.0);
        p.detent();
        assert_eq!(p.detent(), chars(2));
        p.reset();
        assert_eq!(p.speed(), 0.0);
        assert_eq!(p.detent(), chars(1));
    }

    #[test]
    fn jog_pacer_survives_a_degenerate_config() {
        // A zero arming count must not make the ladder jump on one detent, and
        // an exit speed above the enter speed is not a hysteresis band: it is
        // clamped so the two can never cross.
        let mut p = JogPacer::new(360.0, 900.0, 0, true);
        settle(&mut p, 400.0);
        assert_eq!(p.detent(), chars(2), "arm 0 is floored to 1, not to 'always'");
        settle(&mut p, 370.0);
        assert_eq!(p.detent(), chars(2), "an exit clamped to the enter speed still holds");
        // A non-positive dt is ignored rather than divided by.
        let before = p.speed();
        p.observe(1.0, 0.0);
        p.observe(1.0, -0.004);
        assert_eq!(p.speed(), before);
    }

    // Small convenience so the arc tests read as a stream of points.
    impl AngleAccumulator {
        fn update_pt(&mut self, p: (f64, f64)) -> i32 {
            self.update(p.0, p.1)
        }
    }

    // --- the shuttle -------------------------------------------------------

    /// The defaults from `crate::config::ShuttleConfig`, spelled here so the
    /// pacer's tests stay pure — this module knows nothing about config.
    fn shuttle() -> ShuttlePacer {
        ShuttlePacer::new(0.15, 4.0, 25.0, 0.85)
    }

    /// Hold the stick at `x` for `ms` milliseconds of 4 ms steps and collect
    /// every tap it earns — the loop's own cadence.
    fn hold(p: &mut ShuttlePacer, x: f64, ms: u64, t0: Instant) -> Vec<ShuttleTap> {
        let mut taps = Vec::new();
        let mut t = 0;
        while t <= ms {
            if let Some(tap) = p.update(x, t0 + Duration::from_millis(t)) {
                taps.push(tap);
            }
            t += 4;
        }
        taps
    }

    #[test]
    fn a_stick_inside_the_deadzone_is_not_a_shuttle() {
        // The whole reason a caret shuttle wants a bigger deadzone than the
        // pointer does: a resting thumb that drifts a cursor is a nuisance, and
        // one that types arrow keys is a bug in the document.
        let t = Instant::now();
        let mut p = shuttle();
        assert_eq!(p.rate(0.0), 0.0);
        assert_eq!(p.rate(0.15), 0.0, "at the deadzone is still nothing");
        assert!(!p.engaged(0.15) && p.engaged(0.16));
        assert!(hold(&mut p, 0.14, 1000, t).is_empty(), "a whole second, no taps");
        assert!(!p.running(), "and nothing for the loop to wake up for");
        // Both directions, and the sign never matters to the deadzone.
        assert!(hold(&mut p, -0.1, 1000, t).is_empty());
    }

    #[test]
    fn the_shuttle_rate_rises_with_deflection() {
        let p = shuttle();
        // The three waypoints the curve is specified by: the slow end at the
        // deadzone edge, the fast end at the word threshold, and a straight
        // line between them.
        assert!((p.rate(0.1501) - 4.0).abs() < 0.01, "got {}", p.rate(0.1501));
        assert!((p.rate(0.85) - 25.0).abs() < 1e-9);
        assert!((p.rate(1.0) - 25.0).abs() < 1e-9, "past the top it is the top");
        let mid = 0.15 + (0.85 - 0.15) / 2.0;
        assert!((p.rate(mid) - 14.5).abs() < 1e-9, "linear between the two");
        // Monotone the whole way out, which is the only property a hand feels.
        let mut prev = 0.0;
        for i in 16..=100 {
            let r = p.rate(f64::from(i) / 100.0);
            assert!(r >= prev, "rate fell at {i}%");
            prev = r;
        }

        // And the taps actually come out at that rate: a second at full tilt is
        // 25 of them, a second just past the deadzone is 4.
        let t = Instant::now();
        let mut fast = shuttle();
        assert_eq!(hold(&mut fast, 1.0, 1000, t).len(), 26, "the first is immediate");
        let mut slow = shuttle();
        assert_eq!(hold(&mut slow, 0.16, 1000, t).len(), 5);
    }

    #[test]
    fn the_top_of_the_shuttle_is_the_word_tier() {
        let t = Instant::now();
        let mut p = shuttle();
        // Below the threshold every tap is a character...
        assert!(hold(&mut p, 0.84, 500, t).iter().all(|tap| !tap.word));
        // ...and at it, every tap is a word. The unit changes, not the rate:
        // this is the top gear, and a word jump lands on a boundary instead of
        // somewhere inside one.
        let mut w = shuttle();
        let taps = hold(&mut w, 0.85, 500, t);
        assert!(!taps.is_empty() && taps.iter().all(|tap| tap.word));
        // A config that never wants words says so by putting the threshold out
        // of reach.
        let mut never = ShuttlePacer::new(0.15, 4.0, 25.0, 2.0);
        assert!(hold(&mut never, 1.0, 500, t).iter().all(|tap| !tap.word));
        // And one that wants only words puts it at the deadzone.
        let mut always = ShuttlePacer::new(0.15, 4.0, 25.0, 0.0);
        let taps = hold(&mut always, 0.2, 500, t);
        assert!(!taps.is_empty() && taps.iter().all(|tap| tap.word));
        assert!(always.rate(0.2) > 0.0, "a zero-width band must not divide by zero");
    }

    #[test]
    fn reversing_the_shuttle_taps_the_other_way_at_once() {
        // A shuttle is a rate control: the direction is wherever the stick is
        // NOW. The pending tap of the old direction is abandoned, never
        // delivered late into the new one.
        let t = Instant::now();
        let mut p = shuttle();
        let right = p.update(0.5, t).expect("engaging taps immediately");
        assert!(right.right);
        // Well inside the interval: nothing more.
        assert_eq!(p.update(0.5, t + Duration::from_millis(4)), None);
        // Cross to the other side and the tap is instant, and Left.
        let left = p
            .update(-0.5, t + Duration::from_millis(8))
            .expect("a reversal is a fresh run");
        assert!(!left.right);
        // ...and the interval starts again from there.
        assert_eq!(p.update(-0.5, t + Duration::from_millis(12)), None);
    }

    #[test]
    fn a_released_shuttle_parks_and_a_late_step_does_not_burst() {
        let t = Instant::now();
        let mut p = shuttle();
        p.update(1.0, t);
        assert!(p.running() && p.deadline().is_some());
        // Centre it: parked, and the loop may block again.
        assert_eq!(p.update(0.0, t + Duration::from_millis(4)), None);
        assert!(!p.running() && p.deadline().is_none());
        // An explicit reset is the same thing, for the gate-close path.
        p.update(1.0, t + Duration::from_millis(8));
        p.reset();
        assert!(!p.running());

        // A step that arrives a full second late owes exactly one tap, not the
        // twenty-five a catch-up would fire into the document.
        let mut late = shuttle();
        late.update(1.0, t);
        assert!(late.update(1.0, t + Duration::from_secs(1)).is_some());
        assert_eq!(late.update(1.0, t + Duration::from_secs(1)), None, "re-based on now");
    }

    #[test]
    fn shuttle_survives_a_degenerate_config() {
        // A `fast` below `slow` is not a rising curve; it is clamped so the
        // band can never invert. A deadzone of 1.0 would make every push dead,
        // so it is clamped short of full scale.
        let p = ShuttlePacer::new(0.15, 25.0, 4.0, 0.85);
        assert!((p.rate(0.2) - 25.0).abs() < 1e-9);
        assert!((p.rate(1.0) - 25.0).abs() < 1e-9);
        let edge = ShuttlePacer::new(1.5, 4.0, 25.0, 0.85);
        assert!(edge.engaged(1.0), "a deadzone at full scale still leaves the edge");
        // A zero rate emits nothing rather than dividing by zero.
        let t = Instant::now();
        let mut dead = ShuttlePacer::new(0.15, 0.0, 0.0, 0.85);
        assert!(hold(&mut dead, 1.0, 1000, t).is_empty());
    }
}
