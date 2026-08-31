# 09 — The programme: costing the living-room vision

Synthesis of the six research reports in [`research/`](research/) against the
vision in [08](08-living-room-vision.md). Dated 2026-08-31. Ambition level:
**shippable project** (chosen by the owner — estimates reflect building things
others could install, not just personal glue). Target: **the living-room tower
first**, Steam Deck as a later port. Boot posture: **A — boot secret retained,
controller-driven entry** ([06](06-recommendation.md) history).

Sizing legend — focused engineer-weeks at shippable quality:
**S** ≤ 1 wk · **M** 1–3 wk · **L** 3–6 wk · **XL** 6–12 wk.

## The one-table answer

| # | Work item | Size | Category | Vision steps |
|---|---|---|---|---|
| W0 | Validation experiment battery | S | experiments | all |
| W1 | Config-land quick wins | S | config | 4, 7, 10, 12 |
| W2 | hyprsc core: decoder, gesture engine, IPC | **L** | new daemon | 4, 8, 10, 11 |
| W3 | Screenshot / voice / launcher chords | S | glue on W2 | 5, 6, 11 |
| W4 | Living-room classifier + mode switch | M | small software | 3, 9 |
| W5 | BPM workspace via nested gamescope | M | integration | 2, 3, 8 |
| W6 | Global conditional launch wrapper | M | small software | 9 |
| W7 | PiP + MPRIS media control | S–M | glue | 7, 10 |
| W8 | Controller unlock (lock plugin + PAM) | M | integration | 12 |
| W9 | Suspend/resume + wake tuning | M | tower work | 12 |
| W10 | The OSK ("deckboard") | **XL** | new application | 5, 6, 11 |
| W11 | Boot: initramfs button-code unlock | M | initramfs | 1 |
| W11b | Boot: standalone graphical boot OSK | L | initramfs | 1 (fallback UI) |
| W12 | Tier 1: device ownership + focus routing | **L** | daemon + system | input contract |
| W13 | Deck port | L | port | eventual |

**Critical path to the full vision: W2 → W10 → W12.** Everything else hangs
off those three. Total, all items at shippable quality: roughly
**28–55 focused weeks**; a personal-quality cut of the same scope would be
roughly a third of that. The OSK alone is ~a quarter of the total — it is the
single biggest item, and research confirms **no OS-level gamepad OSK exists
anywhere on Linux to borrow** ([osk-technology.md §5.1](research/osk-technology.md)).

## What the research settled (load-bearing facts)

1. **The controller side is fully de-risked.** Raw `0x42` at 263–269 Hz,
   always on, Steam-independent, guide bit confirmed, concurrent hidraw reads
   proven harmless ([03](03-hardware-findings.md)).
2. **This Hyprland build is Lua-only.** Classic `hyprctl dispatch`/`keyword`
   are dead; hyprsc must speak `hl.dsp.*` dispatches. Every online recipe
   needs translation ([gamescope-hyprland-integration.md §0](research/gamescope-hyprland-integration.md)).
3. **BPM belongs inside nested gamescope.** Bare-XWayland BPM inherits open
   bug #8640 (controller drives an invisible overlay) rooted in
   gamescope-written X11 atoms Steam expects; nested gamescope on NVIDIA is
   verified working, embedded is not ([ibid. §1–2](research/gamescope-hyprland-integration.md)).
4. **PiP-over-game is source-proven**; VRR survives; MPRIS, not clicks
   ([ibid. §5–6, §8](research/gamescope-hyprland-integration.md)).
5. **A global launch hook exists and is spec-supported**: a `linux→linux` v2
   compat tool as `CompatToolMapping "0"` — fires for native games too
   ([steam-bpm-launch-wrapping.md §2.3](research/steam-bpm-launch-wrapping.md)).
6. **The lock is a Quickshell plugin, not hyprlock**, virtual-keyboard input
   reaches it, and the sanctioned extension point is
   `omarchy plugin clone omarchy.lock` + an `authenticate()` IPC method +
   `libpam_pwdfile` ([session-lifecycle.md §1–3](research/session-lifecycle.md)).
7. **The OSK must be an OVERLAY layer surface destroyed on dismiss, with two
   injection backends** (virtual-keyboard for the host,
   `gamescope_input_method` for nested games), and the Deck interaction spec
   is now extracted from Steam's own shipped code — including the fact that
   Deck parity means **concurrent modality, not mode switching**
   ([osk-technology.md §2–4](research/osk-technology.md)).
8. **A Plymouth-theme boot OSK is impossible; a pre-`encrypt` hook is easy.**
   Lizard mode gives arrows/Enter/Esc in the initramfs; a button-sequence
   code delivered via `/crypto_keyfile.bin` has working Arch prior art
   ([initramfs-unlock.md](research/initramfs-unlock.md)).
9. **Suspend/resume is nearly free** (Omarchy already locks-before-sleep with
   an inhibitor and waits for `secure`); wake needs tower-side BIOS/bus work
   and a plan for *spurious* wakes ([session-lifecycle.md §4–5](research/session-lifecycle.md),
   [pam-auth-usb-wake.md](research/pam-auth-usb-wake.md)).

## The work items

### W0 — Validation experiment battery (S; do first, mostly minutes each)

From the reports, cheap tests that retire the largest remaining unknowns:

- `wtype 'héllo'` into a native Wayland app, an XWayland app, and a nested
  gamescope window — settles text-injection reality on this exact stack.
- `gamescope-type` against nested gamescope — validates the second OSK backend.
- Virtual-keyboard → Omarchy launcher delivery (layer-shell exclusive-
  interactivity caveat).
- `hyprctl monitors -j | jq '.[].solitaryBlockedBy'` with/without a mapped
  OVERLAY surface — confirms the destroy-on-dismiss rule.
- With the screen locked: `wtype -k Escape` — confirms virtual keyboard
  reaches the lock surface (costs nothing, cannot trip faillock).
- Lizard-mode test at a LUKS prompt (reboot; press dpad/A/B) — Q12.
- Guide-chord suppression (Q1b): hold guide, tap A, release — does Steam
  still take focus?
- Nested gamescope + `--hdr-enabled` + `quirks:prefer_hdr = 2` on the tower's
  HDR TV — first field validation of the verified mechanism.
- Tower probe (Q11): GPU, `mem_sleep`, BIOS ErP, USB topology, EDID physical
  size, keyring behaviour on first login.

### W1 — Config-land quick wins (S total; each is minutes-to-hours)

All shippable as an `omarchy`-style config layer or PR:

- `suppressevent activatefocus` on Steam windows (Lua form), plus
  `float_switch_override_focus = 0`, `cursor:no_warps = true`.
- The validated PiP rule block ([research §5](research/gamescope-hyprland-integration.md)).
- ~~`Relogin=true` in SDDM autologin~~ — **declined by the owner (2026-08-31)**:
  it forecloses deliberate logout and user switching, and is a workaround
  rather than a step toward controller-driven login (Qt6 has no gamepad
  support, so a controller-capable greeter is not on any roadmap). Accepted
  cost: an accidental logout needs the keyboard once.
- Tier 0 Steam neutering: empty the Desktop Layout and Guide Chord Layout
  (removes X→keyboard today; superseded by W12 later).
- `omarchy-voxtype-install` + PTT bindings; install `playerctl`.
- Bind a key/chord to `omarchy-capture-screenshot fullscreen slurp`
  (already does disk+clipboard).

### W2 — hyprsc core (L) — the daemon everything rides on

Rust daemon, per [06](06-recommendation.md) phase 1–3, now with
research-informed specifics:

- `0x42` decoder (finish the button map — Q2/Q3, cross-check SDL3), evdev
  backend for generic pads (`BTN_MODE` guide).
- Gesture/mode machine: tap/hold/double-tap, chord table, analog flick
  detection; declarative config.
- Output: Hyprland socket speaking **Lua dispatches**; `zwlr_virtual_pointer_v1`
  (trackpad cursor — fixes what Steam can't do on Wayland);
  `zwp_virtual_keyboard_v1` bursts (create-use-destroy per burst, keybinds
  excluded via `device { keybinds = false }`; both hazards and fixes are
  documented in [osk-technology.md §3.3](research/osk-technology.md)).
- Arbitration v1 (passive era): suppress desktop gestures when a
  `steam_app_*`/gamescope window is focused; socket2 subscription.
- Haptic feedback on gesture recognition — needs Q4 (writing to hidraw while
  Steam holds it) answered first; degrade gracefully without it.
- Shippable extras: config schema + docs, systemd user unit, packaging.

Deliverable moment: *hold guide, flick right stick, workspace changes* — with
trackpad-as-mouse working on the desktop for the first time with Steam running.

### W3 — Chord glue (S, rides on W2)

Screenshot chord → `omarchy-capture-screenshot fullscreen slurp`; PTT chord →
`voxtype record start/stop` (mic indicator from
`$XDG_RUNTIME_DIR/voxtype/state`); launcher chord → walker/launcher with the
W0-validated typing path; PiP chords → W7.

### W4 — Living-room classifier + mode switch (M)

`DMI match → manual toggle (omarchy-toggle) → connector name + EDID physical
size`, written to a state file everything reads. Hotplug via socket2
`monitoraddedv2` with retry/debounce/`FALLBACK` exclusion. Mode switch applies:
monitor mode (`hl.monitor { mode = "3840x2160@120" }` — the fix for XWayland's
refresh-rate blindness), VRR, `cm`/`bitdepth` for HDR, workspace layout, and
gates W5/W6. Never key on EDID serial.

### W5 — BPM workspace (M)

`gamescope -e --backend wayland … -- steam -gamepadui` (+ SteamOS env block,
`QT_IM_MODULE=steam` inside the session) pinned to a named `steam` workspace,
launched by W4 in living-room mode; window rules; `steam://close/bigpicture`
for scripted exit. Risks: first-run HDR validation; `-steamdeck` softlock
caveat.

### W6 — Global conditional launch wrapper (M)

The `linux→linux` v2 compat tool (Steam-Play-None pattern): `launch.sh` reads
the W4 state file → plain `exec` at the desk, ScopeBuddy/gamescope wrap in
living-room mode; scout-runtime escape; installed to
`~/.steam/root/compatibilitytools.d/`; `CompatToolMapping "0"` written with
Steam closed. Shippable as its own small repo.

### W7 — PiP + media control (S–M)

Chords: launch/park mpv or move a browser window into the pinned-float PiP
(W1 rules); pause/unpause via MPRIS with `CanPause`/`CanPlay` gating and the
per-process-player caveat; hide/show = toggle `pin`+`move` off-screen or
minimize — **not** alpha-0 (scanout is already forfeit with a float; fine).

### W8 — Controller unlock (M)

Clone `omarchy.lock`, add `authenticate(password)` IPC calling
`submitPassword()`; add `libpam_pwdfile` line (hashed second secret) to
`omarchy-lock-password`; hyprsc lock-mode: read button code, map to the
secret, call authenticate. Bonus channel: `locked = true` binds already work
from a virtual keyboard for media/volume while locked. L=6 code ≈ 21.5 bits
against `deny=10 unlock_time=120` — adequate for the house-guest threat
model; shoulder-surfing is the real exposure and is accepted per the
[privacy bar](08-living-room-vision.md).

### W9 — Suspend/resume + wake (M, mostly tower work)

`systemctl suspend` from a chord (Omarchy's inhibitor service handles
lock-before-sleep). Tower: BIOS ErP off, wake-from-USB on, bus-wide
`power/wakeup` udev rule, `deep` vs `s2idle` A/B. Engineering for the
*spurious* wake problem (SteamOS#2641): consider gating `power/wakeup` on
controller idle-timeout state, or accept idle-timeout suspend.

### W10 — The OSK, "deckboard" (XL — the flagship)

Per the [full spec](research/osk-technology.md): fork wvkbd (or build new if
GPL-3.0/renderer chafes); OVERLAY layer surface destroyed on dismiss;
`above_lock` for the lock screen; dual-backend injection
(virtual-keyboard-v1 with the keymap-swap recipe + interpret-bearing
modifier; `gamescope_input_method` for nested); driven by hyprsc over IPC
(absolute dual-trackpad cursors per the extracted Deck spec — 55%/55%
regions, click-down commit, per-key-crossing haptic tick, concurrent
modality, the full button map); CSS-custom-property-style theming; voxtype
mic integration. **This is the first of its kind on Linux** — which is
exactly why it's the item worth shipping.

### W11 — Boot unlock (M for the code path; L for the graphical OSK)

Per [initramfs-unlock.md](research/initramfs-unlock.md): a pre-`encrypt`
mkinitcpio hook reading lizard-mode evdev (arrows/A/B), deriving a secret
from the sequence, delivered via `/crypto_keyfile.bin` (newline-free), with a
second LUKS keyslot and the keyboard passphrase always retained. Plymouth
untouched — the code entry happens against the existing splash with
`display-message` feedback. **W11b** (the bystander-safe graphical QWERTY, a
standalone DRM program in the unl0kr mould, randomized layout now optional
per the relaxed privacy bar) is separable and deferable — the everyday path
after W9 is suspend/resume, making cold boot rare.

### W12 — Tier 1: device ownership + focus routing (L) — **prioritized: the point of the project**

The [06 Tier 1](06-recommendation.md#tier-1--the-structural-answer) design:
deny Steam the physical device (udev mechanism still to be verified — the
`TAG-=`/uaccess question stands), hyprsc feeds a virtual controller only when
Steam/game focused; bare-guide passthrough synthesized on release; Steam's
chords rendered inert by construction. Target choice per the 06 table
(uinput generic vs `deck-uhid`-class clone). This completes the input
contract in [08](08-living-room-vision.md#input-contract). Sequenced late
deliberately: W2's passive tap plus Tier 0 covers most daily pain, and
Steam's own handling of this controller (#13185) should stabilize before a
clone imitates it.

### W13 — Deck port (L, later)

Constraints to carry now: **audit every always-on daemon for GPU/wakeup pins
on battery hardware** — voxtype's Vulkan mode held a persistent NVIDIA
context from the idle daemon, pinning the dGPU active (caught and reverted to
CPU on the laptop, 2026-08-31); hyprsc itself must never hold a GPU or
busy-poll. no tower-specific assumptions in W4's classifier
(internal-vs-external connector is the Deck signal); hyprsc's evdev path
must handle the Deck's built-in controller (hid-steam, kernel ≥7.3); W11's
posture decision diverges (portable device → keep strong secrets); embedded
gamescope questions return on AMD where they're actually supported.

## Suggested order

```
W0 ─ W1 ──► W2 ──► W12 ──► W3 ─► W4 ─► W5/W6/W7 ─► W8/W9 ─► W10 ─► W11 ─► W13
 experiments  daemon  OWNERSHIP glue  mode  living-room   lifecycle  OSK   boot  deck
```

*Reordered 2026-08-31 at the owner's direction: Tier 1 ownership (W12) is the
priority and moves directly after the daemon core.* Two things motivated it:
the `suppress_event` mitigation for Steam's guide focus-steal failed in
practice (Q17), and ownership obsoletes the entire mitigation category — the
input contract holds by construction when Steam only ever sees the virtual
controller. W10 can still start any time after W2 (they share only the IPC
contract). W11 floats freely.

## Risk register (top five)

1. **OSK scope** (W10). First-of-kind, XL, and the vision's typing
   experience lives or dies on it. Mitigation: the extracted Deck spec plus
   the 12-point checklist make it well-specified; ship dpad-only first
   internally even though full parity is the v1 bar.
2. **Device-denial mechanism unverified** (W12). udev tag clearing, bwrap
   namespace, and cgroup paths all unproven. Mitigation: prototype all three
   in an afternoon before designing around one.
3. **Nested gamescope itself needs tower validation** (W5). On the dev
   laptop, nested gamescope SIGABRTs (xwm thread, `_XIOError` → abort) when
   hosting clients — four for four, plausibly hybrid-GPU-specific (Q16). The
   HDR triple is additionally unvalidated in the field. Mitigation: W0 tests
   on the single-GPU tower; fallback for BPM is bare-XWayland windowed mode
   with its documented caveats, and for HDR plain-Hyprland Proton HDR.
4. **Steam moves.** #13185 fix, client updates to `controller_triton`
   handling, or a native Wayland client (CEF blocker now removed) could
   invalidate Tier 0/W12 assumptions. Mitigation: passive-tap core is
   Steam-independent by design; revisit W12 at each client beta.
5. **Fork drift.** HypXRland tracks Hyprland 0.56 with Lua config; upstream
   is dropping hyprlang. hyprsc should target the Lua dispatch surface and
   pin protocol usage to stable protocols only.

## Upstreaming targets (shippable-project dividends)

- `0x42` decode corrections → SDL3 / kernel `hid-steam` (post-7.3).
- The OSK → standalone project; wvkbd patches (keycode-127, magnify popup)
  upstream regardless.
- `RemoteDesktop`/`ConnectToEIS` portal backend interest → XDPH #252 thread.
- The compat-tool wrapper and classifier → omarchy extensions or standalone.
- The lock-plugin `authenticate()` method → omarchy PR (small, general).
