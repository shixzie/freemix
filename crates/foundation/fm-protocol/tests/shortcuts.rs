//! User shortcuts (RC-004): the `shortcut_set`, `shortcut_remove`, and
//! `shortcut_fire` commands, the `shortcut_changed`, `shortcut_removed`,
//! and `shortcut_fired` events, the snapshot's `shortcut_states` count, and
//! the `shortcut_state` follow-on record — exactly, and strictly.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    MAX_SHORTCUT_ACTIONS, ManualTransitionStatus, OverlayStatus, PlaylistRunState, RecordingStatus,
    ShortcutFireChange, ShortcutSpec, ShortcutStateMessage, ShortcutStatus,
    TransitionFavoriteEffect, WireCueAction, WireInputId, WireMessage, WireOverlayChannelId,
    WireRecorderSlot, WireShortcutAction, WireShortcutCueOperation, WireShortcutKey, decode_line,
    encode_line,
};

fn prefix() -> String {
    format!("command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid=a\tidempotency_key=k")
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

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload,
        edit_history: EditHistoryStatus::EMPTY,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn key(name: &str, command: bool, shift: bool, alt: bool) -> WireShortcutKey {
    WireShortcutKey {
        key: name.to_owned(),
        command,
        shift,
        alt,
    }
}

fn spec(action: WireShortcutAction) -> ShortcutSpec {
    ShortcutSpec {
        name: "Intro: lower third".to_owned(),
        enabled: true,
        key: Some(key("F9", false, true, false)),
        osc: Some("intro_1".to_owned()),
        action,
    }
}

fn actions() -> WireShortcutAction {
    WireShortcutAction::Actions(vec![
        WireCueAction::OverlayOn {
            channel: WireOverlayChannelId::new(1).unwrap(),
            input: input(4),
        },
        WireCueAction::Take {
            input: Some(input(3)),
            effect: TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        },
        WireCueAction::Record {
            recorder: WireRecorderSlot::new(1).unwrap(),
            start: true,
        },
    ])
}

#[test]
fn shortcut_commands_round_trip_exactly() {
    let prefix = prefix();
    let line = round_trip(&command(CommandPayload::SetShortcut {
        shortcut: 4,
        spec: Box::new(spec(actions())),
        position: None,
    }));
    assert_eq!(
        line,
        format!(
            "{prefix}\tpayload=shortcut_set\tshortcut=4\tname=Intro%3A%20lower%20third\t\
             enabled=1\tkey=F9.0.1.0\tosc=intro_1\t\
             action=actions%3Aoverlay_on.1.4%2Ftake.3.cut.0%2Frecord.1.1\n"
        )
    );
    for (action, wire) in [
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Go { cue: None },
            },
            "cue%3A2%3Ago%3A-",
        ),
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Go { cue: Some(7) },
            },
            "cue%3A2%3Ago%3A7",
        ),
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Stop,
            },
            "cue%3A2%3Astop",
        ),
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Back,
            },
            "cue%3A2%3Aback",
        ),
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Skip,
            },
            "cue%3A2%3Askip",
        ),
        (
            WireShortcutAction::Cue {
                list: 2,
                operation: WireShortcutCueOperation::Reset,
            },
            "cue%3A2%3Areset",
        ),
        (WireShortcutAction::Script { script: 9 }, "script%3A9"),
    ] {
        let mut spec = spec(action);
        spec.key = None;
        spec.osc = None;
        spec.enabled = false;
        let line = round_trip(&command(CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(spec),
            position: None,
        }));
        assert!(
            line.ends_with(&format!("\tenabled=0\tkey=-\tosc=-\taction={wire}\n")),
            "{line}"
        );
    }
    assert_eq!(
        round_trip(&command(CommandPayload::RemoveShortcut { shortcut: 4 })),
        format!("{prefix}\tpayload=shortcut_remove\tshortcut=4\n")
    );
    assert_eq!(
        round_trip(&command(CommandPayload::FireShortcut { shortcut: 9 })),
        format!("{prefix}\tpayload=shortcut_fire\tshortcut=9\n")
    );
}

#[test]
fn shortcut_commands_are_strict() {
    let prefix = prefix();
    let good = format!(
        "{prefix}\tpayload=shortcut_set\tshortcut=4\tname=A\tenabled=1\tkey=F9.0.1.0\t\
         osc=intro\taction=script%3A2\n"
    );
    assert!(decode_line(&good).is_ok());
    for (from, to) in [
        ("shortcut=4", "shortcut=0"),
        ("shortcut=4", "shortcut=04"),
        ("\tname=A", "\tname=%20"),
        ("enabled=1", "enabled=2"),
        ("enabled=1", "enabled=true"),
        ("key=F9.0.1.0", "key=F9.0.1"),
        ("key=F9.0.1.0", "key=F%2B9.0.1.0"),
        ("key=F9.0.1.0", "key=.0.1.0"),
        ("key=F9.0.1.0", "key=F9.0.1.2"),
        ("key=F9.0.1.0", "key=ABCDEFGHIJKLMNOPQ.0.0.0"),
        ("osc=intro", "osc=Intro"),
        ("osc=intro", "osc=in%2Ftro"),
        ("osc=intro", "osc="),
        ("osc=intro", "osc=abcdefghijklmnopqrstuvwxyz0123456"),
        ("action=script%3A2", "action=script%3A0"),
        ("action=script%3A2", "action=macro%3A2"),
        ("action=script%3A2", "action=cue%3A0%3Astop"),
        ("action=script%3A2", "action=cue%3A1%3Ago%3A0"),
        ("action=script%3A2", "action=cue%3A1%3Ago"),
        ("action=script%3A2", "action=cue%3A1%3Apause"),
        ("action=script%3A2", "action=cue%3A1%3Astop%3A3"),
        ("action=script%3A2", "action=actions%3A"),
        ("action=script%3A2", "action=actions%3Apreview.3%2Fwait.25"),
        (
            "action=script%3A2",
            "action=actions%3Apreview.3%2Fdelay.100",
        ),
        (
            "action=script%3A2",
            "action=actions%3Atake.3.cut.0%2Fplaylist_start",
        ),
        ("action=script%3A2", "action=actions%3Atake.3.cut.5"),
        ("\tosc=intro", ""),
        ("\n", "\textra=1\n"),
    ] {
        let broken = good.replacen(from, to, 1);
        assert!(decode_line(&broken).is_err(), "{from} -> {to}");
    }
    // At most MAX_SHORTCUT_ACTIONS actions.
    let too_many = vec!["preview.3"; MAX_SHORTCUT_ACTIONS + 1].join("%2F");
    assert!(
        decode_line(&good.replacen("script%3A2", &format!("actions%3A{too_many}"), 1)).is_err()
    );
    let enough = vec!["preview.3"; MAX_SHORTCUT_ACTIONS].join("%2F");
    assert!(decode_line(&good.replacen("script%3A2", &format!("actions%3A{enough}"), 1)).is_ok());
    // The encoder refuses what the decoder refuses.
    let mut wait = spec(WireShortcutAction::Actions(vec![WireCueAction::Wait {
        frames: 5,
    }]));
    assert!(
        encode_line(&command(CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(wait.clone()),
            position: None,
        }))
        .is_err()
    );
    wait.action = WireShortcutAction::Script { script: 1 };
    wait.osc = Some("Bad".to_owned());
    assert!(
        encode_line(&command(CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(wait),
            position: None,
        }))
        .is_err()
    );
    assert!(encode_line(&command(CommandPayload::FireShortcut { shortcut: 0 })).is_err());
    assert!(decode_line(&format!("{prefix}\tpayload=shortcut_fire\n")).is_err());
}

#[test]
fn shortcut_events_and_states_round_trip_exactly() {
    let status = ShortcutStatus {
        id: 4,
        spec: spec(actions()),
    };
    let line = round_trip(&event(EventPayload::ShortcutChanged(Box::new(
        status.clone(),
    ))));
    assert!(
        line.contains("\tevent=shortcut_changed\tshortcut=4\tname=Intro"),
        "{line}"
    );
    let line = round_trip(&event(EventPayload::ShortcutRemoved { shortcut: 4 }));
    assert!(
        line.contains("\tevent=shortcut_removed\tshortcut=4"),
        "{line}"
    );

    let change = ShortcutFireChange {
        shortcut: 4,
        frame: 1_000,
        actions: 3,
        program: input(3),
        preview: input(4),
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        overlays: OverlayStatus::empty_channels(),
        input_audio_strips: vec![fm_protocol::InputAudioStripStatus {
            input: input(3),
            gain_millidb: -6_000,
            balance_basis_points: 0,
            pan_basis_points: 0,
            group: None,
            muted: true,
            soloed: false,
            follow_video: false,
            delay_samples: 0,
        }],
        streams: Vec::new(),
        recordings: vec![
            RecordingStatus {
                recorder: WireRecorderSlot::new(1).unwrap(),
                desired_active: true,
            },
            RecordingStatus {
                recorder: WireRecorderSlot::new(2).unwrap(),
                desired_active: false,
            },
        ],
        playlist: PlaylistRunState::Stopped,
        titles: Vec::new(),
        media: Vec::new(),
        scripts: vec![fm_protocol::CueScriptRun {
            script: 2,
            actions: 1,
            instructions: 4,
        }],
    };
    let line = round_trip(&event(EventPayload::ShortcutFired(Box::new(change))));
    assert!(
        line.contains("\tevent=shortcut_fired\tshortcut=4\tframe=1000\tactions=3\t"),
        "{line}"
    );
    for (from, to) in [
        ("\tshortcut=4\t", "\tshortcut=0\t"),
        ("\tactions=3\t", "\tactions=03\t"),
        ("\tframe=1000\t", "\t"),
    ] {
        assert!(decode_line(&line.replacen(from, to, 1)).is_err(), "{from}");
    }

    let state = round_trip(&WireMessage::ShortcutState(ShortcutStateMessage {
        revision: 12,
        shortcut: status,
    }));
    assert!(
        state.starts_with("shortcut_state\trevision=12\tshortcut=4\t"),
        "{state}"
    );
    assert!(
        decode_line(
            "shortcut_state\trevision=1\tshortcut=1\tname=A\tenabled=1\tkey=-\tosc=-\t\
             action=script%3A1\n"
        )
        .is_ok()
    );
    for bad in [
        "shortcut_state\trevision=1\tshortcut=0\tname=A\tenabled=1\tkey=-\tosc=-\taction=script%3A1\n",
        "shortcut_state\trevision=1\tshortcut=1\tname=A\tenabled=1\tkey=-\tosc=-\n",
        "shortcut_state\tshortcut=1\tname=A\tenabled=1\tkey=-\tosc=-\taction=script%3A1\n",
    ] {
        assert!(decode_line(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_snapshot_announces_at_most_the_shortcut_bound() {
    let line = encode_line(&WireMessage::Snapshot(snapshot(3))).unwrap();
    assert!(
        line.contains("\tscript_states=0\tshortcut_states=3\t"),
        "{line}"
    );
    assert_eq!(
        decode_line(&line).unwrap(),
        WireMessage::Snapshot(snapshot(3))
    );
    let bound = u16::try_from(fm_protocol::MAX_SHORTCUTS).unwrap();
    assert!(encode_line(&WireMessage::Snapshot(snapshot(bound))).is_ok());
    assert!(encode_line(&WireMessage::Snapshot(snapshot(bound + 1))).is_err());
    let over = line.replacen(
        "\tshortcut_states=3\t",
        &format!("\tshortcut_states={}\t", bound + 1),
        1,
    );
    assert!(decode_line(&over).is_err());
    let missing = line.replacen("\tshortcut_states=3\t", "\t", 1);
    assert!(decode_line(&missing).is_err());
}

fn snapshot(shortcut_states: u16) -> fm_protocol::SnapshotMessage {
    use fm_protocol::{EditLockStatus, InputStatus, SnapshotMessage};
    SnapshotMessage {
        engine: EngineIdentity {
            engine_id: "e".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 12,
        show_name: "Show".to_owned(),
        inputs: [1, 2, 5, 7]
            .map(|value| InputStatus {
                input: input(value),
                name: format!("Input {value}"),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            })
            .into(),
        outputs: Vec::new(),
        input_audio_strips: [1, 2, 5, 7]
            .map(|value| fm_protocol::InputAudioStripStatus {
                input: input(value),
                gain_millidb: 0,
                balance_basis_points: 0,
                pan_basis_points: 0,
                group: None,
                muted: false,
                soloed: false,
                follow_video: true,
                delay_samples: 0,
            })
            .into(),
        desired_program: input(1),
        desired_preview: input(2),
        realized_program: input(1),
        realized_preview: input(2),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        realized_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: [1, 2]
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: false,
            })
            .into(),
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks: EditLockStatus::default(),
        edit_history: EditHistoryStatus::EMPTY,
        titles: Vec::new(),
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        paged_states: 0,
        cue_list_states: 0,
        script_states: 0,
        shortcut_states,
        still_states: 0,
        media_list_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs: Vec::new(),
        screen_capture_inputs: Vec::new(),
        capture_inputs: Vec::new(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
    }
}

/// `osc=-` is the wire's "no OSC name", so `-` itself can never be an OSC
/// name: it would encode as none and a client would lose the address the
/// project answers.
#[test]
fn a_lone_dash_is_not_an_osc_name() {
    assert!(!fm_protocol::shortcut_osc_is_valid("-"));
    assert!(fm_protocol::shortcut_osc_is_valid("-a"));
    let mut dash = spec(WireShortcutAction::Script { script: 1 });
    dash.osc = Some("-".to_owned());
    assert!(
        encode_line(&command(CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(dash),
            position: None,
        }))
        .is_err()
    );
}

/// `shortcut_set` carries an optional `position` (the index in the
/// project's shortcut list, below `MAX_SHORTCUTS`): an undone removal is
/// journaled with its old place so a replay restores the order.
#[test]
fn shortcut_set_carries_an_optional_position_exactly() {
    let prefix = prefix();
    let set = |position| {
        command(CommandPayload::SetShortcut {
            shortcut: 4,
            spec: Box::new(spec(WireShortcutAction::Script { script: 2 })),
            position,
        })
    };
    let without = round_trip(&set(None));
    assert!(!without.contains("position="), "{without}");
    let line = round_trip(&set(Some(2)));
    assert!(line.ends_with("\tposition=2\n"), "{line}");
    let last = u8::try_from(fm_protocol::MAX_SHORTCUTS - 1).unwrap();
    let line = round_trip(&set(Some(last)));
    for bad in [
        format!("position={}", fm_protocol::MAX_SHORTCUTS),
        "position=02".to_owned(),
        "position=-1".to_owned(),
        "position=".to_owned(),
        "position=256".to_owned(),
    ] {
        let line = line.replacen(&format!("position={last}"), &bad, 1);
        assert!(decode_line(&line).is_err(), "{line}");
    }
    assert!(encode_line(&set(Some(last + 1))).is_err());
    assert!(
        decode_line(&format!(
            "{prefix}\tpayload=shortcut_remove\tshortcut=4\tposition=1\n"
        ))
        .is_err()
    );
}
