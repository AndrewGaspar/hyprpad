use hyprpad::report::Frame;
use hyprpad::{bindings_sheet, broker, hidraw, run, setup};
use std::ffi::OsString;
use std::io::Write;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run::run().unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); }),
        // `reload`: nudge the running daemon (found via its pidfile) to re-read
        // its config live, by sending it SIGHUP. `kill -HUP <pid>` also works.
        Some("reload") => run::reload().unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); }),
        // `off`: turn the CONTROLLER off (not the session). Nudges the running
        // daemon with SIGUSR1 so the `0x9F` write goes out over the descriptors
        // it already holds — see `run::controller_off_command`.
        Some("off") => run::controller_off_command().unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); }),
        // `puck-settings [id…]`: read firmware settings back off the puck.
        // Read-only: it sends the `0x89`/`0x8B`/`0x8C` queries and prints what
        // came back. Needs the controller's descriptors, through the same
        // acquire path the daemon uses (broker first, direct open otherwise).
        Some("puck-settings") => puck_settings(&args[2..]),
        Some("monitor") => monitor(),
        // `bindings [--json]`: print the current bindings — the cheat sheet's
        // data source. Reads the config the daemon would read, so it is safe to
        // run while `hyprpad run` owns the device.
        Some("bindings") => {
            let json = match args.get(2).map(String::as_str) {
                None => false,
                Some("--json") => true,
                Some(other) => {
                    eprintln!("hyprpad bindings: unknown argument {other:?}");
                    eprintln!("usage: hyprpad bindings [--json]");
                    std::process::exit(2);
                }
            };
            bindings_sheet::run(json).unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); });
        }
        Some("setup") => {
            // `setup`         install the user-level hook and PRINT the root
            //                 host-integration steps (never run them);
            // `setup --check` report, read-only, on the host integration;
            // `setup --print` only print the root steps (plus the user unit);
            // `setup --user`  only print the systemd user unit's install;
            // `setup --revert` uninstall the user-level hook.
            let mode = match args.get(2).map(String::as_str) {
                None => setup::Mode::Install,
                Some("--check") => setup::Mode::Check,
                Some("--print") => setup::Mode::Print,
                Some("--user") => setup::Mode::User,
                Some("--revert") => setup::Mode::Revert,
                Some(other) => {
                    eprintln!("hyprpad setup: unknown argument {other:?}");
                    eprintln!("{}", setup::USAGE);
                    std::process::exit(2);
                }
            };
            setup::run(mode).unwrap_or_else(|e| { eprintln!("hyprpad: {e}"); std::process::exit(1); });
        }
        // `broker`: the privileged fd helper. Meant to run as root under
        // systemd (packaging/systemd/hyprpad-broker.{socket,service}); running
        // it by hand is for debugging. See src/broker.rs for the protocol.
        Some("broker") => {
            let rest: Vec<OsString> = std::env::args_os().skip(2).collect();
            let env_uid = std::env::var(broker::UID_ENV).ok();
            let opts = broker::parse_options(&rest, env_uid.as_deref()).unwrap_or_else(|e| {
                eprintln!("hyprpad broker: {e}");
                eprintln!("{}", broker::USAGE);
                std::process::exit(2);
            });
            broker::run(&opts).unwrap_or_else(|e| { eprintln!("hyprpad broker: {e}"); std::process::exit(1); });
        }
        _ => {
            eprintln!("usage: hyprpad <run|reload|off|bindings|puck-settings|monitor|setup|broker>");
            eprintln!();
            eprintln!("  run              drive Hyprland from the controller (gestures -> dispatch)");
            eprintln!("  reload           tell the running daemon to re-read its config (SIGHUP)");
            eprintln!("  off              turn the CONTROLLER off (SIGUSR1 to the daemon)");
            eprintln!("  bindings [--json] print the current bindings (the cheat sheet's data)");
            eprintln!("  puck-settings [id…]  read firmware settings off the puck (read-only)");
            eprintln!("  monitor          decode and print controller events (passive; Steam-safe)");
            eprintln!("  setup [--check|--user]  install the Steam hook; print the host and login steps");
            eprintln!("  broker           privileged fd helper (root, socket-activated; see setup)");
            std::process::exit(2);
        }
    }
}

/// `hyprpad puck-settings [id…]` — ask the firmware what its settings are.
///
/// Defaults to the two power settings this exists for: 25
/// (`SETTING_STEAMBUTTON_POWEROFF_TIME`, the guide-hold power-off timer) and 50
/// (`SETTING_SLEEP_INACTIVITY_TIMEOUT`). Prints current, max and default side by
/// side, because the units and defaults of both are unverified and the maximum
/// is the only thing that tells you the scale — see
/// `docs/design/puck-power.md` for the recipe this is step one of.
///
/// Strictly read-only. The only frames it sends are the `0x89`/`0x8B`/`0x8C`
/// queries; nothing here writes a setting or powers anything off.
fn puck_settings(args: &[String]) {
    let ids: Vec<u8> = if args.is_empty() {
        vec![25, 50]
    } else {
        let mut ids = Vec::new();
        for a in args {
            match a.parse::<u8>() {
                Ok(id) => ids.push(id),
                Err(_) => {
                    eprintln!("hyprpad puck-settings: {a:?} is not a setting id (0-255)");
                    eprintln!("usage: hyprpad puck-settings [id…]   (default: 25 50)");
                    std::process::exit(2);
                }
            }
        }
        ids
    };
    let table = hyprpad::lizard::read_puck_settings(&ids).unwrap_or_else(|e| {
        eprintln!("hyprpad: {e}");
        std::process::exit(1);
    });
    println!("{:>4}  {:>8}  {:>8}  {:>8}  name", "id", "current", "max", "default");
    let show = |v: Option<u16>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
    for s in &table {
        println!(
            "{:>4}  {:>8}  {:>8}  {:>8}  {}",
            s.id,
            show(s.current),
            show(s.max),
            show(s.default),
            s.name
        );
    }
    eprintln!();
    eprintln!(
        "note: the UNITS of settings 25 and 50 are unverified (docs/research/\
         guide-hold-poweroff.md §6). `max` is the best clue to the scale."
    );
}

fn monitor() {
    // Through the daemon's own acquire path, so `monitor` keeps working once the
    // udev rule has made the nodes root-only: with a broker installed the
    // descriptors come from it, without one they are direct opens as before.
    let Some(source) = hidraw::PuckSource::acquire() else {
        eprintln!("no Steam Controller puck found (28de:1304)");
        std::process::exit(1);
    };
    eprintln!("monitoring {} node(s), {} (passive tap)", source.len(), source.label());
    let rx = hidraw::read_all(source);
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
