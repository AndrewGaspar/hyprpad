//! The shared key/keycode model and the geometry engine that serves BOTH
//! presentation modes from it.
//!
//! # The one model, two geometries idea
//!
//! There is exactly one [`Keyboard`] — the QWERTY key set with, for every key,
//! its label, its **raw Linux/evdev keycode** (the same number the uinput
//! backend emits — see [`crate::output`]), its grid row, and crucially its
//! [`Hand`]. Everything mode-specific is *geometry* derived from that model by
//! [`LayoutEngine`]; the keys and keycodes never change between modes.
//!
//! * **Mode A — [`LayoutMode::BottomDeck`]**: the full grid laid out
//!   left-to-right / top-to-bottom in one bottom-docked panel. The two
//!   trackpad regions are the left 55% and right 55% of the surface with a 10%
//!   overlap in the middle (osk-technology.md §4.1/§4.9) — an *absolute* cursor
//!   overlay, exactly the Steam Deck model.
//!
//! * **Mode B — [`LayoutMode::SideSplit`]**: the SAME keys partitioned by
//!   [`Hand`] into two edge-docked columns — left-hand keys dock the left
//!   screen edge, right-hand keys the right edge, workspace content reflows into
//!   the centre strip between them. The **left trackpad addresses the left
//!   column, the right trackpad the right column**, so the Deck's two-region
//!   dual-trackpad model maps straight onto physical screen geography (each
//!   thumb → its own side). `Hand` is the single field that makes this fall out
//!   of the shared model.
//!
//! The keycodes and the grid come from osk-technology.md §4.6 (code-verified
//! against the shipped Steam Deck OSK bundle).

use crate::theme::Geom;

/// Which hand — and therefore which trackpad — owns a key.
///
/// This is the ergonomic pivot of the whole model. In [`LayoutMode::SideSplit`]
/// it *is* the column assignment: `Left` keys go to the left-edge panel
/// addressed by the left pad, `Right` keys to the right-edge panel addressed by
/// the right pad. In [`LayoutMode::BottomDeck`] the two pad regions physically
/// overlap in the middle, but the hand metadata is still carried for parity and
/// for future per-hand tinting/haptics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
    /// Spans both hands (e.g. the space bar); placed in whichever column the
    /// side-split assigns spanning keys to (currently the left column's base).
    Either,
}

/// What a key does when committed. Character keys type their keycode; the rest
/// are modifiers/whitespace/meta whose full behaviour (sticky shift, caps,
/// long-press repeat, layer switch) is only partially implemented in this
/// kickoff — see the TODOs in [`crate::app`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRole {
    /// A character key: typing it emits `keycode` (with the active shift level).
    Char,
    /// Shift. Sticky-shift state machine is stubbed (osk-technology.md §4.6).
    Shift,
    /// Caps Lock toggle.
    Caps,
    Backspace,
    Enter,
    Tab,
    Space,
    /// A meta key with no direct evdev keycode — layer switch, emoji, arrows,
    /// close, etc. Behaviour deferred (osk-technology.md §4.6 "Layers").
    Meta,
}

/// One key in the model: label + evdev keycode + grid placement + hand.
#[derive(Clone, Debug)]
pub struct Key {
    /// Legend drawn on the keycap in the unshifted state.
    pub label: &'static str,
    /// Legend/character produced when Shift is active. `None` for keys with no
    /// distinct shifted form. (Rendering of the shifted legend is deferred.)
    pub shifted: Option<&'static str>,
    /// **Raw Linux/evdev keycode** (`KEY_*`) — fed verbatim to the uinput
    /// backend. `0` means "no direct keycode" (meta keys).
    pub keycode: u16,
    pub role: KeyRole,
    pub hand: Hand,
    /// Grid row, 0 (number row) .. 4 (bottom/meta row).
    pub row: u8,
    /// Width in key-units (1.0 = a normal letter key). Wider keys (Backspace,
    /// Enter, Space, Shift) use >1.0; matches the Deck grid proportions loosely.
    pub units: f32,
}

impl Key {
    const fn c(label: &'static str, shifted: &'static str, keycode: u16, hand: Hand, row: u8) -> Key {
        Key { label, shifted: Some(shifted), keycode, role: KeyRole::Char, hand, row, units: 1.0 }
    }
}

/// The immutable key set. Built once by [`Keyboard::qwerty`]; the geometry
/// engine borrows it to place keys for whichever mode is showing.
pub struct Keyboard {
    pub keys: Vec<Key>,
}

impl Keyboard {
    /// The QWERTY layout, grid + PC scancodes straight from osk-technology.md
    /// §4.6:
    ///
    /// ```text
    /// r0: `~ 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) -_ =+  Backspace
    /// r1: Tab q w e r t y u i o p [{ ]} \|
    /// r2: Caps a s d f g h j k l ;: '"  Enter
    /// r3: LShift z x c v b n m ,< .> /?  RShift
    /// r4: [layer] Space  ← →           (meta row, partial)
    /// keycodes: [41,2..14][15..27,43][58,30..40,28][42,44..54][…,57,105,106]
    /// ```
    ///
    /// The QWERTY hand split (left = `…QWERT/ASDFG/ZXCVB`, right =
    /// `YUIOP/HJKL/NM…`) is the standard touch-typing split, which is exactly
    /// the left-column / right-column division Mode B needs.
    #[allow(clippy::vec_init_then_push)] // grouped push-per-row is the readable
    // shape here — the rows mirror the §4.6 grid one line at a time.
    pub fn qwerty() -> Keyboard {
        use Hand::{Left, Right};
        use KeyRole::*;

        let mut keys = Vec::new();

        // --- row 0: number row -------------------------------------------------
        keys.push(Key::c("`", "~", 41, Left, 0));
        keys.push(Key::c("1", "!", 2, Left, 0));
        keys.push(Key::c("2", "@", 3, Left, 0));
        keys.push(Key::c("3", "#", 4, Left, 0));
        keys.push(Key::c("4", "$", 5, Left, 0));
        keys.push(Key::c("5", "%", 6, Left, 0));
        keys.push(Key::c("6", "^", 7, Right, 0));
        keys.push(Key::c("7", "&", 8, Right, 0));
        keys.push(Key::c("8", "*", 9, Right, 0));
        keys.push(Key::c("9", "(", 10, Right, 0));
        keys.push(Key::c("0", ")", 11, Right, 0));
        keys.push(Key::c("-", "_", 12, Right, 0));
        keys.push(Key::c("=", "+", 13, Right, 0));
        keys.push(Key { label: "Bksp", shifted: None, keycode: 14, role: Backspace, hand: Right, row: 0, units: 1.8 });

        // --- row 1: QWERTY top -------------------------------------------------
        keys.push(Key { label: "Tab", shifted: None, keycode: 15, role: Tab, hand: Left, row: 1, units: 1.5 });
        keys.push(Key::c("q", "Q", 16, Left, 1));
        keys.push(Key::c("w", "W", 17, Left, 1));
        keys.push(Key::c("e", "E", 18, Left, 1));
        keys.push(Key::c("r", "R", 19, Left, 1));
        keys.push(Key::c("t", "T", 20, Left, 1));
        keys.push(Key::c("y", "Y", 21, Right, 1));
        keys.push(Key::c("u", "U", 22, Right, 1));
        keys.push(Key::c("i", "I", 23, Right, 1));
        keys.push(Key::c("o", "O", 24, Right, 1));
        keys.push(Key::c("p", "P", 25, Right, 1));
        keys.push(Key::c("[", "{", 26, Right, 1));
        keys.push(Key::c("]", "}", 27, Right, 1));
        keys.push(Key::c("\\", "|", 43, Right, 1));

        // --- row 2: home row ---------------------------------------------------
        keys.push(Key { label: "Caps", shifted: None, keycode: 58, role: Caps, hand: Left, row: 2, units: 1.8 });
        keys.push(Key::c("a", "A", 30, Left, 2));
        keys.push(Key::c("s", "S", 31, Left, 2));
        keys.push(Key::c("d", "D", 32, Left, 2));
        keys.push(Key::c("f", "F", 33, Left, 2));
        keys.push(Key::c("g", "G", 34, Left, 2));
        keys.push(Key::c("h", "H", 35, Right, 2));
        keys.push(Key::c("j", "J", 36, Right, 2));
        keys.push(Key::c("k", "K", 37, Right, 2));
        keys.push(Key::c("l", "L", 38, Right, 2));
        keys.push(Key::c(";", ":", 39, Right, 2));
        keys.push(Key::c("'", "\"", 40, Right, 2));
        keys.push(Key { label: "Enter", shifted: None, keycode: 28, role: Enter, hand: Right, row: 2, units: 1.8 });

        // --- row 3: bottom row -------------------------------------------------
        keys.push(Key { label: "Shift", shifted: None, keycode: 42, role: Shift, hand: Left, row: 3, units: 2.2 });
        keys.push(Key::c("z", "Z", 44, Left, 3));
        keys.push(Key::c("x", "X", 45, Left, 3));
        keys.push(Key::c("c", "C", 46, Left, 3));
        keys.push(Key::c("v", "V", 47, Left, 3));
        keys.push(Key::c("b", "B", 48, Left, 3));
        keys.push(Key::c("n", "N", 49, Right, 3));
        keys.push(Key::c("m", "M", 50, Right, 3));
        keys.push(Key::c(",", "<", 51, Right, 3));
        keys.push(Key::c(".", ">", 52, Right, 3));
        keys.push(Key::c("/", "?", 53, Right, 3));
        keys.push(Key { label: "Shift", shifted: None, keycode: 54, role: Shift, hand: Right, row: 3, units: 2.2 });

        // --- row 4: meta / space row (partial; §4.6 "Layers"/arrows) ----------
        // Meta keys (layer switch, emoji, etc.) are stubbed with keycode 0.
        keys.push(Key { label: "?123", shifted: None, keycode: 0, role: Meta, hand: Left, row: 4, units: 2.0 });
        keys.push(Key { label: "Space", shifted: None, keycode: 57, role: Space, hand: Hand::Either, row: 4, units: 6.0 });
        keys.push(Key { label: "<", shifted: None, keycode: 105, role: Meta, hand: Right, row: 4, units: 1.0 }); // KEY_LEFT
        keys.push(Key { label: ">", shifted: None, keycode: 106, role: Meta, hand: Right, row: 4, units: 1.0 }); // KEY_RIGHT

        Keyboard { keys }
    }

    /// The number of grid rows in the model (0..=`rows()-1`).
    pub fn rows(&self) -> u8 {
        self.keys.iter().map(|k| k.row).max().map_or(0, |m| m + 1)
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// A rectangle in **panel-local pixels** (origin = top-left of the panel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// A key placed at a concrete pixel rectangle within a panel.
#[derive(Clone, Copy, Debug)]
pub struct PlacedKey {
    /// Index into [`Keyboard::keys`].
    pub key: usize,
    pub rect: Rect,
}

/// The two presentation modes. Both always render on the OVERLAY layer tier
/// (osk-technology.md §2.1, non-negotiable); they differ only in geometry and
/// in which panels exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    /// One bottom-docked full-width panel (horizontal Deck-style).
    BottomDeck,
    /// Two edge-docked columns (vertical dual-region split); workspace content
    /// reflows into the centre.
    SideSplit,
}

/// Which physical panel we are placing keys into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelRole {
    /// The single bottom panel of [`LayoutMode::BottomDeck`].
    Bottom,
    /// The left-edge column of [`LayoutMode::SideSplit`] (left-hand keys).
    LeftColumn,
    /// The right-edge column of [`LayoutMode::SideSplit`] (right-hand keys).
    RightColumn,
}

impl PanelRole {
    /// The panels a mode is composed of, in draw order.
    pub fn for_mode(mode: LayoutMode) -> &'static [PanelRole] {
        match mode {
            LayoutMode::BottomDeck => &[PanelRole::Bottom],
            LayoutMode::SideSplit => &[PanelRole::LeftColumn, PanelRole::RightColumn],
        }
    }

    /// Does this panel hold the given key? Bottom holds everything; the columns
    /// partition by hand (spanning "Either" keys land in the left column).
    fn holds(self, key: &Key) -> bool {
        match self {
            PanelRole::Bottom => true,
            PanelRole::LeftColumn => matches!(key.hand, Hand::Left | Hand::Either),
            PanelRole::RightColumn => key.hand == Hand::Right,
        }
    }
}

/// Which trackpad (thumb). The daemon forwards each pad's absolute position and
/// click independently; the two are always live at once (osk-technology.md
/// §4.5 — concurrent modality, no mode switching).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pad {
    Left,
    Right,
}

/// Places the shared [`Keyboard`] into pixel rectangles for a given panel and
/// size, and hit-tests trackpad cursors against the result.
pub struct LayoutEngine<'k> {
    keyboard: &'k Keyboard,
    pub mode: LayoutMode,
}

impl<'k> LayoutEngine<'k> {
    pub fn new(keyboard: &'k Keyboard, mode: LayoutMode) -> Self {
        LayoutEngine { keyboard, mode }
    }

    /// The rows this panel actually holds, each as `(row, key indices, total
    /// units)`, in draw order. Empty rows are dropped. Shared by
    /// [`Self::content_size`] and [`Self::place`] so sizing and placement can
    /// never disagree.
    fn panel_rows(&self, panel: PanelRole) -> Vec<(u8, Vec<usize>, f32)> {
        let mut out = Vec::new();
        for row in 0..self.keyboard.rows() {
            let indices: Vec<usize> = self
                .keyboard
                .keys
                .iter()
                .enumerate()
                .filter(|(_, k)| k.row == row && panel.holds(k))
                .map(|(i, _)| i)
                .collect();
            if indices.is_empty() {
                continue;
            }
            let total_units: f32 = indices.iter().map(|&i| self.keyboard.keys[i].units).sum();
            out.push((row, indices, total_units));
        }
        out
    }

    /// The **content-sized** pixel dimensions of `panel` for the given geometry
    /// tokens — the whole size-to-content mechanism. The surface is only as big
    /// as the keys need, derived purely from [`Geom`] and the key grid, **never**
    /// from the screen size.
    ///
    /// A key of `u` units is `u*key_size + (u-1)*gap` px wide (it absorbs the
    /// gaps a run of unit-keys would have had), and rows are separated by `gap`,
    /// which collapses a row's width to a clean function of its total units:
    ///
    /// ```text
    /// row_w   = key_size*total_units + gap*(total_units - 1)
    /// content_w = max row_w over the panel's rows
    /// content_h = key_size*n_rows    + gap*(n_rows    - 1)
    /// panel     = (content_w, content_h) + 2*margin on each axis
    /// ```
    pub fn content_size(&self, panel: PanelRole, g: &Geom) -> (f32, f32) {
        let rows = self.panel_rows(panel);
        if rows.is_empty() {
            return (2.0 * g.margin, 2.0 * g.margin);
        }
        let n_rows = rows.len() as f32;
        let max_units = rows.iter().map(|(_, _, u)| *u).fold(0.0_f32, f32::max);
        let content_w = g.key_size * max_units + g.gap * (max_units - 1.0);
        let content_h = g.key_size * n_rows + g.gap * (n_rows - 1.0);
        (content_w + 2.0 * g.margin, content_h + 2.0 * g.margin)
    }

    /// Place all keys that belong in `panel` at **absolute** pixel rectangles
    /// sized by `g` (not stretched to a panel size). Each row is centred
    /// horizontally within the content width, so the placement exactly fills the
    /// [`Self::content_size`] box: keys span `[margin, panel_w-margin]` on the
    /// widest row and `[margin, panel_h-margin]` vertically.
    ///
    /// The algorithm is identical for every panel — group by grid row, lay each
    /// row out left-to-right weighted by [`Key::units`]. Because a side-split
    /// column only *contains* its hand's keys (see [`PanelRole::holds`]), the
    /// very same routine yields the full grid for [`PanelRole::Bottom`] and a
    /// hand-local sub-grid for a column. One placement routine, three panels,
    /// two modes.
    pub fn place(&self, panel: PanelRole, g: &Geom) -> Vec<PlacedKey> {
        let rows = self.panel_rows(panel);
        let (content_w, _) = {
            let (w, h) = self.content_size(panel, g);
            (w - 2.0 * g.margin, h - 2.0 * g.margin)
        };

        let key_w = |units: f32| units * g.key_size + (units - 1.0) * g.gap;

        let mut placed = Vec::new();
        for (ord, (_, indices, total_units)) in rows.iter().enumerate() {
            let row_w = key_w(*total_units);
            // Centre this row within the content width.
            let mut x = g.margin + (content_w - row_w) / 2.0;
            let y = g.margin + ord as f32 * (g.key_size + g.gap);
            for &i in indices {
                let w = key_w(self.keyboard.keys[i].units);
                placed.push(PlacedKey {
                    key: i,
                    rect: Rect { x, y, w, h: g.key_size },
                });
                x += w + g.gap;
            }
        }
        placed
    }

    /// The trackpad region for `pad` within `panel`, as an absolute pixel rect
    /// over the panel's **content size** (osk-technology.md §4.1/§4.9 item 1).
    ///
    /// This is the load-bearing half of the `[-1,1] -> key` contract: the region
    /// is expressed as a fraction of the (now content-sized) panel, so a pad's
    /// full normalized range still spans exactly the keys after the surface
    /// shrinks — the fractions are unchanged, only the panel got smaller.
    ///
    /// * [`LayoutMode::BottomDeck`]: left pad = leftmost 55%, right pad =
    ///   rightmost 55% (10% overlap band in the middle) — together they cover
    ///   the full width, so every key is reachable by at least one pad.
    /// * [`LayoutMode::SideSplit`]: each pad owns its whole column panel, so
    ///   the region is the full panel — the split already put each hand on its
    ///   own surface, giving each thumb its own physical side.
    pub fn trackpad_region(&self, pad: Pad, panel: PanelRole, g: &Geom) -> Rect {
        let (w, h) = self.content_size(panel, g);
        match self.mode {
            LayoutMode::BottomDeck => match pad {
                Pad::Left => Rect { x: 0.0, y: 0.0, w: 0.55 * w, h },
                Pad::Right => Rect { x: 0.45 * w, y: 0.0, w: 0.55 * w, h },
            },
            LayoutMode::SideSplit => {
                // A pad only addresses its own column; if asked about the other
                // panel it gets an empty region.
                let owns = matches!(
                    (pad, panel),
                    (Pad::Left, PanelRole::LeftColumn) | (Pad::Right, PanelRole::RightColumn)
                );
                if owns {
                    Rect { x: 0.0, y: 0.0, w, h }
                } else {
                    Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }
                }
            }
        }
    }

    /// Map a pad's absolute normalized position `(nx, ny)` in `[-1, 1]` (as the
    /// controller reports it) to a panel-local pixel point, following the
    /// code-verified Deck formula (osk-technology.md §4.1):
    ///
    /// ```text
    /// t = 0.5 * (1 + clamp(nx * scale, -1, 1))   // X within region
    /// o = 0.5 * (1 - clamp(ny * scale, -1, 1))   // Y within region (inverted)
    /// point = region.top_left + (region.w * t, region.h * o)
    /// ```
    ///
    /// `scale` is "Trackpad Sensitivity": a gain applied *before* clamping, so
    /// it expands reach, not cursor speed. This is pure absolute position —
    /// no velocity, no accumulator.
    ///
    /// The mapping targets the panel's content-sized [`Self::trackpad_region`],
    /// so it and [`Self::place`] always agree regardless of what the compositor
    /// reports for the surface — preserving the `[-1,1] -> key` contract.
    pub fn map_cursor(&self, pad: Pad, panel: PanelRole, g: &Geom, pos: (f32, f32), scale: f32) -> (f32, f32) {
        let region = self.trackpad_region(pad, panel, g);
        let t = 0.5 * (1.0 + (pos.0 * scale).clamp(-1.0, 1.0));
        let o = 0.5 * (1.0 - (pos.1 * scale).clamp(-1.0, 1.0));
        (region.x + region.w * t, region.y + region.h * o)
    }

    /// Hit-test a panel-local pixel point against placed keys: the key whose
    /// rect contains the point (osk-technology.md §4.1 — literal
    /// `elementFromPoint` hit-testing filtered on `data-key`). Returns the index
    /// into `placed`.
    pub fn hit_test(placed: &[PlacedKey], px: f32, py: f32) -> Option<usize> {
        placed.iter().position(|p| p.rect.contains(px, py))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwerty_keycodes_match_the_research_grid() {
        let kb = Keyboard::qwerty();
        let find = |label: &str| kb.keys.iter().find(|k| k.label == label).unwrap();
        // §4.6 spot checks — these keycodes are the raw evdev codes uinput emits.
        assert_eq!(find("`").keycode, 41);
        assert_eq!(find("q").keycode, 16);
        assert_eq!(find("a").keycode, 30);
        assert_eq!(find("z").keycode, 44);
        assert_eq!(find("Enter").keycode, 28);
        assert_eq!(find("Bksp").keycode, 14);
        assert_eq!(find("Space").keycode, 57);
        assert_eq!(find("Tab").keycode, 15);
        assert_eq!(find("Caps").keycode, 58);
    }

    #[test]
    fn hand_split_is_the_standard_qwerty_division() {
        let kb = Keyboard::qwerty();
        let hand = |label: &str| kb.keys.iter().find(|k| k.label == label).unwrap().hand;
        // The T/Y, G/H, B/N boundary is what makes Mode B's two columns work.
        assert_eq!(hand("t"), Hand::Left);
        assert_eq!(hand("y"), Hand::Right);
        assert_eq!(hand("g"), Hand::Left);
        assert_eq!(hand("h"), Hand::Right);
        assert_eq!(hand("b"), Hand::Left);
        assert_eq!(hand("n"), Hand::Right);
    }

    fn geom() -> Geom {
        crate::theme::Theme::default().geom
    }

    #[test]
    fn side_split_columns_partition_by_hand() {
        let kb = Keyboard::qwerty();
        let g = geom();
        let eng = LayoutEngine::new(&kb, LayoutMode::SideSplit);
        let left = eng.place(PanelRole::LeftColumn, &g);
        let right = eng.place(PanelRole::RightColumn, &g);
        let has = |placed: &[PlacedKey], label: &str| {
            placed.iter().any(|p| kb.keys[p.key].label == label)
        };
        // Left-hand keys only on the left, right-hand keys only on the right.
        assert!(has(&left, "q") && has(&left, "t") && has(&left, "b"));
        assert!(!has(&left, "y") && !has(&left, "p"));
        assert!(has(&right, "y") && has(&right, "p") && has(&right, "Enter"));
        assert!(!has(&right, "q") && !has(&right, "a"));
        // Every key is placed exactly once across the two columns.
        assert_eq!(left.len() + right.len(), kb.keys.len());
    }

    #[test]
    fn keys_are_roughly_square_and_content_sized() {
        let kb = Keyboard::qwerty();
        let g = geom();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let placed = eng.place(PanelRole::Bottom, &g);
        // A 1-unit character keycap is exactly key_size x key_size (square).
        let a = placed.iter().find(|p| kb.keys[p.key].label == "a").unwrap();
        assert!((a.rect.w - g.key_size).abs() < 0.001);
        assert!((a.rect.h - g.key_size).abs() < 0.001);

        // The panel is content-sized: its width is far below a full 2048 screen,
        // and every key sits inside the computed content box (with margin).
        let (pw, ph) = eng.content_size(PanelRole::Bottom, &g);
        assert!(pw < 1600.0, "bottom content width {pw} should be well under screen width");
        for p in &placed {
            assert!(p.rect.x >= g.margin - 0.5 && p.rect.x + p.rect.w <= pw - g.margin + 0.5);
            assert!(p.rect.y >= g.margin - 0.5 && p.rect.y + p.rect.h <= ph - g.margin + 0.5);
        }
    }

    #[test]
    fn cursor_maps_absolute_center_and_extents() {
        let kb = Keyboard::qwerty();
        let g = geom();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let (w, h) = eng.content_size(PanelRole::Bottom, &g);
        // Centre of the pad (0,0) → centre of the left region.
        let (x, y) = eng.map_cursor(Pad::Left, PanelRole::Bottom, &g, (0.0, 0.0), 1.0);
        assert!((x - 0.5 * 0.55 * w).abs() < 0.01);
        assert!((y - 0.5 * h).abs() < 0.01);
        // Full right/up deflection → far corner of the region (Y inverted).
        let (x2, y2) = eng.map_cursor(Pad::Left, PanelRole::Bottom, &g, (1.0, 1.0), 1.0);
        assert!((x2 - 0.55 * w).abs() < 0.01);
        assert!(y2.abs() < 0.01);
    }

    /// The CRITICAL contract (task): after resizing/re-centering the key area,
    /// a pad's full `[-1,1]` normalized range must still map onto its key
    /// cluster — every key remains reachable by the appropriate pad.
    ///
    /// * Split mode: each pad owns its whole column, so *every* key centre in
    ///   that column must be inside the pad's reachable rect.
    /// * Bottom mode: the left pad covers the leftmost 55% and the right pad the
    ///   rightmost 55% (the §4.1 geometry, with the 10% overlap band), and their
    ///   union must cover *every* key centre so nothing is unreachable.
    #[test]
    fn pad_range_covers_the_key_cluster_after_resize() {
        let kb = Keyboard::qwerty();
        let g = geom();

        // Reachable rect for a pad = the four normalized corners mapped in.
        let reach = |eng: &LayoutEngine, pad: Pad, panel: PanelRole| -> Rect {
            let c = |nx: f32, ny: f32| eng.map_cursor(pad, panel, &g, (nx, ny), 1.0);
            let pts = [c(-1.0, -1.0), c(1.0, -1.0), c(-1.0, 1.0), c(1.0, 1.0)];
            let minx = pts.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
            let maxx = pts.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
            let miny = pts.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
            let maxy = pts.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
            Rect { x: minx, y: miny, w: maxx - minx, h: maxy - miny }
        };
        let center = |p: &PlacedKey| (p.rect.x + p.rect.w / 2.0, p.rect.y + p.rect.h / 2.0);

        // --- Split: every key centre in a column is reachable by its pad. -----
        let eng = LayoutEngine::new(&kb, LayoutMode::SideSplit);
        for (pad, panel) in [(Pad::Left, PanelRole::LeftColumn), (Pad::Right, PanelRole::RightColumn)] {
            let r = reach(&eng, pad, panel);
            for p in eng.place(panel, &g) {
                let (cx, cy) = center(&p);
                assert!(r.contains(cx, cy), "split {:?} unreachable by {:?}", kb.keys[p.key].label, pad);
            }
        }

        // --- Bottom: 55%/55% regions, and their UNION covers every key. -------
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let (w, _) = eng.content_size(PanelRole::Bottom, &g);
        let rl = reach(&eng, Pad::Left, PanelRole::Bottom);
        let rr = reach(&eng, Pad::Right, PanelRole::Bottom);
        // The §4.1 region geometry survived the resize.
        assert!((rl.x).abs() < 0.01 && (rl.w - 0.55 * w).abs() < 0.5, "left pad != leftmost 55%");
        assert!((rr.x - 0.45 * w).abs() < 0.5 && (rr.x + rr.w - w).abs() < 0.5, "right pad != rightmost 55%");
        for p in eng.place(PanelRole::Bottom, &g) {
            let (cx, cy) = center(&p);
            assert!(
                rl.contains(cx, cy) || rr.contains(cx, cy),
                "bottom key {:?} centre unreachable by either pad",
                kb.keys[p.key].label
            );
        }
    }

    #[test]
    fn hit_test_finds_the_key_under_a_point() {
        let kb = Keyboard::qwerty();
        let g = geom();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let placed = eng.place(PanelRole::Bottom, &g);
        let first = placed[0];
        let cx = first.rect.x + first.rect.w / 2.0;
        let cy = first.rect.y + first.rect.h / 2.0;
        assert_eq!(LayoutEngine::hit_test(&placed, cx, cy), Some(0));
        // A point in the inter-key gap hits nothing (the §4.3 "no key" gap).
        assert_eq!(LayoutEngine::hit_test(&placed, first.rect.x - 1.0, cy), None);
    }
}
