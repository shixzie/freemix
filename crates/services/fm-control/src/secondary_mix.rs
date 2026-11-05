//! Secondary mixes between the engine and the wire (IN-023).

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{
    CommandPayload, EventPayload, WireMixNumber, WireSecondaryMixConfig, WireSecondaryMixMode,
    WireSecondaryMixSource, WireSecondaryMixState,
};

/// The engine command of a secondary-mix payload.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    match payload {
        CommandPayload::SetSecondaryMix { number, config } => {
            Some(EngineCommand::SetSecondaryMix {
                number: model_number(*number),
                config: Some(Box::new(model_config(config))),
            })
        }
        CommandPayload::ClearSecondaryMix { number } => Some(EngineCommand::SetSecondaryMix {
            number: model_number(*number),
            config: None,
        }),
        CommandPayload::SecondaryMixCut { number } => Some(EngineCommand::SecondaryMixCut {
            number: model_number(*number),
        }),
        CommandPayload::SecondaryMixTransition {
            number,
            kind,
            duration_frames,
        } => Some(EngineCommand::SecondaryMixTransition {
            number: model_number(*number),
            kind: crate::transition::domain_transition_kind(*kind),
            duration_frames: *duration_frames,
        }),
        _ => None,
    }
}

/// The durable event of a secondary-mix edit, or of its undo or redo.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::SecondaryMixChanged { number, config } = event else {
        return None;
    };
    Some(EventPayload::SecondaryMixChanged {
        number: wire_number(*number),
        config: config.as_ref().map(|config| Box::new(wire_config(config))),
    })
}

/// The wire command of an undoable secondary-mix edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetSecondaryMix { number, config } = command else {
        return None;
    };
    Some(match config {
        Some(config) => CommandPayload::SetSecondaryMix {
            number: wire_number(*number),
            config: Box::new(wire_config(config)),
        },
        None => CommandPayload::ClearSecondaryMix {
            number: wire_number(*number),
        },
    })
}

/// Every configured secondary mix in number order.
pub(super) fn wire_entries(show: &ShowState) -> Vec<WireSecondaryMixConfig> {
    show.secondary_mixes().map(wire_config).collect()
}

/// Every configured mix's live Program/Preview and transition progress.
pub(super) fn wire_states(frame: &fm_switcher::MixBankFrame) -> Vec<WireSecondaryMixState> {
    frame
        .mixes
        .iter()
        .filter(|mix| !mix.mix.is_main())
        .map(|mix| WireSecondaryMixState {
            number: wire_mix_id(mix.mix),
            program: wire_layer(mix.primary),
            preview: mix.preview.and_then(wire_layer),
            numerator: mix.mix_numerator,
            denominator: mix.mix_denominator,
        })
        .collect()
}

fn wire_mix_id(mix: fm_switcher::MixId) -> WireMixNumber {
    WireMixNumber::new(mix.number()).expect("a bank mix number is in range")
}

fn wire_layer(layer: fm_switcher::MixLayer) -> Option<WireSecondaryMixSource> {
    match layer {
        fm_switcher::MixLayer::Input(input) => Some(WireSecondaryMixSource::Input(
            fm_protocol::WireInputId::from_domain(input),
        )),
        fm_switcher::MixLayer::Mix { mix, .. } => {
            Some(WireSecondaryMixSource::Mix(wire_mix_id(mix)))
        }
        fm_switcher::MixLayer::Black => None,
    }
}

fn model_number(number: WireMixNumber) -> fm_model::MixNumber {
    fm_model::MixNumber::new(number.number()).expect("a validated wire mix number")
}

fn wire_number(number: fm_model::MixNumber) -> WireMixNumber {
    WireMixNumber::new(number.number()).expect("a model mix number is in range")
}

fn model_source(source: WireSecondaryMixSource) -> fm_model::SecondaryMixSource {
    match source {
        WireSecondaryMixSource::Input(input) => {
            fm_model::SecondaryMixSource::Input(input.to_domain())
        }
        WireSecondaryMixSource::Mix(number) => {
            fm_model::SecondaryMixSource::Mix(model_number(number))
        }
    }
}

fn wire_source(source: fm_model::SecondaryMixSource) -> WireSecondaryMixSource {
    match source {
        fm_model::SecondaryMixSource::Input(input) => {
            WireSecondaryMixSource::Input(fm_protocol::WireInputId::from_domain(input))
        }
        fm_model::SecondaryMixSource::Mix(number) => {
            WireSecondaryMixSource::Mix(wire_number(number))
        }
    }
}

fn model_config(config: &WireSecondaryMixConfig) -> fm_model::SecondaryMixConfig {
    fm_model::SecondaryMixConfig {
        number: model_number(config.number),
        name: config.name.clone(),
        mode: match config.mode {
            WireSecondaryMixMode::PreviewProgram => fm_model::SecondaryMixMode::PreviewProgram,
            WireSecondaryMixMode::OutputOnly => fm_model::SecondaryMixMode::OutputOnly,
        },
        program: model_source(config.program),
        preview: config.preview.map(model_source),
    }
}

fn wire_config(config: &fm_model::SecondaryMixConfig) -> WireSecondaryMixConfig {
    WireSecondaryMixConfig {
        number: wire_number(config.number),
        name: config.name.clone(),
        mode: match config.mode {
            fm_model::SecondaryMixMode::PreviewProgram => WireSecondaryMixMode::PreviewProgram,
            fm_model::SecondaryMixMode::OutputOnly => WireSecondaryMixMode::OutputOnly,
        },
        program: wire_source(config.program),
        preview: config.preview.map(wire_source),
    }
}
