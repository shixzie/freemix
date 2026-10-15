//! Mixer strip labels and VCA-style strip groups (AU-001).
//!
//! Every input's Master strip carries an optional operator label and an
//! optional group; the show keeps [`fm_model::AUDIO_GROUP_COUNT`] groups,
//! each with a name, a fader, and a mute. A group carries no audio of its
//! own: a member's effective gain is its own fader plus the group fader in
//! dB, and a member is audible only while neither the strip nor its group is
//! muted (see `fm_audio::MixerGroup`).
//!
//! # Command classes
//!
//! - [`EngineAudioMixerEdit::SetLabel`], [`EngineAudioMixerEdit::SetGroup`],
//!   and [`EngineAudioMixerEdit::RenameGroup`] change durable configuration.
//!   They are project edits: refused while the project padlock (and, for a
//!   strip, the input's padlock) is engaged, and undoable with the exact
//!   previous label, membership, or name.
//! - [`EngineAudioMixerEdit::SetGroupLevel`] moves a group fader or mute. Like
//!   a strip fader move it is a live operation: never recorded, never
//!   reverted by undo, and never blocked by a padlock.
//!
//! Membership and group levels reach the media plane on the command's target
//! frame through [`crate::FrameResult::audio_mixer_updates`].

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::AudioGroupNumber;
use fm_types::InputId;

use crate::{MAX_INPUT_AUDIO_GAIN_MILLIDB, MIN_INPUT_AUDIO_GAIN_MILLIDB, ShowState};

/// One strip group's live fader and mute.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineAudioGroupState {
    pub gain_millidb: i32,
    pub muted: bool,
}

/// One strip group: its name and live fader state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineAudioGroup {
    pub name: String,
    pub state: EngineAudioGroupState,
}

impl EngineAudioGroup {
    /// The default group `number`: "Group N" at unity, unmuted.
    #[must_use]
    pub fn default_for(number: AudioGroupNumber) -> Self {
        Self {
            name: format!("Group {}", number.get()),
            state: EngineAudioGroupState::default(),
        }
    }
}

/// One mixer label, group, or group-level edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineAudioMixerEdit {
    /// Labels `input`'s strip; empty shows the input name. A project edit.
    SetLabel { input: InputId, label: String },
    /// Assigns `input`'s strip to `group`, or to none. A project edit.
    SetGroup {
        input: InputId,
        group: Option<AudioGroupNumber>,
    },
    /// Renames a group. A project edit.
    RenameGroup {
        group: AudioGroupNumber,
        name: String,
    },
    /// Moves a group fader or mute. A live operation.
    SetGroupLevel {
        group: AudioGroupNumber,
        state: EngineAudioGroupState,
    },
}

impl EngineAudioMixerEdit {
    /// Whether this edit is a live operation rather than a project edit.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::SetGroupLevel { .. })
    }

    /// The input whose padlock covers this edit.
    #[must_use]
    pub const fn locked_input(&self) -> Option<InputId> {
        match self {
            Self::SetLabel { input, .. } | Self::SetGroup { input, .. } => Some(*input),
            Self::RenameGroup { .. } | Self::SetGroupLevel { .. } => None,
        }
    }

    /// Whether this edit refers to `input`.
    #[must_use]
    pub fn names_input(&self, input: InputId) -> bool {
        self.locked_input() == Some(input)
    }
}

/// A mixer change the media plane applies on a realized frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineAudioMixerUpdate {
    /// `input`'s strip joined `group`, or left every group.
    Membership {
        input: InputId,
        group: Option<AudioGroupNumber>,
    },
    /// A group's fader or mute moved.
    GroupLevel {
        group: AudioGroupNumber,
        state: EngineAudioGroupState,
    },
}

impl EngineAudioMixerUpdate {
    /// The media-plane update an accepted edit realizes, if any.
    #[must_use]
    pub const fn of(edit: &EngineAudioMixerEdit) -> Option<Self> {
        match edit {
            EngineAudioMixerEdit::SetGroup { input, group } => Some(Self::Membership {
                input: *input,
                group: *group,
            }),
            EngineAudioMixerEdit::SetGroupLevel { group, state } => Some(Self::GroupLevel {
                group: *group,
                state: *state,
            }),
            EngineAudioMixerEdit::SetLabel { .. } | EngineAudioMixerEdit::RenameGroup { .. } => {
                None
            }
        }
    }
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

fn label_rejection(field: &str, error: fm_model::AudioLabelError) -> Rejection {
    let issue = match error {
        fm_model::AudioLabelError::TooLong => "too_long",
        fm_model::AudioLabelError::ControlCharacter => "control_character",
        fm_model::AudioLabelError::Empty => "empty",
    };
    rejection(
        RejectionCode::InvalidCommand,
        field,
        issue,
        error.to_string(),
    )
}

/// Validates `state` as a group level.
///
/// # Errors
///
/// Returns an invalid-command rejection for a gain outside the strip range.
pub(crate) fn validate_group_state(state: EngineAudioGroupState) -> Result<(), Rejection> {
    if (MIN_INPUT_AUDIO_GAIN_MILLIDB..=MAX_INPUT_AUDIO_GAIN_MILLIDB).contains(&state.gain_millidb) {
        Ok(())
    } else {
        Err(rejection(
            RejectionCode::InvalidCommand,
            "gain_millidb",
            "out_of_range",
            format!(
                "audio group gain must be between {MIN_INPUT_AUDIO_GAIN_MILLIDB} and \
                 {MAX_INPUT_AUDIO_GAIN_MILLIDB} millidB"
            ),
        ))
    }
}

fn unknown_input(input: InputId) -> Rejection {
    rejection(
        RejectionCode::NotFound,
        "input",
        "unknown_input",
        format!("input {input} is not part of this show"),
    )
}

/// Applies `edit` to the desired show.
///
/// # Errors
///
/// Returns a rejection for an unknown input, an unusable label or name, or
/// an out-of-range group level. The show is unchanged on error.
pub(crate) fn apply(state: &mut ShowState, edit: &EngineAudioMixerEdit) -> Result<(), Rejection> {
    match edit {
        EngineAudioMixerEdit::SetLabel { input, label } => {
            fm_model::validate_audio_strip_label(label)
                .map_err(|error| label_rejection("label", error))?;
            state
                .set_input_audio_label(*input, label.clone())
                .map_err(|_| unknown_input(*input))
        }
        EngineAudioMixerEdit::SetGroup { input, group } => state
            .set_input_audio_group(*input, *group)
            .map_err(|_| unknown_input(*input)),
        EngineAudioMixerEdit::RenameGroup { group, name } => {
            fm_model::validate_audio_group_name(name)
                .map_err(|error| label_rejection("name", error))?;
            state.audio_group_mut(*group).name.clone_from(name);
            Ok(())
        }
        EngineAudioMixerEdit::SetGroupLevel {
            group,
            state: level,
        } => {
            validate_group_state(*level)?;
            state.audio_group_mut(*group).state = *level;
            Ok(())
        }
    }
}

/// The exact edit that reverts project edit `edit` on `state`, or `None` for
/// a live group level or an unknown input.
pub(crate) fn inverse(
    state: &ShowState,
    edit: &EngineAudioMixerEdit,
) -> Option<EngineAudioMixerEdit> {
    match edit {
        EngineAudioMixerEdit::SetLabel { input, .. } => {
            state
                .inputs()
                .contains(input)
                .then(|| EngineAudioMixerEdit::SetLabel {
                    input: *input,
                    label: state.input_audio_label(*input).to_owned(),
                })
        }
        EngineAudioMixerEdit::SetGroup { input, .. } => {
            state
                .inputs()
                .contains(input)
                .then(|| EngineAudioMixerEdit::SetGroup {
                    input: *input,
                    group: state.input_audio_group(*input),
                })
        }
        EngineAudioMixerEdit::RenameGroup { group, .. } => {
            Some(EngineAudioMixerEdit::RenameGroup {
                group: *group,
                name: state.audio_group(*group).name.clone(),
            })
        }
        EngineAudioMixerEdit::SetGroupLevel { .. } => None,
    }
}
