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

/// Which backend produced a [`Frame`] — and therefore what the frame's empty
/// fields *mean*.
///
/// The daemon's vocabulary is the puck's: face buttons, grips, two sticks, two
/// trackpads. A second backend ([`crate::evdev`]) fills the same struct from an
/// ordinary Linux gamepad, which has no trackpads and no per-pad actuators. The
/// difference is not "the pads happen to be untouched this frame" — it is "this
/// device has no pads at all", and every consumer that would otherwise idle
/// forever on a touch bit needs to be able to tell the two apart.
///
/// [`Frame`] derives `Default`, and [`Source::Puck`] is the default, so every
/// existing test and the whole puck path are byte-identical to before this
/// existed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Source {
    /// The 2026 Steam Controller puck, decoded from hidraw ([`Frame::decode`]).
    #[default]
    Puck,
    /// A generic Linux gamepad read over evdev ([`crate::evdev`]) — the Xbox
    /// Elite Series 2 is the one this was built for. Phase 2's Bluetooth hidraw
    /// sidecar (`docs/design/xbox-elite.md`) will be a third variant beside it,
    /// which is why this is an enum and not a bool.
    Evdev,
}

/// The cheat sheet's layout id for the puck —
/// `shell/hyprpad.cheatsheet/layouts/steam-controller-2026.json`.
pub const LAYOUT_PUCK: &str = "steam-controller-2026";

/// The cheat sheet's layout id for a gamepad read over evdev. Every pad the
/// second backend adopts draws as an Xbox-shaped one: two asymmetric sticks, a
/// face diamond, four paddles.
pub const LAYOUT_XBOX_ELITE_2: &str = "xbox-elite-2";

impl Source {
    /// Which cheat-sheet layout this source draws as.
    ///
    /// One string is the whole of "which controller is the reader holding":
    /// the widget names no controller anywhere, it just loads
    /// `layouts/<id>.json`.
    pub const fn layout(self) -> &'static str {
        match self {
            Source::Puck => LAYOUT_PUCK,
            Source::Evdev => LAYOUT_XBOX_ELITE_2,
        }
    }

    /// The inverse of [`layout`](Self::layout): what a published layout id says
    /// about the controller.
    ///
    /// Used by `hyprpad bindings`, which has no device and no daemon state —
    /// only the layout id the daemon published — but still needs to know
    /// whether the reader has trackpads, because the ambient cursor and scroll
    /// rows say different things if they do not. An unknown id is read as the
    /// puck, the safe default for a config written against it.
    pub fn from_layout(id: &str) -> Source {
        if id == LAYOUT_XBOX_ELITE_2 {
            Source::Evdev
        } else {
            Source::Puck
        }
    }

    /// Whether this device has trackpads. `false` means every pad consumer —
    /// [`crate::run::drive_cursor`]'s touch gate, the OSK's pad router, the
    /// scroll and scrub handlers — sees a permanently untouched pad and idles
    /// cleanly, which is exactly the behaviour they already have for a lifted
    /// finger. Nothing has to be special-cased; this is the *reason* nothing
    /// has to be.
    pub fn has_pads(self) -> bool {
        matches!(self, Source::Puck)
    }

    /// Whether this device can play the puck's `0x81` pulse reports. A rumble
    /// motor cannot: `ff-memless` runs at jiffy granularity and an ERM motor
    /// needs tens of milliseconds to spin up, so a 250 Hz texture would be
    /// noise. Phase 2 maps only `Gesture`/`Commit` to a short rumble tap.
    pub fn has_haptics(self) -> bool {
        matches!(self, Source::Puck)
    }

    /// Whether the cursor on this device is *rate* controlled (a stick is a
    /// velocity command) rather than *position* controlled (a pad is an
    /// absolute coordinate). The inverse of [`has_pads`](Self::has_pads),
    /// named for the thing it decides.
    pub fn cursor_is_rate(self) -> bool {
        !self.has_pads()
    }
}

/// One decoded frame of controller state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Frame {
    /// Which backend produced this frame. Not on the wire — set by the decoder.
    pub source: Source,
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
            source: Source::Puck,
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

    /// Set or clear one button bit.
    ///
    /// The puck's decoder builds `buttons` in one pass from the wire, but a
    /// change-driven backend ([`crate::evdev`]) is handed one button at a time
    /// and keeps a persistent frame between events, so it needs the inverse of
    /// [`pressed`](Self::pressed).
    pub fn set(&mut self, b: Button, down: bool) {
        let idx = BUTTON_BITS.iter().position(|&(_, _, bb)| bb == b).unwrap();
        if down {
            self.buttons |= 1 << idx;
        } else {
            self.buttons &= !(1 << idx);
        }
    }

    /// Whether this frame carries no deliberate input at all: nothing pressed,
    /// both sticks inside the gesture engine's centre deadzone, both triggers
    /// released, and neither pad touched.
    ///
    /// This is the question "last-active source wins" asks
    /// ([`crate::run`]): with two controllers connected, a frame from the
    /// *inactive* one is dropped unless it says the user actually did
    /// something. A resting stick's idle offset and a change-driven device's
    /// periodic re-send therefore never steal the cursor from the pad the hand
    /// is really on.
    pub fn is_neutral(&self) -> bool {
        let centred = |(x, y): (i16, i16)| {
            // The same centre the flick recogniser uses, so "did the user move
            // a stick" has exactly one answer in the daemon.
            let dz = crate::gesture::DEADZONE;
            i32::from(x).abs() < dz && i32::from(y).abs() < dz
        };
        self.buttons == 0
            && self.l2 == 0
            && self.r2 == 0
            && centred(self.left_stick)
            && centred(self.right_stick)
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
