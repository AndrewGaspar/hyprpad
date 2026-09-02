# 13 — Modality: context-driven controller modes

## Motivation

Today the daemon has a single binary gate: `Arbiter::suppressed()` — true when a
game window (`steam_app_*` / `steam_proton*` / `gamescope*`) is focused, which
drops the ambient desktop handlers (cursor/scroll/bare-buttons) while leaving
guide chords alive. That's really a two-mode system with the modes and their
trigger hard-coded.

The owner wants to generalize this into **modes** (modalities): named profiles of
controller behavior, selected by **generic context conditions**, so the
controller does different things depending on what's in front of you. Concrete
near-term needs:

1. **Game / Steam Big Picture focused → passthrough**: disable *everything* that
   isn't a guide chord, so the raw controller belongs to the game (and guide
   chords stay as hyprpad's always-available overlay). This also fixes the
   double-input we saw in Big Picture (its class isn't in the game list).
2. **A terminal running Claude Code focused → a bespoke mode** (behavior TBD).
   Crucially, a Claude Code terminal has the *same window class* as any terminal,
   so selection must inspect the **process running inside the focused window**,
   not just its class/title. This is the requirement that forces a generic,
   pluggable condition system.

## Core concepts

- **Mode** — a named profile declaring which input *categories* are active, plus
  optional per-mode binding overrides. Modes layer over a base so each only
  states its deltas.
- **Context** — the current world the daemon can observe: focused window class /
  title / fullscreen, the process tree inside the focused window, a manual
  override, (later) running processes, time, battery, etc.
- **Rule** — `context predicate → mode`. Rules are ordered; first match wins.
- **ModeEngine** — subsumes `Arbiter`: tracks Context, re-resolves the active
  Mode on every context change, and exposes the resolved Mode's flags to the loop.

## Input categories a mode gates

These map 1:1 to the existing per-frame handlers, so wiring is mechanical:

| category | controls | today's handler |
|---|---|---|
| `cursor`  | right pad → desktop cursor | `drive_cursor` |
| `scroll`  | left pad → scroll          | `drive_scroll` |
| `buttons` | bare buttons → keys (`[buttons]`) | `drive_buttons` |
| `chords`  | guide chords → actions (`[bindings]`) | `handle_gesture` |
| `osk`     | guide+Y can raise the keyboard | `toggle_keyboard` |
| `forward` | raw controller → the virtual gamepad / game | `drive_gamepad` (uhid/gamepad) |

**Invariant (by convention, not hard-coded):** the default `game` mode keeps
`chords = true` — guide chords are hyprpad's escape hatch and should survive
every "hands-off" mode. A mode *can* set `chords = false` if someone really wants
a total-passthrough mode, but the shipped game mode never does.

## Config schema (proposed)

```toml
# ── Modes ──────────────────────────────────────────────────────────────────
# Every mode inherits the top-level [bindings]/[buttons]/[osk_buttons] and the
# built-in category defaults; a mode only states what differs. `default_mode`
# picks the fallback when no rule matches (else "desktop").
default_mode = "desktop"

[modes.desktop]        # full hyprpad — all categories on (the base)
cursor  = true
scroll  = true
buttons = true
chords  = true
osk     = true
forward = false

[modes.game]           # game / Big Picture: only guide chords survive
cursor  = false
scroll  = false
buttons = false
chords  = true         # escape hatch stays
osk     = true         # guide+Y still raises the keyboard over a game
forward = true         # raw input goes to the game (virtual pad / Steam)

[modes.claude]         # terminal running Claude Code (behavior TBD by owner)
# e.g. keep the cursor, repurpose bindings for reviewing diffs / approving:
# inherits desktop; example override:
# [modes.claude.bindings]
# "guide+r1" = "key pagedown"
# "guide+l1" = "key pageup"

# ── Rules: first match wins, evaluated on every context change ───────────────
# A rule matches when ALL of its stated conditions match (AND). A rule with no
# conditions is a catch-all. Conditions are lists → any element matches (OR).
[[mode_when]]
mode = "game"
focus_class = ["steam_app_*", "steam_proton*", "gamescope*", "steam", "steamwebhelper"]

[[mode_when]]
mode = "game"
focus_fullscreen = true            # optional: treat any fullscreen app as hands-off

[[mode_when]]
mode = "claude"
focus_process = ["claude"]         # focused window's process tree runs `claude`

# (implicit catch-all → default_mode; or state it explicitly)
[[mode_when]]
mode = "desktop"
```

### Conditions (all optional, extensible)

| condition | matches when… | source |
|---|---|---|
| `focus_class` | focused window class glob-matches any pattern | Hyprland `activewindow.class` |
| `focus_title` | focused window title matches any glob/regex | Hyprland `activewindow.title` |
| `focus_fullscreen` | focused window is fullscreen | Hyprland fullscreen event |
| `focus_process` | a process in the focused window's **process tree** matches (name or cmdline substring) | `activewindow.pid` → walk `/proc` descendants |
| *future* `process_running` | any process matches (regardless of focus) | `/proc` scan |
| *future* `manual` | a mode was forced via a binding | override slot |
| *future* `time_between`, `on_battery`, … | | |

**`focus_process` is the generic mechanism that catches Claude Code** (and any
"app-in-a-terminal" case). On a focus change we already learn the focused
window's `pid` from Hyprland; walk its descendants (`/proc/<pid>/task/*/children`
or a parent-map scan) and match `comm`/cmdline against the patterns. Cache the
result per focused-window pid so we only walk on focus changes (cheap — not
per-frame). A refinement re-walks on a slow timer so starting/quitting Claude in
an already-focused terminal switches mode without a refocus.

## Resolution precedence

1. **Manual override** — if a binding forced a mode (`Action::SetMode(name)` /
   `PushMode`/`ClearMode`), it wins. Lets the owner bind e.g. `guide+view` to
   force `game` or pop back to `desktop`, overriding focus rules.
2. **First matching `[[mode_when]]` rule** (top-to-bottom).
3. **`default_mode`** (`desktop`).

Re-resolve on: focus change, fullscreen change, manual override change, the
slow-timer process re-walk. On a resolved-mode *transition*, run the same
"clean handoff" we already do (release held clicks/keys, neutral the virtual
pad, reset dampers) so nothing strands across a mode switch.

## Runtime integration

- `ModeEngine` replaces `Arbiter`. It keeps the Context (the focus fields the
  arbiter already tracks, plus the focused pid + process-match cache) and the
  parsed modes/rules. `HyprEvent::ActiveWindow{class,title,pid}` and
  `Fullscreen` feed it (we'll need `pid` and `title` added to the event, both
  available from Hyprland).
- The loop gates each handler on `mode.cursor` / `.scroll` / `.buttons` /
  `.chords` / `.osk` / `.forward` instead of `arbiter.suppressed()`. The current
  `guide_active()` gating is orthogonal and stays (guide-held still steals the
  pads for the guide layer; whether a resolved chord *acts* is `mode.chords`).
- Per-mode binding overrides (`[modes.<name>.bindings]` etc.) layer over the base
  `Config` maps when that mode is active — the loop looks up bindings in the
  active mode's merged table. (Phase 2; the near-term game mode needs no overrides.)
- Hot-reload: modes/rules re-read on `hyprpad reload` like everything else; the
  active mode re-resolves against the new rules immediately.

## Phasing

- **Phase 1 (the near-term ask):** `[modes]` with category gates + `[[mode_when]]`
  with `focus_class` / `focus_fullscreen`, resolution, loop gating. Ships the
  game/Big-Picture passthrough and fixes the double-input. `Arbiter` → `ModeEngine`.
- **Phase 2:** `focus_process` (Claude Code detection) + `focus_title`, the
  process-tree walk + cache + slow re-walk.
- **Phase 3:** manual override actions, per-mode binding overrides, further
  conditions (process_running, time, battery).

## Owner decisions (2026-09-01) — these reshape the design

1. **Per-binding gating, NOT categories.** The owner wants granularity below the
   six-category level: each binding/input decides for itself whether it's active
   in a given context — not "game mode turns off the `cursor` category." This
   largely dissolves the category model: a **mode becomes a named context/tag**,
   and **every binding carries an optional guard** (which modes/conditions it's
   active under). The "game passthrough" is then "no binding except guide chords
   is tagged active in `game`," expressed per-binding rather than per-category.
   → This is exactly the structure that is painful in declarative TOML and
   natural as a **per-binding Lua predicate**, so the concrete schema is now
   COUPLED to the config-format decision (see `docs/research/lua-config.md`,
   in progress). Do not lock a per-binding TOML schema until that lands.
2. **Fullscreen ≠ game.** Drop `focus_fullscreen` as a game trigger; it's a wrong
   classification (fullscreen video, etc.). Keep it available only if some future
   mode explicitly wants it, but it is not part of the game rule.
3. **Claude detection: implementer's call** (owner has no preference; "finnicky
   by design"). Default to the `focus_process` tree-walk on `claude`, title match
   as a cheap optional add; keep expectations low on reliability.
4. **Manual override: yes.** A binding can force a mode (over the context rules),
   and pop back. Keep this a first-class part of the resolution precedence.

**Consequence:** the category table above is superseded by a per-binding-guard
model; it stays only as the conceptual bridge from today's binary arbiter. The
final schema is deferred to the config-format (Lua vs TOML) recommendation.

## Implemented shape (branch `feat-lua-config`)

The config-format question landed on `docs/research/lua-config.md`'s option D2,
so the schema above is superseded by the Lua one. What shipped:

- `src/mode.rs` — `ModeEngine`, which subsumes `Arbiter` as the daemon's gate.
  It *embeds* an `Arbiter` for the built-in path, so a config that declares no
  modes (every `config.toml`) keeps today's behaviour exactly rather than a
  re-implementation of it.
- `src/lua_config.rs` — the `hyprpad.lua` (Lua) front-end and the `hyprpad` API.
- `src/config.rs` — `ModeDef`, `Guard`, `ModeState`, `Action::SetMode` /
  `Action::ClearMode`, and the dual-front-end `Config::load`.
- `config/hyprpad.lua` — the owner's live `config.toml` translated 1:1, plus the
  game mode, the guards, and a commented Claude-Code mode.

Schema, in the shape the owner's decisions asked for:

```lua
h.mode("game", { forward = true }).when(function(ctx)
  return ctx.focus.class:lower():match("^steam_app_") ~= nil
end)
h.mode("claude").when(function(ctx) return ctx.focus:process_tree_has("claude") end)
h.mode("desktop")
h.default_mode "desktop"

h.cursor { only_in = { "desktop" } }              -- ambient handlers are guardable
h.button("a", h.key "enter"):only_in("desktop")   -- per binding, not per category
h.bind("guide+r1", h.workspace "+1")              -- unguarded: the escape hatch
h.bind("guide+view", h.set_mode "desktop")        -- manual override, first class
```

Deltas from the proposal above, all following from the owner's decisions:

| proposed | shipped |
|---|---|
| category flags per mode (`cursor = false` …) | **gone** — a mode is a name + rule + `forward`; every binding carries its own guard |
| `focus_fullscreen` as a game trigger | **dropped** — `ctx.focus.fullscreen` exists, nothing ships using it |
| `[[mode_when]]` glob lists | Lua predicates over `ctx` |
| per-mode binding *overrides* | not built — guards cover the near-term need; a mode that wants a different action for the same chord still needs this (see open items) |
| `focus_process` glob condition | `ctx.focus:process_tree_has(name)`, cached per focused pid |

Still open: the slow-timer `/proc` re-walk (so starting `claude` in an
already-focused terminal switches mode without a refocus), per-mode binding
overrides, and the further conditions (process_running, time, battery).

### The guide-held cursor (`feat-guide-mouse`)

The one place the guide layer *keeps* an ambient handler rather than taking it
away. `h.cursor` carries a second guard:

```lua
h.cursor { only_in = { "desktop" }, guide_in = { "game" } }
h.bind("guide+rpad_click", h.mouse "left"):only_in("game")
```

`only_in` is where the right pad drives the cursor with the guide **up**;
`guide_in` is where it goes on doing so with the guide **held** — Steam Input's
own "guide + pad = mouse", so a game can be pointed at without leaving it. The
frame arm's rule is `cursor_active(guide, ambient, under_guide)` in
`src/run.rs`: guide up → `only_in` decides; guide held → only `guide_in` can
keep the pad. Default is nowhere. Two consequences fall out of the precedence:

* The game gets nothing meanwhile — the guide is rank 1, forwarding is off for
  as long as it is held, so `desktop_yielded()` is untouched and the
  "forward = true but the cursor is live" warning never fires for it.
* A hold spent on pointing is **consumed** (`GestureEngine::consume_hold()` on
  the first cursor motion), so releasing the guide afterwards is not handed to
  Steam as a bare guide tap — the rule `docs/research/text-scrub.md` §5 asks
  of every guide-scoped pad handler.

Clicking is a *held chord output*: a `h.key` / `h.mouse` on a guide chord is
pressed on recognition and released on the chord button's lift
(`GestureEvent::GuideChordRelease`) or the guide's, whichever first — tracked
in `ChordKeys` beside `ButtonKeys` and released with it on the mode handoff and
disconnect. This also makes `h.bind("guide+l5", h.key "leftshift")` a modifier
on a grip, which the text-scrub design wants.

Known interaction: while Steam runs unmasked it acts on guide+pad itself
(Steam Input's chord layer), so two mice move together; the uhid/udev masking
work is what removes that, not this feature.

## Open questions for the owner (superseded — see decisions above)

1. **Category granularity** — is the six-category set (cursor/scroll/buttons/
   chords/osk/forward) the right resolution, or do you want per-binding gating
   (e.g. disable *some* chords in game mode but not others)? Categories are
   simpler; per-binding is Phase 2+ via mode binding-overrides.
2. **Fullscreen = game?** Should any fullscreen window (e.g. a fullscreen video)
   trigger passthrough, or only known game classes? (Proposed: off by default,
   available as a rule.)
3. **Claude Code detection** — process-tree match on `claude` (robust, proposed)
   vs. matching the terminal title glyph Claude sets (we saw `◑ hyprpad`; lighter
   but brittle). Recommend process-tree, with title as an optional extra condition.
4. **Manual override ergonomics** — do you want a dedicated chord to force
   desktop mode over a game (the reserved `force_desktop`), and/or a mode-cycle?
