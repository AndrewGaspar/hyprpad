-- hyprpad daemon configuration.
--
-- Drop this at ~/.config/hyprpad/config.lua. When that file exists it is used
-- instead of config.toml; the TOML front-end stays supported, so deleting this
-- file falls straight back to it (docs/research/lua-config.md, option D2).
--
-- NOTE this is the *daemon's* config. `config/hyprpad.lua` in this repo is the
-- unrelated compositor-side Hyprland config (`~/.config/hypr/hyprpad.lua`).
--
-- A config.lua is a SCRIPT, re-run from the top on every `hyprpad reload` —
-- the same contract as the Hyprland Lua config. Keep it declarative: loops and
-- helpers are fine, side effects at file scope are not.
--
-- Everything below the bindings is the modality layer (docs/13). Its rules and
-- guards are Lua predicates that run ONLY on a context change — a focus change,
-- a fullscreen change, an overlay opening or closing, a manual override, a
-- reload — never per input frame.
--
-- A rule sees `ctx.focus` (the focused window: `.class`, `.title`, `.pid`,
-- `.fullscreen`, `:process_tree_has("claude")`) and `ctx.layers` (the
-- layer-shell overlays on screen: `:has("hyprpad-cheatsheet")`, `:list()`,
-- and `ipairs`-able). Overlays are how hyprpad's own windowless UI — the cheat
-- sheet, the on-screen keyboard — becomes something a rule can select on.

local h = hyprpad

-- ---------------------------------------------------------------------------
-- Modes: named contexts, evaluated in definition order, first match wins.
--
-- A mode carries nothing but its name, its rule, and whether it forwards raw
-- input to the game. What is LIVE in a mode is decided per binding, below.
-- ---------------------------------------------------------------------------

-- The cheat sheet is modal while it's up: B closes it (it listens for Escape).
--
-- FIRST, above even `game`: the sheet is an overlay drawn over whatever is
-- focused, including a game, and while it is up it owns the keyboard. Layer
-- surfaces do not raise an `activewindow` event, so `ctx.layers` is the only
-- thing that can see it — that is what makes this a mode rather than a special
-- case wired into the daemon.
h.mode("cheatsheet").when(function(ctx) return ctx.layers:has("hyprpad-cheatsheet") end)

-- Game / Steam Big Picture. Steam launches native and Proton titles as
-- `steam_app_<id>`; gamescope and Big Picture (`steamwebhelper`) are matched
-- too, which is what fixes the double-input seen in Big Picture.
--
-- Deliberately NOT triggered by fullscreen: a fullscreen video is not a game.
h.mode("game", { forward = true }).when(function(ctx)
  local c = ctx.focus.class:lower()
  return c:match("^steam_app_") ~= nil
      or c:match("^steam_proton") ~= nil
      or c:match("^gamescope") ~= nil
      or c == "steam"
      or c == "steamwebhelper"
end)

-- A terminal running Claude Code. Same window class as any other terminal, so
-- the rule has to look INSIDE the window: `process_tree_has` walks the focused
-- window's /proc descendants (once per focused pid, cached). Finnicky by
-- design — starting `claude` in an already-focused terminal needs a refocus to
-- be noticed. Behaviour TBD; the mode is declared but nothing is guarded into
-- it yet, so it currently behaves exactly like the desktop.
--
-- Uncomment to try it, then guard bindings with `:only_in("claude")`:
-- h.mode("claude").when(function(ctx)
--   return ctx.focus:process_tree_has("claude")
-- end)

-- Ordinary desktop use. No rule: it is the fallback.
h.mode("desktop")
h.default_mode "desktop"

-- ---------------------------------------------------------------------------
-- Daemon
-- ---------------------------------------------------------------------------

h.daemon { own_lizard = true }

-- ---------------------------------------------------------------------------
-- Feel
-- ---------------------------------------------------------------------------

-- One Euro Filter + hysteresis tuning. Lower min_cutoff = more smoothing when
-- slow (easier to land a target); higher beta = less lag when moving fast;
-- hysteresis = sticky dead-band around a settled finger.
--
-- `only_in` makes the cursor a guarded binding like any other: in game mode the
-- right pad belongs to the game, not to the desktop pointer.
h.cursor {
  sens = 0.06,
  one_euro_min_cutoff = 0.3,
  one_euro_beta = 1.0,
  one_euro_d_cutoff = 1.0,
  hysteresis = 0.0008,
  only_in = { "desktop" },
}

h.scroll {
  mode = "circular",
  sensitivity = 1.0,
  circular_step_degrees = 15.0,
  only_in = { "desktop" },
}

h.haptics { cursor_spacing_px = 96 } -- sparser cursor texture (default 64)

-- ---------------------------------------------------------------------------
-- Bare buttons (no guide modifier) -> keys, on the desktop layer only.
--
-- Already suppressed while the OSK is up or guide is held; `:only_in("desktop")`
-- is what hands them to the game under a game-classed window.
-- ---------------------------------------------------------------------------

h.button("dpad_up",    h.key "up"):only_in("desktop")
h.button("dpad_down",  h.key "down"):only_in("desktop")
h.button("dpad_left",  h.key "left"):only_in("desktop")
h.button("dpad_right", h.key "right"):only_in("desktop")
h.button("a", h.key "enter"):only_in("desktop")     -- A = Enter/confirm
h.button("b", h.key "backspace"):only_in("desktop") -- B = Backspace

-- The same button, a different meaning in a different mode. The cheat sheet has
-- keyboard focus and closes on Escape, so B dismisses it — and the two never
-- collide, because a mode is exclusive: under the sheet we are in `cheatsheet`,
-- never in `desktop`.
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")

-- While the on-screen keyboard is up, these tap keys THROUGH the OSK
-- (Deck-style helpers) so you never hunt for them with a cursor. Unguarded on
-- purpose: guide+Y is meant to raise the keyboard over a game too.
h.osk_button("y", h.key "space")
h.osk_button("x", h.key "backspace")

-- ---------------------------------------------------------------------------
-- Guide chords.
--
-- Unguarded, so they survive EVERY mode: the guide layer is hyprpad's escape
-- hatch and must work over a fullscreen game. This is what "game passthrough"
-- means in the per-binding model — not "a category is off", but "nothing except
-- these is live in `game`".
-- ---------------------------------------------------------------------------

h.bind("guide+r1",          "Workspace right",     h.workspace "+1")
h.bind("guide+l1",          "Workspace left",      h.workspace "-1")
h.bind("guide+stick_right", "Workspace right",     h.workspace "+1")
h.bind("guide+stick_left",  "Workspace left",      h.workspace "-1")
h.bind("guide+x",           "Omarchy launcher",    h.exec "omarchy-menu")
h.bind("guide+a",           "Dictation toggle",    h.exec "voxtype record toggle")
h.bind("guide+b",           "Close window",        h.dispatch "hl.dsp.window.close()")
h.bind("guide+r5",          "Media play/pause",    h.exec "playerctl play-pause")
h.bind("guide+menu",        "Omarchy menu",        h.exec "omarchy-menu")
h.bind("guide+l2",          "Previous tab",        h.dispatch "hl.dsp.group.prev()")
h.bind("guide+r2",          "Next tab",            h.dispatch "hl.dsp.group.next()")
h.bind("guide+y",           "On-screen keyboard",  h.keyboard { mode = "split" })

-- View is the "show me the map" key: it raises the cheat sheet — this very
-- file, drawn onto a controller diagram. The descriptions above are what it
-- prints, so a binding with no description reads as a guess (`hyprpad bindings`
-- shows the same table in a terminal).
--
-- Needs the Quickshell widget installed once: `scripts/hyprpad-cheatsheet
-- install`, which prints the two steps it will not take for you.
h.bind("guide+view",        "Cheat sheet",         h.exec "hyprpad-cheatsheet toggle")

-- ---------------------------------------------------------------------------
-- Manual override (docs/13 decision #4). Beats the focus rules until cleared.
--
-- Uncomment to force the desktop layer live over a game (to answer a message
-- mid-match), and to hand it back:
-- h.bind("guide+l4", "Force desktop mode", h.set_mode "desktop")
-- h.bind("guide+l5", "Back to automatic",  h.clear_mode())

-- R4 grip: screenshot (fullscreen = no picker to drag through).
h.bind("guide+r4", "Screenshot", h.exec "omarchy screenshot fullscreen")   -- one press, whole screen; use "windows"/"region" for a picker
