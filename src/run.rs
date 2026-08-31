//! The daemon pipeline: passive controller tap -> gesture recognition ->
//! config lookup -> game-focus gate -> Hyprland dispatch.
//!
//! Two threads feed one loop. `hidraw::read_all` streams raw reports (merged
//! across the puck's pairing slots); `hypr::subscribe` streams compositor
//! events. The main loop owns the [`GestureEngine`], [`Config`], and
//! [`Arbiter`], and executes [`Action`]s via [`Hypr`].

use crate::arbitrate::Arbiter;
use crate::config::{Action, Config, CursorConfig, ScrollConfig, ScrollMode, WorkspaceTarget};
use crate::filter::{AngleAccumulator, PadDamper};
use crate::gesture::{GestureEngine, GestureEvent};
use crate::hypr::{Hypr, HyprEvent};
use crate::osk::{OskHandle, OskMode, OskPad};
use crate::output::{PointerButton, VirtualPointer};
use crate::{hidraw, report};

use std::path::PathBuf;
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
    /// Every puck reader thread has exited — the controller went away. The loop
    /// enters its reconnect wait instead of terminating.
    ReadersEnded,
}

pub fn run() -> std::io::Result<()> {
    let nodes = hidraw::puck_nodes()?;
    if nodes.is_empty() {
        eprintln!("no Steam Controller puck found (28de:1304)");
        std::process::exit(1);
    }
    let config = match Config::load() {
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
    let own_lizard = lizard_ownership_enabled(&config);

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

    let debug = std::env::var_os("HYPRSC_DEBUG").is_some();
    let mut engine = GestureEngine::new();
    let mut arbiter = Arbiter::new();

    // The on-screen keyboard bridge. Spawns lazily on the first toggle and is
    // killed when this function returns (daemon exit). When it is showing, the
    // two pads drive the keyboard instead of the desktop cursor.
    let mut osk = OskHandle::new();
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
            Input::Compositor(ev) => {
                if debug {
                    eprintln!("[event] {ev:?}");
                }
                update_arbiter(&mut arbiter, ev);
            }
            Input::Report(data) => {
                let Some(frame) = report::Frame::decode(&data) else { continue };
                let now = Instant::now();
                for ge in engine.update(&frame, now) {
                    if debug {
                        eprintln!("[gesture] {ge:?} -> {:?}", config.resolve(&ge));
                    }
                    handle_gesture(&hypr, &config, &arbiter, &mut osk, ge);
                }
                if osk.is_active() {
                    // The keyboard owns both pads: route them to it, and keep the
                    // desktop cursor and scroll released (guide/suppressed = true
                    // forces the drop-and-forget branch of `drive_cursor` /
                    // `drive_scroll`).
                    route_osk(&mut osk, &frame, &prev_frame, &mut osk_route, now);
                    if let Some(ptr) = pointer.as_mut() {
                        drive_cursor(ptr, &frame, &mut cursor, true, true, now);
                        drive_scroll(ptr, &frame, &mut scroll, true, true, now);
                    }
                } else if let Some(ptr) = pointer.as_mut() {
                    // Ambient (non-guide) layer: the RIGHT pad drives the cursor
                    // and the LEFT pad drives scroll. The guide layer and a
                    // focused game both take the pads away. Same gate for both.
                    drive_cursor(ptr, &frame, &mut cursor, engine.guide_active(), arbiter.suppressed(), now);
                    drive_scroll(ptr, &frame, &mut scroll, engine.guide_active(), arbiter.suppressed(), now);
                }
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
fn route_osk(
    osk: &mut OskHandle,
    frame: &report::Frame,
    prev: &report::Frame,
    st: &mut OskRoute,
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
    // trigger pull on the same side is an alternate commit.
    for b in frame.edges_down(prev) {
        match b {
            PadLeftClick | TriggerL2Full => osk.commit(OskPad::Left),
            PadRightClick | TriggerR2Full => osk.commit(OskPad::Right),
            _ => {}
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
}
