# 07 — Open questions

Ordered by how much the answer changes the plan.

## Q1 — Does Steam open Big Picture on guide *press* or on guide *release*?

**Why it matters most.** It determines whether architecture B's hold-based chords
can coexist with Steam, or whether every gesture flashes Big Picture. If Steam
acts on release-after-short-press, a >300 ms hold never triggers it and the main
weakness of the recommended design largely evaporates. If Steam acts on press,
escalation to architecture D moves much closer.

**How to answer.** No code needed. Hold the guide button for three seconds and
watch when Big Picture appears — immediately on press, or only on release. Then
repeat while watching `~/.steam/steam/logs/controller.txt`.

## Q2 — The rest of the `0x42` button map

[03](03-hardware-findings.md#confirmed-button-bits) confirms the Steam button
(`b4` bit 0) and Quick Access (`b2` bit 4) unambiguously, and has high confidence
on A/B/X/Y. The D-pad direction-to-bit mapping, the individual grip buttons,
Start/Select and the bumper assignments are **not** pinned down — the guided
capture had two overlapping passes and an on-screen keyboard that ate some
presses.

**How to answer.** `tools/sc2-capture.py capture`, one control at a time, three
isolated presses each — the method that produced the two confirmed rows. Roughly
fifteen minutes of button pressing. Cross-check against SDL3's
`SDL_hidapi_steam*` sources, which already decode this device.

## Q3 — Axis encoding in `b30`–`b45`

Sticks, trackpads and capacitive touch all live in this range, and it was only
established that they *move independently* during a guide hold, not how they are
encoded. Endianness, signedness, resolution, and which pair belongs to which
control are all open. Probably `int16` little-endian pairs, unverified.

**How to answer.** Capture single-axis motion — push the right stick fully right,
release, fully left — and correlate. SDL3 again is the reference.

## Q4 — Does writing to `hidraw` while Steam holds it disturb Steam?

Reading concurrently is proven safe. **Writing** — feature reports for haptics,
LED, or lizard-mode control — was deliberately not tested, because it could
confuse a client that believes it owns the device state.

**Why it matters.** Determines whether the daemon can give haptic feedback on
gesture recognition, which is a significant UX difference for an eyes-off couch
interface.

**How to answer.** Carefully, with Steam running and a game open, watching
`controller.txt`. Low priority; the feature is a nicety.

## Q5 — Does `hid-steam` on kernel 7.3 change the `hidraw` picture?

Once a real driver binds `28de:1304` instead of `hid-generic`, the device gains a
gamepad `evdev` node. Unknown: whether the `hidraw` node remains available for
concurrent reading with the same report format, and whether Steam continues to
prefer `hidraw` over the new `evdev` node.

**Why it matters.** If `hid-steam` changes what `hidraw` exposes, the decoding
in [03](03-hardware-findings.md) may need revisiting. If Steam switches to
`evdev`, the whole arbitration picture changes — possibly for the better, since
`EVIOCGRAB` would then work against Steam.

**How to answer.** Install `linux-mainline`, re-run the probes in
[03](03-hardware-findings.md), and re-check which nodes Steam holds.

## Q6 — Will Steam's registration bug be fixed, and does it matter?

[ValveSoftware/steam-for-linux#13185](https://github.com/ValveSoftware/steam-for-linux/issues/13185)
is open. The failure is reproducible here on every launch.

**Why it matters.** Architecture D depends on a synthetic clone surviving a
registration path that genuine hardware currently does not survive. If the bug is
fixed, D becomes more plausible. If Valve instead reworks how the client claims
this device, the passive tap's assumptions should be re-verified.

## Q7 — Game detection: which signal is authoritative?

`~/.steam/registry.vdf` on this machine does not contain a `RunningAppID` key —
that is a Windows-registry concept without a direct Linux equivalent. Candidate
signals: the `reaper SteamLaunch` process in the tree, Hyprland's `activewindow`
and `fullscreen` events, or simply "a gamescope session is focused".

**How to answer.** Launch a game and observe all three. Low risk; the gamescope
route (architecture F) makes this nearly moot.

## Q8 — Multi-controller behaviour

The puck exposes four controller slots. Only slot 1 was populated during testing.
Unknown whether slot assignment is stable across reconnects, and how the daemon
should behave with two controllers paired.

## Q9 — Bluetooth versus the puck

All testing used the 2.4 GHz puck. Whether the controller presents the same
report format and the same concurrent-access properties over Bluetooth is
untested.

## Q10 — Latency budget

The input side is measured: 263 Hz, 4.00 ms median gap. Unmeasured: the
end-to-end cost of Hyprland IPC dispatch and of `zwlr_virtual_pointer_v1`
injection. Success criterion 1 in [06](06-recommendation.md) asserts a 100 ms
budget without having verified it is achievable.
