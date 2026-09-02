//! The identities hyprpad can present to Steam, as **data**.
//!
//! Two profiles ship, and which one is live is one config line
//! (`h.gamepad { kind = "steam", identity = "triton" | "deck" }`):
//!
//! | | [`triton`] (default) | [`deck`] (proven fallback) |
//! |---|---|---|
//! | VID:PID | `28de:1302` — the *wired* single-interface Steam Controller | `28de:12f0` — InputPlumber's `ProductId::Generic` |
//! | Descriptor | the real 372-byte capture, `docs/research/assets/` | InputPlumber's 38-byte vendor-only blob |
//! | Input report | **the puck's own `0x42`, passed through** | a translated 64-byte Deck report |
//! | Report IDs | yes, on all three types | none at all |
//! | Steam adoption | **unproven** — awaits the owner's live test | **PROVEN on-device 2026-09-01** |
//!
//! # Why `triton` is the default despite being unproven
//!
//! Because it is the *least translation*. hyprpad owns a real Triton, and the
//! wired `1302`'s vendor collection declares input report `0x42` with a 53-byte
//! payload — byte-for-byte the same report `src/report.rs` already decodes off
//! the puck. So the input path is a **pass-through**: no transcoding, and the
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
//! # Why not `28de:1304`, the puck's own PID
//!
//! Steam derives the controller *slot index* from `bInterfaceNumber` for the
//! dongle PIDs, and SDL gates `0x1304` on `interface_number` being 2..=5. A uhid
//! device reports `-1`. Cloning the real puck therefore cannot work — §3.2, and
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
    /// The puck's own vendor report `0x42`: 54 bytes, **report-id prefixed**
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

/// The **proven** identity: an emulated Steam Deck controller under
/// InputPlumber's non-Deck `Generic` PID.
///
/// Every field is the proven probe's, byte for byte
/// (`scripts/research/uhid_active_probe.py`; `docs/research/uhid-steam-controller.md`
/// "PROVEN on-device (2026-09-01)"). Steam opened it, logged
/// `!! Steam controller device opened for index 5.`, loaded
/// `configset_neptune.vdf` for it, and sent it 39 `SetSettingsValues` writes.
///
/// `uniq` is deliberately empty, as InputPlumber leaves it: Steam then names the
/// device `28de-12f0-<hash>` and keys its config on that, which was stable
/// across the proven run. Pinning a serial here would change the config key.
pub fn deck() -> &'static Profile {
    static DECK: Profile = Profile {
        identity: Identity::Deck,
        name: "Steam Controller",
        phys: "",
        uniq: "",
        bus: BUS_USB,
        vendor: VID_VALVE,
        product: 0x12f0,
        version: 0x1000,
        country: 0,
        descriptor: &DECK_DESCRIPTOR,
        kind: ReportKind::Deck,
        attributes: DECK_ATTRIBUTES,
        serial: "1NPU7PLUMB3R",
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
/// `src/report.rs` decodes off the puck. That identity is what makes the triton
/// input path a pass-through.
pub const TRITON_DESCRIPTOR: [u8; 372] =
    *include_bytes!("../../docs/research/assets/triton-wired-1302-report-descriptor.bin");

/// The wired unit's serial, captured with the descriptor
/// (`docs/research/assets/triton-wired-1302-identity.md`). Pinned as `uniq`
/// because Steam keys `configset_<uniq>.vdf` on it (§3.3, §6 R11) — a Steam
/// Input config bound to this string survives every restart of the daemon.
pub const TRITON_SERIAL: &str = "FXA9961402A6C";

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

    /// Walk the captured descriptor and confirm the single fact the whole
    /// triton design rests on: its vendor input report `0x42` is 53 payload
    /// bytes, i.e. 54 on the wire — identical to what the puck streams and
    /// `report::Frame::decode` accepts. If this ever fails, the pass-through is
    /// no longer a pass-through.
    #[test]
    fn the_triton_descriptor_declares_the_pucks_own_0x42_report() {
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

    struct Item {
        /// The main-item tag: 0x81 Input, 0x91 Output, 0xb1 Feature.
        main: u8,
        report_id: u8,
        payload_bytes: usize,
    }

    /// A minimal HID report-descriptor walker: enough to recover
    /// `(main item, report id, payload size)` for each Input/Output/Feature.
    fn report_items(rd: &[u8]) -> Vec<Item> {
        let (mut id, mut count, mut size) = (0u8, 0usize, 0usize);
        let mut out = Vec::new();
        let mut i = 0;
        while i < rd.len() {
            let b = rd[i];
            let len = usize::from(b & 3);
            let val = rd[i + 1..(i + 1 + len).min(rd.len())]
                .iter()
                .rev()
                .fold(0usize, |a, &x| (a << 8) | usize::from(x));
            match b & 0xfc {
                0x84 => id = val as u8,           // Report ID
                0x94 => count = val,              // Report Count
                0x74 => size = val,               // Report Size
                _ => {}
            }
            if matches!(b, 0x81 | 0x91 | 0xb1) {
                out.push(Item { main: b, report_id: id, payload_bytes: count * size / 8 });
            }
            i += 1 + len;
        }
        out
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
        assert_eq!(t.uniq, "FXA9961402A6C", "pinned so Steam's config key is stable");
        assert_eq!(d.uniq, "", "verbatim InputPlumber — Steam names it 28de-12f0-<hash>");
        assert_eq!(t.vid_pid(), "28de:1302");
        assert_eq!(d.vid_pid(), "28de:12f0");
        // Never the puck's own PID: the bInterfaceNumber slot gate (§3.2).
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

        // The probe: `[0x00, 0xAE, 0x14, 0x01] + b"1NPU7PLUMB3R"`, resized to 64.
        let serial = d.canned_reply(cmd::GET_STRING_ATTRIBUTE);
        assert_eq!(&serial[..4], &[0x00, 0xAE, 0x14, 0x01]);
        assert_eq!(&serial[4..16], b"1NPU7PLUMB3R");
        assert!(serial[16..].iter().all(|&b| b == 0));

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
        assert_eq!(&serial[4..17], b"FXA9961402A6C");
        // R3: the attributes answer must not contradict the claimed identity.
        let attrs = t.canned_reply(cmd::GET_ATTRIBUTES_VALUES);
        assert_eq!(u32::from_le_bytes(attrs[4..8].try_into().unwrap()), 0x1302);
        assert_eq!(attrs[3], 0x01, "still ATTRIB_PRODUCT_ID");
        // Everything after the patched u32 is untouched Deck data.
        assert_eq!(&attrs[8..], &DECK_ATTRIBUTES[8..]);
    }

    #[test]
    fn a_serial_longer_than_the_report_is_truncated_not_panicked() {
        let long = framed(&[0x00, 0xAE, 0x14, 0x01], &[0xab; 200]);
        assert_eq!(long.len(), 64);
        assert!(long[4..].iter().all(|&b| b == 0xab));
    }
}
