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

    // A string, not a bool: every IPC verb this shell ships returns one, and
    // `qs ipc` prints whatever comes back — `[[ $(...) == true ]]` in a script.
    function navIsActive(): string {
      return root.navAnyActive() ? "true" : "false"
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
  }

  function navLeaveLocal() {
    if (!root.navActive && !root.navRing) return
    root.navActive = false
    root.navSetTarget(null)
  }

  function navNextLocal() { root.navStep(1) }
  function navPrevLocal() { root.navStep(-1) }

  // Activate IS a click: same call, same target, same handler. A right click
  // (`navSecondary`) is what reaches the actions that have no IPC verb at all —
  // audio's mute-all, the clock's format cycle.
  function navPress(button) {
    if (!root.navActive || !root.navTarget) return
    if (typeof root.navTarget.triggerPress !== "function") return
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
    if (root.navActive) root.navTakeFocus()
    else {
      navPrimeTimer.stop()
      root.navPrimed = false
    }
  }

  // A panel closed and handed the keyboard back.
  onNavYieldedChanged: if (root.navActive && !root.navYielded) root.navTakeFocus()

  Timer {
    id: navPrimeTimer
    // Long enough for the Wayland commits that follow the map, short enough
    // that compositor-wide pointer routing is imperceptible. KeyboardPanel's
    // number, on purpose — this is the same hazard (omacom/omarchy#9029).
    interval: 75
    onTriggered: if (root.navActive) root.navPrimed = true
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
