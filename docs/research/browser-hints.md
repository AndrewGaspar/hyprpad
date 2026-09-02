# Browser link hints and tab switching from the controller

Research for: (1) a Vimium-style **"press a button, then 1–3 buttons to whack a
hyperlink"** flow from the Steam Controller, contextual on hyprpad's modes, with
the capital-`F` "open in a new tab" variant; and (2) **switching browser tabs**
with a controller chord that does not collide with the Hyprland *group* tabbing
already on `guide+l2`/`guide+r2`.

Everything below is marked **VERIFIED** (read from source at a cited line, or
observed read-only on this machine on 2026-09-01), **INFERRED** (follows from
verified facts, or from documentation not re-checked against source), or
**UNVERIFIED** (needs a live test that this research was not allowed to run —
no browser was launched and nothing under `~/.config` was touched).

Sources of truth:

- hyprpad: this repo at `e2ddad0` (`Merge branch 'feat-mouse-bindings'`). Line
  cites are `src/…` relative to the repo root.
- The compositor: the owner's HypXRland fork at `/home/ajg/code/Hyprland`
  (`7b7e193`, read-only).
- Vimium: the copy actually installed in the owner's browser, **2.4.2**, at
  `~/.config/google-chrome/Default/Extensions/dbepggeogbaibhgnhhndojpepiihcmeb/2.4.2_0/`
  (VERIFIED, `manifest.json:44`). GitHub tag `v2.4.2` (2026-03-07) is the newest
  release, so local line numbers are current. Cited below as `vimium/<path>:<line>`.
- Live session facts: `xdg-settings`, `hyprctl clients -j`, the flags files, the
  Omarchy native-messaging hosts — all read-only.

---

## 0. Recommendation in one paragraph

Do tabs first, because they may cost **zero hyprpad code**: the fork exposes
Hyprland's `sendshortcut` as `hl.dsp.send_shortcut({ mods = "CTRL", key = "Tab" })`
(VERIFIED in the fork's Lua bindings), which the existing `h.dispatch` action can
already send; bind it to `guide+dpad_left/right` guarded `:only_in("browser")`
(both chords are free) and verify live with one `hyprctl dispatch` call. Hints
need Vimium exactly as installed plus a `browser` mode, a rule-less manual
`hints` mode, and a small set of hyprpad additions: **letters in the key table**
(today it has none), **a chord that taps a key** (today a `h.key` on a chord is
a documented no-op), **modifier combos** (`shift+f`), **a two-step action**
(`type f, then set_mode hints`), and **a self-clearing manual mode** (so the
daemon leaves `hints` without a signal Vimium cannot give). Pick the hint
alphabet so the labels *name the buttons* — Vimium uppercases labels, so with
`linkHintCharacters = "axynswe…"` the A/X/Y buttons' hints literally read `A`,
`X`, `Y` and the D-pad reads compass letters — which makes glyph rendering a
polish item, not a prerequisite. Glyphs come later via Vimium's own custom-CSS
hook (hints are plain page DOM, so an `@font-face` works in principle; two
caveats to test). A companion extension is the only way to get a real
"hints are up / hints are down" signal; build it as a *DOM observer* of Vimium's
documented hint container rather than a fork, and only if the self-clearing mode
proves annoying in practice.

---

## 1. What is on this machine

| fact | value | status |
|---|---|---|
| Default browser | `google-chrome.desktop` (`xdg-settings get default-web-browser`) | VERIFIED |
| Browsers installed | `google-chrome-stable`, `chromium`; no Firefox, Brave, Zen or Vivaldi in `/usr/bin` | VERIFIED |
| Vimium | 2.4.2 in Chrome's `Default` profile; **not** in Chromium's profile (`~/.config/chromium/Default/Extensions/` has no `dbepggeo…`) | VERIFIED |
| Chrome runs native Wayland | `~/.config/chrome-flags.conf` and `chromium-flags.conf`: `--ozone-platform=wayland` (Omarchy's stock `/usr/share/omarchy/config/chromium-flags.conf:1-2`) | VERIFIED |
| Chrome's window class | `google-chrome` (live `hyprctl clients -j`) | VERIFIED |
| Omarchy web-app windows | `chrome-<host>__-Default`, e.g. `chrome-x.com__-Default`, `chrome-web.whatsapp.com__-Default` (live) | VERIFIED |
| Key auto-repeat | `repeat_delay = 600`, `repeat_rate = 40` (`~/.config/hypr/input.lua:22`, `input.conf:14-15`) | VERIFIED |
| Native-messaging precedent | Omarchy already installs hosts for Chrome/Chromium/Brave: `~/.config/google-chrome/NativeMessagingHosts/com.omarchy.copy_url.json` → `/usr/share/omarchy/bin/omarchy-chromium-copy-url-host`, a 50-line bash script reading a 4-byte little-endian length + JSON from stdin | VERIFIED |

The web-app windows matter: they are Chrome, Vimium's content scripts run in
them (`manifest.json:14,21` — `all_frames: true`, no host restriction), and the
owner has several open. The `browser` mode's predicate should match both
spellings (§5.1).

---

## 2. How Vimium hints work

### 2.1 Commands and entry points

Default key map (`vimium/background_scripts/commands.js:440-443`), VERIFIED:

| key | command | what it does |
|---|---|---|
| `f` | `LinkHints.activateMode` | hints; the chosen link opens **in the current tab** |
| `F` | `LinkHints.activateModeToOpenInNewTab` | hints; opens **in a new *background* tab** |
| `<a-f>` | `LinkHints.activateModeWithQueue` | hints stay up; open many links in background tabs |
| `yf` | `LinkHints.activateModeToCopyLinkUrl` | copy the link's URL |

Every hint variant is one `LinkHintsMode` with a different *mode object*
(`vimium/content_scripts/link_hints.js:76-159`). The full set, with the HUD
indicator text each shows (VERIFIED):

| mode | HUD indicator | default key | notes |
|---|---|---|---|
| `OPEN_IN_CURRENT_TAB` | "Open link in current tab" | `f` | |
| `OPEN_IN_NEW_BG_TAB` | "Open link in new tab" | `F` | **background** tab — the surprise in §2.7 |
| `OPEN_IN_NEW_FG_TAB` | "Open link in new tab and switch to it" | *(unbound)* | `LinkHints.activateModeToOpenInNewForegroundTab` (`all_commands.js:198`) |
| `OPEN_WITH_QUEUE` | "Open multiple links in new tabs" | `<a-f>` | |
| `COPY_LINK_URL` | "Copy link URL to Clipboard" | `yf` | |
| `OPEN_INCOGNITO` | "Open link in incognito window" | *(unbound)* | `activateModeToOpenIncognito` |
| `DOWNLOAD_LINK_URL` | "Download link URL" | *(unbound)* | `activateModeToDownloadLink` |
| `COPY_LINK_TEXT` | "Copy link text" | *(unbound)* | |
| `HOVER_LINK` | "Hover link" | *(unbound)* | |
| `FOCUS_LINK` | "Focus link" | *(unbound)* | |

Anything unbound can be bound in Vimium's *Custom key mappings* box
(`map <key> LinkHints.activateModeToOpenInNewForegroundTab`).

### 2.2 Alphabet mode: how the codes are generated

Settings (`vimium/lib/settings.js:14-16`), VERIFIED:

```js
linkHintCharacters: "sadfjklewcmpgh",   // home-row-ish, 14 chars
linkHintNumbers:    "0123456789",
filterLinkHints:    false,
```

The options page labels these "Characters used for link hints" and "Numbers used
for link hints" (`vimium/pages/options.html:110-124`).

With `filterLinkHints = false` (the default) the matcher is `AlphabetHints`
(`link_hints.js:384`). It lower-cases the setting and requires more than one
character (`:802-807`). The generator (`:831-849`, VERIFIED, quoted):

```js
hintStrings(linkCount) {
  let hints = [""];
  let offset = 0;
  while (((hints.length - offset) < linkCount) || (hints.length === 1)) {
    const hint = hints[offset++];
    for (const ch of this.linkHintCharacters) {
      hints.push(ch + hint);
    }
  }
  hints = hints.slice(offset, offset + linkCount);
  // Shuffle the hints so that they're scattered; hints starting with the same character and short
  // hints are spread evenly throughout the array.
  return hints.sort().map((str) => str.reverse());
}
```

This is a breadth-first expansion of a *k*-ary tree: each step replaces one leaf
with *k* longer leaves (net +*k*−1), shallow leaves are expanded before deep
ones, and it stops once there are at least *n* leaves. Consequences
(INFERRED from the code):

- The codes are **prefix-free** (a code is never the start of another), so a
  link fires the instant its last character is typed — no Enter.
- **The longest code has exactly ⌈log<sub>k</sub> n⌉ characters**, and codes
  are of *mixed* length — the comment says so ("may be of different lengths",
  `:827-829`). With *k* = 9 and *n* = 30: 6 links get one-character codes and
  24 get two.
- Labels are rendered **upper-case** (`marker.element.innerHTML =
  spanWrap(marker.hintString.toUpperCase())`, `:820`), one `<span>` per
  character (`spanWrap`, `:1060-1066`).
- Typed characters are lower-cased before matching (`:560-563`), so case does
  not matter on the way in; a typed key that leaves zero matching hints
  **cancels the mode** (`updateKeyState`: `linksMatched.length === 0` →
  `deactivateMode()`, `:596-601`); exactly one match **activates the link**
  (`:602`).
- Duplicate characters in `linkHintCharacters` are not rejected and would
  produce duplicate codes (INFERRED; the generator has no dedup).

Capacity table (⌈log<sub>k</sub> n⌉ presses), for alphabets the controller can
supply (§4.1):

| alphabet size *k* | 1 press | ≤ 2 presses | ≤ 3 presses | presses for 30 / 100 / 300 / 1000 links |
|---|---|---|---|---|
| 7 (A X Y + D-pad) | 7 | 49 | 343 | 2 / 3 / 3 / 4 |
| 9 (+ bumpers) | 9 | 81 | 729 | 2 / 3 / 3 / 4 |
| 13 (+ four grips) | 13 | 169 | 2197 | 2 / 2 / 3 / 3 |
| 15 (+ stick clicks) | 15 | 225 | 3375 | 2 / 2 / 3 / 3 |
| 14 (Vimium's default) | 14 | 196 | 2744 | 2 / 2 / 3 / 3 |

Ordinary pages have 50–300 clickable things, so the owner's "1 to 3 at most" is
met by *any* of these; 13+ keeps most pages at two presses.

### 2.3 Filter mode

`filterLinkHints = true` ("Use the link's name and characters for link-hint
filtering", `options.html:140-141`) switches to `FilterHints` (`:869-`): hints
are **numbered** in `linkHintNumbers` (`generateHintString`, `:889-893`,
sequential base-10 so ~2–3 digits for most pages), and any *other* typed
character filters by link text. From a controller that is strictly worse:
sequential numbers are longer than prefix-free codes, and there is no way to
type link text. **Leave it off.**

### 2.4 Keys while hints are up (all VERIFIED, `link_hints.js`)

| key | behaviour | line |
|---|---|---|
| **Escape** | mode exits, `exitOnEscape: true`; the coordinator is told `isSuccess: false` | `:394`, `:400-405` |
| **Backspace** | pops the last typed character; **exits** when nothing is typed | `:536-546` |
| **any click** | exits (`exitOnClick: true`) | `:395`, `:400` |
| **Shift** (held, alphabet mode) | toggles current-tab ↔ new-background-tab *for as long as it is held* | `:505-535` |
| **Control** (held) | toggles new-foreground ↔ new-background | same |
| **Enter** | activates the "active" hint — filter mode only | `:547-552` |
| **Space** | rotates overlapping hints | `:559` |
| **key repeat** | `if (event.repeat) return;` — auto-repeat is ignored | `:502` |

The repeat line matters for hyprpad: `drive_buttons` holds the key down for as
long as the button is held (`src/run.rs:1180,1196-1210`), and Hyprland starts
repeating after 600 ms. Inside hint mode the repeats are harmless. Only a
button still held after Vimium has *left* hint mode would type repeats into the
page.

Vimium is inert on `chrome://` pages, the Web Store, the built-in PDF viewer,
the New Tab page and any excluded site (Gmail is excluded by default,
`settings.js:36-40`), and it treats keys as text while an input is focused
(insert mode). In all of those, `f` does nothing — which is why the hyprpad side
needs an exit that does not depend on hints having appeared (§5.3).

### 2.5 Rendering, and the custom-CSS hook

Hints are **ordinary page DOM, not shadow DOM and not an iframe** (VERIFIED):
a `div#vimium-hint-marker-container.vimium-reset` is appended to
`document.documentElement` (`link_hints.js:410-417`), each marker is a
`div.vimium-reset.internal-vimium-hint-marker.vimiumHintMarker` (`:481-486`)
positioned absolutely, its text one `<span class='vimium-reset'>` per character
(`:1060-1066`), and `removeHintMarkers` detaches the container on exit
(`:791-796`). (The Vomnibar/HUD are the things in iframes inside a shadow root
— `ui_component.js:57-62` — not the hints.)

The stylesheet contract is explicit (`vimium/content_scripts/vimium.css:83-98`):
`vimiumHintMarker`, `matchingCharacter` and `vimiumActiveHintMarker` are
"user-facing and should not be changed". The default internal style
(`:105-135`) sets `font-family: Helvetica, Arial, sans-serif; font-weight: bold;
font-size: 11px` on `div.internal-vimium-hint-marker span`.

The user's CSS — the "CSS for Vimium UI" textarea, setting
`userDefinedLinkHintCss` (`options.html:224-228`; default value at
`settings.js:19-35`, which styles `div > .vimiumHintMarker`, `div >
.vimiumHintMarker span`, and `.matchingCharacter`) — is injected with
`chrome.scripting.insertCSS({ css: … })` into **every frame** on navigation
(`background_scripts/main.js:493-500`) and at extension start (`:883-887`,
*after* Vimium's own CSS, so equal-specificity user rules win). It is stored in
`chrome.storage.sync` (`settings.js:107,125,250`), which caps a single item at
8 KB (INFERRED from Chrome's storage docs) — the ceiling on any font embedded
in it (§4.2).

### 2.6 Tab commands

Defaults (`commands.js:466-479`, VERIFIED) — all single keys or two-key
sequences, several needing Shift:

| key | command | | key | command |
|---|---|---|---|---|
| `K`, `gt` | `nextTab` | | `J`, `gT` | `previousTab` |
| `^` | `visitPreviousTab` | | `g0` / `g$` | `firstTab` / `lastTab` |
| `t` | `createTab` | | `yt` | `duplicateTab` |
| `x` | `removeTab` | | `X` | `restoreTab` |
| `T` | `Vomnibar.activateTabSelection` (search open tabs) | | `W` | `moveTabToNewWindow` |
| `<<` / `>>` | `moveTabLeft` / `moveTabRight` | | `<a-p>` / `<a-m>` | pin / mute |

Two facts shape §6: **`x` closes the tab** — a bare lowercase letter, so it is
the most dangerous stray key on the controller; and every tab command that a
single unshifted button could type is either a prefix (`g…`, `y…`) or a
destructive single letter (`t`, `x`). Vimium is not the right layer for tab
switching from the pad; Chrome's own accelerators are (§6).

### 2.7 The one surprise: `F` opens a *background* tab

`activateModeToOpenInNewTab` uses `OPEN_IN_NEW_BG_TAB` (`link_hints.js:339-341`):
the new tab opens behind the current one. If the owner's intent is "open it and
look at it", the command to type is `LinkHints.activateModeToOpenInNewForegroundTab`
(`:342-344`), which has no default key — `map` one in Vimium (any key hyprpad
can type; see §5) rather than emulating a held Control.

---

## 3. What hyprpad can do today

VERIFIED against `e2ddad0`:

| capability | state | where |
|---|---|---|
| Bare button → **one evdev keycode**, held while the button is held | yes | `Action::Key(u16)` `src/config.rs:125`; `drive_buttons` `src/run.rs:1196-1210` |
| Key names hyprpad can type | **arrows, enter, backspace, space, tab, escape, back, home, end, pageup, pagedown; mouse buttons** — **no letters, digits or F-keys** | `key_code` `src/config.rs:329-351` |
| Modifier combos (`shift+f`, `ctrl+tab`) | **no** — `Action::Key` carries one code; the uinput device only ever sees `key(code, pressed)` | `src/keyboard.rs:122-128` |
| `h.key` on a **guide chord** | **no-op**, by design: "a `key` bound to a guide chord no-ops" | `execute` `src/run.rs:2196` |
| `h.button` value | **must be `h.key`/`h.mouse`**; `h.set_mode`/`h.clear_mode`/`h.exec` on a bare button are rejected at load | `src/lua_config.rs:1026-1030` |
| One binding → **one** action | yes; no sequence/compound action exists | `value_to_action` `src/lua_config.rs:1429-1470` |
| Same button, different key per mode | **yes** — a second `h.button` on a bound button becomes a `ButtonAlt`; `buttons_in` resolves base then alts, first passing guard wins | `src/config.rs:690-700,1418-1425`; `src/lua_config.rs:1031-1045` |
| Mode with **no rule**, reachable only by `h.set_mode` | **yes** — `ModeDef.rule: None` is "reachable only as `default_mode` or via a manual override"; rule-less modes never match by themselves | `src/config.rs:659-671`; `src/mode.rs:476-483` |
| Manual override sticks across focus changes until `clear_mode` | yes (and is dropped on reload if the new config lacks the mode) | `src/mode.rs:301-333`; test `:608-627` |
| Guide chords | `guide+<button>` (every button except Steam, the capacitive pads and pad *touch*), `guide+stick_<dir>` / `guide+lstick_<dir>`, `guide_tap`, `guide_hold`; **no non-guide chords** | `src/gesture.rs:211-217`; `src/config.rs:228-275` |
| Hyprland dispatch | `h.dispatch "<lua expr>"` → socket `dispatch <expr>`; the fork evaluates `return hl.dispatch(<expr>)` and **calls** the dispatcher closure | `src/hypr.rs:149-155`; fork `src/debug/HyprCtl.cpp:1159-1174`, `src/config/lua/bindings/LuaBindingsToplevel.cpp:352-375` |
| Context a rule can see | `ctx.focus.{class,title,pid,fullscreen}`, `ctx.focus:process_tree_has()`, `ctx.layers:has()/list()` | `src/lua_config.rs:231-290` |
| Re-resolve on the focused window's title change | yes, default on (`rescan_on_title_change`) — but a manual override still wins | `src/config.rs:759-766`; `src/mode.rs:476-483` |
| A control channel other than SIGHUP | **none** — `hyprpad reload` sends SIGHUP via the pidfile | `src/main.rs:10-12`; `src/run.rs:171-195` |

The proven reference for typing with a modifier already exists in the repo: the
on-screen keyboard's `VirtualKeyboard::tap(code, shift)` presses `KEY_LEFTSHIFT`
around the key (`osk/src/output.rs:136-150`).

---

## 4. Mapping hints to the controller

### 4.1 Option A — Vimium as-is, plus a hyprpad `hints` mode

The flow: a chord types `f` (or `F`) and puts the daemon into a `hints` mode;
in that mode the bare buttons emit the letters of an alphabet that is also
Vimium's `linkHintCharacters`; the link opens on the last letter.

**Which buttons, which letters.** Vimium shows the letters in upper case, so the
cheapest possible "glyph" is to *choose letters that are the button names*:

| button | letter | label reads | stray-key safety in Vimium normal mode (§2.6) |
|---|---|---|---|
| A | `a` | **A** | unbound — safe |
| X | `x` | **X** | **`x` = close tab** — must `unmap x` in Vimium, or use a different letter until glyphs exist |
| Y | `y` | **Y** | prefix of `yy`/`yf`/`yt` — a pending prefix, harmless |
| D-pad up / down / left / right | `n` `s` `w` `e` | **N S W E** (compass) | `n` = find-next (harmless with no search), `s` `w` `e` unbound |
| L1 / R1 | `q` `c` | **Q C** | unbound |
| L4 / L5 / R4 / R5 (grips) | `1` `2` `3` `4` | **1 2 3 4** | digits are a count prefix — harmless |
| L3 / R3 (stick clicks) | `5` `6` | **5 6** | harmless |
| B | — | — | **reserved: cancel** (hyprpad's "B means back" convention, docs/research/omarchy-menu-navigation.md) |
| R2 / right-pad click | — | — | keep as **mouse click**: a click cancels Vimium hints too (§2.4) |

That is *k* = 7 with the face buttons and D-pad, 9 with the bumpers, 13 with the
grips, 15 with the stick clicks (capacities in §2.2). Letters bound in Vimium's
normal mode and *not* in this table (`j k h l d u r p i v f o b t m`) are
deliberately avoided so that a letter arriving after Vimium has already left
hint mode does nothing worse than a pending prefix. (`x` is the one exception
worth keeping for the label; `unmap x` costs one line in Vimium and `X` still
restores.)

**Why this is enough for phase 1.** The labels are legible mnemonics for every
button, not glyphs; the HUD says which mode is up ("Open link in current tab.",
`link_hints.js:466-471`); and the bar widget shows `hints` while the daemon is
in the mode (README, *The bar widget*), so the user can always see the state.

**What hyprpad cannot know: when it is over.** Vimium sends nothing outside the
page. The candidates, honestly:

| exit signal | works when | cost | verdict |
|---|---|---|---|
| **B** (types Escape *and* clears the mode) | always; two things on one press | needs a compound action or a self-clearing mode (§5.3) | **yes — the explicit exit** |
| any **non-hint button** (a click, a trigger, the pads) | user clicks instead | same machinery | yes, as a rule of the transient mode |
| **after the maximum hint length** (3 presses) | always, as a cap | trivial counter | yes, as the **safety net** — but not alone: a page with ≤ *k* links needs one press, and presses 2–3 would then be strays |
| **timeout** (e.g. 4 s idle) | user wandered off | timer | optional; Vimium itself has no timeout |
| **focus / title change** | the link was followed in the current tab (title changes → `windowtitle` event, which the daemon already re-resolves on); a foreground tab opened; another window focused | make the override *transient* so a context change drops it | **yes** — the natural exit for the `f` case; does nothing for `F` (background tab keeps the title) or an in-page anchor |
| watch the page for the hint container | the only *true* signal | a companion extension (§4.3) | later, if the above proves annoying |

None of these is airtight alone; **B + a 3-press cap + drop-on-context-change**
together leave only "opened a background tab, then pressed nothing" as a state
where the daemon lingers in `hints` — and there the next press is a mnemonic
letter into a page where it is a no-op, then the cap fires. Acceptable.

**A variant that needs no mode at all — rejected.** Holding guide and chording
the letters (`guide+a`, `guide+x`…, release guide = Escape) would make the
guide release the exit signal, for free. But the Steam button and the face
buttons are both right-thumb controls on this pad, and the D-pad is the other
thumb: holding guide through a two-letter code is physically awkward, and a
`GuideLeave{was_chorded:true}` carries no binding today (`src/config.rs:245`).
Not pursued.

### 4.2 Option A′ — glyphs, through Vimium's custom CSS

Because hints are page DOM styled by a user stylesheet (§2.5), a font can turn
the letters into controller glyphs with no extension code:

```css
@font-face {
  font-family: "hyprpad-hints";
  src: url(data:font/woff2;base64,…) format("woff2");   /* glyphs at U+0041 'A', 'X', 'Y', 'N', … */
}
div > .vimiumHintMarker span {
  font-family: "hyprpad-hints", Helvetica, Arial, sans-serif;
  font-size: 14px;
}
```

- The label is upper-cased (§2.2), so the font needs glyphs only at the
  upper-case code points of the alphabet — 7–15 glyphs. The cheat sheet already
  has the artwork (`shell/hyprpad.cheatsheet/art/`); a subset WOFF2 of that
  many simple outlines is a few KB, and base64 must fit in the ~8 KB
  `chrome.storage.sync` item with the rest of the CSS (INFERRED — the quota is
  Chrome's documented `QUOTA_BYTES_PER_ITEM`, not tested here).
- The selector `div > .vimiumHintMarker span` is the one Vimium's own default
  custom CSS uses, so it is stable by contract.
- **UNVERIFIED, must test first:** whether a `data:` font referenced from
  extension-injected CSS loads on pages with a strict `font-src` CSP
  (github.com is a good probe — its CSP has no `data:` in `font-src`). If the
  page CSP applies, the glyphs silently fall back to Helvetica letters on those
  sites — which is exactly the phase-1 state, so the failure mode is benign.
- CSS cannot select on text content, so **without a font there is no
  per-letter styling**; colour/shape per button is not possible this way.

Building the font is a one-off: SVG per glyph → `fontforge`/`fonttools`
(`svg2ttf` + `pyftsubset --flavor=woff2`) → base64 → paste. Half a day
including the CSP probe.

### 4.3 Option B — a companion extension (observer, not fork)

Vimium is MIT, and a fork could render glyphs in `fillInMarkers`
(`link_hints.js:820`) and signal state from `renderHints`/`deactivateMode`
(`:409`, `:786`). **Don't fork.** Two facts make a *separate* companion
extension cheaper and unbreakable by Vimium releases:

1. Vimium's hint DOM is a documented, user-facing contract (§2.5): a
   `MutationObserver` on `document.documentElement` sees
   `#vimium-hint-marker-container` appear and disappear — that *is* the
   "hints up / hints down" signal — and can rewrite each marker's `<span>`
   text into a glyph (inline SVG, or a PUA character in an extension-packaged
   font, which sidesteps the CSP question because `chrome-extension://` fonts
   listed in `web_accessible_resources` are the extension's own).
2. Chrome's native messaging (INFERRED from the Chrome docs; the exact same
   framing Omarchy's host uses, VERIFIED locally): the extension does
   `chrome.runtime.connectNative("com.hyprpad.hints")`, Chrome launches the
   host and speaks length-prefixed JSON over stdio. Manifest at
   `~/.config/google-chrome/NativeMessagingHosts/com.hyprpad.hints.json`
   (`~/.config/chromium/…` for Chromium, `~/.config/BraveSoftware/Brave-Browser/…`
   for Brave — Omarchy's `omarchy-install-chromium-copy-url` already loops over
   all of them). **The host is always launched by the browser**, never the
   reverse, so it is the host that must reach hyprpad.

Which exposes the real cost: **hyprpad has no control channel** (§3, SIGHUP
only). The companion needs one — `$XDG_RUNTIME_DIR/hyprpad/control.sock` with
`mode set hints` / `mode clear`, or a `hyprpad mode …` subcommand that speaks
it. That is a daemon feature in its own right (~a day), useful beyond hints (a
`hyprpad mode set game` from a script, for one), and it is what makes the
extension's "hints are up" message *arrive* as a mode transition rather than a
guess. The extension itself is ~150 lines (manifest v3, one content script with
the observer, one background script holding the native port), loaded unpacked
in developer mode (the `--load-extension` flag Omarchy uses in
`chrome-flags.conf` is honoured by Chromium; whether current *Google Chrome*
stable still honours it is INFERRED-negative and worth checking at
`chrome://extensions` before relying on it). Per-browser install is the same
three-file drop Omarchy does.

Maintenance, honestly: the DOM contract has been stable since Vimium's hints
were rewritten and is documented as such, so the observer is low-churn; the
native host protocol is frozen; the hyprpad socket is ours. What *does* rot is
the unpacked-extension install (profile migrations, Chrome's developer-mode
nag) and the fact that it is one more thing to install on the living-room
tower. It is the only route to a true signal and to glyphs that cannot be
blocked by a page CSP — so it is phase 3, gated on the transient mode (§5.3)
actually being irritating in daily use.

### 4.4 Option C — hyprpad draws its own overlay

No. hyprpad can see the focused window's class, title and pid and the layer
set; it cannot see link rectangles inside a browser. Drawing glyphs would
require the same in-page script as option B, at which point B is simply better.

### 4.5 Browsers

| browser | hints | tabs via compositor (§6) | notes |
|---|---|---|---|
| Google Chrome (the default here) | Vimium 2.4.2, installed | yes | everything above is verified against this copy |
| Chromium | Vimium not installed in its profile | yes | same extension id from the Web Store; Omarchy's flags file is shared |
| Brave | Vimium (Chromium extension) | yes | class `brave-browser` (INFERRED) |
| Firefox | **Vimium-FF 2.4.2** on AMO (same codebase, same author) | yes | not installed here |
| Firefox + Tridactyl | `hintchars` (default `hjklasdfgyuiopqwertnmzxcvb`, "used preferentially from left to right"), `hintnames = short|numeric|uniform|words`, `hintuppercase`, `hintfiltermode`; `f` = `hint`, `F` = `hint -b` | yes | Its native messenger exists for `:editor`, `:source`, rc files and `about:` pages — **not** for outside-in control: "Control Tridactyl via CLI through native messenger" (tridactyl#780) has been open since 2018-07-09 at P4. So Tridactyl is more scriptable *inside* the page but offers hyprpad nothing Vimium does not, and Firefox is not installed. Firefox-only. |

### 4.6 Prior art: Steam's own browsers

Valve's answer to "click a link with a gamepad" has always been a **cursor**,
never labels (INFERRED from Steam's Big Picture page and community guides; no
source is available):

- The original Big Picture browser (2012–2022) was "reticle-based": a reticle
  fixed at screen centre with the page panning under the stick, **Y cycling
  three zoom levels**, X opening an options menu, the right trigger clicking.
  Steam Controller users reported the right pad acting as a mouse when it
  worked at all.
- The current Big Picture (the Deck UI) has no standalone browser; the overlay
  browser is driven by the right trackpad as a mouse with the triggers as
  buttons — the same thing hyprpad's right pad already does on the desktop.

Nothing to borrow beyond "a button that cycles zoom" — which, if wanted, is
`ctrl+=`/`ctrl+-`/`ctrl+0` through the same `send_shortcut` path as tabs.

---

## 5. The hyprpad side

### 5.1 The `browser` mode

Rules run in definition order, first match wins, so the mode goes **after
`game` and before `desktop`** in `config/hyprpad.lua`. The predicate API is a
Lua function over `ctx` (`src/lua_config.rs:231-290`); class matching is plain
Lua pattern matching on `ctx.focus.class`, exactly as the `game` rule does it
(`config/hyprpad.lua:61-68`):

```lua
-- A browser: Chrome/Chromium/Brave/Firefox, and Omarchy's Chrome web apps
-- (class `chrome-<host>__-Default`, VERIFIED live) — Vimium runs in those too.
h.mode("browser").when(function(ctx)
  local c = ctx.focus.class:lower()
  return c == "google-chrome" or c == "chromium" or c == "firefox"
      or c:match("^brave") ~= nil
      or c:match("^chrome%-") ~= nil
end)

-- Link hints. No rule: reachable only through h.set_mode, and exclusive like
-- every mode, so the desktop's A = Enter and B = Backspace are simply not live
-- here (src/config.rs:659-671).
h.mode("hints")
```

**Every `only_in("desktop", "omarchy-ui")` guard that should keep working in a
browser needs `"browser"` added** — the cursor, the scroll, the mouse buttons,
the D-pad, A and B (`config/hyprpad.lua:102-146`). Otherwise focusing Chrome
switches the pointer off. This is the same widening the Omarchy-UI research
did for its two modes.

### 5.2 Hint letters as per-mode bare buttons

`ButtonAlt` is exactly the mechanism (§3): a second `h.button` on `a`, the
D-pad, `l1`/`r1` becomes an alternate guarded to `hints`; `x`, `y`, the grips
and stick clicks are first bindings and get a plain guard. Modes are exclusive,
so nothing competes.

```lua
-- Same letters as Vimium's `linkHintCharacters = "axynswe qc 1234 56"`
-- (without the spaces). Upper-cased on screen, so A/X/Y read as the button.
local hint = {
  a = "a", x = "x", y = "y",
  dpad_up = "n", dpad_down = "s", dpad_left = "w", dpad_right = "e",
  l1 = "q", r1 = "c",
  l4 = "1", l5 = "2", r4 = "3", r5 = "4",
  l3 = "5", r3 = "6",
}
for btn, letter in pairs(hint) do
  h.button(btn, "Hint " .. letter:upper(), h.key(letter)):only_in("hints")
end
h.button("b", "Cancel hints", h.key "escape"):only_in("hints")
```

This loads today **except** that `h.key "a"` is rejected: `key_code` has no
letters (`src/config.rs:329-351`). That is Δ0 below — the same one-line-per-key
table change the Omarchy research made for `KEY_BACK`.

### 5.3 Entering and leaving — the capability gaps

Entry, as the owner described it, is one chord that does two things: type `f`,
then switch mode. Three separate limits stand in the way today (§3): a chord
cannot type a key at all; a key cannot carry Shift; one binding is one action.
Exit needs B to do two things (Escape to Vimium, `clear_mode` to the daemon)
or the mode to clear itself.

| Δ | addition | why | size |
|---|---|---|---|
| **Δ0** | letters, digits and `f1`–`f24` in `key_code` (`src/config.rs:329-351`); the uinput device already registers codes 1–255 (`src/keyboard.rs:95-97`) so nothing else changes | hint letters; Vimium-mapped F-keys (§6.3) | 30 min |
| **Δ1** | a `h.key` on a **guide chord** *taps* the key (press, ~6 ms, release — the OSK's `tap`) instead of no-op'ing at `src/run.rs:2196`; needs the loop's `Option<VirtualKeyboard>` handed to `handle_gesture`/`execute` | typing `f` from a chord | 1–2 h |
| **Δ2** | `Action::Key` grows modifiers: `h.key "shift+f"`, `h.key "ctrl+tab"` — parse `mod+…+name`, press `KEY_LEFTSHIFT`/`KEY_LEFTCTRL`/`KEY_LEFTALT`/`KEY_LEFTMETA` before and release after (bare buttons: hold the mods for the hold; chords: the tap). Reference: `osk/src/output.rs:136-150`. The cheat sheet's derived labels need the mod prefix | `F`; tabs by uinput (§6.2) | 2 h |
| **Δ3** | `h.seq { a, b, … }` → `Action::Seq(Vec<Action>)`, executed in order by `handle_gesture` (so `set_mode` inside a sequence still routes to the mode engine) — **for chords only**; bare buttons stay key-only | `f` then `set_mode "hints"` in one press | 2–3 h |
| **Δ4** | a **transient manual mode**: `h.mode("hints", { transient = { max_presses = 3, exit_on = { "b", "r2", "rpad_click" }, on_context_change = true, timeout_ms = 5000 } })`. The engine counts bare presses while the override is live, and clears it on the cap, on a listed button (*after* that button's key is delivered — B's Escape still reaches Vimium), on any context change (focus, title, layer), or on the timer | leaving `hints` without a signal (§4.1) | 3–4 h |

With those, the whole feature is config:

```lua
-- Chords (free today: guide+l4, guide+l5, guide+x, guide+dpad_*, guide+l3/r3 …)
h.bind("guide+l4", "Link hints",            h.seq { h.key "f",       h.set_mode "hints" }):only_in("browser")
h.bind("guide+l5", "Link hints → new tab",  h.seq { h.key "shift+f", h.set_mode "hints" }):only_in("browser")
```

(If "new tab" should mean *and switch to it*, put `map <f14>
LinkHints.activateModeToOpenInNewForegroundTab` in Vimium and type `h.key "f14"`
— §2.7 — which also needs only Δ0/Δ1.)

Δ4 is the piece to weigh. The alternative — letting `h.button` carry
`h.clear_mode()`/`h.seq` — is a larger structural change (bare buttons are a
`Button → keycode` map reconciled per frame, `src/run.rs:1196-1210`, not an
action path), and it still leaves the "pressed nothing after a background tab"
case. A mode that knows it is transient is smaller and encodes the actual
contract: hints last at most ⌈log<sub>k</sub> n⌉ presses. It is also the right
shape for the companion in §4.3: its "hints are down" message just becomes one
more exit source.

### 5.4 Cheat sheet and bar

Nothing to do: `hints` is a declared mode, so `hyprpad bindings` prints its
tab, the sheet draws one, and `status.json` reports `"mode": "hints"` while it
is live — the visible "you are in hints" the design otherwise lacks.

---

## 6. Tab switching

### 6.1 Chrome's accelerators (the target)

Linux Chrome, from Google's shortcut reference (INFERRED — documentation, not
source): next tab **Ctrl+Tab** or **Ctrl+PgDn**; previous **Ctrl+Shift+Tab** or
**Ctrl+PgUp**; tab *n* **Ctrl+1…8**; last tab **Ctrl+9**; new tab **Ctrl+T**;
close **Ctrl+W**; reopen closed **Ctrl+Shift+T**. These work on every page,
including `chrome://` and the New Tab page, which Vimium's `J`/`K` do not
(§2.4).

### 6.2 Path 1 — the compositor: `hl.dsp.send_shortcut` (no hyprpad change)

VERIFIED in the fork:

- `hl.dsp.send_shortcut` is registered in the `dsp` table
  (`src/config/lua/bindings/LuaBindingsDispatchers.cpp:1443`) and takes
  `{ mods, key, window? }` (`:480-491`), returning a dispatcher closure. The
  legacy hyprlang spelling `sendshortcut MOD, KEY, WINDOW`
  (`src/config/legacy/DispatcherTranslator.cpp:536-597,938`) is *not* what the
  socket runs under a Lua config: `dispatch <expr>` becomes
  `return hl.dispatch(<expr>)` (`src/debug/HyprCtl.cpp:1163-1165`) and
  `hl.dispatch` calls the closure under the dispatch watchdog
  (`LuaBindingsToplevel.cpp:352-375`).
- `mods` is a case-insensitive substring match for `SHIFT`, `CTRL`/`CONTROL`,
  `ALT`, `SUPER`/`META`, … (`src/managers/KeybindManager.cpp:222-243`).
  `key` is an XKB keysym name resolved against the seat keyboard's keymap at
  level 0 (`LuaBindingsDispatchers.cpp:398-437`): `Tab`, `Page_Down`,
  `Page_Up`, `t`, `w`, `1`…`9` all resolve; `code:23` / `mouse:272` also accepted.
- Delivery (`src/config/shared/actions/ConfigActions.cpp:1541-1600`): with no
  `window`, the key goes to the surface that currently has keyboard focus —
  `sendKeyboardMods(modMask)`, key press, key release, `sendKeyboardMods(0)`.
  With a `window` selector it refocuses to that window for the key and back.

Spelling from hyprpad (`h.dispatch` passes the expression verbatim after
`dispatch `, `src/hypr.rs:149-155`; use parentheses — the fork's "did you mean
hyprlang?" heuristic keys on `(`):

```lua
local function chrome_key(mods, key)
  return h.dispatch(string.format('hl.dsp.send_shortcut({ mods = "%s", key = "%s" })', mods, key))
end
h.bind("guide+dpad_right", "Browser: next tab",     chrome_key("CTRL", "Tab")):only_in("browser")
h.bind("guide+dpad_left",  "Browser: previous tab", chrome_key("CTRL SHIFT", "Tab")):only_in("browser")
-- optional:
h.bind("guide+dpad_up",    "Browser: new tab",      chrome_key("CTRL", "t")):only_in("browser")
h.bind("guide+dpad_down",  "Browser: close tab",    chrome_key("CTRL", "w")):only_in("browser")
h.bind("guide+x",          "Browser: reopen closed tab", chrome_key("CTRL SHIFT", "t")):only_in("browser")
```

**UNVERIFIED — the one live test that decides this path:** with Chrome focused,
from a terminal on another workspace or via a Hyprland bind,

```
hyprctl dispatch 'hl.dsp.send_shortcut({ mods = "CTRL", key = "Tab", window = "class:google-chrome" })'
```

(the `window` selector targets Chrome even though the terminal has focus; drop
it in the hyprpad binding). Chrome on Wayland derives modifier state from
`wl_keyboard.modifiers`, which is what `pass` sends, so this is expected to
work — it is how upstream Hyprland users script Chrome — but the fork's `pass`
has X11 special-casing and this machine's Chrome is native Wayland, so test it
rather than trust it. If Ctrl+Tab misbehaves, try `Page_Down`/`Page_Up`
(`Next`/`Prior`), which some Chromium builds treat more plainly.

### 6.3 Path 2 — uinput with modifiers (Δ2)

`h.bind("guide+dpad_right", h.key "ctrl+tab")` after Δ1+Δ2. Real evdev key
events through the kernel, resolved by the compositor like any keyboard, so
they work identically for native Wayland and XWayland clients and do not depend
on fork-specific Lua. Costs the two code deltas that hints need anyway.

### 6.4 Path 3 — bare bumpers through Vimium F-keys (Δ0 only)

`h.button("l1", h.key "f13"):only_in("browser")` + Vimium `map <f13>
previousTab` (and `<f14> nextTab`). Bumpers are free bare buttons on the
desktop (they page the cheat sheet only in `cheatsheet` mode,
`config/hyprpad.lua:152-153`), no guide hold, and the same trick gives
`map <f15> LinkHints.activateMode` — hints on a *bare* grip with no chord. Only
Vimium's key-notation acceptance of `<f13>` and Chrome delivering `event.key ===
"F13"` for `KEY_F13` are INFERRED, not tested; and it only works where Vimium
runs (§2.4). A nice-to-have on top of path 1, not a replacement.

### 6.5 Trade-off

| | compositor `send_shortcut` | uinput modifiers | Vimium F-keys |
|---|---|---|---|
| hyprpad change | **none** | Δ1 + Δ2 | Δ0 |
| works on `chrome://`, NTP, PDF viewer | yes | yes | no |
| works for XWayland windows | fork's `pass` X11 path — INFERRED | yes (real keys) | yes |
| can target an unfocused window | yes (`window =`) | no | no |
| coupling | HypXRland Lua spelling | none | Vimium config |
| latency | one socket round trip | none | none |
| failure mode | `error:` reply logged by `dispatch_raw` | silent | silent |

### 6.6 Free chords (VERIFIED against `config/hyprpad.lua:170-197` and `parse_button`)

Used: `r1 l1 stick_right stick_left a b r5 menu l2 r2 y view r4`.
**Free:** `x` (the old launcher chord is gone; `guide+menu` remains), `dpad_up
dpad_down dpad_left dpad_right`, `l3 r3`, `l4 l5` (only in a commented example),
`qam`, `rpad_click lpad_click`, right-stick `stick_up stick_down`, all four
`lstick_*`, `guide_hold`. (`guide_tap` belongs to Steam.)

Proposal: **D-pad left/right = browser tabs** (mirrors `guide+l2/r2` for groups
in placement logic — a pair — while being a different pair of buttons, as
asked), **`guide+l4`/`guide+l5` = hints / hints-in-new-tab**, D-pad up/down and
`guide+x` for new/close/reopen if wanted. All `:only_in("browser")`, so nothing
changes anywhere else, and the cheat sheet's `browser` tab shows exactly this
set.

---

## 7. Recommendation and phased plan

| phase | delivers | needs | effort | risk |
|---|---|---|---|---|
| **1a — tabs** | `guide+dpad_left/right` prev/next tab in browsers; optional new/close/reopen | `browser` mode + guards widened (config only); one live `hyprctl` test of §6.2 | **½ day**, ~15 min of it the test | `send_shortcut` misdelivering to Chrome (fallback: 1b's Δ2 via uinput) |
| **1b — hints, letters** | `guide+l4` → hints, `guide+l5` → hints in new tab; letters on A/X/Y, D-pad, bumpers, grips; B cancels; mode self-clears | Δ0–Δ4 (§5.3), Vimium: `linkHintCharacters`, `unmap x`, optionally `map <f14> …ForegroundTab` | **1–1½ days** incl. tests and sheet labels | stray letters after Vimium exits early (mitigated: safe alphabet + cap + context-change drop); hints entered on a page Vimium ignores (cap clears it) |
| **2 — glyphs** | hint labels show controller glyphs on most sites | subset WOFF2 from the cheat-sheet art, pasted as `@font-face` into Vimium's CSS box; the CSP probe (§4.2) | **½ day** | strict-CSP sites fall back to letters (benign); 8 KB sync-storage cap |
| **3 — companion** | true "hints up/down" signal; CSP-proof glyphs | hyprpad control socket (`mode set/clear`), a ~150-line observer extension, a native host + manifest per browser | **2–3 days** | unpacked-extension upkeep; one more install step per machine; only worth it if phase 1's transient mode is irritating |

Order of operations: 1a's live test first (it is the cheapest possible
result and it decides whether Δ2 is needed for tabs at all), then Δ0/Δ1/Δ3
(which together already give `f` + hints with a *chord* to clear), then Δ4,
then Δ2 for `F`. Vimium's side of phase 1 is three lines in its options page.

---

## 8. What could not be verified

- **`hl.dsp.send_shortcut` reaching Chrome** with Ctrl held (no browser
  launch was allowed). The exact command to run is in §6.2.
- **`@font-face` from a `data:` URI in extension-injected CSS on a strict-CSP
  page**, and the 8 KB `chrome.storage.sync` item cap on the CSS box.
- **Vimium accepting `<f13>`-style names** in *Custom key mappings*, and
  Chrome reporting `event.key === "F13"` for evdev `KEY_F13`.
- Whether current Google Chrome stable honours `--load-extension` (Chromium
  does) — matters only for phase 3.
- Steam's browser behaviour is from Valve's marketing page and community
  threads, not source.
- Brave's window class (`brave-browser`) — not installed.

---

## 9. Sources

hyprpad (`/home/ajg/code/hyprsc`, `e2ddad0`):
`src/config.rs:125` (`Action::Key`), `:228-275` (gesture keys / `parse_button`),
`:329-351` (`key_code`), `:659-671` (`ModeDef`), `:690-700` (`ButtonAlt`),
`:759-766` (`rescan_on_title_change`), `:1418-1425` (`buttons_in`);
`src/lua_config.rs:231-290` (`build_ctx`), `:784,796` (`h.button`, `h.key`),
`:1026-1045` (button values must be keys; alternates), `:1429-1470`
(`value_to_action`); `src/run.rs:1180,1196-1210` (`drive_buttons`),
`:1945-2005` (`handle_gesture`), `:2170-2200` (`execute`; `Action::Key` no-op
at `:2196`); `src/mode.rs:301-333` (`set_mode`/`clear_mode`/`reconfigure`),
`:465-491` (`refresh_declared`); `src/keyboard.rs:95-97,122-128`;
`src/gesture.rs:100-160,211-217`; `src/hypr.rs:149-155`; `src/main.rs:10-12`;
`osk/src/output.rs:136-150`; `config/hyprpad.lua`; `README.md`;
`docs/13-modality-design.md`; `docs/research/omarchy-menu-navigation.md`.

HypXRland (`/home/ajg/code/Hyprland`, `7b7e193`):
`src/config/lua/bindings/LuaBindingsDispatchers.cpp:398-437` (`resolveKeycode`),
`:440-459` (`dsp_sendShortcut`), `:480-491` (`hlSendShortcut`), `:1380-1450`
(`hl.dsp` table; `send_shortcut` at `:1443`, `send_key_state` at `:1444`);
`src/config/lua/bindings/LuaBindingsToplevel.cpp:352-375` (`hl.dispatch`);
`src/config/lua/bindings/LuaBindingsInternal.cpp:323-332` (`pushWindowUpval`);
`src/config/shared/actions/ConfigActions.cpp:48-50,1541-1600` (`Actions::pass`);
`src/config/legacy/DispatcherTranslator.cpp:536-597,938` (legacy `sendshortcut`);
`src/managers/KeybindManager.cpp:222-243` (`stringToModMask`);
`src/debug/HyprCtl.cpp:1159-1174` (`dispatch` → `hl.dispatch`).

Vimium 2.4.2 (local install = GitHub `v2.4.2`, https://github.com/philc/vimium/tree/v2.4.2):
`content_scripts/link_hints.js:76-159` (modes), `:339-356` (entry points),
`:384-417` (mode init, Escape/click exit, container), `:466-486` (HUD text,
marker element), `:502-606` (key handling), `:786-796` (deactivate/remove),
`:800-866` (`AlphabetHints`, `hintStrings`), `:869-893` (`FilterHints`),
`:1060-1066` (`spanWrap`); `lib/settings.js:14-40,107,125,250`;
`lib/keyboard_utils.js:8-14,34-73`; `lib/dom_utils.js:561-567`;
`background_scripts/commands.js:420-500`; `background_scripts/main.js:493-500,883-887`;
`background_scripts/all_commands.js:181-226`; `content_scripts/vimium.css:83-135`;
`content_scripts/ui_component.js:57-62`; `pages/options.html:110-141,224-228`;
`manifest.json:14,21,34,42,44`. README key list:
https://github.com/philc/vimium/blob/master/README.md. Firefox port:
https://addons.mozilla.org/en-US/firefox/addon/vimium-ff/ (2.4.2, 2026-03-07).

Tridactyl: https://github.com/tridactyl/tridactyl/blob/master/readme.md;
settings and defaults https://raw.githubusercontent.com/tridactyl/tridactyl/master/src/lib/config.ts;
outside-in control request https://github.com/tridactyl/tridactyl/issues/780
(open, P4, 2018-07-09); native messenger https://github.com/tridactyl/native_messenger.

Chrome: native messaging https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
(manifest paths, `stdio`, 32-bit native-order length prefix, host launched by
the browser); keyboard shortcuts https://support.google.com/chrome/answer/157179.

Omarchy (local): `/usr/bin/omarchy-chromium-copy-url-host`,
`/usr/bin/omarchy-install-chromium-copy-url`,
`~/.config/google-chrome/NativeMessagingHosts/com.omarchy.copy_url.json`,
`/usr/share/omarchy/config/chromium-flags.conf`.

Steam: https://store.steampowered.com/bigpicture/ ("reticle-based navigation,
tabbed browsing"); https://steamcommunity.com/sharedfiles/filedetails/?id=192523320
(Y cycles zoom, X options menu);
https://steamcommunity.com/groups/bigpicture/discussions/1/458604254427297940/
(Steam Controller in the BPM browser: right pad as mouse, right trigger clicks).

---

## Appendix — commands used (all read-only)

```sh
ls /usr/bin | grep -iE 'chrom|brave|firefox|zen|vivaldi'
xdg-settings get default-web-browser
ls ~/.config/google-chrome/Default/Extensions/dbepggeogbaibhgnhhndojpepiihcmeb/
ls -d ~/.config/chromium/Default/Extensions/*
cat ~/.config/chrome-flags.conf ~/.config/chromium-flags.conf
hyprctl clients -j | grep -o '"class": *"[^"]*"' | sort | uniq -c
grep -n 'repeat_delay\|repeat_rate' ~/.config/hypr/input.lua ~/.config/hypr/input.conf
cat ~/.config/google-chrome/NativeMessagingHosts/*.json
cd /home/ajg/code/Hyprland && grep -rn sendshortcut src/ && git log -1
```
