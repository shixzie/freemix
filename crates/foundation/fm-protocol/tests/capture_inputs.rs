//! Protocol 2.47 local capture inputs (IN-001): `input_add` with
//! `kind=capture`, the `input_capture_set` command, the
//! `capture_settings_changed` durable event, the `capture_status` and
//! `capture_devices` runtime events, the `capture_devices_request` /
//! `capture_devices` exchange, and the snapshot's `capture_inputs` —
//! exactly, and strictly.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CaptureAudio, CaptureDevice, CaptureDevicesMessage,
    CaptureDevicesRequest, CaptureFormat, CaptureInputConfig, CaptureInputEntry, CaptureInventory,
    CaptureLossReason, CaptureLossSlate, CapturePixelFormat, CaptureScan, CaptureSettings,
    CaptureSignalState, CaptureStatus, CommandMessage, CommandPayload, EditHistoryStatus,
    EngineIdentity, EventCursor, EventMessage, EventPayload, LiveInputSource,
    MAX_CAPTURE_DEVICE_FORMATS, MAX_CAPTURE_DEVICES, MAX_CAPTURE_ERROR_BYTES, MAX_CAPTURE_INPUTS,
    MAX_CAPTURE_TEXT_BYTES, MAX_LINE_BYTES, ProtocolVersion, RuntimeEventMessage,
    RuntimeLifecycleEvent, ServerIdentity, WireInputId, WireMessage, decode_line, encode_line,
    fit_capture_inventory, wire_capture_status,
};
use fm_types::FrameRate;

const PREFIX: &str = "command\tprotocol=2.74\tid=a\tidempotency_key=k";

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

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn format(
    width: u32,
    height: u32,
    (numerator, denominator): (u32, u32),
    pixel_format: CapturePixelFormat,
    scan: CaptureScan,
) -> CaptureFormat {
    CaptureFormat {
        width,
        height,
        rate: FrameRate::new(numerator, denominator).unwrap(),
        pixel_format,
        scan,
    }
}

fn hd() -> CaptureFormat {
    format(
        1920,
        1080,
        (30_000, 1_001),
        CapturePixelFormat::Yuyv,
        CaptureScan::InterlacedTff,
    )
}

fn settings() -> CaptureSettings {
    CaptureSettings {
        format: Some(hd()),
        audio: CaptureAudio::Alsa("hw:CARD=C920,DEV=0".into()),
        loss: CaptureLossSlate::Black,
    }
}

const SETTINGS: &str = "format=1920x1080%4030000%2F1001%3Ayuyv%3Atff\taudio=alsa%3Ahw%3ACARD%3DC920%2CDEV%3D0\tloss=black";

#[test]
fn the_current_protocol_is_2_47() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 74));
}

#[test]
fn commands_have_an_exact_wire_form() {
    let add = command(CommandPayload::AddInput {
        input: input(5),
        name: "Camera".into(),
        source: LiveInputSource::Capture(Box::new(CaptureInputConfig {
            stable_key: "v4l2:usb-1#Cam".into(),
            settings: settings(),
        })),
    });
    assert_eq!(
        round_trip(&add),
        format!(
            "{PREFIX}\tpayload=input_add\tinput=5\tname=Camera\tkind=capture\tdevice=v4l2%3Ausb-1%23Cam\t{SETTINGS}\n"
        )
    );
    let set = command(CommandPayload::SetCaptureSettings {
        input: input(5),
        settings: Box::new(CaptureSettings::default()),
    });
    let line = round_trip(&set);
    assert_eq!(
        line,
        format!(
            "{PREFIX}\tpayload=input_capture_set\tinput=5\tformat=-\taudio=paired\tloss=hold\n"
        )
    );

    // Every setting is required and validated.
    for (from, to) in [
        ("format=-", "format=1920x1080"),
        ("format=-", "format=8x8%4030%2F1%3Ayuyv%3Ap"),
        ("format=-", "format=1920x1080%4060%2F2%3Ayuyv%3Ap"),
        ("audio=paired", "audio=pulse"),
        ("audio=paired", "audio=alsa%3A"),
        ("loss=hold", "loss=blue"),
        ("\tloss=hold", ""),
        ("input=5", "input=05"),
    ] {
        refused(&line.replacen(from, to, 1));
    }
    let add_line = encode_line(&add).unwrap();
    for (from, to) in [
        ("device=v4l2%3Ausb-1%23Cam", "device=%20"),
        ("device=v4l2%3Ausb-1%23Cam", "device=a%0Ab"),
        ("\tdevice=v4l2%3Ausb-1%23Cam", ""),
        ("kind=capture", "kind=camera"),
    ] {
        refused(&add_line.replacen(from, to, 1));
    }
    let long_key = command(CommandPayload::AddInput {
        input: input(5),
        name: "Camera".into(),
        source: LiveInputSource::Capture(Box::new(CaptureInputConfig::new("k".repeat(257)))),
    });
    assert!(encode_line(&long_key).is_err());
}

#[test]
fn durable_events_carry_the_config_and_settings() {
    let changed = event(EventPayload::CaptureSettingsChanged {
        input: input(5),
        settings: Box::new(settings()),
    });
    let line = round_trip(&changed);
    assert!(
        line.contains(&format!(
            "\tevent=capture_settings_changed\tinput=5\t{SETTINGS}"
        )),
        "{line}"
    );
    let added = event(EventPayload::InputAdded {
        input: input(5),
        name: "Camera".into(),
        network_stream: None,
        system_audio: None,
        capture: Some(Box::new(CaptureInputConfig {
            stable_key: "v4l2:usb-1#Cam".into(),
            settings: settings(),
        })),
        still: None,
    });
    let line = round_trip(&added);
    assert!(
        line.contains(&format!(
            "\tkind=capture\tdevice=v4l2%3Ausb-1%23Cam\t{SETTINGS}"
        )),
        "{line}"
    );
    // A capture add carries its device.
    refused(&line.replacen("\tdevice=v4l2%3Ausb-1%23Cam", "", 1));
    // One kind at most.
    let both = event(EventPayload::InputAdded {
        input: input(5),
        name: "Camera".into(),
        network_stream: Some(Box::new(fm_protocol::NetworkStreamConfig::new(
            "udp://@:9000",
        ))),
        system_audio: None,
        capture: Some(Box::new(CaptureInputConfig::new("cam"))),
        still: None,
    });
    assert!(encode_line(&both).is_err());
}

fn status() -> CaptureStatus {
    CaptureStatus {
        state: CaptureSignalState::SignalLost,
        reason: Some(CaptureLossReason::Unplugged),
        device: Some("/dev/video0".into()),
        format: Some(hd()),
        audio: true,
        frames: 12_345,
        losses: 2,
        recoveries: 1,
        renegotiations: 3,
        last_error: Some("device gone ~ ENODEV, 19".into()),
    }
}

#[test]
fn status_events_are_exact_and_bounded() {
    let message = runtime(RuntimeLifecycleEvent::CaptureStatus {
        input: input(5),
        status: Box::new(status()),
    });
    let line = round_trip(&message);
    assert!(
        line.ends_with(
            "\tevent=capture_status\tinput=5\tstate=signal_lost\treason=unplugged\tdevice=%2Fdev%2Fvideo0\tformat=1920x1080%4030000%2F1001%3Ayuyv%3Atff\taudio=1\tframes=12345\tlosses=2\trecoveries=1\trenegotiations=3\terror=device%20gone%20%7E%20ENODEV%2C%2019\n"
        ),
        "{line}"
    );
    for state in CaptureSignalState::ALL {
        let quiet = runtime(RuntimeLifecycleEvent::CaptureStatus {
            input: input(5),
            status: Box::new(CaptureStatus {
                state,
                ..CaptureStatus::default()
            }),
        });
        let line = round_trip(&quiet);
        assert!(
            line.contains(&format!(
                "\tstate={}\treason=-\tdevice=-\tformat=-\taudio=0\tframes=0\tlosses=0\trecoveries=0\trenegotiations=0\terror=-\n",
                state.token()
            )),
            "{line}"
        );
    }
    let quiet = encode_line(&runtime(RuntimeLifecycleEvent::CaptureStatus {
        input: input(5),
        status: Box::new(CaptureStatus::default()),
    }))
    .unwrap();
    for (from, to) in [
        ("state=idle", "state=lost"),
        ("reason=-", "reason=gone"),
        ("audio=0", "audio=2"),
        ("frames=0", "frames=00"),
        ("\terror=-", ""),
        ("error=-", "error=a%0Ab"),
        ("device=-", "device="),
    ] {
        refused(&quiet.replacen(from, to, 1));
    }
    refused(&quiet.replacen(
        "error=-",
        &format!("error={}", "e".repeat(MAX_CAPTURE_ERROR_BYTES + 1)),
        1,
    ));

    // Over-long or placeholder texts are refused on encode; the wire
    // helper bounds them.
    let mut long = status();
    long.last_error = Some("e".repeat(MAX_CAPTURE_ERROR_BYTES + 10));
    long.device = Some("-".into());
    let raw = runtime(RuntimeLifecycleEvent::CaptureStatus {
        input: input(5),
        status: Box::new(long.clone()),
    });
    assert!(encode_line(&raw).is_err());
    let bounded = wire_capture_status(&long);
    assert_eq!(bounded.device, None);
    assert_eq!(
        bounded.last_error.as_deref().map(str::len),
        Some(MAX_CAPTURE_ERROR_BYTES)
    );
    round_trip(&runtime(RuntimeLifecycleEvent::CaptureStatus {
        input: input(5),
        status: Box::new(bounded),
    }));
}

fn device(key: &str, formats: Vec<CaptureFormat>) -> CaptureDevice {
    CaptureDevice {
        stable_key: key.into(),
        name: "HD Pro Webcam C920; rev ~2, 100%".into(),
        path: "/dev/video0".into(),
        driver: "uvcvideo".into(),
        bus: "usb-0000:00:14.0-1".into(),
        audio: Some("hw:CARD=C920,DEV=0".into()),
        formats,
    }
}

fn inventory() -> CaptureInventory {
    CaptureInventory {
        backend: Some("v4l2".into()),
        error: None,
        devices: vec![
            device(
                "v4l2:usb-1#C920",
                vec![
                    hd(),
                    format(
                        640,
                        480,
                        (30, 1),
                        CapturePixelFormat::Mjpeg,
                        CaptureScan::Progressive,
                    ),
                ],
            ),
            CaptureDevice {
                audio: None,
                driver: String::new(),
                bus: String::new(),
                formats: Vec::new(),
                ..device("v4l2:usb-2#-", Vec::new())
            },
        ],
    }
}

#[test]
fn device_inventories_round_trip_as_replies_and_runtime_events() {
    let request = WireMessage::CaptureDevicesRequest(CaptureDevicesRequest {
        request_id: "devices-1".into(),
    });
    assert_eq!(
        round_trip(&request),
        "capture_devices_request\trequest_id=devices-1\n"
    );
    let reply = WireMessage::CaptureDevices(CaptureDevicesMessage {
        request_id: "devices-1".into(),
        inventory: inventory(),
    });
    let line = round_trip(&reply);
    assert!(
        line.starts_with("capture_devices\trequest_id=devices-1\tbackend=v4l2\terror=-\tdevices="),
        "{line}"
    );
    let event = runtime(RuntimeLifecycleEvent::CaptureDevices(inventory()));
    let event_line = round_trip(&event);
    assert!(event_line.contains("\tevent=capture_devices\tbackend=v4l2\terror=-\tdevices="));
    let empty = WireMessage::CaptureDevices(CaptureDevicesMessage {
        request_id: "r".into(),
        inventory: CaptureInventory {
            backend: None,
            error: Some("no capture backend".into()),
            devices: Vec::new(),
        },
    });
    assert_eq!(
        round_trip(&empty),
        "capture_devices\trequest_id=r\tbackend=-\terror=no%20capture%20backend\tdevices=\n"
    );

    // Out of order, duplicate, or unbounded inventories are refused.
    let mut descending = inventory();
    descending.devices.reverse();
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::CaptureDevices(descending))).is_err());
    let mut bad_backend = inventory();
    bad_backend.backend = Some("V4L2".into());
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::CaptureDevices(bad_backend))).is_err());
    let mut long_name = inventory();
    long_name.devices[0].name = "n".repeat(MAX_CAPTURE_TEXT_BYTES + 1);
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::CaptureDevices(long_name))).is_err());
    let mut too_many = inventory();
    too_many.devices = (0..=MAX_CAPTURE_DEVICES)
        .map(|index| device(&format!("cam-{index:02}"), Vec::new()))
        .collect();
    assert!(encode_line(&runtime(RuntimeLifecycleEvent::CaptureDevices(too_many))).is_err());
    let mut many_formats = inventory();
    many_formats.devices[0].formats = vec![hd(); MAX_CAPTURE_DEVICE_FORMATS + 1];
    assert!(
        encode_line(&runtime(RuntimeLifecycleEvent::CaptureDevices(
            many_formats
        )))
        .is_err()
    );
    refused(&line.replacen("backend=v4l2", "backend=V4L2", 1));
    refused(&line.replacen("\terror=-", "", 1));
    refused(&line.replacen("request_id=devices-1", "request_id=", 1));
}

#[test]
fn fitting_bounds_an_inventory_to_one_line() {
    // Every device at its bounds: the fitted inventory still encodes with
    // the longest request id, losing formats from the longest lists.
    let formats = (0..MAX_CAPTURE_DEVICE_FORMATS)
        .map(|index| {
            format(
                8_000 + u32::try_from(index).unwrap(),
                4_320,
                (240_000, 1_001),
                CapturePixelFormat::Mjpeg,
                CaptureScan::InterlacedBff,
            )
        })
        .collect::<Vec<_>>();
    let huge = CaptureInventory {
        backend: Some("v4l2".into()),
        error: Some("x\u{7}".repeat(200)),
        devices: (0..MAX_CAPTURE_DEVICES + 4)
            .rev()
            .map(|index| CaptureDevice {
                name: "n,~".repeat(100),
                path: format!("/dev/video{index}"),
                ..device(&format!("v4l2:{index:02}"), formats.clone())
            })
            .chain([device("\u{1}bad", Vec::new())])
            .collect(),
    };
    let fitted = fit_capture_inventory(&huge);
    assert_eq!(fitted.devices.len(), MAX_CAPTURE_DEVICES);
    assert!(
        fitted
            .devices
            .windows(2)
            .all(|pair| pair[0].stable_key < pair[1].stable_key)
    );
    assert!(
        fitted
            .devices
            .iter()
            .all(|device| device.name.len() <= MAX_CAPTURE_TEXT_BYTES)
    );
    assert!(
        fitted
            .error
            .as_ref()
            .is_some_and(|error| error.len() <= MAX_CAPTURE_ERROR_BYTES)
    );
    let kept = fitted
        .devices
        .iter()
        .map(|device| device.formats.len())
        .collect::<Vec<_>>();
    assert!(
        kept.iter().all(|count| *count < MAX_CAPTURE_DEVICE_FORMATS),
        "{kept:?}"
    );
    assert!(
        kept.iter().max().unwrap() - kept.iter().min().unwrap() <= 1,
        "{kept:?}"
    );
    let line = encode_line(&WireMessage::CaptureDevices(CaptureDevicesMessage {
        request_id: "r".repeat(128),
        inventory: fitted.clone(),
    }))
    .unwrap();
    assert!(line.len() <= MAX_LINE_BYTES - 2 * 1024, "{}", line.len());
    round_trip(&runtime(RuntimeLifecycleEvent::CaptureDevices(fitted)));
    // A small inventory is unchanged.
    assert_eq!(fit_capture_inventory(&inventory()), inventory());
}

#[test]
fn snapshot_entries_round_trip_and_are_bounded() {
    let entries = vec![
        CaptureInputEntry {
            input: input(5),
            config: CaptureInputConfig {
                stable_key: "v4l2:usb-1#C920,~;%".into(),
                settings: settings(),
            },
            status: CaptureStatus {
                last_error: None,
                ..status()
            },
        },
        CaptureInputEntry {
            input: input(7),
            config: CaptureInputConfig::new("-"),
            status: CaptureStatus::default(),
        },
    ];
    let line = encode_line(&snapshot(entries.clone())).unwrap();
    let WireMessage::Snapshot(decoded) = decode_line(&line).unwrap() else {
        panic!("{line}");
    };
    assert_eq!(decoded.capture_inputs, entries);
    assert!(
        line.ends_with(
            "~-~paired~hold~idle~-~-~-~0~0~0~0~0\tsystem_audio_inputs=\tstream_targets=\tlive_lan=0%7Eprogram%7E8%7E0\tlive_lan_status=off%7E0%7E0%7E\n"
                .replace('~', "%7E")
                .as_str()
        ),
        "{line}"
    );

    // Ascending, without error text, and bounded.
    let mut descending = entries.clone();
    descending.reverse();
    assert!(encode_line(&snapshot(descending)).is_err());
    let mut with_error = entries.clone();
    with_error[0].status.last_error = Some("boom".into());
    assert!(encode_line(&snapshot(with_error)).is_err());
    let too_many = (1..=u128::try_from(MAX_CAPTURE_INPUTS).unwrap() + 1)
        .map(|value| CaptureInputEntry {
            input: input(value),
            config: CaptureInputConfig::new(format!("cam-{value}")),
            status: CaptureStatus::default(),
        })
        .collect();
    assert!(encode_line(&snapshot(too_many)).is_err());
    // A non-canonical escape is refused.
    let canonical = encode_line(&snapshot(entries)).unwrap();
    refused(&canonical.replacen("%2525", "%2537", 1));
}

fn snapshot(capture_inputs: Vec<CaptureInputEntry>) -> WireMessage {
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
        frame_rate: FrameRate::new(30, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
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
        capture_inputs,
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
    })
}
