//! Protocol 2.90 web title editor: the `title_field_exposure` command, the
//! `title_exposure_changed` event, draft fields on an exact title preview,
//! and the lossy `title_phases` record round-trip exactly and refuse
//! everything outside the contract.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_LIST_ITEMS,
    MAX_WEB_TITLE_PREVIEW_HEIGHT, MAX_WEB_TITLE_PREVIEW_WIDTH, ProtocolVersion, TitleFieldExposure,
    TitleFieldStatus, TitleFieldUpdate, TitleFieldValue, TitlePhase, TitlePhaseStatus,
    TitlePhasesMessage, TitlePreviewRequest, TitlePreviewSample, TitleShownOn, TitleStateMessage,
    TitleTimelineStatus, WireInputId, WireMessage, WireTitleFieldId, decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.149\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn field(value: u128) -> WireTitleFieldId {
    WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
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

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload,
        edit_history: EditHistoryStatus {
            undo_depth: 0,
            redo_depth: 0,
            next_undo: None,
            next_redo: None,
        },
    })
}

#[test]
fn the_protocol_is_2_90() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 149));
}

#[test]
fn field_exposure_commands_are_exact_and_strict() {
    for (exposed, flag) in [(true, 1), (false, 0)] {
        let line = round_trip(&command(CommandPayload::SetTitleFieldExposure {
            input: input(7),
            field: field(3),
            exposed,
        }));
        assert_eq!(
            line,
            format!("{PREFIX}\tpayload=title_field_exposure\tinput=7\tfield=3\texposed={flag}\n")
        );
    }
    for bad in [
        "input=7\tfield=0\texposed=1",
        "input=7\tfield=03\texposed=1",
        "input=7\tfield=x\texposed=1",
        "input=7\tfield=3\texposed=2",
        "input=7\tfield=3",
        "input=7\texposed=1",
        "input=7\tfield=3\texposed=1\textra=1",
    ] {
        let line = format!("{PREFIX}\tpayload=title_field_exposure\t{bad}\n");
        assert!(decode_line(&line).is_err(), "{bad}");
    }
}

#[test]
fn exposure_change_events_are_exact_and_strict() {
    let payload = |inputs: Vec<WireInputId>, fields: Vec<(u128, bool)>| {
        event(EventPayload::TitleExposureChanged {
            inputs,
            fields: fields
                .into_iter()
                .map(|(id, exposed)| TitleFieldExposure {
                    field: field(id),
                    exposed,
                })
                .collect(),
        })
    };
    let line = round_trip(&payload(
        vec![input(4), input(9)],
        vec![(1, true), (2, false)],
    ));
    assert!(
        line.contains("\tevent=title_exposure_changed\tinputs=4%2C9\tfields=1%3A1%2C2%3A0\t"),
        "{line}"
    );
    // A Web session's copy may name no field at all.
    let line = round_trip(&payload(vec![input(4)], Vec::new()));
    assert!(line.contains("\tinputs=4\tfields=\t"), "{line}");
    for (inputs, fields) in [
        (Vec::new(), vec![(1, true)]),
        (vec![input(9), input(4)], vec![(1, true)]),
        (vec![input(4)], vec![(2, true), (1, true)]),
        (vec![input(4)], vec![(1, true), (1, false)]),
    ] {
        assert!(encode_line(&payload(inputs, fields)).is_err());
    }
    let good = encode_line(&payload(vec![input(4)], vec![(1, true), (2, false)])).unwrap();
    for (from, to) in [
        ("fields=1%3A1%2C2%3A0", "fields=1%3A1%2C2%3A2"),
        ("fields=1%3A1%2C2%3A0", "fields=2%3A0%2C1%3A1"),
        ("fields=1%3A1%2C2%3A0", "fields=1%3A1%2C02%3A0"),
        ("fields=1%3A1%2C2%3A0", "fields=1"),
        ("inputs=4", "inputs="),
    ] {
        let tampered = good.replace(from, to);
        assert_ne!(tampered, good);
        assert!(decode_line(&tampered).is_err(), "{to}");
    }
}

fn preview(fields: Vec<TitleFieldUpdate>) -> WireMessage {
    WireMessage::TitlePreviewRequest(TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "web-1".to_owned(),
        input: input(7),
        sample: TitlePreviewSample::Hold,
        width: MAX_WEB_TITLE_PREVIEW_WIDTH,
        height: MAX_WEB_TITLE_PREVIEW_HEIGHT,
        clip: None,
        fields,
    })
}

#[test]
fn previews_carry_draft_fields_only_when_there_are_any() {
    let line = round_trip(&preview(Vec::new()));
    assert_eq!(
        line,
        "title_preview_request\tprotocol=2.149\trequest_id=web-1\tinput=7\tphase=hold\t\
         frame=0\twidth=320\theight=180\n"
    );
    let drafted = preview(vec![
        TitleFieldUpdate {
            field: field(1),
            value: TitleFieldValue::Text("Ana, Lima".to_owned()),
        },
        TitleFieldUpdate {
            field: field(4),
            value: TitleFieldValue::Image {
                asset_uri: "asset://uploads/logo-0123456789abcdef.png".to_owned(),
            },
        },
    ]);
    let line = round_trip(&drafted);
    assert!(
        line.ends_with(
            "\tfields=1%3At%3A9%3AAna%2C%20Lima%2C4%3Ai%3A41%3Aasset%3A%2F%2Fuploads%2Flogo-0123456789abcdef.png\n"
        ),
        "{line}"
    );
    // An empty list is written as no key; written explicitly it is refused.
    let empty = line.split("\tfields=").next().unwrap().to_owned() + "\tfields=\n";
    assert!(matches!(
        decode_line(&empty),
        Err(CodecError::InvalidField {
            field: "fields",
            ..
        })
    ));
    // Out of order or repeated drafts are refused both ways.
    let unordered = preview(vec![
        TitleFieldUpdate {
            field: field(4),
            value: TitleFieldValue::Number(1),
        },
        TitleFieldUpdate {
            field: field(1),
            value: TitleFieldValue::Number(2),
        },
    ]);
    assert!(encode_line(&unordered).is_err());
}

fn phases(titles: Vec<TitlePhaseStatus>) -> WireMessage {
    WireMessage::TitlePhases(TitlePhasesMessage { frame: 900, titles })
}

fn phase(
    id: u128,
    phase: TitlePhase,
    progress: (u64, u64),
    timeline: Option<TitleTimelineStatus>,
) -> TitlePhaseStatus {
    TitlePhaseStatus {
        input: input(id),
        phase,
        phase_frame: progress.0,
        phase_frames: progress.1,
        timeline,
    }
}

#[test]
fn title_phase_records_are_exact_and_strict() {
    let air = Some(TitleTimelineStatus {
        shown: TitleShownOn::Air,
        since_frame: 880,
    });
    let preview_shown = Some(TitleTimelineStatus {
        shown: TitleShownOn::Preview,
        since_frame: 12,
    });
    let line = round_trip(&phases(vec![
        phase(2, TitlePhase::In, (20, 30), air),
        phase(3, TitlePhase::Hold, (0, 0), preview_shown),
        phase(5, TitlePhase::Idle, (0, 0), None),
        phase(8, TitlePhase::Out, (1, 10), air),
    ]));
    assert_eq!(
        line,
        "title_phases\tframe=900\ttitles=2%3Ain%3A20%3A30%3Aair%3A880%2C3%3Ahold%3A0%3A0%3Apreview%3A12%2C\
         5%3Aidle%3A0%3A0%3Anone%3A0%2C8%3Aout%3A1%3A10%3Aair%3A880\n"
    );
    assert_eq!(
        round_trip(&phases(Vec::new())),
        "title_phases\tframe=900\ttitles=\n"
    );
    for bad in [
        vec![
            phase(3, TitlePhase::Idle, (0, 0), None),
            phase(2, TitlePhase::Idle, (0, 0), None),
        ],
        vec![
            phase(2, TitlePhase::Idle, (0, 0), None),
            phase(2, TitlePhase::Idle, (0, 0), None),
        ],
        vec![phase(2, TitlePhase::In, (30, 30), air)],
        vec![phase(2, TitlePhase::Hold, (1, 2), air)],
    ] {
        assert!(encode_line(&phases(bad)).is_err());
    }
    let too_many = (1..=u128::try_from(MAX_LIST_ITEMS).unwrap() + 1)
        .map(|id| phase(id, TitlePhase::Idle, (0, 0), None))
        .collect();
    assert!(encode_line(&phases(too_many)).is_err());
    let good = "title_phases\tframe=900\ttitles=2%3Ain%3A20%3A30%3Aair%3A880\n";
    assert!(decode_line(good).is_ok());
    for (from, to) in [
        ("2%3Ain", "0%3Ain"),
        ("in%3A20", "In%3A20"),
        ("%3A20%3A30", "%3A30%3A30"),
        ("air%3A880", "program%3A880"),
        ("air%3A880", "air%3A0880"),
        ("air%3A880", "air"),
        ("frame=900", "frame=0900"),
    ] {
        let tampered = good.replace(from, to);
        assert_ne!(tampered, good);
        assert!(decode_line(&tampered).is_err(), "{to}");
    }
    assert!(decode_line("title_phases\tframe=900\ttitles=5%3Aidle%3A0%3A0%3Anone%3A3\n").is_err());
    assert!(decode_line("title_phases\tframe=900\n").is_err());
}

fn title_state(revisions: [u64; 2]) -> WireMessage {
    WireMessage::TitleState(TitleStateMessage {
        revision: 30,
        input: input(2),
        name: "Lower".to_owned(),
        field_revision: 6,
        phase: TitlePhase::Idle,
        phase_frame: 0,
        phase_frames: 0,
        timeline: None,
        fields: [(1, "Name"), (4, "Role")]
            .into_iter()
            .zip(revisions)
            .map(|((id, name), revision)| TitleFieldStatus {
                field: field(id),
                name: name.to_owned(),
                value: TitleFieldValue::Text("x".to_owned()),
                exposed: true,
                revision,
            })
            .collect(),
    })
}

#[test]
fn title_states_carry_exact_per_field_revisions() {
    // Protocol 2.96: each field's revision follows its exposure flag,
    // positionally, none above the title's field revision.
    let line = round_trip(&title_state([0, 6]));
    assert!(
        line.ends_with("\tfield_exposed=11\tfield_revisions=0%2C6\n"),
        "{line}"
    );
    assert!(encode_line(&title_state([0, 7])).is_err());
    for (to, why) in [
        ("field_revisions=0", "too few"),
        ("field_revisions=0%2C6%2C1", "too many"),
        ("field_revisions=0%2C7", "after the title"),
        ("field_revisions=0%2C06", "not canonical"),
        ("field_revisions=0%2C", "empty item"),
        ("field_revisions=0%2C-1", "negative"),
        ("field_revisions=", "missing items"),
    ] {
        let tampered = line.replace("field_revisions=0%2C6", to);
        assert_ne!(tampered, line);
        assert!(decode_line(&tampered).is_err(), "{why}");
    }
    let missing = line.replace("\tfield_revisions=0%2C6", "");
    assert_eq!(
        decode_line(&missing),
        Err(CodecError::MissingField("field_revisions"))
    );
}
