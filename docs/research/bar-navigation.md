# Bar navigation from the controller — a "bar mode"

Research for: **use the D-pad to step between the status-bar widgets and press A**
— "get Bluetooth, audio and display within easy reach without building a bunch
of custom key bindings", so the controller map stays small and high-value.

Everything below is marked **VERIFIED** (read from source at a cited line, or
observed live on this machine on 2026-09-02) or **INFERRED**. Live probing was
read-only: `hyprctl layers -j`, `hyprctl cursorpos`, `hyprctl monitors -j`,
`omarchy-shell shell debugBarGeometry|listPlugins`. Nothing was opened, moved,
restarted or written outside this file.

Sources of truth:

- Shell: `~/code/omarchy-bluetooth-friendly-name/shell/`, HEAD `98b38583`
  (the owner's `Qt.Key_Back` patch on top of upstream, which the tree's own
  history shows through `5edc3497` / `e1fc5022`). `OMARCHY_PATH` points here,
  so it is the running shell (established in
  [omarchy-menu-navigation.md](omarchy-menu-navigation.md)). Line cites are
  relative to `shell/` unless a path says otherwise.
- The owner's own widgets, read-only: `~/.config/omarchy/plugins/ajg.workspaces/`
  and `~/.config/omarchy/plugins/hyprpad.status/` (the latter is a copy of this
  repo's `shell/hyprpad.status/`).
- hyprpad: this repo at `e2ddad0` (`master`). Note two things that landed
  *during* this research and are folded in: `guide+x` is free again
  (`ea4b6a8`), and mouse clicks are now bare-button bindings — `h.mouse "left"`
  is an `h.key` in the `BTN_*` code space (`b0b84b3`, `src/config.rs:196-200,
  362-379`).
- Upstream tracker: the repository is now `omacom/omarchy` (`basecamp/omarchy`
  redirects, and GitHub's search API refuses the old name — the tooling
  limitation the previous doc hit). Searches below ran against `omacom/omarchy`
  and worked.

---

## 0. Recommendation in one paragraph

The bar already has most of a "bar mode" — just not for the bar *icons*. Every
panel is keyboard-driven, **Tab inside any open panel walks to the neighbouring
panel** (`plugins/bar/Bar.qml:435-466`, upstream since 2026-05-27), and panels
open by IPC without a click (`omarchy-shell shell togglePanelAt right N`,
`shell toggle omarchy.<id>`). So **phase 0 is config only, today**: a guide chord
opens the first panel of the right section, R1 sends Tab to walk the ring,
D-pad/A drive the panel and B (already `KEY_BACK`) closes it. That is most of
the ask with no code. What is missing is a *focus ring over the icons* — the bar
surface never takes keyboard focus (`WlrLayershell.keyboardFocus` unset →
Quickshell's default `None`) and has no selection state at all. The right end
state is a **small shell-side patch** (option (a)): a transient keyboard-focused
layer surface, namespace `omarchy-bar-nav`, mapped only while bar mode is on,
whose key handler moves an accent ring across the bar's existing click-target
registry and *activates* by calling the same `triggerPress(Qt.LeftButton)` a
mouse click calls. hyprpad then needs **one line** — add `omarchy-bar-nav` to
the `omarchy-ui` allowlist — because D-pad → arrows, A → Enter, B → Back are
already the right keys, and "bar mode is on" is visible to the daemon through
the same `openlayer`/`closelayer` channel the cheat sheet uses. Cursor-warping
(option (b)) works today but is the wrong feel and leans on a debugging IPC; a
hyprpad-owned quick panel (option (c)) would be a second source of truth for
what the bar can do. Enter on **`guide+dpad_up`** (up to the bar), leave on B
at the ring level, no timeout.

---

## 1. What the bar can do today

### 1.1 The bar surface: no keyboard focus, no selection model

VERIFIED:

- The bar is one `PanelWindow` per monitor (`plugins/bar/Bar.qml:966-976`,
  `component BarPanel` at `:1004`), `WlrLayershell.namespace: "omarchy-bar"`,
  `WlrLayershell.layer: WlrLayer.Top` (`:1038-1039`). **`keyboardFocus` is never
  set.** Quickshell's default is `None`: `wlr_layershell.hpp:107` in
  `quickshell-mirror/quickshell` — *"The degree of keyboard focus taken.
  Defaults to `KeyboardFocus.None`."* (Quickshell 0.3.1 is what runs here.)
  So arrow keys can never reach the bar as it stands; a focus ring needs either
  a focus-taking surface or an IPC.
- Live baseline `hyprctl layers -j`: `omarchy-background` (level 0) and
  `omarchy-bar` (level 2, `2048×26`) only.
- There is **no focus/selection state**. The only per-slot visuals are
  `hovered`, `dragSource`, and `panelOpen` (`Bar.qml:1568-1570`). `panelOpen`
  drives `openPanelIndicator` — the accent dot under an icon whose panel is open
  (`:1641-1667`, `Color.accent`, sized by the module's `openPanelIndicatorWidth`
  hint, `:1575-1580`). That is the visual idiom a focus ring should reuse.

### 1.2 The click plumbing a synthetic "activate" can call

VERIFIED, and this is the load-bearing hook for option (a):

- Every `WidgetButton` **registers itself with the bar as a click target** on
  creation (`Ui/WidgetButton.qml:44-48, 50-55` → `Bar.qml:104-114`,
  `clickTargets`). `BarIconButton` and `BarIndicator` are `WidgetButton`s
  (`Ui/BarIconButton.qml:5`, `Ui/BarIndicator.qml:4`), so this covers every
  first-party icon, every workspace number, every indicator, the tray chevron —
  and the owner's `hyprpad.status` widget (`Widget.qml:210`).
- A mouse click is `target.triggerPress(button)` (`WidgetButton.qml:35-38`,
  called from its own `MouseArea` at `:116`). The bar dispatches slot clicks
  through the registry — `moduleClickTargetAt` / `pressModuleClickTarget`
  (`Bar.qml:762-790`), filtered by `moduleTargetClickable` (`:752-760`: visible,
  opaque, `interactive`, `pressable`, not `concealed`) — and the open panel's
  dismiss overlay forwards bar-strip clicks through the *same* registry
  (`Ui/KeyboardPanel.qml:302-322`, using `anchorWindow.itemPosition(target)` for
  geometry). **So "activate the focused widget" is one call that cannot drift
  from what a click does**, and `clickTargets` is already the list of things
  worth focusing, with visibility rules already applied.

### 1.3 Geometry and the cursor (for option (b))

VERIFIED:

- `omarchy-shell shell debugBarGeometry` (`shell.qml:998-1000` → `Bar.qml:128-152`)
  returns one row per **slot** `{id, section, x, y, width, height, visible,
  itemVisible, …}` in the bar's content coordinates. Live:
  `omarchy.clock` at `x 969 w 111`, `omarchy.bluetooth` at `x 1905 w 27`,
  `omarchy.power` at `x 2013 w 27` on a screen that is `2560×1600 @ 1.25` =
  `2048×1280` logical; `hyprctl cursorpos` answers in the same logical space
  (`185, 937`). For a full-width top bar `x` maps straight to screen `x` and the
  bar's `y` is 0; for other edges the bar's own screen offset has to be added
  (`hyprctl layers` gives it). It is documented as *"bar geometry dump for
  debugging"* (`docs/omarchy-shell.md:126`), i.e. not a contract.
- HypXRland has an absolute cursor warp: `hl.cursor.move({ x = …, y = … })`
  (`~/code/hypxrland/src/config/lua/bindings/LuaBindingsDispatchers.cpp:77-85`,
  wrapping the `movecursor` dispatcher), reachable from hyprpad as
  `h.dispatch "hl.cursor.move({ x = 1918, y = 13 })"` (`src/hypr.rs:145-155`).
  hyprpad's own virtual pointer is relative-only (`src/output.rs:129-131`).

### 1.4 Three keyboard/IPC paths that already walk the bar

VERIFIED:

| path | how | source |
|---|---|---|
| **Named panel hotkeys** | `omarchy-shell shell toggle omarchy.<audio\|bluetooth\|monitor\|clock\|network\|power\|weather\|agents>` opens the panel on the focused monitor | `shell.qml:1010-1012` → `Bar.findPanelWidget` `:500-516` (focused-monitor pick via `BarModel.pickPanelSlot`, `:490-493`); upstream binds `SUPER+CTRL+{A,B,D,W,P}`, `SUPER+CTRL+ALT+D` (`default/hypr/bindings/utilities.lua:97-102`) |
| **Positional hotkeys** | `omarchy-shell shell togglePanelAt <section> <index>` — the Nth *panel-bearing, visible* widget in a section, 1-based | `shell.qml:1018-1025` → `Bar.qml:429-433` → `panelNavigationSlots` `:401-418` (requires `open`/`close`/`opened` on the widget; skips hidden ones). Upstream `SUPER+CTRL+1-9` (`utilities.lua:105-115`), commit `5edc3497` (#6702, 2026-08-11); manual `05-the-top-bar.md:48,61` |
| **Tab walks neighbours** | inside any open panel, `Tab`/`Shift-Tab` closes it and opens the next/previous panel **in the same section**, wrapping | `Ui/PanelKeyCatcher.qml:54-58` → each panel's `onTabRequested` → `Panel.switchPanel` (`Ui/Panel.qml:32-35`) → `Bar.switchPanelFrom` `:435-466` (`% slots.length` at `:460`). Commit `b03a7cc7` ("Use Tab to switch bar panels", DHH, 2026-05-27). Manual `05-the-top-bar.md:59`: *"arrows move, Return activates, Tab steps to the neighbouring panel, and Escape closes."* |

On the owner's layout (`~/.config/omarchy/shell.json`, right section =
`tray, hyprpad.status, agents, bluetooth, network, audio, monitor, power`), the
right-section panel ring is **agents → bluetooth → network → audio → monitor →
power → (wrap)**: the tray has no panel and `hyprpad.status` has no `open()`, so
both are skipped by `panelNavigationSlots` (`:411`). `togglePanelAt right 2` is
Bluetooth. The centre ring is `clock → weather` (keyboard-layout and
system-update are hidden right now: live `visible:false`).

**So a panel ring exists today. What does not exist is a ring over the icons
themselves** — which matters only for the widgets that have no panel
(workspaces, keyboard-layout, system-update, indicators, tray, `hyprpad.status`,
menu). See the per-widget table for which of those are worth reaching.

### 1.5 Per widget

`IPC` = reachable without a click. `Keys` = whether the surface a click opens is
navigable with what hyprpad's `omarchy-ui` mode already sends (D-pad → arrows,
A → Enter, B → `KEY_BACK`, `config/hyprpad.lua:127-131,146`). All bar panels share
the `omarchy-keyboard-panel` namespace (`Ui/KeyboardPanel.qml:85`), take
keyboard focus while open (`Exclusive` prime → `OnDemand`, `:98-100`), and route
keys through `Ui/PanelKeyCatcher.qml` (Escape **or Back** → close `:51`,
Tab `:54-58`, arrows/hjkl `:59-70`, Return `:71-74`, Space `:75`, `x` delete
`:78`). All VERIFIED from source.

| widget | click (left · right · middle · scroll) | IPC without a click | Keys once open |
|---|---|---|---|
| `omarchy.menu` | `omarchy-shell shell toggle omarchy.menu '{"menu":"root"}'` · terminal | yes — `omarchy-menu`, and already on `guide+menu` | menu: arrows, Enter, Back (owner's patch) |
| `ajg.workspaces` | one `WidgetButton` **per workspace** → `hyprctl dispatch hl.dsp.focus({workspace=N})` (`Workspaces.qml:38-41,59-74`) | hyprpad has `h.workspace` natively; nothing opens | n/a — but it is a **multi-target slot**: a ring must step through its buttons, not the slot |
| `omarchy.clock` | calendar panel · cycle format · `omarchy-menu-timezone` (`panels/clock/BarWidget.qml:158-162`) | `omarchy.clock open/close/toggle/cycleFormat/toggleWeekStart` (`:133-144`); `shell toggle omarchy.clock` | yes: arrows step days/months, Enter = today, Tab neighbour (`panels/clock/Panel.qml:247-257`) |
| `omarchy.keyboard-layout` | `hyprctl switchxkblayout <kb> next` (`bar/widgets/KeyboardLayout.qml:157-161,216`) | none (no IpcHandler); hyprpad could `h.exec` the same `hyprctl` | nothing opens; hidden with one layout (`:287`, live hidden) |
| `omarchy.weather` | panel · `omarchy-notification-send "$(omarchy-weather-status)"` · refresh (`panels/weather/BarWidget.qml:76-81`) | `omarchy.weather` target (`Panel.qml:476`); `shell toggle omarchy.weather` | Escape/Back + Tab only — a read-only card, no cursor (`panels/weather/Panel.qml:498-504`) |
| `omarchy.system-update` | floating terminal running `omarchy-update` (`bar/widgets/SystemUpdate.qml:20-21,63`) | `omarchy.system-update refresh/clear` only (`:27-38`); the action itself is a plain command | n/a; visible only with an update pending (`:24`, live hidden) |
| `omarchy.tray` | hover reveals the drawer (`Tray.qml:262-264`); chevron right-click → manage popup (`:273-275`); item left → SNI `activate()` or its menu (`:832-842`), right → menu (`:826-830`), scroll → SNI scroll | none | items are plain `MouseArea`s, **not** `WidgetButton`s (`:818-846`) → not in `clickTargets`, and the slot root has no `triggerPress` → **a slot-level activate does nothing**. Menus are a custom QML drill-down (`:36-40, 100-131`), mouse-driven as read (INFERRED: no key handling seen). Live: no items, width 0 |
| `omarchy.indicators` | each indicator is a `BarIndicator` = `WidgetButton`; left toggles its state (e.g. DND: `bar/indicators/Dnd.qml:17-21`) | `omarchy.indicators refresh` only (`Indicators.qml:168-174`); the underlying toggles have their own `omarchy-toggle-*` commands | n/a. Inactive indicators are `concealed` and non-`interactive` until hovered (`Ui/BarIndicator.qml:41-42`) → `moduleTargetClickable` false → a ring only sees *active* ones. Live: none active, width 0 |
| `hyprpad.status` | opens the cheat sheet on the current mode (`Widget.qml:224`) | `hyprpad-cheatsheet toggle`, already on `guide+view` | cheat sheet: L1/R1 tabs, B closes |
| `omarchy.agents` | panel · `omarchy-agent --pick` · next provider (`agents/Panel.qml:344-348`) | `omarchy.agents open/close/toggle/refresh/next` (`:327-336`) | yes: Left/Right provider, Up/Down scroll, Enter refresh, Tab (`:363-379`) |
| `omarchy.bluetooth` | panel · toggle radio (`panels/bluetooth/Panel.qml:750-759`) | `omarchy.bluetooth open/close/toggle/toggleBluetooth` (`:739-748`) | yes, full cursor model: arrows, Enter connect/disconnect, `x` forget, `b` radio, `r` rename; keys blocked while the rename `TextField` is up (`:771-790`) |
| `omarchy.network` | open/close (`panels/network/Panel.qml:956-970`) | `omarchy.network open/close/toggle` (`:210-217`) | yes: sections header/band/dns/wifi, arrows, Enter connect; the passphrase prompt is an inline `TextField` that takes over keys (`:996`) — **needs the OSK to type** |
| `omarchy.audio` | panel · mute all · — · **scroll = volume ±5 %** (`panels/audio/Panel.qml:630-648`) | `omarchy.audio open/close/toggle` (`Ui/Panel.qml:48-57` via `ipcTarget`, `:14`) | yes: Up/Down rows, **Left/Right = volume ±5 %**, Enter select sink, `m` mute (`:660-675`) |
| `omarchy.monitor` | panel · — · — · **scroll = brightness ±5** (`panels/monitor/Panel.qml:468-482`) | `omarchy.monitor open/close/toggle/show/hide` (`:220-229`) | yes: Up/Down sections, **Left/Right = brightness / text size / scale**, Enter (`:494-508`) |
| `omarchy.power` | panel (only with a battery, `:286,297`) · toggle percentage (`panels/power/Panel.qml:285-289`) | `omarchy.power open/close/toggle/togglePercentage` (`:177-184`) | yes: any arrow selects a profile, Enter applies (`:302-312`) |

Three things fall out of the table:

1. **The high-value targets — Bluetooth, network, audio, display, power — are
   all panels, all IPC-openable, all fully keyboard-driven, and all already in
   `omarchy-ui`.** Left/Right on the audio and display panels *are* the volume
   and brightness sliders. Nothing about them needs a click.
2. The right-click actions (mute all, radio toggle, percentage toggle, cycle
   clock format) are the ones a controller cannot reach by "activate", and only
   some have IPC verbs (`toggleBluetooth`, `togglePercentage`; **mute-all has
   none** — `toggleAllMuted` is click-only, `audio/Panel.qml:636`). A ring with
   a "secondary activate" button (X → `triggerPress(Qt.RightButton)`) would
   cover them exactly, since that is what a right click is.
3. The widgets with **no panel** are either not worth reaching from the pad
   (workspaces — `guide+l1/r1` and the stick already do it; system-update;
   keyboard-layout) or need per-item work (tray). So a *panel* ring covers the
   ask, and an *icon* ring is the polish.

### 1.6 What the pad cannot do in `omarchy-ui` yet

VERIFIED against `config/hyprpad.lua` and `src/config.rs:310-329`:

- **No Tab.** `tab` is in the key table (`:319`) but nothing binds it, so
  panel-to-panel walking is not on the pad. The bumpers are free in
  `omarchy-ui` (they are only bound under `cheatsheet`, `config/hyprpad.lua:152-153`;
  `guide+l1/r1` are chords and unaffected).
- **No Shift-Tab.** There is no evdev keycode for Backtab; it is `Shift+Tab`,
  and a bare button emits exactly one keycode (`Action::Key(u16)`,
  `src/config.rs:116`; `drive_buttons`). Walking backwards would need either a
  daemon change (multi-keycode bare bindings) or a shell change (accept
  `PageUp`/`PageDown` — both in hyprpad's table, `:324-325` — as ±1 in
  `PanelKeyCatcher`). With a six-panel ring that wraps, forward-only is
  acceptable for phase 0.
- **No letters.** The panels' `b`/`m`/`r`/`w`/`x` shortcuts are printable keys;
  the table has none, and adding them would make them type into the menu's
  filter elsewhere. Leave them.
- **A bare button cannot `exec`.** `src/lua_config.rs:1024-1029` still rejects a
  non-key action on `h.button` (a mouse button now counts as a key, `:1015`).
  This is the same constraint the menu research hit (§3.3 there) and it decides
  the shape of every option below: **per-press IPC from the D-pad is not
  available; per-press keys are.**

---

## 2. Options for a "bar mode"

### (a) Shell-side: a focus ring plus a transient keyboard surface

**Sketch.** All in the owner's checkout, additive, like the `Key_Back` patch.

1. *State* on the bar root (`Bar.qml`): `navActive: bool`, `navTarget: Item`.
   The ring order is the existing `clickTargets` registry filtered by
   `moduleTargetClickable` and restricted to one bar window
   (`targetBelongsToWindow`, `:158-160`), sorted along the bar by
   `window.itemPosition(target)` (the call `KeyboardPanel` already uses,
   `:310`). Workspace numbers, indicators and the tray chevron become stops for
   free; hidden and concealed things are skipped for free. Pure ordering logic
   goes in `BarModel.js` where `test/shell.d/bar-test.sh` can reach it.
2. *Visual*: a ring/underline drawn from the bar with the focused target's
   geometry, in the `openPanelIndicator` idiom (`:1641-1667`, `Color.accent`).
   Optionally show the target's `tooltipText` as a label: `showTooltip`
   currently requires real hover (`targetTooltipHovered`, `:173-175, 888`), so
   that needs an `|| target === navTarget` — two lines.
3. *Keys*: the bar must **not** take keyboard focus permanently (it would steal
   typing from every app). Instead, while `navActive`, map a small
   `PanelWindow` — `WlrLayershell.namespace: "omarchy-bar-nav"`, `Overlay`,
   `keyboardFocus` primed `Exclusive` then `OnDemand` exactly as
   `KeyboardPanel` does (`Ui/KeyboardPanel.qml:87-100, 251-259`) — containing a
   `PanelKeyCatcher`: Left/Right (and Up/Down on a vertical bar) move the ring,
   Return/Space activate, Escape/Back leave, Tab = next. **hyprpad then needs no
   new code and no IPC per press**: D-pad/A/B are already the right keys, and
   the surface's namespace puts the daemon in the right mode (§3.4).
4. *Activate* = `navTarget.triggerPress(Qt.LeftButton)` — identical to a click.
   A secondary button (X) → `triggerPress(Qt.RightButton)` covers the
   right-click actions (§1.5 point 2).
5. *Yield to panels*: when activation opens a panel, `KeyboardPanel` takes
   focus (`Exclusive` prime) and `bar.activePopout` becomes non-null
   (`requestPopout`, `:316-323`). The nav surface must drop to
   `keyboardFocus: None` while `activePopout !== null` and re-prime when it goes
   back to null (B closed the panel). That gives the console idiom: **B in a
   panel closes it and lands you back on the ring; B on the ring leaves.** Tab
   inside a panel still walks panels; keep `navTarget` in step by watching
   `activePopout`.
6. *Entry/exit IPC* on the existing `IpcHandler { target: "omarchy.bar" }`
   (`Bar.qml:955-964`, today only `syncHidden`): `navigate()`, `leave()`,
   `toggleNavigate()`, plus `focusNext/focusPrev/activate` for scripts and
   `isNavigating`. (Note `shell call <id> …` cannot reach the bar: `callIfLoaded`
   looks in `panelLoaders`, `shell.qml:567-579`, and the bar is not a panel — so
   the verbs go on `omarchy.bar` or on the `shell` target next to
   `togglePanelAt`.)
7. *How the daemon learns bar mode is on*: the `omarchy-bar-nav` layer in
   `hyprctl layers` / `openlayer` — the exact channel the cheat sheet uses
   (`src/hypr.rs:184-200`, `src/mode.rs:271-297`). Optionally `omarchy.bar
   isNavigating` for scripts. hyprpad's `status.json` can carry it *back* to the
   bar (`hyprpad.status` reads `mode`, `Widget.qml:66-67`) if a distinct mode is
   declared (§3.4).

**Surfaces touched (shell):** `plugins/bar/Bar.qml` (state, ring order, nav
`PanelWindow`, IPC verbs, ring rectangle in `ModuleSlot` or a sibling item),
`plugins/bar/BarModel.js` (ordering), `Ui/WidgetButton.qml` (a `navFocused`
property if the ring is drawn per target rather than from the bar),
`test/shell.d/bar-test.sh`, `docs/omarchy-shell.md` (verb table `:107-126`),
`manual/05-the-top-bar.md`. **hyprpad:** `config/hyprpad.lua` (+1 allowlist
entry, +1 chord, +1 bumper binding). Optionally a 20-line `scripts/hyprpad-bar`
wrapper in the style of `scripts/hyprpad-cheatsheet` so the chord's `exec`
stays stable if the IPC spelling moves.

**Effort:** ~200-300 lines of QML/JS plus tests; one to two days including
live testing. **Testing hazard (from memory):** the shell hot-reloads on every
write under `~/.config/omarchy/plugins/`, and a reload while the session is
locked kills the lock screen — but this patch lives in the checkout, not the
plugin dir, and is applied by an explicit shell restart. Never restart while
locked.

**Risks:**

- **Upstream drift.** `Bar.qml` is 1842 lines and moves often (its 15 most
  recent upstream commits span 2026-07-20 → 2026-08-30,
  `gh api …/commits?path=shell/plugins/bar/Bar.qml&per_page=15`).
  A local patch costs a rebase per upstream pull. Mitigate by keeping the ring
  in a *new* file (`plugins/bar/BarNavigator.qml`) with a handful of hooks in
  `Bar.qml`, and by proposing it upstream as keyboard accessibility
  (`SUPER+CTRL+Up` focuses the bar; arrows walk it) rather than as controller
  support.
- **Exclusive prime.** For ~75 ms after mapping, the nav surface routes every
  pointer event compositor-wide to itself (`KeyboardPanel.qml:16-19, 251-259`);
  upstream issue omacom/omarchy#9029 is the same hazard on the menu. Harmless
  here (the surface is tiny and short-lived), but copy the prime timing rather
  than inventing it.
- **Hidden bar.** `barHidden` parks the bar off-screen (`Bar.qml:32, 1013-1025`);
  `navigate()` should either refuse or reveal for the duration.
- **Multi-monitor.** One bar per screen; the ring must live on
  `focusedScreenName()` (`:490-493`), as panel hotkeys do since `667d2d2f`.
- **Over a game.** The nav surface takes keyboard focus, so the daemon leaves
  `game` for `omarchy-ui` while it is up — the same behaviour a panel has today
  (§4.5 of the menu doc). Correct, and worth knowing.

**Variant (a′): the same ring from inside hyprpad's own widget, no Omarchy
patch.** VERIFIED that the pieces are reachable: the bar injects itself into
every widget as `bar` (`Ui/BarWidget.qml:15`, `Bar.qml:1766-1772`), and `bar` is
the *root* item, so `bar.clickTargets`, `bar.moduleSlots`,
`bar.pressModuleClickTarget`, `bar.activePopout`, `bar.targetBelongsToWindow`
are all callable from `hyprpad.status/Widget.qml`; a widget can host its own
focus-taking `PanelWindow` (that is exactly what `KeyboardPanel` is, inside a
widget); and a ring can be drawn by re-parenting a `Rectangle` onto the focused
target (`ring.parent = target; anchors.fill: parent` — same window, so legal;
INFERRED, not tried). IPC would be `hyprpad.status navigate` on its existing
handler (`Widget.qml:163-164`), namespace `hyprpad-bar-nav`. Zero upstream
patch, hot-reloadable, and it *proves the interaction* before anyone argues
about it upstream. The cost: it leans on `Bar.qml` internals the README does
**not** document (`plugins/bar/README.md:154-165` lists only
`foreground/background/urgent`, `fontFamily`, `position`, `vertical`,
`barSize`, `run`, `showTooltip`, `requestPopout`), so an upstream rename breaks
it silently; and the widget is only present while the puck is connected
(`Widget.qml:78-81` — fine, since no puck means no bar mode, but the handler
sits in a hidden item; INFERRED that a `visible: false` item's `IpcHandler`
still answers, since the object is instantiated as long as the slot is in the
layout).

### (b) Purely hyprpad-side: warp the real cursor to widget centres

**Sketch.** A guide chord runs a script: `omarchy-shell shell debugBarGeometry`
→ pick the next visible slot after the one under `hyprctl cursorpos` →
`hyprctl dispatch 'hl.cursor.move({ x = cx, y = cy })'`. Click with the pad
(`rpad_click`/`r2` are `h.mouse "left"` now, `config/hyprpad.lua:137-138`) or,
in a dedicated mode, bind A to `h.mouse "left"` — that part is now expressible.

**Why it is the wrong shape**, VERIFIED:

- **The D-pad cannot drive it.** A bare button cannot `exec` (§1.6), so the
  step has to be `guide+dpad_left/right` (free chords) — two-handed, and not
  "D-pad then A".
- **Per step: a `/bin/sh` + `qs ipc` round trip** (~40 ms measured in the menu
  doc) plus a `hyprctl` spawn. Tolerable, not console-instant.
- **Geometry is per *slot*, not per target.** Workspaces are one slot with N
  buttons; indicators and tray items are invisible to it.
- **`debugBarGeometry` is explicitly a debugging dump** (`docs/omarchy-shell.md:126`)
  — no stability promise — and its coordinates are bar-content space, which
  only equals screen space for a full-width top bar (`KeyboardPanel.qml:25-31,
  139-143` explain the offset for other edges).
- **Side effects of parking the cursor on the bar**: `barHovered` and the
  centre-section hover reveal (`Bar.qml:52-56, 587-609`) fire, tooltips appear
  after 400 ms (`:919-926`), and upstream issue omacom/omarchy#8834 ("Bar centre
  section is unclickable with an absolute pointing device") is a warning that
  absolute pointer placement and the centre `CenterGestureArea` (`:1407-1487`)
  do not always agree.
- The affordance is the real cursor, which is fine, but the ring then *is* the
  pointer: moving the right pad afterwards loses it silently.

**Verdict:** works today, ~60-line script, no shell change — keep it as the
fallback if the shell patch is ever unwelcome, not as the design.

### (c) A hyprpad-owned "quick panel" plugin

**Sketch.** A `panel`-kind plugin like the cheat sheet (`shell/hyprpad.cheatsheet/manifest.json`,
`Panel.qml:207-235`: full-screen `PanelWindow`, `OnDemand` focus, own
namespace), summoned by `omarchy-shell shell toggle hyprpad.quickpanel`, drawing
a D-pad grid of tiles — Bluetooth, Network, Audio, Display, Power, Calendar,
Cheat sheet, Menu — each tile running `omarchy-shell shell toggle omarchy.<id>`
(the real panel opens over it) or a verb (`omarchy.bluetooth toggleBluetooth`,
`omarchy.power togglePercentage`).

**Contrast with (a) on "one source of truth":** the bar's layout *is* the list
of controls, and each widget's click handler *is* what it does. A quick panel
is a second, hand-maintained list that (i) has to be edited when a widget is
added or the section is rearranged (whereas the ring and `togglePanelAt` follow
the bar automatically, which is the whole point of `#6702`), (ii) can only do
what IPC exposes — mute-all, cycle-clock-format and every custom module's
`onClick` are click-only — and (iii) duplicates status the panels already show.
It also does not solve "get to the bar"; it replaces the bar for the pad.

**When it would be right:** if the goal shifts to a living-room *quick
settings* overlay (docs/08) with big tiles, live sliders and a controller-first
layout. Even then the tiles should call `shell toggle omarchy.*` so the panels
stay canonical. Effort 300-500 lines; not recommended for this ask.

### (d) What upstream has, and is building

VERIFIED against the checkout history and the `omacom/omarchy` tracker:

- **Already there:** keyboard-driven panels (`PanelKeyCatcher`, all panels);
  **Tab walks panels** (`b03a7cc7`, 2026-05-27, direct commit by DHH, no PR);
  named panel hotkeys; **positional hotkeys** `togglePanelAt` (`5edc3497`,
  #6702, 2026-08-11, "counting rather than naming means the hotkeys follow the
  bar"); panels open on the focused monitor (`667d2d2f`, #6613); an
  `omarchy.bar` IPC target (`e1fc5022`, 2026-08-30 — the natural home for new
  verbs). The manual documents all of it (`manual/05-the-top-bar.md:48-61`,
  `07-hotkeys.md:71`; https://omarchy.org/manual/the-top-bar/).
- **The owner's `Key_Back` handling is local only** (`98b38583`, not upstream).
- **Not there, and nobody has asked:** a focus ring over bar icons, or any
  gamepad/TV-remote affordance. Searches on `omacom/omarchy` (search API works
  there): `keyboard navigation bar widgets` → 8 hits, all PRs *adding* widgets;
  `focus bar widget keyboard` → 32, none about focus; `gamepad navigation` → 1
  (uinput permissions, #8373); `tv remote` → 0; `accessibility` → fonts,
  contrast, a login virtual keyboard. `gh issue list -S "bar keyboard"` finds
  keyboard-*layout* widget bugs plus the two hazards cited below (#9029,
  #8834) — nothing about focusing or walking bar widgets.
- **Adjacent open PRs worth knowing about:** #7637 "Fix positional hotkeys for
  multi-surface bar widgets" (routes `togglePanelAt` straight to the live bar
  widget instead of `shell.toggle()` — the same resolver a ring would use);
  #7975 "Make the tray the bar's organizer" (tray internals will change; do not
  build a tray ring yet); #9029 (Exclusive-focus pointer routing, the prime
  hazard in (a)); #8834 (absolute pointer vs. the bar centre, the hazard in (b)).

---

## 3. How enter/exit should feel

### 3.1 The chord

Current guide map (`config/hyprpad.lua:170-200`): `r1 l1 stick_left
stick_right a b r5 menu l2 r2 y view r4`. Free: **`dpad_up/down/left/right`**,
**`x`** (freed in `ea4b6a8`), `l3`, `r3`, `quick_access`, `stick_up/down`;
`l4`/`l5` are reserved by comment for the manual override (`:196-197`).

Recommend **`guide+dpad_up`** — "up to the bar". It is spatial (the bar is at
the top; alias `guide+dpad_down` when `position` is `bottom`), it puts entry on
the same cluster that then does the walking, and it leaves `guide+x` — the one
free *face* button — for something used constantly. Pressing it again while in
bar mode leaves (toggle), so there is always a two-press escape that does not
depend on B.

```lua
h.bind("guide+dpad_up", "Bar", h.exec "omarchy-shell -q omarchy.bar toggleNavigate")
```

### 3.2 Levels, and what B does at each

```
desktop ──guide+dpad_up──▶ ring on the bar ──A──▶ panel ──A──▶ action
   ▲                         │  ▲                   │
   └────────── B ────────────┘  └──────── B ────────┘
```

- **Ring level:** Left/Right move (wrapping left → centre → right → left),
  Down = leave on a top bar (stepping "off" the bar; Up on a bottom bar) as
  well as B. A activates; X = secondary activate (right click). L1/R1 = Tab
  order = same as Left/Right, so muscle memory from the panel level carries.
- **Panel level:** unchanged from today — arrows, A, `Tab` via R1 to the
  neighbouring panel; **B closes the panel and returns focus to the ring**
  (the nav surface re-primes when `activePopout` clears). The ring index
  follows the panel Tab moved to.
- **Inside a panel action** that grabs the keyboard (Wi-Fi passphrase, Bluetooth
  rename): Escape/B is handled by the field first (`network/Panel.qml:996`,
  `bluetooth/Panel.qml:777`), which is right — B cancels the edit, B again
  closes the panel, B again leaves the bar. Three presses from the deepest
  point to the desktop, each one a visible step.
- **Where the ring starts:** the first panel of the right section (Agents on
  this layout; the owner may want Bluetooth — a `startAt` setting or "last
  used" remembered per session). Starting on the right is deliberate: that is
  where every control the owner named lives.

### 3.3 Timeouts and affordance

- **No timeout by default.** The Deck's quick-access overlay and Big Picture's
  menus do not time out; a ring that vanishes while you read a tooltip is a
  surprise. If wanted, it is a shell-side `Timer` restarted on every key
  (hyprpad has no timer facility for modes and should not grow one for this).
- **Visual:** the accent ring/underline (§2(a).2) plus the tooltip text as a
  label; the `hyprpad.status` widget can also show a bar glyph if a distinct
  mode is declared (§3.4). Haptics: hyprpad already buzzes on a landed chord
  (`src/run.rs:1969-1973`); nothing extra needed for the ring itself since the
  synthetic keys are silent by design.

### 3.4 Interaction with `omarchy-ui` — verified against the engine

VERIFIED from `src/mode.rs`:

- Declared modes resolve **manual override > first matching rule in definition
  order > default** (`:476-485`); the layer set is folded from
  `openlayer`/`closelayer` (`:271-281`) and seeded at startup (`:291-297`).
- **Therefore bar mode must be layer-keyed, not `h.set_mode`.** A manual
  override wins over every rule (`:477-478`) until `clear_mode`, and only a
  chord can clear it (bare buttons are keys) — B could never leave. Keying on
  `ctx.layers:has("omarchy-bar-nav")` makes the shell the owner of the state and
  hyprpad a follower, exactly as with the cheat sheet and the panels.
- **Simplest correct config: add the namespace to the existing allowlist**
  (`config/hyprpad.lua:53-55`). The bindings bar mode needs are the ones
  `omarchy-ui` already has — D-pad → arrows, A → Enter, B → Back, cursor and
  scroll live (`:108,115,127-131,146`) — and when A opens a panel both
  `omarchy-bar-nav` and `omarchy-keyboard-panel` are in the set, which the
  allowlist's OR handles without a second rule. Two additions:

```lua
-- in the omarchy-ui allowlist:
"omarchy-bar-nav",
-- bumpers walk the panel ring (and, with the shell patch, the icon ring):
h.button("r1", "Next panel", h.key "tab"):only_in("omarchy-ui")
-- X = the widget's right-click action (mute all, radio, percentage) once the
-- nav surface maps it; until then leave X unbound here.
```

- **Declare a separate `bar` mode only if a binding differs.** Candidates that
  might: X as secondary-activate, `dpad_down` as leave. If either is wanted,
  declare `h.mode("bar")` **above** `omarchy-ui` keyed on
  `ctx.layers:has("omarchy-bar-nav") and not ctx.layers:has("omarchy-keyboard-panel")`
  so a panel opened from the ring drops back to `omarchy-ui`; then `status.json`
  reads `"mode": "bar"` (`src/status.rs:58-60`) and the cheat sheet gets a tab.
  The cost is duplicating the six D-pad/A/B guards onto a third mode name.
- **Mode transitions release held keys** (the "clean handoff", README) — the
  entry chord's guide release lands after the surface maps; harmless.
- **Cursor and scroll stay live** (`only_in` already includes `omarchy-ui`).
  Worth noting as a no-code trick that exists today: hover the audio or
  display icon with the right pad and the **left-pad scroll adjusts volume or
  brightness** (`audio/Panel.qml:640-647`, `monitor/Panel.qml:474-481`).

---

## 4. Recommendation and plan

| phase | what | where | effort |
|---|---|---|---|
| **0 — today, config only** | `guide+dpad_up` → `omarchy-shell -q shell togglePanelAt right 1` (or `shell toggle omarchy.bluetooth`); `r1` → `tab` in `omarchy-ui`. Result: chord opens a panel, R1 walks Agents → Bluetooth → Network → Audio → Display → Power, D-pad/A drive it, B closes. Add `guide+dpad_down` → `togglePanelAt center 1` for clock/weather if wanted. | `config/hyprpad.lua` | 10 min, no code, no shell change. Verify live that a synthetic Tab reaches a panel (not done here — it would have opened one). |
| **1 — the ring, prototyped as (a′)** | `hyprpad.bar` (or inside `hyprpad.status`): nav `PanelWindow` `hyprpad-bar-nav`, ring over `bar.clickTargets`, `triggerPress`, yield on `activePopout`, IPC `navigate/leave/toggleNavigate`. hyprpad: allowlist +1, chord repointed. Settles the feel (start position, wrap, Down-to-leave, secondary activate) without touching upstream code. | `shell/hyprpad.status/` or a new `shell/hyprpad.bar/`, `scripts/hyprpad-bar`, `config/hyprpad.lua` | 1 day |
| **2 — the shell patch (a), upstream-shaped** | Port the proven ring into `Bar.qml` + `BarNavigator.qml` + `BarModel.js` with tests, verbs on `omarchy.bar`, namespace `omarchy-bar-nav`, docs + manual; open the PR as keyboard accessibility (`SUPER+CTRL+Up`). Drop the (a′) plugin when it lands. | `shell/plugins/bar/Bar.qml`, `shell/plugins/bar/BarModel.js`, new `shell/plugins/bar/BarNavigator.qml`, `shell/Ui/WidgetButton.qml` (optional `navFocused`), `test/shell.d/bar-test.sh`, `docs/omarchy-shell.md`, `manual/05-the-top-bar.md` | 1-2 days + review time |
| **3 — polish** | tooltip label on focus; multi-monitor (focused screen); vertical bars; hidden-bar reveal; tray per-item ring (after #7975 settles); `PageUp/PageDown` as ±1 in `PanelKeyCatcher` so L1 can walk backwards without a daemon change; optional timeout setting. | as above | incremental |

**Why this order:** phase 0 delivers Bluetooth/audio/display/power on the pad
this afternoon using only what upstream already built and documents. Phase 1
answers the only open *design* questions (feel) at the lowest cost and with
zero rebase burden. Phase 2 is the right end state because the bar is the
single source of truth for what the bar can do, and a focus ring belongs to the
component that owns the click targets — and framed as accessibility it is a
better upstream proposal than "controller support", for the same reason
`Key_Back` was.

**What it costs on the controller map:** one chord (`guide+dpad_up`) and one
bare button in one mode (`r1` → Tab). Everything else is keys the pad already
sends. That is the "use that space efficiently" outcome the owner asked for.

---

## 5. Sources

Shell (`~/code/omarchy-bluetooth-friendly-name/shell/`, HEAD `98b38583`):

- `plugins/bar/Bar.qml` — click registry `:104-114`; `debugBarGeometry`
  `:128-152`; `targetBelongsToWindow` `:158-160`; tooltip hover test `:173-175`;
  popout coordination `:83, :316-327`; `panelNavigationSlots` `:401-418`;
  `panelWidgetIdAt` `:429-433`; `switchPanelFrom` `:435-466`; `findPanelWidget`
  `:500-516`; `summon/hide/isBarWidgetOpen` `:518-535`; `focusedScreenName`
  `:490-493`; `moduleTargetClickable`/`moduleClickTargetAt`/`pressModuleClickTarget`
  `:752-790`; `showTooltip` `:885-926`; `IpcHandler omarchy.bar` `:955-964`;
  `BarPanel` `:1004-1039` (namespace/layer, no `keyboardFocus`); `ModuleSlot`
  `:1543-1778` (`panelOpen` `:1570`, `openPanelIndicator` `:1641-1667`,
  `injectProps` `:1766-1772`).
- `plugins/bar/BarModel.js:166-178` (`pickPanelSlot`); `plugins/bar/README.md:51-75, 154-165`.
- `Ui/WidgetButton.qml:22-23, 35-38, 44-55, 95-118`; `Ui/BarIconButton.qml:5`;
  `Ui/BarIndicator.qml:4, 41-42`; `Ui/BarWidget.qml:15, 29-35`.
- `Ui/Panel.qml:21-35, 48-57`; `Ui/PanelController.qml`; `Ui/PanelKeyCatcher.qml:51-78`;
  `Ui/KeyboardPanel.qml:16-19, 25-31, 85-100, 139-143, 226-259, 302-322`.
- Widgets: `plugins/menu/BarWidget.qml:18-22`; `plugins/panels/clock/BarWidget.qml:63-78,
  133-144, 158-162`, `Panel.qml:247-257`; `plugins/panels/weather/BarWidget.qml:22-37,
  76-81`, `Panel.qml:476-504`; `plugins/bar/widgets/KeyboardLayout.qml:157-161, 216, 287`;
  `plugins/bar/widgets/SystemUpdate.qml:20-38, 63`; `plugins/bar/widgets/Tray.qml:36-40,
  100-131, 262-275, 818-846`; `plugins/bar/widgets/Indicators.qml:168-174`;
  `plugins/bar/indicators/Dnd.qml:17-21`; `plugins/agents/Panel.qml:62-65, 327-379`;
  `plugins/panels/bluetooth/Panel.qml:739-790`; `plugins/panels/network/Panel.qml:210-217,
  956-1075`; `plugins/panels/audio/Panel.qml:630-675`; `plugins/panels/monitor/Panel.qml:220-229,
  468-508`; `plugins/panels/power/Panel.qml:177-184, 285-312`.
- `shell.qml:480-513` (hide/isPluginOpen/toggle), `:567-579` (`callIfLoaded`),
  `:872-1030` (`shell` IPC: `debugBarGeometry` `:998`, `toggle` `:1010`,
  `togglePanelAt` `:1018-1025`, `call` `:1027`).
- Checkout, outside `shell/`: `default/hypr/bindings/utilities.lua:97-115`;
  `docs/omarchy-shell.md:99-130`; `manual/05-the-top-bar.md:48, 59, 61`;
  `manual/07-hotkeys.md:71`; commits `b03a7cc7` (Tab switches panels),
  `5edc3497` (#6702 positional hotkeys), `667d2d2f` (#6613 focused monitor),
  `e1fc5022` (`omarchy.bar` IPC), `98b38583` (owner's `Key_Back`).
- Owner's plugins: `~/.config/omarchy/plugins/ajg.workspaces/Workspaces.qml:38-41, 59-74`;
  `~/.config/omarchy/plugins/hyprpad.status/Widget.qml:1-33, 66-81, 163-164, 210-224`;
  `~/.config/omarchy/shell.json` (bar layout).

hyprpad (`~/code/hyprsc`, `e2ddad0`):

- `config/hyprpad.lua:41-59` (modes, `omarchy-ui` allowlist `:52-59`), `:102-116`
  (cursor/scroll guards), `:127-146` (bare buttons, B per mode), `:137-139`
  (mouse bindings), `:152-153` (bumpers under `cheatsheet`), `:170-200` (chord map).
- `src/mode.rs:16-29` (precedence), `:271-297` (layers), `:299-316` (manual
  override), `:476-485` (declared resolution). `src/config.rs:95-125`
  (`Action`), `:196-200, 362-379` (mouse codes), `:310-329` (key table).
  `src/lua_config.rs:1015, 1024-1029` (bare-button rule). `src/run.rs:1937-1995`
  (`handle_gesture`: SetMode/ClearMode, exec with `HYPRPAD_MODE`), `:929-932`
  (pad click). `src/hypr.rs:145-155` (`dispatch_raw`), `:184-200` (`open_layers`),
  `:207-232` (`spawn_env`). `src/output.rs:129-131` (relative pointer only).
  `src/status.rs:58-60`. `shell/hyprpad.cheatsheet/Panel.qml:66-99, 207-235`;
  `scripts/hyprpad-cheatsheet`.
- `docs/research/omarchy-menu-navigation.md` (namespace table §1.1, key handling
  §2, IPC verdict §3.4, exclusion rule §4.3).

External:

- HypXRland `hl.cursor.move`: `~/code/hypxrland/src/config/lua/bindings/LuaBindingsDispatchers.cpp:39-41, 77-85`.
- Quickshell `keyboardFocus` default: `src/wayland/wlr_layershell/wlr_layershell.hpp:107`
  via `gh api repos/quickshell-mirror/quickshell/contents/...` (the docs site
  returns 403 to fetches). Installed: Quickshell 0.3.1.
- Omarchy manual: https://omarchy.org/manual/the-top-bar/ ("Every panel takes
  the keyboard as well as the mouse: arrows move, Return activates, Tab steps to
  the neighbouring panel, and Escape closes"; `Super + Ctrl + 1-9`).
- Tracker (`omacom/omarchy`): PR #7637 https://github.com/omacom/omarchy/pull/7637;
  PR #7975 https://github.com/omacom/omarchy/pull/7975; issue #9029
  https://github.com/omacom/omarchy/issues/9029; issue #8834
  https://github.com/omacom/omarchy/issues/8834; issue #8373
  https://github.com/omacom/omarchy/issues/8373. Searches: `gh api
  "search/issues?q=repo:omacom/omarchy+<terms>"` for `keyboard navigation bar
  widgets`, `focus bar widget keyboard`, `gamepad navigation`, `controller
  navigate menu`, `tv remote`, `switch panels Tab`; `gh issue list -R
  omacom/omarchy -S "<terms>"` for `bar keyboard`, `gamepad`, `controller`,
  `accessibility`, `focus ring`.

---

## Appendix — commands used for live verification (all read-only)

```sh
hyprctl layers -j                       # baseline: omarchy-background + omarchy-bar only
hyprctl cursorpos                       # 185, 937  (logical px)
hyprctl monitors -j                     # eDP-2 2560x1600 @1.25 → 2048x1280 logical, reserved [0,26,0,0]
omarchy-shell shell debugBarGeometry    # per-slot x/y/w/h/visible
omarchy-shell shell listPlugins         # enabled widgets and kinds
omarchy-shell --help                    # no `bar` or `shell --help` subcommands exist (Function/Target not found)
git -C ~/code/omarchy-bluetooth-friendly-name log -S switchPanelFrom -- shell/plugins/bar/Bar.qml   # b03a7cc7
git -C ~/code/omarchy-bluetooth-friendly-name log -S togglePanelAt   -- shell/shell.qml            # 5edc3497
gh api "repos/omacom/omarchy/commits?path=shell/plugins/bar/Bar.qml&per_page=15"
gh api "search/issues?q=repo:omacom/omarchy+keyboard+navigation+bar+widgets"
strings "$(command -v Hyprland)" | grep -i movecursor
```

Nothing was opened, toggled, injected, installed or restarted.
