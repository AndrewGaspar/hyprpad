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

## Open questions for the owner

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
