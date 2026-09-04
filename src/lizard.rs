//! Lizard-mode ownership: disable the controller's firmware keyboard/mouse
//! emulation, on whichever transport it is reachable over.
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
//! kernel the controller binds `hid-generic`, not `hid-steam`, so there is no in-kernel
//! lizard control to defer to — we must send the reports ourselves.
//!
//! ## The report sequence (source: Linux `drivers/hid/hid-steam.c`)
//!
//! The 2026 controller — reached through the puck, `28de:1304`
//! `USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS` — is
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
//! On this device the controller feature report (id 1) declares a **63-byte** body,
//! so the full frame the kernel transfers is **64 bytes** (`hid_report_len` =
//! 63 + 1 for the id) — verified live by reading the device's HID report
//! descriptor. `ID_SET_SETTINGS_VALUES` packs as `0x87, len, (setting, val_lo,
//! val_hi)…` with `len = 3 * settings`. SDL's `controller_constants.h` defines
//! the identical command/setting numbers (`ID_CLEAR_DIGITAL_MAPPINGS`,
//! `ID_SET_SETTINGS_VALUES`, `SETTING_LIZARD_MODE`, `SETTING_STEAM_WATCHDOG_ENABLE`).
//!
//! Only these two documented, non-persistent reports are ever sent by the
//! ownership loop. No factory reset, no digital-mapping *writes*, nothing
//! speculative.
//!
//! ## The firmware power knobs, and the deliberate power-off
//!
//! Three more commands live here, all from the same documented set and all
//! opt-in (`docs/research/guide-hold-poweroff.md`):
//!
//! * [`PowerSettings`] — `SETTING_STEAMBUTTON_POWEROFF_TIME` (25) and
//!   `SETTING_SLEEP_INACTIVITY_TIMEOUT` (50), appended to the *same* `0x87`
//!   frame the disable sequence already re-sends every 30 s. Setting 25 is the
//!   register behind "holding the Steam button while deliberating turns the
//!   controller off": the firmware owns that timer, not Steam and not hyprpad.
//!   Nothing is written unless the config asks.
//! * [`read_settings`] / [`read_controller_settings`] — the `0x89`/`0x8B`/`0x8C`
//!   round trip that asks the firmware what a setting currently is, what
//!   maximum it accepts and what its factory default is. Read-only, and the
//!   only way to learn the units the write above needs, since Valve publishes
//!   no defaults table. `hyprpad controller-settings` is its front-end.
//! * [`turn_off_controller`] — `0x9F ID_TURN_OFF_CONTROLLER`, the *deliberate*
//!   power-off that replaces the accidental one, on a chord, `hyprpad off`, or
//!   a right-click on the bar widget.
//!
//! ## Safety of concurrent access, and the sleeping controller
//!
//! hidraw is not exclusive (docs/03): opening a controller node read-write and sending
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

// The power-related command and setting numbers are taken from
// [`crate::uhid::settings`] rather than respelled here, so the two halves of
// the project — the one that *writes* these to the real controller and the one that
// *decodes* Steam writing them to the virtual one — cannot drift. Their
// ultimate source is SDL's `controller_constants.h`, catalogued in
// `docs/research/guide-hold-poweroff.md` §1.4.

/// `SETTING_STEAMBUTTON_POWEROFF_TIME` (25 / `0x19`) — how long the Steam
/// button must be held before the *firmware* powers the controller off. The
/// knob this whole module-level feature exists for; see [`PowerSettings`] for
/// what is and is not known about its units.
const SETTING_STEAMBUTTON_POWEROFF_TIME: u8 =
    crate::uhid::settings::setting::STEAMBUTTON_POWEROFF_TIME;
/// `SETTING_SLEEP_INACTIVITY_TIMEOUT` (50 / `0x32`) — how long the controller
/// sits idle before it sleeps on its own. A `u16` of **seconds** on the 2015
/// firmware (sc-controller's `sc_dongle.py` writes it as one, default 600);
/// UNVERIFIED on Triton.
const SETTING_SLEEP_INACTIVITY_TIMEOUT: u8 =
    crate::uhid::settings::setting::SLEEP_INACTIVITY_TIMEOUT;
/// `SETTING_IMU_MODE` (48 / `0x30`) — the gyro switch. A `u16` bitmask of
/// [`crate::uhid::settings::gyro_mode`] flags; `0` is off. Written into the same
/// `0x87` frame as everything else here, which is what keeps the "one writer"
/// invariant true while Steam drives the controller's IMU through the relay.
const SETTING_IMU_MODE: u8 = crate::uhid::settings::setting::IMU_MODE;

/// `ID_GET_SETTINGS_VALUES` — ask the firmware what a setting is *currently*
/// set to.
pub const ID_GET_SETTINGS_VALUES: u8 = 0x89;
/// `ID_GET_SETTINGS_MAXS` — ask the firmware the maximum a setting accepts.
pub const ID_GET_SETTINGS_MAXS: u8 = 0x8B;
/// `ID_GET_SETTINGS_DEFAULTS` — ask the firmware a setting's factory default.
pub const ID_GET_SETTINGS_DEFAULTS: u8 = 0x8C;

/// `ID_TURN_OFF_CONTROLLER` — the deliberate power-off command.
const ID_TURN_OFF_CONTROLLER: u8 = crate::uhid::settings::cmd::TURN_OFF_CONTROLLER;

/// The payload sc-controller sends with [`ID_TURN_OFF_CONTROLLER`]: the four
/// ASCII bytes `off!` (`scc/drivers/sc_dongle.py:368-371`, "Mercilessly stolen
/// from scraw library"), making the frame `01 9f 04 6f 66 66 21` zero-padded to
/// [`WIRE_LEN`]. Whether Triton actually requires the magic is **UNVERIFIED**
/// (`docs/research/guide-hold-poweroff.md` §2, §6); a bare `01 9f 00` is the
/// documented fallback to try if this is ignored.
const TURN_OFF_MAGIC: &[u8; 4] = b"off!";

/// The largest number of `(setting, u16)` pairs one [`settings_report`] frame
/// can carry: the payload starts at byte 3 of a [`WIRE_LEN`] buffer and each
/// pair is three bytes.
const MAX_SETTINGS_PAIRS: usize = (WIRE_LEN - 3) / 3;

/// Length of the controller feature-report body, excluding the leading report
/// id. The controller feature report (id 1) declares 63 payload bytes;
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

// ---------------------------------------------------------------------------
// The firmware power knobs
// ---------------------------------------------------------------------------

/// The value the config spelling `"off"` writes to either power setting.
///
/// **A guess, and deliberately the safe one of the two candidates.** Valve does
/// not publish `g_DefaultSettingValues`, so neither the unit nor the "disabled"
/// encoding of `SETTING_STEAMBUTTON_POWEROFF_TIME` is known
/// (`docs/research/guide-hold-poweroff.md` §6 lists both as UNVERIFIED). Two
/// readings of `0` are plausible and they are opposites:
///
/// * `0` means **never** — the usual convention for a timeout, and the reading
///   §3.A assumes first ("*`0` is not 'never'.* Then write the maximum the
///   firmware reports");
/// * `0` means **no delay** — in which case writing it powers the controller
///   off on *any* guide press, and the 30 s re-send puts that back after every
///   wake.
///
/// So `"off"` writes `0xFFFF` instead: the largest value the `u16` field can
/// carry — "as long as the firmware can express" — which is ~65 s under the
/// shortest candidate unit (milliseconds), 18 hours under seconds, and is safe
/// under *both* readings. §3.A's own fallback is the same idea, and
/// `0x8B ID_GET_SETTINGS_MAXS` ([`read_settings`], `hyprpad controller-settings`) is
/// how to learn what maximum the firmware will actually accept.
///
/// To test the "`0` = never" reading, write the integer explicitly:
/// `steam_button_poweroff = 0`.
pub const POWER_SETTING_OFF: u16 = u16::MAX;

/// The two firmware power knobs hyprpad can write, as the config asked for
/// them. `None` means "write nothing", which is the default and today's
/// behaviour: a setting hyprpad does not mention keeps whatever the firmware
/// has.
///
/// # What is known, and what is not
///
/// * **`steam_button_poweroff`** (`SETTING_STEAMBUTTON_POWEROFF_TIME`, 25) is
///   the register behind the complaint this feature exists for: the firmware,
///   not Steam and not hyprpad, powers the controller off on a long Steam-button hold
///   (`docs/research/guide-hold-poweroff.md` §1). Its **units, default, maximum
///   and whether `0` disables it are all UNVERIFIED**, as is whether Triton
///   honours the setting at all — the name and number come from a shared enum
///   that dates to the 2015 controller. Nothing public writes it.
/// * **`sleep_inactivity_timeout`** (`SETTING_SLEEP_INACTIVITY_TIMEOUT`, 50) is
///   the passive power-off — how long the controller sits idle before sleeping.
///   sc-controller writes it as a `u16` of **seconds** (default 600) on the
///   2015 firmware, which is the only third-party evidence for the unit of
///   *any* setting in this frame; UNVERIFIED on Triton.
///
/// Both are therefore knobs the owner is expected to **test**: read them with
/// `hyprpad controller-settings 25 50`, write a value, read it back, then time a hold
/// with a stopwatch. See the verify recipe in `docs/design/controller-power.md`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerSettings {
    /// `SETTING_STEAMBUTTON_POWEROFF_TIME` (25), raw as written to the wire.
    pub steam_button_poweroff: Option<u16>,
    /// `SETTING_SLEEP_INACTIVITY_TIMEOUT` (50), raw as written to the wire.
    pub sleep_inactivity_timeout: Option<u16>,
}

impl PowerSettings {
    /// The `(setting, value)` pairs to append to the lizard-disable frame — in
    /// setting-number order, and empty when the config set neither knob (the
    /// default, which keeps the frame byte-for-byte what it has always been).
    pub fn pairs(&self) -> Vec<(u8, u16)> {
        let mut pairs = Vec::new();
        if let Some(v) = self.steam_button_poweroff {
            pairs.push((SETTING_STEAMBUTTON_POWEROFF_TIME, v));
        }
        if let Some(v) = self.sleep_inactivity_timeout {
            pairs.push((SETTING_SLEEP_INACTIVITY_TIMEOUT, v));
        }
        pairs
    }

    /// Whether the config asked for nothing, i.e. whether the settings write is
    /// unchanged from the historical two-pair frame.
    pub fn is_empty(&self) -> bool {
        self.steam_button_poweroff.is_none() && self.sleep_inactivity_timeout.is_none()
    }
}

/// The power settings the ownership loop currently writes.
///
/// A process-wide cell rather than an argument because [`disable_lizard_mode`]
/// is called from four places in [`crate::run`] — startup, reconnect, reload and
/// the ownership loop's own timer — none of which should have to thread a
/// config value through, and because the loop runs on its own thread with no
/// channel back to the daemon loop. [`crate::run`] pushes the config's value in
/// at startup and again on every reload, so a `hyprpad reload` retunes the next
/// re-send with nothing to restart.
static POWER_SETTINGS: std::sync::RwLock<PowerSettings> =
    std::sync::RwLock::new(PowerSettings {
        steam_button_poweroff: None,
        sleep_inactivity_timeout: None,
    });

/// Install the power settings the next lizard write will carry (from
/// `h.daemon { steam_button_poweroff = …, sleep_inactivity_timeout = … }`).
/// Called on startup and on every live reload.
pub fn set_power_settings(power: PowerSettings) {
    if let Ok(mut slot) = POWER_SETTINGS.write() {
        *slot = power;
    }
}

/// The power settings currently installed. A poisoned lock falls back to
/// "write nothing", which is the historical frame — the safe direction.
pub fn power_settings() -> PowerSettings {
    POWER_SETTINGS.read().map(|p| *p).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The IMU (gyro) hold — gap G5 of `docs/design/uhid-relay.md`
// ---------------------------------------------------------------------------

/// Who currently decides what the controller's IMU is doing, and what they decided.
///
/// # Why this lives here and not in the relay
///
/// The relay decodes Steam's writes; it does not own a descriptor on the real
/// controller and must not acquire one. `docs/research/uhid-steam-controller.md` §5.2
/// is explicit that hyprpad writes the hardware **through one writer**, and for
/// feature reports that writer is this module. So the relay's contribution is a
/// number in this cell, and the number rides out on the *same*
/// `ID_SET_SETTINGS_VALUES` frame [`disable_lizard_settings_report`] already
/// builds — which is also what makes it survive the 30 s
/// [`RESEND_INTERVAL`] re-send, a controller reconnect and a power cycle for free,
/// with no second timer and no second code path.
///
/// # The two authorities
///
/// * [`preference`](Self::preference) is **hyprpad's own**, from
///   `h.gamepad { gyro = … }`. It is what the IMU goes back to when Steam is
///   not asking — the default is [`gyro_mode::OFF`], because a gyro streaming
///   for a desktop nobody is aiming with is battery spent for nothing.
/// * [`steam`](Self::steam) is what the relay last saw Steam write, and it
///   **wins while it is set**. Steam's request is already scoped to a game that
///   asked its runtime for sensors (SDL calls
///   `HIDAPI_DriverSteam_SetSensorsEnabled` on exactly that), so following it
///   verbatim is both the most faithful behaviour and the cheapest.
///
/// # Why the hold is *not* gated on hyprpad's forwarding gate
///
/// It would be one line, and it would be wrong. The forwarding gate flips on
/// every focus change — the guide held for a chord, the OSK raised, a moment on
/// the desktop — many times a minute in ordinary use, and each flip would
/// become a feature-report write to the controller. That is a write storm on a
/// battery device to save power. Steam's own request is already the right
/// granularity, and while hyprpad is not forwarding the relay streams the
/// *neutral* report anyway (`translate::triton_neutral`), whose IMU bytes are
/// zero — so a game sees a parked gyro during a guide chord regardless of what
/// the firmware is doing. The power question is answered by the default being
/// off and by the restore below, not by chasing focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImuHold {
    /// hyprpad's own baseline, from the config.
    preference: u16,
    /// What Steam asked the relay for, while the relay is live.
    steam: Option<u16>,
    /// Whether anything has ever asked for a mode.
    ///
    /// Until something does, **no `SETTING_IMU_MODE` pair is written at all**
    /// and the frame is byte-for-byte the one this module has always sent. That
    /// matters: `kind = "xbox"` users, which is the default, must not start
    /// having a new setting written to their controller because a feature they
    /// do not use exists.
    engaged: bool,
}

impl ImuHold {
    /// The mode the controller should be holding right now, or `None` while nothing
    /// has ever asked and the frame should stay as it was.
    pub fn effective(&self) -> Option<u16> {
        if !self.engaged {
            return None;
        }
        Some(self.steam.unwrap_or(self.preference))
    }

    /// The `(setting, value)` pair to append to the settings frame — empty
    /// until something has asked.
    pub fn pair(&self) -> Option<(u8, u16)> {
        self.effective().map(|mode| (SETTING_IMU_MODE, mode))
    }

    /// The pair the **exit** path writes: hyprpad's own preference, with
    /// Steam's request discarded, because Steam is about to lose the device.
    fn restore_pair(&self) -> Option<(u8, u16)> {
        self.engaged.then_some((SETTING_IMU_MODE, self.preference))
    }
}

/// The IMU hold, shared between the daemon loop (which sets it) and the
/// ownership loop (which sends it). Same shape and same reasoning as
/// [`POWER_SETTINGS`]: the ownership loop runs on its own thread with no
/// channel back to the daemon.
static IMU_HOLD: std::sync::RwLock<ImuHold> =
    std::sync::RwLock::new(ImuHold { preference: 0, steam: None, engaged: false });

/// Install hyprpad's own gyro preference (`h.gamepad { gyro = … }`). Called on
/// startup and on every live reload, exactly like [`set_power_settings`].
///
/// A non-off preference engages the hold immediately, so the very next re-send
/// carries it; an off preference does not, so the default config's frame is
/// unchanged until Steam actually asks for something.
pub fn set_imu_preference(mode: u16) {
    if let Ok(mut hold) = IMU_HOLD.write() {
        hold.preference = mode;
        if mode != crate::uhid::settings::gyro_mode::OFF {
            hold.engaged = true;
        }
    }
}

/// Record what Steam asked the relay for — `Some(mode)` for a write it made,
/// `None` when it has stopped asking (it closed the fake, or the relay went
/// away), which hands the IMU back to [`ImuHold::preference`].
///
/// Returns whether the effective mode changed, so the caller can skip the
/// [`nudge`] when Steam re-sends a mode the controller is already holding — Steam
/// repeats its settings writes, and each one must not become a feature report.
pub fn set_imu_requested(mode: Option<u16>) -> bool {
    let Ok(mut hold) = IMU_HOLD.write() else { return false };
    let before = hold.effective();
    hold.steam = mode;
    if mode.is_some() {
        hold.engaged = true;
    }
    hold.effective() != before
}

/// The IMU hold as it currently stands. A poisoned lock reads as "nothing
/// asked", which is the historical frame — the safe direction, as for
/// [`power_settings`].
pub fn imu_hold() -> ImuHold {
    IMU_HOLD.read().map(|h| *h).unwrap_or_default()
}

/// Build the `ID_CLEAR_DIGITAL_MAPPINGS` feature frame.
fn clear_digital_mappings_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_CLEAR_DIGITAL_MAPPINGS;
    buf
}

/// Build an `ID_SET_SETTINGS_VALUES` feature frame carrying `pairs`.
///
/// `[report_id][0x87][3 * n][(setting, value_lo, value_hi) x n]`, zero-padded to
/// [`WIRE_LEN`] — the frame SDL's `DisableSteamTritonLizardMode` and the
/// kernel's `steam_write_settings` build, generalised from the fixed pair this
/// module used to hardcode so extra settings can ride along in the same write.
/// Pairs past [`MAX_SETTINGS_PAIRS`] would not fit in the report and are
/// dropped; nothing in the daemon can produce that many.
fn settings_report(pairs: &[(u8, u16)]) -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_SET_SETTINGS_VALUES;
    let n = pairs.len().min(MAX_SETTINGS_PAIRS);
    buf[2] = (3 * n) as u8;
    for (i, (id, value)) in pairs.iter().take(n).enumerate() {
        let at = 3 + i * 3;
        buf[at] = *id;
        buf[at + 1] = (*value & 0x00FF) as u8;
        buf[at + 2] = (*value >> 8) as u8;
    }
    buf
}

/// The `(setting, value)` pairs of the disable-lizard write: the two settings
/// this module has always written, plus whatever [`PowerSettings`] the config
/// asked for and whatever [`ImuHold`] is in force — so the power knobs and the
/// gyro are carried by the *same* frame that already goes out every
/// [`RESEND_INTERVAL`] and survives a reconnect for free.
///
/// Ordered by setting number, and the two historical pairs come first, so the
/// default config's frame is byte-for-byte what it has always been.
fn disable_lizard_pairs(power: PowerSettings, imu: ImuHold) -> Vec<(u8, u16)> {
    let mut pairs = vec![
        (SETTING_LIZARD_MODE, 0),
        (SETTING_STEAM_WATCHDOG_ENABLE, 0),
    ];
    pairs.extend(power.pairs());
    pairs.extend(imu.pair());
    pairs
}

/// Build the `ID_SET_SETTINGS_VALUES` feature frame that turns lizard mode and
/// the revert-watchdog off (the IBEX/Deck disable path), plus any configured
/// power settings and the current gyro hold.
fn disable_lizard_settings_report(power: PowerSettings, imu: ImuHold) -> [u8; WIRE_LEN] {
    settings_report(&disable_lizard_pairs(power, imu))
}

/// The full disable-lizard feature sequence, in send order: clear the digital
/// mappings, then write the disabling settings.
fn disable_sequence(power: PowerSettings, imu: ImuHold) -> [[u8; WIRE_LEN]; 2] {
    [clear_digital_mappings_report(), disable_lizard_settings_report(power, imu)]
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
///
/// It deliberately does **not** revert [`PowerSettings`]. There is nothing to
/// revert them *to*: the firmware's defaults for settings 25 and 50 are not
/// published (`docs/research/guide-hold-poweroff.md` §6), so restoring a guess
/// would be worse than leaving the owner's chosen value in place — and a value
/// the daemon wrote is in any case not known to survive a power cycle.
/// `hyprpad controller-settings 25 50` reads the firmware's own defaults
/// (`0x8C ID_GET_SETTINGS_DEFAULTS`) for anyone who wants to put them back.
///
/// It **does** revert the gyro, and unlike the power settings there is
/// something to revert it *to*: hyprpad wrote `SETTING_IMU_MODE` in the first
/// place only because Steam asked, and Steam is losing the device. So the pair
/// goes out one last time carrying [`ImuHold::preference`] — off unless the
/// owner asked for `gyro = true`. Without this, a session that ended while a
/// game had the gyro on would leave the controller streaming IMU data to nobody until
/// its next power cycle.
fn enable_lizard_settings_report(imu: ImuHold) -> [u8; WIRE_LEN] {
    let mut pairs = vec![(SETTING_LIZARD_MODE, 1), (SETTING_STEAM_WATCHDOG_ENABLE, 1)];
    pairs.extend(imu.restore_pair());
    settings_report(&pairs)
}

/// The full enable-lizard feature sequence, in send order: restore the default
/// digital mappings, then write the enabling settings. Undoes exactly what
/// [`disable_sequence`] did, so the firmware keyboard/mouse comes back when
/// hyprpad exits.
fn enable_sequence(imu: ImuHold) -> [[u8; WIRE_LEN]; 2] {
    [set_default_digital_mappings_report(), enable_lizard_settings_report(imu)]
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
    feature_ioctl(0x06, len)
}

/// The `HIDIOCGFEATURE` ioctl request code for a buffer of `len` bytes —
/// `_IOC(_IOC_WRITE|_IOC_READ, 'H', 0x07, len)`, the read twin of
/// [`hidiocsfeature`]. Byte 0 of the buffer selects the report id on the way in
/// and comes back as part of the reply.
const fn hidiocgfeature(len: usize) -> libc::c_ulong {
    feature_ioctl(0x07, len)
}

/// The shared encoding behind both feature ioctls: only the `nr` differs.
const fn feature_ioctl(nr: u32, len: usize) -> libc::c_ulong {
    let dir = _IOC_WRITE | _IOC_READ;
    let ty = b'H' as u32;
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
    /// The node is there but will not take this report — `ENODEV` (it went away
    /// between the open and the ioctl) or `EINVAL` (it does not implement this
    /// feature channel).
    ///
    /// Kept apart from [`SendErr::Other`], and **not** retried, because with two
    /// transports open at once a node that cannot serve a feature report is an
    /// ordinary member of the set rather than a fault: `apply_to_controller`
    /// sends to every node the controller has, and only the live one is expected
    /// to answer. Retrying `EINVAL` twelve times at 20 ms per node would spend a
    /// quarter-second per node learning what the first attempt already said.
    Refused(String),
    /// Any other failure (could not open the node, or an unexpected errno).
    Other(String),
}

/// Send one feature report on an open hidraw fd via `HIDIOCSFEATURE`, retrying
/// a STALL (`EPIPE`) a bounded number of times as the kernel driver does.
///
/// The errno split is the whole of this function's judgement:
///
/// * `EPIPE` — the endpoint stalled. Retried, because on a controller that *is*
///   attached this is transient, and it is what the mainline driver does.
/// * `ENODEV` / `EINVAL` — a definite no. Returned immediately as
///   [`SendErr::Refused`]; see that variant for why it is soft rather than fatal.
/// * anything else — [`SendErr::Other`], which is the only kind that can make
///   the whole call fail.
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
        match err.raw_os_error() {
            Some(libc::EPIPE) => {
                attempts -= 1;
                if attempts == 0 {
                    return Err(SendErr::Stall);
                }
                std::thread::sleep(EPIPE_RETRY_DELAY);
                continue;
            }
            Some(libc::ENODEV) | Some(libc::EINVAL) => {
                return Err(SendErr::Refused(format!("HIDIOCSFEATURE: {err}")));
            }
            _ => return Err(SendErr::Other(format!("HIDIOCSFEATURE: {err}"))),
        }
    }
}

/// Send the whole sequence to one already-open controller node.
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

/// Send one feature sequence to every node the controller has, succeeding if
/// *any* of them accepts the full sequence.
///
/// hidraw is not exclusive, so this succeeds while Steam also holds the nodes.
///
/// # "Try every node, any success wins" is also what makes Bluetooth work
///
/// The rule was written for the puck (the dongle): it exposes one interface per
/// pairing slot plus a dongle-control interface, only the slot with the connected controller
/// accepts a controller feature report, and the active slot can change on
/// replug — so trying all of them and accepting one success was already the
/// honest way to address "wherever the controller actually is".
///
/// `ControllerSource::acquire` now returns the Bluetooth node alongside the
/// dongle's when both exist, and that same rule carries settings to it with **no
/// transport logic here at all**. Whichever link the controller is on is the one
/// that answers; the rest refuse, exactly as an empty pairing slot always did.
///
/// The reports themselves need no change: report id `1`, 64 bytes,
/// `ID_SET_SETTINGS_VALUES = 0x87`, unchanged over BLE — `STEAM_QUIRK_BLE`
/// touches no report layer and there is no BLE chunking for this controller
/// (`docs/research/bluetooth.md` §2.3), which the identical report descriptors
/// on the two transports independently confirm. What *does* change is the cost:
/// a `HIDIOCSFEATURE` over BLE is an ATT round trip rather than a USB control
/// transfer. [`RESEND_INTERVAL`] is 30 s and this sends two frames, so that is
/// invisible — but it is the reason no chattier caller should be added.
///
/// # What counts as a failure
///
/// Returns `Err` only if no node could be found or none accepted the reports.
/// Per-node failures are *expected* — the dongle-control interface has no
/// controller feature report, an empty pairing slot stalls, and a node on the
/// transport the controller is not using does one or the other — so they are
/// folded into the error only when they are total, and a total soft failure
/// (every node stalled or refused) is reported as "no controller to configure"
/// rather than as a fault. The re-send loop simply tries again in 30 s.
fn apply_to_controller(reports: &[[u8; WIRE_LEN]]) -> Result<(), String> {
    // Through the same acquire path everything else uses, so lizard ownership
    // survives `packaging/udev/72-hyprpad-puck.rules` making the nodes root-only:
    // with a broker installed these descriptors come from it, without one they
    // are direct opens exactly as before.
    let opened = crate::hidraw::ControllerSource::acquire()
        .ok_or_else(|| {
            "no Steam Controller found (28de:1304 puck or 28de:1303 Bluetooth)".to_string()
        })?
        .into_open();
    if opened.is_empty() {
        return Err("no Steam Controller node could be opened".to_string());
    }
    let mut accepted = 0usize;
    let mut soft = 0usize;
    let mut hard_errors = Vec::new();
    for (node, fd) in &opened {
        match send_sequence(fd.as_raw_fd(), reports) {
            Ok(()) => accepted += 1,
            // Both of these mean "not this node" rather than "something is
            // wrong": a stalled endpoint has no controller behind it, and a
            // refusal is a node that does not serve this channel.
            Err(SendErr::Stall) | Err(SendErr::Refused(_)) => soft += 1,
            Err(SendErr::Other(e)) => hard_errors.push(format!("{}: {e}", node.display())),
        }
    }
    if accepted > 0 {
        Ok(())
    } else if hard_errors.is_empty() {
        // Every node declined: the controller is present in `/sys` but is not
        // reachable on any of its links right now (asleep, unpaired, or on the
        // other machine). Not a hard failure — the re-send loop will catch it
        // once it wakes.
        Err(format!(
            "no controller to configure: all {soft} node(s) stalled or refused \
             (controller asleep, or connected elsewhere)"
        ))
    } else {
        Err(hard_errors.join("; "))
    }
}

/// Disable lizard mode on the attached controller (clear the digital mappings, then
/// turn `SETTING_LIZARD_MODE` and the revert-watchdog off), carrying whatever
/// [`PowerSettings`] the config installed in the same settings write.
pub fn disable_lizard_mode() -> Result<(), String> {
    apply_to_controller(&disable_sequence(power_settings(), imu_hold()))
}

/// Build the `ID_TURN_OFF_CONTROLLER` feature frame.
///
/// `01 9f 04 6f 66 66 21`, zero-padded to [`WIRE_LEN`] — command `0x9F` with the
/// four-byte payload `"off!"`, exactly the bytes sc-controller sends
/// (`scc/drivers/sc_dongle.py:368-371`) and the form
/// `docs/research/guide-hold-poweroff.md` §2 prescribes. The framing is the
/// same feature-report framing every other command in this module uses, so it
/// goes out through the same `HIDIOCSFEATURE` path with the same STALL
/// handling.
fn turn_off_report() -> [u8; WIRE_LEN] {
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = ID_TURN_OFF_CONTROLLER;
    buf[2] = TURN_OFF_MAGIC.len() as u8;
    buf[3..3 + TURN_OFF_MAGIC.len()].copy_from_slice(TURN_OFF_MAGIC);
    buf
}

/// Turn the controller off, deliberately — the `h.controller_off()` action, the
/// `guide+quickaccess` chord, `hyprpad off`, and a right-click on the bar
/// widget all end here.
///
/// This is the *replacement* for the firmware's guide-hold power-off, not an
/// addition to it: the point of [`PowerSettings::steam_button_poweroff`] is to
/// stop the hold from firing while deliberating, and the point of this is to
/// still have a way to put the controller to sleep.
///
/// Best-effort against a possibly-absent controller, like every other write
/// here: a controller that is asleep STALLs every node and this returns
/// `Err`, which for "turn it off" is a harmless no-op — it already is.
///
/// sc-controller refuses `0x9F` on a *wired* controller
/// (`scc/drivers/sc_by_cable.py:102`); hyprpad does not special-case that
/// because it only ever talks to the puck (`28de:1304`).
pub fn turn_off_controller() -> Result<(), String> {
    apply_to_controller(&[turn_off_report()])
}

/// Re-enable lizard mode on the attached controller — the inverse of
/// [`disable_lizard_mode`]: restore the default digital mappings, then turn
/// `SETTING_LIZARD_MODE` and `SETTING_STEAM_WATCHDOG_ENABLE` back on. Called on
/// exit so the firmware keyboard/mouse comes back and the user is not left
/// without a pointer.
///
/// Like disable, this is best-effort against a possibly-absent controller: if
/// the controller is asleep/unpaired every node STALLs and this returns `Err`. That is
/// fine on exit — the firmware powers up in lizard mode by default, so the next
/// wake restores it anyway.
pub fn enable_lizard_mode() -> Result<(), String> {
    apply_to_controller(&enable_sequence(imu_hold()))
}

// ---------------------------------------------------------------------------
// Reading settings back
// ---------------------------------------------------------------------------
//
// Everything hyprpad writes above is a fire-and-forget `SET_FEATURE`. The
// firmware will also *answer*, and it has to, because the units and defaults of
// the power settings are not published anywhere: the only way to learn what
// setting 25 means on this controller is to ask it
// (`docs/research/guide-hold-poweroff.md` §2, §6).
//
// The round trip is the one the kernel driver uses. `hid-steam.c`'s
// `steam_get_serial` (`drivers/hid/hid-steam.c:667-690`) documents it in a
// comment — "Send: 0xae 0x15 0x01 / Recv: 0xae 0x15 0x01 serialnumber" — and
// implements it as `steam_send_report` (a `SET_FEATURE` of the command frame)
// followed by `steam_recv_report` (a `GET_FEATURE` on the same report id) whose
// reply echoes `[cmd][len][payload…]`. `steam_recv_report_id`
// (`hid-steam.c:433-500`) is the same shape.
//
// **UNVERIFIED, and the first thing to suspect if a read comes back empty:**
// what the length byte of a *request* means. `steam_get_serial` sends
// `0xae 0x15 0x01` — one payload byte behind a length of 21 — so on that
// command the byte plainly describes the *reply*, not the request.
// `docs/research/guide-hold-poweroff.md` §2/§4 prescribes `[01][89][n][ids…]`
// for `GET_SETTINGS_VALUES`, i.e. the count of ids, and that is what
// [`get_settings_request`] builds. If the firmware STALLs or answers with an
// empty payload, the reply-length reading is the other thing to try.
//
// The split below is deliberate and is what makes this testable at all: the
// request builder and the reply parser are **pure functions over bytes**, the
// round trip is generic over a [`FeatureDevice`], and the only implementation
// that touches a real descriptor is [`HidrawDevice`]. The unit tests drive a
// fake device end to end, so the read path is covered without a controller
// anywhere near it.

/// Something that can exchange feature reports: a real hidraw descriptor, or a
/// fake one in a test.
///
/// Both methods take the **whole wire frame including the leading report id**,
/// which is what the `HIDIOCSFEATURE` / `HIDIOCGFEATURE` ioctls themselves
/// take, so an implementation has nothing to reframe.
pub trait FeatureDevice {
    /// Send one feature report (`SET_FEATURE`).
    fn write_feature(&mut self, report: &[u8]) -> Result<(), String>;
    /// Read one feature report (`GET_FEATURE`) into `buf`, whose byte 0 selects
    /// the report id. Returns how many bytes the device produced.
    fn read_feature(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

/// Build a settings-query frame: `[report_id][command][n][id…]`, zero-padded to
/// [`WIRE_LEN`].
///
/// `command` is one of [`ID_GET_SETTINGS_VALUES`], [`ID_GET_SETTINGS_MAXS`] or
/// [`ID_GET_SETTINGS_DEFAULTS`] — the same frame shape answers all three, which
/// is why the caller picks. Pure: no device, no I/O.
pub fn get_settings_request(command: u8, ids: &[u8]) -> Result<[u8; WIRE_LEN], String> {
    if ids.is_empty() {
        return Err("a settings query needs at least one setting id".to_string());
    }
    if ids.len() > WIRE_LEN - 3 {
        return Err(format!(
            "too many setting ids ({}, max {}) for one {WIRE_LEN}-byte report",
            ids.len(),
            WIRE_LEN - 3
        ));
    }
    let mut buf = [0u8; WIRE_LEN];
    buf[0] = REPORT_ID_FEATURES_CONTROLLER;
    buf[1] = command;
    buf[2] = ids.len() as u8;
    buf[3..3 + ids.len()].copy_from_slice(ids);
    Ok(buf)
}

/// Parse a settings reply: `[report_id][command][len][id, value_lo, value_hi]…`,
/// where `len` is `3 * pairs`.
///
/// Strict on purpose — this is the half that tells the owner what the firmware
/// actually said, so "the reply was not an answer to this question" must not be
/// reported as "the firmware returned nothing". Errors on a reply too short to
/// hold a header, one whose command byte is not the one asked for (including
/// the all-zero buffer a device that answered nothing leaves behind), one whose
/// declared length runs past the bytes actually read, and one whose length is
/// not a whole number of `(id, u16)` triples. Pure: no device, no I/O.
pub fn parse_settings_reply(command: u8, reply: &[u8]) -> Result<Vec<(u8, u16)>, String> {
    if reply.len() < 3 {
        return Err(format!(
            "settings reply truncated: {} byte(s), need at least 3",
            reply.len()
        ));
    }
    if reply[0] != REPORT_ID_FEATURES_CONTROLLER {
        return Err(format!(
            "settings reply has report id {:#04x}, expected {:#04x}",
            reply[0], REPORT_ID_FEATURES_CONTROLLER
        ));
    }
    if reply[1] != command {
        return Err(format!(
            "settings reply is for command {:#04x}, expected {command:#04x}",
            reply[1]
        ));
    }
    let declared = reply[2] as usize;
    let available = reply.len() - 3;
    if declared > available {
        return Err(format!(
            "settings reply claims {declared} payload byte(s) but only {available} were read"
        ));
    }
    if !declared.is_multiple_of(3) {
        return Err(format!(
            "settings reply payload is {declared} byte(s), not a whole number of \
             (id, u16) triples"
        ));
    }
    Ok((0..declared / 3)
        .map(|i| {
            let at = 3 + i * 3;
            (reply[at], u16::from_le_bytes([reply[at + 1], reply[at + 2]]))
        })
        .collect())
}

/// One settings query against one device: build the request, send it, read the
/// answer, parse it.
///
/// Generic over [`FeatureDevice`] so the whole round trip — including the
/// ordering of the two halves — is exercised against a fake in the tests and
/// never needs the real controller.
pub fn read_settings<D: FeatureDevice>(
    dev: &mut D,
    command: u8,
    ids: &[u8],
) -> Result<Vec<(u8, u16)>, String> {
    let request = get_settings_request(command, ids)?;
    dev.write_feature(&request)?;
    let mut reply = [0u8; WIRE_LEN];
    reply[0] = REPORT_ID_FEATURES_CONTROLLER;
    let n = dev.read_feature(&mut reply)?;
    parse_settings_reply(command, &reply[..n.min(WIRE_LEN)])
}

/// A real hidraw descriptor as a [`FeatureDevice`].
///
/// Borrows the fd rather than owning it: the descriptors come from
/// [`crate::hidraw::ControllerSource`], which owns them for the length of the query.
pub struct HidrawDevice {
    fd: RawFd,
}

impl HidrawDevice {
    /// Wrap an open hidraw descriptor. The caller keeps it alive.
    pub fn new(fd: RawFd) -> HidrawDevice {
        HidrawDevice { fd }
    }
}

impl FeatureDevice for HidrawDevice {
    fn write_feature(&mut self, report: &[u8]) -> Result<(), String> {
        match send_feature_report(self.fd, report) {
            Ok(()) => Ok(()),
            Err(SendErr::Stall) => Err("STALL (no controller attached to this slot)".to_string()),
            // `read_controller_settings` tries every node in turn and reports the
            // first that answers, so a refusal here is just "not this node" —
            // the same shape as a stall, and it needs the same wording rather
            // than a bare errno the user cannot act on.
            Err(SendErr::Refused(e)) => Err(format!("{e} (this node does not serve settings)")),
            Err(SendErr::Other(e)) => Err(e),
        }
    }

    fn read_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let request = hidiocgfeature(buf.len());
        // SAFETY: `self.fd` is a live hidraw fd for the duration of the call and
        // `HIDIOCGFEATURE(len)` writes at most `buf.len()` bytes through the
        // pointer, which is a unique mutable borrow of exactly that many bytes.
        let ret = unsafe { libc::ioctl(self.fd, request, buf.as_mut_ptr()) };
        if ret < 0 {
            return Err(format!("HIDIOCGFEATURE: {}", std::io::Error::last_os_error()));
        }
        Ok(ret as usize)
    }
}

/// What the firmware says about one setting: its current value, and the maximum
/// and factory default it reports for it.
///
/// A field is `None` when that query failed or the firmware left the setting out
/// of its answer — which is itself the interesting result for setting 25, since
/// "not stored at all" is one of the live hypotheses
/// (`docs/research/guide-hold-poweroff.md` §3.A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingReport {
    /// The `SETTING_*` number.
    pub id: u8,
    /// Its SDL name, for printing.
    pub name: &'static str,
    /// `0x89 ID_GET_SETTINGS_VALUES`.
    pub current: Option<u16>,
    /// `0x8B ID_GET_SETTINGS_MAXS`.
    pub max: Option<u16>,
    /// `0x8C ID_GET_SETTINGS_DEFAULTS`.
    pub default: Option<u16>,
}

/// Assemble the current/max/default table for `ids` from three replies.
///
/// Pure, so the "the firmware left this one out" case is unit-tested without a
/// device: each argument is whatever [`read_settings`] returned for that command
/// (an empty slice for a query that failed outright).
fn settings_table(
    ids: &[u8],
    current: &[(u8, u16)],
    maxs: &[(u8, u16)],
    defaults: &[(u8, u16)],
) -> Vec<SettingReport> {
    let find = |pairs: &[(u8, u16)], id: u8| pairs.iter().find(|(i, _)| *i == id).map(|(_, v)| *v);
    ids.iter()
        .map(|&id| SettingReport {
            id,
            name: crate::uhid::settings::setting_name(id),
            current: find(current, id),
            max: find(maxs, id),
            default: find(defaults, id),
        })
        .collect()
}

/// Ask the attached controller for the current value, maximum and default of
/// each setting in `ids`.
///
/// Goes through [`crate::hidraw::ControllerSource::acquire`], exactly as the daemon
/// and `hyprpad monitor` do, so it works whether the nodes are opened directly
/// or handed over by the root broker. Only *one* node has the controller behind
/// it — one of the puck's five pairing slots, or the single Bluetooth node —
/// so this tries each in turn and returns the first that
/// answers the current-values query; the max and default queries then go to the
/// same node, and a failure of either leaves those columns empty rather than
/// failing the whole read.
///
/// Read-only: the only thing it sends is a query. Nothing here can change the
/// controller's state.
pub fn read_controller_settings(ids: &[u8]) -> Result<Vec<SettingReport>, String> {
    if ids.is_empty() {
        return Err("no setting ids given".to_string());
    }
    let opened = crate::hidraw::ControllerSource::acquire()
        .ok_or_else(|| {
            "no Steam Controller found (28de:1304 puck or 28de:1303 Bluetooth)".to_string()
        })?
        .into_open();
    if opened.is_empty() {
        return Err("no Steam Controller node could be opened".to_string());
    }
    let mut errors = Vec::new();
    for (node, fd) in &opened {
        let mut dev = HidrawDevice::new(fd.as_raw_fd());
        match read_settings(&mut dev, ID_GET_SETTINGS_VALUES, ids) {
            Ok(current) => {
                let maxs = read_settings(&mut dev, ID_GET_SETTINGS_MAXS, ids).unwrap_or_default();
                let defaults =
                    read_settings(&mut dev, ID_GET_SETTINGS_DEFAULTS, ids).unwrap_or_default();
                return Ok(settings_table(ids, &current, &maxs, &defaults));
            }
            Err(e) => errors.push(format!("{}: {e}", node.display())),
        }
    }
    Err(format!(
        "no controller node answered the settings query (the controller is probably asleep \
         — press the Steam button and try again): {}",
        errors.join("; ")
    ))
}

// ---------------------------------------------------------------------------
// What the exit paths do about lizard mode — `docs/12-lizard-free.md` step 1
// ---------------------------------------------------------------------------

/// What an exit does about lizard mode. Two states, decided by one config knob,
/// and named so the choice can be tested without a controller in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    /// Hand the controller back to its firmware: re-enable the keyboard/mouse
    /// emulation so a stopped daemon never leaves the controller inert. The
    /// default, and every release before the knob existed.
    Restore,
    /// Leave lizard mode **disabled** — the lizard-free boot. The controller stays
    /// silent on the desktop between daemon restarts, which is the point: no
    /// firmware arrow keys typing into whatever has focus while nothing owns
    /// the controller. The cost is that a crashed or stopped daemon leaves a
    /// controller that does nothing at all until hyprpad runs again, which is why
    /// this pairs with a user unit that restarts it
    /// (`packaging/systemd/user/hyprpad.service`).
    LeaveDisabled,
}

impl ExitAction {
    /// The decision, from the knob alone — pure, so both branches are a test
    /// rather than a thing you can only find out by killing the daemon.
    ///
    /// `restore_on_exit` is `Config::restore_lizard_on_exit`, default `true`.
    pub fn decide(restore_on_exit: bool) -> ExitAction {
        if restore_on_exit {
            ExitAction::Restore
        } else {
            ExitAction::LeaveDisabled
        }
    }

    /// Whether this action writes anything to the controller at all.
    pub fn writes_to_the_controller(self) -> bool {
        matches!(self, ExitAction::Restore)
    }
}

/// Whether a clean exit hands lizard mode back to the firmware. `true` is the
/// historical behaviour and the default; [`set_restore_on_exit`] is how
/// `[daemon] restore_lizard_on_exit` reaches the exit paths, which run from a
/// signal-waiter thread and a `Drop` guard and so cannot be handed a `Config`.
static RESTORE_ON_EXIT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Install the exit policy. Called on startup and on every live reload, exactly
/// like [`set_power_settings`].
pub fn set_restore_on_exit(restore: bool) {
    RESTORE_ON_EXIT.store(restore, std::sync::atomic::Ordering::Relaxed);
}

/// The exit policy as it stands now.
pub fn restore_on_exit() -> bool {
    RESTORE_ON_EXIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Best-effort lizard handling for the exit paths: do whatever
/// [`ExitAction::decide`] says and log the outcome (never propagate the error —
/// the process is on its way out).
pub fn restore_lizard_on_exit() {
    match ExitAction::decide(restore_on_exit()) {
        ExitAction::Restore => match enable_lizard_mode() {
            Ok(()) => eprintln!(
                "hyprpad: lizard mode re-enabled on exit (firmware kbd/mouse restored)"
            ),
            Err(e) => eprintln!("hyprpad: best-effort lizard restore on exit skipped: {e}"),
        },
        ExitAction::LeaveDisabled => eprintln!(
            "hyprpad: leaving lizard mode DISABLED on exit \
             (restore_lizard_on_exit = false) — the controller does nothing on the \
             desktop until hyprpad runs again"
        ),
    }
}

/// The ownership loop's doorbell: `(re-send wanted, condvar)`.
///
/// [`nudge`] rings it and [`own_lizard_loop`] waits on it instead of sleeping,
/// so a setting the daemon just changed reaches the controller in milliseconds
/// rather than at the next 30 s tick — **without a second thread ever touching
/// the controller**. That is the whole point: `docs/research/uhid-steam-controller.md`
/// §5.2 requires one writer, and this keeps the feature-report writer singular
/// while still being responsive to Steam.
static RESEND_NOW: (std::sync::Mutex<bool>, std::sync::Condvar) =
    (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Ask the ownership loop to re-send the settings frame now.
///
/// Cheap, non-blocking and safe to call from the daemon's frame loop — it takes
/// an uncontended lock and signals. It never does I/O itself, which is what
/// keeps a 12-retry `EPIPE` budget on a sleeping controller (up to ~240 ms per
/// node) off the 250 Hz path.
pub fn nudge() {
    let (lock, cv) = &RESEND_NOW;
    if let Ok(mut wanted) = lock.lock() {
        *wanted = true;
        cv.notify_all();
    }
}

/// Block until [`nudge`] rings or `timeout` elapses, and clear the bell.
fn wait_for_nudge(timeout: Duration) {
    let (lock, cv) = &RESEND_NOW;
    let Ok(wanted) = lock.lock() else { return };
    // A nudge that arrived while the loop was mid-send is still pending here,
    // so this returns immediately and the send it asked for happens next —
    // never dropped on the floor.
    let Ok((mut wanted, _)) = cv.wait_timeout_while(wanted, timeout, |w| !*w) else { return };
    *wanted = false;
}

/// Run lizard-mode ownership forever: disable it now, then re-send on a timer to
/// re-cover the controller across power-cycles/reconnects
/// ([`RESEND_INTERVAL`]), or immediately whenever [`nudge`] rings. Degrades
/// gracefully — a failed write logs a warning and the loop keeps trying rather
/// than taking the daemon down.
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
                    eprintln!("hyprpad: lizard mode disabled on the controller (owning it)");
                    let power = power_settings();
                    if !power.is_empty() {
                        // Name the values: they are guesses whose units are
                        // unverified, so the log is where the owner checks that
                        // what they meant is what went out.
                        let pairs: Vec<String> = power
                            .pairs()
                            .iter()
                            .map(|(id, v)| {
                                format!("{}={v}", crate::uhid::settings::setting_name(*id))
                            })
                            .collect();
                        eprintln!(
                            "hyprpad: firmware power settings written with it: {} \
                             (units UNVERIFIED — see `hyprpad controller-settings 25 50`)",
                            pairs.join(", ")
                        );
                    }
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
        wait_for_nudge(RESEND_INTERVAL);
    }
}

/// RAII guard that settles lizard mode when it drops — restoring it, or
/// deliberately leaving it disabled, per [`ExitAction::decide`]. Owning one for
/// the life of [`crate::run::run`] covers the *normal* exit paths: a clean
/// return and a panic unwinding out of the daemon both run this destructor.
/// Signal-driven exit (Ctrl-C / SIGTERM) does **not** unwind, so it is handled
/// separately by [`install_signal_restore`], whose waiter calls
/// `std::process::exit` before this guard could run — the two paths never both
/// fire.
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

/// Install SIGINT/SIGTERM handling that settles lizard mode before the process
/// dies — restoring it, or leaving it disabled, per [`ExitAction::decide`].
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
            // What happens to lizard mode is the knob's call, not this line's,
            // so say only what is certain here and let the exit routine report
            // which of the two things it actually did.
            eprintln!("hyprpad: caught signal {sig}, settling lizard mode before exit");
        }
        restore_lizard_on_exit();
        std::process::exit(128 + sig);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- what an exit does about lizard mode (docs/12-lizard-free.md) -------

    /// The default, and every release before the knob existed: a clean exit
    /// hands the firmware keyboard/mouse back, so a stopped daemon never leaves
    /// the controller inert.
    #[test]
    fn the_default_exit_restores_lizard_mode() {
        assert_eq!(ExitAction::decide(true), ExitAction::Restore);
        assert!(ExitAction::decide(true).writes_to_the_controller());
    }

    /// The lizard-free boot: exit leaves lizard mode DISABLED, so the controller never
    /// types into the desktop between daemon restarts. The cost — a stopped
    /// daemon means a controller that does nothing — is the trade the knob exists to
    /// let an owner make, and it is why this pairs with a restarting user unit.
    #[test]
    fn the_lizard_free_exit_leaves_it_disabled_and_touches_nothing() {
        assert_eq!(ExitAction::decide(false), ExitAction::LeaveDisabled);
        assert!(
            !ExitAction::decide(false).writes_to_the_controller(),
            "the lizard-free exit must not write to the controller at all"
        );
    }

    /// The knob is the only input to the decision, and it round-trips through
    /// the cell the exit paths actually read — the signal waiter and the `Drop`
    /// guard cannot be handed a `Config`, so this cell is the whole contract.
    /// (The *default* is a config-layer fact, tested there; what this pins is
    /// that setting the cell is what the exit paths then see.)
    #[test]
    fn the_exit_policy_cell_round_trips_the_knob() {
        for wanted in [false, true, false, true] {
            set_restore_on_exit(wanted);
            assert_eq!(restore_on_exit(), wanted);
            assert_eq!(
                ExitAction::decide(restore_on_exit()),
                if wanted { ExitAction::Restore } else { ExitAction::LeaveDisabled }
            );
        }
        // Leave the process-wide cell as the rest of the suite expects it.
        set_restore_on_exit(true);
    }

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
        let r = disable_lizard_settings_report(PowerSettings::default(), ImuHold::default());
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
        let seq = disable_sequence(PowerSettings::default(), ImuHold::default());
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0][1], 0x81); // clear digital mappings first
        assert_eq!(seq[1][1], 0x87); // then set settings values
    }

    // --- the power settings ------------------------------------------------

    #[test]
    fn the_settings_frame_is_unchanged_when_no_power_knob_is_set() {
        // The default must be *today's* behaviour, byte for byte: an owner who
        // does not ask for the power knobs must not get a different write.
        let plain = disable_lizard_settings_report(PowerSettings::default(), ImuHold::default());
        assert_eq!(plain[2], 6, "payload is still two pairs");
        assert!(plain[9..].iter().all(|&b| b == 0));
        assert!(PowerSettings::default().is_empty());
        assert!(PowerSettings::default().pairs().is_empty());
    }

    #[test]
    fn the_settings_frame_carries_setting_25_when_the_knob_is_set() {
        let r = disable_lizard_settings_report(
                PowerSettings {
                steam_button_poweroff: Some(0x1234),
            sleep_inactivity_timeout: None,
                },
                ImuHold::default(),
            );
        // Three pairs now: lizard, watchdog, then 25 = 0x1234 little-endian.
        assert_eq!(r[2], 9);
        assert_eq!(&r[3..12], &[0x09, 0, 0, 0x47, 0, 0, 25, 0x34, 0x12]);
        assert!(r[12..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn the_settings_frame_carries_both_power_settings_in_id_order() {
        let r = disable_lizard_settings_report(
                PowerSettings {
                steam_button_poweroff: Some(POWER_SETTING_OFF),
            sleep_inactivity_timeout: Some(600),
                },
                ImuHold::default(),
            );
        assert_eq!(r[2], 12, "four pairs");
        assert_eq!(
            &r[3..15],
            &[0x09, 0, 0, 0x47, 0, 0, 25, 0xFF, 0xFF, 50, 0x58, 0x02]
        );
        assert!(r[15..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    #[test]
    fn the_sleep_timeout_alone_is_the_only_extra_pair() {
        let r = disable_lizard_settings_report(
                PowerSettings {
                steam_button_poweroff: None,
            sleep_inactivity_timeout: Some(1),
                },
                ImuHold::default(),
            );
        assert_eq!(r[2], 9);
        assert_eq!(&r[9..12], &[50, 0x01, 0x00]);
    }

    #[test]
    fn off_writes_the_widest_value_the_field_can_hold_not_zero() {
        // The documented reason: `0` might mean "no delay" rather than "never",
        // and a poweroff time of zero would fire on every guide press.
        assert_eq!(POWER_SETTING_OFF, 0xFFFF);
    }

    #[test]
    fn the_power_setting_numbers_are_the_shared_ones() {
        // Not respelled here: they come from the module that decodes Steam
        // writing the same settings to the virtual controller.
        assert_eq!(SETTING_STEAMBUTTON_POWEROFF_TIME, 25);
        assert_eq!(SETTING_SLEEP_INACTIVITY_TIMEOUT, 50);
        assert_eq!(ID_TURN_OFF_CONTROLLER, 0x9F);
    }

    #[test]
    fn the_enable_frame_never_carries_power_settings() {
        // Restore-on-exit must not write a guess at settings whose defaults we
        // do not know: it is the two-pair frame it always was.
        let en = enable_lizard_settings_report(ImuHold::default());
        assert_eq!(&en[..9], &[0x01, 0x87, 0x06, 0x09, 0x01, 0x00, 0x47, 0x01, 0x00]);
        assert!(en[9..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_settings_report_drops_pairs_that_would_not_fit() {
        let many: Vec<(u8, u16)> = (0..40u8).map(|i| (i, u16::from(i))).collect();
        let r = settings_report(&many);
        assert_eq!(r[2] as usize, 3 * MAX_SETTINGS_PAIRS);
        // The declared payload length is what the buffer actually holds, so a
        // caller cannot make the frame lie about itself.
        assert_eq!(3 + r[2] as usize, r.len() - (WIRE_LEN - 3 - 3 * MAX_SETTINGS_PAIRS));
    }

    // --- the power-off command ---------------------------------------------

    #[test]
    fn turn_off_report_bytes() {
        let r = turn_off_report();
        assert_eq!(r.len(), 64);
        // sc-controller's `9f 04 6f 66 66 21`, behind the report id.
        assert_eq!(&r[..7], &[0x01, 0x9F, 0x04, 0x6F, 0x66, 0x66, 0x21]);
        assert_eq!(&r[3..7], b"off!");
        assert!(r[7..].iter().all(|&b| b == 0), "rest must be zero-padded");
    }

    // --- the read round trip, against a fake device -------------------------

    /// A [`FeatureDevice`] that records what was written and replays a canned
    /// reply. No hidraw node is opened anywhere in these tests.
    struct FakeDevice {
        written: Vec<Vec<u8>>,
        reply: Vec<u8>,
        read_err: Option<String>,
    }

    impl FakeDevice {
        fn answering(reply: Vec<u8>) -> FakeDevice {
            FakeDevice { written: Vec::new(), reply, read_err: None }
        }
        /// A well-formed reply frame for `command` carrying `pairs`.
        fn reply_frame(command: u8, pairs: &[(u8, u16)]) -> Vec<u8> {
            let mut v = vec![REPORT_ID_FEATURES_CONTROLLER, command, (3 * pairs.len()) as u8];
            for (id, value) in pairs {
                v.push(*id);
                v.extend_from_slice(&value.to_le_bytes());
            }
            v.resize(WIRE_LEN, 0);
            v
        }
    }

    impl FeatureDevice for FakeDevice {
        fn write_feature(&mut self, report: &[u8]) -> Result<(), String> {
            self.written.push(report.to_vec());
            Ok(())
        }
        fn read_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            if let Some(e) = &self.read_err {
                return Err(e.clone());
            }
            let n = self.reply.len().min(buf.len());
            buf[..n].copy_from_slice(&self.reply[..n]);
            Ok(n)
        }
    }

    #[test]
    fn a_settings_query_is_report_id_command_count_then_ids() {
        let r = get_settings_request(ID_GET_SETTINGS_VALUES, &[25, 50]).unwrap();
        assert_eq!(r.len(), 64);
        assert_eq!(&r[..5], &[0x01, 0x89, 0x02, 25, 50]);
        assert!(r[5..].iter().all(|&b| b == 0), "rest must be zero-padded");
        // The maxes and defaults queries are the same frame with another verb.
        assert_eq!(get_settings_request(ID_GET_SETTINGS_MAXS, &[25]).unwrap()[1], 0x8B);
        assert_eq!(get_settings_request(ID_GET_SETTINGS_DEFAULTS, &[25]).unwrap()[1], 0x8C);
    }

    #[test]
    fn a_settings_query_needs_at_least_one_id_and_fits_the_report() {
        assert!(get_settings_request(ID_GET_SETTINGS_VALUES, &[]).is_err());
        let too_many: Vec<u8> = (0..=200u8).collect();
        assert!(get_settings_request(ID_GET_SETTINGS_VALUES, &too_many).is_err());
    }

    #[test]
    fn a_reply_parses_back_into_pairs() {
        let frame = FakeDevice::reply_frame(ID_GET_SETTINGS_VALUES, &[(25, 300), (50, 600)]);
        assert_eq!(
            parse_settings_reply(ID_GET_SETTINGS_VALUES, &frame).unwrap(),
            vec![(25, 300), (50, 600)]
        );
    }

    #[test]
    fn a_short_or_garbled_reply_is_an_error_not_an_empty_answer() {
        // Too short to hold a header.
        assert!(parse_settings_reply(ID_GET_SETTINGS_VALUES, &[0x01, 0x89]).is_err());
        // The device answered nothing: an all-zero buffer is not an answer.
        assert!(parse_settings_reply(ID_GET_SETTINGS_VALUES, &[0u8; WIRE_LEN]).is_err());
        // An answer to a different question.
        let other = FakeDevice::reply_frame(ID_GET_SETTINGS_MAXS, &[(25, 1)]);
        let e = parse_settings_reply(ID_GET_SETTINGS_VALUES, &other).unwrap_err();
        assert!(e.contains("0x8b"), "{e}");
        // A wrong report id.
        let mut wrong_id = FakeDevice::reply_frame(ID_GET_SETTINGS_VALUES, &[(25, 1)]);
        wrong_id[0] = 0x02;
        assert!(parse_settings_reply(ID_GET_SETTINGS_VALUES, &wrong_id).is_err());
        // A length that runs past what was read.
        let truncated = [0x01, 0x89, 6, 25, 0, 0];
        let e = parse_settings_reply(ID_GET_SETTINGS_VALUES, &truncated).unwrap_err();
        assert!(e.contains("only 3"), "{e}");
        // A length that is not a whole number of triples.
        let ragged = [0x01, 0x89, 4, 25, 0, 0, 50];
        assert!(parse_settings_reply(ID_GET_SETTINGS_VALUES, &ragged).is_err());
    }

    #[test]
    fn the_read_round_trip_sends_the_query_then_parses_the_answer() {
        let mut dev =
            FakeDevice::answering(FakeDevice::reply_frame(ID_GET_SETTINGS_VALUES, &[(25, 5000)]));
        let got = read_settings(&mut dev, ID_GET_SETTINGS_VALUES, &[25]).unwrap();
        assert_eq!(got, vec![(25, 5000)]);
        assert_eq!(dev.written.len(), 1, "exactly one query went out");
        assert_eq!(&dev.written[0][..4], &[0x01, 0x89, 0x01, 25]);
        assert_eq!(dev.written[0].len(), WIRE_LEN);
    }

    #[test]
    fn a_read_that_fails_propagates_rather_than_reporting_no_settings() {
        let mut dev = FakeDevice::answering(Vec::new());
        dev.read_err = Some("HIDIOCGFEATURE: Broken pipe".to_string());
        let e = read_settings(&mut dev, ID_GET_SETTINGS_VALUES, &[25]).unwrap_err();
        assert!(e.contains("Broken pipe"), "{e}");
    }

    #[test]
    fn the_settings_table_leaves_a_missing_answer_empty() {
        let t = settings_table(&[25, 50], &[(25, 300)], &[(25, 65535), (50, 65535)], &[]);
        assert_eq!(t[0].id, 25);
        assert_eq!(t[0].name, "SETTING_STEAMBUTTON_POWEROFF_TIME");
        assert_eq!((t[0].current, t[0].max, t[0].default), (Some(300), Some(65535), None));
        // 50 was not in the current-values answer at all: that is a real result,
        // not a zero.
        assert_eq!((t[1].current, t[1].max, t[1].default), (None, Some(65535), None));
    }

    #[test]
    fn hidiocgfeature_encoding() {
        // _IOC(WRITE|READ, 'H', 0x07, 64) — the read twin of HIDIOCSFEATURE.
        assert_eq!(hidiocgfeature(WIRE_LEN), 0xC040_4807);
        assert_ne!(hidiocgfeature(WIRE_LEN), hidiocsfeature(WIRE_LEN));
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
        let r = enable_lizard_settings_report(ImuHold::default());
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
        let seq = enable_sequence(ImuHold::default());
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0][1], 0x85); // restore default digital mappings first
        assert_eq!(seq[1][1], 0x87); // then set settings values
    }

    #[test]
    fn enable_exactly_inverts_disable_settings() {
        // Same command, same settings, same order — only the values flip 0 <-> 1.
        let dis = disable_lizard_settings_report(PowerSettings::default(), ImuHold::default());
        let en = enable_lizard_settings_report(ImuHold::default());
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

    // -----------------------------------------------------------------------
    // The gyro hold (gap G5) — pure frame tests, no device anywhere
    // -----------------------------------------------------------------------

    use crate::uhid::settings::gyro_mode;

    /// A hold as the daemon would leave it after Steam asked for sensors.
    fn steam_asked(mode: u16) -> ImuHold {
        ImuHold { preference: gyro_mode::OFF, steam: Some(mode), engaged: true }
    }

    /// **The invariant that made this safe to ship.** With nothing asking for
    /// the gyro — which is every `kind = "xbox"` install, the default — the
    /// settings frame is byte-for-byte the two-pair frame this module has
    /// always sent. A feature nobody uses writes nothing to anybody's
    /// controller.
    #[test]
    fn an_unengaged_hold_leaves_the_historical_frame_byte_for_byte_unchanged() {
        let before =
            settings_report(&[(SETTING_LIZARD_MODE, 0), (SETTING_STEAM_WATCHDOG_ENABLE, 0)]);
        let now = disable_lizard_settings_report(PowerSettings::default(), ImuHold::default());
        assert_eq!(now, before);
        assert_eq!(now[2], 6, "two pairs, exactly as before");
        assert_eq!(ImuHold::default().pair(), None);
        assert_eq!(ImuHold::default().effective(), None);
    }

    /// Steam's IMU-enable reaching lizard's frame builder: the one wire fact
    /// the whole gyro path reduces to.
    ///
    /// `[01][87][len][9,0,0][71,0,0][48,0x18,0x00]` — the pair rides in the
    /// *same* frame as the two lizard settings, which is what makes it survive
    /// the 30 s re-send, a reconnect and a power cycle with no second timer.
    #[test]
    fn steams_imu_enable_rides_out_in_lizards_own_settings_frame() {
        let r = disable_lizard_settings_report(
            PowerSettings::default(),
            steam_asked(gyro_mode::SENSORS_ON),
        );
        assert_eq!(r[0], REPORT_ID_FEATURES_CONTROLLER);
        assert_eq!(r[1], ID_SET_SETTINGS_VALUES);
        assert_eq!(r[2], 9, "three pairs");
        assert_eq!(
            &r[3..12],
            &[
                SETTING_LIZARD_MODE, 0, 0, //
                SETTING_STEAM_WATCHDOG_ENABLE, 0, 0, //
                SETTING_IMU_MODE, 0x18, 0x00, // SEND_RAW_ACCEL | SEND_RAW_GYRO
            ]
        );
        assert!(r[12..].iter().all(|&b| b == 0), "rest must be zero-padded");
        assert_eq!(r.len(), WIRE_LEN, "still one 64-byte feature report");
    }

    /// Turning it back off is the same frame with a zero value — not the
    /// absence of the pair, which would leave the firmware holding whatever it
    /// was last told.
    #[test]
    fn the_imu_off_write_is_an_explicit_zero_not_a_missing_pair() {
        let off =
            disable_lizard_settings_report(PowerSettings::default(), steam_asked(gyro_mode::OFF));
        assert_eq!(off[2], 9, "the pair is still there");
        assert_eq!(&off[9..12], &[SETTING_IMU_MODE, 0x00, 0x00]);

        let on = disable_lizard_settings_report(
            PowerSettings::default(),
            steam_asked(gyro_mode::SENSORS_ON),
        );
        assert_ne!(on, off, "and the two frames differ");
    }

    /// The gyro pair comes last, after the power knobs, so a config that sets
    /// everything still produces one well-formed frame in setting order.
    #[test]
    fn the_gyro_pair_rides_behind_the_power_knobs_in_one_frame() {
        let r = disable_lizard_settings_report(
            PowerSettings {
                steam_button_poweroff: Some(POWER_SETTING_OFF),
                sleep_inactivity_timeout: Some(600),
            },
            steam_asked(gyro_mode::SENSORS_ON),
        );
        assert_eq!(r[2], 15, "five pairs");
        assert_eq!(
            &r[3..18],
            &[
                SETTING_LIZARD_MODE, 0, 0, //
                SETTING_STEAM_WATCHDOG_ENABLE, 0, 0, //
                SETTING_STEAMBUTTON_POWEROFF_TIME, 0xFF, 0xFF, //
                SETTING_SLEEP_INACTIVITY_TIMEOUT, 0x58, 0x02, //
                SETTING_IMU_MODE, 0x18, 0x00,
            ]
        );
        assert!(r[18..].iter().all(|&b| b == 0));
    }

    /// Steam wins over hyprpad's preference while it is asking, and hyprpad's
    /// preference is what is left when it stops — the whole authority model, in
    /// the four states it has.
    #[test]
    fn steam_outranks_the_preference_only_while_it_is_asking() {
        let off_pref = gyro_mode::OFF;
        let on_pref = gyro_mode::SENSORS_ON;

        // Nothing asked at all: no pair.
        let idle = ImuHold { preference: off_pref, steam: None, engaged: false };
        assert_eq!(idle.effective(), None);

        // Steam asked, hyprpad prefers off: Steam wins.
        let asked = ImuHold { preference: off_pref, steam: Some(on_pref), engaged: true };
        assert_eq!(asked.effective(), Some(on_pref));

        // Steam let go: back to hyprpad's preference, explicitly written.
        let released = ImuHold { steam: None, ..asked };
        assert_eq!(released.effective(), Some(off_pref));

        // With `gyro = true` the preference is on, so letting go changes
        // nothing — the gyro stays up.
        let held = ImuHold { preference: on_pref, steam: None, engaged: true };
        assert_eq!(held.effective(), Some(on_pref));
    }

    /// The exit path restores hyprpad's preference, discarding Steam's request
    /// — Steam is losing the device, so its opinion stops counting. Without
    /// this a session that ended with a gyro game open would leave the controller
    /// streaming IMU data to nobody.
    #[test]
    fn the_exit_frame_restores_the_preference_and_forgets_what_steam_asked() {
        // Steam had it on; hyprpad's own preference is off.
        let r = enable_lizard_settings_report(steam_asked(gyro_mode::SENSORS_ON));
        assert_eq!(r[1], ID_SET_SETTINGS_VALUES);
        assert_eq!(r[2], 9, "lizard on, watchdog on, gyro back to the preference");
        assert_eq!(
            &r[3..12],
            &[
                SETTING_LIZARD_MODE, 1, 0, //
                SETTING_STEAM_WATCHDOG_ENABLE, 1, 0, //
                SETTING_IMU_MODE, 0x00, 0x00, // the preference: off
            ]
        );

        // With `gyro = true` the exit frame holds it on instead.
        let kept = enable_lizard_settings_report(ImuHold {
            preference: gyro_mode::SENSORS_ON,
            steam: Some(gyro_mode::OFF),
            engaged: true,
        });
        assert_eq!(&kept[9..12], &[SETTING_IMU_MODE, 0x18, 0x00]);

        // Nothing ever asked: the exit frame is the historical two-pair one.
        let untouched = enable_lizard_settings_report(ImuHold::default());
        assert_eq!(untouched[2], 6);
        assert_eq!(
            &untouched[3..9],
            &[SETTING_LIZARD_MODE, 1, 0, SETTING_STEAM_WATCHDOG_ENABLE, 1, 0]
        );
    }

    /// `set_imu_requested` reports whether anything actually changed, which is
    /// what keeps Steam's repeated settings writes from becoming a feature
    /// report each. The process-wide cell is exercised end to end here, so the
    /// engage-once rule and the restore are both covered.
    ///
    /// Serialised by hand into one test because the cell is a static: two
    /// `#[test]`s touching it would race under the default threaded harness.
    #[test]
    fn the_shared_hold_engages_once_and_only_reports_real_changes() {
        set_imu_preference(gyro_mode::OFF);
        assert_eq!(imu_hold().effective(), None, "an off preference does not engage");

        assert!(set_imu_requested(Some(gyro_mode::SENSORS_ON)), "first ask is a change");
        assert_eq!(imu_hold().effective(), Some(gyro_mode::SENSORS_ON));
        assert!(
            !set_imu_requested(Some(gyro_mode::SENSORS_ON)),
            "Steam re-stating the same mode must not write again"
        );

        assert!(set_imu_requested(None), "letting go restores the preference");
        assert_eq!(imu_hold().effective(), Some(gyro_mode::OFF), "explicitly off, not absent");
        assert!(!set_imu_requested(None), "and letting go twice is not a change");

        // `gyro = true` engages on its own, with no Steam in the picture.
        set_imu_preference(gyro_mode::SENSORS_ON);
        assert_eq!(imu_hold().effective(), Some(gyro_mode::SENSORS_ON));

        // Leave the cell as the rest of the suite found it.
        set_imu_preference(gyro_mode::OFF);
        set_imu_requested(None);
    }

    /// The doorbell is what lets the daemon's 250 Hz loop ask for an immediate
    /// write without ever doing one itself: a nudge that arrives before the
    /// wait is not dropped, and one that arrives during it wakes the wait.
    #[test]
    fn a_nudge_wakes_the_wait_and_is_never_dropped() {
        let long = Duration::from_secs(30);

        // Rung before the wait: returns at once, well inside the 30 s timeout.
        nudge();
        let t0 = std::time::Instant::now();
        wait_for_nudge(long);
        assert!(t0.elapsed() < Duration::from_secs(1), "a pending nudge is not lost");

        // Rung from another thread while the wait is blocked.
        let t1 = std::time::Instant::now();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(20));
            nudge();
        });
        wait_for_nudge(long);
        assert!(t1.elapsed() < Duration::from_secs(5), "a nudge wakes a blocked wait");

        // And the bell is cleared, so the next wait actually waits.
        let t2 = std::time::Instant::now();
        wait_for_nudge(Duration::from_millis(30));
        assert!(t2.elapsed() >= Duration::from_millis(25), "the bell was cleared");
    }
}
