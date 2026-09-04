# Research: text scrubbing — moving the caret from the trackpad

*Produced 2026-09-01. Goal: give the owner a fast, precise way to move the text
caret from the puck — "spin left / spin right, slow down where I need precision
and speed up when moving quickly" — for fixing dictated text
(`voxtype record toggle`, `guide+a`) and on-screen-keyboard typos, without
giving up the left pad's scroll or the D-pad's line movement.*

**Verification basis.** **VERIFIED(code)** = read from this repository at commit
`e2ddad0` (line numbers below are against that tree). **VERIFIED(web)** = a
cited page was fetched and the quoted text read. **VERIFIED(secondary)** = only a
summary or a third-party write-up of the source was reachable; the claim is
plausible but the primary text was not seen. **INFERRED** = reasoned from those,
not confirmed on this device. Every number offered as a tuning value is a
*starting point*, in the same spirit as `docs/research/pointer-damping.md`.

---

## 0. TL;DR

- The thing the owner is describing already has a name and a house
  implementation: a **jog wheel**. Jog = *position* (each detent is one unit,
  the count is exact, the rate follows the hand); shuttle = *velocity* (deflect
  to set a speed, release to stop). The puck's circular scroll is a jog wheel
  for scroll units (`AngleAccumulator`, `src/filter.rs:318`); the caret scrub
  is the same wheel with a different emitter — one `KEY_LEFT`/`KEY_RIGHT` *tap*
  per detent — and the D-pad stays the shuttle (held → the client's own key
  repeat).
- **Steam Input's "Scroll Wheel" mode is exactly this idea** — clockwise and
  counter-clockwise bindings that fire "every 'click' of the 'wheel'", a
  sensitivity that sets "how much of a rotation is required for the next
  binding to fire", and an Off/Low/Medium/High haptic tick per click. Valve
  ships no caret-specific mode; on the Deck the arrow keys are keycaps on the
  OSK's bottom row and the D-pad moves key *focus*, not the caret. Phones solve
  the same problem with a *linear* drag on the space bar (iOS trackpad mode,
  Gboard, BlackBerry), relative and bounded by the key's width.
- **Recommendation: option (a), guide-scoped.** Hold guide, circle the LEFT pad
  → one caret step per 15° detent with a haptic tick; speed tiers multiply the
  step (×1 / ×2, later a *word* tier via `ctrl+left/right` with a heavier
  tick); guide+D-pad ↑/↓ = lines (held, so it repeats); hold a left grip while
  circling = Shift held (select as you go). It needs the least new machinery:
  the detent engine, the smoothing, the haptic vocabulary, the guide gate, the
  uinput keyboard and the guard model all exist. Phase 1 is one new per-frame
  handler, one config section, one cheat-sheet row.
- Two capability gaps stand between phase 1 and the full design: (1)
  `Action::Key` is a single evdev code, so `ctrl+left` cannot be spelled —
  a `KeyChord { mods, code }` output and a `tap_with(mods, code)` on
  `VirtualKeyboard` is the smallest addition; (2) a guide-scoped pad rotation
  must mark the hold as *consumed* or the release reads as a bare guide tap.
- **Tap, never hold.** Key repeat on Wayland is performed by the *client* at
  the compositor-advertised rate (`wl_keyboard.repeat_info`, "characters per
  second"), after a fixed delay, at a fixed rate — a one-speed shuttle that
  cannot follow the hand and that starts ~half a second late. A tap per
  detent keeps the count exact, never arms a repeat timer, and can never strand
  a key if the daemon dies mid-spin.

---

## 1. The problem, in this codebase's terms

### 1.1 What "efficient caret movement" has to beat

Today the caret moves by the D-pad: `h.button("dpad_left", h.key "left")`
(`config/hyprpad.lua:127-130`), pressed on the down edge and released on the up
edge by `drive_buttons` (`src/run.rs:1196`), so holding it repeats at whatever
rate the focused client repeats at. That is a **shuttle with one speed**: a
100-character dictated sentence takes ~4 s to cross at a 25 Hz repeat (and every
held run starts with the repeat *delay*), overshoots are corrected by more
held runs, and there is no way to slow down near the target except to release
and tap. The owner's request — slow where precision is needed, fast when
moving far — is a request for a **position control whose rate follows the
hand**, i.e. a jog wheel.

### 1.2 What already exists (VERIFIED(code))

| piece | where | what it gives the scrub |
|---|---|---|
| Circular-scroll detent engine | `AngleAccumulator` — `src/filter.rs:318-379`; `wrap_pi` `:385` | angle unwrap across the `atan2` branch cut, a dead centre (`min_radius`), whole-tick emission with sub-tick carry, reset on lift |
| Pad smoothing | `PadDamper` — `src/filter.rs:224` (`update`), `:249` (`relative`) | One Euro + moving-centre hysteresis, so a still thumb is hard-zeroed and never ticks (`config/hyprpad.lua:107` sets `hysteresis = 0.0008`) |
| The scroll consumer | `drive_scroll` `src/run.rs:980`; `ScrollMode::Circular` arm `:1013-1025`; `circular_scroll` `:1065` | the exact shape a `drive_scrub` copies: gate → touched? → smooth → `angle.update` → ticks → emitter + one haptic per frame |
| Scroll knobs | `ScrollConfig` `src/config.rs:476-503`; `h.scroll {}` `src/lua_config.rs:1210` | `circular_step_degrees` (default 15°), `circular_min_radius` (0.35), `only_in`/`not_in` guard via `Slot::Scroll` (`:663-674`) |
| Haptic vocabulary | `Feel::{Tick, Click, Buzz, Texture}` `src/haptics.rs:152-178`; `Haptic::Scroll → Feel::Tick` on the left pad `src/run.rs:694`, one pulse per frame max `:1017-1023`; queue depth 8 `src/haptics.rs:103-107` | a per-character tick and a heavier per-word click already have feels |
| Guide layer | `GestureEngine` `src/gesture.rs`; `guide_active()`; `drive_cursor`/`drive_scroll` release both pads while guide is held (`src/run.rs:559-577`) | the left pad is *free* under guide — nothing consumes its motion today |
| Bare-button keyboard | `VirtualKeyboard` `src/keyboard.rs:122` (`key(code, pressed)`), `ButtonKeys::reconcile` `src/run.rs:1097` | a uinput device with real `KEY_*` codes, created at startup; press/release only, no timing of its own |
| Guide chords | `handle_gesture` `src/run.rs:1945`; `execute` `:2170` — `Action::Key(_) => Ok(())` at `:2196` | **a `key` bound to a guide chord is a no-op**, and chords are edge-only (no release event) |
| OSK routing | `route_osk` `src/run.rs:2067`; `osk.key(code)` → child `key <keycode>` (`osk/src/control.rs:129`) → `tap` with two 6 ms sleeps (`osk/src/output.rs:136-149`) | the OSK child can tap arrows on request, at ≤ ~80 taps/s, blocking its render loop |
| Modes and guards | `Guard` `src/config.rs:709`; `ModeEngine::set_mode/clear_mode` `src/mode.rs:301,312`; `desktop_yielded` `:377` | a scrub handler can be guarded like the cursor; an `edit` mode is one `h.mode` line |
| Cheat sheet | `Sheet::build` ambient rows `src/bindings_sheet.rs:227-247`; `SECTION_MODIFIER` `shell/hyprpad.cheatsheet/Callouts.js:30-36` | a pad's ambient behaviour is a row on the pad's callout; the modifier glyph is chosen per *section* |

### 1.3 Key repeat on this stack, precisely (why the scrub taps)

- Wayland: key repeat is **client-side**. `wl_keyboard.repeat_info` "informs
  the client about the keyboard's repeat rate and delay"; `rate` is in
  "characters per second", `delay` in milliseconds, and the client performs the
  repeat (VERIFIED(web), [wayland protocol appendix](https://wayland.freedesktop.org/docs/html/apa.html)).
  Martin Gräßlin's write-up of the same design: repeat "is then handled directly
  in the client", because Wayland key events "are promised to be physically
  generated by input devices and never faked by a compositor" (VERIFIED(secondary),
  [blog](https://blog.martin-graesslin.com/blog/2016/12/how-input-works-keyboard-input/)).
- Kernel: `input_register_device` only enables software autorepeat (`250` ms
  delay, `33` ms period) when the device asks for `EV_REP` and the driver set no
  values (VERIFIED(web), [drivers/input/input.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/input/input.c)).
  `VirtualKeyboard::new` registers only `EV_KEY`/`EV_SYN` (`src/keyboard.rs`),
  so the kernel never repeats for hyprpad; libinput does not implement repeat
  either (VERIFIED(secondary), [libinput FAQ](https://wayland.freedesktop.org/libinput/doc/latest/faqs.html)).
- Hyprland advertises `input:repeat_rate` / `input:repeat_delay`; the widely
  documented defaults are 25 Hz and 600 ms (**unverified** — the wiki refused
  the fetch, see §6).

Consequence: a *held* `KEY_LEFT` moves at one fixed rate after a fixed dead
time, per client, unrelated to the thumb; a *tapped* `KEY_LEFT` per detent moves
exactly one unit, immediately, at whatever rate the thumb chooses, and a tap
shorter than `delay` never starts a repeat timer. The scrub must tap.

---

## 2. Prior art

### 2.1 Steam Input — the Scroll Wheel input source mode (VERIFIED(web))

The Steamworks documentation, [Input Source Modes](https://partner.steamgames.com/doc/features/steam_controller/input_source_modes):

> "This input will operate as a Scroll Wheel. Rotating clockwise or
> counterclockwise will fire off events like a scrollwheel, clickwheel, or
> jogwheel."

| option | text / range | meaning for a caret scrub |
|---|---|---|
| Clockwise Binding / Counter Clockwise Binding | one binding each | bind `Right` / `Left` and you have a caret scrub — this is how Steam Controller users did it on the desktop |
| Sensitivity | 0.0–1.0 — "determines how much of a rotation is required for the next binding to fire" | = `step_degrees`; Valve does not publish the angle per click |
| Swipe Direction | Circular / Horizontal / Vertical | the *linear* variant (option (d)) exists in the same mode |
| Spin Friction | Off / Low / Medium / High / None — "determines spin duration after flicking" | momentum: a flicked wheel keeps ticking — a shuttle bolted onto the jog; avoid for a caret (overshoot) |
| Haptics Intensity | Off / Low / Medium / High | the per-click tick; strength only, no per-tier feel |
| Scroll Wheel List, Wrap List | up to 10 slots, cycle, optional wrap | not relevant to a caret |
| Scroll Wheel Click Action | binding on pad/stick click | a free commit/select button under the spinning thumb |

A community explainer states the detent semantics plainly: "the joystick or
touchpad must be rotate[d] a set distance from the last activation to trigger
the next", "just like real mouse wheels have grooves that dictate when the next
scroll action happens" (VERIFIED(web), [Steam Input Essentials ep. 3](https://bryanrumsey.wordpress.com/2018/06/24/steam-input-essentials-eps-3-input-styles/)).
The clockwise/counter-clockwise bindings are silently disabled if a list slot is
also set (VERIFIED(secondary), [steam-for-linux #10733](https://github.com/ValveSoftware/steam-for-linux/issues/10733)).
Reviewers remember the mode's feel — "a perfect way to allow for continuous
scrolling without having to lift your thumb from the touch pad"
(VERIFIED(secondary), Steam community threads via search). Mouse mode's
"Acceleration" toggle is Valve's velocity-gain: "faster movements … cause more
mouse movement relative to slow movements for the same space covered on the
pad" — the same toggle `docs/research/pointer-damping.md` §3 already cites.

**The Deck's own keyboard has no caret scrub** (VERIFIED(code) in
`docs/research/osk-technology.md` §4, read from the shipped SteamUI bundle):
dual-trackpad typing is an *absolute* position map, the D-pad and left stick
move DOM key *focus*, X = Backspace (auto-repeat 450 ms then 200 ms), Y =
Space, L2 = Shift held, R2 = Enter, L1/R1 = IME candidate up/down, and the
arrows are **keycaps** on row 4 (`[…,57,105,106]` — "arrows added
2023-01-17"). A January 2023 client beta added up/down cursor keys, reached by
"shift then left/right cursor" (VERIFIED(secondary), [Linux Gaming Central summary](https://linuxgamingcentral.com/posts/steam-deck-client-beta-1-6-2023/);
Valve's own post could not be fetched). Deck users in Desktop mode reported the
OSK arrows not moving the caret in Konsole and fell back to "Steam + trackpad"
mouse movement plus "Steam + RT" click to reposition
(VERIFIED(secondary), [bug report](https://steamcommunity.com/app/1675200/discussions/1/3806153359231226480/),
[bug report](https://steamcommunity.com/app/1675200/discussions/1/3273566073554789807/)).
So the closest shipped analogue to the owner's ask on Valve hardware is
"desktop layout, left pad = Scroll Wheel, bindings = Left/Right" — a user
configuration, not a product feature. hyprpad's own OSK mirrors the Deck
table (`osk/src/layout.rs:408-409` puts `KEY_LEFT`/`KEY_RIGHT` on row 4;
`h.osk_button("y", h.key "space")`, `("x", h.key "backspace")` in
`config/hyprpad.lua`).

### 2.2 Phone keyboards — linear drag, relative, bounded

- **iOS / iPadOS "trackpad mode"** (VERIFIED(web), [Apple iPad user guide](https://support.apple.com/guide/ipad/type-with-the-onscreen-keyboard-ipad997da459/ipados),
  text via search excerpt): "touch and hold the Space bar with one finger until
  the keyboard turns light gray. To move the insertion point, drag your finger
  around the keyboard. To select text, touch and hold the keyboard with a
  second finger, then adjust the selection by moving the first finger". History:
  a 3D Touch hard-press on iPhone 6s (iOS 9), generalised to the space-bar hold
  in iOS 12; iPad also accepts two fingers placed on the keyboard
  (VERIFIED(web), [AppleInsider](https://appleinsider.com/articles/18/10/20/how-to-turn-the-ios-12-keyboard-into-a-trackpad-on-any-iphone-or-ipad)).
  The motion is *relative* (the caret moves by the finger's displacement, not
  to its position); whether it is accelerated is **not documented** (§6).
  Selection = a second contact while dragging: the model for "hold a modifier
  while scrubbing".
- **Gboard** (VERIFIED(web), [Google Gboard help](https://support.google.com/gboard/answer/2842292)):
  "To move your cursor, swipe left or right on the space bar" (Android and
  iOS), gated by *Settings → Glide typing → Enable gesture cursor control*
  (VERIFIED(secondary), [Pocket-lint](https://www.pocket-lint.com/how-to-use-the-hidden-cursor-on-gboard/)).
  Gboard also ships a **Text editing panel** — a cursor-control row with
  arrows, select, select-all, cut/copy/paste — reached from the toolbar
  (VERIFIED(secondary), [Computerworld](https://www.computerworld.com/article/1663400/gboard-android-typing-shortcuts.html)):
  the "dedicated mode with explicit keys" design, i.e. option (b) on a phone.
- **BlackBerry** (VERIFIED(secondary), [MobileSyrup](https://mobilesyrup.com/2014/06/25/additional-details-of-the-touch-enabled-keyboard-on-the-blackberry-passport/),
  [CrackBerry](https://crackberry.com/blackberry-passport-keyboard)): the
  Passport's touch-sensitive physical keyboard — "double-tap the keyboard, then
  drag your finger across the keyboard to drop the cursor at precisely the right
  point"; BB10.1's on-screen cursor "circle" moves one character per tap on its
  left/right edge and switches rows on up/down taps. Two ideas worth keeping:
  an explicit *arm* gesture before the drag (double-tap ≈ hold guide), and a
  one-character-per-tap fallback for the last step.

All three are **linear and bounded**: a full-width drag reaches a few dozen
characters, then the finger lifts and re-engages. That is the "rowing" the
circular-scrolling literature was written to eliminate (§2.4).

### 2.3 Console on-screen keyboards — a shared button vocabulary

The Deck's map (§2.1) is the only console-style OSK whose code was read. The
official PlayStation, Nintendo and Xbox pages describing their OSK controller
shortcuts could not be fetched (§6); the widely repeated conventions —
bumpers/triggers move the caret or shift, Y/△ = space, X/□ = backspace, a
modifier + D-pad = word jumps (a Switch forum answer describes "hold ZL and use
the left and right directional buttons to jump over words") — are
**unverified**. The pattern that *is* consistent across every one that could be
checked: the shoulder controls carry caret/space/backspace so the thumbs never
leave the pointing surface, and none of them offers proportional caret speed —
it is always one step per press, repeat by hold.

### 2.4 Jog wheels, shuttle rings, and rotary acceleration — the literature

- **Jog vs shuttle** (VERIFIED(web), [Wikipedia: Jog dial](https://en.wikipedia.org/wiki/Jog_dial)).
  A jog dial is a free-spinning incremental encoder: "frame-by-frame"
  positioning, "the faster it spins forward or back, the faster it
  fast-forwards or rewinds" — but it is still *position*: stop turning, it
  stops. A shuttle ring is spring-loaded with stops, "three or so speeds which
  depend on how far it is turned", and "once the wheel is released, it springs
  back to the middle". Professional edit controllers put both on one knob:
  inner jog, outer shuttle. For the puck: **pad rotation = jog, D-pad held =
  shuttle**. (A radius-band variant — inner ring characters, outer ring words —
  is the jog/shuttle geometry on one pad; see §3.5.)
- **Apple, "Method and apparatus for accelerated scrolling", US 7,312,785**
  (VERIFIED(web), [Google Patents](https://patents.google.com/patent/US7312785B2/en)),
  the iPod click wheel: rotational speed is "determined based on the number of
  rotational units and an amount of time over which such rotational inputs were
  received"; acceleration proceeds "in successive stages" — a four-state machine
  ×1 → ×2 → ×4 → ×8, each transition on sustained fast rotation past a time
  threshold, "the acceleration factor [is] doubled", with an optional cap. The
  factor **multiplies the units** (the count), not the rate: "51 input units × 2
  acceleration factor = 102 units", then chunked into items. This is the model
  §3.5 adopts: multiply detents into taps, in integer stages, with a cap.
- **Moscovich & Hughes, "Navigating documents with the virtual scroll ring",
  UIST 2004** (VERIFIED(web), [PDF](https://www.dgp.toronto.edu/~tomer/store/papers/scrollring04.pdf)).
  A circular-motion scroller on an ordinary touchpad: it fits a circle to the
  last *n* = 30 pointer samples and "scrolls the document by the length of the
  arc traversed on the circle since the last scroll event" (distance `2θr`,
  sign of θ = direction; ~55 events/s). Three findings transfer directly:
  1. It argues *against* angle-only mapping for continuous scrolling: "scrolling
     by angle is a contrary mapping for the task, as slow, small, circles would
     cause fast scrolling" — which is why hyprpad's dead centre
     (`circular_min_radius = 0.35`) matters, and why a scrub must state its
     units as *detents per revolution*, never as "speed".
  2. It names what the wheel has that a ring lacks: "the haptic feedback
     provided by the notched wheel. The clicking of the notches help the user
     determine the distance scrolled, and snap the motion to integer lines." For
     short distances users preferred the wheel — "More accurate, stopping where
     I wanted it to"; "Several mentioned that they found the tactile feedback
     very helpful". The puck has the notches (haptic ticks), so the scrub can be
     a ring *with* the wheel's precision.
  3. Results: the ring beat the wheel at long distances (192 lines) and lost at
     short ones (touchpad ring 1.3 s slower over 6 lines, attributed to the
     touchpad's low-speed gain and a circle too small to fit). "Continuous
     nature of the motion; it does not require the user to release and
     re-engage the input device" is the whole case for circular over linear.
- **Smith & schraefel, "The Radial Scroll Tool", UIST 2004** (VERIFIED(secondary),
  [ACM DL](https://dl.acm.org/doi/10.1145/1029632.1029641)): the same idea
  for stylus/touch; cited for "eliminat[ing] rowing" (clutching).
- **Synaptics ChiralMotion** (VERIFIED(secondary), [Synaptics press release](https://investor.synaptics.com/news-releases/news-release-details/synaptics-chiralmotiontm-technology-provides-intuitive-touch)):
  the productised laptop version — start a linear scroll at the pad edge, then
  "begin making a circular motion"; "the rotation speed directly controls
  scrolling speed"; reversing the circle reverses the scroll. Note the
  *arm-then-circle* entry, the same shape as guide-then-circle.
- **Microsoft Surface Dial** (VERIFIED(secondary), [Windows UWP docs](https://learn.microsoft.com/en-us/windows/uwp/ui-input/windows-wheel-interactions)):
  rotation events are delivered every 10° by default
  (`RotationResolutionInDegrees`), each with a haptic pulse, and Microsoft
  recommends *disabling* the haptic when the resolution is set below 5°
  because "more frequent feedback at higher sensitivities can become
  uncomfortable". A calibration point for detent spacing and for thinning the
  tick at high rates.
- **Logitech SmartShift** (VERIFIED(secondary), [OpenLogi docs](https://openlogi.org/en/docs/guide/smartshift),
  [mxctl](https://github.com/RussellCastro/mxctl)): the MX Master's wheel flips
  from ratchet (detents) to free-spin when spun faster than a threshold —
  "sets how fast you have to spin before the wheel auto-releases", an 8–50
  slider — and returns to ratchet when it slows. A **speed threshold that
  changes the wheel's *unit*** is the model for the character → word tier in
  §3.5, including the need for hysteresis so the tier does not chatter.
- **Rotary-encoder acceleration libraries** (VERIFIED(secondary),
  [RotaryEncoderAcceleration](https://github.com/hamspot/RotaryEncoderAcceleration)):
  the embedded-UI idiom is a time window plus a multiplier — "the faster the
  encoder is rotated the greater the step". Same shape as Apple's, simpler.

What the literature agrees on: **position semantics with detents for
precision, integer stage multipliers for distance, tactile ticks so the count is
felt, and hysteresis on any speed-driven mode change.**

---

## 3. Design options in hyprpad

### 3.0 The common chain

Every option shares the same trunk (all pieces VERIFIED(code)):

```
puck 0x42 report (250 Hz)
   │  frame.left_pad {x, y}  (i16, ±32767)          src/report.rs:94
   ▼
PadDamper::update ── One Euro + moving-centre hysteresis      src/filter.rs:224
   │  smoothed (nx, ny) ∈ [-1, 1]; hard zero when the thumb is still
   ▼
AngleAccumulator::update ── atan2, wrap_pi, dead centre, carry src/filter.rs:350
   │  ticks ∈ ℤ per frame  (+ = CCW, − = CW)
   ▼
[ NEW ]  JogPacer ── angular velocity → tier (×1 / ×2 / word), hysteresis
   │  (direction, unit, count) per frame
   ▼
[ NEW ]  emitter ── n × tap(KEY_LEFT|KEY_RIGHT)  or  tap_with(CTRL, …)
   │                + Haptic::ScrubChar (Tick) / ScrubWord (Click), ≤ 1 per frame
   ▼
VirtualKeyboard (uinput, EV_KEY only)                          src/keyboard.rs:122
   │
   ▼
Hyprland → focused client (no repeat timer ever armed: taps)
```

The scroll path is this chain with `circular_scroll` and `ptr.scroll()` as the
emitter (`src/run.rs:1013-1030`). The options differ only in **what gates the
chain** (guide held / a mode / the OSK / nothing) and **where the D-pad,
selection and word jumps live**.

### 3.1 Option (a) — guide-scoped: hold guide, circle the left pad

```
        guide held? ──no──▶ ambient layer (scroll on the left pad, as today)
             │yes
   drive_cursor / drive_scroll release both pads     src/run.rs:559-577
             │
   ┌─────────┴──────────────────────────────────────────────────────┐
   │ LEFT pad rotation ─▶ chain (§3.0) ─▶ Left/Right taps (×tier)    │
   │ guide + D-pad ↑/↓  ─▶ Up/Down HELD (reconciled like ButtonKeys) │  = shuttle
   │ guide + L5 held    ─▶ Shift held around every tap               │  = select
   │ guide + A          ─▶ dictation toggle (already bound)          │
   └────────────────────────────────────────────────────────────────┘
             │ guide released
   Shift released, D-pad keys released, angle state reset; hold marked consumed
```

Config surface (proposed; mirrors `h.cursor`/`h.scroll`):

```lua
h.scrub {
  pad           = "left",          -- which pad jogs (default left: right keeps its stick flicks)
  step_degrees  = 15.0,            -- one character per detent (24 per revolution)
  min_radius    = 0.35,            -- dead centre, as circular scroll
  accel         = { { above = 360, mult = 2 }, { above = 720, mult = 4 } },  -- deg/s → taps per detent
  words         = { above = 540, below = 270 },  -- phase 2: word tier (ctrl+left/right), enter/exit ω
  lines         = "dpad",          -- guide+dpad ↑/↓ = Up/Down, held (client repeat)
  select_with   = "l5",            -- hold this while jogging → Shift held
  only_in       = { "desktop", "omarchy-ui" },
}
```

Why guide: the left pad is already vacated while guide is held; the guide layer
is unguarded in the sample config so the scrub works over an Omarchy menu too;
and `guide+a` (dictation) is one thumb away — *hold guide, tap A to stop
dictation, circle to the mistake, type, tap A to resume* is a single hold.

Costs and quirks (all VERIFIED(code) unless marked):

- **`chordable` excludes `PadLeftTouch`** (`src/gesture.rs:211-216`), so a
  scrub-only hold ends in `GuideLeave { was_chorded: false }` — the "bare tap"
  that a `guide_tap` binding resolves against (`GestureKey::Tap`,
  `src/config.rs:244-250`). The scrub must mark the hold consumed on its first
  detent (a `GestureEngine::consume_hold()` or equivalent), or a future
  `guide_tap` binding would fire after every caret fix.
- **Steam still sees the release.** Steam acts on the guide button's release
  and ignores holds longer than ~3 s (`README.md:35-40`,
  `docs/03-hardware-findings.md` "Guide-button timing"). A caret fix shorter
  than that ends with the same trailing focus steal every guide chord already
  has, mitigated by `suppressevent activatefocus, match:class steam`
  (`docs/06-recommendation.md:44`). Not new, but a scrub session is *long*
  compared to a chord, so it will be noticed if the rule is not in place.
- **Triggers are taken under guide** (`guide+l2`/`guide+r2` = group prev/next,
  `config/hyprpad.lua`), so "hold a trigger to select" collides; the left grips
  L4/L5 are free and are under the fingers of the same hand whose thumb is
  circling — `select_with = "l5"` by default. R3/L3 and the four D-pad chords
  are also free.
- **`guide+b` = close window** sits under the other thumb while it scrubs.
  Nothing to change, but the cheat sheet should show it on the same tab so the
  user is not surprised.
- **Guide-chord keys are no-ops** (`execute`, `src/run.rs:2196`) and chords have
  no release edge, so `h.bind("guide+dpad_up", h.key "up")` cannot be the line
  mover. The `lines = "dpad"` knob instead installs a second `ButtonKeys` map
  reconciled while the scrub is armed, which gets hold-to-repeat for free —
  exactly `drive_buttons` with a different gate.

Discoverability: one row on the left-pad callout of every tab whose guard
passes — `Ⓢ + [left pad]  Scrub the caret (15°/char)` — plus `Ⓢ + D-pad ↑↓
Line up/down` and `Ⓢ + L5 (hold)  Select while scrubbing`. Needs a
guide-modifier ambient section (§4).

Accidental entry: essentially none — guide has to be held *and* the pad
circled past the dead centre; a thumb resting on the pad while pressing
`guide+r1` moves nothing (the accumulator needs 15° of rotation and the damper
hard-zeroes a still thumb). The failure mode is the opposite: forgetting to
hold guide and *scrolling* the window instead — recoverable and obvious.

### 3.2 Option (b) — an `edit` mode

```
guide+l3 ─▶ h.set_mode "edit"  (manual override, sticky)        src/mode.rs:301
   │
   │  mode tab "edit" (auto on the cheat sheet; bar widget reads "edit")
   │  ┌────────────────────────────────────────────────────────────┐
   │  │ LEFT pad   ─▶ chain (§3.0), *ambient* (no guide)            │
   │  │ RIGHT pad  ─▶ cursor as usual (h.cursor only_in += "edit")  │
   │  │ D-pad      ─▶ arrows, held                                  │
   │  │ L1 / R1    ─▶ ctrl+left / ctrl+right (word jumps)           │
   │  │ L2 / R2    ─▶ Home / End                                    │
   │  │ A = Enter, X = Delete, Y = Backspace, hold L5 = Shift       │
   │  │ B  ─▶ leave edit mode                                       │
   │  └────────────────────────────────────────────────────────────┘
   ▼
guide+l3 again / B ─▶ h.clear_mode()
```

```lua
h.mode("edit")                                   -- no rule: manual only  (src/config.rs:664)
h.scrub  { … , only_in = { "edit" } }            -- ambient in this mode
h.scroll { … , only_in = { "desktop", "omarchy-ui" } }   -- left pad is the scrub here, not scroll
h.button("l1", "Word left",  h.key "ctrl+left"):only_in("edit")   -- needs KeyChord
h.button("l2", "Line start", h.key "home"):only_in("edit")
h.button("x",  "Delete",     h.key "delete"):only_in("edit")      -- `delete` is not in key_code yet
h.bind("guide+l3", "Edit mode", h.toggle_mode "edit")             -- needs toggle_mode
```

Contrast with (a):

| | (a) guide-scoped | (b) `edit` mode |
|---|---|---|
| discoverability | one row per tab, under a modifier glyph | **its own tab** on the sheet and its own word in the bar widget — the best the system can do |
| accidental entry | none (needs a hold + rotation) | needs a chord; **accidental persistence** is the risk: a manual override "sticks across a focus change" (`src/mode.rs` tests: "the manual override beats a matching rule"), so an `edit` left on in a text field is still on in the game you alt-tab to, and the `game` rule cannot fire until it is cleared |
| hands | guide thumb is busy; the free buttons are the D-pad, grips, L3/R3 | every button is free: bumpers = word jumps, triggers = Home/End, face = Enter/Delete/Backspace |
| machinery | scrub handler + guard | scrub handler + guard **plus** `h.toggle_mode`, a way for a *bare* button to run an action (`ButtonAlt.code` is a `u16`, `src/config.rs:690-705` — B cannot `clear_mode`), and per-mode *chord* overrides if `guide+b` is to mean something else in `edit` (`bindings: HashMap<GestureKey, Action>` holds one action per chord; docs/13 lists overrides as not built) |
| dictation pairing | hold guide, tap A, fix, tap A | enter edit, guide+A (chords stay live in every mode), fix, guide+A, leave edit |

A non-sticky override ("clear on next focus change", `h.set_mode("edit", {
until = "focus" })`) would remove the persistence hazard and is a small
addition to `ModeEngine::focus_changed`. Without it, (b) should not be the
first thing shipped.

### 3.3 Option (c) — inside the OSK

```
OSK up (route_osk owns both pads)                          src/run.rs:2067
   │
   │  L1 (an OSK helper) toggles the LEFT pad:  typing cursor  ⇄  scrub
   │
   │  scrub: OskRoute.left ─▶ chain (§3.0) ─▶ either
   │      (i)  osk.key(105|106)  → child `key` cmd → tap() with 2×6 ms sleeps   osk/src/output.rs:136
   │      (ii) the daemon's own VirtualKeyboard (exists already)
   ▼
```

For: the OSK already owns the pads and has a `key <keycode>` command; the
child's uinput keyboard is the one the typed text comes from, so caret and
text share a device. Against, and decisive:

- The child's `tap` sleeps 6 ms twice per key **on its event loop**
  (`osk/src/output.rs:144,149`), capping ~80 taps/s and stalling cursor
  rendering during a fast spin. Emitting from the daemon's keyboard (ii) fixes
  that but then the OSK is only the *gate*, and the gate is the expensive part.
- The OSK helpers can only tap keys (`h.osk_button(btn, h.key …)`), so the
  toggle needs a new OSK-context action.
- It costs the left half of the keyboard while toggled (the left pad's typing
  region is the leftmost 55 %, `docs/research/osk-technology.md` §4.1).
- **Dictation fixes happen with the OSK down.** The owner's primary case is
  editing `voxtype` output; raising the keyboard to move the caret is the
  wrong order of operations. The Deck's precedent is a *keycap* arrow row, which
  hyprpad's OSK already has.

Verdict: not as the primary path. Worth doing later as *the same handler with
the OSK gate* — while the OSK is up, `guide` + left-pad rotation should scrub
exactly as it does on the desktop (today `route_osk` runs only when guide is
not consuming; `handle_gesture` suppresses everything but the keyboard toggle
under the OSK, `src/run.rs:1973-1975`, so this needs a one-line exception).

### 3.4 Option (d) — the "caret mouse": linear pad motion → taps

```
LEFT pad (guide held, or in edit mode)
   │  PadDamper::relative(s, sens, …)  → (dx, dy) in pad counts     src/filter.rs:249
   ▼
accumulate dx / chars_per_unit  → n taps Left/Right   (sub-char remainder carried)
   speed term: One Euro's own |dx̂| → ×2 above a threshold (iOS-style acceleration, INFERRED)
```

This is the phone model (§2.2) on a pad. Compared with circular:

| | circular (jog) | linear (caret mouse) |
|---|---|---|
| travel per engagement | **unbounded** — "does not require the user to release and re-engage" (Moscovich & Hughes) | bounded by pad width: at 1 char per 0.05 of the normalized range a full-width swipe is ~40 characters, then lift and re-swipe ("rowing") |
| precision for ±1 char | one 15° detent; at r = 0.6 that is an arc of ~0.16 normalized units — roughly 3–4 mm if the pad is ~40 mm across (**pad size unverified**, §6) | one 0.05-unit slide ≈ 1 mm; finer, but with no natural stop — the same dead-band/hysteresis machinery is required |
| feel | matches the scroll idiom the owner already uses on this pad, and the notched-wheel haptic that the VSR study's users asked for | matches the phone idiom; no rotation to learn |
| fatigue (INFERRED) | continuous small circles; sustained ~1 rev/s is easy for tens of seconds | repeated swipes with lifts; each lift costs a re-clutch and a re-seed of the damper |
| failure mode | small circles near the dead centre tick fast (VSR's "contrary mapping") — bounded by `min_radius` | overshoot at the pad edge; a diagonal swipe also emits vertical motion unless clamped |

The two are not exclusive: the same `h.scrub` section can carry
`motion = "circular" | "linear"` (as Steam's Scroll Wheel mode does with
Circular/Horizontal/Vertical), with the linear variant reusing `PadDamper::relative`.
Recommendation: circular first, linear as a later knob for anyone who finds
the circle awkward on the puck's pad.

### 3.5 The velocity / acceleration model, concretely

**Detent.** `step_degrees = 15` (24 characters per revolution), inherited from
`circular_step_degrees` where it has been tuned on this device for scroll. If it
proves twitchy for text, 20° (18/rev) is the next stop; the Surface Dial's
default is 10° and Microsoft's advice is to drop the haptic below 5°, so 15°
with a tick is well inside the comfortable band.

**Rate.** Angular velocity `ω` in °/s from the *smoothed* angle: per frame,
`Δθ = wrap_pi(θ − θ_prev)` (already computed inside `AngleAccumulator::update`,
`src/filter.rs:366`) over the measured `dt`, low-passed with an EMA at ≈ 5 Hz
(α ≈ 0.11 at 250 Hz, the table in `pointer-damping.md` §2.2). At 15°/detent,
ω = 360°/s (one revolution per second) is 24 characters/s.

**Tiers** (Apple's staged doubling, capped; INFERRED numbers):

| tier | enter at | exit at | unit per detent | haptic |
|---|---|---|---|---|
| ×1 | — | — | 1 × `Left`/`Right` | `Tick`, left pad |
| ×2 | ω > 360°/s for ≥ 2 consecutive detents | ω < 180°/s | 2 taps | `Tick` |
| ×4 (phase 1 cap) | ω > 720°/s for ≥ 2 detents | ω < 360°/s | 4 taps | `Tick` |
| **word** (phase 2, replaces ×4) | ω > 540°/s for ≥ 3 detents | ω < 270°/s | 1 × `ctrl+Left`/`ctrl+Right` | `Click` |

Rationale: (1) multiply the *count*, not the rate — the count is what the
thumb feels and what the ticks report (Apple, §2.4); (2) a 2:1 enter/exit gap
is the SmartShift lesson — without it a thumb hovering at the threshold
chatters between units; (3) the "≥ N consecutive detents" arming means a single
fast flick never jumps a tier, and a tier change never emits a tick of its own
(no burst on entry); (4) cap at ×4 for characters because, unlike a list, the
caret gives no preview of where ×8 would land; (5) prefer the **word tier** to
×4 once modifiers exist: dictation errors are word-shaped, `ctrl+left` lands on a
boundary, and the heavier `Click` tells the thumb the unit changed. Average
English word + space is ~6 characters, so the word tier is ≈ ×6 with
boundary-snapping, at the same 24 detents/rev.

Throughput check (INFERRED): a 500-character rambling dictation at ×1 is ~21
revolutions; at ×2 from the second revolution, ~11; in the word tier, ~85 words
≈ 3.5 revolutions — a few seconds either way, and the last revolution is always
at ×1 for the landing.

**Alternative tier selector — radius bands** (the jog/shuttle geometry on one
pad, INFERRED): inner band `0.35–0.7` = characters, outer band `0.7–1.0` =
words. Deterministic and needs no velocity hysteresis, but the thumb's circle
radius is hard to hold steady on a small pad and the VSR authors note users
naturally *vary* amplitude to control speed. Offer as `tier_by = "speed" |
"radius"` only if speed tiers prove hard to control on-device.

**Hysteresis so slow motion never double-fires** — three layers, two of which
exist:

1. The `PadDamper`'s moving-centre hysteresis (`hysteresis = 0.0008`
   normalized, `config/hyprpad.lua:107`) hard-zeroes a still thumb, so sensor
   noise never reaches the angle at all (`src/filter.rs:138`).
2. `AngleAccumulator` truncates toward zero and carries the remainder
   (`src/filter.rs:369-371`): after a tick the accumulator is at `r ∈ [0,
   step)`, so a reversal needs a **full step** before the opposite tick fires —
   there is already one detent of hysteresis in the reverse direction, and the
   forward direction cannot re-fire without another full step. A thumb parked
   on a detent boundary and breathing ±1° emits nothing.
3. New: the tier state machine's own enter/exit gap and arming count (above).

An explicit Schmitt band on the detent itself (`detent_hysteresis = 0.2 ×
step`) is therefore *not* proposed for phase 1; add it only if on-device
testing shows double-fires, which the two existing layers should prevent.

**Taps, and the auto-repeat boundary.** Each detent emits `n` complete taps
(`key(code, true)` then `key(code, false)`, each followed by `SYN_REPORT`),
back-to-back in the same frame. Because the key is never held past a frame, no
client repeat timer reaches its delay; because every unit is a discrete event,
the count is exact and a daemon crash mid-spin leaves nothing pressed. Rate
ceiling: at 3 rev/s and ×4 the emitter produces ~290 taps/s — slightly more
than one per 4 ms frame — trivially within uinput and toolkit budgets
(INFERRED; the OSK's 6 ms sleeps between press and release suggest verifying
that a zero-gap press/release pair is honoured by XWayland clients, §6). The
*shuttle* stays where it is: the D-pad is held, the client repeats at its
`repeat_info` rate, and the two never interact because the scrub never holds
an arrow.

**Haptics.** One `Tick` per frame that emitted ≥ 1 character detent (the
existing `Haptic::Scroll` policy, `src/run.rs:1017-1023`) — at 24–48 detents/s
that is one pulse per 5–10 frames, comfortably below the 8-deep writer queue's
drop point; a `Click` per word detent; `[haptics] scrub = true` as its own
toggle beside `scroll`. Above ~50 detents/s thin to every other detent (the
Surface Dial's "uncomfortable" band) rather than let the queue drop pulses at
random.

**Direction.** Clockwise = forward (`Right`), counter-clockwise = back (`Left`)
— the reading direction of a clock hand; `natural = true` inverts, as scroll
does. Shift-selection is symmetric: `Shift` goes down on the grip's down edge
and up on its release, on guide release, and in `mode_handoff`
(`src/run.rs:787`), so a mode transition under a held grip cannot strand it.

### 3.6 Options compared

| | (a) guide-scoped | (b) `edit` mode | (c) in the OSK | (d) linear caret mouse |
|---|---|---|---|---|
| works with OSK down (dictation) | yes | yes | **no** | yes |
| new machinery for phase 1 | handler + config + sheet row | (a) + toggle/non-sticky override + bare-button actions | (a) + OSK gate/toggle action | handler variant of (a) |
| word jumps | phase 2 (modifiers) | bumpers, needs modifiers | phase 2 | phase 2 |
| selection | grip = Shift | grip or trigger = Shift | not planned | grip = Shift |
| lines | guide+D-pad, held | D-pad, held | D-pad already moves key focus | same as (a) |
| discoverability | row under a Ⓢ glyph on each tab | **own tab + bar word** | row on the `osk` tab | as (a) |
| accidental entry / persistence | none / none | chord / **sticky override** | toggle / OSK-bound | as (a) |
| hand load | guide thumb committed | all buttons free | one bumper | as (a) |

---

## 4. What it needs from hyprpad

Effort: **S** = an hour or two, **M** = a day, **L** = days. "Exists" cites the
code that is reused as-is.

| # | change | exists today | new work | effort | needed by |
|---|---|---|---|---|---|
| 1 | `ScrubState` + `drive_scrub(kbd, frame, st, armed, hx, now)` in `src/run.rs`, called from the report arm beside `drive_scroll` | `ScrollState`/`drive_scroll` are the template (`src/run.rs:919-1030`); `AngleAccumulator`, `PadDamper`, `damper_from` (`:812`) | copy the circular arm with a tap emitter; reset on lift, on disarm, in `reset_frame_state` and `release_outputs` (`:772-800`) | M | all |
| 2 | Angular-velocity estimate + tier state machine (`JogPacer` in `src/filter.rs`, pure, unit-tested like the accumulator's arc tests `src/filter.rs:600-676`) | `wrap_pi`, measured `dt` in `PadDamper` | EMA of Δθ/dt; tiers with enter/exit thresholds and arming counts; returns `(unit, count)` | S–M | all |
| 3 | `VirtualKeyboard::tap(code)` and `tap_with(mods: &[u16], code)` | `key(code, pressed)` (`src/keyboard.rs:122`); the OSK's `tap` as a reference (`osk/src/output.rs:136`) | press/release pairs; hold mods around the pair | S | all |
| 4 | **Modifier combos as an output**: `Action::Key(u16)` → `Action::Key(KeyChord { mods, code })` (or a sibling `KeyChord` variant), `key_code` accepting `ctrl+left`, `shift+…`, plus `leftctrl`/`leftshift`/`leftalt`/`leftmeta` and `delete` names | `key_code` table (`src/config.rs:329-352`) has no modifier names; `ButtonAlt.code: u16` (`:690-705`), `buttons: HashMap<Button, u16>`, `bindings_sheet::key_name` (`src/bindings_sheet.rs:702`) all assume one code | touch config/TOML/Lua parse, `ButtonKeys::reconcile` (hold mods for a held combo, release in reverse), sheet rendering | M | word tier; (b)'s bumpers |
| 5 | `ScrubConfig` + `h.scrub {}` (`Slot::Scrub`, guard) + TOML `[scrub]` | `ScrollConfig`/`section_scroll` (`src/lua_config.rs:1210-1250`), `Slot` (`:663-674`), `cursor_guard`/`scroll_guard` plumbing (`src/config.rs:864-866`, `:1435-1453`) | one more section, one more guard slot, `scrub_enabled_in` | S–M | all |
| 6 | Mode-engine gate: `scrub_enabled()`; for (b), scrub counts against `desktop_yielded` (`src/mode.rs:377`) so a forwarding mode with a live scrub warns like a live cursor does (`:430-440`) | `cursor`/`scroll` flags in `refresh_declared` (`:465`) | one flag | S | (a) via guard; (b) |
| 7 | Guide-hold consumption: the scrub's first detent marks `was_chorded` (`GestureEngine::consume_hold()`), so a `guide_tap` binding cannot fire after a fix | `was_chorded` is private to `GestureEngine` (`src/gesture.rs:82-95`) | one method | S | (a) |
| 8 | Guide-layer D-pad → Up/Down with hold semantics: a second `ButtonKeys` reconciled against `scrub.lines` while armed | `ButtonKeys::reconcile` is already gate-aware (`src/run.rs:1097`) | instantiate with `active = guide_active && scrub live`; release in `mode_handoff` | S | (a) |
| 9 | Selection modifier: `Shift` down/up on the grip's edges while armed; released on guide leave and in `mode_handoff` | `frame.edges_down` (`src/report.rs:132`), `kbd.key(42, …)` | small state in `ScrubState` | S | (a)/(b) |
| 10 | Haptics: `Haptic::ScrubChar → Tick`, `Haptic::ScrubWord → Click`, `[haptics] scrub` toggle; rate thinning above ~50/s | `Haptic` enum + `haptic_feel` (`src/run.rs:667-705`), `HapticsConfig` (`src/config.rs`) | two variants, one knob | S | all |
| 11 | Cheat sheet: a `Section::GuideAmbient` (wire `guide_ambient`, modifier `guide` in `Callouts.js` `SECTION_MODIFIER`) and rows for the pad, the D-pad and the grip | ambient rows (`src/bindings_sheet.rs:227-247`), section→modifier map (`Callouts.js:30-38`) | one section + three synthesized rows; JSON consumers see a new `section` string | S–M | (a) |
| 12 | Status/bar: nothing for (a) — guide-held is not a mode. For (b) the `edit` mode appears automatically (`status.set_mode`, `src/status.rs:199`) | — | — | 0 | — |
| 13 | For (b): `h.toggle_mode`, a non-sticky override (`until = "focus"` cleared in `focus_changed`), bare buttons that run actions (B → `clear_mode`), per-mode chord overrides | `set_mode`/`clear_mode` (`src/mode.rs:301-320`); overrides listed as open in `docs/13` | three separate features | M + M + L | (b) only |
| 14 | For (c): scrub while the OSK is up (guide + left pad), `handle_gesture`'s OSK suppression exception (`src/run.rs:1973-1975`) | `route_osk` releases nothing for guide today | small | S | (c) |
| 15 | Tests: tier hysteresis (no chatter at a threshold, no burst on entry), count exactness across a 3-revolution sweep, reverse-after-tick needs a full detent, taps never leave a key down after `reset` | the accumulator's `on_circle` test helpers (`src/filter.rs:678`) | pure tests | S | all |

Guard semantics: `h.scrub { only_in = … }` is evaluated like the cursor's and
scroll's guard — on a context change, cached, read per frame. Under (a) the
handler is *additionally* gated on `engine.guide_active()`; because
`gamepad_forwarding` already yields to guide (`src/run.rs:1256-1262`), a scrub
can never fight the virtual pad, and a `game` mode simply guards it out like
everything else.

---

## 5. Recommendation

Ranked:

1. **(a) guide-scoped circular scrub** — ships on the existing engine, pairs with
   the existing dictation chord, has no persistence hazard, and reads as one
   line on the sheet.
2. **(b) `edit` mode** — the better *destination* for someone who does long
   editing sessions (all buttons free, own tab, own bar word), but only after
   a non-sticky override exists; build it as the same handler behind a different
   gate, not as a second implementation.
3. **(d) linear** — a `motion = "linear"` knob on the same section for people
   who dislike circling; cheap once (a) exists.
4. **(c) OSK-internal** — reduce to "guide + left pad scrubs while the OSK is up
   too", one exception in `handle_gesture`; never the only path.

**Phase 1** (rows 1, 2, 3, 5, 7, 8, 9, 10, 11, 15; no modifier combos):
`h.scrub { step_degrees = 15, accel = { {above=360, mult=2} } }`; guide + left
pad → `Left`/`Right` taps, ×2 above one revolution per second with 2:1
hysteresis, `Tick` per detent; guide + D-pad ↑/↓ held → `Up`/`Down`; hold L5 →
`Shift`. Measure on-device: the comfortable revolution rate, whether 15° is
right for text, whether a zero-gap tap is honoured everywhere.

**Phase 2** (row 4): `KeyChord`; the **word tier** (`ctrl+left/right`, `Click`)
replaces ×4; `h.button("l1", h.key "ctrl+left")` becomes expressible for (b)
and for anyone who wants word jumps on bare bumpers on the desktop.

**Phase 3** (rows 6, 13, 14): `edit` mode with a focus-scoped override; OSK
exception; `motion = "linear"`.

**On the cheat sheet.** In (a), every tab where `h.scrub`'s guard passes shows
on the left-pad callout, under the pad's own ambient row:

```
[left pad]   Scroll (circular)
             Ⓢ + ⟳  Scrub the caret · 15°/char · fast = ×2
[D-pad]      ↑ ↓ ← →  Arrow keys
             Ⓢ + ↑ ↓  Line up / down
[L5]         Ⓢ + L5 (hold)  Select while scrubbing
```

— the same "chord you press, then what it does" row grammar the sheet already
uses (`README.md`, "The cheat sheet"), with the guide glyph supplied by the new
section's modifier. In (b) the `edit` tab lists all of it with no glyphs, and
the bar reads `edit`.

---

## 6. What could not be verified

- **Hyprland's default `repeat_rate` / `repeat_delay`** (25 Hz / 600 ms is what
  every secondary source says; the wiki and the `ConfigDescriptions.hpp` fetch
  both failed). The argument in §1.3 does not depend on the numbers.
- **The puck's trackpad diameter**, hence the physical arc length of a 15°
  detent; `docs/03-hardware-findings.md` records counts and rates, not
  millimetres. All geometry above is in normalized units.
- **Steam Input's angle per Scroll Wheel "click"** at a given sensitivity —
  Valve documents the slider, not the mapping.
- **Whether iOS trackpad mode is accelerated** — Apple documents only the
  gesture.
- **PlayStation 5, Nintendo Switch and Xbox OSK button maps** — official pages
  redirected or required JavaScript; the conventions in §2.3 are community
  lore.
- **Valve's own text for the 2023-01 Deck OSK cursor-key change** — only a
  secondary summary was reachable.
- **A zero-gap press/release pair from uinput reaching XWayland clients
  correctly** — the daemon has only ever emitted press and release on separate
  frames; the OSK sleeps 6 ms. Test before trusting bursts of taps.
- **`voxtype`'s insertion behaviour** (caret left at the end of the
  transcription) — assumed; it is what every dictation tool does.

---

## 7. Sources

External (fetched or searched 2026-09-01):

- Steamworks, *Input Source Modes* — Scroll Wheel, Mouse (Acceleration), D-Pad, Touch Menu: <https://partner.steamgames.com/doc/features/steam_controller/input_source_modes>
- Bryan Rumsey, *Steam Input Essentials ep. 3: Input Styles* — the "click" definition: <https://bryanrumsey.wordpress.com/2018/06/24/steam-input-essentials-eps-3-input-styles/>
- ValveSoftware/steam-for-linux #10733 — clockwise/counter-clockwise vs list bindings: <https://github.com/ValveSoftware/steam-for-linux/issues/10733>
- Steam Deck bug reports — OSK arrows in Desktop mode, the "Steam + trackpad" workaround: <https://steamcommunity.com/app/1675200/discussions/1/3806153359231226480/>, <https://steamcommunity.com/app/1675200/discussions/1/3273566073554789807/>
- Linux Gaming Central — Steam Deck client beta 2023-01-06/17, up/down cursor keys: <https://linuxgamingcentral.com/posts/steam-deck-client-beta-1-6-2023/>
- Apple, *Type with the onscreen keyboard on iPad* — trackpad mode: <https://support.apple.com/guide/ipad/type-with-the-onscreen-keyboard-ipad997da459/ipados>
- AppleInsider — iOS 12 keyboard trackpad on any device: <https://appleinsider.com/articles/18/10/20/how-to-turn-the-ios-12-keyboard-into-a-trackpad-on-any-iphone-or-ipad>
- Google, *Gboard Help — Use your keyboard*: <https://support.google.com/gboard/answer/2842292>
- Pocket-lint — Gboard gesture cursor control: <https://www.pocket-lint.com/how-to-use-the-hidden-cursor-on-gboard/>
- Computerworld — Gboard text-editing panel: <https://www.computerworld.com/article/1663400/gboard-android-typing-shortcuts.html>
- MobileSyrup / CrackBerry — BlackBerry Passport touch keyboard, BB10.1 cursor: <https://mobilesyrup.com/2014/06/25/additional-details-of-the-touch-enabled-keyboard-on-the-blackberry-passport/>, <https://crackberry.com/blackberry-passport-keyboard>
- Wikipedia, *Jog dial*: <https://en.wikipedia.org/wiki/Jog_dial>
- Apple, US 7,312,785 *Method and apparatus for accelerated scrolling*: <https://patents.google.com/patent/US7312785B2/en>
- Moscovich & Hughes, *Navigating documents with the virtual scroll ring*, UIST 2004: <https://www.dgp.toronto.edu/~tomer/store/papers/scrollring04.pdf>
- Smith & schraefel, *The radial scroll tool*, UIST 2004: <https://dl.acm.org/doi/10.1145/1029632.1029641>
- Synaptics, *ChiralMotion* press release: <https://investor.synaptics.com/news-releases/news-release-details/synaptics-chiralmotiontm-technology-provides-intuitive-touch>
- Microsoft, *Surface Dial interactions* — `RotationResolutionInDegrees`, haptics guidance: <https://learn.microsoft.com/en-us/windows/uwp/ui-input/windows-wheel-interactions>
- OpenLogi, *SmartShift*; mxctl — ratchet/free-spin threshold: <https://openlogi.org/en/docs/guide/smartshift>, <https://github.com/RussellCastro/mxctl>
- hamspot, *RotaryEncoderAcceleration*: <https://github.com/hamspot/RotaryEncoderAcceleration>
- Wayland protocol, `wl_keyboard.repeat_info`: <https://wayland.freedesktop.org/docs/html/apa.html>
- Martin Gräßlin, *How input works — keyboard input* (client-side repeat): <https://blog.martin-graesslin.com/blog/2016/12/how-input-works-keyboard-input/>
- libinput FAQ (no key repeat in libinput): <https://wayland.freedesktop.org/libinput/doc/latest/faqs.html>
- Linux `drivers/input/input.c` — `input_enable_softrepeat(dev, 250, 33)`: <https://raw.githubusercontent.com/torvalds/linux/master/drivers/input/input.c>
- libinput pointer acceleration (velocity gain, already cited by `pointer-damping.md`): <https://wayland.freedesktop.org/libinput/doc/latest/pointer-acceleration.html>

This repository (commit `e2ddad0`):

- `src/filter.rs:138` `hysteresis`; `:224` `PadDamper::update`; `:249` `PadDamper::relative`; `:318-379` `AngleAccumulator`; `:385` `wrap_pi`; `:600-676` arc tests
- `src/run.rs:559-577` guide releases both pads; `:667-705` `Haptic`/`haptic_feel`; `:772-800` `release_outputs`/`mode_handoff`; `:812` `damper_from`; `:919-1030` `ScrollState`/`drive_scroll`; `:1013-1025` circular arm + one tick per frame; `:1065` `circular_scroll`; `:1077-1130` `ButtonKeys`; `:1196` `drive_buttons`; `:1256` `gamepad_forwarding`; `:1945` `handle_gesture`; `:1973-1975` OSK suppression; `:2067` `route_osk`; `:2170-2196` `execute` (`Action::Key` no-op for chords)
- `src/gesture.rs:82-95` engine state; `:132,141` `was_chorded`; `:211-216` `chordable` (pad touch excluded)
- `src/keyboard.rs:122` `VirtualKeyboard::key`; EV_KEY-only registration in `new`
- `src/config.rs:125` `Action::Key(u16)`; `:244-250` `GestureKey::of`; `:329-352` `key_code`; `:437-503` `ScrollMode`/`ScrollConfig`; `:664` rule-less modes; `:690-705` `ButtonAlt`; `:709` `Guard`; `:864-866` cursor/scroll guards
- `src/lua_config.rs:663-674` `Slot`; `:778` `h.scroll`; `:796-797` `h.key`/`h.mouse`; `:1210-1250` `section_scroll`
- `src/mode.rs:301-320` `set_mode`/`clear_mode`; `:377` `desktop_yielded`; `:408` `allows_gesture`; `:430-440` forward-with-live-desktop warning; `:465` `refresh_declared`
- `src/bindings_sheet.rs:154` `OSK_MODE`; `:227-247` ambient rows; `:702` `key_name`
- `shell/hyprpad.cheatsheet/Callouts.js:26-38` sections, modifiers, ranks
- `src/haptics.rs:103-107` queue depth; `:152-178` `Feel`; `:297` `play`
- `src/report.rs:94` `Pad`; `:132` `edges_down`
- `osk/src/control.rs:129` `key` command; `osk/src/output.rs:136-149` `tap` with 6 ms sleeps; `osk/src/layout.rs:408-409` arrow keycaps
- `config/hyprpad.lua:102-116` `h.cursor`/`h.scroll`; `:127-132` D-pad/A/B bare keys; guide chords section (`guide+a` dictation, `guide+l2`/`r2` tabs, `guide+b` close)
- `docs/research/osk-technology.md` §4 — the Deck OSK's code-verified button map and absolute pad typing
- `docs/research/pointer-damping.md` §2.2 (α table), §3 (Steam Input mouse mode)
- `docs/research/haptics.md` — the pulse vocabulary and its device verification
- `docs/03-hardware-findings.md` — 250 Hz reports, guide-button timing; `README.md:35-40` — Steam acts on guide release, ignores > ~3 s holds; `docs/06-recommendation.md:44` — `suppressevent activatefocus`

---

## 8. The shuttle as built — 2026-09-03

*Written after adding the padless half of `h.scrub`, once the daemon had a
second backend (`docs/research/xbox-elite.md`) and the owner asked the obvious
question: "how do I scrub with the Xbox controller, without the trackpad?" This
section supersedes §3.4's sketch for that case; nothing above changes for the
puck, whose left pad is still the jog wheel §3.1 describes.*

**The answer is the other half of §0's own distinction.** Jog is *position*
(one detent, one step, the rate follows the hand) and shuttle is *velocity*
(deflect to choose a speed, release to stop). A trackpad has an absolute
position to count, so it earns the jog wheel. A stick springs back to centre and
has none — asking a thumb to "circle" a self-centring stick would be a worse
version of both — so on a source with no pads (`report::Source::has_pads`, the
same tag the cursor and scroll paths switch on) the identical `h.scrub` binding
becomes a shuttle on the **left stick's horizontal axis**. Same guard, same
`select`-with-Shift level, same taps, same consumed hold; only the gesture
differs, and the device picks it, not the config.

Note this is *not* §3.4's option (d): that was a linear variant of the **pad**,
reusing `PadDamper::relative` and still position control. The stick could not
reuse it — there is no travel to accumulate — so what was built is the rate
control §0 contrasts the jog wheel with, and the sketch's "linear pad" variant
remains unbuilt and still available as a later `motion = "linear"` knob.

### The curve

Four knobs, `h.scrub { shuttle = { … } }` / `[scrub] shuttle_*`, all optional:

| deflection \|x\| | tap rate | unit |
|---|---|---|
| ≤ `deadzone` (0.15) | — | nothing; the run is parked and the loop may block |
| `deadzone` … `word_above` | `slow_per_s` (4/s) rising **linearly** to `fast_per_s` (25/s) | `Left`/`Right` |
| ≥ `word_above` (0.85) | `fast_per_s` (25/s) | `ctrl`+arrow — a word, and the heavier haptic where one exists |

So: ~4 characters/s just off the deadzone, ~14/s at half deflection, 25/s just
under the word threshold, and past it the same 25/s in *words* — the top gear,
about six times the caret speed, and the reason the ladder tops out in words
rather than in more characters is §3.5's: a word jump lands on a boundary
instead of somewhere inside one, which is the shape a dictation error has.

The deadzone is deliberately above the pointer's `0.12` (`[sticks]`): a resting
thumb that drifts a cursor is a nuisance, and one that types arrow keys is a
defect in the document. Only the horizontal axis is read — the D-pad already
walks lines (§3.1), and a vertical channel would make every diagonal thumb a
caret jumping rows nobody asked for.

### What it needed from the daemon

- **`filter::ShuttlePacer`** — pure, `Instant`-driven, the [`JogPacer`] of this
  gesture: deflection in, at most one tap out per call, and the schedule for the
  next. First tap immediate (a flick of the stick is one character, the fine
  adjustment); a reversal is a fresh run, never a late tap in the direction the
  stick has left; and a step that arrives more than one interval late re-bases
  on `now` instead of firing a burst.
- **The clock.** A gamepad reports only on change, so a stick *held* over is
  silence on the wire and the taps cannot come from frames. They come from the
  4 ms conditional deadline the Elite backend already added
  (`sticks::StickDrive`), which now takes the shuttle's "still running" answer
  alongside the cursor's and scroll's — and, unlike them, is not gated on
  `[sticks] enabled`, because that switch is the pointer's and the scrub has its
  own.
- **One suppression.** While the shuttle is live, `guide+lstick_left` /
  `guide+lstick_right` do not fire (`GestureEngine::set_shuttle_left`). The
  sample config binds those to a focus move, and without this every scrub would
  also rearrange the windows behind the text. Vertical flicks are untouched.
- **A sheet row that says which.** `Scrub the caret · jog (pad)` on a layout
  with trackpads, `· shuttle (stick)` on one without, re-aimed by
  `Sheet::set_layout` from the published layout id — the same mechanism the
  ambient cursor and scroll rows use.

### Not built, and why

- **No momentum.** §2's note on Steam's "Spin Friction" applies double here:
  a flicked wheel that keeps ticking is overshoot, and a shuttle's whole promise
  is that centring the stick stops the caret at once.
- **No vertical shuttle**, per above.
- **No haptics in practice.** The pulses are fired (`Haptic::Scroll` /
  `ScrubWord`), but every padless source so far has rumble motors rather than
  the puck's actuators, and `HapticCtx::fire` drops them at the one gate that
  already exists for that.
