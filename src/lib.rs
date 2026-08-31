//! Library surface for `hyprsc`.
//!
//! The binary (`src/main.rs`) carries its own `mod` lines and does not depend
//! on this crate; `lib.rs` exists so integration tests, examples, and future
//! consumers can reach the modules by name.

pub mod report;
pub mod hidraw;
pub mod hypr;
