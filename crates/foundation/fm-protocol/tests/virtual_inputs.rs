//! IN-024 / IN-025: alias and feed authoring over Protocol 2.162.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, LiveInputSource, WireInputId,
    WireLayerCrop, WireLayerEffect, WireMessage, decode_line, encode_line,
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
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
    });
    round_trip(LiveInputSource::Alias {
        target: input(2),
        crop: Some(WireLayerCrop {
            x: 10,
            y: 20,
            width: 640,
            height: 360,
        }),
        effects: vec![WireLayerEffect {
            name: "saturation".into(),
            parameters: vec![-25],
        }],
        opacity: u8::MAX,
        mask: None,
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
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
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
fn an_alias_crop_past_the_source_bound_is_refused() {
    let line = encode_line(&command(LiveInputSource::Alias {
        target: input(1),
        crop: Some(WireLayerCrop {
            x: 1,
            y: 0,
            width: 3,
            height: 4,
        }),
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
    }))
    .unwrap();
    let past = line.replace("crop_width=3", "crop_width=16384");
    assert!(decode_line(&past).is_err(), "{past}");

    // The encoder refuses an invalid crop instead of emitting it.
    let invalid = command(LiveInputSource::Alias {
        target: input(1),
        crop: Some(WireLayerCrop {
            x: 1,
            y: 0,
            width: 16_384,
            height: 1,
        }),
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
    });
    assert!(encode_line(&invalid).is_err());

    // The largest valid crop still round-trips.
    round_trip(LiveInputSource::Alias {
        target: input(1),
        crop: Some(WireLayerCrop {
            x: 0,
            y: 0,
            width: 16_384,
            height: 16_384,
        }),
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
    });
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
        effects: Vec::new(),
        opacity: u8::MAX,
        mask: None,
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

#[test]
fn alias_effects_round_trip_and_are_bounded() {
    let effects = vec![
        WireLayerEffect {
            name: "levels".into(),
            parameters: vec![16, 235],
        },
        WireLayerEffect {
            name: "flip".into(),
            parameters: vec![1, 0],
        },
    ];
    round_trip(LiveInputSource::Alias {
        target: input(1),
        crop: None,
        effects: effects.clone(),
        opacity: u8::MAX,
        mask: None,
    });

    let invalid = command(LiveInputSource::Alias {
        target: input(1),
        crop: None,
        effects: vec![WireLayerEffect {
            name: "posterize".into(),
            parameters: vec![4],
        }],
        opacity: u8::MAX,
        mask: None,
    });
    assert!(encode_line(&invalid).is_err());

    let too_many = command(LiveInputSource::Alias {
        target: input(1),
        crop: None,
        effects: vec![
            WireLayerEffect {
                name: "saturation".into(),
                parameters: vec![0],
            };
            fm_protocol::MAX_SCENE_LAYER_EFFECTS + 1
        ],
        opacity: u8::MAX,
        mask: None,
    });
    assert!(encode_line(&too_many).is_err());
}
