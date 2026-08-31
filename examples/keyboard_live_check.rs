//! Exercise `hyprsc::output::VirtualKeyboard` end-to-end against a live app.
//!
//! Not part of `cargo test` (it needs a running Hyprland, `/dev/uinput`, and a
//! terminal emulator). Run it by hand:
//!
//! ```sh
//! cargo run --release --example keyboard_live_check
//! ```
//!
//! It spawns a `foot` terminal whose shell captures one typed line to a file,
//! types a known string through the uinput keyboard, then reads the file back
//! and reports whether the text arrived — proving real evdev keycodes reach a
//! focused, newly-created client.

use hyprsc::output::VirtualKeyboard;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hyprsc-typed.txt"));
    let text = "hello world 123!\n";
    let _ = std::fs::remove_file(&out);

    println!("== VirtualKeyboard live check ==");
    println!("  capture file: {}", out.display());

    // A terminal whose shell reads exactly one line and writes it to `out`.
    // Hyprland focuses the newly-mapped window, so it receives our keystrokes.
    let script = format!(
        "IFS= read -r line; printf '%s' \"$line\" > '{}'; sleep 0.5",
        out.display()
    );
    let mut child = Command::new("foot")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .spawn()
        .expect("spawn foot; is a terminal available?");

    // Let foot map, take focus, and reach the blocking `read`.
    sleep(Duration::from_millis(1500));

    let mut kbd = match VirtualKeyboard::new() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("  VirtualKeyboard::new failed: {e}");
            let _ = child.kill();
            std::process::exit(1);
        }
    };
    println!("  typing: {text:?}");
    kbd.type_text(text);

    // Give the shell time to write the file, then collect.
    sleep(Duration::from_millis(700));
    let _ = child.wait();

    match std::fs::read_to_string(&out) {
        Ok(got) => {
            let want = text.trim_end_matches('\n');
            println!("  captured: {got:?}");
            if got == want {
                println!("  RESULT: PASS — typed text arrived exactly");
            } else {
                println!("  RESULT: MISMATCH — wanted {want:?}");
            }
        }
        Err(e) => println!("  RESULT: no capture file ({e}); was the terminal focused?"),
    }
}
