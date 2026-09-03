# hyprpad — driving Hyprland with a game controller

Research and architecture notes for controlling a [Hyprland](https://hypr.land)
desktop from a game controller — primarily the 2026 Steam Controller, ideally any
common gamepad — **without giving up Steam Input** when a game is running.

This repository is documentation-first. The only code here is
[`tools/sc2-capture.py`](tools/sc2-capture.py), the HID probe used to establish
the findings below.

## The goal

Hold the **guide button** as a prime modifier and drive the window manager with
it — e.g. *guide + L1/R1* changes workspace. When a game is running, the game
wins. Steam Input keeps working the rest of the time, so controller maps and
older-game compatibility are unaffected.

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

Both halves were measured on the **real** puck, in the era when Steam was
reading it directly. That is no longer the arrangement: with the forwarding path
live, what Steam reads is the virtual pad hyprpad synthesizes, and the guide
button is stripped from every frame of it. The release-not-press behaviour is
still the thing that matters, but it is now something hyprpad *plays back* —
see [the Steam button in a game](#the-steam-button-in-a-game).

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

h.daemon  { own_lizard = true, steam_button_poweroff = "off" }  -- see "Powering the controller off"
-- h.daemon { restore_lizard_on_exit = false }                  -- see "Lizard-free boot"
h.cursor  { sens = 0.06, hysteresis = 0.0008, only_in = { "desktop" } }
h.scroll  { mode = "circular", only_in = { "desktop" } }
h.scrub   { select = "l5", only_in = { "desktop" } }            -- guide + circle the left pad = caret
h.haptics { cursor_spacing_px = 96 }
h.gamepad { enabled = true, guide_tap = "steam" }               -- see "The Steam button in a game"

h.bind("guide+r1", "Workspace right", h.workspace "+1")  -- desc is optional
h.bind("guide+menu", h.exec "omarchy-menu")
h.bind("guide+b",  h.dispatch "hl.dsp.window.close()")
h.bind("guide+x",  h.workspace "emptyn")                 -- first empty workspace to the right
h.bind("guide+stick_down", h.workspace "previous")
h.bind("guide+stick_up",   "Scratchpad", h.dispatch 'hl.dsp.workspace.toggle_special("scratchpad")')  -- toggles, unlike h.workspace
h.bind("guide+lstick_left", "Focus window left", h.dispatch 'hl.dsp.focus({ direction = "l" })'):only_in("desktop")  -- the LEFT stick flicks move focus
h.bind("guide+dpad_down",  h.move_to_workspace "emptyn")
h.bind("guide+dpad_up",    "Bar panels", h.exec "omarchy-shell -q shell togglePanelAt right 1")  -- then R1 walks them
h.bind("guide+y",  h.keyboard { mode = "split" })
h.bind("guide+l4", "Link hints", h.seq { h.key "f", h.set_mode "hints" })  -- several actions, in order
h.bind("guide+quickaccess", "Controller off", h.controller_off())  -- turn the PAD off
h.button("dpad_up", h.key "up"):only_in("desktop")       -- bare button, held with it
h.button("r2",      h.mouse "left"):only_in("desktop")   -- a mouse button, through the pointer
h.button("l5",      h.exec "voxtype record toggle")      -- any other action fires once, on press
h.osk_button("y",   h.key "space")                       -- only while the OSK is up
h.osk_button("l2",  h.osk "shift")                       -- the keyboard's own actions, not just keys
```

Actions: `h.workspace`, `h.move_to_workspace`, `h.exec`, `h.dispatch`,
`h.keyboard`, `h.key`, `h.mouse`, `h.fullscreen`, `h.set_mode`, `h.clear_mode`,
`h.controller_off`, `h.seq`, `h.none`, and — on `h.osk_button` only — `h.osk`.
A plain string (`"workspace +1"`) works too — it is parsed by the same grammar
the TOML file uses.

`h.workspace` and `h.move_to_workspace` take a **Hyprland workspace selector**.
`+1`/`-1` step to the next/previous *existing* workspace (Hyprland's `e±n`,
which wraps), and a bare number is a workspace **id** (`h.workspace(3)` goes to
workspace 3, creating it if need be). Everything else is passed to the
compositor verbatim, so the whole selector grammar is available:
`h.workspace "emptyn"` (first empty workspace to the right, `max+1` if none is
free), `"empty"`, `"emptym"`, `"previous"`, `"next"`, `"r+1"`, `"m-1"`,
`"special"`, `"special:term"`. A *named* workspace is spelled out —
`h.workspace "name:foo"` — because a bare word is a **selector, not a name**.
(See `docs/research/empty-workspace.md` for what each selector resolves to.)

`h.key` takes a **combo**: modifier names joined to the key with `+`, as in
`h.key "shift+tab"`, `h.key "ctrl+left"`, `h.key "ctrl+shift+tab"` or
`h.key "super+1"` (`"key shift+tab"` in TOML) — a modifier is
`shift|ctrl|control|alt|super|meta|win` or an explicit `leftshift`/`rightctrl`/…
form, and the key is any name the table knows: the letters, the digits, the
arrows and editing keys, `f1`–`f12` and the US punctuation. The modifiers go
down before the key and come up after it wherever it is pressed — a bare button,
a guide chord, or an `h.osk_button` typed through the on-screen keyboard — so a
held `h.key "ctrl+left"` auto-repeats as a unit, and an unknown token, a
repeated modifier or a combo with nothing after its `+` is a reported error.

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
remove the line instead.

`h.osk_button` is the keyboard's own table: what a button does **while the
on-screen keyboard is up**. Out of the box it is the Steam Deck's map — a pad
click types the key under that pad's cursor, **L2 holds Shift** (momentary,
like the key itself: capitals while it is down, and the keyboard's own
one-shot/caps latch untouched), R2 is Enter, Y is Space, X is Backspace, and B
or Menu close it. A config's entries are layered *over* that map rather than
replacing it, so `h.osk_button("y", h.key "space")` changes nothing and the pad
clicks keep committing without being listed. A value is a key typed through
the keyboard (`h.key "space"`) or one of its own three actions —
`h.osk "commit"`, `h.osk "shift"`, `h.osk "dismiss"` (`"osk commit"` and so on
in TOML) — and `h.none()` takes a built-in away
(`h.osk_button("menu", h.none())`). `h.osk` means nothing on a chord or a bare
button. The cheat sheet's `osk` tab shows the map as it ends up.

A button still held when the controller changes hands never leaks into the
new layer: B closes the keyboard without typing the desktop's Backspace, the Y
of the `guide+Y` that raised it does not type a Space, a chord's button
released after the guide does not fire its bare binding, and a key held while
the mode flips away and back is not pressed again. Every layer acts on fresh
presses only — which also means a key held straight through a guide tap has to
be pressed again.

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

#### The caret jog wheel on the left pad

`h.scrub { … }` gives the **left** pad a second job under a held guide: circling
it steps the text caret, one arrow key per detent, which is how you fix a
dictation error without reaching for the keyboard. Writing the block *is* the
opt-in — the wheel is off until a config asks for it, and `enabled = false`
inside the block switches it back off without deleting the tuning beside it. A
detent is `detent_deg` of rotation (15°, so 24 to a revolution — the
granularity the circular scroll is already tuned to); circling faster than
`fast_deg_per_s` (360°/s, a revolution a second) for `fast_min_detents` detents
in a row climbs a rung to two characters a step and then to whole words
(`ctrl+arrow`, because a dictation error is word-shaped and a word jump lands on
a boundary instead of somewhere inside one), and it drops back below
`slow_deg_per_s` — half the fast threshold, a 2:1 gap so a thumb hovering near
it cannot chatter between units. Holding `select` (`l5`, the left grip: under
the fingers of the same hand whose thumb is circling) sends every step with
Shift, turning the same motion into a selection — that one is a *level* read
per frame rather than a binding, so give it a button the guide layer leaves
alone, and the cheat sheet draws both on the same callout so a collision is
visible. The guard is the cursor's and the scroll's (`only_in` / `not_in`), and
it is read together with the master switch, so a scrub nobody asked for is off
rather than live everywhere. TOML spells it `[scrub]`, same keys.

### Modes and guards

A **mode** is a named context chosen by a predicate. Rules run in definition
order, first match wins, and **only on a context change** — a focus change, a
fullscreen change, an overlay opening or closing, the session locking or
unlocking, a manual override, a reload. Never per input frame.

```lua
h.mode("game", { forward = true }).when(function(ctx)
  return ctx.focus.class:lower():match("^steam_app_") ~= nil
end)
h.mode("browser").when(function(ctx)         -- a window class, like `game`:
  local c = ctx.focus.class:lower()           -- Chrome, and Omarchy's web apps
  return c == "google-chrome" or c:match("^chrome%-") ~= nil
end)
h.mode("agent").when(function(ctx)           -- an AI coding agent in a terminal:
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

#### `ctx.locked` and the `locked` mode

`ctx.locked` is the third context source: true while the session is locked.
It exists because the other two are blind to the lock screen — Omarchy's is an
`ext-session-lock-v1` surface, so it is neither a window (no `activewindow`) nor
a layer (nothing in `ctx.layers`), and to an engine watching only those a locked
session and an idle desktop look identical. They are not: on a locked session
every bare button is typing into a password field. The sample config therefore
declares `h.mode("locked").when(function(ctx) return ctx.locked end)` **first**,
above even the cheat sheet, and guards nothing into it — which is enough, because
the D-pad, A, B, both mouse clicks, the cursor and the scroll are all `:only_in`
*other* modes and so vanish here. The guide chords are unguarded and stay live
on purpose (a chord cannot reach the field, and `guide+r5` for play/pause is a
reasonable thing to want from a lock screen), as does `guide+y` — raising the
on-screen keyboard is the one deliberate way to type at a lock screen from the
controller. `ctx.locked` reads the same in a `:when` guard, if you would rather
silence one binding than declare a mode:
`h.button("a", h.key "enter"):when(function(ctx) return not ctx.locked end)`.

The compositor emits no event for this — there is no `lockscreen>>1` on the
event socket — so the daemon seeds the state from `j/locked` at startup and then
polls the same query once a second (0.2 ms on the socket it already holds; the
`omarchy-shell lock isLocked` alternative is a subprocess at ~80 ms). Both are
opt-in: a config that never mentions `locked` is never polled, and a TOML config
never is at all.

A button can be bound once per mode, which is how one control means two things:

```lua
h.button("b", h.key "backspace"):only_in("desktop")
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
```

Modes are exclusive, so at most one of them is ever live. The sample's
`browser` mode (Google Chrome, and Omarchy's `chrome-*` web apps) is where the
browser-only bindings live — L1/R1 switch tabs there — and the desktop guards
list it as well, so everything else behaves exactly as on the desktop.

The sample's `agent` mode is the same idea one level in: an AI coding agent in
the focused terminal — Claude Code, OpenAI Codex, Muse, OpenCode — has no window
class of its own, so its rule asks `ctx.focus:process_tree_has(…)`, which matches
a lowercase **substring** of `"<comm> <cmdline>"` across the focused window's
`/proc` descendants (hence `muse-bin`: the binary is `muse-bin-<version>` and
`comm` is truncated to 15 characters). It carries one binding of its own — bare
X clears the line, readline's `ctrl+u` — and the desktop guards list it too, so
the pad is otherwise exactly what it is on the desktop.

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

#### Sequences, and a mode that gives itself back

Two spellings make the browser link hints work, and both are general.
`h.seq { h.key "f", h.set_mode "hints" }` is the one action that is a **list**
of actions, run in order on a single press: type into the focused window, *then*
move the daemon into the mode whose bare buttons are that page's hint letters.
Each step does what it would do alone, with one difference — an `h.key` inside a
sequence is a **tap**, pressed and released on the spot, because a held key
needs a release edge to pair with and a sequence has none. Sequences never nest.
What it enters is declared `h.mode("hints"):transient { max_presses = 3,
exit_on = { "b", "focus", "title", "click" }, timeout_ms = 8000 }`: a mode you
can only *enter* — `h.set_mode` is the way in, and a transient mode may not also
carry a `:when`, which the loader refuses by name — and which then lets go of
itself, because nothing outside the page can report that Vimium's hints are
gone. Every field is optional and "off" is the empty value rather than omission
(`max_presses = 0`, `timeout_ms = 0`, `exit_on = {}`), so `:transient {}` alone
is that whole contract: three presses, `b` cancels and that press is
**consumed** rather than typed, a focus change or a rename or a click drops it,
and eight seconds of silence ends a session you walked away from. Each of them
ends in the same re-resolve and the same handoff a focus change runs. The
sample config carries the whole arrangement, alphabet and all.

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

## Powering the controller off

Holding the Steam button turns the controller off — while you are still
deciding what to do with it. **The firmware does that**, not Steam and not
hyprpad: it happens with no Steam process running, the kernel's `hid-steam` is
not even bound to this pad, and Steam's own chord layout for it has no
long-press at all (its power-off is the instant `guide+Y`). The evidence is in
[docs/research/guide-hold-poweroff.md](docs/research/guide-hold-poweroff.md).

So hyprpad offers the two halves of a replacement: lengthen or disable the
firmware's timer, and give you a deliberate off switch instead.

**The timer.** `SETTING_STEAMBUTTON_POWEROFF_TIME` (25) is written in the same
`0x87` settings frame that already disables lizard mode and is re-sent every
30 s, so it survives a reconnect for free. Its companion is
`SETTING_SLEEP_INACTIVITY_TIMEOUT` (50), the idle sleep.

```lua
h.daemon { steam_button_poweroff = "off", sleep_inactivity_timeout = 600 }
```

```toml
[daemon]
steam_button_poweroff = "off"     # or a raw number: 300, 0, …
sleep_inactivity_timeout = 600
```

Both default to **unset**, which writes nothing at all and leaves the firmware
exactly as it is. `"off"` writes `0xFFFF` — the widest value the `u16` field
holds — rather than `0`: whether the firmware reads `0` as "never" or as "no
delay at all" is unverified, and under the second reading `0` would power the
pad off on *any* guide press. An integer is written raw, so `= 0` is available
for testing that reading deliberately.

> **The units of both settings are UNVERIFIED.** Valve publishes no defaults
> table and nothing public writes setting 25. Treat these as knobs to test, not
> to set and forget — the recipe is in
> [docs/design/puck-power.md](docs/design/puck-power.md).

**Reading the firmware back.** `hyprpad puck-settings [id…]` asks the controller
what a setting currently is, what maximum it accepts and what its factory
default is (`0x89`/`0x8B`/`0x8C`). Read-only — it sends queries and nothing
else — and it defaults to the two settings above:

```
$ hyprpad puck-settings 25 50
  id   current       max   default  name
  25       300     65535       300  SETTING_STEAMBUTTON_POWEROFF_TIME
  50       600     65535       600  SETTING_SLEEP_INACTIVITY_TIMEOUT
```

It needs the controller's descriptors, through the same acquire path the daemon
uses (the root fd broker if one is installed, a direct open otherwise), and the
controller has to be awake — an asleep one STALLs every node.

**The off switch.** `h.controller_off()` (`"controller_off"` in TOML) sends
`0x9F ID_TURN_OFF_CONTROLLER`. It is a one-shot, so it works on a guide chord
and on a bare button alike. The sample config puts it on the quick-access "…"
button, which is free and is nowhere near a thumb resting on the guide:

```lua
h.bind("guide+quickaccess", "Controller off", h.controller_off())
```

Two more ways in, both ending at the same write:

* `hyprpad off` — sends **SIGUSR1** to the running daemon (found through its
  pidfile, exactly as `hyprpad reload` sends SIGHUP). It goes through the daemon
  rather than opening the device itself, because the daemon already holds the
  descriptors and one writer to the puck is the invariant the design rests on;
* **right-click the bar widget.** Left click still opens the cheat sheet.
  Choosing the other button is the deliberation — there is no confirmation
  dialog, on purpose.

Turning the controller off does not stop the daemon: it sits in its reconnect
wait and the widget vanishes, exactly as when the pad sleeps on its own. Press
the Steam button to bring it back.

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
 "relay": "xbox", "source": "direct", "pid": 12345,
 "modes": ["cheatsheet", "omarchy-ui", "game", "browser", "desktop", "osk"],
 "updated": 1725230000}
```

`relay` is which virtual controller a focused game is actually being given —
`xbox` for the uinput pad, `steam` for the virtual Valve controller
(`[gamepad] kind = "steam"`, see `docs/design/uhid-relay.md`), or `none` when the
forwarding path is switched off or the Steam relay could not be created. It
reports the sink that *exists*, not the one the config asked for.

`source` is `broker` or `direct`: how hyprpad got hold of the *real* controller.
`broker` means the root fd helper passed the descriptors over, which is the only
arrangement in which Steam cannot also open the puck — see the next section.

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

## The Steam button in a game

While a game holds focus hyprpad feeds it a synthesized pad — the Xbox one by
default, the virtual Valve controller with `[gamepad] kind = "steam"` — and the
guide button is **stripped from every frame of it**, because the guide is
hyprpad's global modifier and `guide+r1` must not also open the Steam overlay.
Left there, that would take the Steam button away from Steam entirely: the guide
layer outranks game forwarding, so the frames in which you are actually holding
the button are streamed to the game as neutral, and the press never arrives.

So hyprpad makes it up instead. A **bare tap** of the guide in a game — press,
release, and nothing in between — is replayed onto the virtual pad as a 60 ms
press of the guide button, which is what opens the Steam overlay.

Nothing else qualifies, and the list is the whole point: not a chord (`guide+r1`
is a workspace change), not a stick flick, not a hold the daemon spent on the
pads (the guide-mouse, the caret scrub), and not a hold longer than **400 ms** —
that last one is the deliberating case, guide down while you decide which chord
to press and then think better of it, and it must not end in an overlay.

On the desktop that same bare tap is a **binding**, `guide_tap`, resolved
through the same guard and performing the same actions as any guide chord:

```lua
h.bind("guide_tap", "App launcher", h.exec "omarchy launcher toggle"):not_in("game")
```

The two never both fire. A bare tap has exactly one consumer and the forwarding
gate picks it: **in a game the Steam button is Steam's** — the overlay pulse
goes out and no binding runs — and everywhere else the binding does. The
`:not_in("game")` above says the same thing a second time, in the language a
config has, which is worth writing anyway so the cheat sheet reads right.

What makes a binding safe to hang off the same button as the whole guide layer
is that narrowness. A caret fix, a guide-mouse drag, a chord you thought better
of and a three-second deliberating hold all end in a release that binds
**nothing** — so a scrub session cannot summon the launcher on its way out, and
neither can a long think.

```lua
h.gamepad { guide_tap = "none" }        -- the guide is hyprpad's alone, in a game too
h.gamepad { guide_tap_max_ms = 250 }    -- a stricter idea of "a tap"
```

`guide_tap = "steam"` is the default. The cheat sheet shows the overlay as a row
on the Steam button's callout in the tab of every mode that forwards, and a
`guide_tap` binding as a row of its own. With `guide_tap = "none"` there is no
pulse anywhere to defer to, so the binding runs in a game as well — which is
exactly what that word means.

## Steam sees a Steam Controller (setup)

Optional, and off by default. With `[gamepad] kind = "steam"` hyprpad presents
Steam a **virtual Valve controller** on `/dev/uhid` and streams the real puck
into it, so Steam Input gives you trackpads as trackpads, gyro, the four back
grips and per-game configs — instead of the synthesized Xbox pad. The design is
[docs/design/uhid-relay.md](docs/design/uhid-relay.md).

Two things have to be true for that to be worth anything, and both need root:

1. hyprpad must be able to open `/dev/uhid`, which is `root 0600`;
2. Steam must **not** be able to open the real puck — otherwise it sees both
   controllers and a game counts every press twice.

Steam and hyprpad run as the same user, so no group or ACL can separate them.
The split is made by taking the puck's hidraw nodes away from *everything*
unprivileged (a udev rule) and handing hyprpad its descriptors from a tiny root
helper over a unix socket (`hyprpad broker`, socket-activated by systemd).

`hyprpad setup` **prints** the install steps and never runs them — read them,
then run them yourself:

```
hyprpad setup --print          # just the block
hyprpad setup --check          # read-only: is it installed, and does it work?
```

The files it installs all live under [`packaging/`](packaging), each with a
comment header explaining what it does and how to remove it:

| | |
|---|---|
| `packaging/udev/72-hyprpad-puck.rules` | makes the real puck root-only, and strips the `uaccess` tag Valve's own rule adds. The number is load-bearing — see the design doc §6.1 |
| `packaging/sysusers.d/hyprpad.conf` | the `hyprpad` group, which is who may ask the broker |
| `packaging/systemd/hyprpad-broker.socket` | `/run/hyprpad/broker.sock`, `0660 root:hyprpad` |
| `packaging/systemd/hyprpad-broker.service` | the broker itself, root and tightly sandboxed |

After installing, **re-login or `newgrp hyprpad`** — group membership only
reaches processes started after it is granted — and confirm with
`hyprpad setup --check`, which should print six `ok` lines and `READY`.

If the broker is not installed, or is installed and refuses, **nothing breaks**:
hyprpad opens the puck directly exactly as it always has, says so once, and runs
with no Steam relay. `status.json`'s `source` field says which half is live.

### The gyro

You should not have to do anything. When a game turns its sensors on, Steam
writes `SETTING_IMU_MODE` to the virtual controller, and hyprpad passes that to
the **real** puck — through the same feature-report writer that owns lizard mode,
so there is still exactly one thing writing to the controller. The puck then puts
its IMU data in the report hyprpad is already forwarding, and it reaches Steam
untouched. When Steam stops asking, or lets go of the device, the IMU goes back
off.

One knob exists, and it is for diagnosis rather than use:

```lua
h.gamepad { kind = "steam", gyro = true }   -- hold the IMU on regardless of Steam
```

It tells "the gyro is not working" apart from "Steam never asked" — with it on,
the IMU streams with no Steam in the picture at all. Leave it off otherwise: a
gyro running for a desktop nobody is aiming with is battery spent for nothing.

## Running at login

By default the daemon is launched by hand — from a shell that already has the
`hyprpad` group:

```
newgrp hyprpad
setsid env HYPRPAD_OSK_BIN=~/.local/bin/hyprpad-osk hyprpad run &
```

[`packaging/systemd/user/hyprpad.service`](packaging/systemd/user/hyprpad.service)
replaces that with a systemd **user** unit, so the daemon comes up with the
graphical session and restarts if it dies. `hyprpad setup --user` prints the
install and never runs it:

```
pkill -TERM -f 'hyprpad run'        # stop the hand-launched one first
install -Dm644 packaging/systemd/user/hyprpad.service \
        ~/.config/systemd/user/hyprpad.service
systemctl --user daemon-reload
systemctl --user enable --now hyprpad.service
```

The unit runs `~/.local/bin/hyprpad` and sets
`Environment=HYPRPAD_OSK_BIN=%h/.local/bin/hyprpad-osk`, because a user unit does
**not** inherit your shell's `PATH`. If either symlink is missing, make it:

```
ln -sf "$PWD/target/release/hyprpad"          ~/.local/bin/hyprpad
ln -sf "$PWD/osk/target/release/hyprpad-osk"  ~/.local/bin/hyprpad-osk
```

`ExecReload=` is `hyprpad reload`, which finds the daemon through its pidfile and
sends SIGHUP — a live config re-read, not a restart — so `systemctl --user reload
hyprpad` and `hyprpad reload` do the same thing. Logs go to the journal:
`journalctl --user -u hyprpad -f`.

**The group is the one thing the unit cannot fix.** With the broker installed the
daemon reaches its socket only if its process is in the `hyprpad` group, and
`SupplementaryGroups=` does not work in a user unit: systemd.exec(5) files it
under *USER/GROUP IDENTITY*, "only available for system services and not
supported for services running in per-user instances of the service manager" — a
user manager has no `CAP_SETGID`. Worse, it is a silent trap, since
`systemd-analyze --user verify` accepts the directive anyway. What actually
decides the group is the **login session** the user manager inherited, so after
`sudo usermod -aG hyprpad $USER` you must log out and back in. `newgrp` reaches
only the shell you type it in, never the already-running user manager.

`hyprpad setup --check` reports all of it, read-only and without running
`systemctl` — it reads the unit file, the `graphical-session.target.wants/`
symlink, and the running daemon's own `/proc` entry:

```
Running at login (optional; `hyprpad setup --user` prints the install)

  NO  user unit              nothing at ~/.config/systemd/user/hyprpad.service — …
  NO  user unit enabled      no …/graphical-session.target.wants/hyprpad.service — …
  NO  daemon under the unit  pid 3933019 is hand-launched — `pkill -TERM -f 'hyprpad run'`, …
  NO  daemon has the group   pid 3933019 lacks gid 949 (hyprpad) — … RE-LOGIN after `usermod -aG` …
```

### Lizard-free boot

The unit is also what makes the lizard-free setting safe:

```lua
h.daemon { own_lizard = true, restore_lizard_on_exit = false }
```

`restore_lizard_on_exit` defaults to **true**: on a clean exit hyprpad re-enables
the puck's firmware keyboard/mouse emulation, so a stopped daemon never leaves
the controller inert. Set it **false** and exit leaves lizard mode *disabled*, so
the puck never types arrow keys into the desktop between daemon restarts or at
boot — the goal of [docs/12-lizard-free.md](docs/12-lizard-free.md).

The trade is real and is the whole point: with `false`, a crashed or stopped
daemon leaves a puck that does **nothing** on the desktop until hyprpad runs
again. That is only acceptable when something restarts it — which is exactly what
`Restart=on-failure` in the user unit is for — and when you still have a keyboard.
Turning the knob off without the unit is the configuration to avoid.

The setting is read live: flip it and `hyprpad reload`, and the *next* exit obeys
the new value. It only means anything when `own_lizard` is on; with ownership off
hyprpad never disabled lizard mode and has nothing to restore.

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
| [12 — The lizard-free goal](docs/12-lizard-free.md) | Never boot into firmware lizard mode: the `restore_lizard_on_exit` knob and the user unit |
| [research/](docs/research/) | Six consolidated deep-research reports the programme rests on |
| [design/](docs/design/) | How the shipped features are built: the [uhid relay](docs/design/uhid-relay.md), the [puck's power](docs/design/puck-power.md) |

## Status

Research complete through the full living-room programme
([09](docs/09-programme.md)); no implementation started. Findings dated
2026-08-30/31, taken against Hyprland 0.56.2 (HypXRland fork), Linux 7.1.9,
Steam client build 1785799196 on Omarchy 4/Arch — on the Framework 16
development machine; the living-room tower is not yet probed.
