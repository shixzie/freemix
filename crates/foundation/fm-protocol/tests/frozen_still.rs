//! SW-016: frozen-still export requests and parts round-trip at Protocol
//! 2.164.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, FrozenStillRequest, FrozenStillResponse, WireInputId, WireMessage,
    decode_line, encode_line, frozen_still_parts,
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
        line.starts_with("frozen_still_request\tprotocol=2.164\t"),
        "{line}"
    );
    assert!(line.contains("\trequest_id=still-1\tinput=3\n"), "{line}");
    assert_eq!(decode_line(&line).unwrap(), message);
}

#[test]
fn png_parts_round_trip_and_reassemble() {
    let png = vec![0x5a_u8; 70_000];
    let parts = frozen_still_parts(
        CURRENT_PROTOCOL_VERSION,
        "still-2",
        input(1),
        640,
        360,
        &png,
    )
    .unwrap();
    assert_eq!(parts.len(), 3);
    let mut joined = Vec::new();
    for part in parts {
        assert!(part.data.len() <= fm_protocol::MAX_FROZEN_STILL_PART_BYTES);
        let message = WireMessage::FrozenStillResponse(part.clone());
        let line = encode_line(&message).unwrap();
        let WireMessage::FrozenStillResponse(decoded) = decode_line(&line).unwrap() else {
            panic!("expected a frozen still response");
        };
        assert_eq!(decoded, part);
        joined.extend_from_slice(&decoded.data);
    }
    assert_eq!(joined, png);
}

#[test]
fn out_of_bounds_parts_are_refused() {
    let parts =
        frozen_still_parts(CURRENT_PROTOCOL_VERSION, "still-3", input(1), 4, 3, b"png").unwrap();
    let message = WireMessage::FrozenStillResponse(FrozenStillResponse {
        part: 1,
        ..parts[0].clone()
    });
    assert!(encode_line(&message).is_err());
    let message = WireMessage::FrozenStillResponse(FrozenStillResponse {
        width: 0,
        ..parts[0].clone()
    });
    assert!(encode_line(&message).is_err());
    assert!(
        frozen_still_parts(CURRENT_PROTOCOL_VERSION, "still-4", input(1), 0, 3, b"png").is_err()
    );
}
