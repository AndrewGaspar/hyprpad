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
//!
//! Commands come in; machine-readable **events go out on stdout** (`event
//! crossed <L|R>` when a pad's cursor moves onto a new key — the daemon's cue to
//! fire that pad's haptic tick). Logs stay on stderr.

use std::path::PathBuf;

use hyprpad_osk::app::Osk;
use hyprpad_osk::control::Channel;
use hyprpad_osk::predict::Predictor;
use hyprpad_osk::theme::ThemeSource;

fn main() {
    let mut use_stdin = false;
    let mut theme_source = ThemeSource::BuiltIn;
    let mut model: Option<PathBuf> = None;
    let mut no_predict = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdin" => use_stdin = true,
            "--socket" => use_stdin = false,
            "--model" => match args.next() {
                Some(path) => model = Some(PathBuf::from(path)),
                None => {
                    eprintln!("hyprpad-osk: --model needs a path");
                    std::process::exit(2);
                }
            },
            "--no-predict" => no_predict = true,
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
    let predictor = load_predictor(model, no_predict);

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

    if let Err(e) = Osk::run(channel, theme, predictor) {
        eprintln!("hyprpad-osk: fatal: {e}");
        std::process::exit(1);
    }
}

/// Resolve the prediction model, in the order `--model`, `$HYPRPAD_OSK_MODEL`,
/// then the first of the standard locations that exists
/// (osk-prediction.md §8.3).
///
/// **No model is not an error**: prediction goes quietly off and the keyboard is
/// exactly what it was before — same geometry, same behaviour. An *explicitly
/// named* model that will not open is worth a loud line on stderr, because the
/// user asked for it.
fn load_predictor(explicit: Option<PathBuf>, disabled: bool) -> Predictor {
    if disabled {
        return Predictor::disabled();
    }
    let named = explicit.or_else(|| std::env::var_os("HYPRPAD_OSK_MODEL").map(PathBuf::from));
    let (path, asked_for) = match named {
        Some(p) => (p, true),
        None => match model_search_path().into_iter().find(|p| p.exists()) {
            Some(p) => (p, false),
            None => return Predictor::disabled(),
        },
    };
    match Predictor::open(&path) {
        Ok(p) => {
            eprintln!("hyprpad-osk: prediction model {}", path.display());
            p
        }
        Err(e) => {
            let how = if asked_for { "error" } else { "note" };
            eprintln!("hyprpad-osk: {how}: {}: {e} (prediction off)", path.display());
            Predictor::disabled()
        }
    }
}

/// Where a model is looked for when none is named: the user's data directory
/// first, then the system one — the same precedence `$XDG_DATA_HOME` /
/// `$XDG_DATA_DIRS` imply.
fn model_search_path() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        out.push(home.join("hyprpad-osk").join("en.model"));
    }
    let dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in dirs.split(':').filter(|d| !d.is_empty()) {
        out.push(PathBuf::from(dir).join("hyprpad-osk").join("en.model"));
    }
    out
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
           --model <path>  prediction model (else $HYPRPAD_OSK_MODEL, else\n\
         \x20                 $XDG_DATA_HOME/hyprpad-osk/en.model, else off)\n\
           --no-predict    start with no prediction at all\n\
           -h,--help       print this help\n\
         \n\
         CONTROL COMMANDS (one per line):\n\
           show <bottom|split> [reflow|overlay]   create + render (overlay is default)\n\
           hide                                    DESTROY the surface(s)\n\
           cursor <L|R> <nx> <ny>                  pad absolute pos, axes in [-1,1]\n\
           commit <L|R>                            commit key under that pad's cursor\n\
           shift <off|oneshot|stuck|on>            set the latched shift/caps state\n\
           shift <down|up>                         hold / release a physical Shift (momentary)\n\
           layer <base|symbols|toggle>            switch QWERTY <-> numeric/symbols\n\
           reflow <on|off>                         displace (on) vs overlay/float (off)\n\
           key <keycode>                           commit a raw evdev keycode\n\
           type <text>                             type an ASCII string\n\
           candidate accept [n]                    accept the highlighted suggestion (R1)\n\
           candidate next|prev                     move the strip's highlight (L1)\n\
           context reset                           forget the typed context (focus change)\n\
           learn on|off                            gate the personal word cache\n\
           predict on|off                          show / hide the suggestion strip\n\
           forget                                  delete every learned word\n\
           quit                                    exit\n\
         \n\
         EVENTS (one per line, on STDOUT; logs stay on stderr):\n\
           event crossed <L|R>                     that pad's cursor moved onto a NEW key\n\
           event candidate <n> <text>              the strip's highlight moved\n"
    );
}
