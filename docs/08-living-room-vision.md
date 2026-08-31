# 08 — The living-room vision

The target experience, as described by the project owner on 2026-08-31, and the
gap between it and today.

## Today

The living room is a desktop tower running Omarchy/Hyprland, driven by a Logitech
K400 media-centre keyboard. Every interaction that is not *playing the game*
means putting down the controller and picking up the keyboard: unlocking the
disk, navigating Steam, launching apps, changing workspace, taking screenshots,
media multitasking, locking, suspending, resuming.

The affordances the Steam Controller is supposed to provide are absent in
practice. Virtual keyboard input works only inside Steam's own UI. Trackpad
cursor control works only when Steam is **fully closed**
([03](03-hardware-findings.md#steams-handling-of-this-controller-is-broken-today)).

This is dramatically worse than a console, or than a Steam Deck.

## The target

A single narrative, which the programme in [09](09-programme.md) is costed
against.

1. **Cold boot.** Power button on the tower. Omarchy's disk-decrypt prompt
   appears. A controller-optimised security code is entered using buttons — or,
   if none is configured, a full QWERTY keyboard that does not betray specific
   inputs to a bystander in the room.
2. **Seamless handoff.** Autologin carries the LUKS passphrase through SDDM. No
   flashing, no intermediate UI. The first thing seen after the passcode is the
   Steam Big Picture splash.
3. **Environment-aware launch.** Hyprland recognises it is in the living room and
   boots directly to Big Picture in a named `steam` workspace. At the desk, it
   does not.
4. **Workspace navigation.** `Guide+R1` / `Guide+L1` move between workspaces.
5. **Launching.** `Guide+X` opens the Omarchy launcher. A custom OS-level virtual
   keyboard appears with Steam-parity affordances: dual-trackpad entry,
   alternating with d-pad navigation, modality switching automatically between
   the two. Type a few letters, dismiss with `B` or `Start`, then d-pad and
   trackpad-scroll drive the launcher, `A` confirms.
6. **Desktop use.** Left trackpad drives the cursor. Right trigger clicks.
   Clicking a text field raises the same OS-level keyboard. A microphone control
   engages voice input (Whisper-style, ideally via Omarchy's existing support).
7. **Picture-in-picture.** A guide chord floats and pins a window as PiP.
8. **Back to the game.** `Guide+L1` returns to the `steam` workspace. Big Picture
   takes controller input; the PiP video keeps playing.
9. **Game launch.** The same classifier that chose Big Picture also decides the
   game launches under **gamescope** with full resolution, refresh rate and HDR.
   At the desk, the same game launches directly under XWayland.
10. **In-game multitasking.** Guide chords pause the video and hide overlays for
    a cinematic, then restore both. **The game retains primary controller focus
    across every one of these interactions.**
11. **Ask an AI about the game.** A guide chord screenshots to disk *and*
    clipboard. `Guide+R1` to a fresh workspace, launcher, Claude in a browser,
    paste the screenshot, type or dictate the question. `Guide+L1`/`R1` freely
    between game and browser while waiting.
12. **Lock, unlock, suspend, resume** — all controller-driven, never touching a
    keyboard.

Everything above is configurable from the Hyprland config.

## Input contract

The rule that makes the rest coherent:

- **`Guide` + anything** belongs to the desktop layer and never reaches the
  focused application.
- **A bare `Guide` press and release** passes through to Steam Big Picture, so
  Steam's own button still works.
- **Steam's own guide chords are inert** — the desktop layer owns chords.
- Everything else follows focus, like any other input device.

The bare-press passthrough is the subtle one. Because Steam acts on guide
*release* ([03](03-hardware-findings.md#steams-reaction-to-the-guide-button)), a
daemon that owns the device can wait for release, and only then synthesise a
press-and-release on the virtual controller if no chord occurred. No guessing, no
timeout heuristic.

## Eventual target

Migrate the whole stack to a Steam Deck running Omarchy XR / HypXRland. Treated
as a portability constraint on design, not a v1 deliverable.
