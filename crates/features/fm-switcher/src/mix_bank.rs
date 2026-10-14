//! A bounded bank of the Main mix plus up to fifteen secondary mixes.
//!
//! Every mix owns its own Program (and, unless it runs in output-only mode,
//! its own Preview), cuts and timed transitions, and per-mix overlay channel
//! state. A mix may use another mix's Program as a source ([`MixSource::Mix`]).
//!
//! # Frame timing contract
//!
//! [`MixBank::advance_frame`] produces bank frame `N` (starting at zero) in
//! three fixed phases:
//!
//! 1. **Boundary:** every effect scheduled with [`MixBank::schedule_effect`]
//!    since the previous call is applied, in scheduling order. Each effect is
//!    re-validated against every target and applied to all targets or to none.
//! 2. **Compose:** every mix's frame `N` is described from the post-boundary
//!    state. Transition mix values are the elapsed counts *before* this frame's
//!    advance, matching [`crate::SwitcherState::program_frame`].
//! 3. **Advance:** every running transition advances by one frame and
//!    completes when its elapsed count reaches its duration.
//!
//! Reading another mix is uniformly delayed by exactly one frame: a layer that
//! names `Mix(m)` in bank frame `N` resolves to [`MixLayer::Mix`] with
//! `frame: N - 1`, the Program that `m` composed in the previous bank frame.
//! On bank frame zero there is no previous frame, so the read resolves to
//! [`MixLayer::Black`]. The rule does not depend on mix numbering or
//! evaluation order, so a chain `A -> B -> C` delivers `A`'s frame `N - 2` to
//! `C` at frame `N`. Routing cycles are nevertheless rejected: a mix may not
//! (transitively) reference itself through Program, Preview, or a running
//! transition endpoint.

use fm_types::InputId;

use crate::{
    MAX_OVERLAY_TRANSITION_DURATION_FRAMES, OVERLAY_CHANNEL_COUNT, OverlayChannelId, StingerSlotId,
    TransitionKind,
};

/// Number of secondary mixes addressable beside the Main mix.
pub const MAX_SECONDARY_MIXES: usize = 15;
/// Total mixes, including Main, addressable by a [`MixBank`].
pub const MIX_BANK_CAPACITY: usize = MAX_SECONDARY_MIXES + 1;
/// Upper bound on inputs a bank may route.
pub const MAX_MIX_BANK_INPUTS: usize = 1_000;
/// Upper bound on a timed mix transition, in frames.
pub const MAX_MIX_TRANSITION_DURATION_FRAMES: u32 = MAX_OVERLAY_TRANSITION_DURATION_FRAMES;
/// Upper bound on effects waiting for the next frame boundary.
pub const MAX_PENDING_MIX_EFFECTS: usize = 32;

/// A one-based mix number: `1` is Main, `2..=16` are the secondary mixes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MixId(u8);

impl MixId {
    pub const MAIN: Self = Self(1);

    /// Creates a mix id from its one-based operator-facing number.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= MIX_BANK_CAPACITY {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_main(self) -> bool {
        self.0 == 1
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }

    const fn bit(self) -> u16 {
        1 << (self.0 - 1)
    }

    fn all() -> impl Iterator<Item = Self> {
        (1..=MIX_BANK_CAPACITY)
            .map(|number| Self(u8::try_from(number).expect("mix bank capacity fits in u8")))
    }
}

impl core::fmt::Display for MixId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "Mix {}", self.0)
    }
}

/// A bounded set of mixes used for Overlay/Stinger targeting.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MixSet(u16);

impl MixSet {
    pub const EMPTY: Self = Self(0);

    #[must_use]
    pub const fn single(mix: MixId) -> Self {
        Self(mix.bit())
    }

    /// Collects the given mixes; repeats collapse into one member.
    #[must_use]
    pub fn from_mixes(mixes: impl IntoIterator<Item = MixId>) -> Self {
        let mut set = Self::EMPTY;
        for mix in mixes {
            set.insert(mix);
        }
        set
    }

    pub const fn insert(&mut self, mix: MixId) {
        self.0 |= mix.bit();
    }

    #[must_use]
    pub const fn contains(self, mix: MixId) -> bool {
        self.0 & mix.bit() != 0
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Members in ascending mix-number order.
    pub fn iter(self) -> impl Iterator<Item = MixId> {
        MixId::all().filter(move |mix| self.contains(*mix))
    }
}

/// What a mix layer shows: a bank input or another mix's Program.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MixSource {
    Input(InputId),
    Mix(MixId),
}

/// Whether a mix keeps a Preview bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixMode {
    PreviewProgram,
    /// Program only; takes name their destination source directly.
    OutputOnly,
}

/// Initial configuration for a mix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixConfig {
    PreviewProgram {
        program: MixSource,
        preview: MixSource,
    },
    OutputOnly {
        program: MixSource,
    },
}

/// A timed transition running inside one mix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixTransition {
    kind: TransitionKind,
    from: MixSource,
    to: MixSource,
    duration_frames: u32,
    elapsed_frames: u32,
}

impl MixTransition {
    #[must_use]
    pub const fn kind(self) -> TransitionKind {
        self.kind
    }

    #[must_use]
    pub const fn from(self) -> MixSource {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> MixSource {
        self.to
    }

    #[must_use]
    pub const fn duration_frames(self) -> u32 {
        self.duration_frames
    }

    #[must_use]
    pub const fn elapsed_frames(self) -> u32 {
        self.elapsed_frames
    }
}

/// Read-only state of one configured mix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixState {
    program: MixSource,
    preview: Option<MixSource>,
    transition: Option<MixTransition>,
    overlays: [Option<InputId>; OVERLAY_CHANNEL_COUNT],
}

impl MixState {
    #[must_use]
    pub const fn mode(&self) -> MixMode {
        if self.preview.is_some() {
            MixMode::PreviewProgram
        } else {
            MixMode::OutputOnly
        }
    }

    #[must_use]
    pub const fn program(&self) -> MixSource {
        self.program
    }

    #[must_use]
    pub const fn preview(&self) -> Option<MixSource> {
        self.preview
    }

    #[must_use]
    pub const fn transition(&self) -> Option<MixTransition> {
        self.transition
    }

    #[must_use]
    pub fn overlay(&self, channel: OverlayChannelId) -> Option<InputId> {
        self.overlays[channel.index()]
    }

    fn references(&self) -> impl Iterator<Item = MixSource> + '_ {
        [Some(self.program), self.preview]
            .into_iter()
            .chain(
                self.transition
                    .into_iter()
                    .flat_map(|transition| [Some(transition.from), Some(transition.to)]),
            )
            .flatten()
    }
}

/// Overlay or Stinger action applied to a set of mixes at a frame boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixEffect {
    OverlayOn {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayOff {
        channel: OverlayChannelId,
    },
    /// Starts a stinger-labelled Preview-to-Program transition in each target.
    Stinger {
        slot: StingerSlotId,
        duration_frames: u32,
    },
}

/// Operator commands that mutate the bank immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixBankCommand {
    AddMix {
        mix: MixId,
        config: MixConfig,
    },
    RemoveMix(MixId),
    SelectPreview {
        mix: MixId,
        source: MixSource,
    },
    /// Swaps Preview and Program of a Preview/Program mix.
    Cut(MixId),
    /// Starts a timed Program-to-Preview transition of a Preview/Program mix.
    Transition {
        mix: MixId,
        kind: TransitionKind,
        duration_frames: u32,
    },
    /// Cuts an output-only mix directly to `source`.
    CutTo {
        mix: MixId,
        source: MixSource,
    },
    /// Starts a timed transition of an output-only mix toward `source`.
    TransitionTo {
        mix: MixId,
        source: MixSource,
        kind: TransitionKind,
        duration_frames: u32,
    },
    /// Drops the Preview bus of a secondary mix.
    SetOutputOnly(MixId),
    /// Restores a Preview bus with the given source.
    EnablePreview {
        mix: MixId,
        preview: MixSource,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixBankEvent {
    MixAdded {
        mix: MixId,
    },
    MixRemoved {
        mix: MixId,
    },
    PreviewSelected {
        mix: MixId,
        source: MixSource,
    },
    ProgramChanged {
        mix: MixId,
        previous: MixSource,
        program: MixSource,
    },
    TransitionStarted {
        mix: MixId,
        kind: TransitionKind,
        from: MixSource,
        to: MixSource,
        duration_frames: u32,
    },
    TransitionCompleted {
        mix: MixId,
        kind: TransitionKind,
        program: MixSource,
    },
    ModeChanged {
        mix: MixId,
        mode: MixMode,
    },
    OverlayChanged {
        mix: MixId,
        channel: OverlayChannelId,
        source: Option<InputId>,
    },
    EffectScheduled {
        effect: MixEffect,
        targets: MixSet,
        apply_frame: u64,
    },
    EffectApplied {
        effect: MixEffect,
        targets: MixSet,
        frame: u64,
    },
    /// A scheduled effect failed boundary re-validation and touched no mix.
    EffectRejected {
        effect: MixEffect,
        targets: MixSet,
        frame: u64,
        error: MixBankError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixBankError {
    TooManyInputs {
        requested: usize,
        maximum: usize,
    },
    DuplicateInput(InputId),
    UnknownInput(InputId),
    UnknownMix(MixId),
    MixAlreadyExists(MixId),
    MainMixIsPermanent,
    MainMixRequiresPreview,
    /// The mix is output-only and has no Preview bus.
    PreviewUnavailable(MixId),
    PreviewAlreadyEnabled(MixId),
    /// Direct takes are only defined for output-only mixes.
    DirectTakeRequiresOutputOnly(MixId),
    PreviewEqualsProgram {
        mix: MixId,
        source: MixSource,
    },
    SourceAlreadyProgram {
        mix: MixId,
        source: MixSource,
    },
    SelfRoute(MixId),
    RoutingCycle {
        mix: MixId,
        source: MixId,
    },
    MixReferenced {
        mix: MixId,
        by: MixId,
    },
    TransitionInProgress(MixId),
    InvalidTransitionDuration {
        duration_frames: u32,
        maximum: u32,
    },
    EmptyTargetSet,
    PendingEffectsFull {
        maximum: usize,
    },
}

impl core::fmt::Display for MixBankError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyInputs { requested, maximum } => write!(
                formatter,
                "mix bank requests {requested} inputs; maximum is {maximum}"
            ),
            Self::DuplicateInput(input) => write!(formatter, "input {input} occurs more than once"),
            Self::UnknownInput(input) => write!(formatter, "input {input} is not part of the bank"),
            Self::UnknownMix(mix) => write!(formatter, "{mix} is not configured"),
            Self::MixAlreadyExists(mix) => write!(formatter, "{mix} is already configured"),
            Self::MainMixIsPermanent => formatter.write_str("the Main mix cannot be removed"),
            Self::MainMixRequiresPreview => {
                formatter.write_str("the Main mix always keeps a Preview bus")
            }
            Self::PreviewUnavailable(mix) => write!(formatter, "{mix} is output-only"),
            Self::PreviewAlreadyEnabled(mix) => {
                write!(formatter, "{mix} already has a Preview bus")
            }
            Self::DirectTakeRequiresOutputOnly(mix) => {
                write!(
                    formatter,
                    "{mix} takes from Preview; direct takes need output-only"
                )
            }
            Self::PreviewEqualsProgram { mix, .. } => {
                write!(formatter, "{mix} Preview and Program must differ")
            }
            Self::SourceAlreadyProgram { mix, .. } => {
                write!(formatter, "the requested source is already {mix} Program")
            }
            Self::SelfRoute(mix) => write!(formatter, "{mix} cannot use itself as a source"),
            Self::RoutingCycle { mix, source } => {
                write!(
                    formatter,
                    "routing {source} into {mix} would create a cycle"
                )
            }
            Self::MixReferenced { mix, by } => write!(formatter, "{mix} is a source of {by}"),
            Self::TransitionInProgress(mix) => {
                write!(formatter, "{mix} already has a transition in progress")
            }
            Self::InvalidTransitionDuration {
                duration_frames,
                maximum,
            } => write!(
                formatter,
                "transition duration {duration_frames} is outside 1..={maximum}"
            ),
            Self::EmptyTargetSet => formatter.write_str("an effect needs at least one target mix"),
            Self::PendingEffectsFull { maximum } => {
                write!(
                    formatter,
                    "at most {maximum} effects may wait for a frame boundary"
                )
            }
        }
    }
}

impl std::error::Error for MixBankError {}

/// A composed layer reference for one bank frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixLayer {
    Input(InputId),
    /// The Program that `mix` composed in bank frame `frame` (always `N - 1`).
    Mix {
        mix: MixId,
        frame: u64,
    },
    /// A mix read on bank frame zero, where no previous frame exists.
    Black,
}

/// What one mix shows in one bank frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixFrame {
    pub mix: MixId,
    pub primary: MixLayer,
    pub secondary: Option<MixLayer>,
    pub transition_kind: Option<TransitionKind>,
    pub mix_numerator: u32,
    pub mix_denominator: u32,
    /// Preview monitor content; `None` for output-only mixes.
    pub preview: Option<MixLayer>,
    pub overlays: [Option<InputId>; OVERLAY_CHANNEL_COUNT],
}

/// All configured mixes for one bank frame, in ascending mix order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MixBankFrame {
    pub frame: u64,
    pub mixes: Vec<MixFrame>,
}

impl MixBankFrame {
    #[must_use]
    pub fn mix(&self, mix: MixId) -> Option<&MixFrame> {
        self.mixes.iter().find(|frame| frame.mix == mix)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingEffect {
    effect: MixEffect,
    targets: MixSet,
}

/// Main plus up to fifteen secondary mixes with deterministic frame timing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MixBank {
    inputs: Vec<InputId>,
    mixes: [Option<MixState>; MIX_BANK_CAPACITY],
    pending: Vec<PendingEffect>,
    next_frame: u64,
}

impl MixBank {
    /// Creates a bank whose Main mix uses the given inputs.
    ///
    /// # Errors
    ///
    /// Returns [`MixBankError`] when the input list is too long or repeats an
    /// input, a Main source is unknown or names a mix, or Preview equals
    /// Program.
    pub fn new(
        inputs: Vec<InputId>,
        program: InputId,
        preview: InputId,
    ) -> Result<Self, MixBankError> {
        if inputs.len() > MAX_MIX_BANK_INPUTS {
            return Err(MixBankError::TooManyInputs {
                requested: inputs.len(),
                maximum: MAX_MIX_BANK_INPUTS,
            });
        }
        for (index, input) in inputs.iter().enumerate() {
            if inputs[..index].contains(input) {
                return Err(MixBankError::DuplicateInput(*input));
            }
        }
        let mut bank = Self {
            inputs,
            mixes: [None; MIX_BANK_CAPACITY],
            pending: Vec::new(),
            next_frame: 0,
        };
        let config = MixConfig::PreviewProgram {
            program: MixSource::Input(program),
            preview: MixSource::Input(preview),
        };
        bank.validate_config(MixId::MAIN, config)?;
        bank.mixes[MixId::MAIN.index()] = Some(new_mix_state(config));
        Ok(bank)
    }

    #[must_use]
    pub fn inputs(&self) -> &[InputId] {
        &self.inputs
    }

    #[must_use]
    pub fn mix(&self, mix: MixId) -> Option<&MixState> {
        self.mixes[mix.index()].as_ref()
    }

    /// Configured mixes in ascending order.
    #[must_use]
    pub fn configured_mixes(&self) -> MixSet {
        MixSet::from_mixes(MixId::all().filter(|mix| self.mixes[mix.index()].is_some()))
    }

    /// The bank frame number that the next [`Self::advance_frame`] produces.
    #[must_use]
    pub const fn next_frame(&self) -> u64 {
        self.next_frame
    }

    #[must_use]
    pub fn pending_effects(&self) -> usize {
        self.pending.len()
    }

    /// Applies one command after validating it completely.
    ///
    /// # Errors
    ///
    /// Returns [`MixBankError`] and leaves the bank unchanged when the
    /// command is invalid for the current state.
    pub fn apply(&mut self, command: MixBankCommand) -> Result<Vec<MixBankEvent>, MixBankError> {
        match command {
            MixBankCommand::AddMix { mix, config } => self.add_mix(mix, config),
            MixBankCommand::RemoveMix(mix) => self.remove_mix(mix),
            MixBankCommand::SelectPreview { mix, source } => {
                let state = self.require_idle(mix)?;
                if state.preview.is_none() {
                    return Err(MixBankError::PreviewUnavailable(mix));
                }
                if source == state.program {
                    return Err(MixBankError::PreviewEqualsProgram { mix, source });
                }
                self.validate_route(mix, source)?;
                self.state_mut(mix).preview = Some(source);
                Ok(vec![MixBankEvent::PreviewSelected { mix, source }])
            }
            MixBankCommand::Cut(mix) => {
                let state = self.require_idle(mix)?;
                let preview = state.preview.ok_or(MixBankError::PreviewUnavailable(mix))?;
                let previous = state.program;
                let state = self.state_mut(mix);
                state.program = preview;
                state.preview = Some(previous);
                Ok(vec![MixBankEvent::ProgramChanged {
                    mix,
                    previous,
                    program: preview,
                }])
            }
            MixBankCommand::Transition {
                mix,
                kind,
                duration_frames,
            } => {
                self.validate_preview_transition(mix, duration_frames)?;
                Ok(vec![self.start_preview_transition(
                    mix,
                    kind,
                    duration_frames,
                )])
            }
            MixBankCommand::CutTo { mix, source } => {
                let previous = self.validate_direct_take(mix, source)?;
                self.state_mut(mix).program = source;
                Ok(vec![MixBankEvent::ProgramChanged {
                    mix,
                    previous,
                    program: source,
                }])
            }
            MixBankCommand::TransitionTo {
                mix,
                source,
                kind,
                duration_frames,
            } => {
                validate_duration(duration_frames)?;
                let from = self.validate_direct_take(mix, source)?;
                self.state_mut(mix).transition = Some(MixTransition {
                    kind,
                    from,
                    to: source,
                    duration_frames,
                    elapsed_frames: 0,
                });
                Ok(vec![MixBankEvent::TransitionStarted {
                    mix,
                    kind,
                    from,
                    to: source,
                    duration_frames,
                }])
            }
            MixBankCommand::SetOutputOnly(mix) => self.set_output_only(mix),
            MixBankCommand::EnablePreview { mix, preview } => self.enable_preview(mix, preview),
        }
    }

    /// Queues an Overlay/Stinger effect for the next frame boundary.
    ///
    /// The effect is validated now against the current state and validated
    /// again at the boundary; it is applied to every target or to none.
    ///
    /// # Errors
    ///
    /// Returns [`MixBankError`] when the target set is empty, the queue is
    /// full, or the effect is invalid for any target right now.
    pub fn schedule_effect(
        &mut self,
        effect: MixEffect,
        targets: MixSet,
    ) -> Result<MixBankEvent, MixBankError> {
        if self.pending.len() >= MAX_PENDING_MIX_EFFECTS {
            return Err(MixBankError::PendingEffectsFull {
                maximum: MAX_PENDING_MIX_EFFECTS,
            });
        }
        self.validate_effect(effect, targets)?;
        self.pending.push(PendingEffect { effect, targets });
        Ok(MixBankEvent::EffectScheduled {
            effect,
            targets,
            apply_frame: self.next_frame,
        })
    }

    /// Produces the next bank frame following the module timing contract.
    pub fn advance_frame(&mut self) -> (MixBankFrame, Vec<MixBankEvent>) {
        let frame = self.next_frame;
        let mut events = Vec::new();
        for PendingEffect { effect, targets } in std::mem::take(&mut self.pending) {
            match self.validate_effect(effect, targets) {
                Ok(()) => {
                    for mix in targets.iter() {
                        self.apply_effect(mix, effect, &mut events);
                    }
                    events.push(MixBankEvent::EffectApplied {
                        effect,
                        targets,
                        frame,
                    });
                }
                Err(error) => events.push(MixBankEvent::EffectRejected {
                    effect,
                    targets,
                    frame,
                    error,
                }),
            }
        }

        let mixes = MixId::all()
            .filter_map(|mix| {
                self.mixes[mix.index()]
                    .as_ref()
                    .map(|state| compose(mix, state, frame))
            })
            .collect();

        for mix in MixId::all() {
            let Some(state) = self.mixes[mix.index()].as_mut() else {
                continue;
            };
            let Some(mut transition) = state.transition else {
                continue;
            };
            transition.elapsed_frames += 1;
            if transition.elapsed_frames >= transition.duration_frames {
                state.transition = None;
                let previous = state.program;
                state.program = transition.to;
                if state.preview.is_some() {
                    state.preview = Some(previous);
                }
                events.push(MixBankEvent::ProgramChanged {
                    mix,
                    previous,
                    program: transition.to,
                });
                events.push(MixBankEvent::TransitionCompleted {
                    mix,
                    kind: transition.kind,
                    program: transition.to,
                });
            } else {
                state.transition = Some(transition);
            }
        }

        self.next_frame += 1;
        (MixBankFrame { frame, mixes }, events)
    }

    fn set_output_only(&mut self, mix: MixId) -> Result<Vec<MixBankEvent>, MixBankError> {
        if mix.is_main() {
            return Err(MixBankError::MainMixRequiresPreview);
        }
        let state = self.require_idle(mix)?;
        if state.preview.is_none() {
            return Ok(Vec::new());
        }
        self.state_mut(mix).preview = None;
        Ok(vec![MixBankEvent::ModeChanged {
            mix,
            mode: MixMode::OutputOnly,
        }])
    }

    fn enable_preview(
        &mut self,
        mix: MixId,
        preview: MixSource,
    ) -> Result<Vec<MixBankEvent>, MixBankError> {
        let state = self.require_idle(mix)?;
        if state.preview.is_some() {
            return Err(MixBankError::PreviewAlreadyEnabled(mix));
        }
        if preview == state.program {
            return Err(MixBankError::PreviewEqualsProgram {
                mix,
                source: preview,
            });
        }
        self.validate_route(mix, preview)?;
        self.state_mut(mix).preview = Some(preview);
        Ok(vec![
            MixBankEvent::ModeChanged {
                mix,
                mode: MixMode::PreviewProgram,
            },
            MixBankEvent::PreviewSelected {
                mix,
                source: preview,
            },
        ])
    }

    fn add_mix(
        &mut self,
        mix: MixId,
        config: MixConfig,
    ) -> Result<Vec<MixBankEvent>, MixBankError> {
        if self.mixes[mix.index()].is_some() {
            return Err(MixBankError::MixAlreadyExists(mix));
        }
        self.validate_config(mix, config)?;
        self.mixes[mix.index()] = Some(new_mix_state(config));
        Ok(vec![MixBankEvent::MixAdded { mix }])
    }

    fn remove_mix(&mut self, mix: MixId) -> Result<Vec<MixBankEvent>, MixBankError> {
        if mix.is_main() {
            return Err(MixBankError::MainMixIsPermanent);
        }
        self.require_mix(mix)?;
        for other in MixId::all() {
            if let Some(state) = &self.mixes[other.index()]
                && state
                    .references()
                    .any(|source| source == MixSource::Mix(mix))
            {
                return Err(MixBankError::MixReferenced { mix, by: other });
            }
        }
        self.mixes[mix.index()] = None;
        Ok(vec![MixBankEvent::MixRemoved { mix }])
    }

    fn validate_config(&self, mix: MixId, config: MixConfig) -> Result<(), MixBankError> {
        match config {
            MixConfig::PreviewProgram { program, preview } => {
                if program == preview {
                    return Err(MixBankError::PreviewEqualsProgram {
                        mix,
                        source: preview,
                    });
                }
                self.validate_route(mix, program)?;
                self.validate_route(mix, preview)
            }
            MixConfig::OutputOnly { program } => {
                if mix.is_main() {
                    return Err(MixBankError::MainMixRequiresPreview);
                }
                self.validate_route(mix, program)
            }
        }
    }

    /// Checks that `mix` may reference `source` without a self-route or cycle.
    fn validate_route(&self, mix: MixId, source: MixSource) -> Result<(), MixBankError> {
        match source {
            MixSource::Input(input) => {
                if self.inputs.contains(&input) {
                    Ok(())
                } else {
                    Err(MixBankError::UnknownInput(input))
                }
            }
            MixSource::Mix(upstream) => {
                if upstream == mix {
                    return Err(MixBankError::SelfRoute(mix));
                }
                self.require_mix(upstream)?;
                if self.reaches(upstream, mix) {
                    return Err(MixBankError::RoutingCycle {
                        mix,
                        source: upstream,
                    });
                }
                Ok(())
            }
        }
    }

    /// Whether `from` references `target` directly or transitively.
    fn reaches(&self, from: MixId, target: MixId) -> bool {
        let mut visited = MixSet::EMPTY;
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            if current == target {
                return true;
            }
            if visited.contains(current) {
                continue;
            }
            visited.insert(current);
            if let Some(state) = &self.mixes[current.index()] {
                stack.extend(state.references().filter_map(|source| match source {
                    MixSource::Mix(next) => Some(next),
                    MixSource::Input(_) => None,
                }));
            }
        }
        false
    }

    fn validate_preview_transition(
        &self,
        mix: MixId,
        duration_frames: u32,
    ) -> Result<(), MixBankError> {
        let state = self.require_idle(mix)?;
        if state.preview.is_none() {
            return Err(MixBankError::PreviewUnavailable(mix));
        }
        validate_duration(duration_frames)
    }

    fn start_preview_transition(
        &mut self,
        mix: MixId,
        kind: TransitionKind,
        duration_frames: u32,
    ) -> MixBankEvent {
        let state = self.state_mut(mix);
        let from = state.program;
        let to = state.preview.expect("validated Preview/Program mix");
        state.transition = Some(MixTransition {
            kind,
            from,
            to,
            duration_frames,
            elapsed_frames: 0,
        });
        MixBankEvent::TransitionStarted {
            mix,
            kind,
            from,
            to,
            duration_frames,
        }
    }

    /// Validates a direct take and returns the current Program.
    fn validate_direct_take(
        &self,
        mix: MixId,
        source: MixSource,
    ) -> Result<MixSource, MixBankError> {
        let state = self.require_idle(mix)?;
        if state.preview.is_some() {
            return Err(MixBankError::DirectTakeRequiresOutputOnly(mix));
        }
        if source == state.program {
            return Err(MixBankError::SourceAlreadyProgram { mix, source });
        }
        self.validate_route(mix, source)?;
        Ok(state.program)
    }

    fn validate_effect(&self, effect: MixEffect, targets: MixSet) -> Result<(), MixBankError> {
        if targets.is_empty() {
            return Err(MixBankError::EmptyTargetSet);
        }
        for mix in targets.iter() {
            match effect {
                MixEffect::OverlayOn { source, .. } => {
                    self.require_mix(mix)?;
                    if !self.inputs.contains(&source) {
                        return Err(MixBankError::UnknownInput(source));
                    }
                }
                MixEffect::OverlayOff { .. } => {
                    self.require_mix(mix)?;
                }
                MixEffect::Stinger {
                    duration_frames, ..
                } => self.validate_preview_transition(mix, duration_frames)?,
            }
        }
        Ok(())
    }

    fn apply_effect(&mut self, mix: MixId, effect: MixEffect, events: &mut Vec<MixBankEvent>) {
        match effect {
            MixEffect::OverlayOn { channel, source } => {
                self.state_mut(mix).overlays[channel.index()] = Some(source);
                events.push(MixBankEvent::OverlayChanged {
                    mix,
                    channel,
                    source: Some(source),
                });
            }
            MixEffect::OverlayOff { channel } => {
                self.state_mut(mix).overlays[channel.index()] = None;
                events.push(MixBankEvent::OverlayChanged {
                    mix,
                    channel,
                    source: None,
                });
            }
            MixEffect::Stinger {
                slot,
                duration_frames,
            } => events.push(self.start_preview_transition(
                mix,
                TransitionKind::Stinger(slot),
                duration_frames,
            )),
        }
    }

    fn require_mix(&self, mix: MixId) -> Result<&MixState, MixBankError> {
        self.mixes[mix.index()]
            .as_ref()
            .ok_or(MixBankError::UnknownMix(mix))
    }

    fn require_idle(&self, mix: MixId) -> Result<&MixState, MixBankError> {
        let state = self.require_mix(mix)?;
        if state.transition.is_some() {
            return Err(MixBankError::TransitionInProgress(mix));
        }
        Ok(state)
    }

    fn state_mut(&mut self, mix: MixId) -> &mut MixState {
        self.mixes[mix.index()]
            .as_mut()
            .expect("mutations follow successful validation")
    }
}

const fn new_mix_state(config: MixConfig) -> MixState {
    let (program, preview) = match config {
        MixConfig::PreviewProgram { program, preview } => (program, Some(preview)),
        MixConfig::OutputOnly { program } => (program, None),
    };
    MixState {
        program,
        preview,
        transition: None,
        overlays: [None; OVERLAY_CHANNEL_COUNT],
    }
}

fn validate_duration(duration_frames: u32) -> Result<(), MixBankError> {
    if duration_frames == 0 || duration_frames > MAX_MIX_TRANSITION_DURATION_FRAMES {
        Err(MixBankError::InvalidTransitionDuration {
            duration_frames,
            maximum: MAX_MIX_TRANSITION_DURATION_FRAMES,
        })
    } else {
        Ok(())
    }
}

/// Resolves a source for bank frame `frame` under the one-frame-delay rule.
const fn resolve(source: MixSource, frame: u64) -> MixLayer {
    match source {
        MixSource::Input(input) => MixLayer::Input(input),
        MixSource::Mix(mix) => match frame.checked_sub(1) {
            Some(previous) => MixLayer::Mix {
                mix,
                frame: previous,
            },
            None => MixLayer::Black,
        },
    }
}

fn compose(mix: MixId, state: &MixState, frame: u64) -> MixFrame {
    let preview = state.preview.map(|source| resolve(source, frame));
    match state.transition {
        Some(transition) => MixFrame {
            mix,
            primary: resolve(transition.from, frame),
            secondary: Some(resolve(transition.to, frame)),
            transition_kind: Some(transition.kind),
            mix_numerator: transition.elapsed_frames,
            mix_denominator: transition.duration_frames,
            preview,
            overlays: state.overlays,
        },
        None => MixFrame {
            mix,
            primary: resolve(state.program, frame),
            secondary: None,
            transition_kind: None,
            mix_numerator: 0,
            mix_denominator: 1,
            preview,
            overlays: state.overlays,
        },
    }
}
