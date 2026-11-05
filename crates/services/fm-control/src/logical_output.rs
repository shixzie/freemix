//! Logical output routing between the engine and the wire (SW-012).

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, EventPayload, WireLogicalAudioMap, WireLogicalOutputCell,
    WireLogicalOutputConfig, WireLogicalOutputOverlays, WireLogicalOutputSlot,
    WireLogicalOutputSource, WireOverlayChannelId,
};

/// The engine command of a logical-output payload.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    match payload {
        CommandPayload::SetLogicalOutput { slot, config } => {
            Some(EngineCommand::SetLogicalOutput {
                slot: model_slot(*slot),
                config: Some(Box::new(model_config(config))),
            })
        }
        CommandPayload::ClearLogicalOutput { slot } => Some(EngineCommand::SetLogicalOutput {
            slot: model_slot(*slot),
            config: None,
        }),
        _ => None,
    }
}

/// The durable event of a logical-output edit, or of its undo or redo.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::LogicalOutputChanged { slot, config } = event else {
        return None;
    };
    Some(EventPayload::LogicalOutputChanged {
        slot: wire_slot(*slot),
        config: config.as_ref().map(|config| Box::new(wire_config(config))),
    })
}

/// The wire command of an undoable logical-output edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetLogicalOutput { slot, config } = command else {
        return None;
    };
    Some(match config {
        Some(config) => CommandPayload::SetLogicalOutput {
            slot: wire_slot(*slot),
            config: Box::new(wire_config(config)),
        },
        None => CommandPayload::ClearLogicalOutput {
            slot: wire_slot(*slot),
        },
    })
}

/// Every configured logical output in slot order.
pub(super) fn wire_entries(show: &ShowState) -> Vec<WireLogicalOutputConfig> {
    show.logical_outputs().map(wire_config).collect()
}

fn model_slot(slot: WireLogicalOutputSlot) -> fm_model::LogicalOutputSlot {
    fm_model::LogicalOutputSlot::new(slot.number()).expect("a validated wire slot")
}

fn wire_slot(slot: fm_model::LogicalOutputSlot) -> WireLogicalOutputSlot {
    WireLogicalOutputSlot::new(slot.number()).expect("a model slot is in range")
}

fn model_config(config: &WireLogicalOutputConfig) -> fm_model::LogicalOutputConfig {
    fm_model::LogicalOutputConfig {
        slot: model_slot(config.slot),
        output: config.output.to_domain(),
        source: model_source(&config.source),
        overlays: model_overlays(&config.overlays),
        audio: model_audio(config.audio),
    }
}

fn wire_config(config: &fm_model::LogicalOutputConfig) -> WireLogicalOutputConfig {
    WireLogicalOutputConfig {
        slot: wire_slot(config.slot),
        output: fm_protocol::WireOutputId::from_domain(config.output),
        source: wire_source(&config.source),
        overlays: wire_overlays(&config.overlays),
        audio: wire_audio(config.audio),
    }
}

fn model_source(source: &WireLogicalOutputSource) -> fm_model::LogicalOutputSource {
    match source {
        WireLogicalOutputSource::Program => fm_model::LogicalOutputSource::Program,
        WireLogicalOutputSource::Preview => fm_model::LogicalOutputSource::Preview,
        WireLogicalOutputSource::Input(input) => {
            fm_model::LogicalOutputSource::Input(input.to_domain())
        }
        WireLogicalOutputSource::Mix(mix) => fm_model::LogicalOutputSource::Mix(*mix),
        WireLogicalOutputSource::MultiView(cells) => {
            fm_model::LogicalOutputSource::MultiView(cells.iter().map(model_cell).collect())
        }
    }
}

fn wire_source(source: &fm_model::LogicalOutputSource) -> WireLogicalOutputSource {
    match source {
        fm_model::LogicalOutputSource::Program => WireLogicalOutputSource::Program,
        fm_model::LogicalOutputSource::Preview => WireLogicalOutputSource::Preview,
        fm_model::LogicalOutputSource::Input(input) => {
            WireLogicalOutputSource::Input(fm_protocol::WireInputId::from_domain(*input))
        }
        fm_model::LogicalOutputSource::Mix(mix) => WireLogicalOutputSource::Mix(*mix),
        fm_model::LogicalOutputSource::MultiView(cells) => {
            WireLogicalOutputSource::MultiView(cells.iter().map(wire_cell).collect())
        }
    }
}

fn model_cell(cell: &WireLogicalOutputCell) -> fm_model::LogicalMultiViewCell {
    match cell {
        WireLogicalOutputCell::Program => fm_model::LogicalMultiViewCell::Program,
        WireLogicalOutputCell::Preview => fm_model::LogicalMultiViewCell::Preview,
        WireLogicalOutputCell::Input(input) => {
            fm_model::LogicalMultiViewCell::Input(input.to_domain())
        }
        WireLogicalOutputCell::Mix(mix) => fm_model::LogicalMultiViewCell::Mix(*mix),
        WireLogicalOutputCell::Output(slot) => {
            fm_model::LogicalMultiViewCell::Output(model_slot(*slot))
        }
    }
}

fn wire_cell(cell: &fm_model::LogicalMultiViewCell) -> WireLogicalOutputCell {
    match cell {
        fm_model::LogicalMultiViewCell::Program => WireLogicalOutputCell::Program,
        fm_model::LogicalMultiViewCell::Preview => WireLogicalOutputCell::Preview,
        fm_model::LogicalMultiViewCell::Input(input) => {
            WireLogicalOutputCell::Input(fm_protocol::WireInputId::from_domain(*input))
        }
        fm_model::LogicalMultiViewCell::Mix(mix) => WireLogicalOutputCell::Mix(*mix),
        fm_model::LogicalMultiViewCell::Output(slot) => {
            WireLogicalOutputCell::Output(wire_slot(*slot))
        }
    }
}

fn model_overlays(overlays: &WireLogicalOutputOverlays) -> fm_model::LogicalOverlayPolicy {
    match overlays {
        WireLogicalOutputOverlays::All => fm_model::LogicalOverlayPolicy::All,
        WireLogicalOutputOverlays::Clean => fm_model::LogicalOverlayPolicy::Clean,
        WireLogicalOutputOverlays::Routed => fm_model::LogicalOverlayPolicy::Routed,
        WireLogicalOutputOverlays::Channels(channels) => fm_model::LogicalOverlayPolicy::Channels(
            fm_model::LogicalOverlayChannels::from_channels(
                channels.iter().map(|channel| channel.number()),
            )
            .expect("a validated wire overlay set"),
        ),
    }
}

fn wire_overlays(overlays: &fm_model::LogicalOverlayPolicy) -> WireLogicalOutputOverlays {
    match overlays {
        fm_model::LogicalOverlayPolicy::All => WireLogicalOutputOverlays::All,
        fm_model::LogicalOverlayPolicy::Clean => WireLogicalOutputOverlays::Clean,
        fm_model::LogicalOverlayPolicy::Routed => WireLogicalOutputOverlays::Routed,
        fm_model::LogicalOverlayPolicy::Channels(channels) => WireLogicalOutputOverlays::Channels(
            channels
                .iter()
                .map(|channel| {
                    WireOverlayChannelId::new(channel).expect("a model channel is in range")
                })
                .collect(),
        ),
    }
}

fn model_audio(audio: WireLogicalAudioMap) -> fm_model::LogicalAudioMap {
    match audio {
        WireLogicalAudioMap::Master => fm_model::LogicalAudioMap::Master,
        WireLogicalAudioMap::Bus(bus) => fm_model::LogicalAudioMap::Bus(bus),
        WireLogicalAudioMap::FollowVideo => fm_model::LogicalAudioMap::FollowVideo,
    }
}

fn wire_audio(audio: fm_model::LogicalAudioMap) -> WireLogicalAudioMap {
    match audio {
        fm_model::LogicalAudioMap::Master => WireLogicalAudioMap::Master,
        fm_model::LogicalAudioMap::Bus(bus) => WireLogicalAudioMap::Bus(bus),
        fm_model::LogicalAudioMap::FollowVideo => WireLogicalAudioMap::FollowVideo,
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn config() -> WireLogicalOutputConfig {
        WireLogicalOutputConfig {
            slot: WireLogicalOutputSlot::new(2).unwrap(),
            output: fm_protocol::WireOutputId::new(NonZeroU128::new(8).unwrap()),
            source: WireLogicalOutputSource::MultiView(vec![
                WireLogicalOutputCell::Program,
                WireLogicalOutputCell::Input(fm_protocol::WireInputId::new(
                    NonZeroU128::new(3).unwrap(),
                )),
                WireLogicalOutputCell::Output(WireLogicalOutputSlot::new(1).unwrap()),
            ]),
            overlays: WireLogicalOutputOverlays::Channels(vec![
                WireOverlayChannelId::new(1).unwrap(),
                WireOverlayChannelId::new(3).unwrap(),
            ]),
            audio: WireLogicalAudioMap::Bus(fm_types::BusId::new(NonZeroU128::new(7).unwrap())),
        }
    }

    #[test]
    fn a_wire_config_round_trips_through_the_model() {
        let wire = config();
        assert_eq!(wire_config(&model_config(&wire)), wire);
    }

    #[test]
    fn set_and_clear_commands_round_trip_through_the_engine() {
        for payload in [
            CommandPayload::SetLogicalOutput {
                slot: config().slot,
                config: Box::new(config()),
            },
            CommandPayload::ClearLogicalOutput {
                slot: config().slot,
            },
        ] {
            let command = engine_command(&payload).unwrap();
            assert_eq!(wire_edit_payload(&command), Some(payload));
        }
    }

    #[test]
    fn snapshot_entries_are_in_slot_order() {
        use fm_engine::ShowState;
        use fm_types::{InputId, OutputId};

        let show = ShowState::new(
            "logical",
            vec![
                (InputId::new(NonZeroU128::new(1).unwrap()), "A".to_owned()),
                (InputId::new(NonZeroU128::new(2).unwrap()), "B".to_owned()),
                (InputId::new(NonZeroU128::new(3).unwrap()), "C".to_owned()),
            ],
            InputId::new(NonZeroU128::new(1).unwrap()),
            InputId::new(NonZeroU128::new(2).unwrap()),
        )
        .unwrap()
        .with_outputs(vec![
            (
                OutputId::new(NonZeroU128::new(7).unwrap()),
                "One".to_owned(),
            ),
            (
                OutputId::new(NonZeroU128::new(8).unwrap()),
                "Two".to_owned(),
            ),
        ])
        .unwrap();
        let mut show = show;
        let first = WireLogicalOutputConfig {
            slot: WireLogicalOutputSlot::new(1).unwrap(),
            source: WireLogicalOutputSource::Program,
            output: fm_protocol::WireOutputId::new(NonZeroU128::new(7).unwrap()),
            overlays: WireLogicalOutputOverlays::All,
            audio: WireLogicalAudioMap::Master,
        };
        show.set_logical_output(
            fm_model::LogicalOutputSlot::new(1).unwrap(),
            Some(model_config(&first)),
        )
        .unwrap();
        let second = model_config(&config());
        show.set_logical_output(second.slot, Some(second)).unwrap();
        assert_eq!(wire_entries(&show), vec![first, config()]);
    }

    #[test]
    fn a_changed_event_maps_to_the_wire() {
        let model = model_config(&config());
        let event = EngineEvent::LogicalOutputChanged {
            slot: model.slot,
            config: Some(Box::new(model)),
        };
        assert_eq!(
            changed(&event),
            Some(EventPayload::LogicalOutputChanged {
                slot: config().slot,
                config: Some(Box::new(config())),
            })
        );
    }
}
