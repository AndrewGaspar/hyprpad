//! `hyprpad-osk` — a controller-driven on-screen keyboard for Hyprland.
//!
//! This is the **kickoff skeleton**: a runnable, architecturally faithful
//! foundation that renders a QWERTY keyboard as a `zwlr_layer_shell_v1` OVERLAY
//! surface and types into the focused app via a uinput keyboard, with the
//! harder Steam-Deck-parity parts clearly stubbed and TODO'd against
//! `docs/research/osk-technology.md`. See `README.md` for the
//! done / stubbed / deferred map to the research sections.
//!
//! # Module map
//!
//! * [`layout`] — the shared key/keycode model (QWERTY grid + evdev scancodes,
//!   §4.6) and the geometry engine that serves BOTH presentation modes from it.
//!   [`layout::Hand`] is the ergonomic pivot: it is what lets Mode B's two
//!   edge-docked columns fall out of the same model that Mode A lays out
//!   horizontally.
//! * [`surface`] — the layer-shell anchoring / exclusive-zone policy. Bakes in
//!   the non-negotiable rules: OVERLAY tier only, destroy-on-dismiss, stable
//!   namespace for the `no_screen_share` / `above_lock` layer rules.
//! * [`theme`] — the theming foundation: the colour + geometry + font token
//!   table ([`theme::Theme`]) the draw path reads from, and the
//!   [`theme::ThemeSource`] seam (built-in / TOML file / Omarchy) that decides
//!   where those tokens come from. Size-to-content geometry is derived from
//!   [`theme::Geom`], never from the screen size.
//! * [`render`] — a small CPU renderer drawing keys + labels into an shm buffer.
//! * [`output`] — the uinput keyboard (real evdev keycodes → types everywhere,
//!   XWayland included; adapted from the parent crate's proven `src/output.rs`).
//! * [`control`] — the line-based control channel (unix socket or stdin) the
//!   hyprpad daemon drives, with a dual-trackpad-shaped vocabulary.
//! * [`predict`] — word completion and next-word prediction: the mmap'd
//!   unigram/bigram model, the composition buffer of what the keyboard typed,
//!   and the personal word cache (`docs/research/osk-prediction.md` phase 1).
//! * [`app`] — binds the globals, owns the live surfaces, and runs the poll
//!   loop that services Wayland and the control channel together.

pub mod app;
pub mod control;
pub mod layout;
pub mod output;
pub mod predict;
pub mod render;
pub mod surface;
pub mod theme;
