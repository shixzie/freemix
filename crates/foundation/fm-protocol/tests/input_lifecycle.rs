//! Protocol 2.38 input lifecycle (IN-026): the lifecycle commands, the
//! `input_lifecycle_changed` durable event, the `input_availability`
//! runtime event, and the snapshot's per-input lifecycle and category
//! list — exactly, and strictly.

use fm_protocol::{
    AddedInput, CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, ImportItemSpec, ImportRefusal,
    InputCategoryStatus, InputColorLabel, InputLifecycleOperation, InputOfflineReason,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, WireGeneratorVideo, WireInputId,
    WireInputSource, WireMessage, decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.87\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn command(operation: InputLifecycleOperation) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::InputLifecycle(operation),
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn categories() -> Vec<InputCategoryStatus> {
    vec![
        InputCategoryStatus {
            id: 1,
            name: "Cameras".into(),
        },
        InputCategoryStatus {
            id: 7,
            name: "Clips, b~roll".into(),
        },
    ]
}

fn operations() -> Vec<InputLifecycleOperation> {
    vec![
        InputLifecycleOperation::Duplicate {
            source: input(1),
            input: input(9),
            name: Some("Camera copy".into()),
        },
        InputLifecycleOperation::Duplicate {
            source: input(1),
            input: input(9),
            name: None,
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::Generator {
                video: WireGeneratorVideo::Solid {
                    rgba: [255, 16, 0, 128],
                },
                tone_hz: Some(440),
            },
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::Generator {
                video: WireGeneratorVideo::Bars,
                tone_hz: None,
            },
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::Media {
                uri: "asset://clips/a b:c.mov".into(),
            },
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::Device {
                stable_key: "usb:cam~1".into(),
            },
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::Network {
                endpoint: "srt://10.0.0.2:9000".into(),
            },
        },
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::SystemAudio(fm_protocol::SystemAudioConfig::new(
                fm_protocol::SystemAudioBackend::Pulse,
                "alsa_output.pci:analog-stereo.monitor",
            )),
        },
        InputLifecycleOperation::SetCategory {
            input: input(3),
            category: Some(7),
        },
        InputLifecycleOperation::SetCategory {
            input: input(3),
            category: None,
        },
        InputLifecycleOperation::SetColor {
            input: input(3),
            color: Some(InputColorLabel::Teal),
        },
        InputLifecycleOperation::SetColor {
            input: input(3),
            color: None,
        },
        InputLifecycleOperation::SetPaused {
            input: input(3),
            paused: true,
        },
        InputLifecycleOperation::SetClosed {
            input: input(3),
            closed: false,
        },
        InputLifecycleOperation::SetCategories {
            categories: categories(),
        },
        InputLifecycleOperation::SetCategories {
            categories: Vec::new(),
        },
        InputLifecycleOperation::Import {
            items: vec![
                ImportItemSpec {
                    input: input(20),
                    name: None,
                    uri: "/media/Interview Take.mov".into(),
                },
                ImportItemSpec {
                    input: input(21),
                    name: Some("-".into()),
                    uri: "asset://b~roll.mp4".into(),
                },
            ],
        },
        InputLifecycleOperation::RemoveInputs {
            inputs: vec![input(20), input(21)],
        },
    ]
}

#[test]
fn every_lifecycle_command_round_trips_under_its_payload_name() {
    for operation in operations() {
        let payload = operation.payload();
        let line = round_trip(&command(operation));
        assert!(
            line.starts_with(&format!("{PREFIX}\tpayload={payload}\t")),
            "{line}"
        );
    }
}

#[test]
fn lifecycle_commands_have_an_exact_wire_form() {
    let cases = [
        (
            InputLifecycleOperation::SetColor {
                input: input(3),
                color: Some(InputColorLabel::Teal),
            },
            "payload=input_color_set\tinput=3\tcolor=teal\n",
        ),
        (
            InputLifecycleOperation::SetCategory {
                input: input(3),
                category: None,
            },
            "payload=input_category_set\tinput=3\tcategory=-\n",
        ),
        (
            InputLifecycleOperation::SetPaused {
                input: input(3),
                paused: true,
            },
            "payload=input_pause_set\tinput=3\tpaused=true\n",
        ),
        (
            InputLifecycleOperation::RemoveInputs {
                inputs: vec![input(20), input(21)],
            },
            "payload=inputs_remove\tinputs=20%2C21\n",
        ),
    ];
    for (operation, tail) in cases {
        let line = round_trip(&command(operation));
        assert_eq!(line, format!("{PREFIX}\t{tail}"));
    }
}

#[test]
fn lifecycle_commands_are_strict() {
    // Category 0 is not a category; the colour palette is closed; a
    // generator colour is eight hex digits; batches are bounded.
    for (payload, fields) in [
        ("input_category_set", "input=3\tcategory=0"),
        ("input_color_set", "input=3\tcolor=magenta"),
        ("input_color_set", "input=3\tcolor=Teal"),
        ("input_pause_set", "input=3\tpaused=yes"),
        ("input_close_set", "input=0\tclosed=true"),
        ("input_replace_source", "input=2\tsource=solid%3Aff00"),
        ("input_replace_source", "input=2\tsource=hologram%3Ax"),
        ("input_replace_source", "input=2\tsource=media%3A"),
        ("input_duplicate", "source=1\tinput=1"),
        ("inputs_remove", "inputs="),
        ("input_categories_set", "categories=0~Zero"),
        ("input_categories_set", "categories=1~A%2C1~B"),
    ] {
        let line = format!("{PREFIX}\tpayload={payload}\t{fields}\n");
        assert!(decode_line(&line).is_err(), "{line}");
    }
    let too_many = InputLifecycleOperation::Import {
        items: (1..=65)
            .map(|id| ImportItemSpec {
                input: input(id),
                name: None,
                uri: format!("asset://{id}.mov"),
            })
            .collect(),
    };
    assert!(encode_line(&command(too_many)).is_err());
    let blank_category = InputLifecycleOperation::SetCategories {
        categories: vec![InputCategoryStatus {
            id: 1,
            name: " ".into(),
        }],
    };
    assert!(encode_line(&command(blank_category)).is_err());
}

#[test]
fn the_lifecycle_event_carries_the_operation_added_inputs_and_refusals() {
    let event = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload: EventPayload::InputLifecycleChanged {
            operation: operations()[16].clone(),
            added: vec![AddedInput {
                input: input(20),
                name: "Interview Take".into(),
            }],
            refused: vec![(1, ImportRefusal::DuplicateName)],
        },
    });
    let line = round_trip(&event);
    assert!(line.contains("\tevent=input_lifecycle_changed\t"), "{line}");
    assert!(line.contains("\toperation=input_import_batch\t"), "{line}");
    for refusal in ImportRefusal::ALL {
        let EventPayload::InputLifecycleChanged { operation, .. } = (match &event {
            WireMessage::Event(message) => message.payload.clone(),
            _ => unreachable!(),
        }) else {
            unreachable!()
        };
        round_trip(&WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "e".into(),
                    state_epoch: 1,
                    log_id: "l".into(),
                },
                revision: 4,
            },
            edit_history: EditHistoryStatus::EMPTY,
            payload: EventPayload::InputLifecycleChanged {
                operation,
                added: Vec::new(),
                refused: vec![(0, refusal)],
            },
        }));
    }
}

#[test]
fn availability_runtime_events_round_trip_every_reason() {
    let server = ServerIdentity {
        engine_id: "e".into(),
        project_id: "1".into(),
        state_epoch: 1,
        log_id: "l".into(),
    };
    for offline in InputOfflineReason::ALL
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None))
    {
        let line = round_trip(&WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server.clone(),
            revision: 3,
            generation: 2,
            sequence: 1,
            event: RuntimeLifecycleEvent::InputAvailability {
                input: input(5),
                offline,
            },
        }));
        let token = offline.map_or("-", InputOfflineReason::token);
        assert!(
            line.ends_with(&format!(
                "\tevent=input_availability\tinput=5\toffline={token}\n"
            )),
            "{line}"
        );
    }
}
