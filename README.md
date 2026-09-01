# hyprpad — driving Hyprland with a game controller

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

Build **hyprpad** as a passive-tap userspace daemon: read the controller
read-only (`hidraw` for the Steam Controller, `evdev` for everything else),
run a small mode machine over guide-chords and gestures, and drive the desktop
through Hyprland's IPC socket plus the `zwlr_virtual_pointer_v1` /
`zwp_virtual_keyboard_v1` Wayland protocols — all of which Hyprland already
supports. A `uhid`-proxy design remains documented as a last resort, but the
guide-release behaviour above means it is unlikely to be needed.
Details and the rejected alternatives are in
[docs/06-recommendation.md](docs/06-recommendation.md).

## Configuring the daemon

hyprpad reads **one** config file, checked in this order (first that exists wins):

| path | front-end | when to use it |
|---|---|---|
| `~/.config/hypr/hyprpad.lua` | embedded Lua 5.4 (`mlua`, vendored) | the convention — alongside `hypridle.conf`/`hyprlock.conf`; you want **modes** |
| `~/.config/hyprpad/config.lua` | same Lua front-end | the pre-convention location, still honoured |
| `~/.config/hyprpad/config.toml` | the built-in flat TOML dialect | everything else; nothing about it changed |

Lua always wins over TOML, so migrating is "write the Lua file" and rolling
back is "rename it". Both front-ends produce the same internal config, so
nothing downstream knows which ran. `config/hyprpad.lua` in this repo is a
ready-to-copy sample (and `config/hyprpad-rules.lua` is the *compositor-side*
rules file Hyprland `require`s — a different consumer, hence the distinct name). See
[docs/research/lua-config.md](docs/research/lua-config.md) for why the second
front-end exists (short version: **mode selection is logic, not data**) and
[docs/13-modality-design.md](docs/13-modality-design.md) for the model.

### The `hyprpad` API

The one global is `hyprpad` (bind it to `h`). It mirrors the ergonomics of the
owner's HypXRland `hl`/`o` config; it does **not** depend on or share a file
with the compositor's Lua state.

```lua
local h = hyprpad

h.daemon  { own_lizard = true }
h.cursor  { sens = 0.06, hysteresis = 0.0008, only_in = { "desktop" } }
h.scroll  { mode = "circular", only_in = { "desktop" } }
h.haptics { cursor_spacing_px = 96 }
h.gamepad { enabled = true }

h.bind("guide+r1", "Workspace right", h.workspace "+1")  -- desc is optional
h.bind("guide+x",  h.exec "omarchy-menu")
h.bind("guide+b",  h.dispatch "hl.dsp.window.close()")
h.bind("guide+y",  h.keyboard { mode = "split" })
h.button("dpad_up", h.key "up"):only_in("desktop")       -- bare button
h.osk_button("y",   h.key "space")                       -- only while the OSK is up
```

Actions: `h.workspace`, `h.move_to_workspace`, `h.exec`, `h.dispatch`,
`h.keyboard`, `h.key`, `h.fullscreen`, `h.set_mode`, `h.clear_mode`, `h.none`.
A plain string (`"workspace +1"`) works too — it is parsed by the same grammar
the TOML file uses.

### Modes and guards

A **mode** is a named context chosen by a predicate. Rules run in definition
order, first match wins, and **only on a context change** — a focus change, a
fullscreen change, a manual override, a reload. Never per input frame.

```lua
h.mode("game", { forward = true }).when(function(ctx)
  return ctx.focus.class:lower():match("^steam_app_") ~= nil
end)
h.mode("claude").when(function(ctx)          -- Claude Code in a terminal:
  return ctx.focus:process_tree_has("claude") -- same class as any terminal, so
end)                                          -- look inside the window
h.mode("desktop")
h.default_mode "desktop"
```

`ctx.focus` carries `class`, `title`, `pid`, `fullscreen`, and the
`process_tree_has` method (a cached `/proc` descendant walk).

Every binding then decides for itself where it is live — **guards are per
binding, not per category**:

```lua
h.button("a", h.key "enter"):only_in("desktop")
h.bind("guide+l1", h.workspace "-1"):not_in("game")
h.bind("guide+i", h.exec "…"):when(function(ctx) return ctx.focus.pid ~= nil end)
```

Unguarded means live everywhere, which is why the guide chords survive a
fullscreen game — hyprpad's escape hatch. "Game passthrough" is not a category
switch; it is *"nothing but the guide chords is live in `game`"*.

Resolution precedence: **manual override** (`h.set_mode` / `h.clear_mode` bound
to a chord) → **first matching rule** → **`default_mode`**. On any mode
transition the daemon runs the same clean handoff a controller disconnect does:
held clicks and keys released, the virtual pad neutralled, dampers reset.

### Guardrails

A config that is a program can hang or throw. The Lua front-end copies the
design the owner already proved in HypXRland's `src/config/lua/ConfigManager.cpp`:

* **Compile before anything is swapped.** The whole file is compiled with
  `into_function()` before a line of it executes, so a syntax error is reported
  with its line number and nothing was applied.
* **Fresh state every load.** Each load builds a new interpreter and a new
  config; nothing is mutated in place.
* **An instruction-count watchdog** (`lua_sethook`'s mlua equivalent) with the
  same per-context budgets: 1500 ms for a config load, 100 ms for a predicate.
  A `while true do end` in a rule is cut and counts as *no match*.
* **Last-good retention.** Any failure — syntax, runtime, a guard naming a mode
  nobody declared — leaves the running config untouched and logs the reason.
  `hyprpad reload` can never leave the daemon input dead.

## The cheat sheet

`hyprpad bindings` prints what the controller currently does, as a terminal
table; `hyprpad bindings --json` prints the same data for the on-screen widget.
Both load the config through the daemon's own `Config::load()`, so the sheet
cannot drift from the daemon — and neither touches the device, so it is safe to
run while `hyprpad run` owns the controller.

A binding's Lua description is what the sheet shows:

```lua
h.bind("guide+r1", "Workspace right", h.workspace "+1")
```

A binding without one — and every TOML binding, since that dialect cannot spell
a description — gets a label *derived* from its action (`workspace +1` reads as
"Workspace next", `exec omarchy-menu` as "Omarchy menu"). Derived labels are
marked in the JSON and shown dimmed on the card, so it is obvious which
bindings still want a description.

The widget itself is a Quickshell plugin for Omarchy's shell: it draws every
guide chord onto a diagram of the pad, with tables for the bare buttons, the
OSK helpers and the modes. `guide+view` toggles it. Install and design notes
are in [shell/README.md](shell/README.md); artwork provenance and licensing in
[shell/hyprpad.cheatsheet/art/LICENSES.md](shell/hyprpad.cheatsheet/art/LICENSES.md).

```
scripts/hyprpad-cheatsheet install
omarchy plugin enable hyprpad.cheatsheet
```

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
| [08 — The living-room vision](docs/08-living-room-vision.md) | The full target experience, and the input contract |
| [09 — The programme](docs/09-programme.md) | **The costed attack on the vision** — work items, sizes, order, risks |
| [research/](docs/research/) | Six consolidated deep-research reports the programme rests on |

## Status

Research complete through the full living-room programme
([09](docs/09-programme.md)); no implementation started. Findings dated
2026-08-30/31, taken against Hyprland 0.56.2 (HypXRland fork), Linux 7.1.9,
Steam client build 1785799196 on Omarchy 4/Arch — on the Framework 16
development machine; the living-room tower is not yet probed.
