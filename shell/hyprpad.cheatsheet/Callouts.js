.pragma library

// Turning `hyprpad bindings --json` plus a layout descriptor into placed
// callouts. Kept out of Sheet.qml because it is arithmetic, not scene graph —
// and because it is the only part of the widget that is worth reasoning about
// on paper.
//
// The model is one callout per PHYSICAL control, carrying every binding that
// lands on it whatever family it came from. A row opens with the CHORD that
// fires it — the button's own glyph, behind a modifier glyph and a "+" when
// something has to be held first — and then says what that does. Nothing is
// banished to a table underneath the drawing, because a table underneath the
// drawing is exactly the thing that made the first version confusing: the
// reader had to join "x" in a table to the X button in the picture themselves.
//
// One sheet is one CONTEXT: a tab per mode, showing what is live in that mode
// and nothing else. A binding that lived under a dim "only in desktop" tag on
// the old single sheet is now simply on the desktop tab, and absent from the
// others — which is the difference between a document about the config and a
// picture of what the pad does right now.

// The sheet's four families, in the order their rows stack inside a callout:
// what the control does on its own first, then what a held modifier makes it
// do. (`sortRows` re-applies this by modifier, so a family that lands in the
// wrong place here is still ordered correctly.)
var SECTIONS = ["buttons", "ambient", "guide_chords", "osk_buttons"]

// The modifier each family implies. "" is "press it, nothing held" — the
// bare buttons and the pads' ambient behaviour.
var SECTION_MODIFIER = {
  "button": "",
  "ambient": "",
  "guide": "guide",
  "osk_button": "osk"
}

// Bare first, then the guide layer, then the on-screen keyboard's helpers.
var MODIFIER_RANK = { "": 0, "guide": 1, "osk": 2 }

var ARROWS = { up: "↑", down: "↓", left: "←", right: "→" }

function arrow(direction) { return ARROWS[direction] || "" }

function modifierOf(entry) {
  var m = SECTION_MODIFIER[entry.section]
  return m === undefined ? "" : m
}

// --- tabs -------------------------------------------------------------------

// One tab per context the pad can be in, in the order the config's rules are
// evaluated, plus the ones the daemon hardwires (`builtin`, which today is the
// on-screen keyboard). The sheet is always showing SOME context, so a config
// that declares no modes still gets a tab — named for its default.
function tabs(sheet) {
  var out = []
  var modes = (sheet && sheet.modes) || []
  var declared = 0
  for (var i = 0; i < modes.length; i++) {
    var m = modes[i]
    if (!m.builtin) declared++
    out.push({
      name: m.name,
      builtin: m.builtin === true,
      // A built-in context names the section that IS it, which is how the tab
      // knows its rows without knowing what the context does.
      section: m.section || "",
      isDefault: m.default === true,
      forward: m.forward === true,
      hasRule: m.has_rule === true
    })
  }
  if (declared === 0) {
    out.unshift({
      name: (sheet && sheet.default_mode) || "all",
      builtin: false,
      section: "",
      isDefault: true,
      forward: false,
      hasRule: false
    })
  }
  return out
}

// The sections a built-in tab has claimed. A declared mode never shows those
// rows: they are not live in a mode, they are live in that context — the
// keyboard's helpers do nothing until the keyboard is up, and while it is up
// nothing else works.
function ownedSections(list) {
  var owned = {}
  for (var i = 0; i < list.length; i++)
    if (list[i].section !== "") owned[list[i].section] = true
  return owned
}

// Whether a binding is live in a mode, from the guard the JSON carries.
function allows(guard, mode) {
  if (!guard) return true
  if (guard.kind === "only_in") return (guard.modes || []).indexOf(mode) >= 0
  if (guard.kind === "not_in") return (guard.modes || []).indexOf(mode) < 0
  // A `when` predicate needs a live context to answer. The sheet cannot have
  // one, so the row shows wherever it might fire and keeps its guard tag.
  return true
}

// Whether a row belongs on a tab. `tab` undefined is "every binding", which is
// what a caller that does not tab wants.
function inTab(entry, tab, owned) {
  if (!tab) return true
  if (tab.section !== "") return entry.section === tab.section
  if (owned[entry.section]) return false
  return allows(entry.guard, tab.name)
}

// --- grouping ---------------------------------------------------------------

// `layout.groups` lets a descriptor say that several of hyprpad's control ids
// are one thing you can point at — the four d-pad directions are a d-pad. The
// engine never names them: it reads the members out of the layout, so a pad
// with a hat switch or a second stick groups the same way.
function memberIndex(layout) {
  var idx = {}
  var groups = (layout && layout.groups) || {}
  for (var key in groups) {
    var members = groups[key].members || {}
    for (var m in members) idx[m] = key
  }
  return idx
}

// One callout per control (or per group), with a row for every binding on it
// that is live on `tab` (see `inTab`; omit the tab for every binding there is).
function callouts(sheet, layout, tab, owned) {
  var idx = memberIndex(layout)
  var groups = (layout && layout.groups) || {}
  var controls = (layout && layout.controls) || {}
  var glyphs = (layout && layout.glyphs) || {}
  var byKey = {}
  var order = []

  for (var s = 0; s < SECTIONS.length; s++) {
    var list = sheet[SECTIONS[s]] || []
    for (var i = 0; i < list.length; i++) {
      var e = list[i]
      if (!inTab(e, tab, owned || {})) continue
      var key = idx[e.control] || e.control
      var anchor = controls[key]
      // A binding on a control this layout does not draw is not lost: the
      // caller lists it under the drawing.
      if (!anchor) continue

      var c = byKey[key]
      if (!c) {
        c = byKey[key] = {
          control: key,
          controlLabel: groups[key] ? (groups[key].label || key) : e.control_label,
          grouped: groups[key] !== undefined,
          side: anchor.side === "left" ? "left" : "right",
          hidden: anchor.hidden === true,
          ax: anchor.x,
          ay: anchor.y,
          rows: []
        }
        order.push(key)
      }

      // A flick carries its own direction; a grouped member's direction is
      // whatever the layout said that member points at.
      var direction = e.direction || ""
      if (direction === "" && groups[key])
        direction = (groups[key].members || {})[e.control] || ""

      c.rows.push({
        modifier: modifierOf(e),
        direction: direction,
        label: e.label,
        described: e.described === true,
        guard: e.guard_label,
        // The tab is the context now, so "only in desktop" on the desktop tab
        // is noise. What survives is the guard the sheet cannot decide for
        // itself: a `when` predicate might or might not be live in here.
        guarded: !!(e.guard && e.guard.kind === "when"),
        action: e.action,
        kind: e.action_kind,
        member: e.control
      })
    }
  }

  var out = []
  for (var k = 0; k < order.length; k++) {
    var g = byKey[order[k]]
    g.rows = sortRows(collapse(groups[g.control], g.rows))
    // Which button each row's chord draws. A member the layout gives a glyph
    // of draws itself — an unbound d-pad direction reads as that direction —
    // and everything else falls back to the callout's own glyph, which is what
    // a collapsed row (no single member) and a coarse glyph map both want.
    for (var r = 0; r < g.rows.length; r++)
      g.rows[r].glyph = glyphs[g.rows[r].member] ? g.rows[r].member : g.control
    // `control_label` for a stick flick carries that flick's arrow ("Right
    // stick →"). Each row draws its own arrow, so the shared heading must drop
    // it — otherwise a two-flick stick is headed with only one of its
    // directions.
    if (!g.grouped && anyDirection(g.rows))
      g.controlLabel = String(g.controlLabel).replace(/\s*[↑↓←→]\s*$/, "")
    g.hasDirection = anyDirection(g.rows)
    out.push(g)
  }
  return out
}

function anyDirection(rows) {
  for (var i = 0; i < rows.length; i++) if (rows[i].direction !== "") return true
  return false
}

// Stable sort by modifier, so "what it does bare" always heads the stack.
function sortRows(rows) {
  var deco = []
  for (var i = 0; i < rows.length; i++) deco.push({ i: i, r: rows[i] })
  deco.sort(function (a, b) {
    var d = (MODIFIER_RANK[a.r.modifier] || 0) - (MODIFIER_RANK[b.r.modifier] || 0)
    return d !== 0 ? d : a.i - b.i
  })
  var out = []
  for (var j = 0; j < deco.length; j++) out.push(deco[j].r)
  return out
}

// A group every one of whose members is bound to nothing but its own direction
// key reads better as one row: "arrow keys", not four lines saying Up, Down,
// Left, Right. The engine only checks that the bindings really are that; what
// the collapsed row *says* comes from the layout, so the rule stays generic.
//
// The check runs per (modifier, guard) bucket, so a d-pad that is arrows bare
// and something else under the guide button collapses the arrows and leaves
// the rest alone.
function collapse(group, rows) {
  if (!group || !group.collapse) return rows
  var members = group.members || {}
  var names = []
  for (var m in members) names.push(m)
  if (names.length < 2) return rows

  var buckets = {}
  var order = []
  for (var i = 0; i < rows.length; i++) {
    // A separator neither half can contain, spelled as an escape: a raw one
    // in the source makes this file look binary to every tool that reads it.
    var key = rows[i].modifier + "\u0000" + (rows[i].guarded ? rows[i].guard : "")
    if (!buckets[key]) { buckets[key] = []; order.push(key) }
    buckets[key].push(rows[i])
  }

  var out = []
  for (var b = 0; b < order.length; b++) {
    var bucket = buckets[order[b]]
    if (isWholeGroup(bucket, members, names.length)) {
      out.push({
        modifier: bucket[0].modifier,
        direction: group.collapse.direction || "",
        label: group.collapse.label || "",
        described: false,
        guard: bucket[0].guard,
        guarded: bucket[0].guarded,
        action: "",
        kind: bucket[0].kind,
        member: ""
      })
    } else {
      for (var q = 0; q < bucket.length; q++) out.push(bucket[q])
    }
  }
  return out
}

function isWholeGroup(bucket, members, count) {
  if (bucket.length !== count) return false
  var seen = {}
  for (var i = 0; i < bucket.length; i++) {
    var r = bucket[i]
    var dir = members[r.member]
    if (dir === undefined || seen[r.member]) return false
    seen[r.member] = true
    // Only "this direction's key, and nothing else" collapses. A member doing
    // something interesting is a binding that has earned its own row.
    if (r.kind !== "key" || r.action !== "key " + dir) return false
  }
  return true
}

// Which modifiers this sheet actually uses, in stacking order — the legend
// under the drawing explains those and no others.
function usedModifiers(list) {
  var seen = {}
  var out = []
  for (var i = 0; i < list.length; i++) {
    for (var j = 0; j < list[i].rows.length; j++) {
      var m = list[i].rows[j].modifier
      if (m !== "" && !seen[m]) { seen[m] = true; out.push(m) }
    }
  }
  out.sort(function (a, b) {
    return (MODIFIER_RANK[a] || 0) - (MODIFIER_RANK[b] || 0)
  })
  return out
}

// Which of this tab's entries had no anchor in this layout — listed under the
// drawing so a control the drawing omits is still documented.
function unplaced(sheet, layout, tab, owned) {
  var idx = memberIndex(layout)
  var controls = (layout && layout.controls) || {}
  var out = []
  for (var s = 0; s < SECTIONS.length; s++) {
    var list = sheet[SECTIONS[s]] || []
    for (var i = 0; i < list.length; i++) {
      var e = list[i]
      if (!inTab(e, tab, owned || {})) continue
      if (!controls[idx[e.control] || e.control]) out.push(e)
    }
  }
  return out
}

// --- placement --------------------------------------------------------------

function extent(items, gap) {
  var h = 0
  for (var i = 0; i < items.length; i++) h += items[i].h
  return items.length ? h + (items.length - 1) * gap : 0
}

// How many lanes a side wants. The drawing's own height is the budget: a
// column taller than the controller it annotates is what made the first
// version's right-hand side a wall of text. Capped so the sheet never grows a
// third lane and starts looking like a spreadsheet.
function laneCount(items, budget, gap, maxLanes) {
  if (items.length < 4) return 1   // two lanes of one box is not a column
  var want = Math.ceil(extent(items, gap) / Math.max(1, budget))
  return Math.max(1, Math.min(maxLanes, want, Math.floor(items.length / 2)))
}

// Split one side into lanes at the point that makes the taller lane shortest.
//
// `items` must be sorted innermost-first (see `byInner`), so the lane nearest
// the drawing gets the controls nearest the drawing: the columns then read
// outward in the same order as the hardware, and no leader has to double back
// past a control that sits closer in than the one it serves.
function splitLanes(items, lanes, gap) {
  if (lanes < 2 || items.length < 2) return [items]
  var best = 1
  var bestCost = Infinity
  for (var k = 1; k < items.length; k++) {
    var cost = Math.max(extent(items.slice(0, k), gap),
                        extent(items.slice(k), gap))
    if (cost < bestCost) { bestCost = cost; best = k }
  }
  return [items.slice(0, best), items.slice(best)]
}

// Place one lane's callouts.
//
// Each box wants its HEADER — the line the leader lands on — level with its own
// anchor; where that would make two overlap, the lane is spread. A single
// forward sweep (push everything down to clear its predecessor) followed by a
// backward sweep (pull the whole run up if it overflowed the bottom) is enough
// for a lane of this size and is stable — the same input always lands in the
// same place, so the sheet does not jitter between summons.
//
// `items` must already be sorted by `ay`, and each must carry `h` (box height),
// `link` (header offset from the box top) and `py` (anchor, in the same pixel
// space). Sets `top` on each.
function place(items, top, bottom, gap) {
  var n = items.length
  if (n === 0) return items

  var y = top
  for (var i = 0; i < n; i++) {
    var want = items[i].py - items[i].link
    if (want < y) want = y
    items[i].top = want
    y = want + items[i].h + gap
  }

  // Overflowed the bottom: walk back up, packing against the floor.
  var overflow = (y - gap) - bottom
  if (overflow > 0) {
    var floor = bottom
    for (var j = n - 1; j >= 0; j--) {
      if (items[j].top + items[j].h > floor) items[j].top = floor - items[j].h
      if (items[j].top < top) items[j].top = top
      floor = items[j].top - gap
    }
  }
  return items
}

function clamp(v, lo, hi) { return v < lo ? lo : (v > hi ? hi : v) }

// The clear runs down a placed lane: above the first box, between each pair,
// and below the last, as the y interval a line may cross in.
//
// A run only as wide as the gap between two boxes offers no choice worth
// having — anywhere but its middle grazes a border and reads as an underline —
// so a narrow one collapses to its midpoint. A wide one (above the first box,
// below the last) keeps its interval, inset from the boxes, so a leader can
// cross at its own height and stay straight.
var NARROW_RUN = 24

function run(a, b, inset) {
  if (b - a < NARROW_RUN) {
    var mid = (a + b) / 2
    return { a: mid, b: mid }
  }
  return { a: a + inset, b: b - inset }
}

function freeRuns(items, top, bottom, inset) {
  var runs = []
  var prev = top
  for (var i = 0; i < items.length; i++) {
    if (items[i].top - prev > 2) runs.push(run(prev, items[i].top, inset))
    prev = items[i].top + items[i].h
  }
  if (bottom - prev > 2) runs.push(run(prev, bottom, inset))
  return runs
}

// Pick, for each outer-lane callout, the y at which its leader crosses the
// inner lane.
//
// This is the whole reason a second lane is legible at all. A leader that
// reaches past the near column has to get through it somehow; if it simply
// runs at its own height it disappears behind a near box and comes out the far
// side, which reads as though the two boxes were joined. Crossing in a GAP
// keeps the line unbroken and obviously in transit. Runs are handed out in y
// order and never reused, so the crossings cannot cross each other either.
//
// `outer` must be sorted by `linkY`; `inner` must already be placed.
function threadGaps(outer, inner, top, bottom, inset) {
  var runs = freeRuns(inner, top, bottom, inset)
  var next = 0
  for (var i = 0; i < outer.length; i++) {
    var want = outer[i].linkY
    var best = -1
    var bestDist = Infinity
    for (var j = next; j < runs.length; j++) {
      var d = Math.abs(clamp(want, runs[j].a, runs[j].b) - want)
      if (d < bestDist) { bestDist = d; best = j }
    }
    // More boxes out here than runs in there: the last few cross where they
    // like and take the occlusion.
    if (best < 0) { outer[i].crossY = want; continue }
    outer[i].crossY = clamp(want, runs[best].a, runs[best].b)
    next = best + 1
  }
}

// Sort helper: by anchor y, then x, so a lane is deterministic.
function byAnchor(a, b) {
  if (a.ay !== b.ay) return a.ay - b.ay
  return a.ax - b.ax
}

// Sort helper: innermost control first, for the lane split. "Innermost" is
// nearest the middle of the drawing, which is the smallest x on the right-hand
// side and the largest on the left.
function byInner(side) {
  var sign = side === "left" ? -1 : 1
  return function (a, b) {
    var d = sign * (a.ax - b.ax)
    return d !== 0 ? d : a.ay - b.ay
  }
}

// The widest box in a lane — the lane is exactly that wide, so nothing is ever
// clipped and a lane of short labels does not reserve space it will not use.
function laneWidth(items) {
  var w = 0
  for (var i = 0; i < items.length; i++) if (items[i].w > w) w = items[i].w
  return w
}

// Recolour a white-on-transparent SVG to `color`.
//
// Both drawings the controller layout can use — Valve's and ours — stroke in plain
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
