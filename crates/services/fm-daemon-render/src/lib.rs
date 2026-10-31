//! The daemon's shared renderers: grades, stills, the CPU scene reference,
//! title rendering, and the simulated monitor tap.
//!
//! Extracted from `freemixd` as a mechanical move.

// Public only so the daemon binary can use these items; this crate is not a
// stable API surface.
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

pub mod grading;
pub mod monitor_tap;
pub mod scene_render;
pub mod stills;
pub mod title_render;
