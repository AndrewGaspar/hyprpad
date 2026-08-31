# Research: PAM controller-auth mechanics + USB controller wake

*Produced 2026-08-31 by a research agent (sub-thread of the session-lifecycle
investigation). VERIFIED/INFERRED markings as delivered.*

## TOPIC A — pam_exec semantics, rate-limiting, prior art

**1. pam_exec semantics — VERIFIED against upstream source
([pam_exec.c, linux-pam master](https://raw.githubusercontent.com/linux-pam/linux-pam/master/modules/pam_exec/pam_exec.c))
and the [man page](https://man7.org/linux/man-pages/man8/pam_exec.8.html):**

- `expose_authtok` **works in the auth phase** — restricted to `auth` and
  `password` types (lines 156-162: any other type logs
  `"expose_authtok not supported for type %s"` and disables it).
- **It prompts by itself.** It calls `pam_get_item(pamh, PAM_AUTHTOK, ...)`
  (line 168), and if the token is NULL it calls
  `pam_prompt(pamh, PAM_PROMPT_ECHO_OFF, &resp, "Password: ")` (line 181), then
  `pam_set_item(PAM_AUTHTOK, resp)` so later modules can reuse it. The man-page
  folklore that it needs a prior module is wrong; **pam_exec has no
  `use_first_pass` option at all** (full option list: `debug, stdout, log=,
  type=, seteuid, quiet, quiet_log, expose_authtok`).
- **Exit-status mapping gotcha (VERIFIED, lines ~290-321):** exit 0 →
  `PAM_SUCCESS`; **any non-zero exit → `PAM_SYSTEM_ERR`, not `PAM_AUTH_ERR`**.
  Also on failure it calls `pam_error()` printing `"<cmd> failed: exit code %d"`
  to the user unless `quiet` is set.
- Token delivery: raw bytes on child stdin, **no trailing newline**, truncated
  to `PAM_MAX_RESP_SIZE` (line 101, write at line 258). Child gets PAM env +
  `PAM_USER/SERVICE/TTY/RUSER/RHOST/TYPE`; man page warns the user can control
  the environment.

**2. Rate-limiting interplay — PARTIALLY VERIFIED / flagged:** because pam_exec
fails with `PAM_SYSTEM_ERR`, whether `pam_faillock authfail` increments depends
on the stack's control-flag routing, not on the module. In the
omarchy-lock-password layout, `[default=die] pam_faillock.so authfail` catches
*any* non-success falling through, so it should count — but this is INFERRED
from control-flag semantics; test empirically (`pamtester` +
`faillock --user $USER`). Related VERIFIED detail: hyprlock's PAM code
special-cases the pam_faillock "left to unlock" info message
([Pam.cpp line 63](https://raw.githubusercontent.com/hyprwm/hyprlock/main/src/auth/Pam.cpp));
how Quickshell's `PamContext` surfaces `PAM_TEXT_INFO` was not verified, so
faillock lockout messages may be silently dropped by the Omarchy lock UI.

**3. Prior art for gamepad/button-sequence PAM auth: none found.** Searches for
gamepad/joystick PAM modules or pam_exec helpers returned nothing. Greenfield.

## TOPIC B — USB/2.4GHz controller wake

**Field reports (VERIFIED):**
- **8BitDo Ultimate C 2.4G dongle (`2dc8:3106`) — works.** Two independent
  Bazzite writeups:
  [longplaytech](https://longplaytech.com/posts/wake-bazzite-from-sleep-using-a-controller/)
  and [arnaught](https://arnaught.neocities.org/blog/2024/12/28/bazzite-usb-wakeup).
  Notable: arnaught's *per-device* rule
  (`ATTRS{idVendor}=="2dc8", ATTRS{idProduct}=="3106", ATTR{power/wakeup}="enabled"`)
  **did not work**; the whole-bus form did — `ACTION=="add", SUBSYSTEM=="usb",
  KERNEL=="usb3", TEST=="power/wakeup", ATTR{power/wakeup}="enabled"`, or a
  systemd oneshot echoing `enabled` into every `usb*/power/wakeup`.
- **Original Steam Controller (2015) + dongle — works** on Linux even with
  Steam closed
  ([Steam discussion](https://steamcommunity.com/app/4165870/discussions/0/838376331376909100/));
  also confirmed working in arnaught's tests. Wired PDP Xbox One — works.
- **Xbox Wireless Adapter (`045e:02e6`) — wakes the PC but delivers no input**
  until `xone_dongle` is reloaded via a `/etc/systemd/system-sleep/` script
  ([bazzite#3569](https://github.com/ublue-os/bazzite/issues/3569),
  [DeckFilter](https://deckfilter.app/blog/how-i-fixed-xbox-wireless-adapter-in-bazzite-linux/)).
- **Bluetooth controllers — do NOT wake** (arnaught, explicit).
- **Dominant field complaint is the inverse problem:** dongle re-enumeration
  when the controller powers off/times out **instantly re-wakes the box** —
  [ValveSoftware/SteamOS#2641](https://github.com/ValveSoftware/SteamOS/issues/2641)
  ("All Controllers instantly wake steamOS from sleep if they use a USB
  Dongle"; EasySMX D05, Xbox adapter, official Steam Controllers; **open,
  unassigned, no dev comment**). 8BitDo dongle re-enumerates as a distinct
  "8BitDo IDLE" device. Both bloggers hit it; one abandoned manual suspend for
  idle-timeout suspend.

**s2idle vs deep (VERIFIED from
[kernel sleep-states doc](https://www.kernel.org/doc/html/latest/admin-guide/pm/sleep-states.html)):**
s2idle is woken "by in-band interrupts" — theoretically *any*
interrupt-capable device; for deep/S3 "the set of devices that can wake up the
system... usually is reduced" and needs platform setup. So s2idle should be
*more* permissive on paper — but in practice a Steam Controller user in contact
with Valve support reports the new controller "has trouble waking PCs from
**modern standby (S0)**," possibly fixable in firmware
([same Steam thread](https://steamcommunity.com/app/4165870/discussions/0/838376331376909100/)).
Practical advice: if the desktop's `/sys/power/mem_sleep` offers `deep`, A/B
test both; field reports favor S3 for dongle wake.

**BIOS/ErP gating (VERIFIED from the Bazzite guides):** two gates — "Wake from
USB"/"USB Wake Support" enabled, and **ErP Mode disabled** (it cuts +5V standby
to USB ports; with ErP on, no USB wake is possible regardless of sysfs). Vendor
naming (INFERRED): ASUS "ErP Ready", Gigabyte "ErP", MSI "ErP Ready"/"Resume by
USB Device", ASRock "Deep Sleep" (disable). Controllers cannot power on from
full S5 shutdown (user reports, same thread).

**Valve rule / 2026 controller:** (a) the `==`/`=` pairing in
`60-steam-input.rules` lines 16-17 is **correct guard-then-assign**, not a bug —
`ATTR{power/wakeup}=="*"` matches "attribute exists with any value," then
`ATTR{power/wakeup}="enabled"` assigns; equivalent to the `TEST=="power/wakeup"`
idiom. VERIFIED from
[60-steam-input.rules](https://raw.githubusercontent.com/ValveSoftware/steam-devices/master/60-steam-input.rules).
(b) The comment naming "Steam Controller 2026 receiver" and "Steam Machine
Bluetooth" is the strongest technical evidence found that the 2026 controller
wakes hosts via **ordinary USB remote wakeup on a vendor-28de receiver**, and
that the Steam Machine's own BT radio is a wake-enabled 28de USB device (that
last part INFERRED from the comment wording). No deeper official Valve
technical statement found; SteamOS-side friction persists
([GamingOnLinux July 2026](https://www.gamingonlinux.com/2026/07/i-love-my-steam-machine-but-theres-lots-of-work-valve-need-to-do/),
[Steam Machine wakes right after sleep thread](https://steamcommunity.com/discussions/forum/1/573795560006374150/)).

**Hub-chain question — honest answer: the strict "whole chain must be enabled"
claim is NOT documented.** The
[kernel USB PM doc](https://www.kernel.org/doc/html/latest/driver-api/usb/power-management.html)
never states it. The
[ArchWiki wakeup-triggers page](https://wiki.archlinux.org/title/Power_management/Wakeup_triggers)
says: "An endpoint device should be able to wake the device if the trigger is
enabled **regardless of the controller's setting, however this might be
hardware-dependent**" (VERIFIED). The
[ArchWiki Udev page](https://wiki.archlinux.org/title/Udev) notes host
controllers are ACPI-wake-enabled by default (`/proc/acpi/wakeup`), yet its own
example rule sets **both** the device and its controller. Field evidence
(per-device rule failing where whole-bus succeeded for 8BitDo) suggests
enabling the root-hub/bus level is what matters in practice on some hardware.
Pragmatic rule: enable leaf + root hub; hardware-dependent (INFERRED).

Two operational VERIFIED details: changing `power/wakeup` while suspended only
takes effect at the *next* suspend cycle (kernel doc), and hardware can reset
the attribute across transitions — use `ACTION=="add|change"` (ArchWiki; also
[systemd#6364](https://github.com/systemd/systemd/issues/6364): no generic
`wakeup.target`).
