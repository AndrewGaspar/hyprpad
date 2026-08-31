//! Exercise `hyprsc::hypr` against the live compositor.
//!
//! Not part of `cargo test` (it needs a running Hyprland and it mutates global
//! state — workspaces, fullscreen). Run it by hand:
//!
//! ```sh
//! cargo run --release --example hypr_live_check
//! ```
//!
//! It reads the active workspace, drives each dispatch primitive while
//! checking the state actually changed, and prints a few parsed events from
//! the live stream.

use hyprsc::hypr::{self, Hypr, HyprEvent};
use std::time::Duration;

/// Pull the `"id"` out of an `activeworkspace` JSON reply without a JSON crate.
fn active_ws_id(h: &Hypr) -> i64 {
    let json = h.query("activeworkspace").unwrap();
    let key = "\"id\":";
    let start = json.find(key).unwrap() + key.len();
    let rest = json[start..].trim_start();
    let end = rest.find([',', '\n', '}']).unwrap();
    rest[..end].trim().parse().unwrap()
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let start = json.find(&pat)? + pat.len();
    let rest = json[start..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        Some(&r[..r.find('"')?])
    } else {
        let end = rest.find([',', '\n', '}'])?;
        Some(rest[..end].trim())
    }
}

fn main() {
    let h = Hypr::connect().expect("connect to Hyprland");

    // Subscribe first so we can watch the dispatches land as events.
    let events = hypr::subscribe().expect("subscribe to event stream");

    println!("== workspace_relative ==");
    let before = active_ws_id(&h);
    h.workspace_relative(1).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let after = active_ws_id(&h);
    h.workspace_relative(-1).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let restored = active_ws_id(&h);
    println!("  ws before={before} after(+1)={after} restored(-1)={restored}");
    assert_eq!(before, restored, "workspace should return to start");
    assert_ne!(before, after, "workspace should have changed on +1");

    println!("== toggle_fullscreen (x2) ==");
    let fs0 = field(&h.query("activewindow").unwrap(), "fullscreen")
        .unwrap_or("?")
        .to_string();
    h.toggle_fullscreen().unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let fs1 = field(&h.query("activewindow").unwrap(), "fullscreen")
        .unwrap_or("?")
        .to_string();
    h.toggle_fullscreen().unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let fs2 = field(&h.query("activewindow").unwrap(), "fullscreen")
        .unwrap_or("?")
        .to_string();
    println!("  fullscreen field: {fs0} -> {fs1} -> {fs2}");

    println!("== move_window_to_workspace_relative (+1 then -1) ==");
    let addr_before = field(&h.query("activewindow").unwrap(), "address")
        .unwrap_or("?")
        .to_string();
    h.move_window_to_workspace_relative(1).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let ws_moved = active_ws_id(&h);
    h.move_window_to_workspace_relative(-1).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let ws_back = active_ws_id(&h);
    println!("  active window {addr_before} followed to ws {ws_moved}, back to {ws_back}");

    println!("== focus_window (idiomatic dispatch; reply only) ==");
    let addr = field(&h.query("activewindow").unwrap(), "address")
        .unwrap_or("0x0")
        .to_string();
    match h.focus_window(&addr) {
        Ok(()) => println!("  focus_window({addr}) accepted (ok)"),
        Err(e) => println!("  focus_window error: {e}"),
    }

    println!("== dispatch_raw error surfacing ==");
    match h.dispatch_raw("hl.dsp.nope_xyz()") {
        Ok(r) => println!("  unexpected ok: {r:?}"),
        Err(e) => println!("  error surfaced as expected: {e}"),
    }

    println!("== spawn (fire-and-forget) ==");
    h.spawn("true");
    println!("  spawned `true`");

    println!("== live events (up to 12, or ~5s) ==");
    // Drain whatever the earlier dispatches buffered so the nudges below are
    // what we actually observe.
    while events.try_recv().is_ok() {}
    // Nudge the compositor so a spread of event kinds flow, then drain them:
    // a titled throwaway window (OpenWindow/CloseWindow), fullscreen toggles
    // (Fullscreen), and workspace hops (Workspace).
    h.spawn("foot --title hyprsc-evt-probe sh -c 'sleep 1'");
    h.toggle_fullscreen().unwrap();
    h.toggle_fullscreen().unwrap();
    h.workspace_relative(1).unwrap();
    h.workspace_relative(-1).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut n = 0;
    while n < 12 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match events.recv_timeout(remaining) {
            Ok(ev) => {
                // Skip the noisy title-spam passthrough for legibility.
                if let HyprEvent::Other { name, .. } = &ev {
                    if name.starts_with("windowtitle") {
                        continue;
                    }
                }
                println!("  {ev:?}");
                n += 1;
            }
            Err(_) => break,
        }
    }

    println!("done.");
}
