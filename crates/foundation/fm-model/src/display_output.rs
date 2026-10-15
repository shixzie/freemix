//! Project fullscreen display outputs (OR-011, schema 40).
//!
//! A project configures at most [`MAX_DISPLAY_OUTPUTS`] borderless,
//! cursor-hidden fullscreen display outputs, one per [`DisplayOutputSlot`].
//! Each slot chooses a display (a platform id with an optional name
//! fallback, a name, or a desktop index), a source (Program, Preview, or a
//! project output), a mode (`current` or an exact `WxH@mHz`), an
//! [`fm_types::OutputScaling`] policy, and an HDR request. Two slots never
//! claim the same display (see [`DisplaySelector::conflicts_with`]) and a
//! slot showing a project output keeps that output from being removed.

use fm_types::{
    OutputId,
    display::{
        DisplayOutputConfig, DisplayOutputSlot, DisplayOutputSource, DisplaySelector, ModeRequest,
        validate_display_text,
    },
};

pub use fm_types::display::MAX_DISPLAY_OUTPUTS;

/// Why a display output edit is refused. Nothing changes on error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetDisplayOutputError {
    /// `source=output:<id>` names an output the project does not have.
    UnknownOutput(OutputId),
    /// The other slot already claims the display.
    DisplayInUse {
        display: DisplaySelector,
        slot: DisplayOutputSlot,
    },
    /// The display id or name is unusable or the index is out of range.
    InvalidDisplay(DisplaySelector),
    /// The exact mode is outside the supported raster or refresh range.
    InvalidMode,
}

impl std::fmt::Display for SetDisplayOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutput(output) => write!(formatter, "output {output} does not exist"),
            Self::DisplayInUse { display, slot } => write!(
                formatter,
                "display {display} is already used by display output {slot}"
            ),
            Self::InvalidDisplay(display) => write!(formatter, "display {display} is invalid"),
            Self::InvalidMode => formatter.write_str("display mode is out of range"),
        }
    }
}

impl std::error::Error for SetDisplayOutputError {}

impl SetDisplayOutputError {
    /// A stable machine-readable refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownOutput(_) => "unknown_output",
            Self::DisplayInUse { .. } => "display_in_use",
            Self::InvalidDisplay(_) => "invalid_display",
            Self::InvalidMode => "invalid_mode",
        }
    }
}

/// Whether a selector's own fields are usable (text bounds and index
/// range); conflicts are checked against the other slot separately.
#[must_use]
pub fn display_selector_is_valid(selector: &DisplaySelector) -> bool {
    match selector {
        DisplaySelector::Id(id) => validate_display_text(id.as_str()).is_ok(),
        DisplaySelector::Name(name) => validate_display_text(name).is_ok(),
        DisplaySelector::IdOrName { id, name } => {
            validate_display_text(id.as_str()).is_ok() && validate_display_text(name).is_ok()
        }
        DisplaySelector::Index(index) => *index <= fm_types::display::MAX_DISPLAY_INDEX,
    }
}

/// Checks one slot's configuration against the project's outputs and the
/// other configured slots.
///
/// # Errors
///
/// Returns the first refusal.
pub fn check_display_output<'a>(
    slot: DisplayOutputSlot,
    config: &DisplayOutputConfig,
    outputs: &[OutputId],
    others: impl IntoIterator<Item = (DisplayOutputSlot, &'a DisplayOutputConfig)>,
) -> Result<(), SetDisplayOutputError> {
    if !display_selector_is_valid(&config.display) {
        return Err(SetDisplayOutputError::InvalidDisplay(
            config.display.clone(),
        ));
    }
    if let ModeRequest::Exact(mode) = config.mode
        && !mode.is_valid()
    {
        return Err(SetDisplayOutputError::InvalidMode);
    }
    if let DisplayOutputSource::Output(output) = config.source
        && !outputs.contains(&output)
    {
        return Err(SetDisplayOutputError::UnknownOutput(output));
    }
    for (other, existing) in others {
        if other != slot && existing.display.conflicts_with(&config.display) {
            return Err(SetDisplayOutputError::DisplayInUse {
                display: config.display.clone(),
                slot: other,
            });
        }
    }
    Ok(())
}
