//! Protocol 2.59 live input thumbnails: the exact
//! `input_thumbnail_subscription` and `input_thumbnail` records, their
//! bounds, and decoding.

use std::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, InputThumbnailMessage, InputThumbnailSubscription,
    MAX_INPUT_THUMBNAIL_BYTES, MAX_INPUT_THUMBNAIL_HEIGHT, MAX_INPUT_THUMBNAIL_WIDTH,
    MonitorFrameOrigin, MonitorImageError, ProtocolVersion, ServerIdentity, WireInputId,
    WireMessage, decode_input_thumbnail, decode_line, encode_line, encode_qoi_rgb,
};

fn server() -> ServerIdentity {
    ServerIdentity {
        project_id: "p".into(),
        engine_id: "e".into(),
        state_epoch: 3,
        log_id: "l".into(),
    }
}

fn thumbnail() -> InputThumbnailMessage {
    let pixels = [255_u8, 0, 0, 0, 255, 0];
    InputThumbnailMessage {
        server: server(),
        input: WireInputId::from_domain(fm_types::InputId::new(NonZeroU128::new(7).unwrap())),
        sequence: 12,
        origin: MonitorFrameOrigin::Native,
        age_ms: 1_500,
        width: 2,
        height: 1,
        data: encode_qoi_rgb(2, 1, &pixels, 6, 3).unwrap(),
    }
}

#[test]
fn the_version_is_current() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 62));
}

#[test]
fn subscriptions_encode_exactly() {
    for (enabled, flag) in [(true, "1"), (false, "0")] {
        let message =
            WireMessage::InputThumbnailSubscription(InputThumbnailSubscription { enabled });
        let line = encode_line(&message).unwrap();
        assert_eq!(
            line,
            format!("input_thumbnail_subscription\tenabled={flag}\n")
        );
        assert_eq!(decode_line(&line).unwrap(), message);
    }
    for bad in [
        "input_thumbnail_subscription\tenabled=2\n",
        "input_thumbnail_subscription\tenabled=true\n",
        "input_thumbnail_subscription\n",
        "input_thumbnail_subscription\tenabled=1\textra=1\n",
    ] {
        assert!(decode_line(bad).is_err(), "{bad}");
    }
}

#[test]
fn thumbnails_round_trip_with_an_exact_field_order() {
    let message = WireMessage::InputThumbnail(thumbnail());
    let line = encode_line(&message).unwrap();
    assert_eq!(
        line,
        "input_thumbnail\tengine_id=e\tproject_id=p\tstate_epoch=3\tlog_id=l\tinput=7\t\
         sequence=12\torigin=native\tage_ms=1500\twidth=2\theight=1\t\
         data=cW9pZgAAAAIAAAABAwBadgAAAAAAAAAB\n"
    );
    assert_eq!(decode_line(&line).unwrap(), message);

    let image = decode_input_thumbnail(&thumbnail()).unwrap();
    assert_eq!(
        (image.width, image.height, image.sequence, image.age_ms),
        (2, 1, 12, 1_500)
    );
    assert_eq!(&*image.rgba, &[255, 0, 0, 255, 0, 255, 0, 255]);
}

#[test]
fn thumbnail_bounds_are_enforced_both_ways() {
    let mut wide = thumbnail();
    wide.width = MAX_INPUT_THUMBNAIL_WIDTH + 1;
    let mut tall = thumbnail();
    tall.height = MAX_INPUT_THUMBNAIL_HEIGHT + 1;
    let mut empty = thumbnail();
    empty.data.clear();
    let mut huge = thumbnail();
    huge.data = vec![0; MAX_INPUT_THUMBNAIL_BYTES + 1];
    for message in [wide, tall, empty, huge] {
        assert!(matches!(
            encode_line(&WireMessage::InputThumbnail(message)),
            Err(CodecError::InvalidField { .. })
        ));
    }
    // The largest record still fits one line.
    let mut largest = thumbnail();
    largest.data = vec![0xa5; MAX_INPUT_THUMBNAIL_BYTES];
    largest.sequence = u64::MAX;
    largest.age_ms = u32::MAX;
    largest.width = MAX_INPUT_THUMBNAIL_WIDTH;
    largest.height = MAX_INPUT_THUMBNAIL_HEIGHT;
    let line = encode_line(&WireMessage::InputThumbnail(largest.clone())).unwrap();
    assert_eq!(
        decode_line(&line).unwrap(),
        WireMessage::InputThumbnail(largest)
    );

    let line = encode_line(&WireMessage::InputThumbnail(thumbnail())).unwrap();
    for (from, to) in [
        ("\twidth=2\t", "\twidth=161\t"),
        ("\theight=1\t", "\theight=0\t"),
        ("\torigin=native\t", "\torigin=gpu\t"),
        ("\tsequence=12\t", "\tsequence=012\t"),
        ("\tage_ms=1500\t", "\tage_ms=4294967296\t"),
        ("\tinput=7\t", "\tinput=0\t"),
        ("\tdata=", "\tdata=*"),
    ] {
        let bad = line.replace(from, to);
        assert_ne!(bad, line);
        assert!(decode_line(&bad).is_err(), "{bad}");
    }
}

#[test]
fn decoding_checks_the_size_against_the_stream() {
    let mut wrong = thumbnail();
    wrong.width = 3;
    assert!(decode_input_thumbnail(&wrong).is_err());
    let mut wide = thumbnail();
    wide.width = 200;
    assert_eq!(
        decode_input_thumbnail(&wide),
        Err(MonitorImageError::InvalidDimensions {
            width: 200,
            height: 1
        })
    );
}
