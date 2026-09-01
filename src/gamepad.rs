//! A daemon-owned virtual **Xbox-360-class gamepad** (`uinput`) — the Tier-1
//! keystone.
//!
//! hyprpad owns the real puck; Steam is denied it
//! (docs/experiments/w12-device-denial.md). So Steam and games need *something*
//! to see, and this is it: a kernel gamepad wearing the identity every engine,
//! SDL mapping database and anti-cheat already trusts —
//! `045e:028e`, "hyprpad virtual gamepad". The daemon forwards the real
//! controller's state to it **only while a game holds focus**, and forwards the
//! game's force-feedback rumble back to the puck's actuators.
//!
//! ## Precedence — who owns the controller right now
//!
//! Four layers claim the same physical device. Highest wins, and exactly one
//! layer is ever driving:
//!
//! | Rank | Layer | Claim |
//! |---|---|---|
//! | 1 | **Guide** ([`GestureEngine::guide_active`](crate::gesture::GestureEngine::guide_active)) | `Guide` + anything belongs to the desktop and never reaches the app — docs/08's input contract. Holding guide *inside a game* pulls the controller back to hyprpad; the chord layer keeps working in-game exactly as it does on the desktop. |
//! | 2 | **On-screen keyboard** ([`OskHandle::is_active`](crate::osk::OskHandle::is_active)) | While the OSK is up it owns both pads for key selection — including over a game, since `Guide+Y` is meant to work mid-game. |
//! | 3 | **Game forwarding** (this module) | A game-classed window has focus ([`Arbiter::game_focused`](crate::arbitrate::Arbiter::game_focused)) and neither layer above is claiming: every frame goes to the virtual pad. |
//! | 4 | **Desktop** (cursor / scroll / bare buttons) | Everything else. Already self-suppressing via [`Arbiter::suppressed`](crate::arbitrate::Arbiter::suppressed), which is true exactly when a game is focused — so ranks 3 and 4 are mutually exclusive by construction. |
//!
//! The one invariant that matters: **on every transition out of rank 3 the pad
//! is sent [`neutral()`](VirtualGamepad::neutral) exactly once**, so a game can
//! never be left with a stuck stick, a held trigger or a pressed button because
//! the user tabbed away, raised the keyboard, or grabbed the guide button.
//!
//! ## Why uinput and not a HID clone
//!
//! Same reasoning as [`crate::keyboard`]: a kernel evdev device is resolved by
//! every downstream consumer — native Wayland, XWayland, SDL, Proton's
//! `xinput`/`SDL_GAMECONTROLLER` paths — with no protocol translation to get
//! wrong. `/dev/uinput` is user-accessible here via Steam's `uaccess` udev rule.
//! A `uhid`-level clone (docs/06's other option) would buy Steam-Input-specific
//! features we do not need, at the cost of reimplementing a HID descriptor.
//!
//! ## Force feedback
//!
//! The pad advertises `EV_FF`/`FF_RUMBLE` — the same single capability the
//! kernel's own `xpad` and `hid-steam` drivers advertise. uinput does not
//! implement effects itself: it hands each upload/erase back to userspace as an
//! `EV_UINPUT` request to be answered with the
//! `UI_BEGIN_FF_UPLOAD`/`UI_END_FF_UPLOAD` ioctl pair, and each play/stop as an
//! `EV_FF` event. A reader thread owns that protocol and publishes the currently
//! commanded `(strong, weak)` magnitudes into a [`RumbleChannel`] the daemon's
//! main loop samples once per frame (see `run::drive_rumble`).
//!
//! `FF_PERIODIC` is deliberately **not** advertised: honouring it properly means
//! implementing waveforms and envelopes, and advertising a capability we would
//! silently ignore is worse for a game than not advertising it at all.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::report::{Button, Frame};

// ---------------------------------------------------------------------------
// Kernel ABI: event types, codes, ioctl request numbers.
//
// Hand-rolled like `keyboard.rs` (libc only, no `input-linux` dependency). Every
// number below was cross-checked against this machine's `linux/uinput.h` and
// `linux/input-event-codes.h` by compiling a probe that printed them; the struct
// sizes are asserted in the tests, which is what actually keeps this honest.
// ---------------------------------------------------------------------------

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
/// `EV_UINPUT` — not a real input event: the kernel's way of asking *us* to
/// service a force-feedback request. `code` says which, `value` is the request id.
const EV_UINPUT: u16 = 0x0101;
const UI_FF_UPLOAD: u16 = 1;
const UI_FF_ERASE: u16 = 2;

const SYN_REPORT: u16 = 0x00;
const BUS_USB: u16 = 0x03;

const FF_RUMBLE: u16 = 0x50;
/// Master gain, `0..=65535`. uinput does not list it in the device's advertised
/// `ffbit` (it wires its `set_gain` callback up after `input_ff_create` has
/// already published the bits), but a client's `EV_FF`/`FF_GAIN` write still
/// reaches us — verified on-device by halving the gain and watching the
/// published magnitudes halve with it. Honoured because a client that sets it
/// means it.
const FF_GAIN: u16 = 0x60;
const FF_AUTOCENTER: u16 = 0x61;

// Absolute axis codes.
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

// Gamepad button codes. Note `BTN_X == BTN_NORTH` and `BTN_Y == BTN_WEST` in the
// kernel headers, so emitting SOUTH/EAST/NORTH/WEST for A/B/X/Y is *byte
// identical* to what `xpad` emits for a real 360 pad — which is what makes SDL's
// stock mapping for `045e:028e` land correctly.
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;

// uinput ioctl request codes (asm-generic `_IOC` encoding; x86_64 here).
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_SET_FFBIT: libc::c_ulong = 0x4004_556b;
const UI_BEGIN_FF_UPLOAD: libc::c_ulong = 0xc068_55c8;
const UI_END_FF_UPLOAD: libc::c_ulong = 0x4068_55c9;
const UI_BEGIN_FF_ERASE: libc::c_ulong = 0xc00c_55ca;
const UI_END_FF_ERASE: libc::c_ulong = 0x400c_55cb;

/// The identity the pad presents. `045e:028e` is the Microsoft Xbox 360 wired
/// pad — the most widely recognised gamepad id on Linux, present in SDL's
/// built-in mapping table and every engine's controller database. Games see a
/// device they already know how to drive, with no per-title configuration.
const VENDOR: u16 = 0x045e;
const PRODUCT: u16 = 0x028e;
const DEVICE_NAME: &[u8] = b"hyprpad virtual gamepad";

/// How many concurrent force-feedback effects the pad will store. Must be
/// non-zero whenever `EV_FF` is advertised or `UI_DEV_CREATE` fails with
/// `EINVAL`. Sixteen is what `xpad`-class devices offer and far more than any
/// title uses.
const FF_EFFECTS_MAX: u32 = 16;

/// Analog trigger full scale on the wire. The puck reports `0..=32767`; a 360
/// pad's `ABS_Z`/`ABS_RZ` are `0..=255`.
const TRIGGER_MAX: u16 = 32_767;
const TRIGGER_OUT_MAX: u32 = 255;

/// Stick/pad `flat` (dead-zone hint) and `fuzz` published in `absinfo`, copied
/// from `xpad`. Consumers that honour `flat` — SDL does — will ignore the puck
/// sticks' small idle offset (docs/03) without hyprpad filtering it first.
const STICK_FUZZ: i32 = 16;
const STICK_FLAT: i32 = 128;

/// How long the force-feedback reader parks in `poll` before re-checking the
/// stop flag and expiring finished effects. Bounds both shutdown latency and the
/// error on an effect's `replay.length` deadline.
const FF_POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// Kernel ABI structs. Sizes and field offsets are asserted in the tests against
// the values this machine's headers actually produce.
// ---------------------------------------------------------------------------

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [libc::c_char; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

/// `struct ff_effect`'s trailing union. Held as raw bytes with the union's real
/// alignment (8, from the `custom_data` pointer inside `ff_periodic_effect`) so
/// `FfEffect` reproduces the C layout exactly without us modelling five variants
/// we never read. Only the `ff_rumble_effect` arm is decoded, and it lives at
/// offset 0 like every union member: `strong_magnitude` then `weak_magnitude`,
/// both native-endian `u16`.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
struct FfUnion([u8; 32]);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfEffect {
    type_: u16,
    id: i16,
    direction: u16,
    /// `struct ff_trigger { __u16 button; __u16 interval; }`
    trigger_button: u16,
    trigger_interval: u16,
    /// `struct ff_replay { __u16 length; __u16 delay; }`, both milliseconds.
    replay_length: u16,
    replay_delay: u16,
    // (two bytes of padding here, to align the union to 8)
    u: FfUnion,
}

impl FfEffect {
    /// The `ff_rumble_effect` view of the union: `(strong, weak)` magnitudes,
    /// each `0..=65535`. Only meaningful when `type_ == FF_RUMBLE`.
    fn rumble(&self) -> (u16, u16) {
        (
            u16::from_ne_bytes([self.u.0[0], self.u.0[1]]),
            u16::from_ne_bytes([self.u.0[2], self.u.0[3]]),
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UinputFfUpload {
    request_id: u32,
    retval: i32,
    effect: FfEffect,
    old: FfEffect,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UinputFfErase {
    request_id: u32,
    retval: i32,
    effect_id: u32,
}

// ---------------------------------------------------------------------------
// The report: what the pad looks like on the wire for one frame.
// ---------------------------------------------------------------------------

/// The absolute axes the pad advertises, in report order:
/// `(code, min, max, fuzz, flat)`. Indexed by the `AX_*` constants below.
const AXES: [(u16, i32, i32, i32, i32); 8] = [
    (ABS_X, -32768, 32767, STICK_FUZZ, STICK_FLAT),
    (ABS_Y, -32768, 32767, STICK_FUZZ, STICK_FLAT),
    (ABS_RX, -32768, 32767, STICK_FUZZ, STICK_FLAT),
    (ABS_RY, -32768, 32767, STICK_FUZZ, STICK_FLAT),
    (ABS_Z, 0, TRIGGER_OUT_MAX as i32, 0, 0),
    (ABS_RZ, 0, TRIGGER_OUT_MAX as i32, 0, 0),
    (ABS_HAT0X, -1, 1, 0, 0),
    (ABS_HAT0Y, -1, 1, 0, 0),
];

const AX_LX: usize = 0;
const AX_LY: usize = 1;
const AX_RX: usize = 2;
const AX_RY: usize = 3;
/// `ABS_Z` — the **left** trigger, per `xpad`.
const AX_LT: usize = 4;
/// `ABS_RZ` — the **right** trigger, per `xpad`.
const AX_RT: usize = 5;
const AX_HAT_X: usize = 6;
const AX_HAT_Y: usize = 7;

/// The buttons the pad advertises, in report order. Indexed by the `K_*`
/// constants below; one bit per entry in [`PadReport::keys`].
const KEYS: [u16; 11] = [
    BTN_SOUTH,
    BTN_EAST,
    BTN_NORTH,
    BTN_WEST,
    BTN_TL,
    BTN_TR,
    BTN_SELECT,
    BTN_START,
    BTN_MODE,
    BTN_THUMBL,
    BTN_THUMBR,
];

const K_SOUTH: usize = 0;
const K_EAST: usize = 1;
const K_NORTH: usize = 2;
const K_WEST: usize = 3;
const K_TL: usize = 4;
const K_TR: usize = 5;
const K_SELECT: usize = 6;
const K_START: usize = 7;
const K_MODE: usize = 8;
const K_THUMBL: usize = 9;
const K_THUMBR: usize = 10;

/// One complete virtual-pad state — every axis and every button, so a report is
/// always a total picture rather than a diff. Building it is pure, which is what
/// makes the whole frame→pad mapping unit-testable with no device.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PadReport {
    /// Axis values, parallel to [`AXES`] and indexed by the `AX_*` constants.
    abs: [i32; AXES.len()],
    /// Button bitmask, one bit per [`KEYS`] entry, indexed by the `K_*` constants.
    keys: u16,
}

impl PadReport {
    /// Everything centred, released and zeroed — what a game must see the
    /// instant hyprpad stops forwarding.
    pub fn neutral() -> PadReport {
        PadReport::default()
    }

    /// Map one decoded controller frame onto the virtual pad.
    ///
    /// | Puck | Virtual pad | Note |
    /// |---|---|---|
    /// | left stick | `ABS_X` / `ABS_Y` | Y negated: the puck reports `+Y` up, evdev wants `+Y` down |
    /// | right pad (while touched) | `ABS_RX` / `ABS_RY` | absolute deflection, Y negated the same way |
    /// | right stick (pad lifted) | `ABS_RX` / `ABS_RY` | the puck has a real right stick; stranding it would be a bug |
    /// | `l2` / `r2` (0..32767) | `ABS_Z` / `ABS_RZ` (0..255) | `xpad`'s trigger assignment |
    /// | D-pad | `ABS_HAT0X` / `ABS_HAT0Y` | `-1`/`+1`, `HAT0Y` negative is up |
    /// | A / B / X / Y | `BTN_SOUTH` / `EAST` / `NORTH` / `WEST` | identical to `xpad`'s `BTN_A/B/X/Y` |
    /// | L1 / R1 | `BTN_TL` / `BTN_TR` | |
    /// | L3 | `BTN_THUMBL` | |
    /// | R3 **or** right-pad click | `BTN_THUMBR` | both drive `RS`, so both click it |
    /// | Menu / View | `BTN_START` / `BTN_SELECT` | |
    /// | Steam (guide) | `BTN_MODE`, **only if `forward_guide`** | otherwise it stays hyprpad's modifier |
    ///
    /// Not forwarded in v1: the four grips (`GripL4/L5/R4/R5`), `QuickAccess`,
    /// the left pad (it is the desktop scroller and the OSK's left cursor), and
    /// the capacitive flags. TODO: expose the grips as `BTN_TRIGGER_HAPPY1..4`
    /// once there is a config surface to bind them.
    ///
    /// `TriggerL2Full`/`TriggerR2Full` are intentionally dropped too — they are
    /// the digital edge of a pull the analog `ABS_Z`/`ABS_RZ` already reports at
    /// full scale.
    pub fn from_frame(frame: &Frame, forward_guide: bool) -> PadReport {
        let mut r = PadReport::default();

        r.abs[AX_LX] = axis(frame.left_stick.0);
        r.abs[AX_LY] = axis_inverted(frame.left_stick.1);

        // The right pad wins while a thumb is on it (Steam Controller muscle
        // memory: the pad *is* the right stick), and hands back to the physical
        // right stick on lift — which reads as centred when untouched, so the
        // "neutral on lift" contract still holds.
        let (rx, ry) = if frame.pressed(Button::PadRightTouch) {
            (frame.right_pad.x, frame.right_pad.y)
        } else {
            frame.right_stick
        };
        r.abs[AX_RX] = axis(rx);
        r.abs[AX_RY] = axis_inverted(ry);

        r.abs[AX_LT] = trigger(frame.l2);
        r.abs[AX_RT] = trigger(frame.r2);

        r.abs[AX_HAT_X] = i32::from(frame.pressed(Button::DpadRight))
            - i32::from(frame.pressed(Button::DpadLeft));
        r.abs[AX_HAT_Y] =
            i32::from(frame.pressed(Button::DpadDown)) - i32::from(frame.pressed(Button::DpadUp));

        let mut set = |slot: usize, on: bool| {
            if on {
                r.keys |= 1 << slot;
            }
        };
        set(K_SOUTH, frame.pressed(Button::A));
        set(K_EAST, frame.pressed(Button::B));
        set(K_NORTH, frame.pressed(Button::X));
        set(K_WEST, frame.pressed(Button::Y));
        set(K_TL, frame.pressed(Button::BumperL1));
        set(K_TR, frame.pressed(Button::BumperR1));
        set(K_SELECT, frame.pressed(Button::View));
        set(K_START, frame.pressed(Button::Menu));
        set(K_THUMBL, frame.pressed(Button::L3));
        set(
            K_THUMBR,
            frame.pressed(Button::R3) || frame.pressed(Button::PadRightClick),
        );
        // The guide button is hyprpad's global modifier (docs/08 input
        // contract); forwarding it hands Steam's overlay back its own chord
        // layer, which is opt-in only.
        set(K_MODE, forward_guide && frame.pressed(Button::Steam));

        r
    }

    /// Whether this report is fully neutral. Used by tests and by the daemon's
    /// "did the transition actually release everything?" assertions.
    pub fn is_neutral(&self) -> bool {
        *self == PadReport::neutral()
    }
}

/// Widen a puck axis to the wire, clamped to the advertised range.
fn axis(v: i16) -> i32 {
    i32::from(v).clamp(-32768, 32767)
}

/// Widen and **negate** a puck axis: the puck reports `+Y` up, evdev `+Y` down.
/// `i16::MIN` has no positive counterpart, so the clamp is load-bearing.
fn axis_inverted(v: i16) -> i32 {
    (-i32::from(v)).clamp(-32768, 32767)
}

/// Scale an analog trigger from the puck's `0..=32767` to the pad's `0..=255`,
/// rounded to nearest.
fn trigger(v: u16) -> i32 {
    let v = u32::from(v.min(TRIGGER_MAX));
    let full = u32::from(TRIGGER_MAX);
    ((v * TRIGGER_OUT_MAX + full / 2) / full) as i32
}

// ---------------------------------------------------------------------------
// Rumble: the FF back-channel from the game to the puck.
// ---------------------------------------------------------------------------

/// The rumble the game is currently asking for, published by the force-feedback
/// reader thread and sampled by the daemon's main loop once per frame.
///
/// Lock-free on purpose: the loop services a ~250 Hz report stream and must
/// never block on the reader, and a rumble command is small enough to fit in two
/// atomics. Consistency between the two halves of `magnitudes` matters (they are
/// one command), which is why they share a single `AtomicU32` rather than two.
#[derive(Debug)]
pub struct RumbleChannel {
    /// `strong << 16 | weak`, the two `FF_RUMBLE` magnitudes, each `0..=65535`.
    magnitudes: AtomicU32,
    /// The `FF_GAIN` master gain the client set, `0..=65535`. Full scale until a
    /// client says otherwise.
    gain: AtomicU32,
}

impl Default for RumbleChannel {
    fn default() -> RumbleChannel {
        RumbleChannel {
            magnitudes: AtomicU32::new(0),
            gain: AtomicU32::new(u32::from(u16::MAX)),
        }
    }
}

impl RumbleChannel {
    fn publish(&self, strong: u16, weak: u16) {
        self.magnitudes
            .store((u32::from(strong) << 16) | u32::from(weak), Ordering::Relaxed);
    }

    /// The `(strong, weak)` magnitudes currently commanded, **already scaled by
    /// the client's `FF_GAIN`** — that is what "gain" means to a client, and
    /// folding it in here keeps the caller from having to know about it.
    pub fn magnitudes(&self) -> (u16, u16) {
        let m = self.magnitudes.load(Ordering::Relaxed);
        let gain = self.gain.load(Ordering::Relaxed).min(u32::from(u16::MAX)) as u16;
        let (strong, weak) = ((m >> 16) as u16, m as u16);
        (apply_gain(strong, gain), apply_gain(weak, gain))
    }
}

/// Scale a magnitude by a `0..=65535` gain. `65535` is unity.
fn apply_gain(magnitude: u16, gain: u16) -> u16 {
    if gain == u16::MAX {
        return magnitude;
    }
    ((u32::from(magnitude) * u32::from(gain)) / u32::from(u16::MAX)) as u16
}

/// Scale a magnitude by the `[gamepad] rumble_intensity` knob, saturating at
/// full scale. A non-finite or non-positive scale silences rumble rather than
/// producing a garbage magnitude (use `rumble = false` to switch it off
/// properly) — the same belt-and-braces policy as `[haptics] intensity`.
pub fn scale_magnitude(magnitude: u16, intensity: f64) -> u16 {
    if !intensity.is_finite() || intensity <= 0.0 {
        return 0;
    }
    let scaled = f64::from(magnitude) * intensity;
    scaled.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// One stored force-feedback effect: the magnitudes it plays and how long one
/// repetition lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StoredEffect {
    strong: u16,
    weak: u16,
    /// `replay.delay + replay.length` in milliseconds. `0` means the effect runs
    /// until the client stops it.
    period_ms: u32,
}

/// An effect currently playing, and when it ends.
#[derive(Clone, Copy, Debug)]
struct Playing {
    strong: u16,
    weak: u16,
    /// `None` for an effect with no `replay.length` — it runs until stopped.
    until: Option<Instant>,
}

/// Combine every playing effect into one command, by taking the strongest of
/// each channel. Real hardware mixes; taking the max is the conventional
/// approximation and is what a single-effect title (nearly all of them) reduces
/// to exactly. Pure, so the mixing and expiry rules are unit-testable.
fn mix(playing: &HashMap<i16, Playing>, now: Instant) -> (u16, u16) {
    playing
        .values()
        .filter(|p| p.until.is_none_or(|t| t > now))
        .fold((0u16, 0u16), |(s, w), p| (s.max(p.strong), w.max(p.weak)))
}

// ---------------------------------------------------------------------------
// The device.
// ---------------------------------------------------------------------------

/// A kernel-level virtual gamepad created through `/dev/uinput`.
///
/// Created **lazily**, on the first frame the daemon actually wants to forward,
/// so a session that never focuses a game never creates a device and never
/// starts the force-feedback thread. Dropping it destroys the device (games see
/// a normal unplug) and stops the reader.
pub struct VirtualGamepad {
    /// Write side, owned by the main loop.
    file: File,
    /// The last report actually written, so a 250 Hz frame stream only emits the
    /// axes and buttons that changed.
    sent: PadReport,
    /// Cleared until the first report, which is therefore written in full.
    primed: bool,
    /// The rumble the game is asking for, published by the reader thread.
    rumble: Arc<RumbleChannel>,
    /// Tells the reader thread to wind up; it checks between `poll` timeouts.
    stop: Arc<AtomicBool>,
}

impl VirtualGamepad {
    /// Create the uinput gamepad: register the capability set, publish the
    /// `045e:028e` identity, and spawn the force-feedback reader.
    ///
    /// Every failure is reported as an `Err` for the caller to log once and
    /// degrade on — a machine with no `/dev/uinput` still gets the whole desktop
    /// layer, it just cannot feed a game.
    pub fn new() -> Result<VirtualGamepad, String> {
        let file = OpenOptions::new()
            // Read access is not optional here (unlike `keyboard.rs`): the
            // force-feedback protocol is delivered *back* to us on this fd.
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| format!("open /dev/uinput: {e} (Steam's uaccess udev rule grants it)"))?;
        let fd = file.as_raw_fd();

        // SAFETY: `fd` is a valid, open uinput fd for the duration of this
        // block. Each ioctl is the documented uinput setup call with the
        // argument type its request code encodes.
        unsafe {
            for ev in [EV_KEY, EV_ABS, EV_FF, EV_SYN] {
                set_bit(fd, UI_SET_EVBIT, libc::c_int::from(ev))?;
            }
            for code in KEYS {
                set_bit(fd, UI_SET_KEYBIT, libc::c_int::from(code))?;
            }
            set_bit(fd, UI_SET_FFBIT, libc::c_int::from(FF_RUMBLE))?;

            // `UI_ABS_SETUP` sets the axis' `absbit` *and* publishes its range,
            // which is the part `keyboard.rs` never needed: a gamepad whose
            // sticks report no `absinfo` is unusable, because every consumer
            // normalises against the advertised min/max.
            for &(code, minimum, maximum, fuzz, flat) in &AXES {
                set_bit(fd, UI_SET_ABSBIT, libc::c_int::from(code))?;
                let setup = UinputAbsSetup {
                    code,
                    absinfo: InputAbsinfo {
                        value: 0,
                        minimum,
                        maximum,
                        fuzz,
                        flat,
                        resolution: 0,
                    },
                };
                if libc::ioctl(fd, UI_ABS_SETUP, &setup as *const UinputAbsSetup) < 0 {
                    return Err(format!(
                        "UI_ABS_SETUP(code {code:#x}): {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }

            let mut setup: UinputSetup = std::mem::zeroed();
            setup.id = InputId {
                bustype: BUS_USB,
                vendor: VENDOR,
                product: PRODUCT,
                version: 0x0114,
            };
            for (i, &b) in DEVICE_NAME.iter().enumerate() {
                setup.name[i] = b as libc::c_char;
            }
            // Must be non-zero whenever EV_FF is set, or UI_DEV_CREATE refuses.
            setup.ff_effects_max = FF_EFFECTS_MAX;
            if libc::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup) < 0 {
                return Err(format!("UI_DEV_SETUP: {}", std::io::Error::last_os_error()));
            }
            if libc::ioctl(fd, UI_DEV_CREATE) < 0 {
                return Err(format!("UI_DEV_CREATE: {}", std::io::Error::last_os_error()));
            }
        }

        // The reader needs its own handle. `try_clone` dups the descriptor, so
        // both refer to the same open file description: the ioctls the
        // force-feedback protocol needs work on either, and reading on one while
        // the loop writes on the other is exactly what uinput expects.
        let reader_fd = file
            .try_clone()
            .map_err(|e| format!("dup /dev/uinput for the force-feedback reader: {e}"))?;

        let rumble = Arc::new(RumbleChannel::default());
        let stop = Arc::new(AtomicBool::new(false));
        {
            let rumble = Arc::clone(&rumble);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || ff_reader(reader_fd, &rumble, &stop));
        }

        let mut pad = VirtualGamepad {
            file,
            sent: PadReport::neutral(),
            primed: false,
            rumble,
            stop,
        };
        // Publish a full neutral report immediately, so the device's very first
        // state on the wire is a known-good one rather than whatever the kernel
        // initialised `absinfo.value` to.
        pad.neutral();
        Ok(pad)
    }

    /// Forward one decoded controller frame to the pad, as a single
    /// SYN-terminated report carrying only what changed.
    pub fn apply(&mut self, frame: &Frame, forward_guide: bool) {
        self.send(PadReport::from_frame(frame, forward_guide));
    }

    /// Release everything: sticks centred, triggers zero, hats zero, every
    /// button up. Sent on every transition out of forwarding so a game is never
    /// left holding input hyprpad has stopped feeding it. Idempotent — a second
    /// call writes nothing.
    pub fn neutral(&mut self) {
        self.send(PadReport::neutral());
    }

    /// The rumble channel this pad's force-feedback reader publishes to.
    pub fn rumble(&self) -> &RumbleChannel {
        &self.rumble
    }

    /// Diff `want` against the last report and write the difference, terminated
    /// by one `SYN_REPORT`. Nothing changed means nothing is written at all,
    /// which is what keeps a 250 Hz stream of identical frames off the bus.
    fn send(&mut self, want: PadReport) {
        // At most every axis, every button, and the terminating SYN.
        let mut batch = [InputEvent::default(); AXES.len() + KEYS.len() + 1];
        let mut n = 0;
        for (i, &(code, ..)) in AXES.iter().enumerate() {
            if !self.primed || want.abs[i] != self.sent.abs[i] {
                batch[n] = event(EV_ABS, code, want.abs[i]);
                n += 1;
            }
        }
        for (i, &code) in KEYS.iter().enumerate() {
            let down = want.keys & (1 << i) != 0;
            let was_down = self.sent.keys & (1 << i) != 0;
            if !self.primed || down != was_down {
                batch[n] = event(EV_KEY, code, i32::from(down));
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        batch[n] = event(EV_SYN, SYN_REPORT, 0);
        n += 1;

        // SAFETY: `InputEvent` is `#[repr(C)]` and `Copy`; we only view the
        // first `n` events' bytes to write them to the uinput fd.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                batch.as_ptr().cast::<u8>(),
                n * std::mem::size_of::<InputEvent>(),
            )
        };
        if let Err(e) = self.file.write_all(bytes) {
            eprintln!("hyprpad: virtual gamepad write failed: {e}");
        }
        self.sent = want;
        self.primed = true;
    }
}

impl Drop for VirtualGamepad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // SAFETY: destroying the device on the same valid fd we created it with.
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

/// One evdev event, timestamped by the kernel (a zero `tv_*` is the documented
/// "stamp it for me" convention on the uinput write path).
fn event(type_: u16, code: u16, value: i32) -> InputEvent {
    InputEvent {
        tv_sec: 0,
        tv_usec: 0,
        type_,
        code,
        value,
    }
}

/// Issue a `UI_SET_*BIT`-style ioctl carrying a single `int`. Mirrors
/// [`crate::keyboard`]'s helper.
///
/// # Safety
/// `fd` must be an open `/dev/uinput` file descriptor.
unsafe fn set_bit(fd: libc::c_int, req: libc::c_ulong, val: libc::c_int) -> Result<(), String> {
    if libc::ioctl(fd, req, val) < 0 {
        return Err(format!(
            "uinput ioctl {req:#x}({val}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The force-feedback reader thread.
// ---------------------------------------------------------------------------

/// Own the uinput force-feedback protocol on `fd` for the life of the device.
///
/// uinput turns each of a client's `ioctl`-level effect operations into a
/// request we must answer:
///
/// * `EV_UINPUT`/`UI_FF_UPLOAD` — the client uploaded an effect. Fetch it with
///   `UI_BEGIN_FF_UPLOAD`, store its magnitudes, answer `UI_END_FF_UPLOAD`.
/// * `EV_UINPUT`/`UI_FF_ERASE` — likewise, for a deleted effect.
/// * `EV_FF`/`<effect id>` — play (`value` = repeat count) or stop (`value` 0).
/// * `EV_FF`/`FF_GAIN` — master gain.
///
/// Nothing here touches the device: it only publishes the resulting `(strong,
/// weak)` command into `rumble` for the daemon's main loop to act on, which
/// keeps the puck writes on the one thread that owns the haptics queue.
fn ff_reader(mut fd: File, rumble: &Arc<RumbleChannel>, stop: &Arc<AtomicBool>) {
    let mut effects: HashMap<i16, StoredEffect> = HashMap::new();
    let mut playing: HashMap<i16, Playing> = HashMap::new();
    let mut buf = [0u8; std::mem::size_of::<InputEvent>() * 64];
    let raw = fd.as_raw_fd();

    while !stop.load(Ordering::Relaxed) {
        if poll_readable(raw, FF_POLL_INTERVAL) {
            let n = match fd.read(&mut buf) {
                Ok(0) => return, // device gone
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            for chunk in buf[..n].chunks_exact(std::mem::size_of::<InputEvent>()) {
                // SAFETY: `InputEvent` is `#[repr(C)]`, `Copy`, and made only of
                // integers, so any 24 bytes are a valid value; `chunk` is
                // exactly that long and the read comes from the kernel's own
                // `input_event` queue.
                let ev: InputEvent =
                    unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast::<InputEvent>()) };
                handle_ff_event(raw, &ev, &mut effects, &mut playing, rumble);
            }
        }
        // Retire finished effects and republish, whether or not anything was
        // read: an effect with a `replay.length` ends on a clock, not an event.
        let now = Instant::now();
        playing.retain(|_, p| p.until.is_none_or(|t| t > now));
        let (strong, weak) = mix(&playing, now);
        rumble.publish(strong, weak);
    }
    // Winding up (the device is being destroyed): leave nothing commanded.
    rumble.publish(0, 0);
}

/// Block for at most `timeout` waiting for `fd` to become readable. Returns
/// whether it did, so the caller can re-check its stop flag on every timeout.
fn poll_readable(fd: libc::c_int, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a live, initialised single-element array for the duration
    // of the call, holding an fd this thread has a handle to.
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int) };
    ret > 0 && pfd.revents & libc::POLLIN != 0
}

/// Dispatch one event from the uinput fd. Split out from [`ff_reader`] so the
/// protocol is readable in one screen.
fn handle_ff_event(
    fd: libc::c_int,
    ev: &InputEvent,
    effects: &mut HashMap<i16, StoredEffect>,
    playing: &mut HashMap<i16, Playing>,
    rumble: &Arc<RumbleChannel>,
) {
    match (ev.type_, ev.code) {
        (EV_UINPUT, UI_FF_UPLOAD) => ff_upload(fd, ev.value as u32, effects),
        (EV_UINPUT, UI_FF_ERASE) => ff_erase(fd, ev.value as u32, effects, playing),
        (EV_FF, FF_GAIN) => {
            rumble
                .gain
                .store(ev.value.clamp(0, i32::from(u16::MAX)) as u32, Ordering::Relaxed);
        }
        // Autocentre is a wheel concept; a gamepad has nothing to do with it.
        (EV_FF, FF_AUTOCENTER) => {}
        // Anything else on EV_FF addresses an effect by id. Effect ids are
        // allocated from 0 upward and bounded by `FF_EFFECTS_MAX` (16), well
        // below `FF_GAIN` (0x60), so the two can never collide.
        (EV_FF, code) => {
            let id = code as i16;
            if ev.value > 0 {
                if let Some(e) = effects.get(&id) {
                    let repeats = ev.value.max(1) as u32;
                    let until = (e.period_ms > 0).then(|| {
                        Instant::now()
                            + Duration::from_millis(u64::from(e.period_ms) * u64::from(repeats))
                    });
                    playing.insert(
                        id,
                        Playing {
                            strong: e.strong,
                            weak: e.weak,
                            until,
                        },
                    );
                }
            } else {
                playing.remove(&id);
            }
        }
        _ => {}
    }
}

/// Answer a `UI_FF_UPLOAD` request: fetch the effect the client uploaded, keep
/// the bit of it we can honour, and report the outcome back.
fn ff_upload(fd: libc::c_int, request_id: u32, effects: &mut HashMap<i16, StoredEffect>) {
    let mut up = UinputFfUpload {
        request_id,
        ..UinputFfUpload::default()
    };
    // SAFETY: `up` is a live, correctly-sized `uinput_ff_upload`; the kernel
    // fills `effect`/`old` in place (the request code is `_IOWR`).
    if unsafe { libc::ioctl(fd, UI_BEGIN_FF_UPLOAD, &mut up as *mut UinputFfUpload) } < 0 {
        return;
    }
    if up.effect.type_ == FF_RUMBLE {
        let (strong, weak) = up.effect.rumble();
        effects.insert(
            up.effect.id,
            StoredEffect {
                strong,
                weak,
                period_ms: u32::from(up.effect.replay_delay) + u32::from(up.effect.replay_length),
            },
        );
        up.retval = 0;
    } else {
        // Only FF_RUMBLE is advertised, so this should be unreachable; refuse
        // rather than silently accept an effect that would never play.
        up.retval = -libc::EINVAL;
    }
    // SAFETY: same live struct, now carrying our `retval`.
    unsafe {
        libc::ioctl(fd, UI_END_FF_UPLOAD, &up as *const UinputFfUpload);
    }
}

/// Answer a `UI_FF_ERASE` request: forget the effect, and stop it if it happened
/// to be playing.
fn ff_erase(
    fd: libc::c_int,
    request_id: u32,
    effects: &mut HashMap<i16, StoredEffect>,
    playing: &mut HashMap<i16, Playing>,
) {
    let mut er = UinputFfErase {
        request_id,
        ..UinputFfErase::default()
    };
    // SAFETY: `er` is a live, correctly-sized `uinput_ff_erase`; the kernel
    // fills `effect_id` in place (the request code is `_IOWR`).
    if unsafe { libc::ioctl(fd, UI_BEGIN_FF_ERASE, &mut er as *mut UinputFfErase) } < 0 {
        return;
    }
    let id = er.effect_id as i16;
    effects.remove(&id);
    playing.remove(&id);
    er.retval = 0;
    // SAFETY: same live struct, now carrying our `retval`.
    unsafe {
        libc::ioctl(fd, UI_END_FF_ERASE, &er as *const UinputFfErase);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes and offsets this machine's `linux/uinput.h` actually produces.
    /// Everything the module does with raw pointers rests on these.
    #[test]
    fn kernel_abi_structs_have_the_right_shape() {
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<InputId>(), 8);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<InputAbsinfo>(), 24);
        assert_eq!(std::mem::size_of::<UinputAbsSetup>(), 28);
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        assert_eq!(std::mem::align_of::<FfEffect>(), 8);
        assert_eq!(std::mem::size_of::<UinputFfUpload>(), 104);
        assert_eq!(std::mem::size_of::<UinputFfErase>(), 12);
        // The union sits at offset 16 (two bytes of tail padding after
        // `replay_delay`), and the two `ff_effect`s in an upload at 8 and 56.
        assert_eq!(std::mem::offset_of!(FfEffect, u), 16);
        assert_eq!(std::mem::offset_of!(FfEffect, replay_length), 10);
        assert_eq!(std::mem::offset_of!(UinputFfUpload, effect), 8);
        assert_eq!(std::mem::offset_of!(UinputFfUpload, old), 56);
        assert_eq!(std::mem::offset_of!(UinputAbsSetup, absinfo), 4);
    }

    /// A frame with a set of buttons pressed and nothing else.
    fn pressed(buttons: &[Button]) -> Frame {
        let mut f = Frame::default();
        for &b in buttons {
            // Round-trip through the public API: find the bit `pressed` reads.
            let mut probe = Frame::default();
            for bit in 0..32 {
                probe.buttons = 1 << bit;
                if probe.pressed(b) {
                    f.buttons |= 1 << bit;
                    break;
                }
            }
        }
        f
    }

    #[test]
    fn neutral_is_all_zeroes() {
        let n = PadReport::neutral();
        assert!(n.is_neutral());
        assert_eq!(n.abs, [0; AXES.len()]);
        assert_eq!(n.keys, 0);
        // An all-zero frame maps to the neutral report, so a game that loses
        // forwarding and one that is fed an idle controller see the same thing.
        assert_eq!(PadReport::from_frame(&Frame::default(), false), n);
    }

    #[test]
    fn left_stick_maps_to_abs_xy_with_y_inverted() {
        let mut f = Frame {
            left_stick: (1000, 2000),
            ..Frame::default()
        };
        let r = PadReport::from_frame(&f, false);
        assert_eq!(r.abs[AX_LX], 1000);
        // Pad +Y is up; evdev +Y is down.
        assert_eq!(r.abs[AX_LY], -2000);

        // The extreme has no positive counterpart in i16; it must clamp rather
        // than wrap to a full-deflection *negative* value.
        f.left_stick = (i16::MIN, i16::MIN);
        let r = PadReport::from_frame(&f, false);
        assert_eq!(r.abs[AX_LX], -32768);
        assert_eq!(r.abs[AX_LY], 32767);
    }

    #[test]
    fn right_pad_drives_rs_while_touched_and_hands_back_on_lift() {
        let mut f = pressed(&[Button::PadRightTouch]);
        f.right_pad = crate::report::Pad {
            x: -4000,
            y: 8000,
            force: 0,
        };
        f.right_stick = (500, 500);
        let r = PadReport::from_frame(&f, false);
        assert_eq!(r.abs[AX_RX], -4000, "the touched pad owns RS");
        assert_eq!(r.abs[AX_RY], -8000, "and its Y is inverted too");

        // Lift: the physical right stick takes RS back. A pad that is not
        // touched must contribute nothing, however stale its last position.
        let lifted = Frame {
            right_pad: f.right_pad,
            right_stick: (500, -500),
            ..Frame::default()
        };
        let r = PadReport::from_frame(&lifted, false);
        assert_eq!(r.abs[AX_RX], 500);
        assert_eq!(r.abs[AX_RY], 500);
    }

    #[test]
    fn triggers_scale_to_the_360_range() {
        let mut f = Frame {
            l2: 0,
            r2: 32767,
            ..Frame::default()
        };
        let r = PadReport::from_frame(&f, false);
        assert_eq!(r.abs[AX_LT], 0);
        assert_eq!(r.abs[AX_RT], 255, "a full pull must reach full scale");

        // Half travel lands mid-scale, rounded to nearest.
        f.l2 = 16384;
        assert_eq!(PadReport::from_frame(&f, false).abs[AX_LT], 128);
        // And L2 is ABS_Z, R2 is ABS_RZ — xpad's assignment, not the reverse.
        f.l2 = 32767;
        f.r2 = 0;
        let r = PadReport::from_frame(&f, false);
        assert_eq!((r.abs[AX_LT], r.abs[AX_RT]), (255, 0));
        // Out-of-range input (the field is a u16) clamps rather than wraps.
        f.l2 = u16::MAX;
        assert_eq!(PadReport::from_frame(&f, false).abs[AX_LT], 255);
    }

    #[test]
    fn dpad_encodes_as_hat_axes_with_up_negative() {
        let r = PadReport::from_frame(&pressed(&[Button::DpadUp]), false);
        assert_eq!((r.abs[AX_HAT_X], r.abs[AX_HAT_Y]), (0, -1));
        let r = PadReport::from_frame(&pressed(&[Button::DpadDown]), false);
        assert_eq!((r.abs[AX_HAT_X], r.abs[AX_HAT_Y]), (0, 1));
        let r = PadReport::from_frame(&pressed(&[Button::DpadLeft]), false);
        assert_eq!((r.abs[AX_HAT_X], r.abs[AX_HAT_Y]), (-1, 0));
        let r = PadReport::from_frame(&pressed(&[Button::DpadRight]), false);
        assert_eq!((r.abs[AX_HAT_X], r.abs[AX_HAT_Y]), (1, 0));
        // A diagonal is both axes at once.
        let r = PadReport::from_frame(&pressed(&[Button::DpadUp, Button::DpadRight]), false);
        assert_eq!((r.abs[AX_HAT_X], r.abs[AX_HAT_Y]), (1, -1));
        // Opposing presses cancel rather than latching one side.
        let r = PadReport::from_frame(&pressed(&[Button::DpadLeft, Button::DpadRight]), false);
        assert_eq!(r.abs[AX_HAT_X], 0);
    }

    #[test]
    fn face_and_shoulder_buttons_map_to_the_xbox_layout() {
        let cases = [
            (Button::A, K_SOUTH),
            (Button::B, K_EAST),
            (Button::X, K_NORTH),
            (Button::Y, K_WEST),
            (Button::BumperL1, K_TL),
            (Button::BumperR1, K_TR),
            (Button::View, K_SELECT),
            (Button::Menu, K_START),
            (Button::L3, K_THUMBL),
            (Button::R3, K_THUMBR),
            (Button::PadRightClick, K_THUMBR),
        ];
        for (button, slot) in cases {
            let r = PadReport::from_frame(&pressed(&[button]), false);
            assert_eq!(r.keys, 1 << slot, "{button:?} should press KEYS[{slot}]");
        }
        // The codes themselves are xpad's, which is what makes SDL's stock
        // 045e:028e mapping correct.
        assert_eq!(KEYS[K_SOUTH], 0x130);
        assert_eq!(KEYS[K_NORTH], 0x133);
        assert_eq!(KEYS[K_WEST], 0x134);
    }

    #[test]
    fn guide_is_withheld_unless_forwarding_is_configured() {
        let f = pressed(&[Button::Steam]);
        // Default: the guide button is hyprpad's modifier and never reaches the
        // game — the docs/08 input contract.
        assert!(PadReport::from_frame(&f, false).is_neutral());
        // Opt in and it becomes BTN_MODE.
        assert_eq!(PadReport::from_frame(&f, true).keys, 1 << K_MODE);
        assert_eq!(KEYS[K_MODE], 0x13c);
    }

    #[test]
    fn grips_and_capacitive_flags_are_not_forwarded_in_v1() {
        for b in [
            Button::GripL4,
            Button::GripL5,
            Button::GripR4,
            Button::GripR5,
            Button::QuickAccess,
            Button::Cap0,
            Button::PadLeftTouch,
            Button::PadLeftClick,
            // The digital full-pull is redundant with the analog axis.
            Button::TriggerL2Full,
            Button::TriggerR2Full,
        ] {
            assert!(
                PadReport::from_frame(&pressed(&[b]), true).is_neutral(),
                "{b:?} must not reach the game in v1"
            );
        }
    }

    #[test]
    fn gain_scales_magnitudes_and_unity_is_lossless() {
        assert_eq!(apply_gain(30000, u16::MAX), 30000);
        assert_eq!(apply_gain(u16::MAX, u16::MAX), u16::MAX);
        assert_eq!(apply_gain(u16::MAX, 0), 0);
        // Half gain halves the magnitude (to within integer truncation).
        let half = apply_gain(u16::MAX, u16::MAX / 2);
        assert!((32_000..=32_768).contains(&half), "got {half}");
    }

    #[test]
    fn rumble_intensity_scales_and_saturates() {
        assert_eq!(scale_magnitude(30_000, 1.0), 30_000);
        assert_eq!(scale_magnitude(30_000, 0.5), 15_000);
        // Above full scale saturates rather than wrapping.
        assert_eq!(scale_magnitude(50_000, 2.0), u16::MAX);
        // Nonsense scales silence rumble rather than emitting garbage.
        assert_eq!(scale_magnitude(30_000, 0.0), 0);
        assert_eq!(scale_magnitude(30_000, -1.0), 0);
        assert_eq!(scale_magnitude(30_000, f64::NAN), 0);
    }

    #[test]
    fn rumble_channel_reports_gain_scaled_magnitudes() {
        let ch = RumbleChannel::default();
        assert_eq!(ch.magnitudes(), (0, 0), "silent until a client asks");
        ch.publish(40_000, 10_000);
        assert_eq!(ch.magnitudes(), (40_000, 10_000));
        // A client that turns the master gain down turns the rumble down.
        ch.gain.store(0, Ordering::Relaxed);
        assert_eq!(ch.magnitudes(), (0, 0));
    }

    #[test]
    fn mixing_takes_the_strongest_channel_and_drops_expired_effects() {
        let now = Instant::now();
        let mut playing = HashMap::new();
        // One endless effect and one that has already finished.
        playing.insert(
            0,
            Playing {
                strong: 20_000,
                weak: 5_000,
                until: None,
            },
        );
        playing.insert(
            1,
            Playing {
                strong: 60_000,
                weak: 1_000,
                until: Some(now - Duration::from_millis(1)),
            },
        );
        assert_eq!(mix(&playing, now), (20_000, 5_000), "expired must not mix in");

        // A second live effect mixes per channel, taking the strongest of each.
        playing.insert(
            2,
            Playing {
                strong: 1_000,
                weak: 50_000,
                until: Some(now + Duration::from_secs(1)),
            },
        );
        assert_eq!(mix(&playing, now), (20_000, 50_000));
        // Nothing playing is silence.
        assert_eq!(mix(&HashMap::new(), now), (0, 0));
    }

    #[test]
    fn ff_effect_union_decodes_the_rumble_magnitudes() {
        let mut e = FfEffect {
            type_: FF_RUMBLE,
            ..Default::default()
        };
        // `struct ff_rumble_effect { __u16 strong_magnitude; __u16 weak_magnitude; }`
        // at the union's offset 0, native-endian.
        e.u.0[0..2].copy_from_slice(&40_000u16.to_ne_bytes());
        e.u.0[2..4].copy_from_slice(&9_000u16.to_ne_bytes());
        assert_eq!(e.rumble(), (40_000, 9_000));
    }

    #[test]
    fn every_axis_and_button_is_reachable_from_its_index() {
        // The `AX_*`/`K_*` constants index the tables; a mismatch would silently
        // send the wrong axis, so pin them.
        assert_eq!(AXES[AX_LX].0, ABS_X);
        assert_eq!(AXES[AX_LY].0, ABS_Y);
        assert_eq!(AXES[AX_RX].0, ABS_RX);
        assert_eq!(AXES[AX_RY].0, ABS_RY);
        assert_eq!(AXES[AX_LT].0, ABS_Z);
        assert_eq!(AXES[AX_RT].0, ABS_RZ);
        assert_eq!(AXES[AX_HAT_X].0, ABS_HAT0X);
        assert_eq!(AXES[AX_HAT_Y].0, ABS_HAT0Y);
        assert!(KEYS.len() <= 16, "PadReport::keys is a u16 bitmask");
        // Effect ids run 0..FF_EFFECTS_MAX and must stay clear of FF_GAIN.
        assert!(FF_EFFECTS_MAX < u32::from(FF_GAIN));
    }

    #[test]
    fn stick_ranges_are_advertised_the_way_xpad_advertises_them() {
        for i in [AX_LX, AX_LY, AX_RX, AX_RY] {
            let (_, min, max, fuzz, flat) = AXES[i];
            assert_eq!((min, max), (-32768, 32767));
            assert_eq!((fuzz, flat), (STICK_FUZZ, STICK_FLAT));
        }
        for i in [AX_LT, AX_RT] {
            let (_, min, max, ..) = AXES[i];
            assert_eq!((min, max), (0, 255));
        }
        for i in [AX_HAT_X, AX_HAT_Y] {
            let (_, min, max, ..) = AXES[i];
            assert_eq!((min, max), (-1, 1));
        }
    }
}
