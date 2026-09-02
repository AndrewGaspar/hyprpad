# Getting to an empty workspace from the controller

Research for: "I need some way to easily get to or spawn a new workspace.
Normally I'd hit super + number. All I have bound on the controller is
workspace ±1. I need some way to get to an empty workspace — almost just a
button. Ideal would be to allocate spatially relative to my current one, but
that could have confusing semantics (what if both neighbours are occupied?).
Or just allocate the first empty one."

Everything below is marked **VERIFIED** (read from source at a cited line, or
observed live on this machine on 2026-09-01 with read-only `hyprctl -j`
queries) or **INFERRED**. Nothing was dispatched, nothing under `~/.config`
was touched.

Sources of truth:

- Compositor source: `/home/ajg/code/Hyprland`, branch `hypxrland`, HEAD
  `7b7e1939d`. The *running* compositor is `Hyprland 0.56.2 … hypxrland at
  commit 67200a838` (`hyprctl version`, VERIFIED). Line cites are HEAD; I did
  not diff the cited files between the two commits.
- hyprpad: this repo at `master` (`e1afe64`), plus the uncommitted edit to
  `config/hyprpad.lua` that landed during this research (it freed `guide+x`).
- Owner's Hyprland config: `~/.config/hypr/*.lua`, with Omarchy defaults from
  `$OMARCHY_PATH = /home/ajg/code/omarchy-bluetooth-friendly-name/default/hypr/`
  (the dev checkout is the live one; see `omarchy-menu-navigation.md`).
- Bar: `~/.config/omarchy/plugins/ajg.workspaces/Workspaces.qml`.
- Upstream wiki source: `hyprwm/hyprland-wiki@main`,
  `content/configuring/naming-conventions.md` (selector forms) and
  `content/configuring/core/dispatchers.md` (Lua dispatcher table). The
  rendered page `https://wiki.hypr.land/Configuring/Dispatchers/` now 404s;
  the new rendered paths are presumably
  `https://wiki.hypr.land/configuring/naming-conventions/` and
  `https://wiki.hypr.land/configuring/core/dispatchers/` (INFERRED — I could
  only fetch the markdown from GitHub).

---

## 0. Recommendation in one paragraph

Bind one face button to **`emptyn`** — "the first empty workspace to the
*right* of the one I'm on, creating `max+1` if there is none" — because it is
the only stock selector whose answer to "what if both neighbours are
occupied?" is simple (skip right until a free slot), it never wraps, never
picks the workspace you are standing on, keeps the existing numbers stable,
and the bar shows exactly where you landed. It works **today with zero hyprpad
code** as `h.dispatch 'hl.dsp.focus({ workspace = "emptyn" })'`, and a
~40-line change lets it be spelled `h.workspace "emptyn"` (which, as things
stand, would silently create a *named* workspace called "emptyn" — see §1.1).
Put it on `guide+x` (just freed; "B closes, X opens"), with `guide+stick_up`
as the flick-family alias and `guide+dpad_up` for "take this window to a new
workspace". Spatial *insertion* (renumbering) is possible on this fork via
`hl.dsp.workspace.change_id` but would break the owner's `SUPER+number`
muscle memory; a special workspace is a scratchpad, not a new place, and is
invisible in the bar. Details, edge cases and the alternatives follow.

---

## 1. Where things stand today (VERIFIED)

### 1.1 hyprpad's workspace path

| Layer | What it does | Cite |
|---|---|---|
| Lua front-end | `h.workspace(x)` / `h.move_to_workspace(x)` / `h.dispatch(expr)` are one-arg constructors that re-enter the TOML grammar as `workspace <x>` etc. | `src/lua_config.rs:791-797`, `:1444-1447` |
| Grammar | `WorkspaceTarget::parse`: `+n`/`-n` → `Relative(n)`; bare integer → `Number(n)`; **anything else → `Named(s)`**. No notion of `empty`, `previous`, `r+1`, `special:` … | `src/config.rs:54-62`, `:67-90` |
| Execute | `Relative(n)` → `hl.dsp.focus({ workspace = "e±n" })`; `Number(n)` → `workspace_named("n")`; `Named(s)` → `workspace_named("name:s")`. Move variants use `hl.dsp.window.move({ workspace = … })`. | `src/run.rs:2164-2177`, `src/hypr.rs:93-98`, `:103-113`, `:117-122`, `:519-526` |
| Escape hatch | `Action::Dispatch(p)` → `dispatch_raw(p)` = raw `dispatch <lua expr>` over `.socket.sock`; replies `ok` or `error:`. | `src/run.rs:2183`, `src/hypr.rs:149-155` |
| Queries | `Hypr::query("workspaces")` → `j/workspaces` JSON; tiny `json_number`/`json_string` helpers exist (used by `parse_active_window`). | `src/hypr.rs:159`, `:529` |
| Sheet | `derive_label` prints `Named(s)` as "Workspace s"; `action_string` round-trips it as `workspace s`. | `src/bindings_sheet.rs:646-647`, `:687-690`, `:723-726`, `:750-756` |

Two traps in that table (VERIFIED by reading, **not** exercised live):

1. **`h.workspace "empty"` does not mean "empty".** It parses as
   `Named("empty")` and is sent as `hl.dsp.focus({ workspace = "name:empty" })`.
   On the compositor side `name:` looks the workspace up by name and, if it
   does not exist, allocates a *named* workspace with a **negative** id
   (`src/helpers/MiscFunctions.cpp:142-149` → `nextAvailableNamedWorkspace()`).
   The bar hides negative ids (§1.3). So the naive spelling creates a hidden
   workspace literally called "empty". Until §5's change lands, use
   `h.dispatch`.
2. **`h.workspace(7)` sends `name:7`, not `7`.** `run.rs:2166` passes the
   bare digits to `workspace_named`, which prefixes `name:` whenever there is
   no `:` (`hypr.rs:107-111`). For an *existing* numeric workspace this works
   by accident (its `m_name` is `"7"`, VERIFIED in `hyprctl -j workspaces`);
   for a **non-existent** one it creates a named workspace "7" with a negative
   id. Nothing in the owner's config uses `h.workspace(N)` today, so this is
   latent — but option (e) below would trip over it. Same bug for
   `h.workspace "special:x"`: `run.rs:2167` pre-qualifies it as
   `name:special:x` (the `:` passthrough in `hypr.rs:107` never gets a chance).

### 1.2 What the owner has on the keyboard

- `SUPER + 1..0` → `hl.dsp.focus({ workspace = "N" })` (Omarchy default,
  `default/hypr/bindings/tiling.lua:21-25`; `SHIFT` moves, `SHIFT+ALT` moves
  silently). Not overridden.
- `SUPER + CTRL + L / H` → `e+1` / `e-1` (`~/.config/hypr/bindings.lua:59-60`).
  `SUPER + TAB` is re-purposed to group cycling (`:63-64`, `:18-19`).
- `SUPER + CTRL + TAB` → `previous` (`tiling.lua:35`); `SUPER + S` / `SUPER +
  grave` → `hl.dsp.workspace.toggle_special("scratchpad")` (`tiling.lua:28-31`).
  That special is Omarchy's "qconsole": a workspace rule seeds `omarchy-agent`
  into it on creation (`default/hypr/qconsole.lua:15`, `:38-39`; live
  `hyprctl -j workspacerules` shows `onCreatedEmpty` on `special:scratchpad`).
- 3-finger horizontal swipe = workspace swipe, `workspace_swipe_distance = 700`
  (`~/.config/hypr/input.lua:31-38`). Workspace slide animation on
  (`looknfeel.lua:18`).
- Options, live (`hyprctl -j getoption`): `binds:allow_workspace_cycles`
  false (unset), `binds:workspace_back_and_forth` false, `binds:workspace_center_on`
  1 (default), `binds:hide_special_on_workspace_change` **true** (set by Omarchy,
  `default/hypr/looknfeel.lua:123-125`), `misc:initial_workspace_tracking` 0.
- **No per-monitor or persistent workspaces.** `hyprctl -j workspacerules`
  lists only `special:scratchpad`, `w[tv1]`, `f[1]` (the last two are the
  owner's no-gaps rules, `looknfeel.lua:21-25`); every workspace reports
  `ispersistent: false`. One monitor active (`eDP-2`); the XReal glasses are a
  second output when plugged in (`monitors.lua:17-19`).

### 1.3 What the bar shows

`Workspaces.qml:22-36`: the widget lists every workspace Hyprland currently has
with `id > 0` (`:29`; plus the focused one, `:32`), sorted numerically. Special
workspaces (negative ids) and named workspaces (negative ids) are **never
shown**. The focused pill is drawn as a glyph, not its number (`:67`); an
unoccupied but alive pill is dimmed to 50 % (`:68`); `10` prints as `0`.
Clicking dispatches `hl.dsp.focus({ workspace = "<id>" })` (`:40`). The comment at `:20-21`
("Hyprland reaps empty, unseen ones") matches the live state: workspace 1 is
absent from `hyprctl -j workspaces` while 2, 3, 4, 7, 10 exist with one window
each. So **an empty workspace exists only while you are looking at it**; leave
it and its pill disappears. (I did not locate the reaping routine by name in
the fork — see §6.)

### 1.4 Live snapshot, and what each selector would resolve to

Workspaces `{2, 3, 4, 7, 10}`, all occupied, active `7`, one monitor. Applying
the parser in §2.2:

| selector | lands on | note |
|---|---|---|
| `empty`, `emptym` | **1** | lowest id with no windows; 1 doesn't exist → created |
| `emptyn`, `emptynm` | **8** | first free id strictly above 7 |
| `e+1` (today's `guide+r1`) | 10 | existing only, wraps 10 → 2 |
| `r+1` | 8 | "including empty/non-existent", creates 8 |
| `+1`, `next` | 8 | literal id+1 |
| `previous` | whatever was focused before 7 | history, not geometry |
| max+1 (no selector; hyprpad-computed) | 11 | |

---

## 2. What HypXRland offers

### 2.1 Spelling in the fork's Lua API (VERIFIED)

Classic `dispatch workspace empty` is a Lua syntax error on this fork
(`src/hypr.rs:18-20`). The dispatcher table is built in
`src/config/lua/bindings/LuaBindingsDispatchers.cpp:1381-1457`
(`registerDispatcherBindings`):

| Intent | Lua dispatcher | Cite |
|---|---|---|
| go to workspace `<sel>` | `hl.dsp.focus({ workspace = "<sel>" })` | `hlFocus` `:1179-1237`; workspace branch `:1200-1212` → `dsp_changeWorkspace` `:1168-1170` → `CA::changeWorkspace(string)` |
| … pulling it onto this monitor (`focusworkspaceoncurrentmonitor`) | `hl.dsp.focus({ workspace = "<sel>", on_current_monitor = true })` | `:1206-1209` → `dsp_focusWorkspaceOnCurrentMonitor` `:1172-1177` → `CA::changeWorkspaceOnCurrentMonitor` |
| move active window to `<sel>` and follow (`movetoworkspace`) | `hl.dsp.window.move({ workspace = "<sel>" })` | `:898-906` → `dsp_moveToWorkspace` `:519-526` → `CA::moveToWorkspace(ws, silent=false)` |
| … without following (`movetoworkspacesilent`) | `hl.dsp.window.move({ workspace = "<sel>", follow = false })` | `:900-901` (`silent = follow.has_value() && !*follow`) |
| toggle special (`togglespecialworkspace`) | `hl.dsp.workspace.toggle_special("name")` — applies the `special:` prefix itself | `:1245-1261`, `:1311-1315`; wiki `dispatchers.md:191` |
| open (not toggle) a special | `hl.dsp.focus({ workspace = "special:name" })` | `CA::changeWorkspace(ws)` special branch, `ConfigActions.cpp:946-950` (`setSpecialWorkspace`) |
| rename | `hl.dsp.workspace.rename({ workspace = "<sel>", name = "…" })` | `:1263-1269`, `:1318-1332` |
| **renumber** (no classic equivalent) | `hl.dsp.workspace.change_id({ workspace = "<sel>", id = N })` — `N > 0`, refused for named/special ("managed id") and for a taken id | `:1271-1278`, `:1334-1347`; `ConfigActions.cpp:1074-1083` |
| move workspace to monitor | `hl.dsp.workspace.move({ workspace? = "<sel>", monitor = "<mon>" })` | `:1350-1366` |
| swap monitors' active workspaces | `hl.dsp.workspace.swap_monitors({ monitor1, monitor2 })` | `:1369-1379` |

The `workspace = …` field is **not validated at config time** — any string (or
a workspace object) is accepted and resolved when the dispatcher fires
(`LuaBindingsInternal.cpp:179-200`). So `"empty"`, `"emptyn"`, `"r+1"`,
`"previous"` are all legal here; the in-tree test suite exercises
`hl.dsp.focus({ workspace = 'empty' })`, `'m+1'`, `'r+1'`, `'r~1'`,
`'previous'` (`hyprtester/src/tests/main/workspaces.cpp:480-535`; from
workspace 1 with 2 absent, `empty` lands on 2 — `:523-529`).

### 2.2 Selector semantics (`getWorkspaceIDNameFromString`, `src/helpers/MiscFunctions.cpp:126-466`, VERIFIED)

| form | resolves to | wraps? | can pick the current ws? | cite |
|---|---|---|---|---|
| `empty` | the **lowest id ≥ 1** whose workspace either does not exist or has `getWindowCount() == 0` — searched across all monitors | n/a | yes, if it is the lowest empty (then the switch is a no-op) | `:150-176` (loop `:170-176`, `id` starts at 0) |
| `emptym` | same, but ids that a **workspace rule** binds to another monitor are skipped. It does *not* skip an existing empty workspace that merely lives on another monitor | n/a | yes | `:151`, `:159-168` |
| `emptyn` | the lowest empty id **strictly greater than the active workspace's id** | **no** — walks up to `LONG_MAX`, creating `max+1` when nothing is free | **never** | `:152`, `:170` (`id = next ? active : 0`, then `++id`) |
| `emptynm` / `emptymn` | both flags (`contains("m")`, `contains("n")`, so any order) | no | never | `:151-152` |
| `previous`, `previous_per_monitor` | MRU history via `WorkspaceHistoryTracker`; handled *before* the parser in `resolveWorkspaceForChange` | – | no (returns null if prev == current) | `ConfigActions.cpp:1022-1033`; `MiscFunctions.cpp:178-199` |
| `next` | literally `active + 1` | no | no | `:201-213` |
| `+n` / `-n` | `active ± n`, clamped to ≥ 1 | no | – | `:447-455` |
| `N` | `max(N, 1)` | – | – | `:456-457` |
| `name:foo` | existing workspace named `foo`, else a **new negative-id named** workspace | – | – | `:142-149` |
| `special` / `special:foo` | the special workspace (created on demand) | – | – | `:129-141` |
| `r±n`, `r~n` | walk over ids **on this monitor including non-existent ones**, skipping ids rule-bound elsewhere; named workspaces sit "before" 1 | up: no (creates); down: falls into named ws or bounces back up | – | `:218-373` |
| `m±n`, `m~n` | walk over **existing, non-special** workspaces on this monitor, sorted | **yes** (modulo) | – | `:381-446`, wrap at `:425` |
| `e±n`, `e~n` | as `m` but across all monitors — what `guide+r1`/`l1` send today | **yes** | – | `:382`, `:425` |

Wiki wording (`naming-conventions.md:159-167`, "Workspace search"): *"`empty` -
Search for first empty workspace. Suffix with `m` to only search on monitor,
and/or `n` to make it the next available empty workspace (e.g., `emptynm`)"*;
*"`r` - Search for workspace on current monitor including empty/non-existant
workspaces"*; *"`e` - Search on all monitors"*.

### 2.3 What happens after resolution (VERIFIED)

- `CA::changeWorkspace(string)` (`ConfigActions.cpp:1058-1063` →
  `resolveWorkspaceForChange` `:1008-1056`): the workspace is **created on the
  focused monitor if it does not exist** (`:1052-1054`), so `empty*`/`r+`/`N`
  can always land somewhere. `binds:workspace_back_and_forth` only matters when
  the target *is* the current workspace (`:1039-1050`).
- `CA::changeWorkspace(ws)` (`:935-1006`): if the workspace is owned by
  **another monitor, focus moves to that monitor** (`:952-968`, `:974-986`) —
  relevant when the glasses are plugged in and `empty`/`emptyn` returns an id
  that is an existing, empty workspace currently *shown* on the other output
  (only *rule*-bound ids are excluded by `m`, §2.2). `on_current_monitor =
  true` instead swaps/moves it onto the focused monitor
  (`CA::changeWorkspaceOnCurrentMonitor` `:1097-1119`). With
  `hide_special_on_workspace_change = true` (the owner's setting) any open
  special is hidden first (`:970-971`). `workspace_center_on` (`:940`,
  `:980-981`) only decides where the cursor warps on a cross-monitor switch;
  on an empty workspace there is no window to centre on.
- `binds:allow_workspace_cycles` (`ConfigValues.cpp:543`: "workspaces don't
  forget their previous workspace") only changes how `previous` history is
  de-duplicated (`WorkspaceHistoryTracker.cpp:40-43`). Irrelevant to `empty*`.
- `misc:close_special_on_empty` defaults **true** (`ConfigValues.cpp:516`): a
  special closes itself when its last window goes.
- Window-rule `workspace = "empty…"` (not the dispatcher) has one extra
  special case: if the spawning workspace is itself empty the window stays
  there (`src/desktop/view/Window.cpp:2402-2405`). The *dispatcher* has no such
  clause, but it cannot matter — the window being moved makes its own
  workspace non-empty.

---

## 3. Semantics options for the controller

### (a) First empty — `empty` / `emptym`

- Does: lowest free id, i.e. almost always **workspace 1** once Hyprland has
  reaped it (§1.4). From 7 you jump *left* to 1.
- Pros: one dispatcher, deterministic, zero hyprpad code.
- Cons: reads as "go home", not "new"; the destination is not spatially related
  to where you are. If you are already standing on the lowest empty it is a
  silent no-op; if you are on an empty 8 and 1 is free it *moves you to 1*.
  With two outputs it can pick an empty workspace that is visible on the other
  monitor and focus that monitor (§2.3) — add `on_current_monitor = true`.

### (b) Next empty to the right — `emptyn` (recommended)

- Does: first free id above the current one; creates `max+1` if none.
  `{2,3,4,7,10}`: from 7 → 8, from 2 → 5, from 10 → 11. Never wraps, never
  returns the current workspace.
- Answers the owner's "both neighbours occupied?" worry with the simplest
  rule: *skip right until free*. The bar shows the new pill at its sorted
  position so you always see where you landed.
- Edge cases: pressing twice walks 8 → 9 (8 is reaped as you leave, so no
  litter, but the pill visibly hops). Holes to the *left* (1, 5, 6) are never
  reused from a higher workspace — fine, they are reachable by `guide+l1`.
  Same two-monitor caveat as (a). Ids above 10 are unreachable by
  `SUPER+number` but the bar prints them (`11`, `12`, …; only 10 is
  special-cased to `0`).
- Optional nicety (needs code): if the current workspace is already empty, do
  nothing — one `j/activeworkspace` query (`"windows": 0`).

### (c) Spatial insert — "put a new workspace between me and my right neighbour"

- Hyprland has **no insert**. But this fork exposes `hl.dsp.workspace.change_id`
  (§2.1; upstream has the same dispatcher in its Lua API, wiki
  `dispatchers.md:134`), so renumbering is possible: from `C`, for every
  existing id `> C` in **descending** order (a taken id is refused,
  `ConfigActions.cpp:1078-1079`) `change_id(id → id+1)`, then
  `focus({ workspace = tostring(C+1) })`. Refused for named/special
  workspaces (`m_id <= 0`), so a named workspace in the way aborts the shift.
- When `C+1` is already free this degenerates to (b), so the only case it buys
  is "right neighbour occupied", and what it buys is exactly what makes it
  confusing: **every workspace to the right changes number**, so
  `SUPER+number`, the bar labels, and any `workspace = N` window rule now point
  at different content. The owner still uses `SUPER+number` on the keyboard;
  that is the decisive argument against.
- Cost: hyprpad-side it is a `j/workspaces` query plus N round trips (each a
  separate socket connection, `hypr.rs:149-155`), with a race if a window
  opens mid-shift. Cheaper and atomic would be a Lua function in
  `hyprland.lua` that does the loop in-compositor and returns the final
  `hl.dsp.focus` dispatcher; hyprpad would then send
  `h.dispatch "ajg.workspace_insert_right()"`. **INFERRED**: the socket wraps
  the payload as `return hl.dispatch(<expr>)` (`hypr.rs:13-15`), so any
  expression evaluating to a dispatcher should work, but I did not verify that
  globals from the config script are visible to socket-evaluated expressions.
- Verdict: possible, not recommended.

### (d) A special workspace as "the new place" — `hl.dsp.workspace.toggle_special("pad")`

- Does: overlays a scratchpad on the current workspace; toggle again to hide.
  Must not be `scratchpad`: that one auto-spawns `omarchy-agent` (§1.2).
- Pros: a true toggle (one button both ways), instant, always "empty" the
  first time, closes itself when its last window leaves
  (`close_special_on_empty`), hidden automatically on `guide+r1`/`l1`
  (`hide_special_on_workspace_change = true`).
- Cons: **invisible in the bar** (negative id filtered, §1.3) — the exact
  "where am I" problem the owner wants to avoid; it is one fixed place, not a
  fresh one each press; windows left there are easy to forget; overlays rather
  than replaces (dimmed underlay per Omarchy's qconsole rule only for
  `scratchpad`). Showing an "S" pill would be a small edit to the owner's
  widget (`Hyprland.workspaces` does contain negative ids; the filter at
  `Workspaces.qml:26` drops them) — read-only here, so noted, not done.
- Verdict: good for "park this window", not for "new workspace".

### (e) Append at the end — `max+1`

- Does: the macOS/GNOME dynamic-workspaces feel; `{2,3,4,7,10}` → 11, then 12.
  Ordering of existing workspaces never changes.
- No selector does this: `r+1` only creates when you are already on the
  highest; from the middle it goes to `C+1` even if occupied. So this is
  **hyprpad-computed**: `j/workspaces` → max positive id → `hl.dsp.focus({
  workspace = "<max+1>" })` (send the bare digits via `dispatch_raw`, not
  through `WorkspaceTarget::Number`, until trap 2 in §1.1 is fixed).
- Holes are never reused, so numbers drift upward for the life of the session
  (12, 13, …) and quickly leave `SUPER+1..0` range; the bar copes. From the
  highest workspace (e) and (b) are identical, which is the common case when
  you work left-to-right.
- Verdict: nice feel, ~30 lines of hyprpad, but (b) gives the same result
  most of the time with none of the drift.

### (f) Take this window to a fresh workspace — `hl.dsp.window.move({ workspace = "emptyn" })`

- Does: moves the focused window to the next free id to the right and follows
  it (`follow = false` to stay). Pairs naturally with (b) as the `SHIFT`
  variant of the same button.
- Edge cases: if the window was alone on its workspace, the old one is reaped
  the moment it empties, so the bar shows the pill "jump" from 7 to 8 — a
  renumber in effect, harmless. If you'd rather it stayed put when alone, a
  guard is one `j/activeworkspace` query (`"windows": 1` → no-op). Using
  `empty` instead of `emptyn` here would drag windows down to 1.

---

## 4. Binding proposals

### 4.1 Free guide chords (VERIFIED against `config/hyprpad.lua` as of this research)

Bound today (`config/hyprpad.lua:170-180`, `:189`, `:200`): `guide+r1`, `l1`,
`stick_right`, `stick_left`, `a`, `b`, `r5`, `menu`, `l2`, `r2`, `y`, `view`,
`r4`. Pencilled in comments for the manual override (`:196-197`): `guide+l4`,
`guide+l5`.

Free and bindable (`src/config.rs:235-300` — `parse_button` and the
flick prefixes):

- **`guide+x`** — freed by the latest config edit (the launcher moved to
  `guide+menu`). A face button, opposite `guide+b` = close window.
- `guide+dpad_up` / `dpad_down` / `dpad_left` / `dpad_right`.
- **`guide+stick_up` / `guide+stick_down`** — flicks fire on the dominant axis
  (`src/gesture.rs:171-191`, `:440-446`), so up/down are independent of the
  left/right flicks already bound.
- `guide+lstick_{up,down,left,right}` (left-stick flicks).
- `guide+r3`, `guide+l3` (stick clicks; R3 shows accidental repeats during
  stick circles, `docs/03-hardware-findings.md:154` — avoid for anything
  destructive), `guide+quickaccess`, `guide+rpad_click`, `guide+lpad_click`.
- `guide_hold` (`GestureKey::Hold`).

Not available: **trigger soft pulls** (only `r2`/`l2` *full* pulls exist as
buttons, `src/config.rs:281-282`); **`guide_tap`** parses but is dead by design
— a bare guide tap is handed to Steam before bindings are consulted
(`src/run.rs:1945-1949`); pad touches.

### 4.2 Proposed bindings

```lua
-- New workspace: the first empty one to the right of this one (created at
-- the end if none). "B closes, X opens".
h.bind("guide+x",        "New workspace",        h.dispatch 'hl.dsp.focus({ workspace = "emptyn" })')
-- Same thing as a flick, so the right stick is a whole family:
-- left/right = ±1, up = new, down = back to where I was.
h.bind("guide+stick_up",   "New workspace",      h.dispatch 'hl.dsp.focus({ workspace = "emptyn" })')
h.bind("guide+stick_down", "Previous workspace", h.dispatch 'hl.dsp.focus({ workspace = "previous" })')
-- Take the focused window with me to a new workspace.
h.bind("guide+dpad_up",  "Window to new workspace", h.dispatch 'hl.dsp.window.move({ workspace = "emptyn" })')
```

All four work **without touching hyprpad's Rust** (`Action::Dispatch` →
`dispatch_raw`). When the XReal glasses are the second output, add
`, on_current_monitor = true` inside the `focus` table (§2.3).

With the §5 change they become `h.workspace "emptyn"`, `h.workspace
"previous"`, `h.move_to_workspace "emptyn"` — shorter, and the cheat sheet
derives a sane label without a description.

### 4.3 Does `WorkspaceTarget::parse` accept `empty` today?

No (§1.1): it falls through to `Named("empty")` and is dispatched as
`name:empty`, which creates a hidden named workspace. Minimal change:

1. `src/config.rs` — add `WorkspaceTarget::Selector(String)` and, in `parse`,
   route these to it before the `Named` fallback: `empty` + any of `m`/`n`
   suffixes, `previous`, `previous_per_monitor`, `next`, `[rme][+-~]<digits>`,
   and any token containing `:` (`name:`, `special:`). Keep `Named` for bare
   words.
2. `src/hypr.rs` — add `workspace_selector(&str)` / `move_window_to_workspace_selector(&str)`
   that pass the string through verbatim (Lua-escaped) into
   `hl.dsp.focus({ workspace = … })` / `hl.dsp.window.move({ workspace = … })`.
   While there, make `Number(n)` send bare digits and stop `run.rs:2167`
   pre-qualifying an already-qualified `Named` (§1.1 trap 2).
3. `src/run.rs:2164-2177` — dispatch the new variant.
4. `src/bindings_sheet.rs` — `target()` / `relative_words()` print the
   selector; optionally map `empty*` → "New workspace" and `previous` →
   "Previous workspace" in `derive_label`.
5. Tests: `parses_workspace_selectors` in `config.rs`; the existing
   `relative_workspace_argument` style test in `hypr.rs` for the new string
   builders; a `lua_config.rs` round-trip for `h.workspace "emptyn"`.

The TOML front-end gets the same grammar for free (`workspace emptyn`).
Roughly 40 lines plus tests.

---

## 5. Recommendation

Ranked:

1. **(b) `emptyn` on `guide+x`**, plus the `guide+stick_up`/`stick_down`
   pair and `guide+dpad_up` for (f). Zero-code today via `h.dispatch`; then
   land the `Selector` variant so the config reads `h.workspace "emptyn"`.
   This is the option whose semantics fit in one sentence and whose result is
   always visible in the bar.
2. **(e) `max+1`** if, after living with (b), the "walks into a hole in the
   middle" behaviour feels wrong. It is a strict superset in code (one query
   + one dispatch) and keeps numbers monotone.
3. **(a) `empty`** only as a second button ("go to the first free slot"),
   never as the primary — it mostly means "go to 1".
4. **(d) special workspace** for a different job ("park this"), on a
   different name than `scratchpad`.
5. **(c) insertion** — technically feasible on this fork, rejected because it
   renumbers the workspaces the owner reaches by `SUPER+number`.

Tiny plan:

1. Today: add the four `h.dispatch` lines from §4.2 to `~/.config/hyprpad/config.lua`,
   `hyprpad reload`, try `guide+x` from workspace 7 (expect pill `8`), press
   again (expect `9`, pill `8` gone), `guide+stick_down` (expect back to 7).
2. Then: the §4.3 change (`Selector` variant, fix the two `name:` traps),
   switch the lines to `h.workspace "emptyn"` etc.
3. Optional: the "already empty → no-op" and "alone → no-op" guards for (b)
   and (f) via `j/activeworkspace`.

---

## 6. Could not verify

- None of the dispatchers were fired (by instruction). The §1.4 resolutions
  are derived from the parser source against the live workspace list, and
  from the in-tree test that shows `empty` picking the lowest missing id.
- The two `name:` traps in §1.1 are read from code, not reproduced.
- The exact routine that destroys an empty, no-longer-visible workspace in
  this fork (upstream's `sanityCheckWorkspaces` does not exist under that
  name; `CWorkspace::~CWorkspace` emits `destroy` at `src/desktop/Workspace.cpp:87`).
  The behaviour itself is observed: workspace 1 is gone from
  `hyprctl -j workspaces`.
- Whether globals defined in `hyprland.lua` are callable from a socket
  `dispatch` expression (only matters for option (c)).
- How Quickshell's `Hyprland.focusedWorkspace` behaves while a special is
  open (only matters for option (d)); the widget filters negative ids either
  way.
- The cited fork files were read at HEAD `7b7e1939d`; the running compositor
  is `67200a838`. I did not diff the two.

---

## Sources

hyprpad (`/home/ajg/code/hyprsc`):
`src/hypr.rs:10-37, 93-122, 149-159, 519-526` · `src/config.rs:54-99, 130-143,
235-300` · `src/run.rs:1945-1949, 2164-2185` · `src/lua_config.rs:791-797,
1444-1447` · `src/bindings_sheet.rs:646-647, 687-690, 723-756` ·
`src/gesture.rs:171-191, 440-446` · `config/hyprpad.lua:170-200` ·
`README.md:88-99` · `docs/03-hardware-findings.md:154`.

HypXRland (`/home/ajg/code/Hyprland`, `hypxrland` @ `7b7e1939d`):
`src/config/lua/bindings/LuaBindingsDispatchers.cpp:519-526, 898-906,
1169-1237, 1245-1278, 1310-1378, 1428-1435, 1452` ·
`src/config/lua/bindings/LuaBindingsInternal.cpp:179-200, 389-402` ·
`src/helpers/MiscFunctions.cpp:126-466` ·
`src/config/shared/actions/ConfigActions.cpp:316, 935-1006, 1008-1063,
1065-1083, 1097-1121` · `src/desktop/history/WorkspaceHistoryTracker.cpp:36-50`
· `src/config/values/ConfigValues.cpp:516, 541-544` ·
`src/desktop/view/Window.cpp:2402-2405` ·
`hyprtester/src/tests/main/workspaces.cpp:480-535`.

Owner's config: `~/.config/hypr/bindings.lua:14-19, 59-64` ·
`~/.config/hypr/input.lua:31-38` · `~/.config/hypr/looknfeel.lua:18, 21-25` ·
`~/.config/hypr/monitors.lua:15-19` ·
`$OMARCHY_PATH/default/hypr/bindings/tiling.lua:21-40, 70-71` ·
`$OMARCHY_PATH/default/hypr/looknfeel.lua:111-125` ·
`$OMARCHY_PATH/default/hypr/qconsole.lua:15, 38-39` ·
`~/.config/omarchy/plugins/ajg.workspaces/Workspaces.qml:20-40, 59-68`.

Live (2026-09-01, read-only): `hyprctl -j workspaces`, `monitors`,
`activeworkspace`, `workspacerules`, `getoption binds:*`, `hyprctl version`.

Upstream wiki (`github.com/hyprwm/hyprland-wiki`, branch `main`):
`content/configuring/naming-conventions.md:111-167` — workspace selectors and
"Workspace search"; `content/configuring/core/dispatchers.md:62` (`focus({
workspace, on_current_monitor? })`), `:105` (`window.move({ workspace,
follow? })`), `:132-138` (`hl.dsp.workspace.*` incl. `change_id`), `:174-192`
(special workspaces). Raw:
`https://raw.githubusercontent.com/hyprwm/hyprland-wiki/main/content/configuring/naming-conventions.md`
and `…/content/configuring/core/dispatchers.md`.
