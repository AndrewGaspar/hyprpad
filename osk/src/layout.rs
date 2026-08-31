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

    /// Place all keys that belong in `panel` into a `panel_w` x `panel_h` area.
    ///
    /// The algorithm is identical for every panel — group by grid row, lay each
    /// row out left-to-right weighted by [`Key::units`], normalise to the panel
    /// width. Because a side-split column only *contains* its hand's keys
    /// (see [`PanelRole::holds`]), the very same routine yields the full grid
    /// for [`PanelRole::Bottom`] and a hand-local sub-grid for a column. That is
    /// the whole trick: one placement routine, three panels, two modes.
    pub fn place(&self, panel: PanelRole, panel_w: f32, panel_h: f32) -> Vec<PlacedKey> {
        let pad = 2.0_f32; // gap between keys, px. §4.9 item 11 wants gap:0 for
                           // haptics; a hairline gap here is a kickoff legibility
                           // choice, TODO revisit with real theming (§4.7).
        let rows = self.keyboard.rows();
        let row_h = panel_h / rows as f32;

        let mut placed = Vec::new();
        for row in 0..rows {
            // Keys of this row that live in this panel, in model order.
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
            let unit_w = panel_w / total_units;

            let mut x = 0.0_f32;
            let y = row as f32 * row_h;
            for &i in &indices {
                let w = self.keyboard.keys[i].units * unit_w;
                placed.push(PlacedKey {
                    key: i,
                    rect: Rect { x: x + pad, y: y + pad, w: w - 2.0 * pad, h: row_h - 2.0 * pad },
                });
                x += w;
            }
        }
        placed
    }

    /// The trackpad region for `pad` within a panel of the given size, as a
    /// fraction rect (osk-technology.md §4.1/§4.9 item 1).
    ///
    /// * [`LayoutMode::BottomDeck`]: left pad = leftmost 55%, right pad =
    ///   rightmost 55% (10% overlap band in the middle).
    /// * [`LayoutMode::SideSplit`]: each pad owns its whole column panel, so
    ///   the region is the full panel — the split already put each hand on its
    ///   own surface, giving each thumb its own physical side.
    pub fn trackpad_region(&self, pad: Pad, panel: PanelRole, w: f32, h: f32) -> Rect {
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
    pub fn map_cursor(&self, pad: Pad, panel: PanelRole, size: (f32, f32), pos: (f32, f32), scale: f32) -> (f32, f32) {
        let region = self.trackpad_region(pad, panel, size.0, size.1);
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

    #[test]
    fn side_split_columns_partition_by_hand() {
        let kb = Keyboard::qwerty();
        let eng = LayoutEngine::new(&kb, LayoutMode::SideSplit);
        let left = eng.place(PanelRole::LeftColumn, 400.0, 800.0);
        let right = eng.place(PanelRole::RightColumn, 400.0, 800.0);
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
    fn bottom_deck_places_every_key_in_one_panel() {
        let kb = Keyboard::qwerty();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let placed = eng.place(PanelRole::Bottom, 1280.0, 360.0);
        assert_eq!(placed.len(), kb.keys.len());
        // Keys stay inside the panel bounds.
        for p in &placed {
            assert!(p.rect.x >= 0.0 && p.rect.x + p.rect.w <= 1280.0 + 0.5);
            assert!(p.rect.y >= 0.0 && p.rect.y + p.rect.h <= 360.0 + 0.5);
        }
    }

    #[test]
    fn cursor_maps_absolute_center_and_extents() {
        let kb = Keyboard::qwerty();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        // Centre of the pad (0,0) → centre of the left region.
        let (x, y) = eng.map_cursor(Pad::Left, PanelRole::Bottom, (1000.0, 400.0), (0.0, 0.0), 1.0);
        assert!((x - 0.5 * 0.55 * 1000.0).abs() < 0.01);
        assert!((y - 200.0).abs() < 0.01);
        // Full right/up deflection → far corner of the region (Y inverted).
        let (x2, y2) = eng.map_cursor(Pad::Left, PanelRole::Bottom, (1000.0, 400.0), (1.0, 1.0), 1.0);
        assert!((x2 - 0.55 * 1000.0).abs() < 0.01);
        assert!(y2.abs() < 0.01);
    }

    #[test]
    fn hit_test_finds_the_key_under_a_point() {
        let kb = Keyboard::qwerty();
        let eng = LayoutEngine::new(&kb, LayoutMode::BottomDeck);
        let placed = eng.place(PanelRole::Bottom, 1000.0, 400.0);
        let first = placed[0];
        let cx = first.rect.x + first.rect.w / 2.0;
        let cy = first.rect.y + first.rect.h / 2.0;
        assert_eq!(LayoutEngine::hit_test(&placed, cx, cy), Some(0));
        // A point in the inter-key gap hits nothing (the §4.3 "no key" gap).
        assert_eq!(LayoutEngine::hit_test(&placed, first.rect.x - 1.0, cy), None);
    }
}
