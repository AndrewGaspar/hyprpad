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
-- Omarchy's transient, keyboard-owning shell surfaces (ALLOWLIST -- omarchy-bar and
-- omarchy-background are always present and must never match). While one is up,
-- B = Escape (closes any of them; the menu closes from any depth). The menu is
-- mouse-driven and arrow/Enter-navigable, so the cursor, scroll, D-pad and A stay live.
h.mode("omarchy-ui").when(function(ctx)
  for _, ns in ipairs({ "omarchy-menu", "omarchy-keyboard-panel", "omarchy-clipboard",
                        "omarchy-emojis", "omarchy-image-selector", "omarchy-reminders",
                        "omarchy-polkit", "omarchy-network-qr" }) do
    if ctx.layers:has(ns) then return true end
  end
  return false
end)

h.mode("game", { forward = true }).when(function(ctx)
  local c = ctx.focus.class:lower()
  return c:match("^steam_app_") ~= nil
      or c:match("^steam_proton") ~= nil
      or c:match("^gamescope") ~= nil
      or c == "steam"
      or c == "steamwebhelper"
end)

-- A browser: Google Chrome, and Omarchy's web apps (Chrome in `--app` mode,
-- class `chrome-<host>__-Default`). Class-keyed like `game`, so it sits below
-- the layer-keyed modes — an open menu or the cheat sheet still wins — and
-- above `desktop`, the fallback. Every desktop guard below lists it too, so
-- a browser behaves exactly like the desktop except where a binding says
-- otherwise — today the bumpers, which switch tabs
-- (docs/research/browser-hints.md).
h.mode("browser").when(function(ctx)
  local c = ctx.focus.class:lower()
  return c == "google-chrome" or c:match("^chrome%-") ~= nil
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
  only_in = { "desktop", "omarchy-ui", "browser" },
}

h.scroll {
  mode = "circular",
  sensitivity = 1.0,
  circular_step_degrees = 15.0,
  only_in = { "desktop", "omarchy-ui", "browser" },
}

h.haptics { cursor_spacing_px = 96 } -- sparser cursor texture (default 64)

-- ---------------------------------------------------------------------------
-- Bare buttons (no guide modifier) -> keys, on the desktop layer only.
--
-- Already suppressed while the OSK is up or guide is held; `:only_in("desktop")`
-- is what hands them to the game under a game-classed window.
-- ---------------------------------------------------------------------------

h.button("dpad_up",    h.key "up"):only_in("desktop", "omarchy-ui", "browser")
h.button("dpad_down",  h.key "down"):only_in("desktop", "omarchy-ui", "browser")
h.button("dpad_left",  h.key "left"):only_in("desktop", "omarchy-ui", "browser")
h.button("dpad_right", h.key "right"):only_in("desktop", "omarchy-ui", "browser")
h.button("a", h.key "enter"):only_in("desktop", "omarchy-ui", "browser")     -- A = Enter/confirm
h.button("b", h.key "backspace"):only_in("desktop", "browser") -- B = Backspace

-- The mouse buttons. These were hardwired once; now they are bindings like
-- any other, so the sheet shows them and a mode can take them away. Guarded
-- exactly like the cursor above: where the pad moves the pointer, it clicks.
h.button("rpad_click", h.mouse "left"):only_in("desktop", "omarchy-ui", "browser")
h.button("r2", h.mouse "left"):only_in("desktop", "omarchy-ui", "browser")
h.button("l2", h.mouse "right"):only_in("desktop", "omarchy-ui", "browser")

-- The same button, a different meaning in a different mode. The cheat sheet has
-- keyboard focus and closes on Escape, so B dismisses it — and the two never
-- collide, because a mode is exclusive: under the sheet we are in `cheatsheet`,
-- never in `desktop`.
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
h.button("b", "Back (closes at top level)", h.key "back"):only_in("omarchy-ui")

-- The sheet has one tab per mode — what the pad does in THAT context, and
-- nothing else — and pages through them on Left/Right. The bumpers are the
-- obvious thing to page with, and they are free here because the sheet's own
-- mode is exclusive: `guide+l1`/`guide+r1` still change workspace.
h.button("l1", "Previous sheet tab", h.key "left"):only_in("cheatsheet")
h.button("r1", "Next sheet tab", h.key "right"):only_in("cheatsheet")

-- Bar panels (docs/research/bar-navigation.md, phase 0). Every panel the bar
-- opens shares the `omarchy-keyboard-panel` layer — already in the `omarchy-ui`
-- allowlist above — and Tab inside one closes it and opens its neighbour, so
-- R1 walks the ring that `guide+dpad_up` (below) opens: Agents, Bluetooth,
-- Network, Audio, Display, Power. R1's second alternate, and as exclusive as
-- the first: under a panel we are in `omarchy-ui`, never in `cheatsheet`.
h.button("r1", "Next panel", h.key "tab"):only_in("omarchy-ui")

-- Browser tabs on the bumpers (docs/research/browser-hints.md §6): Chrome's
-- own Ctrl+Shift+Tab / Ctrl+Tab, sent by the compositor to the focused
-- window — a bare button carrying a dispatch fires once, on the press edge.
-- Guarded into `browser`, so the bumpers still page the cheat sheet and walk
-- the bar's panels elsewhere, and `guide+l1`/`guide+r1` still change
-- workspace everywhere.
h.button("l1", "Previous browser tab", h.dispatch 'hl.dsp.send_shortcut({ mods = "CTRL SHIFT", key = "Tab" })'):only_in("browser")
h.button("r1", "Next browser tab",     h.dispatch 'hl.dsp.send_shortcut({ mods = "CTRL", key = "Tab" })'):only_in("browser")

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
h.bind("guide+x",           "New workspace",       h.dispatch 'hl.dsp.focus({ workspace = "emptyn" })')
h.bind("guide+stick_down",  "Previous workspace",  h.dispatch 'hl.dsp.focus({ workspace = "previous" })')
h.bind("guide+a",           "Dictation toggle",    h.exec "voxtype record toggle")
h.bind("guide+b",           "Close window",        h.dispatch "hl.dsp.window.close()")
h.bind("guide+r5",          "Media play/pause",    h.exec "playerctl play-pause")
h.bind("guide+menu",        "Omarchy menu",        h.exec "omarchy-menu")
h.bind("guide+l2",          "Previous tab",        h.dispatch "hl.dsp.group.prev()")
h.bind("guide+r2",          "Next tab",            h.dispatch "hl.dsp.group.next()")
h.bind("guide+y",           "On-screen keyboard",  h.keyboard { mode = "split" })

-- `guide+x` and the stick's down flick above are HypXRland workspace
-- selectors (docs/research/empty-workspace.md): `emptyn` is the first empty
-- workspace to the RIGHT of this one, created at the end if none is free, and
-- `previous` is where you were before it. They go through `h.dispatch` until
-- `h.workspace` learns them. "B closes, X opens", the right stick is a whole
-- family (left/right = ±1, down = back), and D-pad down takes the focused
-- window along to the new one.
h.bind("guide+dpad_down",   "Window to new workspace", h.dispatch 'hl.dsp.window.move({ workspace = "emptyn", follow = true })')

-- Up to the bar (docs/research/bar-navigation.md, phase 0): opens the first
-- panel of the bar's right section; R1 then walks the rest, the D-pad and A
-- drive the panel, B closes it.
h.bind("guide+dpad_up",     "Bar panels",          h.exec "omarchy-shell -q shell togglePanelAt right 1")

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
