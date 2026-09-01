# Research: haptic feedback on the 2026 Steam Controller puck (28de:1304) from userspace

*Produced 2026-08-31. Goal: fire haptic pulses (a subtle "tick" when the OSK
cursor crosses to a new key, a firmer "click" on commit, and a buzz on a
recognized gesture) from the `hyprpad` Rust daemon, which already owns the
puck's hidraw node and disables lizard mode the same raw-HID way (`src/lizard.rs`).*

**Verification basis:** **VERIFIED(kernel)** = read from the Linux mainline
driver `drivers/hid/hid-steam.c` on the `torvalds/linux` master branch
(fetched 2026-08-31). **VERIFIED(sdl)** = read from SDL `main` branch
(`src/joystick/hidapi/...`). **INFERRED** = reasoned from those sources, not
directly confirmed on this device. Every device-write claim below is grounded in
one of the two upstream sources; **no report is invented**.

---

## 0. TL;DR

- The 2026 puck (`USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS`, `28de:1304`) is driven
  by the kernel under `STEAM_QUIRK_IBEX | STEAM_QUIRK_WIRELESS`. **Haptics moved
  to a new "IBEX" wire form for this controller** — it is NOT the gen-1/Deck
  `0x8F` feature-report form. VERIFIED(kernel).
- A single **key-crossing tick or commit click is an OUTPUT report, report id
  `0x81` (`REPORT_ID_HAPTIC_PULSE`), 8 bytes total**:
  `[0x81, side, on_us_lo, on_us_hi, off_us_lo, off_us_hi, count_lo, count_hi]`.
  VERIFIED(kernel).
- **This is the one departure from `lizard.rs`:** lizard uses a *feature* report
  (`HIDIOCSFEATURE` ioctl). IBEX haptics are *output* reports → in userspace
  that is a plain **`write()` to a writable hidraw fd**, first byte = report id
  `0x81`. Not an ioctl. VERIFIED(kernel: `hid_hw_output_report`), INFERRED(the
  userspace `write()` equivalence).
- **Tick vs. click is purely duration/count**, not gain and not a different
  report. On the IBEX path the `gain` argument is **dropped** (the pulse struct
  has no gain field); strength is the `on_us` pulse width (and repeat count).
  VERIFIED(kernel).
- **Safe to try on-device?** Low risk, but *verify empirically*. The exact
  byte layout is copied from the kernel driver for this exact device id, and only
  two documented reports are ever sent. The residual unknown is that `hid-generic`
  (not `hid-steam`) is bound here, so nothing in-kernel has exercised this output
  report on your unit — see §11.

---

## 1. Sources (and which one is authoritative)

| Source | What it gives | Authority for 0x1304 haptics |
|---|---|---|
| Linux `drivers/hid/hid-steam.c` (master) | Full IBEX + gen-1 haptic wire format, structs, pad mapping, the device-id/quirk table, and the kernel's own tuning values | **Yes — the single authoritative open implementation** |
| SDL `src/joystick/hidapi/steam/controller_constants.h` | The gen-1 command constants (`0x8F/0xEA/0xEB`), `FEATURE_REPORT_SIZE 64`, haptic settings indices | Confirms gen-1 constants only |
| SDL `src/joystick/hidapi/SDL_hidapi_steam.c` | **Does NOT implement haptics** — `HIDAPI_DriverSteam_RumbleJoystick` returns `SDL_Unsupported()` (*"You should use the full Steam Input API for rumble support"*); no Ibex/Proteus/`0x1304` branch; no `0x80/0x81` | None — Valve keeps the pulse code in closed Steam Input |

**Key finding:** the open SDL driver deliberately *doesn't* build haptic pulses
(it defers to proprietary Steam Input), and has no 2026-controller branch at all.
So the mainline kernel driver is the **only** open, implementation-grade source
for the Proteus/IBEX haptic wire format. Everything device-specific below comes
from it.

URLs (raw, master, fetched 2026-08-31):
- `https://raw.githubusercontent.com/torvalds/linux/master/drivers/hid/hid-steam.c`
- `https://raw.githubusercontent.com/libsdl-org/SDL/main/src/joystick/hidapi/steam/controller_constants.h`
- `https://raw.githubusercontent.com/libsdl-org/SDL/main/src/joystick/hidapi/SDL_hidapi_steam.c`

---

## 2. The device and its quirks — VERIFIED(kernel)

`drivers/hid/hid-steam.c` device table entry for our puck:

```c
{ /* Steam Controller (2026) Puck */
  HID_USB_DEVICE(USB_VENDOR_ID_VALVE,
		USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS),
  .driver_data = STEAM_QUIRK_IBEX | STEAM_QUIRK_WIRELESS
},
```

`USB_VENDOR_ID_VALVE = 0x28de`, `USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS = 0x1304`
(matches the `28de:1304` this daemon already targets in `hidraw.rs`). Because
`STEAM_QUIRK_IBEX` is set, **every haptic call takes the IBEX branch** — this is
the branch that matters for us.

Pad selector constants (VERIFIED(kernel)):

```c
#define STEAM_PAD_LEFT 0
#define STEAM_PAD_RIGHT 1
#define STEAM_PAD_BOTH 2
```

---

## 3. The IBEX haptic wire format — the authoritative one for 0x1304

### 3.1 The structs — VERIFIED(kernel)

```c
struct steam_ibex_haptic_rumble {
	u8 type;
	__le16 intensity;
	struct {
		__le16 speed;
		u8 gain;
	} __packed left, right;
} __packed;
static_assert(sizeof(struct steam_ibex_haptic_rumble) == 9);

struct steam_ibex_haptic_pulse {
	u8 side;
	__le16 on_us;
	__le16 off_us;
	__le16 repeat_count;
} __packed;
static_assert(sizeof(struct steam_ibex_haptic_pulse) == 7);

struct steam_ibex_output_report {
	u8 id;
	union {
		struct steam_ibex_haptic_rumble rumble;
		struct steam_ibex_haptic_pulse pulse;
	};
} __packed;
```

Output report ids (VERIFIED(kernel)):

```c
enum {
	/* Output */
	REPORT_ID_HAPTIC_RUMBLE		= 0x80,
	REPORT_ID_HAPTIC_PULSE		= 0x81,
	REPORT_ID_HAPTIC_COMMAND	= 0x82,
	REPORT_ID_HAPTIC_LFO_TONE	= 0x83,
	REPORT_ID_HAPTIC_LOG_SWEEP	= 0x84,
	REPORT_ID_HAPTIC_SCRIPT		= 0x85,
};
```

### 3.2 The pulse path (this is what OSK ticks/clicks use) — VERIFIED(kernel)

```c
static inline int steam_haptic_pulse(struct steam_device *steam, u8 pad,
			u16 duration, u16 interval, u16 count, u8 gain)
{
	int ret;

	if (pad < STEAM_PAD_BOTH)
		pad ^= 1;

	if (steam->quirks & STEAM_QUIRK_IBEX) {
		struct steam_ibex_output_report *report =
			kzalloc(sizeof(struct steam_ibex_output_report), GFP_KERNEL);
		...
		report->id = REPORT_ID_HAPTIC_PULSE;
		report->pulse.side = pad;
		put_unaligned_le16(duration, &report->pulse.on_us);
		put_unaligned_le16(interval, &report->pulse.off_us);
		put_unaligned_le16(count,    &report->pulse.repeat_count);

		ret = hid_hw_output_report(steam->hdev, (u8 *)report, 8);
		kfree(report);
	} else {
		/* gen-1 / Deck path — feature report, 10 bytes, id 0x8F */
		u8 report[10] = {ID_TRIGGER_HAPTIC_PULSE, 8, pad};
		report[3] = duration & 0xFF; report[4] = duration >> 8;
		report[5] = interval & 0xFF; report[6] = interval >> 8;
		report[7] = count & 0xFF;    report[8] = count >> 8;
		report[9] = gain;
		ret = steam_send_report(steam, report, 10);
	}
	return ret;
}
```

**The exact 8-byte OUTPUT report for one pulse on the puck** (little-endian):

| Byte | Field | Meaning |
|---|---|---|
| 0 | `id` = `0x81` | `REPORT_ID_HAPTIC_PULSE` (this byte IS the report id) |
| 1 | `side` | actuator: **`0` = right pad, `1` = left pad, `2` = both** — see §3.4 on the XOR |
| 2–3 | `on_us` (le16) | pulse **ON** time, microseconds → the "strength"/feel knob |
| 4–5 | `off_us` (le16) | gap between pulses, microseconds |
| 6–7 | `repeat_count` (le16) | number of on/off cycles |

Total length: **8 bytes**, and the kernel sends exactly 8 (`hid_hw_output_report(..., 8)`).
Note: **output reports are NOT padded to 64 bytes** here (contrast the 64-byte
feature frames in `lizard.rs`). VERIFIED(kernel).

- `gain` (the function's last arg) is **used only on the gen-1 branch** (`report[9]`).
  On the IBEX branch there is **no gain field** in `steam_ibex_haptic_pulse`, so
  gain is silently discarded. On the puck, pulse strength is `on_us` + `repeat_count`
  only. VERIFIED(kernel).

### 3.3 The rumble path (for completeness / a longer gesture buzz) — VERIFIED(kernel)

IBEX rumble is report id `0x80`, **10 bytes**, sent the same way (output report):

```c
report->id = REPORT_ID_HAPTIC_RUMBLE;              /* byte 0 = 0x80 */
put_unaligned_le16(intensity,   &report->rumble.intensity);
put_unaligned_le16(left_speed,  &report->rumble.left.speed);
report->rumble.left.gain  = left_gain;
put_unaligned_le16(right_speed, &report->rumble.right.speed);
report->rumble.right.gain = right_gain;
ret = hid_hw_output_report(steam->hdev, (u8 *)report, 10);
```

Wire layout (10 bytes): `[0x80, type(=0), intensity_lo, intensity_hi,
left_speed_lo, left_speed_hi, left_gain, right_speed_lo, right_speed_hi,
right_gain]`. (`type` is left 0 by the driver.) This is a continuous
force-feedback rumble, not a discrete tick — for the OSK use the **pulse** form.
Rumble is only a candidate for a longer "gesture recognized" buzz (§9), and even
then a multi-repeat pulse is simpler.

### 3.4 The `side` byte is XOR-inverted vs. the logical pad — VERIFIED(kernel)

`steam_haptic_pulse` runs `if (pad < STEAM_PAD_BOTH) pad ^= 1;` **before** writing
`report->pulse.side = pad`. So the firmware's actuator numbering is swapped
relative to the driver's logical LEFT/RIGHT:

| You want to buzz | Logical `STEAM_PAD_*` | Byte 1 on the wire |
|---|---|---|
| **Left pad** | `STEAM_PAD_LEFT` = 0 | `1` |
| **Right pad** | `STEAM_PAD_RIGHT` = 1 | `0` |
| Both | `STEAM_PAD_BOTH` = 2 | `2` (unchanged) |

**To be safe, mirror the kernel exactly:** take a logical pad `0/1/2`, apply
`if (p < 2) p ^= 1`, then emit. That reproduces the driver's behaviour bit-for-bit
and side-steps any "is left really left?" doubt. (Confirm the physical side once
on-device anyway — §11.)

---

## 4. Which actuator = which pad, and tick vs. click

- **Actuator ↔ pad:** the puck has a haptic actuator behind each trackpad; `side`
  selects it (with the §3.4 inversion). The OSK's left cursor → left pad tick;
  right cursor → right pad tick. This matches the Deck OSK, which fires the
  crossing tick on *that* pad only (`osk-technology.md` §4.3:
  `PlayHaptic(source, leftPad|rightPad, HapticType.Tick, …)`). VERIFIED(kernel:
  per-pad `side`); the pad↔OSK-cursor mapping is INFERRED from the OSK design.
- **Tick vs. click is duration/count, not a different report and not gain.** Both
  are `REPORT_ID_HAPTIC_PULSE` (`0x81`); a "tick" is a single short pulse, a
  "click" is a longer/repeated one. The Deck client models this as
  `HapticType { Tick=1, Click=2 }` with intensity/gain params
  (`osk-technology.md` §4.3), but on the **puck's HID path those higher-level
  knobs collapse to `on_us`/`repeat_count`** because the IBEX pulse struct exposes
  nothing else. VERIFIED(kernel).

---

## 5. Concrete tuning starting points

All values below are `on_us` / `off_us` / `repeat_count` for the `0x81` pulse.
The kernel's **own** mode-switch feedback gives calibrated reference points —
VERIFIED(kernel):

```c
steam_haptic_pulse(steam, STEAM_PAD_RIGHT, 0x190, 0,     1,    0);   /* single 400us tick */
if (gamepad_mode)
	steam_haptic_pulse(steam, STEAM_PAD_LEFT, 0x14D, 0x14D, 0x2D, 0); /* 333us on/off x45 ~ buzz */
else
	steam_haptic_pulse(steam, STEAM_PAD_LEFT, 0x1F4, 0x1F4, 0x1E, 0); /* 500us on/off x30 ~ buzz */
```

Derived starting points (INFERRED, built on the VERIFIED values above — **tune
on-device**):

| Feel | `side` | `on_us` | `off_us` | `repeat_count` | Rationale |
|---|---|---|---|---|---|
| **(a) key-crossing tick** (subtle) | that pad | `0x00C8` (200µs) | `0` | `1` | shorter than the kernel's 400µs mode tick, so it stays light during fast typing |
| **(a′) crossing tick** (firmer) | that pad | `0x0190` (400µs) | `0` | `1` | the kernel's exact single-tick value; use if 200µs feels too faint (Valve deliberately *increased* crossing strength over time — `osk-technology.md` §4.3) |
| **(b) commit click** (firm) | that pad | `0x0258` (600µs) | `0x012C` (300µs) | `2` | a short double-pulse reads as a distinct "clunk", clearly heavier than the tick |
| **(c) gesture buzz** | `STEAM_PAD_BOTH` or right | `0x01F4` (500µs) | `0x01F4` (500µs) | `0x0A`–`0x1E` (10–30) | a ~10–30 ms brrr, the kernel's mode-switch buzz shape |

Suppress the tick when the crossing target is "no key" (a gap), exactly as the
Deck was fixed to do (`osk-technology.md` §4.3 — the "double-thunk" bug). That is
a call-site decision (§10), not a wire-format one.

---

## 6. Feature settings that gate haptics — VERIFIED(sdl), optional preflight

SDL `controller_constants.h` defines haptic *settings* (written via the same
`ID_SET_SETTINGS_VALUES = 0x87` feature report `lizard.rs` already uses):

- `SETTING_HAPTICS_ENABLED` (index **70**)
- `SETTING_HAPTIC_MASTER_GAIN_DB` (index **76**)
- `SETTING_HAPTIC_INTENSITY` (index **79**)
- `HAPTIC_PULSE_*` priority flags (`0x0000`..`0x0003`)

These are **not required** to fire a pulse (the kernel's mode-switch feedback
sends none of them first), and their defaults are presumably "enabled". Flagged
only as a fallback: if pulses appear to do nothing on-device, try writing
`SETTING_HAPTICS_ENABLED = 1` (and a non-attenuated master gain) via the existing
`0x87` feature path before concluding the pulse report is wrong. INFERRED that
this is unnecessary; VERIFIED that the setting indices exist.

---

## 7. Gen-1 / Deck form (for contrast and as a fallback only)

If the IBEX output report were ever rejected on your unit, the gen-1 form is the
best-known alternative — but note it targets a *different* protocol generation
and would most likely be **wrong** for the puck. VERIFIED(kernel + sdl):

- `ID_TRIGGER_HAPTIC_PULSE = 0x8F`, `ID_TRIGGER_RUMBLE_CMD = 0xEB`,
  `ID_TRIGGER_HAPTIC_CMD = 0xEA`.
- Sent as a **FEATURE report** (`steam_send_report` → `hid_hw_raw_request(...,
  HID_FEATURE_REPORT, HID_REQ_SET_REPORT)`) — i.e. via `HIDIOCSFEATURE` like
  `lizard.rs`. On IBEX the feature frame is prefixed with
  `REPORT_ID_FEATURES_CONTROLLER` (0x01) and SDL pads it to `FEATURE_REPORT_SIZE
  = 64`.
- Pulse body: `[0x8F, 8, pad, on_lo, on_hi, off_lo, off_hi, count_lo, count_hi,
  gain]` (10 bytes, then zero-padded).

**Do not lead with this on the puck.** The kernel explicitly routes IBEX devices
away from it. It is documented here only as the fallback the "verify on-device"
step would fall back to.

---

## 8. Newer-protocol caveat (item 4) — answered

**Yes, haptics moved to a newer command form for the 2026 controller, and we
have the exact form.** VERIFIED(kernel):

- Gen-1/Deck: feature report, command id `0x8F` inside a `0x01`/64-byte frame.
- 2026 puck (IBEX/Proteus, `0x1304`): **dedicated output reports** `0x80`
  (rumble) / `0x81` (pulse) with the compact fixed structs in §3, sent via
  `hid_hw_output_report`.

This is not the "uncertain, use gen-1 as a guess" case the task worried about —
the mainline driver contains the **device-specific** IBEX path keyed on the exact
`0x1304` quirk, so the `0x81` pulse report *is* the puck's real haptic report.
The one honest uncertainty is empirical, not documentary: on this machine the
puck binds `hid-generic`, so the in-kernel `hid-steam` path has never actually
emitted this report on your unit — we are replicating it over raw hidraw. Hence
"verify on-device" (§11), not "guess".

---

## 9. Concurrency with Steam (item 3)

- hidraw is **not exclusive** (`lizard.rs` doc-comment, `docs/03`): hyprpad can
  open the node writable and send output reports while Steam also holds it.
- **Risk if both write:** haptic output reports from two writers **interleave with
  no arbitration** — hyprpad's ticks and Steam Input's own haptics would race and
  could feel jittery or cut each other off. There is no per-report locking.
- **Mostly moot in the intended design:** hyprpad *owns* the puck (Steam masked /
  denied the device, `docs/experiments/w12-device-denial.md`), so Steam isn't
  writing haptics. Flag it only for the un-masked configuration: if Steam is live,
  don't also drive haptics from hyprpad.
- A haptic write needs a **writable fd** (`O_RDWR`), same as `lizard.rs`'s
  `HIDIOCSFEATURE`. The daemon's *input* path opens read-only; haptics must open
  its own writable fd (or reuse lizard's pattern of opening read-write per send).

---

## 10. Where the daemon would call haptics — architecture note

Important wrinkle from reading `src/run.rs` and `src/osk.rs`: **the daemon does
not know when the OSK cursor crosses to a new key.** Hit-testing
(`elementFromPoint` over `data-key`) happens inside the **`hyprpad-osk` child
process**; the daemon only streams normalized cursor positions to it over the
child's stdin (`osk.rs`, one-way control channel). But the daemon is the one that
**owns the writable puck node**. So responsibilities split:

| Event | Detected by | Owns hidraw | Path to haptics |
|---|---|---|---|
| **Commit click** | daemon (`route_osk` sees the pad click-down edge, `run.rs:383-389`) | daemon | **direct — easiest win, no protocol change** |
| **Gesture-recognized buzz** | daemon (`handle_gesture`, `run.rs:264`) | daemon | **direct** |
| **Key-crossing tick** | **OSK child** (hit-test change) | daemon | needs a **back-channel** OSK→daemon (e.g. OSK prints `tick L` / `tick R` on stdout; daemon reads it and fires `haptic_tick`) |

So:
1. **Commit click** — fire `haptic_click(pad)` right in `route_osk`'s commit
   loop (`run.rs:383-389`), where `PadLeftClick`/`PadRightClick` (and the trigger
   full-pull) edges are already detected. Zero new plumbing.
2. **Gesture buzz** — fire in `handle_gesture` on a recognized `GuideChord` /
   `GuideStickFlick` (`run.rs:264-303`).
3. **Key-crossing tick** — add a tiny **back-channel**: have `hyprpad-osk` emit a
   line (`tick L`/`tick R`, and suppress it on a "no key" crossing per §5) on its
   stdout when its own hit-test changes; the daemon (which currently inherits the
   child's stdout) reads those lines and calls `haptic_tick(pad)`. This keeps the
   authoritative crossing detection in the OSK (where the layout lives) and the
   device write in the daemon (where the fd lives). It is the only item needing a
   protocol addition — do commit-click first, then add the tick channel.

---

## 11. Rust implementation sketch — `src/haptics.rs`

Mirrors `lizard.rs`'s structure and its `puck_nodes()` reuse, **but writes an
output report with `write()` instead of the `HIDIOCSFEATURE` ioctl** (§0/§3.2).
This is a sketch, not finished code — labelled INFERRED where it goes beyond what
the kernel source states.

```rust
//! Haptic pulses on the puck (28de:1304, IBEX/Proteus).
//!
//! Unlike `lizard.rs` (feature reports via HIDIOCSFEATURE), IBEX haptics are
//! HID *output* reports: a plain write() to a writable hidraw fd, first byte =
//! report id 0x81. Wire format copied from the kernel `hid-steam.c` IBEX path.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// REPORT_ID_HAPTIC_PULSE (kernel `hid-steam.c`). Byte 0 of the output report.
const REPORT_ID_HAPTIC_PULSE: u8 = 0x81;

/// Logical pad, matching the kernel's STEAM_PAD_* selectors.
#[derive(Clone, Copy)]
pub enum Pad { Left = 0, Right = 1, Both = 2 }

/// The firmware's `side` byte is XOR-inverted vs. the logical pad for L/R
/// (`if (pad < STEAM_PAD_BOTH) pad ^= 1;` in steam_haptic_pulse). Mirror it.
fn wire_side(pad: Pad) -> u8 {
    let p = pad as u8;
    if p < 2 { p ^ 1 } else { p }
}

/// Build the 8-byte `0x81` pulse output report (all fields little-endian).
/// on_us = pulse ON width (strength), off_us = gap, count = repeat cycles.
fn build_pulse(pad: Pad, on_us: u16, off_us: u16, count: u16) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = REPORT_ID_HAPTIC_PULSE;
    b[1] = wire_side(pad);
    b[2..4].copy_from_slice(&on_us.to_le_bytes());
    b[4..6].copy_from_slice(&off_us.to_le_bytes());
    b[6..8].copy_from_slice(&count.to_le_bytes());
    b
}

// Tuning starting points (§5) — verify on-device.
pub fn haptic_tick(h: &mut Haptics, pad: Pad)  { h.pulse(build_pulse(pad, 0x0190, 0,      1)); }
pub fn haptic_click(h: &mut Haptics, pad: Pad) { h.pulse(build_pulse(pad, 0x0258, 0x012C, 2)); }
pub fn haptic_buzz(h: &mut Haptics)            { h.pulse(build_pulse(Pad::Both, 0x01F4, 0x01F4, 0x1E)); }

/// Holds writable fds to the puck nodes so frequent ticks don't reopen per pulse.
/// (Open-per-pulse, like lizard.rs, also works for the rarer click/buzz.)
pub struct Haptics { nodes: Vec<PathBuf>, fds: Vec<File> }

impl Haptics {
    pub fn open() -> std::io::Result<Self> {
        let nodes = crate::hidraw::puck_nodes()?;
        let fds = nodes.iter()
            .filter_map(|n| OpenOptions::new().read(true).write(true).open(n).ok())
            .collect();
        Ok(Self { nodes, fds })
    }

    /// Send the report to every open puck node. The active-slot node accepts it;
    /// inactive slots may error/STALL (EPIPE) — tolerated, exactly like lizard.
    /// INFERRED: writing to all nodes mirrors lizard's "try every node" strategy;
    /// confirm which node the active controller uses on-device.
    fn pulse(&mut self, report: [u8; 8]) {
        for fd in &mut self.fds {
            let _ = fd.write_all(&report); // output report: write(), not ioctl
        }
        let _ = &self.nodes; // (retain for reopen-on-error, omitted here)
    }
}
```

Call sites in `src/run.rs` (see §10):

```rust
// build once near the OSK handle; degrade gracefully if it can't open (like OskHandle)
let mut haptics = crate::haptics::Haptics::open().ok();

// in route_osk's commit loop (run.rs:383-389), alongside osk.commit(..):
PadLeftClick  | TriggerL2Full => { osk.commit(OskPad::Left);
    if let Some(h) = haptics.as_mut() { haptics::haptic_click(h, Pad::Left); } }
PadRightClick | TriggerR2Full => { osk.commit(OskPad::Right);
    if let Some(h) = haptics.as_mut() { haptics::haptic_click(h, Pad::Right); } }

// key-crossing tick: on a `tick L|R` line read back from the OSK child's stdout
// (new back-channel, §10) -> haptics::haptic_tick(h, pad)

// gesture buzz: in handle_gesture on a recognized GuideChord/flick ->
//   haptics::haptic_buzz(h)
```

Notes:
- `write_all` of exactly 8 bytes reproduces the kernel's `hid_hw_output_report(...,
  8)`. **Do not** zero-pad to 64 (that's the feature-report convention, not this
  output report). VERIFIED(kernel) for the length; INFERRED that hidraw `write()`
  routes it identically to `hid_hw_output_report` (it does for numbered output
  reports, but confirm — §11 on-device).
- Frequent ticks: hold the fds open (as above) rather than reopening per pulse.

---

## 12. Prioritized recommendation

1. **Ship commit-click first.** It needs no protocol change: the daemon already
   detects the pad click-down in `route_osk`, and it owns the writable node. Wire
   `haptic_click(pad)` there with the `0x81` report from §3.2 and the §5 "click"
   values.
2. **Add the key-crossing tick via an OSK→daemon back-channel** (§10). This is the
   headline Deck feel, but it requires the OSK to report hit-test changes back.
   Suppress the tick on "no key" crossings (§5) to avoid Valve's double-thunk.
3. **Gesture buzz** in `handle_gesture` — trivial once `Haptics` exists.
4. Keep `Haptics::open()` **fallible and non-fatal**, exactly like `OskHandle` and
   `VirtualPointer` (`run.rs`): a puck with no haptic actuator or a failed open
   must degrade to a silent no-op, never take the daemon down.
5. Only ever send the two documented reports (`0x81` pulse, optionally `0x80`
   rumble). Do not touch `0x82`–`0x85` (command/LFO/log-sweep/script) — out of
   scope and unverified for this use.

## 13. Safe to try on-device?

**Yes, with low risk — but verify empirically before trusting the feel.**

Why low risk:
- The byte layout is copied verbatim from the mainline kernel driver's IBEX path,
  keyed on this exact `0x1304` device id — not guessed, not gen-1-extrapolated.
- Only a transient haptic pulse is triggered; no persistent state, no settings
  write, no factory reset. A wrong pulse at worst does nothing or buzzes the wrong
  pad — it cannot brick or reconfigure the device (contrast the lizard settings
  writes, which `lizard.rs` already does safely).
- hidraw is non-exclusive and the daemon already owns the node.

What to verify on first run (the genuine unknowns):
1. **That the `0x81` output report fires at all** over raw hidraw with
   `hid-generic` bound (nothing in-kernel has exercised it on your unit; §8).
   If silent, try `SETTING_HAPTICS_ENABLED = 1` via the existing `0x87` path (§6),
   then, only as a last resort, the gen-1 `0x8F` feature form (§7).
2. **Physical side:** confirm `side` byte `1` really buzzes the *left* pad (the
   §3.4 XOR); swap if the firmware on your unit disagrees.
3. **Which node** the active controller slot is on — writing to all puck nodes
   (as lizard does) covers this, but confirm you're not spamming a wrong node.
4. **Feel/tuning:** the §5 values are starting points; adjust `on_us`/`count` live.

Start with a one-shot manual test (a single `haptic_tick(Pad::Right)` behind a
debug key) before wiring it into the OSK loop.

---

## 14. Verified on-device — 2026-08-31 (implementation of `src/haptics.rs`)

One-shot pulses were sent to the live puck (controller awake, a `hyprpad run`
daemon owning it, `hid-generic` bound) by opening each node `O_RDWR` and
`write()`ing the 8 bytes. Five reports: tick right `81 00 90 01 00 00 01 00`,
tick left `81 01 90 01 …`, click left/right `81 0x 58 02 2c 01 02 00`, buzz both
`81 02 f4 01 f4 01 0a 00`.

1. **The `0x81` output report fires** (§13 item 1): every write returned 8 bytes
   written on **every** one of the five puck nodes, in ~1–3 ms (one 19–31 ms
   outlier on the first touch of `/dev/hidraw11`). No `EPIPE`, no STALL, no
   `SETTING_HAPTICS_ENABLED` preflight needed (§6 was not exercised).
2. **Which node** (§13 item 3): *the write-success signal cannot tell you.* An
   output report is fire-and-forget, so all five nodes accept it — unlike the
   *feature* reports in `lizard.rs`, where the inactive slots STALL and the
   "try every node, keep what works" strategy converges. Input identifies the
   live slot instead: a 1 s read poll saw **268 reports/s (ids `0x42`, `0x7B`) on
   `/dev/hidraw7` and 0 on the other four**. `src/haptics.rs` therefore opens
   every node read-write, `poll`s briefly for readability, and pulses only the
   node(s) that streamed — falling back to the shotgun if nothing is streaming.
3. **Physical side** (§13 item 2) and **feel/tuning** (item 4) still need a human
   in the loop: the agent that sent these pulses could not feel them.
