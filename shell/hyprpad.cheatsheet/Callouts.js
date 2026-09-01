.pragma library

// Turning `hyprpad bindings --json` plus a layout descriptor into placed
// callouts. Kept out of Panel.qml because it is arithmetic, not scene graph —
// and because it is the only part of the widget that is worth reasoning about
// on paper.

// Group the entries that belong on the diagram by the physical control they
// live on, so the two right-stick flicks become ONE callout with two lines
// rather than two leaders fighting over the same anchor.
//
// `sections` picks which of the sheet's four families are drawn on the
// diagram; the rest go in the tables below it.
function group(sheet, layout, sections) {
  var byControl = {}
  var order = []
  for (var s = 0; s < sections.length; s++) {
    var list = sheet[sections[s]] || []
    for (var i = 0; i < list.length; i++) {
      var e = list[i]
      var anchor = layout.controls ? layout.controls[e.control] : null
      // A binding on a control this layout does not draw is not lost: the
      // caller shows it in the overflow table.
      if (!anchor) continue
      if (!byControl[e.control]) {
        byControl[e.control] = {
          control: e.control,
          controlLabel: e.control_label,
          side: anchor.side === "left" ? "left" : "right",
          hidden: anchor.hidden === true,
          ax: anchor.x,
          ay: anchor.y,
          lines: []
        }
        order.push(e.control)
      }
      byControl[e.control].lines.push({
        label: e.label,
        described: e.described === true,
        action: e.action,
        kind: e.action_kind,
        guard: e.guard_label,
        guarded: !(e.guard && e.guard.kind === "always"),
        // A flick's direction is what distinguishes the two right-stick rows.
        direction: e.direction || "",
        section: e.section
      })
    }
  }
  var out = []
  for (var k = 0; k < order.length; k++) {
    var g = byControl[order[k]]
    // `control_label` for a stick flick carries that flick's arrow ("Right
    // stick →"). When a control has more than one binding each row draws its
    // own arrow, so the shared heading must drop it — otherwise a two-flick
    // stick is headed with only one of its two directions.
    if (g.lines.length > 1) g.controlLabel = g.controlLabel.replace(/\s*[↑↓←→]\s*$/, "")
    out.push(g)
  }
  return out
}

// Which entries had no anchor in this layout — shown in a table so a control
// the drawing omits is still documented.
function unplaced(sheet, layout, sections) {
  var out = []
  for (var s = 0; s < sections.length; s++) {
    var list = sheet[sections[s]] || []
    for (var i = 0; i < list.length; i++) {
      var e = list[i]
      if (!layout.controls || !layout.controls[e.control]) out.push(e)
    }
  }
  return out
}

// Place one column's callouts.
//
// Each callout wants to sit level with its own anchor; where that would make
// two overlap, the column is spread. A single forward sweep (push everything
// down to clear its predecessor) followed by a backward sweep (pull the whole
// run up if it overflowed the bottom) is enough for a column of this size and
// is stable — the same input always lands in the same place, so the sheet does
// not jitter between summons.
//
// `items` must already be sorted by `ay`. Returns the same array with a `cy`
// (callout centre, in the same pixel space as `heights`) on each item.
function place(items, heights, top, bottom, gap) {
  var n = items.length
  if (n === 0) return items

  var y = top
  for (var i = 0; i < n; i++) {
    var h = heights[i]
    var want = items[i].py - h / 2      // ideal top edge: level with the anchor
    if (want < y) want = y
    items[i]._top = want
    y = want + h + gap
  }

  // Overflowed the bottom: walk back up, packing against the floor.
  var overflow = (y - gap) - bottom
  if (overflow > 0) {
    var floor = bottom
    for (var j = n - 1; j >= 0; j--) {
      var hh = heights[j]
      if (items[j]._top + hh > floor) items[j]._top = floor - hh
      if (items[j]._top < top) items[j]._top = top
      floor = items[j]._top - gap
    }
  }

  for (var k = 0; k < n; k++) items[k].cy = items[k]._top + heights[k] / 2
  return items
}

// Sort helper: by anchor y, then x, so a column is deterministic.
function byAnchor(a, b) {
  if (a.ay !== b.ay) return a.ay - b.ay
  return a.ax - b.ax
}

// Recolour a white-on-transparent SVG to `color`.
//
// Both drawings the puck layout can use — Valve's and ours — stroke in plain
// `white`, and Kenney's glyphs fill in `#FFFFFF`, so one substitution themes
// every piece of art the widget draws. Anything that is not white is left
// alone, which is what keeps a future full-colour layout from being wrecked.
function recolor(svg, color) {
  if (!svg) return ""
  return svg
    .replace(/#FFFFFF/gi, color)
    .replace(/#FFF\b/gi, color)
    .replace(/"white"/gi, '"' + color + '"')
    .replace(/:\s*white\b/gi, ": " + color)
}
