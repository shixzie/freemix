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
    SolidColor, SourceRef, SrtPassphrase, StartupPolicy, StingerAudioPolicy, StingerConfig,
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
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: vec!["codec.video.hevc".into()],
        },
        Input {
            id: input_id(high + 2),
            name: "Device".into(),
            kind: InputKind::Device {
                stable_key: "decklink:1".into(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
            required_capabilities: vec!["capture.device.sdi".into()],
        },
        Input {
            id: input_id(high + 3),
            name: "Network".into(),
            kind: InputKind::NetworkStream(fm_types::network_stream::NetworkStreamConfig::new(
                "rtsp://user:pass@example.test:554/stream",
            )),
            required_capabilities: vec!["network.input.rtsp".into()],
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

#[allow(clippy::too_many_lines)] // one literal of every current field
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
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Camera".into(),
            source: SourceRef::Input(input_id(high + 2)),
            enabled: true,
            geometry: LayerGeometry::new(-20, 30, 1920, 1080, Rotation::Deg90),
            crop: Some(CropRect::new(10, 20, 1000, 700)),
            mask: Some(RectMask::new(11, 12, 500, 300).inverted(true)),
            key: None,
            effects: Vec::new(),
            opacity: 200,
            z_order: -7,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_scene(Scene {
        id: scene_id(high + 11),
        name: "Composite".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            Layer {
                id: fm_model::LayerId::new(2).unwrap(),
                name: "Base scene".into(),
                source: SourceRef::Scene(scene_id(high + 10)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 3840, 2160, Rotation::Deg0),
                crop: None,
                mask: None,
                key: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 3,
            },
            Layer {
                id: fm_model::LayerId::new(3).unwrap(),
                name: "Overlay".into(),
                source: SourceRef::Input(input_id(high + 4)),
                enabled: false,
                geometry: LayerGeometry::new(100, -200, 640, 360, Rotation::Deg270),
                crop: Some(CropRect::new(1, 2, 3, 4)),
                mask: Some(RectMask::new(1, 1, 2, 3)),
                key: None,
                effects: Vec::new(),
                opacity: 127,
                z_order: 3,
            },
        ],
        animation: fm_model::SceneAnimation::default(),
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
        pan: fm_model::InputPanBasisPoints::new(-4_000).unwrap(),
        gain: InputGainMilliDb::new(-12_345).unwrap(),
        balance: InputBalanceBasisPoints::new(2_500).unwrap(),
        delay_samples: InputDelaySamples::new(1_200).unwrap(),
        muted: true,
        soloed: true,
        follow_video: false,
        sends: fm_types::audio_bus::AudioSends::from_mask(
            fm_types::audio_bus::AudioBusMask::from_token("BG").unwrap(),
        ),
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
            "\"gain_milli_db\": -12345, \"balance_basis_points\": 2500, \"pan_basis_points\": -4000, \"delay_samples\": 1200, \"muted\": true, \"soloed\": true, \"follow_video\": false, \"sends\": \"B@0,G@0\", \"label\": \"\", \"group\": null"
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
        encoded.replacen("\"sends\": \"B@0,G@0\"", "\"buses\": \"GB\"", 1),
        encoded.replacen("\"sends\": \"B@0,G@0\"", "\"buses\": \"MM\"", 1),
        encoded.replacen("\"sends\": \"B@0,G@0\", ", "", 1),
        encoded.replacen(
            "\"pan_basis_points\": -4000",
            "\"pan_basis_points\": -10001",
            1,
        ),
        encoded.replacen("\"pan_basis_points\": -4000, ", "", 1),
        encoded.replacen(
            "\"follow_video\": false, \"sends\": \"B@0,G@0\", \"label\": \"\", \"group\": null",
            "\"follow_video\": false, \"sends\": \"B@0,G@0\", \"label\": \"\", \"group\": 5",
            1,
        ),
        encoded.replacen(
            "\"follow_video\": false, \"sends\": \"B@0,G@0\", \"label\": \"\", \"group\": null",
            "\"follow_video\": false, \"sends\": \"B@0,G@0\", \"label\": null, \"group\": null",
            1,
        ),
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
    const PASSPHRASE: &str = "srt-roundtrip-passphrase";
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
    .unwrap()
    .with_srt_latency_ms(350)
    .unwrap()
    .with_srt_passphrase(Some(SrtPassphrase::parse(PASSPHRASE).unwrap()))
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
    // Schema 51: the latency and the passphrase (the bundle's trust model,
    // like the key).
    assert!(encoded.contains("\"srt_latency_ms\": 350"), "{encoded}");
    assert!(encoded.contains(&format!("\"srt_passphrase\": \"{PASSPHRASE}\"")));

    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    let target = &loaded.project().stream_targets()[0];
    assert_eq!(target.protocol(), StreamProtocol::Srt);
    assert_eq!(target.srt_latency_ms(), Some(350));
    assert_eq!(
        target.srt_passphrase().map(SrtPassphrase::expose_secret),
        Some(PASSPHRASE)
    );
    assert!(!format!("{target:?}").contains(PASSPHRASE));

    // Strict: SRT needs its latency in range, other protocols carry none,
    // the passphrase follows libsrt's bounds, and no refusal repeats it.
    for malformed in [
        encoded.replacen(",\n        \"srt_latency_ms\": 350", "", 1),
        encoded.replacen("\"srt_latency_ms\": 350", "\"srt_latency_ms\": null", 1),
        encoded.replacen("\"srt_latency_ms\": 350", "\"srt_latency_ms\": 19", 1),
        encoded.replacen("\"srt_latency_ms\": 350", "\"srt_latency_ms\": 8001", 1),
        encoded.replacen("\"srt_latency_ms\": 350", "\"srt_latency_ms\": \"350\"", 1),
        encoded.replacen(PASSPHRASE, "short", 1),
        encoded.replacen(PASSPHRASE, "has spaces in it", 1),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let refused = store.load().unwrap_err();
        assert!(!format!("{refused:?}").contains(PASSPHRASE), "{refused:?}");
    }
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
    assert!(encoded.contains("\"schema_version\": 101"));
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
    assert!(encoded.contains("\"schema_version\": 101"));
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
    assert!(valid.contains("\"schema_version\": 101"));
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
        valid.replacen("\"schema_version\": 101", "\"schema_version\": 21", 1),
    )
    .unwrap();
    let error = store.load().unwrap_err();
    assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
}

/// OR-006 (schema 49): each destination's reconnect policy is stored
/// exactly — a finite or unlimited attempt budget — and is a required,
/// bounded part of the current contract.
#[test]
fn stream_target_reconnect_policy_round_trips_and_is_strict_at_the_current_schema() {
    let temp = TestDirectory::new("stream-target-reconnect");
    let store = temp.store("show");
    for (policy, attempts) in [
        (
            fm_model::StreamReconnectPolicy::new(500, 8_000, Some(12)).unwrap(),
            "12",
        ),
        (
            fm_model::StreamReconnectPolicy::new(100, 300_000, None).unwrap(),
            "null",
        ),
    ] {
        let mut project = rich_project();
        let target = project.stream_targets()[0].clone().with_reconnect(policy);
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
        assert!(
            encoded.contains(&format!(
                "\"reconnect\": {{\"initial_delay_ms\": {}, \"max_delay_ms\": {}, \"max_attempts\": {attempts}}}",
                policy.initial_delay_ms(),
                policy.max_delay_ms()
            )),
            "{encoded}"
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(loaded.project().stream_targets()[0].reconnect(), policy);
    }

    store.save(&stored_rich_project()).unwrap();
    let valid = fs::read_to_string(store.manifest_path()).unwrap();
    let default =
        "\"reconnect\": {\"initial_delay_ms\": 250, \"max_delay_ms\": 2000, \"max_attempts\": 40}";
    assert!(valid.contains(default), "{valid}");
    for malformed in [
        valid.replacen(&format!(",\n        {default}"), "", 1),
        valid.replacen("\"initial_delay_ms\": 250", "\"initial_delay_ms\": 99", 1),
        valid.replacen("\"max_delay_ms\": 2000", "\"max_delay_ms\": 200", 1),
        valid.replacen("\"max_attempts\": 40", "\"max_attempts\": 0", 1),
        valid.replacen("\"max_attempts\": 40", "\"max_attempts\": \"40\"", 1),
        valid.replacen(
            "\"max_attempts\": 40}",
            "\"max_attempts\": 40, \"extra\": 1}",
            1,
        ),
    ] {
        assert_ne!(malformed, valid);
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(store.load().is_err());
    }
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
        // SRT options on an RTMPS destination (schema 51).
        valid.replacen("\"srt_latency_ms\": null", "\"srt_latency_ms\": 120", 1),
        valid.replacen(
            "\"srt_passphrase\": null",
            "\"srt_passphrase\": \"rtmps-passphrase\"",
            1,
        ),
        valid.replacen(",\n        \"srt_passphrase\": null", "", 1),
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
                id: fm_model::LayerId::new(4).unwrap(),
                name: "Plain".into(),
                source: SourceRef::Input(input_id(1)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
                crop: None,
                mask: None,
                key: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 0,
            },
            Layer {
                id: fm_model::LayerId::new(5).unwrap(),
                name: "Effected".into(),
                source: SourceRef::Input(input_id(2)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0),
                crop: None,
                mask: None,
                key: Some(fm_model::LayerKey::Chroma(fm_model::ChromaLayerKey {
                    red: 0,
                    green: 255,
                    blue: 0,
                    similarity: 64,
                    smoothness: 32,
                    spill: 128,
                    refinement: fm_model::KeyRefinement::NONE,
                })),
                effects: vec![
                    LayerEffect::new("levels", vec![0, 128]),
                    LayerEffect::new("sharpen", vec![1000]),
                ],
                opacity: 255,
                z_order: 1,
            },
        ],
        animation: fm_model::SceneAnimation::default(),
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
    assert!(encoded.contains("\"schema_version\": 101"));
    // Every layer writes the field explicitly in layer order: the plain layer
    // as an empty array, the effected layer as inline ordered descriptors.
    assert!(
        encoded.contains(
            "\"effects\": [],\n            \"opacity\": 255,\n            \"z_order\": 0"
        )
    );
    assert!(encoded.contains(
        "\"effects\": [{\"name\": \"levels\", \"parameters\": [0, 128]}, {\"name\": \
         \"sharpen\", \"parameters\": [1000]}]"
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
            LayerEffect::new("sharpen", vec![1000]),
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
    let effects_array =
        format!("[{descriptor}, {{\"name\": \"sharpen\", \"parameters\": [1000]}}]");

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
fn key_fill_keys_and_refinements_round_trip_at_the_current_schema() {
    let temp = TestDirectory::new("key-fill-refinements");
    let store = temp.store("show");
    let mut project = effected_project();
    project
        .set_scene_layer_key(
            scene_id(1),
            1,
            Some(fm_model::LayerKey::Fill(fm_model::FillLayerKey {
                key_input: input_id(1),
                channel: fm_model::KeyChannel::Alpha,
                low: 8,
                high: 247,
                softness: 90,
                invert: true,
                refinement: fm_model::KeyRefinement {
                    clip_black: 12,
                    clip_white: 230,
                    choke: -8,
                    edge_softness: 8,
                    despill: fm_model::KeyDespill::Green { strength: 200 },
                    light_wrap: 150,
                    light_wrap_width: 8,
                    garbage_matte: Some(
                        fm_types::GarbageMatte::new(
                            &[
                                fm_types::MattePoint::new(100, 200),
                                fm_types::MattePoint::new(900, 200),
                                fm_types::MattePoint::new(500, 900),
                            ],
                            true,
                        )
                        .and_then(|matte| matte.with_feather(6))
                        .unwrap(),
                    ),
                },
            })),
        )
        .unwrap();
    project
        .set_scene_layer_key(
            scene_id(1),
            0,
            Some(fm_model::LayerKey::Luma(fm_model::LumaLayerKey {
                refinement: fm_model::KeyRefinement {
                    choke: 3,
                    despill: fm_model::KeyDespill::Blue { strength: 1 },
                    ..fm_model::KeyRefinement::NONE
                },
                ..fm_model::LumaLayerKey::BLACK
            })),
        )
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
    assert!(
        encoded.contains(
            "\"key\": {\"type\": \"fill\", \"key_input\": 1, \"channel\": \"alpha\", \
             \"low\": 8, \"high\": 247, \"softness\": 90, \"invert\": true, \"refinement\": \
             {\"clip_black\": 12, \"clip_white\": 230, \"choke\": -8, \"edge_softness\": 8, \
             \"despill\": {\"type\": \"green\", \"strength\": 200}, \"light_wrap\": 150, \
             \"light_wrap_width\": 8, \"garbage_matte\": {\"invert\": true, \"points\": \
             [[100, 200], [900, 200], [500, 900]], \"feather\": 6}}}"
        ),
        "{encoded}"
    );
    assert_eq!(store.load().unwrap(), expected);
}

#[allow(clippy::too_many_lines)] // one table: every bound of keys and effects
#[test]
fn layer_effect_bounds_are_rejected_at_the_current_schema() {
    let (_temp, store, valid) = stored_effected_manifest("strict-effects-bounds");
    let descriptor = "{\"name\": \"levels\", \"parameters\": [0, 128]}";
    let effects_array =
        format!("[{descriptor}, {{\"name\": \"sharpen\", \"parameters\": [1000]}}]");
    let many_effects = (0..=LayerEffect::MAX_EFFECTS_PER_LAYER)
        .map(|_| descriptor)
        .collect::<Vec<_>>()
        .join(", ");

    let refinement = "\"refinement\": {\"clip_black\": 0, \"clip_white\": 255, \"choke\": 0, \
                      \"edge_softness\": 0, \"despill\": {\"type\": \"off\"}, \
                      \"light_wrap\": 0, \"light_wrap_width\": 0, \"garbage_matte\": null}";
    let key = format!(
        "\"key\": {{\"type\": \"chroma\", \"red\": 0, \"green\": 255, \"blue\": 0, \
         \"similarity\": 64, \"smoothness\": 32, \"spill\": 128, {refinement}}}"
    );
    let key = key.as_str();
    assert!(valid.contains(key));
    assert!(valid.contains("\"key\": null"));

    // The effect table and the key bounds are part of the current contract.
    for (index, malformed) in [
        valid.replacen(descriptor, "{\"name\": \"\", \"parameters\": [0, 128]}", 1),
        valid.replacen(descriptor, "{\"name\": \"blur\", \"parameters\": [3]}", 1),
        valid.replacen(descriptor, "{\"name\": \"levels\", \"parameters\": [0]}", 1),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [128, 0]}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\", \"parameters\": [0, 256]}",
            1,
        ),
        valid.replacen(
            descriptor,
            "{\"name\": \"levels\\u0007\", \"parameters\": [0, 128]}",
            1,
        ),
        valid.replacen(key, "\"key\": {\"type\": \"alpha\"}", 1),
        valid.replacen(key, &key.replace("\"spill\": 128", "\"spill\": 256"), 1),
        valid.replacen(key, &key.replace(", \"spill\": 128", ""), 1),
        valid.replacen(
            key,
            &format!(
                "\"key\": {{\"type\": \"luma\", \"low\": 40, \"high\": 40, \"softness\": 0, \
                 \"invert\": false, {refinement}}}"
            ),
            1,
        ),
        // Refinements are required and bounded (schema 61, SW-008).
        valid.replacen(key, &key.replace(&format!(", {refinement}"), ""), 1),
        valid.replacen(
            key,
            &key.replace("\"clip_black\": 0", "\"clip_black\": 255"),
            1,
        ),
        valid.replacen(key, &key.replace("\"choke\": 0", "\"choke\": 9"), 1),
        valid.replacen(key, &key.replace("\"choke\": 0", "\"choke\": -200"), 1),
        valid.replacen(
            key,
            &key.replace("\"edge_softness\": 0", "\"edge_softness\": 9"),
            1,
        ),
        // Light wrap is required and bounded (schema 79).
        valid.replacen(key, &key.replace(", \"light_wrap_width\": 0", ""), 1),
        valid.replacen(
            key,
            &key.replace("\"light_wrap\": 0", "\"light_wrap\": 10"),
            1,
        ),
        valid.replacen(
            key,
            &key.replace("\"light_wrap_width\": 0", "\"light_wrap_width\": 2"),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"light_wrap\": 0, \"light_wrap_width\": 0",
                "\"light_wrap\": 10, \"light_wrap_width\": 9",
            ),
            1,
        ),
        // The garbage matte is required: null or three to eight in-range
        // points (schema 79).
        valid.replacen(key, &key.replace(", \"garbage_matte\": null", ""), 1),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": false, \"points\": [[0, 0], [1000, 0]]}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": false, \"points\": \
                 [[0, 0], [1001, 0], [0, 1000]]}",
            ),
            1,
        ),
        // ... not all on one line, which would key the whole layer out.
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": true, \"points\": \
                 [[0, 0], [500, 500], [1000, 1000]]}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": false, \"points\": \
                 [[0, 0], [1000, 0, 1], [0, 1000]]}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"points\": [[0, 0], [1000, 0], [0, 1000]]}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": false, \"points\": \
                 [[0, 0], [1000, 0], [0, 1000]], \"feather\": 65}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "\"garbage_matte\": null",
                "\"garbage_matte\": {\"invert\": false, \"points\": \
                 [[0, 0], [1000, 0], [0, 1000]]}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "{\"type\": \"off\"}",
                "{\"type\": \"off\", \"strength\": 1}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace(
                "{\"type\": \"off\"}",
                "{\"type\": \"red\", \"strength\": 1}",
            ),
            1,
        ),
        valid.replacen(
            key,
            &key.replace("{\"type\": \"off\"}", "{\"type\": \"green\"}"),
            1,
        ),
        // A key/fill key names its key input and a known channel.
        valid.replacen(
            key,
            &format!(
                "\"key\": {{\"type\": \"fill\", \"key_input\": 0, \"channel\": \"luma\", \
                 \"low\": 0, \"high\": 255, \"softness\": 255, \"invert\": false, {refinement}}}"
            ),
            1,
        ),
        valid.replacen(
            key,
            &format!(
                "\"key\": {{\"type\": \"fill\", \"key_input\": 1, \"channel\": \"red\", \
                 \"low\": 0, \"high\": 255, \"softness\": 255, \"invert\": false, {refinement}}}"
            ),
            1,
        ),
        valid.replacen("\"key\": null,\n", "", 1),
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
    assert!(encoded.contains("\"schema_version\": 101"));
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
        "\"recording_desired\": [false, false],\n    \"edit_locks\": {\"project\": false, \"inputs\": []},\n    \"playlist\": {\"loop\": \"off\", \"entries\": [], \"state\": {\"type\": \"stopped\"}, \"next_take\": 1, \"observed_frame\": 0},\n    \"cue_lists\": [],\n    \"luts\": [],\n    \"grades\": [],\n    \"input_grades\": [],\n    \"layer_grades\": [],\n    \"program_grade\": null,\n    \"animation_presets\": [],\n    \"transition_favorites\": [\n      {\"effect\": \"fade\", \"duration_frames\": 30},\n      {\"effect\": \"wipe\", \"duration_frames\": 30},\n      {\"effect\": \"fly\", \"duration_frames\": 30},\n      {\"effect\": \"cube\", \"duration_frames\": 30}\n    ],\n    \"input_categories\": [],\n    \"input_lifecycle\": [],\n    \"audio_groups\": [\n      {\"name\": \"Group 1\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null},\n      {\"name\": \"Group 2\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null},\n      {\"name\": \"Group 3\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null},\n      {\"name\": \"Group 4\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}\n    ],\n    \"aux_buses\": [\n      {\"name\": \"Bus A\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus B\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus C\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus D\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus E\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus F\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false},\n      {\"name\": \"Bus G\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}\n    ],\n    \"output_aux_buses\": [],\n    \"output_mix_minus\": [],\n    \"audio_solo_mode\": \"sip\",\n    \"audio_monitor_output\": {\"device\": null, \"gain_milli_db\": 0, \"muted\": false},\n    \"master_audio_dsp\": null,\n    \"master_audio_plugins\": null,\n    \"fade_to_black_excluded_outputs\": [],\n    \"fade_to_black_output_durations\": [],\n    \"fade_to_black_curve\": \"linear\",\n    \"display_outputs\": [],\n    \"live_lan\": {\"enabled\": false, \"source\": \"program\", \"max_viewers\": 8, \"pin\": null},\n    \"scripts\": [],\n    \"script_runs\": {\"last_run\": 0, \"runs\": [], \"faults\": []},\n    \"shortcuts\": [],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": [],\n    \"replay\": {\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}\n  },\n  \"runtime\": {"
    ), "{unlocked}");

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
    assert!(encoded.contains("\"schema_version\": 101"));
    let locks = "\"edit_locks\": {\"project\": true, \"inputs\": [18446744073709551716, 18446744073709551718]}";
    assert!(
        encoded.contains(&format!(
            "\"recording_desired\": [false, false],\n    {locks},\n    \"playlist\": {{\"loop\": \"off\", \"entries\": [], \"state\": {{\"type\": \"stopped\"}}, \"next_take\": 1, \"observed_frame\": 0}},\n    \"cue_lists\": [],\n    \"luts\": [],\n    \"grades\": [],\n    \"input_grades\": [],\n    \"layer_grades\": [],\n    \"program_grade\": null,\n    \"animation_presets\": [],\n    \"transition_favorites\": [\n      {{\"effect\": \"fade\", \"duration_frames\": 30}},\n      {{\"effect\": \"wipe\", \"duration_frames\": 30}},\n      {{\"effect\": \"fly\", \"duration_frames\": 30}},\n      {{\"effect\": \"cube\", \"duration_frames\": 30}}\n    ],\n    \"input_categories\": [],\n    \"input_lifecycle\": [],\n    \"audio_groups\": [\n      {{\"name\": \"Group 1\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}},\n      {{\"name\": \"Group 2\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}},\n      {{\"name\": \"Group 3\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}},\n      {{\"name\": \"Group 4\", \"gain_milli_db\": 0, \"muted\": false, \"soloed\": false, \"parent\": null}}\n    ],\n    \"aux_buses\": [\n      {{\"name\": \"Bus A\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus B\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus C\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus D\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus E\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus F\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}},\n      {{\"name\": \"Bus G\", \"gain_milli_db\": 0, \"muted\": false, \"dsp\": null, \"device\": null, \"omt\": false}}\n    ],\n    \"output_aux_buses\": [],\n    \"output_mix_minus\": [],\n    \"audio_solo_mode\": \"sip\",\n    \"audio_monitor_output\": {{\"device\": null, \"gain_milli_db\": 0, \"muted\": false}},\n    \"master_audio_dsp\": null,\n    \"master_audio_plugins\": null,\n    \"fade_to_black_excluded_outputs\": [],\n    \"fade_to_black_output_durations\": [],\n    \"fade_to_black_curve\": \"linear\",\n    \"display_outputs\": [],\n    \"live_lan\": {{\"enabled\": false, \"source\": \"program\", \"max_viewers\": 8, \"pin\": null}},\n    \"scripts\": [],\n    \"script_runs\": {{\"last_run\": 0, \"runs\": [], \"faults\": []}},\n    \"shortcuts\": [],\n    \"media_lists\": [],\n    \"title_timelines\": [],\n    \"screen_capture_restore_tokens\": [],\n    \"replay\": {{\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}}\n  }},\n  \"runtime\": {{"
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

fn title_entry(id: u128, value: fm_model::TitleFieldValue) -> fm_model::TitleFieldEntry {
    fm_model::TitleFieldEntry {
        id: fm_model::TitleFieldId::new(nz(id)),
        value,
        // Each field last changed at its own revision, below the title's 3.
        revision: u64::try_from(id).unwrap() - 1,
    }
}

fn title_project() -> Project {
    use fm_model::{TitleFieldValue, TitleInput};

    let mut project = Project::new(project_id(1), "Titles", rich_settings())
        .with_main_mix(MainMix::new(input_id(1), input_id(2)));
    project.add_input(Input {
        id: input_id(1),
        name: "Bars".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input_id(2),
        name: "Lower third".into(),
        kind: InputKind::Title(TitleInput {
            template_uri: "asset://titles/lower-third.fmtitle".into(),
            field_revision: 3,
            fields: vec![
                title_entry(1, TitleFieldValue::Text("Jane \"JD\" Doe".into())),
                title_entry(2, TitleFieldValue::Number(-4)),
                title_entry(
                    3,
                    TitleFieldValue::Color(Rgba8::new(0x11, 0x22, 0x33, 0xff)),
                ),
                title_entry(
                    4,
                    TitleFieldValue::Image {
                        asset_uri: "asset://images/logo.png".into(),
                    },
                ),
            ],
        }),
        required_capabilities: Vec::new(),
    });
    project
}

#[test]
fn title_inputs_round_trip_exactly_at_the_current_schema() {
    let temp = TestDirectory::new("title-inputs");
    let store = temp.store("show");
    let expected = StoredProject::from_project(
        title_project(),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.starts_with("{\n  \"schema_version\": 101,\n"));
    let title = "{\"type\": \"title\", \"template_uri\": \"asset://titles/lower-third.fmtitle\", \
         \"field_revision\": 3, \"fields\": [{\"id\": 1, \"type\": \"text\", \"value\": \
         \"Jane \\\"JD\\\" Doe\", \"revision\": 0}, {\"id\": 2, \"type\": \"number\", \"value\": -4, \
         \"revision\": 1}, {\"id\": 3, \"type\": \"color\", \"value\": \"#112233ff\", \"revision\": 2}, \
         {\"id\": 4, \"type\": \"image\", \"asset_uri\": \"asset://images/logo.png\", \
         \"revision\": 3}]}";
    assert!(
        encoded.contains(title),
        "unexpected title encoding in {encoded}"
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded, expected);
    // Encoding is canonical: a reload writes the same bytes.
    store.save(&loaded).unwrap();
    assert_eq!(fs::read_to_string(store.manifest_path()).unwrap(), encoded);

    // Upper-case hex is accepted and re-encoded lower-case.
    fs::write(
        store.manifest_path(),
        encoded.replacen("#112233ff", "#112233FF", 1),
    )
    .unwrap();
    assert_eq!(store.load().unwrap(), expected);

    // Field presence, types, and enum spellings are exact.
    for malformed in [
        encoded.replacen("\"type\": \"title\"", "\"type\": \"titles\"", 1),
        encoded.replacen("\"type\": \"number\"", "\"type\": \"integer\"", 1),
        encoded.replacen("\"type\": \"color\"", "\"type\": \"colour\"", 1),
        encoded.replacen("\"field_revision\": 3, ", "", 1),
        encoded.replacen("\"field_revision\": 3", "\"field_revision\": -3", 1),
        encoded.replacen(
            "\"field_revision\": 3",
            "\"field_revision\": 3, \"extra\": 1",
            1,
        ),
        encoded.replacen("{\"id\": 2, ", "{\"id\": 2, \"label\": \"x\", ", 1),
        encoded.replacen("{\"id\": 2, ", "{\"id\": 0, ", 1),
        encoded.replacen("\"value\": -4", "\"value\": \"-4\"", 1),
        encoded.replacen("\"value\": -4", "\"value\": 9223372036854775808", 1),
        encoded.replacen("\"value\": -4", "\"value\": -9223372036854775809", 1),
        encoded.replacen("#112233ff", "#112233", 1),
        encoded.replacen("#112233ff", "112233ff", 1),
        encoded.replacen("#112233ff", "#11223g00", 1),
        encoded.replacen(
            "\"asset_uri\": \"asset://images",
            "\"value\": \"asset://images",
            1,
        ),
        encoded.replacen(", \"revision\": 1}", "}", 1),
        encoded.replacen("\"revision\": 1}", "\"revision\": -1}", 1),
        encoded.replacen("\"revision\": 1}", "\"revision\": \"1\"}", 1),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "got {error:?} for {malformed}"
        );
    }

    // Number extremes round-trip.
    for extreme in ["-9223372036854775808", "9223372036854775807"] {
        fs::write(
            store.manifest_path(),
            encoded.replacen("\"value\": -4", &format!("\"value\": {extreme}"), 1),
        )
        .unwrap();
        store.load().unwrap();
    }

    // Well-formed but invalid title fields are rejected by project validation.
    for invalid in [
        encoded.replacen(
            "\"template_uri\": \"asset://titles/lower-third.fmtitle\"",
            "\"template_uri\": \" \"",
            1,
        ),
        encoded.replacen("{\"id\": 2, ", "{\"id\": 1, ", 1),
        encoded.replacen("{\"id\": 3, ", "{\"id\": 1, ", 1),
        encoded.replacen("\"asset://images/logo.png\"", "\"\"", 1),
        // A field may not have changed after its title's field revision.
        encoded.replacen("\"revision\": 3}", "\"revision\": 4}", 1),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), &invalid).unwrap();
        let error = store.load().unwrap_err();
        assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
    }

    // The previous schema is refused outright; no migration path exists.
    fs::write(
        store.manifest_path(),
        encoded.replacen("\"schema_version\": 101", "\"schema_version\": 26", 1),
    )
    .unwrap();
    let error = store.load().unwrap_err();
    assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
}

/// Schema 59: a cue's data update action (`title_number`) round-trips
/// exactly; its update kind and value are strict, and a step of nothing
/// or an out-of-range value is a validation error.
#[test]
fn title_number_cue_actions_round_trip_exactly_at_the_current_schema() {
    use fm_model::{Cue, CueAction, CueList, CueNumberUpdate, CueRunState, TitleFieldId};

    let temp = TestDirectory::new("title-number-cues");
    let store = temp.store("show");
    let number = |update| CueAction::TitleNumber {
        input: input_id(2),
        field: TitleFieldId::new(nz(2)),
        update,
    };
    let mut project = title_project();
    project.set_cue_lists(vec![CueList {
        id: 1,
        name: "Score".into(),
        cues: vec![Cue {
            id: 1,
            name: "Goal".into(),
            actions: vec![
                number(CueNumberUpdate::Add(1)).into(),
                number(CueNumberUpdate::Set(-999_999_999_999)).into(),
            ],
            auto_follow: false,
        }],
        state: CueRunState {
            standby: Some(1),
            ..CueRunState::default()
        },
    }]);
    let expected = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let add = "{\"type\": \"title_number\", \"input\": 2, \"field\": 2, \"update\": \"add\", \
               \"value\": 1}";
    let set = "{\"type\": \"title_number\", \"input\": 2, \"field\": 2, \"update\": \"set\", \
               \"value\": -999999999999}";
    assert!(encoded.contains(&format!("{add}, {set}")), "{encoded}");
    assert_eq!(store.load().unwrap(), expected);

    for malformed in [
        encoded.replacen("\"update\": \"add\"", "\"update\": \"inc\"", 1),
        encoded.replacen("\"update\": \"add\", ", "", 1),
        encoded.replacen("\"value\": 1}", "\"value\": \"1\"}", 1),
        encoded.replacen("\"field\": 2, \"update\"", "\"field\": 0, \"update\"", 1),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), &malformed).unwrap();
        let error = store.load().unwrap_err();
        assert!(
            matches!(error, StoreError::MalformedManifest { .. }),
            "got {error:?} for {malformed}"
        );
    }
    for invalid in [
        encoded.replacen("\"value\": 1}", "\"value\": 0}", 1),
        encoded.replacen("-999999999999", "-1000000000000", 1),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), &invalid).unwrap();
        let error = store.load().unwrap_err();
        assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
    }
}

#[test]
fn title_fields_change_through_the_model_and_persist() {
    use fm_model::TitleFieldValue;

    let temp = TestDirectory::new("title-mutation");
    let store = temp.store("show");
    let stored = StoredProject::from_project(
        title_project(),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut project = stored.project().clone();
    project
        .set_title_fields(
            input_id(2),
            4,
            vec![
                title_entry(1, TitleFieldValue::Text("Ana".into())),
                title_entry(2, TitleFieldValue::Number(7)),
                title_entry(3, TitleFieldValue::Color(Rgba8::new(0, 0, 0, 0))),
                title_entry(
                    4,
                    TitleFieldValue::Image {
                        asset_uri: "asset://images/other.png".into(),
                    },
                ),
            ],
        )
        .unwrap();
    let updated = stored.with_project(project).unwrap();
    store.save(&updated).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"field_revision\": 4"));
    assert!(encoded.contains("\"value\": \"#00000000\""));
    assert_eq!(store.load().unwrap(), updated);
}

fn playlist_project(state: fm_model::PlaylistRunState) -> StoredProject {
    use fm_model::{
        Playlist, PlaylistEntry, PlaylistLoop, PlaylistTransition, PlaylistTransitionKind,
    };

    let high = u128::from(u64::MAX) + 101;
    let mut project = rich_project();
    project.set_playlist(Playlist {
        loop_mode: PlaylistLoop::All,
        entries: vec![
            PlaylistEntry {
                id: 7,
                input: input_id(high),
                duration_frames: 90,
                follow_media: false,
                transition: PlaylistTransition::CUT,
                enabled: true,
            },
            PlaylistEntry {
                id: u64::MAX,
                input: input_id(high + 3),
                duration_frames: 45,
                follow_media: false,
                transition: PlaylistTransition {
                    kind: PlaylistTransitionKind::AlphaFade,
                    duration_frames: 12,
                    stinger_slot: None,
                },
                enabled: false,
            },
        ],
        state,
        next_take: 4,
        observed_frame: 321,
    });
    StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn playlist_round_trips_every_state_exactly_and_is_strict_at_the_current_schema() {
    use fm_model::PlaylistRunState;

    let temp = TestDirectory::new("playlist");
    let store = temp.store("show");
    for state in [
        PlaylistRunState::Stopped,
        PlaylistRunState::Armed {
            start_frame: 400,
            from: Some(u64::MAX),
        },
        PlaylistRunState::Armed {
            start_frame: 400,
            from: None,
        },
        PlaylistRunState::Playing {
            entry: 7,
            take: 3,
            cycle: 1,
            started_frame: 300,
        },
        PlaylistRunState::Paused {
            entry: 7,
            take: 3,
            cycle: 1,
            elapsed_frames: 21,
        },
        PlaylistRunState::Finished { last: Some(7) },
        PlaylistRunState::Finished { last: None },
    ] {
        let expected = playlist_project(state);
        store.save(&expected).unwrap();
        assert_eq!(store.load().unwrap(), expected, "state {state:?}");
    }

    let expected = playlist_project(PlaylistRunState::Playing {
        entry: 7,
        take: 3,
        cycle: 1,
        started_frame: 300,
    });
    store.save(&expected).unwrap();
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 101"));
    let playlist = "\"playlist\": {\"loop\": \"all\", \"entries\": [{\"id\": 7, \"input\": 18446744073709551716, \"duration_frames\": 90, \"follow_media\": false, \"transition\": {\"kind\": \"cut\", \"duration_frames\": 0}, \"enabled\": true}, {\"id\": 18446744073709551615, \"input\": 18446744073709551719, \"duration_frames\": 45, \"follow_media\": false, \"transition\": {\"kind\": \"alpha_fade\", \"duration_frames\": 12}, \"enabled\": false}], \"state\": {\"type\": \"playing\", \"entry\": 7, \"take\": 3, \"cycle\": 1, \"started_frame\": 300}, \"next_take\": 4, \"observed_frame\": 321}";
    assert!(
        encoded.contains(&format!(
            "    {playlist},\n    \"cue_lists\": [],\n    \"luts\": [],"
        )),
        "unexpected playlist encoding in {encoded}"
    );

    // The field is required, typed, and closed.
    for malformed in [
        encoded.replacen(&format!(",\n    {playlist}"), "", 1),
        encoded.replacen(playlist, "\"playlist\": null", 1),
        encoded.replacen("\"loop\": \"all\"", "\"loop\": \"forever\"", 1),
        encoded.replacen("\"kind\": \"cut\"", "\"kind\": \"stinger\"", 1),
        encoded.replacen("\"type\": \"playing\"", "\"type\": \"running\"", 1),
        encoded.replacen("\"enabled\": true", "\"enabled\": 1", 1),
        encoded.replacen("\"started_frame\": 300", "\"started_frame\": -1", 1),
        encoded.replacen(", \"observed_frame\": 321", "", 1),
        encoded.replacen(
            "\"observed_frame\": 321}",
            "\"observed_frame\": 321, \"extra\": 1}",
            1,
        ),
    ] {
        assert_ne!(malformed, encoded);
        fs::write(store.manifest_path(), malformed).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "accepted a malformed playlist"
        );
    }

    // Structural bounds and input references are domain errors.
    for invalid in [
        encoded.replacen("\"duration_frames\": 90", "\"duration_frames\": 0", 1),
        encoded.replacen(
            "\"kind\": \"cut\", \"duration_frames\": 0",
            "\"kind\": \"cut\", \"duration_frames\": 3",
            1,
        ),
        encoded.replacen(
            "\"kind\": \"alpha_fade\", \"duration_frames\": 12",
            "\"kind\": \"alpha_fade\", \"duration_frames\": 46",
            1,
        ),
        encoded.replacen("\"id\": 18446744073709551615", "\"id\": 7", 1),
        encoded.replacen(
            "{\"id\": 7, \"input\": 18446744073709551716",
            "{\"id\": 7, \"input\": 99",
            1,
        ),
        encoded.replacen("\"next_take\": 4", "\"next_take\": 0", 1),
    ] {
        assert_ne!(invalid, encoded);
        fs::write(store.manifest_path(), invalid).unwrap();
        let error = store.load().unwrap_err();
        assert!(matches!(error, StoreError::Validation(_)), "got {error:?}");
    }
}

#[test]
fn playlist_entries_keep_their_inputs_from_being_removed() {
    let high = u128::from(u64::MAX) + 101;
    let stored = playlist_project(fm_model::PlaylistRunState::Stopped);
    let mut project = stored.project().clone();
    assert_eq!(
        project.input_reference(input_id(high + 3)),
        Some(fm_model::InputReference::PlaylistEntry(u64::MAX))
    );
    assert_eq!(
        project.remove_input(input_id(high + 3)),
        Err(fm_model::RemoveInputError::DomainReference(input_id(
            high + 3
        )))
    );
}

/// IN-015: a network stream input round-trips every setting, keeps its
/// credentials in `project.json` (the receiver needs them), and refuses a
/// malformed configuration.
#[test]
fn network_stream_inputs_round_trip_exactly_and_reject_malformed_values() {
    use fm_types::network_stream::{
        GiveUp, NetworkStreamConfig, ReconnectPolicy, RtspTransport, StallSlate,
    };

    let temp = TestDirectory::new("network-streams");
    let store = temp.store("show");
    let mut project = rich_project();
    let network = project
        .inputs()
        .iter()
        .find(|input| matches!(input.kind, InputKind::NetworkStream(_)))
        .unwrap()
        .id;
    let mut config = NetworkStreamConfig::new("rtsp://user:pass@example.test:554/stream?token=t0k");
    config.settings.transport = RtspTransport::Tcp;
    config.settings.jitter_buffer_ms = 750;
    config.settings.reconnect = ReconnectPolicy {
        backoff_min_ms: 250,
        backoff_max_ms: 8_000,
        give_up: GiveUp::After(std::num::NonZeroU32::new(12).unwrap()),
    };
    config.settings.timeout_ms = 3_000;
    config.settings.stall = StallSlate::Black;
    project
        .replace_input_source(
            network,
            InputKind::NetworkStream(config.clone()),
            Vec::new(),
        )
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    let kind = &loaded
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == network)
        .unwrap()
        .kind;
    assert_eq!(*kind, InputKind::NetworkStream(config));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let object = "{\"type\": \"network_stream\", \"url\": \"rtsp://user:pass@example.test:554/stream?token=t0k\", \"transport\": \"tcp\", \"jitter_ms\": 750, \"reconnect\": {\"backoff_min_ms\": 250, \"backoff_max_ms\": 8000, \"give_up\": 12}, \"timeout_ms\": 3000, \"stall\": \"black\", \"mode\": \"caller\", \"latency_ms\": 120, \"key_length\": 16, \"audio\": \"mix\", \"stream_id\": null, \"stream_key\": null, \"passphrase\": null}";
    assert!(encoded.contains(object), "{encoded}");

    for (from, to) in [
        ("\"transport\": \"tcp\"", "\"transport\": \"quic\""),
        ("\"jitter_ms\": 750", "\"jitter_ms\": 5001"),
        ("\"jitter_ms\": 750", "\"jitter_ms\": \"750\""),
        ("\"backoff_min_ms\": 250", "\"backoff_min_ms\": 9000"),
        ("\"give_up\": 12", "\"give_up\": 0"),
        ("\"give_up\": 12", "\"give_up\": \"sometimes\""),
        ("\"timeout_ms\": 3000", "\"timeout_ms\": 100"),
        ("\"stall\": \"black\"", "\"stall\": \"blue\""),
        ("rtsp://user:pass@example.test:554", "ftp://example.test"),
        (
            "rtsp://user:pass@example.test:554",
            "udp://example.test:9000",
        ),
        (", \"timeout_ms\": 3000", ""),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// IN-016/IN-017 (schema 46): RTMP and SRT inputs round-trip their mode,
/// latency, key length, audio pair, stream ID, and secrets (kept in
/// `project.json`, which the receiver needs), and malformed or missing
/// values are refused.
#[test]
fn rtmp_and_srt_inputs_round_trip_their_secrets_and_reject_malformed_values() {
    use fm_types::network_stream::{AudioChannels, NetworkStreamConfig, SrtKeyLength, StreamMode};

    let temp = TestDirectory::new("rtmp-srt-streams");
    let store = temp.store("show");
    let mut project = rich_project();
    let network = project
        .inputs()
        .iter()
        .find(|input| matches!(input.kind, InputKind::NetworkStream(_)))
        .unwrap()
        .id;
    let mut config = NetworkStreamConfig::new("srt://0.0.0.0:9000");
    config.settings.mode = StreamMode::Listener;
    config.settings.latency_ms = 400;
    config.settings.key_length = SrtKeyLength::Aes256;
    config.settings.audio = AudioChannels::Pair56;
    config.stream_id = Some("cam1".into());
    config.passphrase = Some("long-enough-secret".into());
    project
        .replace_input_source(
            network,
            InputKind::NetworkStream(config.clone()),
            Vec::new(),
        )
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    let kind = |loaded: &StoredProject| {
        loaded
            .project()
            .inputs()
            .iter()
            .find(|input| input.id == network)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(kind(&loaded), InputKind::NetworkStream(config));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let fields = "\"mode\": \"listener\", \"latency_ms\": 400, \"key_length\": 32, \"audio\": \"5-6\", \"stream_id\": \"cam1\", \"stream_key\": null, \"passphrase\": \"long-enough-secret\"}";
    assert!(encoded.contains(fields), "{encoded}");

    // An RTMP listener keeps its stream key.
    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("obs-key".into());
    let rtmp_encoded = encoded
        .replace("srt://0.0.0.0:9000", "rtmp://0.0.0.0:1935/live")
        .replace(
            "\"latency_ms\": 400, \"key_length\": 32, \"audio\": \"5-6\", \"stream_id\": \"cam1\", \"stream_key\": null, \"passphrase\": \"long-enough-secret\"",
            "\"latency_ms\": 120, \"key_length\": 16, \"audio\": \"mix\", \"stream_id\": null, \"stream_key\": \"obs-key\", \"passphrase\": null",
        );
    fs::write(store.manifest_path(), &rtmp_encoded).unwrap();
    assert_eq!(kind(&store.load().unwrap()), InputKind::NetworkStream(rtmp));

    for (from, to) in [
        ("\"mode\": \"listener\"", "\"mode\": \"server\""),
        ("\"latency_ms\": 400", "\"latency_ms\": 10"),
        ("\"key_length\": 32", "\"key_length\": 8"),
        ("\"key_length\": 32", "\"key_length\": \"32\""),
        ("\"audio\": \"5-6\"", "\"audio\": \"9-10\""),
        (
            "\"passphrase\": \"long-enough-secret\"",
            "\"passphrase\": \"short\"",
        ),
        ("\"stream_key\": null", "\"stream_key\": \"rtmp-only\""),
        ("srt://0.0.0.0:9000", "srt://0.0.0.0:80"),
        (", \"stream_id\": \"cam1\"", ""),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// IN-001: a capture device input round-trips its stable key and every
/// capture setting, and a malformed or missing setting is refused.
#[test]
#[allow(clippy::too_many_lines)]
fn capture_inputs_round_trip_exactly_and_reject_malformed_values() {
    use fm_types::capture::{
        CaptureAudio, CaptureFormat, CaptureLossSlate, CapturePixelFormat, CaptureScan,
        CaptureSettings,
    };

    let temp = TestDirectory::new("capture-inputs");
    let store = temp.store("show");
    let mut project = rich_project();
    let device = project
        .inputs()
        .iter()
        .find(|input| matches!(input.kind, InputKind::Device { .. }))
        .unwrap()
        .id;
    let settings = CaptureSettings {
        format: Some(CaptureFormat {
            width: 1920,
            height: 1080,
            rate: fm_types::FrameRate::new(30_000, 1_001).unwrap(),
            pixel_format: CapturePixelFormat::Yuyv,
            scan: CaptureScan::InterlacedTff,
        }),
        audio: CaptureAudio::Alsa("hw:CARD=C920,DEV=0".into()),
        loss: CaptureLossSlate::Black,
    };
    project
        .set_capture_settings(device, settings.clone())
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();

    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.project().capture_input(device),
        Some(("decklink:1", &settings))
    );
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 101"));
    let object = "{\"type\": \"device\", \"stable_key\": \"decklink:1\", \"capture\": {\"format\": \"1920x1080@30000/1001:yuyv:tff\", \"audio\": \"alsa:hw:CARD=C920,DEV=0\", \"loss\": \"black\"}}";
    assert!(encoded.contains(object), "{encoded}");

    // The automatic format is `null`.
    let mut automatic = loaded.project().clone();
    automatic
        .set_capture_settings(device, CaptureSettings::default())
        .unwrap();
    let stored = StoredProject::from_project(
        automatic,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let automatic = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(
        automatic
            .contains("\"capture\": {\"format\": null, \"audio\": \"paired\", \"loss\": \"hold\"}"),
        "{automatic}"
    );
    assert_eq!(
        store.load().unwrap().project().capture_input(device),
        Some(("decklink:1", &CaptureSettings::default()))
    );

    for (from, to) in [
        (
            "\"1920x1080@30000/1001:yuyv:tff\"",
            "\"1920x1080@60000/2002:yuyv:tff\"",
        ),
        (
            "\"1920x1080@30000/1001:yuyv:tff\"",
            "\"1920x1080@30/1:rgb:p\"",
        ),
        ("\"1920x1080@30000/1001:yuyv:tff\"", "1080"),
        (
            "\"audio\": \"alsa:hw:CARD=C920,DEV=0\"",
            "\"audio\": \"pulse\"",
        ),
        (
            "\"audio\": \"alsa:hw:CARD=C920,DEV=0\"",
            "\"audio\": \"alsa:\"",
        ),
        ("\"loss\": \"black\"", "\"loss\": \"blue\""),
        (", \"loss\": \"black\"", ""),
        ("\"stable_key\": \"decklink:1\"", "\"stable_key\": \" \""),
        (
            ", \"capture\": {\"format\": \"1920x1080@30000/1001:yuyv:tff\", \"audio\": \"alsa:hw:CARD=C920,DEV=0\", \"loss\": \"black\"}",
            "",
        ),
        ("\"loss\": \"black\"}", "\"loss\": \"black\", \"extra\": 1}"),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// AU-011 (schema 52): a system audio input round-trips its audio system
/// and device exactly, and a missing, unknown, or unusable value is
/// refused.
#[test]
fn system_audio_inputs_round_trip_exactly_and_reject_malformed_values() {
    use fm_types::system_audio::{SystemAudioBackend, SystemAudioConfig};

    let temp = TestDirectory::new("system-audio");
    let store = temp.store("show");
    let mut project = rich_project();
    let target = project.inputs()[0].id;
    let config = SystemAudioConfig::new(
        SystemAudioBackend::Pulse,
        "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
    );
    project
        .replace_input_source(target, InputKind::SystemAudio(config.clone()), Vec::new())
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    let kind = &loaded
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == target)
        .unwrap()
        .kind;
    assert_eq!(*kind, InputKind::SystemAudio(config));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"schema_version\": 101"));
    let object = "{\"type\": \"system_audio\", \"backend\": \"pulse\", \"device\": \"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor\"}";
    assert!(encoded.contains(object), "{encoded}");

    for (from, to) in [
        ("\"backend\": \"pulse\"", "\"backend\": \"jack\""),
        ("\"backend\": \"pulse\", ", ""),
        (", \"device\": \"alsa_output", ", \"name\": \"alsa_output"),
        ("\"device\": \"alsa_output", "\"device\": \"-alsa_output"),
        (
            "\"device\": \"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor\"",
            "\"device\": \"\"",
        ),
        (
            "\"device\": \"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor\"",
            "\"device\": 7",
        ),
        (".monitor\"}", ".monitor\", \"extra\": true}"),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// IN-008/IN-009 (schema 63): image and slideshow inputs round-trip every
/// setting exactly, and a missing, unknown, or out-of-bounds value is
/// refused.
#[test]
#[allow(clippy::too_many_lines)]
fn image_and_slideshow_inputs_round_trip_exactly_and_reject_malformed_values() {
    use fm_types::OutputScaling;
    use fm_types::still::{
        ImageInputConfig, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem,
        SlideshowSource,
    };

    let temp = TestDirectory::new("still-inputs");
    let store = temp.store("show");
    let mut project = rich_project();
    let image_input = project.inputs()[0].id;
    let list_input = project.inputs()[1].id;
    let image = ImageInputConfig::new("asset://stills/logo.png", OutputScaling::OneToOne);
    let list = SlideshowConfig {
        slide_frames: 90,
        transition: SlideTransitionStyle::Cut,
        transition_frames: 12,
        ken_burns: true,
        order: SlideOrder::Random { seed: u64::MAX },
        loop_show: false,
        auto_advance: false,
        fit: OutputScaling::Fill,
        ..SlideshowConfig::new(SlideshowSource::List(vec![
            SlideshowItem::new("asset://a.jpg").with_caption("Día \"uno\""),
            SlideshowItem::new("asset://b.webp"),
        ]))
    };
    project
        .replace_input_source(image_input, InputKind::Image(image.clone()), Vec::new())
        .unwrap();
    project
        .replace_input_source(list_input, InputKind::Slideshow(list.clone()), Vec::new())
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    let kind_of = |input| {
        loaded
            .project()
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(kind_of(image_input), InputKind::Image(image));
    assert_eq!(kind_of(list_input), InputKind::Slideshow(list));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let image_object =
        "{\"type\": \"image\", \"asset_uri\": \"asset://stills/logo.png\", \"fit\": \"1:1\"}";
    assert!(encoded.contains(image_object), "{encoded}");
    assert!(encoded.contains("\"order\": {\"random\": 18446744073709551615}"));

    // A folder source round-trips too.
    let mut folder_project = loaded.project().clone();
    let folder = SlideshowConfig::new(SlideshowSource::Folder {
        asset_uri: "asset://photos".into(),
    });
    folder_project
        .replace_input_source(list_input, InputKind::Slideshow(folder.clone()), Vec::new())
        .unwrap();
    let stored = StoredProject::from_project(
        folder_project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let reloaded = store.load().unwrap();
    assert!(
        reloaded
            .project()
            .inputs()
            .iter()
            .any(|input| input.kind == InputKind::Slideshow(folder.clone()))
    );
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    assert!(encoded.contains("\"source\": {\"folder\": \"asset://photos\"}"));

    for (from, to) in [
        ("\"fit\": \"1:1\"", "\"fit\": \"zoom\""),
        (
            "\"asset_uri\": \"asset://stills/logo.png\"",
            "\"asset_uri\": \"asset://stills/logo.mp4\"",
        ),
        (
            "\"asset_uri\": \"asset://stills/logo.png\"",
            "\"asset_uri\": \"/tmp/logo.png\"",
        ),
        (
            "\"source\": {\"folder\": \"asset://photos\"}",
            "\"source\": {\"folder\": \"asset://../photos\"}",
        ),
        (
            "\"source\": {\"folder\": \"asset://photos\"}",
            "\"source\": {\"items\": []}",
        ),
        ("\"slide_frames\": 150", "\"slide_frames\": 0"),
        ("\"transition\": \"fade\"", "\"transition\": \"wipe\""),
        ("\"transition_frames\": 30", "\"transition_frames\": 3601"),
        ("\"order\": \"ordered\"", "\"order\": \"shuffled\""),
        ("\"auto_advance\": true, ", ""),
        ("\"loop\": true", "\"loop\": 1"),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// IN-011: a screen or window capture input round-trips its target, crop,
/// cursor, rate, stall slate, and audio exactly, and a missing, unknown, or
/// unusable value (such as a screen taking an application's audio) is
/// refused.
#[test]
fn screen_capture_inputs_round_trip_exactly_and_reject_malformed_values() {
    use fm_types::network_stream::StallSlate;
    use fm_types::screen_capture::{
        ScreenCaptureAudio, ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureTarget,
        TitleMatch, WindowRule,
    };

    let temp = TestDirectory::new("screen-capture");
    let store = temp.store("show");
    let mut project = rich_project();
    let window = project.inputs()[0].id;
    let screen = project.inputs()[1].id;
    let slides = ScreenCaptureConfig {
        target: ScreenCaptureTarget::Window(WindowRule {
            app: "Keynote".into(),
            title: "Deck \"final\"".into(),
            title_match: TitleMatch::Contains,
            window_id: Some(4_242),
        }),
        crop: ScreenCaptureCrop {
            left: 1,
            top: 28,
            right: 3,
            bottom: 2,
        },
        cursor: false,
        fps: 25,
        slate: StallSlate::Black,
        audio: ScreenCaptureAudio::Application,
    };
    let desktop = ScreenCaptureConfig {
        audio: ScreenCaptureAudio::System,
        ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 1 })
    };
    project
        .replace_input_source(window, InputKind::ScreenCapture(slides.clone()), Vec::new())
        .unwrap();
    project
        .replace_input_source(
            screen,
            InputKind::ScreenCapture(desktop.clone()),
            Vec::new(),
        )
        .unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    let kind = |id| {
        loaded
            .project()
            .inputs()
            .iter()
            .find(|input| input.id == id)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(kind(window), InputKind::ScreenCapture(slides));
    assert_eq!(kind(screen), InputKind::ScreenCapture(desktop));
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let window_object = "{\"type\": \"screen_capture\", \"target\": {\"kind\": \"window\", \"app\": \"Keynote\", \"title\": \"Deck \\\"final\\\"\", \"title_match\": \"contains\", \"window_id\": 4242}, \"crop\": {\"left\": 1, \"top\": 28, \"right\": 3, \"bottom\": 2}, \"cursor\": false, \"fps\": 25, \"stall\": \"black\", \"audio\": \"app\"}";
    assert!(encoded.contains(window_object), "{encoded}");
    let screen_object = "{\"type\": \"screen_capture\", \"target\": {\"kind\": \"screen\", \"index\": 1}, \"crop\": {\"left\": 0, \"top\": 0, \"right\": 0, \"bottom\": 0}, \"cursor\": true, \"fps\": 30, \"stall\": \"freeze\", \"audio\": \"system\"}";
    assert!(encoded.contains(screen_object), "{encoded}");

    for (from, to) in [
        ("\"kind\": \"window\"", "\"kind\": \"region\""),
        (
            "\"title_match\": \"contains\"",
            "\"title_match\": \"regex\"",
        ),
        ("\"window_id\": 4242", "\"window_id\": 0"),
        ("\"app\": \"Keynote\", ", ""),
        ("\"index\": 1", "\"index\": 64"),
        ("\"left\": 1", "\"left\": 16385"),
        ("\"cursor\": false", "\"cursor\": 0"),
        ("\"fps\": 25", "\"fps\": 0"),
        ("\"fps\": 25", "\"fps\": 61"),
        ("\"stall\": \"black\"", "\"stall\": \"slate\""),
        (
            "\"audio\": \"app\"}",
            "\"audio\": \"app\", \"extra\": true}",
        ),
        ("\"audio\": \"app\"", "\"audio\": \"application\""),
        (", \"audio\": \"app\"", ""),
        ("\"audio\": \"system\"", "\"audio\": \"app\""),
        ("\"audio\": \"system\"", "\"audio\": null"),
    ] {
        let invalid = encoded.replacen(from, to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::MalformedManifest { .. })),
            "{from} -> {to}"
        );
    }
}

/// IN-011 (schema 72): the desktop restore token a Wayland desktop granted
/// a screen capture input round-trips beside it, is dropped with the input
/// or when the input stops capturing that kind of target, and a token of
/// another input or an unusable token is refused.
#[test]
#[allow(clippy::too_many_lines)] // one section's exact form, then each strictness rule
fn screen_capture_restore_tokens_round_trip_exactly_and_strictly() {
    use std::collections::BTreeMap;

    use fm_types::screen_capture::{ScreenCaptureConfig, ScreenCaptureTarget, WindowRule};

    let temp = TestDirectory::new("screen-capture-tokens");
    let store = temp.store("show");
    let mut project = rich_project();
    let window = project.inputs()[0].id;
    let screen = project.inputs()[1].id;
    let other = project.inputs()[2].id;
    project
        .replace_input_source(
            window,
            InputKind::ScreenCapture(ScreenCaptureConfig::new(ScreenCaptureTarget::Window(
                WindowRule::picked(),
            ))),
            Vec::new(),
        )
        .unwrap();
    project
        .replace_input_source(
            screen,
            InputKind::ScreenCapture(ScreenCaptureConfig::new(ScreenCaptureTarget::Screen {
                index: 0,
            })),
            Vec::new(),
        )
        .unwrap();
    // Only valid tokens of screen capture inputs are kept.
    assert!(project.set_screen_capture_restore_tokens(BTreeMap::from([
        (window, "6e1f-window".to_owned()),
        (screen, "9a2b-screen".to_owned()),
        (other, "not-a-capture".to_owned()),
    ])));
    assert_eq!(project.screen_capture_restore_tokens().len(), 2);
    let same = project.screen_capture_restore_tokens().clone();
    assert!(!project.set_screen_capture_restore_tokens(same));
    let stored = StoredProject::from_project(
        project.clone(),
        RuntimeRouting::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.project(), &project);
    let encoded = fs::read_to_string(store.manifest_path()).unwrap();
    let (low, high) = if window < screen {
        ((window, "6e1f-window"), (screen, "9a2b-screen"))
    } else {
        ((screen, "9a2b-screen"), (window, "6e1f-window"))
    };
    let field = format!(
        ",\n    \"screen_capture_restore_tokens\": [\n      {{\"input\": {}, \"token\": \"{}\"}},\n      {{\"input\": {}, \"token\": \"{}\"}}\n    ],\n    \"replay\": {{\"sources\": [], \"retention_seconds\": 600, \"storage_root\": null, \"auto_return\": null, \"transition\": \"cut\"}}\n  }},",
        low.0, low.1, high.0, high.1
    );
    assert!(encoded.contains(&field), "{encoded}");

    for (from, to) in [
        (
            format!("{{\"input\": {}, \"token\"", low.0),
            format!("{{\"input\": {other}, \"token\""),
        ),
        (
            format!("\"token\": \"{}\"", low.1),
            "\"token\": \"\"".to_owned(),
        ),
        (
            format!("\"token\": \"{}\"", low.1),
            "\"token\": \"with space\"".to_owned(),
        ),
        (
            format!("\"token\": \"{}\"", low.1),
            "\"token\": 7".to_owned(),
        ),
        (
            format!("\"token\": \"{}\"}}", low.1),
            format!("\"token\": \"{}\", \"extra\": true}}", low.1),
        ),
        (
            format!("{{\"input\": {}, ", high.0),
            format!("{{\"input\": {}, ", low.0),
        ),
        (
            "\"screen_capture_restore_tokens\"".to_owned(),
            "\"restore_tokens\"".to_owned(),
        ),
    ] {
        let invalid = encoded.replacen(&from, &to, 1);
        assert_ne!(invalid, encoded, "{from}");
        fs::write(store.manifest_path(), invalid).unwrap();
        assert!(
            matches!(
                store.load(),
                Err(StoreError::MalformedManifest { .. } | StoreError::Validation(_))
            ),
            "{from} -> {to}"
        );
    }

    // A screen capture input that now captures a window drops the screen's
    // token; one still capturing a window keeps its token through a crop
    // edit; another kind of source drops it.
    let mut edited = project;
    edited
        .replace_input_source(
            screen,
            InputKind::ScreenCapture(ScreenCaptureConfig::new(ScreenCaptureTarget::Window(
                WindowRule::picked(),
            ))),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(
        edited
            .screen_capture_restore_tokens()
            .keys()
            .collect::<Vec<_>>(),
        [&window]
    );
    let mut cropped = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::picked()));
    cropped.crop.top = 30;
    edited
        .replace_input_source(window, InputKind::ScreenCapture(cropped), Vec::new())
        .unwrap();
    assert_eq!(edited.screen_capture_restore_tokens().len(), 1, "same kind");
    let media = edited.inputs()[2].kind.clone();
    edited
        .replace_input_source(window, media, Vec::new())
        .unwrap();
    assert!(edited.screen_capture_restore_tokens().is_empty());
}
