# Artwork provenance

Four sources, on purpose. The rule is simple: **nothing that hyprpad cannot
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
leader line. (`controller_steam_new.svg` is a recognisable 2026 Steam
Controller silhouette, but at icon scale and without the sticks.) They are good for a
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

## Bundled — `xbox-elite-2.svg` (the second controller)

**Two provenances in one file.** The pad is Xelu's, CC0; the back-view paddle
inset under it is ours.

### The pad — Xelu's Xbox Series X diagram, CC0

> Body outline derived from "Free Keyboard and Controllers Prompts" by
> Nicolae (Xelu) Berbece, <https://thoseawesomeguys.com/prompts/>, released
> under CC0 1.0. Vector source via
> <https://github.com/haaldor/Xelu_prompts_SVG>.

The prompts page states: "All the assets are in the public domain license
under Creative Commons 0 (CC0) completely free to use in any personal or
commercial project". The official pack is PNG; `haaldor/Xelu_prompts_SVG`
carries the vector master (`Vector Source.svg`) under a CC0-1.0 `LICENSE`
file. CC0 requires no attribution — we give it anyway.

It is an Xbox **Series X** diagram and this layout is an Xbox **Elite Series
2**. That is deliberate, and it is not a compromise: **no openly licensed
drawing of an Elite Series 2 exists** — `docs/research/controller-art.md`
surveyed for one, and its single hit (`lemonxah/xbelite2`) ships no LICENSE —
and the Elite's *front face is a Series pad*: same ABXY diamond, same
staggered stick and d-pad, same View / Xbox / Menu, same bumpers and triggers.
Seen from the front the two shells differ in exactly one control, and we
redrew that one.

**Modifications**, all reproducible from the upstream file with the recipe in
`docs/research/controller-art.md` §7:

* extracted the `Xbox_Series` diagram group from the 2.7 MB combined vector;
* split each compound path into its subpaths — keeping every original curve
  command, rebasing only each subpath's `moveto` — so pieces could be removed
  without touching a single control point. Verified by rendering the rebuilt
  paths against the original: identical bar 43 antialiasing pixels in 547k;
* renormalised into a `456 x 436` viewBox at one uniform scale (no stretch),
  the pad occupying `x 14..442, y 6..286`;
* `#ffffff` → `white`, so `Callouts.recolor()` themes it exactly as it themes
  the Steam Controller's drawing;
* **removed the Xbox nexus** inside the guide button, drawing a plain ring in
  its place. A CC0 *copyright* dedication grants no *trademark* rights, and
  the nexus was in any case the one solid mass in an otherwise stroke-only
  drawing and dominated the page;
* **removed the Series X Share button** and drew the Elite's round **Profile**
  button at the same spot — the one control the two faces do not share, and
  worth drawing because in profile slots 1-3 the firmware mutes the paddles.

Nothing else was moved, so the drawing's control centres are still Xelu's, and
the layout's anchors were **generated rather than eyeballed**: the eight paths
decompose into 60 subpaths, and each subpath's bounding-box centre is a
control. See `layouts/xbox-elite-2.json`.

### The inset — ours

The shell's lower 114 units, repeated below the pad at 1:1 and in line with
it, with the four paddles drawn on it. Ours, CC0 / own work, under the repo's
licence; its contour is traced from Xelu's own silhouette so the two halves of
the file cannot drift apart. The paddles are the reason this controller exists
and a front view physically cannot show them, which is the whole argument for
the inset.

### Why not Steam's own Elite drawing

Steam does ship one, at

    $STEAM_ROOT/steamui/images/controller/controller_config_controller_xboxelite.png

but it is a **raster**. The widget themes a drawing by recolouring SVG strokes
(`Callouts.recolor()`), so a PNG could not follow the theme — and it is
proprietary besides. The `art` list for this layout therefore has one entry,
not two.

**No new glyphs were needed**, which is worth recording so the next
controller's author checks before drawing anything: Kenney's vendored Steam
set is already right for an Xbox pad. `steam_lb.svg` and `steam_lt.svg`
literally draw the strings "LB" and "LT" — Xbox's own naming; the A/B/X/Y
chips are lettered circles and the Elite's face diamond carries the same four
letters in the same four positions; and the d-pad, stick and grip chips have
no platform in them at all.

## Adding another controller

Drop a `layouts/<id>.json` in beside the existing one:

* `art` — an ordered list of drawings. Point it at a bundled SVG you have the
  right to ship. (Kenney's pictograms will not do; see above.) Look before you
  draw: `docs/research/controller-art.md` surveys every openly licensed
  controller vector that exists, with each one's licence and an adaptation
  recipe. A found CC0 drawing beat a careful hand-drawn one here, and it was
  not close.
* `viewBox` — the coordinate space that drawing is authored in.
* `controls` — hyprpad's control ids mapped to `{x, y, side}` in that space.
* `glyphs` — the same ids mapped to `art/kenney/<platform>_<control>.svg`,
  with a `text` fallback.
* `groups` / `modifiers` — optional; see `shell/README.md`. The modifier
  glyphs are Kenney too (`controller_icon.svg` for the Steam button,
  `keyboard.svg` for the on-screen keyboard), so a new controller inherits
  them by naming the same files.

No QML changes. `Panel.qml` names no controller anywhere.
