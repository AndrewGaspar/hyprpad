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
//!    guide button is held, in a game as much as on the desktop.
//! 2. **On-screen keyboard** — `osk.is_active()`. It owns both pads.
//! 3. **Game forwarding** — [`drive_gamepad`], when the active mode forwards
//!    and neither layer above is claiming.
//! 4. **Desktop** — [`drive_cursor`] / [`drive_scroll`] / [`fire_buttons`] /
//!    [`drive_buttons`], each gated by its own guard in the active mode.
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
//! | [`drive_cursor`] | `modes.cursor_enabled()` |
//! | [`drive_scroll`] | `modes.scroll_enabled()` |
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

use crate::config::{
    Action, ButtonAction, Config, CursorConfig, GamepadConfig, HapticsConfig, RumbleMode,
    ScrollConfig, ScrollMode, WorkspaceTarget,
};
use crate::filter::{AngleAccumulator, PadDamper};
use crate::gamepad::{self, VirtualGamepad};
use crate::gesture::{GestureEngine, GestureEvent};
// `Pad` is renamed: this module already talks about `report::Pad` (a pad
// *position*), while the haptics one names an *actuator*.
use crate::haptics::{Feel, Haptics, Pad as HapticPad};
use crate::hypr::{Hypr, HyprEvent};
use crate::keyboard::VirtualKeyboard;
use crate::mode::ModeEngine;
use crate::osk::{OskEvent, OskHandle, OskMode, OskPad};
use crate::output::{PointerButton, VirtualPointer};
use crate::status::StatusWriter;
use crate::{hidraw, report, status};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often the reconnect wait re-scans for the controller's return. Bounded so
/// we never busy-spin (the scan sleeps between attempts) yet re-arm within a
/// couple of seconds of the puck reappearing.
const RECONNECT_SCAN_INTERVAL: Duration = Duration::from_millis(1500);

/// One unit of work for the main loop: a controller report, a compositor event,
/// or the signal that the controller's readers have all ended. The source
/// threads funnel into this so the loop can own all mutable state without locks.
enum Input {
    Report(Vec<u8>),
    Compositor(HyprEvent),
    /// An event the on-screen keyboard child reported back over its stdout
    /// channel (see [`crate::osk`]) — currently a key crossing, which the daemon
    /// answers with a haptic tick because it, not the child, owns the device.
    Osk(OskEvent),
    /// Every puck reader thread has exited — the controller went away. The loop
    /// enters its reconnect wait instead of terminating.
    ReadersEnded,
    /// SIGHUP (or `hyprpad reload`) asked us to re-read the config and apply it
    /// live. Delivered by the SIGHUP self-pipe's waiter thread (see
    /// [`install_reload_signal`]) so the async-signal-safe handler only ever
    /// `write`s, and the actual reload runs in the loop's ordinary context.
    Reload,
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

    // Wait for the controller rather than exiting when it isn't there yet. The
    // loop below already survives the puck *leaving*; this makes startup
    // symmetric, so a daemon launched at login (or restarted while the puck is
    // unplugged / asleep on a dead dongle) simply sits until it appears —
    // "leave it running" has to include "start it before the controller".
    let nodes = {
        let mut nodes = hidraw::puck_nodes()?;
        if nodes.is_empty() {
            eprintln!("hyprpad: no Steam Controller puck found (28de:1304); waiting for one…");
            while nodes.is_empty() {
                std::thread::sleep(RECONNECT_SCAN_INTERVAL);
                nodes = hidraw::puck_nodes()?;
            }
            eprintln!("hyprpad: controller found ({} node(s))", nodes.len());
        }
        nodes
    };
    status.set_connected(true);
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

    // Lizard-mode ownership (opt-in). Off by default so we never fight an
    // unmasked Steam that is managing lizard mode itself
    // (docs/experiments/w12-device-denial.md). When enabled, a background thread
    // disables the puck's firmware keyboard/mouse emulation and re-sends
    // periodically to re-cover it across reconnects. It degrades gracefully:
    // failures log a warning and never take the daemon down.
    // Mutable so a live reload can react to `own_lizard` flipping in the config
    // (see `apply_own_lizard_change`). The one-time startup wiring below —
    // restore-on-exit and the periodic ownership loop — is keyed off the value
    // as it stands *now*; reload only does a best-effort one-shot, and a full
    // ownership change wants a restart.
    let mut own_lizard = lizard_ownership_enabled(&config);

    // When we own lizard we must give it back on the way out, or the firmware
    // keyboard/mouse stays dead and the user has no pointer until a power-cycle.
    // Install the signal-driven restore early, so an immediate Ctrl-C is already
    // covered, and pair it with a `_restore` guard that handles the normal-return
    // and panic paths. The two never both fire: the signal path exits the process
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
    eprintln!("hyprpad: {} controller node(s), Hyprland IPC connected", nodes.len());

    if own_lizard {
        eprintln!("hyprpad: lizard-mode ownership on; taking over the puck's firmware kbd/mouse");
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

    spawn_reader_pipeline(&nodes, &tx);

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
    // owns the writable puck node.
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

    // Supervisor loop. Two states: *connected*, where it blocks on `rx`; and
    // *waiting*, entered when the readers all end (controller gone), where it
    // re-scans for the puck on a timer while still draining compositor events —
    // and, crucially, never returns. Hyprland IPC and the virtual pointer stay
    // alive across the gap; only the hidraw reader pipeline is torn down and
    // re-armed. `recv_timeout` with a shrinking deadline guarantees the scan
    // still fires even if compositor events keep arriving.
    let mut waiting = false;
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
                    &mut keyboard,
                    &mut gamepad,
                    &mut haptics,
                );
            }
            continue;
        }

        // The earliest deadline this iteration must wake for: the reconnect
        // re-scan while the controller is away, and the process rescan whenever
        // it is armed. With neither, this is the plain blocking receive it has
        // always been.
        let wake = match (waiting.then_some(next_scan), rescan_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let input = match wake {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if waiting && Instant::now() >= next_scan {
                        if let Some(nodes) = hidraw::puck_readable() {
                            eprintln!("hyprpad: controller reconnected ({} node(s))", nodes.len());
                            if own_lizard {
                                // Re-cover the fresh device now rather than waiting for
                                // the ownership loop's periodic re-send.
                                if let Err(e) = crate::lizard::disable_lizard_mode() {
                                    eprintln!("warning: lizard re-disable on reconnect: {e}");
                                }
                            }
                            spawn_reader_pipeline(&nodes, &tx);
                            status.set_connected(true);
                            waiting = false;
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
                // Same contract for the game: a vanished controller must not
                // leave the virtual pad holding its last frame, or a rumble
                // running with nothing left to stop it.
                gamepad.release(&mut haptics);
                waiting = true;
                next_scan = Instant::now() + RECONNECT_SCAN_INTERVAL;
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
                    _ => "",
                };
                if update_modes(&mut modes, &config, &hypr, &mut watch, ev) {
                    eprintln!("hyprpad: mode -> {}{why}", modes.active());
                    status.set_mode(modes.active());
                    mode_handoff(
                            &mut cursor,
                        &mut scroll,
                        &mut osk_route,
                            pointer.as_mut(),
                        &mut button_keys,
                        &mut keyboard,
                        &mut gamepad,
                        &mut haptics,
                    );
                }
            }
            Input::Osk(OskEvent::Crossed(pad)) => {
                // The child hit-tested a crossing onto a NEW key; we hold the
                // writable puck node, so the tick is fired here. Gating and
                // intensity come off the live config, so a reload retunes it.
                let mut hx = HapticCtx { dev: &mut haptics, cfg: config.haptics() };
                hx.fire(Haptic::Crossing, haptic_pad(pad));
            }
            Input::Report(data) => {
                let Some(frame) = report::Frame::decode(&data) else { continue };
                let now = Instant::now();
                // The live haptics context for this frame: the device handle plus
                // the current `[haptics]` knobs, read fresh so `hyprpad reload`
                // takes effect on the very next pulse.
                let mut hx = HapticCtx { dev: &mut haptics, cfg: config.haptics() };
                let mut mode_changed = false;
                for ge in engine.update(&frame, now) {
                    if debug {
                        eprintln!("[gesture] {ge:?} -> {:?}", config.resolve_in(&ge, modes.state()));
                    }
                    mode_changed |=
                        handle_gesture(&hypr, &config, &mut modes, &mut osk, &mut hx, ge);
                }
                // Bare-button actions that *fire* (`h.button("l5", h.exec …)`):
                // the same actions a chord performs, on a button's press edge.
                // The bare-button gate: off on the guide layer (so guide+l5
                // stays a chord) and while the OSK owns the pads. Read again
                // below for the held bindings, since an action fired here may
                // have raised the keyboard.
                let buttons_active = !engine.guide_active() && !osk.is_active();
                mode_changed |= fire_buttons(
                    &hypr,
                    &config,
                    &mut modes,
                    &mut osk,
                    &frame,
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
                        drive_cursor(ptr, &frame, &mut cursor, true, true, &mut hx, now);
                        drive_scroll(ptr, &frame, &mut scroll, true, true, &mut hx, now);
                    }
                } else if let Some(ptr) = pointer.as_mut() {
                    // Ambient (non-guide) layer: the RIGHT pad drives the cursor
                    // and the LEFT pad drives scroll. The guide layer takes both
                    // pads away; the active mode decides each independently, via
                    // that handler's own guard (`h.cursor { only_in = … }`).
                    drive_cursor(
                        ptr,
                        &frame,
                        &mut cursor,
                        engine.guide_active(),
                        !modes.cursor_enabled(),
                        &mut hx,
                        now,
                    );
                    drive_scroll(
                        ptr,
                        &frame,
                        &mut scroll,
                        engine.guide_active(),
                        !modes.scroll_enabled(),
                        &mut hx,
                        now,
                    );
                }
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
                drive_buttons(
                    &mut keyboard,
                    pointer.as_mut(),
                    &frame,
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
                    config.gamepad(),
                    forwarding,
                    hx.dev,
                    now,
                );
                prev_frame = frame;
            }
        }
    }
    // On the way out: release the pad and silence any rumble, before `Drop`
    // destroys the device and the game sees it unplug.
    gamepad.release(&mut haptics);
    Ok(())
}

/// Arm the hidraw reader pipeline for `nodes`: spawn `read_all`'s per-node
/// reader threads and a forwarder that funnels their reports into the main
/// loop's channel. Used for both the initial connect and every reconnect, so
/// the two paths are identical.
fn spawn_reader_pipeline(nodes: &[PathBuf], tx: &mpsc::Sender<Input>) {
    let reports = hidraw::read_all(nodes);
    let tx = tx.clone();
    std::thread::spawn(move || forward_reports(reports, tx));
}

/// Forward every report from one reader generation into the loop's input
/// channel, then — once they have all ended (the controller went away) — send a
/// single [`Input::ReadersEnded`] so the loop enters its reconnect wait instead
/// of going silently idle. Split out from [`spawn_reader_pipeline`] so the
/// sentinel behaviour is unit-testable without hardware.
fn forward_reports(reports: mpsc::Receiver<hidraw::Report>, tx: mpsc::Sender<Input>) {
    for r in reports {
        if tx.send(Input::Report(r.data)).is_err() {
            return; // main loop gone; nothing to announce to.
        }
    }
    let _ = tx.send(Input::ReadersEnded);
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
    /// An OSK key was committed (pad click, trigger, or an `[osk_buttons]`
    /// helper).
    Commit,
    /// A guide chord or stick flick resolved to an action.
    Gesture,
    /// One circular-scroll detent was emitted.
    Scroll,
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
}

impl HapticCtx<'_> {
    /// Fire `what` on `pad`, if the config enables it. Non-blocking: the pulse is
    /// queued for the haptics writer thread and dropped if that queue is full.
    fn fire(&mut self, what: Haptic, pad: HapticPad) {
        if let Some(feel) = haptic_feel(self.cfg, what) {
            self.dev.play(feel, pad, self.cfg.intensity);
        }
    }
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
/// — a focus change, a rename of the focused window, the periodic process
/// rescan, a manual override from a binding, or a reload.
///
/// Whatever the outgoing mode was holding — a held key or mouse button, the
/// virtual pad's last stick values, a mid-flight gesture — is let go before the
/// incoming mode's gates apply. One function, so a new way of *noticing* a
/// transition can never come with a subtly different way of *making* one.
#[allow(clippy::too_many_arguments)]
fn mode_handoff(
    cursor: &mut CursorState,
    scroll: &mut ScrollState,
    osk_route: &mut OskRoute,
    pointer: Option<&mut VirtualPointer>,
    button_keys: &mut ButtonKeys,
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
    button_keys.release_all(keyboard, pointer);
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

/// Drive the pointer's *motion* from the right trackpad for a single frame.
///
/// Active only on the ambient (non-guide) layer and when the arbiter is not
/// suppressing desktop control: the guide layer owns the pad for workspace
/// gestures, and a focused game owns it for itself. Mouse *buttons* are not
/// this function's business: a pad click or trigger pull is a bare-button
/// binding (`rpad_click = "mouse left"` by default), pressed and released by
/// [`drive_buttons`] under its own gate.
fn drive_cursor(
    ptr: &mut VirtualPointer,
    frame: &report::Frame,
    st: &mut CursorState,
    guide_active: bool,
    suppressed: bool,
    hx: &mut HapticCtx,
    now: Instant,
) {
    if guide_active || suppressed {
        // Release the desktop's claim: forget the tracking origin (all filter
        // state) so re-entry starts clean.
        st.damper.reset();
        st.travel_px = 0.0;
        return;
    }

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

/// Cross-frame state for the *held* bare-button bindings
/// ([`ButtonAction::Hold`]): the evdev codes currently held down, keyed by the
/// controller button holding each. Kept so a gate change (the guide/OSK layer
/// opening, or a game taking focus, while a button is held) releases the key
/// cleanly instead of leaving it stuck down. A fired binding
/// ([`ButtonAction::Fire`]) has no state here: it is over the moment its press
/// edge is.
struct ButtonKeys {
    held: HashMap<report::Button, u16>,
}

impl ButtonKeys {
    fn new() -> ButtonKeys {
        ButtonKeys { held: HashMap::new() }
    }

    /// Reconcile the desired key state against what is held, returning the
    /// `(button, code, pressed)` events to emit and updating the held set. Pure
    /// (no I/O), so the gating and release-on-state-change logic is
    /// unit-testable. The originating button rides along so the caller can fire
    /// feedback on the side of the controller it sits on.
    ///
    /// When `active` is false (guide/OSK), every held key is released.
    /// Otherwise each `Hold`-bound button that is physically down but not yet
    /// held presses its key, and each held key whose button lifted — **or
    /// whose binding has gone away or changed**, which is what a mode
    /// transition or a reload looks like from here — releases. Releases are
    /// emitted before presses. `Fire` bindings are not this function's: they
    /// have no held state ([`fire_edges`]).
    fn reconcile<F: Fn(report::Button) -> bool>(
        &mut self,
        bindings: &HashMap<report::Button, ButtonAction>,
        pressed: F,
        active: bool,
    ) -> Vec<(report::Button, u16, bool)> {
        let mut events = Vec::new();
        // Release anything that should no longer be held: the gate closed, the
        // button lifted, or the binding is no longer live in this mode.
        self.held.retain(|&btn, &mut code| {
            let keep =
                active && pressed(btn) && bindings.get(&btn) == Some(&ButtonAction::Hold(code));
            if !keep {
                events.push((btn, code, false));
            }
            keep
        });
        // Press bound buttons that are down but not yet held — only when open.
        if active {
            for (&btn, what) in bindings {
                let ButtonAction::Hold(code) = *what else { continue };
                if pressed(btn) && !self.held.contains_key(&btn) {
                    events.push((btn, code, true));
                    self.held.insert(btn, code);
                }
            }
        }
        events
    }

    /// Release every held key and mouse button immediately, outside the
    /// per-frame reconcile.
    ///
    /// Used by the mode-transition handoff and the disconnect path: `reconcile`
    /// would get there on the next report anyway, but "no key is stranded
    /// across a mode switch" should not depend on another report arriving —
    /// and after a disconnect none will.
    fn release_all(
        &mut self,
        kbd: &mut Option<VirtualKeyboard>,
        mut pointer: Option<&mut VirtualPointer>,
    ) {
        for (_, code) in self.held.drain() {
            emit_button_code(kbd, pointer.as_deref_mut(), code, false);
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

/// Emit one bare-button edge on the device its code belongs to. A missing
/// device (uinput or the virtual-pointer protocol unavailable) makes the edge a
/// no-op for that code only; the other device keeps working, and the held set
/// is updated regardless so nothing is remembered as down that was never sent.
fn emit_button_code(
    kbd: &mut Option<VirtualKeyboard>,
    pointer: Option<&mut VirtualPointer>,
    code: u16,
    pressed: bool,
) {
    match route(code) {
        Route::Pointer(mb) => {
            if let Some(ptr) = pointer {
                ptr.button(mb, pressed);
            }
        }
        Route::Keyboard => {
            if let Some(kbd) = kbd.as_mut() {
                kbd.key(code, pressed);
            }
        }
    }
}

/// Drive the *held* bare-button bindings ([`ButtonAction::Hold`]) for a single
/// frame: press each bound button's key or mouse button on its down edge and
/// release it on its up edge, subject to `active` (the guide/OSK/suppressed
/// gate, mirroring [`drive_cursor`]). Each code is routed to the device it
/// belongs to ([`route`]); a missing keyboard or pointer silences the codes
/// that would have gone to it and nothing else, so the pad click still clicks
/// on a box with no uinput. The bindings that *fire* are [`fire_buttons`]'s.
///
/// Feedback fires on the **press edge only** ([`Haptic::Button`], off by
/// default): a held D-pad auto-repeats in the kernel — which produces no further
/// events here, so a repeat never buzzes — and a release is not a keystroke.
fn drive_buttons(
    kbd: &mut Option<VirtualKeyboard>,
    mut pointer: Option<&mut VirtualPointer>,
    frame: &report::Frame,
    bindings: &HashMap<report::Button, ButtonAction>,
    st: &mut ButtonKeys,
    active: bool,
    hx: &mut HapticCtx,
) {
    for (btn, code, pressed) in st.reconcile(bindings, |b| frame.pressed(b), active) {
        emit_button_code(kbd, pointer.as_deref_mut(), code, pressed);
        if pressed {
            hx.fire(Haptic::Button, button_pad(btn));
        }
    }
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
    frame: &report::Frame,
    prev: &report::Frame,
    active: bool,
    hx: &mut HapticCtx,
) -> bool {
    let mut mode_changed = false;
    for (btn, action) in fire_edges(modes.buttons(), frame, prev, active) {
        hx.fire(Haptic::Button, button_pad(btn));
        mode_changed |= perform_action(hypr, config, modes, osk, &action);
    }
    mode_changed
}

// ---------------------------------------------------------------------------
// The virtual gamepad: forwarding under focus, and rumble back to the puck.
// ---------------------------------------------------------------------------

/// How often a non-zero rumble command is (re-)written to the puck, and the
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

/// Cross-frame state for the virtual gamepad: the lazily-created device, the
/// edge-detection flag behind the neutral-on-transition guarantee, and the
/// rumble command last written to the puck.
struct GamepadState {
    /// The uinput device. `None` until the first frame that actually wants to
    /// forward — or forever, if creation failed.
    pad: Option<VirtualGamepad>,
    /// Whether creation has been attempted, so a failure warns exactly once
    /// instead of on every frame of every game.
    tried: bool,
    /// Whether the previous frame was forwarding. The falling edge of this flag
    /// is the neutral-on-transition guarantee.
    forwarding: bool,
    /// The `(strong, weak)` magnitudes last written to the puck, and when — the
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

/// Forward one frame to the virtual gamepad, or release it.
///
/// While `forwarding`, every frame becomes one SYN-terminated report on the
/// virtual pad (only the axes and buttons that changed are written). On the
/// frame `forwarding` goes false — for *any* reason: the game lost focus, the
/// guide button went down, the on-screen keyboard came up, the config was
/// reloaded with `enabled = false` — the pad is neutralled exactly once.
///
/// The device is created here rather than at startup, on the first frame that
/// wants it, so a session that never focuses a game never creates one. Games
/// discover it through the usual udev hotplug path.
fn drive_gamepad(
    st: &mut GamepadState,
    frame: &report::Frame,
    cfg: &GamepadConfig,
    forwarding: bool,
    haptics: &mut Haptics,
    now: Instant,
) {
    if forwarding {
        if st.pad.is_none() && !st.tried {
            st.tried = true;
            match VirtualGamepad::new() {
                Ok(pad) => {
                    eprintln!(
                        "hyprpad: virtual gamepad created (045e:028e); forwarding under game focus"
                    );
                    st.pad = Some(pad);
                }
                Err(e) => eprintln!("warning: no virtual gamepad ({e}); games get no input"),
            }
        }
        if let Some(pad) = st.pad.as_mut() {
            pad.apply(frame, cfg.forward_guide);
        }
        st.forwarding = true;
    } else if st.forwarding {
        // The falling edge, and the only place it is handled.
        if let Some(pad) = st.pad.as_mut() {
            pad.neutral();
        }
        st.forwarding = false;
    }
    drive_rumble(st, cfg, forwarding, haptics, now);
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
    let want = match st.pad.as_ref() {
        Some(pad) if forwarding && cfg.rumble => {
            let (strong, weak) = pad.rumble().magnitudes();
            (
                gamepad::scale_magnitude(strong, cfg.rumble_intensity),
                gamepad::scale_magnitude(weak, cfg.rumble_intensity),
            )
        }
        _ => (0, 0),
    };
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

/// Write one rumble command to the puck in the configured form.
fn emit_rumble(haptics: &mut Haptics, mode: RumbleMode, (strong, weak): (u16, u16)) {
    match mode {
        // The puck's own force-feedback report, byte-for-byte what `hid-steam`
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

/// Whether to take ownership of the puck's lizard mode. Enabled by the
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

/// Write end of the self-pipe the SIGHUP handler pokes to request a config
/// reload. `-1` until [`install_reload_signal`] runs. Separate from the
/// lizard restore-on-signal pipe in [`crate::lizard`]: that one drives an exit,
/// this one drives a reload, and SIGHUP must never exit.
static RELOAD_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// The SIGHUP handler. Async-signal-safe by construction: it does nothing but
/// `write` a single byte to the self-pipe so the waiter thread can run the real
/// reload in ordinary thread context. Mirrors [`crate::lizard`]'s
/// `on_exit_signal`, but this pipe feeds a reload, not a process exit.
extern "C" fn on_reload_signal(_sig: libc::c_int) {
    let fd = RELOAD_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1u8;
        // `write` is on POSIX's async-signal-safe list; best-effort, ignore the
        // result — a full pipe just means a reload is already pending.
        let _ = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

/// Install SIGHUP handling that asks the main loop to reload its config.
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
/// lizard mode and exits); SIGHUP is a distinct signal handled here, so the two
/// never contend. `SA_RESTART` keeps a SIGHUP mid-read from erroring out the
/// hidraw reader threads. Best-effort: if the pipe can't be created we log and
/// leave SIGHUP at its default.
fn install_reload_signal(tx: &mpsc::Sender<Input>) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!(
            "warning: config reload-on-SIGHUP not installed (pipe: {})",
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
            // Ordinary thread context now: nudge the loop to reload. If the loop
            // is gone the channel is closed and we stop.
            if tx.send(Input::Reload).is_err() {
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

    // SAFETY: sending SIGHUP to the daemon pid.
    if unsafe { libc::kill(pid, libc::SIGHUP) } != 0 {
        return Err(format!(
            "sending SIGHUP to pid {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    eprintln!("hyprpad: sent SIGHUP to daemon (pid {pid}); it will re-read its config");
    Ok(())
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
fn handle_gesture(
    hypr: &Hypr,
    config: &Config,
    modes: &mut ModeEngine,
    osk: &mut OskHandle,
    hx: &mut HapticCtx,
    ge: GestureEvent,
) -> bool {
    // A bare guide tap belongs to Steam: do nothing so the client sees its own
    // button (it acts on release). Chords and flicks are ours.
    if let GestureEvent::GuideLeave { was_chorded: false } = ge {
        return false;
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

    // Past every gate: the gesture *landed*. Buzz both actuators before acting,
    // so the confirmation is felt at the moment of recognition rather than after
    // the compositor has answered. This is the only feedback the guide layer
    // gives — nothing else about a chord is visible or audible.
    hx.fire(Haptic::Gesture, HapticPad::Both);

    perform_action(hypr, config, modes, osk, &action)
}

/// Perform one resolved action: the tail every binding shares once its own
/// gates have passed, whether it resolved from a guide chord
/// ([`handle_gesture`]) or a bare button's press edge ([`fire_buttons`]) — one
/// function, so the two can never mean subtly different things. Returns
/// whether it moved the active mode (via [`Action::SetMode`] /
/// [`Action::ClearMode`]), which the caller answers with the mode handoff.
fn perform_action(
    hypr: &Hypr,
    config: &Config,
    modes: &mut ModeEngine,
    osk: &mut OskHandle,
    action: &Action,
) -> bool {
    // The keyboard toggle drives the OSK child, not Hyprland: flip show/hide.
    if let Action::ToggleKeyboard { mode, reflow } = action {
        toggle_keyboard(osk, *mode, *reflow);
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
fn toggle_keyboard(osk: &mut OskHandle, mode: OskMode, reflow: bool) {
    if osk.is_active() {
        osk.hide();
    } else {
        osk.show(mode, reflow);
    }
}

/// Cross-frame state for OSK-active pad routing: a smoothing damper plus the
/// last sent (rounded) position for each pad, so identical positions aren't
/// re-sent every 4 ms frame.
struct OskRoute {
    left: OskPadState,
    right: OskPadState,
}

/// One OSK pad's routing state: its smoothing damper and the last position sent
/// on the wire (rounded to the wire's precision so a settled finger stops
/// re-sending). `None` means untouched, so a re-touch always re-sends.
struct OskPadState {
    damper: PadDamper,
    last_sent: Option<(f32, f32)>,
}

impl OskRoute {
    fn new(cfg: &CursorConfig) -> OskRoute {
        OskRoute {
            left: OskPadState::new(cfg),
            right: OskPadState::new(cfg),
        }
    }

    /// Forget both pads (on dismiss), resetting their filter state.
    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
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

/// Route one frame to the on-screen keyboard while it owns the pads:
/// each pad's absolute position becomes a `cursor L|R`, a pad click (or a full
/// trigger pull) commits the key under it, and `B`/`Menu` dismiss the keyboard.
///
/// Every commit gets a haptic click on the committing side ([`Haptic::Commit`]).
/// The lighter per-key-crossing tick is *not* fired here: only the OSK child
/// knows where the key boundaries are, so it reports crossings back and the loop
/// answers them (`Input::Osk`).
fn route_osk(
    osk: &mut OskHandle,
    frame: &report::Frame,
    prev: &report::Frame,
    st: &mut OskRoute,
    osk_buttons: &HashMap<report::Button, u16>,
    hx: &mut HapticCtx,
    now: Instant,
) {
    use report::Button::*;

    // Dismiss on B or Menu (edge-down): hide and leave OSK-active mode.
    if frame.edges_down(prev).any(|b| matches!(b, B | Menu)) {
        osk.hide();
        st.reset();
        return;
    }

    // Move each pad's cursor first, so a click on the same frame commits the key
    // actually under the finger. Only while touched, and only on change.
    route_pad(osk, OskPad::Left, frame.pressed(PadLeftTouch), frame.left_pad, &mut st.left, now);
    route_pad(osk, OskPad::Right, frame.pressed(PadRightTouch), frame.right_pad, &mut st.right, now);

    // Commit on click-down (the Deck commits on click, not release). A full
    // trigger pull on the same side is an alternate commit. Other buttons may be
    // Deck-style helpers (`[osk_buttons]`, e.g. Y = Space, X = Backspace): they
    // tap their key through the OSK's virtual keyboard. Dismiss (B/Menu) and the
    // commit buttons take precedence over a helper binding.
    for b in frame.edges_down(prev) {
        match b {
            PadLeftClick | TriggerL2Full => {
                osk.commit(OskPad::Left);
                hx.fire(Haptic::Commit, HapticPad::Left);
            }
            PadRightClick | TriggerR2Full => {
                osk.commit(OskPad::Right);
                hx.fire(Haptic::Commit, HapticPad::Right);
            }
            _ => {
                if let Some(&code) = osk_buttons.get(&b) {
                    osk.key(code);
                    // A helper (Y = Space, X = Backspace) is a commit too — click
                    // under the hand whose button it is.
                    hx.fire(Haptic::Commit, button_pad(b));
                }
            }
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
        Action::Workspace(t) => match t {
            WorkspaceTarget::Relative(n) => hypr.workspace_relative(*n),
            WorkspaceTarget::Number(n) => hypr.workspace_named(&n.to_string()),
            WorkspaceTarget::Named(s) => hypr.workspace_named(&format!("name:{s}")),
        },
        Action::MoveWindowToWorkspace(t) => match t {
            WorkspaceTarget::Relative(n) => hypr.move_window_to_workspace_relative(*n),
            WorkspaceTarget::Number(n) => {
                hypr.dispatch_raw(&format!("hl.dsp.window.move({{ workspace = \"{n}\" }})")).map(|_| ())
            }
            WorkspaceTarget::Named(s) => hypr
                .dispatch_raw(&format!("hl.dsp.window.move({{ workspace = \"name:{s}\" }})"))
                .map(|_| ()),
        },
        Action::ToggleFullscreen => hypr.toggle_fullscreen(),
        Action::Exec(cmd) => {
            hypr.spawn_env(cmd, &[("HYPRPAD_MODE", mode)]);
            Ok(())
        }
        Action::Dispatch(payload) => hypr.dispatch_raw(payload).map(|_| ()),
        // Handled in `perform_action` against the OSK handle, never reaches here.
        Action::ToggleKeyboard { .. } => Ok(()),
        // Bare-button keys are emitted by `drive_buttons` against the virtual
        // keyboard, not dispatched here; a `key` bound to a guide chord no-ops.
        Action::Key(_) => Ok(()),
        // Handled in `perform_action` against the mode engine, never here.
        Action::SetMode(_) | Action::ClearMode => Ok(()),
        Action::None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        hidraw::Report { node: PathBuf::from("/dev/hidraw0"), data }
    }

    #[test]
    fn forward_reports_emits_readers_ended_when_source_closes() {
        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        rtx.send(report(vec![1, 2, 3])).unwrap();
        rtx.send(report(vec![4])).unwrap();
        drop(rtx); // every reader gone -> the source channel closes

        // Runs to completion: the source is already closed, so it drains the two
        // buffered reports and then announces the readers ended.
        forward_reports(rrx, itx);

        assert!(matches!(irx.recv(), Ok(Input::Report(d)) if d == [1, 2, 3]));
        assert!(matches!(irx.recv(), Ok(Input::Report(d)) if d == [4]));
        assert!(matches!(irx.recv(), Ok(Input::ReadersEnded)));
        // The forwarder dropped its sender, so the loop's channel is now closed.
        assert!(irx.recv().is_err());
    }

    #[test]
    fn forward_reports_stops_without_sentinel_when_loop_gone() {
        let (rtx, rrx) = mpsc::channel::<hidraw::Report>();
        let (itx, irx) = mpsc::channel::<Input>();
        rtx.send(report(vec![9])).unwrap();
        drop(irx); // main loop is gone: sends fail

        // Returns on the failed send; must not panic and must not try to send a
        // sentinel into the dead channel.
        forward_reports(rrx, itx);
        drop(rtx);
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

    #[test]
    fn bare_button_presses_on_down_and_releases_on_up() {
        use report::Button::*;
        use ButtonAction::Hold;
        let bindings = HashMap::from([(DpadUp, Hold(103)), (DpadDown, Hold(108))]);
        let mut st = ButtonKeys::new();
        // DpadUp goes down while active: press KEY_UP, nothing for the unpressed
        // DpadDown. The originating button rides along for the haptic side.
        assert_eq!(
            st.reconcile(&bindings, |b| b == DpadUp, true),
            vec![(DpadUp, 103, true)]
        );
        // Held and still down next frame: no repeated event (the kernel repeats)
        // — which is also why a held arrow cannot buzz per repeat.
        assert!(st.reconcile(&bindings, |b| b == DpadUp, true).is_empty());
        // Lifts: release KEY_UP, held set empties.
        assert_eq!(
            st.reconcile(&bindings, |_| false, true),
            vec![(DpadUp, 103, false)]
        );
        assert!(st.held.is_empty());
    }

    #[test]
    fn bare_button_gate_off_releases_and_suppresses() {
        use report::Button::*;
        let bindings = HashMap::from([(DpadUp, ButtonAction::Hold(103))]);
        let mut st = ButtonKeys::new();
        // Press while active.
        assert_eq!(
            st.reconcile(&bindings, |b| b == DpadUp, true),
            vec![(DpadUp, 103, true)]
        );
        // The gate closes (guide/OSK/game) while the D-pad is still held: the key
        // is released cleanly rather than left stuck down.
        assert_eq!(
            st.reconcile(&bindings, |b| b == DpadUp, false),
            vec![(DpadUp, 103, false)]
        );
        assert!(st.held.is_empty());
        // Still held but gated off: no new press (the chord/game gets it instead).
        assert!(st.reconcile(&bindings, |b| b == DpadUp, false).is_empty());
        // Gate reopens while still held: re-press so the arrow resumes.
        assert_eq!(
            st.reconcile(&bindings, |b| b == DpadUp, true),
            vec![(DpadUp, 103, true)]
        );
    }

    #[test]
    fn bare_button_unbound_ignored_and_multiple_tracked() {
        use report::Button::*;
        use ButtonAction::{Fire, Hold};
        let bindings = HashMap::from([
            (DpadUp, Hold(103)),
            (DpadLeft, Hold(105)),
            (GripL5, Fire(Action::Exec("true".into()))),
        ]);
        let mut st = ButtonKeys::new();
        // An unbound button (A) produces nothing.
        assert!(st.reconcile(&bindings, |b| b == A, true).is_empty());
        // Nor does a button whose binding fires rather than holds: nothing is
        // pressed for it, and nothing is remembered as held.
        assert!(st.reconcile(&bindings, |b| b == GripL5, true).is_empty());
        assert!(st.held.is_empty());
        // Two bound buttons down at once: both press (HashMap order is arbitrary).
        let mut ev = st.reconcile(&bindings, |b| b == DpadUp || b == DpadLeft, true);
        ev.sort_by_key(|&(_, code, _)| code);
        assert_eq!(ev, vec![(DpadUp, 103, true), (DpadLeft, 105, true)]);
        assert_eq!(st.held.len(), 2);
    }

    #[test]
    fn a_fired_binding_fires_once_on_the_press_edge_and_never_while_gated() {
        use report::Button::*;
        use ButtonAction::{Fire, Hold};
        let go = Action::Exec("true".into());
        let bindings = HashMap::from([(GripL5, Fire(go.clone())), (DpadUp, Hold(103))]);
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
        assert_eq!(modes.active(), BUILTIN_DESKTOP);

        // Forcing game mode from the desktop is a change; forcing it again is not.
        let force_game = Action::SetMode(BUILTIN_GAME.into());
        assert!(perform_action(&hypr, &cfg, &mut modes, &mut osk, &force_game));
        assert_eq!(modes.active(), BUILTIN_GAME);
        assert!(!perform_action(&hypr, &cfg, &mut modes, &mut osk, &force_game));
        // Clearing it hands the decision back to the rules (nothing focused:
        // desktop); clearing an override that is not there changes nothing.
        assert!(perform_action(&hypr, &cfg, &mut modes, &mut osk, &Action::ClearMode));
        assert_eq!(modes.active(), BUILTIN_DESKTOP);
        assert!(!perform_action(&hypr, &cfg, &mut modes, &mut osk, &Action::ClearMode));
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
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg };
        let idle = report::Frame::default();
        let l5 = frame_of(&[GripL5]);

        // Under the guide layer the press is a chord's, not ours.
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &l5, &idle, false, &mut hx));
        assert_eq!(modes.active(), BUILTIN_DESKTOP);
        // On the desktop the press edge forces the game mode — and says so.
        assert!(fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &l5, &idle, true, &mut hx));
        assert_eq!(modes.active(), BUILTIN_GAME);
        // Held: nothing more. And in the game the bare buttons are not live at
        // all (the built-in path empties the map), so a fresh press is inert.
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &l5, &l5, true, &mut hx));
        assert!(!fire_buttons(&hypr, &cfg, &mut modes, &mut osk, &l5, &idle, true, &mut hx));
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
            Haptic::Button,
        ] {
            assert_eq!(haptic_feel(&off, what), None, "{what:?} must be silent");
        }
        // A per-trigger toggle silences only its own trigger.
        let no_scroll = HapticsConfig { scroll: false, ..HapticsConfig::default() };
        assert_eq!(haptic_feel(&no_scroll, Haptic::Scroll), None);
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
            Ok(Action::Key(PointerButton::Left.evdev()))
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
        let mut keys = ButtonKeys::new();

        // Desktop: D-pad up is bound, press it.
        let down = keys.reconcile(m.buttons(), |b| b == report::Button::DpadUp, true);
        assert_eq!(down, vec![(report::Button::DpadUp, 103, true)]);

        // A game takes focus. The map goes empty, so the held key releases even
        // though the button is still physically down.
        m.focus_changed(&cfg, "steam_app_413080", "", None);
        let up = keys.reconcile(m.buttons(), |b| b == report::Button::DpadUp, true);
        assert_eq!(up, vec![(report::Button::DpadUp, 103, false)]);
        assert!(keys.held.is_empty());
    }

    #[test]
    fn release_all_drops_every_held_key_with_no_keyboard() {
        // `release_all` runs on the transition and disconnect paths, where the
        // uinput keyboard and the virtual pointer may legitimately be absent;
        // it must still clear the held set — mouse codes included, with no
        // pointer to send their release to.
        use ButtonAction::Hold;
        let mut keys = ButtonKeys::new();
        let map = HashMap::from([
            (report::Button::DpadUp, Hold(103)),
            (report::Button::A, Hold(28)),
            (report::Button::TriggerR2Full, Hold(PointerButton::Left.evdev())),
            (report::Button::TriggerL2Full, Hold(PointerButton::Right.evdev())),
        ]);
        keys.reconcile(&map, |_| true, true);
        assert_eq!(keys.held.len(), 4);
        keys.release_all(&mut None, None);
        assert!(keys.held.is_empty());

        // The same with no devices on the per-frame path: `drive_buttons` must
        // not bail out when the keyboard is missing, because the pointer may
        // still be there (and vice versa) — the held set tracks either way.
        let mut keys = ButtonKeys::new();
        let mut kbd: Option<VirtualKeyboard> = None;
        let mut hap = Haptics::new();
        let hcfg = HapticsConfig::default();
        let mut hx = HapticCtx { dev: &mut hap, cfg: &hcfg };
        let bit = |b: report::Button| -> u32 {
            (0..32)
                .find(|&i| report::Frame { buttons: 1 << i, ..report::Frame::default() }.pressed(b))
                .expect("button has a bit")
        };
        let frame = report::Frame {
            buttons: (1 << bit(report::Button::TriggerR2Full)) | (1 << bit(report::Button::DpadUp)),
            ..report::Frame::default()
        };
        drive_buttons(&mut kbd, None, &frame, &map, &mut keys, true, &mut hx);
        assert_eq!(keys.held.len(), 2);
        drive_buttons(&mut kbd, None, &report::Frame::default(), &map, &mut keys, true, &mut hx);
        assert!(keys.held.is_empty());
    }

    /// Drive `st` through one frame at the given gate, with no real device
    /// (`tried` pre-set so nothing is created) and a haptics handle that opens
    /// nothing until a pulse is queued.
    fn step(st: &mut GamepadState, hap: &mut Haptics, forwarding: bool, now: Instant) {
        drive_gamepad(
            st,
            &report::Frame::default(),
            &GamepadConfig::default(),
            forwarding,
            hap,
            now,
        );
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
        mode_handoff(
            &mut cursor,
            &mut scroll,
            &mut osk_route,
            None,
            &mut button_keys,
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
}
