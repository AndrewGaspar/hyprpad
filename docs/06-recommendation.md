# 06 — Recommendation

**Build architecture B — a passive-tap daemon into Hyprland IPC — and keep
architecture D as an explicit escalation hatch.** Adopt F (gamescope) as the
game-mode half. Try A once, cheaply, before writing anything.

## Why B

One property decides it: **`hidraw` is not exclusive**, verified on the target
hardware ([03](03-hardware-findings.md#the-central-finding-concurrent-hidraw-access)).
Everything else follows.

- It is the only design that satisfies constraint 4 — *Steam Input is not
  sacrificed* — trivially rather than by fighting for it. Steam is not merely
  tolerated; it is untouched.
- Its failure mode is degradation to the status quo. Kill the daemon and the
  controller behaves exactly as it does today. Every device-owning design fails
  by making the controller disappear.
- It needs no root, no udev rules, no kernel module, and no privileged helper.
- The status quo it must beat is *broken*
  ([03](03-hardware-findings.md#steams-handling-of-this-controller-is-broken-today)),
  so even a partial result is a net gain.

## The guide button is not actually a problem

The design's one real worry was that a passive tap cannot suppress, so Steam
would also react to the guide button. Direct observation has largely dissolved
it ([03](03-hardware-findings.md#steams-reaction-to-the-guide-button)):

> **Steam acts on guide _release_, not on press — and ignores holds longer than
> ~3 s entirely.**

This is close to the best case. It means:

- **No race.** The daemon has the whole hold to recognise and dispatch a gesture.
  Steam's reaction is a trailing side effect, not a competing consumer of the
  same event.
- **The side effect is only a focus steal** (Steam takes focus if it did not have
  it; launches Steam if it did).
- **Long holds are free.** Anything past ~3 s is invisible to Steam.

Three composable mitigations, cheapest first:

1. **Window rule.** `windowrule = suppressevent activatefocus, match:class steam`
   — config-only, blocks the focus steal outright. The `activatefocus` token is
   present in Hyprland 0.56.2. Note this build uses the non-legacy parser, so it
   must go in the config file; `hyprctl keyword` will not take it.
2. **Focus restore over IPC.** The daemon notes the focused window when a gesture
   starts and restores it a beat after guide release. Entirely within the passive
   design, no configuration required, and it also covers the "Steam was already
   focused" branch.
3. **Hold past 3 s** for gestures where even a flash is unacceptable — a
   guaranteed-silent escape, though too slow for a snappy workspace flick.

**Consequence for the plan: architecture D is demoted.** It was carried as an
escalation hatch on the assumption that reassigning the guide button might
eventually require owning the device. That now looks unlikely to be necessary.
D remains documented in [05](05-architectures.md#d--uhid-device-proxy-full-interposition)
as a fallback, but it should be treated as a design of last resort rather than a
planned phase.

One thing still unmeasured: whether a guide **chord** (guide plus another button)
already suppresses Steam's release action on its own. Steam has a Guide Button
Chord Layout, so it plainly has a concept of chords; if pressing any button
during the hold cancels the release behaviour, mitigation 1 may be unnecessary
for chorded gestures. See [07, Q1b](07-open-questions.md#q1b--does-a-guide-chord-suppress-steams-release-action).

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
