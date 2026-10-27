//! Mixer strip labels, VCA-style strip groups, the solo mode, the monitor
//! output, and the Fade-to-Black output selection (Protocols 2.40, 2.44, and
//! 2.53, AU-001 and SW-004).
//!
//! Every input's Master strip carries an optional operator label and an
//! optional group (`1..=`[`AUDIO_GROUP_COUNT`]). A group carries no audio of
//! its own: a member's effective gain is its own fader plus the group fader
//! in dB, and a member is audible only while neither the strip nor its group
//! is muted. Strip `pan_basis_points` positions a mono source with a -3 dB
//! constant-power law, while `balance_basis_points` trims a stereo signal.
//!
//! Since 2.44 a group can be soloed (soloing every member) and nested in a
//! parent group: a member then follows every group on its parent chain (the
//! gains add in dB, any muted group mutes, any soloed group solos), and
//! parents never form a cycle. The show's [`AudioSoloMode`] chooses whether
//! a solo gates Program (`sip`, solo-in-place) or is heard on the monitor
//! bus pre-fader (`pfl`) or after-fader (`afl`) without touching Program.
//!
//! Since 2.53 the monitor bus can play on a local audio output device
//! (headphones) at its own level and mute, independent of Program: the
//! device is a project edit and the level a live operation. Without a device
//! the bus is only metered.
//!
//! | Operation | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`AudioMixerOperation::SetLabel`] | `audio_strip_label` | `input`, `label` (may be empty) | project edit |
//! | [`AudioMixerOperation::SetGroup`] | `audio_strip_group` | `input`, `group` (`-` for none) | project edit |
//! | [`AudioMixerOperation::RenameGroup`] | `audio_group_name` | `group`, `name` | project edit |
//! | [`AudioMixerOperation::SetGroupLevel`] | `audio_group_level` | `group`, `gain_millidb`, `muted` | live operation |
//! | [`AudioMixerOperation::SetGroupSolo`] | `audio_group_solo` | `group`, `soloed` (`0\|1`) | live operation |
//! | [`AudioMixerOperation::SetGroupParent`] | `audio_group_parent` | `group`, `parent` (`-` for none) | project edit |
//! | [`AudioMixerOperation::SetSoloMode`] | `audio_solo_mode` | `mode` (`sip\|pfl\|afl`) | project edit |
//! | [`AudioMixerOperation::SetMonitorDevice`] | `audio_monitor_device` | `device` (empty for none) | project edit |
//! | [`AudioMixerOperation::SetMonitorLevel`] | `audio_monitor_level` | `gain_millidb`, `muted` | live operation |
//!
//! Since 2.75 every input strip and the Master bus carry a native DSP chain
//! (AU-005, [`fm_types::strip_dsp::StripDsp`]): polarity, high-pass, five EQ bands,
//! gate, expander, compressor, and limiter, each switchable, plus a chain bypass.
//!
//! | [`AudioMixerOperation::SetStripDsp`] | `audio_strip_dsp` | `target` (`master` or an input), `chain` (the 138-character chain code), optional `gesture` + `continues` | project edit |
//!
//! A knob drag sends its whole chain every few frames under one `gesture`
//! token (as a grade drag does, 2.56): the server folds the drag into one
//! undo step. A snapshot carries the Master chain in `audio_master_dsp` and
//! announces `audio_strip_dsp_states`: that many
//! [`AudioStripDspStateMessage`] records (one per input whose chain is not
//! the default, ascending) follow the snapshot's other follow-on records.
//! Gain reduction reaches clients in `audio_meters` `dynamics`, and
//! BS.1770 loudness and true peak in `audio_meters` `loudness`
//! ([`AudioLoudnessMeter`]).
//!
//! Since 2.92 a loudness entry also carries the EBU Tech 3342 loudness
//! range, and an operator restarts one bus's integrated loudness, loudness
//! range, and maximum true peak:
//!
//! | [`AudioMixerOperation::ResetLoudness`] | `audio_loudness_reset` | `target` (`master` or an input) | live operation |
//!
//! The reset changes no show state: it is journaled like every accepted
//! command and realized by the media plane on its frame, so a replay
//! accepts it exactly as the live show did.
//!
//! Every accepted operation, undo and redo included, is announced by
//! `event=audio_mixer_changed` carrying the operation (`operation=<payload>`
//! and the same fields). `payload=fade_to_black_output` with `output` and
//! `affected` (`0|1`) chooses whether Fade to Black fades an output, a
//! project edit announced by `event=fade_to_black_output_changed`.
//!
//! A snapshot carries `audio_groups` (four
//! `<name>~<gain>~<muted>~<soloed>~<parent>` entries, group 1 first, `parent`
//! being `-` or a group number), `audio_solo_mode`, `audio_monitor`
//! (`<device>~<gain>~<muted>`, the device empty for none),
//! `input_audio_labels`
//! (sparse `<input>~<label>` entries,
//! ascending), each strip's group inside `input_audio_strips`, and
//! `fade_to_black_excluded_outputs` (the outputs Fade to Black leaves live,
//! ascending; empty when it fades every output).

use fm_types::audio_bus::{AUX_BUS_COUNT, AuxBusNumber};
use fm_types::strip_dsp::StripDsp;

use crate::{WireInputId, WireOutputId};

/// Most [`AudioStripDspStateMessage`] records one snapshot announces: one per
/// show input.
pub const MAX_AUDIO_STRIP_DSP_STATES: u16 = 256;

/// The chain a DSP edit or gain-reduction meter names (2.75).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AudioDspTarget {
    /// The Master bus (`master`).
    Master,
    /// One input's strip (its decimal id).
    Input(WireInputId),
}

/// Largest gain reduction a dynamics meter reports, in millidecibels.
pub const MAX_DYNAMICS_REDUCTION_MILLIDB: u32 = 200_000;

/// One chain's gain reduction over an `audio_meters` interval (2.75): the
/// deepest gate attenuation, expander attenuation, compressor reduction,
/// and limiter reduction on any channel, in positive millidecibels, each `None` while that stage is
/// off or the chain bypassed. Only chains with a running dynamics stage are
/// reported.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AudioDynamicsMeter {
    pub target: AudioDspTarget,
    pub gate_millidb: Option<u32>,
    pub expander_millidb: Option<u32>,
    pub compressor_millidb: Option<u32>,
    pub limiter_millidb: Option<u32>,
}

/// Lowest and highest loudness or true-peak value `audio_meters` carries,
/// in milli-LUFS or milli-dBTP; a quieter measurement is reported as none.
pub const MIN_LOUDNESS_MILLI: i32 = -100_000;
pub const MAX_LOUDNESS_MILLI: i32 = 30_000;
/// Largest loudness range `audio_meters` carries, in milli-LU: the span of
/// the meter's -70 to +10 LUFS short-term histogram (2.92).
pub const MAX_LOUDNESS_RANGE_MILLILU: i32 = 80_000;

/// One bus's ITU-R BS.1770 / EBU R128 loudness over an `audio_meters`
/// interval (AU-005): momentary (400 ms), short-term (3 s), and gated
/// integrated loudness in milli-LUFS, and the true peak over the interval
/// and since the meter started or was last reset in milli-dBTP, and (since
/// 2.92) the EBU Tech 3342 loudness range since the meter started or was
/// last reset in milli-LU. Each is `None` while its window is not yet
/// full, the bus is silent, or the value is below [`MIN_LOUDNESS_MILLI`].
/// A strip's loudness hears its post-fader contribution to Program;
/// Master's hears Program as it leaves.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AudioLoudnessMeter {
    pub target: AudioDspTarget,
    pub momentary_millilufs: Option<i32>,
    pub short_term_millilufs: Option<i32>,
    pub integrated_millilufs: Option<i32>,
    pub true_peak_millidbtp: Option<i32>,
    pub max_true_peak_millidbtp: Option<i32>,
    /// `0..=`[`MAX_LOUDNESS_RANGE_MILLILU`] (2.92).
    pub loudness_range_millilu: Option<i32>,
}

impl AudioLoudnessMeter {
    /// The five loudness and true-peak values in wire order (the loudness
    /// range follows them on the wire).
    #[must_use]
    pub const fn values(&self) -> [Option<i32>; 5] {
        [
            self.momentary_millilufs,
            self.short_term_millilufs,
            self.integrated_millilufs,
            self.true_peak_millidbtp,
            self.max_true_peak_millidbtp,
        ]
    }
}

/// One input strip's non-default DSP chain at a snapshot's `revision`
/// (record `audio_strip_dsp_state`, 2.75).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioStripDspStateMessage {
    pub revision: u64,
    pub input: WireInputId,
    pub chain: StripDsp,
}

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

/// Longest monitor output device name, in bytes (2.53).
pub const MAX_AUDIO_MONITOR_DEVICE_BYTES: usize = 64;

/// Whether `device` is a usable monitor output device name: 1 to
/// [`MAX_AUDIO_MONITOR_DEVICE_BYTES`] bytes of printable ASCII without
/// spaces, not starting with `-` (2.53). The name is the last argument of
/// the daemon's `FFmpeg` child, which would read a leading `-` as an option.
#[must_use]
pub fn audio_monitor_device_is_valid(device: &str) -> bool {
    !device.is_empty()
        && device.len() <= MAX_AUDIO_MONITOR_DEVICE_BYTES
        && !device.starts_with('-')
        && device.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Where the monitor bus is heard, and how loud (2.53).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AudioMonitorStatus {
    /// The local audio output device, or `None` for meter-only.
    pub device: Option<String>,
    /// The monitor feed level; Program is untouched.
    pub gain_millidb: i32,
    /// Whether the monitor feed is muted.
    pub muted: bool,
}

/// Whether `group` names one of the show's groups.
#[must_use]
pub const fn audio_group_is_valid(group: u8) -> bool {
    group >= 1 && group as usize <= AUDIO_GROUP_COUNT
}

/// One strip group's name, fader, mute, solo, and parent group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioGroupStatus {
    pub name: String,
    pub gain_millidb: i32,
    pub muted: bool,
    /// Whether the group is soloed, soloing every member (2.44).
    pub soloed: bool,
    /// The group (`1..=4`) this group is nested in (2.44).
    pub parent: Option<u8>,
}

impl AudioGroupStatus {
    /// The default group `number`: "Group N" at unity, unmuted, unsoloed,
    /// and top-level.
    #[must_use]
    pub fn default_for(number: u8) -> Self {
        Self {
            name: format!("Group {number}"),
            gain_millidb: 0,
            muted: false,
            soloed: false,
            parent: None,
        }
    }
}

/// One aux bus's name, fader, and mute (2.118).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxBusStatus {
    pub name: String,
    pub gain_millidb: i32,
    pub muted: bool,
}

impl AuxBusStatus {
    /// The default aux bus `bus`: "Bus A" through "Bus G" at unity, unmuted.
    #[must_use]
    pub fn default_for(bus: AuxBusNumber) -> Self {
        Self {
            name: format!("Bus {}", bus.letter()),
            gain_millidb: 0,
            muted: false,
        }
    }
}

/// How a soloed strip is heard (2.44).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AudioSoloMode {
    /// `sip`: solo-in-place gates every other strip out of Program.
    #[default]
    InPlace,
    /// `pfl`: soloed strips are heard pre-fader on the monitor bus.
    Pfl,
    /// `afl`: soloed strips are heard after-fader on the monitor bus.
    Afl,
}

impl AudioSoloMode {
    /// Every mode in display order.
    pub const ALL: [Self; 3] = [Self::InPlace, Self::Pfl, Self::Afl];

    /// The wire token: `sip`, `pfl`, or `afl`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::InPlace => "sip",
            Self::Pfl => "pfl",
            Self::Afl => "afl",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.token() == token)
    }

    /// Whether a solo is heard on the monitor bus instead of gating Program.
    #[must_use]
    pub const fn listens(self) -> bool {
        !matches!(self, Self::InPlace)
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
    /// How a soloed strip is heard (2.44).
    pub solo_mode: AudioSoloMode,
    /// Where the monitor bus is heard, and how loud (2.53).
    pub monitor: AudioMonitorStatus,
    /// The Master bus DSP chain (2.75).
    pub master_dsp: StripDsp,
    /// How many [`AudioStripDspStateMessage`] records follow the snapshot
    /// (2.75); not maintained by [`Self::apply`].
    pub strip_dsp_states: u16,
    /// The aux buses, bus A first (2.118). Each output's bus rides its
    /// [`crate::OutputStatus`].
    pub aux_buses: [AuxBusStatus; AUX_BUS_COUNT],
}

impl Default for AudioMixerStatus {
    fn default() -> Self {
        Self {
            groups: [1, 2, 3, 4].map(AudioGroupStatus::default_for),
            labels: Vec::new(),
            solo_mode: AudioSoloMode::InPlace,
            monitor: AudioMonitorStatus::default(),
            master_dsp: StripDsp::DEFAULT,
            strip_dsp_states: 0,
            aux_buses: AuxBusNumber::ALL.map(AuxBusStatus::default_for),
        }
    }
}

impl AudioMixerStatus {
    /// `group` and then each ancestor, at most [`AUDIO_GROUP_COUNT`] groups.
    pub fn chain(&self, group: Option<u8>) -> impl Iterator<Item = u8> + '_ {
        let mut next = group.filter(|group| audio_group_is_valid(*group));
        (0..AUDIO_GROUP_COUNT).map_while(move |_| {
            let group = next?;
            next = self.groups[usize::from(group) - 1]
                .parent
                .filter(|parent| audio_group_is_valid(*parent));
            Some(group)
        })
    }

    /// Whether nesting `group` inside `parent` keeps the groups cycle-free.
    #[must_use]
    pub fn parent_is_acyclic(&self, group: u8, parent: Option<u8>) -> bool {
        parent.is_none_or(|parent| {
            !self.chain(Some(parent)).any(|ancestor| ancestor == group)
                && self.chain(Some(parent)).count() < AUDIO_GROUP_COUNT
        })
    }

    /// Whether a strip in `group` is soloed through any group on its chain.
    #[must_use]
    pub fn group_chain_soloed(&self, group: Option<u8>) -> bool {
        self.chain(group)
            .any(|group| self.groups[usize::from(group) - 1].soloed)
    }

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
            // Membership lives in the strip records and an output's bus in its
            // output status; a loudness reset changes no status (2.92).
            AudioMixerOperation::SetGroup { .. }
            | AudioMixerOperation::ResetLoudness { .. }
            | AudioMixerOperation::SetOutputBus { .. } => {}
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
            AudioMixerOperation::SetGroupSolo { group, soloed } => {
                if audio_group_is_valid(*group) {
                    self.groups[usize::from(*group) - 1].soloed = *soloed;
                }
            }
            AudioMixerOperation::SetGroupParent { group, parent } => {
                if audio_group_is_valid(*group) {
                    self.groups[usize::from(*group) - 1].parent = *parent;
                }
            }
            AudioMixerOperation::SetSoloMode { mode } => self.solo_mode = *mode,
            AudioMixerOperation::SetMonitorDevice { device } => {
                self.monitor.device.clone_from(device);
            }
            AudioMixerOperation::SetMonitorLevel {
                gain_millidb,
                muted,
            } => {
                self.monitor.gain_millidb = *gain_millidb;
                self.monitor.muted = *muted;
            }
            AudioMixerOperation::SetStripDsp { target, chain, .. } => {
                if *target == AudioDspTarget::Master {
                    self.master_dsp = **chain;
                }
            }
            AudioMixerOperation::RenameAuxBus { bus, name } => {
                self.aux_buses[bus.index()].name.clone_from(name);
            }
            AudioMixerOperation::SetAuxBusLevel {
                bus,
                gain_millidb,
                muted,
            } => {
                let entry = &mut self.aux_buses[bus.index()];
                entry.gain_millidb = *gain_millidb;
                entry.muted = *muted;
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
    /// Solos or unsolos `group` and with it every member.
    SetGroupSolo { group: u8, soloed: bool },
    /// Nests `group` inside `parent` (`1..=4`), or makes it top-level.
    SetGroupParent { group: u8, parent: Option<u8> },
    /// Chooses how a soloed strip is heard.
    SetSoloMode { mode: AudioSoloMode },
    /// Plays the monitor bus on a local audio output device, or on none
    /// (meter-only) (2.53).
    SetMonitorDevice { device: Option<String> },
    /// Moves the monitor feed level or mute; Program is untouched (2.53).
    SetMonitorLevel { gain_millidb: i32, muted: bool },
    /// Replaces a strip's or the Master's DSP chain (2.75); `gesture`
    /// folds a knob drag into one undo step.
    SetStripDsp {
        target: AudioDspTarget,
        /// Boxed: a chain is larger than every other payload.
        chain: Box<StripDsp>,
        gesture: Option<crate::GradeGesture>,
    },
    /// Restarts a bus's integrated loudness, loudness range, and maximum
    /// true peak (2.92).
    ResetLoudness { target: AudioDspTarget },
    /// Renames an aux bus (2.118).
    RenameAuxBus { bus: AuxBusNumber, name: String },
    /// Moves an aux bus fader or mute (2.118).
    SetAuxBusLevel {
        bus: AuxBusNumber,
        gain_millidb: i32,
        muted: bool,
    },
    /// Makes `output` hear aux bus `bus`, or Master for `None` (2.118).
    SetOutputBus {
        output: WireOutputId,
        bus: Option<AuxBusNumber>,
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
            Self::SetGroupSolo { .. } => "audio_group_solo",
            Self::SetGroupParent { .. } => "audio_group_parent",
            Self::SetSoloMode { .. } => "audio_solo_mode",
            Self::SetMonitorDevice { .. } => "audio_monitor_device",
            Self::SetMonitorLevel { .. } => "audio_monitor_level",
            Self::SetStripDsp { .. } => "audio_strip_dsp",
            Self::ResetLoudness { .. } => "audio_loudness_reset",
            Self::RenameAuxBus { .. } => "audio_aux_bus_name",
            Self::SetAuxBusLevel { .. } => "audio_aux_bus_level",
            Self::SetOutputBus { .. } => "audio_output_bus",
        }
    }

    /// Whether this is a live operation rather than a project edit.
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
}

/// The Fade-to-Black output selection: the outputs it leaves live.
#[must_use]
pub fn fade_to_black_affects(excluded: &[WireOutputId], output: WireOutputId) -> bool {
    !excluded.contains(&output)
}
