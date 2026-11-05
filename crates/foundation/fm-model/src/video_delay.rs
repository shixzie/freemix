//! Per-input video delay (IN-022).
//!
//! A show may hold one input's pictures by a bounded number of frames before
//! the mixer and outputs see them. The persisted setting is the requested
//! delay; `fm-video`'s delay line applies it at the source clock.

use core::{fmt, num::NonZeroU128};

use fm_types::InputId;

/// Largest delay the project may store, in frames (ten seconds at 60 fps).
pub const MAX_INPUT_VIDEO_DELAY_FRAMES: u32 = 600;

/// Why an input video delay is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputVideoDelayError {
    /// The delay names an input the show does not contain.
    UnknownInput(InputId),
    /// The delay is longer than [`MAX_INPUT_VIDEO_DELAY_FRAMES`].
    TooLong { frames: u32, maximum: u32 },
}

impl fmt::Display for InputVideoDelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::TooLong { frames, maximum } => write!(
                formatter,
                "video delay {frames} frames is outside 0..={maximum}"
            ),
        }
    }
}

impl std::error::Error for InputVideoDelayError {}

/// Parses a one-based input from its stored number.
#[must_use]
pub fn input_from_u128(value: u128) -> Option<InputId> {
    NonZeroU128::new(value).map(InputId::new)
}

/// Checks one delay against the bound and the show's inputs.
///
/// # Errors
///
/// Returns [`InputVideoDelayError::UnknownInput`] for an input outside
/// `inputs`, or [`InputVideoDelayError::TooLong`] above the bound.
pub fn validate_input_video_delay(
    input: InputId,
    frames: u32,
    inputs: &[InputId],
) -> Result<(), InputVideoDelayError> {
    if !inputs.contains(&input) {
        return Err(InputVideoDelayError::UnknownInput(input));
    }
    if frames > MAX_INPUT_VIDEO_DELAY_FRAMES {
        return Err(InputVideoDelayError::TooLong {
            frames,
            maximum: MAX_INPUT_VIDEO_DELAY_FRAMES,
        });
    }
    Ok(())
}

/// Checks a complete delay map against the show's inputs.
///
/// # Errors
///
/// Returns the first unknown input or oversized delay in input order.
pub fn validate_input_video_delays(
    delays: &std::collections::BTreeMap<InputId, u32>,
    inputs: &[InputId],
) -> Result<(), InputVideoDelayError> {
    for (&input, &frames) in delays {
        validate_input_video_delay(input, frames, inputs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    #[test]
    fn delays_are_bounded_and_named() {
        let inputs = [input(1)];
        assert_eq!(validate_input_video_delay(input(1), 0, &inputs), Ok(()));
        assert_eq!(
            validate_input_video_delay(input(1), MAX_INPUT_VIDEO_DELAY_FRAMES + 1, &inputs),
            Err(InputVideoDelayError::TooLong {
                frames: MAX_INPUT_VIDEO_DELAY_FRAMES + 1,
                maximum: MAX_INPUT_VIDEO_DELAY_FRAMES,
            })
        );
        assert_eq!(
            validate_input_video_delay(input(2), 1, &inputs),
            Err(InputVideoDelayError::UnknownInput(input(2)))
        );
    }
}
