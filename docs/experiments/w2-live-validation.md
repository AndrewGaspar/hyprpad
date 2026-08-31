# W2 — live end-to-end validation (2026-08-31)

The full daemon (`hyprsc run`) driven by hand against the real controller, with
Steam running and holding the same hidraw nodes. All output paths confirmed.

## Trackpad cursor (`zwlr_virtual_pointer_v1`)
Right-pad swipes moved the compositor cursor across the whole screen — 12
distinct sampled positions spanning `396,1081` … `1343,1023` during a 28 s run.
This is the affordance Steam cannot provide on Wayland (its XTEST cursor output
never escapes XWayland); hyprsc delivers it from a passive tap. Right-pad click
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
