# Research: running the 2026 Steam Controller over Bluetooth

*Produced 2026-09-03. The owner wants Bluetooth as the **daily** transport on the
laptop — "having to plug in the puck is annoying, a USB cable dangling out of the
side" — while the dongle stays with the tower. So this note is not a
dongle-versus-Bluetooth comparison; it is the shortest path to a working
Bluetooth setup, the cost of taking it, and the two-machine story.*

**Evidence tags.** `VALVE` = Valve's own support/store pages, quoted verbatim.
`PRESS` = a hands-on review with a stated method. `KERNEL` / `SDL` / `BLUEZ` =
read in upstream source at the cited file:line (mainline `torvalds/linux` master,
`libsdl-org/SDL` main, `bluez/bluez` 5.87 and master), fetched today.
`LOCAL` = observed on this machine, read-only. `COMMUNITY` = forum/issue tracker,
labelled as such. `UNVERIFIED` = a gap this note could not close without pairing
the controller, which it deliberately did not do.

**Nothing in this note touched the device.** No pairing, no HID writes, no daemon
restarts, nothing under `~/.config`. Everything about the live Bluetooth *link*
is still prediction; everything about the *protocol* is now read from source.

---

## 0. Verdict

**The pairing chord is documented by Valve, and the 2015 chords do not carry
over.** Power the controller off, then hold **B + R1 + Steam** and *keep holding
past the second chime* until the LED double-pulses blue. It advertises as
`Steam Ctrl (BT) FXA…`. §1 has the verbatim quotes and the LED table.

**The protocol side is far better than expected, and is no longer guesswork.**
Mainline Linux gained the 2026 controller in 2026-08 (Vicki Pfau, Valve), and it
settles every framing question this note was written to ask:

* **The Bluetooth product id is `0x1303`** — `USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE`,
  `drivers/hid/hid-ids.h:1390`, bound as `HID_BLUETOOTH_DEVICE` at
  `hid-steam.c:2756-2760`. That single number is the whole input phase 1 needed.
* **The BLE input report is `0x45`, 46 bytes — and it is byte-identical to the
  familiar `0x42` for its entire length.** The kernel's own table
  (`hid-steam.c:2325-2355`) documents `0x42` as `0x45` *plus* a trailing
  quaternion at bytes 46-53; both ids are dispatched into the same handler
  (`:2552`, `:2571`). **hyprpad's `Frame::decode` reads no byte past 29**, so
  every field it uses is present. The decode fix is a guard, not a decoder.
* **Feature reports, haptics and rumble are unchanged.** Report id 1, 64 bytes,
  `0x87 SET_SETTINGS_VALUES`; output `0x80`/`0x81`. `STEAM_QUIRK_BLE` appears
  three times in 2784 lines and touches none of it. **There is no BLE chunking**
  — the 2015 controller's segmented `0x03` framing does not apply here.
* **Exactly one hidraw node over Bluetooth**, not five (`hid-steam.c:1618-1621`,
  *"There is only one BLE HID interface"*).

**So hyprpad's port is genuinely small.** Phase 1 — discovery, the udev hide
rule, the broker wording, a status field — plus a two-line decode guard and a
46→54 re-frame in the relay. §5 is the whole of it, and §5 is a day.

**The problems are all in the transport, not the protocol, and there are three.**

1. **Linux's default LE connection interval is 30–50 ms** —
   `net/bluetooth/hci_core.c:2453-2456`, `le_conn_min_interval = 0x0018`,
   `le_conn_max_interval = 0x0028`. That is **20–33 Hz** out of the box, against
   the puck's 250 Hz. The BLE floor is 7.5 ms (133 Hz) and the controller can
   request it, but nothing guarantees it does. §2.5 has the levers.
2. **Feature-report writes cost a full ATT round trip** — 30–100 ms each at those
   defaults, against ~1 ms over USB. `src/lizard.rs` re-sends its settings frame
   every 30 s, which is fine; anything chattier is not. And **output reports have
   no backpressure at all** (`hog-lib.c` `forward_report`, fire-and-forget with
   NULL callbacks), so hyprpad's OSK haptic ticks need rate-limiting rather than
   the free-running writes they are today.
3. **`COMMUNITY`, and this is the one that decides whether the plan is viable:**
   [steam-for-linux#13383](https://github.com/ValveSoftware/steam-for-linux/issues/13383)
   (opened 2026-07-03, **still open**) reports that the 2026 controller pairs
   over Bluetooth but **never reconnects after being powered off — you must
   delete the bond and re-pair every time**, while a DualSense on the same host
   reconnects normally. If that reproduces on your laptop, Bluetooth is not a
   daily transport yet, and no amount of hyprpad work changes that.

**Recommendation: run §7 before writing any code.** It is an hour, read-only
except for the pairing itself, and its *first* job is now to reproduce or refute
#13383. If the controller reconnects cleanly on your hardware, do phase 1 and
enjoy it. If it does not, the blocker is Valve's firmware or BlueZ, and the
honest answer is to wait — with a kernel 7.3 upgrade (which brings `hid-steam`
support for `0x1303`) as the next thing to retest against.

**What you are accepting even in the good case.** `PRESS`, GamersNexus, 499
clicks per mode: puck 21.6 ms mean σ 3.1, Bluetooth 37.3 ms **σ 20.6**. The mean
is fine for driving a window manager. The jitter lands on gesture thresholds and
trackpad cursor motion, and §4 is the accounting.

---

## 1. Pairing — the procedure, verbatim

**Source.** Valve, *"Steam Controller (2026) — Feature & Troubleshooting guide"*,
<https://help.steampowered.com/en/faqs/view/33E8-5EDF-24E6-4CFB>, FAQ version 17,
last edited 2026-06-29. And Valve, *"Reference — Steam Controller LED"*,
<https://help.steampowered.com/en/faqs/view/6AB9-3A71-ED45-3FB3>, last edited
2026-08-06. (Both pages inject their body from a `data-faqstore` JSON attribute,
so a naive fetch returns only page chrome — decode the attribute.)

### 1.1 Into Bluetooth pairing mode — VALVE

> **Bluetooth**:
> - To **start** the Controller in Bluetooth mode, press and hold **B + R1 + Steam**
>   until you hear a chime and the LED turns blue.
> - To **pair** the Controller in Bluetooth mode, make sure your Controller is
>   powered off, then press and hold **B + R1 + Steam**. The Controller will chime
>   and power on, but keep holding this chord until the second chime and you see
>   the LED rapidly blink with double blue pulses to indicate it is ready to pair.
> - Navigate to your device's Bluetooth settings and find the Steam Controller. It
>   will identify itself as **'Steam Ctrl (BT) FXA…'**
> - Once pairing is complete, the LED should show solid blue.

Three things worth pulling out, because they are what people get wrong:

* **The same chord does both.** "Start in Bluetooth mode" and "advertise for
  pairing" differ only by *how long you hold*. One chime = start. Second chime =
  pairing. The commonest failure is letting go at the first chime.
* **It must start from powered off.** Hold Steam ~5 s until the chime and the LED
  goes out first. A mode switch from a running controller does not work.
* **There is no dedicated pairing button and no auto-advertise.** Removing the
  puck does not make the controller fall back to Bluetooth.

The advertised name carries the controller's own serial prefix. This unit's
wired serial is `FXA9961402A6C` (`LOCAL`, and pinned as the relay's `uniq` in
`src/uhid/profile.rs:539-543`), so expect `Steam Ctrl (BT) FXA99614…` — a useful
way to tell it from someone else's in a scan.

### 1.2 Back to the puck, and the other modes — VALVE

> - To use Puck wireless mode (right slot), press and hold **A + R1 + Steam** until
>   you hear a chime and the LED turns white.
> - To use Puck wireless mode (left slot), press and hold **A + L1 + Steam** until
>   you hear a chime and the LED turns white.

Wired USB-C: plug in while the controller is off, or hold Steam while plugging in
if it is already on in another mode. LED green.

### 1.3 Does it remember both?

**Two puck slots plus one Bluetooth bond, held simultaneously; the controller
connects to whichever was last used on power-on.** That is the fact the whole
two-machine setup rests on, so it gets its own section with the quotes and the
consequences — including the reconnect bug that may undermine it: **§6**.

### 1.4 The LED — VALVE

Colour is the transport, pattern is the state. The LED sits above the Steam
button.

| Colour | Transport |
|---|---|
| **White** | Puck, or a Steam Machine's built-in adapter |
| **Blue** | Bluetooth |
| **Green** | USB-C wired |
| Orange | No connection (charging only) |
| Red | Software update mode |
| Dim red | Low battery, ~1 hr left |

| Pattern | State |
|---|---|
| Solid | Connected, working normally |
| Blinking ~2×/s | Trying to connect, or lost the connection |
| Breathing | Connected and charging |
| **Rapid double pulse** | **Advertising for pairing** |

Valve, verbatim, for the Bluetooth row: *"Blue, solid: connection is working
normally. Blue, blinking slowly: lost connection. Blue, breathing: controller is
charging. **Blue, double-pulse: the controller is in Bluetooth pairing mode.**"*

One more chord worth knowing before you go hunting for a dead controller —
**backpack mode**, `R4 + R5 + L4 + L5 + Steam` while on, disables power-on
entirely. The same chord, or USB-C, exits.

### 1.5 The 2015 chords are wrong, and the internet is full of them

The 2015 controller's BLE mode was `Y + Steam` to launch and `B + Steam` to pair,
and it needed a **firmware flash** first
(<https://help.steampowered.com/en/faqs/view/1796-5FC3-88B3-C85F>). None of it
carries over; the 2026 controller has Bluetooth natively, at launch, no flash.

**This topic is unusually contaminated.** Search results are dominated by
AI-generated pages recycling the 2015 chords —
`electronics.alibaba.com/buyingguides/steam-controller-*`,
`fixoryhq.com`, `specclear.com`, `dropreference.com`, `gofirmware.com`.
Recurring fabrications: *"Hold Steam + Y for 5 seconds, then Steam + B to
finalize"*; *"up to three paired Bluetooth devices with context-aware switching"*
(Valve documents one); *"CVE-2026-33721 mandatory security patch"*; *"requires
BlueZ ≥ 5.70, kernel ≥ 6.8"*. A Steam forum poster noticed the same thing:
*"Multiple AI search overviews have hallucinated that this is true, seemingly
based on information from the 2015 Steam Controller."*
(<https://steamcommunity.com/discussions/forum/11/576047170584609677/>)

Corroboration for the real chord, `PRESS`: PC Gamer's review lists *"Power on
into Bluetooth pairing slot: B + R1/L1 + Steam button"*
(<https://www.pcgamer.com/hardware/game-pads/steam-controller-2026-review/>) —
the "/L1" is PC Gamer's addition; Valve documents R1 only.

### 1.6 Clearing the bond — COMMUNITY, and a real risk

There is **no documented chord to clear the controller's single Bluetooth slot**.
A thread on Valve's own forum reports that removing the bond on the host at the
wrong moment can leave the controller undiscoverable
(<https://steamcommunity.com/app/4165870/discussions/0/832746831844852850/>).
Unverified — but note that §0's reconnect bug (#13383) prescribes exactly that
delete-and-re-pair cycle as a workaround, so the two interact badly. Treat
`bluetoothctl remove` as a deliberate act, not a troubleshooting reflex.

---

## 2. What Linux sees over Bluetooth

Almost all of this is now read from source rather than inferred. What remains
open is the *live link* — the negotiated connection interval and whether the
controller reconnects — which §7 measures.

### 2.1 The identity — VERIFIED, and it is `0x1303`

`KERNEL`, `drivers/hid/hid-ids.h:1385-1392`:

```c
#define USB_VENDOR_ID_VALVE                     0x28de
#define USB_DEVICE_ID_STEAM_CONTROLLER          0x1102  /* 2015 wired   */
#define USB_DEVICE_ID_STEAM_CONTROLLER_WIRELESS 0x1142  /* 2015 dongle  */
#define USB_DEVICE_ID_STEAM_DECK                0x1205
#define USB_DEVICE_ID_STEAM_CONTROLLER_IBEX     0x1302  /* 2026 wired   */
#define USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE 0x1303  /* 2026 BLE     */
#define USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS  0x1304  /* the Puck     */
#define USB_DEVICE_ID_STEAM_CONTROLLER_NEREID   0x1305  /* Steam Machine receiver */
```

and the one Bluetooth entry in the driver's whole table,
`drivers/hid/hid-steam.c:2756-2760`:

```c
	{ /* Steam Controller (2026) BLE */
	  HID_BLUETOOTH_DEVICE(USB_VENDOR_ID_VALVE,
		USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE),
	  .driver_data = STEAM_QUIRK_IBEX | STEAM_QUIRK_BLE
	},
```

`SDL` corroborates independently — `src/joystick/controller_list.h:670-673`:

```
{ MAKE_CONTROLLER_ID( 0x28de, 0x1302 ), … },  // Valve Steam Triton Controller
{ MAKE_CONTROLLER_ID( 0x28de, 0x1303 ), … },  // Valve Steam Triton Controller (BLE)
{ MAKE_CONTROLLER_ID( 0x28de, 0x1304 ), … },  // Valve Steam Proteus Dongle
{ MAKE_CONTROLLER_ID( 0x28de, 0x1305 ), … },  // Valve Steam Nereid Dongle
```

So, concretely, on the wire and in `/sys`:

| Where | Puck today (`LOCAL`) | Controller over Bluetooth |
|---|---|---|
| `device/uevent` | `HID_ID=0003:000028DE:00001304` | `HID_ID=0005:000028DE:00001303` |
| hid device kernel name | `0003:28DE:1304.0002` | `0005:28DE:1303.NNNN` |
| `MODALIAS` | `hid:b0003g0001v000028DEp00001304` | `hid:b0005g0001v000028DEp00001303` |
| `HID_UNIQ` | `FXB99614031B4` (the dongle serial) | **the controller's bdaddr**, lowercase |
| hidraw nodes | **5** | **1** |

The formats are `hid-core.c:2983` `"HID_ID=%04X:%08X:%08X"` and `:2996`
`"MODALIAS=hid:b%04Xg%04Xv%08Xp%08X"`; `BUS_BLUETOOTH = 0x05`,
`include/uapi/linux/input.h` (`LOCAL`, `/usr/include/linux/input.h:256`).

**Note the `HID_UNIQ` change.** Over USB it is the `FX…` serial; over Bluetooth
BlueZ sets it to the device's bdaddr (`bluez src/shared/uhid.c`, `%2.2x` colon
form). It does not affect hyprpad — the relay pins its own `uniq`
(`profile.rs:539-543`) precisely so Steam's per-controller config key stays
stable — but it does mean the node cannot be identified by serial over BT.

My earlier extrapolation from SDL's controller database (`LOCAL`,
`strings /usr/lib/libSDL3.so.0.4.14`: Valve bus-`0005` entries `1105`, `1106`,
`1202` against bus-`0003` `1102`, `1142`, `1205`) predicted "a distinct `0x13xx`
id". That was right, and is now superseded by the exact number.

### 2.2 The input report — `0x45`, and it is `0x42` minus the tail

This is the finding that shrinks the port from a week to a day. `KERNEL`,
`hid-steam.c:2325-2355`, the driver's own layout table:

```
 * The size for this message payload is 53 in REPORT_ID_INPUT and 45 in REPORT_ID_INPUT2.
 *  (* values only in REPORT_ID_INPUT)
 *  Offset| Type  | Mapped to |Meaning
 *  1     | u8    | --        | sequence number
 *  2-5   | u32   | see below | buttons
 *  6-7   | s16   | ABS_HAT2Y | left trigger
 *  8-9   | s16   | ABS_HAT2X | right trigger
 *  10-17 |       |           | sticks
 *  18-29 |       |           | pads (x, y, pressure) × 2
 *  30-33 | u32   | timestamp | IMU timestamp
 *  34-45 |       |           | accel × 3, gyro × 3
 *  46-47 | s16   | --        | * quaternion W value
 *  48-49 | s16   | --        | * quaternion X value
 *  50-51 | s16   | --        | * quaternion Y value
 *  52-53 | s16   | --        | * quaternion Z value
```

`REPORT_ID_INPUT = 0x42` and `REPORT_ID_INPUT2 = 0x45` (`:323`, `:325`), gated on
`size != 54` and `size != 46` respectively (`:2553`, `:2572`) — and **both call
the same `steam_do_ibex_input_event()` and `steam_do_ibex_sensors_event()` with
the same fixed offsets**. `SDL` says it in a comment: *"Triton newer state MTUs
are identical until touchpads"*, and its `TritonMTUFull_t` is
`TritonMTUNoQuat_t` plus the quaternion
(`src/joystick/hidapi/steam/controller_structs.h`); its dispatch handles
`ID_TRITON_CONTROLLER_STATE` and `ID_TRITON_CONTROLLER_STATE_BLE` in one arm
(`SDL_hidapi_steam_triton.c:526-527`).

**And `src/report.rs::decode` reads no byte past 29** — its deepest access is
`u16le(28)` for the right pad's force (`report.rs:215`). Sticks, pads, triggers,
buttons: all inside the first 30 bytes, all present in the 46-byte report. So
hyprpad loses **nothing at all** by moving to `0x45`. Only the relay, which
forwards raw bytes to a fake device whose descriptor declares `0x42` at 54 bytes,
has to re-frame — and that is a memcpy and eight zero bytes.

**One forward hazard, `SDL` vs `KERNEL`.** SDL also handles
`ID_TRITON_CONTROLLER_STATE_TIMESTAMP = 0x47` — added 2026-05, described in
`controller_structs.h` as *"New Ibex packet that adds a timestamp to the trackpad
sampling and reduces the size of the IMU timestamp"* — and SDL's iOS BLE backend
discovers a **dedicated GATT characteristic for `0x47` on `0x1303`**
(`src/hidapi/ios/hid.m`, `VALVE_INPUT_CHAR_0x1303_0x47`). **The kernel driver
does not handle `0x47` at all.** If a firmware revision makes the Bluetooth link
emit `0x47` instead of `0x45`, both `hid-steam` and hyprpad drop every frame.
Whether that can happen on Linux is `UNVERIFIED`; §7 step 3 sees the real id.

### 2.3 Feature reports, haptics, lizard mode — unchanged, and unchunked

The thing I most expected to bite does not. `KERNEL`: `STEAM_QUIRK_BLE` occurs
**three times in 2784 lines** — the `#define` (`:63`), the device-table entry
(`:2759`), and one sensor-lifecycle check (`:1258`) — plus a probe short-circuit
(`:1620`). It touches the report layer **not at all**.

* **Feature reports**: report id `REPORT_ID_FEATURES_CONTROLLER = 1` for every
  Ibex variant (`:580-585`), 64-byte buffers, `ID_SET_SETTINGS_VALUES = 0x87`.
  Identical to what `src/lizard.rs` already sends.
* **Haptics**: `REPORT_ID_HAPTIC_PULSE = 0x81` via `hid_hw_output_report(…, 8)`
  (`:767-773`); `0x80` rumble likewise. Identical to `src/haptics.rs`.
* **No chunking.** The 2015 controller's BLE framing — report id `0x03`,
  18-byte segments with `0x80`/`0x40` header flags, reassembled by SDL's
  `SteamControllerPacketAssembler` (`SDL_hidapi_steam.c:232-288`) — **does not
  apply to this controller**. SDL's Triton path reads with a bare
  `SDL_hid_read(dev, data, 64)` and has no assembler at all. On the proprietary
  GATT service SDL's iOS backend uses, Triton writes one report to one
  characteristic per report id, no segmentation (`hid.m:599-616`).

So `src/lizard.rs` and `src/haptics.rs` need **no framing work**. What they need
is rate discipline — §2.5.

### 2.4 Which driver binds it, and battery

`LOCAL`, kernel 7.1.9-arch1-2: the installed `hid-steam` device table is three
entries, all USB, all 2015/Deck era (`modinfo -F alias`): `p00001205`,
`p00001142`, `p00001102`. **No `1302`/`1303`/`1304`, no `b0005`.** So today the
puck runs on `hid-generic`, and a Bluetooth controller would too. That is fine
for hyprpad — hidraw carries raw reports regardless of which driver is bound.

2026 support landed for **Linux 7.3**. After that upgrade `hid-steam` will bind
`0005:28DE:1303`, which changes two things worth knowing in advance:

* the driver creates a **hidraw shim** reporting `0005:28DE:1303`
  (`hid-steam.c:1716-1719`, `HID_CONNECT_HIDRAW`), so hyprpad's hidraw access
  survives — this is the same arrangement Steam relies on;
* the driver will drive lizard mode and haptics itself, which is the existing
  R5-class hazard from `docs/research/uhid-steam-controller.md:703-720`, now
  applying to the Bluetooth path too.

**Battery works over Bluetooth** — `hid-steam.c:1410-1411` registers a
`power_supply` for `STEAM_QUIRK_WIRELESS | STEAM_QUIRK_IBEX`, which includes the
BLE entry, fed from input report `0x43` (15 bytes) with charge state, capacity,
voltage, current and temperature. Only on 7.3+, though; and BlueZ separately
exposes `org.bluez.Battery1` from the GATT Battery Service.

### 2.5 The transport — where the real costs are

Everything above says the protocol is easy. This says the link is not.

**Report rate.** The BLE connection-interval floor is 7.5 ms (Core spec: a
multiple of 1.25 ms, minimum 6), enforced by Linux in `hci_check_conn_params()`,
so **133 Hz** is the ceiling at one notification per connection event. But the
*default* is far worse — `KERNEL`, `net/bluetooth/hci_core.c:2453-2456`:

```c
	hdev->le_conn_min_interval = 0x0018;  /* 24 × 1.25 ms = 30 ms */
	hdev->le_conn_max_interval = 0x0028;  /* 40 × 1.25 ms = 50 ms */
```

**30–50 ms is 20–33 Hz.** `BLUEZ` ships `MinConnectionInterval` /
`MaxConnectionInterval` commented out (`LOCAL`, `/etc/bluetooth/main.conf:237-238`),
so the kernel defaults stand. Two things can rescue it: the controller sending a
Connection Parameter Update Request, which Linux honours
(`net/bluetooth/l2cap_core.c:4768-4806`), or setting those two keys in
`main.conf` by hand. **Which happens on this controller is `UNVERIFIED` and is
the single most important number §7 measures.**

**Feature-report latency.** `BLUEZ` serves HOGP in userspace: a `HIDIOCGFEATURE`
becomes `gatt_read_char()` and a `HIDIOCSFEATURE` becomes `gatt_write_char()`
(`profiles/input/hog-lib.c`), i.e. one ATT request/response pair — **at least one
connection interval, typically two**. At the 30–50 ms defaults that is
**30–100 ms per feature report**, against ~1 ms over USB. `src/lizard.rs` sends
two 64-byte frames every 30 s, which is nothing. `hyprpad puck-settings` doing
several query round trips will simply feel slow. Neither is a problem; a chatty
future caller would be.

*(The 64-byte MTU worry turns out to be a non-issue: BlueZ's `[GATT] ExchangeMTU`
defaults to 517, and `gatt_write_char()` falls back to a prepare/execute long
write when the value does not fit.)*

**Output reports have no backpressure.** `BLUEZ`, `hog-lib.c` `forward_report()`
prefers an acked Write Request and falls back to `gatt_write_cmd()` — fire and
forget, NULL callbacks, no queue. A hidraw `write()` returns immediately
regardless. **hyprpad's haptics are the exposed caller**: `src/haptics.rs` fires
an `0x81` pulse per OSK key crossing, at whatever rate the user's thumb moves,
with no rate limit. Over USB that is free. Over BLE at 20–33 Hz it will silently
drop pulses or stall the link. `SDL` shows the shape of the fix — it re-sends
rumble every 40 ms *because* the hardware safety timeout is ~50 ms
(`SDL_hidapi_steam_triton.c:40-44`) — i.e. one output report per connection
interval is the budget, and hyprpad should coalesce to it.

**Permissions need no new grant, only a new denial.** Valve's
`60-steam-input.rules:11` reads `KERNELS=="000[356]:28DE:*"` — `0005` is in that
class, so a Bluetooth Steam Controller gets `uaccess` automatically. Line 14
(`SUBSYSTEM=="input", ATTRS{id/vendor}=="28de"`) additionally exposes its evdev
nodes, which the rule's own comment calls *"Valve HID devices over bluetooth
evdev"*. hyprpad can therefore open it with no rule of its own — and so can
Steam, which is the problem §3.1 fixes.

**Reconnect.** Not what I assumed. BlueZ's `ReconnectUUIDs` default is
**audio-only** (`LOCAL`, `main.conf:382`: HSP AG, HFP AG, A2DP source/sink);
neither HID UUID is in it, so `ReconnectAttempts`/`ReconnectIntervals` never
apply to a controller. BLE reconnect is instead the kernel's background-scan
allowlist, programmed by `bluetoothd` via `MGMT_OP_ADD_DEVICE` with
`action = 0x02` (`bluez src/adapter.c` `adapter_auto_connect_add()`) — **and it
is explicitly BLE-only**, that function bails for `BDADDR_BREDR`. Being
**trusted** is what keeps a device on the list after an explicit disconnect.

**The hidraw node number is not stable across a reconnect.** `BLUEZ`
`src/shared/uhid.c:546-563` force-destroys the uhid device on disconnect for
everything that is not a keyboard — a gamepad is `BT_UHID_GAMING` — so the HID
device and its hidraw node **go away and come back**, with a new instance suffix
(`hid_add_device()`'s id counter only increases) and possibly a different
`hidrawN` minor. hyprpad already copes: every `PuckSource::acquire()`
re-enumerates `/sys/class/hidraw` and the broker caches nothing.

---

## 3. What breaks in hyprpad

Everything funnels through **one function** — `hidraw::puck_nodes()` — and **one
decoder** — `report::Frame::decode`. Both are small changes now that §2 has
settled the identity and the format.

| # | Thing | State over Bluetooth | What it needs |
|---|---|---|---|
| 1 | **Discovery** — `src/hidraw.rs:62-78` | **Broken.** Substring-matches `28DE` *and* `1304` in `device/uevent`. Over BT the uevent reads `HID_ID=0005:000028DE:00001303` — vendor matches, product does not. | Parse `HID_ID` into `(bus, vid, pid)`; allow-list `0003:28DE:1304` and `0005:28DE:1303`. |
| 2 | **The self-match trap** — same function | **A hazard to avoid while fixing 1.** `LOCAL`: hyprpad's own fake is `hidraw12`, `HID_ID=0003:000028DE:00001302`. Widening to "any Valve HID" makes the daemon read its own output. | Keep it a strict allow-list of `(bus, pid)` pairs; if ever loosened, exclude `HID_PHYS=hyprpad-uhid/*`. |
| 3 | **The udev hide rule** — `packaging/udev/72-hyprpad-puck.rules` | **Broken, and this is the dangerous one.** `ATTRS{idVendor}`/`ATTRS{idProduct}` walk to a USB parent that does not exist. Valve's line 11 still grants `uaccess`, so **Steam sees the real controller and the fake** — R10, every press doubled. | The second clause in §3.1, now pinnable to the exact id. |
| 4 | **The broker** — `src/broker.rs:710` | **Fixed for free** by (1). The unit needs nothing: `DevicePolicy=closed` + `DeviceAllow=char-hidraw rw` covers a BT hidraw node (char major 243, `LOCAL`). | Reword `:100` and `:714`, which say `28de:1304`. |
| 5 | **Input decode** — `src/report.rs:195` | **Broken, but trivially.** Requires `len == 54 && raw[0] == 0x42`; BT sends `0x45`/46. **`decode` reads no byte past 29**, all of which `0x45` carries (§2.2). | Accept `(0x42, 54)` or `(0x45, 46)`. Nothing else in the function changes. |
| 6 | **Relay pass-through** — `src/uhid/translate.rs:93` | **Broken.** `puck_to_triton` is a memcpy that requires 54 bytes of `0x42`, because the fake's descriptor declares exactly that. | Re-frame: `out[0] = 0x42`, copy `raw[1..46]`, zero `46..54`. Lossless for everything but the quaternion, which Steam gets as zeros. |
| 7 | **`hyprpad monitor` misleads** — `src/main.rs:154` | Counts only frames `decode` accepted, so a `0x45` stream prints **nothing** and reads as "no input". | Nothing to fix once (5) lands — but the test plan must hexdump *first*. |
| 8 | **Lizard / settings** — `src/lizard.rs` | **Works unchanged.** Report id 1, 64 bytes, `0x87`; no BLE chunking (§2.3). Each write costs 30–100 ms round trip; the 30 s re-send is unaffected. | Nothing. |
| 9 | **Haptics** — `src/haptics.rs` | **Code works; the rate does not.** `0x80`/`0x81` are unchanged, but BLE output reports have no backpressure (§2.5) and hyprpad fires a pulse per OSK key crossing with no limit. | Coalesce to roughly one output report per connection interval. SDL's 40 ms rumble cadence is the reference. |
| 10 | **The relay's 250 Hz** — `src/uhid/relay.rs:56` | **Already correct.** The streamer has its own clock and repeats the held frame every 4 ms — upsampling for free, so Steam never sees a slow device. | Nothing. |
| 11 | **`STALE_AFTER`** — `relay.rs:70`, 200 ms | **Now marginal.** It is 50× the puck's 4 ms period, but only **4–6×** a 30–50 ms BLE period — and GN measured σ 20.6 ms of jitter. A stall could false-trigger neutral mid-input. | Raise it on the Bluetooth transport, or derive it from the observed inter-frame interval. |
| 12 | **Latency the relay cannot hide** | Frames arrive **stale, not sparse**. Upsampling fixes rate, not age or jitter. | Nothing. This is §4's cost. |
| 13 | **Reconnect** | **Shape already right** — node vanishes, `acquire()` returns `None`, the 1.5 s wait re-scans, the uhid device stays created. BlueZ destroys and recreates the node with a new number (§2.5), which re-enumeration handles. | Nothing — *if* the controller reconnects at all (§0 item 3, §6.4). |
| 14 | **`status.json`** — `src/status.rs` | Has `"source"`, no notion of transport. | `"transport": "usb"\|"bt"`. With two transports and a controller that can be on either, this is how you tell "off" from "on the other machine". |
| 15 | **`setup --check`** — `src/setup.rs:746-780` | Correctly reports "still reachable" until (3) lands. | Teach it the second clause. |

### 3.1 The udev rule

A hidraw node carries **no `HID_ID` in its own udev properties** — `LOCAL`,
`udevadm info -q property -n /dev/hidraw1` lists `ID_VENDOR_ID`/`ID_MODEL_ID`
from the USB parent and no `HID_*` at all. So `ENV{HID_ID}` **cannot** be used in
a hidraw rule. What udev exposes is the parent hid device's kernel name —
`LOCAL`, `udevadm info -a -n /dev/hidraw1`:

```
  looking at parent device '…/3-2.1:1.2/0003:28DE:1304.0002':
    KERNELS=="0003:28DE:1304.0002"
    SUBSYSTEMS=="hid"
```

That name is `dev_set_name(&hdev->dev, "%04X:%04X:%04X.%04X", bus, vendor,
product, id)` (`KERNEL`, `hid-core.c:3061`), and it is exactly the hook Valve's
own rule uses. `SUBSYSTEMS=="bluetooth"` is **not** an option: a BlueZ-created
HID device is parented to `/devices/virtual/misc/uhid`
(`drivers/hid/uhid.c:531`), so there is no bluetooth device in the chain and no
`ATTRS{address}` anywhere.

The rule, syntax-checked with `udevadm verify` (`LOCAL`):

```udev
ACTION=="remove", GOTO="hyprpad_puck_end"
SUBSYSTEM!="hidraw", GOTO="hyprpad_puck_end"

# USB: the dongle, matched on the USB parent (today's rule, unchanged).
ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", \
    TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0600"

# Bluetooth: the 2026 controller's own BLE identity, matched on the parent HID
# device's kernel name — the same shape Valve's 60-steam-input.rules:11 uses.
# 0005 = BUS_BLUETOOTH; 1303 = USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE.
KERNELS=="0005:28DE:1303.*", \
    TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0600"

LABEL="hyprpad_puck_end"
```

* **The id is exact, not a glob** — `0x1303` is pinned in mainline, so there is
  no reason to leave `13??` open. It deliberately does not match `1302` (wired,
  which is also the relay's fake identity) or `0005:28DE:11??` (the 2015
  controller).
* **Still numbered 72.** The uaccess-ordering finding is transport-independent:
  `60-steam-input.rules` adds the tag, `73-seat-late.rules` consumes it and
  queues the ACL builtin, and a `TAG-=` after that does nothing.
  `docs/design/uhid-relay.md:800-840` has the argument; `75`/`95`/`99` are all
  too late.
* **The fake stays safe.** hyprpad's virtual device is `0003:28DE:1302.000D`
  (`LOCAL`) — neither clause matches it, so it keeps the `uaccess` Valve's line
  11 grants. Proof that `KERNELS=` reaches uhid-created nodes with no USB parent:
  `/dev/hidraw12` currently carries `user:ajg:rw-` from that line alone.
* **Consider the evdev nodes too.** Valve's line 14 exposes a Bluetooth Valve
  device's `input`/`event` nodes. Today's rule deliberately leaves evdev alone
  (`uhid-relay.md:787-792`) because the puck produces no evdev node worth hiding.
  Over Bluetooth the lizard `0x40`/`0x41` collections may well produce a mouse
  and keyboard node — §7 step 2 looks, and if they appear, hiding them becomes a
  separate decision rather than an assumption.

---

## 4. What you are accepting

Bluetooth is a first-class mode on this controller, but it is the one Valve
publishes least about, and the one independent measurement says the link is
*jittery* rather than merely slower.

`VALVE`, store page (<https://store.steampowered.com/app/4165870>): *"One puck,
two jobs… **Prefer using Bluetooth or USB? Steam Controller supports those
too.**"* The asymmetry in the spec sheet is the finding:

| | Puck | Bluetooth |
|---|---|---|
| Valve's description | "Proprietary 2.4Ghz wireless connection" | "Standard 2.4GHz wireless." |
| Valve's spec figure | **"~8ms full end-to-end, 4ms polling rate (measured at 5m)"** | **none published** — only a host requirement, *"Bluetooth 4.2 minimum, 5.0 or higher recommended"* |
| Valve's recommendation | *"For the fastest and most reliable connection, we generally recommend using this mode."* | — |

**Valve documents no feature gating over Bluetooth** — no statement that gyro,
haptics or the trackpads are dongle-only. That is an absence of a documented
limitation, and now it is corroborated by source: `STEAM_QUIRK_BLE` gates nothing
input-related in the kernel, and SDL parses `0x42` and `0x45` in the same arm
(§2.2, §2.3). **Feature parity over Bluetooth is real.**

`PRESS`, GamersNexus (2026-05-01), 499 clicks per mode, click-to-photon:
<https://gamersnexus.net/handheld-pcs-peripherals/valve-steam-controller-review-latency-benchmarks-battery-life>

| Mode | Mean | σ |
|---|---|---|
| Wired | 19.0 ms | 3.1 |
| Puck (2.4 GHz) | 21.6 ms | 3.1 |
| **Bluetooth, alone** | **37.3 ms** | **20.6** |
| Bluetooth, with 7 other controllers present | 73.8 ms | 48 (101 dropped inputs) |

GN: *"Bluetooth doesn't scale well with multiple devices, and it's not the best
option to start with."* The puck was unaffected by the same contention (21.8 ms).

The 16 ms of extra mean is fine for driving a window manager. **The σ going
3.1 → 20.6 is what will be felt**, and it lands on the two things hyprpad is made
of: gesture thresholds on a guide hold (`src/gesture.rs`) and trackpad cursor
motion (`docs/research/pointer-damping.md`). Three mitigations worth knowing
before concluding it is unusable:

* **The 7-controller row is not your case.** That is GN deliberately saturating
  the band with eight pads. One controller on a laptop is the 37.3 ms row.
* **`pointer-damping.md` already exists** because the *puck's* pointer needed
  smoothing. The same knob absorbs BLE jitter; it may just want a different
  constant on this transport — one more reason the `"transport"` field in phase
  1d earns its place, since it lets the config branch on the link.
* **The connection interval may be tunable.** If §7 finds Linux's 30–50 ms
  default in force, `MinConnectionInterval`/`MaxConnectionInterval` in
  `/etc/bluetooth/main.conf` can pull it toward the 7.5 ms floor (§2.5). That is
  a bigger lever than anything in hyprpad.

One more, `VALVE`: *"Steam must be running on your PC for the Steam Controller to
function properly."* `PRESS` corroborates the shape — PC Gamer: *"I tested it on
GeForce Now via my iPhone and while it did connect over Bluetooth, it was not
recognised in-game."* This does **not** apply to hyprpad, which reads the vendor
report over hidraw and has never needed Steam (`docs/03-hardware-findings.md`).
On the laptop, hyprpad is precisely what makes a Bluetooth bond useful without
Steam running.

---

## 5. The shortest path to a working Bluetooth setup

Three phases. Phase 1 is the whole of "hyprpad drives the controller over
Bluetooth", and — unlike when this note was started — **all of it can be written
today**, because §2 settled the product id and the report format.

### Phase 1 — identity and framing (a day)

**1a. Discovery — `src/hidraw.rs:62-78`.** Today:

```rust
let upper = text.to_uppercase();
if upper.contains(VID_VALVE) && upper.contains(PID_PUCK) {
```

A substring search over the whole uevent text: bus-agnostic, and looking for
`1304`. Replace with a parse of the `HID_ID=` line and an allow-list:

| bus | vid | pid | what |
|---|---|---|---|
| `0003` | `28DE` | `1304` | the puck |
| `0005` | `28DE` | `1303` | the controller over Bluetooth |

**Do not relax to "any Valve HID."** `LOCAL`: the relay's own fake is
`0003:000028DE:00001302`; a vendor-only match makes the daemon read its own
output. Everything downstream — `puck_readable`, `acquire`, the broker, haptics,
`monitor`, `puck-settings` — inherits this one function.

**1b. Decode — `src/report.rs:195`.** Two lines:

```rust
let ok = (raw.len() == 54 && raw[0] == 0x42) || (raw.len() == 46 && raw[0] == 0x45);
if !ok { return None; }
```

Nothing else in `decode` changes: its deepest read is byte 29 and `0x45` carries
bytes 1–45 with identical offsets (§2.2). Worth a test pinning that a 46-byte
`0x45` and a 54-byte `0x42` with the same first 46 bytes decode to equal frames.

**1c. Relay re-frame — `src/uhid/translate.rs:93`.** The fake's descriptor
declares `0x42` at 54 bytes, so a `0x45` cannot be forwarded verbatim. Widen
`puck_to_triton` to accept both and normalise: `out[0] = 0x42`, copy `raw[1..46]`,
leave `46..54` zero. Steam gets a zero quaternion, which it does not use for
anything hyprpad exposes — and note a 2026-05 firmware reportedly made the
controller emit the no-quaternion report on *every* transport anyway.

**1d. The udev rule — §3.1.** **Not optional and not cosmetic**: without it Steam
sees two controllers and doubles every press. Install it with 1a, not after.

**1e. Wording and status.** `src/broker.rs:100,714` say `28de:1304`.
`src/status.rs` gains `"transport": "usb"|"bt"`. `src/setup.rs` `--check` learns
the second clause.

That is phase 1. After it, with the controller on Bluetooth, hyprpad finds it,
decodes it, hides it from Steam, and relays it.

### Phase 2 — rate discipline (half a day, and it is not optional)

Not framing — §2.3 showed there is none to do. **Throughput.**

**2a. Haptics must be coalesced.** `src/haptics.rs` fires an `0x81` pulse per OSK
key crossing at whatever rate a thumb moves. BLE output reports are fire-and-forget
with no queue and no backpressure (`BLUEZ`, `hog-lib.c` `forward_report`), and the
link may only carry one report per 30–50 ms. Add a minimum inter-pulse interval
on the Bluetooth transport — SDL's own cadence (a rumble refresh every 40 ms
against a ~50 ms hardware safety timeout, `SDL_hidapi_steam_triton.c:40-44`) is
the reference for what the device expects.

**2b. `STALE_AFTER` needs raising.** `relay.rs:70` is 200 ms, chosen as 50× the
puck's 4 ms period. Against a 30–50 ms BLE period with σ 20.6 ms of jitter it is
only 4–6 frames of headroom, and a false trigger drops the relay to neutral
mid-input — i.e. a dropped button. Either raise it on the Bluetooth transport or
derive it from the observed inter-frame interval.

**2c. Feature-report callers stay as they are.** `src/lizard.rs` sends two frames
every 30 s; at 30–100 ms per round trip that is invisible. `hyprpad puck-settings`
will just feel slow. No change needed, but do not add a chatty caller.

### Phase 3 — nothing

The relay already streams on its own 4 ms clock and repeats the held frame, so a
slow input rate needs no resampling (`docs/design/uhid-relay.md:577-594`). What
no phase can fix: frames arrive **stale, not sparse**. Upsampling restores rate,
never age or jitter.

### What this adds up to

**A day and a half of work, and it is fully specified.** The uncertainty that
justified a phased, contingent plan when this note was started is gone: the id is
`0x1303`, the report is `0x45`/46 with identical offsets, and the feature and
haptic channels are unchanged.

**The remaining risk is not in hyprpad at all.** It is whether the controller
reconnects (§0 item 3) and what connection interval it negotiates (§2.5). Both
are transport-level, both are measured in §7, and neither is worth writing code
ahead of.

---

## 6. Two machines, one controller

The setup you want — **dongle stays on the tower, laptop uses Bluetooth** — is
what Valve's slot model is for. With one large caveat at §6.4.

### 6.1 The slot model — VALVE

> The Steam Controller has two virtual 'slots' - right and left - that can remember
> connections for two separate Pucks.
> The Steam Controller can also maintain a Bluetooth pairing independently of its
> Puck pairings.
> When powered on, the Controller will connect by default to the last paired
> connection used.

Three persistent registrations, held **at the same time**:

| Slot | Chord | LED |
|---|---|---|
| Puck, right | `A + R1 + Steam` | white |
| Puck, left | `A + L1 + Steam` | white |
| Bluetooth (one only) | `B + R1 + Steam` | blue |

So: pair to the laptop **once**, leave the tower's puck in the right slot
**permanently**, and move between machines with a chord at power-on.
**Switching transports re-pairs nothing** — both registrations persist, and a
bare Steam press returns to whichever was last used.

Note the asymmetry: two puck slots, one Bluetooth bond. A second Bluetooth host
would cost you the first; there is no `B + L1`.

### 6.2 What the dongle does while the controller is away

`UNVERIFIED`, and §7 step 4b measures it. The puck is a USB device in its own
right — 7 interfaces, 5 hidraw nodes, four controller slots plus a pogo-pin
interface (`KERNEL`, `hid-steam.c:1606-1616`). Those nodes come from the
*dongle*. But `src/hidraw.rs:99-102` records the opposite on controller **sleep**:

> after the controller sleeps and the device unbinds its hidraw nodes disappear,
> and this returns `None` until they come back and are openable again

Whether "powered on but talking to another host" looks like sleep (nodes vanish)
or like an empty slot (nodes persist, silent) decides how the tower behaves:

* **Nodes vanish** — the good case, already handled. `acquire()` returns `None`,
  the 1.5 s reconnect wait polls, `connected = false`, the widget clears, and the
  uhid device stays created so Steam never sees a disconnect.
* **Nodes persist, silent** — the tower's hyprpad **reports a controller it does
  not have**. `puck_readable()` succeeds, startup prints *"controller found
  (5 node(s), broker)"* and sets `connected = true` (`src/run.rs:283-299`). No
  frames arrive, so the relay falls to neutral and stays there — benign, but
  `Input::ReadersEnded` never fires either, because the readers block in `read()`
  on a live node rather than hitting EOF. The daemon sits "connected and
  permanently silent" and the status widget lies.

  The fix, if it turns out that way, is a frame-arrival watchdog flipping
  `connected` false after a few seconds of silence. Better: the dongle already
  sends `0x79 REPORT_ID_WIRELESS_EVENT` (2 bytes) with
  `WIRELESS_EVENT_DISCONNECT = 1 / CONNECT = 2 / PAIR = 3` (`KERNEL`,
  `hid-steam.c:346-350`) — **that is the dongle telling the host exactly whether a
  controller is attached.** hyprpad drops it today. Decoding it answers the
  question properly rather than by timeout, and it is four bytes of work.

### 6.3 Practical consequences

* **Both machines run hyprpad permanently.** They never contend: the controller
  is registered with both, connected to one.
* **The tower is untouched.** Phase 1 is additive — the allow-list keeps
  `0003:28DE:1304` as its first row and the udev rule keeps its USB clause.
* **The laptop needs the full host install** — udev rule with the Bluetooth
  clause, broker socket, `hyprpad` group. `hyprpad setup` covers it.
* **`bluetoothctl trust` is required**, not optional: BLE auto-reconnect is the
  kernel allowlist and trust is what keeps the device on it after a disconnect
  (§2.5).
* **Don't casually `bluetoothctl remove`** (§1.6).

### 6.4 The caveat that may sink it — COMMUNITY

[steam-for-linux#13383](https://github.com/ValveSoftware/steam-for-linux/issues/13383),
opened 2026-07-03, **open at the time of writing**: the 2026 Steam Controller
pairs over Bluetooth on Linux but **does not reconnect after being powered off**
— the reporter must delete the bond and re-pair each session, while a DualSense
on the same host reconnects normally.

If that reproduces, the two-machine story collapses: "move between machines with
a chord" becomes "re-pair every time you come back to the laptop", which is worse
than the dangling dongle it was meant to replace. It also interacts badly with
§1.6, since the workaround is exactly the delete-and-re-pair cycle that thread
warns about.

**So this is §7's first job, ahead of anything else.** It is also why the plan is
"measure, then write code" rather than "write phase 1 and see": phase 1 is a day
of work that a firmware or BlueZ bug can render pointless. Retest after a kernel
7.3 upgrade regardless — that is when `hid-steam` starts binding `0x1303`, and
an in-kernel driver changes the reconnect path.

---

## 7. The safe experiment

An hour, read-only except the pairing itself. Run it **before** writing code.

### Step 0 — baseline

```bash
ls -l /dev/hidraw*          # five puck nodes, 0600 root:root, no '+' ACL
hyprpad monitor             # note the Hz — should be ~250
```

### Step 1 — pair (the one state-changing step)

Power off: hold **Steam ~5 s** until the chime, LED out. Then hold
**B + R1 + Steam**; it chimes and powers on; **keep holding** to the second chime
and the blue double-pulse.

```bash
bluetoothctl scan on            # "Steam Ctrl (BT) FXA99614…"
bluetoothctl pair  <bdaddr>
bluetoothctl trust <bdaddr>     # required for auto-reconnect (§2.5)
```

### Step 2 — the reconnect question, first

**Do this before anything else, because it decides whether the rest matters.**
Power the controller off (hold Steam ~5 s), then press Steam to wake it.

```bash
bluetoothctl info <bdaddr>      # Connected: yes?
ls /sys/class/hidraw/           # did a node come back?
```

Repeat two or three times, including once after a laptop suspend/resume. If it
reconnects every time, #13383 does not affect you and Bluetooth is viable. If it
needs a re-pair, **stop** — record it on the issue and revisit after kernel 7.3.

### Step 3 — what Linux sees (read-only)

```bash
bluetoothctl info <bdaddr>
```

Note the HID UUID — `00001812` (HOGP/BLE) or `00001124` (classic) — the
`Modalias` (expect `…p1303…`), and whether Battery Service `0000180f` is present.
For comparison, this machine's bonded pads sit either side of that split
(`LOCAL`): DualSense `00001124` classic, Xbox `00001812` BLE with a battery
service.

```bash
for d in /sys/class/hidraw/hidraw*; do echo "-- $d"; cat "$d/device/uevent"; done
```

Expect **one** new node with `HID_ID=0005:000028DE:00001303`. If the product id
is *not* `1303`, that is a genuine surprise worth chasing before writing the
allow-list.

```bash
udevadm info -a -n /dev/hidrawN | head -30     # the KERNELS== value for §3.1
cat /proc/bus/input/devices                    # lizard mouse/keyboard nodes? (§3.1)
xxd /sys/class/hidraw/hidrawN/device/report_descriptor
```

Save the descriptor.

### Step 4 — the report id and the rate

```bash
hexdump -C /dev/hidrawN | head -40
```

**Before `hyprpad monitor`**, which prints nothing for a non-`0x42` stream and
would read as "no input" (`src/main.rs:154`). First byte of each report:

* `0x45` — expected. Phase 1b/1c apply exactly as written.
* `0x42` — even easier; only discovery changes.
* **`0x47`** — the one to watch for (§2.2). Neither `hid-steam` nor hyprpad
  handles it. Capture a few reports and reopen the framing question.

Then the number that matters most after reconnect:

```bash
hyprpad monitor    # after phase 1b, or count reports from the hexdump
```

Roughly 20–33 Hz means Linux's default connection interval is in force and
`MinConnectionInterval`/`MaxConnectionInterval` are worth tuning. Near 100–133 Hz
means the controller negotiated a fast interval and the transport is in good
shape.

### Step 4b — the dongle, while the controller is away

On the tower, with the puck still plugged in (§6.2):

```bash
ls /sys/class/hidraw/          # are the puck's five nodes still there?
hyprpad monitor                # if so, does anything arrive?
```

Nodes gone → nothing to do. Nodes present and silent → the watchdog / `0x79`
work in §6.2.

### Step 5 — do writes get through

```bash
hyprpad puck-settings 25 50
```

A feature write plus read, round trip. Source says this works unchanged over BLE
(§2.3); expect it to be *slow* (30–100 ms per round trip) but correct. If it
stalls or errors, lizard mode cannot be disabled and the controller will type at
you — go back to the puck.

### Step 6 — back to the puck

Power off; hold **A + R1 + Steam** until the chime, LED white. `hyprpad monitor`
should read ~250 Hz again.

### Deliberately not in this plan

Installing any udev rule; editing anything under `~/.config`; restarting
`bluetoothd`; running Steam while the controller is on Bluetooth and the hide
rule cannot hide it; and `bluetoothctl remove` (§1.6).

---

## 8. What this note could not verify

Much shorter than it was — the two source agents closed the framing questions
outright. What is left is the live link and one firmware behaviour.

| # | Open question | Why | Closed by |
|---|---|---|---|
| 1 | **Does the controller reconnect after power-off?** #13383 says no; it is one reporter and still open. **This decides whether the whole plan is viable.** | Requires pairing. | §7 step 2 |
| 2 | **What connection interval is negotiated.** Linux defaults to 30–50 ms (20–33 Hz); the floor is 7.5 ms; the controller may request better and Linux honours it. | Requires the live link. `/sys/kernel/debug/bluetooth` is root-only and was not read. | §7 step 4 |
| 3 | **Whether the BLE link ever emits `0x47`** rather than `0x45`. SDL handles it and has a dedicated GATT characteristic for it on `0x1303`; **the kernel does not handle it at all.** | No source shows which the firmware selects on Linux. | §7 step 4 |
| 4 | **Whether the controller exposes standard HOGP on Linux**, or only Valve's proprietary GATT service (`100F6C32-…`, one characteristic per report id, seen in SDL's iOS backend). The kernel's `HID_BLUETOOTH_DEVICE` entry presupposes HOGP. | The two code paths each assume their own transport; neither proves the advertisement. | §7 step 3 |
| 5 | **What the dongle presents while the controller is elsewhere** (§6.2). | Would require powering the controller off. | §7 step 4b |
| 6 | **Real-world haptic behaviour over BLE** — how badly the un-coalesced pulse rate actually degrades. | Requires the link. | Use, after phase 1 |
| 7 | **Whether a Bluetooth bond can be cleared from the controller** (§1.6). | No Valve documentation; one community report of a bricked pairing. | Valve, or an experiment nobody should volunteer for |

Two things this note deliberately did not do: pair the controller, and write to
it. Both are §7's business, and §7's only state change is the pairing.

**One process note.** The `0x45` layout, the `0x1303` id, and the
no-BLE-chunking finding are each corroborated by two independent readings
(mainline `hid-steam.c` and SDL's Triton driver, fetched separately). The kernel
source quoted here was re-fetched and checked directly rather than taken on
report: `hid-ids.h:1390`, the device-table entry at `hid-steam.c:2756-2760`, the
`size != 46` gate at `:2571`, and the layout comment at `:2325-2355` were all
read in this session.

---

## 9. Sources

**Valve**

* Steam Controller (2026) — Feature & Troubleshooting guide, FAQ v17, edited
  2026-06-29 — <https://help.steampowered.com/en/faqs/view/33E8-5EDF-24E6-4CFB>
* Reference — Steam Controller LED, edited 2026-08-06 —
  <https://help.steampowered.com/en/faqs/view/6AB9-3A71-ED45-3FB3>
* Steam Controller store page and spec sheet —
  <https://store.steampowered.com/app/4165870>
* Steam Controller (2015) BLE firmware update, for contrast —
  <https://help.steampowered.com/en/faqs/view/1796-5FC3-88B3-C85F>

**Upstream source (fetched 2026-09-03)**

* Linux mainline `drivers/hid/hid-ids.h:1385-1392` — the four 2026 product ids
* Linux mainline `drivers/hid/hid-steam.c` — device table `:2736-2773`;
  `HID_BLUETOOTH_DEVICE(0x1303)` `:2756-2760`; quirk bits `:60-63`; report ids
  `:316-334`; input layout comment `:2325-2355`; size gates `:2553`, `:2572`;
  one-BLE-interface `:1618-1621`; feature report id `:571-585`; haptic pulse
  `:767-773`; lizard `:880-906`; battery `:1409-1411`; BLE sensor open/close
  `:1249-1266`; wireless events `:346-350`; hidraw shim `:1716-1753`.
  Support added 2026-08-12 by Vicki Pfau (Valve), first shipping in **Linux 7.3**
* Linux mainline `drivers/hid/hid-core.c:2983,2996` (`HID_ID`/`MODALIAS`
  formats), `:3061` (`dev_set_name`), `drivers/hid/uhid.c:531` (uhid parent)
* SDL main `src/joystick/controller_list.h:670-673` — `0x1303` "(BLE)"
* SDL main `src/joystick/hidapi/SDL_hidapi_steam_triton.c:40-44` (rate/rumble
  cadence), `:429-449` (device match), `:524-565` (report dispatch)
* SDL main `src/joystick/hidapi/steam/controller_structs.h` — `ETritonReportIDTypes`
  (`0x42`, `0x43`, `0x45`, `0x46`, `0x47`, `0x79`), `TritonMTUFull_t` vs
  `TritonMTUNoQuat_t`
* SDL main `src/joystick/hidapi/SDL_hidapi_steam.c:150-182,232-341` — the **2015**
  controller's segmented BLE framing, for contrast; and `src/hidapi/ios/hid.m` —
  the Valve GATT service, `TRITON_BLE_PID 0x1303`, per-report-id characteristics
* BlueZ `profiles/input/hog-lib.c` (feature reports via `gatt_read_char` /
  `gatt_write_char`; `forward_report` output path), `src/shared/uhid.c`
  (`bus = BUS_BLUETOOTH`; force-destroy for non-keyboards), `src/adapter.c`
  (`adapter_auto_connect_add`, BLE-only)
* Linux mainline `net/bluetooth/hci_core.c:2453-2456` — LE connection interval
  defaults 30/50 ms; `net/bluetooth/l2cap_core.c:4768-4806` — peripheral-initiated
  parameter updates
* ValveSoftware/steam-devices `60-steam-input.rules` lines 2, 8, 11, 14 — the
  `000[356]:28DE:*` match, added 2026-06 by Vicki Pfau
* ShadowBlip/InputPlumber — **no support for the 2026 controller**; tracking
  issue [#606](https://github.com/ShadowBlip/InputPlumber/issues/606), open

**Press and community**

* GamersNexus latency benchmarks, 2026-05-01 —
  <https://gamersnexus.net/handheld-pcs-peripherals/valve-steam-controller-review-latency-benchmarks-battery-life>
* PC Gamer, Steam Controller (2026) review —
  <https://www.pcgamer.com/hardware/game-pads/steam-controller-2026-review/>
* Phoronix, *Linux 7.3 HID*, 2026-08-24 — <https://www.phoronix.com/news/Linux-7.3-HID>
* `COMMUNITY` — Bluetooth reconnect failure:
  <https://github.com/ValveSoftware/steam-for-linux/issues/13383>
* `COMMUNITY` — bond-clearing risk:
  <https://steamcommunity.com/app/4165870/discussions/0/832746831844852850/>
* `COMMUNITY` — AI-hallucinated pairing chords:
  <https://steamcommunity.com/discussions/forum/11/576047170584609677/>

**This repo**

* `src/hidraw.rs:37-38, 62-78, 99-102, 141` — product-id constants, the uevent
  substring match, the sleep comment, the acquire path
* `src/report.rs:195, 215` — the `0x42`/54 guard, and the deepest byte read (29)
* `src/uhid/translate.rs:83-105` — `puck_to_triton`, the pass-through
* `src/uhid/profile.rs:539-543, 797-820` — the pinned `uniq`, the report table
* `src/uhid/relay.rs:56, 70` — `STREAM_PERIOD` 4 ms, `STALE_AFTER` 200 ms
* `src/lizard.rs:173` — `RESEND_INTERVAL` 30 s
* `src/haptics.rs:125, 473-497` — probe timeout, node narrowing
* `src/broker.rs:100, 710-714` — the `puck` request
* `src/run.rs:167, 283-299` — the reconnect interval, the startup wait
* `src/main.rs:140-184` — `hyprpad monitor` and what it counts
* `src/setup.rs:355, 746-780` — rule name, reachability check
* `packaging/udev/72-hyprpad-puck.rules`,
  `packaging/systemd/hyprpad-broker.service`
* `docs/design/uhid-relay.md:577-594, 709-727, 773-840` — the streaming rule,
  reconnect, the udev rule and uaccess ordering
* `docs/research/uhid-steam-controller.md:479-520, 703-720` — the report table,
  the driver-binding risk

**This machine (`LOCAL`, read-only, 2026-09-03)**

* `/usr/lib/udev/rules.d/60-steam-input.rules`; `udevadm info -q property` and
  `-a` on `/dev/hidraw1` and `/dev/hidraw12`; `udevadm verify` on the candidate rule
* `/sys/class/hidraw/*/device/uevent`, `/proc/bus/input/devices`
* `modinfo hid-steam` on kernel 7.1.9-arch1-2; `/usr/include/linux/input.h:254,256`
* `bluetoothctl show` / `devices` / `info` (DualSense, Xbox Wireless)
* `strings /usr/lib/libSDL3.so.0.4.14` — Valve GUIDs in SDL 3.4.14's database
* `/etc/bluetooth/main.conf` (all defaults; `:237-238` connection interval,
  `:382-392` reconnect policy), `/etc/bluetooth/input.conf`

## Live results (2026-09-03, owner's laptop, kernel 7.1.9, BlueZ)

- Paired with the documented chord (B + R1 + Steam past the second chime); advertised as
  `Steam Ctrl (BT) FXA9961402A6C`; bonded + trusted.
- Linux sees `0005:000028DE:00001303`, ONE hidraw node (0660 root + a uaccess ACL for the
  user, so Steam grabbed it until Steam was quit), plus evdev Mouse/Keyboard nodes that
  Hyprland adopted (lizard mode is on over BT until hyprpad disables it there).
- Stream: `0x45` 46 B at ~134 Hz + `0x40` 6 B (lizard mouse) at the same cadence, one `0x43`;
  pairs arrive back-to-back, max gap 15 ms → connection interval ≈ 7.5 ms. Far better than the
  20–33 Hz assumed in §2.
- The report descriptor (372 B, `assets/triton-bt-1303-report-descriptor.bin`) is the wired
  1302 table byte-for-byte.
- The dongle's five nodes stay enumerated and emit nothing while the controller is on BT.
- **Reconnect (§7 step 2): PASSES.** Power-off at 21:19:47 (disconnected, node gone); power-on
  → reconnected at 21:20:01 with the node back, no bond deletion, no re-pair.
  steam-for-linux#13383 does not reproduce here.
