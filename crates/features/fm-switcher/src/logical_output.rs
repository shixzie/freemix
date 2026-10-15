//! Four independent logical outputs and clean-feed routing.
//!
//! [`LogicalOutputs`] holds the desired routing of logical outputs 1 through
//! 4. Each enabled slot binds a project [`OutputId`] to:
//!
//! - an [`OutputSource`]: Program, Preview, one input, one mix, or a multiview
//!   grid whose cells may show other logical outputs;
//! - an [`OverlayPolicy`]: every active overlay channel, none (a clean feed), a
//!   fixed channel subset, or the per-channel output routing already stored in
//!   [`SwitcherState`] via `SetOverlayOutputInclusion`;
//! - an [`AudioMap`]: the master bus, one named bus, or audio that follows the
//!   selected video source.
//!
//! [`LogicalOutputs::resolve`] turns the routing plus the current switcher
//! (and, optionally, [`MixBank`](crate::MixBank) frame) into one
//! [`OutputRenderPlan`] per enabled slot. The resolver is a pure function of
//! its inputs: it never mutates switcher state and produces identical plans for
//! identical inputs.
//!
//! # Structural and live validation
//!
//! Structural rules are enforced on every mutation, which is atomic and leaves
//! the routing unchanged on error: project output identifiers are unique
//! across slots, a multiview holds between one and [`MAX_OUTPUT_MULTIVIEW_CELLS`]
//! distinct cells, every referenced logical output is enabled, and no output
//! may (transitively) display itself through multiview cells.
//!
//! Live rules depend on switcher state and are checked by
//! [`LogicalOutputs::resolve`] per output: inputs must be part of the
//! switcher, secondary mixes must be present in the supplied bank frame, and
//! named buses must be known. A failing output never affects the plans of
//! independent outputs; a multiview that shows a failing output fails with
//! [`OutputRoutingError::DependentOutputUnavailable`].
//!
//! # Source semantics
//!
//! - `Program` and `Mix(MixId::MAIN)` read [`SwitcherState::program_frame`],
//!   including any running timed or manual transition, and carry the current
//!   Fade-to-Black position when Fade to Black affects the output's
//!   [`OutputId`] ([`SwitcherState::fade_to_black_affects`]); an excluded
//!   output shows Program live. Program cells of a multiview follow the
//!   multiview output's own selection.
//! - `Preview` shows the switcher Preview input as a settled, unfaded layer.
//! - `Input(id)` shows that input as a settled, unfaded layer.
//! - `Mix(n)` for a secondary mix copies that mix's [`MixFrame`] background,
//!   including its one-frame-delayed mix-to-mix reads.
//! - `MultiView` composes cells in declaration order. Program/Preview/Input/Mix
//!   cells are clean backgrounds; `Output(slot)` cells show that output's full
//!   plan, overlays included. [`OutputRoutingFrame::render_order`] lists the
//!   enabled outputs so that every referenced output precedes its viewers.
//!
//! Overlay layers come from the main switcher overlay channels for Program,
//! Preview, and Input sources, and from the per-mix overlay channels of a
//! secondary mix. Multiview outputs never carry their own overlay layers.

use core::fmt;

use fm_types::{BusId, InputId, OutputId};

use crate::{
    FadeToBlackPosition, MixBankFrame, MixFrame, MixId, MixLayer, OVERLAY_CHANNEL_COUNT,
    OverlayBorderPreset, OverlayChannelId, OverlayPositionPreset, SwitcherState, TransitionKind,
};

/// Number of independently routed logical outputs.
pub const LOGICAL_OUTPUT_COUNT: usize = 4;
/// Maximum number of cells in one logical-output multiview.
pub const MAX_OUTPUT_MULTIVIEW_CELLS: usize = 16;

/// A one-based logical output number in the range one through four.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LogicalOutputSlot(u8);

impl LogicalOutputSlot {
    pub const ALL: [Self; LOGICAL_OUTPUT_COUNT] = [Self(1), Self(2), Self(3), Self(4)];

    /// Creates a slot from its one-based operator-facing number.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= LOGICAL_OUTPUT_COUNT {
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
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

impl fmt::Display for LogicalOutputSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "output {}", self.0)
    }
}

/// One multiview cell selection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MultiViewCell {
    Program,
    Preview,
    Input(InputId),
    Mix(MixId),
    /// The complete plan of another logical output, overlays included.
    Output(LogicalOutputSlot),
}

/// What a logical output shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputSource {
    Program,
    Preview,
    Input(InputId),
    Mix(MixId),
    /// Cells in grid reading order; geometry is planned by the compositor.
    MultiView(Vec<MultiViewCell>),
}

/// A set of overlay channels.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct OverlaySet(u8);

impl OverlaySet {
    pub const EMPTY: Self = Self(0);
    pub const ALL: Self = Self(u8::MAX);

    #[must_use]
    pub fn from_channels(channels: impl IntoIterator<Item = OverlayChannelId>) -> Self {
        let mut set = Self::EMPTY;
        for channel in channels {
            set.insert(channel);
        }
        set
    }

    pub const fn insert(&mut self, channel: OverlayChannelId) {
        self.0 |= 1 << (channel.number() - 1);
    }

    #[must_use]
    pub const fn contains(self, channel: OverlayChannelId) -> bool {
        self.0 & (1 << (channel.number() - 1)) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = OverlayChannelId> {
        OverlayChannelId::ALL
            .into_iter()
            .filter(move |channel| self.contains(*channel))
    }
}

/// Which overlay channels a logical output composites.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayPolicy {
    /// Every visible overlay channel.
    #[default]
    All,
    /// No overlays: a clean feed.
    Clean,
    /// Only the listed channels.
    Channels(OverlaySet),
    /// Channels whose switcher output routing includes this output's
    /// [`OutputId`] (see [`crate::OverlayChannelState::is_included_in`]).
    Routed,
}

/// Which audio a logical output carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AudioMap {
    #[default]
    Master,
    Bus(BusId),
    /// Audio follows the selected video source.
    ///
    /// Program and the Main mix carry Master; Preview and Input carry the
    /// shown input's own audio; a secondary mix follows its current primary
    /// layer (through other mixes); a mix layer that is still black and every
    /// multiview are silent.
    FollowVideo,
}

/// Routing for one enabled logical output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalOutputConfig {
    pub output: OutputId,
    pub source: OutputSource,
    pub overlays: OverlayPolicy,
    pub audio: AudioMap,
}

impl LogicalOutputConfig {
    /// Creates a Program output with all overlays and the master bus.
    #[must_use]
    pub const fn program(output: OutputId) -> Self {
        Self {
            output,
            source: OutputSource::Program,
            overlays: OverlayPolicy::All,
            audio: AudioMap::Master,
        }
    }

    #[must_use]
    pub fn with_source(mut self, source: OutputSource) -> Self {
        self.source = source;
        self
    }

    #[must_use]
    pub const fn with_overlays(mut self, overlays: OverlayPolicy) -> Self {
        self.overlays = overlays;
        self
    }

    #[must_use]
    pub const fn with_audio(mut self, audio: AudioMap) -> Self {
        self.audio = audio;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputRoutingError {
    /// The slot is disabled.
    OutputDisabled(LogicalOutputSlot),
    /// Another slot already uses this project output identifier.
    DuplicateOutputId {
        slot: LogicalOutputSlot,
        output: OutputId,
        existing: LogicalOutputSlot,
    },
    EmptyMultiView(LogicalOutputSlot),
    MultiViewCellLimit {
        slot: LogicalOutputSlot,
        cells: usize,
        maximum: usize,
    },
    DuplicateMultiViewCell {
        slot: LogicalOutputSlot,
        cell: MultiViewCell,
    },
    /// A multiview cell names a disabled logical output.
    UnknownOutput {
        slot: LogicalOutputSlot,
        referenced: LogicalOutputSlot,
    },
    /// Routing `output` would make it display itself; `via` is the cell
    /// target that closes the loop (equal to `output` for a direct self-reference).
    MultiViewRecursion {
        output: LogicalOutputSlot,
        via: LogicalOutputSlot,
    },
    UnknownInput {
        slot: LogicalOutputSlot,
        input: InputId,
    },
    UnknownMix {
        slot: LogicalOutputSlot,
        mix: MixId,
    },
    UnknownBus {
        slot: LogicalOutputSlot,
        bus: BusId,
    },
    /// A multiview cell shows an output whose own plan failed.
    DependentOutputUnavailable {
        slot: LogicalOutputSlot,
        referenced: LogicalOutputSlot,
    },
}

impl fmt::Display for OutputRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputDisabled(slot) => write!(formatter, "{slot} is disabled"),
            Self::DuplicateOutputId {
                slot,
                output,
                existing,
            } => write!(
                formatter,
                "{slot} cannot use project output {output}; {existing} already uses it"
            ),
            Self::EmptyMultiView(slot) => write!(formatter, "{slot} MultiView has no cells"),
            Self::MultiViewCellLimit {
                slot,
                cells,
                maximum,
            } => write!(
                formatter,
                "{slot} MultiView has {cells} cells; maximum is {maximum}"
            ),
            Self::DuplicateMultiViewCell { slot, cell } => {
                write!(formatter, "{slot} MultiView repeats cell {cell:?}")
            }
            Self::UnknownOutput { slot, referenced } => {
                write!(formatter, "{slot} MultiView shows disabled {referenced}")
            }
            Self::MultiViewRecursion { output, via } => write!(
                formatter,
                "{output} would display itself through a MultiView cell showing {via}"
            ),
            Self::UnknownInput { slot, input } => {
                write!(formatter, "{slot} routes unknown input {input}")
            }
            Self::UnknownMix { slot, mix } => write!(formatter, "{slot} routes unconfigured {mix}"),
            Self::UnknownBus { slot, bus } => write!(formatter, "{slot} routes unknown bus {bus}"),
            Self::DependentOutputUnavailable { slot, referenced } => write!(
                formatter,
                "{slot} MultiView shows {referenced}, which cannot be planned"
            ),
        }
    }
}

impl std::error::Error for OutputRoutingError {}

/// Desired routing of the four logical outputs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogicalOutputs {
    slots: [Option<LogicalOutputConfig>; LOGICAL_OUTPUT_COUNT],
}

impl LogicalOutputs {
    /// Creates routing with every slot disabled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn slot(&self, slot: LogicalOutputSlot) -> Option<&LogicalOutputConfig> {
        self.slots[slot.index()].as_ref()
    }

    /// Enabled slots in ascending order.
    pub fn enabled(&self) -> impl Iterator<Item = (LogicalOutputSlot, &LogicalOutputConfig)> {
        LogicalOutputSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot(slot).map(|config| (slot, config)))
    }

    /// Enables, replaces, or (with `None`) disables one slot atomically.
    ///
    /// # Errors
    ///
    /// Returns the first violated structural rule; the routing is unchanged.
    pub fn configure(
        &mut self,
        slot: LogicalOutputSlot,
        config: Option<LogicalOutputConfig>,
    ) -> Result<(), OutputRoutingError> {
        let mut candidate = self.slots.clone();
        candidate[slot.index()] = config;
        validate_structure(&candidate)?;
        self.slots = candidate;
        Ok(())
    }

    /// Changes only the source of an enabled slot.
    ///
    /// # Errors
    ///
    /// Returns [`OutputRoutingError::OutputDisabled`] or a structural error.
    pub fn set_source(
        &mut self,
        slot: LogicalOutputSlot,
        source: OutputSource,
    ) -> Result<(), OutputRoutingError> {
        let config = self.require(slot)?.clone().with_source(source);
        self.configure(slot, Some(config))
    }

    /// Changes only the overlay policy of an enabled slot.
    ///
    /// # Errors
    ///
    /// Returns [`OutputRoutingError::OutputDisabled`].
    pub fn set_overlay_policy(
        &mut self,
        slot: LogicalOutputSlot,
        overlays: OverlayPolicy,
    ) -> Result<(), OutputRoutingError> {
        let config = self.require(slot)?.clone().with_overlays(overlays);
        self.configure(slot, Some(config))
    }

    /// Changes only the audio map of an enabled slot.
    ///
    /// # Errors
    ///
    /// Returns [`OutputRoutingError::OutputDisabled`].
    pub fn set_audio_map(
        &mut self,
        slot: LogicalOutputSlot,
        audio: AudioMap,
    ) -> Result<(), OutputRoutingError> {
        let config = self.require(slot)?.clone().with_audio(audio);
        self.configure(slot, Some(config))
    }

    /// Resolves every enabled slot against the current switcher state.
    #[must_use]
    pub fn resolve(&self, context: &RoutingContext<'_>) -> OutputRoutingFrame {
        let render_order = render_order(&self.slots);
        let mut outputs: [Option<Result<OutputRenderPlan, OutputRoutingError>>;
            LOGICAL_OUTPUT_COUNT] = Default::default();
        for slot in render_order.iter().copied() {
            let Some(config) = self.slots[slot.index()].as_ref() else {
                continue;
            };
            let plan = resolve_output(slot, config, context, &outputs);
            outputs[slot.index()] = Some(plan);
        }
        OutputRoutingFrame {
            outputs,
            render_order,
        }
    }

    fn require(&self, slot: LogicalOutputSlot) -> Result<&LogicalOutputConfig, OutputRoutingError> {
        self.slot(slot)
            .ok_or(OutputRoutingError::OutputDisabled(slot))
    }
}

/// Live state that logical outputs read from.
#[derive(Clone, Copy, Debug)]
pub struct RoutingContext<'a> {
    pub switcher: &'a SwitcherState,
    /// The current bank frame; secondary mixes are unknown without one.
    pub mixes: Option<&'a MixBankFrame>,
    /// Buses that [`AudioMap::Bus`] may name.
    pub buses: &'a [BusId],
}

/// One composited video layer reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedLayer {
    Input(InputId),
    /// A mix Program composed in bank frame `frame`.
    Mix {
        mix: MixId,
        frame: u64,
    },
    Black,
}

/// The background of one feed, before overlays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundPlan {
    pub primary: FeedLayer,
    pub secondary: Option<FeedLayer>,
    pub transition_kind: Option<TransitionKind>,
    pub mix_numerator: u32,
    pub mix_denominator: u32,
    /// Fade-to-Black applied after the background; live for non-Program feeds.
    pub fade_to_black: FadeToBlackPosition,
}

impl BackgroundPlan {
    const fn settled(primary: FeedLayer) -> Self {
        Self {
            primary,
            secondary: None,
            transition_kind: None,
            mix_numerator: 0,
            mix_denominator: 1,
            fade_to_black: FadeToBlackPosition::LIVE,
        }
    }
}

/// What one multiview cell displays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellFeed {
    Background(BackgroundPlan),
    /// The resolved plan of another logical output, rendered earlier.
    Output(LogicalOutputSlot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiViewCellPlan {
    pub cell: MultiViewCell,
    pub feed: CellFeed,
}

/// The video below a logical output's overlays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputVideo {
    Background(BackgroundPlan),
    MultiView(Vec<MultiViewCellPlan>),
}

/// One overlay layer composited above the background, in channel order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayLayer {
    pub channel: OverlayChannelId,
    pub source: InputId,
    pub opacity: u8,
    pub position: OverlayPositionPreset,
    pub border: OverlayBorderPreset,
    /// The motion transition moving this layer in this frame, if any.
    pub motion: Option<crate::OverlayMotion>,
}

/// Resolved audio for one logical output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioSource {
    Master,
    Bus(BusId),
    Input(InputId),
    Silent,
}

/// Everything a renderer needs for one logical output in one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputRenderPlan {
    pub slot: LogicalOutputSlot,
    pub output: OutputId,
    pub video: OutputVideo,
    /// Bottom-to-top overlay layers above [`Self::video`].
    pub overlays: Vec<OverlayLayer>,
    pub audio: AudioSource,
}

/// Plans for all slots in one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputRoutingFrame {
    /// Indexed by slot; `None` for a disabled slot.
    pub outputs: [Option<Result<OutputRenderPlan, OutputRoutingError>>; LOGICAL_OUTPUT_COUNT],
    /// Enabled slots ordered so referenced outputs precede multiviews that show
    /// them; each step takes the lowest-numbered slot whose references are placed.
    pub render_order: Vec<LogicalOutputSlot>,
}

impl OutputRoutingFrame {
    #[must_use]
    pub fn output(
        &self,
        slot: LogicalOutputSlot,
    ) -> Option<&Result<OutputRenderPlan, OutputRoutingError>> {
        self.outputs[slot.index()].as_ref()
    }
}

type Slots = [Option<LogicalOutputConfig>; LOGICAL_OUTPUT_COUNT];

fn referenced_outputs(
    config: &LogicalOutputConfig,
) -> impl Iterator<Item = LogicalOutputSlot> + '_ {
    let cells: &[MultiViewCell] = match &config.source {
        OutputSource::MultiView(cells) => cells,
        _ => &[],
    };
    cells.iter().filter_map(|cell| match cell {
        MultiViewCell::Output(slot) => Some(*slot),
        _ => None,
    })
}

fn validate_structure(slots: &Slots) -> Result<(), OutputRoutingError> {
    for slot in LogicalOutputSlot::ALL {
        let Some(config) = &slots[slot.index()] else {
            continue;
        };
        if let Some(existing) =
            LogicalOutputSlot::ALL[..slot.index()]
                .iter()
                .copied()
                .find(|other| {
                    slots[other.index()]
                        .as_ref()
                        .is_some_and(|other| other.output == config.output)
                })
        {
            return Err(OutputRoutingError::DuplicateOutputId {
                slot,
                output: config.output,
                existing,
            });
        }
        if let OutputSource::MultiView(cells) = &config.source {
            if cells.is_empty() {
                return Err(OutputRoutingError::EmptyMultiView(slot));
            }
            if cells.len() > MAX_OUTPUT_MULTIVIEW_CELLS {
                return Err(OutputRoutingError::MultiViewCellLimit {
                    slot,
                    cells: cells.len(),
                    maximum: MAX_OUTPUT_MULTIVIEW_CELLS,
                });
            }
            for (index, cell) in cells.iter().enumerate() {
                if cells[..index].contains(cell) {
                    return Err(OutputRoutingError::DuplicateMultiViewCell { slot, cell: *cell });
                }
            }
        }
        for referenced in referenced_outputs(config) {
            if referenced == slot {
                return Err(OutputRoutingError::MultiViewRecursion {
                    output: slot,
                    via: slot,
                });
            }
            if slots[referenced.index()].is_none() {
                return Err(OutputRoutingError::UnknownOutput { slot, referenced });
            }
        }
    }
    for slot in LogicalOutputSlot::ALL {
        if let Some(via) = find_cycle(slots, slot) {
            return Err(OutputRoutingError::MultiViewRecursion { output: slot, via });
        }
    }
    Ok(())
}

/// Returns the reference that leads back to `start`, if any.
fn find_cycle(slots: &Slots, start: LogicalOutputSlot) -> Option<LogicalOutputSlot> {
    let config = slots[start.index()].as_ref()?;
    let mut visited = [false; LOGICAL_OUTPUT_COUNT];
    let mut stack: Vec<(LogicalOutputSlot, LogicalOutputSlot)> = referenced_outputs(config)
        .map(|referenced| (referenced, referenced))
        .collect();
    while let Some((current, first_hop)) = stack.pop() {
        if current == start {
            return Some(first_hop);
        }
        if std::mem::replace(&mut visited[current.index()], true) {
            continue;
        }
        if let Some(config) = &slots[current.index()] {
            stack.extend(referenced_outputs(config).map(|next| (next, first_hop)));
        }
    }
    None
}

/// Enabled slots in dependency order: each step emits the lowest-numbered
/// enabled slot whose multiview references have all been emitted.
fn render_order(slots: &Slots) -> Vec<LogicalOutputSlot> {
    let mut order = Vec::with_capacity(LOGICAL_OUTPUT_COUNT);
    let mut placed = [false; LOGICAL_OUTPUT_COUNT];
    loop {
        let next = LogicalOutputSlot::ALL.into_iter().find(|slot| {
            !placed[slot.index()]
                && slots[slot.index()].as_ref().is_some_and(|config| {
                    referenced_outputs(config).all(|referenced| placed[referenced.index()])
                })
        });
        let Some(next) = next else {
            return order;
        };
        placed[next.index()] = true;
        order.push(next);
    }
}

fn resolve_output(
    slot: LogicalOutputSlot,
    config: &LogicalOutputConfig,
    context: &RoutingContext<'_>,
    resolved: &[Option<Result<OutputRenderPlan, OutputRoutingError>>; LOGICAL_OUTPUT_COUNT],
) -> Result<OutputRenderPlan, OutputRoutingError> {
    let (video, overlays, follow_audio) = match &config.source {
        OutputSource::Program => (
            OutputVideo::Background(program_background(context.switcher, config.output)),
            main_overlays(context.switcher),
            AudioSource::Master,
        ),
        OutputSource::Preview => {
            let preview = context.switcher.preview();
            (
                OutputVideo::Background(BackgroundPlan::settled(FeedLayer::Input(preview))),
                main_overlays(context.switcher),
                AudioSource::Input(preview),
            )
        }
        OutputSource::Input(input) => {
            require_input(slot, context, *input)?;
            (
                OutputVideo::Background(BackgroundPlan::settled(FeedLayer::Input(*input))),
                main_overlays(context.switcher),
                AudioSource::Input(*input),
            )
        }
        OutputSource::Mix(mix) if mix.is_main() => (
            OutputVideo::Background(program_background(context.switcher, config.output)),
            main_overlays(context.switcher),
            AudioSource::Master,
        ),
        OutputSource::Mix(mix) => {
            let frame = require_mix(slot, context, *mix)?;
            (
                OutputVideo::Background(mix_background(frame)),
                mix_overlays(frame),
                follow_mix_audio(context, frame),
            )
        }
        OutputSource::MultiView(cells) => {
            let cells = cells
                .iter()
                .map(|cell| resolve_cell(slot, config.output, *cell, context, resolved))
                .collect::<Result<Vec<_>, _>>()?;
            (
                OutputVideo::MultiView(cells),
                Vec::new(),
                AudioSource::Silent,
            )
        }
    };
    let overlays = overlays
        .into_iter()
        .filter(|layer| match config.overlays {
            OverlayPolicy::All => true,
            OverlayPolicy::Clean => false,
            OverlayPolicy::Channels(set) => set.contains(layer.channel),
            OverlayPolicy::Routed => context
                .switcher
                .overlay(layer.channel)
                .is_included_in(config.output),
        })
        .collect();
    let audio = match config.audio {
        AudioMap::Master => AudioSource::Master,
        AudioMap::Bus(bus) => {
            if !context.buses.contains(&bus) {
                return Err(OutputRoutingError::UnknownBus { slot, bus });
            }
            AudioSource::Bus(bus)
        }
        AudioMap::FollowVideo => follow_audio,
    };
    Ok(OutputRenderPlan {
        slot,
        output: config.output,
        video,
        overlays,
        audio,
    })
}

fn resolve_cell(
    slot: LogicalOutputSlot,
    output: OutputId,
    cell: MultiViewCell,
    context: &RoutingContext<'_>,
    resolved: &[Option<Result<OutputRenderPlan, OutputRoutingError>>; LOGICAL_OUTPUT_COUNT],
) -> Result<MultiViewCellPlan, OutputRoutingError> {
    let feed = match cell {
        MultiViewCell::Program => {
            CellFeed::Background(program_background(context.switcher, output))
        }
        MultiViewCell::Mix(mix) if mix.is_main() => {
            CellFeed::Background(program_background(context.switcher, output))
        }
        MultiViewCell::Preview => CellFeed::Background(BackgroundPlan::settled(FeedLayer::Input(
            context.switcher.preview(),
        ))),
        MultiViewCell::Input(input) => {
            require_input(slot, context, input)?;
            CellFeed::Background(BackgroundPlan::settled(FeedLayer::Input(input)))
        }
        MultiViewCell::Mix(mix) => {
            CellFeed::Background(mix_background(require_mix(slot, context, mix)?))
        }
        MultiViewCell::Output(referenced) => match &resolved[referenced.index()] {
            Some(Ok(_)) => CellFeed::Output(referenced),
            _ => {
                return Err(OutputRoutingError::DependentOutputUnavailable { slot, referenced });
            }
        },
    };
    Ok(MultiViewCellPlan { cell, feed })
}

fn require_input(
    slot: LogicalOutputSlot,
    context: &RoutingContext<'_>,
    input: InputId,
) -> Result<(), OutputRoutingError> {
    if context.switcher.inputs().contains(&input) {
        Ok(())
    } else {
        Err(OutputRoutingError::UnknownInput { slot, input })
    }
}

fn require_mix<'a>(
    slot: LogicalOutputSlot,
    context: &RoutingContext<'a>,
    mix: MixId,
) -> Result<&'a MixFrame, OutputRoutingError> {
    context
        .mixes
        .and_then(|frame| frame.mix(mix))
        .ok_or(OutputRoutingError::UnknownMix { slot, mix })
}

fn program_background(switcher: &SwitcherState, output: OutputId) -> BackgroundPlan {
    let frame = switcher.program_frame();
    BackgroundPlan {
        primary: FeedLayer::Input(frame.primary),
        secondary: frame.secondary.map(FeedLayer::Input),
        transition_kind: frame.transition_kind,
        mix_numerator: frame.mix_numerator,
        mix_denominator: frame.mix_denominator,
        fade_to_black: switcher.fade_to_black_position_for(output),
    }
}

const fn feed_layer(layer: MixLayer) -> FeedLayer {
    match layer {
        MixLayer::Input(input) => FeedLayer::Input(input),
        MixLayer::Mix { mix, frame } => FeedLayer::Mix { mix, frame },
        MixLayer::Black => FeedLayer::Black,
    }
}

fn mix_background(frame: &MixFrame) -> BackgroundPlan {
    BackgroundPlan {
        primary: feed_layer(frame.primary),
        secondary: frame.secondary.map(feed_layer),
        transition_kind: frame.transition_kind,
        mix_numerator: frame.mix_numerator,
        mix_denominator: frame.mix_denominator,
        fade_to_black: FadeToBlackPosition::LIVE,
    }
}

fn main_overlays(switcher: &SwitcherState) -> Vec<OverlayLayer> {
    OverlayChannelId::ALL
        .into_iter()
        .filter_map(|channel| {
            let state = switcher.overlay(channel);
            let source = state.source()?;
            let visible = (state.is_active() || state.is_transitioning()) && state.opacity() > 0;
            visible.then_some(OverlayLayer {
                channel,
                source,
                opacity: state.opacity(),
                position: state.position(),
                border: state.border(),
                motion: state.motion(),
            })
        })
        .collect()
}

fn mix_overlays(frame: &MixFrame) -> Vec<OverlayLayer> {
    debug_assert_eq!(frame.overlays.len(), OVERLAY_CHANNEL_COUNT);
    OverlayChannelId::ALL
        .into_iter()
        .zip(frame.overlays)
        .filter_map(|(channel, source)| {
            source.map(|source| OverlayLayer {
                channel,
                source,
                opacity: u8::MAX,
                position: OverlayPositionPreset::FullFrame,
                border: OverlayBorderPreset::None,
                motion: None,
            })
        })
        .collect()
}

fn follow_mix_audio(context: &RoutingContext<'_>, frame: &MixFrame) -> AudioSource {
    let mut layer = frame.primary;
    // Mix routing is acyclic, so a chain visits each mix at most once.
    for _ in 0..crate::MIX_BANK_CAPACITY {
        match layer {
            MixLayer::Input(input) => return AudioSource::Input(input),
            MixLayer::Black => return AudioSource::Silent,
            MixLayer::Mix { mix, .. } if mix.is_main() => return AudioSource::Master,
            MixLayer::Mix { mix, .. } => match context.mixes.and_then(|bank| bank.mix(mix)) {
                Some(next) => layer = next.primary,
                None => return AudioSource::Silent,
            },
        }
    }
    AudioSource::Silent
}
