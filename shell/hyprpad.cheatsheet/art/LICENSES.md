# Artwork provenance

Three sources, on purpose. The rule is simple: **nothing that hyprpad cannot
redistribute is ever copied into this repo.**

## Bundled — `kenney/` (glyphs)

Kenney "Input Prompts" 1.5A, <https://kenney.nl/assets/input-prompts>.
**CC0 1.0 (public domain dedication).** The pack's own `License.txt` is copied
verbatim to `kenney/LICENSE.txt`; the operative wording is:

> License: (Creative Commons Zero, CC0)
> <http://creativecommons.org/publicdomain/zero/1.0/>
> You can use this content for personal, educational, and commercial purposes.
> Support by crediting 'Kenney' or 'www.kenney.nl' (this is not a requirement)

Attribution is explicitly optional, so bundling is unencumbered — we credit
anyway, here and in `shell/README.md`.

Only the 28 SVGs the Steam Controller layout actually references are vendored
(~113 KB), not the whole 4.8 MB pack. They are byte-for-byte upstream. Kenney
is the intended base for the *multi-controller* collection: it covers Xbox,
PlayStation 1–5, Switch/Switch 2 + Joy-Con + Pro, Steam Deck, Steam Frame,
Valve Index, Meta Quest, keyboard and mouse with the same systematic
`<platform>_<control>.svg` naming, so another controller's glyph map is a list
of filenames rather than new art.

Two of the 28 are not button glyphs and are worth naming, because they carry
meaning rather than identifying a control:

* `keyboard.svg`, from the pack's **Keyboard & Mouse** set, is the modifier
  glyph for the on-screen keyboard's helper bindings — the "while the keyboard
  is up" mark in front of a callout row. It is the only glyph the widget takes
  from outside the Steam Controller set, and it is deliberately a *device*
  pictogram rather than a key cap: the row means "whenever the OSK is on
  screen", not "press this key".
* `steam_dpad.svg` is the whole d-pad rather than one of its directions, for
  the grouped d-pad callout. The four `steam_dpad_<dir>.svg` files are still
  vendored, because a layout that does not group its d-pad draws four
  callouts and wants them.

### What Kenney is NOT used for

Kenney also ships `controller_<device>.svg` files, and they sound like exactly
what a callout diagram needs. They are not: they are **64×64 solid-filled
pictograms** — no analog sticks, no separable controls, no room to anchor a
leader line. (`controller_steam_new.svg` is a recognisable 2026-puck
silhouette, but at icon scale and without the sticks.) They are good for a
device picker and useless as a callout base, so the diagram comes from
elsewhere.

## Referenced, never copied — the local Steam install

`layouts/steam-controller-2026.json` prefers

    $STEAM_ROOT/steamui/images/controller/controller_config_controller_triton.svg

which is Valve's own front-view line drawing of the 2026 Steam Controller,
shipped inside Steam's UI resources. It is the only accurate diagram of this
hardware that exists — the pad is new enough that no CC0 pack covers it.

It is **read at runtime from the user's own Steam install and never copied into
this repo, never redistributed**. Users who have Steam get Valve's drawing;
users who do not get the schematic below, and the sheet is identical otherwise.
If Steam moves or renames the file, the layout falls through to the fallback —
that is what the ordered `art` list is for.

## Bundled — `steam-controller-2026.svg` (diagram fallback)

hyprpad's own schematic, drawn from scratch for this repo and covered by the
repo's licence. Its `viewBox` and every control centre are deliberately
identical to Valve's file, so one coordinate map in the layout descriptor
anchors callouts correctly on either drawing, and swapping between them is
invisible to the rest of the widget.

It also draws the shoulder ridges and trigger hints that Valve's pure front
view omits, which gives the L1/R1/L2/R2 callouts something real to point at.

## Adding another controller

Drop a `layouts/<id>.json` in beside the existing one:

* `art` — an ordered list of drawings. Point it at a bundled SVG you have the
  right to ship. (Kenney's pictograms will not do; see above.)
* `viewBox` — the coordinate space that drawing is authored in.
* `controls` — hyprpad's control ids mapped to `{x, y, side}` in that space.
* `glyphs` — the same ids mapped to `art/kenney/<platform>_<control>.svg`,
  with a `text` fallback.
* `groups` / `modifiers` — optional; see `shell/README.md`. The modifier
  glyphs are Kenney too (`controller_icon.svg` for the Steam button,
  `keyboard.svg` for the on-screen keyboard), so a new controller inherits
  them by naming the same files.

No QML changes. `Panel.qml` names no controller anywhere.
