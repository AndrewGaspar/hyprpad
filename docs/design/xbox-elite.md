# Design: a second input backend — driving hyprpad from an Xbox Elite Series 2

*Implements phase 1 of `docs/research/xbox-elite.md`. Code: `src/evdev.rs`
(the backend), `src/sticks.rs` (rate control), `src/report.rs`
(`Source`), plus the loop plumbing in `src/run.rs`, the `[sticks]` / `[device]`
sections in `src/config.rs` + `src/lua_config.rs`, `"sources"` / `"layout"` in
`src/status.rs`, and `shell/hyprpad.cheatsheet/layouts/xbox-elite-2.json`.
Phase 2 is §8.*

## What it is

hyprpad reads the 2026 Steam Controller controller over hidraw. This adds a **second
source**: any ordinary Linux gamepad on `/dev/input/event*`, read over evdev,
decoded into the *same* [`report::Frame`], and fed to the *same* loop. The Xbox
Elite Series 2 is the pad it was built for — the owner has one — but nothing
below is Elite-specific except the paddle handling and the name in the log.

Both controllers may be connected at once. Everything above the decoder — the
gesture engine, the config, the mode engine, the bare-button router, the OSK
bridge, the virtual gamepad, the cheat sheet — is unchanged and unaware.

**A config written for the controller works on the Elite with no edit**, minus the
bindings that name a trackpad. `guide+r4`, `dpad_up`, `guide+stick_right`,
`l2`/`r2` and the whole chord grammar spell the same physical controls on both.

## 1. Discovery — by capability, never by the joystick tag

`src/evdev.rs`, `gamepad_nodes()`.

The Bluetooth Elite's HID descriptor carries a full keyboard collection, so
systemd's *joystick un-detection* classifies it as a **keyboard**: `udevadm
info` on its node reports `ID_INPUT_KEYBOARD=1` and **no `ID_INPUT_JOYSTICK`**
(research §1.3.4, verified on the owner's machine). Anything filtering on the
joystick tag — SDL's udev path included — does not see this pad at all.

So discovery reads the sysfs capability bitmaps and asks the only question that
matters:

| requirement | why |
|---|---|
| `EV_ABS` and `EV_KEY` | it has axes and buttons |
| `ABS_X` + `ABS_Y` | a left stick |
| (`ABS_RX` + `ABS_RY`) **or** (`ABS_Z` + `ABS_RZ`) | a right stick — see §2 |
| 11 of the gamepad cluster, `BTN_SOUTH`..`BTN_THUMBR` | it is a *gamepad*, not a flight yoke |

`BTN_C` (0x132) and `BTN_Z` (0x135) are deliberately **not** required: the BLE
Elite sets them (HID buttons 3 and 6, never pressed) but `xpad` does not, and
requiring them would reject the wired pad — the one transport that gives four
distinct paddles out of the box.

**The loop-back guard is a path rule, not a vendor rule.** hyprpad's own output
pad wears `045e:028e` on purpose — the wired Xbox 360 id every engine knows —
and so does a real 360 pad somebody might want to use; Steam's virtual pads
have the same problem. What they all share, and no physical device has, is a
canonical sysfs path under `/sys/devices/virtual/input/`. Note that a
Bluetooth pad lives under `/sys/devices/virtual/misc/uhid/…` — *virtual/misc*,
not *virtual/input* — so the rule does not catch it.

Hotplug is the controller's own periodic rescan (`SCAN_INTERVAL`, 1.5 s, the same
cadence as `RECONNECT_SCAN_INTERVAL`) plus an immediate re-scan on any read
error. No udev monitor, no inotify, no new dependency. The whole state machine
— scan, open, grab, read, release, rescan — lives in one thread in
`evdev::watch`, which is why `run.rs` grew three match arms and not a second
supervisor.

## 2. The trap: one controller, two axis layouts

The **same Elite reports different axis codes depending on how it is
attached.** This was not in the research doc and is the single most important
thing in this design:

| | left stick | right stick | triggers |
|---|---|---|---|
| USB, `xpad` | `ABS_X`/`ABS_Y` | `ABS_RX`/`ABS_RY` | `ABS_Z` / `ABS_RZ` |
| Bluetooth, `hid-microsoft` | `ABS_X`/`ABS_Y` | **`ABS_Z`/`ABS_RZ`** | **`ABS_BRAKE` / `ABS_GAS`** |

Over BLE the descriptor spells the right stick as GD `Z`/`Rz` and the triggers
as Simulation `Brake`/`Accelerator`, which generic `hid-input` maps to
`ABS_Z`/`ABS_RZ` and `ABS_BRAKE`/`ABS_GAS` (research §1.3.2). The live
`capabilities/abs` on the owner's pad is `30627` — X, Y, Z, RZ, GAS, BRAKE,
HAT0X, HAT0Y — with **no `ABS_RX` at all**.

A backend keyed on `ABS_RX`/`ABS_RY` would not have *found* the Bluetooth pad,
let alone read its right stick. `AxisMap::detect` picks the layout from the
capability bitmap once, at adoption, and everything downstream reads *roles*
rather than codes.

Ranges come from the device (`EVIOCGABS`) rather than a table of magic numbers,
so a pad whose sticks are `0..65535` or whose triggers are `0..255` normalises
correctly with nothing to configure.

## 3. Mapping table

`src/evdev.rs`, `FrameBuilder::button_for` and `FrameBuilder::apply_abs`.

| Elite control | evdev | `report::Button` / `Frame` field | config name |
|---|---|---|---|
| A / B / X / Y | `BTN_SOUTH` / `BTN_EAST` / `0x133` / `0x134` | `A`/`B`/`X`/`Y` | `a b x y` |
| D-pad | `ABS_HAT0X/Y`, or `BTN_DPAD_*` | four `Dpad*` bits; a diagonal sets two | `dpad_*` |
| LB / RB | `BTN_TL` / `BTN_TR` | `BumperL1` / `BumperR1` | `l1 r1` |
| LT / RT analog | `ABS_Z`/`ABS_RZ` **or** `ABS_BRAKE`/`ABS_GAS` | `l2`/`r2`, scaled to the controller's `0..32767` | — |
| LT / RT full pull | *synthesised* | `TriggerL2Full` / `TriggerR2Full` | `l2 r2` |
| View / Menu | `BTN_SELECT` / `BTN_START` | `View` / `Menu` | `view menu` |
| L3 / R3 | `BTN_THUMBL` / `BTN_THUMBR` | `L3` / `R3` | `l3 r3` |
| **Xbox button** | `BTN_MODE` | `Steam` — the guide | `guide+…` |
| **P1** upper-right | `BTN_GRIPR` / `BTN_TRIGGER_HAPPY5` | `GripR4` | `r4` |
| **P2** lower-right | `BTN_GRIPR2` / `HAPPY6` | `GripR5` | `r5` |
| **P3** upper-left | `BTN_GRIPL` / `HAPPY7` | `GripL4` | `l4` |
| **P4** lower-left | `BTN_GRIPL2` / `HAPPY8` | `GripL5` | `l5` |
| sticks | `ABS_X/Y` + §2's right pair | `left_stick` / `right_stick`, `±32767`, **+Y up** | `guide+stick_*` |
| Profile button | `KEY_RECORD` | *unmapped* — see §7 | — |
| trackpads, Quick Access, capacitive | — | always zero / false | — |

Three details worth naming:

* **`BTN_X` is `BTN_NORTH` (0x133) and `BTN_Y` is `BTN_WEST` (0x134).** The
  Linux Gamepad Specification would put X at WEST for an Xbox pad, but neither
  path this backend reads is spec-compliant: `xpad`'s table is literally
  `{BTN_A, BTN_B, BTN_X, BTN_Y}`, and over Bluetooth `hid-input` maps HID
  button 4 → 0x133 and button 5 → 0x134, which the descriptor labels X and Y in
  that order. The two agree, so the legacy reading is the correct one.
* **Y is inverted on the way in.** evdev's `+Y` is *down*; the controller's is *up*,
  and the whole daemon — flicks, the OSK wire, the virtual pad — is written to
  the controller's.
* **The full-pull bit is a Schmitt trigger**, down at 85 % of travel and up at
  70 %. The controller's is a firmware click; an Xbox trigger has none. Hysteresis
  is not a nicety here: the gesture engine's chords are edge-triggered, so a
  trigger resting on a bare threshold would re-chord continuously.

## 4. Sticks in place of pads

`src/sticks.rs`.

The controller's cursor is **position** control: `drive_cursor` differences the
smoothed absolute pad coordinate, and the One Euro filter exists because
differencing amplifies sensor noise. A stick is **rate** control — the
deflection *is* a velocity command, it returns to a mechanical centre, and the
kernel has already applied the axis's `fuzz`/`flat`. There is nothing to
difference and therefore **no One Euro filter**; running one would only add lag.

```text
r  = |(x, y)|                                    radial deflection, 0..1
d  = clamp((r - inner) / (outer - inner), 0, 1)  deadzone rescale
g  = d^curve                                     response curve
v  = max * g                                     output units per second
(vx, vy) = v * (x, y) / r                        the stick's exact direction
```

Radial, not per-axis: a per-axis deadzone lets the diagonals engage at a smaller
push than the cardinals, and the cursor drifts diagonally. Sub-unit remainders
carry between steps, so a slow deliberate nudge still moves the pointer.

| | drives | default top speed |
|---|---|---|
| right stick | the desktop pointer | 1500 px/s |
| left stick | scrolling | 180 `wl_pointer.axis` units/s = 12 notches/s |
| both sticks | the OSK's two cursors | 2.4 normalised units/s ≈ 1.2 keyboard widths/s |

Deadzone 0.12, outer 0.95, curve 2.0 (quadratic — a gentler low end, which is
the precision a desktop pointer wants), and a 15 ms EMA on the velocity. All of
it is `[sticks]` / `h.sticks`; §6.

**The guards are the existing ones.** A stick cursor is a cursor, so it obeys
`h.cursor { only_in = … }` and `guide_in` through the same `cursor_active`
decision the right pad goes through, and stick scrolling obeys `h.scroll`'s
guard and the guide layer exactly as the left pad does. There is no second
permission model to keep in step with the first.

## 5. The clock — a conditional deadline, not a tick

A gamepad reports **only on change**. Hold a stick at 60 % and the device goes
silent, so an integrator driven by frames would move the cursor once and stop.
It needs its own clock — but not a free-running one.

The daemon's loop already blocks on its input channel with a *deadline*: the
reconnect scan, the process rescan, a transient mode's timer. The stick
integrators are a **fourth deadline on the same mechanism**:

* `StickDrive::deadline()` is `Some(when)` **only** while a stick is outside its
  deadzone, or a velocity is still decaying through the EMA after one was
  released. Centred and settled, it is `None`;
* `None` contributes nothing to the loop's `wake` array, so the loop blocks
  indefinitely — exactly as it does today with the controller asleep. **An idle
  controller costs zero wakeups.** `docs/09`'s "hyprpad must never busy-poll"
  holds unchanged;
* the deadline is checked at the **top** of the loop, beside the rescan and the
  transient timer, and not only on a receive *timeout*. That is not tidiness: a
  moving stick produces a busy report stream, and a deadline honoured only on a
  timeout would then never fire at all;
* a frame from a source *with* pads parks the integrators outright. The controller's
  sticks are for guide flicks and its pads drive the cursor; a deflected controller
  stick must never hold the deadline open;
* the step is clamped to 50 ms, so the first step after a long idle gap cannot
  fling the cursor across the screen.

Per wakeup the cost is microseconds of arithmetic on two axis pairs.

## 6. Config

Nothing is required: a connected pad is adopted with no config at all. Both
sections are optional and both front-ends spell the same thing.

```lua
h.sticks {
  tick_ms = 4,                      -- the integration step, while a stick is deflected
  cursor = { deadzone = 0.12, outer = 0.95, curve = 2.0,
             max_px_s = 1500, smoothing_ms = 15 },
  scroll = { max_units_s = 180 },   -- 15 units = one wheel notch
  osk    = { max_units_s = 2.4 },
}
h.device {
  evdev = true,                     -- run the gamepad backend at all
  grab  = true,                     -- EVIOCGRAB an adopted pad
}
```

```toml
[sticks]
tick_ms = 4
cursor_deadzone = 0.12
cursor_curve = 2.0
cursor_max_px_s = 1500
scroll_max_units_s = 180
osk_max_units_s = 2.4

[device]
evdev = true
grab = true
```

The TOML parser has no nesting, so a sub-table is a `<group>_<knob>` prefix;
both dialects go through `config::set_stick_knob`, so they cannot drift, and a
test asserts the two produce an identical `SticksConfig`.

`grab` is read when a pad is *adopted*, so a reload changing it takes effect on
the next reconnect rather than by tearing the read out from under a live device.

## 7. `EVIOCGRAB`, and what the owner must do

**On by default, and the default matters.** Two reasons, in order:

1. the Bluetooth node is a *keyboard* to libinput (§1), and the Elite's Profile
   button is `KEY_RECORD` — ungrabbed, pressing it types into whatever Hyprland
   has focused. Its paddles, trigger locks and profile slot all share one
   `KEY_UNKNOWN` that toggles as they change;
2. for the wired node a grab hides the physical pad from every other evdev
   consumer, so a game sees only hyprpad's virtual `045e:028e` — the docs/06
   Tier-1 shape with no masking needed.

A refused grab is logged and survived, not fatal.

It does **nothing** to Steam over Bluetooth, which reads the pad's *hidraw* node
instead. Either turn off Steam → Settings → Controller → "Xbox controller
support", or extend `scripts/steam-masked` (which matches only `28DE:1304`
today) to cover `045E:0B22`.

### Paddles: profile slot 0, and one of two transports

The four paddles are on the wire in every mode, but reach **evdev** only two
ways, and hyprpad accepts both code sets for good (a future `udev-hid-bpf` may
switch to `BTN_GRIP*`; a kernel older than 6.17 still speaks
`BTN_TRIGGER_HAPPY*`):

| transport | what the owner does | codes |
|---|---|---|
| USB-C cable | plug it in | `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2` (`xpad` ≥ 6.17) |
| Bluetooth | `pacman -S udev-hid-bpf`, then re-pair/reconnect | `BTN_TRIGGER_HAPPY5..8` |
| Bluetooth, stock | — | **none** — all four fold into one `KEY_UNKNOWN` |

**And the controller must be in profile slot 0** — press the Profile button
until no profile LED is lit. In slots 1–3 the firmware re-emits each paddle as
whatever face button the Xbox Accessories app mapped it to, and every consumer
(`xpad`, xpadneo, SDL) mutes the raw nibble as a result. Phase 1 cannot read the
slot byte, so it cannot warn; the log line at adoption says which code set (if
either) the node advertises, which is the next best thing.

## 8. Coexistence — last-active source wins

There is one pointer, one keyboard, one OSK and one mode engine, so two
controllers cannot both drive.

* a frame from the source that is not active is **dropped** unless
  `Frame::is_neutral()` says the user actually did something — a press, a
  click, a pad touch, a trigger off zero, or a stick past the gesture engine's
  centre deadzone. A change-driven pad's re-sends therefore never steal the
  cursor;
* **the capacitive flags are excluded from that test, and this is the subtle
  bit.** `Cap0..3` fire on hand *contact* with the controller's grips, not on an
  action. Counting them would make every frame from a controller a resting hand
  happens to be on "deliberate", and the two sources would swap the cursor back
  and forth at the report rate. Proximity is not intent;
* a frame that *does* switches the active source, and switching runs the same
  release the disconnect path runs: the gesture engine and `prev_frame` are
  reset, every held key, click and chord is let go, the virtual pad is
  neutralised, and the rate integrators are dropped. Nothing is left held by the
  source that lost the device;
* either controller may leave without the other. `Input::ReadersEnded` is the
  controller going; `Input::EvdevGone` is the gamepad going; each releases only what
  it was holding.

`status.json` gains `"sources"` — every controller the daemon can hear, e.g.
`["controller", "elite"]` — and `"layout"`, the cheat-sheet id of the one that is
*driving*.

## 9. Haptics

`HapticCtx::fire` is the **one place** a source without actuators is handled,
through the pure `haptic_for(source, cfg, what)`. The controller has an actuator
behind each trackpad and its pulses are 200–600 µs; an Xbox pad has two rumble
motors, which `ff-memless` runs at jiffy granularity and which need tens of
milliseconds to spin up. There is no honest way to play a 250 Hz texture on one,
so every `Feel` is dropped there rather than at each of the dozen call sites —
which is exactly what lets `drive_scroll`, `route_osk`, `drive_buttons` and the
rest stay source-agnostic.

Game rumble (`drive_rumble`) is untouched: it already only reaches the controller.

## 10. The on-screen keyboard — phase 1, option (i)

While the keyboard is up it owns **both sticks**, as it owns both pads on the
controller. Each stick integrates into a per-hand position in the OSK's own
`[-1, 1]` box, and the daemon sends it over the **existing** `cursor L|R` wire.

**The OSK child is completely unchanged.** Commits come from the same
`[osk_buttons]` map every config already has, and the built-in Deck map already
suits this pad almost exactly: L2 Shift, R2 Enter, Y Space, X Backspace,
B/Menu dismiss are all buttons an Elite has. The **only** thing missing is the
commit, which the built-ins put on the two pad *clicks*. Give it a home:

```lua
h.osk_button("a",  h.osk "commit")   -- commits under the RIGHT cursor
h.osk_button("l3", h.osk "commit")   -- left stick click -> the left cursor
h.osk_button("r3", h.osk "commit")   -- right stick click -> the right cursor
```

```toml
[osk_buttons]
a  = "osk commit"
l3 = "osk commit"
r3 = "osk commit"
```

The stick clicks are the natural home because `commit_pad` reads the cursor
under the **same hand** as the button, so each thumb commits its own cursor —
which is what the split layout was designed for. `a` reads the right cursor,
like the rest of the face cluster.

Phase 2's snap navigation (a focus model in `osk/`) is a change to the child,
not to this.

## 11. The cheat sheet

`layouts/xbox-elite-2.json` is a new file and **no QML at all**. The daemon is
the only process that knows which pad is in the user's hands, so it publishes
`"layout"` in `status.json`; `scripts/hyprpad-cheatsheet` reads that one string
into the summon payload `Panel.qml` already accepts, and `hyprpad bindings
--json` carries the same id.

On a padless layout the sheet re-aims the two rows that name a physical pad: the
ambient cursor and scroll point at the right and left **sticks** and carry the
stick's own top speed rather than the pad's `sens`, and the caret scrub is
dropped — a jog wheel needs a surface to circle. Everything that is a *binding*
is identical, which is the point of one vocabulary.

The drawing is hyprpad's own schematic (`art/xbox-elite-2.svg`): Steam ships an
Elite diagram but it is a PNG, and the widget themes a drawing by recolouring
SVG strokes. No new glyphs were needed — see `art/LICENSES.md`.

## 12. What phase 2 holds

| | why it waits |
|---|---|
| **hidraw sidecar for `045e:0b22`** | four paddles over Bluetooth with *nothing installed*, plus the profile slot (byte 17) so the daemon can mute them itself and warn. A `Frame::decode_xbox_ble` beside `Frame::decode` over the pad's own hidraw node; the evdev fd then stays open for the grab alone. The hook is `evdev::PADDLE_HOOK`. |
| **Rumble taps** | `FF_RUMBLE` via `EVIOCSFF` on the grabbed fd (wired), or report 3 on the hidraw node (BT). Only `Gesture` and `Commit` are worth mapping — 30–40 ms at ~30 % — and `Crossing`, `Scroll`, `Button` and `CursorMove` stay dropped. The hook is the one `if` in `haptic_for`; opening the evdev node `O_RDWR` is the other line. |
| **OSK snap navigation** | option (ii): a focus model in `osk/`, `focus <dir>` and `press` on the control wire, `MAINTAIN_X` column memory. It changes what the OSK cheat-sheet tab says, so it lands with the tab. |
| **inotify hotplug** | the 1.5 s rescan is fine; `inotify` on `/dev/input` is the refinement. A udev monitor is not worth the dependency. |
| **DualSense / Switch Pro** | the backend is already generic. What is missing is each pad's own paddle/extra-button story and a layout file. |

## 13. What is deliberately not done

* **`hyprpad monitor --evdev`.** The research doc's phase 0 suggests it; the
  adoption log line and `hyprpad bindings --json` covered what was needed here.
* **A `labels` map in the layout.** The Elite's silkscreen says P1–P4 where
  hyprpad says R4/R5/L4/L5. Control labels are Rust-side (`button_label`), so
  showing the pad's own names would be a widget change; the layout's glyph
  `text` fallbacks carry them and the mapping is in §3.
* **Reading `ABS_PROFILE`.** xpadneo exports the profile slot; stock `xpad` and
  `hid-microsoft` do not, and phase 2's sidecar reads it from the wire anyway.
