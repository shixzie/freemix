//! The daemon's native media runtime: the GPU composition root, source
//! playback, the audio mixer, and overlay layout.
//!
//! Extracted from `freemixd` as a mechanical move.

// Public only so the daemon binary can use these items; this crate is not a
// stable API surface.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

pub mod native_media;
pub mod native_sync;
