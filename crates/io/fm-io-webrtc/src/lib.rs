//! Transport-neutral WebRTC signaling, media, and data-channel contracts.
//!
//! This crate intentionally contains no browser or native WebRTC adapter. The
//! [`fake`] module supplies a deterministic in-memory implementation for tests
//! and simulations.
//!
//! [`sdp`], [`signaling`], and [`bandwidth`] are deterministic, network-free
//! building blocks for encrypted browser guests: a strict SDP offer/answer
//! subset, a perfect-negotiation signaling state machine with bounded ICE
//! trickle and ICE restart, and a ladder-based bandwidth controller capped at
//! 1080p/4 Mbps.

pub mod bandwidth;
mod contract;
pub mod fake;
pub mod sdp;
pub mod signaling;

pub use contract::*;

#[cfg(test)]
mod tests;
