//! hyprsc library crate.
//!
//! The binary (`main.rs`) declares its own `mod report; mod hidraw;` for the
//! passive monitor. This library re-exports the same modules plus the
//! higher-level [`gesture`] and [`config`] layers so that unit and integration
//! tests — and any future front-end — can build against them.

pub mod config;
pub mod gesture;
pub mod hidraw;
pub mod report;
