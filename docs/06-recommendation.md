# 06 — Recommendation

> **Revised 2026-08-30**, after a requirement was added: *the controller must be
> focus-routed like any other input device — Steam should receive controller
> input only when a Steam window or game is focused.*
>
> That requirement changes the answer. The earlier version of this document
> recommended the passive tap (B) and demoted the `uhid` proxy (D) to a last
> resort. **Passive tapping cannot satisfy focus routing**, so D is re-promoted
> to the structural answer. The reasoning for the original call is preserved
> below under [Why the passive tap is still the right first
> step](#why-the-passive-tap-is-still-the-right-first-step), because it remains
> correct for everything except suppression.

## The constraint that decides it

`hidraw` has no suppression mechanism
([02](02-background-linux-input.md#exclusivity-the-property-the-whole-design-hinges-on)).
A passive reader sees everything and can prevent nothing. So long as Steam can
*open* the device, Steam receives every report and decides for itself what to do
with it — including while unfocused, which is exactly the complaint.

"Steam behaves like any other window" therefore requires that **Steam not have
the device**. There is no configuration of a read-only design that achieves it.

## Tier 0 — make Steam inert without taking the device

Cheap, reversible, config-only, and worth doing first because it addresses the
*observable* symptom in an afternoon. It does not achieve focus routing; it
achieves "Steam does nothing when it isn't wanted", which may be close enough in
practice.

1. **Empty the Desktop Layout — this is the main offender.** The 2026 controller
   has no `desktop_triton.vdf` and falls back to the Steam Deck's
   `desktop_neptune.vdf`, which binds *bare* presses system-wide: X →
   `SHOW_KEYBOARD`, A/B/Y → Return/Escape/Space, dpad → arrows, bumpers →
   Ctrl/Alt, a grip → `LEFT_WINDOWS`, trackpad click → mouse buttons
   ([03](03-hardware-findings.md#steam-treats-this-controller-as-controller_triton)).
   No modifier is involved. Unbinding this removes most of the problem.
2. **Empty the Guide Button Chord Layout.** `chord_triton.vdf` adds guide-held
   chords on top: `quit_application`, `SCREENSHOT`, `controller_poweroff`,
   `toggle_magnifier`, game recording, volume, Alt-Tab.
3. **Add the window rule.**
   `windowrule = suppressevent activatefocus, match:class steam` — stops the
   focus steal on guide release.

With all three, Steam still holds the device and still exits lizard mode, but its
out-of-focus behaviour is empty.

Note that only some of these bindings currently reach the desktop anyway:
`key_press` and `mouse_button` go through `XTEST` and are trapped in XWayland,
while `controller_action` (the on-screen keyboard) and `xinput_button` (Steam's
virtual pad) do land
([03](03-hardware-findings.md#not-all-of-these-reach-a-hyprland-desktop)). Tier 0
is therefore cheaper *and* less complete than it looks — but the bindings it
removes are precisely the ones that currently get through. hyprsc passive-taps and owns the desktop.

**Limits, stated plainly.** This is behavioural, not structural. Steam still owns
the device, so a client update or a config resync can reintroduce behaviour
without warning. Emptying the Desktop Layout also means the controller does
nothing *in* the Steam client window when it is focused, which is a regression
unless hyprsc covers that case. And it is not focus routing — it is
globally-nothing, which happens to look the same from the desktop.

## Tier 1 — the structural answer

hyprsc owns the physical device; Steam receives a virtual controller that hyprsc
feeds **only when a Steam window or Steam game holds focus**.

```
  physical SC2 (28de:1304)
        │  exclusively owned — Steam cannot open it
        ▼
  ┌──────────────────────────────────────────────┐
  │  hyprsc (privileged)                         │
  │    decode 0x42                               │
  │    ── desktop focused ─▶ Hyprland IPC        │
  │                          virtual pointer     │
  │    ── Steam/game focused ─▶ forward ──┐      │
  └───────────────────────────────────────┼──────┘
                                          ▼
                              virtual controller (uinput or uhid)
                                          │
                                          ▼
                                    Steam / Steam Input
```

### Denying Steam the device

Three candidate mechanisms, none yet verified on this machine:

- **udev override.** A rule ordered after Steam's `60-steam-input.rules` (which
  grants `MODE="0660", TAG+="uaccess"` to every `28de` hidraw by vendor ID)
  restricting the puck to root or a dedicated group. Note `uaccess` is applied by
  logind from the tag, and the udev man page documents `TAG` as a match key and
  `TAG+=` as an assignment — **it does not document `TAG-=`**, so clearing an
  inherited tag needs testing rather than assuming. This implies hyprsc runs as a
  system daemon, since any permission that lets a user-session hyprsc open the
  node also lets user-session Steam open it.
- **Mount namespace.** Launch Steam under `bwrap` with the puck's nodes masked.
  Avoids root, but `hidraw` numbering is dynamic and changes on replug, so the
  wrapper must resolve nodes at launch and cannot survive a mid-session replug.
- **systemd device cgroup.** `DevicePolicy=` / `DeviceAllow=` on Steam's user
  unit — Steam is launched here via `uwsm-app`, so it already has one. Clean if
  it works, but the cgroup v2 device controller generally requires delegation and
  may not be usable from a user unit. Unverified.

### What to hand Steam

| Target | Steam sees | Keeps trackpads + gyro | Effort |
|---|---|---|---|
| `uinput` generic pad (Xbox/DS4) | a normal gamepad | **no** | low |
| `uhid` Steam Deck (`neptune`) clone | a Deck controller | yes | medium — InputPlumber already does this |
| `uhid` SC2 (`triton`) clone | a Steam Controller | yes | high — and see the risk below |

The generic `uinput` pad is enough for the stated need — Steam Input mapping
modern pads onto older games works fine on a generic controller. It costs the
trackpads and gyro *as Steam Input inputs*, which matters for games that use
them.

The `triton` clone is the most faithful and the worst bet today: Steam's
registration path for this controller **already fails on genuine hardware**
([03](03-hardware-findings.md#steams-handling-of-this-controller-is-broken-today)).
A clone would have to survive a code path real hardware does not.

### The upside nobody should overlook

Once hyprsc owns the trackpads, it drives `zwlr_virtual_pointer_v1` directly —
which produces a **working desktop cursor**, something Steam cannot currently
deliver on Hyprland at all because its `XTEST` output never escapes XWayland
([03](03-hardware-findings.md#portal-and-protocol-support)). Tier 1 does not just
satisfy the focus-routing requirement; it fixes the trackpad.

### Prefer InputPlumber if it can be made to fit

InputPlumber already solves device ownership as a root daemon, ships a
`deck-uhid` target, and has DBus intercept mode. hyprsc would shrink to a DBus
client plus a Hyprland IPC bridge.

**Unverified and important:** whether InputPlumber can prevent Steam from opening
a `hidraw` source it manages. Its evdev sources are protected by `EVIOCGRAB`,
which has no `hidraw` equivalent, and research did not confirm a hidraw-hiding
mechanism. If it cannot, InputPlumber needs the same udev work as a hand-rolled
daemon and the reuse argument weakens considerably. Also note
[CVE-2025-66005 / CVE-2025-14338](https://security.opensuse.org/2026/01/09/inputplumber-lack-of-dbus-auth.html).

## Why the passive tap is still the right first step

Nothing in the original analysis was wrong except its scope. The passive tap
remains the correct mechanism for *reading* the controller, and Tier 1 is the
passive tap plus exclusive ownership plus a gated output. Building B first is not
wasted work — the decoder, the mode machine, the gesture engine and the Hyprland
IPC layer are all identical. Tier 1 adds ownership and a virtual output; it does
not replace anything.

Sequencing therefore stands: get Tier 0 in place today, build the passive daemon,
then add ownership once the gesture layer is proven.

## Phasing

### Phase 0 — cheap experiments, no code

- ~~Test whether Steam acts on guide press or release.~~ **Answered:** on
  release, and not at all past ~3 s. See above.
- Add `windowrule = suppressevent activatefocus, match:class steam` and confirm
  it removes the focus steal. One config line, immediately testable.
- Test whether a guide **chord** already suppresses Steam's release action
  ([07, Q1b](07-open-questions.md#q1b--does-a-guide-chord-suppress-steams-release-action)).
- Try architecture A (`extest` + Steam Input Desktop Layout). An afternoon. If it
  works well enough, the scope of everything below shrinks.
- Move to a kernel with `hid-steam` 2026 support (7.3, or `linux-mainline`). This
  removes the lizard-mode-or-nothing situation and yields a normalized gamepad
  `evdev` node, which the generic code path can then use for the Steam Controller
  as well.

### Phase 1 — the daemon, minimal

A single-binary daemon. Rust is the natural choice given the existing toolchain
and the precedent of InputPlumber, but nothing here demands it.

- `hidraw` reader for `28de:1304`, decoding report `0x42`; consult SDL3's
  `SDL_hidapi_steam*` sources for the fields not yet pinned down in
  [03](03-hardware-findings.md#confirmed-button-bits).
- A mode machine: idle → guide-held → gesture recognized → dispatch.
  Tap/hold/double-tap discrimination on the guide button.
- Output via Hyprland IPC (`$XDG_RUNTIME_DIR/hypr/$HIS/.socket.sock`) for
  window-manager intent.
- Declarative binding configuration — chord or gesture to action — rather than
  hard-coded behaviour.

Target the specific gesture that motivated the project: **hold guide, flick the
right stick, change workspace.** Verified as fully observable from the passive
stream — 1 226 consecutive frames with the guide bit set while stick axes moved
independently ([03](03-hardware-findings.md#the-full-gesture-is-observable)).

### Phase 2 — pointer and generic controllers

- `zwlr_virtual_pointer_v1` for trackpad-driven cursor and scroll; the protocol
  is confirmed present. This is the piece that fixes what Steam currently breaks.
- `zwp_virtual_keyboard_v1` for keystrokes aimed at applications.
- An `evdev` reader for generic pads, with `BTN_MODE` as the guide button, so
  a DualSense or an Xbox pad gets the same desktop layer minus trackpads and gyro.

### Phase 3 — arbitration

- Subscribe to Hyprland's `.socket2.sock` (`activewindow`, `fullscreen`,
  `openwindow`) and suppress desktop gestures while a game is focused.
- Detect running Steam games from the process tree (`reaper SteamLaunch`) rather
  than from Steam's config files.
- Adopt gamescope for game mode (F), which makes arbitration nearly free: a game
  session is a different session.

### Phase 4 — escalation, only if warranted

Only if the mitigations above prove insufficient in daily use — which now looks
unlikely — take architecture D — but prefer doing it as an
**InputPlumber capability map** (E) rather than a private `uhid` clone. Upstream
plumbing beats a bespoke device emulator that has to be maintained against
firmware and client updates.

## What not to do

- **Do not route desktop control through Steam Input's keyboard output.** It
  cannot escape XWayland on this system; there is no `RemoteDesktop` portal
  backend ([03](03-hardware-findings.md#portal-and-protocol-support)).
- **Do not `EVIOCGRAB` the controller expecting to hide it from Steam.** Steam
  reads Valve hardware over `hidraw`; the grab accomplishes nothing against it.
- **Do not write a kernel module.** `hid-steam` is upstream and actively
  developed; contribute there if the kernel is the problem.
- **Do not build on Steam's virtual X360 pad or its desktop emulation.** Both sit
  downstream of a registration path that currently fails on this hardware.

## Success criteria

1. Hold guide + flick right stick changes workspace, reliably, within 100 ms.
2. Steam Input configurations continue to work unchanged for every game.
3. Killing the daemon returns the system to exactly its current behaviour.
4. A generic Xbox or DualSense pad gets the same desktop layer, minus trackpads
   and gyro.
5. No desktop gesture fires while a game holds focus.
