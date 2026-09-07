//! Discovery and reading of the Steam Controller's hidraw nodes, over either
//! transport.
//!
//! hidraw has no exclusive mode: every open fd receives every report, so this
//! reader coexists with a running Steam client (docs/02, docs/03).
//!
//! The puck — the USB dongle the controller talks to wirelessly — exposes one
//! interface per pairing slot plus a dongle-control interface, and the
//! controller may occupy any slot. So we read every node concurrently and merge
//! into one channel.
//!
//! # Two transports, one node set
//!
//! The 2026 controller reaches the host two ways, and this module treats them as
//! one device seen through two doors ([`docs/research/bluetooth.md`] §2):
//!
//! * **the dongle** ("the Puck") — USB `0003:28DE:1304`, **five** hidraw nodes,
//!   streaming input report `0x42` at 54 bytes, 250 Hz;
//! * **Bluetooth** — BLE `0005:28DE:1303`, exactly **one** hidraw node
//!   (`hid-steam.c:1618-1621`, *"There is only one BLE HID interface"*),
//!   streaming input report `0x45` at 46 bytes, ~134 Hz measured.
//!
//! Both are listed by [`controller_nodes`] whenever both are present, and every reader
//! opens every node. **Only one can be live at a time** — the controller
//! connects to a single host link, so the dongle's nodes fall silent the moment
//! it switches to Bluetooth and vice versa. Which one is "the controller" right
//! now is therefore not a property of the node set but of the *traffic*, and
//! [`ActiveTransport`] is that rule: whichever node produced the most recent
//! decodable frame is the active transport. Silence changes nothing.
//!
//! Opening both is deliberate rather than wasteful. It means a transport switch
//! costs no re-enumeration and no reconnect: the frames simply start arriving on
//! a different descriptor, and the feature/output writers ([`crate::lizard`],
//! [`crate::haptics`]) — which already try every node and keep whichever
//! answers — follow the controller with no transport logic of their own.
//!
//! # Where the descriptors come from
//!
//! Two ways, and the daemon prefers the first:
//!
//! * **the broker** — `hyprpad broker` runs as root and passes already-open
//!   descriptors over a unix socket ([`crate::broker`]). This is what makes the
//!   nodes openable at all once `packaging/udev/72-hyprpad-puck.rules` has taken
//!   them away from every unprivileged process, which is in turn what hides the
//!   real controller from Steam, on both transports;
//! * **directly** — the historical path: enumerate `/sys/class/hidraw` and open
//!   the nodes ourselves. Still correct on a machine with no broker installed,
//!   and the automatic fallback whenever the broker cannot be reached.
//!
//! [`ControllerSource`] is the two of them as one value, and [`ControllerSource::acquire`]
//! is the single place the preference is expressed. Everything downstream —
//! the reader pipeline, haptics, lizard mode — takes descriptors and does not
//! care which half produced them.

use std::fs;
use std::io::{self, Read};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crate::broker;

/// Valve's USB vendor id, `USB_VENDOR_ID_VALVE` (`hid-ids.h:1385`).
pub const VID_VALVE: u16 = 0x28de;
/// `USB_DEVICE_ID_STEAM_CONTROLLER_PROTEUS` — the Puck, the 2.4 GHz dongle.
pub const PID_PUCK: u16 = 0x1304;
/// `USB_DEVICE_ID_STEAM_CONTROLLER_IBEX_BLE` — the 2026 controller's own
/// Bluetooth identity (`hid-ids.h:1390`, bound as `HID_BLUETOOTH_DEVICE` at
/// `hid-steam.c:2756-2760`).
pub const PID_BT: u16 = 0x1303;

/// `BUS_USB` from `<linux/input.h>`.
pub const BUS_USB: u16 = 0x0003;
/// `BUS_BLUETOOTH` from `<linux/input.h>`.
pub const BUS_BLUETOOTH: u16 = 0x0005;

/// Which link a node reaches the controller over.
///
/// Not a question about the controller's *identity* — it is one controller
/// either way — but about the wire, and therefore about report ids, rates and
/// what a silence means. `status.json` publishes it as `"transport"` so the
/// widget can tell "off" from "on the other machine" (research §3, row 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Transport {
    /// USB `0003:28DE:1304`, the 2.4 GHz dongle. The default because it is what
    /// every node was before Bluetooth existed: an unidentifiable descriptor
    /// reads as the historical case and nothing changes behaviour.
    #[default]
    Dongle,
    /// BLE `0005:28DE:1303`, the controller's own Bluetooth radio.
    Bluetooth,
}

impl Transport {
    /// The wire spelling — the `"transport"` field of `status.json`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::Dongle => "dongle",
            Transport::Bluetooth => "bluetooth",
        }
    }

    /// The transport a `(bus, vendor, product)` triple names, or `None` for
    /// anything that is not a Steam Controller hyprpad should read.
    ///
    /// **A strict allow-list of two rows, and it must stay one.** hyprpad's own
    /// virtual device is `0003:28DE:1302` — a vendor-only match would make the
    /// daemon read its own output back (research §3, row 2). The wired `1302`
    /// is deliberately absent for the same reason, and `0005:28DE:11??` (the
    /// 2015 controller) is not this controller at all.
    pub const fn of_ids(bus: u16, vendor: u16, product: u16) -> Option<Transport> {
        match (bus, vendor, product) {
            (BUS_USB, VID_VALVE, PID_PUCK) => Some(Transport::Dongle),
            (BUS_BLUETOOTH, VID_VALVE, PID_BT) => Some(Transport::Bluetooth),
            _ => None,
        }
    }
}

/// Parse the `HID_ID=` line of a `device/uevent` into `(bus, vendor, product)`.
///
/// The kernel writes it as `"HID_ID=%04X:%08X:%08X"` (`hid-core.c:2983`), so the
/// three fields are fixed-width uppercase hex — `HID_ID=0005:000028DE:00001303`
/// for this controller over Bluetooth.
///
/// Parsing beats the substring search this replaces because the bus matters:
/// `28DE` and `1303` both appear in the text of unrelated lines
/// (`MODALIAS=hid:b0005g0001v000028DEp00001303`), and — the reason it was a bug
/// waiting to happen — a substring match cannot tell the *bus* apart, which is
/// the only thing distinguishing a Bluetooth `1303` from anything else Valve
/// might ship on USB.
pub fn parse_hid_id(uevent: &str) -> Option<(u16, u16, u16)> {
    let line = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?;
    let mut parts = line.trim().split(':');
    let bus = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
    let product = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((u16::try_from(bus).ok()?, u16::try_from(vendor).ok()?, u16::try_from(product).ok()?))
}

/// The flags every hyprpad open of a controller hidraw node uses — the daemon's
/// own and the broker's alike, so a descriptor behaves identically whichever
/// side opened it, and whichever transport it is on.
///
/// * `O_RDWR`, not `O_RDONLY`: input is a read, but `HIDIOCSFEATURE`
///   ([`crate::lizard`]) and output reports ([`crate::haptics`]) are writes, and
///   once the broker is the only way in there is one descriptor set serving all
///   three.
/// * `O_CLOEXEC`: the daemon spawns the OSK as a child and must not leak the
///   controller into it. The broker's replies get the same property from
///   `MSG_CMSG_CLOEXEC`.
/// * **not** `O_NONBLOCK`: [`read_all`]'s per-node threads block in `read`,
///   which is the whole design — a non-blocking descriptor would turn them into
///   spin loops on `EAGAIN`.
pub const OPEN_FLAGS: libc::c_int = libc::O_RDWR | libc::O_CLOEXEC;

/// Every hidraw node belonging to a Steam Controller — the dongle's and the
/// Bluetooth one alike — numerically ordered, each labelled with the transport
/// it arrived on.
///
/// Reads `/sys` only, so it keeps working after the udev rule has made the nodes
/// themselves unopenable: "is the controller present" and "may I open it" are
/// different questions and this one answers the first.
///
/// A machine may legitimately show **both** at once: the dongle stays plugged in
/// and enumerated while the controller is talking to the same host over
/// Bluetooth (measured — the `1304` nodes persist and simply go silent). Both
/// are returned; [`ActiveTransport`] decides which is live.
pub fn controller_nodes_by_transport() -> io::Result<Vec<(PathBuf, Transport)>> {
    let mut nodes: Vec<(u32, PathBuf, Transport)> = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(text) = fs::read_to_string(entry.path().join("device/uevent")) else {
            continue;
        };
        let Some((bus, vendor, product)) = parse_hid_id(&text) else { continue };
        let Some(transport) = Transport::of_ids(bus, vendor, product) else { continue };
        let n: u32 = name.trim_start_matches("hidraw").parse().unwrap_or(u32::MAX);
        nodes.push((n, PathBuf::from("/dev").join(&name), transport));
    }
    // By hidraw minor, so the order is stable and readable in a log. The
    // dongle's nodes therefore stay in their historical order, and the
    // Bluetooth node lands wherever its minor puts it rather than being
    // special-cased to an end.
    nodes.sort_by_key(|(n, _, _)| *n);
    Ok(nodes.into_iter().map(|(_, p, t)| (p, t)).collect())
}

/// [`controller_nodes_by_transport`] without the labels, for the many callers that
/// only want "which nodes are the controller's".
pub fn controller_nodes() -> io::Result<Vec<PathBuf>> {
    Ok(controller_nodes_by_transport()?.into_iter().map(|(p, _)| p).collect())
}

// `HIDIOCGRAWINFO` from <linux/hidraw.h> is `_IOR('H', 0x03,
// struct hidraw_devinfo)`. Same asm-generic layout `src/lizard.rs` encodes for
// the feature ioctls: dir[31:30] size[29:16] type[15:8] nr[7:0].
const HIDIOCGRAWINFO: libc::c_ulong = ((2u32 << 30)
    | ((std::mem::size_of::<RawDevInfo>() as u32) << 16)
    | ((b'H' as u32) << 8)
    | 0x03) as libc::c_ulong;

/// `struct hidraw_devinfo` — what `HIDIOCGRAWINFO` fills in.
#[repr(C)]
#[derive(Default)]
struct RawDevInfo {
    bustype: u32,
    vendor: i16,
    product: i16,
}

impl Transport {
    /// Ask an **already open** descriptor which transport it is, via
    /// `HIDIOCGRAWINFO`.
    ///
    /// This exists because the broker hands over descriptors with no path
    /// attached — a passed fd carries no name — so the node's identity cannot be
    /// recovered from `/sys` on that half. The ioctl can be asked of any hidraw
    /// fd whoever opened it, which makes the brokered and direct halves label
    /// their nodes by exactly the same rule rather than by two.
    ///
    /// Read-only, and it touches no report channel: it reads the three numbers
    /// the kernel already recorded at probe time.
    ///
    /// `None` for a descriptor that is not a Steam Controller hidraw node —
    /// including anything that is not a hidraw node at all, which is how the
    /// pipe-backed tests below reach this path.
    pub fn of_fd<F: std::os::fd::AsRawFd>(fd: &F) -> Option<Transport> {
        let mut info = RawDevInfo::default();
        // SAFETY: `fd` is a live descriptor for the duration of the call and
        // `info` is a valid, writable `hidraw_devinfo` of exactly the size
        // encoded in the request. The ioctl only writes that struct.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), HIDIOCGRAWINFO, &mut info) };
        if ret < 0 {
            return None;
        }
        Transport::of_ids(
            u16::try_from(info.bustype).ok()?,
            info.vendor as u16,
            info.product as u16,
        )
    }
}

/// Which transport is carrying the controller right now: **last decoded frame
/// wins**.
///
/// With the dongle plugged in and a Bluetooth bond live, both node sets are open
/// and both are legitimate — but the controller is only ever on one link, so the
/// other set is silent. That makes traffic, not enumeration, the signal.
///
/// Deliberately *only* moved by a frame. Silence leaves the answer unchanged,
/// which is the property that matters: a controller that stops streaming has not
/// changed transport, it has gone away, and that is
/// [`crate::run::Input::ReadersEnded`]'s question rather than this one.
#[derive(Debug, Default)]
pub struct ActiveTransport {
    current: Option<Transport>,
}

impl ActiveTransport {
    pub fn new() -> ActiveTransport {
        ActiveTransport { current: None }
    }

    /// Note that a decodable frame arrived on `t`. Returns `Some(t)` only when
    /// that is a *change* — the first frame of a generation, or a genuine
    /// switch — so the caller can log and publish transitions without
    /// rate-limiting 134 reports a second.
    pub fn note(&mut self, t: Transport) -> Option<Transport> {
        if self.current == Some(t) {
            return None;
        }
        self.current = Some(t);
        Some(t)
    }

    /// The live transport, or `None` if no frame has arrived yet.
    pub fn current(&self) -> Option<Transport> {
        self.current
    }
}

/// Open one hidraw node with [`OPEN_FLAGS`].
pub fn open_node(path: &Path) -> io::Result<OwnedFd> {
    use std::os::unix::ffi::OsStrExt;
    let mut c_path = Vec::with_capacity(path.as_os_str().len() + 1);
    c_path.extend_from_slice(path.as_os_str().as_bytes());
    c_path.push(0);
    // SAFETY: `c_path` is a NUL-terminated path that outlives the call, and the
    // flags are a valid `open` mask.
    let fd = unsafe { libc::open(c_path.as_ptr().cast(), OPEN_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open` returned a fresh descriptor nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The controller's nodes if it is reachable on either transport *and* at least
/// one node can be opened right now, else `None`.
///
/// The direct half of [`ControllerSource::acquire`]. A bare non-empty [`controller_nodes`]
/// is not a strong enough signal on its own: after the controller sleeps and the
/// device unbinds its hidraw nodes disappear, and this returns `None` until they
/// come back and are openable again.
///
/// The probe opens with [`OPEN_FLAGS`], exactly as the readers will, so a node
/// this accepts is a node they can use.
pub fn controller_readable() -> Option<Vec<PathBuf>> {
    let nodes = controller_nodes().ok()?;
    if nodes.is_empty() {
        return None;
    }
    if nodes.iter().any(|n| open_node(n).is_ok()) {
        Some(nodes)
    } else {
        None
    }
}

/// Where one generation of the controller's descriptors came from.
///
/// The daemon keeps this around a reader generation so `status.json` can say
/// which half is live, and so a reconnect can re-announce a change of half
/// (installing the broker mid-session, or stopping it).
#[derive(Debug)]
pub enum ControllerSource {
    /// Paths the daemon found and will open itself. The historical behaviour,
    /// and the fallback whenever the broker is absent or refuses.
    Paths(Vec<PathBuf>),
    /// Descriptors the root broker already opened and passed over.
    Fds(Vec<OwnedFd>),
}

impl ControllerSource {
    /// Obtain the controller's descriptors, preferring the broker.
    ///
    /// `None` means the controller is not reachable *by either route* right now
    /// — it is asleep, unplugged, or the dongle has unbound its nodes — which is
    /// exactly the condition the startup wait and the reconnect wait sit on.
    ///
    /// The decision table is [`broker::use_broker`]'s, and every broker failure
    /// is logged at most once per process by [`log_broker_failure_once`].
    pub fn acquire() -> Option<ControllerSource> {
        let path = broker::socket_path();
        let outcome = broker::request_at(&path, broker::Request::Controller);
        let answer = broker::classify(&path, &outcome);
        match (broker::use_broker(answer), outcome) {
            (true, Ok(fds)) => {
                announce_broker_once();
                Some(ControllerSource::Fds(fds))
            }
            (_, outcome) => {
                if let Err(e) = outcome {
                    log_broker_failure_once(answer, &path, &e);
                }
                controller_readable().map(ControllerSource::Paths)
            }
        }
    }

    /// How many descriptors this generation has.
    pub fn len(&self) -> usize {
        match self {
            ControllerSource::Paths(p) => p.len(),
            ControllerSource::Fds(f) => f.len(),
        }
    }

    /// Whether this generation has nothing in it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether these descriptors came from the broker — the `"source"` field of
    /// `status.json`.
    pub fn is_broker(&self) -> bool {
        matches!(self, ControllerSource::Fds(_))
    }

    /// The wire spelling of [`ControllerSource::is_broker`].
    pub fn label(&self) -> &'static str {
        if self.is_broker() {
            "broker"
        } else {
            "direct"
        }
    }

    /// Turn the source into `(label, descriptor)` pairs, opening paths on the
    /// way if that is what it holds.
    ///
    /// A path that will not open is dropped with a warning rather than failing
    /// the whole generation: the dongle-control interface and a slot with no
    /// controller behind it are both ordinary — as is the dongle sitting silent
    /// while the controller is on Bluetooth — and the controller is usable as
    /// long as *something* opened.
    pub fn into_open(self) -> Vec<(PathBuf, OwnedFd)> {
        match self {
            ControllerSource::Paths(paths) => paths
                .into_iter()
                .filter_map(|p| match open_node(&p) {
                    Ok(fd) => Some((p, fd)),
                    Err(e) => {
                        eprintln!("warning: {}: {e}", p.display());
                        None
                    }
                })
                .collect(),
            ControllerSource::Fds(fds) => fds
                .into_iter()
                .enumerate()
                .map(|(i, fd)| (PathBuf::from(format!("<broker fd {i}>")), fd))
                .collect(),
        }
    }

    /// Just the descriptors, for callers that do not log per node.
    pub fn into_fds(self) -> Vec<OwnedFd> {
        self.into_open().into_iter().map(|(_, fd)| fd).collect()
    }
}

/// Log the first broker failure of the process and then stay quiet.
///
/// The reconnect wait retries every 1.5 s and haptics re-open on a timer, so an
/// un-throttled message here would be a log flood on any machine that has no
/// broker — which is every machine that has not run `hyprpad setup`.
fn log_broker_failure_once(answer: broker::Answer, path: &Path, e: &io::Error) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    match answer {
        // The ordinary state on an un-set-up machine, and not a problem: say it
        // once at a low key so a reader knows which half is running, then never
        // again.
        broker::Answer::Absent => eprintln!(
            "hyprpad: no fd broker at {} — opening the controller directly. \
             Run `hyprpad setup` if you want Steam to stop seeing the real \
             controller.",
            path.display()
        ),
        _ => eprintln!(
            "warning: the fd broker at {} did not hand over the controller ({e}); \
             falling back to opening it directly",
            path.display()
        ),
    }
}

/// Say once, on the first successful broker hand-over, which half is live.
fn announce_broker_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        eprintln!("hyprpad: controller descriptors come from the root fd broker");
    }
}

/// A raw report from one node.
pub struct Report {
    /// The node it came from — a `/dev/hidrawN` path for a direct open, a
    /// `<broker fd N>` placeholder for a passed descriptor (a passed descriptor
    /// carries no name). Diagnostics only; nothing routes on it.
    pub node: PathBuf,
    /// Which link this node reaches the controller over, read off the
    /// descriptor once when its reader started ([`Transport::of_fd`]).
    ///
    /// This is what [`ActiveTransport`] consumes, and it is a property of the
    /// *node* rather than of the bytes on purpose: inferring the transport from
    /// the report id would work today (`0x42` dongle, `0x45` Bluetooth) and
    /// break the day a firmware emits the short report on both links — which
    /// research §2.2 records as already reported.
    pub transport: Transport,
    pub data: Vec<u8>,
}

/// Spawn a blocking reader thread per descriptor; reports merge into the
/// returned channel. The channel closes when every reader has exited.
///
/// **One reader ending does not end the generation.** With both transports
/// present the channel outlives any single node, which is exactly what a
/// transport switch needs: the Bluetooth node vanishing while the dongle's five
/// stay open must not read as "the controller disconnected", because the
/// controller is about to start streaming on the other descriptor. Only when
/// *every* reader has gone does the channel close and
/// [`crate::run::Input::ReadersEnded`] fire.
pub fn read_all(source: ControllerSource) -> mpsc::Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    for (node, fd) in source.into_open() {
        let tx = tx.clone();
        // Once per node, before the reads start: the answer cannot change for
        // the life of a descriptor, so paying an ioctl per report would be
        // 134–250 wasted syscalls a second.
        let transport = Transport::of_fd(&fd).unwrap_or_default();
        thread::spawn(move || {
            let mut file = fs::File::from(fd);
            let mut buf = [0u8; 512];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        let report =
                            Report { node: node.clone(), transport, data: buf[..n].to_vec() };
                        if tx.send(report).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        // The ordinary way a Bluetooth node ends: BlueZ
                        // force-destroys the uhid device on disconnect
                        // (`src/shared/uhid.c:546-563`), so the node goes away
                        // under an open descriptor rather than reaching EOF.
                        eprintln!("warning: {} ({}): {e}", node.display(), transport.as_str());
                        return;
                    }
                }
            }
        });
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// The flags are the contract between the broker and the direct path; a
    /// change to either half that is not a change to both is a bug.
    #[test]
    fn the_open_flags_are_read_write_cloexec_and_blocking() {
        assert_eq!(OPEN_FLAGS & libc::O_ACCMODE, libc::O_RDWR, "writes go to the controller too");
        assert_ne!(
            OPEN_FLAGS & libc::O_CLOEXEC,
            0,
            "the OSK child must not inherit the controller"
        );
        assert_eq!(OPEN_FLAGS & libc::O_NONBLOCK, 0, "the readers block on purpose");
    }

    /// `open_node` is a real `open`, so it works on any file — which is how this
    /// tests it without a device.
    #[test]
    fn open_node_returns_a_working_descriptor() {
        use std::io::Write as _;
        let path =
            std::env::temp_dir().join(format!("hyprpad-open-node-{}", std::process::id()));
        std::fs::write(&path, b"seed").unwrap();
        let fd = open_node(&path).expect("open");
        // O_RDWR, and no O_APPEND/O_TRUNC: a plain writable descriptor at
        // offset 0 — which is exactly what an output report to hidraw wants.
        let mut f = fs::File::from(fd);
        f.write_all(b"!").unwrap();
        drop(f);
        assert_eq!(std::fs::read(&path).unwrap(), b"!eed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_node_reports_a_missing_path_rather_than_panicking() {
        let err = open_node(Path::new("/definitely/not/here/hidraw999")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_source_knows_which_half_it_came_from() {
        let direct = ControllerSource::Paths(vec![PathBuf::from("/dev/hidraw7")]);
        assert!(!direct.is_broker());
        assert_eq!(direct.label(), "direct");
        assert_eq!(direct.len(), 1);
        assert!(!direct.is_empty());

        let (_r, w) = std::io::pipe().unwrap();
        let brokered = ControllerSource::Fds(vec![OwnedFd::from(w)]);
        assert!(brokered.is_broker());
        assert_eq!(brokered.label(), "broker");
        assert_eq!(brokered.len(), 1);

        assert!(ControllerSource::Paths(Vec::new()).is_empty());
        assert!(ControllerSource::Fds(Vec::new()).is_empty());
    }

    /// `read_all` over passed descriptors is the broker path end to end, with a
    /// pipe standing in for a hidraw node.
    #[test]
    fn read_all_streams_from_passed_descriptors() {
        use std::io::Write as _;
        let (r, mut w) = std::io::pipe().unwrap();
        let rx = read_all(ControllerSource::Fds(vec![OwnedFd::from(r)]));
        w.write_all(&[0x42, 0x01, 0x02]).unwrap();
        let got = rx.recv().expect("a report");
        assert_eq!(got.data, vec![0x42, 0x01, 0x02]);
        assert_eq!(got.node, PathBuf::from("<broker fd 0>"));
        // Closing the write end ends the reader, which closes the channel — the
        // signal `run.rs` turns into `Input::ReadersEnded`.
        drop(w);
        assert!(rx.recv().is_err(), "the channel must close when the readers end");
    }

    /// A path that cannot be opened is skipped, not fatal: the whole generation
    /// survives one bad node.
    #[test]
    fn a_dead_path_is_dropped_from_a_direct_generation() {
        let good = std::env::temp_dir().join(format!("hyprpad-src-{}", std::process::id()));
        std::fs::write(&good, b"x").unwrap();
        let source = ControllerSource::Paths(vec![
            PathBuf::from("/definitely/not/here/hidraw999"),
            good.clone(),
        ]);
        let opened = source.into_open();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].0, good);
        let _ = std::fs::remove_file(&good);
    }

    // -----------------------------------------------------------------------
    // Discovery: the uevent match, both transports
    // -----------------------------------------------------------------------

    /// The two uevent texts this machine actually produces, captured verbatim
    /// (`/sys/class/hidraw/hidraw1` and `hidraw13`, 2026-09-03) minus the lines
    /// that are irrelevant to the match. The exact widths matter — the kernel
    /// writes `HID_ID=%04X:%08X:%08X`, so the vendor and product are **eight**
    /// hex digits, not four, and a parser that assumed otherwise would find
    /// nothing.
    const UEVENT_DONGLE: &str = "DRIVER=hid-generic\n\
         HID_ID=0003:000028DE:00001304\n\
         HID_NAME=Valve Software Steam Controller Puck\n\
         HID_PHYS=usb-0000:c1:00.3-2.1/input2\n\
         HID_UNIQ=FXB0000000002\n\
         MODALIAS=hid:b0003g0001v000028DEp00001304\n";

    const UEVENT_BT: &str = "DRIVER=hid-generic\n\
         HID_ID=0005:000028DE:00001303\n\
         HID_NAME=Steam Ctrl (BT) FXA0000000001\n\
         HID_PHYS=b0:e4:d5:11:22:33\n\
         HID_UNIQ=e8:47:3a:9d:1c:04\n\
         MODALIAS=hid:b0005g0001v000028DEp00001303\n";

    /// hyprpad's **own** virtual controller, as udev sees it on this machine.
    /// The one node that must never be adopted: reading it back would make the
    /// daemon consume its own relay output (research §3, row 2).
    const UEVENT_FAKE: &str = "DRIVER=hid-generic\n\
         HID_ID=0003:000028DE:00001302\n\
         HID_NAME=Valve Software Steam Controller\n\
         HID_PHYS=hyprpad-uhid/triton\n\
         MODALIAS=hid:b0003g0001v000028DEp00001302\n";

    #[test]
    fn the_uevent_of_each_transport_parses_to_its_own_identity() {
        assert_eq!(parse_hid_id(UEVENT_DONGLE), Some((0x0003, 0x28de, 0x1304)));
        assert_eq!(parse_hid_id(UEVENT_BT), Some((0x0005, 0x28de, 0x1303)));
        assert_eq!(parse_hid_id(UEVENT_FAKE), Some((0x0003, 0x28de, 0x1302)));
    }

    #[test]
    fn both_transports_are_recognised_and_nothing_else_is() {
        let of = |u: &str| parse_hid_id(u).and_then(|(b, v, p)| Transport::of_ids(b, v, p));
        assert_eq!(of(UEVENT_DONGLE), Some(Transport::Dongle));
        assert_eq!(of(UEVENT_BT), Some(Transport::Bluetooth));

        // The relay's own fake, and the only reason this is an allow-list.
        assert_eq!(of(UEVENT_FAKE), None, "the daemon must never read its own output");

        // The wired 1302 over USB is the fake's identity too — same row.
        assert_eq!(Transport::of_ids(BUS_USB, VID_VALVE, 0x1302), None);
        // The 2015 controller over Bluetooth is a different device entirely.
        assert_eq!(Transport::of_ids(BUS_BLUETOOTH, VID_VALVE, 0x1102), None);
        // Right ids, wrong bus, both ways round: the bus is load-bearing.
        assert_eq!(Transport::of_ids(BUS_BLUETOOTH, VID_VALVE, PID_PUCK), None);
        assert_eq!(Transport::of_ids(BUS_USB, VID_VALVE, PID_BT), None);
        // Another vendor's device that happens to carry the same product id.
        assert_eq!(Transport::of_ids(BUS_BLUETOOTH, 0x045e, PID_BT), None);
    }

    /// A substring search over the whole uevent — what this replaced — could not
    /// have told these apart, because `MODALIAS` repeats every number in a
    /// different shape. The parse reads exactly one line.
    #[test]
    fn the_match_reads_the_hid_id_line_and_not_the_modalias() {
        // A device whose MODALIAS mentions 1304 but whose HID_ID does not.
        let decoy = "HID_ID=0003:000028DE:00001302\n\
                     MODALIAS=hid:b0003g0001v000028DEp00001304\n";
        assert_eq!(parse_hid_id(decoy), Some((0x0003, 0x28de, 0x1302)));
        let (b, v, p) = parse_hid_id(decoy).unwrap();
        assert_eq!(Transport::of_ids(b, v, p), None);
    }

    #[test]
    fn a_uevent_with_no_or_malformed_hid_id_is_simply_not_a_controller() {
        assert_eq!(parse_hid_id(""), None);
        assert_eq!(parse_hid_id("DRIVER=hid-generic\nMODALIAS=usb:v28DEp1304\n"), None);
        assert_eq!(parse_hid_id("HID_ID=0003:000028DE\n"), None, "too few fields");
        assert_eq!(parse_hid_id("HID_ID=0003:000028DE:00001304:x\n"), None, "too many");
        assert_eq!(parse_hid_id("HID_ID=zzzz:000028DE:00001304\n"), None, "not hex");
        assert_eq!(parse_hid_id("HID_ID=00030000:000028DE:00001304\n"), None, "bus overflows u16");
    }

    #[test]
    fn the_transport_names_are_the_ones_status_json_publishes() {
        assert_eq!(Transport::Dongle.as_str(), "dongle");
        assert_eq!(Transport::Bluetooth.as_str(), "bluetooth");
        // The default is the historical case, so an unlabelled node behaves
        // exactly as it did before Bluetooth existed.
        assert_eq!(Transport::default(), Transport::Dongle);
    }

    /// `HIDIOCGRAWINFO` on something that is not a hidraw node fails with
    /// `ENOTTY`, and that must be a `None` rather than a panic or a wrong
    /// answer: it is the path every pipe-backed test in this file takes.
    #[test]
    fn a_descriptor_that_is_not_a_hidraw_node_names_no_transport() {
        let (r, w) = std::io::pipe().unwrap();
        assert_eq!(Transport::of_fd(&r), None);
        assert_eq!(Transport::of_fd(&w), None);
        // And `read_all` therefore labels it with the default rather than
        // dropping it.
        let rx = read_all(ControllerSource::Fds(vec![OwnedFd::from(r)]));
        drop(rx);
    }

    // -----------------------------------------------------------------------
    // The active-transport rule
    // -----------------------------------------------------------------------

    /// Last frame wins, and only a *change* is announced — the caller logs and
    /// republishes on the return value, and 134 frames a second on one
    /// transport must produce exactly one of those.
    #[test]
    fn the_active_transport_is_whichever_streamed_last() {
        let mut a = ActiveTransport::new();
        assert_eq!(a.current(), None, "no frame yet is not a transport");

        assert_eq!(a.note(Transport::Dongle), Some(Transport::Dongle), "the first frame announces");
        assert_eq!(a.current(), Some(Transport::Dongle));
        for _ in 0..250 {
            assert_eq!(a.note(Transport::Dongle), None, "a steady stream announces nothing");
        }

        // The controller switched links: one announcement, then quiet again.
        assert_eq!(a.note(Transport::Bluetooth), Some(Transport::Bluetooth));
        for _ in 0..134 {
            assert_eq!(a.note(Transport::Bluetooth), None);
        }
        assert_eq!(a.current(), Some(Transport::Bluetooth));

        // And back.
        assert_eq!(a.note(Transport::Dongle), Some(Transport::Dongle));
        assert_eq!(a.current(), Some(Transport::Dongle));
    }

    /// Silence is not a transport change. This is the whole reason the rule is
    /// frame-driven: with both node sets open, the inactive one is *always*
    /// silent, and if silence moved the answer the two would fight.
    #[test]
    fn silence_leaves_the_active_transport_alone() {
        let mut a = ActiveTransport::new();
        a.note(Transport::Bluetooth);
        // No `note` calls at all — the dongle's five nodes are open and quiet.
        assert_eq!(a.current(), Some(Transport::Bluetooth));
    }

    /// A mixed generation is the two-transport machine: the dongle's five nodes
    /// plus the Bluetooth one, all open, only one streaming. The channel must
    /// survive the Bluetooth node vanishing, because the controller has
    /// switched links rather than gone away — `ReadersEnded` fires only when
    /// *every* reader has ended.
    #[test]
    fn one_node_ending_does_not_end_a_mixed_generation() {
        use std::io::Write as _;
        let (bt_r, bt_w) = std::io::pipe().unwrap();
        let (usb_r, mut usb_w) = std::io::pipe().unwrap();
        let rx = read_all(ControllerSource::Fds(vec![OwnedFd::from(bt_r), OwnedFd::from(usb_r)]));

        // The Bluetooth node goes away (BlueZ destroyed it on disconnect).
        drop(bt_w);

        // The dongle picks the controller up. The channel is still open, so the
        // frame arrives and the loop never saw a disconnect.
        usb_w.write_all(&[0x42, 0x07]).unwrap();
        assert_eq!(rx.recv().expect("the surviving node still streams").data, vec![0x42, 0x07]);

        // Only when the last reader ends does the channel close.
        drop(usb_w);
        assert!(rx.recv().is_err(), "the generation ends when every node has");
    }

    /// Descriptors survive the conversion, in order.
    #[test]
    fn into_fds_keeps_every_passed_descriptor() {
        let pipes: Vec<_> = (0..3).map(|_| std::io::pipe().unwrap()).collect();
        let (readers, writers): (Vec<_>, Vec<_>) = pipes.into_iter().unzip();
        let fds = ControllerSource::Fds(writers.into_iter().map(OwnedFd::from).collect()).into_fds();
        assert_eq!(fds.len(), 3);
        // Every one is distinct and still open.
        let mut raws: Vec<_> = fds.iter().map(|f| f.as_raw_fd()).collect();
        raws.sort_unstable();
        raws.dedup();
        assert_eq!(raws.len(), 3);
        drop(readers);
    }
}
