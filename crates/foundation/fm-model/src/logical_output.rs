//! Logical output routing (SW-012).
//!
//! The show has four independently routed logical outputs. Each enabled slot
//! names one project output and what that output hears: Program, Preview, an
//! input, a secondary mix, or a bounded multiview of cells. A cell may show
//! another logical output, so the routing is validated for missing and
//! recursive references before it is accepted.

use core::fmt;

use fm_types::{BusId, InputId, OutputId};

/// Number of independently routed logical outputs.
pub const MAX_LOGICAL_OUTPUTS: usize = 4;
/// Maximum cells in one logical-output multiview.
pub const MAX_LOGICAL_OUTPUT_MULTIVIEW_CELLS: usize = 16;
/// Highest secondary mix number (Main is 1).
pub const MAX_LOGICAL_MIX_NUMBER: u8 = 16;
/// Number of overlay channels a logical output may select.
pub const LOGICAL_OVERLAY_CHANNELS: u8 = 8;

/// One logical output slot, numbered 1 through 4.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LogicalOutputSlot(u8);

impl LogicalOutputSlot {
    pub const ALL: [Self; MAX_LOGICAL_OUTPUTS] = [Self(1), Self(2), Self(3), Self(4)];

    /// Creates a slot from its one-based number.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= MAX_LOGICAL_OUTPUTS {
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
        write!(formatter, "logical output {}", self.0)
    }
}

/// One cell of a logical-output multiview.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LogicalMultiViewCell {
    Program,
    Preview,
    Input(InputId),
    Mix(u8),
    /// The complete plan of another logical output.
    Output(LogicalOutputSlot),
}

/// What a logical output shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalOutputSource {
    Program,
    Preview,
    Input(InputId),
    Mix(u8),
    /// Cells in grid reading order.
    MultiView(Vec<LogicalMultiViewCell>),
}

/// A set of overlay channels, numbered 1 through 8.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct LogicalOverlayChannels(u8);

impl LogicalOverlayChannels {
    pub const EMPTY: Self = Self(0);

    /// Builds a set from one-based channel numbers.
    ///
    /// # Errors
    ///
    /// Returns [`LogicalOutputError::InvalidOverlayChannel`] for a channel
    /// outside `1..=8`.
    pub fn from_channels(
        channels: impl IntoIterator<Item = u8>,
    ) -> Result<Self, LogicalOutputError> {
        let mut set = Self::EMPTY;
        for channel in channels {
            if set.contains(channel) {
                return Err(LogicalOutputError::DuplicateOverlayChannel(channel));
            }
            set.insert(channel)?;
        }
        Ok(set)
    }

    /// Adds one channel.
    ///
    /// # Errors
    ///
    /// Returns [`LogicalOutputError::InvalidOverlayChannel`] outside
    /// `1..=8`.
    pub fn insert(&mut self, channel: u8) -> Result<(), LogicalOutputError> {
        if !(1..=LOGICAL_OVERLAY_CHANNELS).contains(&channel) {
            return Err(LogicalOutputError::InvalidOverlayChannel(channel));
        }
        self.0 |= 1 << (channel - 1);
        Ok(())
    }

    #[must_use]
    pub const fn contains(self, channel: u8) -> bool {
        channel >= 1 && channel <= LOGICAL_OVERLAY_CHANNELS && self.0 & (1 << (channel - 1)) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = u8> {
        (1..=LOGICAL_OVERLAY_CHANNELS).filter(move |channel| self.contains(*channel))
    }
}

/// Which overlay channels a logical output composites.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogicalOverlayPolicy {
    #[default]
    All,
    Clean,
    Channels(LogicalOverlayChannels),
    /// The per-channel output inclusion already stored by the switcher.
    Routed,
}

/// Which audio a logical output carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogicalAudioMap {
    #[default]
    Master,
    Bus(BusId),
    /// Audio follows the selected video source.
    FollowVideo,
}

/// Routing for one enabled logical output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalOutputConfig {
    pub slot: LogicalOutputSlot,
    pub output: OutputId,
    pub source: LogicalOutputSource,
    pub overlays: LogicalOverlayPolicy,
    pub audio: LogicalAudioMap,
}

impl LogicalOutputConfig {
    /// A Program output with all overlays and the Master bus.
    #[must_use]
    pub const fn program(slot: LogicalOutputSlot, output: OutputId) -> Self {
        Self {
            slot,
            output,
            source: LogicalOutputSource::Program,
            overlays: LogicalOverlayPolicy::All,
            audio: LogicalAudioMap::Master,
        }
    }
}

/// Why logical-output routing is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalOutputError {
    TooManyOutputs { maximum: usize },
    OutOfOrder,
    DuplicateSlot(LogicalOutputSlot),
    DuplicateOutput(OutputId),
    UnknownOutput(OutputId),
    UnknownInput(InputId),
    InvalidMix(u8),
    EmptyMultiView,
    MultiViewCellLimit { cells: usize, maximum: usize },
    DuplicateMultiViewCell(usize),
    UnknownOutputSlot(LogicalOutputSlot),
    SelfReference(LogicalOutputSlot),
    Cycle,
    UnknownAudioBus(BusId),
    InvalidOverlayChannel(u8),
    DuplicateOverlayChannel(u8),
}

impl LogicalOutputError {
    /// The project output the error belongs to, when it names one.
    #[must_use]
    pub const fn output(&self) -> Option<OutputId> {
        match self {
            Self::DuplicateOutput(output) | Self::UnknownOutput(output) => Some(*output),
            _ => None,
        }
    }
}

impl fmt::Display for LogicalOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyOutputs { maximum } => {
                write!(formatter, "at most {maximum} logical outputs are allowed")
            }
            Self::OutOfOrder => formatter.write_str("logical outputs must be in slot order"),
            Self::DuplicateSlot(slot) => write!(formatter, "{slot} is configured twice"),
            Self::DuplicateOutput(output) => {
                write!(formatter, "project output {output} is routed twice")
            }
            Self::UnknownOutput(output) => write!(formatter, "unknown project output {output}"),
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::InvalidMix(mix) => write!(
                formatter,
                "mix {mix} is outside 1..={MAX_LOGICAL_MIX_NUMBER}"
            ),
            Self::EmptyMultiView => formatter.write_str("a multiview needs at least one cell"),
            Self::MultiViewCellLimit { cells, maximum } => write!(
                formatter,
                "a multiview holds {cells} cells; maximum is {maximum}"
            ),
            Self::DuplicateMultiViewCell(index) => {
                write!(formatter, "multiview cell {index} repeats an earlier cell")
            }
            Self::UnknownOutputSlot(slot) => write!(formatter, "{slot} is not enabled"),
            Self::SelfReference(slot) => write!(formatter, "{slot} displays itself"),
            Self::Cycle => formatter.write_str("logical outputs form a multiview cycle"),
            Self::UnknownAudioBus(bus) => write!(formatter, "unknown audio bus {bus}"),
            Self::InvalidOverlayChannel(channel) => write!(
                formatter,
                "overlay channel {channel} is outside 1..={LOGICAL_OVERLAY_CHANNELS}"
            ),
            Self::DuplicateOverlayChannel(channel) => {
                write!(formatter, "overlay channel {channel} is selected twice")
            }
        }
    }
}

impl std::error::Error for LogicalOutputError {}

/// Checks a complete logical-output routing against the show's project
/// references.
///
/// `buses` is the project's audio-bus list; `None` skips that check for a
/// caller that does not model the project's audio graph.
///
/// # Errors
///
/// Returns the first structural or reference error.
pub fn validate_logical_outputs(
    configs: &[LogicalOutputConfig],
    outputs: &[OutputId],
    inputs: &[InputId],
    buses: Option<&[BusId]>,
) -> Result<(), LogicalOutputError> {
    if configs.len() > MAX_LOGICAL_OUTPUTS {
        return Err(LogicalOutputError::TooManyOutputs {
            maximum: MAX_LOGICAL_OUTPUTS,
        });
    }
    let mut seen_slots = Vec::with_capacity(configs.len());
    let mut seen_outputs = Vec::with_capacity(configs.len());
    for config in configs {
        if seen_slots.last().is_some_and(|slot| *slot >= config.slot) {
            return Err(LogicalOutputError::OutOfOrder);
        }
        seen_slots.push(config.slot);
        if seen_outputs.contains(&config.output) {
            return Err(LogicalOutputError::DuplicateOutput(config.output));
        }
        seen_outputs.push(config.output);
        if !outputs.contains(&config.output) {
            return Err(LogicalOutputError::UnknownOutput(config.output));
        }
        if let LogicalAudioMap::Bus(bus) = config.audio
            && buses.is_some_and(|buses| !buses.contains(&bus))
        {
            return Err(LogicalOutputError::UnknownAudioBus(bus));
        }
        validate_source(&config.source, inputs)?;
    }
    for config in configs {
        validate_multiview_references(config, configs)?;
    }
    Ok(())
}

fn validate_source(
    source: &LogicalOutputSource,
    inputs: &[InputId],
) -> Result<(), LogicalOutputError> {
    let check_cell = |cell: &LogicalMultiViewCell| match cell {
        LogicalMultiViewCell::Program
        | LogicalMultiViewCell::Preview
        | LogicalMultiViewCell::Output(_) => Ok(()),
        LogicalMultiViewCell::Input(input) if inputs.contains(input) => Ok(()),
        LogicalMultiViewCell::Input(input) => Err(LogicalOutputError::UnknownInput(*input)),
        LogicalMultiViewCell::Mix(mix) if (1..=MAX_LOGICAL_MIX_NUMBER).contains(mix) => Ok(()),
        LogicalMultiViewCell::Mix(mix) => Err(LogicalOutputError::InvalidMix(*mix)),
    };
    match source {
        LogicalOutputSource::Program | LogicalOutputSource::Preview => Ok(()),
        LogicalOutputSource::Input(input) if inputs.contains(input) => Ok(()),
        LogicalOutputSource::Input(input) => Err(LogicalOutputError::UnknownInput(*input)),
        LogicalOutputSource::Mix(mix) if (1..=MAX_LOGICAL_MIX_NUMBER).contains(mix) => Ok(()),
        LogicalOutputSource::Mix(mix) => Err(LogicalOutputError::InvalidMix(*mix)),
        LogicalOutputSource::MultiView(cells) => {
            if cells.is_empty() {
                return Err(LogicalOutputError::EmptyMultiView);
            }
            if cells.len() > MAX_LOGICAL_OUTPUT_MULTIVIEW_CELLS {
                return Err(LogicalOutputError::MultiViewCellLimit {
                    cells: cells.len(),
                    maximum: MAX_LOGICAL_OUTPUT_MULTIVIEW_CELLS,
                });
            }
            for (index, cell) in cells.iter().enumerate() {
                if cells[..index].contains(cell) {
                    return Err(LogicalOutputError::DuplicateMultiViewCell(index));
                }
                check_cell(cell)?;
            }
            Ok(())
        }
    }
}

fn validate_multiview_references(
    config: &LogicalOutputConfig,
    configs: &[LogicalOutputConfig],
) -> Result<(), LogicalOutputError> {
    let LogicalOutputSource::MultiView(cells) = &config.source else {
        return Ok(());
    };
    for cell in cells {
        let LogicalMultiViewCell::Output(slot) = cell else {
            continue;
        };
        if *slot == config.slot {
            return Err(LogicalOutputError::SelfReference(config.slot));
        }
        if !configs.iter().any(|candidate| candidate.slot == *slot) {
            return Err(LogicalOutputError::UnknownOutputSlot(*slot));
        }
    }
    let mut visiting = Vec::new();
    let mut done = Vec::new();
    visit(config.slot, configs, &mut visiting, &mut done)
}

fn visit(
    slot: LogicalOutputSlot,
    configs: &[LogicalOutputConfig],
    visiting: &mut Vec<LogicalOutputSlot>,
    done: &mut Vec<LogicalOutputSlot>,
) -> Result<(), LogicalOutputError> {
    if done.contains(&slot) {
        return Ok(());
    }
    if visiting.contains(&slot) {
        return Err(LogicalOutputError::Cycle);
    }
    let Some(config) = configs.iter().find(|config| config.slot == slot) else {
        return Ok(());
    };
    visiting.push(slot);
    if let LogicalOutputSource::MultiView(cells) = &config.source {
        for cell in cells {
            if let LogicalMultiViewCell::Output(target) = cell {
                visit(*target, configs, visiting, done)?;
            }
        }
    }
    visiting.pop();
    done.push(slot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn output(value: u128) -> OutputId {
        OutputId::new(NonZeroU128::new(value).unwrap())
    }

    fn bus(value: u128) -> BusId {
        BusId::new(NonZeroU128::new(value).unwrap())
    }

    fn slot(value: u8) -> LogicalOutputSlot {
        LogicalOutputSlot::new(value).unwrap()
    }

    fn config(number: u8, output: u128, source: LogicalOutputSource) -> LogicalOutputConfig {
        LogicalOutputConfig {
            slot: slot(number),
            output: output_id(output),
            source,
            overlays: LogicalOverlayPolicy::All,
            audio: LogicalAudioMap::Master,
        }
    }

    fn output_id(value: u128) -> OutputId {
        output(value)
    }

    #[test]
    fn a_complete_routing_is_valid() {
        let configs = vec![
            config(1, 40, LogicalOutputSource::Program),
            config(2, 41, LogicalOutputSource::Input(input(10))),
            config(
                3,
                42,
                LogicalOutputSource::MultiView(vec![
                    LogicalMultiViewCell::Program,
                    LogicalMultiViewCell::Preview,
                    LogicalMultiViewCell::Output(slot(1)),
                ]),
            ),
            config(4, 43, LogicalOutputSource::Mix(2)),
        ];
        assert_eq!(
            validate_logical_outputs(
                &configs,
                &[output(40), output(41), output(42), output(43)],
                &[input(10)],
                Some(&[bus(30)])
            ),
            Ok(())
        );
    }

    #[test]
    fn references_duplicates_and_cycles_are_refused() {
        let outputs = [output(40), output(41)];
        let inputs = [input(10)];
        assert_eq!(
            validate_logical_outputs(
                &[config(1, 99, LogicalOutputSource::Program)],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::UnknownOutput(output(99)))
        );
        assert_eq!(
            validate_logical_outputs(
                &[config(1, 40, LogicalOutputSource::Input(input(99)))],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::UnknownInput(input(99)))
        );
        assert_eq!(
            validate_logical_outputs(
                &[config(1, 40, LogicalOutputSource::Mix(0))],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::InvalidMix(0))
        );
        assert_eq!(
            validate_logical_outputs(
                &[
                    config(1, 40, LogicalOutputSource::Program),
                    config(2, 40, LogicalOutputSource::Program)
                ],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::DuplicateOutput(output(40)))
        );
        assert_eq!(
            validate_logical_outputs(
                &[config(
                    1,
                    40,
                    LogicalOutputSource::MultiView(vec![LogicalMultiViewCell::Output(slot(1))])
                )],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::SelfReference(slot(1)))
        );
        assert_eq!(
            validate_logical_outputs(
                &[
                    config(
                        1,
                        40,
                        LogicalOutputSource::MultiView(vec![LogicalMultiViewCell::Output(slot(2))])
                    ),
                    config(
                        2,
                        41,
                        LogicalOutputSource::MultiView(vec![LogicalMultiViewCell::Output(slot(1))])
                    ),
                ],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::Cycle)
        );
        assert_eq!(
            validate_logical_outputs(
                &[config(1, 40, LogicalOutputSource::MultiView(vec![]))],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::EmptyMultiView)
        );
        assert_eq!(
            validate_logical_outputs(
                &[config(
                    1,
                    40,
                    LogicalOutputSource::MultiView(vec![
                        LogicalMultiViewCell::Program,
                        LogicalMultiViewCell::Program,
                    ])
                )],
                &outputs,
                &inputs,
                None
            ),
            Err(LogicalOutputError::DuplicateMultiViewCell(1))
        );
    }

    #[test]
    fn overlay_channels_are_bounded() {
        assert_eq!(
            LogicalOverlayChannels::from_channels([1, 8]).map(|set| set.iter().collect::<Vec<_>>()),
            Ok(vec![1, 8])
        );
        assert_eq!(
            LogicalOverlayChannels::from_channels([0]),
            Err(LogicalOutputError::InvalidOverlayChannel(0))
        );
        assert_eq!(
            LogicalOverlayChannels::from_channels([9]),
            Err(LogicalOutputError::InvalidOverlayChannel(9))
        );
        assert_eq!(
            LogicalOverlayChannels::from_channels([1, 1]),
            Err(LogicalOutputError::DuplicateOverlayChannel(1))
        );
    }
}
