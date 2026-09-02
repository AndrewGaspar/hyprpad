//! Lizard-mode ownership: disable the puck's firmware keyboard/mouse emulation.
//!
//! The 2026 Steam Controller runs "lizard mode" in firmware: without a host
//! driver it maps the dpad to arrow keys, A/B to Enter/Esc, the right pad to a
//! mouse cursor, and so on, emitting them on its evdev keyboard/mouse nodes
//! (docs/03-hardware-findings.md, "Lizard-mode output map"). Steam normally
//! turns this off; when Steam is denied the device (docs/experiments/w12), or
//! simply absent, the firmware keyboard/mouse keeps firing and fights hyprpad's
//! own cursor and gestures. This module lets hyprpad disable lizard mode itself.
//!
//! We do it exactly the way the mainline kernel `hid-steam` driver and SDL do:
//! raw hidraw **feature reports** sent with the `HIDIOCSFEATURE` ioctl. On this
//! kernel the puck binds `hid-generic`, not `hid-steam`, so there is no in-kernel
//! lizard control to defer to — we must send the reports ourselves.
//!
//! ## The report sequence (source: Linux `drivers/hid/hid-steam.c`)
//!
//! The 2026 puck (`28de:1304`, `USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS`) is
//! driven under `STEAM_QUIRK_IBEX | STEAM_QUIRK_WIRELESS`. `steam_set_lizard_mode`
//! with `enable = false` sends, in order:
//!
//! 1. `ID_CLEAR_DIGITAL_MAPPINGS` (`0x81`) — clears the button→key/mouse mappings.
//! 2. `ID_SET_SETTINGS_VALUES` (`0x87`) with, for the IBEX/Deck path,
//!    `SETTING_LIZARD_MODE` (`9`) = 0 and `SETTING_STEAM_WATCHDOG_ENABLE`
//!    (`71`) = 0. Disabling the watchdog stops the controller from reverting to
//!    lizard mode on its own when it stops seeing a host heartbeat.
//!
//! The IBEX feature-report wire frame is `[report_id][cmd bytes…]` zero-padded to
//! the report length, where `report_id = REPORT_ID_FEATURES_CONTROLLER` (`1`).
//! On this puck the controller feature report (id 1) declares a **63-byte** body,
//! so the full frame the kernel transfers is **64 bytes** (`hid_report_len` =
//! 63 + 1 for the id) — verified live by reading the device's HID report
//! descriptor. `ID_SET_SETTINGS_VALUES` packs as `0x87, len, (setting, val_lo,
//! val_hi)…` with `len = 3 * settings`. SDL's `controller_constants.h` defines
//! the identical command/setting numbers (`ID_CLEAR_DIGITAL_MAPPINGS`,
//! `ID_SET_SETTINGS_VALUES`, `SETTING_LIZARD_MODE`, `SETTING_STEAM_WATCHDOG_ENABLE`).
//!
//! Only these two documented, non-persistent reports are ever sent. No factory
//! reset, no digital-mapping *writes*, nothing speculative.
//!
//! ## Safety of concurrent access, and the sleeping controller
//!
//! hidraw is not exclusive (docs/03): opening a puck node read-write and sending
//! a feature report works while Steam holds the same node. The reports are
//! idempotent, so re-sending them is harmless.
//!
//! A `SET_FEATURE` only succeeds when a controller is actually attached to the
//! addressed slot: with the controller asleep or unpaired the endpoint STALLs
//! (`EPIPE`) because there is nothing to configure. The mainline driver treats
//! `EPIPE` as retryable (up to 50 × 20 ms); we do the same on a shorter budget
//! so a persistently-idle controller does not stall the re-send loop.

use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

/// `REPORT_ID_FEATURES_CONTROLLER` — the IBEX feature-report id for the
/// controller (as opposed to the dongle). Byte 0 of every feature frame.
const REPORT_ID_FEATURES_CONTROLLER: u8 = 0x01;

/// `ID_CLEAR_DIGITAL_MAPPINGS` — clear the firmware button→key/mouse mappings.
const ID_CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;
/// `ID_SET_DEFAULT_DIGITAL_MAPPINGS` — restore the firmware's default
/// button→key/mouse mappings (the mirror of `ID_CLEAR_DIGITAL_MAPPINGS`). This
/// is exactly what the kernel `hid-steam` driver sends from
/// `steam_set_lizard_mode(enable = true)` to bring the lizard keyboard/mouse
/// back (`STEAM_CMD_DEFAULT_MAPPINGS`).
const ID_SET_DEFAULT_DIGITAL_MAPPINGS: u8 = 0x85;
/// `ID_SET_SETTINGS_VALUES` — write `(setting, u16 value)` pairs.
const ID_SET_SETTINGS_VALUES: u8 = 0x87;

/// `SETTING_LIZARD_MODE` — the master lizard-mode switch (0 = off).
const SETTING_LIZARD_MODE: u8 = 9;
/// `SETTING_STEAM_WATCHDOG_ENABLE` — the "is a host active?" watchdog that
/// otherwise reverts the controller to lizard mode (0 = off).
const SETTING_STEAM_WATCHDOG_ENABLE: u8 = 71;

/// Length of the controller feature-report body, excluding the leading report
/// id. The puck's controller feature report (id 1) declares 63 payload bytes;
/// with the report id that makes a 64-byte wire frame (confirmed from the live
/// HID report descriptor). Sending any other length STALLs the endpoint.
const FEATURE_BODY_LEN: usize = 63;
/// Full on-the-wire feature buffer: report id byte + body (64 bytes).
const WIRE_LEN: usize = FEATURE_BODY_LEN + 1;

/// EPIPE retry budget for one feature-report send. The mainline `hid-steam`
/// driver retries a STALLed send up to 50 times at 20 ms; we use a shorter
/// budget (enough to ride out a transient link stall when a controller *is*
/// attached) so that an idle/asleep controller — which STALLs every attempt —
/// does not block the re-send loop for long.
const EPIPE_RETRIES: u32 = 12;
/// Delay between EPIPE retries (matches the kernel's 20 ms).
const EPIPE_RETRY_DELAY: Duration = Duration::from_millis(20);

/// How often the ownership loop re-sends the disable sequence.
///
/// The settings write disables the firmware watchdog, so the controller will
/// *not* revert to lizard mode on its own while it stays powered. A periodic
/// re-send exists only to re-cover the controller after it power-cycles or
/// reconnects (which resets it to defaults, lizard on) without needing explicit
/// reconnect detection. 30 s bounds the window in which the firmware
/// keyboard/mouse could briefly fight hyprpad after a replug; the reports are
/// idempotent so the cost of re-sending is negligible.
pub const RESEND_INTERVAL: Duration = Duration::from_secs(30);

/// Build the `ID_CLEAR_DIGITAL_MAPPINGS` feature frame.
fn clear_digital_mappings_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_CLEAR_DIGITAL_MAPPINGS;
    buf
}

/// Build the `ID_SET_SETTINGS_VALUES` feature frame that turns lizard mode and
/// the revert-watchdog off (the IBEX/Deck disable path).
fn disable_lizard_settings_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_SET_SETTINGS_VALUES;
    buf[2] = 6; // payload length = 3 bytes * 2 settings
    // SETTING_LIZARD_MODE = 0 (u16 little-endian)
    buf[3] = SETTING_LIZARD_MODE;
    buf[4] = 0x00;
    buf[5] = 0x00;
    // SETTING_STEAM_WATCHDOG_ENABLE = 0 (u16 little-endian)
    buf[6] = SETTING_STEAM_WATCHDOG_ENABLE;
    buf[7] = 0x00;
    buf[8] = 0x00;
    buf
}

/// The full disable-lizard feature sequence, in send order: clear the digital
/// mappings, then write the disabling settings.
fn disable_sequence() -> [[u8; WIRE_LEN]; 2] {
    [clear_digital_mappings_report(), disable_lizard_settings_report()]
}

/// Build the `ID_SET_DEFAULT_DIGITAL_MAPPINGS` feature frame — restore the
/// default button→key/mouse mappings that [`clear_digital_mappings_report`]
/// wiped, so the firmware keyboard/mouse fires again.
fn set_default_digital_mappings_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_SET_DEFAULT_DIGITAL_MAPPINGS;
    buf
}

/// Build the `ID_SET_SETTINGS_VALUES` feature frame that turns lizard mode and
/// the revert-watchdog back **on** — the exact inverse of
/// [`disable_lizard_settings_report`] (values 1 instead of 0). Re-enabling
/// `SETTING_STEAM_WATCHDOG_ENABLE` matters: disable turned it off, and it is the
/// switch that lets the firmware fall back to lizard mode on its own when no
/// host heartbeat is seen.
fn enable_lizard_settings_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_SET_SETTINGS_VALUES;
    buf[2] = 6; // payload length = 3 bytes * 2 settings
    // SETTING_LIZARD_MODE = 1 (u16 little-endian)
    buf[3] = SETTING_LIZARD_MODE;
    buf[4] = 0x01;
    buf[5] = 0x00;
    // SETTING_STEAM_WATCHDOG_ENABLE = 1 (u16 little-endian)
    buf[6] = SETTING_STEAM_WATCHDOG_ENABLE;
    buf[7] = 0x01;
    buf[8] = 0x00;
    buf
}

/// The full enable-lizard feature sequence, in send order: restore the default
/// digital mappings, then write the enabling settings. Undoes exactly what
/// [`disable_sequence`] did, so the firmware keyboard/mouse comes back when
/// hyprpad exits.
fn enable_sequence() -> [[u8; WIRE_LEN]; 2] {
    [set_default_digital_mappings_report(), enable_lizard_settings_report()]
}

// `HIDIOCSFEATURE(len)` from <linux/hidraw.h> is `_IOC(_IOC_WRITE|_IOC_READ,
// 'H', 0x06, len)`. We encode the asm-generic ioctl layout (x86_64, aarch64,
// arm, …): dir[31:30] size[29:16] type[15:8] nr[7:0].
const _IOC_NRSHIFT: u32 = 0;
const _IOC_TYPESHIFT: u32 = 8;
const _IOC_SIZESHIFT: u32 = 16;
const _IOC_DIRSHIFT: u32 = 30;
const _IOC_WRITE: u32 = 1;
const _IOC_READ: u32 = 2;

/// The `HIDIOCSFEATURE` ioctl request code for a payload of `len` bytes.
const fn hidiocsfeature(len: usize) -> libc::c_ulong {
    let dir = _IOC_WRITE | _IOC_READ;
    let ty = b'H' as u32;
    let nr = 0x06u32;
    ((dir << _IOC_DIRSHIFT)
        | (ty << _IOC_TYPESHIFT)
        | (nr << _IOC_NRSHIFT)
        | ((len as u32) << _IOC_SIZESHIFT)) as libc::c_ulong
}

/// Why a send to one node failed.
enum SendErr {
    /// The endpoint STALLed (`EPIPE`) even after retries: no controller is
    /// attached to this slot to receive the settings (asleep/unpaired), or the
    /// link stalled longer than our retry budget.
    Stall,
    /// Any other failure (could not open the node, or an unexpected errno).
    Other(String),
}

/// Send one feature report on an open hidraw fd via `HIDIOCSFEATURE`, retrying
/// a STALL (`EPIPE`) a bounded number of times as the kernel driver does.
fn send_feature_report(fd: RawFd, report: &[u8]) -> Result<(), SendErr> {
    let request = hidiocsfeature(report.len());
    let mut attempts = EPIPE_RETRIES;
    loop {
        // SAFETY: `fd` is a live, open hidraw fd for the duration of the call.
        // `HIDIOCSFEATURE(len)` reads exactly `report.len()` bytes from the
        // pointer; `report` is a readable slice of that length. The ioctl only
        // reads the buffer (SET_REPORT), so no aliasing/mutation concern.
        let ret = unsafe { libc::ioctl(fd, request, report.as_ptr()) };
        if ret >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPIPE) {
            attempts -= 1;
            if attempts == 0 {
                return Err(SendErr::Stall);
            }
            std::thread::sleep(EPIPE_RETRY_DELAY);
            continue;
        }
        return Err(SendErr::Other(format!("HIDIOCSFEATURE: {err}")));
    }
}

/// Send the whole sequence to one already-open puck node.
///
/// The descriptor is writable by construction: [`crate::hidraw::OPEN_FLAGS`] is
/// `O_RDWR` precisely because `HIDIOCSFEATURE` is a write to the device, and the
/// broker opens with the same flags.
fn send_sequence(fd: RawFd, reports: &[[u8; WIRE_LEN]]) -> Result<(), SendErr> {
    for report in reports {
        send_feature_report(fd, report)?;
    }
    Ok(())
}

/// Send one feature sequence to every puck node, succeeding if *any* node
/// accepts the full sequence.
///
/// Finds the puck's hidraw nodes and sends `reports` to each. hidraw is not
/// exclusive, so this succeeds while Steam also holds the nodes. The puck
/// exposes one interface per pairing slot plus a dongle-control interface; only
/// the slot with the connected controller (and the controller feature report)
/// will accept the reports, and the active slot can change on replug — so we try
/// every node and treat the call as successful if *any* node accepted the full
/// sequence.
///
/// Returns `Err` only if no node could be found or none accepted the reports;
/// per-node failures (e.g. the dongle-control interface, which has no controller
/// feature report) are expected and folded into the error only when they are
/// total.
fn apply_to_puck(reports: &[[u8; WIRE_LEN]]) -> Result<(), String> {
    // Through the same acquire path everything else uses, so lizard ownership
    // survives `packaging/udev/72-hyprpad-puck.rules` making the nodes root-only:
    // with a broker installed these descriptors come from it, without one they
    // are direct opens exactly as before.
    let opened = crate::hidraw::PuckSource::acquire()
        .ok_or_else(|| "no Steam Controller puck found (28de:1304)".to_string())?
        .into_open();
    if opened.is_empty() {
        return Err("no Steam Controller puck node could be opened".to_string());
    }
    let mut accepted = 0usize;
    let mut stalls = 0usize;
    let mut hard_errors = Vec::new();
    for (node, fd) in &opened {
        match send_sequence(fd.as_raw_fd(), reports) {
            Ok(()) => accepted += 1,
            Err(SendErr::Stall) => stalls += 1,
            Err(SendErr::Other(e)) => hard_errors.push(format!("{}: {e}", node.display())),
        }
    }
    if accepted > 0 {
        Ok(())
    } else if hard_errors.is_empty() {
        // Every node STALLed: the puck is there but has no controller to
        // configure right now (asleep or unpaired). Not a hard failure — the
        // re-send loop will catch it once the controller wakes.
        Err(format!(
            "no controller to configure: all {stalls} puck node(s) STALLed \
             (controller asleep or disconnected)"
        ))
    } else {
        Err(hard_errors.join("; "))
    }
}

/// Disable lizard mode on the attached puck (clear the digital mappings, then
/// turn `SETTING_LIZARD_MODE` and the revert-watchdog off).
pub fn disable_lizard_mode() -> Result<(), String> {
    apply_to_puck(&disable_sequence())
}

/// Re-enable lizard mode on the attached puck — the inverse of
/// [`disable_lizard_mode`]: restore the default digital mappings, then turn
/// `SETTING_LIZARD_MODE` and `SETTING_STEAM_WATCHDOG_ENABLE` back on. Called on
/// exit so the firmware keyboard/mouse comes back and the user is not left
/// without a pointer.
///
/// Like disable, this is best-effort against a possibly-absent controller: if
/// the puck is asleep/unpaired every node STALLs and this returns `Err`. That is
/// fine on exit — the firmware powers up in lizard mode by default, so the next
/// wake restores it anyway.
pub fn enable_lizard_mode() -> Result<(), String> {
    apply_to_puck(&enable_sequence())
}

/// Best-effort lizard restore for the exit paths: re-enable lizard mode and log
/// the outcome (never propagate the error — the process is on its way out).
pub fn restore_lizard_on_exit() {
    match enable_lizard_mode() {
        Ok(()) => eprintln!("hyprpad: lizard mode re-enabled on exit (firmware kbd/mouse restored)"),
        Err(e) => eprintln!("hyprpad: best-effort lizard restore on exit skipped: {e}"),
    }
}

/// Run lizard-mode ownership forever: disable it now, then re-send on a timer to
/// re-cover the controller across power-cycles/reconnects
/// ([`RESEND_INTERVAL`]). Degrades gracefully — a failed write logs a warning
/// and the loop keeps trying rather than taking the daemon down.
///
/// Intended to be spawned on its own thread from [`crate::run`].
pub fn own_lizard_loop() {
    // Log only on state transitions so the periodic re-send stays quiet: one
    // line when we first take ownership, one when it first can't (e.g. the
    // controller is asleep), and again whenever the state flips back.
    let mut last_ok = false;
    let mut warned = false;
    loop {
        match disable_lizard_mode() {
            Ok(()) => {
                if !last_ok {
                    eprintln!("hyprpad: lizard mode disabled on the puck (owning it)");
                }
                last_ok = true;
                warned = false;
            }
            Err(e) => {
                if !warned {
                    eprintln!("warning: lizard-mode disable pending: {e} (retrying)");
                    warned = true;
                }
                last_ok = false;
            }
        }
        std::thread::sleep(RESEND_INTERVAL);
    }
}

/// RAII guard that restores lizard mode when it drops. Owning one for the life
/// of [`crate::run::run`] covers the *normal* exit paths: a clean return and a
/// panic unwinding out of the daemon both run this destructor. Signal-driven
/// exit (Ctrl-C / SIGTERM) does **not** unwind, so it is handled separately by
/// [`install_signal_restore`], whose waiter calls `std::process::exit` before
/// this guard could run — the two paths never both fire.
pub struct LizardRestoreGuard;

impl Drop for LizardRestoreGuard {
    fn drop(&mut self) {
        restore_lizard_on_exit();
    }
}

/// Write end of the self-pipe the signal handler nudges. `-1` until installed.
static SIGNAL_WRITE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The SIGINT/SIGTERM handler. Async-signal-safe: it does nothing but poke the
/// self-pipe with the signal number so the waiter thread can do the real work.
extern "C" fn on_exit_signal(sig: libc::c_int) {
    let fd = SIGNAL_WRITE_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // `write` is on POSIX's async-signal-safe list; best-effort, ignore the
        // result — a full/closed pipe just means the waiter already fired.
        let _ = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

/// Install SIGINT/SIGTERM handling that restores lizard mode before the process
/// dies.
///
/// A `Drop` guard cannot cover a signal, because a signal terminates the process
/// without unwinding — and the real restore (`open`/`ioctl`/allocation/logging)
/// is not async-signal-safe, so it must not run inside the handler. We use the
/// self-pipe trick: the handler only `write`s the signal number to a pipe, and a
/// dedicated waiter thread — blocked on the read end — performs the restore in
/// ordinary thread context, then exits `128 + signum`.
///
/// We deliberately install a plain handler and do **not** block the signals:
/// blocking would set a process-wide mask that child processes (the OSK) inherit,
/// which would stop Ctrl-C from ever reaching them. `SA_RESTART` keeps worker
/// threads' blocking reads from erroring out on the delivery.
pub fn install_signal_restore() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!(
            "warning: lizard restore-on-signal not installed (pipe: {})",
            std::io::Error::last_os_error()
        );
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    SIGNAL_WRITE_FD.store(write_fd, std::sync::atomic::Ordering::Relaxed);

    // SAFETY: `action` is fully zero-initialised, then `sa_sigaction`,
    // `sa_mask`, and `sa_flags` are set to valid values before `sigaction` reads
    // it; the null old-action pointer is allowed.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_exit_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }

    std::thread::spawn(move || {
        // Park until the handler pokes the pipe, then restore and exit.
        let mut byte = [0u8; 1];
        // SAFETY: `read_fd` is the live read end; `byte` is a valid 1-byte buffer.
        let n = unsafe { libc::read(read_fd, byte.as_mut_ptr().cast(), 1) };
        let sig = if n == 1 { libc::c_int::from(byte[0]) } else { 0 };
        if sig != 0 {
            eprintln!("hyprpad: caught signal {sig}, restoring lizard mode before exit");
        }
        restore_lizard_on_exit();
        std::process::exit(128 + sig);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_length_is_64() {
        // report id + 63-byte controller feature body, per the HID descriptor.
        assert_eq!(WIRE_LEN, 64);
    }

    #[test]
    fn clear_mappings_report_bytes() {
        let r = clear_digital_mappings_report();
        assert_eq!(r.len(), 64);
        assert_eq!(r[0], 0x01); // REPORT_ID_FEATURES_CONTROLLER
        assert_eq!(r[1], 0x81); // ID_CLEAR_DIGITAL_MAPPINGS
        assert!(r[2..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn set_settings_report_bytes() {
        let r = disable_lizard_settings_report();
        assert_eq!(r.len(), 64);
        // 0x87 SET_SETTINGS_VALUES, len 6, (9,0,0) lizard off, (71,0,0) watchdog off.
        assert_eq!(
            &r[..9],
            &[0x01, 0x87, 0x06, 0x09, 0x00, 0x00, 0x47, 0x00, 0x00]
        );
        assert!(r[9..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn sequence_is_clear_then_settings() {
        let seq = disable_sequence();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0][1], 0x81); // clear digital mappings first
        assert_eq!(seq[1][1], 0x87); // then set settings values
    }

    #[test]
    fn set_default_mappings_report_bytes() {
        let r = set_default_digital_mappings_report();
        assert_eq!(r.len(), 64);
        assert_eq!(r[0], 0x01); // REPORT_ID_FEATURES_CONTROLLER
        assert_eq!(r[1], 0x85); // ID_SET_DEFAULT_DIGITAL_MAPPINGS
        assert!(r[2..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn enable_settings_report_bytes() {
        let r = enable_lizard_settings_report();
        assert_eq!(r.len(), 64);
        // 0x87 SET_SETTINGS_VALUES, len 6, (9,1,0) lizard on, (71,1,0) watchdog on.
        assert_eq!(
            &r[..9],
            &[0x01, 0x87, 0x06, 0x09, 0x01, 0x00, 0x47, 0x01, 0x00]
        );
        assert!(r[9..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn enable_sequence_is_default_mappings_then_settings() {
        let seq = enable_sequence();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0][1], 0x85); // restore default digital mappings first
        assert_eq!(seq[1][1], 0x87); // then set settings values
    }

    #[test]
    fn enable_exactly_inverts_disable_settings() {
        // Same command, same settings, same order — only the values flip 0 <-> 1.
        let dis = disable_lizard_settings_report();
        let en = enable_lizard_settings_report();
        assert_eq!(dis[1], en[1]); // ID_SET_SETTINGS_VALUES
        assert_eq!(dis[2], en[2]); // payload length
        assert_eq!(dis[3], en[3]); // SETTING_LIZARD_MODE id
        assert_eq!(dis[6], en[6]); // SETTING_STEAM_WATCHDOG_ENABLE id
        assert_eq!((dis[4], en[4]), (0x00, 0x01)); // lizard value 0 -> 1
        assert_eq!((dis[7], en[7]), (0x00, 0x01)); // watchdog value 0 -> 1
    }

    #[test]
    fn hidiocsfeature_encoding() {
        // _IOC(WRITE|READ, 'H', 0x06, 64) on the asm-generic layout.
        assert_eq!(hidiocsfeature(WIRE_LEN), 0xC040_4806);
        // Length is encoded in the size field, so a 2-byte report differs.
        assert_eq!(hidiocsfeature(2), 0xC002_4806);
    }
}
