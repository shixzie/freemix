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
    FadeToBlackController, FadeToBlackCurve, FadeToBlackError, FadeToBlackFrame,
    FadeToBlackPosition, FadeToBlackRequest, FadeToBlackTarget,
    MAX_OVERLAY_TRANSITION_DURATION_FRAMES, MissingMediaFallback, OVERLAY_CHANNEL_COUNT,
    OverlayChannelId, StingerSlotId, TBarPosition, TransitionKind,
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
    t_bar: Option<MixTBarState>,
    overlays: [Option<MixOverlayState>; OVERLAY_CHANNEL_COUNT],
    fade_to_black: FadeToBlackController,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MixTBarState {
    kind: TransitionKind,
    from: MixSource,
    to: MixSource,
    position: TBarPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OverlayFade {
    from: u8,
    to: u8,
    duration_frames: u32,
    elapsed_frames: u32,
}

impl OverlayFade {
    fn opacity(self, elapsed_frames: u32) -> u8 {
        let elapsed = elapsed_frames.min(self.duration_frames);
        let from = i32::from(self.from);
        let delta = i32::from(self.to) - from;
        let value = from
            + delta * i32::try_from(elapsed).unwrap_or(i32::MAX)
                / i32::try_from(self.duration_frames).unwrap_or(1);
        u8::try_from(value.clamp(0, i32::from(u8::MAX))).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MixOverlayState {
    source: MixSource,
    opacity: u8,
    fade: Option<OverlayFade>,
}

impl MixOverlayState {
    fn frame_opacity(self) -> u8 {
        self.fade.map_or(self.opacity, |fade| {
            fade.opacity(fade.elapsed_frames.saturating_add(1))
        })
    }

    fn advance(&mut self) -> Option<u8> {
        let mut fade = self.fade?;
        fade.elapsed_frames += 1;
        let finished = fade.elapsed_frames >= fade.duration_frames;
        let opacity = if finished {
            fade.to
        } else {
            fade.opacity(fade.elapsed_frames)
        };
        self.opacity = opacity;
        if finished {
            self.fade = None;
        } else {
            self.fade = Some(fade);
        }
        Some(opacity)
    }
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
    pub fn overlay(&self, channel: OverlayChannelId) -> Option<MixSource> {
        self.overlays[channel.index()].map(|overlay| overlay.source)
    }

    #[must_use]
    pub const fn fade_to_black(&self) -> FadeToBlackController {
        self.fade_to_black
    }

    #[must_use]
    pub const fn t_bar_position(&self) -> Option<TBarPosition> {
        match self.t_bar {
            Some(t_bar) => Some(t_bar.position),
            None => None,
        }
    }

    fn references(&self) -> impl Iterator<Item = MixSource> + '_ {
        [Some(self.program), self.preview]
            .into_iter()
            .chain(
                self.transition
                    .into_iter()
                    .flat_map(|transition| [Some(transition.from), Some(transition.to)]),
            )
            .chain(
                self.overlays
                    .iter()
                    .flatten()
                    .map(|overlay| Some(overlay.source)),
            )
            .flatten()
    }
}

/// What a Stinger effect does when its slot's media is not ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixStingerAction {
    /// The clip is ready: start a Stinger transition. The cut point must fit
    /// inside the requested duration.
    Play { cut_point_frames: u32 },
    /// The clip is missing: apply the slot's configured fallback to every
    /// target instead of a Stinger transition.
    Fallback(MissingMediaFallback),
    /// No slot is configured: refuse the whole effect.
    Unconfigured,
}

/// Overlay or Stinger action applied to a set of mixes at a frame boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixEffect {
    OverlayOn {
        channel: OverlayChannelId,
        source: MixSource,
        duration_frames: u32,
    },
    OverlayOff {
        channel: OverlayChannelId,
        duration_frames: u32,
    },
    /// Starts a stinger-labelled Preview-to-Program transition in each target,
    /// or applies the slot's missing-media fallback.
    Stinger {
        slot: StingerSlotId,
        duration_frames: u32,
        action: MixStingerAction,
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
    /// Starts a manual T-bar transition of one Preview/Program mix.
    StartTBar {
        mix: MixId,
        kind: TransitionKind,
    },
    /// Moves a mix's active T-bar to an absolute position.
    SetTBarPosition {
        mix: MixId,
        position: TBarPosition,
    },
    /// Completes a mix's T-bar and swaps Program with Preview.
    CommitTBar(MixId),
    /// Cancels a mix's T-bar without changing Program or Preview.
    CancelTBar(MixId),
    /// Fades one mix independently to Black or back to Live.
    FadeToBlack {
        mix: MixId,
        target: FadeToBlackTarget,
        duration_frames: u32,
        curve: FadeToBlackCurve,
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
        source: Option<MixSource>,
    },
    OverlayFadeChanged {
        mix: MixId,
        channel: OverlayChannelId,
        opacity: u8,
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
    /// A Stinger effect target lacked ready media and applied its fallback.
    StingerFallbackApplied {
        mix: MixId,
        slot: StingerSlotId,
        fallback: MissingMediaFallback,
    },
    TBarStarted {
        mix: MixId,
        kind: TransitionKind,
        from: MixSource,
        to: MixSource,
    },
    TBarPositionChanged {
        mix: MixId,
        position: TBarPosition,
    },
    TBarCancelled {
        mix: MixId,
    },
    FadeToBlackStarted {
        mix: MixId,
        from: FadeToBlackPosition,
        target: FadeToBlackTarget,
        duration_frames: u32,
    },
    FadeToBlackPositionChanged {
        mix: MixId,
        position: FadeToBlackPosition,
    },
    FadeToBlackCompleted {
        mix: MixId,
        target: FadeToBlackTarget,
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
    InvalidOverlayDuration {
        duration_frames: u32,
        maximum: u32,
    },
    EmptyTargetSet,
    PendingEffectsFull {
        maximum: usize,
    },
    /// A Stinger effect names a slot with no media configured.
    UnconfiguredStinger(StingerSlotId),
    /// A mix has no manual T-bar in flight.
    NoTBar(MixId),
    /// A Stinger is taken with its own command, not a mix T-bar.
    UnsupportedManualTransitionKind(MixId),
    /// A mix's Fade-to-Black request was invalid.
    FadeToBlack {
        mix: MixId,
        error: FadeToBlackError,
    },
    /// A ready clip's cut point does not fit inside the effect duration.
    StingerCutPointOutOfRange {
        slot: StingerSlotId,
        cut_point_frames: u32,
        duration_frames: u32,
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
            Self::InvalidOverlayDuration {
                duration_frames,
                maximum,
            } => write!(
                formatter,
                "overlay fade duration {duration_frames} exceeds {maximum} frames"
            ),
            Self::EmptyTargetSet => formatter.write_str("an effect needs at least one target mix"),
            Self::PendingEffectsFull { maximum } => {
                write!(
                    formatter,
                    "at most {maximum} effects may wait for a frame boundary"
                )
            }
            Self::UnconfiguredStinger(slot) => write!(
                formatter,
                "the media of Stinger {} is not configured",
                slot.number()
            ),
            Self::NoTBar(mix) => write!(formatter, "{mix} has no T-bar in progress"),
            Self::UnsupportedManualTransitionKind(mix) => {
                write!(formatter, "{mix} T-bar cannot scrub a Stinger")
            }
            Self::FadeToBlack { mix, error } => {
                write!(formatter, "{mix} Fade-to-Black is invalid: {error}")
            }
            Self::StingerCutPointOutOfRange {
                slot,
                cut_point_frames,
                duration_frames,
            } => write!(
                formatter,
                "Stinger {} cut point {cut_point_frames} exceeds duration {duration_frames}",
                slot.number()
            ),
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

/// One overlay channel's source and faded opacity in a composed mix frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MixOverlay {
    pub source: MixLayer,
    pub opacity: u8,
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
    pub overlays: [Option<MixOverlay>; OVERLAY_CHANNEL_COUNT],
    pub fade_to_black: FadeToBlackFrame,
}

/// All configured mixes for one bank frame, in ascending mix order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MixBankFrame {
    pub frame: u64,
    pub mixes: Vec<MixFrame>,
}

impl MixBankFrame {
    /// An empty bank frame with no configured mixes.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            frame: 0,
            mixes: Vec::new(),
        }
    }

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
            MixBankCommand::StartTBar { mix, kind } => {
                let state = self.require_idle(mix)?;
                if state.preview.is_none() {
                    return Err(MixBankError::PreviewUnavailable(mix));
                }
                if matches!(kind, TransitionKind::Stinger(_)) {
                    return Err(MixBankError::UnsupportedManualTransitionKind(mix));
                }
                let from = state.program;
                let to = state.preview.expect("validated Preview/Program mix");
                self.state_mut(mix).t_bar = Some(MixTBarState {
                    kind,
                    from,
                    to,
                    position: TBarPosition::START,
                });
                Ok(vec![MixBankEvent::TBarStarted {
                    mix,
                    kind,
                    from,
                    to,
                }])
            }
            MixBankCommand::SetTBarPosition { mix, position } => {
                let state = self.require_mix(mix)?;
                let Some(mut t_bar) = state.t_bar else {
                    return Err(MixBankError::NoTBar(mix));
                };
                t_bar.position = position;
                self.state_mut(mix).t_bar = Some(t_bar);
                Ok(vec![MixBankEvent::TBarPositionChanged { mix, position }])
            }
            MixBankCommand::CommitTBar(mix) => {
                let state = self.require_mix(mix)?;
                let Some(t_bar) = state.t_bar else {
                    return Err(MixBankError::NoTBar(mix));
                };
                let state = self.state_mut(mix);
                state.t_bar = None;
                let previous = state.program;
                state.program = t_bar.to;
                if state.preview.is_some() {
                    state.preview = Some(t_bar.from);
                }
                Ok(vec![
                    MixBankEvent::ProgramChanged {
                        mix,
                        previous,
                        program: t_bar.to,
                    },
                    MixBankEvent::TransitionCompleted {
                        mix,
                        kind: t_bar.kind,
                        program: t_bar.to,
                    },
                ])
            }
            MixBankCommand::CancelTBar(mix) => {
                let state = self.require_mix(mix)?;
                if state.t_bar.is_none() {
                    return Err(MixBankError::NoTBar(mix));
                }
                self.state_mut(mix).t_bar = None;
                Ok(vec![MixBankEvent::TBarCancelled { mix }])
            }
            MixBankCommand::FadeToBlack {
                mix,
                target,
                duration_frames,
                curve,
            } => {
                let mut controller = self.require_mix(mix)?.fade_to_black;
                match controller.request_with_curve(target, duration_frames, curve) {
                    Ok(FadeToBlackRequest::Unchanged) => Ok(Vec::new()),
                    Ok(FadeToBlackRequest::Started(started)) => {
                        self.state_mut(mix).fade_to_black = controller;
                        Ok(vec![MixBankEvent::FadeToBlackStarted {
                            mix,
                            from: started.from,
                            target: started.target,
                            duration_frames: started.duration_frames,
                        }])
                    }
                    Ok(FadeToBlackRequest::Completed(target)) => {
                        self.state_mut(mix).fade_to_black = controller;
                        Ok(vec![MixBankEvent::FadeToBlackCompleted { mix, target }])
                    }
                    Err(error) => Err(MixBankError::FadeToBlack { mix, error }),
                }
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

        for mix in MixId::all() {
            let Some(state) = self.mixes[mix.index()].as_mut() else {
                continue;
            };
            let advance = state.fade_to_black.advance();
            if let Some(position) = advance.position_changed {
                events.push(MixBankEvent::FadeToBlackPositionChanged { mix, position });
            }
            if let Some(target) = advance.completed {
                events.push(MixBankEvent::FadeToBlackCompleted { mix, target });
            }
            for channel in OverlayChannelId::ALL {
                let Some(overlay) = state.overlays[channel.index()].as_mut() else {
                    continue;
                };
                if let Some(opacity) = overlay.advance() {
                    events.push(MixBankEvent::OverlayFadeChanged {
                        mix,
                        channel,
                        opacity,
                    });
                    if overlay.fade.is_none() && opacity == 0 {
                        state.overlays[channel.index()] = None;
                        events.push(MixBankEvent::OverlayChanged {
                            mix,
                            channel,
                            source: None,
                        });
                    }
                }
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
                MixEffect::OverlayOn {
                    source,
                    duration_frames,
                    ..
                } => {
                    self.require_mix(mix)?;
                    self.validate_route(mix, source)?;
                    if duration_frames > MAX_OVERLAY_TRANSITION_DURATION_FRAMES {
                        return Err(MixBankError::InvalidOverlayDuration {
                            duration_frames,
                            maximum: MAX_OVERLAY_TRANSITION_DURATION_FRAMES,
                        });
                    }
                }
                MixEffect::OverlayOff {
                    duration_frames, ..
                } => {
                    self.require_mix(mix)?;
                    if duration_frames > MAX_OVERLAY_TRANSITION_DURATION_FRAMES {
                        return Err(MixBankError::InvalidOverlayDuration {
                            duration_frames,
                            maximum: MAX_OVERLAY_TRANSITION_DURATION_FRAMES,
                        });
                    }
                }
                MixEffect::Stinger {
                    slot,
                    duration_frames,
                    action,
                } => {
                    if matches!(action, MixStingerAction::Unconfigured) {
                        return Err(MixBankError::UnconfiguredStinger(slot));
                    }
                    self.validate_preview_transition(mix, duration_frames)?;
                    if let MixStingerAction::Play { cut_point_frames } = action
                        && cut_point_frames > duration_frames
                    {
                        return Err(MixBankError::StingerCutPointOutOfRange {
                            slot,
                            cut_point_frames,
                            duration_frames,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one arm per mix effect
    fn apply_effect(&mut self, mix: MixId, effect: MixEffect, events: &mut Vec<MixBankEvent>) {
        match effect {
            MixEffect::OverlayOn {
                channel,
                source,
                duration_frames,
            } => {
                let state = self.state_mut(mix);
                let from =
                    state.overlays[channel.index()].map_or(0, MixOverlayState::frame_opacity);
                let overlay = MixOverlayState {
                    source,
                    opacity: if duration_frames == 0 { u8::MAX } else { from },
                    fade: (duration_frames != 0).then_some(OverlayFade {
                        from,
                        to: u8::MAX,
                        duration_frames,
                        elapsed_frames: 0,
                    }),
                };
                state.overlays[channel.index()] = Some(overlay);
                events.push(MixBankEvent::OverlayChanged {
                    mix,
                    channel,
                    source: Some(source),
                });
                if duration_frames != 0 {
                    events.push(MixBankEvent::OverlayFadeChanged {
                        mix,
                        channel,
                        opacity: overlay.frame_opacity(),
                    });
                }
            }
            MixEffect::OverlayOff {
                channel,
                duration_frames,
            } => {
                let state = self.state_mut(mix);
                let Some(mut overlay) = state.overlays[channel.index()] else {
                    events.push(MixBankEvent::OverlayChanged {
                        mix,
                        channel,
                        source: None,
                    });
                    return;
                };
                if duration_frames == 0 {
                    state.overlays[channel.index()] = None;
                    events.push(MixBankEvent::OverlayChanged {
                        mix,
                        channel,
                        source: None,
                    });
                } else {
                    let from = overlay.frame_opacity();
                    overlay.opacity = from;
                    overlay.fade = Some(OverlayFade {
                        from,
                        to: 0,
                        duration_frames,
                        elapsed_frames: 0,
                    });
                    state.overlays[channel.index()] = Some(overlay);
                    events.push(MixBankEvent::OverlayFadeChanged {
                        mix,
                        channel,
                        opacity: overlay.frame_opacity(),
                    });
                }
            }
            MixEffect::Stinger {
                slot,
                duration_frames,
                action,
            } => match action {
                MixStingerAction::Play { .. } => events.push(self.start_preview_transition(
                    mix,
                    TransitionKind::Stinger(slot),
                    duration_frames,
                )),
                MixStingerAction::Fallback(fallback) => {
                    events.push(MixBankEvent::StingerFallbackApplied {
                        mix,
                        slot,
                        fallback,
                    });
                    match fallback {
                        MissingMediaFallback::Cut => {
                            let state = self.state_mut(mix);
                            let previous = state.program;
                            let preview = state.preview.expect("validated Preview/Program mix");
                            state.program = preview;
                            state.preview = Some(previous);
                            events.push(MixBankEvent::ProgramChanged {
                                mix,
                                previous,
                                program: preview,
                            });
                        }
                        MissingMediaFallback::Fade => events.push(self.start_preview_transition(
                            mix,
                            TransitionKind::Fade,
                            duration_frames,
                        )),
                        MissingMediaFallback::KeepProgram => {}
                    }
                }
                MixStingerAction::Unconfigured => {
                    unreachable!("an unconfigured Stinger is refused before the boundary")
                }
            },
        }
    }

    fn require_mix(&self, mix: MixId) -> Result<&MixState, MixBankError> {
        self.mixes[mix.index()]
            .as_ref()
            .ok_or(MixBankError::UnknownMix(mix))
    }

    fn require_idle(&self, mix: MixId) -> Result<&MixState, MixBankError> {
        let state = self.require_mix(mix)?;
        if state.transition.is_some() || state.t_bar.is_some() {
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

fn new_mix_state(config: MixConfig) -> MixState {
    let (program, preview) = match config {
        MixConfig::PreviewProgram { program, preview } => (program, Some(preview)),
        MixConfig::OutputOnly { program } => (program, None),
    };
    MixState {
        program,
        preview,
        transition: None,
        t_bar: None,
        overlays: [None; OVERLAY_CHANNEL_COUNT],
        fade_to_black: FadeToBlackController::default(),
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
    if let Some(t_bar) = state.t_bar {
        return MixFrame {
            mix,
            primary: resolve(t_bar.from, frame),
            secondary: Some(resolve(t_bar.to, frame)),
            transition_kind: Some(t_bar.kind),
            mix_numerator: u32::from(t_bar.position.basis_points()),
            mix_denominator: u32::from(TBarPosition::MAX),
            preview,
            overlays: state.overlays.map(|overlay| {
                overlay.map(|overlay| MixOverlay {
                    source: resolve(overlay.source, frame),
                    opacity: overlay.frame_opacity(),
                })
            }),
            fade_to_black: state.fade_to_black.frame(),
        };
    }
    match state.transition {
        Some(transition) => MixFrame {
            mix,
            primary: resolve(transition.from, frame),
            secondary: Some(resolve(transition.to, frame)),
            transition_kind: Some(transition.kind),
            mix_numerator: transition.elapsed_frames,
            mix_denominator: transition.duration_frames,
            preview,
            overlays: state.overlays.map(|overlay| {
                overlay.map(|overlay| MixOverlay {
                    source: resolve(overlay.source, frame),
                    opacity: overlay.frame_opacity(),
                })
            }),
            fade_to_black: state.fade_to_black.frame(),
        },
        None => MixFrame {
            mix,
            primary: resolve(state.program, frame),
            secondary: None,
            transition_kind: None,
            mix_numerator: 0,
            mix_denominator: 1,
            preview,
            overlays: state.overlays.map(|overlay| {
                overlay.map(|overlay| MixOverlay {
                    source: resolve(overlay.source, frame),
                    opacity: overlay.frame_opacity(),
                })
            }),
            fade_to_black: state.fade_to_black.frame(),
        },
    }
}
