//! The daemon pipeline: passive controller tap -> gesture recognition ->
//! config lookup -> game-focus gate -> Hyprland dispatch.
//!
//! Two threads feed one loop. `hidraw::read_all` streams raw reports (merged
//! across the puck's pairing slots); `hypr::subscribe` streams compositor
//! events. The main loop owns the [`GestureEngine`], [`Config`], and
//! [`Arbiter`], and executes [`Action`]s via [`Hypr`].

use crate::arbitrate::Arbiter;
use crate::config::{
    Action, Config, CursorConfig, HapticsConfig, ScrollConfig, ScrollMode, WorkspaceTarget,
};
use crate::filter::{AngleAccumulator, PadDamper};
use crate::gesture::{GestureEngine, GestureEvent};
// `Pad` is renamed: this module already talks about `report::Pad` (a pad
// *position*), while the haptics one names an *actuator*.
use crate::haptics::{Feel, Haptics, Pad as HapticPad};
use crate::hypr::{Hypr, HyprEvent};
use crate::keyboard::VirtualKeyboard;
use crate::osk::{OskEvent, OskHandle, OskMode, OskPad};
use crate::output::{PointerButton, VirtualPointer};
use crate::{hidraw, report};

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
    let nodes = hidraw::puck_nodes()?;
    if nodes.is_empty() {
        eprintln!("no Steam Controller puck found (28de:1304)");
        std::process::exit(1);
    }
    // Mutable because SIGHUP / `hyprpad reload` swaps in a freshly loaded config
    // live (see the `Input::Reload` arm and `apply_reload`).
    let mut config = match Config::load() {
        Ok(c) => {
            match Config::config_path() {
                Some(p) if p.exists() => eprintln!("hyprpad: config loaded from {}", p.display()),
                _ => eprintln!("hyprpad: using built-in default config"),
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
    let _pidfile = PidFile::create();

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

    let debug = std::env::var_os("HYPRSC_DEBUG").is_some();
    let mut engine = GestureEngine::new();
    let mut arbiter = Arbiter::new();

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
    loop {
        let input = if waiting {
            match rx.recv_timeout(next_scan.saturating_duration_since(Instant::now())) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => {
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
                        waiting = false;
                    }
                    next_scan = Instant::now() + RECONNECT_SCAN_INTERVAL;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(input) => input,
                Err(_) => break,
            }
        };

        match input {
            Input::ReadersEnded => {
                eprintln!("hyprpad: controller disconnected, waiting for it to return…");
                // Release everything held for the vanished controller — a
                // synthetic click and all smoothing/gesture state — so nothing is
                // stuck down and a stale `prev` cannot jump the cursor on return.
                reset_frame_state(
                    &mut engine,
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    &mut prev_frame,
                    pointer.as_mut(),
                );
                waiting = true;
                next_scan = Instant::now() + RECONNECT_SCAN_INTERVAL;
            }
            Input::Reload => {
                apply_reload(
                    &mut config,
                    &mut cursor,
                    &mut scroll,
                    &mut osk_route,
                    pointer.as_mut(),
                    &mut own_lizard,
                );
            }
            Input::Compositor(ev) => {
                if debug {
                    eprintln!("[event] {ev:?}");
                }
                update_arbiter(&mut arbiter, ev);
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
                for ge in engine.update(&frame, now) {
                    if debug {
                        eprintln!("[gesture] {ge:?} -> {:?}", config.resolve(&ge));
                    }
                    handle_gesture(&hypr, &config, &arbiter, &mut osk, &mut hx, ge);
                }
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
                        config.osk_buttons(),
                        &mut hx,
                        now,
                    );
                    if let Some(ptr) = pointer.as_mut() {
                        drive_cursor(ptr, &frame, &mut cursor, true, true, now);
                        drive_scroll(ptr, &frame, &mut scroll, true, true, &mut hx, now);
                    }
                } else if let Some(ptr) = pointer.as_mut() {
                    // Ambient (non-guide) layer: the RIGHT pad drives the cursor
                    // and the LEFT pad drives scroll. The guide layer and a
                    // focused game both take the pads away. Same gate for both.
                    drive_cursor(ptr, &frame, &mut cursor, engine.guide_active(), arbiter.suppressed(), now);
                    drive_scroll(
                        ptr,
                        &frame,
                        &mut scroll,
                        engine.guide_active(),
                        arbiter.suppressed(),
                        &mut hx,
                        now,
                    );
                }
                // Bare-button bindings (D-pad -> arrows by default). Gated OFF on
                // the guide layer (so guide+dpad stays a chord), while the OSK
                // owns the pads, and in a focused game (there the D-pad reaches
                // the game via the virtual controller) — mirroring `drive_cursor`.
                let buttons_active =
                    !engine.guide_active() && !osk.is_active() && !arbiter.suppressed();
                drive_buttons(
                    &mut keyboard,
                    &frame,
                    config.buttons(),
                    &mut button_keys,
                    buttons_active,
                    &mut hx,
                );
                prev_frame = frame;
            }
        }
    }
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
/// no reports flow in between): drop any held synthetic click, then reset the
/// gesture engine, the cursor/scroll dampers, and the OSK router, and clear the
/// previous frame. Without this a stale `prev` (position or timestamp) would
/// jump the cursor or misfire a gesture on the first frame after the gap.
fn reset_frame_state(
    engine: &mut GestureEngine,
    cursor: &mut CursorState,
    scroll: &mut ScrollState,
    osk_route: &mut OskRoute,
    prev_frame: &mut report::Frame,
    pointer: Option<&mut VirtualPointer>,
) {
    if let Some(ptr) = pointer {
        if cursor.left_down {
            ptr.button(PointerButton::Left, false);
            cursor.left_down = false;
        }
    }
    *engine = GestureEngine::new();
    cursor.damper.reset();
    scroll.reset();
    osk_route.reset();
    *prev_frame = report::Frame::default();
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
    /// Whether the synthetic left mouse button is currently held.
    left_down: bool,
}

impl CursorState {
    fn new(cfg: &CursorConfig) -> CursorState {
        CursorState {
            damper: damper_from(cfg),
            sens: cfg.sens,
            left_down: false,
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

/// Drive the pointer from the right trackpad for a single frame.
///
/// Active only on the ambient (non-guide) layer and when the arbiter is not
/// suppressing desktop control: the guide layer owns the pad for workspace
/// gestures, and a focused game owns it for itself.
fn drive_cursor(
    ptr: &mut VirtualPointer,
    frame: &report::Frame,
    st: &mut CursorState,
    guide_active: bool,
    suppressed: bool,
    now: Instant,
) {
    if guide_active || suppressed {
        // Release the desktop's claim: drop any held click and forget the
        // tracking origin (all filter state) so re-entry starts clean.
        if st.left_down {
            ptr.button(PointerButton::Left, false);
            st.left_down = false;
        }
        st.damper.reset();
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
        }
    } else {
        st.damper.reset();
    }

    // A hard pad click (or a full right-trigger pull) is a left click.
    let want_down = frame.pressed(report::Button::PadRightClick)
        || frame.pressed(report::Button::TriggerR2Full);
    if want_down != st.left_down {
        ptr.button(PointerButton::Left, want_down);
        st.left_down = want_down;
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

/// Cross-frame state for the bare-button keyboard bindings: the evdev keycodes
/// currently held down, keyed by the controller button holding each. Kept so a
/// gate change (the guide/OSK layer opening, or a game taking focus, while a
/// button is held) releases the key cleanly instead of leaving it stuck down.
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
    /// When `active` is false (guide/OSK/suppressed), every held key is
    /// released. Otherwise each bound button that is physically down but not yet
    /// held presses its key, and each held key whose button lifted releases.
    /// Releases are emitted before presses.
    fn reconcile<F: Fn(report::Button) -> bool>(
        &mut self,
        bindings: &HashMap<report::Button, u16>,
        pressed: F,
        active: bool,
    ) -> Vec<(report::Button, u16, bool)> {
        let mut events = Vec::new();
        // Release anything that should no longer be held: the gate closed or the
        // button lifted.
        self.held.retain(|&btn, &mut code| {
            let keep = active && pressed(btn);
            if !keep {
                events.push((btn, code, false));
            }
            keep
        });
        // Press bound buttons that are down but not yet held — only when open.
        if active {
            for (&btn, &code) in bindings {
                if pressed(btn) && !self.held.contains_key(&btn) {
                    events.push((btn, code, true));
                    self.held.insert(btn, code);
                }
            }
        }
        events
    }
}

/// Drive the bare-button keyboard for a single frame: press each bound button's
/// key on its down edge and release it on its up edge, subject to `active` (the
/// guide/OSK/suppressed gate, mirroring [`drive_cursor`]). A missing keyboard
/// (uinput unavailable) makes this a no-op.
///
/// Feedback fires on the **press edge only** ([`Haptic::Button`], off by
/// default): a held D-pad auto-repeats in the kernel — which produces no further
/// events here, so a repeat never buzzes — and a release is not a keystroke.
fn drive_buttons(
    kbd: &mut Option<VirtualKeyboard>,
    frame: &report::Frame,
    bindings: &HashMap<report::Button, u16>,
    st: &mut ButtonKeys,
    active: bool,
    hx: &mut HapticCtx,
) {
    let Some(kbd) = kbd.as_mut() else { return };
    for (btn, code, pressed) in st.reconcile(bindings, |b| frame.pressed(b), active) {
        kbd.key(code, pressed);
        if pressed {
            hx.fire(Haptic::Button, button_pad(btn));
        }
    }
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
    pointer: Option<&mut VirtualPointer>,
    own_lizard: &mut bool,
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

    // Release any held synthetic left click before rebuilding the cursor damper,
    // so a reload mid-drag never strands the button down (the rebuilt state
    // starts with `left_down = false`).
    if let Some(ptr) = pointer {
        if cursor.left_down {
            ptr.button(PointerButton::Left, false);
            cursor.left_down = false;
        }
    }

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

fn update_arbiter(arbiter: &mut Arbiter, ev: HyprEvent) {
    match ev {
        HyprEvent::ActiveWindow { class, .. } => arbiter.focus_changed(&class),
        HyprEvent::Fullscreen(on) => arbiter.set_fullscreen(on),
        _ => {}
    }
}

fn handle_gesture(
    hypr: &Hypr,
    config: &Config,
    arbiter: &Arbiter,
    osk: &mut OskHandle,
    hx: &mut HapticCtx,
    ge: GestureEvent,
) {
    // A bare guide tap belongs to Steam: do nothing so the client sees its own
    // button (it acts on release). Chords and flicks are ours.
    if let GestureEvent::GuideLeave { was_chorded: false } = ge {
        return;
    }

    // Guide-held gestures drive the desktop even over a game — that is the whole
    // point of a global modifier. Only ambient (non-guide) actions would be
    // gated by the arbiter; the current binding set is entirely guide-scoped, so
    // we consult `suppressed()` defensively for any future ambient action.
    let action = config.resolve(&ge);
    if matches!(action, Action::None) {
        return;
    }
    // While the keyboard is up it owns the pads: suppress every desktop gesture
    // except the keyboard toggle itself, so the toggle chord can still dismiss.
    if osk.is_active() && !matches!(action, Action::ToggleKeyboard { .. }) {
        return;
    }
    if arbiter.suppressed() && !is_guide_scoped(&ge) {
        return;
    }

    // Past every gate: the gesture *landed*. Buzz both actuators before acting,
    // so the confirmation is felt at the moment of recognition rather than after
    // the compositor has answered. This is the only feedback the guide layer
    // gives — nothing else about a chord is visible or audible.
    hx.fire(Haptic::Gesture, HapticPad::Both);

    // The keyboard toggle drives the OSK child, not Hyprland: flip show/hide.
    if let Action::ToggleKeyboard { mode } = action {
        toggle_keyboard(osk, mode);
        return;
    }

    if let Err(e) = execute(hypr, &action) {
        eprintln!("dispatch failed: {e}");
    }
}

/// Flip the on-screen keyboard: show it (in `mode`, reflowing workspace content)
/// when hidden, hide it when shown.
fn toggle_keyboard(osk: &mut OskHandle, mode: OskMode) {
    if osk.is_active() {
        osk.hide();
    } else {
        osk.show(mode, true);
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

fn execute(hypr: &Hypr, action: &Action) -> std::io::Result<()> {
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
            hypr.spawn(cmd);
            Ok(())
        }
        Action::Dispatch(payload) => hypr.dispatch_raw(payload).map(|_| ()),
        // Handled in `handle_gesture` against the OSK handle, never reaches here.
        Action::ToggleKeyboard { .. } => Ok(()),
        // Bare-button keys are emitted by `drive_buttons` against the virtual
        // keyboard, not dispatched here; a `key` bound to a guide chord no-ops.
        Action::Key(_) => Ok(()),
        Action::None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn bare_button_presses_on_down_and_releases_on_up() {
        use report::Button::*;
        let bindings = HashMap::from([(DpadUp, 103u16), (DpadDown, 108u16)]);
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
        let bindings = HashMap::from([(DpadUp, 103u16)]);
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
        let bindings = HashMap::from([(DpadUp, 103u16), (DpadLeft, 105u16)]);
        let mut st = ButtonKeys::new();
        // An unbound button (A) produces nothing.
        assert!(st.reconcile(&bindings, |b| b == A, true).is_empty());
        // Two bound buttons down at once: both press (HashMap order is arbitrary).
        let mut ev = st.reconcile(&bindings, |b| b == DpadUp || b == DpadLeft, true);
        ev.sort_by_key(|&(_, code, _)| code);
        assert_eq!(ev, vec![(DpadUp, 103, true), (DpadLeft, 105, true)]);
        assert_eq!(st.held.len(), 2);
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
    fn cursor_reconfigure_applies_sens_and_keeps_held_click() {
        // A live reload rebuilds the damper (fresh filter state, no jump) and
        // applies the new gain, but must not clear a held click — the reload path
        // releases that itself before calling this.
        let mut st = CursorState::new(&CursorConfig::default());
        st.left_down = true;
        let cfg = CursorConfig { sens: 0.2, ..CursorConfig::default() };
        st.reconfigure(&cfg);
        assert_eq!(st.sens, 0.2);
        assert!(st.left_down, "reconfigure must not clear a held click");
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
}
