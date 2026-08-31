//! A daemon-owned `uinput` keyboard for the bare-button bindings.
//!
//! This is adapted from the on-screen keyboard's `VirtualKeyboard`
//! (`osk/src/output.rs`), which is the proven working reference on this
//! machine, trimmed to the daemon's needs: the daemon does not type ASCII text,
//! it presses and releases whole `KEY_*` codes (a bare D-pad press becomes an
//! arrow key held down for as long as the D-pad is held).
//!
//! **Why uinput and not `zwp_virtual_keyboard_v1`.** The Wayland
//! virtual-keyboard route garbles into XWayland (docs/07-open-questions.md Q14):
//! it produces digits. A kernel uinput keyboard emitting real `KEY_*` codes is
//! resolved against the user's actual keymap by every downstream client, native
//! Wayland and XWayland alike, so the arrow keys land correctly everywhere.
//! `/dev/uinput` is user-accessible here via Steam's udev `uaccess` rule.
//!
//! **Auto-repeat is the kernel's job.** We emit key-down on press and key-up on
//! release only; holding the D-pad leaves the arrow key down, and the
//! kernel/compositor synthesise the repeat, exactly like a real keyboard.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::Duration;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0x00;
const BUS_USB: u16 = 0x03;

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
/// It emits **real evdev keycodes** (`KEY_UP`, `KEY_LEFT`, …), so the compositor
/// and every downstream client — native Wayland *and* XWayland — resolve them
/// against the user's actual keymap.
pub struct VirtualKeyboard {
    file: File,
}

impl VirtualKeyboard {
    /// Create the uinput device, register the standard key range (`1..=127`,
    /// which covers the arrow and navigation keys the bare-button table emits —
    /// `KEY_UP` = 103 … `KEY_PAGEDOWN` = 109 all sit inside it), and wait
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
            // Register the standard key range so every code the bare-button
            // key-name table can produce is deliverable.
            for code in 1..=127i32 {
                set_bit(fd, UI_SET_KEYBIT, code)?;
            }

            let mut setup: UinputSetup = std::mem::zeroed();
            setup.id = InputId { bustype: BUS_USB, vendor: 0x1234, product: 0x5678, version: 1 };
            let name = b"hyprpad virtual keyboard";
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

    /// Press (`pressed = true`) or release (`false`) one raw Linux keycode
    /// (`KEY_*`). No timing and no auto-repeat of our own: hold maps to a held
    /// key so the kernel/compositor handle repeat naturally.
    pub fn key(&mut self, linux_keycode: u16, pressed: bool) {
        if linux_keycode == 0 {
            return;
        }
        self.write_event(EV_KEY, linux_keycode, i32::from(pressed));
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
            eprintln!("hyprpad: uinput write failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_structs_have_the_kernel_abi_size() {
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<InputId>(), 8);
    }
}
