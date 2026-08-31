# 04 — Prior art

What already exists, what each is genuinely good at, and whether it can be
reused rather than rebuilt.

## InputPlumber

Rust daemon; input router and remapper, DBus-controlled, systemd service. Part
of SteamOS and packaged in Arch `extra` (`inputplumber`).

**What it does well.** Composites several physical devices into one logical
device — the standard fix for handhelds that split a gamepad across a keyboard
interface and a joystick interface. Ships ~35 capability maps (ROG Ally, MSI
Claw, Ayaneo, Legion Go, GPD, Zotac Zone). Emits target devices via both `uinput`
and `uhid`, including a synthetic **Steam Deck** controller so Steam enables Deck
features on non-Deck hardware.

**Intercept mode** is the interesting part: set it over DBus and input events are
routed to a DBus client instead of to applications. `GAMEPAD_ONLY` intercepts
gamepad input while leaving keyboard/mouse alone. OpenGamepadUI uses this to show
an overlay without leaking presses to the game underneath — structurally the same
problem as "guide button opens my WM layer, not the game's menu".

**Why it is not simply the answer here.** It has no 2026 Steam Controller
profile; one would have to be written, which means doing the report decoding in
[03](03-hardware-findings.md) anyway. Its model is *own the device and re-emit*,
so using it means taking the controller away from Steam — the exact cost the
passive-tap design avoids. It is also a large dependency for what may be a small
daemon, and it has had DBus authorization problems
([CVE-2025-66005 / CVE-2025-14338](https://security.opensuse.org/2026/01/09/inputplumber-lack-of-dbus-auth.html):
missing DBus authorization and input validation allowing UI input injection and
DoS).

**Verdict.** The right thing to adopt *if* the escalation path in
[06](06-recommendation.md) is taken. Not needed for the passive design.

## Handheld Daemon (HHD)

Python daemon by hhd-dev; preinstalled on Bazzite. Hardware abstraction, controller
emulation, TDP and RGB control for handheld PCs.

**Directly relevant precedent.** HHD's whole reason for existing is the problem
in this project: handhelds have vendor buttons (a "Menu" key, a QAM key) that
nothing understands, so HHD grabs the physical devices, interprets chords, and
re-emits a clean virtual controller. It ships generic HID emulators for Xbox
Elite, DS4, DualSense and Joy-Cons so a user can pick a target per game. It
implements chord semantics explicitly — on the GPD Win 4, one physical button
serves as short-press QAM, long-press Xbox, double-press HHD menu.

**Why not reuse.** It is handheld-specific by design (built around integrated
hardware, TDP, RGB), Python, and again predicated on owning the device. The
*design pattern* — tap/hold/double-tap discrimination on a system button, with a
per-game target device — is worth stealing wholesale. The code is not a fit.

## Steam Input

Steam's own layer: per-game controller configurations, action sets, a Desktop
Layout for out-of-game use, and a **Guide Button Chord Layout** for bindings that
fire while the guide button is held.

**The shape is exactly right.** A guide-chord layer, arbitrated automatically
against whatever game is running, configurable per title, working across every
controller Steam supports — that is the feature this project wants.

**Two hard blocks.**

1. Its keyboard/mouse output goes through `XTEST`, which does not escape XWayland
   on Hyprland. Verified locally: no `RemoteDesktop` portal backend
   ([03](03-hardware-findings.md#portal-and-protocol-support)). Desktop bindings
   therefore do not reach Hyprland.
2. The guide button itself cannot be rebound — Steam reserves it. Only chords are
   available.

A partial workaround exists: **`extest`**, an `LD_PRELOAD` shim that reimplements
the `XTEST` extension and converts calls into a `uinput` device, which *does*
reach the compositor. Community reports on it are genuinely mixed. It is a
plausible zero-code experiment (see [05, Architecture A](05-architectures.md#a--steam-input-plus-an-xtest-shim-zero-custom-code))
and a poor foundation.

Worth keeping regardless: Steam Input remains the mechanism for mapping games,
and nothing here should disturb it.

## gamescope

Valve's micro-compositor. Runs nested inside an existing session with its own
embedded XWayland.

**What it solves.** Inside gamescope, `XTEST` works, and Steam's focus tracking
behaves. Big Picture as a gamescope session sidesteps both the XTEST problem and
the overlay focus-stealing problem in one move. It also provides HDR, VRR and
FPS caps that Hyprland does not expose to the game.

**What it does not solve.** It is a container, not a desktop input layer. It says
nothing about driving Hyprland itself. Several projects exist to switch between a
Hyprland desktop session and a gamescope game session.

**Verdict.** Complementary, not competing. The natural "game mode" half of a
two-mode setup.

## evdev remappers: evsieve, evremap, input-remapper, xremap

A family of tools that `EVIOCGRAB` a device, transform events, and re-emit
through `uinput`.

`evsieve` is the most composable — a small language of maps and filters over
evdev streams. `evremap` and `input-remapper` are keyboard-centric.
`input-remapper-rs` is a Rust rewrite.

**Relevant property.** They demonstrate the standard suppression pattern:
grab, transform, re-emit. Without the grab, both original and transformed events
reach applications.

**Why not reuse.** They operate on evdev, and the Steam Controller has no gamepad
evdev node on this kernel at all — only lizard-mode keyboard/mouse. Even after
`hid-steam` lands in 7.3, they cannot express "hold guide, flick stick, change
workspace"; they are stateless-ish remappers, not gesture engines with a
compositor IPC back end. For a *generic* pad they are a reasonable prototyping
shortcut.

## sc-controller

Userspace driver and profile system for the original Steam Controller, with an
OSD and a GUI. Long-standing, and the closest thing to "what this project wants"
for gen-1 hardware.

**Why not reuse.** It targets gen-1 (`1102`/`1142`), predates Wayland-first
desktops, and its output path is X11-oriented. The 2026 controller would need new
protocol support written into it. Its *profile model* — per-application profiles,
modeshift, gesture bindings — is a good reference for what a mature configuration
schema looks like.

## Kernel `hid-steam`

Not a competitor; a dependency.

Linux 7.3 merges initial 2026 Steam Controller support, bringing it to gen-1
parity — the pad works before any userspace library is involved, so `evdev`
consumers (Wine, native games, emulators) see a real gamepad without Steam
running. Haptics and finer touch reporting are expected in later cycles.

This matters for two reasons. It gives a normalized gamepad `evdev` node, which
makes the generic code path in [05](05-architectures.md) work for the Steam
Controller too. And it removes the current situation where the device is either
in lizard mode or wholly owned by Steam.

Note the tension: Valve's Pierre-Loup Griffais has expressed a preference *not*
to ship a kernel driver, on stability grounds, with the gap closed via SDL
instead. The community driver landed anyway. Do not assume the kernel path is the
one Valve will support long-term.

## SDL3

SDL3 added 2026 Steam Controller support, now enabled by default, covering USB,
the wireless dongle, and dongle pairing. Further work covers trackpads,
capacitive stick touch and grip sense.

**Relevance.** SDL3 is the cleanest existing decoder of this device's HID
protocol. If the passive-tap daemon needs more of the report format than
[03](03-hardware-findings.md) establishes, SDL's `SDL_hidapi_steam*` sources are
the reference implementation to read — and their existence means the decoding
work is bounded and already done once, publicly.

SDL is not usable *as* the daemon's input layer, though: SDL's gamepad API
normalizes away the guide button's chord semantics, and SDL wants to own the
event loop.

## Summary

| Project | Owns the device? | Reusable here? |
|---|---|---|
| InputPlumber | Yes | Only in the escalation path |
| Handheld Daemon | Yes | Pattern, not code |
| Steam Input | Yes | Must keep working; cannot be the desktop layer |
| gamescope | N/A (compositor) | Yes — as game mode |
| evsieve &c. | Yes (`EVIOCGRAB`) | Prototyping for generic pads |
| sc-controller | Yes | Reference for profile schema |
| `hid-steam` (7.3) | Kernel | Dependency / enabler |
| SDL3 | Yes | Reference for HID decoding |

Nothing in this list does what is wanted, and everything that comes close does it
by taking the controller away from Steam. That is the gap
[06 — Recommendation](06-recommendation.md) addresses.
