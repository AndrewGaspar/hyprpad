# The cheat-sheet widget

An on-screen card showing what the controller currently does: a diagram of the
pad with one callout per physical control, every binding that lands on that
control stacked inside it, and a tab per context the pad can be in.

    scripts/hyprpad-cheatsheet install     # copy it into Omarchy
    omarchy plugin enable hyprpad.cheatsheet
    hyprpad-cheatsheet toggle              # what the guide chord runs

A second, much smaller plugin ships alongside it — the **bar widget**
`hyprpad.status`, documented at the [bottom of this file](#the-bar-widget).

## What a callout says

A callout is a **control**, not a binding family. Every row inside it is one
binding, and it opens with the **chord** that fires it — the button's own glyph,
behind a modifier glyph and a `+` when something has to be held first:

| row | means |
| --- | --- |
| `Ⓐ  Enter` | press it on its own |
| `Ⓢ + Ⓐ  Dictation toggle` | hold Steam, then press |
| `⌨ + Ⓧ  Backspace` | while the on-screen keyboard is up |
| `Ⓢ + Ⓡ →  Workspace right` | hold Steam, then flick that way |
| *italic* | the label was derived from the action, not written in the config |

So the A button reads `Ⓐ Enter` / `Ⓢ + Ⓐ Dictation toggle`, and X reads
`Ⓢ + Ⓧ Omarchy launcher`. The chord column is reserved at the widest chord in
the box and right-aligned into it, so the buttons stack in one column and every
label starts in the same place. The first version put the bare buttons and the
OSK helpers in tables under the drawing, which meant the reader had to join the
string `x` in a table back to the X button in the picture themselves — the one
thing the drawing was supposed to do for them; the version after that dropped
the table but left the button implicit, so a row said what it did without
saying what you press.

Rows stack bare-first, then the guide layer, then the keyboard helpers, so the
top of every callout is what the control does if you just press it.

A **group** in the layout descriptor lets several of hyprpad's control ids be
one thing you point at: the four d-pad directions get one anchor and one
leader. If all four are bound to nothing but their own arrow key, that collapses
further to a single `→ arrow keys` row; bind one of them to something
interesting and the four rows come back, each with its direction arrow. The
engine checks that for itself — the layout only supplies the members and what
the collapsed row should say.

## One tab per context

The card is one **context** at a time. There is a tab per mode the config
declares, in the order their rules are tried, plus one per context the daemon
hardwires — today the on-screen keyboard, which `hyprpad bindings --json`
reports as a `builtin` mode naming the section that *is* it. A tab shows what
is live there and nothing else, so `only in desktop` is a tab rather than a tag
on a row, and the keyboard's tab lists the things nobody can bind: the pads
driving its two cursors, a pad click or a full trigger typing the key under
one, B or Menu closing it.

Left/Right page the tabs; the bumpers are the obvious thing to bind to them,
and they are free under the sheet because its mode is exclusive:

```lua
h.button("l1", "Previous tab", h.key "left"):only_in("cheatsheet")
h.button("r1", "Next tab", h.key "right"):only_in("cheatsheet")
```

The sheet opens on the mode you were in when you summoned it. It cannot ask:
raising the sheet is itself a mode change (the layer selects `cheatsheet`), so
the daemon exports `HYPRPAD_MODE` to the command a binding execs and
`hyprpad-cheatsheet` forwards it as `{"mode":"…"}` in the summon payload.
Unset, the sheet falls back to the config's default mode.

The card is measured for its widest tab, so paging moves nothing but the rows.

The chord is `guide+view` — View is the "show me the map" button — bound in
`config/hyprpad.lua`:

```lua
h.bind("guide+view", "Cheat sheet", h.exec "hyprpad-cheatsheet toggle")
```

## How it plugs into Omarchy

Omarchy's shell is Quickshell, launched by `omarchy-launch-shell` as
`quickshell -n -p $OMARCHY_PATH/shell`, and it has a documented, first-class
extension point: a **plugin** directory under `~/.config/omarchy/plugins/<id>/`
holding a `manifest.json` plus QML. That is what this is — kind `panel`, id
`hyprpad.cheatsheet`, entry point `Panel.qml`.

The payoff is that the shell does all the wiring. `omarchy-shell shell toggle
hyprpad.cheatsheet '{}'` (which is `qs ipc -p $OMARCHY_PATH/shell call shell
toggle …` underneath) finds our loader and calls the `open()` / `close()` /
`toggle()` functions on `Panel.qml`'s root — no IPC handler of our own, no
second Quickshell instance, no socket to keep alive. Saving a file under
`plugins/` hot-reloads it.

Two things the installer deliberately leaves to the owner, and prints instead
of doing: listing the plugin in `shell.json` (that is `omarchy plugin enable`'s
job) and adding the binding to the hyprpad config.

## Where the data comes from

`hyprpad bindings --json`, re-run on every summon. That subcommand loads the
config through exactly the same `Config::load()` the daemon uses, so the sheet
cannot drift from the daemon: edit the config, summon again, and it is current.
No IPC to the running daemon, nothing that could disturb it while it owns the
controller.

`hyprpad bindings` with no flag prints the same thing as a terminal table.

Because the widget shells out to `hyprpad`, the binary has to be reachable from
the *shell's* environment. It looks for `$HYPRPAD_BIN`, then `hyprpad` on PATH,
then `~/.local/bin/hyprpad` and `~/.cargo/bin/hyprpad`, and says so plainly on
the card if it finds none. `hyprpad-cheatsheet status` reports the same.

## Files

| file | what it is |
| --- | --- |
| `manifest.json` | the Omarchy plugin manifest (schemaVersion 1, kind `panel`) |
| `Panel.qml` | layer-shell surface, summon/dismiss, runs `hyprpad bindings --json` |
| `Sheet.qml` | the card itself — pure QtQuick, no Quickshell, no `qs.Commons` |
| `Callouts.js` | grouping, collapsing, lane splitting, placement, leader routing and the SVG recolour; the arithmetic |
| `layouts/*.json` | one file per controller: drawing + control anchors + glyphs |
| `art/` | the drawings and glyphs, and `LICENSES.md` for their provenance |

`Sheet.qml` takes its palette and its data through properties rather than
importing Omarchy's singletons. `Panel.qml` is the only file that knows about
Omarchy, which keeps the door open to running the same card from a standalone
`quickshell -p` config.

## Controller-agnostic by construction

A **layout** is the whole of what the widget knows about a pad:

```jsonc
{
  "viewBox":   { "width": 456, "height": 320 },
  "art":       [ /* ordered; first drawing that exists wins */ ],
  "controls":  { "r1": { "x": 362.5, "y": 16.8, "side": "right" }, ... },
  "glyphs":    { "r1": { "image": "art/kenney/steam_rb.svg", "text": "R1" }, ... },

  // optional: several control ids that are one thing you can point at
  "groups":    { "dpad": { "label": "D-pad",
                           "members":  { "dpad_up": "up", ... },
                           "collapse": { "direction": "→", "label": "arrow keys" } } },

  // optional: what a row's leading glyph means, and how the legend spells it
  "modifiers": { "guide": { "image": "art/kenney/controller_icon.svg",
                            "text": "S", "legend": "hold Steam, then press" },
                 "osk":   { "image": "art/kenney/keyboard.svg",
                            "text": "K", "legend": "while the on-screen keyboard is up" } }
}
```

The keys are hyprpad's own control ids — the `control` field of every
`hyprpad bindings --json` entry — so adding Xbox, PlayStation, Switch Pro or
the Steam Deck is a new file in `layouts/`, not a code change. A Nintendo
layout swaps A and B by pointing the glyph map elsewhere; a pad with a hat
switch groups it the same way the puck groups its d-pad. `Panel.qml` names no
controller anywhere; the layout is chosen by `layoutId`, overridable per
summon:

    omarchy-shell shell toggle hyprpad.cheatsheet '{"layout":"steam-deck"}'

Glyphs come from Kenney's CC0 "Input Prompts", which covers every mainstream
pad with one naming convention. The *diagram* is the part that varies: see
`art/LICENSES.md` for why Kenney's own `controller_*.svg` files cannot serve as
one, and how the 2026 puck's diagram is sourced.

## Laying out multi-row callouts

A callout is now a box as tall as its rows and as wide as its widest line
(measured with `TextMetrics`, so nothing is ever clipped and a lane of terse
labels reserves no room it will not use). That makes the columns much taller
than they were, and the puck is lopsided: the face cluster, Menu, the right
stick, the right pad and the grips all want the right-hand side.

So a side is not a column, it is **one or two lanes**, decided from the data:

1. **How many.** Enough that no lane is taller than the drawing it annotates,
   capped at two — `ceil(extent / diagramHeight)`, and never more lanes than
   half the boxes, so two lanes of one box each cannot happen.
2. **Which box in which lane.** Sort innermost-first (nearest the middle of
   the drawing), then split at the point that makes the taller lane shortest.
   Keeping the split on the *x* order means the lanes read outward in the same
   order as the hardware, and no leader doubles back past a control that sits
   closer in than the one it serves.
3. **Down the lane.** Each box wants its *header* — the line the leader lands
   on, and the line that names the control — level with its own anchor. A
   forward sweep pushes boxes down to clear their predecessors; a backward
   sweep packs the run against the floor if it overflowed. Deterministic, so
   the sheet does not jitter between summons.
4. **Leaders.** Every leader on a side bends at the same x, just clear of the
   drawing, so the lead-ins read as one comb. A lane-0 leader then runs
   straight into its box. A lane-1 leader has to get *past* lane 0, and this is
   the part worth knowing about: it crosses in a **gap** between two lane-0
   boxes and then turns down the channel between the lanes. Letting it run at
   its own height instead would put it behind a near box and out the far side,
   which reads as though the two boxes were joined. Gaps are handed out in y
   order and never reused, so the crossings cannot cross each other either.

Boxes paint the card's own background, so the few leaders that still have to
pass behind one do so cleanly rather than through the text.

## Theme

Omarchy's palette, straight from the `Color` and `Style` singletons in
`qs.Commons` — `Color.popups.*`, `Color.accent`, `Color.muted`,
`Style.font.*` — so the card follows the active theme with no configuration.
The line art is recoloured to match: both the bundled schematic and Valve's
file stroke in plain `white`, and Kenney's glyphs fill `#FFFFFF`, so a single
substitution in `Callouts.recolor()` themes every piece of art. The recoloured
SVG is handed to `Image` as a `data:` URL, so no themed copy is ever written to
disk.

## Focus

The layer is `WlrKeyboardFocus.OnDemand`, not `Exclusive` — unlike most Omarchy
overlays. A cheat sheet is read *while* you keep working, and hyprpad's
on-screen keyboard types through a virtual keyboard into whatever the
compositor has focused; an exclusive grab would swallow those keystrokes. The
dismissals that matter on a controller are the chord itself and a click
anywhere outside the card; Escape works too, once the card has focus.

`exclusionMode: ExclusionMode.Ignore` means the surface reserves no space and
displaces nothing, so summoning the sheet never reflows the desktop or fights
the OSK's exclusive zone.

# The bar widget

`hyprpad.status` — the controller's mode, in Omarchy's bar. A glyph and a word:

    scripts/hyprpad-statusbar install      # copy it into Omarchy
    omarchy plugin enable hyprpad.status --section right

It is the cheat sheet's opposite number. The sheet is a thing you summon and
read; this is a thing that sits there and is *true*, and its whole job is to
answer "is the pad listening, and to what" without being asked. Clicking it
opens the sheet on the mode it is showing.

## A different kind of plugin

Same directory, same manifest envelope, same `omarchy plugin` CLI — and a
different `kind`, which changes everything downstream:

| | cheat sheet | bar widget |
| --- | --- | --- |
| `kinds` | `["panel"]` | `["bar-widget"]` |
| entry point | `"panel": "Panel.qml"` | `"barWidget": "Widget.qml"` |
| turned on by | an entry in `plugins[]` in `shell.json` | an entry in `bar.layout.<section>` |
| loaded as | a `Loader`, live only while summoned | a component registered with `BarWidgetRegistry`, instantiated once per monitor |
| injected with | `shell`, `manifest`, `omarchyPath`, … | `bar`, `moduleName`, `settings` |
| owns | its own layer-shell surface | a slot inside the bar, sized by `implicitWidth`/`implicitHeight` |

Presence in a bar section *is* "enabled" — there is no second list — so a bar
widget must **not** also be added to `plugins[]`, where it would do nothing.
Per-widget settings are inline sibling keys on that entry, read through
`BarWidget`'s `setting()`: `{"id": "hyprpad.status", "showMode": false}` gives
a glyph with no word beside it.

As with the cheat sheet, `scripts/hyprpad-statusbar install` copies the plugin
and then *prints* the placement line rather than editing `shell.json` — where a
widget sits on someone's bar is not an installer's decision.

## Where the state comes from

`$XDG_RUNTIME_DIR/hyprpad/status.json`, written by the daemon (`src/status.rs`):

```json
{"connected": true, "mode": "desktop", "controller": "Steam Controller Puck",
 "pid": 12345, "modes": ["cheatsheet", "omarchy-ui", "game", "desktop", "osk"],
 "updated": 1725230000}
```

The widget **watches a file and spawns nothing**. That is the whole design, and
it is deliberately unlike the cheat sheet, which shells out to `hyprpad
bindings --json` on every summon: a card is summoned occasionally and can
afford a subprocess, whereas a bar item is always there and must cost nothing
when nothing is happening. So there is no polling of the daemon, no IPC, and no
process spawned per update — a mode change is one `rename(2)` on the daemon's
side and one inotify wakeup on the widget's.

Each publish writes `status.json.tmp` and renames it over `status.json`, so a
reader woken mid-write still parses a whole object. The writer is an RAII guard
like the daemon's pidfile: the file exists for exactly as long as the daemon
does.

## Why the slot disappears

A bar item for hardware that is usually absent should not sit there saying
"absent". Three conditions must all hold before the widget takes any space:

1. the status file parses,
2. it says `connected: true` — the puck is here *now*. This is not the same as
   "the daemon is up": the daemon deliberately outlives its controller, sitting
   through the startup wait and the reconnect wait rather than exiting, and
   publishes `connected: false` throughout both, and
3. `/proc/<pid>` still answers for the pid in the file.

(3) covers the one case the RAII writer cannot. A daemon killed with SIGKILL
never runs its guard, so it can leave behind a file that still claims a live
controller — which is exactly why the object carries `pid`. The probe is a
plain read of `/proc/<pid>/cmdline`; procfs's zero `st_size` does not trouble
`FileView`, so even the liveness check spawns nothing.

Setting the root item's `visible: false` is the supported way to collapse a bar
slot: `Bar.qml`'s `ModuleSlot` zeroes a hidden child's `implicitWidth`, so the
neighbouring widgets close the gap rather than leaving a hole.

## The one timer that is not a poll

`FileView`'s watcher only arms on a file that **existed when the view first
loaded**. A missing file fails the load and nothing afterwards wakes it — so a
bar that came up before the daemon would never notice it arrive. A 4 s timer
covers exactly that gap and runs *only* while there is no parsed status; the
moment one loads it stops, and the watcher carries every update from then on,
including the file being removed and coming back. A second timer re-runs the
liveness probe, and runs only while there is a status file to distrust.

## The click

`env HYPRPAD_MODE=<mode> hyprpad-cheatsheet toggle`, through `Util.execArgv`,
which runs an argv without a shell re-tokenizing it.

The mode has to travel in the environment because summoning the sheet is itself
a mode change — the overlay puts the pad in `cheatsheet` — so by the time the
sheet is up, the context the reader wanted is gone. This is the same contract
the guide chord uses: the daemon exports `HYPRPAD_MODE` to the command a
binding execs, and `hyprpad-cheatsheet` forwards it in the summon payload.
Going through the script rather than spelling the IPC here is what keeps the
widget correct if that spelling ever moves.

## Theme

`bar.barForeground` and `bar.fontFamily` rather than the `Color`/`Style`
singletons directly — the bar's own copies are animated across a theme switch
and account for transparency mode. Kenney's glyph fills `#FFFFFF`, so it is
recoloured to the bar foreground and handed to `Image` as a `data:` URL, the
same trick the cheat sheet uses on its diagram; the glyph follows a light theme
with no second asset. A vertical bar drops the word and shows the glyph alone.

The widget is built on `qs.Ui`'s `WidgetButton` with `labelVisible: false`,
which is how `BarIconButton` is built too: it brings hover, the tooltip, click
registration and the theme's colour animation, and leaves the drawing to us —
a glyph and a word, which no stock button shape covers.

## Files

| file | what it is |
| --- | --- |
| `manifest.json` | the Omarchy plugin manifest (schemaVersion 1, kind `bar-widget`) |
| `Widget.qml` | the whole widget: two `FileView`s, two timers, one button |
| `art/` | Kenney's CC0 controller glyph, and `LICENSES.md` for its provenance |
