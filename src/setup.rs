//! `hyprpad setup` — the user-level installer that wires the masked Steam
//! launcher into the session.
//!
//! Everything here is USER-LEVEL and reversible. Nothing under `/usr` or `/etc`
//! is touched, nothing needs root. Two files are installed:
//!
//!   * `~/.local/bin/hyprpad-steam`                   — the masking wrapper
//!   * `~/.local/share/applications/steam.desktop`    — a user override that
//!     shadows the system `steam.desktop`, routing menu + `steam://` launches
//!     through the wrapper.
//!
//! The Hyprland autostart line (`steam -silent`) is NOT edited silently —
//! Omarchy generates that config from Lua, so we detect it and PRINT a
//! copy-pasteable instruction instead.
//!
//! Both installed files carry the [`MARKER`] string so `--revert` only ever
//! deletes files hyprpad itself wrote.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Marker embedded in every file hyprpad installs. `--revert` refuses to delete
/// a file that does not contain it, so a user's own hand-written override or a
/// same-named file from another tool is never clobbered.
pub const MARKER: &str = "hyprpad-managed";

/// The shipped wrapper script, embedded at build time so the installed copy is
/// byte-for-byte the one in `scripts/hyprpad-steam` (which itself carries the
/// [`MARKER`] in its header comment).
const WRAPPER: &str = include_str!("../scripts/hyprpad-steam");

/// System `.desktop` used as the override template.
const SYSTEM_DESKTOP: &str = "/usr/share/applications/steam.desktop";

/// Fallback template when the system `steam.desktop` is absent.
const MINIMAL_DESKTOP: &str = "\
[Desktop Entry]
Name=Steam
Comment=Application for managing and playing games on Steam
Exec=/usr/bin/steam %U
Icon=steam
Terminal=false
Type=Application
Categories=Network;FileTransfer;Game;
MimeType=x-scheme-handler/steam;x-scheme-handler/steamlink;
";

/// Entry point for the `setup` subcommand. `revert == true` uninstalls.
pub fn run(revert: bool) -> io::Result<()> {
    let home = home_dir()?;
    if revert {
        revert_all(&home)
    } else {
        install(&home)
    }
}

// --- Paths -----------------------------------------------------------------

fn home_dir() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "$HOME is not set"))
}

fn wrapper_path(home: &Path) -> PathBuf {
    home.join(".local/bin/hyprpad-steam")
}

fn desktop_path(home: &Path) -> PathBuf {
    home.join(".local/share/applications/steam.desktop")
}

// --- Install ---------------------------------------------------------------

fn install(home: &Path) -> io::Result<()> {
    let wrapper = wrapper_path(home);
    let desktop = desktop_path(home);

    // 1. Install the wrapper into ~/.local/bin, executable.
    install_wrapper(&wrapper)?;

    // 2. Write the user .desktop override, built from the system template.
    let template = fs::read_to_string(SYSTEM_DESKTOP).unwrap_or_else(|_| MINIMAL_DESKTOP.to_string());
    let override_contents = build_desktop_override(&template, &wrapper.to_string_lossy());
    if let Some(parent) = desktop.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&desktop, override_contents)?;

    // 3. Autostart: detect and instruct, do not silently edit generated Lua.
    let autostart_hits = detect_autostart(home);

    print_install_summary(home, &wrapper, &desktop, &autostart_hits);
    Ok(())
}

fn install_wrapper(wrapper: &Path) -> io::Result<()> {
    if let Some(parent) = wrapper.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(wrapper, WRAPPER)?;
    // chmod +x (0o755).
    let mut perms = fs::metadata(wrapper)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(wrapper, perms)?;
    Ok(())
}

// --- The .desktop transform (pure, unit-tested) ----------------------------

/// Rewrite a single line: if it is an `Exec=` line whose program is Steam
/// (`/usr/bin/steam`, a bare `steam`, or any path whose basename is `steam`),
/// replace just the program with `wrapper`, preserving all arguments
/// (`%U`, `steam://…`, etc.). Any other line is returned unchanged.
pub fn rewrite_exec_line(line: &str, wrapper: &str) -> String {
    let Some(value) = line.strip_prefix("Exec=") else {
        return line.to_string();
    };
    // Split "program args…" at the first whitespace run.
    let (prog, rest) = match value.find(char::is_whitespace) {
        Some(i) => (&value[..i], Some(value[i..].trim_start())),
        None => (value, None),
    };
    let is_steam = prog == "/usr/bin/steam"
        || prog == "steam"
        || Path::new(prog).file_name().is_some_and(|f| f == "steam");
    if !is_steam {
        return line.to_string();
    }
    match rest {
        Some(args) if !args.is_empty() => format!("Exec={wrapper} {args}"),
        _ => format!("Exec={wrapper}"),
    }
}

/// Build the full user-override contents from a system `.desktop` template:
/// a marker header comment followed by the template with every Steam `Exec=`
/// line rerouted through `wrapper`.
pub fn build_desktop_override(template: &str, wrapper: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {MARKER}: user override generated by `hyprpad setup`.\n\
         # Shadows {SYSTEM_DESKTOP}; routes every Steam launch through the\n\
         # masked wrapper at {wrapper}. Remove with `hyprpad setup --revert`.\n"
    ));
    for line in template.lines() {
        out.push_str(&rewrite_exec_line(line, wrapper));
        out.push('\n');
    }
    out
}

// --- Autostart detection ---------------------------------------------------

/// A detected `steam -silent` autostart occurrence and its suggested rewrite.
struct AutostartHit {
    file: PathBuf,
    line_no: usize,
    original: String,
    suggested: String,
}

/// Scan the usual Hyprland autostart files for a `steam -silent` line. We only
/// read and report — never edit — because on Omarchy the `.conf` is generated
/// from `.lua` and would be overwritten.
fn detect_autostart(home: &Path) -> Vec<AutostartHit> {
    let candidates = [
        home.join(".config/hypr/autostart.lua"),
        home.join(".config/hypr/autostart.conf"),
        home.join(".config/hypr/hyprland.conf"),
    ];
    let mut hits = Vec::new();
    for file in candidates {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.contains("steam -silent") && !line.contains("hyprpad-steam -silent") {
                hits.push(AutostartHit {
                    file: file.clone(),
                    line_no: i + 1,
                    original: line.to_string(),
                    suggested: line.replace("steam -silent", "hyprpad-steam -silent"),
                });
            }
        }
    }
    hits
}

// --- Revert ----------------------------------------------------------------

fn revert_all(home: &Path) -> io::Result<()> {
    let wrapper = wrapper_path(home);
    let desktop = desktop_path(home);

    println!("hyprpad setup --revert");
    println!();
    remove_if_ours(&wrapper);
    remove_if_ours(&desktop);

    println!();
    println!("Autostart: if you changed it during setup, restore the original line:");
    println!("    hyprpad-steam -silent   ->   steam -silent");
    println!(
        "  (check ~/.config/hypr/autostart.lua and ~/.config/hypr/autostart.conf)"
    );
    Ok(())
}

/// Delete `path` only if it exists and carries our [`MARKER`]; otherwise leave
/// it and say why. Idempotent: a missing file is reported, not an error.
fn remove_if_ours(path: &Path) {
    match fs::read_to_string(path) {
        Ok(contents) if contents.contains(MARKER) => match fs::remove_file(path) {
            Ok(()) => println!("  removed  {}", path.display()),
            Err(e) => println!("  FAILED to remove {}: {e}", path.display()),
        },
        Ok(_) => println!(
            "  kept     {} (not hyprpad-managed — left untouched)",
            path.display()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("  absent   {} (nothing to remove)", path.display())
        }
        Err(e) => println!("  skipped  {} ({e})", path.display()),
    }
}

// --- Summary printing ------------------------------------------------------

fn print_install_summary(home: &Path, wrapper: &Path, desktop: &Path, autostart: &[AutostartHit]) {
    let local_bin = home.join(".local/bin");
    println!("hyprpad setup — installed (user-level, no root):");
    println!();
    println!("  wrapper   {}", wrapper.display());
    println!("  desktop   {}  (shadows {SYSTEM_DESKTOP})", desktop.display());
    println!();

    // PATH note for the wrapper.
    let on_path = std::env::var("PATH")
        .map(|p| p.split(':').any(|d| Path::new(d) == local_bin))
        .unwrap_or(false);
    if !on_path {
        println!(
            "  NOTE: {} is not on your PATH; a bare `hyprpad-steam` in autostart",
            local_bin.display()
        );
        println!("        may not resolve. Use the full path, or add the dir to PATH.");
        println!();
    }

    // Autostart instruction — the required baseline: print, do not edit.
    println!("Autostart (ACTION REQUIRED — edit this yourself, it is generated config):");
    if autostart.is_empty() {
        println!("  No `steam -silent` autostart line found. If you start Steam from");
        println!("  Hyprland autostart, change `steam -silent` to `hyprpad-steam -silent`.");
    } else {
        for hit in autostart {
            println!("  in {} (line {}):", hit.file.display(), hit.line_no);
            println!("      - {}", hit.original.trim());
            println!("      + {}", hit.suggested.trim());
        }
        println!();
        println!("  Change `steam -silent` to `hyprpad-steam -silent` in the file(s) above.");
        println!("  (Left as an instruction on purpose: that config is generated from Lua,");
        println!("   so editing it by hand — the source .lua — is safer than an auto-edit.)");
    }
    println!();
    println!("Menu launches and steam:// links already go through the wrapper via the");
    println!("override above. A bare `steam` from a shell resolves to the wrapper too if");
    println!("{} precedes /usr/bin on PATH.", local_bin.display());
    println!();
    println!("Revert everything:  hyprpad setup --revert");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_embeds_marker_and_mask() {
        // The shipped script must carry the marker (so revert recognises it) and
        // the mask machinery (so it is the real launcher, not a stub).
        assert!(WRAPPER.contains(MARKER), "wrapper missing marker");
        assert!(WRAPPER.contains("bwrap"), "wrapper missing bwrap");
        assert!(WRAPPER.contains("28DE"), "wrapper missing puck VID");
        assert!(WRAPPER.contains("1304"), "wrapper missing puck PID");
        assert!(WRAPPER.contains("--dev-bind /dev/null"), "wrapper missing mask bind");
    }

    #[test]
    fn rewrite_main_exec_with_field_code() {
        assert_eq!(
            rewrite_exec_line("Exec=/usr/bin/steam %U", "/home/u/.local/bin/hyprpad-steam"),
            "Exec=/home/u/.local/bin/hyprpad-steam %U"
        );
    }

    #[test]
    fn rewrite_action_exec_with_steam_url() {
        assert_eq!(
            rewrite_exec_line(
                "Exec=/usr/bin/steam steam://store",
                "/home/u/.local/bin/hyprpad-steam"
            ),
            "Exec=/home/u/.local/bin/hyprpad-steam steam://store"
        );
    }

    #[test]
    fn rewrite_bare_exec_no_args() {
        assert_eq!(
            rewrite_exec_line("Exec=/usr/bin/steam", "/w/hyprpad-steam"),
            "Exec=/w/hyprpad-steam"
        );
    }

    #[test]
    fn rewrite_bare_steam_token() {
        assert_eq!(
            rewrite_exec_line("Exec=steam -silent", "/w/hyprpad-steam"),
            "Exec=/w/hyprpad-steam -silent"
        );
    }

    #[test]
    fn non_exec_lines_are_untouched() {
        for line in ["Name=Steam", "# Exec=/usr/bin/steam %U", "Icon=steam", ""] {
            assert_eq!(rewrite_exec_line(line, "/w/hyprpad-steam"), line);
        }
    }

    #[test]
    fn non_steam_exec_is_untouched() {
        let line = "Exec=/usr/bin/other steam://x";
        assert_eq!(rewrite_exec_line(line, "/w/hyprpad-steam"), line);
    }

    #[test]
    fn override_reroutes_every_steam_exec_and_marks_the_file() {
        // A representative slice of the real system steam.desktop: the main
        // entry plus two Desktop Actions, interleaved with unrelated keys.
        let template = "\
[Desktop Entry]
Name=Steam
Exec=/usr/bin/steam %U
Icon=steam
Type=Application

[Desktop Action Store]
Name=Store
Exec=/usr/bin/steam steam://store

[Desktop Action BigPicture]
Name=Big Picture
Exec=/usr/bin/steam steam://open/bigpicture
";
        let wrapper = "/home/ajg/.local/bin/hyprpad-steam";
        let out = build_desktop_override(template, wrapper);

        // Every original Steam Exec must now point at the wrapper…
        assert!(!out.contains("Exec=/usr/bin/steam"), "a raw steam Exec survived:\n{out}");
        assert_eq!(
            out.matches(&format!("Exec={wrapper}")).count(),
            3,
            "expected all 3 Exec lines rerouted:\n{out}"
        );
        // …with their args preserved…
        assert!(out.contains(&format!("Exec={wrapper} %U")));
        assert!(out.contains(&format!("Exec={wrapper} steam://store")));
        assert!(out.contains(&format!("Exec={wrapper} steam://open/bigpicture")));
        // …unrelated keys untouched, and the marker present.
        assert!(out.contains("Name=Steam"));
        assert!(out.contains("Icon=steam"));
        assert!(out.contains(MARKER));
    }

    #[test]
    fn minimal_template_is_rewritable() {
        let out = build_desktop_override(MINIMAL_DESKTOP, "/w/hyprpad-steam");
        assert!(!out.contains("Exec=/usr/bin/steam"));
        assert!(out.contains("Exec=/w/hyprpad-steam %U"));
        assert!(out.contains(MARKER));
    }
}
