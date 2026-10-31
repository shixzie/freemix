//! The daemon's native media runtime: the GPU composition root, source
//! playback, the audio mixer, and overlay layout.
//!
//! Extracted from `freemixd` as a mechanical move.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod native_media;
