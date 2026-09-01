# Research: presenting a *real* Steam Controller to Steam from userspace via `/dev/uhid`

*Produced 2026-08-31. Question: hyprpad owns the 2026 Steam Controller puck
(`28de:1304`, Triton/IBEX) exclusively and currently gives Steam a uinput
**Xbox-360 pad** (`src/gamepad.rs`). That discards trackpads-as-trackpads, gyro,
per-game Steam Input configs, Steam-driven haptics and back grips. Can hyprpad
instead create a **virtual HID device** via `/dev/uhid` that Steam treats as a
genuine Steam Controller, relaying real input reports in and Steam's feature
reports back out to real hardware?*

**Verification basis.**
**VERIFIED(local)** = observed on this Framework 16 today — sysfs, udev rules,
`objdump`/`nm` on the installed Steam client, and **Steam's own
`~/.local/share/Steam/logs/controller.txt`**, which turns out to contain a
complete trace of Steam driving both the puck *and* a wired `28de:1302`.
**VERIFIED(kernel)** = `torvalds/linux` master, fetched today:
`drivers/hid/uhid.c`, `hidraw.c`, `hid-core.c`, `hid-generic.c`,
`include/uapi/linux/uhid.h`, `Documentation/hid/uhid.rst`.
**VERIFIED(sdl)** = `libsdl-org/SDL` `main`, sparse clone taken today.
**VERIFIED(hidapi)** = `libusb/hidapi` master `linux/hid.c`.
**VERIFIED(hhd)** / **VERIFIED(ip)** = fresh clone of `hhd-dev/hhd`;
`ShadowBlip/InputPlumber` sources and issues.
**INFERRED** = reasoned from the above. **UNKNOWN** = called out as such.

> **One thing could not be tested.** `/dev/uhid` here is `crw------- root root`
> and this session has no passwordless sudo, so **no uhid device was actually
> created**. uhid *runtime* behaviour below is VERIFIED(kernel)/VERIFIED(hhd),
> not VERIFIED(local). §9 is the experiment that closes the gap.

---

## 0. TL;DR — verdict first

**Feasible. Higher confidence than expected, because Valve ships this pattern
themselves — but there is one trap that would have sunk the obvious attempt.**

1. **Steam uses hidraw over libudev. It does not use libusb for controllers.**
   VERIFIED(local): `ubuntu12_32/steamclient.so` has `NEEDED libudev.so.1`, **25
   undefined `udev_*` symbols, and zero undefined `libusb_*` symbols**. Its log
   shows `path: /dev/hidraw7` … `/dev/hidraw11` for the puck. The
   "you'll need USB/IP" scenario is off the table.

2. **The kernel side is unambiguous.** `UHID_CREATE2` makes a real HID device;
   `hid-generic` binds it with `HID_CONNECT_DEFAULT`, so it gets a real
   `/dev/hidrawN`, named `0003:28DE:####.00NN`, with
   `HID_ID=0003:000028DE:0000####`, parented at
   `/sys/devices/virtual/misc/uhid/`. VERIFIED(kernel).

3. **Valve wrote a udev rule specifically for this case.** Shipped
   `/usr/lib/udev/rules.d/60-steam-input.rules` line 11 (VERIFIED local):
   `SUBSYSTEM=="hidraw", KERNELS=="000[356]:28DE:*", MODE="0660", TAG+="uaccess"`
   — `000[356]` = bus `0003` USB / `0005` Bluetooth / **`0006` `BUS_VIRTUAL`**.
   It came from steam-devices PR #87 by Valve's Vicki Pfau (2026-06-25) to fix
   InputPlumber's uhid devices losing permissions; her note: *"It'll also expose
   ones with BUS_VIRTUAL, in case that ever becomes relevant."* VERIFIED(ip).

4. **Valve's own SteamOS creates uhid Valve controllers for Steam to consume.**
   `steamos-manager` (© Valve/Collabora) switches InputPlumber to the
   `"deck-uhid"` target; InputPlumber's `steam_deck_uhid.rs` calls
   `UHIDDevice::create` with `vendor: 0x28de, bus: Bus::USB`. `hhd` does the same
   in Python (`28de:12ff`). VERIFIED(ip)+VERIFIED(hhd). **This is dispositive
   that Steam accepts a parentless uhid device as a Valve controller.**

5. **THE TRAP — do not clone `28de:1304`.** For the Proteus/Nereid *dongle* PIDs,
   the controller slot is derived from the USB `bInterfaceNumber`, which a uhid
   device does not have. VERIFIED(local), from Steam's own log:

   ```
   type: 28de 1304  path: /dev/hidraw7   Interface: 2   → "device opened for index 0"
   type: 28de 1304  path: /dev/hidraw8   Interface: 3   → index 1
   type: 28de 1304  path: /dev/hidraw9   Interface: 4   → index 2
   type: 28de 1304  path: /dev/hidraw10  Interface: 5   → index 3
   type: 28de 1304  path: /dev/hidraw11  Interface: 6   → index 4, "Docked slot = true"
   ```
   and VERIFIED(sdl), `SDL_hidapi_steam_triton.c`:
   ```c
   if (IsProteusDongle(product_id)) {              /* 0x1304, 0x1305 */
       if (interface_number >= 2 && interface_number <= 5) { return true; }
   } else if (SDL_IsJoystickSteamTriton(vendor_id, product_id)) {
       return true;
   }
   return false;
   ```
   and VERIFIED(ip), InputPlumber's own comment for the analogous Deck case:
   *"True PID will only work with the VCHI target as Steam looks for a specific
   bInterfaceNumber when that PID is detected."* A uhid device reports
   `interface_number == -1`.

6. **THE ESCAPE — clone `28de:1302`, the *wired* Triton.** The `else if` branch
   has no interface test, and `controller_list.h` maps `0x28de:0x1302` to
   `k_eControllerType_SteamControllerTriton`. VERIFIED(sdl).
   **Better still: this machine has already run one.** VERIFIED(local), same log:

   ```
   type: 28de 1302   path: /dev/hidraw7   serial_number: FXA9961402A6C - 0
     Manufacturer: Valve Software   Product: Steam Controller   Release: 307   Interface: 0
   Controller uses V1 HID protocol via USB
   !! Steam controller device opened for index 0.
   ```
   Single HID interface, `Interface: 0`, **no dongle/pairing work items at all** —
   just `CGetControllerInfoWorkItem`, `CSetControllerSettingWorkItem`,
   `CExitLizardModeWorkItem`, `CWriteFeatureReportWorkItem`,
   `CLoadControllerConfigWorkItem`. Steam even wrote
   `config/configset_controller_triton.vdf` for it.

7. **Hard prerequisite: `/dev/uhid` is root-only and no rule on this system opens
   it.** VERIFIED(local): `crw------- 1 root root 10, 239`; zero matches for
   `uhid` across `/usr/lib/udev/rules.d/` and `/etc/udev/rules.d/`. `uhid.c` has
   **no `capable()` check** — pure file permission, so a rule suffices
   (VERIFIED kernel). It is also a **static node** (`/run/tmpfiles.d/static-nodes.conf`
   line: `c! /dev/uhid 0600 - - - 10:239`), so the rule needs
   `OPTIONS+="static_node=uhid"`.

8. **The one genuine remaining unknown** is whether Steam accepts a `0x1302`
   device with `Interface: -1`, `Manufacturer: ""`, `Release: 0`. Everything else
   is settled. §9 answers it in about 30 minutes.

**Recommendation: build it on `28de:1302`, behind a config flag, keeping
`src/gamepad.rs` as the fallback; run §9 first. If `-1` is rejected, fall back to
Path B (`28de:12f0`, Deck protocol) which SteamOS proves works.**

---

## 1. `/dev/uhid` mechanics

### 1.1 The API

One `open()` of `/dev/uhid` = one HID device. `write()` `struct uhid_event`
records to create and feed it; `read()` events the kernel wants serviced.
`close()` implicitly destroys. VERIFIED(kernel).

| # | Event | Dir | Notes |
|---|---|---|---|
| 0 | `__UHID_LEGACY_CREATE` | → | deprecated; **also the only path with an `f_cred != current_cred()` check** — another reason to use CREATE2 |
| 1 | `UHID_DESTROY` | → | no payload |
| 2 | `UHID_START` | ← | *"always the first event"*; carries `dev_flags` — **see §1.3** |
| 3 | `UHID_STOP` | ← | *"You can usually ignore any UHID_STOP events safely"* |
| 4 | `UHID_OPEN` | ← | someone is reading the device — **this is Steam attaching** |
| 5 | `UHID_CLOSE` | ← | last reader gone |
| 6 | `UHID_OUTPUT` | ← | host wrote an output/feature report on the interrupt channel; *"may be received even though you haven't received UHID_OPEN yet"* |
| 9 | `UHID_GET_REPORT` | ← | control-channel GET_REPORT |
| 10 | `UHID_GET_REPORT_REPLY` | → | must echo `id`; `err=0` or `EIO` |
| 11 | `UHID_CREATE2` | → | creates the device |
| 12 | `UHID_INPUT2` | → | one input report |
| 13 | `UHID_SET_REPORT` | ← | control-channel SET_REPORT |
| 14 | `UHID_SET_REPORT_REPLY` | → | `id` + `err` only, never data |

Payload structs (all `__packed` except `uhid_start_req`; VERIFIED(kernel), header
cross-checked against `/usr/include/linux/uhid.h` from `linux-api-headers 7.2-1`):

```c
#define UHID_DATA_MAX 4096              /* == HID_MAX_DESCRIPTOR_SIZE */

struct uhid_create2_req {
    __u8  name[128]; __u8 phys[64]; __u8 uniq[64];
    __u16 rd_size; __u16 bus;
    __u32 vendor; __u32 product; __u32 version; __u32 country;
    __u8  rd_data[HID_MAX_DESCRIPTOR_SIZE];
} __attribute__((__packed__));
struct uhid_start_req            { __u64 dev_flags; };            /* NOT packed */
struct uhid_input2_req           { __u16 size; __u8 data[UHID_DATA_MAX]; } __packed;
struct uhid_output_req           { __u8 data[UHID_DATA_MAX]; __u16 size; __u8 rtype; } __packed;
struct uhid_get_report_req       { __u32 id; __u8 rnum; __u8 rtype; } __packed;
struct uhid_get_report_reply_req { __u32 id; __u16 err; __u16 size; __u8 data[UHID_DATA_MAX]; } __packed;
struct uhid_set_report_req       { __u32 id; __u8 rnum; __u8 rtype; __u16 size; __u8 data[UHID_DATA_MAX]; } __packed;
struct uhid_set_report_reply_req { __u32 id; __u16 err; } __packed;
```

`rtype`: `UHID_FEATURE_REPORT=0`, `UHID_OUTPUT_REPORT=1`, `UHID_INPUT_REPORT=2`.

**`sizeof(struct uhid_event) == 4380` on x86-64** (4376 on i386 — `uhid_start_req`
is the one unpacked member and its `__u64` gives the union 8-byte alignment;
field offsets are identical, only trailing padding differs). `offsetof(u) == 4`.
Useful offsets *within `uhid_event`*: `create2.rd_data` = 280,
`input2.size` = 4 / `input2.data` = 6, `output.data` = 4 / `.size` = 4100 /
`.rtype` = 4102, `get_report.{id,rnum,rtype}` = 4/8/9,
`set_report.{id,rnum,rtype,size,data}` = 4/8/9/10/12,
`*_reply.{id,err}` = 4/8.

### 1.2 Semantics that constrain the implementation

- **`write()` never blocks**; short writes are zero-extended by the kernel
  (`memset` then `min(count, sizeof(input_buf))`), so writing only the populated
  prefix (e.g. `6 + N` bytes for `UHID_INPUT2`) is correct and cheap. Minimum 4
  bytes. Unsupported type → `-EOPNOTSUPP`; bad payload → `-EINVAL`.
- **`read()` returns exactly one event per call and *truncates* to your buffer
  size** — always read into a full 4380-byte `uhid_event`. `poll()` is
  implemented; there is **no `ioctl` at all** on this fd.
- **GET/SET_REPORT time out after 5 s, hardcoded**, returning **`-EIO`** to the
  in-kernel caller (i.e. to Steam's `ioctl`). VERIFIED(kernel):
  ```c
  ret = wait_event_interruptible_timeout(uhid->report_wait,
              !uhid->report_running || !READ_ONCE(uhid->running), 5 * HZ);
  if (!ret || !READ_ONCE(uhid->running) || uhid->report_running) ret = -EIO;
  ```
  A late reply is silently dropped. **Never let this fire.**
- **One outstanding request at a time**; the kernel matches replies on *both* the
  `id` and the event type, and *"The 'id' field is never re-used"*. You echo `id`,
  never generate it.
- **The output queue is 32 events deep and drops silently on overflow**
  (`UHID_BUFSIZE 32`, `hid_warn("Output queue is full")`). A slow reader
  therefore manifests as 5-second GET_REPORT stalls in Steam. **Drain reads every
  loop iteration.**
- **`UHID_START` arrives asynchronously**, after `write()` returns —
  `hid_add_device()` is deferred to a workqueue (fix for *"HID: uhid: fix timeout
  when probe races with IO"*). Do not block waiting for it in a loop that also
  has to service reads, or a driver doing I/O in `.probe` deadlocks for 5 s.
- **No capability required**, only file permission on `/dev/uhid`. The
  `f_cred != current_cred()` → `-EACCES` check applies **only to legacy
  `UHID_CREATE`**, not `UHID_CREATE2`. VERIFIED(kernel).
- `hid->dev.parent = uhid_misc.this_device` → the sysfs path is
  `/sys/devices/virtual/misc/uhid/<BUS>:<VID>:<PID>.<NNNN>/`. That absence of a
  USB parent is the whole story of §3.

### 1.3 Report-ID framing — get this right or nothing works

**The kernel tells you, per report type, at `UHID_START`.** VERIFIED(kernel),
`Documentation/hid/uhid.rst`:

> *"If numbered reports are used for a type, all messages from the kernel already
> have the report-number as prefix. Otherwise, no prefix is added by the kernel.
> For messages sent by user-space to the kernel, you must adjust the prefixes
> according to these flags."*

```c
if (hid->report_enum[HID_FEATURE_REPORT].numbered) ev->u.start.dev_flags |= UHID_DEV_NUMBERED_FEATURE_REPORTS;
if (hid->report_enum[HID_OUTPUT_REPORT].numbered)  ev->u.start.dev_flags |= UHID_DEV_NUMBERED_OUTPUT_REPORTS;
if (hid->report_enum[HID_INPUT_REPORT].numbered)   ev->u.start.dev_flags |= UHID_DEV_NUMBERED_INPUT_REPORTS;
```

**Read `dev_flags` and branch on it — do not hardcode from your own reading of
the descriptor.** The three flags are independent; a device can be numbered for
one type and not another.

For the Triton descriptor (§4.1) every report type carries `REPORT_ID` items, so
**all three flags will be set** and the report-ID byte is present in both
directions. Cross-checking from the other end: `hidraw.c` does **not** strip the
leading byte — `hidraw_send_report()` takes `report_number = buf[0]` and passes
`__hid_hw_raw_request(dev, buf[0], buf, count, …)` with the full buffer and
unchanged `count`. VERIFIED(kernel). So:

| Steam does | hyprpad receives | forwards to the real node |
|---|---|---|
| `hid_send_feature_report(buf[64])`, `buf[0]=0x01` | `SET_REPORT` `rtype=0 rnum=0x01 size=64 data[0]=0x01` | `ioctl(HIDIOCSFEATURE(64))` — **byte-identical** |
| `hid_get_feature_report(buf[64])`, `buf[0]=0x01` | `GET_REPORT` `rtype=0 rnum=0x01` | `ioctl(HIDIOCGFEATURE(64))`, reply 64 B |
| `hid_write(buf[10])`, `buf[0]=0x80` | `OUTPUT` `rtype=1 size=10 data[0]=0x80` | `write(fd, data, 10)` — **byte-identical** |

Because the Triton feature ID is `0x01` (never `0x00`) there is no
zero-byte-stripping asymmetry. This is exactly the framing `src/lizard.rs` and
`src/haptics.rs` already speak. **INFERRED from two kernel sources; confirm in §9.**

### 1.4 Permissions on this machine

```
crw------- 1 root root 10, 239 Aug 28 19:15 /dev/uhid
/run/tmpfiles.d/static-nodes.conf:  c! /dev/uhid 0600 - - - 10:239
grep -rn uhid /usr/lib/udev/rules.d/ /etc/udev/rules.d/  →  (nothing)
uhid 24576 2      # already loaded; BlueZ uses it for HID-over-Bluetooth
```
VERIFIED(local). Required rule, mirroring what `60-steam-input.rules:5` already
does for `uinput`, and numbered below `73-seat-late.rules`:

```udev
KERNEL=="uhid", SUBSYSTEM=="misc", MODE="0660", TAG+="uaccess", OPTIONS+="static_node=uhid"
```

`static_node=` is mandatory — the node is materialised by `systemd-tmpfiles`
before udev runs, so a plain rule would never apply. (`hhd` ships the blunter
`KERNEL=="uhid", MODE="0666", TAG+="uaccess"`; VERIFIED(hhd).)

---

## 2. Does a uhid device look like a real device to udev?

**Yes where it matters. INFERRED from kernel source + corroborated by a real
InputPlumber sysfs dump in steam-devices#86.**

VERIFIED(kernel):
```c
dev_set_name(&hdev->dev, "%04X:%04X:%04X.%04X", hdev->bus, hdev->vendor, hdev->product, hdev->id);
add_uevent_var(env, "HID_ID=%04X:%08X:%08X", hdev->bus, hdev->vendor, hdev->product);
add_uevent_var(env, "MODALIAS=hid:b%04Xg%04Xv%08Xp%08X", hdev->bus, hdev->group, hdev->vendor, hdev->product);
```

| Property | Real puck (VERIFIED local, hidraw10) | uhid clone of 1302 (INFERRED) |
|---|---|---|
| HID device name | `0003:28DE:1304.0027` | `0003:28DE:1302.00NN` |
| `HID_ID` | `0003:000028DE:00001304` | `0003:000028DE:00001302` |
| `MODALIAS` | `hid:b0003g0001v000028DEp00001304` | `hid:b0003g0001v000028DEp00001302` |
| sysfs | `…/usb3/3-2/3-2.1/3-2.1:1.5/0003:28DE:1304.0027/hidraw/hidraw10` | `/sys/devices/virtual/misc/uhid/0003:28DE:1302.00NN/hidraw/hidrawN` |
| USB parent (`idVendor`, `bInterfaceNumber`, `bcdDevice`) | present | **absent** |

Real observed InputPlumber device (from steam-devices#86, VERIFIED(ip)):
```
looking at device '/devices/virtual/misc/uhid/0003:28DE:12F0.0006/hidraw/hidraw4':
looking at parent device '/devices/virtual/misc/uhid/0003:28DE:12F0.0006':
    KERNELS=="0003:28DE:12F0.0006"   SUBSYSTEMS=="hid"   DRIVERS=="hid-generic"
```

**Rule-by-rule against the shipped `60-steam-input.rules`** (VERIFIED local):

| Rule | Fires for uhid? |
|---|---|
| `SUBSYSTEMS=="usb", ATTRS{idVendor}=="28de"` (line 2) | **No** — no USB ancestor |
| `KERNEL=="hidraw*", ATTRS{idVendor}=="28de"` (line 8) | **No** — `idVendor` is a USB sysattr |
| `SUBSYSTEM=="hidraw", KERNELS=="000[356]:28DE:*"` (line 11) | **YES** — matches the parent HID device's kernel name |
| `SUBSYSTEM=="input", ATTRS{id/vendor}=="28de"` (line 14) | No, and irrelevant (a vendor-only descriptor makes no evdev node) |

Line 11 alone gives `MODE=0660` + `uaccess` → a logind ACL for the seat user →
Steam can `open()` it. **No hyprpad-authored rule is needed for the virtual
device itself.** This rule exists *because* of uhid: steam-devices issue #86,
Vicki Pfau's diagnosis — *"the input node isn't being created, which is where the
`ATTRS{id/*}` come from in the first place. Only a hidraw is created, which has
no `ATTRS{id/*}`"* — and PR #87 fixed it, breaking-then-restoring InputPlumber's
uhid Deck target system-wide (InputPlumber#616). VERIFIED(ip).

**Use `bus = BUS_USB (0x03)`, not `BUS_VIRTUAL`.** SDL's vendored hidraw backend
filters bus types and drops anything that isn't USB/Bluetooth/I2C/SPI, so a
`BUS_VIRTUAL` device is never enumerated by SDL at all — even though the udev
rule would cover it. VERIFIED(sdl). InputPlumber and hhd both declare `BUS_USB`.

Also note `parse_uevent_info()` requires `HID_ID`, `HID_NAME` **and** `HID_UNIQ`
lines to all be present or the device is dropped. `HID_UNIQ` may be empty, but
the variable must exist — set `create2.uniq` and the kernel emits it.
VERIFIED(hidapi).

---

## 3. What Steam actually requires

### 3.1 hidraw, via libudev. Not libusb. VERIFIED(local)

```
$ objdump -p ubuntu12_32/steamclient.so | grep NEEDED | grep -E 'udev|usb'
  NEEDED               libudev.so.1
$ nm -D --undefined-only ubuntu12_32/steamclient.so | grep -c libusb_
0
$ nm -D --undefined-only ubuntu12_32/steamclient.so | grep -c udev_
25
```

The 25 udev imports include `udev_enumerate_scan_devices`,
`udev_enumerate_add_match_subsystem`, `udev_monitor_new_from_netlink`,
`udev_monitor_filter_add_match_subsystem_devtype`. The client also carries
`-enable-libusb` / `-disable-libusb` / `-enable-libusb-gamecube` launch flags —
libusb is opt-in and named only for the GameCube adapter.

Steam's log confirms the access path end to end: `path: /dev/hidraw7` for Valve
devices (its own driver), versus `path: sdl://1` for a DualSense (delegated to
SDL). And when denial worked, the failure mode was
`Unable to open local device: /dev/hidraw7` — 20 occurrences, most recently
`2026-08-31 10:17:42`, i.e. today's masking test. VERIFIED(local).

**Conclusion: uhid is the right layer. USB/IP and `dummy_hcd`+`raw-gadget` are
not needed.** (They remain a ranked fallback in §8 for completeness.)

### 3.2 The interface-number trap

Steam derives the *controller slot index* from `bInterfaceNumber` for the puck.
Directly observed, VERIFIED(local):

| log `Interface:` | log result |
|---|---|
| 2 | `!! Steam controller device opened for index 0.` |
| 3 | index 1 |
| 4 | index 2 |
| 5 | index 3 |
| 6 | index 4, then `Docked slot = true` |

matching the puck's real USB layout (VERIFIED local: `3-2.1:1.2`…`3-2.1:1.6`;
interfaces 0/1 are a CDC-ACM pair). SDL applies the same 2..5 window in source
(§0.5). And hidapi sets `interface_number = -1` for a device with no
`usb_interface` parent (VERIFIED hidapi) — in fact its uhid `break` fires
*before* `cur_dev->bus_type = HID_API_BUS_USB`, so such a device also ends up
with `bus_type == HID_API_BUS_UNKNOWN` and `release_number == 0`.

The same gate applies to the Steam Deck PID; InputPlumber's code comment
(VERIFIED ip): *"True PID will only work with the VCHI target as Steam looks for
a specific bInterfaceNumber when that PID is detected"* — which is exactly why
InputPlumber's default is `ProductId::Generic = 0x12f0` rather than
`SteamDeck = 0x1205`.

**Do not clone `28de:1304`.** There is also nothing to gain: the dongle path
drags in `CGetTritonDonglePairingBondWorkItem`, `CGetTritonSlotInfoWorkItem`,
`CTritonDockedToPuckWorkItem`, `CGetTritonDongleSerialNumber` (all present in
`steamclient.so`; the first observed firing in the log) — pairing questions a
single virtual device cannot answer honestly.

### 3.3 The escape — `28de:1302`, and what Steam does with one

The `else if` branch of SDL's Triton test has no interface check, and
`controller_list.h` line 670 is
`{ MAKE_CONTROLLER_ID( 0x28de, 0x1302 ), k_eControllerType_SteamControllerTriton, NULL }`.
VERIFIED(sdl).

And **this machine has already driven a real one.** VERIFIED(local), Steam's log,
2026-06-04 (the same physical controller, plugged in by cable rather than via the
puck):

```
Local Device Found
  type: 28de 1302
  path: /dev/hidraw7
  serial_number: FXA9961402A6C - 0
  Manufacturer: Valve Software
  Product:      Steam Ctrl (USB)          ← later entries say "Steam Controller"
  Release:      307
  Interface:    0

Controller uses V1 HID protocol via USB
!! Steam controller device opened for index 0.
Steam Controller reserving XInput slot 0
… 26CGetControllerInfoWorkItem(0)      (1054 ms — a real device round-trip)
… 29CSetControllerSettingWorkItem(0)
… 23CExitLizardModeWorkItem(0)
… 27CWriteFeatureReportWorkItem(0)   ×7
… 29CLoadControllerConfigWorkItem(0) ×5
ConfigSet - failed to find config set file on-disk: …/config/configset_FXA9961402A6C.vdf
ConfigSet - found config set file on-disk: …/config/configset_controller_neptune.vdf
ConfigSet - failed to find ibex config set file on-disk, saving now: …/config/configset_controller_triton.vdf
Deck Controller PCB Serial# invalid: NA      ← queried, failed, tolerated
BYieldingRegisterSteamController
BYieldingCompleteSteamControllerRegistration
```

Everything hyprpad must satisfy is in that trace, and it is modest:

- **Zero dongle/pairing work items.** Confirms `0x1302` is the low-surface path.
- `Interface: 0` — a single HID interface. So **one uhid device, not five.**
- **Per-controller Steam Input config is keyed by the serial**:
  `configset_<HID_UNIQ>.vdf`. Whatever hyprpad puts in `create2.uniq` becomes the
  controller's persistent Steam identity. Use the *real* controller's serial
  (`FXA9961402A6C`) so existing configs carry over — hyprpad can read it from
  `HID_UNIQ` when the unit is wired, or just pin a constant. A missing serial is
  not fatal — Steam logs *"Controller has an Invalid or missing unit serial
  number, setting to `<vid>-<pid>-<hash>`"* — but then the config identity is
  unstable, so **pin it**.
- Family config set is `configset_controller_triton.vdf`; Steam calls the family
  "ibex" internally.
- `Deck Controller PCB Serial# invalid: NA` — Steam probes a string attribute,
  fails, and carries on. **Failing a GET_REPORT is survivable.**
- `Manufacturer`/`Product`/`Release` come from the USB device descriptor for a
  real device. A uhid clone will show `Manufacturer: ""` (hidapi hardcodes empty),
  `Product: <create2.name>`, `Release: 0`. **INFERRED risk, small; part of what
  §9 tests.**

### 3.4 Prior art is dispositive that uhid Valve devices are accepted

- **SteamOS itself.** `steamos-manager` (© Valve Software / Collabora)
  `inputplumber.rs`: `proxy.set_target_devices(&["deck-uhid"]).await` and
  `target.device_type().await? == "deck-uhid"`. VERIFIED(ip). Valve deliberately
  makes Steam consume a uhid-created Valve controller on third-party handhelds.
- **InputPlumber** `src/input/target/steam_deck_uhid.rs` uses the `uhid-virt`
  crate with `bus: Bus::USB, vendor: 0x28de` and
  `ProductId { SteamDeck = 0x1205, Generic = 0x12f0, MsiClaw = 0x12fa,
  LenovoLegionGo2 = 0x12fb, ZotacZone = 0x12fc, AsusRogAlly = 0x12fd,
  LenovoLegionGo = 0x12fe, LenovoLegionGoS = 0x12ff }`. VERIFIED(ip).
- **hhd** `src/hhd/controller/virtual/sd/` — `UhidDevice(vid=0x28DE, pid=0x12FF,
  bus=BUS_USB, version=256, country=0, name=b"Steam Controller (HHD)",
  unique_name=b"", physical_name=b"")`, a 39-byte vendor-only descriptor, and it
  answers `UHID_GET_REPORT` for `0x83` (attributes) / `0xAE` (string attribute)
  and consumes `UHID_SET_REPORT` `0xEB` rumble / `0xEA` touchpad / `0x8F` haptics
  / `0x87` settings. VERIFIED(hhd).
- **A Steam bug report against a `deck-uhid` device** (steam-for-linux#12064)
  complains only that the *rumble button* is missing in Steam's controller
  tester — i.e. Steam renders it as a full Valve controller. VERIFIED(ip).
- Steam's own PID table is broader than SDL's: a byte-scan of
  `ubuntu12_32/steamclient.so` finds `28de:12f0, 12fa, 12fb, 12fc, 12fd, 12fe,
  12ff` alongside `1102/1142/1205/1302/1303/1304/1305/11ff`; the bundled
  `libSDL3.so.0` has only the public set.

**No project was found doing hyprpad's exact variant — owning the real device and
*relaying* through a clone.** hhd and InputPlumber *synthesise* from other
hardware. hyprpad would be first at the relay, but every mechanism it needs is
individually proven.

---

## 4. The protocol surface

### 4.1 The real puck's report descriptor (VERIFIED local, read today)

`28de:1304`, `Valve Software / Steam Controller Puck`, serial `FXB99614031B4`,
`bcdDevice 0002`, 7 interfaces (0/1 = CDC-ACM):

| hidraw | USB iface | descriptor | role |
|---|---|---|---|
| hidraw7–10 | `:1.2`–`:1.5` | 372 B (identical) | pairing slots 0–3 |
| hidraw11 | `:1.6` | 54 B, vendor-only (`0x42`, `0x79`, features `0x01`/`0x02`) | dongle control |

The 372-byte descriptor has **three top-level collections**: a **Mouse**
(report `0x40`) and a **Keyboard** (report `0x41`) — lizard mode's — and the
vendor page `0xFF00` usage `0x01` collection carrying the Steam protocol:

| Kind | ID | payload | total | SDL name (VERIFIED sdl, `controller_structs.h`) |
|---|---|---|---|---|
| Input | `0x42` | 53 | **54** | `ID_TRITON_CONTROLLER_STATE` — what `src/report.rs` decodes |
| Input | `0x43` | 14 | 15 | `ID_TRITON_BATTERY_STATUS` |
| Input | `0x44` | 5 | 6 | *(unhandled by SDL, undecoded)* |
| Input | `0x45` | 45 | 46 | `ID_TRITON_CONTROLLER_STATE_BLE` |
| Input | `0x79` | 1 | 2 | `ID_TRITON_WIRELESS_STATUS` |
| Input | `0x7B` | 12 | 13 | *(unhandled by SDL, undecoded)* |
| Output | `0x80` | 9 | **10** | `ID_OUT_REPORT_HAPTIC_RUMBLE` (`HID_RUMBLE_OUTPUT_REPORT_BYTES == 10` ✓) |
| Output | `0x81` | 7 | **8** | `REPORT_ID_HAPTIC_PULSE` — what `src/haptics.rs` writes ✓ |
| Output | `0x82`–`0x86` | 3/9/8/3/3 | 4/10/9/4/4 | unidentified |
| Output | `0x87`,`0x88`,`0x89` | 63 | 64 | unidentified 64-byte channels |
| Feature | `0x01` | 63 | **64** | `REPORT_ID_FEATURES_CONTROLLER` (`HID_FEATURE_REPORT_BYTES == 64` ✓) |
| Feature | `0x02` | 63 | 64 | dongle feature channel (INFERRED) |

Every length independently matches SDL's constants *and* `src/lizard.rs` /
`src/haptics.rs`. Strong cross-check that the local decode is right.

SDL additionally handles `ID_TRITON_WIRELESS_STATUS_X = 0x46` and
`ID_TRITON_CONTROLLER_STATE_TIMESTAMP = 0x47`, which this puck does not declare.

**A wired `0x1302` unit's descriptor was not captured** — the log proves one has
been attached to this machine, but not today. **This is free and exact: plug the
controller in by cable and dump
`/sys/class/hidraw/hidrawN/device/report_descriptor` for the `28de:1302` node.**
Cloning *that* descriptor makes the impersonation exact rather than approximate.
Do this as step 0 of §9.

### 4.2 What SDL (and by extension Steam) sends

VERIFIED(sdl), `SDL_hidapi_steam_triton.c`:

- **Init: nothing.** `InitDevice` allocates a context; `OpenJoystick` only
  declares capabilities — gyro + accel sensors, **two touchpads**, cap-sense on
  both sticks and both grips, 1 hat, `SDL_GAMEPAD_NUM_TRITON_BUTTONS`.
- **No `GET_REPORT` at all** in the Triton path. No serial query, no attributes
  query, no firmware check. (Steam's own client does more — the log shows
  `CGetControllerInfoWorkItem` and a PCB-serial probe — but both are tolerant of
  failure.)
- **Every 3 s**, `DisableSteamTritonLizardMode()`:
  ```c
  Uint8 buffer[HID_FEATURE_REPORT_BYTES] = { 1 };   /* report id 0x01, rest zero */
  FeatureReportMsg *msg = (FeatureReportMsg *)(buffer + 1);
  msg->header.type   = ID_SET_SETTINGS_VALUES;                                   /* 0x87 */
  msg->header.length = 1 * sizeof(ControllerSetting);
  msg->payload.setSettingsValues.settings[0].settingNum   = SETTING_LIZARD_MODE; /* 9 */
  msg->payload.setSettingsValues.settings[0].settingValue = LIZARD_MODE_OFF;     /* 0 */
  SDL_hid_send_feature_report(dev, buffer, sizeof(buffer));
  ```
  **Byte-for-byte what `src/lizard.rs` already sends.**
- **Rumble**: `SDL_hid_write()` of a 10-byte `0x80` output report.
- **Reads**: plain 64-byte `hid_read`, dispatched on `data[0]`.

### 4.3 Answer-vs-relay policy

Default: **relay almost everything.** Unlike hhd, hyprpad has real hardware
behind the clone, so relaying is both simpler *and* more faithful — Steam's IMU
enable reaches the real puck and gyro starts streaming in the same `0x42` report
hyprpad is already forwarding. That is the biggest single win over the Xbox pad.

**Relay** — feature report `0x01` for the safe command set, and all output
reports `0x80`–`0x89`. Commands VERIFIED(sdl), `enum FeatureReportMessageIDs`:
`0x80` SET_DIGITAL_MAPPINGS, `0x81` CLEAR_DIGITAL_MAPPINGS, `0x82` GET_DIGITAL_MAPPINGS,
`0x83` GET_ATTRIBUTES_VALUES, `0x84` GET_ATTRIBUTE_LABEL, `0x85` SET_DEFAULT_DIGITAL_MAPPINGS,
`0x87` SET_SETTINGS_VALUES, `0x88` CLEAR_SETTINGS_VALUES, `0x89` GET_SETTINGS_VALUES,
`0x8A`–`0x8C` labels/maxes/defaults, `0x8D` SET_CONTROLLER_MODE, `0x8E` LOAD_DEFAULT_SETTINGS,
`0x8F` TRIGGER_HAPTIC_PULSE, `0xA1` GET_DEVICE_INFO, `0xAE` GET_STRING_ATTRIBUTE,
`0xBA` GET_CHIPID, `0xCE` RESET_IMU, `0xEA`/`0xEB`.

**Block** (reply success, drop the command) — destructive, or hyprpad's to own:

| ID | Command | Why |
|---|---|---|
| `0x86` | `ID_FACTORY_RESET` | irrecoverable |
| `0x9F` | `ID_TURN_OFF_CONTROLLER` | this is the `guide+y` Steam-poweroff path (project memory); hyprpad decides when the controller sleeps |
| `0xA7`,`0xB5`,`0xBF`,`0xC0`,`0xC3` | trackpad/gyro/joystick/trigger/analog calibration | persistent writes |
| `0xA9` | `ID_SET_SERIAL_NUMBER` | irrecoverable identity write |
| `0xAF`,`0xB0` | `RADIO_ERASE_RECORDS`, `RADIO_WRITE_RECORD` | destroys pairing |
| `0xAD`,`0xB1`–`0xB4` | pairing / dongle | meaningless for a claimed wired unit |
| `0xB7`–`0xB9` | `AUDIO_UPDATE_*` | firmware-ish payload; **PID-mismatch hazard** (§7 R3/R4) |

**Answer locally:** nothing is strictly required. If Steam's
`CGetControllerInfoWorkItem` turns out to read `0x83`, the relayed reply carries
`ATTRIB_PRODUCT_ID = 0x1304` while the device claims `0x1302` — patch that one
`u32`. hhd's synthesised layout shows the encoding: `[0x83][0x2D][attr_id][u32 le]…`
with `ATTRIB_PRODUCT_ID = 0x01`, `ATTRIB_PRODUCT_REVISION = 0x02`,
`ATTRIB_BOOTLOADER_BUILD_TIME = 0x0A`, `ATTRIB_FIRMWARE_BUILD_TIME = 0x04`,
`ATTRIB_BOARD_REVISION = 0x09`, `ATTRIB_CONNECTION_INTERVAL_IN_US = 0x0B`.
VERIFIED(hhd).

### 4.4 What to strip from relayed input reports

Per `src/report.rs` (VERIFIED local), on report `0x42`:

- **Steam/guide bit — byte 4, bit 0. Clear it, always,** while hyprpad owns the
  guide chord layer; otherwise every `guide+x` also opens the Steam overlay.
  One `raw[4] &= !0x01` on a copy.
- **QuickAccess — byte 2, bit 4** — clear if bound.
- Anything else the loaded config claims (the owner's config binds L4/L5/R4/R5).
  **Derive the mask from the binding table** so `hyprpad reload` updates it.
- Byte 1 is a sequence counter — **relay untouched**; renumbering risks desync
  with the IMU timestamp path, and Steam tolerates gaps.
- Bytes 30+ are the IMU, streaming only after Steam's own enable feature report,
  which the relay delivers. **Gyro works for free.**

Never emit `0x40`/`0x41` (the lizard mouse/keyboard) on the virtual device.

### 4.5 Which descriptor to publish

1. **The real wired `0x1302` descriptor**, captured per §4.1. **Best — do this.**
2. Failing that, the puck's 372-byte slot descriptor verbatim. Cost: the kernel
   creates **phantom evdev keyboard + mouse nodes** from the `0x40`/`0x41`
   collections. Silent, but they pollute the input list.
3. Failing that, the vendor-collection only (~245 B). No phantom evdev nodes;
   precedent is the puck's own interface 6 and hhd's 39-byte descriptor.

Steam's acceptance is by VID/PID, not descriptor shape (§3.3, §3.4), so (3) is a
safe simplification if (1) is unavailable.

### 4.6 Forward hazard: a future `hid-steam` that claims Triton

This kernel's `hid-steam` aliases only `28DE:1205/1142/1102` (VERIFIED local,
`modinfo`), which is why the puck binds `hid-generic` — consistent with
`src/lizard.rs`'s premise. **Upstream `hid-steam` covers `1302`–`1305`**, so when
a kernel with Triton support lands it will bind hyprpad's uhid device too: HID
matching is on bus/vendor/product and does not care that the device is virtual.
It would create its own evdev node and send its own lizard-disable feature
reports into hyprpad's uhid fd (harmless if relayed — idempotent — but noisy).

Three mitigations, two of which are free: `driver_override` the uhid device to
`hid-generic`; **or** note that many `hid-steam` paths call `hid_is_usb()`, which
compares the *ll_driver*, not `hdev->bus` — so a uhid device declaring `BUS_USB`
enters `probe` and is then **rejected** by any `hid_is_usb()` guard
(VERIFIED(kernel); whether Triton's path is guarded is **UNKNOWN**); **or** pick a
PID `hid-steam` will never claim — which is a standing advantage of Path B's
`0x12f0` (§8.1.2), a PID that exists only in Steam's userspace table.

---

## 5. Arbitration: hyprpad vs Steam over one physical controller

### 5.1 Lizard mode — no real conflict

Steam sends `0x87 / SETTING_LIZARD_MODE = 0` every 3 s (and
`CExitLizardModeWorkItem` on connect). `src/lizard.rs` sends `0x81` +
`0x87` with lizard **and** watchdog off. Both want lizard off; the reports are
idempotent. Two consequences:

- Keep hyprpad's watchdog-disable — it makes Steam's 3 s heartbeat redundant
  rather than load-bearing.
- **Gate `restore_lizard_on_exit`** (docs/12) on "no `UHID_OPEN` outstanding", or
  hyprpad's `0x85` will race Steam's next `0x87`.

### 5.2 Haptics — temporal, not structural

Both write fire-and-forget output reports to the same actuators; last write wins
for its pattern's duration. The relay **must** go through `haptics.rs`'s existing
single writer thread and fd set — never open a second writable fd on the puck.

- **While relaying (rank 3): Steam owns the actuators.** hyprpad's UI pulses are
  already suppressed there by construction (OSK and desktop layers inactive).
- **While hyprpad owns the controller (ranks 1, 2, 4): hyprpad owns them.** Steam's
  rumble writes still arrive over uhid — **drop them**, so a backgrounded game
  cannot buzz the OSK.

### 5.3 The state machine

Reuse `Arbiter` and `gamepad.rs`'s four-rank precedence verbatim; only the sink
changes.

```
 uhid device: created at daemon start, destroyed at exit — NEVER torn down on a
 focus change (a create makes Steam re-detect, re-apply configs and toast).

 RELAY  (rank 3: game focused, guide not held, OSK down)
   in : real 0x42/0x43/0x45/0x79 -> mask reserved bits -> UHID_INPUT2
   out: UHID_OUTPUT 0x80/0x81/0x8F -> haptics writer thread -> real node
   ctl: UHID_SET/GET_REPORT (whitelisted) -> real node -> reply

 HOLD   (ranks 1,2,4: desktop / OSK / guide held)
   in : nothing forwarded  (Steam sees a connected-but-idle controller)
   out: UHID_OUTPUT dropped
   ctl: SET_REPORT still relayed for SETTINGS/MAPPINGS (0x80/0x81/0x85/0x87) so
        Steam's configuration survives; GET_REPORT still relayed; every request
        answered immediately, success or error

 RELAY -> HOLD : send ONE neutral 0x42 (buttons clear, sticks/pads centred,
                 triggers 0, counter++). Same invariant gamepad.rs::neutral()
                 already enforces — no game left holding a stuck input.
 HOLD  -> RELAY: nothing special; the next real 0x42 is absolute state.
```

**Surface to the owner:** in HOLD, Steam Big Picture and the Steam overlay get no
controller input. If pad navigation in BPM is wanted, `Arbiter::game_focused()`
must also match Steam's own window classes. That is a config decision, not a
technical one.

### 5.4 Sleep / reconnect

`src/hidraw.rs::puck_readable()` already handles the controller sleeping. Keep
the uhid device **created** across a sleep: input relay simply stops, and
feature/output relays fail fast with `EPIPE` — at which point hyprpad **must
still send `UHID_*_REPLY` with a non-zero `err` immediately**. Never let the 5 s
timeout fire (§1.2). Steam handles this gracefully; its log already shows
`Controller device closed after hid_read failure` as a normal event.

---

## 6. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | Steam rejects `Interface: -1` / `Manufacturer: ""` / `Release: 0` for `0x1302`. | **The one open question** | §9. Fallback = Path B (§8.1.2). |
| R2 | `/dev/uhid` needs a shipped udev rule; the W12 device-hide probably needs **root**. | High, structural | One `99-hyprpad.rules` covering both. Decide: system service vs. minimal root helper. **Note §1.2 — a helper cannot merely pass the `/dev/uhid` fd; legacy `UHID_CREATE` is credential-checked, and while `CREATE2` is not, the cleanest split is helper-creates-and-hands-over or daemon-runs-as-root.** |
| R3 | **PID mismatch**: device claims `0x1302`, hardware answers `0x1304`. Relayed `0x83`/`0xA1` replies are inconsistent. | Medium | Patch `ATTRIB_PRODUCT_ID`; block `0xB7`–`0xB9`. |
| R4 | Steam attempts a **firmware update** on the "wired Triton". | Medium/serious | Blocklist. Note the log already shows the product string changing (`Steam Ctrl (USB)` → `Steam Controller`) across one Steam session on the real wired unit — Steam *does* touch firmware/bootloader state. |
| R5 | A future `hid-steam` binds the uhid device. | Medium, deferred | `driver_override`, or rely on `hid_is_usb()` rejecting it (§4.6). |
| R6 | **5-second stalls** from a missed report reply. | High, easy to hit | Structural rule: answer every GET/SET on the same loop iteration; drain reads every iteration (32-deep queue drops silently). |
| R7 | **Latency** — the relay adds a userspace hop. | Low | Both hops are non-blocking; the puck streams ~250 Hz. Budget ≪1 ms. **Measure in the spike.** |
| R8 | The puck's CDC-ACM pair cannot be cloned; a real wired unit may expose one too. | Low / UNKNOWN | No evidence Steam uses it for input. |
| R9 | Anti-cheat sees a HID device with no USB parent. | Low | Identical exposure to SteamOS's own `deck-uhid`, hhd, InputPlumber, and every Bluetooth controller. |
| R10 | Two controllers appear if the real puck isn't fully hidden. | Medium | W12 must be *solved*. Use hhd's `hide.py` pattern (§8.3). |
| R11 | Steam's per-controller config is keyed by `HID_UNIQ`; a changing `uniq` orphans configs. | Low | Pin `create2.uniq` to a constant (ideally the real controller serial `FXA9961402A6C`). |

---

## 7. Implementation notes

**Rust crate: hand-roll it.** `uhid-virt 0.0.8` (2025-01-01, ~24k downloads/90d,
last repo push 2026-03-09, used by InputPlumber) is the only viable crate, and it
has four sharp edges that all bite this use case:

1. It opens the fd `O_NONBLOCK` unconditionally and **the `AsFd`/`AsRawFd` impls
   exist only on master, not in published 0.0.8** — so on crates.io you cannot
   `poll`/`epoll` it and are stuck busy-polling. §1.2 makes a polled loop
   mandatory.
2. `StreamError` derives **nothing** — not `Debug`, not `Display`, not `Error`.
   `?`, `.unwrap()`, `{:?}` all fail to compile.
3. `CreateParams` copies with an index-assign loop: an oversized descriptor or
   name **panics** instead of returning `Err`.
4. It pulls in **bindgen + libclang** via `uhidrs-sys`.

The UAPI is a stable ~200-line header; two other crates (`passless-uhid`,
`soft-fido2-transport`) hand-roll it with just `libc`/`nix`. A ~150-line in-tree
`src/uhid.rs` FFI is the right call and matches how `lizard.rs`/`haptics.rs`
already talk to the kernel. (`hid-replay` on crates.io is a useful reference
implementation of the read/reply loop.)

**Module shape.**

```rust
//! A daemon-owned virtual Steam Controller (/dev/uhid) claiming 28de:1302.
pub struct VirtualSteamController { fd: File, numbered: DevFlags, state: RelayState }
pub enum RelayState { Hold, Relay }

impl VirtualSteamController {
    /// UHID_CREATE2: bus=BUS_USB(3), vendor=0x28DE, product=0x1302, country=0,
    /// version = the real unit's bcdDevice (307), name = "Steam Controller",
    /// uniq = the real controller serial (Steam's config key), phys = "hyprpad".
    pub fn create(uniq: &str, rd: &[u8]) -> io::Result<Self>;

    /// UHID_INPUT2. Caller has applied the reserved-bit mask.
    /// Prefixes the report id iff UHID_DEV_NUMBERED_INPUT_REPORTS (§1.3).
    pub fn send_input(&mut self, report: &[u8]) -> io::Result<()>;

    /// Non-blocking drain. Every GET/SET_REPORT is answered before this returns.
    pub fn poll(&mut self, sink: &mut dyn RelaySink) -> io::Result<Vec<UhidEvent>>;
}

/// Implemented by the real-device side, next to hidraw/haptics so there is
/// exactly one writer thread and one fd set on the puck.
pub trait RelaySink {
    fn set_feature(&mut self, data: &[u8]) -> io::Result<()>;               // HIDIOCSFEATURE
    fn get_feature(&mut self, id: u8, len: usize) -> io::Result<Vec<u8>>;   // HIDIOCGFEATURE
    fn output(&mut self, data: &[u8]) -> io::Result<()>;                    // write()
}
```

**Wiring into `run.rs`.** In the rank-3 branch, alongside (or instead of) the
`gamepad.rs` forward: `raw 0x42 -> mask_reserved(&cfg.bindings) -> vsc.send_input()`.
Service `vsc.poll()` every loop iteration; `UHID_OUTPUT` enqueues onto
`haptics.rs`'s existing bounded queue (dropping when full is correct);
`UHID_SET_REPORT` whitelist-checks then does a synchronous `HIDIOCSFEATURE` with
the existing `EPIPE` retry budget; `UHID_GET_REPORT` does a synchronous
`HIDIOCGFEATURE`. All three reply immediately.

**Config.** `[output] mode = "xbox" | "steam_controller" | "none"`, plus
`steam_controller.product_id` (default `0x1302`) and
`steam_controller.descriptor = "wired" | "puck" | "vendor_only"`.

---

## 8. Verdict, ranked

### 8.1 Options

1. **Path A — uhid clone of `28de:1302` + two-way relay. RECOMMENDED.**
   Kernel: certain. Permissions: certain. hidapi/SDL acceptance: certain from
   source. Steam acceptance: **highly likely** — Valve ships uhid Valve
   controllers themselves (§3.4), and `0x1302` is a first-class Steam path with
   no interface gate and no dongle interrogation (§3.3). Gives the owner
   everything: real trackpads, gyro, per-game Steam Input, Steam haptics, back
   grips. Smallest code: relay, don't synthesise.
2. **Path B — uhid clone of `28de:12f0` (Deck protocol). PROVEN FALLBACK.**
   This is exactly what SteamOS/InputPlumber/hhd do, so it is the highest-
   confidence option — but it is *emulation*, not relay: hyprpad must **transcode**
   the puck's `0x42` into the Deck's 64-byte report (the 38-byte `06 FF FF`
   vendor descriptor both projects share, header `01 00 09 40`, **no report IDs**),
   **synthesise** `0x83` / `0xAE` / `0xBA` replies (InputPlumber returns the
   string `"1NPU7PLUMB3R"` for `0xAE`), and **translate** Steam's `0xEB` rumble /
   `0x8F` haptics into the puck's IBEX `0x80`/`0x81` forms. That is real work, and
   it loses the free-gyro-passthrough property. Feature parity is still excellent
   — the Deck controller has two trackpads, gyro, four back grips and capacitive
   sticks, nearly identical to the Triton.

   Two incidental advantages: `0x12f0` exists only in Steam's *userspace* table
   (not in SDL's public list, not in any `hid-steam` alias), so **no kernel driver
   will ever claim it** (§4.6). One gotcha: SDL's Deck path calls
   `SDL_hid_read_timeout(…, 16)` in `InitDevice` and **bails if zero bytes
   arrive** — the virtual device must already be streaming at detection time.
   VERIFIED(sdl).
3. **Hybrid — Path A (or B) for Steam, plus the existing uinput Xbox pad for
   non-Steam games. RECOMMENDED as the shipping *configuration*** — selected by
   config, **never both at once by default** (Steam would see two pads and games
   would double-count).
4. **USB/IP (`vhci-hcd`) or `dummy_hcd` + `raw-gadget`.** Only if both A and B
   fail. All modules exist on this kernel (VERIFIED local: `CONFIG_USB_DUMMY_HCD=m`,
   `CONFIG_USB_RAW_GADGET=m`, `CONFIG_USBIP_VHCI_HCD=m`, `CONFIG_USBIP_VUDC=m`).
   It creates a **genuine `usb_device`**, so `ATTRS{idVendor}`, `bInterfaceNumber`
   and hidapi's interface lookup all work — it defeats *every* objection in §3.2,
   and would even let you clone `0x1304` with all five interfaces. Cost: implement
   a full USB device (descriptors, enumeration, control-endpoint handling, 7
   interfaces), needs root and two debugging-grade modules loaded at boot.
   **5–10× the uhid path.** §3.1 says you don't need it.
5. **Status quo (Xbox pad only)** — already rejected by the owner.

### 8.2 The prioritised recommendation

> Run §9. If `0x1302` with `Interface: -1` is detected, build Path A and keep the
> Xbox pad as a config-selectable fallback. If it is not, build Path B — it is
> proven, and the extra transcoding work is bounded and well-documented by hhd
> and InputPlumber, both of which can be read directly.

### 8.3 Bonus: this research also settles W12

`docs/experiments/w12-device-denial.md` left device denial "inconclusive" with a
fragile bwrap mask. **hhd's `src/hhd/controller/lib/hide.py` is Candidate C,
implemented and battle-tested** (VERIFIED hhd): write
`/run/udev/rules.d/95-hhd-devhide-<root>.rules` containing

```udev
SUBSYSTEMS=="input", ATTRS{id/vendor}=="{vid}", ATTRS{id/product}=="{pid}", GOTO="hhd_valid"
GOTO="hhd_end"
LABEL="hhd_valid"
# Keep SDL from falling back to probing the hidden device's sysfs capabilities.
KERNEL=="js[0-9]*|event[0-9]*", SUBSYSTEM=="input", ENV{ID_INPUT}="0", ENV{ID_INPUT_JOYSTICK}="0", …,
    MODE:="000", GROUP:="root", TAG-="uaccess", RUN+="/bin/chmod 000 /dev/input/%k"
LABEL="hhd_end"
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="{vid}", ATTRS{idProduct}=="{pid}", MODE:="000", GROUP:="root", TAG-="uaccess"
SUBSYSTEM=="usb", KERNEL=="hiddev[0-9]*", …, MODE:="000", GROUP:="root", TAG-="uaccess"
```

then `udevadm control --reload-rules` and `udevadm trigger --action remove` +
`--action add -b <usb parent>`. No Steam-launch wrapping, no instance-lock race,
no pressure-vessel nesting. **It requires hyprpad (or a helper) to be root** —
the same prerequisite R2 already imposes, so the two decisions collapse into one.

*(Steam's log already contains 20 `Unable to open local device: /dev/hidraw7`
lines, the most recent from `2026-08-31 10:17`, so denial has demonstrably worked
at least once on this machine. VERIFIED(local).)*

---

## 9. The decisive experiment

Everything reduces to: **does `steamclient.so` accept a parentless uhid device
claiming `28de:1302`?** ~30 minutes, no daemon changes.

**Step 0 — capture the real thing (2 minutes, no root).** Unplug the puck, plug
the controller in by USB-C, then:
```bash
for h in /sys/class/hidraw/hidraw*; do
  grep -q 1302 $h/device/uevent && { echo "== $h"; cat $h/device/uevent; od -A x -t x1z -v $h/device/report_descriptor; }
done
```
This yields the **exact** descriptor, `HID_UNIQ`, `bcdDevice` and interface layout
of a wired `0x1302` — the thing to clone, rather than an approximation.

**Step 1 — permissions (once, root).**
```bash
printf 'KERNEL=="uhid", SUBSYSTEM=="misc", MODE="0660", TAG+="uaccess", OPTIONS+="static_node=uhid"\n' \
  | sudo tee /etc/udev/rules.d/72-hyprpad-uhid.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
ls -l /dev/uhid && getfacl /dev/uhid        # expect an ACL for uid 1000
```

**Step 2 — the probe** (`examples/uhid_probe.rs`, ~150 lines, daemon stopped):

1. `UHID_CREATE2`: `bus=BUS_USB(3)`, `vendor=0x28DE`, `product=0x1302`,
   `version=307`, `country=0`, `name="Steam Controller"`,
   `uniq="FXA9961402A6C"`, `phys="hyprpad"`, `rd_data` = step 0's descriptor.
2. Log the `UHID_START` `dev_flags` — this settles §1.3 empirically.
3. Open a real puck node read-only; for each 54-byte `0x42`, clear `raw[4] & 0x01`
   and `UHID_INPUT2` it.
4. **Log, do not act on, every `UHID_OUTPUT` / `UHID_GET_REPORT` /
   `UHID_SET_REPORT`** — hex-dump `rtype`, `rnum`, `size`, `data`. Reply to every
   GET with `err=0, size=0` and every SET with `err=0`, immediately.
   *This log is worth as much as the pass/fail: it is the complete empirical list
   of what Steam sends a Triton, which §4.3's whitelist currently guesses at.*
5. Record `udevadm info -a` for the virtual hidraw node, `getfacl /dev/hidrawN`,
   and whether any phantom evdev nodes appeared.

**Step 3 — run Steam** (`setsid uwsm-app -- steam -silent`, standing permission)
and read `~/.local/share/Steam/logs/controller.txt` — it reports everything
directly:

| Look for | Meaning |
|---|---|
| `getfacl /dev/hidrawN` shows `user:ajg:rw-` | Valve's `KERNELS=="000[356]:28DE:*"` rule fired → §2 confirmed |
| `Local Device Found / type: 28de 1302 / path: /dev/hidrawN` | Steam enumerated it |
| `Interface: -1` followed by `Controller uses V1 HID protocol via USB` | **the `-1` question answered: PASS** |
| `!! Steam controller device opened for index 0.` | **Detection works. Path A is green.** |
| `23CExitLizardModeWorkItem(0)` / a `SET_REPORT` with `data[1]==0x87` in the probe log | Steam is actively driving it — proof of acceptance |
| Settings → Controller shows trackpads / gyro / back grips | Full Triton feature set, not a generic pad |
| `Unable to open local device` | permissions, not detection — fix the rule |
| Nothing at all | Steam's filter is stricter than SDL's → **go to Path B** |

**A/B it in one sitting:** run the probe with `product=0x1302`, then `0x1304`,
then `0x12f0`. The log names each. That single sitting settles the architecture.

---

## 10. Open questions

- **Q-u1** Does Steam accept `0x1302` at `Interface: -1`? **UNKNOWN** — §9. This
  is the only thing standing between "probably" and "yes".
- **Q-u2** The exact set of feature reports Steam sends a Triton. **UNKNOWN** —
  §9 step 4 produces it verbatim.
- **Q-u3** The wired `0x1302` report descriptor. **UNKNOWN today, but 2 minutes
  away** (§9 step 0) — the controller is on hand.
- **Q-u4** Reports `0x44` (6 B) and `0x7B` (13 B) are undecoded and unhandled by
  SDL. **UNKNOWN.** Relay verbatim; never synthesise.
- **Q-u5** Does hyprpad become a root system service, or grow a minimal root
  helper? R2 forces this decision, and W12's device-hide (§8.3) needs the same
  answer — resolve them together.
- **Q-u6** Does the future `hid-steam` Triton path guard on `hid_is_usb()`?
  **UNKNOWN** (§4.6). Determines whether `driver_override` is needed.
- **Q-u7** Does Steam's `DetectUsbEntities` (a libudev path present only in
  `steamclient.so` — a full-tree scan found it in no other Steam binary) matter
  for anything but firmware update / device inventory? **Probably not** — it never
  appears in the input path in the log, and the public reports that name it
  (steam-for-linux#13250) are cases where *firmware update worked but the
  controller was not detected*, i.e. it is the separate USB-subsystem path. A uhid
  device will be invisible to it; that appears to be harmless. **INFERRED.**

---

## Addendum: InputPlumber's proven recipe + why our passive probe was inconclusive

*Produced 2026-09-01. Trigger: a live probe today created a uhid `28de:1302`
device (real 372-byte wired-Triton descriptor, `BUS_VIRTUAL`) that DID enumerate
as `/dev/hidrawN` with `HID_ID=0006:000028DE:00001302` and got `uaccess` from
Valve's rule — but Steam sent only zero feature/output reports and logged nothing
for it. The test was confounded: `/proc/*/fd` showed **Steam held no hidraw open
at all, not even the real puck**, so Steam's controller subsystem was engaging
nothing. This addendum resolves "will Steam adopt a uhid Steam Controller" from
the proven reference implementation rather than from that inconclusive probe.*

**New verification basis.** **VERIFIED(ip-src)** = InputPlumber `main` raw source
read today via `raw.githubusercontent.com/ShadowBlip/InputPlumber/main/…`:
`src/input/target/steam_deck_uhid.rs`, `src/input/target/mod.rs`,
`src/drivers/steam_deck/report_descriptor.rs`, `src/drivers/steam_deck/mod.rs`.
**VERIFIED(sdl)** = `libsdl-org/SDL` `main` read today:
`src/joystick/hidapi/SDL_hidapi_steamdeck.c`,
`src/joystick/hidapi/SDL_hidapi_steam_triton.c`.

### A.0 The single most important correction to §0–§9

**InputPlumber does not clone the Steam Controller (1102/1142) OR the Triton
(1302/1304). It emulates the Steam *Deck* controller protocol under a *non-Deck*
PID, and it is anything but passive: it streams a fresh input report every 4 ms
*and* answers the control-channel GET_REPORT handshake with specific canned
bytes.** Our probe did neither. So the probe did not test the thing the reference
implementation proves is necessary. Detail below.

### A.1 The exact target InputPlumber exposes (VERIFIED ip-src)

`steamos-manager` selects the target string `"deck-uhid"` (existing doc §3.4).
`src/input/target/mod.rs` maps that string to `SteamDeckUhidDevice::new()` with
driver options **`poll_rate: Duration::from_millis(4)` (250 Hz), `buffer_size:
2048`**. The device is created in `create_virtual_device` (VERIFIED ip-src,
`steam_deck_uhid.rs`) with **exactly** this `uhid_virt::CreateParams`:

```rust
CreateParams {
    name: config.name.clone(),          // per-handheld, e.g. "Steam Controller" / "…Controller"
    phys: String::from(""),             // empty
    uniq: String::from(""),             // empty  ← note: NOT a serial (contrast §3.3)
    bus:  Bus::USB,                     // BUS_USB 0x03 — NOT BUS_VIRTUAL (our probe used 0x06)
    vendor:  VID as u32,                // VID = 0x28de   (drivers/steam_deck/mod.rs)
    product: config.product_id.to_u32(),
    version: 0x1000,                    // 4096
    country: 0,
    rd_data: CONTROLLER_DESCRIPTOR.to_vec(),
}
```

`ProductId` (VERIFIED ip-src, `drivers/steam_deck/mod.rs`) — none of these is the
real Deck `0x1205`; the enum exists precisely so a virtual device avoids the
`bInterfaceNumber` gate (existing doc §3.2/§4.6):

```rust
pub const VID: u16 = 0x28de;
pub enum ProductId {                 // to_u16()/to_u32() provided
    SteamDeck        = 0x1205,        // the "true" PID — used only for the vhci/VCHI target
    Generic          = 0x12f0,        // the generic uhid PID (Path B in §8.1.2)
    MsiClaw          = 0x12fa,
    LenovoLegionGo2  = 0x12fb,
    ZotacZone        = 0x12fc,
    AsusRogAlly      = 0x12fd,
    LenovoLegionGo   = 0x12fe,
    LenovoLegionGoS  = 0x12ff,
}
```

**The report descriptor is 38 bytes, pure vendor page, NO report IDs**
(VERIFIED ip-src, `report_descriptor.rs` — matches the "38-byte `06 FF FF`" the
existing doc cited second-hand from hhd, now confirmed byte-for-byte in
InputPlumber):

```rust
pub const CONTROLLER_DESCRIPTOR: [u8; 38] = [
    0x06,0xff,0xff, 0x09,0x01, 0xa1,0x01,     // Usage Page 0xFFFF, Usage 0x01, Collection(App)
    0x09,0x02, 0x09,0x03, 0x15,0x00, 0x26,0xff,0x00, 0x75,0x08, 0x95,0x40, 0x81,0x02, // Input:  64 bytes
    0x09,0x06, 0x09,0x07, 0x15,0x00, 0x26,0xff,0x00, 0x75,0x08, 0x95,0x40, 0xb1,0x02, // Feature:64 bytes
    0xc0,                                     // End Collection
];
```

Consequence for §1.3: because there are **no `REPORT_ID` items**, all three
`UHID_START` `dev_flags` numbered-report bits are **unset** for the Deck target,
so reports are **unprefixed** on the wire. (This is the opposite of a cloned real
`1302` descriptor, which carries report IDs and therefore all three flags set —
§1.3 still holds for Path A; the Deck path just happens to be report-ID-free.)

### A.2 It streams continuously — and for the Deck protocol that is mandatory

**VERIFIED(ip-src).** `SteamDeckUhidDevice::poll()` — invoked by the target driver
on every 4 ms tick — calls `self.write_state()?` **unconditionally**, after
servicing any inbound UHID events. `write_state()` packs the current
`PackedInputDataReport` (whose `frame` field is bumped `frame.wrapping_add(1)`
each write) and does `device.write(&data)` (`UHID_INPUT`). `write_event()` only
mutates internal state; **the wire write happens on the timer, not on input
change**. So the device emits ~250 input reports/second whether or not anything
moved. This is a live keepalive, exactly what our passive fake lacked.

**Why the stream is load-bearing (VERIFIED sdl):** SDL's Steam *Deck* hidapi
backend gates detection on a live report —
`src/joystick/hidapi/SDL_hidapi_steamdeck.c` `InitDevice`:

```c
size = SDL_hid_read_timeout(device->dev, data, sizeof(data), 16);
if (size == 0)
    return false;      // no report within 16 ms → device rejected outright
```

So a **passive** Deck-protocol device is rejected before anything else is even
examined. This is the single cleanest explanation of "a passive fake is ignored"
for Path B.

**Contrast, and it matters for Path A (VERIFIED sdl):** SDL's *Triton* backend,
`SDL_hidapi_steam_triton.c` `HIDAPI_DriverSteamTriton_InitDevice`, does **not**
read at init for a wired unit — it sets the name and returns
`HIDAPI_DriverSteamTriton_SetControllerConnected(device, true)`; the only read is
later in `UpdateDevice`. So under **SDL**, a wired `0x1302` needs *no* input
stream to be enumerated. **But Steam's own client uses its native "V1 HID protocol
via USB" driver for the Triton, not SDL** (existing doc §3.3), and that driver's
stream requirement is **UNPROVEN**. The safe reading of the reference impl is:
assume Steam's client wants a live stream too, and stream regardless.

### A.3 The GET_REPORT handshake it answers — the likely real reason passivity fails

**VERIFIED(ip-src).** On the control channel, `SteamDeckUhidDevice` answers three
GET_REPORT (feature) requests with canned data via
`device.write_get_report_reply(id, 0, data)` — i.e. `err=0` plus a fixed payload,
selected by a `current_report` field that a prior SET_REPORT can switch:

- **`GetAttributesValues`** — the attributes blob. Reply (padded to 64):
  ```
  0x00, <GetAttributesValues>, 0x2d, 0x01, 0x05, 0x12, 0x00, 0x00, 0x02, 0x00,
  0x00, 0x00, 0x00, 0x0a, 0x2b, 0x12, 0xa9, 0x62, 0x04, 0xad, 0xf1, 0xe4, 0x65,
  0x09, 0x2e, 0x00, 0x00, 0x00, 0x0b, 0xa0, 0x0f, 0x00, 0x00, 0x0d, 0x00, 0x00,
  0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, /* …zeros… */
  ```
  In-source comment, verbatim: *"No idea what these bytes mean, but this is what
  is sent from the real device."* (`0x2d` = 45 = payload length; the body is a run
  of `attr_id, u32` TLVs — `0x01,0x02,0x0a,0x09,0x0b,0x0d,0x0c,0x0e` — the same
  attribute-id encoding §4.3 sketched from hhd, now confirmed as literal bytes.)
- **`GetStringAttribute`** (serial): `[0x00, <type>, 0x14, 0x01]` + serial bytes,
  resized to 64. Default `serial_number = "1NPU7PLUMB3R"`.
- **`GetChipId`**: `[0x00, <type>, 0x11, 0x00]` + 15-byte `chip_id`
  (`[0,1,2,3,4,5,6,7,8,9,0,1,2,3,4]`), resized to 64.

**This is the handshake a passive fake cannot pass.** A device that replies to
every GET_REPORT with `err=0, size=0` (as our probe did) hands Steam an empty
attributes/serial/chipid blob. InputPlumber demonstrates that a *specific,
correctly-framed* reply is part of what Steam consumes on a Valve controller.
Whether Steam *hard-rejects* an empty reply or merely logs and tolerates it (the
existing doc §3.3 notes `Deck Controller PCB Serial# invalid: NA` is *survived*
for the real Triton) is the open margin — but the reference impl never leaves
these empty, so neither should the probe.

### A.4 How it handles SET_REPORT / OUTPUT (VERIFIED ip-src)

It **decodes-and-translates the known ones, swallows the rest** — it does **not**
forward to a physical device (InputPlumber synthesises from *other* hardware):

- `TriggerRumbleCommand` → unpack `PackedRumbleReport` → emit
  `OutputEvent::SteamDeckRumble` (routed to the source device's FF).
- `TriggerHapticCommand` → unpack `PackedHapticReport` → emit
  `OutputEvent::SteamDeckHaptics` (`Haptic::TrackpadLeft/Right`).
- Everything else: `log::trace!("Got SetReport for ReportType we aren't handling:
  …")` — swallowed. A SET_REPORT can also switch `current_report` so the next
  GET_REPORT returns the matching canned blob. `lizard_mode_enabled` is tracked as
  a bool, not forwarded.

For hyprpad this is the one place the recipe *differs by design*: hyprpad has the
real puck behind the clone, so §4.3's **relay** policy is strictly better than
InputPlumber's translate-or-swallow — but InputPlumber proves the *minimum* Steam
needs is "accept the write and reply success promptly," which the relay also does.

### A.5 What triggers Steam to (re)detect — resolves the "holds nothing open" state

**Detection path (VERIFIED local, from §3.1's symbol dump + INFERRED assembly):**
`steamclient.so` imports both `udev_enumerate_scan_devices` (a one-shot scan at
Steam startup) **and** `udev_monitor_new_from_netlink` +
`udev_monitor_filter_add_match_subsystem_devtype` (a live hotplug monitor filtered
on `hidraw`). So Steam adopts a hidraw node in exactly two situations: (1) it
existed and was permitted at Steam's own startup scan, or (2) a udev **`add`**
uevent for a `hidraw` device arrives on the netlink monitor while Steam runs.

**Does `UHID_CREATE2` fire that uevent? Yes (VERIFIED kernel, existing doc §1.2 +
standard driver-model behaviour).** `uhid_dev_create2` → `hid_add_device`
(deferred to a workqueue) → `device_add` on the HID device → `KOBJ_ADD` uevent;
`hid-generic` then binds → `hidraw_connect` → `device_create` for `/dev/hidrawN` →
a second `KOBJ_ADD` uevent with `SUBSYSTEM=hidraw`. That second event is exactly
what Steam's monitor is filtered for. **This is not theoretical: it is the
mechanism by which SteamOS's runtime target switch works** — `steamos-manager`
tells InputPlumber to switch to `"deck-uhid"`, InputPlumber tears down and
recreates its uhid device, and Steam adopts the new virtual controller **live,
without a Steam restart** (existing doc §3.4, VERIFIED ip). A uhid device is
therefore detectable after Steam is up *provided Steam's monitor is running and
the node gets `uaccess`* — which Valve's `KERNELS=="000[356]:28DE:*"` rule grants.

**So the "Steam holds nothing open, not even the puck" state is almost certainly
not a detection-plumbing failure — it is Steam not being in a controller-engaging
state at all** (next section). If it *were* a plumbing failure, `udevadm trigger
--action=add` on the virtual node, or a create-after-Steam-is-up, re-fires the
`add` and forces a re-scan.

### A.6 Is there a Steam setting gating this? (plausible root cause of "empty list")

**Yes, and it is the leading explanation for zero engagement.** Steam only holds a
controller's hidraw open **while it is actively running Steam Input on it**, and
that is gated by toggles, not just detection:

- **Settings → Controller** has the global Steam Input enablement and a "Detected
  Controllers" list. If Steam Input is off (or the specific device family's
  support is off), Steam may enumerate but not grab.
- **Per-game** "Manage → Controller Options" is `Default / Enabled / Disabled /
  Forced On`; `Disabled` (or a desktop with no game focused and desktop-config
  Steam Input off) yields exactly "nothing open."
- Desktop vs Big Picture differs: BPM is far more aggressive about grabbing.

**How to check / force a detecting state (do this BEFORE trusting any negative):**
open **Settings → Controller**, confirm the real puck appears in "Detected
Controllers" and Steam Input is enabled; put a game in the foreground with
controller option **Forced On**, or enter **Big Picture**. The definitive baseline
is behavioural, not UI: with hyprpad **not** owning the puck and no fake present,
does `controller.txt` log `!! Steam controller device opened for index N` and does
`/proc/<steam>/fd` show the puck's `/dev/hidrawN`? **If Steam will not grab the
real puck, it will not grab the fake, and the probe proves nothing.** That gate is
the confound we hit.

### A.7 1302 vs 1304 vs the InputPlumber/Deck target — recommendation

- **`1304` is out** — the `bInterfaceNumber` slot gate, unchanged (§3.2).
- **InputPlumber's actual choice is the Deck protocol under `0x12f0`/`0x12fx`,
  `BUS_USB`, VID `0x28de`, 38-byte vendor descriptor, streamed at 250 Hz, with the
  three canned GET_REPORT replies.** This is the *proven-in-production* path
  (SteamOS ships it). Its cost for hyprpad is real: transcode the puck's `0x42`
  into the Deck `PackedInputDataReport`, synthesise the attribute/serial/chipid
  replies, and translate Steam's Deck rumble/haptic SET_REPORTs into IBEX
  `0x80`/`0x81` — and it forfeits the free-gyro/trackpad passthrough.
- **`1302` remains the least-translation target for hyprpad**, *because hyprpad
  owns the real Triton* — if Steam's native client driver adopts a parentless
  `1302`, Steam speaks the exact IBEX protocol hyprpad already relays (`0x42` in,
  `0x01` feature, `0x80/0x81` out), so trackpads/gyro/haptics pass through for
  free. The InputPlumber recipe does not change this recommendation; it *sharpens
  the requirements on the `1302` attempt*: stream continuously, and answer the
  GET_REPORT handshake with real (relayed) data rather than empty replies. In
  other words, the reference impl says the probe's **passivity**, not its identity,
  is the more likely culprit — but only a corrected probe (A.8) separates the two.

Net recommendation, unchanged in direction but firmer: **build Path A on `1302`,
but the client must (1) stream input at ~250 Hz from device-create onward and
(2) answer every GET_REPORT with relayed puck data (never `size=0`).** Keep Path B
(`0x12f0`, Deck) as the SteamOS-proven fallback, for which A.1–A.4 are now a
copy-able spec.

### A.8 The one decisive experiment (confound-controlled)

Replaces §9's probe with three gates the original lacked — a *detecting-state*
baseline, a *live stream*, and *answered handshakes*. All in one Steam session.

1. **Prove Steam is engaging controllers (the gate we skipped).** Stop hyprpad;
   ensure the real puck is visible + permitted. Launch Steam. Enable Steam Input
   (Settings → Controller); foreground a game with controller option **Forced On**
   (or use Big Picture). **Confirm** `controller.txt` logs `!! Steam controller
   device opened for index N` for the real puck **and** `/proc/<steam-pid>/fd`
   contains its `/dev/hidrawN`. **Do not proceed until this holds** — this is the
   step whose absence invalidated today's probe.
2. **Create an *active* fake, while Steam is already up** (so the `add` uevent hits
   the live monitor). Use `bus=BUS_USB(0x03)` (not `BUS_VIRTUAL`), `vendor=0x28DE`,
   `product=0x1302`, `version=307`, `name="Steam Controller"`,
   `uniq="FXA9961402A6C"`, `phys="hyprpad"`, `rd_data` = the real wired-`1302`
   descriptor (§9 step 0). Then, unlike the passive probe:
   - **Stream** a valid `0x42` report continuously at ~250 Hz from the moment of
     create — copy the real puck's frames (guide bit cleared) or emit a neutral
     frame with an incrementing sequence byte. This clears any SDL-style 16 ms read
     gate and mimics InputPlumber's unconditional per-tick write.
   - **Answer every `UHID_GET_REPORT`** immediately by relaying to the real puck
     (`HIDIOCGFEATURE`) — or, if the puck is absent, with correctly-*framed* canned
     data à la A.3 (right length byte, right leading `report_id`/`type`), never
     `size=0`. **Answer every `UHID_SET_REPORT`** with `err=0`. Never let the 5 s
     kernel timeout fire (§1.2).
3. **Read the verdict** in `controller.txt` + `/proc/*/fd`:

   | Observation | Meaning |
   |---|---|
   | `Local Device Found / type: 28de 1302 / path: /dev/hidrawN`, `Interface: -1`, `Controller uses V1 HID protocol via USB` | enumerated; the `-1` question answered |
   | **`!! Steam controller device opened for index N`** + `/proc/<steam>/fd` holds the fake's `/dev/hidrawN` | **PROVEN: Steam adopts it** |
   | `CGetControllerInfoWorkItem` / `CExitLizardModeWorkItem` / `SET_REPORT` with the lizard `0x87` arriving on the uhid fd | Steam is actively driving it — dispositive |
   | Enumerated but never opened, *while the real puck IS opened in the same session* | identity/parentless-ness rejected → A/B to `product=0x12f0` (Deck) with the A.1–A.4 stream+handshake and retest |
   | Nothing, and the real puck is ALSO not opened | still not a detecting state — return to step 1; the result is again meaningless |

   Run it once as `1302`, then (device destroyed/recreated) as `12f0` with the Deck
   descriptor + streamed `PackedInputDataReport` + the three canned replies. That
   A/B, in one Steam sitting that has *already been shown to grab the real puck*,
   settles the architecture.

### A.9 Bottom line

- **(a) Exact recipe to copy (the proven one, `deck-uhid`):** `uhid_virt`
  `CreateParams { bus: Bus::USB, vendor: 0x28de, product: 0x12f0 (Generic; or a
  `0x12fx` per-handheld), version: 0x1000, country: 0, name: "…Controller",
  phys: "", uniq: "", rd_data: 38-byte vendor descriptor (A.1) }`; a target driver
  polling at **4 ms** that writes the current 64-byte input report **every tick**
  (frame counter `wrapping_add(1)`, no report-ID prefix); GET_REPORT answered with
  the canned `GetAttributesValues` / `GetStringAttribute` (`"1NPU7PLUMB3R"`) /
  `GetChipId` blobs (A.3); SET_REPORT rumble/haptic decoded, everything else
  swallowed with `err=0`. For hyprpad's *relay* variant, keep this shape but swap
  the `1302` identity + real descriptor and relay GET/SET to the puck.
- **(b) Why our passive `1302` probe got zero engagement:** the dominant cause is
  that **Steam was not in a controller-engaging state** — it held no hidraw open at
  all, not even the real puck, so it was adopting *nothing*; the fake never got a
  fair test (A.5/A.6). Secondarily, even in a detecting state the fake violated
  **both** invariants the reference implementation shows are required of a virtual
  Valve controller: it **streamed no input** (fatal at SDL's 16 ms Deck gate; risk
  unproven but plausible for Steam's Triton client driver) and it **answered every
  GET_REPORT empty** instead of with framed attribute/serial/chipid data. It also
  used `BUS_VIRTUAL` rather than InputPlumber's `BUS_USB`. The zero feature/output
  reports we watched were kernel/startup noise, not Steam driving the device.
- **(c) The one decisive experiment:** A.8 — first *prove Steam grabs the real
  puck this session*, then present an **active** `1302` fake (stream ~250 Hz +
  answer GET/SET) created while Steam is live, and look for `!! Steam controller
  device opened for index N` plus the fake's fd in `/proc/<steam>/fd`. That is the
  single run that converts "probably" to "proven," with the confound removed.
