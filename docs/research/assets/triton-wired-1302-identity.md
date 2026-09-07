# Wired Steam Controller (28de:1302) capture — uhid impersonation blueprint

**IMPORTANT hardware correction (2026-09-01):** this `1302` descriptor was captured
from a *separate* single-interface Steam Controller ("the controller"), NOT the
2026 puck. Confirmed by re-enumerating with the puck cabled:

- **The puck** = `28de:1304`, product "Steam Controller Puck", serial FXB0000000002,
  **7 USB interfaces → 5 hidraw slots** (phys input2..input6), *wired or wireless*.
  This is what hyprpad owns. Cloning ITS identity via uhid fails the interface-number
  slot test (SDL gates 1304 on interface 2..5; uhid has no interface).
- **"The controller"** = `28de:1302`, single interface 0, serial FXA0000000001.
  Its descriptor is the one saved here — the SDL WIRED branch takes it with no
  interface test, which is why it's the impersonation identity of choice.

**Architecture consequence:** present Steam a `1302` (single-interface) identity it
will happily drive, and RELAY underneath to the real `1304` puck. Steam thinks
`1302`; hardware is `1304`; hyprpad translates input reports in and feature reports
out. Open question for the build: how cleanly the puck's `0x42` report + its feature
set map onto the `1302` protocol Steam will speak — TBD after the decisive test.

## uhid CREATE2 fields (present the fake as this)
- bus: BUS_USB (0x03) for the descriptor's origin; **create the virtual device on
  BUS_VIRTUAL (0x06)** so Valve's 60-steam-input.rules `000[356]:28DE:*` matches it.
- vendor: 0x28DE  product: 0x1302  version/bcdDevice: 0x0307
- name (HID_NAME): "Valve Software Steam Controller"
- uniq (HID_UNIQ): the controller serial, e.g. FXA0000000001 — Steam keys
  configset_<UNIQ>.vdf on this, so it is the Steam Input identity. (Use the real
  puck's own serial so per-game configs follow the hardware.)
- phys: any stable string (e.g. "hyprpad-uhid/1302").
- report descriptor: triton-wired-1302-report-descriptor.bin (372 bytes)

## USB shape (single interface — the key advantage over 1304)
- one HID interface, bInterfaceNumber 0, class 03 proto 00. SDL's WIRED-Triton
  branch has no interface-number test, so uhid's missing interface is fine here
  (unlike wireless 1304, gated on interface 2..6).

## Report descriptor
- 372 bytes; three top-level collections — the V1 wired HID protocol Steam's log
  calls "V1 HID protocol via USB".
- full hexdump: triton-wired-1302-report-descriptor.hex

> **Correction (2026-09-02).** This section originally read "top-level Generic
> Desktop / report id 0x40 (64-byte reports)". Walking the bytes says otherwise:
> `0x40` is the **lizard mouse** (5-byte payload) in the Generic Desktop
> collection, `0x41` the lizard keyboard (8), and the Steam protocol lives in the
> vendor `0xFF00` collection — input `0x42` with 53 payload bytes, feature `0x01`
> and `0x02` with 63 each, and ten numbered output reports including `0x80`
> rumble (9) and `0x81` haptic pulse (7). The full table is pinned in
> `src/uhid/profile.rs`
> (`the_triton_descriptor_report_table_is_pinned`) and reproduced in
> `docs/design/uhid-relay.md` §1. Nothing in this descriptor is 64 bytes on the
> input side, and nothing in it is unnumbered.

## Relay plan (Path A)
- Real puck streams input on its own report id; the wired 1302 presents id 0x40.
  hyprpad relays real input reports into UHID_INPUT2 (guide/Steam bit stripped
  while a game is focused), and relays Steam's UHID_OUTPUT / UHID_SET_REPORT
  (haptics, lizard, gyro-enable, config) back out to the real puck's hidraw.
- GET_REPORT queries (serial/config) answered from the captured identity or
  relayed to the real device.
