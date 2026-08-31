# 07 — Open questions

Ordered by how much the answer changes the plan.

## Q1 — Does Steam act on guide press or release?  **ANSWERED**

**Answered 2026-08-30 by direct observation: on _release_.** Steam takes focus if
it did not have it, launches Steam if it did, and does **nothing at all** if the
button was held longer than ~3 s.

This was the highest-stakes question and the answer is close to the best case.
Recorded in [03](03-hardware-findings.md#steams-reaction-to-the-guide-button);
its consequences for the plan are in
[06](06-recommendation.md#the-guide-button-is-not-actually-a-problem).

## Q1b — Does a guide chord suppress Steam's release action?  **ANSWERED 2026-08-31**

Tested live with Steam running, a non-Steam window focused, three trials:

| Gesture | Steam's reaction on release |
|---|---|
| Guide + **A** tap (button chord) | **Focus steal fires anyway** |
| Guide + **right-stick flick** (analog chord) | **Nothing — suppressed** |
| Bare guide tap (control) | Focus steal (as established in Q1) |

So suppression is **analog-only**: stick (and presumably trackpad) motion
during the hold cancels Steam's release action — most plausibly because the
Guide Chord Layout binds guide+stick to mouse emulation, marking the hold as
"chord consumed" — while a button press during the hold does not.

**Consequences:** the flagship gesture (guide + stick flick → workspace) needs
*no* mitigation at all in the passive design. Guide + *button* chords still
need `suppressevent activatefocus` (and Tier-0 chord emptying to silence any
bound chord action). Recorded in
[03](03-hardware-findings.md#steams-reaction-to-the-guide-button).

## Q1c — Does `suppressevent activatefocus` fully block the focus steal?

The token is present in Hyprland 0.56.2 and the rule is the obvious fix, but it
has not been applied and tested. Note this build uses the non-legacy config
parser, so `hyprctl keyword windowrule` is rejected — the rule must go in the
config file. This machine's Hyprland config is Lua-generated, so the edit belongs
in `hyprland.lua`, not the generated `.conf`.

## Q2 — The rest of the `0x42` button map  **ANSWERED 2026-08-31**

Fully mapped via a combined hidraw+lizard-evdev capture with Steam closed —
see [03](03-hardware-findings.md#button-map--complete). Only the four
capacitive bits remain individually unassigned (they matter as a class).
Original text follows.

[03](03-hardware-findings.md) confirms the Steam button
(`b4` bit 0) and Quick Access (`b2` bit 4) unambiguously, and has high confidence
on A/B/X/Y. The D-pad direction-to-bit mapping, the individual grip buttons,
Start/Select and the bumper assignments are **not** pinned down — the guided
capture had two overlapping passes and an on-screen keyboard that ate some
presses.

**How to answer.** `tools/sc2-capture.py capture`, one control at a time, three
isolated presses each — the method that produced the two confirmed rows. Roughly
fifteen minutes of button pressing. Cross-check against SDL3's
`SDL_hidapi_steam*` sources, which already decode this device.

## Q3 — Axis encoding  **ANSWERED 2026-08-31**

Answered, with a correction: sticks/pads live at `b10`–`b29` (i16/u16 LE,
±32767 sticks, u16 triggers and pad force), and `b30`+ is the **IMU**, which
streams only when enabled by a feature report (Steam does this; frozen
otherwise). Full table in
[03](03-hardware-findings.md#layout--fully-decoded-2026-08-31-steam-closed-lizard-mode-cross-correlation).

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

## Q10 — Latency budget  **LARGELY ANSWERED**

Input side: 263–269 Hz, 4.00 ms median gap. Hyprland IPC round-trip measured
2026-08-31 at **0.01 ms median, 0.09 ms max** over 50 samples on the command
socket. The 100 ms gesture budget is therefore dominated by gesture-recognition
dwell time and compositor render, both well in hand. Remaining unmeasured:
`zwlr_virtual_pointer_v1` injection-to-render, which needs a live client test.

---

*The questions below were added 2026-08-31 after the scope expanded to the full
living-room programme ([08](08-living-room-vision.md)). They concern the target
tower, which has not yet been probed — all hardware findings above are from the
Framework 16 development machine.*

## Q11 — Tower probe

Everything in [03](03-hardware-findings.md) needs re-verification on the actual
living-room tower: GPU (NVIDIA assumed), monitor/TV EDID and HDR capabilities,
USB topology for the puck, BIOS wake-from-USB behaviour, boot chain (Limine +
UKI assumed to match the laptop since both run Omarchy).

## Q12 — Lizard mode in the initramfs  **HALF-ANSWERED**

*2026-08-31: the userspace half is confirmed on hardware — with Steam closed,
lizard mode is live and emits exactly the arrows/Enter/Esc/Tab + mouse
vocabulary the initramfs plan assumes
([03](03-hardware-findings.md#lizard-mode-output-map-steam-closed--the-initramfs-vocabulary)).
Remaining: confirm the same at an actual LUKS prompt (a reboot).*


The controller's lizard mode presents a standard HID boot keyboard
(dpad→arrows, A→Enter, B→Esc). If that interface enumerates during early boot,
the initramfs unlock UI can be driven with zero controller-specific code in the
initramfs. Needs: confirmation the puck's if02 keyboard is active pre-Steam at
cold boot, and that the `keyboard` mkinitcpio hook's usbhid coverage picks it up.
Test: at the current LUKS prompt, press dpad/A/B on the controller and observe.

## Q13 — Does the puck wake the machine from suspend?

Steam's udev rule sets `power/wakeup=enabled` for all `28de` USB devices. Whether
a button press actually wakes the tower from s2idle/S3 depends on the USB
controller's wake chain and BIOS settings. Test on the tower: suspend, press
guide.

## Q14 — Text-injection matrix validation  **STARTED 2026-08-31**

`wtype 'héllo'` into **Omawrite** (native Wayland, `xwayland=false`): rendered
exactly, é included — the virtual-keyboard-v1 path with the Unicode
keymap-swap works on this stack for the native-Wayland row. Remaining rows:
XWayland/Chromium (the worst-reported case — wtype#62 class) and nested
gamescope (`gamescope-type`).
