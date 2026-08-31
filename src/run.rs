//! The daemon pipeline: passive controller tap -> gesture recognition ->
//! config lookup -> game-focus gate -> Hyprland dispatch.
//!
//! Two threads feed one loop. `hidraw::read_all` streams raw reports (merged
//! across the puck's pairing slots); `hypr::subscribe` streams compositor
//! events. The main loop owns the [`GestureEngine`], [`Config`], and
//! [`Arbiter`], and executes [`Action`]s via [`Hypr`].

use crate::arbitrate::Arbiter;
use crate::config::{Action, Config, WorkspaceTarget};
use crate::gesture::{GestureEngine, GestureEvent};
use crate::hypr::{Hypr, HyprEvent};
use crate::output::{PointerButton, VirtualPointer};
use crate::{hidraw, report};

use std::sync::mpsc;
use std::time::Instant;

/// One unit of work for the main loop: either a controller report or a
/// compositor event. The two source threads funnel into this so the loop can
/// own all mutable state without locks.
enum Input {
    Report(Vec<u8>),
    Compositor(HyprEvent),
}

pub fn run() -> std::io::Result<()> {
    let nodes = hidraw::puck_nodes()?;
    if nodes.is_empty() {
        eprintln!("no Steam Controller puck found (28de:1304)");
        std::process::exit(1);
    }
    let hypr = Hypr::connect()?;
    let config = match Config::load() {
        Ok(c) => {
            match Config::config_path() {
                Some(p) if p.exists() => eprintln!("hyprsc: config loaded from {}", p.display()),
                _ => eprintln!("hyprsc: using built-in default config"),
            }
            c
        }
        Err(e) => {
            eprintln!("warning: {e}; using built-in default config");
            Config::load_default()
        }
    };
    eprintln!("hyprsc: {} controller node(s), Hyprland IPC connected", nodes.len());

    // Merge both event sources into one channel so the loop stays single-owner.
    let (tx, rx) = mpsc::channel::<Input>();

    let reports = hidraw::read_all(&nodes);
    let tx_r = tx.clone();
    std::thread::spawn(move || {
        for r in reports {
            if tx_r.send(Input::Report(r.data)).is_err() {
                return;
            }
        }
    });

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
    drop(tx);

    // The cursor path degrades gracefully: without it, gestures and workspace
    // switching still work.
    let mut pointer = match VirtualPointer::new() {
        Ok(p) => {
            eprintln!("hyprsc: virtual pointer ready (right trackpad drives the cursor)");
            Some(p)
        }
        Err(e) => {
            eprintln!("warning: no virtual pointer ({e}); trackpad cursor disabled");
            None
        }
    };
    let mut cursor = CursorState::default();

    let debug = std::env::var_os("HYPRSC_DEBUG").is_some();
    let mut engine = GestureEngine::new();
    let mut arbiter = Arbiter::new();

    for input in rx {
        match input {
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
                    handle_gesture(&hypr, &config, &arbiter, ge);
                }
                // Ambient (non-guide) right-pad cursor. The guide layer and a
                // focused game both take the pad away from cursor duty.
                if let Some(ptr) = pointer.as_mut() {
                    drive_cursor(ptr, &frame, &mut cursor, engine.guide_active(), arbiter.suppressed());
                }
            }
        }
    }
    Ok(())
}

/// Right-trackpad cursor sensitivity: compositor pixels per pad count. The pad
/// reports absolute i16 touch coordinates (roughly ±32767 across its surface),
/// so at ~0.06 px/count a full-width swipe travels well over a screen width and
/// the whole screen is reachable without re-clutching. Tune to taste.
const PAD_CURSOR_SENS: f64 = 0.06;

/// Cross-frame state for the trackpad cursor.
#[derive(Default)]
struct CursorState {
    /// Last touched right-pad (x, y); `None` when not tracking, so a
    /// lift-and-retouch never produces a jump.
    prev: Option<(i16, i16)>,
    /// Whether the synthetic left mouse button is currently held.
    left_down: bool,
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
) {
    if guide_active || suppressed {
        // Release the desktop's claim: drop any held click and forget the
        // tracking origin so re-entry starts clean.
        if st.left_down {
            ptr.button(PointerButton::Left, false);
            st.left_down = false;
        }
        st.prev = None;
        return;
    }

    if frame.pressed(report::Button::PadRightTouch) {
        let cur = (frame.right_pad.x, frame.right_pad.y);
        if let Some((px, py)) = st.prev {
            // Widen to f64 before subtracting: an i16 delta can overflow.
            let dx = (f64::from(cur.0) - f64::from(px)) * PAD_CURSOR_SENS;
            // Pad +Y is up; screen +Y is down, so invert.
            let dy = (f64::from(cur.1) - f64::from(py)) * -PAD_CURSOR_SENS;
            if dx != 0.0 || dy != 0.0 {
                ptr.move_relative(dx, dy);
            }
        }
        st.prev = Some(cur);
    } else {
        st.prev = None;
    }

    // A hard pad click (or a full right-trigger pull) is a left click.
    let want_down = frame.pressed(report::Button::PadRightClick)
        || frame.pressed(report::Button::TriggerR2Full);
    if want_down != st.left_down {
        ptr.button(PointerButton::Left, want_down);
        st.left_down = want_down;
    }
}

fn update_arbiter(arbiter: &mut Arbiter, ev: HyprEvent) {
    match ev {
        HyprEvent::ActiveWindow { class, .. } => arbiter.focus_changed(&class),
        HyprEvent::Fullscreen(on) => arbiter.set_fullscreen(on),
        _ => {}
    }
}

fn handle_gesture(hypr: &Hypr, config: &Config, arbiter: &Arbiter, ge: GestureEvent) {
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
    if arbiter.suppressed() && !is_guide_scoped(&ge) {
        return;
    }

    if let Err(e) = execute(hypr, &action) {
        eprintln!("dispatch failed: {e}");
    }
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
        Action::None => Ok(()),
    }
}
