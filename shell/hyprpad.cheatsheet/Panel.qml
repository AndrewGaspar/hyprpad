// hyprpad cheat sheet — an Omarchy shell plugin of kind `panel`.
//
// Summoned with `omarchy-shell shell toggle hyprpad.cheatsheet '{}'` (which is
// what `hyprpad-cheatsheet toggle` runs, which is what the guide chord binds
// to). Omarchy's generic plugin dispatch calls `open()` / `close()` / `toggle()`
// on this root Item, so no IpcHandler of our own is needed for those.
//
// Everything the sheet draws is re-read on every summon: the bindings come from
// `hyprpad bindings --json`, which loads the config exactly as the daemon does.
// Edit the config, summon again, and the sheet is current — no reload of the
// shell, no restart of the daemon.
//
// The layer is deliberately NOT keyboard-exclusive. A cheat sheet is something
// you read while you keep working, and hyprpad's on-screen keyboard types
// through a virtual keyboard into whatever the compositor has focused — taking
// an exclusive grab here would swallow those keystrokes. Escape still closes
// the sheet when it is clicked, and the chord that summoned it always closes
// it, which is the dismissal that actually matters on a controller.

import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import "Callouts.js" as Callouts

Item {
  id: root

  // Injected by Omarchy's plugin loader.
  property var shell: null
  property var manifest: null
  property string omarchyPath: Quickshell.env("OMARCHY_PATH")

  readonly property string pluginDir:
    (manifest && manifest.__sourceDir)
      ? String(manifest.__sourceDir).replace(/\/$/, "")
      : Qt.resolvedUrl(".").toString().replace("file://", "").replace(/\/$/, "")

  readonly property string steamRoot: {
    var r = Quickshell.env("STEAM_ROOT")
    if (r) return String(r).replace(/\/$/, "")
    return Quickshell.env("HOME") + "/.local/share/Steam"
  }

  property bool opened: false
  property string layoutId: "steam-controller-2026"
  property string toggleChord: "guide+view"

  property var sheetData: null
  property var layoutData: null
  property string artSvg: ""
  property string error: ""

  // --- summon / dismiss ----------------------------------------------------

  function open(payloadJson) {
    var payload = {}
    try { payload = JSON.parse(payloadJson || "{}") || {} } catch (e) {}
    // A payload may pin a different controller layout:
    //   omarchy-shell shell toggle hyprpad.cheatsheet '{"layout":"steam-deck"}'
    if (payload.layout) root.layoutId = String(payload.layout)
    if (payload.chord) root.toggleChord = String(payload.chord)
    root.error = ""
    reload()
    root.opened = true
  }

  function close() {
    root.opened = false
    if (bindingsProc.running) bindingsProc.running = false
  }

  function toggle() {
    if (root.opened) dismiss()
    else open("{}")
  }

  // Route dismissal back through the shell so its own idea of what is open
  // stays in step with ours — otherwise the next `toggle` would be inverted.
  function dismiss() {
    var id = (root.manifest && root.manifest.id) || "hyprpad.cheatsheet"
    if (root.shell && typeof root.shell.hide === "function") root.shell.hide(id)
    else close()
  }

  function reload() {
    layoutFile.path = root.pluginDir + "/layouts/" + root.layoutId + ".json"
    layoutFile.reload()
    if (bindingsProc.running) bindingsProc.running = false
    bindingsProc.running = true
  }

  // --- data ----------------------------------------------------------------

  // Where to find the daemon binary. `hyprpad` on PATH is the normal answer;
  // the fallbacks cover a `cargo install` and a hand-placed symlink, and
  // $HYPRPAD_BIN covers running a build straight out of a checkout. Resolving
  // in the shell rather than in QML keeps the whole search in one greppable
  // string and gives a real error message when nothing matches.
  readonly property string resolveHyprpad:
      'if [ -n "$HYPRPAD_BIN" ] && [ -x "$HYPRPAD_BIN" ]; then exec "$HYPRPAD_BIN" bindings --json; fi; '
    + 'if command -v hyprpad >/dev/null 2>&1; then exec hyprpad bindings --json; fi; '
    + 'for c in "$HOME/.local/bin/hyprpad" "$HOME/.cargo/bin/hyprpad"; do '
    + '  [ -x "$c" ] && exec "$c" bindings --json; '
    + 'done; '
    + 'echo "hyprpad not found — put it on PATH, or set HYPRPAD_BIN to the binary" >&2; '
    + 'exit 127'

  // The single source of truth for what is bound. Re-run on every summon.
  Process {
    id: bindingsProc
    command: ["sh", "-c", root.resolveHyprpad]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          root.sheetData = JSON.parse(text)
          root.error = ""
        } catch (e) {
          root.sheetData = null
          root.error = "could not parse `hyprpad bindings --json`: " + e
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var t = String(text || "").trim()
        if (t !== "" && !root.sheetData) root.error = t
      }
    }
    onExited: function (exitCode) {
      if (exitCode !== 0 && !root.sheetData && root.error === "")
        root.error = "`hyprpad bindings --json` exited " + exitCode
          + " — is hyprpad on PATH?"
    }
  }

  FileView {
    id: layoutFile
    onLoaded: {
      try {
        root.layoutData = JSON.parse(text())
        pickArt()
      } catch (e) {
        root.error = "layout " + root.layoutId + " is not valid JSON: " + e
      }
    }
    onLoadFailed: root.error = "no layout " + root.layoutId
      + ".json in " + root.pluginDir + "/layouts"
  }

  // Walk the layout's `art` list in order and keep the first drawing that
  // exists. For the puck that means Valve's own diagram when the local Steam
  // install has it, and hyprpad's bundled schematic otherwise — the official
  // art is READ from the install, never copied into the repo.
  property int artIndex: 0
  function pickArt() {
    root.artIndex = 0
    root.artSvg = ""
    tryArt()
  }
  function tryArt() {
    if (!root.layoutData || !root.layoutData.art
        || root.artIndex >= root.layoutData.art.length) return
    var a = root.layoutData.art[root.artIndex]
    artFile.path = (a.source === "steam" ? root.steamRoot : root.pluginDir)
      + "/" + a.path
    artFile.reload()
  }

  FileView {
    id: artFile
    onLoaded: {
      var svg = text()
      if (svg && svg.indexOf("<svg") >= 0) {
        root.artSvg = Callouts.recolor(svg, String(Color.foreground))
      } else {
        root.artIndex++
        root.tryArt()
      }
    }
    onLoadFailed: {
      root.artIndex++
      root.tryArt()
    }
  }

  // --- the surface ---------------------------------------------------------

  PanelWindow {
    visible: root.opened
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "hyprpad-cheatsheet"
    WlrLayershell.layer: WlrLayer.Overlay
    // OnDemand, not Exclusive: see the note at the top of this file.
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.OnDemand

    Rectangle {
      anchors.fill: parent
      color: Qt.rgba(0, 0, 0, 0.55)
      MouseArea {
        anchors.fill: parent
        onClicked: root.dismiss()
      }
    }

    Item {
      id: keyCatcher
      anchors.fill: parent
      focus: true
      Keys.onEscapePressed: root.dismiss()

      Item {
        anchors.centerIn: parent
        width: card.implicitWidth
        height: card.implicitHeight
        // Never let the card run off a small or heavily scaled output.
        scale: Math.min(1,
          (keyCatcher.width - 48) / Math.max(1, width),
          (keyCatcher.height - 48) / Math.max(1, height))

        // Clicks on the sheet itself must not dismiss it.
        MouseArea { anchors.fill: parent; onClicked: {} }

        Sheet {
          id: card
          anchors.fill: parent
          visible: root.sheetData !== null && root.layoutData !== null
          sheet: root.sheetData
          layout: root.layoutData
          pluginDir: root.pluginDir
          artSvg: root.artSvg
          toggleChord: root.toggleChord

          palBackground: Color.popups.background
          palText: Color.popups.text
          palBorder: Color.popups.border
          palAccent: Color.accent
          palMuted: Color.muted
          fontFamily: Style.font.family
          monoFamily: Style.font.familyMono !== undefined
            ? Style.font.familyMono : "monospace"
          fontCaption: Style.font.caption
          fontSmall: Style.font.bodySmall
          fontBody: Style.font.body
          fontTitle: Style.font.title
          fontHeading: Style.font.heading
        }
      }

      // Loading / failure state, in place of the card.
      Rectangle {
        anchors.centerIn: parent
        visible: root.error !== "" || !root.sheetData
        width: Math.min(520, parent.width - 96)
        height: 96
        radius: 10
        color: Color.popups.background
        border.color: root.error !== "" ? Color.urgent : Color.popups.border

        Text {
          anchors.centerIn: parent
          anchors.margins: 16
          width: parent.width - 32
          textFormat: Text.PlainText
          horizontalAlignment: Text.AlignHCenter
          wrapMode: Text.Wrap
          text: root.error !== "" ? root.error : "Reading bindings…"
          color: root.error !== "" ? Color.urgent : Color.popups.text
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
        }
      }
    }
  }
}
