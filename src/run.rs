//! The daemon pipeline: passive controller tap -> gesture recognition ->
//! config lookup -> mode gate -> Hyprland dispatch, or forwarding to the
//! virtual gamepad a focused game sees.
//!
//! Two threads feed one loop. `hidraw::read_all` streams raw reports (merged
//! across the puck's pairing slots); `hypr::subscribe` streams compositor
//! events. The main loop owns the [`GestureEngine`], [`Config`], and
//! [`ModeEngine`], and executes [`Action`]s via [`Hypr`].
//!
//! ## Who owns the controller on any given frame
//!
//! Four layers claim the same device; [`gamepad_forwarding`] is the single place
//! the precedence is written down, and [`crate::gamepad`]'s module docs explain
//! why it is ordered this way. Highest first:
//!
//! 1. **Guide** — `guide_active()`. Every per-frame handler drops out while the
//!    guide button is held, in a game as much as on the desktop — with two
//!    opt-in exceptions, both of them a pad the guide layer would otherwise
//!    leave idle being handed to the desktop rather than to nobody. The guide
//!    layer is still rank 1 for both: the game gets nothing either way.
//!     * where `h.cursor { guide_in = … }` lists the active mode, the **right
//!       pad keeps driving the desktop cursor under the guide**
//!       ([`cursor_active`]), which is how a game is pointed at without leaving
//!       it (Steam Input's own "guide + pad = mouse");
//!     * where `h.scrub`'s guard passes, circling the **left pad** taps the
//!       arrow keys — the caret jog wheel ([`drive_scrub`],
//!       `docs/research/text-scrub.md`). The two never meet: one reads the
//!       right pad, the other the left, and neither touches the sticks that
//!       `GuideStickFlick` needs.
//!
//!    Both spend the hold ([`GestureEngine::consume_hold`]) the moment they
//!    actually do something, so the guide's release is never handed on as the
//!    bare tap Steam — or a `guide_tap` binding — acts on.
//! 2. **On-screen keyboard** — `osk.is_active()`. It owns both pads.
//! 3. **Game forwarding** — [`drive_gamepad`], when the active mode forwards
//!    and neither layer above is claiming.
//! 4. **Desktop** — [`drive_cursor`] / [`drive_scroll`] / [`drive_scrub`] /
//!    [`fire_buttons`] / [`drive_buttons`], each gated by its own guard in the
//!    active mode.
//!
//! Ranks 3 and 4 are mutually exclusive by construction: forwarding needs
//! `ModeEngine::desktop_yielded()`, which is true exactly when neither the
//! cursor nor scroll is live. On every transition *out* of rank 3 the virtual
//! pad is sent one `neutral()` report and the rumble is stopped, so a game is
//! never left holding input hyprpad has stopped feeding it.
//!
//! ## Where modality is consulted
//!
//! [`ModeEngine`] re-resolves only on a **context change** — a compositor
//! event ([`update_modes`]), a manual override from a binding
//! ([`handle_gesture`], [`fire_buttons`]), or a reload ([`apply_reload`]).
//! Every per-frame read is a cached lookup:
//!
//! | handler | consults |
//! |---|---|
//! | [`handle_gesture`] | `modes.allows_gesture(..)` + `config.resolve_in(.., modes.state())` |
//! | [`drive_cursor`] | `modes.cursor_enabled()` with the guide up, `modes.cursor_guide_enabled()` with it held ([`cursor_active`]) |
//! | [`drive_scroll`] | `modes.scroll_enabled()` |
//! | [`drive_scrub`] | `config.scrub_enabled_in(modes.state())` with the guide held |
//! | [`fire_buttons`] / [`drive_buttons`] | `modes.buttons()` (already filtered by each binding's guard) |
//! | [`drive_gamepad`] | `gamepad_forwarding(.., modes.forwards(), modes.desktop_yielded(), ..)` |
//!
//! A mode transition runs the same clean handoff a controller disconnect does
//! ([`reset_frame_state`] + `GamepadState::release`), so no key, click, or
//! stick value is ever stranded across a switch.
//!
//! ## Bare buttons
//!
//! A button pressed without the guide modifier does what its `[buttons]` /
//! `h.button` binding says ([`crate::config::ButtonAction`]), in one of two
//! ways. A key or mouse button is **held** with it — pressed on the down edge,
//! released on the up edge, so the kernel auto-repeats an arrow and a click
//! drags ([`drive_buttons`]). Any other action — `exec`, `keyboard`,
//! `set_mode`, a workspace switch — is **fired** once on the press edge and
//! never repeated while the button stays down ([`fire_buttons`]), through
//! exactly the path a guide chord takes once it has resolved
//! ([`perform_action`]). Both obey one gate: off while the guide button is
//! held and while the on-screen keyboard owns the pads.
//!
//! ## Held outputs on guide chords
//!
//! The mirror image: a guide chord takes any action a bare button takes, and a
//! key or mouse button on one (`h.bind("guide+rpad_click", h.mouse "left")`)
//! is **held** rather than fired — pressed when the chord is recognised,
//! released when the chord button lifts ([`GestureEvent::GuideChordRelease`])
//! or the guide is released, whichever comes first, and routed by code to the
//! keyboard or the pointer exactly as a bare `Hold` binding is. That is what
//! clicks while the guide-mouse above is active, and what a modifier on a grip
//! (`guide+l5` = Shift) will hang off. [`ChordKeys`] tracks what is down; the
//! mode handoff and the disconnect path release it beside [`ButtonKeys`].
//!
//! A hold spent this way — a chord, or the pad moving the cursor — is
//! *consumed* ([`GestureEngine::consume_hold`]): its release is never handed on
//! as the bare guide tap Steam acts on.
//!
//! What is left — a quick press and release that meant nothing else — has
//! **exactly one** consumer, and the forwarding gate picks it.
//!
//! In a game it is the *game's* ([`guide_tap_pulse`]). The guide is stripped
//! from every frame the game sink sees, and while it is held nothing is
//! forwarded at all, so the button Steam needs for its overlay is synthesized
//! on the fake instead: a 60 ms pulse on the release
//! (`SteamRelay::pulse_guide`, [`VirtualGamepad::pulse_guide`]). No binding
//! runs there — in a game the Steam button is Steam's.
//!
//! Everywhere else it is the `guide_tap` **binding's** ([`guide_tap_binding`]):
//! an ordinary gesture binding, resolved through the same guard and the same
//! [`perform_action`] tail as a chord. The narrowness above is what makes that
//! safe — a caret fix, a guide-mouse drag and a long deliberating hold are all
//! unchorded releases, and none of them binds anything.
//!
//! ## Layer transitions: a held button never leaks into the next layer
//!
//! Every layer acts on **press edges only**, and never on the frame it became
//! live. A button that is physically down when ownership of the controller
//! changes hands — B, still held from closing the keyboard, as the desktop's
//! bare bindings reopen; the Y of the `guide+Y` chord that raised the
//! keyboard, as the keyboard starts routing; A, still held after the guide was
//! released first; R1, still held while a mode flips away and straight back —
//! is by construction down on the previous frame from then on, so it is not an
//! edge and does nothing until it is released and pressed again. The frame a
//! transition lands on is suppressed outright: [`ButtonKeys::reconcile`] and
//! [`OskRoute::step`] both remember whether they were live last frame, and
//! [`ButtonKeys::release_all`] (every mode handoff and the disconnect path)
//! resets that memory. That covers a gate that opens and a press that lands
//! in the same report, and a handoff that runs between two reports. The price
//! is that a key held straight through a guide tap does not resume on its
//! own — it wants a fresh press.
//!
//! A config reload is deliberately **not** one of those. [`apply_reload`]
//! re-resolves the mode against the unchanged context and rebuilds the pad
//! dampers, but releases nothing wholesale and runs no handoff. The next
//! frame's [`ButtonKeys::reconcile`] settles the held set against the new map
//! instead, so a binding the reload left alone stays held — a reload mid-drag
//! keeps the drag — and only one that changed or vanished is let go.

use crate::config::{
    Action, ButtonAction, Config, CursorConfig, GamepadConfig, GamepadKind, GuideTap,
    HapticsConfig, KeyChord, OskAction, RumbleMode, ScrollConfig, ScrollMode, ScrubConfig,
};
use crate::filter::{AngleAccumulator, JogPacer, JogUnit, PadDamper, ShuttlePacer};
use crate::gamepad::{self, VirtualGamepad};
use crate::gesture::{GestureEngine, GestureEvent};
// `Pad` is renamed: this module already talks about `report::Pad` (a pad
// *position*), while the haptics one names an *actuator*.
use crate::haptics::{Feel, Haptics, Pad as HapticPad};
use crate::hypr::{Hypr, HyprEvent};
use crate::keyboard::VirtualKeyboard;
use crate::mode::ModeEngine;
use crate::osk::{self, NavRepeat, OskEvent, OskHandle, OskNav, OskPad, OskPresentation};
use crate::output::{PointerButton, VirtualPointer};
use crate::status::{RelayKind, StatusWriter};
use crate::uhid::settings::{self, Action as RelayAction};
use crate::uhid::translate::StripMask;
use crate::uhid::SteamRelay;
use crate::{hidraw, report, status, uhid};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often the reconnect wait re-scans for the controller's return. Bounded so
/// we never busy-spin (the scan sleeps between attempts) yet re-arm within a
/// couple of seconds of the controller reappearing.
const RECONNECT_SCAN_INTERVAL: Duration = Duration::from_millis(1500);

/// One unit of work for the main loop: a controller report, a compositor event,
/// or the signal that the controller's readers have all ended. The source
/// threads funnel into this so the loop can own all mutable state without locks.
enum Input {
    /// One decoded frame from **either** backend.
    ///
    /// The controller's bytes are decoded in its forwarder thread rather than in the
    /// loop, so both sources arrive in the same shape and every layer above
    /// this one keeps its single meaning of "a frame". `raw` is the controller's
    /// original report, kept only because the Steam relay forwards the controller's
    /// own bytes unprocessed ([`drive_gamepad`]); an evdev frame has none, and
    /// the uinput pad — which re-encodes from the frame — does not want one.
    Frame { frame: report::Frame, raw: Option<Vec<u8>> },
    /// The evdev backend adopted a gamepad ([`crate::evdev`]). Carries what
    /// `status.json` needs to name it and which cheat-sheet layout it draws as.
    EvdevAdopted { label: &'static str, layout: &'static str, name: String },
    /// The adopted gamepad went away. Distinct from [`Input::ReadersEnded`],
    /// which is the *Steam Controller* leaving: either device may go without
    /// the other, and only the one that left is released.
    EvdevGone,
    Compositor(HyprEvent),
    /// An event the on-screen keyboard child reported back over its stdout
    /// channel (see [`crate::osk`]) — currently a key crossing, which the daemon
    /// answers with a haptic tick because it, not the child, owns the device.
    Osk(OskEvent),
    /// Every Steam Controller reader thread has exited — the controller went
    /// away. The loop enters its reconnect wait instead of terminating.
    ///
    /// **Every** reader, not any: with the dongle plugged in and a Bluetooth
    /// bond live, both node sets are open at once and only one is streaming, so
    /// the silent set's readers are still blocked in `read()` and hold the
    /// channel open. A transport switch therefore never reaches this arm — the
    /// frames simply start arriving on a different descriptor
    /// ([`crate::hidraw::read_all`]).
    ReadersEnded,
    /// The controller started streaming on a different link — the first frame of
    /// a generation, or a genuine switch between the dongle and Bluetooth.
    /// Published as `status.json`'s `"transport"` and logged; nothing else in
    /// the daemon branches on it, because one controller behaves the same way
    /// down either wire.
    Transport(hidraw::Transport),
    /// SIGHUP (or `hyprpad reload`) asked us to re-read the config and apply it
    /// live. Delivered by the SIGHUP self-pipe's waiter thread (see
    /// [`install_reload_signal`]) so the async-signal-safe handler only ever
    /// `write`s, and the actual reload runs in the loop's ordinary context.
    Reload,
    /// SIGUSR1 (or `hyprpad off`) asked us to turn the *controller* off. Rides
    /// the same self-pipe as [`Input::Reload`], for the same reason: the
    /// `0x9F` write opens descriptors and logs, neither of which is
    /// async-signal-safe. The same one-shot [`Action::ControllerOff`] performs
    /// from a chord.
    ControllerOff,
}

pub fn run() -> std::io::Result<()> {
    // Advertise our PID before anything that can block, so `hyprpad reload` /
    // `status` can find a daemon that is still waiting for its controller. The
    // guard removes the pidfile on the clean-return and panic paths.
    let _pidfile = PidFile::create();

    // Live status for a bar widget, published from here on: the same RAII
    // contract as the pidfile, and started before the controller wait so a
    // widget can show "daemon up, controller not here yet" rather than nothing.
    let mut status = StatusWriter::create();

    // Load the config BEFORE the controller wait, because the wait now has to
    // ask it a question: whether the gamepad backend is armed, and therefore
    // whether a pad on `/dev/input` counts as "a controller is here".
    //
    // Mutable because SIGHUP / `hyprpad reload` swaps in a freshly loaded config
    // live (see the `Input::Reload` arm and `apply_reload`).
    let mut config = match Config::load() {
        Ok(c) => {
            match Config::active_config_path() {
                Some((p, fmt)) => {
                    eprintln!("hyprpad: config loaded from {} ({fmt} front-end)", p.display())
                }
                None => eprintln!("hyprpad: using built-in default config"),
            }
            c
        }
        Err(e) => {
            eprintln!("warning: {e}; using built-in default config");
            Config::load_default()
        }
    };

    // Wait for *a* controller rather than exiting when there isn't one yet. The
    // loop below already survives a controller *leaving*; this makes startup
    // symmetric, so a daemon launched at login (or restarted while the controller is
    // unplugged / asleep on a dead dongle) simply sits until one appears —
    // "leave it running" has to include "start it before the controller".
    //
    // The controller half polls exactly what the reconnect wait polls —
    // `ControllerSource::acquire` — so both paths ask the broker first and fall back
    // to opening the nodes directly, and neither can be satisfied by a controller
    // that is listed in `/sys` but not actually openable.
    //
    // The gamepad half is why this returns an `Option`: a machine with only an
    // Xbox pad plugged in is a perfectly good hyprpad session, and waiting
    // forever for a Steam Controller that is not coming would be the wrong
    // answer. `None` means "start with no controller", which is exactly the state the
    // reconnect wait already handles — so the loop below begins in it and picks
    // the controller up on its next scan if it ever arrives.
    let source = {
        let mut source = hidraw::ControllerSource::acquire();
        if source.is_none() {
            // Distinguish the ways this can fail, because they need different
            // things from the user. A controller that is *listed* in `/sys` but not
            // obtainable is a half-finished host install — the udev rule took
            // the nodes away and no broker is handing them back — and "waiting
            // for a controller" would be a misleading thing to say about a
            // controller that is plugged in.
            let listed = hidraw::controller_nodes().map(|n| n.len()).unwrap_or(0);
            if listed > 0 {
                eprintln!(
                    "hyprpad: the controller is present ({listed} node(s)) but none can be \
                     obtained — the udev rule is installed and the fd broker is not \
                     reachable. Run `hyprpad setup --check`. Waiting…"
                );
            } else {
                eprintln!(
                    "hyprpad: no Steam Controller found — neither the puck (28de:1304) nor a \
                     Bluetooth link (28de:1303); waiting for one…"
                );
            }
            while source.is_none() && !gamepad_present(&config) {
                std::thread::sleep(RECONNECT_SCAN_INTERVAL);
                source = hidraw::ControllerSource::acquire();
            }
        }
        match &source {
            Some(s) => {
                eprintln!("hyprpad: controller found ({} node(s), {})", s.len(), s.label())
            }
            None => eprintln!(
                "hyprpad: starting on a gamepad instead; the controller will be picked up if it \
                 turns up"
            ),
        }
        source
    };
    let node_count = source.as_ref().map_or(0, hidraw::ControllerSource::len);
    if let Some(s) = &source {
        status.set_source(status::Source::of(s));
        status.set_connected(true);
    }

    // Lizard-mode ownership (opt-in). Off by default so we never fight an
    // unmasked Steam that is managing lizard mode itself
    // (docs/experiments/w12-device-denial.md). When enabled, a background thread
    // disables the controller's firmware keyboard/mouse emulation and re-sends
    // periodically to re-cover it across reconnects. It degrades gracefully:
    // failures log a warning and never take the daemon down.
    // Mutable so a live reload can react to `own_lizard` flipping in the config
    // (see `apply_own_lizard_change`). The one-time startup wiring below —
    // restore-on-exit and the periodic ownership loop — is keyed off the value
    // as it stands *now*; reload only does a best-effort one-shot, and a full
    // ownership change wants a restart.
    let mut own_lizard = lizard_ownership_enabled(&config);

    // The firmware power knobs ride in the lizard-disable frame, so hand them to
    // the module that builds it rather than threading them through the four
    // places that ask for a disable. Re-installed on every reload, below.
    crate::lizard::set_power_settings(config.power_settings());
    // Same cell, same frame, same reason: hyprpad's own gyro baseline, which the
    // relay then overrides for as long as Steam is asking for the IMU.
    crate::lizard::set_imu_preference(config.gamepad().imu_preference());
    // And what the exit paths should do about lizard mode. Same shape as the two
    // above and for the same reason: the exit runs from a signal-waiter thread
    // and a `Drop` guard, neither of which can be handed a `Config`. Re-installed
    // on every reload, below.
    crate::lizard::set_restore_on_exit(config.restore_lizard_on_exit());

    // When we own lizard we normally give it back on the way out, or the firmware
    // keyboard/mouse stays dead and the user has no pointer until a power-cycle.
    // `restore_lizard_on_exit = false` deliberately opts out of that — the
    // lizard-free boot of docs/12-lizard-free.md — but the wiring is the same
    // either way, because the exit routine is what reads the knob. Install the
    // signal-driven exit early, so an immediate Ctrl-C is already covered, and
    // pair it with a `_restore` guard that handles the normal-return and panic
    // paths. The two never both fire: the signal path exits the process
    // (skipping unwinding), the guard fires only when the function actually returns.
    let _restore = if own_lizard {
        crate::lizard::install_signal_restore();
        Some(crate::lizard::LizardRestoreGuard)
    } else {
        None
    };

    // Advertise our PID so `hyprpad reload` can find us and send SIGHUP. The
    // guard removes the pidfile on the clean-return and panic paths (a
    // signal-driven exit via `std::process::exit` leaves it, but `hyprpad
    // reload` treats a stale pidfile — a pid that is gone — as "not running").

    let hypr = Hypr::connect()?;
    eprintln!("hyprpad: {node_count} controller node(s), Hyprland IPC connected");

    if own_lizard {
        eprintln!(
            "hyprpad: lizard-mode ownership on; taking over the controller's firmware kbd/mouse"
        );
        std::thread::spawn(crate::lizard::own_lizard_loop);
    }

    // Merge both event sources into one channel so the loop stays single-owner.
    // We keep `tx` alive for the whole run (never `drop` it) so `rx` never
    // disconnects while the controller is briefly gone: the loop then blocks in
    // its reconnect wait instead of ending. `tx` is also how we re-arm a fresh
    // reader pipeline when the controller returns.
    let (tx, rx) = mpsc::channel::<Input>();

    // SIGHUP -> live config reload, routed through its own self-pipe into the
    // loop as `Input::Reload`. Installed regardless of lizard ownership so
    // `hyprpad reload` works either way.
    install_reload_signal(&tx);

    // `None` means we started on a gamepad with no controller present. That is the
    // reconnect wait's own state, so begin in it: the loop scans for the controller
    // on its timer and arms a reader pipeline the moment it appears.
    let mut waiting = match source {
        Some(source) => {
            spawn_reader_pipeline(source, &tx);
            false
        }
        None => true,
    };

    // The second backend: any ordinary Linux gamepad on `/dev/input/event*`
    // ([`crate::evdev`]). One thread owns discovery, adoption, the grab, the
    // blocking read and the hotplug rescan, and reports into this same channel
    // — which is why the loop below grows three match arms and not a second
    // supervisor. Armed unconditionally when the config allows it: with no pad
    // plugged in it costs one sleeping thread and nothing else.
    //
    // `grab` is read here, once. A reload that changes it takes effect on the
    // next adoption, not immediately — the alternative is tearing the read out
    // from under a live device, which is a lot of machinery for a knob nobody
    // flips twice.
    if config.device().evdev {
        let events = crate::evdev::watch(config.device().grab);
        let tx_e = tx.clone();
        std::thread::spawn(move || forward_evdev(events, tx_e));
        eprintln!("hyprpad: gamepad backend armed (evdev; scanning for a pad)");
    }

    match crate::hypr::subscribe() {
        Ok(events) => {
            let tx_e = tx.clone();
            std::thread::spawn(move || {
                for e in events {
                    if tx_e.send(Input::Compositor(e)).is_err() {
                        return;
                    }
                }
            });
        }
        // No event feed => no arbitration; desktop gestures always act. The
        // daemon is still useful, just not game-aware.
        Err(e) => eprintln!("warning: no compositor event feed ({e}); arbitration disabled"),
    }

    // The cursor path degrades gracefully: without it, gestures and workspace
    // switching still work.
    let mut pointer = match VirtualPointer::new() {
        Ok(p) => {
            eprintln!("hyprpad: virtual pointer ready (right trackpad drives the cursor)");
            Some(p)
        }
        Err(e) => {
            eprintln!("warning: no virtual pointer ({e}); trackpad cursor disabled");
            None
        }
    };
    let mut cursor = CursorState::new(config.cursor());
    // Left-pad scroll shares the cursor's One Euro/hysteresis smoothing knobs.
    let mut scroll = ScrollState::new(config.scroll(), config.cursor());
    // The same pad under a held guide: the caret jog wheel. Off in every
    // config that has not written an `h.scrub` block, and it re-reads its own
    // knobs per frame, so a reload can switch it on with no restart.
    let mut scrub = ScrubState::new(config.scrub(), config.cursor());

    // Bare-button keyboard: buttons pressed without the guide modifier emit raw
    // keys (the D-pad acts as arrow keys by default). Degrades gracefully —
    // without it, bare-button bindings are simply inert.
    let mut keyboard = match VirtualKeyboard::new() {
        Ok(k) => {
            eprintln!("hyprpad: virtual keyboard ready (bare D-pad drives the arrow keys)");
            Some(k)
        }
        Err(e) => {
            eprintln!("warning: no virtual keyboard ({e}); bare-button bindings disabled");
            None
        }
    };
    let mut button_keys = ButtonKeys::new();
    // The outputs guide chords hold (`guide+rpad_click` = a mouse button), the
    // chord-side twin of `button_keys`.
    let mut chord_keys = ChordKeys::new();

    // Haptic feedback from the actuator behind each trackpad. Built
    // unconditionally: it opens nothing until the first pulse, so a
    // `[haptics] enabled = false` config never touches the device, and a live
    // reload that switches it on works with no restart. Failures are
    // logged-once no-ops inside the module, never fatal here.
    let mut haptics = Haptics::new();

    // The virtual gamepad games see (docs/06 Tier 1). Built empty: the uinput
    // device is created on the first frame a game-classed window actually holds
    // focus, so a session that never plays a game never creates one — and a
    // machine with no `/dev/uinput` warns once and carries on with the whole
    // desktop layer intact.
    let mut gamepad = GamepadState::new();
    gamepad.relay = start_relay(config.gamepad());
    if config.gamepad().enabled {
        eprintln!("hyprpad: virtual gamepad armed (created on first game focus)");
    } else {
        eprintln!("hyprpad: virtual gamepad disabled by config ([gamepad] enabled = false)");
    }

    let debug = std::env::var_os("HYPRSC_DEBUG").is_some();
    let mut engine = GestureEngine::new();
    // Modality (docs/13). With no modes declared this is exactly the old
    // `Arbiter` game gate; a `config.lua` that declares modes drives it with
    // Lua predicates instead. Either way it is re-resolved only on a context
    // change and read as cached state per frame.
    let mut modes = ModeEngine::new(&config);
    status.set_modes(status::mode_names(&config));
    status.set_mode(modes.active());
    status.set_relay(relay_kind(config.gamepad(), gamepad.relay.is_some()));
    if !config.modes().is_empty() {
        eprintln!(
            "hyprpad: {} mode(s) declared, starting in '{}'",
            config.modes().len(),
            modes.active()
        );
    }

    // The on-screen keyboard bridge. Spawns lazily on the first toggle and is
    // killed when this function returns (daemon exit). When it is showing, the
    // two pads drive the keyboard instead of the desktop cursor.
    //
    // Its stdout back-channel (`event crossed <L|R>`) arrives on its own
    // channel; a forwarder thread funnels it into this loop's single input
    // stream as `Input::Osk`, where a crossing becomes a haptic tick under that
    // thumb. Split this way because the child owns the hit-test and the daemon
    // owns the writable controller node.
    let (osk_tx, osk_rx) = mpsc::channel::<OskEvent>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || forward_osk_events(osk_rx, tx));
    }
    let mut osk = OskHandle::with_events(osk_tx);
    let mut osk_route = OskRoute::new(config.cursor());
    // Previous frame, kept for edge detection in the OSK-active router (pad
    // click-down and the B/Menu dismiss).
    let mut prev_frame = report::Frame::default();

    // --- two controllers, one pointer ------------------------------------
    //
    // Both backends may be live at once, and there is exactly one cursor, one
    // keyboard, one OSK and one mode engine, so they cannot both drive. The
    // rule is **last-active source wins**: a frame from the source that is not
    // active is dropped unless it carries deliberate input
    // ([`report::Frame::is_neutral`]), and one that does runs the same clean
    // handoff a mode change does before taking over. A hand resting on the
    // Steam Controller therefore keeps it, and picking up the Xbox pad and
    // moving a stick switches on the spot.
    let mut active_source = report::Source::default();
    // The gamepad the evdev backend currently holds, if any: its `status.json`
    // name and its cheat-sheet layout id.
    let mut evdev_pad: Option<(&'static str, &'static str)> = None;
    // The rate integrators the sticks drive on a controller with no trackpads,
    // and the owner of this loop's fourth receive deadline ([`crate::sticks`]).
    let mut sticks = crate::sticks::StickDrive::new();
    status.set_layout(report::LAYOUT_STEAM_CONTROLLER);
    status.set_sources(source_names(!waiting, evdev_pad));

    // Supervisor loop. Two states: *connected*, where it blocks on `rx`; and
    // *waiting*, entered when the readers all end (controller gone), where it
    // re-scans for the controller on a timer while still draining compositor events —
    // and, crucially, never returns. Hyprland IPC and the virtual pointer stay
    // alive across the gap; only the hidraw reader pipeline is torn down and
    // re-armed. `recv_timeout` with a shrinking deadline guarantees the scan
    // still fires even if compositor events keep arriving. `waiting` was
    // decided above, when the reader pipeline was (or was not) armed.
    let mut next_scan = Instant::now();
    // What the loop knows about the focused window: which address it is (so a
    // `windowtitle` event can be attributed), which title form this compositor
    // speaks, and when its process tree may next be re-walked.
    let mut watch = FocusWatch::new();

    // Seed focus from the compositor at startup. The event socket only tells us
    // about *changes*, so without this a daemon (re)started while a game or
    // Big Picture is already focused would sit in the default mode — and keep
    // driving the desktop cursor into the game — until the next alt-tab. Same
    // for title-rescan attribution, which keys on the focused address. Only
    // when modes are declared (a plain TOML config never needs focus).
    if config.needs_focus_pid() {
        match hypr.active_window() {
            Ok(info) if !info.class.is_empty() => {
                watch.address = info.address.clone();
                // Same reason, for the keyboard: it must know which window its
                // prediction context belongs to before the first announcement
                // arrives, or that announcement looks like a move.
                osk.seed_focus(&info.class, &info.address, config.osk_learn_deny());
                let focus = crate::mode::Focus {
                    class: info.class.clone(),
                    title: info.title,
                    pid: info.pid,
                    fullscreen: info.fullscreen,
                };
                let _ = modes.context_changed(&config, focus);
                eprintln!(
                    "hyprpad: focus seeded from activewindow ({}), mode '{}'",
                    info.class,
                    modes.active()
                );
            }
            Ok(_) => {} // focus on the desktop: the default mode is already right
            Err(e) => eprintln!("warning: could not seed focus from activewindow ({e})"),
        }
    }

    // Seed the overlays already on screen, for the same reason and with the
    // same gating: `openlayer`/`closelayer` announce only *changes*, so a
    // daemon restarted while the cheat sheet is up would think nothing is
    // showing and leave B on its desktop meaning until the sheet was closed and
    // reopened. Nothing is held yet at this point, so — as with the focus seed
    // — there is no handoff to run, only a line saying where we landed.
    if !config.modes().is_empty() {
        match hypr.open_layers() {
            Ok(open) => {
                let showing = open.iter().map(String::as_str).collect::<Vec<_>>().join(", ");
                if modes.seed_layers(&config, open) {
                    eprintln!(
                        "hyprpad: overlays seeded ({showing}), mode '{}'",
                        modes.active()
                    );
                }
            }
            Err(e) => eprintln!("warning: could not seed open overlays from j/layers ({e})"),
        }
    }

    // Seed the session lock, and start watching it. Third context source, same
    // reason as the first two — a daemon started while the screen is locked
    // must not spend the lock in `desktop`, typing D-pad arrows into a password
    // field — but with one difference: there is no event to fall back on. The
    // compositor emits nothing on socket2 for `ext-session-lock-v1`, so the
    // seed and the watch are the *same* `j/locked` query, once here and once a
    // second in a thread (`hypr::watch_locked` weighs that against the
    // alternatives). Gated on a config that actually asks: `watches_lock` is
    // false for every TOML config and for any `config.lua` that never mentions
    // the lock, and then not a single query is made.
    //
    // Keyed off the config as it stands *now*, like the lizard-ownership wiring
    // above: a `hyprpad reload` that introduces the first `ctx.locked`
    // predicate re-resolves against it (the engine's context is still whatever
    // was last seeded) but does not start the watcher — adding the lock to a
    // config that never had it wants a restart. Taking it away is free, because
    // no rule reads it any more.
    if config.watches_lock() {
        let locked = match hypr.locked() {
            Ok(locked) => {
                if modes.lock_changed(&config, locked) {
                    eprintln!("hyprpad: session is locked, mode '{}'", modes.active());
                }
                locked
            }
            Err(e) => {
                eprintln!("warning: could not seed the session lock state ({e})");
                false
            }
        };
        match crate::hypr::watch_locked(locked, crate::hypr::LOCK_POLL_INTERVAL) {
            Ok(events) => {
                let tx_l = tx.clone();
                std::thread::spawn(move || {
                    for e in events {
                        if tx_l.send(Input::Compositor(e)).is_err() {
                            return;
                        }
                    }
                });
            }
            Err(e) => eprintln!("warning: not watching the session lock ({e})"),
        }
    }
    // The seeds above may have moved the mode (a daemon started under the lock,
    // or over a game); the status file was written before them, so say so.
    status.set_mode(modes.active());

    loop {
        // The periodic process-tree rescan (`process_rescan_ms`), the title-less
        // half of noticing a program that starts under the focused window.
        // Checked *before* the receive, so a busy report stream (a hand resting
        // on a pad) can never starve it, and used as the receive deadline below,
        // so an idle loop still wakes for it. `arm_rescan` is what rate-limits
        // it: one walk per interval however many events arrive in between.
        let rescan_at = process_rescan_due(&config, &modes, &watch);
        if rescan_at.is_some_and(|at| at <= Instant::now()) {
            watch.arm_rescan(&config);
            if modes.rescan_processes(&config) {
                eprintln!("hyprpad: mode -> {} (process rescan)", modes.active());
                status.set_mode(modes.active());
                mode_handoff(
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    pointer.as_mut(),
                    &mut button_keys,
                    &mut chord_keys,
                    &mut keyboard,
                    &mut gamepad,
                    &mut haptics,
                );
            }
            continue;
        }

        // A transient mode's timer (`h.mode("hints"):transient { timeout_ms =
        // … }`). Checked here beside the rescan and folded into the receive
        // deadline below for the same reason: nothing else is going to wake
        // this loop when the user simply stops pressing, and "the hints are
        // still up eight seconds later" is exactly the case the timer exists
        // for.
        let transient_at = modes.transient_deadline();
        if transient_at.is_some_and(|at| at <= Instant::now()) {
            if modes.check_deadline(&config, Instant::now()) {
                eprintln!("hyprpad: mode -> {} (transient timeout)", modes.active());
                status.set_mode(modes.active());
                mode_handoff(
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    pointer.as_mut(),
                    &mut button_keys,
                    &mut chord_keys,
                    &mut keyboard,
                    &mut gamepad,
                    &mut haptics,
                );
            }
            continue;
        }

        // The stick integrators' step, for a controller whose sticks stand in
        // for the controller's trackpads. Checked HERE, before the receive, for the
        // same reason the two above are: a change-driven pad moving a stick
        // produces a busy report stream, and a deadline that were only honoured
        // on a receive *timeout* would then never fire at all.
        //
        // This is a deadline and not a tick. `StickDrive::deadline` is `Some`
        // only while a stick is outside its deadzone or a velocity is still
        // decaying through the smoothing; centred, it is `None`, this branch is
        // dead, nothing is added to `wake`, and the loop blocks indefinitely
        // exactly as it does today with the controller asleep.
        let stick_at = sticks.deadline();
        if stick_at.is_some_and(|at| at <= Instant::now()) {
            let now = Instant::now();
            // The same guards the pad paths consult, read before the call so
            // the keyboard's own handle is not borrowed twice.
            let gates = StickGates {
                cursor: cursor_active(
                    engine.guide_active(),
                    modes.cursor_enabled(),
                    modes.cursor_guide_enabled(),
                ),
                // `mode = off` is "the left input does not scroll", and it
                // means that on a stick as much as on a pad — the pad path
                // checks it inside `drive_scroll`, so the stick path checks it
                // here rather than letting the two disagree.
                scroll: !engine.guide_active()
                    && modes.scroll_enabled()
                    && config.scroll().mode != ScrollMode::Off,
                osk: osk.is_active(),
            };
            let mut hx =
                HapticCtx { dev: &mut haptics, cfg: config.haptics(), source: active_source };
            drive_sticks(
                &mut sticks,
                pointer.as_mut(),
                &mut osk,
                config.sticks(),
                config.scroll(),
                gates,
                &mut hx,
                now,
            );
            // The caret shuttle rides this clock too, and it is the reason the
            // clock exists for it: a stick held over emits no reports, so the
            // taps have to come from here. The last frame is the state — its
            // deflection and its `select` level — exactly as it is for the
            // integrators above.
            let scrub_active = engine.guide_active()
                && !osk.is_active()
                && config.scrub_enabled_in(modes.state());
            drive_scrub(
                &mut keyboard,
                &mut engine,
                &prev_frame,
                &mut scrub,
                config.scrub(),
                config.cursor(),
                scrub_active,
                &mut hx,
                now,
            );
            sticks.set_shuttle(scrub.shuttle_running(), config.sticks(), now);
            sticks.stepped(config.sticks(), now);
            continue;
        }

        // The earliest deadline this iteration must wake for: the reconnect
        // re-scan while the controller is away, the process rescan whenever it
        // is armed, a transient mode's timer while one is running, and the
        // stick integrators' step while a stick is deflected. With none of
        // them, this is the plain blocking receive it has always been.
        let wake = [waiting.then_some(next_scan), rescan_at, transient_at, stick_at]
            .into_iter()
            .flatten()
            .min();
        let input = match wake {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if waiting && Instant::now() >= next_scan {
                        if let Some(source) = hidraw::ControllerSource::acquire() {
                            eprintln!(
                                "hyprpad: controller reconnected ({} node(s), {})",
                                source.len(),
                                source.label()
                            );
                            // The answer can change under a running daemon —
                            // start the broker and this is the reconnect that
                            // notices — so republish it every generation.
                            status.set_source(status::Source::of(&source));
                            if own_lizard {
                                // Re-cover the fresh device now rather than waiting for
                                // the ownership loop's periodic re-send.
                                if let Err(e) = crate::lizard::disable_lizard_mode() {
                                    eprintln!("warning: lizard re-disable on reconnect: {e}");
                                }
                            }
                            spawn_reader_pipeline(source, &tx);
                            status.set_connected(true);
                            waiting = false;
                            status.set_sources(source_names(true, evdev_pad));
                        }
                        next_scan = Instant::now() + RECONNECT_SCAN_INTERVAL;
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(input) => input,
                Err(_) => break,
            },
        };

        match input {
            Input::Transport(t) => {
                // Publish only. A transport switch is not a connect, not a
                // disconnect and not a config change: the same controller is
                // still there and every layer above the reader keeps its state.
                status.set_transport(t);
            }
            Input::ReadersEnded => {
                eprintln!("hyprpad: controller disconnected, waiting for it to return…");
                status.set_connected(false);
                // Release everything held for the vanished controller — every
                // bare-button key and click, and all smoothing/gesture state —
                // so nothing is stuck down and a stale `prev` cannot jump the
                // cursor on return. No report will arrive to `reconcile` the
                // held set against, so it is released here, explicitly.
                reset_frame_state(
                    &mut engine,
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    &mut prev_frame,
                );
                button_keys.release_all(&mut keyboard, pointer.as_mut());
                // The fresh gesture engine above has forgotten the guide was
                // held, so no `GuideLeave` will ever release these: do it here.
                chord_keys.release_all(&mut keyboard, pointer.as_mut());
                // Same contract for the game: a vanished controller must not
                // leave the virtual pad holding its last frame, or a rumble
                // running with nothing left to stop it.
                gamepad.release(&mut haptics);
                // The controller's rate integrators are always idle (its pads drive
                // the cursor), but if the Xbox pad was driving and the controller is
                // what left, this is still the right place to drop any claim
                // held for a source that is now gone.
                if active_source == report::Source::SteamController {
                    sticks.release();
                }
                waiting = true;
                status.set_sources(source_names(false, evdev_pad));
                next_scan = Instant::now() + RECONNECT_SCAN_INTERVAL;
            }
            Input::ControllerOff => {
                // The same thing the `guide+quickaccess` chord does, reached
                // from `hyprpad off` (or the bar widget's right-click) instead.
                // Deliberately not a shutdown of anything else: the daemon
                // stays up and sits in its reconnect wait, exactly as it does
                // when the controller sleeps on its own.
                eprintln!("hyprpad: turning the controller off (SIGUSR1)");
                controller_off();
            }
            Input::Reload => {
                apply_reload(
                    &mut config,
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    &mut own_lizard,
                    &mut modes,
                );
                // The new file can rename modes as well as re-resolve the
                // active one, so republish both. Each is a no-op when the
                // reload changed nothing about it.
                status.set_modes(status::mode_names(&config));
                status.set_mode(modes.active());
            }
            Input::Compositor(ev) => {
                if debug {
                    eprintln!("[event] {ev:?}");
                }
                // Name the cause: a transition with no focus change behind it
                // is otherwise a mystery in the log.
                let why = match &ev {
                    HyprEvent::WindowTitle { .. } => " (title change)",
                    HyprEvent::Layer { .. } => " (overlay)",
                    HyprEvent::Locked(true) => " (session locked)",
                    HyprEvent::Locked(false) => " (session unlocked)",
                    _ => "",
                };
                // The keyboard's word prediction keys on the focused window:
                // its typed context belonged to the field we just left, and the
                // class decides whether it may learn what is typed here at all
                // (docs/research/osk-prediction.md §5.3 rule 2 / §8.1). Both
                // halves of the compositor's announcement go in — the class,
                // then the address that identifies the window — because an
                // `activewindow` line is not a focus *change*: Hyprland re-emits
                // it for the window that already has focus, and resetting on
                // each one wiped the typed context about once a second behind a
                // terminal with a spinner in its title.
                match &ev {
                    HyprEvent::ActiveWindow { class, .. } => {
                        osk.focus_changed(class, config.osk_learn_deny());
                    }
                    HyprEvent::ActiveWindowV2 { address } => osk.focus_window_changed(address),
                    _ => {}
                }
                if update_modes(&mut modes, &config, &hypr, &mut watch, ev) {
                    eprintln!("hyprpad: mode -> {}{why}", modes.active());
                    status.set_mode(modes.active());
                    mode_handoff(
                        &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                        pointer.as_mut(),
                        &mut button_keys,
                        &mut chord_keys,
                        &mut keyboard,
                        &mut gamepad,
                        &mut haptics,
                    );
                }
            }
            Input::Osk(OskEvent::Crossed(pad)) => {
                // The child hit-tested a crossing onto a NEW key; we hold the
                // writable controller node, so the tick is fired here. Gating and
                // intensity come off the live config, so a reload retunes it.
                let mut hx =
                    HapticCtx { dev: &mut haptics, cfg: config.haptics(), source: active_source };
                hx.fire(Haptic::Crossing, haptic_pad(pad));
            }
            Input::EvdevAdopted { label, layout, name } => {
                evdev_pad = Some((label, layout));
                status.set_sources(source_names(!waiting, evdev_pad));
                eprintln!("hyprpad: second source available — {name} ({label})");
            }
            Input::EvdevGone => {
                evdev_pad = None;
                status.set_sources(source_names(!waiting, evdev_pad));
                // Only the pad that left is released. If it was the one
                // driving, everything it was holding goes with it — exactly
                // what `Input::ReadersEnded` does for the controller — and the
                // active source falls back to the controller, whose own presence is
                // `waiting`'s business.
                if active_source == report::Source::Evdev {
                    active_source = report::Source::SteamController;
                    status.set_layout(report::LAYOUT_STEAM_CONTROLLER);
                    sticks.release();
                    reset_frame_state(
                        &mut engine,
                        &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                        &mut prev_frame,
                    );
                    button_keys.release_all(&mut keyboard, pointer.as_mut());
                    chord_keys.release_all(&mut keyboard, pointer.as_mut());
                    gamepad.release(&mut haptics);
                }
            }
            Input::Frame { frame, raw } => {
                // Last-active-source wins. An idle frame from the source that
                // is not driving is dropped outright — a change-driven pad
                // re-sends, and a resting hand on either controller must not
                // take the cursor from the other. A frame that says the user
                // actually did something switches, and switching runs the same
                // clean handoff a mode change does, so nothing is left held by
                // the source that just lost the device.
                if frame.source != active_source {
                    if frame.is_neutral() {
                        continue;
                    }
                    eprintln!("hyprpad: input source -> {:?}", frame.source);
                    active_source = frame.source;
                    status.set_layout(match (frame.source, evdev_pad) {
                        (report::Source::Evdev, Some((_, layout))) => layout,
                        _ => report::LAYOUT_STEAM_CONTROLLER,
                    });
                    sticks.release();
                    // The outgoing source's edge state is meaningless to the
                    // incoming one, so `prev_frame` and the gesture engine are
                    // reset the way a disconnect resets them. Everything the
                    // old source was holding is released here: no report from
                    // it will arrive to `reconcile` it away.
                    reset_frame_state(
                        &mut engine,
                        &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                        &mut prev_frame,
                    );
                    button_keys.release_all(&mut keyboard, pointer.as_mut());
                    chord_keys.release_all(&mut keyboard, pointer.as_mut());
                    gamepad.release(&mut haptics);
                }
                let now = Instant::now();
                // Record the sticks for the rate integrators and arm (or drop)
                // their deadline. A controller frame parks them: its pads drive the
                // cursor and its sticks are for guide flicks, so nothing here
                // must ever hold the deadline open for it.
                sticks.observe(&frame, config.sticks(), now);
                // The live haptics context for this frame: the device handle plus
                // the current `[haptics]` knobs, read fresh so `hyprpad reload`
                // takes effect on the very next pulse. The source rides along so
                // a device with no actuators drops every pulse in one place
                // rather than at each of the call sites.
                let mut hx =
                    HapticCtx { dev: &mut haptics, cfg: config.haptics(), source: frame.source };
                let mut mode_changed = false;
                // The one gesture threshold a config can move, read fresh so
                // `hyprpad reload` retunes it on the very next report.
                engine.set_guide_tap_max(config.gamepad().guide_tap_max());
                // Who owns the LEFT stick's horizontal axis under a held guide.
                // On a padless controller with the scrub live it is the caret
                // shuttle's, and `guide+lstick_left/right` must not also fire —
                // one push would otherwise walk the caret AND move the window
                // focus. Asked before the update, because the flick is decided
                // inside it; vertical flicks are untouched.
                engine.set_shuttle_left(
                    !frame.source.has_pads()
                        && config.scrub().enabled
                        && !osk.is_active()
                        && config.scrub_enabled_in(modes.state()),
                );
                let gestures = engine.update(&frame, now);
                // The forwarding gate, asked HERE — after the update, so the
                // guide is already up on a release frame, and before any
                // binding can move the mode, so the answer is about the mode
                // the gesture happened in. `handle_gesture` needs it for one
                // question only: a bare guide tap in a game is Steam's (the
                // pulse `drive_gamepad` fires below with the same inputs), and
                // everywhere else it is the `guide_tap` binding's.
                let tap_forwarding = gamepad_forwarding(
                    config.gamepad().enabled,
                    modes.forwards(),
                    modes.desktop_yielded(),
                    engine.guide_active(),
                    osk.is_active(),
                );
                for ge in gestures {
                    if debug {
                        eprintln!("[gesture] {ge:?} -> {:?}", config.resolve_in(&ge, modes.state()));
                    }
                    mode_changed |= handle_gesture(
                        &hypr,
                        &config,
                        &mut modes,
                        &mut osk,
                        active_source,
                        &mut hx,
                        &mut chord_keys,
                        &mut keyboard,
                        pointer.as_mut(),
                        tap_forwarding,
                        ge,
                    );
                }
                // Bare-button actions that *fire* (`h.button("l5", h.exec …)`):
                // the same actions a chord performs, on a button's press edge.
                // The bare-button gate: off on the guide layer (so guide+l5
                // stays a chord) and while the OSK owns the pads. Read again
                // below for the held bindings, since an action fired here may
                // have raised the keyboard.
                let buttons_active = !engine.guide_active() && !osk.is_active();
                // A transient mode counts the presses that resolve in it, and
                // one of them may BE its exit. Both questions are asked here,
                // before the button layer, against the map the press resolved
                // against: an exit press clears the mode now and is masked out
                // of the frame the layer sees, while the press cap only marks
                // the budget spent — the press that spends it still has to
                // reach the window, so the clearing waits for `settle_presses`
                // at the bottom of the frame.
                let mut eaten: Vec<report::Button> = Vec::new();
                for b in press_edges(&frame, &prev_frame, buttons_active) {
                    let out = modes.note_press(&config, b);
                    mode_changed |= out.changed;
                    if out.consumed {
                        eaten.push(b);
                    }
                }
                let button_frame = frame.without(&eaten);
                mode_changed |= fire_buttons(
                    &hypr,
                    &config,
                    &mut modes,
                    &mut osk,
                    &mut keyboard,
                    pointer.as_mut(),
                    &button_frame,
                    &prev_frame,
                    buttons_active,
                    &mut hx,
                );
                if mode_changed {
                    // A binding forced a mode (`h.set_mode` / `h.clear_mode`),
                    // from a chord or a bare button. Same handoff as a
                    // context-driven transition.
                    eprintln!("hyprpad: mode -> {} (manual)", modes.active());
                    status.set_mode(modes.active());
                    mode_handoff(
                        &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                        pointer.as_mut(),
                        &mut button_keys,
                        &mut chord_keys,
                        &mut keyboard,
                        &mut gamepad,
                        hx.dev,
                    );
                }
                // The keyboard is not a mode the engine knows, but to a bar
                // widget it is the mode the pad is in. A no-op on every frame
                // it has not flipped.
                status.set_osk(osk.is_active());
                if osk.is_active() {
                    // The keyboard owns both pads: route them to it, and keep the
                    // desktop cursor and scroll released (guide/suppressed = true
                    // forces the drop-and-forget branch of `drive_cursor` /
                    // `drive_scroll`, which also means no scroll detent ticks).
                    route_osk(
                        &mut osk,
                        &frame,
                        &prev_frame,
                        &mut osk_route,
                        modes.osk_buttons(),
                        &mut hx,
                        now,
                    );
                    if let Some(ptr) = pointer.as_mut() {
                        drive_cursor(ptr, &frame, &mut cursor, false, &mut hx, now);
                        drive_scroll(ptr, &frame, &mut scroll, true, true, &mut hx, now);
                    }
                } else {
                    // The keyboard is down — or went down this frame by a
                    // toggle rather than its own dismiss binding. Forget its
                    // routing state, so the next show starts as a fresh layer
                    // and a still-held Shift button is not remembered as
                    // holding it. A no-op when it was already down.
                    osk_route.disarm();
                    // The stick-driven halves of the same routing state. A
                    // fresh keyboard starts with both cursors centred rather
                    // than wherever the last one was left.
                    sticks.osk_left.reset();
                    sticks.osk_right.reset();
                    if let Some(ptr) = pointer.as_mut() {
                        // Ambient (non-guide) layer: the RIGHT pad drives the
                        // cursor and the LEFT pad drives scroll. The guide layer
                        // takes both pads away — except where `h.cursor
                        // { guide_in = … }` keeps the right pad a mouse under a
                        // held guide — and the active mode decides each
                        // independently, via that handler's own guard
                        // (`h.cursor { only_in = … }`).
                        let guide = engine.guide_active();
                        let moved = drive_cursor(
                            ptr,
                            &frame,
                            &mut cursor,
                            cursor_active(guide, modes.cursor_enabled(), modes.cursor_guide_enabled()),
                            &mut hx,
                            now,
                        );
                        // A hold spent on pointing is not a bare tap: once the
                        // pad has moved the cursor under the guide, its release
                        // must not reach Steam as the guide button
                        // (docs/research/text-scrub.md §5) — the same rule a
                        // chord's recognition applies.
                        if guide && moved {
                            engine.consume_hold();
                        }
                        drive_scroll(
                            ptr,
                            &frame,
                            &mut scroll,
                            guide,
                            !modes.scroll_enabled(),
                            &mut hx,
                            now,
                        );
                    }
                }
                // The guide layer's own use for the LEFT pad: circle it and the
                // text caret walks, one step per detent
                // (`docs/research/text-scrub.md` §3.1). A guide-scoped ambient
                // handler, exactly like the guide-mouse on the right pad — it
                // runs only while the guide is held AND `h.scrub`'s guard
                // passes, so the pad's ambient scroll is untouched, and never
                // while the keyboard owns both pads. Its first detent consumes
                // the hold, so the release is not handed to Steam as a tap.
                let scrub_active = engine.guide_active()
                    && !osk.is_active()
                    && config.scrub_enabled_in(modes.state());
                drive_scrub(
                    &mut keyboard,
                    &mut engine,
                    &frame,
                    &mut scrub,
                    config.scrub(),
                    config.cursor(),
                    scrub_active,
                    &mut hx,
                    now,
                );
                // On a padless controller the scrub is a shuttle, and a stick
                // held over reports nothing: hand the answer to the deadline
                // that will step it while the wire is silent.
                sticks.set_shuttle(scrub.shuttle_running(), config.sticks(), now);
                // Bare-button bindings that are *held* (D-pad -> arrows, pad
                // click / triggers -> mouse clicks by default; each code goes
                // to the device it belongs to). Gated OFF on the guide layer
                // (so guide+dpad and guide+l2/r2 stay chords) and while the OSK
                // owns the pads (it takes the pad clicks as key commits itself,
                // via `route_osk`). The per-mode gate is in the *map*:
                // `modes.buttons()` already carries only the bindings whose
                // guard passes, so a game mode that guards them out yields an
                // empty map and `reconcile` releases anything held.
                let buttons_active = !engine.guide_active() && !osk.is_active();
                let clicked = drive_buttons(
                    &mut keyboard,
                    pointer.as_mut(),
                    &button_frame,
                    &prev_frame,
                    modes.buttons(),
                    &mut button_keys,
                    buttons_active,
                    &mut hx,
                );
                // The other side of the same gate: what the desktop layer gives
                // up, the game gets. Rank 3 of the precedence in the module
                // docs — and the falling edge of this gate is what guarantees a
                // game never sees a stuck stick.
                let forwarding = gamepad_forwarding(
                    config.gamepad().enabled,
                    modes.forwards(),
                    modes.desktop_yielded(),
                    engine.guide_active(),
                    osk.is_active(),
                );
                drive_gamepad(
                    &mut gamepad,
                    &frame,
                    // The Steam relay forwards the controller's own bytes
                    // unprocessed; an evdev frame has none, and the uinput pad
                    // re-encodes from the frame either way. `&[]` is what the
                    // relay reads as "nothing to pass through", which is the
                    // truth for a source that is not the controller.
                    raw.as_deref().unwrap_or(&[]),
                    config.gamepad(),
                    forwarding,
                    // The guide's release, asked *after* the forwarding gate so
                    // the two agree about this frame: a bare tap that meant
                    // nothing else becomes a synthesized guide press on the
                    // game sink, and Steam opens its overlay. False on every
                    // other frame, and on every desktop frame.
                    engine.guide_tap(),
                    hx.dev,
                    now,
                    debug,
                );
                // A transient mode's last two exits, settled once this frame's
                // outputs are on the wire.
                //
                // The press cap waits until HERE — and not for tidiness: the
                // press that spends the last of the budget is the keystroke
                // that picked the link, and clearing on the spot would run the
                // handoff, close the bare-button layer and swallow it. It has
                // gone out by now; the handoff below releases it, which is
                // what everything that acts on a key's press edge (Vimium
                // included) wants.
                //
                // A click emitted this frame is an exit of its own: Vimium's
                // hints go on any click, so the daemon goes with them.
                let mut expired = modes.settle_presses(&config);
                if clicked {
                    expired |= modes.note_click(&config);
                }
                if expired {
                    eprintln!("hyprpad: mode -> {} (transient exit)", modes.active());
                    status.set_mode(modes.active());
                    mode_handoff(
                        &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                        pointer.as_mut(),
                        &mut button_keys,
                        &mut chord_keys,
                        &mut keyboard,
                        &mut gamepad,
                        &mut haptics,
                    );
                }
                // The *unmasked* frame: a press eaten by a transient exit is
                // down on the next frame's `prev` too, so it never comes back
                // as a fresh edge while the button stays held.
                prev_frame = frame;
            }
        }
    }
    // On the way out: release the pad and silence any rumble, before `Drop`
    // destroys the device and the game sees it unplug.
    gamepad.release(&mut haptics);
    Ok(())
}

/// Arm the hidraw reader pipeline for one generation of descriptors: spawn
/// `read_all`'s per-node reader threads and a forwarder that funnels their
/// reports into the main loop's channel. Used for both the initial connect and
/// every reconnect, so the two paths are identical.
///
/// Takes the [`hidraw::ControllerSource`] by value because a brokered generation *is*
/// the descriptors — there is nothing to re-open from, and handing them on is
/// the only way to use them.
fn spawn_reader_pipeline(source: hidraw::ControllerSource, tx: &mpsc::Sender<Input>) {
    let reports = hidraw::read_all(source);
    let tx = tx.clone();
    std::thread::spawn(move || forward_reports(reports, tx));
}

/// Forward every report from one reader generation into the loop's input
/// channel, then — once they have all ended (the controller went away) — send a
/// single [`Input::ReadersEnded`] so the loop enters its reconnect wait instead
/// of going silently idle. Split out from [`spawn_reader_pipeline`] so the
/// sentinel behaviour is unit-testable without hardware.
///
/// The decode runs **here**, in the reader thread, rather than in the loop: the
/// evdev backend builds its frames in its own thread too, so doing the same for
/// the controller is what makes [`Input::Frame`] mean one thing whichever controller
/// produced it. A report that does not decode — the controller's other report
/// ids, listed in [`report::Frame::decode`] — is dropped exactly as the loop
/// used to drop it.
///
/// # Two transports arrive here as one stream
///
/// `read_all` merges every node of the generation, the dongle's and Bluetooth's
/// alike, so this function sees them in arrival order and is the natural place
/// for the active-transport rule ([`hidraw::ActiveTransport`]): last decoded
/// frame wins, and only a change is announced. It is deliberately driven by
/// *decoded* frames rather than by any report, so the lizard mouse chattering on
/// a link the user is not using could never claim the transport.
fn forward_reports(reports: mpsc::Receiver<hidraw::Report>, tx: mpsc::Sender<Input>) {
    let debug = std::env::var_os("HYPRSC_DEBUG").is_some();
    let mut active = hidraw::ActiveTransport::new();
    let mut logged_battery = false;
    for r in reports {
        let Some(frame) = report::Frame::decode(&r.data) else {
            // The battery report is the one dropped id worth seeing once: it is
            // the only other thing the controller sends unprompted over
            // Bluetooth (measured at ~0.3 Hz), and `hid-steam.c:1409-1411`
            // builds a whole power_supply out of it, so a future phase that
            // wants a battery reading starts by reading this line.
            if debug && !logged_battery && r.data.first() == Some(&0x43) {
                logged_battery = true;
                let hex: String =
                    r.data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("");
                eprintln!(
                    "hyprpad: dropping report 0x43 ({} bytes, battery/charge state) from {} \
                     [{}]: {hex} — this line appears once per generation",
                    r.data.len(),
                    r.node.display(),
                    r.transport.as_str(),
                );
            }
            continue;
        };
        if let Some(t) = active.note(r.transport) {
            eprintln!("hyprpad: controller streaming over {} ({})", t.as_str(), r.node.display());
            if tx.send(Input::Transport(t)).is_err() {
                return;
            }
        }
        if tx.send(Input::Frame { frame, raw: Some(r.data) }).is_err() {
            return; // main loop gone; nothing to announce to.
        }
    }
    let _ = tx.send(Input::ReadersEnded);
}

/// Forward the evdev backend's events into the loop's single input stream, the
/// way [`forward_reports`] and [`forward_osk_events`] do for the other two
/// sources. Split out for the same reason: it is testable without a device.
fn forward_evdev(events: mpsc::Receiver<crate::evdev::Event>, tx: mpsc::Sender<Input>) {
    for ev in events {
        let input = match ev {
            crate::evdev::Event::Adopted { label, layout, name } => {
                Input::EvdevAdopted { label, layout, name }
            }
            crate::evdev::Event::Frame(frame) => Input::Frame { frame, raw: None },
            crate::evdev::Event::Released => Input::EvdevGone,
        };
        if tx.send(input).is_err() {
            return; // main loop gone
        }
    }
}

/// Forward every event the OSK child reported on its stdout back-channel into
/// the loop's single input stream. Ends when the OSK handle (and hence its
/// reader thread) is gone, or the loop is. Split out from [`run`] so the
/// forwarding is unit-testable without spawning a child, exactly like
/// [`forward_reports`].
fn forward_osk_events(events: mpsc::Receiver<OskEvent>, tx: mpsc::Sender<Input>) {
    for ev in events {
        if tx.send(Input::Osk(ev)).is_err() {
            return; // main loop gone
        }
    }
}

/// One haptic trigger point in the daemon, each gated by its own `[haptics]`
/// toggle. Naming the *occasion* rather than the pulse keeps the "which feel?"
/// decision in one place ([`haptic_feel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Haptic {
    /// An OSK cursor crossed onto a new key (reported by the child).
    Crossing,
    /// An OSK key was committed: a pad click typed the key under its cursor,
    /// or an `[osk_buttons]` binding typed a key through the keyboard.
    Commit,
    /// A guide chord or stick flick resolved to an action.
    Gesture,
    /// One circular-scroll detent was emitted — and one caret-scrub detent at
    /// the character tiers, which is the same notched-wheel feel on the same
    /// pad.
    Scroll,
    /// A caret-scrub detent that moved a whole *word* (`ctrl`+arrow). The
    /// heavier click is how the thumb feels the unit change under it
    /// (`docs/research/text-scrub.md` §3.5); it rides the same `[haptics]
    /// scroll` toggle, because it is the same wheel with a bigger notch.
    ScrubWord,
    /// A bare-button (`[buttons]`) key went down.
    Button,
    /// The desktop cursor travelled one texture-spacing of pixels (right pad).
    CursorMove,
}

/// The pulse a trigger fires under `cfg`, or `None` when `[haptics]` has it
/// switched off — globally (`enabled = false`) or per trigger. Pure, so the
/// whole gating policy is unit-testable without a device.
fn haptic_feel(cfg: &HapticsConfig, what: Haptic) -> Option<Feel> {
    if !cfg.enabled {
        return None;
    }
    let (on, feel) = match what {
        Haptic::Crossing => (cfg.crossing, Feel::Tick),
        Haptic::Commit => (cfg.commit, Feel::Click),
        Haptic::Gesture => (cfg.gesture, Feel::Buzz),
        Haptic::Scroll => (cfg.scroll, Feel::Tick),
        Haptic::ScrubWord => (cfg.scroll, Feel::Click),
        Haptic::Button => (cfg.buttons, Feel::Tick),
        Haptic::CursorMove => (cfg.cursor, Feel::Texture),
    };
    on.then_some(feel)
}

/// The haptic side of one frame: the device handle plus the live `[haptics]`
/// knobs. Bundled into one value so the per-frame routers take a single extra
/// argument, and so every call site reads the *current* config — a `hyprpad
/// reload` retunes the feel on the next pulse with nothing to rebuild.
struct HapticCtx<'a> {
    dev: &'a mut Haptics,
    cfg: &'a HapticsConfig,
    /// Which controller this frame came from. A device with no actuators drops
    /// every pulse — see [`HapticCtx::fire`].
    source: report::Source,
}

impl HapticCtx<'_> {
    /// Fire `what` on `pad`, if the config enables it. Non-blocking: the pulse is
    /// queued for the haptics writer thread and dropped if that queue is full.
    ///
    /// **The one place a source without haptics is handled.** The controller has an
    /// actuator behind each trackpad and the pulses are 200–600 µs; an Xbox pad
    /// has two rumble motors, which `ff-memless` runs at jiffy granularity and
    /// which need tens of milliseconds to spin up. There is no honest way to
    /// play a 250 Hz texture on one, so every `Feel` is dropped here rather
    /// than at each of the dozen call sites — which is what lets
    /// `drive_scroll`, `route_osk` and the rest stay source-agnostic.
    ///
    /// Phase 2 maps only `Gesture` and `Commit` to a short weak-motor tap
    /// (`FF_RUMBLE` over the grabbed evdev fd, or report 3 over the Bluetooth
    /// hidraw node) and keeps dropping the rest; the hook is this one `if`.
    fn fire(&mut self, what: Haptic, pad: HapticPad) {
        if let Some(feel) = haptic_for(self.source, self.cfg, what) {
            self.dev.play(feel, pad, self.cfg.intensity);
        }
    }
}

/// The whole "should this pulse happen" decision, as a pure function: the
/// device must be able to play it *and* the config must want it.
///
/// Split out from [`HapticCtx::fire`] so the source gate is testable without a
/// device, and so it reads as one rule rather than two guards in sequence.
fn haptic_for(source: report::Source, cfg: &HapticsConfig, what: Haptic) -> Option<Feel> {
    source.has_haptics().then(|| haptic_feel(cfg, what)).flatten()
}

/// Which actuator sits under an OSK pad: each thumb feels its own cursor.
fn haptic_pad(pad: OskPad) -> HapticPad {
    match pad {
        OskPad::Left => HapticPad::Left,
        OskPad::Right => HapticPad::Right,
    }
}

/// Which actuator sits under a controller button — the side its feedback should
/// fire on. The face cluster, R bumper/trigger/grips, R3, Menu and the right pad
/// are under the right thumb; the D-pad, L bumper/trigger/grips, L3, View and the
/// left pad under the left. The guide button and the capacitive flags belong to
/// neither hand, so they buzz both. Exhaustive on purpose: a new button has to
/// pick a side.
fn button_pad(b: report::Button) -> HapticPad {
    use report::Button::*;
    match b {
        A | B | X | Y | QuickAccess | R3 | Menu | GripR4 | GripR5 | BumperR1 | PadRightTouch
        | PadRightClick | TriggerR2Full => HapticPad::Right,
        DpadUp | DpadDown | DpadLeft | DpadRight | View | L3 | GripL4 | GripL5 | BumperL1
        | PadLeftTouch | PadLeftClick | TriggerL2Full => HapticPad::Left,
        Steam | Cap0 | Cap1 | Cap2 | Cap3 => HapticPad::Both,
    }
}

/// Forget all per-frame state on disconnect (equivalently, before a reconnect —
/// no reports flow in between): reset the gesture engine, the cursor/scroll
/// dampers, and the OSK router, and clear the previous frame. Without this a
/// stale `prev` (position or timestamp) would jump the cursor or misfire a
/// gesture on the first frame after the gap. Held bare-button output (keys and
/// clicks alike) is [`ButtonKeys`]'s to release; the caller does that beside
/// this.
fn reset_frame_state(
    engine: &mut GestureEngine,
    cursor: &mut CursorState,
    scroll: &mut ScrollState,
    osk_route: &mut OskRoute,
    prev_frame: &mut report::Frame,
) {
    release_outputs(cursor, scroll, osk_route);
    // Forgetting held buttons is right ONLY here: the device went away, so
    // whatever was held is gone too. A mode transition must never do this
    // (see `mode_handoff`).
    *engine = GestureEngine::new();
    *prev_frame = report::Frame::default();
}

/// Drop the desktop layer's *continuous* output state — the cursor/scroll/OSK
/// smoothing state, so nothing differences across a gap — WITHOUT touching
/// gesture edge state. This is the safe half of a reset: it changes what we
/// emit, not what we think is pressed. Pressed output (a held key or mouse
/// button) is a bare-button binding, released by [`ButtonKeys::release_all`].
fn release_outputs(cursor: &mut CursorState, scroll: &mut ScrollState, osk_route: &mut OskRoute) {
    cursor.damper.reset();
    scroll.reset();
    osk_route.reset();
}

/// The clean handoff a mode transition runs, wherever the transition came from
/// — a compositor event (a focus change, a rename of the focused window, a
/// fullscreen change, an overlay, the session locking), the periodic process
/// rescan, a manual override from a binding, or a transient mode giving itself
/// back (its press budget running out, a click, or its timer).
///
/// Whatever the outgoing mode was holding — a held key or mouse button, the
/// virtual pad's last stick values, a mid-flight gesture — is let go before the
/// incoming mode's gates apply. One function, so a new way of *noticing* a
/// transition can never come with a subtly different way of *making* one.
///
/// A config reload is the one re-resolve that does **not** come through here:
/// [`apply_reload`] swaps the config in and asks `ModeEngine::reconfigure` for
/// a fresh answer, and the next frame's [`ButtonKeys::reconcile`] releases only
/// the bindings that actually changed or vanished. Releasing everything would
/// drop a held key that the new file still binds exactly as the old one did.
#[allow(clippy::too_many_arguments)]
fn mode_handoff(
    cursor: &mut CursorState,
    scroll: &mut ScrollState,
    osk_route: &mut OskRoute,
    mut pointer: Option<&mut VirtualPointer>,
    button_keys: &mut ButtonKeys,
    chord_keys: &mut ChordKeys,
    keyboard: &mut Option<VirtualKeyboard>,
    gamepad: &mut GamepadState,
    haptics: &mut Haptics,
) {
    // Outputs only. The gesture engine and `prev_frame` are deliberately NOT
    // reset: a transition is usually *caused* by a chord (guide+view opens the
    // sheet, whose layer flips the mode), and that chord is still physically
    // held. Resetting edge state would make it look freshly pressed on the next
    // frame -> fire again -> toggle the overlay closed -> flip back -> reset ->
    // ... an open/close loop for as long as the chord is held (observed live).
    release_outputs(cursor, scroll, osk_route);
    button_keys.release_all(keyboard, pointer.as_deref_mut());
    // A chord's held output is let go too (a drag under the guide-mouse does
    // not follow us into the next mode); the chord itself stays recognised,
    // so its eventual release finds nothing to do rather than firing again.
    chord_keys.release_all(keyboard, pointer);
    gamepad.release(haptics);
}

/// Build a [`PadDamper`] from the cursor/damping config knobs. Both pad
/// consumers — the desktop cursor and each OSK pad — construct their damper this
/// way, so the on-screen cursors are smoothed with *exactly* the same filter as
/// the desktop cursor: one smoothed-position source, two consumers.
fn damper_from(cfg: &CursorConfig) -> PadDamper {
    PadDamper::new(
        cfg.one_euro_min_cutoff,
        cfg.one_euro_beta,
        cfg.one_euro_d_cutoff,
        cfg.hysteresis,
        cfg.deadzone,
    )
}

/// Cross-frame state for the trackpad cursor.
struct CursorState {
    /// Per-pad smoothing (One Euro + hysteresis + sub-pixel accumulation). Its
    /// `reset()` on lift replaces the old raw `prev = None`, so a lift-and-
    /// retouch never produces a jump.
    damper: PadDamper,
    /// Desktop-cursor gain (px per pad count), from `[cursor] sens`.
    sens: f64,
    /// Pixels of cursor travel accumulated toward the next haptic texture tick
    /// (`[haptics] cursor` / `cursor_spacing_px`). Reset on lift so a re-touch
    /// starts a fresh spacing.
    travel_px: f64,
}

impl CursorState {
    fn new(cfg: &CursorConfig) -> CursorState {
        CursorState {
            damper: damper_from(cfg),
            sens: cfg.sens,
            travel_px: 0.0,
        }
    }

    /// Apply new cursor tuning on a live reload: rebuild the damper from the new
    /// knobs (a fresh [`PadDamper`] with cleared filter state, so a sens/smoothing
    /// change can't jolt the cursor) and update the gain. The held-click flag is
    /// deliberately left untouched — the reload path releases any held click
    /// itself, so this never strands the button.
    fn reconfigure(&mut self, cfg: &CursorConfig) {
        self.damper = damper_from(cfg);
        self.sens = cfg.sens;
    }
}

/// Whether the right pad drives the desktop cursor on this frame.
///
/// The cursor's two guards, joined by the guide button: with the guide **up**
/// the ambient guard decides (`h.cursor { only_in = … }`, `ambient`); with it
/// **held** only the guide guard can keep the pad (`h.cursor { guide_in = … }`,
/// `under_guide`), and the ambient one is irrelevant — the guide layer takes
/// the pad away by default, and a game mode that guards the ambient cursor
/// out (so the game gets the pad) is exactly the one that wants it back under
/// the guide. Pure, so the whole decision table is unit-testable.
fn cursor_active(guide: bool, ambient: bool, under_guide: bool) -> bool {
    if guide {
        under_guide
    } else {
        ambient
    }
}

/// Drive the pointer's *motion* from the right trackpad for a single frame.
/// Returns whether the pointer actually moved.
///
/// `active` is [`cursor_active`]'s answer (or `false` while the on-screen
/// keyboard owns the pads): off, the desktop's claim is released — all filter
/// state forgotten, so re-entry never differences across the gap — and the
/// frame is dropped. On, the pad is smoothed and differenced into pointer
/// motion whoever is holding what: the guide-mouse of `h.cursor { guide_in =
/// … }` is this same path, same damper, same texture haptic. Mouse *buttons*
/// are not this function's business: a pad click or trigger pull is a
/// bare-button binding (`rpad_click = "mouse left"` by default), pressed and
/// released by [`drive_buttons`] — or, under the guide, a chord's held output
/// ([`ChordKeys`]).
fn drive_cursor(
    ptr: &mut VirtualPointer,
    frame: &report::Frame,
    st: &mut CursorState,
    active: bool,
    hx: &mut HapticCtx,
    now: Instant,
) -> bool {
    if !active {
        // Release the desktop's claim: forget the tracking origin (all filter
        // state) so re-entry starts clean.
        st.damper.reset();
        st.travel_px = 0.0;
        return false;
    }

    let mut moved = false;
    if frame.pressed(report::Button::PadRightTouch) {
        // Smooth the absolute pad position first, then difference the *smoothed*
        // position (pointer-damping doc §4): the One Euro Filter + hysteresis
        // crush held-still jitter before the differencing high-pass can amplify
        // it, and sub-pixel accumulation keeps slow, deliberate drags alive.
        let s = st.damper.update(frame.right_pad.x, frame.right_pad.y, now);
        // `relative` inverts y (pad +Y is up, screen +Y is down) and carries the
        // sub-pixel remainder; the first touched frame clutches to (0, 0).
        let (dx, dy) = st.damper.relative(s, st.sens, true);
        if dx != 0 || dy != 0 {
            ptr.move_relative(f64::from(dx), f64::from(dy));
            moved = true;
            // Trackpad texture: one faint pulse per spacing of cursor travel
            // (Steam Input's friction feel), on the pad doing the driving. At
            // most one per frame — the remainder carries, so fast flicks don't
            // burst-fire and slow drags still tick.
            let spacing = hx.cfg.cursor_spacing_px.max(1.0);
            st.travel_px += f64::from(dx).hypot(f64::from(dy));
            if st.travel_px >= spacing {
                st.travel_px %= spacing;
                hx.fire(Haptic::CursorMove, HapticPad::Right);
            }
        }
    } else {
        st.damper.reset();
        st.travel_px = 0.0;
    }
    moved
}

/// Scroll units emitted per `1.0` of normalized finger travel at `sensitivity`
/// `1.0`, in swipe mode. A centre-to-edge swipe (Δ ≈ `1.0`) at sensitivity `1.0`
/// emits this many continuous scroll units (~this ÷ a wheel notch of scrolling).
/// A deliberately *tunable* starting point, like the cursor's gain.
const SWIPE_SCROLL_REF: f64 = 120.0;

/// Cross-frame state for the LEFT-trackpad scroll (the mirror of [`CursorState`]
/// for the right pad). Carries the [`ScrollConfig`], the shared-style
/// [`PadDamper`] smoothing, the previous smoothed position (swipe differencing),
/// and the [`AngleAccumulator`] (circular ticking).
struct ScrollState {
    cfg: ScrollConfig,
    /// Left-pad smoothing — same One Euro + hysteresis pipeline as the cursor,
    /// built from the `[cursor]` knobs — so scrolling isn't jittery. `reset()`
    /// on lift means a re-touch never differences across the gap.
    damper: PadDamper,
    /// Previous smoothed position, for the swipe mode's delta. `None` clutches
    /// the first touched frame to zero scroll.
    prev: Option<(f64, f64)>,
    /// Angle→tick accumulator for the circular mode.
    angle: AngleAccumulator,
}

impl ScrollState {
    fn new(cfg: &ScrollConfig, cursor: &CursorConfig) -> ScrollState {
        ScrollState {
            cfg: *cfg,
            damper: damper_from(cursor),
            prev: None,
            angle: AngleAccumulator::new(
                cfg.circular_step_degrees.to_radians(),
                cfg.circular_min_radius,
            ),
        }
    }

    /// Forget all cross-frame state (on lift or when the desktop's claim is
    /// released), so a re-touch starts clean.
    fn reset(&mut self) {
        self.damper.reset();
        self.prev = None;
        self.angle.reset();
    }

    /// Apply new scroll (and shared cursor-smoothing) tuning on a live reload by
    /// rebuilding from the new knobs. This resets all cross-frame state, so a
    /// mode/sensitivity/step change can't jump the scroll on the next frame.
    fn reconfigure(&mut self, cfg: &ScrollConfig, cursor: &CursorConfig) {
        *self = ScrollState::new(cfg, cursor);
    }

    /// Difference the smoothed position `s` against the previous frame's, for the
    /// swipe mode. The first frame after a reset seeds `prev` and returns
    /// `(0, 0)` (the clutch), exactly like the cursor's relative path.
    fn swipe_delta(&mut self, s: (f64, f64)) -> (f64, f64) {
        let Some((px, py)) = self.prev else {
            self.prev = Some(s);
            return (0.0, 0.0);
        };
        self.prev = Some(s);
        (s.0 - px, s.1 - py)
    }
}

/// Drive scrolling from the LEFT trackpad for a single frame.
///
/// Active only on the ambient (non-guide) layer, when the arbiter is not
/// suppressing desktop control, and while the LEFT pad is touched — the same
/// gate as [`drive_cursor`], but for the left pad and the [`VirtualPointer::scroll`]
/// axis instead of relative motion. Off (mode `off`, gated away, or untouched)
/// forgets all state so a re-touch never jumps.
fn drive_scroll(
    ptr: &mut VirtualPointer,
    frame: &report::Frame,
    st: &mut ScrollState,
    guide_active: bool,
    suppressed: bool,
    hx: &mut HapticCtx,
    now: Instant,
) {
    // Disabled, or the desktop's claim is released (guide layer / focused game /
    // OSK, which calls this with both flags set): drop all state and bail.
    if st.cfg.mode == ScrollMode::Off || guide_active || suppressed {
        st.reset();
        return;
    }
    // Only while the LEFT pad is actually touched; a lift resets so a re-touch
    // never differences across the gap (swipe) or jumps the angle (circular).
    if !frame.pressed(report::Button::PadLeftTouch) {
        st.reset();
        return;
    }

    // Smooth the absolute left-pad position with the same One Euro + hysteresis
    // damper the cursor uses, then read scroll off the *smoothed* signal — so a
    // held-still finger (hard-zeroed by the hysteresis) neither swipes nor ticks.
    let s = st.damper.update(frame.left_pad.x, frame.left_pad.y, now);
    let (dx, dy) = match st.cfg.mode {
        // Unreachable (gated above); keeps the match exhaustive without a wildcard.
        ScrollMode::Off => (0.0, 0.0),
        ScrollMode::Swipe => {
            let (dnx, dny) = st.swipe_delta(s);
            swipe_scroll(dnx, dny, st.cfg.sensitivity, st.cfg.natural, st.cfg.horizontal)
        }
        ScrollMode::Circular => {
            let ticks = st.angle.update(s.0, s.1);
            if ticks == 0 {
                (0.0, 0.0)
            } else {
                // A detent under the rotating thumb — this is what makes the
                // radial scroll feel like a physical wheel. One pulse per frame
                // that emitted at least one detent: several detents inside a
                // single 4 ms frame (a very fast spin) would blur into one buzz
                // anyway, and firing them back-to-back would only smear it.
                hx.fire(Haptic::Scroll, HapticPad::Left);
                circular_scroll(ticks, st.cfg.sensitivity, st.cfg.circular_step_degrees, st.cfg.natural)
            }
        }
    };
    if dx != 0.0 || dy != 0.0 {
        ptr.scroll(dx, dy);
    }
}

// ---------------------------------------------------------------------------
// Sticks in place of pads
// ---------------------------------------------------------------------------

/// Which of the stick-driven layers may act on this step.
///
/// The guards are the **existing** ones, deliberately: a stick cursor is a
/// cursor, so it obeys `h.cursor { only_in = … }` and `guide_in` through
/// [`cursor_active`] exactly as the right pad does, and stick scrolling obeys
/// `h.scroll`'s guard and the guide layer exactly as the left pad does. There
/// is no second permission model to keep in step with the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StickGates {
    cursor: bool,
    scroll: bool,
    /// The keyboard owns both sticks while it is up, as it owns both pads.
    osk: bool,
}

/// One integration step of the stick-driven layers, run off this loop's fourth
/// receive deadline ([`crate::sticks::StickDrive::deadline`]).
///
/// Why a step at all: a gamepad reports **only on change**. Hold a stick at
/// 60 % and the device goes silent, so a cursor driven off frames would move
/// once and stop. Why a *deadline* and not a tick: the deadline is armed only
/// while something is moving, so an idle pad costs nothing (`docs/09`'s "never
/// busy-poll" holds unchanged).
///
/// The OSK comes first and takes both sticks: while the keyboard is up it owns
/// them, as it owns both pads on the controller.
#[allow(clippy::too_many_arguments)]
fn drive_sticks(
    st: &mut crate::sticks::StickDrive,
    mut ptr: Option<&mut VirtualPointer>,
    osk: &mut OskHandle,
    cfg: &crate::config::SticksConfig,
    scroll_cfg: &ScrollConfig,
    gates: StickGates,
    hx: &mut HapticCtx,
    now: Instant,
) {
    if gates.osk {
        // The keyboard on a padless controller owns both sticks, as it owns
        // both pads on the Steam Controller — but what they drive is the
        // **highlight**,
        // not a cursor (`docs/research/xbox-elite.md` §3.5 option (ii); phase 1
        // sent two integrated `cursor L|R` positions here instead). A stick
        // thrown past [`osk::NAV_THROW_OUT`] steps the highlight one key and
        // then repeats while it is held, which is what a spring-loaded stick
        // can do accurately and what every console keyboard does.
        //
        // The left stick is the primary. The right stick nudges the *same*
        // highlight rather than carrying a second one, because there is only
        // ever one: the split layout's two per-hand cursors need two thumbs on
        // two surfaces, and this controller has neither.
        for dir in [st.osk_left.stick(st.sticks.left, now), st.osk_right.stick(st.sticks.right, now)]
            .into_iter()
            .flatten()
        {
            osk.nav(dir);
        }
        // Nothing else runs while the keyboard is up.
        st.cursor.reset();
        st.scroll.reset();
        st.scroll_detent = 0.0;
        return;
    }
    st.osk_left.reset();
    st.osk_right.reset();

    // The RIGHT stick is the pointer, as the right pad is.
    if gates.cursor {
        let (dx, dy) = st.cursor.step_whole(st.sticks.right, &cfg.cursor, now);
        if dx != 0 || dy != 0 {
            if let Some(ptr) = ptr.as_mut() {
                // Frame `+y` is up, screen `+y` is down.
                ptr.move_relative(f64::from(dx), f64::from(-dy));
            }
        }
    } else {
        st.cursor.reset();
    }

    // The LEFT stick scrolls, in the same `wl_pointer.axis` units the pad's
    // circular mode emits — 15 units to a notch — and honouring the same
    // `natural` / `horizontal` knobs, so the stick and the pad read as one
    // setting rather than two.
    if gates.scroll {
        let (dx, dy) = st.scroll.step(st.sticks.left, &cfg.scroll, now);
        // Frame `+y` is up; a non-natural wheel scrolls up when the thumb goes
        // up, which is `dy < 0` in axis convention. `natural` flips it, exactly
        // as `swipe_scroll` does for the pad.
        let out_y = if scroll_cfg.natural { dy } else { -dy };
        let out_x = if scroll_cfg.horizontal {
            if scroll_cfg.natural {
                -dx
            } else {
                dx
            }
        } else {
            0.0
        };
        if out_x != 0.0 || out_y != 0.0 {
            // The detent haptic, on the same policy the circular pad scroll
            // uses: one pulse per notch of travel, at most one per step, with
            // the remainder carried so a slow scroll still ticks. A no-op on a
            // device with no actuators — `HapticCtx::fire` drops it there, in
            // one place, rather than here.
            st.scroll_detent += out_x.hypot(out_y);
            if st.scroll_detent >= SCROLL_NOTCH {
                st.scroll_detent %= SCROLL_NOTCH;
                hx.fire(Haptic::Scroll, HapticPad::Left);
            }
            if let Some(ptr) = ptr.as_mut() {
                ptr.scroll(out_x, out_y);
            }
        }
    } else {
        st.scroll.reset();
        st.scroll_detent = 0.0;
    }
}

/// `wl_pointer.axis` units in one wheel notch — the same 15 the circular pad
/// scroll emits per detent (`sensitivity * circular_step_degrees` at the
/// defaults), so the stick and the pad agree on what a notch is.
const SCROLL_NOTCH: f64 = 15.0;

/// Whether a gamepad the evdev backend would adopt is plugged in right now.
///
/// The startup wait's second exit. `false` whenever the backend is switched off
/// in the config, because a pad nothing is going to read is not a controller
/// being present — that would leave the daemon awake with no input at all,
/// which is worse than waiting.
fn gamepad_present(config: &Config) -> bool {
    config.device().evdev
        && crate::evdev::gamepad_nodes().is_ok_and(|nodes| !nodes.is_empty())
}

/// The `"sources"` list for `status.json`: every controller the daemon can hear
/// right now, controller first.
fn source_names(controller: bool, evdev: Option<(&'static str, &'static str)>) -> Vec<String> {
    let mut out = Vec::new();
    if controller {
        out.push("steam-controller".to_string());
    }
    if let Some((label, _)) = evdev {
        out.push(label.to_string());
    }
    out
}

/// Map a swipe's smoothed-position delta (normalized units, pad `+Y` up) to a
/// `(dx, dy)` continuous scroll vector, in `wl_pointer::axis` convention
/// (`+dy` = down, `+dx` = right).
///
/// Base direction (`natural = false`, traditional desktop wheel): finger up
/// scrolls up (`dy < 0`); `natural = true` inverts to the touch-screen feel
/// (content follows the finger). Horizontal is emitted only when enabled, with
/// the same inversion. Pure, for testability.
fn swipe_scroll(dnx: f64, dny: f64, sensitivity: f64, natural: bool, horizontal: bool) -> (f64, f64) {
    let gain = SWIPE_SCROLL_REF * sensitivity;
    // Pad +Y is up; screen scroll +Y is down. Non-natural: finger up → scroll up
    // (dy < 0), so the y sign is negated; natural flips it back.
    let ysign = if natural { 1.0 } else { -1.0 };
    let dy = ysign * dny * gain;
    let dx = if horizontal {
        // Pad +X and screen scroll +X are both rightward (no axis flip), so the
        // non-natural sign is the opposite of y's.
        let xsign = if natural { -1.0 } else { 1.0 };
        xsign * dnx * gain
    } else {
        0.0
    };
    (dx, dy)
}

/// Map a signed circular tick count to a `(dx, dy)` continuous scroll vector.
///
/// Each tick is `step_degrees` of rotation and carries `sensitivity *
/// step_degrees` scroll units. Base direction (`natural = false`): clockwise
/// (negative ticks) scrolls down (`dy > 0`), counter-clockwise (positive ticks)
/// scrolls up; `natural = true` inverts. Circular scroll is vertical only. Pure,
/// for testability.
fn circular_scroll(ticks: i32, sensitivity: f64, step_degrees: f64, natural: bool) -> (f64, f64) {
    let per_tick = sensitivity * step_degrees;
    // Positive ticks are CCW → up (dy < 0), so negate; natural flips it.
    let base = -f64::from(ticks) * per_tick;
    let dy = if natural { -base } else { base };
    (0.0, dy)
}

// ---------------------------------------------------------------------------
// The caret scrub: the guide layer's jog wheel on the LEFT pad
// ---------------------------------------------------------------------------

/// `KEY_LEFTCTRL`, `KEY_LEFTSHIFT`, `KEY_LEFT`, `KEY_RIGHT` — the four codes
/// the scrub can emit. Spelled as constants rather than parsed per detent; a
/// test pins each against [`KeyChord::parse`]'s own name table, so the two can
/// never drift.
const KEY_LEFTCTRL: u16 = 29;
/// See [`KEY_LEFTCTRL`].
const KEY_LEFTSHIFT: u16 = 42;
/// See [`KEY_LEFTCTRL`].
const KEY_LEFT: u16 = 105;
/// See [`KEY_LEFTCTRL`].
const KEY_RIGHT: u16 = 106;

/// Cross-frame state for the caret scrub — the left pad's jog wheel under a
/// held guide ([`drive_scrub`]).
///
/// The shape is [`ScrollState`]'s, because the wheel is the same wheel: the
/// same [`PadDamper`] smoothing off the same `[cursor]` knobs, the same
/// [`AngleAccumulator`] detent engine. What differs is the emitter (arrow taps
/// on the virtual keyboard, not scroll units on the pointer) and the
/// [`JogPacer`] between them, which turns the spin's *speed* into the size of
/// each step.
///
/// It holds the live [`ScrubConfig`] and [`CursorConfig`] it was built from and
/// rebuilds itself when either changes ([`sync`](Self::sync)), so `hyprpad
/// reload` retunes — or switches on — the wheel with nothing to wire in the
/// reload path.
struct ScrubState {
    cfg: ScrubConfig,
    /// The cursor knobs the damper was built from, so a reload that retunes
    /// smoothing rebuilds this damper too.
    cursor: CursorConfig,
    /// Left-pad smoothing — the same One Euro + hysteresis pipeline the cursor
    /// and the scroll use, so a held-still thumb is hard-zeroed and can never
    /// tick.
    damper: PadDamper,
    /// Angle → detent accumulator: one tick per `detent_deg` of rotation, with
    /// the sub-tick carry that makes a reversal cost a full detent.
    angle: AngleAccumulator,
    /// Speed → step-size ladder (×1 / ×2 / ×4-or-word).
    pacer: JogPacer,
    /// The padless twin of the wheel: deflection → *when the next tap is due*.
    /// Only one of the two is ever live on a given frame, and which is decided
    /// by [`report::Source::has_pads`].
    shuttle: ShuttlePacer,
    /// Previous frame's timestamp, for the pacer's `dt`. `None` until the first
    /// scrubbing frame after a reset.
    last_t: Option<Instant>,
    /// Whether this guide hold has already been marked consumed. The first
    /// detent spends the hold ([`GestureEngine::consume_hold`]); the rest of
    /// the spin has nothing left to spend.
    consumed: bool,
}

impl ScrubState {
    fn new(cfg: &ScrubConfig, cursor: &CursorConfig) -> ScrubState {
        ScrubState {
            cfg: *cfg,
            cursor: cursor.clone(),
            damper: damper_from(cursor),
            angle: AngleAccumulator::new(cfg.detent_deg.to_radians(), cfg.min_radius),
            pacer: JogPacer::new(
                cfg.fast_deg_per_s,
                cfg.slow_deg_per_s,
                cfg.fast_min_detents,
                cfg.word_tier,
            ),
            shuttle: ShuttlePacer::new(
                cfg.shuttle.deadzone,
                cfg.shuttle.slow_per_s,
                cfg.shuttle.fast_per_s,
                cfg.shuttle.word_above,
            ),
            last_t: None,
            consumed: false,
        }
    }

    /// Rebuild from `cfg`/`cursor` if either has changed since the last frame —
    /// the whole of this handler's reload story. Rebuilding resets every
    /// cross-frame value, which is what a retuned detent or damper wants: no
    /// carry from a wheel with different notches on it.
    fn sync(&mut self, cfg: &ScrubConfig, cursor: &CursorConfig) {
        if self.cfg != *cfg || self.cursor != *cursor {
            *self = ScrubState::new(cfg, cursor);
        }
    }

    /// Forget all cross-frame state: on lift, on guide release, and whenever
    /// the gate closes. No output can be stranded by this — the scrub only ever
    /// *taps*, so unlike the cursor's click or a held key there is nothing it
    /// could leave down — which is why it needs no place in `release_outputs`.
    fn reset(&mut self) {
        self.damper.reset();
        self.angle.reset();
        self.pacer.reset();
        self.shuttle.reset();
        self.last_t = None;
        self.consumed = false;
    }

    /// Whether the shuttle is holding a stick over, and the loop therefore owes
    /// it a wakeup ([`crate::sticks::StickDrive::set_shuttle`]). Always false
    /// on a controller with trackpads, where the jog wheel resets it every
    /// frame.
    fn shuttle_running(&self) -> bool {
        self.shuttle.running()
    }
}

/// One frame's worth of scrub output: the `(chord, pressed)` edges to emit, in
/// order, and whether any of them was a word-tier step (which feels heavier).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScrubBurst {
    events: Vec<(KeyChord, bool)>,
    word: bool,
}

/// The key a detent taps: an arrow, `ctrl`-ed for a word jump and `shift`-ed
/// while the select button is held.
///
/// `right` is the direction of travel; `word` picks `ctrl+arrow` over a plain
/// one; `select` adds Shift so the scrub extends a selection instead of moving
/// the caret. Modifier order is ctrl-then-shift, so a config could spell the
/// same chord as `ctrl+shift+left` and get an identical [`KeyChord`] (pinned by
/// a test). Pure.
fn scrub_chord(right: bool, word: bool, select: bool) -> KeyChord {
    let mut mods: [u16; 2] = [0; 2];
    let mut n = 0;
    if word {
        mods[n] = KEY_LEFTCTRL;
        n += 1;
    }
    if select {
        mods[n] = KEY_LEFTSHIFT;
        n += 1;
    }
    let code = if right { KEY_RIGHT } else { KEY_LEFT };
    // `KeyChord::new` only errors past MAX_CHORD_MODS, and two is not past it.
    KeyChord::new(&mods[..n], code).unwrap_or_else(|_| KeyChord::plain(code))
}

/// Turn one frame's signed detent count into the taps it emits, advancing the
/// [`JogPacer`] once per detent.
///
/// Direction: the accumulator counts counter-clockwise positive, and a clock
/// hand reads forward, so **clockwise (negative ticks) is `Right`** and
/// counter-clockwise is `Left`. Each detent is a *complete* tap — press then
/// release, both in this frame — so no client repeat timer is ever armed and a
/// daemon that dies mid-spin leaves nothing held down
/// (`docs/research/text-scrub.md` §1.3). A ×N character step is N taps of the
/// same key; a word step is one `ctrl`+arrow.
///
/// Pure apart from the pacer it advances, which is the point: the whole
/// direction/tier/modifier policy is testable without a device.
fn scrub_burst(ticks: i32, pacer: &mut JogPacer, select: bool) -> ScrubBurst {
    let mut out = ScrubBurst::default();
    if ticks == 0 {
        return out;
    }
    let right = ticks < 0;
    for _ in 0..ticks.unsigned_abs() {
        let step = pacer.detent();
        let word = step.unit == JogUnit::Word;
        out.word |= word;
        let chord = scrub_chord(right, word, select);
        for _ in 0..step.count.max(1) {
            out.events.push((chord, true));
            out.events.push((chord, false));
        }
    }
    out
}

/// Drive the caret scrub from the LEFT input for a single frame — or, on a
/// controller with no trackpads, for a single step of the stick clock.
///
/// The guide-layer twin of [`drive_scroll`]: while the guide button is held and
/// `h.scrub`'s guard passes in the active mode (`active`, which the caller
/// computes from [`Config::scrub_enabled_in`] and `guide_active()`), circling
/// the left pad walks the text caret one step per detent —
/// `docs/research/text-scrub.md` §3.1, option (a).
///
/// **Two gestures, one binding.** A pad is an absolute surface, so it gets the
/// jog wheel below. A stick springs back to centre and has no position to
/// count, so a padless source gets the other half of the video-editing pair
/// instead — the shuttle in [`drive_shuttle`], where deflection picks a *rate*
/// — and this function forks to it on [`report::Source::has_pads`], per frame,
/// because a session may hold either controller and the choice belongs to the
/// device and not to the config. Everything downstream of the gesture is shared:
/// the taps, the select-with-Shift level, the consumed hold, the word tier.
///
/// Precedence, and why it fights nothing:
///
/// * the **guide layer** is where it lives, and the left pad is otherwise idle
///   there ([`drive_scroll`] drops out under the guide), so the ambient scroll
///   is untouched — release the guide and the pad scrolls again;
/// * the **right pad** is the guide-mouse's (`h.cursor { guide_in = … }`) and
///   the **sticks** are `GuideStickFlick`'s: this handler reads neither;
/// * the **on-screen keyboard** owns both pads when it is up, so the caller
///   passes `active = false` there and the wheel resets (scrubbing under the
///   keyboard is phase 3 of the research doc, §3.3).
///
/// The first detent of a hold marks it **consumed**
/// ([`GestureEngine::consume_hold`]), exactly as the guide-mouse does: a hold
/// spent scrubbing must not reach Steam as the bare guide tap when it is
/// released. The pad touch itself is not a chord (`chordable` excludes
/// `PadLeftTouch`), so without this call every caret fix would end in a tap the
/// daemon would hand on.
#[allow(clippy::too_many_arguments)]
fn drive_scrub(
    kbd: &mut Option<VirtualKeyboard>,
    engine: &mut GestureEngine,
    frame: &report::Frame,
    st: &mut ScrubState,
    cfg: &ScrubConfig,
    cursor: &CursorConfig,
    active: bool,
    hx: &mut HapticCtx,
    now: Instant,
) {
    // A reload can retune the wheel or switch it on under us; rebuilding here
    // keeps the reload path from having to know this handler exists.
    st.sync(cfg, cursor);
    // Off in this config, gated out of this mode, guide not held, or the
    // keyboard owns the pads: drop everything so a re-entry starts clean and
    // no rotation carries across.
    if !cfg.enabled || !active {
        st.reset();
        return;
    }
    // Which of the two this controller gets. A pad has an absolute position to
    // read, so it is a jog wheel; a stick springs back, so it is a shuttle.
    // Same binding, same guard, same taps — see [`drive_shuttle`].
    if !frame.source.has_pads() {
        let _ = drive_shuttle(kbd, engine, frame, st, cfg, hx, now);
        return;
    }
    // The wheel from here down. Park the shuttle so the loop is never asked to
    // keep a deadline armed for a stick this controller drives nothing with.
    st.shuttle.reset();
    // Only while the LEFT pad is actually touched. A lift resets, so a
    // lift-and-retouch never injects the angle it jumped across.
    if !frame.pressed(report::Button::PadLeftTouch) {
        st.reset();
        return;
    }

    // Smooth the absolute pad position exactly as the scroll does, then read
    // the angle off the *smoothed* signal: the damper's hard zero is the first
    // of the three hysteresis layers that keep a resting thumb from ticking.
    let s = st.damper.update(frame.left_pad.x, frame.left_pad.y, now);
    let dt = match st.last_t {
        Some(prev) => now.saturating_duration_since(prev).as_secs_f64(),
        None => 0.0,
    };
    st.last_t = Some(now);
    let ticks = st.angle.update(s.0, s.1);
    // Feed the rate estimator every frame, ticking or not, so the ladder sees
    // the spin slow down between detents as well as speed up into them.
    st.pacer.observe(st.angle.last_delta(), dt);
    if ticks == 0 {
        return;
    }

    // A detent that fires is a hold spent: the guide's release is ours, not
    // Steam's. Once per hold — `consume_hold` is idempotent, but saying so here
    // keeps the "first detent" rule visible.
    if !st.consumed {
        engine.consume_hold();
        st.consumed = true;
    }

    let select = frame.pressed(cfg.select);
    let burst = scrub_burst(ticks, &mut st.pacer, select);
    for (chord, pressed) in &burst.events {
        // Arrows and modifiers are all keyboard codes, so there is no pointer
        // for `emit_chord` to route anything to.
        emit_chord(kbd, None, chord, *pressed);
    }
    // One pulse per frame that emitted at least one detent, as the circular
    // scroll does — several detents inside one 4 ms frame would blur into a
    // single buzz anyway. A word step gets the heavier click, so the thumb can
    // feel that the unit changed under it.
    let feel = if burst.word { Haptic::ScrubWord } else { Haptic::Scroll };
    hx.fire(feel, HapticPad::Left);
}

/// The caret scrub on a controller with **no trackpads**: the LEFT stick's
/// horizontal deflection as a shuttle, for a single step of the loop's clock.
///
/// [`drive_scrub`]'s other half, and the answer to "how do I scrub without the
/// trackpad". A jog wheel needs an absolute surface to count position on; a
/// stick has none, so it does the other half of the video-editing pair — rate
/// control. Hold it over and the caret walks at a speed the deflection chooses
/// ([`ShuttlePacer`]): `slow_per_s` at the deadzone edge, rising to
/// `fast_per_s` at `word_above`, and past that the same tap is a `ctrl`+arrow
/// so the caret hops word boundaries instead of characters. Everything else is
/// the wheel's, unchanged and deliberately so — the same [`scrub_chord`] taps
/// (press and release in the same step, never a held key), the same
/// select-with-Shift level, the same heavier feel for a word, and the same rule
/// that the first tap spends the guide hold.
///
/// Only the horizontal axis is read. Lines are the D-pad's job already, and a
/// vertical channel here would turn every diagonal thumb into a caret jumping
/// rows it was not asked to.
///
/// **Called on a clock, not only on frames.** A gamepad reports only on change,
/// so a stick *held* over is silence on the wire; the loop's stick deadline
/// (`crate::sticks::StickDrive`, 4 ms) is what steps this, with the last frame
/// as the state, exactly as the rate-controlled cursor is stepped. That is why
/// the emission is time-based and not per-frame: the taps come from the clock.
///
/// Returns the chord this step tapped, if any — which is what the tests read,
/// the daemon having nowhere else to put it.
fn drive_shuttle(
    kbd: &mut Option<VirtualKeyboard>,
    engine: &mut GestureEngine,
    frame: &report::Frame,
    st: &mut ScrubState,
    cfg: &ScrubConfig,
    hx: &mut HapticCtx,
    now: Instant,
) -> Option<KeyChord> {
    // The same normalization the stick cursor uses, so one deadzone number
    // means the same push on every path.
    let x = crate::sticks::Deflection::of(frame).left.0;
    let tap = st.shuttle.update(x, now)?;
    // A tap is a hold spent, exactly as a detent is: the guide's release is
    // ours, not Steam's.
    if !st.consumed {
        engine.consume_hold();
        st.consumed = true;
    }
    let chord = scrub_chord(tap.right, tap.word, frame.pressed(cfg.select));
    emit_chord(kbd, None, &chord, true);
    emit_chord(kbd, None, &chord, false);
    hx.fire(
        if tap.word { Haptic::ScrubWord } else { Haptic::Scroll },
        HapticPad::Left,
    );
    Some(chord)
}

/// Cross-frame state for the *held* bare-button bindings
/// ([`ButtonAction::Hold`]): the evdev codes currently held down, keyed by the
/// controller button holding each. Kept so a gate change (the guide/OSK layer
/// opening, or a game taking focus, while a button is held) releases the key
/// cleanly instead of leaving it stuck down. A fired binding
/// ([`ButtonAction::Fire`]) has no state here: it is over the moment its press
/// edge is.
struct ButtonKeys {
    held: HashMap<report::Button, KeyChord>,
    /// Whether the bare-button layer was live at the end of the last
    /// [`reconcile`](Self::reconcile), with no transition since. A press is
    /// honoured only when this was already true: the frame the gate opens on
    /// — or the first frame after a handoff, a reload, a disconnect
    /// ([`release_all`](Self::release_all)) — presses nothing, so a button
    /// still held from the previous layer waits for a fresh press (module
    /// docs, "Layer transitions").
    open: bool,
}

impl ButtonKeys {
    fn new() -> ButtonKeys {
        ButtonKeys { held: HashMap::new(), open: false }
    }

    /// Reconcile the desired key state against what is held, returning the
    /// `(button, chord, pressed)` events to emit and updating the held set. A
    /// chord is one event, not one per code: the expansion into modifier and
    /// key edges is [`chord_edges`]'s, so this stays about *which* binding is
    /// down and never about the order its codes go down in. Pure
    /// (no I/O), so the gating and release-on-state-change logic is
    /// unit-testable. The originating button rides along so the caller can fire
    /// feedback on the side of the controller it sits on.
    ///
    /// When `active` is false (guide/OSK), every held key is released.
    /// Otherwise each held key whose button lifted — **or whose binding has
    /// gone away or changed**, which is what a mode transition or a reload
    /// looks like from here — releases, and each `Hold`-bound button on a
    /// **press edge** (down now per `pressed`, up last frame per
    /// `was_pressed`) presses its key — provided the layer was already live
    /// last frame. A button that is down when the gate opens, or that a
    /// handoff released while it stayed down, is down on the previous frame
    /// from then on and so never an edge: it presses again only after a
    /// release. Releases are emitted before presses. `Fire` bindings are not
    /// this function's: they have no held state ([`fire_edges`]).
    fn reconcile<F: Fn(report::Button) -> bool, G: Fn(report::Button) -> bool>(
        &mut self,
        bindings: &HashMap<report::Button, ButtonAction>,
        pressed: F,
        was_pressed: G,
        active: bool,
    ) -> Vec<(report::Button, KeyChord, bool)> {
        let mut events = Vec::new();
        // Release anything that should no longer be held: the gate closed, the
        // button lifted, or the binding is no longer live in this mode.
        self.held.retain(|&btn, &mut chord| {
            let keep =
                active && pressed(btn) && bindings.get(&btn) == Some(&ButtonAction::Hold(chord));
            if !keep {
                events.push((btn, chord, false));
            }
            keep
        });
        // Press only on a fresh edge, and only once the layer has been live for
        // a whole frame: a button already down when it became live — or pressed
        // on the very frame it did — waits for its release.
        let settled = active && self.open;
        self.open = active;
        if settled {
            for (&btn, what) in bindings {
                let ButtonAction::Hold(chord) = *what else { continue };
                if pressed(btn) && !was_pressed(btn) && !self.held.contains_key(&btn) {
                    events.push((btn, chord, true));
                    self.held.insert(btn, chord);
                }
            }
        }
        events
    }

    /// Release every held key and mouse button immediately, outside the
    /// per-frame reconcile, and mark a transition.
    ///
    /// Used by the mode-transition handoff and the disconnect path: `reconcile`
    /// would get there on the next report anyway, but "no key is stranded
    /// across a mode switch" should not depend on another report arriving —
    /// and after a disconnect none will. The transition mark is what keeps a
    /// button that is *still down* from pressing again on the next frame: a
    /// Tab that walks the bar's panels flips the mode away and back within a
    /// few milliseconds, each flip running this, and without the mark one tap
    /// became two or three.
    fn release_all(
        &mut self,
        kbd: &mut Option<VirtualKeyboard>,
        mut pointer: Option<&mut VirtualPointer>,
    ) {
        for (_, chord) in self.held.drain() {
            emit_chord(kbd, pointer.as_deref_mut(), &chord, false);
        }
        self.open = false;
    }
}

/// Cross-frame state for the *held* outputs on guide chords: the evdev code
/// each chord is holding down, keyed by the chord's button. The chord-side
/// twin of [`ButtonKeys`], kept apart from it because the two have different
/// edges — a bare binding follows its button's level through `reconcile`, a
/// chord's output is pressed on recognition ([`handle_gesture`]) and released
/// on [`GestureEvent::GuideChordRelease`] or the guide's release, whichever
/// comes first. Nothing here is a binding lookup: by the time a code lands in
/// this map every gate and guard has already passed.
///
/// Released beside [`ButtonKeys`] on the mode handoff and the disconnect path,
/// so a click held under the guide-mouse is never stranded across either.
struct ChordKeys {
    held: HashMap<report::Button, KeyChord>,
}

impl ChordKeys {
    fn new() -> ChordKeys {
        ChordKeys { held: HashMap::new() }
    }

    /// A chord on `btn` resolved to a held `chord`: the `(chord, pressed)`
    /// edges to emit. Normally one press. Should the button already be holding
    /// an output — its release was never seen, which a fresh gesture engine can
    /// do — that one is let go first, so no code is ever pressed twice. Pure.
    fn press(&mut self, btn: report::Button, chord: KeyChord) -> Vec<(KeyChord, bool)> {
        let mut events = Vec::with_capacity(2);
        if let Some(old) = self.held.insert(btn, chord) {
            events.push((old, false));
        }
        events.push((chord, true));
        events
    }

    /// The chord button lifted under the guide: the chord to release, if it
    /// was holding one. A button holding nothing (its chord was not a key, or
    /// the handoff already let it go) is `None`. Pure.
    fn release(&mut self, btn: report::Button) -> Option<KeyChord> {
        self.held.remove(&btn)
    }

    /// Forget everything held, returning the chords to release. Pure.
    fn drain(&mut self) -> Vec<KeyChord> {
        self.held.drain().map(|(_, chord)| chord).collect()
    }

    /// Release every held output now: on the guide's release, the mode
    /// handoff, and the disconnect path. Same contract as
    /// [`ButtonKeys::release_all`] — a missing device drops the edge, the
    /// held set empties regardless.
    fn release_all(
        &mut self,
        kbd: &mut Option<VirtualKeyboard>,
        mut pointer: Option<&mut VirtualPointer>,
    ) {
        for chord in self.drain() {
            emit_chord(kbd, pointer.as_deref_mut(), &chord, false);
        }
    }
}

/// Which virtual device a bare-button binding's evdev code is emitted on.
///
/// The `[buttons]` map holds one code space — evdev's — and this is the single
/// place it is split: a `BTN_*` mouse code (`mouse left`, 0x110..=0x112) is
/// clicked through the virtual pointer, everything else is a `KEY_*` typed
/// through the virtual keyboard. Pure, so the decision is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Keyboard,
    Pointer(PointerButton),
}

fn route(code: u16) -> Route {
    match PointerButton::from_evdev(code) {
        Some(b) => Route::Pointer(b),
        None => Route::Keyboard,
    }
}

/// The `(code, pressed)` edges one edge of a held [`KeyChord`] expands to.
///
/// Pressing runs the modifiers in order and then the key; releasing runs the
/// key and then the modifiers in reverse, so the combo brackets its key the way
/// a real keyboard does and nothing is left down out of order. A chord with no
/// modifiers is the single edge it always was.
///
/// Pure, and the *only* place the order lives: [`ButtonKeys`] and
/// [`ChordKeys`] both hand their edges through here, so a bare button and a
/// guide chord can never press a combo differently.
fn chord_edges(chord: &KeyChord, pressed: bool) -> Vec<(u16, bool)> {
    let mut edges = Vec::with_capacity(chord.mods().len() + 1);
    if pressed {
        edges.extend(chord.mods().iter().map(|&m| (m, true)));
        edges.push((chord.code(), true));
    } else {
        edges.push((chord.code(), false));
        edges.extend(chord.mods().iter().rev().map(|&m| (m, false)));
    }
    edges
}

/// Emit one edge of a held chord, code by code ([`chord_edges`]), each on the
/// device it belongs to ([`emit_button_code`]). The modifiers are always
/// `KEY_*` and so always the keyboard's; only the key itself can be a mouse
/// button, which is what makes `shift+btn_left` a coherent binding.
fn emit_chord(
    kbd: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    chord: &KeyChord,
    pressed: bool,
) -> bool {
    let mut clicked = false;
    for (code, down) in chord_edges(chord, pressed) {
        clicked |= emit_button_code(kbd, pointer.as_deref_mut(), code, down);
    }
    clicked
}

/// How long a tapped key stays down ([`tap_chord`]).
///
/// The on-screen keyboard's own figure (`osk/src/output.rs`), and for the same
/// reason: a press and a release in the same instant is a legal evdev sequence
/// but an easy one for a client to coalesce, and 6 ms is below any human
/// threshold while being a whole event loop to everything downstream.
const TAP_HOLD: Duration = Duration::from_millis(6);

/// The `(code, pressed)` edges a **tapped** chord expands to: the press edges
/// and then, with nothing in between but [`TAP_HOLD`], the release edges.
///
/// Pure, and the whole definition of "a key in a sequence is a tap": the key
/// goes down and comes straight back up inside the one press, with its
/// modifiers bracketing it exactly as they do on a held binding
/// ([`chord_edges`]) — so `h.seq { h.key "shift+f", … }` lands as a real
/// Shift+F and not as two loose keystrokes.
fn tap_edges(chord: &KeyChord) -> Vec<(u16, bool)> {
    let mut edges = chord_edges(chord, true);
    edges.extend(chord_edges(chord, false));
    edges
}

/// Tap a key or mouse button — press, [`TAP_HOLD`], release — for a step of an
/// [`Action::Seq`] that has no release edge of its own to pair with.
fn tap_chord(
    kbd: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    chord: &KeyChord,
) {
    let mut waited = false;
    for (code, down) in tap_edges(chord) {
        // The key is held for the length of the tap, not for zero time: a
        // press and a release in the same instant is legal evdev but an easy
        // pair for a client to coalesce into nothing.
        if !down && !waited {
            std::thread::sleep(TAP_HOLD);
            waited = true;
        }
        emit_button_code(kbd, pointer.as_deref_mut(), code, down);
    }
}

/// Emit one bare-button edge on the device its code belongs to. A missing
/// device (uinput or the virtual-pointer protocol unavailable) makes the edge a
/// no-op for that code only; the other device keeps working, and the held set
/// is updated regardless so nothing is remembered as down that was never sent.
///
/// Returns whether a **mouse button actually went down** on the wire — the one
/// fact a transient mode wants back from the output layer
/// ([`ModeEngine::note_click`]): Vimium's hints exit on any click, so a mode
/// that says `exit_on = { "click" }` follows them out. A click that went
/// nowhere (no virtual pointer) is not a click.
fn emit_button_code(
    kbd: &mut Option<VirtualKeyboard>,
    pointer: Option<&mut VirtualPointer>,
    code: u16,
    pressed: bool,
) -> bool {
    match route(code) {
        Route::Pointer(mb) => match pointer {
            Some(ptr) => {
                ptr.button(mb, pressed);
                pressed
            }
            None => false,
        },
        Route::Keyboard => {
            if let Some(kbd) = kbd.as_mut() {
                kbd.key(code, pressed);
            }
            false
        }
    }
}

/// Drive the *held* bare-button bindings ([`ButtonAction::Hold`]) for a single
/// frame: press each bound button's key or mouse button on its down edge
/// (against `prev`) and release it on its up edge, subject to `active` (the
/// guide/OSK/suppressed gate, mirroring [`drive_cursor`]). Each code is routed
/// to the device it belongs to ([`route`]); a missing keyboard or pointer
/// silences the codes that would have gone to it and nothing else, so the pad
/// click still clicks on a box with no uinput. The bindings that *fire* are
/// [`fire_buttons`]'s.
///
/// Feedback fires on the **press edge only** ([`Haptic::Button`], off by
/// default): a held D-pad auto-repeats in the kernel — which produces no further
/// events here, so a repeat never buzzes — and a release is not a keystroke.
///
/// Returns whether a mouse button went down this frame, for the transient-mode
/// click exit ([`emit_button_code`]).
#[allow(clippy::too_many_arguments)]
fn drive_buttons(
    kbd: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    frame: &report::Frame,
    prev: &report::Frame,
    bindings: &HashMap<report::Button, ButtonAction>,
    st: &mut ButtonKeys,
    active: bool,
    hx: &mut HapticCtx,
) -> bool {
    let events = st.reconcile(bindings, |b| frame.pressed(b), |b| prev.pressed(b), active);
    let mut clicked = false;
    for (btn, chord, pressed) in events {
        clicked |= emit_chord(kbd, pointer.as_deref_mut(), &chord, pressed);
        if pressed {
            hx.fire(Haptic::Button, button_pad(btn));
        }
    }
    clicked
}

/// The bare-button actions due to *fire* this frame: every
/// [`ButtonAction::Fire`] binding whose button went down on this frame and
/// not the last. The press edge and nothing else — a button held across
/// frames fires once, and its release fires nothing — and nothing at all
/// while the gate is closed: a press under the guide layer or the keyboard is
/// swallowed, not deferred. Pure, so the edge selection is unit-testable
/// without a device.
fn fire_edges(
    bindings: &HashMap<report::Button, ButtonAction>,
    frame: &report::Frame,
    prev: &report::Frame,
    active: bool,
) -> Vec<(report::Button, Action)> {
    if !active {
        return Vec::new();
    }
    frame
        .edges_down(prev)
        .filter_map(|b| match bindings.get(&b) {
            Some(ButtonAction::Fire(action)) => Some((b, action.clone())),
            _ => None,
        })
        .collect()
}

/// Every bare button that went down this frame, or nothing while the layer is
/// gated off — the presses a transient mode is *offered*
/// ([`ModeEngine::note_press`], which decides which of them count).
///
/// Deliberately unfiltered by the binding map, unlike [`fire_edges`]: a
/// transient mode's named exit matters whatever it is bound to, or to nothing
/// at all — B is the cancel precisely because the hints mode leaves it
/// unbound. The engine holds the resolved map and applies it to the presses
/// that *are* about bindings (the press cap). Pure, so the selection is
/// testable without a device.
fn press_edges(frame: &report::Frame, prev: &report::Frame, active: bool) -> Vec<report::Button> {
    if !active {
        return Vec::new();
    }
    frame.edges_down(prev).collect()
}

/// Fire the bare-button actions due this frame ([`fire_edges`]), each through
/// [`perform_action`] — the path a guide chord takes once it has resolved, so
/// `h.exec` (with `HYPRPAD_MODE`), `h.dispatch`, `h.workspace`, `h.keyboard`,
/// `h.set_mode` and the rest mean on a bare button exactly what they mean on a
/// chord. Returns whether one of them moved the active mode, for the caller to
/// run the same handoff a chord's override gets.
///
/// Feedback is [`Haptic::Button`] on the press edge, as for a held binding: to
/// the hand this is a button press, whatever it does.
#[allow(clippy::too_many_arguments)]
fn fire_buttons(
    hypr: &Hypr,
    config: &Config,
    modes: &mut ModeEngine,
    osk: &mut OskHandle,
    keyboard: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    frame: &report::Frame,
    prev: &report::Frame,
    active: bool,
    hx: &mut HapticCtx,
) -> bool {
    let mut mode_changed = false;
    for (btn, action) in fire_edges(modes.buttons(), frame, prev, active) {
        hx.fire(Haptic::Button, button_pad(btn));
        mode_changed |= perform_action(
            hypr,
            config,
            modes,
            osk,
            frame.source,
            keyboard,
            pointer.as_deref_mut(),
            &action,
        );
    }
    mode_changed
}

// ---------------------------------------------------------------------------
// The virtual gamepad: forwarding under focus, and rumble back to the controller.
// ---------------------------------------------------------------------------

/// How often a non-zero rumble command is (re-)written to the controller, and the
/// minimum gap between any two rumble writes.
///
/// This is `hid-steam`'s own `HZ / 20`. The driver rate-limits rumble to 20 Hz
/// *and* re-sends the same packet every 50 ms while either magnitude is
/// non-zero, because "the controller resets the haptic pattern every time it
/// receives a rumble packet, leading to weird discontinuities or sometimes
/// cutting out entirely". Matching it is not an optimisation — it is what makes
/// rumble feel continuous.
const RUMBLE_INTERVAL: Duration = Duration::from_millis(50);

/// Narrowest and widest `on_us` the pulse-mode rumble approximation will use, in
/// µs. The floor is [`Feel::Texture`]'s width (the faintest pulse the daemon
/// fires) and the ceiling [`Feel::Click`]'s, so a game's rumble spans exactly
/// the range of feels the device is already known to produce.
const PULSE_ON_MIN: u16 = 200;
const PULSE_ON_MAX: u16 = 600;

/// Whether the virtual gamepad should be forwarding this frame.
///
/// The single expression of the precedence documented at the top of this module
/// and in [`crate::gamepad`]: **guide > OSK > game-forwarding > desktop**.
///
/// * `guide_active` — the guide layer owns the controller globally, in a game
///   just as on the desktop. `Guide+R1` must switch workspace mid-game.
/// * `osk_active` — the on-screen keyboard owns both pads. `Guide+Y` is meant to
///   raise it over a game, and while it is up the game gets a neutral pad rather
///   than the thumb movements aimed at the keyboard.
/// * `game_focused` — the active mode declares `forward = true`
///   ([`ModeEngine::forwards`]); on the built-in path that is exactly "a
///   game-classed window holds focus".
/// * `desktop_suppressed` — the ambient desktop layer has actually let go of
///   the pads ([`ModeEngine::desktop_yielded`]). Consulting it as well as
///   `game_focused` is what keeps ranks 3 and 4 mutually exclusive: a manual
///   override that forces the desktop live over a game, or a mode that
///   forwards while leaving the cursor guarded in, yields rather than having
///   both layers drive at once.
///
/// Pure, so the whole gate is unit-testable without a device or a compositor.
fn gamepad_forwarding(
    enabled: bool,
    game_focused: bool,
    desktop_suppressed: bool,
    guide_active: bool,
    osk_active: bool,
) -> bool {
    enabled && game_focused && desktop_suppressed && !guide_active && !osk_active
}

/// Whether this frame should synthesize a guide press on the game sink.
///
/// The owner's rule in one line: *the Steam button, pressed by itself, in a
/// game, opens the Steam overlay.* Each conjunct is one word of that.
///
/// * `bare_tap` — the gesture engine's
///   [`guide_tap`](GestureEngine::guide_tap): a quick press-and-release that
///   was not a chord, not a flick, not a hold the daemon spent on the pads, and
///   not long enough to have been a deliberation. Everything the desktop layer
///   claims, it keeps.
/// * `forwarding` — the very same [`gamepad_forwarding`] decision the frame's
///   input rides on, evaluated at the release (by which point the guide is up,
///   so the guide layer is no longer suppressing it). No game, no pulse: on the
///   desktop a bare tap goes on meaning nothing, exactly as it always has.
/// * the config's [`GuideTap`], so the whole thing is one word to turn off.
///
/// Pure, so the decision table is unit-testable without a device.
fn guide_tap_pulse(cfg: &GamepadConfig, forwarding: bool, bare_tap: bool) -> bool {
    cfg.guide_tap == GuideTap::Steam && forwarding && bare_tap
}

/// Whether this frame's guide release should run the **`guide_tap` binding**
/// ([`crate::config::GestureKey::Tap`]).
///
/// The other consumer of the same bare tap, and the precedence between the two
/// written down in one line: *a bare tap has exactly one consumer, and
/// [`guide_tap_pulse`] gets first refusal.* In a game the pulse takes it and no
/// binding runs — the owner's rule, "in a game the Steam button is Steam's" —
/// and everywhere else the binding does.
///
/// The two conjuncts:
///
/// * `bare_tap` — the engine's narrow decision, carried on the event
///   ([`gesture::GestureEvent::GuideLeave`]). A chorded hold, a hold the
///   guide-mouse or the caret scrub spent, a hold already holding a button, and
///   a hold past `guide_tap_max_ms` are each a leave that binds nothing: a
///   caret fix must not also summon whatever `guide_tap` is bound to, and
///   neither must three seconds of deliberating.
/// * not [`guide_tap_pulse`] — so the tap Steam is about to be handed is not
///   *also* a binding. Note what this leaves standing: with `guide_tap =
///   "none"` there is no pulse to defer to anywhere, so the binding runs in a
///   game too, which is exactly what that word means ("the guide is hyprpad's
///   alone, in a game too").
///
/// The binding's own guard is a separate, later question — `:not_in("game")`
/// on the config side says the same thing again, in the language a config has.
///
/// Pure, so the decision table is unit-testable without a device.
fn guide_tap_binding(cfg: &GamepadConfig, forwarding: bool, bare_tap: bool) -> bool {
    bare_tap && !guide_tap_pulse(cfg, forwarding, bare_tap)
}

/// Create the virtual Steam Controller, if the config asks for one *and* a
/// `/dev/uhid` descriptor can be had.
///
/// Called once, at startup. Unlike the Xbox pad — which is made lazily, on the
/// first frame a game actually holds focus — the relay is created eagerly and
/// kept for the daemon's whole life, because Steam adopts a controller when its
/// hidraw node appears and re-adopts it every time it reappears. Creating it on
/// a focus change would make Steam re-detect and re-apply configs each time a
/// game came forward.
///
/// **Every failure is non-fatal and logged exactly once.** On an ordinary
/// desktop `acquire_uhid` fails with `EACCES` — `/dev/uhid` is `crw------- root
/// root` and no shipped rule opens it — which is the expected outcome until the
/// host-integration half lands. The daemon carries on with no game sink at all
/// rather than quietly substituting the Xbox pad, which would be a different
/// controller than the config asked for.
fn start_relay(cfg: &GamepadConfig) -> Option<SteamRelay> {
    if !cfg.enabled || cfg.kind != GamepadKind::Steam {
        return None;
    }
    let profile = cfg.identity.profile();
    let (fd, from) = match uhid::acquire_uhid_from() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!(
                "hyprpad: [gamepad] kind = \"steam\" but /dev/uhid is unavailable ({e}); \
                 games get no input. Install the root fd broker with `hyprpad setup`, \
                 then check it with `hyprpad setup --check`."
            );
            return None;
        }
    };
    match SteamRelay::start(fd, profile) {
        Ok(relay) => {
            eprintln!(
                "hyprpad: virtual Steam Controller created ({}, identity '{}', \
                 /dev/uhid from the {from}); \
                 streaming at 250 Hz, forwarding under game focus",
                profile.vid_pid(),
                profile.identity.as_str()
            );
            Some(relay)
        }
        Err(e) => {
            eprintln!("warning: could not create the virtual Steam Controller ({e}); \
                       games get no input");
            None
        }
    }
}

/// Which sink `status.json` should report.
///
/// Deliberately reports what a game would *actually* receive, not what the
/// config asked for: `kind = "steam"` with no usable `/dev/uhid` is
/// [`RelayKind::None`]. Pure, so the whole mapping is unit-testable.
fn relay_kind(cfg: &GamepadConfig, relay_live: bool) -> RelayKind {
    if !cfg.enabled {
        return RelayKind::None;
    }
    match cfg.kind {
        GamepadKind::Xbox => RelayKind::Xbox,
        GamepadKind::Steam if relay_live => RelayKind::Steam,
        GamepadKind::Steam => RelayKind::None,
    }
}

/// Cross-frame state for the virtual gamepad: the lazily-created device, the
/// edge-detection flag behind the neutral-on-transition guarantee, and the
/// rumble command last written to the controller.
struct GamepadState {
    /// The uinput device. `None` until the first frame that actually wants to
    /// forward — or forever, if creation failed. Always `None` under
    /// `[gamepad] kind = "steam"`: only ever one sink (see [`start_relay`]).
    pad: Option<VirtualGamepad>,
    /// The virtual Valve controller, when `[gamepad] kind = "steam"` and a
    /// `/dev/uhid` descriptor could be had. Unlike the uinput pad this is
    /// created **once, at startup**, and never torn down on a focus change: a
    /// create/destroy cycle makes Steam re-detect the controller, re-apply its
    /// configs and toast about it (research doc §5.3).
    relay: Option<SteamRelay>,
    /// The `(strong, weak)` magnitudes Steam last asked the relay for.
    ///
    /// Kept as *state* rather than acted on where it arrives, so the relay's
    /// rumble goes through exactly the same 20 Hz coalescing clock as the Xbox
    /// pad's: [`drive_rumble`] reads it once a frame.
    relay_rumble: (u16, u16),
    /// Whether Steam held the virtual device open on the previous frame.
    ///
    /// Only the **falling** edge matters, and only for one thing: it is when
    /// the controller's IMU goes back to hyprpad's own preference, because whoever
    /// asked for it has stopped listening. Starts `false`, so a daemon that
    /// comes up with Steam already holding the device sees one rising edge and
    /// no spurious restore.
    relay_open: bool,
    /// Whether creation has been attempted, so a failure warns exactly once
    /// instead of on every frame of every game.
    tried: bool,
    /// Whether the previous frame was forwarding. The falling edge of this flag
    /// is the neutral-on-transition guarantee.
    forwarding: bool,
    /// The `(strong, weak)` magnitudes last written to the controller, and when — the
    /// two halves of the 20 Hz coalescing clock.
    rumble: (u16, u16),
    rumble_sent: Option<Instant>,
    /// The report form `rumble` was written in. Tracked separately from the
    /// config so a live reload that switches `rumble_mode` can stop the running
    /// rumble in the form that started it (see [`drive_rumble`]).
    rumble_mode: RumbleMode,
}

impl GamepadState {
    fn new() -> GamepadState {
        GamepadState {
            pad: None,
            relay: None,
            relay_rumble: (0, 0),
            relay_open: false,
            tried: false,
            forwarding: false,
            rumble: (0, 0),
            rumble_sent: None,
            rumble_mode: RumbleMode::default(),
        }
    }

    /// Give up the controller: neutral the pad and stop any rumble, without
    /// destroying the device.
    ///
    /// [`drive_gamepad`] already does this on every in-session transition; this
    /// is for the two events that bypass the per-frame path entirely — the
    /// controller disconnecting, and the daemon exiting.
    fn release(&mut self, haptics: &mut Haptics) {
        if let Some(pad) = self.pad.as_mut() {
            pad.neutral();
        }
        // The relay is never destroyed here — only fed neutral. Steam must not
        // see the controller disappear when the controller naps or a game loses focus.
        if let Some(relay) = self.relay.as_ref() {
            relay.release();
        }
        self.relay_rumble = (0, 0);
        self.forwarding = false;
        self.stop_rumble(haptics);
    }

    /// Stop whatever rumble is running, in the form it was started in.
    fn stop_rumble(&mut self, haptics: &mut Haptics) {
        if self.rumble != (0, 0) {
            emit_rumble(haptics, self.rumble_mode, (0, 0));
            self.rumble = (0, 0);
            self.rumble_sent = None;
        }
    }
}

/// Forward one frame to whichever game sink is live, or release it.
///
/// Two sinks can stand here, chosen by `[gamepad] kind`, and **only ever one at
/// a time** — two would put two pads in front of Steam and make every press
/// count twice:
///
/// * the **Xbox pad** ([`crate::gamepad`]), where each frame becomes one
///   SYN-terminated uinput report (only the axes and buttons that changed are
///   written). Created here rather than at startup, on the first frame that
///   wants it, so a session that never focuses a game never creates one; games
///   discover it through the usual udev hotplug path.
/// * the **Steam relay** ([`crate::uhid`]), where each frame updates what a
///   250 Hz streamer thread is already sending. That device is created at
///   startup instead ([`start_relay`]) and outlives every focus change, because
///   Steam re-detects a controller whenever its node appears.
///
/// On the frame `forwarding` goes false — for *any* reason: the game lost
/// focus, the guide button went down, the on-screen keyboard came up, the
/// config was reloaded with `enabled = false` — the live sink is neutralled
/// exactly once. Neither sink is destroyed there; a game is left with a
/// connected, idle controller rather than a vanished one.
///
/// With `kind = "steam"` and no relay (no writable `/dev/uhid`) there is no
/// sink at all: the Xbox pad is deliberately **not** substituted, because it is
/// a different controller than the config asked for.
#[allow(clippy::too_many_arguments)]
fn drive_gamepad(
    st: &mut GamepadState,
    frame: &report::Frame,
    raw: &[u8],
    cfg: &GamepadConfig,
    forwarding: bool,
    guide_tap: bool,
    haptics: &mut Haptics,
    now: Instant,
    debug: bool,
) {
    // Whatever Steam has written to the relay since the last frame, first: a
    // rumble it asked for should not wait a frame behind the one it is
    // reacting to. Reads nothing when there is no relay.
    absorb_relay_writes(st, cfg, forwarding, haptics, debug);

    // A bare guide tap, in a game, with the feature on: the press the sink was
    // never allowed to relay is made up here instead. The gate is inside
    // `guide_tap_pulse` — `forwarding` is one of its conjuncts, so a tap on the
    // desktop arms nothing at all — and whichever sink exists is armed just
    // BEFORE this frame is handed to it, so the pulse starts on this report.
    let pulse = guide_tap_pulse(cfg, forwarding, guide_tap);

    if forwarding {
        if let Some(relay) = st.relay.as_ref() {
            // The Steam sink. `raw` goes in unprocessed: the triton profile
            // relays the controller's own bytes, so anything re-encoded on the way
            // would be a loss.
            if pulse {
                relay.pulse_guide();
            }
            relay.forward(raw, frame, strip_mask(cfg));
        } else {
            if st.pad.is_none() && !st.tried && cfg.kind == GamepadKind::Xbox {
                st.tried = true;
                match VirtualGamepad::new() {
                    Ok(pad) => {
                        eprintln!(
                            "hyprpad: virtual gamepad created (045e:028e); \
                             forwarding under game focus"
                        );
                        st.pad = Some(pad);
                    }
                    Err(e) => eprintln!("warning: no virtual gamepad ({e}); games get no input"),
                }
            }
            if let Some(pad) = st.pad.as_mut() {
                if pulse {
                    pad.pulse_guide();
                }
                pad.apply(frame, cfg.forward_guide);
            }
        }
        st.forwarding = true;
    } else if st.forwarding {
        // The falling edge, and the only place it is handled. Both sinks make
        // the same promise here: neutral, so no game is left holding a stick.
        if let Some(pad) = st.pad.as_mut() {
            pad.neutral();
        }
        if let Some(relay) = st.relay.as_ref() {
            relay.release();
        }
        st.forwarding = false;
    }
    drive_rumble(st, cfg, forwarding, haptics, now);
}

/// Which buttons the relay withholds from Steam.
///
/// The guide is hyprpad's global chord modifier, so it is stripped unless
/// `forward_guide` hands it back — the same knob, and the same meaning, as on
/// the Xbox pad, where it gates `BTN_MODE`. Read per frame, so a reload
/// retunes it on the next report.
///
/// Quick Access is not stripped yet. §4.4 of the research doc suggests deriving
/// the whole mask from the live binding table so any button the owner has bound
/// is withheld; that is a follow-on, and it is recorded in
/// `docs/design/uhid-relay.md`.
fn strip_mask(cfg: &GamepadConfig) -> StripMask {
    StripMask { guide: !cfg.forward_guide, quick_access: false }
}

/// Drain and act on everything Steam wrote to the relay.
///
/// Steam drives a Valve controller it has adopted: the proven run took 39
/// `SetSettingsValues` writes inside two minutes. Rumble and haptics become
/// real pulses on the controller through `haptics.rs`'s single writer thread — §5.2
/// is explicit that the relay must never open a second writable fd on the
/// device. Everything else is logged under `HYPRSC_DEBUG` and dropped.
///
/// **The queue is drained whether or not a game is forwarding, and its
/// actuator commands are then dropped unless one is.** Both halves matter. The
/// draining keeps the relay's bounded queue from filling and stalling, and
/// keeps the debug log honest about what Steam is doing. The dropping is §5.2's
/// arbitration: while hyprpad owns the controller — the desktop, the OSK, a
/// held guide — hyprpad owns the actuators too, so a backgrounded game cannot
/// buzz the on-screen keyboard.
fn absorb_relay_writes(
    st: &mut GamepadState,
    cfg: &GamepadConfig,
    forwarding: bool,
    haptics: &mut Haptics,
    debug: bool,
) {
    let Some(relay) = st.relay.as_ref() else { return };
    let ours = forwarding && cfg.rumble;
    for action in relay.drain_actions() {
        if debug {
            eprintln!("[relay] {}", action.describe());
        }
        match action {
            // Held, not fired: `drive_rumble` applies it on the shared 20 Hz
            // clock, the same one the Xbox pad's force feedback runs on.
            RelayAction::Rumble { strong, weak } if ours => st.relay_rumble = (strong, weak),
            RelayAction::Haptic { pad, on_us, off_us, count } if ours => {
                // Steam's trackpad haptics are game feedback, so they answer to
                // the same `rumble_intensity` knob as its rumble — "turn what
                // the game does to my hands down" should mean one setting, not
                // two. Strength on this device *is* pulse width (the IBEX pulse
                // struct has no gain field), so the knob scales `on_us`;
                // `Haptics::pulse` clamps the result to its own ceiling.
                let on_us = gamepad::scale_magnitude(on_us, cfg.rumble_intensity);
                haptics.pulse(pad, on_us, off_us, count);
            }
            // The one setting that is relayed rather than interpreted: only the
            // real controller can turn its own IMU on, so Steam's gyro request has to
            // reach the hardware or the gyro bytes never appear in the `0x42`
            // the triton profile is already passing through (gap G5).
            //
            // It does **not** become a second writer. The value goes into
            // `lizard`'s shared cell and rides out on the same `0x87` frame that
            // module already re-sends every 30 s; `nudge` only asks it to send
            // now instead of at the next tick, and is skipped entirely when
            // Steam re-states a mode the controller is already holding — which it
            // does, repeatedly.
            RelayAction::Settings(ref pairs) => {
                if let Some(mode) = settings::imu_mode(pairs) {
                    if crate::lizard::set_imu_requested(Some(mode)) {
                        if debug {
                            eprintln!(
                                "[relay] IMU {} -> controller (Steam asked)",
                                settings::gyro_mode::describe(mode)
                            );
                        }
                        crate::lizard::nudge();
                    }
                }
            }
            // Drops, malformed frames, and every actuator command that arrived
            // while hyprpad owns the pads: logged above, and deliberately not
            // acted on. See `uhid::settings` for the policy.
            _ => {}
        }
    }

    // Steam letting go of the fake hands the IMU back to hyprpad's own
    // preference — off, unless `gyro = true`. Without this, quitting Steam with
    // a gyro game open would leave the controller streaming IMU data to nobody until
    // its next power cycle. Edge-triggered, so an ordinary session of Steam
    // holding the device open costs nothing.
    let open = relay.is_open();
    if open != st.relay_open {
        st.relay_open = open;
        if !open && crate::lizard::set_imu_requested(None) {
            if debug {
                eprintln!("[relay] Steam closed the device; IMU restored to hyprpad's preference");
            }
            crate::lizard::nudge();
        }
    }
}

/// Carry the game's force feedback back to the real controller for one frame.
///
/// The magnitudes come from the virtual pad's force-feedback reader thread
/// ([`crate::gamepad`]), already scaled by whatever `FF_GAIN` the client set;
/// this applies the `[gamepad] rumble_intensity` knob on top, gates on
/// forwarding, and enforces the 20 Hz clock.
fn drive_rumble(
    st: &mut GamepadState,
    cfg: &GamepadConfig,
    forwarding: bool,
    haptics: &mut Haptics,
    now: Instant,
) {
    // A live reload can switch the report form under a running rumble. The new
    // form has no way to stop what the old one started (a `0x80` zero does not
    // end a pulse train, and no pulse ends a native rumble), so stop it in its
    // own form first and start clean.
    if cfg.rumble_mode != st.rumble_mode {
        st.stop_rumble(haptics);
        st.rumble_mode = cfg.rumble_mode;
    }

    // Not forwarding, or rumble switched off: the target is silence. Reading the
    // knobs per frame is what makes `hyprpad reload` take effect on the next one.
    // Where the magnitudes come from is the one difference between the two
    // sinks: the Xbox pad's force-feedback reader thread publishes what a game
    // uploaded, while the relay carries what Steam wrote to the virtual
    // controller. Both are then scaled, clocked and emitted identically.
    let (strong, weak) = if !(forwarding && cfg.rumble) {
        (0, 0)
    } else if st.relay.is_some() {
        st.relay_rumble
    } else {
        match st.pad.as_ref() {
            Some(pad) => pad.rumble().magnitudes(),
            None => (0, 0),
        }
    };
    let want = (
        gamepad::scale_magnitude(strong, cfg.rumble_intensity),
        gamepad::scale_magnitude(weak, cfg.rumble_intensity),
    );
    if !rumble_due(want, st.rumble, st.rumble_sent, now) {
        return;
    }
    st.rumble = want;
    st.rumble_sent = Some(now);
    emit_rumble(haptics, cfg.rumble_mode, want);
}

/// Whether a rumble command goes out on this frame.
///
/// Mirrors `hid-steam`'s coalescing: at most one write per [`RUMBLE_INTERVAL`],
/// and a *repeat* write every interval while the rumble is non-zero (the
/// controller needs the refresh). A change arriving inside the window is simply
/// picked up by the next refresh, exactly as the driver's cached magnitudes are.
///
/// **A stop is the one exception** and goes out immediately: a rumble left
/// running is the single failure a player actually feels, so it is never made to
/// wait for the clock. Pure, so the whole policy is unit-testable.
fn rumble_due(
    want: (u16, u16),
    sent: (u16, u16),
    last_sent: Option<Instant>,
    now: Instant,
) -> bool {
    if want == (0, 0) {
        return sent != (0, 0);
    }
    match last_sent {
        None => true,
        Some(t) => now.saturating_duration_since(t) >= RUMBLE_INTERVAL,
    }
}

/// Write one rumble command to the controller in the configured form.
fn emit_rumble(haptics: &mut Haptics, mode: RumbleMode, (strong, weak): (u16, u16)) {
    match mode {
        // The controller's own force-feedback report, byte-for-byte what `hid-steam`
        // sends for an `FF_RUMBLE` effect.
        RumbleMode::Native => haptics.rumble(strong, weak),
        // The approximation: a train of the `0x81` pulse under each thumb, sized
        // to fill one refresh window so consecutive windows run together. There
        // is no "stop a train" report and none is needed — a train that is not
        // re-fired simply runs out inside the window, which is why silence here
        // is the absence of a write rather than a write of zero.
        RumbleMode::Pulse => {
            for (pad, magnitude) in [(HapticPad::Left, strong), (HapticPad::Right, weak)] {
                if let Some((on_us, off_us, count)) = pulse_train(magnitude) {
                    haptics.pulse(pad, on_us, off_us, count);
                }
            }
        }
    }
}

/// Map one `FF_RUMBLE` magnitude to a `0x81` pulse train filling one
/// [`RUMBLE_INTERVAL`], as `(on_us, off_us, count)`. `None` for silence.
///
/// **A starting point, not a calibration.** Strength on this device is pulse
/// width — the IBEX pulse struct has no gain field — so the magnitude is scaled
/// across [`PULSE_ON_MIN`]..[`PULSE_ON_MAX`] at a 50 % duty cycle (the shape of
/// the kernel's own mode-switch buzz), and the repeat count is whatever fills
/// the refresh window. It will not feel like a real rumble motor; it exists so
/// that if the `0x80` report turns out inert on this unit, a game still gets
/// *something* under the thumbs. Pure, for testability.
fn pulse_train(magnitude: u16) -> Option<(u16, u16, u16)> {
    if magnitude == 0 {
        return None;
    }
    let span = u32::from(PULSE_ON_MAX - PULSE_ON_MIN);
    let on_us = PULSE_ON_MIN + ((u32::from(magnitude) * span) / u32::from(u16::MAX)) as u16;
    // 50 % duty: on and off equal, so the train is a continuous buzz rather than
    // a string of distinguishable ticks.
    let off_us = on_us;
    let period_us = u32::from(on_us) + u32::from(off_us);
    let count = (RUMBLE_INTERVAL.as_micros() as u32 / period_us).clamp(1, u32::from(u16::MAX));
    Some((on_us, off_us, count as u16))
}

/// Whether to take ownership of the controller's lizard mode. Enabled by the
/// `own_lizard` config flag (`[daemon]` section) or the `HYPRPAD_OWN_LIZARD`
/// environment variable. The env var wins when set to a truthy value
/// (`1`/`true`/`yes`/`on`); an empty or falsey value forces it off, so it can
/// override a config that turned it on. Default off.
fn lizard_ownership_enabled(config: &Config) -> bool {
    match std::env::var("HYPRPAD_OWN_LIZARD") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => config.own_lizard(),
    }
}

/// Write end of the self-pipe the SIGHUP/SIGUSR1 handler pokes. `-1` until
/// [`install_reload_signal`] runs. Separate from the lizard restore-on-signal
/// pipe in [`crate::lizard`]: that one drives an exit, this one drives work the
/// loop does and keeps running afterwards.
static RELOAD_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// The SIGHUP/SIGUSR1 handler. Async-signal-safe by construction: it does
/// nothing but `write` the signal number to the self-pipe so the waiter thread
/// can run the real work in ordinary thread context. Mirrors
/// [`crate::lizard`]'s `on_exit_signal`, but this pipe feeds the loop, not a
/// process exit.
///
/// The signal *number* is what goes on the wire (both fit in a `u8`), so one
/// pipe carries both requests and [`signal_to_input`] is the only place that
/// decides which is which.
extern "C" fn on_reload_signal(sig: libc::c_int) {
    let fd = RELOAD_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // `write` is on POSIX's async-signal-safe list; best-effort, ignore the
        // result — a full pipe just means a request is already pending.
        let _ = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

/// Which loop input a caught signal asks for. The pure half of the waiter
/// thread, so the mapping is unit-tested without signals, pipes or a daemon.
///
/// * `SIGHUP` — re-read the config (`hyprpad reload`).
/// * `SIGUSR1` — turn the controller off (`hyprpad off`).
///
/// Anything else is ignored: only those two handlers are installed, so a third
/// number arriving on the pipe means the byte was corrupt, and dropping it is
/// safer than guessing which of the two was meant — one of them powers the
/// controller off.
fn signal_to_input(sig: libc::c_int) -> Option<Input> {
    match sig {
        libc::SIGHUP => Some(Input::Reload),
        libc::SIGUSR1 => Some(Input::ControllerOff),
        _ => None,
    }
}

/// Turn the controller off, and say what happened.
///
/// The single place [`Action::ControllerOff`] and [`Input::ControllerOff`] both
/// land, so a chord and `hyprpad off` cannot drift. An `Err` here is almost
/// always "every controller node STALLed", i.e. the controller is already asleep —
/// which is the outcome that was asked for — so it is reported at a low key and
/// never propagated.
fn controller_off() {
    match crate::lizard::turn_off_controller() {
        Ok(()) => eprintln!("hyprpad: controller off (0x9F sent)"),
        Err(e) => eprintln!("hyprpad: controller off not delivered: {e}"),
    }
}

/// Install SIGHUP handling that asks the main loop to reload its config, and
/// SIGUSR1 handling that asks it to turn the controller off.
///
/// This reuses the same self-pipe pattern as the lizard restore-on-signal
/// handler ([`crate::lizard::install_signal_restore`]) rather than adding a
/// competing mechanism: the async-signal-safe handler only `write`s a byte to a
/// pipe, and a dedicated waiter thread — blocked on the read end — turns each
/// poke into an [`Input::Reload`] on the loop's channel, where the reload runs
/// in ordinary context (loading the config, rebuilding dampers, logging). Because
/// SIGHUP's default action is to *terminate*, installing this handler is what
/// keeps a reload request from killing the daemon.
///
/// SIGINT/SIGTERM stay entirely with [`crate::lizard`] (whose waiter restores
/// lizard mode and exits); SIGHUP and SIGUSR1 are distinct signals handled here,
/// so the two never contend. Those two share one pipe and one waiter — the
/// handler writes the signal number and [`signal_to_input`] decides — which
/// keeps the async-signal-safe surface at exactly one `write`. `SA_RESTART`
/// keeps a signal mid-read from erroring out the hidraw reader threads.
/// Best-effort: if the pipe can't be created we log and leave both at their
/// defaults. SIGUSR1's default action is also to *terminate*, so installing this
/// is what keeps `hyprpad off` from killing the daemon it is talking to.
fn install_reload_signal(tx: &mpsc::Sender<Input>) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!(
            "warning: reload-on-SIGHUP / controller-off-on-SIGUSR1 not installed (pipe: {})",
            std::io::Error::last_os_error()
        );
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    RELOAD_WRITE_FD.store(write_fd, Ordering::Relaxed);

    // SAFETY: `action` is fully zero-initialised, then `sa_sigaction`, `sa_mask`,
    // and `sa_flags` are set to valid values before `sigaction` reads it; the
    // null old-action pointer is allowed.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_reload_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut());
    }

    let tx = tx.clone();
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        loop {
            // SAFETY: `read_fd` is the live read end; `byte` is a valid 1-byte buffer.
            let n = unsafe { libc::read(read_fd, byte.as_mut_ptr().cast(), 1) };
            if n < 0 {
                // SA_RESTART covers most cases, but tolerate a stray EINTR.
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return; // unexpected read error: stop waiting.
            }
            if n == 0 {
                return; // write end closed (only at shutdown).
            }
            // Ordinary thread context now: nudge the loop with whatever the
            // signal asked for. If the loop is gone the channel is closed and
            // we stop.
            let Some(input) = signal_to_input(libc::c_int::from(byte[0])) else {
                continue; // not one of ours; nothing to do.
            };
            if tx.send(input).is_err() {
                return;
            }
        }
    });
}

/// Choose the config to run with after a reload attempt.
///
/// On a successful load the new config replaces the current one; on a parse
/// error the current config is kept verbatim — the key safety property, so a
/// typo in the file never crashes or blanks the running daemon. Returns the
/// chosen config alongside the outcome the caller logs. Pure and side-effect
/// free, so the keep-old-config-on-error policy is unit-tested without touching
/// signals or the filesystem.
///
/// This is the **last-good retention** half of the Lua guardrails, and it needs
/// nothing new to cover a `config.lua`: [`crate::lua_config::load_str`] builds
/// a fresh interpreter and a fresh `Config` and only returns `Ok` when the file
/// compiled *and* ran *and* validated, so a broken `config.lua` reaches here as
/// an `Err` with nothing swapped — exactly like a broken `config.toml`.
fn resolve_reload(current: Config, loaded: Result<Config, String>) -> (Config, Result<(), String>) {
    match loaded {
        Ok(new) => (new, Ok(())),
        Err(e) => (current, Err(e)),
    }
}

/// Apply a SIGHUP / `hyprpad reload` request: re-read the config from disk and,
/// on success, swap it in live and re-derive everything cached from it.
///
/// Bindings, the `[buttons]` map, and the `[haptics]` knobs are read per-event
/// straight off `config` (`config.resolve(..)`, `config.buttons()`,
/// `config.haptics()`), so swapping the `config` value is all those need. Only
/// the pre-built pad dampers carry tuning that must be rebuilt: the cursor and
/// scroll [`PadDamper`]s (from `[cursor]`/`[scroll]`) and the OSK router's
/// dampers (from `[cursor]`). Rebuilding resets their filter state, so a tuning
/// change can't jump the cursor/scroll.
///
/// On a parse error the current config is kept and the error logged (see
/// [`resolve_reload`]).
fn apply_reload(
    config: &mut Config,
    cursor: &mut CursorState,
    scroll: &mut ScrollState,
    osk_route: &mut OskRoute,
    own_lizard: &mut bool,
    modes: &mut ModeEngine,
) {
    // `mem::take` lets `resolve_reload` own the current config so it can hand it
    // straight back on a parse error; on success it returns the freshly loaded
    // one instead. Either way we reinstall a real config immediately.
    let (new_config, outcome) = resolve_reload(std::mem::take(config), Config::load());
    *config = new_config;

    if let Err(e) = outcome {
        eprintln!("warning: config reload failed: {e}; keeping current config");
        return;
    }

    // A held key or mouse button is a bare-button binding, and the next frame's
    // `reconcile` settles it against the NEW map: a binding that survived the
    // reload stays held (a reload mid-drag keeps the drag), one that changed or
    // vanished is released. Nothing to do here.
    cursor.reconfigure(config.cursor());
    scroll.reconfigure(config.scroll(), config.cursor());
    // The OSK router shares the `[cursor]` smoothing knobs; rebuild it too so a
    // tuning change reaches the on-screen cursors. Reset (fresh dampers) is fine
    // whether or not the keyboard is currently up.
    *osk_route = OskRoute::new(config.cursor());

    // The power knobs are read out of this cell by the next settings write, so a
    // reload retunes them with nothing to restart — within one `RESEND_INTERVAL`
    // if ownership was already on, and immediately if it just came on.
    crate::lizard::set_power_settings(config.power_settings());
    crate::lizard::set_imu_preference(config.gamepad().imu_preference());
    // The exit policy is re-read live too, so flipping `restore_lizard_on_exit`
    // and reloading changes what the *next* exit does with no restart. The
    // signal handler and the guard are installed once, at startup; both consult
    // this cell at the moment they fire, which is what makes that work.
    crate::lizard::set_restore_on_exit(config.restore_lizard_on_exit());
    // The gyro baseline is the one of the two a reload can make *urgent* — the
    // owner flipped `gyro` expecting to see IMU bytes — so ring the doorbell
    // rather than making them wait out a 30 s tick.
    crate::lizard::nudge();

    let new_own = lizard_ownership_enabled(config);
    if new_own != *own_lizard {
        apply_own_lizard_change(new_own);
        *own_lizard = new_own;
    }

    // Modes and guards came from the new file: re-resolve immediately against
    // the *unchanged* context, so the reload takes effect without waiting for
    // the next focus change. A held key or click settles on the next frame's
    // `reconcile` against the re-resolved map.
    if modes.reconfigure(config) {
        eprintln!("hyprpad: mode -> {} (reload)", modes.active());
    }

    eprintln!("hyprpad: config reloaded");
}

/// React to a live change of the effective `own_lizard` setting on reload.
///
/// Turning it **on** disables lizard mode once, now, so the change takes visible
/// effect immediately. Turning it **off** leaves the device as-is (documented
/// choice): the firmware keyboard/mouse comes back on the next power-cycle, or on
/// exit if the restore guard is installed — we don't re-enable it here to avoid
/// over-engineering. The periodic re-send loop, the restore-on-exit guard, and
/// the SIGINT/SIGTERM restore are wired once at startup and are not reconfigured
/// live, so a full ownership change wants a restart. (When `HYPRPAD_OWN_LIZARD`
/// is set it pins the value, so this never fires from a config edit.)
fn apply_own_lizard_change(now_on: bool) {
    if now_on {
        eprintln!(
            "hyprpad: own_lizard enabled via reload; disabling lizard mode now \
             (periodic re-send + restore-on-exit unchanged until restart)"
        );
        if let Err(e) = crate::lizard::disable_lizard_mode() {
            eprintln!("warning: lizard disable on reload: {e}");
        }
    } else {
        eprintln!(
            "hyprpad: own_lizard disabled via reload; leaving current device state \
             (firmware kbd/mouse restored on exit or next power-cycle)"
        );
    }
}

/// Path to the daemon's pidfile: `$XDG_RUNTIME_DIR/hyprpad.pid`. `hyprpad run`
/// writes it on startup and removes it on clean exit; `hyprpad reload` reads it
/// to find the daemon. Returns `None` when `XDG_RUNTIME_DIR` is unset/empty.
pub fn pid_file_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(dir).join("hyprpad.pid"))
}

/// Owns the daemon pidfile for the life of [`run`]: writes `$XDG_RUNTIME_DIR/
/// hyprpad.pid` on creation and removes it on drop (the clean-return and panic
/// paths). A signal-driven exit (`std::process::exit` from the lizard waiter)
/// skips the drop and leaves the file, but that is harmless — [`reload`] treats a
/// pidfile whose pid is gone as "not running".
struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Write the pidfile with our PID. Best-effort: a failure (e.g. no
    /// `XDG_RUNTIME_DIR`) just means `hyprpad reload` won't find us, and a manual
    /// `kill -HUP <pid>` still works — so we log and carry on rather than fail.
    fn create() -> Option<PidFile> {
        let path = pid_file_path()?;
        match std::fs::write(&path, format!("{}\n", std::process::id())) {
            Ok(()) => {
                eprintln!("hyprpad: pidfile {} (reload with `hyprpad reload`)", path.display());
                Some(PidFile { path })
            }
            Err(e) => {
                eprintln!("warning: could not write pidfile {}: {e}", path.display());
                None
            }
        }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The `hyprpad reload` subcommand: find the running daemon via its pidfile and
/// send it SIGHUP, which triggers a live config reload (see
/// [`install_reload_signal`]). Prints a clear message and returns `Err` when no
/// live daemon can be found, so a stale or missing pidfile is not mistaken for
/// success. (`kill -HUP <pid>` by hand does the same thing for power users.)
pub fn reload() -> Result<(), String> {
    let pid = signal_daemon(libc::SIGHUP)?;
    eprintln!("hyprpad: sent SIGHUP to daemon (pid {pid}); it will re-read its config");
    Ok(())
}

/// The `hyprpad off` subcommand: find the running daemon via its pidfile and
/// send it SIGUSR1, which makes it turn the **controller** off (see
/// [`install_reload_signal`] and [`Input::ControllerOff`]) — the CLI spelling of
/// the same one-shot the `guide+quickaccess` chord fires, and what the bar
/// widget runs on a right-click.
///
/// It goes through the daemon rather than sending `0x9F` itself for two
/// reasons: the daemon already holds the controller's descriptors (which, once
/// `packaging/udev/72-hyprpad-puck.rules` is installed, is the only process
/// that can without the broker), and one writer to the device is the invariant
/// the whole design rests on. So this reports "no daemon" rather than falling
/// back to opening the controller behind its back.
pub fn controller_off_command() -> Result<(), String> {
    let pid = signal_daemon(libc::SIGUSR1)?;
    eprintln!("hyprpad: sent SIGUSR1 to daemon (pid {pid}); turning the controller off");
    Ok(())
}

/// Find the running daemon through its pidfile and send it `sig`, returning the
/// pid it went to.
///
/// Shared by [`reload`] and [`controller_off_command`], so "which daemon" is
/// answered once: a missing pidfile, a corrupt one and a stale one each get
/// their own message rather than a confusing failure at `kill` time.
fn signal_daemon(sig: libc::c_int) -> Result<i32, String> {
    let Some(path) = pid_file_path() else {
        return Err("cannot locate pidfile (XDG_RUNTIME_DIR is unset)".to_string());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "no pidfile at {} — is the daemon running? start it with `hyprpad run`",
                path.display()
            ));
        }
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let pid: i32 = text
        .trim()
        .parse()
        .map_err(|_| format!("pidfile {} is corrupt (not a pid): {:?}", path.display(), text.trim()))?;

    // Probe liveness with signal 0 (checks existence/permission, sends nothing)
    // so a stale pidfile gives a clear message instead of a confusing failure.
    // SAFETY: `kill` with a pid and signal is always safe to call.
    if unsafe { libc::kill(pid, 0) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Err(format!(
                "daemon pid {pid} from {} is not running (stale pidfile); start it with `hyprpad run`",
                path.display()
            ));
        }
        return Err(format!("cannot signal daemon pid {pid}: {err}"));
    }

    // SAFETY: sending a signal to the daemon pid.
    if unsafe { libc::kill(pid, sig) } != 0 {
        return Err(format!(
            "sending signal {sig} to pid {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(pid)
}

/// What the loop remembers about the focused window between compositor events,
/// so a `windowtitle` line can be answered without asking the compositor
/// anything, and so the process rescan has a clock.
struct FocusWatch {
    /// The focused window's address, from `activewindowv2`. A `windowtitle`
    /// event for any other address is some background window renaming itself
    /// (a browser tab, a spinner in another terminal) and is not our business.
    /// Empty until the first focus change of the session.
    address: String,
    /// Whether this compositor emits the `windowtitlev2` form, which carries
    /// the new title on the wire. The target fork emits **both** forms for
    /// every rename, so once one v2 line has been seen the bare `windowtitle`
    /// line is redundant — and answering it anyway would cost a
    /// `j/activewindow` round trip per rename, on a stream that runs at
    /// spinner speed.
    titles_v2: bool,
    /// Earliest instant the focused window's process tree may be re-walked.
    /// Pushed out by every walk, whatever caused it, so the interval measures
    /// time since the last walk rather than time since the last timeout.
    next_rescan: Instant,
}

impl FocusWatch {
    fn new() -> FocusWatch {
        FocusWatch { address: String::new(), titles_v2: false, next_rescan: Instant::now() }
    }

    /// Push the process rescan a full interval out. Called whenever the tree
    /// has just been (or is about to be) re-walked.
    fn arm_rescan(&mut self, config: &Config) {
        self.next_rescan = Instant::now() + Duration::from_millis(config.process_rescan_ms());
    }

    /// Decide what a `windowtitle` event asks of the loop.
    ///
    /// Pure, so the focused-window test is unit-testable without a compositor:
    /// everything it needs is the event's address and title, this struct, and
    /// the live knob.
    fn title_update(
        &mut self,
        config: &Config,
        address: &str,
        title: Option<String>,
    ) -> TitleUpdate {
        if title.is_some() {
            self.titles_v2 = true;
        }
        // A config that declares no modes (every `config.toml`) resolves on
        // window class alone, so no rename can change its answer — the same
        // "never pay for what you did not ask for" rule as `needs_focus_pid`.
        if config.modes().is_empty()
            || !config.rescan_on_title_change()
            || address.is_empty()
            || address != self.address
        {
            return TitleUpdate::Ignore;
        }
        match title {
            Some(t) => TitleUpdate::Title(t),
            None if self.titles_v2 => TitleUpdate::Ignore,
            None => TitleUpdate::Query,
        }
    }
}

/// What a `windowtitle` event asks the loop to do.
#[derive(Debug, PartialEq, Eq)]
enum TitleUpdate {
    /// Another window renamed itself, or the title path is switched off:
    /// nothing to do, and nothing to ask the compositor.
    Ignore,
    /// The focused window's new title, straight off the wire (`windowtitlev2`).
    Title(String),
    /// The focused window renamed itself, but this line did not say to what
    /// (the bare `windowtitle` form): ask `j/activewindow`.
    Query,
}

/// Feed a compositor event into the mode engine; returns whether the active
/// mode changed (so the caller can run the clean handoff).
///
/// The `activewindow` event carries class and title only. When a config
/// actually declares modes, the focused window's **pid** and fullscreen state
/// are fetched with one `j/activewindow` query — on the focus change, never per
/// frame — because `ctx.focus:process_tree_has(..)` needs a pid to walk from. A
/// config with no modes (every `config.toml`) never pays for the round trip.
///
/// `windowtitle` is the second context source, and the one that fixes starting a
/// program inside an already-focused terminal: the terminal renames itself, so
/// the rules get to run again the moment `claude` starts rather than on the next
/// refocus. Only the focused window's rename counts — [`FocusWatch`] tracks
/// which address that is, from `activewindowv2`, so a background window's
/// spinner costs one string compare.
fn update_modes(
    modes: &mut ModeEngine,
    config: &Config,
    hypr: &Hypr,
    watch: &mut FocusWatch,
    ev: HyprEvent,
) -> bool {
    match ev {
        HyprEvent::ActiveWindow { class, title, pid } => {
            let mut focus = crate::mode::Focus { class, title, pid, fullscreen: false };
            if config.needs_focus_pid() {
                match hypr.active_window() {
                    Ok(info) => {
                        focus.pid = focus.pid.or(info.pid);
                        focus.fullscreen = info.fullscreen;
                    }
                    Err(e) => eprintln!("warning: could not read the focused window's pid: {e}"),
                }
            }
            // A fresh window means a fresh tree; the resolve below walks it, so
            // the sweep starts its interval from here.
            watch.arm_rescan(config);
            // One complete context, one re-resolve — never a half-updated one.
            modes.context_changed(config, focus)
        }
        HyprEvent::ActiveWindowV2 { address } => {
            watch.address = address;
            false
        }
        HyprEvent::WindowTitle { address, title } => {
            let title = match watch.title_update(config, &address, title) {
                TitleUpdate::Ignore => return false,
                TitleUpdate::Title(t) => t,
                TitleUpdate::Query => match hypr.active_window() {
                    Ok(info) => info.title,
                    Err(e) => {
                        eprintln!("warning: could not read the focused window's title: {e}");
                        return false;
                    }
                },
            };
            // Only a *real* rename re-walks the tree, so only a real rename
            // resets the sweep's clock — a compositor that re-announced the
            // title it already sent must not be able to starve it.
            if modes.focus_title() != title {
                watch.arm_rescan(config);
            }
            modes.title_changed(config, &title)
        }
        HyprEvent::Fullscreen(on) => modes.set_fullscreen(config, on),
        // An overlay came up or went away. No compositor round trip: the
        // namespace is the whole event, and the engine keeps the set.
        HyprEvent::Layer { namespace, open } => modes.layer_changed(config, &namespace, open),
        // The session locked or unlocked. Synthesized by `hypr::watch_locked`
        // today and read off the wire if the fork ever emits it; either way it
        // is an ordinary context change from here on, and a restatement of the
        // state we hold costs one comparison.
        HyprEvent::Locked(on) => modes.lock_changed(config, on),
        _ => false,
    }
}

/// The instant the focused window's process tree is next due to be re-walked,
/// or `None` when the periodic sweep is off for this config and context.
///
/// Off is the common case and every test of it is free: `process_rescan_ms = 0`,
/// a config no predicate of which mentions `process_tree_has`, or no focused pid
/// to walk from.
fn process_rescan_due(config: &Config, modes: &ModeEngine, watch: &FocusWatch) -> Option<Instant> {
    (config.process_rescan_ms() > 0 && modes.process_rescan_useful(config))
        .then_some(watch.next_rescan)
}

/// Handle one recognized gesture. Returns whether it moved the active mode
/// (via [`Action::SetMode`] / [`Action::ClearMode`]).
///
/// A chord that resolves to a key or mouse button ([`Action::Key`]) is not
/// performed but **held** ([`ChordKeys`]): pressed here, on the keyboard or
/// the pointer by code, and released when the chord button lifts or the guide
/// does. Those two releases are handled first and gated by nothing — an output
/// that was pressed gets released whatever mode or overlay we are in now.
#[allow(clippy::too_many_arguments)]
fn handle_gesture(
    hypr: &Hypr,
    config: &Config,
    modes: &mut ModeEngine,
    osk: &mut OskHandle,
    // Which controller is driving. Read only by the keyboard toggle, which
    // means something different on a pad with no trackpads ([`osk::presentation_for`]).
    source: report::Source,
    hx: &mut HapticCtx,
    chords: &mut ChordKeys,
    keyboard: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    // The forwarding gate as it stands at this gesture — the same
    // [`gamepad_forwarding`] answer the frame's input rides on. Read by the
    // guide's release alone, to decide whether a bare tap is Steam's or a
    // binding's.
    forwarding: bool,
    ge: GestureEvent,
) -> bool {
    match ge {
        // The other edge of a held chord output. Not a binding: nothing
        // resolves against it, and a button holding nothing is nothing.
        GestureEvent::GuideChordRelease(b) => {
            if let Some(chord) = chords.release(b) {
                emit_chord(keyboard, pointer, &chord, false);
            }
            return false;
        }
        // The guide's release ends every chord at once, chorded or not (a
        // chord button still down is released by the leave, never by a
        // `GuideChordRelease` of its own).
        GestureEvent::GuideLeave { .. } => chords.release_all(keyboard, pointer.as_deref_mut()),
        _ => {}
    }

    // The guide's release. It binds something only when it was a *narrow* bare
    // tap that no game is about to be handed instead — the whole rule is in
    // `guide_tap_binding`, and both halves matter here: a scrub session's
    // release and a three-second deliberation are unchorded leaves that must
    // summon nothing, and in a game the tap is Steam's (`guide_tap_pulse`
    // replays it onto the sink this same frame), so no binding runs there.
    if let GestureEvent::GuideLeave { bare_tap, .. } = ge {
        if !guide_tap_binding(config.gamepad(), forwarding, bare_tap) {
            return false;
        }
    }

    // The modality gate. On the built-in path this is the old rule verbatim —
    // guide-held gestures drive the desktop even over a game, because that is
    // the whole point of a global modifier. With declared modes it is *this
    // binding's* guard, so a config can retire individual chords in a mode
    // while the rest survive.
    if !modes.allows_gesture(config, &ge, is_guide_scoped(&ge)) {
        return false;
    }
    let action = config.resolve_in(&ge, modes.state());
    if matches!(action, Action::None) {
        return false;
    }
    // While the keyboard is up it owns the pads: suppress every desktop gesture
    // except the keyboard toggle itself, so the toggle chord can still dismiss.
    if osk.is_active() && !matches!(action, Action::ToggleKeyboard { .. }) {
        return false;
    }

    // A key or mouse button on a chord is held, not fired: press it now and
    // let `GuideChordRelease` / `GuideLeave` above let it go — the chord-side
    // twin of a bare `Hold` binding, down to the feedback: the press-edge tick
    // under the hand that pressed it ([`Haptic::Button`]), not the chord buzz,
    // because to the hand this is a button going down, and the pad click that
    // is its usual home already has its own. On a flick or the guide hold a
    // key still does nothing: there is no release edge to pair it with.
    if let (GestureEvent::GuideChord(b), Action::Key(chord)) = (ge, &action) {
        hx.fire(Haptic::Button, button_pad(b));
        let mut clicked = false;
        for (chord, pressed) in chords.press(b, *chord) {
            clicked |= emit_chord(keyboard, pointer.as_deref_mut(), &chord, pressed);
        }
        // A click under the guide is a click: a transient mode that exits on
        // one goes here too, exactly as it does for the bare pad click.
        return clicked && modes.note_click(config);
    }

    // Past every gate: the gesture *landed*. Buzz both actuators before acting,
    // so the confirmation is felt at the moment of recognition rather than after
    // the compositor has answered. This is the only feedback the guide layer
    // gives — nothing else about a chord is visible or audible.
    hx.fire(Haptic::Gesture, HapticPad::Both);

    perform_action(hypr, config, modes, osk, source, keyboard, pointer, &action)
}

/// Perform one resolved action: the tail every binding shares once its own
/// gates have passed, whether it resolved from a guide chord
/// ([`handle_gesture`]) or a bare button's press edge ([`fire_buttons`]) — one
/// function, so the two can never mean subtly different things. Returns
/// whether it moved the active mode (via [`Action::SetMode`] /
/// [`Action::ClearMode`]), which the caller answers with the mode handoff.
#[allow(clippy::too_many_arguments)]
fn perform_action(
    hypr: &Hypr,
    config: &Config,
    modes: &mut ModeEngine,
    osk: &mut OskHandle,
    // Which controller is driving; see [`handle_gesture`].
    source: report::Source,
    keyboard: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    action: &Action,
) -> bool {
    // A sequence is its steps through this same function, in order: one press,
    // several things, each meaning what it means alone. The "the mode moved"
    // answer is the OR — `h.seq { h.key "f", h.set_mode "hints" }` moved it,
    // whichever step did — and a `set_mode` applies where it stands, so a
    // later step runs in the new mode.
    if let Action::Seq(steps) = action {
        let mut moved = false;
        for step in steps {
            moved |= match step {
                // The one difference a sequence makes: a key is TAPPED, not
                // held. A held output needs a release edge to pair with, and
                // by the time the button lifts the sequence is long over.
                Action::Key(chord) => {
                    tap_chord(keyboard, pointer.as_deref_mut(), chord);
                    false
                }
                other => perform_action(
                    hypr,
                    config,
                    modes,
                    osk,
                    source,
                    keyboard,
                    pointer.as_deref_mut(),
                    other,
                ),
            };
        }
        return moved;
    }

    // The keyboard toggle drives the OSK child, not Hyprland: flip show/hide.
    if let Action::ToggleKeyboard { mode, reflow, padless } = action {
        toggle_keyboard(osk, source, osk::presentation_for(source, *mode, *reflow, *padless));
        return false;
    }

    // The manual override drives the mode engine, not Hyprland. Top of the
    // resolution precedence (docs/13 decision #4), so this wins over whatever
    // the focus rules say until it is cleared.
    match action {
        Action::SetMode(name) => return modes.set_mode(config, name),
        Action::ClearMode => return modes.clear_mode(config),
        _ => {}
    }

    // The mode as it is at the moment the binding fires: an `exec` that
    // summons an overlay is about to change it.
    if let Err(e) = execute(hypr, action, modes.state().active()) {
        eprintln!("dispatch failed: {e}");
    }
    false
}

/// Flip the on-screen keyboard: show it (in `mode`, with the binding's chosen
/// presentation — overlay floats over the desktop, reflow displaces content)
/// when hidden, hide it when shown.
fn toggle_keyboard(osk: &mut OskHandle, source: report::Source, present: OskPresentation) {
    if osk.is_active() {
        osk.hide();
    } else {
        osk.show(present.mode, present.reflow);
        if !source.has_pads() {
            // This keyboard will be driven by a highlight rather than a
            // cursor, so arm it: a starting key is lit before the user has
            // pushed anything. A `nav` sent to a keyboard that has not been
            // navigated yet lands on its start key without moving, which is
            // exactly what arming means. A pad source sends none of this and
            // its keyboard comes up with no highlight, as it always has.
            osk.nav(OskNav::Home);
        }
    }
}

/// Cross-frame state for OSK-active routing: a smoothing damper plus the last
/// sent (rounded) position for each pad, so identical positions aren't re-sent
/// every 4 ms frame; and the keyboard layer's own press-edge memory, so a
/// button held across its rise or fall never leaks (module docs, "Layer
/// transitions").
struct OskRoute {
    left: OskPadState,
    right: OskPadState,
    /// Whether the keyboard's layer routed the previous frame. The first frame
    /// after a show acts on no button: whatever is down then — the Y of the
    /// `guide+Y` that raised it — waits for a fresh press.
    live: bool,
    /// The buttons currently holding Shift (`osk shift`), in press order:
    /// Shift goes down with the first and comes up with the last.
    shift_holders: Vec<report::Button>,
    /// The D-pad's snap-navigation clock. Only ever fed on a controller with no
    /// trackpads: there, the keyboard is steered by a highlight and the D-pad
    /// is what moves it. On the Steam Controller the D-pad under the keyboard
    /// does what it always did — nothing, unless a config binds it.
    ///
    /// Unlike the sticks', this one is paced by the controller's **own report
    /// stream** rather than by the loop's stick deadline, because a D-pad
    /// direction is frame state and this is the frame path. The press edge
    /// therefore always steps; the repeat runs for as long as frames keep
    /// arriving, and a pad that goes quiet with a direction held simply steps
    /// once — the console minimum, and what the sticks are there for.
    dpad_nav: NavRepeat,
}

/// One OSK pad's routing state: its smoothing damper and the last position sent
/// on the wire (rounded to the wire's precision so a settled finger stops
/// re-sending). `None` means untouched, so a re-touch always re-sends.
struct OskPadState {
    damper: PadDamper,
    last_sent: Option<(f32, f32)>,
}

/// What one frame of the keyboard's layer asks the daemon to do, decided by
/// [`OskRoute::step`] and carried out by [`route_osk`] — split so the edge and
/// hold logic is unit-testable without a keyboard child or a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OskOp {
    /// Tap a key — with any modifiers held around it — through the keyboard,
    /// with the hand the button is under (for the commit click).
    Key(KeyChord, HapticPad),
    /// Type the key under this pad's cursor.
    Commit(OskPad),
    /// Move the snap-navigation highlight one key (the padless modality).
    Nav(OskNav),
    /// The first Shift-bound button went down.
    ShiftDown,
    /// The last Shift-bound button came up (or the keyboard is closing).
    ShiftUp,
    /// Accept the keyboard's highlighted word suggestion (R1).
    CandidateAccept,
    /// Move the suggestion strip's highlight along (L1).
    CandidateNext,
    /// Close the keyboard.
    Dismiss,
}

impl OskRoute {
    fn new(cfg: &CursorConfig) -> OskRoute {
        OskRoute {
            left: OskPadState::new(cfg),
            right: OskPadState::new(cfg),
            live: false,
            shift_holders: Vec::new(),
            dpad_nav: NavRepeat::default(),
        }
    }

    /// Forget both pads' smoothing state (a mode handoff, a disconnect). The
    /// layer's press-edge memory and its Shift holders survive: the keyboard
    /// is still up, and a trigger still pulled is still holding Shift.
    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }

    /// The keyboard went down: forget everything, so the next show is a fresh
    /// layer that acts on nothing until a frame has passed. Called on every
    /// frame the keyboard is not up, so it is a no-op once disarmed.
    fn disarm(&mut self) {
        if !self.live {
            return;
        }
        self.reset();
        self.live = false;
        self.shift_holders.clear();
        self.dpad_nav.reset();
    }

    /// Decide one frame of the keyboard's button layer. Pure: what to send,
    /// from what changed between `prev` and `frame` under `bindings` (the
    /// layered map, [`crate::config::Config::osk_buttons_in`]).
    ///
    /// Releases are honoured on every frame — Shift comes up when the last
    /// button holding it lifts, whatever else is going on. Presses are
    /// honoured only on a press edge, and only once this layer routed the
    /// previous frame too: the frame the keyboard came up on acts on nothing,
    /// so the button that raised it is not also its first keystroke. A
    /// `Dismiss` ends the frame's processing (and this layer): any held Shift
    /// is released first, and the route is disarmed for the next show.
    fn step(
        &mut self,
        frame: &report::Frame,
        prev: &report::Frame,
        bindings: &HashMap<report::Button, OskAction>,
        now: Instant,
    ) -> Vec<OskOp> {
        let mut ops = Vec::new();
        let settled = self.live;
        self.live = true;

        let was_holding = !self.shift_holders.is_empty();
        self.shift_holders.retain(|&b| frame.pressed(b));
        if was_holding && self.shift_holders.is_empty() {
            ops.push(OskOp::ShiftUp);
        }
        if !settled {
            return ops;
        }

        // Snap navigation, on a controller with no trackpads only. The D-pad
        // moves the highlight — it is the one control every console keyboard
        // uses for exactly this — with the same repeat clock the sticks get, so
        // holding a direction walks the grid. On a pad source none of this
        // runs: the D-pad keeps doing what it did (nothing, unless bound), and
        // the two cursors keep being the way to point at a key.
        if !frame.source.has_pads() {
            if let Some(dir) = self.dpad_nav.held(dpad_direction(frame), now) {
                ops.push(OskOp::Nav(dir));
            }
        }

        for b in frame.edges_down(prev) {
            // A config entry, else the padless default for this button.
            match bindings.get(&b).copied().or_else(|| padless_osk_default(frame.source, b)) {
                Some(OskAction::Key(chord)) => ops.push(OskOp::Key(chord, button_pad(b))),
                Some(OskAction::Commit) => ops.push(OskOp::Commit(commit_pad(b))),
                Some(OskAction::Shift) => {
                    if self.shift_holders.is_empty() {
                        ops.push(OskOp::ShiftDown);
                    }
                    self.shift_holders.push(b);
                }
                Some(OskAction::CandidateAccept) => ops.push(OskOp::CandidateAccept),
                Some(OskAction::CandidateNext) => ops.push(OskOp::CandidateNext),
                Some(OskAction::Dismiss) => {
                    if !self.shift_holders.is_empty() {
                        ops.push(OskOp::ShiftUp);
                    }
                    ops.push(OskOp::Dismiss);
                    self.disarm();
                    break;
                }
                Some(OskAction::None) | None => {}
            }
        }
        ops
    }
}

/// The snap-navigation direction the D-pad is currently asking for, or `None`
/// when it is neutral. A diagonal resolves to one direction (the first of
/// up/down/left/right that is down) rather than two, so a hat rolling through a
/// corner steps once per direction rather than lurching diagonally.
fn dpad_direction(frame: &report::Frame) -> Option<OskNav> {
    use report::Button::*;
    for (btn, dir) in [
        (DpadUp, OskNav::Up),
        (DpadDown, OskNav::Down),
        (DpadLeft, OskNav::Left),
        (DpadRight, OskNav::Right),
    ] {
        if frame.pressed(btn) {
            return Some(dir);
        }
    }
    None
}

/// What a button does under the keyboard on a controller with **no trackpads**,
/// when no config entry claims it.
///
/// The built-in map ([`crate::config::osk_builtins`]) puts the commit on the two
/// pad *clicks*, which this controller does not have — so out of the box its
/// keyboard would have a highlight and no way to type it. `A` is where every
/// console puts "select", and it reads the right-hand cursor like the rest of
/// the face cluster ([`commit_pad`]); with no pad cursor to read, the keyboard
/// commits the highlight instead. A config entry on `A` still wins, and a pad
/// source never reaches here at all.
fn padless_osk_default(source: report::Source, b: report::Button) -> Option<OskAction> {
    if source.has_pads() {
        return None;
    }
    crate::config::padless_osk_builtins().find(|(btn, _)| *btn == b).map(|(_, what)| what)
}

/// Which pad's cursor an `osk commit` on `b` types under: the pad on the same
/// side of the controller as the button ([`button_pad`]). The pad clicks are the
/// obvious case; a commit rebound to a grip or a bumper reads the pad under
/// that hand, and one on a face button the right pad. The guide button belongs
/// to neither hand and reads the right pad, like the face cluster.
fn commit_pad(b: report::Button) -> OskPad {
    match button_pad(b) {
        HapticPad::Left => OskPad::Left,
        HapticPad::Right | HapticPad::Both => OskPad::Right,
    }
}

impl OskPadState {
    fn new(cfg: &CursorConfig) -> OskPadState {
        OskPadState { damper: damper_from(cfg), last_sent: None }
    }

    fn reset(&mut self) {
        self.damper.reset();
        self.last_sent = None;
    }
}

/// Route one frame to the on-screen keyboard while it owns the pads: each
/// pad's absolute position becomes a `cursor L|R`, and the buttons do what the
/// keyboard's table says ([`crate::config::OskAction`] — by default the Deck's
/// map: pad clicks commit the key under that pad's cursor, L2 holds Shift, R2
/// is Enter, Y Space, X Backspace, and B or Menu close it). The decisions are
/// [`OskRoute::step`]'s; this carries them out.
///
/// Every commit — a pad click or a key typed through the keyboard — gets a
/// haptic click on its side ([`Haptic::Commit`]). The lighter per-key-crossing
/// tick is *not* fired here: only the OSK child knows where the key boundaries
/// are, so it reports crossings back and the loop answers them (`Input::Osk`).
fn route_osk(
    osk: &mut OskHandle,
    frame: &report::Frame,
    prev: &report::Frame,
    st: &mut OskRoute,
    osk_buttons: &HashMap<report::Button, OskAction>,
    hx: &mut HapticCtx,
    now: Instant,
) {
    use report::Button::*;

    // Move each pad's cursor first, so a click on the same frame commits the key
    // actually under the finger. Only while touched, and only on change.
    route_pad(osk, OskPad::Left, frame.pressed(PadLeftTouch), frame.left_pad, &mut st.left, now);
    route_pad(osk, OskPad::Right, frame.pressed(PadRightTouch), frame.right_pad, &mut st.right, now);

    // Commit on click-down (the Deck commits on click, not release).
    for op in st.step(frame, prev, osk_buttons, now) {
        match op {
            OskOp::Key(chord, hand) => {
                osk.key(&chord);
                hx.fire(Haptic::Commit, hand);
            }
            OskOp::Commit(pad) => {
                osk.commit(pad);
                hx.fire(Haptic::Commit, haptic_pad(pad));
            }
            // No pulse: a highlight moving between keys is the padless
            // equivalent of a cursor crossing one, and the keyboard's own
            // `event crossed` is what the crossing tick is wired to. Every
            // controller that navigates by highlight has rumble motors rather
            // than actuators, where `haptic_for` drops the pulse anyway.
            OskOp::Nav(dir) => osk.nav(dir),
            OskOp::ShiftDown => osk.hold_shift(true),
            OskOp::ShiftUp => osk.hold_shift(false),
            // Accepting a suggestion types a whole word, so it earns the same
            // commit pulse a key does; moving the highlight is a lighter
            // change, and the keyboard reports it back as
            // `event candidate <n> <text>` for a tick of its own.
            OskOp::CandidateAccept => {
                osk.candidate_accept();
                hx.fire(Haptic::Commit, HapticPad::Right);
            }
            OskOp::CandidateNext => osk.candidate_next(),
            OskOp::Dismiss => osk.hide(),
        }
    }
}

/// Forward one pad's *smoothed* position to the OSK, rate-limited to real
/// movement. The pad is smoothed by the same [`PadDamper`] as the desktop cursor
/// (One Euro + hysteresis), so a held-still finger keeps the on-screen cursor
/// steady too. Resets the damper on lift so a lift-and-retouch starts clean.
fn route_pad(
    osk: &mut OskHandle,
    pad: OskPad,
    touched: bool,
    p: report::Pad,
    st: &mut OskPadState,
    now: Instant,
) {
    if !touched {
        st.reset();
        return;
    }
    // The smoothed position is already normalized to [-1, 1]. Pad +Y is up and
    // the OSK's +ny is up too, so both axes pass straight through with no flip.
    let (nx, ny) = st.damper.update(p.x, p.y, now);
    // Dedup at the wire's precision (`cursor` serializes at %.4f), so a settled
    // finger — whose smoothed position stops changing — stops re-sending.
    let sent = (round_wire(nx), round_wire(ny));
    if st.last_sent == Some(sent) {
        return;
    }
    st.last_sent = Some(sent);
    osk.cursor(pad, sent.0, sent.1);
}

/// Round a normalized axis to the OSK `cursor` command's wire precision (4
/// decimals), as `f32`. Used both to dedup and as the value actually sent, so
/// the dedup key and the wire value never disagree.
fn round_wire(v: f64) -> f32 {
    ((v * 10_000.0).round() / 10_000.0) as f32
}

/// Guide-scoped gestures carry the guide modifier and are always honored.
fn is_guide_scoped(ge: &GestureEvent) -> bool {
    matches!(
        ge,
        GestureEvent::GuideChord(_)
            | GestureEvent::GuideChordRelease(_)
            | GestureEvent::GuideStickFlick { .. }
            | GestureEvent::GuideHold
            | GestureEvent::GuideEnter
            | GestureEvent::GuideLeave { .. }
    )
}

/// Perform one resolved action. `mode` is the mode the gesture fired in, handed
/// to an `exec` child as `HYPRPAD_MODE`: a command that summons an overlay
/// changes the mode by being launched (the cheat sheet's own layer selects
/// `cheatsheet`), so the mode it wants to know about is the one it can no
/// longer read for itself.
fn execute(hypr: &Hypr, action: &Action, mode: &str) -> std::io::Result<()> {
    match action {
        // Both arms hand the target to `hypr`, which owns the one mapping from
        // a `WorkspaceTarget` to a Hyprland selector — so `focus` and `move`
        // can never disagree about what `h.workspace "emptyn"` means.
        Action::Workspace(t) => hypr.workspace(t),
        Action::MoveWindowToWorkspace(t) => hypr.move_window_to_workspace(t),
        Action::ToggleFullscreen => hypr.toggle_fullscreen(),
        Action::Exec(cmd) => {
            hypr.spawn_env(cmd, &[("HYPRPAD_MODE", mode)]);
            Ok(())
        }
        Action::Dispatch(payload) => hypr.dispatch_raw(payload).map(|_| ()),
        // Handled in `perform_action` against the OSK handle, never reaches here.
        Action::ToggleKeyboard { .. } => Ok(()),
        // A key is never dispatched: a bare button's is held by
        // `drive_buttons`, a chord's by `handle_gesture` (which never gets
        // here with one). What does arrive is a key on a flick or the guide
        // hold, which has no release edge to pair with and so does nothing.
        Action::Key(_) => Ok(()),
        // Handled in `perform_action` against the mode engine, never here.
        Action::SetMode(_) | Action::ClearMode => Ok(()),
        // Turn the controller off: `0x9F` through the same feature-report path
        // lizard mode uses. A failure here is almost always "the controller is
        // already asleep" (every controller node STALLs), which for this action is
        // the outcome asked for — so it is logged, not propagated as an IPC
        // error, and never takes the daemon down.
        Action::ControllerOff => {
            controller_off();
            Ok(())
        }
        // Unrolled into its steps by `perform_action`, each of which arrives
        // here on its own; a whole sequence never does.
        Action::Seq(_) => Ok(()),
        Action::None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkspaceTarget;

    /// A `config.lua` whose rule walks the process tree, so the periodic
    /// rescan has something to be armed for.
    fn walking_config(daemon: &str) -> Config {
        crate::lua_config::load_str(
            &format!(
                r#"
                hyprpad.daemon {{ {daemon} }}
                hyprpad.mode("claude").when(function(ctx)
                  return ctx.focus:process_tree_has("nothing-by-that-name")
                end)
                hyprpad.mode("desktop")
                "#
            ),
            "test.lua",
        )
        .expect("config should load")
    }

    /// The focused-window check: which `windowtitle` line is ours, and which of
    /// the compositor's two lines per rename to answer.
    #[test]
    fn a_title_event_is_answered_only_for_the_focused_window() {
        let c = walking_config("");
        let mut w = FocusWatch::new();
        w.address = "0xfocused".to_string();

        // The v2 form for the focused window carries the new title.
        assert_eq!(
            w.title_update(&c, "0xfocused", Some("claude — foot".into())),
            TitleUpdate::Title("claude — foot".into())
        );
        // Another window's spinner is not our business, in either form.
        assert_eq!(w.title_update(&c, "0xother", Some("⠙ build".into())), TitleUpdate::Ignore);
        assert_eq!(w.title_update(&c, "0xother", None), TitleUpdate::Ignore);
        // Nor is the bare line for the focused window, now that a v2 has proved
        // this compositor sends both per rename.
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Ignore);

        // Before the session's first focus change we do not know which window
        // holds focus, so no rename is attributable.
        let mut fresh = FocusWatch::new();
        assert_eq!(fresh.title_update(&c, "0xfocused", Some("x".into())), TitleUpdate::Ignore);
    }

    #[test]
    fn the_bare_title_form_is_queried_until_a_v2_form_proves_it_redundant() {
        let c = walking_config("");
        let mut w = FocusWatch::new();
        w.address = "0xfocused".to_string();
        // A compositor that has only ever sent the title-less form: ask it.
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Query);
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Query);
        // One v2 line settles it for the rest of the session.
        w.title_update(&c, "0xfocused", Some("t".into()));
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Ignore);
    }

    #[test]
    fn rescan_on_title_change_false_suppresses_the_title_path() {
        let c = walking_config("rescan_on_title_change = false");
        let mut w = FocusWatch::new();
        w.address = "0xfocused".to_string();
        assert_eq!(w.title_update(&c, "0xfocused", Some("claude".into())), TitleUpdate::Ignore);
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Ignore);
    }

    #[test]
    fn a_config_with_no_modes_never_takes_the_title_path() {
        // Every `config.toml`: the built-in path selects on class alone, so a
        // rename cannot change its answer and must not cost a query.
        let c = Config::load_default();
        let mut w = FocusWatch::new();
        w.address = "0xfocused".to_string();
        assert_eq!(w.title_update(&c, "0xfocused", Some("claude".into())), TitleUpdate::Ignore);
        assert_eq!(w.title_update(&c, "0xfocused", None), TitleUpdate::Ignore);
    }

    #[test]
    fn the_process_rescan_timer_is_armed_only_when_a_walk_could_matter() {
        let c = walking_config("");
        let mut modes = ModeEngine::new(&c);
        let mut w = FocusWatch::new();
        assert!(process_rescan_due(&c, &modes, &w).is_none(), "no focused pid yet");

        modes.focus_changed(&c, "foot", "shell", Some(std::process::id() as i32));
        assert!(process_rescan_due(&c, &modes, &w).is_some());

        // Arming pushes the deadline a full interval out — the rate limit, and
        // the only thing that moves it, so an event burst cannot bring it
        // forward.
        w.arm_rescan(&c);
        let due = process_rescan_due(&c, &modes, &w).expect("armed");
        assert!(due > Instant::now());
        assert!(due <= Instant::now() + Duration::from_millis(c.process_rescan_ms()));

        // `process_rescan_ms = 0` switches the sweep off outright...
        let off = walking_config("process_rescan_ms = 0");
        let mut modes = ModeEngine::new(&off);
        modes.focus_changed(&off, "foot", "shell", Some(std::process::id() as i32));
        assert!(process_rescan_due(&off, &modes, &w).is_none());

        // ...and so does a config no rule of which walks the tree, however
        // eagerly it is configured.
        let title_only = crate::lua_config::load_str(
            r#"
            hyprpad.daemon { process_rescan_ms = 50 }
            hyprpad.mode("claude").when(function(ctx)
              return ctx.focus.title:match("claude") ~= nil
            end)
            hyprpad.mode("desktop")
            "#,
            "test.lua",
        )
        .unwrap();
        let mut modes = ModeEngine::new(&title_only);
        modes.focus_changed(&title_only, "foot", "shell", Some(std::process::id() as i32));
        assert!(process_rescan_due(&title_only, &modes, &w).is_none());
    }

    #[test]
    fn swipe_maps_vertical_delta_to_scroll() {
        // Non-natural: finger up (Δny > 0) scrolls up (dy < 0), no horizontal.
        let (dx, dy) = swipe_scroll(0.0, 0.1, 1.0, false, false);
        assert_eq!(dx, 0.0);
        assert!(dy < 0.0, "finger up should scroll up (dy < 0), got {dy}");
        assert!((dy - (-0.1 * SWIPE_SCROLL_REF)).abs() < 1e-9);
        // Finger down (Δny < 0) scrolls down (dy > 0).
        let (_, dy_down) = swipe_scroll(0.0, -0.1, 1.0, false, false);
        assert!(dy_down > 0.0);
        // Sensitivity scales the magnitude linearly.
        let (_, dy2) = swipe_scroll(0.0, 0.1, 2.0, false, false);
        assert!((dy2 - 2.0 * dy).abs() < 1e-9);
        // Zero delta (held-still finger, after hysteresis) produces zero scroll.
        assert_eq!(swipe_scroll(0.0, 0.0, 1.0, false, false), (0.0, 0.0));
    }

    #[test]
    fn swipe_natural_inverts_vertical() {
        let (_, dy) = swipe_scroll(0.0, 0.1, 1.0, false, false);
        let (_, dy_nat) = swipe_scroll(0.0, 0.1, 1.0, true, false);
        assert!((dy_nat + dy).abs() < 1e-9, "natural must negate dy");
    }

    #[test]
    fn swipe_horizontal_gated_and_inverts() {
        // Horizontal motion is ignored unless enabled.
        let (dx_off, _) = swipe_scroll(0.2, 0.0, 1.0, false, false);
        assert_eq!(dx_off, 0.0);
        // Enabled, non-natural: finger right (Δnx > 0) scrolls right (dx > 0).
        let (dx_on, _) = swipe_scroll(0.2, 0.0, 1.0, false, true);
        assert!(dx_on > 0.0);
        assert!((dx_on - 0.2 * SWIPE_SCROLL_REF).abs() < 1e-9);
        // Natural inverts horizontal too.
        let (dx_nat, _) = swipe_scroll(0.2, 0.0, 1.0, true, true);
        assert!((dx_nat + dx_on).abs() < 1e-9);
    }

    #[test]
    fn circular_clockwise_scrolls_down_ccw_up() {
        // One clockwise tick (negative) scrolls down (dy > 0) at the defaults.
        let (dx, dy) = circular_scroll(-1, 1.0, 15.0, false);
        assert_eq!(dx, 0.0);
        assert!(dy > 0.0, "clockwise should scroll down (dy > 0), got {dy}");
        assert!((dy - 15.0).abs() < 1e-9, "one 15° tick at sens 1.0 = 15 units");
        // Counter-clockwise (positive) scrolls up.
        let (_, dy_up) = circular_scroll(1, 1.0, 15.0, false);
        assert!(dy_up < 0.0);
        // Natural inverts the direction.
        let (_, dy_nat) = circular_scroll(-1, 1.0, 15.0, true);
        assert!(dy_nat < 0.0);
        // Tick count and sensitivity scale the magnitude.
        let (_, dy3) = circular_scroll(-3, 1.0, 15.0, false);
        assert!((dy3 - 45.0).abs() < 1e-9);
        let (_, dy_sens) = circular_scroll(-1, 2.0, 15.0, false);
        assert!((dy_sens - 30.0).abs() < 1e-9);
    }

    fn report(data: Vec<u8>) -> hidraw::Report {
        report_from(hidraw::Transport::Dongle, data)
    }

    fn report_from(transport: hidraw::Transport, data: Vec<u8>) -> hidraw::Report {
        let node = match transport {
            hidraw::Transport::Dongle => "/dev/hidraw0",
            hidraw::Transport::Bluetooth => "/dev/hidraw13",
        };
        hidraw::Report { node: PathBuf::from(node), transport, data }
    }

    /// The 46-byte `0x45` the Bluetooth link streams, with the same `A` press
    /// as [`raw_report_with_a`], so the two transports can be compared.
    fn raw_bt_report_with_a() -> Vec<u8> {
        let mut raw = vec![0u8; 46];
        raw[0] = 0x45;
        raw[1] = 0x07;
        raw[2] = 0x01;
        raw
    }

    /// A raw 54-byte `0x42` with `A` down, so the forwarder — which now
    /// decodes, rather than handing bytes to the loop — has something real to
    /// decode.
    fn raw_report_with_a() -> Vec<u8> {
        let mut raw = vec![0u8; 54];
        raw[0] = 0x42;
        raw[1] = 0x07; // counter
        raw[2] = 0x01; // byte 2 bit 0 = A
        raw
    }

    /// The two-transport machine, end to end through the forwarder: the dongle
    /// streams, the controller switches to Bluetooth, and it switches back.
    ///
    /// Three things are pinned. The transport is announced **once** per change
    /// and not once per frame — 134 reports a second on a link would otherwise
    /// rewrite `status.json` 134 times a second. A `0x45` decodes to the same
    /// frame as the `0x42` around it, so nothing above the reader can tell which
    /// wire it came off. And **no `ReadersEnded`** is emitted anywhere in the
    /// middle: a transport switch is not a disconnect, and if it produced one
    /// the daemon would drop every held key and reset the cursor mid-use.
    #[test]
    fn a_transport_switch_is_announced_once_and_is_not_a_reconnect() {
        use hidraw::Transport::{Bluetooth, Dongle};

        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        for _ in 0..3 {
            rtx.send(report_from(Dongle, raw_report_with_a())).unwrap();
        }
        // The controller moved to Bluetooth: the dongle's nodes are still open
        // and simply stop producing.
        for _ in 0..3 {
            rtx.send(report_from(Bluetooth, raw_bt_report_with_a())).unwrap();
        }
        // …and back to the dongle.
        rtx.send(report_from(Dongle, raw_report_with_a())).unwrap();
        drop(rtx);
        forward_reports(rrx, itx);

        let got: Vec<Input> = irx.into_iter().collect();

        // Exactly three announcements, in order, and no more.
        let switches: Vec<hidraw::Transport> = got
            .iter()
            .filter_map(|i| match i {
                Input::Transport(t) => Some(*t),
                _ => None,
            })
            .collect();
        assert_eq!(switches, vec![Dongle, Bluetooth, Dongle], "one line per change, not per frame");

        // Every frame arrived, and the Bluetooth ones are indistinguishable
        // from the dongle's once decoded.
        let frames: Vec<&report::Frame> = got
            .iter()
            .filter_map(|i| match i {
                Input::Frame { frame, .. } => Some(frame),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 7);
        assert!(frames.iter().all(|f| f.pressed(report::Button::A) && f.counter == 0x07));
        assert!(frames.windows(2).all(|w| w[0] == w[1]), "the wire does not change the frame");

        // The relay still gets the original bytes, short report and all — it is
        // `controller_to_triton`'s job, not the reader's, to re-frame them.
        let raws: Vec<usize> = got
            .iter()
            .filter_map(|i| match i {
                Input::Frame { raw, .. } => Some(raw.as_ref()?.len()),
                _ => None,
            })
            .collect();
        assert_eq!(raws, vec![54, 54, 54, 46, 46, 46, 54]);

        // The disconnect is announced once, at the end, and nowhere else.
        let ended = got.iter().filter(|i| matches!(i, Input::ReadersEnded)).count();
        assert_eq!(ended, 1, "a switch of transport is never a disconnect");
        assert!(matches!(got.last(), Some(Input::ReadersEnded)));
    }

    /// A generation that only ever sees Bluetooth — the laptop with no dongle,
    /// which is the setup this whole phase is for.
    #[test]
    fn a_bluetooth_only_generation_announces_bluetooth() {
        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        rtx.send(report_from(hidraw::Transport::Bluetooth, raw_bt_report_with_a())).unwrap();
        // The battery report the controller sends unprompted: dropped, and it
        // must not be mistaken for a frame or claim the transport.
        rtx.send(report_from(hidraw::Transport::Bluetooth, vec![0x43; 15])).unwrap();
        drop(rtx);
        forward_reports(rrx, itx);

        let got: Vec<Input> = irx.into_iter().collect();
        assert!(matches!(got[0], Input::Transport(hidraw::Transport::Bluetooth)));
        assert!(matches!(got[1], Input::Frame { .. }));
        assert!(matches!(got[2], Input::ReadersEnded));
        assert_eq!(got.len(), 3, "the 0x43 produced nothing");
    }

    #[test]
    fn forward_reports_decodes_and_emits_readers_ended_when_source_closes() {
        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        rtx.send(report(raw_report_with_a())).unwrap();
        // A report that is not a 0x42 — a battery or status report — is dropped
        // here rather than in the loop. Same effect, one less thing the loop
        // has to know about the controller.
        rtx.send(report(vec![0x03, 0x01])).unwrap();
        drop(rtx); // every reader gone -> the source channel closes

        // Runs to completion: the source is already closed, so it drains the two
        // buffered reports and then announces the readers ended.
        forward_reports(rrx, itx);

        // The first decoded frame of a generation names its transport, ahead of
        // the frame itself: `status.json` must never describe a frame it has
        // not yet said the transport of.
        assert!(
            matches!(irx.recv(), Ok(Input::Transport(hidraw::Transport::Dongle))),
            "the first frame announces its transport"
        );
        let got = irx.recv().expect("a frame");
        let Input::Frame { frame, raw } = got else { panic!("wanted a frame") };
        assert_eq!(frame.source, report::Source::SteamController);
        assert_eq!(frame.counter, 0x07);
        assert!(frame.pressed(report::Button::A));
        assert_eq!(raw.as_deref(), Some(raw_report_with_a().as_slice()), "the relay's bytes");
        assert!(matches!(irx.recv(), Ok(Input::ReadersEnded)), "the undecodable one was dropped");
        // The forwarder dropped its sender, so the loop's channel is now closed.
        //
        // (see `a_transport_switch_is_announced_once_and_is_not_a_reconnect`
        // below for the two-transport half of this contract)
        assert!(irx.recv().is_err());
    }

    #[test]
    fn forward_reports_stops_without_sentinel_when_loop_gone() {
        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        rtx.send(report(raw_report_with_a())).unwrap();
        drop(irx); // main loop is gone: sends fail

        // Returns on the failed send; must not panic and must not try to send a
        // sentinel into the dead channel.
        forward_reports(rrx, itx);
        drop(rtx);
    }

    /// The evdev backend's three events reach the loop in the same shape the
    /// controller's do — and its frames carry no raw bytes, because there are none.
    #[test]
    fn forward_evdev_maps_every_event_and_stops_with_the_loop() {
        let (etx, erx) = mpsc::channel::<crate::evdev::Event>();
        let (itx, irx) = mpsc::channel::<Input>();
        etx.send(crate::evdev::Event::Adopted {
            label: "elite",
            layout: "xbox-elite-2",
            name: "Xbox Wireless Controller".to_string(),
        })
        .unwrap();
        let frame = report::Frame { source: report::Source::Evdev, ..Default::default() };
        etx.send(crate::evdev::Event::Frame(frame)).unwrap();
        etx.send(crate::evdev::Event::Released).unwrap();
        drop(etx);
        forward_evdev(erx, itx);

        assert!(matches!(
            irx.recv(),
            Ok(Input::EvdevAdopted { label: "elite", layout: "xbox-elite-2", .. })
        ));
        let Ok(Input::Frame { frame, raw }) = irx.recv() else { panic!("wanted a frame") };
        assert_eq!(frame.source, report::Source::Evdev);
        assert!(raw.is_none(), "an evdev frame has no controller bytes to relay");
        assert!(matches!(irx.recv(), Ok(Input::EvdevGone)));
        assert!(irx.recv().is_err());

        // And it gives up quietly when the loop is gone, like the other two
        // forwarders.
        let (etx, erx) = mpsc::channel::<crate::evdev::Event>();
        let (itx, irx) = mpsc::channel::<Input>();
        etx.send(crate::evdev::Event::Released).unwrap();
        drop(irx);
        forward_evdev(erx, itx);
        drop(etx);
    }

    // --- the second source -------------------------------------------------

    /// Build an evdev frame the way the daemon really does: through the
    /// backend's own builder, from kernel event codes.
    fn xbox_frame(keys: &[u16], axes: &[(u16, i32)]) -> report::Frame {
        use crate::evdev::*;
        let mut b = FrameBuilder::new(AxisMap::from_parts(&[
            (ABS_X, Axis::LeftX, AbsRange::signed(32767)),
            (ABS_Y, Axis::LeftY, AbsRange::signed(32767)),
            (ABS_RX, Axis::RightX, AbsRange::signed(32767)),
            (ABS_RY, Axis::RightY, AbsRange::signed(32767)),
            (ABS_Z, Axis::TriggerL, AbsRange::unsigned(1023)),
            (ABS_RZ, Axis::TriggerR, AbsRange::unsigned(1023)),
        ]));
        let ev = |kind, code, value| InputEvent { tv_sec: 0, tv_usec: 0, kind, code, value };
        for &k in keys {
            b.apply(&ev(EV_KEY, k, 1));
        }
        for &(c, v) in axes {
            b.apply(&ev(EV_ABS, c, v));
        }
        b.apply(&ev(EV_SYN, SYN_REPORT, 0)).expect("a SYN publishes a frame")
    }

    /// The whole point of the `Source` tag: a frame from the evdev backend goes
    /// through the SAME gesture engine, the SAME bare-button layer and the SAME
    /// config as a controller frame. Nothing above the decoder is source-aware.
    #[test]
    fn a_frame_from_the_evdev_source_resolves_the_same_bindings() {
        use crate::evdev::{BTN_GRIPR, BTN_MODE, BTN_SOUTH};
        use report::Button::*;

        // Xbox button + the upper-right paddle: `guide+r4` on the owner's pad.
        let xbox = xbox_frame(&[BTN_MODE, BTN_GRIPR], &[]);
        let controller = frame_of(&[Steam, GripR4]);
        assert_eq!(xbox.buttons, controller.buttons, "the same bits, from a different wire");
        assert_ne!(xbox.source, controller.source);

        let cfg = Config::from_toml_str(
            "[bindings]\n\"guide+r4\" = \"workspace +1\"\n[buttons]\na = \"key enter\"\n",
        )
        .unwrap();
        let modes = ModeEngine::new(&cfg);

        // The gesture engine recognises the chord on either frame, and the
        // config resolves it to the same action.
        for f in [xbox, controller] {
            let mut engine = GestureEngine::new();
            let events = engine.update(&f, Instant::now());
            assert!(
                events.iter().any(|e| matches!(e, GestureEvent::GuideChord(GripR4))),
                "guide+r4 from {:?}",
                f.source
            );
            let action = cfg.resolve_in(&GestureEvent::GuideChord(GripR4), modes.state());
            assert!(!matches!(action, Action::None), "bound from {:?}", f.source);
        }

        // And the bare-button layer: A presses the same key whichever
        // controller it came from.
        let xbox_a = xbox_frame(&[BTN_SOUTH], &[]);
        let controller_a = frame_of(&[A]);
        let mut from_xbox = live();
        let mut from_controller = live();
        let held_xbox =
            from_xbox.reconcile(modes.buttons(), |b| xbox_a.pressed(b), none, true);
        let held_controller =
            from_controller.reconcile(modes.buttons(), |b| controller_a.pressed(b), none, true);
        assert_eq!(held_xbox, held_controller);
        assert_eq!(held_xbox.len(), 1, "A is bound and held");
    }

    /// The trigger full-pull the evdev backend synthesises is an ordinary
    /// `Button::TriggerR2Full` by the time anything above sees it — which is
    /// what lets the OSK's existing `commit` binding work on a pad with no
    /// trackpad clicks.
    #[test]
    fn a_synthesised_trigger_pull_is_an_ordinary_button_to_the_layers_above() {
        use crate::evdev::ABS_RZ;
        let pulled = xbox_frame(&[], &[(ABS_RZ, 900)]); // 900/1023 = 0.88
        assert!(pulled.pressed(report::Button::TriggerR2Full));
        assert_eq!(commit_pad(report::Button::TriggerR2Full), OskPad::Right);
        assert_eq!(button_pad(report::Button::TriggerR2Full), HapticPad::Right);
    }

    /// The pad-driven handlers go quiet on a padless source without knowing
    /// anything about it: their touch gates simply never open, which is the
    /// "safe by construction" claim the whole design rests on.
    #[test]
    fn the_pad_handlers_idle_on_a_source_with_no_pads() {
        // Everything a hand could be doing on an Xbox pad at once.
        let xbox = xbox_frame(
            &[crate::evdev::BTN_SOUTH, crate::evdev::BTN_TR],
            &[(crate::evdev::ABS_X, 30_000), (crate::evdev::ABS_RZ, 1023)],
        );
        assert!(!xbox.source.has_pads());
        // `drive_cursor` gates on PadRightTouch, `drive_scroll` and the scrub
        // on PadLeftTouch, `route_osk`'s pad router on both. None can open.
        assert!(!xbox.pressed(report::Button::PadLeftTouch));
        assert!(!xbox.pressed(report::Button::PadRightTouch));
        assert!(!xbox.pressed(report::Button::PadLeftClick));
        assert!(!xbox.pressed(report::Button::PadRightClick));
        assert_eq!(xbox.left_pad, report::Pad::default());
        assert_eq!(xbox.right_pad, report::Pad::default());
        // …while the buttons, sticks and triggers are all live.
        assert!(xbox.pressed(report::Button::A) && xbox.pressed(report::Button::BumperR1));
        assert!(xbox.pressed(report::Button::TriggerR2Full));
        assert!(xbox.left_stick.0 > 0);
    }

    /// Every haptic call site is source-agnostic because the drop happens in
    /// exactly one place. A device with no actuators plays nothing, whatever
    /// the config says.
    #[test]
    fn a_source_without_haptics_drops_every_pulse_in_one_place() {
        let cfg = HapticsConfig::default();
        assert!(cfg.enabled, "the test needs the feature on to prove the SOURCE gates it");
        // `Haptic::Button` is off by default, so it would prove nothing here.
        for what in [Haptic::Commit, Haptic::Crossing, Haptic::Scroll, Haptic::Gesture] {
            assert!(
                haptic_for(report::Source::SteamController, &cfg, what).is_some(),
                "{what:?} is enabled in the config"
            );
            assert!(
                haptic_for(report::Source::Evdev, &cfg, what).is_none(),
                "{what:?} has no actuator to play on"
            );
        }
        // The config still has the last word for the controller: turning a trigger
        // point off turns it off.
        let quiet = HapticsConfig { enabled: false, ..cfg };
        assert!(haptic_for(report::Source::SteamController, &quiet, Haptic::Commit).is_none());
    }

    /// The loop's fourth deadline: armed only while a stick is deflected.
    #[test]
    fn the_stick_deadline_arms_only_while_a_stick_is_deflected() {
        let cfg = crate::config::SticksConfig::default();
        let mut sticks = crate::sticks::StickDrive::new();
        let now = Instant::now();

        // A centred Xbox frame: nothing to integrate, so the loop blocks.
        sticks.observe(&xbox_frame(&[], &[]), &cfg, now);
        assert_eq!(sticks.deadline(), None);

        // Right stick over: armed.
        sticks.observe(&xbox_frame(&[], &[(crate::evdev::ABS_RX, 30_000)]), &cfg, now);
        assert!(sticks.deadline().is_some(), "a deflected stick wants a step");

        // Back to centre and stepped: disarmed again.
        sticks.observe(&xbox_frame(&[], &[]), &cfg, now);
        sticks.stepped(&cfg, now);
        assert_eq!(sticks.deadline(), None);

        // A controller frame with a stick held right over — a guide flick in
        // progress — must never arm it: the controller's cursor is its pads.
        let controller = report::Frame { right_stick: (30_000, 0), ..report::Frame::default() };
        sticks.observe(&controller, &cfg, now);
        assert_eq!(sticks.deadline(), None);
    }

    /// The stick layers obey the *existing* guards, so there is no second
    /// permission model: a stick cursor is a cursor.
    #[test]
    fn the_stick_gates_are_the_pad_gates() {
        // With the guide up, the ambient cursor guard decides; with it held,
        // only `h.cursor { guide_in = … }` can keep the sticks.
        assert!(cursor_active(false, true, false), "ambient on, guide up");
        assert!(!cursor_active(true, true, false), "the guide layer takes the sticks");
        assert!(cursor_active(true, false, true), "unless guide_in says otherwise");

        // And the keyboard takes both sticks while it is up, exactly as it
        // takes both pads.
        let gates = StickGates { cursor: true, scroll: true, osk: true };
        assert!(gates.osk);
    }

    /// The startup wait's second exit. A machine with only an Xbox pad is a
    /// perfectly good hyprpad session, so waiting forever for a controller that is
    /// not coming would be the wrong answer — but a pad nothing is going to
    /// read is not a controller being present, so the config knob wins.
    #[test]
    fn the_startup_wait_ends_on_a_gamepad_only_when_the_backend_is_armed() {
        let off = Config::from_toml_str("[device]\nevdev = false\n").unwrap();
        assert!(!gamepad_present(&off), "a backend that is off finds nothing");

        // With the backend on the answer is whatever is actually plugged into
        // the machine running the tests, which is not something to assert —
        // but it must agree with the scan, and must never hang or panic.
        let on = Config::from_toml_str("[device]\nevdev = true\n").unwrap();
        let scanned = crate::evdev::gamepad_nodes().is_ok_and(|n| !n.is_empty());
        assert_eq!(gamepad_present(&on), scanned);
    }

    /// Last-active-source wins, and the one case that would otherwise thrash:
    /// a hand resting on the controller's grips while the other drives an Xbox pad.
    #[test]
    fn only_deliberate_input_takes_the_device_from_the_other_controller() {
        use report::Button::*;
        assert!(report::Frame::default().is_neutral());

        // Proximity is not intent. The capacitive cluster fires on hand
        // contact, so a hand simply on the controller must not claim the cursor —
        // it would fight the other pad at the report rate.
        for cap in [Cap0, Cap1, Cap2, Cap3] {
            assert!(frame_of(&[cap]).is_neutral(), "{cap:?} is a hand, not a press");
        }
        assert!(frame_of(&[Cap0, Cap1, Cap2, Cap3]).is_neutral());

        // A press, a click, a touch, a trigger or a stick is.
        for b in [A, Steam, GripR4, PadLeftClick, PadRightTouch, DpadUp] {
            assert!(!frame_of(&[b]).is_neutral(), "{b:?} is deliberate");
        }
        let pulled = report::Frame { r2: 9_000, ..report::Frame::default() };
        assert!(!pulled.is_neutral(), "a trigger off zero is deliberate");

        // A stick's idle offset is not, but a real push is. The threshold is
        // the gesture engine's own centre, so there is one answer in the
        // daemon to "did the user move a stick".
        let dz = crate::gesture::DEADZONE as i16;
        let idle = report::Frame { left_stick: (dz - 1, 0), ..report::Frame::default() };
        assert!(idle.is_neutral(), "a resting stick is not a command");
        let pushed = report::Frame { right_stick: (0, dz + 1), ..report::Frame::default() };
        assert!(!pushed.is_neutral());
    }

    /// The `"sources"` list, controller first, and only what is actually there.
    #[test]
    fn the_status_source_list_names_every_live_controller() {
        assert_eq!(source_names(true, None), vec!["steam-controller"]);
        assert_eq!(
            source_names(true, Some(("elite", "xbox-elite-2"))),
            vec!["steam-controller", "elite"]
        );
        assert_eq!(source_names(false, Some(("gamepad", "xbox-elite-2"))), vec!["gamepad"]);
        assert!(source_names(false, None).is_empty(), "no daemon-invented sources");
    }

    /// A frame with exactly these buttons down.
    fn frame_of(buttons: &[report::Button]) -> report::Frame {
        let mut bits = 0u32;
        for &b in buttons {
            let bit = (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit");
            bits |= 1 << bit;
        }
        report::Frame { buttons: bits, ..report::Frame::default() }
    }

    /// A `ButtonKeys` whose layer has already been live for a frame, so a press
    /// edge presses — what every test wants unless it is about the transition.
    fn live() -> ButtonKeys {
        ButtonKeys { held: HashMap::new(), open: true }
    }

    /// "Nothing was down" — the `was_pressed` of a fresh press, or the
    /// `pressed` of a frame with everything released.
    fn none(_: report::Button) -> bool {
        false
    }

    #[test]
    fn a_combo_presses_its_modifiers_first_and_releases_them_last() {
        use crate::config::KeyChord;
        // Shift+Tab: the modifier goes down before the key and comes up after
        // it, which is the whole reason a chord is one binding and not two.
        let shift_tab = KeyChord::parse("shift+tab").unwrap();
        assert_eq!(chord_edges(&shift_tab, true), vec![(42, true), (15, true)]);
        assert_eq!(chord_edges(&shift_tab, false), vec![(15, false), (42, false)]);
        // Several modifiers nest: pressed left to right, released right to
        // left, so nothing is ever left down out of order.
        let csa = KeyChord::parse("ctrl+shift+alt+left").unwrap();
        assert_eq!(
            chord_edges(&csa, true),
            vec![(29, true), (42, true), (56, true), (105, true)]
        );
        assert_eq!(
            chord_edges(&csa, false),
            vec![(105, false), (56, false), (42, false), (29, false)]
        );
        // A lone key is the single edge it always was.
        assert_eq!(chord_edges(&KeyChord::plain(103), true), vec![(103, true)]);
        assert_eq!(chord_edges(&KeyChord::plain(103), false), vec![(103, false)]);
        // A combo whose key is a mouse button splits across the two devices —
        // the modifier is a KEY_*, the button a BTN_*, and `route` sorts them.
        let shift_click = KeyChord::parse("shift+btn_left").unwrap();
        assert_eq!(chord_edges(&shift_click, true), vec![(42, true), (0x110, true)]);
        assert_eq!(route(42), Route::Keyboard);
        assert_eq!(route(0x110), Route::Pointer(PointerButton::Left));
    }

    #[test]
    fn a_bare_combo_binding_behaves_like_a_single_key_through_the_latch() {
        // The combo is one held output: pressed on the down edge, released on
        // the up edge, silent while held (the kernel repeats the key with the
        // modifiers still down), and subject to the same stale-press latch a
        // single key is — the rule that keeps a button held across a gate
        // change from pressing again on its own.
        use report::Button::*;
        use crate::config::KeyChord;
        let shift_tab = KeyChord::parse("shift+tab").unwrap();
        let bindings = HashMap::from([(BumperL1, ButtonAction::Hold(shift_tab))]);
        let mut st = live();
        let l1 = |b: report::Button| b == BumperL1;
        assert_eq!(
            st.reconcile(&bindings, l1, none, true),
            vec![(BumperL1, shift_tab, true)]
        );
        assert!(st.reconcile(&bindings, l1, l1, true).is_empty(), "held: no repeat of ours");
        assert_eq!(
            st.reconcile(&bindings, none, l1, true),
            vec![(BumperL1, shift_tab, false)]
        );
        assert!(st.held.is_empty());

        // Gate closes mid-hold: the whole combo is released, once.
        assert_eq!(st.reconcile(&bindings, l1, none, true).len(), 1);
        assert_eq!(
            st.reconcile(&bindings, l1, l1, false),
            vec![(BumperL1, shift_tab, false)]
        );
        // Still down when the gate reopens: nothing, until a fresh press.
        assert!(st.reconcile(&bindings, l1, l1, true).is_empty());
        assert!(st.reconcile(&bindings, l1, l1, true).is_empty());
        assert!(st.reconcile(&bindings, none, l1, true).is_empty());
        assert_eq!(st.reconcile(&bindings, l1, none, true).len(), 1);

        // And rebinding the same button to a *different* combo releases the
        // old one: the held set compares whole chords, not bare keycodes.
        let ctrl_tab = KeyChord::parse("ctrl+tab").unwrap();
        let rebound = HashMap::from([(BumperL1, ButtonAction::Hold(ctrl_tab))]);
        assert_eq!(
            st.reconcile(&rebound, l1, l1, true),
            vec![(BumperL1, shift_tab, false)],
            "the old combo lets go; the new one waits for a fresh press"
        );
    }

    #[test]
    fn release_all_lets_a_held_combo_go_whole_and_in_order() {
        use crate::config::KeyChord;
        // The handoff and disconnect paths must not strand a modifier down —
        // a stuck Ctrl is worse than a stuck arrow, because every later key
        // means something different.
        let ctrl_left = KeyChord::parse("ctrl+left").unwrap();
        let mut keys = live();
        let map = HashMap::from([(report::Button::DpadLeft, ButtonAction::Hold(ctrl_left))]);
        keys.reconcile(&map, |_| true, none, true);
        assert_eq!(keys.held.len(), 1);
        keys.release_all(&mut None, None);
        assert!(keys.held.is_empty());
        // The order the release would have gone out in: key first, modifier
        // after, so nothing outlives the key it was holding.
        assert_eq!(chord_edges(&ctrl_left, false), vec![(105, false), (29, false)]);

        // The chord-side twin, on the guide layer.
        let mut chords = ChordKeys::new();
        chords.press(report::Button::A, ctrl_left);
        assert_eq!(chords.drain(), vec![ctrl_left]);
        chords.press(report::Button::A, ctrl_left);
        chords.release_all(&mut None, None);
        assert!(chords.held.is_empty());
    }

    #[test]
    fn a_combo_on_a_guide_chord_is_held_whole() {
        // `h.bind("guide+a", h.key "ctrl+c")`: a Key on a chord is a held
        // output, so a short press is the copy and a long one is a held
        // Ctrl+C. Through the real dispatch, gates and all.
        use report::Button::*;
        use crate::config::KeyChord;
        let cfg = Config::from_toml_str(
            "[bindings]\n\"guide+a\" = \"key ctrl+c\"\n",
        )
        .expect("parse");
        let hypr = Hypr::detached();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut chords = ChordKeys::new();
        let mut kbd: Option<VirtualKeyboard> = None;
        let ctrl_c = KeyChord::parse("ctrl+c").unwrap();

        macro_rules! gesture {
            ($ge:expr) => {
                handle_gesture(
                    &hypr, &cfg, &mut modes, &mut osk, report::Source::SteamController, &mut hx,
                    &mut chords, &mut kbd, None, false, $ge,
                )
            };
        }

        // Recognition holds the whole combo; nothing is performed.
        assert!(!gesture!(GestureEvent::GuideChord(A)));
        assert_eq!(chords.held.get(&A), Some(&ctrl_c));
        // The button lifting under the guide releases it — key then modifier.
        assert!(!gesture!(GestureEvent::GuideChordRelease(A)));
        assert!(chords.held.is_empty());
        // And the guide letting go first does the same.
        assert!(!gesture!(GestureEvent::GuideChord(A)));
        assert_eq!(chords.held.len(), 1);
        assert!(!gesture!(GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }));
        assert!(chords.held.is_empty());
    }

    #[test]
    fn bare_button_presses_on_down_and_releases_on_up() {
        use report::Button::*;
        use ButtonAction::Hold;
        let bindings = HashMap::from([(DpadUp, Hold(103.into())), (DpadDown, Hold(108.into()))]);
        let mut st = live();
        let up = |b: report::Button| b == DpadUp;
        // DpadUp goes down while active: press KEY_UP, nothing for the unpressed
        // DpadDown. The originating button rides along for the haptic side.
        let up_down = vec![(DpadUp, KeyChord::plain(103), true)];
        assert_eq!(st.reconcile(&bindings, up, none, true), up_down);
        // Held and still down next frame: no repeated event (the kernel repeats)
        // — which is also why a held arrow cannot buzz per repeat.
        assert!(st.reconcile(&bindings, up, up, true).is_empty());
        // Lifts: release KEY_UP, held set empties.
        let up_up = vec![(DpadUp, KeyChord::plain(103), false)];
        assert_eq!(st.reconcile(&bindings, none, up, true), up_up);
        assert!(st.held.is_empty());
    }

    #[test]
    fn bare_button_gate_off_releases_and_suppresses() {
        use report::Button::*;
        let bindings = HashMap::from([(DpadUp, ButtonAction::Hold(103.into()))]);
        let mut st = live();
        let up = |b: report::Button| b == DpadUp;
        // Press while active.
        let up_down = vec![(DpadUp, KeyChord::plain(103), true)];
        assert_eq!(st.reconcile(&bindings, up, none, true), up_down);
        // The gate closes (guide/OSK/game) while the D-pad is still held: the key
        // is released cleanly rather than left stuck down.
        let up_up = vec![(DpadUp, KeyChord::plain(103), false)];
        assert_eq!(st.reconcile(&bindings, up, up, false), up_up);
        assert!(st.held.is_empty());
        // Still held but gated off: no new press (the chord/game gets it instead).
        assert!(st.reconcile(&bindings, up, up, false).is_empty());
        // Gate reopens while still held: NOT re-pressed. The button was down
        // when this layer became live again, so it waits for a fresh press —
        // the rule that keeps B from typing Backspace after closing the
        // keyboard. The arrow does not resume on its own.
        assert!(st.reconcile(&bindings, up, up, true).is_empty());
        assert!(st.reconcile(&bindings, up, up, true).is_empty());
        assert!(st.held.is_empty());
        // Release and press again: the arrow is back.
        assert!(st.reconcile(&bindings, none, up, true).is_empty());
        let up_down = vec![(DpadUp, KeyChord::plain(103), true)];
        assert_eq!(st.reconcile(&bindings, up, none, true), up_down);
    }

    #[test]
    fn bare_button_unbound_ignored_and_multiple_tracked() {
        use report::Button::*;
        use ButtonAction::{Fire, Hold};
        let bindings = HashMap::from([
            (DpadUp, Hold(103.into())),
            (DpadLeft, Hold(105.into())),
            (GripL5, Fire(Action::Exec("true".into()))),
        ]);
        let mut st = live();
        // An unbound button (A) produces nothing.
        assert!(st.reconcile(&bindings, |b| b == A, none, true).is_empty());
        // Nor does a button whose binding fires rather than holds: nothing is
        // pressed for it, and nothing is remembered as held.
        assert!(st.reconcile(&bindings, |b| b == GripL5, none, true).is_empty());
        assert!(st.held.is_empty());
        // Two bound buttons down at once: both press (HashMap order is arbitrary).
        let mut ev = st.reconcile(&bindings, |b| b == DpadUp || b == DpadLeft, none, true);
        ev.sort_by_key(|&(_, chord, _)| chord.code());
        assert_eq!(
            ev,
            vec![(DpadUp, KeyChord::plain(103), true), (DpadLeft, KeyChord::plain(105), true)]
        );
        assert_eq!(st.held.len(), 2);
    }

    #[test]
    fn a_button_held_across_a_gate_opening_waits_for_a_fresh_press() {
        // B is Backspace on the desktop and closes the keyboard while it is
        // up. The press that closes it must not ALSO type a Backspace — not on
        // the frame the gate reopens (B is a genuine press edge there: it went
        // down this frame, under the keyboard, and `route_osk` closed the
        // keyboard before the bare layer ran), nor while B stays down.
        use report::Button::*;
        let bindings = HashMap::from([(B, ButtonAction::Hold(14.into()))]);
        let mut st = live();
        let b = |x: report::Button| x == B;
        // Keyboard up: the layer is gated.
        assert!(st.reconcile(&bindings, none, none, false).is_empty());
        // B goes down and closes the keyboard in this same frame: the gate is
        // open again by the time the bare layer runs, with B a fresh edge.
        assert!(st.reconcile(&bindings, b, none, true).is_empty(), "the closing press must not type");
        // Still down on the next frames: still nothing.
        assert!(st.reconcile(&bindings, b, b, true).is_empty());
        assert!(st.reconcile(&bindings, b, b, true).is_empty());
        assert!(st.held.is_empty());
        // Released, then a fresh press: Backspace works again.
        assert!(st.reconcile(&bindings, none, b, true).is_empty());
        assert_eq!(st.reconcile(&bindings, b, none, true), vec![(B, KeyChord::plain(14), true)]);
        assert_eq!(st.reconcile(&bindings, none, b, true), vec![(B, KeyChord::plain(14), false)]);
        // And the daemon starts gated, so a button held while it comes up
        // (or while the controller reconnects) is not a keystroke either.
        let mut fresh = ButtonKeys::new();
        assert!(fresh.reconcile(&bindings, b, none, true).is_empty());
        assert!(fresh.reconcile(&bindings, b, b, true).is_empty());
    }

    #[test]
    fn a_chord_button_released_after_the_guide_does_not_fire_its_bare_binding() {
        // guide+A fires a chord; the guide is let go FIRST, with A still held.
        // The bare layer's gate opens on a button already down: no Enter until
        // A is released and pressed again. Same for a fired binding.
        use report::Button::*;
        let held = HashMap::from([(A, ButtonAction::Hold(28.into()))]);
        let mut st = live();
        let a = |x: report::Button| x == A;
        // Guide held: the layer is gated; A goes down under it (the chord).
        assert!(st.reconcile(&held, none, none, false).is_empty());
        assert!(st.reconcile(&held, a, none, false).is_empty());
        // Guide released, A still down.
        assert!(st.reconcile(&held, a, a, true).is_empty());
        assert!(st.reconcile(&held, a, a, true).is_empty());
        // Fresh press: Enter.
        assert!(st.reconcile(&held, none, a, true).is_empty());
        assert_eq!(st.reconcile(&held, a, none, true), vec![(A, KeyChord::plain(28), true)]);

        // The fired kind: under the guide the press is swallowed rather than
        // deferred, and once the gate opens A is down on both frames — not
        // an edge.
        let go = Action::Exec("true".into());
        let fired = HashMap::from([(A, ButtonAction::Fire(go.clone()))]);
        let idle = report::Frame::default();
        let down = frame_of(&[A]);
        assert!(fire_edges(&fired, &down, &idle, false).is_empty(), "under the guide");
        assert!(fire_edges(&fired, &down, &down, true).is_empty(), "guide released, A still down");
        assert_eq!(fire_edges(&fired, &down, &idle, true), vec![(A, go)], "a fresh press");
    }

    #[test]
    fn a_button_held_across_a_mode_handoff_presses_exactly_once() {
        // Reported live: R1 = Tab only in `omarchy-ui`. Tab walks the bar's
        // panels; every panel shares one layer namespace, so a switch closes
        // and reopens it and the mode flips omarchy-ui -> desktop ->
        // omarchy-ui within a few ms, each flip running the handoff
        // (`release_all`). With R1 still physically down, the next reconcile
        // used to see "bound, down, not held" and press Tab again: one tap
        // became two or three.
        use report::Button::*;
        let ui = HashMap::from([(BumperR1, ButtonAction::Hold(15.into()))]);
        let desktop: HashMap<report::Button, ButtonAction> = HashMap::new();
        let mut st = live();
        let r1 = |x: report::Button| x == BumperR1;
        // The tap: a press edge in omarchy-ui. The ONE press of this test.
        assert_eq!(st.reconcile(&ui, r1, none, true), vec![(BumperR1, KeyChord::plain(15), true)]);
        // Tab reaches the bar; the panel layer closes: handoff to the desktop,
        // where R1 is unbound. The handoff releases Tab (no device here, so
        // the held set just empties) and marks the transition.
        st.release_all(&mut None, None);
        assert!(st.held.is_empty());
        assert!(st.reconcile(&desktop, r1, r1, true).is_empty());
        // The next panel opens: handoff back to omarchy-ui, R1 bound again and
        // STILL down. Nothing may press.
        st.release_all(&mut None, None);
        assert!(st.reconcile(&ui, r1, r1, true).is_empty());
        assert!(st.reconcile(&ui, r1, r1, true).is_empty());
        // Two flips between two reports, the same.
        st.release_all(&mut None, None);
        st.release_all(&mut None, None);
        assert!(st.reconcile(&ui, r1, r1, true).is_empty());
        assert!(st.held.is_empty());
        // Release, press again: the second tap is a second Tab.
        assert!(st.reconcile(&ui, none, r1, true).is_empty());
        assert_eq!(st.reconcile(&ui, r1, none, true), vec![(BumperR1, KeyChord::plain(15), true)]);

        // A fired binding across the same handoff: the press edge fired once,
        // and after the flip R1 is down on both frames — not an edge.
        let go = Action::Exec("true".into());
        let fired = HashMap::from([(BumperR1, ButtonAction::Fire(go.clone()))]);
        let down = frame_of(&[BumperR1]);
        assert_eq!(fire_edges(&fired, &down, &report::Frame::default(), true), vec![(BumperR1, go)]);
        assert!(fire_edges(&fired, &down, &down, true).is_empty());
    }

    /// A fixed instant for the keyboard layer's repeat clock. The trackpad
    /// frames these tests use never feed it (snap navigation is padless-only),
    /// so one frozen `now` is all they need; the padless tests step it by hand.
    fn t0() -> Instant {
        *T0.get_or_init(Instant::now)
    }
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

    // --- snap navigation: the padless keyboard ------------------------------

    /// The same button set as [`frame_of`], on a controller with no trackpads.
    fn padless_frame(buttons: &[report::Button]) -> report::Frame {
        report::Frame { source: report::Source::Evdev, ..frame_of(buttons) }
    }

    /// `t0 + ms`, for the repeat clock.
    fn ms(n: u64) -> Instant {
        t0() + Duration::from_millis(n)
    }

    #[test]
    fn the_dpad_moves_the_highlight_on_a_padless_controller_and_repeats() {
        use report::Button::*;
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = padless_frame(&[]);
        // Settle the layer (the frame the keyboard came up on acts on nothing).
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());

        // A held direction steps at once, waits out the delay, then repeats at
        // eight a second — the same clock the sticks use.
        let right = padless_frame(&[DpadRight]);
        assert_eq!(st.step(&right, &idle, &bindings, ms(4)), vec![OskOp::Nav(OskNav::Right)]);
        assert!(st.step(&right, &right, &bindings, ms(200)).is_empty(), "still inside the delay");
        assert_eq!(
            st.step(&right, &right, &bindings, ms(354)),
            vec![OskOp::Nav(OskNav::Right)],
            "the repeat has started"
        );
        assert!(st.step(&right, &right, &bindings, ms(400)).is_empty());
        assert_eq!(st.step(&right, &right, &bindings, ms(479)), vec![OskOp::Nav(OskNav::Right)]);

        // Every direction, and letting go stops it.
        let up = padless_frame(&[DpadUp]);
        assert_eq!(st.step(&up, &right, &bindings, ms(500)), vec![OskOp::Nav(OskNav::Up)]);
        let down = padless_frame(&[DpadDown]);
        assert_eq!(st.step(&down, &up, &bindings, ms(504)), vec![OskOp::Nav(OskNav::Down)]);
        let left = padless_frame(&[DpadLeft]);
        assert_eq!(st.step(&left, &down, &bindings, ms(508)), vec![OskOp::Nav(OskNav::Left)]);
        assert!(st.step(&idle, &left, &bindings, ms(512)).is_empty());
        assert!(st.step(&idle, &idle, &bindings, ms(900)).is_empty(), "nothing repeats when neutral");
    }

    #[test]
    fn a_stick_thrown_past_the_gate_walks_the_highlight() {
        // What `drive_sticks` does with the two sticks while the keyboard is
        // up: the decision is `NavRepeat`'s, so it is testable without a
        // keyboard child or a device.
        let mut left = crate::osk::NavRepeat::default();
        assert_eq!(left.stick((0.0, -0.9), ms(0)), Some(OskNav::Down), "a throw steps at once");
        assert_eq!(left.stick((0.0, -0.9), ms(100)), None, "and waits out the delay");
        assert_eq!(left.stick((0.0, -0.9), ms(360)), Some(OskNav::Down));
        // Half-way out is inside the gate: a stick being rested on does not
        // move the highlight at all.
        let mut right = crate::osk::NavRepeat::default();
        assert_eq!(right.stick((0.5, 0.0), ms(0)), None);
        assert_eq!(right.stick((0.5, 0.0), ms(500)), None);
    }

    #[test]
    fn a_padless_controller_commits_the_highlight_with_a() {
        use report::Button::*;
        // Out of the box the commit is on the two pad *clicks*, which this
        // controller does not have. `A` takes its place — and the keyboard,
        // finding no cursor under the right hand, types the highlighted key.
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = padless_frame(&[]);
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());
        assert_eq!(
            st.step(&padless_frame(&[A]), &idle, &bindings, ms(4)),
            vec![OskOp::Commit(OskPad::Right)]
        );

        // The rest of the Deck map is unchanged here: B closes it, X is
        // Backspace, Y is Space, the triggers are Shift and Enter.
        let mut st = OskRoute::new(&CursorConfig::default());
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());
        assert_eq!(
            st.step(&padless_frame(&[Y]), &idle, &bindings, ms(4)),
            vec![OskOp::Key(KeyChord::plain(57), HapticPad::Right)]
        );
        let mut st = OskRoute::new(&CursorConfig::default());
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());
        assert_eq!(st.step(&padless_frame(&[B]), &idle, &bindings, ms(4)), vec![OskOp::Dismiss]);
    }

    #[test]
    fn a_config_entry_on_a_still_wins_over_the_padless_commit() {
        use report::Button::*;
        // The padless default is a *default*: it fills a button in only when
        // nothing else claims it, exactly like the built-in map.
        let mut bindings = deck_map();
        bindings.insert(A, OskAction::Dismiss);
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = padless_frame(&[]);
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());
        assert_eq!(st.step(&padless_frame(&[A]), &idle, &bindings, ms(4)), vec![OskOp::Dismiss]);
    }

    #[test]
    fn a_trackpad_controllers_keyboard_is_untouched_by_snap_navigation() {
        use report::Button::*;
        // Neither the D-pad nor `A` means anything under the keyboard on a
        // controller that has trackpads: the two cursors are how you point at a
        // key there, and giving those buttons a meaning would be a change to a
        // controller this feature is not about.
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = report::Frame::default();
        assert!(st.step(&idle, &idle, &bindings, ms(0)).is_empty());
        for held in [DpadUp, DpadDown, DpadLeft, DpadRight, A] {
            let f = frame_of(&[held]);
            assert!(
                st.step(&f, &idle, &bindings, ms(4)).is_empty(),
                "{held:?} must do nothing on the Steam Controller"
            );
            assert!(st.step(&f, &f, &bindings, ms(500)).is_empty(), "and never start repeating");
            assert!(st.step(&idle, &f, &bindings, ms(504)).is_empty());
        }
        // The pad clicks still commit under their own cursor, as always.
        assert_eq!(
            st.step(&frame_of(&[PadRightClick]), &idle, &bindings, ms(600)),
            vec![OskOp::Commit(OskPad::Right)]
        );
    }

    /// The Deck map as the keyboard's layer sees it: the built-ins with the
    /// default config's (identical) `y`/`x` over them.
    fn deck_map() -> HashMap<report::Button, OskAction> {
        Config::load_default().osk_buttons_in(&crate::config::ModeState::new("desktop", vec![]))
    }

    #[test]
    fn the_keyboard_layer_ignores_the_button_that_raised_it_until_a_fresh_press() {
        // guide+Y raises the keyboard, and Y is also Space on it. The Y still
        // held from the chord must not type a Space — not on the frame the
        // keyboard came up (Y is a press edge there), nor while it stays down.
        // Released and pressed again, it is Space.
        use report::Button::*;
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let guide = frame_of(&[Steam]);
        let chord = frame_of(&[Steam, Y]);
        // The frame the chord landed on: the keyboard is up, routing starts.
        assert!(st.step(&chord, &guide, &bindings, t0()).is_empty(), "the raising press must not type");
        assert!(st.step(&chord, &chord, &bindings, t0()).is_empty());
        // Guide let go, Y still down; then Y let go.
        let y = frame_of(&[Y]);
        assert!(st.step(&y, &chord, &bindings, t0()).is_empty());
        let idle = report::Frame::default();
        assert!(st.step(&idle, &y, &bindings, t0()).is_empty());
        // A fresh press: Space, clicked under the right hand.
        let space = vec![OskOp::Key(KeyChord::plain(57), HapticPad::Right)];
        assert_eq!(st.step(&y, &idle, &bindings, t0()), space);
        assert!(st.step(&y, &y, &bindings, t0()).is_empty(), "held: no repeat");
    }

    #[test]
    fn the_keyboard_layer_routes_the_decks_map() {
        use report::Button::*;
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = report::Frame::default();
        assert!(st.step(&idle, &idle, &bindings, t0()).is_empty(), "the first frame settles the layer");

        // Pad clicks commit under their own pad; X is Backspace, under the
        // right hand; R2 is Enter, not a commit.
        let lclick = frame_of(&[PadLeftClick]);
        let rclick = frame_of(&[PadRightClick]);
        assert_eq!(st.step(&lclick, &idle, &bindings, t0()), vec![OskOp::Commit(OskPad::Left)]);
        assert_eq!(st.step(&rclick, &lclick, &bindings, t0()), vec![OskOp::Commit(OskPad::Right)]);
        let backspace = vec![OskOp::Key(KeyChord::plain(14), HapticPad::Right)];
        assert_eq!(st.step(&frame_of(&[X]), &rclick, &bindings, t0()), backspace);
        assert_eq!(
            st.step(&frame_of(&[TriggerR2Full]), &idle, &bindings, t0()),
            vec![OskOp::Key(KeyChord::plain(28), HapticPad::Right)]
        );

        // L2 holds Shift: down with the pull, nothing while held, up with the
        // release. A commit in between is just a commit — the keyboard applies
        // the shift.
        let l2 = frame_of(&[TriggerL2Full]);
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        assert!(st.step(&l2, &l2, &bindings, t0()).is_empty());
        let l2_click = frame_of(&[TriggerL2Full, PadRightClick]);
        assert_eq!(st.step(&l2_click, &l2, &bindings, t0()), vec![OskOp::Commit(OskPad::Right)]);
        assert!(st.step(&l2, &l2_click, &bindings, t0()).is_empty());
        assert_eq!(st.step(&idle, &l2, &bindings, t0()), vec![OskOp::ShiftUp]);

        // B closes it — releasing a held Shift first — and disarms the layer.
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        let l2_b = frame_of(&[TriggerL2Full, B]);
        assert_eq!(st.step(&l2_b, &l2, &bindings, t0()), vec![OskOp::ShiftUp, OskOp::Dismiss]);
        assert!(!st.live && st.shift_holders.is_empty());
        // Shown again with L2 still pulled: the first frame acts on nothing,
        // and L2 is not remembered as holding Shift.
        assert!(st.step(&l2, &l2_b, &bindings, t0()).is_empty());
        assert!(st.step(&l2, &l2, &bindings, t0()).is_empty());
        assert!(st.step(&idle, &l2, &bindings, t0()).is_empty(), "nothing to release");
        // Menu closes it too; an unbound button (a grip) does nothing.
        assert_eq!(st.step(&frame_of(&[Menu]), &idle, &bindings, t0()), vec![OskOp::Dismiss]);
        st.step(&idle, &idle, &bindings, t0());
        assert!(st.step(&frame_of(&[GripL5]), &idle, &bindings, t0()).is_empty());
    }

    #[test]
    fn two_shift_buttons_hold_one_shift() {
        // Shift goes down with the first holder and up with the last: a config
        // binding both triggers to `osk shift` gets one Shift, not a flicker.
        use report::Button::*;
        let bindings = HashMap::from([
            (TriggerL2Full, OskAction::Shift),
            (TriggerR2Full, OskAction::Shift),
            (Menu, OskAction::Dismiss),
        ]);
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = report::Frame::default();
        st.step(&idle, &idle, &bindings, t0());
        let l2 = frame_of(&[TriggerL2Full]);
        let both = frame_of(&[TriggerL2Full, TriggerR2Full]);
        let r2 = frame_of(&[TriggerR2Full]);
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        assert!(st.step(&both, &l2, &bindings, t0()).is_empty());
        assert!(st.step(&r2, &both, &bindings, t0()).is_empty(), "one holder left");
        assert_eq!(st.step(&idle, &r2, &bindings, t0()), vec![OskOp::ShiftUp]);
        // Swapping holders within one frame: up for the old, down for the new.
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        assert_eq!(st.step(&r2, &l2, &bindings, t0()), vec![OskOp::ShiftUp, OskOp::ShiftDown]);
    }

    #[test]
    fn a_mode_handoff_keeps_the_keyboard_layer_but_a_toggle_forgets_it() {
        // `reset` (a handoff while typing) keeps the layer live and its Shift
        // holder — the keyboard is still up and the trigger still pulled;
        // `disarm` (the keyboard went down) forgets both.
        use report::Button::*;
        let bindings = deck_map();
        let mut st = OskRoute::new(&CursorConfig::default());
        let idle = report::Frame::default();
        let l2 = frame_of(&[TriggerL2Full]);
        st.step(&idle, &idle, &bindings, t0());
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        st.reset();
        assert!(st.live && st.shift_holders == vec![TriggerL2Full]);
        assert_eq!(st.step(&idle, &l2, &bindings, t0()), vec![OskOp::ShiftUp], "the release is still seen");
        assert_eq!(st.step(&l2, &idle, &bindings, t0()), vec![OskOp::ShiftDown]);
        st.disarm();
        assert!(!st.live && st.shift_holders.is_empty());
        assert!(st.step(&idle, &l2, &bindings, t0()).is_empty());
    }

    #[test]
    fn a_commit_reads_the_pad_under_the_same_hand() {
        use report::Button::*;
        assert_eq!(commit_pad(PadLeftClick), OskPad::Left);
        assert_eq!(commit_pad(PadRightClick), OskPad::Right);
        assert_eq!(commit_pad(TriggerL2Full), OskPad::Left);
        assert_eq!(commit_pad(GripL5), OskPad::Left);
        assert_eq!(commit_pad(A), OskPad::Right);
        assert_eq!(commit_pad(BumperR1), OskPad::Right);
        assert_eq!(commit_pad(Steam), OskPad::Right);
    }

    #[test]
    fn a_fired_binding_fires_once_on_the_press_edge_and_never_while_gated() {
        use report::Button::*;
        use ButtonAction::{Fire, Hold};
        let go = Action::Exec("true".into());
        let bindings = HashMap::from([(GripL5, Fire(go.clone())), (DpadUp, Hold(103.into()))]);
        let idle = report::Frame::default();
        let down = frame_of(&[GripL5, DpadUp]);

        // The press edge: L5 fires. The D-pad is held, not fired — not this
        // pass's business.
        assert_eq!(fire_edges(&bindings, &down, &idle, true), vec![(GripL5, go.clone())]);
        // Still down on the next frame: nothing. A fired action never repeats.
        assert!(fire_edges(&bindings, &down, &down, true).is_empty());
        // Release: nothing either.
        assert!(fire_edges(&bindings, &idle, &down, true).is_empty());
        // A fresh press fires again.
        assert_eq!(fire_edges(&bindings, &down, &idle, true), vec![(GripL5, go)]);

        // Gate closed (guide held, keyboard up): the edge is swallowed, not
        // deferred to the frame the gate reopens on.
        assert!(fire_edges(&bindings, &down, &idle, false).is_empty());
        assert!(fire_edges(&bindings, &down, &down, true).is_empty());
        // An unbound button's edge is nothing.
        assert!(fire_edges(&bindings, &frame_of(&[A]), &idle, true).is_empty());
    }

    #[test]
    fn perform_action_reports_a_manual_mode_change_for_the_handoff() {
        use crate::mode::{BUILTIN_DESKTOP, BUILTIN_GAME};
        let hypr = Hypr::detached();
        let cfg = Config::load_default();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        let src = report::Source::SteamController;
        assert_eq!(modes.active(), BUILTIN_DESKTOP);

        // Forcing game mode from the desktop is a change; forcing it again is not.
        let force_game = Action::SetMode(BUILTIN_GAME.into());
        assert!(perform_action(&hypr, &cfg, &mut modes, &mut osk, src, &mut None, None, &force_game));
        assert_eq!(modes.active(), BUILTIN_GAME);
        assert!(!perform_action(&hypr, &cfg, &mut modes, &mut osk, src, &mut None, None, &force_game));
        // Clearing it hands the decision back to the rules (nothing focused:
        // desktop); clearing an override that is not there changes nothing.
        assert!(perform_action(&hypr, &cfg, &mut modes, &mut osk, src, &mut None, None, &Action::ClearMode));
        assert_eq!(modes.active(), BUILTIN_DESKTOP);
        assert!(!perform_action(&hypr, &cfg, &mut modes, &mut osk, src, &mut None, None, &Action::ClearMode));
    }

    #[test]
    fn a_sequence_performs_its_steps_in_order() {
        use crate::mode::{BUILTIN_DESKTOP, BUILTIN_GAME};
        let hypr = Hypr::detached();
        let cfg = Config::load_default();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        macro_rules! perform {
            ($a:expr) => {
                perform_action(
                    &hypr,
                    &cfg,
                    &mut modes,
                    &mut osk,
                    report::Source::SteamController,
                    &mut None,
                    None,
                    &$a,
                )
            };
        }

        // Order, not set semantics: the LAST `set_mode` is where we end up.
        let both = Action::Seq(vec![
            Action::SetMode(BUILTIN_GAME.into()),
            Action::SetMode(BUILTIN_DESKTOP.into()),
        ]);
        assert!(perform!(both), "the sequence moved the mode, so the caller runs the handoff");
        assert_eq!(modes.active(), BUILTIN_DESKTOP);

        // The hints shape: a key then a mode. The key step must not swallow
        // the sequence — the `set_mode` after it still lands, and the "mode
        // moved" answer is the OR over the steps.
        let hints = Action::Seq(vec![
            Action::Key(KeyChord::parse("f").unwrap()),
            Action::SetMode(BUILTIN_GAME.into()),
        ]);
        assert!(perform!(hints));
        assert_eq!(modes.active(), BUILTIN_GAME);

        // A sequence that moves nothing says so, however many steps it has.
        let quiet = Action::Seq(vec![
            Action::Key(KeyChord::parse("f").unwrap()),
            Action::Key(KeyChord::parse("shift+f").unwrap()),
        ]);
        assert!(!perform!(quiet));
        assert_eq!(modes.active(), BUILTIN_GAME);
        // And `clear_mode` inside one is the ordinary clear.
        assert!(perform!(Action::Seq(vec![Action::ClearMode])));
        assert_eq!(modes.active(), BUILTIN_DESKTOP);
    }

    #[test]
    fn a_sequence_on_a_guide_chord_performs_rather_than_holding() {
        // The entry chord, through the real dispatch. A lone `h.key` on a
        // chord is a HELD output; a `h.seq` containing one is not — it is
        // performed on recognition, taps its key, and its `set_mode` lands.
        use report::Button::*;
        let cfg = crate::lua_config::load_str(
            r#"
            local h = hyprpad
            h.mode("hints"):transient {}
            h.mode("desktop")
            h.bind("guide+l4", "Link hints", h.seq { h.key "f", h.set_mode "hints" })
            "#,
            "t.lua",
        )
        .expect("config should load");
        let hypr = Hypr::detached();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut chords = ChordKeys::new();
        let mut kbd: Option<VirtualKeyboard> = None;

        // Recognition performs the whole sequence and reports the mode move,
        // so the caller runs the handoff.
        assert!(handle_gesture(
            &hypr, &cfg, &mut modes, &mut osk, report::Source::SteamController, &mut hx, &mut chords,
            &mut kbd, None, false,
            GestureEvent::GuideChord(GripL4),
        ));
        assert_eq!(modes.active(), "hints");
        assert!(chords.held.is_empty(), "a sequence holds nothing: its key was tapped");
        // The chord's release therefore has nothing to let go of.
        assert!(!handle_gesture(
            &hypr, &cfg, &mut modes, &mut osk, report::Source::SteamController, &mut hx, &mut chords,
            &mut kbd, None, false,
            GestureEvent::GuideChordRelease(GripL4),
        ));
        assert_eq!(modes.active(), "hints");
    }

    #[test]
    fn a_key_inside_a_sequence_is_a_tap_not_a_hold() {
        // KEY_F = 33, KEY_LEFTSHIFT = 42. A sequence has no release edge to
        // pair a held key with — by the time the button lifts the sequence is
        // long over — so the key goes down and comes straight back up.
        let f = KeyChord::parse("f").unwrap();
        assert_eq!(tap_edges(&f), vec![(33, true), (33, false)]);
        // With modifiers bracketing it exactly as a held binding's do, so
        // `h.key "shift+f"` in a sequence is a real Shift+F and not two loose
        // keystrokes.
        let shift_f = KeyChord::parse("shift+f").unwrap();
        assert_eq!(
            tap_edges(&shift_f),
            vec![(42, true), (33, true), (33, false), (42, false)]
        );
        // The two halves are the held binding's own edges, in order — one
        // definition of "what pressing this chord means", not two.
        assert_eq!(
            tap_edges(&shift_f),
            [chord_edges(&shift_f, true), chord_edges(&shift_f, false)].concat()
        );
    }

    #[test]
    fn a_transient_exit_press_is_counted_before_the_button_layer_and_masked_out_of_it() {
        // The frame arm's own order, in miniature: the press edges are noted
        // against the mode they resolved in, an exit press clears the mode and
        // is masked out of the frame the bare-button layer then sees.
        use report::Button::*;
        let cfg = crate::lua_config::load_str(
            r#"
            local h = hyprpad
            h.mode("hints"):transient { max_presses = 3, exit_on = { "b" }, timeout_ms = 0 }
            h.mode("browser").when(function(ctx) return ctx.focus.class == "google-chrome" end)
            h.mode("desktop")
            h.button("a", h.key "a"):only_in("hints")
            h.button("b", h.key "backspace"):only_in("browser")
            "#,
            "t.lua",
        )
        .expect("config should load");
        let mut modes = ModeEngine::new(&cfg);
        modes.focus_changed(&cfg, "google-chrome", "Some page", None);
        modes.set_mode(&cfg, "hints");

        let idle = report::Frame::default();
        let b_down = frame_of(&[B]);
        // The daemon offers the engine every press edge the layer is open for
        // — B is the exit precisely BECAUSE the hints mode leaves it unbound,
        // so a map-filtered list would never surface it.
        assert_eq!(press_edges(&b_down, &idle, true), vec![B]);
        assert!(press_edges(&b_down, &idle, false).is_empty());
        // The engine applies the map where the map is what matters: only a
        // press this mode binds spends one of the three, so a stray grip does
        // not.
        assert_eq!(modes.note_press(&cfg, GripL4), crate::mode::PressOutcome::default());
        assert_eq!(modes.note_press(&cfg, A), crate::mode::PressOutcome::default());
        assert!(!modes.settle_presses(&cfg));
        assert_eq!(modes.active(), "hints");

        // B is the exit: consumed, and the mode goes.
        let out = modes.note_press(&cfg, B);
        assert_eq!(out, crate::mode::PressOutcome { consumed: true, changed: true });
        assert_eq!(modes.active(), "browser");

        // Masked out, the frame the button layer sees has no B in it — so the
        // browser's `b` = Backspace does not fire on the way out...
        let masked = b_down.without(&[B]);
        assert!(fire_edges(modes.buttons(), &masked, &idle, true).is_empty());
        let mut keys = ButtonKeys { held: HashMap::new(), open: true };
        assert!(keys
            .reconcile(modes.buttons(), |b| masked.pressed(b), |b| idle.pressed(b), true)
            .is_empty());
        // ...and the UNMASKED frame is what becomes `prev`, so B still being
        // held on the next frame is not a fresh edge either.
        assert!(press_edges(&b_down, &b_down, true).is_empty());
    }

    #[test]
    fn a_bare_button_that_forces_a_mode_reports_it_like_a_chord_would() {
        use crate::mode::{BUILTIN_DESKTOP, BUILTIN_GAME};
        use report::Button::*;
        // A `set_mode` on a bare button goes through the same `perform_action`
        // a chord's does, so the frame arm gets the same "mode changed" answer
        // and runs the same handoff.
        let cfg = Config::from_toml_str("[buttons]\nl5 = \"set_mode game\"\ndpad_up = \"key up\"\n")
            .expect("parse");
        let hypr = Hypr::detached();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let idle = report::Frame::default();
        let l5 = frame_of(&[GripL5]);

        // Under the guide layer the press is a chord's, not ours.
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &mut None, None, &l5, &idle, false, &mut hx));
        assert_eq!(modes.active(), BUILTIN_DESKTOP);
        // On the desktop the press edge forces the game mode — and says so.
        assert!(fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &mut None, None, &l5, &idle, true, &mut hx));
        assert_eq!(modes.active(), BUILTIN_GAME);
        // Held: nothing more. And in the game the bare buttons are not live at
        // all (the built-in path empties the map), so a fresh press is inert.
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &mut None, None, &l5, &l5, true, &mut hx));
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &mut None, None, &l5, &idle, true, &mut hx));
        assert!(modes.buttons().is_empty());
    }

    #[test]
    fn haptic_feel_maps_each_trigger_to_its_pulse() {
        // The defaults: everything on except the bare-button tick.
        let cfg = HapticsConfig::default();
        assert_eq!(haptic_feel(&cfg, Haptic::Crossing), Some(Feel::Tick));
        assert_eq!(haptic_feel(&cfg, Haptic::Commit), Some(Feel::Click));
        assert_eq!(haptic_feel(&cfg, Haptic::Gesture), Some(Feel::Buzz));
        assert_eq!(haptic_feel(&cfg, Haptic::Scroll), Some(Feel::Tick));
        // A word-tier scrub detent is the same wheel with a bigger notch: the
        // scroll toggle, a heavier feel.
        assert_eq!(haptic_feel(&cfg, Haptic::ScrubWord), Some(Feel::Click));
        assert_eq!(haptic_feel(&cfg, Haptic::Button), None);
    }

    #[test]
    fn haptic_master_switch_and_per_trigger_toggles_gate() {
        // `enabled = false` silences every trigger, whatever the per-trigger
        // toggles say.
        let off = HapticsConfig { enabled: false, buttons: true, ..HapticsConfig::default() };
        for what in [
            Haptic::Crossing,
            Haptic::Commit,
            Haptic::Gesture,
            Haptic::Scroll,
            Haptic::ScrubWord,
            Haptic::Button,
        ] {
            assert_eq!(haptic_feel(&off, what), None, "{what:?} must be silent");
        }
        // A per-trigger toggle silences only its own trigger — and the scrub's
        // two detent feels share the scroll toggle, so one knob silences the
        // whole wheel.
        let no_scroll = HapticsConfig { scroll: false, ..HapticsConfig::default() };
        assert_eq!(haptic_feel(&no_scroll, Haptic::Scroll), None);
        assert_eq!(haptic_feel(&no_scroll, Haptic::ScrubWord), None);
        assert_eq!(haptic_feel(&no_scroll, Haptic::Crossing), Some(Feel::Tick));
        // And the opt-in bare-button tick can be switched on.
        let buttons = HapticsConfig { buttons: true, ..HapticsConfig::default() };
        assert_eq!(haptic_feel(&buttons, Haptic::Button), Some(Feel::Tick));
    }

    #[test]
    fn haptic_pads_follow_the_hand() {
        use report::Button::*;
        // Each OSK cursor ticks under its own thumb.
        assert_eq!(haptic_pad(OskPad::Left), HapticPad::Left);
        assert_eq!(haptic_pad(OskPad::Right), HapticPad::Right);
        // Buttons fire on the side they sit on; the guide belongs to neither.
        assert_eq!(button_pad(Y), HapticPad::Right);
        assert_eq!(button_pad(BumperR1), HapticPad::Right);
        assert_eq!(button_pad(PadRightClick), HapticPad::Right);
        assert_eq!(button_pad(DpadUp), HapticPad::Left);
        assert_eq!(button_pad(TriggerL2Full), HapticPad::Left);
        assert_eq!(button_pad(PadLeftClick), HapticPad::Left);
        assert_eq!(button_pad(Steam), HapticPad::Both);
    }

    #[test]
    fn forward_osk_events_feeds_the_loop_and_stops_with_it() {
        let (etx, erx) = mpsc::channel::<OskEvent>();
        let (itx, irx) = mpsc::channel::<Input>();
        etx.send(OskEvent::Crossed(OskPad::Left)).unwrap();
        etx.send(OskEvent::Crossed(OskPad::Right)).unwrap();
        drop(etx); // the OSK reader thread ended (child gone)

        forward_osk_events(erx, itx);

        assert!(matches!(irx.recv(), Ok(Input::Osk(OskEvent::Crossed(OskPad::Left)))));
        assert!(matches!(irx.recv(), Ok(Input::Osk(OskEvent::Crossed(OskPad::Right)))));
        assert!(irx.recv().is_err(), "forwarder dropped its sender on exit");

        // With the loop gone the forwarder returns instead of panicking.
        let (etx, erx) = mpsc::channel::<OskEvent>();
        let (itx, irx) = mpsc::channel::<Input>();
        etx.send(OskEvent::Crossed(OskPad::Left)).unwrap();
        drop(irx);
        forward_osk_events(erx, itx);
        drop(etx);
    }

    #[test]
    fn reload_keeps_old_config_on_parse_error() {
        // The safety property: a parse error keeps the current config verbatim,
        // so a typo in the file never blanks the daemon.
        let current = Config::load_default();
        let current_len = current.len();
        let (cfg, outcome) = resolve_reload(current, Err("line 3: unknown button".to_string()));
        assert!(outcome.is_err());
        assert_eq!(cfg.len(), current_len);
        // The kept config still resolves its default binding.
        assert_eq!(
            cfg.resolve(&GestureEvent::GuideChord(report::Button::BumperR1)),
            Action::Workspace(WorkspaceTarget::Relative(1))
        );
    }

    #[test]
    fn a_broken_config_lua_leaves_the_last_good_config_intact() {
        // The Lua front-end's half of the reload guardrail: a `config.lua` that
        // no longer compiles arrives here as an `Err`, and `resolve_reload`
        // keeps the running config byte for byte — the daemon never goes input
        // dead over a typo. (HypXRland's "a broken syntax doesn't nuke the
        // config and leave the user with no binds", one layer up.)
        let good = crate::lua_config::load_str(
            r#"
            hyprpad.mode("desktop")
            hyprpad.bind("guide+r1", hyprpad.exec "good")
            "#,
            "good.lua",
        )
        .expect("the good config loads");

        for broken in [
            "hyprpad.bind(",                       // syntax error
            r#"hyprpad.cursor { sesn = 1 }"#,      // unknown key
            r#"hyprpad.button("a", hyprpad.key "up"):only_in("nope")"#, // undeclared mode
        ] {
            let loaded = crate::lua_config::load_str(broken, "broken.lua");
            assert!(loaded.is_err(), "{broken:?} should not load");
            let (kept, outcome) = resolve_reload(good.clone(), loaded);
            assert!(outcome.is_err());
            assert_eq!(
                kept.resolve(&GestureEvent::GuideChord(report::Button::BumperR1)),
                Action::Exec("good".into()),
                "the last-good binding must survive {broken:?}"
            );
            assert_eq!(kept.modes().len(), 1, "and so must its modes");
        }
    }

    #[test]
    fn reload_swaps_in_new_config_on_success() {
        // A successful load replaces the config wholesale: the new binding is in
        // effect and the old default binding is gone.
        let current = Config::load_default();
        let new = Config::from_toml_str("[bindings]\n\"guide+a\" = \"fullscreen\"\n").unwrap();
        let (cfg, outcome) = resolve_reload(current, Ok(new));
        assert!(outcome.is_ok());
        assert_eq!(
            cfg.resolve(&GestureEvent::GuideChord(report::Button::A)),
            Action::ToggleFullscreen
        );
        assert_eq!(
            cfg.resolve(&GestureEvent::GuideChord(report::Button::BumperR1)),
            Action::None
        );
    }

    #[test]
    fn the_two_handled_signals_map_to_their_own_loop_inputs() {
        // One pipe, one waiter, two meanings — and getting them the wrong way
        // round would turn a config reload into a power-off, so the mapping is
        // pinned here rather than only in the waiter thread.
        assert!(matches!(signal_to_input(libc::SIGHUP), Some(Input::Reload)));
        assert!(matches!(signal_to_input(libc::SIGUSR1), Some(Input::ControllerOff)));
        // Nothing else is handled: a corrupt byte must be dropped, not guessed
        // at. SIGTERM in particular belongs to the lizard restore path, and
        // must never be answered here.
        for other in [libc::SIGTERM, libc::SIGINT, libc::SIGUSR2, 0] {
            assert!(signal_to_input(other).is_none(), "signal {other} should not be ours");
        }
    }

    #[test]
    fn cursor_reconfigure_applies_sens() {
        // A live reload rebuilds the damper (fresh filter state, no jump) and
        // applies the new gain. There is no held-click state here to preserve
        // any more: a click is a bare-button binding, `ButtonKeys`'s to hold.
        let mut st = CursorState::new(&CursorConfig::default());
        let cfg = CursorConfig { sens: 0.2, ..CursorConfig::default() };
        st.reconfigure(&cfg);
        assert_eq!(st.sens, 0.2);
    }

    #[test]
    fn bare_button_codes_route_by_the_evdev_code_space() {
        // The whole design in one decision: the three BTN_MOUSE codes go to the
        // pointer, every KEY_* code (and anything else) to the keyboard.
        assert_eq!(route(0x110), Route::Pointer(PointerButton::Left));
        assert_eq!(route(0x111), Route::Pointer(PointerButton::Right));
        assert_eq!(route(0x112), Route::Pointer(PointerButton::Middle));
        for code in [1u16, 14, 28, 103, 158, 255, 0x10f, 0x113] {
            assert_eq!(route(code), Route::Keyboard, "code {code:#x}");
        }
        // And the config agrees on what a mouse binding parses to.
        assert_eq!(
            Action::parse("mouse left"),
            Ok(Action::Key(PointerButton::Left.evdev().into()))
        );
    }

    #[test]
    fn gamepad_gate_encodes_the_precedence_model() {
        // The everyday case: a game has focus, nothing above it is claiming.
        assert!(gamepad_forwarding(true, true, true, false, false));
        // Rank 4 only: no game focused, so the desktop layer keeps the pads and
        // the virtual pad stays neutral.
        assert!(!gamepad_forwarding(true, false, false, false, false));
        // Rank 1 beats rank 3: the guide layer works *in* a game, and while it
        // is held the game gets nothing — that is the docs/08 input contract.
        assert!(!gamepad_forwarding(true, true, true, true, false));
        // Rank 2 beats rank 3: `Guide+Y` raises the keyboard over a game, and
        // while it is up the pads aim at keys, not at the game.
        assert!(!gamepad_forwarding(true, true, true, false, true));
        // Both at once is still off.
        assert!(!gamepad_forwarding(true, true, true, true, true));
        // The config master switch overrides everything below it.
        assert!(!gamepad_forwarding(false, true, true, false, false));
    }

    /// The whole decision table for the synthesized guide tap: mode × tap kind.
    #[test]
    fn a_guide_tap_reaches_steam_only_as_a_bare_tap_in_a_game() {
        let on = GamepadConfig::default();
        assert_eq!(on.guide_tap, GuideTap::Steam, "the owner wants this on");
        let off = GamepadConfig { guide_tap: GuideTap::None, ..GamepadConfig::default() };

        // forwarding × bare_tap, with the feature on.
        assert!(guide_tap_pulse(&on, true, true), "a bare tap in a game opens the overlay");
        assert!(
            !guide_tap_pulse(&on, true, false),
            "a chord, a flick, a consumed hold or a long hold: the engine said no"
        );
        assert!(
            !guide_tap_pulse(&on, false, true),
            "the same tap on the desktop does what it always did — nothing"
        );
        assert!(!guide_tap_pulse(&on, false, false));

        // And one word turns it off, in a game as anywhere else.
        for forwarding in [true, false] {
            for tap in [true, false] {
                assert!(!guide_tap_pulse(&off, forwarding, tap));
            }
        }
    }

    /// The other consumer of the same bare tap, and the precedence between the
    /// two: exactly one of them gets it, and `forwarding` is what picks.
    #[test]
    fn a_bare_tap_binds_everywhere_the_pulse_does_not() {
        let on = GamepadConfig::default();
        let off = GamepadConfig { guide_tap: GuideTap::None, ..GamepadConfig::default() };

        // In a game the pulse takes it and the binding does not run: the
        // owner's rule, "in a game the Steam button is Steam's".
        assert!(guide_tap_pulse(&on, true, true));
        assert!(!guide_tap_binding(&on, true, true), "in a game the Steam button is Steam's");
        // On the desktop nothing is being forwarded to, so the binding is the
        // tap's only consumer — this is what summons the launcher.
        assert!(!guide_tap_pulse(&on, false, true));
        assert!(guide_tap_binding(&on, false, true));

        // Never both, and never neither, for a real tap — whatever the config
        // and whatever the mode.
        for cfg in [&on, &off] {
            for forwarding in [true, false] {
                assert!(
                    guide_tap_pulse(cfg, forwarding, true)
                        ^ guide_tap_binding(cfg, forwarding, true),
                    "a bare tap has exactly one consumer"
                );
            }
        }

        // What the engine did NOT call a tap binds nothing, in either mode: a
        // chord, a flick, a hold the scrub or the guide-mouse spent, a hold
        // past the cap.
        for forwarding in [true, false] {
            assert!(!guide_tap_binding(&on, forwarding, false));
            assert!(!guide_tap_binding(&off, forwarding, false));
        }

        // `guide_tap = "none"` means "the guide is hyprpad's alone, in a game
        // too": with no pulse anywhere to defer to, the binding runs in a game
        // as well.
        assert!(guide_tap_binding(&off, true, true));
    }

    /// End to end through the real engine and the real config: which guide
    /// releases run a `guide_tap` binding. The narrow decision, not "the leave
    /// was unchorded" — the two differ exactly where the owner would feel it.
    #[test]
    fn the_tap_binding_is_the_engines_narrow_tap_not_any_unchorded_leave() {
        let cfg =
            Config::from_toml_str("[bindings]\n\"guide_tap\" = \"exec omarchy launcher toggle\"\n")
                .expect("parse");
        let bound = Action::Exec("omarchy launcher toggle".to_string());

        let bit = |b: report::Button| -> u32 {
            (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit")
        };
        let frame = |buttons: &[report::Button]| report::Frame {
            buttons: buttons.iter().fold(0, |acc, &b| acc | (1 << bit(b))),
            ..report::Frame::default()
        };

        // Hold the guide for `hold_ms`, optionally spending it the way the
        // caret scrub and the guide-mouse do, and resolve the leave it ends in.
        let leave = |hold_ms: u64, consume: bool| -> Action {
            let mut engine = GestureEngine::new();
            let t = Instant::now();
            engine.update(&frame(&[]), t);
            engine.update(&frame(&[report::Button::Steam]), t + Duration::from_millis(4));
            if consume {
                engine.consume_hold();
            }
            let ev = engine
                .update(&frame(&[]), t + Duration::from_millis(4 + hold_ms))
                .into_iter()
                .find(|e| matches!(e, GestureEvent::GuideLeave { .. }))
                .expect("the release is a leave");
            cfg.resolve(&ev)
        };

        // A quick press and release with nothing in it: the binding.
        assert_eq!(leave(120, false), bound, "a bare tap is what the binding is for");
        // A hold the scrub or the guide-mouse spent: nothing. Without this,
        // every caret fix would summon the launcher on the way out.
        assert_eq!(leave(120, true), Action::None, "a consumed hold binds nothing");
        assert_eq!(leave(3_000, true), Action::None);
        // A long deliberating hold that ended in nothing at all: also nothing,
        // even though its leave is unchorded.
        assert_eq!(leave(3_000, false), Action::None, "3 s is deliberating, not tapping");
        // And the cap is where the owner set it.
        assert_eq!(leave(400, false), bound, "the cap itself is still a tap");
        assert_eq!(leave(401, false), Action::None, "one ms past it is not");
    }

    /// The gate the pulse rides on is the *same* gate the frame's input rides
    /// on — and it is asked at the release, when the guide is already up, so
    /// the guide layer is no longer suppressing it.
    #[test]
    fn the_tap_is_gated_on_the_forwarding_decision_of_the_release_frame() {
        let cfg = Config::load_default();
        let mut m = ModeEngine::new(&cfg);
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        let gp = cfg.gamepad();

        // While the guide is HELD the game is getting nothing, and a tap cannot
        // be recognised anyway — belt and braces, the gate says no.
        let held = gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), true, false);
        assert!(!guide_tap_pulse(gp, held, true));
        // On the release frame the guide is up, so the gate is open again and
        // the tap lands.
        let released = gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), false, false);
        assert!(guide_tap_pulse(gp, released, true));

        // On the desktop the same release is inert.
        let mut d = ModeEngine::new(&cfg);
        d.focus_changed(&cfg, "Alacritty", "", None);
        let desktop = gamepad_forwarding(true, d.forwards(), d.desktop_yielded(), false, false);
        assert!(!guide_tap_pulse(gp, desktop, true));

        // As it is with the whole forwarding path switched off.
        let off = GamepadConfig { enabled: false, ..GamepadConfig::default() };
        let disabled = gamepad_forwarding(false, m.forwards(), m.desktop_yielded(), false, false);
        assert!(!guide_tap_pulse(&off, disabled, true));
    }

    /// End to end through the gesture engine: the frames a quick tap in a game
    /// produces make `guide_tap_pulse` true exactly once, and a chord or a long
    /// hold over the same game never does.
    #[test]
    fn the_engine_and_the_gate_agree_about_what_a_game_gets() {
        let cfg = Config::load_default();
        let mut m = ModeEngine::new(&cfg);
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        let gp = cfg.gamepad();

        let bit = |b: report::Button| -> u32 {
            (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit")
        };
        let frame = |buttons: &[report::Button]| report::Frame {
            buttons: buttons.iter().fold(0, |acc, &b| acc | (1 << bit(b))),
            ..report::Frame::default()
        };
        // Feed one gesture and count the frames that would pulse.
        let pulses = |steps: &[(&[report::Button], u64)]| -> usize {
            let mut engine = GestureEngine::new();
            let t = Instant::now();
            let mut n = 0;
            for &(buttons, at) in steps {
                engine.update(&frame(buttons), t + Duration::from_millis(at));
                let fwd = gamepad_forwarding(
                    true,
                    m.forwards(),
                    m.desktop_yielded(),
                    engine.guide_active(),
                    false,
                );
                if guide_tap_pulse(gp, fwd, engine.guide_tap()) {
                    n += 1;
                }
            }
            n
        };

        use report::Button::{BumperR1, Steam};
        assert_eq!(
            pulses(&[(&[], 0), (&[Steam], 4), (&[], 120), (&[], 124)]),
            1,
            "a quick tap pulses on exactly the release frame"
        );
        assert_eq!(
            pulses(&[(&[], 0), (&[Steam], 4), (&[Steam, BumperR1], 8), (&[], 60)]),
            0,
            "guide+r1 changed workspace; the game hears nothing"
        );
        assert_eq!(
            pulses(&[(&[], 0), (&[Steam], 4), (&[], 1_500)]),
            0,
            "a hold the owner spent deliberating is not a tap"
        );
    }

    #[test]
    fn gamepad_gate_yields_to_a_forced_desktop_layer() {
        // The manual mode override (`Action::SetMode "desktop"`, docs/13
        // decision #4) is the successor to `Arbiter::set_force_desktop`. Ranks 3
        // and 4 must stay mutually exclusive: if the desktop is forced live over
        // a game, forwarding yields rather than both driving the pad at once.
        let cfg = Config::load_default();
        let mut m = ModeEngine::new(&cfg);
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        assert!(gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), false, false));

        m.set_mode(&cfg, crate::mode::BUILTIN_DESKTOP);
        assert!(m.cursor_enabled() && !m.desktop_yielded());
        assert!(!gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), false, false));

        // Clearing the override hands the pad straight back to the game.
        m.clear_mode(&cfg);
        assert!(gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), false, false));
    }

    #[test]
    fn a_mode_change_releases_a_held_bare_button_key() {
        // The mode-transition handoff must not strand a key: `reconcile` drops
        // anything whose binding is no longer live, and `release_all` does it
        // without waiting for another report to arrive.
        let cfg = Config::load_default();
        let mut m = ModeEngine::new(&cfg);
        let mut keys = live();
        let up = |b: report::Button| b == report::Button::DpadUp;

        // Desktop: D-pad up is bound, press it.
        let down = keys.reconcile(m.buttons(), up, none, true);
        assert_eq!(down, vec![(report::Button::DpadUp, KeyChord::plain(103), true)]);

        // A game takes focus. The map goes empty, so the held key releases even
        // though the button is still physically down.
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        let released = keys.reconcile(m.buttons(), up, up, true);
        assert_eq!(released, vec![(report::Button::DpadUp, KeyChord::plain(103), false)]);
        assert!(keys.held.is_empty());
    }

    #[test]
    fn release_all_drops_every_held_key_with_no_keyboard() {
        // `release_all` runs on the transition and disconnect paths, where the
        // uinput keyboard and the virtual pointer may legitimately be absent;
        // it must still clear the held set — mouse codes included, with no
        // pointer to send their release to.
        use ButtonAction::Hold;
        let mut keys = live();
        let map = HashMap::from([
            (report::Button::DpadUp, Hold(103.into())),
            (report::Button::A, Hold(28.into())),
            (report::Button::TriggerR2Full, Hold(PointerButton::Left.evdev().into())),
            (report::Button::TriggerL2Full, Hold(PointerButton::Right.evdev().into())),
        ]);
        keys.reconcile(&map, |_| true, none, true);
        assert_eq!(keys.held.len(), 4);
        keys.release_all(&mut None, None);
        assert!(keys.held.is_empty());

        // The same with no devices on the per-frame path: `drive_buttons` must
        // not bail out when the keyboard is missing, because the pointer may
        // still be there (and vice versa) — the held set tracks either way.
        let mut keys = live();
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let bit = |b: report::Button| -> u32 {
            (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit")
        };
        let frame = report::Frame {
            buttons: (1 << bit(report::Button::TriggerR2Full)) | (1 << bit(report::Button::DpadUp)),
            ..report::Frame::default()
        };
        let idle = report::Frame::default();
        drive_buttons(&mut kbd, None, &frame, &idle, &map, &mut keys, true, &mut hx);
        assert_eq!(keys.held.len(), 2);
        drive_buttons(&mut kbd, None, &idle, &frame, &map, &mut keys, true, &mut hx);
        assert!(keys.held.is_empty());
    }

    /// Drive `st` through one frame at the given gate, with no real device
    /// (`tried` pre-set so nothing is created) and a haptics handle that opens
    /// nothing until a pulse is queued.
    fn step(st: &mut GamepadState, hap: &mut Haptics, forwarding: bool, now: Instant) {
        drive_gamepad(
            st,
            &report::Frame::default(),
            &[],
            &GamepadConfig::default(),
            forwarding,
            false, // no guide tap; `guide_tap_pulse` has its own tests
            hap,
            now,
            false,
        );
    }

    /// What `status.json` reports is what a game would actually receive —
    /// which is not the same as what the config asked for.
    #[test]
    fn the_published_relay_is_the_sink_that_really_exists() {
        let xbox = GamepadConfig::default();
        assert_eq!(xbox.kind, GamepadKind::Xbox);
        assert_eq!(relay_kind(&xbox, false), RelayKind::Xbox);

        let steam = GamepadConfig { kind: GamepadKind::Steam, ..GamepadConfig::default() };
        assert_eq!(relay_kind(&steam, true), RelayKind::Steam);
        // The case on this machine today: /dev/uhid is root-only, so the relay
        // never came up. Report "none" — never silently fall back to the Xbox
        // pad, which is a different controller than the config asked for.
        assert_eq!(relay_kind(&steam, false), RelayKind::None);

        // The master switch wins over both.
        let off = GamepadConfig { enabled: false, ..GamepadConfig::default() };
        assert_eq!(relay_kind(&off, false), RelayKind::None);
        let off_steam =
            GamepadConfig { enabled: false, kind: GamepadKind::Steam, ..GamepadConfig::default() };
        assert_eq!(relay_kind(&off_steam, true), RelayKind::None);
    }

    /// The relay is never even attempted unless the config asks for it, so a
    /// default install never touches `/dev/uhid`.
    #[test]
    fn no_relay_is_started_unless_the_config_asks_for_one() {
        assert!(start_relay(&GamepadConfig::default()).is_none(), "kind = xbox");
        let off =
            GamepadConfig { enabled: false, kind: GamepadKind::Steam, ..GamepadConfig::default() };
        assert!(start_relay(&off).is_none(), "the master switch is off");
    }

    /// The guide is hyprpad's chord modifier, so the relay withholds it unless
    /// the owner hands it back — the same rule, and the same knob, as the Xbox
    /// pad's `BTN_MODE`.
    #[test]
    fn the_strip_mask_follows_the_forward_guide_knob() {
        let d = GamepadConfig::default();
        assert!(!d.forward_guide);
        assert_eq!(strip_mask(&d), StripMask { guide: true, quick_access: false });
        let fwd = GamepadConfig { forward_guide: true, ..GamepadConfig::default() };
        assert_eq!(strip_mask(&fwd), StripMask { guide: false, quick_access: false });
    }

    #[test]
    fn gamepad_tracks_the_forwarding_edge_for_neutral_on_transition() {
        let mut st = GamepadState { tried: true, ..GamepadState::new() };
        let mut hap = Haptics::new();
        let t = Instant::now();

        // Nothing focused: no edge to report, and nothing is claimed.
        step(&mut st, &mut hap, false, t);
        assert!(!st.forwarding);
        // A game takes focus: forwarding latches on.
        step(&mut st, &mut hap, true, t);
        assert!(st.forwarding);
        // Still forwarding next frame — no spurious re-transition.
        step(&mut st, &mut hap, true, t);
        assert!(st.forwarding);
        // Guide goes down (or the OSK comes up, or focus leaves — the gate does
        // not say which): the falling edge fires exactly once and latches off.
        step(&mut st, &mut hap, false, t);
        assert!(!st.forwarding, "the falling edge must release the pad");
        step(&mut st, &mut hap, false, t);
        assert!(!st.forwarding, "and must not re-fire while still released");
        // Guide comes back up mid-game: forwarding resumes.
        step(&mut st, &mut hap, true, t);
        assert!(st.forwarding);
        // `release` is the out-of-band path (disconnect / daemon exit) and does
        // the same thing without waiting for a frame.
        st.release(&mut hap);
        assert!(!st.forwarding);
    }

    #[test]
    fn rumble_mode_change_stops_the_running_rumble_in_its_own_form() {
        // A live reload that flips `rumble_mode` under a running rumble must not
        // strand it: the new form has no stop for what the old one started.
        let mut st = GamepadState { tried: true, ..GamepadState::new() };
        let mut hap = Haptics::new();
        st.rumble = (40_000, 10_000);
        st.rumble_sent = Some(Instant::now());
        assert_eq!(st.rumble_mode, RumbleMode::Native);

        let pulse = GamepadConfig { rumble_mode: RumbleMode::Pulse, ..GamepadConfig::default() };
        drive_rumble(&mut st, &pulse, false, &mut hap, Instant::now());
        assert_eq!(st.rumble, (0, 0), "the old form's rumble must be stopped");
        assert_eq!(st.rumble_mode, RumbleMode::Pulse, "and the new form adopted");
        assert!(st.rumble_sent.is_none());
    }

    #[test]
    fn rumble_is_coalesced_at_20hz_but_stops_immediately() {
        let t = Instant::now();
        // Nothing commanded and nothing outstanding: no write at all.
        assert!(!rumble_due((0, 0), (0, 0), None, t));
        // The first non-zero command goes out at once.
        assert!(rumble_due((30_000, 0), (0, 0), None, t));
        // A change inside the window waits — the kernel caches magnitudes the
        // same way; writing back-to-back makes the controller stutter.
        assert!(!rumble_due((40_000, 0), (30_000, 0), Some(t), t));
        // Once the window elapses it goes out, change or not: a *non-zero*
        // rumble needs the 50 ms refresh or the controller lets it lapse.
        let later = t + RUMBLE_INTERVAL;
        assert!(rumble_due((40_000, 0), (30_000, 0), Some(t), later));
        assert!(rumble_due((30_000, 0), (30_000, 0), Some(t), later));
        // A stop never waits for the clock — a stuck rumble is the one failure a
        // player actually feels.
        assert!(rumble_due((0, 0), (30_000, 0), Some(t), t));
        // But a stop is sent once, not on every subsequent frame.
        assert!(!rumble_due((0, 0), (0, 0), Some(t), later));
    }

    #[test]
    fn pulse_train_fills_one_refresh_window_and_scales_with_magnitude() {
        // Silence produces no train at all: an un-refreshed train simply runs
        // out, so "stop" here is the absence of a write.
        assert_eq!(pulse_train(0), None);

        let (on_min, off_min, count_min) = pulse_train(1).unwrap();
        let (on_max, off_max, count_max) = pulse_train(u16::MAX).unwrap();
        // Strength is pulse width on this device, so magnitude spans the
        // texture..click range and nothing else changes shape.
        assert_eq!(on_min, PULSE_ON_MIN);
        assert_eq!(on_max, PULSE_ON_MAX);
        assert!(on_max > on_min, "a harder rumble must be a wider pulse");
        // 50 % duty in both cases.
        assert_eq!((on_min, off_min), (on_min, on_min));
        assert_eq!((on_max, off_max), (on_max, on_max));
        // And every train fills one refresh window, so consecutive windows run
        // together into a continuous buzz rather than a string of ticks.
        for (on, off, count) in [(on_min, off_min, count_min), (on_max, off_max, count_max)] {
            let span_us = u32::from(count) * (u32::from(on) + u32::from(off));
            let window_us = RUMBLE_INTERVAL.as_micros() as u32;
            assert!(
                span_us > window_us / 2 && span_us <= window_us,
                "a {on}/{off}x{count} train spans {span_us} µs of a {window_us} µs window"
            );
        }
        // A narrower pulse needs more cycles to fill the same window, and every
        // train has at least one cycle (a count of 0 would be a silent write).
        assert!(count_min > count_max);
        assert!(count_max >= 1);
    }

    // --- the caret scrub ---------------------------------------------------

    /// A point on the pad at radius `r` (normalized) and `deg` around the
    /// centre, in raw counts — what a thumb tracing a circle reports.
    fn pad_at(r: f64, deg: f64) -> report::Pad {
        let a = deg.to_radians();
        report::Pad {
            x: (r * a.cos() * 32767.0) as i16,
            y: (r * a.sin() * 32767.0) as i16,
            force: 0,
        }
    }

    /// A frame with the left pad touched at `(r, deg)`, plus any extra buttons.
    fn scrub_frame(r: f64, deg: f64, extra: &[report::Button]) -> report::Frame {
        let mut btns = vec![report::Button::PadLeftTouch];
        btns.extend_from_slice(extra);
        report::Frame { left_pad: pad_at(r, deg), ..frame_of(&btns) }
    }

    /// A scrub whose ladder cannot climb, so a test about direction or count
    /// is not also a test about speed.
    fn one_x_pacer() -> JogPacer {
        JogPacer::new(f64::MAX, 0.0, 2, true)
    }

    /// Run a pacer's rate estimator to steady state at `deg_per_s` (250 Hz,
    /// 0.8 s — many time constants of its low-pass).
    fn settle(p: &mut JogPacer, deg_per_s: f64) {
        for _ in 0..200 {
            p.observe((deg_per_s * 0.004).to_radians(), 0.004);
        }
    }

    #[test]
    fn a_scrub_chord_is_spelled_the_way_a_config_would_spell_it() {
        // The four codes are constants here for speed; this pins each against
        // the config's own name table, so a rename there cannot leave the
        // scrub tapping something else.
        let parse = |s: &str| KeyChord::parse(s).expect("a key name the config knows");
        assert_eq!(scrub_chord(true, false, false), parse("right"));
        assert_eq!(scrub_chord(false, false, false), parse("left"));
        assert_eq!(scrub_chord(true, true, false), parse("ctrl+right"));
        assert_eq!(scrub_chord(false, true, false), parse("ctrl+left"));
        assert_eq!(scrub_chord(false, false, true), parse("shift+left"));
        assert_eq!(scrub_chord(true, false, true), parse("shift+right"));
        assert_eq!(scrub_chord(false, true, true), parse("ctrl+shift+left"));
        assert_eq!(scrub_chord(true, true, true), parse("ctrl+shift+right"));
    }

    #[test]
    fn a_scrub_detent_taps_and_never_holds() {
        // The whole reason the scrub exists: a tap per detent, pressed and
        // released in the same frame, so no client repeat timer is ever armed
        // and a daemon that dies mid-spin leaves nothing down
        // (docs/research/text-scrub.md §1.3). Combos bracket their key exactly
        // as `chord_edges` does for a held binding.
        let right = KeyChord::parse("right").unwrap();
        let mut pacer = one_x_pacer();
        let burst = scrub_burst(-1, &mut pacer, false);
        assert_eq!(burst.events, vec![(right, true), (right, false)]);
        assert!(!burst.word);

        // A word step is one ctrl+arrow, and its edges expand modifier-first,
        // key-last-out — the order the combo code uses everywhere.
        let ctrl_left = KeyChord::parse("ctrl+left").unwrap();
        let mut fast = JogPacer::new(360.0, 180.0, 1, true);
        settle(&mut fast, 700.0);
        fast.detent();
        fast.detent();
        let word = scrub_burst(1, &mut fast, false);
        assert_eq!(word.events, vec![(ctrl_left, true), (ctrl_left, false)]);
        assert!(word.word, "a word step asks for the heavier feel");
        assert_eq!(chord_edges(&ctrl_left, true), vec![(29, true), (105, true)]);
        assert_eq!(chord_edges(&ctrl_left, false), vec![(105, false), (29, false)]);
    }

    #[test]
    fn a_scrub_step_is_n_whole_taps_of_one_key() {
        // ×2 is two complete taps, not one longer press: the count is what the
        // thumb feels and what the client counts.
        let left = KeyChord::parse("left").unwrap();
        // Fast enough for ×2 (360°/s), well under the ×4 rung's 720°/s.
        let mut p = JogPacer::new(360.0, 180.0, 1, false);
        settle(&mut p, 500.0);
        p.detent(); // arms ×2
        let burst = scrub_burst(1, &mut p, false);
        assert_eq!(
            burst.events,
            vec![(left, true), (left, false), (left, true), (left, false)]
        );
        assert!(!burst.word, "characters keep the light tick");
    }

    #[test]
    fn a_held_select_button_puts_shift_around_every_tap() {
        let shift_right = KeyChord::parse("shift+right").unwrap();
        let mut pacer = one_x_pacer();
        let burst = scrub_burst(-2, &mut pacer, true);
        assert_eq!(
            burst.events,
            vec![
                (shift_right, true),
                (shift_right, false),
                (shift_right, true),
                (shift_right, false)
            ]
        );
    }

    #[test]
    fn a_slow_clockwise_revolution_is_exactly_twenty_four_right_taps() {
        // The jog wheel's contract: 24 detents per revolution at 15°, one
        // character each at the landing tier, and never a tap the other way.
        let mut angle = AngleAccumulator::new(15f64.to_radians(), 0.35);
        let mut pacer = one_x_pacer();
        let mut events = Vec::new();
        // A full turn clockwise in 1° steps, at a safe radius.
        for i in 0..=360 {
            let ticks = angle.update(
                0.6 * f64::from(-i).to_radians().cos(),
                0.6 * f64::from(-i).to_radians().sin(),
            );
            events.extend(scrub_burst(ticks, &mut pacer, false).events);
        }
        let right = KeyChord::parse("right").unwrap();
        assert!(
            events.iter().all(|&(c, _)| c == right),
            "clockwise is Right, and only Right"
        );
        let taps = events.iter().filter(|&&(_, pressed)| pressed).count();
        assert_eq!(taps, 24, "360° / 15° = 24 characters");
        assert_eq!(events.len(), 48, "each tap is a press and a release");

        // And the same turn the other way is 24 Lefts.
        let mut angle = AngleAccumulator::new(15f64.to_radians(), 0.35);
        let mut pacer = one_x_pacer();
        let mut ccw = Vec::new();
        for i in 0..=360 {
            let ticks = angle.update(
                0.6 * f64::from(i).to_radians().cos(),
                0.6 * f64::from(i).to_radians().sin(),
            );
            ccw.extend(scrub_burst(ticks, &mut pacer, false).events);
        }
        let left = KeyChord::parse("left").unwrap();
        assert!(ccw.iter().all(|&(c, _)| c == left));
        assert_eq!(ccw.iter().filter(|&&(_, p)| p).count(), 24);
    }

    #[test]
    fn reversing_direction_costs_a_full_detent() {
        // The accumulator carries the sub-detent remainder, so turning back
        // needs a whole step before the opposite key fires: a thumb parked on a
        // boundary and breathing cannot chatter Left/Right.
        let mut angle = AngleAccumulator::new(15f64.to_radians(), 0.35);
        let mut pacer = one_x_pacer();
        let mut events = Vec::new();
        let push = |deg: f64, angle: &mut AngleAccumulator, pacer: &mut JogPacer, out: &mut Vec<(KeyChord, bool)>| {
            let a = deg.to_radians();
            let ticks = angle.update(0.6 * a.cos(), 0.6 * a.sin());
            out.extend(scrub_burst(ticks, pacer, false).events);
        };
        // 16° counter-clockwise: one Left.
        push(0.0, &mut angle, &mut pacer, &mut events);
        push(16.0, &mut angle, &mut pacer, &mut events);
        assert_eq!(events.iter().filter(|&&(_, p)| p).count(), 1);
        // Now back 14°: still inside the detent that just fired — nothing.
        push(2.0, &mut angle, &mut pacer, &mut events);
        assert_eq!(events.iter().filter(|&&(_, p)| p).count(), 1, "no double fire");
        // Two more degrees back completes a whole reverse detent: one Right.
        push(0.0, &mut angle, &mut pacer, &mut events);
        let taps: Vec<KeyChord> =
            events.iter().filter(|&&(_, p)| p).map(|&(c, _)| c).collect();
        assert_eq!(
            taps,
            vec![KeyChord::parse("left").unwrap(), KeyChord::parse("right").unwrap()]
        );
    }

    #[test]
    fn the_first_scrub_detent_consumes_the_guide_hold() {
        // A pad touch is not chordable, so without this the release of every
        // caret fix would reach Steam as a bare guide tap. Driven through the
        // real handler, gate and all.
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let cursor = CursorConfig::default();
        let mut st = ScrubState::new(&cfg, &cursor);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut engine = GestureEngine::new();
        let t = Instant::now();

        // Guide down: the layer opens with the hold unspent.
        let guide = frame_of(&[report::Button::Steam]);
        assert!(engine
            .update(&guide, t)
            .iter()
            .any(|e| matches!(e, GestureEvent::GuideEnter)));

        // Circle the pad for a quarter turn, a frame every 4 ms.
        let mut now = t;
        for i in 0..=120 {
            now = t + Duration::from_millis(4 * i);
            let f = scrub_frame(0.6, f64::from(i as i32) * -0.75, &[report::Button::Steam]);
            let _ = engine.update(&f, now);
            drive_scrub(
                &mut kbd, &mut engine, &f, &mut st, &cfg, &cursor, true, &mut hx, now,
            );
        }
        assert!(st.consumed, "a detent must spend the hold");

        // Releasing the guide is therefore NOT a bare tap.
        let up = report::Frame::default();
        assert_eq!(
            engine.update(&up, now + Duration::from_millis(4)),
            vec![GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }]
        );
    }

    #[test]
    fn a_resting_thumb_under_the_guide_leaves_the_tap_alone() {
        // The other half of the rule: touching the pad without turning it is
        // not a scrub, so a `guide_tap` binding still fires. Same frames, no
        // rotation.
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let cursor = CursorConfig::default();
        let mut st = ScrubState::new(&cfg, &cursor);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut engine = GestureEngine::new();
        let t = Instant::now();
        let _ = engine.update(&frame_of(&[report::Button::Steam]), t);
        let mut now = t;
        for i in 0..=50 {
            now = t + Duration::from_millis(4 * i);
            let f = scrub_frame(0.6, 30.0, &[report::Button::Steam]);
            let _ = engine.update(&f, now);
            drive_scrub(
                &mut kbd, &mut engine, &f, &mut st, &cfg, &cursor, true, &mut hx, now,
            );
        }
        assert!(!st.consumed, "a still thumb emits nothing and spends nothing");
        assert_eq!(
            engine.update(&report::Frame::default(), now + Duration::from_millis(4)),
            vec![GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }]
        );
    }

    #[test]
    fn the_scrub_gate_drops_every_carry() {
        // Guide released, guarded out of the mode, the keyboard taking the
        // pads, or the thumb lifting: each closes the gate, and a closed gate
        // forgets the spin so nothing carries into the next one.
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let cursor = CursorConfig::default();
        let mut st = ScrubState::new(&cfg, &cursor);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut engine = GestureEngine::new();
        let t = Instant::now();
        let _ = engine.update(&frame_of(&[report::Button::Steam]), t);
        for i in 0..=200 {
            let now = t + Duration::from_millis(4 * i);
            let f = scrub_frame(0.6, f64::from(i as i32) * -2.0, &[report::Button::Steam]);
            drive_scrub(
                &mut kbd, &mut engine, &f, &mut st, &cfg, &cursor, true, &mut hx, now,
            );
        }
        assert!(st.consumed && st.pacer.speed() > 0.0, "the spin was live");

        // The guide comes up: `active` goes false and everything is dropped.
        let now = t + Duration::from_millis(1000);
        drive_scrub(
            &mut kbd,
            &mut engine,
            &report::Frame::default(),
            &mut st,
            &cfg,
            &cursor,
            false,
            &mut hx,
            now,
        );
        assert!(!st.consumed, "a fresh hold gets to spend itself again");
        assert_eq!(st.pacer.speed(), 0.0, "no speed carries into the next spin");
        assert!(st.last_t.is_none());

        // And a config that never asked for a scrub is inert even with the
        // gate open — the section is off by default.
        let off = ScrubConfig::default();
        let mut idle = ScrubState::new(&off, &cursor);
        for i in 0..=200 {
            let now = t + Duration::from_millis(4 * i);
            let f = scrub_frame(0.6, f64::from(i as i32) * -2.0, &[report::Button::Steam]);
            drive_scrub(
                &mut kbd, &mut engine, &f, &mut idle, &off, &cursor, true, &mut hx, now,
            );
        }
        assert!(!idle.consumed, "an unconfigured scrub does nothing at all");
    }

    // --- the caret shuttle, for a controller with no pads ------------------

    /// A frame from the padless backend with the left stick held at `x`
    /// (normalised) and any extra buttons down. `Source::Evdev` is the whole of
    /// "this controller has no trackpads" as far as every handler is concerned.
    fn shuttle_frame(x: f64, extra: &[report::Button]) -> report::Frame {
        let mut btns = vec![report::Button::Steam];
        btns.extend_from_slice(extra);
        report::Frame {
            source: report::Source::Evdev,
            left_stick: ((x * 32767.0) as i16, 0),
            ..frame_of(&btns)
        }
    }

    /// The scrub's world for a test: state, a null keyboard, null haptics.
    fn shuttle_rig(cfg: &ScrubConfig) -> (ScrubState, GestureEngine, Haptics, HapticsConfig) {
        (
            ScrubState::new(cfg, &CursorConfig::default()),
            GestureEngine::new(),
            Haptics::new(),
            HapticsConfig::default(),
        )
    }

    #[test]
    fn a_padless_source_shuttles_and_a_padded_one_keeps_the_wheel() {
        // The switch the whole feature turns on: one binding, and the *device*
        // picks the gesture. A stick held over on an Xbox frame walks the
        // caret; the same deflection on a Steam Controller frame does nothing
        // at all, because there the sticks are guide flicks and the left PAD is
        // the wheel.
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let cursor = CursorConfig::default();
        let (mut st, mut engine, mut hap, hcfg) = shuttle_rig(&cfg);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::Evdev };
        let t = Instant::now();
        let _ = engine.update(&frame_of(&[report::Button::Steam]), t);

        // Half over to the right, for a second of the loop's 4 ms clock.
        let right = KeyChord::parse("right").unwrap();
        let mut taps = Vec::new();
        for i in 0..=250 {
            let now = t + Duration::from_millis(4 * i);
            let f = shuttle_frame(0.5, &[]);
            taps.extend(drive_shuttle(&mut kbd, &mut engine, &f, &mut st, &cfg, &mut hx, now));
        }
        assert!(taps.iter().all(|&c| c == right), "pushing right taps Right");
        // The rate at half deflection: 4/s at the deadzone edge rising to 25/s
        // at the word threshold, so ~14/s here (plus the immediate first tap).
        assert!(
            (14..=16).contains(&taps.len()),
            "a second at half deflection tapped {} times",
            taps.len()
        );
        assert!(st.consumed, "the first tap spends the guide hold");
        assert!(st.shuttle_running(), "and the loop still owes it a wakeup");

        // The same frame from the Steam Controller: the sticks are not the
        // scrub's there, and the wheel wants a pad touch it is not getting.
        let (mut st, mut engine, mut hap, hcfg) = shuttle_rig(&cfg);
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        for i in 0..=250 {
            let now = t + Duration::from_millis(4 * i);
            let f = report::Frame {
                source: report::Source::SteamController,
                ..shuttle_frame(0.5, &[])
            };
            drive_scrub(
                &mut kbd, &mut engine, &f, &mut st, &cfg, &cursor, true, &mut hx, now,
            );
        }
        assert!(!st.consumed, "a padded controller's left stick is not a shuttle");
        assert!(!st.shuttle_running(), "and it must never hold the deadline open");
    }

    #[test]
    fn the_shuttle_takes_shift_from_select_and_words_from_the_far_end() {
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let (mut st, mut engine, mut hap, hcfg) = shuttle_rig(&cfg);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::Evdev };
        let t = Instant::now();
        let _ = engine.update(&frame_of(&[report::Button::Steam]), t);

        // Held select (`l5` by default) puts Shift around the tap, exactly as
        // it does on the wheel: the same motion selects instead of moving.
        let f = shuttle_frame(-0.5, &[cfg.select]);
        assert_eq!(
            drive_shuttle(&mut kbd, &mut engine, &f, &mut st, &cfg, &mut hx, t),
            Some(KeyChord::parse("shift+left").unwrap())
        );
        // Past `word_above` the unit changes and the modifier stays.
        let far = shuttle_frame(0.95, &[cfg.select]);
        assert_eq!(
            drive_shuttle(
                &mut kbd,
                &mut engine,
                &far,
                &mut st,
                &cfg,
                &mut hx,
                t + Duration::from_millis(4)
            ),
            Some(KeyChord::parse("ctrl+shift+right").unwrap()),
            "a reversal taps at once, and the far end taps words"
        );
        // ...and without the button it is the bare arrow again.
        let (mut st, mut engine, mut hap, hcfg) = shuttle_rig(&cfg);
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::Evdev };
        assert_eq!(
            drive_shuttle(
                &mut kbd,
                &mut engine,
                &shuttle_frame(0.95, &[]),
                &mut st,
                &cfg,
                &mut hx,
                t
            ),
            Some(KeyChord::parse("ctrl+right").unwrap())
        );
    }

    #[test]
    fn releasing_the_guide_parks_the_shuttle() {
        // The gate is the wheel's gate, and it drops the same things: no
        // direction, no schedule, and a fresh hold that gets to spend itself.
        // Nothing can be stranded — the shuttle only ever taps — which is why
        // it needs no place in `release_outputs` either.
        let cfg = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let cursor = CursorConfig::default();
        let (mut st, mut engine, mut hap, hcfg) = shuttle_rig(&cfg);
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::Evdev };
        let t = Instant::now();
        let _ = engine.update(&frame_of(&[report::Button::Steam]), t);
        for i in 0..=50 {
            let now = t + Duration::from_millis(4 * i);
            drive_scrub(
                &mut kbd,
                &mut engine,
                &shuttle_frame(0.9, &[]),
                &mut st,
                &cfg,
                &cursor,
                true,
                &mut hx,
                now,
            );
        }
        assert!(st.consumed && st.shuttle_running());

        // Guide up (or the mode's guard closing, or the keyboard taking over):
        // `active` goes false and the run is parked.
        let now = t + Duration::from_millis(1000);
        drive_scrub(
            &mut kbd,
            &mut engine,
            &shuttle_frame(0.9, &[]),
            &mut st,
            &cfg,
            &cursor,
            false,
            &mut hx,
            now,
        );
        assert!(!st.consumed, "a fresh hold gets to spend itself again");
        assert!(!st.shuttle_running(), "and the loop may block again");

        // A config that never asked for a scrub is inert with the gate wide
        // open, on this controller as much as on the other one.
        let off = ScrubConfig::default();
        let mut idle = ScrubState::new(&off, &cursor);
        for i in 0..=250 {
            let now = t + Duration::from_millis(4 * i);
            drive_scrub(
                &mut kbd,
                &mut engine,
                &shuttle_frame(1.0, &[]),
                &mut idle,
                &off,
                &cursor,
                true,
                &mut hx,
                now,
            );
        }
        assert!(!idle.consumed && !idle.shuttle_running());
    }

    #[test]
    fn a_reload_retunes_the_shuttle_too() {
        // The shuttle is rebuilt with the rest of the state whenever the config
        // it was built from changes, so `hyprpad reload` retunes the curve with
        // nothing to wire in the reload path — and a retune drops the run, so
        // no schedule from the old rates survives into the new ones.
        let cursor = CursorConfig::default();
        let on = ScrubConfig { enabled: true, ..ScrubConfig::default() };
        let mut st = ScrubState::new(&on, &cursor);
        let t = Instant::now();
        st.shuttle.update(1.0, t);
        assert!(st.shuttle_running());
        let faster = ScrubConfig {
            shuttle: crate::config::ShuttleConfig { fast_per_s: 40.0, ..on.shuttle },
            ..on
        };
        st.sync(&faster, &cursor);
        assert!(!st.shuttle_running(), "a retuned shuttle starts parked");
        assert_eq!(st.cfg.shuttle.fast_per_s, 40.0);
    }

    #[test]
    fn a_reload_retunes_the_wheel_in_place() {
        // The scrub reads its knobs per frame, which is the whole of its
        // reload story: no wiring in `apply_reload`, and a retune drops the
        // cross-frame state so a wheel with different notches never carries.
        let cursor = CursorConfig::default();
        let off = ScrubConfig::default();
        let mut st = ScrubState::new(&off, &cursor);
        assert!(!st.cfg.enabled);
        let on = ScrubConfig { enabled: true, detent_deg: 20.0, ..ScrubConfig::default() };
        st.sync(&on, &cursor);
        assert_eq!(st.cfg.detent_deg, 20.0);
        assert!(st.cfg.enabled);
        // Same config again: not a rebuild (the marker survives).
        st.consumed = true;
        st.sync(&on, &cursor);
        assert!(st.consumed, "an unchanged config must not reset the spin");
        // A retuned damper is a rebuild too, since the smoothing feeds the angle.
        st.sync(&on, &CursorConfig { hysteresis: 0.01, ..CursorConfig::default() });
        assert!(!st.consumed);
    }

    #[test]
    fn scroll_reconfigure_applies_new_mode() {
        // Rebuilding from new knobs swaps the scroll mode live.
        let mut st = ScrollState::new(&ScrollConfig::default(), &CursorConfig::default());
        assert_eq!(st.cfg.mode, ScrollMode::Circular);
        let cfg = ScrollConfig { mode: ScrollMode::Swipe, ..ScrollConfig::default() };
        st.reconfigure(&cfg, &CursorConfig::default());
        assert_eq!(st.cfg.mode, ScrollMode::Swipe);
    }

    /// Regression: a chord that CAUSES a mode transition (guide+view opens the
    /// sheet, whose layer flips the mode) is still physically held when the
    /// handoff runs. The handoff must not reset gesture edge state, or the held
    /// chord looks freshly pressed on the next frame and fires again -- which
    /// toggled the sheet / Omarchy menu open-closed in a loop, observed live.
    #[test]
    fn a_held_chord_does_not_refire_across_a_mode_handoff() {
        use report::Button;
        fn bit(b: Button) -> u32 {
            (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit")
        }
        fn frame(buttons: &[Button]) -> report::Frame {
            let mut bits = 0u32;
            for &b in buttons {
                bits |= 1 << bit(b);
            }
            report::Frame { buttons: bits, ..report::Frame::default() }
        }
        let t = Instant::now();
        let mut engine = GestureEngine::new();
        // Guide down (emits GuideEnter -- expected, not a chord).
        let _ = engine.update(&frame(&[Button::Steam]), t);
        let fired = engine.update(&frame(&[Button::Steam, Button::Menu]), t + Duration::from_millis(8));
        assert!(
            fired.iter().any(|e| matches!(e, GestureEvent::GuideChord(Button::Menu))),
            "the chord fires once on the press edge: {fired:?}"
        );

        // The transition the chord caused: the same handoff the loop runs.
        let cc = CursorConfig::default();
        let mut cursor = CursorState::new(&cc);
        let mut scroll = ScrollState::new(&ScrollConfig::default(), &cc);
        let mut osk_route = OskRoute::new(&cc);
        let mut button_keys = ButtonKeys::new();
        let mut keyboard: Option<VirtualKeyboard> = None;
        let mut gamepad = GamepadState { tried: true, ..GamepadState::new() };
        let mut haptics = Haptics::new();
        let mut chord_keys = ChordKeys::new();
        mode_handoff(
            &mut cursor,
            &mut scroll,
            &mut osk_route,
            None,
            &mut button_keys,
            &mut chord_keys,
            &mut keyboard,
            &mut gamepad,
            &mut haptics,
        );

        // Still holding the chord on the very next frame: NOTHING may fire.
        let again = engine.update(&frame(&[Button::Steam, Button::Menu]), t + Duration::from_millis(12));
        assert!(
            !again.iter().any(|e| matches!(e, GestureEvent::GuideChord(_))),
            "held chord re-fired across the handoff: {again:?}"
        );
        assert!(engine.guide_active(), "edge state must survive the handoff");
    }

    #[test]
    fn the_cursor_follows_its_ambient_guard_with_the_guide_up_and_guide_in_with_it_held() {
        // (guide, ambient, under_guide). Guide up: the ambient guard alone
        // decides, whatever `guide_in` says.
        assert!(cursor_active(false, true, false));
        assert!(cursor_active(false, true, true));
        assert!(!cursor_active(false, false, false));
        assert!(!cursor_active(false, false, true));
        // Guide held: only `guide_in` can keep the pad. The default (nothing
        // listed) is today's behaviour — the guide layer takes it away, on the
        // desktop as much as in a game.
        assert!(!cursor_active(true, true, false));
        assert!(!cursor_active(true, false, false));
        // The case this exists for: a game mode that guards the ambient cursor
        // OUT (so the game gets the pad) and lists itself in `guide_in`, so
        // holding the guide turns the pad into a mouse.
        assert!(cursor_active(true, false, true));
        assert!(cursor_active(true, true, true));

        // The same table read off a live mode engine, on the built-in path a
        // TOML config gets. The virtual pad must never see the pad meanwhile:
        // the guide is rank 1, so forwarding is off for as long as it is held.
        let cfg = Config::from_toml_str("[cursor]\nguide_in = [\"game\"]\n").unwrap();
        let mut m = ModeEngine::new(&cfg);
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        assert!(!cursor_active(false, m.cursor_enabled(), m.cursor_guide_enabled()));
        assert!(cursor_active(true, m.cursor_enabled(), m.cursor_guide_enabled()));
        assert!(gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), false, false));
        assert!(!gamepad_forwarding(true, m.forwards(), m.desktop_yielded(), true, false));
        m.focus_changed(&cfg, "foot", "", None);
        assert!(cursor_active(false, m.cursor_enabled(), m.cursor_guide_enabled()));
        assert!(!cursor_active(true, m.cursor_enabled(), m.cursor_guide_enabled()));
    }

    #[test]
    fn a_key_on_a_guide_chord_is_held_until_the_button_or_the_guide_lets_go() {
        use report::Button::*;
        let mut chords = ChordKeys::new();
        // Recognition presses.
        let click = KeyChord::plain(0x110);
        assert_eq!(chords.press(PadRightClick, click), vec![(click, true)]);
        assert_eq!(chords.held.get(&PadRightClick), Some(&click));
        // The button lifting under the guide releases exactly that output,
        // once; a button holding nothing is nothing.
        assert_eq!(chords.release(PadRightClick), Some(click));
        assert_eq!(chords.release(PadRightClick), None);
        assert_eq!(chords.release(BumperR1), None);
        assert!(chords.held.is_empty());

        // Two chords held at once; the guide releasing ends both.
        chords.press(PadRightClick, click);
        chords.press(GripL5, KeyChord::plain(42));
        let mut all: Vec<u16> = chords.drain().iter().map(KeyChord::code).collect();
        all.sort_unstable();
        assert_eq!(all, vec![42, 0x110]);
        assert!(chords.held.is_empty());

        // A press with no release ever seen for that button (a fresh gesture
        // engine after a reconnect) lets the old output go first.
        chords.press(GripL5, KeyChord::plain(42));
        assert_eq!(
            chords.press(GripL5, KeyChord::plain(54)),
            vec![(KeyChord::plain(42), false), (KeyChord::plain(54), true)]
        );
        assert_eq!(chords.held.len(), 1);

        // The mode handoff releases it like a bare button's, with no devices
        // to send to; so does the disconnect path (`release_all` directly).
        let cc = CursorConfig::default();
        let mut cursor = CursorState::new(&cc);
        let mut scroll = ScrollState::new(&ScrollConfig::default(), &cc);
        let mut osk_route = OskRoute::new(&cc);
        let mut button_keys = ButtonKeys::new();
        let mut keyboard: Option<VirtualKeyboard> = None;
        let mut gamepad = GamepadState { tried: true, ..GamepadState::new() };
        let mut haptics = Haptics::new();
        mode_handoff(
            &mut cursor,
            &mut scroll,
            &mut osk_route,
            None,
            &mut button_keys,
            &mut chords,
            &mut keyboard,
            &mut gamepad,
            &mut haptics,
        );
        assert!(chords.held.is_empty(), "the handoff strands nothing");
        chords.press(PadRightClick, click);
        chords.release_all(&mut None, None);
        assert!(chords.held.is_empty(), "nor does a disconnect");
    }

    #[test]
    fn a_mouse_button_on_a_chord_clicks_through_handle_gesture_and_lets_go_on_either_edge() {
        use report::Button::*;
        // The owner's binding, through the real dispatch: gates, guard,
        // resolution, then the hold instead of `perform_action`.
        let cfg = Config::from_toml_str(
            "[bindings]\n\"guide+rpad_click\" = \"mouse left\"\n\"guide+l5\" = \"key leftshift\"\n",
        )
        .expect("parse");
        let hypr = Hypr::detached();
        let mut modes = ModeEngine::new(&cfg);
        let mut osk = OskHandle::new();
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx =
            HapticCtx { dev: &mut hap, cfg: &hcfg, source: report::Source::SteamController };
        let mut chords = ChordKeys::new();
        let mut kbd: Option<VirtualKeyboard> = None;
        // In a game, where the guide-mouse lives: chords are the escape hatch
        // and survive the built-in game mode.
        modes.focus_changed(&cfg, "steam_app_413080", "", None);

        macro_rules! gesture {
            ($ge:expr) => {
                handle_gesture(
                    &hypr,
                    &cfg,
                    &mut modes,
                    &mut osk,
                    report::Source::SteamController,
                    &mut hx,
                    &mut chords,
                    &mut kbd,
                    None,
                    // A game holds focus in this test, so a bare tap is
                    // Steam's, not a binding's.
                    true,
                    $ge,
                )
            };
        }

        // Recognition presses and holds; nothing is performed, no mode moves.
        assert!(!gesture!(GestureEvent::GuideChord(PadRightClick)));
        assert_eq!(
            chords.held.get(&PadRightClick),
            Some(&KeyChord::plain(PointerButton::Left.evdev()))
        );
        // A second chord holds beside it — a modifier on a grip.
        assert!(!gesture!(GestureEvent::GuideChord(GripL5)));
        assert_eq!(chords.held.len(), 2);
        // The pad click lifting under the guide releases the click and only
        // the click.
        assert!(!gesture!(GestureEvent::GuideChordRelease(PadRightClick)));
        assert_eq!(chords.held.len(), 1);
        assert!(chords.held.contains_key(&GripL5));
        // The guide letting go first releases whatever is left.
        assert!(!gesture!(GestureEvent::GuideChord(PadRightClick)));
        assert!(!gesture!(GestureEvent::GuideLeave { was_chorded: true, bare_tap: false }));
        assert!(chords.held.is_empty());
        // A release for a button holding nothing, and a chord that is not a
        // key, hold nothing. (An unbound chord resolves to `None` before it
        // reaches the hold.)
        assert!(!gesture!(GestureEvent::GuideChordRelease(BumperR1)));
        assert!(!gesture!(GestureEvent::GuideChord(BumperR1)));
        assert!(chords.held.is_empty());
        // And a bare tap in a game still belongs to Steam, not to a binding.
        assert!(!gesture!(GestureEvent::GuideLeave { was_chorded: false, bare_tap: true }));
    }

    /// The lock, all the way through the daemon's event arm: a
    /// [`HyprEvent::Locked`] — however it arrived, off the wire or synthesized
    /// by the poll — is an ordinary context change and needs no compositor
    /// round trip to become one (the `Hypr` here is detached; touching it would
    /// fail).
    #[test]
    fn a_lock_event_moves_the_mode_with_no_compositor_round_trip() {
        let c = crate::lua_config::load_str(
            r#"
            local h = hyprpad
            h.mode("locked").when(function(ctx) return ctx.locked end)
            h.mode("desktop")
            h.button("a", h.key "enter"):only_in("desktop")
            "#,
            "test.lua",
        )
        .expect("config should load");
        let hypr = Hypr::detached();
        let mut modes = ModeEngine::new(&c);
        let mut w = FocusWatch::new();
        assert_eq!(modes.active(), "desktop");
        assert_eq!(modes.buttons().len(), 1);

        assert!(update_modes(&mut modes, &c, &hypr, &mut w, HyprEvent::Locked(true)));
        assert_eq!(modes.active(), "locked");
        assert!(modes.buttons().is_empty());
        // A repeat is not a transition, so the caller runs no second handoff.
        assert!(!update_modes(&mut modes, &c, &hypr, &mut w, HyprEvent::Locked(true)));
        assert!(update_modes(&mut modes, &c, &hypr, &mut w, HyprEvent::Locked(false)));
        assert_eq!(modes.active(), "desktop");
        assert_eq!(modes.buttons().len(), 1);
    }

    #[test]
    fn a_chord_release_is_guide_scoped_and_bound_to_nothing() {
        use report::Button::PadRightClick;
        assert!(is_guide_scoped(&GestureEvent::GuideChordRelease(PadRightClick)));
        let cfg = Config::from_toml_str("[bindings]\n\"guide+rpad_click\" = \"mouse left\"\n")
            .unwrap();
        assert_eq!(
            cfg.resolve(&GestureEvent::GuideChordRelease(PadRightClick)),
            Action::None,
            "the release edge carries no binding of its own"
        );
    }
}
