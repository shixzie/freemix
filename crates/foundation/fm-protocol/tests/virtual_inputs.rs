//! IN-024 / IN-025: alias and feed authoring over Protocol 2.162.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, LiveInputSource, WireInputId,
    WireLayerCrop, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn command(source: LiveInputSource) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:virtual".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(7),
        deadline_ms: Some(500),
        payload: CommandPayload::AddInput {
            input: input(3),
            name: "Virtual".to_owned(),
            source,
        },
    })
}

fn round_trip(source: LiveInputSource) {
    let message = command(source);
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn alias_and_feed_add_commands_round_trip() {
    round_trip(LiveInputSource::Alias {
        target: input(1),
        crop: None,
    });
    round_trip(LiveInputSource::Alias {
        target: input(2),
        crop: Some(WireLayerCrop {
            x: 10,
            y: 20,
            width: 640,
            height: 360,
        }),
    });
    round_trip(LiveInputSource::ProgramFeed);
    round_trip(LiveInputSource::PreviewFeed);
}

#[test]
fn alias_tokens_are_canonical() {
    let line = encode_line(&command(LiveInputSource::Alias {
        target: input(1),
        crop: Some(WireLayerCrop {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        }),
    }))
    .unwrap();
    for field in [
        "kind=alias",
        "target=1",
        "crop_x=1",
        "crop_y=2",
        "crop_width=3",
        "crop_height=4",
    ] {
        assert!(line.contains(field), "{line}");
    }
    let feed = encode_line(&command(LiveInputSource::ProgramFeed)).unwrap();
    assert!(feed.contains("kind=program_feed"), "{feed}");
    let preview = encode_line(&command(LiveInputSource::PreviewFeed)).unwrap();
    assert!(preview.contains("kind=preview_feed"), "{preview}");
}

#[test]
fn an_incomplete_or_empty_alias_crop_is_refused() {
    let line = encode_line(&command(LiveInputSource::Alias {
        target: input(1),
        crop: Some(WireLayerCrop {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        }),
    }))
    .unwrap();
    for damaged in [
        line.replace("\tcrop_height=4", ""),
        line.replace("crop_width=3", "crop_width=0"),
        line.replace("crop_height=4", "crop_height=0"),
    ] {
        assert!(decode_line(&damaged).is_err(), "{damaged}");
    }
}
