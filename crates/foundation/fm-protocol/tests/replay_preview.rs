//! OR-014: the replay multiview request and its frame parts round-trip
//! exactly and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ReplayPreviewRequest, ServerIdentity, TitlePreviewFrameMessage,
    WireInputId, WireMessage, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "e".into(),
        project_id: "p".into(),
        state_epoch: 1,
        log_id: "l".into(),
    }
}

#[test]
fn replay_preview_requests_round_trip_and_are_bounded() {
    let request = WireMessage::ReplayPreviewRequest(ReplayPreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "replay-preview-3".to_owned(),
        input: input(7),
        width: 320,
        height: 180,
    });
    let line = encode_line(&request).unwrap();
    assert_eq!(
        line,
        "replay_preview_request\tprotocol=2.126\trequest_id=replay-preview-3\tinput=7\t\
         width=320\theight=180\n"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);

    for fields in [
        // Missing, zero, oversized, or non-canonical dimensions.
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\theight=180",
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\twidth=0\theight=180",
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\twidth=641\theight=180",
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\twidth=320\theight=361",
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\twidth=0320\theight=180",
        // A blank or missing request id, and an old protocol.
        "replay_preview_request\tprotocol=2.126\trequest_id=\tinput=7\twidth=320\theight=180",
        // An unknown field.
        "replay_preview_request\tprotocol=2.126\trequest_id=p\tinput=7\twidth=320\theight=180\tx=1",
    ] {
        assert!(decode_line(&format!("{fields}\n")).is_err(), "{fields}");
    }
}

#[test]
fn replay_preview_frames_round_trip_exactly() {
    let message = WireMessage::ReplayPreviewFrame(TitlePreviewFrameMessage {
        server: server(),
        request_id: "replay-preview-3".to_owned(),
        input: input(7),
        animation_revision: 0,
        draft: false,
        width: 320,
        height: 180,
        part: 0,
        parts: 1,
        data: vec![1, 2, 3, 4],
    });
    let line = encode_line(&message).unwrap();
    assert!(
        line.starts_with(
            "replay_preview_frame\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\t\
             request_id=replay-preview-3\tinput=7\tanimation_revision=0\tdraft=0\t\
             width=320\theight=180\tpart=0\tparts=1\tdata="
        ),
        "{line}"
    );
    assert_eq!(encode_line(&decode_line(&line).unwrap()).unwrap(), line);
}
