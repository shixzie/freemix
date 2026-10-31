//! The daemon's operator status listener.
//!
//! Extracted from `freemixd` as a mechanical move: the binary composes it
//! with the health registry and `fm-server` readiness the same way it did
//! when it lived beside `main.rs`.

// Public only so the daemon binary can use these items; this crate is not a
// stable API surface.
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

use std::error::Error;

/// A failure whose message is already operator-facing.
pub type AppResult<T> = Result<T, Box<dyn Error>>;

pub mod status_listener;
