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
//!
//! # The third half: running at login
//!
//! [`user_unit_steps`] prints the *user-level* install of
//! `packaging/systemd/user/hyprpad.service`, which starts the daemon with the
//! graphical session. It needs no root either, but it is a separate decision
//! from the Steam masking above — and it is the piece that makes
//! `[daemon] restore_lizard_on_exit = false` safe (`docs/12-lizard-free.md`),
//! since a daemon that is restarted is a puck that comes back.

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
    /// Print only the user-unit block ([`user_unit_steps`]) — the "run at login"
    /// half. Installs nothing; needs no root.
    User,
    /// Remove the user-level Steam hook.
    Revert,
}

/// `hyprpad setup`'s usage line.
pub const USAGE: &str = "usage: hyprpad setup [--check | --print | --user | --revert]";

/// Entry point for the `setup` subcommand.
pub fn run(mode: Mode) -> io::Result<()> {
    match mode {
        Mode::Print => {
            print!("{}", host_install_steps());
            println!();
            print!("{}", user_unit_steps());
            Ok(())
        }
        Mode::User => {
            print!("{}", user_unit_steps());
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
    println!();
    print!("{}", user_unit_steps());
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

// ===========================================================================
// Running at login — the systemd user unit
// ===========================================================================

/// The user unit's name, as systemd knows it.
pub const USER_UNIT_NAME: &str = "hyprpad.service";
/// The shipped file that becomes it.
pub const USER_UNIT_SOURCE: &str = "packaging/systemd/user/hyprpad.service";
/// The session target that pulls the unit in at login. `systemctl --user enable`
/// writes the symlink into `<this>.wants/`, which is how `--check` tells
/// "enabled" from "merely installed" without running `systemctl`.
pub const USER_UNIT_TARGET: &str = "graphical-session.target";
/// The group that gates the broker's socket — the one thing a user unit cannot
/// grant itself. See [`user_unit_steps`] for why.
pub const BROKER_GROUP: &str = "hyprpad";

/// The user-level block: how to run the daemon from the graphical session.
///
/// Printed, never run — the same contract as [`host_install_steps`], even though
/// nothing here needs root. Pure, so what it says is a test.
pub fn user_unit_steps() -> String {
    format!(
        "\
Running at login — the systemd user unit (no root, but read it first)
=====================================================================
This starts the daemon with your graphical session instead of from a shell, and
restarts it if it dies. It is also the half that makes `[daemon]
restore_lizard_on_exit = false` safe: with that knob off, a stopped daemon
leaves the puck doing nothing on the desktop, and `Restart=on-failure` is what
puts it back (docs/12-lizard-free.md).

    # 0. stop the hand-launched daemon, or two of them fight over the puck
    pkill -TERM -f 'hyprpad run'

    # 1. the unit
    install -Dm644 {USER_UNIT_SOURCE} \\
            ~/.config/systemd/user/{USER_UNIT_NAME}

    # 2. enable it for the session
    systemctl --user daemon-reload
    systemctl --user enable --now {USER_UNIT_NAME}

The unit runs ~/.local/bin/hyprpad, and points HYPRPAD_OSK_BIN at
~/.local/bin/hyprpad-osk. A user unit does NOT inherit your shell's PATH, so if
either symlink is missing, make it — from the checkout you built in:

    ln -sf \"$PWD/target/release/hyprpad\"          ~/.local/bin/hyprpad
    ln -sf \"$PWD/osk/target/release/hyprpad-osk\"  ~/.local/bin/hyprpad-osk

THE GROUP IS NOT SOMETHING THE UNIT CAN FIX. With the broker installed, the
daemon reaches its socket only if the process is in the `{BROKER_GROUP}` group —
and `SupplementaryGroups=` is documented in systemd.exec(5) under USER/GROUP
IDENTITY, which is \"only available for system services and not supported for
services running in per-user instances of the service manager\". A user manager
has no CAP_SETGID. What decides the group is the LOGIN SESSION the user manager
inherited, so after `sudo usermod -aG {BROKER_GROUP} $USER` you must LOG OUT AND
BACK IN — `newgrp` reaches only the shell you type it in, never the already
running user manager. Without the group nothing breaks: the daemon says so once
and opens the puck directly.

    journalctl --user -u {USER_UNIT_NAME} -f      # what it is saying
    systemctl --user reload {USER_UNIT_NAME}      # = `hyprpad reload` (SIGHUP)
    hyprpad setup --check                        # read-only, incl. the group

To undo: `systemctl --user disable --now {USER_UNIT_NAME}`, then remove
~/.config/systemd/user/{USER_UNIT_NAME} and `systemctl --user daemon-reload`.
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
    /// The controller's hidraw nodes and what `stat` says about each, labelled
    /// with the transport each arrived on. An empty vec means no controller is
    /// present on either link.
    ///
    /// Both transports can be listed at once — the dongle stays plugged in and
    /// enumerated while the controller talks over Bluetooth — and the check
    /// reports on them separately, because the two are hidden by two different
    /// clauses of the udev rule and either can be installed without the other.
    fn controller_nodes(&self) -> Vec<(PathBuf, crate::hidraw::Transport, Option<NodeStat>)>;

    /// Where the user unit lives when it is installed:
    /// `$XDG_CONFIG_HOME/systemd/user/hyprpad.service`.
    fn user_unit_file(&self) -> PathBuf;

    /// The symlink `systemctl --user enable` writes, in
    /// `<target>.wants/`. Its presence is "enabled", and reading it is how this
    /// check answers that question without running `systemctl`.
    fn user_unit_enable_link(&self) -> PathBuf;

    /// Does anything exist at `path`, symlink or not? Distinct from
    /// [`file_exists`](HostView::file_exists), which follows symlinks and wants
    /// a regular file at the end — a dangling enable symlink is still an enable
    /// symlink, and saying so is more useful than pretending it is absent.
    fn path_exists(&self, path: &Path) -> bool;

    /// The running daemon, read out of `/proc`. `None` when no live daemon
    /// could be found through the pidfile.
    fn running_daemon(&self) -> Option<DaemonProc>;

    /// The numeric gid of the [`BROKER_GROUP`] group, if the group exists.
    fn hyprpad_gid(&self) -> Option<u32>;
}

/// What `--check` can learn about the running daemon by reading `/proc`, which
/// is all it is allowed to do — it runs no commands, `systemctl` included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonProc {
    /// The pid from `$XDG_RUNTIME_DIR/hyprpad.pid`, confirmed alive and named
    /// `hyprpad` (a pidfile can be stale and pids are reused).
    pub pid: u32,
    /// Whether `/proc/<pid>/cgroup` places it inside [`USER_UNIT_NAME`] — i.e.
    /// this daemon *is* the systemd unit, not a hand-launched `hyprpad run`.
    pub under_unit: bool,
    /// Every gid the daemon holds, from both credential lines of
    /// `/proc/<pid>/status`: the four on `Gid:` (real, effective, saved, fs)
    /// and the supplementary ones on `Groups:`. Reading the process is the only
    /// honest answer to "does the daemon have the broker group", because the
    /// group is inherited from the login session and neither the unit file nor
    /// the group database can tell you what actually happened. Both lines
    /// count: `usermod -aG` plus a fresh login puts the group on `Groups:`,
    /// while `newgrp hyprpad` makes it the *primary* gid and leaves it off
    /// `Groups:` entirely — and either daemon reaches the broker.
    pub gids: Vec<u32>,
}

/// The gids on a `/proc/<pid>/status` `Groups:` line. A file with no such line
/// yields an empty list, which is a real answer: a process may have no
/// supplementary groups at all.
pub fn parse_proc_groups(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|rest| rest.split_whitespace().filter_map(|g| g.parse().ok()).collect())
        .unwrap_or_default()
}

/// The gids on a `/proc/<pid>/status` `Gid:` line — real, effective, saved
/// and filesystem, in that order. A file with no such line yields an empty
/// list.
pub fn parse_proc_gid_line(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Gid:"))
        .map(|rest| rest.split_whitespace().filter_map(|g| g.parse().ok()).collect())
        .unwrap_or_default()
}

/// Every gid a `/proc/<pid>/status` body says the process holds: the primary
/// four from `Gid:` followed by the supplementary ones from `Groups:`. The
/// kernel grants access on any of them, so a check that reads only `Groups:`
/// calls a daemon started from a `newgrp hyprpad` shell — where the group is
/// the primary gid and appears nowhere else — groupless, even as it happily
/// opens the broker's socket.
pub fn parse_proc_gids(status: &str) -> Vec<u32> {
    let mut gids = parse_proc_gid_line(status);
    gids.extend(parse_proc_groups(status));
    gids
}

/// The gid of `name` in an `/etc/group`-format body (`name:passwd:gid:members`).
/// Read rather than shelled out to, because this module runs no commands.
pub fn parse_group_gid(group_file: &str, name: &str) -> Option<u32> {
    for line in group_file.lines() {
        let mut fields = line.split(':');
        if fields.next() != Some(name) {
            continue;
        }
        // Skip the password field; the gid is the third.
        if let Some(gid) = fields.nth(1).and_then(|g| g.parse().ok()) {
            return Some(gid);
        }
    }
    None
}

/// Whether a `/proc/<pid>/cgroup` body places the process inside `unit`.
///
/// A line is `hierarchy:controllers:path`, and on cgroup v2 there is exactly
/// one: `0::/user.slice/user-1000.slice/user@1000.service/app.slice/hyprpad.service`.
/// The unit must match a whole path *segment*, or `hyprpad.service` would also
/// answer for `not-hyprpad.service`.
pub fn cgroup_names_unit(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        line.rsplit(':')
            .next()
            .is_some_and(|path| path.split('/').any(|seg| seg == unit))
    })
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
    //
    //    Note what this row does *not* distinguish: a broker binary older than
    //    the Bluetooth work answers this perfectly well (the client falls back
    //    to the pre-rename verb) but hands over only the dongle's nodes, because
    //    its own enumeration predates `28de:1303`. The `bt node root-only` row
    //    below is what makes that visible — an old broker plus a paired
    //    controller shows `ok` here and a BT node hyprpad cannot reach.
    for (verb, label, want) in [
        (broker::Request::Uhid, "broker: uhid", 1usize),
        (broker::Request::Controller, "broker: controller", 1usize),
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

    // 6. Are the controller's nodes actually closed to us? The rule being
    //    *installed* is not the same as the rule having been *applied*: udev
    //    only re-runs on a trigger or a replug.
    //
    //    Reported per transport, because they are two clauses of one rule and
    //    either can be in force without the other — a machine that installed
    //    the rule before Bluetooth support existed has the USB clause and not
    //    the BLE one, and its symptom (Steam sees the real controller, every
    //    press doubled) would otherwise be invisible here.
    let nodes = view.controller_nodes();
    items.push(node_check(
        &nodes,
        crate::hidraw::Transport::Dongle,
        "puck nodes root-only",
        "no 28de:1304 puck present — plug it in and re-run",
    ));
    // The Bluetooth row is only shown when there is something to say about it.
    // A tower with no bond has no BLE node and never will; printing a permanent
    // "unknown" there would be noise, and the pairing chord is a README matter
    // rather than a setup step.
    if nodes.iter().any(|(_, t, _)| *t == crate::hidraw::Transport::Bluetooth) {
        items.push(node_check(
            &nodes,
            crate::hidraw::Transport::Bluetooth,
            "bt node root-only",
            "no 28de:1303 controller paired over Bluetooth",
        ));
    }

    items
}

/// One "are these nodes closed to us" row, for the nodes of a single transport.
///
/// Split out so the two transports are checked by identical logic rather than by
/// two copies of it: the question, the remedy and the failure mode are the same
/// on both, and only the label and the "nothing here" wording differ.
fn node_check(
    nodes: &[(PathBuf, crate::hidraw::Transport, Option<NodeStat>)],
    transport: crate::hidraw::Transport,
    label: &'static str,
    absent: &'static str,
) -> Check {
    let mine: Vec<&(PathBuf, crate::hidraw::Transport, Option<NodeStat>)> =
        nodes.iter().filter(|(_, t, _)| *t == transport).collect();
    if mine.is_empty() {
        return Check::new(Verdict::Unknown, label, absent);
    }
    let open: Vec<String> = mine
        .iter()
        .filter(|(_, _, st)| !st.is_some_and(NodeStat::root_only))
        .map(|(p, _, st)| match st {
            Some(s) => format!("{} ({:04o} uid {})", p.display(), s.mode, s.uid),
            None => format!("{} (cannot stat)", p.display()),
        })
        .collect();
    if open.is_empty() {
        Check::new(Verdict::Ok, label, format!("{} node(s), all 0600 root:root", mine.len()))
    } else {
        Check::new(
            Verdict::Bad,
            label,
            format!(
                "still reachable: {} — run `sudo udevadm control --reload && \
                 sudo udevadm trigger --subsystem-match=hidraw`, or replug",
                open.join(", ")
            ),
        )
    }
}

/// The "running at login" checks. Deliberately a **separate** list from
/// [`check_items`]: the user unit is optional and answers a different question
/// (does the daemon start itself) from the Steam masking above (can Steam see
/// only the fake), so a machine with no unit installed must not be reported as
/// not ready for the relay.
pub fn user_unit_items(view: &dyn HostView) -> Vec<Check> {
    let mut items = Vec::new();

    // 1. The unit file itself.
    let unit = view.user_unit_file();
    items.push(if view.path_exists(&unit) {
        Check::new(Verdict::Ok, "user unit", format!("{}", unit.display()))
    } else {
        Check::new(
            Verdict::Bad,
            "user unit",
            format!(
                "nothing at {} — `hyprpad setup --user` prints the install",
                unit.display()
            ),
        )
    });

    // 2. Enabled, read as the `.wants/` symlink rather than by running
    //    `systemctl is-enabled` — this module runs nothing.
    let link = view.user_unit_enable_link();
    items.push(if view.path_exists(&link) {
        Check::new(Verdict::Ok, "user unit enabled", format!("wanted by {USER_UNIT_TARGET}"))
    } else {
        Check::new(
            Verdict::Bad,
            "user unit enabled",
            format!("no {} — `systemctl --user enable --now {USER_UNIT_NAME}`", link.display()),
        )
    });

    // 3. Is the daemon that is running *the unit's*? This is the check that
    //    catches the state the first two cannot see: unit installed and enabled,
    //    and a hand-launched `hyprpad run` from a shell holding the puck, so the
    //    unit's own start silently loses the race for the device.
    let daemon = view.running_daemon();
    items.push(match &daemon {
        None => Check::new(
            Verdict::Unknown,
            "daemon under the unit",
            "no daemon running — start it with `systemctl --user start hyprpad`",
        ),
        Some(d) if d.under_unit => Check::new(
            Verdict::Ok,
            "daemon under the unit",
            format!("pid {} is in {USER_UNIT_NAME}'s cgroup", d.pid),
        ),
        Some(d) => Check::new(
            Verdict::Bad,
            "daemon under the unit",
            format!(
                "pid {} is hand-launched — `pkill -TERM -f 'hyprpad run'`, then \
                 `systemctl --user start {USER_UNIT_NAME}`",
                d.pid
            ),
        ),
    });

    // 4. And the group, which is the whole reason this section exists: a user
    //    unit cannot grant it, so the only way to know is to look at the process.
    let gid = view.hyprpad_gid();
    items.push(match (gid, &daemon) {
        (None, _) => Check::new(
            Verdict::Bad,
            "daemon has the group",
            format!("no `{BROKER_GROUP}` group here — see `hyprpad setup --print` step 2"),
        ),
        (Some(_), None) => Check::new(
            Verdict::Unknown,
            "daemon has the group",
            "unknown until started — it is read off the running daemon",
        ),
        (Some(g), Some(d)) if d.gids.contains(&g) => Check::new(
            Verdict::Ok,
            "daemon has the group",
            format!("gid {g} ({BROKER_GROUP}) — the broker will answer it"),
        ),
        (Some(g), Some(d)) => Check::new(
            Verdict::Bad,
            "daemon has the group",
            format!(
                "pid {} lacks gid {g} ({BROKER_GROUP}) — a user unit cannot add it, so \
                 RE-LOGIN after `sudo usermod -aG {BROKER_GROUP} $USER`",
                d.pid
            ),
        ),
    });

    items
}

/// The user-unit section's closing line. Same shape as [`verdict_line`], and
/// deliberately worded so it can never be mistaken for the relay's verdict.
pub fn user_unit_line(items: &[Check]) -> String {
    let bad: Vec<&str> =
        items.iter().filter(|c| c.verdict == Verdict::Bad).map(|c| c.label).collect();
    let unknown: Vec<&str> =
        items.iter().filter(|c| c.verdict == Verdict::Unknown).map(|c| c.label).collect();
    if bad.is_empty() && unknown.is_empty() {
        return "Running at login: the unit is installed, enabled, and the daemon is its own."
            .to_string();
    }
    let mut out = String::from("NOT running from the unit. ");
    if !bad.is_empty() {
        out.push_str(&format!("Missing or wrong: {}. ", bad.join(", ")));
    }
    if !unknown.is_empty() {
        out.push_str(&format!("Could not check: {}. ", unknown.join(", ")));
    }
    out.push_str("See `hyprpad setup --user` for the install block.");
    out
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

    // The login half, under its own heading and its own verdict — see
    // `user_unit_items` for why the two lists are never merged.
    let user = user_unit_items(view);
    out.push_str("\nRunning at login (optional; `hyprpad setup --user` prints the install)\n\n");
    for item in &user {
        out.push_str(&item.line());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&user_unit_line(&user));
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

    fn controller_nodes(&self) -> Vec<(PathBuf, crate::hidraw::Transport, Option<NodeStat>)> {
        use std::os::unix::fs::MetadataExt;
        crate::hidraw::controller_nodes_by_transport()
            .unwrap_or_default()
            .into_iter()
            .map(|(p, t)| {
                let st = fs::metadata(&p)
                    .ok()
                    .map(|m| NodeStat { mode: m.mode() & 0o7777, uid: m.uid(), gid: m.gid() });
                (p, t, st)
            })
            .collect()
    }

    fn user_unit_file(&self) -> PathBuf {
        user_unit_dir().join(USER_UNIT_NAME)
    }

    fn user_unit_enable_link(&self) -> PathBuf {
        user_unit_dir().join(format!("{USER_UNIT_TARGET}.wants")).join(USER_UNIT_NAME)
    }

    fn path_exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn running_daemon(&self) -> Option<DaemonProc> {
        let pid: u32 = fs::read_to_string(crate::run::pid_file_path()?)
            .ok()?
            .trim()
            .parse()
            .ok()?;
        // A pidfile outlives a signal-driven exit and pids get reused, so the
        // pid alone proves nothing: only a live process actually named hyprpad
        // counts as the daemon.
        if fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim() != "hyprpad" {
            return None;
        }
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        Some(DaemonProc {
            pid,
            under_unit: cgroup_names_unit(&cgroup, USER_UNIT_NAME),
            gids: parse_proc_gids(&status),
        })
    }

    fn hyprpad_gid(&self) -> Option<u32> {
        parse_group_gid(&fs::read_to_string("/etc/group").ok()?, BROKER_GROUP)
    }
}

/// `$XDG_CONFIG_HOME/systemd/user`, or `~/.config/systemd/user` — read exactly
/// the way systemd itself resolves it, so the paths `--check` reports are the
/// ones `systemctl --user` would use.
fn user_unit_dir() -> PathBuf {
    crate::config::Config::config_home()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("systemd/user")
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
        // Both printed blocks together: the root half and the login half. A new
        // file under `packaging/` that neither mentions is an incomplete install.
        let block = format!("{}{}", host_install_steps(), user_unit_steps());
        for shipped in [
            "packaging/udev/72-hyprpad-puck.rules",
            "packaging/sysusers.d/hyprpad.conf",
            "packaging/systemd/hyprpad-broker.socket",
            "packaging/systemd/hyprpad-broker.service",
            "packaging/systemd/user/hyprpad.service",
        ] {
            assert!(block.contains(shipped), "the install block never mentions {shipped}");
        }
        // And the file names match the constants `--check` looks for.
        assert!(block.contains(RULE_NAME));
        assert!(block.contains(USER_UNIT_SOURCE));
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
        nodes: Vec<(PathBuf, crate::hidraw::Transport, Option<NodeStat>)>,
        /// Paths that exist for [`HostView::path_exists`] — the unit file and
        /// its enable symlink.
        present: Vec<PathBuf>,
        daemon: Option<DaemonProc>,
        gid: Option<u32>,
    }

    /// A `/proc/<pid>/status` body, tab-separated the way the kernel writes
    /// it, with `gid` on all four fields of the primary `Gid:` line and
    /// `groups` on the supplementary `Groups:` line. The fixtures build their
    /// daemons out of this rather than out of a gid list, so what the check is
    /// given is what the kernel would actually have written.
    fn status_body(gid: u32, groups: &[u32]) -> String {
        let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
        format!(
            "Name:\thyprpad\nUid:\t1000\t1000\t1000\t1000\n\
             Gid:\t{gid}\t{gid}\t{gid}\t{gid}\n\
             Groups:\t{} \nThreads:\t9\n",
            groups.join(" ")
        )
    }

    /// The unit file and the enable symlink, as [`FakeHost`] spells them.
    const FAKE_UNIT: &str = "/home/u/.config/systemd/user/hyprpad.service";
    const FAKE_LINK: &str =
        "/home/u/.config/systemd/user/graphical-session.target.wants/hyprpad.service";

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
                            crate::hidraw::Transport::Dongle,
                            Some(NodeStat { mode: 0o600, uid: 0, gid: 0 }),
                        )
                    })
                    .collect(),
                present: vec![PathBuf::from(FAKE_UNIT), PathBuf::from(FAKE_LINK)],
                daemon: Some(DaemonProc {
                    pid: 4242,
                    under_unit: true,
                    // `usermod -aG` plus a fresh login: 1000 primary, 949
                    // supplementary.
                    gids: parse_proc_gids(&status_body(1000, &[949, 1000])),
                }),
                gid: Some(949),
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
                            crate::hidraw::Transport::Dongle,
                            // 0660 root:root with a uaccess ACL — the group bits
                            // are the ACL mask, which is why `stat` can see it.
                            Some(NodeStat { mode: 0o660, uid: 0, gid: 0 }),
                        )
                    })
                    .collect(),
                // No unit, no group, and the daemon started by hand from a
                // shell — the state of this machine today.
                present: Vec::new(),
                daemon: Some(DaemonProc {
                    pid: 4242,
                    under_unit: false,
                    gids: parse_proc_gids(&status_body(1000, &[1000])),
                }),
                gid: None,
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
                crate::broker::Request::Controller => self.puck.clone(),
            }
        }
        fn controller_nodes(&self) -> Vec<(PathBuf, crate::hidraw::Transport, Option<NodeStat>)> {
            self.nodes.clone()
        }
        fn user_unit_file(&self) -> PathBuf {
            PathBuf::from(FAKE_UNIT)
        }
        fn user_unit_enable_link(&self) -> PathBuf {
            PathBuf::from(FAKE_LINK)
        }
        fn path_exists(&self, path: &Path) -> bool {
            self.present.iter().any(|p| p == path)
        }
        fn running_daemon(&self) -> Option<DaemonProc> {
            self.daemon.clone()
        }
        fn hyprpad_gid(&self) -> Option<u32> {
            self.gid
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
                    "broker: controller",
                    "puck nodes root-only",
                ]
            );
        }
    }

    /// The seventh row is conditional and always last, so the six above keep
    /// their positions and anything parsing the report by order still works.
    #[test]
    fn the_bluetooth_row_is_appended_and_never_reorders_the_others() {
        let mut host = FakeHost::ready();
        host.nodes.push((
            PathBuf::from("/dev/hidraw13"),
            crate::hidraw::Transport::Bluetooth,
            Some(NodeStat { mode: 0o600, uid: 0, gid: 0 }),
        ));
        let labels: Vec<&str> = check_items(&host).iter().map(|c| c.label).collect();
        assert_eq!(
            labels,
            vec![
                "Valve's udev rule",
                "hyprpad udev rule",
                "broker socket",
                "broker: uhid",
                "broker: controller",
                "puck nodes root-only",
                "bt node root-only",
            ]
        );
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
        host.nodes[2].2 = Some(NodeStat { mode: 0o660, uid: 0, gid: 0 });
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

    // -----------------------------------------------------------------------
    // The Bluetooth clause
    // -----------------------------------------------------------------------

    /// A path to a file shipped in this repo, so the tests below check the
    /// artefact that is actually installed rather than a transcription of it.
    fn repo_file(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// The shipped rules file must be syntactically valid — this is the one
    /// artefact in the repo that is consumed by another program's parser, and a
    /// typo in it is silent (udev logs and moves on, the node stays reachable,
    /// Steam sees two controllers).
    ///
    /// Read-only and installs nothing. Skipped where `udevadm` is not present
    /// rather than failed: it is a check on the file, not on the host.
    #[test]
    fn the_shipped_udev_rules_file_is_valid() {
        let path = repo_file("packaging/udev/72-hyprpad-puck.rules");
        let Ok(out) = std::process::Command::new("udevadm").arg("verify").arg(&path).output()
        else {
            eprintln!("skipping: no udevadm on this machine");
            return;
        };
        assert!(
            out.status.success(),
            "udevadm verify rejected {}:\n{}{}",
            path.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// Both transports are hidden, and the fake is not.
    ///
    /// The rule is what stops Steam seeing the real controller alongside
    /// hyprpad's relay; a missing clause is not a degraded mode but a doubled
    /// input on every press, so each identity is pinned by hand.
    #[test]
    fn the_udev_rule_hides_both_transports_and_neither_more() {
        let text = std::fs::read_to_string(repo_file("packaging/udev/72-hyprpad-puck.rules"))
            .expect("the rule ships in the repo");
        // Only the rules themselves, not the file's (extensive) commentary —
        // the comments name every id under discussion, including the ones that
        // must *not* be matched.
        let rules: String = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        // The dongle, matched on its USB parent.
        assert!(
            rules.contains(r#"ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304""#),
            "the USB clause"
        );
        // Bluetooth, matched on the parent HID device's kernel name — there is
        // no USB parent and a hidraw node carries no HID_ID of its own.
        assert!(rules.contains(r#"KERNELS=="0005:28DE:1303.*""#), "the Bluetooth clause");

        // Both clauses do the same four things, so neither can be half-applied.
        let acted = rules.matches(r#"TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0600""#);
        assert_eq!(acted.count(), 2, "one action per transport");

        // The relay's own fake is 0003:28DE:1302 and must keep the ACL Valve's
        // rule grants it — if a *rule* here ever mentions 1302, the daemon has
        // hidden its own output device from Steam.
        assert!(!rules.contains("1302"), "the fake must stay visible to Steam");
        assert!(!rules.contains("12f0"), "…and so must the other fake identity");
        // …and the glob must not be widened to catch it either.
        assert!(!rules.contains("130?") && !rules.contains("13??"), "the ids are exact");
        assert!(!rules.contains(r#"ATTRS{idProduct}=="*""#), "no vendor-only match");

        // The instance suffix must stay globbed: BlueZ destroys and recreates
        // the device on every disconnect, so the number always changes.
        assert!(!rules.contains("1303.0000"), "the instance suffix is never pinned");
        // And the whole file stays behind the two guards that make it cheap.
        assert!(rules.contains(r#"ACTION=="remove", GOTO="hyprpad_puck_end""#));
        assert!(rules.contains(r#"SUBSYSTEM!="hidraw", GOTO="hyprpad_puck_end""#));
    }

    /// The Bluetooth row appears in `--check` when there is a BLE node, and
    /// stays out of the way when there is not.
    #[test]
    fn the_check_reports_the_bluetooth_node_only_when_one_is_paired() {
        use crate::hidraw::Transport;

        // A tower: dongle only. No BT row at all — a permanent "unknown" there
        // would be noise on a machine that will never have a bond.
        let dongle_only = FakeHost::ready();
        let labels: Vec<&str> =
            check_items(&dongle_only).iter().map(|i| i.label).collect::<Vec<_>>();
        assert!(labels.contains(&"puck nodes root-only"));
        assert!(!labels.contains(&"bt node root-only"), "{labels:?}");

        // A laptop with the controller on Bluetooth: the dongle is still
        // plugged in and enumerated, and both rows are reported.
        let mut both = FakeHost::ready();
        both.nodes.push((
            PathBuf::from("/dev/hidraw13"),
            Transport::Bluetooth,
            Some(NodeStat { mode: 0o600, uid: 0, gid: 0 }),
        ));
        let items = check_items(&both);
        let bt = items.iter().find(|i| i.label == "bt node root-only").expect("a BT row");
        assert_eq!(bt.verdict, Verdict::Ok);
        assert!(bt.detail.contains("1 node(s)"), "exactly one BLE interface: {}", bt.detail);
        // The dongle's row is unaffected by the BT node's presence.
        let usb = items.iter().find(|i| i.label == "puck nodes root-only").unwrap();
        assert_eq!(usb.verdict, Verdict::Ok);
        assert!(usb.detail.contains("5 node(s)"), "{}", usb.detail);
    }

    /// The failure this row exists to catch: the rule was installed before the
    /// Bluetooth clause existed, so the dongle is hidden and the controller is
    /// not. Steam would see the real controller and hyprpad's fake at once.
    #[test]
    fn a_reachable_bluetooth_node_fails_its_own_row_and_not_the_dongles() {
        use crate::hidraw::Transport;

        let mut host = FakeHost::ready();
        host.nodes.push((
            PathBuf::from("/dev/hidraw13"),
            Transport::Bluetooth,
            // 0660 root:root with a uaccess ACL — exactly what Valve's
            // 60-steam-input.rules leaves behind when our clause is missing.
            Some(NodeStat { mode: 0o660, uid: 0, gid: 0 }),
        ));
        let items = check_items(&host);

        let bt = items.iter().find(|i| i.label == "bt node root-only").unwrap();
        assert_eq!(bt.verdict, Verdict::Bad);
        assert!(bt.detail.contains("/dev/hidraw13"), "{}", bt.detail);
        assert!(bt.detail.contains("udevadm"), "the remedy is in the line: {}", bt.detail);

        // And the dongle's row still passes, so the report says which half is
        // wrong rather than just that something is.
        let usb = items.iter().find(|i| i.label == "puck nodes root-only").unwrap();
        assert_eq!(usb.verdict, Verdict::Ok);
    }

    /// A node we cannot even stat is not evidence of success.
    #[test]
    fn an_unstattable_node_fails_the_check() {
        let host = FakeHost {
            nodes: vec![(PathBuf::from("/dev/hidraw7"), crate::hidraw::Transport::Dongle, None)],
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
            for item in check_items(&host).into_iter().chain(user_unit_items(&host)) {
                assert!(!item.line().contains('\n'), "{:?}", item);
                assert!(item.line().starts_with("  "), "{:?}", item);
            }
            assert!(!verdict_line(&check_items(&host)).contains('\n'));
            assert!(!user_unit_line(&user_unit_items(&host)).contains('\n'));
        }
    }

    // --- the shipped user unit ---------------------------------------------

    /// The unit file that `user_unit_steps` tells people to install.
    const USER_UNIT: &str = include_str!("../packaging/systemd/user/hyprpad.service");

    /// Everything the unit is *for*, asserted against the shipped file rather
    /// than against a copy of it in a comment.
    #[test]
    fn the_shipped_user_unit_says_what_it_must() {
        for directive in [
            "ExecStart=%h/.local/bin/hyprpad run",
            "ExecReload=%h/.local/bin/hyprpad reload",
            "Environment=HYPRPAD_OSK_BIN=%h/.local/bin/hyprpad-osk",
            "Restart=on-failure",
            "RestartSec=2",
            "After=graphical-session.target",
            "PartOf=graphical-session.target",
            "WantedBy=graphical-session.target",
            "StandardOutput=journal",
        ] {
            assert!(USER_UNIT.contains(directive), "the unit is missing `{directive}`");
        }
    }

    /// The trap this whole section exists to document: `SupplementaryGroups=`
    /// is a system-service-only directive (systemd.exec(5), USER/GROUP
    /// IDENTITY) that `systemd-analyze --user verify` accepts *without
    /// complaint*, so nothing but a test stops it being re-added.
    #[test]
    fn the_user_unit_never_tries_to_set_a_supplementary_group() {
        // The directive, not the word: the file explains at length why it is
        // absent, and that explanation must be allowed to name it.
        for line in USER_UNIT.lines() {
            assert!(
                !line.trim_start().starts_with("SupplementaryGroups="),
                "a user manager has no CAP_SETGID; this directive cannot work here: {line}"
            );
        }
        // And the explanation is present, so the next reader does not re-add it.
        assert!(USER_UNIT.contains("SupplementaryGroups"), "the unit must say why it is absent");
        assert!(USER_UNIT.contains("usermod -aG hyprpad"));
    }

    /// The unit runs the *installed symlink*, never a build-tree path, or it
    /// would only work for the one checkout it was written in.
    #[test]
    fn the_user_unit_runs_the_installed_binary_not_the_build_tree() {
        assert!(!USER_UNIT.contains("target/release/hyprpad run"));
        assert!(!USER_UNIT.contains("ExecStart=cargo"));
    }

    // --- the printed user block --------------------------------------------

    #[test]
    fn the_user_block_carries_every_required_step() {
        let block = user_unit_steps();
        for step in [
            "pkill -TERM -f 'hyprpad run'",
            "install -Dm644 packaging/systemd/user/hyprpad.service",
            "~/.config/systemd/user/hyprpad.service",
            "systemctl --user daemon-reload",
            "systemctl --user enable --now hyprpad.service",
            "hyprpad-osk",
            "usermod -aG hyprpad",
            "hyprpad setup --check",
        ] {
            assert!(block.contains(step), "the user block is missing `{step}`");
        }
        // The two facts that are easy to get wrong and expensive to debug.
        assert!(block.contains("SupplementaryGroups="), "the block must name the trap");
        assert!(block.contains("LOG OUT AND"), "the block must say re-login, not newgrp");
        assert!(block.contains("restore_lizard_on_exit"), "the block must say what it is for");
    }

    /// `--user` is reachable and advertised, and `--print` shows both halves —
    /// so nobody who runs the documented command misses the login unit.
    #[test]
    fn the_user_block_is_advertised_and_included_in_print() {
        assert!(USAGE.contains("--user"), "{USAGE}");
        // `--print`'s output is the two blocks concatenated, and the user block
        // must be all of the second one.
        let both = format!("{}\n{}", host_install_steps(), user_unit_steps());
        assert!(both.contains(&user_unit_steps()));
        assert!(both.contains("Host integration"));
        assert!(both.contains("Running at login"));
    }

    // --- `--check`'s login section -----------------------------------------

    fn user_verdicts(host: &FakeHost) -> Vec<Verdict> {
        user_unit_items(host).iter().map(|c| c.verdict).collect()
    }

    /// Four items, always the same four in the same order — the same contract
    /// the six relay items have.
    #[test]
    fn the_login_section_always_reports_the_same_four_items_in_order() {
        for host in [FakeHost::ready(), FakeHost::untouched()] {
            let labels: Vec<&str> = user_unit_items(&host).iter().map(|c| c.label).collect();
            assert_eq!(
                labels,
                vec![
                    "user unit",
                    "user unit enabled",
                    "daemon under the unit",
                    "daemon has the group",
                ]
            );
        }
    }

    #[test]
    fn a_machine_running_from_the_unit_passes_the_login_section() {
        let host = FakeHost::ready();
        assert_eq!(user_verdicts(&host), vec![Verdict::Ok; 4]);
        let report = check_report(&host);
        assert!(report.contains("Running at login: the unit is installed"), "{report}");
        assert!(!report.contains("NOT running from the unit"), "{report}");
    }

    /// The state of this machine today: no unit, no group, and a hand-launched
    /// daemon. Every one of those is a separate, actionable line.
    #[test]
    fn a_hand_launched_daemon_is_reported_as_such() {
        let host = FakeHost::untouched();
        assert_eq!(user_verdicts(&host), vec![Verdict::Bad; 4]);
        let items = user_unit_items(&host);
        assert!(items[0].detail.contains("setup --user"), "{}", items[0].detail);
        assert!(items[1].detail.contains("systemctl --user enable"), "{}", items[1].detail);
        assert!(items[2].detail.contains("pkill -TERM"), "{}", items[2].detail);
        assert!(items[3].detail.contains("setup --print"), "{}", items[3].detail);
        assert!(check_report(&host).contains("NOT running from the unit"));
    }

    /// The failure the machine actually has: everything installed, the group in
    /// the database, and the login session that started the user manager older
    /// than the `usermod`. Nothing but the daemon's own `/proc` entry can see it.
    #[test]
    fn a_daemon_whose_session_predates_the_group_is_caught() {
        let host = FakeHost {
            daemon: Some(DaemonProc {
                pid: 4242,
                under_unit: true,
                gids: parse_proc_gids(&status_body(1000, &[1000])),
            }),
            ..FakeHost::ready()
        };
        let items = user_unit_items(&host);
        assert_eq!(items[3].verdict, Verdict::Bad);
        assert!(items[3].detail.contains("RE-LOGIN"), "{}", items[3].detail);
        assert!(items[3].detail.contains("949"), "{}", items[3].detail);
        // And the first three still pass — this is not a unit problem.
        assert_eq!(items[0].verdict, Verdict::Ok);
        assert_eq!(items[2].verdict, Verdict::Ok);
    }

    /// Either credential line answers "does the daemon have the group". A
    /// daemon started from a `newgrp hyprpad` shell carries 949 as its
    /// *primary* gid and nowhere else; it reaches the broker exactly like one
    /// holding the group supplementary, so it must not be told to re-login.
    /// Reading `Groups:` alone once called such a daemon groupless.
    #[test]
    fn the_group_counts_from_either_credential_line() {
        for (primary, supplementary, want) in [
            // `usermod -aG hyprpad` and a fresh login: supplementary only.
            (1000, &[949, 1000][..], Verdict::Ok),
            // `newgrp hyprpad`, which is how this machine starts its daemon:
            // primary only, and `Groups:` never mentions 949.
            (949, &[958, 967, 1000][..], Verdict::Ok),
            // Neither: the session that started the daemon predates the
            // `usermod`, and only a re-login fixes it.
            (1000, &[958, 967, 1000][..], Verdict::Bad),
        ] {
            let status = status_body(primary, supplementary);
            let host = FakeHost {
                daemon: Some(DaemonProc {
                    pid: 4242,
                    under_unit: true,
                    gids: parse_proc_gids(&status),
                }),
                ..FakeHost::ready()
            };
            let item = &user_unit_items(&host)[3];
            let seen = format!("Gid: {primary}, Groups: {supplementary:?}");
            assert_eq!(item.verdict, want, "{seen} — {}", item.detail);
        }
    }

    /// With no daemon running, the two questions that need a process must say
    /// "unknown", never "ok".
    #[test]
    fn no_running_daemon_makes_the_process_checks_unknown() {
        let host = FakeHost { daemon: None, ..FakeHost::ready() };
        assert_eq!(
            user_verdicts(&host),
            vec![Verdict::Ok, Verdict::Ok, Verdict::Unknown, Verdict::Unknown]
        );
        let items = user_unit_items(&host);
        assert!(items[3].detail.contains("unknown until started"), "{}", items[3].detail);
    }

    /// The login section must never change the relay's verdict: a machine with
    /// no user unit at all is still READY for `kind = "steam"`.
    #[test]
    fn the_login_section_does_not_change_the_relay_verdict() {
        let host = FakeHost { present: Vec::new(), gid: None, daemon: None, ..FakeHost::ready() };
        assert_eq!(verdicts(&host), vec![Verdict::Ok; 6]);
        let report = check_report(&host);
        assert!(report.contains("READY for `[gamepad] kind = \"steam\"`"), "{report}");
        assert!(report.contains("NOT running from the unit"), "{report}");
    }

    // --- the /proc parsers --------------------------------------------------

    #[test]
    fn the_groups_line_is_read_off_a_real_status_file() {
        // Trimmed from /proc/<pid>/status, tab-separated as the kernel writes it.
        let status = "Name:\thyprpad\nUid:\t1000\t1000\t1000\t1000\n\
                      Gid:\t1000\t1000\t1000\t1000\n\
                      Groups:\t958 967 990 992 998 1000 \nThreads:\t9\n";
        assert_eq!(parse_proc_groups(status), vec![958, 967, 990, 992, 998, 1000]);
        // A process with no supplementary groups writes an empty line, and that
        // is an answer, not a failure to parse.
        assert!(parse_proc_groups("Name:\thyprpad\nGroups:\t\n").is_empty());
        assert!(parse_proc_groups("Name:\thyprpad\n").is_empty());
    }

    /// The primary line is four fields — real, effective, saved and fs — and
    /// any of them holding the group means the process holds it. `newgrp` sets
    /// all four; an sgid binary would set only some.
    #[test]
    fn the_gid_line_is_read_off_a_real_status_file() {
        let newgrp = "Name:\thyprpad\nGid:\t949\t949\t949\t949\nGroups:\t1000 \n";
        assert_eq!(parse_proc_gid_line(newgrp), vec![949, 949, 949, 949]);
        // Both lines together, primary first — what the check actually asks.
        assert_eq!(parse_proc_gids(newgrp), vec![949, 949, 949, 949, 1000]);
        // A daemon that got the group the documented way has it on `Groups:`
        // instead, and both must count.
        let usermod = "Name:\thyprpad\nGid:\t1000\t1000\t1000\t1000\nGroups:\t949 1000 \n";
        assert!(parse_proc_gids(usermod).contains(&949));
        // Neither line: the state the re-login advice is for.
        let stale = "Name:\thyprpad\nGid:\t1000\t1000\t1000\t1000\nGroups:\t1000 \n";
        assert!(!parse_proc_gids(stale).contains(&949));
        // A body with no `Gid:` line at all is an empty answer, not a panic.
        assert!(parse_proc_gid_line("Name:\thyprpad\n").is_empty());
        assert_eq!(parse_proc_gids("Name:\thyprpad\nGroups:\t1000 \n"), vec![1000]);
    }

    #[test]
    fn the_group_gid_is_read_off_an_etc_group_body() {
        let group = "root:x:0:\nwheel:x:998:ajg\nhyprpad:x:949:ajg\ninput:x:994:ajg\n";
        assert_eq!(parse_group_gid(group, "hyprpad"), Some(949));
        assert_eq!(parse_group_gid(group, "wheel"), Some(998));
        assert_eq!(parse_group_gid(group, "nosuch"), None);
        // A name that is only a *prefix* of a real one must not match.
        assert_eq!(parse_group_gid(group, "hyp"), None);
    }

    #[test]
    fn the_cgroup_path_places_the_daemon_in_its_unit() {
        let under = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/hyprpad.service\n";
        assert!(cgroup_names_unit(under, USER_UNIT_NAME));
        // Hand-launched from a terminal: the session scope, not the unit.
        let byhand = "0::/user.slice/user-1000.slice/session-2.scope\n";
        assert!(!cgroup_names_unit(byhand, USER_UNIT_NAME));
        // A segment must match whole, or a similarly-named unit answers for us.
        let neighbour = "0::/user.slice/user-1000.slice/user@1000.service/not-hyprpad.service\n";
        assert!(!cgroup_names_unit(neighbour, USER_UNIT_NAME));
        assert!(!cgroup_names_unit("", USER_UNIT_NAME));
    }
}
