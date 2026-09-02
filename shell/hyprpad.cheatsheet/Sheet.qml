// The cheat sheet itself: a controller diagram with one callout per physical
// control, each stacking every binding that lands on that control, plus the
// declared modes and a legend underneath.
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
//
// # The callout model
//
// A callout is a control, not a binding family. A row inside it is one
// binding, and it opens with the CHORD that fires it — the button's own glyph,
// with a modifier glyph and a "+" in front when something has to be held first
// (the Steam button, the on-screen keyboard) — so every row reads as what you
// press. Then the direction a flick wants, the label, and a dim tag for a
// binding that only lives in some modes. That is the whole of it — there is no
// table underneath the drawing that the reader has to join back to a button by
// eye, which is what the first version got wrong.

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

  // The layout's two art maps — one entry per control, one per modifier — and
  // how a delegate draws an entry from either: the file the layout points at,
  // or its `text` fallback for a layout that ships no art at all.
  readonly property var glyphs: (layout && layout.glyphs) ? layout.glyphs : ({})
  readonly property var modifiers: (layout && layout.modifiers) ? layout.modifiers : ({})

  function artSource(spec) {
    return (spec && spec.image && pluginDir)
      ? "file://" + pluginDir + "/" + spec.image : ""
  }
  function artText(spec, fallback) {
    return (spec && spec.text) ? spec.text : fallback
  }

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
  readonly property int pad: 28
  readonly property int diagramWidth: 520
  readonly property int diagramHeight: layout && layout.viewBox
    ? Math.round(diagramWidth * layout.viewBox.height / layout.viewBox.width)
    : 365
  readonly property int columnGap: 26     // drawing to nearest lane
  readonly property int laneGap: 14       // lane to lane
  readonly property int calloutGap: 12    // box to box down a lane, and the width of a leader channel
  readonly property int maxLanes: 2       // a third lane is a spreadsheet

  // Inside a box.
  readonly property int glyphSize: 18     // the control's chip, in the header
  readonly property int rowGlyph: 14      // either half of a row's chord
  readonly property int arrowW: 13        // the direction column
  readonly property int cellGap: 6        // column to column across a row
  readonly property int chordGap: 4       // inside a chord, around the "+"
  readonly property int boxPadH: 8
  readonly property int boxPadTop: 5
  readonly property int boxPadBottom: 7
  readonly property int headerH: fontSmall + 9
  readonly property int rowH: fontBody + 7

  implicitWidth: content.implicitWidth + 2 * pad
  implicitHeight: content.implicitHeight + 2 * pad

  // --- placement ----------------------------------------------------------
  //
  // Everything below is computed once, in `relayout()`, and parked in these
  // properties: the delegates read them and never compute, so no binding can
  // loop back into the arithmetic.

  property var placed: []             // every callout, with box geometry
  property var overflow: []           // bindings this layout cannot anchor
  property var modifierKeys: []       // which modifiers the legend explains
  property int leftWidth: 0
  property int rightWidth: 0
  property int rowHeight: 0
  property int diagramX: 0
  property int diagramTop: 0

  function measure(tm, s) {
    tm.text = String(s === undefined || s === null ? "" : s)
    return tm.advanceWidth
  }

  // One row's chord: the button it lands on, behind the modifier it is held
  // under and the "+" that joins them.
  function chordWidth(r) {
    return r.modifier === ""
      ? rowGlyph
      : rowGlyph + 2 * chordGap + measure(mPlus, "+") + rowGlyph
  }

  // The chord column is reserved at the widest chord in the BOX — a bare row
  // in a box that also has a held one still starts its label in the same
  // place, and the chords right-align into it, so the buttons stack in one
  // column with the modifiers hanging off to their left.
  function boxChordWidth(c) {
    var w = 0
    for (var i = 0; i < c.rows.length; i++) {
      var cw = chordWidth(c.rows[i])
      if (cw > w) w = cw
    }
    return Math.ceil(w)
  }

  // A box is exactly as wide as its widest line, so nothing is ever clipped
  // and a lane of terse labels does not reserve room it will not use.
  function boxWidth(c) {
    var w = glyphSize + cellGap + measure(mHeader, c.controlLabel)
    for (var i = 0; i < c.rows.length; i++) {
      var r = c.rows[i]
      // The chord and direction columns are reserved for the whole box, not
      // per row, so the labels in one callout line up under each other.
      var rw = c.chordW + cellGap
      if (c.hasDirection) rw += arrowW + cellGap
      rw += measure(r.described ? mRow : mRowItalic, r.label)
      if (r.guarded) rw += cellGap + measure(mTag, "· " + r.guard)
      if (rw > w) w = rw
    }
    return Math.ceil(w) + 2 * boxPadH
  }

  function relayout() {
    if (!sheet || !layout || !mHeader) {
      placed = []; overflow = []; modifierKeys = []
      leftWidth = 0; rightWidth = 0; rowHeight = 0
      return
    }

    var list = Callouts.callouts(sheet, layout)
    overflow = Callouts.unplaced(sheet, layout)
    modifierKeys = Callouts.usedModifiers(list)

    var left = [], right = []
    for (var i = 0; i < list.length; i++) {
      var c = list[i]
      c.h = boxPadTop + headerH + c.rows.length * rowH + boxPadBottom
      // Where the leader lands: on the header, which is the line that names
      // the control. Pointing at the middle of a five-row box points at a
      // binding rather than at the thing bound.
      c.link = boxPadTop + headerH / 2
      c.chordW = boxChordWidth(c)
      c.w = boxWidth(c)
      ;(c.side === "left" ? left : right).push(c)
    }

    var lanesLeft = laneSplit(left, "left")
    var lanesRight = laneSplit(right, "right")

    leftWidth = sideWidth(lanesLeft)
    rightWidth = sideWidth(lanesRight)
    diagramX = leftWidth + columnGap

    // The drawing's height is the target; a lane that still will not fit sets
    // the row height itself rather than being crammed.
    var need = diagramHeight
    var all = lanesLeft.concat(lanesRight)
    for (var l = 0; l < all.length; l++)
      need = Math.max(need, Callouts.extent(all[l], calloutGap))
    rowHeight = need
    diagramTop = Math.round((rowHeight - diagramHeight) / 2)

    var scale = diagramWidth / layout.viewBox.width
    for (var k = 0; k < list.length; k++) {
      list[k].px = diagramX + list[k].ax * scale
      list[k].py = diagramTop + list[k].ay * scale
    }

    layLane(lanesLeft, "left")
    layLane(lanesRight, "right")

    placed = left.concat(right)
  }

  function laneSplit(items, side) {
    items.sort(Callouts.byInner(side))
    var n = Callouts.laneCount(items, diagramHeight, calloutGap, maxLanes)
    var lanes = Callouts.splitLanes(items, n, calloutGap)
    for (var i = 0; i < lanes.length; i++) lanes[i].sort(Callouts.byAnchor)
    return lanes
  }

  function sideWidth(lanes) {
    var w = 0
    for (var i = 0; i < lanes.length; i++) {
      if (lanes[i].length === 0) continue
      if (w > 0) w += laneGap
      w += Callouts.laneWidth(lanes[i])
    }
    return w
  }

  // Give every box in every lane of one side its x, its y, and the four points
  // its leader is drawn through. Lane 0 is the one nearest the drawing.
  //
  // A lane-0 leader is the simple thing: out of the anchor, bend, into the box.
  // A lane-1 leader threads a gap in lane 0 and then runs down the channel
  // between the lanes to its own box — see `Callouts.threadGaps`. Both are the
  // same four-point path; for lane 0 the middle two points collapse onto the
  // bend, so one delegate draws either.
  function layLane(lanes, side) {
    var isLeft = side === "left"
    // Every leader on a side bends at the same x, just clear of the drawing,
    // so the lead-ins read as one comb rather than a scribble.
    var bend = isLeft ? leftWidth + columnGap - 4
                      : diagramX + diagramWidth + 4
    var cursor = isLeft ? leftWidth : diagramX + diagramWidth + columnGap

    for (var j = 0; j < lanes.length; j++) {
      var lane = lanes[j]
      if (lane.length === 0) continue
      var lw = Callouts.laneWidth(lane)
      Callouts.place(lane, 0, rowHeight, calloutGap)
      for (var i = 0; i < lane.length; i++) {
        var c = lane[i]
        c.lane = j
        c.bx = isLeft ? cursor - c.w : cursor
        c.by = c.top
        c.linkY = c.top + c.link
        // The leader meets the box on the edge that faces the drawing, which
        // is the lane edge either way: a left-hand box is right-aligned in its
        // lane, a right-hand one left-aligned.
        c.endX = cursor
        c.bendX = bend
        c.channelX = bend
        c.crossY = c.linkY
      }
      cursor += isLeft ? -(lw + laneGap) : (lw + laneGap)
    }

    if (lanes.length < 2 || lanes[0].length === 0 || lanes[1].length === 0) return

    var inner = Callouts.laneWidth(lanes[0])
    var channel = isLeft
      ? leftWidth - inner - laneGap / 2
      : diagramX + diagramWidth + columnGap + inner + laneGap / 2
    var outer = lanes[1].slice()
    outer.sort(function (a, b) { return a.linkY - b.linkY })
    Callouts.threadGaps(outer, lanes[0], 0, rowHeight, 3)
    for (var q = 0; q < outer.length; q++) outer[q].channelX = channel
  }

  onSheetChanged: relayout()
  onLayoutChanged: relayout()

  // Text measurement for `boxWidth`. Hidden, and only ever poked at from
  // `relayout()` — the fonts here must match the delegates below exactly.
  TextMetrics {
    id: mHeader
    font.family: root.fontFamily; font.pixelSize: root.fontSmall; font.bold: true
  }
  TextMetrics {
    id: mRow
    font.family: root.fontFamily; font.pixelSize: root.fontBody
  }
  TextMetrics {
    id: mRowItalic
    font.family: root.fontFamily; font.pixelSize: root.fontBody; font.italic: true
  }
  TextMetrics {
    id: mTag
    font.family: root.fontFamily; font.pixelSize: root.fontCaption
  }
  TextMetrics {
    id: mPlus
    font.family: root.fontFamily; font.pixelSize: root.fontSmall
  }

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
      implicitWidth: root.leftWidth + root.rightWidth + 2 * root.columnGap
        + root.diagramWidth
      implicitHeight: root.rowHeight

      // The drawing. `artSvg` arrives already recoloured, as a data: URL —
      // Qt renders SVG from one happily, and it keeps the themed copy out of
      // the filesystem.
      Image {
        x: root.diagramX
        y: root.diagramTop
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

      // Leader lines, UNDER the callout boxes. A box paints the card's own
      // background, so where a second-lane leader has to reach past the first
      // lane it disappears behind it instead of scribbling through the text.
      Repeater {
        model: root.placed
        delegate: Shape {
          id: leader
          required property var modelData
          anchors.fill: parent
          preferredRendererType: Shape.CurveRenderer
          z: 0

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
            PathLine { x: leader.modelData.bendX; y: leader.modelData.crossY }
            PathLine { x: leader.modelData.channelX; y: leader.modelData.crossY }
            PathLine { x: leader.modelData.channelX; y: leader.modelData.linkY }
            PathLine { x: leader.modelData.endX; y: leader.modelData.linkY }
          }
        }
      }

      // Anchor dots, on top of the drawing.
      Repeater {
        model: root.placed
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
        model: root.placed
        delegate: calloutComponent
      }
    }

    Rectangle {
      Layout.fillWidth: true
      height: 1
      color: root.palMuted
      opacity: 0.35
    }

    // -------------------------------------------------------- legend + modes
    RowLayout {
      Layout.fillWidth: true
      spacing: 28

      ModesBlock {
        modes: root.sheet ? root.sheet.modes : []
        total: root.sheet
          ? root.sheet.guide_chords.length + root.sheet.buttons.length
            + root.sheet.osk_buttons.length + root.sheet.ambient.length
          : 0
        Layout.alignment: Qt.AlignTop
      }

      Item { Layout.fillWidth: true }

      // Anything the layout could not anchor. Empty for a drawing that covers
      // the pad, which is the point — but a binding must never vanish just
      // because the diagram has no dot for it.
      ColumnLayout {
        visible: root.overflow.length > 0
        Layout.alignment: Qt.AlignTop
        spacing: 4
        Text {
          text: "Not on this diagram"
          color: root.palAccent
          font.family: root.fontFamily
          font.pixelSize: root.fontSmall
          font.bold: true
        }
        Repeater {
          model: root.overflow
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
          }
        }
      }
    }

    // ------------------------------------------------------------- legend
    Flow {
      Layout.fillWidth: true
      spacing: 18

      // One entry per modifier the live config actually uses, spelled the way
      // the rows above spell it: the glyph, a "+", and what holding it means.
      Repeater {
        model: root.modifierKeys
        delegate: Row {
          id: legendItem
          required property var modelData
          spacing: 6
          readonly property var spec: root.modifiers[modelData]

          Item {
            width: root.rowGlyph; height: root.rowGlyph
            anchors.verticalCenter: parent.verticalCenter
            Image {
              id: legendGlyph
              anchors.fill: parent
              fillMode: Image.PreserveAspectFit
              smooth: true
              sourceSize.width: root.rowGlyph * 3
              sourceSize.height: root.rowGlyph * 3
              source: root.artSource(legendItem.spec)
              visible: status === Image.Ready
            }
            Text {
              anchors.centerIn: parent
              visible: !legendGlyph.visible
              text: root.artText(legendItem.spec, "")
              color: root.palAccent
              font.family: root.fontFamily
              font.pixelSize: root.fontCaption
              font.bold: true
            }
          }
          Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "+ … = " + ((legendItem.spec && legendItem.spec.legend)
                              ? legendItem.spec.legend : legendItem.modelData)
            color: root.palMuted
            font.family: root.fontFamily
            font.pixelSize: root.fontCaption
          }
        }
      }

      Text {
        text: "italic = label derived from the action"
        color: root.palMuted
        font.family: root.fontFamily
        font.pixelSize: root.fontCaption
        font.italic: true
      }
      Text {
        text: "dim = only in that mode"
        color: root.palMuted
        font.family: root.fontFamily
        font.pixelSize: root.fontCaption
      }
    }

    // ------------------------------------------------------------- footer
    Text {
      Layout.fillWidth: true
      textFormat: Text.PlainText
      text: {
        var bits = []
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
      id: callout
      required property var modelData
      z: 2
      x: modelData.bx
      y: modelData.by
      width: modelData.w
      height: modelData.h

      // Opaque, so a leader running past to the outer lane goes behind the
      // text rather than through it; the hairline border is what turns a
      // stack of rows into one thing you can read as a unit.
      Rectangle {
        anchors.fill: parent
        radius: 6
        color: root.palBackground
        border.width: 1
        border.color: Qt.rgba(root.palMuted.r, root.palMuted.g, root.palMuted.b, 0.30)
      }

      Column {
        x: root.boxPadH
        y: root.boxPadTop
        width: parent.width - 2 * root.boxPadH
        spacing: 0

        // Header: the control's chip and its name. This is the line the
        // leader lands on.
        Item {
          width: parent.width
          height: root.headerH

          Row {
            anchors.verticalCenter: parent.verticalCenter
            spacing: root.cellGap

            Item {
              width: root.glyphSize; height: root.glyphSize
              Image {
                id: glyphImage
                anchors.fill: parent
                fillMode: Image.PreserveAspectFit
                smooth: true
                sourceSize.width: root.glyphSize * 3
                sourceSize.height: root.glyphSize * 3
                source: root.artSource(root.glyphs[callout.modelData.control])
                visible: status === Image.Ready
              }
              // The layout's `text` fallback, for a glyph file that is missing
              // — which is the normal case for a layout that ships no art.
              Text {
                anchors.centerIn: parent
                visible: !glyphImage.visible
                text: root.artText(root.glyphs[callout.modelData.control],
                                   callout.modelData.control)
                color: root.palAccent
                font.family: root.fontFamily
                font.pixelSize: root.fontCaption
                font.bold: true
              }
            }

            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: callout.modelData.controlLabel
              color: root.palAccent
              font.family: root.fontFamily
              font.pixelSize: root.fontSmall
              font.bold: true
            }
          }
        }

        // One row per binding on this control.
        Repeater {
          model: modelData.rows
          delegate: Item {
            id: row
            required property var modelData
            width: parent.width
            height: root.rowH

            Row {
              anchors.verticalCenter: parent.verticalCenter
              spacing: root.cellGap

              // The chord this row fires on: what you hold, a "+", and the
              // button it lands on. The column is reserved for the whole box
              // and the chord right-aligned inside it, so the buttons stack in
              // one column with the held modifiers hanging off to their left.
              Item {
                width: callout.modelData.chordW; height: root.rowH

                Row {
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: root.chordGap

                  Item {
                    width: root.rowGlyph; height: root.rowGlyph
                    visible: row.modelData.modifier !== ""
                    Image {
                      id: modImage
                      anchors.fill: parent
                      fillMode: Image.PreserveAspectFit
                      smooth: true
                      sourceSize.width: root.rowGlyph * 3
                      sourceSize.height: root.rowGlyph * 3
                      source: root.artSource(root.modifiers[row.modelData.modifier])
                      visible: status === Image.Ready
                    }
                    Text {
                      anchors.centerIn: parent
                      visible: !modImage.visible
                      text: root.artText(root.modifiers[row.modelData.modifier],
                                         row.modelData.modifier)
                      color: root.palAccent
                      font.family: root.fontFamily
                      font.pixelSize: root.fontCaption
                      font.bold: true
                    }
                  }

                  Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: row.modelData.modifier !== ""
                    text: "+"
                    color: root.palMuted
                    font.family: root.fontFamily
                    font.pixelSize: root.fontSmall
                  }

                  Item {
                    width: root.rowGlyph; height: root.rowGlyph
                    Image {
                      id: rowGlyphImage
                      anchors.fill: parent
                      fillMode: Image.PreserveAspectFit
                      smooth: true
                      sourceSize.width: root.rowGlyph * 3
                      sourceSize.height: root.rowGlyph * 3
                      source: root.artSource(root.glyphs[row.modelData.glyph])
                      visible: status === Image.Ready
                    }
                    Text {
                      anchors.centerIn: parent
                      visible: !rowGlyphImage.visible
                      text: root.artText(root.glyphs[row.modelData.glyph],
                                         row.modelData.glyph)
                      color: root.palAccent
                      font.family: root.fontFamily
                      font.pixelSize: root.fontCaption
                      font.bold: true
                    }
                  }
                }
              }

              // Reserved for the whole box, so the labels line up whether or
              // not a given row is a flick.
              Item {
                width: root.arrowW; height: root.rowH
                visible: callout.modelData.hasDirection
                Text {
                  anchors.centerIn: parent
                  text: Callouts.arrow(row.modelData.direction)
                    || row.modelData.direction
                  color: root.palText
                  font.family: root.fontFamily
                  font.pixelSize: root.fontBody
                }
              }

              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: row.modelData.label
                color: root.palText
                font.family: root.fontFamily
                font.pixelSize: root.fontBody
                // A label the config actually wrote reads as authored; a
                // derived one is dimmed and italic, so the owner can see at a
                // glance which bindings still want a description.
                opacity: row.modelData.described ? 1.0 : 0.75
                font.italic: !row.modelData.described
              }

              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: row.modelData.guarded ? "· " + row.modelData.guard : ""
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
