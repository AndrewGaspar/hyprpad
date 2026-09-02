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
    tooltipText: root.controller + " — " + root.modeLabel + " (click for the cheat sheet)"
    fixedWidth: root.vertical
      ? -1 : Math.max(12, content.implicitWidth + button.scaledHorizontalMargin * 2)
    fixedHeight: root.vertical
      ? Math.max(12, content.implicitHeight + button.scaledVerticalPadding * 2) : -1
    onPressed: root.openCheatSheet()

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
}
