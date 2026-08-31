# Research: Steam BPM under Hyprland + global game-launch wrapping

*Produced 2026-08-31 by a research agent; evidence base: web sources (URLs cited)
plus direct `strings` inspection of the installed 2026 Steam client on the
development machine (`steam 1.0.0.87-3`, binaries dated 2026-08-03:
`~/.local/share/Steam/ubuntu12_32/{steam,steamui.so,steamclient.so}`,
`ubuntu12_64/libcef.so`), `gamescope 3.16.25-1`, `hyprland 0.56.2-1`.
"VERIFIED (local binary)" = extracted from those binaries/files directly.*

## TOPIC 1 — Steam Big Picture Mode as a windowed app on Hyprland

### 1.1 CLI flags in the 2026 client — definitive

VERIFIED (local binary): `steamui.so` contains the flag table from
`/data/src/steamUI/startupmodemanager.cpp`:

```
Start in regular mode (force Big Picture mode off)  -nobigpicture
Silent startup mode (tray mode only)                -silent
Start in Steam Big Picture mode                     -tenfoot  -bigpicture
Start in gamepadui mode                             -gamepadui
                                                    -steamdeckdisplay  -steammachinedisplay
```

plus `ComputeStartupMode` strings: `steam://open/bigpicture`,
`steambeta://open/bigpicture`, `%s: forcing gamepadui, overriding
tenfoot/bigpicture`, `%s: forcing gamepadui for steamdeck + gamescope`,
`Switching to desktopui (from legacy vgui mode)`.

- `-gamepadui` — **works; canonical**. VERIFIED (local binary).
- `-bigpicture` / `-tenfoot` — **work as accepted aliases, silently upgraded to
  gamepadui** ("forcing gamepadui, overriding tenfoot/bigpicture"). The old VGUI
  BPM no longer exists. VERIFIED (local binary).
- `steam steam://open/bigpicture` — works, forces gamepadui. VERIFIED (local binary).
- `-newbigpicture` — **removed** (absent from all 2026 binaries). It was a
  transitional 2022 flag: Valve changelog Nov 9 2022 via
  [GamingOnLinux](https://www.gamingonlinux.com/2022/11/steam-client-beta-adds-a-new-launch-option-for-big-picture-mode/) — VERIFIED.
- `-steamos`, `-steamos3`, `-steamdeck`, `-nobigpicture`, `-silent`,
  `-fulldesktopres` — present. VERIFIED (local binary). `-steampal` — **not
  present** as a parsed flag in 2026 (ChimeraOS still passes it; harmlessness
  INFERRED).
- Startup mode also persists via registry keys `StartupMode`/`StartupModeTmp`/
  `StartupModeTmpIsValid` in `~/.steam/registry.vdf` — VERIFIED (local binary +
  local file); enum values NOT VERIFIED.
- No official Valve doc of Linux client flags exists;
  [developer.valvesoftware.com Command_Line_Options](https://developer.valvesoftware.com/wiki/Command_Line_Options)
  is the Arch-Wiki-linked reference but 403s and is Windows-oriented. NOT
  VERIFIED beyond that.

### 1.2 Rendering as a plain XWayland window on Hyprland

Works, with three attested problem classes:

1. **BPM performance collapse / corruption outside gamescope, incl. NVIDIA** —
   [steam-for-linux #11255](https://github.com/ValveSoftware/steam-for-linux/issues/11255)
   (2024-09 → 2026, 113 comments, closed COMPLETED but reports continue into Feb
   2026): 3–15 FPS in BPM, "Works correctly under Gamescope" in the title;
   comment 2025-08-03 explicitly names **Hyprland** and river as affected, Niri
   not — compositor-dependent. Partial workaround (mixed results): Settings →
   Interface → "Enable GPU accelerated rendering in web views" (BPM is a
   `steamwebhelper` CEF view). VERIFIED.
2. **BPM black screen on NVIDIA** —
   [#13519](https://github.com/ValveSoftware/steam-for-linux/issues/13519)
   (OPEN, 2026-08-15): window created/focused but painted opaque black; UI
   routed to "ensure desktopui window". Flatpak/X11 report; applicability to
   native client INFERRED.
3. **XWayland HiDPI blur** —
   [Arch Wiki Steam/Troubleshooting](https://wiki.archlinux.org/title/Steam/Troubleshooting):
   Steam renders at half res upscaled. Fixes:
   `gamescope -f -m 1 -e -- steam -gamepadui`, or Hyprland
   `xwayland { force_zero_scaling = true }` **plus** — because *"newer versions
   of Steam read their scale from the Xorg `Xft.dpi` resource directly"* —
   `echo "Xft.dpi: 192" | xrdb -merge -` before Steam starts (96→1.0, 144→1.5,
   192→2.0). VERIFIED.
4. Also: BPM self-minimizes on focus loss (multi-monitor) →
   `SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS=0`
   ([Arch Wiki](https://wiki.archlinux.org/title/Steam/Troubleshooting), refs
   [#4769](https://github.com/ValveSoftware/steam-for-linux/issues/4769)). VERIFIED.

### 1.3 Focus/controller input under wlroots/Hyprland

- **Headline bug:**
  [#8640 "Controller input navigates invisible Overlay menu in wlroots compositors"](https://github.com/ValveSoftware/steam-for-linux/issues/8640)
  — **OPEN since 2022, confirmed again 2026-03-20**; names Sway/River/**Hyprland**;
  controller input goes simultaneously to game and an invisible Steam overlay
  (can blind-launch other games); works fine on X11 WMs; 2026 comment:
  kwin_wayland unaffected, *"some signaling from wlroots must be missing"*;
  workaround = SIGSTOP `steamwebhelper` during play. VERIFIED.
- Mechanism (INFERRED, high confidence): gamescope manages X11 atoms
  `STEAM_INPUT_FOCUS`, `STEAM_BIGPICTURE`, `STEAM_GAME`, `STEAM_OVERLAY`
  (VERIFIED in `/usr/bin/gamescope` strings); `STEAM_INPUT_FOCUS` appears in
  **no** Steam binary — it's gamescope-side focus arbitration that a generic
  wlroots compositor never performs. This is why BPM-in-gamescope behaves and
  plain-windowed BPM doesn't.
- Supporting:
  [#11948 Steam steals keyboard input](https://github.com/ValveSoftware/steam-for-linux/issues/11948) (OPEN);
  [#10887 Steam UI freezes when switching workspaces](https://github.com/ValveSoftware/steam-for-linux/issues/10887) (OPEN);
  [#12231 controller input cut off to all non-Steam apps on Wayland (KDE+Hyprland)](https://github.com/valvesoftware/steam-for-linux/issues/12231);
  [#8020 Steam overlay has no Wayland support](https://github.com/ValveSoftware/steam-for-linux/issues/8020) (OPEN since 2021);
  [#13374 Steam Controller loses gamepad function in BPM with PROTON_ENABLE_WAYLAND](https://github.com/ValveSoftware/steam-for-linux/issues/13374)
  (2026, dup of #8020). Hyprland-side:
  [hyprwm/Hyprland #6468](https://github.com/hyprwm/Hyprland/issues/6468),
  [#7155](https://github.com/hyprwm/Hyprland/issues/7155). All VERIFIED. No
  Hyprland-repo issue specifically about BPM found — NOT VERIFIED that one exists.
- Window identification for rules: BPM window title literal **"Steam Big
  Picture"**, class `steam` — VERIFIED (local binary strings; class corroborated
  by [ThingLab blog](https://thinglab.org/2026/01/hyprland_steam_windowrule/),
  blog-confidence).

### 1.4 Native Wayland Steam client in 2026

**Does not exist.** VERIFIED (local binary): Steam UI platform layer is
`X11Context`; the client's `CWaylandContext` exists **only** to speak gamescope
protocols (`GAMESCOPE_WAYLAND_DISPLAY`, `GAMESCOPE_XWAYLAND_MODE_CONTROL`). No
ozone flags in Steam's own binaries; only bundled `libcef.so` (Chrome/126)
carries the ozone-wayland code, undriven. **`STEAM_ENABLE_WAYLAND`: NOT
VERIFIED — no evidence it exists anywhere.** (`PROTON_ENABLE_WAYLAND=1` is a
Proton/game setting, not client.) The CEF blocker was removed May 26 2026
(ANGLE Wayland merge): [Phoronix](https://www.phoronix.com/news/ANGLE-Merges-Wayland)
— VERIFIED; future native client plausible but not shipping (INFERRED). Forcing
`--ozone-platform=wayland` breaks games:
[#12997](https://github.com/ValveSoftware/steam-for-linux/issues/12997) — VERIFIED.

### 1.5 Does BPM need gamescope? OSK?

- BPM runs standalone (no gamescope dependency in the startup path) — VERIFIED
  (local binary) — but controller navigation is degraded outside gamescope on
  wlroots per #8640 — VERIFIED.
- OSK: SteamOS sessions export `QT_IM_MODULE=steam` / `GTK_IM_MODULE=Steam` to
  surface the Steam keyboard
  ([ChimeraOS session file](https://github.com/ChimeraOS/gamescope-session-steam/blob/master/usr/share/gamescope-session-plus/sessions.d/steam))
  — VERIFIED; unset on a plain desktop ⇒ incomplete OSK integration INFERRED.
  Open OSK bugs:
  [#12920](https://github.com/ValveSoftware/steam-for-linux/issues/12920),
  [#13467](https://github.com/ValveSoftware/steam-for-linux/issues/13467),
  [#8782](https://github.com/ValveSoftware/steam-for-linux/issues/8782),
  [#11158](https://github.com/ValveSoftware/steam-for-linux/issues/11158) —
  VERIFIED. `steam://close/bigpicture` exists for scripted exit
  ([#12577](https://github.com/ValveSoftware/steam-for-linux/issues/12577)) — VERIFIED.

### 1.6 SteamOS recipe / Arch packages

- SteamOS-style client command: **`steam -gamepadui -steamos3 -steampal
  -steamdeck`** inside `gamescope-session-plus`, which appends
  `--steam -R <socket> -T <stats>` etc. — VERIFIED:
  [sessions.d/steam](https://github.com/ChimeraOS/gamescope-session-steam/blob/master/usr/share/gamescope-session-plus/sessions.d/steam),
  [gamescope-session-plus](https://github.com/ChimeraOS/gamescope-session/blob/master/usr/share/gamescope-session-plus/gamescope-session-plus).
  ~20 `STEAM_GAMESCOPE_*`/`STEAM_*` env vars unlock Deck features; all
  recognized by the 2026 binaries (cross-VERIFIED). `steamos-session-select` =
  `/usr/lib/os-session-select` hook, else `steam -shutdown` — VERIFIED.
- AUR (VERIFIED via AUR RPC): `gamescope-session-git` (2026-02),
  `gamescope-session-steam-git` (2026-02), `gamescope-session-steam-sk-git`,
  `steam-gamepadui-session-git`
  ([chenx-dust repo](https://github.com/chenx-dust/steam-gamepadui-session)) —
  usable on desktop Arch.
- Arch Wiki minimal session: `Exec=/usr/bin/gamescope -e -- /usr/bin/steam
  -tenfoot`; `-steamdeck` needed for BPM network panels but can softlock
  "Switch to desktop" — [Arch Wiki Steam](https://wiki.archlinux.org/title/Steam)
  — VERIFIED; matching real bug on Arch+NVIDIA:
  [#11749](https://github.com/ValveSoftware/steam-for-linux/issues/11749)
  (OPEN). NVIDIA needs `nvidia_drm.modeset=1`
  ([Arch Wiki Gamescope](https://wiki.archlinux.org/title/Gamescope)) — VERIFIED.

**Topic 1 verdict:** windowed `steam -gamepadui` on Hyprland works but inherits
#8640 (open, names Hyprland), #11255-class CEF performance issues, XWayland DPI
blur (needs `force_zero_scaling` + `Xft.dpi`), and no Wayland overlay (#8020).
`gamescope -f -e -- steam -gamepadui` as a nested window on a workspace
sidesteps all of these.

## TOPIC 2 — Global/conditional launch wrapping without per-game launch options

### 2.1 Global launch-options setting: does not exist

VERIFIED:
[#3453 "Allow global launch options"](https://github.com/ValveSoftware/steam-for-linux/issues/3453)
OPEN since **2014**, `reviewed` label, last comment 2026-04-11; dups closed
unimplemented: [#8495](https://github.com/ValveSoftware/steam-for-linux/issues/8495),
[#12478](https://github.com/ValveSoftware/steam-for-linux/issues/12478),
[#7684](https://github.com/ValveSoftware/steam-for-linux/issues/7684). Local
binary corroboration: only per-app `LaunchOptions`/`%command%` machinery exists
— VERIFIED.

### 2.2 `steam_dev.cfg`

Read at `CSteamEngine::Init`; sets internal cvars (download/network/shader/
site-license/metrics: `@nClientDownloadEnableHTTP2PlatformLinux`,
`unShaderBackgroundProcessingThreads`, etc.) — VERIFIED (local binary +
[Arch Wiki](https://wiki.archlinux.org/title/Steam)). Only Valve-documented key:
`@LocalContentServer`
([Steamworks docs](https://partner.steamgames.com/doc/sdk/uploading/local_content_server))
— VERIFIED. **No launch-wrapper/prefix key exists** — VERIFIED by exhaustive
negative search of the `steamclient.so` cvar namespace. Dead end.

### 2.3 Compatibility tool as the global hook — the answer

- Official spec (Valve/Collabora `steam-compat-tool-interface.md`):
  [retrievable mirror](https://github.com/xytovl/steam-runtime-tools/blob/feat/openxr/docs/steam-compat-tool-interface.md)
  (canonical gitlab.steamos.cloud is bot-gated). VERIFIED. Key fields:
  `compatibilitytool.vdf` (`install_path`, `display_name`, `from_oslist`,
  `to_oslist`); `toolmanifest.vdf` v2 (`commandline` with `%verb%`,
  `require_tool_appid`, `compatmanager_layer_name`,
  `filter_exclusive_priority`, `unlisted`, `use_tool_subprocess_reaper`).
  Search paths incl. `~/.steam/root/compatibilitytools.d` and
  `/usr/local/share/steam/compatibilitytools.d` (implemented from
  [#6310](https://github.com/ValveSoftware/steam-for-linux/issues/6310)) —
  VERIFIED (local binary).
- **Global default = `~/.steam/steam/config/config.vdf` → `CompatToolMapping` →
  key `"0"`** (`{"name" "<tool internal name>" "config" "" "Priority" "75"}`) —
  the scriptable equivalent of "Enable Steam Play for all other titles".
  Documented by [STL wiki](https://github.com/sonic2kk/steamtinkerlaunch/wiki/Steam-Compatibility-Tool);
  structure VERIFIED against local `config.vdf`. GUI selection of third-party
  tools as default reported flaky
  ([STL #278](https://github.com/sonic2kk/steamtinkerlaunch/issues/278#issuecomment-932826852));
  direct edit is the reliable route — VERIFIED (report) / INFERRED
  (recommendation). Edit with Steam closed — INFERRED.
- **Native Linux games: YES, v2 compat tools run for them.** Spec section
  "Native Linux Steam games": *"Version 2 compat tools are invoked with `%verb%`
  replaced by `waitforexitandrun`… The launch options are used."* — VERIFIED.
  Working precedent:
  [Scrumplex/Steam-Play-None](https://github.com/Scrumplex/Steam-Play-None) —
  `from_oslist "linux"` / `to_oslist "linux"`, `commandline "/launch.sh %verb%"`,
  `launch.sh` = `#!/bin/bash` + `exec "${@:2}"` — the minimal wrapper template.
  VERIFIED.
- **Gotcha:** `from_oslist "windows"` tools force the Windows depot (STL wiki
  states this explicitly) — use `linux→linux` for a wrapper that keeps native
  games native. VERIFIED (consequence) / whether one tool can declare both
  oslists NOT VERIFIED.
- Env passed to tools: `STEAM_COMPAT_APP_ID`, `STEAM_COMPAT_DATA_PATH`,
  `STEAM_COMPAT_CLIENT_INSTALL_PATH`, `STEAM_COMPAT_TOOL_PATHS`,
  `STEAM_COMPAT_LIBRARY_PATHS`, `STEAM_COMPAT_MOUNTS`,
  `STEAM_COMPAT_SHADER_PATH`, `STEAM_COMPAT_FLAGS`,
  `STEAM_COMPAT_LAUNCHER_SERVICE`, etc. — VERIFIED (spec + `steamclient.so`).
- ⚠️ Tool commandline runs in the scout LDLP env; escape with
  `"$STEAM_RUNTIME/scripts/switch-runtime.sh" --runtime="" --` before exec'ing
  host `gamescope` — VERIFIED (spec).
- User `%command%` launch options wrap the whole tool stack and still compose on
  top — VERIFIED (spec).

### 2.4 SteamTinkerLaunch

Global-default capable (`steamtinkerlaunch compat add`, internal name
`Proton-stl`, config.vdf `"0"`), gamescope support global or per-game
(`USEGAMESCOPE=1`, `GAMESCOPE_ARGS` —
[wiki](https://github.com/sonic2kk/steamtinkerlaunch/wiki/GameScope)) —
VERIFIED. **But:** native games not supported via compat-tool mode (forces
Windows depot) — VERIFIED; env-var/state-file driving undocumented
([#1235](https://github.com/sonic2kk/steamtinkerlaunch/issues/1235) OPEN) —
NOT VERIFIED; **maintenance red flag** — last release v12.12 (2023-03), yad 15
breaks all dialogs
([#1320](https://github.com/sonic2kk/steamtinkerlaunch/issues/1320) OPEN
2026-07) — VERIFIED. Not recommended in 2026.

### 2.5 Precedents

[Boxtron](https://github.com/dreamer/boxtron/blob/master/toolmanifest.vdf) (v1
manifest, `commandline_waitforexitandrun`/`_getcompatpath` variants),
[Roberta](https://github.com/dreamer/roberta),
[Luxtorpeda](https://github.com/luxtorpeda-dev/luxtorpeda) — all
`windows→linux`; Steam-Play-None — `linux→linux`; GE-Proton local manifest
(`/proton %verb%`, `require_tool_appid 4183110`) — all VERIFIED.

### 2.6 Other hooks

- `STEAM_COMPAT_LAUNCHER_SERVICE` — debugging-IPC sidecar (pressure-vessel
  `steam-runtime-launcher-service`), not a command prefix — VERIFIED (spec).
  Wrong tool.
- Editing `SteamLinuxRuntime_*/ (_v2-entry-point, run)` — files marked
  `// Generated file, do not edit`, depot-managed, reverted on update/verify —
  VERIFIED (local files) / non-durability INFERRED (high confidence).
- `PROTON_REMOTE_DEBUG_CMD` — launches msvsmon (VS remote debugger) in Proton
  prefix, registry-gated, Proton-only — VERIFIED (local binary context). Not a
  wrapper.
- `SteamLaunchWrapper` — **no such thing exists** (negative search of binaries +
  web) — NOT VERIFIED/negative.
- LD_PRELOAD/LD_AUDIT shims — Steam owns LD_PRELOAD for the overlay; known
  "Gamescope Lag Bomb" interaction
  ([Arch Wiki Gamescope](https://wiki.archlinux.org/title/Gamescope)) — fragile,
  not recommended (INFERRED).
- **[ScopeBuddy](https://github.com/HikariKnight/ScopeBuddy)** — best gamescope
  wrapper payload: global config `~/.config/scopebuddy/scb.conf`,
  `SCB_AUTO_RES/HDR/VRR`, fixes Steam Overlay + SteamInput in nested mode,
  self-disables inside gamescope-session, **wlroots supported via `wlr-randr`**;
  canonical home **OpenGamingCollective/ScopeBuddy** (206 stars, pushed
  2026-08-18); AUR `scopebuddy` v1.5.0-2 (2026-08-11, active) — VERIFIED
  (README + AUR RPC). Still needs a per-game `scb -- %command%` *unless* invoked
  from your global compat tool (combination INFERRED).

### 2.7 Scripting `localconfig.vdf` LaunchOptions

Plain-text VDF at `~/.steam/steam/userdata/<id>/config/localconfig.vdf`, path
`UserLocalConfigStore→Software→Valve→Steam→apps→<appid>→LaunchOptions` —
VERIFIED (local file). Feasible but inferior: Steam rewrites the file on exit
(must be closed — folklore consensus, primary citation NOT VERIFIED); entries
must be created per appid and re-run on new installs; launch options are not
cloud-synced (cloud sync is an open FR:
[#10475](https://github.com/ValveSoftware/steam-for-linux/issues/10475)).
Existing tooling —
[BlafKing/steam-global-launch-options](https://github.com/BlafKing/steam-global-launch-options),
a **Millennium** plugin (updated 2026-08-20) that deliberately intercepts at
runtime and restores options after launch instead of editing files — VERIFIED;
[Millennium](https://github.com/SteamClientHomebrew/Millennium) supports Linux,
AUR `millennium` v3.4.1 (2026-08-18) — VERIFIED; fragile across Steam client
updates — INFERRED.

### 2.8 Built-in desktop gamescope toggle: no

Desktop Steam has no gamescope launch toggle in 2026. The client's gamescope
strings (`CGamescope_*`, `gamepadui_via_gamescope`) are control-IPC for when
Steam already runs *inside* a gamescope session — VERIFIED (local binary).
Deck's "Game Resolution" settings appear only because the session exports
`STEAM_GAMESCOPE_*` capability vars — VERIFIED (session file) / INFERRED.
Community FR confirms absence:
[SteamClientBeta discussion](https://steamcommunity.com/groups/SteamClientBeta/discussions/3/3832045251557071140/).
No 2025/26 changelog adding one found — NOT VERIFIED that any exists.

## Recommended architecture (synthesis)

1. **BPM:** run `gamescope -f -e -- steam -gamepadui` (optionally + SteamOS
   `STEAM_*` env block and `QT_IM_MODULE=steam`/`GTK_IM_MODULE=Steam`) as a
   nested window on a Hyprland workspace — avoids #8640/#11255/DPI-blur. If
   windowed-native instead: `force_zero_scaling` + `Xft.dpi` via xrdb +
   `SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS=0` + GPU-accel webviews on.
2. **Global wrapper:** custom v2 compat tool modeled on Steam-Play-None
   (`linux→linux`, `launch.sh` reads an env var/state file, escapes scout
   runtime, optionally delegates to ScopeBuddy), installed in
   `~/.steam/root/compatibilitytools.d/`, set as default by writing the `"0"`
   entry in `config.vdf → CompatToolMapping` with Steam closed. Applies to
   native Linux and (chained) Proton titles; per-game `%command%` options still
   compose on top.
