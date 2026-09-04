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
switch groups it the same way the controller groups its d-pad. `Panel.qml` names no
controller anywhere; the layout is chosen by `layoutId`, overridable per
summon:

    omarchy-shell shell toggle hyprpad.cheatsheet '{"layout":"steam-deck"}'

`layouts/xbox-elite-2.json` is the second one, and it cost **no QML at all**.
The daemon publishes which controller is actually in your hands as
`"layout"` in `status.json` — it is the only process that knows — and
`scripts/hyprpad-cheatsheet` reads that one string into the summon payload
above. `hyprpad bindings --json` carries the same id, and on a pad with no
trackpads it also re-aims the two rows that name one: the ambient cursor and
scroll rows point at the right and left **sticks**, with the stick's own top
speed rather than the pad's `sens`, and the caret scrub — a thumb circling an
absolute surface — is dropped, because nothing on that controller does it.
Everything that is a *binding* is identical on both, which is the whole point
of one vocabulary.

Glyphs come from Kenney's CC0 "Input Prompts", which covers every mainstream
pad with one naming convention. The *diagram* is the part that varies: see
`art/LICENSES.md` for why Kenney's own `controller_*.svg` files cannot serve as
one, and how the 2026 controller's diagram is sourced.

## Laying out multi-row callouts

A callout is now a box as tall as its rows and as wide as its widest line
(measured with `TextMetrics`, so nothing is ever clipped and a lane of terse
labels reserves no room it will not use). That makes the columns much taller
than they were, and the controller is lopsided: the face cluster, Menu, the right
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
a glyph with no word beside it. The other two are `livenessSec` (how often the
daemon's pid is re-probed, default 5) and `navIdleSec` (how long the bar focus
ring may sit untouched before it lets go, default 12; see "The ring lets go of
itself").

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

`mode` is the mode the pad is *in*, which is not always the mode engine's: while
the on-screen keyboard owns the pads it reads `osk`, the same built-in context the
cheat sheet draws as a tab, and the engine's mode comes back when the keyboard goes
down.

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
2. it says `connected: true` — the controller is here *now*. This is not the same as
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

## Bar mode: the focus ring

The widget also hosts an accent **focus ring over the bar's own icons**, so the
D-pad can walk the bar and A can press what it lands on
(`docs/research/bar-navigation.md`, variant a′). It is a prototype for a patch
to Omarchy's `Bar.qml`, and it exists here first because nothing about it needs
upstream's permission: the bar injects itself into every widget as `bar`, and
`bar` is the bar's *root*, so `clickTargets`, `moduleTargetClickable`,
`activePopout` and `focusedScreenName()` are all callable from a widget.

Three of the bar's own facts do the work:

| what | why it matters |
| --- | --- |
| `bar.clickTargets` | every `WidgetButton` registers itself there, so the ring's stops *are* the things a mouse can click — workspace numbers and active indicators included — with the bar's visibility rules already applied |
| `target.triggerPress(button)` | "activate" is the same call a click makes, so it cannot drift from what clicking does; a right-click activate reaches the actions with no IPC verb at all (mute-all, the clock's format cycle) |
| `bar.activePopout` | the ring yields the keyboard while a panel is open and takes it back when the panel closes |

### How the controller reaches it without a single new binding

Entering bar mode maps a 1×1, click-through, keyboard-focused layer surface whose
namespace is **`omarchy-bar-nav`**. Nothing is drawn on it. Both of its jobs are
invisible: it holds the keyboard, and its namespace shows up in `hyprctl layers`
— which is the only channel hyprpad needs, because modes are keyed on
`openlayer`/`closelayer` (`src/mode.rs`), exactly as the cheat sheet is.

`config/hyprpad.lua` lists that namespace in the `omarchy-ui` allowlist. From
the moment the ring appears, then, the pad is *already* sending the right keys:

    D-pad → arrows      A → Enter      B → XF86Back

and the surface has the keyboard, so the widget's own key handler moves and
activates the ring. **One chord to enter is the entire cost on the controller
map**; there is no per-press IPC from the daemon, which there could not be
anyway — a bare button can only carry a key, never an `exec`.

    desktop --guide+dpad_up--> ring --A--> panel --A--> action
       ^                        |  ^                |
       +---------- B -----------+  +------- B ------+

Escape and Back leave. So does the arrow that steps *off* the bar — Down on a
top bar, Up on a bottom one, the inward arrow on a vertical one — which is why
the axis the ring walks and the key that leaves can never collide. Tab and
Shift-Tab step the ring too, so the bumpers (`h.key "tab"` in `omarchy-ui`) mean
the same thing at the ring level as they already do inside a panel.

### The verbs

On the widget's existing `IpcHandler`, so a keyboard user or a script can drive
the same ring:

| call | what it does |
| --- | --- |
| `omarchy-shell -q hyprpad.status navEnter` | raise the ring (this is what `guide+dpad_up` runs) |
| `omarchy-shell -q hyprpad.status navLeave` | drop it |
| `omarchy-shell -q hyprpad.status navToggle` | either, whichever applies |
| `omarchy-shell -q hyprpad.status navNext` / `navPrev` | step along the bar |
| `omarchy-shell -q hyprpad.status navActivate` | left-click the focused widget |
| `omarchy-shell -q hyprpad.status navSecondary` | right-click it |
| `omarchy-shell hyprpad.status navIsActive` | whether the ring is up — prints `true`/`false` |
| `omarchy-shell hyprpad.status navStatus` | what the ring knows about itself — one JSON object per live copy |

Note the missing `-q` on the last two rows. `omarchy-shell`'s quiet mode is
best-effort *and silent*: it suppresses stdout and exits 0 whatever happened
(`bin/omarchy-shell`, `if (( !QUIET )) && [[ -n $output ]]`). That is exactly
right for the fire-and-forget verbs `config/hyprpad.lua` binds, and exactly
wrong for a question — `omarchy-shell -q … navIsActive` prints nothing no
matter what the widget returns, which is why the ring once looked like it could
not answer for itself.

Every verb is broadcast to all live copies of the widget, because one bar
surface exists per monitor while an IPC target routes to a single handler. Each
copy then decides whether *it* is the one on the focused output; only that one
maps the nav surface.

`navStatus` is the one to reach for when the ring misbehaves, because
`navIsActive` only says the surface is up — it cannot say whether anything is
going to take it down:

```console
$ omarchy-shell hyprpad.status navStatus
[{"screen":"eDP-2","owner":true,"active":true,"idleSec":12,"idleMs":12000,
  "idleRunning":true,"yielded":false,"focused":true,"focusSeen":true,
  "locked":false,"lockService":true,"targets":17}]
```

A ring that is `active` with `idleRunning: false` is a ring that is never
leaving. `focusSeen: false` means the compositor has never fed the surface, so
the focus watch is not armed yet and the idle clock is the only backstop.
`lockService: false` means the lock hop found nothing and the lock exit is off.
One object per copy, so the array length is also the instance count.

### Making a change actually run

**Editing the installed plugin is not enough, and the shell will tell you it
was.** Omarchy file-watches `~/.config/omarchy/plugins/` and logs

    DEBUG qml: Local plugin changed, reloading: hyprpad.status

on every write, and the reload really does run: the widget is unregistered, its
item is destroyed and a fresh one is built. But the QML *type loader* inside a
long-running Quickshell keeps its own cache of that directory, so
`Widget.qml` recompiles to the unit it compiled at process start — the new file
on disk is never read. The same frozen cache rejects a file added next to it:

    WARN qml: Plugin widget hyprpad.status failed:
      file://…/hyprpad.status/Widget2.qml: File name case mismatch

for a file that plainly exists with exactly that name. So a widget edit lands
only on the *next* shell start, and a session that has been up since before the
edit is running the old code while reporting a successful reload.

This is worth knowing because the ring's exits are the kind of thing you verify
by watching it, and watching stale code proves nothing: the pre-timeout widget
stays up forever and looks exactly like a timeout that does not fire. Before
concluding anything about the ring's behaviour, confirm the running widget is
the installed one — `omarchy-shell hyprpad.status navStatus` answering `No such
method` means it is not.

To pick up an edit: restart the shell. To pick one up *without* restarting —
useful while iterating, because a plugin reload while the session is locked
takes the lock screen down with it — point the manifest's entry point at a
subdirectory the process has never listed (`"barWidget": "v2/Widget.qml"`) and
put the new file there; a directory with no cache entry is read from disk.

### The ring lets go of itself

The nav surface is not a decoration: hyprpad keys its modes on layer
namespaces, so while `omarchy-bar-nav` is mapped the daemon is *pinned* in
`omarchy-ui`. A ring nobody dismissed therefore keeps `game` from ever winning
(no controller forwarding into a game), leaves B meaning Back instead of
Backspace on the desktop, and quietly keeps the keyboard on a 1×1 surface. This
was observed for real — the ring survived a session lock and was still mapped
hours later, and only an explicit `navLeave` cleared it.

So bar mode expires. Five things end it with nobody asking:

| exit | how |
| --- | --- |
| **idle** | `navIdleSec` seconds (default 12, plugin setting, 3–60) with no key and no verb; every step, activate, verb and key press restarts the clock. With a panel open the window stretches ×5 rather than stopping — the panel owns the keyboard so the ring sees no keys, but an exit that depends on the bar's `activePopout` clearing is exactly the exit that can get stuck |
| **focus loss** | the surface stops being the compositor's keyboard focus and it is not a panel it deliberately yielded to. Read from QtQuick's attached `Window.active` on the item inside the layer surface — on Wayland a window is active exactly while it holds the keyboard — after a 750 ms grace, sized for the slowest handback because the idle timeout is what really backs it up |
| **session lock** | `omarchy.lock`'s `locked`, the same fact `omarchy-shell lock isLocked` prints, reached through the bar's injected `shell` via `serviceFor("omarchy.lock")`. Focus loss would catch the lock too; the binding makes it immediate |
| **nothing to focus** | the target list goes empty. `clickTargetsChanged` catches registration, but a widget that merely hides itself changes `moduleTargetClickable` without touching the registry, so the ring re-checks its own stops once a second while it is up |
| **teardown** | `Component.onDestruction` unmaps the surface and destroys the ring, so a plugin hot-reload cannot strand a layer behind it |

Everything is defensive: a bar that injects no `shell`, or a shell with the lock
plugin disabled, costs a `null` and falls back on the idle timeout.

### Two details worth keeping

**The ring is created as a child of the target**, not reparented onto it. It
then tracks the widget's geometry with a plain anchor, paints inside that
widget's own slot (so no z-order fight with the slots either side), and — the
reason for creating rather than reparenting — dies with the target if that
widget goes away underneath it. A workspace button vanishing when its workspace
closes is a real case; the ring is simply rebuilt on the next step.

**The focus prime is copied, not invented**: `Exclusive` at map time, then
`OnDemand` 75 ms later, exactly as `Ui/KeyboardPanel.qml` does it. Hyprland
grants an `OnDemand` surface focus when it first maps but not when an
already-mapped one asks for it back, and `Exclusive` routes every pointer event
compositor-wide to the surface for as long as it lasts (omacom/omarchy#9029) —
so the prime has to be brief, and its timing is not a number to guess at.

**Right-click turns the controller off** (`hyprpad off`). `WidgetButton` emits
`pressed(int button)` and its `MouseArea` already accepts all three buttons, so
the second gesture costs one branch: `Qt.RightButton` runs `hyprpad off`,
anything else opens the sheet. That is the *deliberate* power-off which replaces
the firmware's guide-hold one (README, "Powering the controller off") — and
choosing the other mouse button is the whole confirmation, on purpose: the owner
asked for no menu. `hyprpad off` signals the running daemon rather than touching
the controller, so the widget needs no access to the device; the widget then vanishes
by itself, because the daemon publishes `connected: false` as soon as the stream
stops. The tooltip says both.

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
| `Widget.qml` | the whole widget: two `FileView`s, six timers, one button, and the bar-mode focus ring that lets go of itself |
| `art/` | Kenney's CC0 controller glyph, and `LICENSES.md` for its provenance |
