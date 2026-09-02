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

*(Everything above is the plan as written on 2026-08-31. The status section below
supersedes it: steps 1 and 2 are built, and step 3 was declined — the nudge
cannot poke a daemon that is already running, which is what step 2 guarantees.)*

## Status (2026-09-02) — steps 1 and 2 are built; step 3 is declined

**Lizard-free boot = `restore_lizard_on_exit = false` + the user unit.** Those
two together are the whole feature; step 3 turned out not to be worth building.

### Step 1 — the knob: DONE

```lua
h.daemon { own_lizard = true, restore_lizard_on_exit = false }
```

`[daemon] restore_lizard_on_exit` in TOML, `h.daemon { … }` in Lua, **default
`true`** — today's behaviour, the safety net. `false` makes every exit path leave
lizard mode disabled.

The knob reaches the exit paths through a cell in `src/lizard.rs`
(`set_restore_on_exit`), the same shape as the power settings and the IMU
preference, and for the same reason: the two exit paths are a signal-waiter
thread and a `Drop` guard, neither of which can be handed a `Config`. The
decision itself is `ExitAction::decide(bool)`, pure, so both branches are a unit
test rather than something you can only learn by killing the daemon. It is
re-read on every live reload, so flipping it and running `hyprpad reload` changes
what the *next* exit does with no restart.

It only means anything when `own_lizard` is on. With ownership off hyprpad never
disabled lizard mode, so there is nothing to restore either way.

### Step 2 — the user unit: DONE

`packaging/systemd/user/hyprpad.service`, installed by the block
`hyprpad setup --user` prints. `WantedBy=graphical-session.target` (the unit
`uwsm` binds on this machine — `wayland-session@start-hyprland.target` binds
`graphical-session.target`, which already carries the session's other user
services), `PartOf=` + `After=` the same target, `Restart=on-failure`,
`RestartSec=2`, `ExecStart=%h/.local/bin/hyprpad run`,
`ExecReload=%h/.local/bin/hyprpad reload` (the pidfile SIGHUP — a live re-read,
not a restart), and `Environment=HYPRPAD_OSK_BIN=%h/.local/bin/hyprpad-osk`,
because a user unit does not inherit the login shell's `PATH`.

This is the half that makes step 1 safe to turn on: `Restart=on-failure` is what
answers the trade the knob makes.

**The group finding, which cost the most to establish.** With the broker
installed, the daemon reaches `/run/hyprpad/broker.sock` only if its process is
in the `hyprpad` group — and **`SupplementaryGroups=` cannot do that in a user
unit.** `systemd.exec(5)` files it under *USER/GROUP IDENTITY*, a section whose
opening sentence is "These options are only available for system services and are
not supported for services running in per-user instances of the service manager";
a user manager has no `CAP_SETGID` and cannot call `setgroups()` at all. It is a
*silent* trap: `systemd-analyze --user verify` accepts the directive without a
word, so only the unit's own comment and a test in `src/setup.rs` stop it being
re-added.

What actually decides the group is the login session the user manager inherited,
and every unit it starts inherits that in turn. On this machine, today:

```
$ getent group hyprpad
hyprpad:x:949:ajg                    # the database says yes
$ grep ^Groups: /proc/$(pgrep -u "$USER" -x systemd)/status
Groups: 958 967 990 992 998 1000     # the running user manager says no
```

— a session that predates the `usermod -aG hyprpad`. So the instruction is
**re-login**, not `newgrp` (which reaches only the shell you type it in).
`hyprpad setup --check` reports this off the running daemon's own `/proc` entry,
which is the only place the truth is written down.

### Step 3 — the udev nudge: NOT BUILT, and here is why

The proposal was a rule on the `28de:1304` add event —
`TAG+="systemd", ENV{SYSTEMD_USER_WANTS}="hyprpad.service"` — to close the ~1.5 s
window in which a freshly-connected puck is still in lizard mode. It is
declined, for three reasons in increasing order of weight:

1. **`SYSTEMD_USER_WANTS` only *starts* a unit; it cannot poke one that is
   already running.** `systemd.device(5)`: "Adds dependencies of type `Wants=`
   from the device unit to the specified units… systemd will only act on
   `Wants=` dependencies when a device first becomes active." Once step 2 is in
   place the daemon is *already up* when the puck appears, so the nudge is a
   no-op in exactly the configuration this document is building toward. It would
   matter only if the unit were not enabled — i.e. only when you have not done
   step 2.
2. **It needs a tag hidraw does not get.** Same man page: "these udev device
   properties are not taken into account unless the device is tagged with the
   `systemd` tag", and systemd tags "all block and network devices, and a few
   others" — not hidraw. So the rule would also have to `TAG+="systemd"` the
   puck's nodes, adding device units for something nothing else models, and it
   would have to coexist with `72-hyprpad-puck.rules`, whose whole job is
   removing a tag from those same nodes at a carefully chosen sequence number.
   New failure modes, for a no-op.
3. **The window it closes is 1.5 s of a poll we control.** The daemon already
   re-disables lizard within one `RECONNECT_SCAN_INTERVAL` of the puck coming
   back (`src/run.rs`). If that ever proves too slow, the fix is in-process and
   testable — shorten the scan while lizard is known to be un-disabled, or ring
   the ownership loop's existing `lizard::nudge()` doorbell from the reconnect
   path — rather than a udev rule that can only start an already-running unit.

No file was added. `packaging/udev/` still holds only
`72-hyprpad-puck.rules`. If someone revisits this, the thing to measure first is
whether the 1.5 s is perceptible at all with the unit running: the firmware
powers up in lizard mode regardless, so some window is unavoidable and the
question is only how much of it a user notices.

### Step 4 — graduation

Unchanged, and still the owner's call: flip `restore_lizard_on_exit = false` once
hyprpad is daily-driver reliable *and* the user unit is enabled. The knob
defaults to `true` precisely so that decision has to be made deliberately.
