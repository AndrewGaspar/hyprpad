//! hyprpad library crate.
//!
//! The binary (`main.rs`) declares its own `mod report; mod hidraw;` for the
//! passive monitor. This library re-exports the same low-level modules plus the
//! higher-level layers — [`gesture`] recognition, [`config`] bindings and its
//! [`lua_config`] front-end, [`mode`] modality (which subsumes [`arbitrate`]'s
//! binary game-focus gate), [`gamepad`] the virtual pad games see, and [`hypr`]
//! compositor IPC — so unit and integration tests and the daemon front-end
//! build against them.

pub mod arbitrate;
pub mod bindings_sheet;
pub mod config;
pub mod filter;
pub mod gamepad;
pub mod gesture;
pub mod haptics;
pub mod hidraw;
pub mod hypr;
pub mod keyboard;
pub mod lizard;
pub mod lua_config;
pub mod mode;
pub mod osk;
pub mod output;
pub mod report;
pub mod run;
pub mod setup;
pub mod status;
pub mod uhid;
