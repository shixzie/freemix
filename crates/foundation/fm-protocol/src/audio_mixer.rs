//! Mixer strip labels, VCA-style strip groups, and the Fade-to-Black output
//! selection (Protocol 2.40, AU-001 and SW-004).
//!
//! Every input's Master strip carries an optional operator label and an
//! optional group (`1..=`[`AUDIO_GROUP_COUNT`]). A group carries no audio of
//! its own: a member's effective gain is its own fader plus the group fader
//! in dB, and a member is audible only while neither the strip nor its group
//! is muted. Strip `pan_basis_points` positions a mono source with a -3 dB
//! constant-power law, while `balance_basis_points` trims a stereo signal.
//!
//! | Operation | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`AudioMixerOperation::SetLabel`] | `audio_strip_label` | `input`, `label` (may be empty) | project edit |
//! | [`AudioMixerOperation::SetGroup`] | `audio_strip_group` | `input`, `group` (`-` for none) | project edit |
//! | [`AudioMixerOperation::RenameGroup`] | `audio_group_name` | `group`, `name` | project edit |
//! | [`AudioMixerOperation::SetGroupLevel`] | `audio_group_level` | `group`, `gain_millidb`, `muted` | live operation |
//!
//! Every accepted operation, undo and redo included, is announced by
//! `event=audio_mixer_changed` carrying the operation (`operation=<payload>`
//! and the same fields). `payload=fade_to_black_output` with `output` and
//! `affected` (`0|1`) chooses whether Fade to Black fades an output, a
//! project edit announced by `event=fade_to_black_output_changed`.
//!
//! A snapshot carries `audio_groups` (four `<name>~<gain>~<muted>` entries,
//! group 1 first), `input_audio_labels` (sparse `<input>~<label>` entries,
//! ascending), each strip's group inside `input_audio_strips`, and
//! `fade_to_black_excluded_outputs` (the outputs Fade to Black leaves live,
//! ascending; empty when it fades every output).

use crate::{WireInputId, WireOutputId};

/// How many strip groups a show has.
pub const AUDIO_GROUP_COUNT: usize = 4;
/// Longest strip label or group name, in UTF-8 bytes.
pub const MAX_AUDIO_LABEL_BYTES: usize = 32;

/// Whether `text` is a usable strip label (empty allowed) or, with
/// `allow_empty` false, a usable group name.
#[must_use]
pub fn audio_label_is_valid(text: &str, allow_empty: bool) -> bool {
    (allow_empty || !text.trim().is_empty())
        && text.len() <= MAX_AUDIO_LABEL_BYTES
        && !text.chars().any(char::is_control)
}

/// Whether `group` names one of the show's groups.
#[must_use]
pub const fn audio_group_is_valid(group: u8) -> bool {
    group >= 1 && group as usize <= AUDIO_GROUP_COUNT
}

/// One strip group's name, fader, and mute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioGroupStatus {
    pub name: String,
    pub gain_millidb: i32,
    pub muted: bool,
}

impl AudioGroupStatus {
    /// The default group `number`: "Group N" at unity, unmuted.
    #[must_use]
    pub fn default_for(number: u8) -> Self {
        Self {
            name: format!("Group {number}"),
            gain_millidb: 0,
            muted: false,
        }
    }
}

/// One labelled strip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputAudioLabel {
    pub input: WireInputId,
    pub label: String,
}

/// The mixer state a snapshot carries beyond each strip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioMixerStatus {
    /// The groups, group 1 first.
    pub groups: [AudioGroupStatus; AUDIO_GROUP_COUNT],
    /// Every non-empty strip label, ascending by input.
    pub labels: Vec<InputAudioLabel>,
}

impl Default for AudioMixerStatus {
    fn default() -> Self {
        Self {
            groups: [1, 2, 3, 4].map(AudioGroupStatus::default_for),
            labels: Vec::new(),
        }
    }
}

impl AudioMixerStatus {
    /// The label of `input`'s strip; empty when unlabelled.
    #[must_use]
    pub fn label(&self, input: WireInputId) -> &str {
        self.labels
            .iter()
            .find(|label| label.input == input)
            .map_or("", |label| label.label.as_str())
    }

    /// Applies one accepted operation.
    pub fn apply(&mut self, operation: &AudioMixerOperation) {
        match operation {
            AudioMixerOperation::SetLabel { input, label } => {
                self.labels.retain(|entry| entry.input != *input);
                if !label.is_empty() {
                    let index = self
                        .labels
                        .partition_point(|entry| entry.input.get() < input.get());
                    self.labels.insert(
                        index,
                        InputAudioLabel {
                            input: *input,
                            label: label.clone(),
                        },
                    );
                }
            }
            AudioMixerOperation::SetGroup { .. } => {}
            AudioMixerOperation::RenameGroup { group, name } => {
                if audio_group_is_valid(*group) {
                    self.groups[usize::from(*group) - 1].name.clone_from(name);
                }
            }
            AudioMixerOperation::SetGroupLevel {
                group,
                gain_millidb,
                muted,
            } => {
                if audio_group_is_valid(*group) {
                    let entry = &mut self.groups[usize::from(*group) - 1];
                    entry.gain_millidb = *gain_millidb;
                    entry.muted = *muted;
                }
            }
        }
    }
}

/// One mixer label, group, or group-level operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioMixerOperation {
    /// Labels `input`'s strip; empty clears the label.
    SetLabel { input: WireInputId, label: String },
    /// Assigns `input`'s strip to `group` (`1..=4`), or to none.
    SetGroup {
        input: WireInputId,
        group: Option<u8>,
    },
    /// Renames `group`.
    RenameGroup { group: u8, name: String },
    /// Moves `group`'s fader or mute.
    SetGroupLevel {
        group: u8,
        gain_millidb: i32,
        muted: bool,
    },
}

impl AudioMixerOperation {
    /// The command payload (and event operation) token.
    #[must_use]
    pub const fn payload(&self) -> &'static str {
        match self {
            Self::SetLabel { .. } => "audio_strip_label",
            Self::SetGroup { .. } => "audio_strip_group",
            Self::RenameGroup { .. } => "audio_group_name",
            Self::SetGroupLevel { .. } => "audio_group_level",
        }
    }

    /// Whether this is a live operation rather than a project edit.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::SetGroupLevel { .. })
    }
}

/// The Fade-to-Black output selection: the outputs it leaves live.
#[must_use]
pub fn fade_to_black_affects(excluded: &[WireOutputId], output: WireOutputId) -> bool {
    !excluded.contains(&output)
}
