//! A small software renderer: draws the placed keys into an ARGB8888 shm
//! buffer, with legible labels from an embedded 8x8 bitmap font (`font8x8`).
//!
//! This is deliberately un-themed and CPU-only — the kickoff needs keys that
//! are *legible and correctly positioned*, not Deck-grade animation. The full
//! theming model (CSS-custom-property-equivalent token table, glow/pulse
//! animations) is DEFERRED (osk-technology.md §4.7), and the research notes CPU
//! cairo/pango is the weak point for animated cursors — a GPU path is a later
//! decision. The renderer takes a list of [`Highlight`]s so the structure for
//! §4.5's concurrent per-source highlights (left pad, right pad, d-pad focus —
//! up to three at once) is already in place, even though this kickoff drives a
//! subset.

use font8x8::{UnicodeFonts, BASIC_FONTS};

use crate::layout::{Key, PlacedKey};

/// ARGB colour, non-premultiplied `0xAARRGGBB`. [`Canvas`] premultiplies on
/// write so translucent fills composite correctly on the layer surface.
#[derive(Clone, Copy)]
pub struct Color(pub u32);

impl Color {
    fn parts(self) -> (u32, u32, u32, u32) {
        let a = (self.0 >> 24) & 0xff;
        let r = (self.0 >> 16) & 0xff;
        let g = (self.0 >> 8) & 0xff;
        let b = self.0 & 0xff;
        (a, r, g, b)
    }
}

// Palette (kickoff, un-themed).
const BG: Color = Color(0xE6_11_14_1A); // translucent dark backdrop
const KEY_CHAR: Color = Color(0xFF_23_28_33);
const KEY_MOD: Color = Color(0xFF_2E_25_33); // modifiers/whitespace/meta tint
const KEY_BORDER: Color = Color(0xFF_3C_44_50);
const KEY_LABEL: Color = Color(0xFF_E6_EA_F0);
const HL_LEFT: Color = Color(0xFF_2E_6C_C8); // left-pad highlight (blue)
const HL_RIGHT: Color = Color(0xFF_C8_5A_2E); // right-pad highlight (orange)
const HL_FOCUS: Color = Color(0xFF_3C_A0_5A); // d-pad focus highlight (green)

/// Which input source a highlight comes from — mirrors osk-technology.md §4.5's
/// three concurrent highlight sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightKind {
    LeftPad,
    RightPad,
    Focus,
}

impl HighlightKind {
    fn color(self) -> Color {
        match self {
            HighlightKind::LeftPad => HL_LEFT,
            HighlightKind::RightPad => HL_RIGHT,
            HighlightKind::Focus => HL_FOCUS,
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

    fn border(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, c: Color) {
        self.fill_rect(x, y, w, thick, c);
        self.fill_rect(x, y + h - thick, w, thick, c);
        self.fill_rect(x, y, thick, h, c);
        self.fill_rect(x + w - thick, y, thick, h, c);
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
    /// integer glyph scale that fits both dimensions, so single-char keycaps get
    /// big glyphs and multi-char legends ("Enter", "Space") shrink to fit.
    fn text_centered(&mut self, text: &str, bx: i32, by: i32, bw: i32, bh: i32, c: Color) {
        let n = text.chars().count() as i32;
        if n == 0 {
            return;
        }
        let by_w = (bw - 4) / (n * 8);
        let by_h = (bh - 4) / 8;
        let scale = by_w.min(by_h).clamp(1, 3);
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

/// Draw a whole panel: background, then every placed key, with any highlights.
pub fn draw_panel(canvas: &mut Canvas, keys: &[Key], placed: &[PlacedKey], highlights: &[Highlight]) {
    canvas.fill_rect(0, 0, canvas.w, canvas.h, BG);

    for pk in placed {
        let key = &keys[pk.key];
        let (x, y, w, h) = (pk.rect.x as i32, pk.rect.y as i32, pk.rect.w as i32, pk.rect.h as i32);

        // Highlighted keys take their source colour; otherwise role tint.
        let hl = highlights.iter().find(|hh| hh.key == pk.key);
        let fill = match hl {
            Some(hh) => hh.kind.color(),
            None => match key.role {
                crate::layout::KeyRole::Char => KEY_CHAR,
                _ => KEY_MOD,
            },
        };
        canvas.fill_rect(x, y, w, h, fill);
        canvas.border(x, y, w, h, if hl.is_some() { 2 } else { 1 }, KEY_BORDER);
        canvas.text_centered(key.label, x, y, w, h, KEY_LABEL);
    }
}

/// Bytes needed for a `w x h` ARGB8888 buffer.
pub fn buffer_len(w: i32, h: i32) -> usize {
    (w * h * 4) as usize
}
