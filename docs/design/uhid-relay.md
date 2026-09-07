# Design: the uhid relay — a virtual Valve controller Steam adopts

*Implements the device-independent core of `docs/research/uhid-steam-controller.md`.
Code: `src/uhid.rs` and `src/uhid/{profile,translate,settings,relay}.rs`, wired
into `src/run.rs` beside `src/gamepad.rs`. The privileged host half — the udev
rule, the root fd broker and the systemd units that make Steam see the fake and
only the fake — is §6, and lives in `src/broker.rs` and `packaging/`.*

## What it is

hyprpad owns the 2026 Steam Controller — reached through the puck, `28de:1304` —
exclusively, and today
hands games a synthesized Xbox-360 pad. That works, and it discards everything
Steam Input adds on a *Valve* controller: trackpads as trackpads, gyro, per-game
configs, Steam-driven haptics, the four back grips.

This feature creates a **second sink**: a virtual HID device on `/dev/uhid`
carrying a Valve VID/PID, which Steam adopts as a genuine Steam Controller.
hyprpad streams the real controller's input into it and interprets Steam's writes back
out onto the real hardware.

It is **opt-in and off by default**. One config line turns it on:

```toml
[gamepad]
kind = "steam"          # xbox (default) | steam
identity = "triton"     # triton (default) | deck
gyro = false            # hold the IMU on regardless of Steam (diagnosis; §3.1)
```

```lua
h.gamepad { kind = "steam", identity = "triton" }
```

Nothing changes for an existing config: `kind = "xbox"` is the default, and with
it the daemon never opens `/dev/uhid` at all.

**The mechanism is proven.** On 2026-09-01 an active probe
(`scripts/research/uhid_active_probe.py`) presented a `28de:12f0` uhid device to
a normally-running Steam. Steam opened it, logged
`!! Steam controller device opened for index 5.`, loaded `configset_neptune.vdf`
for it, listed it under Settings → Controller, and sent it **39
`SetSettingsValues` writes**. See the "PROVEN on-device" section of the research
doc.

---

## 1. The two profiles

The identity is **data**, not a constant (`src/uhid/profile.rs`), and two ship.

| | `triton` — the default | `deck` — the proven fallback |
|---|---|---|
| VID:PID | `28de:1302`, the *wired* single-interface Steam Controller | `28de:12f0`, InputPlumber's `ProductId::Generic` |
| `name` | `Valve Software Steam Controller` | `Steam Controller` |
| `uniq` (Steam's config key) | `FXA0000000001`, the captured unit's serial — pinned | `HYPRPAD-DECK-0001`, a fixed synthetic — pinned (§1.3) |
| `phys` | `hyprpad-uhid/1302` | `""` |
| `version` | `0x0307` (bcdDevice 307, as Steam logged for the real unit) | `0x1000` |
| `bus` | `BUS_USB` | `BUS_USB` |
| Descriptor | the real 372-byte capture, `docs/research/assets/triton-wired-1302-report-descriptor.bin` | InputPlumber's 38-byte vendor-only blob |
| Report IDs | **yes**, on input, output *and* feature | **none at all** |
| Input report | `0x42`, 54 bytes — **pass-through** | 64 bytes — **transcoded** |
| Steam adoption | **UNPROVEN** — awaits the owner's live test | **PROVEN on-device, 2026-09-01** |

*The hardware serials quoted in this document — `FXA0000000001` for the
controller, `FXB0000000002` for the puck — are synthetic stand-ins of the same
shape as the captured ones. No real unit's serial is published in this repo.*

Neither is `28de:1304`, the puck's own PID, and neither can be: Steam derives the
controller *slot index* from `bInterfaceNumber` for the dongle PIDs and SDL gates
`0x1304` on `interface_number` being 2..=5, while a uhid device reports `-1`
(research §3.2).

Both declare `BUS_USB`, never `BUS_VIRTUAL`. SDL's hidraw backend drops any bus
that is not USB/Bluetooth/I2C/SPI, so a `BUS_VIRTUAL` device is never enumerated
at all — and the proven run settled it on-device. (This supersedes the
`BUS_VIRTUAL` suggestion in `docs/research/assets/triton-wired-1302-identity.md`,
which predates that finding.)

### Why `triton` is the default even though `deck` is the proven one

Because it is the **least translation**, and hyprpad is the only project in a
position to take it — everyone else (InputPlumber, hhd) *synthesises* a Valve
controller from other hardware, whereas hyprpad has a real Triton behind the
clone.

Walking the captured 372-byte `1302` descriptor gives its **whole** report
table. Every framing decision below is read off it, and
`profile::tests::the_triton_descriptor_report_table_is_pinned` pins it so a
re-capture that changes any row fails the build rather than silently reframing
the wire. Wire length is always payload + 1, because the report id goes in
front.

| Collection | Type | Id | Payload | Wire |
|---|---|---|---|---|
| lizard Mouse (Generic Desktop) | Input | `0x40` | 5 | 6 |
| lizard Keyboard | Input | `0x41` | 8 | 9 |
| vendor `0xFF00` | Input | **`0x42`** | **53** | **54** |
| | Input | `0x44` / `0x79` / `0x43` / `0x7b` / `0x45` | 5 / 1 / 14 / 12 / 45 | +1 each |
| | Output | `0x80` rumble | 9 | 10 |
| | Output | `0x81` haptic pulse | 7 | 8 |
| | Output | `0x82` / `0x83` / `0x84` / `0x85` / `0x86` | 3 / 9 / 8 / 3 / 3 | +1 each |
| | Output | `0x87` / `0x89` / `0x88` | 63 | 64 |
| | Feature | **`0x01`** — the Valve control channel | **63** | **64** |
| | Feature | `0x02` | 63 | 64 |

(The `0x40` row is why the walker accumulates **bits** and rounds once, exactly
as the kernel's `hid_compute_report_size` does: that report's first byte is a
`2 + 6`-bit split, and summing byte-rounded main items loses it. The earlier
capture note's "report id `0x40` (64-byte reports)" was a misreading — `0x40` is
the lizard mouse, and the Steam protocol is on `0x42` / `0x01`.)

Those are, byte for byte, the reports `src/report.rs` already decodes and
`src/lizard.rs` / `src/haptics.rs` already write. **The wired `1302` speaks the
controller's own protocol**, so the input path is a copy, not a transcode — and every
field hyprpad does not model rides along untouched, including the IMU at bytes
30+ and the undecoded tail. That is the standing advantage over `deck`, which
forfeits it.

### 1.1 Framing: what numbered reports actually oblige us to do

`profile::Framing` carries this per profile, and it is the half of an identity
Steam's *parser* sees.

**There is nothing to switch on.** Numbering is a property of the descriptor and
the kernel derives it: `uhid_hid_start` (`drivers/hid/uhid.c`) walks the
descriptor it was handed and raises `UHID_DEV_NUMBERED_{FEATURE,OUTPUT,INPUT}_REPORTS`
from `hid->report_enum[…].numbered`. Those flags live in `struct uhid_start_req`
and travel **kernel → user space** in `UHID_START`; `struct uhid_create2_req`
(`include/uapi/linux/uhid.h`) has no flags field at all. Publishing the numbered
372-byte descriptor *is* the request, so `Framing::expected_dev_flags` is checked
against what `UHID_START` reports (`0b111` for triton, `0` for deck) and a
mismatch is warned about at startup.

What user space must then do is match it, because **uhid neither inserts nor
strips the leading report-number byte on any channel**:

| Channel | Kernel | Consequence |
|---|---|---|
| Input | `uhid_dev_input2` → `hid_report_raw_event`, which takes `data[0]` as the report id when the input enum is numbered | triton emits `[0x42][53 bytes]`; deck emits a bare 64 |
| Feature `GET` | `uhid_hid_get_report`: `ret = min3(count, req->size, UHID_DATA_MAX); memcpy(buf, req->data, ret)` — then `hidraw_get_report` copies that to the caller verbatim | the **entire** buffer, byte 0 included, is ours to build |
| Feature `SET` | `uhid_hid_set_report` copies hidraw's whole buffer, report-number byte and all | the Valve command id is `data[1]` — what `HostWrite::command` strips for |

The one that was wrong: **a real `1302` answers a 65-byte request with 65
bytes.** Real hardware is framed by `usbhid_get_raw_report`
(`drivers/hid/usbhid/hid-core.c`), which uhid does not emulate:

```c
/* Byte 0 is the report number. Report data starts at byte 1.*/
buf[0] = report_number;
if (report_number == 0x0) {
        /* Offset the return buffer by 1, so that the report ID
           will remain in byte 0. */
        buf++; count--; skipped_report_id = 1;
}
ret = usb_control_msg(…, buf, count, …);
/* count also the report id */
if (ret > 0 && skipped_report_id)
        ret++;
```

Valve's own client asks for exactly that much and says why:
`src/joystick/hidapi/SDL_hidapi_steam.c` declares `unsigned char buf[65]` under
the comment *"Firmware quirk: Set Feature and Get Feature requests always
require a 65-byte buffer"*, sends with `SDL_hid_send_feature_report(dev,
uBuffer, 65)`, reads with `SDL_hid_get_feature_report(dev, uBuffer, 65)`, and
validates the answer at `uBuffer[1] != nExpectedResponse` — never `[0]` — then
bounds-checks `nAttributesLength = buf[2]` against the returned byte count.

So triton's `GET_REPORT` answer is **65 bytes**: the report number echoed into
byte 0 the way `usbhid_get_raw_report` does it, then the 64-byte Valve message.
The message content is unchanged; only the frame around it grew, from
`[id byte][63-byte message]` to `[id byte][64-byte message]`.

`deck` keeps its 64 bytes untouched — one byte shorter than a real USB device
would return, and pinned anyway, because those are the bytes Steam adopted on
2026-09-01. Changing a proven wire to satisfy a theory is how you lose the
fallback.

### 1.2 What is still unsettled

Risk **R1**: whether Steam accepts a `1302` whose `Interface:` reads `-1`
because a uhid device has no USB parent (research §3.2, §6 R1, Q-u1).

And the framing fix above is **inferred, not observed**: it makes the wire
byte-for-byte what a real `1302` puts there, but nobody has watched Steam accept
it. The failure it targets is the 2026-09-02 run where Steam opened the fake,
looped `CGetControllerInfoWorkItem::RunFunc: Read failure.` and
`Warning, couldn't get controller details for SC, PID=4866` several times a
second, and re-opened the device every 15 s — with the daemon answering every
`GetAttributesValues` correctly except that its answer was 64 bytes long.

> **If `triton` is not adopted, the fallback is one line.** Set
> `identity = "deck"` and **restart the daemon** — `identity` chooses a device
> created once at startup, so unlike the rest of `[gamepad]` it is not picked up
> by `hyprpad reload`. That path is the exact recipe SteamOS ships, and the one
> this machine has already run.

### 1.3 The unit serial, and which field Steam actually reads

On 2026-09-02 the `deck` profile was adopted and Steam immediately said:

```text
Local Device Found
  type: 28de 12f0
  path: /dev/hidraw12
  serial_number:  - 0
  Interface:    -1

Unrecognized controller using V1 HID protocol
!! Steam controller device opened for index 0.
Steam Controller reserving XInput slot 0
Controller has an Invalid or missing unit serial number, setting to '28de-12f0-3147b8f'
ConfigSet - found config set file on-disk: …/configset_28de-12f0-3147b8f.vdf
```

**The field it read is `uniq`, not the `0xAE` answer** — which matters, because
the two are different code paths and only one of them was empty. Three things
settle it:

* the `serial_number:` line comes from **hidapi's enumeration**, whose hidraw
  backend parses `HID_UNIQ` out of the node's `uevent` — and `uniq` is what
  `struct uhid_create2_req` puts there. On this machine the live triton fake
  reads `HID_UNIQ=FXA0000000001`;
* the complaint lands **immediately on open**, one line after `!! Steam
  controller device opened`, before any feature-report round trip could have
  happened;
* the `triton` profile, whose `uniq` *is* set, is enumerated as
  `serial_number: FXA0000000001`, loads `configset_FXA0000000001.vdf`, and draws
  no complaint at all.

The canned `0xAE` answer said `1NPU7PLUMB3R` (InputPlumber's joke serial)
throughout, on both the complaining run and the quiet one, and changed nothing.

#### What it is now, and why not something else

`deck`'s `uniq` is `HYPRPAD-DECK-0001`, and the `0xAE` answer says the same
thing so the device does not state two different serials on two channels — the
self-consistency rule `TRITON_ATTRIBUTES` already follows for the product id
(risk R3), applied to the serial.

| Candidate | Rejected because |
|---|---|
| `""` (InputPlumber's) | this is the bug |
| the controller's or the puck's own hardware serial | ships one machine's hardware identity to everyone who builds hyprpad — and colliding with `triton`'s key would apply a Deck-shaped binding set to a Triton-shaped device. The serials this tree states (`FXA0000000001`, `FXB0000000002`) are synthetic stand-ins for exactly that reason |
| `1NPU7PLUMB3R`, the `0xAE` answer's old value | not distinct from InputPlumber's own devices, and not obviously ours in a log |
| **`HYPRPAD-DECK-0001`** | **stable** (Steam keys `configset_<serial>.vdf` on it, so bindings must survive a restart), **distinct** from `triton`'s, and **obviously ours** in a Steam log |

The framing is unchanged and was checked against SDL rather than against itself:
SDL validates `uBuffer[1] != nExpectedResponse` — never byte 0 — and
bounds-checks the declared length against the bytes it read, so the answer stays
`[0x00][0xAE][0x14][0x01]` + serial + NULs. `0x01` is `ATTRIB_STR_UNIT_SERIAL`
in SDL's `ControllerStringAttributes` (`ATTRIB_STR_BOARD_SERIAL` is `0`), which
is precisely the attribute Steam's message names.

#### The cost

**Steam's config key for this identity changes**, from the `28de-12f0-3147b8f`
Steam invented to one derived from this string. Any Steam Input binding already
saved against the old key is orphaned and has to be redone once. That is a
one-time cost on a fallback identity, paid to stop Steam inventing a key it
might not invent identically elsewhere — and it is why this value must never
change again.

Everything else about `deck` is still the proven run's, byte for byte:
descriptor, attributes blob, chip id, framing, name, phys, PID, version,
country. `uhid::tests::create2_matches_the_proven_probe_but_for_the_deck_serial`
zeroes the `uniq` slot in both and asserts the rest is identical, so a second
departure cannot slip in behind this one.

---

## 2. Report translation

### 2.1 `triton` — pass-through (`translate::controller_to_triton`)

The raw 54-byte `0x42` is copied verbatim into `UHID_INPUT2`. Two things are
changed, and only these:

| What | Where | Rule |
|---|---|---|
| Steam / guide button | byte 4, bit 0 | Cleared unless `[gamepad] forward_guide` is on. The guide is hyprpad's global chord modifier; without this every `guide+x` chord would *also* open the Steam overlay (research §4.4). |
| Quick Access | byte 2, bit 4 | Available in `StripMask`, **not currently stripped** — see §7. |

The byte-1 sequence counter is **relayed untouched**. §4.4 is explicit:
renumbering risks desync with the IMU timestamp path, and Steam tolerates gaps.

There is a third change, and it goes the other way: the streamer **sets** the
guide bit for 15 consecutive ticks (60 ms) when the daemon asks it to
(`SteamRelay::pulse_guide`), on top of whatever it is otherwise streaming —
live frame or neutral. That is the synthesized *bare guide tap*. `forward_guide`
cannot deliver one however it is set, because no frame with the guide down is
ever forwarded at all (the guide layer outranks game forwarding, so those ticks
are neutral by rank); a quick press-and-release that meant nothing else is
therefore replayed after the fact, on the release Steam acts on. The deck
profile does the same thing at byte 9, bit 5. See `[gamepad] guide_tap`.

Because the `1302` descriptor numbers every report type, all three `UHID_START`
`dev_flags` bits are set and the report-id prefix is required in both directions
— which the controller's own `raw[0] == 0x42` already provides. The pass-through is
correct *because* of that, not in spite of it.

### 2.2 `deck` — transcode (`translate::controller_to_deck`)

Into InputPlumber's `PackedInputDataReport`
(`src/drivers/steam_deck/hid_report.rs`, `bit_numbering = "msb0"`,
`size_bytes = "64"`). Bit *N* of that struct is byte `N / 8`, mask
`0x80 >> (N % 8)`.

**Header and counter**

| Deck field | Bytes | Value |
|---|---|---|
| `major_ver` | 0 | `0x01` |
| `minor_ver` | 1 | `0x00` |
| `report_type` | 2 | `0x09` (`InputData`) |
| `report_size` | 3 | `0x40` (64) |
| `frame` | 4..8 | u32 LE — the stream's tick counter |

**Buttons** — controller `report::Button` → Deck field, byte and mask

| Controller button | Deck field | Byte | Mask | Bit |
|---|---|---|---|---|
| `A` | `a` | 8 | `0x80` | 64 |
| `X` | `x` | 8 | `0x40` | 65 |
| `B` | `b` | 8 | `0x20` | 66 |
| `Y` | `y` | 8 | `0x10` | 67 |
| `BumperL1` | `l1` | 8 | `0x08` | 68 |
| `BumperR1` | `r1` | 8 | `0x04` | 69 |
| `TriggerL2Full` | `l2` | 8 | `0x02` | 70 |
| `TriggerR2Full` | `r2` | 8 | `0x01` | 71 |
| `GripL5` | `l5` | 9 | `0x80` | 72 |
| `Menu` | `menu` | 9 | `0x40` | 73 |
| `Steam` | `steam` | 9 | `0x20` | 74 |
| `View` | `options` | 9 | `0x10` | 75 |
| `DpadDown` | `down` | 9 | `0x08` | 76 |
| `DpadLeft` | `left` | 9 | `0x04` | 77 |
| `DpadRight` | `right` | 9 | `0x02` | 78 |
| `DpadUp` | `up` | 9 | `0x01` | 79 |
| `L3` | `l3` | 10 | `0x40` | 81 |
| `PadRightTouch` | `r_pad_touch` | 10 | `0x10` | 83 |
| `PadLeftTouch` | `l_pad_touch` | 10 | `0x08` | 84 |
| `PadRightClick` | `r_pad_press` | 10 | `0x04` | 85 |
| `PadLeftClick` | `l_pad_press` | 10 | `0x02` | 86 |
| `GripR5` | `r5` | 10 | `0x01` | 87 |
| `R3` | `r3` | 11 | `0x04` | 93 |
| `GripR4` | `r4` | 13 | `0x04` | 109 |
| `GripL4` | `l4` | 13 | `0x02` | 110 |
| `QuickAccess` | `quick_access` | 14 | `0x04` | 117 |

Note the Deck's face order is `a, x, b, y` — not `a, b, x, y`. All four back
grips reach Steam, which the Xbox pad forwards none of.

**Analog**

| Controller field | Deck field | Bytes | Encoding |
|---|---|---|---|
| `left_pad.x` / `.y` | `l_pad_x` / `l_pad_y` | 16..20 | i16 LE |
| `right_pad.x` / `.y` | `r_pad_x` / `r_pad_y` | 20..24 | i16 LE |
| `l2` | `l_trigg` | 44..46 | u16 LE, 0..=32767 both ends |
| `r2` | `r_trigg` | 46..48 | u16 LE |
| `left_stick.0` / `.1` | `l_stick_x` / `l_stick_y` | 48..52 | i16 LE, +x right, +y up |
| `right_stick.0` / `.1` | `r_stick_x` / `r_stick_y` | 52..56 | i16 LE |
| `left_pad.force` | `l_pad_force` | 56..58 | u16 LE |
| `right_pad.force` | `r_pad_force` | 58..60 | u16 LE |

**Deliberately left zero**

| Deck field | Bytes | Why |
|---|---|---|
| `accel_x/y/z` | 24..30 | `report::Frame` carries no IMU. The controller's `0x42` has it at bytes 30+ but only after an enable feature report, and `src/report.rs` leaves those bytes undecoded. This is the cost of `deck`; `triton` gets them for free. |
| `pitch`, `yaw`, `roll` | 30..36 | as above |
| magnetometer | 36..44 | as above |
| `l_stick_force` / `r_stick_force` | 60..64 | capacitive stick sensors; see below |
| `_unk31` | 15 | unknown in the reference implementation too |

The controller's `Cap0`..`Cap3` bits are **not mapped**. `src/report.rs` calls their
individual assignment "tentative", and the only plausible targets are the Deck's
`l_stick_touch` / `r_stick_touch`. Guessing would put phantom stick-touch flags
in front of Steam Input's capacitive-stick behaviours, so they stay clear.

---

## 3. Interpreting Steam's writes

`src/uhid/settings.rs`. Framing differs by channel, which matters because the
same byte means different things:

* a **feature** write is `[report_id][command][len][payload…]` — Valve
  multiplexes the whole command set through one feature report, so the command is
  the *second* byte (`0x01` on `triton`, an unnumbered `0x00` on `deck`). This is
  the frame `src/lizard.rs` already builds;
* an **output** write is `[report_id][payload…]`, where the report id **is** the
  command. Only reachable on `triton`, whose descriptor declares output reports.

| Command | Id | Channel | hyprpad does |
|---|---|---|---|
| `SetSettingsValues` | `0x87` | feature | **Decoded, not applied.** Parsed into `(setting, u16)` pairs and logged by SDL name. |
| ↳ `SETTING_LIZARD_MODE` | 9 | | **Ignored — hyprpad's.** `src/lizard.rs` holds it at 0 and re-sends every 30 s. |
| ↳ `SETTING_STEAM_WATCHDOG_ENABLE` | 71 | | **Ignored — hyprpad's**, same reason. |
| ↳ `SETTING_IMU_MODE` | 48 | | **RELAYED — the one setting that is.** Written to the real controller through `lizard.rs`'s frame builder. See §3.1. |
| ↳ everything else | | | Logged by SDL name (all 82 of them), dropped. |
| `TriggerRumbleCommand` | `0xEB` | feature | **Translated.** `left_speed`/`right_speed` (u16 LE at bytes 5..7, 7..9 of `PackedRumbleReport`) become `haptics::Haptics::rumble`'s two `FF_RUMBLE` magnitudes. |
| `TriggerHapticPulse` | `0x8F` | feature | **Translated.** `[pad][duration][interval][count][gain]` per the kernel's `steam_haptic_pulse`; the wire side is un-XORed to a logical `haptics::Pad`, the gain dropped (the IBEX pulse struct has no gain field). |
| `TriggerHapticCommand` | `0xEA` | feature | **Translated, partly UNVERIFIED** — see §7. |
| `0x80` rumble | `0x80` | output | **Translated.** The controller's own `0x80`, read back with the exact field layout `haptics::build_rumble` writes. |
| `0x81` pulse | `0x81` | output | **Translated.** Likewise, mirroring `haptics::build_pulse`. |
| everything else | | | **Dropped** with a `HYPRSC_DEBUG` log naming the command. |

### Why not relay to the controller

Research §4.3 sketches a relay policy — forward Steam's feature writes straight
to the real device — and §5.1/§5.2 then spend two sections on the arbitration
that requires. This build takes the smaller road, the one InputPlumber's proven
implementation takes (A.4), for two structural reasons:

1. **hyprpad already owns lizard mode.** `src/lizard.rs` sends exactly the two
   settings Steam's `0x87` asks for. Forwarding would be redundant at best and a
   write race at worst — §5.1 warns about precisely this.
2. **hyprpad already owns the actuators, through one writer.** `haptics.rs` runs
   a single writer thread over the controller's only writable fd, and §5.2 requires the
   relay to go through it and "never open a second writable fd on the controller". So
   rumble and haptics are *translated into that path*, not forwarded as bytes.

**What this cost, and what it stopped costing:** Steam's IMU-enable did not
reach the controller, so the gyro never started streaming. That was gap G5, and §3.1
is how it is closed — without breaking either reason above, because the gyro
goes out through *lizard's* frame builder rather than a second writer.


### 3.1 The gyro — the one setting that *is* relayed

Gap G5. Everything above is "hyprpad can satisfy this itself, so it does"; the
IMU is the one thing it cannot. **Only the real controller can turn its own IMU on.**
Its `0x42` carries no gyro bytes at all until it has been told to put them
there, and no amount of interpreting on this side of the wire conjures them.

So `SETTING_IMU_MODE` is relayed to the hardware, and it is the only setting
that is.

**The number, and where it comes from.** SDL's `controller_constants.h` declares
the `ControllerSettings` enum append-only ("*only add to this enum and never
change the order*"), which makes an index a stable identity:

| | |
|---|---|
| `SETTING_IMU_MODE` | **48** (`0x30`). Older headers spell the same slot `SETTING_GYRO_MODE`. |
| value | a `u16` bitmask: `OFF 0x0000`, `STEERING 0x0001`, `TILT 0x0002`, `SEND_ORIENTATION 0x0004`, `SEND_RAW_ACCEL 0x0008`, `SEND_RAW_GYRO 0x0010` |
| what Steam writes | `HIDAPI_DriverSteam_SetSensorsEnabled` (`SDL_hidapi_steam.c`) sends a one-pair `0x87` frame: `SEND_RAW_ACCEL \| SEND_RAW_GYRO` = **`0x0018`** to enable, `OFF` to disable. That is the whole of Steam's gyro request. |

`src/uhid/settings.rs` now carries the **entire** 82-entry enum as
`SETTING_NAMES`, not just the six numbers the daemon spells by name, so a
`HYPRSC_DEBUG` log never prints `SETTING_?` again — the first live Triton
session logged eight distinct unnamed ids, and an unnamed id is a question
nobody can answer from the log alone. Six of those numbers are independently
corroborated: sc-controller's `configure()` sends
`87 15 32 t_lo t_hi 18 00 00 31 02 00 08 07 00 07 07 00 30 gy 00 2e 00 00`
(`docs/research/guide-hold-poweroff.md` §2), which reads back as settings **50,
24, 49, 8, 7, 48, 46** in that order — sleep timeout, smooth mouse, packet
version, both trackpad modes at `TRACKPAD_NONE` (7), **the gyro**, raw joystick.
Five of the seven land on values that only make sense under this numbering, and
the sixth is the gyro variable itself.

**The write path, and the one-writer invariant.** The relay does not write it.
`settings::imu_mode` lifts the value out of the decoded `0x87` and `run.rs`
puts it in a cell that `src/lizard.rs` owns; `disable_lizard_pairs` then appends
`(48, mode)` to the **same** `ID_SET_SETTINGS_VALUES` frame that module has
always built:

```text
[01][87][09][09 00 00][47 00 00][30 18 00]
 id  cmd len  lizard=0  watchdog=0  IMU=raw-accel|raw-gyro
```

One frame builder, one writer, one heartbeat. Research §5.2's "never open a
second writable fd on the controller" is satisfied structurally rather than by
convention, and three properties fall out for free, with no new timer and no new
code path:

* it **survives the 30 s `RESEND_INTERVAL` re-send**, because it is *in* the
  frame that re-send sends;
* it **survives a reconnect and a power cycle**, for the same reason — the
  re-send exists to re-cover a controller that came back with firmware defaults,
  and the gyro is now part of what gets re-covered;
* it **cannot race the ownership loop**, because the ownership loop is the only
  thread that sends it.

Steam's request must not wait up to 30 s, though, so `lizard::nudge()` rings a
condvar the ownership loop now waits on instead of sleeping. The daemon's 250 Hz
loop takes an uncontended lock and signals; it never does the I/O itself, which
keeps a 12-retry `EPIPE` budget on a sleeping controller (~240 ms per node) off
the frame path. `set_imu_requested` returns whether the effective mode actually
*changed*, so Steam re-stating a mode the controller already holds — which it does,
repeatedly — is not a feature report.

**Who decides, and what "off" means.** Two authorities, and the rule is one
line: *Steam wins while it is asking; hyprpad's preference is what is left.*

| State | Written to the controller |
|---|---|
| Nothing has ever asked (**the default**, and every `kind = "xbox"` install) | **no `SETTING_IMU_MODE` pair at all** — the frame is byte-for-byte the two-pair frame this module has always sent |
| Steam asked for sensors | Steam's mask, verbatim |
| Steam asked for `OFF`, or closed the fake, or the relay stopped | hyprpad's preference, **written explicitly as `0`** — not omitted, which would leave the firmware holding whatever it was last told |
| `h.gamepad { gyro = true }` | `SEND_RAW_ACCEL \| SEND_RAW_GYRO`, held whether or not Steam is asking |

The first row is load-bearing: a feature nobody uses must write nothing to
anybody's controller, and
`lizard::tests::an_unengaged_hold_leaves_the_historical_frame_byte_for_byte_unchanged`
pins it.

`gyro = true` is a **diagnostic knob, not a feature switch.** Its use is telling
"the gyro is not working" apart from "Steam never asked": turn it on and the IMU
bytes appear in the controller's `0x42` with no Steam in the picture at all. In
ordinary use it should stay off, because a gyro streaming for a desktop nobody is
aiming with is battery spent for nothing.

**Why it is deliberately *not* gated on the forwarding gate.** Gating the IMU on
game focus would be one line, and it would be wrong. The forwarding gate flips on
every focus change — the guide held for a chord, the OSK raised, a moment on the
desktop — many times a minute in ordinary use, and each flip would become a
feature-report write to a battery-powered controller. That is a write storm to
save power. Two things already do the job better:

* **Steam's request is already game-scoped.** SDL calls `SetSensorsEnabled` when
  a game turns its sensors on and again when it turns them off, so the IMU is up
  only while something is actually reading it.
* **Not-forwarding already parks the gyro from Steam's side.** While hyprpad owns
  the controller the relay streams `translate::triton_neutral`, whose IMU bytes
  are zero — so a game sees a stationary gyro during a guide chord regardless of
  what the firmware is doing.

The power question is answered by the default being off and by the restore, not
by chasing focus.

**Where the bytes land, and what is still UNVERIFIED.** `src/report.rs` decodes
bytes 0..30 of the 54-byte `0x42` and says of the rest: *"Bytes 30+ carry the IMU
and stream only after an enable feature-report (Steam sends one); they are
untouched here."* The `triton` profile is a pass-through — `controller_to_triton`
copies all 54 bytes and clears two bits — so **nothing else has to change**: the
moment the controller starts filling bytes 30+, Steam gets them. The classic Valve
state packet puts accel `x,y,z`, gyro `x,y,z` and the orientation quaternion
`w,x,y,z` there as ten little-endian `i16`s (offsets 30..50), which fits the 24
undecoded bytes with 4 to spare.

That last sentence is **inferred from the report table and the classic layout,
not observed** — no IMU-enabled `0x42` has been captured from this controller. Two
things would falsify it: the controller answering the enable but putting IMU data in a
*different* report (the descriptor also declares inputs `0x43`, `0x44`, `0x45`,
`0x79`, `0x7b`, none of which the relay forwards), or the firmware ignoring
setting 48 entirely. Both are visible in one step — see §8 step 6.

**`deck` gets none of this.** `controller_to_deck` transcodes from `report::Frame`,
which has no IMU fields, so the Deck report's accel/gyro/magnetometer bytes stay
zero however the controller is configured. The setting still reaches the hardware; the
transcode is what drops it. Decoding the IMU into `Frame` is the follow-on that
would close it, and it is worth doing only if `deck` ever becomes the default.

### `GET_REPORT` answers

Never late, never an error, never empty. The three handshake answers are
InputPlumber's verbatim bytes — the ones the adopted run sent:

| Selector | Answer |
|---|---|
| `GetAttributesValues` `0x83` | the 64-byte TLV blob, `[0x00, 0x83, 0x2d, …]`. For `triton`, `ATTRIB_PRODUCT_ID` (the first TLV's u32) is patched from `0x1205` to `0x1302` so the answer does not contradict the claimed identity — risk R3's prescribed mitigation. |
| `GetStringAttribute` `0xAE` | `[0x00, 0xAE, 0x14, 0x01]` + serial, padded to 64. `0x01` is `ATTRIB_STR_UNIT_SERIAL`; `0x14` = 20 is the declared payload, so the serial has 19 bytes to fit in. `deck`: `HYPRPAD-DECK-0001`. `triton`: `FXA0000000001`, the captured unit's. Both equal that profile's `uniq` — §1.3. |
| `GetChipId` `0xBA` | `[0x00, 0xBA, 0x11, 0x00]` + 15 chip-id bytes, padded to 64. |
| anything else | a **correctly framed** 64 bytes: `[0x00, <selector>, 0x00, …]`. |

Which selector is live is chosen by the preceding `SET_REPORT`, exactly as
InputPlumber's `current_report` field does; the initial value is `InputData`
(`0x09`).

The last row is the one deliberate deviation from the proven probe, which
answered `size = 0` there. Research A.8 prescribes "correctly framed canned data
(right length byte, right leading `report_id`/type), never `size = 0`", and a
real device always returns a full report, so this is the strictly safer of the
two behaviours rather than a departure from what was tested.

---

## 4. The streaming rule, and reconnect

### 250 Hz, unconditional, from creation

**The device emits an input report every 4 ms whether or not anything moved.**
This is load-bearing, not an optimisation, and it is the single difference
between the probe Steam ignored and the one it adopted:

* InputPlumber's `SteamDeckUhidDevice::poll()` calls `write_state()`
  unconditionally on every 4 ms tick — the wire write is on the timer, not on
  input change (A.2);
* SDL's Deck backend hard-gates detection on a live report:
  `SDL_hid_read_timeout(…, 16); if (size == 0) return false;`;
* the passive probe streamed nothing and got zero engagement.

So `relay::stream_loop` is a thread with its own clock, and the daemon's frame
path only ever *updates what it streams*.

### Neutral

| State | Streamed |
|---|---|
| A game is forwarding, controller live | the current frame (re-stamped counter on `deck`; the controller's own counter untouched on `triton`) |
| Not forwarding — desktop, OSK, guide held (ranks 1, 2, 4) | **neutral** |
| Controller silent for more than `STALE_AFTER` (200 ms) | **neutral** |
| Daemon exiting | device destroyed (`UHID_DESTROY`) |

Neutral is a valid, complete, absolute-state report: buttons clear, sticks and
pads centred, triggers zero, counter advancing. The same invariant
`gamepad::PadReport::neutral` enforces for the Xbox pad — a game is never left
holding a stuck input.

The 200 ms staleness timer exists because the frame path *stops running* when the
controller sleeps; without it the last live frame would repeat forever, and whatever
was pressed at the moment the controller napped would stay pressed.

### 4.1 The Steam log churn (#35) — measured, and it is not ours

With the fake adopted, `~/.local/share/Steam/logs/controller.txt` fills up fast,
and the obvious suspicion was the 250 Hz stream: something in a "neutral" report
toggling under Steam and being read as an event. It was worth checking properly,
because a stream that lies about being idle is the kind of bug that never shows
up in a unit test.

**It is not us.** Three measurements, all on 2026-09-02 on this machine.

#### What we actually put on the wire

10 seconds of the live fake read straight off its hidraw node — `28DE:1302`,
found by walking `/sys/class/hidraw/*/device/uevent` — while the daemon was on
the desktop (i.e. streaming neutral) and Steam held the device open:

```
reports: 2501 over 9.997 s = 250.1 Hz
lengths: [54]          report ids: [0x42]
byte indices that ever change over the WHOLE capture: [1]
counter steps != +1 (mod 256): 0
bytes differing between ANY consecutive pair: [1]
reports with any non-zero byte at 30+: 0 / 2501
inter-report ms: min 1.33  p50 4.00  p99 4.50  max 4.77
```

Byte 1 is the sequence counter. **It is the only byte that changes anywhere in
the capture**, it advances by exactly one every tick with no exceptions across
2500 reports, and it wraps cleanly. There is no battery field, no timestamp, no
"connected" flag, no pad-touch bit and no capacitive bit — not "constant", but
*absent*, because a neutral report is zeros. Nothing here can be what Steam is
reacting to. `relay::tests` now pins all of that
(`six_hundred_neutral_triton_ticks_differ_only_in_the_sequence_counter` and
friends), so it stays true.

#### The churn predates the relay, and was worse with the real controller

The lines the churn is made of — `Controller N uses xinput`, `Queueing
activation for controller`, `Add to Config Cache Request`, `HID: Add to Config
Cache - full cache hit`, `Opted-in Controller Mask for AppId`, `Controller
PollState Changed` — are ordinary Steam Input bookkeeping:

| Evidence | Number |
|---|---|
| `uses xinput` lines in this log | **3381**, spread over 20 dates back to 2026-06-11 — months before the relay existed |
| `uses xinput` lines on 2026-09-02, the day the fake was adopted | **0** |
| Busiest single second with the **real** controller (2026-08-31, `V1 HID protocol via Dongle`) | **1168 lines**, and 243 lines/s was routine that day |
| That day's composition | `Add to Config Cache Request` ×412, `HID: … full cache hit` ×350, `Queueing activation` ×307, `Opted-in Controller Mask` ×48 — the exact categories |

So the baseline the change should be judged against is not "a quiet log"; it is
a log that hit four figures in a second when Steam had the hardware directly.
`PollState Changed` is not high-frequency either: today it went `1 → 2` at
08:04:42 and `2 → 1` at **08:14:26**, ten minutes apart.

#### The part that *was* ours, and is already fixed

One category was genuinely ours, and it is the 64-versus-65-byte `GET_REPORT`
answer of §1.1 — not the input stream at all:

| Window (2026-09-02) | Lines | `CGetControllerInfoWorkItem::RunFunc: Read failure.` + `couldn't get controller details for SC, PID=4866` |
|---|---|---|
| before the framing fix (to 08:05) | 4165 | **1527** — 37% of the log |
| after it (08:22 onward) | 340 | **0** |

Pre-fix, Steam also re-enumerated every device on a **15-second cycle**
(08:02:59, 08:03:13, 08:03:28, 08:03:43, 08:03:58, 08:04:13, 08:04:28 …), each
sweep costing ~50 lines. Post-fix that stops dead: two bursts, one per adoption,
and then `Controller Info: HWID: 46, FWTimestamp: 0x65E4F1AD` — Steam reading
the attributes successfully for the first time — after which **the log went
silent at 08:23:47 and stayed silent for minutes while the fake kept streaming
at 250 Hz**.

That is the dispositive observation: a 250 Hz stream producing *zero* log lines.
Whatever churn remains is Steam talking to itself about config sets.

#### The one bit of noise that is a consequence of our design

`packaging/udev/72-hyprpad-puck.rules` makes the puck's five hidraw nodes
unopenable, so every Steam device sweep logs `Local Device Found … Unable to
open local device: /dev/hidraw7` five times over. That is ~45 lines per sweep,
and it is the *intended* behaviour — Steam must not see the real controller (§6) —
with the log line as its only cost. It fires per sweep, not continuously.

#### What is deliberately *not* changed

The obvious "fix" would be to renumber relayed reports so the counter is always
ours and always monotonic. **No.** §4.4 forbids it, and now more strongly than
when it was written: the counter shares the report with the IMU data §3.1 turned
on, and renumbering risks desync with the IMU timestamp path. The consequence is
that a live frame held between controller updates repeats its counter for up to
`STALE_AFTER`, and that crossing live → neutral moves the counter's *source*
once per transition. Both are bounded, both are on the record
(`a_held_live_frame_repeats_its_own_counter_rather_than_being_renumbered`,
`the_live_to_neutral_transition_is_one_counter_discontinuity_and_nothing_else`),
and neither is tens of lines per second.

### Reconnect

**The uhid device is created once, at daemon start, and destroyed only at exit.**
It is never torn down on a focus change or a controller disconnect. Steam adopts a
controller when its hidraw node appears, so a create/destroy cycle makes it
re-detect, re-apply configs and toast about it (§5.3).

So when the controller naps:

* the hidraw readers end, `Input::ReadersEnded` fires, and `GamepadState::release`
  puts the relay on neutral;
* the virtual device stays created and keeps streaming. **Steam never sees a
  disconnect.**

On reconnect the daemon's existing reconnect wait re-arms the readers and frames
resume. The relay itself needs nothing: it is the same device it always was.

If the *uhid* fd is ever lost, `send_input` fails and the streamer exits; the
caller re-creates the relay with a fresh descriptor. That is the one path where
the host-integration half hands in a new fd.

---

## 5. The fd-injection boundary

Nothing in `src/uhid/` opens a device node. The whole module takes an
already-open `OwnedFd`:

```rust
// src/uhid.rs
pub fn acquire_uhid() -> io::Result<OwnedFd>;
impl UhidDevice {
    pub fn create(fd: OwnedFd, profile: &'static Profile) -> io::Result<UhidDevice>;
}
// src/uhid/relay.rs
impl SteamRelay {
    pub fn start(fd: OwnedFd, profile: &'static Profile) -> io::Result<SteamRelay>;
}
// src/run.rs
fn start_relay(cfg: &GamepadConfig) -> Option<SteamRelay>;
```

`acquire_uhid` is the seam. **It is now filled** (§6): it asks the root broker
first and falls back to a plain `open("/dev/uhid")`, which on a machine that has
not run `hyprpad setup` still fails with `EACCES` — the node is
`crw------- 1 root root 10, 239` and no shipped udev rule opens it. That failure
is logged once, and the daemon carries on with no game sink.

`UHID_CREATE2` — unlike the legacy `UHID_CREATE` — has **no
`f_cred != current_cred()` check**, so a descriptor opened by another process is
usable as-is. That is precisely what makes the broker in §6 possible, and why
filling this seam needed no change anywhere else in `src/uhid/`.

---

## 6. Host integration — the privileged half

Two problems, one shape. Getting a writable `/dev/uhid`, and taking the real controller
away from Steam, both need root; and because **Steam and hyprpad run as the same
user**, no group and no ACL can separate them. Anything the session-user daemon
may open, session-user Steam may open too.

So the privilege is moved out of the daemon entirely, into two pieces.

### 6.1 The udev rule — `packaging/udev/72-hyprpad-puck.rules`

```udev
ACTION=="remove", GOTO="hyprpad_puck_end"
SUBSYSTEM!="hidraw", GOTO="hyprpad_puck_end"
ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", \
    TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0600"
LABEL="hyprpad_puck_end"
```

The match is the one `src/hidraw.rs` uses — vendor and product of the USB device
the hidraw node hangs off — so a single rule covers all five interface slots
however they are numbered this boot. `:=` is final assignment, so nothing later
can widen the permissions back.

**It matches hidraw only.** The controller's `input`/`event` nodes are deliberately
left exactly as the system configures them: hyprpad reads the controller over
hidraw and only over hidraw, and on this machine the vendor-only descriptor
produces no evdev node at all. Hiding an evdev node that might appear on a future
firmware is a separate decision, not this file's.

It does not touch the *virtual* device. Valve's `60-steam-input.rules` line
`SUBSYSTEM=="hidraw", KERNELS=="000[356]:28DE:*"` gives `uaccess` to hyprpad's
`28de:1302` / `28de:12f0` clone for free (research §2), and our rule pins
`idProduct` to `1304` — which the clone could not match anyway, having no USB
parent.

#### The uaccess-ordering finding — why the number is 72, and why 99 was wrong

This is the detail the W12 log left open ("whether an inherited `uaccess` tag can
be removed vs. only overriding `MODE`/`GROUP` still needs a live test").

`TAG+="uaccess"` grants nothing by itself. It sets a tag. The ACL is applied
later, by systemd's `73-seat-late.rules`:

```udev
TAG=="uaccess|xaccess-*", ENV{MAJOR}!="", RUN{builtin}+="uaccess"
```

and `man udev` is explicit that a `RUN` entry is *"executed after processing of
all the rules for the event"*, with the list only clearable wholesale by
`RUN=""` / `RUN:=""`. **So once `73-seat-late.rules` has seen the tag and queued
the builtin, removing the tag afterwards changes nothing** — the ACL is applied
anyway. And an ACL beats the mode bits: a node at `0600 root:root` with
`user:you:rw-` on it is still openable by you.

The window for `TAG-="uaccess"` is therefore bounded on *both* sides:

| Must run after | Because |
|---|---|
| `60-steam-input.rules` | Valve adds the tag there |
| `70-uaccess.rules` | systemd's own tag adders (none match a Steam Controller today, but ordering after it costs nothing) |

| Must run **before** | Because |
|---|---|
| `73-seat-late.rules` | the tag is consumed and the ACL queued there |

`72-` is in that window. `71-` would also work (`71-seat.rules` does not touch
uaccess). **`75-`, `95-` and `99-` do not** — which retires
`scripts/99-hyprpad-claim-puck.rules`, the W12-era reference rule, whose number
put it after the tag had already been spent. That file now carries a header
saying so and pointing here.

*(VERIFIED by reading the shipped rules on this machine, systemd 261: `grep -rn
uaccess /usr/lib/udev/rules.d/7*.rules` shows `73-seat-late.rules` as the only
consumer. Not yet verified by installing the rule — that is the owner's step.)*

### 6.2 The broker — `src/broker.rs`, `hyprpad broker`

A root helper whose whole vocabulary is two words. It opens what it is asked for
and passes the open descriptors back over a unix socket with `SCM_RIGHTS`.

```text
->  "uhid\n"        <-  "ok 1\n"  + 1 fd    /dev/uhid,  O_RDWR|O_CLOEXEC
->  "controller\n"  <-  "ok 5\n"  + 5 fds   every Steam Controller hidraw node
    (or "puck\n",                          (28de:1304 and 28de:1303),
     the legacy alias)                     hidraw::OPEN_FLAGS
->  anything else   <-  "err unknown request\n"  + 0 fds
```

One request per connection, then close. The client half-closes its write side, so
the broker's read always terminates.

Security posture, and why it is a small surface:

* **It takes nothing from the client but a choice between two hard-coded verbs.**
  No path, no flags, no numbers. `parse_request` is an allowlist of two exact
  words: no leading space, no different case, no arguments, nothing over 32 bytes.
* **It never reads from, writes to or ioctls a device.** It opens and hands over.
* **It caches nothing.** Every `controller` rescans `/sys/class/hidraw`, so the controller
  sleeping and coming back on different node numbers needs no special handling
  anywhere — it is just a later request returning different descriptors.
* **Two gates on who may ask.** The socket is `0660 root:hyprpad`, so only
  members of that group can connect — this is the default and the documented
  control. `--uid N` / `HYPRPAD_UID=N` adds an `SO_PEERCRED` check on top, for a
  machine several humans share. uid 0 is always allowed, since root can open the
  nodes without asking.
* **It logs every decision to stderr**, which under the shipped unit is the
  journal.

The `cmsg` arithmetic is hand-rolled (`CMSG_ALIGN`/`CMSG_LEN`/`CMSG_SPACE` are C
macros with nothing to call from Rust) and checked against `libc`'s own
implementations for every payload size a reply can have, so a wrong `sizeof`
assumption fails `cargo test` rather than silently truncating a descriptor array.
A truncated control message is an error, never a short set that looks complete.

### 6.3 The units — `packaging/systemd/`

`hyprpad-broker.socket` owns `/run/hyprpad/broker.sock` at `0660 root:hyprpad`,
`Accept=no`, and starts the service on the first connection, handing it the
listening descriptor as fd 3. The broker implements `sd_listen_fds` by hand
(`LISTEN_PID` + `LISTEN_FDS`), so **it never creates, chmods or chowns anything**
— which is why the service can run with no writable filesystem at all.

`hyprpad-broker.service` runs `/usr/local/bin/hyprpad broker` as root under:

| Setting | Why |
|---|---|
| `CapabilityBoundingSet=` (empty), `AmbientCapabilities=` | it is uid 0 and only ever opens root-**owned** nodes, so plain DAC owner-match suffices and no capability is ever needed. If the rule's `OWNER:=` ever stops being root, this becomes `CAP_DAC_OVERRIDE`. |
| `NoNewPrivileges=yes` | nothing it execs could gain more |
| `DevicePolicy=closed` + `DeviceAllow=/dev/uhid rw` + `DeviceAllow=char-hidraw rw` | the cgroup device controller is the wall behind the socket: even a compromised broker can reach only the two device classes it exists for |
| **no** `PrivateDevices=` | it would replace `/dev` with a minimal one that has neither `/dev/uhid` nor the hidraw nodes |
| `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `UMask=0077` | it writes no files |
| `RestrictAddressFamilies=AF_UNIX`, `PrivateNetwork=yes`, `IPAddressDeny=any` | the socket is the only channel |
| `ProtectProc=invisible`, `ProcSubset=pid` | it never inspects other processes |
| `SystemCallFilter=@system-service`, `SystemCallArchitectures=native`, `LockPersonality`, `MemoryDenyWriteExecute`, `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`, `ProtectKernel{Tunables,Logs}`, `ProtectClock`, `ProtectHostname`, `ProtectControlGroups` | ordinary hardening, none of it load-bearing for function |
| **no** `ProtectKernelModules=` | opening `/dev/uhid` can trigger a char-major module autoload, and there is no upside to gambling on that when an empty capability bounding set already makes a deliberate `modprobe` impossible. `uhid` is loaded on any desktop anyway — BlueZ uses it. |

`packaging/sysusers.d/hyprpad.conf` creates the group: a system group with no
members and no user of its own, only ever a filter on who may connect.

### 6.4 The daemon side

One function expresses the whole preference, and everything that touches the controller
goes through it:

```rust
// src/hidraw.rs
pub enum ControllerSource { Paths(Vec<PathBuf>), Fds(Vec<OwnedFd>) }
impl ControllerSource { pub fn acquire() -> Option<ControllerSource>; }
```

| Broker answer | What the daemon does |
|---|---|
| no socket at `/run/hyprpad/broker.sock` | direct opens; one quiet line, once per process |
| socket there, exchange failed (refused, wrong uid, mid-restart) | direct opens; one warning, once per process |
| `ok 0` | direct opens — nothing usable, and the controller may simply be away |
| `ok N`, N ≥ 1 | use the passed descriptors |

`None` means neither route worked, which is exactly the condition the startup
wait and the reconnect wait already sit on — so **both waits now poll
`ControllerSource::acquire` at `RECONNECT_SCAN_INTERVAL`**, and both ask the broker
first. The answer is re-evaluated on every generation, so starting the broker
under a running daemon is picked up by the next reconnect, and stopping it falls
back.

`spawn_reader_pipeline` takes a `ControllerSource` by value (a brokered generation *is*
the descriptors; there is nothing to re-open from). `haptics.rs`'s periodic
re-open and `lizard.rs`'s feature-report sends go through the same function —
which they must, or installing the rule would break haptics and lizard ownership
even while input kept working.

`hidraw::OPEN_FLAGS` is the shared contract: `O_RDWR | O_CLOEXEC`, and
deliberately **not** `O_NONBLOCK`.

* `O_RDWR` because `HIDIOCSFEATURE` (lizard) and output reports (haptics) are
  writes, and once the broker is the only way in there is one descriptor set
  serving reading and writing alike.
* `O_CLOEXEC` because the daemon spawns the OSK as a child; the broker's replies
  get the same property from `MSG_CMSG_CLOEXEC`.
* Not `O_NONBLOCK` because `read_all`'s per-node threads block in `read` by
  design — a non-blocking descriptor would turn each into a spin loop on
  `EAGAIN`.

`status.json` grows one field:

```json
{"connected": true, "relay": "steam", "source": "broker", …}
```

`"source"` is `"broker"` or `"direct"` and answers a different question from
`"connected"`: not *is the controller here* but *is Steam able to see it too*.

### 6.5 Install and verify

`hyprpad setup` **prints** the root block and never runs it; `hyprpad setup
--print` prints only the block; `hyprpad setup --check` reports on it read-only,
opening nothing:

```
hyprpad setup --check (read-only; opens nothing)

  ok  Valve's udev rule      /usr/lib/udev/rules.d/60-steam-input.rules
  NO  hyprpad udev rule      72-hyprpad-puck.rules not installed — …
  NO  broker socket          nothing at /run/hyprpad/broker.sock — …
  ?   broker: uhid           no socket to ask
  ?   broker: controller     no socket to ask
  NO  dongle nodes root-only still reachable: /dev/hidraw7 (0660 uid 0), …

NOT READY. Missing or wrong: … See `hyprpad setup --print` for the install block.
```

The node check is `stat` only, and that is sufficient: when a POSIX ACL grants
anyone anything, the group bits of `st_mode` become the ACL *mask*, so a
`uaccess` ACL shows up as non-zero group bits. Which is just as well — the one
thing the check must not do is open the node whose unopenability it is testing.

The two `broker:` lines are the part a file listing cannot replace. They catch
the failure that looks like success: everything installed and enabled, and the
shell still refusing because it predates `usermod -aG hyprpad`.

### 6.6 What is still unverified

Everything in §6 is tested where it can be tested without privilege, and the
broker's socket, peer-credential, parse and refusal paths were exercised end to
end over a real unix socket. Not verified, because it needs an install:

* that udev actually applies the rule at 72 and the controller's nodes come back
  `0600 root:root` with no ACL — the ordering argument is read off the shipped
  rules and `man udev`, not off a live trigger;
* the socket-activation path (`LISTEN_FDS`); the standalone `--socket` path was
  exercised;
* a `controller` or `uhid` request that actually *succeeds* — both need root, and the
  descriptor hand-over itself is covered by unit tests over a `socketpair` with
  pipes standing in for devices;
* that Steam, with the real controller hidden, sees exactly one controller (§8 step 4).

---

## 7. Known gaps, and what is UNVERIFIED

| # | Gap | Status |
|---|---|---|
| G1 | **Does Steam adopt `triton` (`1302`) at `Interface: -1`?** | **UNPROVEN.** The one open question. Risk R1 / Q-u1. Falls back to `identity = "deck"`, which is proven. |
| G2 | `triton`'s `GetAttributesValues` blob | **UNVERIFIED.** No `1302` blob was ever captured. It is the Deck blob with `ATTRIB_PRODUCT_ID` patched to `0x1302`. Steam's log shows a real Triton survives a *failed* attribute probe (`Deck Controller PCB Serial# invalid: NA`), so a well-formed wrong answer is low severity. Replace when a real blob is captured. |
| — | **`deck` had no unit serial** | **FIXED, §1.3.** `uniq` was empty and Steam invented `28de-12f0-3147b8f`; it is now `HYPRPAD-DECK-0001`, matched by the `0xAE` answer. Costs one re-do of any binding saved against the invented key. |
| G3 | `triton`'s chip id | **UNVERIFIED.** Modelled on the Deck answer. |
| G4 | `TriggerHapticCommand` (`0xEA`) shape | **Partly UNVERIFIED.** The `PackedHapticReport` layout is verbatim, but `PadSide`/`Intensity` are enums whose discriminants were not captured, and the command carries **no duration** — only an intensity class. hyprpad fires its own calibrated single tick (`0x190` = 400 µs, from the kernel's `steam_haptic_pulse`) on the named side, reading side as `0 = left, 1 = right`. The first live session's `HYPRSC_DEBUG` log settles it. |
| G5 | **Gyro does not reach Steam.** | **CLOSED on `triton`, pending a live check** — §3.1. Steam's `SETTING_IMU_MODE` (48) is now written to the real controller through `lizard.rs`'s frame builder, and the pass-through carries whatever the controller then puts in bytes 30+. That the controller answers setting 48 by filling those bytes of the *same* `0x42` is **inferred from the report table, not observed** — §8 step 6 is the check. Still open **by construction on `deck`**: `controller_to_deck` transcodes from `report::Frame`, which has no IMU fields. |
| — | **Steam log churn (#35)** | **Not ours — measured, §4.1.** Our stream is 2500 consecutive reports in which byte 1 is the only byte that changes; the churn categories predate the relay by months and hit 1168 lines/s with the *real* controller. The one part that was ours was the `GET_REPORT` framing (§1.1), now fixed: 1527 read-failure lines before, zero after, and the log falls silent while the fake streams at 250 Hz. |
| G6 | Quick Access is never stripped | `StripMask` supports it; nothing sets it. §4.4 suggests deriving the whole mask from the live binding table so any bound button is withheld. Follow-on. |
| G7 | `identity` / `kind` do not take effect on `hyprpad reload` | They choose a device created once at startup. Restart the daemon. |
| G8 | Latency of the extra userspace hop | Unmeasured (risk R7). Both hops are non-blocking; budget ≪1 ms. Measure on the first live run. |
| G9 | A future `hid-steam` binding the virtual device | Deferred (R5). `0x12f0` exists only in Steam's userspace table so no kernel driver will claim it; `0x1302` **is** in upstream `hid-steam`'s table, which is a standing argument for `deck` if it ever lands here. |

---

## 8. Manual test plan (for the owner)

Everything below needs the host-integration half (§6) installed first — without a
writable `/dev/uhid` the daemon logs one line and runs with no relay, which is
step 0's expected result on a machine that has not run `hyprpad setup`.

### Step 0 — confirm the current, expected failure

```bash
hyprpad setup --check     # expect NOT READY, everything but Valve's rule missing
hyprpad run               # with kind = "steam" configured
```

Expect one line naming `/dev/uhid` and pointing at `hyprpad setup`, and:

```bash
jq -r '.relay, .source' "$XDG_RUNTIME_DIR/hyprpad/status.json"   # "none", "direct"
```

### Step 1 — install the host half, restart, confirm the device exists

Run the block `hyprpad setup --print` emits (§6.5), **re-login or `newgrp
hyprpad`**, then:

```bash
hyprpad setup --check     # expect READY, all six lines ok
```

The two checks that matter most, spelled out by hand:

```bash
# the real controller must be root-only, with NO '+' and no ACL
ls -l /dev/hidraw*
getfacl /dev/hidraw7      # expect user::rw- / group::--- / other::--- only

# …while the FAKE still carries an ACL for you, from Valve's own rule
for h in /sys/class/hidraw/hidraw*; do
  grep -qi '28DE:1302\|28DE:12F0' "$h/device/uevent" && { echo "== $h"; cat "$h/device/uevent"; }
done
getfacl /dev/hidrawN      # the fake: expect user:<you>:rw-
```

Restart the daemon, then:

```bash
jq -r '.relay, .source' "$XDG_RUNTIME_DIR/hyprpad/status.json"   # "steam", "broker"
journalctl -u hyprpad-broker -n 20                              # one line per request
```

The daemon's startup line names the identity, the VID:PID it created, and which
half produced `/dev/uhid`.

If `source` says `"direct"` after all this, the group has not reached the
daemon's process: it was started before `usermod -aG hyprpad`. Restart it from a
shell that has the group.

### Step 2 — establish the baseline **before** trusting any negative

This is the step whose absence invalidated the first probe. Steam must be in a
controller-engaging state at all:

```bash
grep -iE 'opened|V1 HID' ~/.local/share/Steam/logs/controller.txt | tail
ls -l /proc/$(pgrep -x steam)/fd | grep hidraw
```

Steam must be logging `!! Steam controller device opened for index N` and
holding a hidraw fd. If it grabs *nothing*, fix that first (Settings →
Controller, Steam Input enabled; a game foregrounded with controller option
Forced On, or Big Picture) — otherwise the test proves nothing.

### Step 3 — the verdict

```bash
grep -iE '1302|12f0|opened|V1 HID|deck' ~/.local/share/Steam/logs/controller.txt | tail -30
```

| Look for | Meaning |
|---|---|
| `Local Device Found / type: 28de 1302` (or `12f0`) | Steam enumerated it |
| `serial_number: FXA0000000001` (triton) or `HYPRPAD-DECK-0001` (deck), **not** blank | the `uniq` field arrived — no `Invalid or missing unit serial number` should follow (§1.3) |
| `Interface: -1` then `Controller uses V1 HID protocol via USB` | the `-1` question answered — **G1 PASS** |
| **`!! Steam controller device opened for index N`** | **adopted** |
| the fake's `/dev/hidrawN` in `/proc/$(pgrep -x steam)/fd` | Steam is holding it |
| `HYPRSC_DEBUG=1` log lines `[relay] SetSettingsValues […]` | **dispositive** — Steam is actively configuring it |
| enumerated but never opened, *while the real controller is opened in the same session* | identity rejected → set `identity = "deck"`, restart, retest |

### Step 4 — the UI, and the thing that must **not** be there

Steam → Settings → Controller:

* **exactly ONE** Steam Controller listed, and **no Xbox 360 pad**. Two entries
  means either the host-integration device-hide (§6.2) is not in place, or the
  Xbox pad is still being created — the latter would be a bug in this change, as
  `kind = "steam"` must not create it;
* the device should show **trackpads, gyro and back grips**, not a generic pad.
  Gyro is now wired (§3.1) but unproven — step 6 is where it is settled.

### Step 5 — routing and haptics

* Foreground a game: input reaches it. Hold the guide: input **stops** and the
  game sees neutral, no stuck stick. Raise the OSK: same. Return to the desktop:
  same.
* A guide **chord** must not open the Steam overlay — that is the strip mask
  working. A bare **tap** of the guide, in a game, must: the daemon replays it
  as a 60 ms pulse on the fake (§2.1). Check both, and check that a long hold
  (past `guide_tap_max_ms`, 400 ms) opens nothing. `guide_tap = "none"` turns
  the whole thing off. Note that `forward_guide` is not the knob for this and
  never was: while the button is held nothing is forwarded, so setting it makes
  no observable difference here.
* Trigger rumble in-game: the controller's actuators should buzz. With the game
  backgrounded they must **not** (§5.2 arbitration).
* Run with `HYPRSC_DEBUG=1` for one session and keep the `[relay]` log — it is
  the empirical answer to G4, and to what Steam actually sends a Triton (Q-u2).

---

### Step 6 — the gyro (§3.1), the one thing this build added and cannot prove

Three checks, in order of what they distinguish. Run the daemon with
`HYPRSC_DEBUG=1` for all of them.

**6a — does Steam ask?** Open a game with gyro in its Steam Input config (or
Steam → Settings → Controller → *Calibration & Advanced*, which enables sensors
to draw the gyro readout):

```
[relay] SetSettingsValues [SETTING_IMU_MODE=24 [raw-accel+raw-gyro]]
[relay] IMU raw-accel+raw-gyro -> controller (Steam asked)
```

No such line means Steam never asked, and nothing downstream is hyprpad's
problem yet. `gyro = true` in the config forces the same write with no Steam in
the picture, which is how to carry on testing regardless.

**6b — does the controller obey?** Read the *fake's* stream and watch bytes 30+, which
are zero on every neutral frame:

```bash
h=$(for d in /sys/class/hidraw/hidraw*; do
      grep -qi '28DE:0*1302' "$d/device/uevent" && basename "$d"; done)
sudo -u "$USER" python3 - <<'EOF'
import os, binascii
fd = os.open("/dev/$h", os.O_RDONLY)
for _ in range(200):
    d = os.read(fd, 64)
    if any(d[30:]):
        print("IMU:", binascii.hexlify(d[30:50], " ").decode()); break
else:
    print("bytes 30+ stayed zero")
EOF
```

Move the controller while it runs. Non-zero, changing bytes 30..50 is **G5 proven**:
the enable reached the firmware and the pass-through is carrying it.

If they stay zero while 6a logged the write, the firmware either ignored setting
48 or put the IMU somewhere else. The descriptor declares five other input
reports the relay does not forward (`0x43`, `0x44`, `0x45`, `0x79`, `0x7b`), so
the next step is to read the **real controller** instead of the fake and see which
report id grew:

```bash
sudo python3 -c "
import os,binascii
fd=os.open('/dev/hidraw7',os.O_RDONLY)
seen={}
for _ in range(2000):
    d=os.read(fd,64); seen[d[0]]=len(d)
print(seen)"
```

A report id appearing there that was not there before the enable is the answer,
and it makes the fix a `translate` change rather than a settings one.

**6c — does Steam use it?** Steam → Settings → Controller → the fake →
*Calibration & Advanced*. The gyro readout should move when the controller does. That
is the end-to-end verdict, and the one to report.

**And check the restore.** Quit Steam (or close the game) and confirm:

```
[relay] Steam closed the device; IMU restored to hyprpad's preference
```

then re-run 6b: bytes 30+ must go back to zero. A controller left streaming IMU data
to nobody is exactly the battery drain §3.1's default exists to avoid.

## 9. Tests

All pure, no devices. `cargo test` gates on the exit code.

* **`src/uhid.rs`** — every `struct uhid_event` offset restated against
  `include/uapi/linux/uhid.h`; the `deck` `UHID_CREATE2` event rebuilt the way
  the proven probe builds it and compared byte for byte; event decode for every
  type; the event loop driven against a `SOCK_SEQPACKET` socketpair standing in
  for the uhid fd (feed a `GET_REPORT`, assert the canned reply bytes; feed a
  `SET_REPORT`, assert both the ack and the payload on the channel; feed an
  `OUTPUT`, assert it is forwarded and *not* replied to).
* **`profile.rs`** — the `deck` descriptor and attributes blob parsed **out of
  the probe script itself** at test time and compared; the `triton` descriptor
  compared against the committed capture; a descriptor walk asserting the `1302`
  declares input `0x42` at 54 bytes (the fact the whole pass-through rests on)
  and that the Deck descriptor is unnumbered and 64/64; and the **unit serial**
  of §1.3 — that `deck`'s is fixed and non-empty, that it equals its own `uniq`
  so the two channels agree, that it differs from `triton`'s so the two
  identities keep separate config keys, that it is neither real unit's serial,
  that it fits the `0xAE` answer's declared payload and a `.vdf` filename, and
  that the answer's framing is `[0x00][0xAE][0x14][ATTRIB_STR_UNIT_SERIAL]` on
  both profiles.
* **`translate.rs`** — pass-through fidelity including bytes the decoder does not
  model; the strip mask; the untouched counter; every Deck button bit; every
  analog offset; neutral for both profiles; and a guard that every mapped button
  lands on a distinct bit.
* **`settings.rs`** — the exact `0x87` frame `src/lizard.rs` builds, decoded
  back; multi-pair and malformed frames; `0xEB`/`0x8F`/`0xEA`; the round trip
  that `haptics.rs`'s own `0x80`/`0x81` output reports decode to what they were
  built from; the full 82-entry `SETTING_NAMES` table with every named constant
  pinned to its own slot from both directions; the `SettingGyroMode` bits; and
  Steam's one-pair IMU enable and disable decoded to a mode, including the
  last-pair-wins rule for a gyro folded into a larger write.
* **`lizard.rs`** (the gyro half) — that an unengaged hold leaves the settings
  frame **byte-for-byte** the historical two-pair one; that Steam's enable rides
  out in lizard's own frame as `[01][87][09][09 00 00][47 00 00][30 18 00]`; that
  "off" is an explicit zero rather than a missing pair; the gyro pair's place
  behind the power knobs; the four states of the Steam-versus-preference
  authority rule; that the exit frame restores the preference and discards what
  Steam asked; that a re-stated mode reports no change (so it becomes no write);
  and that a nudge is never dropped and wakes a blocked wait.
* **`relay.rs`** — the neutral stream, the advancing Deck counter, the untouched
  Triton counter, and the staleness decay; and the **neutral-frame stability**
  rule of §4.1: 600 consecutive neutral ticks on each profile in which the
  sequence counter is the only byte that may differ and may only advance by one,
  that a neutral report has no flag or reading anywhere that *could* toggle
  (including a zero IMU window), that the `u8` counter wraps without a stutter,
  and the two deliberate counter discontinuities — a held live frame repeating
  its own counter, and the one jump when live gives way to neutral.
* **`config.rs` / `lua_config.rs`** — `kind` and `identity` on both front-ends,
  their aliases, their defaults, and that a typo fails the whole parse.
* **`status.rs` / `run.rs`** — the `"relay"` field, and that it reports the sink
  that exists rather than the one configured; the `"source"` field, and that
  `Source::of` is the one mapping from a live generation to the wire value.
* **`broker.rs`** — the two-verb allowlist and everything it refuses (case,
  spacing, arguments, embedded NUL, overlength); the status-line round trip; the
  hand-rolled `CMSG_ALIGN`/`CMSG_LEN`/`CMSG_SPACE` against `libc`'s own for every
  payload size a reply can have; an `SCM_RIGHTS` round trip over a `socketpair`
  passing pipe descriptors, asserting the *received* descriptor works and that
  five of them land on the five right pipes; that a truncated control message is
  an error rather than a short set; the uid gate's decision table; `SO_PEERCRED`
  against our own uid; argument and bind-path precedence; the daemon's fallback
  decision table; and `serve` driven over a real socket for the refused-verb,
  refused-peer and split-request cases.
* **`hidraw.rs`** — that `OPEN_FLAGS` is read-write, cloexec and *blocking*;
  `open_node` on a real file and on a missing one; that a `ControllerSource` knows
  which half it came from; `read_all` streaming from passed descriptors and
  closing its channel when they end; and that one dead path does not sink a whole
  direct generation.
* **`setup.rs`** — that the printed block names every file actually shipped under
  `packaging/` and carries every required step; that the module never spawns a
  process (it *prints* the privileged half); and the whole `--check` verdict
  table against an injected `HostView`, covering a fully installed machine, an
  untouched one, a broker that refuses, a rule installed but not yet applied, an
  absent controller, a missing Valve rule, and an `ok 0` answer.
