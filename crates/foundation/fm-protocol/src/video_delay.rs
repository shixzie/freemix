//! Per-input video delays on the wire (IN-022, Protocol 2.160).
//!
//! A snapshot's `input_video_delays` is a comma-separated list of
//! `<input>:<frames>` in strictly ascending input order. `frames` is a
//! canonical decimal in `0..=`[`MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES`].

use crate::WireInputId;

/// Largest delay the wire accepts, in frames.
pub const MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES: u32 = 600;

/// One input's requested video delay in frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireInputVideoDelay {
    pub input: WireInputId,
    pub frames: u32,
}
