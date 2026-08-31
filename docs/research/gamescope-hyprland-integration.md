# Research: living-room gaming on Hyprland — gamescope, BPM, HDR, PiP, focus (consolidated agent report)

*Produced 2026-08-30/31. Probed machine: hybrid laptop (AMD 890M iGPU scanout +
NVIDIA RTX 5070 Laptop, driver 610.57.04); target: NVIDIA tower. Companion for
deep §1/§3 detail: [steam-bpm-launch-wrapping.md](steam-bpm-launch-wrapping.md).*

## 0. Environment and build-specific warnings

| Fact | Evidence (all VERIFIED locally) |
|---|---|
| Hyprland **0.56.2**, fork `hypxrland`, tag `v0.56.2-374-g67200a8383`, binary `/usr/lib/hypxrland/Hyprland` | `hyprctl version` |
| Config provider: **Lua** (`hyprland.lua`); `backend: drm` | `hyprctl status` |
| gamescope 3.16.25, steam 1.0.0.87-3, grim 1.5.0, slurp 1.5.0, wl-clipboard 2.3.0, mpv 0.41.0 | `pacman -Q` |
| **Not** installed: playerctl, hyprshot, mpvpaper, wlrctl | `pacman -Q` |

**Lua-only dispatch.** Classic dispatchers are dead on this build —
`hyprctl dispatch pin` → `error: … expected a dispatcher (e.g.
hl.dsp.window.close())`; `hyprctl dispatch "focuswindow address:0x…"` fails to
parse. Use `hl.dispatch(hl.dsp.window.pin{…})`,
`hl.dsp.focus({ window = "address:0x…" })`, `hl.dsp.cursor.move({x,y})`,
`hl.dsp.window.set_prop{prop,value,window?}`,
`hl.dsp.window.fullscreen_state{internal,client,…}` (all enumerated live via
`hyprctl repl`). `hyprctl keyword` silently no-ops (exits 0) under the Lua
config manager (VERIFIED, and documented in `~/.config/hypr/AGENTS.md`). Lua
config landed in 0.55; hyprlang is being dropped —
https://hypr.land/news/26_lua/ (VERIFIED). **Every online
`hyprctl dispatch`/`keyword` recipe needs translation.**

## 1. Steam Big Picture in a Hyprland workspace (summary — full detail in companion report)

- **Flags (VERIFIED, `strings` on installed 2026 client):** `ubuntu12_32/steam`
  has `-gamepadui -steamos -steamos3 -steamdeck -fulldesktopres`;
  `ubuntu12_32/steamui.so` has `-bigpicture -gamepadui -tenfoot` and the
  literal log strings `"%s: forcing gamepadui, overriding tenfoot/bigpicture"`
  and `"forcing gamepadui for steamdeck + gamescope"` — all three flags
  converge on the modern gamepad UI; **use `-gamepadui`**. `-steampal` and
  `-newbigpicture` are **absent** from local binaries. Shipped desktop action
  (VERIFIED): `/usr/share/applications/steam.desktop:252:
  Exec=/usr/bin/steam steam://open/bigpicture`; exit URL
  `steam://close/bigpicture`
  ([steam-for-linux#12577](https://github.com/ValveSoftware/steam-for-linux/issues/12577), open).
- **Rendering under XWayland:** works; no native Wayland client exists
  ([#4924](https://github.com/ValveSoftware/steam-for-linux/issues/4924), open
  since 2017, 248 comments — VERIFIED). Local `xwayland:force_zero_scaling =
  true` handles HiDPI (VERIFIED live). `SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS=0`
  prevents self-minimize (INFERRED, community-standard).
- **The real risk is controller-focus arbitration.**
  [steam-for-linux#8640](https://github.com/ValveSoftware/steam-for-linux/issues/8640)
  — *"Controller input navigates invisible Overlay menu in wlroots
  compositors"* — **open, updated 2026-03-20**; Hyprland named in-thread;
  latest comment: kwin_wayland unaffected, *"Some 'signaling' from wlroots must
  be missing"* (VERIFIED). The missing signal is structural: the gamescope
  binary defines the X11 atoms `STEAM_INPUT_FOCUS`, `STEAM_BIGPICTURE`,
  `STEAM_GAME`, `STEAM_OVERLAY`, `STEAM_GAMES_RUNNING` (VERIFIED,
  `strings /usr/bin/gamescope`), and Steam's binaries contain 0 occurrences of
  `STEAM_INPUT_FOCUS` — gamescope writes it *for* Steam; plain Hyprland never
  does (INFERRED, high confidence). Related:
  [#8020](https://github.com/ValveSoftware/steam-for-linux/issues/8020)
  overlay not Wayland-aware (open);
  [#10251](https://github.com/ValveSoftware/steam-for-linux/issues/10251) OSK
  not Wayland-aware (open);
  [#13385](https://github.com/ValveSoftware/steam-for-linux/issues/13385)
  Steam Input Game Actions broken under Wayland (open, 2026-07);
  [#11255](https://github.com/ValveSoftware/steam-for-linux/issues/11255) BPM
  lag on NVIDIA, *"Works correctly under Gamescope"* in the title — **closed**
  2026-06 (VERIFIED).
- **SteamOS recipe (VERIFIED at source):**
  [ChimeraOS sessions.d/steam](https://raw.githubusercontent.com/ChimeraOS/gamescope-session-steam/main/usr/share/gamescope-session-plus/sessions.d/steam)
  line 100: `CLIENTCMD="steam -gamepadui -steamos3 -steampal -steamdeck"`,
  lines 90–91: `QT_IM_MODULE=steam` / `GTK_IM_MODULE=Steam` (the OSK enabler).
  AUR: `gamescope-session-steam-git r10`, `gamescope-session-git r339`
  (VERIFIED via AUR RPC).

**Recommendation:** run BPM inside a *nested* gamescope window placed on the
living-room workspace (`gamescope -e --backend wayland … -- steam -gamepadui`)
rather than bare XWayland — it sidesteps #8640/#8020/#10251 wholesale.

## 2. Nested gamescope on NVIDIA + HDR; Hyprland HDR status

### 2a. Nested gamescope works on NVIDIA (VERIFIED by execution)

```
$ gamescope --backend wayland -W 480 -H 270 -b -- sleep 3        # on this machine
vulkan: selecting physical device 'NVIDIA GeForce RTX 5070 Laptop GPU'
vulkan: physical device supports DRM format modifiers
vulkan: supported DRM formats: … AB4H XB4H AB30 XB30 AR30 XR30   <- 10-bit + FP16 present
xdg_backend: Post-Initted Wayland backend
```

- The startup spam `xdg_backend: Compositor released us but we were not
  acquired. Oh no.`
  ([gamescope#1636](https://github.com/ValveSoftware/gamescope/issues/1636),
  open) is **cosmetic**: measured exactly 10 occurrences at startup then
  silence, over 8 s runs, with and without `WLR_RENDER_NO_EXPLICIT_SYNC=1`
  (VERIFIED).
- Two benign `zero modifiers for DRM format` errors are the trace of the
  **hybrid-laptop** modifier-intersection issue
  ([#2081](https://github.com/ValveSoftware/gamescope/issues/2081),
  [#1590](https://github.com/ValveSoftware/gamescope/issues/1590)); single-GPU
  towers don't hit it; `--prefer-vk-device <pci-id>` fixes it where it bites
  (VERIFIED from source).
- Nested is gamescope's **best-supported NVIDIA path**: the de-facto
  maintainer develops on an RTX 5090
  ([PR#2315](https://github.com/ValveSoftware/gamescope/pull/2315)); nested
  confirmed on RTX 4090/nvidia-open 610
  ([PR#2358](https://github.com/ValveSoftware/gamescope/pull/2358)). The
  catastrophic NVIDIA bugs are all **embedded/DRM-backend**: HDR black screen
  [#1593](https://github.com/ValveSoftware/gamescope/issues/1593) (open since
  2024-10, 40 comments, reconfirmed 2026-08-16, NVIDIA states the allocation
  bug won't be fixed gamescope-side), 4K scanout corruption
  [#2309](https://github.com/ValveSoftware/gamescope/issues/2309), hotplug
  corruption [#2333](https://github.com/ValveSoftware/gamescope/issues/2333).
  **Avoid gamescope-session/embedded on NVIDIA.**
- **Performance:** nested is **zero-copy by default** — game DMA-BUFs are
  re-exported to the host as subsurfaces without a composite pass
  (`WaylandBackend.cpp`; contributor statement *"performance will be the
  same"*, [#1204](https://github.com/ValveSoftware/gamescope/issues/1204)).
  Composite is forced only by FSR/NIS/blur/reshade/ITM. Forum "3–6% overhead"
  numbers: NOT VERIFIED, treat as chatter.
- **Known NVIDIA failure mode to avoid:** FSR/NIS filters black-screen on
  Hyprland's Wayland backend
  ([#1833](https://github.com/ValveSoftware/gamescope/issues/1833),
  [#2089](https://github.com/ValveSoftware/gamescope/issues/2089)) *and*
  trigger an explicit-sync crash NVIDIA won't accept the fix for
  ([#1662](https://github.com/ValveSoftware/gamescope/issues/1662), rejected
  [PR#1841](https://github.com/ValveSoftware/gamescope/pull/1841)). **Keep the
  default `-F linear`**, or set `gamescope_upscale_preemptive=false` — since
  3.16.24 any ConVar is settable via env `gamescope_<convar>=<value>` (commit
  `0799dcfb`; present in installed 3.16.25). The nested `-r` fps-limiter is
  broken ([#1479](https://github.com/ValveSoftware/gamescope/issues/1479));
  use `gamescopectl debug_set_fps_limit N`.

### 2b. HDR through nested gamescope — mechanism verified end-to-end

gamescope's Wayland backend is a **client** of `wp_color_manager_v1`
(VERIFIED: `strings /usr/bin/gamescope` → `wp_color_manager_v1`,
`wp_image_description_v1`, `frog_color_management_factory_v1`, …). Live
handshake on this machine with `--hdr-enabled`:

```
xdg_backend: HDR INFO
  cv_hdr_enabled: true
  uMaxLum: 80, uRefLum: 80        <- equals Hyprland's sdrMaxLuminance=80 -> CM session ACTIVE
  bExposeHDRSupport: false        <- host output is SDR (cmPreset=srgb, 80 nits laptop panel)
```

- gamescope gates HDR on the host advertising six `wp_color_manager_v1`
  features all-or-nothing
  ([PR#2358](https://github.com/ValveSoftware/gamescope/pull/2358), open
  2026-08-29 — that PR fixes **Mutter**, which misses two). **Hyprland passes
  the gate** (VERIFIED at source on both sides; corroborated locally — the CM
  session demonstrably activated above). The local `bExposeHDRSupport: false`
  is solely the SDR panel — a chicken-and-egg the next item solves.
- **`quirks:prefer_hdr` exists in 0.56.2** (VERIFIED live):
  `default=0 map=[{disable:0},{enable:1},{gamescope_only:2}] :: "Prefer HDR
  mode."` — value **2 is purpose-built for this**: advertise HDR to gamescope
  clients without forcing the desktop into HDR. It's a
  `hl.config({ quirks = { prefer_hdr = 2 } })` option, *not* an `hl.monitor`
  field (probe-VERIFIED).
- No `ENABLE_HDR_WSI` needed; `ENABLE_GAMESCOPE_WSI=1` is set automatically
  for gamescope's children (VERIFIED, `steamcompmgr.cpp`); the WSI layer is
  installed
  (`/usr/share/vulkan/implicit_layer.d/VkLayer_FROG_gamescope_wsi.x86_64.json`,
  VERIFIED). Gap: no positive user report yet for the exact
  Hyprland+NVIDIA+HDR-nested triple
  ([discussion #10240](https://github.com/hyprwm/Hyprland/discussions/10240)
  predates `prefer_hdr`) — treat first run as validation. NOT VERIFIED
  end-to-end.

### 2c. Hyprland native HDR status (0.56.2)

Implements the **merged** `wp_color_manager_v1` (not `xx-color-management-v4`)
— VERIFIED from binary protocol symbols. Options (VERIFIED via
`hyprctl descriptions`, authoritative for this build): `render:cm_enabled`
(default true), `render:cm_auto_hdr` `{disable:0,hdr:1,hdredid:2}` (default
1), `render:cm_sdr_eotf`, `experimental:wp_cm_1_2` (default true),
`debug:full_cm_proto`. Monitor fields (probe-VERIFIED):
`cm ∈ auto|srgb|wide|edid|hdr|hdredid`, `bitdepth ∈ 8|10|auto`,
`supports_hdr`, `supports_wide_color`, `sdrbrightness`, `sdrsaturation` (no
underscores in the sdr names). There is **no** `render:cm_fs_passthrough` —
fullscreen passthrough is automatic; wiki: *"Fullscreen HDR is possible
without the hdr cm setting if render:cm_auto_hdr is enabled"* —
https://wiki.hypr.land/configuring/core/monitors/colors/ (VERIFIED). Master
tracker [#9064](https://github.com/hyprwm/Hyprland/issues/9064): HDR
passthrough done, SDR passthrough done, HW-cursor CM done (2026-08);
maintainer vaxerski 2026-08-25: *"Most things left are small edge cases."*
Direct-scanout+HDR: PQ10 works, scRGB does not. For Proton HDR:
`PROTON_ENABLE_WAYLAND=1 PROTON_ENABLE_HDR=1` with GE-Proton, driver >=
595.58.03, `bitdepth = 10` (VERIFIED, user-confirmed on NVIDIA).

### 2d. HDR path ranking (NVIDIA tower)

1. **Plain Hyprland fullscreen + Proton-Wayland HDR + `cm_auto_hdr`** —
   primary. 2. **Nested gamescope `--hdr-enabled` + `quirks:prefer_hdr = 2`**,
   default `-F linear` only — mechanism verified, awaiting field confirmation.
   3. **Embedded gamescope — avoid on NVIDIA** (#1593 et al.).

## 3. Global conditional launch wrapping (summary — full detail in companion report)

- **No global launch options exist in Steam.**
  [#3453](https://github.com/ValveSoftware/steam-for-linux/issues/3453) open
  since **2014-08-26** (latest comment 2026-04-11); also
  [#10475](https://github.com/ValveSoftware/steam-for-linux/issues/10475).
  `steam_dev.cfg` is connection/cvar config, no launch-wrapper key.
- **The clean hook: custom v2 compat tool as global default.**
  `~/.steam/steam/config/config.vdf` → `CompatToolMapping` (VERIFIED locally
  at line 1774; appid `"0"` = global default, currently unset on this box).
  Native Linux games covered: canonical spec
  ([steam-compat-tool-interface.md](https://gitlab.steamos.cloud/steamrt/steam-runtime-tools/-/blob/main/docs/steam-compat-tool-interface.md),
  §"Native Linux Steam games"): *"Version 2 compat tools are invoked with a
  `%verb%` in the `commandline` (if any) replaced by `waitforexitandrun`."*
  Working precedent
  [Scrumplex/Steam-Play-None](https://github.com/Scrumplex/Steam-Play-None):
  `from_oslist "linux"` / `to_oslist "linux"`, manifest
  `"commandline" "/launch.sh %verb%"`, `launch.sh` = `exec "${@:2}"`. **Use
  `linux→linux`** — `from_oslist "windows"` forces Windows depots. Escape the
  runtime before exec'ing host gamescope:
  `"$STEAM_RUNTIME/scripts/switch-runtime.sh" --runtime="" --` (spec,
  VERIFIED). Local schema reference:
  `~/.steam/root/compatibilitytools.d/GE-Proton11-5-x86_64/{compatibilitytool,toolmanifest}.vdf`.
- **Conditionality:** gate the wrapper on the existing Omarchy flag-file
  system — `omarchy-toggle livingroom` touches
  `~/.local/state/omarchy/toggles/livingroom`; `omarchy-hyprland-toggle` drops
  a Lua delta into `~/.local/state/omarchy/toggles/hypr/` and reloads (both
  VERIFIED from script source).
- **Payload: [ScopeBuddy](https://github.com/OpenGamingCollective/ScopeBuddy)**
  (canonical repo OpenGamingCollective, 206 stars, pushed 2026-08-18; AUR
  `scopebuddy 1.5.0-2` — VERIFIED). README: *"Fixes the Steam Overlay when
  used in nested/desktop mode; Fixes SteamInput when used in nested/desktop
  mode; Does not launch gamescope when it detects being started inside
  gamemode/gamescope-session"*; `SCB_AUTO_RES/HDR/VRR/REFRESH`, wlroots via
  `wlr-randr` (VERIFIED).
- **Avoid:** SteamTinkerLaunch (global-default flaky per its own wiki; no
  release since 2023-03; yad-15 GUI breakage
  [#1320](https://github.com/sonic2kk/steamtinkerlaunch/issues/1320)); editing
  `SteamLinuxRuntime_*/toolmanifest.vdf` (VERIFIED locally:
  `// Generated file, do not edit`); mass-editing `localconfig.vdf`
  LaunchOptions; `PROTON_REMOTE_DEBUG_CMD` / `STEAM_COMPAT_LAUNCHER_SERVICE`
  (debug channels, not prefixes).

## 4. Direct scanout / VRR / is gamescope necessary

**Options in this build** (VERIFIED, `hyprctl descriptions`):
`render:direct_scanout` `{disable:0,enable:1,auto:2}` default 0 (auto = on
with content-type *game*); `misc:vrr` `{off:0,on:1,fullscreen:2,
fullscreen_game:3}` default 0 (3 = fullscreen with *game or video* content);
`cursor:no_break_fs_vrr` `{0,1,auto:2}` default 2; `render:send_content_type`
default true; window rules `content = "game"`, `immediate`, `no_vrr`
(probe-VERIFIED). A per-window `direct_scanout` rule was proposed and
**closed unmerged**
([#14880](https://github.com/hyprwm/Hyprland/pull/14880)); no such key exists
here (probe-VERIFIED).

**Complete blocker enums** (VERIFIED, extracted from the binary; live JSON:
`hyprctl monitors -j` →
`solitary/solitaryBlockedBy/directScanoutTo/directScanoutBlockedBy/tearingBlockedBy`):
- Solitary: `WINDOWED CANDIDATE NOTIFICATION LOCK WORKSPACE DND SPECIAL ALPHA
  OFFSET OPAQUE OVERLAYS FLOAT WORKSPACES SURFACES CONFIGERROR FADEOUT`
- Direct scanout adds: `CONTENT MIRROR RECORD SW SURFACE TRANSFORM DMA FAILED
  CM STEREO USER`

**Key consequences** (source-verified against `v0.56.2`
`Monitor.cpp`/`Renderer.cpp`):
- **Any floating window on the workspace (`FLOAT`) or any non-empty overlay
  layer / visible top layer (`OVERLAYS`) kills solitary → kills direct scanout
  and tearing.** The omarchy bar (top layer, alpha 1 — VERIFIED via
  `hyprctl layers`) already blocks it whenever visible.
- **VRR modes 2/3 check only internal-fullscreen — never the solitary gate.**
  A PiP does **not** break VRR (source: `ensureVRR()`). Maximize
  (`internal=1`) breaks scanout *and* fullscreen-gated VRR; use true
  fullscreen.
- XWayland windows have a *fast path* for solitary (root surface returned
  directly) — X11 games qualify more easily than Wayland-native ones.
- **XWayland cannot change refresh rate** — VERIFIED empirically: `xrandr`
  lists ~30 emulated resolutions all at ~165 Hz; the panel's real 60 Hz mode
  is not exposed. Resolution changes are viewporter *scaling* emulation
  ([Phoronix on XWayland RandR emulation](https://www.phoronix.com/news/XWayland-RandR-Emulation)),
  never a modeset. Handle per-mode via
  `hl.monitor({ mode = "3840x2160@120", … })` in the living-room toggle, or
  gamescope's `-r`/`-w -h` spoofing — **the one capability Hyprland genuinely
  lacks**.
- NVIDIA caveat: direct scanout black-screens some native-Wayland games
  ([discussion #14843](https://github.com/hyprwm/Hyprland/discussions/14843),
  0.55→0.56.1, incl. driver 610.43.03); maintainer workaround
  `quirks:skip_non_kms_dmabuf_formats = 1` (exists here, default false —
  VERIFIED). Multi-GPU scanout carries an in-source
  `// This may implode on multi-gpu!!` (laptop concern, not tower).
- Latency numbers Hyprland-vs-gamescope: **NOT VERIFIED** — no reproducible
  benchmark exists; scanout saves at most one composite pass (INFERRED).

**Is gamescope necessary?** Not for res/VRR/latency. Its residual value:
**refresh/resolution spoofing** (unique), frame limiter, mangoapp, the §1
Steam-atom arbitration, and (pending validation) HDR containment. FSR/NIS:
avoid on Hyprland+NVIDIA (§2a). Hyprland's own wiki concedes the escape hatch:
*"Using gamescope tends to fix any and all issues with Wayland/Hyprland"*
(wiki 0.56.0 Performance page — VERIFIED).

## 5. PiP over a fullscreen game

**Render order** (VERIFIED at source, `Renderer.cpp#L1102`, bottom→top):
background → bottom-layer → fullscreen pass (with explicit *"then render
windows over fullscreen"* sub-pass) → special workspaces → **"pinned always
above"** pass → top-layer → **overlay-layer** → popups. Three escalating ways
above a fullscreen game: over-fullscreen floats (all newly-mapped floats get
`m_allowedOverFullscreen = true`), **pinned floats** (unconditional, own
pass; `pin` requires floating), layer-shell overlay (beats everything;
maintainer: *"top layers over fs is expected"* —
[#15937](https://github.com/hyprwm/Hyprland/pull/15937)). Live observability:
`hyprctl clients -j` → `allowedOverFullscreen`, `pinFullscreened` (VERIFIED;
from merged [#13066](https://github.com/hyprwm/Hyprland/pull/13066)).

**Layer-shell is a dead end for mpv:** mpv has no layer-shell support
([mpv#8830](https://github.com/mpv-player/mpv/issues/8830), open since 2021);
mpvpaper is whole-output-only (source-verified); no generic "any app as layer
surface" tool exists. **Pinned float is the mechanism.** Cost: direct scanout
+ tearing (accepted per §4); VRR survives.

Validated rule block (parses `config ok` against this exact binary):

```lua
hl.window_rule({ match = { class = "^(steam_app_.*)$" }, content = "game", idle_inhibit = "fullscreen" })
hl.window_rule({ name = "pip", match = { class = "^(mpv-pip)$" },
  float = true, pin = true,
  no_initial_focus = true, no_follow_mouse = true, suppress_event = "activate",
  size = { "monitor_w * 0.25", "monitor_h * 0.25" }, move = { "monitor_w * 0.74", "monitor_h * 0.02" },
  no_anim = true, no_blur = true, no_shadow = true, border_size = 0, rounding = 0,
})
hl.config({
  misc   = { vrr = 3 },
  input  = { float_switch_override_focus = 0 },
  cursor = { no_break_fs_vrr = 2, no_warps = true },
})
```

Launch: `mpv --title=PIP --x11-name=mpv-pip …` (stable rule match).

## 6. Focus semantics

Four distinct steal paths, each with its verified counter:

1. **Hover (follow_mouse).** This box: `input:follow_mouse = 0` (VERIFIED
   live) — ordinary hover cannot move focus.
2. **Hover (float-switch).** `input:float_switch_override_focus` (default
   **1**, active here) moves focus on **tiled/fullscreen↔floating** boundary
   crossings *even with follow_mouse=0* (VERIFIED in source). **Set it to 0**
   — the one config change this scenario requires.
3. **Map-time.** A new floating window steals focus regardless of
   follow_mouse unless `no_initial_focus`/`no_focus` (source
   `Window.cpp#L2444`); bonus: an active pointer constraint on the game
   blocks this automatically. `misc:on_focus_under_fullscreen` is
   **irrelevant** to floating windows (guard requires `!m_isFloating`).
4. **Activation.** `misc:focus_on_activate` is **true in this config**
   (Hyprland default false — VERIFIED) → `suppress_event = "activate"` on the
   PiP.

`no_focus` is a hard block; `stay_focused` is partial (enforced only with the
cursor over nothing — VERIFIED).

**Synthetic clicks:** `zwlr_virtual_pointer_v1` is implemented and **not**
permission-gated (this build's `hl.permission` accepts only
`screencopy|plugin|keyboard` — probe-VERIFIED); virtual pointers feed the same
event bus as real ones, hit-testing at the compositor cursor position.
Click-to-focus applies (except `follow_mouse=3`). Restore with
`hl.dsp.focus({ window = "address:0x…" })` after reading
`hyprctl activewindow -j` — **gotcha:** the focus dispatcher warps the cursor
into the target (`window->warpCursor()`); `cursor:no_warps = true` suppresses
it (honored — non-forced warp), while `hl.dsp.cursor.move` warps forced and
bypasses it.

**Pointer lock is decisive (source-verified):** while a game holds
`zwp_locked_pointer`/`zwp_confined_pointer`, `onMouseMoved` early-returns
before hit-testing — real mouse, virtual pointer, and `cursor.move` all snap
back; the constraint releases only when keyboard focus leaves the game.
**Therefore drive the PiP via MPRIS (§8), not clicks**; if a click is
unavoidable: focus-PiP → click → refocus game.

## 7. Screenshot to disk + clipboard

`~/.local/share/omarchy/bin/omarchy-capture-screenshot` (VERIFIED, read in
full) already does both. Invocation for **silent, non-interactive,
full-output** capture: **`omarchy-capture-screenshot fullscreen slurp`** —
mode `fullscreen` resolves the focused monitor's geometry with no picker
(`omarchy-capture-region fullscreen` → `0,0 2048x1280`, VERIFIED
non-interactive), and processing mode `slurp` (the default) does
file+clipboard+notification:

```bash
grim -g "$SELECTION" "$FILEPATH" || exit 1
echo "$FILEPATH"
wl-copy --type image/png <"$FILEPATH"
omarchy-notification-send "Screenshot saved to clipboard and file" … || true
```

Files land in `${OMARCHY_SCREENSHOT_DIR:-${XDG_PICTURES_DIR:-$HOME/Pictures}}`
as `screenshot-YYYY-mm-dd_HH-MM-SS.png`. Caveats (from source): the script
starts with `pkill slurp && exit 0` (no-ops if a picker is up) and temporarily
forces hardware cursors so the pointer isn't baked in. Minimal alternative:
`grim -o <output> - | wl-copy --type image/png` (grim 1.5.0 flags `-o/-g/-c/-T`
VERIFIED). `screencopy` is permission-gateable in this build but no
`hl.permission` rules are configured (VERIFIED).

## 8. MPRIS control of a Chromium YouTube tab

VERIFIED live against a running Chromium instance
(`org.mpris.MediaPlayer2.chromium.instance3003830`):

- **One MPRIS player per browser *process*** (`chromium.instance<PID>`), not
  per tab; with several YouTube tabs it controls the most-recently-active
  media session — no per-tab targeting without an extension
  ([browser-mpris2](https://github.com/otommod/browser-mpris2),
  [browser-playerctl](https://github.com/beingmohit/browser-playerctl)).
- **Stale-player bug reproduced live:** finished media leaves the player as
  `PlaybackStatus="Stopped"`, `CanPause=false`, `CanControl=true`, and
  `DesktopEntry` Get errors out —
  [Chromium bug 40703847](https://issues.chromium.org/issues/40703847). The
  daemon must check `CanPause`/`CanPlay` before acting (mirror
  `omarchy/shell/plugins/services/media/MediaModel.js`, which gates on exactly
  these — VERIFIED).
- **Chromium publishes no introspection XML** (`busctl introspect` returns
  empty — VERIFIED); property Get and method calls work fine. Use `playerctl`
  (Arch `extra/playerctl 2.4.1-5`; **install it** — not present) with
  `playerctl -l` → `playerctl -p chromium.instance<PID> play-pause`, or raw
  `busctl --user call <name> /org/mpris/MediaPlayer2
  org.mpris.MediaPlayer2.Player PlayPause`. Omarchy precedent for pausing
  media exists (`voxtype/config.toml: pause_media = true`, VERIFIED).

## Feasibility verdicts

**(a) BPM-in-workspace + per-game nested gamescope on NVIDIA with HDR —
feasible, with a specific shape.** BPM runs fine as an XWayland window and
`-gamepadui` is the verified 2026 flag, but bare-Hyprland BPM inherits the
open wlroots controller-arbitration bug (#8640) plus non-Wayland-aware overlay
and OSK — all of which stem from gamescope-written X11 atoms Steam expects
(`STEAM_INPUT_FOCUS` et al.), so BPM belongs *inside* a nested gamescope
window on the living-room workspace, and nested gamescope on NVIDIA is
verified working on this hardware and is upstream's best-supported NVIDIA path
(embedded/DRM is where the unfixable NVIDIA bugs live — avoid it). For HDR,
both remaining paths are live on the tower: plain Hyprland fullscreen with
Proton-Wayland HDR and `cm_auto_hdr` is the primary, and nested-gamescope HDR
is mechanically verified end-to-end — Hyprland passes gamescope's six-feature
color-management gate, with `quirks:prefer_hdr = 2` resolving the SDR-host
chicken-and-egg — but lacks a field report on the exact triple, so validate on
first run, keep `-F linear`, and never enable FSR/NIS on this stack.

**(b) PiP-over-fullscreen with focus retention — fully feasible,
source-proven, one accepted cost.** Pinned floats render in a dedicated pass
above fullscreen windows; the four focus-steal paths (hover, float-switch
hover, map-time, activation) are each closed by a verified knob — the only
config change needed beyond the PiP rule itself is
`float_switch_override_focus = 0`, since `follow_mouse = 0` is already set —
and the whole rule block parses clean against this exact binary. The cost is
direct scanout and tearing (the `FLOAT` solitary blocker is structural),
which the always-visible omarchy bar forfeits today anyway, while **VRR
survives untouched** because it never consults the solitary gate. Drive the
PiP by MPRIS rather than synthetic clicks: pointer-locked games provably
block every cursor path to the overlay, and Chromium's per-process MPRIS with
capability-checking (plus `playerctl`, to be installed) is sufficient for
pause/unpause.

**(c) Global conditional gamescope wrapping — feasible and clean.** Steam
will never give you global launch options (#3453, twelve years open), but a
custom v2 compatibility tool registered `linux→linux` and set as the `"0"`
default in `config.vdf`'s `CompatToolMapping` is a spec-supported single hook
that also fires for native Linux games (`%verb%` → `waitforexitandrun`);
Steam-Play-None proves the pattern in three lines of shell. Gate it on the
existing `~/.local/state/omarchy/toggles/livingroom` flag file (paired with an
`omarchy-hyprland-toggle` Lua delta that sets the TV mode, VRR, and HDR
monitor settings), escape the scout runtime before exec'ing host binaries, and
delegate gamescope invocation to ScopeBuddy. Avoid SteamTinkerLaunch,
generated-file edits, and `localconfig.vdf` mass-rewrites.
