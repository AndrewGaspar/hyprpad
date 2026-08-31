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
use crate::layout::{Keyboard, KeyRole, Layer, LayoutEngine, LayoutMode, Pad, PanelRole, PlacedKey, ShiftState};
use crate::output::VirtualKeyboard;
use crate::render::{self, Canvas, Chrome, Cursor, Highlight, HighlightKind};
use crate::surface;
use crate::theme::Theme;

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
    // Shift/caps state machine ({Off, OneShot, Stuck}, osk-technology.md §4.6).
    shift: ShiftState,
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
            shift: ShiftState::Off,
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
            // Apply queued Wayland events, then push our requests out.
            event_queue
                .dispatch_pending(&mut osk)
                .map_err(|e| format!("dispatch: {e}"))?;
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

            let ret = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, -1) };
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
            Command::Layer { target } => self.set_layer(target.unwrap_or_else(|| self.layer.toggled())),
            Command::Reflow { on } => self.set_reflow(on, qh),
            Command::Key { keycode } => self.type_keycode(keycode),
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
    /// it, refresh the highlight + the visible cursor sprite, and redraw
    /// (osk-technology.md §4.1). The haptic tick on key-crossing (§4.3) is
    /// DEFERRED.
    fn update_cursor(&mut self, pad: Pad, nx: f32, ny: f32) {
        match pad {
            Pad::Left => self.left_norm = Some((nx, ny)),
            Pad::Right => self.right_norm = Some((nx, ny)),
        }
        self.recompute_pad(pad);
        self.rebuild_highlights();
        self.rebuild_cursors();
        self.redraw_all();
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
                self.shift = self.shift.cycle_shift(); // Off→OneShot→Stuck→Off (§4.6)
            }
            KeyRole::Caps => {
                self.shift = self.shift.toggle_caps(); // Off↔Stuck (§4.6)
            }
            KeyRole::LayerToggle => {
                self.set_layer(self.layer.toggled());
                return; // set_layer re-places + redraws
            }
            KeyRole::DisplayToggle => {
                self.set_reflow(!self.reflow, qh);
                return; // set_reflow recreates the surface(s)
            }
            KeyRole::Meta => {
                // Arrows and other meta keys with a real keycode tap it; keycode
                // 0 (emoji/close) stays inert (§4.6, deferred).
                if key.keycode != 0 {
                    self.tap(key.keycode, false);
                }
            }
            KeyRole::Char => {
                // A single boolean drives both the drawn legend and the output,
                // so the committed character matches what the key shows. A
                // symbols-layer key can force shift (e.g. `!` from KEY_1).
                let shift = self.shift.is_active() || key.force_shift;
                self.tap(key.keycode, shift);
                self.shift = self.shift.after_char(); // one-shot clears
            }
            KeyRole::Backspace | KeyRole::Enter | KeyRole::Tab | KeyRole::Space => {
                self.tap(key.keycode, false);
            }
        }
        self.rebuild_highlights();
        self.redraw_all();
    }

    fn type_keycode(&mut self, keycode: u16) {
        self.tap(keycode, self.shift.is_active());
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

    /// Set the shift/caps state directly (control channel) and redraw so the
    /// legends and the Shift/Caps indicator update.
    fn set_shift(&mut self, state: ShiftState) {
        self.shift = state;
        self.redraw_all();
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
        self.redraw_all();
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

    fn redraw_all(&mut self) {
        let chrome = self.chrome();
        for i in 0..self.panels.len() {
            draw_panel(&mut self.pool, &self.theme, &self.keyboard, &mut self.panels[i], chrome);
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
        let chrome = self.chrome();
        draw_panel(&mut self.pool, &self.theme, &self.keyboard, &mut self.panels[idx], chrome);
    }
}

/// Draw one panel into a fresh shm buffer and commit it. Free function so the
/// disjoint borrows of `pool`, `theme`, `keyboard`, and one `panel` are clear.
fn draw_panel(pool: &mut SlotPool, theme: &Theme, keyboard: &Keyboard, panel: &mut PanelSurface, chrome: Chrome) {
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
    surface.attach(Some(buffer.wl_buffer()), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    panel.layer.commit();
    panel.buffer = Some(buffer);
    let _ = render::buffer_len; // keep the helper referenced; used by tests/tools
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
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
