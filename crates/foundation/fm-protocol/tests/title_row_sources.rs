//! GX-005: scheduled title data sources round-trip at Protocol 2.166.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, TitleRowSourceOperation, TitleRowSourceRequest,
    TitleRowSourceResponse, WireInputId, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

#[test]
fn title_source_requests_and_responses_round_trip() {
    let requests = [
        TitleRowSourceOperation::Start {
            path: "data/scores.csv".into(),
            interval_ms: 1_500,
            keep_live: true,
        },
        TitleRowSourceOperation::Stop,
        TitleRowSourceOperation::Start {
            path: "http://example.test/scores.json".into(),
            interval_ms: 1_000,
            keep_live: false,
        },
        TitleRowSourceOperation::Status,
    ];
    for operation in requests {
        let message = WireMessage::TitleRowSourceRequest(TitleRowSourceRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "source-1".into(),
            input: input(9),
            operation,
        });
        let line = encode_line(&message).unwrap();
        assert_eq!(decode_line(&line).unwrap(), message);
    }
    let response = WireMessage::TitleRowSourceResponse(TitleRowSourceResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "source-1".into(),
        input: input(9),
        active: true,
        path: Some("data/scores.csv".into()),
        interval_ms: Some(1_500),
        keep_live: true,
        applied_rows: 4,
        error: None,
    });
    let line = encode_line(&response).unwrap();
    assert_eq!(decode_line(&line).unwrap(), response);
}

#[test]
fn a_source_path_outside_the_bundle_is_refused() {
    for path in ["/etc/passwd", "../rows.csv", "data/../../rows.csv"] {
        let message = WireMessage::TitleRowSourceRequest(TitleRowSourceRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "source-2".into(),
            input: input(1),
            operation: TitleRowSourceOperation::Start {
                path: path.into(),
                interval_ms: 1_000,
                keep_live: false,
            },
        });
        let line = encode_line(&message);
        assert!(
            line.is_err() || decode_line(&line.unwrap()).is_err(),
            "{path}"
        );
    }
}
