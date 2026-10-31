//! Protocol 2.37 media-file playback (IN-003): the `media_*` commands, the
//! `media_changed` durable event, the `media` runtime event, the snapshot's
//! `media_states` count, and the `media_state` follow-on record — exactly,
//! and strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, MAX_MEDIA_EVENTS, MediaAudioPolicy, MediaAudioTrack,
    MediaEndAction, MediaEvent, MediaFault, MediaGrowingStatus, MediaOperation, MediaSeekTarget,
    MediaSettingsStatus, MediaStateMessage, MediaStatus, MediaTime, MediaTransportState,
    MediaTransportStatus, ProtocolVersion, RuntimeEventMessage, RuntimeLifecycleEvent,
    ServerIdentity, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::FrameRate;

const PREFIX: &str = "command\tprotocol=2.135\tid=a\tidempotency_key=k";

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn command(operation: MediaOperation) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::Media {
            input: input(4),
            operation,
        },
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn status() -> MediaStatus {
    MediaStatus {
        input: input(4),
        settings: MediaSettingsStatus {
            trim_in: 10,
            trim_out: Some(250),
            end_action: MediaEndAction::Loop,
            end_trigger_lead: Some(25),
            speed_basis_points: 15_000,
            audio_policy: MediaAudioPolicy::Resample,
            audio_stream: Some(2),
            growing: Some(MediaGrowingStatus {
                live_edge_offset_frames: 3,
                idle_completion_refreshes: 40,
            }),
            start_frame: 12,
        },
        transport: MediaTransportStatus {
            available: true,
            state: MediaTransportState::Playing,
            position: 60,
            position_time: MediaTime {
                numerator: 2_002,
                denominator: 1_000,
            }
            .reduced(),
            playable_end: 120,
            frames: 122,
            complete: false,
            loops: 1,
            audio_track: Some(2),
            tracks: vec![
                MediaAudioTrack {
                    stream_index: 1,
                    sample_rate: 48_000,
                    channels: 2,
                    language: Some("eng".into()),
                    title: Some("Main mix".into()),
                    default: true,
                },
                MediaAudioTrack {
                    stream_index: 2,
                    sample_rate: 48_000,
                    channels: 2,
                    language: Some("spa".into()),
                    title: None,
                    default: false,
                },
            ],
            source_rate: Some(FrameRate::new(30_000, 1_001).unwrap()),
            fault: None,
        },
    }
}

const STATUS_FIELDS: &str = "input=4\ttrim_in=10\ttrim_out=250\tend_action=loop\t\
end_trigger_lead=25\tspeed_basis_points=15000\taudio_policy=resample\taudio_stream=2\t\
growing=3%3A40\tstart_frame=12\tavailable=1\tstate=playing\tposition=60\t\
position_time=1001%2F500\tplayable_end=120\tframes=122\tcomplete=0\tloops=1\taudio_track=2\t\
tracks=1%3A48000%3A2%3A1%3A3%3Aeng%3A8%3AMain%20mix%2C2%3A48000%3A2%3A0%3A3%3Aspa%3A0%3A\t\
source_rate=30000%2F1001\tfault=-";

#[test]
fn protocol_is_2_39() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 132));
}

#[test]
fn every_media_command_has_an_exact_wire_form() {
    let cases = [
        (MediaOperation::Play, "payload=media_play\tinput=4"),
        (MediaOperation::Pause, "payload=media_pause\tinput=4"),
        (MediaOperation::Stop, "payload=media_stop\tinput=4"),
        (
            MediaOperation::Seek(MediaSeekTarget::Frame(90)),
            "payload=media_seek\tinput=4\tframe=90",
        ),
        (
            MediaOperation::Seek(MediaSeekTarget::Time {
                numerator: 3_003,
                denominator: 1_000,
            }),
            "payload=media_seek\tinput=4\ttime=3003%2F1000",
        ),
        (
            MediaOperation::CompleteSource,
            "payload=media_complete_source\tinput=4",
        ),
        (
            MediaOperation::Trim {
                trim_in: 5,
                trim_out: None,
            },
            "payload=media_trim\tinput=4\ttrim_in=5\ttrim_out=-",
        ),
        (
            MediaOperation::Rate {
                speed_basis_points: 2_500,
            },
            "payload=media_rate\tinput=4\tspeed_basis_points=2500",
        ),
        (
            MediaOperation::EndAction(MediaEndAction::Stop),
            "payload=media_end_action\tinput=4\tend_action=stop",
        ),
        (
            MediaOperation::EndTriggerLead(Some(30)),
            "payload=media_end_trigger_lead\tinput=4\tend_trigger_lead=30",
        ),
        (
            MediaOperation::AudioTrack(None),
            "payload=media_audio_track\tinput=4\taudio_track=-",
        ),
        (
            MediaOperation::AudioPolicy(MediaAudioPolicy::Mute),
            "payload=media_audio_policy\tinput=4\taudio_policy=mute",
        ),
    ];
    for (operation, fields) in cases {
        assert_eq!(
            round_trip(&command(operation)),
            format!("{PREFIX}\t{fields}\n")
        );
    }
}

#[test]
fn media_commands_are_strict() {
    for fields in [
        "payload=media_seek\tinput=4",
        "payload=media_seek\tinput=4\tframe=1\ttime=1%2F2",
        "payload=media_seek\tinput=4\ttime=1%2F0",
        "payload=media_seek\tinput=4\tframe=01",
        "payload=media_trim\tinput=4\ttrim_in=5\ttrim_out=5",
        "payload=media_trim\tinput=4\ttrim_in=5",
        "payload=media_rate\tinput=4\tspeed_basis_points=2499",
        "payload=media_rate\tinput=4\tspeed_basis_points=40001",
        "payload=media_end_action\tinput=4\tend_action=pause",
        "payload=media_end_trigger_lead\tinput=4\tend_trigger_lead=3601",
        "payload=media_audio_policy\tinput=4\taudio_policy=stretch",
        "payload=media_rewind\tinput=4",
        "payload=media_play\tinput=04",
        "payload=media_play",
        "payload=media_play\tinput=4\textra=1",
    ] {
        assert!(
            decode_line(&format!("{PREFIX}\t{fields}\n")).is_err(),
            "{fields}"
        );
    }
    assert!(
        encode_line(&command(MediaOperation::Rate {
            speed_basis_points: 50_000
        }))
        .is_err()
    );
}

#[test]
fn media_changed_carries_the_operation_and_the_projected_status() {
    let message = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 12,
        },
        payload: EventPayload::MediaChanged {
            frame: 300,
            operation: MediaOperation::Trim {
                trim_in: 10,
                trim_out: Some(250),
            },
            status: Box::new(status()),
        },
        edit_history: EditHistoryStatus {
            undo_depth: 0,
            redo_depth: 0,
            next_undo: None,
            next_redo: None,
        },
    });
    let line = round_trip(&message);
    assert!(
        line.contains(&format!(
            "\tevent=media_changed\tframe=300\toperation=trim\toperation_trim_in=10\t\
             operation_trim_out=250\t{STATUS_FIELDS}\t"
        )),
        "{line}"
    );
    let seek = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: EngineIdentity {
                engine_id: "engine-a".to_owned(),
                state_epoch: 7,
                log_id: "log-a".to_owned(),
            },
            revision: 13,
        },
        payload: EventPayload::MediaChanged {
            frame: 301,
            operation: MediaOperation::Seek(MediaSeekTarget::Frame(40)),
            status: Box::new(status()),
        },
        edit_history: EditHistoryStatus {
            undo_depth: 0,
            redo_depth: 0,
            next_undo: None,
            next_redo: None,
        },
    });
    let line = round_trip(&seek);
    assert!(
        line.contains("\toperation=seek\toperation_frame=40\t"),
        "{line}"
    );
    assert!(decode_line(&line.replace("operation=seek", "operation=rewind")).is_err());
    assert!(decode_line(&line.replace("\toperation_frame=40", "")).is_err());
}

fn runtime(events: Vec<MediaEvent>, status: MediaStatus) -> WireMessage {
    WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "engine-a".into(),
            project_id: "project-9".into(),
            state_epoch: 7,
            log_id: "log-a".into(),
        },
        revision: 12,
        generation: 1,
        sequence: 5,
        event: RuntimeLifecycleEvent::Media {
            frame: 400,
            events,
            status: Box::new(status),
        },
    })
}

#[test]
fn runtime_media_events_list_what_happened() {
    let events = vec![
        MediaEvent::EndTrigger {
            remaining_frames: 25,
        },
        MediaEvent::Ended {
            action: MediaEndAction::Loop,
        },
        MediaEvent::Looped { count: 1 },
        MediaEvent::SourceGrew { frames: 122 },
        MediaEvent::LiveEdgeReached { frame: 119 },
        MediaEvent::Resumed { frame: 119 },
        MediaEvent::SourceCompleted { frames: 130 },
        MediaEvent::Seeked { frame: 10 },
        MediaEvent::AudioTrackChanged {
            from: Some(1),
            to: Some(2),
        },
        MediaEvent::Faulted {
            fault: MediaFault::DecodeFailed,
        },
    ];
    let line = round_trip(&runtime(events, status()));
    assert!(
        line.contains(&format!(
            "\tevent=media\tframe=400\tmedia_events=end_trigger%3A25%2Cended%3Aloop%2C\
             looped%3A1%2Csource_grew%3A122%2Clive_edge_reached%3A119%2Cresumed%3A119%2C\
             source_completed%3A130%2Cseeked%3A10%2Caudio_track_changed%3A1%3A2%2C\
             faulted%3Adecode_failed\t{STATUS_FIELDS}\n"
        )),
        "{line}"
    );
    // A periodic position sample carries no events.
    let sample = round_trip(&runtime(Vec::new(), status()));
    assert!(sample.contains("\tmedia_events=\t"), "{sample}");
    assert!(
        encode_line(&runtime(
            vec![MediaEvent::Looped { count: 1 }; MAX_MEDIA_EVENTS + 1],
            status()
        ))
        .is_err()
    );
}

#[test]
fn media_state_records_follow_the_snapshot() {
    let state = WireMessage::MediaState(MediaStateMessage {
        revision: 9,
        frame: 77,
        status: status(),
    });
    assert_eq!(
        round_trip(&state),
        format!("media_state\trevision=9\tframe=77\t{STATUS_FIELDS}\n")
    );
    // An unavailable source lists nothing about the file.
    let mut missing = status();
    missing.transport = MediaTransportStatus {
        fault: Some(MediaFault::SourceMissing),
        ..MediaTransportStatus::default()
    };
    let line = round_trip(&WireMessage::MediaState(MediaStateMessage {
        revision: 9,
        frame: 77,
        status: missing,
    }));
    assert!(
        line.contains(
            "\tavailable=0\tstate=stopped\tposition=0\tposition_time=0%2F1\tplayable_end=0\t\
             frames=0\tcomplete=0\tloops=0\taudio_track=-\ttracks=\tsource_rate=-\t\
             fault=source_missing\n"
        ),
        "{line}"
    );
}

#[test]
fn media_status_is_strict() {
    let line = encode_line(&WireMessage::MediaState(MediaStateMessage {
        revision: 9,
        frame: 77,
        status: status(),
    }))
    .unwrap();
    for (from, to) in [
        ("trim_out=250", "trim_out=10"),
        ("start_frame=12", "start_frame=9"),
        ("start_frame=12", "start_frame=250"),
        ("speed_basis_points=15000", "speed_basis_points=1000"),
        ("growing=3%3A40", "growing=0%3A40"),
        ("growing=3%3A40", "growing=3"),
        ("state=playing", "state=running"),
        ("position_time=1001%2F500", "position_time=2002%2F1000"),
        ("position_time=1001%2F500", "position_time=1%2F0"),
        ("audio_track=2", "audio_track=3"),
        ("complete=0", "complete=2"),
        ("fault=-", "fault=broken"),
        ("source_rate=30000%2F1001", "source_rate=-"),
        ("%3A3%3Aspa%3A0%3A", "%3A4%3Aspa%3A0%3A"),
        ("\tloops=1", ""),
    ] {
        assert!(line.contains(from), "{from}");
        assert!(decode_line(&line.replace(from, to)).is_err(), "{to}");
    }
    // Tracks must ascend and carry no control characters.
    let mut unordered = status();
    unordered.transport.tracks.swap(0, 1);
    assert!(
        encode_line(&WireMessage::MediaState(MediaStateMessage {
            revision: 9,
            frame: 77,
            status: unordered,
        }))
        .is_err()
    );
    let mut control = status();
    control.transport.tracks[0].title = Some("a\nb".into());
    assert!(
        encode_line(&WireMessage::MediaState(MediaStateMessage {
            revision: 9,
            frame: 77,
            status: control,
        }))
        .is_err()
    );
}
