//! Output injection: a Wayland virtual pointer and a uinput virtual keyboard.
//!
//! These are the two OUTPUT primitives the daemon drives (W2). The input side
//! (decode -> gesture -> config -> arbitrate -> dispatch) stays dependency-free;
//! everything here needs to talk to the compositor or the kernel, so this is
//! where the crate's first real dependencies live.
//!
//! **Why two different mechanisms.** They are chosen from the empirically
//! validated injection matrix on this machine (see
//! `docs/research/osk-technology.md` §1–3 and `docs/07-open-questions.md` Q14):
//!
//! * **Pointer** — `zwlr_virtual_pointer_v1`. Hyprland implements it, it is not
//!   permission-gated, and it moves the real cursor over every surface
//!   including XWayland games. This is the piece Steam cannot provide on
//!   Wayland: a controller-driven desktop cursor.
//! * **Keyboard** — a **uinput** device emitting real evdev keycodes. The
//!   Wayland `zwp_virtual_keyboard_v1` + synthetic-keymap route types cleanly
//!   into native Wayland apps but *garbles into XWayland* (produces digits —
//!   proven, Q14). A kernel uinput keyboard with real `KEY_*` codes types
//!   correctly everywhere, XWayland included. `/dev/uinput` is user-accessible
//!   here via Steam's udev `uaccess` rule.
//!
//! Both constructors return `Result` and the daemon treats a failure as
//! non-fatal: gestures and workspace switching keep working without them.

use std::time::Instant;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_pointer, wl_registry};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

// ---------------------------------------------------------------------------
// Virtual pointer (zwlr_virtual_pointer_v1)
// ---------------------------------------------------------------------------

/// A mouse button, mapped to its Linux evdev button code on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

impl PointerButton {
    /// The `BTN_*` evdev code the protocol expects.
    fn code(self) -> u32 {
        match self {
            PointerButton::Left => 0x110,   // BTN_LEFT
            PointerButton::Right => 0x111,  // BTN_RIGHT
            PointerButton::Middle => 0x112, // BTN_MIDDLE
        }
    }
}

/// State object for the Wayland event queue. The virtual-pointer protocol is
/// pure output — neither the manager nor the pointer emits events — so this is
/// a zero-sized sink; the only dispatch that matters is the registry one that
/// [`registry_queue_init`] requires.
struct PointerState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for PointerState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(PointerState: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(PointerState: ignore ZwlrVirtualPointerV1);

/// A relative-motion pointer injected through `zwlr_virtual_pointer_v1`.
///
/// The protocol mirrors `wl_pointer`: each `motion`/`button`/`axis` request
/// must be followed by a `frame` to commit the batch, and each carries a
/// millisecond timestamp. The compositor mostly ignores the exact timestamp
/// value; we pass a monotonic counter derived from a start [`Instant`].
pub struct VirtualPointer {
    conn: Connection,
    // The queue owns the proxies' event routing; kept alive for the pointer's
    // lifetime even though nothing dispatches on it.
    _queue: EventQueue<PointerState>,
    pointer: ZwlrVirtualPointerV1,
    start: Instant,
}

impl VirtualPointer {
    /// Connect to the compositor and bind `zwlr_virtual_pointer_manager_v1`,
    /// then create a single virtual pointer with no seat suggestion.
    pub fn new() -> Result<VirtualPointer, String> {
        let conn =
            Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, queue) = registry_queue_init::<PointerState>(&conn)
            .map_err(|e| format!("wayland registry init: {e}"))?;
        let qh = queue.handle();
        let manager: ZwlrVirtualPointerManagerV1 =
            globals.bind(&qh, 1..=2, ()).map_err(|e| {
                format!("bind zwlr_virtual_pointer_manager_v1 ({e}); compositor lacks wlr-virtual-pointer?")
            })?;
        let pointer = manager.create_virtual_pointer(None, &qh, ());
        conn.flush().map_err(|e| format!("wayland flush: {e}"))?;
        Ok(VirtualPointer {
            conn,
            _queue: queue,
            pointer,
            start: Instant::now(),
        })
    }

    /// Monotonic millisecond timestamp for the next request batch.
    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Push the buffered requests to the compositor. Failures are swallowed;
    /// the daemon degrades to "no cursor" rather than crashing on a lost socket.
    fn flush(&self) {
        let _ = self.conn.flush();
    }

    /// Move the cursor by a relative `(dx, dy)` in compositor pixels. `+dy` is
    /// down (screen coordinates), matching `wl_pointer`.
    pub fn move_relative(&mut self, dx: f64, dy: f64) {
        let t = self.time();
        self.pointer.motion(t, dx, dy);
        self.pointer.frame();
        self.flush();
    }

    /// Press or release a mouse button.
    pub fn button(&mut self, btn: PointerButton, pressed: bool) {
        let t = self.time();
        let state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        self.pointer.button(t, btn.code(), state);
        self.pointer.frame();
        self.flush();
    }

    /// Scroll by `(dx, dy)`. `+dy` scrolls down, `+dx` scrolls right, in the
    /// same continuous units `wl_pointer::axis` uses.
    pub fn scroll(&mut self, dx: f64, dy: f64) {
        let t = self.time();
        if dy != 0.0 {
            self.pointer.axis(t, wl_pointer::Axis::VerticalScroll, dy);
        }
        if dx != 0.0 {
            self.pointer.axis(t, wl_pointer::Axis::HorizontalScroll, dx);
        }
        self.pointer.frame();
        self.flush();
    }
}

// ---------------------------------------------------------------------------
// Virtual keyboard (uinput, real evdev keycodes)
// ---------------------------------------------------------------------------

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::Duration;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0x00;
const BUS_USB: u16 = 0x03;

const KEY_LEFTSHIFT: u16 = 42;

// uinput ioctl request codes (generic Linux encoding, matches x86_64 here):
//   UI_SET_EVBIT  = _IOW('U', 100, int)              -> 0x40045564
//   UI_SET_KEYBIT = _IOW('U', 101, int)              -> 0x40045565
//   UI_DEV_SETUP  = _IOW('U',   3, struct(92 bytes)) -> 0x405c5503
//   UI_DEV_CREATE = _IO('U', 1)                       -> 0x5501
//   UI_DEV_DESTROY= _IO('U', 2)                       -> 0x5502
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

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
struct InputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

/// A kernel-level virtual keyboard created through `/dev/uinput`.
///
/// It emits **real evdev keycodes** (`KEY_H`, `KEY_LEFTSHIFT`, …), so the
/// compositor and every downstream client — native Wayland *and* XWayland —
/// resolve them against the user's actual keymap. That is the whole reason for
/// choosing uinput over `zwp_virtual_keyboard_v1`, whose synthetic keymap is
/// discarded by XWayland (Q14).
///
/// **Coverage is ASCII/US-layout.** [`type_text`](Self::type_text) maps the
/// printable ASCII set (letters, digits, common punctuation, space, tab,
/// newline) to `KEY_*` + Shift. Non-ASCII characters are skipped. Full Unicode
/// into native Wayland apps would need `zwp_virtual_keyboard_v1` with a
/// generated keymap; that is deliberately out of scope for this backend, which
/// exists to reach XWayland correctly.
pub struct VirtualKeyboard {
    file: File,
}

impl VirtualKeyboard {
    /// Create the uinput device, register the ASCII key set, and wait ~200 ms
    /// for Hyprland/libinput to enumerate it before any keystroke is sent.
    pub fn new() -> Result<VirtualKeyboard, String> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| format!("open /dev/uinput: {e} (Steam's uaccess udev rule grants it)"))?;
        let fd = file.as_raw_fd();

        // SAFETY: `fd` is a valid, open uinput fd for the duration of this
        // block. Each ioctl below is the documented uinput setup call with the
        // argument type its request code encodes.
        unsafe {
            set_bit(fd, UI_SET_EVBIT, EV_KEY as libc::c_int)?;
            set_bit(fd, UI_SET_EVBIT, EV_SYN as libc::c_int)?;
            // Register the standard key range so both `type_text` and arbitrary
            // `key()` calls in this range are deliverable.
            for code in 1..=127i32 {
                set_bit(fd, UI_SET_KEYBIT, code)?;
            }

            let mut setup: UinputSetup = std::mem::zeroed();
            setup.id = InputId {
                bustype: BUS_USB,
                vendor: 0x1234,
                product: 0x5678,
                version: 1,
            };
            let name = b"hyprsc virtual keyboard";
            for (i, &b) in name.iter().enumerate() {
                setup.name[i] = b as libc::c_char;
            }
            if libc::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup) < 0 {
                return Err(format!("UI_DEV_SETUP: {}", std::io::Error::last_os_error()));
            }
            if libc::ioctl(fd, UI_DEV_CREATE) < 0 {
                return Err(format!("UI_DEV_CREATE: {}", std::io::Error::last_os_error()));
            }
        }

        // The device is not usable the instant it is created: libinput has to
        // notice the new evdev node and the compositor has to add it to the
        // seat. Typing immediately drops the first keystrokes.
        std::thread::sleep(Duration::from_millis(200));
        Ok(VirtualKeyboard { file })
    }

    /// Type a string of ASCII text. Characters outside the printable ASCII set
    /// are silently skipped (see the type-level note on coverage).
    pub fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            if let Some((code, shift)) = key_for(c) {
                self.type_key(code, shift);
            }
        }
    }

    /// Press or release a raw Linux keycode (`KEY_*`).
    pub fn key(&mut self, linux_keycode: u16, pressed: bool) {
        self.emit_key(linux_keycode, pressed);
    }

    /// Tap one key, optionally with Shift held around it.
    fn type_key(&mut self, code: u16, shift: bool) {
        if shift {
            self.emit_key(KEY_LEFTSHIFT, true);
        }
        self.emit_key(code, true);
        std::thread::sleep(Duration::from_millis(6));
        self.emit_key(code, false);
        if shift {
            self.emit_key(KEY_LEFTSHIFT, false);
        }
        std::thread::sleep(Duration::from_millis(6));
    }

    /// Emit a key event plus the `SYN_REPORT` that commits it.
    fn emit_key(&mut self, code: u16, pressed: bool) {
        self.write_event(EV_KEY, code, i32::from(pressed));
        self.write_event(EV_SYN, SYN_REPORT, 0);
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) {
        let ev = InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_,
            code,
            value,
        };
        // SAFETY: `InputEvent` is `#[repr(C)]` with no padding-sensitive reads;
        // we only view its bytes to write them to the uinput fd.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &ev as *const InputEvent as *const u8,
                std::mem::size_of::<InputEvent>(),
            )
        };
        if let Err(e) = self.file.write_all(bytes) {
            eprintln!("hyprsc: uinput write failed: {e}");
        }
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        // SAFETY: destroying the device on the same valid fd we created it with.
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

/// Issue a `UI_SET_*BIT`-style ioctl carrying a single `int`.
///
/// # Safety
/// `fd` must be an open `/dev/uinput` file descriptor.
unsafe fn set_bit(fd: libc::c_int, req: libc::c_ulong, val: libc::c_int) -> Result<(), String> {
    if libc::ioctl(fd, req, val) < 0 {
        return Err(format!(
            "uinput ioctl {req:#x}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Map a printable ASCII character to `(evdev keycode, needs_shift)` on a US
/// layout. Returns `None` for characters this backend does not type.
fn key_for(c: char) -> Option<(u16, bool)> {
    // KEY_A..KEY_Z in alphabetical order.
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    // KEY_0..KEY_9 indexed by digit value (0 -> KEY_0 = 11).
    const DIGITS: [u16; 10] = [11, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    match c {
        'a'..='z' => Some((LETTERS[c as usize - 'a' as usize], false)),
        'A'..='Z' => Some((LETTERS[c as usize - 'A' as usize], true)),
        '0'..='9' => Some((DIGITS[c as usize - '0' as usize], false)),
        ' ' => Some((57, false)),  // KEY_SPACE
        '\n' => Some((28, false)), // KEY_ENTER
        '\t' => Some((15, false)), // KEY_TAB
        '!' => Some((2, true)),
        '@' => Some((3, true)),
        '#' => Some((4, true)),
        '$' => Some((5, true)),
        '%' => Some((6, true)),
        '^' => Some((7, true)),
        '&' => Some((8, true)),
        '*' => Some((9, true)),
        '(' => Some((10, true)),
        ')' => Some((11, true)),
        '-' => Some((12, false)),
        '_' => Some((12, true)),
        '=' => Some((13, false)),
        '+' => Some((13, true)),
        '[' => Some((26, false)),
        '{' => Some((26, true)),
        ']' => Some((27, false)),
        '}' => Some((27, true)),
        '\\' => Some((43, false)),
        '|' => Some((43, true)),
        ';' => Some((39, false)),
        ':' => Some((39, true)),
        '\'' => Some((40, false)),
        '"' => Some((40, true)),
        '`' => Some((41, false)),
        '~' => Some((41, true)),
        ',' => Some((51, false)),
        '<' => Some((51, true)),
        '.' => Some((52, false)),
        '>' => Some((52, true)),
        '/' => Some((53, false)),
        '?' => Some((53, true)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_map_covers_letters_digits_and_shift() {
        assert_eq!(key_for('h'), Some((35, false)));
        assert_eq!(key_for('H'), Some((35, true)));
        assert_eq!(key_for('a'), Some((30, false)));
        assert_eq!(key_for('z'), Some((44, false)));
        assert_eq!(key_for('0'), Some((11, false)));
        assert_eq!(key_for('1'), Some((2, false)));
        assert_eq!(key_for('9'), Some((10, false)));
        assert_eq!(key_for(' '), Some((57, false)));
        assert_eq!(key_for('!'), Some((2, true)));
        assert_eq!(key_for('?'), Some((53, true)));
        // Non-ASCII is unsupported by this backend.
        assert_eq!(key_for('é'), None);
    }

    #[test]
    fn input_structs_have_the_kernel_abi_size() {
        // These must match the kernel's struct layout exactly or every ioctl /
        // write silently corrupts. Guard the sizes the ioctl codes encode.
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<InputId>(), 8);
    }

    #[test]
    fn pointer_button_codes_are_evdev() {
        assert_eq!(PointerButton::Left.code(), 0x110);
        assert_eq!(PointerButton::Right.code(), 0x111);
        assert_eq!(PointerButton::Middle.code(), 0x112);
    }
}
