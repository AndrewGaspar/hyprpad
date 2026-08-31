# 01 — Problem statement

## What is wanted

Drive a Hyprland desktop from a game controller, well enough that a couch or
lean-back session never needs a keyboard.

1. **The guide button is the prime modifier.** Holding it puts the controller
   into a "window manager" layer. *Guide + right-stick flick* changes workspace;
   other chords move windows, launch things, toggle fullscreen, and so on.
2. **Cursor and text still work.** Trackpads move a real pointer; a virtual
   keyboard or on-screen input is reachable.
3. **Games win when a game is running.** Entering a game hands the controller
   back. No stray workspace switches mid-firefight.
4. **Steam Input is not sacrificed.** Steam's per-game controller configurations
   remain the mechanism for mapping older games to newer controllers. Whatever is
   built must sit *beside* Steam, not replace it.
5. **Not only the Steam Controller.** The 2026 Steam Controller is the primary
   target, but a generic XInput/DirectInput-class pad should work too, with
   whatever subset of features it has.

## Why this is not simply a configuration exercise

Four independent obstacles stack up.

### Gamepads are invisible to the compositor

Wayland compositors do not route gamepads. `libinput` deliberately does not
handle joysticks, so Hyprland never sees a gamepad as an input device and there
is no notion of a "focused" application for controller input. Every process with
read permission on the device node reads *all* of its events, always.

This cuts both ways. It means there is no compositor-level keybinding mechanism
to hook — Hyprland cannot bind a gamepad button, because it does not know the
gamepad exists. It also means arbitration between Steam and anything else is not
handled by anybody; whoever opens the device gets the events.

### Steam's desktop input emulation does not reach a Wayland desktop

Steam emits emulated pointer and keyboard input through the X11 `XTEST`
extension. Under Hyprland, Steam is an XWayland client, so those events land
inside XWayland and go no further.
`XTestFakeRelativeMotionEvent` is not implemented in XWayland at all — only
pointer warping is. The visible symptom is a cursor that Steam's own UI reacts to
but that nothing else on the desktop sees.

The upstream fix is `libei`: XWayland can translate `XTEST` into `libei` events
negotiated through the `RemoteDesktop` portal, supported since XWayland 23.2.0.
The gap on Hyprland is the portal side — `xdg-desktop-portal-hyprland` does not
ship a `RemoteDesktop` backend upstream ([#252](https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/252)).
This was confirmed locally; see [03 — Hardware findings](03-hardware-findings.md#portal-and-protocol-support).

The consequence for this project is specific and important: **any design that
routes desktop control through Steam Input's keyboard/mouse output is dead on
arrival**, because that output cannot escape XWayland.

### The guide button is claimed

Steam reserves the Steam/Guide button; it cannot be rebound in Steam Input. The
most Steam offers is a *Guide Button Chord Layout* — bindings that fire when the
guide button is held together with another control. That is the right shape, but
its output is keyboard/mouse emulation, which lands in the XWayland trap above.

Reassigning the guide button therefore means either observing it in parallel with
Steam and tolerating Steam's own reaction, or taking the device away from Steam
entirely.

### The controller is new, and support is in flux

The 2026 Steam Controller (`28de:1304`, "Steam Controller Puck") post-dates the
kernel on this machine. Kernel 7.3 merges the first `hid-steam` support for it;
7.1.9 binds it to `hid-generic`. SDL3 gained support separately. Steam's own
handling of it is buggy today — a Deck-specific registration path fails on every
launch. See [03 — Hardware findings](03-hardware-findings.md).

## Constraints adopted

- **Do not degrade Steam.** Any design that makes Steam Input worse than it is
  today is a regression, regardless of how good the desktop story becomes.
- **Prefer userspace.** No custom kernel module. Kernel work belongs upstream in
  `hid-steam`, not in this project.
- **Prefer no root.** Wayland virtual-input protocols avoid `/dev/uinput`
  privileges; use them where possible.
- **Degrade gracefully.** A generic pad should get a useful subset without
  device-specific reverse engineering.
