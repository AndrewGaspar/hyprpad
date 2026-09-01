# Should hyprpad's config match / integrate with Hyprland's Lua config?

Research note. Scope: whether hyprpad (Rust daemon, `~/code/hyprsc`) should replace
its hand-rolled TOML config with something that matches — or literally integrates
into — HypXRland's Lua config scheme, especially in light of the modality system
proposed in `docs/13-modality-design.md`.

Method: read the live configs and the HypXRland source on this machine, read
hyprpad's own config + IPC code, and cross-check `mlua` and prior-art daemons on
the web. Findings are tagged **VERIFIED** (read from source/live files) or
**INFERRED** (reasoned from evidence).

**TL;DR recommendation:** Option **D/B** — keep the flat static tables declarative,
but move mode-selection to real predicates by embedding Lua (`mlua`, `lua54` +
`vendored`) in a hyprpad `config.lua` that *mirrors* Hyprland's feel. Do **not**
literally integrate with or share a file with the Hyprland config (Option C):
Hyprland never sees the controller, so there is nothing to integrate into. The
decision hinges on one thing — **how much logic will live in mode selection**. If
modality stays category-flag tables with a few globs (docs/13 Phase 1), TOML is
enough and Lua is over-engineering; if `focus_process` and arbitrary predicates
(Phase 2/3) are real, that is exactly what Lua is for and half-measures will chase
its tail.

---

## 0. Decisive context: the owner authored HypXRland

`~/code/Hyprland` is a Hyprland **fork owned by the same person as hyprpad**:

- `git remote -v` → `origin https://github.com/AndrewGaspar/Hyprland.git`,
  `upstream https://github.com/hyprwm/Hyprland.git` (VERIFIED)
- branch `hypxrland`, `VERSION` = `0.56.2` (VERIFIED)

So "HypXRland" is not a third-party project to conform to — it is the owner's own
compositor fork, and its Lua config manager (`src/config/lua/`) is the owner's own
code. That cuts both ways in the analysis below: real integration is *technically
within reach* (the owner can change the compositor), but the architecture makes it
the wrong move anyway (§3), and coupling to a personal fork undermines hyprpad's
portability goals (Steam Deck port W13, gamescope).

---

## 1. What HypXRland's Lua config actually is

### 1.1 One config manager, chosen by file extension (VERIFIED)

A session has exactly one config manager, picked by the extension of the main
config file: `.lua` → the Lua manager (`src/config/lua/ConfigManager.cpp`), anything
else → the legacy hyprlang manager (`src/config/legacy/`). There is **no bridge**:
`require` only resolves `.lua`/`init.lua`, `source =` only parses hyprlang, a `.lua`
cannot source a `.conf` and a `.conf` cannot reach a `.lua`.

- Source: live config comment block, `~/.config/hypr/hyprland-xr.lua:18-48`
- Source: `~/.config/hypr/AGENTS.md` "Config format: Lua and .conf side by side"
  and "hyprctl keyword is dead under Lua"

### 1.2 It is PUC Lua 5.4, full stdlib, NOT sandboxed (VERIFIED)

- `src/config/lua/ConfigManager.cpp:521` → `luaL_openlibs(m_lua)` opens **all**
  standard libraries: `_G, coroutine, debug, io, math, os, package, string, table,
  utf8` (the reload code enumerates exactly this stdlib set at
  `ConfigManager.cpp:661`).
- System Lua is 5.4 (`pkg-config` lists `lua-5.4`; no LuaJIT subproject; the manager
  uses `lua_sethook`/`LUA_MASKCOUNT`, the PUC C API).
- The live config uses `io.open`, `io.popen`, `os.getenv`, `string.format`, `require`,
  loops, and closures freely (`~/.config/hypr/hyprland-xr.lua:153-175` scans DRM EDIDs
  with `io.open`; `helpers.lua:23` shells out with `io.popen`).

**Consequence:** the Hyprland config is already **arbitrary code with full system
access**. The owner has already accepted "config is a program" for the compositor;
extending the same trust model to hyprpad's own config is not a new risk class.

### 1.3 The config is a script re-run top-to-bottom on every reload (VERIFIED)

`CConfigManager::reload()` (`ConfigManager.cpp:635-764`):

- **Phase 1** loads the file with `luaL_loadfile` to check *syntax* **before clearing
  any state** — verbatim comment: *"so a broken syntax doesn't entirely fucking nuke
  the config and leave the user with no binds"* (`:650-655`).
- **Phase 2** clears rule engines / keybinds / timers / window rules, wipes
  `package.loaded` for user modules so `require()` re-executes them, re-inits the Lua
  state, then runs the whole file under a watchdog.
- The live XR config documents this too: *"a Lua config is a SCRIPT that is re-run
  from the top on every reload, so a bare `hl.exec_cmd` at file scope would respawn on
  each one"* — the once-per-session idiom is `hl.on("hyprland.start", …)`
  (`hyprland-xr.lua:269-287`).

### 1.4 Callbacks and reloads are watchdog-guarded; errors don't crash (VERIFIED)

`guardedPCall` installs an instruction-count hook (`lua_sethook(..., LUA_MASKCOUNT,
…)`) with a deadline (`ConfigManager.cpp:459-478`). Per-context timeouts
(`ConfigManager.hpp:108-114`):

| context | timeout |
|---|---|
| config reload | 1500 ms |
| keybind callback | 100 ms |
| dispatch | 100 ms |
| event / timer / layout callback | 50 ms |
| `hyprctl eval` | 250 ms |

Errors are caught, reported via `addError`, and the compositor continues; on a
reload error `m_lastConfigVerificationWasSuccessful=false` and the pre-error state
stays. **This is a mature hot-reload safety design, and it is directly copyable by
hyprpad (§4.3).**

### 1.5 Binds take Lua function callbacks (VERIFIED)

Not just dispatcher strings — real closures:

- Source: `ConfigManager.cpp` registers dispatcher `"__lua"` that invokes a Lua
  function stored by registry ref, via `guardedPCall(… KEYBIND_CALLBACK_MS …)`
  (`LuaBindingsRegistration.cpp:64-95`).
- Live use: `~/.config/hypr/bindings.lua:127` binds a `function() … end`;
  `:141` `hl.define_submap("passthrough", function() … end)`.

### 1.6 The API surface actually in use

**`hl.*`** (compositor-provided global; `lua_setglobal(L,"hl")`,
`LuaBindingsRegistration.cpp:96`). Documented type stub:
`/usr/share/hypr/stubs/hl.meta.lua` (67 KB, LuaLS annotations; `.luarc.json` pins
globals `hl`,`o`). Observed / documented members:

- `hl.config({ nested = { tables } })` — config keywords as nested tables
  (`hyprland.lua`-family, `hyprpad.lua:5`, `hyprland-xr.lua:70`).
- `hl.bind(keys, dispatcher, opts)` / `hl.unbind(keys)` — opts documented as
  `HL.BindOptions` (`repeating, locked, release, long_press, description, device,
  click, drag, …`, stub `:437-452`). Live: `bindings.lua:13-27`,
  `hyprland-xr.lua:293-297`.
- `hl.dispatch(dispatcher)` — run a dispatcher imperatively (`bindings.lua:148`).
- `hl.dsp.*` — a **curated** dispatcher set (no by-name escape hatch;
  `AGENTS.md:78-82`): `window.{close,pin,swap,move,float,fullscreen,resize,drag,
  pseudo,cycle_next,bring_to_top}`, `focus`, `layout`, `workspace.{move,
  toggle_special}`, `group.{next,prev,toggle,active}`, `submap`, `exec_cmd`,
  and fork-only `xrmonitor` (`default/hypr/bindings/tiling.lua`, `hyprland-xr.lua`).
- `hl.on(event, fn)` — event subscriptions incl. `hyprland.start`
  (`hyprland-xr.lua:273`).
- `hl.define_submap(name, fn)` (`bindings.lua:141`).
- `hl.window_rule({ name=…, match={…}, …effects… })` — whole-rule form, returns the
  rule object (`hyprland-xr.lua:91-138`); `hl.monitor`, `hl.xr_rule`, `hl.xr_monitor`.
- `hl.exec_cmd`, `hl.env`, `hl.get_config("debug.overlay")` (`bindings.lua:128`).
- Objects with methods (stub + `src/config/lua/objects/`): Timer, EventSubscription,
  Window/Layer/Workspace rules, Keybind, Notification. There is even a **Lua custom
  layout provider** (`LuaLayoutProvider`, stub `HL.LayoutContext/LayoutProvider`).

**`o.*`** is **not** Hyprland — it is Omarchy's pure-Lua sugar over `hl.*`, defined in
`/usr/share/omarchy/default/hypr/helpers.lua`:

- `o.bind(keys, description, action, opts)` where `action` may be a **string** (shell
  cmd → `hl.dsp.exec_cmd`), a **dispatcher object** (`hl.dsp…`), a **table**
  (`{omarchy="browser"}`, `{launch=…}`, `{webapp=…}`, `{tui=…}`), or a **function**
  (`helpers.lua:92-106`, `command_from` `:56-82`).
- `o.window(match, rules)`, `o.launch_on_start`, `o.exec_on_start`, `o.bind_toggle`,
  `o.notify` (`helpers.lua:108-155`).

**`require`** is real and arbitrary: HypXRland wraps it (keeps original as
`__require`, installs a "safe require", hooks `package.searchers[2]` to track required
files so edits to any required module trigger a watcher reload —
`ConfigManager.cpp:549-581`). Omarchy adds `require_all.files(dir, prefix)` to load a
whole directory in sorted order (`require_all.lua`). `package.path` is set up by
`bootstrap.lua` to search `~/.local/state`, `~/.config`, and `$OMARCHY_PATH`.

**Plugin registration exists but is for C++ plugins, not external daemons:**
`m_registeredPlugins`, `reregisterLuaPluginFns()` (`ConfigManager.cpp:714,737`), and
an `HL.Plugin` stub class (`hl.meta.lua:486`). This lets a compiled Hyprland plugin
expose Lua functions; it is **not** a hook for a separate process to register
controller binds.

**Runtime injection:** `hyprctl keyword` is dead under Lua (exits 0, does nothing —
`AGENTS.md:53-66`); the live path is `hyprctl eval 'hl…'` (`luaL_loadstring` +
`guardedPCall(… EVAL_MS)`, `ConfigManager.cpp:898-907`).

---

## 2. How hyprpad talks to Hyprland today (VERIFIED)

From `hyprpad/src/hypr.rs` (module doc + methods):

- Two UNIX sockets under `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
  `.socket.sock` (one-shot request/reply) and `.socket2.sock` (event stream).
- hyprpad **sends `dispatch <lua-expr>`** — under the Lua manager the command socket
  routes `dispatch X` as `return hl.dispatch(X)`, so `X` must be a Lua expression
  (`hypr.rs:12-25`). e.g. `dispatch hl.dsp.window.fullscreen({ mode = "fullscreen" })`
  (`hypr.rs:126`).
- hyprpad **reads** JSON via `j/<command>` — `activewindow`, `clients`,
  `activeworkspace` (`hypr.rs:156-167`).

Note the current TOML already carries Lua fragments as opaque strings:
`~/.config/hyprpad/config.toml:47` → `"guide+b" = "dispatch hl.dsp.window.close()"`.
The config is *already* smuggling Lua; it just isn't first-class.

---

## 3. Is real INTEGRATION possible? (mostly no — strong pushback)

### 3.1 Hyprland never sees the controller — nothing to integrate into (VERIFIED)

hyprpad reads the Steam Controller off `hidraw` and only ever *speaks* to Hyprland
over the IPC sockets (§2). No controller input event exists inside the compositor, so
**Hyprland's config cannot bind controller inputs.** "Bind the controller in the
Hyprland config" is not a thing that can exist without first routing controller events
into the compositor. That would be a Hyprland input-source plugin or a virtual-gamepad
protocol — a much larger project, and *backwards*: hyprpad's whole reason to exist is
to be a standalone userspace daemon (docs 04/05/06), portable to gamescope and the
Steam Deck.

Because the owner authors HypXRland, a `hl.controller_bind(...)` API is *theoretically*
buildable. It should still be rejected: it folds a standalone daemon into one
compositor fork, couples hyprpad's core to that fork's release cadence and Lua dialect,
and discards the portability that motivates the project. (Pushback: this is the
seductive-but-wrong option precisely *because* the owner can do it.)

### 3.2 One shared `.lua` read by both — fragile, no upside (INFERRED)

- Hyprland's manager runs the **whole file**. Any hyprpad-only top-level statement that
  references a global Hyprland doesn't define is a runtime error there — and vice versa,
  hyprpad's interpreter would have to provide `hl`, `o`, and **every dispatcher object
  the file constructs at load time** (`hl.dsp.window.close()` is evaluated when the file
  runs), or the shared file throws on hyprpad's side. You would be stubbing the entire
  `hl`/`o` surface inside hyprpad just to not-error.
- Two interpreters, two reload lifecycles (Hyprland's file-watch vs. hyprpad's SIGHUP),
  one file. A change meant for one re-runs the other.
- Verdict: maximal coupling and fragility for zero functional gain. **Reject.**

### 3.3 hyprpad *reading* the Hyprland config to share definitions — limited (INFERRED)

The Hyprland config is **code, not data** — extracting "the browser launch command" or
"the theme" means executing the config in a compatible `hl`/`o` environment, which
hyprpad doesn't have. The values hyprpad actually needs to share are already exposed as
a stable contract elsewhere: `hl.dsp.*` dispatcher expressions (hyprpad already emits
them), `omarchy-*` command names, and `hyprctl -j` queries. Share through those, not by
parsing the compositor config.

---

## 4. Cost & shape of hyprpad embedding a Lua runtime

### 4.1 `mlua` fit (VERIFIED from mlua README, crate 0.12)

- Supports `lua54` (matches HypXRland) and `luau`; pick one via a Cargo feature.
- **`vendored`** builds a static Lua from source (via `lua-src`) — **no system Lua-dev
  package**, self-contained, reproducible. Adds a `cc` build step + the crate; compiles
  a small C library once. Modest, but it *does* break today's "input/gesture/IPC core
  stays dependency-free" profile (`Cargo.toml:9-11`; the crate hand-rolls even its TOML
  parser precisely to stay lean — `src/config.rs:24-28`).
- Safe by construction: Lua errors are Rust `Result`s; panics inside Rust callbacks are
  wrapped as Lua errors. **But** an infinite loop (`while true do end`) in a predicate is
  *not* caught by default — you must install an `mlua` hook, i.e. reimplement exactly the
  `lua_sethook` watchdog HypXRland already runs (§1.4).
- Sandboxing (`Lua::sandbox`) is **Luau-only**. With `lua54` you get full stdlib and no
  sandbox — same trust model as the Hyprland config (a user's own file), which is
  acceptable. If you wanted true untrusted-config isolation you'd choose `luau`, at the
  cost of diverging from Hyprland's 5.4 dialect (defeating "match the feel").
- MSRV 1.88.

Recommendation if embedding: **`features = ["lua54", "vendored"]`** — mirrors
HypXRland's dialect and stays self-contained.

### 4.2 What a hyprpad `config.lua` that mirrors Hyprland's feel looks like (sketch)

```lua
-- ~/.config/hyprpad/config.lua  — mirrors the hl/o idiom the owner already lives in.
-- A `pad` global is hyprpad's analogue of `hl`.

pad.config({
  daemon  = { own_lizard = true },
  cursor  = { sens = 0.06, one_euro_min_cutoff = 0.3, one_euro_beta = 1.0, hysteresis = 0.0008 },
  scroll  = { mode = "circular", sensitivity = 1.0, circular_step_degrees = 15.0 },
  haptics = { cursor_spacing_px = 96 },
})

-- Bare buttons (desktop layer). `key` / `dispatch` / `exec` mirror today's action verbs.
pad.buttons({
  dpad_up = pad.key("up"), a = pad.key("enter"), b = pad.key("backspace"),
})

-- Guide chords. `dispatch` takes the SAME hl.dsp expression hyprpad already sends,
-- but as a first-class value instead of the opaque string it is in the TOML today.
pad.bind("guide+r1",   pad.dispatch(hl.dsp.workspace.relative(1)))   -- (hl stub, hyprpad-side)
pad.bind("guide+x",    pad.exec("omarchy-menu"))
pad.bind("guide+b",    pad.dispatch("hl.dsp.window.close()"))         -- string form still ok
pad.bind("guide+y",    pad.keyboard("split"))
```

The `hl.dsp.*` used in a hyprpad config would be a **thin hyprpad-side stub** that
just serializes to the dispatch string hyprpad puts on the socket — hyprpad *mirrors*
the API, it does **not** depend on HypXRland's `hl`.

### 4.3 Hot-reload & failure modes (INFERRED, modeled on §1.3–1.4)

Copy HypXRland's pattern exactly:

1. On `hyprpad reload`, **load+syntax-check** the new `config.lua` before swapping.
2. Run it under an `mlua` instruction-count hook with a deadline (config-load budget).
3. On any error, **keep the last-good config** and surface the error (log/notify) —
   never leave the daemon input-dead.
4. Wrap every *runtime* predicate/callback call in `pcall` + the watchdog, and treat a
   throwing/timing-out predicate as "no match, keep current mode."
5. Keep a static built-in default (today's `DEFAULT_TOML` analogue) so a totally broken
   user file still yields a usable daemon.

---

## 5. Modality (docs/13) in Lua vs. TOML

Today the conditions are declarative data (`docs/13:88-106`):

```toml
[[mode_when]]
mode = "game"
focus_class = ["steam_app_*", "steam_proton*", "gamescope*", "steam"]

[[mode_when]]
mode = "claude"
focus_process = ["claude"]   # walk the focused window's /proc tree
```

The same as Lua predicates:

```lua
mode("game", { cursor=false, scroll=false, buttons=false, chords=true, osk=true, forward=true })
  .when(function(ctx)
    return ctx.focus.class:match("^steam_app_")
        or ctx.focus.class:match("^steam_proton")
        or ctx.focus.class == "steam"
        or ctx.focus.fullscreen
  end)

mode("claude", { inherits = "desktop" })
  .when(function(ctx) return ctx.focus:process_tree_has("claude") end)   -- helper hides the /proc walk
  .bind("guide+r1", pad.key("pagedown"))   -- per-mode override, first-class
```

**Expressiveness gain (real):** `focus_process` (the docs/13 requirement that forces a
generic condition system — a Claude-Code terminal shares a terminal's class) is a
one-liner predicate instead of a bespoke glob-list engine; combined conditions
(`class AND fullscreen AND NOT pinned`) are just boolean Lua; per-mode binding overrides
are ordinary table edits. Today's TOML bindings already embed Lua dispatcher strings, so
Lua removes an existing quote-and-parse layer rather than adding one.

**Cost (bounded):** predicates run on **context change** (focus / fullscreen / manual
override / slow `/proc` re-walk) — *not per input frame* (`docs/13:120-126`) — so the
runtime cost is negligible and the "config is Turing-complete" blast radius is small and
event-driven. The real obligations are the guardrails in §4.3 (watchdog + last-good +
pcall). A `while true` in a predicate must not be able to wedge the input loop — which is
exactly why the `mlua` hook is non-negotiable if you go this way.

---

## 6. Prior art

| tool | config format | lesson |
|---|---|---|
| **InputPlumber** (Rust, systemd, DBus) | **YAML** profiles + Capability Maps | The closest peer (Rust input-router daemon) chose *declarative data*, GUI/tooling-friendly, no logic in-config. |
| **AntiMicroX** | **XML** profiles + GUI editor, DBus control | Data + editor, not hand-written code. |
| **Steam Input** | binary **VDF**, GUI-edited, cloud-synced | Never hand-edited; the config is an artifact of a GUI. |
| **gamescope** | CLI flags / env | Minimal; logic lives in launch scripts around it. |
| **sway / i3** | declarative text; logic via external `*-msg` tools | Deliberately *not* a language — keeps the WM config safe/simple, pushes logic out. |
| **AwesomeWM** | **`rc.lua`** — the whole config is Lua | Maximal power; a `rc.lua` error historically drops to a fallback / can fail to start — hence Awesome's pre-load config check. |
| **Hammerspoon** | **`init.lua`** + rich API | The canonical "automation as Lua." Explicit reload; errors show in a console and the *running* config survives until a good reload. |
| **Neovim** | **`init.lua`** | Lua replaced vimscript for exactly the expressiveness reasons here; community norm is `pcall`-guarding user config and lazy-loading. |

**Reading of the field:** the *input-daemon* peers (InputPlumber, AntiMicroX, Steam
Input) are uniformly **declarative data** — none embed a language — because their
configs are mostly static remap tables and they lean on GUIs/DBus. The *desktop-scripting*
peers (Awesome, Hammerspoon, Neovim) go **full Lua** because their configs are inherently
logic. hyprpad is a hybrid: mostly-static tables **plus** a genuinely logic-shaped
modality layer. The Lua-as-config tools all teach the same three survival rules, which
HypXRland already implements and hyprpad must copy: **(a)** guard evaluation (pcall +
watchdog), **(b)** retain last-good on error, **(c)** ship a static fallback / verify
command.

---

## 7. Options, costs, migration

### A — Keep/improve TOML
- **Pros:** zero new deps (keeps the lean, hand-rolled ethos, `config.rs:24-28`); trivial
  to validate; matches the InputPlumber/AntiMicroX peer norm; safest failure mode.
- **Cons:** modality conditions must become a bespoke mini-DSL in strings; `focus_process`
  + boolean combinations + per-mode overrides hit a ceiling fast; the config keeps
  smuggling Lua dispatcher strings as opaque text.
- **Migration:** none. **Deps:** none. **Failure mode:** best (pure data).

### B — hyprpad embeds Lua; its own `config.lua` mirrors Hyprland's API (no integration)
- **Pros:** matches the `hl`/`o` idiom the owner already lives in daily; modality
  predicates + per-mode overrides become first-class; dispatcher expressions become real
  values; one mental model across compositor and controller; the owner can copy his own
  proven reload-safety machinery (§1.3–1.4).
- **Cons:** breaks zero-dep purity (one vendored C lib, `cc` build step, ~mlua+lua-src);
  config becomes Turing-complete → requires the §4.3 guardrails; a second config front-end
  to maintain during migration.
- **Migration:** medium. Add `mlua`; write the `pad.*` binding layer + `hl.dsp` serializer
  stub; port the TOML tables 1:1; keep TOML as fallback front-end during transition (the
  same dual-front-end pattern the owner runs for `hyprland-xr.{conf,lua}`).
- **Deps:** `mlua = { features = ["lua54","vendored"] }`. **Failure mode:** good *if* §4.3
  is implemented; dangerous if not.

### C — Shared-file integration with the Hyprland Lua config
- **Pros:** (aspirational) one file, shared defs.
- **Cons:** architecturally hollow — Hyprland can't see the controller (§3.1); a shared
  file forces hyprpad to stub the entire `hl`/`o` surface and survive two reload lifecycles
  (§3.2); couples hyprpad to a personal compositor fork and kills portability.
- **Migration:** high and ongoing. **Deps:** Lua + a large `hl`/`o` compatibility shim.
  **Failure mode:** worst (cross-process coupling). **Reject.**

### D — Hybrid: declarative static tables + Lua predicates only where modality needs them
- **D1 (no Lua):** TOML everywhere; a small predicate grammar in strings for `mode_when`.
  Cheapest, but reinvents a DSL and still ceilings out at `focus_process`/real logic.
- **D2 (= B, scoped):** declarative tables stay declarative; Lua is introduced *only* for
  `mode(...).when(fn)` and per-mode overrides. Same deps/cost as B, but the smallest
  Lua surface — the static config stays boring data and only the logic-shaped part becomes
  code.

---

## 8. Recommendation

**Adopt D2 (which is B, scoped): keep the flat tables declarative and introduce an
embedded Lua predicate layer for modality — a hyprpad `config.lua` that mirrors
Hyprland's `hl`/`o` feel but does not depend on it. Reject C outright. Treat plain B
(all-Lua) and A (all-TOML) as the endpoints D2 sits between.**

Reasoning:
1. The config **already** carries Lua dispatcher strings; the modality system is the
   tipping point that turns config into logic. That is precisely the boundary where
   declarative data stops paying and a language starts.
2. The expensive, scary parts of "config is code" — hot-reload safety, watchdogs,
   last-good retention, error isolation — **already exist, written by the owner, in
   HypXRland** (§1.3–1.4). hyprpad can copy a proven design instead of inventing one.
3. Mirroring (not integrating) gives the living-room user one idiom across compositor and
   controller while keeping hyprpad a portable standalone daemon (Steam Deck / gamescope
   goals intact).
4. Scoping Lua to predicates (D2) keeps the static config boring, testable data and
   confines the Turing-complete blast radius to event-driven, watchdog-guarded callbacks.

**Honest pushback / where this could be the wrong call:**
- If modality realistically stays **Phase 1** (category flags + a handful of
  `focus_class`/`focus_fullscreen` globs), then **A is the right answer** and any Lua is
  over-engineering — a `mlua`+vendored build cost and a Turing-complete config for what is
  a 20-line match table. Do not embed Lua speculatively.
- Embedding Lua **without** the §4.3 guardrails is worse than TOML: a throwing or looping
  predicate on the input path is a far nastier failure than a parse error at startup.
- "Match Hyprland's Lua" must mean *mirror the ergonomics*, never *depend on HypXRland's
  `hl` or share its file*. The naive reading (C) is architecturally empty because the
  compositor has no controller input to bind.

---

## 9. Single most decisive consideration + next step

**Decisive:** *How much logic will live in mode selection?* Mode selection is the entire
reason this question exists. Static match tables → TOML (A). Arbitrary predicates over
focus/process/state with per-mode overrides (docs/13 Phase 2–3) → Lua (D2/B). Everything
else (dependency weight, "matching Hyprland", reload safety) is secondary to that one
call.

**Measured cost (2026-09-01, branch `feat-lua-config`, VERIFIED).** The note above
asked for the build-time and binary-size delta from vendored `lua54` before committing.
Release binaries built back to back on the Framework 16 with a warm cargo cache:

| | `hyprpad` (release, `lto = true`) |
|---|---|
| before (`master`) | 1,141,824 B |
| after (`+ mlua 0.10.5, lua54 + vendored`) | 1,781,504 B |
| **delta** | **+639,680 B (+56%)** |

The vendored `liblua5.4.a` is 681 KB before LTO. Cold-build cost is the one-off
compile of PUC Lua's C sources plus `mlua`/`mlua-sys` (~15 s on this machine, inside
the existing wayland build); incremental rebuilds of the crate itself are unchanged.
`vendored` means no system Lua and no `-dev` package, so the daemon stays a single
self-contained binary — which was the condition for accepting the dependency at all.

**Next step:** Prototype D2 behind a build/runtime flag — add `mlua` (`lua54`,`vendored`),
load a `config.lua` that reads the *existing* tables into today's `Config` struct, port the
docs/13 modes as `mode(name).when(fn)` with the `/proc`-walk hidden behind a
`ctx.focus:process_tree_has(...)` helper, and wire the §4.3 reload guardrails
(syntax-check-before-swap + instruction-count watchdog + last-good retention). Keep TOML as
the fallback front-end during migration — the same dual-front-end approach the owner already
runs for `hyprland-xr.{conf,lua}`. Measure the build-time and binary-size delta from vendored
`lua54` before committing; if modality lands and stays Phase-1-shaped, abandon the branch and
keep TOML.

---

### Sources
Local (VERIFIED): `~/code/Hyprland/src/config/lua/ConfigManager.cpp`,
`.../LuaBindingsRegistration.cpp`, `.../ConfigManager.hpp`; `~/code/Hyprland/VERSION`,
git remotes/branch; `/usr/share/hypr/stubs/hl.meta.lua`;
`/usr/share/omarchy/default/hypr/{helpers,bootstrap,require_all,bindings,omarchy}.lua`,
`.../bindings/tiling.lua`; `~/.config/hypr/{hyprland,hyprland-xr,bindings,hyprpad}.lua`,
`~/.config/hypr/AGENTS.md`, `~/.config/hypr/.luarc.json`;
`~/code/hyprsc/src/{config.rs,hypr.rs}`, `Cargo.toml`, `docs/13-modality-design.md`,
`~/.config/hyprpad/config.toml`.
Web: [mlua (GitHub)](https://github.com/mlua-rs/mlua) ·
[mlua feature flags (lib.rs)](https://lib.rs/crates/mlua/features) ·
[InputPlumber usage](https://shadowblip.github.io/InputPlumber/usage/) ·
[InputPlumber (GitHub)](https://github.com/ShadowBlip/InputPlumber) ·
[AntiMicroX (GitHub)](https://github.com/AntiMicroX/antimicrox) ·
[Steam Input Configurator (ArchWiki)](https://wiki.archlinux.org/title/Steam_Input_Configurator) ·
[awesome-lua](https://github.com/uhub/awesome-lua) ·
[Hammerspoon LSP discussion](https://github.com/Hammerspoon/hammerspoon/discussions/3451)
</content>
</invoke>
