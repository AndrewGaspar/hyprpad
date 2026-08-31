# 12 — The lizard-free goal: hyprpad as the sole driver

**Owner's goal (2026-08-31):** never boot into (or fall back to) the
controller's firmware "lizard mode." hyprpad should be the *only* thing that
ever drives the controller, so the user never experiences the confusing
dual-mode UX where the pad behaves one way (firmware mouse/keyboard) before
hyprpad takes over and another way after.

## The tension to design around

This is the deliberate opposite of the resilience safety net just built
([experiments/w2-live-validation.md] / `feat-resilience`): today hyprpad
**re-enables** lizard mode on exit precisely so a stopped/crashed daemon never
leaves the controller inert. The lizard-free goal wants that fallback gone.

The trade is real: **with no lizard fallback, whenever hyprpad is not running
the controller does nothing at all** — no pad, no keys. That is only acceptable
once hyprpad is trustworthy enough to be the sole driver, and/or the user always
has other input (keyboard/mouse) to recover. So this is a *graduation* step, not
a default.

Also note a hard constraint: **the firmware powers up in lizard mode and that
default cannot be changed** (it lives in the controller, not the host). So there
is always some window between "controller connects" and "hyprpad disables
lizard." The goal is to make that window *imperceptible*, not literally zero.

## Phased path

1. **Config knob** `restore_lizard_on_exit` (default **true** for safety). A user
   committed to hyprpad-always sets it **false** — hyprpad then never re-enables
   lizard, even on exit. Low-effort; the exit-restore code already exists to gate.
2. **Start hyprpad early and reliably** — the `hyprpad.service` systemd user unit
   (`WantedBy=graphical-session.target`, `Restart=always`) from
   [10-distribution.md], so it's up before the user reaches for the controller
   and respawns instantly if it dies. This shrinks the boot-time lizard window
   and removes the "daemon stopped" inert-controller risk.
3. **Near-zero on-connect latency** — today the reconnect loop re-disables lizard
   within one `RECONNECT_SCAN_INTERVAL` (1.5 s) of the controller reappearing,
   so a fresh connect flashes ~1.5 s of lizard. Tighten with a **udev hook** on
   the `28de:1304` add event that nudges the running hyprpad to disable lizard
   immediately (a signal or socket poke), instead of waiting for the poll. That
   makes the connect-time lizard window imperceptible.
4. **Graduation criterion** — flip `restore_lizard_on_exit = false` (and rely on
   the always-on service) only once hyprpad is daily-driver reliable. Until then,
   keep the safety net.

## Why not just disable lizard's *default* in firmware

Can't — Valve's firmware owns the power-up default and there is no host-side way
to change it (unlike the runtime disable we already do via feature reports). The
udev-nudge approach (step 3) is the closest we get to "never see lizard."

Status: roadmap. The `restore_lizard_on_exit` knob (step 1) is the cheap first
piece; the systemd unit (step 2) comes with packaging (W-install); the udev nudge
(step 3) is the polish that makes it feel seamless.
