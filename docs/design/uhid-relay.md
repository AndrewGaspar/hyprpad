# Design: the uhid relay — a virtual Valve controller Steam adopts

*Implements the device-independent core of `docs/research/uhid-steam-controller.md`.
Code: `src/uhid.rs` and `src/uhid/{profile,translate,settings,relay}.rs`, wired
into `src/run.rs` beside `src/gamepad.rs`. The privileged host half — the udev
rule, the root fd broker and the systemd units that make Steam see the fake and
only the fake — is §6, and lives in `src/broker.rs` and `packaging/`.*

## What it is

hyprpad owns the 2026 Steam Controller puck (`28de:1304`) exclusively and today
hands games a synthesized Xbox-360 pad. That works, and it discards everything
Steam Input adds on a *Valve* controller: trackpads as trackpads, gyro, per-game
configs, Steam-driven haptics, the four back grips.

This feature creates a **second sink**: a virtual HID device on `/dev/uhid`
carrying a Valve VID/PID, which Steam adopts as a genuine Steam Controller.
hyprpad streams the real puck's input into it and interprets Steam's writes back
out onto the real hardware.

It is **opt-in and off by default**. One config line turns it on:

```toml
[gamepad]
kind = "steam"          # xbox (default) | steam
identity = "triton"     # triton (default) | deck
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
| `uniq` (Steam's config key) | `FXA9961402A6C`, the captured unit's serial — pinned | `""`; Steam names it `28de-12f0-<hash>` |
| `phys` | `hyprpad-uhid/1302` | `""` |
| `version` | `0x0307` (bcdDevice 307, as Steam logged for the real unit) | `0x1000` |
| `bus` | `BUS_USB` | `BUS_USB` |
| Descriptor | the real 372-byte capture, `docs/research/assets/triton-wired-1302-report-descriptor.bin` | InputPlumber's 38-byte vendor-only blob |
| Report IDs | **yes**, on input, output *and* feature | **none at all** |
| Input report | `0x42`, 54 bytes — **pass-through** | 64 bytes — **transcoded** |
| Steam adoption | **UNPROVEN** — awaits the owner's live test | **PROVEN on-device, 2026-09-01** |

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

Decoding the captured 372-byte `1302` descriptor shows its vendor collection
declares:

```
Input   id=0x42  count=53 size=8  ->  53-byte payload, 54 on the wire
Feature id=0x01  count=63 size=8  ->  64 on the wire
Output  id=0x80  count=9          ->  10 on the wire   (rumble)
Output  id=0x81  count=7          ->   8 on the wire   (haptic pulse)
```

Those are, byte for byte, the reports `src/report.rs` already decodes and
`src/lizard.rs` / `src/haptics.rs` already write. **The wired `1302` speaks the
puck's own protocol**, so the input path is a copy, not a transcode — and every
field hyprpad does not model rides along untouched, including the IMU at bytes
30+ and the undecoded tail. That is the standing advantage over `deck`, which
forfeits it.

The one thing not settled is risk **R1**: whether Steam accepts a `1302` whose
`Interface:` reads `-1` because a uhid device has no USB parent (research §3.2,
§6 R1, Q-u1).

> **If `triton` is not adopted, the fallback is one line.** Set
> `identity = "deck"` and **restart the daemon** — `identity` chooses a device
> created once at startup, so unlike the rest of `[gamepad]` it is not picked up
> by `hyprpad reload`. That path is the exact recipe SteamOS ships, and the one
> this machine has already run.

---

## 2. Report translation

### 2.1 `triton` — pass-through (`translate::puck_to_triton`)

The raw 54-byte `0x42` is copied verbatim into `UHID_INPUT2`. Two things are
changed, and only these:

| What | Where | Rule |
|---|---|---|
| Steam / guide button | byte 4, bit 0 | Cleared unless `[gamepad] forward_guide` is on. The guide is hyprpad's global chord modifier; without this every `guide+x` chord would *also* open the Steam overlay (research §4.4). |
| Quick Access | byte 2, bit 4 | Available in `StripMask`, **not currently stripped** — see §7. |

The byte-1 sequence counter is **relayed untouched**. §4.4 is explicit:
renumbering risks desync with the IMU timestamp path, and Steam tolerates gaps.

Because the `1302` descriptor numbers every report type, all three `UHID_START`
`dev_flags` bits are set and the report-id prefix is required in both directions
— which the puck's own `raw[0] == 0x42` already provides. The pass-through is
correct *because* of that, not in spite of it.

### 2.2 `deck` — transcode (`translate::puck_to_deck`)

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

**Buttons** — puck `report::Button` → Deck field, byte and mask

| Puck button | Deck field | Byte | Mask | Bit |
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

| Puck field | Deck field | Bytes | Encoding |
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
| `accel_x/y/z` | 24..30 | `report::Frame` carries no IMU. The puck's `0x42` has it at bytes 30+ but only after an enable feature report, and `src/report.rs` leaves those bytes undecoded. This is the cost of `deck`; `triton` gets them for free. |
| `pitch`, `yaw`, `roll` | 30..36 | as above |
| magnetometer | 36..44 | as above |
| `l_stick_force` / `r_stick_force` | 60..64 | capacitive stick sensors; see below |
| `_unk31` | 15 | unknown in the reference implementation too |

The puck's `Cap0`..`Cap3` bits are **not mapped**. `src/report.rs` calls their
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
| ↳ everything else | | | Logged, dropped. |
| `TriggerRumbleCommand` | `0xEB` | feature | **Translated.** `left_speed`/`right_speed` (u16 LE at bytes 5..7, 7..9 of `PackedRumbleReport`) become `haptics::Haptics::rumble`'s two `FF_RUMBLE` magnitudes. |
| `TriggerHapticPulse` | `0x8F` | feature | **Translated.** `[pad][duration][interval][count][gain]` per the kernel's `steam_haptic_pulse`; the wire side is un-XORed to a logical `haptics::Pad`, the gain dropped (the IBEX pulse struct has no gain field). |
| `TriggerHapticCommand` | `0xEA` | feature | **Translated, partly UNVERIFIED** — see §7. |
| `0x80` rumble | `0x80` | output | **Translated.** The puck's own `0x80`, read back with the exact field layout `haptics::build_rumble` writes. |
| `0x81` pulse | `0x81` | output | **Translated.** Likewise, mirroring `haptics::build_pulse`. |
| everything else | | | **Dropped** with a `HYPRSC_DEBUG` log naming the command. |

### Why not relay to the puck

Research §4.3 sketches a relay policy — forward Steam's feature writes straight
to the real device — and §5.1/§5.2 then spend two sections on the arbitration
that requires. This build takes the smaller road, the one InputPlumber's proven
implementation takes (A.4), for two structural reasons:

1. **hyprpad already owns lizard mode.** `src/lizard.rs` sends exactly the two
   settings Steam's `0x87` asks for. Forwarding would be redundant at best and a
   write race at worst — §5.1 warns about precisely this.
2. **hyprpad already owns the actuators, through one writer.** `haptics.rs` runs
   a single writer thread over the puck's only writable fd, and §5.2 requires the
   relay to go through it and "never open a second writable fd on the puck". So
   rumble and haptics are *translated into that path*, not forwarded as bytes.

**What this costs:** Steam's IMU-enable does not reach the puck, so gyro does not
start streaming by itself. Recorded in §7.

### `GET_REPORT` answers

Never late, never an error, never empty. The three handshake answers are
InputPlumber's verbatim bytes — the ones the adopted run sent:

| Selector | Answer |
|---|---|
| `GetAttributesValues` `0x83` | the 64-byte TLV blob, `[0x00, 0x83, 0x2d, …]`. For `triton`, `ATTRIB_PRODUCT_ID` (the first TLV's u32) is patched from `0x1205` to `0x1302` so the answer does not contradict the claimed identity — risk R3's prescribed mitigation. |
| `GetStringAttribute` `0xAE` | `[0x00, 0xAE, 0x14, 0x01]` + serial, padded to 64. `deck`: `1NPU7PLUMB3R`. `triton`: `FXA9961402A6C`, the captured unit's. |
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
| A game is forwarding, puck live | the current frame (re-stamped counter on `deck`; the puck's own counter untouched on `triton`) |
| Not forwarding — desktop, OSK, guide held (ranks 1, 2, 4) | **neutral** |
| Puck silent for more than `STALE_AFTER` (200 ms) | **neutral** |
| Daemon exiting | device destroyed (`UHID_DESTROY`) |

Neutral is a valid, complete, absolute-state report: buttons clear, sticks and
pads centred, triggers zero, counter advancing. The same invariant
`gamepad::PadReport::neutral` enforces for the Xbox pad — a game is never left
holding a stuck input.

The 200 ms staleness timer exists because the frame path *stops running* when the
puck sleeps; without it the last live frame would repeat forever, and whatever
was pressed at the moment the controller napped would stay pressed.

### Reconnect

**The uhid device is created once, at daemon start, and destroyed only at exit.**
It is never torn down on a focus change or a puck disconnect. Steam adopts a
controller when its hidraw node appears, so a create/destroy cycle makes it
re-detect, re-apply configs and toast about it (§5.3).

So when the puck naps:

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

Two problems, one shape. Getting a writable `/dev/uhid`, and taking the real puck
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

**It matches hidraw only.** The puck's `input`/`event` nodes are deliberately
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
->  "puck\n"        <-  "ok 5\n"  + 5 fds   every 28de:1304 hidraw node,
                                            hidraw::OPEN_FLAGS
->  anything else   <-  "err unknown request\n"  + 0 fds
```

One request per connection, then close. The client half-closes its write side, so
the broker's read always terminates.

Security posture, and why it is a small surface:

* **It takes nothing from the client but a choice between two hard-coded verbs.**
  No path, no flags, no numbers. `parse_request` is an allowlist of two exact
  words: no leading space, no different case, no arguments, nothing over 32 bytes.
* **It never reads from, writes to or ioctls a device.** It opens and hands over.
* **It caches nothing.** Every `puck` rescans `/sys/class/hidraw`, so the puck
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

One function expresses the whole preference, and everything that touches the puck
goes through it:

```rust
// src/hidraw.rs
pub enum PuckSource { Paths(Vec<PathBuf>), Fds(Vec<OwnedFd>) }
impl PuckSource { pub fn acquire() -> Option<PuckSource>; }
```

| Broker answer | What the daemon does |
|---|---|
| no socket at `/run/hyprpad/broker.sock` | direct opens; one quiet line, once per process |
| socket there, exchange failed (refused, wrong uid, mid-restart) | direct opens; one warning, once per process |
| `ok 0` | direct opens — nothing usable, and the puck may simply be away |
| `ok N`, N ≥ 1 | use the passed descriptors |

`None` means neither route worked, which is exactly the condition the startup
wait and the reconnect wait already sit on — so **both waits now poll
`PuckSource::acquire` at `RECONNECT_SCAN_INTERVAL`**, and both ask the broker
first. The answer is re-evaluated on every generation, so starting the broker
under a running daemon is picked up by the next reconnect, and stopping it falls
back.

`spawn_reader_pipeline` takes a `PuckSource` by value (a brokered generation *is*
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
  ?   broker: puck           no socket to ask
  NO  puck nodes root-only   still reachable: /dev/hidraw7 (0660 uid 0), …

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

* that udev actually applies the rule at 72 and the puck's nodes come back
  `0600 root:root` with no ACL — the ordering argument is read off the shipped
  rules and `man udev`, not off a live trigger;
* the socket-activation path (`LISTEN_FDS`); the standalone `--socket` path was
  exercised;
* a `puck` or `uhid` request that actually *succeeds* — both need root, and the
  descriptor hand-over itself is covered by unit tests over a `socketpair` with
  pipes standing in for devices;
* that Steam, with the real puck hidden, sees exactly one controller (§8 step 4).

---

## 7. Known gaps, and what is UNVERIFIED

| # | Gap | Status |
|---|---|---|
| G1 | **Does Steam adopt `triton` (`1302`) at `Interface: -1`?** | **UNPROVEN.** The one open question. Risk R1 / Q-u1. Falls back to `identity = "deck"`, which is proven. |
| G2 | `triton`'s `GetAttributesValues` blob | **UNVERIFIED.** No `1302` blob was ever captured. It is the Deck blob with `ATTRIB_PRODUCT_ID` patched to `0x1302`. Steam's log shows a real Triton survives a *failed* attribute probe (`Deck Controller PCB Serial# invalid: NA`), so a well-formed wrong answer is low severity. Replace when a real blob is captured. |
| G3 | `triton`'s chip id | **UNVERIFIED.** Modelled on the Deck answer. |
| G4 | `TriggerHapticCommand` (`0xEA`) shape | **Partly UNVERIFIED.** The `PackedHapticReport` layout is verbatim, but `PadSide`/`Intensity` are enums whose discriminants were not captured, and the command carries **no duration** — only an intensity class. hyprpad fires its own calibrated single tick (`0x190` = 400 µs, from the kernel's `steam_haptic_pulse`) on the named side, reading side as `0 = left, 1 = right`. The first live session's `HYPRSC_DEBUG` log settles it. |
| G5 | **Gyro does not reach Steam.** | By construction on `deck` (`report::Frame` has no IMU) and because Steam's IMU-enable is not relayed on either profile. On `triton` the pass-through *would* carry it if the enable reached the puck — the smallest follow-on with the largest payoff. |
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
# the real puck must be root-only, with NO '+' and no ACL
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
| `Interface: -1` then `Controller uses V1 HID protocol via USB` | the `-1` question answered — **G1 PASS** |
| **`!! Steam controller device opened for index N`** | **adopted** |
| the fake's `/dev/hidrawN` in `/proc/$(pgrep -x steam)/fd` | Steam is holding it |
| `HYPRSC_DEBUG=1` log lines `[relay] SetSettingsValues […]` | **dispositive** — Steam is actively configuring it |
| enumerated but never opened, *while the real puck is opened in the same session* | identity rejected → set `identity = "deck"`, restart, retest |

### Step 4 — the UI, and the thing that must **not** be there

Steam → Settings → Controller:

* **exactly ONE** Steam Controller listed, and **no Xbox 360 pad**. Two entries
  means either the host-integration device-hide (§6.2) is not in place, or the
  Xbox pad is still being created — the latter would be a bug in this change, as
  `kind = "steam"` must not create it;
* the device should show **trackpads, gyro and back grips**, not a generic pad.
  Missing gyro is expected today (G5).

### Step 5 — routing and haptics

* Foreground a game: input reaches it. Hold the guide: input **stops** and the
  game sees neutral, no stuck stick. Raise the OSK: same. Return to the desktop:
  same.
* The guide button itself must **not** open the Steam overlay while
  `forward_guide = false` — that is the strip mask working. Set
  `forward_guide = true`, reload, and it should.
* Trigger rumble in-game: the puck's actuators should buzz. With the game
  backgrounded they must **not** (§5.2 arbitration).
* Run with `HYPRSC_DEBUG=1` for one session and keep the `[relay]` log — it is
  the empirical answer to G4, and to what Steam actually sends a Triton (Q-u2).

---

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
  and that the Deck descriptor is unnumbered and 64/64.
* **`translate.rs`** — pass-through fidelity including bytes the decoder does not
  model; the strip mask; the untouched counter; every Deck button bit; every
  analog offset; neutral for both profiles; and a guard that every mapped button
  lands on a distinct bit.
* **`settings.rs`** — the exact `0x87` frame `src/lizard.rs` builds, decoded
  back; multi-pair and malformed frames; `0xEB`/`0x8F`/`0xEA`; and the round trip
  that `haptics.rs`'s own `0x80`/`0x81` output reports decode to what they were
  built from.
* **`relay.rs`** — the neutral stream, the advancing Deck counter, the untouched
  Triton counter, and the staleness decay.
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
  `open_node` on a real file and on a missing one; that a `PuckSource` knows
  which half it came from; `read_all` streaming from passed descriptors and
  closing its channel when they end; and that one dead path does not sink a whole
  direct generation.
* **`setup.rs`** — that the printed block names every file actually shipped under
  `packaging/` and carries every required step; that the module never spawns a
  process (it *prints* the privileged half); and the whole `--check` verdict
  table against an injected `HostView`, covering a fully installed machine, an
  untouched one, a broker that refuses, a rule installed but not yet applied, an
  absent puck, a missing Valve rule, and an `ok 0` answer.
