//! The OSK application: binds the Wayland globals, owns the live layer
//! surfaces, and services the Wayland fd and the control-channel fd(s) from one
//! poll loop (no calloop, no threads — the control channel *is* the driver).
//!
//! It wires together the four other modules:
//! * [`crate::layout`] — the shared key model + geometry for both modes.
//! * [`crate::surface`] — anchoring / exclusive-zone policy (OVERLAY, destroy
//!   on dismiss).
//! * [`crate::render`] — draws each panel into an shm buffer.
//! * [`crate::output`] — the uinput keyboard that actually types.

use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output::WlOutput, wl_shm::Format, wl_surface::WlSurface},
    Connection, QueueHandle,
};

use crate::control::{Channel, Command};
use crate::layout::{
    Key, Keyboard, KeyRole, Layer, LayoutEngine, LayoutMode, Pad, PanelRole, PlacedKey, ShiftModel,
    ShiftState,
};
use crate::output::VirtualKeyboard;
use crate::render::{self, Canvas, Chrome, Cursor, Highlight, HighlightKind};
use crate::surface;
use crate::theme::Theme;

/// The floor on how often a single panel is redrawn. At ~6 ms this caps drawing
/// at ~165 Hz (this class of panel's max useful rate, matching a 165 Hz
/// display), collapsing the ~250 Hz controller report stream into at most one
/// draw per display frame. Applies once a `wl_surface.frame` callback has told
/// us the compositor is ready for the next frame.
const MIN_FRAME: Duration = Duration::from_micros(6_000);

/// The fallback: if a requested `wl_surface.frame` callback has not arrived this
/// long after a draw, draw anyway. A compositor is not obliged to send a frame
/// callback when it is not repainting the surface (Hyprland withholds them from
/// an OVERLAY layer that another full-screen overlay covers, for instance), and
/// without this the per-pad cursors would freeze mid-move. It is a little longer
/// than one 165 Hz frame so that when callbacks *do* arrive they pace us exactly
/// to the display and this fallback never fires. See [`Osk::render_tick`].
const FRAME_FALLBACK: Duration = Duration::from_millis(11);

/// One live layer surface (a panel) plus its cached geometry and draw state.
struct PanelSurface {
    role: PanelRole,
    layer: LayerSurface,
    /// Current pixel size (from the last configure, or requested fallback).
    size: (u32, u32),
    /// Requested fallback size when the compositor lets us choose (0 axis).
    want: (u32, u32),
    configured: bool,
    placed: Vec<PlacedKey>,
    highlights: Vec<Highlight>,
    /// The trackpad cursor sprite(s) to draw on this panel (per-pad, §4.5).
    cursors: Vec<Cursor>,
    /// Kept alive so the compositor can read the committed buffer.
    buffer: Option<Buffer>,
    /// The panel's draw state has changed and it needs a redraw. Set by any
    /// state change (cursor move, highlight, shift/layer/reflow); cleared when
    /// the panel is actually drawn. Coalesces a burst of control commands into
    /// one draw per display frame (the frame-callback pacing in the poll loop).
    dirty: bool,
    /// A `wl_surface.frame` callback has been requested and has not fired yet.
    /// While a callback is outstanding the panel waits for it (up to
    /// [`FRAME_FALLBACK`]) rather than redrawing per control command — this is
    /// what paces drawing to the display's refresh rate instead of the ~250 Hz
    /// controller report rate.
    frame_pending: bool,
    /// When this panel was last drawn+committed. Drives the [`MIN_FRAME`] cap and
    /// the [`FRAME_FALLBACK`] deadline (see [`Osk::render_tick`]).
    last_draw: Option<Instant>,
}

/// The whole OSK state — also the Wayland dispatch target.
pub struct Osk {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,

    /// The key set for the currently active [`Layer`]. Rebuilt on a layer
    /// switch (both layers share geometry, so no surface resize is needed).
    keyboard: Keyboard,
    /// Which key layer (`Base` QWERTY or `Symbols`) `keyboard` currently holds.
    layer: Layer,
    /// The active theme: every colour and every geometry token the draw path and
    /// the size-to-content computation read from.
    theme: Theme,
    mode: LayoutMode,
    /// Current surface policy: `true` = displace (exclusive zone), `false` =
    /// overlay/float (the default). Toggled from the keyboard or the control
    /// channel by recreating the surface(s).
    reflow: bool,
    /// Best-known output logical size (kept for future clamping / diagnostics;
    /// the panels are content-sized from the theme, not the screen).
    output_size: (u32, u32),

    panels: Vec<PanelSurface>,
    /// The uinput keyboard, created lazily on first type and kept alive for the
    /// process lifetime. (Unlike the Wayland `zwp_virtual_keyboard_v1` path
    /// which must be destroyed after each burst to restore the seat keymap —
    /// osk-technology.md §3.3B — a uinput device emits real keycodes with no
    /// keymap swap, so keeping it alive is correct and avoids re-enumeration
    /// latency.)
    vkbd: Option<VirtualKeyboard>,

    // Concurrent per-pad focus (osk-technology.md §4.5): both live at once.
    left_focus: Option<usize>,
    right_focus: Option<usize>,
    // Each pad's last absolute normalized position (input) and the panel-local
    // pixel point it maps to (for drawing the cursor sprite). Kept per pad so
    // both cursors stay live at once and survive a re-place (§4.1/§4.5).
    left_norm: Option<(f32, f32)>,
    right_norm: Option<(f32, f32)>,
    left_px: Option<(f32, f32)>,
    right_px: Option<(f32, f32)>,
    // The shift level: the {Off, OneShot, Stuck} latch the on-screen keys drive
    // plus the daemon's held (momentary) shift, osk-technology.md §4.6.
    shift: ShiftModel,
    trackpad_scale: f32,

    pub exit: bool,
}

impl Osk {
    /// Connect, bind globals, and run the poll loop until `quit`/EOF. `theme`
    /// is the resolved active theme (from [`crate::theme::ThemeSource`]).
    pub fn run(mut channel: Channel, theme: Theme) -> Result<(), String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, mut event_queue) =
            registry_queue_init(&conn).map_err(|e| format!("registry init: {e}"))?;
        let qh = event_queue.handle();

        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);
        let compositor =
            CompositorState::bind(&globals, &qh).map_err(|e| format!("bind wl_compositor: {e}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|e| format!("bind zwlr_layer_shell_v1: {e}"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| format!("bind wl_shm: {e}"))?;
        let pool = SlotPool::new(1920 * 520 * 4, &shm).map_err(|e| format!("shm pool: {e}"))?;

        let mut osk = Osk {
            registry_state,
            output_state,
            compositor,
            layer_shell,
            shm,
            pool,
            keyboard: Layer::Base.keyboard(),
            layer: Layer::Base,
            theme,
            mode: LayoutMode::BottomDeck,
            reflow: false,
            output_size: (0, 0),
            panels: Vec::new(),
            vkbd: None,
            left_focus: None,
            right_focus: None,
            left_norm: None,
            right_norm: None,
            left_px: None,
            right_px: None,
            shift: ShiftModel::default(),
            trackpad_scale: 1.0,
            exit: false,
        };

        // Learn about outputs before the first show.
        event_queue
            .roundtrip(&mut osk)
            .map_err(|e| format!("initial roundtrip: {e}"))?;

        eprintln!(
            "hyprpad-osk: ready (namespace '{}', theme '{}', key_size {}px)",
            surface::NAMESPACE, osk.theme.name, osk.theme.geom.key_size
        );

        while !osk.exit {
            // Apply queued Wayland events (a fired frame callback clears its
            // panel's `frame_pending` here), draw any panel whose paced deadline
            // has arrived, then push our requests out.
            event_queue
                .dispatch_pending(&mut osk)
                .map_err(|e| format!("dispatch: {e}"))?;
            osk.render_tick(&qh);
            conn.flush().map_err(|e| format!("flush: {e}"))?;

            // Prepare to read; if events are still queued, loop to dispatch them.
            let guard = match event_queue.prepare_read() {
                Some(g) => g,
                None => continue,
            };
            let wl_fd = guard.connection_fd().as_raw_fd();

            let ctrl_fds = channel.poll_fds();
            let mut pfds: Vec<libc::pollfd> = Vec::with_capacity(1 + ctrl_fds.len());
            pfds.push(libc::pollfd { fd: wl_fd, events: libc::POLLIN, revents: 0 });
            for &fd in &ctrl_fds {
                pfds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
            }

            // Block until an fd is readable, or only until the next panel's paced
            // draw is due (so a coalesced burst still gets its one frame even if
            // the compositor never sends the frame callback).
            let timeout = osk.poll_timeout_ms();
            let ret = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    drop(guard);
                    continue;
                }
                return Err(format!("poll: {err}"));
            }

            // Wayland readable?
            if pfds[0].revents & libc::POLLIN != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
            }

            // Control readable? drain all currently-available commands.
            if pfds[1..].iter().any(|p| p.revents != 0) {
                let mut cmds: Vec<Command> = Vec::new();
                let alive = channel
                    .drain(|c| cmds.push(c))
                    .map_err(|e| format!("control channel: {e}"))?;
                for c in cmds {
                    osk.handle(c, &qh);
                }
                if !alive {
                    osk.exit = true; // stdin EOF
                }
            }
        }

        Ok(())
    }

    fn handle(&mut self, cmd: Command, qh: &QueueHandle<Self>) {
        match cmd {
            Command::Show { mode, reflow } => self.show(mode, reflow, qh),
            Command::Hide => self.hide(),
            Command::Cursor { pad, nx, ny } => self.update_cursor(pad, nx, ny),
            Command::Commit { pad } => self.commit(pad, qh),
            Command::Shift { state } => self.set_shift(state),
            Command::ShiftHeld { down } => self.set_held_shift(down),
            Command::Layer { target } => self.set_layer(target.unwrap_or_else(|| self.layer.toggled())),
            Command::Reflow { on } => self.set_reflow(on, qh),
            Command::Key { keycode, mods } => self.type_keycode(keycode, &mods),
            Command::Type { text } => self.type_str(&text),
            Command::Quit => self.exit = true,
        }
    }

    /// Create the layer surface(s) for `mode`, destroying any currently shown.
    /// Preserves the shift and layer state (so a display/policy toggle re-shows
    /// without losing them); only the per-pad cursors reset.
    fn show(&mut self, mode: LayoutMode, reflow: bool, qh: &QueueHandle<Self>) {
        self.hide(); // destroy first — never stack surfaces
        self.mode = mode;
        self.reflow = reflow;
        let specs = surface::panels_for(mode, reflow, &self.keyboard, &self.theme.geom);
        for spec in specs {
            let wl_surface = self.compositor.create_surface(qh);
            let layer = self.layer_shell.create_layer_surface(
                qh,
                wl_surface,
                surface::LAYER,
                Some(surface::NAMESPACE),
                None,
            );
            layer.set_anchor(spec.anchor);
            layer.set_size(spec.width, spec.height);
            layer.set_exclusive_zone(spec.exclusive_zone);
            layer.set_keyboard_interactivity(surface::INTERACTIVITY);
            layer.commit();
            self.panels.push(PanelSurface {
                role: spec.role,
                layer,
                size: (0, 0),
                want: (spec.width, spec.height),
                configured: false,
                placed: Vec::new(),
                highlights: Vec::new(),
                cursors: Vec::new(),
                buffer: None,
                dirty: false,
                frame_pending: false,
                last_draw: None,
            });
        }
        self.reset_pad_state();
        eprintln!(
            "hyprpad-osk: show mode={:?} reflow={} layer={:?} panels={}",
            mode,
            reflow,
            self.layer,
            self.panels.len()
        );
    }

    /// **Destroy** the surface(s) (osk-technology.md §2.3 — dropping the
    /// `LayerSurface` sends the destroy request; an idle OVERLAY surface would
    /// otherwise block direct scanout for every fullscreen game).
    fn hide(&mut self) {
        if !self.panels.is_empty() {
            eprintln!("hyprpad-osk: hide (destroying {} surface(s))", self.panels.len());
        }
        self.panels.clear();
        self.reset_pad_state();
        // A held shift belongs to a button the daemon was holding while the
        // keyboard was up; it cannot outlive the keyboard. The latch can — a
        // re-show keeps Caps — so only the held bit is dropped.
        self.shift = self.shift.with_held(false);
    }

    /// Clear both pads' focus and cursor state (on show/hide/layer swap).
    fn reset_pad_state(&mut self) {
        self.left_focus = None;
        self.right_focus = None;
        self.left_norm = None;
        self.right_norm = None;
        self.left_px = None;
        self.right_px = None;
    }

    /// The panel a pad addresses in the current mode.
    fn panel_for_pad(&self, pad: Pad) -> Option<usize> {
        let want = match (self.mode, pad) {
            (LayoutMode::BottomDeck, _) => PanelRole::Bottom,
            (LayoutMode::SideSplit, Pad::Left) => PanelRole::LeftColumn,
            (LayoutMode::SideSplit, Pad::Right) => PanelRole::RightColumn,
        };
        self.panels.iter().position(|p| p.role == want && p.configured)
    }

    /// Move a pad's absolute cursor: record its position, hit-test the key under
    /// it, refresh the highlight + the visible cursor sprite, and mark the
    /// affected panel dirty (osk-technology.md §4.1).
    ///
    /// A **key crossing** — the hit-test landing on a different key than last
    /// move — is announced on stdout as `event crossed <L|R>` (§4.3): the daemon
    /// owns the controller's haptic actuators, so it turns that line into a tick
    /// under this pad's thumb. Emitted only on an actual change, and never for a
    /// crossing onto a gap (the Deck's "double-thunk" fix, §4.3).
    ///
    /// A no-op move — same focused key AND the cursor sprite lands on the same
    /// integer pixel — is skipped entirely: the daemon already dedupes at
    /// `%.4f`, but that is still sub-pixel churn at ~250 Hz, so we collapse it on
    /// the render side too (nothing the renderer would draw differently). Real
    /// moves only mark the surface dirty; the actual draw is paced by the poll
    /// loop's [`Self::render_tick`].
    fn update_cursor(&mut self, pad: Pad, nx: f32, ny: f32) {
        match pad {
            Pad::Left => self.left_norm = Some((nx, ny)),
            Pad::Right => self.right_norm = Some((nx, ny)),
        }
        // Snapshot what the renderer currently draws for this pad, recompute,
        // then compare: if neither the focused key nor the on-screen cursor
        // pixel changed, there is nothing to redraw.
        let old_focus = self.pad_focus(pad);
        let old_px = self.pad_px(pad);
        self.recompute_pad(pad);
        let new_focus = self.pad_focus(pad);
        // Crossing onto a *key* (not off one, and not key→gap) is the tick.
        if new_focus != old_focus && new_focus.is_some() {
            emit_event(&format!("crossed {}", pad_wire(pad)));
        }
        if new_focus == old_focus && same_pixel(old_px, self.pad_px(pad)) {
            return;
        }
        self.rebuild_highlights();
        self.rebuild_cursors();
        if let Some(idx) = self.panel_for_pad(pad) {
            self.mark_panel_dirty(idx);
        }
    }

    /// The key a pad currently focuses (for the no-op dedup in
    /// [`Self::update_cursor`]).
    fn pad_focus(&self, pad: Pad) -> Option<usize> {
        match pad {
            Pad::Left => self.left_focus,
            Pad::Right => self.right_focus,
        }
    }

    /// A pad's current cursor pixel point (for the no-op dedup).
    fn pad_px(&self, pad: Pad) -> Option<(f32, f32)> {
        match pad {
            Pad::Left => self.left_px,
            Pad::Right => self.right_px,
        }
    }

    /// (Re)derive a pad's focused key and cursor pixel point from its last
    /// normalized position against the current placement. Called on every cursor
    /// move and after any re-place (layer switch / configure), so a moved pad
    /// stays correct even when the key set under it changed.
    fn recompute_pad(&mut self, pad: Pad) {
        let norm = match pad {
            Pad::Left => self.left_norm,
            Pad::Right => self.right_norm,
        };
        let (Some((nx, ny)), Some(idx)) = (norm, self.panel_for_pad(pad)) else {
            return;
        };
        let (w, h) = self.panels[idx].size;
        if w == 0 || h == 0 {
            return;
        }
        let role = self.panels[idx].role;
        let eng = LayoutEngine::new(&self.keyboard, self.mode);
        let (px, py) = eng.map_cursor(pad, role, &self.theme.geom, (nx, ny), self.trackpad_scale);
        let hit = LayoutEngine::hit_test(&self.panels[idx].placed, px, py)
            .map(|i| self.panels[idx].placed[i].key);
        match pad {
            Pad::Left => {
                self.left_focus = hit;
                self.left_px = Some((px, py));
            }
            Pad::Right => {
                self.right_focus = hit;
                self.right_px = Some((px, py));
            }
        }
    }

    /// Commit the key under `pad`'s cursor (trackpad click-down, §4.2).
    fn commit(&mut self, pad: Pad, qh: &QueueHandle<Self>) {
        let focus = match pad {
            Pad::Left => self.left_focus,
            Pad::Right => self.right_focus,
        };
        let Some(k) = focus else { return };
        self.commit_key_index(k, qh);
    }

    fn commit_key_index(&mut self, k: usize, qh: &QueueHandle<Self>) {
        let key = self.keyboard.keys[k].clone();
        match key.role {
            KeyRole::Shift => {
                self.shift = self.shift.tap_shift(); // Off→OneShot→Stuck→Off (§4.6)
            }
            KeyRole::Caps => {
                self.shift = self.shift.tap_caps(); // Off↔Stuck (§4.6)
            }
            KeyRole::LayerToggle => {
                self.set_layer(self.layer.toggled());
                return; // set_layer re-places + redraws
            }
            KeyRole::DisplayToggle => {
                self.set_reflow(!self.reflow, qh);
                return; // set_reflow recreates the surface(s)
            }
            KeyRole::Char | KeyRole::Meta | KeyRole::Backspace | KeyRole::Enter | KeyRole::Tab
            | KeyRole::Space => {
                if let Some((code, shifted)) = keystroke_for(&key, self.shift) {
                    self.tap(code, shifted);
                }
                if key.role == KeyRole::Char {
                    self.shift = self.shift.after_char(); // one-shot clears
                }
            }
        }
        self.rebuild_highlights();
        self.mark_all_dirty();
    }

    /// The daemon's `key <code> [mod…]`: tap the code with those modifiers
    /// held around it, and with the keyboard's own Shift folded in if its latch
    /// (or a held L2) is up — the same rule a key committed off the layout gets.
    fn type_keycode(&mut self, keycode: u16, mods: &[u16]) {
        let shift = self.shift.is_active();
        if self.ensure_vkbd() {
            if let Some(kbd) = self.vkbd.as_mut() {
                kbd.tap_with_mods(keycode, shift, mods);
            }
        }
    }

    fn type_str(&mut self, text: &str) {
        if self.ensure_vkbd() {
            if let Some(kbd) = self.vkbd.as_mut() {
                kbd.type_text(text);
            }
        }
    }

    fn tap(&mut self, keycode: u16, shift: bool) {
        if self.ensure_vkbd() {
            if let Some(kbd) = self.vkbd.as_mut() {
                kbd.tap(keycode, shift);
            }
        }
    }

    /// Create the uinput keyboard on first use. Returns whether one is available.
    fn ensure_vkbd(&mut self) -> bool {
        if self.vkbd.is_none() {
            match VirtualKeyboard::new() {
                Ok(k) => self.vkbd = Some(k),
                Err(e) => {
                    eprintln!("hyprpad-osk: uinput keyboard unavailable: {e}");
                    return false;
                }
            }
        }
        true
    }

    /// Recompute per-panel highlight lists from the current per-pad focus.
    fn rebuild_highlights(&mut self) {
        for p in &mut self.panels {
            p.highlights.clear();
        }
        Self::push_highlight(&mut self.panels, self.left_focus, HighlightKind::LeftPad);
        Self::push_highlight(&mut self.panels, self.right_focus, HighlightKind::RightPad);
    }

    fn push_highlight(panels: &mut [PanelSurface], focus: Option<usize>, kind: HighlightKind) {
        let Some(k) = focus else { return };
        for p in panels.iter_mut() {
            if p.placed.iter().any(|pk| pk.key == k) {
                p.highlights.push(Highlight { key: k, kind });
            }
        }
    }

    /// Recompute each panel's cursor-sprite list from the per-pad pixel points.
    /// A pad's cursor is drawn on whichever panel that pad addresses in the
    /// current mode (both on the bottom panel for `BottomDeck`; one per column
    /// for `SideSplit`), so up to two cursors are live at once (§4.5).
    fn rebuild_cursors(&mut self) {
        for p in &mut self.panels {
            p.cursors.clear();
        }
        for (pad, pos, kind) in [
            (Pad::Left, self.left_px, HighlightKind::LeftPad),
            (Pad::Right, self.right_px, HighlightKind::RightPad),
        ] {
            if let (Some((x, y)), Some(idx)) = (pos, self.panel_for_pad(pad)) {
                self.panels[idx].cursors.push(Cursor { x, y, kind });
            }
        }
    }

    /// Set the latched shift/caps state directly (control channel) and redraw so
    /// the legends and the Shift/Caps indicator update.
    fn set_shift(&mut self, state: ShiftState) {
        self.shift = self.shift.with_latched(state);
        self.mark_all_dirty();
    }

    /// Hold or release the physical shift (`shift down` / `shift up`): the
    /// legends re-render shifted for as long as the daemon holds it, and every
    /// commit in between types the shifted glyph. The latch is untouched.
    fn set_held_shift(&mut self, down: bool) {
        if self.shift.held == down {
            return;
        }
        self.shift = self.shift.with_held(down);
        self.mark_all_dirty();
    }

    /// Switch the active key layer. Rebuilds `keyboard` for the new layer,
    /// re-places every live panel (the layers share geometry, so no resize), and
    /// re-derives each pad's focus/cursor against the new keys, then redraws.
    fn set_layer(&mut self, layer: Layer) {
        if self.layer == layer {
            return;
        }
        self.layer = layer;
        self.keyboard = layer.keyboard();
        for i in 0..self.panels.len() {
            if !self.panels[i].configured {
                continue;
            }
            let role = self.panels[i].role;
            let placed = {
                let eng = LayoutEngine::new(&self.keyboard, self.mode);
                eng.place(role, &self.theme.geom)
            };
            self.panels[i].placed = placed;
        }
        // Indices changed with the key set; re-derive focus/cursor from the
        // stored pad positions.
        self.left_focus = None;
        self.right_focus = None;
        self.recompute_pad(Pad::Left);
        self.recompute_pad(Pad::Right);
        self.rebuild_highlights();
        self.rebuild_cursors();
        self.mark_all_dirty();
        eprintln!("hyprpad-osk: layer -> {:?}", self.layer);
    }

    /// Switch the surface policy (overlay/float ↔ displace). Recreates the live
    /// surface(s) with the flipped exclusive zone, preserving the shift and
    /// layer state (only the transient cursors reset).
    fn set_reflow(&mut self, reflow: bool, qh: &QueueHandle<Self>) {
        if self.panels.is_empty() {
            self.reflow = reflow; // nothing shown yet; the next `show` will use it
            return;
        }
        if self.reflow == reflow {
            return;
        }
        let mode = self.mode;
        self.show(mode, reflow, qh); // destroy + recreate with the new zone
        eprintln!("hyprpad-osk: reflow -> {reflow}");
    }

    /// Mark every live panel dirty. Used by the chrome-wide changes (shift/caps,
    /// layer, commit) that can alter every panel's legends at once. The poll
    /// loop's [`Self::render_tick`] draws them at the paced cadence.
    fn mark_all_dirty(&mut self) {
        for p in &mut self.panels {
            p.dirty = true;
        }
    }

    /// Mark one panel dirty. Does not draw — [`Self::render_tick`] does, once the
    /// panel's paced deadline arrives. This is what coalesces a burst of ~250 Hz
    /// cursor updates into at most one draw+commit per display frame — the fix
    /// for the flicker/tearing and the CPU spike from redrawing per command.
    fn mark_panel_dirty(&mut self, idx: usize) {
        if let Some(panel) = self.panels.get_mut(idx) {
            panel.dirty = true;
        }
    }

    /// Draw every panel whose paced deadline has arrived. Called once per poll
    /// wakeup. A panel is *due* when it is dirty and either (a) its last frame
    /// callback has fired and at least [`MIN_FRAME`] has elapsed — the normal,
    /// display-paced case — or (b) it is still waiting on a frame callback that
    /// has not come within [`FRAME_FALLBACK`] — the safety net for compositors
    /// that withhold frame callbacks from a covered OVERLAY surface. Either way a
    /// panel draws at most once per [`MIN_FRAME`], never per control command.
    fn render_tick(&mut self, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        for i in 0..self.panels.len() {
            if self.panel_due(i, now) {
                let chrome = self.chrome();
                draw_panel(&mut self.pool, &self.theme, &self.keyboard, &mut self.panels[i], chrome, qh);
            }
        }
    }

    /// Whether panel `idx` should be drawn at instant `now` (see
    /// [`Self::render_tick`] for the policy).
    fn panel_due(&self, idx: usize, now: Instant) -> bool {
        let p = &self.panels[idx];
        if !p.configured || !p.dirty {
            return false;
        }
        match p.last_draw {
            None => true, // never drawn (e.g. first frame after configure)
            Some(t) => {
                let since = now.saturating_duration_since(t);
                if p.frame_pending {
                    since >= FRAME_FALLBACK
                } else {
                    since >= MIN_FRAME
                }
            }
        }
    }

    /// The poll timeout (ms) until the next panel becomes due: `-1` (block) when
    /// nothing is dirty, else the shortest wait so the loop wakes to draw a
    /// coalesced burst even without a frame callback. Rounds a sub-millisecond
    /// wait up to 1 ms so the loop never busy-spins.
    fn poll_timeout_ms(&self) -> libc::c_int {
        let now = Instant::now();
        let mut soonest: Option<Duration> = None;
        for p in &self.panels {
            if !p.configured || !p.dirty {
                continue;
            }
            let wait = match p.last_draw {
                None => Duration::ZERO,
                Some(t) => {
                    let deadline = if p.frame_pending { FRAME_FALLBACK } else { MIN_FRAME };
                    deadline.saturating_sub(now.saturating_duration_since(t))
                }
            };
            soonest = Some(soonest.map_or(wait, |c| c.min(wait)));
        }
        match soonest {
            None => -1,
            Some(d) if d.is_zero() => 0,
            Some(d) => d.as_millis().max(1).min(libc::c_int::MAX as u128) as libc::c_int,
        }
    }

    /// The live chrome state (shift level + reflow policy) the renderer needs.
    fn chrome(&self) -> Chrome {
        Chrome { shift: self.shift, reflow: self.reflow }
    }

    /// Recompute a panel's key placement after a configure and redraw it.
    fn configure_panel(&mut self, layer: &LayerSurface, cfg: &LayerSurfaceConfigure) {
        let Some(idx) = self
            .panels
            .iter()
            .position(|p| p.layer.wl_surface() == layer.wl_surface())
        else {
            return;
        };
        let (fw, fh) = self.panels[idx].want;
        let w = if cfg.new_size.0 != 0 { cfg.new_size.0 } else if fw != 0 { fw } else { self.output_size.0.max(640) };
        let h = if cfg.new_size.1 != 0 { cfg.new_size.1 } else if fh != 0 { fh } else { self.output_size.1.max(360) };
        self.panels[idx].size = (w, h);

        let role = self.panels[idx].role;
        // Placement is content-sized from the theme geometry (not stretched to
        // the configured size), so it always agrees with the `[-1,1]` cursor
        // mapping. In practice the configured size equals this content size.
        let placed = {
            let eng = LayoutEngine::new(&self.keyboard, self.mode);
            eng.place(role, &self.theme.geom)
        };
        self.panels[idx].placed = placed;
        self.panels[idx].configured = true;
        // A pad may already be resting over this newly-placed panel — re-derive
        // its focus/cursor so a resize/first-configure doesn't drop it.
        self.recompute_pad(Pad::Left);
        self.recompute_pad(Pad::Right);
        self.rebuild_highlights();
        self.rebuild_cursors();
        // First configure makes the panel drawable: mark it dirty. `last_draw`
        // is `None`, so the next `render_tick` draws it immediately and starts
        // the frame-callback pacing loop.
        self.mark_panel_dirty(idx);
    }
}

/// Draw one panel into a **free** shm buffer, attach it with proper release
/// tracking, request the next frame callback, and commit. Free function so the
/// disjoint borrows of `pool`, `theme`, `keyboard`, and one `panel` are clear.
///
/// Buffer discipline (the fix for the tearing/flicker): the buffer is attached
/// via [`Buffer::attach_to`], which `activate()`s it so SCTK's `SlotPool` keeps
/// the slot reserved until the compositor sends `wl_buffer.release`. A new
/// `create_buffer` therefore always lands on a slot the compositor is *not*
/// scanning out (the released one is recycled from the freelist), so we never
/// draw into a buffer that is still on screen. The old code attached the raw
/// `wl_buffer` directly, which bypassed this tracking and let the just-freed
/// slot be reused mid-scanout.
///
/// Pacing: we request a `wl_surface.frame` callback (unless one is already
/// outstanding) and set `frame_pending`, so the next draw waits for the
/// display's frame clock — or, if the compositor withholds the callback, for the
/// [`FRAME_FALLBACK`] deadline — rather than firing per control command.
fn draw_panel(
    pool: &mut SlotPool,
    theme: &Theme,
    keyboard: &Keyboard,
    panel: &mut PanelSurface,
    chrome: Chrome,
    qh: &QueueHandle<Osk>,
) {
    if !panel.configured {
        return;
    }
    let (w, h) = (panel.size.0 as i32, panel.size.1 as i32);
    if w <= 0 || h <= 0 {
        return;
    }
    let (buffer, canvas_bytes) = match pool.create_buffer(w, h, w * 4, Format::Argb8888) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("hyprpad-osk: create_buffer {w}x{h}: {e}");
            return;
        }
    };
    {
        let mut canvas = Canvas::new(canvas_bytes, w, h);
        render::draw_panel(&mut canvas, theme, &keyboard.keys, &panel.placed, &panel.highlights, &panel.cursors, chrome);
    }
    let surface = panel.layer.wl_surface();
    // Ask for a frame callback only when one is not already outstanding: on a
    // compositor that answers them this is one-per-draw and paces us to the
    // display; on one that withholds them (the fallback path) it avoids piling
    // up never-answered `wl_callback` objects while the timer drives redraws.
    if !panel.frame_pending {
        surface.frame(qh, surface.clone());
    }
    // `attach_to` activates the buffer (release-tracked) before attaching.
    if let Err(e) = buffer.attach_to(surface) {
        eprintln!("hyprpad-osk: attach buffer: {e}");
        return;
    }
    surface.damage_buffer(0, 0, w, h);
    panel.layer.commit();
    // Hold the buffer alive; it is released by the compositor (tracked by SCTK)
    // and its slot recycled on a later draw.
    panel.buffer = Some(buffer);
    panel.dirty = false;
    panel.frame_pending = true;
    panel.last_draw = Some(Instant::now());
    let _ = render::buffer_len; // keep the helper referenced; used by tests/tools
}

/// The `L`/`R` token a pad uses on the wire — the same one the control channel
/// parses in `cursor`/`commit`, reused for the outbound event lines so the two
/// directions of the protocol can never drift apart.
fn pad_wire(pad: Pad) -> &'static str {
    match pad {
        Pad::Left => "L",
        Pad::Right => "R",
    }
}

/// Emit one machine-readable event line on **stdout**: the back-channel the
/// hyprpad daemon reads (`event <name> [args…]`).
///
/// Human-oriented logs stay on **stderr** (every `hyprpad-osk:` line above), so
/// a daemon that pipes stdout gets a clean, parseable stream and still sees the
/// logs inherited on its own stderr. Best-effort: a closed or full pipe is
/// ignored rather than killing the keyboard — feedback is never worth a crash.
/// Rust's stdout is line-buffered, and the explicit flush keeps the tick timely
/// even if that ever changes.
fn emit_event(args: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "event {args}");
    let _ = out.flush();
}

/// Whether two optional cursor points land on the same integer pixel — the
/// renderer draws the cursor sprite at `x as i32 / y as i32`, so a sub-pixel
/// move that rounds to the same pixel produces an identical frame and is
/// skipped (see [`Osk::update_cursor`]).
fn same_pixel(a: Option<(f32, f32)>, b: Option<(f32, f32)>) -> bool {
    match (a, b) {
        (Some((ax, ay)), Some((bx, by))) => ax as i32 == bx as i32 && ay as i32 == by as i32,
        (None, None) => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// SCTK handler trait impls
// ---------------------------------------------------------------------------

impl CompositorHandler for Osk {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wayland_client::protocol::wl_output::Transform,
    ) {
    }
    /// A `wl_surface.frame` callback fired: the compositor is ready for the next
    /// frame on this surface. Just clear `frame_pending`; the poll loop's
    /// [`Self::render_tick`] draws it (if dirty) right after this dispatch. That
    /// keeps all drawing on one path and paced by one clock. If the panel is not
    /// dirty we go idle — no busy redraw.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        if let Some(idx) = self.panels.iter().position(|p| p.layer.wl_surface() == surface) {
            self.panels[idx].frame_pending = false;
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl OutputHandler for Osk {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.record_output(&output);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.record_output(&output);
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl Osk {
    fn record_output(&mut self, output: &WlOutput) {
        if let Some(info) = self.output_state.info(output) {
            if let Some((w, h)) = info.logical_size {
                if w > 0 && h > 0 {
                    self.output_size = (w as u32, h as u32);
                }
            }
        }
    }
}

impl LayerShellHandler for Osk {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.panels.retain(|p| p.layer.wl_surface() != layer.wl_surface());
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.configure_panel(layer, &configure);
    }
}

impl ShmHandler for Osk {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Osk {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(Osk);
delegate_output!(Osk);
delegate_shm!(Osk);
delegate_layer!(Osk);
delegate_registry!(Osk);

/// What committing `key` types, as `(keycode, with Shift held)`, under the live
/// shift level — or `None` for a key that changes state instead of typing
/// (Shift, Caps, the layer and display toggles) and for an inert meta key
/// (`keycode == 0`: emoji/close, §4.6, deferred). A character key follows the
/// level so the committed glyph always matches the drawn legend; the editing
/// keys and arrows never take Shift. Pure, so the effect of the daemon's
/// `shift down` on what a commit produces is unit-testable without a display.
pub(crate) fn keystroke_for(key: &Key, shift: ShiftModel) -> Option<(u16, bool)> {
    match key.role {
        KeyRole::Char => Some((key.keycode, shift.commits_shifted(key))),
        KeyRole::Meta | KeyRole::Backspace | KeyRole::Enter | KeyRole::Tab | KeyRole::Space => {
            (key.keycode != 0).then_some((key.keycode, false))
        }
        KeyRole::Shift | KeyRole::Caps | KeyRole::LayerToggle | KeyRole::DisplayToggle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{parse, Command};

    /// Apply a parsed shift command to a level, the way `handle` does.
    fn apply(shift: ShiftModel, line: &str) -> ShiftModel {
        match parse(line).expect("a shift command") {
            Command::Shift { state } => shift.with_latched(state),
            Command::ShiftHeld { down } => shift.with_held(down),
            other => panic!("not a shift command: {other:?}"),
        }
    }

    #[test]
    fn the_held_shift_command_changes_what_a_commit_types() {
        let kb = Keyboard::qwerty();
        let key = |label: &str| kb.keys.iter().find(|k| k.label == label).unwrap();
        let (a, one, space) = (key("a"), key("1"), key("Space"));

        let level = ShiftModel::default();
        assert_eq!(keystroke_for(a, level), Some((30, false)));
        assert_eq!(keystroke_for(one, level), Some((2, false)));

        // `shift down`: the same commits now carry Shift — `A`, `!` — and the
        // legend the renderer draws agrees (it reads the same `is_active`).
        let held = apply(level, "shift down");
        assert!(held.is_active());
        assert_eq!(keystroke_for(a, held), Some((30, true)));
        assert_eq!(keystroke_for(one, held), Some((2, true)));
        assert_eq!(a.display_label(held.is_active()), "A");
        // Space and the other editing keys never take Shift.
        assert_eq!(keystroke_for(space, held), Some((57, false)));
        // A character commit does not consume a held shift (it is not a one-shot).
        assert_eq!(held.after_char(), held);

        // `shift up`: back to exactly the level before, lower-case again.
        let released = apply(held, "shift up");
        assert_eq!(released, level);
        assert_eq!(keystroke_for(a, released), Some((30, false)));

        // The latch commands still drive the latch, and neither touches the
        // held bit: Caps set under a held shift outlives the release.
        let caps_held = apply(held, "shift on");
        assert_eq!(caps_held, ShiftModel { latched: ShiftState::Stuck, held: true });
        assert_eq!(keystroke_for(a, apply(caps_held, "shift up")), Some((30, true)));

        // State-changing keys type nothing; an inert meta key types nothing.
        assert_eq!(keystroke_for(key("Shift"), held), None);
        assert_eq!(keystroke_for(key("Push"), held), None);
    }

    #[test]
    fn event_pad_tokens_match_the_control_grammar() {
        // The outbound `event crossed <L|R>` reuses the inbound `cursor <L|R>`
        // tokens; the daemon parses both with the same table.
        assert_eq!(pad_wire(Pad::Left), "L");
        assert_eq!(pad_wire(Pad::Right), "R");
        assert_eq!(
            crate::control::parse(&format!("cursor {} 0 0", pad_wire(Pad::Left))),
            Ok(crate::control::Command::Cursor { pad: Pad::Left, nx: 0.0, ny: 0.0 })
        );
    }
}
