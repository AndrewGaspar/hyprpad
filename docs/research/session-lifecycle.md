# Research: controller-driven lock / unlock / suspend / resume (consolidated agent report)

*Produced 2026-08-31. Integrates local inspection, three research strands, and
direct reading of the HypXRland compositor source. §3 and §4 are summarized
with cross-reference to [pam-auth-usb-wake.md](pam-auth-usb-wake.md).*

**Scope-correcting facts established up front (all VERIFIED locally):**

- **hyprlock and hypridle are not installed and not used.** Omarchy 4
  ("quattro") retired both. `/usr/bin/omarchy-upgrade-to-quattro` lists
  `hypridle` and `hyprlock` in `remove_retired_default_packages()`.
  `command -v hyprlock` and `pacman -Q hyprlock` both fail. The lock is now a
  **Quickshell** `WlSessionLock` client. `~/.config/hypr/hypridle.conf` and
  `~/.config/hypr/hyprlock.conf` still exist on disk but are dead — nothing
  reads them.
- **The inspected machine is a Framework Laptop 16, not the living-room
  tower.** All hardware readings must be re-verified on the tower (Q11).
- **This machine offers only s2idle.** `cat /sys/power/mem_sleep` →
  `[s2idle]` (no `deep`). A desktop board likely differs, and that difference
  is decisive for wake behavior (§4).
- **The compositor is HypXRland**, the user's own Hyprland 0.56.2 fork
  (`hypxrland-omarchy 1.1.0`, `URL: https://github.com/AndrewGaspar/Hyprland`),
  running from `/usr/lib/hypxrland/Hyprland`. A source checkout of exactly
  this tree is at `/home/ajg/code/hypxrland` — §2's verdict is read from that
  source, not inferred from web reports. Patching the compositor is therefore
  on the table.
- **The session locked itself on the idle timer during this investigation**
  (`hyprctl locked` → `true`) — proof the pipeline below is live.

## §1 — Omarchy 4 lock / idle pipeline (VERIFIED, local files quoted)

Everything runs inside the Quickshell process (`quickshell 0.3.1`, launched as
`quickshell -n -p $OMARCHY_PATH/shell`). `OMARCHY_PATH` here is a dev
checkout, `/home/ajg/code/omarchy-bluetooth-friendly-name`, symlinked into
`PATH`; the installed copy lives at `/usr/share/omarchy`.

### The chain

```
IdleMonitor 150s -> omarchy-launch-screensaver
IdleMonitor 300s -+
Ctrl+Super+L      +-> omarchy-system-lock -> omarchy-shell lock lock -> (qs ipc call lock lock)
sleep-lock svc   -+         |                      |
                            |                      v  Quickshell lock plugin: WlSessionLock.locked = true
                            |                      v  PamContext(omarchy-lock-password) / (omarchy-lock-fingerprint)
                            |              finishUnlock() -> omarchy-system-wake
                     (also: 1password --lock, kill screensaver, hyprctl switchxkblayout all 0)
```

### Idle detection — `shell/plugins/services/idle/Service.qml`

A Quickshell `IdleMonitor` (the `ext_idle_notifier_v1` protocol) with
`respectInhibitors: true`. Timeouts read from `~/.config/omarchy/shell.json`:

```json
{ "idle": { "lock": 300, "screensaver": 150 } }
```

At the screensaver timeout it runs `omarchy-launch-screensaver`; at the lock
timeout it runs `omarchy-system-lock`. IPC target `idle` exposes
`status / enable / disable / toggle`. The stay-awake ("caffeine") state is the
file `~/.local/state/omarchy/indicators/stay-awake`, toggled by
`omarchy-toggle-idle`; while present, `idleEnabled` is false and no
lock/screensaver fires.

### Lock service — `shell/plugins/lock/Service.qml`

A `WlSessionLock` (ext-session-lock-v1) whose `WlSessionLockSurface` hosts
`LockView.qml`. The password field is a plain Qt `TextInput`
(`echoMode: TextInput.Password`, `activeFocusOnPress: true`). Two independent
PAM flows:

```qml
PamContext { id: passwordPam;    config: "omarchy-lock-password";    user: root.userName
  onResponseRequiredChanged: root.respondToPasswordPrompt()
  onCompleted: function(result) { if (result === PamResult.Success) root.finishUnlock() else root.handlePasswordFailure() } }
PamContext { id: fingerprintPam; config: "omarchy-lock-fingerprint"; user: root.userName }
```

`config:` maps to the `/etc/pam.d/<name>` service name — confirmed by
`strings /usr/bin/quickshell` showing `pam_start_confdir` and `/etc/pam.d`.
The plugin also has robust handling for orphaned locks (`strandedLock`
recovery after a shell restart) and a suspend-aware blank timer (§5).

**The lock plugin's IPC surface** (`IpcHandler { target: "lock" }`) is:

```
lock() -> "ok"|"failed"|"missing-pam"     isLocked() -> "true"|"false"
status() -> JSON                          preview() / hidePreview()
```

There is **no `unlock` and no `authenticate`** exposed today. That is the
single extension point verdict (b) targets.

### Lock trigger scripts (all under `$OMARCHY_PATH/bin/`)

- **`omarchy-system-lock`** — `omarchy-shell lock lock >/dev/null`, then
  `hyprctl switchxkblayout all 0`, then locks 1Password and kills the
  screensaver.
- **`omarchy-apply-lock`** — the PAM installer (`omarchy:requires-sudo=true`).
  Writes `/etc/pam.d/omarchy-lock-password` and, only when fingerprints are
  enrolled, `/etc/pam.d/omarchy-lock-fingerprint`. Current on-disk contents:

```
# /etc/pam.d/omarchy-lock-password
#%PAM-1.0
auth       required                    pam_faillock.so preauth silent deny=10 unlock_time=120
-auth      [success=2 default=ignore]  pam_systemd_home.so
auth       [success=1 default=bad]     pam_unix.so try_first_pass nullok
auth       [default=die]               pam_faillock.so authfail deny=10 unlock_time=120
auth       optional                    pam_permit.so
auth       required                    pam_env.so
auth       required                    pam_faillock.so authsucc
account    include                     system-local-login

# /etc/pam.d/omarchy-lock-fingerprint
#%PAM-1.0
auth       required                    pam_fprintd.so
account    include                     system-local-login
```

- **`omarchy-system-wake`** — `omarchy-brightness-display on;
  omarchy-brightness-keyboard restore; omarchy-hyprland-monitor-clamshell`.
- **`omarchy-hyprland-session-locked`** — exits 0/1/2 by inspecting
  `solitaryBlockedBy` for `LOCK`. Its own comment claims "Hyprland reports no
  lock state directly" — **out of date** (see below).

### Lock state is readable three ways — all work while locked

1. **`hyprctl locked`** → `true` / `false`; `hyprctl -j locked` →
   `{"locked": true}`. Upstream Hyprland
   (`registerCommand({"locked", true, getIsLocked})`, PR #6042, present in
   the fork). It contradicts the stale comment above.
2. **`omarchy-shell lock status`** → rich JSON
   (`locked/requested/pending/sessionLocked/secure/realScreens/passwordPam/fingerprint/authenticating/lastEvent`).
   Live sample while locked:
   `{"locked":true,...,"sessionLocked":true,"secure":true,...}`.
3. **`omarchy-hyprland-session-locked`** (exit code).

**Critical for hyprsc: both `hyprctl` and the Quickshell IPC socket answer
normally while the session is locked** — verified live during the self-lock.
The IPC socket is `/run/user/1000/quickshell/by-id/*/ipc.sock` (mode
`srwxr-xr-x`, inside `0700` `/run/user/1000`).

### No logind lock integration (VERIFIED)

Grepping all of `$OMARCHY_PATH` for `lock-session`, `SetLockedHint`,
`LockedHint`, or the login1 `Lock`/`Unlock` D-Bus signal returns nothing.
`loginctl show-session 1` reports `LockedHint=no` even while the screen is
genuinely locked, and `loginctl lock-session` does **not** lock Omarchy. This
differs from the old hypridle model. The only logind-driven hook now is the
**sleep** path (§5).

## §2 — ext_session_lock_v1 vs virtual-keyboard injection

**Verdict: `zwp_virtual_keyboard_v1` injected keys DO reach the lock
surface's password field. VERIFIED from compositor source**
(`/home/ajg/code/hypxrland`), independently corroborated by community
evidence.

### The source path (four load-bearing facts)

1. **Virtual keyboards are ordinary keyboards.**
   `src/devices/VirtualKeyboard.hpp`:
   `class CVirtualKeyboard : public IKeyboard`.
   `CInputManager::newVirtualKeyboard` pushes them onto the same `m_keyboards`
   vector as physical ones. Live proof: `hyprctl devices` lists
   `hl-virtual-keyboard-fcitx5` beside the physical keyboards.
2. **One shared key path with no session-lock check.**
   `src/managers/input/InputManager.cpp:1642` `onKeyboardKey()` handles every
   keyboard. The only virtual-specific gate is `shouldIgnoreVirtualKeyboard()`
   (line 1763), which returns true **only** when the virtual keyboard's
   `wl_client` is the active input-method grab client. An hyprsc keyboard is
   not the IME grab client, so events flow through
   `g_pKeybindManager->onKeyEvent(...)` then
   `g_pSeatManager->sendKeyboardKey(...)` — identical to physical. **There is
   no `isSessionLocked()` anywhere in `onKeyboardKey`.**
3. **The lock surface owns keyboard focus.**
   `src/desktop/state/FocusState.cpp:227`:
   ```cpp
   if (g_pSessionLockManager->isSessionLocked() && pSurface && !g_pSessionLockManager->isSurfaceSessionLock(pSurface))
       return;
   ```
   So `sendKeyboardKey` necessarily lands on the lock surface.
4. **Binding both protocols is unprivileged.** `src/protocols/VirtualKeyboard.cpp`
   has no lock gating; `CSessionLockProtocol::bindManager` has no privilege
   check at all. The only global filter (`Compositor.cpp` `filterGlobals`)
   restricts privileged globals **only for
   Flatpak-`wp_security_context_v1`-sandboxed clients**, and neither
   `ext_session_lock_manager_v1` nor `zwp_virtual_keyboard_manager_v1` is even
   on the privileged list. A user-run daemon gets both unconditionally.
   Empirically: `wtype -M shift -m shift` returns exit 0 as an ordinary user.

### Corroboration (VERIFIED, web)

Every Wayland on-screen keyboard (wvkbd, sysboard, squeekboard) is a
`zwp_virtual_keyboard_v1` client and types into lock screens. The Hyprland
implementer states the mechanism in
[PR #9793](https://github.com/hyprwm/Hyprland/pull/9793): the `abovelock`
layer rule grants only *pointer* focus, "keyboard focus will stay on the
lockscreen." **Corollary for hyprsc: a headless daemon needs no layer surface
and no rendering — `abovelock` exists only so a human can see/click an OSK.**
The recurring "virtual keyboards don't work on lock screens" claim is always
about *visibility*, never input routing. uinput injection (ydotool) is even
more certain — such devices have `isVirtual()==false` and there is no hook to
filter them; [hyprlock #997](https://github.com/hyprwm/hyprlock/issues/997)
confirms ydotool typing into a lock field as a first-party-acknowledged
workaround.

### Three additional VERIFIED facts that shape the design

- **A virtual keyboard triggers `locked = true` keybinds while locked.**
  `KeybindManager.cpp:650`:
  `if (!k->locked && g_pSessionLockManager->isSessionLocked()) continue;` —
  and this runs for virtual keyboards too. Omarchy already relies on locked
  binds (`default/hypr/bindings/media.lua` marks all volume/brightness/media
  binds `{ locked = true }`). **This is a second, independent
  controller-to-action channel during lock that never touches the password
  field.**
- **A virtual keyboard alone satisfies the seat's keyboard-capability
  requirement** (`anyHidHasCap(HID_INPUT_CAPABILITY_KEYBOARD)`), so a **tower
  with no physical keyboard** still gets working lock-surface focus once
  hyprsc creates its virtual keyboard.
- **`input:virtualkeyboard` knobs that will bite a synthetic-input daemon:**
  `release_pressed_on_close` (default false — a daemon dying mid-keystroke
  leaves the key logically held), `share_states` (default 2 — merges modifier
  state), `misc:name_vk_after_proc` (default true — names the device
  `hl-virtual-keyboard-<binary>`).

### If a lock client dies (VERIFIED from source + spec)

`SessionLockManager::shallConsiderLockMissing()` compares `lockTimer` to
`misc:lockdead_screen_delay` (1000 ms here); `Renderer.cpp:1430` stops
rendering workspaces. Per the ext-session-lock spec, the compositor **must
not** unlock on client death — "acceptable for the session to be permanently
locked." So `pkill` is never an unlock path. This is exactly why the
recommended approach extends the existing lock plugin rather than
killing/replacing it. Note `misc:allow_session_lock_restore` is **on** here
(set in `default/hypr/looknfeel.lua:114`, confirmed `hyprctl getoption` →
`true`), which is what makes the plugin's `strandedLock` recovery work.

### Two ways to drive unlock from a controller

- **Path A — inject the password via `zwp_virtual_keyboard_v1`** (or via the
  puck's real lizard-mode HID keyboards, which are physical by every
  definition — `hyprctl devices` lists
  `valve-software-steam-controller-puck-keyboard{,-1,-2,-3}`; but lizard mode
  is arrows/Enter/Esc, not alphanumeric). Works, but stores the login
  password in hyprsc's memory.
- **Path B — extend the lock plugin over IPC (RECOMMENDED).**
  `omarchy plugin clone omarchy.lock` copies the plugin to
  `~/.config/omarchy/plugins/<user>.lock/`, rewrites the id, sets
  `omarchy.clonedFrom` (which **preserves the `lock` IPC target name**), swaps
  out the built-in, and hot-reloads on save. A cloned plugin
  (`ajg.workspaces`) already runs on this box, so the path is proven. Add:
  ```qml
  function authenticate(password: string): string { root.submitPassword(password); return "ok" }
  ```
  This keeps PAM, `pam_faillock`, and fingerprint fully in the loop. Prefer
  it over a bare `unlock()` that calls `finishUnlock()` directly. **Security
  note:** the IPC socket is reachable by any process with the user's uid, so
  a bare `unlock()` would let any user-uid process dismiss the lock;
  `authenticate(password)` does not (caller must know the secret). The
  pre-existing posture is already loose in this direction (unprivileged
  `bindManager` + `allow_session_lock_restore=true` mean any user process can
  already take over the lock).

## §3 — PAM: a controller-code as a second factor (summary; full detail in [pam-auth-usb-wake.md](pam-auth-usb-wake.md))

**Feasible and lower-risk than usual**, because
`/etc/pam.d/omarchy-lock-password` is a dedicated per-service file whose
`auth` block does **not** `include system-auth` — editing it cannot touch
`sudo`/`login`/sshd. Omarchy also ships an anti-lockout failsafe:
`lock/Service.qml` watches that file with a `FileView`, gates
`passwordPamConfigured`, and `beginLock()` returns `"missing-pam"` and
refuses to lock if it fails to load — so a broken file stops locking rather
than locking you out.

Mechanisms, best first:
1. **`libpam_pwdfile`** — in Arch `extra` (2.0-2).
   `auth required pam_pwdfile.so pwdfile=/etc/omarchy-lock.pwd` validates a
   short secret distinct from the login password, hashed (yescrypt/bcrypt).
   No custom code, no cleartext. Cleanest answer.
2. **`pam_exec`** (installed). Three man-page corrections verified from
   source: `expose_authtok` **is** supported in the `auth` phase; the module
   **prompts for itself** when `PAM_AUTHTOK` is unset (there is **no
   `use_first_pass` option at all**); and **any non-zero exit maps to
   `PAM_SYSTEM_ERR`, not `PAM_AUTH_ERR`**. Token arrives on stdin with no
   trailing newline. **Precedent already on this box:** `/etc/pam.d/sudo`
   line 1 is
   `auth [success=1 default=ignore] pam_exec.so quiet /usr/bin/omarchy-hw-laptop-closed`.

Not installed: `pam_pwdfile`, `pam_yubico`, `pam_u2f`, `pam_python`.
Installed: `pam_exec`, `pam_fprintd`, `pam_faillock`, `pam_permit`,
`pam_succeed_if`, `pam_listfile`.

**Entropy** (N=12 usable buttons, `H = L*log2(12)`), against the existing
`deny=10 unlock_time=120`:

| L | bits | space | median online-guess time |
|---|---|---|---|
| 4 | 14.3 | 20,736 | 1.4 days |
| 6 | **21.5** | 2.99M | 208 days |
| 8 | 28.7 | 430M | 82 years |

Baselines: 4-digit PIN 13.3 bits, 6-digit PIN 19.9 bits. **L=6 is the sweet
spot.** The real exposure is **shoulder-surfing, materially worse than a
keyboard**: gamepad presses are large, open-hand, acoustically
distinguishable, ~3.6 bits each, unoccluded at living-room distance — assume
any code entered in front of someone is burned. But the threat model is
house-guest/child physical access, where 21.5 bits + 120 s lockout is ample.
**Store an argon2id/bcrypt hash, never plaintext**, pin the button-to-symbol
map in the helper, test with `pamtester` while keeping a root TTY open. No
prior art exists for gamepad-driven PAM — this would be first.

## §4 — Suspend and wake-from-suspend (summary; full detail in [pam-auth-usb-wake.md](pam-auth-usb-wake.md))

**Suspend is trivial and unprivileged:** `busctl ... CanSuspend` → `"yes"`;
`systemctl suspend` from hyprsc just works (polkit grants suspend to an
active local session).

**Wake: the whole chain is armed here and the puck advertises the
capability.** VERIFIED local readings (Steam Controller Puck = `28de:1304`):

| Link | Value |
|---|---|
| Puck `bmAttributes` | `0xa0` → **bit 5 set = USB remote-wakeup capable** |
| `/sys/bus/usb/devices/3-2.1/power/wakeup` | `enabled` |
| Registered wakeup source | `/sys/class/wakeup/wakeup76` name `3-2.1` |
| Parent hub `3-2` / root hub `usb3` | **`disabled`** <- the one weak link |
| PCI xHCI `0000:c4:00.0` wakeup / `/proc/acpi/wakeup` `XHC0` | `enabled` / `S3 *enabled` |
| `/sys/power/mem_sleep` | `[s2idle]` only, no `deep` |

**The udev rule is upstream and correct.**
`/usr/lib/udev/rules.d/60-steam-input.rules` line 17 is canonical
guard-then-assign, not a bug. The line 16 comment names the **"Steam
Controller 2026 receiver"** and **"Steam Machine Bluetooth"** explicitly —
direct upstream evidence Valve's 2026 puck is designed to wake the host via
ordinary USB remote wakeup.

**Field reports:** 2.4 GHz dongles wake Linux PCs; Bluetooth controllers do
not. **The dominant real-world problem is the opposite — dongles wake the box
too eagerly**
([ValveSoftware/SteamOS#2641](https://github.com/ValveSoftware/SteamOS/issues/2641),
open). On the tower, check `mem_sleep`; if `deep` is offered, try it first (a
Steam user reports the 2026 controller "has trouble waking PCs from modern
standby"). BIOS "Wake from USB" on + **ErP/EuP off** gates everything.
Diagnose wakes by diffing `/sys/class/wakeup/*/event_count`, then
`/proc/acpi/wakeup`, then `journalctl -b -k --grep 'PM:'`. Enable the whole
bus with a udev rule before testing; changing `power/wakeup` on an
already-suspended device only takes effect next cycle.

## §5 — On resume, the session comes back locked (VERIFIED, engineered carefully)

Not a fragile `before_sleep_cmd` but a dedicated inhibitor-based service:

- **`omarchy-sleep-lock.service`** (user unit,
  `WantedBy=graphical-session.target`, `Restart=always`) runs
  `omarchy-system-sleep-monitor`, which `exec`s
  `systemd-inhibit --what=sleep --mode=delay --who=Omarchy --why="Lock screen
  before suspend"` and watches D-Bus for `login1.Manager.PrepareForSleep`.
- On `boolean true` it runs **`omarchy-system-sleep-lock`**, which derives a
  budget from logind's real `InhibitDelayMaxUSec` (keeps a fifth in reserve,
  caps at 12 s), requests the lock over IPC, then **polls
  `omarchy-shell lock status` until `.secure == true`** before releasing the
  inhibitor. On timeout it fires a critical notification: *"Screen did not
  lock before suspend."*
- Omarchy widens logind's window:
  `/etc/systemd/logind.conf.d/20-inhibit-delay.conf` →
  `InhibitDelayMaxSec=15`. Also
  `/etc/systemd/logind.conf.d/10-ignore-power-button.conf` →
  `HandlePowerKey=ignore`.
- On resume the lock plugin's `idleBlankTimer` detects the wall-clock gap
  (`Date.now() - armedAt > interval + 2000`) and re-arms instead of blanking
  the freshly-woken unlock screen (`lock/Service.qml:420` region).

**So the session does come back locked, and the mechanism waits for `secure`
rather than hoping.** hyprsc should `systemctl suspend` and let this run — do
not lock manually first.

## §6 — SDDM with a gamepad (verdict: "never log out, only lock")

Current config: `/etc/sddm.conf.d/autologin.conf` → `[Autologin] User=ajg`,
`Session=omarchy-xr.desktop`; `/etc/sddm.conf.d/10-wayland.conf` → Wayland
greeter via `start-hyprland`. Logout (`omarchy-system-logout` → `uwsm stop`)
is the only path that surfaces the greeter.

- **Qt6 has no gamepad support (VERIFIED).** Qt Gamepad was never ported to
  Qt 6 (maintainer,
  [qt-project dev list Mar 2021](https://lists.qt-project.org/pipermail/development/2021-March/041125.html);
  absent from [qtmodules](https://doc.qt.io/qt-6/qtmodules.html)). The SDDM
  Qt/QML greeter cannot see a joystick.
- **Every SDDM/LightDM gaming distro autologins and never renders the
  greeter, using `Relogin=true`.** VERIFIED across SteamOS (Valve's SDDM
  fork, `steamos.conf` with `Relogin=true`), Bazzite Deck/HTPC
  (`bazzite-autologin.service`), ChimeraOS (LightDM with *no greeter package
  installed*), Jovian-NixOS (`jovian.steam.autoStart` →
  `sddm.autoLogin.relogin`). Mode-switching is universally "rewrite
  `Session=`, kill the session, let Relogin re-fire"
  ([SDDM Configuration.h](https://github.com/sddm/sddm/blob/develop/src/common/Configuration.h);
  [sddm.conf.5](https://man.archlinux.org/man/sddm.conf.5)).
- **Actionable gap:** Omarchy's autologin.conf lacks `Relogin=true`. Adding
  it makes even an accidental logout land back in the session instead of a
  controller-dead greeter. (Trade-off: precludes "log out to switch users.")
- A root uinput mapper *would* drive the greeter (uinput devices get
  seat-tagged by systemd's `71-seat.rules`, INFERRED for SDDM specifically),
  and the puck's lizard mode is the zero-software version
  (community-confirmed at BIOS/GRUB) — but this is belt-and-braces insurance,
  not the design.

**Verdict: never log out, only lock. Add `Relogin=true` as the safety net.**

## §7 — Cold-boot handoff: LUKS → autologin (VERIFIED; simpler than assumed)

**There is no credential cascading and no keyring keyed to a login
password.** The chain:

1. **Limine → initramfs.** `HOOKS=(base udev plymouth keyboard autodetect
   microcode modconf kms keymap consolefont block encrypt filesystems fsck
   btrfs-overlayfs)`. Note **`keyboard` precedes `autodetect`**, so all
   USB-HID keyboard modules are bundled unconditionally. Plymouth draws the
   passphrase prompt.
2. **LUKS passphrase typed once — the only authentication.**
3. **SDDM autologin.** `/etc/pam.d/sddm-autologin` is `pam_env` +
   `pam_shells` + `pam_nologin` + **`pam_permit`** — it authenticates
   *nobody* by design. No password exists to cascade.

So "Omarchy preserves the LUKS password through SDDM" is more precisely:
**the LUKS passphrase is the login, and SDDM is configured not to ask
again.**

**One caveat:** `gnome-keyring 1:50.0` is installed and
`pam_gnome_keyring.so` is wired into `/etc/pam.d/sddm-autologin` (both `auth`
and `session auto_start`), but since no login password is ever typed, a
keyring keyed to it cannot auto-unlock. Expect a keyring prompt on first use
or an empty-password keyring — **verify on the tower** before declaring
boot-to-BPM seamless.

**For a controller at the LUKS prompt (Q12):** the `keyboard` hook enumerates
the puck and Steam isn't running yet, so lizard mode is active — but lizard
mode is arrows/Enter/Esc, **not alphanumeric**. A button-sequence passphrase
there needs either a custom initramfs input handler or a second LUKS keyslot
enrolled with a code expressible in lizard-mode keys. This is the hardest
link in the chain.

## §8 — Living-room / TV vs desk classification

**Key reframing (VERIFIED from gamescope source): "docked" is not detected,
it is declared by connector type.** gamescope's `GetScreenType()` is purely
`connector_type == eDP/LVDS/DSI ? INTERNAL : EXTERNAL`
([DRMBackend.cpp](https://github.com/ValveSoftware/gamescope/blob/master/src/Backends/DRMBackend.cpp));
SteamOS policy is `--prefer-output '*,eDP-1'`. No dock-specific USB/PD/ACPI
signal exists anywhere in the stack; jovian-nixos adds nothing. Valve
persists per-display mode state keyed on **`make model`** (no serial) in
`~/.config/gamescope/modes.cfg`.

**Omarchy already ships the primitives:**
- `omarchy-hw-external-monitors` — scans `/sys/class/drm/card*-*/status`,
  skipping `eDP|LVDS|DSI`. Works pre-Hyprland.
- `omarchy-hyprland-monitor-external-active` — `hyprctl monitors all -j |
  jq -e '...select(name not ^(eDP|LVDS|DSI))... select(.disabled==false)'`.
- **`omarchy-hw-match <pattern>`** — greps DMI `product_name`/`product_family`.
  **Strongest tower-vs-laptop signal, free and deterministic.**
- **`omarchy-toggle <flag>` / `omarchy-toggle-enabled <flag>`** — flag files
  under `~/.local/state/omarchy/toggles/`. Ready-made manual-override
  convention.

**Signals ranked (VERIFIED unless noted):**
1. **DMI product name** (`omarchy-hw-match`) — deterministic, pre-Hyprland.
   If "living room" == "the tower," stop here.
2. **Manual override flag** (`omarchy-toggle living-room`) — always include.
3. **Connector name** (`HDMI-A-*` vs `DP-*` vs `eDP-*`) — the practical
   discriminator every community script uses; pre-Hyprland via sysfs.
4. **`physicalWidth`/`physicalHeight`** from `hyprctl monitors -j` (mm, EDID)
   — a 55" TV vs 27" monitor is unambiguous by diagonal; cleaner than any
   EDID "TV bit."
5. **Monitor `description`** — but note the JSON `description` is the *short*
   form (`make model serial`, commas stripped) while `desc:` config matching
   is a **prefix match** against short or long (`… (connector)`) form, no
   globs ([Monitor.cpp](https://github.com/hyprwm/Hyprland/blob/v0.56.2/src/output/Monitor.cpp)).
6. **ELD** (`/proc/asound/card*/eld#*`) when a sink is attached —
   `connection_type` (`"HDMI"`/`"DisplayPort"`), `monitor_name`, and numeric
   `manufacture_id`/`product_id`
   ([hda_eld.c](https://github.com/torvalds/linux/blob/v6.12/sound/pci/hda/hda_eld.c));
   the numeric IDs are a better stable key than the serial. All eight nodes
   read `eld_valid 0` here with nothing attached — re-test on the tower.

**Do not build on `serial`:** libdisplay-info's own header (the library
aquamarine uses) says make/model/serial strings are *"not meant to be used in
programmatic decisions, configuration keys, etc."*
([info.h](https://gitlab.freedesktop.org/emersion/libdisplay-info/-/blob/main/include/libdisplay-info/info.h));
TVs are the exact failure case. There is no reliable "this is a TV" EDID bit.

**Hotplug wiring:** classify once in autostart, subscribe to socket2
`monitoraddedv2`/`monitorremovedv2` (payload `ID,NAME,SHORTDESCRIPTION`).
Copy the community patterns: `monitoradded` can fire *before the output is
usable* (retry with backoff), debounce with `flock` in `$XDG_RUNTIME_DIR`,
and **exclude Hyprland's synthetic `FALLBACK` output** from any monitor
count. Every surveyed tool (kanshi/shikane/way-displays, autorandr) makes
manual overrides *temporary*, cleared on next hotplug — a sticky
`$XDG_STATE_HOME` file is a deliberate departure. Doc-path note:
`wiki.hypr.land/Configuring/Monitors/` and `/IPC/` now 404; live paths are
lowercase (`/configuring/core/monitors/`, `/ipc/`).

**Recommendation:** `DMI match → manual toggle → connector name + physical
size`, result written to a state file everything else reads. Don't be clever
about EDID.

## Feasibility verdicts

**(a) Controller unlock of the lock screen via virtual keyboard — FEASIBLE,
HIGH CONFIDENCE (VERIFIED from compositor source).** Virtual-keyboard key
events traverse the identical path as physical keyboards; the lock surface
holds exclusive keyboard focus; nothing gates protocol binding or key
delivery on lock state; and a virtual keyboard alone satisfies the seat's
keyboard capability (so a keyboard-less tower works). Not yet confirmed by
live injection (deliberately not run against the live lock). **60-second
confirmation test:** with the screen locked, `wtype -k Escape` and watch the
backlight return (`LockView`'s `Keys.onPressed` → `wakeRequested()`); Escape
clears the field, costs nothing, cannot trip faillock. Caveat: this path
stores the login password in hyprsc's memory — prefer (b).

**(b) Custom / extended lock client — FEASIBLE, AND THE BEST OPTION.** Don't
write an ext-session-lock client from scratch (waylock's `src/Lock.zig`,
~17 KB, is the minimal correct skeleton if ever needed).
`omarchy plugin clone omarchy.lock` gives a supported, upgrade-safe copy with
the IPC target name preserved and hot-reload; add an `authenticate(password)`
function calling the existing `submitPassword()`, keeping
PAM/faillock/fingerprint intact. Combine with a `libpam_pwdfile` line in
`/etc/pam.d/omarchy-lock-password` and the controller code becomes a genuine
hashed second factor with no cleartext and no custom crypto. Omarchy's
`FileView` PAM failsafe means mistakes stop locking rather than lock you
out. Low risk, entirely within supported extension points.

**(c) Controller wake-from-suspend — LIKELY, UNVERIFIED ON THE TARGET.**
Every checkable precondition is met (puck advertises remote wakeup `0xa0`,
udev enables `power/wakeup`, upstream Valve names the 2026 receiver as a
wake source, field reports confirm 2.4 GHz dongles wake Linux). Three
tower-side unknowns: BIOS ErP must be off; check `/sys/power/mem_sleep` for
`deep` and try it before s2idle; enable `wakeup` bus-wide (the
intermediate/root hubs read `disabled` here). Budget for the *opposite*
failure — spurious wake on dongle re-enumeration
([SteamOS#2641](https://github.com/ValveSoftware/SteamOS/issues/2641), open)
is what pushes people off manual suspend.

**(d) Seamless boot-to-BPM — FEASIBLE EXCEPT THE FIRST LINK.** LUKS →
autologin → Hyprland → Big Picture is already seamless: autologin is plain
`[Autologin]` + `pam_permit` with no credential handoff to break, and
environment classification has good primitives (DMI + toggle +
connector/size). Add `Relogin=true` to harden against logout. **The unsolved
link is the LUKS prompt itself:** lizard mode gives arrows/Enter/Esc in the
initramfs — enough to drive a menu, not to type a passphrase. Realistic
options: a second LUKS keyslot with a lizard-expressible passphrase; a
controller-aware initramfs prompt; or accept a keyboard for cold boot and
make **suspend/resume the everyday path** — which, given (c), is the
pragmatic answer.
