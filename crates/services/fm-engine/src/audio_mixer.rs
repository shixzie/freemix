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
//!   device. Choosing the current monitor device again only retries it on
//!   the media plane and records no undo entry.
//! - [`EngineAudioMixerEdit::SetGroupLevel`] moves a group fader or mute,
//!   [`EngineAudioMixerEdit::SetGroupSolo`] solos a group, and
//!   [`EngineAudioMixerEdit::SetMonitorLevel`] moves the monitor output level
//!   or mute. Like a strip fader or solo they are live operations: never
//!   recorded, never reverted by undo, and never blocked by a padlock.
//!
//! - [`EngineAudioMixerEdit::ResetLoudness`] restarts one input strip's or
//!   the Master bus's integrated loudness, loudness range, and maximum true
//!   peak (AU-005). A live operation that changes no show state: it is
//!   refused only for an unknown input, so a journal replay accepts it
//!   exactly as the live show did, and the media plane realizes it on the
//!   command's frame.
//!
//! - [`EngineAudioMixerEdit::SetStripDsp`] replaces one input strip's or the
//!   Master bus's DSP chain (AU-005). It is a project edit: refused while
//!   the project padlock (and, for a strip, the input's padlock) is
//!   engaged, and undoable with the exact previous chain. A knob drag sends
//!   its chain every few frames under one [`crate::GradeGesture`]: a
//!   continuing edit folds into the undo step its drag opened for the same
//!   target, so the whole drag undoes at once, and a drag back where it
//!   started leaves no step.
//!
//! - [`EngineAudioMixerEdit::RenameAuxBus`] renames an aux bus and
//!   [`EngineAudioMixerEdit::SetOutputAuxBus`] makes a project output hear
//!   an aux bus instead of Master (AU-002). Both are project edits, undoable
//!   with the exact previous name or bus. [`EngineAudioMixerEdit::SetAuxBusLevel`]
//!   moves an aux bus fader or mute: a live operation, like a group fader.
//!
//! Membership, group levels, solos, parents, the solo mode, the monitor
//! output, aux bus levels and output buses, and DSP chains reach the media
//! plane on the command's target
//! frame through [`crate::FrameResult::audio_mixer_updates`].

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{AudioGroupNumber, AudioSoloMode, AuxBusNumber};
use fm_types::strip_dsp::{StripDsp, StripDspTarget};
use fm_types::{InputId, OutputId};

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

/// One aux bus (AU-002): its name, live fader and mute, and DSP chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineAuxBus {
    pub name: String,
    pub state: EngineAudioGroupState,
    /// The bus's DSP chain, after its fader and mute.
    pub dsp: StripDsp,
}

impl EngineAuxBus {
    /// The default aux bus `bus`: "Bus A" through "Bus G" at unity,
    /// unmuted.
    #[must_use]
    pub fn default_for(bus: AuxBusNumber) -> Self {
        Self {
            name: format!("Bus {}", bus.letter()),
            state: EngineAudioGroupState::default(),
            dsp: StripDsp::DEFAULT,
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
    /// Replaces a strip's or the Master bus's plugin chain (AU-006). A
    /// project edit, persisted and undone like a DSP chain.
    SetPluginChain {
        target: StripDspTarget,
        /// Boxed: a chain is larger than every other command's payload.
        chain: Box<fm_plugin_host_api::PluginChain>,
    },
    /// Replaces a strip's or the Master bus's DSP chain (AU-005). A project
    /// edit; `gesture` folds a knob drag into one undo step.
    SetStripDsp {
        target: StripDspTarget,
        /// Boxed: a chain is larger than every other command's payload.
        chain: Box<StripDsp>,
        gesture: Option<crate::GradeGesture>,
    },
    /// Restarts a strip's or the Master bus's integrated loudness, loudness
    /// range, and maximum true peak. A live operation.
    ResetLoudness { target: StripDspTarget },
    /// Renames an aux bus (AU-002). A project edit.
    RenameAuxBus { bus: AuxBusNumber, name: String },
    /// Moves an aux bus fader or mute (AU-002). A live operation.
    SetAuxBusLevel {
        bus: AuxBusNumber,
        state: EngineAudioGroupState,
    },
    /// Replaces an aux bus's DSP chain (AU-002). A project edit.
    SetAuxBusDsp {
        bus: AuxBusNumber,
        /// Boxed: a chain is larger than every other command's payload.
        chain: Box<StripDsp>,
    },
    /// Makes `output` hear aux bus `bus`, or Master for `None` (AU-002). A
    /// project edit.
    SetOutputAuxBus {
        output: OutputId,
        bus: Option<AuxBusNumber>,
    },
}

impl EngineAudioMixerEdit {
    /// This edit without its drag gesture, as the history records it.
    #[must_use]
    pub const fn recorded(mut self) -> Self {
        if let Self::SetStripDsp { gesture, .. } = &mut self {
            *gesture = None;
        }
        self
    }

    /// The drag a DSP chain edit belongs to.
    #[must_use]
    pub const fn gesture(&self) -> Option<crate::GradeGesture> {
        match self {
            Self::SetStripDsp { gesture, .. } => *gesture,
            _ => None,
        }
    }

    /// The chain target of a DSP chain edit.
    #[must_use]
    pub const fn dsp_target(&self) -> Option<StripDspTarget> {
        match self {
            Self::SetStripDsp { target, .. } => Some(*target),
            _ => None,
        }
    }

    /// Whether this edit is a live operation rather than a project edit.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::SetGroupLevel { .. }
                | Self::SetGroupSolo { .. }
                | Self::SetMonitorLevel { .. }
                | Self::ResetLoudness { .. }
                | Self::SetAuxBusLevel { .. }
        )
    }

    /// The input whose padlock covers this edit.
    #[must_use]
    pub const fn locked_input(&self) -> Option<InputId> {
        match self {
            Self::SetLabel { input, .. }
            | Self::SetGroup { input, .. }
            | Self::SetStripDsp {
                target: StripDspTarget::Input(input),
                ..
            }
            | Self::SetPluginChain {
                target: StripDspTarget::Input(input),
                ..
            } => Some(*input),
            Self::SetStripDsp {
                target: StripDspTarget::Master | StripDspTarget::Aux(_),
                ..
            }
            | Self::SetPluginChain {
                target: StripDspTarget::Master | StripDspTarget::Aux(_),
                ..
            }
            | Self::RenameGroup { .. }
            | Self::SetGroupLevel { .. }
            | Self::SetGroupSolo { .. }
            | Self::SetGroupParent { .. }
            | Self::SetSoloMode { .. }
            | Self::SetMonitorDevice { .. }
            | Self::SetMonitorLevel { .. }
            | Self::ResetLoudness { .. }
            | Self::RenameAuxBus { .. }
            | Self::SetAuxBusLevel { .. }
            | Self::SetAuxBusDsp { .. }
            | Self::SetOutputAuxBus { .. } => None,
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
    /// A strip's or the Master bus's plugin chain changed (AU-006).
    PluginChain {
        target: StripDspTarget,
        chain: Box<fm_plugin_host_api::PluginChain>,
    },
    /// A strip's or the Master bus's DSP chain changed (AU-005).
    StripDsp {
        target: StripDspTarget,
        chain: StripDsp,
    },
    /// A strip's or the Master bus's integrated loudness, loudness range,
    /// and maximum true peak start over (AU-005).
    LoudnessReset { target: StripDspTarget },
    /// An aux bus fader or mute moved (AU-002).
    AuxBusLevel {
        bus: AuxBusNumber,
        state: EngineAudioGroupState,
    },
    /// An aux bus's DSP chain changed (AU-002).
    AuxBusDsp {
        bus: AuxBusNumber,
        chain: Box<StripDsp>,
    },
    /// An output now hears an aux bus, or Master for `None` (AU-002).
    OutputAuxBus {
        output: OutputId,
        bus: Option<AuxBusNumber>,
    },
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
            EngineAudioMixerEdit::SetStripDsp { target, chain, .. } => Some(Self::StripDsp {
                target: *target,
                chain: **chain,
            }),
            EngineAudioMixerEdit::SetPluginChain { target, chain } => Some(Self::PluginChain {
                target: *target,
                chain: chain.clone(),
            }),
            EngineAudioMixerEdit::ResetLoudness { target } => {
                Some(Self::LoudnessReset { target: *target })
            }
            EngineAudioMixerEdit::SetAuxBusLevel { bus, state } => Some(Self::AuxBusLevel {
                bus: *bus,
                state: *state,
            }),
            EngineAudioMixerEdit::SetAuxBusDsp { bus, chain } => Some(Self::AuxBusDsp {
                bus: *bus,
                chain: chain.clone(),
            }),
            EngineAudioMixerEdit::SetOutputAuxBus { output, bus } => Some(Self::OutputAuxBus {
                output: *output,
                bus: *bus,
            }),
            EngineAudioMixerEdit::SetLabel { .. }
            | EngineAudioMixerEdit::RenameGroup { .. }
            | EngineAudioMixerEdit::RenameAuxBus { .. } => None,
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
/// Replaces one plugin chain: only installed processors are accepted
/// (AU-006).
fn apply_plugin_chain(
    state: &mut ShowState,
    target: StripDspTarget,
    chain: &fm_plugin_host_api::PluginChain,
) -> Result<(), Rejection> {
    for slot in chain.slots() {
        if !fm_plugin_host_api::is_reference_plugin(slot.plugin()) {
            return Err(rejection(
                RejectionCode::InvalidCommand,
                "plugins",
                "unsupported_plugin",
                format!("plugin {} has no installed processor", slot.plugin()),
            ));
        }
    }
    // Validate the saved state now, so a chain the media plane cannot build
    // is refused at the command instead of faulting a later frame.
    if fm_plugin_host_api::ReferenceRack::new(chain, 48_000.0).is_err() {
        return Err(rejection(
            RejectionCode::InvalidCommand,
            "plugins",
            "invalid_state",
            "a plugin slot holds invalid saved state".to_owned(),
        ));
    }
    match target {
        StripDspTarget::Master => {
            state.set_master_audio_plugins(chain.clone());
            Ok(())
        }
        StripDspTarget::Input(input) => {
            loudness_target(state, StripDspTarget::Input(input))?;
            state.set_input_audio_plugins(input, chain.clone());
            Ok(())
        }
        // Aux buses carry a DSP chain but no plugin chain.
        StripDspTarget::Aux(_) => Err(rejection(
            RejectionCode::InvalidCommand,
            "target",
            "unsupported_target",
            "an aux bus carries no plugin chain".to_owned(),
        )),
    }
}

/// Returns a rejection for an unknown input, an unusable label or name, or
/// an out-of-range group level. The show is unchanged on error.
pub(crate) fn apply(state: &mut ShowState, edit: &EngineAudioMixerEdit) -> Result<(), Rejection> {
    match edit {
        EngineAudioMixerEdit::SetPluginChain { target, chain } => {
            apply_plugin_chain(state, *target, chain)
        }
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
        EngineAudioMixerEdit::SetStripDsp { target, chain, .. } => {
            chain.validate().map_err(|error| {
                rejection(
                    RejectionCode::InvalidCommand,
                    "chain",
                    "out_of_range",
                    error.to_string(),
                )
            })?;
            match *target {
                StripDspTarget::Master => state.set_master_audio_dsp(**chain),
                StripDspTarget::Input(input) => state
                    .set_input_audio_dsp(input, **chain)
                    .map_err(|_| unknown_input(input))?,
                StripDspTarget::Aux(bus) => state.aux_bus_mut(bus).dsp = **chain,
            }
            Ok(())
        }
        EngineAudioMixerEdit::ResetLoudness { target } => loudness_target(state, *target),
        EngineAudioMixerEdit::RenameAuxBus { .. }
        | EngineAudioMixerEdit::SetAuxBusLevel { .. }
        | EngineAudioMixerEdit::SetAuxBusDsp { .. }
        | EngineAudioMixerEdit::SetOutputAuxBus { .. } => apply_aux(state, edit),
    }
}

/// Applies an aux bus rename, level, or output bus edit (AU-002).
fn apply_aux(state: &mut ShowState, edit: &EngineAudioMixerEdit) -> Result<(), Rejection> {
    match edit {
        EngineAudioMixerEdit::RenameAuxBus { bus, name } => {
            fm_model::validate_audio_group_name(name)
                .map_err(|error| label_rejection("name", error))?;
            state.aux_bus_mut(*bus).name.clone_from(name);
            Ok(())
        }
        EngineAudioMixerEdit::SetAuxBusDsp { bus, chain } => {
            chain.validate().map_err(|error| {
                rejection(
                    RejectionCode::InvalidCommand,
                    "chain",
                    "out_of_range",
                    error.to_string(),
                )
            })?;
            state.aux_bus_mut(*bus).dsp = **chain;
            Ok(())
        }
        EngineAudioMixerEdit::SetAuxBusLevel { bus, state: level } => {
            validate_group_state(*level).map_err(|_| {
                rejection(
                    RejectionCode::InvalidCommand,
                    "gain_millidb",
                    "out_of_range",
                    format!(
                        "aux bus gain must be between {MIN_INPUT_AUDIO_GAIN_MILLIDB} and \
                         {MAX_INPUT_AUDIO_GAIN_MILLIDB} millidB"
                    ),
                )
            })?;
            state.aux_bus_mut(*bus).state = *level;
            Ok(())
        }
        EngineAudioMixerEdit::SetOutputAuxBus { output, bus } => {
            if !state
                .outputs()
                .iter()
                .any(|(candidate, _)| candidate == output)
            {
                return Err(rejection(
                    RejectionCode::NotFound,
                    "output",
                    "unknown_output",
                    format!("show does not contain output {output}"),
                ));
            }
            state.set_output_aux_bus(*output, *bus);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Validates a loudness reset's target: Master, an input of the show, or an
/// aux bus (which reports no loudness).
fn loudness_target(state: &ShowState, target: StripDspTarget) -> Result<(), Rejection> {
    match target {
        StripDspTarget::Master | StripDspTarget::Aux(_) => Ok(()),
        StripDspTarget::Input(input) if state.inputs().contains(&input) => Ok(()),
        StripDspTarget::Input(input) => Err(unknown_input(input)),
    }
}

/// The exact edit that reverts project edit `edit` on `state`, or `None` for
/// a live group level, solo, monitor level, or loudness reset, or an unknown
/// input.
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
        // Choosing the current device again only retries it (the media plane
        // reopens it); the project is unchanged, so nothing is recorded.
        EngineAudioMixerEdit::SetMonitorDevice { device } => {
            let current = &state.audio_monitor().device;
            (current != device).then(|| EngineAudioMixerEdit::SetMonitorDevice {
                device: current.clone(),
            })
        }
        EngineAudioMixerEdit::SetStripDsp { target, .. } => match *target {
            StripDspTarget::Master => Some(EngineAudioMixerEdit::SetStripDsp {
                target: *target,
                chain: Box::new(*state.master_audio_dsp()),
                gesture: None,
            }),
            StripDspTarget::Aux(bus) => Some(EngineAudioMixerEdit::SetStripDsp {
                target: *target,
                chain: Box::new(state.aux_bus(bus).dsp),
                gesture: None,
            }),
            StripDspTarget::Input(input) => {
                state
                    .inputs()
                    .contains(&input)
                    .then(|| EngineAudioMixerEdit::SetStripDsp {
                        target: *target,
                        chain: Box::new(state.input_audio_dsp(input)),
                        gesture: None,
                    })
            }
        },
        EngineAudioMixerEdit::SetPluginChain { target, .. } => match *target {
            StripDspTarget::Master => Some(EngineAudioMixerEdit::SetPluginChain {
                target: *target,
                chain: Box::new(state.master_audio_plugins().clone()),
            }),
            StripDspTarget::Input(input) => {
                state
                    .inputs()
                    .contains(&input)
                    .then(|| EngineAudioMixerEdit::SetPluginChain {
                        target: *target,
                        chain: Box::new(state.input_audio_plugins(input).clone()),
                    })
            }
            // An aux bus carries no plugin chain, so such an edit never
            // reaches the history.
            StripDspTarget::Aux(_) => None,
        },
        EngineAudioMixerEdit::RenameAuxBus { bus, .. } => {
            Some(EngineAudioMixerEdit::RenameAuxBus {
                bus: *bus,
                name: state.aux_bus(*bus).name.clone(),
            })
        }
        EngineAudioMixerEdit::SetAuxBusDsp { bus, .. } => {
            Some(EngineAudioMixerEdit::SetAuxBusDsp {
                bus: *bus,
                chain: Box::new(state.aux_bus(*bus).dsp),
            })
        }
        EngineAudioMixerEdit::SetOutputAuxBus { output, .. } => state
            .outputs()
            .iter()
            .any(|(candidate, _)| candidate == output)
            .then(|| EngineAudioMixerEdit::SetOutputAuxBus {
                output: *output,
                bus: state.output_aux_bus(*output),
            }),
        EngineAudioMixerEdit::SetGroupLevel { .. }
        | EngineAudioMixerEdit::SetGroupSolo { .. }
        | EngineAudioMixerEdit::SetMonitorLevel { .. }
        | EngineAudioMixerEdit::ResetLoudness { .. }
        | EngineAudioMixerEdit::SetAuxBusLevel { .. } => None,
    }
}
