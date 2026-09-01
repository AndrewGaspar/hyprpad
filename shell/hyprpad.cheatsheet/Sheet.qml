// The cheat sheet itself: a controller diagram with a callout per guide chord,
// plus compact tables for the bare buttons, the OSK helpers and the modes.
//
// Deliberately PURE QtQuick — no Quickshell, no `qs.Commons`. Everything it
// needs comes in through properties:
//
//   * `sheet`     — the parsed `hyprpad bindings --json` document
//   * `layout`    — the parsed layouts/<controller>.json descriptor
//   * `pluginDir` — where to resolve the layout's bundled art from
//   * `artSvg`    — the diagram's SVG text, already recoloured
//   * the `pal*` / `font*` palette knobs
//
// which is what would let it be dropped into a plain `quickshell -p` config,
// or rendered by a test harness, without dragging Omarchy in behind it.
// `Panel.qml` is the only file here that knows Omarchy exists.

import QtQuick
import QtQuick.Layouts
import QtQuick.Shapes
import "Callouts.js" as Callouts

Item {
  id: root

  // --- data ---------------------------------------------------------------
  property var sheet: null            // hyprpad bindings --json
  property var layout: null           // layouts/*.json
  property string pluginDir: ""
  property string artSvg: ""          // recoloured diagram SVG source text
  property string toggleChord: ""     // the chord that summons this sheet

  // --- palette ------------------------------------------------------------
  property color palBackground: "#0B1418"
  property color palText: "#D9CDB4"
  property color palMuted: "#7d8a91"
  property color palAccent: "#D4A24C"
  property color palBorder: "#D4A24C"
  property string fontFamily: "sans-serif"
  property string monoFamily: "monospace"
  property int fontCaption: 10
  property int fontSmall: 11
  property int fontBody: 12
  property int fontTitle: 14
  property int fontHeading: 16

  // --- geometry -----------------------------------------------------------
  readonly property int diagramWidth: 520
  readonly property int diagramHeight: layout && layout.viewBox
    ? Math.round(diagramWidth * layout.viewBox.height / layout.viewBox.width)
    : 365
  readonly property int columnWidth: 330
  readonly property int columnGap: 26
  readonly property int calloutGap: 8

  implicitWidth: content.implicitWidth + 2 * pad
  implicitHeight: content.implicitHeight + 2 * pad
  readonly property int pad: 28

  // --- placement ----------------------------------------------------------
  //
  // Which sheet sections get a callout on the drawing. The guide layer is what
  // the sheet is FOR, and the trackpads are the two biggest controls on the
  // puck — leaving them unlabelled would be a lie. Bare buttons and OSK
  // helpers go in the tables instead: they would double the leader count for
  // controls the diagram already names.
  readonly property var diagramSections: ["guide_chords", "ambient"]

  property var leftPlaced: []
  property var rightPlaced: []
  property var overflow: []

  function calloutHeight(item) {
    // Deterministic, so placement can run before the delegates exist and no
    // binding loop is possible: a title row plus one row per binding.
    return 20 + item.lines.length * 19 + 10
  }

  function relayout() {
    if (!sheet || !layout) {
      leftPlaced = []; rightPlaced = []; overflow = []
      return
    }
    var groups = Callouts.group(sheet, layout, diagramSections)
    overflow = Callouts.unplaced(sheet, layout, diagramSections)

    var vb = layout.viewBox
    var scale = diagramWidth / vb.width
    var top = diagramTop()

    var left = [], right = []
    for (var i = 0; i < groups.length; i++) {
      var g = groups[i]
      // Anchor in diagram-row pixel space.
      g.px = diagramX() + g.ax * scale
      g.py = top + g.ay * scale
      g.h = calloutHeight(g)
      ;(g.side === "left" ? left : right).push(g)
    }
    left.sort(Callouts.byAnchor)
    right.sort(Callouts.byAnchor)

    var rowH = diagramRowHeight()
    leftPlaced = Callouts.place(left, left.map(function (g) { return g.h }),
                                0, rowH, calloutGap)
    rightPlaced = Callouts.place(right, right.map(function (g) { return g.h }),
                                 0, rowH, calloutGap)
  }

  // The diagram row is as tall as the taller of (the drawing, either column),
  // so a right-heavy config — which the puck's face cluster makes the norm —
  // simply grows the card rather than cramming its callouts.
  function columnExtent(side) {
    if (!sheet || !layout) return 0
    var groups = Callouts.group(sheet, layout, diagramSections)
    var n = 0, h = 0
    for (var i = 0; i < groups.length; i++) {
      var anchor = layout.controls[groups[i].control]
      if ((anchor.side === "left" ? "left" : "right") !== side) continue
      h += calloutHeight(groups[i]); n++
    }
    return n > 0 ? h + (n - 1) * calloutGap : 0
  }

  function diagramRowHeight() {
    return Math.max(diagramHeight, columnExtent("left"), columnExtent("right"))
  }

  function diagramTop() { return (diagramRowHeight() - diagramHeight) / 2 }
  function diagramX() { return columnWidth + columnGap }

  onSheetChanged: relayout()
  onLayoutChanged: relayout()

  // --- chrome -------------------------------------------------------------

  Rectangle {
    anchors.fill: parent
    radius: 10
    color: root.palBackground
    border.color: root.palBorder
    border.width: 1
  }

  ColumnLayout {
    id: content
    anchors.fill: parent
    anchors.margins: root.pad
    spacing: 18

    // ------------------------------------------------------------- header
    RowLayout {
      Layout.fillWidth: true
      spacing: 12

      ColumnLayout {
        spacing: 2
        Text {
          text: "hyprpad · controller bindings"
          color: root.palText
          font.family: root.fontFamily
          font.pixelSize: root.fontHeading
          font.bold: true
        }
        Text {
          textFormat: Text.PlainText
          text: {
            if (!root.sheet) return "loading…"
            var src = root.sheet.source
            return src ? (src.path + "  (" + src.format + ")")
                       : "built-in defaults (no config file)"
          }
          color: root.palMuted
          font.family: root.monoFamily
          font.pixelSize: root.fontCaption
        }
      }

      Item { Layout.fillWidth: true }

      ColumnLayout {
        spacing: 2
        Text {
          text: root.layout ? root.layout.name : ""
          color: root.palAccent
          font.family: root.fontFamily
          font.pixelSize: root.fontTitle
          font.bold: true
          Layout.alignment: Qt.AlignRight
        }
        Text {
          text: root.layout && root.layout.subtitle ? root.layout.subtitle : ""
          color: root.palMuted
          font.family: root.fontFamily
          font.pixelSize: root.fontCaption
          Layout.alignment: Qt.AlignRight
        }
      }
    }

    Rectangle {
      Layout.fillWidth: true
      height: 1
      color: root.palMuted
      opacity: 0.35
    }

    // ------------------------------------------------- diagram + callouts
    Item {
      id: diagramRow
      Layout.alignment: Qt.AlignHCenter
      implicitWidth: 2 * root.columnWidth + 2 * root.columnGap + root.diagramWidth
      implicitHeight: root.diagramRowHeight()

      // The drawing. `artSvg` arrives already recoloured, as a data: URL —
      // Qt renders SVG from one happily, and it keeps the themed copy out of
      // the filesystem.
      Image {
        x: root.diagramX()
        y: root.diagramTop()
        width: root.diagramWidth
        height: root.diagramHeight
        fillMode: Image.PreserveAspectFit
        smooth: true
        // Rasterize at 2x so the thin strokes stay crisp.
        sourceSize.width: root.diagramWidth * 2
        sourceSize.height: root.diagramHeight * 2
        source: root.artSvg
          ? "data:image/svg+xml;utf8," + encodeURIComponent(root.artSvg)
          : ""
      }

      // Leader lines, under the callout chips.
      Repeater {
        model: root.leftPlaced.concat(root.rightPlaced)
        delegate: Shape {
          id: leader
          required property var modelData
          anchors.fill: parent
          preferredRendererType: Shape.CurveRenderer
          z: 0

          readonly property bool isLeft: modelData.side === "left"
          // Where the leader meets the callout: its inner edge, at its centre.
          readonly property real endX: isLeft
            ? root.columnWidth
            : root.columnWidth + root.columnGap + root.diagramWidth + root.columnGap
          readonly property real endY: modelData.cy
          // A short horizontal run into the label. The column is always taller
          // than the span of the anchors it serves — ten callouts do not fit in
          // the height of one face-button cluster — so the leaders necessarily
          // fan out; the lead-in is what makes it obvious, at a glance, which
          // line belongs to which label.
          // The anchor is always on the diagram, i.e. INSIDE `endX`: to the
          // right of a left-hand label, to the left of a right-hand one.
          readonly property real leadX: isLeft ? endX + 22 : endX - 22

          ShapePath {
            strokeColor: root.palAccent
            strokeWidth: 1
            fillColor: "transparent"
            // A control the front view cannot actually show (a trigger, a
            // grip) gets a dashed leader, so the sheet never implies you can
            // see it in the drawing.
            strokeStyle: leader.modelData.hidden ? ShapePath.DashLine : ShapePath.SolidLine
            dashPattern: [3, 3]
            startX: leader.modelData.px
            startY: leader.modelData.py
            PathLine { x: leader.leadX; y: leader.endY }
            PathLine { x: leader.endX; y: leader.endY }
          }
        }
      }

      // Anchor dots, on top of the drawing.
      Repeater {
        model: root.leftPlaced.concat(root.rightPlaced)
        delegate: Rectangle {
          required property var modelData
          z: 1
          width: 6; height: 6; radius: 3
          x: modelData.px - 3
          y: modelData.py - 3
          color: root.palAccent
        }
      }

      // The callouts themselves.
      Repeater {
        model: root.leftPlaced
        delegate: calloutComponent
      }
      Repeater {
        model: root.rightPlaced
        delegate: calloutComponent
      }
    }

    Rectangle {
      Layout.fillWidth: true
      height: 1
      color: root.palMuted
      opacity: 0.35
    }

    // ------------------------------------------------------------- tables
    RowLayout {
      Layout.fillWidth: true
      spacing: 24

      TableBlock {
        title: "Bare buttons"
        subtitle: "no modifier"
        rows: root.sheet ? root.sheet.buttons.concat(root.overflow) : []
        Layout.alignment: Qt.AlignTop
      }
      TableBlock {
        title: "OSK helpers"
        subtitle: "while the keyboard is up"
        rows: root.sheet ? root.sheet.osk_buttons : []
        Layout.alignment: Qt.AlignTop
      }
      ModesBlock {
        modes: root.sheet ? root.sheet.modes : []
        total: root.sheet
          ? root.sheet.guide_chords.length + root.sheet.buttons.length
            + root.sheet.osk_buttons.length + root.sheet.ambient.length
          : 0
        Layout.alignment: Qt.AlignTop
        Layout.fillWidth: true
      }
    }

    // ------------------------------------------------------------- footer
    Text {
      Layout.fillWidth: true
      textFormat: Text.PlainText
      text: {
        var bits = ["Hold Steam, then press."]
        if (root.toggleChord !== "") bits.push(root.toggleChord + " or Esc closes this sheet.")
        else bits.push("Esc closes this sheet.")
        bits.push("Re-read on every summon — edit the config and summon again.")
        return bits.join("  ")
      }
      color: root.palMuted
      font.family: root.fontFamily
      font.pixelSize: root.fontCaption
    }
  }

  // --- one callout --------------------------------------------------------
  Component {
    id: calloutComponent

    Item {
      required property var modelData
      z: 2
      readonly property bool isLeft: modelData.side === "left"
      width: root.columnWidth
      height: modelData.h
      x: isLeft ? 0 : root.columnWidth + 2 * root.columnGap + root.diagramWidth
      y: modelData.cy - modelData.h / 2

      Column {
        anchors.fill: parent
        anchors.leftMargin: 6
        anchors.rightMargin: 6
        spacing: 2

        // Title row: the glyph chip and the control's name.
        Row {
          spacing: 6
          layoutDirection: isLeft ? Qt.RightToLeft : Qt.LeftToRight
          anchors.right: isLeft ? parent.right : undefined

          Item {
            width: 18; height: 18
            Image {
              id: glyphImage
              anchors.fill: parent
              fillMode: Image.PreserveAspectFit
              smooth: true
              sourceSize.width: 36
              sourceSize.height: 36
              source: {
                var g = root.layout && root.layout.glyphs
                  ? root.layout.glyphs[modelData.control] : null
                return (g && g.image && root.pluginDir)
                  ? "file://" + root.pluginDir + "/" + g.image : ""
              }
              visible: status === Image.Ready
            }
            // The layout's `text` fallback, for a glyph file that is missing —
            // which is the normal case for a layout that ships no art.
            Text {
              anchors.centerIn: parent
              visible: !glyphImage.visible
              text: {
                var g = root.layout && root.layout.glyphs
                  ? root.layout.glyphs[modelData.control] : null
                return g && g.text ? g.text : modelData.control
              }
              color: root.palAccent
              font.family: root.fontFamily
              font.pixelSize: root.fontCaption
              font.bold: true
            }
          }

          Text {
            text: modelData.controlLabel
            color: root.palAccent
            font.family: root.fontFamily
            font.pixelSize: root.fontSmall
            font.bold: true
          }
        }

        // One row per binding on this control.
        Repeater {
          model: modelData.lines
          delegate: Row {
            required property var modelData
            spacing: 6
            anchors.right: isLeft ? parent.right : undefined
            layoutDirection: isLeft ? Qt.RightToLeft : Qt.LeftToRight

            Text {
              text: modelData.direction !== ""
                ? ({ up: "↑", down: "↓", left: "←", right: "→" }[modelData.direction] || "")
                : ""
              visible: text !== ""
              color: root.palText
              font.family: root.fontFamily
              font.pixelSize: root.fontBody
            }
            Text {
              text: modelData.label
              color: root.palText
              font.family: root.fontFamily
              font.pixelSize: root.fontBody
              // A label the config actually wrote reads as authored; a derived
              // one is dimmed, so the owner can see at a glance which bindings
              // still want a description.
              opacity: modelData.described ? 1.0 : 0.75
              font.italic: !modelData.described
            }
            Text {
              text: modelData.guarded ? "· " + modelData.guard : ""
              visible: text !== ""
              color: root.palMuted
              font.family: root.fontFamily
              font.pixelSize: root.fontCaption
            }
          }
        }
      }
    }
  }

  // --- a small two-column table -------------------------------------------
  component TableBlock: ColumnLayout {
    id: block
    property string title: ""
    property string subtitle: ""
    property var rows: []
    spacing: 4

    Text {
      text: block.title
      color: root.palAccent
      font.family: root.fontFamily
      font.pixelSize: root.fontSmall
      font.bold: true
    }
    Text {
      text: block.subtitle
      visible: text !== ""
      color: root.palMuted
      font.family: root.fontFamily
      font.pixelSize: root.fontCaption
    }
    Repeater {
      model: block.rows
      delegate: RowLayout {
        required property var modelData
        spacing: 8
        Text {
          text: modelData.chord
          color: root.palText
          font.family: root.monoFamily
          font.pixelSize: root.fontCaption
          Layout.minimumWidth: 74
        }
        Text {
          text: modelData.label
          color: root.palText
          font.family: root.fontFamily
          font.pixelSize: root.fontCaption
          opacity: modelData.described ? 1.0 : 0.8
        }
        Text {
          text: (modelData.guard && modelData.guard.kind !== "always")
            ? "· " + modelData.guard_label : ""
          visible: text !== ""
          color: root.palMuted
          font.family: root.fontFamily
          font.pixelSize: root.fontCaption
        }
      }
    }
    Text {
      visible: block.rows.length === 0
      text: "(none)"
      color: root.palMuted
      font.family: root.fontFamily
      font.pixelSize: root.fontCaption
    }
  }

  // --- the declared modes --------------------------------------------------
  component ModesBlock: ColumnLayout {
    id: modesBlock
    property var modes: []
    property int total: 0
    spacing: 4

    Text {
      text: "Modes"
      color: root.palAccent
      font.family: root.fontFamily
      font.pixelSize: root.fontSmall
      font.bold: true
    }
    Text {
      text: "rules in order, first match wins"
      color: root.palMuted
      font.family: root.fontFamily
      font.pixelSize: root.fontCaption
    }
    Repeater {
      model: modesBlock.modes
      delegate: RowLayout {
        required property var modelData
        spacing: 8
        Text {
          text: modelData.name
          color: root.palText
          font.family: root.monoFamily
          font.pixelSize: root.fontCaption
          font.bold: modelData.default
          Layout.minimumWidth: 74
        }
        Text {
          textFormat: Text.PlainText
          text: {
            var bits = []
            if (modelData.default) bits.push("default")
            bits.push(modelData.has_rule ? "rule" : "no rule")
            if (modelData.forward) bits.push("forwards raw input")
            return bits.join(", ")
          }
          color: root.palMuted
          font.family: root.fontFamily
          font.pixelSize: root.fontCaption
        }
        Text {
          text: modelData.active.length + "/" + modesBlock.total + " live"
          color: root.palText
          font.family: root.fontFamily
          font.pixelSize: root.fontCaption
        }
      }
    }
  }
}
