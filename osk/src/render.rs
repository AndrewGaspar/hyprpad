//! A small software renderer: draws the placed keys into an ARGB8888 shm
//! buffer, with legible labels from an embedded 8x8 bitmap font (`font8x8`).
//!
//! Every colour and every dimension in the draw path comes from the active
//! [`Theme`] ([`crate::theme`]) — there are **no hardcoded colours or sizes
//! left here**. This is the native equivalent of the Deck's §4.7 model: a token
//! table drives appearance, the geometry is separate and un-themeable. It is
//! still deliberately CPU-only and un-animated — the kickoff needs keys that
//! are *legible and correctly positioned*; glow/pulse animations and a GPU path
//! are DEFERRED (osk-technology.md §4.7 / §8). The renderer takes a list of
//! [`Highlight`]s so the structure for §4.5's concurrent per-source highlights
//! (left pad, right pad, d-pad focus — up to three at once) is already in place.

use font8x8::{UnicodeFonts, BASIC_FONTS};

use crate::layout::{Key, KeyRole, PlacedKey, Rect, ShiftModel};
use crate::theme::{Color, Theme};

/// Which input source a highlight comes from — mirrors osk-technology.md §4.5's
/// three concurrent highlight sources. The concrete colour is resolved from the
/// active [`Theme`] at draw time (see [`Theme::colors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightKind {
    LeftPad,
    RightPad,
    Focus,
}

impl HighlightKind {
    fn color(self, theme: &Theme) -> Color {
        match self {
            HighlightKind::LeftPad => theme.colors.pointer_left,
            HighlightKind::RightPad => theme.colors.pointer_right,
            HighlightKind::Focus => theme.colors.focus,
        }
    }
}

/// A key to draw highlighted, and by which source.
#[derive(Clone, Copy, Debug)]
pub struct Highlight {
    pub key: usize,
    pub kind: HighlightKind,
}

/// A trackpad cursor sprite to draw on top of the keys, at a panel-local pixel
/// position. `kind` selects the per-pad body colour (osk-technology.md §4.1 —
/// the Deck draws one ~30x30 pointer per active pad). Two can be live at once.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub x: f32,
    pub y: f32,
    pub kind: HighlightKind,
}

/// The live chrome state the draw path needs beyond the keys themselves: the
/// shift level — latched *or* held, it drives the legends and lights the Shift
/// keys either way — and the current reflow policy (drives the `Push`/`Float`
/// legend on the display-toggle key).
#[derive(Clone, Copy, Debug)]
pub struct Chrome {
    pub shift: ShiftModel,
    pub reflow: bool,
}

/// One slot of the candidate strip, ready to draw
/// (osk-prediction.md §7.2). An empty `text` draws the slot's well with no
/// legend — the strip keeps its space rather than shifting the key rows when
/// there is nothing to suggest (§7.4: layout stability matters more to an
/// absolute-cursor keyboard than the strip does).
#[derive(Clone, Debug)]
pub struct StripSlot {
    pub rect: Rect,
    pub text: String,
    /// The candidate `R1` would accept. Exactly one slot is highlighted while
    /// there is anything to accept.
    pub selected: bool,
    /// A pad's cursor is resting on this slot, and which pad's.
    pub hover: Option<HighlightKind>,
}

/// A mutable view over an shm buffer as ARGB8888 pixels.
pub struct Canvas<'a> {
    buf: &'a mut [u8],
    w: i32,
    h: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u8], w: i32, h: i32) -> Canvas<'a> {
        Canvas { buf, w, h }
    }

    fn put(&mut self, x: i32, y: i32, c: Color) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let (a, r, g, b) = c.parts();
        // Premultiply so partial alpha composites correctly on the surface.
        let pr = r * a / 255;
        let pg = g * a / 255;
        let pb = b * a / 255;
        // wl_shm ARGB8888 stored as a little-endian u32 → memory [B, G, R, A].
        let idx = ((y * self.w + x) * 4) as usize;
        self.buf[idx] = pb as u8;
        self.buf[idx + 1] = pg as u8;
        self.buf[idx + 2] = pr as u8;
        self.buf[idx + 3] = a as u8;
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, c);
            }
        }
    }

    /// Fill a rounded rectangle of corner radius `r` (px). `r <= 0` degrades to
    /// a plain rectangle. The corner test keeps only pixels inside the quarter
    /// circles — good enough for crisp keycaps at CPU cost.
    fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Color) {
        let r = r.clamp(0, w.min(h) / 2);
        if r == 0 {
            self.fill_rect(x, y, w, h, c);
            return;
        }
        for yy in 0..h {
            for xx in 0..w {
                // Distance from the nearest corner centre, when in a corner box.
                let cx = if xx < r {
                    r - 1 - xx
                } else if xx >= w - r {
                    xx - (w - r)
                } else {
                    0
                };
                let cy = if yy < r {
                    r - 1 - yy
                } else if yy >= h - r {
                    yy - (h - r)
                } else {
                    0
                };
                if cx * cx + cy * cy <= (r - 1) * (r - 1) || cx == 0 || cy == 0 {
                    self.put(x + xx, y + yy, c);
                }
            }
        }
    }

    /// Fill a disc of radius `r` (px) centred at `(cx, cy)`.
    fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        if r <= 0 {
            return;
        }
        let r2 = r * r;
        for yy in -r..=r {
            for xx in -r..=r {
                if xx * xx + yy * yy <= r2 {
                    self.put(cx + xx, cy + yy, c);
                }
            }
        }
    }

    /// Draw a per-pad trackpad **cursor sprite** centred at `(cx, cy)`: a
    /// contrasting halo disc (`cursor_stroke`) with the pad's colour on top,
    /// leaving a rim so it stays visible over a same-hued hover highlight
    /// (osk-technology.md §4.1). Sized from `geom.cursor_size`; both colours come
    /// from the [`Theme`], nothing hardcoded.
    fn draw_cursor(&mut self, cx: i32, cy: i32, theme: &Theme, kind: HighlightKind) {
        let r = ((theme.geom.cursor_size * 0.5) as i32).max(3);
        let rim = (r / 3).max(2);
        self.fill_circle(cx, cy, r, theme.colors.cursor_stroke); // contrast halo
        self.fill_circle(cx, cy, r - rim, kind.color(theme)); // pad-coloured body
        self.fill_circle(cx, cy, (rim / 2).max(1), theme.colors.cursor_stroke); // aim dot
    }

    /// Draw one glyph at integer `scale`, top-left at `(x, y)`.
    fn glyph(&mut self, ch: char, x: i32, y: i32, scale: i32, c: Color) {
        let bitmap = match BASIC_FONTS.get(ch) {
            Some(b) => b,
            None => return,
        };
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..8 {
                if (bits >> col) & 1 != 0 {
                    self.fill_rect(x + col * scale, y + row as i32 * scale, scale, scale, c);
                }
            }
        }
    }

    /// Draw `text` centred within the box `(bx, by, bw, bh)`. Picks the largest
    /// integer glyph scale that fits both dimensions but no larger than
    /// `scale_max` (the font token), so single-char keycaps get big glyphs and
    /// multi-char legends ("Enter", "Space") shrink to fit.
    fn text_centered(&mut self, text: &str, bbox: (i32, i32, i32, i32), scale_max: i32, c: Color) {
        let (bx, by, bw, bh) = bbox;
        let n = text.chars().count() as i32;
        if n == 0 {
            return;
        }
        let by_w = (bw - 4) / (n * 8);
        let by_h = (bh - 4) / 8;
        let scale = by_w.min(by_h).clamp(1, scale_max.max(1));
        let tw = n * 8 * scale;
        let th = 8 * scale;
        let mut x = bx + (bw - tw) / 2;
        let y = by + (bh - th) / 2;
        for ch in text.chars() {
            self.glyph(ch, x, y, scale, c);
            x += 8 * scale;
        }
    }
}

/// Draw a whole panel: themed background, the candidate strip, every placed key
/// (legends reflecting the live shift state, Shift/Caps lit when engaged), then
/// any per-pad highlights, then the trackpad cursor sprites on top. All colours
/// and dimensions are read from `theme` — no hardcoded cursor / highlight /
/// active colours in this path.
///
/// `strip` is empty when no prediction model is loaded, which is what makes the
/// keyboard pixel-identical to the one before prediction existed.
#[allow(clippy::too_many_arguments)]
pub fn draw_panel(
    canvas: &mut Canvas,
    theme: &Theme,
    keys: &[Key],
    placed: &[PlacedKey],
    strip: &[StripSlot],
    highlights: &[Highlight],
    cursors: &[Cursor],
    chrome: Chrome,
) {
    let col = &theme.colors;
    let corner = theme.geom.corner as i32;
    let border = theme.geom.border_width.max(1.0) as i32;
    let shift_active = chrome.shift.is_active();

    canvas.fill_rect(0, 0, canvas.w, canvas.h, col.surface_bg);

    // The candidate strip, drawn first so a pad cursor still floats over it.
    for slot in strip {
        let (x, y, w, h) =
            (slot.rect.x as i32, slot.rect.y as i32, slot.rect.w as i32, slot.rect.h as i32);
        // Hover wins over selection, as it does for keys: where the pad is
        // pointing must always be visible.
        let fill = match (slot.hover, slot.selected) {
            (Some(kind), _) => kind.color(theme),
            (None, true) => col.strip_highlight,
            (None, false) => col.strip_fill,
        };
        let bw = if slot.hover.is_some() || slot.selected { border.max(2) } else { border };
        canvas.fill_round_rect(x, y, w, h, corner, col.key_border);
        canvas.fill_round_rect(x + bw, y + bw, w - 2 * bw, h - 2 * bw, (corner - bw).max(0), fill);
        if !slot.text.is_empty() {
            canvas.text_centered(&slot.text, (x, y, w, h), theme.font.scale_max, col.key_label);
        }
    }

    for pk in placed {
        let key = &keys[pk.key];
        let (x, y, w, h) = (pk.rect.x as i32, pk.rect.y as i32, pk.rect.w as i32, pk.rect.h as i32);

        // A Shift/Caps key whose state is engaged is "active" (lit); a hovered
        // key is "highlighted". Highlight wins so the pad's focus stays visible.
        let hl = highlights.iter().find(|hh| hh.key == pk.key);
        let active = match key.role {
            KeyRole::Shift => shift_active,
            KeyRole::Caps => chrome.shift.caps_active(),
            _ => false,
        };
        let fill = if let Some(hh) = hl {
            hh.kind.color(theme)
        } else if active {
            col.shift_active
        } else {
            match key.role {
                KeyRole::Char => col.key_fill,
                _ => col.key_mod_fill,
            }
        };
        // The display-toggle key's legend tracks what a press will DO next:
        // "Push" while floating (press to displace), "Float" while displacing.
        let label = match key.role {
            KeyRole::DisplayToggle => {
                if chrome.reflow {
                    "Float"
                } else {
                    "Push"
                }
            }
            _ => key.display_label(shift_active),
        };
        // Border via a rounded-rect underlay, then the fill inset by the border
        // width — a themeable stroke that respects the corner radius.
        let bw = if hl.is_some() || active { border.max(2) } else { border };
        canvas.fill_round_rect(x, y, w, h, corner, col.key_border);
        canvas.fill_round_rect(x + bw, y + bw, w - 2 * bw, h - 2 * bw, (corner - bw).max(0), fill);
        canvas.text_centered(label, (x, y, w, h), theme.font.scale_max, col.key_label);
    }

    // Cursor sprites last, so each pad's pointer floats on top of the keys and
    // its own highlight (osk-technology.md §4.1/§4.5 — up to two live at once).
    for cur in cursors {
        canvas.draw_cursor(cur.x as i32, cur.y as i32, theme, cur.kind);
    }
}

/// Bytes needed for a `w x h` ARGB8888 buffer.
pub fn buffer_len(w: i32, h: i32) -> usize {
    (w * h * 4) as usize
}
