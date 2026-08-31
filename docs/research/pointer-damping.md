# Pointer / cursor damping & smoothing for a trackpad-driven cursor

Implementation-oriented research for `hyprpad`'s right-trackpad cursor path.

**Scope.** The 2026 Steam Controller right trackpad reports **absolute** capacitive
touch position (`i16`, roughly ±32767 per axis) at ~250 Hz. `drive_cursor`
(`src/run.rs`) converts that to **relative** `zwlr_virtual_pointer_v1` motion by
differencing consecutive frames: `delta = (cur - prev) * PAD_CURSOR_SENS`
(`PAD_CURSOR_SENS = 0.06`), resetting `prev` to `None` on lift. **Live symptom:
a finger held perfectly still makes the cursor crawl/jitter.** This doc explains
why, surveys the standard fixes with real math and concrete parameters, and ends
with a prioritized recommendation.

Every external claim is tagged **VERIFIED** (backed by a cited source) or
**INFERRED** (my analysis/derivation, not from a cited source). All suggested
numeric parameters are **tuning starting points**, not tuned values — the pad's
real noise floor must be measured on-device.

---

## 1. Why a held-still finger jitters

### 1.1 The physical noise source
Capacitive trackpads locate the finger by interpolating the centroid of
capacitance across a grid of electrodes. That centroid estimate carries
**sub-count electrical noise** every frame: thermal/quantization noise in the
ADC, mutual-capacitance crosstalk, mains/PSU pickup, and centroid-interpolation
error that wobbles as skin contact area breathes. Even a motionless finger
produces a position estimate that dithers by a few counts frame-to-frame. This
is the same "jitter" libinput documents for laptop touchpads
([libinput touchpad-jitter](https://wayland.freedesktop.org/libinput/doc/latest/touchpad-jitter.html), VERIFIED — libinput calls it out as a known property of "some touchpads" that "may send events even for a finger that is not moving").

### 1.2 Why absolute→relative *differencing* amplifies it
Differencing is a discrete derivative, and **differentiation is a high-pass
filter**: it multiplies each frequency component by ~its frequency, so it boosts
exactly the high-frequency sensor noise while passing the low-frequency real
motion unchanged (INFERRED, standard DSP).

Concretely, if the true position is constant `x` but the reading is
`x + n_t` with per-frame noise `n_t`, then the emitted delta is

```
delta_t = ((x + n_t) - (x + n_{t-1})) * SENS = (n_t - n_{t-1}) * SENS
```

The real signal (`x`) cancels; **only the noise survives.** If `n_t` has
standard deviation `σ` counts, the difference `n_t − n_{t-1}` has standard
deviation `σ·√2` (independent samples). At 250 Hz that noise-only delta is
injected as pointer motion **250 times a second**, so even σ ≈ 1–2 counts
becomes a visible, continuous cursor tremor. With `SENS = 0.06` px/count, σ = 2
counts gives per-frame jitter of ~`0.06·2·√2 ≈ 0.17 px` — small alone, but
relentless and directionless, which reads as a "swimming" cursor (INFERRED,
derivation).

Three properties of this pipeline make it worse than a normal touchpad:
- **No hardware hysteresis.** hyprpad reads raw HID reports; unlike libinput on
  a kernel evdev touchpad, nothing has already applied a fuzz/hysteresis margin
  (§2.6). The daemon sees the full noise.
- **High report rate.** 250 Hz means the noise is re-sampled and re-injected
  fast; the eye integrates it into continuous motion.
- **The "held-still" case is the worst case**, because it is *pure* noise with
  no real motion to mask it — exactly when the user expects the cursor to be
  rock-steady (e.g. hovering a small target before clicking).

### 1.3 Design consequence
The fix must **suppress high-frequency, low-amplitude motion while preserving
low-frequency, intentional motion** — and it must do so *before or at* the
differencing step, because once noise is differenced into relative deltas the
real signal is already gone and can't be separated out. The rest of this doc is
about how to shape that filter. The single most important structural decision:
**filter the absolute pad position, then difference the filtered position** —
not the other way around.

---

## 2. Techniques

Each technique below gives the math, typical parameters, and a fit verdict for a
250 Hz pad→relative pipeline. Throughout, `Te = 1/250 s = 0.004 s` is the sample
period.

### 2.1 Dead zone / movement threshold
**Idea.** Only emit motion once the finger has moved more than a threshold `T`
from the tracking origin (or ignore per-frame deltas smaller than `T`).

**Math (radial, from a latched origin `p0`):**
```
d = ||cur - p0||
if d > T:  emit motion for (cur - p0); advance p0
else:      emit nothing
```

**Params.** `T` in pad counts. A starting point is `T ≈ 1.5–3 × σ_noise` so the
noise almost never crosses it; measure σ first (INFERRED starting point).

**Downsides (important, VERIFIED as general UI-filtering knowledge / INFERRED specifics):**
- **Stair-stepping / stiction.** Slow, deliberate motion below `T` is dropped
  entirely, then released in a lump when the threshold trips — the cursor
  "sticks then jumps." This destroys fine positioning, the exact task where you
  most want steadiness.
- **Lost fine control.** Pixel-precise dragging becomes impossible: sub-`T`
  motions never come out.
- **Origin choice matters.** A *per-frame* delta threshold (drop each
  `|delta|<T`) is even worse — it silently discards all slow drags because each
  individual frame's real motion is tiny at 250 Hz.

**Verdict.** Necessary as a *complement*, not a solution. A small dead zone /
hysteresis is the right tool for the "held perfectly still" endpoint, but on its
own it trades jitter for stair-stepping. It must be paired with sub-pixel
accumulation (§2.7) and kept small. libinput's *hysteresis* (§2.6) is the
well-engineered version of this idea (a moving center, not a fixed origin).

### 2.2 Low-pass / exponential smoothing (EMA)
**Idea.** Blend each new sample with the running estimate. The exponential
moving average (a.k.a. one-pole IIR low-pass):
```
x̂_t = α · x_t + (1 − α) · x̂_{t-1},   α ∈ (0, 1]
```
`α → 1` = no smoothing (follows input); `α → 0` = heavy smoothing (sluggish).

**Relation to a cutoff frequency** (VERIFIED, [Casiez et al. / gery.casiez.net/1euro](https://gery.casiez.net/1euro/)):
```
α = 1 / (1 + τ/Te),   τ = 1 / (2π·fc)
```
equivalently `α = r/(r+1)` with `r = 2π·fc·Te` (VERIFIED, [jaantollander](https://jaantollander.com/post/noise-filtering-using-one-euro-filter/)).

**α table at 250 Hz** (INFERRED, computed from the formula above):

| cutoff `fc` (Hz) | `α`    | behavior |
|---|---|---|
| 0.5 | 0.0124 | very heavy smoothing / laggy |
| 1.0 | 0.0245 | heavy smoothing, ~visible lag |
| 2.0 | 0.0479 | moderate |
| 5.0 | 0.112  | light |
| 10  | 0.201  | very light |
| 15  | 0.274  | barely smoothing |

**On position vs on delta.**
- **On position** (filter `x`, then difference): the low-pass attenuates the
  sensor noise *before* the high-pass differencing re-amplifies it. This is the
  correct place. Held still, `x̂` converges to a steady value and the difference
  → ~0.
- **On delta** (difference first, then low-pass the delta stream): mathematically
  you still emit smoothed noise, and you've added lag to real motion; a constant
  slow drag becomes a low-pass of noisy deltas. Filtering position dominates.

**The fundamental EMA tradeoff.** A *fixed* `α` cannot win: pick `α` small enough
to kill held-still jitter (e.g. fc = 1 Hz, α ≈ 0.025) and fast flicks lag
noticeably (the cursor "drags through mud" — the same complaint Steam users
raise about too much smoothing, VERIFIED [Steamworks input source modes](https://partner.steamgames.com/doc/features/steam_controller/input_source_modes): "too much smoothing can result in increased input latency"). Pick `α` large
enough to feel responsive and the jitter returns. **This inability to be both
smooth-when-still and responsive-when-fast is exactly what the One Euro Filter
solves** by making the cutoff adaptive.

**Verdict.** A fixed EMA on position is a real improvement over today's raw
differencing and is trivial to add — but it is strictly dominated by the One
Euro Filter, which *is* an EMA whose `α` adapts to speed.

### 2.3 One Euro Filter — the primary recommendation
Casiez, Roussel & Vogel, "1€ Filter: A Simple Speed-based Low-pass Filter for
Noisy Input in Interactive Systems," **CHI 2012, pp. 2527–2530, DOI
[10.1145/2207676.2208639](https://dl.acm.org/doi/10.1145/2207676.2208639)**;
project page & reference code **[gery.casiez.net/1euro](https://gery.casiez.net/1euro/)**
(formerly gestures.inria.fr) (VERIFIED). It is the de-facto standard for
smoothing noisy interactive pointers (used in VR/AR trackers, gaze, MediaPipe
hand tracking, etc.).

**Core idea (VERIFIED, gery.casiez.net/1euro).** It is a low-pass filter (§2.2)
whose **cutoff frequency adapts to the signal's speed**:
- when the finger is **slow/still**, use a **low** cutoff → heavy smoothing →
  jitter is crushed;
- when the finger is **fast**, use a **high** cutoff → little smoothing → low
  lag, so fast motion stays responsive.

This directly targets the §1.2 problem: kill noise at rest, get out of the way
during a flick.

**Full algorithm.** Two coupled low-passes: one on the signal, one on its
derivative (the derivative drives the adaptive cutoff). Reference form
(VERIFIED, gery.casiez.net/1euro + jaantollander):

```
# smoothing factor for a given cutoff (α from §2.2)
smoothing_factor(Te, cutoff):
    r = 2 * pi * cutoff * Te
    return r / (r + 1)                      # == 1 / (1 + 1/r) == 1/(1 + τ/Te)

exp_smooth(a, x, x_prev):
    return a * x + (1 - a) * x_prev

# state: x_prev, dx_prev (both per-axis), and t_prev for Te
filter(x, Te):
    # 1. derivative of the signal, in signal-units per second
    dx = (x - x_prev) / Te
    # 2. low-pass the derivative with a FIXED cutoff d_cutoff
    a_d      = smoothing_factor(Te, d_cutoff)
    dx_hat   = exp_smooth(a_d, dx, dx_prev)
    # 3. adaptive cutoff rises with |speed|
    cutoff   = min_cutoff + beta * abs(dx_hat)
    # 4. low-pass the signal with that adaptive cutoff
    a        = smoothing_factor(Te, cutoff)
    x_hat    = exp_smooth(a, x, x_prev)
    # 5. save state
    x_prev, dx_prev = x_hat, dx_hat
    return x_hat
```

Run one instance **per axis** (x and y independently). `Te` should be the
*measured* interval between frames (`now - last`), not a hardcoded 1/250, so that
dropped frames don't distort the cutoff (INFERRED — the reference implementation
takes a timestamp per sample precisely so `Te` is real).

**Parameters and their meaning (VERIFIED, gery.casiez.net/1euro):**

| param | role | reference default |
|---|---|---|
| `min_cutoff` (`fcmin`) | cutoff floor at zero speed → sets **held-still smoothing** | **1.0 Hz** |
| `beta` | speed coefficient → how fast the cutoff opens up with motion → sets **lag during fast motion** | **0.0** |
| `d_cutoff` | fixed cutoff for the derivative low-pass | **1.0 Hz** |
| `fcmin` constraint | must be `> 0` | — |

**Author's tuning procedure (VERIFIED, gery.casiez.net/1euro), quoted:**
> "First beta is set to 0 and fcmin … to a reasonable middle-ground value such
> as 1 Hz. Then the body part is held steady … while fcmin is adjusted to remove
> jitter … Next, the body part is moved quickly … while beta is increased."

and the rule of thumb:
> "if high speed lag is a problem, increase beta; if slow speed jitter is a
> problem, decrease fcmin."

**Why speed-adaptive beats a fixed low-pass.** A fixed low-pass sits at one point
on the smoothness↔lag curve forever (§2.2). One Euro *moves along that curve
every frame* based on `|dx_hat|`: near-zero speed → `cutoff ≈ min_cutoff` (≈1 Hz,
α ≈ 0.025 at 250 Hz — hard smoothing, jitter gone); high speed → `cutoff =
min_cutoff + beta·speed` climbs to tens of Hz (α → 0.2–0.6, effectively
pass-through — no lag). You get the small-`α` benefit exactly when you need it
and never pay its lag when you don't (VERIFIED conceptually, gery.casiez.net/1euro).

**Starting values for a ~250 Hz pad (TUNING STARTING POINTS, INFERRED):**
- **Normalize the pad coordinate to a unit range first** (e.g. divide the ±32767
  reading by 32767 → roughly [−1, 1]), *then* filter. This matters:
  `beta` has the units of `1/velocity`, and velocity is in *signal units per
  second*. On raw counts a full-pad flick is ~10⁴–10⁵ counts/s, so a literature
  `beta` would need to be ~10⁻⁵ to mean anything; on a normalized [−1,1] signal a
  fast flick is only a few units/s and **published `beta` values transfer
  directly.** (INFERRED, dimensional analysis.)
- On the normalized signal: `min_cutoff ≈ 1.0 Hz`, `beta ≈ 0.5–2.0`,
  `d_cutoff ≈ 1.0 Hz`. Start at the paper's `min_cutoff = 1.0, d_cutoff = 1.0`
  and raise `beta` from ~0 until fast flicks stop lagging; if the held-still
  cursor still swims, drop `min_cutoff` toward ~0.5 Hz.
- If you filter **raw counts** instead, keep `min_cutoff ≈ 1.0`, `d_cutoff ≈ 1.0`
  but expect `beta` on the order of **1e-5 – 1e-4** (INFERRED, dimensional).

**Verdict.** **The primary recommendation.** It is small (a dozen lines,
per-axis state, no allocation), it is the published standard for exactly this
problem class (noisy interactive pointer), and its speed-adaptive cutoff is the
only technique here that resolves the smooth-vs-responsive tradeoff instead of
just picking a point on it. Placed on the *absolute pad position before
differencing*, it attacks the noise before the high-pass amplifies it (§1.2).

### 2.4 Median / moving-average windows
**Moving average (boxcar, window N):** `x̂_t = (1/N)·Σ_{k=0..N-1} x_{t-k}`. A
low-pass with a `sinc` frequency response and **linear phase lag of ≈ (N−1)/2
samples**. At 250 Hz, N = 5 costs 8 ms of lag for modest smoothing; it also rings
on transients. Strictly worse than an EMA/One Euro for the same lag budget
(INFERRED, standard DSP).

**Median filter (window N):** replace each sample with the median of the last N.
Excellent at removing **impulsive spikes/outliers** (a single stray frame),
because the median ignores them, and it is edge-preserving (doesn't smear a sharp
start-of-motion). But it does **not** remove Gaussian dither well and adds
`(N−1)/2` samples of latency, and it quantizes to actual sample values so it can
itself stair-step. A short median (N = 3) is a cheap, useful *pre-stage* to kill
one-frame glitches / the touchdown outlier (§4), feeding One Euro (INFERRED).

**Verdict.** Moving average: skip (dominated). Median(3): optional pre-filter for
spike rejection and touchdown de-glitching, not a jitter solution on its own.

### 2.5 Kalman filter — overkill here?
A Kalman filter is the optimal linear estimator for a linear system with known
Gaussian process/measurement noise. A constant-velocity model on pad position
would smooth well and could predict to offset latency. **But:** it requires a
motion model and tuned `Q`/`R` covariances; with a constant-velocity model and
steady tuning it behaves like a fixed low-pass and suffers the **same
smooth-vs-lag tradeoff as §2.2** unless you make `Q`/`R` adaptive — at which
point you've reinvented a fussier One Euro. The One Euro paper's own framing is
that it was designed as a *simpler* alternative that matches or beats Kalman for
interactive input with far less tuning (VERIFIED framing, gery.casiez.net/1euro —
the filter is presented explicitly as a simple speed-based low-pass for noisy
interactive input).

**Verdict.** Overkill. More state, more tuning, more latency risk, no advantage
over One Euro for a 1-D-per-axis pointer. Skip unless you later want *predictive*
latency compensation, which is a different feature.

### 2.6 libinput touchpad hysteresis — the "finger held still" fix
libinput ships an explicit hysteresis mechanism for *precisely* the held-still
jitter case (VERIFIED,
[libinput touchpad-jitter](https://wayland.freedesktop.org/libinput/doc/latest/touchpad-jitter.html)):
> "When active, movement within the **hysteresis margin** is discarded. If the
> movement delta is larger than the margin, the movement is passed on as pointer
> movement."

**Exact mechanism (VERIFIED — reference implementation, `src/evdev.h`, quoted
verbatim from the libinput source):**
```c
static inline int
evdev_hysteresis(int in, int center, int margin)
{
    int diff = in - center;
    if (abs(diff) <= margin)
        return center;
    if (diff > 0)
        return in - margin;
    else
        return in + margin;
}
```
So per axis: keep a **center**. If the new reading is within `±margin` of the
center, **output the center unchanged** (→ zero delta → no cursor motion). Once
the reading leaves the margin, output `in ∓ margin` — i.e. the finger "drags" the
center along, sticking to the leading edge of the margin. The center becomes that
returned value for the next frame. This is a **moving-center dead zone**: it
freezes a still finger completely, yet once you're moving it only costs a
constant `margin` offset (no per-frame stair-stepping while in motion), which is
its advantage over the fixed-origin threshold in §2.1 (VERIFIED code + INFERRED
interpretation).

**How the margin is derived (VERIFIED, libinput touchpad-jitter):** libinput uses
the kernel **`fuzz`** value of the absolute axis as the margin. Because a kernel
fuzz makes the *kernel* apply its own inconsistent hysteresis, libinput:
> "sets the kernel fuzz on the device to 0 to disable this kernel behavior but
> remembers what the fuzz was on startup. The fuzz is stored in the
> `LIBINPUT_FUZZ_XX` udev property."

If a device reports no fuzz, hysteresis is essentially **off by default** (later
libinput only logs wobble rather than enabling hysteresis unprompted); users set
fuzz via a udev hwdb entry, aided by the `libinput measure fuzz` tool (VERIFIED,
libinput touchpad-jitter + the "drop motion hysteresis by default" / "don't turn
on the hysteresis on wobbly touchpads, just log it" patches on wayland-devel).

**For hyprpad (INFERRED):** there is no kernel fuzz on the raw HID path, so pick
a `margin` directly, in pad counts, sized to the measured noise (start ~`2–4 ×
σ_noise`, e.g. a handful of counts out of ±32767). Apply it per axis with the
exact code above, on the absolute position, as the **last cheap stage before
differencing** — it guarantees a genuinely motionless finger emits *exactly zero*
delta, which a low-pass alone (which only *attenuates*) cannot promise.

**Verdict.** The precise, purpose-built answer to "held still still jitters."
Complements One Euro rather than replacing it: One Euro shapes the dynamic
response (smooth-when-slow, snappy-when-fast); the hysteresis margin guarantees a
hard zero at true rest. Recommended together.

### 2.7 Sub-pixel / fractional delta accumulation
**Problem.** The virtual-pointer `motion` request takes `f64` deltas, but any
downstream integer quantization — or a dead-zone/hysteresis that suppresses small
motion — will **drop slow movement** if you discard the fractional remainder each
frame. At 250 Hz a slow, deliberate drag is a fraction of a pixel per frame; if
each frame rounds/truncates to 0, the cursor never moves no matter how long you
push.

**Fix (VERIFIED as standard technique / INFERRED specifics).** Accumulate the
residual across frames and only emit the integer part, carrying the remainder:
```
acc_x += dx_filtered            # dx_filtered = (x̂_t - x̂_{t-1}) * SENS
out_x  = trunc(acc_x)           # integer pixels to emit this frame
acc_x -= out_x                  # keep the fraction for next frame
if out_x != 0 or out_y != 0: move_relative(out_x, out_y)
```
This way ten frames of +0.1 px reliably become +1 px, so slow motion survives
quantization and thresholds.

**Why combine it with a threshold/hysteresis.** A dead zone (§2.1/§2.6) and
sub-pixel accumulation are complements: the threshold removes *noise-scale*
motion at rest, and the accumulator makes sure that *real but slow* motion which
survives the threshold is never lost to rounding. Without the accumulator, a
threshold's "keep only motion > T" plus integer output silently eats slow drags;
without the threshold, the accumulator faithfully accumulates *noise* too. Use
both. Note the current code passes `f64` straight to `move_relative`, so today the
compositor does the rounding invisibly — accumulate on hyprpad's side so you
control it (INFERRED).

**Verdict.** Cheap, mandatory companion to any thresholding/quantization. Two
`f64` accumulators.

---

## 3. What Steam Input / SteamOS document for trackpad-as-mouse

Steam Input exposes several trackpad "mouse" behaviors and smoothing controls
(VERIFIED,
[Steamworks: Input Source Modes](https://partner.steamgames.com/doc/features/steam_controller/input_source_modes),
[Steamworks: Mouse Regions](https://partner.steamgames.com/doc/features/steam_controller/mouse_regions)):

- **Mouse mode (relative)** — pad drives a relative cursor (the analog of
  hyprpad's path). Documented per-control settings:
  - **Smoothing** — "Smoothing helps to remove noise and jitter from the mouse.
    Smaller values will result in less filtering, while higher values will be
    more filtered and smoother," slider **0.0–1.0** (VERIFIED). This is Valve's
    name for exactly the §1 problem; the doc also warns "too much smoothing can
    result in increased input latency … like you are trying to pull [the camera]
    through mud" (VERIFIED). The *internal filter* Valve uses is **not
    documented** (INFERRED: undocumented; commonly assumed to be a low-pass, but
    treat the algorithm as unknown).
  - **Sensitivity** — 0.0–1.0 (VERIFIED).
  - **Acceleration** — On/Off: "faster movements … cause more mouse movement
    relative to slow movements for the same space covered on the pad" (VERIFIED)
    — i.e. velocity-dependent gain (§2.8), which also attenuates slow jitter.
  - **Trackball mode / Friction** — pad gains momentum and coasts; "Friction"
    sets how fast it decays; a low friction lets a flick keep gliding (VERIFIED).
    This is a *feel* feature, not a noise filter, though momentum inherently
    low-passes flicks.
- **Mouse Region (absolute)** — "treats the pad as a 1:1 map to screen space, so
  touching a particular place on the pad will always put the cursor in the same
  place on the screen"; regions can be scaled/stretched/mode-shifted. No
  smoothing control is documented for this mode (VERIFIED /
  INFERRED-absence-of-doc). This is the absolute analog and is *not* what hyprpad
  does (hyprpad is relative), but it is the mode that never suffers
  differencing-amplified jitter because it doesn't difference.

**Takeaways for hyprpad (INFERRED):** (1) Valve treats a user-facing **Smoothing
0–1 slider** as the primary knob for trackpad noise, validating exposing one
scalar "smoothing" control mapped onto filter aggressiveness. (2) They pair it
with an **Acceleration** toggle (velocity gain), reinforcing §2.8. (3) The exact
filter is undocumented — do not cite Steam for a specific algorithm or numbers
beyond the 0–1 slider semantics.

---

## 4. Recommended pipeline for hyprpad

Filter the **absolute pad position first, difference second** — this is the whole
game (§1.2). Concretely, per touched frame, per axis:

```
raw i16 (x, y)                          # from frame.right_pad
  │
  ├─(1) touchdown gate / settle         # ignore first few frames after touch-down (clutch), §5
  │
  ├─(2) normalize: n = coord / 32767    # → ~[-1,1] so literature beta transfers, §2.3
  │
  ├─(3) median(3)                       # OPTIONAL: kill single-frame outliers/glitches, §2.4
  │
  ├─(4) One Euro Filter (per axis)      # PRIMARY smoothing, min_cutoff/beta/d_cutoff, §2.3
  │        → x̂, ŷ  (still normalized)
  │
  ├─(5) hysteresis(margin) (per axis)   # moving-center dead zone → hard zero at rest, §2.6
  │        operate in counts: multiply back by 32767, or keep a normalized margin
  │
  ├─(6) difference: d = (cur_filt - prev_filt)     # prev_filt = last frame's post-hysteresis value
  │
  ├─(7) scale: d *= PAD_CURSOR_SENS                 # existing SENS, unchanged meaning
  │
  ├─(8) sub-pixel accumulate: acc += d; out = trunc(acc); acc -= out    # §2.7
  │
  └─(9) if out != 0: ptr.move_relative(out_x, out_y)
```

Stages (3) and (5) are optional-but-recommended; (4) is the core. A minimal first
cut is **(2)+(4)+(6)+(7)+(8)** — One Euro on normalized position, difference,
scale, sub-pixel accumulate — then add hysteresis (5) if a dead-stop finger still
creeps.

### 4.1 Starting parameters (TUNING STARTING POINTS — measure `σ_noise` on-device)
On the **normalized** signal (INFERRED):
- `min_cutoff = 1.0 Hz`, `beta = 1.0`, `d_cutoff = 1.0 Hz` (paper defaults for
  the two cutoffs; beta raised from 0 to give a real speed response). Follow the
  author's two-step tune: hold still, lower `min_cutoff` until the swim stops;
  flick, raise `beta` until lag is acceptable.
- `hysteresis_margin ≈ 2–4 × σ_noise` counts (in raw-count space). If σ ≈ 2
  counts, margin ≈ 4–8 counts out of ±32767.
- `median` window = 3 (or disabled).
- `PAD_CURSOR_SENS` stays 0.06 and keeps its current meaning (px per count) — the
  filter changes *what* is differenced, not the gain.
- Use **measured `Te`** (`now − last_frame`) in the filter, not a fixed 1/250.

### 4.2 What to expose as config (this project wants heavy configurability)
A `[cursor]` config block (INFERRED design):
- `smoothing` — a single 0.0–1.0 "smoothing" scalar à la Steam (§3) mapped onto
  `min_cutoff` (e.g. `min_cutoff = lerp(hi_hz … lo_hz)` so higher slider = lower
  cutoff = more smoothing), for users who don't want to think in Hz.
- Advanced/expert keys for direct control: `one_euro_min_cutoff`,
  `one_euro_beta`, `one_euro_d_cutoff`, `hysteresis_margin`, `median_window`,
  `sens` (existing), plus `enable_filter` / `enable_hysteresis` toggles and a
  `touch_settle_ms` / `touch_settle_frames` (§5).
- Keep every filter stage independently disable-able so the raw path is
  recoverable for A/B feel testing.

### 4.3 Interaction with `SENS` and the touch-down/lift `prev` reset
- **`SENS` is orthogonal.** It multiplies the *filtered* difference; filtering
  reshapes the signal, gain scales it. No re-tuning coupling beyond taste.
- **Reset filter state on lift, exactly where `prev` is reset today.** In the
  `else`/`!touched` branches of `drive_cursor` that set `st.prev = None`, also
  **reset the One Euro state (mark x_prev/dx_prev uninitialized) and the
  hysteresis center and the sub-pixel accumulators.** Otherwise a lift-and-retouch
  elsewhere on the pad carries stale filter state and derivative, which would
  either produce a jump or a wrong initial cutoff (INFERRED — this is the direct
  analog of why `prev = None` already exists).
- **First-frame-after-touch must not jump (clutch).** Today the first touched
  frame sets `prev` and emits nothing — preserve that. With filtering, the first
  frame must *initialize* the filter to the current reading (x_hat = x, dx = 0)
  and emit nothing, so there is no startup transient (INFERRED).

---

## 5. Absolute-trackpad-as-relative-pointer specifics

- **Reset-on-lift clutch (keep it).** hyprpad already treats the pad as a
  clutchable relative device: lift resets `prev` so a retouch never jumps. This
  is the correct model for absolute→relative and *all filter state resets with
  it* (§4.3). It is why hyprpad, unlike Steam's absolute "Mouse Region" (§3),
  never maps position→screen.
- **First-touch settle (new).** Capacitive pads report a **noisy or off-centroid
  position for the first 1–3 frames** of contact as the finger lands and contact
  area stabilizes; differencing that landing transient produces a visible flick
  at touch-down. Mitigation (INFERRED, tuning starting points): after a
  fresh touch-down, **ignore/emit-nothing for the first ~2–3 frames (~8–12 ms)**
  or until consecutive readings agree within a small tolerance, using those
  frames only to seed `prev`/filter state. Expose as `touch_settle_frames` /
  `touch_settle_ms`. A `median(3)` pre-stage (§2.4) also absorbs a single-frame
  landing outlier.
- **Edge behavior.** Near the pad edge the capacitive centroid gets pulled inward
  and noisier as part of the finger leaves the sensor, and the reported value
  saturates toward ±32767. Because hyprpad is *relative*, saturation at an edge
  simply produces zero delta (clamped) rather than a jump — acceptable — but the
  pre-saturation edge noise can inject spurious motion. Options (INFERRED): rely
  on the One Euro + hysteresis to swallow it, and/or optionally ignore frames
  within a thin edge band (`edge_margin` counts) so a finger sliding off the pad
  doesn't emit a noisy final delta. Keep this optional; the clutch reset on the
  inevitable lift already limits the damage.
- **Lift detection latency.** `prev` resets on `!PadRightTouch`. If the firmware
  drops touch a frame late, the last pre-lift frame can carry an edge/lift-off
  jerk; the settle logic on the *next* touch plus hysteresis limits its visible
  effect (INFERRED).

---

## 6. Prioritized recommendation

1. **Highest impact: per-axis One Euro Filter on the normalized absolute pad
   position, before differencing** (§2.3, §4). This is the single change that
   turns "held-still swim" into a steady cursor while keeping flicks responsive,
   because its speed-adaptive cutoff resolves the smooth-vs-lag tradeoff that a
   fixed low-pass cannot. Start `min_cutoff = 1.0 Hz`, `beta = 1.0`,
   `d_cutoff = 1.0 Hz` on a [−1,1]-normalized signal, using measured `Te`; tune
   per Casiez's two-step method. (VERIFIED filter; INFERRED starting values.)
2. **Add a moving-center hysteresis margin** (§2.6, libinput's exact
   `evdev_hysteresis`) as the last stage before differencing, to guarantee a
   *hard zero* delta for a truly motionless finger — the one thing a low-pass
   can't promise. Margin ≈ 2–4×σ_noise counts.
3. **Sub-pixel delta accumulation** (§2.7) so hysteresis/quantization never eats
   slow, deliberate motion. Cheap, mandatory companion to #2.
4. **Touch-down settle + full filter-state reset on lift** (§4.3, §5) so the
   clutch stays jump-free and the landing transient doesn't flick the cursor.
5. **Expose it all as config** (§4.2): one Steam-style 0–1 `smoothing` scalar for
   casual users plus expert `min_cutoff`/`beta`/`d_cutoff`/`margin`/settle keys,
   each stage toggleable.
6. **Optional extras:** `median(3)` spike pre-filter (§2.4) if single-frame
   glitches appear; an `edge_margin` band (§5) if edge noise is visible. **Skip**
   Kalman (§2.5, overkill) and plain moving-average (§2.4, dominated). A **fixed
   EMA** (§2.2) is a valid *smaller* fallback if One Euro is deferred, but it is
   strictly dominated by #1.

---

## References

VERIFIED (cited):
- One Euro Filter — project page & reference code: <https://gery.casiez.net/1euro/>
- One Euro Filter — Casiez, Roussel, Vogel, *CHI 2012*, pp. 2527–2530, DOI 10.1145/2207676.2208639: <https://dl.acm.org/doi/10.1145/2207676.2208639>
- One Euro Filter — worked math & Python reference (defaults min_cutoff=1.0, beta=0.0, d_cutoff=1.0): <https://jaantollander.com/post/noise-filtering-using-one-euro-filter/>
- libinput touchpad jitter / hysteresis / fuzz: <https://wayland.freedesktop.org/libinput/doc/latest/touchpad-jitter.html>
- libinput `evdev_hysteresis` reference implementation (`src/evdev.h`): <https://github.com/jiixyj/libinput/blob/master/src/evdev.h>
- libinput pointer acceleration (velocity gain, adaptive/flat/custom, 0.3–3.5 factor range, touchpad "squashed" curve): <https://wayland.freedesktop.org/libinput/doc/latest/pointer-acceleration.html>
- Steam Input source modes (Mouse smoothing 0–1, sensitivity, acceleration, trackball friction): <https://partner.steamgames.com/doc/features/steam_controller/input_source_modes>
- Steam Input Mouse Regions (absolute 1:1 pad→screen): <https://partner.steamgames.com/doc/features/steam_controller/mouse_regions>

INFERRED items are the derivations (§1.2 noise math, §2.2 α table), all suggested
numeric parameters (labeled tuning starting points), the hyprpad-specific
pipeline (§4), and the absolute→relative clutch/settle/edge analysis (§5). They
are my analysis, not drawn from a cited source, and must be validated against the
pad's measured noise on-device.
