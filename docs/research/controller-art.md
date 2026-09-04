# Controller art: what exists, what we can use, what we must draw

*Produced 2026-09-03.*

**Status:** research note. Nothing here is a decision; the recommendation is §8.
**Scope:** artwork for `shell/hyprpad.cheatsheet` — the controller *body*
drawing behind the callouts, and the per-button *glyph* chips at the head of
each row.

The brief was the owner's: *"we're not the first ones with this problem,
somebody already built a collection and placed it in the Creative Commons —
let's make sure it's documented in the repo so we know our options."*

---

## 0. TL;DR

* **The collection exists — two of them — and neither is a drop-in.**
  **`makecindy/cindy`** (Apache-2.0) ships purpose-built keybinding-diagram
  line art for **Xbox Series X\|S, DualSense, Switch Pro, Joy-Con and a
  logo-free generic pad**, all on one `1050 x 660` canvas, stroke-only, with a
  **`press-*` hotspot layer whose ids yield exact anchor coordinates**.
  **`C0rn3j/sc-controller`** puts `images/**` under **CC0** — including
  `sc2.svg`, a correct top view of **our own 2026 Steam Controller**, with
  per-control ids and 24 `AREA_*` hit rects.
* **Xelu's prompts (CC0)** contain full white-line-art **body diagrams** for
  Xbox 360/One/Series X, PS4, PS5, Luna and Wii U — the official pack is PNG,
  but `haaldor/Xelu_prompts_SVG` (CC0) ships the vector master. Proven here at
  our exact `456 x 320` viewBox.
* **Nothing openly licensed exists for the Xbox Elite Series 2 or the Steam
  Deck.** The Elite's front face is an Xbox Series pad, so that gap closes by
  adaptation. The Deck we should draw.
* **Glyphs are a solved problem** — Kenney (CC0) is the only pack covering
  Steam Frame, Switch 2, Index and Quest. Keep it; re-pull to 1.5A.
* **No general icon set has a controller body outline.** Not one. They are
  16–24 px pictograms, and most paint with `currentColor`, which **Qt renders
  black**.
* **Stock sites are all traps** — Vecteezy, Flaticon, Freepik/Magnific, Icons8,
  IconScout each forbid redistributing the source file, which is exactly what a
  public repo does.
* **Valve's art stays where it is.** SSA §2.G forbids copying it out; reading it
  at runtime is the sanctioned pattern and we already do it. Valve *does*
  publish Steam Deck SVGs on an ungated Steamworks page — with no licence,
  so it changes nothing.
* **US design patents** are the reference nobody thinks of: orthographic plan
  views, drawn to scale, and "the text and drawings of a patent are typically
  not subject to copyright restrictions" (USPTO). USD936146S1 is the Elite
  Series 2.
* **Verified the renderer, not just the licences.** Qt 6.11's `QSvgRenderer`
  **silently ignores `clip-path`** and **sizes `<symbol>`+`<use>` wrongly** —
  two ways found art renders wrong with no error anywhere.
* Recommendation in §8: take cindy for Xbox/DualSense/Switch Pro (after one
  licence-scope question), take sc-controller's CC0 `sc2.svg` for the Steam
  Controller fallback, draw the Deck, keep Kenney.

---

## 1. What the pipeline actually demands

`shell/hyprpad.cheatsheet/art/LICENSES.md` covers provenance policy; this
section is the *mechanical* constraints, which are what disqualify most of what
the internet has.

The widget draws a sheet out of three things:

1. **One body drawing per layout.** `layouts/<id>.json` lists `art` as an
   ordered list of candidates (`steam` = resolved against the local Steam
   install; `bundled` = shipped in the plugin). First hit wins.
2. **A coordinate map.** `viewBox` (today `456 x 320` for both layouts) plus
   `controls: { <id>: {x, y, side, hidden} }` — the anchor each callout's
   leader line points at, in the drawing's own coordinate space.
3. **Per-control glyphs.** `glyphs: { <id>: {image, text} }`, today all from
   Kenney's CC0 Input Prompts, vendored under `art/kenney/`.

`Panel.qml` names no controller anywhere. Adding one is a JSON file plus art.

### 1.1 The five hard constraints

| # | Constraint | Where it comes from | What it rules out |
|---|---|---|---|
| C1 | **SVG only** | `Callouts.recolor()` is a string substitution over SVG *source text*; `Sheet.qml:605` hands the result to `Image` as a `data:image/svg+xml` URL | every PNG/JPG/WebP pack — including Valve's own Xbox / PlayStation / Switch diagrams, which are PNG |
| C2 | **Recoloured by literal colour** | `recolor()` replaces `#FFFFFF`, `#FFF`, `"white"`, `: white` — and nothing else | art coloured by `currentColor`, a CSS variable, or a grey palette. Fixable by normalisation, but that forks the upstream file |
| C3 | **Stroke-shaped art** | the sheet is a line diagram on a themed background; a filled silhouette recoloured to the foreground is a solid blob | solid-filled icon pictograms (including Kenney's own `controller_*.svg`), silhouette clip-art, photoreal renders |
| C4 | **Anchorable** | every control needs an `{x, y}` in the drawing's space that visually lands on that control | icon-scale art (24 px, 64 px) where the sticks are three pixels apart or absent; 3/4 perspective renders, where "where is R3" has no honest answer |
| C5 | **Qt's SVG renderer** | Qt 6.11 `QSvgRenderer` behind `QQuickImage` | see §1.2 — `clip-path` is silently ignored, `<symbol>` sizing is broken |

C3 kills the most candidates. The internet is full of *icons* of controllers.
We need a *diagram* of one.

### 1.2 What Qt 6.11's SVG renderer actually supports (verified here)

Measured on this machine (`qt6-svg 6.11.2-1`) by rendering fifteen probe SVGs
through `QSvgRenderer` + `QPainter` — the exact path `Image { source:
"data:image/svg+xml,…" }` takes — and looking at the output:

| Feature | Result | Consequence when adapting found art |
|---|---|---|
| plain `stroke="white"` paths | renders | the target form |
| `<style>` block, class selectors | renders | fine, *if* the declaration says `white`/`#FFFFFF` (C2) |
| `style="…"` attribute | renders | fine |
| `stroke="currentColor"` **with** a root `color` | renders in that colour | draws, but `recolor()` never sees it, so the theme is ignored. Normalise it |
| `stroke="currentColor"` **without** a root `color` | **renders BLACK** | this is the default form of *every* modern icon set (Lucide, Tabler, Phosphor, Material, Bootstrap, Fluent, Iconoir). Dropped into our dark sheet it is invisible, silently. `sed 's/currentColor/white/g'` first |
| element with **no `fill` declared** | **renders BLACK** (SVG's default) | 150 of PromptFont's 732 glyphs are like this. A substitution cannot fix it — the attribute has to be *injected* |
| inherited stroke on `<g>` / root | renders | fine — our own files rely on this |
| `<linearGradient>` fill | renders | draws, but defeats recolouring; flatten |
| **`clip-path`** | **silently ignored** — the clipped rect drew in full | an Illustrator/Figma export that clips a screen or grip region renders as a solid slab, with no error anywhere |
| `mask` | renders | works; flatten anyway |
| `filter` (`feGaussianBlur`) | renders | works; strip it, it is never wanted here |
| `<use xlink:href>` / `href` | renders | fine |
| `transform` + `stroke-dasharray` | renders | fine — we already dash the hidden controls |
| `<text>` | renders | uses *system* fonts, so the drawing changes machine to machine → **convert text to paths** |
| `vector-effect="non-scaling-stroke"` | renders | renders, but Qt's support is reported unreliable across sizes — scale `stroke-width` yourself rather than depending on it |
| no `viewBox`, only `width`/`height` | renders | works, but the coordinate map needs a `viewBox` |
| **`<symbol>` + `<use width/height>`** | **wrong** — drew a ~4 px dot where a 60 px circle belonged | icon *sprite sheets* wrap art in `<symbol>`; unwrap before use |

The two failures are the dangerous ones because neither raises an error: the
art simply renders wrong. Re-verify against a newer Qt with the recipe in §7.5.

### 1.3 What we ship today

| Layout | Body art | Provenance | Verdict |
|---|---|---|---|
| `steam-controller-2026` | Valve's `controller_config_controller_triton.svg`, read from `$STEAM_ROOT` at runtime | proprietary, **never copied** into the repo | accurate; only reaches users who have Steam installed |
| `steam-controller-2026` fallback | `art/steam-controller-2026.svg` | ours, repo licence | acceptable — the grip curves cross the shell outline near the bottom, but it reads as the right controller |
| `xbox-elite-2` | `art/xbox-elite-2.svg` | ours, repo licence | **the stopgap.** Rendered at 456 px: the grip curves cross the body outline instead of meeting it; the right-hand face cluster runs off the shell edge (the B circle is bisected by the outline); the ABXY diamond is not a diamond; the paddles are dashed lozenges floating below the grips; the d-pad and left stick sit diagonally rather than in the Xbox stagger. The owner's "pretty bad" is accurate |
| glyphs (both layouts) | Kenney Input Prompts 1.5A, CC0, 28 vendored files | bundled | good; nothing to fix |

So the problem is precisely **body outlines**, not glyphs.

---

## 2. Survey — controller BODY outlines

Ordered roughly best-fit first. "Verified" means somebody on this job fetched
the file, read the licence on its own page, and rendered it; where I say
*verified here* I did it myself in this session.

### 2.1 The two that actually work

| Source | URL | Licence | Format | Contains | Style | Fit |
|---|---|---|---|---|---|---|
| **`makecindy/cindy` design-rules silhouettes** | `github.com/makecindy/cindy` → `docs/design-rules/…` | **Apache-2.0** (repo `LICENSE`; `NOTICE`: "Cindy, Copyright 2026 XD Inc.") — *verified here* | SVG, shared `viewBox 0 0 1050 660` | **Xbox Series X\|S, DualSense, Switch Pro, Joy-Con, and a logo-free generic pad ("ultimate-c1")** | stroke-only line art: the CSS is literally `fill:none; stroke:#231815` (DualSense uses `#1b1b1b`) | **best in class.** See below |
| **`C0rn3j/sc-controller` `images/`** | `github.com/C0rn3j/sc-controller` | **CC0** for `images/**` — the repo is GPL-2.0 but `ADDITIONAL-LICENSES` says "All images in images/ directory … are licensed under the CC0 license". *Verified here by fetching both files* | SVG | `controller-images/sc2.svg` (**2026 Steam Controller**), `sc.svg` (2015 SC), `deck.svg` (Steam Deck), `ds5.svg` (DualSense) | filled greyscale with dark outlines; converts to stroke-only in four regexes | **the only open art of our own device** |

**cindy — why it is the find of this survey.** It is not controller art that
happens to exist; it is *purpose-built keybinding-diagram line art*, with a
published authoring spec (`docs/design-rules/gamepad-silhouette-authoring.md`,
which even bans auto-tracing bitmaps). Verified here:

* renders correctly through Qt 6.11.2 `QSvgRenderer` (I rendered all four);
  a subagent additionally measured it pixel-identical to librsvg (compare AE 0);
* recolours with a **single hex substitution per file** — the whole drawing is
  one `<style>` block; after `sed 's/#231815/white/'` it is white line art on
  our dark ground, and `recolor()` then themes it as-is;
* **every file carries a `<g id="press">` layer of per-control hotspots** —
  `press-face`/`press-A`…, `press-dpad`/`press-up`…, `press-lb/rb/lt/rt`,
  `press-stick-left/right`, `press-guide/view/menu/share`, and on Switch Pro
  `press-ZL/ZR/L/R/minus/plus/capture/sync`. `QSvgRenderer::boundsOnElement(id)`
  returns exact centres, and it still resolves with `display="none"` on the
  layer — i.e. **the anchor map can be generated, not eyeballed**;
* all five files share one canvas, so a whole family of layouts is consistent.

Two caveats to action before using it:

1. The README grants "the *source code* in this repository". An SVG under
   `docs/` is arguably outside that wording. The repo-wide `LICENSE` is
   Apache-2.0 with no carve-out, so the reading is fine, but **open an issue
   asking XD Inc. to confirm the SVGs under `docs/design-rules/` are
   Apache-2.0** before
   vendoring. Cheap insurance.
2. **Apache-2.0 §6 grants no trademark rights.** Strip the marks — they are
   discretely id'd (`nintendo-switch-wordmark`, `nintendo-switch-symbol`, the
   Xbox sphere inside `guide`, the PS logo inside `ps_button`). `ultimate-c1`
   has none at all. Our own schematics have never drawn a logo.

**sc-controller — the 2026 Steam Controller, CC0.** `sc2.svg` is
`viewBox 0 0 686.451 510.519`, a correct top view of the Triton: d-pad, ABXY
cluster, both sticks, both square trackpads, grips. It carries **semantic ids**
(`DPAD LPAD RPAD LSTICK RSTICK BODY LB RB LT RT LGRIP…`) *plus 24 invisible
`AREA_*` hit-test rects* — it was built for a binding editor, which is our
problem exactly. Qt-clean: no `<style>`, `<filter>`, `<mask>`, `clipPath`,
`<image>`, `<use>`, gradients; four `<text>` nodes, all `opacity:0` leftovers.

Caveats, verified here by rendering both the original and a converted copy:

* it ships **filled greyscale**, not strokes, so it needs the conversion in
  §7.2. A blanket `fill:#xxx → fill:none` is not safe: `sc2.svg` survives it,
  but the same treatment on `ds5.svg` **erases the face buttons**, which are
  drawn as fills with no stroke. Convert per shape, then look at it.
* `deck.svg` is a *stylised split-halves* Steam Deck (the screen is a torn
  graphic), not a clean front view. Usable, off-style.
* `ds5.svg` has no face buttons in the body file at all (they live in
  `images/button-images/`).
* the art is a **trace** (`tools/gen_sc2_image.py`) and what it was traced from
  is undocumented. A CC0 label does not extinguish trade-dress risk, and
  `images/sc2/C.svg` is the Steam logo. Do not vendor that one.
* `tools/sc2-source.svg` (the Inkscape master) is **outside `images/`** and so
  is GPL-2.0-only. Do not vendor it either.
* upstream `kozec/sc-controller` does **not** have this art — the C0rn3j fork
  is the source.

### 2.2 Genuinely open, second tier

**Wikimedia Commons has no Steam Controller and no Steam Deck body vector** —
only logos, compatibility badges and photographs. The categories "Steam
Controller (2015)", "Steam Controller (2026)", "Steam Deck (LCD)/(OLED)"
contain zero SVG body art. What Commons does have is Xbox 360 and PlayStation,
and a couple of genuine CC0 files among a lot of copyleft.

| Source | URL | Licence | Format | Contains | Notes |
|---|---|---|---|---|---|
| **Xelu's Free Controller & Key Prompts** | `thoseawesomeguys.com/prompts/`; SVG export at `github.com/haaldor/Xelu_prompts_SVG` (CC0-1.0 LICENSE file) | **CC0** — page: "All the assets are in the public domain license under Creative Commons 0 (CC0) completely free to use in any personal or commercial project" | official pack is **PNG only** (642 files) + a `.fla` vector master; haaldor's mirror ships `Vector Source.svg` (2.7 MB, 3287 paths, layered per platform) | **full body diagrams** for Xbox 360 / One / Series X, PS4, PS5, Luna, Wii U — 1452×940, pure white line art | a subagent extracted the Xbox Series X diagram (7 paths, 9.4 KB, zero Qt hazards), retargeted it into a `456 x 320` viewBox and recoloured it — the whole pipeline demonstrated end to end. The 7 paths decompose into **43 subpaths, one per control**. **No Steam Controller and no Steam Deck diagram.** Its *button glyphs* are 3-D bevelled rasters and will not recolour |
| **`nicefrog` "Generic Gamepad Template"** | `opengameart.org/content/generic-gamepad-template` (direct: `.../sites/default/files/controller.svg`) | **CC0** — attribution field reads "None! If you really want to, use the name 'nicefrog', thanks!" | SVG, 25 KB, 37 paths | brand-neutral generic pad in semantic Inkscape layers (`dpad`, `buttons`, `face_buttons`) | **verified here:** converts to clean white line art and renders in Qt. Cleanup needed — 9 `<text>` nodes referencing fonts nobody has, **two stray rects sitting over the sticks**, grip curves that do not close to the body, and auto-generated path ids to rename. Best available answer for a *generic gamepad* layout |
| **The Noun Project** "dualsense" #4574710 (Simone Sciacovelli) | `thenounproject.com/icon/dualsense-4574710/` | **CC BY 3.0** for the free download | SVG | detailed top-view DualSense stroke line art — d-pad arrows, four face buttons, both stick rings, touchpad, speaker grille | free downloads have **the credit typeset into the canvas**; strip and re-crop. CC BY **3.0**, not 4.0. Unmistakable Sony trade dress |
| **`Zergatul/Zergatul.Obs.InputOverlay`** | GitHub | **MIT** | SVG | Xbox Series X, DualSense | already white stroke-only with ids `ButtonA`/`DPadUp`/`LeftStickCore`; needs the red trigger-clip rects deleted and the viewBox widened (grips clip) |
| **Ryujinx `Controller_ProCon.svg`** | Ryujinx (Forgejo host; GitHub mirrors DMCA'd) | **MIT** | SVG | Switch Pro | perfect ids (`A_Button`, `Directional_Pad`); plainer than cindy; 7 CSS classes need rewriting |
| **Commons `File:Xbox Controller.svg`** (Jishenaz) | `upload.wikimedia.org/wikipedia/commons/1/1b/Xbox_Controller.svg` | **CC0** — file page carries `{{self|cc-zero}}` | SVG 744×500, 61.7 KB, 80 paths | Xbox 360, top view, full detail; a CC0 family of per-control highlight variants exists (`…AllButtons`, `…AllDPad`, `…AllAxis`) | stroke + flat grey fills, strokes uniformly `#000000`; zero gradients/filters/styles. Per-button separation proven by fill colour (caveat: the guide X glyph shares the A button's green — index by position instead). No `viewBox` |
| **Commons `File:Dualshock 4 Layout.svg`** (Tokyoship) | `upload.wikimedia.org/wikipedia/commons/e/e8/Dualshock_4_Layout.svg` | **CC BY 3.0** — attribution "Tokyoship, Wikimedia Commons" | SVG | DS4, top view | **the cleanest recolour target found anywhere**: only 3 fills (`#cccccc`, `#000000`, one `#37abc8`) plus `stroke:#000000`×53. Its touchpad texture is a single 44 KB path — deleting it takes the file 108 KB → 60 KB. One immaterial `clipPath` |
| **Commons `Xbox360_gamepad.svg`** (Grumbel) | Wikimedia Commons | **CC0 1.0** via `{{PD-OpenClipart}}` (template resolved — it *is* CC0 1.0) | SVG 43 KB, 49 paths | Xbox 360 pad | sticks are flat discs, less detail. The OpenClipart original (`/detail/12754`) is gone; Commons' copy is the survivor |
| **Commons `PlayStation 3 gamepad.svg`** (Grumbel) | Wikimedia Commons | **CC0** via `{{PD-OpenClipart}}` | SVG, 38 paths | DS3, top view | dark filled, zero gradients |
| **Openclipart "Game Controller Outline White"** (qubodup) | `openclipart.org/detail/212651` | **CC0** | SVG, 11 paths, stable ids | generic pad | ideal structure — but **no analog sticks** |
| **`icculus/ControllerImage`** (Ryan Gordon, SDL) | GitHub | code zlib; **`DATA-LICENSE.txt`: "All SVG images provided here are in the public domain… Use them how you like, without restrictions."** | 411 SVG | *glyphs*, not bodies — but filenames are **SDL gamepad element names**, and it includes `steamcontroller/` and `steamdeck/` sets with paddles | see §3 |

### 2.3 Checked and rejected

| Source | Why not |
|---|---|
| **Valve's `steamui/images/controller/*.png`** (Xbox Elite, Xbox One, PS4/PS5, Switch Pro, generic) | good line art, but **PNG** — C1. And proprietary (§4.3) |
| **Valve's public Steam Deck SVG zip** | perfect technically (§4.3) — but no licence, no redistribution grant |
| **`AL2009man/Gamepad-Asset-Pack`** | has the best-looking Steam Deck body, MIT file — **but the author's own README says the assets are "ripped straight from the official source"**. He cannot license what he does not own. Avoid |
| **RPCS3 `DualShock_3.svg`** | GPL-2.0. Technically the best-engineered file anyone found — worth copying the *structure*, not the art |
| **PCSX2** | GPL-3.0 |
| **ControllerBuddy, AntiMicroX** | GPL-3.0 |
| **EmulationStation-DE `system-controllers-outline`** | **no licence at all**, and its Xbox file is re-hosted SVG Repo art. Its bundled themes are CC BY-NC-SA |
| **`baxysquare/baxy-retroarch-themes`** Steam Deck | renders fine but 256×256 icon-grade, no ids, and GitHub reports **no licence** (MIT is a README-only claim) |
| **`lemonxah/xbelite2`** | the only Elite Series 2 art found anywhere — **no LICENSE file** |
| **`slashdevslashurandom/svg-gamepad-icons`** | Unlicense, but 9 masks + 4 filters — Qt-hostile |
| **`mimicrymedia` itch controller pack** | has Steam Deck + "Steam Controller 2.0" SVG, but the licence forbids "repackaging, redistribution, or reselling" |
| **`spragginsdesigns/react-steam-deck`** | MIT and clean, but the body is a rounded `<rect>` with gradients and 3–4 paths. Forkable, not accurate |
| **Commons `File:360 controller.svg`** (Alphathon) | `{{self|cc-by-sa-3.0|GFDL}}` — **dual copyleft.** This is the famous Wikipedia diagram and is aesthetically the closest thing in existence to our spec (pure stroke-only body outline). **Do not use.** Same for its translations (`360 controller pl/pt.svg`, `Xbox 360 -controller (fi).svg`) and `GCController Layout.svg` |
| **Commons `Nintendo Switch Joy-Con illustration.svg`** | the API's `extmetadata` says "Public domain", but the **file page carries `{{PD-shape}}` AND `{{self|cc-by-sa-4.0}}`**. PD-shape ("simple geometry, ineligible for copyright") is a stretch for a detailed illustration; if it fails you are on CC BY-SA 4.0. A good reminder to read the wikitext, not the metadata |
| Commons `DUALSHOCK3 japanese layout.svg`, `Ps-controller-icon.svg`, `Wii Classic Controller Icon.svg`, `DefaultController.svg`, `P videogame controller.svg` | CC BY-SA |
| Commons `Controller current.svg`, `Gamepad.svg`, `Gamepad stub.svg` | LGPL. `Circle-icons-gamecontroller.svg` is GPL |
| Commons `Dualshock 4 Layout 2.svg` / `Dualshock3 Layout.svg` / `SNES controller.svg` / `Retro gamepad*.svg` | licence fine, art not: 3/4 perspective (C4), or 192 gradients, or 45 gradients + clip-paths + drop-shadow filters (C5) |
| **Rawpixel** | "free with account" is the Personal Licence — non-commercial, no redistribution. Only its labelled Public Domain collection is CC0, and that holds no gamepads |
| **Desktop icon themes** (Adwaita, Breeze, Papirus, Yaru — checked locally) | only 16–48 px generic `input-gaming` pictograms, and mostly CC BY-SA / LGPL. C3 + C4 |
| **Kenney `controller_*.svg`** | 64×64 solid silhouettes, one merged path, controls knocked out as holes. Already documented in `art/LICENSES.md`; the survey re-confirms it |

---

## 3. Survey — button GLYPHS

We are not short of glyphs. Kenney is CC0, 100 % Qt-clean, recolours with one
literal, and — the point that matters most — it is the *only* pack that covers
the hardware we actually care about. This section exists so the next person
does not re-run the search.

| Pack | URL | Licence (exact) | Format | Coverage | Recolour | Verdict |
|---|---|---|---|---|---|---|
| **Kenney Input Prompts 1.5A** (what we ship) | `kenney.nl/assets/input-prompts` | "License: (Creative Commons Zero, CC0) … You can use this content for personal, educational, and commercial purposes. Support by crediting 'Kenney' or 'www.kenney.nl' (this is not a requirement)" | **1504 SVG** + 3056 PNG + 17 TTF/OTF icon fonts | 17 platform folders incl. **Steam Controller (100), Steam Deck (116), Steam Frame (65), Nintendo Switch 2 (122), Valve Index, Xbox Series (99), PlayStation Series (136), Quest, Playdate, KB+M (257)** | `fill="#FFFFFF"` in 1455/1504 — one literal replace does the set | **keep.** A subagent ran all 1504 through Qt `QSvgRenderer`: 0 invalid, 0 blank, 0 stderr; zero `<style>`, `<filter>`, `<mask>`, `clipPath`, `<text>`, `<image>`, `<use>` across the whole pack. Only 14 files use a gradient (Steam Frame grips), avoidable |
| **`icculus/ControllerImage`** (Ryan Gordon) | `github.com/icculus/ControllerImage` | code zlib; **`DATA-LICENSE.txt`: "All SVG images provided here are in the public domain… Use them how you like, without restrictions."** | 411 SVG | `art/standard/` is **Xelu redrawn as clean SVG** (the vector artifact Xelu never shipped); `art/kenney/` is Kenney. Includes `steamcontroller/` and `steamdeck/` with paddles | some files use gradients → flatten | **the best alternative to Kenney.** Filenames are SDL gamepad element names, which maps onto our control ids nicely |
| **Mr. Breakfast's Free Prompts** | `mrbreakfastsdelight.itch.io` | **CC0 1.0** (verified on itch *and* in the repo LICENSE) | 462 SVG + 462 PNG | Switch, Xbox Series, PS5, **Steam Deck**, generic, KB+M | every glyph is driven by shared Inkscape **gradients** → needs flattening | good art, more conversion work than Kenney |
| **Xelu's prompts** (glyph half) | `thoseawesomeguys.com/prompts/` | CC0 (see §2.2) | PNG | broad | **no** — the glyphs are dark grey 3-D bevelled rasters with drop shadows (`#272727`/`#545454`) | body diagrams yes (§2.2), glyphs no |
| **PromptFont** (Yukari Hafner / Shinmera) | now **codeberg.org/shinmera/promptfont** (the GitHub repo is archived) | **SIL OFL 1.1** — though the LICENSE omits the OFL copyright header, so no Reserved Font Name is actually reserved, and `package.json` claims `"(OFL-1.1 OR zlib)"` with no zlib text present. Assume OFL. Attribution requested: "PromptFont by Shinmera (Yukari Hafner), available at https://shinmera.com/promptfont" | TTF/OTF/WOFF + **732 per-glyph SVGs in `glyphs/`** (repo only) | 964 glyphs: Xbox, DS4, DualSense, Nintendo, **Steam Deck paddles**, 18 trackpad glyphs, handheld PCs (ROG Ally, Legion Go, GPD…) | **broken for us:** 1041 shapes declare fill inside `style="…;fill:#000000;…"`, only 16 use a `fill=` attribute, and **150 of 732 files declare no fill at all** (relying on SVG's default black). `currentColor` appears zero times | as a *font* it is excellent and the trackpad-direction glyphs are unique; as SVG source it needs a rewrite, and there is no reliable codepoint→filename mapping (only 71 of 964 resolve) |
| Kenney Input Prompts **Pixel** / **Pixel 1-Bit** | kenney.nl | CC0 | **PNG only**, 16×16 | no Switch 2 / Frame / 2026 SC | irrelevant at 456×320 |
| **Meritite "1000+ Input Prompts"**, **JulioCacko**, **greatdocbrown** | itch / OGA | CC0 | SVG+PNG / PNG / 16px pixel | assorted | no reason to switch |
| **Steam's own `controller_base/images/api/knockout/`** | local Steam install | proprietary — runtime read only | ~300 SVG, 32×32 | `shared_*`, `sc_*`, `sd_*`, `ps4/ps5_*`, `switchpro_*`, `xbox_*`, `joyconpair_*`, 8BitDo, Legion Go S | **pure white silhouettes — recolour with our existing `recolor()` untouched** | a runtime fallback if Kenney ever falls short; never a bundled replacement |

**Conclusion for glyphs: change nothing.** Kenney 1.5A is dated 2026-07-11 and
added the Steam Controller / Steam Frame / Index / Quest / Switch 2 sets — worth
**re-pulling the pack** (we vendored 28 files from it) regardless of what
happens to the bodies.

---

## 4. Valve, and the other vendors

### 4.1 What is actually in a local Steam install

Verified on this machine (`~/.local/share/Steam`, read-only inspection).
`steamui/images/controller/` holds **three SVGs and fourteen PNGs**:

| File | Format | Notes |
|---|---|---|
| `controller_config_controller_triton.svg` | SVG, `viewBox 0 0 456 320` | the 2026 Steam Controller. 39 top-level elements, **no groups, no ids, no `<defs>`, no `<style>`, no clip-path, no gradients**. `stroke="white"` on 25 elements, `fill="white"` on 14. This is why our pipeline works at all — it is, by luck, authored exactly as `recolor()` wants. 39 substitutions on our `recolor()` |
| `controller_config_controller_steam_deck.svg` | SVG, `viewBox 0 0 449 181` | Steam Deck front plan. Strokes are **`#8B929A`**, body fill `#0D131B` — our `recolor()` makes *one* substitution and it hits an invisible rect inside a `clipPath`. Usable, but only after adding `#8B929A` to the substitution list |
| `controller_config_controller_steam_frame.svg` | SVG, `viewBox 0 0 728.9 648` | the Frame's controller pair, three-quarter view. 234 paths, **zero `stroke` attributes** — the line weight is baked into filled geometry, coloured by a two-rule `<style>` (`.cls-1{fill:#fff}`, `.cls-2{fill:#afafaf}`). Recolours only half without adding `#afafaf`; per-control highlighting is impossible without authoring classes |
| `..._xboxelite.png`, `..._xboxone.png`, `..._x360.png`, `..._ps4/ps5/ps3.png`, `..._switch_pro.png`, `..._switch_joycon*.png`, `..._generic/android/apple/touch.png`, `cropped_controller_config_controller.png` (2015 SC) | PNG ~1000 px | flat two-colour line art on transparency — body `#0D131B`, strokes `#67707B`. `xboxone` and `x360` are **byte-identical**. `ps3` is a shaded photoreal render and `touch` is a UI mock; neither is line art |

Two consequences:

* **C1 is the whole story for Xbox/PlayStation/Switch.** Valve *has* good line
  art for all of them. It is PNG, so our stroke-recolour pipeline cannot theme
  it, which is exactly what `art/LICENSES.md` already records for the Elite.
* **The Steam Deck is the one free win.** Its drawing is an SVG, it is already
  on disk for every Steam user, and it needs one extra colour in `recolor()`.
  A `steam-deck` layout is achievable today by the same runtime-read route as
  the Steam Controller, with a bundled fallback we still have to draw.

Anchor extraction from `triton.svg` is easier than it looks despite having no
ids: the 39 children are in a stable order, and the circles and rects carry
literal `cx`/`cy` (Steam button at `cx 227.5, cy 76`; A at `cx 367.13,
cy 101.51` — which is where our layout's numbers came from). Steam itself does
*not* derive anchors from the file: its own config screen sets the SVG as a CSS
`background-image` and positions labels with hardcoded per-controller offsets.

### 4.2 A glyph library nobody mentions: `controller_base/images/api/`

`~/.local/share/Steam/controller_base/images/api/{dark,light,knockout}/` is
~17 MB of Steam Input glyph art — the files behind
`ISteamInput::GetGlyphSVGForActionOrigin`.

* **~300 SVGs per theme**, 32×32 viewBox, fill-only.
* **`knockout/` is pure white silhouette** — i.e. it recolours with our
  existing `recolor()` untouched.
* Families: `shared_*` (74 — ABXY, d-pad, sticks, gyro, M1–M8, mouse), `sc_*`
  (2015 SC, 41), **`sd_*` (Steam Deck, 32)**, `ps4_*`, `ps5_*`, `switchpro_*`,
  `xbox_*`, `xbox360_*`, `joyconpair_*`, `8bitdo_*`, Legion Go S.
* No `triton_*` set — the 2026 controller reuses `sc_*`/`sd_*`/`shared_*`.

Same licence position as `triton.svg`: read at runtime, never redistribute. It
is a *fallback* option for glyph coverage if Kenney ever falls short, not a
replacement for a bundled CC0 set — we do not want the sheet to lose its glyphs
on a machine without Steam.

### 4.3 The legal position, quoted

**Valve — bundling is out.** Steam Subscriber Agreement §2.G
(<https://store.steampowered.com/subscriber_agreement/>):

> "Except as otherwise permitted under this Agreement …, you may not, in whole
> or in part, copy, photocopy, reproduce, publish, distribute, translate,
> reverse engineer, derive source code from, modify, disassemble, decompile,
> create derivative works based on, or remove any proprietary notices or labels
> from the Content and Services or any software accessed via Steam without the
> prior consent, in writing, of Valve."

§1.B defines "Content and Services" to cover the Steam client and everything
downloaded through it, so the SVG is inside it. §2.D (Fan Art) does not help:
it is scoped to *Valve games*, not client UI art, and is "solely on a
non-commercial basis" — incompatible with an OSI licence anyway.

**Reading at runtime is the sanctioned pattern.** `ISteamInput::
GetGlyphSVGForActionOrigin` is documented as returning "a local path to a SVG
file", with Valve's own example pointing inside the user's install. Valve
documents games reading its glyph art off the user's disk; it attaches no
redistribution grant because the API never contemplates one. Our layout's
`art: [{source: "steam"}, {source: "bundled"}]` ordering is precisely that
line, and it should stay there.

**The one surprise: Valve publishes Steam Deck SVGs publicly.**
<https://partner.steamgames.com/doc/steamhardware/steamdeck/svg> is un-gated
(HTTP 200, no partner login) and says, in full:

> "Here are front, top, and back views of Steam Deck in SVG format. These are
> provided in case you'd like to add Steam Deck specific input callouts in your
> game."

The ZIP is on a public CDN
(`https://shared.fastly.steamstatic.com/community_assets/images/steamworks_docs/english/steamdeckSVG.zip`,
13 KB: `steamdeckFront.svg`, `steamdeckTop.svg`, `steamdeckBack.svg`).

I downloaded and rendered them. They are **exactly the shape our pipeline
wants**: `fill="none"`, `stroke="white"`, no `<style>`, no clip-path, no
gradients, `viewBox 0 0 1024 414` (front). The front view is clean, accurate
line art — shell outline, both grips, d-pad, both sticks with concentric
rings, both trackpads, four unlettered face circles, View/Menu/Steam/QAM
pills, speaker grilles, screen bezel. Better than anything else in this survey,
and the stated purpose — "in case you'd like to add Steam Deck specific input
callouts in your game" — is *literally our use case*. The catch: the whole
drawing is **3–4 merged mega-paths**, so there is no per-control geometry to
extract; anchors would be read off by eye like ours are today.

**It states no licence, no redistribution grant and no modification terms** —
it is an offer of *use*, not a licence to *redistribute*, so it does not change
what we may bundle. It is Steam Deck only; there is no `/svg` page for the
Steam Frame, Steam Machine or the 2026 Steam Controller.

Two separate rights, worth stating once: an open licence on somebody's
re-creation clears the *drawing's copyright* only. Valve's trade dress in the
device shape, and its trademarks (the Steam logo is literally elements 32–33 of
`triton.svg`), cannot be sub-licensed by a third party. Our own schematics have
never drawn a logo; keep it that way.

**Valve's 2026 controller CAD** was released on
`gitlab.steamos.cloud/SteamHardware/SteamController` under "a Creative Commons
license" (Valve's wording; press reports **CC BY-NC-SA 4.0**). Irrelevant twice
over: NC + SA, and it is STP/STL 3D geometry, not 2D vector.

**Microsoft, Sony, Nintendo — all no**, verified from their public pages:

* Microsoft trademark/IP page: "our logos, app and product icons,
  illustrations, photographs, videos, and designs **can never be used without an
  express license**." The copyright-permissions page bars icon use "in software
  applications" outright.
* Sony Interactive Entertainment ToS: "Except for personal, non-commercial,
  internal use, you are prohibited from using (including … copying, modifying,
  reproducing …, distributing, licensing, selling and publishing) any of the
  materials, without obtaining SIE Inc's prior written permission."
* Nintendo: "**Nintendo does not grant permission to individuals to use any
  content from this website.** Because we receive thousands of such requests,
  our policy is to decline use of our trademarks and copyrights."

There is no Valve press kit with hardware vector assets
(<https://www.valvesoftware.com/en/press> is a boilerplate page), and the
2025–26 hardware store pages carry no media-asset downloads.

---

## 5. Icon sets and stock sites — the negative result, written down

Two reasons this section exists: so nobody re-runs the search, and because the
licence traps are worth knowing once.

### 5.1 General icon sets

**No general icon set contains a controller body outline.** Every one is a
16–24 px pictogram — a rounded blob with two dots for sticks. That is C3 and C4
together. Verified by rendering, not by reading descriptions.

They do have *glyph* content, and some of it is decent:

| Set | Licence (verified) | Gamepad content | Notes |
|---|---|---|---|
| **Qlementine Icons** | **MIT** ("Copyright (c) 2023 Olivier Cléro") | a complete family: `gamepad-button-{top,bottom,left,right}`, `-dpad`, `-joystick-{left,right}`, `-shoulder-{left,right}`, `-start` | Qt-native set. **Position-based, not letter-based**, which sidesteps the A/B-vs-Cross/Circle problem entirely. Two-tone via `fill-opacity`, which Qt renders correctly |
| **Fluent UI System Icons** (Microsoft) | **MIT** | circled A/B/X/Y, shaped LB/RB/LT/RT | exactly the vocabulary; the lettering is Xbox-branded, and trademark is separate from the MIT copyright grant |
| **Iconoir** | MIT | `xbox-a/b/x/y` | clean stroke circles |
| **Tabler** | MIT | `playstation-{circle,square,triangle,x}` | |
| **Material Symbols** | **Apache-2.0** | directional `gamepad-{up,down,left,right}` | cleanest licence of the set |
| **Bootstrap Icons** | MIT | `dpad`, `dpad-fill` | |
| **game-icons.net** | repo `license.txt`: "Creative Commons 3.0 BY or CC0 if mentioned below" (per contributor) | generic gamepads/consoles | note **3.0**, not 4.0. Attribution: "Icons made by {author}. Available on https://game-icons.net". Files carry a **black background rect you must delete** |
| **Font Awesome Free** | icons CC BY 4.0, fonts OFL 1.1, code MIT | `gamepad` + brand marks | each SVG embeds `<!--! Font Awesome Free 6.7.2 by @fontawesome … -->`; **preserving that comment satisfies CC BY**. Brand icons: "Please do not use brand logos for any purpose except to represent the company, product, or service to which they refer" |
| **Simple Icons** | CC0 | PlayStation, `steamdeck` — **Xbox was removed** (404; six re-add requests, all closed) | logos, not controls |
| **OpenMoji** | **CC BY-SA 4.0** | emoji gamepad | SA — excluded |
| **Twemoji / Noto Emoji** | CC BY 4.0 / Apache-2.0 + OFL | emoji gamepad | 8–9 colour illustrations; useless for single-colour recolour |
| **Feather, Heroicons** | MIT | **none** | zero gamepad content |
| **Nerd Fonts** | NOASSERTION ("various sources under various licenses") | re-exposes MDI/FA glyphs | go upstream instead |
| **Lucide (ISC), Phosphor (MIT)** | as stated | generic `gamepad` pictogram | C3/C4 |
| desktop icon themes — Adwaita, Breeze, Papirus, Yaru (checked locally) | CC BY-SA / LGPL / GPL | 16–48 px `input-gaming` | C3/C4 *and* copyleft |

**Two licence corrections worth carrying forward:**

* **Remix Icon is no longer Apache-2.0.** Upstream now ships a bespoke "Remix
  Icon License v1.0, January 2026" barring use "as the primary value of the
  product". **Iconify still reports Apache-2.0** because it indexes a mirror.
  Do not trust aggregator metadata over the upstream repo — that goes for every
  row in this table.
* Every set above except Kenney and game-icons paints with **`currentColor`**,
  which Qt renders **black** when no `color` is set (§1.2). It is a one-line
  fix, but it is a fix you must remember.

### 5.2 Stock marketplaces — five hard noes

These are the traps. All five let you *use* a file and forbid the thing an
open-source repo does, which is *redistribute the source file*.

| Site | The operative clause | Verdict |
|---|---|---|
| **Vecteezy** | prohibits "If you add Vecteezy files into software or an app where people other than you can edit or access the original image files" | **no** — that is precisely a public git repo |
| **Flaticon** | prohibits "Distribute Flaticon Contents unless it has been expressly authorized" and "Include Flaticon Contents in an online or offline database or file"; grant is "non-sublicensable" | **no** — non-sublicensable alone kills MIT relicensing |
| **Freepik** (now **Magnific**) | "the User is not authorized to distribute, resell or rent any Magnific Content" | **no** |
| **Icons8** | prohibits "To distribute, export, post, upload, download Licensed Material online in the downloadable format" | **no.** Linkware buys *use*, never redistribution. The only route is their case-by-case "established open-source projects can get our graphics for free" grant — i.e. send an email |
| **IconScout** | no redistribution "With source files included" | **no** |
| **SVG Repo** | house licence: "You can't redistribute material in a similar way to SVG Repo website as it is" — a bespoke non-OSI term | **conditional.** Only usable per-icon where the page's `LICENSE:` field says CC0 / Public Domain / MIT — it is an aggregator, and the per-icon field is the only thing that matters |
| **The Noun Project** | CC BY 3.0 for free downloads, otherwise royalty-free paid | **usable with work.** "dualsense" #4574710 by Simone Sciacovelli is genuine top-view stroke line art with every button drawn. Two costs: free downloads have **the credit typeset into the canvas** (strip and re-crop), and it is CC BY **3.0**. Attribution form: "'Tree' icon by Edward Boatman from Noun Project CC BY 3.0". Unmistakable Sony trade dress |
| **Figma Community** | free files default to CC BY 4.0; there is **no per-file licence selector**, so an author must say so in text | **don't.** Every controller file traces Microsoft or Sony hardware, exports with clip-paths and filters (C5), and needs an account |
| **OpenGameArt** | per-asset; the licence field records only the *uploader's declaration* | **yes, selectively** — see nicefrog below |

### 5.3 The one stock-ish find worth having

**nicefrog, "Generic Gamepad Template"** —
<https://opengameart.org/content/generic-gamepad-template>, direct SVG at
`opengameart.org/sites/default/files/controller.svg`. **CC0**; the attribution
field reads verbatim: *"None! If you really want to, use the name 'nicefrog',
thanks!"*

A genuine top-view line-art body — d-pad, two stick wells, four face buttons,
select/start, shoulder buttons — in 37 separate paths inside semantic Inkscape
layers (`dpad`, `buttons`, `face_buttons`). Deliberately generic (drawn against
a Logitech F310), **no trademarks at all**. Cleanup: delete the 9 `<text>`
nodes (they reference fonts nobody has), drop two stray rects, and name the
auto-generated path ids once.

**This is the answer for a "generic gamepad" layout** — the fallback sheet for
a pad we have no specific art for.

---

## 6. Reference sources for drawing our own

If we draw the bodies ourselves (§8), the drawing still has to be *accurate*.
These are references, not art to copy: what they give is correct proportions
and control positions.

### 6.1 US design patents — orthographic plan views, effectively PD

This is the best-kept secret in the survey. Every one of these controllers has
a US **design patent**, and a design patent is a set of clean orthographic line
drawings: front, back, top, bottom, both sides. The front/plan figure is
exactly the view our diagram wants, drawn to scale by a professional patent
draftsman.

The USPTO's own terms of use say:

> "most government-produced materials appearing on this website are not subject
> to copyright restrictions within the United States"
> … "the text and drawings of a patent are typically not subject to copyright
> restrictions"
> — <https://www.uspto.gov/terms-use-uspto-websites>

with two caveats the same page raises and that matter here:

* the patent *grant* is a right over the ornamental design of the article — it
  stops you **making the controller**, not drawing a diagram of one. A cheat
  sheet is not an infringing article of manufacture. (Not legal advice; it is
  the reading that lets emulator projects ship controller diagrams.)
* "trademarks may be embedded in patents as part of the drawing" — so do not
  trace a logo. Our schematics have never drawn one, and should not start.

Verified entries:

| Controller | Patent | Assignee | Filed | Plan view | PDF |
|---|---|---|---|---|---|
| **Xbox Elite Series 2** | [USD936146S1](https://patents.google.com/patent/USD936146S1/en) | Microsoft | 2019-04-04 | FIG. 3 is a full front/plan view; FIG. 5 is the top view | `patentimages.storage.googleapis.com/4b/11/83/910f023a6cfd5f/USD936146.pdf` |
| Xbox (Series X\|S family) | [USD905166S1](https://patents.google.com/patent/USD905166S1/en) | Microsoft | 2019-05-31 | eight figures incl. top and front | linked from the Google Patents page |

I downloaded and looked at USD936146's FIG. 3: a clean orthographic plan view
of the Elite Series 2 with the claimed faceplate in solid line and the rest of
the shell in broken line — bumpers, both sticks with their concentric rings,
the d-pad, the four face buttons, View/Menu/Guide, the profile button, the USB-C
port and the 3.5 mm jack, all in correct relative position. It is *better*
reference than any photograph, because it is orthographic: no perspective to
undo.

**Format caveat, verified:** the Google-hosted patent PDFs are not vector. Every
page is a single 1-bit CCITT-G4 bitmap at 300 dpi (2560 × 3300) — I checked with
`pdfimages -list`, and `pdftocairo -svg` yields zero `<path>` elements. So there
is no free vector extraction; the workflow is `pdfimages -png` → `potrace` (or
trace by hand over the image on a locked layer). At 300 dpi 1-bit, potrace's
output is very clean.

The equivalent Sony (DualSense) and Nintendo (Switch Pro) design patents
certainly exist — the Nintendo hits in search were the *Wii U* Pro Controller
(USD692000S1, USD692887S1, 2013) and I did not pin down the Switch-era or
DualSense numbers before writing this. Finding them is a five-minute job on
Google Patents (`inassignee:"Sony Interactive Entertainment" game controller`,
design patents only) and should be done at drawing time, not now.

### 6.2 Photographs and manufacturer press assets

Manufacturer press kits (Xbox Wire, Sony Interactive Entertainment press,
Nintendo) publish high-resolution *photographs*, generally under terms that
allow editorial use and forbid redistribution or derivative works. They are
fine to look at while drawing and must not be traced-and-shipped. Same for
retail product photography.

### 6.3 Our own hardware

For the Steam Controller and the Xbox Elite 2, the owner has the hardware. A
flatbed-scanner or straight-overhead phone photo with a ruler in frame beats
every online reference for getting the *proportions* right, and it is
unambiguously ours.

---

## 7. How to adapt a found SVG to this pipeline

Assume the best case: someone hands us a permissively-licensed, top-view,
line-art SVG of a DualSense. It still needs five passes before it is a layout.

### 7.1 Strip what Qt cannot or should not draw

```sh
# Inspect before touching anything.
grep -o '<\(style\|filter\|mask\|clipPath\|symbol\|image\|text\|foreignObject\)' art.svg | sort | uniq -c
grep -o 'url(#[^)]*)' art.svg | sort | uniq -c
```

* `clip-path` — **must go** (Qt 6.11 ignores it silently, §1.2). Either apply
  the clip in a vector editor and export the flattened geometry, or delete the
  clipped element if it was only hiding an overhang.
* `<symbol>`/sprite wrappers — **must go** (Qt sizes them wrong). Hoist the
  children out and apply the `<use>`'s translate by hand.
* `<text>` — convert to paths (Inkscape: Path ▸ Object to Path) so the drawing
  does not depend on the user's fonts.
* `filter`, `mask`, gradients, `<image>` — Qt draws them, but they defeat the
  stroke recolour. Flatten to plain paths.
* `<metadata>`, `sodipodi:`/`inkscape:` namespaces, editor comments — drop for
  size; `scour`/`svgo` do this, but check the diff, they also merge paths and
  can lose the ids we want in §7.4.

### 7.2 Filled → stroked

Most found art is a filled silhouette (C3). Two honest routes:

1. **Outline the fill.** In Inkscape, Path ▸ *Linked Offset* / *Stroke to Path*
   inverted — practically: select the filled shape, set `fill:none`, set
   `stroke:white; stroke-width:2`. If the shape was a compound path built from
   two contours (outer shell + inner cut-out), this gives a usable double line
   for free. If it was a single blob, you get an outline with no interior
   detail — which is often *exactly* what a schematic wants, with the buttons
   re-added as circles.
2. **Trace a raster reference and stroke the trace.** `potrace` (`extra/potrace`
   on Arch, not installed here) on a 300 dpi 1-bit source gives clean centre
   lines; then the same `fill:none; stroke:white` treatment.

Either way the result is *our* drawing derived from theirs, so the source
licence still travels with it — record it in `art/LICENSES.md`.

### 7.3 Colour normalisation

`Callouts.recolor()` only substitutes `#FFFFFF`, `#FFF`, `"white"` and
`: white`. Normalise everything else *once*, at vendoring time:

```sh
sed -i -e 's/currentColor/white/g' \
       -e 's/#fff\b/white/gi' -e 's/#ffffff/white/gi' \
       -e 's/stroke:[^;"]*/stroke:white/g' art.svg
```

Then verify by eye that nothing turned invisible: the file should render as
white-on-transparent and *nothing else*. A quick check:

```sh
rsvg-convert -w 456 -b "#111111" art.svg -o check.png   # any colour left is a bug
```

(Alternatively: teach `recolor()` a `data-hyprpad-theme` attribute convention
instead of forking every file. Out of scope for this note, but it is the
cleaner long-term answer if we ever vendor several third-party drawings.)

### 7.4 viewBox normalisation and anchor extraction

The layout descriptor needs `viewBox` and one `{x, y}` per control **in that
same space**. Both existing layouts use `456 x 320`; keeping that means a new
drawing scales identically in the panel and the callout geometry is comparable.

To re-space a drawing without touching its paths, wrap it:

```xml
<svg viewBox="0 0 456 320" …>
  <g transform="translate(TX,TY) scale(S)">  <!-- original content -->
```

with `S = min(456/W, 320/H)` and `TX/TY` centring the result. Qt renders
nested transforms correctly (§1.2), and the anchors below are then computed in
the *outer* space, which is the only one the layout knows about.

Anchor extraction, cheapest first:

1. **`QSvgRenderer::boundsOnElement()` — use the same renderer the widget uses.**
   If the art has named ids, ~15 lines of Qt gives the whole anchor map, in the
   drawing's own coordinate space, from the renderer that will actually draw it.
   Verified here against cindy's Xbox file:

   ```
   press-dpad          centre=(385.0, 450.3)   A-3   centre=(799.6, 351.9)
   press-stick-left    centre=(247.6, 301.7)   B-3   centre=(873.9, 278.7)
   press-guide         centre=(524.4, 179.7)   X-3   centre=(728.2, 287.3)
   press-view          centre=(446.2, 289.1)   Y-3   centre=(803.7, 213.2)
   ```

   ```cpp
   QSvgRenderer r(path);
   if (r.elementExists(id)) { QRectF b = r.boundsOnElement(id); /* b.center() */ }
   ```

   It resolves ids inside a `display="none"` layer too, so a hidden hotspot
   layer is a *feature*: the anchors exist without drawing anything.
2. **Read the geometry out of the XML.** Works when controls are circles/rects:

   ```python
   from lxml import etree
   t = etree.parse("art.svg")
   for el in t.iter():
       i = el.get("id")
       if i:
           print(i, el.get("cx"), el.get("cy"), el.get("x"), el.get("y"))
   ```

   This is how Valve's `triton.svg` gives up its centres despite having no ids
   at all — the circles carry literal `cx`/`cy` (§4.1).
3. **Bounding boxes from Inkscape.** `inkscape --query-id=<id> --query-x
   --query-y --query-width --query-height file.svg`. (Inkscape is not installed
   on this machine.)
4. **By eye, which is what we did for both current layouts.** Render at
   `456 x 320`, open it, read off coordinates. For ~25 controls it is not
   slower than scripting, and it is what the JSON comments in
   `steam-controller-2026.json` already document.

Expect ids to be *inconsistent* even in good art: cindy names the d-pad
children `top/bottom/left/right`, the face buttons `A-3/B-3/X-3/Y-3`, and
everything else `Vector-49`…`Vector-59`. A small per-file id→control map is
part of the job.

Cross-check by rendering the sheet: a wrong anchor is instantly visible as a
leader line pointing at empty shell.

One legibility note from the survey: found art holds up at ~400 px of drawing
width, but at ~260 px the strokes go thin and the diagram greys out. Scale
`stroke-width` when you renormalise the viewBox — do not reach for
`vector-effect="non-scaling-stroke"`, whose Qt support is not dependable.

### 7.5 Re-verifying the Qt SVG feature matrix

The §1.2 table came from a ~40-line C++ probe: `QSvgRenderer` over a directory
of one-feature-per-file SVGs, each rendered into a 128×128 `QImage` on a dark
ground and montaged into a contact sheet.

```sh
g++ -fPIC t.cpp -o t $(pkg-config --cflags --libs Qt6Svg Qt6Gui Qt6Core)
./t svg/*.svg && magick montage -label '%f' out/*.png -tile 5x3 \
    -geometry 128x128+6+6 -background '#111' -fill white grid.png
```

Do this again before trusting a fancy SVG on a new Qt: the two failure modes
found here (`clip-path`, `<symbol>`) both render *silently wrong*, and a
release note is not a substitute for a picture.

---

## 8. Shortlist and recommendation

### 8.1 Ranked shortlist

**(a) Xbox Elite Series 2 body — the one we most need**

There is **no openly-licensed Elite Series 2 drawing anywhere**. The only hit
in the whole survey (`lemonxah/xbelite2`) has no LICENSE file. But the Elite's
*front face is an Xbox Series pad*: same ABXY diamond, same staggered stick and
d-pad, same View/Menu/Guide, and our four paddles are already `hidden: true` in
the layout because a front view cannot show them. So:

1. **Xelu's `XboxSeriesX_Diagram_Simple`** via `haaldor/Xelu_prompts_SVG`
   (**CC0** — the cleanest licence in the survey, and no trademark to strip
   because the stroke-only filter drops the filled Xbox nexus automatically).
   **Verified here:** I rendered the retargeted file through Qt at `456 x 320`
   and it is exactly what the sheet wants — shell, both bumpers, both sticks,
   d-pad, four face circles, View/Menu/Share, ~2 px effective stroke. Anchors
   come from splitting its 7 paths into 43 subpaths, or by eye.
2. **cindy `xbox-series-gamepad.silhouette.svg`** (Apache-2.0) — richer
   drawing, and anchors *generate* from the `press-*` layer instead of being
   read off. Costs an Apache NOTICE and the licence-scope question.
3. **`Zergatul.Obs.InputOverlay`** (MIT) — white stroke-only already, ids
   `ButtonA`/`DPadUp`; needs the trigger-clip rects removed.
4. Draw our own over **USD936146S1 FIG. 3** (§6.1), which is literally an
   orthographic plan view of the Elite Series 2.

Whichever base: add the Elite's profile-select button between View and Menu,
and keep the four paddles dashed below the grips as the current layout does.

**(b) DualSense body**

1. **cindy `playstation-dualsense-gamepad.silhouette.svg`** (Apache-2.0).
   Verified here: after `sed 's/#1b1b1b/white/'` it is accurate, detailed white
   line art — touchpad, mute, create/options, both sticks with rings, the LED
   strip, the four face symbols. Strip the `ps_button` logo (trademark).
2. **`Zergatul.Obs.InputOverlay`** (MIT) DualSense.
3. **Xelu's `PS5_Diagram_Simple`** (CC0) — the licence-purist option; a
   simpler drawing (no mute button, no face symbols), which our glyph chips
   make up for anyway.
4. **Commons `File:Dualshock 4 Layout.svg`** (Tokyoship, CC BY 3.0) — DS4 not
   DualSense, but the cleanest recolour target found anywhere: three fills and
   one stroke colour.
5. sc-controller `ds5.svg` (CC0) — **not recommended**: the body file has no
   face buttons at all.

**(c) Switch Pro body**

1. **cindy `nintendo-switch-pro/controller.svg`** (Apache-2.0). Strip
   `nintendo-switch-wordmark` and `nintendo-switch-symbol`; the rest is clean
   and has the richest id set of the family (`press-ZL/ZR/L/R/minus/plus/
   capture/sync`).
2. **Ryujinx `Controller_ProCon.svg`** (MIT) — plainer, perfect ids
   (`A_Button`, `Directional_Pad`), 7 CSS classes to rewrite. Fetch from the
   project's Forgejo host; the GitHub mirrors are DMCA'd.

**(c bis) Generic gamepad — for a pad we have no specific art for**

**nicefrog's "Generic Gamepad Template"** (CC0, OpenGameArt) or cindy's
`ultimate-c1` (Apache-2.0). Both are brand-neutral by design, so neither
carries trademark exposure. nicefrog wins on licence, `ultimate-c1` on polish
and anchors.

**(d) A glyph set beyond Kenney — not needed**

Keep Kenney. If we ever do need a second source:

1. **`icculus/ControllerImage`** — `DATA-LICENSE.txt` puts the SVGs in the
   public domain, filenames are SDL gamepad element names, and it has
   `steamcontroller/` and `steamdeck/` sets.
2. **Mr. Breakfast's Free Prompts** (CC0) — flatten the gradients.
3. **PromptFont** (OFL) — only for its unique trackpad-direction glyphs; its
   SVG sources need a fill rewrite before they will theme.

**Bonus, unasked: two more bodies fall out of this survey**

* **Steam Controller (2026)** — `C0rn3j/sc-controller` `images/controller-images/sc2.svg`
  is **CC0**, correct, and carries per-control ids. It is a better bundled
  fallback than our hand-drawn `art/steam-controller-2026.svg`, and it would
  give Steam-less users a decent sheet.
* **Steam Deck** — no good open art. Valve's own is on disk *and* published
  publicly (§4.3) but unlicensed; sc-controller's `deck.svg` is a stylised
  split-halves view. **Draw this one.** Valve's `steamdeckFront.svg` and the
  on-disk `..._steam_deck.svg` are the reference, and the runtime-read route
  works for Deck owners today with one extra colour in `recolor()`.

### 8.2 Attribution we would ship

Add to `shell/hyprpad.cheatsheet/art/LICENSES.md` (and mirror the short form in
`shell/README.md`), per source actually used:

* **cindy** (Apache-2.0) — the licence requires the `LICENSE` text, the `NOTICE`
  contents, and a statement of changes:

  > Controller body outlines for `<devices>` are derived from Cindy
  > (<https://github.com/makecindy/cindy>), Copyright 2026 XD Inc., licensed
  > under the Apache License, Version 2.0. Modified: recoloured to a single
  > stroke colour, viewBox renormalised to 456 × 320, vendor trademarks
  > removed, hotspot layer hidden. A copy of the licence is in
  > `art/cindy/LICENSE`; the upstream NOTICE is in `art/cindy/NOTICE`.

  Vendor `LICENSE` and `NOTICE` verbatim next to the art. Apache-2.0 §4(b)
  requires the modification notice, §4(d) requires carrying NOTICE.

* **sc-controller** (CC0) — attribution is not required; say it anyway:

  > `art/sc-controller/sc2.svg` is derived from sc-controller
  > (<https://github.com/C0rn3j/sc-controller>). All images in that project's
  > `images/` directory are released under CC0 1.0
  > (<https://creativecommons.org/publicdomain/zero/1.0/>) per its
  > `ADDITIONAL-LICENSES` file, a copy of which is in
  > `art/sc-controller/ADDITIONAL-LICENSES`. Modified: fills converted to
  > strokes, colour normalised to white, `<text>` removed, viewBox renormalised.

* **Xelu** (CC0) — if used:

  > Body outline derived from "Free Keyboard and Controllers Prompts" by
  > Nicolae (Xelu) Berbece, <https://thoseawesomeguys.com/prompts/>, released
  > under CC0 1.0. Vector source via <https://github.com/haaldor/Xelu_prompts_SVG>.

* **Kenney** (CC0) — already in `art/LICENSES.md`; no change.

* **Ryujinx / Zergatul** (MIT) — if used, vendor the upstream `LICENSE` file
  and keep the copyright line; MIT requires the notice to travel.

### 8.3 Recommendation

**Yes, somebody already built the collection — twice — and neither is a
drop-in. Take both, and still draw two of them.**

1. **Ship cindy's Xbox Series, DualSense and Switch Pro bodies** (Apache-2.0),
   trademark-stripped, renormalised to our viewBox, anchors generated from the
   `press-*` layer with `boundsOnElement()`. That is three controllers at a
   quality we are not going to hand-draw, plus a logo-free generic pad for the
   "unknown controller" case. **Open the licence-scope issue first** (§2.1).
2. **Replace `art/xbox-elite-2.svg`** with a cindy-derived Xbox Series body
   plus the Elite's profile button. The Elite's front face is a Series pad and
   our paddles are already drawn as hidden/dashed.
3. **Replace `art/steam-controller-2026.svg`** with sc-controller's CC0
   `sc2.svg`, converted to stroke-only. Better art, better licence hygiene than
   a hand drawing, and it keeps the runtime-read Valve preference untouched.
4. **Draw the Steam Deck ourselves**, CC0, from Valve's on-disk SVG and the
   public `steamdeckFront.svg` as reference — and separately add `#8B929A` to
   `recolor()` so Deck owners get Valve's own drawing at runtime, exactly as
   Steam Controller owners get Valve's Triton today.
5. **Keep Kenney for glyphs**, and re-pull the pack to 1.5A (2026-07-11) —
   it now covers Steam Frame, Switch 2, Index and Quest, which is the whole of
   our medium-term device list.
6. **Do not** touch Valve's, Microsoft's, Sony's or Nintendo's art beyond
   reading it at runtime from the user's own install. §4.3 is unambiguous.

If the licence-scope question on cindy comes back badly, the fallback ladder is
Xelu (CC0, Xbox + PlayStation, no Nintendo, no Valve) → Zergatul/Ryujinx (MIT)
→ draw our own over the design-patent plan views (§6.1). We are never stuck;
we are only ever slower.

---

## 9. Sources

**Body art**

* cindy design-rules silhouettes — <https://github.com/makecindy/cindy>
  (`docs/design-rules/xbox-series-gamepad.silhouette.svg`,
  `…/playstation-dualsense-gamepad.silhouette.svg`,
  `…/gamepads/nintendo-switch-pro/controller.svg`,
  `…/gamepads/ultimate-c1/controller.svg`,
  `…/gamepads/switch-joy-con/controller.svg`,
  `…/gamepad-silhouette-authoring.md`)
* sc-controller (C0rn3j fork) — <https://github.com/C0rn3j/sc-controller>
  (`images/controller-images/{sc2,sc,deck,ds5}.svg`, `ADDITIONAL-LICENSES`)
* Xelu's Free Keyboard and Controllers Prompts —
  <https://thoseawesomeguys.com/prompts/> · SVG export:
  <https://github.com/haaldor/Xelu_prompts_SVG>
* nicefrog, Generic Gamepad Template —
  <https://opengameart.org/content/generic-gamepad-template>
* qubodup, Game Controller Outline White —
  <https://openclipart.org/detail/212651>
* Wikimedia Commons — `File:Xbox Controller.svg` (Jishenaz, CC0),
  `File:Dualshock 4 Layout.svg` (Tokyoship, CC BY 3.0),
  `File:Xbox360 gamepad.svg` and `File:PlayStation 3 gamepad.svg` (Grumbel,
  CC0 via PD-OpenClipart), `File:360 controller.svg` (Alphathon, CC BY-SA 3.0 +
  GFDL — **do not use**)
* Zergatul.Obs.InputOverlay — <https://github.com/Zergatul/Zergatul.Obs.InputOverlay>
* Ryujinx `Controller_ProCon.svg` — project Forgejo host (GitHub mirrors DMCA'd)
* The Noun Project — <https://thenounproject.com/icon/dualsense-4574710/>
* AL2009man/Gamepad-Asset-Pack — **rejected**, author states assets were ripped
  from official sources

**Glyphs**

* Kenney Input Prompts — <https://kenney.nl/assets/input-prompts> (1.5A,
  2026-07-11)
* icculus/ControllerImage — <https://github.com/icculus/ControllerImage>
  (`DATA-LICENSE.txt`)
* Mr. Breakfast's Free Prompts — <https://mrbreakfastsdelight.itch.io>
* PromptFont — <https://codeberg.org/shinmera/promptfont> (the GitHub repo is
  archived) · <https://shinmera.com/promptfont>
* Qlementine Icons — <https://github.com/oclero/qlementine-icons>
* Fluent UI System Icons, Iconoir, Tabler, Material Symbols, Bootstrap Icons,
  Simple Icons, Font Awesome Free, game-icons.net, OpenMoji, Twemoji

**Valve, vendors, law**

* Steam Subscriber Agreement — <https://store.steampowered.com/subscriber_agreement/>
* Steamworks Steam Deck SVG page —
  <https://partner.steamgames.com/doc/steamhardware/steamdeck/svg>
* Steamworks branding —
  <https://partner.steamgames.com/doc/marketing/branding>
* `ISteamInput::GetGlyphSVGForActionOrigin` — Steamworks Steam Input docs
* Microsoft trademark and brand guidelines —
  <https://www.microsoft.com/en-us/legal/intellectualproperty/trademarks>
* Sony Interactive Entertainment terms —
  <https://sonyinteractive.com/en/terms-of-service/>
* Nintendo copyright/permissions (UK) — nintendo.com legal information
* USPTO terms of use — <https://www.uspto.gov/terms-use-uspto-websites>
* USD936146S1 (Xbox Elite Series 2) —
  <https://patents.google.com/patent/USD936146S1/en>
* USD905166S1 (Xbox Series family) —
  <https://patents.google.com/patent/USD905166S1/en>

**In this repo**

* `shell/hyprpad.cheatsheet/art/LICENSES.md` — the provenance policy this note
  extends
* `shell/hyprpad.cheatsheet/Callouts.js` (`recolor()`), `Sheet.qml`,
  `Panel.qml`, `layouts/steam-controller-2026.json`
* `docs/research/xbox-elite.md` §"art" — the original stopgap decision

## 10. What could not be verified

* **cindy's licence scope.** The repo `LICENSE` is Apache-2.0 with no carve-out,
  but the README's grant wording says "the source code in this repository", and
  the SVGs live under `docs/`. Nobody asked XD Inc. Also unverified: whether XD
  Inc. holds clear rights to every silhouette it published.
* **sc-controller's trace provenance.** `tools/gen_sc2_image.py` traced the
  2026 controller from *something* undocumented. CC0 clears copyright, not
  trade dress.
* **Xelu's Steam Deck glyphs and the COCOGOOSE font.** haaldor's README warns
  the Deck glyphs use a font that is free for personal use only; the font's
  terms check out, but that Xelu actually used it was not confirmed. It does not
  affect the body diagrams.
* **PromptFont's licence metadata.** `package.json` claims
  `"(OFL-1.1 OR zlib)"`, itch claims zlib/CC0, and no zlib or CC0 text exists in
  the repo. The LICENSE is OFL boilerplate with the copyright header missing.
  Assume OFL.
* **Whether Valve grants any redistribution right for its on-disk glyph
  files.** No document was found either way; the SSA's default is no.
* **`gitlab.steamos.cloud`** (the 2026 controller CAD) sits behind an anti-bot
  wall. Its "CC BY-NC-SA 4.0" designation is second-hand press; Valve's own
  wording is only "a Creative Commons license". Moot — it is 3D geometry.
* **Figma Community, Flaticon, Vecteezy, publicdomainvectors, svgrepo** — all
  bot-gated (403/429/Cloudflare). Their terms here come from search-surfaced
  quotes of their own pages, not first-party reads. Treat the exact wording as
  indicative.
* **The Sony (DualSense) and Nintendo (Switch Pro) design patent numbers.**
  The Nintendo hits found were the *Wii U* Pro Controller. Five minutes on
  Google Patents at drawing time will close this.
* **Qt 5 behaviour.** Everything in §1.2 was measured on Qt 6.11.2 only.
