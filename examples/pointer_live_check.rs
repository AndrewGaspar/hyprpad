//! Exercise `hyprpad::output::VirtualPointer` against the live compositor.
//!
//! Not part of `cargo test` (it needs a running Hyprland and it moves the real
//! cursor). Run it by hand:
//!
//! ```sh
//! cargo run --release --example pointer_live_check
//! ```
//!
//! It reads `hyprctl cursorpos`, moves the pointer by a known delta, and
//! reports the observed change so a human (or the launcher) can confirm the
//! cursor really moved.

use hyprpad::output::{PointerButton, VirtualPointer};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn cursorpos() -> (i64, i64) {
    let out = Command::new("hyprctl")
        .arg("cursorpos")
        .output()
        .expect("run hyprctl cursorpos");
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    let (x, y) = s.split_once(',').unwrap_or_else(|| panic!("bad cursorpos: {s:?}"));
    (x.trim().parse().unwrap(), y.trim().parse().unwrap())
}

fn step(ptr: &mut VirtualPointer, dx: f64, dy: f64) {
    let before = cursorpos();
    ptr.move_relative(dx, dy);
    sleep(Duration::from_millis(200));
    let after = cursorpos();
    println!(
        "  move_relative({dx:>6.0},{dy:>6.0})  {before:?} -> {after:?}   delta = ({}, {})",
        after.0 - before.0,
        after.1 - before.1
    );
}

fn main() {
    let mut ptr = VirtualPointer::new().expect("create virtual pointer");
    println!("== VirtualPointer live check ==");

    // Nudge toward the top-left corner first so the test deltas do not clip on
    // a screen edge, then move in each direction.
    ptr.move_relative(-4000.0, -4000.0);
    sleep(Duration::from_millis(200));

    step(&mut ptr, 300.0, 0.0);
    step(&mut ptr, 0.0, 220.0);
    step(&mut ptr, 150.0, 150.0);
    step(&mut ptr, -200.0, -120.0);

    println!("== button + scroll (no cursorpos effect; visual/log only) ==");
    ptr.button(PointerButton::Left, true);
    sleep(Duration::from_millis(80));
    ptr.button(PointerButton::Left, false);
    ptr.scroll(0.0, 15.0);
    ptr.scroll(0.0, -15.0);
    println!("  left click + vertical scroll sent");

    println!("done");
}
