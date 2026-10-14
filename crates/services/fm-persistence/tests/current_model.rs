use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{
    AudioBus, BusSend, CURRENT_SCHEMA_VERSION, CropRect, Input, InputAudioStripState,
    InputBalanceBasisPoints, InputDelaySamples, InputGainMilliDb, InputKind, Layer, LayerEffect,
    LayerGeometry, MainMix, MixFeed, Output, Project, ProjectSettings, RecorderSlot, RectMask,
    RestartPolicy, Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo,
    SolidColor, SourceRef, StartupPolicy, StingerAudioPolicy, StingerConfig,
    StingerMissingMediaFallback, StingerSlotNumber, StreamEndpoint, StreamKey, StreamLadder,
    StreamLadderRung, StreamProtocol, StreamTarget, StreamTargetId,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoreError, StoredProject};
use fm_types::{
    AudioFormat, BusId, Channel, ChannelLayout, ChromaLocation, ColorMetadata, ColorPrimaries,
    DiscreteChannel, FrameRate, InputId, MatrixCoefficients, OutputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, SignalRange, TransferFunction, VideoDimensions,
    VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-v3-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self, name: &str) -> ProjectStore {
        ProjectStore::new(self.0.join(format!("{name}.freemix"))).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}
fn project_id(value: u128) -> ProjectId {
    ProjectId::new(nz(value))
}
fn input_id(value: u128) -> InputId {
    InputId::new(nz(value))
}
fn scene_id(value: u128) -> SceneId {
    SceneId::new(nz(value))
}
fn bus_id(value: u128) -> BusId {
    BusId::new(nz(value))
}
fn output_id(value: u128) -> OutputId {
    OutputId::new(nz(value))
}
fn stream_target_id(value: u128) -> StreamTargetId {
    StreamTargetId::new(nz(value))
}

/// The rich fixture's stream key. Nothing but `project.json` may contain it.
const RICH_STREAM_KEY: &str = "live-2f8c41d9-secret";

/// The authored ladder the HLS ladder tests write and rewrite. The array is
/// exactly what the encoder must produce, field order included.
const HLS_LADDER_JSON: &str = "[{\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 360}, {\"video_bitrate_kbps\": 1500, \"width\": 1280, \"height\": 720}]";

fn rich_settings() -> ProjectSettings {
    let frame_rate = FrameRate::new(24_000, 1_001).unwrap();
    ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(3_840, 2_160).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::P010,
            scan: ScanMode::InterlacedTopFieldFirst,
            color: ColorMetadata {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferFunction::Pq,
                matrix: MatrixCoefficients::Bt2020NonConstant,
                range: SignalRange::Full,
                chroma_location: ChromaLocation::TopLeft,
            },
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(96_000).unwrap(),
            sample_format: SampleFormat::I24,
            channels: ChannelLayout::new(vec![
                Channel::Left,
                Channel::Right,
                Channel::Center,
                Channel::LeftSurround,
                Channel::RightSurround,
            ])
            .unwrap(),
        },
    }
}

fn rich_inputs(high: u128) -> [Input; 7] {
    [
        Input {
            id: input_id(high),
            name: "Color".into(),
            kind: InputKind::Color,
            required_capabilities: vec!["gpu.color.fill".into()],
        },
        Input {
            id: input_id(high + 1),
            name: "Media".into(),
            kind: InputKind::Media {
                asset_uri: "asset://opening.mov".into(),
            },
            required_capabilities: vec!["codec.video.hevc".into()],
        },
        Input {
            id: input_id(high + 2),
            name: "Device".into(),
            kind: InputKind::Device {
                stable_key: "decklink:1".into(),
            },
            required_capabilities: vec!["capture.device.sdi".into()],
        },
        Input {
            id: input_id(high + 3),
            name: "Network".into(),
            kind: InputKind::Network {
                endpoint: "srt://example.test:9000".into(),
            },
            required_capabilities: vec!["network.input.srt".into()],
        },
        Input {
            id: input_id(high + 4),
            name: "Solid silence".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 4)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        },
        Input {
            id: input_id(high + 5),
            name: "Bars tone".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Sine {
                    frequency_hz: 1_234,
                },
            )),
            required_capabilities: Vec::new(),
        },
        Input {
            id: input_id(high + 6),
            name: "Scene route".into(),
            kind: InputKind::Scene {
                scene_id: scene_id(high + 11),
                audio_source: Some(input_id(high + 5)),
            },
            required_capabilities: vec!["scene.composite.basic".into()],
        },
    ]
}

fn rich_stream_target(high: u128) -> StreamTarget {
    StreamTarget::new(
        stream_target_id(high + 40),
        "Main ingest".into(),
        StreamProtocol::Rtmps,
        StreamEndpoint::parse("ingest.example.test:443/live").unwrap(),
        Some(StreamKey::parse(RICH_STREAM_KEY).unwrap()),
        output_id(high + 30),
    )
    .unwrap()
    .with_backup_endpoint(Some(
        StreamEndpoint::parse("backup.example.test/live/eu").unwrap(),
    ))
    .unwrap()
    .with_video_bitrate(9_000)
    .unwrap()
    .with_startup(StartupPolicy::ReconcileDesiredState)
    .set_running(true)
}

/// Adds the authored HLS ladder destination to the rich fixture.
///
/// The target is the second stream destination, so callers index it as
/// `stream_targets()[1]`. Setting the ladder moves its base bitrate to the top
/// rung, so the authored `video_bitrate_kbps` is `1500`.
fn laddered_hls_project(high: u128) -> Project {
    let mut project = rich_project();
    let target = StreamTarget::new(
        stream_target_id(high + 42),
        "Local HLS ladder".into(),
        StreamProtocol::Hls,
        StreamEndpoint::parse_for(StreamProtocol::Hls, "/var/lib/freemix/live").unwrap(),
        None,
        output_id(high + 30),
    )
    .unwrap()
    .with_ladder(Some(hls_ladder()))
    .unwrap();
    project.add_stream_target_checked(target).unwrap();
    project
}

fn hls_ladder() -> StreamLadder {
    StreamLadder::new(vec![
        StreamLadderRung::new(1_000, 640, 360).unwrap(),
        StreamLadderRung::new(1_500, 1_280, 720).unwrap(),
    ])
    .unwrap()
}

fn rich_project() -> Project {
    let high = u128::from(u64::MAX) + 101;
    let mut project = Project::new(
        project_id(u128::MAX - 1),
        "Rich production",
        rich_settings(),
    )
    .with_main_mix(MainMix::new(input_id(high), input_id(high + 1)))
    .with_restart_policy(RestartPolicy::OnFailure { max_attempts: 7 });
    for input in rich_inputs(high) {
        project.add_input(input);
    }
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        input_id(high + 1),
        true,
        17,
        StingerAudioPolicy::StingerOnly,
        StingerMissingMediaFallback::Fade,
    ));
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(8).unwrap(),
        input_id(high + 5),
        false,
        0,
        StingerAudioPolicy::MixWithProgram,
        StingerMissingMediaFallback::KeepProgram,
    ));

    project.add_scene(Scene {
        id: scene_id(high + 10),
        name: "Base".into(),
        background: Rgba8::new(8, 4, 2, 16),
        layers: vec![Layer {
            name: "Camera".into(),
            source: SourceRef::Input(input_id(high + 2)),
            enabled: true,
            geometry: LayerGeometry::new(-20, 30, 1920, 1080, Rotation::Deg90),
            crop: Some(CropRect::new(10, 20, 1000, 700)),
            mask: Some(RectMask::new(11, 12, 500, 300).inverted(true)),
            effects: Vec::new(),
            opacity: 200,
            z_order: -7,
        }],
    });
    project.add_scene(Scene {
        id: scene_id(high + 11),
        name: "Composite".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            Layer {
                name: "Base scene".into(),
                source: SourceRef::Scene(scene_id(high + 10)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 3840, 2160, Rotation::Deg0),
                crop: None,
                mask: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 3,
            },
            Layer {
                name: "Overlay".into(),
                source: SourceRef::Input(input_id(high + 4)),
                enabled: false,
                geometry: LayerGeometry::new(100, -200, 640, 360, Rotation::Deg270),
                crop: Some(CropRect::new(1, 2, 3, 4)),
                mask: Some(RectMask::new(1, 1, 2, 3)),
                effects: Vec::new(),
                opacity: 127,
                z_order: 3,
            },
        ],
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(high + 20),
        name: "Program".into(),
        sends: vec![BusSend {
            destination: bus_id(high + 21),
        }],
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(high + 21),
        name: "Monitor".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id(high + 30),
        name: "Primary".into(),
        video_source: scene_id(high + 11),
        audio_source: bus_id(high + 20),
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: vec!["output.network.srt".into()],
    });
    project.add_stream_target(rich_stream_target(high));
    project
}

fn stored_rich_project() -> StoredProject {
    stored_rich_project_with_running_target(true)
}

fn stored_rich_project_with_running_target(running: bool) -> StoredProject {
    let high = u128::from(u64::MAX) + 101;
    let mut project = rich_project();
    let target = project.stream_targets()[0].clone().set_running(running);
    project.replace_stream_target(target).unwrap();
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input_id(high)),
            realized_program_id: Some(input_id(high + 1)),
            desired_preview_id: Some(input_id(high + 1)),
            realized_preview_id: Some(input_id(high)),
        },
        ProjectPosition {
            revision: 8,
            state_epoch: 3,
            event_sequence: 13,
            frames_rendered: 800,
            runtime_generation: 2,
            clock_time_nanos: 99_000,
        },
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn input_audio_strips_round_trip_exactly_and_reject_malformed_values() {
    let temp = TestDirectory::new("input-audio-strips");
    let store = temp.store("show");
    let mut project = rich_project();
    let input = project.inputs()[1].id;
    let state = InputAudioStripState {
        gain: InputGainMilliDb::new(-12_345).unwrap(),
        balance: InputBalanceBasisPoints::new(2_500).unwrap(),
        delay_samples: InputDelaySamples::new(1_200).unwrap(),
        muted: true,
        soloed: true,
        follow_video: false,
    };
    assert!(project.set_input_audio_strip(input, state));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project().input_audio_strip(input), Some(state));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(
        encoded.contains(
            "\"gain_milli_db\": -12345, \"balance_basis_points\": 2500, \"delay_samples\": 1200, \"muted\": true, \"soloed\": true, \"follow_video\": false"
        )
    );

    let strip_line = encoded
        .lines()
        .find(|line| line.contains("\"gain_milli_db\": -12345"))
        .unwrap();
    for invalid in [
        encoded.replacen("\"gain_milli_db\": -12345", "\"gain_milli_db\": 24001", 1),
        encoded.replacen(
            "\"gain_milli_db\": -12345",
            "\"gain_milli_db\": \"-12345\"",
            1,
        ),
        encoded.replacen(
            "\"balance_basis_points\": 2500",
            "\"balance_basis_points\": 10001",
            1,
        ),
        encoded.replacen(
            "\"balance_basis_points\": 2500",
            "\"balance_basis_points\": \"2500\"",
            1,
        ),
        encoded.replacen("\"delay_samples\": 1200", "\"delay_samples\": 48001", 1),
        encoded.replacen("\"delay_samples\": 1200", "\"delay_samples\": \"1200\"", 1),
        encoded.replacen("\"soloed\": true", "\"soloed\": \"true\"", 1),
        encoded.replacen(
            &format!("\"input\": {input}, \"gain_milli_db\": -12345"),
            "\"input\": 999, \"gain_milli_db\": -12345",
            1,
        ),
        encoded.replacen(strip_line, &format!("{strip_line}\n{strip_line}"), 1),
    ] {
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }
}

#[test]
fn current_project_round_trip_preserves_formats_graph_capabilities_and_u128_ids() {
    let temp = TestDirectory::new("rich-round-trip");
    let store = temp.store("show");
    let expected = stored_rich_project();

    store.save(&expected).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, expected);
    assert_eq!(loaded.project().id().get().get(), u128::MAX - 1);
    assert_eq!(
        loaded.project().inputs()[4].kind,
        expected.project().inputs()[4].kind
    );
    assert_eq!(
        loaded.project().inputs()[5].kind,
        expected.project().inputs()[5].kind
    );
    assert_eq!(loaded.project().settings(), &rich_settings());
    assert_eq!(loaded.runtime_routing(), expected.runtime_routing());
    assert_eq!(
        loaded.project().scenes()[0].layers[0].mask,
        Some(RectMask::new(11, 12, 500, 300).inverted(true))
    );
    assert_eq!(
        loaded.project().scenes()[1].layers[1].mask,
        Some(RectMask::new(1, 1, 2, 3))
    );
    assert!(matches!(
        loaded.project().inputs()[6].kind,
        InputKind::Scene {
            scene_id: id,
            audio_source: Some(audio)
        } if id.get().get() > u128::from(u64::MAX)
            && audio.get().get() > u128::from(u64::MAX)
    ));
}

#[test]
fn stream_destination_round_trips_at_the_current_schema_and_keeps_its_key_off_every_other_surface()
{
    let temp = TestDirectory::new("stream-destination");
    let store = temp.store("show");
    let expected = stored_rich_project();

    store.save(&expected).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, expected);
    assert_eq!(loaded.project().schema_version(), CURRENT_SCHEMA_VERSION);
    let target = &loaded.project().stream_targets()[0];
    assert_eq!(target.name(), "Main ingest");
    assert_eq!(target.protocol(), StreamProtocol::Rtmps);
    assert_eq!(target.endpoint().as_str(), "ingest.example.test:443/live");
    assert_eq!(
        target.backup_endpoint().map(StreamEndpoint::as_str),
        Some("backup.example.test/live/eu")
    );
    assert_eq!(target.key().unwrap().expose_secret(), RICH_STREAM_KEY);
    assert_eq!(target.startup(), StartupPolicy::ReconcileDesiredState);
    assert!(target.running());
    assert_eq!(
        target.expose_url(),
        format!("rtmps://ingest.example.test:443/live/{RICH_STREAM_KEY}")
    );

    // The manifest is the one place the key is allowed to appear: the bundle
    // is plaintext by design and must be protected like the credential it now
    // holds. Nothing derived from the loaded project may repeat it.
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains(&format!("\"key\": \"{RICH_STREAM_KEY}\"")));
    for rendered in [
        format!("{target:?}"),
        format!("{target}"),
        format!("{:?}", loaded.project()),
        target.redacted_url(),
        target.redacted_backup_url().unwrap(),
    ] {
        assert!(
            !rendered.contains(RICH_STREAM_KEY),
            "stream key leaked into `{rendered}`"
        );
    }
    assert_eq!(
        target.redacted_url(),
        "rtmps://ingest.example.test:443/live/****"
    );
}

#[test]
fn srt_stream_destination_round_trips_at_the_current_schema() {
    const KEY: &str = "srt-4e7a-roundtrip-key";
    let high = u128::from(u64::MAX) + 101;
    let temp = TestDirectory::new("stream-target-srt");
    let store = temp.store("show");

    let mut project = rich_project();
    let target = StreamTarget::new(
        stream_target_id(high + 40),
        "SRT relay".into(),
        StreamProtocol::Srt,
        StreamEndpoint::parse_for(StreamProtocol::Srt, "relay.example.test:9710").unwrap(),
        Some(StreamKey::parse(KEY).unwrap()),
        output_id(high + 30),
    )
    .unwrap()
    .with_backup_endpoint(Some(
        StreamEndpoint::parse_for(StreamProtocol::Srt, "fallback.example.test").unwrap(),
    ))
    .unwrap();
    project.replace_stream_target(target).unwrap();
    let expected = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"protocol\": \"srt\""));
    assert!(encoded.contains("\"endpoint\": \"relay.example.test:9710\""));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    let target = &loaded.project().stream_targets()[0];
    assert_eq!(target.protocol(), StreamProtocol::Srt);
    assert_eq!(target.endpoint().as_str(), "relay.example.test:9710");
    assert_eq!(
        target.backup_endpoint().map(StreamEndpoint::as_str),
        Some("fallback.example.test")
    );
    assert_eq!(
        target.expose_url(),
        format!("srt://relay.example.test:9710?streamid={KEY}")
    );
    assert_eq!(
        target.redacted_url(),
        "srt://relay.example.test:9710?streamid=****"
    );
}

#[test]
fn hls_stream_destination_round_trips_with_a_null_key_at_the_current_schema() {
    let high = u128::from(u64::MAX) + 101;
    let temp = TestDirectory::new("stream-target-hls");
    let store = temp.store("show");

    let mut project = rich_project();
    let target = StreamTarget::new(
        stream_target_id(high + 41),
        "Local HLS".into(),
        StreamProtocol::Hls,
        StreamEndpoint::parse_for(StreamProtocol::Hls, "/var/lib/freemix/live").unwrap(),
        None,
        output_id(high + 30),
    )
    .unwrap();
    project.add_stream_target_checked(target).unwrap();
    let expected = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 26"));
    assert!(encoded.contains("\"protocol\": \"hls\""));
    assert!(encoded.contains("\"endpoint\": \"/var/lib/freemix/live\""));
    assert!(encoded.contains("\"backup_endpoint\": null"));
    assert!(encoded.contains("\"key\": null"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    let loaded_target = &loaded.project().stream_targets()[1];
    assert_eq!(loaded_target.protocol(), StreamProtocol::Hls);
    assert_eq!(loaded_target.key(), None);
    assert_eq!(loaded_target.endpoint().as_str(), "/var/lib/freemix/live");
    assert_eq!(loaded_target.expose_url(), "hls:///var/lib/freemix/live");
    assert_eq!(loaded_target.redacted_url(), "hls:///var/lib/freemix/live");

    // HLS belongs to the current contract: the key is exactly `null`, so a
    // string, another type, or a missing field is malformed.
    for malformed in [
        encoded.replacen("\"key\": null", "\"key\": \"live-key-012345\"", 1),
        encoded.replacen("\"key\": null", "\"key\": 12345", 1),
        encoded.replacen(",\n        \"key\": null", "", 1),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "expected a malformed manifest, got {error:?}"
        );
        assert!(!error.to_string().contains("live-key-012345"));
    }

    // RTMP/RTMPS/SRT still require the key string: `null` or a non-string
    // there is malformed rather than a keyless remote destination.
    store.save(&stored_rich_project()).unwrap();
    let remote = fs::read_to_string(store.manifest_path()).unwrap();
    for malformed in [
        remote.replacen(
            &format!("\"key\": \"{RICH_STREAM_KEY}\""),
            "\"key\": null",
            1,
        ),
        remote.replacen(
            &format!("\"key\": \"{RICH_STREAM_KEY}\""),
            "\"key\": 12345",
            1,
        ),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }
}

#[test]
fn hls_stream_ladder_round_trips_at_the_current_schema() {
    let high = u128::from(u64::MAX) + 101;
    let temp = TestDirectory::new("stream-target-ladder");
    let store = temp.store("show");
    let expected = StoredProject::from_project(
        laddered_hls_project(high),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 26"));
    // The ladder is written as an inline array of rung objects, in list order
    // and with exactly the authored fields.
    assert!(
        encoded.contains(&format!(
            "\"video_bitrate_kbps\": 1500,\n        \"ladder\": {HLS_LADDER_JSON}"
        )),
        "encoded ladder must be exactly the authored rungs"
    );
    // A destination without a ladder writes an explicit `null`.
    assert!(encoded.contains("\"video_bitrate_kbps\": 9000,\n        \"ladder\": null"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.project().schema_version(), CURRENT_SCHEMA_VERSION);
    let loaded_target = &loaded.project().stream_targets()[1];
    assert_eq!(loaded_target.ladder(), Some(&hls_ladder()));
    assert_eq!(loaded_target.video_bitrate_kbps(), 1_500);
    assert_eq!(
        loaded_target.ladder().unwrap().to_string(),
        "640x360@1000,1280x720@1500"
    );
}

#[test]
fn strict_stream_ladder_parser_rejects_missing_wrong_typed_and_out_of_contract_fields() {
    let high = u128::from(u64::MAX) + 101;
    let temp = TestDirectory::new("strict-stream-ladder");
    let store = temp.store("show");
    let expected = StoredProject::from_project(
        laddered_hls_project(high),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();

    let first_rung = "{\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 360}";
    let nine_rungs = (0..9)
        .map(|index| {
            format!(
                "{{\"video_bitrate_kbps\": {}, \"width\": {}, \"height\": {}}}",
                1_000 + index * 1_000,
                640 + index * 16,
                360 + index * 16,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    for malformed in [
        // Missing and wrong-typed fields: a current manifest carries `ladder`
        // explicitly, as `null` or as an array of rung objects.
        encoded.replacen(",\n        \"ladder\": null", "", 1),
        encoded.replacen(&format!(",\n        \"ladder\": {HLS_LADDER_JSON}"), "", 1),
        encoded.replacen(
            &format!("\"ladder\": {HLS_LADDER_JSON}"),
            "\"ladder\": 7",
            1,
        ),
        encoded.replacen(
            &format!("\"ladder\": {HLS_LADDER_JSON}"),
            "\"ladder\": {\"rungs\": []}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": \"1000\", \"width\": 640, \"height\": 360}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 1000, \"height\": 360}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 360, \"future\": 1}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 1000, \"width\": -640, \"height\": 360}",
            1,
        ),
        // Out-of-contract lists and rungs the model constructors enforce.
        encoded.replacen(HLS_LADDER_JSON, &format!("[{first_rung}]"), 1),
        encoded.replacen(HLS_LADDER_JSON, &format!("[{nine_rungs}]"), 1),
        encoded.replacen(
            HLS_LADDER_JSON,
            "[{\"video_bitrate_kbps\": 1500, \"width\": 1280, \"height\": 720}, {\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 360}]",
            1,
        ),
        encoded.replacen(
            HLS_LADDER_JSON,
            "[{\"video_bitrate_kbps\": 1000, \"width\": 1280, \"height\": 720}, {\"video_bitrate_kbps\": 1500, \"width\": 640, \"height\": 360}]",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 999, \"width\": 640, \"height\": 360}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 1000, \"width\": 641, \"height\": 360}",
            1,
        ),
        encoded.replacen(
            first_rung,
            "{\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 15}",
            1,
        ),
        // A ladder is an HLS-only shape: the RTMP target refuses it through
        // the model's own builder.
        encoded.replacen(
            "\"ladder\": null",
            &format!("\"ladder\": {HLS_LADDER_JSON}"),
            1,
        ),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "expected a malformed manifest, got {error:?}"
        );
    }

    // The untouched manifest still loads after every rejected rewrite.
    fs::write(store.manifest_path(), encoded).unwrap();
    assert_eq!(store.load().unwrap(), expected);
}

#[test]
fn stream_target_running_state_round_trips_and_is_required_at_the_current_schema() {
    let temp = TestDirectory::new("stream-target-running");
    let store = temp.store("show");

    // Both authored desired states survive a full save/load cycle unchanged.
    for running in [true, false] {
        let expected = stored_rich_project_with_running_target(running);
        store.save(&expected).unwrap();
        let encoded = fs::read_to_string(store.manifest_path()).unwrap();
        assert!(encoded.contains(&format!("\"running\": {running}")));
        let loaded = store.load().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(loaded.project().stream_targets()[0].running(), running);
    }

    // The field is part of the current contract: it must be present with a
    // boolean value and the manifest must carry the current schema version.
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(valid.contains("\"schema_version\": 26"));
    for malformed in [
        valid.replacen(",\n        \"running\": true", "", 1),
        valid.replacen("\"running\": true", "\"running\": \"true\"", 1),
        valid.replacen("\"running\": true", "\"running\": 1", 1),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }

    // An older manifest is refused outright; no migration path exists.
    fs::write(
        store.manifest_path(),
        valid.replacen("\"schema_version\": 26", "\"schema_version\": 21", 1),
    )
    .unwrap();
    let error = store.load().unwrap_err();
    assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
}

#[test]
fn recording_intent_and_video_bitrate_round_trip_at_the_current_schema() {
    let temp = TestDirectory::new("recording-bitrate");
    let store = temp.store("show");

    for recording_desired in [[true, false], [false, true]] {
        let mut project = rich_project();
        for (slot, active) in RecorderSlot::ALL.into_iter().zip(recording_desired) {
            project.set_recording_desired(slot, active);
        }
        let expected = StoredProject::from_project(
            project,
            RuntimeRouting::default(),
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap();

        store.save(&expected).unwrap();
        let encoded = fs::read_to_string(store.manifest_path()).unwrap();
        assert!(encoded.contains(&format!("\"recording_desired\": {recording_desired:?}")));
        assert!(
            encoded.contains("\"video_bitrate_kbps\": 9000"),
            "authored video bitrate must be written verbatim"
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(
            loaded.project().recording_desired_slots(),
            recording_desired
        );
        assert_eq!(
            loaded.project().stream_targets()[0].video_bitrate_kbps(),
            9_000
        );
    }

    // Both fields belong to the current contract: missing or wrong-typed
    // values are refused as malformed manifests. The recorder slot array is
    // exactly two booleans in slot order.
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(valid.contains("\"recording_desired\": [false, false]"));
    for malformed in [
        valid.replacen(",\n    \"recording_desired\": [false, false]", "", 1),
        valid.replacen(
            "\"recording_desired\": [false, false]",
            "\"recording_desired\": false",
            1,
        ),
        valid.replacen(
            "\"recording_desired\": [false, false]",
            "\"recording_desired\": [false]",
            1,
        ),
        valid.replacen(
            "\"recording_desired\": [false, false]",
            "\"recording_desired\": [false, false, false]",
            1,
        ),
        valid.replacen(
            "\"recording_desired\": [false, false]",
            "\"recording_desired\": [\"false\", false]",
            1,
        ),
        valid.replacen(
            "\"recording_desired\": [false, false]",
            "\"recording_desired\": [0, false]",
            1,
        ),
        valid.replacen(",\n        \"video_bitrate_kbps\": 9000", "", 1),
        valid.replacen(
            "\"video_bitrate_kbps\": 9000",
            "\"video_bitrate_kbps\": \"9000\"",
            1,
        ),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }

    // The bitrate bound is enforced at decode through the model constructor.
    for out_of_range in ["999", "100001", "0"] {
        fs::write(
            store.manifest_path(),
            valid.replacen(
                "\"video_bitrate_kbps\": 9000",
                &format!("\"video_bitrate_kbps\": {out_of_range}"),
                1,
            ),
        )
        .unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }
}

#[test]
fn strict_stream_destination_parser_rejects_missing_wrong_typed_and_out_of_contract_fields() {
    let temp = TestDirectory::new("strict-stream-destination");
    let store = temp.store("show");
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();

    for malformed in [
        // Missing, wrong-typed, unknown and duplicated fields.
        valid.replacen("\"protocol\": \"rtmps\",\n        ", "", 1),
        valid.replacen("\"protocol\": \"rtmps\"", "\"protocol\": 443", 1),
        valid.replacen(",\n        \"running\": true", "", 1),
        valid.replacen("\"running\": true", "\"running\": \"true\"", 1),
        valid.replacen(
            "\"backup_endpoint\":",
            "\"future_field\": 1,\n        \"backup_endpoint\":",
            1,
        ),
        valid.replacen(
            &format!("\"key\": \"{RICH_STREAM_KEY}\""),
            &format!("\"key\": \"{RICH_STREAM_KEY}\", \"key\": \"{RICH_STREAM_KEY}\""),
            1,
        ),
        valid.replacen("\"output\": ", "\"output\": 0, \"ignored\": ", 1),
        // Values outside the destination contract the sink enforces.
        valid.replacen("\"protocol\": \"rtmps\"", "\"protocol\": \"srt\"", 1),
        valid.replacen(
            "\"endpoint\": \"ingest.example.test:443/live\"",
            "\"endpoint\": \"rtmps://ingest.example.test/live\"",
            1,
        ),
        valid.replacen(
            "\"endpoint\": \"ingest.example.test:443/live\"",
            "\"endpoint\": \"operator:hunter2@ingest.example.test/live\"",
            1,
        ),
        valid.replacen(
            "\"endpoint\": \"ingest.example.test:443/live\"",
            "\"endpoint\": \"ingest.example.test\"",
            1,
        ),
        valid.replacen(
            "\"backup_endpoint\": \"backup.example.test/live/eu\"",
            "\"backup_endpoint\": \"backup.example.test/live/\"",
            1,
        ),
        valid.replacen(
            &format!("\"key\": \"{RICH_STREAM_KEY}\""),
            "\"key\": \"ab\"",
            1,
        ),
        valid.replacen(
            &format!("\"key\": \"{RICH_STREAM_KEY}\""),
            "\"key\": \"has/slash\"",
            1,
        ),
        valid.replacen(
            "\"startup\": \"reconcile_desired_state\",\n        \"output\"",
            "\"startup\": \"running\",\n        \"output\"",
            1,
        ),
    ] {
        fs::write(store.manifest_path(), &malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "expected a malformed manifest, got {error:?}"
        );
        assert!(
            !error.to_string().contains(RICH_STREAM_KEY),
            "stream key leaked into `{error}`"
        );
    }

    // A destination whose output does not exist is a project-level failure,
    // not a syntax one.
    let missing_output = u128::from(u64::MAX) + 999;
    let current_output = u128::from(u64::MAX) + 131;
    fs::write(
        store.manifest_path(),
        valid.replacen(
            &format!("\"output\": {current_output}"),
            &format!("\"output\": {missing_output}"),
            1,
        ),
    )
    .unwrap();
    assert!(matches!(store.load(), Err(StoreError::Validation(_))));
}

#[test]
fn display_p3_bt709_round_trips_in_current_schema() {
    let temp = TestDirectory::new("bt709-transfer");
    let store = temp.store("show");
    let mut settings = rich_settings();
    settings.video.color.primaries = ColorPrimaries::DisplayP3;
    settings.video.color.transfer = TransferFunction::Bt709;
    let expected = StoredProject::from_project(
        Project::new(project_id(77), "BT.709 transfer", settings),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(
        loaded.project().settings().video.color.transfer,
        TransferFunction::Bt709
    );
    assert_eq!(
        loaded.project().settings().video.color.primaries,
        ColorPrimaries::DisplayP3
    );
}

#[test]
fn current_encoding_is_deterministic_for_the_complete_model() {
    let temp = TestDirectory::new("deterministic-rich");
    let first = temp.store("first");
    let second = temp.store("second");
    let project = stored_rich_project();
    first.save(&project).unwrap();
    second.save(&project).unwrap();
    assert_eq!(
        fs::read(first.manifest_path()).unwrap(),
        fs::read(second.manifest_path()).unwrap()
    );
}

#[test]
fn malformed_enum_format_and_reference_are_rejected() {
    let temp = TestDirectory::new("malformed-domain");
    let store = temp.store("show");
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();

    fs::write(
        store.manifest_path(),
        valid.replacen("\"p010\"", "\"future_pixel\"", 1),
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));

    fs::write(
        store.manifest_path(),
        valid.replacen("\"width\": 3840", "\"width\": 0", 1),
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(StoreError::MalformedManifest { .. })
    ));

    let missing_scene = u128::from(u64::MAX) + 999;
    let current_scene = u128::from(u64::MAX) + 112;
    fs::write(
        store.manifest_path(),
        valid.replacen(
            &format!("\"video_source\": {current_scene}"),
            &format!("\"video_source\": {missing_scene}"),
            1,
        ),
    )
    .unwrap();
    assert!(matches!(store.load(), Err(StoreError::Validation(_))));
}

#[test]
fn strict_composition_parser_rejects_unknown_values_ranges_and_fields() {
    let temp = TestDirectory::new("strict-composition");
    let store = temp.store("show");
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();

    for malformed in [
        valid.replacen("\"rotation\": \"deg90\"", "\"rotation\": \"deg45\"", 1),
        valid.replacen(
            "\"translation_x\": -20",
            "\"translation_x\": -2147483649",
            1,
        ),
        valid.replacen(
            "\"rotation\": \"deg90\"",
            "\"rotation\": \"deg90\", \"future\": 1",
            1,
        ),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }

    fs::write(
        store.manifest_path(),
        valid.replacen(
            "\"red\": 8, \"green\": 4, \"blue\": 2, \"alpha\": 16",
            "\"red\": 17, \"green\": 4, \"blue\": 2, \"alpha\": 16",
            1,
        ),
    )
    .unwrap();
    assert!(matches!(store.load(), Err(StoreError::Validation(_))));
}

#[test]
fn strict_rect_mask_parser_rejects_malformed_and_out_of_bounds_values() {
    let temp = TestDirectory::new("strict-mask");
    let store = temp.store("show");
    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();

    for malformed in [
        valid.replacen("\"invert\": true", "\"invert\": 1", 1),
        valid.replacen("\"invert\": true", "\"invert\": true, \"feather\": 1", 1),
        valid.replacen("\"width\": 500", "\"width\": -1", 1),
    ] {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }

    for invalid in [
        valid.replacen("\"width\": 500", "\"width\": 0", 1),
        valid.replacen("\"width\": 500", "\"width\": 1000", 1),
        valid.replacen("\"x\": 11", "\"x\": 4294967295", 1),
    ] {
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(matches!(store.load(), Err(StoreError::Validation(_))));
    }
}

/// A minimal current-contract project whose scene has one effect-free layer
/// and one layer with two ordered descriptors, the second an unknown name.
fn effected_project() -> Project {
    let mut project = Project::new(project_id(1), "Effects", rich_settings())
        .with_main_mix(MainMix::new(input_id(1), input_id(2)));
    for value in [1_u128, 2] {
        project.add_input(Input {
            id: input_id(value),
            name: format!("Input {value}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Effects".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            Layer {
                name: "Plain".into(),
                source: SourceRef::Input(input_id(1)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
                crop: None,
                mask: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 0,
            },
            Layer {
                name: "Effected".into(),
                source: SourceRef::Input(input_id(2)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
                crop: None,
                mask: None,
                effects: vec![
                    LayerEffect::new("levels", vec![0, 128]),
                    LayerEffect::new("future.effect", vec![i32::MIN, -1, 0, 7]),
                ],
                opacity: 255,
                z_order: 1,
            },
        ],
    });
    project
}

#[test]
fn scene_layer_effects_round_trip_in_order_at_the_current_schema() {
    let temp = TestDirectory::new("scene-layer-effects");
    let store = temp.store("show");
    let expected = StoredProject::from_project(
        effected_project(),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 26"));
    // Every layer writes the field explicitly in layer order: the plain layer
    // as an empty array, the effected layer as inline ordered descriptors.
    assert!(
        encoded.contains(
            "\"effects\": [],\n            \"opacity\": 255,\n            \"z_order\": 0"
        )
    );
    assert!(encoded.contains(
        "\"effects\": [{\"name\": \"levels\", \"parameters\": [0, 128]}, {\"name\": \
         \"future.effect\", \"parameters\": [-2147483648, -1, 0, 7]}]"
    ));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.project().schema_version(), CURRENT_SCHEMA_VERSION);
    assert_eq!(
        loaded.project().scenes()[0].layers[0].effects,
        [] as [fm_model::LayerEffect; 0]
    );
    assert_eq!(
        loaded.project().scenes()[0].layers[1].effects,
        vec![
            LayerEffect::new("levels", vec![0, 128]),
            LayerEffect::new("future.effect", vec![i32::MIN, -1, 0, 7]),
        ]
    );
}

/// Saves [`effected_project`] and returns its store plus the encoded manifest.
fn stored_effected_manifest(name: &str) -> (TestDirectory, ProjectStore, String) {
    let temp = TestDirectory::new(name);
    let store = temp.store("show");
    store
        .save(
            &StoredProject::from_project(
                effected_project(),
                RuntimeRouting::default(),
                ProjectPosition::default(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    (temp, store, encoded)
}

#[test]
fn strict_layer_effect_parser_rejects_missing_mistyped_unknown_and_duplicate_fields() {
    let (_temp, store, valid) = stored_effected_manifest("strict-effects-fields");
    let descriptor = "{\"name\": \"levels\", \"parameters\": [0, 128]}";
    let effects_array = format!(
        "[{descriptor}, {{\"name\": \"future.effect\", \"parameters\": [-2147483648, -1, 0, 7]}}]"
    );

    for (index, malformed) in [
        // Missing and mistyped fields: the current contract carries `effects`
        // explicitly as an array of descriptor objects.
        valid.replacen(
            ",\n            \"effects\": [],\n            \"opacity\"",
            ",\n            \"opacity\"",
            1,
        ),
        valid.replacen(
            &format!("\"effects\": {effects_array}"),
            "\"effects\": null",
            1,
        ),
        valid.replacen(
            &format!("\"effects\": {effects_array}"),
            "\"effects\": 7",
            1,
        ),
        valid.replacen(
            &format!("\"effects\": {effects_array}"),
            "\"effects\": [7]",
            1,
        ),
        valid.replacen(descriptor, "{\"parameters\": [0, 128]}", 1),
        valid.replacen(descriptor, "{\"name\": 7, \"parameters\": [0, 128]}", 1),
        valid.replacen(descriptor, "{\"name\": \"levels\"}", 1),
        valid.replacen(descriptor, "{\"name\": \"levels\", \"parameters\": 0}", 1),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [0, \"128\"]}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [0, 1.5]}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [0, 4294967296]}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [0, 128], \"future\": 1}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"name\": \"levels\", \"parameters\": [0, 128]}",
            1,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "malformed effects {index} were accepted"
        );
    }

    // The untouched manifest still loads after every rejected rewrite.
    fs::write(store.manifest_path(), valid).unwrap();
    assert_eq!(store.load().unwrap().project().scenes()[0].layers.len(), 2);
}

#[test]
fn layer_effect_bounds_are_rejected_at_the_current_schema() {
    let (_temp, store, valid) = stored_effected_manifest("strict-effects-bounds");
    let descriptor = "{\"name\": \"levels\", \"parameters\": [0, 128]}";
    let effects_array = format!(
        "[{descriptor}, {{\"name\": \"future.effect\", \"parameters\": [-2147483648, -1, 0, 7]}}]"
    );
    let high_parameters = (0..=LayerEffect::MAX_PARAMETERS)
        .map(|parameter| parameter.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let many_effects = (0..=LayerEffect::MAX_EFFECTS_PER_LAYER)
        .map(|_| descriptor)
        .collect::<Vec<_>>()
        .join(", ");

    // The compositor descriptor bounds are part of the current contract.
    for (index, malformed) in [
        valid.replacen(descriptor, "{\"name\": \"\", \"parameters\": [0, 128]}", 1),
        valid.replacen(
            descriptor,
            &format!(
                "{{\"name\": \"{}\", \"parameters\": [0, 128]}}",
                "x".repeat(65)
            ),
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\\u0007\", \"parameters\": [0, 128]}",
            1,
        ),
        valid.replacen(
            descriptor,
            &format!("{{\"name\": \"levels\", \"parameters\": [{high_parameters}]}}"),
            1,
        ),
        valid.replacen(
            &format!("\"effects\": {effects_array}"),
            &format!("\"effects\": [{many_effects}]"),
            1,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "out-of-bounds effects {index} were accepted"
        );
    }

    fs::write(store.manifest_path(), valid).unwrap();
    assert_eq!(store.load().unwrap().project().scenes()[0].layers.len(), 2);
}

fn discrete(number: u8) -> Channel {
    Channel::Discrete(DiscreteChannel::new(number).unwrap())
}

fn stored_with_audio_channels(channels: Vec<Channel>) -> StoredProject {
    let mut settings = rich_settings();
    settings.audio.channels = ChannelLayout::new(channels).unwrap();
    StoredProject::from_project(
        Project::new(project_id(77), "Multichannel", settings),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn rear_surround_and_discrete_channel_labels_round_trip_at_the_current_schema() {
    let seven_one = vec![
        Channel::Left,
        Channel::Right,
        Channel::Center,
        Channel::LowFrequency,
        Channel::LeftSurround,
        Channel::RightSurround,
        Channel::LeftRearSurround,
        Channel::RightRearSurround,
    ];
    // Discrete labels may mix with named labels; one- and two-digit numbers.
    let discrete_layout = vec![
        discrete(1),
        discrete(2),
        discrete(9),
        discrete(10),
        discrete(16),
        Channel::Mono,
    ];
    for (name, channels, encoded) in [
        (
            "seven-one",
            seven_one,
            "\"left_surround\", \"right_surround\", \"left_rear_surround\", \"right_rear_surround\"",
        ),
        (
            "discrete",
            discrete_layout,
            "\"discrete_1\", \"discrete_2\", \"discrete_9\", \"discrete_10\", \"discrete_16\", \"mono\"",
        ),
    ] {
        let temp = TestDirectory::new(name);
        let store = temp.store("show");
        let expected = stored_with_audio_channels(channels);
        store.save(&expected).unwrap();
        let manifest = fs::read_to_string(store.manifest_path()).unwrap();
        let compact: String = manifest.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(compact.contains(encoded), "{compact}");
        let loaded = store.load().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(
            loaded.project().settings().audio.channels,
            expected.project().settings().audio.channels
        );
    }
}

#[test]
fn malformed_discrete_channel_labels_are_rejected() {
    let temp = TestDirectory::new("malformed-discrete");
    let store = temp.store("show");
    store
        .save(&stored_with_audio_channels(vec![discrete(16)]))
        .unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(valid.contains("\"discrete_16\""));
    for malformed in [
        "discrete_0",
        "discrete_17",
        "discrete_016",
        "discrete_",
        "discrete_+1",
        "discrete_x",
        "Discrete_16",
        "back_left",
    ] {
        fs::write(
            store.manifest_path(),
            valid.replacen("\"discrete_16\"", &format!("\"{malformed}\""), 1),
        )
        .unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{malformed}"
        );
    }
}

/// A project with two virtual inputs (one cropped alias and one alias of that
/// alias) and both bus feeds, the Program feed being on Program itself.
fn virtual_input_project() -> Project {
    let mut project = Project::new(project_id(1), "Virtual", rich_settings())
        .with_main_mix(MainMix::new(input_id(5), input_id(4)));
    for value in [1_u128, 2] {
        project.add_input(Input {
            id: input_id(value),
            name: format!("Input {value}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    for (value, name, kind) in [
        (
            3_u128,
            "Close-up",
            InputKind::Alias {
                target: input_id(1),
                crop: Some(CropRect::new(10, 20, 640, 360)),
            },
        ),
        (
            4,
            "Close-up 2",
            InputKind::Alias {
                target: input_id(3),
                crop: None,
            },
        ),
        (5, "Program feed", InputKind::MixFeed(MixFeed::Program)),
        (6, "Preview feed", InputKind::MixFeed(MixFeed::Preview)),
    ] {
        project.add_input(Input {
            id: input_id(value),
            name: name.into(),
            kind,
            required_capabilities: Vec::new(),
        });
    }
    project
}

#[test]
fn alias_and_mix_feed_inputs_round_trip_exactly_at_the_current_schema() {
    let temp = TestDirectory::new("virtual-inputs");
    let store = temp.store("show");
    let expected = StoredProject::from_project(
        virtual_input_project(),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 26"));
    assert!(encoded.contains(
        "{\"type\": \"alias\", \"target\": 1, \"crop\": {\"x\": 10, \"y\": 20, \"width\": 640, \
         \"height\": 360}}"
    ));
    assert!(encoded.contains("{\"type\": \"alias\", \"target\": 3, \"crop\": null}"));
    assert!(encoded.contains("{\"type\": \"mix_feed\", \"feed\": \"program\"}"));
    assert!(encoded.contains("{\"type\": \"mix_feed\", \"feed\": \"preview\"}"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(
        loaded.project().inputs()[2].kind,
        InputKind::Alias {
            target: input_id(1),
            crop: Some(CropRect::new(10, 20, 640, 360)),
        }
    );
    assert_eq!(
        loaded.project().inputs()[4].kind,
        InputKind::MixFeed(MixFeed::Program)
    );
    // Every alias and feed keeps its own audio strip.
    assert_eq!(loaded.project().input_audio_strips().len(), 6);

    // Field presence, types, and enum spellings are exact.
    for malformed in [
        encoded.replacen(", \"crop\": null}", "}", 1),
        encoded.replacen("\"target\": 3", "\"target\": 0", 1),
        encoded.replacen("\"target\": 3", "\"target\": \"3\"", 1),
        encoded.replacen("\"feed\": \"program\"", "\"feed\": \"aux\"", 1),
        encoded.replacen("\"feed\": \"program\"", "\"feed\": \"Program\"", 1),
        encoded.replacen("\"type\": \"mix_feed\"", "\"type\": \"program\"", 1),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "got {error:?}"
        );
    }

    // Well-formed but invalid references are rejected by project validation.
    for invalid in [
        // Missing target.
        encoded.replacen("\"target\": 3", "\"target\": 99", 1),
        // Self reference.
        encoded.replacen("\"target\": 3", "\"target\": 4", 1),
        // Two-alias cycle: 3 -> 4 -> 3.
        encoded.replacen("\"target\": 1", "\"target\": 4", 1),
        // Empty and out-of-frame crops.
        encoded.replacen("\"width\": 640", "\"width\": 0", 1),
        encoded.replacen("\"x\": 10", "\"x\": 4294967295", 1),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), invalid).unwrap();
        let error = store.load().unwrap_err();
        assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
    }
}

#[test]
fn edit_locks_round_trip_exactly_and_are_strict_at_the_current_schema() {
    let temp = TestDirectory::new("edit-locks");
    let store = temp.store("show");
    let high = u128::from(u64::MAX) + 101;

    // An unlocked project writes an explicit, empty padlock.
    store.save(&stored_rich_project()).unwrap();
    let unlocked = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(unlocked.contains(
        "\"recording_desired\": [false, false],\n    \"edit_locks\": {\"project\": false, \"inputs\": []}\n  },\n  \"runtime\": {"
    ));

    // Locks are written in identifier order regardless of the order applied.
    let mut project = rich_project();
    project.set_project_locked(true);
    project
        .set_input_locked_checked(input_id(high + 2), true)
        .unwrap();
    project
        .set_input_locked_checked(input_id(high), true)
        .unwrap();
    let expected = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 26"));
    let locks = "\"edit_locks\": {\"project\": true, \"inputs\": [18446744073709551716, 18446744073709551718]}";
    assert!(
        encoded.contains(&format!(
            "\"recording_desired\": [false, false],\n    {locks}\n  }},\n  \"runtime\": {{"
        )),
        "unexpected edit lock encoding in {encoded}"
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    assert!(loaded.project().project_locked());
    assert_eq!(
        loaded
            .project()
            .locked_inputs()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![input_id(high), input_id(high + 2)]
    );

    // The field is required, typed, canonical, and duplicate-free.
    for malformed in [
        encoded.replacen(&format!(",\n    {locks}"), "", 1),
        encoded.replacen(locks, "\"edit_locks\": null", 1),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": 1, \"inputs\": []}",
            1,
        ),
        encoded.replacen(locks, "\"edit_locks\": {\"inputs\": []}", 1),
        encoded.replacen(locks, "\"edit_locks\": {\"project\": true}", 1),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [], \"extra\": 1}",
            1,
        ),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [18446744073709551718, 18446744073709551716]}",
            1,
        ),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [18446744073709551716, 18446744073709551716]}",
            1,
        ),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [0]}",
            1,
        ),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [\"18446744073709551716\"]}",
            1,
        ),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(matches!(
            store.load(),
            Err(StoreError::MalformedManifest { .. })
        ));
    }

    // A lock on an input the project does not contain is a domain error.
    fs::write(
        store.manifest_path(),
        encoded.replacen(
            locks,
            "\"edit_locks\": {\"project\": true, \"inputs\": [99]}",
            1,
        ),
    )
    .unwrap();
    let error = store.load().unwrap_err();
    assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
}
