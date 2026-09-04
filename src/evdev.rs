//! The second input backend: an ordinary Linux gamepad read over **evdev**.
//!
//! The controller is read from hidraw ([`crate::hidraw`]) because it is a Valve
//! device with a vendor report the kernel does not decode. An Xbox Elite
//! Series 2 is the opposite case: `xpad` (USB) and `hid-microsoft` (Bluetooth)
//! already decode it, so the cheapest correct tap is `/dev/input/event*`. This
//! module turns that stream into the daemon's own [`report::Frame`], tagged
//! [`report::Source::Evdev`], so every layer above it — the gesture engine, the
//! config, the mode engine, the bare-button router, the OSK — needs no new
//! vocabulary. `docs/design/xbox-elite.md` is the design; the research behind
//! it is `docs/research/xbox-elite.md`.
//!
//! Hand-rolled on `libc`, like [`crate::keyboard`] and [`crate::gamepad`]: the
//! read side of evdev is `read(2)` of 24-byte `input_event` structs plus five
//! ioctls, and the repo already owns that much uinput code. No new crate.
//!
//! # Discovery is by capability, never by `ID_INPUT_JOYSTICK`
//!
//! The BLE Elite 2's HID descriptor carries a full keyboard collection, so
//! systemd's *joystick un-detection* classifies it as a **keyboard**:
//! `udevadm info` on its node reports `ID_INPUT_KEYBOARD=1` and no
//! `ID_INPUT_JOYSTICK` at all (research §1.3.4, VERIFIED on this machine).
//! Anything that filters on the joystick tag — SDL's udev path included — does
//! not see this pad. So [`gamepad_nodes`] reads the sysfs capability bitmaps
//! and asks the only question that matters: does this node have two sticks and
//! a gamepad button cluster?
//!
//! # Two axis layouts, one device
//!
//! The same controller reports **different axis codes** depending on how it is
//! attached, and this is the single most surprising thing in this file:
//!
//! | | left stick | right stick | triggers |
//! |---|---|---|---|
//! | USB, `xpad` | `ABS_X`/`ABS_Y` | `ABS_RX`/`ABS_RY` | `ABS_Z` / `ABS_RZ` |
//! | Bluetooth, `hid-microsoft` | `ABS_X`/`ABS_Y` | `ABS_Z`/`ABS_RZ` | `ABS_BRAKE` / `ABS_GAS` |
//!
//! over BLE the descriptor spells the right stick as GD `Z`/`Rz` and the
//! triggers as Simulation `Brake`/`Accelerator`, which generic `hid-input`
//! maps to `ABS_Z`/`ABS_RZ` and `ABS_BRAKE`/`ABS_GAS` (research §1.3.2). A
//! backend that assumed `ABS_RX`/`ABS_RY` would not even *find* the Bluetooth
//! pad, let alone read its right stick. [`AxisMap::detect`] picks the layout
//! from the capability bitmap at adoption, once, and everything downstream
//! reads roles rather than codes.
//!
//! # Ranges come from the device
//!
//! Stick and trigger scales are read per axis with `EVIOCGABS` rather than
//! assumed, so a pad whose sticks are 0..65535 (or whose triggers are 0..255)
//! normalises correctly with no table of magic numbers.
//!
//! # `EVIOCGRAB`
//!
//! On by default (`h.device { grab = true }`). Two reasons, in order:
//!
//! * the BLE node is a *keyboard* to libinput, and the Elite's Profile button
//!   is `KEY_RECORD` — pressing it types into whatever Hyprland has focused.
//!   Its paddles, trigger locks and profile slot all share one `KEY_UNKNOWN`
//!   that toggles as they change. A grab stops both leaks at the source;
//! * for the wired node it hides the physical pad from every other evdev
//!   consumer, so a game sees only hyprpad's virtual `045e:028e`
//!   ([`crate::gamepad`]) — the docs/06 Tier-1 shape with no masking needed.
//!
//! It does **nothing** to Steam over Bluetooth, which reads the pad's hidraw
//! node instead; `docs/design/xbox-elite.md` says what to do about that.
//!
//! # Paddles
//!
//! The four back paddles are on the wire in every mode but reach evdev under
//! **two different code sets**, and hyprpad accepts both forever:
//!
//! * `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2` — `xpad` since 6.17 (this box runs 7.1),
//!   and xpadneo;
//! * `BTN_TRIGGER_HAPPY5..8` — the in-tree `udev-hid-bpf` program for the BLE
//!   descriptor, and `xpad` before 6.17.
//!
//! Either way the pad must be in **profile slot 0** (no LED): every consumer —
//! `xpad`, xpadneo, SDL — mutes the paddles in slots 1–3, because there the
//! firmware re-emits them as face buttons. Phase 2's hidraw sidecar reads the
//! slot byte directly and can say so; phase 1 cannot see it. See
//! [`PADDLE_HOOK`].

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::report::{self, Button};

// ---------------------------------------------------------------------------
// evdev constants (uapi/linux/input-event-codes.h)
// ---------------------------------------------------------------------------

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;

// The gamepad button cluster. NOTE the legacy names: `BTN_X` is `BTN_NORTH`
// (0x133) and `BTN_Y` is `BTN_WEST` (0x134). The Linux Gamepad Specification
// would put X at WEST and Y at NORTH for an Xbox pad, but neither path this
// backend reads is spec-compliant: `xpad`'s table is literally
// `{BTN_A, BTN_B, BTN_X, BTN_Y}` = 0x130/0x131/0x133/0x134, and over Bluetooth
// `hid-input` maps HID Button 4 -> 0x133 and Button 5 -> 0x134, which the
// descriptor labels X and Y in that order (research §1.3.2). The two agree, so
// the legacy reading is the correct one here.
pub const BTN_SOUTH: u16 = 0x130; // A
pub const BTN_EAST: u16 = 0x131; // B
pub const BTN_C: u16 = 0x132;
pub const BTN_X: u16 = 0x133;
pub const BTN_Y: u16 = 0x134;
pub const BTN_Z: u16 = 0x135;
pub const BTN_TL: u16 = 0x136; // LB
pub const BTN_TR: u16 = 0x137; // RB
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a; // View
pub const BTN_START: u16 = 0x13b; // Menu
pub const BTN_MODE: u16 = 0x13c; // the Xbox button -> the guide
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;

// Some drivers report the d-pad as buttons rather than a hat.
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_DOWN: u16 = 0x221;
pub const BTN_DPAD_LEFT: u16 = 0x222;
pub const BTN_DPAD_RIGHT: u16 = 0x223;

// Paddles, code set A: `xpad` >= 6.17 and xpadneo.
pub const BTN_GRIPL: u16 = 0x224; // P3, upper left
pub const BTN_GRIPR: u16 = 0x225; // P1, upper right
pub const BTN_GRIPL2: u16 = 0x226; // P4, lower left
pub const BTN_GRIPR2: u16 = 0x227; // P2, lower right

// Paddles, code set B: `udev-hid-bpf`'s Elite-2 program, and `xpad` < 6.17.
pub const BTN_TRIGGER_HAPPY5: u16 = 0x2c4; // P1
pub const BTN_TRIGGER_HAPPY6: u16 = 0x2c5; // P2
pub const BTN_TRIGGER_HAPPY7: u16 = 0x2c6; // P3
pub const BTN_TRIGGER_HAPPY8: u16 = 0x2c7; // P4

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_GAS: u16 = 0x09;
pub const ABS_BRAKE: u16 = 0x0a;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

/// Highest `EV_KEY` code, for the `EVIOCGKEY` bitmap.
const KEY_MAX: u16 = 0x2ff;

/// Microsoft's vendor id — the Elite Series 2 under every transport.
pub const VENDOR_MICROSOFT: u16 = 0x045e;

/// Product ids the Elite Series 2 presents. `0x0b00` is the wired pad under
/// `xpad`; `0x0b05` is the pre-BLE Bluetooth firmware and `0x0b22` the BLE one,
/// both under `hid-microsoft` (research §1.2, §1.3.1). Matching these turns on
/// nothing this backend does not do generically — it only names the device and
/// picks the cheat-sheet layout.
pub const ELITE_2_PRODUCTS: [u16; 3] = [0x0b00, 0x0b05, 0x0b22];

/// The product id of hyprpad's own virtual pad ([`crate::gamepad`]), which is
/// also a real wired Xbox 360 pad's id — which is exactly why this is **not**
/// how the loop-back is avoided. The sysfs-path rule below is.
const PID_XBOX360: u16 = 0x028e;

/// How often the watcher re-scans for a pad when it has none. Deliberately the
/// same cadence as the controller's reconnect scan ([`crate::run`]'s
/// `RECONNECT_SCAN_INTERVAL`), so both controllers come back on the same
/// rhythm.
pub const SCAN_INTERVAL: Duration = Duration::from_millis(1500);

/// Where the phase-2 Bluetooth paddle decoder attaches.
///
/// Over Bluetooth with a stock kernel and no extra package, all four paddles
/// arrive folded into a single `KEY_UNKNOWN` (research §1.3.2): the input core
/// clamps `EV_KEY` to pressed/not-pressed and three separate fields share that
/// one code, so the nibble 0 -> 1 -> 3 is *one* press and no further events.
/// It is not recoverable from evdev at all.
///
/// The fix that needs nothing installed is to read the pad's own hidraw node
/// beside this one — `045e:0b22`, 464-byte descriptor, report 1, **byte 19
/// bits 0..3** = P1/P2/P3/P4, with **byte 17** carrying the profile slot so the
/// daemon can mute them itself and warn. That is a `Frame::decode_xbox_ble`
/// next to [`report::Frame::decode`] plus a [`crate::hidraw::read_all`] over
/// `/sys/class/hidraw`, and it is **phase 2**. When it lands, the evdev fd
/// stays open for [`EVIOCGRAB`](Device::grab) alone.
///
/// Until then the owner's two routes to four distinct paddles are a USB-C
/// cable (`xpad`, `BTN_GRIP*`) or `pacman -S udev-hid-bpf` over Bluetooth
/// (`BTN_TRIGGER_HAPPY5..8`). Both are already handled above.
pub const PADDLE_HOOK: () = ();

// ---------------------------------------------------------------------------
// ioctl plumbing
// ---------------------------------------------------------------------------

const IOC_READ: u32 = 2;
const IOC_WRITE: u32 = 1;

/// Encode an ioctl request the way `<asm-generic/ioctl.h>` does:
/// `dir << 30 | size << 16 | type << 8 | nr`.
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (ty << 8) | nr) as libc::c_ulong
}

/// `EVIOCGRAB` — `_IOW('E', 0x90, int)`.
const fn eviocgrab() -> libc::c_ulong {
    ioc(IOC_WRITE, b'E' as u32, 0x90, 4)
}

/// `EVIOCGABS(axis)` — `_IOR('E', 0x40 + axis, struct input_absinfo)`.
const fn eviocgabs(axis: u16) -> libc::c_ulong {
    ioc(IOC_READ, b'E' as u32, 0x40 + axis as u32, 24)
}

/// `EVIOCGKEY(len)` — `_IOR('E', 0x18, len)`: the whole key state at once.
const fn eviocgkey(len: u32) -> libc::c_ulong {
    ioc(IOC_READ, b'E' as u32, 0x18, len)
}

/// One evdev event as the kernel writes it. `time` is a `timeval`, which on a
/// 64-bit target is two `i64`s — 24 bytes total, and the read loop below slices
/// on exactly that.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEvent {
    pub tv_sec: libc::time_t,
    pub tv_usec: libc::suseconds_t,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// `struct input_absinfo`: value, min, max, fuzz, flat, resolution.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

// ---------------------------------------------------------------------------
// Capability bitmaps
// ---------------------------------------------------------------------------

/// One sysfs `capabilities/*` bitmap.
///
/// The kernel prints these as space-separated 64-bit hex words, **most
/// significant first** — `"e080ffdf01cfffff fffffffffffffffe"` is bits 0..127
/// with the *low* word last. Getting that order backwards is the classic way
/// to mis-identify a device, so the ordering is reversed once, here, and
/// [`Caps::has`] is then a plain index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// Words in *ascending* significance: `words[0]` holds bits 0..63.
    words: Vec<u64>,
}

impl Caps {
    /// Parse a sysfs capability string. Unparseable words are dropped, which
    /// degrades to "this device claims less than it does" — the safe direction
    /// for a discovery filter.
    pub fn parse(s: &str) -> Caps {
        let mut words: Vec<u64> = s
            .split_whitespace()
            .filter_map(|w| u64::from_str_radix(w, 16).ok())
            .collect();
        words.reverse();
        Caps { words }
    }

    /// Whether bit `n` is set.
    pub fn has(&self, n: u16) -> bool {
        let n = n as usize;
        self.words.get(n / 64).is_some_and(|w| w & (1u64 << (n % 64)) != 0)
    }

    /// Whether every bit in `bits` is set.
    pub fn has_all(&self, bits: &[u16]) -> bool {
        bits.iter().all(|&b| self.has(b))
    }

    /// Whether any bit in `bits` is set.
    pub fn has_any(&self, bits: &[u16]) -> bool {
        bits.iter().any(|&b| self.has(b))
    }
}

/// The gamepad button cluster a node must advertise to qualify.
///
/// `BTN_C` (0x132) and `BTN_Z` (0x135) are deliberately **not** required: the
/// BLE Elite sets them (HID Buttons 3 and 6, never actually pressed) but
/// `xpad` does not, and requiring them would reject the wired pad — the one
/// transport that gives four distinct paddles out of the box.
const REQUIRED_BUTTONS: [u16; 11] = [
    BTN_SOUTH, BTN_EAST, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE,
    BTN_THUMBL, BTN_THUMBR,
];

// ---------------------------------------------------------------------------
// Axis roles
// ---------------------------------------------------------------------------

/// What one `EV_ABS` code means on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    TriggerL,
    TriggerR,
    HatX,
    HatY,
}

/// An axis's reported range, from `EVIOCGABS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsRange {
    pub min: i32,
    pub max: i32,
}

impl AbsRange {
    /// A signed axis centred on zero, the shape a stick usually has.
    pub fn signed(half: i32) -> AbsRange {
        AbsRange { min: -half, max: half }
    }

    /// An unsigned axis starting at zero, the shape a trigger usually has.
    pub fn unsigned(max: i32) -> AbsRange {
        AbsRange { min: 0, max }
    }

    /// Normalise `v` to `[-1, 1]` across the range's midpoint. Used for sticks.
    pub fn bipolar(&self, v: i32) -> f64 {
        let mid = (f64::from(self.min) + f64::from(self.max)) / 2.0;
        let half = (f64::from(self.max) - f64::from(self.min)) / 2.0;
        if half <= 0.0 {
            return 0.0;
        }
        ((f64::from(v) - mid) / half).clamp(-1.0, 1.0)
    }

    /// Normalise `v` to `[0, 1]` across the whole range. Used for triggers.
    pub fn unipolar(&self, v: i32) -> f64 {
        let span = f64::from(self.max) - f64::from(self.min);
        if span <= 0.0 {
            return 0.0;
        }
        ((f64::from(v) - f64::from(self.min)) / span).clamp(0.0, 1.0)
    }
}

/// Which `EV_ABS` code plays which role on one device, plus each one's range.
///
/// Built once at adoption by [`AxisMap::detect`] from the capability bitmap
/// (which layout) and `EVIOCGABS` (what scale). See the module docs for why
/// the same controller needs two layouts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxisMap {
    by_code: HashMap<u16, (Axis, AbsRange)>,
}

impl AxisMap {
    /// Assemble a map from explicit `(code, role, range)` triples. The tests
    /// use it; [`detect`](Self::detect) is this with the device asked.
    pub fn from_parts(parts: &[(u16, Axis, AbsRange)]) -> AxisMap {
        AxisMap {
            by_code: parts.iter().map(|&(c, a, r)| (c, (a, r))).collect(),
        }
    }

    /// What `code` means here, if anything.
    pub fn role(&self, code: u16) -> Option<(Axis, AbsRange)> {
        self.by_code.get(&code).copied()
    }

    /// Every code in the map, for a state resync.
    pub fn codes(&self) -> impl Iterator<Item = u16> + '_ {
        self.by_code.keys().copied()
    }

    /// Decide the axis layout from an `EV_ABS` capability bitmap, asking
    /// `range` for each code's scale.
    ///
    /// The right stick is `RX`/`RY` where the device has them (USB `xpad`) and
    /// `Z`/`RZ` otherwise (Bluetooth `hid-input`); the triggers are whichever
    /// pair is left — `Z`/`RZ` in the first case, `BRAKE`/`GAS` in the second.
    /// Pure over `range`, so the whole table is unit-testable with no device.
    pub fn detect<F: FnMut(u16) -> AbsRange>(abs: &Caps, mut range: F) -> AxisMap {
        let mut parts: Vec<(u16, Axis, AbsRange)> = Vec::new();
        let mut add = |code: u16, axis: Axis, range: &mut F| {
            parts.push((code, axis, range(code)));
        };
        if abs.has(ABS_X) {
            add(ABS_X, Axis::LeftX, &mut range);
        }
        if abs.has(ABS_Y) {
            add(ABS_Y, Axis::LeftY, &mut range);
        }
        if abs.has(ABS_RX) && abs.has(ABS_RY) {
            // USB / xpad: sticks on RX/RY, triggers on Z/RZ.
            add(ABS_RX, Axis::RightX, &mut range);
            add(ABS_RY, Axis::RightY, &mut range);
            if abs.has(ABS_Z) {
                add(ABS_Z, Axis::TriggerL, &mut range);
            }
            if abs.has(ABS_RZ) {
                add(ABS_RZ, Axis::TriggerR, &mut range);
            }
        } else if abs.has(ABS_Z) && abs.has(ABS_RZ) {
            // Bluetooth / hid-input: the right stick IS Z/Rz, and the triggers
            // came in on the Simulation page as Brake (left) and Accelerator
            // (right).
            add(ABS_Z, Axis::RightX, &mut range);
            add(ABS_RZ, Axis::RightY, &mut range);
            if abs.has(ABS_BRAKE) {
                add(ABS_BRAKE, Axis::TriggerL, &mut range);
            }
            if abs.has(ABS_GAS) {
                add(ABS_GAS, Axis::TriggerR, &mut range);
            }
        }
        if abs.has(ABS_HAT0X) {
            add(ABS_HAT0X, Axis::HatX, &mut range);
        }
        if abs.has(ABS_HAT0Y) {
            add(ABS_HAT0Y, Axis::HatY, &mut range);
        }
        AxisMap::from_parts(&parts)
    }
}

// ---------------------------------------------------------------------------
// Frame building
// ---------------------------------------------------------------------------

/// Analog full scale on the wire: the controller reports its triggers `0..=32767`
/// and its sticks `±32767`, and the whole daemon is written against that, so
/// this backend scales into it rather than adding a second convention.
const FULL_SCALE: f64 = 32767.0;

/// Fraction of a trigger's travel at which the synthesised full-pull button
/// goes **down**. The controller's `TriggerL2Full`/`TriggerR2Full` are a firmware
/// click at the end of the throw; an Xbox trigger has no click, so it is a
/// threshold — with hysteresis, because the gesture engine's chords are
/// edge-triggered and a trigger resting at the threshold must not re-chord.
pub const TRIGGER_PRESS: f64 = 0.85;
/// Fraction at which it comes back **up**. The 0.15 gap is the hysteresis.
pub const TRIGGER_RELEASE: f64 = 0.70;

/// Turns a change-driven evdev stream into the daemon's frames.
///
/// A gamepad reports **only what changed** — hold a stick at 60 % and the
/// device goes silent — so the frame is *persistent*: events mutate it and a
/// `SYN_REPORT` publishes a copy. That is the whole reason the daemon grows a
/// conditional deadline for its rate integrators ([`crate::sticks`]); without
/// one, a held stick would move the cursor exactly once.
pub struct FrameBuilder {
    frame: report::Frame,
    axes: AxisMap,
    /// Live state of the trigger hysteresis, so the threshold is a Schmitt
    /// trigger rather than a comparator.
    l2_full: bool,
    r2_full: bool,
}

impl FrameBuilder {
    /// A builder over `axes`, starting from a neutral frame.
    pub fn new(axes: AxisMap) -> FrameBuilder {
        FrameBuilder {
            frame: report::Frame { source: report::Source::Evdev, ..Default::default() },
            axes,
            l2_full: false,
            r2_full: false,
        }
    }

    /// The frame as it stands.
    pub fn frame(&self) -> report::Frame {
        self.frame
    }

    /// Apply one event. Returns `Some(frame)` on a `SYN_REPORT` — one published
    /// frame per kernel report, exactly as the controller's decoder produces one per
    /// hidraw read — and `None` for everything else.
    pub fn apply(&mut self, ev: &InputEvent) -> Option<report::Frame> {
        match ev.kind {
            EV_SYN if ev.code == SYN_REPORT => return Some(self.frame),
            EV_KEY => self.apply_key(ev.code, ev.value != 0),
            EV_ABS => self.apply_abs(ev.code, ev.value),
            _ => {}
        }
        None
    }

    /// Map one key code onto a [`Button`], if it is one hyprpad knows.
    ///
    /// Both paddle code sets are accepted unconditionally and for good: a
    /// future `udev-hid-bpf` may switch to `BTN_GRIP*` too, and a kernel older
    /// than 6.17 still speaks `BTN_TRIGGER_HAPPY*` over USB. Nothing is lost by
    /// listening for both — no device emits both — and the alternative is a
    /// silent regression on somebody's machine.
    pub fn button_for(code: u16) -> Option<Button> {
        Some(match code {
            BTN_SOUTH => Button::A,
            BTN_EAST => Button::B,
            BTN_X => Button::X,
            BTN_Y => Button::Y,
            BTN_TL => Button::BumperL1,
            BTN_TR => Button::BumperR1,
            BTN_SELECT => Button::View,
            BTN_START => Button::Menu,
            // The Xbox button is the guide: everything in `gesture.rs` keys off
            // `Button::Steam`, so the chord grammar works unchanged.
            BTN_MODE => Button::Steam,
            BTN_THUMBL => Button::L3,
            BTN_THUMBR => Button::R3,
            BTN_DPAD_UP => Button::DpadUp,
            BTN_DPAD_DOWN => Button::DpadDown,
            BTN_DPAD_LEFT => Button::DpadLeft,
            BTN_DPAD_RIGHT => Button::DpadRight,
            // Paddles, code set A (xpad >= 6.17, xpadneo). P1 upper-right and
            // P3 upper-left are the *upper* pair, which is why they land on L4
            // and R4: the sheet already draws l4 above l5.
            BTN_GRIPR => Button::GripR4,   // P1
            BTN_GRIPR2 => Button::GripR5,  // P2
            BTN_GRIPL => Button::GripL4,   // P3
            BTN_GRIPL2 => Button::GripL5,  // P4
            // Paddles, code set B (udev-hid-bpf over BLE, xpad < 6.17).
            BTN_TRIGGER_HAPPY5 => Button::GripR4, // P1
            BTN_TRIGGER_HAPPY6 => Button::GripR5, // P2
            BTN_TRIGGER_HAPPY7 => Button::GripL4, // P3
            BTN_TRIGGER_HAPPY8 => Button::GripL5, // P4
            // Analog triggers reach us as axes; the digital `BTN_TL2`/`BTN_TR2`
            // some pads also send would double-fire against the synthesised
            // full-pull bit, so they are deliberately ignored.
            BTN_TL2 | BTN_TR2 => return None,
            _ => return None,
        })
    }

    fn apply_key(&mut self, code: u16, down: bool) {
        if let Some(b) = Self::button_for(code) {
            self.frame.set(b, down);
        }
    }

    fn apply_abs(&mut self, code: u16, value: i32) {
        let Some((axis, range)) = self.axes.role(code) else { return };
        match axis {
            // Stick Y is inverted on the way in: evdev's +Y is *down* (both
            // `xpad` and the HID convention), the controller's is *up*, and the whole
            // daemon — flicks, the OSK wire, the virtual pad — is written to
            // the controller's.
            Axis::LeftX => self.frame.left_stick.0 = scale_stick(range.bipolar(value)),
            Axis::LeftY => self.frame.left_stick.1 = scale_stick(-range.bipolar(value)),
            Axis::RightX => self.frame.right_stick.0 = scale_stick(range.bipolar(value)),
            Axis::RightY => self.frame.right_stick.1 = scale_stick(-range.bipolar(value)),
            Axis::TriggerL => {
                let n = range.unipolar(value);
                self.frame.l2 = scale_trigger(n);
                self.l2_full = trigger_full(self.l2_full, n);
                self.frame.set(Button::TriggerL2Full, self.l2_full);
            }
            Axis::TriggerR => {
                let n = range.unipolar(value);
                self.frame.r2 = scale_trigger(n);
                self.r2_full = trigger_full(self.r2_full, n);
                self.frame.set(Button::TriggerR2Full, self.r2_full);
            }
            // A hat is a tri-state axis; the daemon's d-pad is four bits. A
            // diagonal on a real hat sets two of them, which is what a
            // `dpad_up`+`dpad_left` chord expects.
            Axis::HatX => {
                self.frame.set(Button::DpadLeft, value < 0);
                self.frame.set(Button::DpadRight, value > 0);
            }
            Axis::HatY => {
                self.frame.set(Button::DpadUp, value < 0);
                self.frame.set(Button::DpadDown, value > 0);
            }
        }
    }
}

/// `[-1, 1]` to the controller's `±32767`.
fn scale_stick(n: f64) -> i16 {
    (n.clamp(-1.0, 1.0) * FULL_SCALE).round() as i16
}

/// `[0, 1]` to the controller's `0..=32767`.
fn scale_trigger(n: f64) -> u16 {
    (n.clamp(0.0, 1.0) * FULL_SCALE).round() as u16
}

/// The Schmitt trigger behind the synthesised full-pull bit: `was` is the
/// current state, `n` the normalised pull.
pub fn trigger_full(was: bool, n: f64) -> bool {
    if was {
        n > TRIGGER_RELEASE
    } else {
        n >= TRIGGER_PRESS
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// A qualifying gamepad node found in sysfs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// `/dev/input/eventN`.
    pub path: PathBuf,
    /// The device's `name` attribute, e.g. `"Xbox Wireless Controller"`.
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    /// Whether this is an Elite Series 2 by vendor and product.
    pub elite: bool,
    /// `EV_ABS` capabilities, kept so the axis layout can be decided without a
    /// second sysfs read.
    pub abs: Caps,
    /// `EV_KEY` capabilities, kept so the adoption line can say which paddle
    /// code set (if either) this node advertises.
    pub key: Caps,
}

impl Node {
    /// The short wire name for `status.json`'s `"sources"` list.
    pub fn label(&self) -> &'static str {
        if self.elite {
            "elite"
        } else {
            "gamepad"
        }
    }

    /// The cheat-sheet layout id this controller draws as.
    ///
    /// Every pad this backend adopts is an Xbox-shaped one as far as the sheet
    /// is concerned — two asymmetric sticks, a face diamond, four paddles — so
    /// a generic gamepad borrows the Elite's drawing rather than getting none.
    pub fn layout(&self) -> &'static str {
        report::LAYOUT_XBOX_ELITE_2
    }

    /// Which paddle code set this node advertises, for the adoption log line.
    pub fn paddles(&self) -> Paddles {
        if self.key.has_any(&[BTN_GRIPL, BTN_GRIPR, BTN_GRIPL2, BTN_GRIPR2]) {
            Paddles::Grip
        } else if self
            .key
            .has_any(&[BTN_TRIGGER_HAPPY5, BTN_TRIGGER_HAPPY6, BTN_TRIGGER_HAPPY7, BTN_TRIGGER_HAPPY8])
        {
            Paddles::TriggerHappy
        } else {
            Paddles::None
        }
    }
}

/// Which of the two paddle code sets a node speaks — or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paddles {
    /// `BTN_GRIPR/GRIPR2/GRIPL/GRIPL2` — `xpad` >= 6.17, xpadneo.
    Grip,
    /// `BTN_TRIGGER_HAPPY5..8` — `udev-hid-bpf`'s BLE program, `xpad` < 6.17.
    TriggerHappy,
    /// No paddles on this node. Over Bluetooth with a stock kernel that is the
    /// expected answer, and the fix is a cable or `udev-hid-bpf`
    /// ([`PADDLE_HOOK`]).
    None,
}

impl Paddles {
    pub fn as_str(self) -> &'static str {
        match self {
            Paddles::Grip => "BTN_GRIP* (4 paddles)",
            Paddles::TriggerHappy => "BTN_TRIGGER_HAPPY5..8 (4 paddles)",
            Paddles::None => "none on this node",
        }
    }
}

/// Whether a sysfs device path belongs to a **virtual** (uinput) device.
///
/// This is the loop-back guard, and it is a path rule rather than a vendor rule
/// on purpose. hyprpad's own output pad wears `045e:028e` — the wired Xbox 360
/// id, deliberately, because every engine knows it — and so does a real 360
/// pad somebody might actually want to use. Steam's own virtual pads have the
/// same problem. What they all share, and what no physical device has, is a
/// canonical sysfs path under `/sys/devices/virtual/input/`.
pub fn is_virtual(sysfs: &Path) -> bool {
    sysfs.to_string_lossy().contains("/devices/virtual/input/")
}

/// Whether a node's capabilities say "this is a gamepad".
///
/// Two sticks and a gamepad button cluster. The right stick is accepted under
/// **either** spelling (`RX`/`RY` or `Z`/`RZ`) because the same Elite reports
/// it differently over USB and Bluetooth — see the module docs. Nothing here
/// looks at `ID_INPUT_JOYSTICK`, which udev does not set on the BLE pad.
pub fn qualifies(ev: &Caps, key: &Caps, abs: &Caps) -> bool {
    if !ev.has(EV_ABS) || !ev.has(EV_KEY) {
        return false;
    }
    if !abs.has(ABS_X) || !abs.has(ABS_Y) {
        return false;
    }
    let right_stick = (abs.has(ABS_RX) && abs.has(ABS_RY)) || (abs.has(ABS_Z) && abs.has(ABS_RZ));
    right_stick && key.has_all(&REQUIRED_BUTTONS)
}

/// Read one sysfs attribute, trimmed.
fn attr(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string())
}

/// Every `/dev/input/event*` that looks like a gamepad hyprpad may adopt,
/// numerically ordered.
///
/// Reads `/sys` only — "is a pad plugged in" and "may I open it" are different
/// questions and this one answers the first, exactly as
/// [`crate::hidraw::controller_nodes`] does for the controller.
pub fn gamepad_nodes() -> io::Result<Vec<Node>> {
    let mut found: Vec<(u32, Node)> = Vec::new();
    for entry in fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(n) = name.strip_prefix("event").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        // The canonical path is what tells a uinput device from a real one; a
        // `/sys/class/input/eventN` symlink looks the same either way.
        let Ok(real) = fs::canonicalize(entry.path()) else { continue };
        if is_virtual(&real) {
            continue;
        }
        let dev = entry.path().join("device");
        let ev = Caps::parse(&attr(&dev, "capabilities/ev").unwrap_or_default());
        let key = Caps::parse(&attr(&dev, "capabilities/key").unwrap_or_default());
        let abs = Caps::parse(&attr(&dev, "capabilities/abs").unwrap_or_default());
        if !qualifies(&ev, &key, &abs) {
            continue;
        }
        let hex = |f: &str| {
            attr(&dev, &format!("id/{f}")).and_then(|v| u16::from_str_radix(&v, 16).ok())
        };
        let vendor = hex("vendor").unwrap_or(0);
        let product = hex("product").unwrap_or(0);
        // Belt and braces beside the path rule: our own pad's identity, on a
        // node that somehow escaped it, is still not something to read back.
        if vendor == VENDOR_MICROSOFT
            && product == PID_XBOX360
            && attr(&dev, "name").as_deref() == Some(crate::gamepad::DEVICE_NAME_STR)
        {
            continue;
        }
        found.push((
            n,
            Node {
                path: PathBuf::from("/dev/input").join(&name),
                name: attr(&dev, "name").unwrap_or_default(),
                vendor,
                product,
                elite: vendor == VENDOR_MICROSOFT && ELITE_2_PRODUCTS.contains(&product),
                abs,
                key,
            },
        ));
    }
    found.sort_by_key(|(n, _)| *n);
    Ok(found.into_iter().map(|(_, node)| node).collect())
}

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

/// An opened, optionally grabbed evdev node.
pub struct Device {
    fd: OwnedFd,
    pub node: Node,
    pub axes: AxisMap,
    pub grabbed: bool,
}

impl Device {
    /// Open `node` read-only, learn its axis ranges, and grab it if asked.
    ///
    /// `O_RDONLY`: this backend never writes. Phase 2's rumble taps
    /// (`EVIOCSFF` + an `EV_FF` play) need `O_RDWR`, and that is the one line
    /// to change when they land. `O_CLOEXEC` because the daemon spawns the OSK
    /// as a child and must not leak a controller into it.
    pub fn open(node: Node, grab: bool) -> io::Result<Device> {
        use std::os::unix::ffi::OsStrExt;
        let mut c_path = Vec::with_capacity(node.path.as_os_str().len() + 1);
        c_path.extend_from_slice(node.path.as_os_str().as_bytes());
        c_path.push(0);
        // SAFETY: `c_path` is NUL-terminated and outlives the call.
        let fd = unsafe { libc::open(c_path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `open` returned a fresh descriptor nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let raw = fd.as_raw_fd();
        let axes = AxisMap::detect(&node.abs, |code| {
            abs_range(raw, code).unwrap_or(AbsRange::signed(32767))
        });
        let mut dev = Device { fd, node, axes, grabbed: false };
        if grab {
            // A refused grab is not fatal: another process holding the device
            // (a second daemon, a test harness) should cost us the leak
            // protection, not the controller.
            match dev.grab(true) {
                Ok(()) => dev.grabbed = true,
                Err(e) => eprintln!(
                    "warning: could not grab {} ({e}); the pad's keys will also reach \
                     the compositor",
                    dev.node.path.display()
                ),
            }
        }
        Ok(dev)
    }

    /// `EVIOCGRAB` on or off.
    pub fn grab(&mut self, on: bool) -> io::Result<()> {
        let arg: libc::c_int = i32::from(on);
        // SAFETY: EVIOCGRAB takes an int by value; the fd is ours and open.
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), eviocgrab(), arg) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Read the device's current state into `b`, so a freshly adopted pad whose
    /// sticks are already deflected — or one that just dropped events — starts
    /// from the truth rather than from "centred, nothing pressed".
    ///
    /// This is also the `SYN_DROPPED` recovery. The kernel drops events when
    /// its per-client buffer overflows and tells us so; the only correct answer
    /// is to re-read the whole state, because an unknown number of edges are
    /// simply gone.
    pub fn resync(&self, b: &mut FrameBuilder) {
        let raw = self.fd.as_raw_fd();
        for code in self.axes.codes().collect::<Vec<_>>() {
            if let Some(v) = abs_value(raw, code) {
                b.apply_abs(code, v);
            }
        }
        if let Some(keys) = key_state(raw) {
            for code in 0..=KEY_MAX {
                if FrameBuilder::button_for(code).is_some() {
                    b.apply_key(code, keys.has(code));
                }
            }
        }
    }

    /// Read the next batch of events into `buf`, returning the events read.
    fn read_events(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is a valid writable slice of the length passed.
        let n = unsafe {
            libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
}

/// `EVIOCGABS` for one axis, as a range.
fn abs_range(fd: libc::c_int, code: u16) -> Option<AbsRange> {
    let info = abs_info(fd, code)?;
    Some(AbsRange { min: info.minimum, max: info.maximum })
}

/// `EVIOCGABS` for one axis, as its current value.
fn abs_value(fd: libc::c_int, code: u16) -> Option<i32> {
    Some(abs_info(fd, code)?.value)
}

fn abs_info(fd: libc::c_int, code: u16) -> Option<RawAbsInfo> {
    let mut info = RawAbsInfo::default();
    // SAFETY: the request encodes `sizeof(struct input_absinfo)` and `info` is
    // exactly that, initialised and exclusively borrowed.
    let rc = unsafe { libc::ioctl(fd, eviocgabs(code), std::ptr::addr_of_mut!(info)) };
    (rc >= 0).then_some(info)
}

/// `EVIOCGKEY`: the whole `EV_KEY` state as a bitmap.
fn key_state(fd: libc::c_int) -> Option<Caps> {
    let len = (KEY_MAX as usize / 8) + 1;
    let mut bits = vec![0u8; len];
    // SAFETY: the request carries `len`, and `bits` is a `len`-byte buffer.
    let rc = unsafe { libc::ioctl(fd, eviocgkey(len as u32), bits.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    // The ioctl fills a byte array; `Caps` indexes 64-bit words, so pack it.
    let mut words = vec![0u64; len.div_ceil(8)];
    for (i, byte) in bits.iter().enumerate() {
        words[i / 8] |= u64::from(*byte) << ((i % 8) * 8);
    }
    Some(Caps { words })
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// What the watcher thread reports to the daemon loop.
#[derive(Debug)]
pub enum Event {
    /// A pad was adopted. Carries what `status.json` needs to name it.
    Adopted { label: &'static str, layout: &'static str, name: String },
    /// One published frame — one `SYN_REPORT` from the device.
    Frame(report::Frame),
    /// The adopted pad went away (unplugged, slept, or the read failed). The
    /// daemon releases whatever it was holding for that source and the watcher
    /// goes back to scanning.
    Released,
}

/// Start the evdev backend: one thread that finds a pad, reads it until it goes
/// away, and looks for the next one.
///
/// Hotplug is the same periodic scan the controller's reconnect wait uses
/// ([`SCAN_INTERVAL`]) plus an immediate re-scan on any read error — no udev
/// monitor, no inotify, no new dependency. The whole state machine lives here
/// rather than in the loop, which is why `run.rs` gains three match arms and
/// not a supervisor.
pub fn watch(grab: bool) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || watch_loop(grab, &tx));
    rx
}

fn watch_loop(grab: bool, tx: &mpsc::Sender<Event>) {
    // Warn at most once per (path, error kind) so a node we can never open —
    // wrong group, no uaccess rule — costs one line and not one per scan.
    let mut warned: Option<PathBuf> = None;
    loop {
        let node = match gamepad_nodes() {
            Ok(nodes) => nodes.into_iter().next(),
            Err(e) => {
                eprintln!("warning: could not scan /sys/class/input ({e})");
                None
            }
        };
        let Some(node) = node else {
            std::thread::sleep(SCAN_INTERVAL);
            continue;
        };
        let path = node.path.clone();
        let dev = match Device::open(node, grab) {
            Ok(d) => d,
            Err(e) => {
                if warned.as_deref() != Some(path.as_path()) {
                    eprintln!(
                        "warning: {} looks like a gamepad but will not open ({e}); \
                         you may need to be in the 'input' group",
                        path.display()
                    );
                    warned = Some(path);
                }
                std::thread::sleep(SCAN_INTERVAL);
                continue;
            }
        };
        warned = None;
        eprintln!(
            "hyprpad: gamepad adopted — {} ({:04x}:{:04x}) on {}, paddles: {}{}",
            dev.node.name,
            dev.node.vendor,
            dev.node.product,
            dev.node.path.display(),
            dev.node.paddles().as_str(),
            if dev.grabbed { ", grabbed" } else { "" },
        );
        if dev.node.elite && dev.node.paddles() == Paddles::None {
            eprintln!(
                "hyprpad: this Elite exposes no paddle buttons on evdev — plug it in over \
                 USB-C, or install udev-hid-bpf for Bluetooth, and leave it in profile \
                 slot 0 (no LED)"
            );
        }
        let announced = Event::Adopted {
            label: dev.node.label(),
            layout: dev.node.layout(),
            name: dev.node.name.clone(),
        };
        if tx.send(announced).is_err() {
            return; // the loop is gone
        }
        let ended = read_loop(&dev, tx);
        eprintln!("hyprpad: gamepad released ({})", dev.node.path.display());
        if tx.send(Event::Released).is_err() || ended.is_none() {
            return;
        }
        // A device that opens and instantly EOFs must not become a hot loop.
        std::thread::sleep(SCAN_INTERVAL);
    }
}

/// Read one adopted device until it goes away. `None` means the daemon loop
/// hung up (stop the thread); `Some(())` means the device did (rescan).
fn read_loop(dev: &Device, tx: &mpsc::Sender<Event>) -> Option<()> {
    const EVENT_SIZE: usize = std::mem::size_of::<InputEvent>();
    let mut b = FrameBuilder::new(dev.axes.clone());
    // Seed from the device: a pad adopted mid-deflection must not claim to be
    // centred until the user happens to move it.
    dev.resync(&mut b);
    let mut buf = [0u8; EVENT_SIZE * 64];
    loop {
        let n = match dev.read_events(&mut buf) {
            Ok(0) => return Some(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Some(()), // ENODEV on unplug: rescan
        };
        for chunk in buf[..n].chunks_exact(EVENT_SIZE) {
            // SAFETY: `InputEvent` is `repr(C)` and plain-old-data, and the
            // chunk is exactly its size; `read_unaligned` makes no alignment
            // claim about the buffer.
            let ev: InputEvent = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
            if ev.kind == EV_SYN && ev.code == SYN_DROPPED {
                // The kernel's buffer overflowed and an unknown number of edges
                // are gone. Re-read everything rather than carrying a frame we
                // know to be wrong.
                dev.resync(&mut b);
                continue;
            }
            if let Some(frame) = b.apply(&ev) {
                tx.send(Event::Frame(frame)).ok()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- capability bitmaps ------------------------------------------------

    /// The word order is the thing to get right: sysfs prints most-significant
    /// first, so the LAST word holds bits 0..63. This string is the BLE Elite's
    /// own `capabilities/key`, read off the machine (research §1.1).
    #[test]
    fn a_capability_bitmap_is_parsed_most_significant_word_first() {
        let key = Caps::parse(
            "7fff000000000000 1000000000000 8000000000 e080ffdf01cfffff fffffffffffffffe",
        );
        // Word 4 (the last) is bits 0..63: KEY_ESC (1) up, KEY_RESERVED (0) down.
        assert!(key.has(1), "KEY_ESC");
        assert!(!key.has(0), "KEY_RESERVED");
        // The gamepad cluster lives at 0x130..0x13e in the FIRST printed word.
        for b in [BTN_SOUTH, BTN_EAST, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_MODE, BTN_THUMBR] {
            assert!(key.has(b), "{b:#x} should be set");
        }
        // KEY_RECORD (167) — the Profile button — and KEY_UNKNOWN (240).
        assert!(key.has(167), "KEY_RECORD");
        assert!(key.has(240), "KEY_UNKNOWN");
        // Nothing past the printed words.
        assert!(!key.has(BTN_GRIPR), "no paddles on a stock BLE node");
    }

    #[test]
    fn a_short_bitmap_answers_false_past_its_end() {
        let ev = Caps::parse("30001b");
        assert!(ev.has(EV_KEY) && ev.has(EV_ABS) && ev.has(EV_SYN));
        assert!(!ev.has(255));
        assert!(Caps::parse("").words.is_empty());
        assert!(!Caps::parse("").has(0));
    }

    /// The two live bitmaps from this machine, and the discovery answer for
    /// each. `qualifies` is the whole filter, so both transports have to pass
    /// it and the controller's own firmware keyboard has to fail it.
    #[test]
    fn discovery_accepts_both_transports_and_rejects_a_keyboard() {
        // BLE Elite 2 (VERIFIED local): ABS=30627 — X, Y, Z, RZ, GAS, BRAKE,
        // HAT0X, HAT0Y. NOTE: no ABS_RX/RY at all.
        let ble_abs = Caps::parse("30627");
        assert!(!ble_abs.has(ABS_RX), "the BLE pad has no ABS_RX — this is the trap");
        let key = Caps::parse(
            "7fff000000000000 1000000000000 8000000000 e080ffdf01cfffff fffffffffffffffe",
        );
        assert!(qualifies(&Caps::parse("30001b"), &key, &ble_abs));

        // Wired xpad: X, Y, Z, RX, RY, RZ, HAT0X, HAT0Y.
        let usb_abs = Caps::parse("3003f");
        assert!(qualifies(&Caps::parse("20000b"), &key, &usb_abs));

        // The controller's firmware keyboard node: keys but no axes.
        let kbd = Caps::parse("120013");
        assert!(!qualifies(&kbd, &key, &Caps::parse("")));

        // A pad missing the button cluster is not a pad.
        assert!(!qualifies(&Caps::parse("30001b"), &Caps::parse("0"), &usb_abs));
    }

    /// The loop-back guard. hyprpad's own pad and Steam's live under
    /// `/sys/devices/virtual/input/`; nothing physical does.
    #[test]
    fn virtual_nodes_are_excluded_by_path_not_by_vendor() {
        assert!(is_virtual(Path::new("/sys/devices/virtual/input/input265/event20")));
        assert!(is_virtual(Path::new("/sys/devices/virtual/input/input99")));
        assert!(!is_virtual(Path::new(
            "/sys/devices/pci0000:00/0000:00:08.3/usb3/3-2/3-2.1/input/input221/event25"
        )));
        // A uhid-backed Bluetooth pad lives under virtual/misc/uhid, NOT under
        // virtual/input — it is a real controller and must not be filtered.
        assert!(!is_virtual(Path::new(
            "/sys/devices/virtual/misc/uhid/0005:045E:0B22.0074/input/input242/event31"
        )));
    }

    // --- axis layout -------------------------------------------------------

    #[test]
    fn the_usb_layout_puts_the_right_stick_on_rx_ry_and_triggers_on_z_rz() {
        let m = AxisMap::detect(&Caps::parse("3003f"), |_| AbsRange::signed(32767));
        assert_eq!(m.role(ABS_RX).unwrap().0, Axis::RightX);
        assert_eq!(m.role(ABS_RY).unwrap().0, Axis::RightY);
        assert_eq!(m.role(ABS_Z).unwrap().0, Axis::TriggerL);
        assert_eq!(m.role(ABS_RZ).unwrap().0, Axis::TriggerR);
        assert_eq!(m.role(ABS_HAT0X).unwrap().0, Axis::HatX);
    }

    #[test]
    fn the_bluetooth_layout_puts_the_right_stick_on_z_rz_and_triggers_on_brake_gas() {
        let m = AxisMap::detect(&Caps::parse("30627"), |_| AbsRange::signed(32767));
        assert_eq!(m.role(ABS_Z).unwrap().0, Axis::RightX);
        assert_eq!(m.role(ABS_RZ).unwrap().0, Axis::RightY);
        assert_eq!(m.role(ABS_BRAKE).unwrap().0, Axis::TriggerL);
        assert_eq!(m.role(ABS_GAS).unwrap().0, Axis::TriggerR);
        assert!(m.role(ABS_RX).is_none());
    }

    #[test]
    fn a_range_normalises_both_polarities() {
        let stick = AbsRange::signed(32767);
        assert!((stick.bipolar(32767) - 1.0).abs() < 1e-9);
        assert!(stick.bipolar(0).abs() < 1e-9);
        assert!((stick.bipolar(-32768) + 1.0).abs() < 1e-4);
        // A pad whose sticks are unsigned 0..65535 normalises to the same thing.
        let unsigned = AbsRange { min: 0, max: 65535 };
        assert!(unsigned.bipolar(65535) > 0.999);
        assert!(unsigned.bipolar(32767).abs() < 1e-4);
        let trig = AbsRange::unsigned(1023);
        assert!((trig.unipolar(1023) - 1.0).abs() < 1e-9);
        assert!(trig.unipolar(0).abs() < 1e-9);
        assert!((trig.unipolar(512) - 0.5).abs() < 1e-3);
    }

    // --- frame building ----------------------------------------------------

    fn usb_builder() -> FrameBuilder {
        FrameBuilder::new(AxisMap::from_parts(&[
            (ABS_X, Axis::LeftX, AbsRange::signed(32767)),
            (ABS_Y, Axis::LeftY, AbsRange::signed(32767)),
            (ABS_RX, Axis::RightX, AbsRange::signed(32767)),
            (ABS_RY, Axis::RightY, AbsRange::signed(32767)),
            (ABS_Z, Axis::TriggerL, AbsRange::unsigned(1023)),
            (ABS_RZ, Axis::TriggerR, AbsRange::unsigned(1023)),
            (ABS_HAT0X, Axis::HatX, AbsRange::signed(1)),
            (ABS_HAT0Y, Axis::HatY, AbsRange::signed(1)),
        ]))
    }

    fn ev(kind: u16, code: u16, value: i32) -> InputEvent {
        InputEvent { tv_sec: 0, tv_usec: 0, kind, code, value }
    }

    /// A frame is published on `SYN_REPORT` and only then — the same
    /// one-frame-per-report contract the controller's decoder has.
    #[test]
    fn events_accumulate_and_a_syn_publishes() {
        let mut b = usb_builder();
        assert!(b.apply(&ev(EV_KEY, BTN_SOUTH, 1)).is_none());
        assert!(b.apply(&ev(EV_KEY, BTN_TR, 1)).is_none());
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).expect("a frame");
        assert_eq!(f.source, report::Source::Evdev);
        assert!(f.pressed(Button::A) && f.pressed(Button::BumperR1));
        assert!(!f.pressed(Button::B));
        // Release is just as sticky until the next syn.
        b.apply(&ev(EV_KEY, BTN_SOUTH, 0));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(!f.pressed(Button::A), "state persists between reports, and clears on release");
        assert!(f.pressed(Button::BumperR1), "an untouched button keeps its state");
    }

    #[test]
    fn the_xbox_button_is_the_guide_and_the_face_cluster_matches_both_paths() {
        let mut b = usb_builder();
        for (code, want) in [
            (BTN_MODE, Button::Steam),
            (BTN_SOUTH, Button::A),
            (BTN_EAST, Button::B),
            (BTN_X, Button::X),
            (BTN_Y, Button::Y),
            (BTN_SELECT, Button::View),
            (BTN_START, Button::Menu),
            (BTN_THUMBL, Button::L3),
            (BTN_THUMBR, Button::R3),
            (BTN_TL, Button::BumperL1),
            (BTN_TR, Button::BumperR1),
        ] {
            b.apply(&ev(EV_KEY, code, 1));
            let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
            assert!(f.pressed(want), "{code:#x} -> {want:?}");
            b.apply(&ev(EV_KEY, code, 0));
            b.apply(&ev(EV_SYN, SYN_REPORT, 0));
        }
        // BTN_C / BTN_Z exist on the BLE node and mean nothing.
        b.apply(&ev(EV_KEY, BTN_C, 1));
        b.apply(&ev(EV_KEY, BTN_Z, 1));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert_eq!(f.buttons, 0, "BTN_C and BTN_Z are not hyprpad buttons");
    }

    /// Both paddle code sets land on the same four grips, in the same order.
    #[test]
    fn paddles_arrive_under_either_code_set() {
        for (set, codes) in [
            ("grip", [BTN_GRIPR, BTN_GRIPR2, BTN_GRIPL, BTN_GRIPL2]),
            (
                "trigger_happy",
                [BTN_TRIGGER_HAPPY5, BTN_TRIGGER_HAPPY6, BTN_TRIGGER_HAPPY7, BTN_TRIGGER_HAPPY8],
            ),
        ] {
            let mut b = usb_builder();
            for c in codes {
                b.apply(&ev(EV_KEY, c, 1));
            }
            let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
            // P1 -> R4, P2 -> R5, P3 -> L4, P4 -> L5.
            for want in [Button::GripR4, Button::GripR5, Button::GripL4, Button::GripL5] {
                assert!(f.pressed(want), "{set}: {want:?}");
            }
        }
    }

    #[test]
    fn the_hat_becomes_four_dpad_bits_and_diagonals_set_two() {
        let mut b = usb_builder();
        b.apply(&ev(EV_ABS, ABS_HAT0Y, -1));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(f.pressed(Button::DpadUp) && !f.pressed(Button::DpadDown));
        b.apply(&ev(EV_ABS, ABS_HAT0X, 1));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(f.pressed(Button::DpadUp) && f.pressed(Button::DpadRight), "up-right diagonal");
        b.apply(&ev(EV_ABS, ABS_HAT0X, 0));
        b.apply(&ev(EV_ABS, ABS_HAT0Y, 0));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert_eq!(f.buttons, 0, "a centred hat presses nothing");
    }

    /// Drivers that report the d-pad as buttons rather than a hat work too.
    #[test]
    fn a_button_dpad_works_beside_the_hat() {
        let mut b = usb_builder();
        b.apply(&ev(EV_KEY, BTN_DPAD_LEFT, 1));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(f.pressed(Button::DpadLeft));
    }

    /// Y inversion is the one sign error that would be invisible until someone
    /// pushed a stick up and the cursor went down.
    #[test]
    fn stick_y_is_inverted_and_x_is_not() {
        let mut b = usb_builder();
        // evdev +Y is DOWN; the controller's +Y is UP.
        b.apply(&ev(EV_ABS, ABS_Y, 32767));
        b.apply(&ev(EV_ABS, ABS_X, 32767));
        b.apply(&ev(EV_ABS, ABS_RY, -32767));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert_eq!(f.left_stick.0, 32767, "+X stays +X");
        assert_eq!(f.left_stick.1, -32767, "stick pushed down reads as -Y");
        assert_eq!(f.right_stick.1, 32767, "stick pushed up reads as +Y");
    }

    /// The full-pull bit is a Schmitt trigger, because the gesture engine's
    /// chords are edge-triggered: a trigger resting on the threshold must not
    /// chatter.
    #[test]
    fn the_trigger_full_pull_bit_has_hysteresis() {
        assert!(!trigger_full(false, 0.84));
        assert!(trigger_full(false, 0.85));
        assert!(trigger_full(true, 0.71), "still down inside the gap");
        assert!(!trigger_full(true, 0.70), "released at the lower threshold");

        let mut b = usb_builder();
        // 1023 full scale: 0.84 * 1023 = 859, 0.86 -> 879, 0.75 -> 767.
        b.apply(&ev(EV_ABS, ABS_Z, 859));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(!f.pressed(Button::TriggerL2Full));
        assert!(f.l2 > 27_000 && f.l2 < 28_000, "analog value scaled to the controller's range");
        b.apply(&ev(EV_ABS, ABS_Z, 879));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(f.pressed(Button::TriggerL2Full));
        b.apply(&ev(EV_ABS, ABS_Z, 767));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(f.pressed(Button::TriggerL2Full), "0.75 is inside the hysteresis gap");
        b.apply(&ev(EV_ABS, ABS_Z, 700));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(!f.pressed(Button::TriggerL2Full));
        assert_eq!(f.r2, 0, "the other trigger is untouched");
    }

    /// An Xbox frame has no pads, and every pad consumer must see that as
    /// "untouched" rather than as garbage.
    #[test]
    fn an_evdev_frame_has_no_pads() {
        let mut b = usb_builder();
        b.apply(&ev(EV_KEY, BTN_SOUTH, 1));
        let f = b.apply(&ev(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert!(!f.source.has_pads());
        assert!(f.source.cursor_is_rate());
        assert!(!f.source.has_haptics());
        assert!(!f.pressed(Button::PadLeftTouch) && !f.pressed(Button::PadRightTouch));
        assert_eq!(f.left_pad, report::Pad::default());
        assert_eq!(f.right_pad, report::Pad::default());
    }

    // --- a replayed stream -------------------------------------------------

    /// A whole synthetic session, in the shape and order a real device sends
    /// it: a d-pad press, a guide chord, a paddle, a trigger pull and a stick
    /// deflection, each terminated by its own `SYN_REPORT`.
    ///
    /// The fixture is a text file (`tests/data/xbox-elite-usb.evtest`) in the
    /// `type code value` form `evtest` prints, so a capture from a real pad can
    /// replace it verbatim.
    #[test]
    fn a_recorded_event_stream_replays_into_the_right_frames() {
        let text = include_str!("../tests/data/xbox-elite-usb.evtest");
        let mut b = usb_builder();
        let mut frames: Vec<report::Frame> = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split_whitespace();
            let kind = parse_num(it.next().unwrap());
            let code = parse_num(it.next().unwrap());
            let value = parse_num(it.next().unwrap());
            if let Some(f) = b.apply(&ev(kind as u16, code as u16, value)) {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 6, "one frame per SYN_REPORT");
        assert!(frames[0].pressed(Button::DpadUp));
        assert!(frames[1].pressed(Button::Steam), "guide down");
        assert!(frames[2].pressed(Button::Steam) && frames[2].pressed(Button::GripR4));
        assert!(!frames[3].pressed(Button::Steam) && !frames[3].pressed(Button::GripR4));
        assert!(frames[4].pressed(Button::TriggerR2Full));
        assert!(frames[5].right_stick.0 > 20_000 && frames[5].right_stick.1 > 20_000);
    }

    fn parse_num(s: &str) -> i32 {
        match s.strip_prefix("0x") {
            Some(hex) => i32::from_str_radix(hex, 16).expect("hex"),
            None => s.parse().expect("decimal"),
        }
    }

    // --- ioctl encoding ----------------------------------------------------

    /// The request numbers are the one thing here with no compiler to check
    /// it; pinned against the values `<linux/input.h>` generates.
    #[test]
    fn the_ioctl_requests_match_the_uapi_encoding() {
        assert_eq!(eviocgrab(), 0x4004_4590);
        assert_eq!(eviocgabs(ABS_X), 0x8018_4540);
        assert_eq!(eviocgabs(ABS_RZ), 0x8018_4545);
        assert_eq!(eviocgkey(96), 0x8060_4518);
    }

    #[test]
    fn an_input_event_is_the_size_the_kernel_writes() {
        // 2 * 8 (timeval) + 2 + 2 + 4 on a 64-bit target.
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<RawAbsInfo>(), 24);
    }

    /// Discovery must not panic or hang on the machine running the tests,
    /// whatever is plugged into it — and must never return a virtual node.
    #[test]
    fn scanning_the_real_machine_is_safe_and_excludes_virtual_nodes() {
        let Ok(nodes) = gamepad_nodes() else { return };
        // Printed under `--nocapture`, which is how you check the filter
        // against whatever is actually plugged into the machine in front of
        // you without a daemon and without opening anything.
        for n in &nodes {
            eprintln!(
                "evdev: {} {:04x}:{:04x} {:?} paddles={:?} layout={}",
                n.path.display(),
                n.vendor,
                n.product,
                n.name,
                n.paddles(),
                n.layout()
            );
        }
        for n in &nodes {
            assert!(n.path.starts_with("/dev/input/event"));
            let real = fs::canonicalize(
                Path::new("/sys/class/input").join(n.path.file_name().unwrap()),
            );
            if let Ok(real) = real {
                assert!(!is_virtual(&real), "{} is virtual", n.path.display());
            }
        }
    }
}
