//! IN-022: per-input video delay commands round-trip at Protocol 2.161.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES,
    WireInputId, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn command(payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:test".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(1_842),
        deadline_ms: Some(500),
        payload,
    }
}

fn round_trip(payload: CommandPayload) {
    let message = WireMessage::Command(command(payload));
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn video_delay_commands_round_trip() {
    round_trip(CommandPayload::SetInputVideoDelay {
        input: input(7),
        frames: 5,
    });
    round_trip(CommandPayload::SetInputVideoDelay {
        input: input(7),
        frames: 0,
    });
    round_trip(CommandPayload::SetInputVideoDelay {
        input: input(9),
        frames: MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES,
    });
}

#[test]
fn a_delay_above_the_wire_bound_is_refused() {
    let message = WireMessage::Command(command(CommandPayload::SetInputVideoDelay {
        input: input(7),
        frames: MAX_WIRE_INPUT_VIDEO_DELAY_FRAMES + 1,
    }));
    assert!(encode_line(&message).is_err());
}
