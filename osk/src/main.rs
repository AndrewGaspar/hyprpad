//! `hyprpad-osk` binary entry point.
//!
//! Usage:
//! ```text
//! hyprpad-osk [--stdin | --socket] [--theme <builtin|file|omarchy>] [--theme-file <path>]
//!     --socket        (default) listen on $XDG_RUNTIME_DIR/hyprpad-osk.sock
//!     --stdin         read control commands from stdin (handy for the live test)
//!     --theme <src>   theme source: builtin (default) | file | omarchy
//!     --theme-file P  load theme tokens from P (implies --theme file)
//!     -h,--help       print this help
//! ```
//!
//! Drive it over the control channel (see [`hyprpad_osk::control`]), e.g.:
//! ```text
//! printf 'show bottom\ntype hello world\nhide\nquit\n' | hyprpad-osk --stdin
//! # or, with the socket running:
//! printf 'show split\n' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/hyprpad-osk.sock
//! ```

use hyprpad_osk::app::Osk;
use hyprpad_osk::control::Channel;
use hyprpad_osk::theme::ThemeSource;

fn main() {
    let mut use_stdin = false;
    let mut theme_source = ThemeSource::BuiltIn;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdin" => use_stdin = true,
            "--socket" => use_stdin = false,
            "--theme" => match args.next().as_deref() {
                Some("builtin") => theme_source = ThemeSource::BuiltIn,
                Some("file") => theme_source = ThemeSource::TomlFile(ThemeSource::default_toml_path()),
                Some("omarchy") => theme_source = ThemeSource::Omarchy,
                Some(other) => {
                    eprintln!("hyprpad-osk: unknown --theme '{other}' (want builtin|file|omarchy)");
                    std::process::exit(2);
                }
                None => {
                    eprintln!("hyprpad-osk: --theme needs a value (builtin|file|omarchy)");
                    std::process::exit(2);
                }
            },
            "--theme-file" => match args.next() {
                Some(path) => theme_source = ThemeSource::TomlFile(path.into()),
                None => {
                    eprintln!("hyprpad-osk: --theme-file needs a path");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("hyprpad-osk: unknown argument '{other}' (try --help)");
                std::process::exit(2);
            }
        }
    }

    let theme = theme_source.load();

    let channel = if use_stdin {
        Channel::stdin()
    } else {
        match Channel::socket() {
            Ok(c) => {
                eprintln!(
                    "hyprpad-osk: control socket at {}/hyprpad-osk.sock",
                    std::env::var("XDG_RUNTIME_DIR").unwrap_or_default()
                );
                c
            }
            Err(e) => {
                eprintln!("hyprpad-osk: {e}");
                std::process::exit(1);
            }
        }
    };

    if let Err(e) = Osk::run(channel, theme) {
        eprintln!("hyprpad-osk: fatal: {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    print!(
        "hyprpad-osk — controller-driven on-screen keyboard for Hyprland (kickoff)\n\
         \n\
         USAGE:\n\
           hyprpad-osk [--stdin | --socket] [--theme <src>] [--theme-file <path>]\n\
         \n\
         OPTIONS:\n\
           --socket        (default) listen on $XDG_RUNTIME_DIR/hyprpad-osk.sock\n\
           --stdin         read control commands from stdin\n\
           --theme <src>   theme source: builtin (default) | file | omarchy\n\
           --theme-file P  load theme tokens from P (implies --theme file)\n\
           -h,--help       print this help\n\
         \n\
         CONTROL COMMANDS (one per line):\n\
           show <bottom|split> [reflow|overlay]   create + render the surface(s)\n\
           hide                                    DESTROY the surface(s)\n\
           cursor <L|R> <nx> <ny>                  pad absolute pos, axes in [-1,1]\n\
           commit <L|R>                            commit key under that pad's cursor\n\
           key <keycode>                           commit a raw evdev keycode\n\
           type <text>                             type an ASCII string\n\
           quit                                    exit\n"
    );
}
