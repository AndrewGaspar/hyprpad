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
("Valve Software Steam Controller Puck", serial `FXB0000000002`), exposing
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

### Layout — fully decoded (2026-08-31, Steam closed, lizard-mode cross-correlation)

Established by a combined capture (`tools/sc2-combined-capture.py`) reading the
raw hidraw stream and the lizard-mode evdev nodes simultaneously, with a guided
one-control-at-a-time script. Lizard-mode key events provided ground truth for
several buttons; both earlier captures agree everywhere they overlap.

| Bytes | Content | Encoding |
|---|---|---|
| `b0` | report ID | `0x42` |
| `b1` | sequence counter | u8, wraps |
| `b2`–`b5` | buttons + touch flags | bitfields, table below |
| `b6`–`b7` | **L2 trigger** | u16 LE, 0…32767 |
| `b8`–`b9` | **R2 trigger** | u16 LE, 0…32767 |
| `b10`–`b11` | **Left stick X** | i16 LE, ±32767, +X = right |
| `b12`–`b13` | **Left stick Y** | i16 LE, +Y = up |
| `b14`–`b15` | **Right stick X** | i16 LE |
| `b16`–`b17` | **Right stick Y** | i16 LE |
| `b18`–`b19` | **Left pad X** | i16 LE touch coords, 0 untouched |
| `b20`–`b21` | **Left pad Y** | i16 LE, +Y = up |
| `b22`–`b23` | **Left pad force** | u16 LE, 0…~12550, spikes on click |
| `b24`–`b25` | **Right pad X** | i16 LE |
| `b26`–`b27` | **Right pad Y** | i16 LE |
| `b28`–`b29` | **Right pad force** | u16 LE |
| `b30`–`b53` | **IMU** | streams **only when enabled** (Steam sends a feature report); with Steam closed this region is byte-for-byte constant across an entire capture |

Stick idle offsets are small but nonzero (±~400 counts) — apply a deadzone.

> The IMU discovery corrects the first capture's reading: with Steam running,
> `b30`–`b45` stream gyro/orientation continuously, which was mistaken for
> stick/pad data. Sticks and pads actually live at `b10`–`b29`.

### Button map — complete

| Byte.bit | Mask | Control | Evidence |
|---|---|---|---|
| `b2.0` | 0x01 | **A** | + lizard `KEY_ENTER` same 10 ms |
| `b2.1` | 0x02 | **B** | + lizard `KEY_ESC` |
| `b2.2` | 0x04 | **X** | isolated press (no lizard output) |
| `b2.3` | 0x08 | **Y** | isolated press (no lizard output) |
| `b2.4` | 0x10 | **Quick Access (…)** | 3/3 isolated (earlier) + isolated here |
| `b2.5` | 0x20 | **R3** (right-stick click) | isolated + accidental repeat during stick circle |
| `b2.6` | 0x40 | **Menu/Start** | + lizard `KEY_ESC` |
| `b2.7` | 0x80 | **R4 grip** | consistent across both sessions |
| `b3.0` | 0x01 | **R5 grip** | consistent across both sessions |
| `b3.1` | 0x02 | **R1 bumper** | position in script, both sessions |
| `b3.2` | 0x04 | **D-pad DOWN** | + lizard `KEY_DOWN` (twice — user pressed an extra Down, both captured) |
| `b3.3` | 0x08 | **D-pad RIGHT** | + lizard `KEY_RIGHT` |
| `b3.4` | 0x10 | **D-pad LEFT** | + lizard `KEY_LEFT` |
| `b3.5` | 0x20 | **D-pad UP** | + lizard `KEY_UP` |
| `b3.6` | 0x40 | **View/Select** | + lizard `KEY_TAB` |
| `b3.7` | 0x80 | **L3** (left-stick click) | isolated |
| `b4.0` | 0x01 | **Steam/Guide** | 3/3 isolated (earlier) + isolated here |
| `b4.1` | 0x02 | **L4 grip** | both sessions |
| `b4.2` | 0x04 | **L5 grip** | both sessions |
| `b4.3` | 0x08 | **L1 bumper** | both sessions |
| `b4.4` | 0x10 | capacitive (right-side grip/stick touch)* | fires on hand contact |
| `b4.5` | 0x20 | **Right pad touch** | touch-without-click window |
| `b4.6` | 0x40 | **Right pad click** | + lizard `BTN_LEFT` |
| `b4.7` | 0x80 | **R2 full press** | at end of slow analog pull; + lizard `BTN_LEFT` |
| `b5.0` | 0x01 | capacitive (left-stick touch)* | recurs during left-stick work |
| `b5.1` | 0x02 | **Left pad touch** | touch-without-click window |
| `b5.2` | 0x04 | **Left pad click** | force channel spikes simultaneously |
| `b5.3` | 0x08 | **L2 full press** | + lizard `BTN_RIGHT` |
| `b5.4` | 0x10 | capacitive* | fires on grip/pickup |
| `b5.5` | 0x20 | capacitive* | fires on grip/pickup |

\* The four capacitive bits (`b4.4`, `b5.0`, `b5.4`, `b5.5`) all fire on hand
contact and during pickup; individual assignment (left/right grip sense vs
stick capacitive touch) is tentative — they were never isolated one at a time.
Functionally they matter as a class ("hands on controller"), which is enough
for hyprpad.

### Lizard-mode output map (Steam closed) — the initramfs vocabulary

Confirmed live from the puck's `if02` evdev pair (`event20` mouse, `event21`
keyboard), all other interface slots silent:

| Control | Lizard output |
|---|---|
| A / B | `KEY_ENTER` / `KEY_ESC` |
| D-pad | arrow keys |
| Menu/Start | `KEY_ESC` |
| View/Select | `KEY_TAB` |
| L2 full / R2 full | `BTN_RIGHT` / `BTN_LEFT` |
| Right pad | mouse motion (`REL_X/Y`); click = `BTN_LEFT` |
| Left pad | scroll (`REL_WHEEL`/`REL_HWHEEL` + hi-res); click = nothing |
| X, Y, bumpers, grips, sticks, QAM, Steam | **nothing** |

Key repeat is firmware-driven (~30 Hz autorepeat after ~250 ms). This confirms
the initramfs plan's premise directly on hardware: the boot-time vocabulary is
arrows/Enter/Esc/Tab plus a mouse — no letters
([research/initramfs-unlock.md](research/initramfs-unlock.md)).

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

## The raw stream flows with Steam dead

Verified 2026-08-31 (with the owner's permission to cycle Steam): after a clean
`steam -shutdown`, with **no process holding any controller hidraw node**, report
`0x42` continued to flow on `hidraw7` at **269 Hz** — the same stream, same
format, same rate as with Steam running.

Lizard mode therefore governs only what the *evdev keyboard/mouse* interfaces
emit; the vendor gamepad report is always on. Consequences:

- hyprpad behaves identically pre-Steam, post-Steam, at a lock screen, in a
  cold-boot userspace — no mode switching, no Steam dependency, no
  initialization handshake needed to read input.
- The passive tap needs no awareness of Steam's lifecycle at all; Steam starting
  or stopping changes nothing about the read path.
- On Steam exit the virtual X360 pad disappears and all five hidraw nodes are
  released; on restart Steam re-grabs all five. Concurrent reading was
  unaffected throughout.

## Steam's reaction to the guide button

Established by direct observation (2026-08-30), and decisive for the
architecture: **Steam acts on guide _release_, never on press.** Three branches:

| Condition | Steam's reaction on release |
|---|---|
| Short hold, Steam window **not** focused | Steam window takes focus |
| Short hold, Steam window **already** focused | Launches Steam |
| **Hold longer than ~3 s** | **Nothing at all** |

**Addendum (2026-08-31, live test):** the release action is suppressed by
**analog** input during the hold but not by buttons: guide+right-stick-flick →
no reaction at all; guide+A → focus steal fires anyway; bare tap → focus steal
(control). Analog guide-chords therefore coexist with Steam for free; button
guide-chords need the window-rule mitigation.

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
    FXA0000000001 Invalid Parameter
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

**`desktop_neptune.vdf`** — the Desktop Layout, and the main offender. There is
**no `desktop_triton.vdf`**; the 2026 controller falls back to the Steam Deck's
desktop configuration (`"controller_type" "controller_neptune"`, titled "Desktop
Configuration"). Every binding below fires on a **bare press with no modifier**,
system-wide, while Steam is unfocused:

| Input | Binding |
|---|---|
| `button_x` | `controller_action SHOW_KEYBOARD` |
| `button_a` / `button_b` / `button_y` | `key_press RETURN` / `ESCAPE` / `SPACE` |
| dpad | arrow keys, and `xinput_button DPAD_*` |
| `left_bumper` / `right_bumper` | `key_press LEFT_CONTROL` / `LEFT_ALT` |
| `button_back_left` | `key_press LEFT_WINDOWS` |
| `button_back_left_upper` | `key_press LEFT_SHIFT` |
| `button_back_right` / `_upper` | `key_press PAGE_DOWN` / `PAGE_UP` |
| `button_menu` | `key_press TAB`, `xinput_button select` |
| `button_escape` | `key_press ESCAPE`; long press → `CHANGE_PRESET` |
| trackpad `click` / `edge` | `mouse_button LEFT` / `RIGHT` / `MIDDLE` |
| trackpad `scroll_*` | `mouse_wheel SCROLL_UP` / `SCROLL_DOWN` |
| `button_capture` | `controller_action system_key_1` |

**`chord_triton.vdf`** — the Guide Button Chord Layout, "Steam Button Chord Basic
Configuration". Requires the guide button held: guide + X → `SHOW_KEYBOARD`,
guide + B (long, 2400 ms) → `quit_application`, plus `SCREENSHOT`,
`controller_poweroff`, `toggle_magnifier`, game recording (`gr_toggle`,
`gr_clip`, `gr_marker`), `VOLUME_UP` / `VOLUME_DOWN`, `Alt-Tab`.

> **Correction.** An earlier revision of this document attributed the unbidden
> on-screen keyboard during the guided capture to `chord_triton.vdf`'s
> guide + X binding. That was wrong: the keyboard appears on a **bare X press**,
> confirmed by the device owner. The responsible binding is `button_x →
> SHOW_KEYBOARD` in `desktop_neptune.vdf`, which needs no modifier at all. Both
> files bind X to the keyboard; only the Desktop Layout does so unconditionally.

### Not all of these reach a Hyprland desktop

The bindings fall into three classes, which behave very differently here:

| Class | Route | Reaches Hyprland? |
|---|---|---|
| `controller_action` (`SHOW_KEYBOARD`, `system_key_1`, `CHANGE_PRESET`) | internal to Steam | **Yes, always** — Steam acts on it directly |
| `key_press`, `mouse_button`, `mouse_wheel` | `XTEST` | **No** — trapped inside XWayland ([see below](#portal-and-protocol-support)) |
| `xinput_button` | Steam's `uinput` virtual pad (`28de:11ff`) | **Yes** — a real kernel input device, visible to any evdev reader |

So on this system the observable damage is narrower than the table suggests, but
it is not zero, and the two classes that *do* land are the ones a user cannot
opt out of per-window.

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
