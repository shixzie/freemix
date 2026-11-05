//! Resolving the show's logical outputs into one frame's render plans
//! (SW-012).

use fm_model::{
    LogicalAudioMap, LogicalMultiViewCell, LogicalOutputConfig, LogicalOutputSource,
    LogicalOverlayPolicy,
};
use fm_switcher::{
    AudioMap, LogicalOutputSlot, LogicalOutputs, MultiViewCell, OutputRoutingFrame, OutputSource,
    OverlayPolicy, OverlaySet, RoutingContext,
};

use crate::ShowState;

/// Resolves every enabled logical output against the show's desired switcher
/// state. Secondary mixes are not carried by the engine yet, so a source that
/// names one resolves as unavailable; Program, Preview, inputs, multiviews,
/// overlays, and audio maps resolve normally.
pub(crate) fn frame(state: &ShowState) -> OutputRoutingFrame {
    let mut routing = LogicalOutputs::new();
    for config in state.logical_outputs() {
        let Some(slot) = LogicalOutputSlot::new(config.slot.number()) else {
            continue;
        };
        let _ = routing.configure(slot, Some(switcher_config(config)));
    }
    routing.resolve(&RoutingContext {
        switcher: state.desired_switcher(),
        mixes: None,
        buses: state.audio_buses(),
    })
}

fn switcher_config(config: &LogicalOutputConfig) -> fm_switcher::LogicalOutputConfig {
    fm_switcher::LogicalOutputConfig {
        output: config.output,
        source: switcher_source(&config.source),
        overlays: switcher_overlays(&config.overlays),
        audio: switcher_audio(config.audio),
    }
}

fn switcher_source(source: &LogicalOutputSource) -> OutputSource {
    match source {
        LogicalOutputSource::Program => OutputSource::Program,
        LogicalOutputSource::Preview => OutputSource::Preview,
        LogicalOutputSource::Input(input) => OutputSource::Input(*input),
        LogicalOutputSource::Mix(mix) => {
            OutputSource::Mix(fm_switcher::MixId::new(*mix).unwrap_or(fm_switcher::MixId::MAIN))
        }
        LogicalOutputSource::MultiView(cells) => {
            OutputSource::MultiView(cells.iter().map(switcher_cell).collect())
        }
    }
}

fn switcher_cell(cell: &LogicalMultiViewCell) -> MultiViewCell {
    match cell {
        LogicalMultiViewCell::Program => MultiViewCell::Program,
        LogicalMultiViewCell::Preview => MultiViewCell::Preview,
        LogicalMultiViewCell::Input(input) => MultiViewCell::Input(*input),
        LogicalMultiViewCell::Mix(mix) => {
            MultiViewCell::Mix(fm_switcher::MixId::new(*mix).unwrap_or(fm_switcher::MixId::MAIN))
        }
        LogicalMultiViewCell::Output(slot) => MultiViewCell::Output(
            LogicalOutputSlot::new(slot.number()).unwrap_or(LogicalOutputSlot::ALL[0]),
        ),
    }
}

fn switcher_overlays(policy: &LogicalOverlayPolicy) -> OverlayPolicy {
    match policy {
        LogicalOverlayPolicy::All => OverlayPolicy::All,
        LogicalOverlayPolicy::Clean => OverlayPolicy::Clean,
        LogicalOverlayPolicy::Routed => OverlayPolicy::Routed,
        LogicalOverlayPolicy::Channels(channels) => {
            OverlayPolicy::Channels(OverlaySet::from_channels(
                channels
                    .iter()
                    .filter_map(fm_switcher::OverlayChannelId::new),
            ))
        }
    }
}

fn switcher_audio(audio: LogicalAudioMap) -> AudioMap {
    match audio {
        LogicalAudioMap::Master => AudioMap::Master,
        LogicalAudioMap::Bus(bus) => AudioMap::Bus(bus),
        LogicalAudioMap::FollowVideo => AudioMap::FollowVideo,
    }
}
