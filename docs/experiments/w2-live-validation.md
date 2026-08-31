# W2 — live end-to-end validation (2026-08-31)

The full daemon (`hyprpad run`) driven by hand against the real controller, with
Steam running and holding the same hidraw nodes. All output paths confirmed.

## Trackpad cursor (`zwlr_virtual_pointer_v1`)
Right-pad swipes moved the compositor cursor across the whole screen — 12
distinct sampled positions spanning `396,1081` … `1343,1023` during a 28 s run.
This is the affordance Steam cannot provide on Wayland (its XTEST cursor output
never escapes XWayland); hyprpad delivers it from a passive tap. Right-pad click
and full-R2 map to left button.

## Guide handoff (pad ↔ gesture coexistence)
While the guide button was held, the pad stopped driving the cursor and the
stick/bumpers drove the window manager instead — verified in one continuous run:

```
GuideEnter
GuideStickFlick{Right,Right} -> Workspace(+1)   (compositor: ws 7->10)
GuideStickFlick{Right,Left}  -> Workspace(-1)   (ws 10->7)
GuideChord(BumperR1)         -> Workspace(+1)
GuideChord(BumperL1)         -> Workspace(-1)
GuideLeave{was_chorded:true}
```

Workspace events (10,7,10,7,…,2) confirm the compositor actually switched. The
two modes (ambient pad-cursor, guide-held gestures) hand off cleanly with no
cross-talk, and a game focus would suppress the ambient layer via the arbiter.

## Verdict
The project thesis is running code: one controller drives the mouse cursor,
window management, and workspaces simultaneously, while Steam holds the same
physical device undisturbed. W2 is feature-complete (remaining: full-Unicode
keyboard into native Wayland apps — the uinput path is ASCII/XWayland-correct,
the Unicode/native path is deferred to the W10 OSK).

## Lizard-mode ownership (2026-08-31)

The W12 follow-on — hyprpad disabling the puck's firmware kbd/mouse itself, so a
masked Steam's inability to do so doesn't leave lizard fighting the daemon.
Verified live with Steam closed (so firmware lizard mode was ON):

| Step | Lizard evdev (event20/21) |
|---|---|
| Lizard on (Steam closed), pad moved | **2274 events** (REL_X/REL_Y mouse motion) |
| After `HYPRPAD_OWN_LIZARD=1` disable, pad moved | **0 events** |
| Raw `0x42` stream, same moment | **alive** (pad bytes changing) — controller not asleep |

So `disable_lizard_mode()` (kernel-faithful `CLEAR_DIGITAL_MAPPINGS` +
`SET_SETTINGS_VALUES` lizard=0/watchdog=0) genuinely silences the firmware
keyboard/mouse, and it **persists after the daemon exits** because the
revert-watchdog is also disabled. The write succeeds on an awake device
(the agent's earlier EPIPE was purely the sleeping-controller case). This is
the missing piece for the masked-Steam design: hyprpad owns lizard mode.

## Dual-trackpad typing — end-to-end, confirmed by real use (2026-08-31)

**The owner typed a full message to me using the OSK**, dual-trackpad, into a
focused terminal. That exercises the entire stack live: passive hidraw tap →
`GestureEngine` (Guide+Y chord) → `Action::ToggleKeyboard` → `OskHandle` spawns
and shows `hyprpad-osk` → both trackpads drive the two on-screen cursors
(`cursor L|R`) → pad click-down → `commit L|R` → the OSK's uinput keyboard →
characters into the focused app. Steam was closed (no guide-chord collision),
lizard-mode ownership on (firmware kbd/mouse disabled so the pads only drive the
OSK), OSK rendered with the new square-key `hyprpad-dark` theme (72px keys,
size-to-content).

This is the dogfooding milestone: the controller drives Hyprland AND types,
usably, from a passive tap — the whole project thesis, in a real user's hands.
Remaining polish: cursor damping (jitter while held steady — One Euro Filter,
in progress per docs/research/pointer-damping.md).
