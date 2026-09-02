# Design: the puck's power — the firmware timer, and a deliberate off switch

*Implements options A + B of `docs/research/guide-hold-poweroff.md` §4. Code:
`src/lizard.rs` (the settings write, the read round trip, the `0x9F` command),
`src/config.rs` + `src/lua_config.rs` (the `[daemon]` knobs and
`h.controller_off()`), `src/run.rs` (the `execute()` arm and SIGUSR1),
`src/main.rs` (`hyprpad off`, `hyprpad puck-settings`),
`shell/hyprpad.status/Widget.qml` (the right-click).*

## What it is

The complaint: *"I'll be holding the guide button while being indecisive and it
turns off the controller rather than waiting for me."* The research note pins
the culprit on the **firmware** — it fires with no Steam process, with
`hid-steam` unbound, and hyprpad sends no `0x9F` anywhere — and finds Valve's
own register for it: `SETTING_STEAMBUTTON_POWEROFF_TIME`, setting 25.

So this feature is two halves that only make sense together:

1. **Lengthen or disable the firmware's timer**, so the hold stops firing while
   you deliberate — `[daemon] steam_button_poweroff`, written in the same `0x87`
   frame `src/lizard.rs` already re-sends every 30 s. `sleep_inactivity_timeout`
   (setting 50) is the same treatment for the idle sleep, and is what still
   powers the pad down passively once the hold is gone.
2. **A deliberate off switch**, because a controller you cannot turn off is not
   an improvement — `Action::ControllerOff` (`0x9F ID_TURN_OFF_CONTROLLER`) on a
   chord, on `hyprpad off`, and on a right-click of the bar widget.

And, because half of the above is a guess, a **read path**: `hyprpad
puck-settings` asks the firmware what it actually thinks.

## What is UNVERIFIED

Everything about the *meaning* of the two settings. This is worth stating
plainly because the knobs are shipped defaulting to "write nothing", and that is
why.

| | status |
|---|---|
| setting numbers 25 / 50, command `0x9F`, the `0x87` frame layout | **verified** — SDL's `controller_constants.h`, the kernel's `hid-steam.c`, and the frame `src/lizard.rs` already sends successfully |
| the `"off!"` payload on `0x9F` | sc-controller sends exactly `9f 04 6f 66 66 21`; whether **Triton requires** the magic is unverified. Fallback to try: a bare `01 9f 00` |
| the **unit** of setting 25 (ms? 100 ms ticks? seconds?) | unverified — nothing public writes it |
| whether `0` means "never" or "no delay" for setting 25 | unverified, and the reason `"off"` writes `0xFFFF` instead of `0` |
| whether Triton honours setting 25 **at all** | unverified — the enum is shared with the 2015 firmware; the implementation may not be |
| whether either setting persists across a power cycle | unverified (the 30 s re-send covers it either way, with a ≤30 s window after a wake) |
| the unit of setting 50 | sc-controller documents it as **seconds** on the 2015 firmware, default 600; unverified here |
| what the length byte of a *request* means (`[89][n][ids…]` vs. a reply length) | unverified — see the note in `src/lizard.rs`; it is the first thing to suspect if a read comes back empty |

## The verify recipe

Run this **when the controller is not otherwise in use**: step 4 deliberately
powers it off, and step 2 may make a hold do so.

1. **Read what the firmware says now.** Nothing is written; this is safe at any
   time. The controller must be awake (press the Steam button first).

   ```
   hyprpad puck-settings 25 50
   ```

   Read the three columns together. `max` is the only clue to the *scale*: a
   maximum of `65535` says nothing, but a maximum like `3000` alongside a
   default of `300` argues for 100 ms ticks and a 30 s ceiling. A `-` in
   `current` means the firmware left the setting out of its answer entirely —
   which is itself the answer for "is 25 even stored on Triton".

   If every node fails with a STALL, the controller is asleep. If the reply
   parses but is empty, suspect the request length byte (above).

2. **Write a value and read it back.** Start with something you can *time*, not
   with `"off"` — a value that is obviously longer or obviously shorter than
   today's hold tells you the unit far faster than one that disables it.

   ```lua
   h.daemon { steam_button_poweroff = 3000 }
   ```

   ```
   hyprpad reload
   hyprpad puck-settings 25
   ```

   `current` should now read back `3000`. If it does not, the firmware rejected
   or clamped it — compare against `max`. Note that the write rides in the
   lizard-disable frame, so it only goes out when `own_lizard = true`, and the
   daemon logs the pairs it sent (`firmware power settings written with it: …`).

3. **Time a hold with a stopwatch.** Hold the Steam button and count until the
   controller powers off. Cross-check with `hyprpad monitor`: the last `0x42`
   frame's timestamp before the stream stops is the moment it went. Compare with
   the baseline you measured before writing anything.

   * the hold got **longer** in proportion to the value → the setting is honoured
     and you have the unit;
   * the hold got **shorter** → the unit is smaller than you assumed;
   * **no change**, but step 2 read the value back → the setting is stored and
     ignored: Triton does not honour it, and option D of the research note (a
     rumble warning one second before the firmware fires) is the fallback.

4. **Then, and only then, try `"off"`.**

   ```lua
   h.daemon { steam_button_poweroff = "off" }
   ```

   `"off"` writes `0xFFFF`, not `0` — see the README for why. If `max` from step
   1 was smaller than `0xFFFF`, write that maximum as an integer instead.

5. **Check persistence.** Power the controller off (`hyprpad off`), press the
   Steam button to wake it, and read again *within* 30 s and then after:

   ```
   hyprpad off
   # …press Steam to wake…
   hyprpad puck-settings 25
   ```

   A value that survived the cycle is stored in the firmware; one that comes
   back as the default is re-applied by the 30 s re-send, which is fine — it
   just means a ≤30 s window after every wake in which the old hold is live.

6. **Confirm the daemon recovered.** Lizard mode should be re-disabled within
   30 s of the wake (the daemon logs it), and the bar widget should reappear.

## Design notes

**Why the power settings ride in the lizard frame.** They are `(setting, u16)`
pairs in exactly the report `own_lizard_loop` already builds, sends to every
puck node, retries on STALL and re-sends every 30 s. Appending to it inherits
all of that — including recovery after a reconnect — for the cost of three bytes
per setting. The cost is that the knobs only apply when hyprpad owns lizard
mode; that is stated in the config docs.

**Why the settings live in a process-wide cell.** `disable_lizard_mode()` is
called from four places in `src/run.rs` (startup, reconnect, reload, and the
ownership loop's own timer) and the loop runs on its own thread. Threading a
config value through all four would have meant four signature changes for a
value that is the same everywhere; `lizard::set_power_settings` is called twice
— once at startup, once per reload — and the loop reads it.

**Why the restore-on-exit does not revert them.** There is nothing to revert
*to*: the firmware's defaults for 25 and 50 are not published, so writing a
guess back on exit would be worse than leaving the owner's chosen value. Step 1
of the recipe above is how to recover the real default
(`0x8C ID_GET_SETTINGS_DEFAULTS`) if it is ever wanted.

**Why `hyprpad off` goes through the daemon.** SIGUSR1 to the pid in the
pidfile, exactly as `hyprpad reload` sends SIGHUP. The daemon already holds the
puck's descriptors — after `packaging/udev/72-hyprpad-puck.rules` it is the only
unprivileged process that can — and one writer to the device is the invariant
the haptics and relay paths also rest on. A CLI that opened the puck behind the
daemon's back would break both.

**Why the read path is a trait.** `FeatureDevice` has two methods and one real
implementation. The request builder and the reply parser either side of it are
pure functions over bytes, so the whole round trip is unit-tested against a fake
device — which is the only way to develop this at all while the owner is holding
the controller, since the command next door to the ones being tested powers it
off.

**Why the chord is `guide+quickaccess`.** The gesture engine's chords are
single-button (`GuideChord(b)` per edge), so "guide + two buttons" is not
available without engine work. Of the buttons free in the owner's config, the
quick-access "…" button is the one furthest from anything a thumb does while the
guide is held. `guide+l5` (a back paddle) is the documented alternative. Steam's
own power-off chord is `guide+Y`, which this project deliberately keeps as the
on-screen-keyboard toggle.
