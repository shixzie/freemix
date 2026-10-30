//! Protocol 2.34 animation preset library: the `animation_preset_add`,
//! `animation_preset_rename`, and `animation_preset_remove` commands, the
//! user preset applications `title_animation_user_preset` and
//! `scene_animation_user_preset`, the `animation_preset_state` record, and
//! the `animation_preset_changed` event round-trip exactly and refuse
//! everything outside the contract.

use core::num::{NonZeroU32, NonZeroU128};

use fm_animation::{
    Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Timebase, Track,
    TrackKey, Value,
};
use fm_protocol::{
    AnimationPresetChange, AnimationPresetEntry, AnimationPresetStateMessage,
    CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, MAX_ANIMATION_PRESET_NAME_BYTES,
    MAX_ANIMATION_PRESETS, WireInputId, WireMessage, WireSceneLayerId, WireTitleElementId,
    decode_line, encode_line, motion_wire_bytes,
};
use fm_types::FrameRate;

const PREFIX: &str = "command\tprotocol=2.126\tid=a\tidempotency_key=k";

fn id(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
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

/// A 10-frame slide in from x -100 at 25 fps, authored against 400x100.
fn motion() -> PhaseMotion {
    let clip = MotionClip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        10,
        0,
        vec![Track::new(
            TrackKey::new(MotionTarget, Phase::In, Property::PositionX),
            vec![
                Keyframe::new(0, Value::Scalar(-100), Easing::Linear),
                Keyframe::new(10, Value::Scalar(0), Easing::Linear),
            ],
        )],
    )
    .unwrap();
    PhaseMotion::new(clip, 400, 100).unwrap()
}

/// The exact encoding of [`motion`]: the reference box as varints, then the
/// title clip encoding with the single target `1`.
fn motion_bytes() -> Vec<u8> {
    let mut bytes = vec![0x90, 0x03, 100];
    bytes.extend_from_slice(&[25, 1, 10, 0, 1]);
    bytes.extend_from_slice(&[1, 0, 0, 2, 0, 0xc7, 0x01, 0, 10, 0, 0]);
    bytes
}

fn entry(name: &str) -> AnimationPresetEntry {
    AnimationPresetEntry {
        preset: id(3),
        name: name.to_owned(),
        phase: Phase::Out,
        motion: Box::new(motion()),
    }
}

#[test]
fn library_commands_are_exact() {
    let encoded = base64url(&motion_bytes());
    assert_eq!(motion_wire_bytes(&motion()), motion_bytes().len());
    let add = |index| {
        command(CommandPayload::AddAnimationPreset {
            preset: id(3),
            name: "Slide in".to_owned(),
            phase: Phase::Out,
            motion: Box::new(motion()),
            index,
        })
    };
    assert_eq!(
        round_trip(&add(None)),
        format!(
            "{PREFIX}\tpayload=animation_preset_add\tpreset_id=3\tname=Slide%20in\tphase=out\t\
             motion={encoded}\n"
        )
    );
    assert!(round_trip(&add(Some(2))).ends_with(&format!("motion={encoded}\tindex=2\n")));
    assert!(matches!(
        encode_line(&add(Some(MAX_ANIMATION_PRESETS))),
        Err(CodecError::InvalidField { field: "index", .. })
    ));
    assert_eq!(
        round_trip(&command(CommandPayload::RenameAnimationPreset {
            preset: id(3),
            name: "Swoosh".to_owned(),
        })),
        format!("{PREFIX}\tpayload=animation_preset_rename\tpreset_id=3\tname=Swoosh\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::RemoveAnimationPreset {
            preset: id(3)
        })),
        format!("{PREFIX}\tpayload=animation_preset_remove\tpreset_id=3\n")
    );

    // Blank, control-character, and oversized names, preset 0, and malformed motions
    // are refused.
    let long = "x".repeat(MAX_ANIMATION_PRESET_NAME_BYTES + 1);
    for name in ["", "%20", "a%09b", long.as_str()] {
        let line = format!("{PREFIX}\tpayload=animation_preset_rename\tpreset_id=3\tname={name}\n");
        assert!(decode_line(&line).is_err(), "{name}");
    }
    assert!(
        decode_line(&format!(
            "{PREFIX}\tpayload=animation_preset_remove\tpreset_id=0\n"
        ))
        .is_err()
    );
    let mut zero_box = motion_bytes();
    zero_box[2] = 0;
    let mut out_track = motion_bytes();
    out_track[9] = 1;
    let mut trailing = motion_bytes();
    trailing.push(0);
    for bytes in [zero_box, out_track, trailing, vec![0x90]] {
        let line = format!(
            "{PREFIX}\tpayload=animation_preset_add\tpreset_id=3\tname=a\tphase=in\tmotion={}\n",
            base64url(&bytes)
        );
        assert!(
            matches!(
                decode_line(&line),
                Err(CodecError::InvalidField {
                    field: "motion",
                    ..
                })
            ),
            "{bytes:?}"
        );
    }
}

#[test]
fn user_preset_applications_are_exact() {
    let title = round_trip(&command(CommandPayload::ApplyTitleAnimationUserPreset {
        input: input(7),
        base_revision: 3,
        element: WireTitleElementId::new(NonZeroU128::new(12).unwrap()),
        phase: Phase::In,
        preset: id(4),
        frames: 15,
    }));
    assert_eq!(
        title,
        format!(
            "{PREFIX}\tpayload=title_animation_user_preset\tinput=7\tbase_revision=3\t\
             element=12\tphase=in\tpreset_id=4\tframes=15\n"
        )
    );
    let scene = round_trip(&command(CommandPayload::ApplySceneAnimationUserPreset {
        input: input(7),
        base_revision: 3,
        layer: WireSceneLayerId::new(id(2)),
        phase: Phase::Out,
        preset: id(4),
        frames: 0,
    }));
    assert_eq!(
        scene,
        format!(
            "{PREFIX}\tpayload=scene_animation_user_preset\tinput=7\tbase_revision=3\t\
             layer=2\tphase=out\tpreset_id=4\tframes=0\n"
        )
    );
    for bad in ["preset_id=0", "phase=hold", "frames=x"] {
        let field = bad.split('=').next().unwrap();
        let line = scene.replacen(
            &format!(
                "{field}={}",
                scene
                    .split(&format!("\t{field}="))
                    .nth(1)
                    .unwrap()
                    .split(['\t', '\n'])
                    .next()
                    .unwrap()
            ),
            bad,
            1,
        );
        assert!(decode_line(&line).is_err(), "{line}");
    }
}

#[test]
fn state_records_and_change_events_round_trip() {
    let encoded = base64url(&motion_bytes());
    let line = round_trip(&WireMessage::AnimationPresetState(
        AnimationPresetStateMessage {
            revision: 12,
            entry: entry("Slide in"),
        },
    ));
    assert_eq!(
        line,
        format!(
            "animation_preset_state\trevision=12\tpreset_id=3\tname=Slide%20in\tphase=out\t\
             motion={encoded}\n"
        )
    );
    let event = |change| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "engine-a".to_owned(),
                    state_epoch: 7,
                    log_id: "log-a".to_owned(),
                },
                revision: 12,
            },
            payload: EventPayload::AnimationPresetChanged(change),
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                redo_depth: 0,
                next_undo: Some(12),
                next_redo: None,
            },
        })
    };
    let set = round_trip(&event(AnimationPresetChange::Set {
        index: 1,
        entry: entry("Slide in"),
    }));
    assert!(set.contains(&format!(
        "\tevent=animation_preset_changed\tchange=set\tindex=1\tpreset_id=3\tname=Slide%20in\t\
         phase=out\tmotion={encoded}\t"
    )));
    let removed = round_trip(&event(AnimationPresetChange::Removed { preset: id(3) }));
    assert!(removed.contains("\tevent=animation_preset_changed\tchange=removed\tpreset_id=3\t"));
    assert!(matches!(
        encode_line(&event(AnimationPresetChange::Set {
            index: MAX_ANIMATION_PRESETS,
            entry: entry("Slide in"),
        })),
        Err(CodecError::InvalidField { field: "index", .. })
    ));
    assert!(decode_line(&set.replace("change=set", "change=moved")).is_err());
}
