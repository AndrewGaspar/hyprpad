-- hyprsc living-room programme: compositor-side configuration (W1).
-- Research basis: hyprsc repo docs/research/gamescope-hyprland-integration.md
-- (rule block validated `config ok` against this exact Hyprland build).

hl.config({
  input = {
    -- Close the hover focus-steal path across the tiled/fullscreen<->floating
    -- boundary. With follow_mouse = 0 (set in input.lua) this is the one
    -- remaining hover path that could pull focus from a fullscreen game to a
    -- PiP overlay. Invisible in normal desktop use.
    float_switch_override_focus = 0,
  },

  cursor = {
    -- Don't let a cursor over a PiP/notification break fullscreen VRR.
    no_break_fs_vrr = 2,

    -- Deferred until hyprsc does programmatic focus-restore (the focus
    -- dispatcher warps the cursor into the target window; this suppresses
    -- it). Changes daily cursor behaviour, so left off until needed.
    -- no_warps = true,
  },

  misc = {
    -- VRR for fullscreen game/video content. VRR survives a PiP overlay
    -- (research: ensureVRR never consults the solitary gate).
    vrr = 3,
  },
})

-- Steam acts on guide-button *release* by stealing focus (research 03).
-- Analog guide-chords are already suppressed Steam-side; this rule closes
-- the remaining paths: bare taps and button-chords no longer yank focus.
o.window("steam", { suppress_event = "activate" })

-- Picture-in-picture: a window with class "mpv-pip" floats, pins above
-- fullscreen games (dedicated render pass), and can never steal focus.
-- Launch e.g.: mpv --title=PIP --x11-name=mpv-pip <url>
local pip = { class = "^mpv-pip$" }
o.window(pip, { float = true })
o.window(pip, { pin = true })
o.window(pip, { no_initial_focus = true })
o.window(pip, { no_follow_mouse = true })
o.window(pip, { suppress_event = "activate" })
o.window(pip, { size = "25% 25%" })
o.window(pip, { move = "74% 2%" })
o.window(pip, { no_anim = true })
o.window(pip, { no_blur = true })
o.window(pip, { no_shadow = true })
o.window(pip, { border_size = 0 })
o.window(pip, { rounding = 0 })

-- Silent full-screen screenshot to disk + clipboard (no picker).
o.bind("SUPER + SHIFT + PRINT", "Screenshot (silent, full screen)",
  "omarchy-capture-screenshot fullscreen slurp")
