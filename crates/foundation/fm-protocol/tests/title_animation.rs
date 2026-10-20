//! Protocol 2.29 title animation: the three edit commands, the
//! `title_animation_state` record, the `title_animation_changed` event, and
//! exact title previews round-trip exactly and refuse everything outside the
//! contract, including every clip bound `fm_animation` enforces.

use core::num::NonZeroU128;

use fm_animation::{
    AnimationPreset, Clip, CubicBezier, Easing, Keyframe, MAX_CLIP_KEYS, MAX_PHASE_FRAMES, Phase,
    Property, Rgba, Timebase, Track, TrackKey, Value,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_TITLE_ANIMATION_BYTES,
    MAX_TITLE_PREVIEW_PART_BYTES, ServerIdentity, TitleAnimationStateMessage,
    TitlePreviewAssembler, TitlePreviewFrameMessage, TitlePreviewRequest, TitlePreviewSample,
    WireInputId, WireMessage, WireTitleClip, WireTitleElementId, decode_line, decode_qoi_rgba,
    encode_line, encode_qoi_rgba, title_clip_wire_bytes, title_preview_parts,
};
use fm_types::FrameRate;

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn element(value: u128) -> WireTitleElementId {
    WireTitleElementId::new(NonZeroU128::new(value).unwrap())
}

fn timebase(numerator: u32, denominator: u32) -> Timebase {
    Timebase::new(FrameRate::new(numerator, denominator).unwrap())
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

const PREFIX: &str = "command\tprotocol=2.75\tid=a\tidempotency_key=k";

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(word >> (18 - 6 * index) & 0x3f) as usize],
            ));
        }
    }
    out
}

/// A small clip touching scalar, color, preset, and bezier keys in both
/// phases, at 25p.
fn clip() -> WireTitleClip {
    let bezier = Easing::CubicBezier(CubicBezier::new(10_000, -20_000, 50_000, 131_072).unwrap());
    Clip::from_tracks(
        timebase(25, 1),
        12,
        8,
        vec![
            Track::new(
                TrackKey::new(element(3), Phase::In, Property::PositionX),
                vec![
                    Keyframe::new(0, Value::Scalar(-1_920), Easing::CubicOut),
                    Keyframe::new(12, Value::Scalar(0), Easing::Linear),
                ],
            ),
            Track::new(
                TrackKey::new(element(3), Phase::Out, Property::Color),
                vec![
                    Keyframe::new(0, Value::Color(Rgba::new(255, 0, 16, 255)), bezier),
                    Keyframe::new(8, Value::Color(Rgba::new(0, 0, 0, 0)), Easing::Hold),
                ],
            ),
            Track::new(
                TrackKey::new(element(u128::MAX), Phase::In, Property::Opacity),
                vec![Keyframe::new(4, Value::Scalar(255), Easing::Step)],
            ),
        ],
    )
    .unwrap()
}

/// The exact binary encoding of [`clip`].
fn clip_bytes() -> Vec<u8> {
    let mut bytes = vec![25, 1, 12, 8, 3];
    // element 3, In, position-x (index 0), two keys
    bytes.extend_from_slice(&[3, 0, 0, 2]);
    // frame 0, zigzag(-1920) = 3839 = 0xff 0x1d, cubic-out (6)
    bytes.extend_from_slice(&[0, 0xff, 0x1d, 6]);
    // frame 12, value 0, linear
    bytes.extend_from_slice(&[12, 0, 0]);
    // element 3, Out, color (index 10), two keys
    bytes.extend_from_slice(&[3, 1, 10, 2]);
    // frame 0, rgba, bezier: x1 10000, y1 zigzag(-20000) = 39999,
    // x2 50000, y2 zigzag(131072) = 262144
    bytes.extend_from_slice(&[0, 255, 0, 16, 255, 8]);
    bytes.extend_from_slice(&[
        0x90, 0x4e, 0xbf, 0xb8, 0x02, 0xd0, 0x86, 0x03, 0x80, 0x80, 0x10,
    ]);
    // frame 8, transparent, hold (9)
    bytes.extend_from_slice(&[8, 0, 0, 0, 0, 9]);
    // element u128::MAX: nineteen bytes
    bytes.extend_from_slice(&[0xff; 18]);
    bytes.push(0x03);
    // In, opacity (index 5), one key: frame 4, zigzag(255) = 510, step (1)
    bytes.extend_from_slice(&[0, 5, 1, 4, 0xfe, 0x03, 1]);
    bytes
}

#[test]
fn clip_encoding_is_exact_and_canonical() {
    let encoded = base64url(&clip_bytes());
    let line = round_trip(&command(CommandPayload::SetTitleAnimation {
        input: input(7),
        base_revision: 4,
        clip: Box::new(clip()),
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=title_animation_set\tinput=7\tbase_revision=4\tclip={encoded}\n"
        )
    );
    assert_eq!(title_clip_wire_bytes(&clip()), clip_bytes().len());

    // The empty default clip.
    let empty = Clip::new(timebase(30, 1), 0, 0).unwrap();
    let line = round_trip(&command(CommandPayload::SetTitleAnimation {
        input: input(7),
        base_revision: 0,
        clip: Box::new(empty),
    }));
    assert!(line.ends_with(&format!("clip={}\n", base64url(&[30, 1, 0, 0, 0]))));
}

fn set_line(bytes: &[u8]) -> String {
    format!(
        "{PREFIX}\tpayload=title_animation_set\tinput=7\tbase_revision=4\tclip={}\n",
        base64url(bytes)
    )
}

fn assert_clip_refused(bytes: &[u8], why: &str) {
    match decode_line(&set_line(bytes)) {
        Err(CodecError::InvalidField { field: "clip", .. }) => {}
        other => panic!("{why}: {other:?}"),
    }
}

#[test]
fn malformed_clips_are_refused() {
    let valid = clip_bytes();
    assert!(decode_line(&set_line(&valid)).is_ok());

    let mut trailing = valid.clone();
    trailing.push(0);
    assert_clip_refused(&trailing, "trailing byte");
    assert_clip_refused(&valid[..valid.len() - 1], "truncated");
    // Overlong varint for the rate numerator (25 as 0x99 0x00).
    let mut overlong = vec![0x99, 0x00];
    overlong.extend_from_slice(&valid[1..]);
    assert_clip_refused(&overlong, "overlong varint");
    assert_clip_refused(&[50, 2, 0, 0, 0], "rate not in lowest terms");
    assert_clip_refused(&[0, 1, 0, 0, 0], "zero rate");
    // 36001 in-frames: 0xa1 0x99 0x02.
    assert_clip_refused(&[30, 1, 0xa1, 0x99, 0x02, 0, 0], "phase too long");
    let one_key = |head: &[u8]| {
        let mut bytes = vec![30, 1, 10, 0, 1];
        bytes.extend_from_slice(head);
        bytes
    };
    assert_clip_refused(&one_key(&[0, 0, 0, 1, 0, 0, 0]), "zero element");
    assert_clip_refused(&one_key(&[1, 2, 0, 1, 0, 0, 0]), "phase byte");
    assert_clip_refused(&one_key(&[1, 0, 11, 1, 0, 0, 0]), "property index");
    assert_clip_refused(&one_key(&[1, 0, 0, 0]), "empty track");
    assert_clip_refused(&one_key(&[1, 0, 0, 1, 0, 0, 19]), "easing tag");
    assert_clip_refused(&one_key(&[1, 0, 0, 1, 11, 0, 0]), "key outside phase");
    // Opacity 256 is out of its range.
    assert_clip_refused(&one_key(&[1, 0, 5, 1, 0, 0x80, 0x04, 0]), "value range");
    // Bezier x1 = 65537.
    assert_clip_refused(
        &one_key(&[1, 0, 0, 1, 0, 0, 8, 0x81, 0x80, 0x04, 0, 0, 0]),
        "bezier control",
    );
    // Two keys at the same frame.
    assert_clip_refused(&one_key(&[1, 0, 0, 2, 0, 0, 0, 0, 0, 0]), "key order");
    // Tracks out of order, and a duplicate track.
    let mut unordered = vec![30, 1, 10, 0, 2];
    unordered.extend_from_slice(&[2, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0]);
    assert_clip_refused(&unordered, "track order");
    let mut duplicate = vec![30, 1, 10, 0, 2];
    duplicate.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0]);
    assert_clip_refused(&duplicate, "duplicate track");

    // Padding and the standard alphabet are refused.
    let padded = format!("{}=", set_line(&valid).trim_end());
    assert!(decode_line(&format!("{padded}\n")).is_err());
}

#[test]
fn oversized_clips_are_refused_by_the_encoder() {
    // 4096 keys with the widest values and bezier easings: inside every
    // fm_animation bound, but beyond the one-field wire budget.
    let bezier = Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
    let per_track = MAX_CLIP_KEYS / 16;
    let tracks = (1..=16_u128)
        .map(|target| {
            Track::new(
                TrackKey::new(element(u128::MAX - target), Phase::In, Property::PositionX),
                (0..per_track)
                    .map(|frame| {
                        Keyframe::new(
                            u32::try_from(frame * 100).unwrap(),
                            Value::Scalar(i64::from(i32::MIN)),
                            bezier,
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let big = Clip::from_tracks(timebase(30, 1), MAX_PHASE_FRAMES, 0, tracks).unwrap();
    assert!(title_clip_wire_bytes(&big) > MAX_TITLE_ANIMATION_BYTES);
    assert!(matches!(
        encode_line(&command(CommandPayload::SetTitleAnimation {
            input: input(7),
            base_revision: 0,
            clip: Box::new(big),
        })),
        Err(CodecError::InvalidField { field: "clip", .. })
    ));
}

#[test]
fn preset_and_stretch_commands_are_exact() {
    let line = round_trip(&command(CommandPayload::ApplyTitleAnimationPreset {
        input: input(7),
        base_revision: 3,
        element: element(12),
        phase: Phase::Out,
        preset: AnimationPreset::SlideFromLeft,
        frames: 15,
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=title_animation_preset\tinput=7\tbase_revision=3\telement=12\t\
             phase=out\tpreset=slide-from-left\tframes=15\n"
        )
    );
    let line = round_trip(&command(CommandPayload::StretchTitleAnimation {
        input: input(7),
        base_revision: 3,
        phase: Phase::In,
        frames: MAX_PHASE_FRAMES,
    }));
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=title_animation_stretch\tinput=7\tbase_revision=3\tphase=in\t\
             frames=36000\n"
        )
    );
    for (bad, field) in [
        ("phase=hold\tpreset=fade\tframes=1", "phase"),
        ("phase=in\tpreset=spin\tframes=1", "preset"),
        ("phase=in\tpreset=fade\tframes=36001", "frames"),
        ("phase=in\tpreset=fade\tframes=01", "frames"),
    ] {
        let line = format!(
            "{PREFIX}\tpayload=title_animation_preset\tinput=7\tbase_revision=3\telement=12\t{bad}\n"
        );
        match decode_line(&line) {
            Err(CodecError::InvalidField { field: actual, .. }) => assert_eq!(actual, field),
            other => panic!("{bad}: {other:?}"),
        }
    }
    let zero_element = format!(
        "{PREFIX}\tpayload=title_animation_preset\tinput=7\tbase_revision=3\telement=0\t\
         phase=in\tpreset=fade\tframes=1\n"
    );
    assert!(matches!(
        decode_line(&zero_element),
        Err(CodecError::InvalidField {
            field: "element",
            ..
        })
    ));
    assert!(matches!(
        encode_line(&command(CommandPayload::StretchTitleAnimation {
            input: input(7),
            base_revision: 3,
            phase: Phase::In,
            frames: MAX_PHASE_FRAMES + 1,
        })),
        Err(CodecError::InvalidField {
            field: "frames",
            ..
        })
    ));
    let unknown = format!("{PREFIX}\tpayload=title_animation_spin\tinput=7\tbase_revision=3\n");
    assert!(decode_line(&unknown).is_err());
}

fn cursor() -> EventCursor {
    EventCursor {
        engine: EngineIdentity {
            engine_id: "engine-a".to_owned(),
            state_epoch: 7,
            log_id: "log-a".to_owned(),
        },
        revision: 12,
    }
}

#[test]
fn state_records_and_change_events_round_trip() {
    let encoded = base64url(&clip_bytes());
    let line = round_trip(&WireMessage::TitleAnimationState(
        TitleAnimationStateMessage {
            revision: 12,
            input: input(7),
            animation_revision: 5,
            clip: Box::new(clip()),
        },
    ));
    assert_eq!(
        line,
        format!(
            "title_animation_state\trevision=12\tinput=7\tanimation_revision=5\tclip={encoded}\n"
        )
    );
    let event = |inputs: Vec<WireInputId>| {
        WireMessage::Event(EventMessage {
            cursor: cursor(),
            payload: EventPayload::TitleAnimationChanged {
                inputs,
                animation_revision: 6,
                clip: Box::new(clip()),
            },
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                redo_depth: 0,
                next_undo: Some(12),
                next_redo: None,
            },
        })
    };
    let line = round_trip(&event(vec![input(7), input(9)]));
    assert!(line.contains(&format!(
        "\tevent=title_animation_changed\tinputs=7%2C9\tanimation_revision=6\tclip={encoded}\t"
    )));
    for inputs in [Vec::new(), vec![input(9), input(7)]] {
        assert!(matches!(
            encode_line(&event(inputs)),
            Err(CodecError::InvalidField {
                field: "inputs",
                ..
            })
        ));
    }
    let unordered = line.replace("inputs=7%2C9", "inputs=9%2C7");
    assert!(decode_line(&unordered).is_err());
    let unknown = line.replace("\tclip=", "\tcolour=1\tclip=");
    assert!(decode_line(&unknown).is_err());
}

fn request(sample: TitlePreviewSample, clip: Option<WireTitleClip>) -> WireMessage {
    WireMessage::TitlePreviewRequest(TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "preview-1".to_owned(),
        input: input(7),
        sample,
        width: 640,
        height: 360,
        clip: clip.map(Box::new),
    })
}

#[test]
fn preview_requests_round_trip_and_are_bounded() {
    let line = round_trip(&request(TitlePreviewSample::In { frame: 6 }, None));
    assert_eq!(
        line,
        "title_preview_request\tprotocol=2.75\trequest_id=preview-1\tinput=7\tphase=in\t\
         frame=6\twidth=640\theight=360\n"
    );
    for sample in [
        TitlePreviewSample::Static,
        TitlePreviewSample::Hold,
        TitlePreviewSample::Out {
            frame: MAX_PHASE_FRAMES,
        },
    ] {
        round_trip(&request(sample, Some(clip())));
    }
    for (bad, field) in [
        ("phase=static\tframe=1\twidth=640\theight=360", "frame"),
        ("phase=hold\tframe=2\twidth=640\theight=360", "frame"),
        ("phase=in\tframe=36001\twidth=640\theight=360", "frame"),
        ("phase=idle\tframe=0\twidth=640\theight=360", "phase"),
        ("phase=in\tframe=0\twidth=641\theight=360", "width"),
        ("phase=in\tframe=0\twidth=64\theight=0", "height"),
        ("phase=in\tframe=0\twidth=64\theight=361", "height"),
    ] {
        let line = format!("title_preview_request\tprotocol=2.75\trequest_id=p\tinput=7\t{bad}\n");
        match decode_line(&line) {
            Err(CodecError::InvalidField { field: actual, .. }) => {
                assert_eq!(actual, field, "{bad}");
            }
            other => panic!("{bad}: {other:?}"),
        }
    }
    let WireMessage::TitlePreviewRequest(mut oversized) = request(TitlePreviewSample::Hold, None)
    else {
        unreachable!()
    };
    oversized.width = 641;
    assert!(encode_line(&WireMessage::TitlePreviewRequest(oversized)).is_err());
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".into(),
        project_id: "project-9".into(),
        state_epoch: 7,
        log_id: "log-a".into(),
    }
}

/// A deterministic, poorly compressible RGBA pattern with varied alpha.
fn noise(width: u16, height: u16) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    (0..usize::from(width) * usize::from(height) * 4)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn preview_frames_carry_exact_straight_rgba() {
    // A transparent canvas with a translucent block keeps its alpha.
    let (width, height) = (40_u16, 20_u16);
    let mut rgba = vec![0_u8; usize::from(width) * usize::from(height) * 4];
    for pixel in rgba.chunks_mut(4).skip(100).take(60) {
        pixel.copy_from_slice(&[200, 30, 10, 128]);
    }
    let qoi = encode_qoi_rgba(width, height, &rgba).unwrap();
    assert_eq!(decode_qoi_rgba(width, height, &qoi).unwrap(), rgba);

    let noisy = noise(640, 360);
    let qoi = encode_qoi_rgba(640, 360, &noisy).unwrap();
    let parts = title_preview_parts(&server(), "preview-1", input(7), 5, true, 640, 360, &qoi);
    assert!(parts.len() > 1 && parts.len() <= usize::from(fm_protocol::MAX_TITLE_PREVIEW_PARTS));
    let mut assembler = TitlePreviewAssembler::new();
    let mut image = None;
    for part in &parts {
        assert!(part.data.len() <= MAX_TITLE_PREVIEW_PART_BYTES);
        let line = round_trip(&WireMessage::TitlePreviewFrame(part.clone()));
        let WireMessage::TitlePreviewFrame(decoded) = decode_line(&line).unwrap() else {
            unreachable!()
        };
        image = assembler.push(&decoded).unwrap();
    }
    let image = image.unwrap();
    assert_eq!(image.request_id, "preview-1");
    assert_eq!(image.animation_revision, 5);
    assert!(image.draft);
    assert_eq!(&image.rgba[..], &noisy[..]);

    // Out-of-order parts and a header change are refused.
    let mut assembler = TitlePreviewAssembler::new();
    assert!(assembler.push(&parts[1]).is_err());
    assembler.push(&parts[0]).unwrap();
    let changed = TitlePreviewFrameMessage {
        request_id: "preview-2".into(),
        ..parts[1].clone()
    };
    assert!(assembler.push(&changed).is_err());
    assert!(!assembler.is_in_progress());

    // A three-channel stream is not a preview.
    let rgb = fm_protocol::encode_qoi_rgb(4, 4, &[0; 48], 12, 3).unwrap();
    assert!(decode_qoi_rgba(4, 4, &rgb).is_err());
    assert!(encode_qoi_rgba(641, 1, &[0; 641 * 4]).is_err());
}
