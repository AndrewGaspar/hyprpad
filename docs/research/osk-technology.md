# Research: controller-driven OSK for Hyprland (consolidated agent report)

*Produced 2026-08-31. Verification basis: **VERIFIED(local)** = read from the
source tree of the compositor running on the target machine
(`/home/ajg/code/hypxrland`, Hyprland 0.56.2 branch `hypxrland`, tag
`v0.56.2-374-g67200a8383`, HEAD `ba5e361f8`) or installed binaries/packages.
**VERIFIED(code)** = read from the shipped Steam client bundle on this machine
(`~/.local/share/Steam/steamui/chunk~2dcc5aaf7.js`, bundle dated 2026-08-03,
`~/.local/share/Steam/steamui/css/chunk~2dcc5aaf7.css`,
`~/.local/share/Steam/controller_base/basicui_neptune.vdf` — the Deck and
desktop clients share this SteamUI codebase, so this is the actual OSK
implementation). **VERIFIED(web)** = confirmed in a cited upstream source.
**INFERRED** = reasoned, not directly confirmed.*

## 1. Wayland protocol landscape on Hyprland (Q1)

### 1.1 What Hyprland 0.56.2 actually implements

**VERIFIED(local)** — `src/managers/ProtocolManager.cpp:185-200`, advertised globals and versions:

| Protocol | Version | Impl file |
|---|---|---|
| `zwp_input_method_manager_v2` | 1 | `src/protocols/InputMethodV2.cpp` |
| `zwp_virtual_keyboard_manager_v1` | 1 | `src/protocols/VirtualKeyboard.cpp` |
| `zwp_text_input_manager_v3` | 1 | `src/protocols/TextInputV3.cpp` |
| `zwp_text_input_manager_v1` | 1 | `src/protocols/TextInputV1.cpp` |
| `zwlr_layer_shell_v1` | **5** | `src/protocols/LayerShell.cpp` |
| `zwlr_virtual_pointer_v1` | — | `src/protocols/VirtualPointer.cpp` |

- **`zwp_input_method_v2` is fully implemented**, including
  **`zwp_input_popup_surface_v2`** (`src/protocols/InputMethodV2.hpp:98`,
  `CInputMethodPopupV2`) and **`zwp_input_method_keyboard_grab_v2`**
  (`CInputMethodKeyboardGrabV2`).
- **No `text-input-v2` and no `text-input-v4`.** **INFERRED:** Qt/KDE apps
  configured for text-input-v2 expose no text input at all, so IME
  `commit_string` silently fails for them. Test before relying on the IME path
  for Qt apps.
- No permission gate on either `input-method` or `virtual-keyboard`
  (`ProtocolManager.cpp`), unlike screencopy. There *is* a
  `PERMISSION_TYPE_KEYBOARD` dynamic permission, but it defaults to allow
  (`src/managers/permissions/DynamicPermissionManager.cpp:222`: "keyboards are
  allow default").

### 1.2 IME popups render above everything, including the lock screen — but do not use them

**VERIFIED(local)** — `src/render/Renderer.cpp:2622-2627`:

```cpp
renderWorkspace(pMonitor, pMonitor->m_activeWorkspace, NOW, renderBox);
renderLockscreen(pMonitor, NOW, renderBox);
// render IME even above the lockscreen - allow the user to use it to potentially input stuff on it.
renderIME(pMonitor, NOW, renderBox);
```

**Fatal caveat:** IME popups are *not* in the solitary-blocker list (§2.3), so
they vanish over a solitary fullscreen client. Confirmed upstream as
[Hyprland #16010](https://github.com/hyprwm/Hyprland/issues/16010) — *"IME
candidate window (zwp_input_popup_surface_v2) not rendered on internally
fullscreen windows (still on 0.56.2)"*, **closed as not planned**, 2026-08-26.
The reporter notes layer rules cannot work around it because an IME popup is
not a layer surface.

Also, popup geometry is anchored to the focused text input's cursor rectangle
(`src/managers/input/InputMethodPopup.cpp:80-100`), not free-floating — the
wrong shape for a full-width Deck-style keyboard.

**Conclusion: render the OSK as a layer-shell surface, never as an IME popup.**

### 1.3 Hyprland IME issue history

**VERIFIED(web)**, GitHub search API on `repo:hyprwm/Hyprland`:

| # | Title | State | Date |
|---|---|---|---|
| [16010](https://github.com/hyprwm/Hyprland/issues/16010) | IME popup not rendered on internally fullscreen windows | **closed, not planned** | 2026-08-26 |
| [16003](https://github.com/hyprwm/Hyprland/issues/16003) | Support passing preedit rectangle to input methods | closed | 2026-08-25 |
| [15644](https://github.com/hyprwm/Hyprland/issues/15644) | Ensure input method popups are rendered above overlay layers | closed | 2026-07-29 |
| [5290](https://github.com/hyprwm/Hyprland/issues/5290) | Persistent IME candidate words / UI ghosting | closed | 2024-03-27 |
| [1871](https://github.com/hyprwm/Hyprland/issues/1871) | IME popup/cursor too small under XWayland HiDPI | closed | 2023-03-24 |
| [1706](https://github.com/hyprwm/Hyprland/issues/1706) | input-method-v2 popups sometimes out of screen | closed | 2023-03-05 |

The implementation is mature; the live defect relevant here is #16010,
sidestepped by not using IME popups for rendering.

### 1.4 What existing OSKs bind (summary; details §5)

- **wvkbd** — layer-shell (OVERLAY) + virtual-keyboard-v1 mandatory;
  input-method-v2 optional (`--auto`, show/hide only); xdg_wm_base required for
  its magnify popup. **VERIFIED(web/source)**.
- **squeekboard** — layer-shell + virtual-keyboard-v1 required, input-method-v2
  "strongly recommended" and used to **commit text** when present. Runs
  standalone on Hyprland/sway (no phoc dependency). **VERIFIED(web)**.
- **onboard** — X11-era, upstream dead since 1.4.1 (2017-02-16); an active fork
  ([onboard-osk/onboard](https://github.com/onboard-osk/onboard), v1.4.4-5,
  2026-08-05) adds experimental Wayland support via **gtk-layer-shell +
  `/dev/uinput`**, not virtual-keyboard-v1. **VERIFIED(web)**.
- Hyprland-community consensus OSK is wvkbd;
  [awesome-hyprland](https://github.com/hyprland-community/awesome-hyprland)
  lists exactly two OSKs: sysboard and wvkbd. **VERIFIED(web)**.

## 2. Rendering above fullscreen games and gamescope (Q2)

### 2.1 OVERLAY works; TOP does not — explicit, not incidental

**VERIFIED(local)** — `src/desktop/view/LayerSurface.cpp:336-343`:

```cpp
m_aboveFullscreen = NEW_LAYER >= ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY;
// if in fullscreen, only overlay can be above.
*m_alpha.get(LS_ALPHA_FADE) = Fullscreen::controller()->hasFullscreen(PMONITOR)
    ? (m_layer >= ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY ? 1.F : 0.F) : 1.F;
```

TOP layers are faded to alpha 0 under fullscreen; OVERLAY keeps alpha 1. The
paint order (`Renderer.cpp:1502-1562`) draws TOP then OVERLAY *after* both
`renderWorkspaceWindowsFullscreen()` and `renderWorkspaceWindows()`, with no
fullscreen branch. `renderLayer()` (`Renderer.cpp:1254-1265`) gates only on
`visible()` and the `above_lock` rule. `CLayerSurface::visible()`
(`LayerSurface.cpp:107`) is purely a mapped-state test.

**Use `ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY`. Never TOP.** Upstream is actively
making TOP strictly-below-fullscreen:
[#15937](https://github.com/hyprwm/Hyprland/issues/15937) (open draft PR,
2026-08-22, *"core/layers: Top layers go below fullscreen windows"*, adds
`misc:allow_new_top_layers_over_existing_fullscreen`). Related history:
[#11658](https://github.com/hyprwm/Hyprland/issues/11658) (closed 2025-09-10),
[#2686](https://github.com/hyprwm/Hyprland/issues/2686) (closed 2023-07-12),
[#14735](https://github.com/hyprwm/Hyprland/issues/14735) (closed 2026-05-21).
**No open issue reports OVERLAY failing over fullscreen.**

### 2.2 XWayland fullscreen / override-redirect

**VERIFIED(local)** — an override-redirect X11 surface is an ordinary
`CWindow`; there is no separate always-on-top pass for OR windows
(`src/xwayland/` + renderer read). They are drawn inside the window passes,
which run before the TOP/OVERLAY loops. An OVERLAY layer surface draws over an
XWayland fullscreen game exactly as over a native one.

### 2.3 The direct-scanout trap — design around it

**VERIFIED(local)** — `src/output/Monitor.cpp:2245-2248` (`isSolitaryBlocked()`):

```cpp
if (!m_layerSurfaceLayers[ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY].empty()) {
    reasons |= SC_OVERLAYS;
```

Note the asymmetry: **OVERLAY blocks on mere existence** — mapped or not, alpha
irrelevant — whereas the TOP check immediately below tests
`alpha()[LS_ALPHA_FADE]->value() != 0.F`. `recheckSolitary()`
(`Monitor.cpp:2296`) then never selects a solitary client, and
`canAttemptDirectScanoutFast()` (`Monitor.cpp:2627`) fails.

Consequences:

- **Good:** the OSK can never be invisible-because-scanout; its existence forces
  composition.
- **Bad:** an idle OVERLAY surface **permanently disables direct scanout and
  tearing for every fullscreen game on that monitor**.

> **Design requirement: destroy the `zwlr_layer_surface_v1` on dismiss.** Do
> not merely hide it, shrink it, or set alpha 0.

Debug aid, **VERIFIED(local)**: `hyprctl monitors -j` exposes `solitary`,
`solitaryBlockedBy`, `directScanoutTo`, `directScanoutBlockedBy` (observed live
on this machine). Use these to confirm the OSK leaves scanout unblocked when
hidden.

### 2.4 Lock screen and layer rules

**VERIFIED(local)** — the `above_lock` layer rule is an int 0/1/2:
- `1` → renders above the session lock (`Renderer.cpp:1262`);
- `2` → additionally hit-testable / receives input
  (`src/desktop/state/ViewHitTester.cpp:349`:
  `aboveLockscreen && ...aboveLock() != 2` → skip).

`above_lock = 2` is exactly what a lock-screen OSK needs. **INFERRED:** since
the OSK is driven entirely over IPC by the hyprsc daemon and needs no Wayland
pointer input, `above_lock = 1` may suffice; `2` only if touch/pointer on the
OSK is wanted.

Full layerrule effect set, **VERIFIED(local)** (`src/desktop/rule/layerRule/`):
`order`, `above_lock`, `blur`, `blur_popups`, `depth`, `dim_around`,
`ignore_alpha`, `animation`, `no_anim`, `no_screen_share`, `xray`. **There is
no `ignorezero`** on this build — it became `ignore_alpha` (float). No layer
rule can change which of the four tiers a surface occupies; that is chosen
client-side at `get_layer_surface()`. Set `no_screen_share` so the OSK doesn't
leak into screen captures.

### 2.5 gamescope — renders under you, but you cannot type into it with host protocols

**VERIFIED(local, binary inspection of `/usr/bin/gamescope` 3.16.25 + fetched
source):**

- gamescope exposes `zwlr_layer_shell_v1` to *its own* nested clients, and the
  private `gamescope_input_method` / `gamescope_input_method_manager`.
- It has **no** `zwp_virtual_keyboard_v1`, **no** `zwp_input_method_v2`, **no**
  `zwp_text_input_*` (the only `virtual_keyboard` string is the internal
  `wlserver.wlr.virtual_keyboard_device`).
- It builds its nested seat's keymap from `XKB_DEFAULT_*` env vars
  (`xkb_keymap_new_from_names`) — **its own keymap, not the host's**.

**VERIFIED(web)** — gamescope's Wayland backend discards the host keymap
outright (`src/Backends/WaylandBackend.cpp`: *"We are not doing much with the
keymap, we pass keycodes thru"*); long-open
[gamescope#203](https://github.com/Plagman/gamescope/issues/203) *"Import
keyboard layout from parent session in nested mode"* (open since 2021).

**Consequence — decisive:** the dynamic-keymap-swap technique is **dead through
nested gamescope**. You upload a host keymap saying "keycode 24 = é"; Hyprland
forwards the keycode; gamescope resolves it against its own `us` map; the game
receives `o`. QWERTY garbage.

**The supported path** — **VERIFIED(local)** from
`protocol/gamescope-input-method.xml` (interface version 3):

- `gamescope_input_method_manager.create_input_method(seat, id)`
- Requests: `set_string(utf8)`, `set_action(enum)`, `commit(serial)`
  (double-buffered; serial from the `done` event)
- Actions: `none, submit, delete_left, delete_right, move_left, move_right,
  move_up (v2), move_down (v2)`
- Since v3: `pointer_motion(dx,dy)`, `pointer_warp(x,y)`,
  `pointer_wheel(x*120,y*120)`, `pointer_button(linux_button_code, state)` —
  annotated *"Collection of pointer/button related things for Steam Input."*
- `unavailable` event if another input method already exists on the seat (*"No
  more than one input method must be associated with any seat at any given
  time"*).

The XML says *"This is a private Gamescope protocol. Regular Wayland clients
must not use it"* — but **VERIFIED(local)** `src/ime.cpp:647` creates it with a
plain `wl_global_create` on the main nested display, with no privileged-socket
filter. Any gamescope client can bind it.

**A working reference client ships on this machine: `/usr/bin/gamescope-type`**
(part of the `gamescope` package; **VERIFIED(local)** — strings contain
`gamescope_input_method_manager` and `GAMESCOPE_WAYLAND_DISPLAY`; upstream
`src/Apps/gamescope_type.c`, *"Based on wl-ime-type by Simon Ser"*).

**Architecture implication — two injection backends are mandatory:**

```
OSK = Hyprland OVERLAY layer surface (rendering, always)
  ├── target is a normal Hyprland client → zwp_virtual_keyboard_v1 on the host seat
  └── target is a gamescope window       → second wl_display connect to $GAMESCOPE_WAYLAND_DISPLAY
                                           (gamescope-N socket in $XDG_RUNTIME_DIR),
                                           bind gamescope_input_method_manager,
                                           set_string()/set_action() + commit(serial)
```

Detect via `hyprctl activewindow -j` (`class` = `gamescope`) or presence of the
`gamescope-N` socket.

Related: [gamescope#1067](https://github.com/ValveSoftware/gamescope/issues/1067)
*"Is there a way to use fcitx5/ibus in gamescope?"* (open, 2026-01-23) —
confirms no standard IME support.
[gamescope#668](https://github.com/ValveSoftware/gamescope/issues/668) (closed)
— games must use Valve's `ShowGamepadTextInput`; `SDL_StartTextInput` does not
work on Deck.

## 3. Injecting text (Q3)

### 3.1 The three injection paths and where each dies

| Target | IM-v2 `commit_string` | vkbd-v1 + keymap swap | `gamescope_input_method` | uinput/ydotool |
|---|---|---|---|---|
| Wayland app with text-input-v3 | ✅ clean UTF-8, no race | ✅ | n/a | ⚠️ ASCII/US-layout only |
| Wayland app without text-input-v3 (most games) | ❌ **silently dropped** | ✅ | n/a | ⚠️ |
| XWayland app | ❌ **silently dropped** | ⚠️ works in principle; documented failures (§3.4) | n/a | ⚠️ |
| App inside nested gamescope | ❌ not exposed | ❌ **keymap discarded → garbage** | ✅ **only working path** | ⚠️ works (below compositor) but ASCII-only |

**XWayland has no text-input support at all — VERIFIED(local):**
`strings /usr/bin/Xwayland` (xorg-xwayland 24.1.13) contains zero matches for
`zwp_text_input`, `text_input_manager`, or `input_method`; `Xwayland -help`
shows no IME flag. There is no XIM bridge.

**Hyprland silently drops IME commits when focus is XWayland —
VERIFIED(local):** `CInputMethodRelay::getFocusedTextInput()`
(`src/managers/input/InputMethodRelay.cpp:73-88`) returns `nullptr` when the
focused surface owns no text-input object, and the IME commit listener
(`InputMethodRelay.cpp:28-37`) just logs *"No focused TextInput on IME
Commit"*. The IME is never even `activate()`d for such surfaces.

**Games are overwhelmingly SDL/Unity/Unreal and do not implement
text-input-v3.** The IME `commit_string` path is unusable for the primary
target regardless of XWayland. Use it only as an opportunistic fast path for
well-behaved Wayland apps.

### 3.2 The keymap-swap recipe — gamescope's `ime.cpp` is the reference implementation

**VERIFIED(local)**, read from gamescope source `src/ime.cpp` — the same code
backing the Steam Deck OSK, encoding years of workarounds:

1. **Try the current keymap first.** `try_type_keysym()` (ime.cpp:313)
   brute-force scans every (keycode, layout, level) of the *existing* keymap
   for the target keysym reachable with only Shift/Ctrl/Alt; if found, type it
   with **no keymap change**. Only generate a keymap otherwise (`type_text()`,
   ime.cpp:363).
2. **Restrict synthetic keycodes to real character keys.** gamescope keeps an
   `allow_keycodes[]` list (`KEY_1..KEY_0`, `KEY_Q..KEY_RIGHTBRACE`,
   `KEY_A..KEY_BACKSLASH`, `KEY_Z..KEY_SLASH`, 46 entries), commented *"Some
   clients assume keycodes are coming from evdev and interpret them."* Never
   use keycode 127/`<MENU>` (see wvkbd bug, §5.2).
3. **Release held keys before the keymap change**
   (`release_key_if_needed(ime); // before keymap change`, ime.cpp:346/393).
4. **Timing:** key held **30 ms** (ime.cpp:310); generated keymap reset only
   after **100 ms idle** (ime.cpp:402), deliberately deferred: *"resetting it
   immediately is racy: clients will interpret the keycodes we've just sent
   with the new keymap."*
5. **Xwayland ignores the event `time` field** (comment in `press_key`); pass
   `time = 0`.
6. **Codepoint workarounds** in `keysym_from_ch()` (ime.cpp:127): Euro-sign
   libxkbcommon bug; Hangul Jamo ranges forced to raw `ch | 0x01000000`
   Unicode keysyms (CEF rejects the named keysyms).
7. Generated keymap format (ime.cpp:183-258): a minimal
   `xkb_keymap { xkb_keycodes … key <K%u> {[ %s ]}; … }` with
   `xkb_types "(unnamed)" { include "complete" };` and
   `xkb_compatibility "(unnamed)" { include "complete" };`, compiled with
   `xkb_keymap_new_from_buffer`.

**The serialisation trap — Hyprland re-serialises your keymap.
VERIFIED(local):** `src/devices/IKeyboard.cpp:159-163` — Hyprland compiles your
uploaded keymap text, then re-emits it via `xkb_keymap_get_as_string()` (both
TEXT_V1 and TEXT_V2 forms) and ships *that* string to clients. This round-trip
is the documented cause of
[gamescope#2311](https://github.com/ValveSoftware/gamescope/issues/2311)
(**open**, 2026-08-08): *"committed text showing as digits"* — xkbcommon's
serialiser drops interprets, xkbcomp then rejects the empty compatibility
section, and all symbols are lost.

> **Design requirement:** every generated keymap must retain at least one
> `interpret`-generating entry that survives re-serialisation — include a real
> modifier key (NumLock, or Shift/Ctrl/Alt) alongside the synthetic character
> keys.

**Diagnostic fingerprint:** output like `^[1234567890- qwertyuiop[` = evdev
keycodes 9,10,11… resolved against a stock `us` map. The keymap did not land —
debug keymap propagation, not the OSK.

**Also VERIFIED(local):** Hyprland's `CVirtualKeyboardV1Resource`
(`src/protocols/VirtualKeyboard.cpp`) enforces the protocol error
`ZWP_VIRTUAL_KEYBOARD_V1_ERROR_NO_KEYMAP` on key/modifier events before a
keymap upload — upload the keymap first, always.

### 3.3 Two Hyprland-specific hazards, with clean fixes

**Hazard A — synthetic keys run through the keybind manager.**

**VERIFIED(local)** — `src/managers/input/InputManager.cpp:1663-1667`:

```cpp
bool passEvent = DISALLOWACTION && !PROTO::inputCapture->isCaptured();
if (!DISALLOWACTION)
    passEvent = g_pKeybindManager->onKeyEvent(event, pKeyboard) && ...;
```

For an ordinary virtual keyboard `DISALLOWACTION` is false → **Hyprland
keybinds fire on your synthetic keystrokes** (typing `q` while `SUPER+Q` — or
any bare-key bind — exists can trigger dispatchers).

Fixes, both **VERIFIED(local)**:

1. **Config route.** `misc:name_vk_after_proc` defaults **true**
   (`src/config/values/ConfigValues.cpp:501`), naming the device
   `hl-virtual-keyboard-<binaryname>`. Per-device config sets `m_allowBinds`
   (`InputManager.cpp:1253/1261`), honoured at
   `src/managers/KeybindManager.cpp:126,145,355`:
   ```
   device { name = hl-virtual-keyboard-<osk-binary>  keybinds = false }
   ```
2. **Protocol route (cleaner).** `shouldIgnoreVirtualKeyboard()`
   (`InputManager.cpp:1763-1778`) returns true when the virtual keyboard's
   `wl_client` **is the same client that holds the IME keyboard grab**
   (`m_relay.m_inputMethod->grabClient() == CLIENT`). That sets
   `DISALLOWACTION = true`, which **bypasses the keybind manager entirely and
   sends the key straight to the seat**.

   > If the OSK binds `zwp_input_method_v2`, takes a
   > `zwp_input_method_keyboard_grab_v2`, *and* creates its own
   > `zwp_virtual_keyboard_v1` from the same client, its synthetic keystrokes
   > bypass Hyprland keybinds and reach the focused app directly, with no
   > feedback loop into its own grab. This is the cleanest architecture — **but
   > it collides with fcitx5 (§7), so it must be optional.**

   Cost: while the grab is held, *physical* keyboard events route to the IME
   instead of the app (`USEIME` branch, `InputManager.cpp:1688-1691`), so the
   OSK must forward them while shown. Hyprland's own keybinds still work from
   the physical keyboard (keybind dispatch runs before the IME branch).

**Hazard B — the seat's keymap follows the last keyboard that sent a key, and
is never restored.**

**VERIFIED(local)** — `InputManager.cpp:1704` calls
`g_pSeatManager->setKeyboard(pKeyboard)` on **every** key event;
`CSeatManager::setKeyboard()` (`src/managers/SeatManager.cpp:161-178`) →
`updateActiveKeyboardData()` → `PROTO::seat->updateKeymap()`, broadcasting the
active keyboard's keymap to every client `wl_keyboard`, including Xwayland's.
Nothing switches back except keyboard destruction or a key event from another
device.

For a controller-only user who never touches a physical keyboard, **the game is
left on your synthetic keymap indefinitely after typing** — WASD and hotkeys
broken.

> **Design requirement: destroy the `zwp_virtual_keyboard_v1` after each typing
> burst** (Hyprland removes the device, and the seat reverts on the next real
> event — this is exactly why `wtype`-as-a-subprocess behaves and a long-lived
> VK does not), or explicitly re-upload the user's original keymap.

### 3.4 Known real-world failures on this path

**VERIFIED(web):**
- [wtype#62](https://github.com/atx/wtype/issues/62) *"Can't use to type
  anything on XWayland windows"* — **open**, 2024-08-19, reported **on
  Hyprland**; native Wayland fine, Electron/Chrome fail. No diagnosis.
- [wtype#60](https://github.com/atx/wtype/issues/60) — on Hyprland,
  `wtype "abcdefg"` produced `123456` (the §3.2 fingerprint).
- [wtype#71](https://github.com/atx/wtype/issues/71),
  [#72](https://github.com/atx/wtype/issues/72),
  [#31](https://github.com/atx/wtype/issues/31) — Chromium/Electron cache
  keymaps client-side; breaks after ~14 unique characters and on `å/ä/ö`.
- Hyprland #15897 — `hyprctl send_key_state` breaks with minimal keymaps
  (same serialisation family as gamescope#2311).
- wvkbd's Unicode fallback presses keycode 127, which Electron interprets as
  ContextMenu — [wvkbd#89](https://github.com/jjsullivan5196/wvkbd/issues/89),
  open, reproduced on Hyprland.

**wtype locally:** version 0.4-2 installed; **VERIFIED(local)** its strings
contain `zwp_virtual_keyboard_v1` and an embedded `xkb_keymap {` template.
Upstream [atx/wtype](https://github.com/atx/wtype): MIT, C, 556 stars, last
push 2024-04-27; bundles exactly one protocol XML
(virtual-keyboard-unstable-v1); one-shot auto-releasing modifiers (the clean
fix for stuck-Super).

**ydotool** — weakest path: hardcoded 128-entry ASCII→US-QWERTY table in
`Client/tool_type.c` (**no Unicode**, layout-dependent), needs `/dev/uinput`
access (root or `input`/udev rule). Its one advantage: it sits below the
compositor, so it works identically inside nested gamescope — ASCII only.

## 4. Steam Deck OSK — code-verified interaction spec (Q4)

All `[CODE]` items read from the shipped SteamUI bundle / `basicui_neptune.vdf`
on this machine (see header). Third-party corroboration: the Windows
reimplementation [neptune-osk](https://github.com/vilmire/neptune-osk) uses the
identical model (default overlap 55%).

### 4.1 Dual-trackpad typing — the mapping is ABSOLUTE. VERIFIED(code)

The deminified handler:

```js
n = (e.inputScale ?? 1) * ep.O.TrackPadTypingInputScale;   // "Trackpad Sensitivity"
// per analog pad event (i = padX, a = padY ∈ [-1,1]):
let t = .5 * (1 + clamp(i * n, -1, 1)),      // normalized X within region
    o = .5 * (1 - clamp(a * n, -1, 1)),      // normalized Y, inverted
    d = s.current,                            // DOMRect of this pad's region
    u = d.left + d.width  * t,
    m = d.top  + d.height * o;
e.fnCallback(true, u, m, r);
window.setTimeout(c, 100);                    // no event for 100 ms -> cursor deactivates
```

Key selection is literal hit-testing: `document.elementFromPoint(x,y)` filtered
on `data-key`. **No velocity, no accumulator — pure absolute position map.**
Sensitivity is a gain applied *before* clamping: it expands reach, not cursor
speed.

**Region geometry** (shipped CSS): each `TrackpadRegion` is
`width:55%; top:0; bottom:0`; `.LeftTrackpad { left:0 }`,
`.RightTrackpad { right:0 }` → **left pad = leftmost 55% x full height, right
pad = rightmost 55% x full height, 10% overlap band in the middle.** During an
extended-character popup, an `.ExtendedRowTrackpad` region of `width:100%` is
mounted for the initiating pad.

**Steam Input side** (`basicui_neptune.vdf`, preset "Keyboard", action set "On
Screen Keyboard"): both pads `mode "joystick_move"` with `virtual_mode "1"`
(absolute pad position as analog axis, not relative mouse); `haptic_intensity
"0"` (input-layer haptics off — the OSK does its own); actions
`LEFTPAD_ANALOG → "Left Cursor"`, `RIGHTPAD_ANALOG → "Right Cursor"`.

**Cursor visual:** 30x30 px SVG, `opacity:.5`, `stroke-width:3`, colors from
`--key-pointer-stroke-color`/`--key-pointer-background-color`;
`transform:scale(0.8)` while pressed; auto-hides 100 ms after the pad stops
reporting. Updates at 60 Hz
([Valve, 2022-09-23](https://store.steampowered.com/news/app/1675200/view/3308480236507403332)).

**Corroborating VERIFIED(web):** Valve changelog 2022-04-05 *"Updated bounds
for dual trackpad typing so that users can reach the extents of the keyboard
more easily"*
([announcement](https://steamcommunity.com/ogg/1675200/announcements/detail/3215014689194345915));
user report making absoluteness unambiguous — sensitivity increase lets the
left pad *"cross over the half way point"*
([discussion](https://steamcommunity.com/app/1675200/discussions/1/3319736698844418797/));
*"as long as you nailed the position of your finger on the trackpad as you were
pressing down, it would input the correct letter"*
([discussion](https://steamcommunity.com/app/1675200/discussions/0/3269061071550699568/)).

### 4.2 Commit — trackpad click-DOWN. VERIFIED(web + code)

Valve changelog 2022-04-01: *"Keyboard input is now sent on trackpad click
down, instead of up, which should improve accuracy for fast trackpad
typists."*
([announcement](https://steamcommunity.com/ogg/1675200/announcements/detail/3215014689176368944))

`[CODE]` `HandleTrackpadClick(button, bDown)` synthesizes an A-press on
whatever key is under *that pad's* cursor (`LPAD_CLICK`/`RPAD_CLICK`, and
`TRIGGER_LEFT/RIGHT` when trigger-click is enabled and that pad is touched).
Ordinary keys commit immediately on down; keys owning an extended-character
popup or tintable emoji commit on **up** (or open the popup at 450 ms).
Rollover (changelog 2022-07-14): pressing a key while holding another commits
the held key immediately (`if (this.state.holdTarget) { this.TypeKey(...) }`).

**Trigger commit is opt-in:** setting *"Enable Trigger Click"* — *"Use Left and
Right Triggers to select highlighted keys under left and right cursors"*
(`keyboard_trackpding_typing_trigger_as_click`); the trigger acts as commit
only when the setting is on **and** that pad is currently touched; otherwise
**L2 = Shift-hold, R2 = Enter**. Bumpers are not commit (§4.4). The physical
pad click is a soft press with a configurable pressure threshold (Big Picture
Configuration → "On Screen Keyboard" action set).

### 4.3 Haptics. VERIFIED(code)

Two distinct paths:

- **(a) Per-key-crossing Tick, trackpad only.** The hovered key's rect is
  cached; `elementFromPoint` re-runs only when the point exits it. If the
  resolved element changed: move the `.Focused` highlight and
  `PlayHaptic(source, leftPad|rightPad, HapticType.Tick, intensity 1, gain 0)`
  — one fixed-strength tick on *that* pad per hit-target change. **"No key" (a
  gap) also counts as a change**, producing the notorious double-thunk
  ([user complaint](https://steamcommunity.com/app/1675200/discussions/2/5135803832905185147/)
  — old BPM keyboard gave one thunk per key). Valve's mitigations: stronger
  crossing haptics (2022-04-05) and **removing inter-key gaps** — the CSS
  confirms `gap: 0` on keyboard and rows.
- **(b) Key-press haptic** exists only on the touchscreen path
  (`HandleTouchStart` → user preset); trackpad clicks get their feel from the
  physical click (Valve removed the extra haptic there, changelog 2022-04-25).

Preset table (`HapticType { Tick=1, Click=2 }`): Off = (0,0,0); Low = (Tick, 2,
-2 dB); Medium = (Click, 4, -5); High = (Click, 3, -3); Custom is Valve-gated.
API: `SteamClient.Input.TriggerSimpleHapticEvent(idx, target, eType,
unIntensity, ndBGain)`; legacy fallback
`TriggerHapticPulse(idx, target, 360, 0)`. Key presses also play a typing
nav-sound.

### 4.4 D-pad / stick mode and complete button map. VERIFIED(code)

Discrete DOM focus, not a cursor: the keyboard is `role="grid"`; nav uses
SteamUI's focus tree with `navEntryPreferPosition: MAINTAIN_X` (vertical moves
keep the column). Initial focus row 2, column 5 (the `G` key).

| Input | Action |
|---|---|
| **A** | Press the focused key |
| **B** | Back / dismiss (bound on **release**) |
| **X** | **Backspace**, auto-repeat 450 ms then every 200 ms |
| **Y** | **Space** |
| **L2** | **Shift held** (momentary) — unless Trigger Click on + left pad touched |
| **R2** | **Enter** — unless Trigger Click on + right pad touched |
| **L1 / R1** | IME candidate list up/down + emoji/Steam-item category prev/next |
| **L3** | **Caps Lock toggle** |
| **Start** | Move keyboard (`RotateWindowPosition()`) |
| **Select** | Chat radial menu |
| **Pad clicks** | Commit key under that pad's cursor |
| **D-pad + left stick** | Move focus |

Warning: the action-set *labels* in `basicui_neptune.vdf` differ
(LSHOULDER→Backspace, X→Contextual Action, Start→Submit Text, …) —
stale/indirect naming; the OSK's own handler implements the table above,
matching all user-facing descriptions
([howtogeek](https://www.howtogeek.com/898672/how-to-use-and-customize-the-virtual-keyboard-on-your-steam-deck/)).
Trust the code table.

**On-screen legend:** hints are rendered **as controller glyphs on the keycaps
themselves** — Y on Space, X on Backspace, R2 on Enter, L2 on both Shifts, L3
on CapsLock (`leftActionButton`/`rightActionButton` in the layout definitions);
glyphs always shown except VR; `FilterButtonForTrackpad` **hides the L2 glyph
while the left pad is touched and the R2 glyph while the right pad is
touched** (because their meanings change).

### 4.5 Mode switching — there is none. VERIFIED(code)

No mode variable exists. All paths are concurrently live: d-pad/stick move DOM
focus (`.gpfocus`); each pad independently sets `bLeft/RightTrackpadActive` and
paints its own `.Focused` on the key under its cursor; touchscreen multi-touch
is tracked separately. Three different keys can be highlighted simultaneously.
The only mode-like behaviors are cosmetic/consequential: 100 ms pad-cursor
auto-hide, and the L2/R2 glyph/meaning flip while the corresponding pad is
touched. **Parity means concurrent modality with per-source highlights, not
exclusive auto-switching.**

### 4.6 Other mechanics. VERIFIED(code) unless noted

- **Shift/Caps:** bitfield `{Off=0, OneShot=1, Stuck=2, Held=4}`. Tapping Shift
  cycles Off→OneShot→Stuck→Off; after any character, OneShot→Off (Held bit
  preserved). CapsLock (and L3) toggles Off↔Stuck. L2 = Stuck on press, Off on
  release. Physical hold adds Held (chording works). CSS states `ShiftActive`,
  `ToggleOn`, `ToggleOneShot`.
- **Long-press / accents:** `s_longPressThreshold = 450 ms`, repeat 200 ms.
  Keys carry `extended_keys` (e.g. `{key:",", extended_keys:"，,<"}`); 450 ms
  hold opens the extended-character row popup (extends left or right),
  addressable by **either pad over the full keyboard width** while open.
  Backspace shares the 450/200 timer as auto-repeat.
- **Layers:** `Standard / Numeric / Emoji / SteamItems`. **No symbols layer** —
  symbols live on Shift of the same keys. Meta keys include Emoji, Globe
  (layout switch; hidden if only one layout enabled), ABC, AltGr, Paste,
  Close/Done, Move, arrows (arrows added 2023-01-17).
- **Exact QWERTY grid + PC scancodes** (useful for an evdev implementation):
  ```
  r0: [` ~](2xHalf) 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) -_ =+ Backspace
  r1: Tab q w e r t y u i o p [{ ]} \|
  r2: CapsLock a s d f g h j k l ;: '" Enter
  r3: LShift z x c v b n m ,< .> /? RShift
  r4: Emoji Globe Space LEFT-UP RIGHT-DOWN Paste Close/Done Move
  keycodes: [41,2..14][15..27,43][58,30..40,28][42,44..54][0,0,57,75,77,0,0]
  ```
  Shipped layouts: qwerty, dvorak, colemak, 26+ localized, CJK IMEs (Pinyin,
  Zhuyin, Cangjie, Sucheng, Kana, Korean; via IBus on desktop, SteamOS 3.3).
- **No text prediction row** (negative, VERIFIED — no prediction UI in the
  bundle; only CJK IME candidates, navigated with L1/R1).
- **No magnifier/zoom on the hovered key** (negative, VERIFIED — `.Focused` is
  a theme-colored glow `box-shadow: 0 0 5px 1px <accent>, inset 0 0 8px 1px
  <accent>` with pulse animation; `.Touched` adds an accent fill + 0.3 s
  expanding "shine" `::after`. Steam's "Magnifier" is an unrelated whole-screen
  accessibility feature).
- **Invocation:** STEAM+X in Game and Desktop mode; the chord fires on
  **release** (changelog 2022-08-17). While mounted the OSK calls
  `SteamClient.Input.SetKeyboardActionset(true, …)` on mount **and re-asserts
  it every 1000 ms via a watchdog**, and enables analog input messages.
- **Positioning:** the OSK never resizes/moves the app — it moves itself. In
  gamescope: 2 slots (`center-bottom`, `center-top`); desktop: 6 (adds
  corners); offset 10 px. Start cycles the slot. Modal use:
  `SetTextFieldLocation(x,y,w,h)` records the app's declared field rect and
  `SelectBestModalPosition()` jumps to the best non-overlapping slot. Games
  declare it via `ISteamUtils::ShowFloatingGamepadTextInput(mode, x, y, w, h)`
  (modes SingleLine/MultipleLines/Email/Numeric) —
  [API docs](https://partner.steamgames.com/doc/api/ISteamUtils). Max-width
  1280 px (changelog 2022-11-10).

### 4.7 Themes. VERIFIED(code + web)

A theme is **a CSS class on the keyboard root plus CSS custom properties —
nothing more.** `GetKeyboardThemeClassName()` returns the equipped skin name or
`"DefaultTheme"`. Per-key hooks `KeyTheme_<key>`, per-row/col `Row_<n>` /
`Col_<n>`.

- **20 built-in theme classes** in the shipped CSS: DefaultTheme, Candy,
  Celebration, Cerulean, DEX, Digital, Evolve, Grape, LimitedEditionWhite,
  Lounger, NightShift, OLED, Pumpkin, Ruby, Seafoam, Spectrum, SteamGreen,
  TestChamber, TotallyTubular, TwoTone.
- **53 themeable custom properties**: `--background-color`,
  `--foreground-color`, and `--key-*` variants covering
  background/color/focused/touched/glow for every key class (backspace, caps,
  deadkey, emoji, enter, extendedkey, meta, shift, spacebar, spacer, tab,
  toggleon, toggleoneshot) plus `--key-pointer-background-color` /
  `--key-pointer-stroke-color` (the trackpad cursor) and
  `--key-action-button-glyph-color` (the keycap glyph legend).
- Themes may add gradients, a container border, and per-theme
  `.Focused`/`.Touched` keyframe animations (`glow`, `pulse`, `shine`,
  `night-shift-click`). **They cannot change layout or geometry.**
- Distribution: Steam Points Shop inventory items (~5,000 points,
  [shop](https://store.steampowered.com/points/shop/c/keyboard)); the CSS ships
  with the client, ownership unlocks the class name; requires being online to
  change. The 1TB OLED ships an exclusive theme
  ([steamdeck.com/en/tips](https://www.steamdeck.com/en/tips)).

This is a directly copyable theming model: root class + custom-property table.

### 4.8 Prior art: Daisywheel

Valve's Big Picture radial keyboard (*"QWERTY is for keyboards. Daisywheel is
for controllers."* —
[store.steampowered.com/bigpicture](https://store.steampowered.com/bigpicture/)).
Mechanics are **INFERRED/weakly sourced** — no Valve spec found; community
descriptions: radial wheel of character clusters, stick deflection picks the
cluster, a face button picks the character within it
([2013 Steam Universe discussion](https://steamcommunity.com/groups/steamuniverse/discussions/2/666826166396505929/)).
The modern client no longer ships Daisywheel resources. Treat petal
count/button assignment as unverified; non-goal.

### 4.9 The 12-point implementation checklist (distilled)

1. Two overlay regions over the keyboard: **left = `[0, 0.55W] x [0, H]`,
   right = `[0.45W, W] x [0, H]`**.
2. Read each pad's absolute position normalized to `[-1,1]`;
   `p = 0.5*(1 ± clamp(v*scale, -1, 1))`; map into that pad's rect; hit-test
   the key. Expose `scale` as "Trackpad Sensitivity" (default: 1.0 reaches
   exactly the region edge).
3. Cache the hovered key's rect; only re-hit-test when the point exits it. On
   key change: move that pad's highlight **and** fire one haptic tick on that
   pad. **Suppress the tick when the new target is "no key"** (fixing Valve's
   double-thunk).
4. Commit on **pad click-down**; optionally on L2/R2 full-press when "Trigger
   Click" is enabled *and* that pad is touched. Ordinary keys commit on down;
   extended-popup keys commit on up or open the popup at 450 ms.
5. Deactivate a pad cursor 100 ms after its last analog sample; hide its
   cursor.
6. Keep d-pad/stick focus, both pad cursors, and touch **all live
   simultaneously** — up to three independent highlights.
7. Glyph legend on the keycaps: Y=Space, X=Backspace, R2=Enter, L2=Shifts,
   L3=Caps. Hide the L2 glyph while the left pad is touched, R2 while the
   right pad is touched.
8. Shift: one-shot on first tap, sticky on second, off on third; one-shot
   auto-clears after one character. L2 = momentary shift. L3 = caps lock.
9. Backspace auto-repeat 450 ms → 200 ms; long-press 450 ms opens the
   extended-character popup, addressable by **either** pad over the **full**
   keyboard width.
10. Theme via CSS-custom-property-equivalent token table on a root theme name +
    per-key/row/col hooks. No zoom/magnifier — glow + press flash only.
11. `gap: 0` between keys; keyboard max-width ~1280 px.
12. Reposition the keyboard itself (2 slots fullscreen, 6 on desktop) to avoid
    the focused text field; never resize the app window.

## 5. Existing projects (Q5)

### 5.1 Headline

**No OS-level gamepad-driven on-screen keyboard exists for Wayland — or for
Linux at all.** Repeated GitHub searches return nothing relevant; every
controller-driven OSK is embedded in an application (Steam, RetroArch, Kodi,
OpenGamepadUI). The survey doc
[Hoverth/wayland-virtual-keyboards](https://github.com/Hoverth/wayland-virtual-keyboards)
reaches the same conclusion for desktop OSKs generally. **Playtron GameOS**
ships no OSK package (InputPlumber configs; closed-source launcher; pivoting to
a COSMIC/Smithay stack) — VERIFIED(web, repo grep). **You would be building the
first one.**

### 5.2 wvkbd — closest architectural fit

**VERIFIED(web/API):** C, **GPL-3.0**, 461 stars, active (last push
2026-07-24), [github.com/jjsullivan5196/wvkbd](https://github.com/jjsullivan5196/wvkbd).
Tiny: `main.c` 41 KB, `keyboard.c` 26 KB, `drw.c` 8.4 KB ~= 2 kLOC of logic;
renders with cairo+pango.

Binds exactly the right protocols (`main.c:39,45,50`): `zwlr_layer_shell_v1`
(mandatory), `zwp_virtual_keyboard_manager_v1` (mandatory),
`zwp_input_method_manager_v2` (only under `--auto`, `main.c:76/464`); layer is
already **OVERLAY** (`main.c:63`) with `keyboard_interactivity = NONE`
(`main.c:795`); layer namespace `"wvkbd"` (since
[commit b0fd6777](https://github.com/jjsullivan5196/wvkbd/commit/b0fd6777),
2025-04-26) — usable for `layerrule`. **It never calls `commit_string`** — text
is always keycodes; `--auto`'s IM-v2 listener maps `activate→show()` /
`deactivate→hide()` and stubs everything else, **including `unavailable` as an
empty no-op** (`main.c:517-553`) — if fcitx5 wins the IME slot, auto-show
silently never works.

Keymap strategy: base layouts are seven **static pre-baked XKB keymaps** with
standard evdev keycode names (`keymap.mobintl.h`, 454 KB), swapped only on
layout change — safer than per-character swapping. **But** out-of-map Unicode
uses a temp-keymap trick: sprintf the codepoint into
`key <COMP> { [ U%08X ] };`
([keymap.mobintl.h:1358](https://raw.githubusercontent.com/jjsullivan5196/wvkbd/master/keymap.mobintl.h))
and press **evdev keycode 127** (`keyboard.c:543`) — which Electron interprets
as ContextMenu ([wvkbd#89](https://github.com/jjsullivan5196/wvkbd/issues/89),
**open**, reproduced on Hyprland). Other known defects: requires `xdg_wm_base`
for its key-magnify popup (fragile on Hyprland —
[Hyprland#5011](https://github.com/hyprwm/Hyprland/issues/5011) /
[wvkbd#65](https://github.com/jjsullivan5196/wvkbd/issues/65)); latching
modifiers can arm Hyprland's `$mainMod + mouse` binds
([wvkbd#81](https://github.com/jjsullivan5196/wvkbd/issues/81)); one unresolved
2026-01 report of `above_lock 2` still under hyprlock. IPC is signals only
(SIGUSR1 hide / SIGUSR2 show / SIGRTMIN toggle).

Lacks: any cursor/position selection model, focus navigation, gamepad concept,
haptics (explicitly out of scope per README), gamescope backend.

Hyprland-community field use: wvkbd is the consensus choice, typically toggled
via hyprgrass edge-swipe → `kill -34 $(pgrep -x wvkbd-mobintl)`
([r/hyprland 184ljgb](https://www.reddit.com/r/hyprland/comments/184ljgb/anyone_using_an_onscreenkeyboard_with_hyprland/),
[17u71ht](https://www.reddit.com/r/hyprland/comments/17u71ht/on_screen_keyboard/));
it was *"the only one that pushed the windows up rather than overlapping"*
(exclusive zone)
([r/hyprland 1s9m965](https://www.reddit.com/r/hyprland/comments/1s9m965/how_do_you_all_feel_about_a_touchcentric_hyprland/)).

### 5.3 squeekboard

**VERIFIED(web):** Rust+C, GPL-3.0-or-later, 2,629 commits,
[gitlab.gnome.org/World/Phosh/squeekboard](https://gitlab.gnome.org/World/Phosh/squeekboard).
Requires layer-shell + virtual-keyboard-v1; input-method-v2 "strongly
recommended" and **used to commit text when present** => hard fcitx5 conflict.
Runs standalone on Hyprland/sway (no phoc dep; Arch deps pull `feedbackd` +
`gnome-desktop`). YAML layouts (no Shift/Super keys — motivation for the
[squeekboard-sway fork](https://github.com/valderman/squeekboard-sway)). D-Bus
show/hide: `busctl call --user sm.puri.OSK0 /sm/puri/OSK0 sm.puri.OSK0
SetVisible b true`. Honors GNOME's `screen-keyboard-enabled` GSetting —
enabling it globally on Hyprland **crash-looped GDM**
([r/gnome thread](https://www.reddit.com/r/gnome/comments/1fma83o/squeekboard_on_hyperland_crashes_gnome_because_of/)
→ [gnome-shell#7912](https://gitlab.gnome.org/GNOME/gnome-shell/-/issues/7912)).
**Being replaced upstream by `stevia`** (Arch `stevia` 0.57.0, 2026-08-19,
`conflicts: squeekboard`).

### 5.4 OpenGamepadUI — has an OSK; not reusable

**VERIFIED(web/API):** GDScript/Godot 4, GPL-3.0, 941 stars, active (pushed
2026-08-28), [github.com/ShadowBlip/OpenGamepadUI](https://github.com/ShadowBlip/OpenGamepadUI).
Real OSK at `core/ui/common/osk/` (`on_screen_keyboard.gd`,
`keyboard_layout.gd`, `keyboard_row.gd`, `keyboard_key_config.gd`,
`keyboard_context.gd`) + `core/global/keyboard_instance.gd`,
`core/systems/input/keyboard_opener.gd`; docs
`docs/class-reference/Keyboard*.md`.

Read from `on_screen_keyboard.gd`: a Godot `Control` node wired to OGUI's state
machines, `GamescopeInstance`, and `InputPlumberInstance`; layout = rows of
Godot Buttons → **focus-based navigation only, no dual-trackpad**; injection =
**InputPlumber virtual keyboard over DBus**
(`virtual_keyboard.send_key("KEY_LEFTSHIFT", true)`;
`InputPlumberEvent.virtual_key_from_keycode`) → **evdev keycodes only, no
Unicode**, requires InputPlumber; target types GODOT/DBUS/X11 (X11 path
manipulates gamescope XWayland focus).
`ShadowBlip/gamescope-session-opengamepadui` is **archived** (2026-02).
**Verdict: not reusable** — Godot-, InputPlumber-, and
gamescope-session-coupled, keycode-limited; the layout data model is worth
reading, the code is not portable.

### 5.5 The rest

| Project | Finding |
|---|---|
| **InputPlumber** | Input router/remapper, DBus; provides virtual keyboard *devices* but no OSK UI. Already assessed in `docs/04-prior-art.md` (CVE-2025-66005/CVE-2025-14338 DBus auth issues). |
| **HHD** (hhd-dev/hhd) | Python, LGPL-2.1, 399 stars, active. Has `src/hhd/plugins/overlay/` (SDL overlay + Steam integration) but **no OSK** — VERIFIED(API, tree grep). |
| **ChimeraOS / gamescope-session(-steam)** | Session-launcher shell scripts; ship **no OSK**, rely on Steam's built-in — VERIFIED(API). |
| **Steam's own OSK** | Not invocable outside Steam. In gamescope it types via `gamescope_input_method`; in an **X11 desktop session its XTEST output does reach arbitrary non-Steam windows** — with keycode/layout mangling on non-QWERTY ([steam-for-linux#13220](https://github.com/ValveSoftware/steam-for-linux/issues/13220)) and overlay grab lockouts over Proton games ([#13467](https://github.com/ValveSoftware/steam-for-linux/issues/13467)). On Wayland/Hyprland: no XTEST path, steals focus — unusable. [gamescope#2119](https://github.com/ValveSoftware/gamescope/issues/2119) (open) asks to *disable* it. |
| **Decky Loader** | No OSK plugins found. |
| **wkeys** ([ptazithos/wkeys](https://github.com/ptazithos/wkeys)) | Rust, vkbd-v1 + layer-shell, no IM-v2 (fcitx5-safe). |
| **HyprOsk** ([Ziggs25/HyprOsk](https://github.com/Ziggs25/HyprOsk)) | Rust, Hyprland-targeted, IM-v2 + text-input-v3 + commit_string + vkbd — created **2026-08-30**, 2 stars, unproven. |
| **wf-osk** (Wayfire) | vkbd-v1 + layer-shell, last push 2024-03. |
| **sysboard** ([System64fumo/sysboard](https://github.com/System64fumo/sysboard)) | Simple Wayland OSK; one of the two on awesome-hyprland. |
| **maliit-keyboard / plasma-keyboard** | Ride **input-method-unstable-v1** (KWin) which Hyprland does not implement → dead on arrival. |
| **onboard fork** | gtk-layer-shell + `/dev/uinput`; on Hyprland "typing works, anchored to bottom", move/resize broken. |
| **svkbd** | suckless, X11 only. **eekboard** dead (2012). **corekeyboard** X11/XTEST. **wl-kbptr** is a pointer tool, not an OSK. |
| **Omarchy-specific** | [javon27/omarchy-surface-touch](https://github.com/javon27/omarchy-surface-touch) (patched wvkbd-deskintl + `omarchy-toggle-osk`, incl. a touch-jitter fix); [abdxdev/omarchy-onscreen-keyboard](https://github.com/abdxdev/omarchy-onscreen-keyboard) (Quickshell/QML bar plugin, types via wtype, created 2026-08-20). |
| **wtype** | Not an OSK; the right subprocess/reference for burst typing (§3.4). |
| **gamescope-type** | **Installed at `/usr/bin/gamescope-type`** — reference `gamescope_input_method` client; copy or shell out. |

## 6. Voice input (Q6) — premise corrected

**Two different components were conflated. VERIFIED(local).**

**HypXRVoice is the user's own project and does not do dictation.** Packages
`hypxrvoice 0.20260812.1` + `hypxrvoice-model-base-en 1.0.0-2` (whisper
`ggml-base.en.bin` at `/usr/share/hypxrvoice/models/`). Per
`/usr/share/doc/hypxrvoice/README.md` it is a **voice-control** daemon:
PipeWire → energy VAD → whisper.cpp (DTW word timestamps) → intent tier (rule
grammar or GBNF-constrained llama) → **strict-allowlisted `hyprctl` argv**
(19-verb `SAction` schema: XR monitor manipulation +
focus/fullscreen/workspace/move verbs; `executor.dry_run` defaults true). **It
never types text into the focused app.** Reusable pieces:
- `hypxrvoicectl ptt start|stop|toggle`, `status`, `reload` over an `AF_UNIX`
  socket in `$XDG_RUNTIME_DIR/hypxrvoice/` — ready-made push-to-talk trigger.
- `feedback.stdout_json = true` — each transcript as a JSON line with `onsetMs`
  + per-word timestamps.
- `hypxrvoiced --oneshot file.wav [--ptt] [--intent]`, `--intent-text "..."`.
- `hypxrhud` D-Bus panel API (`io.github.andrewgaspar.hypxrhud1`) for feedback UI.
- Binaries: `/usr/bin/hypxrvoiced`, `/usr/bin/hypxrvoicectl`,
  `/usr/bin/hypxrvoiced-session`; user unit
  `/usr/lib/systemd/user/hypxrvoiced.service`; config
  `/usr/share/hypxrvoice/config.toml` (→ `~/.config/hypxrvoice/config.toml`).

**Omarchy's actual dictation tool is `voxtype`.** **VERIFIED(local)** from
`/home/ajg/code/omarchy`:
- `bin/omarchy-voxtype-install`: `omarchy-pkg-add wtype voxtype-bin`, then
  `voxtype setup --download`, `voxtype setup gpu --enable` (if Vulkan),
  `voxtype setup systemd`.
- `default/hypr/bindings/voxtype.lua` — push-to-talk already wired:
  ```lua
  o.bind("SUPER + CTRL + X", "Toggle dictation", "voxtype record toggle")
  o.bind("F9", "Start dictation (push-to-talk)", "voxtype record start")
  o.bind("F9", "Stop dictation (push-to-talk)", "voxtype record stop", { release = true })
  ```
- `default/voxtype/config.toml`: `[output] mode = "type"` | `"clipboard"`,
  `fallback_to_clipboard = true`, `type_delay_ms = 1`; `[whisper] model =
  "base.en"`; `state_file = "auto"` → **`$XDG_RUNTIME_DIR/voxtype/state`**
  carrying `idle` / `recording` / `transcribing`; spoken punctuation, word
  replacements, optional LLM post-process hook; `pause_media = true` (pauses
  MPRIS during recording).
- First-run hook: `install/user/first-run/install-voxtype.hook`.

**VERIFIED(web)** — this is [github.com/peteonrails/voxtype](https://github.com/peteonrails/voxtype)
/ [voxtype.io](https://voxtype.io/): local-first (whisper.cpp et al.),
Wayland-first, CLI `voxtype record start|stop|toggle`, `voxtype status`,
`voxtype setup`, `voxtype configure`; **types via `wtype`** (i.e.
`zwp_virtual_keyboard_v1`) with dotool/ydotool/clipboard fallbacks; Hyprland
keybinding docs included.

**Answer: yes, push-to-talk dictation into the focused app is trivially
triggerable programmatically** — `voxtype record start` / `record stop` from
the hyprsc daemon; the state file drives a mic indicator on the OSK; and
because its output path is the same virtual-keyboard protocol as the OSK's, it
inherits the same XWayland and gamescope limitations (§3.1) — dictation into a
gamescope-hosted game would need routing through the OSK's
`gamescope_input_method` backend instead of wtype.

**Caveat:** `voxtype` is **not currently installed** on this machine (only
`wtype` is); install with `omarchy-voxtype-install`.

## 7. IME / text-input coexistence (Q7)

**Hyprland permits exactly one input method; first binder wins.
VERIFIED(local)** — `src/managers/input/InputMethodRelay.cpp:17-24`:

```cpp
void CInputMethodRelay::onNewIME(SP<CInputMethodV2> pIME) {
    if (!m_inputMethod.expired()) {
        Log::logger->log(Log::ERR, "Cannot register 2 IMEs at once!");
        pIME->unavailable();
        return;
    }
```

The second client receives `zwp_input_method_v2.unavailable` and its object is
inert. No queueing, no arbitration, no config knob.

**fcitx5 takes a keyboard grab. VERIFIED(web)** — `fcitx5
src/frontend/waylandim/waylandimserver.cpp:189`:
`keyboard_.reset(ic_->grabKeyboard());`.

Consequences, in order of severity:

1. **Whoever starts first wins.** fcitx5 running → the OSK's
   `get_input_method` gets `unavailable`. OSK first → **fcitx5's CJK input
   breaks entirely.** Both failures are silent (fcitx5 logs debug; wvkbd-style
   clients stub the event).
2. **Worse than mutual exclusion:** while fcitx5 holds the grab, a *different*
   client's virtual-keyboard events are routed **into fcitx5**, not the app —
   **VERIFIED(local)**: `USEIME = HASIME && !DISALLOWACTION`
   (`InputManager.cpp:1656-1659`), and `DISALLOWACTION` is false because
   `grabClient()` is fcitx5's client, not the OSK's
   (`InputManager.cpp:1772`). fcitx5 would intercept and re-interpret the
   OSK's output.
3. This **directly conflicts with the §3.3 keybind-bypass architecture**, which
   requires *the OSK's* client to hold the grab.

**Coexistence matrix (VERIFIED):**

| OSK design | Binds IM-v2? | Conflicts with fcitx5 on Hyprland? |
|---|---|---|
| vkbd-v1 only (wvkbd default, wtype, wkeys, wf-osk) | No | **No** |
| wvkbd `--auto` | Yes (show/hide only) | **Yes** — loser silently loses (empty `unavailable` stub) |
| squeekboard / HyprOsk (commit via IM-v2) | Yes | **Yes**, unavoidable |
| uinput (onboard fork, ydotool) / D-Bus (fcitx5-osk) | No | No |

**Recommendation:** make the IM-v2 binding **optional and runtime-detected**.
Attempt `get_input_method`; on `unavailable`, fall back to
virtual-keyboard-only mode plus
`device { name = hl-virtual-keyboard-<osk>  keybinds = false }`. Losing the IME
slot costs only auto-show + the keybind-bypass optimization, never core typing.
Document OSK-with-IME and fcitx5 as mutually exclusive on Hyprland — a
compositor-level constraint. Also prefer **one-shot auto-releasing modifiers**
(wtype-style) so a controller-driven Super never arms `$mainMod + mouse` binds.
gamescope's own input-method has the identical one-per-seat rule
(**VERIFIED(local)**, protocol XML), so Steam's OSK and ours cannot coexist
inside one gamescope instance either.

**Caveat for the living-room flow:** Hyprland users report OSKs cannot type
into layer-shell launchers (wofi/tofi/anyrun) that take *exclusive* keyboard
interactivity
([r/hyprland 1kq1360](https://www.reddit.com/r/hyprland/comments/1kq1360/app_launchers_and_virtual_keyboard/));
workaround `wofi --normal-window`. Since the target flow is "Guide+X opens the
Omarchy launcher → OSK types into it", **test virtual-keyboard→launcher
delivery early** and be prepared to run the launcher as a normal window or
special-case it.

## 8. Comparison and ranking — strictly technical fit

| Criterion | **New on layer-shell** | **Fork wvkbd** | **Fork squeekboard** | **Adopt OGUI OSK** |
|---|---|---|---|---|
| Correct surface tier out of the box | you choose | ✅ OVERLAY + interactivity NONE already | ⚠️ layer-shell, phone-oriented | ❌ Godot node, not a layer surface |
| Text path suited to games | you choose | ✅ vkbd-primary; IM-v2 only for auto-show | ❌ IM-v2-first (wrong primary, §3.1) | ❌ InputPlumber keycodes, no Unicode |
| Unicode strategy | build it | ⚠️ static evdev-named keymaps (good) **but** keycode-127 fallback breaks Electron ([#89](https://github.com/jjsullivan5196/wvkbd/issues/89)) | ✅ commit_string — unreachable for games | ❌ none |
| gamescope backend | build it | ❌ absent | ❌ absent | ⚠️ gamescope-aware, wrong layer |
| Dual-trackpad absolute-cursor selection | build it | ❌ absent | ❌ absent | ❌ absent |
| D-pad focus navigation | build it | ❌ absent | ❌ absent | ✅ native |
| Haptics hooks | build it | ❌ explicitly out of scope | ❌ | ⚠️ via InputPlumber |
| Layout + theming model | build it | ✅ C headers, colour schemes, compose | ✅ YAML (most mature) — but being replaced by stevia | ✅ Godot-only resources |
| IPC for an external daemon | design it | ⚠️ signals only — must be replaced | ⚠️ D-Bus, Phosh-shaped | ❌ internal signals |
| Dependency weight | none | cairo+pango+wayland+xkb — light | Rust+GTK+GNOME — heavy | Godot 4 + InputPlumber + gamescope-session — heaviest |
| Renderer for Deck-grade animation | free choice | ⚠️ CPU cairo/pango — fine for keys, weak for animated cursors/glow | ⚠️ GTK | ✅ GPU scene graph (its one advantage) |
| Licence | your choice | GPL-3.0 | GPL-3.0+ | GPL-3.0 |

**Ranking:**

1. **Fork wvkbd.** The only candidate whose *architecture* is already correct
   on every point that is expensive to get right: OVERLAY tier,
   `keyboard_interactivity = NONE`, virtual-keyboard-primary with IM-v2
   strictly optional, static evdev-named base keymaps, exclusive-zone
   behaviour users already prefer on Hyprland. Its ~2 kLOC of legible C makes
   replacing the input front-end wholesale — signals → hyprsc IPC;
   touch/pointer → dual absolute cursors + d-pad focus per §4.9 — a rewrite of
   the *input* layer against a *correct* output layer. Patch out its known
   defects while there: keycode-127 Unicode fallback (use gamescope's
   `allow_keycodes` set + keep an interpret-bearing modifier per §3.2), the
   `xdg_wm_base` magnify popup, latching modifiers.
2. **Build new on layer-shell.** End-state equivalent; free renderer choice,
   which matters because the code-verified Deck spec (§4) demands smooth
   animated cursors, glow/pulse animations, and tight haptic sync — where
   cairo/pango is the weak point. You re-derive wvkbd's plumbing from scratch,
   but §§2–3 plus gamescope's `ime.cpp` provide the complete recipe. Choose
   this over (1) if the renderer or GPL-3.0 is a real constraint.
3. **Fork squeekboard.** Primary text path (IM-v2 commit) is silently dropped
   for games, XWayland, and everything in gamescope — the entire primary use
   case; you would delete its core and keep its layout system, while upstream
   is replacing it with stevia anyway. Heaviest OSK dependency stack for the
   least retained value.
4. **Adopt the OpenGamepadUI OSK.** Structurally disqualified: not a layer
   surface, cannot type Unicode, requires InputPlumber + a Godot runtime,
   focus-navigation only. Its GPU renderer is the best of the four; everything
   attached to it is wrong for an OS-level overlay.

**Non-negotiable design rules regardless of choice** (all derived above):
OVERLAY tier only (§2.1); destroy the layer surface on dismiss — never hide
(§2.3); two injection backends with gamescope detection, using
`gamescope_input_method` `set_string`/`set_action` for nested targets (§2.5);
destroy the virtual keyboard after each typing burst (§3.3B); keep an
interpret-bearing modifier key in every generated keymap and use only real
character keycodes (§3.2); IM-v2 binding optional/runtime-detected because
fcitx5 may own it (§7); concurrent modality with per-source highlights, commit
on click-down, per-key-change haptic tick suppressed on key→gap (§4).

**Cheapest next experiments:** (a) `wtype 'héllo'` into a native Wayland app,
an XWayland game, and a gamescope window — three commands that empirically
settle §3.1/§3.4 on this exact stack; (b)
`hyprctl monitors -j | jq '.[].solitaryBlockedBy'` with and without a mapped
OVERLAY surface (§2.3); (c) `gamescope-type` against a nested gamescope to
validate the second backend; (d) virtual-keyboard → Omarchy launcher delivery
(§7 caveat).
