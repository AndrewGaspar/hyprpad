# Design: the uhid relay — a virtual Valve controller Steam adopts

*Implements the device-independent core of `docs/research/uhid-steam-controller.md`.
Code: `src/uhid.rs` and `src/uhid/{profile,translate,settings,relay}.rs`, wired
into `src/run.rs` beside `src/gamepad.rs`.*

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

`acquire_uhid` is the seam, and it is deliberately three lines: open `/dev/uhid`
read-write and return the error. **On this machine it fails with `EACCES`** —
the node is `crw------- 1 root root 10, 239` and no shipped udev rule opens it.
That is the expected outcome today. The daemon logs it once, with the rule that
would fix it, and carries on with no game sink.

`UHID_CREATE2` — unlike the legacy `UHID_CREATE` — has **no
`f_cred != current_cred()` check**, so a descriptor opened by another process is
usable as-is. Replacing `acquire_uhid` with a `SCM_RIGHTS` receive from a
privileged helper needs no other change anywhere in the module.

The path that opens the puck's hidraw nodes (`src/hidraw.rs`) is untouched by
this feature.

---

## 6. Host integration — **a separate task, out of scope here**

Two things this build deliberately does not do. Both are privileged, both are
somebody else's ticket, and the code above is shaped so neither requires
re-opening it.

1. **Getting a writable `/dev/uhid`.** Options, from the research doc §1.4 and
   §6 R2:
   * a shipped udev rule —
     `KERNEL=="uhid", SUBSYSTEM=="misc", MODE="0660", TAG+="uaccess", OPTIONS+="static_node=uhid"`.
     `static_node=` is **mandatory**: the node is materialised by
     `systemd-tmpfiles` before udev runs, so a plain rule never applies;
   * a minimal root helper that opens the node and passes the fd over a unix
     socket;
   * running the daemon as a system service.

   Whichever is chosen, the deliverable into this code is one function returning
   an `OwnedFd`.

2. **Hiding the real puck from Steam.** Without it Steam sees *both* the real
   `28de:1304` (five hidraw nodes, five controller slots) and the fake, and a
   game double-counts every press — risk R10. The battle-tested pattern is hhd's
   `src/hhd/controller/lib/hide.py`: a generated
   `/run/udev/rules.d/95-…-devhide-*.rules` that sets `MODE:="000"` and
   `TAG-="uaccess"` on the hidden device's hidraw and input nodes, then
   `udevadm control --reload-rules` and a remove/add trigger. It needs root — the
   same prerequisite as (1), so the two decisions collapse into one.

   `docs/experiments/w12-device-denial.md` and research §8.3 carry the detail.

Also out of scope: `hyprpad setup` does not install any of it, and no udev rule
is shipped in this change.

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

Everything below needs the host-integration half (§6) first — without a writable
`/dev/uhid` the daemon logs one line and runs with no relay, which is step 0's
expected result today.

### Step 0 — confirm the current, expected failure

```bash
hyprpad run          # with kind = "steam" configured
```

Expect exactly one line naming `/dev/uhid` and the udev rule, and:

```bash
jq .relay "$XDG_RUNTIME_DIR/hyprpad/status.json"    # "none"
```

### Step 1 — grant access, restart, confirm the device exists

```bash
printf 'KERNEL=="uhid", SUBSYSTEM=="misc", MODE="0660", TAG+="uaccess", OPTIONS+="static_node=uhid"\n' \
  | sudo tee /etc/udev/rules.d/72-hyprpad-uhid.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
getfacl /dev/uhid            # expect an ACL for your uid
```

Restart the daemon, then:

```bash
jq .relay "$XDG_RUNTIME_DIR/hyprpad/status.json"    # "steam"

# the virtual node, and that Valve's own rule gave it uaccess
for h in /sys/class/hidraw/hidraw*; do
  grep -qi '28DE:1302\|28DE:12F0' "$h/device/uevent" && { echo "== $h"; cat "$h/device/uevent"; }
done
getfacl /dev/hidrawN         # expect user:<you>:rw-
```

The daemon's startup line names the identity and VID:PID it created.

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
  that exists rather than the one configured.
