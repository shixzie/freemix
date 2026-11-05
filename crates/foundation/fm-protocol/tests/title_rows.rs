//! GX-005: title data rows round-trip at Protocol 2.165.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, TitleAirRequirement, TitleRowValue,
    WireInputId, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn message(values: Vec<TitleRowValue>) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:row".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(7),
        deadline_ms: Some(500),
        payload: CommandPayload::SetTitleRow {
            input: input(9),
            base_revision: 4,
            require: TitleAirRequirement::OnAir,
            values,
            keep_live: true,
        },
    })
}

#[test]
fn a_title_row_round_trips_with_exact_values() {
    let values = vec![
        TitleRowValue {
            field: "Bar color".to_owned(),
            value: "#ABCDEF".to_owned(),
        },
        TitleRowValue {
            field: "Score".to_owned(),
            value: "12".to_owned(),
        },
    ];
    let message = message(values);
    let line = encode_line(&message).unwrap();
    assert!(line.contains("payload=title_row"), "{line}");
    assert!(line.contains("require=on_air"), "{line}");
    assert!(line.contains("keep_live=1"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn duplicate_or_malformed_row_fields_are_refused() {
    let duplicate = message(vec![
        TitleRowValue {
            field: "Score".to_owned(),
            value: "1".to_owned(),
        },
        TitleRowValue {
            field: "Score".to_owned(),
            value: "2".to_owned(),
        },
    ]);
    assert!(encode_line(&duplicate).is_err());

    let empty_field = message(vec![TitleRowValue {
        field: String::new(),
        value: "1".to_owned(),
    }]);
    assert!(encode_line(&empty_field).is_err());

    let oversized = message(vec![TitleRowValue {
        field: "Score".to_owned(),
        value: "x".repeat(fm_protocol::MAX_TITLE_ROW_VALUE_BYTES + 1),
    }]);
    assert!(encode_line(&oversized).is_err());
}
