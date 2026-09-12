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
-- `.fullscreen`, `:process_tree_has("claude")`), `ctx.layers` (the layer-shell
-- overlays on screen: `:has("hyprpad-cheatsheet")`, `:list()`, and
-- `ipairs`-able) and `ctx.locked` (whether the session is locked). Overlays are
-- how hyprpad's own windowless UI — the cheat sheet, the on-screen keyboard —
-- becomes something a rule can select on; `ctx.locked` is how the lock screen
-- does, since it is neither a window nor a layer.

local h = hyprpad

-- ---------------------------------------------------------------------------
-- Modes: named contexts, evaluated in definition order, first match wins.
--
-- A mode carries nothing but its name, its rule, and whether it forwards raw
-- input to the game. What is LIVE in a mode is decided per binding, below.
-- ---------------------------------------------------------------------------

-- The session lock, FIRST — above even the cheat sheet, because the lock is
-- drawn over everything and owns the keyboard outright.
--
-- Omarchy's lock screen is an `ext-session-lock-v1` surface, which is neither a
-- window nor a layer: no `activewindow`, nothing in `ctx.layers`, so without
-- `ctx.locked` a locked session looks exactly like the desktop and the bare
-- buttons below type into the password field — D-pad arrows, A as Enter, B as
-- Backspace, the pads as a mouse.
--
-- The mode has NO bindings guarded into it, and that is the whole point: every
-- bare button, both mouse clicks, the cursor and the scroll are `:only_in`
-- other modes, so declaring `locked` and mentioning it nowhere else takes all
-- of them away here. Nothing is forwarded either — no `forward = true`.
--
-- The guide chords stay live, deliberately. They are unguarded (see the bottom
-- of this file), so they survive every mode including this one; a chord cannot
-- reach the password field, and `guide+r5` (play/pause) is a reasonable thing
-- to want from a locked screen. The one exception to "nothing is live here" is
-- the on-screen keyboard, which `guide+y` can still raise on purpose — it is
-- the only deliberate way to type at a lock screen from the controller.
h.mode("locked").when(function(ctx) return ctx.locked end)

-- The cheat sheet is modal while it's up: B closes it (it listens for Escape).
--
-- FIRST, above even `game`: the sheet is an overlay drawn over whatever is
-- focused, including a game, and while it is up it owns the keyboard. Layer
-- surfaces do not raise an `activewindow` event, so `ctx.layers` is the only
-- thing that can see it — that is what makes this a mode rather than a special
-- case wired into the daemon.
h.mode("cheatsheet").when(function(ctx) return ctx.layers:has("hyprpad-cheatsheet") end)

-- Omarchy's gamepad launcher (the fullscreen app grid): its own layer, its own
-- keys (arrows, Enter, PgUp/PgDn, p = pin, Esc = clear filter then close).
h.mode("launcher").when(function(ctx) return ctx.layers:has("omarchy-launcher") end)

-- Game / Steam Big Picture. Steam launches native and Proton titles as
-- `steam_app_<id>`; gamescope is matched too. The Steam client itself gets the
-- desktop bindings; only its Big Picture window (matched by title) passes through.
--
-- Deliberately NOT triggered by fullscreen: a fullscreen video is not a game.
-- Omarchy's transient, keyboard-owning shell surfaces (ALLOWLIST -- omarchy-bar and
-- omarchy-background are always present and must never match). While one is up,
-- B = Escape (closes any of them; the menu closes from any depth). The menu is
-- mouse-driven and arrow/Enter-navigable, so the cursor, scroll, D-pad and A stay live.
h.mode("omarchy-ui").when(function(ctx)
  for _, ns in ipairs({ "omarchy-menu", "omarchy-keyboard-panel", "omarchy-clipboard",
                        "omarchy-emojis", "omarchy-image-selector", "omarchy-reminders",
                        "omarchy-polkit", "omarchy-network-qr", "omarchy-bar-nav" }) do
    if ctx.layers:has(ns) then return true end
  end
  return false
end)

h.mode("game", { forward = true }).when(function(ctx)
  local c = ctx.focus.class:lower()
  local t = ctx.focus.title:lower()
  return c:match("^steam_app_") ~= nil
      or c:match("^steam_proton") ~= nil
      or c:match("^gamescope") ~= nil
      -- The Steam client is a desktop app (library, store, chat) and keeps the
      -- desktop bindings -- except in Big Picture, a controller UI that owns
      -- every input. The window keeps its class and retitles itself
      -- "Steam Big Picture Mode", so the title is the switch.
      or ((c == "steam" or c == "steamwebhelper") and t:match("big picture") ~= nil)
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

-- An AI coding agent in the focused terminal: Claude Code, OpenAI Codex, Muse,
-- OpenCode. Titles are project names with spinners, so the rule looks INSIDE the
-- window: `process_tree_has` walks the focused window's /proc descendants and
-- matches a lowercase substring of "<comm> <cmdline>". `muse-bin` because the
-- binary is `muse-bin-<version>` (comm is truncated to 15 chars); `opencode`
-- matches both `opencode2` and its bun child; `codex` also matches its
-- `codex-code-mode` helper. No agent-specific bindings yet — this only names
-- the context, so the bar and the cheat sheet show it and rules can key on it.
h.mode("agent").when(function(ctx)
  local f = ctx.focus
  return f:process_tree_has("claude") or f:process_tree_has("codex")
      or f:process_tree_has("muse-bin") or f:process_tree_has("opencode")
end)

-- Ordinary desktop use. No rule: it is the fallback.
h.mode("desktop")
h.default_mode "desktop"

-- ---------------------------------------------------------------------------
-- Daemon
-- ---------------------------------------------------------------------------

h.daemon { own_lizard = true }

-- The firmware's own power timers, written alongside the lizard disable and
-- re-sent with it every 30 s (docs/research/guide-hold-poweroff.md).
--
-- `steam_button_poweroff` is SETTING_STEAMBUTTON_POWEROFF_TIME (25): how long
-- the FIRMWARE wants the Steam button held before it powers the controller off.
-- That timer — not Steam, not hyprpad — is what turns the controller off while you
-- are holding the guide and deliberating. `sleep_inactivity_timeout` is
-- SETTING_SLEEP_INACTIVITY_TIMEOUT (50), the idle sleep.
--
-- Both are COMMENTED OUT because their units are UNVERIFIED and Valve publishes
-- no defaults table: "off" writes 0xFFFF (the widest value the u16 field holds,
-- which is a long time under every candidate unit and is safe whether or not
-- the firmware reads 0 as "never"), and an integer is written raw. Read the
-- firmware's own numbers first, then test with a stopwatch:
--
--   hyprpad controller-settings 25 50    # current / max / default, read-only
--   …uncomment, `hyprpad reload`, read again, then time a hold
--
-- h.daemon { steam_button_poweroff = "off" }        -- or a number: 300, 0, …
-- h.daemon { sleep_inactivity_timeout = 600 }       -- seconds on the 2015 fw

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
  only_in = { "desktop", "omarchy-ui", "browser", "agent", "launcher" },
  guide_in = { "game" }, -- except while the guide is held: then it is a mouse in a game too
}

h.scroll {
  mode = "circular",
  sensitivity = 1.0,
  circular_step_degrees = 15.0,
  only_in = { "desktop", "omarchy-ui", "browser", "agent" },
}

-- The caret jog wheel (docs/research/text-scrub.md): with the guide HELD,
-- circling the LEFT pad steps the caret, faster circling climbs to whole words,
-- and holding `select` sends every step with Shift. Writing the block IS the
-- opt-in — the values below are the defaults, spelled out to be tuned.
--
-- Guarded like the cursor above, minus `browser`, deliberately: `select` is L5
-- and `guide+l5` is the link-hints chord down there, so leaving the scrub out
-- of that one mode is what keeps the two off the same button.
h.scrub {
  detent_deg       = 15.0,   -- one caret step per 15 degrees (24 per revolution)
  min_radius       = 0.35,   -- ignore the dead centre, where the angle is noise
  fast_deg_per_s   = 360.0,  -- above one revolution/s the step doubles...
  fast_min_detents = 2,      -- ...once two detents in a row are that fast
  slow_deg_per_s   = 180.0,  -- and drops back below this (2:1, so it cannot chatter)
  word_tier        = true,   -- top rung is ctrl+arrow, not x4 characters
  select           = "l5",   -- hold the left grip to select as you scrub
  -- On a controller with no pads (the Xbox Elite) the same binding is a
  -- SHUTTLE on the left stick: hold it over and the caret walks at a rate the
  -- deflection picks -- 4/s at the deadzone edge up to 25/s, and past 85% the
  -- taps become ctrl+arrow so it hops whole words. Defaults, spelled out.
  shuttle = { deadzone = 0.15, slow_per_s = 4, fast_per_s = 25, word_above = 0.85 },
  only_in          = { "desktop", "omarchy-ui", "agent" },
}

h.haptics { cursor_spacing_px = 96 } -- sparser cursor texture (default 64)

-- ---------------------------------------------------------------------------
-- Bare buttons (no guide modifier) -> keys, on the desktop layer only.
--
-- Already suppressed while the OSK is up or guide is held; `:only_in("desktop")`
-- is what hands them to the game under a game-classed window.
-- ---------------------------------------------------------------------------

h.button("dpad_up",    h.key "up"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("dpad_down",  h.key "down"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("dpad_left",  h.key "left"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("dpad_right", h.key "right"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("a", h.key "enter"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")     -- A = Enter/confirm
h.button("b", h.key "backspace"):only_in("desktop", "browser", "agent") -- B = Backspace

-- In an agent terminal, X clears the line (readline's ctrl+u); B is still Backspace.
h.button("x", "Clear line", h.key "ctrl+u"):only_in("agent")
-- Y is Escape: the interrupt in Claude Code, Codex and OpenCode, and the way
-- out of any prompt. R1 is the hard interrupt (ctrl+c) for the shells and
-- tools that want it — a bumper, so a squeeze can't kill a running command.
h.button("y",  "Escape / interrupt", h.key "escape"):only_in("agent")
h.button("r1", "Interrupt (ctrl+c)", h.key "ctrl+c"):only_in("agent")
-- Clipboard, as the terminal spells it: L1 pastes (ctrl+shift+v), View copies the
-- selection (ctrl+shift+c) — a no-op with nothing highlighted, so a stray press is harmless.
h.button("l1",   "Paste clipboard",    h.key "ctrl+shift+v"):only_in("agent")
h.button("view", "Copy selection",     h.key "ctrl+shift+c"):only_in("agent")
-- Menu expands the transcript (Claude Code's ctrl+o); a view toggle, harmless elsewhere.
h.button("menu", "Expand transcript",  h.key "ctrl+o"):only_in("agent")

-- In the launcher: B backs out (Esc clears the filter, then closes — the grid does
-- not speak XF86Back), X pins the tile, bumpers turn pages.
h.button("b",  "Back / close",   h.key "escape"):only_in("launcher")
h.button("x",  "Pin / unpin",    h.key "p"):only_in("launcher")
h.button("l1", "Previous page",  h.key "pageup"):only_in("launcher")
h.button("r1", "Next page",      h.key "pagedown"):only_in("launcher")

-- The mouse buttons. These were hardwired once; now they are bindings like
-- any other, so the sheet shows them and a mode can take them away. Guarded
-- exactly like the cursor above: where the pad moves the pointer, it clicks.
h.button("rpad_click", h.mouse "left"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("r2", h.mouse "left"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")
h.button("l2", h.mouse "right"):only_in("desktop", "omarchy-ui", "browser", "agent", "launcher")

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

-- Bar panels (docs/research/bar-navigation.md). Every panel the bar opens shares
-- the `omarchy-keyboard-panel` layer — already in the `omarchy-ui` allowlist
-- above — and Tab inside one closes it and opens its neighbour, so R1 walks the
-- panel ring: Agents, Bluetooth, Network, Audio, Display, Power — and Shift+Tab
-- on L1 walks it backwards. The same two keys step the ICON ring that
-- `guide+dpad_up` (below) raises, so the bumpers mean the same thing at both
-- levels. The bumpers' second alternate, and as exclusive as the first: under a
-- panel we are in `omarchy-ui`, never in `cheatsheet`.
h.button("l1", "Previous panel", h.key "shift+tab"):only_in("omarchy-ui")
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

-- The keyboard's own verbs, not just keys: `h.osk "commit|shift|dismiss"`. The
-- entries here layer OVER the built-in Deck map, so this line only restates the
-- default (L2 = Shift while held; R2 = Enter, the pad clicks commit, B and Menu
-- close it) — it is here to show the form, and to give the sheet a real label.
h.osk_button("l2", "Hold for capitals", h.osk "shift")

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
h.bind("guide+x",           h.workspace "emptyn")
h.bind("guide+stick_down",  h.workspace "previous")
h.bind("guide+stick_up",    "Scratchpad",          h.dispatch 'hl.dsp.workspace.toggle_special("scratchpad")')

-- Everywhere: guide + a LEFT-stick flick moves focus between windows, the way
-- Omarchy's SUPER + arrows do (hl.dsp.focus({ direction = … })). Guide chords are
-- system-level, so this one is deliberately unguarded.
h.bind("guide+lstick_left",  "Focus window left",  h.dispatch 'hl.dsp.focus({ direction = "l" })')
h.bind("guide+lstick_right", "Focus window right", h.dispatch 'hl.dsp.focus({ direction = "r" })')
h.bind("guide+lstick_up",    "Focus window above", h.dispatch 'hl.dsp.focus({ direction = "u" })')
h.bind("guide+lstick_down",  "Focus window below", h.dispatch 'hl.dsp.focus({ direction = "d" })')
h.bind("guide+a",           "Dictation toggle",    h.exec "voxtype record toggle")
h.bind("guide+b",           "Close window",        h.dispatch "hl.dsp.window.close()")
h.bind("guide+r5",          "Media play/pause",    h.exec "playerctl play-pause")
h.bind("guide+menu",        "Omarchy menu",        h.exec "omarchy-menu")

-- The bare Steam button: the launcher on the desktop, Steam's overlay in a game.
-- Not a chord at all — a quick press and release with NOTHING in it, which is
-- what makes it safe to bind next to the guide layer: a chord, a flick, a hold
-- spent on the caret scrub or the guide-mouse, and anything past
-- `guide_tap_max_ms` (400 ms of deliberating) each bind nothing. `:not_in("game")`
-- spells out in the config what the daemon enforces anyway — in a game the tap
-- is replayed to Steam and no binding runs.
h.bind("guide_tap",         "App launcher",        h.exec "omarchy launcher toggle"):not_in("game")
h.bind("guide+l2",          "Previous tab",        h.dispatch "hl.dsp.group.prev()")
h.bind("guide+r2",          "Next tab",            h.dispatch "hl.dsp.group.next()")
-- `split` is what a controller WITH trackpads gets: two edge columns, one per
-- thumb, floating over the desktop. On one without, the same line raises the
-- full keyboard from the bottom edge with an exclusive zone, so content is
-- displaced rather than covered, and the D-pad/sticks move a highlight that A
-- types. Add `padless = { mode = …, reflow = … }` to say otherwise.
h.bind("guide+y",           "On-screen keyboard",  h.keyboard { mode = "split" })

-- The one guarded chord here, and the other half of `guide_in` above: with the
-- guide held the right pad is a mouse in `game`, so the pad click has to be a
-- click there too. Elsewhere the bare `rpad_click` binding is already one.
h.bind("guide+rpad_click",  "Click (guide mouse)", h.mouse "left"):only_in("game")

-- `guide+x` and the stick's down flick above are Hyprland workspace selectors
-- (docs/research/empty-workspace.md), handed straight to `h.workspace`:
-- `emptyn` is the first empty workspace to the RIGHT of this one, created at
-- the end if none is free, and `previous` is where you were before it. They
-- need no description of their own — the sheet words a selector itself ("Next
-- empty workspace"). The UP flick is deliberately NOT one of them: `h.workspace
-- "special:scratchpad"` would compile to `hl.dsp.focus({ workspace = … })`,
-- which only ever FOCUSES the scratchpad — flick up twice and it is still
-- there. The dispatch above is `toggle_special`, the same verb Omarchy binds to
-- SUPER + S, so the second flick puts it away again. "B closes, X opens", the
-- stick's left/right flicks are free (L1/R1 already step the workspace), and
-- D-pad down takes the focused window along to the new one.
h.bind("guide+dpad_down",   h.move_to_workspace "emptyn")

-- Up to the bar (docs/research/bar-navigation.md, phase 1): raises the focus
-- ring over the bar's own icons. The ring lives in the `hyprpad.status` widget
-- and maps a keyboard-focused layer surface named `omarchy-bar-nav` while it is
-- up — which is the ONLY thing the daemon has to know. That namespace is in the
-- `omarchy-ui` allowlist above, so from the moment the ring appears the pad is
-- already sending the right keys: D-pad -> arrows walk it, A -> Enter activates
-- the focused widget (the same call a mouse click makes), B -> Back leaves.
--
-- So this one chord is the whole cost on the controller map. Nothing below the
-- entry needs a binding of its own: the ring reads the keys the pad was already
-- sending in `omarchy-ui`.
--
-- Once a panel opens, the ring hands the keyboard over and the panel behaves as
-- it always has (R1 = Tab walks to the next panel); B closes it and the ring
-- comes back, B again leaves the bar.
h.bind("guide+dpad_up",     "Bar ring",            h.exec "omarchy-shell -q hyprpad.status navEnter")

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

-- Turn the CONTROLLER off, deliberately (`0x9F ID_TURN_OFF_CONTROLLER`). The
-- replacement for the firmware's guide-hold power-off that fires while you are
-- deliberating: lengthen or disable that with `steam_button_poweroff` above,
-- and put the off switch somewhere you cannot hit by accident.
--
-- The quick-access "…" button is free on this pad and is nowhere near a thumb
-- resting on the guide. `hyprpad off` does the same from a script, and the bar
-- widget does it on a right-click.
h.bind("guide+quickaccess", "Controller off", h.controller_off())

-- R4 grip: screenshot (fullscreen = no picker to drag through).
h.bind("guide+r4", "Screenshot", h.exec "omarchy screenshot fullscreen")   -- one press, whole screen; use "windows"/"region" for a picker

-- ---------------------------------------------------------------------------
-- Browser link hints (docs/research/browser-hints.md).
--
-- One chord types Vimium's `f` into the page and puts the pad into `hints`,
-- where the bare buttons ARE the hint letters — chosen so Vimium's upper-cased
-- labels read as the button you press (A/X/Y, N/S/W/E on the D-pad, Q/C on the
-- bumpers, 1-4 on the grips, 5/6 on the stick clicks). Vimium must be told the
-- same alphabet, or it will label the links with letters no button sends:
-- Options -> "Characters used for link hints" = axynsweqc123456, and add
-- `unmap x` under Custom key mappings so X is a hint letter rather than
-- Vimium's own close-tab.
--
-- `hints` has NO rule and lets itself go (`:transient`) — nothing outside the
-- page can say when Vimium's hints are gone, so the mode carries its own way
-- out: three presses is Vimium's longest code, B cancels (eaten, never typed),
-- following a link renames the window, a click dismisses the hints too, and 8 s
-- ends a session you walked away from. A transient mode may not have a `:when`;
-- `h.set_mode` is the only way in.
-- ---------------------------------------------------------------------------

h.mode("hints"):transient {
  max_presses = 3,
  exit_on = { "b", "focus", "title", "click" },
  timeout_ms = 8000,
}

-- `h.seq` is the one action that is a list of actions, run in order on a single
-- press: type into the page, THEN move the daemon into the mode whose buttons
-- are that page's hint letters. An `h.key` inside a sequence is a tap, not a
-- held output — there is no release edge for it to pair with.
h.bind("guide+l4", "Link hints",            h.seq { h.key "f",       h.set_mode "hints" }):only_in("browser")
h.bind("guide+l5", "Link hints -> new tab", h.seq { h.key "shift+f", h.set_mode "hints" }):only_in("browser")

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

-- B stays UNBOUND in `hints`: it is the mode's exit button, eaten not typed.
-- The clicks are kept, because a hint you cannot reach is still a link you can
-- point at — and a click is one of the mode's own exits.
h.button("rpad_click", h.mouse "left"):only_in("hints")
h.button("r2", h.mouse "left"):only_in("hints")
h.button("l2", h.mouse "right"):only_in("hints")
