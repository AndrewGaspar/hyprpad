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
//!
//! # The other half: host integration
//!
//! Hiding the *real* puck from Steam and getting a writable `/dev/uhid` needs
//! root, and this command **never takes root**. Instead it prints the exact
//! block of commands that does the job ([`host_install_steps`]), and
//! `hyprpad setup --check` reports, read-only, on whether that block has been
//! run and whether the result works ([`check_items`]).
//!
//! The files that block installs all live under `packaging/`, each with a
//! comment header explaining itself:
//!
//! | File | What it does |
//! |---|---|
//! | `packaging/udev/72-hyprpad-puck.rules` | takes the puck's hidraw nodes away from every unprivileged process |
//! | `packaging/sysusers.d/hyprpad.conf` | the `hyprpad` group that gates the broker socket |
//! | `packaging/systemd/hyprpad-broker.socket` | `/run/hyprpad/broker.sock`, `0660 root:hyprpad` |
//! | `packaging/systemd/hyprpad-broker.service` | the root fd broker ([`crate::broker`]) |

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::broker;

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

/// What `hyprpad setup` was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The default: install the user-level Steam hook, then print the root
    /// host-integration block. Nothing privileged is run.
    Install,
    /// Report, read-only, on the host integration. Opens nothing, changes
    /// nothing, needs no root.
    Check,
    /// Print the root host-integration block and stop. Installs nothing at all,
    /// so it is safe to pipe.
    Print,
    /// Remove the user-level Steam hook.
    Revert,
}

/// `hyprpad setup`'s usage line.
pub const USAGE: &str = "usage: hyprpad setup [--check | --print | --revert]";

/// Entry point for the `setup` subcommand.
pub fn run(mode: Mode) -> io::Result<()> {
    match mode {
        Mode::Print => {
            print!("{}", host_install_steps());
            Ok(())
        }
        Mode::Check => {
            let view = LiveHost;
            print!("{}", check_report(&view));
            Ok(())
        }
        Mode::Revert => revert_all(&home_dir()?),
        Mode::Install => install(&home_dir()?),
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
    println!();
    print!("{}", host_install_steps());
}

// ===========================================================================
// Host integration — printed, never run
// ===========================================================================

/// Where the udev rule is installed, and where `--check` looks for it. The
/// admin-owned directory wins over the vendor one, which is udev's own rule.
pub const RULE_NAME: &str = "72-hyprpad-puck.rules";
/// Directories udev reads rules from, in the order it reads them.
pub const RULE_DIRS: [&str; 2] = ["/etc/udev/rules.d", "/usr/lib/udev/rules.d"];
/// Valve's own rule, which is what makes hyprpad's *virtual* 28de device visible
/// to Steam with no rule of our own (research §2).
pub const VALVE_RULE_NAME: &str = "60-steam-input.rules";

/// The exact block `hyprpad setup` prints and never runs.
///
/// Every line is copy-pasteable from a hyprpad checkout. Pure, so the block is
/// tested rather than trusted — the tests below check that each shipped file
/// under `packaging/` is named here and that nothing is silently run.
pub fn host_install_steps() -> String {
    format!(
        "\
Host integration — Steam sees a Steam Controller (ACTION REQUIRED, needs root)
=============================================================================
This is the half that needs root, so `hyprpad setup` PRINTS it and never runs
it. Read packaging/*/ for what each file does; every one has a comment header
and a revert recipe. Run this from the hyprpad checkout you built in:

    SRC=\"$(pwd)\"

    # 0. the binary the broker unit runs
    sudo install -Dm755 \"$SRC/target/release/hyprpad\" /usr/local/bin/hyprpad

    # 1. take the real puck away from everything running as you — Steam included
    sudo install -Dm644 \"$SRC/packaging/udev/{RULE_NAME}\" \\
         /etc/udev/rules.d/{RULE_NAME}

    # 2. the group that gates the broker's socket
    sudo install -Dm644 \"$SRC/packaging/sysusers.d/hyprpad.conf\" \\
         /usr/lib/sysusers.d/hyprpad.conf
    sudo systemd-sysusers
    sudo usermod -aG hyprpad \"$USER\"

    # 3. the root fd broker that hands the daemon what it can no longer open
    sudo install -Dm644 \"$SRC/packaging/systemd/hyprpad-broker.socket\" \\
         /etc/systemd/system/hyprpad-broker.socket
    sudo install -Dm644 \"$SRC/packaging/systemd/hyprpad-broker.service\" \\
         /etc/systemd/system/hyprpad-broker.service

    # 4. apply both halves
    sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw
    sudo systemctl daemon-reload && sudo systemctl enable --now hyprpad-broker.socket

THEN RE-LOGIN — or run `newgrp hyprpad` in this shell. Group membership only
reaches processes started after it was granted, so until you do, the daemon
still cannot talk to the broker and will fall back to opening the puck directly.

Confirm the whole thing, read-only, with:

    hyprpad setup --check

To undo: remove /etc/udev/rules.d/{RULE_NAME} and reload udev, then
`sudo systemctl disable --now hyprpad-broker.socket`. See the comment headers.
"
    )
}

// ---------------------------------------------------------------------------
// `hyprpad setup --check`
// ---------------------------------------------------------------------------

/// What `stat` says about one device node. The only three fields the check
/// needs, and deliberately no more: this never *opens* a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeStat {
    /// The permission bits, `st_mode & 0o7777`.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl NodeStat {
    /// Whether the node is closed to everyone but root.
    ///
    /// `stat` is enough to answer this even in the presence of an ACL: when a
    /// POSIX ACL grants anyone anything, the group bits of `st_mode` become the
    /// ACL *mask*, so an ACL that would let the session user in shows up here as
    /// group bits that are not zero. Which is why the check never has to open
    /// the node — and must not, since opening it is exactly what it is testing
    /// the absence of.
    pub fn root_only(self) -> bool {
        self.uid == 0 && self.mode & 0o077 == 0
    }
}

/// The read-only facts `--check` needs, behind a trait so the whole verdict
/// table is testable without a machine that has any of it installed.
pub trait HostView {
    /// Is there a regular file at `path`?
    fn file_exists(&self, path: &Path) -> bool;
    /// Is there a socket at the broker's path?
    fn socket_present(&self) -> bool;
    /// The broker's path, for the message.
    fn socket_path(&self) -> PathBuf;
    /// Ask the broker for `verb`; `Ok(n)` is how many descriptors came back.
    fn broker(&self, verb: broker::Request) -> Result<usize, String>;
    /// The puck's hidraw nodes and what `stat` says about each. An empty vec
    /// means the puck is not plugged in.
    fn puck_nodes(&self) -> Vec<(PathBuf, Option<NodeStat>)>;
}

/// One line of the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// As it should be.
    Ok,
    /// Definitely not as it should be.
    Bad,
    /// Cannot be determined right now — almost always "the puck is not here".
    Unknown,
}

impl Verdict {
    /// The three-character tag each line starts with.
    pub fn tag(self) -> &'static str {
        match self {
            Verdict::Ok => "ok ",
            Verdict::Bad => "NO ",
            Verdict::Unknown => "?  ",
        }
    }
}

/// One checked item: what was looked at, how it came out, and the detail a human
/// needs to act on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub verdict: Verdict,
    pub label: &'static str,
    pub detail: String,
}

impl Check {
    fn new(verdict: Verdict, label: &'static str, detail: impl Into<String>) -> Check {
        Check { verdict, label, detail: detail.into() }
    }

    /// The printed line: `ok  label — detail`.
    pub fn line(&self) -> String {
        format!("  {} {:<22} {}", self.verdict.tag(), self.label, self.detail)
    }
}

/// Every check, in the order they are printed. Pure over [`HostView`].
pub fn check_items(view: &dyn HostView) -> Vec<Check> {
    let mut items = Vec::new();

    // 1. Valve's rule. Not ours to install, but its absence explains a fake that
    //    Steam never sees, so it is worth one line.
    let valve = RULE_DIRS.iter().map(|d| Path::new(d).join(VALVE_RULE_NAME)).find(|p| view.file_exists(p));
    items.push(match valve {
        Some(p) => Check::new(Verdict::Ok, "Valve's udev rule", format!("{}", p.display())),
        None => Check::new(
            Verdict::Bad,
            "Valve's udev rule",
            format!("{VALVE_RULE_NAME} not found — install steam-devices, or Steam \
                     will not see the virtual controller either"),
        ),
    });

    // 2. Our rule.
    let ours = RULE_DIRS.iter().map(|d| Path::new(d).join(RULE_NAME)).find(|p| view.file_exists(p));
    items.push(match ours {
        Some(p) => Check::new(Verdict::Ok, "hyprpad udev rule", format!("{}", p.display())),
        None => Check::new(
            Verdict::Bad,
            "hyprpad udev rule",
            format!("{RULE_NAME} not installed — the real puck is still visible to Steam"),
        ),
    });

    // 3. The socket. Everything below it depends on this one, so when it is
    //    missing the rest are reported as unknown rather than as failures.
    let socket = view.socket_present();
    items.push(if socket {
        Check::new(Verdict::Ok, "broker socket", format!("{}", view.socket_path().display()))
    } else {
        Check::new(
            Verdict::Bad,
            "broker socket",
            format!(
                "nothing at {} — `systemctl enable --now hyprpad-broker.socket`",
                view.socket_path().display()
            ),
        )
    });

    // 4. and 5. The two verbs, actually exercised. This is the only part of the
    //    check that talks to anything, and it is the part that catches the
    //    failure a file listing cannot: installed, enabled, and still refusing
    //    you because your shell predates `usermod -aG hyprpad`.
    for (verb, label, want) in [
        (broker::Request::Uhid, "broker: uhid", 1usize),
        (broker::Request::Puck, "broker: puck", 1usize),
    ] {
        items.push(if !socket {
            Check::new(Verdict::Unknown, label, "no socket to ask")
        } else {
            match view.broker(verb) {
                Ok(n) if n >= want => Check::new(
                    Verdict::Ok,
                    label,
                    format!("{n} descriptor(s)"),
                ),
                Ok(n) => Check::new(Verdict::Bad, label, format!("{n} descriptor(s) — expected at least {want}")),
                Err(e) => Check::new(Verdict::Bad, label, e),
            }
        });
    }

    // 6. Are the puck's nodes actually closed to us? The rule being *installed*
    //    is not the same as the rule having been *applied*: udev only re-runs on
    //    a trigger or a replug.
    let nodes = view.puck_nodes();
    items.push(if nodes.is_empty() {
        Check::new(
            Verdict::Unknown,
            "puck nodes root-only",
            "no 28de:1304 puck present — plug it in and re-run",
        )
    } else {
        let open: Vec<String> = nodes
            .iter()
            .filter(|(_, st)| !st.is_some_and(NodeStat::root_only))
            .map(|(p, st)| match st {
                Some(s) => format!("{} ({:04o} uid {})", p.display(), s.mode, s.uid),
                None => format!("{} (cannot stat)", p.display()),
            })
            .collect();
        if open.is_empty() {
            Check::new(
                Verdict::Ok,
                "puck nodes root-only",
                format!("{} node(s), all 0600 root:root", nodes.len()),
            )
        } else {
            Check::new(
                Verdict::Bad,
                "puck nodes root-only",
                format!(
                    "still reachable: {} — run `sudo udevadm control --reload && \
                     sudo udevadm trigger --subsystem-match=hidraw`, or replug",
                    open.join(", ")
                ),
            )
        }
    });

    items
}

/// The final line: ready, or exactly what is in the way.
pub fn verdict_line(items: &[Check]) -> String {
    let bad: Vec<&str> = items.iter().filter(|c| c.verdict == Verdict::Bad).map(|c| c.label).collect();
    let unknown: Vec<&str> =
        items.iter().filter(|c| c.verdict == Verdict::Unknown).map(|c| c.label).collect();
    if bad.is_empty() && unknown.is_empty() {
        return "READY for `[gamepad] kind = \"steam\"` — Steam will see only the virtual \
                controller."
            .to_string();
    }
    let mut out = String::from("NOT READY. ");
    if !bad.is_empty() {
        out.push_str(&format!("Missing or wrong: {}. ", bad.join(", ")));
    }
    if !unknown.is_empty() {
        out.push_str(&format!("Could not check: {}. ", unknown.join(", ")));
    }
    out.push_str("See `hyprpad setup --print` for the install block.");
    out
}

/// The whole `--check` output, as one string. Pure over [`HostView`], so the
/// exact report a given machine state produces is a unit test.
pub fn check_report(view: &dyn HostView) -> String {
    let items = check_items(view);
    let mut out = String::from("hyprpad setup --check (read-only; opens nothing)\n\n");
    for item in &items {
        out.push_str(&item.line());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&verdict_line(&items));
    out.push('\n');
    out
}

/// [`HostView`] against the machine this is running on.
pub struct LiveHost;

impl HostView for LiveHost {
    fn file_exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn socket_present(&self) -> bool {
        use std::os::unix::fs::FileTypeExt;
        fs::symlink_metadata(broker::socket_path())
            .map(|m| m.file_type().is_socket())
            .unwrap_or(false)
    }

    fn socket_path(&self) -> PathBuf {
        broker::socket_path()
    }

    fn broker(&self, verb: broker::Request) -> Result<usize, String> {
        // The descriptors are dropped the moment this returns: the check is
        // "could I have them", not "give me them".
        broker::request(verb).map(|fds| fds.len()).map_err(|e| e.to_string())
    }

    fn puck_nodes(&self) -> Vec<(PathBuf, Option<NodeStat>)> {
        use std::os::unix::fs::MetadataExt;
        crate::hidraw::puck_nodes()
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let st = fs::metadata(&p)
                    .ok()
                    .map(|m| NodeStat { mode: m.mode() & 0o7777, uid: m.uid(), gid: m.gid() });
                (p, st)
            })
            .collect()
    }
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

    // --- the printed host-integration block --------------------------------

    /// The block must name every file the repo actually ships under
    /// `packaging/`, or the install it describes is incomplete. This is the test
    /// that fails when a new packaging file is added and forgotten here.
    #[test]
    fn the_printed_block_names_every_shipped_packaging_file() {
        let block = host_install_steps();
        for shipped in [
            "packaging/udev/72-hyprpad-puck.rules",
            "packaging/sysusers.d/hyprpad.conf",
            "packaging/systemd/hyprpad-broker.socket",
            "packaging/systemd/hyprpad-broker.service",
        ] {
            assert!(block.contains(shipped), "the install block never mentions {shipped}");
        }
        // And the file names match the constants `--check` looks for.
        assert!(block.contains(RULE_NAME));
    }

    /// Every step the task requires, and the re-login note without which the
    /// group membership has not reached the shell that reads this.
    #[test]
    fn the_printed_block_carries_every_required_step() {
        let block = host_install_steps();
        for step in [
            "install -Dm644",
            "install -Dm755",
            "systemd-sysusers",
            "usermod -aG hyprpad",
            "udevadm control --reload",
            "udevadm trigger --subsystem-match=hidraw",
            "systemctl daemon-reload",
            "systemctl enable --now hyprpad-broker.socket",
            "newgrp hyprpad",
            "hyprpad setup --check",
        ] {
            assert!(block.contains(step), "the install block is missing `{step}`");
        }
    }

    /// It is a *printed* block: nothing in this module may execute any of it.
    /// The only way to be sure is that the module never spawns a process.
    #[test]
    fn setup_never_runs_a_command() {
        // Only the shipped half — this test module mentions the forbidden names
        // in order to forbid them.
        let src = include_str!("setup.rs");
        let shipped = src.split("#[cfg(test)]").next().unwrap();
        // Assembled rather than written out, so the needle is not its own match.
        let needle = format!("Comm{}", "and::new");
        assert!(
            !shipped.contains(&needle),
            "setup.rs must PRINT the privileged steps, never run them ({needle})"
        );
        assert!(!shipped.contains("Stdio"), "setup.rs spawns nothing");
    }

    // --- `--check` against an injectable view ------------------------------

    /// A machine state, as `--check` sees it. Every field is a fact the real
    /// [`LiveHost`] reads off the filesystem or the socket.
    #[derive(Clone)]
    struct FakeHost {
        files: Vec<PathBuf>,
        socket: bool,
        uhid: Result<usize, String>,
        puck: Result<usize, String>,
        nodes: Vec<(PathBuf, Option<NodeStat>)>,
    }

    impl FakeHost {
        /// Everything installed and working: the state `--check` calls READY.
        fn ready() -> FakeHost {
            FakeHost {
                files: vec![
                    PathBuf::from("/usr/lib/udev/rules.d/60-steam-input.rules"),
                    PathBuf::from("/etc/udev/rules.d/72-hyprpad-puck.rules"),
                ],
                socket: true,
                uhid: Ok(1),
                puck: Ok(5),
                nodes: (7..12)
                    .map(|n| {
                        (
                            PathBuf::from(format!("/dev/hidraw{n}")),
                            Some(NodeStat { mode: 0o600, uid: 0, gid: 0 }),
                        )
                    })
                    .collect(),
            }
        }

        /// A machine that has never run the install block — this one, today.
        fn untouched() -> FakeHost {
            FakeHost {
                files: vec![PathBuf::from("/usr/lib/udev/rules.d/60-steam-input.rules")],
                socket: false,
                uhid: Err("no socket".to_string()),
                puck: Err("no socket".to_string()),
                nodes: (7..12)
                    .map(|n| {
                        (
                            PathBuf::from(format!("/dev/hidraw{n}")),
                            // 0660 root:root with a uaccess ACL — the group bits
                            // are the ACL mask, which is why `stat` can see it.
                            Some(NodeStat { mode: 0o660, uid: 0, gid: 0 }),
                        )
                    })
                    .collect(),
            }
        }
    }

    impl HostView for FakeHost {
        fn file_exists(&self, path: &Path) -> bool {
            self.files.iter().any(|p| p == path)
        }
        fn socket_present(&self) -> bool {
            self.socket
        }
        fn socket_path(&self) -> PathBuf {
            PathBuf::from(crate::broker::DEFAULT_SOCKET)
        }
        fn broker(&self, verb: crate::broker::Request) -> Result<usize, String> {
            match verb {
                crate::broker::Request::Uhid => self.uhid.clone(),
                crate::broker::Request::Puck => self.puck.clone(),
            }
        }
        fn puck_nodes(&self) -> Vec<(PathBuf, Option<NodeStat>)> {
            self.nodes.clone()
        }
    }

    fn verdicts(host: &FakeHost) -> Vec<Verdict> {
        check_items(host).iter().map(|c| c.verdict).collect()
    }

    /// The six items are always the same six, in the same order, whatever the
    /// machine looks like — so a reader can diff two reports line by line.
    #[test]
    fn the_check_always_reports_the_same_six_items_in_order() {
        for host in [FakeHost::ready(), FakeHost::untouched()] {
            let labels: Vec<&str> = check_items(&host).iter().map(|c| c.label).collect();
            assert_eq!(
                labels,
                vec![
                    "Valve's udev rule",
                    "hyprpad udev rule",
                    "broker socket",
                    "broker: uhid",
                    "broker: puck",
                    "puck nodes root-only",
                ]
            );
        }
    }

    #[test]
    fn a_fully_installed_machine_is_ready() {
        let host = FakeHost::ready();
        assert_eq!(verdicts(&host), vec![Verdict::Ok; 6]);
        let report = check_report(&host);
        assert!(report.contains("READY for `[gamepad] kind = \"steam\"`"), "{report}");
        assert!(!report.contains("NOT READY"), "{report}");
    }

    /// The state of this machine before anything is installed: Valve's rule is
    /// there, nothing of ours is, and the puck is wide open.
    #[test]
    fn an_untouched_machine_says_exactly_what_is_missing() {
        let host = FakeHost::untouched();
        assert_eq!(
            verdicts(&host),
            vec![
                Verdict::Ok,      // Valve's rule ships with steam
                Verdict::Bad,     // ours is not installed
                Verdict::Bad,     // no socket
                Verdict::Unknown, // nothing to ask
                Verdict::Unknown,
                Verdict::Bad, // nodes still reachable
            ]
        );
        let report = check_report(&host);
        assert!(report.contains("NOT READY"), "{report}");
        assert!(report.contains("hyprpad udev rule"), "{report}");
        assert!(report.contains("broker socket"), "{report}");
        assert!(report.contains("setup --print"), "{report}");
    }

    /// Rules installed, broker running, and the *group* not yet in this shell —
    /// the failure a file listing cannot see, and the reason the block ends with
    /// "re-login". Both verbs must fail loudly.
    #[test]
    fn a_broker_that_refuses_us_is_a_failure_not_an_unknown() {
        let host = FakeHost {
            uhid: Err("broker refused: not permitted".to_string()),
            puck: Err("broker refused: not permitted".to_string()),
            ..FakeHost::ready()
        };
        assert_eq!(
            verdicts(&host),
            vec![Verdict::Ok, Verdict::Ok, Verdict::Ok, Verdict::Bad, Verdict::Bad, Verdict::Ok]
        );
        assert!(check_report(&host).contains("not permitted"));
    }

    /// The rule file being present is not the same as udev having applied it.
    #[test]
    fn an_unapplied_rule_is_caught_by_the_node_permissions() {
        let mut host = FakeHost::ready();
        host.nodes[2].1 = Some(NodeStat { mode: 0o660, uid: 0, gid: 0 });
        let items = check_items(&host);
        let nodes = items.last().unwrap();
        assert_eq!(nodes.verdict, Verdict::Bad);
        assert!(nodes.detail.contains("/dev/hidraw9"), "{}", nodes.detail);
        assert!(nodes.detail.contains("udevadm trigger"), "{}", nodes.detail);
    }

    /// With no puck plugged in the node check cannot answer, and must say so
    /// rather than claiming success.
    #[test]
    fn no_puck_makes_the_node_check_unknown_not_ok() {
        let host = FakeHost { nodes: Vec::new(), ..FakeHost::ready() };
        let items = check_items(&host);
        assert_eq!(items.last().unwrap().verdict, Verdict::Unknown);
        assert!(items.last().unwrap().detail.contains("no 28de:1304 puck"));
        assert!(check_report(&host).contains("Could not check: puck nodes root-only"));
    }

    /// Valve's rule missing is worth saying: without it Steam never sees the
    /// *virtual* controller either, and the whole feature is pointless.
    #[test]
    fn valves_rule_missing_is_reported_in_its_own_right() {
        let host = FakeHost { files: vec![PathBuf::from("/etc/udev/rules.d/72-hyprpad-puck.rules")], ..FakeHost::ready() };
        let items = check_items(&host);
        assert_eq!(items[0].verdict, Verdict::Bad);
        assert!(items[0].detail.contains("steam-devices"), "{}", items[0].detail);
    }

    /// Either udev directory counts — a distro that ships the rule in
    /// `/usr/lib` is as installed as one that has it in `/etc`.
    #[test]
    fn the_rule_is_found_in_either_udev_directory() {
        for dir in RULE_DIRS {
            let host = FakeHost {
                files: vec![
                    PathBuf::from("/usr/lib/udev/rules.d/60-steam-input.rules"),
                    Path::new(dir).join(RULE_NAME),
                ],
                ..FakeHost::ready()
            };
            assert_eq!(check_items(&host)[1].verdict, Verdict::Ok, "{dir}");
        }
    }

    /// A broker that answers `uhid` with nothing is a broker that is not doing
    /// its job, even though the exchange "succeeded".
    #[test]
    fn an_empty_answer_is_not_a_pass() {
        let host = FakeHost { uhid: Ok(0), ..FakeHost::ready() };
        let items = check_items(&host);
        assert_eq!(items[3].verdict, Verdict::Bad);
        assert!(items[3].detail.contains("expected at least 1"));
    }

    // --- the stat-only permission rule -------------------------------------

    /// The whole basis of "root-only" without ever opening the node.
    #[test]
    fn root_only_is_decidable_from_stat_alone() {
        assert!(NodeStat { mode: 0o600, uid: 0, gid: 0 }.root_only());
        assert!(NodeStat { mode: 0o400, uid: 0, gid: 0 }.root_only());
        // A uaccess ACL raises the group bits to the ACL mask, so it is visible.
        assert!(!NodeStat { mode: 0o660, uid: 0, gid: 0 }.root_only());
        assert!(!NodeStat { mode: 0o666, uid: 0, gid: 0 }.root_only());
        assert!(!NodeStat { mode: 0o604, uid: 0, gid: 0 }.root_only());
        // Owned by anyone but root is not root-only whatever the bits say.
        assert!(!NodeStat { mode: 0o600, uid: 1000, gid: 0 }.root_only());
    }

    /// A node we cannot even stat is not evidence of success.
    #[test]
    fn an_unstattable_node_fails_the_check() {
        let host = FakeHost {
            nodes: vec![(PathBuf::from("/dev/hidraw7"), None)],
            ..FakeHost::ready()
        };
        let items = check_items(&host);
        assert_eq!(items.last().unwrap().verdict, Verdict::Bad);
        assert!(items.last().unwrap().detail.contains("cannot stat"));
    }

    /// Every line is one line, so the report is greppable.
    #[test]
    fn every_reported_item_is_a_single_line() {
        for host in [FakeHost::ready(), FakeHost::untouched()] {
            for item in check_items(&host) {
                assert!(!item.line().contains('\n'), "{:?}", item);
                assert!(item.line().starts_with("  "), "{:?}", item);
            }
            assert!(!verdict_line(&check_items(&host)).contains('\n'));
        }
    }
}
