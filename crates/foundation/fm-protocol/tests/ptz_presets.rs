//! IN-028: PTZ preset requests and answers round-trip exactly and strictly
//! (Protocol 2.157).

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, PtzPresetStatus, PtzPresetsRequest, PtzPresetsResponse, WireMessage,
    decode_line, encode_line,
};

fn request() -> WireMessage {
    WireMessage::PtzPresetsRequest(PtzPresetsRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "ptz-1".to_owned(),
    })
}

fn statuses() -> Vec<PtzPresetStatus> {
    vec![
        PtzPresetStatus {
            camera: "cam-a".to_owned(),
            preset: 3,
            name: "Wide".to_owned(),
            thumbnail: Some("asset://thumbs/wide.png".to_owned()),
            enabled: true,
        },
        PtzPresetStatus {
            camera: "cam-b".to_owned(),
            preset: 0,
            name: "Close".to_owned(),
            thumbnail: None,
            enabled: false,
        },
    ]
}

#[test]
fn ptz_preset_requests_and_answers_round_trip_exactly() {
    let line = encode_line(&request()).unwrap();
    assert_eq!(
        line,
        "ptz_presets_request\tprotocol=2.160\trequest_id=ptz-1\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    let response = WireMessage::PtzPresetsResponse(PtzPresetsResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "ptz-1".to_owned(),
        presets: statuses(),
    });
    let line = encode_line(&response).unwrap();
    assert!(
        line.starts_with(
            "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1\tpresets=63616d2d61%2C3%2C1%2C"
        ),
        "{line}"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    let empty = WireMessage::PtzPresetsResponse(PtzPresetsResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "ptz-2".to_owned(),
        presets: Vec::new(),
    });
    let line = encode_line(&empty).unwrap();
    assert_eq!(
        line,
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-2\tpresets=\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);
}

#[test]
fn ptz_preset_messages_are_strict() {
    for line in [
        "ptz_presets_request\trequest_id=ptz-1",
        "ptz_presets_request\tprotocol=2.160\trequest_id=",
        "ptz_presets_request\tprotocol=2.160\trequest_id=ptz-1\textra=1",
        "ptz_presets_request\tprotocol=3\trequest_id=ptz-1",
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1",
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1\tpresets=zz",
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1\tpresets=63616d%2C1%2C1%2C-%2C",
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1\tpresets=63616d%2C1%2C1%2C-%2C6e6",
        "ptz_presets_response\tprotocol=2.160\trequest_id=ptz-1\t\
         presets=63616d%2C1%2C1%2C-%2C6e616d65%2Cextra",
    ] {
        assert!(
            decode_line(&format!("{line}\n")).is_err(),
            "accepted {line:?}"
        );
    }
}
