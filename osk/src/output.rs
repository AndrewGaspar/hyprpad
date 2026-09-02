//! Text injection via a kernel `uinput` keyboard emitting **real evdev
//! keycodes**.
//!
//! This is copied and adapted from the parent `hyprpad` crate's
//! `src/output.rs` (`VirtualKeyboard`), which is proven working on this
//! machine. The keyboard model in [`crate::layout`] already carries each key's
//! raw `KEY_*` keycode, so committing a key is just tapping that keycode here.
//!
//! **Why uinput and not `zwp_virtual_keyboard_v1`.** The Wayland
//! virtual-keyboard + synthetic-keymap route types cleanly into native Wayland
//! apps but *garbles into XWayland* — it produces digits (validated on this
//! stack, docs/07-open-questions.md Q14, and the §3.2 "digits" fingerprint). A
//! kernel uinput keyboard with real `KEY_*` codes is resolved against the
//! user's actual keymap by every downstream client, native Wayland and XWayland
//! alike, so it types correctly EVERYWHERE. `/dev/uinput` is user-accessible
//! here via Steam's udev `uaccess` rule.
//!
//! **Coverage is ASCII / US-layout** — the same bound as the parent backend.
//! Full-Unicode-into-native-apps via a generated keymap on
//! `zwp_virtual_keyboard_v1` (osk-technology.md §3.2), and the
//! `gamescope_input_method` backend for nested games (§2.5), are deliberately
//! DEFERRED — see the crate README's done/stubbed/deferred map.

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
/// resolve them against the user's actual keymap.
pub struct VirtualKeyboard {
    file: File,
}

impl VirtualKeyboard {
    /// Create the uinput device, register the standard key range, and wait
    /// ~200 ms for Hyprland/libinput to enumerate it before any keystroke.
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
            // `key()`/`tap()` calls in this range are deliverable.
            for code in 1..=255i32 {
                set_bit(fd, UI_SET_KEYBIT, code)?;
            }

            let mut setup: UinputSetup = std::mem::zeroed();
            setup.id = InputId { bustype: BUS_USB, vendor: 0x1234, product: 0x5678, version: 1 };
            let name = b"hyprpad-osk virtual keyboard";
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
        // notice the new evdev node and the compositor add it to the seat.
        std::thread::sleep(Duration::from_millis(200));
        Ok(VirtualKeyboard { file })
    }

    /// Type a string of ASCII text. Characters outside printable ASCII are
    /// silently skipped (US-layout coverage; see the module note).
    pub fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            if let Some((code, shift)) = key_for(c) {
                self.tap(code, shift);
            }
        }
    }

    /// Tap one raw evdev keycode, optionally with Shift held around it. This is
    /// the bridge from the layout model: a [`crate::layout::Key`]'s `keycode`
    /// goes straight in here, with `shift` from the active shift state.
    pub fn tap(&mut self, code: u16, shift: bool) {
        if code == 0 {
            return; // meta key with no direct keycode
        }
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

    /// Press or release a raw Linux keycode (`KEY_*`) with no timing — for a
    /// caller that manages hold/repeat itself (e.g. backspace auto-repeat).
    pub fn key(&mut self, linux_keycode: u16, pressed: bool) {
        self.emit_key(linux_keycode, pressed);
    }

    /// Emit a key event plus the `SYN_REPORT` that commits it.
    fn emit_key(&mut self, code: u16, pressed: bool) {
        self.write_event(EV_KEY, code, i32::from(pressed));
        self.write_event(EV_SYN, SYN_REPORT, 0);
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) {
        let ev = InputEvent { tv_sec: 0, tv_usec: 0, type_, code, value };
        // SAFETY: `InputEvent` is `#[repr(C)]`; we only view its bytes to write
        // them to the uinput fd.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &ev as *const InputEvent as *const u8,
                std::mem::size_of::<InputEvent>(),
            )
        };
        if let Err(e) = self.file.write_all(bytes) {
            eprintln!("hyprpad-osk: uinput write failed: {e}");
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
        return Err(format!("uinput ioctl {req:#x}: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Map a printable ASCII character to `(evdev keycode, needs_shift)` on a US
/// layout. Returns `None` for characters this backend does not type.
pub fn key_for(c: char) -> Option<(u16, bool)> {
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
        assert_eq!(key_for('9'), Some((10, false)));
        assert_eq!(key_for(' '), Some((57, false)));
        assert_eq!(key_for('!'), Some((2, true)));
        assert_eq!(key_for('?'), Some((53, true)));
        assert_eq!(key_for('é'), None);
    }

    #[test]
    fn input_structs_have_the_kernel_abi_size() {
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<InputId>(), 8);
    }
}
