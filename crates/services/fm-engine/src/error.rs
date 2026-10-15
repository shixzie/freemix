use core::fmt;

use fm_clock::ClockError;
use fm_command::{CounterOverflow, FieldIssue, Rejection, RejectionCode};
use fm_scheduler::{ActionError, TickError};
use fm_switcher::{InputUsage, SwitcherError};
use fm_titles::{FieldId, FieldType, MAX_TITLE_CONTENT_BYTES, MAX_TITLE_TEXT_CHARS, UpdateError};
use fm_types::{InputId, MAX_INPUT_NAME_BYTES, OutputId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShowError {
    EmptyName,
    EmptyInputName,
    InputNameTooLong,
    DuplicateInputName,
    NoInputs,
    DuplicateInput,
    EmptyOutputName,
    DuplicateOutputName,
    DuplicateOutput(OutputId),
    UnknownInput(InputId),
    InvalidMediaPlayback(InputId, fm_model::MediaPlaybackSettingsError),
    /// The input category list or a lifecycle entry is invalid; carries the
    /// [`fm_model::InputLifecycleError::code`] (IN-026).
    InvalidInputLifecycle(&'static str),
    /// A strip label or group name is unusable (AU-001).
    InvalidAudioMixer(fm_model::AudioLabelError),
    /// A group fader is outside the strip gain range (AU-001).
    InvalidAudioGroupGain(i32),
    /// An output is not part of the show's output roster.
    UnknownOutput(OutputId),
    Switcher(SwitcherError),
}

impl fmt::Display for ShowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("show name must not be empty"),
            Self::EmptyInputName => formatter.write_str("show input names must not be empty"),
            Self::InputNameTooLong => write!(
                formatter,
                "show input names must not exceed {MAX_INPUT_NAME_BYTES} bytes"
            ),
            Self::DuplicateInputName => formatter.write_str("show input names must be unique"),
            Self::NoInputs => formatter.write_str("show must contain at least one input"),
            Self::DuplicateInput => formatter.write_str("show input identifiers must be unique"),
            Self::EmptyOutputName => formatter.write_str("show output names must not be empty"),
            Self::DuplicateOutputName => formatter.write_str("show output names must be unique"),
            Self::DuplicateOutput(output) => {
                write!(formatter, "show output identifier {output} is duplicated")
            }
            Self::UnknownInput(input) => write!(formatter, "show does not contain input {input}"),
            Self::InvalidMediaPlayback(input, error) => {
                write!(formatter, "media input {input} playback settings: {error}")
            }
            Self::InvalidInputLifecycle(code) => {
                write!(
                    formatter,
                    "input categories or lifecycle are invalid: {code}"
                )
            }
            Self::InvalidAudioMixer(error) => error.fmt(formatter),
            Self::InvalidAudioGroupGain(gain) => {
                write!(formatter, "audio group gain {gain} millidB is out of range")
            }
            Self::UnknownOutput(output) => {
                write!(formatter, "show does not contain output {output}")
            }
            Self::Switcher(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ShowError {}

/// Refusal of a media source report (IN-003).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineMediaError {
    /// The input is not a media input of the show.
    NotMediaInput(InputId),
    /// The source's rates cannot drive a transport.
    Playback(fm_playback::MediaControlError),
}

impl fmt::Display for EngineMediaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMediaInput(input) => write!(formatter, "input {input} is not a media input"),
            Self::Playback(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EngineMediaError {}

/// Refusal of a live input add or remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputCatalogueError {
    DuplicateInput(InputId),
    EmptyName,
    NameTooLong,
    DuplicateName,
    TooManyInputs {
        maximum: usize,
    },
    UnknownInput(InputId),
    InUse {
        input: InputId,
        usage: InputUsage,
    },
    /// A playlist entry plays the input.
    InPlaylist {
        input: InputId,
        entry: u64,
    },
}

impl fmt::Display for InputCatalogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInput(input) => write!(formatter, "input {input} already exists"),
            Self::EmptyName => formatter.write_str("input name must not be empty"),
            Self::NameTooLong => write!(
                formatter,
                "input name must not exceed {MAX_INPUT_NAME_BYTES} bytes"
            ),
            Self::DuplicateName => formatter.write_str("input name is already in use"),
            Self::TooManyInputs { maximum } => {
                write!(
                    formatter,
                    "the show already holds the maximum of {maximum} inputs"
                )
            }
            Self::UnknownInput(input) => write!(formatter, "show does not contain input {input}"),
            Self::InUse { input, usage } => {
                write!(formatter, "input {input} cannot be removed: it is {usage}")
            }
            Self::InPlaylist { input, entry } => write!(
                formatter,
                "input {input} cannot be removed: playlist entry {entry} plays it"
            ),
        }
    }
}

impl std::error::Error for InputCatalogueError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    WorkInFlight,
    IncompatibleSwitcher,
    MismatchedSwitcherRouting,
    MismatchedManualTransition,
    InvalidFrameCounter,
    ClockTimeMismatch {
        expected_ns: u64,
        actual_ns: u64,
    },
    UnrealizedAcceptedReceipt {
        target_frame: u64,
        frame_cursor: u64,
    },
    CounterMismatch {
        accepted_commands: u64,
        revision: u64,
        event_sequence: u64,
        runtime_generation: u64,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkInFlight => {
                formatter.write_str("cannot snapshot or restore while runtime work is in flight")
            }
            Self::IncompatibleSwitcher => formatter
                .write_str("persisted realized switcher inputs do not match the durable show"),
            Self::MismatchedSwitcherRouting => formatter.write_str(
                "persisted desired and realized switcher routing must match for an idle restore",
            ),
            Self::MismatchedManualTransition => formatter.write_str(
                "persisted desired and realized manual transitions must describe the same active mix",
            ),
            Self::InvalidFrameCounter => {
                formatter.write_str("snapshot frame counter cannot be restored")
            }
            Self::ClockTimeMismatch {
                expected_ns,
                actual_ns,
            } => write!(
                formatter,
                "persisted clock time {actual_ns}ns does not match the frame cursor deadline {expected_ns}ns"
            ),
            Self::UnrealizedAcceptedReceipt {
                target_frame,
                frame_cursor,
            } => write!(
                formatter,
                "accepted receipt targets frame {target_frame}, which has not elapsed before cursor {frame_cursor}"
            ),
            Self::CounterMismatch {
                accepted_commands,
                revision,
                event_sequence,
                runtime_generation,
            } => write!(
                formatter,
                "persisted counters must agree and cover the {accepted_commands} accepted receipts (revision {revision}, event sequence {event_sequence}, runtime generation {runtime_generation})"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    Clock(ClockError),
    Schedule(ActionError),
    Tick(TickError),
    RuntimeSwitcher(SwitcherError),
    CounterExhausted,
    Snapshot(SnapshotError),
    StalePreparation,
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clock(error) => error.fmt(formatter),
            Self::Schedule(error) => error.fmt(formatter),
            Self::Tick(error) => error.fmt(formatter),
            Self::RuntimeSwitcher(error) => error.fmt(formatter),
            Self::CounterExhausted => formatter.write_str("engine runtime generation exhausted"),
            Self::Snapshot(error) => error.fmt(formatter),
            Self::StalePreparation => {
                formatter.write_str("prepared engine execution was based on stale authority")
            }
        }
    }
}

impl std::error::Error for EngineError {}

impl From<ClockError> for EngineError {
    fn from(value: ClockError) -> Self {
        Self::Clock(value)
    }
}

impl From<ActionError> for EngineError {
    fn from(value: ActionError) -> Self {
        Self::Schedule(value)
    }
}

impl From<TickError> for EngineError {
    fn from(value: TickError) -> Self {
        Self::Tick(value)
    }
}

impl From<SwitcherError> for EngineError {
    fn from(value: SwitcherError) -> Self {
        Self::RuntimeSwitcher(value)
    }
}

impl From<CounterOverflow> for EngineError {
    fn from(_: CounterOverflow) -> Self {
        Self::CounterExhausted
    }
}

impl From<SnapshotError> for EngineError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

/// Stable classification of a title command refusal.
///
/// The engine reports these through ordinary [`Rejection`] receipts whose
/// first field issue carries [`TitleRejectionKind::code`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleRejectionKind {
    RevisionConflict,
    UnknownTitle,
    UnknownField,
    DuplicateField,
    FieldTypeMismatch,
    InvalidFieldValue,
    ContentTooLarge,
    OnAir,
    OffAir,
    OnProgram,
    OnMultipleOverlays,
    RevisionExhausted,
}

impl TitleRejectionKind {
    const ALL: [Self; 12] = [
        Self::RevisionConflict,
        Self::UnknownTitle,
        Self::UnknownField,
        Self::DuplicateField,
        Self::FieldTypeMismatch,
        Self::InvalidFieldValue,
        Self::ContentTooLarge,
        Self::OnAir,
        Self::OffAir,
        Self::OnProgram,
        Self::OnMultipleOverlays,
        Self::RevisionExhausted,
    ];

    /// Returns the stable field-issue code for this refusal.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RevisionConflict => "title_revision_conflict",
            Self::UnknownTitle => "unknown_title",
            Self::UnknownField => "unknown_field",
            Self::DuplicateField => "duplicate_field",
            Self::FieldTypeMismatch => "field_type_mismatch",
            Self::InvalidFieldValue => "invalid_field_value",
            Self::ContentTooLarge => "title_content_too_large",
            Self::OnAir => "title_on_air",
            Self::OffAir => "title_off_air",
            Self::OnProgram => "title_on_program",
            Self::OnMultipleOverlays => "title_on_multiple_overlays",
            Self::RevisionExhausted => "title_revision_exhausted",
        }
    }

    /// Returns the command field the refusal names.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::RevisionConflict | Self::RevisionExhausted => "base_revision",
            Self::UnknownField
            | Self::DuplicateField
            | Self::FieldTypeMismatch
            | Self::InvalidFieldValue
            | Self::ContentTooLarge => "fields",
            Self::UnknownTitle
            | Self::OnAir
            | Self::OffAir
            | Self::OnProgram
            | Self::OnMultipleOverlays => "input",
        }
    }

    /// Classifies a rejection produced by a title command.
    #[must_use]
    pub fn of(rejection: &Rejection) -> Option<Self> {
        let issue = rejection.fields.first()?;
        Self::ALL.into_iter().find(|kind| kind.code() == issue.code)
    }
}

/// A typed title command refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleRejection {
    /// `base_revision` is not the title's current field revision.
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    /// The input does not exist or is not a title.
    UnknownTitle(InputId),
    UnknownField(FieldId),
    DuplicateField(FieldId),
    FieldTypeMismatch {
        field: FieldId,
        expected: FieldType,
        actual: FieldType,
    },
    /// The value is well typed but not acceptable (a blank image asset).
    InvalidFieldValue(FieldId),
    /// A text value is longer than [`MAX_TITLE_TEXT_CHARS`].
    TextTooLong {
        field: FieldId,
        chars: usize,
    },
    /// The title content would exceed [`MAX_TITLE_CONTENT_BYTES`].
    ContentTooLarge {
        bytes: usize,
    },
    /// The command requires the title off air, but it is on air.
    OnAir(InputId),
    /// The command requires the title on air, but it is not.
    OffAir(InputId),
    /// The title is on the Program bus, which an overlay command cannot clear.
    OnProgram(InputId),
    /// The title is shown by more than one overlay channel.
    OnMultipleOverlays(InputId),
    RevisionExhausted,
}

impl TitleRejection {
    #[must_use]
    pub const fn kind(self) -> TitleRejectionKind {
        match self {
            Self::RevisionConflict { .. } => TitleRejectionKind::RevisionConflict,
            Self::UnknownTitle(_) => TitleRejectionKind::UnknownTitle,
            Self::UnknownField(_) => TitleRejectionKind::UnknownField,
            Self::DuplicateField(_) => TitleRejectionKind::DuplicateField,
            Self::FieldTypeMismatch { .. } => TitleRejectionKind::FieldTypeMismatch,
            Self::InvalidFieldValue(_) => TitleRejectionKind::InvalidFieldValue,
            Self::TextTooLong { .. } | Self::ContentTooLarge { .. } => {
                TitleRejectionKind::ContentTooLarge
            }
            Self::OnAir(_) => TitleRejectionKind::OnAir,
            Self::OffAir(_) => TitleRejectionKind::OffAir,
            Self::OnProgram(_) => TitleRejectionKind::OnProgram,
            Self::OnMultipleOverlays(_) => TitleRejectionKind::OnMultipleOverlays,
            Self::RevisionExhausted => TitleRejectionKind::RevisionExhausted,
        }
    }

    /// Converts this refusal into a receipt rejection with a stable field code.
    #[must_use]
    pub fn into_rejection(self) -> Rejection {
        let kind = self.kind();
        let code = match kind {
            TitleRejectionKind::RevisionConflict
            | TitleRejectionKind::OnAir
            | TitleRejectionKind::OffAir
            | TitleRejectionKind::OnProgram
            | TitleRejectionKind::OnMultipleOverlays => RejectionCode::Conflict,
            TitleRejectionKind::UnknownTitle => RejectionCode::NotFound,
            TitleRejectionKind::UnknownField
            | TitleRejectionKind::DuplicateField
            | TitleRejectionKind::FieldTypeMismatch
            | TitleRejectionKind::InvalidFieldValue
            | TitleRejectionKind::ContentTooLarge => RejectionCode::InvalidCommand,
            TitleRejectionKind::RevisionExhausted => RejectionCode::ResourceExhausted,
        };
        let message = self.to_string();
        Rejection::new(code, message.clone()).with_field(FieldIssue::new(
            kind.field(),
            kind.code(),
            message,
        ))
    }
}

impl From<UpdateError> for TitleRejection {
    fn from(error: UpdateError) -> Self {
        match error {
            UpdateError::RevisionConflict { expected, actual } => {
                Self::RevisionConflict { expected, actual }
            }
            UpdateError::UnknownField(field) => Self::UnknownField(field),
            UpdateError::DuplicateField(field) => Self::DuplicateField(field),
            UpdateError::TypeMismatch {
                field,
                expected,
                actual,
            } => Self::FieldTypeMismatch {
                field,
                expected,
                actual,
            },
            UpdateError::EmptyImageAsset(field) => Self::InvalidFieldValue(field),
            UpdateError::TextTooLong { field, chars } => Self::TextTooLong { field, chars },
            UpdateError::ContentTooLarge { bytes } => Self::ContentTooLarge { bytes },
            UpdateError::RevisionExhausted => Self::RevisionExhausted,
        }
    }
}

impl fmt::Display for TitleRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "title edit is based on revision {expected}, but the title is at revision {actual}"
            ),
            Self::UnknownTitle(input) => write!(formatter, "input {input} is not a title"),
            Self::UnknownField(field) => write!(formatter, "the title has no field {field}"),
            Self::DuplicateField(field) => write!(formatter, "field {field} is set twice"),
            Self::FieldTypeMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} expects {expected:?}, received {actual:?}"
            ),
            Self::InvalidFieldValue(field) => {
                write!(formatter, "field {field} names an empty image asset")
            }
            Self::TextTooLong { field, chars } => write!(
                formatter,
                "field {field} has {chars} characters, maximum is {MAX_TITLE_TEXT_CHARS}"
            ),
            Self::ContentTooLarge { bytes } => write!(
                formatter,
                "title content would be {bytes} bytes, maximum is {MAX_TITLE_CONTENT_BYTES}"
            ),
            Self::OnAir(input) => write!(formatter, "title {input} is on air"),
            Self::OffAir(input) => write!(formatter, "title {input} is not on air"),
            Self::OnProgram(input) => write!(
                formatter,
                "title {input} is on Program; switch Program away instead"
            ),
            Self::OnMultipleOverlays(input) => write!(
                formatter,
                "title {input} is shown by more than one overlay channel; take each off directly"
            ),
            Self::RevisionExhausted => formatter.write_str("title revision is exhausted"),
        }
    }
}

impl std::error::Error for TitleRejection {}
