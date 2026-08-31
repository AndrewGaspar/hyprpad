# 02 — Background: the Linux input stack

Enough of the stack to make the architecture choices in
[05 — Architectures](05-architectures.md) legible.

## The layers

```
       USB / Bluetooth transport
                 │
         ┌───────▼────────┐
         │  HID core      │   parses the report descriptor
         └───┬────────┬───┘
             │        │
   ┌─────────▼──┐  ┌──▼──────────────┐
   │  hidraw    │  │  a HID driver   │  hid-generic, hid-steam, hid-playstation…
   │ /dev/hidraw│  │                 │
   └─────────┬──┘  └──┬──────────────┘
             │        │ emits
             │   ┌────▼─────────────┐
             │   │  evdev           │  /dev/input/eventN
             │   └────┬─────────────┘
             │        │
             │   ┌────▼─────────────┐      ┌──────────────┐
             │   │  libinput        │─────▶│  compositor  │  keyboards, mice,
             │   │  (no joysticks!) │      │  (Hyprland)  │  touchpads only
             │   └──────────────────┘      └──────────────┘
             │
             └──▶ applications reading HID directly (Steam, SDL)
```

Two userspace-facing device nodes matter.

**`hidraw`** hands over raw HID reports, unparsed. Steam uses it for Valve
hardware because it needs the vendor-specific reports — gyro, trackpads,
haptics, lizard-mode control — that the generic HID parser does not expose as
input events.

**`evdev`** is the normalized, kernel-parsed view: `EV_KEY`/`BTN_SOUTH`,
`EV_ABS`/`ABS_X`, and so on. This is what SDL, `libinput` and everything generic
consumes.

## Exclusivity: the property the whole design hinges on

| Node | Multiple readers? | Exclusive mode |
|---|---|---|
| `evdev` | Yes, by default | `EVIOCGRAB` ioctl — one grabber, all others starve |
| `hidraw` | **Yes, always** | **No exclusive mode exists** |

`hidraw` has no equivalent of `EVIOCGRAB`. Every open file description gets its
own report queue, and the kernel fans each incoming report out to all of them.
There is no "steal" and no "lock".

This is *why* the recommended architecture works: a passive reader can watch the
Steam Controller alongside Steam without asking Steam's permission and without
Steam noticing. It is also why passive tapping cannot *suppress* anything —
there is no way to consume a report so that Steam does not also get it.

For `evdev`, the same fan-out applies unless someone calls `EVIOCGRAB`. Most
remapping tools (`evsieve`, `evremap`, `input-remapper`) do grab, precisely
because they need suppression: without it, both the original and the remapped
events reach applications.

## Writing back: uinput vs uhid

Two ways to create a virtual device:

- **`uinput`** creates a virtual *evdev* device. Simple. This is how Steam
  publishes its emulated Xbox 360 pad, and how every keyboard remapper emits its
  output. It cannot express anything evdev cannot express, so no gyro-as-HID, no
  trackpads-as-HID, no vendor reports.
- **`uhid`** creates a virtual *HID* device — you supply a report descriptor and
  feed raw reports. Applications that talk `hidraw` see something indistinguishable
  from real hardware. InputPlumber uses this to present a synthetic Steam Deck
  controller so that Steam enables Deck features for non-Deck handhelds.

`uhid` is the only route to a virtual device that Steam will treat as a *Steam
Controller* rather than as a generic pad. It is also considerably more work, and
requires reimplementing the vendor protocol faithfully enough to fool the client.

## Why gamepads never reach the compositor

`libinput` explicitly excludes joysticks and gamepads. The maintainers' position
is that game input needs semantics libinput does not model, so it declines the
device class entirely. Hyprland, built on `wlroots`-style input handling via
Aquamarine, therefore never sees a gamepad — confirmed locally, where
`hyprctl devices` lists only the controller's *lizard-mode* mouse/keyboard
interfaces, never a gamepad.

Consequences:

- There is no `bind = ,GAMEPAD_A, exec, …` in Hyprland, and cannot be without
  the compositor growing its own joystick handling.
- Gamepad input has no focus routing. A daemon reading the pad must decide for
  itself whether the desktop or a game should receive a given gesture.
- Conversely, anything a daemon *synthesizes* — virtual pointer, virtual
  keyboard — does go through normal focus routing, because those are ordinary
  input devices.

## Getting input *into* a Wayland desktop

Four routes, in rough order of preference for this project:

1. **Compositor IPC.** Hyprland's socket at
   `$XDG_RUNTIME_DIR/hypr/$HIS/.socket.sock` accepts `dispatch` commands
   (`workspace`, `movewindow`, `fullscreen`, `exec`, …), and `.socket2.sock`
   streams events. Highest-level and most reliable: it expresses window-manager
   intent directly rather than faking keystrokes that a keybind then interprets.
2. **`zwlr_virtual_pointer_v1` / `zwp_virtual_keyboard_v1`.** Wayland protocols
   for injecting pointer and key events into the compositor, which then routes
   them to the focused surface normally. No root, no `/dev/uinput`, no XWayland
   involvement. Both are supported by Hyprland (verified — see
   [03](03-hardware-findings.md#portal-and-protocol-support)). This is the right
   tool for cursor control and for keystrokes aimed at applications.
3. **`uinput`.** A virtual keyboard/mouse at the kernel level, seen by
   `libinput` and hence by the compositor as a real device. Works everywhere,
   including XWayland clients, but needs write access to `/dev/uinput` and is a
   blunter instrument. `ydotool` is the common front-end.
4. **`XTEST`.** Effectively unavailable — see
   [01 — Problem statement](01-problem.md#steams-desktop-input-emulation-does-not-reach-a-wayland-desktop).

## Lizard mode

Valve controllers ship firmware that presents as a plain HID keyboard and mouse
when no Steam client is present — trackpad drives the cursor, face buttons send
keystrokes. Valve calls this *lizard mode*. It is a genuine HID device, so it
flows through `libinput` and works on any desktop, Wayland included.

Steam disables it on startup (`CExitLizardModeWorkItem` in the client log) and
replaces it with its own emulation — which, on Hyprland, does not work. The
practical result is the inversion noted in the source conversation for this
project: **the controller's desktop behaviour is better before Steam starts than
after.**

Lizard mode is worth remembering as a fallback layer. It is also a hazard: any
design that takes the device away from Steam must decide, itself, whether to
leave lizard mode on or off.
