# 13 — Modality: modes as contexts, guards on bindings

Read this after [`README.md`](../README.md). The README says *how to write* modes
and guards; this says **what the model is, why it has this shape, and how it
behaves at runtime** — what a contributor needs before changing `src/mode.rs`,
`src/config.rs` or the gates in `src/run.rs`.

> A **mode** is a named context, chosen by an ordered list of Lua predicates
> that run **only when the context changes**. A **binding** carries its own
> guard saying where it is live. There are no category switches.

Everything below follows from that. [§8](#8-owner-decisions-2026-09-01) is the
owner-decision list the code cites by number.

---

## 1. Why not a category switch

The daemon began with one binary gate: `Arbiter::suppressed()` in
[`src/arbitrate.rs`](../src/arbitrate.rs) — true while a game window
(`steam_app_*` / `steam_proton*` / `gamescope*`) holds focus, dropping the ambient
desktop handlers and leaving guide chords alive. A two-mode system with the modes
and their trigger hard-coded. The obvious generalisation is a mode owning category
flags — `cursor = false, scroll = false, buttons = false`. The owner rejected it
(decision #1): the interesting granularity is *below* the category. "Disable some
chords in game mode but not others" has no spelling in a category model, and "a
terminal running Claude Code" is not a window property at all — it is a question
about the process tree inside the focused window, which no glob over
`class`/`title` can answer.

So the model inverts. A mode carries **no** behaviour flags (bar one, `forward`);
it is a name with a rule, and every binding decides for itself where it is live.
"Game passthrough" is not a switch — it is the emergent result of *nothing but
the guide chords being live in `game`*.

`Arbiter` survives as the **built-in path**: a config that declares no modes
(every `config.toml`) gets the original behaviour, and `ModeEngine` implements
that by *embedding an `Arbiter`* rather than restating its rules
(`src/mode.rs:132-135`) — so "no modes declared" is literally the old behaviour,
not a re-implementation of it. Adopting Lua is opt-in twice over: once for the
file, once for the modes.

---

## 2. What a mode is

```lua
h.mode("game", { forward = true }).when(function(ctx)
  return ctx.focus.class:lower():match("^steam_app_") ~= nil
end)
h.mode("cheatsheet").when(function(ctx) return ctx.layers:has("hyprpad-cheatsheet") end)
h.mode("locked").when(function(ctx) return ctx.locked end)
h.mode("desktop")          -- no rule: the fallback
h.default_mode "desktop"
```

A mode is a name, an optional rule, and the `forward` flag. Rules are tried in
**definition order, first match wins**; modes are exclusive, so exactly one is
live. Declaration order is therefore the priority order, which is why the
sample config puts `locked` first, then the layer-keyed modes (an overlay is
drawn *over* a game), then the class-keyed ones, then `desktop` last
(`config/hyprpad.lua:56-120`).

### What a rule can see

`ctx` is built fresh per re-resolve from three sources (`src/mode.rs:60-105`),
each existing because the ones before it are blind to something:

| source | reaches Lua as | why it exists |
|---|---|---|
| the focused window | `ctx.focus.class`, `.title`, `.pid`, `.fullscreen`, and `ctx.focus:process_tree_has(name)` | the ordinary case |
| layer-shell overlays | `ctx.layers` — an array of namespaces with `:has(name)` / `:list()` | a layer surface takes the keyboard with **no `activewindow` event behind it**, so a focus-only engine cannot see hyprpad's own cheat sheet, or Omarchy's menu |
| the session lock | `ctx.locked` (boolean) | Omarchy's lock is an `ext-session-lock-v1` surface: neither a window nor a layer. To an engine watching only the first two, a locked session and an idle desktop are the same picture — except every bare button is now typing into a password field |

`process_tree_has` is the requirement that forced a predicate language in the
first place: a Claude Code terminal has the *same window class* as any other
terminal, so selection has to look **inside** the window. It walks
`/proc/<pid>/task/*/children` — the kernel's own child list — bounded at
`PROC_WALK_LIMIT` (512) processes, matches a lowercased `"<comm> <cmdline>"` per
descendant, and caches the walk per focused pid so it costs one walk however many
predicates ask (`src/lua_config.rs:360-450`).

Above all of it sits the manual override: `h.set_mode` / `h.clear_mode` beat the
rules until cleared ([§8](#8-owner-decisions-2026-09-01) #4).

### A transient mode has no rule

`h.mode("hints"):transient { … }` is a mode you can only *enter* — the one way in
is `h.set_mode`, from a chord or a bare button — because it then gives itself
back on its own (§3). It **may not carry a `:when`**: the loader refuses the two
together by name (`src/lua_config.rs:614-627`) and the rule scan skips a
transient mode even if one somehow reached it (`src/mode.rs:749-759`). Both
guard the same thing — falling *into* such a mode would arm a press budget and a
timer nobody asked for, and then clear the mode out from under the very window
that selected it — and the load error exists so a `:when` cannot sit in the file
looking live. Every field of the contract is optional, and "off" is the empty
value rather than omission (`max_presses = 0`, `timeout_ms = 0`, `exit_on = {}`),
so `:transient {}` alone is the browser-hints shape.

---

## 3. What counts as a context change

Predicates run **only** here — never per input frame. The resolved mode, every
guard result, and the filtered button maps are cached in `ModeEngine`, and the
per-frame handlers only read them (`src/mode.rs:25-32`).

| change | fed by | entry point |
|---|---|---|
| focus moved | `activewindow` (class, title) + a one-shot `j/activewindow` for the fields the event stream omits (pid, fullscreen) | `focus_changed` / `context_changed` |
| the focused window renamed itself | `windowtitle` | `title_changed` |
| fullscreen toggled | Hyprland fullscreen event | `set_fullscreen` |
| an overlay opened or closed | `openlayer` / `closelayer`, plus a startup seed from `j/layers` | `layer_changed` / `seed_layers` |
| the session locked or unlocked | a 1 s poll of `j/locked` on the socket the daemon already holds | `lock_changed` |
| the process tree moved | the periodic `process_rescan_ms` sweep (default 500 ms) | `rescan_processes` |
| a manual override | `h.set_mode` / `h.clear_mode` | `set_mode` / `clear_mode` |
| a config reload | `hyprpad reload` | `reconfigure` |
| a transient mode gave itself back | its own contract: the press cap, a named exit button, a focus change, a rename, a click, or the timer | `settle_presses` / `note_press` / `note_click` / `check_deadline` |

Four properties of that table are load-bearing:

* **One call, one re-resolve.** `context_changed` replaces the whole focus at
  once, so a predicate never sees the new window's pid against the old window's
  class, and a transient mismatch is never reported as a mode transition
  (`src/mode.rs:196-207`).
* **A no-op change is not a change.** A rename to the title already held, a
  repeat `openlayer` for a namespace already open, a lock report matching the
  state already held — none re-resolve. That is what makes the compositor's two
  events per rename cost one re-resolve, and what makes the lock poll free: it
  fires every interval and costs one comparison until the answer moves.
* **Never pay for what you did not ask for.** Each expensive source is opt-in,
  decided once at load by scanning the config *source* for the bare word:
  `process_rescan_useful` refuses the sweep without declared modes, a mention of
  `process_tree_has`, and a focused pid; `watches_lock()` refuses the lock poll
  without a mention of `locked`; `needs_focus_pid()` gates the extra
  `j/activewindow` query. A TOML config never tracks layers or the lock at all.
  The scan errs wide deliberately — over-matching (a mention in a comment) costs
  a socket round trip, under-matching would cost a mode that silently never
  resolves. One consequence: adding the *first* `ctx.locked` predicate is the one
  change a reload cannot pick up, since the watcher is wired at startup.
* **A rename is a process hint.** Starting `claude` in an already-focused
  terminal changes only what the window is *called*, so `rescan_on_title_change`
  (default `true`) reads the rename as "the tree probably moved" and drops the
  `/proc` cache.

### Transient modes: the fourth kind of transition

The rows above are three kinds: the world changed (the first six), the user
overrode it (`set_mode`), or the file did (`reconfigure`). A transient mode is
the fourth — a manual override that carries its own **removal contract**, so
what ends it is neither a compositor event nor a `clear_mode` but the mode's own
bookkeeping. Six ways out, and every one of them lands in the same `refresh` and
the same "did the mode move?" answer, so the daemon runs the same handoff it
runs for a focus change. A new way of *leaving* a mode is never a new way of
*making* a transition.

Two of them need care about **when**, which is why the press cap and the exit
button are separate calls:

* **The consumed press** — the one place the engine eats an input rather than
  routing it. A press of a named exit button *is* the exit, so `note_press`
  answers `consumed: true` and the bare-button layer must not also deliver it:
  `b` cancels the hints instead of typing a hint letter.
* **The press cap** is the opposite. The press that spends the last of the
  budget is the keystroke that picked the link, so it is delivered first and
  `settle_presses` clears the mode afterwards — clearing on the spot would run
  the handoff, close the bare-button layer, and swallow it.

---

## 4. Guards: per binding, not per category

Every `h.bind` / `h.button` / `h.osk_button`, and the `h.cursor` / `h.scroll`
"virtual bindings", carry a `Guard` (`src/config.rs:1204-1254`):

| variant | spelling | live when |
|---|---|---|
| `Always` | *(none)* | everywhere — the default, and everything the TOML front-end produces. This is why guide chords survive a fullscreen game: hyprpad's escape hatch |
| `OnlyIn` | `:only_in("desktop", "browser")` | the active mode is one of these |
| `NotIn` | `:not_in("game")` | the active mode is **not** one of these |
| `When` | `:when(function(ctx) … end)` | the Lua predicate returned truthy at the last re-resolve |

A guard is **exactly one** of those — never a conjunction: chaining a second
replaces the first, so `:only_in("desktop"):when(f)` is just `:when(f)`. All three
methods chain onto every binding kind, in either Lua punctuation (`:only_in(..)`
and `.only_in(..)` both work, since the owner's Hyprland config uses dot calls
throughout). The pads take theirs inline as table keys instead. A *mode* handle
is the one thing that takes no guard — only `when` and `forward` — and says so.

Guards never run on the input path. `Guard::allows(&ModeState)` is a single match
arm over cached data — `OnlyIn`/`NotIn` compare mode names, `When(i)` is an
**index into a cached bool vector**, not a Lua call — and that vector is computed
in one pass per context change (`ModeEngine::refresh_declared`), so a predicate
shared by a mode rule and a binding guard runs exactly once. Rules and guards
share one flat index space (`Config::predicate_slots`).

A `When` whose predicate is missing from the snapshot — it errored or timed out —
reads as **false**: the same "a broken predicate is not a match" rule the mode
rules use, so a bad guard silences one binding rather than taking the daemon with
it. A guard naming a mode nobody declared is a different matter: a **load error**
([§6](#6-guardrails)), because the binding would otherwise just never fire.

### Bare buttons and `ButtonAlt`

A bare button may be bound once per mode — the mechanism by which one physical
control means two things:

```lua
h.button("b", h.key "backspace"):only_in("desktop")
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
```

The first binding stays in the base map (so the TOML path is untouched) and later
ones become `ButtonAlt`s; `Config::buttons_in(mode)` walks base-first, then alts
in declaration order, and the **first passing guard wins**, flattening them into
the single `button → action` map the frame handler reads. Since modes are
exclusive there is normally no competition; two that *are* live at once means
both were left unguarded, which the loader warns about by name. A `ButtonAction`
is either `Hold` (a key/mouse pressed and released with the button) or `Fire`
(run once on the press edge — `h.exec`, `h.keyboard`, `h.set_mode`,
`h.clear_mode`).

`h.seq { h.key "f", h.set_mode "hints" }` is the one action that is a *list* of
actions, run in order on a single press — the two-step binding the hints need:
type into the page, *then* move the daemon into the mode whose buttons are that
page's hint letters. Each step takes the same `perform_action` path it would
alone, with one deliberate difference: an `h.key` **inside** a sequence is a
**tap**, pressed and released on the spot. A held key needs a release edge to
pair with and a sequence has none — by the time the button lifts the sequence is
long over, and the daemon may not be in the mode that resolved it any more. A
`set_mode` step applies where it stands, so later steps run in the new mode.
Sequences never nest: the loader refuses one inside another rather than
flattening it, so the cheat sheet's label for a `seq` is always one flat list.

### The ambient handlers

The two trackpads are guarded like anything else, with one extra:

```lua
h.cursor { only_in = { "desktop", "omarchy-ui", "browser" }, guide_in = { "game" } }
h.scroll { mode = "circular", only_in = { "desktop", "omarchy-ui", "browser" } }
```

`only_in` is where the right pad drives the cursor with the guide **up**;
`guide_in` is where it goes on doing so with the guide **held** — Steam Input's
own "guide + pad = mouse", so a game can be pointed at without leaving it. The
whole rule is `cursor_active(guide, ambient, under_guide)` in `src/run.rs`:
`if guide { under_guide } else { ambient }` — note the two guards are *not*
ANDed, so `guide_in` alone suffices under a held guide and `only_in` is ignored
there. Default is nowhere. Scroll has only the ambient guard. Either handler
gated off does not merely skip: it drops its filter state, so re-entry never
jumps. And a hold spent on pointing is **consumed**, so releasing the guide
afterwards is neither handed to Steam as a bare guide tap nor resolved as the
`guide_tap` binding — the two consumers of a bare tap, and the narrow decision
in `GestureEngine::guide_tap` rules out both at once.

---

## 5. Precedence and the gates

Four layers claim the same device. `run::gamepad_forwarding` (`src/run.rs:1619`)
is the single place the order is written down:

> **guide > OSK > game-forwarding > desktop**

| rank | layer | gate | what reaches it |
|---|---|---|---|
| 1 | **guide** | `guide_active()` | every per-frame handler drops out while the guide is held, in a game as much as on the desktop. Chords and flicks still resolve; a bare-button press is **swallowed, not deferred**. The one opt-in exception is `h.cursor { guide_in = … }` |
| 2 | **OSK** | `osk.is_active()` | it owns both pads; cursor and scroll are forced into their drop-and-forget branches, and every desktop gesture is suppressed **except the keyboard toggle**, so the chord that raised it can dismiss it |
| 3 | **game forwarding** | `gamepad_forwarding(enabled, forwards, desktop_yielded, !guide, !osk)` | the raw report goes to the virtual pad |
| 4 | **desktop** | each handler's own guard in the active mode | cursor, scroll, bare-button fires and holds |

Ranks 3 and 4 are **mutually exclusive by construction**, not by policy:
forwarding also requires `ModeEngine::desktop_yielded()`, true exactly when
neither cursor nor scroll is live. A manual override forcing the desktop live
over a game, or a mode that sets `forward = true` while leaving the cursor
guarded in, yields rather than driving both at once — and `refresh()` warns at
load when a config asks for that, since the gate would otherwise silently never
engage. `cursor_guide_enabled` is deliberately *excluded* from `desktop_yielded`:
it is only live under the guide, where forwarding is already off. On every
transition **out** of rank 3 the live sink gets one `neutral()` report and the
rumble stops — a game is left with a connected, idle controller rather than a
vanished one, and never holding input hyprpad has stopped feeding it.

### The stale-press latch

> Every layer acts on **press edges only**, and never on the frame it became
> live. — `src/run.rs:86`

Without this, a gate that opens and a press that lands in the same report are the
same event: the `Y` of the `guide+Y` that raised the keyboard would also be its
first keystroke. Two independent one-bit flags carry it — `ButtonKeys::open` ("the
bare-button layer was live at the end of the last `reconcile`, with no transition
since") and `OskRoute::live` — each gating presses on
`settled = active && was_live_last_frame`. **Releases are unconditional**: a key
comes up when its button lifts whatever else is going on, and Shift comes up when
the last trigger holding it lifts.

`ButtonKeys::release_all` resets the flag, which is what stops a button that is
*still down* from re-pressing on the next frame. The motivating bug: a Tab that
walks Omarchy's bar panels flips the mode away and back within milliseconds, each
flip running the handoff — and without the mark, one tap became two or three. The
price is that a key held straight through a guide tap does not resume on its own;
it wants a fresh press.

### The mode handoff

One function, `run::mode_handoff` — "so a new way of *noticing* a transition can
never come with a subtly different way of *making* one". It releases, in order:
the cursor damper and scroll state, every held bare-button key or mouse button
(plus the latch), every held guide-chord output, and the virtual pad.

What it deliberately does **not** touch is the load-bearing half: the gesture
engine and `prev_frame` are left alone, because a transition is usually *caused*
by a chord that is still physically held — `guide+view` opens the cheat sheet,
whose layer flips the mode. Resetting edge state would make that chord look
freshly pressed next frame, fire again, toggle the overlay closed, flip back, and
loop for as long as it was held. (Observed live.) Only the disconnect path
rebuilds the gesture engine, and only because the device genuinely went away.

A **reload** is the exception: it runs no handoff. Held keys are settled by the
next frame's `reconcile` against the *new* map, so a binding that survived stays
held (a reload mid-drag keeps the drag) and one that changed or vanished is
released. Every other transition names its cause in the log — `(title change)`,
`(overlay)`, `(session locked)`, `(process rescan)`, `(manual)` — because a
transition with no focus change behind it is otherwise a mystery.

### The `osk` context

Rank 2 is a context, but it is **not a declared mode**: the daemon does not
switch modes for the keyboard and no rule can select it. It behaves like one from
the reader's side, so `hyprpad bindings` reports it as a built-in mode named
`osk` (`bindings_sheet::OSK_MODE`, `builtin: true`) purely so the sheet can show
a tab for it without knowing what a keyboard is.

`h.osk_button("y", h.key "space")` types a key through the keyboard;
`h.osk_button("l2", h.osk "shift")` binds one of its own actions
(`commit` | `shift` | `dismiss`); `h.none()` drops a built-in entry. These layer
*over* the built-in Deck map (`config::osk_builtins`) rather than replacing it,
and an entry whose guard fails lets the built-in show through — so a helper
guarded into one mode never leaves its button dead in the others.

---

## 6. Guardrails

A config that is a *program* can hang or throw where a config that is *data*
could only be malformed. The design mirrors the one the owner already proved in
HypXRland's `src/config/lua/ConfigManager.cpp` (`src/lua_config.rs:98-115`):

| guardrail | how |
|---|---|
| **syntax-check before swap** | the whole file is compiled with `into_function()` before a line executes, so a syntax error is reported with its line number and nothing was applied |
| **fresh state per load** | every load builds a new `Lua` and a new `Config`; nothing is mutated in place, so a failure at any later step leaves the running config untouched *by construction* |
| **watchdog** | an instruction-count hook (`lua_sethook`'s mlua equivalent, every `HOOK_INSTRUCTIONS = 1000` VM instructions) against an `Instant` deadline, with HypXRland's own per-context budgets: `LOAD_TIMEOUT` 1500 ms for the whole file, `PREDICATE_TIMEOUT` 100 ms for one rule or guard, `EVENT_TIMEOUT` 50 ms reserved for the callbacks the API will grow (`h.on`) |
| **validation** | after execution, before anything is handed back: a guard naming a mode no `h.mode` declares is **refused** (a typo in `:only_in("desktopp")` would otherwise silently disable a binding), as is a mode declared twice. A `default_mode` nobody declared is *not* an error — it is declared implicitly, so `h.default_mode "desktop"` alone works. A button re-bound with no guard is a warning naming the button: only the first is live |
| **last-good retention** | any failure leaves the running config in place and logs the reason. `hyprpad reload` can never leave the daemon input-dead |

A predicate that is cut, or that throws, counts as **no match** and is logged
once — never a panic, never a propagated error. The deadline is saved and
restored around each guarded call, so a nested call cannot extend its budget and
a finished predicate does not leave the watchdog armed for the next one.

---

## 7. The cheat sheet's view

`hyprpad bindings [--json]` loads the config through the daemon's own
`Config::load`, so the sheet cannot drift from it, and touches neither the
controller nor the running daemon.

One card is one *context*: **a tab per declared mode, in the order the rules are
tried**, plus the built-in `osk` tab. A tab lists what is live there and nothing
else — so `only_in("desktop")` renders as a *tab* rather than a tag on a row,
which is what makes the guard model legible rather than a thicket of annotations.
Where `h.cursor { guide_in = … }` lists a mode, that tab gets an extra `guide`
row for the right pad carrying *that* guard. The sheet opens on the mode you were
in when you summoned it — raising it is itself a mode change, since it is a layer
surface `ctx.layers` can see — so the daemon exports `HYPRPAD_MODE` to the
command a binding execs.

---

## 8. Owner decisions (2026-09-01)

The decisions that reshaped this design away from the category model, cited **by
number** from `src/mode.rs:9-18`, `src/config.rs:368`, `src/mode.rs:117` and
`config/hyprpad.lua:276` — so the numbering is load-bearing and is preserved.
Each is marked against what the code does today.

**1. Per-binding gating, NOT categories.** ✅ *Holds.*
`ModeDef` carries a name, an optional rule and `forward` — no category flags
(`src/config.rs:1155-1170`) — and every binding carries a `Guard`. The decision
noted the schema was coupled to the then-open config-format question; that landed
on Lua (`docs/research/lua-config.md` D2), which made per-binding predicates
spellable at all. *Residual:* the symmetry is incomplete — bare buttons can be
bound once per mode (`ButtonAlt`), **chords cannot** ([§9](#9-still-open)).

**2. Fullscreen ≠ game.** ✅ *Holds.*
`ctx.focus.fullscreen` is available to a rule that asks for it and nothing ships
using it; the test `fullscreen_is_available_to_a_rule_but_triggers_nothing_by_itself`
asserts `set_fullscreen` alone never moves the mode, with an opt-in `cinema` mode
as the counter-example. The built-in path classifies on class prefixes only.
*Minor staleness elsewhere:* `Arbiter::suppressed`'s doc still reads "a fullscreen
game always suppresses", as if fullscreen contributed; the code reads only
`force_desktop` and `game_focused`.

**3. Claude detection: implementer's call, "finnicky by design".** ⚠️ *Drifted —
it got less finnicky than the decision assumed.*
The mechanism is as decided: `process_tree_has`, a cached `/proc` walk, title
matching left optional. But "keep expectations low" rested on starting `claude`
in an already-focused terminal needing a refocus to be noticed, and that is no
longer true — `rescan_on_title_change` and the 500 ms `process_rescan_ms` sweep
both close it, on by default, gated behind `process_rescan_useful`.
The sample now ships the mode as `agent` — one rule covering Claude Code, Codex,
Muse and OpenCode — and the "needs a refocus" caveat is gone from its comment.

**4. Manual override is first class.** ✅ *Holds, and grew.*
`h.set_mode` / `h.clear_mode` beat the rules (`src/mode.rs:22`), and `reconfigure`
drops an override naming a mode the new config does not declare rather than
stranding the daemon in one that no longer exists. Beyond the decision: it is no
longer chord-only — a **bare button** can fire it (`ButtonAction::Fire`). The
override is sticky until an explicit `clear_mode`.

---

## 9. Still open

* **Per-mode chord overrides.** `bindings` is one action per chord, so a mode
  that wants `guide+b` to mean something else cannot say so; guards can only
  take a binding away. Bare buttons *do* have this (`ButtonAlt`), which is the
  asymmetry to close. See `docs/research/text-scrub.md`.
* **Further conditions** — `process_running` (regardless of focus), time of
  day, battery. The `ctx` table is the extension point.
* **A non-sticky override** — `h.toggle_mode`, or a `set_mode` that clears on
  the next focus change, rather than only on an explicit `clear_mode`.
