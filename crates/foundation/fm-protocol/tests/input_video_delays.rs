//! IN-022: a video-delay event carries its aligned audio strip delay
//! (Protocol 2.163).

use core::num::NonZeroU128;

use fm_protocol::{
    EngineIdentity, EventCursor, EventMessage, EventPayload, WireInputId, WireMessage, decode_line,
    encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> EngineIdentity {
    EngineIdentity {
        engine_id: "engine".into(),
        state_epoch: 1,
        log_id: "log".into(),
    }
}

#[test]
fn a_video_delay_change_carries_its_audio_alignment() {
    let message = WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: EventCursor {
            engine: engine(),
            revision: 5,
        },
        payload: EventPayload::InputVideoDelayChanged {
            input: input(1),
            frames: 5,
            audio_delay_samples: 8_000,
        },
    });
    let line = encode_line(&message).unwrap();
    assert!(
        line.contains("\tframes=5\taudio_delay_samples=8000"),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), message);

    // The alignment is part of the contract, not an optional extra.
    let damaged = line.replace("\taudio_delay_samples=8000", "");
    assert!(decode_line(&damaged).is_err(), "{damaged}");
}
