//! SW-016: input freeze commands round-trip at Protocol 2.161.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, WireInputId, WireMessage,
    decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn round_trip(payload: CommandPayload) {
    let message = WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:freeze".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(7),
        deadline_ms: Some(500),
        payload,
    });
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn freeze_commands_round_trip() {
    round_trip(CommandPayload::SetInputFreeze {
        input: input(1),
        frozen: true,
    });
    round_trip(CommandPayload::SetInputFreeze {
        input: input(2),
        frozen: false,
    });
}
