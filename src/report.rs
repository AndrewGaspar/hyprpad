//! Decoder for the 2026 Steam Controller's vendor input report 0x42.
//!
//! Layout established empirically; see docs/03-hardware-findings.md
//! ("Layout — fully decoded"). 54 bytes: id, counter, four button/touch
//! bitfield bytes, then little-endian analog channels. Bytes 30+ carry the
//! IMU and stream only after an enable feature-report (Steam sends one);
//! they are untouched here.

/// Buttons and touch flags, one bit each, in a stable order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Button {
    A,
    B,
    X,
    Y,
    QuickAccess,
    R3,
    Menu,
    GripR4,
    GripR5,
    BumperR1,
    DpadDown,
    DpadRight,
    DpadLeft,
    DpadUp,
    View,
    L3,
    Steam,
    GripL4,
    GripL5,
    BumperL1,
    Cap0, // capacitive cluster: fire on hand contact; individual
    PadRightTouch,
    PadRightClick,
    TriggerR2Full,
    Cap1, //   assignment tentative (docs/03, capacitive rows)
    PadLeftTouch,
    PadLeftClick,
    TriggerL2Full,
    Cap2,
    Cap3,
}

/// (byte index, bit) for each `Button`, aligned with the enum order.
const BUTTON_BITS: [(usize, u8, Button); 30] = [
    (2, 0, Button::A),
    (2, 1, Button::B),
    (2, 2, Button::X),
    (2, 3, Button::Y),
    (2, 4, Button::QuickAccess),
    (2, 5, Button::R3),
    (2, 6, Button::Menu),
    (2, 7, Button::GripR4),
    (3, 0, Button::GripR5),
    (3, 1, Button::BumperR1),
    (3, 2, Button::DpadDown),
    (3, 3, Button::DpadRight),
    (3, 4, Button::DpadLeft),
    (3, 5, Button::DpadUp),
    (3, 6, Button::View),
    (3, 7, Button::L3),
    (4, 0, Button::Steam),
    (4, 1, Button::GripL4),
    (4, 2, Button::GripL5),
    (4, 3, Button::BumperL1),
    (4, 4, Button::Cap0),
    (4, 5, Button::PadRightTouch),
    (4, 6, Button::PadRightClick),
    (4, 7, Button::TriggerR2Full),
    (5, 0, Button::Cap1),
    (5, 1, Button::PadLeftTouch),
    (5, 2, Button::PadLeftClick),
    (5, 3, Button::TriggerL2Full),
    (5, 4, Button::Cap2),
    (5, 5, Button::Cap3),
];

/// One decoded frame of controller state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Frame {
    pub counter: u8,
    /// Button bits in `Button` enum order (bit i == BUTTON_BITS[i]).
    pub buttons: u32,
    pub l2: u16, // 0..=32767
    pub r2: u16,
    pub left_stick: (i16, i16),  // +x right, +y up; small idle offset
    pub right_stick: (i16, i16),
    pub left_pad: Pad,
    pub right_pad: Pad,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pad {
    pub x: i16,
    pub y: i16,
    pub force: u16, // 0..~12550, spikes on physical click
}

impl Frame {
    /// Decode a raw hidraw report. Returns None unless it is a 54-byte 0x42.
    pub fn decode(raw: &[u8]) -> Option<Frame> {
        if raw.len() != 54 || raw[0] != 0x42 {
            return None;
        }
        let i16le = |i: usize| i16::from_le_bytes([raw[i], raw[i + 1]]);
        let u16le = |i: usize| u16::from_le_bytes([raw[i], raw[i + 1]]);
        let mut buttons = 0u32;
        for (i, &(byte, bit, _)) in BUTTON_BITS.iter().enumerate() {
            if raw[byte] & (1 << bit) != 0 {
                buttons |= 1 << i;
            }
        }
        Some(Frame {
            counter: raw[1],
            buttons,
            l2: u16le(6),
            r2: u16le(8),
            left_stick: (i16le(10), i16le(12)),
            right_stick: (i16le(14), i16le(16)),
            left_pad: Pad { x: i16le(18), y: i16le(20), force: u16le(22) },
            right_pad: Pad { x: i16le(24), y: i16le(26), force: u16le(28) },
        })
    }

    pub fn pressed(&self, b: Button) -> bool {
        let idx = BUTTON_BITS.iter().position(|&(_, _, bb)| bb == b).unwrap();
        self.buttons & (1 << idx) != 0
    }

    /// The same frame with `buttons` reported as *not* pressed.
    ///
    /// How a consumed press is spelled: a button whose press was taken by
    /// something upstream — a transient mode's exit
    /// ([`crate::mode::ModeEngine::note_press`]) — is masked out of the frame
    /// the bare-button layer sees, so it produces no press edge for a `Hold`
    /// binding and no [`edges_down`](Self::edges_down) for a `Fire` one. The
    /// *unmasked* frame is what becomes the next frame's `prev`, so a button
    /// still held after its press was eaten is simply down on both frames and
    /// never becomes an edge again.
    pub fn without(mut self, buttons: &[Button]) -> Frame {
        for &b in buttons {
            if let Some(i) = BUTTON_BITS.iter().position(|&(_, _, bb)| bb == b) {
                self.buttons &= !(1 << i);
            }
        }
        self
    }

    /// Buttons newly pressed relative to `prev`.
    pub fn edges_down(&self, prev: &Frame) -> impl Iterator<Item = Button> + '_ {
        let changed = self.buttons & !prev.buttons;
        BUTTON_BITS
            .iter()
            .enumerate()
            .filter(move |(i, _)| changed & (1 << i) != 0)
            .map(|(_, &(_, _, b))| b)
    }

    /// Buttons newly released relative to `prev`.
    pub fn edges_up(&self, prev: &Frame) -> impl Iterator<Item = Button> + '_ {
        let changed = !self.buttons & prev.buttons;
        BUTTON_BITS
            .iter()
            .enumerate()
            .filter(move |(i, _)| changed & (1 << i) != 0)
            .map(|(_, &(_, _, b))| b)
    }
}
