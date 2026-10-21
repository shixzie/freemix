//! Protocol 2.42 fullscreen display outputs (OR-011): the
//! `set_display_output` / `clear_display_output` commands, the
//! `display_output_changed` durable event, the `display_output_status` and
//! `display_inventory` runtime events, the `display_inventory_request` /
//! `display_inventory` query, and the snapshot fields — exactly, and
//! strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, DisplayId, DisplayInventory,
    DisplayInventoryEntry, DisplayInventoryMessage, DisplayInventoryRequest, DisplayMode,
    DisplayOutputConfig, DisplayOutputSlot, DisplayOutputStatus, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, HdrCapability, HdrState, ProtocolVersion,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, WaitReason, WireActiveDisplay,
    WireDisplayOutputState, WireMessage, decode_line, encode_line,
};

fn slot(value: u8) -> DisplayOutputSlot {
    DisplayOutputSlot::new(value).unwrap()
}

fn config(spec: &str) -> DisplayOutputConfig {
    spec.parse().unwrap()
}

fn command(payload: CommandPayload) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

fn engine() -> EngineIdentity {
    EngineIdentity {
        engine_id: "e".into(),
        state_epoch: 1,
        log_id: "l".into(),
    }
}

fn runtime(event: RuntimeLifecycleEvent) -> WireMessage {
    WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "p".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 4,
        generation: 1,
        sequence: 9,
        event,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn inventory() -> DisplayInventory {
    DisplayInventory {
        presenter: true,
        error: Some("one display, odd ~ name".into()),
        displays: vec![
            DisplayInventoryEntry {
                id: DisplayId::new("edid:1~a").unwrap(),
                name: "Studio Left".into(),
                x: -1920,
                y: 0,
                current_mode: DisplayMode::new(1920, 1080, 59_940),
                modes: vec![
                    DisplayMode::new(3840, 2160, 60_000),
                    DisplayMode::new(1920, 1080, 59_940),
                ],
                hdr: HdrCapability::Hdr,
            },
            DisplayInventoryEntry {
                id: DisplayId::new("edid:2").unwrap(),
                name: "Wall".into(),
                x: 0,
                y: 0,
                current_mode: DisplayMode::new(1280, 720, 50_000),
                modes: Vec::new(),
                hdr: HdrCapability::Unknown,
            },
        ],
    }
}

#[test]
fn the_current_protocol_version_is_pinned() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 101));
}

#[test]
fn commands_have_an_exact_wire_form_and_are_strict() {
    let set = command(CommandPayload::SetDisplayOutput {
        slot: slot(2),
        config: Box::new(config(
            "display=id:cg:1,source=output:41,mode=1920x1080@59940,scaling=1:1,hdr=auto",
        )),
    });
    let line = round_trip(&set);
    let prefix = "command\tprotocol=2.101\tid=a\tidempotency_key=k";
    assert_eq!(
        line,
        format!(
            "{prefix}\tpayload=set_display_output\tslot=2\tdisplay=id%3Acg%3A1\tsource=output%3A41\tmode=1920x1080%4059940\tscaling=1%3A1\thdr=auto\n"
        )
    );
    let clear = command(CommandPayload::ClearDisplayOutput { slot: slot(1) });
    assert_eq!(
        round_trip(&clear),
        format!("{prefix}\tpayload=clear_display_output\tslot=1\n")
    );
    for config in [
        "display=name:Studio Left,source=preview",
        "display=index:3,mode=current,scaling=fill,hdr=off",
    ] {
        round_trip(&command(CommandPayload::SetDisplayOutput {
            slot: slot(1),
            config: Box::new(self::config(config)),
        }));
    }
    for (from, to) in [
        ("slot=2", "slot=3"),
        ("slot=2", "slot=0"),
        ("slot=2", "slot=02"),
        ("source=output%3A41", "source=output%3A0"),
        ("source=output%3A41", "source=multiview"),
        ("mode=1920x1080%4059940", "mode=1920x1080"),
        ("mode=1920x1080%4059940", "mode=0x1080%4059940"),
        ("scaling=1%3A1", "scaling=zoom"),
        ("hdr=auto", "hdr=on"),
        ("\thdr=auto", ""),
        ("display=id%3Acg%3A1", "display=index%3A1x"),
        ("display=id%3Acg%3A1", "display="),
    ] {
        let malformed = line.replacen(from, to, 1);
        assert_ne!(malformed, line, "{from}");
        refused(&malformed);
    }
    refused(&format!(
        "{prefix}\tpayload=clear_display_output\tslot=1\textra=1\n"
    ));
}

#[test]
fn the_durable_event_carries_the_slot_and_its_configuration() {
    for config in [
        Some(Box::new(config(
            "display=index:0,source=preview,scaling=stretch",
        ))),
        None,
    ] {
        round_trip(&WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision: 4,
            },
            edit_history: EditHistoryStatus::EMPTY,
            payload: EventPayload::DisplayOutputChanged {
                slot: slot(2),
                config,
            },
        }));
    }
}

#[test]
fn runtime_status_and_inventory_round_trip_and_are_bounded() {
    let active = runtime(RuntimeLifecycleEvent::DisplayOutputStatus(
        DisplayOutputStatus {
            slot: slot(1),
            state: WireDisplayOutputState::Active(WireActiveDisplay {
                id: DisplayId::new("edid:1").unwrap(),
                name: "Studio, Left".replace(',', " "),
                mode: DisplayMode::new(1920, 1080, 59_940),
                mode_fallback: true,
                hdr: HdrState::SdrPresenterLimited,
            }),
        },
    ));
    round_trip(&active);
    for reason in [
        WaitReason::DisplayAbsent,
        WaitReason::DisplayInUse,
        WaitReason::AmbiguousName,
        WaitReason::EnumerationFailed,
        WaitReason::NoPresenter,
    ] {
        round_trip(&runtime(RuntimeLifecycleEvent::DisplayOutputStatus(
            DisplayOutputStatus {
                slot: slot(2),
                state: WireDisplayOutputState::Waiting(reason),
            },
        )));
    }
    round_trip(&runtime(RuntimeLifecycleEvent::DisplayInventory(
        inventory(),
    )));
    round_trip(&runtime(RuntimeLifecycleEvent::DisplayInventory(
        DisplayInventory::default(),
    )));

    // The query answers by request id.
    round_trip(&WireMessage::DisplayInventoryRequest(
        DisplayInventoryRequest {
            request_id: "studio-displays-1".into(),
        },
    ));
    round_trip(&WireMessage::DisplayInventory(DisplayInventoryMessage {
        request_id: "studio-displays-1".into(),
        inventory: inventory(),
    }));

    // Limits: at most 16 displays, 128 modes each, and a 256-byte error.
    let entry = inventory().displays[1].clone();
    let mut many = inventory();
    many.displays = (0..17)
        .map(|index| DisplayInventoryEntry {
            id: DisplayId::new(format!("d{index}")).unwrap(),
            ..entry.clone()
        })
        .collect();
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::DisplayInventory(many))).is_err());
    let mut modes = inventory();
    modes.displays[1].modes = (0..129)
        .map(|index| DisplayMode::new(640 + index, 480, 60_000))
        .collect();
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::DisplayInventory(modes))).is_err());
    let mut error = inventory();
    error.error = Some("x".repeat(257));
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::DisplayInventory(error))).is_err());
}
