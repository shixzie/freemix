//! IN-024: alias effect edits and reads at Protocol 2.168.

use core::num::NonZeroU128;

use fm_protocol::{
    AliasEffectsRequest, AliasEffectsResponse, CURRENT_PROTOCOL_VERSION, CommandMessage,
    CommandPayload, EditHistoryStatus, EngineIdentity, EventCursor, EventMessage, EventPayload,
    WireInputId, WireLayerEffect, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn effects() -> Vec<WireLayerEffect> {
    vec![
        WireLayerEffect {
            name: "levels".into(),
            parameters: vec![16, 235],
        },
        WireLayerEffect {
            name: "saturation".into(),
            parameters: vec![-50],
        },
    ]
}

#[test]
fn alias_effect_edit_and_event_round_trip() {
    let command = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:fx".into(),
        idempotency_key: "operator-7:fx".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SetAliasEffects {
            input: input(4),
            effects: effects(),
        },
    });
    let line = encode_line(&command).unwrap();
    assert!(line.contains("payload=set_alias_effects"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), command);

    let event = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine".into(),
                state_epoch: 1,
                log_id: "log".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload: EventPayload::InputAliasEffectsChanged {
            input: input(4),
            effects: effects(),
        },
    });
    let line = encode_line(&event).unwrap();
    assert!(line.contains("event=input_alias_effects_changed"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), event);
}

#[test]
fn alias_effect_read_round_trips() {
    let request = WireMessage::AliasEffectsRequest(AliasEffectsRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "alias-fx-1".into(),
        input: input(4),
    });
    let line = encode_line(&request).unwrap();
    assert_eq!(decode_line(&line).unwrap(), request);

    let response = WireMessage::AliasEffectsResponse(AliasEffectsResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "alias-fx-1".into(),
        input: input(4),
        effects: effects(),
    });
    let line = encode_line(&response).unwrap();
    assert_eq!(decode_line(&line).unwrap(), response);
}

#[test]
fn invalid_alias_effects_are_refused() {
    let invalid = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:fx".into(),
        idempotency_key: "operator-7:fx".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SetAliasEffects {
            input: input(4),
            effects: vec![WireLayerEffect {
                name: "saturation".into(),
                parameters: vec![],
            }],
        },
    });
    assert!(encode_line(&invalid).is_err());

    let too_many = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:fx".into(),
        idempotency_key: "operator-7:fx".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SetAliasEffects {
            input: input(4),
            effects: vec![
                WireLayerEffect {
                    name: "saturation".into(),
                    parameters: vec![0],
                };
                fm_protocol::MAX_SCENE_LAYER_EFFECTS + 1
            ],
        },
    });
    assert!(encode_line(&too_many).is_err());
}
