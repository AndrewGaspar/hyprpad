// hyprpad's bar widget: the controller glyph and the mode the pad is in.
//
// It is a *reader*, never a client: the daemon publishes
// `$XDG_RUNTIME_DIR/hyprpad/status.json` and this watches it. Nothing here
// talks to the daemon, so a widget that is running while the daemon is not
// costs one failed open.
//
// ## Why the slot disappears
//
// A bar item for hardware that is usually absent should not sit there saying
// "absent". Three things must all hold before the widget takes any space:
//
//   1. the status file parses,
//   2. it says `connected: true` — the puck is here *now*, not merely that the
//      daemon is up (it sits through the startup and reconnect waits, and
//      publishes `connected: false` throughout both), and
//   3. `/proc/<pid>` still answers for the pid in the file.
//
// (3) is what covers the one case the RAII writer cannot: a daemon killed with
// SIGKILL never runs its guard, so it can leave a file behind that still claims
// a live controller. The pid in the object is there precisely so a reader can
// tell that apart from a live daemon, and the probe is a plain read of
// `/proc/<pid>/cmdline` — no process spawned.
//
// ## Why there is still a timer
//
// `FileView`'s watcher only arms on a file that existed when the view first
// loaded: a missing file fails, and nothing afterwards wakes it. So the poll
// runs *only* while there is no parsed status — the cold start where the bar
// came up before the daemon did — and stops the moment one loads, after which
// the watcher carries every update, including the file being removed and
// coming back. The second timer re-runs the liveness probe, and runs only
// while there is a status file to distrust.
//
// ## Why the ring lets go of itself
//
// The widget also hosts bar mode (the focus ring, far below), and bar mode
// maps a keyboard-focused layer surface whose namespace is `omarchy-bar-nav`.
// hyprpad keys its MODES on layer namespaces, so that surface is not a hint
// about bar mode — it *is* bar mode: while it is mapped, `config/hyprpad.lua`
// puts the daemon in `omarchy-ui` and holds it there. A ring left up
// unattended is therefore never merely a rectangle nobody noticed:
//
//   * `omarchy-ui` outranks `game`, so a game gets no controller forwarding
//     for as long as the surface lives;
//   * B is bound to Back in `omarchy-ui` and to Backspace on the desktop, so
//     typing quietly acquires the wrong Back key;
//   * the 1x1 surface still owns the keyboard, so keys land on a ring that is
//     drawn on nothing the user is looking at.
//
// This is not hypothetical. The ring was once observed still mapped after a
// session lock and for hours afterwards, pinning `omarchy-ui` the whole time;
// only an explicit `navLeave` cleared it. So bar mode is built to expire, not
// to persist. Five things end it without being asked:
//
//   1. an idle timeout (`navIdleSec`, default 12 s) that every step, every
//      activate and every verb restarts;
//   2. losing the keyboard to anything that is not a bar panel it deliberately
//      yielded to;
//   3. the session lock;
//   4. the bar reporting nothing left to focus;
//   5. this widget being destroyed — a plugin hot-reload must not be able to
//      strand the surface behind it.
//
// The ring is allowed to be forgotten. It is not allowed to outlive the
// attention that raised it.

import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "hyprpad.status"

  // --- what the daemon publishes -------------------------------------------

  readonly property string runtimeDir: {
    var d = Quickshell.env("XDG_RUNTIME_DIR")
    return d ? String(d).replace(/\/$/, "") : ""
  }
  // Empty when XDG_RUNTIME_DIR is unset, which is also where the daemon
  // declines to write one. The widget then simply never shows.
  readonly property string statusPath:
    root.runtimeDir ? root.runtimeDir + "/hyprpad/status.json" : ""

  // The parsed object, or null when there is no readable status file. Not
  // named `status`/`state`: both are taken on QML items.
  property var daemonStatus: null
  // Whether `/proc/<pid>` answered on the last probe.
  property bool daemonAlive: false

  readonly property int daemonPid:
    root.daemonStatus && typeof root.daemonStatus.pid === "number" ? root.daemonStatus.pid : 0
  readonly property bool controllerConnected:
    root.daemonStatus ? root.daemonStatus.connected === true : false
  readonly property string mode:
    root.daemonStatus && root.daemonStatus.mode ? String(root.daemonStatus.mode) : ""
  readonly property string controller:
    root.daemonStatus && root.daemonStatus.controller
      ? String(root.daemonStatus.controller) : "Steam Controller"

  // Mode names are config identifiers and read best as written — except the
  // daemon's one built-in context, which is an acronym and looks like a typo
  // in lower case.
  readonly property string modeLabel: root.mode === "osk" ? "OSK" : root.mode

  // The whole visibility rule, in one place.
  readonly property bool live:
    root.daemonStatus !== null && root.controllerConnected && root.daemonAlive

  visible: root.live
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // --- reading it ----------------------------------------------------------

  function parseStatus(content) {
    var parsed = null
    try {
      parsed = JSON.parse(String(content || ""))
    } catch (e) {
      console.warn("hyprpad.status: unparseable status file:", e)
      parsed = null
    }
    root.daemonStatus =
      parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null
    if (root.daemonStatus === null) root.daemonAlive = false
    else root.probeDaemon()
  }

  function clearStatus() {
    root.daemonStatus = null
    root.daemonAlive = false
  }

  // Re-read everything. Exposed over IPC so a confused bar can be nudged by
  // hand without restarting the shell.
  function refresh() {
    statusFile.reload()
    root.probeDaemon()
  }

  // The liveness check. `path` is assigned rather than bound so the read
  // always uses the pid the caller just parsed, never one a pending binding
  // has yet to deliver.
  function probeDaemon() {
    if (root.daemonPid > 0) {
      procFile.path = "/proc/" + root.daemonPid + "/cmdline"
      procFile.reload()
    } else {
      procFile.path = ""
      root.daemonAlive = false
    }
  }

  FileView {
    id: statusFile
    path: root.statusPath
    watchChanges: true
    printErrors: false
    // `text()` is stale inside the change signal; reload and let `onLoaded`
    // do the parsing.
    onFileChanged: reload()
    onLoaded: root.parseStatus(text())
    onLoadFailed: root.clearStatus()
  }

  FileView {
    id: procFile
    printErrors: false
    onLoaded: root.daemonAlive = true
    onLoadFailed: root.daemonAlive = false
  }

  // Cold start only: the bar came up before the daemon, so there is no file to
  // watch yet. Stops as soon as one parses.
  Timer {
    interval: 4000
    running: root.daemonStatus === null && root.statusPath !== ""
    repeat: true
    onTriggered: statusFile.reload()
  }

  // A daemon that is merely idle rewrites nothing, so the file alone cannot
  // say whether it is still there. Runs only while a status file exists.
  Timer {
    interval: Math.max(1, root.setting("livenessSec", 5)) * 1000
    running: root.daemonStatus !== null
    repeat: true
    onTriggered: root.probeDaemon()
  }

  IpcHandler {
    target: "hyprpad.status"

    function refresh(): void {
      root.broadcast("refresh")
    }

    // Bar mode (see "bar navigation" below). Every verb is broadcast because
    // one bar surface exists per monitor while an IPC target routes to a single
    // handler; each copy then decides for itself whether it is the one on the
    // focused output.
    function navEnter(): void {
      root.broadcast("navEnterLocal")
    }

    function navLeave(): void {
      root.broadcast("navLeaveLocal")
    }

    // Decided once, here, rather than per copy: a peer that flipped its own
    // state first would otherwise flip the answer for the peers after it.
    function navToggle(): void {
      root.broadcast(root.navAnyActive() ? "navLeaveLocal" : "navEnterLocal")
    }

    function navNext(): void {
      root.broadcast("navNextLocal")
    }

    function navPrev(): void {
      root.broadcast("navPrevLocal")
    }

    function navActivate(): void {
      root.broadcast("navActivateLocal")
    }

    // The right-click action of the focused widget: mute-all, the Bluetooth
    // radio, the battery percentage — the ones with no IPC verb of their own.
    function navSecondary(): void {
      root.broadcast("navSecondaryLocal")
    }

    // A string, not a bool: every IPC verb this shell ships returns one — the
    // lock service's own `isLocked` is this exact line — and `qs ipc` prints
    // whatever comes back, so `[[ $(...) == true ]]` works in a script.
    //
    // Ask for it WITHOUT `-q`. `omarchy-shell`'s quiet mode is "best effort,
    // say nothing, always exit 0": it drops the answer on the floor
    // (`bin/omarchy-shell`, `if (( !QUIET )) && [[ -n $output ]]`). `-q` is
    // right for the fire-and-forget verbs that `config/hyprpad.lua` binds and
    // wrong for every question, which is why asking the ring whether it was up
    // once looked like a widget that could not answer.
    function navIsActive(): string {
      return root.navAnyActive() ? "true" : "false"
    }

    // Everything the ring knows about itself, one JSON object per live copy.
    //
    // `navIsActive` says whether the surface is up; this says whether anything
    // is going to take it down, which is the question that matters — a ring
    // that is up with `idleRunning: false` is a ring that is never leaving.
    // It also reports how many copies there are, because the verbs broadcast
    // and only the copy on the focused output raises the surface.
    //
    // And it is a version probe. Omarchy logs "Local plugin changed,
    // reloading" on every write into `~/.config/omarchy/plugins/`, but a
    // long-running Quickshell keeps serving the widget QML it compiled at
    // startup (shell/README.md, "Making a change actually run"). A shell on
    // stale code answers `No such method`, which is the whole difference
    // between "the timeout is broken" and "the timeout is not installed".
    // Ask WITHOUT `-q`, for the reason above.
    function navStatus(): string {
      var peers = root.navPeers()
      var out = []
      for (var i = 0; i < peers.length; i++) {
        if (peers[i] && typeof peers[i].navStatusLocal === "function")
          out.push(peers[i].navStatusLocal())
      }
      return JSON.stringify(out)
    }
  }

  // --- bar navigation: the ring --------------------------------------------
  //
  // Variant (a′) of docs/research/bar-navigation.md §2: a focus ring over the
  // bar's OWN click targets, driven from this widget, with no patch to
  // Omarchy's `Bar.qml`. It is a prototype for the shell-side patch (§2(a)) and
  // deliberately leans on three things the bar already owns:
  //
  //   * `bar.clickTargets`      — every `WidgetButton` registers itself there,
  //                               so the ring's stops are exactly the things a
  //                               mouse can click, with the bar's own
  //                               visibility rules (`moduleTargetClickable`)
  //                               already applied. Workspace numbers, active
  //                               indicators and this very widget come for free.
  //   * `target.triggerPress()` — "activate" is the same call a click makes, so
  //                               it cannot drift from what clicking does.
  //   * `bar.activePopout`      — the ring yields the keyboard while a panel is
  //                               open and takes it back when the panel closes.
  //
  // ## How the controller reaches it, with no per-press IPC
  //
  // hyprpad keys its modes on layer-shell NAMESPACES (`openlayer`/`closelayer`),
  // not on anything it is told. So entering bar mode maps a transient,
  // keyboard-focused layer surface whose namespace is `omarchy-bar-nav`, and
  // `config/hyprpad.lua` lists that namespace in the `omarchy-ui` allowlist.
  // From that moment the pad is in `omarchy-ui`, where the daemon ALREADY binds
  //
  //     D-pad -> arrow keys      A -> Enter      B -> XF86Back (Qt.Key_Back)
  //
  // and this surface has the keyboard, so those keys land here and `navKey`
  // below moves and activates the ring. That is the whole point of the design:
  // **one chord to enter, and not one new binding for the walking**. The IPC
  // verbs (`navNext`, `navPrev`, `navActivate`, ...) exist for scripts and for
  // a keyboard user, not for the D-pad.
  //
  // Escape and Back both leave. So does the arrow that steps OFF the bar — Down
  // on a top bar, Up on a bottom one, and the inward arrow on a vertical bar —
  // which is why the movement axis and the leave key can never collide.
  //
  // ## Levels
  //
  //     desktop --guide+dpad_up--> ring --A--> panel --A--> action
  //        ^                        |  ^                |
  //        +---------- B -----------+  +------- B ------+
  //
  // B on the ring leaves; B in a panel closes the panel and the ring comes back
  // (the surface re-primes its focus when `activePopout` clears).
  //
  // ## The ring itself
  //
  // A `Rectangle` created as a CHILD of the focused target: it then tracks that
  // widget's geometry with a plain anchor, paints in the target's own slot (so
  // no z-order fight with the slots either side), and — the reason it is
  // created rather than reparented — dies with the target if that widget goes
  // away underneath it. A workspace button vanishing when its workspace closes
  // is a real case; the ring is simply rebuilt on the next step.

  property bool navActive: false
  // The focused click target, and the ring drawn on it.
  property var navTarget: null
  property var navRing: null
  // Where the ring was when bar mode was last left, so re-entering resumes
  // rather than always starting over on the right-hand section.
  property var navLastTarget: null
  // Focus prime, copied from Ui/KeyboardPanel.qml: Exclusive to acquire focus
  // at map time, then OnDemand ~75 ms later so the compositor stops routing
  // every pointer event on every output to this surface.
  property bool navPrimed: false

  // The bar surface this copy of the widget lives on. One bar exists per
  // monitor, so this is also what scopes the ring to one screen.
  readonly property var navBarWindow: root.QsWindow ? root.QsWindow.window : null

  // While a panel is open it owns the keyboard: the nav surface stays MAPPED
  // (hyprpad must keep seeing `omarchy-bar-nav`) but drops its focus, so the
  // panel's own PanelKeyCatcher gets the D-pad and B.
  readonly property bool navYielded:
    root.navActive && !!(root.bar && root.bar.activePopout)

  // --- letting go ----------------------------------------------------------
  //
  // See "Why the ring lets go of itself" at the top of this file: while the
  // nav surface is mapped, hyprpad is pinned in `omarchy-ui`. So each of the
  // five exits below is a safety property, not a nicety.

  // The idle window. Clamped to the manifest's own range, because the setting
  // reaches us straight out of shell.json and a hand-edited 0 must not buy a
  // ring that never expires.
  readonly property int navIdleSec: {
    var raw = Math.round(Number(root.setting("navIdleSec", 12)))
    if (!(raw > 0)) raw = 12
    return Math.max(3, Math.min(60, raw))
  }

  // Does the compositor still send this surface keys? On Wayland, window
  // activation IS keyboard focus (`wl_keyboard.enter`), so QtQuick's attached
  // `Window.active` on an item inside the layer surface answers exactly the
  // question the ring needs to ask, with no Quickshell-private API: there is
  // no "do I have focus" property on `PanelWindow`/`WlrLayershell`, only the
  // `keyboardFocus` we ask *for*, and upstream's `Ui/KeyboardPanel.qml` never
  // needed the answer because it dismisses on an outside click instead.
  readonly property bool navSurfaceFocused: navLayer.visible && navKeys.windowActive

  // Focus arrives a commit or two after the map, so "not focused" only means
  // something once it has been focused at least once. Until then the idle
  // timeout is the backstop — a ring the compositor never fed is still a ring
  // that expires.
  property bool navFocusSeen: false

  // The session lock. `omarchy.lock` is a first-party service plugin
  // (`shell/plugins/lock/Service.qml`) whose `locked` is the same fact
  // `omarchy-shell lock isLocked` prints, and the host injects itself into the
  // bar as `shell`, which hands out services by id.
  //
  // Losing the keyboard to the lock surface would catch this on its own; the
  // binding is what makes it immediate, and what still holds if the compositor
  // turns out to be stingier with focus events than expected. Both, because
  // this is the case that actually happened.
  readonly property var navLockService: root.navLockFrom(root.bar)
  readonly property bool navSessionLocked:
    !!(root.navLockService && root.navLockService.locked === true)

  // The bar arrives as an untyped argument, which is the honest shape as well
  // as the quiet one: `BarWidget` types `bar` as `QtObject`, so nothing about
  // the host is known here at compile time anyway. Every hop is optional, so a
  // bar that injects no `shell`, or a shell with the lock plugin disabled,
  // costs a null rather than an error — and the ring falls back on its idle
  // timeout, which is why this may be a best-effort lookup at all.
  function navLockFrom(host) {
    var shellRoot = host ? host.shell : null
    if (!shellRoot || typeof shellRoot.serviceFor !== "function") return null
    return shellRoot.serviceFor("omarchy.lock")
  }

  // The arrow that steps off the bar. Always perpendicular to the axis the
  // ring walks, so it can never be a move.
  readonly property int navLeaveKey: {
    var pos = root.bar ? String(root.bar.position || "top") : "top"
    if (pos === "bottom") return Qt.Key_Up
    if (pos === "left") return Qt.Key_Right
    if (pos === "right") return Qt.Key_Left
    return Qt.Key_Down
  }

  function navScreenName(item) {
    var window = item && item.QsWindow ? item.QsWindow.window : null
    return window && window.screen ? String(window.screen.name || "") : ""
  }

  function navPeers() {
    return root.bar && typeof root.bar.moduleWidgets === "function"
      ? root.bar.moduleWidgets(root.moduleName) : [root]
  }

  // Which copy of this widget owns the ring. Only one may map the nav surface,
  // and it has to be the one on the output the user is looking at — the same
  // rule panel hotkeys follow (BarModel.pickPanelSlot). With no focused output
  // reported, the first registered copy wins, which is at least stable.
  function navOwner() {
    var peers = root.navPeers()
    if (peers.length === 0) return root
    if (peers.length === 1) return peers[0]
    var focused = root.bar && typeof root.bar.focusedScreenName === "function"
      ? root.bar.focusedScreenName() : ""
    if (focused) {
      for (var i = 0; i < peers.length; i++)
        if (root.navScreenName(peers[i]) === focused) return peers[i]
    }
    return peers[0]
  }

  function navAnyActive() {
    var peers = root.navPeers()
    for (var i = 0; i < peers.length; i++)
      if (peers[i] && peers[i].navActive === true) return true
    return false
  }

  // This copy's share of `navStatus`. Every exit above is represented, so the
  // answer says not just that the ring is up but which of the five is still
  // armed behind it: `idleRunning` for the idle clock, `focusSeen`/`focused`
  // for the focus watch (it only arms once the compositor has fed the surface
  // at least once), `lockService` for whether the lock hop resolved at all,
  // and `targets` for the stops the ring has left to walk.
  function navStatusLocal() {
    return {
      screen: root.navScreenName(root),
      owner: root.navOwner() === root,
      active: root.navActive,
      idleSec: root.navIdleSec,
      idleMs: navIdleTimer.interval,
      idleRunning: navIdleTimer.running,
      yielded: root.navYielded,
      focused: root.navSurfaceFocused,
      focusSeen: root.navFocusSeen,
      locked: root.navSessionLocked,
      lockService: !!root.navLockService,
      targets: root.navTargets().length
    }
  }

  // The ring's stops, in bar order. The bar's own clickability filter decides
  // membership — hidden widgets, concealed indicators and anything without a
  // `triggerPress` drop out exactly as they do for a mouse — and the position
  // inside the bar surface decides the order, so the ring walks left-to-right
  // (top-to-bottom on a vertical bar) however the registry happened to fill.
  function navTargets() {
    var out = []
    if (!root.bar) return out
    var window = root.navBarWindow
    var content = window ? window.contentItem : null
    var targets = root.bar.clickTargets || []
    for (var i = 0; i < targets.length; i++) {
      var target = targets[i]
      if (!root.bar.moduleTargetClickable(target)) continue
      if (window && !root.bar.targetBelongsToWindow(target, window)) continue
      if (!(target.width > 0) || !(target.height > 0)) continue
      var pos = null
      try {
        pos = target.mapToItem(content, 0, 0)
      } catch (e) {
        continue
      }
      if (!pos) continue
      out.push({ target: target, key: root.vertical ? pos.y : pos.x, order: i })
    }
    out.sort(function(a, b) { return a.key === b.key ? a.order - b.order : a.key - b.key })
    return out.map(function(row) { return row.target })
  }

  // The ModuleSlot a click target sits in, so the ring can ask which bar
  // section it is in. Bounded walk: a target is a handful of items deep.
  function navSlotFor(target) {
    if (!root.bar) return null
    var slots = root.bar.moduleSlots || []
    var node = target
    for (var guard = 0; node && guard < 12; guard++) {
      if (slots.indexOf(node) !== -1) return node
      node = node.parent
    }
    return null
  }

  // Where the ring lands on a cold entry: the right-hand section, because that
  // is where Bluetooth, network, audio, display and power live — the controls
  // the pad is reaching for.
  function navStartTarget(list) {
    for (var i = 0; i < list.length; i++) {
      var slot = root.navSlotFor(list[i])
      if (slot && String(slot.region || "") === "right") return list[i]
    }
    return list.length > 0 ? list[0] : null
  }

  function navSetTarget(target) {
    if (root.navRing) {
      root.navRing.destroy()
      root.navRing = null
    }
    root.navTarget = target || null
    if (root.navTarget) root.navLastTarget = root.navTarget
    if (root.navActive && root.navTarget)
      root.navRing = navRingComponent.createObject(root.navTarget)
  }

  function navStep(delta) {
    if (!root.navActive) return
    root.navPoke()
    var list = root.navTargets()
    if (list.length === 0) {
      root.navLeaveLocal()
      return
    }
    var at = list.indexOf(root.navTarget)
    var next = at < 0 ? (delta > 0 ? 0 : list.length - 1) : at + delta
    root.navSetTarget(list[((next % list.length) + list.length) % list.length])
  }

  // Entry. Refuses on a parked bar: `barHidden` moves the surface past the
  // screen edge, so there would be a ring on nothing.
  function navEnterLocal() {
    if (!root.bar) return
    if (root.bar.barHidden === true) return
    // Refusing is the same rule as leaving: `onNavSessionLockedChanged` only
    // fires on an edge, so a lock that was already up when the verb arrived
    // has to be caught here.
    if (root.navSessionLocked) return
    if (root.navOwner() !== root) {
      root.navLeaveLocal()
      return
    }
    var list = root.navTargets()
    if (list.length === 0) return
    var resume = root.navLastTarget && list.indexOf(root.navLastTarget) !== -1
      ? root.navLastTarget : root.navStartTarget(list)
    root.navActive = true
    root.navSetTarget(resume)
    root.navPoke()
  }

  function navLeaveLocal() {
    if (!root.navActive && !root.navRing) return
    root.navActive = false
    root.navSetTarget(null)
    navIdleTimer.stop()
    navFocusLossTimer.stop()
    root.navFocusSeen = false
  }

  function navNextLocal() { root.navStep(1) }
  function navPrevLocal() { root.navStep(-1) }

  // Activate IS a click: same call, same target, same handler. A right click
  // (`navSecondary`) is what reaches the actions that have no IPC verb at all —
  // audio's mute-all, the clock's format cycle.
  function navPress(button) {
    if (!root.navActive || !root.navTarget) return
    if (typeof root.navTarget.triggerPress !== "function") return
    root.navPoke()
    root.navTarget.triggerPress(button)
  }

  function navActivateLocal() { root.navPress(Qt.LeftButton) }
  function navSecondaryLocal() { root.navPress(Qt.RightButton) }

  // Tab inside a panel walks to the neighbouring panel, which moves the bar
  // out from under the ring. Follow it, so B lands the ring where the user
  // actually is.
  function navSyncToPopout() {
    if (!root.navActive || !root.bar) return
    var owner = root.bar.activePopout
    if (!owner) return
    var list = root.navTargets()
    for (var i = 0; i < list.length; i++) {
      var node = list[i]
      for (var guard = 0; node && guard < 12; guard++) {
        if (node === owner) {
          if (list[i] !== root.navTarget) root.navSetTarget(list[i])
          return
        }
        node = node.parent
      }
    }
  }

  // The registry changed under us — a widget hid itself, a workspace closed,
  // a plugin was toggled. Deferred, because this can arrive from the target's
  // own destruction.
  function navRevalidate() {
    if (!root.navActive) return
    Qt.callLater(root.navRevalidateNow)
  }

  function navRevalidateNow() {
    if (!root.navActive) return
    var list = root.navTargets()
    if (list.length === 0) {
      root.navLeaveLocal()
      return
    }
    if (list.indexOf(root.navTarget) === -1) root.navSetTarget(root.navStartTarget(list))
  }

  // Attention. Every step, activate, verb and key press lands here, and the
  // idle clock starts over.
  function navPoke() {
    if (root.navActive) navIdleTimer.restart()
    else navIdleTimer.stop()
  }

  // Losing the keyboard to anything that is not a panel we yielded to means the
  // user is elsewhere — another window, a menu, the lock screen — and the ring
  // has no business holding a mode open on their behalf. The grace is because
  // focus legitimately blinks off for a frame or two: the Exclusive -> OnDemand
  // prime recommits keyboard interactivity, and a closing panel hands focus
  // back through the compositor rather than directly.
  function navFocusWatch() {
    if (root.navSurfaceFocused) {
      root.navFocusSeen = true
      navFocusLossTimer.stop()
      return
    }
    if (!root.navActive || root.navYielded || !root.navFocusSeen) {
      navFocusLossTimer.stop()
      return
    }
    if (!navFocusLossTimer.running) navFocusLossTimer.start()
  }

  // Tear the surface down for good: called when this widget is destroyed, which
  // on a plugin hot-reload is the only thing standing between a reload and a
  // stranded `omarchy-bar-nav` layer pinning `omarchy-ui` forever. Guarded
  // throughout, because children may already be going away underneath us.
  function navShutdown() {
    try {
      root.navActive = false
      if (root.navRing) {
        root.navRing.destroy()
        root.navRing = null
      }
      root.navTarget = null
    } catch (e) {
      // Nothing useful to do while the object graph is being dismantled.
    }
  }

  function navBeginPrime() {
    if (root.navActive && !root.navYielded && navLayer.backingWindowVisible) navPrimeTimer.restart()
  }

  function navTakeFocus() {
    root.navPrimed = false
    root.navBeginPrime()
    Qt.callLater(function() {
      if (root.navActive && !root.navYielded) navKeys.forceActiveFocus()
    })
  }

  // The whole key model, in one place. Everything here arrives from the pad as
  // an ordinary key because hyprpad is in `omarchy-ui` while this surface is
  // mapped; a keyboard user gets the identical behaviour by construction.
  function navKey(event) {
    if (!root.navActive) return
    // Any key that reaches the ring is attention: the surface is only fed keys
    // while it holds focus, so this covers the pad, the keyboard and the ones
    // the ring does not itself act on.
    root.navPoke()
    var key = event.key
    if (key === Qt.Key_Escape || key === Qt.Key_Back) {
      root.navLeaveLocal(); event.accepted = true; return
    }
    if (key === Qt.Key_Return || key === Qt.Key_Enter || key === Qt.Key_Space) {
      root.navActivateLocal(); event.accepted = true; return
    }
    if (key === Qt.Key_Tab) { root.navStep(1); event.accepted = true; return }
    if (key === Qt.Key_Backtab) { root.navStep(-1); event.accepted = true; return }
    if (key === (root.vertical ? Qt.Key_Down : Qt.Key_Right)) {
      root.navStep(1); event.accepted = true; return
    }
    if (key === (root.vertical ? Qt.Key_Up : Qt.Key_Left)) {
      root.navStep(-1); event.accepted = true; return
    }
    if (key === root.navLeaveKey) { root.navLeaveLocal(); event.accepted = true; return }
  }

  onNavActiveChanged: {
    if (root.navActive) {
      root.navFocusSeen = false
      root.navPoke()
      root.navTakeFocus()
    } else {
      navPrimeTimer.stop()
      navIdleTimer.stop()
      navFocusLossTimer.stop()
      root.navPrimed = false
      root.navFocusSeen = false
    }
  }

  // A panel closed and handed the keyboard back — or opened and took it. Both
  // edges move the idle clock and re-arm the focus watch.
  onNavYieldedChanged: {
    if (root.navActive && !root.navYielded) root.navTakeFocus()
    root.navPoke()
    root.navFocusWatch()
  }

  onNavSurfaceFocusedChanged: root.navFocusWatch()

  // The lock is the case that produced this whole section: the ring outlived
  // one, and hyprpad sat in `omarchy-ui` for hours behind it.
  onNavSessionLockedChanged: if (root.navSessionLocked) root.navLeaveLocal()

  // A hot-reloaded plugin is a new widget object; the old one must not leave
  // its layer surface — and therefore hyprpad's mode — behind it.
  Component.onDestruction: root.navShutdown()

  Timer {
    id: navPrimeTimer
    // Long enough for the Wayland commits that follow the map, short enough
    // that compositor-wide pointer routing is imperceptible. KeyboardPanel's
    // number, on purpose — this is the same hazard (omacom/omarchy#9029).
    interval: 75
    onTriggered: if (root.navActive) root.navPrimed = true
  }

  // Exit 1: the idle timeout. Single-shot and restarted by `navPoke`, so the
  // ring lives exactly as long as the user keeps touching it.
  //
  // While a panel is open the window stretches rather than stopping. It has to
  // stretch — the panel owns the keyboard, so the ring sees no keys, and
  // somebody reading a network list is not idle — and it must not stop, because
  // `activePopout` is the bar's flag, not ours, and a ring whose only exit is
  // somebody else's flag clearing is precisely the ring that got stuck. A
  // minute of nothing at all with a panel up is abandoned by any reading.
  Timer {
    id: navIdleTimer
    interval: root.navIdleSec * (root.navYielded ? 5 : 1) * 1000
    onTriggered: if (root.navActive) root.navLeaveLocal()
  }

  // Exit 2: the grace on losing the keyboard. It only has to be shorter than
  // the idle timeout that backs it up, so it is sized for the slowest handback
  // rather than the fastest: a panel closing gives its focus back through the
  // compositor, and the ring then re-primes Exclusive -> OnDemand to take it.
  // Three quarters of a second of extra life for an abandoned ring costs
  // nothing next to stealing the ring back from someone who just pressed B.
  Timer {
    id: navFocusLossTimer
    interval: 750
    onTriggered:
      if (root.navActive && !root.navYielded && !root.navSurfaceFocused) root.navLeaveLocal()
  }

  // Exit 4: nothing left to focus. `onClickTargetsChanged` catches a target
  // registering or going away, but a widget that merely hides itself changes
  // `moduleTargetClickable` without touching the registry — so the ring
  // re-checks its own stops while it is up. One pass over a handful of bar
  // items, once a second, and only during bar mode.
  Timer {
    id: navGuardTimer
    interval: 1000
    repeat: true
    running: root.navActive
    onTriggered: root.navRevalidateNow()
  }

  Connections {
    target: root.bar
    ignoreUnknownSignals: true
    function onActivePopoutChanged() { root.navSyncToPopout() }
    function onClickTargetsChanged() { root.navRevalidate() }
    function onBarHiddenChanged() { if (root.bar && root.bar.barHidden === true) root.navLeaveLocal() }
  }

  Component {
    id: navRingComponent

    Rectangle {
      anchors.fill: parent
      anchors.margins: Style.space(1)
      z: 60
      radius: Math.min(Style.cornerRadius, Math.min(width, height) / 2)
      // The open-panel dot's idiom — Color.accent — grown into an outline, so
      // "focused" and "open" read as the same family without being the same
      // mark.
      color: Qt.rgba(Color.accent.r, Color.accent.g, Color.accent.b, 0.14)
      border.color: Color.accent
      border.width: Math.max(1, Style.space(1))

      OpacityAnimator on opacity {
        from: 0
        to: 1
        duration: 90
      }
    }
  }

  // The announcement. Nothing is drawn here and nothing is clickable: the
  // surface exists so that (1) it holds the keyboard and (2) its NAMESPACE is
  // in `hyprctl layers`, which is how hyprpad learns bar mode is on. Both are
  // load-bearing; the pixel is not.
  PanelWindow {
    id: navLayer

    visible: root.navActive
    screen: root.navBarWindow ? root.navBarWindow.screen : null
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    implicitWidth: 1
    implicitHeight: 1

    anchors {
      top: true
      left: true
    }

    // Empty input region: every pixel of the bar underneath stays clickable
    // with the mouse while the ring is up.
    mask: Region {}

    WlrLayershell.namespace: "omarchy-bar-nav"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: root.navActive && !root.navYielded
      ? (root.navPrimed ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.Exclusive)
      : WlrKeyboardFocus.None

    // The map is when focus can actually be taken, and the prime has to start
    // from there rather than from the state change that asked for it.
    onBackingWindowVisibleChanged: if (backingWindowVisible) root.navTakeFocus()

    // Layer-shell grants focus to the SURFACE; Qt still needs an item inside
    // it holding active focus before Keys.onPressed fires.
    Item {
      id: navKeys
      anchors.fill: parent
      focus: true

      // Whether the compositor is feeding this surface keys. The attached
      // `Window.active` resolves to the layer surface's backing window, and on
      // Wayland a window is active exactly while it holds the keyboard.
      readonly property bool windowActive: Window.active

      Keys.priority: Keys.BeforeItem
      Keys.onPressed: function(event) { root.navKey(event) }
    }
  }

  // --- the glyph -----------------------------------------------------------

  // Kenney's CC0 controller pictogram, vendored under `art/`. It is authored
  // white, so it is recoloured to the bar's foreground and handed to the Image
  // as a data URL — the same trick the cheat sheet uses on its diagram, and the
  // reason the glyph tracks a theme change with no second asset.
  readonly property string pluginDir:
    Qt.resolvedUrl(".").toString().replace("file://", "").replace(/\/$/, "")
  readonly property color foregroundColor:
    root.bar ? root.bar.barForeground : Color.foreground
  readonly property real glyphSize: Style.bar.iconCanvas

  property string glyphSvg: ""

  function recolor(svg, color) {
    if (!svg) return ""
    return svg.replace(/#FFFFFF/gi, color).replace(/"white"/gi, '"' + color + '"')
  }

  readonly property string glyphSource: root.glyphSvg
    ? "data:image/svg+xml;utf8,"
      + encodeURIComponent(root.recolor(root.glyphSvg, String(root.foregroundColor)))
    : ""

  FileView {
    path: root.pluginDir + "/art/kenney/controller_icon.svg"
    printErrors: false
    onLoaded: root.glyphSvg = text()
    onLoadFailed: root.glyphSvg = ""
  }

  // --- the bar item --------------------------------------------------------

  // A vertical bar has no room for a word beside the glyph, so it goes
  // glyph-only — unless the glyph is what failed, in which case the label is
  // all that stands between the reader and an empty slot.
  readonly property bool showLabel:
    (root.setting("showMode", true) && !root.vertical) || !glyphImage.visible

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    // The label is drawn below rather than by the button, which paints text
    // only; everything else the button brings — hover, tooltip, click
    // registration, the theme's colour animation — is why it is still here.
    labelVisible: false
    hasVisualContent: true
    tooltipText: root.controller + " — " + root.modeLabel
      + " (click for the cheat sheet, right-click to turn the controller off)"
    fixedWidth: root.vertical
      ? -1 : Math.max(12, content.implicitWidth + button.scaledHorizontalMargin * 2)
    fixedHeight: root.vertical
      ? Math.max(12, content.implicitHeight + button.scaledVerticalPadding * 2) : -1
    // `WidgetButton` emits `pressed(int button)` and its MouseArea accepts all
    // three buttons, so the widget gets a second, deliberate gesture for free:
    // left click still opens the cheat sheet, right click turns the controller
    // off. Not a confirmation menu — choosing the right button IS the
    // deliberation (docs/research/guide-hold-poweroff.md §3.B).
    onPressed: function(button) {
      if (button === Qt.RightButton) root.turnControllerOff()
      else root.openCheatSheet()
    }

    Row {
      id: content
      anchors.centerIn: parent
      spacing: Style.space(4)

      Image {
        id: glyphImage
        anchors.verticalCenter: parent.verticalCenter
        width: root.glyphSize
        height: root.glyphSize
        fillMode: Image.PreserveAspectFit
        smooth: true
        // Oversampled, so the SVG stays crisp on a scaled bar.
        sourceSize.width: root.glyphSize * 3
        sourceSize.height: root.glyphSize * 3
        source: root.glyphSource
        visible: status === Image.Ready
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        visible: root.showLabel
        text: root.modeLabel
        color: button.foreground
        font.family: button.fontFamily
        font.pixelSize: Style.font.caption
        renderType: Text.NativeRendering

        Behavior on color {
          enabled: !root.bar || root.bar.foregroundAnimationEnabled
          ColorAnimation { duration: 160 }
        }
      }
    }
  }

  // --- the click -----------------------------------------------------------

  // Open the cheat sheet on the mode the pad is in *now*. It has to arrive as
  // `HYPRPAD_MODE`, because summoning the sheet is itself a mode change — the
  // overlay puts the pad in `cheatsheet` — so by the time the sheet is up, the
  // context the reader wanted is gone. `hyprpad-cheatsheet` reads that variable
  // and forwards it in the summon payload; going through the script rather than
  // spelling the IPC here is what keeps this widget correct if that spelling
  // ever moves.
  //
  // `execArgv` runs the argv without a shell re-tokenizing it, and `env`
  // carries the variable, so a mode name can hold anything and still land as
  // one literal argument.
  function openCheatSheet() {
    Util.execArgv(["env", "HYPRPAD_MODE=" + root.mode, "hyprpad-cheatsheet", "toggle"])
  }

  // Turn the controller off on a right-click. `hyprpad off` nudges the running
  // daemon (SIGUSR1) rather than touching the device itself, so the widget
  // needs no access to the puck and there is still exactly one writer. The
  // widget then disappears on its own, because the daemon publishes
  // `connected: false` the moment the stream stops.
  function turnControllerOff() {
    Util.execArgv(["hyprpad", "off"])
  }
}
