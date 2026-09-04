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
/// The daemon's vocabulary is the controller's: face buttons, grips, two sticks, two
/// trackpads. A second backend ([`crate::evdev`]) fills the same struct from an
/// ordinary Linux gamepad, which has no trackpads and no per-pad actuators. The
/// difference is not "the pads happen to be untouched this frame" — it is "this
/// device has no pads at all", and every consumer that would otherwise idle
/// forever on a touch bit needs to be able to tell the two apart.
///
/// [`Frame`] derives `Default`, and [`Source::SteamController`] is the default, so every
/// existing test and the whole controller path are byte-identical to before this
/// existed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Source {
    /// The 2026 Steam Controller itself, decoded from hidraw ([`Frame::decode`]).
    #[default]
    SteamController,
    /// A generic Linux gamepad read over evdev ([`crate::evdev`]) — the Xbox
    /// Elite Series 2 is the one this was built for. Phase 2's Bluetooth hidraw
    /// sidecar (`docs/design/xbox-elite.md`) will be a third variant beside it,
    /// which is why this is an enum and not a bool.
    Evdev,
}

/// The cheat sheet's layout id for the controller —
/// `shell/hyprpad.cheatsheet/layouts/steam-controller-2026.json`.
pub const LAYOUT_STEAM_CONTROLLER: &str = "steam-controller-2026";

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
            Source::SteamController => LAYOUT_STEAM_CONTROLLER,
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
    /// controller, the safe default for a config written against it.
    pub fn from_layout(id: &str) -> Source {
        if id == LAYOUT_XBOX_ELITE_2 {
            Source::Evdev
        } else {
            Source::SteamController
        }
    }

    /// Whether this device has trackpads. `false` means every pad consumer —
    /// [`crate::run::drive_cursor`]'s touch gate, the OSK's pad router, the
    /// scroll and scrub handlers — sees a permanently untouched pad and idles
    /// cleanly, which is exactly the behaviour they already have for a lifted
    /// finger. Nothing has to be special-cased; this is the *reason* nothing
    /// has to be.
    pub fn has_pads(self) -> bool {
        matches!(self, Source::SteamController)
    }

    /// Whether this device can play the controller's `0x81` pulse reports. A rumble
    /// motor cannot: `ff-memless` runs at jiffy granularity and an ERM motor
    /// needs tens of milliseconds to spin up, so a 250 Hz texture would be
    /// noise. Phase 2 maps only `Gesture`/`Commit` to a short rumble tap.
    pub fn has_haptics(self) -> bool {
        matches!(self, Source::SteamController)
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

/// The dongle's vendor input report: id `0x42`, 54 bytes on the wire
/// (`REPORT_ID_INPUT`, `hid-steam.c:323`).
pub const REPORT_ID_INPUT: u8 = 0x42;
/// Wire length of [`REPORT_ID_INPUT`] — 53 payload bytes plus the id.
pub const REPORT_LEN_INPUT: usize = 54;

/// The Bluetooth link's vendor input report: id `0x45`, 46 bytes on the wire
/// (`REPORT_ID_INPUT2`, `hid-steam.c:325`).
pub const REPORT_ID_INPUT_BLE: u8 = 0x45;
/// Wire length of [`REPORT_ID_INPUT_BLE`] — 45 payload bytes plus the id.
pub const REPORT_LEN_INPUT_BLE: usize = 46;

impl Frame {
    /// Decode a raw hidraw report. Returns `None` for anything that is not one
    /// of the controller's two vendor input reports.
    ///
    /// # Why two ids decode identically
    ///
    /// `0x45` is `0x42` with the trailing quaternion cut off, and **nothing
    /// else**: the kernel's own layout table (`hid-steam.c:2325-2355`)
    /// documents `0x42` as `0x45` plus bytes 46–53, and dispatches both ids
    /// into the same handler at the same fixed offsets (`:2552`, `:2571`). SDL
    /// says it the same way — its `TritonMTUFull_t` is `TritonMTUNoQuat_t` plus
    /// the quaternion, and both ids land in one arm of its dispatch.
    ///
    /// The deepest byte this function reads is **29** (the right pad's force),
    /// and `0x45` carries bytes 1–45 at identical offsets. So the short report
    /// costs the decoder nothing at all: every field below is present either
    /// way, and this is a length/id guard rather than a second decoder. Only
    /// [`crate::uhid::translate::controller_to_triton`], which forwards raw bytes to a
    /// fake device whose descriptor declares `0x42` at 54, has to re-frame.
    ///
    /// The controller's other reports — `0x40` (the lizard mouse, 6 bytes),
    /// `0x41` (lizard keyboard), `0x43` (battery, 15 bytes), `0x44`, `0x79`,
    /// `0x7b` — are refused here and dropped by the reader
    /// ([`crate::run`] logs `0x43` once under `HYPRSC_DEBUG`).
    pub fn decode(raw: &[u8]) -> Option<Frame> {
        let is_input = (raw.len() == REPORT_LEN_INPUT && raw[0] == REPORT_ID_INPUT)
            || (raw.len() == REPORT_LEN_INPUT_BLE && raw[0] == REPORT_ID_INPUT_BLE);
        if !is_input {
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
            source: Source::SteamController,
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
    /// The controller's decoder builds `buttons` in one pass from the wire, but a
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

    /// Whether this frame carries no deliberate input at all: nothing pressed
    /// or touched, both triggers released, and both sticks inside the gesture
    /// engine's centre deadzone.
    ///
    /// This is the question "last-active source wins" asks ([`crate::run`]):
    /// with two controllers connected, a frame from the *inactive* one is
    /// dropped unless it says the user actually did something. A resting
    /// stick's idle offset and a change-driven device's periodic re-send
    /// therefore never steal the cursor from the pad the hand is really on.
    ///
    /// **The capacitive flags are excluded**, and that is the whole subtlety
    /// here. `Cap0..3` fire on *hand contact* with the controller's grips, not on an
    /// action — so a hand simply resting on the controller while the other one drives
    /// an Xbox pad would otherwise make every controller frame "deliberate" and the
    /// two sources would fight over the cursor several hundred times a second.
    /// Proximity is not intent. A press, a click, a trigger or a stick is.
    pub fn is_neutral(&self) -> bool {
        let centred = |(x, y): (i16, i16)| {
            // The same centre the flick recogniser uses, so "did the user move
            // a stick" has exactly one answer in the daemon.
            let dz = crate::gesture::DEADZONE;
            i32::from(x).abs() < dz && i32::from(y).abs() < dz
        };
        self.without(&[Button::Cap0, Button::Cap1, Button::Cap2, Button::Cap3]).buttons == 0
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports captured read-only from the live Bluetooth node — see the file's
    /// own header for the method, the rate and what the bytes are.
    const BT_FIXTURE: &str = include_str!("../tests/data/bt-0x45.hex");

    /// One named report from [`BT_FIXTURE`].
    fn fixture(name: &str) -> Vec<u8> {
        let hex = BT_FIXTURE
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .find_map(|l| Some(l.strip_prefix(name)?.trim().to_string()))
            .unwrap_or_else(|| panic!("no report named {name} in tests/data/bt-0x45.hex"));
        assert!(hex.len() % 2 == 0, "{name}: odd number of hex digits");
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
            .collect()
    }

    /// The 54-byte `0x42` a dongle would have produced from the same state:
    /// the id swapped and the quaternion tail appended as zeros. Exactly the
    /// inverse of the re-frame `crate::uhid::translate::controller_to_triton` does on
    /// the way out to Steam.
    fn as_0x42(bt: &[u8]) -> Vec<u8> {
        assert_eq!(bt.len(), REPORT_LEN_INPUT_BLE);
        let mut out = vec![0u8; REPORT_LEN_INPUT];
        out[0] = REPORT_ID_INPUT;
        out[1..REPORT_LEN_INPUT_BLE].copy_from_slice(&bt[1..]);
        out
    }

    /// **The whole of phase 1's decode claim.** `0x45`/46 and `0x42`/54 carry
    /// the same fields at the same offsets, so the same bytes must produce the
    /// same `Frame` whichever id fronts them. If this ever fails, `0x45` needs a
    /// decoder of its own rather than a length guard.
    #[test]
    fn a_bluetooth_0x45_decodes_to_the_same_frame_as_the_equivalent_0x42() {
        for name in ["idle", "in-hand", "idle-later"] {
            let bt = fixture(name);
            assert_eq!(bt.len(), REPORT_LEN_INPUT_BLE, "{name}");
            assert_eq!(bt[0], REPORT_ID_INPUT_BLE, "{name}");

            let over_bt = Frame::decode(&bt).unwrap_or_else(|| panic!("{name} must decode"));
            let over_dongle = Frame::decode(&as_0x42(&bt)).expect("the 0x42 form must decode too");
            assert_eq!(over_bt, over_dongle, "{name}: the transport must not change the frame");
            assert_eq!(over_bt.source, Source::SteamController, "{name}: one controller, one source");
        }
    }

    /// And the fields are real, not merely equal: the idle capture is a
    /// resting controller, which is the state every other test in the daemon
    /// spells `is_neutral`.
    #[test]
    fn the_idle_bluetooth_capture_decodes_to_a_resting_controller() {
        let f = Frame::decode(&fixture("idle")).expect("decodes");
        assert_eq!(f.counter, 0xa1);
        assert_eq!(f.buttons, 0, "nothing pressed and nothing touched");
        assert_eq!((f.l2, f.r2), (0, 0));
        // The small idle offset `Frame`'s own docs describe, well inside the
        // gesture deadzone.
        assert_eq!(f.left_stick, (318, 345));
        assert_eq!(f.right_stick, (-353, 378));
        assert_eq!(f.left_pad, Pad::default(), "no finger on the left pad");
        assert_eq!(f.right_pad, Pad::default());
        assert!(f.is_neutral(), "a controller lying on a desk is neutral");
    }

    /// The in-hand capture is *not* neutral, so the fixture actually exercises
    /// the bitfield rather than pinning three copies of zero.
    #[test]
    fn the_in_hand_bluetooth_capture_carries_bits_the_idle_one_does_not() {
        let held = Frame::decode(&fixture("in-hand")).expect("decodes");
        let idle = Frame::decode(&fixture("idle")).expect("decodes");
        assert_ne!(held.buttons, 0, "bytes 4-5 carried the hand-contact cluster");
        assert_ne!(held.buttons, idle.buttons);
        // Whatever those bits mean, they survive the transport identically —
        // which is the claim, and the reason this file does not assert which
        // `Button` each one is.
        assert_eq!(Frame::decode(&as_0x42(&fixture("in-hand"))).unwrap(), held);
    }

    /// Everything that is not one of the two input reports is refused here and
    /// dropped by the reader. Over Bluetooth that matters more than it did over
    /// the dongle: lizard mode starts ON, so `0x40` really does stream until
    /// `src/lizard.rs` turns it off.
    #[test]
    fn the_controllers_other_reports_are_refused() {
        assert_eq!(Frame::decode(&fixture("battery-0x43")), None, "0x43 battery");
        assert_eq!(Frame::decode(&fixture("lizard-mouse-0x40")), None, "0x40 lizard mouse");
        assert_eq!(Frame::decode(&[]), None, "an empty read");
        assert_eq!(Frame::decode(&[0x45]), None, "the id alone");
    }

    /// The guard is a *pair* of (id, length) rows, not two independent tests —
    /// a right id at the wrong length is not a report this decoder understands,
    /// and reading one would run off the end of the buffer.
    #[test]
    fn each_input_id_is_accepted_only_at_its_own_length() {
        let bt = fixture("idle");
        let usb = as_0x42(&bt);

        assert!(Frame::decode(&bt).is_some());
        assert!(Frame::decode(&usb).is_some());

        // Right length, wrong id.
        let mut wrong_id = bt.clone();
        wrong_id[0] = REPORT_ID_INPUT;
        assert_eq!(Frame::decode(&wrong_id), None, "0x42 is never 46 bytes");
        let mut wrong_id = usb.clone();
        wrong_id[0] = REPORT_ID_INPUT_BLE;
        assert_eq!(Frame::decode(&wrong_id), None, "0x45 is never 54 bytes");

        // Right id, wrong length — including one byte either side of each.
        for n in [45, 47, 53, 54] {
            let mut short = bt.clone();
            short.resize(n, 0);
            assert_eq!(Frame::decode(&short), None, "0x45 at {n} bytes");
        }
        for n in [46, 53, 55] {
            let mut short = usb.clone();
            short.resize(n, 0);
            assert_eq!(Frame::decode(&short), None, "0x42 at {n} bytes");
        }
    }

    /// The constants are the contract shared with the relay's re-framing and
    /// with the report descriptor's own table.
    #[test]
    fn the_input_report_shapes_are_the_ones_the_descriptor_declares() {
        assert_eq!((REPORT_ID_INPUT, REPORT_LEN_INPUT), (0x42, 54));
        assert_eq!((REPORT_ID_INPUT_BLE, REPORT_LEN_INPUT_BLE), (0x45, 46));
        // 0x45 is 0x42 minus the 8-byte quaternion tail, and nothing else.
        assert_eq!(REPORT_LEN_INPUT - REPORT_LEN_INPUT_BLE, 8);
    }
}
