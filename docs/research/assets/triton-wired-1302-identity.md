# Wired Triton (28de:1302) capture — uhid impersonation blueprint

Captured 2026-09-01 from the owner's puck plugged in by USB cable.

## uhid CREATE2 fields (present the fake as this)
- bus: BUS_USB (0x03) for the descriptor's origin; **create the virtual device on
  BUS_VIRTUAL (0x06)** so Valve's 60-steam-input.rules `000[356]:28DE:*` matches it.
- vendor: 0x28DE  product: 0x1302  version/bcdDevice: 0x0307
- name (HID_NAME): "Valve Software Steam Controller"
- uniq (HID_UNIQ): the controller serial, e.g. FXA9961402A6C — Steam keys
  configset_<UNIQ>.vdf on this, so it is the Steam Input identity. (Use the real
  puck's own serial so per-game configs follow the hardware.)
- phys: any stable string (e.g. "hyprpad-uhid/1302").
- report descriptor: triton-wired-1302-report-descriptor.bin (372 bytes)

## USB shape (single interface — the key advantage over 1304)
- one HID interface, bInterfaceNumber 0, class 03 proto 00. SDL's WIRED-Triton
  branch has no interface-number test, so uhid's missing interface is fine here
  (unlike wireless 1304, gated on interface 2..6).

## Report descriptor
- 372 bytes; top-level Generic Desktop / report id 0x40 (64-byte reports) — the
  V1 wired HID protocol Steam's log calls "V1 HID protocol via USB".
- full hexdump: triton-wired-1302-report-descriptor.hex

## Relay plan (Path A)
- Real puck streams input on its own report id; the wired 1302 presents id 0x40.
  hyprpad relays real input reports into UHID_INPUT2 (guide/Steam bit stripped
  while a game is focused), and relays Steam's UHID_OUTPUT / UHID_SET_REPORT
  (haptics, lizard, gyro-enable, config) back out to the real puck's hidraw.
- GET_REPORT queries (serial/config) answered from the captured identity or
  relayed to the real device.
