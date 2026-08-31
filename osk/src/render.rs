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

use crate::layout::{Key, PlacedKey};
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

/// Draw a whole panel: themed background, then every placed key, with any
/// highlights. All colours and dimensions are read from `theme`.
pub fn draw_panel(canvas: &mut Canvas, theme: &Theme, keys: &[Key], placed: &[PlacedKey], highlights: &[Highlight]) {
    let col = &theme.colors;
    let corner = theme.geom.corner as i32;
    let border = theme.geom.border_width.max(1.0) as i32;

    canvas.fill_rect(0, 0, canvas.w, canvas.h, col.surface_bg);

    for pk in placed {
        let key = &keys[pk.key];
        let (x, y, w, h) = (pk.rect.x as i32, pk.rect.y as i32, pk.rect.w as i32, pk.rect.h as i32);

        // Highlighted keys take their source colour; otherwise role tint.
        let hl = highlights.iter().find(|hh| hh.key == pk.key);
        let fill = match hl {
            Some(hh) => hh.kind.color(theme),
            None => match key.role {
                crate::layout::KeyRole::Char => col.key_fill,
                _ => col.key_mod_fill,
            },
        };
        // Border via a rounded-rect underlay, then the fill inset by the border
        // width — a themeable stroke that respects the corner radius.
        let bw = if hl.is_some() { border.max(2) } else { border };
        canvas.fill_round_rect(x, y, w, h, corner, col.key_border);
        canvas.fill_round_rect(x + bw, y + bw, w - 2 * bw, h - 2 * bw, (corner - bw).max(0), fill);
        canvas.text_centered(key.label, (x, y, w, h), theme.font.scale_max, col.key_label);
    }
}

/// Bytes needed for a `w x h` ARGB8888 buffer.
pub fn buffer_len(w: i32, h: i32) -> usize {
    (w * h * 4) as usize
}
