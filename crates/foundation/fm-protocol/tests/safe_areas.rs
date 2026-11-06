//! SW-014: operator safe-area guides round-trip at Protocol 2.172.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, WireMessage, WireSafeAreaGuides, decode_line,
    encode_line,
};

fn round_trip(payload: CommandPayload) {
    let message = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:test".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(1_842),
        deadline_ms: Some(500),
        payload,
    });
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn set_safe_area_guides_round_trips_every_switch() {
    round_trip(CommandPayload::SetSafeAreaGuides {
        guides: WireSafeAreaGuides {
            action: true,
            title: true,
            center_4_3: false,
            center_16_9: true,
            center_9_16: true,
        },
    });
    round_trip(CommandPayload::SetSafeAreaGuides {
        guides: WireSafeAreaGuides::NONE,
    });
}

#[test]
fn changed_event_round_trips_the_selection() {
    let guides = WireSafeAreaGuides {
        action: true,
        title: false,
        center_4_3: true,
        center_16_9: false,
        center_9_16: true,
    };
    let message = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine".into(),
                state_epoch: 1,
                log_id: "log".into(),
            },
            revision: 2,
        },
        payload: EventPayload::SafeAreaGuidesChanged { guides },
        edit_history: EditHistoryStatus::EMPTY,
    });
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn the_wire_token_list_is_canonical_and_strict() {
    let all = WireSafeAreaGuides {
        action: true,
        title: true,
        center_4_3: true,
        center_16_9: true,
        center_9_16: true,
    };
    assert_eq!(all.tokens(), "action,title,4:3,16:9,9:16");
    assert_eq!(WireSafeAreaGuides::from_tokens(&all.tokens()), Some(all));
    assert_eq!(WireSafeAreaGuides::default().tokens(), "title");
    assert_eq!(WireSafeAreaGuides::NONE.tokens(), "-");
    for invalid in ["", "title,title", "safe", "16:8"] {
        assert_eq!(WireSafeAreaGuides::from_tokens(invalid), None, "{invalid}");
    }
}
