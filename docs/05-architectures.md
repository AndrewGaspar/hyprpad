# 05 — Candidate architectures

Six designs, from "no code at all" to "reimplement the device". Each is judged on
the five constraints from [01](01-problem.md#constraints-adopted), with the
trade-offs stated plainly rather than sold.

Throughout, **tap** means the daemon reads a device node that someone else may
also be reading, and **grab** means it takes the device so that nobody else can.

---

## A — Steam Input plus an XTEST shim (zero custom code)

Configure Steam's *Desktop Layout* and *Guide Button Chord Layout* to emit
keyboard shortcuts, bind those same shortcuts in `hyprland.conf`, and use
`extest` (`LD_PRELOAD`) to redirect Steam's `XTEST` calls into a `uinput` device
so they reach the compositor.

```
Steam Input  ──XTEST──▶  extest shim  ──uinput──▶  libinput  ──▶  Hyprland keybinds
```

**For.** No code. Steam arbitrates game-versus-desktop for free, and does it
correctly, because it already knows which app is running. Works with every
controller Steam supports, immediately. Per-game bindings come free.

**Against.** `extest` is an `LD_PRELOAD` shim reimplementing an X11 extension;
community reports on whether it works are genuinely mixed, and it is a
load-bearing hack. The guide button itself cannot be rebound — only chords are
available, so *tap guide* will always open Big Picture. Bindings are limited to
what a keystroke can express: no continuous analog control, so "flick the stick
to scrub through workspaces" degrades to discrete keypresses. Steam must be
running for any of it to work. And on this machine Steam's handling of this
particular controller is already failing
([03](03-hardware-findings.md#steams-handling-of-this-controller-is-broken-today)).

**Verdict.** Worth an afternoon as an experiment, because if it works it is free.
Not a foundation. Its ceiling is low and its failure mode is opaque.

---

## B — Passive tap into compositor IPC  ⭐ recommended core

A userspace daemon opens the controller **read-only**, alongside Steam, and
drives the desktop through Hyprland's own interfaces.

```
                 ┌──────────────────────────────────────┐
  /dev/hidraw7 ──┤ (multi-reader — no grab, no conflict)│
                 └──────┬──────────────────────┬────────┘
                        │                      │
                        ▼                      ▼
                 ┌────────────┐         ┌─────────────┐
                 │  hyprsc    │         │    Steam    │  unaffected
                 └─────┬──────┘         └─────────────┘
                       │
        ┌──────────────┼───────────────────┐
        ▼              ▼                   ▼
  Hyprland IPC   zwlr_virtual_pointer  zwp_virtual_keyboard
  (.socket.sock)  (cursor, scroll)      (keystrokes)
```

Input sources, in order of preference per device:

- **2026 Steam Controller** — `hidraw`, report `0x42`, decoded per
  [03](03-hardware-findings.md#report-0x42--the-main-input-report). Gives the
  guide button, trackpads, gyro and grip sense.
- **Anything else** — `evdev`, read without `EVIOCGRAB`. `BTN_MODE` is the guide
  button on essentially every XInput-class pad. Loses trackpads and gyro, keeps
  everything else.
- **After kernel 7.3**, the Steam Controller also gets a normalized gamepad
  `evdev` node, so it can fall back to the generic path.

Output paths, chosen by what is being expressed:

- **Window-manager intent** → Hyprland IPC `dispatch` (`workspace`,
  `movewindow`, `fullscreen`, `exec`). Direct and reliable; no synthetic
  keystroke that a keybind must then re-interpret.
- **Cursor and scroll** → `zwlr_virtual_pointer_v1`. Confirmed supported.
  No root, no `/dev/uinput`, no XWayland.
- **Keystrokes for applications** → `zwp_virtual_keyboard_v1`.

Mode arbitration reads Hyprland's `.socket2.sock` event stream (`activewindow`,
`fullscreen`, `openwindow`) and Steam's process tree, and suppresses desktop
gestures while a game is focused.

**For.** Steam is completely undisturbed — Steam Input, per-game configurations
and Big Picture keep working exactly as today, which is the single most important
constraint. No root, no kernel work, no udev changes. Nothing breaks if the
daemon crashes; the controller simply behaves as it does now. Full analog
resolution at 263 Hz, so continuous gestures are expressible. Generalizes to
other controllers through the evdev path.

**Against.** A passive tap cannot suppress, so Steam sees the guide button too.
This was the design's main worry, and direct observation has largely dissolved
it: **Steam acts on guide _release_, not press**, and ignores holds longer than
~3 s entirely ([03](03-hardware-findings.md#steams-reaction-to-the-guide-button)).

That means there is no race — the daemon acts during the hold, Steam acts after
it, and the only leftover is a trailing focus steal. Three ways to remove it,
which compose:

1. **A Hyprland window rule.** `suppressevent activatefocus, match:class steam`
   blocks the focus steal declaratively. Config-only; token verified present in
   0.56.2.
2. **Focus restore over IPC.** The daemon records the focused window when a
   gesture begins and restores it shortly after guide release. Stays entirely
   within the passive design, needs no configuration.
3. **Hold past 3 s** for any gesture where even a flash is unacceptable.

Also against: device-specific HID decoding for the Steam Controller (bounded —
SDL3 has already done it publicly), and the daemon must implement its own
game-detection heuristics rather than inheriting Steam's.

---

## C — Exclusive evdev grab and re-emit

The classic remapper shape: `EVIOCGRAB` the gamepad's evdev node, filter guide
chords out, re-emit the remainder through `uinput`.

```
/dev/input/eventN ──EVIOCGRAB──▶ hyprsc ──uinput──▶ virtual pad ──▶ Steam, games
                                    │
                                    └──▶ Hyprland IPC
```

**For.** Real suppression: guide chords never reach anything downstream. Well-trodden
— `evsieve`, `evremap` and `input-remapper` all work this way. Device-agnostic;
no HID reverse engineering.

**Against, decisively, for the Steam Controller.** Steam reads Valve hardware
over `hidraw`, not `evdev`. Grabbing the evdev node does not hide anything from
Steam; it still sees the raw device and still reacts to the guide button. The
grab buys nothing against the actual competitor. Worse, on this kernel the Steam
Controller has no gamepad evdev node at all — only lizard-mode keyboard and
mouse.

Downstream, Steam sees a generic `uinput` pad instead of a Steam Controller, so
gyro, trackpads and Steam Controller-specific configurations are lost.

**Verdict.** Viable for *generic third-party pads*, where Steam does use evdev.
Not viable for the primary target. Its niche is narrow enough that the evdev tap
in B covers it better.

---

## D — `uhid` device proxy (full interposition)

Take the device away from Steam, and hand Steam a synthetic replacement.

```
udev: strip uaccess from 28de:1304  ──▶  only hyprsc can open the real device
                    │
                    ▼
     ┌────────────────────────────────┐
     │  hyprsc                        │
     │   decode 0x42                  │
     │   consume guide chords         │
     │   re-encode remainder ─────────┼──▶ uhid virtual Steam Controller
     └────────────┬───────────────────┘         │
                  ▼                             ▼
           Hyprland IPC                       Steam
```

Because the replacement is a `uhid` device presenting the Steam Controller's own
report descriptor, Steam can be made to treat it as real hardware — gyro,
trackpads, Steam Input configurations intact. This is exactly what InputPlumber
does with its synthetic Steam Deck target.

**For.** Total control. The guide button becomes genuinely reassignable: Steam
never sees the chord, so Big Picture never opens. This is the only design that
fully delivers the stated goal.

**Against.** Substantially the largest effort. It requires reimplementing enough
of the vendor protocol to be convincing — not just input reports but feature
reports: lizard-mode control, haptics, LED, pairing, battery. It is fragile
against firmware and Steam client updates. It needs system-level privileges and
udev rules, so a crash takes the controller down with it rather than degrading
to today's behaviour.

And there is a specific risk here worth naming: Steam's registration path for
this controller **already fails** on real hardware
([03](03-hardware-findings.md#steams-handling-of-this-controller-is-broken-today)).
A synthetic clone has to survive a code path that genuine hardware does not
currently survive. That is not a good bet to build a foundation on today.

**Verdict.** The right eventual answer for the guide button specifically, and the
right *escalation hatch* — but not the place to start, and not until Steam's own
handling of the device stabilizes.

---

## E — Delegate to InputPlumber

Write a 2026 Steam Controller capability map for InputPlumber, let it own the
device and emit a target, and use its DBus **intercept mode** to route guide
chords to a small hyprsc client that talks to Hyprland.

**For.** Reuses a maintained, packaged daemon that is already part of SteamOS.
Device compositing, target emulation and intercept are solved problems there.
Someone else maintains the hard parts.

**Against.** No 2026 controller profile exists, so the decoding work in
[03](03-hardware-findings.md) still has to happen — the reuse is of plumbing, not
of knowledge. Structurally it is architecture D with a third party in the middle:
InputPlumber owns the device, so all of D's costs apply, plus a heavyweight
dependency and less direct control. Its DBus surface has had authorization
weaknesses ([CVE-2025-66005 / CVE-2025-14338](https://security.opensuse.org/2026/01/09/inputplumber-lack-of-dbus-auth.html)).

**Verdict.** If the escalation to full interposition is ever taken, prefer this
over hand-rolling D — contributing a capability map upstream is better than
maintaining a private `uhid` clone. Not a starting point.

---

## F — Two-session split: gamescope for game mode

Not an input architecture; a scoping decision. Run games and Big Picture inside
gamescope, which has its own embedded XWayland where `XTEST` works and Steam's
focus tracking behaves. Use the desktop controller layer only on the Hyprland
desktop.

**For.** Sidesteps both halves of the Steam-on-Wayland problem at once — the
XTEST breakage and the Big Picture overlay stealing input regardless of
compositor focus. Brings HDR, VRR and FPS caps that Hyprland does not expose.
Makes "is a game running?" trivial: it is a different session.

**Against.** Gives up desktop-wide trackpad-as-mouse — but per
[03](03-hardware-findings.md) that is currently broken anyway. Session switching
is a UX seam. Does nothing on its own for driving Hyprland.

**Verdict.** Complementary. Adopt alongside B as the game-mode half.

---

## Comparison

| | A: Steam+extest | **B: passive tap** | C: evdev grab | D: uhid proxy | E: InputPlumber | F: gamescope |
|---|---|---|---|---|---|---|
| Custom code | none | moderate | moderate | large | moderate | none |
| Steam Input preserved | yes | **yes** | degraded | if clone convinces | if clone convinces | yes |
| Guide fully reassignable | no | **no** — Steam also reacts | not vs. Steam | **yes** | **yes** | n/a |
| Analog / continuous gestures | no | **yes** | yes | yes | yes | n/a |
| Needs root / udev | no | **no** | uinput only | yes | yes | no |
| Failure mode | silent | **degrades to today** | pad disappears | pad disappears | pad disappears | n/a |
| Works without Steam | no | **yes** | yes | yes | yes | no |
| Generic controllers | yes | **yes** (evdev) | yes | per-device | per-device | yes |

The row that decides it is **failure mode**. B is the only design whose worst
case is "the daemon stopped and the controller behaves exactly as it does today".
Every design that owns the device fails by taking the controller away.

The row that constrains it is **guide fully reassignable**, where B is honestly
weak — which is why [06](06-recommendation.md) keeps D as an explicit hatch
rather than pretending the problem is solved.
