//! IN-015 network stream inputs (schema 43): the configuration's bounds,
//! URL validation and redaction, the per-project input limit, and the
//! replaceable network source. IN-016/IN-017 (schema 46): RTMP and SRT
//! modes, stream keys, passphrases, and stream IDs, their redaction, and
//! listen ports no two inputs share.

use std::num::{NonZeroU32, NonZeroU128};

use fm_model::{
    EntityRef, Input, InputKind, InputSourceError, InputSourceSpec, Project, ProjectSettings,
    ValidationErrorKind,
};
use fm_types::network_stream::{
    GiveUp, MAX_JITTER_BUFFER_MS, MAX_NETWORK_STREAM_INPUTS, MIN_JITTER_BUFFER_MS,
    NetworkStreamConfig, NetworkStreamError, ReconnectPolicy, RtspTransport, StreamMode,
    StreamProtocol, redact_stream_url, scrub_stream_text, validate_stream_url,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Streams",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 36).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    )
}

fn network(id: u128, config: NetworkStreamConfig) -> Input {
    Input {
        id: input(id),
        name: format!("Stream {id}"),
        kind: InputKind::NetworkStream(config),
        required_capabilities: Vec::new(),
    }
}

#[test]
fn every_supported_protocol_validates_and_others_are_refused() {
    for (url, protocol) in [
        ("rtsp://cam.local:554/stream1", StreamProtocol::Rtsp),
        ("rtsps://cam.local/stream1", StreamProtocol::Rtsp),
        ("rtp://239.0.0.1:5004", StreamProtocol::Rtp),
        ("udp://@:9000", StreamProtocol::Udp),
        (
            "udp://239.1.1.1:1234?localaddr=10.0.0.2",
            StreamProtocol::Udp,
        ),
        ("tcp://encoder.local:9000", StreamProtocol::Tcp),
        ("http://encoder.local/live.ts", StreamProtocol::Http),
        ("https://cdn.example/live/index.m3u8", StreamProtocol::Hls),
    ] {
        assert_eq!(validate_stream_url(url), Ok(protocol), "{url}");
    }
    for (url, error) in [
        ("", NetworkStreamError::InvalidUrl),
        ("ftp://host/file.ts", NetworkStreamError::UnsupportedScheme),
        (
            "file://host/etc/passwd",
            NetworkStreamError::UnsupportedScheme,
        ),
        // No host either, but the scheme is what is wrong.
        ("file:///etc/passwd", NetworkStreamError::UnsupportedScheme),
        ("udp://host", NetworkStreamError::InvalidUrl),
        ("tcp://:9000", NetworkStreamError::InvalidUrl),
        ("http://host/with space", NetworkStreamError::InvalidUrl),
        ("rtsp://", NetworkStreamError::InvalidUrl),
    ] {
        assert_eq!(validate_stream_url(url), Err(error), "{url:?}");
    }
    let long = format!("http://host/{}", "a".repeat(600));
    assert_eq!(
        validate_stream_url(&long),
        Err(NetworkStreamError::InvalidUrl)
    );
}

#[test]
fn credentials_and_secret_query_values_are_redacted_and_scrubbed() {
    let url = "rtsp://admin:hunter2@cam.local:554/live?token=abc123&channel=1";
    let redacted = redact_stream_url(url);
    assert_eq!(
        redacted,
        "rtsp://***@cam.local:554/live?token=***&channel=1"
    );
    let config = NetworkStreamConfig::new(url);
    assert_eq!(config.redacted().url, redacted);
    assert!(!format!("{config:?}").contains("hunter2"));
    assert!(!format!("{config:?}").contains("abc123"));
    let scrubbed = scrub_stream_text(
        &format!("{url}: 401 Unauthorized for admin:hunter2\nnext line"),
        url,
    );
    for secret in ["hunter2", "abc123", "\n"] {
        assert!(!scrubbed.contains(secret), "{scrubbed}");
    }
    assert!(scrubbed.len() <= fm_types::network_stream::MAX_STREAM_ERROR_BYTES);
    // Nothing to redact: unchanged.
    assert_eq!(redact_stream_url("udp://@:9000"), "udp://@:9000");
}

#[test]
fn settings_are_bounded_and_the_rtsp_transport_hint_is_rtsp_only() {
    let mut config = NetworkStreamConfig::new("rtsp://cam.local/live");
    config.settings.transport = RtspTransport::Udp;
    assert_eq!(config.validate(), Ok(StreamProtocol::Rtsp));
    config.url = "udp://@:9000".into();
    assert_eq!(config.validate(), Err(NetworkStreamError::TransportNotRtsp));
    config.settings.transport = RtspTransport::Auto;

    for jitter in [MIN_JITTER_BUFFER_MS - 1, MAX_JITTER_BUFFER_MS + 1] {
        let mut bad = config.clone();
        bad.settings.jitter_buffer_ms = jitter;
        assert_eq!(bad.validate(), Err(NetworkStreamError::JitterBuffer));
    }
    let mut bad = config.clone();
    bad.settings.reconnect = ReconnectPolicy {
        backoff_min_ms: 2_000,
        backoff_max_ms: 1_000,
        give_up: GiveUp::Never,
    };
    assert_eq!(bad.validate(), Err(NetworkStreamError::Backoff));
    let mut bad = config.clone();
    bad.settings.reconnect.give_up = GiveUp::After(NonZeroU32::new(10_001).unwrap());
    assert_eq!(bad.validate(), Err(NetworkStreamError::GiveUp));
    let mut bad = config.clone();
    bad.settings.timeout_ms = 100;
    assert_eq!(bad.validate(), Err(NetworkStreamError::Timeout));
}

#[test]
fn reconnect_backoff_doubles_to_its_ceiling_and_gives_up_on_policy() {
    let policy = ReconnectPolicy {
        backoff_min_ms: 500,
        backoff_max_ms: 3_000,
        give_up: GiveUp::After(NonZeroU32::new(3).unwrap()),
    };
    let delays = (1..=5)
        .map(|attempt| policy.delay(attempt).as_millis())
        .collect::<Vec<_>>();
    assert_eq!(delays, [500, 1_000, 2_000, 3_000, 3_000]);
    assert!(policy.allows(3));
    assert!(!policy.allows(4));
    assert!(ReconnectPolicy::default().allows(u32::MAX));
}

#[test]
fn project_validation_names_the_invalid_field_and_bounds_the_input_count() {
    let mut project = project();
    let mut bad = NetworkStreamConfig::new("udp://@:9000");
    bad.settings.jitter_buffer_ms = 1;
    project.add_input(network(1, bad));
    let errors = project.validate().unwrap_err();
    assert!(errors.iter().any(|error| {
        error.entity == Some(EntityRef::Input(input(1)))
            && error.field == "kind.network_stream.jitter_ms"
            && error.kind == ValidationErrorKind::OutOfRange
    }));

    let mut project = self::project();
    for id in 1..=u128::try_from(MAX_NETWORK_STREAM_INPUTS).unwrap() {
        project.add_input(network(id, NetworkStreamConfig::new("udp://@:9000")));
    }
    assert_eq!(project.validate(), Ok(()));
    let extra = u128::try_from(MAX_NETWORK_STREAM_INPUTS).unwrap() + 1;
    project.add_input(network(extra, NetworkStreamConfig::new("udp://@:9001")));
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "kind.network_stream"
                && error.kind == ValidationErrorKind::OutOfRange),
        "{errors:?}"
    );
}

#[test]
fn a_network_source_is_replaceable_and_validated() {
    let config = NetworkStreamConfig::new("ftp://not-supported/live.ts");
    assert_eq!(
        InputSourceSpec::Network(config).validate(),
        Err(InputSourceError::InvalidNetworkStream)
    );
    let config = NetworkStreamConfig::new("tcp://encoder.local:9000");
    let spec = InputSourceSpec::Network(config.clone());
    assert_eq!(spec.validate(), Ok(()));
    let kind = InputKind::NetworkStream(config);
    assert_eq!(InputSourceSpec::from_kind(&kind), Some(spec));

    let mut project = project();
    project.add_input(network(1, NetworkStreamConfig::new("udp://@:9000")));
    let replaced = NetworkStreamConfig::new("https://cdn.example/live.m3u8");
    project
        .replace_input_source(
            input(1),
            InputKind::NetworkStream(replaced.clone()),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(project.inputs()[0].kind, InputKind::NetworkStream(replaced));
}

#[test]
fn rtmp_and_srt_fields_validate_per_protocol_and_redact_their_secrets() {
    for (url, protocol) in [
        ("rtmp://ingest.example/live/key123", StreamProtocol::Rtmp),
        ("rtmps://ingest.example:443/app", StreamProtocol::Rtmp),
        ("srt://encoder.local:9000", StreamProtocol::Srt),
        ("srt://0.0.0.0:9000/", StreamProtocol::Srt),
    ] {
        assert_eq!(validate_stream_url(url), Ok(protocol), "{url}");
    }
    assert!(validate_stream_url("srt://encoder.local").is_err());
    assert!(validate_stream_url("rtmp://ingest.example").is_err());
    assert_eq!(
        redact_stream_url("rtmp://ingest.example/live/key123"),
        "rtmp://ingest.example/live/***"
    );

    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("obs-key".into());
    assert_eq!(rtmp.validate(), Ok(StreamProtocol::Rtmp));
    assert_eq!(rtmp.redacted().stream_key.as_deref(), Some("***"));
    assert_eq!(
        rtmp.redacted().validate_redacted(),
        Ok(StreamProtocol::Rtmp)
    );
    assert!(!format!("{:?}", rtmp.redacted()).contains("obs-key"));
    assert_eq!(rtmp.scrub("refused obs-key"), "refused ***");
    let mut rendezvous = rtmp.clone();
    rendezvous.settings.mode = StreamMode::Rendezvous;
    assert_eq!(
        rendezvous.validate(),
        Err(NetworkStreamError::ModeUnsupported)
    );
    let mut low_port = NetworkStreamConfig::new("rtmp://0.0.0.0:80/live");
    low_port.settings.mode = StreamMode::Listener;
    assert_eq!(low_port.validate(), Err(NetworkStreamError::ListenPort));

    let mut srt = NetworkStreamConfig::new("srt://encoder.local:9000");
    srt.passphrase = Some("short".into());
    assert_eq!(srt.validate(), Err(NetworkStreamError::Passphrase));
    srt.passphrase = Some("long-enough-secret".into());
    srt.stream_id = Some("cam1".into());
    assert_eq!(srt.validate(), Ok(StreamProtocol::Srt));
    assert!(srt.connect_url().contains("streamid=cam1"));
    srt.stream_key = Some("rtmp-only".into());
    assert_eq!(srt.validate(), Err(NetworkStreamError::StreamKey));
    srt.stream_key = None;
    srt.settings.latency_ms = 9_000;
    assert_eq!(srt.validate(), Err(NetworkStreamError::Latency));

    // A bad field in a project names its field.
    let mut project = project();
    let mut bad = NetworkStreamConfig::new("srt://encoder.local:9000");
    bad.passphrase = Some("short".into());
    project.add_input(network(1, bad));
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.entity == Some(EntityRef::Input(input(1)))
                && error.field == "kind.network_stream.passphrase"),
        "{errors:?}"
    );
}

#[test]
fn two_inputs_never_listen_on_one_port() {
    let listener = |url: &str| {
        let mut config = NetworkStreamConfig::new(url);
        config.settings.mode = StreamMode::Listener;
        config
    };
    let mut project = project();
    project.add_input(network(1, listener("rtmp://0.0.0.0:1935/live")));
    // SRT listens on UDP: the same number is another port.
    project.add_input(network(2, listener("srt://0.0.0.0:1935")));
    // A caller to the same port listens on nothing.
    project.add_input(network(
        3,
        NetworkStreamConfig::new("rtmp://127.0.0.1:1935/live"),
    ));
    assert_eq!(project.validate(), Ok(()));

    project.add_input(network(4, listener("rtmp://127.0.0.1:1935/other")));
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.entity == Some(EntityRef::Input(input(4)))
                && error.field == "kind.network_stream.url"
                && error.kind == ValidationErrorKind::DuplicateReference),
        "{errors:?}"
    );
}
