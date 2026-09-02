# Guide-hold power-off: who does it, and what to do about it

*Research note, 2026-09-01. Read-only investigation: no HID writes, no probes;
the controller was in use throughout. Everything marked **UNVERIFIED** is a
gap this note could not close without touching the device or Valve's JS-only
support page; §6 lists them. Evidence tags: `LOCAL` = observed on this machine,
`SRC` = read in source, `WEB` = third-party page.*

## The complaint

> Holding the Steam button turns the controller off. I'll be holding the guide
> button while being indecisive and it turns off the controller rather than
> waiting for me. I don't want a menu asking me to confirm either. I'd rather a
> different scheme to turn it off — maybe Steam already has a shortcut for this
> and we should use the same. I just don't like the current behaviour; give me
> options.

## 0. Verdict in one paragraph

**The firmware does it.** Steam is not running on this machine (`LOCAL`), the
kernel's `hid-steam` driver is not bound to the puck (`LOCAL`: all five
`28DE:1304` HID devices sit on `hid-generic`; the installed
`hid-steam.ko` only knows 1102/1142/1205), and hyprpad never sends a power-off
(`SRC`: no `0x9F` anywhere in `src/`). Steam's own chord layout for this
controller has **no** bare Steam-button long-press — its power-off is the
instant `guide+Y` chord the project already found, and the client does nothing
on a hold longer than ~3 s (`README.md:35-40`, `docs/03-hardware-findings.md:254-260`).
What remains is the controller, and Valve's own constants confirm the firmware
owns a knob for exactly this: **`SETTING_STEAMBUTTON_POWEROFF_TIME` (setting
25, `0x19`)**, written with the same `0x87 ID_SET_SETTINGS_VALUES` feature
report hyprpad already sends every 30 s for lizard mode. Its units, default and
whether `0` disables are **UNVERIFIED** (Valve's default table is not public);
the firmware can be *asked* (`0x89`/`0x8B`/`0x8C` reads) before anything is
written. **Top option:** read setting 25, then have hyprpad write it long (or
off) alongside lizard mode, and add a deliberate `h.controller_off()` action
(`0x9F ID_TURN_OFF_CONTROLLER`) on a chord that cannot be hit by accident, plus
an optional right-click on the status widget. If setting 25 turns out to be
inert on Triton, the fallback is a rumble warning a second before the firmware
fires (§3.D), which needs the same read to know *when*.

## 1. Who powers it off — three suspects, with evidence

### 1.1 The Steam client — ruled out for the bare hold

- **Not running.** `pgrep -x steam` → nothing; `ps` shows only the owner's
  `steam-game-touchpad.sh` helpers (`LOCAL`). Steam last ran 08:29–09:07 today
  (`~/.local/share/Steam/logs/controller.txt` tail). The daemon under test
  started 23:00 (`ps -p 871814`), so the present-tense complaint is happening
  with no Steam process at all.
- **Steam's chord layout for this controller has no long-press on the Steam
  button.** `~/.local/share/Steam/controller_base/chord_triton.vdf`
  ("Steam Button Chord Basic Configuration", `controller_type controller_triton`):
  - `button_b` → `Long_Press` (`long_press_time 2400`) →
    `controller_action quit_application` (lines 25-45);
  - `button_x` → `release` → `SHOW_KEYBOARD` (48-58);
  - `button_y` → `full_press` → **`controller_action controller_poweroff`** (60-70).
  That is the `guide+Y` chord `docs/experiments/w12-device-denial.md:268-272`
  already pinned on Steam (owner-verified: with Steam exited, `guide+Y` does
  *not* power off). The same `button_y` → `controller_poweroff` appears in
  `chord_steamcontroller_gordon.vdf:21-30` and `chord_xboxone.vdf:22-31`; the
  Deck's `chord_neptune.vdf` has **no** `controller_poweroff` at all (its
  long-presses are `quit_application`, `brightness_up/down`). The UI string is
  `"ControllerActionKey_Controller_PowerOff":"Turn Off Controller"`
  (`steamui/localization/steamui_english-json.js`).
- **Steam logs its own power-offs, and none look like a hold.** `controller.txt`
  has `Turn off Steam Controller` ×4 at 2026-08-31 16:13:48–16:15:23 — the
  `guide+Y` OSK dogfooding session from the w12 note — each immediately
  followed by `SetLocalControllerConnectionState … disconnected because Event
  from device`. Every other power-off line is `Turn off idle controller 0`
  (Steam's *client-side* idle timer, 2026-07-01 … 08-28). Next time the hold
  fires with Steam up, grep that file for the timestamp: a firmware power-off
  should show only the `disconnected because Event from device` line, without
  a preceding `Turn off Steam Controller`.
- **Steam ignores long holds by design.** `docs/03-hardware-findings.md:254-260`
  (live test): short hold → focus/launch Steam on release; "Hold longer than
  ~3 s → Nothing at all". A 2018 Valve staff reply on the 2015 controller says
  the same thing about chords: "The Steam Button chords that you use like the
  'Turn off controller' command are done from Steam, not via the FW"
  ([thread](https://steamcommunity.com/app/353370/discussions/1/2828702373004266928/)).
- **Masking would not help.** The bwrap launcher (`scripts/hyprpad-steam`,
  uhid doc §8.3) removes what Steam does; a firmware hold-off is unaffected.
  Masking does remove `guide+Y`, which is why the project kept `guide+Y` as the
  OSK toggle.

Caveat: on the 2015 controller the community is split on whether the 5 s
hold-off needs Steam ("handled by the controller itself and its own firmware"
vs. "if I quit steam … holding down the steam button … doesn't turn off the
controller",
[thread](https://steamcommunity.com/app/353370/discussions/0/358416600607003130/)).
For Triton the question is settled locally by the fact that it happens with no
Steam process.

### 1.2 The kernel driver (`hid-steam`) — ruled out

- `ls /sys/bus/hid/drivers/hid-steam/` → does not exist; `lsmod | grep hid_steam`
  → empty. All five puck HID devices (`0003:28DE:1304.006F`–`.0073`) are bound
  to **`hid-generic`** (`LOCAL`).
- `modinfo hid_steam` on this kernel (`7.1.9-arch1-2`) lists aliases for
  `p00001102`, `p00001142`, `p00001205` only — no `1302`/`1304`. Upstream
  support arrived in commit `0a80b4e8ec` "HID: steam: Initial 2026 Steam
  Controller support" (Vicki Pfau, 2026-08-12, `SRC` via `gh api`), which adds
  `USB_DEVICE_ID_STEAM_CONTROLLER_IBEX 0x1302`, `_IBEX_BLE 0x1303`,
  **`_PROTEUS 0x1304` (the puck, `STEAM_QUIRK_IBEX | STEAM_QUIRK_WIRELESS`)**,
  `_NEREID 0x1305` (Steam Machine receiver). It is not in this Arch kernel yet.
- Even once it binds, `hid-steam.c` **defines** `ID_TURN_OFF_CONTROLLER = 0x9F`
  and `SETTING_STEAMBUTTON_POWEROFF_TIME` but never sends either; the only
  settings it writes on IBEX/Deck are `SETTING_LIZARD_MODE 0` and
  `SETTING_STEAM_WATCHDOG_ENABLE 0` — the same pair `src/lizard.rs` writes.
  (Side note for later: `cd33a91d37` "Fully unregister controller when hidraw
  is opened" means the driver will step back when hyprpad opens the node.)

### 1.3 hyprpad — ruled out

`grep -rn -iE '0x9f|turn_off|power.?off' src/` → nothing. The daemon's only
feature reports are `0x81 CLEAR_DIGITAL_MAPPINGS`, `0x85 SET_DEFAULT_DIGITAL_MAPPINGS`
and `0x87 SET_SETTINGS_VALUES` with settings 9 and 71 (`src/lizard.rs:60-77,
107-176`); its only output reports are haptics/rumble (`src/haptics.rs`).
`GuideHold` (`src/gesture.rs:47-53`, fired once at `HOLD_THRESHOLD` = 300 ms,
`gesture.rs:60-63,144-148`) is a *signal*, bound to nothing in either config:
`grep -n hold config/hyprpad.lua` → only a comment; the owner's
`~/.config/hypr/hyprpad.lua` → only a comment (line 32). `hyprpad bindings`
lists no `guide_hold` row.

### 1.4 The firmware — by elimination, and it has a register for it

- Valve's `controller_constants.h` (shipped in SDL,
  `src/joystick/hidapi/steam/controller_constants.h`) has, in the
  `SETTING_*` enum: index **25 `SETTING_STEAMBUTTON_POWEROFF_TIME`** (line 452,
  right after `SETTING_SMOOTH_ABSOLUTE_MOUSE`), index **50
  `SETTING_SLEEP_INACTIVITY_TIMEOUT`** (line 483), 9 `SETTING_LIZARD_MODE`
  (432), 71 `SETTING_STEAM_WATCHDOG_ENABLE` (508), 78
  `SETTING_DEVICE_POWER_STATUS` (515); and `ID_TURN_OFF_CONTROLLER = 0x9F`
  (line 74). The kernel's `hid-steam.c` carries the identical enum (with
  `/* 20 */ … SETTING_STEAMBUTTON_POWEROFF_TIME` at 25 and `/* 50 */
  SETTING_SLEEP_INACTIVITY_TIMEOUT`), as does hhd's
  `src/hhd/controller/virtual/sd/const.py:142,170`.
- A community Triton reverse-engineering spec
  ([FossPrime/Steam-Controller-Auto-Charge, `triton_2026_steam_controller_spec.md`](https://github.com/FossPrime/Steam-Controller-Auto-Charge/blob/master/triton_2026_steam_controller_spec.md))
  glosses 25 as "Hold duration for Guide button to turn off controller" and 50
  as "Idle duration threshold before automatic power-down". It is a gloss, not
  a measurement — treat as a hint.
- Behaviour matches a firmware timer: it fires with no Steam process, with
  hyprpad purely passive on the guide button, and it needs a hold longer than
  the ~3 s the docs/03 test used without incident.
- The 2026 owners' hub describes the normal power-off as exactly this:
  "long-press the Steam button, each time I'm done playing and put it down"
  ([feature-request thread](https://steamcommunity.com/app/4165870/discussions/0/654855914503004847/)),
  with a reply pointing at `Y + Steam button` as the instant alternative and
  the "Guide Button Chord layout" as where to change it.

**What this note could not establish about the firmware:** the actual hold
duration on Triton (2015 lore says ~5 s; one 2026 guide says "hold … until the
unit completely turns off" without a number), the units and default of setting
25, whether `0` means "never", whether the value persists across a power cycle,
and whether the wired interface (`0x1302`) behaves the same. See §6.

## 2. The firmware settings interface hyprpad already uses

Everything needed to act on setting 25 is one function away from code that
already runs.

**Write path (exists).** `src/lizard.rs:115-131` builds the frame
`[0x01][0x87][len][id, lo, hi]…` zero-padded to 64 bytes (`WIRE_LEN`, report id
`REPORT_ID_FEATURES_CONTROLLER = 1`, `lizard.rs:58-85`), sends it with
`HIDIOCSFEATURE` (`lizard.rs:189-235`, 12× EPIPE retry) to each of the five
puck nodes, succeeding if any accepts (`apply_to_puck`, `lizard.rs:267-296`;
an all-STALL result means "controller asleep", exactly what `.play.log:77,89`
shows). `own_lizard_loop` re-sends every `RESEND_INTERVAL` = 30 s
(`lizard.rs:105,332-357`), so any extra setting rides along and survives a
reconnect for free. Byte-for-byte this is what SDL's Triton driver sends every
3 s (`SDL_hidapi_steam_triton.c:127-144,494-499`) and what `hid-steam.c`'s
`steam_write_settings` produces (`0x87 len (reg valLo valHi)*`).

**Read path (missing, small).** `hid-steam.c:433-500` (`steam_recv_report_id`)
and `steam_get_serial` (`hid-steam.c:667-690`, "Send: 0xae 0x15 0x01 / Recv:
0xae 0x15 0x01 serialnumber") show the round trip: `HIDIOCSFEATURE` the
command, then `HIDIOCGFEATURE(64)` on the same report id and parse
`[cmd][len][payload]`. With `0x89 ID_GET_SETTINGS_VALUES`, `0x8B
ID_GET_SETTINGS_MAXS` and `0x8C ID_GET_SETTINGS_DEFAULTS` (all in the enum,
uhid doc §4.3 lists them as safe to relay) the firmware reports the current,
maximum and default of setting 25 and 50 — this is the *only* way to learn the
units, since SDL declares `g_DefaultSettingValues[SETTING_COUNT]`
("default,min,max", `controller_constants.h:556-562`) but the `.c` is not
published.

**Units, from the one setting a third party does write.** sc-controller's
`configure()` (`scc/drivers/sc_dongle.py:295-352`) sends
`87 15 32 t_lo t_hi 18 00 00 31 02 00 08 07 00 07 07 00 30 gy 00 2e 00 00`,
i.e. seven `(id, u16)` pairs starting with **setting `0x32` = 50 =
`SETTING_SLEEP_INACTIVITY_TIMEOUT`, documented as "'idle_timeout' is in
seconds"**, default 600. So the inactivity timeout is a u16 in seconds (on the
2015 firmware; **UNVERIFIED** on Triton). Nothing public writes setting 25,
hence the read-first step.

**Power-off command.** `ID_TURN_OFF_CONTROLLER 0x9F`. sc-controller sends it as
`9f 04 6f 66 66 21` — length 4, payload `"off!"` (`sc_dongle.py:368-371`,
"Mercilessly stolen from scraw library"); its wired driver refuses
(`sc_by_cable.py:102`, "Ignoring request to turn off wired controller").
On IBEX the frame is `01 9f 04 6f 66 66 21` padded to 64, via the same
`send_sequence`. Neither SDL nor `hid-steam` ever send it; hhd/InputPlumber do
not either. Whether Triton needs the `"off!"` magic is **UNVERIFIED** — send it
as sc-controller does; a bare `01 9f 00` is the fallback to try. The uhid
relay design already reserves `0x9F` as "block from Steam; hyprpad decides when
the controller sleeps" (`docs/research/uhid-steam-controller.md:569`), so this
is consistent with the architecture rather than new policy.

## 3. Options

### A. Lengthen or disable the firmware hold (setting 25) — *recommended*

What it takes: a read (`0x89`/`0x8B`/`0x8C` of 25 and 50) to learn units and
range; then add `(25, value)` to the settings frame `own_lizard_loop` already
re-sends, driven by a config knob. A deliberate power-off (B) becomes the way
to turn it off; the inactivity timeout (E) remains the passive one.

Failure modes:
- *Units guessed wrong.* If the field is in 100 ms ticks and we write "60"
  meaning seconds, the hold gets *shorter*. Mitigation: read `0x8B` max and
  `0x8C` default first, write, read back, then time it with `hyprpad monitor`
  (last `0x42` timestamp before the stream stops).
- *`0` is not "never".* Then write the maximum the firmware reports.
- *The setting is inert on Triton.* The name dates from the 2015 firmware; the
  enum is shared, the implementation may not be. The read-back tells us if it
  is even stored; only the stopwatch tells us if it is honoured. If inert →
  option D.
- *Persistence.* Lizard/watchdog writes are known to persist past daemon exit
  (`docs/experiments/w2-live-validation.md:38-55`) but not across a power
  cycle (`.play.log:83-90` shows the disable re-applied after a reconnect).
  The 30 s resend covers it either way; a ≤30 s window after wake is the cost.
- *Steam.* SDL's Triton path writes only `LIZARD_MODE 0`; whether the Steam
  client writes setting 25 itself is **UNVERIFIED** (it reads
  `configset_controller_triton.vdf`, 24 bytes on disk — empty). If it does,
  hyprpad's 30 s resend wins within 30 s of any Steam write.
- *Wired.* Unknown whether the hold-off applies on USB-C; irrelevant to the
  puck setup.

### B. A deliberate power-off: `h.controller_off()` on an intentional chord

`Action::ControllerOff` → `lizard::turn_off_controller()` (the `0x9F` frame
through `apply_to_puck`). Because hidraw is non-exclusive it also works as a
CLI (`hyprpad off`) for scripts, `omarchy-menu` entries and the widget.

Where to bind it. The gesture engine chords are single-button
(`GestureChord(b)` per edge, `gesture.rs:127-135`; a true two-button
guide+L5+R5 needs engine work and is not worth it for this). Chordable buttons
(`gesture.rs:211-217`, names from `config.rs:290-318`) not bound in the owner's
config (`hyprpad bindings`): **`x`** (owner already declared it free),
`quickaccess`/`qam`, `l3`, `r3`, `l4`, `l5`, `dpad_up/down/left/right`,
`lpad_click`, `rpad_click`, and the left-stick flicks. Bound and unavailable:
`a b y l1 r1 l2 r2 r4 r5 menu view` and right-stick left/right. For "can't hit
it by accident", `guide+l4` or `guide+l5` (a *back* paddle, the opposite side
from the thumb on Steam) or `guide+qam` are the candidates; `guide+x` is the
most reachable if the owner would rather it be easy. This is the owner's call.

Optional, explicitly *not* a confirmation menu: the status-bar widget. Omarchy's
`WidgetButton` emits `signal pressed(int button)` and accepts
`Qt.LeftButton | Qt.RightButton | Qt.MiddleButton`
(`~/code/omarchy-bluetooth-friendly-name/shell/Ui/WidgetButton.qml:32,98,116`);
`shell/hyprpad.status/Widget.qml:235` currently does `onPressed:
root.openCheatSheet()` for every button. Right-click → `hyprpad off` is one
line, and choosing it is deliberate.

A longer `guide_hold` as the *off* gesture (bind `guide_hold` to
`controller_off`) is possible only once A works, and needs a second, longer
threshold: `HOLD_THRESHOLD` is a `pub const` 300 ms (`gesture.rs:63`) and is
also the "deliberate hold, reveal an overlay" signal. Ranked last: it is the
very gesture the owner is complaining about.

### C. Reuse "Steam's own shortcut"

Steam's shortcut on this controller is **`guide+Y`, instant** (`chord_triton.vdf:60-70`),
the same as for the 2015 controller and Xbox pads; the Deck's built-in
controller has none (a long Steam-button press there shows the shortcut
overlay — "you can see the entire list of Game Mode shortcuts by long
pressing the 'Steam' or the 'Quick Access' button",
[How-To Geek](https://www.howtogeek.com/882130/steam-deck-shortcuts-the-ultimate-guide/)).
So "the same as Steam" means `guide+Y`, which the owner deliberately kept as
the OSK toggle and masks Steam to protect (`w12-device-denial.md:274-281`,
memory). Mirroring it would mean moving the OSK toggle (to `guide+x`, say) and
binding `guide+Y` → `controller_off` in hyprpad, giving one power-off chord
whether or not Steam is running or masked. It is a coherent option; it just
reverses the earlier decision. Long-hold is *not* Steam's shortcut on any
Valve controller.

### D. Make the hold harmless: a warning a second before the firmware fires

If A is inert, hyprpad can still help: it knows exactly when the guide went
down (`guide_since`, `gesture.rs:144-146`). Add a deadline `hold_warn_at =
guide_since + (T − 1 s)` to the run loop's wake computation (`src/run.rs:391-420`
already merges `next_scan` and `rescan_at` into one `recv_timeout` deadline;
the engine update is at `run.rs:509`) and fire a rumble pulse, not a pad tick:
the pad actuators (`Haptics::tick/click/buzz`, `haptics.rs:297-347`) are under
the trackpads the thumb may not be touching, whereas `Haptics::rumble`
(`haptics.rs:347`, the gamepad path's native rumble) is felt through the body.
T comes from the setting-25 read if that works, else from a stopwatch. Failure
mode: the warning is only as accurate as T, and it does nothing if the owner
keeps holding — by design.

Accidental power-off recovery, as it stands: with the puck the hidraw nodes do
**not** disappear (they are the dongle's; `apply_to_puck` reports STALL rather
than ENOENT, and `.play.log` has no "controller disconnected" lines), so the
daemon never enters its 1.5 s reconnect scan (`run.rs:76,403-417`) — the
stream simply stops, and resumes when the Steam button is pressed to power the
controller back on. Wake latency on Triton is **UNVERIFIED** (not measured;
1–3 s by feel from the 2015 dongle). Lizard mode is re-disabled within ≤30 s.

### E. If the hold is disabled, how does the puck ever turn off?

1. **Firmware inactivity timeout** — `SETTING_SLEEP_INACTIVITY_TIMEOUT` (50),
   u16 seconds on the 2015 firmware (default 600 in sc-controller), same write
   path. Triton default **UNVERIFIED**; the read gives it. hyprpad can expose it
   as a second knob and, e.g., shorten it when the desktop is idle.
2. **Steam's client-side idle timer** — `Turn off idle controller 0` in
   `controller.txt` is Steam acting (Big Picture Settings → Controller, 15–120
   min or never); only when Steam runs.
3. **Deliberate** — B, or Steam's `guide+Y` when unmasked.
4. **Docked on the puck** — charging only; whether it standbys is firmware-
   version dependent ("It used to go to 'standby' (white light slowly
   blinking) but now, the controller stays on in the puck", 2026 hub thread).
5. **Wired (`0x1302`)** — bus-powered; sc-controller refuses `0x9F` on wire.

## 4. Recommendation

1. **A + B** (read setting 25/50 → write 25 long-or-off alongside lizard;
   `h.controller_off()` on `guide+l5` (or the owner's pick) and `hyprpad off`
   behind a right-click on the status widget). Keep E.1 as the passive
   power-off.
2. **D** if the setting-25 read-back shows it is stored but the stopwatch shows
   it is not honoured (a rumble at T − 1 s).
3. **C** only if the owner prefers one chord that means "off" in both worlds
   and is willing to move the OSK toggle off `guide+Y`.

### Tiny implementation plan for #1 (this repo)

Step 0 — **probe, when the controller is not in use** (it powers it off):
`hyprpad monitor`, hold Steam, note the last `0x42` timestamp → baseline T.

1. `src/lizard.rs`
   - constants: `SETTING_STEAMBUTTON_POWEROFF_TIME: u8 = 25`,
     `SETTING_SLEEP_INACTIVITY_TIMEOUT: u8 = 50`, `ID_GET_SETTINGS_VALUES = 0x89`,
     `ID_GET_SETTINGS_MAXS = 0x8B`, `ID_GET_SETTINGS_DEFAULTS = 0x8C`,
     `ID_TURN_OFF_CONTROLLER = 0x9F`.
   - generalise `disable_lizard_settings_report()` (lines 115-131) into
     `settings_report(&[(u8, u16)]) -> [u8; WIRE_LEN]` (`buf[2] = 3 * n`).
   - `pub fn read_settings(ids: &[u8]) -> Result<Vec<(u8,u16)>>`: send
     `[01][89][n][ids…]`, then `HIDIOCGFEATURE(64)` (`_IOC_READ|_IOC_WRITE`,
     nr `0x07`, same `hidiocsfeature` helper at 189-207 with nr changed) on
     the node that accepted; parse `[89][len][id lo hi]…`. Same for `0x8B`/`0x8C`.
   - `pub fn turn_off_controller() -> Result<(), String>`:
     `apply_to_puck(&[frame(0x9F, b"off!")])`.
   - `own_lizard_loop()` (332-357): take the power settings (an
     `Arc<RwLock<PowerSettings>>` filled from config, updated on reload) and
     append them to the disable frame.
2. `src/config.rs` / `src/lua_config.rs`: `Action::ControllerOff` (+ `"controller_off"`
   in the action parser at 153-190 and `h.controller_off()` in the Lua API);
   `h.daemon { steam_button_poweroff = <u16 | false> , sleep_timeout_s = <u16> }`
   next to `own_lizard` (config.rs 1033-1050). `src/bindings_sheet.rs` label:
   "Turn off controller". Update the TOML fixture in `lua_config.rs` if the
   sample gains the binding (the test that enforces parity will fail otherwise).
3. `src/run.rs`: `execute()` arm (2166-2180) → `lizard::turn_off_controller()`.
4. `src/main.rs`: `hyprpad off` and `hyprpad settings get <id…>` (read-only
   probe; prints current/default/max).
5. `shell/hyprpad.status/Widget.qml:235`: `onPressed: function(button) {
   if (button === Qt.RightButton) Util.execArgv(["hyprpad", "off"]); else
   root.openCheatSheet() }`; tooltip text to mention it.
6. `config/hyprpad.lua`: `h.bind("guide+l5", "Turn off controller", h.controller_off())`
   and the `h.daemon` knobs, commented with the read-first caveat.

Test protocol (owner not using the controller): read 25/50 → write → read back
→ stopwatch hold → power-cycle → read back (persistence) → `hyprpad off` →
press Steam to wake → confirm lizard re-disabled within 30 s.

## 5. Sources

Local (this machine, read-only):
- `README.md:35-40`; `docs/03-hardware-findings.md:209-263`;
  `docs/experiments/w12-device-denial.md:266-281`;
  `docs/experiments/w2-live-validation.md:38-55`;
  `docs/research/uhid-steam-controller.md:523-575` (§4.2–4.3, `0x9F` row at 569).
- `src/gesture.rs:45-63,127-148,211-217`; `src/config.rs:96-134,153-190,226-260,290-318`;
  `src/lizard.rs:58-105,107-176,189-296,332-357`; `src/run.rs:73-76,391-451,509,1945-2004,2153-2162`;
  `src/haptics.rs:152-163,297-347`; `src/hidraw.rs:40-62`; `src/main.rs:6-52`;
  `src/lua_config.rs:1005-1022`; `shell/hyprpad.status/Widget.qml:210-235,262-286`.
- `~/code/omarchy-bluetooth-friendly-name/shell/Ui/WidgetButton.qml:32,98,116`.
- `~/.config/hypr/hyprpad.lua:32,146-170`; `config/hyprpad.lua:170-200`;
  `hyprpad bindings` output; `.play.log:77-90`.
- `~/.local/share/Steam/controller_base/chord_triton.vdf:1-70`,
  `chord_neptune.vdf:29-39,192-234`, `chord_steamcontroller_gordon.vdf:21-30`,
  `chord_xboxone.vdf:22-31`; `steamui/localization/steamui_english-json.js`
  (`ControllerActionKey_Controller_PowerOff`); `logs/controller.txt:14788-24216,30219-30896,33519-34114`;
  `steamapps/common/Steam Controller Configs/51673999/config/configset_controller_triton.vdf` (24 bytes).
- `pgrep -af steam`; `ps -p 871814`; `modinfo hid_steam`; `/sys/bus/hid/devices/0003:28DE:1304.*/driver`;
  `journalctl -k` (puck re-enumeration 16:46:11–16, bootloader `28de:1007` → `1304`).

External:
- SDL `src/joystick/hidapi/steam/controller_constants.h` (lines 74, 432, 452, 483, 508, 515, 556-562):
  https://raw.githubusercontent.com/libsdl-org/SDL/main/src/joystick/hidapi/steam/controller_constants.h
- SDL `SDL_hidapi_steam_triton.c` (127-144, 494-499):
  https://raw.githubusercontent.com/libsdl-org/SDL/main/src/joystick/hidapi/SDL_hidapi_steam_triton.c
- Linux `drivers/hid/hid-steam.c` (device table, `ID_*`/`SETTING_*` enums, `steam_recv_report_id` 433-500, `steam_get_serial` 667-690, `steam_write_settings`):
  https://raw.githubusercontent.com/torvalds/linux/master/drivers/hid/hid-steam.c ;
  `drivers/hid/hid-ids.h` (`0x1302`–`0x1305`); commit `0a80b4e8ec` (2026-08-12) and `cd33a91d37`.
- sc-controller `scc/drivers/sc_dongle.py:133-138,295-352,368-371`, `scc/drivers/sc_by_cable.py:102`:
  https://github.com/kozec/sc-controller
- hhd `src/hhd/controller/virtual/sd/const.py:142,170`: https://github.com/hhd-dev/hhd
- FossPrime, *triton_2026_steam_controller_spec.md* (settings 25/50 glosses; PIDs 0x1304/0x1305):
  https://github.com/FossPrime/Steam-Controller-Auto-Charge/blob/master/triton_2026_steam_controller_spec.md
- Valve staff (austinp_valve, 2018) on chords being Steam-side:
  https://steamcommunity.com/app/353370/discussions/1/2828702373004266928/
- 2015 hold-to-off lore (5 s; firmware vs Steam dispute):
  https://steamcommunity.com/app/353370/discussions/0/358416600607003130/
- 2026 hub: "long-press the Steam button … put it down", `Y + Steam` alternative, puck standby change:
  https://steamcommunity.com/app/4165870/discussions/0/654855914503004847/
- Deck long-press = shortcuts overlay:
  https://www.howtogeek.com/882130/steam-deck-shortcuts-the-ultimate-guide/
- 2026 Bluetooth-mode guide ("hold down the central Steam button until the unit completely turns off"):
  https://gamingonsteam.com/2026/05/19/ditching-the-puck-how-to-switch-your-steam-controller-to-bluetooth-mode-and-back/
- Steam Support, *Steam Controller (2026) – Feature & Troubleshooting guide* (body is JS-rendered; not retrievable here):
  https://help.steampowered.com/en/faqs/view/33E8-5EDF-24E6-4CFB

## 6. What could not be verified

- The Triton firmware's actual hold-to-off duration (no number in any
  retrievable source; the 2015 controller's was ~5 s).
- Units, default, maximum and "0 = never?" semantics of
  `SETTING_STEAMBUTTON_POWEROFF_TIME` (25); whether Triton honours it at all;
  whether it persists across a power cycle. All answerable with the read
  probe in §4 step 1 and a stopwatch, once the controller is free.
- The Triton default of `SETTING_SLEEP_INACTIVITY_TIMEOUT` (50) and that its
  unit is still seconds.
- Whether `0x9F` on IBEX requires the `"off!"` payload.
- Whether the Steam client writes setting 25 or 50 itself on Triton.
- Whether the hold-off also applies on the wired interface (`0x1302`) and on
  Bluetooth (`0x1303`).
- Wake-from-off latency with the puck, and whether power-off vs. firmware
  inactivity sleep differ from hyprpad's point of view (both look like "stream
  stops, nodes stay").
- Valve's support article text (JS-only page; `curl`, browser-UA `curl`, and a
  render proxy all returned only the chrome).
- Whether the owner's incidents ever happened with Steam running (none of
  Steam's logged power-offs correspond to a hold; if it recurs with Steam up,
  `controller.txt` distinguishes the two — §1.1).
