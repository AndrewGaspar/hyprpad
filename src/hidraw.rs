//! Discovery and reading of the Steam Controller puck's hidraw nodes.
//!
//! hidraw has no exclusive mode: every open fd receives every report, so this
//! reader coexists with a running Steam client (docs/02, docs/03).
//!
//! The puck exposes one interface per pairing slot (plus a dongle-control
//! interface); a controller may occupy any slot, so we read every node
//! concurrently and merge into one channel.
//!
//! # Where the descriptors come from
//!
//! Two ways, and the daemon prefers the first:
//!
//! * **the broker** — `hyprpad broker` runs as root and passes already-open
//!   descriptors over a unix socket ([`crate::broker`]). This is what makes the
//!   nodes openable at all once `packaging/udev/72-hyprpad-puck.rules` has taken
//!   them away from every unprivileged process, which is in turn what hides the
//!   real puck from Steam;
//! * **directly** — the historical path: enumerate `/sys/class/hidraw` and open
//!   the nodes ourselves. Still correct on a machine with no broker installed,
//!   and the automatic fallback whenever the broker cannot be reached.
//!
//! [`PuckSource`] is the two of them as one value, and [`PuckSource::acquire`]
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

const VID_VALVE: &str = "28DE";
const PID_PUCK: &str = "1304";

/// The flags every hyprpad open of a puck hidraw node uses — the daemon's own
/// and the broker's alike, so a descriptor behaves identically whichever side
/// opened it.
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

/// All hidraw device paths belonging to a Steam Controller puck,
/// numerically ordered.
///
/// Reads `/sys` only, so it keeps working after the udev rule has made the nodes
/// themselves unopenable: "is the puck plugged in" and "may I open it" are
/// different questions and this one answers the first.
pub fn puck_nodes() -> io::Result<Vec<PathBuf>> {
    let mut nodes: Vec<(u32, PathBuf)> = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(text) = fs::read_to_string(entry.path().join("device/uevent")) else {
            continue;
        };
        let upper = text.to_uppercase();
        if upper.contains(VID_VALVE) && upper.contains(PID_PUCK) {
            let n: u32 = name.trim_start_matches("hidraw").parse().unwrap_or(u32::MAX);
            nodes.push((n, PathBuf::from("/dev").join(&name)));
        }
    }
    nodes.sort();
    Ok(nodes.into_iter().map(|(_, p)| p).collect())
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

/// The puck's nodes if it is present *and* at least one node can be opened right
/// now, else `None`.
///
/// The direct half of [`PuckSource::acquire`]. A bare non-empty [`puck_nodes`]
/// is not a strong enough signal on its own: after the controller sleeps and the
/// device unbinds its hidraw nodes disappear, and this returns `None` until they
/// come back and are openable again.
///
/// The probe opens with [`OPEN_FLAGS`], exactly as the readers will, so a node
/// this accepts is a node they can use.
pub fn puck_readable() -> Option<Vec<PathBuf>> {
    let nodes = puck_nodes().ok()?;
    if nodes.is_empty() {
        return None;
    }
    if nodes.iter().any(|n| open_node(n).is_ok()) {
        Some(nodes)
    } else {
        None
    }
}

/// Where one generation of the puck's descriptors came from.
///
/// The daemon keeps this around a reader generation so `status.json` can say
/// which half is live, and so a reconnect can re-announce a change of half
/// (installing the broker mid-session, or stopping it).
#[derive(Debug)]
pub enum PuckSource {
    /// Paths the daemon found and will open itself. The historical behaviour,
    /// and the fallback whenever the broker is absent or refuses.
    Paths(Vec<PathBuf>),
    /// Descriptors the root broker already opened and passed over.
    Fds(Vec<OwnedFd>),
}

impl PuckSource {
    /// Obtain the puck's descriptors, preferring the broker.
    ///
    /// `None` means the controller is not reachable *by either route* right now
    /// — it is asleep, unplugged, or the dongle has unbound its nodes — which is
    /// exactly the condition the startup wait and the reconnect wait sit on.
    ///
    /// The decision table is [`broker::use_broker`]'s, and every broker failure
    /// is logged at most once per process by [`log_broker_failure_once`].
    pub fn acquire() -> Option<PuckSource> {
        let path = broker::socket_path();
        let outcome = broker::request_at(&path, broker::Request::Puck);
        let answer = broker::classify(&path, &outcome);
        match (broker::use_broker(answer), outcome) {
            (true, Ok(fds)) => {
                announce_broker_once();
                Some(PuckSource::Fds(fds))
            }
            (_, outcome) => {
                if let Err(e) = outcome {
                    log_broker_failure_once(answer, &path, &e);
                }
                puck_readable().map(PuckSource::Paths)
            }
        }
    }

    /// How many descriptors this generation has.
    pub fn len(&self) -> usize {
        match self {
            PuckSource::Paths(p) => p.len(),
            PuckSource::Fds(f) => f.len(),
        }
    }

    /// Whether this generation has nothing in it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether these descriptors came from the broker — the `"source"` field of
    /// `status.json`.
    pub fn is_broker(&self) -> bool {
        matches!(self, PuckSource::Fds(_))
    }

    /// The wire spelling of [`PuckSource::is_broker`].
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
    /// controller behind it are both ordinary, and the puck is usable as long as
    /// *something* opened.
    pub fn into_open(self) -> Vec<(PathBuf, OwnedFd)> {
        match self {
            PuckSource::Paths(paths) => paths
                .into_iter()
                .filter_map(|p| match open_node(&p) {
                    Ok(fd) => Some((p, fd)),
                    Err(e) => {
                        eprintln!("warning: {}: {e}", p.display());
                        None
                    }
                })
                .collect(),
            PuckSource::Fds(fds) => fds
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
             Run `hyprpad setup` if you want Steam to stop seeing the real puck.",
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
    pub data: Vec<u8>,
}

/// Spawn a blocking reader thread per descriptor; reports merge into the
/// returned channel. The channel closes when every reader has exited.
pub fn read_all(source: PuckSource) -> mpsc::Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    for (node, fd) in source.into_open() {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut file = fs::File::from(fd);
            let mut buf = [0u8; 512];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if tx.send(Report { node: node.clone(), data: buf[..n].to_vec() }).is_err()
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("warning: {}: {e}", node.display());
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
        assert_eq!(OPEN_FLAGS & libc::O_ACCMODE, libc::O_RDWR, "writes go to the puck too");
        assert_ne!(OPEN_FLAGS & libc::O_CLOEXEC, 0, "the OSK child must not inherit the puck");
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
        let direct = PuckSource::Paths(vec![PathBuf::from("/dev/hidraw7")]);
        assert!(!direct.is_broker());
        assert_eq!(direct.label(), "direct");
        assert_eq!(direct.len(), 1);
        assert!(!direct.is_empty());

        let (_r, w) = std::io::pipe().unwrap();
        let brokered = PuckSource::Fds(vec![OwnedFd::from(w)]);
        assert!(brokered.is_broker());
        assert_eq!(brokered.label(), "broker");
        assert_eq!(brokered.len(), 1);

        assert!(PuckSource::Paths(Vec::new()).is_empty());
        assert!(PuckSource::Fds(Vec::new()).is_empty());
    }

    /// `read_all` over passed descriptors is the broker path end to end, with a
    /// pipe standing in for a hidraw node.
    #[test]
    fn read_all_streams_from_passed_descriptors() {
        use std::io::Write as _;
        let (r, mut w) = std::io::pipe().unwrap();
        let rx = read_all(PuckSource::Fds(vec![OwnedFd::from(r)]));
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
        let source = PuckSource::Paths(vec![
            PathBuf::from("/definitely/not/here/hidraw999"),
            good.clone(),
        ]);
        let opened = source.into_open();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].0, good);
        let _ = std::fs::remove_file(&good);
    }

    /// Descriptors survive the conversion, in order.
    #[test]
    fn into_fds_keeps_every_passed_descriptor() {
        let pipes: Vec<_> = (0..3).map(|_| std::io::pipe().unwrap()).collect();
        let (readers, writers): (Vec<_>, Vec<_>) = pipes.into_iter().unzip();
        let fds = PuckSource::Fds(writers.into_iter().map(OwnedFd::from).collect()).into_fds();
        assert_eq!(fds.len(), 3);
        // Every one is distinct and still open.
        let mut raws: Vec<_> = fds.iter().map(|f| f.as_raw_fd()).collect();
        raws.sort_unstable();
        raws.dedup();
        assert_eq!(raws.len(), 3);
        drop(readers);
    }
}
