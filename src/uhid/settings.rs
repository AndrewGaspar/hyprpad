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
//! forward Steam's feature writes straight to the real puck — and §5.1/§5.2 then
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
//!    runs a single writer thread over the puck's only writable fd. §5.2 is
//!    explicit that the relay must go through it and "never open a second
//!    writable fd on the puck", so rumble and haptics are *translated into that
//!    path* rather than forwarded as raw bytes.
//!
//! 3. **The queries are already answered by the time the daemon sees them.** A
//!    `SET_REPORT` naming `GetAttributesValues`, `GetStringAttribute` or
//!    `GetChipId` selects what the next `GET_REPORT` reads back; `uhid.rs`
//!    stores the selector and replies from the profile's canned data. Those
//!    classify as [`Action::Answered`], not as drops.
//!
//! Everything else is dropped with a debug log. Note what that costs: Steam's
//! IMU-enable would not reach the puck, so gyro does not start streaming by
//! itself. That is a known limitation of this build, recorded in
//! `docs/design/uhid-relay.md`.
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
//! * `0x81` / `0x80` — `src/haptics.rs`, which writes both to the real puck.
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
    /// The puck's `0x80` force-feedback **output** report (`haptics.rs`).
    pub const OUT_RUMBLE: u8 = 0x80;
    /// The puck's `0x81` haptic-pulse **output** report (`haptics.rs`).
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

/// `SETTING_*` numbers, from SDL's `controller_constants.h` as catalogued in
/// `docs/research/guide-hold-poweroff.md`.
pub mod setting {
    /// `SETTING_LIZARD_MODE` — the master firmware keyboard/mouse switch.
    /// **hyprpad's**: `src/lizard.rs` holds it at 0.
    pub const LIZARD_MODE: u8 = 9;
    /// `SETTING_SMOOTH_ABSOLUTE_MOUSE`.
    pub const SMOOTH_ABSOLUTE_MOUSE: u8 = 24;
    /// `SETTING_STEAMBUTTON_POWEROFF_TIME`.
    pub const STEAMBUTTON_POWEROFF_TIME: u8 = 25;
    /// `SETTING_SLEEP_INACTIVITY_TIMEOUT`.
    pub const SLEEP_INACTIVITY_TIMEOUT: u8 = 50;
    /// `SETTING_STEAM_WATCHDOG_ENABLE` — reverts the pad to lizard mode when no
    /// host heartbeat arrives. **hyprpad's**: `src/lizard.rs` holds it at 0.
    pub const STEAM_WATCHDOG_ENABLE: u8 = 71;
    /// `SETTING_DEVICE_POWER_STATUS`.
    pub const DEVICE_POWER_STATUS: u8 = 78;
}

/// A human name for a setting number, for logging only.
pub fn setting_name(id: u8) -> &'static str {
    match id {
        setting::LIZARD_MODE => "SETTING_LIZARD_MODE",
        setting::SMOOTH_ABSOLUTE_MOUSE => "SETTING_SMOOTH_ABSOLUTE_MOUSE",
        setting::STEAMBUTTON_POWEROFF_TIME => "SETTING_STEAMBUTTON_POWEROFF_TIME",
        setting::SLEEP_INACTIVITY_TIMEOUT => "SETTING_SLEEP_INACTIVITY_TIMEOUT",
        setting::STEAM_WATCHDOG_ENABLE => "SETTING_STEAM_WATCHDOG_ENABLE",
        setting::DEVICE_POWER_STATUS => "SETTING_DEVICE_POWER_STATUS",
        _ => "SETTING_?",
    }
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
fn owned_by_hyprpad(id: u8) -> bool {
    matches!(id, setting::LIZARD_MODE | setting::STEAM_WATCHDOG_ENABLE)
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
    /// Drive the puck's force-feedback rumble. `strong`/`weak` are the
    /// `FF_RUMBLE` magnitudes `haptics::Haptics::rumble` takes directly.
    Rumble { strong: u16, weak: u16 },
    /// Fire a haptic pulse train on one actuator —
    /// `haptics::Haptics::pulse`'s four arguments.
    Haptic { pad: Pad, on_us: u16, off_us: u16, count: u16 },
    /// A decoded `SetSettingsValues`. Informational: hyprpad maintains lizard
    /// mode itself and does not forward settings to the puck.
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
        // The puck's own output reports, which the triton profile's descriptor
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
/// `haptics::Haptics::rumble` forwards to the puck's `0x80`, so the translation
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
/// The gain is dropped: the puck's IBEX `0x81` pulse struct has no gain field
/// (`src/haptics.rs`), so there is nowhere to put it.
fn parse_haptic_pulse(cmd: &[u8]) -> Action {
    let Some(&wire) = cmd.get(2) else { return Action::Malformed };
    let (Some(on_us), Some(off_us), Some(count)) = (le_u16(cmd, 3), le_u16(cmd, 5), le_u16(cmd, 7))
    else {
        return Action::Malformed;
    };
    Action::Haptic { pad: unwire_side(wire), on_us, off_us, count }
}

/// The puck's own `0x80` rumble output report, read back with the field layout
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

/// The puck's own `0x81` pulse output report, read back with the field layout
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
    /// puck must decode back to the same request when Steam writes it to us.
    #[test]
    fn the_pucks_own_output_reports_decode_back_to_what_haptics_rs_builds() {
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
}
