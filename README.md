# hyprsc — driving Hyprland with a game controller

Research and architecture notes for controlling a [Hyprland](https://hypr.land)
desktop from a game controller — primarily the 2026 Steam Controller, ideally any
common gamepad — **without giving up Steam Input** when a game is running.

This repository is documentation-first. The only code here is
[`tools/sc2-capture.py`](tools/sc2-capture.py), the HID probe used to establish
the findings below.

## The goal

Hold the **guide button** as a prime modifier and drive the window manager with
it — e.g. *guide + right-stick flick* changes workspace. When a game is running,
the game wins. Steam Input keeps working the rest of the time, so controller maps
and older-game compatibility are unaffected.

## The finding that shapes everything

> **`hidraw` is not exclusive. A second process can read the Steam Controller's
> raw input stream — including the Steam button — while the Steam client holds
> the same device open.**

This was verified empirically on the target machine ([full method and data](docs/03-hardware-findings.md)):
Steam held all five of the puck's `hidraw` nodes plus `/dev/uinput`, and a
passive reader still saw every button at 263 Hz. The Steam/Guide button is
`byte 4, bit 0` of report `0x42`.

That means the primary architecture does **not** need to grab, proxy, or hide the
device from Steam. It observes. Steam is undisturbed, so Steam Input, per-game
configurations and Big Picture all keep working exactly as they do today.

## The second finding

**Steam acts on the guide button's _release_, not its press — and ignores holds
longer than ~3 s entirely.** So a passive daemon has the whole hold to recognise
and dispatch a gesture, and Steam's only reaction is a trailing focus steal,
removable with a one-line Hyprland window rule
(`suppressevent activatefocus, match:class steam`) or by restoring focus over
IPC. Together these mean the passive design does not have to give anything up.

## Recommendation in one paragraph

Build **hyprsc** as a passive-tap userspace daemon: read the controller
read-only (`hidraw` for the Steam Controller, `evdev` for everything else),
run a small mode machine over guide-chords and gestures, and drive the desktop
through Hyprland's IPC socket plus the `zwlr_virtual_pointer_v1` /
`zwp_virtual_keyboard_v1` Wayland protocols — all of which Hyprland already
supports. A `uhid`-proxy design remains documented as a last resort, but the
guide-release behaviour above means it is unlikely to be needed.
Details and the rejected alternatives are in
[docs/06-recommendation.md](docs/06-recommendation.md).

## Documents

| | |
|---|---|
| [01 — Problem statement](docs/01-problem.md) | What "drive Hyprland with a controller" actually requires, and the constraints |
| [02 — Background: the Linux input stack](docs/02-background-linux-input.md) | Why gamepads are a free-for-all, and why XTEST breaks on Wayland |
| [03 — Hardware findings](docs/03-hardware-findings.md) | Empirical probing of the 2026 Steam Controller on this machine |
| [04 — Prior art](docs/04-prior-art.md) | InputPlumber, Handheld Daemon, Steam Input, gamescope, evsieve, and what each is good for |
| [05 — Architectures](docs/05-architectures.md) | Six candidate designs, with honest trade-offs |
| [06 — Recommendation](docs/06-recommendation.md) | The layered proposal, phased |
| [07 — Open questions](docs/07-open-questions.md) | What is still unverified, and how to verify it |

## Status

Research complete; no implementation started. Findings are dated 2026-08-30 and
were taken against Hyprland 0.56.2, Linux 7.1.9, Steam client build 1785799196
on Omarchy/Arch.
