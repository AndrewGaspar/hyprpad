//! hyprsc library crate.
//!
//! The binary (`main.rs`) declares its own `mod report; mod hidraw;` for the
//! passive monitor. This library re-exports the same low-level modules plus the
//! higher-level layers — [`gesture`] recognition, [`config`] bindings,
//! [`arbitrate`] game-focus gating, and [`hypr`] compositor IPC — so unit and
//! integration tests and the daemon front-end build against them.

pub mod arbitrate;
pub mod config;
pub mod gesture;
pub mod hidraw;
pub mod hypr;
pub mod output;
pub mod report;
pub mod run;
