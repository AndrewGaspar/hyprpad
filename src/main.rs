use hyprpad::report::Frame;
use hyprpad::{hidraw, run, setup};
use std::io::Write;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run::run().unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); }),
        Some("monitor") => monitor(),
        Some("setup") => {
            // `setup [--revert]`: install (or uninstall) the masked-Steam hook.
            let revert = match args.get(2).map(String::as_str) {
                None => false,
                Some("--revert") => true,
                Some(other) => {
                    eprintln!("hyprpad setup: unknown argument {other:?}");
                    eprintln!("usage: hyprpad setup [--revert]");
                    std::process::exit(2);
                }
            };
            setup::run(revert).unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); });
        }
        _ => {
            eprintln!("usage: hyprpad <run|monitor|setup>");
            eprintln!();
            eprintln!("  run              drive Hyprland from the controller (gestures -> dispatch)");
            eprintln!("  monitor          decode and print controller events (passive; Steam-safe)");
            eprintln!("  setup [--revert] install (or remove) the masked-Steam launcher hook");
            std::process::exit(2);
        }
    }
}

fn monitor() {
    let nodes = hidraw::puck_nodes().expect("enumerate hidraw");
    if nodes.is_empty() {
        eprintln!("no Steam Controller puck found (28de:1304)");
        std::process::exit(1);
    }
    eprintln!("monitoring {} node(s) (passive tap)", nodes.len());
    let rx = hidraw::read_all(&nodes);
    let mut prev = Frame::default();
    let t0 = Instant::now();
    let mut frames: u64 = 0;
    for report in rx {
        let Some(frame) = Frame::decode(&report.data) else { continue };
        frames += 1;
        let t = t0.elapsed().as_secs_f64();
        for b in frame.edges_down(&prev) {
            println!("{t:9.3}  DOWN {b:?}");
        }
        for b in frame.edges_up(&prev) {
            println!("{t:9.3}  up   {b:?}");
        }
        // Analog: report coarse changes only, to keep the monitor legible.
        let coarse = |v: i16| (v as i32 / 8192) as i8;
        let s = |p: (i16, i16)| (coarse(p.0), coarse(p.1));
        if s(frame.left_stick) != s(prev.left_stick) && s(frame.left_stick) != (0, 0) {
            println!("{t:9.3}  LS   {:?}", s(frame.left_stick));
        }
        if s(frame.right_stick) != s(prev.right_stick) && s(frame.right_stick) != (0, 0) {
            println!("{t:9.3}  RS   {:?}", s(frame.right_stick));
        }
        let tr = |v: u16| (v / 8192) as u8;
        if tr(frame.l2) != tr(prev.l2) {
            println!("{t:9.3}  L2   {}/4", tr(frame.l2));
        }
        if tr(frame.r2) != tr(prev.r2) {
            println!("{t:9.3}  R2   {}/4", tr(frame.r2));
        }
        if frames.is_multiple_of(5000) {
            eprintln!("[{t:9.3}] {frames} frames ({:.0} Hz)", frames as f64 / t);
        }
        std::io::stdout().flush().ok();
        prev = frame;
    }
}
