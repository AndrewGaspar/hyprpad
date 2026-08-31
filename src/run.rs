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
    let config = Config::load_default();
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
            }
        }
    }
    Ok(())
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
