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
h.bind("guide+menu", h.exec "omarchy-menu")
h.bind("guide+b",  h.dispatch "hl.dsp.window.close()")
h.bind("guide+y",  h.keyboard { mode = "split" })
h.button("dpad_up", h.key "up"):only_in("desktop")       -- bare button, held with it
h.button("r2",      h.mouse "left"):only_in("desktop")   -- a mouse button, through the pointer
h.button("l5",      h.exec "voxtype record toggle")      -- any other action fires once, on press
h.osk_button("y",   h.key "space")                       -- only while the OSK is up; keys only
```

Actions: `h.workspace`, `h.move_to_workspace`, `h.exec`, `h.dispatch`,
`h.keyboard`, `h.key`, `h.mouse`, `h.fullscreen`, `h.set_mode`, `h.clear_mode`,
`h.none`. A plain string (`"workspace +1"`) works too — it is parsed by the same
grammar the TOML file uses.

The mouse buttons are bare-button bindings like any other. `h.mouse "left"`
(`left|right|middle`; `"mouse left"` or `"click left"` in TOML) is a `h.key`
whose evdev code names a mouse button, and the daemon clicks it through the
virtual pointer instead of typing it. By default a right-pad click and a full R2
pull are a left click and a full L2 pull a right click (`rpad_click`, `r2`, `l2`
in `[buttons]`); a config that declares its own buttons lists the ones it wants,
so a mode can take a click away like anything else, and each shows on the cheat
sheet with its guard. They obey the same gates as every bare button: off while
the guide button is held or the on-screen keyboard is up.

A bare button takes any action a guide chord takes. What differs is that a
button is *held*: a key or mouse button (`h.key`, `h.mouse`) is pressed on the
down edge and released on the up edge, so a held arrow auto-repeats and a held
click drags; every other action — `h.exec`, `h.dispatch`, `h.workspace`,
`h.keyboard`, `h.fullscreen`, `h.set_mode`, `h.clear_mode` — fires **once** on
the press edge and never repeats while the button stays down. Both kinds go
through the same gates and guards, and a fired action does exactly what it
would do on a chord (`h.exec` gets `HYPRPAD_MODE`, `h.set_mode` runs the mode
handoff). `h.button("l5", h.exec "voxtype record toggle")` is a push-to-talk
toggle on a grip; `h.button("r4", h.keyboard { mode = "split" })` raises the
keyboard without the modifier. `h.none` is not something a button can do —
remove the line instead. `h.osk_button` stays keys-only: it types through the
on-screen keyboard's own virtual keyboard.

The mirror image holds for chords: `h.key` / `h.mouse` on a guide chord is a
**held** output too — pressed when the chord is recognised, released when the
chord button lifts or the guide is released, whichever comes first, and routed
to the keyboard or the pointer by code exactly like a bare button's.
`h.bind("guide+rpad_click", h.mouse "left")` clicks (and drags) under the
guide; `h.bind("guide+l5", h.key "leftshift")` is a modifier on a grip. On a
stick flick or `guide_hold` a key still does nothing — there is no release
edge to pair it with. A held chord ticks the pad under the hand that pressed
it (`h.haptics { buttons = true }`), not the chord buzz.

#### The pad as a mouse inside a game

`h.cursor { only_in = { "desktop" } }` gives the right pad to the game while a
game has focus. `guide_in` gives it back **while the guide button is held**:

```lua
h.cursor { sens = 0.06, only_in = { "desktop" }, guide_in = { "game" } }
h.bind("guide+rpad_click", "Click (guide mouse)", h.mouse "left"):only_in("game")
```

Hold the guide in a game and the right pad moves the desktop cursor — same
damper, same texture haptic as on the desktop — and the pad click, bound as a
chord, clicks. Steam's built-in controls use the same modality. The game gets
nothing while the guide is held (the guide layer is rank 1; the virtual pad
is neutralled on the way in), and a hold spent on pointing is *consumed*, so
releasing the guide afterwards is not handed to Steam as a guide tap. Default
is nowhere: without `guide_in` the guide layer takes the pad away everywhere,
as before. TOML spells it `[cursor] guide_in = ["game"]`, against the built-in
mode names.

One caveat until the uhid/udev masking work lands: while Steam runs
**unmasked** it sees the same controller and acts on guide+pad itself (Steam
Input's chord layer), so the two mice may move together. That is Steam's side
of the device, not something this binding can switch off.

### Modes and guards

A **mode** is a named context chosen by a predicate. Rules run in definition
order, first match wins, and **only on a context change** — a focus change, a
fullscreen change, an overlay opening or closing, a manual override, a reload.
Never per input frame.

```lua
h.mode("game", { forward = true }).when(function(ctx)
  return ctx.focus.class:lower():match("^steam_app_") ~= nil
end)
h.mode("claude").when(function(ctx)          -- Claude Code in a terminal:
  return ctx.focus:process_tree_has("claude") -- same class as any terminal, so
end)                                          -- look inside the window
h.mode("cheatsheet").when(function(ctx)      -- an overlay is a context too:
  return ctx.layers:has("hyprpad-cheatsheet") -- the sheet has the keyboard, and
end)                                          -- no window event fires for it
h.mode("desktop")
h.default_mode "desktop"
```

`ctx.focus` carries `class`, `title`, `pid`, `fullscreen`, and the
`process_tree_has` method (a cached `/proc` descendant walk). `ctx.layers` is
the layer-shell overlays on screen — hyprpad's own (`hyprpad-cheatsheet`,
`hyprpad-osk`) and everyone else's — as an array of namespaces with `:has(name)`
and `:list()` on it. It is seeded from the compositor at startup, so a daemon
restarted under the cheat sheet knows it is there.

A button can be bound once per mode, which is how one control means two things:

```lua
h.button("b", h.key "backspace"):only_in("desktop")
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
```

Modes are exclusive, so at most one of them is ever live.

Every binding then decides for itself where it is live — **guards are per
binding, not per category**:

```lua
h.button("a", h.key "enter"):only_in("desktop")
h.button("l2", h.mouse "right"):only_in("desktop")
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

The widget itself is a Quickshell plugin for Omarchy's shell. It draws a
diagram of the pad with one callout per *physical control*, and every binding
that lands on that control stacked inside it. A row is the **chord you press**:
the button's own glyph, behind a Steam glyph and a `+` when the guide button
has to be held first, or a keyboard glyph when the on-screen keyboard has to be
up. So the X button's callout reads `Ⓢ + Ⓧ  Omarchy launcher`, instead of the
reader having to join a table at the bottom of the card back to a button in the
picture.

One card is one *context*: a tab per declared mode, in the order the rules are
tried, plus one for the on-screen keyboard — a context the daemon hardwires,
which `hyprpad bindings` reports as a built-in mode of its own so the sheet can
show what the pads, the clicks and B/Menu do while the keyboard is up. A tab
lists what is live there and nothing else, so "only in desktop" is a tab rather
than a tag. Left/Right page the tabs, and the sheet opens on the mode you were
in when you summoned it: raising it is itself a mode change, so the daemon
exports `HYPRPAD_MODE` to the command a binding execs and the launcher forwards
it in the summon payload. `guide+view` toggles it. Install and design
notes are in [shell/README.md](shell/README.md); artwork provenance and licensing in
[shell/hyprpad.cheatsheet/art/LICENSES.md](shell/hyprpad.cheatsheet/art/LICENSES.md).

```
scripts/hyprpad-cheatsheet install
omarchy plugin enable hyprpad.cheatsheet
```

## The bar widget

A second, much smaller Omarchy plugin: the controller's mode, in the bar. A
glyph and a word — `desktop`, `game`, `OSK` — that **is not there** unless the
puck is connected and the daemon is alive, and that opens the cheat sheet on
the mode it is showing when you click it.

It reads one file and spawns nothing. The daemon publishes
`$XDG_RUNTIME_DIR/hyprpad/status.json` whenever its state changes — the puck
arriving or going away, and every mode transition — writing a temp file and
renaming it over the real one so a reader woken mid-write still parses a whole
object. The file is owned by an RAII guard, like the pidfile, so it exists for
exactly as long as the daemon does:

```json
{"connected": true, "mode": "desktop", "controller": "Steam Controller Puck",
 "pid": 12345, "modes": ["cheatsheet", "omarchy-ui", "game", "desktop", "osk"],
 "updated": 1725230000}
```

`mode` is the mode the pad is *in*, which is not always the mode engine's: while
the on-screen keyboard owns the pads it reads `osk`, the same built-in context the
cheat sheet draws as a tab, and the engine's mode comes back when the keyboard goes
down.

`connected` is not "the daemon is up". The daemon deliberately outlives its
controller — it sits through the startup wait and the reconnect wait rather
than exiting — and says `connected: false` throughout both, which is what lets
the widget vanish the moment the pad sleeps and come back when it wakes. `pid`
is there for the one case the guard cannot cover: a daemon killed with SIGKILL
leaves the file behind, so the widget also checks that `/proc/<pid>` still
answers before it shows anything.

The widget is a **bar widget**, a different kind of Omarchy plugin from the
cheat sheet's panel — it goes in a bar section of `shell.json` rather than in
`plugins[]`. Design notes and the full comparison are in
[shell/README.md](shell/README.md); artwork provenance in
[shell/hyprpad.status/art/LICENSES.md](shell/hyprpad.status/art/LICENSES.md).

```
scripts/hyprpad-statusbar install
omarchy plugin enable hyprpad.status --section right
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
