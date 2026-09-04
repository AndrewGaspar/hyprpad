//! What Steam writes *back* to the virtual controller, and what hyprpad does
//! about it.
//!
//! The proven run recorded **39 `SetSettingsValues` (`0x87`) writes** from Steam
//! within two minutes of adoption — Steam configuring the device it believes is
//! a Valve controller. This module decodes those, and the rumble and haptic
//! commands alongside them, into a small set of [`Action`]s the daemon can act
//! on. It is pure: no device, no clock, no I/O.
//!
//! # The policy, and why it is not "relay everything"
//!
//! `docs/research/uhid-steam-controller.md` §4.3 sketches a relay policy —
//! forward Steam's feature writes straight to the real controller — and §5.1/§5.2 then
//! spend two sections on the arbitration that would require. This build takes
//! the other, smaller road, the one InputPlumber's proven implementation takes
//! (A.4): **decode the ones hyprpad can act on, acknowledge the rest.** Two
//! reasons, both structural:
//!
//! 1. **hyprpad already owns lizard mode.** `src/lizard.rs` sends
//!    `ID_CLEAR_DIGITAL_MAPPINGS` and `ID_SET_SETTINGS_VALUES` with
//!    `SETTING_LIZARD_MODE = 0` *and* `SETTING_STEAM_WATCHDOG_ENABLE = 0`, and
//!    re-sends every 30 s. Steam's `0x87` writes ask for the same thing, so
//!    forwarding them would be redundant at best and a write race at worst
//!    (§5.1 warns about exactly this against `restore_lizard_on_exit`).
//!    They are therefore **acknowledged and ignored** — Steam's heartbeat is
//!    already satisfied by the state hyprpad maintains.
//! 2. **hyprpad already owns the actuators, through one writer.** `haptics.rs`
//!    runs a single writer thread over the controller's only writable fd. §5.2 is
//!    explicit that the relay must go through it and "never open a second
//!    writable fd on the controller", so rumble and haptics are *translated into that
//!    path* rather than forwarded as raw bytes.
//!
//! 3. **The queries are already answered by the time the daemon sees them.** A
//!    `SET_REPORT` naming `GetAttributesValues`, `GetStringAttribute` or
//!    `GetChipId` selects what the next `GET_REPORT` reads back; `uhid.rs`
//!    stores the selector and replies from the profile's canned data. Those
//!    classify as [`Action::Answered`], not as drops.
//!
//! Everything else is dropped with a debug log.
//!
//! # The one exception: `SETTING_IMU_MODE`, and why it is relayed
//!
//! The policy above has exactly one hole, and it was gap G5 of
//! `docs/design/uhid-relay.md`: **only the real controller can turn its own IMU on.**
//! Steam's gyro request is not something hyprpad can satisfy on its own behalf
//! the way it satisfies the lizard-mode writes, and it is not an actuator
//! command that can be translated into `haptics.rs` — it is a firmware setting
//! that has to reach the hardware or the gyro bytes never appear in the controller's
//! `0x42` at all.
//!
//! So [`setting::IMU_MODE`] (48) is **relayed**, and it is the only setting that
//! is. It does not become a second writer: [`imu_mode`] lifts the value out of
//! the decoded write and the daemon hands it to `src/lizard.rs`, which folds it
//! into the *same* `0x87` frame it already builds and re-sends every 30 s. One
//! frame builder, one writer, one heartbeat — see `lizard::set_imu_requested`.
//!
//! # Sources for every byte layout below
//!
//! * command ids — `ReportType` in InputPlumber
//!   `src/drivers/steam_deck/hid_report.rs`, which matches SDL's
//!   `enum FeatureReportMessageIDs` (`controller_constants.h`) id for id.
//! * `0x87` framing — `src/lizard.rs`, which already builds one, and SDL's
//!   `DisableSteamTritonLizardMode()`.
//! * `SETTING_*` numbers — SDL `controller_constants.h`, as catalogued in
//!   `docs/research/guide-hold-poweroff.md`.
//! * `0xEB` rumble — InputPlumber's `PackedRumbleReport`, field for field.
//! * `0x81` / `0x80` — `src/haptics.rs`, which writes both to the real controller.
//! * `0x8F` — the kernel's `steam_haptic_pulse` (`drivers/hid/hid-steam.c`).

use crate::haptics::Pad;
use crate::uhid::HostWrite;

/// Valve control-protocol command ids.
///
/// The full table from InputPlumber's `ReportType`; hyprpad names them all so a
/// debug log says what Steam asked for rather than printing a bare number.
pub mod cmd {
    /// `InputData` — the input report type, and the initial GET selector.
    pub const INPUT_DATA: u8 = 0x09;
    /// `SetDigitalMappings` — write the firmware button→key/mouse mappings.
    pub const SET_DIGITAL_MAPPINGS: u8 = 0x80;
    /// `ClearDigitalMappings` — the lizard-mode disable half `lizard.rs` sends.
    pub const CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;
    /// `SetDefaultDigitalMappings` — restore the lizard mappings.
    pub const SET_DEFAULT_DIGITAL_MAPPINGS: u8 = 0x85;
    /// `SetSettingsValues` — `(setting, u16)` pairs. Steam's workhorse.
    pub const SET_SETTINGS_VALUES: u8 = 0x87;
    /// `ClearSettingsValues`.
    pub const CLEAR_SETTINGS_VALUES: u8 = 0x88;
    /// `TriggerHapticPulse` — the classic Steam Controller haptic feature.
    pub const TRIGGER_HAPTIC_PULSE: u8 = 0x8F;
    /// `TurnOffController` — the guide+Y Steam-poweroff path.
    pub const TURN_OFF_CONTROLLER: u8 = 0x9F;
    /// `TriggerHapticCommand` — the Deck-era haptic.
    pub const TRIGGER_HAPTIC_COMMAND: u8 = 0xEA;
    /// `TriggerRumbleCommand` — the Deck-era rumble.
    pub const TRIGGER_RUMBLE_COMMAND: u8 = 0xEB;
    /// The controller's `0x80` force-feedback **output** report (`haptics.rs`).
    pub const OUT_RUMBLE: u8 = 0x80;
    /// The controller's `0x81` haptic-pulse **output** report (`haptics.rs`).
    pub const OUT_PULSE: u8 = 0x81;

    /// The queries `profile::Profile::canned_reply` has a real answer for.
    ///
    /// A `SET_REPORT` naming one of these selects what the next `GET_REPORT`
    /// returns; the relay answers it and the daemon has nothing to do. Kept in
    /// the same order as `Profile::canned_reply`'s match arms, and equal to
    /// `profile::cmd`'s three ids by construction — a test pins that.
    pub const ANSWERED_QUERIES: [u8; 3] = [
        crate::uhid::profile::cmd::GET_ATTRIBUTES_VALUES,
        crate::uhid::profile::cmd::GET_STRING_ATTRIBUTE,
        crate::uhid::profile::cmd::GET_CHIP_ID,
    ];
}

/// A human name for a command id, for logging only.
///
/// Every value is InputPlumber's `ReportType` discriminant; the names are its
/// variant names, which are in turn SDL's `FeatureReportMessageIDs`.
pub fn command_name(id: u8) -> &'static str {
    match id {
        0x09 => "InputData",
        0x80 => "SetDigitalMappings",
        0x81 => "ClearDigitalMappings",
        0x82 => "GetDigitalMappings",
        0x83 => "GetAttributesValues",
        0x84 => "GetAttributesLabel",
        0x85 => "SetDefaultDigitalMappings",
        0x86 => "FactoryReset",
        0x87 => "SetSettingsValues",
        0x88 => "ClearSettingsValues",
        0x89 => "GetSettingsValues",
        0x8A => "GetSettingLabel",
        0x8B => "GetSettingsMaxs",
        0x8C => "GetSettingsDefaults",
        0x8D => "SetControllerMode",
        0x8E => "LoadDefaultSettings",
        0x8F => "TriggerHapticPulse",
        0x9F => "TurnOffController",
        0xA1 => "GetDeviceInfo",
        0xA7 => "CalibrateTrackpads",
        0xA9 => "SetSerialNumber",
        0xAA => "GetTrackpadCalibration",
        0xAB => "GetTrackpadFactoryCalibration",
        0xAC => "GetTrackpadRawData",
        0xAD => "EnablePairing",
        0xAE => "GetStringAttribute",
        0xAF => "RadioEraseRecords",
        0xB0 => "RadioWriteRecord",
        0xB1 => "SetDongleSetting",
        0xB2 => "DongleDisconnectDevice",
        0xB3 => "DongleCommitDevice",
        0xB4 => "DongleGetWirelessState",
        0xB5 => "CalibrateGyro",
        0xB6 => "PlayAudio",
        0xB7 => "AudioUpdateStart",
        0xB8 => "AudioUpdateData",
        0xB9 => "AudioUpdateComplete",
        0xBA => "GetChipId",
        0xBF => "CalibrateJoystick",
        0xC0 => "CalibrateAnalogTriggers",
        0xC1 => "SetAudioMappings",
        0xC2 => "CheckGyroFwLoad",
        0xC3 => "CalibrateAnalog",
        0xC4 => "DongleGetConnectedSlots",
        0xCE => "ResetIMU",
        0xDC => "UnknownDc",
        0xE2 => "UnknownE2",
        0xEA => "TriggerHapticCommand",
        0xEB => "TriggerRumbleCommand",
        _ => "unknown",
    }
}

/// `SETTING_*` numbers, from SDL's `controller_constants.h` — the
/// `ControllerSettings` enum, whose header says *"only add to this enum and
/// never change the order"*, which is what makes a bare index a stable name.
///
/// [`SETTING_NAMES`] carries the **whole** enum in order, so a debug log never
/// prints `SETTING_?` for anything Valve has named; the constants below are the
/// handful the daemon refers to by name, each pinned against its index in that
/// table by `every_named_constant_matches_its_slot_in_the_full_table`.
pub mod setting {
    /// `SETTING_LEFT_TRACKPAD_MODE` — one of the pair SDL sets to
    /// `TRACKPAD_NONE` (7) to stop the firmware driving a mouse.
    pub const LEFT_TRACKPAD_MODE: u8 = 7;
    /// `SETTING_RIGHT_TRACKPAD_MODE`.
    pub const RIGHT_TRACKPAD_MODE: u8 = 8;
    /// `SETTING_LIZARD_MODE` — the master firmware keyboard/mouse switch.
    /// **hyprpad's**: `src/lizard.rs` holds it at 0.
    pub const LIZARD_MODE: u8 = 9;
    /// `SETTING_SMOOTH_ABSOLUTE_MOUSE`.
    pub const SMOOTH_ABSOLUTE_MOUSE: u8 = 24;
    /// `SETTING_STEAMBUTTON_POWEROFF_TIME`.
    pub const STEAMBUTTON_POWEROFF_TIME: u8 = 25;
    /// `SETTING_IMU_MODE` — **the gyro switch**, and the reason G5 existed.
    ///
    /// A `u16` bitmask of [`super::gyro_mode`] flags. SDL's
    /// `HIDAPI_DriverSteam_SetSensorsEnabled` writes exactly this setting and
    /// nothing else when a game turns its sensors on or off
    /// (`SDL_hidapi_steam.c`), so it is the one number Steam's IMU-enable
    /// reduces to. Older headers spell it `SETTING_GYRO_MODE`, which is why the
    /// value enum still carries that name.
    pub const IMU_MODE: u8 = 48;
    /// `SETTING_WIRELESS_PACKET_VERSION`.
    pub const WIRELESS_PACKET_VERSION: u8 = 49;
    /// `SETTING_SLEEP_INACTIVITY_TIMEOUT`.
    pub const SLEEP_INACTIVITY_TIMEOUT: u8 = 50;
    /// `SETTING_LEFT_TRACKPAD_CLICK_PRESSURE` — SDL writes `0xFFFF` here to
    /// disable the clicky pad.
    pub const LEFT_TRACKPAD_CLICK_PRESSURE: u8 = 52;
    /// `SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE`.
    pub const RIGHT_TRACKPAD_CLICK_PRESSURE: u8 = 53;
    /// `SETTING_STEAM_WATCHDOG_ENABLE` — reverts the pad to lizard mode when no
    /// host heartbeat arrives. **hyprpad's**: `src/lizard.rs` holds it at 0.
    pub const STEAM_WATCHDOG_ENABLE: u8 = 71;
    /// `SETTING_DEVICE_POWER_STATUS`.
    pub const DEVICE_POWER_STATUS: u8 = 78;
}

/// Every `SETTING_*` name, indexed by its number.
///
/// Transcribed from the `ControllerSettings` enum of SDL's
/// `src/joystick/hidapi/steam/controller_constants.h` (82 entries, 0 through
/// `SETTING_TIMP_MODE_MTE`, before the `SETTING_COUNT` terminator). The enum is
/// declared append-only by its own comment, so an index is a stable identity;
/// six of these numbers are independently corroborated by the frame
/// sc-controller's `configure()` sends (`docs/research/guide-hold-poweroff.md`
/// §2: settings 50, 24, 49, 8, 7, 48, 46 in that order), which is why the table
/// can be trusted for the entries nothing in this tree writes.
///
/// It exists so `HYPRSC_DEBUG` never prints `SETTING_?` again: the first live
/// Triton session logged eight distinct unnamed ids, and an unnamed id is a
/// question nobody can answer from the log alone.
pub const SETTING_NAMES: [&str; 82] = [
    // 0
    "SETTING_MOUSE_SENSITIVITY",
    "SETTING_MOUSE_ACCELERATION",
    "SETTING_TRACKBALL_ROTATION_ANGLE",
    "SETTING_HAPTIC_INTENSITY_UNUSED",
    "SETTING_LEFT_GAMEPAD_STICK_ENABLED",
    "SETTING_RIGHT_GAMEPAD_STICK_ENABLED",
    "SETTING_USB_DEBUG_MODE",
    "SETTING_LEFT_TRACKPAD_MODE",
    "SETTING_RIGHT_TRACKPAD_MODE",
    "SETTING_LIZARD_MODE",
    // 10
    "SETTING_DPAD_DEADZONE",
    "SETTING_MINIMUM_MOMENTUM_VEL",
    "SETTING_MOMENTUM_DECAY_AMOUNT",
    "SETTING_TRACKPAD_RELATIVE_MODE_TICKS_PER_PIXEL",
    "SETTING_HAPTIC_INCREMENT",
    "SETTING_DPAD_ANGLE_SIN",
    "SETTING_DPAD_ANGLE_COS",
    "SETTING_MOMENTUM_VERTICAL_DIVISOR",
    "SETTING_MOMENTUM_MAXIMUM_VELOCITY",
    "SETTING_TRACKPAD_Z_ON",
    // 20
    "SETTING_TRACKPAD_Z_OFF",
    "SETTING_SENSITIVITY_SCALE_AMOUNT",
    "SETTING_LEFT_TRACKPAD_SECONDARY_MODE",
    "SETTING_RIGHT_TRACKPAD_SECONDARY_MODE",
    "SETTING_SMOOTH_ABSOLUTE_MOUSE",
    "SETTING_STEAMBUTTON_POWEROFF_TIME",
    "SETTING_UNUSED_1",
    "SETTING_TRACKPAD_OUTER_RADIUS",
    "SETTING_TRACKPAD_Z_ON_LEFT",
    "SETTING_TRACKPAD_Z_OFF_LEFT",
    // 30
    "SETTING_TRACKPAD_OUTER_SPIN_VEL",
    "SETTING_TRACKPAD_OUTER_SPIN_RADIUS",
    "SETTING_TRACKPAD_OUTER_SPIN_HORIZONTAL_ONLY",
    "SETTING_TRACKPAD_RELATIVE_MODE_DEADZONE",
    "SETTING_TRACKPAD_RELATIVE_MODE_MAX_VEL",
    "SETTING_TRACKPAD_RELATIVE_MODE_INVERT_Y",
    "SETTING_TRACKPAD_DOUBLE_TAP_BEEP_ENABLED",
    "SETTING_TRACKPAD_DOUBLE_TAP_BEEP_PERIOD",
    "SETTING_TRACKPAD_DOUBLE_TAP_BEEP_COUNT",
    "SETTING_TRACKPAD_OUTER_RADIUS_RELEASE_ON_TRANSITION",
    // 40
    "SETTING_RADIAL_MODE_ANGLE",
    "SETTING_HAPTIC_INTENSITY_MOUSE_MODE",
    "SETTING_LEFT_DPAD_REQUIRES_CLICK",
    "SETTING_RIGHT_DPAD_REQUIRES_CLICK",
    "SETTING_LED_BASELINE_BRIGHTNESS",
    "SETTING_LED_USER_BRIGHTNESS",
    "SETTING_ENABLE_RAW_JOYSTICK",
    "SETTING_ENABLE_FAST_SCAN",
    "SETTING_IMU_MODE",
    "SETTING_WIRELESS_PACKET_VERSION",
    // 50
    "SETTING_SLEEP_INACTIVITY_TIMEOUT",
    "SETTING_TRACKPAD_NOISE_THRESHOLD",
    "SETTING_LEFT_TRACKPAD_CLICK_PRESSURE",
    "SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE",
    "SETTING_LEFT_BUMPER_CLICK_PRESSURE",
    "SETTING_RIGHT_BUMPER_CLICK_PRESSURE",
    "SETTING_LEFT_GRIP_CLICK_PRESSURE",
    "SETTING_RIGHT_GRIP_CLICK_PRESSURE",
    "SETTING_LEFT_GRIP2_CLICK_PRESSURE",
    "SETTING_RIGHT_GRIP2_CLICK_PRESSURE",
    // 60
    "SETTING_PRESSURE_MODE",
    "SETTING_CONTROLLER_TEST_MODE",
    "SETTING_TRIGGER_MODE",
    "SETTING_TRACKPAD_Z_THRESHOLD",
    "SETTING_FRAME_RATE",
    "SETTING_TRACKPAD_FILT_CTRL",
    "SETTING_TRACKPAD_CLIP",
    "SETTING_DEBUG_OUTPUT_SELECT",
    "SETTING_TRIGGER_THRESHOLD_PERCENT",
    "SETTING_TRACKPAD_FREQUENCY_HOPPING",
    // 70
    "SETTING_HAPTICS_ENABLED",
    "SETTING_STEAM_WATCHDOG_ENABLE",
    "SETTING_TIMP_TOUCH_THRESHOLD_ON",
    "SETTING_TIMP_TOUCH_THRESHOLD_OFF",
    "SETTING_FREQ_HOPPING",
    "SETTING_TEST_CONTROL",
    "SETTING_HAPTIC_MASTER_GAIN_DB",
    "SETTING_THUMB_TOUCH_THRESH",
    "SETTING_DEVICE_POWER_STATUS",
    "SETTING_HAPTIC_INTENSITY",
    // 80
    "SETTING_STABILIZER_ENABLED",
    "SETTING_TIMP_MODE_MTE",
];

/// The `SettingGyroMode` bitmask — the values [`setting::IMU_MODE`] takes.
///
/// Verbatim from SDL's `controller_constants.h`:
///
/// ```text
/// SETTING_GYRO_MODE_OFF               = 0x0000
/// SETTING_GYRO_MODE_STEERING          = 0x0001
/// SETTING_GYRO_MODE_TILT              = 0x0002
/// SETTING_GYRO_MODE_SEND_ORIENTATION  = 0x0004
/// SETTING_GYRO_MODE_SEND_RAW_ACCEL    = 0x0008
/// SETTING_GYRO_MODE_SEND_RAW_GYRO     = 0x0010
/// ```
///
/// The flags choose *what the controller puts in its input report*, which is
/// why this is the switch that makes the controller's `0x42` carry IMU data at all.
pub mod gyro_mode {
    /// `SETTING_GYRO_MODE_OFF` — no IMU data in the input report.
    pub const OFF: u16 = 0x0000;
    /// `SETTING_GYRO_MODE_STEERING`.
    pub const STEERING: u16 = 0x0001;
    /// `SETTING_GYRO_MODE_TILT`.
    pub const TILT: u16 = 0x0002;
    /// `SETTING_GYRO_MODE_SEND_ORIENTATION` — the fused quaternion.
    pub const SEND_ORIENTATION: u16 = 0x0004;
    /// `SETTING_GYRO_MODE_SEND_RAW_ACCEL` — raw accelerometer.
    pub const SEND_RAW_ACCEL: u16 = 0x0008;
    /// `SETTING_GYRO_MODE_SEND_RAW_GYRO` — raw rate gyro.
    pub const SEND_RAW_GYRO: u16 = 0x0010;

    /// What SDL itself turns on for a game that asked for sensors:
    /// `SETTING_GYRO_MODE_SEND_RAW_ACCEL | SETTING_GYRO_MODE_SEND_RAW_GYRO`
    /// (`HIDAPI_DriverSteam_SetSensorsEnabled`, `SDL_hidapi_steam.c`). This is
    /// hyprpad's own value for `h.gamepad { gyro = true }`, so the controller is
    /// configured the way the reference client configures it and not some
    /// third thing.
    pub const SENSORS_ON: u16 = SEND_RAW_ACCEL | SEND_RAW_GYRO;

    /// A human description of a mode bitmask, for logging.
    pub fn describe(mode: u16) -> String {
        if mode == OFF {
            return "off".to_string();
        }
        let mut on = Vec::new();
        for (bit, name) in [
            (STEERING, "steering"),
            (TILT, "tilt"),
            (SEND_ORIENTATION, "orientation"),
            (SEND_RAW_ACCEL, "raw-accel"),
            (SEND_RAW_GYRO, "raw-gyro"),
        ] {
            if mode & bit != 0 {
                on.push(name);
            }
        }
        let rest = mode & !(STEERING | TILT | SEND_ORIENTATION | SEND_RAW_ACCEL | SEND_RAW_GYRO);
        if rest != 0 {
            return format!("{}+{rest:#06x}", on.join("+"));
        }
        on.join("+")
    }
}

/// A human name for a setting number, for logging only.
pub fn setting_name(id: u8) -> &'static str {
    SETTING_NAMES.get(id as usize).copied().unwrap_or("SETTING_?")
}

/// One `(setting, value)` pair out of a `SetSettingsValues` write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    /// The `SETTING_*` number.
    pub id: u8,
    /// Its little-endian `u16` value.
    pub value: u16,
    /// Whether this is a setting hyprpad maintains itself and will not hand to
    /// Steam — the two `src/lizard.rs` writes.
    pub owned_by_hyprpad: bool,
}

impl Setting {
    /// The SDL name, for logging.
    pub fn name(&self) -> &'static str {
        setting_name(self.id)
    }
}

/// Whether hyprpad, not Steam, is the authority on a setting.
///
/// Deliberately **not** [`setting::IMU_MODE`]: the gyro is the one setting
/// Steam asks for that hyprpad cannot satisfy on its own behalf, because only
/// the real controller can turn its IMU on. That one is *relayed*, through
/// `src/lizard.rs`'s frame builder — see [`imu_mode`].
fn owned_by_hyprpad(id: u8) -> bool {
    matches!(id, setting::LIZARD_MODE | setting::STEAM_WATCHDOG_ENABLE)
}

/// The [`setting::IMU_MODE`] value out of a decoded `SetSettingsValues`, if it
/// carried one.
///
/// The whole of "did Steam just ask for the gyro". SDL's
/// `HIDAPI_DriverSteam_SetSensorsEnabled` builds a `0x87` frame with exactly
/// one pair — `SETTING_IMU_MODE` set to either
/// `SEND_RAW_ACCEL | SEND_RAW_GYRO` or `OFF` — so in practice this reads a
/// one-pair frame; it scans the whole list anyway because Steam is free to fold
/// the setting into a larger write, and takes the **last** occurrence, which is
/// what a firmware applying pairs in order would end up holding.
pub fn imu_mode(settings: &[Setting]) -> Option<u16> {
    settings.iter().rev().find(|s| s.id == setting::IMU_MODE).map(|s| s.value)
}

/// Decode a `SetSettingsValues` (`0x87`) command frame into its pairs.
///
/// Frame, from `src/lizard.rs::disable_lizard_settings_report` and SDL's
/// `DisableSteamTritonLizardMode`:
/// `[0x87][len][setting, value_lo, value_hi] × n`, with `len = 3 * n`.
///
/// A truncated or over-long `len` is clamped to what the buffer actually holds
/// rather than rejected: Steam's frames are zero-padded to the full 64-byte
/// report and a partial trailing triple is simply not a pair.
pub fn parse_set_settings_values(cmd: &[u8]) -> Vec<Setting> {
    if cmd.first() != Some(&cmd::SET_SETTINGS_VALUES) {
        return Vec::new();
    }
    let declared = cmd.get(1).copied().unwrap_or(0) as usize;
    let available = cmd.len().saturating_sub(2);
    let n = declared.min(available) / 3;
    (0..n)
        .map(|i| {
            let at = 2 + i * 3;
            Setting {
                id: cmd[at],
                value: u16::from_le_bytes([cmd[at + 1], cmd[at + 2]]),
                owned_by_hyprpad: owned_by_hyprpad(cmd[at]),
            }
        })
        .collect()
}

/// What the daemon should do about one write from Steam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Drive the controller's force-feedback rumble. `strong`/`weak` are the
    /// `FF_RUMBLE` magnitudes `haptics::Haptics::rumble` takes directly.
    Rumble { strong: u16, weak: u16 },
    /// Fire a haptic pulse train on one actuator —
    /// `haptics::Haptics::pulse`'s four arguments.
    Haptic { pad: Pad, on_us: u16, off_us: u16, count: u16 },
    /// A decoded `SetSettingsValues`. Informational: hyprpad maintains lizard
    /// mode itself and does not forward settings to the controller.
    Settings(Vec<Setting>),
    /// A **query**, already answered by the relay's canned data.
    ///
    /// Valve's control protocol is stateful in one small way: a `SET_REPORT`
    /// naming a query command does not set anything, it *selects* what the next
    /// `GET_REPORT` reads back. `uhid.rs`'s event loop stores the id as the
    /// selector and the following `GET_REPORT` returns
    /// `Profile::get_report_reply` for it — so by the time the daemon sees this
    /// action the exchange is complete.
    ///
    /// It exists to keep the debug log honest. `dropped GetAttributesValues
    /// (0x83)` is what it used to say, which reads as "Steam asked and we
    /// ignored it" when in fact Steam asked and was answered.
    Answered { id: u8, name: &'static str },
    /// Understood, deliberately not acted on.
    Dropped { id: u8, name: &'static str },
    /// Not a well-formed command frame at all.
    Malformed,
}

impl Action {
    /// A one-line description for the daemon's debug log.
    pub fn describe(&self) -> String {
        match self {
            Action::Rumble { strong, weak } => format!("rumble strong={strong} weak={weak}"),
            Action::Haptic { pad, on_us, off_us, count } => {
                format!("haptic {pad:?} on={on_us}us off={off_us}us x{count}")
            }
            Action::Settings(s) => {
                let pairs: Vec<String> = s
                    .iter()
                    .map(|p| {
                        let own = if p.owned_by_hyprpad { " (hyprpad's; ignored)" } else { "" };
                        // The gyro mask is the one value whose *bits* are the
                        // meaning, and the one this log is read to answer
                        // ("did Steam ask for the IMU, and for what?").
                        if p.id == setting::IMU_MODE {
                            return format!(
                                "{}={} [{}]",
                                p.name(),
                                p.value,
                                gyro_mode::describe(p.value)
                            );
                        }
                        format!("{}={}{}", p.name(), p.value, own)
                    })
                    .collect();
                format!("SetSettingsValues [{}]", pairs.join(", "))
            }
            Action::Answered { id, name } => format!("answered {name} ({id:#04x})"),
            Action::Dropped { id, name } => format!("dropped {name} ({id:#04x})"),
            Action::Malformed => "malformed write".to_string(),
        }
    }
}

/// Interpret one write from Steam.
pub fn classify(write: &HostWrite) -> Action {
    let cmd = write.command();
    let Some(&id) = cmd.first() else { return Action::Malformed };
    match id {
        cmd::SET_SETTINGS_VALUES => Action::Settings(parse_set_settings_values(cmd)),
        cmd::TRIGGER_RUMBLE_COMMAND => parse_deck_rumble(cmd),
        cmd::TRIGGER_HAPTIC_COMMAND => parse_deck_haptic(cmd),
        cmd::TRIGGER_HAPTIC_PULSE => parse_haptic_pulse(cmd),
        // The controller's own output reports, which the triton profile's descriptor
        // declares and Steam can therefore write directly. Same fields
        // `src/haptics.rs` builds, read back.
        cmd::OUT_RUMBLE if write.is_output() => parse_native_rumble(cmd),
        cmd::OUT_PULSE if write.is_output() => parse_native_pulse(cmd),
        // A query on the *feature* channel: the event loop has already stored
        // it as the GET selector and the reply is canned, so this is a
        // completed exchange, not a dropped one. Only the feature channel
        // selects — the interrupt-out channel's `0x83` is a declared output
        // report on the triton descriptor, an actuator command with the same
        // number, and must not be mistaken for a query.
        other if !write.is_output() && cmd::ANSWERED_QUERIES.contains(&other) => {
            Action::Answered { id: other, name: command_name(other) }
        }
        other => Action::Dropped { id: other, name: command_name(other) },
    }
}

// ---------------------------------------------------------------------------
// Rumble and haptics
// ---------------------------------------------------------------------------

/// `TriggerRumbleCommand` (`0xEB`), from InputPlumber's `PackedRumbleReport`:
///
/// ```text
/// byte 0    cmd_id (0xEB)
/// byte 1    report_size
/// byte 2    unk_2
/// byte 3    event_type
/// byte 4    intensity
/// bytes 5-6 left_speed   (u16 LE)
/// bytes 7-8 right_speed  (u16 LE)
/// ```
///
/// `left_speed`/`right_speed` are the same `FF_RUMBLE` strong/weak magnitudes
/// `haptics::Haptics::rumble` forwards to the controller's `0x80`, so the translation
/// is a straight hand-off.
fn parse_deck_rumble(cmd: &[u8]) -> Action {
    let (Some(strong), Some(weak)) = (le_u16(cmd, 5), le_u16(cmd, 7)) else {
        return Action::Malformed;
    };
    Action::Rumble { strong, weak }
}

/// `TriggerHapticCommand` (`0xEA`), from InputPlumber's `PackedHapticReport`:
///
/// ```text
/// byte 0 cmd_id (0xEA)   byte 3 cmd_type (enum)
/// byte 1 report_size     byte 4 intensity (enum)
/// byte 2 side (enum)     byte 5 gain (i8)
/// ```
///
/// **Partly UNVERIFIED.** The layout is verbatim, but `PadSide` and `Intensity`
/// are Rust enums whose discriminants were not captured, and — unlike the
/// classic `0x8F` — this command carries **no duration at all**, only an
/// intensity class. So the shape fired here is hyprpad's own calibrated single
/// tick (`0x190` = 400 µs, the value `haptics::Feel::Tick` takes from the
/// kernel's `steam_haptic_pulse(.., 0x190, 0, 1, 0)`), on the named side. The
/// side is read as `0 = left, 1 = right, 2 = both`, matching every other Valve
/// side encoding in the tree. Both assumptions are recorded in
/// `docs/design/uhid-relay.md`; the first live session's debug log settles them.
fn parse_deck_haptic(cmd: &[u8]) -> Action {
    let Some(&side) = cmd.get(2) else { return Action::Malformed };
    Action::Haptic { pad: logical_pad(side), on_us: 0x0190, off_us: 0, count: 1 }
}

/// `TriggerHapticPulse` (`0x8F`), from the kernel's `steam_haptic_pulse`
/// (`drivers/hid/hid-steam.c`):
///
/// ```text
/// byte 0    ID_TRIGGER_HAPTIC_PULSE (0x8F)
/// byte 1    payload length (8)
/// byte 2    pad — the *wire* side, XOR-inverted (see haptics::wire_side)
/// bytes 3-4 duration (u16 LE, µs on)
/// bytes 5-6 interval (u16 LE, µs off)
/// bytes 7-8 count    (u16 LE)
/// byte 9    gain (dB, -24..=+6)
/// ```
///
/// The gain is dropped: the controller's IBEX `0x81` pulse struct has no gain field
/// (`src/haptics.rs`), so there is nowhere to put it.
fn parse_haptic_pulse(cmd: &[u8]) -> Action {
    let Some(&wire) = cmd.get(2) else { return Action::Malformed };
    let (Some(on_us), Some(off_us), Some(count)) = (le_u16(cmd, 3), le_u16(cmd, 5), le_u16(cmd, 7))
    else {
        return Action::Malformed;
    };
    Action::Haptic { pad: unwire_side(wire), on_us, off_us, count }
}

/// The controller's own `0x80` rumble output report, read back with the field layout
/// `haptics::build_rumble` writes:
/// `[0x80, type, intensity:le16, left_speed:le16, left_gain, right_speed:le16,
/// right_gain]`. Only reachable on the triton profile, whose descriptor
/// declares output report `0x80`.
fn parse_native_rumble(cmd: &[u8]) -> Action {
    let (Some(strong), Some(weak)) = (le_u16(cmd, 4), le_u16(cmd, 7)) else {
        return Action::Malformed;
    };
    Action::Rumble { strong, weak }
}

/// The controller's own `0x81` pulse output report, read back with the field layout
/// `haptics::build_pulse` writes:
/// `[0x81, wire_side, on_us:le16, off_us:le16, count:le16]`.
fn parse_native_pulse(cmd: &[u8]) -> Action {
    let Some(&wire) = cmd.get(1) else { return Action::Malformed };
    let (Some(on_us), Some(off_us), Some(count)) = (le_u16(cmd, 2), le_u16(cmd, 4), le_u16(cmd, 6))
    else {
        return Action::Malformed;
    };
    Action::Haptic { pad: unwire_side(wire), on_us, off_us, count }
}

/// Undo the firmware's side inversion.
///
/// `haptics::wire_side` runs `if pad < Both { pad ^= 1 }` on the way out,
/// mirroring the kernel; `^ 1` is its own inverse, so the same operation brings
/// a wire value back to `haptics::Pad`. Anything out of range is read as `Both`,
/// which is the safe over-approximation — a pulse on the wrong pad is a bug, a
/// pulse on both is merely broader.
fn unwire_side(wire: u8) -> Pad {
    match wire {
        0 => Pad::Right,
        1 => Pad::Left,
        _ => Pad::Both,
    }
}

/// Read a side byte that is *not* wire-inverted (the Deck's `PadSide`).
fn logical_pad(side: u8) -> Pad {
    match side {
        0 => Pad::Left,
        1 => Pad::Right,
        _ => Pad::Both,
    }
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uhid::{rtype, WriteChannel};

    fn feature(payload: &[u8]) -> HostWrite {
        // A feature write as it arrives: report-number byte, then the command.
        let mut data = vec![0x01];
        data.extend_from_slice(payload);
        data.resize(64, 0); // Steam pads to the full report
        HostWrite { channel: WriteChannel::SetReport, rtype: rtype::FEATURE, data }
    }

    fn output(payload: &[u8]) -> HostWrite {
        HostWrite {
            channel: WriteChannel::Output,
            rtype: rtype::OUTPUT,
            data: payload.to_vec(),
        }
    }

    /// The exact frame `src/lizard.rs` builds, and the one SDL's
    /// `DisableSteamTritonLizardMode` sends every 3 s — decoded back.
    #[test]
    fn parses_the_lizard_disable_write_hyprpad_itself_sends() {
        let w = feature(&[
            0x87, 6, // SetSettingsValues, 3 bytes * 2 settings
            9, 0x00, 0x00, // SETTING_LIZARD_MODE = 0
            71, 0x00, 0x00, // SETTING_STEAM_WATCHDOG_ENABLE = 0
        ]);
        let Action::Settings(s) = classify(&w) else { panic!("expected Settings") };
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], Setting { id: 9, value: 0, owned_by_hyprpad: true });
        assert_eq!(s[1], Setting { id: 71, value: 0, owned_by_hyprpad: true });
        assert_eq!(s[0].name(), "SETTING_LIZARD_MODE");
        assert_eq!(s[1].name(), "SETTING_STEAM_WATCHDOG_ENABLE");
        assert!(
            s.iter().all(|p| p.owned_by_hyprpad),
            "both are hyprpad's; Steam's copy is acknowledged and ignored"
        );
    }

    /// SDL's single-setting form: `msg->header.length = 1 * sizeof(ControllerSetting)`.
    #[test]
    fn parses_sdls_single_setting_form() {
        let w = feature(&[0x87, 3, 9, 0x00, 0x00]);
        assert_eq!(
            classify(&w),
            Action::Settings(vec![Setting { id: 9, value: 0, owned_by_hyprpad: true }])
        );
    }

    #[test]
    fn parses_several_pairs_including_settings_hyprpad_does_not_own() {
        let cmd = [
            0x87, 12, //
            setting::LIZARD_MODE, 0x00, 0x00, //
            setting::STEAMBUTTON_POWEROFF_TIME, 0x2c, 0x01, // 300
            setting::SLEEP_INACTIVITY_TIMEOUT, 0xff, 0xff, // 65535
            setting::SMOOTH_ABSOLUTE_MOUSE, 0x01, 0x00, //
        ];
        let s = parse_set_settings_values(&cmd);
        assert_eq!(s.len(), 4);
        assert_eq!(s[1], Setting { id: 25, value: 300, owned_by_hyprpad: false });
        assert_eq!(s[2], Setting { id: 50, value: 65_535, owned_by_hyprpad: false });
        assert_eq!(s[3].name(), "SETTING_SMOOTH_ABSOLUTE_MOUSE");
        assert!(!s[3].owned_by_hyprpad);
        assert_eq!(
            Action::Settings(s).describe(),
            "SetSettingsValues [SETTING_LIZARD_MODE=0 (hyprpad's; ignored), \
             SETTING_STEAMBUTTON_POWEROFF_TIME=300, SETTING_SLEEP_INACTIVITY_TIMEOUT=65535, \
             SETTING_SMOOTH_ABSOLUTE_MOUSE=1]"
        );
    }

    #[test]
    fn a_truncated_or_lying_length_yields_only_whole_pairs() {
        // `len` claims four pairs; the buffer holds one and a half.
        assert_eq!(parse_set_settings_values(&[0x87, 12, 9, 0, 0, 71, 0]).len(), 1);
        // `len` is not a multiple of three.
        assert_eq!(parse_set_settings_values(&[0x87, 4, 9, 0, 0, 71]).len(), 1);
        // Zero-length, and the wrong command entirely.
        assert!(parse_set_settings_values(&[0x87, 0]).is_empty());
        assert!(parse_set_settings_values(&[0x88, 3, 9, 0, 0]).is_empty());
        assert!(parse_set_settings_values(&[]).is_empty());
    }

    #[test]
    fn the_deck_rumble_command_becomes_ff_magnitudes() {
        // PackedRumbleReport: cmd, size, unk_2, event_type, intensity,
        //                     left_speed le16, right_speed le16.
        let w = feature(&[0xEB, 9, 0x00, 0x01, 0x02, 0xe8, 0x03, 0xd0, 0x07]);
        assert_eq!(classify(&w), Action::Rumble { strong: 1_000, weak: 2_000 });
        assert_eq!(
            classify(&w).describe(),
            "rumble strong=1000 weak=2000",
            "the log names what Steam asked for"
        );
        // A stop is a rumble of zero, not the absence of a command.
        let w = feature(&[0xEB, 9, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(classify(&w), Action::Rumble { strong: 0, weak: 0 });
    }

    #[test]
    fn the_classic_haptic_pulse_keeps_its_shape_and_unwires_the_side() {
        // steam_haptic_pulse(pad=RIGHT) writes wire side 0; count 3.
        let w = feature(&[0x8F, 8, 0, 0x90, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00]);
        assert_eq!(
            classify(&w),
            Action::Haptic { pad: Pad::Right, on_us: 0x190, off_us: 0, count: 3 }
        );
        // wire 1 is the LEFT actuator (haptics::wire_side XORs on the way out).
        let w = feature(&[0x8F, 8, 1, 0xf4, 0x01, 0xf4, 0x01, 0x0a, 0x00, 0x00]);
        assert_eq!(
            classify(&w),
            Action::Haptic { pad: Pad::Left, on_us: 500, off_us: 500, count: 10 }
        );
        // Anything else is read as both — the safe over-approximation.
        let w = feature(&[0x8F, 8, 2, 0x01, 0x00, 0, 0, 1, 0, 0]);
        assert!(matches!(classify(&w), Action::Haptic { pad: Pad::Both, .. }));
    }

    /// The round trip that matters: what `src/haptics.rs` writes to the real
    /// controller must decode back to the same request when Steam writes it to us.
    #[test]
    fn the_controllers_own_output_reports_decode_back_to_what_haptics_rs_builds() {
        // build_pulse(Pad::Left, 400, 0, 1) -> [0x81, wire_side(Left)=1, …]
        let w = output(&[0x81, 1, 0x90, 0x01, 0x00, 0x00, 0x01, 0x00]);
        assert_eq!(
            classify(&w),
            Action::Haptic { pad: Pad::Left, on_us: 400, off_us: 0, count: 1 }
        );
        // build_rumble(1000, 2000) -> [0x80, 0, int:le16, left:le16, gain, right:le16, gain]
        let w = output(&[0x80, 0, 0, 0, 0xe8, 0x03, 2, 0xd0, 0x07, 0]);
        assert_eq!(classify(&w), Action::Rumble { strong: 1_000, weak: 2_000 });
    }

    /// A `0x80`/`0x81` arriving on the *feature* channel is a command id, not an
    /// output report, and must not be mistaken for haptics.
    #[test]
    fn the_same_ids_mean_different_things_on_the_two_channels() {
        let w = feature(&[0x81, 0x00]); // ClearDigitalMappings
        assert_eq!(classify(&w), Action::Dropped { id: 0x81, name: "ClearDigitalMappings" });
        let w = feature(&[0x80, 0x00]); // SetDigitalMappings
        assert_eq!(classify(&w), Action::Dropped { id: 0x80, name: "SetDigitalMappings" });
    }

    #[test]
    fn the_deck_haptic_command_fires_the_calibrated_tick_on_the_named_side() {
        let w = feature(&[0xEA, 13, 0, 1, 2, 0]);
        assert_eq!(
            classify(&w),
            Action::Haptic { pad: Pad::Left, on_us: 0x190, off_us: 0, count: 1 }
        );
        let w = feature(&[0xEA, 13, 1, 1, 2, 0]);
        assert!(matches!(classify(&w), Action::Haptic { pad: Pad::Right, .. }));
    }

    #[test]
    fn everything_else_is_dropped_by_name() {
        for (id, name) in [
            (0x86u8, "FactoryReset"),
            (0x9F, "TurnOffController"),
            (0xA9, "SetSerialNumber"),
            (0xAF, "RadioEraseRecords"),
            (0xB7, "AudioUpdateStart"),
            (0xCE, "ResetIMU"),
            (0x8D, "SetControllerMode"),
        ] {
            let w = feature(&[id, 0]);
            assert_eq!(classify(&w), Action::Dropped { id, name });
        }
        // An id nobody has documented still logs cleanly rather than panicking.
        let w = feature(&[0x77, 0]);
        assert_eq!(classify(&w), Action::Dropped { id: 0x77, name: "unknown" });
        assert_eq!(classify(&w).describe(), "dropped unknown (0x77)");
    }

    /// A query is a **request**, not a setting: it selects what the next
    /// `GET_REPORT` reads back, `uhid.rs` answers it from the profile's canned
    /// data, and by the time the daemon sees it the exchange is done. Logging
    /// it as "dropped" — which is what `[relay] dropped GetAttributesValues
    /// (0x83)` said — sends anyone reading the log after the wrong bug.
    #[test]
    fn a_query_on_the_feature_channel_is_answered_not_dropped() {
        for (id, name) in [
            (0x83u8, "GetAttributesValues"),
            (0xAE, "GetStringAttribute"),
            (0xBA, "GetChipId"),
        ] {
            let w = feature(&[id, 0]);
            assert_eq!(classify(&w), Action::Answered { id, name });
            assert_eq!(classify(&w).describe(), format!("answered {name} ({id:#04x})"));
        }
        assert_eq!(
            classify(&feature(&[0x83, 0])).describe(),
            "answered GetAttributesValues (0x83)"
        );

        // The list is exactly the set `Profile::canned_reply` has an answer
        // for — no more, no less.
        assert_eq!(
            cmd::ANSWERED_QUERIES,
            [
                crate::uhid::profile::cmd::GET_ATTRIBUTES_VALUES,
                crate::uhid::profile::cmd::GET_STRING_ATTRIBUTE,
                crate::uhid::profile::cmd::GET_CHIP_ID
            ]
        );

        // On the interrupt-out channel `0x83` is a declared output report of
        // the triton descriptor — an actuator command that happens to share the
        // number. It selects nothing and must not read as answered.
        assert_eq!(
            classify(&output(&[0x83, 0])),
            Action::Dropped { id: 0x83, name: "GetAttributesValues" }
        );
    }

    #[test]
    fn a_write_with_no_command_at_all_is_malformed_not_a_panic() {
        let empty =
            HostWrite { channel: WriteChannel::SetReport, rtype: rtype::FEATURE, data: vec![0x01] };
        assert_eq!(classify(&empty), Action::Malformed);
        // A rumble frame cut off before its magnitudes.
        let w = feature(&[0xEB, 9, 0, 0]);
        // (feature() pads to 64, so build the short one by hand)
        let short = HostWrite {
            channel: WriteChannel::SetReport,
            rtype: rtype::FEATURE,
            data: vec![0x01, 0xEB, 9, 0, 0],
        };
        assert_eq!(classify(&short), Action::Malformed);
        assert_eq!(classify(&w), Action::Rumble { strong: 0, weak: 0 }, "padded is fine");
    }

    #[test]
    fn command_names_cover_the_ids_the_probe_logged() {
        // The selector names the proven probe printed, plus the three canned
        // GET_REPORT queries.
        assert_eq!(command_name(0x87), "SetSettingsValues");
        assert_eq!(command_name(0x83), "GetAttributesValues");
        assert_eq!(command_name(0xAE), "GetStringAttribute");
        assert_eq!(command_name(0xBA), "GetChipId");
        assert_eq!(command_name(0xEA), "TriggerHapticCommand");
        assert_eq!(command_name(0xEB), "TriggerRumbleCommand");
        assert_eq!(command_name(cmd::INPUT_DATA), "InputData");
    }

    // -----------------------------------------------------------------------
    // The setting table, and the gyro (gap G5)
    // -----------------------------------------------------------------------

    /// The whole point of [`SETTING_NAMES`]: a debug log that used to print
    /// `SETTING_?` eight times a session now names everything Valve named.
    ///
    /// The four ids in the first live Triton log whose meaning had to be
    /// guessed from their values are checked by name here, together with the
    /// values SDL is on record writing to them — 7 is `TRACKPAD_NONE` and
    /// `0xFFFF` is "no click pressure", both straight out of
    /// `SDL_hidapi_steamdeck.c`'s five-pair configure frame.
    #[test]
    fn the_full_setting_table_names_the_ids_the_first_live_session_could_not() {
        assert_eq!(SETTING_NAMES.len(), 82, "the enum through SETTING_TIMP_MODE_MTE");
        assert_eq!(setting_name(setting::LEFT_TRACKPAD_MODE), "SETTING_LEFT_TRACKPAD_MODE");
        assert_eq!(setting_name(setting::RIGHT_TRACKPAD_MODE), "SETTING_RIGHT_TRACKPAD_MODE");
        assert_eq!(setting_name(setting::IMU_MODE), "SETTING_IMU_MODE");
        assert_eq!(
            setting_name(setting::WIRELESS_PACKET_VERSION),
            "SETTING_WIRELESS_PACKET_VERSION"
        );
        assert_eq!(
            setting_name(setting::LEFT_TRACKPAD_CLICK_PRESSURE),
            "SETTING_LEFT_TRACKPAD_CLICK_PRESSURE"
        );
        assert_eq!(
            setting_name(setting::RIGHT_TRACKPAD_CLICK_PRESSURE),
            "SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE"
        );
        // Past the end of the enum there is still no panic and still a name.
        assert_eq!(setting_name(200), "SETTING_?");
        assert_eq!(setting_name(u8::MAX), "SETTING_?", "SETTING_ALL is not a real slot");
    }

    /// Every constant the daemon spells by name must be its own slot in the
    /// table — the one thing that could silently go wrong when a table is
    /// transcribed by index rather than by name.
    #[test]
    fn every_named_constant_matches_its_slot_in_the_full_table() {
        for (id, name) in [
            (setting::LEFT_TRACKPAD_MODE, "SETTING_LEFT_TRACKPAD_MODE"),
            (setting::RIGHT_TRACKPAD_MODE, "SETTING_RIGHT_TRACKPAD_MODE"),
            (setting::LIZARD_MODE, "SETTING_LIZARD_MODE"),
            (setting::SMOOTH_ABSOLUTE_MOUSE, "SETTING_SMOOTH_ABSOLUTE_MOUSE"),
            (setting::STEAMBUTTON_POWEROFF_TIME, "SETTING_STEAMBUTTON_POWEROFF_TIME"),
            (setting::IMU_MODE, "SETTING_IMU_MODE"),
            (setting::WIRELESS_PACKET_VERSION, "SETTING_WIRELESS_PACKET_VERSION"),
            (setting::SLEEP_INACTIVITY_TIMEOUT, "SETTING_SLEEP_INACTIVITY_TIMEOUT"),
            (setting::LEFT_TRACKPAD_CLICK_PRESSURE, "SETTING_LEFT_TRACKPAD_CLICK_PRESSURE"),
            (setting::RIGHT_TRACKPAD_CLICK_PRESSURE, "SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE"),
            (setting::STEAM_WATCHDOG_ENABLE, "SETTING_STEAM_WATCHDOG_ENABLE"),
            (setting::DEVICE_POWER_STATUS, "SETTING_DEVICE_POWER_STATUS"),
        ] {
            assert_eq!(SETTING_NAMES[id as usize], name, "setting {id}");
        }
        // The three numbers this project had before the table existed, pinned
        // again from the other direction so a re-transcription cannot shift the
        // enum without failing here.
        assert_eq!(setting::LIZARD_MODE, 9);
        assert_eq!(setting::IMU_MODE, 48);
        assert_eq!(setting::STEAM_WATCHDOG_ENABLE, 71);
    }

    /// The gyro bitmask, and the one value hyprpad writes of its own accord.
    #[test]
    fn the_gyro_mode_bits_are_sdls_and_sensors_on_is_what_sdl_enables() {
        use gyro_mode::*;
        assert_eq!(OFF, 0x0000);
        assert_eq!(STEERING, 0x0001);
        assert_eq!(TILT, 0x0002);
        assert_eq!(SEND_ORIENTATION, 0x0004);
        assert_eq!(SEND_RAW_ACCEL, 0x0008);
        assert_eq!(SEND_RAW_GYRO, 0x0010);
        // `HIDAPI_DriverSteam_SetSensorsEnabled`, SDL_hidapi_steam.c:
        // ADD_SETTING(SETTING_IMU_MODE, SEND_RAW_ACCEL | SEND_RAW_GYRO)
        assert_eq!(SENSORS_ON, 0x0018);
        assert_eq!(describe(OFF), "off");
        assert_eq!(describe(SENSORS_ON), "raw-accel+raw-gyro");
        assert_eq!(describe(SEND_ORIENTATION | SEND_RAW_GYRO), "orientation+raw-gyro");
        // sc-controller's `configure()` writes 0x14 here.
        assert_eq!(describe(0x14), "orientation+raw-gyro");
        // An unknown bit is shown, not swallowed.
        assert!(describe(0x8000).contains("0x8000"));
    }

    /// The exact frame SDL sends when a game turns its sensors on, decoded
    /// back — and the value the daemon lifts out of it.
    ///
    /// `HIDAPI_DriverSteam_SetSensorsEnabled` builds `buf[1] = 0x87`, one pair,
    /// `buf[2] = 3`. That is the whole of Steam's gyro request.
    #[test]
    fn steams_imu_enable_and_disable_writes_are_decoded_to_a_mode() {
        let on = feature(&[0x87, 3, setting::IMU_MODE, 0x18, 0x00]);
        let Action::Settings(pairs) = classify(&on) else { panic!("not a settings write") };
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].id, setting::IMU_MODE);
        assert_eq!(pairs[0].value, gyro_mode::SENSORS_ON);
        assert!(!pairs[0].owned_by_hyprpad, "the gyro is relayed, not held by hyprpad");
        assert_eq!(imu_mode(&pairs), Some(gyro_mode::SENSORS_ON));

        let off = feature(&[0x87, 3, setting::IMU_MODE, 0x00, 0x00]);
        let Action::Settings(pairs) = classify(&off) else { panic!("not a settings write") };
        assert_eq!(imu_mode(&pairs), Some(gyro_mode::OFF));
    }

    /// A settings write with no gyro pair asks for nothing — the common case,
    /// and the one that must not nudge the controller.
    #[test]
    fn a_settings_write_without_the_gyro_pair_asks_for_nothing() {
        // The lizard-disable frame hyprpad itself sends.
        let w = feature(&[0x87, 6, 9, 0, 0, 71, 0, 0]);
        let Action::Settings(pairs) = classify(&w) else { panic!("not a settings write") };
        assert_eq!(imu_mode(&pairs), None);
        assert_eq!(imu_mode(&[]), None);
    }

    /// Steam is free to fold the gyro into a larger write, and to state it
    /// twice; the last value is the one a firmware applying pairs in order
    /// would be left holding.
    #[test]
    fn the_last_gyro_pair_in_a_multi_setting_write_wins() {
        let w = feature(&[
            0x87, 12, //
            setting::SMOOTH_ABSOLUTE_MOUSE, 0, 0, //
            setting::IMU_MODE, 0x18, 0x00, //
            setting::LEFT_TRACKPAD_MODE, 7, 0, //
            setting::IMU_MODE, 0x00, 0x00, //
        ]);
        let Action::Settings(pairs) = classify(&w) else { panic!("not a settings write") };
        assert_eq!(pairs.len(), 4);
        assert_eq!(imu_mode(&pairs), Some(gyro_mode::OFF), "the later pair wins");
    }

    /// The debug log is the only place a human sees the gyro request, so the
    /// mask is spelled out rather than printed as a bare number.
    #[test]
    fn the_debug_line_spells_the_gyro_mask_out() {
        let w = feature(&[0x87, 3, setting::IMU_MODE, 0x18, 0x00]);
        let line = classify(&w).describe();
        assert!(line.contains("SETTING_IMU_MODE=24"), "{line}");
        assert!(line.contains("raw-accel+raw-gyro"), "{line}");
    }
}
