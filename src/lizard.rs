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

use std::fs::OpenOptions;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::time::Duration;

/// `REPORT_ID_FEATURES_CONTROLLER` — the IBEX feature-report id for the
/// controller (as opposed to the dongle). Byte 0 of every feature frame.
const REPORT_ID_FEATURES_CONTROLLER: u8 = 0x01;

/// `ID_CLEAR_DIGITAL_MAPPINGS` — clear the firmware button→key/mouse mappings.
const ID_CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;
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

/// Open one puck node read-write and send the whole disable sequence to it.
fn send_sequence(node: &Path, reports: &[[u8; WIRE_LEN]]) -> Result<(), SendErr> {
    // Read-write: the passive tap opens read-only, but `HIDIOCSFEATURE` is a
    // write to the device and needs a writable fd.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(node)
        .map_err(|e| SendErr::Other(format!("open read-write: {e}")))?;
    for report in reports {
        send_feature_report(file.as_raw_fd(), report)?;
    }
    Ok(())
}

/// Disable lizard mode on the attached puck.
///
/// Finds the puck's hidraw nodes and sends the disable sequence to each that
/// accepts it. hidraw is not exclusive, so this succeeds while Steam also holds
/// the nodes. The puck exposes one interface per pairing slot plus a
/// dongle-control interface; only the slot with the connected controller (and
/// the controller feature report) will accept the reports, and the active slot
/// can change on replug — so we try every node and treat the call as successful
/// if *any* node accepted the full sequence.
///
/// Returns `Err` only if no node could be found or none accepted the reports;
/// per-node failures (e.g. the dongle-control interface, which has no controller
/// feature report) are expected and folded into the error only when they are
/// total.
pub fn disable_lizard_mode() -> Result<(), String> {
    let nodes = crate::hidraw::puck_nodes().map_err(|e| format!("enumerating puck nodes: {e}"))?;
    if nodes.is_empty() {
        return Err("no Steam Controller puck found (28de:1304)".to_string());
    }
    let reports = disable_sequence();
    let mut accepted = 0usize;
    let mut stalls = 0usize;
    let mut hard_errors = Vec::new();
    for node in &nodes {
        match send_sequence(node, &reports) {
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
    fn hidiocsfeature_encoding() {
        // _IOC(WRITE|READ, 'H', 0x06, 64) on the asm-generic layout.
        assert_eq!(hidiocsfeature(WIRE_LEN), 0xC040_4806);
        // Length is encoded in the size field, so a 2-byte report differs.
        assert_eq!(hidiocsfeature(2), 0xC002_4806);
    }
}
