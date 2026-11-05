//! SW-016: frozen-still export requests and responses round-trip at
//! Protocol 2.164.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, FrozenStillRequest, FrozenStillResponse, WireInputId, WireMessage,
    decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

#[test]
fn a_frozen_still_request_round_trips() {
    let message = WireMessage::FrozenStillRequest(FrozenStillRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "still-1".into(),
        input: input(3),
    });
    let line = encode_line(&message).unwrap();
    assert!(
        line.starts_with("frozen_still_request\tprotocol=2.165\t"),
        "{line}"
    );
    assert!(line.contains("\trequest_id=still-1\tinput=3\n"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn a_frozen_still_response_names_the_written_asset() {
    let message = WireMessage::FrozenStillResponse(FrozenStillResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "still-2".into(),
        input: input(1),
        width: 1920,
        height: 1080,
        asset_uri: "asset://frozen-1-12.png".into(),
        path: "assets/frozen-1-12.png".into(),
    });
    let line = encode_line(&message).unwrap();
    assert!(
        line.contains("\twidth=1920\theight=1080\tasset_uri=asset%3A%2F%2Ffrozen-1-12.png"),
        "{line}"
    );
    assert!(line.contains("\tpath=assets%2Ffrozen-1-12.png\n"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn an_empty_still_dimension_is_refused() {
    let message = WireMessage::FrozenStillResponse(FrozenStillResponse {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "still-3".into(),
        input: input(1),
        width: 0,
        height: 1080,
        asset_uri: "asset://frozen.png".into(),
        path: "assets/frozen.png".into(),
    });
    assert!(encode_line(&message).is_err());
}
