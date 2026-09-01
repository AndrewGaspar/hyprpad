# The cheat-sheet widget

An on-screen card showing what the controller currently does: every guide chord
drawn onto a diagram of the pad, plus compact tables for the bare buttons, the
OSK helpers and the declared modes.

    scripts/hyprpad-cheatsheet install     # copy it into Omarchy
    omarchy plugin enable hyprpad.cheatsheet
    hyprpad-cheatsheet toggle              # what the guide chord runs

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
| `Callouts.js` | grouping, placement and the SVG recolour; the arithmetic |
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
  "viewBox": { "width": 456, "height": 320 },
  "art":      [ /* ordered; first drawing that exists wins */ ],
  "controls": { "r1": { "x": 362.5, "y": 16.8, "side": "right" }, ... },
  "glyphs":   { "r1": { "image": "art/kenney/steam_rb.svg", "text": "R1" }, ... }
}
```

The keys are hyprpad's own control ids — the `control` field of every
`hyprpad bindings --json` entry — so adding Xbox, PlayStation, Switch Pro or
the Steam Deck is a new file in `layouts/`, not a code change. `Panel.qml`
names no controller anywhere; the layout is chosen by `layoutId`, overridable
per summon:

    omarchy-shell shell toggle hyprpad.cheatsheet '{"layout":"steam-deck"}'

Glyphs come from Kenney's CC0 "Input Prompts", which covers every mainstream
pad with one naming convention. The *diagram* is the part that varies: see
`art/LICENSES.md` for why Kenney's own `controller_*.svg` files cannot serve as
one, and how the 2026 puck's diagram is sourced.

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
