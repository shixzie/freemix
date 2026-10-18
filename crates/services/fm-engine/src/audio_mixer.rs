//! Mixer strip labels, VCA-style strip groups, the solo mode, and the
//! monitor output (AU-001).
//!
//! Every input's Master strip carries an optional operator label and an
//! optional group; the show keeps [`fm_model::AUDIO_GROUP_COUNT`] groups,
//! each with a name, a fader, a mute, a solo, and an optional parent group.
//! A group carries no audio of its own: a member's effective gain is its own
//! fader plus every group fader on its parent chain in dB, a member is
//! audible only while neither the strip nor any group on the chain is muted,
//! and a soloed group on the chain solos the member (see
//! `fm_audio::MixerGroup`). Parents never form a cycle. The show's
//! [`fm_model::AudioSoloMode`] chooses whether a solo gates Program
//! (solo-in-place) or is heard on the monitor bus pre-fader (PFL) or
//! after-fader (AFL). The show's monitor output plays that bus on a local
//! audio output device (headphones) at its own level and mute, independent
//! of Program; without a device the bus is only metered.
//!
//! # Command classes
//!
//! - [`EngineAudioMixerEdit::SetLabel`], [`EngineAudioMixerEdit::SetGroup`],
//!   [`EngineAudioMixerEdit::RenameGroup`],
//!   [`EngineAudioMixerEdit::SetGroupParent`],
//!   [`EngineAudioMixerEdit::SetSoloMode`], and
//!   [`EngineAudioMixerEdit::SetMonitorDevice`] change durable
//!   configuration. They are project edits: refused while the project
//!   padlock (and, for a strip, the input's padlock) is engaged, and undoable
//!   with the exact previous label, membership, name, parent, mode, or
//!   device.
//! - [`EngineAudioMixerEdit::SetGroupLevel`] moves a group fader or mute,
//!   [`EngineAudioMixerEdit::SetGroupSolo`] solos a group, and
//!   [`EngineAudioMixerEdit::SetMonitorLevel`] moves the monitor output level
//!   or mute. Like a strip fader or solo they are live operations: never
//!   recorded, never reverted by undo, and never blocked by a padlock.
//!
//! Membership, group levels, solos, parents, the solo mode, and the monitor
//! output reach the media plane on the command's target frame through
//! [`crate::FrameResult::audio_mixer_updates`].

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{AudioGroupNumber, AudioSoloMode};
use fm_types::InputId;

use crate::{MAX_INPUT_AUDIO_GAIN_MILLIDB, MIN_INPUT_AUDIO_GAIN_MILLIDB, ShowState};

/// One strip group's live fader and mute.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineAudioGroupState {
    pub gain_millidb: i32,
    pub muted: bool,
}

/// One strip group: its name, live fader state and solo, and parent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineAudioGroup {
    pub name: String,
    pub state: EngineAudioGroupState,
    /// Whether the group is soloed, soloing every member. Live state.
    pub soloed: bool,
    /// The group this group is nested in. Durable configuration.
    pub parent: Option<AudioGroupNumber>,
}

impl EngineAudioGroup {
    /// The default group `number`: "Group N" at unity, unmuted, unsoloed,
    /// and top-level.
    #[must_use]
    pub fn default_for(number: AudioGroupNumber) -> Self {
        Self {
            name: format!("Group {}", number.get()),
            state: EngineAudioGroupState::default(),
            soloed: false,
            parent: None,
        }
    }
}

/// The monitor output: the device the monitor bus plays on and its live
/// level and mute.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineAudioMonitor {
    /// The local audio output device, or `None` for meter-only. Durable
    /// configuration.
    pub device: Option<String>,
    /// The monitor feed level and mute. Live state.
    pub state: EngineAudioGroupState,
}

/// Whether nesting `group` inside `parent` keeps `groups` cycle-free.
#[must_use]
pub fn audio_group_parent_is_acyclic(
    groups: &[EngineAudioGroup; fm_model::AUDIO_GROUP_COUNT],
    group: AudioGroupNumber,
    parent: Option<AudioGroupNumber>,
) -> bool {
    let mut next = parent;
    for _ in 0..fm_model::AUDIO_GROUP_COUNT {
        match next {
            None => return true,
            Some(ancestor) if ancestor == group => return false,
            Some(ancestor) => next = groups[ancestor.index()].parent,
        }
    }
    false
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
    /// Solos or unsolos a group and with it every member. A live operation.
    SetGroupSolo {
        group: AudioGroupNumber,
        soloed: bool,
    },
    /// Nests a group inside `parent`, or makes it top-level. A project edit.
    SetGroupParent {
        group: AudioGroupNumber,
        parent: Option<AudioGroupNumber>,
    },
    /// Chooses solo-in-place, PFL, or AFL. A project edit.
    SetSoloMode { mode: AudioSoloMode },
    /// Plays the monitor bus on a local audio output device, or on none. A
    /// project edit.
    SetMonitorDevice { device: Option<String> },
    /// Moves the monitor output level or mute. A live operation.
    SetMonitorLevel { state: EngineAudioGroupState },
}

impl EngineAudioMixerEdit {
    /// Whether this edit is a live operation rather than a project edit.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::SetGroupLevel { .. } | Self::SetGroupSolo { .. } | Self::SetMonitorLevel { .. }
        )
    }

    /// The input whose padlock covers this edit.
    #[must_use]
    pub const fn locked_input(&self) -> Option<InputId> {
        match self {
            Self::SetLabel { input, .. } | Self::SetGroup { input, .. } => Some(*input),
            Self::RenameGroup { .. }
            | Self::SetGroupLevel { .. }
            | Self::SetGroupSolo { .. }
            | Self::SetGroupParent { .. }
            | Self::SetSoloMode { .. }
            | Self::SetMonitorDevice { .. }
            | Self::SetMonitorLevel { .. } => None,
        }
    }

    /// Whether this edit refers to `input`.
    #[must_use]
    pub fn names_input(&self, input: InputId) -> bool {
        self.locked_input() == Some(input)
    }
}

/// A mixer change the media plane applies on a realized frame.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// A group was soloed or unsoloed.
    GroupSolo {
        group: AudioGroupNumber,
        soloed: bool,
    },
    /// A group was nested in `parent`, or made top-level.
    GroupParent {
        group: AudioGroupNumber,
        parent: Option<AudioGroupNumber>,
    },
    /// The solo mode changed.
    SoloMode(AudioSoloMode),
    /// The monitor output device was chosen again, even if unchanged, so the
    /// media plane reopens it.
    MonitorDevice(Option<String>),
    /// The monitor output level or mute moved.
    MonitorLevel(EngineAudioGroupState),
}

impl EngineAudioMixerUpdate {
    /// The media-plane update an accepted edit realizes, if any.
    #[must_use]
    pub fn of(edit: &EngineAudioMixerEdit) -> Option<Self> {
        match edit {
            EngineAudioMixerEdit::SetGroup { input, group } => Some(Self::Membership {
                input: *input,
                group: *group,
            }),
            EngineAudioMixerEdit::SetGroupLevel { group, state } => Some(Self::GroupLevel {
                group: *group,
                state: *state,
            }),
            EngineAudioMixerEdit::SetGroupSolo { group, soloed } => Some(Self::GroupSolo {
                group: *group,
                soloed: *soloed,
            }),
            EngineAudioMixerEdit::SetGroupParent { group, parent } => Some(Self::GroupParent {
                group: *group,
                parent: *parent,
            }),
            EngineAudioMixerEdit::SetSoloMode { mode } => Some(Self::SoloMode(*mode)),
            EngineAudioMixerEdit::SetMonitorDevice { device } => {
                Some(Self::MonitorDevice(device.clone()))
            }
            EngineAudioMixerEdit::SetMonitorLevel { state } => Some(Self::MonitorLevel(*state)),
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
    .with_reason(crate::error::unknown_input_reason(input))
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
        EngineAudioMixerEdit::SetGroupSolo { group, soloed } => {
            state.audio_group_mut(*group).soloed = *soloed;
            Ok(())
        }
        EngineAudioMixerEdit::SetGroupParent { group, parent } => {
            if !audio_group_parent_is_acyclic(state.audio_groups(), *group, *parent) {
                return Err(rejection(
                    RejectionCode::InvalidCommand,
                    "parent",
                    "cycle",
                    format!("{group} cannot be nested inside itself"),
                ));
            }
            state.audio_group_mut(*group).parent = *parent;
            Ok(())
        }
        EngineAudioMixerEdit::SetSoloMode { mode } => {
            state.set_audio_solo_mode(*mode);
            Ok(())
        }
        EngineAudioMixerEdit::SetMonitorDevice { device } => {
            if let Some(device) = device {
                fm_model::validate_audio_monitor_device(device).map_err(|error| {
                    rejection(
                        RejectionCode::InvalidCommand,
                        "device",
                        "invalid_device",
                        error.to_string(),
                    )
                })?;
            }
            state.audio_monitor_mut().device.clone_from(device);
            Ok(())
        }
        EngineAudioMixerEdit::SetMonitorLevel { state: level } => {
            validate_group_state(*level).map_err(|_| {
                rejection(
                    RejectionCode::InvalidCommand,
                    "gain_millidb",
                    "out_of_range",
                    format!(
                        "monitor output gain must be between {MIN_INPUT_AUDIO_GAIN_MILLIDB} and \
                         {MAX_INPUT_AUDIO_GAIN_MILLIDB} millidB"
                    ),
                )
            })?;
            state.audio_monitor_mut().state = *level;
            Ok(())
        }
    }
}

/// The exact edit that reverts project edit `edit` on `state`, or `None` for
/// a live group level, solo, or monitor level, or an unknown input.
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
        EngineAudioMixerEdit::SetGroupParent { group, .. } => {
            Some(EngineAudioMixerEdit::SetGroupParent {
                group: *group,
                parent: state.audio_group(*group).parent,
            })
        }
        EngineAudioMixerEdit::SetSoloMode { .. } => Some(EngineAudioMixerEdit::SetSoloMode {
            mode: state.audio_solo_mode(),
        }),
        EngineAudioMixerEdit::SetMonitorDevice { .. } => {
            Some(EngineAudioMixerEdit::SetMonitorDevice {
                device: state.audio_monitor().device.clone(),
            })
        }
        EngineAudioMixerEdit::SetGroupLevel { .. }
        | EngineAudioMixerEdit::SetGroupSolo { .. }
        | EngineAudioMixerEdit::SetMonitorLevel { .. } => None,
    }
}
