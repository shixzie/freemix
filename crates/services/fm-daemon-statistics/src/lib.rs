//! The daemon's statistics, CPU, and disk helpers.
//!
//! Extracted from `freemixd` as a mechanical move: the binary composes them
//! with [`fm-protocol`] and [`fm-observability`] the same way it did when
//! they lived beside `main.rs`.
//!
//! The items stay `pub(crate)` inside their modules where the binary does
//! not name them; only the module paths are public so `freemixd` can keep
//! using `statistics::…`, `cpu::…`, and `disk::…`.

// Public only so the daemon binary can use these items; this crate is not a
// stable API surface.
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

pub mod cpu;
pub mod disk;
pub mod statistics;
