//! Small, portable domain types shared across `FreeMix`.

pub mod alert_rules;
mod audio;
pub mod audio_output;
pub mod capture;
mod color;
pub mod display;
mod fade_to_black;
pub mod garbage_matte;
mod id;
pub mod live_lan;
pub mod media_list_files;
mod memory;
pub mod network_stream;
pub mod paged;
pub mod reel;
pub mod replay;
mod scaling;
pub mod screen_capture;
pub mod still;
pub mod strip_dsp;
pub mod system_audio;
mod target;
mod time;
mod value_target;
mod video;

pub use audio::{AudioFormat, Channel, ChannelLayout, DiscreteChannel, SampleFormat, SampleRate};
pub use color::{
    AlphaMode, ChromaLocation, ColorMetadata, ColorPrimaries, MatrixCoefficients, SignalRange,
    TransferFunction,
};
pub use fade_to_black::{FadeToBlackCurve, MAX_FADE_TO_BLACK_DURATION_FRAMES};
pub use garbage_matte::{
    GARBAGE_MATTE_UNIT, GarbageMatte, GarbageMatteError, MAX_GARBAGE_MATTE_FEATHER,
    MAX_GARBAGE_MATTE_LAYER_SIDE, MAX_GARBAGE_MATTE_POINTS, MIN_GARBAGE_MATTE_POINTS, MattePoint,
};
pub use id::{BusId, InputId, OutputId, ProjectId, SceneId};
pub use memory::MemoryDomain;
pub use scaling::{OutputScaling, ParseOutputScalingError, ScaledViewport};
pub use target::{InputTarget, TargetBindings};
pub use time::{
    FrameRate, MediaDuration, MediaTimestamp, RateError, TimeBase, Timecode, TimecodeError,
};
pub use value_target::{CUE_GAIN_MAX_MILLIDB, CUE_GAIN_MIN_MILLIDB, CueGain, CueMute, FireValue};
pub use video::{
    PixelFormat, ScanMode, VideoDimensions, VideoFormat, VideoFrameMetadata,
    VideoFrameMetadataError,
};

pub const MAX_INPUT_NAME_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameInputError {
    UnknownInput(InputId),
    EmptyName,
    NameTooLong,
    DuplicateName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputOrderError {
    EmptyOrder,
    WrongLength { expected: usize, actual: usize },
    UnknownInput(InputId),
    DuplicateInput(InputId),
}

impl core::fmt::Display for InputOrderError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyOrder => formatter.write_str("input order must not be empty"),
            Self::WrongLength { expected, actual } => {
                write!(
                    formatter,
                    "input order must contain {expected} inputs, got {actual}"
                )
            }
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::DuplicateInput(input) => write!(formatter, "input {input} occurs more than once"),
        }
    }
}

impl std::error::Error for InputOrderError {}

/// Validates a complete reordered input vector against the current order.
///
/// The requested order must contain exactly the current inputs, once each.
///
/// # Errors
///
/// Returns [`InputOrderError::EmptyOrder`] for an empty request,
/// [`InputOrderError::WrongLength`] on a length mismatch,
/// [`InputOrderError::UnknownInput`] for an input outside the current set,
/// and [`InputOrderError::DuplicateInput`] when an input repeats.
pub fn validate_input_order(
    current: &[InputId],
    requested: &[InputId],
) -> Result<(), InputOrderError> {
    if requested.is_empty() {
        return Err(InputOrderError::EmptyOrder);
    }
    if requested.len() != current.len() {
        return Err(InputOrderError::WrongLength {
            expected: current.len(),
            actual: requested.len(),
        });
    }
    for input in requested {
        if !current.contains(input) {
            return Err(InputOrderError::UnknownInput(*input));
        }
    }
    for (index, input) in requested.iter().enumerate() {
        if requested[..index].contains(input) {
            return Err(InputOrderError::DuplicateInput(*input));
        }
    }
    Ok(())
}

impl core::fmt::Display for RenameInputError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::EmptyName => formatter.write_str("input name must not be empty"),
            Self::NameTooLong => {
                write!(
                    formatter,
                    "input name must not exceed {MAX_INPUT_NAME_BYTES} bytes"
                )
            }
            Self::DuplicateName => formatter.write_str("input name is already in use"),
        }
    }
}

impl std::error::Error for RenameInputError {}

/// Validates an input name against the bounded rename contract.
///
/// # Errors
///
/// Returns [`RenameInputError::EmptyName`] for a blank name,
/// [`RenameInputError::NameTooLong`] when the UTF-8 name exceeds
/// [`MAX_INPUT_NAME_BYTES`], and [`RenameInputError::DuplicateName`] is
/// reserved for callers that compare against existing inputs.
pub fn validate_input_name(name: &str) -> Result<(), RenameInputError> {
    if name.trim().is_empty() {
        Err(RenameInputError::EmptyName)
    } else if name.len() > MAX_INPUT_NAME_BYTES {
        Err(RenameInputError::NameTooLong)
    } else {
        Ok(())
    }
}
