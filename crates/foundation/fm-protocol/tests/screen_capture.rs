//! Screen and window capture inputs (IN-011): `input_add` with
//! `kind=screen_capture`, the `input_added` durable event, the
//! `screen_capture_status` runtime event, the snapshot's
//! `screen_capture_inputs`, the `screen_capture:` replacement source, and
//! the `screen_capture_targets_request` / `screen_capture_targets`
//! exchange — exactly, and strictly — including each input's audio (none,
//! system, or the window's application) and its audio state.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, EditHistoryStatus, EngineIdentity,
    EventCursor, EventMessage, EventPayload, InputLifecycleOperation, LiveInputSource,
    MAX_SCREEN_CAPTURE_ERROR_BYTES, MAX_SCREEN_CAPTURE_INPUTS, MAX_SCREEN_CAPTURE_TITLE_BYTES,
    MAX_SCREEN_CAPTURE_WINDOWS, RuntimeEventMessage, RuntimeLifecycleEvent, ScreenCaptureAudio,
    ScreenCaptureAudioState, ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureEntry,
    ScreenCaptureInventory, ScreenCapturePickRequest, ScreenCaptureState, ScreenCaptureStatus,
    ScreenCaptureTarget, ScreenCaptureTargetsMessage, ScreenCaptureTargetsRequest, ScreenInfo,
    ServerIdentity, TitleMatch, WindowInfo, WindowRule, WireInputId, WireInputSource, WireMessage,
    decode_line, encode_line,
};
use fm_types::network_stream::StallSlate;

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
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
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: EditHistoryStatus::EMPTY,
        payload,
    })
}

fn runtime(event: RuntimeLifecycleEvent) -> WireMessage {
    WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: ServerIdentity {
            engine_id: "e".into(),
            project_id: "p".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        revision: 4,
        generation: 1,
        sequence: 9,
        event,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

/// A field value with its `%XX` escapes decoded.
fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap();
            decoded.push(u8::from_str_radix(hex, 16).unwrap());
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).unwrap()
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn slides() -> ScreenCaptureConfig {
    ScreenCaptureConfig {
        target: ScreenCaptureTarget::Window(WindowRule {
            app: "Keynote".into(),
            title: "Deck~1, final".into(),
            title_match: TitleMatch::Contains,
            window_id: Some(4_242),
        }),
        crop: ScreenCaptureCrop {
            left: 0,
            top: 28,
            right: 0,
            bottom: 2,
        },
        cursor: false,
        fps: 25,
        slate: StallSlate::Black,
        audio: ScreenCaptureAudio::Application,
    }
}

fn add(config: ScreenCaptureConfig) -> WireMessage {
    command(CommandPayload::AddInput {
        input: input(5),
        name: "Slides".into(),
        source: LiveInputSource::ScreenCapture(Box::new(config)),
    })
}

#[test]
fn the_add_command_has_an_exact_wire_form_and_is_strict() {
    let line = round_trip(&add(slides()));
    let prefix = format!(
        "command\tprotocol={}.{}\tid=a\tidempotency_key=k",
        CURRENT_PROTOCOL_VERSION.major, CURRENT_PROTOCOL_VERSION.minor
    );
    assert_eq!(
        line,
        format!(
            "{prefix}\tpayload=input_add\tinput=5\tname=Slides\tkind=screen_capture\ttarget=window\
             \tapp=Keynote\ttitle=Deck%7E1%2C%20final\ttitle_match=contains\twindow_id=4242\
             \tcrop=0.28.0.2\tcursor=0\tfps=25\tstall=black\taudio=app\n"
        )
    );
    let screen = round_trip(&add(ScreenCaptureConfig::new(
        ScreenCaptureTarget::Screen { index: 1 },
    )));
    assert!(
        screen.ends_with(
            "\tkind=screen_capture\ttarget=screen\tscreen=1\tcrop=0.0.0.0\tcursor=1\tfps=30\tstall=freeze\taudio=none\n"
        ),
        "{screen}"
    );
    // Desktop audio fits a screen; an application's audio needs a window.
    let desktop = ScreenCaptureConfig {
        audio: ScreenCaptureAudio::System,
        ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 1 })
    };
    assert!(round_trip(&add(desktop)).ends_with("\tstall=freeze\taudio=system\n"));
    let screen_app = ScreenCaptureConfig {
        audio: ScreenCaptureAudio::Application,
        ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 1 })
    };
    assert!(encode_line(&add(screen_app)).is_err());
    refused(&screen.replacen("audio=none", "audio=app", 1));
    refused(&line.replacen("audio=app", "audio=application", 1));
    refused(&line.replacen("audio=app", "audio=App", 1));
    refused(&line.replacen("\taudio=app", "", 1));
    // Without an id hint the window rule still round-trips.
    let mut unhinted = slides();
    if let ScreenCaptureTarget::Window(rule) = &mut unhinted.target {
        rule.window_id = None;
    }
    assert!(!round_trip(&add(unhinted)).contains("window_id"));

    refused(&line.replacen("\ttarget=window", "", 1));
    refused(&line.replacen("target=window", "target=region", 1));
    refused(&line.replacen("\tapp=Keynote", "", 1));
    refused(&line.replacen("title_match=contains", "title_match=regex", 1));
    refused(&line.replacen("window_id=4242", "window_id=0", 1));
    refused(&line.replacen("window_id=4242", "window_id=04242", 1));
    refused(&line.replacen("crop=0.28.0.2", "crop=0.28.0", 1));
    refused(&line.replacen("crop=0.28.0.2", "crop=0.16385.0.0", 1));
    refused(&line.replacen("cursor=0", "cursor=2", 1));
    refused(&line.replacen("fps=25", "fps=0", 1));
    refused(&line.replacen("fps=25", "fps=61", 1));
    refused(&line.replacen("stall=black", "stall=slate", 1));
    refused(&line.replacen('\n', "\textra=1\n", 1));
    refused(&screen.replacen("screen=1", "screen=64", 1));
    // A rule that matches nothing in particular is not encoded (the rule
    // with no application, no title, and no id is the window picked on the
    // desktop instead, see `desktop_picked_captures_have_exact_wire_forms`).
    let anything = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule {
        app: String::new(),
        title: String::new(),
        title_match: TitleMatch::Any,
        window_id: Some(3),
    }));
    assert!(encode_line(&add(anything)).is_err());
    let mut long = slides();
    if let ScreenCaptureTarget::Window(rule) = &mut long.target {
        rule.title = "t".repeat(MAX_SCREEN_CAPTURE_TITLE_BYTES + 1);
    }
    assert!(encode_line(&add(long)).is_err());
}

#[test]
fn the_added_event_carries_one_kind() {
    let added = event(EventPayload::InputAdded {
        input: input(5),
        name: "Slides".into(),
        network_stream: None,
        capture: None,
        system_audio: None,
        still: None,
        screen_capture: Some(Box::new(slides())),
    });
    let line = round_trip(&added);
    assert!(
        line.contains("\tkind=screen_capture\ttarget=window\tapp=Keynote"),
        "{line}"
    );
    refused(&line.replacen("\tcrop=0.28.0.2", "", 1));
    let both = event(EventPayload::InputAdded {
        input: input(5),
        name: "Slides".into(),
        network_stream: None,
        capture: Some(Box::new(fm_protocol::CaptureInputConfig::new("cam"))),
        system_audio: None,
        still: None,
        screen_capture: Some(Box::new(slides())),
    });
    assert!(encode_line(&both).is_err());
}

#[test]
#[allow(clippy::too_many_lines)] // every picture and audio state, exactly and strictly
fn status_events_are_exact_and_bounded() {
    let gone = runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            state: ScreenCaptureState::TargetGone,
            reopens: 2,
            width: 0,
            height: 0,
            window_id: None,
            last_error: Some("no Keynote window titled \"Deck\" is open".into()),
            audio: ScreenCaptureAudioState::Off,
            audio_error: None,
        }),
    });
    let line = round_trip(&gone);
    assert!(
        line.ends_with(
            "\tevent=screen_capture_status\tinput=5\tstate=target_gone\treopens=2\twidth=0\theight=0\
             \tlast_error=no%20Keynote%20window%20titled%20%22Deck%22%20is%20open\taudio=off\n"
        ),
        "{line}"
    );
    let live_line = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            state: ScreenCaptureState::Live,
            reopens: 0,
            width: 1_280,
            height: 720,
            window_id: Some(4_242),
            last_error: None,
            audio: ScreenCaptureAudioState::Live,
            audio_error: None,
        }),
    }));
    assert!(
        live_line.ends_with(
            "\tinput=5\tstate=live\treopens=0\twidth=1280\theight=720\twindow_id=4242\taudio=live\n"
        ),
        "{live_line}"
    );
    // Every audio state round-trips, with its reason when it has one.
    for audio in ScreenCaptureAudioState::ALL {
        let audio_error = (audio != ScreenCaptureAudioState::Off)
            .then(|| "per-application audio needs Windows 10 version 2004 or later".to_owned());
        let line = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
            input: input(5),
            status: Box::new(ScreenCaptureStatus {
                state: ScreenCaptureState::Live,
                audio,
                audio_error,
                ..ScreenCaptureStatus::default()
            }),
        }));
        assert!(
            line.contains(&format!("\taudio={}", audio.token())),
            "{line}"
        );
    }
    let waiting = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            state: ScreenCaptureState::Live,
            audio: ScreenCaptureAudioState::NoStream,
            audio_error: Some("no stream".into()),
            ..ScreenCaptureStatus::default()
        }),
    }));
    assert!(
        waiting.ends_with("\taudio=no_stream\taudio_error=no%20stream\n"),
        "{waiting}"
    );
    refused(&waiting.replacen("audio=no_stream", "audio=waiting", 1));
    refused(&waiting.replacen("audio=no_stream", "audio=off", 1));
    refused(&waiting.replacen("\taudio=no_stream", "", 1));
    refused(&waiting.replacen("audio_error=no%20stream", "audio_error=", 1));
    let off_with_error = runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            audio_error: Some("stale".into()),
            ..ScreenCaptureStatus::default()
        }),
    });
    assert!(encode_line(&off_with_error).is_err());
    let long_audio = runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            audio: ScreenCaptureAudioState::Failed,
            audio_error: Some("e".repeat(MAX_SCREEN_CAPTURE_ERROR_BYTES + 1)),
            ..ScreenCaptureStatus::default()
        }),
    });
    assert!(encode_line(&long_audio).is_err());
    let reopening = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            state: ScreenCaptureState::Reopening {
                attempt: 3,
                next_in_ms: 2_000,
            },
            reopens: 3,
            ..ScreenCaptureStatus::default()
        }),
    }));
    assert!(
        reopening.contains("\tstate=reopening\tattempt=3\tnext_in_ms=2000\treopens=3\t"),
        "{reopening}"
    );
    for state in [
        ScreenCaptureState::Idle,
        ScreenCaptureState::Opening,
        ScreenCaptureState::Unsupported,
        ScreenCaptureState::Failed,
    ] {
        let line = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
            input: input(5),
            status: Box::new(ScreenCaptureStatus {
                state,
                ..ScreenCaptureStatus::default()
            }),
        }));
        // Only a reopening input carries its attempt.
        refused(&line.replacen("\treopens=0", "\tattempt=1\tnext_in_ms=0\treopens=0", 1));
    }
    refused(&reopening.replacen("\tattempt=3", "\tattempt=0", 1));
    refused(&reopening.replacen("\tattempt=3", "", 1));
    refused(&line.replacen("state=target_gone", "state=gone", 1));
    refused(&live_line.replacen("window_id=4242", "window_id=0", 1));
    refused(&live_line.replacen("width=1280", "width=01280", 1));
    let long = runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            last_error: Some("e".repeat(MAX_SCREEN_CAPTURE_ERROR_BYTES + 1)),
            ..ScreenCaptureStatus::default()
        }),
    });
    assert!(encode_line(&long).is_err());
    let blank = runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
        input: input(5),
        status: Box::new(ScreenCaptureStatus {
            last_error: Some(String::new()),
            ..ScreenCaptureStatus::default()
        }),
    });
    assert!(encode_line(&blank).is_err());
}

#[test]
fn the_replacement_source_round_trips() {
    for config in [
        slides(),
        ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 2 }),
        ScreenCaptureConfig {
            audio: ScreenCaptureAudio::System,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 2 })
        },
    ] {
        let audio = config.audio.token();
        let replace = command(CommandPayload::InputLifecycle(
            InputLifecycleOperation::ReplaceSource {
                input: input(2),
                source: WireInputSource::ScreenCapture(config),
            },
        ));
        let line = round_trip(&replace);
        assert!(line.contains("screen_capture"), "{line}");
        refused(
            &line
                .replacen("%257Ewindow%257E", "%257Eregion%257E", 1)
                .replacen("%257Escreen%257E", "%257Eregion%257E", 1),
        );
        refused(&line.replacen(&format!("%257E{audio}%257E"), "%257Eloud%257E", 1));
    }
    // A screen never takes an application's audio.
    let screen_app = command(CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: input(2),
            source: WireInputSource::ScreenCapture(ScreenCaptureConfig::new(
                ScreenCaptureTarget::Screen { index: 2 },
            )),
        },
    ));
    let line = round_trip(&screen_app);
    refused(&line.replacen("%257Enone%257Escreen", "%257Eapp%257Escreen", 1));
}

fn entry(
    value: u128,
    config: ScreenCaptureConfig,
    status: ScreenCaptureStatus,
) -> ScreenCaptureEntry {
    ScreenCaptureEntry {
        input: input(value),
        config,
        status,
    }
}

#[test]
fn snapshot_entries_round_trip_and_are_bounded() {
    let entries = vec![
        entry(
            5,
            slides(),
            ScreenCaptureStatus {
                state: ScreenCaptureState::Live,
                width: 1_280,
                height: 720,
                window_id: Some(4_242),
                audio: ScreenCaptureAudioState::NoStream,
                ..ScreenCaptureStatus::default()
            },
        ),
        entry(
            7,
            ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 }),
            ScreenCaptureStatus {
                state: ScreenCaptureState::Reopening {
                    attempt: 2,
                    next_in_ms: 1_000,
                },
                reopens: 2,
                ..ScreenCaptureStatus::default()
            },
        ),
    ];
    let line = encode_line(&snapshot(entries.clone())).unwrap();
    let WireMessage::Snapshot(decoded) = decode_line(&line).unwrap() else {
        panic!("{line}");
    };
    assert_eq!(decoded.screen_capture_inputs, entries);
    let value = line
        .split('\t')
        .find_map(|field| field.strip_prefix("screen_capture_inputs="))
        .unwrap();
    let value = unescape(value);
    assert_eq!(
        value,
        "5~live~0~0~0~1280~720~4242~no_stream~0.28.0.2~0~25~black~app~window~contains~4242~7~13:\
         KeynoteDeck~1, final,\
         7~reopening~2~1000~2~0~0~-~off~0.0.0.0~1~30~freeze~none~screen~0"
    );
    // Ascending, without error text, and bounded.
    let mut descending = entries.clone();
    descending.reverse();
    assert!(encode_line(&snapshot(descending)).is_err());
    let mut with_error = entries.clone();
    with_error[0].status.last_error = Some("boom".into());
    assert!(encode_line(&snapshot(with_error)).is_err());
    let mut with_audio_error = entries.clone();
    with_audio_error[0].status.audio_error = Some("boom".into());
    assert!(encode_line(&snapshot(with_audio_error)).is_err());
    refused(&line.replacen("%7Eno_stream%7E", "%7Ewaiting%7E", 1));
    refused(&line.replacen("%7Eno_stream%7E", "%7E", 1));
    let too_many = (1..=u128::try_from(MAX_SCREEN_CAPTURE_INPUTS).unwrap() + 1)
        .map(|value| {
            entry(
                value,
                ScreenCaptureConfig::default(),
                ScreenCaptureStatus::default(),
            )
        })
        .collect();
    assert!(encode_line(&snapshot(too_many)).is_err());
    // A wrong text length is refused.
    refused(&line.replacen("%7E13%3AKeynote", "%7E12%3AKeynote", 1));
}

fn snapshot(screen_capture_inputs: Vec<ScreenCaptureEntry>) -> WireMessage {
    use fm_protocol::{
        EditLockStatus, FadeToBlackPosition, FadeToBlackState, InputStatus, ManualTransitionStatus,
        OverlayStatus, RecordingStatus, SnapshotMessage, WireRecorderSlot,
    };
    WireMessage::Snapshot(SnapshotMessage {
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
                buses: fm_types::audio_bus::AudioBusMask::MASTER,
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
        shortcut_states: 0,
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
        screen_capture_inputs,
        capture_inputs: Vec::new(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
        alert_rules: Vec::new(),
        transitions_in_flight: Vec::new(),
    })
}

fn inventory() -> ScreenCaptureInventory {
    ScreenCaptureInventory {
        supported: true,
        backend: "x11".into(),
        screens: vec![
            ScreenInfo {
                index: 0,
                name: "DP-1".into(),
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
                primary: true,
            },
            ScreenInfo {
                index: 1,
                name: "HDMI~1, left".into(),
                x: -1_280,
                y: 0,
                width: 1_280,
                height: 1_024,
                primary: false,
            },
        ],
        windows: vec![
            WindowInfo {
                id: 4_242,
                app: "Keynote".into(),
                title: "Deck~1, final".into(),
                pid: Some(501),
                x: 10,
                y: -40,
                width: 1_280,
                height: 750,
            },
            WindowInfo {
                id: 77,
                app: "XTerm".into(),
                title: String::new(),
                pid: None,
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            },
        ],
        error: None,
    }
}

fn targets(request_id: &str, inventory: ScreenCaptureInventory) -> WireMessage {
    WireMessage::ScreenCaptureTargets(ScreenCaptureTargetsMessage {
        request_id: request_id.into(),
        inventory,
    })
}

#[test]
fn the_target_list_exchange_is_exact() {
    let request = WireMessage::ScreenCaptureTargetsRequest(ScreenCaptureTargetsRequest {
        request_id: "sc-1".into(),
    });
    assert_eq!(
        round_trip(&request),
        "screen_capture_targets_request\trequest_id=sc-1\n"
    );
    let line = round_trip(&targets("sc-1", inventory()));
    let fields: Vec<(&str, String)> = line
        .trim_end()
        .split('\t')
        .skip(1)
        .map(|field| {
            let (name, value) = field.split_once('=').unwrap();
            (name, unescape(value))
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("request_id", "sc-1".to_owned()),
            ("supported", "1".to_owned()),
            ("backend", "x11".to_owned()),
            (
                "screens",
                "0~1~0~0~1920~1080~4:DP-1,1~0~-1280~0~1280~1024~12:HDMI~1, left".to_owned()
            ),
            (
                "windows",
                "4242~501~10~-40~1280~750~7~13:KeynoteDeck~1, final,77~-~0~0~640~480~5~0:XTerm"
                    .to_owned()
            ),
        ]
    );
    let unsupported = round_trip(&targets(
        "r",
        ScreenCaptureInventory::unsupported("Wayland session: use X11"),
    ));
    assert_eq!(
        unsupported,
        "screen_capture_targets\trequest_id=r\tsupported=0\tbackend=\terror=Wayland%20session%3A%20use%20X11\tscreens=\twindows=\n"
    );
    // The longest lists fit one record.
    let mut full = inventory();
    full.windows = (1..=u64::try_from(MAX_SCREEN_CAPTURE_WINDOWS).unwrap())
        .map(|id| WindowInfo {
            id,
            app: "a".repeat(fm_protocol::MAX_SCREEN_CAPTURE_APP_BYTES),
            title: "é".repeat(MAX_SCREEN_CAPTURE_TITLE_BYTES / 2),
            pid: Some(u32::MAX),
            x: i32::MIN,
            y: i32::MIN,
            width: 16_384,
            height: 16_384,
        })
        .collect();
    assert_eq!(full.fitted(), full, "the longest list is already fitted");
    round_trip(&targets("r", full));
}

#[test]
fn target_lists_that_break_the_contract_are_refused() {
    let encodes = |inventory: ScreenCaptureInventory| encode_line(&targets("r", inventory)).is_ok();
    let mut descending = inventory();
    descending.screens.swap(0, 1);
    assert!(!encodes(descending));
    let mut duplicate = inventory();
    duplicate.windows[1].id = duplicate.windows[0].id;
    assert!(!encodes(duplicate));
    let mut zero = inventory();
    zero.windows[1].id = 0;
    assert!(!encodes(zero));
    let mut empty = inventory();
    empty.windows[1].width = 0;
    assert!(!encodes(empty));
    let mut padded = inventory();
    padded.windows[0].title = " padded".into();
    assert!(!encodes(padded));
    let mut control = inventory();
    control.windows[0].title = "line\nbreak".into();
    assert!(!encodes(control));
    let mut too_many = inventory();
    too_many.windows = (1..=u64::try_from(MAX_SCREEN_CAPTURE_WINDOWS).unwrap() + 1)
        .map(|id| WindowInfo {
            id,
            ..inventory().windows[1].clone()
        })
        .collect();
    assert!(!encodes(too_many));
    let mut unsupported_lists = inventory();
    unsupported_lists.supported = false;
    assert!(!encodes(unsupported_lists));

    let line = round_trip(&targets("r", inventory()));
    refused(&line.replacen("supported=1", "supported=2", 1));
    refused(&line.replacen("%7E13%3AKeynote", "%7E14%3AKeynote", 1));
    refused(&line.replacen("\twindows=", "\twindows=x", 1));
    refused(&line.replacen('\n', "\textra=1\n", 1));
}

#[test]
fn desktop_picked_captures_have_exact_wire_forms() {
    // The picked window rule travels as an empty application and title.
    let picked = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::picked()));
    let line = round_trip(&add(picked.clone()));
    assert!(
        line.contains("\ttarget=window\tapp=\ttitle=\ttitle_match=any\t"),
        "{line}"
    );
    round_trip(&command(CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: input(5),
            source: WireInputSource::ScreenCapture(picked),
        },
    )));

    // The dialog's states.
    for (state, token) in [
        (ScreenCaptureState::AwaitingSelection, "awaiting_selection"),
        (ScreenCaptureState::Denied, "denied"),
    ] {
        let line = round_trip(&runtime(RuntimeLifecycleEvent::ScreenCaptureStatus {
            input: input(5),
            status: Box::new(ScreenCaptureStatus {
                state,
                last_error: Some("the operator cancelled sharing".into()),
                ..ScreenCaptureStatus::default()
            }),
        }));
        assert!(
            line.contains(&format!("\tstate={token}\treopens=0\t")),
            "{line}"
        );
        refused(&line.replacen("\treopens=0", "\tattempt=1\tnext_in_ms=0\treopens=0", 1));
    }

    // A portal inventory lists nothing.
    let portal = ScreenCaptureInventory {
        supported: true,
        backend: fm_protocol::PORTAL_BACKEND.into(),
        ..ScreenCaptureInventory::default()
    };
    assert_eq!(
        round_trip(&targets("p", portal)),
        "screen_capture_targets\trequest_id=p\tsupported=1\tbackend=portal\tscreens=\twindows=\n"
    );

    // Asking the desktop to pick again.
    let pick = WireMessage::ScreenCapturePickRequest(ScreenCapturePickRequest {
        request_id: "pick-1".into(),
        input: input(5),
    });
    assert_eq!(
        round_trip(&pick),
        "screen_capture_pick_request\trequest_id=pick-1\tinput=5\n"
    );
    refused("screen_capture_pick_request\trequest_id=pick-1\n");
    refused("screen_capture_pick_request\tinput=5\n");
    refused("screen_capture_pick_request\trequest_id=pick-1\tinput=05\n");
    refused("screen_capture_pick_request\trequest_id=pick-1\tinput=0\n");
    refused("screen_capture_pick_request\trequest_id=pick-1\tinput=5\textra=1\n");
}
