//! Protocol 2.57 `LiveLAN` (OR-017): `live_lan_set` with its secret PIN edit,
//! the `live_lan_changed` durable event, the `live_lan_status` runtime
//! event, and the snapshot's `live_lan` and `live_lan_status` — exactly,
//! and strictly. No form ever carries a stored PIN, and a refused PIN is
//! never echoed.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, LiveLanEntry, LiveLanSource,
    LiveLanState, LiveLanStatus, ProtocolVersion, RuntimeEventMessage, RuntimeLifecycleEvent,
    SecretEdit, ServerIdentity, WireMessage, decode_line, encode_line,
};
use fm_types::OutputId;

const PREFIX: &str = "command\tprotocol=2.59\tid=a\tidempotency_key=k";

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

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload,
    })
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

fn output(value: u128) -> OutputId {
    OutputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn entry() -> LiveLanEntry {
    LiveLanEntry {
        enabled: true,
        source: LiveLanSource::Output(output(7)),
        max_viewers: 12,
        has_pin: false,
    }
}

#[test]
fn the_current_protocol_is_2_57() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 59));
}

#[test]
fn the_set_command_has_an_exact_wire_form_and_a_secret_pin_edit() {
    let keep = command(CommandPayload::SetLiveLan {
        config: entry(),
        pin: SecretEdit::Keep,
    });
    let line = round_trip(&keep);
    assert_eq!(
        line,
        format!("{PREFIX}\tpayload=live_lan_set\tenabled=1\tsource=output%3A7\tmax_viewers=12\n")
    );
    let set = command(CommandPayload::SetLiveLan {
        config: entry(),
        pin: SecretEdit::Set("024680".into()),
    });
    assert!(round_trip(&set).ends_with("\tmax_viewers=12\tpin=024680\n"));
    let clear = command(CommandPayload::SetLiveLan {
        config: LiveLanEntry {
            enabled: false,
            source: LiveLanSource::Program,
            max_viewers: 1,
            has_pin: false,
        },
        pin: SecretEdit::Clear,
    });
    assert!(round_trip(&clear).ends_with("\tenabled=0\tsource=program\tmax_viewers=1\tpin=\n"));

    for (from, to) in [
        ("\tenabled=1", ""),
        ("\tenabled=1", "\tenabled=true"),
        ("\tsource=output%3A7", ""),
        ("\tsource=output%3A7", "\tsource=preview"),
        ("\tsource=output%3A7", "\tsource=output%3A07"),
        ("\tsource=output%3A7", "\tsource=output%3A0"),
        ("\tmax_viewers=12", ""),
        ("\tmax_viewers=12", "\tmax_viewers=0"),
        ("\tmax_viewers=12", "\tmax_viewers=33"),
        ("\tmax_viewers=12", "\tmax_viewers=012"),
        ("\n", "\textra=1\n"),
    ] {
        refused(&line.replacen(from, to, 1));
    }
    // A PIN must be 4 to 12 digits, and the redaction marker is refused;
    // the refusal never carries the PIN.
    for pin in ["***", "123", "1234567890123", "12a4", "12%2034"] {
        let error = decode_line(&line.replacen('\n', &format!("\tpin={pin}\n"), 1)).unwrap_err();
        assert_eq!(
            error,
            CodecError::InvalidField {
                field: "pin",
                value: "***".into(),
            },
            "{pin}"
        );
    }
    for pin in ["***", "12a4"] {
        let bad = command(CommandPayload::SetLiveLan {
            config: entry(),
            pin: SecretEdit::Set(pin.into()),
        });
        let error = encode_line(&bad).unwrap_err();
        assert!(!format!("{error:?}").contains("12a4"), "{error:?}");
    }
    // `Debug` of a set PIN is masked.
    assert!(!format!("{set:?}").contains("024680"));
}

#[test]
fn the_changed_event_carries_whether_a_pin_is_stored() {
    let changed = event(EventPayload::LiveLanChanged(LiveLanEntry {
        has_pin: true,
        ..entry()
    }));
    let line = round_trip(&changed);
    assert!(
        line.contains(
            "\tevent=live_lan_changed\tenabled=1\tsource=output%3A7\tmax_viewers=12\thas_pin=1\t"
        ),
        "{line}"
    );
    refused(&line.replacen("\thas_pin=1", "", 1));
    refused(&line.replacen("\thas_pin=1", "\thas_pin=2", 1));
    refused(&line.replacen("\thas_pin=1", "\thas_pin=1\tpin=1234", 1));
}

#[test]
fn the_status_runtime_event_is_exact() {
    let streaming = runtime(RuntimeLifecycleEvent::LiveLanStatus(LiveLanStatus {
        state: LiveLanState::Live,
        url: "http://192.168.1.20:8090/".into(),
        viewers: 3,
        egress_kbps: 7_500,
    }));
    let line = round_trip(&streaming);
    assert!(
        line.ends_with(
            "\tevent=live_lan_status\tstate=live\turl=http%3A%2F%2F192.168.1.20%3A8090%2F\tviewers=3\tegress_kbps=7500\n"
        ),
        "{line}"
    );
    let off = runtime(RuntimeLifecycleEvent::LiveLanStatus(
        LiveLanStatus::default(),
    ));
    assert!(round_trip(&off).ends_with("\tstate=off\turl=\tviewers=0\tegress_kbps=0\n"));
    for state in LiveLanState::ALL {
        round_trip(&runtime(RuntimeLifecycleEvent::LiveLanStatus(
            LiveLanStatus {
                state,
                ..LiveLanStatus::default()
            },
        )));
    }
    refused(&line.replacen("state=live", "state=paused", 1));
    refused(&line.replacen("\turl=http%3A%2F%2F192.168.1.20%3A8090%2F", "", 1));
    refused(&line.replacen("http%3A%2F%2F", "https%3A%2F%2F", 1));
    refused(&line.replacen("viewers=3", "viewers=300", 1));
    refused(&line.replacen("egress_kbps=7500", "egress_kbps=-1", 1));
    let bad = runtime(RuntimeLifecycleEvent::LiveLanStatus(LiveLanStatus {
        url: "http://a~b/".into(),
        ..LiveLanStatus::default()
    }));
    assert!(encode_line(&bad).is_err());
}
