//! The Lua config front-end: `~/.config/hyprpad/config.lua`.
//!
//! This is the second of hyprpad's two config front-ends (see [`crate::config`]
//! for the first, the flat TOML dialect). Both produce the same [`Config`], so
//! nothing downstream of the loader knows which one ran.
//!
//! **Why it exists.** `docs/research/lua-config.md` recommends option *D2 —
//! mirror, don't integrate*: the static tables stay boring declarative data,
//! but **mode selection is logic**, and a bespoke glob mini-DSL in TOML strings
//! would chase its tail the moment a rule needs "the process tree inside the
//! focused window runs `claude`". So hyprpad embeds Lua for the logic-shaped
//! part and keeps everything else a table. It **mirrors** the owner's
//! HypXRland `hl`/`o` ergonomics — it does not depend on, share a file with, or
//! talk to the compositor's Lua state (that option is rejected outright in §3
//! of the research note: Hyprland never sees the controller).
//!
//! ## The `hyprpad` API
//!
//! The one global is `hyprpad` (idiomatically bound to `h`). Everything is a
//! function on it; nothing is a magic global table you assign into.
//!
//! ```lua
//! local h = hyprpad
//!
//! -- Settings: one call per section, taking the same keys the TOML has.
//! h.daemon  { own_lizard = true, steam_button_poweroff = "off", process_rescan_ms = 500 }
//! h.cursor  { sens = 0.06, one_euro_min_cutoff = 0.3, hysteresis = 0.0008 }
//! h.scroll  { mode = "circular", sensitivity = 1.0 }
//! h.scrub   { detent_deg = 15, select = "l5" }   -- guide + circle the left pad = caret
//! h.haptics { cursor_spacing_px = 96 }
//! h.gamepad { enabled = true, kind = "xbox", identity = "triton" }
//!
//! -- Bindings. The optional middle string is a description, exactly as
//! -- `o.bind(keys, desc, action)` reads in the Hyprland config.
//! h.bind("guide+r1", "Workspace right", h.workspace "+1")
//! h.bind("guide+x",  h.exec "omarchy-menu")
//! h.bind("guide+b",  h.dispatch "hl.dsp.window.close()")
//! h.bind("guide+y",  h.keyboard { mode = "split" })
//! h.button("dpad_up", h.key "up")       -- bare button, no guide modifier; held with it
//! h.button("l1",      h.key "shift+tab") -- a combo: modifiers held around the key
//! h.button("r2",      h.mouse "left")   -- a mouse button, through the pointer
//! h.button("l5",      h.exec "voxtype record toggle") -- any other action fires once, on press
//! h.osk_button("y",   h.key "space")    -- only while the OSK is up: a key typed through it…
//! h.osk_button("l2",  h.osk "shift")    -- …or one of its own actions (commit|shift|dismiss),
//!                                       -- layered over the built-in Deck map; h.none() drops one
//!
//! -- Modes: a mode is a NAMED CONTEXT selected by a predicate. Rules run in
//! -- definition order, first match wins, and only on a context change.
//! h.mode("desktop")
//! h.mode("game", { forward = true }).when(function(ctx)
//!   return ctx.focus.class:match("^steam_app_") ~= nil
//! end)
//! h.default_mode "desktop"
//!
//! -- Per-binding guards: every binding decides for itself where it is live.
//! h.cursor { only_in = { "desktop" }, guide_in = { "game" } } -- guide + pad = mouse in a game
//! h.button("a", h.key "enter"):only_in("desktop")
//! h.bind("guide+l1", h.workspace "-1"):not_in("game")
//! h.bind("guide+i", h.exec "…"):when(function(ctx) return ctx.focus.pid ~= nil end)
//! ```
//!
//! `h.key` takes a **combo** as readily as a key: modifier names joined to the
//! key with `+` — `h.key "shift+tab"`, `h.key "ctrl+left"`, `h.key "ctrl+shift+tab"`,
//! `h.key "super+1"` — where a modifier is `shift|ctrl|control|alt|super|meta|win`
//! or an explicit `leftshift`/`rightctrl`/… form, and the key is any name the
//! table knows (the letters, the digits, the arrows and editing keys, `f1`–`f12`,
//! the US punctuation). The modifiers are pressed before the key and released
//! after it wherever it goes down — a bare button, a guide chord, or an
//! `h.osk_button` typed through the on-screen keyboard — so a held `ctrl+left`
//! auto-repeats as one, and an unknown token, a repeated modifier or a combo
//! with no key after its `+` is a reported error.
//!
//! `ctx` carries `ctx.focus.class`, `.title`, `.pid`, `.fullscreen`, and the
//! method `ctx.focus:process_tree_has("claude")`, which walks the focused
//! window's `/proc` descendants (the Claude-Code-in-a-terminal detection of
//! docs/13). It also carries `ctx.layers` — the layer-shell overlays on
//! screen, which is how a rule sees hyprpad's *own* windowless UI:
//!
//! ```lua
//! h.mode("cheatsheet").when(function(ctx)
//!   return ctx.layers:has("hyprpad-cheatsheet")
//! end)
//! ```
//!
//! `ctx.layers` is an array of namespaces (so `ipairs` and `#` work) with
//! `:has(name)` and `:list()` on it.
//!
//! The third source is `ctx.locked` — a plain boolean, true while the session
//! is locked. The lock screen is an `ext-session-lock-v1` surface, so it is
//! neither a window nor a layer and the other two are blind to it:
//!
//! ```lua
//! h.mode("locked").when(function(ctx) return ctx.locked end)
//! h.button("a", h.key "enter"):when(function(ctx) return not ctx.locked end)
//! ```
//!
//! See [`crate::mode`] for how a mode is resolved.
//!
//! ## Guardrails (the non-negotiable part)
//!
//! A config that is a *program* can hang or throw where a config that is *data*
//! could only be malformed. The owner already solved this once, in HypXRland's
//! `src/config/lua/ConfigManager.cpp`; this module mirrors that design:
//!
//! | HypXRland | here |
//! |---|---|
//! | phase 1 `luaL_loadfile` **before** clearing any state, "so a broken syntax doesn't nuke the config and leave the user with no binds" | [`load_str`] compiles with `lua.load(..).into_function()` before anything is executed or swapped |
//! | `reinitLuaState()` on every reload | every load builds a **fresh** [`Lua`] and a fresh [`Config`]; nothing is mutated in place |
//! | `guardedPCall` + `lua_sethook(LUA_MASKCOUNT)` + a deadline | [`LuaRuntime::guarded`] arms an instruction-count hook with an [`Instant`] deadline |
//! | `LUA_TIMEOUT_CONFIG_RELOAD_MS = 1500`, `…KEYBIND_CALLBACK_MS = 100`, `…EVENT_CALLBACK_MS = 50` | [`LOAD_TIMEOUT`], [`PREDICATE_TIMEOUT`], [`EVENT_TIMEOUT`] — the same numbers |
//! | errors collected and surfaced, never fatal | every error is a `Result`; the daemon's `resolve_reload` keeps the **last-good** `Config` |
//!
//! One deliberate difference: HypXRland opens the whole stdlib including
//! `debug`; [`Lua::new`] here opens mlua's "all safe" set, which is the same
//! minus `debug`/`ffi`. Nothing a config wants is missing (`io`, `os` and
//! `string` are all there, as the owner's own configs use them).

use crate::config::{
    Action, ButtonAction, ButtonAlt, Config, CursorConfig, GamepadConfig, Guard, HapticsConfig,
    ModeDef, OskAction, ScrollConfig, ScrollMode,
};
use crate::config::{mouse_code, parse_button, GestureKey, KeyChord};
use crate::mode::Context;
use mlua::{Function, HookTriggers, Lua, MultiValue, Table, Value, VmState};
use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Budget for executing the whole `config.lua` on load or reload. Mirrors
/// HypXRland's `LUA_TIMEOUT_CONFIG_RELOAD_MS`.
pub const LOAD_TIMEOUT: Duration = Duration::from_millis(1500);

/// Budget for one mode rule or `:when` guard predicate. Mirrors HypXRland's
/// `LUA_TIMEOUT_KEYBIND_CALLBACK_MS` — the same "a user callback on the input
/// path" role.
pub const PREDICATE_TIMEOUT: Duration = Duration::from_millis(100);

/// Budget for an incidental event-shaped callback. Mirrors HypXRland's
/// `LUA_TIMEOUT_EVENT_CALLBACK_MS`. Reserved for the callbacks the API will
/// grow (`h.on(...)`); nothing uses it yet, so it is the documented ceiling for
/// anything added later rather than a live path.
pub const EVENT_TIMEOUT: Duration = Duration::from_millis(50);

/// VM instructions between watchdog-hook firings. HypXRland uses the same
/// order of magnitude (`LUA_WATCHDOG_INSTRUCTION_INTERVAL`): frequent enough to
/// cut a `while true do end` in well under a frame, rare enough that the hook
/// costs nothing measurable on a predicate that does real work.
const HOOK_INSTRUCTIONS: u32 = 1000;

// ---------------------------------------------------------------------------
// The live Lua state
// ---------------------------------------------------------------------------

/// The Lua interpreter behind a loaded `config.lua`, kept alive for as long as
/// the [`Config`] it produced.
///
/// It holds the mode rules and `:when` guard predicates — the only Lua that
/// runs *after* load. Everything else the config said is already plain Rust
/// data by then.
///
/// Single-threaded on purpose: only the daemon's main loop ever calls in, and
/// not making it `Send` keeps the "config is code" blast radius on one thread.
pub struct LuaRuntime {
    lua: Lua,
    /// Every predicate the config registered, indexed by the ids stored in
    /// [`ModeDef::rule`] and [`Guard::When`].
    predicates: Vec<Function>,
    /// The watchdog's current deadline, or `None` when no guarded call is in
    /// flight. Shared with the hook closure installed on `lua`.
    deadline: Rc<Cell<Option<Instant>>>,
    /// Focused-window `/proc` walk cache, so `process_tree_has` costs one walk
    /// per focused pid however many predicates ask.
    procs: Rc<RefCell<ProcCache>>,
    /// Whether the loaded file mentions `process_tree_has` at all. Decided once,
    /// at load, by scanning the source: a config that never walks the process
    /// tree must never be polled for changes in it
    /// ([`crate::mode::ModeEngine::process_rescan_useful`]).
    walks_process_tree: bool,
    /// Whether the loaded file mentions `locked` at all. Decided the same way,
    /// at load, by scanning the source: a config that never asks about the
    /// session lock must never make the daemon poll the compositor for it
    /// ([`crate::hypr::watch_locked`]).
    reads_locked: bool,
    /// Predicate ids that have already logged a failure, so a permanently
    /// broken predicate complains once instead of on every focus change.
    complained: RefCell<Vec<usize>>,
}

impl std::fmt::Debug for LuaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaRuntime")
            .field("predicates", &self.predicates.len())
            .finish_non_exhaustive()
    }
}

impl LuaRuntime {
    /// How many predicates the config registered.
    pub fn predicate_count(&self) -> usize {
        self.predicates.len()
    }

    /// Whether any predicate in this config walks the focused window's process
    /// tree. Decided at load from the source text, which is conservative in the
    /// safe direction: a file that mentions `process_tree_has` may still never
    /// call it (one wasted `/proc` walk per rescan interval), but a file that
    /// does not mention it cannot possibly call it, and is never polled.
    pub fn walks_process_tree(&self) -> bool {
        self.walks_process_tree
    }

    /// Whether any predicate in this config reads `ctx.locked`. Decided at load
    /// from the source text, by the same rule and with the same bias as
    /// [`walks_process_tree`](Self::walks_process_tree): a file that says
    /// `locked` anywhere may still never read it (one wasted 0.2 ms socket
    /// round trip a second), but a file that never says the word cannot read
    /// it, and is never polled for it.
    ///
    /// The needle is the bare word rather than `ctx.locked`, so the indirect
    /// spellings — a predicate that took `ctx` under another name, or reached
    /// it as `ctx["locked"]` — are caught too. Over-matching here costs a
    /// socket round trip; under-matching would cost a mode that silently never
    /// resolves.
    pub fn reads_locked(&self) -> bool {
        self.reads_locked
    }

    /// Drop the focused window's cached `/proc` walk so the next predicate that
    /// asks re-walks it. The rescan paths' one lever: everything else about
    /// re-resolving is unchanged, which is what keeps a rescan-driven mode
    /// transition identical to a focus-driven one.
    pub fn forget_process_tree(&self) {
        self.procs.borrow_mut().forget();
    }

    /// Evaluate predicate `id` against the current focus context.
    ///
    /// **Never panics and never propagates.** A predicate that throws, or that
    /// the watchdog cuts off, counts as *no match* and logs once — the docs/13
    /// rule, and the reason a runaway `while true do end` in a rule cannot wedge
    /// the input loop.
    pub fn eval_predicate(&self, id: usize, cx: &Context) -> bool {
        let Some(f) = self.predicates.get(id) else { return false };
        let ctx = match self.build_ctx(cx) {
            Ok(t) => t,
            Err(e) => {
                self.complain(id, &e.to_string());
                return false;
            }
        };
        match self.guarded(PREDICATE_TIMEOUT, || f.call::<Value>(ctx)) {
            Ok(v) => truthy(&v),
            Err(e) => {
                self.complain(id, &e.to_string());
                false
            }
        }
    }

    /// Run `f` with the watchdog armed for `budget`, restoring whatever
    /// deadline was in force before (so a nested call cannot extend its
    /// caller's budget). The direct analogue of HypXRland's `guardedPCall`.
    fn guarded<T>(
        &self,
        budget: Duration,
        f: impl FnOnce() -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        let prev = self.deadline.replace(Some(Instant::now() + budget));
        let out = f();
        self.deadline.set(prev);
        out
    }

    /// Build the `ctx` table handed to a predicate: `ctx.focus.{class,title,
    /// pid,fullscreen}` plus the `process_tree_has` method, and `ctx.layers`.
    fn build_ctx(&self, cx: &Context) -> mlua::Result<Table> {
        let f = self.lua.create_table()?;
        f.set("class", cx.focus.class.as_str())?;
        f.set("title", cx.focus.title.as_str())?;
        match cx.focus.pid {
            Some(pid) => f.set("pid", pid)?,
            None => f.set("pid", Value::Nil)?,
        }
        f.set("fullscreen", cx.focus.fullscreen)?;
        f.set("process_tree_has", self.proc_fn(cx.focus.pid)?)?;
        let ctx = self.lua.create_table()?;
        ctx.set("focus", f)?;
        ctx.set("layers", self.layers_table(cx)?)?;
        // A plain boolean, and deliberately so: there is exactly one thing to
        // ask about a lock, and `ctx.locked` reads the way a guard wants to
        // spell it — `:when(function(ctx) return not ctx.locked end)`.
        ctx.set("locked", cx.locked)?;
        Ok(ctx)
    }

    /// The `ctx.layers` table: the layer-shell namespaces currently on screen.
    ///
    /// It is an *array* of names, so `ipairs(ctx.layers)` and `#ctx.layers`
    /// work on it directly, with two methods hung off it:
    ///
    /// ```lua
    /// ctx.layers:has("hyprpad-cheatsheet")  -- the one a rule actually wants
    /// ctx.layers:list()                     -- a fresh copy, sorted
    /// ```
    ///
    /// Cheap enough to build per re-resolve: a session has a handful of
    /// overlays (three here — the bar, the background and the sheet), and this
    /// runs on a context change, never per frame.
    fn layers_table(&self, cx: &Context) -> mlua::Result<Table> {
        let t = self.lua.create_sequence_from(cx.layers.iter().map(String::as_str))?;
        let names = cx.layers.clone();
        let listed = names.clone();
        // Both call forms, as with `process_tree_has`: `layers:has(..)` passes
        // the table as the first argument, `layers.has(..)` does not, so the
        // last string argument is the one being asked about and neither
        // spelling is a silent mismatch.
        t.set(
            "has",
            self.lua.create_function(move |_, args: MultiValue| {
                let needle = args
                    .iter()
                    .rev()
                    .find_map(|v| v.as_str().map(|s| s.to_string()))
                    .ok_or_else(|| {
                        mlua::Error::RuntimeError(
                            "layers:has needs a namespace to look for, e.g. \
                             ctx.layers:has(\"hyprpad-cheatsheet\")"
                                .to_string(),
                        )
                    })?;
                Ok(names.contains(needle.as_str()))
            })?,
        )?;
        t.set(
            "list",
            self.lua.create_function(move |lua, _: MultiValue| {
                lua.create_sequence_from(listed.iter().map(String::as_str))
            })?,
        )?;
        Ok(t)
    }

    /// The `ctx.focus:process_tree_has(pattern)` closure, bound to this
    /// context's pid.
    ///
    /// Accepts both call forms — `focus:process_tree_has("claude")` (method,
    /// with the focus table as the first argument) and
    /// `focus.process_tree_has("claude")` — by taking the last string argument
    /// as the pattern, so neither spelling is a silent mismatch.
    fn proc_fn(&self, pid: Option<i32>) -> mlua::Result<Function> {
        let procs = Rc::clone(&self.procs);
        self.lua.create_function(move |_, args: MultiValue| {
            let needle = args
                .iter()
                .rev()
                .find_map(|v| v.as_str().map(|s| s.to_string()))
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(
                        "process_tree_has needs a name to look for, e.g. \
                         ctx.focus:process_tree_has(\"claude\")"
                            .to_string(),
                    )
                })?;
            let Some(pid) = pid else { return Ok(false) };
            Ok(procs.borrow_mut().tree_has(pid, &needle))
        })
    }

    /// Log a predicate failure at most once per predicate.
    fn complain(&self, id: usize, msg: &str) {
        let mut seen = self.complained.borrow_mut();
        if seen.contains(&id) {
            return;
        }
        seen.push(id);
        eprintln!(
            "warning: config.lua predicate #{id} failed ({}); treating it as no match \
             (logged once)",
            msg.trim()
        );
    }
}

/// Lua truthiness: everything except `nil` and `false`.
fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Boolean(false))
}

// ---------------------------------------------------------------------------
// The focused window's process tree
// ---------------------------------------------------------------------------

/// One focused pid's `/proc` descendants, flattened to lowercase
/// `"<comm> <cmdline>"` strings and cached.
///
/// The walk happens at most once per focused pid however many predicates ask —
/// docs/13's "cheap, not per-frame" requirement — and is re-walked only when
/// something says the tree may have moved under a *still-focused* window:
/// Hyprland renaming it (`rescan_on_title_change`, the event-driven path, since
/// starting `claude` renames the terminal) or the periodic
/// `process_rescan_ms` sweep for the programs that rename nothing. Both spell
/// that as [`LuaRuntime::forget_process_tree`]; neither changes what a walk is.
#[derive(Debug, Default)]
struct ProcCache {
    pid: Option<i32>,
    entries: Vec<String>,
}

impl ProcCache {
    fn tree_has(&mut self, pid: i32, needle: &str) -> bool {
        if self.pid != Some(pid) {
            self.entries = walk_process_tree(pid);
            self.pid = Some(pid);
        }
        let needle = needle.to_ascii_lowercase();
        self.entries.iter().any(|e| e.contains(&needle))
    }

    /// Forget the cached walk, so the next `tree_has` re-reads `/proc`.
    fn forget(&mut self) {
        self.pid = None;
        self.entries.clear();
    }
}

/// Collect `"<comm> <cmdline>"` (lowercased) for `root` and every descendant.
///
/// Descendants come from `/proc/<pid>/task/*/children`, the kernel's own child
/// list — cheaper and racier-proof than scanning every `/proc/*/stat` for a
/// matching PPid. Bounded so a pathological tree (or a pid-reuse cycle) cannot
/// spin: at most [`PROC_WALK_LIMIT`] processes are visited.
fn walk_process_tree(root: i32) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = vec![root];
    let mut seen = vec![root];
    while let Some(pid) = queue.pop() {
        if out.len() >= PROC_WALK_LIMIT {
            break;
        }
        if let Some(desc) = process_description(pid) {
            out.push(desc);
        }
        for child in process_children(pid) {
            if !seen.contains(&child) {
                seen.push(child);
                queue.push(child);
            }
        }
    }
    out
}

/// Ceiling on how many processes one focus change's walk will visit.
const PROC_WALK_LIMIT: usize = 512;

/// `"<comm> <cmdline>"` for one pid, lowercased, or `None` if it is gone.
fn process_description(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    // cmdline is NUL-separated; a kernel thread's is empty.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    Some(format!("{} {}", comm.trim(), cmdline.trim()).to_ascii_lowercase())
}

/// The direct children of `pid`, from every thread's `children` file.
fn process_children(pid: i32) -> Vec<i32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for task in tasks.flatten() {
        let Ok(text) = std::fs::read_to_string(task.path().join("children")) else { continue };
        out.extend(text.split_whitespace().filter_map(|t| t.parse::<i32>().ok()));
    }
    out
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load `path` as a `config.lua`.
///
/// Errors carry the file name and line, and are returned rather than logged so
/// the reload path can keep the last-good config.
pub fn load_file(path: &Path) -> Result<Config, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    load_str(&src, &path.display().to_string())
}

/// Load `src` as a `config.lua` named `name` (the name that appears in error
/// messages). The whole guardrail sequence lives here:
///
/// 1. a **fresh** [`Lua`] with the watchdog hook installed, and a fresh
///    [`Build`] — nothing existing is touched, so a failure at any later step
///    leaves the caller's current config untouched by construction;
/// 2. **compile the whole file** (`into_function`) before executing a line of
///    it, so a syntax error is reported with its line number and nothing ran;
/// 3. execute under the [`LOAD_TIMEOUT`] watchdog;
/// 4. validate what the file declared (unknown mode names in guards, a
///    `default_mode` nobody declared);
/// 5. only then hand back a `Config`.
pub fn load_str(src: &str, name: &str) -> Result<Config, String> {
    let lua = Lua::new();
    let deadline = Rc::new(Cell::new(None::<Instant>));
    install_watchdog(&lua, &deadline);

    let build = Rc::new(RefCell::new(Build::default()));
    install_api(&lua, &build).map_err(|e| format!("{name}: installing the hyprpad API: {e}"))?;

    // Phase 1 — syntax. Compile the entire file before running any of it. This
    // is HypXRland's "check syntax before clearing any state" rule; here the
    // state is the caller's `Config`, which we have not touched at all yet.
    // `@` makes Lua treat the name as a file path, so errors read
    // `/path/config.lua:12: …` rather than `[string "…"]:12: …`.
    let chunk = lua
        .load(src)
        .set_name(format!("@{name}"))
        .into_function()
        .map_err(|e| format_err(name, &e))?;

    // Phase 2 — execute into the fresh Build, under the watchdog.
    let armed = deadline.replace(Some(Instant::now() + LOAD_TIMEOUT));
    let ran = chunk.call::<()>(());
    deadline.set(armed);
    ran.map_err(|e| format_err(name, &e))?;

    // Phase 3 — harvest. `take` empties the builder the Lua closures still hold
    // a handle to, which both hands us the data and breaks the only reference
    // cycle that could keep the interpreter alive after the Config is dropped.
    let b = std::mem::take(&mut *build.borrow_mut());
    // Whether any predicate can walk the process tree is a property of the
    // *source*, not of anything the file did while it ran: a rule that only ever
    // calls `process_tree_has` on a class it has not seen yet would otherwise
    // look unused at load time and never be polled.
    finish(
        b,
        lua,
        deadline,
        src.contains("process_tree_has"),
        src.contains("locked"),
    )
    .map_err(|e| format!("{name}: {e}"))
}

/// Install the instruction-count watchdog hook.
///
/// The hook is set **once**, for the life of the state, and does nothing while
/// `deadline` is `None`; arming and disarming is just a `Cell` write. Erroring
/// from the hook aborts the running Lua exactly the way HypXRland's
/// `watchdogHook` does.
fn install_watchdog(lua: &Lua, deadline: &Rc<Cell<Option<Instant>>>) {
    let deadline = Rc::clone(deadline);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_INSTRUCTIONS),
        move |_, _| match deadline.get() {
            Some(d) if Instant::now() > d => Err(mlua::Error::RuntimeError(
                "hyprpad: Lua execution timed out (runaway loop in config.lua?)".to_string(),
            )),
            _ => Ok(VmState::Continue),
        },
    );
}

/// Turn a finished [`Build`] into a validated [`Config`].
fn finish(
    mut b: Build,
    lua: Lua,
    deadline: Rc<Cell<Option<Instant>>>,
    walks_process_tree: bool,
    reads_locked: bool,
) -> Result<Config, String> {
    // A `default_mode` nobody declared is a convenience, not a typo: declare it
    // implicitly (with no rule) so `h.default_mode "desktop"` alone works.
    let default = b.default_mode.clone().unwrap_or_else(|| "desktop".to_string());
    if !b.modes.iter().any(|m| m.name == default) {
        b.modes.push(ModeDef { name: default.clone(), rule: None, forward: false });
    }
    // A guard naming a mode that was never declared is a typo, and a silent one
    // — the binding would simply never fire. Refuse to load instead.
    let declared: Vec<&str> = b.modes.iter().map(|m| m.name.as_str()).collect();
    for (what, guard) in b.all_guards() {
        for want in guard.mode_names() {
            if !declared.contains(&want.as_str()) {
                return Err(format!(
                    "{what} is guarded on mode '{want}', which no h.mode(…) declares \
                     (declared: {})",
                    if declared.is_empty() { "none".to_string() } else { declared.join(", ") }
                ));
            }
        }
    }

    // A button bound twice with nothing to tell the two bindings apart is a
    // config bug rather than a second binding: modes are what make an alternate
    // reachable, so an unguarded one can never win. Name it instead of letting
    // it read as bound.
    for s in &b.slots {
        if let Slot::ButtonAlt(i, name) = s {
            if b.button_alts.get(*i).is_some_and(|a| matches!(a.guard, Guard::Always)) {
                eprintln!(
                    "warning: button '{name}' is bound more than once and the later binding \
                     carries no guard, so only the first is live; guard them into different \
                     modes (h.button(\"{name}\", …):only_in(\"cheatsheet\"))"
                );
            }
        }
    }

    let runtime = LuaRuntime {
        lua,
        predicates: std::mem::take(&mut b.predicates),
        deadline,
        procs: Rc::new(RefCell::new(ProcCache::default())),
        walks_process_tree,
        reads_locked,
        complained: RefCell::new(Vec::new()),
    };

    Ok(Config {
        bindings: b.bindings,
        buttons: b.buttons,
        osk_buttons: b.osk_buttons,
        own_lizard: b.own_lizard,
        steam_button_poweroff: b.steam_button_poweroff,
        sleep_inactivity_timeout: b.sleep_inactivity_timeout,
        rescan_on_title_change: b.rescan_on_title_change,
        process_rescan_ms: b.process_rescan_ms,
        cursor: b.cursor,
        scroll: b.scroll,
        scrub: b.scrub,
        haptics: b.haptics,
        gamepad: b.gamepad,
        modes: b.modes,
        default_mode: Some(default),
        binding_guards: b.binding_guards,
        button_guards: b.button_guards,
        button_alts: b.button_alts,
        osk_button_guards: b.osk_button_guards,
        binding_descs: b.binding_descs,
        button_descs: b.button_descs,
        osk_button_descs: b.osk_button_descs,
        cursor_guard: b.cursor_guard,
        cursor_guide_guard: b.cursor_guide_guard,
        scroll_guard: b.scroll_guard,
        scrub_guard: b.scrub_guard,
        lua: Some(Rc::new(runtime)),
    })
}

/// Render a Lua error as one line that always names the config line.
///
/// A *Lua* error already carries `config.lua:12:` in its message. An error
/// raised from one of hyprpad's own API functions does not — mlua reports the
/// position only in the traceback below it — so the position is lifted out of
/// that traceback and prepended. Either way the user gets
/// `config.lua:12: unknown h.cursor key 'sesn' …` rather than a bare complaint
/// with no idea which line caused it.
fn format_err(name: &str, e: &mlua::Error) -> String {
    let full = e.to_string();
    let first = full.lines().next().unwrap_or(&full).trim().to_string();
    if first.contains(&format!("{name}:")) {
        return first;
    }
    match traceback_position(&full, name) {
        Some(pos) => format!("{pos}: {first}"),
        None => first,
    }
}

/// Pull the innermost `"<name>:<line>"` out of a Lua traceback.
fn traceback_position(full: &str, name: &str) -> Option<String> {
    let needle = format!("{name}:");
    full.lines().find_map(|line| {
        let at = line.find(&needle)?;
        let digits: String = line[at + needle.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!digits.is_empty()).then(|| format!("{name}:{digits}"))
    })
}

// ---------------------------------------------------------------------------
// The builder the Lua API writes into
// ---------------------------------------------------------------------------

/// What a `config.lua` has declared so far. One of these per load; never
/// reused, never mutated in place across a reload.
#[derive(Default)]
struct Build {
    bindings: HashMap<GestureKey, Action>,
    binding_guards: HashMap<GestureKey, Guard>,
    buttons: HashMap<crate::report::Button, ButtonAction>,
    button_guards: HashMap<crate::report::Button, Guard>,
    /// Bindings for a button `buttons` already binds, in declaration order.
    button_alts: Vec<ButtonAlt>,
    osk_buttons: HashMap<crate::report::Button, OskAction>,
    osk_button_guards: HashMap<crate::report::Button, Guard>,
    /// The optional description each binding was declared with, keyed exactly
    /// like the maps above. Documentation only — read by `hyprpad bindings`,
    /// never by the input path.
    binding_descs: HashMap<GestureKey, String>,
    button_descs: HashMap<crate::report::Button, String>,
    osk_button_descs: HashMap<crate::report::Button, String>,
    own_lizard: bool,
    steam_button_poweroff: Option<u16>,
    sleep_inactivity_timeout: Option<u16>,
    rescan_on_title_change: Option<bool>,
    process_rescan_ms: Option<u64>,
    cursor: CursorConfig,
    cursor_guard: Guard,
    /// `h.cursor { guide_in = … }`: where the pad stays a mouse under a held
    /// guide. `None` until the file says so.
    cursor_guide_guard: Option<Guard>,
    scroll: ScrollConfig,
    scroll_guard: Guard,
    /// `h.scrub { … }`: the guide-layer caret jog wheel. Off until the file
    /// writes the section, which is why the `Default` here is the right one.
    scrub: crate::config::ScrubConfig,
    scrub_guard: Guard,
    haptics: HapticsConfig,
    gamepad: GamepadConfig,
    modes: Vec<ModeDef>,
    default_mode: Option<String>,
    predicates: Vec<Function>,
    /// What each returned handle points at, indexed by the handle's `__slot`.
    slots: Vec<Slot>,
}

/// What a chained `:only_in` / `:not_in` / `:when` call should modify.
#[derive(Clone, Debug)]
enum Slot {
    Binding(GestureKey, String),
    Button(crate::report::Button, String),
    /// A re-binding of an already-bound button, by its index in
    /// [`Build::button_alts`] — the guard lands on the alternate, never on the
    /// binding it sits beside.
    ButtonAlt(usize, String),
    OskButton(crate::report::Button, String),
    Cursor,
    Scroll,
    Scrub,
    Mode(usize),
}

impl Build {
    /// Register a handle slot and return its index.
    fn slot(&mut self, s: Slot) -> usize {
        self.slots.push(s);
        self.slots.len() - 1
    }

    /// Attach a guard to whatever `slot` names. A `Mode` slot has no guard —
    /// `h.mode(…):when(fn)` sets the mode's *rule*, handled separately.
    fn set_guard(&mut self, slot: usize, g: Guard) -> Result<(), String> {
        match self.slots.get(slot).cloned() {
            Some(Slot::Binding(k, _)) => {
                self.binding_guards.insert(k, g);
            }
            Some(Slot::Button(b, _)) => {
                self.button_guards.insert(b, g);
            }
            Some(Slot::ButtonAlt(i, _)) => {
                if let Some(alt) = self.button_alts.get_mut(i) {
                    alt.guard = g;
                }
            }
            Some(Slot::OskButton(b, _)) => {
                self.osk_button_guards.insert(b, g);
            }
            Some(Slot::Cursor) => self.cursor_guard = g,
            Some(Slot::Scroll) => self.scroll_guard = g,
            Some(Slot::Scrub) => self.scrub_guard = g,
            Some(Slot::Mode(_)) | None => {
                return Err("only a binding can be guarded; use h.mode(name).when(fn) \
                            to give a MODE its rule"
                    .to_string())
            }
        }
        Ok(())
    }

    /// Every guard with a human label, for load-time validation.
    fn all_guards(&self) -> Vec<(String, &Guard)> {
        let mut out: Vec<(String, &Guard)> = Vec::new();
        for s in &self.slots {
            let (label, g) = match s {
                Slot::Binding(k, name) => (format!("binding '{name}'"), self.binding_guards.get(k)),
                Slot::Button(b, name) => (format!("button '{name}'"), self.button_guards.get(b)),
                Slot::ButtonAlt(i, name) => {
                    (format!("button '{name}'"), self.button_alts.get(*i).map(|a| &a.guard))
                }
                Slot::OskButton(b, name) => {
                    (format!("osk_button '{name}'"), self.osk_button_guards.get(b))
                }
                Slot::Cursor => ("the cursor".to_string(), Some(&self.cursor_guard)),
                Slot::Scroll => ("scrolling".to_string(), Some(&self.scroll_guard)),
                Slot::Scrub => ("the caret scrub".to_string(), Some(&self.scrub_guard)),
                Slot::Mode(_) => continue,
            };
            if let Some(g) = g {
                out.push((label, g));
            }
        }
        // Not a slot (nothing chains onto it), but a guard all the same, and
        // a typo in it would silently leave the guide-held pad dead.
        if let Some(g) = &self.cursor_guide_guard {
            out.push(("the cursor under a held guide (guide_in)".to_string(), g));
        }
        out
    }

    /// Register a predicate function and return its id.
    fn predicate(&mut self, f: Function) -> usize {
        self.predicates.push(f);
        self.predicates.len() - 1
    }
}

// ---------------------------------------------------------------------------
// The API surface
// ---------------------------------------------------------------------------

/// A Rust error surfaced to Lua. Kept short — mlua wraps it with a traceback
/// that already names the `config.lua` line.
fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(msg.into())
}

/// Build the global `hyprpad` table and install it.
fn install_api(lua: &Lua, build: &Rc<RefCell<Build>>) -> mlua::Result<()> {
    let h = lua.create_table()?;

    // Handle metatables. `__index` is a *function* rather than a method table
    // so the closure it hands back is already bound to that handle's slot —
    // which is what lets both call forms work:
    //
    //   h.bind(..):only_in("desktop")     -- method call, Lua passes the handle
    //   h.mode("game").when(function() …) -- dot call, no handle passed
    //
    // The owner's Hyprland config uses plain dot calls throughout, and the
    // docs/13 sketch spells `h.mode(n).when(fn)` that way, so insisting on one
    // punctuation mark would be a papercut with no upside.
    let guard_mt = lua.create_table()?;
    guard_mt.set("__index", handle_index(lua, build, HandleKind::Guard)?)?;
    let guard_mt = lua.create_registry_value(guard_mt)?;

    let mode_mt = lua.create_table()?;
    mode_mt.set("__index", handle_index(lua, build, HandleKind::Mode)?)?;
    let mode_mt = lua.create_registry_value(mode_mt)?;

    // --- settings ---------------------------------------------------------
    h.set("daemon", section_daemon(lua, build)?)?;
    h.set("cursor", section_cursor(lua, build, &guard_mt)?)?;
    h.set("scroll", section_scroll(lua, build, &guard_mt)?)?;
    h.set("scrub", section_scrub(lua, build, &guard_mt)?)?;
    h.set("haptics", section_haptics(lua, build)?)?;
    h.set("gamepad", section_gamepad(lua, build)?)?;

    // --- bindings ---------------------------------------------------------
    h.set("bind", bind_fn(lua, build, &guard_mt, BindKind::Gesture)?)?;
    h.set("button", bind_fn(lua, build, &guard_mt, BindKind::Button)?)?;
    h.set("osk_button", bind_fn(lua, build, &guard_mt, BindKind::OskButton)?)?;

    // --- modes ------------------------------------------------------------
    h.set("mode", mode_fn(lua, build, &mode_mt)?)?;
    h.set("default_mode", default_mode_fn(lua, build)?)?;

    // --- action constructors ---------------------------------------------
    h.set("workspace", action_ctor(lua, "workspace", "target")?)?;
    h.set("move_to_workspace", action_ctor(lua, "move_to_workspace", "target")?)?;
    h.set("exec", action_ctor(lua, "exec", "cmd")?)?;
    h.set("dispatch", action_ctor(lua, "dispatch", "expr")?)?;
    h.set("key", action_ctor(lua, "key", "name")?)?;
    h.set("mouse", action_ctor(lua, "mouse", "button")?)?;
    h.set("set_mode", action_ctor(lua, "set_mode", "name")?)?;
    h.set("fullscreen", action_nullary(lua, "fullscreen")?)?;
    h.set("clear_mode", action_nullary(lua, "clear_mode")?)?;
    h.set("controller_off", action_nullary(lua, "controller_off")?)?;
    h.set("none", action_nullary(lua, "none")?)?;
    h.set("keyboard", keyboard_ctor(lua)?)?;
    // The on-screen keyboard's own actions — `h.osk "commit"|"shift"|"dismiss"`
    // — only mean something on `h.osk_button` (`value_to_osk_action`).
    h.set("osk", action_ctor(lua, "osk", "keyboard action")?)?;

    lua.globals().set("hyprpad", h)?;
    Ok(())
}

/// Which of the three binding tables `h.bind` / `h.button` / `h.osk_button`
/// writes into.
#[derive(Clone, Copy)]
enum BindKind {
    Gesture,
    Button,
    OskButton,
}

/// Which guard a chained setter builds.
#[derive(Clone, Copy)]
enum GuardKind {
    OnlyIn,
    NotIn,
}

/// Which methods a handle offers.
#[derive(Clone, Copy)]
enum HandleKind {
    /// A binding (or the cursor/scroll virtual bindings): `only_in`, `not_in`,
    /// `when`.
    Guard,
    /// A mode: `when` (its selection rule) and `forward`.
    Mode,
}

/// The `__index` metamethod for both handle kinds: given the handle and a
/// method name, return a closure already bound to that handle's slot.
///
/// Binding at lookup time is what makes `handle.when(f)` and `handle:when(f)`
/// both work — the returned closures ignore a leading handle argument.
fn handle_index(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    kind: HandleKind,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    lua.create_function(move |lua, (this, key): (Table, String)| {
        let slot = slot_of(&this)?;
        let f = match (kind, key.as_str()) {
            (HandleKind::Guard, "only_in") => {
                bound_guard(lua, &build, &this, slot, GuardKind::OnlyIn)?
            }
            (HandleKind::Guard, "not_in") => {
                bound_guard(lua, &build, &this, slot, GuardKind::NotIn)?
            }
            (HandleKind::Guard, "when") => bound_guard_when(lua, &build, &this, slot)?,
            (HandleKind::Mode, "when") => bound_mode_when(lua, &build, &this, slot)?,
            (HandleKind::Mode, "forward") => bound_mode_forward(lua, &build, &this, slot)?,
            (HandleKind::Guard, other) => {
                return Err(err(format!(
                    "a binding has no '{other}'; it takes only_in, not_in or when"
                )))
            }
            (HandleKind::Mode, other) => {
                return Err(err(format!(
                    "a mode has no '{other}'; it takes when or forward"
                )))
            }
        };
        Ok(f)
    })
}

/// `handle:only_in(...)` / `handle.not_in(...)`, bound to one slot.
fn bound_guard(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    this: &Table,
    slot: usize,
    kind: GuardKind,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let this = this.clone();
    lua.create_function(move |_, args: MultiValue| {
        let modes = mode_list(args.into_iter().collect())?;
        if modes.is_empty() {
            return Err(err("only_in/not_in needs at least one mode name"));
        }
        let g = match kind {
            GuardKind::OnlyIn => Guard::OnlyIn(modes),
            GuardKind::NotIn => Guard::NotIn(modes),
        };
        build.borrow_mut().set_guard(slot, g).map_err(err)?;
        Ok(this.clone())
    })
}

/// `handle:when(function(ctx) … end)`, bound to one slot.
fn bound_guard_when(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    this: &Table,
    slot: usize,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let this = this.clone();
    lua.create_function(move |_, args: MultiValue| {
        let f = first_function(&args, "when")?;
        let mut b = build.borrow_mut();
        let id = b.predicate(f);
        b.set_guard(slot, Guard::When(id)).map_err(err)?;
        drop(b);
        Ok(this.clone())
    })
}

/// `mode.when(function(ctx) … end)` — the mode's selection rule.
fn bound_mode_when(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    this: &Table,
    slot: usize,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let this = this.clone();
    lua.create_function(move |_, args: MultiValue| {
        let f = first_function(&args, "when")?;
        let mut b = build.borrow_mut();
        let Some(Slot::Mode(idx)) = b.slots.get(slot).cloned() else {
            return Err(err("when called on something that is not a mode handle"));
        };
        let id = b.predicate(f);
        b.modes[idx].rule = Some(id);
        drop(b);
        Ok(this.clone())
    })
}

/// `mode.forward(true)` — the alternative spelling of
/// `h.mode(name, { forward = true })`.
fn bound_mode_forward(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    this: &Table,
    slot: usize,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let this = this.clone();
    lua.create_function(move |_, args: MultiValue| {
        let on = args
            .iter()
            .find_map(|v| match v {
                Value::Boolean(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(true);
        let mut b = build.borrow_mut();
        let Some(Slot::Mode(idx)) = b.slots.get(slot).cloned() else {
            return Err(err("forward called on something that is not a mode handle"));
        };
        b.modes[idx].forward = on;
        drop(b);
        Ok(this.clone())
    })
}

/// The first function among `args`, ignoring a leading handle from a `:` call.
fn first_function(args: &MultiValue, what: &str) -> mlua::Result<Function> {
    args.iter()
        .find_map(|v| match v {
            Value::Function(f) => Some(f.clone()),
            _ => None,
        })
        .ok_or_else(|| err(format!("{what} needs a function, e.g. {what}(function(ctx) … end)")))
}

/// Make a handle table `{ __slot = n }` carrying `mt`.
fn handle(lua: &Lua, mt: &mlua::RegistryKey, slot: usize) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("__slot", slot)?;
    t.set_metatable(Some(lua.registry_value::<Table>(mt)?));
    Ok(t)
}

/// Read a handle's `__slot`.
fn slot_of(t: &Table) -> mlua::Result<usize> {
    t.get::<Option<usize>>("__slot")?
        .ok_or_else(|| err("not a hyprpad binding handle — call this with ':' on what h.bind returned"))
}

/// `h.bind(key, [desc,] action)` and friends. Returns a guardable handle.
fn bind_fn(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    guard_mt: &mlua::RegistryKey,
    kind: BindKind,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let mt = lua.create_registry_value(lua.registry_value::<Table>(guard_mt)?)?;
    lua.create_function(move |lua, args: MultiValue| {
        let mut it = args.into_iter();
        let key = string_arg(it.next(), "the binding key")?;
        // `h.bind(key, desc, action)` mirrors the owner's `o.bind(keys, desc,
        // action)`. Arity decides which is which: with a third argument the
        // middle one is the description, otherwise the middle one IS the action
        // (an action can be a plain string too, so there is nothing else to go
        // on). The description is stored for `hyprpad bindings` — the cheat
        // sheet — and is never consulted by the input path.
        let second = it.next();
        let (desc, value) = match it.next() {
            Some(third) => (Some(string_arg(second, "the description")?), third),
            None => (None, second.ok_or_else(|| err("h.bind needs an action"))?),
        };
        let mut b = build.borrow_mut();
        let slot = match kind {
            BindKind::Gesture => {
                let action = value_to_action(&value)?;
                let k = GestureKey::parse(&key).map_err(err)?;
                b.bindings.insert(k, action);
                if let Some(d) = desc {
                    b.binding_descs.insert(k, d);
                }
                b.slot(Slot::Binding(k, key))
            }
            // A bare button takes any action a chord takes: a key or mouse
            // button is held with it, anything else fires once on the press
            // edge (`ButtonAction::classify`, the same sort the TOML parser
            // does).
            BindKind::Button => {
                let action = value_to_action(&value)?;
                let btn = parse_button(&key.trim().to_ascii_lowercase()).map_err(err)?;
                let what = ButtonAction::classify(action)
                    .map_err(|e| err(format!("{e} (button '{key}')")))?;
                match b.buttons.entry(btn) {
                    // A button bound a *second* time is the same button meaning
                    // something else in another mode — `b` is backspace on the
                    // desktop and escape under the cheat sheet — so it becomes
                    // an alternate rather than overwriting the first binding.
                    // Modes are exclusive, so only one of them is ever live.
                    Entry::Occupied(_) => {
                        let at = b.button_alts.len();
                        b.button_alts.push(ButtonAlt {
                            button: btn,
                            action: what,
                            guard: Guard::Always,
                            desc,
                        });
                        b.slot(Slot::ButtonAlt(at, key))
                    }
                    Entry::Vacant(first) => {
                        first.insert(what);
                        if let Some(d) = desc {
                            b.button_descs.insert(btn, d);
                        }
                        b.slot(Slot::Button(btn, key))
                    }
                }
            }
            // The on-screen keyboard's table has its own action set: a key
            // typed through the keyboard, or one of its three verbs.
            BindKind::OskButton => {
                let btn = parse_button(&key.trim().to_ascii_lowercase()).map_err(err)?;
                let what = value_to_osk_action(&value)
                    .map_err(|e| err(format!("{e} (osk_button '{key}')")))?;
                b.osk_buttons.insert(btn, what);
                if let Some(d) = desc {
                    b.osk_button_descs.insert(btn, d);
                }
                b.slot(Slot::OskButton(btn, key))
            }
        };
        drop(b);
        handle(lua, &mt, slot)
    })
}



/// `h.mode(name, { forward = true })` — declare a mode. Returns a handle whose
/// `.when(fn)` gives it its selection rule.
fn mode_fn(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    mode_mt: &mlua::RegistryKey,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let mt = lua.create_registry_value(lua.registry_value::<Table>(mode_mt)?)?;
    lua.create_function(move |lua, (name, opts): (String, Option<Table>)| {
        let mut forward = false;
        if let Some(t) = opts {
            for pair in t.pairs::<String, Value>() {
                let (k, v) = pair?;
                match k.as_str() {
                    "forward" => forward = as_bool(&v, "forward")?,
                    other => {
                        return Err(err(format!(
                            "unknown h.mode option '{other}' (want: forward)"
                        )))
                    }
                }
            }
        }
        let mut b = build.borrow_mut();
        if b.modes.iter().any(|m| m.name == name) {
            return Err(err(format!("mode '{name}' is declared twice")));
        }
        b.modes.push(ModeDef { name, rule: None, forward });
        let idx = b.modes.len() - 1;
        let slot = b.slot(Slot::Mode(idx));
        drop(b);
        handle(lua, &mt, slot)
    })
}



/// `h.default_mode "desktop"`.
fn default_mode_fn(lua: &Lua, build: &Rc<RefCell<Build>>) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    lua.create_function(move |_, name: String| {
        build.borrow_mut().default_mode = Some(name);
        Ok(())
    })
}

// --- settings sections -----------------------------------------------------

/// `h.daemon { own_lizard = true, steam_button_poweroff = "off",
/// sleep_inactivity_timeout = 600, rescan_on_title_change = true,
/// process_rescan_ms = 500 }`.
fn section_daemon(lua: &Lua, build: &Rc<RefCell<Build>>) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    lua.create_function(move |_, t: Table| {
        let mut b = build.borrow_mut();
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "own_lizard" => b.own_lizard = as_bool(&v, &k)?,
                "steam_button_poweroff" => {
                    b.steam_button_poweroff = Some(as_power_setting(&v, &k)?)
                }
                "sleep_inactivity_timeout" => {
                    b.sleep_inactivity_timeout = Some(as_power_setting(&v, &k)?)
                }
                "rescan_on_title_change" => {
                    b.rescan_on_title_change = Some(as_bool(&v, &k)?)
                }
                "process_rescan_ms" => b.process_rescan_ms = Some(as_millis(&v, &k)?),
                other => {
                    return Err(unknown_key(
                        "h.daemon",
                        other,
                        &[
                            "own_lizard",
                            "steam_button_poweroff",
                            "sleep_inactivity_timeout",
                            "rescan_on_title_change",
                            "process_rescan_ms",
                        ],
                    ))
                }
            }
        }
        Ok(())
    })
}

/// `h.cursor { … }` — the `[cursor]` knobs, plus the guard keys that make the
/// cursor a first-class guardable "virtual binding": `only_in` / `not_in` for
/// where the pad drives the cursor with the guide up, and `guide_in` for where
/// it keeps doing so **while the guide is held** (the Steam-Input-style
/// guide-mouse; off unless listed).
fn section_cursor(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    guard_mt: &mlua::RegistryKey,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let mt = lua.create_registry_value(lua.registry_value::<Table>(guard_mt)?)?;
    lua.create_function(move |lua, t: Table| {
        let mut b = build.borrow_mut();
        let mut inline: Option<Guard> = None;
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "sens" | "sensitivity" => b.cursor.sens = as_f64(&v, &k)?,
                "one_euro_min_cutoff" | "min_cutoff" => {
                    b.cursor.one_euro_min_cutoff = as_f64(&v, &k)?
                }
                "one_euro_beta" | "beta" => b.cursor.one_euro_beta = as_f64(&v, &k)?,
                "one_euro_d_cutoff" | "d_cutoff" => b.cursor.one_euro_d_cutoff = as_f64(&v, &k)?,
                "hysteresis" | "hysteresis_margin" => b.cursor.hysteresis = as_f64(&v, &k)?,
                "deadzone" | "dead_zone" => b.cursor.deadzone = as_f64(&v, &k)?,
                "only_in" => inline = Some(Guard::OnlyIn(mode_list(vec![v])?)),
                "not_in" => inline = Some(Guard::NotIn(mode_list(vec![v])?)),
                "guide_in" | "guide_only_in" => {
                    let modes = mode_list(vec![v])?;
                    if modes.is_empty() {
                        return Err(err(
                            "guide_in needs at least one mode name (leave it out to keep \
                             the pad off under the guide)",
                        ));
                    }
                    b.cursor_guide_guard = Some(Guard::OnlyIn(modes));
                }
                other => {
                    return Err(unknown_key(
                        "h.cursor",
                        other,
                        &[
                            "sens",
                            "one_euro_min_cutoff",
                            "one_euro_beta",
                            "one_euro_d_cutoff",
                            "hysteresis",
                            "deadzone",
                            "only_in",
                            "not_in",
                            "guide_in",
                        ],
                    ))
                }
            }
        }
        let slot = b.slot(Slot::Cursor);
        if let Some(g) = inline {
            b.set_guard(slot, g).map_err(err)?;
        }
        drop(b);
        handle(lua, &mt, slot)
    })
}

/// `h.scroll { … }` — the `[scroll]` knobs, guardable like the cursor.
fn section_scroll(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    guard_mt: &mlua::RegistryKey,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let mt = lua.create_registry_value(lua.registry_value::<Table>(guard_mt)?)?;
    lua.create_function(move |lua, t: Table| {
        let mut b = build.borrow_mut();
        let mut inline: Option<Guard> = None;
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "mode" => {
                    b.scroll.mode = ScrollMode::parse(&as_string(&v, &k)?).map_err(err)?;
                }
                "sensitivity" | "sens" => b.scroll.sensitivity = as_f64(&v, &k)?,
                "natural" | "invert" => b.scroll.natural = as_bool(&v, &k)?,
                "horizontal" | "swipe_horizontal" => b.scroll.horizontal = as_bool(&v, &k)?,
                "circular_step_degrees" | "step_degrees" | "step" => {
                    b.scroll.circular_step_degrees = as_f64(&v, &k)?
                }
                "circular_min_radius" | "min_radius" => {
                    b.scroll.circular_min_radius = as_f64(&v, &k)?
                }
                "only_in" => inline = Some(Guard::OnlyIn(mode_list(vec![v])?)),
                "not_in" => inline = Some(Guard::NotIn(mode_list(vec![v])?)),
                other => {
                    return Err(unknown_key(
                        "h.scroll",
                        other,
                        &[
                            "mode",
                            "sensitivity",
                            "natural",
                            "horizontal",
                            "circular_step_degrees",
                            "circular_min_radius",
                            "only_in",
                            "not_in",
                        ],
                    ))
                }
            }
        }
        let slot = b.slot(Slot::Scroll);
        if let Some(g) = inline {
            b.set_guard(slot, g).map_err(err)?;
        }
        drop(b);
        handle(lua, &mt, slot)
    })
}

/// `h.scrub { … }` — the `[scrub]` knobs, guardable like the cursor and the
/// scroll.
///
/// Writing the section IS the opt-in: the scrub is off by default
/// ([`crate::config::ScrubConfig`]), so `h.scrub {}` with nothing in it turns
/// the caret jog wheel on with every default, and `h.scrub { enabled = false }`
/// switches it back off without deleting the tuning beside it.
///
/// ```lua
/// h.scrub {
///   detent_deg       = 15,        -- one caret step per 15° (24 per revolution)
///   fast_deg_per_s   = 360,       -- above one revolution/s, two steps per detent
///   fast_min_detents = 2,         -- ... once two detents in a row are that fast
///   slow_deg_per_s   = 180,       -- drop back below this (the 2:1 hysteresis)
///   word_tier        = true,      -- the top rung is ctrl+arrow, not ×4
///   select           = "l5",      -- hold to select as you scrub (Shift)
///   only_in          = { "desktop" },
/// }
/// ```
fn section_scrub(
    lua: &Lua,
    build: &Rc<RefCell<Build>>,
    guard_mt: &mlua::RegistryKey,
) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    let mt = lua.create_registry_value(lua.registry_value::<Table>(guard_mt)?)?;
    lua.create_function(move |lua, t: Table| {
        let mut b = build.borrow_mut();
        // Present = on; an explicit `enabled = false` below overrides it.
        b.scrub.enabled = true;
        let mut inline: Option<Guard> = None;
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "enabled" | "enable" | "on" => b.scrub.enabled = as_bool(&v, &k)?,
                "detent_deg" | "detent_degrees" | "step_degrees" | "step" => {
                    b.scrub.detent_deg = as_f64(&v, &k)?
                }
                "min_radius" => b.scrub.min_radius = as_f64(&v, &k)?,
                "fast_deg_per_s" | "fast" => b.scrub.fast_deg_per_s = as_f64(&v, &k)?,
                "slow_deg_per_s" | "slow" => b.scrub.slow_deg_per_s = as_f64(&v, &k)?,
                "fast_min_detents" | "min_detents" => {
                    let n = as_f64(&v, &k)?;
                    if !n.is_finite() || n < 0.0 || n > f64::from(u32::MAX) {
                        return Err(err(format!(
                            "h.scrub {k} wants a whole number of detents, got {n}"
                        )));
                    }
                    b.scrub.fast_min_detents = n as u32;
                }
                "word_tier" | "words" => b.scrub.word_tier = as_bool(&v, &k)?,
                "select" | "select_with" => {
                    let name = as_string(&v, &k)?.trim().to_ascii_lowercase();
                    b.scrub.select = crate::config::parse_button(&name).map_err(err)?;
                }
                "only_in" => inline = Some(Guard::OnlyIn(mode_list(vec![v])?)),
                "not_in" => inline = Some(Guard::NotIn(mode_list(vec![v])?)),
                other => {
                    return Err(unknown_key(
                        "h.scrub",
                        other,
                        &[
                            "enabled",
                            "detent_deg",
                            "min_radius",
                            "fast_deg_per_s",
                            "fast_min_detents",
                            "slow_deg_per_s",
                            "word_tier",
                            "select",
                            "only_in",
                            "not_in",
                        ],
                    ))
                }
            }
        }
        let slot = b.slot(Slot::Scrub);
        if let Some(g) = inline {
            b.set_guard(slot, g).map_err(err)?;
        }
        drop(b);
        handle(lua, &mt, slot)
    })
}

/// `h.haptics { … }` — the `[haptics]` knobs.
fn section_haptics(lua: &Lua, build: &Rc<RefCell<Build>>) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    lua.create_function(move |_, t: Table| {
        let mut b = build.borrow_mut();
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "enabled" | "enable" | "on" => b.haptics.enabled = as_bool(&v, &k)?,
                "crossing" | "crossings" => b.haptics.crossing = as_bool(&v, &k)?,
                "commit" | "commits" => b.haptics.commit = as_bool(&v, &k)?,
                "gesture" | "gestures" => b.haptics.gesture = as_bool(&v, &k)?,
                "scroll" | "scroll_ticks" => b.haptics.scroll = as_bool(&v, &k)?,
                "buttons" | "bare_buttons" => b.haptics.buttons = as_bool(&v, &k)?,
                "cursor" | "cursor_texture" => b.haptics.cursor = as_bool(&v, &k)?,
                "cursor_spacing_px" | "cursor_spacing" => {
                    b.haptics.cursor_spacing_px = as_f64(&v, &k)?
                }
                "intensity" | "strength" | "gain" => b.haptics.intensity = as_f64(&v, &k)?,
                other => {
                    return Err(unknown_key(
                        "h.haptics",
                        other,
                        &[
                            "enabled",
                            "crossing",
                            "commit",
                            "gesture",
                            "scroll",
                            "buttons",
                            "cursor",
                            "cursor_spacing_px",
                            "intensity",
                        ],
                    ))
                }
            }
        }
        Ok(())
    })
}

/// `h.gamepad { … }` — the `[gamepad]` knobs.
fn section_gamepad(lua: &Lua, build: &Rc<RefCell<Build>>) -> mlua::Result<Function> {
    let build = Rc::clone(build);
    lua.create_function(move |_, t: Table| {
        let mut b = build.borrow_mut();
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match k.as_str() {
                "enabled" | "enable" | "on" => b.gamepad.enabled = as_bool(&v, &k)?,
                "forward_guide" | "guide" => b.gamepad.forward_guide = as_bool(&v, &k)?,
                "rumble" | "force_feedback" | "ff" => b.gamepad.rumble = as_bool(&v, &k)?,
                "rumble_mode" => {
                    b.gamepad.rumble_mode =
                        crate::config::RumbleMode::parse(&as_string(&v, &k)?).map_err(err)?;
                }
                "rumble_intensity" | "rumble_gain" => {
                    b.gamepad.rumble_intensity = as_f64(&v, &k)?
                }
                // Which virtual controller games see, and — for the Steam one —
                // which Valve identity it presents. See `crate::uhid::profile`.
                "kind" => {
                    b.gamepad.kind =
                        crate::config::GamepadKind::parse(&as_string(&v, &k)?).map_err(err)?;
                }
                "identity" => {
                    b.gamepad.identity =
                        crate::uhid::Identity::parse(&as_string(&v, &k)?).map_err(err)?;
                }
                other => {
                    return Err(unknown_key(
                        "h.gamepad",
                        other,
                        &[
                            "enabled",
                            "kind",
                            "identity",
                            "forward_guide",
                            "rumble",
                            "rumble_mode",
                            "rumble_intensity",
                        ],
                    ))
                }
            }
        }
        Ok(())
    })
}

// --- action constructors ---------------------------------------------------

/// A one-argument action constructor, e.g. `h.exec "omarchy-menu"`. Returns the
/// tagged table [`value_to_action`] understands.
fn action_ctor(lua: &Lua, verb: &'static str, arg: &'static str) -> mlua::Result<Function> {
    lua.create_function(move |lua, v: Value| {
        let text = match &v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => {
                // `h.workspace(3)` should mean workspace 3, not "3.0".
                if n.fract() == 0.0 { format!("{}", *n as i64) } else { n.to_string() }
            }
            other => {
                return Err(err(format!(
                    "h.{verb} needs a {arg} (string), got {}",
                    other.type_name()
                )))
            }
        };
        let t = lua.create_table()?;
        t.set(ACTION_TAG, verb)?;
        t.set("arg", text)?;
        Ok(t)
    })
}

/// A zero-argument action constructor, e.g. `h.fullscreen()`.
fn action_nullary(lua: &Lua, verb: &'static str) -> mlua::Result<Function> {
    lua.create_function(move |lua, ()| {
        let t = lua.create_table()?;
        t.set(ACTION_TAG, verb)?;
        Ok(t)
    })
}

/// `h.keyboard { mode = "split", reflow = false }`, `h.keyboard "split"`, or
/// bare `h.keyboard()`.
fn keyboard_ctor(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(move |lua, v: Option<Value>| {
        let mut words = String::new();
        match v {
            None | Some(Value::Nil) => {}
            Some(Value::String(s)) => words = s.to_str()?.to_string(),
            Some(Value::Table(t)) => {
                for pair in t.pairs::<String, Value>() {
                    let (k, val) = pair?;
                    match k.as_str() {
                        "mode" => {
                            words.push(' ');
                            words.push_str(&as_string(&val, &k)?);
                        }
                        "reflow" => {
                            words.push(' ');
                            words.push_str(if as_bool(&val, &k)? { "reflow" } else { "overlay" });
                        }
                        other => {
                            return Err(unknown_key("h.keyboard", other, &["mode", "reflow"]))
                        }
                    }
                }
            }
            Some(other) => {
                return Err(err(format!(
                    "h.keyboard takes a table or a string, got {}",
                    other.type_name()
                )))
            }
        }
        let t = lua.create_table()?;
        t.set(ACTION_TAG, "keyboard")?;
        t.set("arg", words.trim().to_string())?;
        Ok(t)
    })
}

/// The field marking a table as one of hyprpad's action constructors.
const ACTION_TAG: &str = "__hyprpad_action";

/// Turn a Lua value into an [`Action`].
///
/// Two spellings are accepted, both mapping onto the same `Action` enum:
/// * a constructor table — `h.exec "walker"` (the documented form), and
/// * a plain string — `"exec walker"`, parsed by the *same* grammar the TOML
///   front-end uses, so a config can be ported a line at a time and any action
///   spelled in the docs works in either file.
fn value_to_action(v: &Value) -> mlua::Result<Action> {
    match v {
        Value::String(s) => Action::parse(s.to_str()?.as_ref()).map_err(err),
        Value::Table(t) => {
            let verb: Option<String> = t.get(ACTION_TAG)?;
            let Some(verb) = verb else {
                return Err(err(
                    "that table is not an action — use one of h.workspace/h.exec/h.dispatch/\
                     h.keyboard/h.key/h.mouse/h.fullscreen/h.set_mode/h.clear_mode/\
                     h.controller_off/h.none",
                ));
            };
            let arg: String = t.get::<Option<String>>("arg")?.unwrap_or_default();
            match verb.as_str() {
                "fullscreen" => Ok(Action::ToggleFullscreen),
                "clear_mode" => Ok(Action::ClearMode),
                "controller_off" => Ok(Action::ControllerOff),
                "none" => Ok(Action::None),
                "key" => KeyChord::parse(&arg).map(Action::Key).map_err(err),
                // A mouse button is a key in evdev's code space; the daemon
                // routes it to the virtual pointer by its code.
                "mouse" => mouse_code(&arg).map(|c| Action::Key(c.into())).map_err(err),
                "set_mode" => {
                    if arg.is_empty() {
                        Err(err("h.set_mode needs a mode name"))
                    } else {
                        Ok(Action::SetMode(arg))
                    }
                }
                // The rest share the TOML front-end's own verb grammar, so the
                // two front-ends can never drift on what an action means.
                "workspace" | "exec" | "dispatch" | "keyboard" => {
                    Action::parse(&format!("{verb} {arg}")).map_err(err)
                }
                "move_to_workspace" => Action::parse(&format!("movetoworkspace {arg}")).map_err(err),
                // The keyboard's own verbs are `h.osk_button` bindings, live
                // only while it is up; a chord or a bare button is not.
                "osk" => Err(err(format!(
                    "h.osk \"{arg}\" is an on-screen keyboard binding — it belongs on \
                     h.osk_button, not on h.bind or h.button"
                ))),
                other => Err(err(format!("unknown action '{other}'"))),
            }
        }
        other => Err(err(format!(
            "expected an action (e.g. h.exec \"walker\"), got {}",
            other.type_name()
        ))),
    }
}

/// Turn a Lua value into an [`OskAction`] — what `h.osk_button` takes. The same
/// two spellings as [`value_to_action`]: a constructor table (`h.key "space"`,
/// `h.osk "shift"`, `h.none()`) or a plain string in the TOML grammar
/// (`"key space"`, `"osk shift"`, `"none"`), both read by [`OskAction::parse`]
/// so the two front-ends cannot drift on what the keyboard's table means.
fn value_to_osk_action(v: &Value) -> Result<OskAction, String> {
    let text = match v {
        Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
        Value::Table(t) => {
            let verb: Option<String> = t.get(ACTION_TAG).map_err(|e| e.to_string())?;
            let arg: String = t
                .get::<Option<String>>("arg")
                .map_err(|e| e.to_string())?
                .unwrap_or_default();
            match verb.as_deref() {
                Some(v @ ("key" | "mouse" | "osk" | "none")) => format!("{v} {arg}"),
                Some(other) => {
                    return Err(format!(
                        "h.osk_button values must be a key (h.key \"space\") or one of the \
                         keyboard's own actions (h.osk \"commit\"|\"shift\"|\"dismiss\"), \
                         got h.{other}"
                    ))
                }
                None => {
                    return Err("that table is not an action — use h.key, h.osk or h.none".into())
                }
            }
        }
        other => {
            return Err(format!(
                "expected an action (e.g. h.key \"space\"), got {}",
                other.type_name()
            ))
        }
    };
    OskAction::parse(&text)
}

// --- small value helpers ---------------------------------------------------

/// Flatten `("a", "b")` or `({"a","b"})` into a list of mode names.
///
/// A binding handle appearing first — what Lua's `:` call syntax passes — is
/// skipped rather than mistaken for a table of names.
fn mode_list(values: Vec<Value>) -> mlua::Result<Vec<String>> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::String(s) => out.push(s.to_str()?.to_string()),
            Value::Table(t) if t.raw_get::<Option<usize>>("__slot")?.is_some() => {}
            Value::Table(t) => {
                for item in t.sequence_values::<String>() {
                    out.push(item?);
                }
            }
            Value::Nil => {}
            other => {
                return Err(err(format!(
                    "a mode name must be a string (or a table of strings), got {}",
                    other.type_name()
                )))
            }
        }
    }
    Ok(out)
}

fn string_arg(v: Option<Value>, what: &str) -> mlua::Result<String> {
    match v {
        Some(Value::String(s)) => Ok(s.to_str()?.to_string()),
        Some(other) => Err(err(format!("{what} must be a string, got {}", other.type_name()))),
        None => Err(err(format!("missing {what}"))),
    }
}

fn as_f64(v: &Value, key: &str) -> mlua::Result<f64> {
    let n = match v {
        Value::Integer(i) => *i as f64,
        Value::Number(n) => *n,
        // A number written as a string (a habit from the TOML file) is accepted
        // rather than silently type-erroring on a config being ported.
        Value::String(s) => s
            .to_str()?
            .trim()
            .parse::<f64>()
            .map_err(|_| err(format!("'{key}' must be a number, got the string {s:?}")))?,
        other => {
            return Err(err(format!("'{key}' must be a number, got {}", other.type_name())))
        }
    };
    if !n.is_finite() {
        return Err(err(format!("'{key}' must be a finite number")));
    }
    Ok(n)
}

/// One of the two `h.daemon` firmware power knobs: the string `"off"`, or a
/// whole number in `0..=65535` written to the setting raw.
///
/// The same grammar the TOML front-end's `parse_power_setting` reads, so the
/// two files spell the knob identically. The units are the firmware's and are
/// UNVERIFIED (`docs/research/guide-hold-poweroff.md` §6).
fn as_power_setting(v: &Value, key: &str) -> mlua::Result<u16> {
    match v {
        Value::String(s) => {
            let s = s.to_str()?;
            let s = s.trim();
            if s.eq_ignore_ascii_case("off") {
                Ok(crate::lizard::POWER_SETTING_OFF)
            } else {
                Err(err(format!(
                    "'{key}' takes \"off\" or a number, got the string {s:?}"
                )))
            }
        }
        other => {
            let n = as_f64(other, key)?;
            if n < 0.0 || n > f64::from(u16::MAX) || n.fract() != 0.0 {
                return Err(err(format!(
                    "'{key}' must be \"off\" or a whole number 0-65535, got {n}"
                )));
            }
            Ok(n as u16)
        }
    }
}

fn as_bool(v: &Value, key: &str) -> mlua::Result<bool> {
    match v {
        Value::Boolean(b) => Ok(*b),
        other => Err(err(format!("'{key}' must be true or false, got {}", other.type_name()))),
    }
}

/// A whole number of milliseconds (`0` = off). Built on [`as_f64`] so it accepts
/// the same spellings every other numeric knob does, and rejects the ones a
/// timer cannot mean.
fn as_millis(v: &Value, key: &str) -> mlua::Result<u64> {
    let n = as_f64(v, key)?;
    if n < 0.0 || n.fract() != 0.0 {
        return Err(err(format!(
            "'{key}' must be a whole number of milliseconds (0 = off), got {n}"
        )));
    }
    Ok(n as u64)
}

fn as_string(v: &Value, key: &str) -> mlua::Result<String> {
    match v {
        Value::String(s) => Ok(s.to_str()?.to_string()),
        other => Err(err(format!("'{key}' must be a string, got {}", other.type_name()))),
    }
}

/// The standard "you wrote a key I don't know" error, listing what is valid —
/// a typo in a config should never be a silent no-op.
fn unknown_key(section: &str, got: &str, want: &[&str]) -> mlua::Error {
    err(format!("unknown {section} key '{got}' (want: {})", want.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ButtonAction::{Fire, Hold};
    use crate::config::{ModeState, OskAction, WorkspaceTarget};
    use crate::gesture::GestureEvent;
    use crate::mode::Focus;
    use crate::report::Button;

    fn load(src: &str) -> Config {
        load_str(src, "test.lua").expect("config should load")
    }

    /// A context in which only the focused window is interesting — most of
    /// them, since `ctx.layers` has its own tests.
    fn focused(focus: Focus) -> Context {
        Context { focus, ..Context::default() }
    }

    /// A context in which only the overlays are interesting.
    fn showing(namespaces: &[&str]) -> Context {
        Context {
            layers: namespaces.iter().map(|s| s.to_string()).collect(),
            ..Context::default()
        }
    }

    /// A context in which only the session lock is interesting.
    fn locked_session() -> Context {
        Context { locked: true, ..Context::default() }
    }

    fn desktop() -> ModeState {
        ModeState::new("desktop", vec![])
    }

    #[test]
    fn settings_sections_populate_the_same_structs_as_toml() {
        let c = load(
            r#"
            local h = hyprpad
            h.daemon { own_lizard = true }
            h.cursor { sens = 0.06, one_euro_min_cutoff = 0.3, hysteresis = 0.0008 }
            h.scroll { mode = "circular", sensitivity = 1.0, circular_step_degrees = 15.0 }
            h.haptics { cursor_spacing_px = 96 }
            h.gamepad { enabled = true, rumble_mode = "pulse", kind = "steam", identity = "deck" }
            "#,
        );
        assert!(c.own_lizard());
        assert_eq!(c.cursor().sens, 0.06);
        assert_eq!(c.cursor().one_euro_min_cutoff, 0.3);
        assert_eq!(c.cursor().hysteresis, 0.0008);
        assert_eq!(c.scroll().mode, ScrollMode::Circular);
        assert_eq!(c.haptics().cursor_spacing_px, 96.0);
        assert_eq!(c.gamepad().rumble_mode, crate::config::RumbleMode::Pulse);
        assert_eq!(c.gamepad().kind, crate::config::GamepadKind::Steam);
        assert_eq!(c.gamepad().identity, crate::uhid::Identity::Deck);
        // Untouched knobs keep their built-in defaults, exactly as in TOML.
        assert_eq!(c.cursor().one_euro_beta, CursorConfig::default().one_euro_beta);
    }

    #[test]
    fn action_constructors_map_onto_the_action_enum() {
        let c = load(
            r#"
            local h = hyprpad
            h.bind("guide+r1", h.workspace "+1")
            h.bind("guide+l1", h.workspace(-1))
            h.bind("guide+x",  h.exec "omarchy-menu")
            h.bind("guide+b",  h.dispatch "hl.dsp.window.close()")
            h.bind("guide+y",  h.keyboard { mode = "split" })
            h.bind("guide+menu", h.fullscreen())
            h.bind("guide+view", h.set_mode "game")
            h.bind("guide+r3", h.clear_mode())
            "#,
        );
        use crate::osk::OskMode;
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperL1)),
            Action::Workspace(WorkspaceTarget::Relative(-1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::Exec("omarchy-menu".into())
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::B)),
            Action::Dispatch("hl.dsp.window.close()".into())
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::ToggleKeyboard { mode: OskMode::Split, reflow: false }
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Menu)),
            Action::ToggleFullscreen
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::View)),
            Action::SetMode("game".into())
        );
        assert_eq!(c.resolve(&GestureEvent::GuideChord(Button::R3)), Action::ClearMode);
    }

    #[test]
    fn action_strings_are_accepted_by_the_toml_grammar() {
        let c = load(r#"hyprpad.bind("guide+r1", "workspace +1")"#);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
    }

    /// `h.workspace` takes a Hyprland selector, a relative step, or a number,
    /// and both front-ends share the grammar — so the TOML spelling of a
    /// selector is the same word.
    #[test]
    fn workspace_takes_hyprland_selectors_from_either_front_end() {
        let c = load(
            r#"
            local h = hyprpad
            h.bind("guide+x",  h.workspace "emptyn")
            h.bind("guide+a",  h.workspace(3))
            h.bind("guide+r1", h.workspace "+1")
            h.bind("guide+b",  h.workspace "name:foo")
            h.bind("guide+y",  h.move_to_workspace "emptyn")
            h.bind("guide+menu", "workspace emptyn")     -- the TOML grammar
            "#,
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::X)),
            Action::Workspace(WorkspaceTarget::Selector("emptyn".into()))
        );
        // Not `Selector("3")`, and never `name:3`.
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::A)),
            Action::Workspace(WorkspaceTarget::Number(3))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::B)),
            Action::Workspace(WorkspaceTarget::Selector("name:foo".into()))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Y)),
            Action::MoveWindowToWorkspace(WorkspaceTarget::Selector("emptyn".into()))
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::Menu)),
            Action::Workspace(WorkspaceTarget::Selector("emptyn".into()))
        );
    }

    #[test]
    fn bind_accepts_an_hl_style_description() {
        let c = load(r#"hyprpad.bind("guide+r1", "Workspace right", hyprpad.workspace "+1")"#);
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
    }

    #[test]
    fn buttons_and_osk_buttons_land_in_their_own_tables() {
        let c = load(
            r#"
            local h = hyprpad
            h.button("dpad_up", h.key "up")
            h.button("a", h.key "enter")
            h.osk_button("y", h.key "space")
            "#,
        );
        assert_eq!(c.buttons().get(&Button::DpadUp), Some(&Hold(103.into())));
        assert_eq!(c.buttons().get(&Button::A), Some(&Hold(28.into())));
        assert_eq!(c.osk_buttons().get(&Button::Y), Some(&OskAction::Key(57.into())));
        assert!(c.buttons().get(&Button::Y).is_none());
    }

    #[test]
    fn an_osk_button_takes_the_keyboards_own_actions_over_the_built_ins() {
        use OskAction::{Commit, Dismiss, Key, Shift};
        let c = load(
            r#"
            local h = hyprpad
            h.osk_button("l2", h.osk "shift")
            h.osk_button("r2", "osk commit")                 -- the TOML grammar, as a string
            h.osk_button("b", "Put it away", h.osk "dismiss")
            h.osk_button("menu", h.none())                   -- drop the built-in
            h.osk_button("l1", h.key "tab")
            "#,
        );
        let raw = c.osk_buttons();
        assert_eq!(raw.get(&Button::TriggerL2Full), Some(&Shift));
        assert_eq!(raw.get(&Button::TriggerR2Full), Some(&Commit));
        assert_eq!(raw.get(&Button::B), Some(&Dismiss));
        assert_eq!(raw.get(&Button::Menu), Some(&OskAction::None));
        assert_eq!(raw.get(&Button::BumperL1), Some(&Key(15.into())));
        assert_eq!(c.osk_button_descs.get(&Button::B).map(String::as_str), Some("Put it away"));

        // Layered over the built-ins: Menu is gone, R2 is a commit now, and
        // the untouched built-ins (pad clicks, Y, X) are still there.
        let st = ModeState::new(c.default_mode(), Vec::new());
        let live = c.osk_buttons_in(&st);
        assert_eq!(live.get(&Button::Menu), None);
        assert_eq!(live.get(&Button::TriggerR2Full), Some(&Commit));
        assert_eq!(live.get(&Button::PadRightClick), Some(&Commit));
        assert_eq!(live.get(&Button::Y), Some(&Key(57.into())));
        assert_eq!(live.get(&Button::BumperL1), Some(&Key(15.into())));

        // The keyboard's verbs mean nothing on a chord or a bare button.
        let e = load_str(r#"hyprpad.bind("guide+a", hyprpad.osk "commit")"#, "t.lua").unwrap_err();
        assert!(e.contains("h.osk_button"), "{e}");
        let e = load_str(r#"hyprpad.button("a", hyprpad.osk "shift")"#, "t.lua").unwrap_err();
        assert!(e.contains("h.osk_button"), "{e}");
        let e = load_str(r#"hyprpad.osk_button("a", hyprpad.osk "frobnicate")"#, "t.lua").unwrap_err();
        assert!(e.contains("commit|shift|dismiss"), "{e}");
        assert!(e.contains("'a'"), "should name the button: {e}");
    }

    #[test]
    fn a_button_takes_any_action_held_or_fired() {
        let c = load(
            r#"
            local h = hyprpad
            h.button("y", h.exec "foo")
            h.button("x", h.keyboard { mode = "split" })
            h.button("a", "Close window", h.dispatch "hl.dsp.window.close()")
            h.button("r1", "workspace +1")          -- the TOML grammar, as a plain string
            h.button("l1", h.fullscreen())
            h.button("r4", h.set_mode "desktop")
            h.button("l4", h.clear_mode())
            h.button("dpad_up", h.key "up")         -- still held with the button
            h.button("r2", h.mouse "left")
            "#,
        );
        let b = c.buttons();
        assert_eq!(b.get(&Button::Y), Some(&Fire(Action::Exec("foo".into()))));
        assert_eq!(
            b.get(&Button::X),
            Some(&Fire(Action::ToggleKeyboard { mode: crate::osk::OskMode::Split, reflow: false }))
        );
        assert_eq!(
            b.get(&Button::A),
            Some(&Fire(Action::Dispatch("hl.dsp.window.close()".into())))
        );
        assert_eq!(c.button_descs.get(&Button::A).map(String::as_str), Some("Close window"));
        assert_eq!(
            b.get(&Button::BumperR1),
            Some(&Fire(Action::Workspace(crate::config::WorkspaceTarget::Relative(1))))
        );
        assert_eq!(b.get(&Button::BumperL1), Some(&Fire(Action::ToggleFullscreen)));
        assert_eq!(b.get(&Button::GripR4), Some(&Fire(Action::SetMode("desktop".into()))));
        assert_eq!(b.get(&Button::GripL4), Some(&Fire(Action::ClearMode)));
        assert_eq!(b.get(&Button::DpadUp), Some(&Hold(103.into())));
        assert_eq!(b.get(&Button::TriggerR2Full), Some(&Hold(272.into())));

        // `h.none` binds nothing, and a button is not where to say so.
        let e = load_str(r#"hyprpad.button("b", hyprpad.none())"#, "t.lua").unwrap_err();
        assert!(e.contains("a bare button needs an action"), "{e}");
        assert!(e.contains("'b'"), "should name the button: {e}");
    }

    #[test]
    fn an_osk_button_bound_to_a_non_key_action_is_an_error() {
        // The keyboard's table takes a key typed through it or one of its own
        // verbs, not a chord's actions — even though a bare button takes anything.
        let e = load_str(r#"hyprpad.osk_button("y", hyprpad.exec "foo")"#, "t.lua").unwrap_err();
        assert!(e.contains("h.osk_button values must be a key"), "{e}");
        assert!(e.contains("'y'"), "should name the button: {e}");
        let e = load_str(r#"hyprpad.osk_button("y", hyprpad.keyboard "split")"#, "t.lua")
            .unwrap_err();
        assert!(e.contains("must be a key"), "{e}");
    }

    #[test]
    fn a_fired_action_can_be_a_buttons_alternate_in_another_mode() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class:match("^steam_app_") ~= nil
            end)
            h.mode("desktop")
            h.button("b", h.key "backspace"):only_in("desktop")
            h.button("b", "Pause", h.exec "x"):only_in("game")
            "#,
        );
        let game = ModeState::new("game", vec![]);
        // The second binding is an alternate, guard and description intact...
        assert_eq!(c.button_alts.len(), 1);
        assert_eq!(c.button_alts[0].action, Fire(Action::Exec("x".into())));
        assert_eq!(c.button_alts[0].guard, Guard::OnlyIn(vec!["game".into()]));
        assert_eq!(c.button_alts[0].desc.as_deref(), Some("Pause"));
        // ...and it is what `b` does in the game, while the desktop keeps the key.
        assert_eq!(c.buttons_in(&game).get(&Button::B), Some(&Fire(Action::Exec("x".into()))));
        assert_eq!(c.buttons_in(&desktop()).get(&Button::B), Some(&Hold(14.into())));
    }

    #[test]
    fn h_mouse_binds_a_mouse_button_as_a_bare_button() {
        let c = load(
            r#"
            local h = hyprpad
            h.button("r2", h.mouse "left")
            h.button("l2", h.mouse "right")
            h.button("rpad_click", h.mouse "LMB")
            h.button("r3", "mouse middle")   -- the TOML grammar, as a plain string
            h.button("l3", h.key "btn_left") -- the evdev spelling through h.key
            "#,
        );
        assert_eq!(c.buttons().get(&Button::TriggerR2Full), Some(&Hold(272.into())));
        assert_eq!(c.buttons().get(&Button::TriggerL2Full), Some(&Hold(273.into())));
        assert_eq!(c.buttons().get(&Button::PadRightClick), Some(&Hold(272.into())));
        assert_eq!(c.buttons().get(&Button::R3), Some(&Hold(274.into())));
        assert_eq!(c.buttons().get(&Button::L3), Some(&Hold(272.into())));

        let e = load_str(r#"hyprpad.button("r2", hyprpad.mouse "side")"#, "t.lua").unwrap_err();
        assert!(e.contains("unknown mouse button 'side'"), "{e}");
        let e = load_str(r#"hyprpad.button("r2", hyprpad.mouse {})"#, "t.lua").unwrap_err();
        assert!(e.contains("h.mouse needs a button"), "{e}");
    }

    #[test]
    fn an_osk_button_cannot_be_a_mouse_button() {
        for action in [r#"hyprpad.mouse "left""#, r#"hyprpad.key "btn_right""#, r#""click 3""#] {
            let src = format!(r#"hyprpad.osk_button("y", {action})"#);
            let e = load_str(&src, "t.lua").unwrap_err();
            assert!(e.contains("mouse button makes no sense there"), "{e}");
            assert!(e.contains("h.button"), "should suggest h.button: {e}");
            assert!(e.contains("'y'"), "should name the button: {e}");
        }
    }

    #[test]
    fn a_mouse_binding_is_guarded_like_a_key_binding() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx)
              return ctx.focus.class:match("^steam_app_") ~= nil
            end)
            h.mode("desktop")
            h.button("r2", h.mouse "left"):only_in("desktop")
            h.button("l2", h.mouse "right"):not_in("game")
            h.button("dpad_up", h.key "up"):only_in("desktop")
            "#,
        );
        let game = ModeState::new("game", vec![]);
        assert_eq!(c.buttons_in(&desktop()).get(&Button::TriggerR2Full), Some(&Hold(272.into())));
        assert_eq!(c.buttons_in(&desktop()).get(&Button::TriggerL2Full), Some(&Hold(273.into())));
        assert_eq!(c.buttons_in(&desktop()).get(&Button::DpadUp), Some(&Hold(103.into())));
        // In the game the clicks are gone with the arrows: the map is what a
        // mode takes away, and a click is just another entry in it.
        assert!(c.buttons_in(&game).is_empty());
        assert_eq!(c.button_guards.get(&Button::TriggerR2Full), c.button_guards.get(&Button::DpadUp));
    }

    #[test]
    fn unknown_keys_and_bad_types_are_errors_naming_the_line() {
        let e = load_str("hyprpad.cursor { sesn = 0.06 }", "t.lua").unwrap_err();
        assert!(e.contains("unknown h.cursor key 'sesn'"), "{e}");
        assert!(e.contains("t.lua:1"), "error should name the config line: {e}");

        let e = load_str("hyprpad.daemon { own_lizard = 1 }", "t.lua").unwrap_err();
        assert!(e.contains("must be true or false"), "{e}");

        let e = load_str(r#"hyprpad.bind("guide+nope", hyprpad.exec "x")"#, "t.lua").unwrap_err();
        assert!(e.contains("unknown button 'nope'"), "{e}");
    }

    #[test]
    fn h_controller_off_is_an_action_on_a_chord_and_on_a_button() {
        let c = load(
            r#"
            hyprpad.bind("guide+quickaccess", "Controller off", hyprpad.controller_off())
            hyprpad.button("l4", hyprpad.controller_off())
            "#,
        );
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::QuickAccess)),
            Action::ControllerOff
        );
        assert_eq!(
            c.buttons_in(&desktop()).get(&Button::GripL4),
            Some(&crate::config::ButtonAction::Fire(Action::ControllerOff))
        );
        // The string spelling is the TOML one, so a config can be ported a line
        // at a time.
        let s = load(r#"hyprpad.bind("guide+l5", "controller_off")"#);
        assert_eq!(s.resolve(&GestureEvent::GuideChord(Button::GripL5)), Action::ControllerOff);
    }

    #[test]
    fn the_firmware_power_knobs_parse_off_and_a_number() {
        // Untouched by the file: nothing is written, which is the historical
        // settings frame.
        assert!(load(r#"hyprpad.daemon { own_lizard = true }"#).power_settings().is_empty());

        let c = load(
            r#"hyprpad.daemon {
                 steam_button_poweroff = "off",
                 sleep_inactivity_timeout = 600,
               }"#,
        );
        assert_eq!(
            c.power_settings(),
            crate::lizard::PowerSettings {
                steam_button_poweroff: Some(crate::lizard::POWER_SETTING_OFF),
                sleep_inactivity_timeout: Some(600),
            }
        );
        // An explicit 0 is written raw, so the "0 = never" reading is testable.
        assert_eq!(
            load(r#"hyprpad.daemon { steam_button_poweroff = 0 }"#)
                .power_settings()
                .steam_button_poweroff,
            Some(0)
        );
        // A string that is not "off", a fraction, and a value past the u16 the
        // wire carries are all load errors naming the key.
        for bad in [
            r#"hyprpad.daemon { steam_button_poweroff = "long" }"#,
            r#"hyprpad.daemon { steam_button_poweroff = 1.5 }"#,
            r#"hyprpad.daemon { steam_button_poweroff = 70000 }"#,
            r#"hyprpad.daemon { sleep_inactivity_timeout = -1 }"#,
        ] {
            let e = load_str(bad, "t.lua").unwrap_err();
            assert!(e.contains("poweroff") || e.contains("inactivity"), "{bad}: {e}");
        }
        // And the typo message lists the new keys.
        let e = load_str(r#"hyprpad.daemon { steam_button_power = "off" }"#, "t.lua").unwrap_err();
        assert!(e.contains("steam_button_poweroff"), "{e}");
    }

    #[test]
    fn the_rescan_knobs_parse_and_keep_their_defaults() {
        // Untouched by the file: the title path on, the sweep at half a second.
        let d = load(r#"hyprpad.daemon { own_lizard = true }"#);
        assert!(d.rescan_on_title_change());
        assert_eq!(d.process_rescan_ms(), 500);

        let c = load(
            r#"hyprpad.daemon {
                 rescan_on_title_change = false,
                 process_rescan_ms = 250,
               }"#,
        );
        assert!(!c.rescan_on_title_change());
        assert_eq!(c.process_rescan_ms(), 250);
        assert_eq!(load("hyprpad.daemon { process_rescan_ms = 0 }").process_rescan_ms(), 0);

        // Values a timer cannot mean, and a typo, are load errors.
        for bad in ["-1", "0.5"] {
            let e = load_str(&format!("hyprpad.daemon {{ process_rescan_ms = {bad} }}"), "t.lua")
                .unwrap_err();
            assert!(e.contains("whole number of milliseconds"), "{bad}: {e}");
        }
        let e = load_str("hyprpad.daemon { rescan_on_title = true }", "t.lua").unwrap_err();
        assert!(e.contains("rescan_on_title_change"), "the typo should list the real key: {e}");
    }

    /// The load-time gate on the periodic rescan: a config that never mentions
    /// `process_tree_has` must never be polled for changes in it.
    #[test]
    fn walks_process_tree_is_decided_from_the_source() {
        let walker = load(
            r#"
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus:process_tree_has("claude")
            end)
            "#,
        );
        assert!(walker.lua().unwrap().walks_process_tree());

        let title_only = load(
            r#"
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus.title:match("claude") ~= nil
            end)
            "#,
        );
        assert!(!title_only.lua().unwrap().walks_process_tree());
    }

    #[test]
    fn forgetting_the_process_tree_makes_the_next_predicate_re_walk() {
        // The cache is what makes a walk once-per-focus; `forget` is the single
        // lever both rescan paths pull, and this is the proof it re-reads.
        let c = load(
            r#"
            hyprpad.mode("has").when(function(ctx)
              return ctx.focus:process_tree_has("hyprpad-forget-probe")
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let rt = c.lua().unwrap();
        let rule = c.modes()[0].rule.unwrap();
        let focus = focused(Focus { pid: Some(std::process::id() as i32), ..Focus::default() });
        assert!(!rt.eval_predicate(rule, &focus), "nothing by that name yet");

        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg0("hyprpad-forget-probe")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        // Without forgetting, the cached walk still answers "no"...
        assert!(!rt.eval_predicate(rule, &focus), "the cache must not re-walk on its own");
        // ...and with it, the child is found (retried: `spawn` returns before
        // the child has exec'd).
        let mut found = false;
        for _ in 0..50 {
            rt.forget_process_tree();
            found = rt.eval_predicate(rule, &focus);
            if found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(found);

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_syntax_error_names_the_line_and_yields_no_config() {
        let e = load_str("hyprpad.bind(\n\nthis is not lua", "broken.lua").unwrap_err();
        assert!(e.contains("broken.lua:"), "{e}");
    }

    #[test]
    fn a_syntax_error_is_caught_before_a_single_line_runs() {
        // The HypXRland rule, and the reason last-good retention works: a file
        // whose *last* line is a syntax error must not have applied its first
        // lines. `into_function()` compiles the whole chunk first, so a compile
        // failure yields an `Err` and no `Config` at all — there is no partial
        // config for the caller to accidentally install.
        assert!(load_str(
            "hyprpad.daemon { own_lizard = true }\nthis is not lua",
            "half.lua"
        )
        .is_err());
        // ...and the same file with the bad line removed does apply.
        assert!(load(r#"hyprpad.daemon { own_lizard = true }"#).own_lizard());
    }

    #[test]
    fn load_file_reads_a_real_file_and_names_it_in_errors() {
        let dir = std::env::temp_dir().join(format!(
            "hyprpad-lua-load-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.lua");

        std::fs::write(&path, r#"hyprpad.bind("guide+r1", hyprpad.exec "good")"#).unwrap();
        let c = load_file(&path).expect("valid file loads");
        assert_eq!(
            c.resolve(&GestureEvent::GuideChord(Button::BumperR1)),
            Action::Exec("good".into())
        );

        std::fs::write(&path, "hyprpad.bind(").unwrap();
        let e = load_file(&path).unwrap_err();
        assert!(e.contains("config.lua"), "{e}");

        // A file that is not there at all is an error the caller can act on,
        // not a silent empty config.
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_file(&path).unwrap_err().contains("reading"));
    }

    #[test]
    fn a_runtime_error_mid_file_yields_no_config_at_all() {
        // Not a syntax error — the file compiles and then throws partway
        // through. The load still fails as a whole, so the caller keeps its
        // last-good config rather than running on half a file.
        let e = load_str(
            r#"
            hyprpad.bind("guide+r1", hyprpad.exec "first")
            error("something went wrong")
            hyprpad.bind("guide+l1", hyprpad.exec "second")
            "#,
            "boom.lua",
        )
        .unwrap_err();
        assert!(e.contains("boom.lua:"), "{e}");
        assert!(e.contains("something went wrong"), "{e}");
    }

    #[test]
    fn guards_attach_to_bindings() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.mode("game")
            h.button("dpad_up", h.key "up"):only_in("desktop")
            h.button("a", h.key "enter"):not_in("game")
            h.bind("guide+r1", h.workspace "+1")
            h.cursor { sens = 0.06, only_in = { "desktop" } }
            h.scroll { only_in = { "desktop" } }
            "#,
        );
        let game = ModeState::new("game", vec![]);
        assert_eq!(c.buttons_in(&desktop()).len(), 2);
        assert!(c.buttons_in(&game).is_empty());
        assert!(c.cursor_enabled_in(&desktop()) && !c.cursor_enabled_in(&game));
        assert!(c.scroll_enabled_in(&desktop()) && !c.scroll_enabled_in(&game));
        // The unguarded guide chord survives every mode — the escape hatch.
        assert_eq!(
            c.resolve_in(&GestureEvent::GuideChord(Button::BumperR1), &game),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
    }

    #[test]
    fn guide_in_makes_the_pad_a_mouse_under_the_guide_in_the_listed_modes() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.cursor { sens = 0.06, only_in = { "desktop" }, guide_in = { "game" } }
            "#,
        );
        let game = ModeState::new("game", vec![]);
        // Two independent guards on one pad: with the guide up it is the
        // desktop's cursor and the game's pad; with the guide held it is a
        // mouse in the game and nothing on the desktop (where the guide layer
        // keeps taking it away).
        assert!(c.cursor_enabled_in(&desktop()) && !c.cursor_enabled_in(&game));
        assert!(c.cursor_guide_enabled_in(&game) && !c.cursor_guide_enabled_in(&desktop()));
        assert_eq!(c.cursor().sens, 0.06, "the knobs still parse beside it");

        // A bare string is a list of one, as for only_in.
        let one = load(r#"hyprpad.mode("game") hyprpad.cursor { guide_in = "game" }"#);
        assert!(one.cursor_guide_enabled_in(&game));

        // Left out: off everywhere — today's behaviour.
        let off = load(r#"hyprpad.mode("game") hyprpad.cursor { only_in = { "desktop" } }"#);
        assert!(!off.cursor_guide_enabled_in(&game) && !off.cursor_guide_enabled_in(&desktop()));

        // A mode nobody declared is refused at load, like any other guard —
        // a silent typo here would leave the guide-held pad dead.
        let e = load_str(r#"hyprpad.cursor { guide_in = { "gmae" } }"#, "t.lua").unwrap_err();
        assert!(e.contains("guide_in") && e.contains("'gmae'"), "{e}");
        // And an empty list is a typo too: leave the key out for off.
        let e = load_str(r#"hyprpad.cursor { guide_in = {} }"#, "t.lua").unwrap_err();
        assert!(e.contains("guide_in needs at least one mode name"), "{e}");
    }

    #[test]
    fn h_scrub_is_the_opt_in_for_the_caret_wheel_and_is_guarded_like_the_cursor() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true })
            h.mode("desktop")
            h.scrub {
              detent_deg = 20,
              min_radius = 0.4,
              fast_deg_per_s = 300,
              fast_min_detents = 3,
              slow_deg_per_s = 150,
              word_tier = false,
              select = "l4",
              only_in = { "desktop" },
            }
            "#,
        );
        let game = ModeState::new("game", vec![]);
        assert!(c.scrub().enabled);
        assert_eq!(c.scrub().detent_deg, 20.0);
        assert_eq!(c.scrub().min_radius, 0.4);
        assert_eq!(c.scrub().fast_deg_per_s, 300.0);
        assert_eq!(c.scrub().fast_min_detents, 3);
        assert_eq!(c.scrub().slow_deg_per_s, 150.0);
        assert!(!c.scrub().word_tier);
        assert_eq!(c.scrub().select, crate::report::Button::GripL4);
        assert!(c.scrub_enabled_in(&desktop()) && !c.scrub_enabled_in(&game));

        // Writing the section IS the opt-in: an empty table is the whole thing
        // switched on with every default.
        let bare = load(r#"hyprpad.scrub {}"#);
        assert!(bare.scrub().enabled);
        assert_eq!(*bare.scrub(), crate::config::ScrubConfig { enabled: true, ..Default::default() });

        // Left out entirely: off, which is what every config that predates the
        // scrub gets.
        let none = load(r#"hyprpad.cursor { sens = 0.06 }"#);
        assert!(!none.scrub().enabled && !none.scrub_enabled_in(&desktop()));

        // And `enabled = false` inside the block keeps the tuning but not the
        // wheel.
        let off = load(r#"hyprpad.scrub { enabled = false, detent_deg = 22 }"#);
        assert!(!off.scrub().enabled);
        assert_eq!(off.scrub().detent_deg, 22.0);

        // The chained guard form works too, like every other section handle.
        let chained = load(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.scrub { detent_deg = 15 }:only_in("desktop")
            "#,
        );
        assert!(chained.scrub_enabled_in(&desktop()));

        // Typos are load errors that name the key and the section.
        let e = load_str(r#"hyprpad.scrub { detent = 15 }"#, "t.lua").unwrap_err();
        assert!(e.contains("h.scrub") && e.contains("detent"), "{e}");
        let e = load_str(r#"hyprpad.scrub { select = "nope" }"#, "t.lua").unwrap_err();
        assert!(e.contains("nope"), "{e}");
        // A mode nobody declared is refused, as it is for the cursor's guard —
        // a silent typo would leave the wheel dead everywhere.
        let e = load_str(r#"hyprpad.scrub { only_in = { "dekstop" } }"#, "t.lua").unwrap_err();
        assert!(e.contains("dekstop"), "{e}");
    }

    #[test]
    fn a_when_guard_reads_its_cached_predicate_result() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.bind("guide+r1", h.workspace "+1"):when(function(ctx)
              return ctx.focus.class == "foot"
            end)
            "#,
        );
        let ev = GestureEvent::GuideChord(Button::BumperR1);
        assert_eq!(c.resolve_in(&ev, &ModeState::new("desktop", vec![true])), Action::Workspace(WorkspaceTarget::Relative(1)));
        assert_eq!(c.resolve_in(&ev, &ModeState::new("desktop", vec![false])), Action::None);
        // A predicate whose result never made it into the snapshot is "no".
        assert_eq!(c.resolve_in(&ev, &ModeState::new("desktop", vec![])), Action::None);
    }

    #[test]
    fn a_guard_naming_an_undeclared_mode_is_refused() {
        let e = load_str(
            r#"
            hyprpad.mode("desktop")
            hyprpad.button("a", hyprpad.key "enter"):only_in("desktopp")
            "#,
            "t.lua",
        )
        .unwrap_err();
        assert!(e.contains("desktopp"), "{e}");
        assert!(e.contains("no h.mode"), "{e}");
    }

    #[test]
    fn modes_keep_definition_order_and_carry_their_forward_flag() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game", { forward = true }).when(function(ctx) return true end)
            h.mode("claude").when(function(ctx) return false end)
            h.mode("desktop")
            h.default_mode "desktop"
            "#,
        );
        let names: Vec<&str> = c.modes().iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["game", "claude", "desktop"]);
        assert!(c.modes()[0].forward && !c.modes()[1].forward);
        assert!(c.modes()[0].rule.is_some() && c.modes()[2].rule.is_none());
        assert_eq!(c.default_mode(), "desktop");
    }

    #[test]
    fn an_undeclared_default_mode_is_declared_implicitly() {
        let c = load(r#"hyprpad.default_mode "desktop""#);
        assert_eq!(c.default_mode(), "desktop");
        assert_eq!(c.modes().len(), 1);
        assert_eq!(c.modes()[0].name, "desktop");
    }

    #[test]
    fn declaring_a_mode_twice_is_an_error() {
        let e = load_str(
            "hyprpad.mode(\"game\")\nhyprpad.mode(\"game\")",
            "t.lua",
        )
        .unwrap_err();
        assert!(e.contains("declared twice"), "{e}");
    }

    #[test]
    fn predicates_see_the_focus_context() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("game").when(function(ctx)
              return ctx.focus.class:match("^steam_app_") ~= nil
            end)
            h.mode("full").when(function(ctx) return ctx.focus.fullscreen end)
            h.mode("titled").when(function(ctx) return ctx.focus.title == "hyprpad" end)
            h.mode("desktop")
            "#,
        );
        let rt = c.lua().expect("lua runtime");
        let steam = focused(Focus { class: "steam_app_413080".into(), ..Focus::default() });
        assert!(rt.eval_predicate(c.modes()[0].rule.unwrap(), &steam));
        assert!(!rt.eval_predicate(c.modes()[1].rule.unwrap(), &steam));

        let fs = focused(Focus { class: "mpv".into(), fullscreen: true, ..Focus::default() });
        assert!(rt.eval_predicate(c.modes()[1].rule.unwrap(), &fs));

        let titled = focused(Focus { title: "hyprpad".into(), ..Focus::default() });
        assert!(rt.eval_predicate(c.modes()[2].rule.unwrap(), &titled));
    }

    /// `ctx.layers` is the overlay half of the context, and it has to be usable
    /// as *both* a set (`:has`) and a list (`ipairs`, `#`, `:list()`) — a rule
    /// that wants to know whether the sheet is up, and one that wants to see
    /// what is on screen, are both reasonable things to write.
    #[test]
    fn predicates_see_the_open_overlays() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("sheet").when(function(ctx)
              return ctx.layers:has("hyprpad-cheatsheet")
            end)
            h.mode("dot").when(function(ctx)
              return ctx.layers.has("hyprpad-cheatsheet")  -- the non-method form
            end)
            h.mode("counted").when(function(ctx) return #ctx.layers == 2 end)
            h.mode("listed").when(function(ctx)
              return table.concat(ctx.layers:list(), ",") == "omarchy-bar,zzz"
            end)
            h.mode("iterated").when(function(ctx)
              for _, ns in ipairs(ctx.layers) do
                if ns:match("^omarchy%-") then return true end
              end
              return false
            end)
            h.mode("desktop")
            "#,
        );
        let rt = c.lua().expect("lua runtime");
        let rule = |i: usize| c.modes()[i].rule.unwrap();

        let sheet = showing(&["hyprpad-cheatsheet"]);
        assert!(rt.eval_predicate(rule(0), &sheet), "method call form");
        assert!(rt.eval_predicate(rule(1), &sheet), "dot call form");

        // Nothing open: every overlay rule is simply false, not an error.
        let bare = Context::default();
        assert!(!rt.eval_predicate(rule(0), &bare));
        assert!(!rt.eval_predicate(rule(4), &bare));

        // A list, in sorted order, however the namespaces arrived.
        let two = showing(&["zzz", "omarchy-bar"]);
        assert!(rt.eval_predicate(rule(2), &two), "# counts the array part");
        assert!(rt.eval_predicate(rule(3), &two), ":list() is sorted");
        assert!(rt.eval_predicate(rule(4), &two), "ipairs walks it");
    }

    /// `ctx.locked` is the third half of the context (the lock screen is
    /// neither a window nor a layer), and it has to work in both places a
    /// predicate can live: a mode's selection rule, and a binding's `:when`
    /// guard. The guard form is the one the owner asked for — "keep this button
    /// off the password field" without declaring a mode for it.
    #[test]
    fn predicates_see_the_session_lock() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("locked").when(function(ctx) return ctx.locked end)
            h.mode("desktop")
            h.button("a", h.key "enter"):when(function(ctx) return not ctx.locked end)
            h.bind("guide+b", h.key "escape"):when(function(ctx) return not ctx.locked end)
            "#,
        );
        let rt = c.lua().expect("lua runtime");
        let rule = c.modes()[0].rule.unwrap();
        assert!(rt.eval_predicate(rule, &locked_session()), "locked");
        assert!(!rt.eval_predicate(rule, &Context::default()), "unlocked");
        // False, not nil: a rule that spells `ctx.locked == false` must work as
        // readily as one that spells `not ctx.locked`.
        let c2 = load(
            r#"
            local h = hyprpad
            h.mode("unlocked").when(function(ctx) return ctx.locked == false end)
            h.mode("desktop")
            "#,
        );
        let rt2 = c2.lua().expect("lua runtime");
        assert!(rt2.eval_predicate(c2.modes()[0].rule.unwrap(), &Context::default()));
        assert!(!rt2.eval_predicate(c2.modes()[0].rule.unwrap(), &locked_session()));

        // And the `:when` guards. A guard is evaluated from the predicate
        // results a re-resolve already collected, so this is the state the
        // engine would build in each context.
        let slots = c.predicate_slots();
        let results = |cx: &Context| {
            (0..slots).map(|i| rt.eval_predicate(i, cx)).collect::<Vec<_>>()
        };
        let unlocked = ModeState::new("desktop", results(&Context::default()));
        let locked = ModeState::new("locked", results(&locked_session()));
        assert_eq!(
            c.buttons_in(&unlocked).get(&Button::A),
            Some(&Hold(28.into())),
            "A is Enter with the session unlocked"
        );
        assert!(c.buttons_in(&locked).is_empty(), "and nothing at all while locked");
        assert!(matches!(
            c.resolve_in(&GestureEvent::GuideChord(Button::B), &locked),
            Action::None
        ));
    }

    /// The lock poll is opt-in, exactly as the process-tree sweep is: a config
    /// that never says `locked` must never make the daemon ask the compositor
    /// about it.
    #[test]
    fn only_a_config_that_asks_about_the_lock_is_polled_for_it() {
        let asks = load(
            r#"
            local h = hyprpad
            h.mode("locked").when(function(ctx) return ctx.locked end)
            h.mode("desktop")
            "#,
        );
        assert!(asks.lua().unwrap().reads_locked());
        assert!(asks.watches_lock());

        let does_not = load(
            r#"
            local h = hyprpad
            h.mode("game").when(function(ctx) return ctx.focus.class == "steam" end)
            h.mode("desktop")
            "#,
        );
        assert!(!does_not.lua().unwrap().reads_locked());
        assert!(!does_not.watches_lock());

        // Modes are the other half of the gate: a file that mentions the lock
        // but declares nothing cannot resolve on it either. (`h.default_mode`
        // declares one implicitly, so this really is the no-modes case only for
        // a config that declares none at all — which is every `config.toml`.)
        let toml = Config::from_toml_str("[buttons]\na = \"key enter\"\n").expect("toml");
        assert!(!toml.watches_lock(), "a TOML config has no predicates to ask with");
    }

    /// The same button, bound once per mode. A single binding per button was
    /// enough while a button meant one thing everywhere; the cheat sheet is the
    /// case that breaks it — `b` is Backspace on the desktop and Escape under
    /// the sheet — and the second binding must not quietly replace the first.
    #[test]
    fn a_button_can_mean_something_else_in_another_mode() {
        let c = load(
            r#"
            local h = hyprpad
            h.mode("cheatsheet").when(function(ctx)
              return ctx.layers:has("hyprpad-cheatsheet")
            end)
            h.mode("desktop")
            h.button("b", h.key "backspace"):only_in("desktop")
            h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
            "#,
        );
        let sheet = ModeState::new("cheatsheet", vec![]);
        assert_eq!(c.buttons_in(&desktop()).get(&Button::B), Some(&Hold(14.into())), "Backspace");
        assert_eq!(c.buttons_in(&sheet).get(&Button::B), Some(&Hold(1.into())), "Escape");
        assert_eq!(c.buttons_in(&sheet).len(), 1, "and nothing else is live there");

        // The alternate is a binding in its own right, description and all, so
        // `hyprpad bindings` can print it.
        assert_eq!(c.button_alts.len(), 1);
        assert_eq!(c.button_alts[0].desc.as_deref(), Some("Close cheat sheet"));
        // The base map — the unguarded view the no-modes path uses — keeps the
        // binding that was declared first.
        assert_eq!(c.buttons().get(&Button::B), Some(&Hold(14.into())));
    }

    #[test]
    fn an_unguarded_re_binding_loses_to_the_one_it_was_written_beside() {
        // Nothing tells these two apart, so the second can never be live. It is
        // a config bug; the loader warns and keeps the first.
        let c = load(
            r#"
            local h = hyprpad
            h.mode("desktop")
            h.button("b", h.key "backspace")
            h.button("b", h.key "escape")
            "#,
        );
        assert_eq!(c.buttons_in(&desktop()).get(&Button::B), Some(&Hold(14.into())));
    }

    #[test]
    fn a_throwing_predicate_is_no_match_not_a_crash() {
        let c = load(
            r#"
            hyprpad.mode("boom").when(function(ctx) error("nope") end)
            hyprpad.mode("desktop")
            "#,
        );
        let rt = c.lua().unwrap();
        assert!(!rt.eval_predicate(c.modes()[0].rule.unwrap(), &Context::default()));
        // And again — the failure is logged once but stays non-fatal.
        assert!(!rt.eval_predicate(c.modes()[0].rule.unwrap(), &Context::default()));
    }

    #[test]
    fn the_watchdog_cuts_a_runaway_predicate() {
        let c = load(
            r#"
            hyprpad.mode("spin").when(function(ctx) while true do end end)
            hyprpad.mode("desktop")
            "#,
        );
        let rt = c.lua().unwrap();
        let t0 = Instant::now();
        assert!(!rt.eval_predicate(c.modes()[0].rule.unwrap(), &Context::default()));
        let took = t0.elapsed();
        assert!(
            took < PREDICATE_TIMEOUT * 5,
            "the watchdog should have cut it near {PREDICATE_TIMEOUT:?}, took {took:?}"
        );
        assert!(took >= PREDICATE_TIMEOUT / 2, "suspiciously fast: {took:?}");
    }

    #[test]
    fn the_watchdog_cuts_a_runaway_config_load() {
        let t0 = Instant::now();
        let e = load_str("while true do end", "spin.lua").unwrap_err();
        let took = t0.elapsed();
        assert!(e.contains("timed out"), "{e}");
        assert!(took < LOAD_TIMEOUT * 3, "took {took:?}");
    }

    #[test]
    fn a_finished_predicate_does_not_leave_the_watchdog_armed() {
        // The deadline must be restored after each guarded call, or the second
        // (slower) predicate would inherit the first one's spent budget.
        let c = load(
            r#"
            hyprpad.mode("a").when(function(ctx)
              local n = 0
              for i = 1, 200000 do n = n + i end
              return n > 0
            end)
            hyprpad.mode("desktop")
            "#,
        );
        let rt = c.lua().unwrap();
        for _ in 0..5 {
            assert!(rt.eval_predicate(c.modes()[0].rule.unwrap(), &Context::default()));
        }
    }

    /// A spawned test process tree, killed and reaped however the test leaves
    /// the stack — a failing assert must not leak a `sleep` into the session.
    ///
    /// The child leads its own process group (`process_group(0)`, so its pgid
    /// is its pid), because `Child::kill` signals only the process itself and
    /// would orphan the descendants the shell started. One negative-pid signal
    /// takes the whole tree.
    struct ProcessTree(std::process::Child);

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            // SAFETY: a plain `kill(2)`; the pgid is this child's own, created
            // by `process_group(0)` at spawn, so nothing else can be in it.
            unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Poll `pred` on a 10 ms step until it holds or `budget` runs out.
    ///
    /// `/proc` is not a snapshot, so a single look is a coin flip in two ways
    /// that have nothing to do with what is being tested:
    ///
    /// * `Command::spawn` returns once the child's `execve` has reached the
    ///   point that closes the CLOEXEC pipe std waits on. The kernel sets
    ///   `comm` before that but publishes `mm->arg_start` after it, so a walk
    ///   landing in the window reads `/proc/<pid>/comm` as the new name and
    ///   `/proc/<pid>/cmdline` as *empty* — anything that only appears in the
    ///   command line is invisible. Measured at ~90% of spawns on an idle box.
    /// * a grandchild is started by the shell, with no handshake with us at
    ///   all, and `/proc/<pid>/task` listings are not atomic against libtest
    ///   starting and joining threads around us.
    ///
    /// Both windows are transient, so wait for them rather than race them.
    fn poll_until(budget: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + budget;
        loop {
            if pred() {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn process_tree_has_finds_a_descendant_process() {
        // Spawn a process tree under this test process and look for it by name
        // through the same /proc walk a `ctx.focus:process_tree_has(...)`
        // predicate runs.
        //
        // The tree is three deep — test -> sh -> sh -> sleep — so this covers
        // the recursive descent, not just "is it a direct child". Two details
        // keep the needle honest:
        //
        // * the script arrives on the outer shell's *stdin*, not in its argv,
        //   so that shell's own `/proc/<pid>/cmdline` is bare `/bin/sh` and
        //   cannot satisfy the predicate by accident. Only the tagged inner
        //   shell can.
        // * the tag carries our pid, so the sibling tests that spawn `sleep`
        //   in parallel (and any stray one on the machine) cannot satisfy it
        //   either.
        //
        // Readiness is signalled by `/bin/echo` *inside* the tagged shell: an
        // external command that runs only after that shell has exec'd, and
        // that flushes by exiting, so reading the token proves the tag is
        // already published in /proc rather than merely forked.
        //
        // The tagged shell backgrounds its `sleep` and blocks in `wait` rather
        // than running it last, so that it stays a shell. A shell whose final
        // command is a simple one exec's it in place — `sh -c '... ; sleep
        // 300'` *becomes* `sleep 300` and drops the tag from its command line,
        // which made an earlier draft of this test pass only when the lookup
        // won the race against that exec.
        let tag = format!("hyprpad-descendant-probe-{}", std::process::id());
        let script = format!("/bin/sh -c '/bin/echo ready; sleep 300 & wait' {tag} &\nwait\n");

        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::process::CommandExt;
        let mut spawned = std::process::Command::new("/bin/sh")
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn /bin/sh");
        spawned
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .expect("feed the script");
        let out = spawned.stdout.take().expect("piped stdout");
        let _child = ProcessTree(spawned);
        let mut ready = String::new();
        BufReader::new(out).read_line(&mut ready).expect("read the ready token");
        assert_eq!(ready.trim(), "ready", "the probe shell never came up");

        let me = std::process::id() as i32;
        let c = load(&format!(
            r#"
            hyprpad.mode("has").when(function(ctx)
              return ctx.focus:process_tree_has("{tag}")
            end)
            hyprpad.mode("dot").when(function(ctx)
              return ctx.focus.process_tree_has("{tag}")
            end)
            hyprpad.mode("missing").when(function(ctx)
              return ctx.focus:process_tree_has("definitely-not-a-real-process-name")
            end)
            hyprpad.mode("desktop")
            "#
        ));
        let rt = c.lua().unwrap();
        let focus = focused(Focus { pid: Some(me), ..Focus::default() });

        // One bounded poll to let the tree settle, re-walking each time (the
        // cache would otherwise pin the first, negative, answer forever)...
        assert!(
            poll_until(Duration::from_secs(2), || {
                rt.forget_process_tree();
                rt.eval_predicate(c.modes()[0].rule.unwrap(), &focus)
            }),
            "method call form: {tag} never appeared in the walk within 2s; \
             the tree under {me} was {:#?}",
            walk_process_tree(me)
        );
        // The match must have been reached by *descending*: no direct child of
        // ours carries the tag, so a walk that only looked one level down —
        // which is all the old version of this test would have caught — fails
        // here rather than passing quietly.
        let direct: Vec<String> =
            process_children(me).into_iter().filter_map(process_description).collect();
        assert!(
            direct.iter().all(|d| !d.contains(&tag)),
            "the tag should live in a grandchild, not a direct child; ours are {direct:#?}"
        );

        // ...after which the walk is warm and settled, so the rest are strict
        // single checks against that same walk.
        assert!(rt.eval_predicate(c.modes()[1].rule.unwrap(), &focus), "dot call form");
        assert!(!rt.eval_predicate(c.modes()[2].rule.unwrap(), &focus));

        // No pid (focus lost to the desktop) is simply "no match", not an error.
        assert!(!rt.eval_predicate(c.modes()[0].rule.unwrap(), &Context::default()));

        // `_child` drops here, killing the whole probe group on every path.
    }

    #[test]
    fn the_process_walk_is_bounded_and_survives_a_dead_pid() {
        // A pid that is gone must yield an empty walk, never an error or a hang.
        let mut cache = ProcCache::default();
        assert!(!cache.tree_has(i32::MAX, "anything"));
        // And a real one finds at least itself.
        let mut cache = ProcCache::default();
        assert!(cache.tree_has(std::process::id() as i32, "hyprpad"));
    }

    #[test]
    fn the_ported_sample_config_matches_the_toml_it_replaces() {
        // `config/hyprpad.lua` is the shipped translation of the owner's live
        // `config.toml`; the two must agree binding for binding.
        let lua_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/hyprpad.lua"
        ))
        .expect("config/hyprpad.lua");
        let lua = load_str(&lua_src, "config/hyprpad.lua").expect("sample config should load");
        let toml = Config::from_toml_str(SAMPLE_TOML).expect("sample toml");

        assert_eq!(lua.own_lizard(), toml.own_lizard());
        // The firmware power knobs are `[daemon]` settings like `own_lizard`,
        // and both front-ends can spell them: commented out in the sample, so
        // this pins "neither file writes setting 25 or 50 by default".
        assert_eq!(lua.power_settings(), toml.power_settings());
        assert_eq!(lua.cursor(), toml.cursor());
        // `guide_in` is a guard, not a `CursorConfig` field, so `cursor()`
        // above would not notice it drifting. Both front-ends can spell it.
        let in_game = crate::config::ModeState::new("game", Vec::new());
        assert_eq!(
            lua.cursor_guide_enabled_in(&in_game),
            toml.cursor_guide_enabled_in(&in_game),
            "the guide-held cursor differs between config.lua and config.toml"
        );
        assert_eq!(lua.scroll(), toml.scroll());
        assert_eq!(lua.haptics(), toml.haptics());
        // A TOML config has no modes, so a Lua binding guarded INTO one has no
        // TOML counterpart to compare against: the cheat sheet's own Left/Right
        // paging exists only under `cheatsheet`, and R1's Tab only under
        // `omarchy-ui` — modes a TOML user never enters. What must still agree
        // binding for binding is the pad a TOML user actually gets — the
        // default mode.
        //
        // Guide CHORDS are the exception to that exception: `resolve` is
        // guard-blind (`resolve_in` is the one that applies a guard), so a
        // chord guarded into a mode — `guide+rpad_click`, live only in `game`
        // — still needs its row in the fixture below.
        let st = crate::config::ModeState::new(lua.default_mode(), Vec::new());
        assert_eq!(&lua.buttons_in(&st), toml.buttons());
        assert_eq!(lua.osk_buttons(), toml.osk_buttons());
        for ev in every_bindable_gesture() {
            assert_eq!(
                lua.resolve(&ev),
                toml.resolve(&ev),
                "binding for {ev:?} differs between config.lua and config.toml"
            );
        }
    }

    /// Every gesture the two configs could bind, so the comparison above cannot
    /// miss one by only checking the bindings it happens to know about.
    fn every_bindable_gesture() -> Vec<GestureEvent> {
        use crate::gesture::{Stick, StickDir};
        let buttons = [
            Button::A, Button::B, Button::X, Button::Y,
            Button::BumperR1, Button::BumperL1, Button::TriggerR2Full, Button::TriggerL2Full,
            Button::R3, Button::L3, Button::GripR4, Button::GripR5, Button::GripL4, Button::GripL5,
            Button::DpadUp, Button::DpadDown, Button::DpadLeft, Button::DpadRight,
            Button::Menu, Button::View, Button::QuickAccess,
            Button::PadRightClick, Button::PadLeftClick,
        ];
        let mut out: Vec<GestureEvent> = buttons.iter().map(|b| GestureEvent::GuideChord(*b)).collect();
        for stick in [Stick::Left, Stick::Right] {
            for dir in [StickDir::Up, StickDir::Down, StickDir::Left, StickDir::Right] {
                out.push(GestureEvent::GuideStickFlick { stick, dir });
            }
        }
        out.push(GestureEvent::GuideLeave { was_chorded: false });
        out.push(GestureEvent::GuideHold);
        out
    }

    /// The owner's live `config.toml`, plus the cheat-sheet chord, as the
    /// reference the shipped `config/hyprpad.lua` is checked against. Keeping
    /// the two in step is the point: a binding that only one front-end can
    /// spell is a binding a TOML user silently loses.
    const SAMPLE_TOML: &str = r#"
[daemon]
own_lizard = true

[cursor]
sens = 0.06
one_euro_min_cutoff = 0.3
one_euro_beta = 1.0
one_euro_d_cutoff = 1.0
hysteresis = 0.0008
guide_in = ["game"]

[haptics]
cursor_spacing_px = 96

[scroll]
mode = "circular"
sensitivity = 1.0
circular_step_degrees = 15.0

[buttons]
dpad_up = "key up"
dpad_down = "key down"
dpad_left = "key left"
dpad_right = "key right"
a = "key enter"
b = "key backspace"
rpad_click = "mouse left"
r2 = "mouse left"
l2 = "mouse right"

[osk_buttons]
y = "key space"
x = "key backspace"
l2 = "osk shift"

[bindings]
"guide+r1" = "workspace +1"
"guide+l1" = "workspace -1"
"guide+stick_right" = "workspace +1"
"guide+stick_left"  = "workspace -1"
"guide+x" = "workspace emptyn"
"guide+stick_down" = "workspace previous"
"guide+dpad_down" = "movetoworkspace emptyn"
"guide+dpad_up" = "exec omarchy-shell -q hyprpad.status navEnter"
"guide+a" = "exec voxtype record toggle"
"guide+b" = "dispatch hl.dsp.window.close()"
"guide+r5" = "exec playerctl play-pause"
"guide+r4" = "exec omarchy screenshot fullscreen"
"guide+menu" = "exec omarchy-menu"
"guide+l2" = "dispatch hl.dsp.group.prev()"
"guide+r2" = "dispatch hl.dsp.group.next()"
"guide+y" = "keyboard split"
"guide+rpad_click" = "mouse left"
"guide+view" = "exec hyprpad-cheatsheet toggle"
"guide+quickaccess" = "controller_off"
"#;
}
