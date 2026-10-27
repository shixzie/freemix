//! Protocol 2.36 transition family (SW-002, SW-005): the `transition`
//! command takes every automatic mix by kind, overlay channels accept every
//! motion transition, and the four favourite transition buttons travel in
//! the snapshot, the `transition_favorite_set` command, and the
//! `transition_favorite_changed` event — exactly, and strictly.

use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CodecError, CommandMessage, CommandPayload,
    EditHistoryStatus, EngineIdentity, EventCursor, EventMessage, EventPayload,
    OverlayTransitionKind, ProtocolVersion, TransitionFavoriteEffect, TransitionFavoriteStatus,
    WireMessage, WireOverlayChannelId, WireStingerSlotId, decode_line, encode_line,
};

const PREFIX: &str = "command\tprotocol=2.111\tid=a\tidempotency_key=k";

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

#[test]
fn protocol_is_2_39() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 111));
}

#[test]
fn every_mix_kind_is_taken_by_kind() {
    let tokens = [
        "fade",
        "alpha_fade",
        "wipe",
        "vertical_wipe",
        "slide",
        "vertical_slide",
        "zoom",
        "fly",
        "fly_rotate",
        "cube",
        "cube_zoom",
        "cross_zoom",
        "merge",
    ];
    assert_eq!(AutoTransitionKind::ALL.len(), tokens.len());
    for (kind, token) in AutoTransitionKind::ALL.into_iter().zip(tokens) {
        assert_eq!(
            round_trip(&command(CommandPayload::Transition {
                kind,
                duration_frames: 25,
            })),
            format!("{PREFIX}\tpayload=transition\tkind={token}\tduration_frames=25\n")
        );
    }
    let line = format!("{PREFIX}\tpayload=transition\tkind=cube\tduration_frames=25\n");
    for (from, to) in [
        ("kind=cube", "kind=stinger"),
        ("kind=cube", "kind=cut"),
        ("kind=cube", "kind=Cube"),
        ("\tkind=cube", ""),
        ("\tduration_frames=25", ""),
    ] {
        assert!(decode_line(&line.replace(from, to)).is_err(), "{to}");
    }
}

#[test]
fn overlay_channels_accept_every_motion_transition() {
    let channel = WireOverlayChannelId::new(3).unwrap();
    for transition in OverlayTransitionKind::ALL {
        let line = round_trip(&command(CommandPayload::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames: 12,
        }));
        assert_eq!(
            line,
            format!(
                "{PREFIX}\tpayload=overlay_transition\tchannel=3\ttransition={}\t\
                 duration_frames=12\n",
                transition.token()
            )
        );
        assert_eq!(
            OverlayTransitionKind::from_token(transition.token()),
            Some(transition)
        );
    }
    assert_eq!(OverlayTransitionKind::from_token("alpha_fade"), None);
    let line = format!(
        "{PREFIX}\tpayload=overlay_transition\tchannel=3\ttransition=fly_rotate\tduration_frames=12\n"
    );
    assert!(decode_line(&line.replace("fly_rotate", "fly-rotate")).is_err());
    assert!(decode_line(&line.replace("fly_rotate", "stinger")).is_err());
}

fn stinger(slot: u8, duration_frames: u32) -> TransitionFavoriteStatus {
    TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(slot).unwrap()),
        duration_frames,
    }
}

#[test]
fn favourite_assignments_are_exact_and_bounded() {
    let set = |slot, favorite| command(CommandPayload::SetTransitionFavorite { slot, favorite });
    assert_eq!(
        round_trip(&set(4, stinger(2, 45))),
        format!("{PREFIX}\tpayload=transition_favorite_set\tslot=4\tfavorite=stinger_2%3A45\n")
    );
    let cut = TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Cut,
        duration_frames: 1,
    };
    assert!(round_trip(&set(1, cut)).ends_with("\tslot=1\tfavorite=cut%3A1\n"));
    let merge = TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Merge),
        duration_frames: 3_600,
    };
    assert!(round_trip(&set(2, merge)).ends_with("\tslot=2\tfavorite=merge%3A3600\n"));
    for slot in [0, 5] {
        assert!(matches!(
            encode_line(&set(slot, merge)),
            Err(CodecError::InvalidField { field: "slot", .. })
        ));
    }
    for duration_frames in [0, 3_601] {
        let favorite = TransitionFavoriteStatus {
            duration_frames,
            ..merge
        };
        assert!(matches!(
            encode_line(&set(2, favorite)),
            Err(CodecError::InvalidField {
                field: "favorite",
                ..
            })
        ));
    }
    let line =
        format!("{PREFIX}\tpayload=transition_favorite_set\tslot=4\tfavorite=stinger_2%3A45\n");
    for (from, to) in [
        ("slot=4", "slot=5"),
        ("slot=4", "slot=04"),
        ("stinger_2", "stinger_9"),
        ("stinger_2", "stinger"),
        ("%3A45", "%3A0"),
        ("%3A45", ""),
        ("\tslot=4", ""),
    ] {
        assert!(decode_line(&line.replace(from, to)).is_err(), "{to}");
    }
}

#[test]
fn favourite_changes_carry_the_slot_and_its_favourite() {
    let message = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload: EventPayload::TransitionFavoriteChanged {
            slot: 3,
            favorite: stinger(8, 20),
        },
        edit_history: EditHistoryStatus {
            undo_depth: 1,
            redo_depth: 0,
            next_undo: Some(12),
            next_redo: None,
        },
    });
    let line = round_trip(&message);
    assert!(
        line.contains("\tevent=transition_favorite_changed\tslot=3\tfavorite=stinger_8%3A20\t"),
        "{line}"
    );
    assert!(decode_line(&line.replace("slot=3", "slot=0")).is_err());
    assert!(decode_line(&line.replace("stinger_8", "stinger_0")).is_err());
}

#[test]
fn default_favourites_are_fade_wipe_fly_and_cube() {
    assert_eq!(
        TransitionFavoriteStatus::defaults().map(|favorite| favorite.effect.token()),
        ["fade", "wipe", "fly", "cube"]
    );
    assert!(
        TransitionFavoriteStatus::defaults()
            .iter()
            .all(|favorite| favorite.duration_frames == 30)
    );
}
