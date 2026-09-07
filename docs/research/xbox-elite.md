# Research: driving hyprpad from an Xbox Elite Series 2 (paddles, sticks-for-pads, a second input backend)

*Produced 2026-09-01/02 on the Framework 16 (Linux 7.1.9-arch1-2, Omarchy/Arch,
steam 1.0.0.87, bluez 5.87). Goal: let the owner drive Hyprland with the Xbox
Elite Series 2 they have to hand — "it should map well to my current layout
except for the trackpads" — as a second input device beside the 2026 Steam
Controller puck, with the four back paddles standing in for the puck's four
grips.*

**Verification basis.** **VERIFIED(local)** = read off this machine tonight
(`/proc/bus/input/devices`, sysfs, `udevadm info`, the report descriptor,
`~/.local/share/Steam/logs/controller.txt`, the Hyprland log), read-only, with
the controller paired over Bluetooth — it went to sleep at 23:12 before a live
report could be captured. **VERIFIED(kernel)** = read from `torvalds/linux`
`master` (fetched 2026-09-01) and, where the running kernel matters, the `v7.1`
tag. **VERIFIED(xpadneo/xone/SDL/systemd)** = read from those projects'
`master`/`main` branches. **VERIFIED(repo)** = this repository, `file:line`.
**INFERRED** = my reasoning; every number in §3 is a tuning starting point.

---

## 0. TL;DR

- **The controller is already here, over Bluetooth LE**, not USB: `045e:0b22`
  "Xbox Wireless Controller", firmware 5.21 (`bcdDevice 0x0521`), bound to the
  in-kernel `hid-microsoft`, exposed as `/dev/input/event31` + `js0` and
  `/dev/hidraw12`. VERIFIED(local). Steam already opens `hidraw12` with SDL's
  HIDAPI Bluetooth driver ("Controller using HIDAPI driver, vid=0x045e,
  pid=0x0b22" / "Product: Xbox One Elite 2 Controller") — the `steam-devices`
  package ships a `uaccess` rule for exactly `*045E:0B22*`. VERIFIED(local).
- **Paddle verdict.** The paddles *are* on the wire in every mode, but only two
  stock kernel paths surface them as four distinct buttons:
  1. **Wired USB → `xpad`** (`045e:0b00`): `BTN_GRIPR/BTN_GRIPR2/BTN_GRIPL/BTN_GRIPL2`
     (`0x225/0x227/0x224/0x226`) since 6.17 (before that `BTN_TRIGGER_HAPPY5..8`),
     via the fw ≥ 5.11 "extra input packet" the driver enables itself. Muted by
     the driver unless the controller is in **profile slot 0 (no LED)**.
     VERIFIED(kernel). The running 7.1 kernel already has the `BTN_GRIP*` form.
  2. **Bluetooth → `hid-microsoft`**: the four paddles arrive as a 4-bit
     Consumer-page usage `0x81` ("Assign Selection", byte 19 of report 1), which
     generic `hid-input` folds into a single `KEY_UNKNOWN` — indistinguishable.
     VERIFIED(local: `KEY=… 1000000000000 …` bit 240; kernel: `hid-input.c`
     Consumer default). Three fixes exist: the **in-tree HID-BPF program**
     `drivers/hid/bpf/progs/Microsoft__Xbox-Elite-2.bpf.c` (matches this exact
     464-byte descriptor; needs the `udev-hid-bpf` package, in Arch `extra`,
     not installed) → `BTN_TRIGGER_HAPPY5..8`; **xpadneo-dkms** (in the
     `omarchy` repo) → `BTN_GRIP*` + `ABS_PROFILE`, profile-0 only; or
     **hyprpad parsing `hidraw12` itself**, exactly the passive-tap
     architecture it already uses for the puck — zero system changes, profile
     byte included, rumble writable on the same node.
- **Nothing on the Xbox side is a trackpad.** `Frame.left_pad/right_pad` and
  the touch/click bits stay empty; the right stick becomes a **rate-controlled
  cursor** (velocity = f(deflection), no One Euro), the left stick a
  rate-controlled scroll, and the OSK is driven by the two sticks as
  rate-controlled per-hand cursors over the existing `cursor L|R` wire (zero
  OSK-child change) in phase 1, with console-style snap navigation as the
  phase-2 upgrade. Haptics become no-ops (or short rumble taps later).
- **A change-driven device needs a clock.** The puck streams at 263 Hz whether
  or not anything moves; `xpad`, `hid-microsoft` and the BLE firmware report
  **only on change**. A stick held at 60 % produces no further frames, so every
  velocity integrator in this design runs off a 250 Hz `Input::Tick`, not off
  reports. (xpadneo's own mouse mode is a 10 ms kernel timer for this reason.)
- **Phase 1 recommendation:** a generic `src/evdev.rs` backend (hand-rolled
  `libc` ioctls, matching `keyboard.rs`/`gamepad.rs`), capability-based
  discovery (udev calls this pad a *keyboard*, not a joystick — see §1.3.4),
  `EVIOCGRAB` while adopted, a `Source` tag on `Frame`, the 250 Hz tick, stick
  cursor/scroll, OSK option (i), and paddles accepted under **both** code sets
  (`BTN_GRIP*` and `BTN_TRIGGER_HAPPY5..8`). For paddles over Bluetooth the
  owner installs `udev-hid-bpf` (one package) *or* plugs in USB-C; the hidraw
  sidecar decoder that removes even that dependency is phase 2.

  > **Correction (owner review, 2026-09-02):** not a free-running tick. The loop already
  > blocks on its input channel with a *deadline* (reconnect scan, rumble re-send); the
  > stick integrators arm a 4 ms deadline **only while a stick is outside its deadzone or a
  > scroll rate is non-zero**, and drop it the moment everything is centred. Idle, the loop
  > blocks indefinitely, as it does today with the puck asleep. No busy poll, nothing runs
  > while nothing moves; per-wakeup cost is microseconds of arithmetic. Read every
  > `Input::Tick` below as this conditional deadline.

---

## 1. How the Elite Series 2 appears on Linux

### 1.1 What is on this machine right now — VERIFIED(local)

*Bluetooth addresses in this document are redacted: every `Phys`/`Uniq`/`bluetoothctl` address below is a synthetic stand-in for the real one.*

```
I: Bus=0005 Vendor=045e Product=0b22 Version=0521
N: Name="Xbox Wireless Controller"
P: Phys=00:11:22:33:44:55          U: Uniq=00:11:22:33:44:66
S: Sysfs=/devices/virtual/misc/uhid/0005:045E:0B22.0074/input/input242
H: Handlers=sysrq kbd event31 js0
B: EV=30001b        (SYN KEY ABS MSC REP FF)
B: KEY=7fff000000000000 1000000000000 8000000000 e080ffdf01cfffff fffffffffffffffe
B: ABS=30627        FF=107030000 0
```

Decoding the bitmaps (word order is high→low, 64 bits each):

| bitmap | bits set | meaning |
|---|---|---|
| `KEY` words 0–1 | `fffffffffffffffe e080ffdf01cfffff` | a **full PC keyboard**, KEY_ESC…KEY_COMPOSE — the BLE descriptor carries a keyboard collection (`05 01 09 06 a1 01 85 05 …`, report 5) |
| `KEY` word 2 bit 39 | `KEY_RECORD` (167) | Consumer `0xB2` "Record" — on the Elite 2 this is the **Profile button**, not Share (xpadneo docs, VERIFIED) |
| `KEY` word 3 bit 48 | `KEY_UNKNOWN` (240) | the three Consumer usages `hid-input` cannot name: `0x85` profile slot, `0x99` trigger locks, **`0x81` paddles** |
| `KEY` word 4 bits 48–62 | `BTN_SOUTH`…`BTN_THUMBR` (0x130–0x13e) | 15 gamepad buttons, Button usages 1..15 |
| `ABS` | X Y Z RZ GAS BRAKE HAT0X HAT0Y | sticks, triggers, d-pad |
| `FF` | RUMBLE PERIODIC SQUARE TRIANGLE SINE GAIN | `hid-microsoft` `MS_QUIRK_FF` via `ff-memless` |

Other facts from the same session:

- `driver → …/bus/hid/drivers/microsoft`; `MODALIAS=hid:b0005g0001v0000045Ep00000B22`;
  `bluetoothctl info` reports `Modalias: usb:v045Ep0B22d0521`, `Battery
  Percentage: 100`, services Device Information / Battery / HID.
- The report descriptor is **464 bytes** (`/sys/…/0005:045E:0B22.0074/report_descriptor`)
  — the exact size the in-tree HID-BPF fix asserts against (§1.3.5).
- `udevadm info /dev/input/event31`: `ID_INPUT=1 ID_INPUT_KEY=1
  ID_INPUT_KEYBOARD=1 ID_BUS=bluetooth`, **no `ID_INPUT_JOYSTICK`**; `TAGS=:power-switch:`;
  `LIBINPUT_DEVICE_GROUP=5/45e/b22:00:11:22:33:44:55`. Same for `js0`.
- Permissions: `event31` is `crw-rw---- root input` with **no** uaccess ACL —
  this user reads it through the `input` group (`id`: groups include
  `992(input)`). `hidraw12` is `crw-rw----+ root root` **with** `user:ajg:rw-`,
  granted by `/usr/lib/udev/rules.d/60-steam-input.rules`:
  `# Xbox One Elite 2 Controller — KERNEL=="hidraw*", SUBSYSTEM=="hidraw",
  KERNELS=="*045E:0B22*", MODE="0660", TAG+="uaccess"`.
- Steam's `controller.txt`: `Controller using HIDAPI driver, vid=0x045e,
  pid=0x0b22` / `Product: Xbox One Elite 2 Controller` / `path: /dev/hidraw12`,
  and on 2026-09-01 07:40–08:04 six `Unable to open local device: /dev/hidraw12`
  lines — something (the W12 mask experiment, most likely) denied Steam the
  node that morning.
- Hyprland's log: at 22:57:35 `[libinput] libinput bug: Event for missing
  capability CAP_POINTER on device "Xbox Wireless Controller"` (×3) — libinput
  adopted it **as a keyboard** and then received stick/hat events it had no
  capability for; at 23:12:55 `event31 - Xbox Wireless Controller: device
  removed` (the pad slept). So the raw Elite *does* reach the compositor today.
- No `xpad`, `xone`, `xpadneo` or `udev-hid-bpf` present: `lsmod` shows
  `hid_microsoft`, `ff_memless`, `uhid`, `joydev`; `pacman -Q udev-hid-bpf` →
  not installed (available: `extra/udev-hid-bpf 2.3.0.20260703-1`);
  `omarchy/xpadneo-dkms 0.10.4-1` is in a configured repo. Kernel config:
  `CONFIG_HID_BPF=y`, `CONFIG_JOYSTICK_XPAD=m`, `CONFIG_JOYSTICK_XPAD_FF=y`,
  `CONFIG_HID_MICROSOFT=m`, `CONFIG_UHID=m`.
- `/etc/bluetooth/main.conf` has a bare `[LE]` section: no connection-interval
  overrides (relevant to §1.3.6).

### 1.2 Wired USB — `xpad` (`045e:0b00`) — VERIFIED(kernel)

`drivers/input/joystick/xpad.c` matches every Microsoft vendor-specific
interface (`XPAD_XBOXONE_VENDOR(0x045e)`; there is no per-PID modalias, which is
why `modinfo xpad` lists none) and names the device from its table:
`{ 0x045e, 0x0b00, "Microsoft X-Box One Elite 2 pad", MAP_PADDLES, XTYPE_XBOXONE }`
(xpad.c:132). What it exposes:

| control | evdev | notes |
|---|---|---|
| face A/B/X/Y | `BTN_A/B/X/Y` (= `BTN_SOUTH/EAST/NORTH/WEST`) | from the `GIP_CMD_INPUT` (0x20) packet |
| D-pad | `ABS_HAT0X/Y`, range −1..1 | `MAP_DPAD_TO_BUTTONS` is not set for 0x0b00, so **axes**, not `BTN_DPAD_*` |
| bumpers | `BTN_TL`/`BTN_TR` | |
| triggers | `ABS_Z` (left) / `ABS_RZ` (right), **0..1023**, fuzz 0, flat 0 | `xpad_set_up_abs`: `XTYPE_XBOXONE` → 10-bit |
| sticks | `ABS_X/Y` left, `ABS_RX/RY` right, **−32768..32767, fuzz 16, flat 128** | Y is inverted in the driver so +Y = down (Linux convention) |
| Menu / View | `BTN_START` / `BTN_SELECT` | `data[4]` bits 2/3 |
| stick clicks | `BTN_THUMBL` / `BTN_THUMBR` | |
| Xbox button | `BTN_MODE` | arrives in its **own** `GIP_CMD_VIRTUAL_KEY` (0x07) report, which the driver must ack or "it continues sending them forever" |
| Share | — | `MAP_SHARE_BUTTON` is only on 0x0b12 (Series X\|S) and third-party pads; the Elite 2 has no Share button. Irrelevant. |
| **paddles** | **`BTN_GRIPR`, `BTN_GRIPR2`, `BTN_GRIPL`, `BTN_GRIPL2`** = upper-right, lower-right, upper-left, lower-left (xpad.c:461–464) | see below |
| rumble | `FF_RUMBLE` (`CONFIG_JOYSTICK_XPAD_FF`) | `GIP_CMD_RUMBLE` with four motors incl. trigger motors |

**Paddle history and mechanics.**

- Added by `e23c69e33248` "Input: xpad - add support for XBOX One Elite
  paddles" (Christopher Crockett, 2022-08-18, **Linux 6.1**), then as
  `BTN_TRIGGER_HAPPY5..8`. The commit message: *"Starting with firmware v5.11,
  certain inputs for the Elite 2 were moved to an extra packet that is not
  enabled by default. We must first manually enable this extra packet"* and
  *"properly suppresses paddle inputs when using a custom profile slot"*.
- Re-coded by `e7412ba919f6` "Input: xpad - use new BTN_GRIP* buttons" (Vicki
  Pfau, 2025-07-27, **6.17**), alongside the new codes in
  `input-event-codes.h` (`BTN_GRIPL 0x224, BTN_GRIPR 0x225, BTN_GRIPL2 0x226,
  BTN_GRIPR2 0x227`) and the Gamepad Specification text: *"For these
  controllers, BTN_GRIPR and BTN_GRIPR2 should be used for the top and bottom
  right grip button(s), and BTN_GRIPL and BTN_GRIPL2 … left"*
  (`Documentation/input/gamepad.rst` "Grip buttons"). **Both the `v7.1` tag
  and this machine's `linux-api-headers` 7.2 carry the `BTN_GRIP*` form**
  (VERIFIED: `xpad-v7.1.c:460–464`, `/usr/include/linux/input-event-codes.h:605–608`).
- Init sequence (`xboxone_init_packets`, xpad.c:713–728): `xboxone_power_on`,
  then for 0x0b00 `xboxone_s_init = {POWER, INTERNAL, seq, 0x0f, 0x06}` —
  *"required for Xbox One S and Elite Series 2 pads to initialize the
  controller that was previously used in Bluetooth mode"* (so plugging the
  owner's BT-paired pad into USB is expected to work) — then
  `extra_input_packet_init = {0x4d, 0x10, 0x01, 0x02, 0x07, 0x00}` *"required to
  get additional input data from Xbox One Elite Series 2 pads. We mostly do
  this right now to get paddle data"*. SDL sends the same six bytes
  (`SDL_hidapi_xboxone.c` "Enabling paddles on XBox Elite 2").
- Packet-type detection is by USB `bcdDevice` (xpad.c:2154–2184): `< 0x0500` →
  `PKT_XBE2_FW_OLD` (paddles in `data[18]` of the input packet, muted when
  `data[19] != 0`); `< 0x050b` → `PKT_XBE2_FW_5_EARLY` (`data[22]`/`data[23]`);
  else `PKT_XBE2_FW_5_11`: paddles arrive in a **separate `GIP_CMD_FIRMWARE`
  (0x0c) packet**, `data[18]` bits 0..3 = GRIPR, GRIPR2, GRIPL, GRIPL2, and
  *"Mute paddles if controller is in a custom profile slot — checked by looking
  at the active profile slot to verify it's the default slot"* (`data[19] != 0`
  → zero) (xpad.c:1054–1071). This controller's firmware (`0x0521`) takes the
  split-packet path.
- The profile slot itself is **not** exported by `xpad` for the Elite 2;
  `ABS_PROFILE` is only set up under `MAP_PROFILE_BUTTON` (the Adaptive
  Controller). xpadneo exports it (§1.3.5).

### 1.3 Bluetooth — the in-kernel `hid-microsoft`, the descriptor, and the paddle nibble

#### 1.3.1 Driver binding — VERIFIED(kernel, local)

`hid-microsoft.c` claims the Elite 2 by both of its Bluetooth PIDs
(`hid-ids.h:1056–1057`: `USB_DEVICE_ID_MS_XBOX_CONTROLLER_MODEL_1797 0x0b05`
= pre-BLE firmware, `…_1797_BLE 0x0b22` = BLE firmware) with
`.driver_data = MS_QUIRK_FF` (hid-microsoft.c:433–436). The quirk adds
`FF_RUMBLE` through `ff-memless` and nothing else: **the driver has no input
mapping of its own for Xbox pads** — everything comes from generic
`hid-input.c`. Rumble is output report 3 (`xb1s_ff_report`: `report_id, enable
(ENABLE_WEAK=BIT0, ENABLE_STRONG=BIT1), magnitude[4] (0..100; index 2 = strong
= left motor, 3 = weak = right), duration_10ms, start_delay_10ms, loop_count`,
hid-microsoft.c:42–58, 281–305).

#### 1.3.2 The report descriptor and report 1's layout — VERIFIED(local; cross-checked against xpadneo `events.c` and SDL's BT driver)

The 464-byte descriptor dumped from sysfs declares, in order: report 1 (Game
Pad), output report 3 (PID "Set Effect" — the rumble above), input report 12
(four 8-bit Consumer usages), and report 5 (a keyboard). Report 1, **as read
from hidraw with the report ID in front** (20 bytes):

| byte | field | HID usage | range / bits |
|---|---|---|---|
| 0 | report ID | — | `0x01` |
| 1–2 | left stick X | GD `X` | u16 0..65535, centre 32768 |
| 3–4 | left stick Y | GD `Y` | u16, **+Y is down** (HID convention) |
| 5–6 | right stick X | GD `Z` | u16 |
| 7–8 | right stick Y | GD `Rz` | u16 |
| 9–10 | left trigger | Simulation `0xC5` Brake → `ABS_BRAKE` | 10-bit 0..1023 in the low bits |
| 11–12 | right trigger | Simulation `0xC4` Accelerator → `ABS_GAS` | 10-bit |
| 13 | D-pad | GD `Hat switch` (null state) → `ABS_HAT0X/Y` | 0 = centred, 1..8 clockwise from Up |
| 14 | buttons 1–8 | Button page → `BTN_GAMEPAD + n−1` | bit0 A, bit1 B, bit3 X, bit4 Y, bit6 LB, bit7 RB (bits 2 and 5 = `BTN_C`/`BTN_Z`, never set) |
| 15 | buttons 9–15 | | bit2 View (`BTN_SELECT`), bit3 Menu (`BTN_START`), **bit4 Xbox (`BTN_MODE`)**, bit5 L3, bit6 R3 |
| 16 | bit0 | Consumer `0xB2` Record → `KEY_RECORD` | **Profile button** on the Elite 2 (Share on Series X\|S) |
| 17 | **profile slot** | Consumer `0x85` → `KEY_UNKNOWN` | 0 = no profile (LED off), 1..3 |
| 18 | trigger-lock switches | Consumer `0x99` → `KEY_UNKNOWN` | nibble, two 2-bit positions (xpadneo: "Trigger scale switches") |
| 19 | **paddles** | Consumer `0x81` "Assign Selection" → `KEY_UNKNOWN` | **bit0 P1 upper-right, bit1 P2 lower-right, bit2 P3 upper-left, bit3 P4 lower-left** |

Cross-checks: xpadneo's `events.c:138–142` ("firmware 5.x style packet":
`switch_profile(data[17] & 0x03)`, `switch_triggers(data[18] & 0x0F)`) and
`events.c:162–170` (paddle usage value `& 1/2/4/8` → `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2`);
SDL's `HIDAPI_DriverXboxOneBluetooth_HandleButtons` for `size == 20`:
`paddle_index = 19`, `paddles_mapped = (data[17] != 0)`, bits `0x01/0x02/0x04/0x08`
in that order; and `hid-input.c`: Button page → `code += BTN_GAMEPAD`
(hid-input.c:795–810), `0x0b2 → KEY_RECORD` (:1201), Consumer `default:
map_key_clear(KEY_UNKNOWN)` (:1321), Simulation `0xc4 → ABS_GAS`, `0xc5 →
ABS_BRAKE` (:841–842). The Xbox button is a plain Button-13 bit in the same
report (the older `0x02fd` firmware sent it separately as Consumer `AC Home`;
that quirk is gone).

**Why `KEY_UNKNOWN` cannot carry the paddles.** The input core clamps an
`EV_KEY` value to pressed/not-pressed, and three different fields share the one
code. A nibble going 0→1→3 is one press and no further events; in profile slots
1–3 the non-zero profile byte holds `KEY_UNKNOWN` **down permanently**, so the
paddles are invisible even as "some paddle". `MSC_SCAN` (hid-input.c:1787)
does precede each edge with the usage (`0x000C0081`), which would let a
listener tell *which field* toggled but never *which paddle*. Dead end;
the in-tree BPF program exists because of exactly this ("The kernel doesn't
process the paddles usage properly and reports KEY_UNKNOWN. SDL doesn't know
how to interpret KEY_UNKNOWN and thus ignores the paddles.").

#### 1.3.3 What the controller firmware does with the paddles — VERIFIED(kernel, xpadneo, SDL)

In profile slots 1–3 the firmware itself re-emits a paddle as whatever face
button the Xbox Accessories app mapped it to (factory default P1→B, P2→Y,
P3→A, P4→X per user reports; INFERRED as to the exact default), **and still
reports the raw nibble**. That is why every consumer mutes the nibble outside
slot 0: `xpad` (`data[19] != 0` → paddles zeroed), xpadneo ("The default
profile (no LED) exposes the paddles as extra buttons. The other three profiles
behave the same way by default" — `docs/README.md` "Native Profile Switching";
`events.c:163`: `if (gamepad && xdata->profile == 0)`), SDL ("Respect that the
paddles are being used for other controls and don't pass them on to the app").
**The owner must leave the controller in slot 0** (press the Profile button
until no profile LED is lit) for any path below; hyprpad should read the slot
where it can (BT hidraw sidecar; xpadneo's `ABS_PROFILE`) and warn otherwise.

#### 1.3.4 udev classifies the BLE Elite 2 as a keyboard — VERIFIED(local, systemd)

`udevadm test-builtin input_id` on `input242/event31` yields only
`ID_INPUT_KEY=1 ID_INPUT_KEYBOARD=1`. systemd's *joystick un-detection*
(`src/udev/udev-builtin-input_id.c` "Joystick un-detection. Some keyboards have
random joystick buttons set …": if a joystick-looking device also has ≥ 4 of
`KEY_LEFTCTRL, KEY_CAPSLOCK, KEY_NUMLOCK, KEY_INSERT, KEY_MUTE, KEY_CALC,
KEY_FILE, KEY_MAIL, KEY_PLAYPAUSE, KEY_BRIGHTNESSDOWN` it is *"assuming this is
a keyboard, not a joystick"*) fires on the descriptor's keyboard collection —
xpadneo's docs say the same: *"All XBE2 controllers will claim to have a full
keyboard"* (`docs/README.md` "BLE firmware"). Consequences:

- **libinput/Hyprland adopt it as a keyboard** (the `CAP_POINTER` bug lines
  above). The Profile button is a `KEY_RECORD` keystroke into whatever is
  focused, and `KEY_UNKNOWN` toggles whenever the paddles, the locks or the slot
  change. A hyprpad backend that `EVIOCGRAB`s the node while it owns the pad
  stops both leaks (§4.6); a compositor-side
  `hl.device({ name = "xbox-wireless-controller", enabled = false })` rule in
  `~/.config/hypr/hyprpad-rules.lua` is the belt-and-braces alternative.
- **hyprpad must not trust `ID_INPUT_JOYSTICK`** to find the pad. Discovery
  has to be capability-based (`ABS_X`+`ABS_Y` and `BTN_SOUTH`/`BTN_GAMEPAD` in
  the sysfs `capabilities/{abs,key}` bitmaps), the way SDL's
  `SDL_evdev_capabilities.c` does it.
- Steam's evdev fallback (`SDL_udev.c` → `ID_INPUT_JOYSTICK`) would likewise
  not see it; today Steam reaches it over `hidraw12` instead (§1.1).

#### 1.3.5 The three ways to get four paddles over Bluetooth

| path | what appears | profile gating | cost / state on this machine |
|---|---|---|---|
| **A. `udev-hid-bpf` + in-tree `Microsoft__Xbox-Elite-2.bpf.c`** (Benjamin Tissoires, 2024; `HID_DEVICE(BUS_BLUETOOTH, HID_GROUP_GENERIC, 0x045e, 0x0b22)`, asserts `ORIGINAL_RDESC_SIZE 464` and the `0x99/0x81` bytes at offset 211 — **both match this controller**) rewrites the descriptor so usage `0x81` becomes Button usages `0x15..0x18` in a new Game Pad collection → **`BTN_TRIGGER_HAPPY5..8`** (0x2c4–0x2c7) on the same `event31`, still under `hid-microsoft`. VERIFIED(kernel). | 4 distinct buttons | **none** — the program only relabels; slots 1–3 double-fire (firmware button + paddle). hyprpad cannot read the slot byte on this path (it is still one of the `KEY_UNKNOWN` fields). | `pacman -S udev-hid-bpf` (extra 2.3.0). Whether the Arch package ships this in-tree program compiled is **unverified**: check `ls /usr/lib/firmware/hid/bpf/ \| grep -i xbox` and `udev-hid-bpf list-bpf-programs` after install, then reconnect the pad. Note the program's comment still says "over USB the kernel uses BTN_TRIGGER_HAPPY[5-8]" — true when written, false since 6.17; the two stock paths now **disagree on codes**, so hyprpad must accept both. |
| **B. xpadneo-dkms 0.10.4** (`omarchy` repo) | `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2` (its `mappings.c:61` maps `0xC0081` then splits the nibble in `events.c:162–170`; codes changed to match SDL/kernel in v0.10 — "Paddle Button Codes Changed"), plus `ABS_PROFILE` 0..3, a separate keyboard device for the odd keys, "Linux Gamepad Specification" axis compliance, trigger rumble, BLE-latency guidance. VERIFIED(xpadneo). | yes — paddles only in slot 0, slot exported | DKMS module + kernel headers already installed; replaces `hid-microsoft` for the pad; also changes what Steam/SDL see. Heaviest option. |
| **C. hyprpad reads `/dev/hidraw12` itself** | whatever hyprpad decodes from the table in §1.3.2 — paddles, slot, locks, Profile button — into a `Frame` directly, bypassing `hid-input` entirely | yes — byte 17 | **zero system changes**; the node is already user-readable via the `steam-devices` rule; hidraw is non-exclusive so Steam's HIDAPI reader and hyprpad coexist exactly as on the puck (`docs/02`, `docs/03`); rumble is a 9-byte write of report 3 on the same fd (§3.6). Cost: one decoder (`~120 lines`) shaped like `report::Frame::decode`, specific to `045e:0b22`'s 464-byte descriptor (guard on it, as the BPF program does). |

#### 1.3.6 BLE quirks and latency — VERIFIED(xpadneo docs; local config)

- The pad reports at **100 Hz internally**; xpadneo's troubleshooting page
  attributes "laggy or choppy input, lost or delayed button presses" on BLE
  Xbox pads to the connection-interval parameters bluez leaves at defaults and
  recommends `[LE] MinConnectionInterval=7 MaxConnectionInterval=9
  ConnectionLatency=0` (units of 1.25 ms → 8.75–11.25 ms) in
  `/etc/bluetooth/main.conf`. This machine has no such override (the `[LE]`
  section is empty). Only if the owner feels lag; it is a system-wide bluez
  knob and the bluez developers discourage it.
- **Reports only on change** (§0). Not directly observed (the pad was idle,
  then asleep, during the 4 s passive read of `hidraw12`), but it is how HID
  gamepads over Bluetooth behave and it is why xpadneo's mouse profile runs a
  10 ms timer (`mouse.c:52`). Design for it (§3.1, §4.5).
- Rumble over BT is supported by `hid-microsoft` (report 3) and by SDL's BT
  driver (`{0x03, 0x0F, LT, RT, low, high, 0xFF, 0x00, 0xEB}`); `xpad` is not
  involved over Bluetooth at all.
- The 2021+ firmware moved the pad to BLE and to PID `0x0b22`; a dongle without
  BLE cannot pair it (xpadneo docs). The Framework's MediaTek MT7925 is BLE.

### 1.4 The Xbox Wireless Adapter — `xone` — VERIFIED(xone)

`medusalix/xone` is a GIP driver for the USB dongle and wired pads; "Installing
`xone` will disable the `xpad` kernel driver" and Bluetooth is explicitly
delegated to xpadneo. Its `driver/gamepad.c` handles the Share button
(`KEY_RECORD` at a fixed offset from the packet end) and **nothing about
paddles** (no `GRIP`, `TRIGGER_HAPPY`, `paddle` or `profile` in the source).
No adapter is attached here (`lsusb`). Not a path for this project.

### 1.5 Verdict: which path exposes the paddles, and what the owner must do

| connection | driver | paddles as 4 buttons | owner action |
|---|---|---|---|
| **USB-C cable** | `xpad` (stock 7.1) | **yes** — `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2` | plug in; profile slot 0 |
| Bluetooth | `hid-microsoft` (stock) | **no** — one `KEY_UNKNOWN` | — |
| Bluetooth | `hid-microsoft` + `udev-hid-bpf` | yes — `BTN_TRIGGER_HAPPY5..8` | `pacman -S udev-hid-bpf`, reconnect; profile slot 0 (not enforced) |
| Bluetooth | xpadneo-dkms | yes — `BTN_GRIP*` + `ABS_PROFILE` | install DKMS module, reconnect; profile slot 0 (enforced) |
| Bluetooth | hyprpad hidraw sidecar (§4.3) | yes — decoded by hyprpad | nothing; profile slot 0 (enforced by hyprpad) |
| Xbox Wireless Adapter | `xone` | no | not applicable |

**Recommendation:** phase 1 accepts both evdev code sets so wired-`xpad` and
BT-with-BPF both work; the owner's cheapest paddle route tonight is the cable,
the cheapest cable-free route is `udev-hid-bpf`; phase 2's hidraw sidecar makes
the BT case self-contained and profile-aware.

---

## 2. Mapping to `report::Button` / `Frame`

`report::Button` (report.rs:12–43) and `Frame` (report.rs:81–91) stay the
daemon's vocabulary; a second backend fills the same struct so `GestureEngine`,
`Config`, `ModeEngine`, `drive_buttons`, `route_osk` and the sheet need no new
names. The chord grammar (`config.rs:339–368 parse_button`, `bindings_sheet.rs:472–507
button_name`) therefore already spells every Elite control.

| Elite Series 2 | xpad (USB) | hid-microsoft (BT) / hidraw byte | hyprpad `Button` / `Frame` field | config name |
|---|---|---|---|---|
| A / B / X / Y | `BTN_SOUTH/EAST/NORTH/WEST` | byte 14 bits 0/1/3/4 | `A/B/X/Y` | `a b x y` |
| D-pad | `ABS_HAT0X/Y` −1..1 | byte 13 hat 1..8 | `DpadUp/Down/Left/Right` (decode the hat into the four bits; diagonals set two) | `dpad_*` |
| LB / RB | `BTN_TL/TR` | bits 6/7 | `BumperL1/BumperR1` | `l1 r1` |
| LT / RT analog | `ABS_Z/RZ` 0..1023 | bytes 9–12, 10-bit | `l2/r2: u16` scaled ×32 to the puck's 0..32767 | — |
| LT / RT full pull | *synthesised* | *synthesised* | `TriggerL2Full/R2Full` — press at ≥ 0.85 of range, release at ≤ 0.70 (hysteresis; INFERRED starting point). The puck's bit is a firmware click; here it is a threshold. | `l2 r2` |
| View / Menu | `BTN_SELECT/START` | byte 15 bits 2/3 | `View/Menu` | `view menu` |
| L3 / R3 | `BTN_THUMBL/R` | bits 5/6 | `L3/R3` | `l3 r3` |
| **Xbox button** | `BTN_MODE` (own GIP report) | byte 15 bit 4 | `Steam` — the guide. Everything in `gesture.rs` keys off `Button::Steam` (gesture.rs:112) | `guide+…` |
| **P1 upper-right** | `BTN_GRIPR` 0x225 / `BTN_TRIGGER_HAPPY5` 0x2c4 | byte 19 bit 0 | `GripR4` | `r4` |
| **P2 lower-right** | `BTN_GRIPR2` 0x227 / `HAPPY6` 0x2c5 | bit 1 | `GripR5` | `r5` |
| **P3 upper-left** | `BTN_GRIPL` 0x224 / `HAPPY7` 0x2c6 | bit 2 | `GripL4` | `l4` |
| **P4 lower-left** | `BTN_GRIPL2` 0x226 / `HAPPY8` 0x2c7 | bit 3 | `GripL5` | `l5` |
| Profile button | — | byte 16 bit 0 (`KEY_RECORD`) | **unmapped** — pressing it cycles the firmware slot (mutes the paddles); expose only as a "slot changed" warning | — |
| trigger locks | — | byte 18 | not a `Button`; INFERRED: could scale the full-pull threshold when a lock shortens the throw | — |
| sticks | `ABS_X/Y`, `ABS_RX/RY` i16 | bytes 1–8 u16, +Y down | `left_stick/right_stick: (i16, i16)` — convert to the puck's ±32767, **+Y up** (report.rs:87) | `guide+stick_*` flicks |
| Quick Access (puck "…") | — | — | `QuickAccess` — no analogue; leave unbound | `quickaccess` |
| trackpads | — | — | `left_pad/right_pad`, `Pad*Touch/Click` — **no analogue**, always zero/false | `lpad rpad *_click` |
| gyro | — | — | not in `Frame` today; the Elite 2 has no IMU either | — |
| capacitive `Cap0..3` | — | — | always false | — |

Paddle-to-grip correspondence is the one SDL and the kernel both settled on:
`SDL_gamepad.h:172–175` defines `RIGHT_PADDLE1` = "Upper or primary paddle,
under your right hand (e.g. Xbox Elite paddle P1 … Steam Controller R4
button)", `LEFT_PADDLE1` = P3 = L4, `RIGHT_PADDLE2` = P2 = R5, `LEFT_PADDLE2`
= P4 = L5 (VERIFIED(SDL)); slouken: "SDL paddles are numbered across and then
down, and Xbox paddles are numbered down and then across". The cheat-sheet
layout already draws `l4` above `l5` (`layouts/steam-controller-2026.json`,
y 235 vs 268), so the Elite sheet inherits the right geometry.

**What `Frame` cannot carry, and how consumers should treat "no pad".** Add a
capability tag rather than a bag of booleans:

```rust
// report.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Source { #[default] Puck, XboxEvdev, XboxHid }
impl Source {
    pub fn has_pads(self) -> bool     { matches!(self, Source::Puck) }
    pub fn has_haptics(self) -> bool  { matches!(self, Source::Puck) }   // 0x81 pulses
    pub fn has_rumble(self) -> bool   { true }                           // FF_RUMBLE / report 3
    pub fn cursor_is_rate(self) -> bool { !self.has_pads() }
}
pub struct Frame { pub source: Source, /* existing fields */ }
```

`Frame` derives `Default`/`Copy` (report.rs:80), so `Source::default()` keeps
every existing test and the puck path byte-identical. Where it is consulted:

- `drive_cursor` (run.rs:895) and `drive_scroll` (run.rs:1011) already gate on
  `PadRightTouch`/`PadLeftTouch`, so with an Xbox frame they *fall through to
  the reset branch and do nothing* — safe by construction. The stick paths of
  §3 are new siblings selected on `frame.source.cursor_is_rate()`, not edits to
  the pad paths.
- `route_osk` (run.rs:2175) commits on `PadLeftClick | TriggerL2Full`; the
  trigger half already works for the Elite, and `route_pad` is skipped because
  the touch bits are false. §3.5 adds the stick-driven `cursor L|R`.
- `HapticCtx::fire` (run.rs:736–750) becomes a no-op when
  `!source.has_haptics()` — or better, `Haptics` grows a backend enum (§3.6).
- `button_pad` (run.rs:765) is exhaustive over `Button` and unchanged.
- The virtual gamepad's `PadReport::from_frame` (gamepad.rs:364) forwards
  sticks/triggers/buttons and ignores pads, so game forwarding of an Elite
  frame works today; note it will present the Elite's paddles to nothing
  (`045e:028e` has no grips), which is fine — the grips are hyprpad's.

---

## 3. Sticks in place of pads — design with numbers

### 3.1 The model: rate control, not position

The puck's cursor is *position* control: `drive_cursor` differences the
smoothed absolute pad position (`PadDamper::relative`, filter.rs:249–275) and
the One Euro filter exists because differencing amplifies sensor noise
(`docs/research/pointer-damping.md` §1.2). A stick is a *rate* controller —
deflection is a velocity command, and the kernel/firmware already applies
`fuzz`/`flat` — so **skip the One Euro filter entirely**; the only smoothing
question is on the velocity, and the answer is "none, or a 10–20 ms EMA".

The standard gamepad-mouse pipeline, as Steam Input, AntiMicroX and xpadneo
implement it (VERIFIED for each below), is:

```
r   = |(x, y)|                       // radial deflection, 0..1, from the ±32767 frame values
d   = clamp((r − dz_in) / (dz_out − dz_in), 0, 1)   // inner/outer deadzone rescale
g   = curve(d)                        // response curve, 0..1
v   = v_max · g                       // px/s
(vx, vy) = v · (x, y) / r             // keep the stick's direction exactly
Δ   = v · dt                          // per tick, accumulate sub-pixel remainders like acc_x/acc_y in PadDamper
```

- **Steam Input "Joystick Mouse"** (SteamInputWiki `chapter-2/02b05_joystick_mouse.md`):
  *"takes a directional input and converts it to relative Mouse movement … you
  can think of this Style as using the Joystick to push the Mouse cursor around
  the Desktop with full analog control."* Settings, verbatim: **Mouse
  Sensitivity** ("sets the maximum sensitivity of the relative Mouse movement.
  Think of it as a dpi setting"), **Stick Response Curve** {Linear, Aggressive,
  Relaxed, Wide, Extra Wide, Custom (+ slider)}, **Dead Zone Shape** {Circle,
  Cross, Square}, **Enable Deadzone** {None, Calibration, Configuration},
  **Dead Zone Inner** / **Dead Zone Outer** ("the effective 'throw range' …
  get stretched or compressed to be in between the Inner and Outer Dead
  Zones"), **Invert Horizontal/Vertical Axis**, **Output Axis** {both,
  horizontal only, vertical only}, **Haptics Intensity** (Steam Controller
  only). Contrast **Joystick Camera** (`02b07`): applies only to *touchpads
  and gyros* and adds "Adaptive Centering" (the deadzone re-centres under the
  thumb on touch) and "Smooth Joystick" (simulated return-to-centre) — i.e. it
  makes a *pad* act like a stick, the mirror image of our problem; and **Mouse
  Joystick** (`02b04`): mouse-like pad input turned into a right-stick angle
  for games — not relevant. Valve gives no numbers for the curves.
- **AntiMicroX** (`src/joybuttontypes/joybutton.cpp:1050–1174, 1322`;
  `src/globalvariables.cpp`): per mouse tick, `sumDist += difference *
  (mousespeed * JOYSPEED * timeElapsed) * 0.001` with `JOYSPEED = 20`,
  `DEFAULTMOUSESPEEDX/Y = 50`, `mouseRefreshRate = 5` ms — i.e. **1000 px/s at
  full deflection by default**, integrated every 5 ms. `difference` is the
  post-deadzone distance run through the curve: Linear; Quadratic `d²`; Cubic
  `d³`; Quadratic Extreme (`d²`, ×1.5 above 95 %); Power (`d^(1/sensitivity)`);
  **Enhanced Precision** (default): three regions 0–40 % scaled ×0.37, 40–75 %
  linear, above that `d·2.008 − 1.008`; Easing Quadratic/Cubic: the same
  regions but the top region ramps over 0.75 s ("mimic the camera control used
  for gamepad support in recent first person shooters"). Sub-pixel remainders
  carry between ticks.
- **xpadneo mouse profile** (`hid-xpadneo/src/xpadneo/mouse.c`): a **10 ms
  kernel timer**; `XPADNEO_MOUSE_MOVEMENT_DEADZONE 3072` (9.4 % of 32768)
  removed and rescaled (`rescale_axis`); `rel = value / 1024` per tick with the
  remainder kept (`rel_x_err = value % 1024`) → up to 32 px per 10 ms =
  **3200 px/s** at full deflection, linear. Fast, but it is a kernel driver
  with no curve.
- **Enjoyable** (macOS): not consulted — the repository was not reachable
  (`jstpierre/Enjoyable` returns 404 via the GitHub API), so no citation.
- **Microsoft's own XInput constants** for reference:
  `XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE 7849` (24 %) and `RIGHT_THUMB_DEADZONE
  8689` (26.5 %) — deliberately pessimistic per-axis values from `XInput.h`
  (VERIFIED from memory of the header; treat as approximate). `xpad`'s
  `flat 128` (0.4 %) is not a usable deadzone.

**Proposed defaults** (INFERRED; expose all under `h.cursor { stick = {…} }`):

| knob | default | why |
|---|---|---|
| `deadzone` (inner, radial) | 0.12 | above the Elite's resting offset with worn sticks; below XInput's 24 % which is too much for a pointer; xpadneo uses 9.4 % |
| `outer` | 0.95 | the corners of a square gate never reach 1.0 on a round gate; treat 95 % as full |
| `shape` | circle | Steam's advice for mouse output ("I would recommend generally setting this to Circle") |
| `curve` | `pow(d, 2.0)` (exponent configurable; 1.0 = linear) | quadratic = AntiMicroX "Quadratic": "slowed down slightly to allow better control on the low end" — the precision a desktop pointer needs; Enhanced Precision's 0.37 low-region gain is the same idea |
| `max_speed` | 1500 px/s | between AntiMicroX's 1000 and xpadneo's 3200; on the 2560-px-wide panel a full-tilt sweep crosses the screen in ~1.7 s, half tilt moves 375 px/s |
| `smoothing` | 0 ms (off); optional EMA `τ = 15 ms` | Steam has none; the stick's own mechanics are the filter |
| `precision_hold` | none in phase 1 | a later "hold L3 for 0.35× speed" if fine targets prove hard |
| tick | 4 ms (`Input::Tick`, §4.5) | same cadence the puck already drives the loop at, so `drive_buttons` reconcile and hold timers see the same rhythm |

Implementation shape: a `StickCursor { acc: (f64, f64), last: Instant }` with
`fn step(&mut self, stick: (i16, i16), now) -> (i32, i32)` — pure, unit-testable
like `swipe_scroll`. It reuses nothing from `PadDamper` except the idea of
`acc_x/acc_y`. No `travel_px` haptic texture (`Haptic::CursorMove`) — nothing to
tick.

### 3.2 Left stick → scroll rate

`drive_scroll`'s circular mode (run.rs:1011–1061, `AngleAccumulator` filter.rs:318)
needs an absolute finger position; a stick has one too, so **circling the
stick rim would actually work** with `min_radius ≈ 0.6` — an experiment, not
the default, because a thumb circling a spring-loaded stick is tiring. Default:
**vertical rate scroll** — `units_per_s = 180 · curve(d_y)` where 15
`wl_pointer.axis` units ≈ one wheel notch (the existing circular detent already
emits `sensitivity × 15` per 15° tick, run.rs:1054–1055, 1096–1103), i.e. up to 12
notches/s at full tilt, quadratic curve, `natural` and `horizontal` (left-right
tilt → `dx`) honoured exactly as `swipe_scroll` does (run.rs:1072–1088).
Emit the accumulated value each tick through `VirtualPointer::scroll`. The
detent haptic (`Haptic::Scroll`) has nothing to fire on; see §3.6.

### 3.3 Flicks — reuse `GuideStickFlick`

`GestureEngine::flick` (gesture.rs:171–198) runs on `frame.left_stick/right_stick`
with `FLICK_THRESHOLD 19_660` (60 % of ±32767), `RECENTER_THRESHOLD 8_000`,
`DEADZONE 4_000`, all `pub const`. Once the backend normalises the Elite's
sticks to the puck's scale and sign, `guide+stick_right` workspace flicks in
`config/hyprpad.lua` work unchanged. One caveat: the engine only sees a flick
on a *frame*, and a change-driven device produces frames while the stick moves,
so this is fine without the tick.

### 3.4 Trigger full-pull, guide hold, Xbox-button power-off

- Full pull is synthesised (§2). The gesture engine's chords are edge-triggered
  (`edges_down`, gesture.rs:129), so hysteresis on the threshold is what stops a
  trigger hovering at 85 % from re-chording.
- `HOLD_THRESHOLD 300 ms` (gesture.rs:63) is unaffected. **Holding the Xbox
  button ~6 s powers the controller off** (Microsoft's documented gesture;
  INFERRED that BLE firmware 5.x behaves the same) — no hyprpad hold binding
  may rely on multi-second holds.
- Steam acts on the guide *release* for Valve hardware (README); for an Xbox pad
  Steam Input (when enabled for Xbox controllers, §4.7) treats the Xbox button as
  its overlay/Big Picture key immediately. Bare-tap pass-through
  (`GuideLeave { was_chorded: false }`, run.rs:2047) keeps the same contract.

### 3.5 The on-screen keyboard

Today `route_osk` sends each pad's smoothed absolute position as `cursor L|R nx ny`
(osk.rs:7, run.rs:2229–2258) and commits on pad click or trigger full pull; the
child hit-tests (`osk/src/layout.rs:664 hit_test`) and reports crossings for
the haptic tick. The child already has a placeholder for a third, focus-driven
highlight (`osk/src/render.rs:19–36 HighlightKind::{LeftPad, RightPad, Focus}`)
"so the structure for §4.5's concurrent per-source highlights (left pad, right
pad, d-pad focus)" exists, but no focus navigation is implemented.

What console users expect (all three consoles snap a highlight between keys
and put the editing keys on face buttons):

- **Xbox** (Xbox Wire, "Keyboard Button Mapping for Xbox Controllers", 2023;
  Microsoft Q&A): left stick / D-pad move the highlight, **A** selects, **B**
  back, **X** Backspace, **Y** Space, **Menu** Enter, **LT** the `&123` layer,
  **LB/RB** move the text cursor left/right, **L3** Caps Lock.
- **Steam Deck** (`docs/research/osk-technology.md` §4.4, VERIFIED(code) there):
  D-pad + left stick move a DOM focus with `MAINTAIN_X` column memory (initial
  focus on `G`), A press, B dismiss, X Backspace (450 ms then 200 ms repeat),
  Y Space, L2 Shift, R2 Enter, L3 Caps, L1/R1 candidates, pads = independent
  cursors, and — the key finding — "there is no mode switching": focus and the
  two pad cursors are concurrently live.
- PlayStation and Switch keyboards behave the same way (highlight + face-button
  commit); exact button legends not verified here.

Options for hyprpad:

| | mechanism | OSK-child change | feel |
|---|---|---|---|
| **(i) two stick-driven cursors** | integrate each stick's velocity (§3.1 model, `v_max ≈ 1.2 keyboard-widths/s`, quadratic) into a normalised `[-1, 1]` position per hand and send it over the existing `cursor L|R` wire; commit = trigger full pull (already wired) or L3/R3 | **none** — daemon-only, ~80 lines | the split layout was designed for two per-hand cursors; a stick cursor drifts, but the keys are large and the L2/R2 commit is already muscle memory |
| **(ii) snap navigation** | left stick as a repeating D-pad (initial 250 ms, repeat 80 ms) + D-pad move a focus key; A = press, X/Y/Menu/L3 as the consoles; keep `MAINTAIN_X` | new `focus <up\|down\|left\|right>` and `press` commands, a neighbour search over the placed grid, the `Focus` highlight — 1–2 days in `osk/` | what every console user expects; deterministic; pairs with `h.osk_button` helpers that already exist (`osk_button("y", space)`, `("x", backspace)` in `config/hyprpad.lua`) |
| (iii) one cursor + coarse jumps | right stick cursor, left stick jumps rows/columns | both of the above | two mental models at once; nobody ships this |

**Phase-1 recommendation: (i).** It ships with the backend, exercises the whole
pipeline end-to-end, and costs the OSK nothing. **(ii) is the phase-2 target**
and should land together with the Elite cheat-sheet tab, because it changes
what the OSK tab says. Both can coexist afterwards exactly as on the Deck
(concurrent highlights), which is what `HighlightKind` was laid out for.

### 3.6 Haptics on a device that has a rumble motor instead

`Haptics::play` (haptics.rs:297) writes the puck's `0x81` pulse reports —
200–600 µs pulses (`Feel::Tick/Click/Buzz/Texture`, haptics.rs:173–176). A
rumble motor cannot produce those; `ff-memless` runs effects at jiffy
granularity and an ERM motor needs tens of milliseconds to spin up. Therefore:

- `Haptics` gains a backend: `enum HapticBackend { Puck(PuckWriter), Rumble(RumbleSink), None }`
  chosen from the active `Source`; `HapticCtx::fire` stays as is, and for
  `None` every `Feel` is dropped before any queue. The `hx.fire` call sites do
  not change.
- Phase 2 may map **only** `Haptic::Gesture` and `Haptic::Commit` to a short
  weak-motor tap (30–40 ms at ~30 %), and drop `Crossing`, `Scroll`, `Button`,
  `CursorMove` outright — a 250 Hz texture on a rumble motor is noise.
  Delivery: wired → `EVIOCSFF` + `EV_FF` play on the grabbed evdev fd
  (`FF_RUMBLE` is all `xpad`/`hid-microsoft` advertise); BT hidraw sidecar →
  the 9-byte report 3 (`03 enable magLT magRT magStrong magWeak dur10ms delay
  loops`, magnitudes 0..100).
- The game-rumble loop-back (`drive_rumble`, run.rs:1469) is untouched: while
  forwarding, a game's `FF_RUMBLE` on the virtual pad is relayed to the
  physical pad by the same sink; `RUMBLE_INTERVAL 50 ms` (run.rs:1318) matches
  SDL's `RUMBLE_BUSY_TIME_MS = 50` for Bluetooth Xbox pads.

---

## 4. Architecture

### 4.1 Where a second backend fits

```
hidraw::read_all  ─┐                                       (existing, 263 Hz)
evdev::read(fd)   ─┼─▶ Input::Frame { source, frame } ─▶ run loop ─▶ GestureEngine / drive_* / route_osk
timer 250 Hz      ─┘   Input::Tick                          (new: rate integrators)
```

`Input` (run.rs:94–109) gains `Frame { source: Source, frame: Frame }` and
`Tick`; the puck's forwarder decodes in the reader thread instead of the loop
(the two lines at run.rs:514–515 move into `forward_reports`, run.rs:672), so
both sources arrive symmetric. `ReadersEnded` becomes `SourceGone(Source)` so
one pad leaving does not put the loop into the reconnect wait if the other is
still there.

### 4.2 Crate choice — hand-rolled `libc` ioctls, with the `evdev` crate as the fallback

The repo deliberately hand-rolls uinput ("Hand-rolled like `keyboard.rs` (libc
only, no `input-linux` dependency)", gamepad.rs:66–70) and depends on `libc`
already. The evdev *read* side is `read(2)` of 24-byte `input_event` structs
plus five ioctls: `EVIOCGID`, `EVIOCGNAME`, `EVIOCGUNIQ`, `EVIOCGBIT`/`EVIOCGABS`
(or read the sysfs `capabilities/*` bitmaps, as `puck_nodes` reads `uevent`),
`EVIOCGRAB`, and later `EVIOCSFF`. ~250 lines with `SYN_DROPPED` resync
(re-read state with `EVIOCGKEY`/`EVIOCGABS`). If the team would rather not
own that: **`evdev` 0.13.2** (pure Rust, Apache-2.0 OR MIT, released
2025-09-15, actively maintained — `Device::open`, `fetch_events` with
`SYN_DROPPED` synchronisation, `grab()/ungrab()/is_grabbed()`, `input_id()`,
`name()/unique_name()/physical_path()`, `supported_keys()`,
`supported_absolute_axes()`, `get_abs_state()`, `upload_ff_effect()`,
`set_ff_gain()`; VERIFIED(docs.rs)). `evdev-rs` (libevdev binding) and
`udev` 0.9.3 (MIT, Smithay, libudev binding) add C libraries for no gain here.

### 4.3 Discovery, hotplug, and the loop-back hazard

Mirror `hidraw::puck_nodes` (hidraw.rs:22–38): walk `/sys/class/input/event*`,
read `device/name`, `device/id/{bustype,vendor,product}`, `device/uniq`,
`device/capabilities/{ev,key,abs}`, and **classify by capability** — `EV_ABS`
with `ABS_X`+`ABS_Y` and `EV_KEY` with `BTN_SOUTH` (0x130) — never by
`ID_INPUT_JOYSTICK` (§1.3.4). Exclusions, all VERIFIED(local) as present on
this machine:

| exclude | why | how |
|---|---|---|
| `hyprpad virtual gamepad` `045e:028e` (gamepad.rs:8, 139–140) | the daemon's **own output**; reading it back would feed forwarded frames into the loop | sysfs path under `/sys/devices/virtual/input/` (every uinput device) — never open a virtual node. Do not filter on `045e:028e` alone: that is also a real wired 360 pad |
| Steam's own "Microsoft X-Box 360 pad" uinput device | same loop-back through Steam | same rule |
| `hyprpad virtual keyboard` / `hyprpad-osk virtual keyboard` `1234:5678` | ours | same rule |
| the puck's lizard-mode `… Puck Mouse` / `… Puck Keyboard` nodes `28de:1304` | the puck is read over hidraw; its evdev nodes are firmware keyboard/mouse | vendor `28de` |

Hotplug: reuse the existing periodic rescan (`RECONNECT_SCAN_INTERVAL 1.5 s`,
run.rs:89, 413–431) — a `gamepad_nodes()` call beside `puck_readable()`. An
`inotify` watch on `/dev/input` (libc, no crate) is the later refinement; a
udev monitor is not worth its dependency.

The hidraw sidecar for the BLE Elite 2 (phase 2, §1.3.5 C) is the same
`puck_nodes` walk over `/sys/class/hidraw` matching `HID_ID=0005:0000045E:00000B22`
and a 464-byte descriptor, `read_all` verbatim, and a `Frame::decode_xbox_ble`
next to `Frame::decode` (report.rs:102). When both the evdev node and the hidraw
node of the *same* pad are present, prefer hidraw for input (paddles + slot)
and keep the evdev fd only for `EVIOCGRAB` (§4.6).

### 4.4 Multi-device coexistence

One pointer, one keyboard, one OSK, one mode engine: two drivers at once would
fight. **Last-active source wins.** The loop keeps `active: Option<Source>`;
a frame from a non-active source with any non-idle input (button down, stick
beyond `DEADZONE`, trigger > 0) switches the active source, running the same
`mode_handoff`/`release_all` (run.rs:818–838) for the outgoing one so nothing
is stranded. Idle frames from the inactive pad are dropped. Per-source state
that must not be shared: `prev_frame`, a `GestureEngine` (it holds `prev` and
guide state), and the rate integrators; shared: `ButtonKeys`, `CursorState`,
`ScrollState`, `OskRoute`, `GamepadState`. `StatusWriter` reports the active
source's name in `controller` (status.rs:62) and lists all present pads in a
new `sources` array.

### 4.5 The clock

A change-driven device stalls every integrator; the tick is not optional.
Spawn a 4 ms timer thread that sends `Input::Tick` **only while an Xbox source
is active and some rate axis is engaged** (stick outside the deadzone), so an
idle pad costs no wakeups — the docs/09 constraint "hyprpad itself must never
… busy-poll" holds. `recv_timeout` already exists in the loop (run.rs:413) for
the reconnect and rescan deadlines; the tick is a third deadline, not a new
mechanism.

### 4.6 `EVIOCGRAB`, and what Steam sees

- **Grab while adopted, ungrab on release/exit/disconnect** (`h.device { grab
  = true }` default). Benefits: stops the keyboard-classified BLE node leaking
  `KEY_RECORD`/`KEY_UNKNOWN` keystrokes into Hyprland (§1.3.4); for the wired
  `xpad` node it hides the physical pad from every evdev consumer — games then
  see only hyprpad's virtual `045e:028e` pad, the docs/06 Tier-1 shape,
  without any masking. `docs/06` §"What not to do" ("Do not `EVIOCGRAB` the
  controller expecting to hide it from Steam") is about *Valve* hardware over
  hidraw; it does not apply to `xpad`, which has no hidraw node at all.
- **Over Bluetooth a grab does nothing to Steam**: Steam reads `hidraw12`
  (VERIFIED(local)). The uhid research's §8.3 / W12 story applies verbatim —
  either the bwrap mask (`scripts/steam-masked`, currently matching only
  `28DE:1304`, would need `045E:0B22`) or the udev override from
  `docs/research/uhid-steam-controller.md` §8.3 — **or the cheap answer**:
  Steam → Settings → Controller → **disable "Xbox controller support"**
  (the per-type toggles documented on Valve's Steam Input pages). With it off
  Steam neither configures the pad nor emits its desktop mouse/keyboard for
  it; SDL games launched by Steam may still open `hidraw12` through HIDAPI
  (the steam-devices rule grants it), so a title that sees both the real pad
  and hyprpad's virtual one is the remaining hazard — the mask closes it.
- The Xbox button reaches hyprpad and Steam simultaneously over BT, as the
  Steam button does on the puck; `forward_guide = false` (config.rs:668–672)
  keeps chords off the virtual pad.

### 4.7 Permissions

- `/dev/input/event*`: `root:input 0660`, no uaccess for this pad. Works here
  because the user is in `input` (VERIFIED). On a machine without that, ship
  `SUBSYSTEM=="input", ATTRS{id/vendor}=="045e", TAG+="uaccess"` beside
  `scripts/99-hyprpad-claim-puck.rules` (the joystick-tag form would not match
  a pad udev calls a keyboard).
- `/dev/hidraw12`: `uaccess` from `60-steam-input.rules` (VERIFIED) — the
  sidecar needs nothing extra wherever `steam-devices` is installed; add the
  same `KERNELS=="*045E:0B22*"` line to hyprpad's rules for machines without it.
- `/dev/uinput` for the virtual pad: unchanged.

### 4.8 Config surface

Automatic by default — a connected pad is adopted with no config — and a
small, optional table in the Lua front-end (registered like `section_cursor`
etc., lua_config.rs:778–782):

```lua
h.device {                      -- optional; defaults shown
  sources  = { "puck", "xbox" }, -- which backends to arm
  grab     = true,               -- EVIOCGRAB the evdev node while adopted
  paddles  = "profile0",         -- "profile0" | "always" (BPF path cannot read the slot)
}
h.cursor { stick = { deadzone = 0.12, outer = 0.95, curve = 2.0, max_speed = 1500 } }
h.scroll { stick = { max_notches_per_s = 12, curve = 2.0 } }
h.osk    { stick_cursors = true }   -- option (i); false once (ii) lands
```

Bindings need no new names (§2). `h.haptics` gains `rumble_taps = false`
(phase 2).

---

## 5. Cheat sheet

`Panel.qml` "names no controller anywhere; the layout is chosen by `layoutId`,
overridable per summon" (`shell/README.md`; `Panel.qml:50, 66–71, 103`). The
deliverable is `shell/hyprpad.cheatsheet/layouts/xbox-elite-2.json`:

- **`controls`**: the same ids the daemon emits (`bindings_sheet.rs:472–507`):
  `a b x y dpad(+4) l1 r1 l2 r2 l3 r3 lstick rstick view menu steam l4 l5 r4 r5`,
  with `l2 r2 l4 l5 r4 r5` `hidden: true` (back/top of the pad, dashed
  leaders). `lpad rpad lpad_click rpad_click quickaccess` are **omitted**; the
  widget already parks bindings it cannot anchor in `overflow`
  (`Sheet.qml:113`), so a puck-flavoured config summoned with the Elite layout
  degrades gracefully. A `labels` map (`l4: "P3", l5: "P4", r4: "P1", r5:
  "P2"`, `steam: "Xbox"`) is a small widget addition — today
  `button_label` is Rust-side (`bindings_sheet.rs:511`) and says "L4 grip".
- **`glyphs`**: Kenney Input Prompts 1.5A is CC0 and covers Xbox with the same
  `<platform>_<control>.svg` naming (`art/LICENSES.md`); the vendored subset has
  only the Steam set plus the generic `controller_button_{l4,l5,r4,r5,view,
  options}.svg`. Add the Xbox glyphs (`xbox_button_a/b/x/y`, `xbox_lb/rb/lt/rt`,
  `xbox_ls/rs`, `xbox_stick_l/r`, `xbox_dpad*`, `xbox_button_view/menu`,
  `xbox_guide` — names INFERRED from the pack's convention; verify on copy)
  and reuse the generic grip glyphs for P1–P4. The `guide` modifier glyph
  becomes the Xbox logo with legend "hold Xbox, then press".
- **`art`**: Valve's `steamui/images/controller/controller_config_controller_xboxelite.png`
  exists in the local install (VERIFIED(local)) but is a **PNG**; the widget
  recolours SVG text (`Callouts.recolor()`, `shell/README.md`) and cannot
  recolour a raster. So the Elite needs hyprpad's own schematic SVG — a
  copy-and-adapt of `art/steam-controller-2026.svg` with sticks swapped to
  the Xbox's asymmetric positions and the four paddles drawn as dashed
  outlines on the grips — with the Steam PNG as a possible later raster
  fallback if the widget learns to show one unrecoloured.
- **Which controller is connected**: extend `status.json` with
  `"layout": "xbox-elite-2"` (and `"sources": [...]`), written by
  `StatusWriter::set_connected` beside `controller` (status.rs:185–193); the
  bar widget already parses this file (`Widget.qml:54, 68–70`), and `Panel.qml`
  reads it on `open()` when the summon payload does not pin a `layout`.
  `hyprpad bindings --json` stays device-free (README: "neither touches the
  device"), so the *daemon* is the only writer of the layout id, which keeps
  the "sheet cannot drift from the daemon" property.

---

## 6. Plan

| phase | scope | effort | exit criterion |
|---|---|---|---|
| **0 — prep** | Owner: profile slot 0; either cable or `pacman -S udev-hid-bpf` + verify the Elite 2 program loaded; optionally disable Xbox support in Steam. `hyprpad monitor --evdev` (extend `main.rs:monitor`) prints decoded frames from the new backend without touching the daemon. | 0.5 d | four paddles print as `GripL4/L5/R4/R5` on one of the two paths |
| **1 — evdev backend** | `src/evdev.rs` (discovery, exclusions, reader thread, `SYN_DROPPED`, grab); `Source` on `Frame`; `Input::Frame`/`Tick`/`SourceGone`; both paddle code sets; hat → d-pad; trigger thresholds; stick normalisation; `StickCursor` + stick scroll; OSK option (i) over `cursor L\|R`; `HapticBackend::None`; last-active-wins; `status.json` `layout`/`sources`; rescan-based hotplug. | 3–5 d | the owner's `config/hyprpad.lua` works unchanged on the Elite (wired, or BT with BPF) minus pad-only bindings; workspace flicks, guide chords incl. `guide+r4/r5`, D-pad arrows, A/B, trigger clicks, stick cursor and scroll, typing through the OSK with two stick cursors |
| **2 — Bluetooth polish + feel** | hidraw sidecar for `045e:0b22` (paddles + slot without system packages; report-3 rumble); `RumbleSink` taps for Gesture/Commit and the game-rumble relay over BT; OSK option (ii) snap navigation (`osk/` focus model); `xbox-elite-2.json` layout, Kenney Xbox glyphs, the schematic SVG; `Panel.qml` layout from `status.json`; inotify hotplug; bluez interval note in docs. | 3–4 d | BT with stock kernel and no extra package gives paddles; sheet shows the Xbox tab; snap typing |
| 3 — generalisation (optional) | DualSense/Switch Pro through the same evdev backend (docs/06 success criterion 4); `ABS_PROFILE` from xpadneo when present; precision modifier. | 1–2 d each | — |

**Risks.**

- *Two code sets for one control* (`BTN_GRIP*` vs `BTN_TRIGGER_HAPPY5..8`) — a
  future `udev-hid-bpf` may switch to `BTN_GRIP*` too; accept both forever.
- *Profile slots on the BPF path* double-fire; only slot 0 is supported and
  the sidecar (phase 2) is the fix.
- *Change-driven reports* — any place that assumed "a frame every 4 ms"
  (hold timers, `drive_buttons` reconcile after a gate change) must be checked
  against the tick; `mode_handoff` already releases without waiting for a frame.
- *Steam double-driving the desktop* while Xbox support is enabled in Steam
  and hyprpad is also adopting the pad; the phase-0 toggle or the mask.
- *libinput keyboard adoption* of the BLE node before hyprpad grabs it — a
  Profile-button press between connect and grab types `KEY_RECORD`; the
  compositor device rule closes it.
- *BLE latency* on this dongle is unmeasured; xpadneo's interval workaround is
  system-wide.
- *Battery*: a 250 Hz tick while a stick is deflected is cheap; keep it gated.

**Open questions.**

1. Does the Arch `udev-hid-bpf` package ship `Microsoft__Xbox-Elite-2.bpf.o`?
   (Install and list; or build the in-tree program.)
2. Live confirmation that the BLE firmware 5.21 report is 20 bytes with the
   layout in §1.3.2 (SDL and xpadneo both say so; capture one report with
   `hyprpad monitor --hidraw 045e:0b22` when the pad is awake).
3. What the trigger-lock nibble reports and whether a locked trigger's reported
   range shrinks (affects the full-pull threshold).
4. Whether Steam's evdev fallback opens a keyboard-classified node when
   `hidraw12` is masked (matters only if the mask route is chosen over the
   Steam toggle).
5. The exact factory paddle→button defaults in slots 1–3 (only for a warning
   message; the design does not depend on them).
6. Steam Input's numeric curve definitions ("Aggressive", "Relaxed", …) are
   not published; the exponent knob covers the space.

---

## 7. Sources

**This repository (VERIFIED(repo)).**
`src/report.rs:12–43, 78–96, 99–121` · `src/hidraw.rs:22–38, 46–58, 69–101` ·
`src/run.rs:89, 94–109, 413–431, 514–515, 661–679, 736–750, 765–775, 783–838,
895–939, 1011–1103, 1111–1170, 1235–1252, 1318, 1348–1357, 1428–1505, 2037–2097,
2175–2258` · `src/gesture.rs:63–75, 110–198, 211–217` ·
`src/filter.rs:39, 168–286, 318` · `src/gamepad.rs:1–70, 109–141, 364` ·
`src/config.rs:302–368, 444–468, 524–538, 663–675` · `src/status.rs:36, 53–62,
185–193, 275–295` · `src/bindings_sheet.rs:472–542, 596–615` ·
`src/lua_config.rs:778–804` · `src/osk.rs:7, 119–143` · `osk/src/render.rs:19–36` ·
`osk/src/layout.rs:664` · `osk/README.md` · `shell/hyprpad.cheatsheet/Panel.qml:50, 66–71, 103–166` ·
`shell/hyprpad.cheatsheet/Sheet.qml:113` · `shell/hyprpad.cheatsheet/layouts/steam-controller-2026.json` ·
`shell/hyprpad.cheatsheet/art/LICENSES.md` · `shell/hyprpad.status/Widget.qml:54–70` ·
`shell/README.md` · `scripts/steam-masked` · `scripts/99-hyprpad-claim-puck.rules` ·
`docs/02-background-linux-input.md:39–70` · `docs/05-architectures.md:117–147` ·
`docs/06-recommendation.md:196–202, 226–239` · `docs/09-programme.md:122, 243` ·
`docs/research/uhid-steam-controller.md` §8.3 · `docs/experiments/w12-device-denial.md` ·
`docs/research/osk-technology.md` §4.4–4.5 · `docs/research/pointer-damping.md` §1–2 ·
`docs/research/haptics.md` §0.

**Linux kernel (`torvalds/linux`, fetched 2026-09-01).**
- `drivers/input/joystick/xpad.c` (master): device table :129–134; `MAP_PADDLES` :50;
  `xpad_btn_paddles` :461–464; `xboxone_s_init` :641–649; `extra_input_packet_init` :651–658;
  `xboxone_init_packets` :713–728; `xpadone_process_packet` (`GIP_CMD_VIRTUAL_KEY` → `BTN_MODE`,
  `GIP_CMD_FIRMWARE` paddles :1054–1071, per-packet-type paddles :1153–1198);
  packet-type detection :2154–2184; `xpad_set_up_abs`. Same content at tag `v7.1`
  (`xpad_btn_paddles` :460–464).
  https://raw.githubusercontent.com/torvalds/linux/master/drivers/input/joystick/xpad.c
- Commits: `e23c69e33248` "Input: xpad - add support for XBOX One Elite paddles" (2022-08-18, 6.1);
  `e7412ba919f6` "Input: xpad - use new BTN_GRIP* buttons" (2025-07-27, 6.17);
  `a43a503df996` "Input: xpad - change buttons the D-Pad gets mapped as to BTN_DPAD_*";
  `fff1011a26d6` "Input: xpad - add X-Box Adaptive Profile button".
- `drivers/hid/hid-microsoft.c` :29, :42–58, :281–324, :427–436;
  `drivers/hid/hid-ids.h` :1050–1059;
  `drivers/hid/hid-input.c` :795–810, :841–842, :1201, :1282, :1321, :1787;
  `drivers/hid/bpf/progs/Microsoft__Xbox-Elite-2.bpf.c` (whole file);
  `include/uapi/linux/input-event-codes.h` :605–608, :798–801;
  `Documentation/input/gamepad.rst` "Grip buttons", "Profile", `BTN_MODE`.
- Phoronix, "Linux 6.1 To Have Working Support For Xbox Elite Paddles"
  https://www.phoronix.com/news/Linux-6.1-XPad

**xpadneo (`atar-axis/xpadneo` master).** `docs/README.md` ("Paddle Button
Codes Changed", "BLE firmware", "Xbox Elite Series 2", "Profile Switching");
`docs/TROUBLESHOOTING.md` ("High Latency or Lost Button Events with Bluetooth
LE"); `docs/SDL.md`; `hid-xpadneo/src/xpadneo/mappings.c:34–66, 115–124`;
`events.c:96–142, 162–170, 338–351`; `mouse.c:22, 45–69, 95–121`;
`docs/descriptors/xbe2_linux.md`. https://github.com/atar-axis/xpadneo ·
https://atar-axis.github.io/xpadneo/ · issue #332 "Xbox Elite Series 2
Controller: Paddle Support".

**xone (`medusalix/xone` master).** `README.md` :11–19, :104–108; `driver/gamepad.c` (Share only).

**SDL (`libsdl-org/SDL` main).** `include/SDL3/SDL_gamepad.h:172–175`;
`src/joystick/usb_ids.h:197–200`; `src/joystick/SDL_joystick.c` `SDL_IsJoystickBluetoothXboxOne`;
`src/joystick/hidapi/SDL_hidapi_xboxone.c` (`HIDAPI_DriverXboxOne_HandleUnmappedStatePacket`,
`HIDAPI_DriverXboxOneBluetooth_HandleButtons` size-20 paddle block, BT rumble packet, `RUMBLE_BUSY_TIME_MS`);
`src/joystick/linux/SDL_sysjoystick.c:78–88`; `src/core/linux/SDL_evdev_capabilities.c:122, 158–162`.
SDL Discourse, "Which paddle buttons correspond to which positions?"
https://discourse.libsdl.org/t/which-paddle-buttons-correspond-to-which-positions/28710

**systemd.** `src/udev/udev-builtin-input_id.c` "Joystick un-detection" (:277–310).
Local: `/usr/lib/udev/rules.d/60-steam-input.rules` ("Xbox One Elite 2 Controller" rule),
`70-uaccess.rules`, `70-joystick.rules`.

**Steam Input.** SteamInputWiki `chapter-2/02b05_joystick_mouse.md`,
`02b07_joystick_camera.md`, `02b04_mouse_joystick.md`
https://github.com/SteamInputWiki/SteamInputWiki · Valve, "Steam Input —
Getting Started for Players" (controller-type toggles)
https://partner.steamgames.com/doc/features/steam_controller/getting_started_for_players ·
steam-for-linux #8352 (BT Series X\|S misdetection) · local
`~/.local/share/Steam/logs/controller.txt`.

**Gamepad-mouse implementations.** AntiMicroX `src/joybuttontypes/joybutton.cpp:1033–1174, 1322`,
`src/globalvariables.cpp:30–38, 60–87, 116, 206`, wiki "Mouse Settings"
https://github.com/AntiMicroX/antimicrox/wiki/Mouse-Settings · xpadneo `mouse.c` (above).

**Console keyboards.** Xbox Wire, "Keyboard Button Mapping for Xbox Controllers" (2023-08-03)
https://news.xbox.com/en-us/2023/08/03/keyboard-button-mapping-for-xbox-controllers/ ·
Microsoft Q&A, "How do I use an onscreen keyboard in game with a gamepad" · Steam Deck via
`docs/research/osk-technology.md` §4.4.

**Rust crates.** `evdev` 0.13.2 https://docs.rs/evdev (Apache-2.0 OR MIT) ·
`evdev-rs` 0.6.3 https://docs.rs/evdev-rs · `udev` 0.9.3 https://crates.io/crates/udev (MIT).

**Local evidence (2026-09-01 22:55–23:15).** `/proc/bus/input/devices`;
`/sys/devices/virtual/misc/uhid/0005:045E:0B22.0074/{uevent,driver,report_descriptor}`;
`/sys/class/input/input242/{uevent,capabilities/*}`; `udevadm info` on `event31`, `js0`,
`hidraw12`, `input242`; `udevadm test-builtin input_id`; `getfacl`; `id`; `lsmod`;
`modinfo xpad hid_microsoft`; `/proc/config.gz`; `pacman -Q/-Si/-Ss`;
`bluetoothctl info 00:11:22:33:44:66`; `/etc/bluetooth/main.conf`;
`$XDG_RUNTIME_DIR/hypr/*/hyprland.log`; `hyprctl devices -j`;
`~/.local/share/Steam/steamui/images/controller/`; `journalctl --user`
(voxtype transcript of the owner's request, 22:55–22:57).
