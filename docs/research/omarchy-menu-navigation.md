# Omarchy shell navigation from the controller — "B means back"

Research for: make **B** mean *dismiss / go back* in every Omarchy Quickshell
surface, starting with the main menu (`omarchy-menu`), in the most general and
least-hacky way hyprpad's mode system allows.

Everything below is marked **VERIFIED** (read from source at a cited line, or
observed live on this machine on 2026-09-01) or **INFERRED**. Live probing was
done against the running session: `hyprctl layers`, the Hyprland event socket,
`omarchy-shell` IPC, and `wtype` for synthetic keys. Nothing was modified.

Sources of truth:

- Shell (the one actually running): `~/code/omarchy-bluetooth-friendly-name/shell/`
  — `OMARCHY_PATH` is set to that checkout (VERIFIED, `systemctl --user
  show-environment`), so the dev checkout *is* the live shell, not
  `/usr/share/omarchy/shell/`.
- Stock: `/usr/share/omarchy/shell/`. `diff -rq` shows the two differ in ~40
  files, but **not** in any file that decides layer namespaces or key handling:
  `Menu.qml` differs only by the `disabled:` menu-entry feature the owner is
  carrying locally (VERIFIED — the diff hunks are all `disabledResults` /
  `isDisabled`), and every `WlrLayershell.namespace` string is identical between
  the two trees. **The namespace list below is valid for stock Omarchy too.**
- hyprpad `ctx.layers`: currently only on the in-flight worktree
  `/home/ajg/code/hyprsc/.claude/worktrees/agent-a84558a2dc6e05103/`
  (VERIFIED — `master` has no `ctx.layers`). Line cites below are that worktree;
  the files land at `/home/ajg/code/hyprsc/src/`.

---

## 0. Recommendation in one paragraph

Add two modes keyed on `ctx.layers` — `omarchy-menu` (the menu, which has a
navigation *hierarchy*) and `omarchy-ui` (every other transient, keyboard-owning
Omarchy surface, all of which are flat) — from a hand-written **allowlist** of
namespaces, never a `omarchy-*` prefix match. Widen the existing
`only_in("desktop")` guards on the D-pad, A, **the cursor and the scroll** to
include both new modes, and bind `b` per mode. Ship it today with plain keys
(`backspace` in the menu, `escape` elsewhere), which needs no code change
anywhere and is proven to work. Then close the one real gap — B at the menu root
is a dead button — with the *correct* end state: teach hyprpad the one key it is
missing, `KEY_BACK` (a one-line table entry), and teach Omarchy's shell to handle
`Qt.Key_Back` (about six lines, in two files). That is the general answer: the
controller says "back", the shell decides what back means, and hyprpad never has
to know a route name. Do **not** build this on `omarchy-shell` IPC: it needs a
*larger* hyprpad change than `KEY_BACK` does, costs ~40 ms and a `/bin/sh` spawn
per button press, and puts Omarchy route knowledge inside hyprpad.

---

## 1. Layer-shell namespaces

### 1.1 Complete enumeration (from source)

Every `WlrLayershell.namespace` in the live shell tree. `focus` is the
`WlrLayershell.keyboardFocus` on the same surface.

| namespace | source | layer | keyboard focus | lifetime |
|---|---|---|---|---|
| `omarchy-background` | `plugins/background/Background.qml:216` | Background | `None` (`:218`) | **persistent** |
| `omarchy-bar` | `plugins/bar/Bar.qml:1023` | Top (`:1024`) | unset → `None` | **persistent** |
| `omarchy-menu` | `plugins/menu/Menu.qml:1070` | Overlay | **`Exclusive`** (`:1072`) | transient |
| `omarchy-keyboard-panel` | `Ui/KeyboardPanel.qml:85` | Overlay | `Exclusive` prime → `OnDemand` (`:98`) | transient |
| `omarchy-keyboard-panel-dismiss` | `Ui/KeyboardPanel.qml:357` | Overlay | `None` (`:359`) | transient, **multi-monitor only** |
| `omarchy-clipboard` | `plugins/clipboard/Clipboard.qml:319` | Overlay | `Exclusive` (`:321`) | transient |
| `omarchy-emojis` | `plugins/emojis/Emojis.qml:165` | Overlay | `Exclusive` (`:167`) | transient |
| `omarchy-image-selector` | `plugins/image-picker/ImagePicker.qml:368` | Overlay | `Exclusive` while open (`:370`) | transient |
| `omarchy-reminders` | `plugins/reminders/ReminderFlow.qml:100` | Overlay | `Exclusive` (`:102`) | transient |
| `omarchy-polkit` | `plugins/polkit/PolkitAgent.qml:226` | Overlay | `Exclusive` (`:228`) | transient |
| `omarchy-network-qr` | `plugins/panels/wifiqr/Panel.qml:218` | Overlay | `Exclusive` (`:220`) | transient |
| `omarchy-speed-test` | `Ui/SpeedTestOverlay.qml:24,87` (default) | Overlay | `Exclusive` (`:89`) | transient |
| `omarchy-network-speedtest` | `plugins/panels/speedtest/Panel.qml:187` | Overlay | `Exclusive` | transient |
| `omarchy-disk-speedtest` | `plugins/panels/disk-speedtest/Panel.qml:134` | Overlay | `Exclusive` | transient |
| `omarchy-notifications` | `plugins/notifications/Service.qml:960` | Overlay | **`None`** (`:962`) | transient |
| `omarchy-osd` | `plugins/osd/Osd.qml:131` | Overlay | **`None`** (`:133`) + empty input `mask` | transient |
| `omarchy-lock-preview` | `plugins/lock/Service.qml:294` | Overlay | `Exclusive` (`:296`) | transient |
| `omarchy-bar-drag-ghost` | `plugins/bar/Bar.qml:1166` | Overlay | `None` (`:1168`) | transient (bar edit) |
| `omarchy-bar-move-ghost` | `plugins/bar/Bar.qml:1228` | Overlay | `None` (`:1230`) | transient (bar edit) |

**All bar panels share one namespace.** VERIFIED: audio, bluetooth, clock,
dropbox, monitor, network, power, tailscale, weather and the agents panel all
build on `Ui/KeyboardPanel.qml`, which hard-codes `omarchy-keyboard-panel`
(`:85`). The layer set cannot tell them apart. That turns out not to matter —
Escape closes all of them identically (§2.2).

**The real lock screen is NOT a layer.** VERIFIED: `plugins/lock/Service.qml:230`
is a `WlSessionLock` (ext-session-lock-v1). It never appears in `hyprctl layers`
and therefore can never enter `ctx.layers`. `omarchy-lock-preview` (`:294`) is a
*different* thing — the "what does my lock screen look like" preview, which is a
layer. See §4.1 for why this matters more than it first appears.

**There is no separate launcher.** VERIFIED: application launching is a row kind
inside the menu itself (`Menu.qml` `row.kind === "app"` →
`root.appLibrary.launch(...)`), not its own surface. "Menu" and "launcher" are
the same `omarchy-menu` layer. `guide+x` and `guide+menu` in the current hyprpad
config both run bare `omarchy-menu`, which is `verb=toggle route=root`
(VERIFIED, `bin/omarchy-menu:11-12`) — i.e. they are the same binding twice.

### 1.2 Live verification

Baseline (single monitor, eDP-2): only `omarchy-background` (level 0) and
`omarchy-bar` (level 2). VERIFIED.

```
$ omarchy-menu summon root      → adds omarchy-menu             (level 3)
$ omarchy-shell omarchy.power toggle
                                → adds omarchy-keyboard-panel   (level 3)
$ omarchy-shell shell summon omarchy.clipboard {}
                                → adds omarchy-clipboard        (level 3)
$ omarchy-shell shell summon omarchy.emojis {}
                                → adds omarchy-emojis           (level 3)
$ notify-send …                 → adds omarchy-notifications
$ omarchy-shell osd show …      → adds omarchy-osd, gone ~3 s later
```

All VERIFIED live; each disappears on close. `omarchy-keyboard-panel-dismiss`
never appeared — VERIFIED from `Ui/KeyboardPanel.qml:335-345`, it is instantiated
only for outputs *other* than the panel's own, so a single-monitor session never
maps one. On a multi-head setup it will show up alongside
`omarchy-keyboard-panel`; harmless, since it is `keyboardFocus: None` and the
allowlist below never names it.

### 1.3 The Hyprland events hyprpad actually consumes

VERIFIED live on `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`
while opening and closing the menu and a bar panel:

```
openlayer>>omarchy-menu
openlayer>>omarchy-keyboard-panel
closelayer>>omarchy-keyboard-panel
closelayer>>omarchy-menu
```

Exactly one open and one close per surface, correctly namespaced, no duplicates
and none missing. This is precisely what `HyprEvent::Layer` parses
(`src/hypr.rs:473,477`) and what `ModeEngine` folds into `ctx.layers`
(`src/mode.rs:275-277`). **The mechanism the whole design rests on is sound.**

Note for the mode predicate: the layer set is a faithful proxy for *on screen*,
not merely *loaded*. During probing `hyprpad-cheatsheet` appeared and then
disappeared from `hyprctl layers` on its own (the sheet's `PanelWindow` is
`visible: root.opened`), confirming that Quickshell unmaps these surfaces rather
than keeping them mapped-but-hidden — with the documented exception of the
KeyboardPanel fade-out, which keeps the surface mapped for ~140 ms after the
logical close while dropping keyboard focus (`Ui/KeyboardPanel.qml:87-99`). So a
`closelayer` can trail the visual close by a frame or two. Immaterial here.

---

## 2. Keyboard handling

### 2.1 The menu — the only surface with a hierarchy

`plugins/menu/Menu.qml:1123-1163`, `Keys.priority: Keys.BeforeItem` on a
`focus: true` key catcher inside a surface with `WlrKeyboardFocus.Exclusive`.
VERIFIED, in evaluation order:

| key | behaviour |
|---|---|
| `Delete` | uninstall-app confirm |
| **`Escape`** | filter non-empty → clear filter; **else `cancel()` — closes the whole menu, from any depth** (`:1132-1135`) |
| `Backspace` *with* a filter | delete one char (`Util.editsFilter`, `Commons/Util.qml:98-113`) |
| **`Backspace` or `Left`, filter empty** | **`goBack()` — pops one level** (`:1139-1141`) |
| `Up` / `Down` | move cursor |
| `PageUp` / `PageDown` | move 6 |
| `Return` / `Enter` / `Right` | activate the row (descend into a submenu, or run it) |
| printable | append to the filter |

`goBack()` (`:744-757`): returns `false` **and does nothing** when
`activeMenu === "root"`; otherwise pops `navStack`, or falls back to the entry's
declared `parent`.

**This is the crux.** Neither key alone gives the console idiom:

- `Escape` = *close from any depth*. Loses back-one-level.
- `Backspace` = *back one level*, **dead at root**.

Which one you want depends on depth, and the keyboard alone cannot express
"back, and close if there is nowhere to go back to".

The filter clear on Escape is a genuine third state. With no on-screen keyboard
in play a controller cannot type, so it rarely fires from the pad — but hyprpad's
OSK *can* type into the menu, so it is reachable.

### 2.2 Everything else is flat, and Escape is uniformly right

VERIFIED, one pattern across the whole shell — *clear filter if any, else close*:

- `Ui/PanelKeyCatcher.qml:51` — Escape → `closeRequested()`. This is the shared
  handler for **every bar panel**, and it also gives arrows **and hjkl**
  (`:61-70`), `Return`/`Enter` (`:71`), `Space` (`:76`), `Tab`/`Shift-Tab`
  (`:55`), `x` = delete (`:79`). **No Backspace handler at all** — B as
  Backspace is a dead button in every bar panel today.
- `plugins/clipboard/Clipboard.qml:359-362`, `plugins/emojis/Emojis.qml:199-202`,
  `plugins/reminders/ReminderFlow.qml:134-137`,
  `plugins/image-picker/ImagePicker.qml:410-415` — clear filter, else close.
- `plugins/polkit/PolkitAgent.qml:261-263` — Escape → `cancelRequest()`.
- `plugins/panels/wifiqr/Panel.qml:239`, `Ui/SpeedTestOverlay.qml:109` —
  `Keys.onEscapePressed` → dismiss.

So: **the menu needs a decision; nothing else does.**

### 2.3 Do arrows and A already work?

Yes, everywhere — VERIFIED from the tables above. hyprpad's existing
`dpad_* → arrows` and `a → enter` are already the correct navigation and
activation for both the menu and every panel. `Right` even doubles as activate in
the menu and `Left` as back, so the D-pad alone gives a usable four-way idiom.
The **only** button that needs new thinking is B.

### 2.4 Synthetic keys do reach these surfaces — proven

The load-bearing assumption. VERIFIED live against the menu (an
`Exclusive`-keyboard-focus layer surface), using `wtype`:

```
$ omarchy-menu summon system            # a submenu
$ wtype -k BackSpace
$ omarchy-shell shell call omarchy.menu goBack '{}'   → false
      ^ "false" means already at root, i.e. the synthetic BackSpace popped it

$ wtype -k Escape
$ hyprctl layers | grep -c omarchy-menu → 0            # closed
```

`wtype` uses `zwp_virtual_keyboard`, not uinput, so this is an analogue rather
than an identical path — but hyprpad's own uinput Escape already dismisses
`hyprpad-cheatsheet` (an `OnDemand` layer) in the shipped worktree config, and
`Exclusive` focus is strictly stronger. Treat the uinput path as VERIFIED by
precedent.

---

## 3. IPC

### 3.1 The verb list

`omarchy-shell [-q] <target> <method> [args…]` is a thin wrapper over
`qs ipc -n -p "$OMARCHY_PATH/shell" call` (VERIFIED,
`~/code/omarchy-bluetooth-friendly-name/bin/omarchy-shell`). It also
auto-supplies `{}` as the payload for a 3-argument `shell summon` / `shell
toggle`.

`target: "shell"` (`shell.qml:872-1030`), complete: `ping`, `applyTheme`,
`rescanPlugins`, `reloadConfig`, `toggleBarTransparency`, `setPluginEnabled`,
`enablePlugin`, `putBarWidget`, `moveBarWidget`, `setBarWidget`, `listPlugins`,
`listShellConfig`, `debugBarGeometry`, **`summon(id, payloadJson)`**,
**`hide(id)`**, **`toggle(id, payloadJson)`**, `togglePanelAt(section, index)`,
**`call(id, method, arg)`**.

Per-panel targets come from `Ui/Panel.qml:48-56` — `open` / `close` / `show` /
`hide` / `toggle`, at `omarchy.audio`, `omarchy.bluetooth`, `omarchy.clock`,
`omarchy.dropbox`, `omarchy.monitor`, `omarchy.network`, `omarchy.power`,
`omarchy.tailscale`, `omarchy.weather`, `omarchy.agents`.

**There is no `back` or `pop` verb.** VERIFIED — the surface is open / close /
toggle per route, plus the generic escape hatch `call`.

### 3.2 `call` is the escape hatch, and `goBack` is reachable through it

`shell.callIfLoaded` (`shell.qml:567-579`) invokes *any* function on a loaded
plugin's root item and stringifies the result. `Menu.qml`'s `goBack()` is a
function on that root item, so:

```
$ omarchy-menu summon system
$ omarchy-shell shell call omarchy.menu goBack "{}"   → true      # popped
$ omarchy-shell shell call omarchy.menu goBack "{}"   → false     # at root
```

VERIFIED live. That is exactly the missing primitive, and composing it gives the
console idiom end-to-end — also VERIFIED live, two presses from a submenu:

```sh
[ "$(omarchy-shell shell call omarchy.menu goBack '{}' 2>/dev/null)" = true ] \
  || omarchy-menu close
```

Press 1 popped `system` → root (menu still up). Press 2 closed it. This works
**today**, with no change to either project.

`omarchy-menu refresh` already uses this same `shell call omarchy.menu <verb>`
form (VERIFIED, `bin/omarchy-menu:31`), so it is a supported route, not a trick.

### 3.3 …and yet hyprpad cannot bind it to a bare button

**VERIFIED, and it is decisive**: `src/lua_config.rs:1026-1031` rejects any
non-key action on a bare button —

```rust
let Action::Key(code) = action else {
    return Err(err(format!(
        "h.button/h.osk_button values must be a key, e.g. h.key \"up\" \
         (got {action:?} for '{key}')"
    )));
};
```

The whole bare-button path is a `HashMap<Button, u16>` of keycodes
(`Config::buttons_in`, `src/config.rs:1349-1357`) driven straight into the
virtual keyboard by `drive_buttons` (`src/run.rs:1144-1159`). There is no place
for an `Action` to be dispatched on a bare press. Guide *chords* can carry
`h.exec` (`src/run.rs:2134` → `Hypr::spawn` → `/bin/sh -c`, `src/hypr.rs:207-223`,
so a one-liner with `||` works as-is) — bare buttons cannot.

So the IPC route needs a **larger** hyprpad change than the key route does. It
also costs a `/bin/sh` + `qs ipc` round trip per press: **~40 ms**, measured
(`omarchy-shell shell ping`, twice: 41 ms, 38 ms). Tolerable but not free, and
it fails closed if the shell IPC is wedged.

### 3.4 Verdict: synthetic key, not IPC

Send a key and let the shell decide. It is instant, it needs no route knowledge
in hyprpad, it degrades to "the surface ignores it" rather than to a hung shell,
and it is the only one of the two that fits the existing bare-button
architecture. Keep §3.2's one-liner in the back pocket: it is the right shape for
a `guide+b` chord (which *can* carry exec today) if a stopgap is ever wanted
before the shell learns a back key.

---

## 4. Hazards and the exclusion list

### 4.1 The lock screen — a real hazard, but not one `ctx.layers` can cause

The session lock is `WlSessionLock`, not layer-shell (§1.1), so **no
`ctx.layers` predicate can ever match it** — the proposed modes are structurally
incapable of firing while locked. Good.

The hazard is the other direction, and it **pre-exists this work**: because the
lock is invisible to `ctx.layers` *and* does not change `ctx.focus`, the mode
engine keeps whatever mode was active before the lock — normally `desktop` —
so **hyprpad's bare buttons stay live against the lock screen**. Today that means
the controller can drive `LockView`'s password field: D-pad → arrows, A → Enter
submits, B → Backspace deletes a character. Escape, if B were ever remapped to it
in `desktop`, clears the field (`plugins/lock/LockView.qml:179-182`). Nothing
here leaks or unlocks, and this change does not make it worse — but it is worth
knowing that "never inject keys into the lockscreen" is **not** currently true,
and that a layers-based mode cannot be the thing that fixes it. If the owner
wants it fixed, the lever is `omarchy-shell lock isLocked` (`shell.qml:511-519`,
returns `"true"`/`"false"`) or Hyprland's own lock state, surfaced into `ctx` as
something like `ctx.locked` — a separate, worthwhile piece of work.

`omarchy-lock-preview` **is** a layer, and must be **excluded**: `Escape` there
only clears a disabled password field, and the preview actually dismisses on a
*click* (`plugins/lock/Service.qml:315`). B would be a dead button. (Tiny
upstream gap: `Keys.onEscapePressed: root.previewVisible = false` would fix it.)

### 4.2 Never match on a prefix

`omarchy-background` and `omarchy-bar` are in `ctx.layers` **at all times**
(VERIFIED). Any predicate of the form "`ctx.layers` contains something starting
with `omarchy-`" is permanently true and would put the session in the menu mode
forever. **The predicate must be an allowlist.**

### 4.3 Exclusions, with reasons

| namespace | why excluded |
|---|---|
| `omarchy-background` | persistent; `keyboardFocus: None` |
| `omarchy-bar` | persistent; no keyboard focus |
| `omarchy-notifications` | `keyboardFocus: None` — it never has the keyboard, so a synthetic Escape would land on whatever is *behind* it. Notifications come and go on their own; B must not become "dismiss whatever is underneath" every time one pops. |
| `omarchy-osd` | `keyboardFocus: None`, empty input mask — purely decorative, and it fires on every volume/brightness change |
| `omarchy-keyboard-panel-dismiss` | `keyboardFocus: None`; a click-catching twin on other outputs |
| `omarchy-bar-drag-ghost`, `omarchy-bar-move-ghost` | `keyboardFocus: None`; bar-editing artifacts |
| `omarchy-lock-preview` | Escape is a no-op there (§4.1) |
| `hyprpad-cheatsheet`, `hyprpad-osk` | hyprpad's own; already handled by the existing `cheatsheet` mode and the OSK gate (`osk/src/surface.rs:45`) |

The rule that generates this list: **include a namespace only if the surface
takes keyboard focus and closes on Escape.** Both halves are checkable in source,
and both were checked.

### 4.4 `hyprctl layers` does not expose keyboard interactivity

VERIFIED on Hyprland 0.56.2 (`hypxrland` snapshot 67200a8383): each layer object
carries exactly `address`, `x`, `y`, `w`, `h`, `alpha`, `depth`, `namespace`,
`pid`. No keyboard-interactivity field, and the `openlayer` event carries the
namespace *only*. So the appealing predicate "any Omarchy layer that has keyboard
focus" is **not expressible** — the compositor does not tell us. The allowlist is
not a shortcut; it is the only option available.

### 4.5 Two side effects the config must handle

- **The cursor would die.** `h.cursor` and `h.scroll` are guarded
  `only_in { "desktop" }`. Introducing `omarchy-menu` / `omarchy-ui` as separate
  modes means the trackpad cursor **stops working the moment the menu opens** —
  a regression, since the menu is fully mouse-driven (row hover, a scrim
  `MouseArea` that cancels, `Menu.qml:1099-1102`). Both guards must be widened.
  This is the easiest thing to forget and the most obvious in use.
- **Opening the menu over a game stops forwarding.** Placing the shell modes
  above `game` in declaration order (which is required — the menu is drawn over a
  fullscreen game and takes `Exclusive` keyboard focus) means `forward` goes off
  while the menu is up. That is correct and desirable, but it *is* a behaviour
  change: the game stops seeing the pad for as long as the menu is open.

### 4.6 The menu is also a dmenu

`Menu.qml` doubles as a generic picker for scripts (`openDmenu`,
`root.dmenuActive`, `mode === "input"`). In that state `Escape` → `cancel()` →
`finishRequest(null)`, which is a clean cancel the calling script understands
(VERIFIED, `:831-835`). `Backspace` at root is dead. Another point for the
`KEY_BACK` end state: the shell can then do the right thing per sub-mode without
hyprpad knowing a dmenu is up.

---

## 5. Prior art

**The convention is unanimous** across every gamepad-driven shell, and it is the
one the menu cannot currently express with a single key:

> **B (east button on an Xbox layout) = back one level; at the root of the
> navigation stack, B closes/exits the overlay.** A = confirm. Never a
> confirmation prompt for B.

- **Steam Big Picture / Deck Gaming Mode**: B backs out of a submenu; B at the
  top of the Steam-button menu closes it. Nintendo-layout controllers get the
  positions swapped, not the semantics.
- **Bazzite / gamescope-session**: inherits Big Picture's mapping wholesale; the
  session's job is to get you into Steam's UI, which owns the idiom.
- **Android TV / Google TV**: `KEYCODE_BACK` pops the activity/fragment stack and
  finishes the activity at the root — the exact "back-one, close-at-root"
  contract, and the reason `KEY_BACK` exists as a key at all. TV remotes send it.
- **ROG Ally (Armoury Crate SE)**: B backs out; the dedicated Command Centre
  button closes the overlay outright.

Two things follow. First, **hyprpad's B should behave the same way**, which
requires depth knowledge the keyboard cannot carry. Second — and this is the
argument for `KEY_BACK` over anything bespoke — **the industry already
standardised the wire format for this.** `KEY_BACK` / `XF86Back` /
`Qt.Key_Back` is the same key an Android TV remote, a Chromebook, and every
browser's back button send. A shell that handles it works with any of them, not
just with hyprpad.

**Omarchy's own tracker: could not be searched.** GitHub's search API refuses
every query against `basecamp/omarchy` for this token (`gh api graphql
search(...)` returns `issueCount: 0` even for `bluetooth`, and the REST search
endpoint 422s with "the listed users and repositories cannot be searched"), while
`gh issue list` and the GraphQL `discussions` connection both work (9688+ issues,
1655 discussions). So this is a **tooling limitation, not evidence of absence** —
the owner should grep the tracker directly before assuming no one has raised
gamepad navigation. INFERRED from the public docs and the shipped shell: there is
no gamepad affordance in Omarchy today — no joystick/gamepad handling anywhere in
the QML, and the manual documents keyboard navigation only.

---

## 6. Recommendation

### 6.1 Ship now — config only, no code change anywhere

Additions to `config/hyprpad.lua` (the daemon config). This is the ctx.layers
worktree's file plus the deltas below.

```lua
-- Omarchy shell surfaces that are MODAL: transient, keyboard-focused, and
-- dismissed by Escape. An allowlist and never a prefix match, because
-- `omarchy-bar` and `omarchy-background` are in ctx.layers at ALL times --
-- `omarchy-*` would pin the session in this mode forever. The membership rule
-- is checkable in the shell's source: the surface must take keyboard focus AND
-- close on Escape. `omarchy-notifications` and `omarchy-osd` fail the first
-- test (keyboardFocus: None), so B over a notification must not eat the
-- Escape; `omarchy-lock-preview` fails the second (it dismisses on a click).
local OMARCHY_MODAL = {
  ["omarchy-keyboard-panel"]    = true,  -- EVERY bar panel shares this one
  ["omarchy-clipboard"]         = true,
  ["omarchy-emojis"]            = true,
  ["omarchy-image-selector"]    = true,
  ["omarchy-reminders"]         = true,
  ["omarchy-polkit"]            = true,
  ["omarchy-network-qr"]        = true,
  ["omarchy-speed-test"]        = true,
  ["omarchy-network-speedtest"] = true,
  ["omarchy-disk-speedtest"]    = true,
}

-- Declared ABOVE `game`, for the same reason `cheatsheet` is: these are drawn
-- over a fullscreen game and take the keyboard from it, so the pad belongs to
-- the shell while they are up.

-- The menu is the one Omarchy surface with a navigation HIERARCHY, so it gets
-- its own mode: B means "up one level" here and "close" everywhere else.
h.mode("omarchy-menu").when(function(ctx)
  return ctx.layers:has("omarchy-menu")
end)

-- Everything else the shell puts in front of you: flat, and Escape closes it.
h.mode("omarchy-ui").when(function(ctx)
  for _, ns in ipairs(ctx.layers) do
    if OMARCHY_MODAL[ns] then return true end
  end
  return false
end)
```

Then widen the existing guards and bind B. **The cursor and scroll lines matter
as much as the buttons** — without them the trackpad dies the moment the menu
opens, and the menu is fully mouse-driven:

```lua
h.cursor { …unchanged knobs…, only_in = { "desktop", "omarchy-menu", "omarchy-ui" } }
h.scroll { …unchanged knobs…, only_in = { "desktop", "omarchy-menu", "omarchy-ui" } }

h.button("dpad_up",    h.key "up"):only_in("desktop", "omarchy-menu", "omarchy-ui")
h.button("dpad_down",  h.key "down"):only_in("desktop", "omarchy-menu", "omarchy-ui")
h.button("dpad_left",  h.key "left"):only_in("desktop", "omarchy-menu", "omarchy-ui")
h.button("dpad_right", h.key "right"):only_in("desktop", "omarchy-menu", "omarchy-ui")
h.button("a", h.key "enter"):only_in("desktop", "omarchy-menu", "omarchy-ui")

h.button("b", h.key "backspace"):only_in("desktop")
h.button("b", "Close cheat sheet", h.key "escape"):only_in("cheatsheet")
h.button("b", "Back",  h.key "backspace"):only_in("omarchy-menu")  -- pops one level
h.button("b", "Close", h.key "escape"):only_in("omarchy-ui")       -- flat: just close
```

Four guarded `h.button("b", …)` declarations are the supported idiom, not a
workaround: `Config::buttons_in` (`src/config.rs:1349-1357`) resolves the base
binding first and then each alternate in declaration order, first passing guard
wins, and modes are exclusive so they never compete. The "bound more than once"
warning (`src/lua_config.rs:541-550`) fires only for an *unguarded* later
binding, so this stays quiet. VERIFIED from source.

`dpad_left` needs no special case — `Left` is *already* back-one-level in the
menu (`Menu.qml:1139`) and a cursor move in the panels, both correct.

**Leave X, Y and the Menu button unbound in these modes.** Y → Space looks
tempting (Space activates in `PanelKeyCatcher`) but in the menu Space is a
printable character that types into the filter — and A already activates
everywhere, so it buys nothing. X → Delete would put "uninstall this app" on a
face button, which is exactly the kind of accident a controller should not
enable. If a "get me all the way out" button is wanted, bare `menu` →
`h.key "escape"` in both modes is unambiguous and cheap — though `guide+menu`
already toggles the menu shut from any depth (VERIFIED, `bin/omarchy-menu:11-12`),
so the escape hatch exists.

**Known limitation, and the only one:** B at the menu **root** does nothing.
`goBack()` returns `false` and swallows the key. You back out of submenus fine,
then the button goes dead and you need `guide+menu` to close. This is the gap
§6.2 exists to close.

### 6.2 The end state — teach both sides the key that already means this

Two small, independent changes; each is useful alone, and together they give the
console idiom exactly.

**hyprpad — one line.** `src/config.rs:310-327`'s `key_code` table has no `back`:

```rust
"back" => 158,  // KEY_BACK -- XF86Back / Qt.Key_Back; the TV-remote "back"
```

VERIFIED that this reaches clients correctly: evdev 158 → xkb `<I166>` (158 + 8)
→ keysym `XF86Back` in the default `us` keymap (`xkbcli compile-keymap --layout
us` line 1729). Qt maps `XF86Back` to `Qt.Key_Back` (INFERRED — standard Qt
keysym mapping, not tested against a live Quickshell handler because none exists
yet). Pure data addition, no new code path, and it makes `h.key "back"` available
to every future consumer.

**Omarchy — about six lines, two files.** In `Menu.qml:1123`'s handler, alongside
the existing Escape branch:

```qml
} else if (event.key === Qt.Key_Back) {
  // The console idiom: up one level, and close from the root. Distinct from
  // Escape (always close) and from Backspace (edits the filter first), so no
  // existing keyboard behaviour changes.
  if (!root.goBack()) root.cancel()
  event.accepted = true
}
```

and in `Ui/PanelKeyCatcher.qml:51`, fold `Qt.Key_Back` into the Escape branch so
every bar panel gets it for free (`event.key === Qt.Key_Escape || event.key ===
Qt.Key_Back`). Same for the handful of surfaces with their own
`Keys.onEscapePressed`.

Then hyprpad collapses to **one mode and one binding**, and the allowlist stops
having to distinguish the menu from anything else:

```lua
h.button("b", "Back", h.key "back"):only_in("omarchy-menu", "omarchy-ui")
```

VERIFIED that `XF86Back` is currently inert in the menu, so the shell change is a
genuine prerequisite rather than a nicety: with the menu at `system`, `wtype -k
XF86Back` changed nothing (`goBack` afterwards still returned `true`, i.e. still
in the submenu).

**Why this over the IPC one-liner** (which works today, §3.2): the key route
needs a one-line hyprpad change where IPC needs a structural one (§3.3); it costs
nothing per press where IPC costs ~40 ms and a shell spawn; hyprpad never learns
an Omarchy route name, a plugin id, or a method name; and the shell keeps
authority over what "back" means in each of its own surfaces — including the
dmenu and filter states hyprpad has no way to see.

### 6.3 Worth proposing upstream?

**Yes, and the `Qt.Key_Back` handler is the better proposal of the two.** It is
small, additive, changes no existing keyboard behaviour (Escape and Backspace
keep their current meanings exactly), and it is the standard key — it makes the
Omarchy shell navigable by *any* device that speaks it, TV remotes and
Chromebooks included, not just by hyprpad. Frame it as "the shell should
understand the standard back key", not as "please support my Steam Controller".

Two smaller upstream items worth raising alongside it:

- **A `back()` method on the menu**, so `omarchy-shell shell call omarchy.menu
  back` is a supported verb rather than a lucky reach through `callIfLoaded` into
  `goBack`. Five lines: clear the filter, else `goBack()`, else `cancel()`.
  Generalising it — a default `back()` on `Ui/Panel.qml` that aliases `close()` —
  would make `back` a uniform verb across every panel, which is the shape a
  future `omarchy-shell shell back` would want.
- **`Keys.onEscapePressed` on the lock preview** (`plugins/lock/Service.qml:294`),
  which is currently click-only to dismiss — a two-word fix and a small
  accessibility win regardless of controllers.

Not worth proposing: anything that changes what Escape or Backspace already do in
the menu. Both have settled keyboard meanings and remapping either to suit a
gamepad would be a regression for the keyboard users who are the overwhelming
majority.

---

## Appendix — commands used for live verification

Run against the live unlocked session on 2026-09-01; every one is read-only or
self-undoing.

```sh
hyprctl layers -j
omarchy-menu summon root|system ; omarchy-menu close
omarchy-shell omarchy.power toggle|close
omarchy-shell shell summon|hide omarchy.clipboard|omarchy.emojis "{}"
omarchy-shell shell call omarchy.menu goBack "{}"
omarchy-shell shell ping                       # timed: 41 ms, 38 ms
socat -u UNIX-CONNECT:"$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock" -
wtype -k BackSpace|Escape|XF86Back
xkbcli compile-keymap --layout us | grep -n 'I166\|XF86Back'
diff -rq ~/code/omarchy-bluetooth-friendly-name/shell /usr/share/omarchy/shell
```

Session left at baseline: `omarchy-background` + `omarchy-bar` only.
