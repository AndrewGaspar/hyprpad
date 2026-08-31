# Research: controller-driven LUKS unlock at boot (consolidated agent report)

*Produced 2026-08-31. Sections 4 and 5 are marked **best-effort**: two dedicated
sub-threads (distro-precedent deep-dive, NVIDIA risk survey) did not return, so
those sections rest on the agent's own first-hand research — local
binary/config verification plus cited web sources.*

**Machine facts baseline (all VERIFIED locally unless noted).** Framework
Laptop 16 — *not* the living-room tower: NVIDIA GB206M RTX 5070 Max-Q
`[10de:2d58]` (`card1`, driver `nvidia`, `nvidia-open-beta-dkms 610.57.04`) +
AMD Radeon 880M/890M iGPU `[1002:150e]` (`card2`, `amdgpu`). Kernel
7.1.9-arch1-2, mkinitcpio 41.1, plymouth 26.134.222-2, systemd 261.2,
cryptsetup 2.8.7, limine 12.6.0. Root: `nvme0n1p2` = `crypto_LUKS` →
`/dev/mapper/root` (btrfs), unlocked via `cryptdevice=PARTUUID=…:root` on the
cmdline embedded in the UKI at `/boot/EFI/Linux/omarchy_linux.efi` (cmdline
assembled by `limine-mkinitcpio-hook` from `/etc/limine-entry-tool.d/*.conf`;
`/etc/kernel/cmdline` does not exist; `/etc/mkinitcpio.d/` is empty).

**Premise corrections:**
1. **Plymouth IS in HOOKS and runs today.** `/etc/mkinitcpio.conf.d/omarchy_hooks.conf`
   overrides the base file: `HOOKS=(base udev plymouth keyboard autodetect
   microcode modconf kms keymap consolefont block encrypt filesystems fsck
   btrfs-overlayfs)`, `FILES+=(/etc/vconsole.conf)`. `lsinitcpio` on the UKI
   confirms `hooks/plymouth`, `plymouthd`, `script.so`,
   `renderers/{drm,frame-buffer}.so`, and the full `omarchy` script theme.
   Journal from the current boot shows "Forward Password Requests to Plymouth
   Directory Watch" active.
2. **The busybox `encrypt` hook already delegates its prompt to Plymouth.**
   `/usr/lib/initcpio/hooks/encrypt`:
   `plymouth ask-for-password --prompt=… --command="cryptsetup open --type luks
   --key-file=- ${resolved} ${cryptname}…"` when `plymouth --ping` succeeds;
   plain text loop otherwise.
   ([upstream hook](https://github.com/archlinux/mkinitcpio/blob/master/hooks/encrypt))
3. An unmerged `/etc/mkinitcpio.conf.d/omarchy_hooks.conf.pacnew` (2026-08-30)
   adds NVIDIA `kms`-drop logic (keeps `kms` on this hybrid machine) and gates
   the `vconsole.conf` bundling on Latin layouts (`us` passes).

## Q1 — USB HID at the initramfs passphrase prompt

**VERIFIED (local, `/proc/config.gz` + `modules.builtin` on 7.1.9-arch1-2):
every module needed for a USB HID keyboard is compiled into the Arch kernel.**

```
CONFIG_HID=y  CONFIG_HID_GENERIC=y  CONFIG_USB_HID=y  CONFIG_HIDRAW=y
CONFIG_USB_XHCI_HCD=y  CONFIG_USB_XHCI_PCI=y  CONFIG_INPUT_EVDEV=y
CONFIG_HID_STEAM=m  CONFIG_UHID=m  CONFIG_INPUT_UINPUT=m
```
`modules.builtin` lists `hid.ko`, `hid-generic.ko`, `usbhid/usbhid.ko`,
`usbcore.ko`, `ehci/ohci/uhci/xhci-hcd.ko`, `xhci-pci.ko`, `evdev.ko`,
`input-core.ko`, `atkbd.ko`, `i8042.ko`.

- **Any standard USB HID boot keyboard (the K400 included) works at the LUKS
  prompt with zero initramfs modules.** The `keyboard` hook
  (`/usr/lib/initcpio/install/keyboard`: `add_module usbhid;
  add_checked_modules '/hid/hid' '/input/(serio|keyboard)'` plus USB host
  modules) is belt-and-braces on this kernel. The widely-reported "keyboard
  dead at LUKS prompt" failures
  ([omarchy#2345](https://github.com/basecamp/omarchy/issues/2345),
  [Arch BBS 243254](https://bbs.archlinux.org/viewtopic.php?id=243254),
  [Manjaro forum](https://forum.manjaro.org/t/no-keyboard-input-at-luks-passphrase-prompt/29592))
  afflict kernels/initramfs where these are modular and missing.
- Because `keyboard` precedes `autodetect` in the active HOOKS, the image
  carries **all 136 `drivers/hid/*` modules** including `hid-steam.ko`.
- This applies identically to busybox (`encrypt`) and systemd (`sd-encrypt`)
  initramfs: HID/input is a kernel-layer question, independent of the init
  flavor. `sd-encrypt`'s install hook even adds `hid-generic?` explicitly
  (redundant here — builtin).

**Steam Controller 2026 puck (`28de:1304`) as a boot keyboard:**
- VERIFIED: `hid-steam` on kernel 7.1.9 only matches `28de:1102/1142/1205`
  (`modinfo hid-steam` aliases) — it does **not** bind the puck. Upstream added
  `USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS 0x1304` (plus IBEX `0x1302`,
  IBEX_BLE `0x1303`, NEREID `0x1305`) in
  [hid-ids.h](https://raw.githubusercontent.com/torvalds/linux/master/drivers/hid/hid-ids.h),
  merged for **Linux 7.3**
  ([Phoronix](https://www.phoronix.com/news/Linux-7.3-HID),
  [Hardware Busters](https://hwbusters.com/news/linux-7-3-gives-the-2026-steam-controller-a-real-kernel-driver-steam-client-not-required/)).
- Therefore on this kernel the builtin `hid-generic` claims the puck's HID
  interfaces, and **lizard mode stays permanently on** — the firmware
  keyboard/mouse emulation works as a plain HID keyboard in initramfs.
  INFERRED (device not attached during research; it is a standard HID boot
  keyboard on if02, consistent with
  [hid-steam.c](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-steam.c):
  lizard mode is *"implemented as additional HID interfaces"*). Re-validate at
  kernel ≥7.3, where `hid-steam` binds and *"disables [lizard mode] when the
  input device opens"* (driver comment; `lizard_mode` module param exists,
  default true).
- **Key vocabulary constraint:** lizard mode emits **arrows (dpad), Enter (A),
  Escape (B), mouse (right pad + shoulders) — essentially no letters** (driver
  comment: *"A and B buttons are ENTER and ESCAPE"*; 2026-model shortcut list:
  [Lemmy](https://lemmy.world/post/47386512), incl.
  `Steam+Dpad-right = Enter`). **A passphrase cannot be typed on it; any scheme
  must map arrow/Enter sequences to secret material.** The puck presents ~5
  hidraw interfaces; Steam holds all of them at runtime
  ([steam-for-linux#13185](https://github.com/ValveSoftware/steam-for-linux/issues/13185))
  — irrelevant in initramfs where Steam isn't running.

## Q2 — Plymouth theme scripting (source-verified)

Verified two ways: strings/symbols of the installed
`script.so`/`libply-splash-core.so.5`, and a clone of
[gitlab.freedesktop.org/plymouth/plymouth](https://gitlab.freedesktop.org/plymouth/plymouth)
@ `bbcb5ec` (2026-08-30, matches 26.134.222). Note: no GitHub mirror exists;
the freedesktop wiki blocks non-browser fetchers (HTTP 418). Best fetchable
doc: [Gentoo theming wiki](https://wiki.gentoo.org/wiki/User:DerpDays/Plymouth/Theming).

**API surface (VERIFIED, exhaustive — 18 `Plymouth.*` natives):**
`SetRefreshFunction` (default **50 FPS**, `#define FRAMES_PER_SECOND 50`,
overridable via `SetRefreshRate`), `SetBootProgressFunction(duration,
progress)`, `SetKeyboardInputFunction(str)`, `SetDisplayPasswordFunction(prompt,
bullets)` (bullets = UTF-8 char count typed so far, recomputed per keystroke),
`SetDisplayQuestionFunction(prompt, entry_text)` (question path receives
**plaintext**, echoing is theme's choice), `SetDisplayPromptFunction(prompt,
entry_text, is_secret)`, `SetDisplayNormalFunction`,
`SetDisplayMessageFunction`/`SetHideMessageFunction`,
`SetDisplayHotplugFunction`, `SetUpdateStatusFunction`,
`SetValidateInputFunction`, `SetQuitFunction`, `SetRootMountedFunction`,
`SetSystemUpdateFunction`, `GetCapslockState()`, `GetMode()`. Plus `Window.*`,
`Image(file)`/`Image.Text(text,r,g,b,alpha,font,align)`/`.Scale/.Crop/.Rotate/.Tile`,
`Sprite` with z-ordered `SetPosition/SetOpacity`, `Math.*` incl.
**`Math.Random()`**, minimal `String` (`CharAt/SubString/Length`).

- **Arbitrary UI drawing: YES** — a QWERTY grid, randomized layout, highlight
  cursor are all expressible (the local `omarchy.script` already does
  sprite/text composition).
- **Input, the decisive part.** Plymouth has two keyboard providers
  (`ply-keyboard.c`, `ply-device-manager.c:1144-1166`):
  - *Renderer/evdev provider* — used whenever a DRM/framebuffer renderer
    exists, i.e. **every graphical splash**. `apply_key_to_input_buffer()`
    (`ply-input-device.c`) maps Escape→`\033`, Return→`\n`,
    BackSpace→`\177`, printables via `xkb_state_key_get_utf8()`; **arrow keys
    produce zero bytes** (no CSI synthesis; only a VT-switch keysym check).
    Requires a configured XKB layout (hence Omarchy bundling
    `/etc/vconsole.conf`; log: *"Not creating devices for subsystem input
    because there is no configure XKB layout"*). Input is `EV_KEY` only — **no
    EV_ABS/EV_REL/touch/mouse anywhere in Plymouth**.
  - *Terminal provider* — only created when there is **no renderer** (text
    fallback/serial). There, arrows arrive as whole CSI strings (`"\x1b[A"`)
    and *do* reach `SetKeyboardInputFunction` — but the `script` plugin needs
    pixel displays, which only exist with a renderer, so **this path can never
    coexist with a script theme**.
  - On either path, Enter/Escape/Backspace are consumed by dedicated internal
    handler lists (`ply_keyboard_add_{enter,escape,backspace}_handler`) and
    **never reach the theme callback**; escape sequences are explicitly
    discarded from the password buffer (`plymouthd-interaction.c`:
    `/* Ignore escape sequences */`).
- **No injection API.** The password submitted is the daemon's `entry_buffer`,
  built only from real keystrokes and piped to `--command`'s stdin. No
  `SetPassword`/`Answer`/`Submit` binding exists. The single input-adjacent
  hook, `SetValidateInputFunction(entry_text, add_text)→bool`, is a
  **veto-only** per-character/submission filter (and its verdict is discarded
  unless `ply_console_viewer_preferred()`). No file/exec/network primitives in
  the script language (only `Image()` loads from the theme's `ImageDir`).
- **Escape is hostile to a controller UX:** B = Esc toggles splash↔details
  (`plymouth(8)`).
- **Client-side primitives that do exist** (VERIFIED via `plymouth --help`):
  `ask-for-password --command --prompt --number-of-tries
  --dont-pause-progress`; `ask-question`; `display-message --text`;
  `watch-keystroke --keys --command` (one keystroke from the watched set →
  command stdin; used in production by
  [debian-live/live-boot](https://github.com/debian-live/live-boot/blob/master/components/9990-initramfs-tools.sh);
  [example](https://www.mankier.com/1/plymouth)). `--keys` matches characters,
  so arrows can't be watched either.
- **Prior art: none.** GitHub code search for
  `SetKeyboardInputFunction extension:script` → zero real users (only
  boilerplate comments, e.g.
  [heads-plymouth-theme](https://raw.githubusercontent.com/zaolin/heads-plymouth-theme/master/heads.script)).
  Upstream feature request
  [work item #144 "Virtual Keyboard for any text input"](https://gitlab.freedesktop.org/plymouth/plymouth/-/work_items/144)
  open since 2021-03-26, zero comments. postmarketOS,
  [2026-04-30](https://postmarketos.org/edge/2026/04/30/Switching-from-pbsplash-to-Plymouth/),
  after adopting Plymouth: *"unl0kr is still used for the unlock prompt, since
  Plymouth does not have an on-screen keyboard."* Also
  [RH BZ#1411026](https://bugzilla.redhat.com/show_bug.cgi?id=1411026),
  [linux-surface#1001](https://github.com/linux-surface/linux-surface/issues/1001),
  Bazzite's non-functional OSK icon at the FDE prompt
  ([Universal Blue forum](https://universal-blue.discourse.group/t/on-screen-kb-possible-for-full-disk-encryption/5489)).

## Q3 — `encrypt` vs `sd-encrypt` for a custom askpass; agent protocol; hidraw

**Busybox `encrypt` (current setup) — more amenable. Two unpatched injection
points (both VERIFIED in the hook source):**
1. **`/crypto_keyfile.bin`** — if present, tried via `cryptsetup --key-file`
   *before* any prompt; passphrase fallback on failure; `rm -f`'d afterward. A
   custom hook ordered before `encrypt` writes the derived secret there. Prior
   art: [fb-ask-pass-rs](https://github.com/gdamjan/fb-ask-pass-rs) (hook
   order `base udev autodetect block fb-ask-pass encrypt filesystems`).
   **Byte-exactness:** cryptsetup(8): keyfile reads are *"up to the compiled-in
   maximum size. Newline characters do not terminate the input"* — write **no
   trailing newline** or it won't match a keyslot added interactively.
2. **uinput injection** — a background daemon (started from
   `run_hook() { prog & }`) reads the controller and types synthetic
   keystrokes into whatever prompt is active (Plymouth's included). Prior art:
   [pmkap/deckrypt](https://github.com/pmkap/deckrypt) (Arch-native; `install`
   hook: `add_module uinput; add_binary deckrypt_input; add_runscript`).
   Local: `CONFIG_INPUT_UINPUT=m`, **not** currently in the initramfs → needs
   `add_module uinput`.

**`sd-encrypt` / systemd path — documented and implementable, but a net
negative here.**
- **Agent protocol (VERIFIED,
  [systemd PASSWORD_AGENTS spec](https://systemd.io/PASSWORD_AGENTS/)):**
  inotify-watch `/run/systemd/ask-password/` for `IN_CLOSE_WRITE|IN_MOVED_TO`;
  parse `ask.XXXX` ini (`[Ask]` → `Socket=`, `Message=`, `PID=`, `Echo=`,
  `AcceptCached=`, `NotAfter=` CLOCK_MONOTONIC µs); reply with **one
  AF_UNIX/SOCK_DGRAM datagram** to `Socket=`: password prefixed `+` (or `-` =
  cancel). Reference binaries exist locally:
  `/usr/bin/systemd-tty-ask-password-agent` (`--watch --plymouth` mode) and
  `/usr/lib/systemd/systemd-reply-password`. `sd-encrypt`'s install hook
  bundles `systemd-ask-password-console.{path,service}`; the plymouth hook
  bundles `systemd-ask-password-plymouth.{path,service}` when the `systemd`
  hook is present. A custom initramfs agent reading `/dev/input/event*` or
  `/dev/hidraw*` and replying `+<secret>` is fully within spec (root in
  initramfs satisfies the privileged-socket requirement).
- **hidraw in initramfs: YES.** `CONFIG_HIDRAW=y` (builtin — no `hidraw.ko`
  exists in the module tree), udev runs in both initramfs flavors,
  `50-udev-default.rules` is bundled; `/dev/hidraw*` nodes appear via
  devtmpfs+udev. (VERIFIED builtin; node creation in initramfs INFERRED from
  the running system + bundled rules.)
- **Switch costs (VERIFIED per
  [Arch Wiki dm-crypt/System configuration](https://wiki.archlinux.org/title/Dm-crypt/System_configuration)
  + local hook sources):** `udev→systemd`, `keymap consolefont→sd-vconsole`,
  `cryptdevice=`→`rd.luks.name=`/`rd.luks.uuid=` or `/etc/crypttab.initramfs`
  (wiki warns mixing crypttab with `rd.luks.*` silently deactivates unlisted
  devices). Two Plymouth-specific regressions land on this exact config:
  (i) Arch Wiki Plymouth: scripted themes' *"password prompt may not update"*
  under the systemd hook — `omarchy.plymouth` is `ModuleName=script`;
  (ii) `strings /usr/bin/plymouthd` contains `rd.luks.uuid=` (and **no**
  `cryptdevice`) plus `UseSimpledrm`/`"Ignoring UseSimpledrmNoLuks because of
  LUKS use"` — i.e. Plymouth detects LUKS *only* via `rd.luks.uuid=` and then
  suppresses its simpledrm fast-path, waiting up to `DeviceTimeout=8` for a
  real DRM driver (breakage report naming 26.134.222:
  [archinstall#4585](https://github.com/archlinux/archinstall/issues/4585)).
- **Initramfs tooling note** for any hand-rolled TUI on the busybox side: **no
  ncurses/terminfo, and busybox lacks `stty`** (has `ash awk sed dd cat printf
  xxd base64 sha256sum hexdump kbd_mode openvt setfont loadkmap`); raw-mode
  input needs a small static binary.

## Q4 — Distro precedent *(best-effort: dedicated sub-thread did not return; findings below are first-hand, cited)*

| Distro | FDE at boot | Controller unlock |
|---|---|---|
| **SteamOS 3.x (Valve)** | **No LUKS.** Experimental `dirlock` (fscrypt-based, Rust) in 3.8 — unlocks `/home` **at the SDDM login screen, after boot**, precisely to dodge the no-keyboard-at-initramfs problem | N/A by design |
| **Bazzite** | LUKS optional; TPM auto-unlock via `ujust setup-luks-tpm-unlock`; Plymouth prompt otherwise | **No** — docs: *"You will need a physical USB keyboard to decrypt the drive!"* |
| **ChimeraOS / HoloISO** | No evidence of FDE support found (negative result) | — |

Sources: Igalia's dirlock slides list LUKS cons *"Usually unlocked early on
boot"*, requirement *"Not all computers have a keyboard!"*
([PDF](https://www.igalia.com/downloads/slides/albertogarcia-DirlockANewRustTooltoManageEncryptedFilesystems.pdf);
overview [LWN](https://lwn.net/Articles/1038859/)); SteamOS FDE feature request
open, no Valve response
([SteamOS#771](https://github.com/ValveSoftware/SteamOS/issues/771)); Bazzite
quote
([install-guide](https://github.com/ublue-os/docs.bazzite.gg/blob/main/src/General/Installation_Guide/install-guide.md));
unl0kr proposal open since 2023
([bazzite#464](https://github.com/ublue-os/bazzite/issues/464));
Deck-at-Plymouth-prompt hangs
([#609](https://github.com/ublue-os/bazzite/issues/609) closed not-planned,
[#4009](https://github.com/ublue-os/bazzite/issues/4009)).

**Working third-party prior art (both examined at source level):**
- **[xstasi/steamdeck-luks-unlock](https://github.com/xstasi/steamdeck-luks-unlock)**
  — ncurses 6x6 `A–Z0–9` grid on `/dev/console`, driven by **lizard-mode**
  arrows/Enter (`getch()`+`keypad()`); selection → 36-bit bitmap string →
  Debian `keyscript=` → LUKS. Flaws: **36-bit keyspace** (author: *"not
  perfect security wise"*), each letter usable once, and **selected letters
  stay highlighted on screen** — not shoulder-surfing resistant. Debian-only
  integration; port needed.
- **[pmkap/deckrypt](https://github.com/pmkap/deckrypt)** — Arch mkinitcpio
  hooks in-repo; libevdev *gamepad* combos → uinput synthetic keystrokes →
  stock prompt; documented **≈12.5 bits/combo** (5,616 combinations), 4–5
  combos ≈ decent. Caveats: hardcoded button map; gamepad mode requires
  `hid-steam` binding (absent for the puck until kernel 7.3 — on 7.1.9 read
  lizard-mode evdev instead).
- **[unl0kr / BuffyBox](https://gitlab.com/postmarketOS/buffybox)**
  ([wiki](https://wiki.postmarketos.org/index.php?title=Unl0kr),
  [AUR](https://aur.archlinux.org/packages/unl0kr)) — the mature initramfs OSK
  (LVGL on DRM/fbdev, keyscript-style stdout). **Touch-only**; a Steam
  Deck/CachyOS recipe requires **removing Plymouth** (DRM contention)
  ([CachyOS guide](https://discuss.cachyos.org/t/guide-luks-touch-screen-on-steam-deck-with-cachyos-handheld-edition-using-unl0kr/20293)).
- No IR/TV-remote initramfs unlocker found anywhere (negative result);
  headless practice is SSH/dropbear.
- Randomized-layout defense is evidence-backed: entry time ~1.4s→~2s,
  negligible accuracy cost, materially higher observation resistance
  ([PIN Scrambler, ACM](https://dl.acm.org/doi/fullHtml/10.1145/3568444.3568450);
  [RoundPIN](https://iieta.org/journals/ijsse/paper/10.18280/ijsse.110610)).

## Q5 — Risk assessment: Plymouth + NVIDIA + (optionally) sd-encrypt *(best-effort)*

Already-mitigated locally — do not regress:
- **Early NVIDIA KMS:** `MODULES+=(nvidia nvidia_modeset nvidia_uvm
  nvidia_drm)` (`nvidia.conf` drop-in) + `options nvidia_drm modeset=1`, and
  **`etc/modprobe.d/nvidia.conf` is bundled inside the UKI** so the option
  applies at initramfs modprobe time. VERIFIED via `lsinitcpio`.
- **`initramfs_async=0`** (Omarchy drop-in, VERIFIED comment): kernel 7.1's
  async initramfs unpack races `/init`; without it *"plymouthd exits…
  encrypted boots fall back to an unthemed text LUKS prompt."*
- **`FILES+=(/etc/vconsole.conf)`**: without an XKB layout Plymouth creates no
  evdev input devices at all (Q2). Layout is `us`.

Live risks:
1. **No way to pin Plymouth to a display.** Full `plymouth.*` option set in
   this binary: `boot-log debug force-frame-buffer-on-boot force-scale
   force-splash graphical ignore-serial-consoles ignore-show-splash
   ignore-udev nolog splash splash-delay use-simpledrm` — **no
   `plymouth.device=`**. This boot registered three DRM devices (simpledrm
   minor 0 → replaced; `nvidia-drm` minor 1; `amdgpu` primary fbcon). On a
   multi-output setup the prompt can land on an off/secondary display; only
   levers are `plymouth.use-simpledrm`, `video=<out>:d`, and early-KMS
   ordering. (Arch Wiki simpledrm caveat: secondary monitors stay off, *"the
   password prompt … may not be visible"*.)
2. **sd-encrypt migration double-whammy** (Q3): scripted-theme prompt-update
   bug + `rd.luks.uuid=`-triggered simpledrm suppression with
   `DeviceTimeout=8` stall if the real DRM driver is late — in exchange for
   TPM2/FIDO2 features this design doesn't use. Staying on `encrypt`
   sidesteps both.
3. **Beta DKMS fragility:** `nvidia-open-beta-dkms` + bleeding kernel; the
   known Omarchy failure chain (DKMS build fails → mkinitcpio error → limine
   skips installing the UKI → unbootable) is
   [omarchy#5706](https://github.com/basecamp/omarchy/issues/5706). Validate
   any initramfs change against the Limine snapshot/fallback entries (present,
   VERIFIED in `limine.conf`).
4. **Latent HOOKS-order defect:** `plymouth` before `keyboard`/`keymap` breaks
   non-US layouts at the prompt
   ([omarchy#6072](https://github.com/basecamp/omarchy/issues/6072)); dormant
   here (`us`).
5. **Secure Boot inversion:** with SB signing, a UKI *"ignores all command
   line options"* — Limine's `cmdline:` line would silently stop working
   ([Arch Wiki UKI](https://wiki.archlinux.org/title/Unified_kernel_image)).
   SB is currently off.
6. **Recovery hatch exists:** `plymouth.enable=0 disablehooks=plymouth`
   editable at the Limine menu; the `encrypt` hook degrades to a text prompt
   whenever `plymouth --ping` fails.

## Feasibility verdicts

**(a) Controller-button security code → custom askpass — FEASIBLE; lowest
risk; working Arch prior art.** Add a hook immediately before `encrypt` that
reads the puck's lizard-mode events from `/dev/input/event*` (or raw
`/dev/hidraw*` — both available), derives a secret from the button sequence,
and delivers it via `/crypto_keyfile.bin` (newline-free) or uinput typing
(deckrypt's architecture; add `uinput` to the image). Zero patches to Plymouth
or the stock hook; keyboard fallback keyslot retained by construction. Design
constraints: ~6-symbol lizard vocabulary needs a long sequence for real
entropy (deckrypt: ~12.5 bits/combo in gamepad mode; plain arrow-taps are
~2.3 bits/tap — budget accordingly, keep it as a *second* keyslot);
re-validate device behavior on kernel ≥7.3 (`hid-steam` takeover); the puck
was not physically enumerated during this research (marked INFERRED above).

**(b) Plymouth on-screen-keyboard theme — NOT FEASIBLE as a theme; feasible
as a standalone pre-`encrypt` program.** Airtight, source-verified blockers:
at any graphical splash the keyboard is evdev-backed and **arrows generate
zero bytes for the theme**, Enter/Esc/Backspace never reach the script
callback, and **no API can inject characters into the password buffer** (the
only hook, `SetValidateInputFunction`, is veto-only). The lizard-mode key
vocabulary is exactly the set a theme cannot act on; upstream request open and
untouched since 2021; zero themes in the wild do it; postmarketOS ships a
separate tool for precisely this reason. The viable variant is a standalone
console/DRM program before `encrypt` (xstasi's grid ported off Debian
keyscript, or unl0kr-class rendering) — must ship its own TUI stack (no
ncurses/`stty` in the image), coordinate with or temporarily displace
Plymouth, and should improve on the prior art with a randomized layout +
hidden buffer (xstasi's is 36-bit and highlights selections on screen, failing
both of the user's goals).

**(c) Type on the K400 at boot — WORKS TODAY, zero changes.** All
HID/USB/input drivers are kernel-builtin, the `keyboard` hook over-provisions
on top, layout `us` avoids the hook-order bug, and the prompt degrades
gracefully from Plymouth to plain text. Keep a strong keyboard passphrase as
the primary keyslot regardless of (a)/(b).

*Key local files:* `/etc/mkinitcpio.conf.d/omarchy_hooks.conf{,.pacnew}`,
`/etc/mkinitcpio.conf.d/nvidia.conf`, `/usr/lib/initcpio/hooks/encrypt`,
`/usr/lib/initcpio/install/{plymouth,keyboard,sd-encrypt,base}`,
`/usr/share/plymouth/themes/omarchy/omarchy.script`,
`/etc/limine-entry-tool.d/omarchy-defaults.conf`,
`/etc/plymouth/plymouthd.conf`, `/etc/vconsole.conf`,
`/boot/EFI/Linux/omarchy_linux.efi`, `/boot/limine.conf`.
