//! Discovery and passive reading of the Steam Controller puck's hidraw nodes.
//!
//! hidraw has no exclusive mode: every open fd receives every report, so this
//! reader coexists with a running Steam client (docs/02, docs/03). We open
//! read-only and never write.
//!
//! The puck exposes one interface per pairing slot (plus a dongle-control
//! interface); a controller may occupy any slot, so we read every node
//! concurrently and merge into one channel.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

const VID_VALVE: &str = "28DE";
const PID_PUCK: &str = "1304";

/// All hidraw device paths belonging to a Steam Controller puck,
/// numerically ordered.
pub fn puck_nodes() -> std::io::Result<Vec<PathBuf>> {
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

/// A raw report from one node.
pub struct Report {
    pub node: PathBuf,
    pub data: Vec<u8>,
}

/// Spawn a blocking reader thread per node; reports merge into the returned
/// channel. A node that fails to open is skipped with a warning; the channel
/// closes when every reader has exited.
pub fn read_all(nodes: &[PathBuf]) -> mpsc::Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    for node in nodes {
        let node = node.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let mut file = match fs::File::open(&node) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("warning: {}: {e}", node.display());
                    return;
                }
            };
            let mut buf = [0u8; 512];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if tx.send(Report { node: node.clone(), data: buf[..n].to_vec() }).is_err() {
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
