# 10 — Distribution and the install path

Target: as close to **plug-and-play** as the masking requirement allows. Today's
W12 result ([experiments/w12-device-denial.md](experiments/w12-device-denial.md))
makes this much easier than the programme assumed — **nothing needs root.** The
bwrap mask is unprivileged and hyprpad reads the controller through the ordinary
session `uaccess` ACL, so the entire install is user-level: no system daemon, no
`/etc` changes, no sudo installer.

## The pieces

Everything below is a per-user file; none require privilege.

| Piece | What | Where |
|---|---|---|
| `hyprpad` | daemon + CLI (one static-ish binary) | packaged to `/usr/bin` (pacman) or `~/.cargo/bin` (`cargo install`) |
| `hyprpad.service` | the daemon, `WantedBy=graphical-session.target` | `~/.config/systemd/user/` |
| `config.toml` | gesture bindings, defaults on first run | `~/.config/hyprpad/` |
| Steam masking hook | makes every Steam launch masked | `~/.local/bin/` + `~/.local/share/applications/` |
| Hyprland snippet (optional) | the W1 window rules / PiP / focus knobs | `~/.config/hypr/hyprpad.lua` |

## The daemon's responsibilities (recap)

Read the real controller; **own lizard mode** (open the hidraw and send the
disable-lizard feature report, since Steam no longer can — see W12 nuance 2);
drive Hyprland (gestures → IPC, trackpad → virtual pointer); **emit a virtual
controller** (uinput/uhid) that Steam *does* see; and **focus-gate** — route the
real controller into the virtual pad only when a Steam window / game holds focus,
so Steam Input keeps working in games while the desktop layer owns it otherwise.

## The one real design choice: how Steam launches masked

The mask must wrap Steam's `exec`. On this machine Steam starts three ways, all
interceptable:

- **Hyprland autostart** — `steam -silent` (`autostart.lua`)
- **The `.desktop`** — menu launches and `steam://` URL handling → `/usr/bin/steam %U`
- **CLI**

`hyprpad setup` installs a transparent interception that covers all three:

1. **`~/.local/bin/hyprpad-steam`** — the wrapper: ensure the daemon is up, then
   `bwrap`-mask the controller nodes and `exec /usr/bin/steam "$@"`. **Fail-open:** if
   hyprpad isn't installed/running or bwrap is missing, it execs plain Steam —
   a broken hyprpad never leaves the user unable to launch Steam.
2. **`~/.local/share/applications/steam.desktop`** — a user override (shadows the
   system file) with `Exec=hyprpad-steam …`, catching menu + `steam://` launches.
3. **The autostart line** rewritten to `hyprpad-steam -silent` (catches autostart;
   the wrapper on `PATH` also catches bare `steam` from a shell).

All idempotent and reversible: `hyprpad setup --revert` removes the override, the
wrapper, the unit, and restores the autostart line. Nothing under `/usr` or
`/etc` is touched, so an uninstall is complete and clean.

## The plug-and-play sequence

```
yay -S hyprpad-git          # or: omarchy install hyprpad ; or: cargo install hyprpad
hyprpad setup               # enables the service, hooks Steam, writes default config
#   (re-login, or `systemctl --user start hyprpad`)
```

After that: the controller drives Hyprland; launching Steam *any* way launches it
masked; games still receive Steam Input via hyprpad's focus-gated virtual pad.
The only irreducible friction is the one `hyprpad setup` step — a distro package's
root post-install cannot safely edit per-user config, so first-run setup (a CLI
step, or a self-configuring first-start of the user service) is the realistic
minimum. On Omarchy this can be an extension/first-run hook so even that
disappears.

## Omarchy-native packaging (this user's case)

Omarchy already has the right seams: `omarchy install`, first-run hooks, and
config layering (`~/.config/hypr/*.lua` loaded after defaults). hyprpad as an
**Omarchy extension** would: drop `hyprpad.lua` into the hypr config, add the
autostart + service, install the Steam wrapper, and register a revert — using
Omarchy's own mechanisms rather than hand-editing files. This is the most
plug-and-play route on this system and the reference integration to build first.

## Steam Deck (future)

On the Deck, Steam *is* the session (gamescope-session launches it), so the
interception point is the session launcher, not a `.desktop`/autostart — a
different hook. Deferred with the rest of the Deck port (W13); the daemon and
config are unchanged, only the Steam-masking seam differs.

## Open questions before building the installer

- **Lizard-mode ownership** (W2 follow-on): hyprpad must send the exit-lizard
  feature report itself — verify writing that report to the hidraw while hyprpad
  holds it works and survives the controller's power cycles.
- **Replug handling** (W12 nuance 3): masked Steam won't see nodes created after
  its launch; decide between masking the parent USB `/sys` path, a stable
  by-id bind, or a udev-hotplug hook that re-masks / relaunches.
- **uinput vs uhid virtual pad**: uinput generic pad is simplest and enough for
  Steam Input on modern games; a `uhid` Steam-Deck/`neptune` clone preserves
  gyro/trackpads as Steam Input inputs but is far more work and rides Steam's
  unstable handling of this controller. Start with uinput.
