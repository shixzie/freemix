//! Resolving the show's logical outputs into one frame's render plans
//! (SW-012) and the secondary-mix frame they read (IN-023).

use fm_model::{
    LogicalAudioMap, LogicalMultiViewCell, LogicalOutputConfig, LogicalOutputSource,
    LogicalOverlayPolicy, SecondaryMixSource,
};
use fm_switcher::{
    AudioMap, LogicalOutputSlot, LogicalOutputs, MixBankFrame, MixFrame, MixId, MixLayer,
    MultiViewCell, OVERLAY_CHANNEL_COUNT, OutputRoutingFrame, OutputSource, OverlayPolicy,
    OverlaySet, RoutingContext,
};

use crate::ShowState;

/// Resolves every enabled logical output against the show's desired switcher
/// state and the supplied secondary-mix frame (IN-023). A mix source
/// resolves from the project's static Program/Preview routing; live mix
/// transitions remain with the bank when it is wired to the frame loop.
pub(crate) fn frame(state: &ShowState, mixes: &MixBankFrame) -> OutputRoutingFrame {
    let mut routing = LogicalOutputs::new();
    for config in state.logical_outputs() {
        let Some(slot) = LogicalOutputSlot::new(config.slot.number()) else {
            continue;
        };
        let _ = routing.configure(slot, Some(switcher_config(config)));
    }
    routing.resolve(&RoutingContext {
        switcher: state.desired_switcher(),
        mixes: Some(mixes),
        buses: state.audio_buses(),
    })
}

/// The project's secondary mixes as one bank frame. Each mix carries its
/// configured Program and Preview source; no transition is running until the
/// live bank is wired.
pub(crate) fn mixes(state: &ShowState) -> MixBankFrame {
    MixBankFrame {
        frame: 0,
        mixes: state
            .secondary_mixes()
            .map(|config| MixFrame {
                mix: MixId::new(config.number.number()).expect("a validated mix number"),
                primary: mix_layer(config.program),
                secondary: None,
                transition_kind: None,
                mix_numerator: 0,
                mix_denominator: 1,
                preview: config.preview.map(mix_layer),
                overlays: [None; OVERLAY_CHANNEL_COUNT],
            })
            .collect(),
    }
}

fn mix_layer(source: SecondaryMixSource) -> MixLayer {
    match source {
        SecondaryMixSource::Input(input) => MixLayer::Input(input),
        SecondaryMixSource::Mix(number) => MixLayer::Mix {
            mix: MixId::new(number.number()).expect("a validated mix number"),
            frame: 0,
        },
    }
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
