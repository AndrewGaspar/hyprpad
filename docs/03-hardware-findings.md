# 03 — Hardware findings

Empirical probing of a 2026 Steam Controller on the target machine.
All observations dated **2026-08-30**.

| | |
|---|---|
| Host | Framework Laptop 16, Omarchy (Arch) |
| Kernel | 7.1.9-arch1-2 |
| Compositor | Hyprland 0.56.2 (`67200a8383`) |
| Steam | client build 1785799196, running, `-silent` |
| Controller | 2026 Steam Controller via wireless puck |

Reproduce with [`tools/sc2-capture.py`](../tools/sc2-capture.py).

## Device topology

The wireless puck enumerates as a single USB device, `28de:1304`
("Valve Software Steam Controller Puck", serial `FXB99614031B4`), exposing
**five HID interfaces**:

| Interface | `hidraw` | evdev nodes | Role |
|---|---|---|---|
| if02 | `hidraw7` | `event20` (mouse), `event21` (kbd) | controller slot 1 |
| if03 | `hidraw8` | `event22`, `event23` | controller slot 2 |
| if04 | `hidraw9` | `event24`, `event25` | controller slot 3 |
| if05 | `hidraw10` | `event26`, `event27` | controller slot 4 |
| if06 | `hidraw11` | — | dongle control (no input) |

Four paired-controller slots plus a control endpoint. Only the slot with a
controller actually connected produces input; the other three are silent.

Each input slot's evdev pair is the **lizard-mode** keyboard and mouse, not a
gamepad. There is no gamepad evdev node at all on this kernel.

### The driver is `hid-generic`

```
$ cat /sys/class/hidraw/hidraw7/device/uevent
DRIVER=hid-generic
HID_ID=0003:000028DE:00001304
HID_NAME=Valve Software Steam Controller Puck
```

`hid-steam` on 7.1.9 claims only `28de:1205` (Steam Deck), `28de:1142`
(gen-1 wireless dongle) and `28de:1102` (gen-1 wired). `1304` is not in the
table.

Linux **7.3** merges the first `hid-steam` support for the 2026 controller
(work by Vicki Pfau), bringing it to roughly gen-1 parity. Haptics and finer
touch reporting are expected to land later. Until then, on this machine, the
controller is either in lizard mode or being driven entirely by Steam over
`hidraw`.

### Who holds what

```
1423 Hyprland -> /dev/input/event20 … event27      (lizard-mode kbd/mouse)
3853 steam    -> /dev/hidraw7,8,9,10,11
3853 steam    -> /dev/uinput
```

Steam holds **all five** `hidraw` nodes and `/dev/uinput`, and publishes a
virtual pad:

```
I: Bus=0003 Vendor=28de Product=11ff Version=0001
N: Name="Microsoft X-Box 360 pad 0"
S: Sysfs=/devices/virtual/input/input49
H: Handlers=event28 js0
```

Hyprland, meanwhile, has the four lizard-mode mouse/keyboard pairs open and
lists them in `hyprctl devices` as `valve-software-steam-controller-puck-mouse`
&c. They are silent, because Steam has switched lizard mode off.

## The central finding: concurrent `hidraw` access

**With Steam holding `hidraw7` open, a second process opened the same node and
received the full input stream.** No error, no interference, no observable effect
on Steam.

```
$ timeout 2 od -An -tx1 -w54 -v /dev/hidraw7
 42 3a 00 00 00 00 00 00 00 00 12 02 bc 01 58 fe 4b 01 …
 42 3b 00 00 00 00 00 00 00 00 e7 01 a3 01 87 fe 4b 01 …
 42 3c 00 00 00 00 00 00 00 00 bd 01 a3 01 6f fe 04 01 …
```

This is a property of the kernel's `hidraw` implementation, not a quirk of this
device: `hidraw` has no exclusive-access mode, and each open file description
gets its own report queue. See
[02 — Background](02-background-linux-input.md#exclusivity-the-property-the-whole-design-hinges-on).

**Everything in [06 — Recommendation](06-recommendation.md) follows from this.**

## Report `0x42` — the main input report

54 bytes (1 ID + 53 payload), matching the report descriptor's
`Usage Page 0xFF00, Report ID 0x42, Report Count 0x35`.

Measured over a 210-second capture, 55 205 reports:

| | |
|---|---|
| Rate | **263 Hz** |
| Median inter-report gap | 4.00 ms |
| p95 gap | 4.20 ms |

A 250 Hz nominal poll with tight jitter — ample for chord and gesture detection;
a 40 ms gesture window is ten samples.

### Layout

| Bytes | Content |
|---|---|
| `b0` | report ID, `0x42` |
| `b1`–`b4` | frame counter (little-endian; `b1` increments every report) |
| `b2`–`b5` | **button bitfields** — see below |
| `b10`–`b17` | motion (four `int16` values, changing continuously at rest — gyro/orientation) |
| `b18`–`b29` | additional motion / touch, active during trackpad use |
| `b30`–`b45` | stick and trackpad positions, capacitive data |

> **Note.** `b1`–`b4` serve double duty: `b1` is a fast-incrementing counter,
> while `b2`–`b5` carry buttons. The counter appears to be narrower than the
> four bytes initially suggest. Treat the exact counter width as unconfirmed.

### Confirmed button bits

Established by a targeted capture: three isolated presses of a single control,
in isolation, producing exactly three rising edges on one bit.

| Control | Byte | Bit | Mask | Confidence |
|---|---|---|---|---|
| **Steam / Guide** | `b4` | 0 | `0x01` | **Confirmed** — 3/3 isolated presses at 12.32 s, 14.98 s, 18.21 s |
| **Quick Access (`…`)** | `b2` | 4 | `0x10` | **Confirmed** — 3/3 isolated presses at 27.16 s, 27.73 s, 28.36 s |
| A, B, X, Y | `b2` | 0,1,2,3 | `0x01`–`0x08` | High — pressed in order, twice, in both capture passes |
| D-pad (4 directions) | `b3` | 2,3,4,5 | — | High as a group; individual direction↔bit mapping **not** pinned down |
| Bumpers | `b4`.3 and `b3`.1 | | | Medium — consistent position in both passes |
| Grips / Start / Select | `b2`.6,`b2`.7, `b3`.0,`b3`.6, `b4`.1,`b4`.2 | | | Low individually — a cluster of seven unique bits in the right time window, not separated |
| Capacitive / grip sense | `b4`.4, `b5`.0, `b5`.4, `b5`.5 | | | High as a class — these toggle on hand contact with no button press, and bracket deliberate presses |

The unconfirmed rows are honest gaps, not guesses to build on. Pinning them down
is a matter of running `sc2-capture.py capture` once per control; see
[07 — Open questions](07-open-questions.md).

### Guide-button timing

Directly relevant to tap-versus-hold discrimination:

```
STEAM  held    88 ms
STEAM  held   128 ms
STEAM  held   128 ms
STEAM  held  2298 ms      ← deliberate long hold
QAM    held   150 ms
QAM    held   186 ms
QAM    held   196 ms
```

Deliberate taps sit under ~200 ms; an intentional hold is an order of magnitude
longer. A hold threshold in the 250–350 ms range separates them comfortably.

### The full gesture is observable

During the deliberate guide hold, 1 226 consecutive reports carried
`b4 & 0x01` set **while** stick/trackpad bytes `b34`–`b44` moved independently.
The complete *hold-guide-and-flick-the-stick* gesture is therefore recoverable
from the passive stream alone — modifier state and analog axes in the same
frames.

## Steam's reaction to the guide button

Established by direct observation (2026-08-30), and decisive for the
architecture: **Steam acts on guide _release_, never on press.** Three branches:

| Condition | Steam's reaction on release |
|---|---|
| Short hold, Steam window **not** focused | Steam window takes focus |
| Short hold, Steam window **already** focused | Launches Steam |
| **Hold longer than ~3 s** | **Nothing at all** |

Three consequences, in ascending order of importance.

1. **There is no race.** A passive daemon has the entire duration of the hold to
   recognise and act on a gesture before Steam does anything. Steam's reaction is
   a *trailing side effect*, not a competitor for the same event.
2. **A long hold is invisible to Steam.** Anything held past ~3 s is free of
   Steam-side consequences entirely.
3. **The side effect is a focus steal**, which is precisely the class of thing a
   Wayland compositor can suppress declaratively. Hyprland 0.56.2 accepts
   `suppressevent activatefocus` as a window rule (token verified present in the
   binary), so branch 1 is neutralisable in configuration:

   ```
   windowrule = suppressevent activatefocus, match:class steam
   ```

   Branch 2 only fires when the Steam window is already focused — which is
   exactly when a window-manager gesture is not wanted anyway.

This closes what was the recommended architecture's main weakness. See
[06 — Recommendation](06-recommendation.md#the-guide-button-is-not-actually-a-problem).

## Other reports

| ID | Length | Period | Interpretation |
|---|---|---|---|
| `0x42` | 54 | 3.8 ms | main input |
| `0x7b` | 13 | 0.5 s | RF/battery telemetry — values track link quality and drain |
| `0x43` | 15 | 3.5 s | periodic status beacon; payload near-constant |
| `0x44` | 6 | bursts | `44 03 02 00 00 00` / `44 04 02 00 00 00`, always in pairs, correlated with button activity — most likely a haptic acknowledgement |

Only `0x42` matters for input.

## Steam's handling of this controller is broken today

Reproduced on this machine, on every Steam launch:

```
[2026-08-30 21:14:53] Work item took a long time … 23CExitLizardModeWorkItem(1)
[2026-08-30 21:14:53] Work item took a long time … 23CExitLizardModeWorkItem(2)
[2026-08-30 21:14:53] Work item took a long time … 23CExitLizardModeWorkItem(3)
[2026-08-30 21:14:53] Work item took a long time … 23CExitLizardModeWorkItem(4)
[2026-08-30 21:14:55] BYieldingCompleteSteamControllerRegistration
[2026-08-30 21:14:55] BYieldingCompleteSteamControllerRegistration - Error
    committing registration completion of controller & account pair:
    FXA9961402A6C Invalid Parameter
```

(`~/.steam/steam/logs/controller.txt`; identical failure on 2026-08-03.)

Steam routes the 2026 controller through a Steam Deck hardware-registration path
that fails, then leaves the device with lizard mode disabled and all five
`hidraw` interfaces held. This matches
[ValveSoftware/steam-for-linux#13185](https://github.com/ValveSoftware/steam-for-linux/issues/13185).

Two consequences for design:

- **The bar is low.** The status quo for desktop control with this controller
  under Hyprland is "broken", not "good". A passive-tap daemon does not have to
  beat a working system.
- **Do not depend on Steam's virtual pad or on Steam's desktop emulation.** Both
  are downstream of a code path that currently fails.

## Steam treats this controller as `controller_triton`

Steam's internal name for the 2026 Steam Controller is **`controller_triton`**
(seen throughout `~/.steam/steam/logs/controller.txt`). Two config files in
`~/.steam/steam/controller_base/` govern what Steam does with it outside games:

**`chord_triton.vdf`** — the Guide Button Chord Layout, titled "Steam Button
Chord Basic Configuration". Its bindings:

| Chord | Action |
|---|---|
| guide + X | `SHOW_KEYBOARD` |
| guide + B (long press, 2400 ms) | `quit_application` |
| others | `SCREENSHOT`, `controller_poweroff`, `toggle_magnifier`, `gr_toggle` / `gr_clip` / `gr_marker` (game recording), `sr_enable`, `system_key_1`, `VOLUME_UP` / `VOLUME_DOWN`, `Alt-Tab`, `ESCAPE`, `RETURN`, `TAB`, `mouse_button LEFT` / `RIGHT` |

This file explains a symptom observed during the guided capture in this
document: an on-screen keyboard appearing unbidden. That was `guide + X →
SHOW_KEYBOARD` firing from this layout.

**Desktop Layout.** There is no `desktop_triton.vdf`; the controller falls back
to `steamdesktop.vdf` / `desktop_neptune.vdf`. This is what maps trackpads to
cursor motion and triggers to clicks outside games.

### Why this matters architecturally

**Steam acts on this controller globally, not when focused.** Both layouts are
live whenever Steam is running, regardless of which window has focus. Trackpad
motion, trigger pulls and guide chords are consumed and acted upon by an
unfocused Steam. This is by design — Steam Input is a system-wide layer, not a
window — and it is the behaviour that motivates
[architecture D](05-architectures.md#d--uhid-device-proxy-full-interposition).

Both files are user-editable, which makes a cheap behavioural mitigation
possible without any architecture change; see
[06 — Recommendation](06-recommendation.md#tier-0--make-steam-inert-without-taking-the-device).

Related settings, from `localconfig.vdf` on this machine:

```
"UseSteamControllerConfig"   "2"      (Steam Input enabled)
"SteamController_XBoxSupport" "0"
"SteamController_PSSupport"   "2"
"ControllerTypesUsed"  "…,controller_neptune,"   (also logs controller_triton)
```

## Portal and protocol support

Hyprland 0.56.2 exports the protocols the recommended design needs:

```
zwlr_virtual_pointer_manager_v1      zwlr_virtual_pointer_v1
zwp_virtual_keyboard_manager_v1      zwp_virtual_keyboard_v1
hyprland_global_shortcuts_manager_v1
zwp_pointer_constraints_v1
```

IPC sockets are present at
`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
`.socket.sock` (commands) and `.socket2.sock` (event stream).

`xdg-desktop-portal-hyprland` 1.4.1 advertises:

```
Interfaces=org.freedesktop.impl.portal.Screenshot;
           org.freedesktop.impl.portal.ScreenCast;
           org.freedesktop.impl.portal.GlobalShortcuts;
           org.freedesktop.impl.portal.InputCapture;
```

**No `RemoteDesktop`.** This confirms the XWayland/`XTEST` dead end described in
[01 — Problem statement](01-problem.md): XWayland's `libei` translation path has
nothing to negotiate with, so Steam's emulated keyboard and mouse cannot reach
the desktop. Out-of-tree backends exist
([`xdg-desktop-portal-hypr-remote`](https://github.com/gac3k/xdg-desktop-portal-hypr-remote))
but are not installed here.

`InputCapture` *is* present, which is a newer and different thing — it captures
input for forwarding elsewhere, not the reverse.

## Environment notes

- `uinput` module is loaded but only Steam has the node open.
- `inputplumber` is **not** installed or running (it is in Arch `extra`).
- No udev rules on this system reference `1304`. Steam's own
  `60-steam-input.rules` grants `uaccess` to *every* `28de` `hidraw` device by
  vendor ID, which is why both Steam and an unprivileged probe can open it.
