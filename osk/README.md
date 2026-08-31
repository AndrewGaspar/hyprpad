# hyprpad-osk

A controller-driven **on-screen keyboard for Hyprland** — the standalone crate
that grows into the Steam-Deck-parity OSK for the `hyprpad` project.

This is a **kickoff skeleton**: a runnable, architecturally faithful foundation
that renders a QWERTY keyboard as a `zwlr_layer_shell_v1` **OVERLAY** surface and
types into the focused app via a **uinput** keyboard, with the harder parts
clearly stubbed and TODO'd against the authoritative research in
`docs/research/osk-technology.md` (referenced below as **§**).

The crate is fully self-contained: it has its own `Cargo.toml` with an empty
`[workspace]` table so it is **never** pulled into the parent `hyprpad` crate,
and it touches nothing outside `osk/`.

## Build & test

```sh
cd osk
cargo build --release
cargo test
cargo clippy --release
```

## Run

```sh
# default: listen on $XDG_RUNTIME_DIR/hyprpad-osk.sock
hyprpad-osk
# or drive it from stdin (handy for scripting / the live test)
printf 'show bottom\ntype hello world\nhide\nquit\n' | hyprpad-osk --stdin
```

The hyprpad daemon will eventually drive the socket, forwarding the controller's
two trackpad cursors and clicks.

### Hyprland config (compositor-side layer rules, §2.4)

The OSK picks a stable layer **namespace** (`hyprpad-osk`) so these rules — which
live in Hyprland config, not the client protocol — can match it:

```
# keep the OSK out of screen captures (§2.4, non-negotiable)
layerrule = no_screen_share, hyprpad-osk
# for a future lock-screen OSK (§2.4): render (1) and receive input (2)
# layerrule = above_lock 2, hyprpad-osk
```

## Renderer choice

**`smithay-client-toolkit` (SCTK) + a software shm buffer + `font8x8`.**

SCTK wraps the `zwlr_layer_shell_v1` create/configure/ack boilerplate and gives a
ready `SlotPool` shm buffer to draw into — the pragmatic choice the research
calls out (§8). Default features are **off**: no `calloop` (we run our own
`poll(2)` loop so the control-channel fd and the Wayland fd share one thread) and
no `xkbcommon` (the OSK takes no keyboard focus, so it needs no seat/keymap
handling). Labels are drawn with `font8x8`, a pure-Rust embedded 8x8 bitmap font,
so there is **no cairo/pango/fontconfig C dependency** — the whole crate stays
light and deterministic. The research flags CPU rendering as the weak point for
Deck-grade animated cursors/glow (§8); a GPU path is a deliberate later decision
(§4.7, deferred).

## Architecture

| Module | Responsibility |
|---|---|
| `layout` | The **shared key/keycode model** (QWERTY grid + evdev scancodes, §4.6) and the geometry engine that serves **both** modes from it. |
| `surface` | Layer-shell anchoring / exclusive-zone policy — bakes in OVERLAY-only, destroy-on-dismiss, the stable namespace. |
| `render` | CPU renderer: keys + labels + highlights into an ARGB8888 shm buffer. |
| `output` | The **uinput** keyboard (real evdev keycodes → types everywhere incl. XWayland), adapted from the parent crate's proven `src/output.rs`. |
| `control` | The line-based control channel (unix socket or stdin) with a dual-trackpad-shaped vocabulary. |
| `app` | Binds the globals, owns the live surfaces, runs the poll loop over Wayland + the control channel. |

### The layout data model — one model, two modes

There is exactly **one** `Keyboard`: the QWERTY key set where every key carries
its label, its **raw Linux/evdev keycode** (the same number the uinput backend
emits — the §4.6 grid), its grid row, and crucially its **`Hand`** (Left / Right
/ Either). Everything mode-specific is *geometry* derived from that model by
`LayoutEngine`; the keys and keycodes never change between modes.

* **Mode A — `BottomDeck`** (implemented): the full grid in one bottom-docked
  panel. The two trackpad regions are the left 55% / right 55% of the surface
  with a 10% overlap (§4.1/§4.9) — the absolute-cursor Deck model.
* **Mode B — `SideSplit`** (data model + basic render): the **same keys**
  partitioned by `Hand` into two **edge-docked columns** — left-hand keys
  (`…QWERT / ASDFG / ZXCVB`) dock the **left** screen edge, right-hand keys
  (`YUIOP / HJKL / NM…`) dock the **right** edge, and workspace content reflows
  into the centre strip between them. The **left trackpad addresses the left
  column, the right trackpad the right column**, so the Deck's two-region model
  maps onto physical screen geography (each thumb → its own side). `Hand` is the
  single field that makes this fall out of the shared model.

The `surface` module turns a mode into panels: Mode A = one bottom panel; Mode B
= two panels (`LeftColumn` + `RightColumn`), one exclusive zone per edge. Two
surfaces (not one full-width surface with a transparent hole) because an
exclusive zone is a single scalar along one edge — you cannot carve a gap in the
middle of one surface, so genuine centre reflow *requires* an independent
exclusive zone on each edge.

**Reflow vs overlay** is an orthogonal axis: `reflow` sets an exclusive zone so
Hyprland shrinks the tiled area (the pan-up / side-squeeze feel); `overlay` sets
a zero exclusive zone to float over a fullscreen game. Both always use the
OVERLAY layer tier.

### Control-channel vocabulary

Line-based, designed with the **dual-trackpad future** in mind (§4.1/§4.5):

```
show <bottom|split> [reflow|overlay]   create + render the surface(s)
hide                                    DESTROY the surface(s) — never unmap
cursor <L|R> <nx> <ny>                  pad absolute position, each axis in [-1,1]
commit <L|R>                            commit the key under that pad's cursor (click-down)
key <keycode>                           commit a raw evdev keycode directly
type <text>                             type an ASCII string
quit                                    exit
```

`cursor`/`commit` already speak the per-pad model the daemon will forward, even
though this kickoff drives a subset of their behaviour.

## Done / Stubbed / Deferred — mapped to `osk-technology.md`

### Done (implemented + live-verified on Hyprland 0.56.2)

- **OVERLAY layer tier, never TOP** (§2.1). Verified: `hyprctl layers` lists
  namespace `hyprpad-osk` at overlay level 3.
- **Destroy the layer surface on dismiss, never hide** (§2.3). Verified: after
  `hide`, the OSK-owned overlay entry (its real pid) is gone from
  `hyprctl layers`.
- **Mode A bottom exclusive-zone reflow AND overlay (zero zone)** as a parameter
  (task Mode A; §2.3). Verified: bottom panel `2048x435` with exclusive zone.
- **Mode B two edge-docked columns** with per-edge exclusive zones (task Mode B).
  Verified: `show split` → left `450x1254@0` + right `450x1254@1598`.
- **Shared QWERTY key/keycode model** with the §4.6 grid + PC scancodes, drawn as
  a legible grid; one model serving both modes via `Hand`.
- **uinput injection with real evdev keycodes** (§3.1, Q14), reusing the parent's
  proven backend. Verified: device enumerated by Hyprland as seat keyboard
  `hyprpad-osk-virtual-keyboard`; sending `key 42/29/56/125` produced exactly
  those keycodes on the device's evdev node (control → uinput → evdev proven).
- **Control channel** (unix socket + stdin) with the dual-trackpad vocabulary.
- **Stable namespace** for the `no_screen_share` / `above_lock` layer rules
  (§2.4).
- **Absolute dual-trackpad cursor mapping + hit-testing** scaffold (§4.1 formula:
  `p = 0.5(1 ± clamp(v·scale, -1, 1))`, absolute, no accumulator) wired to
  `cursor`/`commit`.

### Stubbed (structure in place, behaviour minimal)

- **Mode B render** — geometry/anchoring correct; keycap render is basic, final
  ergonomics deferred (§4).
- **Shift / Caps state** — simple toggle + one-shot-ish clear; the full §4.6
  bitfield (`Off/OneShot/Stuck/Held`, caps interplay, physical chording) is not
  implemented.
- **Concurrent per-source highlights** (§4.5) — the renderer already takes a list
  of `Highlight{LeftPad,RightPad,Focus}` (up to three at once), but only the two
  pad cursors are driven; no d-pad focus cursor yet.
- **Meta / layer keys** (`?123`, arrows, emoji) — present in the model, commit is
  a no-op (§4.6 "Layers").

### Deferred (clear TODOs → research section)

- **Per-key-crossing haptic tick**, cached-rect re-hit-test, tick suppressed on
  key→gap (§4.3 / §4.9 item 3).
- **Commit-on-click-down nuances**: extended-character popups commit on up / open
  at 450 ms; backspace auto-repeat 450→200 ms; rollover (§4.2/§4.6).
- **`gamescope_input_method` second injection backend** for nested games —
  `set_string`/`set_action` + `commit(serial)` over `$GAMESCOPE_WAYLAND_DISPLAY`
  (§2.5); the keymap-swap route is dead through gamescope.
- **Full-Unicode into native apps** via `zwp_virtual_keyboard_v1` with a generated
  keymap that keeps an interpret-bearing modifier (to survive Hyprland's
  re-serialisation, §3.2), destroying the VK after each burst (§3.3B).
- **Optional IM-v2 binding**, runtime-detected because fcitx5 may own the single
  IME slot; on `unavailable`, fall back + set
  `device{ name=hl-virtual-keyboard-<osk> keybinds=false }` (§3.3A, §7).
- **`above_lock` lock-screen OSK** (§2.4).
- **Themes**: CSS-custom-property-equivalent token table on a root theme name +
  per-key/row/col hooks; glow/pulse animations (§4.7).
- **Vertical (Mode B) final ergonomics** and a GPU renderer for Deck-grade
  animation (§8).

## Live-verification notes

Verified on this machine (Hyprland 0.56.2, HypXRland snapshot). Two
environment quirks worth recording:

- This HypXRland fork retains **stale `hyprctl layers` entries with `pid=-1`**
  after a client dies — a compositor-side artifact, not an OSK leak. Destroy is
  proven by tracking the OSK's **real pid**: its overlay entry disappears after
  `hide` and after process exit.
- Full keystroke-into-a-target-window capture wasn't demonstrated because this
  fork would not let the agent programmatically de-focus its own terminal; the
  injection path is instead proven at the evdev level (correct keycodes on the
  device node) plus seat enumeration, on top of the parent crate's already
  Q14-validated end-to-end uinput result (types correctly incl. XWayland).

## Licence

MIT OR Apache-2.0.
