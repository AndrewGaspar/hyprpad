//! The identities hyprpad can present to Steam, as **data**.
//!
//! Two profiles ship, and which one is live is one config line
//! (`h.gamepad { kind = "steam", identity = "triton" | "deck" }`):
//!
//! | | [`triton`] (default) | [`deck`] (proven fallback) |
//! |---|---|---|
//! | VID:PID | `28de:1302` — the *wired* single-interface Steam Controller | `28de:12f0` — InputPlumber's `ProductId::Generic` |
//! | Descriptor | the real 372-byte capture, `docs/research/assets/` | InputPlumber's 38-byte vendor-only blob |
//! | Input report | **the controller's own `0x42`, passed through** — 54 bytes | a translated 64-byte Deck report |
//! | Report IDs | yes, on all three types (`dev_flags` `0b111`) | none at all (`dev_flags` `0`) |
//! | `GET_REPORT` answer | **65 bytes**: report number + a 64-byte Valve message | 64 bytes, the proven probe's |
//! | Steam adoption | **unproven** — awaits the owner's live test | **PROVEN on-device 2026-09-01** |
//!
//! [`Framing`] carries all of that as data and says where each number comes
//! from; it is what the relay frames every report with.
//!
//! # Why `triton` is the default despite being unproven
//!
//! Because it is the *least translation*. hyprpad owns a real Triton, and the
//! wired `1302`'s vendor collection declares input report `0x42` with a 53-byte
//! payload — byte-for-byte the same report `src/report.rs` already decodes off
//! the controller. So the input path is a **pass-through**: no transcoding, and the
//! IMU bytes (30+) and every field the decoder does not model ride along for
//! free the moment Steam sends its own enable. Steam's own log shows it drives a
//! wired `1302` with no dongle/pairing work items at all
//! (`docs/research/uhid-steam-controller.md` §3.3).
//!
//! The one thing not settled is whether Steam accepts a `1302` whose
//! `Interface:` reads `-1` because a uhid device has no USB parent (§3.2, §6 R1,
//! Q-u1). If it does not, `identity = "deck"` is one line away and is the recipe
//! SteamOS ships.
//!
//! # Why not `28de:1304`, the controller's own PID
//!
//! Steam derives the controller *slot index* from `bInterfaceNumber` for the
//! dongle PIDs, and SDL gates `0x1304` on `interface_number` being 2..=5. A uhid
//! device reports `-1`. Cloning the real controller therefore cannot work — §3.2, and
//! InputPlumber's own comment for the analogous Deck case.

use crate::uhid::BUS_USB;

/// Valve's USB vendor id.
pub const VID_VALVE: u32 = 0x28de;

/// Valve feature/control command ids (`enum FeatureReportMessageIDs` in SDL's
/// `controller_constants.h`; `ReportType` in InputPlumber's
/// `src/drivers/steam_deck/hid_report.rs`). Only the ones this module names.
pub mod cmd {
    /// `ID_GET_ATTRIBUTES_VALUES` — the attributes TLV blob.
    pub const GET_ATTRIBUTES_VALUES: u8 = 0x83;
    /// `ID_GET_STRING_ATTRIBUTE` — the unit serial number.
    pub const GET_STRING_ATTRIBUTE: u8 = 0xAE;
    /// `ID_GET_CHIP_ID`.
    pub const GET_CHIP_ID: u8 = 0xBA;
    /// `ReportType::InputData` — InputPlumber's initial `current_report`, and
    /// so this module's initial [`Profile::default_selector`].
    pub const INPUT_DATA: u8 = 0x09;
}

/// Which of the two identities a [`Profile`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Identity {
    /// The wired Steam Controller, `28de:1302`. Pass-through input. Default.
    #[default]
    Triton,
    /// The Steam Deck controller under InputPlumber's generic PID, `28de:12f0`.
    /// Translated input. The proven fallback.
    Deck,
}

impl Identity {
    /// Parse the `identity = …` config value.
    pub fn parse(s: &str) -> Result<Identity, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "triton" | "1302" | "wired" | "steam_controller" => Ok(Identity::Triton),
            "deck" | "12f0" | "neptune" | "steam_deck" => Ok(Identity::Deck),
            other => Err(format!("unknown gamepad identity '{other}' (triton|deck)")),
        }
    }

    /// The name this identity is written as in a config.
    pub fn as_str(self) -> &'static str {
        match self {
            Identity::Triton => "triton",
            Identity::Deck => "deck",
        }
    }

    /// The profile itself.
    pub fn profile(self) -> &'static Profile {
        match self {
            Identity::Triton => triton(),
            Identity::Deck => deck(),
        }
    }
}

/// How a profile's input reports are shaped on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    /// The controller's own vendor report `0x42`: 54 bytes, **report-id prefixed**
    /// (the `1302` descriptor numbers every report type, so all three
    /// `UHID_START` `dev_flags` bits are set and the prefix is required in both
    /// directions). Built by `translate::triton_report`.
    Triton,
    /// The Steam Deck's 64-byte packed report, **unprefixed** (the 38-byte
    /// descriptor has no `REPORT_ID` items, so no `dev_flags` bit is set).
    /// Built by `translate::deck_report`.
    Deck,
}

impl ReportKind {
    /// The exact wire length of one input report of this kind.
    pub fn report_len(self) -> usize {
        match self {
            ReportKind::Triton => crate::uhid::translate::TRITON_REPORT_LEN,
            ReportKind::Deck => crate::uhid::translate::DECK_REPORT_LEN,
        }
    }
}

/// How a profile's reports are framed on the wire.
///
/// This is the half of an identity that Steam's *parser* sees, as opposed to the
/// half `lsusb` sees, and the two profiles disagree about all of it.
///
/// # There is nothing to "turn on" — the descriptor is the request
///
/// Numbering is a property of the report descriptor, and the kernel derives it
/// itself. `uhid_hid_start` (`drivers/hid/uhid.c`) walks the descriptor it was
/// handed and reports the result *back* to user space:
///
/// ```text
/// if (hid->report_enum[HID_FEATURE_REPORT].numbered)
///         ev->u.start.dev_flags |= UHID_DEV_NUMBERED_FEATURE_REPORTS;
/// if (hid->report_enum[HID_OUTPUT_REPORT].numbered)
///         ev->u.start.dev_flags |= UHID_DEV_NUMBERED_OUTPUT_REPORTS;
/// if (hid->report_enum[HID_INPUT_REPORT].numbered)
///         ev->u.start.dev_flags |= UHID_DEV_NUMBERED_INPUT_REPORTS;
/// ```
///
/// `dev_flags` lives in `struct uhid_start_req`, which travels kernel → user
/// space; `struct uhid_create2_req` (`include/uapi/linux/uhid.h`) has **no flags
/// field at all**. So publishing the numbered 372-byte descriptor *is* how the
/// triton profile asks for numbered framing, and [`expected_dev_flags`] is the
/// answer the kernel is expected to give back — checked at runtime, not set.
///
/// [`expected_dev_flags`]: Framing::expected_dev_flags
///
/// # What user space must then do
///
/// uhid **neither inserts nor strips** the leading report-number byte on any
/// channel, so every byte on the wire is ours to get right:
///
/// * **Input** — `uhid_dev_input2` hands the buffer straight to
///   `hid_report_raw_event`, which takes `data[0]` as the report id whenever the
///   input enum is numbered. So a triton input report is `[0x42][53 bytes]` and
///   a deck one is a bare 64 bytes.
/// * **Feature `GET`** — `uhid_hid_get_report` answers with
///   `ret = min3(count, req->size, UHID_DATA_MAX); memcpy(buf, req->data, ret)`,
///   and `hidraw_get_report` copies that to the caller verbatim. A **real** USB
///   device is framed for us by `usbhid_get_raw_report` instead, which does
///   `buf[0] = report_number`, offsets the payload by one when the caller asked
///   for report 0, and then `if (ret > 0 && skipped_report_id) ret++`. That is
///   why a real `1302` answers Steam's 65-byte request with **65** bytes —
///   one echoed report-number byte plus a 64-byte Valve message — and why
///   [`Framing::feature_reply_len`] is 65 here. SDL's own client says the same
///   thing out loud: *"Firmware quirk: Set Feature and Get Feature requests
///   always require a 65-byte buffer"* (`src/joystick/hidapi/SDL_hidapi_steam.c`,
///   `unsigned char buf[65]`, `SDL_hid_get_feature_report(dev, uBuffer, 65)`),
///   and it reads the command id at `uBuffer[1]` — never `[0]`.
/// * **Feature `SET`** — `uhid_hid_set_report` copies hidraw's whole buffer,
///   report-number byte and all, so the Valve command id is `data[1]`. That is
///   what `crate::uhid::HostWrite::command` already strips for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framing {
    /// Whether this profile's descriptor carries `REPORT_ID` items at all. When
    /// it does, every report on every channel is prefixed with its id.
    pub numbered: bool,
    /// The input report id byte 0 carries, or `None` for an unnumbered profile.
    pub input_report_id: Option<u8>,
    /// Exact wire length of one input report, id byte included.
    pub input_len: usize,
    /// The feature report id Steam's control traffic rides on, or `None`.
    /// Valve multiplexes the whole command set through this one report, so it
    /// says nothing about *which* command — that is the byte after it.
    pub feature_report_id: Option<u8>,
    /// Exact wire length of one `GET_REPORT` answer: the report-number byte
    /// plus the Valve message. 65 for a numbered profile emulating what
    /// `usbhid_get_raw_report` would have produced; 64 for the deck profile,
    /// which is pinned to the proven probe's bytes.
    pub feature_reply_len: usize,
    /// The `dev_flags` `UHID_START` is expected to report for this descriptor.
    /// Diagnostic, never sent: see the type docs.
    pub expected_dev_flags: u64,
}

/// The triton descriptor numbers input, output *and* feature reports, so the
/// kernel raises all three bits.
const NUMBERED_ALL: u64 = crate::uhid::DEV_NUMBERED_FEATURE_REPORTS
    | crate::uhid::DEV_NUMBERED_OUTPUT_REPORTS
    | crate::uhid::DEV_NUMBERED_INPUT_REPORTS;

/// Everything that makes the virtual device one identity rather than the other.
///
/// All `'static`: a profile is a compile-time constant, so the running relay
/// holds a `&'static Profile` and never copies it.
#[derive(Debug)]
pub struct Profile {
    /// Which identity this is.
    pub identity: Identity,
    /// `create2.name` — becomes `HID_NAME`, and the Product string Steam shows.
    pub name: &'static str,
    /// `create2.phys` — `HID_PHYS`. Free-form; useful for finding the node.
    pub phys: &'static str,
    /// `create2.uniq` — `HID_UNIQ`, and **Steam's per-controller config key**:
    /// Steam writes `configset_<uniq>.vdf`. A changing value orphans configs, so
    /// it is pinned (§6 R11).
    pub uniq: &'static str,
    /// `create2.bus`. Always [`BUS_USB`]; see its docs for why not `BUS_VIRTUAL`.
    pub bus: u16,
    /// `create2.vendor`.
    pub vendor: u32,
    /// `create2.product`.
    pub product: u32,
    /// `create2.version` — the `bcdDevice` a real unit would report.
    pub version: u32,
    /// `create2.country`. Zero for both, as for every reference implementation.
    pub country: u32,
    /// The HID report descriptor published to the kernel.
    pub descriptor: &'static [u8],
    /// The shape of this profile's input reports.
    pub kind: ReportKind,
    /// How this profile's reports are framed on the wire.
    pub framing: Framing,
    /// Canned `GetAttributesValues` (`0x83`) answer, 64 bytes.
    pub attributes: [u8; 64],
    /// Serial reported by `GetStringAttribute` (`0xAE`).
    pub serial: &'static str,
    /// Chip id reported by `GetChipId` (`0xBA`).
    pub chip_id: [u8; 15],
    /// Which canned answer a `GET_REPORT` gets before Steam has selected one.
    pub default_selector: u8,
}

impl Profile {
    /// The canned `GET_REPORT` answer for `selector`, always exactly 64 bytes.
    ///
    /// The three handshake answers are InputPlumber's, verbatim, and were sent
    /// by the run Steam adopted. Anything else gets a **correctly framed** empty
    /// reply — `[0x00, <selector>, 0x00, …]`: right leading report-number byte,
    /// right command id, length zero. That is what
    /// `docs/research/uhid-steam-controller.md` A.8 prescribes ("correctly
    /// framed canned data … never `size = 0`"); the proven probe replied with a
    /// zero-length payload there instead, and Steam tolerated it, so this is the
    /// strictly safer of the two behaviours rather than a departure from what
    /// was tested.
    pub fn canned_reply(&self, selector: u8) -> [u8; 64] {
        match selector {
            cmd::GET_ATTRIBUTES_VALUES => self.attributes,
            // `[0x00, 0xAE, 0x14, 0x01]` + serial bytes, resized to 64.
            cmd::GET_STRING_ATTRIBUTE => {
                framed(&[0x00, cmd::GET_STRING_ATTRIBUTE, 0x14, 0x01], self.serial.as_bytes())
            }
            // `[0x00, 0xBA, 0x11, 0x00]` + the 15-byte chip id, resized to 64.
            cmd::GET_CHIP_ID => framed(&[0x00, cmd::GET_CHIP_ID, 0x11, 0x00], &self.chip_id),
            other => framed(&[0x00, other, 0x00], &[]),
        }
    }

    /// The exact bytes one `UHID_GET_REPORT` is answered with — the canned
    /// [`Profile::canned_reply`] frame, framed for this profile's wire.
    ///
    /// `rnum` is the report number the kernel passed through from
    /// `hidraw_get_report`, which read it out of byte 0 of Steam's own buffer.
    ///
    /// * **deck** — 64 bytes, the canned frame verbatim. The device is
    ///   unnumbered and this is the byte sequence Steam adopted on 2026-09-01;
    ///   it does not move.
    /// * **triton** — 65 bytes: `rnum` echoed into byte 0 exactly as
    ///   `usbhid_get_raw_report` does (`buf[0] = report_number`), then the
    ///   64-byte Valve message. The canned frame is 64 bytes of
    ///   `[report-number byte][63-byte message]`, so the message gets its full
    ///   64 bytes here and the reply is the length a real `1302` returns.
    ///   Nothing in the message content changes — only the frame around it.
    pub fn get_report_reply(&self, rnum: u8, selector: u8) -> Vec<u8> {
        let canned = self.canned_reply(selector);
        let mut out = vec![0u8; self.framing.feature_reply_len];
        let n = canned.len().min(out.len());
        out[..n].copy_from_slice(&canned[..n]);
        if self.framing.numbered {
            // A real device echoes the requested report number here; Steam asks
            // with 0, which is what the canned frame already carries, so this
            // only ever differs if Steam names the feature report explicitly.
            out[0] = rnum;
        }
        out
    }

    /// The `28de:12f0`-style `<vid>-<pid>` string Steam falls back to when a
    /// unit reports no serial. Handy for logs and the design doc.
    pub fn vid_pid(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.product)
    }
}

/// Header bytes followed by a payload, zero-padded to a 64-byte report.
fn framed(header: &[u8], payload: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    let h = header.len().min(64);
    out[..h].copy_from_slice(&header[..h]);
    let n = payload.len().min(64 - h);
    out[h..h + n].copy_from_slice(&payload[..n]);
    out
}

// ---------------------------------------------------------------------------
// Profile: deck — the proven one
// ---------------------------------------------------------------------------

/// InputPlumber's Steam Deck controller descriptor: 38 bytes, pure vendor page
/// `0xFFFF`, **no `REPORT_ID` items**, one 64-byte input and one 64-byte feature.
///
/// Verbatim from `ShadowBlip/InputPlumber`
/// `src/drivers/steam_deck/report_descriptor.rs` (`CONTROLLER_DESCRIPTOR`), via
/// `scripts/research/uhid_active_probe.py`, which is the copy Steam adopted.
/// Because nothing here is numbered, all three `UHID_START` numbered-report
/// flags stay clear and reports are unprefixed in both directions.
pub const DECK_DESCRIPTOR: [u8; 38] = [
    0x06, 0xff, 0xff, // Usage Page (Vendor 0xFFFF)
    0x09, 0x01, // Usage (0x01)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x02, 0x09, 0x03, //   Usage 0x02, Usage 0x03
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x40, //   Report Count (64)
    0x81, 0x02, //   Input (Data,Var,Abs)
    0x09, 0x06, 0x09, 0x07, //   Usage 0x06, Usage 0x07
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x40, //   Report Count (64)
    0xb1, 0x02, //   Feature (Data,Var,Abs)
    0xc0,       // End Collection
];

/// The `GetAttributesValues` blob a real Steam Deck sends, verbatim.
///
/// From InputPlumber `steam_deck_uhid.rs`, whose in-source comment is: *"No idea
/// what these bytes mean, but this is what is sent from the real device."*
/// `0x2d` = 45 is the payload length; the body is a run of `attr_id, u32`
/// little-endian TLVs — `0x01` (`ATTRIB_PRODUCT_ID`, here `0x00001205`, the real
/// Deck's PID), `0x02`, `0x0a`, `0x09`, `0x0b`, `0x0d`, `0x0c`, `0x0e`.
pub const DECK_ATTRIBUTES: [u8; 64] = [
    0x00, 0x83, 0x2d, 0x01, 0x05, 0x12, 0x00, 0x00, 0x02, 0x00, //
    0x00, 0x00, 0x00, 0x0a, 0x2b, 0x12, 0xa9, 0x62, 0x04, 0xad, //
    0xf1, 0xe4, 0x65, 0x09, 0x2e, 0x00, 0x00, 0x00, 0x0b, 0xa0, //
    0x0f, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, //
    0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00,
];

/// InputPlumber's default chip id filler, `[0,1,2,…,9,0,1,2,3,4]`.
const DECK_CHIP_ID: [u8; 15] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4];

/// The `deck` profile's unit serial — its `uniq`, and the string its
/// `GetStringAttribute` answers with.
///
/// # Why it exists at all
///
/// InputPlumber leaves `uniq` empty and so did this profile, and on 2026-09-02
/// Steam said what it thinks of that, one line after adopting the fake:
///
/// ```text
/// Local Device Found
///   type: 28de 12f0
///   path: /dev/hidraw12
///   serial_number:  - 0
/// !! Steam controller device opened for index 0.
/// Controller has an Invalid or missing unit serial number, setting to '28de-12f0-3147b8f'
/// ```
///
/// **The field it read is `uniq`, not the `0xAE` answer.** Three things settle
/// that: the `serial_number:` line of the enumeration block comes from hidapi's
/// hidraw backend, which parses `HID_UNIQ` out of the node's `uevent`; the
/// complaint lands *before* any feature round trip, immediately on open; and the
/// `triton` profile, whose `uniq` is set, is enumerated as
/// `serial_number: FXA0000000001` and draws no complaint at all. The canned
/// `0xAE` answer already said `1NPU7PLUMB3R` throughout and changed nothing.
///
/// # Why this string
///
/// Not a hardware serial. Baking a real unit's serial into the source would
/// ship one machine's hardware identity to everyone who builds hyprpad — and
/// would make the `deck` and `triton` identities collide on one Steam config
/// key, so a Deck-shaped binding set would be applied to a Triton-shaped device.
/// (`FXA0000000001` and `FXB0000000002`, the controller's and the puck's serials
/// as this tree states them, are themselves synthetic stand-ins of the captured
/// shape — see `TRITON_SERIAL`.)
///
/// A fixed synthetic is what the field actually needs to be: **stable**, because
/// Steam keys `configset_<serial>.vdf` on it and a binding set must survive a
/// daemon restart; **distinct** from `triton`'s, so the two identities keep
/// separate configs; and **obviously ours**, so anyone reading a Steam log knows
/// what they are looking at. `1NPU7PLUMB3R` — InputPlumber's own joke serial,
/// which this profile answers `0xAE` with — is none of the first two.
///
/// # The cost, which the owner should know about
///
/// Steam's config key for this identity changes from the `28de-12f0-3147b8f` it
/// invented to one derived from this string. **Any Steam Input binding already
/// saved against the old key is orphaned** and has to be redone once. That is a
/// one-time cost on a fallback identity, paid to stop Steam inventing a fresh
/// key it might not invent identically on another machine — and it is why this
/// value must never change again.
pub const DECK_SERIAL: &str = "HYPRPAD-DECK-0001";

/// The **proven** identity: an emulated Steam Deck controller under
/// InputPlumber's non-Deck `Generic` PID.
///
/// Every field is the proven probe's, byte for byte
/// (`scripts/research/uhid_active_probe.py`; `docs/research/uhid-steam-controller.md`
/// "PROVEN on-device (2026-09-01)"). Steam opened it, logged
/// `!! Steam controller device opened for index 5.`, loaded
/// `configset_neptune.vdf` for it, and sent it 39 `SetSettingsValues` writes.
///
/// **One field departs from the probe: `uniq`.** It was empty, as InputPlumber
/// leaves it, and Steam answered `Controller has an Invalid or missing unit
/// serial number, setting to '28de-12f0-3147b8f'` — inventing a key of its own
/// rather than using one this profile controls. It now carries
/// [`DECK_SERIAL`], which is also what `GetStringAttribute` answers, so the
/// device does not state two different serials on two channels the way it used
/// to. See [`DECK_SERIAL`] for which field Steam actually read, and for what
/// the change costs. Everything else — descriptor, attributes blob, chip id,
/// framing, PID, version — is still the proven run's, byte for byte.
pub fn deck() -> &'static Profile {
    static DECK: Profile = Profile {
        identity: Identity::Deck,
        name: "Steam Controller",
        phys: "",
        uniq: DECK_SERIAL,
        bus: BUS_USB,
        vendor: VID_VALVE,
        product: 0x12f0,
        version: 0x1000,
        country: 0,
        descriptor: &DECK_DESCRIPTOR,
        kind: ReportKind::Deck,
        // Nothing in the 38-byte descriptor is numbered, so the kernel raises
        // no `dev_flags` bit and every report is bare. The 64-byte answer is
        // one byte shorter than the 65 a real USB device would return, and is
        // kept anyway: these are the bytes Steam adopted.
        framing: Framing {
            numbered: false,
            input_report_id: None,
            input_len: crate::uhid::translate::DECK_REPORT_LEN,
            feature_report_id: None,
            feature_reply_len: 64,
            expected_dev_flags: 0,
        },
        attributes: DECK_ATTRIBUTES,
        serial: DECK_SERIAL,
        chip_id: DECK_CHIP_ID,
        default_selector: cmd::INPUT_DATA,
    };
    &DECK
}

// ---------------------------------------------------------------------------
// Profile: triton — the least-translation one
// ---------------------------------------------------------------------------

/// The captured 372-byte report descriptor of a real wired `28de:1302`.
///
/// Embedded straight from the committed capture so there is exactly one copy of
/// these bytes in the tree: `docs/research/assets/triton-wired-1302-report-descriptor.bin`,
/// dumped from that unit's `/sys/class/hidraw/hidrawN/device/report_descriptor`.
/// Its three top-level collections are the lizard Mouse (`0x40`), the lizard
/// Keyboard (`0x41`), and the vendor page `0xFF00` carrying the Steam protocol —
/// including **input `0x42` with a 53-byte payload**, which is exactly the report
/// `src/report.rs` decodes off the controller. That identity is what makes the triton
/// input path a pass-through.
pub const TRITON_DESCRIPTOR: [u8; 372] =
    *include_bytes!("../../docs/research/assets/triton-wired-1302-report-descriptor.bin");

/// The wired unit's serial, in the slot the captured one occupied
/// (`docs/research/assets/triton-wired-1302-identity.md`). Pinned as `uniq`
/// because Steam keys `configset_<uniq>.vdf` on it (§3.3, §6 R11) — a Steam
/// Input config bound to this string survives every restart of the daemon.
///
/// **Synthetic.** The captured unit's real serial is not published; this is a
/// stand-in of the same shape (13 characters, `FXA` prefix), which is all the
/// `uniq` field and the 19 bytes inside the `0xAE` payload care about.
pub const TRITON_SERIAL: &str = "FXA0000000001";

/// The feature report the wired `1302` multiplexes Valve's whole command set
/// through: id `0x01`, 63 payload bytes, vendor page `0xFF00`. Read straight off
/// the captured descriptor (`the_triton_descriptor_report_table_is_pinned`); its
/// twin `0x02` is the same shape and Steam does not use it.
pub const TRITON_FEATURE_REPORT_ID: u8 = 0x01;

/// Replace `ATTRIB_PRODUCT_ID` (attribute `0x01`, the first TLV, its `u32` at
/// bytes 4..8) in an attributes blob.
///
/// Risk R3 in the research doc: the device claims one PID while the attributes
/// blob answers with another. Patching this single `u32` is the mitigation the
/// doc prescribes.
const fn with_product_id(mut blob: [u8; 64], pid: u32) -> [u8; 64] {
    let le = pid.to_le_bytes();
    blob[4] = le[0];
    blob[5] = le[1];
    blob[6] = le[2];
    blob[7] = le[3];
    blob
}

/// `GetAttributesValues` for the triton profile.
///
/// **UNVERIFIED.** No `1302` attributes blob was ever captured — the proven
/// probe only ever answered as a Deck. This is the Deck blob with
/// `ATTRIB_PRODUCT_ID` patched from `0x1205` to `0x1302` so the answer is at
/// least self-consistent with the identity the device claims (§6 R3). Steam's
/// own log shows it survives a *failed* attribute probe on a real Triton
/// (`Deck Controller PCB Serial# invalid: NA`, queried, failed, tolerated), so a
/// wrong-but-well-formed answer here is a low-severity risk. Replace it the
/// moment a real `1302` blob is captured.
pub const TRITON_ATTRIBUTES: [u8; 64] = with_product_id(DECK_ATTRIBUTES, 0x1302);

/// The **default** identity: a wired single-interface Steam Controller.
///
/// Identity fields from the capture in
/// `docs/research/assets/triton-wired-1302-identity.md`, with one deliberate
/// departure: that note suggested creating on `BUS_VIRTUAL` so Valve's
/// `60-steam-input.rules` `000[356]:28DE:*` line would match. It is superseded —
/// §2 of the research doc shows SDL drops any bus that is not USB/Bluetooth/
/// I2C/SPI, the rule matches `0003` just as happily, and the proven run
/// established `BUS_USB` on-device.
pub fn triton() -> &'static Profile {
    static TRITON: Profile = Profile {
        identity: Identity::Triton,
        name: "Valve Software Steam Controller",
        phys: "hyprpad-uhid/1302",
        uniq: TRITON_SERIAL,
        bus: BUS_USB,
        vendor: VID_VALVE,
        product: 0x1302,
        // bcdDevice 0x0307 == release 307, exactly what Steam's log recorded
        // for the real wired unit on this machine.
        version: 0x0307,
        country: 0,
        descriptor: &TRITON_DESCRIPTOR,
        kind: ReportKind::Triton,
        // Straight off the captured descriptor: input `0x42` is 53 payload
        // bytes (54 on the wire), the Valve control channel is feature report
        // `0x01` (63 payload bytes), and every report type is numbered, so the
        // kernel raises all three `UHID_DEV_NUMBERED_*` bits. The 65-byte
        // answer is what `usbhid_get_raw_report` builds for a real unit.
        framing: Framing {
            numbered: true,
            input_report_id: Some(crate::uhid::translate::REPORT_ID_INPUT),
            input_len: crate::uhid::translate::TRITON_REPORT_LEN,
            feature_report_id: Some(TRITON_FEATURE_REPORT_ID),
            feature_reply_len: 65,
            expected_dev_flags: NUMBERED_ALL,
        },
        attributes: TRITON_ATTRIBUTES,
        serial: TRITON_SERIAL,
        // UNVERIFIED: no chip id was captured from a 1302. Modelled on the Deck
        // answer, as the coordinator's brief prescribes for exactly this case.
        chip_id: DECK_CHIP_ID,
        default_selector: cmd::INPUT_DATA,
    };
    &TRITON
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Repository root, so a test can read the committed research assets.
    fn repo(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// Pull one `NAME = bytes([ … ])` literal out of the proven probe script.
    ///
    /// The probe is the artefact Steam actually adopted, so comparing against
    /// the file itself — rather than a second transcription — is what makes
    /// "byte-for-byte" mean anything.
    fn probe_bytes(name: &str) -> Vec<u8> {
        let src = std::fs::read_to_string(repo("scripts/research/uhid_active_probe.py"))
            .expect("the proven probe is committed");
        let start = src.find(&format!("{name} = bytes([")).unwrap_or_else(|| {
            panic!("{name} not found in uhid_active_probe.py");
        });
        let body = &src[start..];
        let open = body.find('[').expect("[");
        let close = body.find("])").expect("])");
        body[open + 1..close]
            // Strip the probe's inline `# …` comments *first*: they contain
            // commas of their own ("Input (Data,Var,Abs)").
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ")
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| match t {
                // The probe writes byte 1 of the attributes blob as the symbol
                // it defines just above it, not as a literal.
                "RT_GET_ATTRIBUTES" => cmd::GET_ATTRIBUTES_VALUES,
                "RT_GET_STRING_ATTR" => cmd::GET_STRING_ATTRIBUTE,
                "RT_GET_CHIP_ID" => cmd::GET_CHIP_ID,
                "RT_INPUT_DATA" => cmd::INPUT_DATA,
                lit => {
                    let hex = lit.trim_start_matches("0x");
                    u8::from_str_radix(hex, 16).unwrap_or_else(|e| panic!("{lit:?}: {e}"))
                }
            })
            .collect::<Vec<u8>>()
    }

    #[test]
    fn deck_descriptor_matches_the_proven_probe_byte_for_byte() {
        let want = probe_bytes("CONTROLLER_DESCRIPTOR");
        assert_eq!(want.len(), 38);
        assert_eq!(DECK_DESCRIPTOR.as_slice(), want.as_slice());
        assert_eq!(deck().descriptor, want.as_slice());
    }

    #[test]
    fn deck_attributes_blob_matches_the_proven_probe_byte_for_byte() {
        let want = probe_bytes("ATTRIBUTES_BLOB");
        assert_eq!(want.len(), 64);
        assert_eq!(DECK_ATTRIBUTES.as_slice(), want.as_slice());
        // The length byte is the payload length, and the first TLV is the
        // product id — the two facts `with_product_id` depends on.
        assert_eq!(DECK_ATTRIBUTES[1], cmd::GET_ATTRIBUTES_VALUES);
        assert_eq!(DECK_ATTRIBUTES[2], 0x2d);
        assert_eq!(DECK_ATTRIBUTES[3], 0x01, "ATTRIB_PRODUCT_ID");
        assert_eq!(u32::from_le_bytes(DECK_ATTRIBUTES[4..8].try_into().unwrap()), 0x1205);
    }

    #[test]
    fn triton_descriptor_matches_the_captured_asset_byte_for_byte() {
        let want =
            std::fs::read(repo("docs/research/assets/triton-wired-1302-report-descriptor.bin"))
                .expect("the capture is committed");
        assert_eq!(want.len(), 372);
        assert_eq!(TRITON_DESCRIPTOR.as_slice(), want.as_slice());
    }

    /// The report descriptor read off the live Bluetooth node
    /// (`/sys/class/hidraw/hidraw13/device/report_descriptor`, `0005:28DE:1303`,
    /// 2026-09-03, read-only) is **byte-identical to the wired `1302`'s**.
    ///
    /// That is a much stronger result than phase 1 needed, and it is the reason
    /// the port is a length guard rather than a second protocol. The controller
    /// publishes one HID interface description regardless of the wire it is on:
    /// same two lizard collections, same vendor collection, same report ids at
    /// the same sizes, same feature and output channels. The link changes which
    /// of those reports the firmware chooses to *send* — `0x45` over BLE where
    /// the dongle sends `0x42` — and nothing else.
    ///
    /// Two consequences worth stating, because both are load-bearing:
    ///
    /// * the fake hyprpad publishes to Steam is already a faithful description
    ///   of a Bluetooth controller, so the relay needs no second profile;
    /// * `src/lizard.rs`'s feature channel (`0x01`) and `src/haptics.rs`'s
    ///   output channels (`0x80`/`0x81`) are declared identically on both, which
    ///   is the descriptor-level half of research §2.3's "unchanged and
    ///   unchunked".
    #[test]
    fn the_bluetooth_descriptor_is_the_same_device_description_as_the_wired_one() {
        let bt = std::fs::read(repo("docs/research/assets/triton-bt-1303-report-descriptor.bin"))
            .expect("the Bluetooth capture is committed");
        assert_eq!(bt.len(), 372, "same length as the wired capture");
        assert_eq!(
            bt.as_slice(),
            TRITON_DESCRIPTOR.as_slice(),
            "the controller describes itself identically on both transports"
        );
    }

    /// **The Bluetooth report table, pinned on its own.**
    ///
    /// Deliberately not folded into the equality test above: if a future
    /// firmware or kernel ever makes the two descriptors diverge, this must
    /// still say what the Bluetooth link declares — and in particular that
    /// `0x45` is 45 payload bytes, i.e. the 46 on the wire that
    /// `report::Frame::decode` accepts and `translate::controller_to_triton`
    /// re-frames.
    #[test]
    fn the_bluetooth_descriptor_declares_the_0x45_report_at_46_bytes_on_the_wire() {
        let bt = std::fs::read(repo("docs/research/assets/triton-bt-1303-report-descriptor.bin"))
            .expect("the Bluetooth capture is committed");
        let items = report_items(&bt);

        // The report the link actually streams, measured at 133.5 Hz.
        let input_45 = items
            .iter()
            .find(|i| i.main == 0x81 && i.report_id == 0x45)
            .expect("an input report 0x45");
        assert_eq!(input_45.payload_bytes, 45);
        assert_eq!(input_45.payload_bytes + 1, crate::report::REPORT_LEN_INPUT_BLE);

        // And the long form, which is what the fake re-frames into.
        let input_42 = items
            .iter()
            .find(|i| i.main == 0x81 && i.report_id == 0x42)
            .expect("an input report 0x42");
        assert_eq!(input_42.payload_bytes, 53);
        assert_eq!(input_42.payload_bytes + 1, crate::report::REPORT_LEN_INPUT);
        // The whole re-framing claim in one line: 0x42 is 0x45 plus the
        // quaternion, so the difference is exactly eight bytes.
        assert_eq!(input_42.payload_bytes - input_45.payload_bytes, 8);

        // The battery report the reader drops, at the length it was captured.
        let input_43 = items
            .iter()
            .find(|i| i.main == 0x81 && i.report_id == 0x43)
            .expect("an input report 0x43");
        assert_eq!(input_43.payload_bytes + 1, 15, "matches tests/data/bt-0x45.hex");

        // The lizard mouse, which is ON over Bluetooth until src/lizard.rs
        // turns it off — 6 bytes on the wire, and dropped by the decoder.
        let input_40 = items
            .iter()
            .find(|i| i.main == 0x81 && i.report_id == 0x40)
            .expect("an input report 0x40");
        assert_eq!(input_40.payload_bytes + 1, 6);

        // The write channels lizard mode and haptics use, declared exactly as
        // they are over USB — research §2.3, at descriptor level.
        let feat_1 = items
            .iter()
            .find(|i| i.main == 0xb1 && i.report_id == 0x01)
            .expect("feature 0x01 — the Valve control channel");
        assert_eq!(feat_1.payload_bytes, 63, "the 64-byte frame src/lizard.rs sends");
        let out_80 = items.iter().find(|i| i.main == 0x91 && i.report_id == 0x80).expect("0x80");
        assert_eq!(out_80.payload_bytes + 1, 10, "rumble");
        let out_81 = items.iter().find(|i| i.main == 0x91 && i.report_id == 0x81).expect("0x81");
        assert_eq!(out_81.payload_bytes + 1, 8, "haptic pulse");

        // Whole-table equality with the wired capture, so a diverging future
        // firmware fails loudly rather than in one row nobody checked.
        assert_eq!(table(&bt), table(&TRITON_DESCRIPTOR));
    }

    /// Walk the captured descriptor and confirm the single fact the whole
    /// triton design rests on: its vendor input report `0x42` is 53 payload
    /// bytes, i.e. 54 on the wire — identical to what the controller streams and
    /// `report::Frame::decode` accepts. If this ever fails, the pass-through is
    /// no longer a pass-through.
    #[test]
    fn the_triton_descriptor_declares_the_controllers_own_0x42_report() {
        let items = report_items(&TRITON_DESCRIPTOR);
        let input_42 = items
            .iter()
            .find(|i| i.main == 0x81 && i.report_id == 0x42)
            .expect("an input report 0x42");
        assert_eq!(input_42.payload_bytes, 53);
        assert_eq!(input_42.payload_bytes + 1, crate::uhid::translate::TRITON_REPORT_LEN);

        // The feature channel `src/lizard.rs` already writes: id 0x01, 63+1.
        let feat_1 = items
            .iter()
            .find(|i| i.main == 0xb1 && i.report_id == 0x01)
            .expect("a feature report 0x01");
        assert_eq!(feat_1.payload_bytes, 63);

        // And the haptics channels `src/haptics.rs` already writes.
        let out_80 = items.iter().find(|i| i.main == 0x91 && i.report_id == 0x80).expect("0x80");
        assert_eq!(out_80.payload_bytes, 9, "0x80 rumble is 10 bytes on the wire");
        let out_81 = items.iter().find(|i| i.main == 0x91 && i.report_id == 0x81).expect("0x81");
        assert_eq!(out_81.payload_bytes, 7, "0x81 pulse is 8 bytes on the wire");

        // Every report type carries REPORT_ID items, so all three UHID_START
        // numbered-report flags will be set and prefixes are required.
        assert!(items.iter().any(|i| i.main == 0x81));
        assert!(items.iter().any(|i| i.main == 0x91));
        assert!(items.iter().any(|i| i.main == 0xb1));
        assert!(items.iter().all(|i| i.report_id != 0), "no unnumbered report anywhere");
    }

    /// The Deck descriptor is the mirror image: one input, one feature, both 64
    /// bytes, and **no report ids at all**.
    #[test]
    fn the_deck_descriptor_is_unnumbered_and_64_bytes_each_way() {
        let items = report_items(&DECK_DESCRIPTOR);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.report_id == 0), "no REPORT_ID items");
        assert!(items.iter().all(|i| i.payload_bytes == 64));
        assert_eq!(items[0].main, 0x81, "input");
        assert_eq!(items[1].main, 0xb1, "feature");
        assert_eq!(items[0].payload_bytes, crate::uhid::translate::DECK_REPORT_LEN);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Item {
        /// The main-item tag: 0x81 Input, 0x91 Output, 0xb1 Feature.
        main: u8,
        report_id: u8,
        payload_bytes: usize,
    }

    /// A minimal HID report-descriptor walker: enough to recover
    /// `(main item, report id, payload size)` for every report the descriptor
    /// declares, in the order it declares them.
    ///
    /// One report is usually built from several main items, so the fields are
    /// accumulated **in bits** and rounded once at the end — exactly the
    /// kernel's `hid_compute_report_size`, `((report->size - 1) >> 3) + 1`.
    /// Summing byte-rounded main items instead loses the lizard mouse's
    /// `2 + 6`-bit button byte, and the whole point of this walker is that the
    /// numbers it prints are the numbers on the wire.
    fn report_items(rd: &[u8]) -> Vec<Item> {
        let (mut id, mut count, mut size) = (0u8, 0usize, 0usize);
        // (main, id) in first-seen order, with the running bit total.
        let mut order: Vec<(u8, u8)> = Vec::new();
        let mut bits: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < rd.len() {
            let b = rd[i];
            // Short items: the low two bits are the data length, and `3` means
            // four bytes (HID 1.11 §6.2.2.2).
            let len = match b & 3 {
                3 => 4,
                n => usize::from(n),
            };
            let val = rd[i + 1..(i + 1 + len).min(rd.len())]
                .iter()
                .rev()
                .fold(0usize, |a, &x| (a << 8) | usize::from(x));
            match b & 0xfc {
                0x84 => id = val as u8, // Report ID
                0x94 => count = val,    // Report Count
                0x74 => size = val,     // Report Size
                _ => {}
            }
            if matches!(b, 0x81 | 0x91 | 0xb1) {
                let key = (b, id);
                let at = order.iter().position(|k| *k == key).unwrap_or_else(|| {
                    order.push(key);
                    bits.push(0);
                    order.len() - 1
                });
                bits[at] += count * size;
            }
            i += 1 + len;
        }
        order
            .into_iter()
            .zip(bits)
            .map(|((main, report_id), n)| Item {
                main,
                report_id,
                payload_bytes: if n == 0 { 0 } else { ((n - 1) >> 3) + 1 },
            })
            .collect()
    }

    /// A row of the pinned tables below: `(type, report id, payload bytes)`.
    fn table(rd: &[u8]) -> Vec<(&'static str, u8, usize)> {
        report_items(rd)
            .into_iter()
            .map(|i| {
                let ty = match i.main {
                    0x81 => "input",
                    0x91 => "output",
                    _ => "feature",
                };
                (ty, i.report_id, i.payload_bytes)
            })
            .collect()
    }

    /// **The whole captured `1302` report table, pinned.**
    ///
    /// Every framing decision the triton profile makes is read off this list:
    /// which id the input stream carries and how long it is, which feature
    /// report Valve's command set is multiplexed through, and — because *every*
    /// row is numbered — that the kernel will number all three channels. A
    /// re-capture that changes any of it must fail here rather than silently
    /// reframing the wire.
    ///
    /// Wire length is always one more than the payload: the report id goes in
    /// front of it.
    #[test]
    fn the_triton_descriptor_report_table_is_pinned() {
        assert_eq!(
            table(&TRITON_DESCRIPTOR),
            vec![
                // Collection 1: the lizard Mouse (Generic Desktop / Pointer).
                ("input", 0x40, 5),
                // Collection 2: the lizard Keyboard.
                ("input", 0x41, 8),
                // Collection 3: vendor page 0xFF00 — the Steam protocol.
                ("input", 0x42, 53), // controller state: THE pass-through
                ("input", 0x44, 5),
                ("input", 0x79, 1),
                ("input", 0x43, 14),
                ("input", 0x7b, 12),
                ("input", 0x45, 45),
                ("output", 0x80, 9), // rumble      — src/haptics.rs writes it
                ("output", 0x81, 7), // haptic pulse — likewise
                ("output", 0x82, 3),
                ("output", 0x83, 9),
                ("output", 0x84, 8),
                ("output", 0x85, 3),
                ("output", 0x86, 3),
                ("output", 0x87, 63),
                ("output", 0x89, 63),
                ("output", 0x88, 63),
                ("feature", 0x01, 63), // the Valve control channel
                ("feature", 0x02, 63),
            ]
        );
    }

    /// The deck table is the mirror image, and just as load-bearing: two
    /// unnumbered 64-byte reports, so no `dev_flags` bit and no prefix byte
    /// anywhere.
    #[test]
    fn the_deck_descriptor_report_table_is_pinned() {
        assert_eq!(table(&DECK_DESCRIPTOR), vec![("input", 0x00, 64), ("feature", 0x00, 64)]);
    }

    /// Each profile's declared [`Framing`] is exactly what its own descriptor
    /// says — including the `dev_flags` the kernel will hand back in
    /// `UHID_START`, which is derived and checked, never sent.
    #[test]
    fn each_profiles_framing_is_derived_from_its_own_descriptor() {
        for p in [triton(), deck()] {
            let items = report_items(p.descriptor);
            let f = p.framing;

            let numbered = |main: u8| items.iter().any(|i| i.main == main && i.report_id != 0);
            assert_eq!(
                f.numbered,
                items.iter().any(|i| i.report_id != 0),
                "{}: numbered",
                p.identity.as_str()
            );
            // `uhid_hid_start` raises one bit per *report type* that is
            // numbered, so build the expectation the same way it does.
            let mut want = 0u64;
            if numbered(0xb1) {
                want |= crate::uhid::DEV_NUMBERED_FEATURE_REPORTS;
            }
            if numbered(0x91) {
                want |= crate::uhid::DEV_NUMBERED_OUTPUT_REPORTS;
            }
            if numbered(0x81) {
                want |= crate::uhid::DEV_NUMBERED_INPUT_REPORTS;
            }
            assert_eq!(f.expected_dev_flags, want, "{}: dev_flags", p.identity.as_str());

            // The input report the streamer actually emits.
            let input = items
                .iter()
                .find(|i| i.main == 0x81 && i.report_id == f.input_report_id.unwrap_or(0))
                .expect("the streamed input report is declared");
            assert_eq!(
                f.input_len,
                input.payload_bytes + usize::from(f.numbered),
                "{}: input wire length",
                p.identity.as_str()
            );
            assert_eq!(f.input_len, p.kind.report_len(), "{}: kind agrees", p.identity.as_str());

            // The feature report Valve's control traffic rides on.
            let feature = items
                .iter()
                .find(|i| i.main == 0xb1 && i.report_id == f.feature_report_id.unwrap_or(0))
                .expect("the control feature report is declared");
            assert_eq!(feature.payload_bytes, if f.numbered { 63 } else { 64 });
        }

        assert_eq!(triton().framing.expected_dev_flags, 1 | 2 | 4, "all three, numerically");
        assert_eq!(deck().framing.expected_dev_flags, 0);
        assert_eq!(triton().framing.input_report_id, Some(0x42));
        assert_eq!(deck().framing.input_report_id, None);
        assert_eq!(triton().framing.feature_report_id, Some(TRITON_FEATURE_REPORT_ID));
        assert_eq!(deck().framing.feature_report_id, None);
    }

    #[test]
    fn the_two_identities_differ_in_exactly_the_documented_ways() {
        let (t, d) = (triton(), deck());
        assert_eq!((t.vendor, t.product), (0x28de, 0x1302));
        assert_eq!((d.vendor, d.product), (0x28de, 0x12f0));
        assert_eq!(t.bus, d.bus, "both are BUS_USB — never BUS_VIRTUAL");
        assert_eq!(t.bus, BUS_USB);
        assert_eq!(t.kind, ReportKind::Triton);
        assert_eq!(d.kind, ReportKind::Deck);
        assert_eq!(t.uniq, "FXA0000000001", "pinned so Steam's config key is stable");
        assert_eq!(d.uniq, DECK_SERIAL, "pinned for the same reason — see DECK_SERIAL");
        assert_ne!(t.uniq, d.uniq, "the two identities must not share a Steam config key");
        assert!(!d.uniq.is_empty(), "an empty uniq is what Steam called an invalid unit serial");
        assert_eq!(t.vid_pid(), "28de:1302");
        assert_eq!(d.vid_pid(), "28de:12f0");
        // Never the controller's own PID: the bInterfaceNumber slot gate (§3.2).
        assert_ne!(t.product, 0x1304);
        assert_ne!(d.product, 0x1304);
    }

    #[test]
    fn identity_parses_its_config_spellings_and_rejects_typos() {
        assert_eq!(Identity::parse("triton"), Ok(Identity::Triton));
        assert_eq!(Identity::parse(" DECK "), Ok(Identity::Deck));
        assert_eq!(Identity::parse("1302"), Ok(Identity::Triton));
        assert_eq!(Identity::parse("neptune"), Ok(Identity::Deck));
        assert_eq!(Identity::default(), Identity::Triton, "triton is the default");
        assert_eq!(Identity::Triton.as_str(), "triton");
        assert_eq!(Identity::Deck.as_str(), "deck");
        assert_eq!(Identity::Triton.profile().identity, Identity::Triton);
        assert!(Identity::parse("1304").unwrap_err().contains("triton|deck"));
    }

    #[test]
    fn the_three_canned_answers_are_the_proven_bytes() {
        let d = deck();
        assert_eq!(d.canned_reply(cmd::GET_ATTRIBUTES_VALUES), DECK_ATTRIBUTES);

        // The probe's framing, verbatim — `[0x00, 0xAE, 0x14, 0x01]` — with
        // this profile's own serial as the payload. `0x01` is
        // `ATTRIB_STR_UNIT_SERIAL` (SDL `controller_constants.h`), which is
        // exactly the attribute Steam's "Invalid or missing unit serial
        // number" names, and `0x14` = 20 is the declared payload length: the
        // attribute selector plus up to 19 serial bytes.
        let serial = d.canned_reply(cmd::GET_STRING_ATTRIBUTE);
        assert_eq!(&serial[..4], &[0x00, 0xAE, 0x14, 0x01], "framing is the proven probe's");
        assert_eq!(&serial[4..4 + DECK_SERIAL.len()], DECK_SERIAL.as_bytes());
        assert!(serial[4 + DECK_SERIAL.len()..].iter().all(|&b| b == 0));
        assert!(
            DECK_SERIAL.len() <= 19,
            "the serial must fit inside the declared 0x14-byte payload"
        );

        // The probe: `[0x00, 0xBA, 0x11, 0x00] + chip_id`, resized to 64.
        let chip = d.canned_reply(cmd::GET_CHIP_ID);
        assert_eq!(&chip[..4], &[0x00, 0xBA, 0x11, 0x00]);
        assert_eq!(&chip[4..19], &DECK_CHIP_ID);
        assert!(chip[19..].iter().all(|&b| b == 0));
    }

    #[test]
    fn an_unselected_query_still_gets_a_full_correctly_framed_report() {
        // Never `size = 0`: right leading zero, right command id, length zero.
        let r = deck().canned_reply(cmd::INPUT_DATA);
        assert_eq!(r.len(), 64);
        assert_eq!(&r[..3], &[0x00, cmd::INPUT_DATA, 0x00]);
        assert!(r[3..].iter().all(|&b| b == 0));
        assert_eq!(deck().default_selector, cmd::INPUT_DATA, "InputPlumber's initial state");
    }

    #[test]
    fn triton_answers_with_its_own_serial_and_a_self_consistent_product_id() {
        let t = triton();
        let serial = t.canned_reply(cmd::GET_STRING_ATTRIBUTE);
        assert_eq!(&serial[4..17], b"FXA0000000001");
        // R3: the attributes answer must not contradict the claimed identity.
        let attrs = t.canned_reply(cmd::GET_ATTRIBUTES_VALUES);
        assert_eq!(u32::from_le_bytes(attrs[4..8].try_into().unwrap()), 0x1302);
        assert_eq!(attrs[3], 0x01, "still ATTRIB_PRODUCT_ID");
        // Everything after the patched u32 is untouched Deck data.
        assert_eq!(&attrs[8..], &DECK_ATTRIBUTES[8..]);
    }

    /// The framed `GET_REPORT` answer, which is what Steam's parser actually
    /// reads.
    ///
    /// A real wired `1302` is framed by `usbhid_get_raw_report`: `buf[0] =
    /// report_number`, the device's payload from `buf[1]`, and `ret++` for the
    /// echoed id — so Steam's 65-byte request comes back **65 bytes**, and SDL
    /// reads the Valve command id at `uBuffer[1]`. uhid inserts nothing
    /// (`uhid_hid_get_report` is a bare `memcpy` of what user space sent), so
    /// the profile has to produce that frame itself.
    #[test]
    fn a_triton_get_report_answer_is_65_bytes_with_the_id_byte_in_front() {
        let t = triton();
        let r = t.get_report_reply(0x00, cmd::GET_ATTRIBUTES_VALUES);
        assert_eq!(r.len(), 65, "one report-number byte plus a 64-byte Valve message");
        assert_eq!(r.len(), t.framing.feature_reply_len);
        assert_eq!(r[0], 0x00, "the report number Steam asked with, echoed");
        assert_eq!(r[1], cmd::GET_ATTRIBUTES_VALUES, "SDL reads the command id here");
        assert_eq!(r[2], 0x2d, "…and the payload length here");
        // The message itself is untouched: the canned 64-byte frame, then the
        // one pad byte that gives it its full 64 bytes.
        assert_eq!(&r[..64], &TRITON_ATTRIBUTES[..]);
        assert_eq!(r[64], 0x00);
        assert_eq!(u32::from_le_bytes(r[4..8].try_into().unwrap()), 0x1302);

        // A request naming the feature report explicitly is echoed as such,
        // exactly as `buf[0] = report_number` would leave it.
        let named = t.get_report_reply(TRITON_FEATURE_REPORT_ID, cmd::GET_ATTRIBUTES_VALUES);
        assert_eq!(named[0], 0x01);
        assert_eq!(&named[1..], &r[1..], "only the report-number byte differs");

        // Every other canned answer is framed the same way.
        for selector in [cmd::GET_STRING_ATTRIBUTE, cmd::GET_CHIP_ID, cmd::INPUT_DATA, 0x77] {
            let r = t.get_report_reply(0, selector);
            assert_eq!(r.len(), 65, "never short, never empty");
            assert_eq!(r[1], selector, "the command id Steam asked for");
        }
        assert_eq!(&t.get_report_reply(0, cmd::GET_STRING_ATTRIBUTE)[4..17], b"FXA0000000001");
    }

    /// The deck answer does **not** move: 64 bytes, the canned frame verbatim,
    /// the bytes Steam adopted on 2026-09-01.
    #[test]
    fn the_deck_get_report_answer_is_the_proven_64_bytes_unchanged() {
        let d = deck();
        for selector in [cmd::GET_ATTRIBUTES_VALUES, cmd::GET_STRING_ATTRIBUTE, cmd::INPUT_DATA] {
            for rnum in [0x00, 0x01, 0xff] {
                let r = d.get_report_reply(rnum, selector);
                assert_eq!(r.len(), 64);
                assert_eq!(r.as_slice(), &d.canned_reply(selector)[..]);
                assert_eq!(r[0], 0x00, "unnumbered: the leading byte is never an id");
            }
        }
        assert_eq!(d.get_report_reply(0, cmd::GET_ATTRIBUTES_VALUES), DECK_ATTRIBUTES.to_vec());
    }

    #[test]
    fn a_serial_longer_than_the_report_is_truncated_not_panicked() {
        let long = framed(&[0x00, 0xAE, 0x14, 0x01], &[0xab; 200]);
        assert_eq!(long.len(), 64);
        assert!(long[4..].iter().all(|&b| b == 0xab));
    }

    /// The `deck` serial: what it is, where Steam reads it, and the framing of
    /// the answer that carries it.
    ///
    /// On 2026-09-02 Steam enumerated the empty-`uniq` deck fake as
    /// `serial_number:  - 0` and logged `Controller has an Invalid or missing
    /// unit serial number, setting to '28de-12f0-3147b8f'` — inventing a config
    /// key rather than using one this profile controls.
    #[test]
    fn the_deck_serial_is_fixed_stable_and_stated_the_same_on_both_channels() {
        let d = deck();

        // `uniq` is the field Steam's enumeration reads (hidapi parses HID_UNIQ
        // out of the node's uevent), and the one that was empty.
        assert_eq!(d.uniq, DECK_SERIAL);
        assert!(!d.uniq.is_empty());

        // The device must not state two different serials on two channels: the
        // `0xAE` answer says the same thing the HID descriptor does. This is the
        // same self-consistency rule `TRITON_ATTRIBUTES` follows for the product
        // id (risk R3), applied to the serial.
        assert_eq!(d.serial, d.uniq, "the 0xAE answer agrees with the HID descriptor");
        assert_eq!(triton().serial, triton().uniq, "and triton already did");

        // Distinct from triton's, so the two identities keep separate
        // `configset_<serial>.vdf` files — a Deck-shaped binding set applied to
        // a Triton-shaped device would bind the wrong buttons.
        assert_ne!(d.uniq, triton().uniq);

        // Not a controller serial, on either channel: the deck identity must
        // never collide with the one `triton` states. (Those two are the
        // synthetic stand-ins this tree uses for the controller and the puck —
        // no real unit's serial is published here.)
        for other in ["FXA0000000001", "FXB0000000002"] {
            assert_ne!(d.serial, other, "no controller serial is baked into the deck identity");
        }

        // Plain ASCII, and short enough for every field that carries it: the
        // 63-usable-byte `uniq` of `struct uhid_create2_req`, the 19 bytes left
        // inside the `0x14`-byte declared payload of the `0xAE` answer, and a
        // filename, since Steam makes one out of it.
        assert!(DECK_SERIAL.is_ascii());
        assert!(DECK_SERIAL.len() <= 19, "fits the declared 0xAE payload");
        assert!(
            DECK_SERIAL.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "safe in a configset_<serial>.vdf filename"
        );
    }

    /// The `0xAE` answer's framing, checked against SDL rather than against
    /// itself — the half of the fix that had to stay right while the payload
    /// changed.
    #[test]
    fn the_string_attribute_answer_is_framed_the_way_sdl_reads_it() {
        for p in [deck(), triton()] {
            let r = p.canned_reply(cmd::GET_STRING_ATTRIBUTE);
            assert_eq!(r.len(), 64);
            // byte 0: the report-number byte. byte 1: the command SDL validates
            // (`uBuffer[1] != nExpectedResponse`, never `[0]`). byte 2: the
            // declared payload length, which SDL bounds-checks against the
            // bytes it actually read. byte 3: ATTRIB_STR_UNIT_SERIAL = 1, the
            // attribute Steam's "invalid or missing unit serial" names —
            // ATTRIB_STR_BOARD_SERIAL is 0 and would be the wrong one.
            assert_eq!(r[0], 0x00, "{:?}: report-number byte", p.identity);
            assert_eq!(r[1], cmd::GET_STRING_ATTRIBUTE, "{:?}: echoed command", p.identity);
            assert_eq!(r[2], 0x14, "{:?}: declared payload length", p.identity);
            assert_eq!(r[3], 0x01, "{:?}: ATTRIB_STR_UNIT_SERIAL", p.identity);

            // The serial, then zeros to the end — no stale bytes behind it.
            let n = p.serial.len();
            assert_eq!(&r[4..4 + n], p.serial.as_bytes(), "{:?}", p.identity);
            assert!(r[4 + n..].iter().all(|&b| b == 0), "{:?}: NUL padded", p.identity);
            // The payload fits inside what byte 2 declares: 1 selector + serial.
            assert!(n < r[2] as usize, "{:?}: serial fits the declared length", p.identity);
        }
    }
}
