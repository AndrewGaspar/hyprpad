//! Pure puck-frame → wire-report conversion, one function per profile.
//!
//! Nothing here touches a device, a thread or a clock: given a
//! [`report::Frame`] (or the raw bytes it was decoded from) and a sequence
//! number, each function returns the exact bytes to hand to `UHID_INPUT2`. That
//! makes the whole translation table testable against the captured reports in
//! `src/report.rs`'s own tests.
//!
//! # The two paths are deliberately asymmetric
//!
//! * **triton** — [`puck_to_triton`] is a *pass-through*. The wired `1302`
//!   descriptor declares input report `0x42` with a 53-byte payload; the puck
//!   streams input report `0x42` with a 53-byte payload; they are the same
//!   report. So the function copies the raw bytes and clears the handful of bits
//!   §4.4 of `docs/research/uhid-steam-controller.md` says hyprpad must keep for
//!   itself. Nothing is re-encoded, which means fields hyprpad does not model —
//!   the IMU at bytes 30+, the undecoded tail — reach Steam intact.
//! * **deck** — [`puck_to_deck`] is a *transcode* into the Steam Deck's 64-byte
//!   packed report. Necessary because that protocol shares no layout with the
//!   puck's, and the price of the identity SteamOS has proven.
//!
//! # What gets stripped, and why
//!
//! Per §4.4, the guide (Steam) bit is cleared on every relayed frame while
//! hyprpad owns the guide chord layer — otherwise every `guide+x` chord *also*
//! opens the Steam overlay. [`StripMask`] carries that decision plus the Quick
//! Access button, which the owner's config may likewise claim.

use crate::report::{Button, Frame};

/// Wire length of the triton profile's input report: report id `0x42` plus its
/// 53-byte payload. Asserted against the captured descriptor in `profile`.
pub const TRITON_REPORT_LEN: usize = 54;

/// Wire length of the deck profile's input report. No report id: the 38-byte
/// descriptor has no `REPORT_ID` items, so nothing is prefixed.
pub const DECK_REPORT_LEN: usize = 64;

/// The puck's vendor input report id.
pub const REPORT_ID_INPUT: u8 = 0x42;

/// One triton-profile input report, ready for `UHID_INPUT2`.
pub type TritonReport = [u8; TRITON_REPORT_LEN];

/// One deck-profile input report, ready for `UHID_INPUT2`.
pub type DeckReport = [u8; DECK_REPORT_LEN];

/// Which buttons hyprpad keeps for itself rather than relaying.
///
/// Derived from the live config by the caller, so `hyprpad reload` changes it on
/// the next frame with nothing to rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct StripMask {
    /// Clear the Steam/guide button. Set whenever the guide is hyprpad's global
    /// chord modifier — i.e. whenever `[gamepad] forward_guide` is off.
    pub guide: bool,
    /// Clear the Quick Access button.
    pub quick_access: bool,
}

impl StripMask {
    /// Nothing withheld: every button reaches Steam.
    pub fn none() -> StripMask {
        StripMask { guide: false, quick_access: false }
    }

    /// The ordinary case — hyprpad owns the guide layer, Steam gets the rest.
    pub fn guide_only() -> StripMask {
        StripMask { guide: true, quick_access: false }
    }
}

// ---------------------------------------------------------------------------
// triton: pass-through
// ---------------------------------------------------------------------------

/// Byte 4, bit 0 of report `0x42` — the Steam/guide button
/// (`src/report.rs`'s `Button::Steam`).
const RAW_STEAM: (usize, u8) = (4, 0x01);
/// Byte 2, bit 4 — Quick Access (`Button::QuickAccess`).
const RAW_QUICK_ACCESS: (usize, u8) = (2, 0x10);

/// Relay one raw puck report as the wired `1302`'s own input report.
///
/// Returns `None` for anything that is not a 54-byte `0x42` — the puck also
/// streams `0x43` battery, `0x44`, `0x45`, `0x79` and `0x7b`, which this build
/// does not forward (see the module docs of `relay`).
///
/// The sequence counter at byte 1 is **relayed untouched**: §4.4 is explicit
/// that renumbering risks desync with the IMU timestamp path and that Steam
/// tolerates gaps.
pub fn puck_to_triton(raw: &[u8], strip: StripMask) -> Option<TritonReport> {
    if raw.len() != TRITON_REPORT_LEN || raw[0] != REPORT_ID_INPUT {
        return None;
    }
    let mut out: TritonReport = [0u8; TRITON_REPORT_LEN];
    out.copy_from_slice(raw);
    if strip.guide {
        out[RAW_STEAM.0] &= !RAW_STEAM.1;
    }
    if strip.quick_access {
        out[RAW_QUICK_ACCESS.0] &= !RAW_QUICK_ACCESS.1;
    }
    Some(out)
}

/// Set the Steam/guide bit in a triton report — the deliberate undo of
/// [`StripMask::guide`], for the synthesized tap.
///
/// Steam never sees the real guide press (the guide layer outranks game
/// forwarding, so those frames stream neutral), so a bare tap in a game is
/// *put back* here, on top of whatever the relay was already streaming, for
/// the handful of ticks `SteamRelay::pulse_guide` asks for.
///
/// Takes a slice rather than a [`TritonReport`] because the streamer builds its
/// tick as a `Vec`; a report too short to hold the bit is left alone.
pub fn press_triton_guide(report: &mut [u8]) {
    if let Some(b) = report.get_mut(RAW_STEAM.0) {
        *b |= RAW_STEAM.1;
    }
}

/// A triton report with everything at rest: no buttons, centred sticks and pads,
/// no trigger travel. `seq` becomes the byte-1 counter so a stream of neutrals
/// still looks alive.
///
/// This is the same invariant `gamepad::PadReport::neutral` enforces for the
/// Xbox pad — a sink must never be left holding a stuck input.
pub fn triton_neutral(seq: u8) -> TritonReport {
    let mut out = [0u8; TRITON_REPORT_LEN];
    out[0] = REPORT_ID_INPUT;
    out[1] = seq;
    out
}

// ---------------------------------------------------------------------------
// deck: transcode
// ---------------------------------------------------------------------------

// The 64-byte Steam Deck input report, from InputPlumber's
// `src/drivers/steam_deck/hid_report.rs` `PackedInputDataReport`
// (`#[packed_struct(bit_numbering = "msb0", size_bytes = "64")]`), read from
// `main` on 2026-09-02. Bit N of the struct is byte `N / 8`, mask
// `0x80 >> (N % 8)` — so bit 64 is byte 8's high bit.

/// `major_ver`, byte 0.
const DECK_MAJOR_VER: u8 = 0x01;
/// `minor_ver`, byte 1.
const DECK_MINOR_VER: u8 = 0x00;
/// `report_type`, byte 2 — `ReportType::InputData`.
const DECK_REPORT_TYPE: u8 = 0x09;
/// `report_size`, byte 3 — 64.
const DECK_REPORT_SIZE: u8 = 0x40;

/// Puck button → (Deck report byte, bit mask).
///
/// Left column: `report::Button`, decoded from the puck's `0x42` by
/// `src/report.rs`. Right column: the `PackedInputDataReport` field of the same
/// name, at its `packed_field(bits = …)` position.
///
/// Deliberately unmapped:
/// * `Cap0`..`Cap3` — `src/report.rs` calls their individual assignment
///   "tentative", and the Deck's `l_stick_touch`/`r_stick_touch` are the only
///   plausible targets. Guessing here would put phantom stick-touch flags in
///   front of Steam Input's capacitive-stick behaviours, so they stay clear.
const DECK_BUTTONS: [(Button, usize, u8); 26] = [
    // byte 8 — face cluster and shoulders (bits 64..71)
    (Button::A, 8, 0x80),            // a           bit 64
    (Button::X, 8, 0x40),            // x           bit 65
    (Button::B, 8, 0x20),            // b           bit 66
    (Button::Y, 8, 0x10),            // y           bit 67
    (Button::BumperL1, 8, 0x08),     // l1          bit 68
    (Button::BumperR1, 8, 0x04),     // r1          bit 69
    (Button::TriggerL2Full, 8, 0x02), // l2         bit 70
    (Button::TriggerR2Full, 8, 0x01), // r2         bit 71
    // byte 9 — back button, menu/steam/options, dpad (bits 72..79)
    (Button::GripL5, 9, 0x80),       // l5          bit 72
    (Button::Menu, 9, 0x40),         // menu        bit 73
    (Button::Steam, 9, 0x20),        // steam       bit 74
    (Button::View, 9, 0x10),         // options     bit 75
    (Button::DpadDown, 9, 0x08),     // down        bit 76
    (Button::DpadLeft, 9, 0x04),     // left        bit 77
    (Button::DpadRight, 9, 0x02),    // right       bit 78
    (Button::DpadUp, 9, 0x01),       // up          bit 79
    // byte 10 — left stick click, trackpad touch/press, back button (80..87)
    (Button::L3, 10, 0x40),          // l3          bit 81
    (Button::PadRightTouch, 10, 0x10), // r_pad_touch bit 83
    (Button::PadLeftTouch, 10, 0x08), // l_pad_touch bit 84
    (Button::PadRightClick, 10, 0x04), // r_pad_press bit 85
    (Button::PadLeftClick, 10, 0x02), // l_pad_press bit 86
    (Button::GripR5, 10, 0x01),      // r5          bit 87
    // byte 11 — right stick click (bits 88..95)
    (Button::R3, 11, 0x04),          // r3          bit 93
    // byte 13 — the upper back grips (bits 104..111)
    (Button::GripR4, 13, 0x04),      // r4          bit 109
    (Button::GripL4, 13, 0x02),      // l4          bit 110
    // byte 14 — Quick Access (bits 112..119)
    (Button::QuickAccess, 14, 0x04), // quick_access bit 117
];

/// Where the Steam/guide bit lands in a Deck report, for [`StripMask`].
const DECK_STEAM: (usize, u8) = (9, 0x20);
/// Where Quick Access lands.
const DECK_QUICK_ACCESS: (usize, u8) = (14, 0x04);

/// Transcode one decoded puck frame into a Steam Deck input report.
///
/// `seq` becomes the `frame` field (bytes 4..8, little-endian u32) that
/// InputPlumber bumps `wrapping_add(1)` on every write. The relay supplies it,
/// so a repeated frame under a silent puck still advances the counter.
///
/// # Fields that stay zero, and why
///
/// * **accelerometer (24..30), gyro (30..36), magnetometer (36..44)** —
///   `report::Frame` carries no IMU. The puck's `0x42` has IMU data at bytes 30+
///   but only after an enable feature report, and `src/report.rs` explicitly
///   leaves those bytes undecoded. Relaying gyro is one of the standing
///   advantages of the `triton` pass-through, which needs no decoder at all.
/// * **`l_stick_force` / `r_stick_force` (60..64)** — the capacitive stick
///   sensors; see `DECK_BUTTONS` on why the puck's `Cap*` bits are not guessed
///   into them.
/// * **`_unk31` (15)** — unknown in the reference implementation too.
pub fn puck_to_deck(frame: &Frame, seq: u32) -> DeckReport {
    let mut out = deck_header(seq);
    for &(button, byte, mask) in &DECK_BUTTONS {
        if frame.pressed(button) {
            out[byte] |= mask;
        }
    }
    // Trackpads: absolute position, then force. Same i16/u16 encoding both ends.
    put_i16(&mut out, 16, frame.left_pad.x);
    put_i16(&mut out, 18, frame.left_pad.y);
    put_i16(&mut out, 20, frame.right_pad.x);
    put_i16(&mut out, 22, frame.right_pad.y);
    put_u16(&mut out, 56, frame.left_pad.force);
    put_u16(&mut out, 58, frame.right_pad.force);
    // Analog triggers: both sides use an unsigned 0..=32767 scale.
    put_u16(&mut out, 44, frame.l2);
    put_u16(&mut out, 46, frame.r2);
    // Sticks: i16, +x right and +y up on both sides.
    put_i16(&mut out, 48, frame.left_stick.0);
    put_i16(&mut out, 50, frame.left_stick.1);
    put_i16(&mut out, 52, frame.right_stick.0);
    put_i16(&mut out, 54, frame.right_stick.1);
    out
}

/// A Deck report with everything at rest — the header, the sequence number, and
/// zeros. Exactly what the proven probe streamed between real frames.
pub fn deck_neutral(seq: u32) -> DeckReport {
    deck_header(seq)
}

/// The four-byte header plus the frame counter, which every Deck report carries.
fn deck_header(seq: u32) -> DeckReport {
    let mut out = [0u8; DECK_REPORT_LEN];
    out[0] = DECK_MAJOR_VER;
    out[1] = DECK_MINOR_VER;
    out[2] = DECK_REPORT_TYPE;
    out[3] = DECK_REPORT_SIZE;
    out[4..8].copy_from_slice(&seq.to_le_bytes());
    out
}

fn put_i16(buf: &mut DeckReport, at: usize, v: i16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u16(buf: &mut DeckReport, at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

/// Apply a [`StripMask`] to an already-built Deck report.
///
/// Separate from [`puck_to_deck`] so the transcode stays a pure function of the
/// frame and the masking policy stays one readable place.
pub fn strip_deck(report: &mut DeckReport, strip: StripMask) {
    if strip.guide {
        report[DECK_STEAM.0] &= !DECK_STEAM.1;
    }
    if strip.quick_access {
        report[DECK_QUICK_ACCESS.0] &= !DECK_QUICK_ACCESS.1;
    }
}

/// Set the Steam button's bit in a Deck report — [`press_triton_guide`]'s
/// opposite number, for the same synthesized tap.
pub fn press_deck_guide(report: &mut [u8]) {
    if let Some(b) = report.get_mut(DECK_STEAM.0) {
        *b |= DECK_STEAM.1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw 54-byte `0x42` with `buttons` pressed — the same construction
    /// `src/gamepad.rs`'s tests use, but kept raw so the pass-through can be
    /// tested on the bytes rather than on the decode.
    fn raw_with(buttons: &[Button]) -> Vec<u8> {
        // (byte, bit) for each button, mirroring `report::BUTTON_BITS`.
        let bits = |b: Button| -> (usize, u8) {
            match b {
                Button::A => (2, 0),
                Button::B => (2, 1),
                Button::X => (2, 2),
                Button::Y => (2, 3),
                Button::QuickAccess => (2, 4),
                Button::R3 => (2, 5),
                Button::Menu => (2, 6),
                Button::GripR4 => (2, 7),
                Button::GripR5 => (3, 0),
                Button::BumperR1 => (3, 1),
                Button::DpadDown => (3, 2),
                Button::DpadRight => (3, 3),
                Button::DpadLeft => (3, 4),
                Button::DpadUp => (3, 5),
                Button::View => (3, 6),
                Button::L3 => (3, 7),
                Button::Steam => (4, 0),
                Button::GripL4 => (4, 1),
                Button::GripL5 => (4, 2),
                Button::BumperL1 => (4, 3),
                Button::Cap0 => (4, 4),
                Button::PadRightTouch => (4, 5),
                Button::PadRightClick => (4, 6),
                Button::TriggerR2Full => (4, 7),
                Button::Cap1 => (5, 0),
                Button::PadLeftTouch => (5, 1),
                Button::PadLeftClick => (5, 2),
                Button::TriggerL2Full => (5, 3),
                Button::Cap2 => (5, 4),
                Button::Cap3 => (5, 5),
            }
        };
        let mut raw = vec![0u8; TRITON_REPORT_LEN];
        raw[0] = REPORT_ID_INPUT;
        raw[1] = 0x11; // an arbitrary live counter value
        for &b in buttons {
            let (byte, bit) = bits(b);
            raw[byte] |= 1 << bit;
        }
        raw
    }

    /// The decoder and the raw builder must agree, or every test below is
    /// testing a fiction.
    #[test]
    fn the_test_helper_agrees_with_the_real_decoder() {
        for b in [Button::A, Button::Steam, Button::GripL4, Button::QuickAccess, Button::Cap3] {
            let raw = raw_with(&[b]);
            let f = Frame::decode(&raw).expect("a valid 0x42");
            assert!(f.pressed(b), "{b:?} must round-trip through the decoder");
        }
    }

    // -- triton: pass-through -----------------------------------------------

    #[test]
    fn triton_relays_a_frame_byte_for_byte_when_nothing_is_withheld() {
        let mut raw = raw_with(&[Button::A, Button::BumperR1, Button::DpadUp]);
        // Analog channels, so the copy is proved over the whole report.
        raw[6..8].copy_from_slice(&12_345u16.to_le_bytes()); // l2
        raw[10..12].copy_from_slice(&(-4_096i16).to_le_bytes()); // left stick x
        raw[30] = 0xde; // an "IMU" byte the decoder does not model
        raw[53] = 0xad; // the last byte of the report

        let out = puck_to_triton(&raw, StripMask::none()).expect("a 0x42 passes through");
        assert_eq!(out.as_slice(), raw.as_slice(), "not one byte re-encoded");
    }

    #[test]
    fn triton_clears_the_guide_bit_and_leaves_the_rest_alone() {
        let raw = raw_with(&[Button::Steam, Button::A, Button::QuickAccess]);
        let out = puck_to_triton(&raw, StripMask::guide_only()).unwrap();
        let f = Frame::decode(&out).unwrap();
        assert!(!f.pressed(Button::Steam), "the guide never reaches Steam");
        assert!(f.pressed(Button::A), "everything else is untouched");
        assert!(f.pressed(Button::QuickAccess), "not stripped unless asked");

        let out = puck_to_triton(&raw, StripMask { guide: true, quick_access: true }).unwrap();
        let f = Frame::decode(&out).unwrap();
        assert!(!f.pressed(Button::QuickAccess));
        assert!(f.pressed(Button::A));
    }

    #[test]
    fn triton_relays_the_sequence_counter_untouched() {
        let mut raw = raw_with(&[]);
        raw[1] = 0xa7;
        let out = puck_to_triton(&raw, StripMask::guide_only()).unwrap();
        assert_eq!(out[1], 0xa7, "byte 1 is relayed, never renumbered (§4.4)");
    }

    #[test]
    fn triton_refuses_anything_that_is_not_a_54_byte_0x42() {
        assert!(puck_to_triton(&[], StripMask::none()).is_none());
        // The battery report the puck also streams.
        let mut battery = vec![0u8; 15];
        battery[0] = 0x43;
        assert!(puck_to_triton(&battery, StripMask::none()).is_none());
        // Right id, wrong length.
        let mut short = vec![0u8; 53];
        short[0] = 0x42;
        assert!(puck_to_triton(&short, StripMask::none()).is_none());
    }

    #[test]
    fn triton_neutral_is_a_valid_empty_report() {
        let n = triton_neutral(9);
        assert_eq!(n.len(), TRITON_REPORT_LEN);
        assert_eq!(n[0], REPORT_ID_INPUT);
        assert_eq!(n[1], 9, "the counter still advances under a silent puck");
        assert!(n[2..].iter().all(|&b| b == 0));
        let f = Frame::decode(&n).expect("a neutral report still decodes");
        assert_eq!(f.buttons, 0);
        assert_eq!((f.l2, f.r2), (0, 0));
        assert_eq!(f.left_stick, (0, 0));
        assert_eq!(f.right_stick, (0, 0));
        assert_eq!(f.left_pad, Default::default());
        assert_eq!(f.right_pad, Default::default());
    }

    // -- deck: transcode ----------------------------------------------------

    fn deck_from(buttons: &[Button]) -> DeckReport {
        let f = Frame::decode(&raw_with(buttons)).unwrap();
        puck_to_deck(&f, 0)
    }

    #[test]
    fn deck_reports_carry_the_reference_header_and_sequence() {
        let r = deck_neutral(0x0102_0304);
        assert_eq!(&r[..4], &[0x01, 0x00, 0x09, 0x40], "major, minor, InputData, 64");
        assert_eq!(&r[4..8], &0x0102_0304u32.to_le_bytes());
        assert!(r[8..].iter().all(|&b| b == 0), "a neutral Deck report is header + zeros");
        assert_eq!(r.len(), DECK_REPORT_LEN);
        // The transcode of an all-zero frame is exactly the neutral report.
        assert_eq!(puck_to_deck(&Frame::default(), 7), deck_neutral(7));
    }

    #[test]
    fn deck_maps_the_face_cluster_and_shoulders_into_byte_8() {
        assert_eq!(deck_from(&[Button::A])[8], 0x80);
        assert_eq!(deck_from(&[Button::X])[8], 0x40);
        assert_eq!(deck_from(&[Button::B])[8], 0x20);
        assert_eq!(deck_from(&[Button::Y])[8], 0x10);
        assert_eq!(deck_from(&[Button::BumperL1])[8], 0x08);
        assert_eq!(deck_from(&[Button::BumperR1])[8], 0x04);
        assert_eq!(deck_from(&[Button::TriggerL2Full])[8], 0x02);
        assert_eq!(deck_from(&[Button::TriggerR2Full])[8], 0x01);
        // A/B and X/Y are *not* swapped: the Deck's bit order is a,x,b,y.
        let both = deck_from(&[Button::A, Button::B]);
        assert_eq!(both[8], 0xa0);
    }

    #[test]
    fn deck_maps_dpad_menu_steam_and_the_lower_grip_into_byte_9() {
        assert_eq!(deck_from(&[Button::GripL5])[9], 0x80);
        assert_eq!(deck_from(&[Button::Menu])[9], 0x40);
        assert_eq!(deck_from(&[Button::Steam])[9], 0x20);
        assert_eq!(deck_from(&[Button::View])[9], 0x10, "View is the Deck's `options`");
        assert_eq!(deck_from(&[Button::DpadDown])[9], 0x08);
        assert_eq!(deck_from(&[Button::DpadLeft])[9], 0x04);
        assert_eq!(deck_from(&[Button::DpadRight])[9], 0x02);
        assert_eq!(deck_from(&[Button::DpadUp])[9], 0x01);
    }

    #[test]
    fn deck_maps_the_pads_stick_clicks_and_every_grip() {
        assert_eq!(deck_from(&[Button::L3])[10], 0x40);
        assert_eq!(deck_from(&[Button::PadRightTouch])[10], 0x10);
        assert_eq!(deck_from(&[Button::PadLeftTouch])[10], 0x08);
        assert_eq!(deck_from(&[Button::PadRightClick])[10], 0x04);
        assert_eq!(deck_from(&[Button::PadLeftClick])[10], 0x02);
        assert_eq!(deck_from(&[Button::GripR5])[10], 0x01);
        assert_eq!(deck_from(&[Button::R3])[11], 0x04);
        assert_eq!(deck_from(&[Button::GripR4])[13], 0x04);
        assert_eq!(deck_from(&[Button::GripL4])[13], 0x02);
        assert_eq!(deck_from(&[Button::QuickAccess])[14], 0x04);
        // All four back grips are reachable — the whole point over the Xbox pad,
        // which forwards none of them.
        for g in [Button::GripL4, Button::GripL5, Button::GripR4, Button::GripR5] {
            assert_ne!(deck_from(&[g])[8..15], [0u8; 7], "{g:?} must reach Steam");
        }
    }

    #[test]
    fn deck_leaves_the_capacitive_bits_unmapped() {
        for c in [Button::Cap0, Button::Cap1, Button::Cap2, Button::Cap3] {
            assert_eq!(deck_from(&[c])[8..15], [0u8; 7], "{c:?} is tentative; never guessed");
        }
    }

    #[test]
    fn deck_carries_every_analog_channel_at_its_documented_offset() {
        let mut raw = raw_with(&[]);
        raw[6..8].copy_from_slice(&30_000u16.to_le_bytes()); // l2
        raw[8..10].copy_from_slice(&1_234u16.to_le_bytes()); // r2
        raw[10..12].copy_from_slice(&(-20_000i16).to_le_bytes()); // ls x
        raw[12..14].copy_from_slice(&20_000i16.to_le_bytes()); // ls y
        raw[14..16].copy_from_slice(&(-1i16).to_le_bytes()); // rs x
        raw[16..18].copy_from_slice(&2i16.to_le_bytes()); // rs y
        raw[18..20].copy_from_slice(&(-300i16).to_le_bytes()); // l pad x
        raw[20..22].copy_from_slice(&400i16.to_le_bytes()); // l pad y
        raw[22..24].copy_from_slice(&9_000u16.to_le_bytes()); // l pad force
        raw[24..26].copy_from_slice(&500i16.to_le_bytes()); // r pad x
        raw[26..28].copy_from_slice(&(-600i16).to_le_bytes()); // r pad y
        raw[28..30].copy_from_slice(&12_550u16.to_le_bytes()); // r pad force
        let f = Frame::decode(&raw).unwrap();
        let d = puck_to_deck(&f, 0);

        let i16at = |at: usize| i16::from_le_bytes(d[at..at + 2].try_into().unwrap());
        let u16at = |at: usize| u16::from_le_bytes(d[at..at + 2].try_into().unwrap());
        assert_eq!((i16at(16), i16at(18)), (-300, 400), "l_pad_x/y");
        assert_eq!((i16at(20), i16at(22)), (500, -600), "r_pad_x/y");
        assert_eq!((u16at(44), u16at(46)), (30_000, 1_234), "l_trigg/r_trigg");
        assert_eq!((i16at(48), i16at(50)), (-20_000, 20_000), "l_stick_x/y");
        assert_eq!((i16at(52), i16at(54)), (-1, 2), "r_stick_x/y");
        assert_eq!((u16at(56), u16at(58)), (9_000, 12_550), "l_pad_force/r_pad_force");
        // Untranscoded windows stay clear.
        assert!(d[24..44].iter().all(|&b| b == 0), "accel, gyro and magnetometer");
        assert!(d[60..64].iter().all(|&b| b == 0), "capacitive stick force");
        assert_eq!(d[15], 0, "_unk31");
    }

    #[test]
    fn stripping_a_deck_report_clears_exactly_the_named_buttons() {
        let mut d = deck_from(&[Button::Steam, Button::QuickAccess, Button::A]);
        strip_deck(&mut d, StripMask::guide_only());
        assert_eq!(d[9] & 0x20, 0, "steam cleared");
        assert_eq!(d[14] & 0x04, 0x04, "quick access untouched");
        assert_eq!(d[8] & 0x80, 0x80, "A untouched");

        let mut d = deck_from(&[Button::Steam, Button::QuickAccess, Button::A]);
        strip_deck(&mut d, StripMask { guide: true, quick_access: true });
        assert_eq!(d[9] & 0x20, 0);
        assert_eq!(d[14] & 0x04, 0);
        assert_eq!(d[8] & 0x80, 0x80);

        let mut d = deck_from(&[Button::Steam]);
        strip_deck(&mut d, StripMask::none());
        assert_eq!(d[9] & 0x20, 0x20, "nothing withheld when nothing is asked");
    }

    /// Putting the guide bit back is exactly the inverse of stripping it, in
    /// both report shapes — and it touches nothing else.
    #[test]
    fn pressing_the_guide_is_the_exact_inverse_of_stripping_it() {
        // triton: on a neutral report, and on a live one that was stripped.
        let mut n = triton_neutral(7).to_vec();
        press_triton_guide(&mut n);
        assert!(Frame::decode(&n).unwrap().pressed(Button::Steam));
        assert_eq!(n[1], 7, "the counter is untouched");
        assert_eq!(n.iter().filter(|&&b| b != 0).count(), 3, "id, counter, guide");

        let raw = raw_with(&[Button::Steam, Button::A]);
        let stripped = puck_to_triton(&raw, StripMask::guide_only()).unwrap();
        let mut back = stripped.to_vec();
        press_triton_guide(&mut back);
        assert_eq!(back.as_slice(), raw.as_slice(), "byte for byte, the original");

        // deck: the same, on the transcode.
        let mut d = deck_neutral(3).to_vec();
        press_deck_guide(&mut d);
        assert_eq!(d[9] & 0x20, 0x20, "the steam bit is set");
        assert_eq!(&d[..4], &[0x01, 0x00, 0x09, 0x40], "the header is untouched");
        assert!(d[10..].iter().all(|&b| b == 0), "and nothing else is set");

        let mut d = deck_from(&[Button::Steam, Button::A]);
        strip_deck(&mut d, StripMask::guide_only());
        let want = deck_from(&[Button::Steam, Button::A]);
        press_deck_guide(&mut d);
        assert_eq!(d, want);
    }

    /// A report too short to hold the bit is left alone rather than panicking:
    /// the streamer builds its tick as a `Vec` and these take a slice.
    #[test]
    fn pressing_the_guide_on_a_short_report_is_a_no_op() {
        let mut short: Vec<u8> = vec![0x42];
        press_triton_guide(&mut short);
        press_deck_guide(&mut short);
        assert_eq!(short, vec![0x42]);
    }

    #[test]
    fn every_mapped_button_lands_on_a_distinct_bit() {
        let mut seen: Vec<(usize, u8)> = Vec::new();
        for &(_, byte, mask) in &DECK_BUTTONS {
            assert!(mask.count_ones() == 1, "one bit per button");
            assert!(!seen.contains(&(byte, mask)), "byte {byte} mask {mask:#04x} used twice");
            seen.push((byte, mask));
        }
        assert_eq!(DECK_BUTTONS.len(), 26);
        // Buttons only ever land in the bitfield window, bytes 8..=14.
        assert!(DECK_BUTTONS.iter().all(|&(_, b, _)| (8..=14).contains(&b)));
    }
}
