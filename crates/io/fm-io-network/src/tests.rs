use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;

use fm_frame::{CodecId, NormalizedDuration, NormalizedTimestamp, TimeBase, VideoDimensions};

use super::*;

fn destination_id(value: u8) -> DestinationId {
    DestinationId::new(value).unwrap()
}

fn rendition_id(value: u32) -> RenditionId {
    RenditionId::new(NonZeroU32::new(value).unwrap())
}

fn duration(seconds: u64) -> NormalizedDuration {
    NormalizedDuration::from_nanos(seconds * 1_000_000_000).unwrap()
}

fn profile(width: u32, height: u32, video_bitrate: u64) -> RenditionProfile {
    let video = VideoRendition::new(
        CodecId::new("video/h264").unwrap(),
        "high",
        VideoDimensions::new(width, height).unwrap(),
        FrameRate::new(60_000, 1_001).unwrap(),
        ColorDescription::Rec709Limited,
        video_bitrate,
        120,
    )
    .unwrap()
    .with_codec_setting("b_frames", "2")
    .unwrap();
    let audio = AudioRendition::new(
        CodecId::new("audio/aac").unwrap(),
        "lc",
        128_000,
        48_000,
        2,
        vec![0, 1],
    )
    .unwrap();
    RenditionProfile::new(
        video,
        audio,
        TimingProfile::new(TimeBase::new(1, 90_000).unwrap(), 0),
    )
}

fn config(id: u8, capacity: usize, protocol: OutputProtocol) -> DestinationConfig {
    let tls = protocol
        .requires_tls()
        .then(|| TlsConfig::system_roots(Some("stream.example.test".to_owned())).unwrap());
    // HLS publishes into one local directory: a backup endpoint and a
    // credential are refused for it, so this shared helper omits both.
    let local = protocol == OutputProtocol::Hls;
    DestinationConfig::new(
        destination_id(id),
        protocol,
        Endpoint::new("stream.example.test", 1_935, "/live").unwrap(),
        (!local).then(|| Endpoint::new("backup.example.test", 1_935, "/live").unwrap()),
        tls,
        (!local).then(|| CredentialReference::new(format!("secret://stream-key/{id}")).unwrap()),
        QueueCapacity::new(capacity).unwrap(),
        ReconnectPolicy::new(100, 800, 2, Some(4)).unwrap(),
    )
    .unwrap()
}

fn packet(rendition: RenditionId, sequence: u64) -> OutputPacket {
    packet_with_random_access(rendition, sequence, sequence == 0, 1)
}

fn packet_with_random_access(
    rendition: RenditionId,
    sequence: u64,
    random_access: bool,
    payload_bytes: usize,
) -> OutputPacket {
    OutputPacket::new(
        rendition,
        sequence,
        NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 1_000_000),
        NormalizedDuration::from_nanos(1_000_000).unwrap(),
        random_access,
        Arc::<[u8]>::from(vec![
            u8::try_from(sequence).unwrap_or(u8::MAX);
            payload_bytes
        ]),
    )
    .unwrap()
    .with_encode_latency_ms(4)
}

#[derive(Default)]
struct FakeSink {
    connect_results: VecDeque<Result<ConnectionObservation, SinkError>>,
    write_results: VecDeque<Result<SinkWrite, SinkError>>,
    connected: bool,
    disconnects: u64,
    connection_hosts: Vec<String>,
    sequences: Vec<u64>,
    payloads: Vec<Arc<[u8]>>,
    /// Models an asynchronous transport whose accepted writes do not prove
    /// the peer received anything.
    unproven: bool,
}

impl FakeSink {
    fn with_connect_results(
        results: impl IntoIterator<Item = Result<ConnectionObservation, SinkError>>,
    ) -> Self {
        Self {
            connect_results: results.into_iter().collect(),
            ..Self::default()
        }
    }

    fn with_write_results(results: impl IntoIterator<Item = Result<SinkWrite, SinkError>>) -> Self {
        Self {
            write_results: results.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl TransportSink for FakeSink {
    fn connect(
        &mut self,
        _config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<ConnectionObservation, SinkError> {
        self.connection_hosts.push(endpoint.host().to_owned());
        let result = self
            .connect_results
            .pop_front()
            .unwrap_or(Ok(ConnectionObservation {
                round_trip_time_ms: Some(12),
            }));
        self.connected = result.is_ok();
        result
    }

    fn write(&mut self, packet: &OutputPacket) -> Result<SinkWrite, SinkError> {
        assert!(self.connected);
        let result =
            self.write_results
                .pop_front()
                .unwrap_or(Ok(SinkWrite::Sent(SendObservation {
                    round_trip_time_ms: Some(14),
                    packet_loss_ppm: Some(0),
                    retransmitted_packets: 0,
                    bitrate_bps: Some(4_000_000),
                })));
        if matches!(result, Ok(SinkWrite::Sent(_))) {
            self.sequences.push(packet.sequence());
            self.payloads.push(packet.shared_payload());
        }
        result
    }

    fn session_proven(&self) -> bool {
        !self.unproven
    }

    fn disconnect(&mut self) {
        self.connected = false;
        self.disconnects += 1;
    }
}

fn protocol_config(
    id: u8,
    protocol: OutputProtocol,
    tls: Option<TlsConfig>,
    credential: Option<CredentialReference>,
) -> Result<DestinationConfig, ConfigError> {
    DestinationConfig::new(
        destination_id(id),
        protocol,
        Endpoint::new("stream.example.test", 9_710, "/live").unwrap(),
        None,
        tls,
        credential,
        QueueCapacity::new(4).unwrap(),
        ReconnectPolicy::new(100, 800, 2, Some(4)).unwrap(),
    )
}

/// SRT follows the plain-RTMP TLS and credential matrices exactly: TLS
/// configuration is refused outright, a bare configuration is accepted, and an
/// optional credential reference is accepted like any other protocol's.
#[test]
fn srt_config_rejects_tls_and_mirrors_the_rtmp_matrices() {
    let tls = Some(TlsConfig::system_roots(Some("stream.example.test".to_owned())).unwrap());
    for protocol in [OutputProtocol::Rtmp, OutputProtocol::Srt] {
        assert_eq!(
            protocol_config(1, protocol, tls.clone(), None)
                .expect_err("plain protocols must refuse TLS"),
            ConfigError::TlsNotSupported,
            "{protocol:?}"
        );
        assert!(
            protocol_config(1, protocol, None, None).is_ok(),
            "{protocol:?}"
        );
        assert!(
            protocol_config(
                1,
                protocol,
                None,
                Some(CredentialReference::new("secret://stream-key").unwrap()),
            )
            .is_ok(),
            "{protocol:?} accepts a resolved credential reference"
        );
    }
    // RTMPS keeps its opposite rule: it requires the TLS configuration.
    assert_eq!(
        protocol_config(1, OutputProtocol::Rtmps, None, None).err(),
        Some(ConfigError::TlsRequired)
    );
    assert!(
        protocol_config(1, OutputProtocol::Rtmps, tls, None).is_ok(),
        "RTMPS with TLS stays valid"
    );
}

/// HLS is a local destination: TLS, a backup endpoint, and a credential each
/// describe a peer or a stream key that a local directory does not have, and
/// are refused rather than stored and ignored.
#[test]
fn hls_config_refuses_tls_backup_and_credential() {
    fn hls_config(
        backup: Option<Endpoint>,
        tls: Option<TlsConfig>,
        credential: Option<CredentialReference>,
    ) -> Result<DestinationConfig, ConfigError> {
        DestinationConfig::new(
            destination_id(1),
            OutputProtocol::Hls,
            Endpoint::local_directory("/srv/freemix/hls").unwrap(),
            backup,
            tls,
            credential,
            QueueCapacity::new(4).unwrap(),
            ReconnectPolicy::new(100, 800, 2, Some(4)).unwrap(),
        )
    }
    assert!(
        hls_config(None, None, None).is_ok(),
        "a bare local HLS configuration is valid"
    );
    assert_eq!(
        hls_config(
            Some(Endpoint::local_directory("/srv/freemix/hls-backup").unwrap()),
            None,
            None
        )
        .err(),
        Some(ConfigError::BackupNotSupported)
    );
    assert_eq!(
        hls_config(
            None,
            None,
            Some(CredentialReference::new("secret://stream-key").unwrap())
        )
        .err(),
        Some(ConfigError::CredentialNotSupported)
    );
    assert_eq!(
        hls_config(
            None,
            Some(TlsConfig::system_roots(Some("stream.example.test".to_owned())).unwrap()),
            None
        )
        .err(),
        Some(ConfigError::TlsNotSupported)
    );
}

/// A local-directory endpoint carries the absolute directory in its path and
/// placeholders the transport never renders, and it applies the same path rules
/// the codec applies to an `hls://` destination.
#[test]
fn local_directory_endpoints_are_validated_and_carry_placeholders() {
    let endpoint = Endpoint::local_directory("/srv/freemix/hls").unwrap();
    assert_eq!(endpoint.path(), "/srv/freemix/hls");
    assert_eq!(endpoint.host(), "localhost");
    assert_eq!(endpoint.port().get(), 1);
    for rejected in [
        "",
        "relative/path",
        "/",
        "/trailing/",
        "/double//segment",
        "/dot/./segment",
        "/dotdot/../segment",
        "/non ascii/é",
    ] {
        assert_eq!(
            Endpoint::local_directory(rejected).err(),
            Some(ConfigError::InvalidPath),
            "{rejected}"
        );
    }
    // A path the codec would accept but the endpoint's own length bound cannot
    // carry is still refused, not truncated.
    assert_eq!(
        Endpoint::local_directory(format!("/{}", "a".repeat(2_048))).err(),
        Some(ConfigError::InvalidPath)
    );
}

#[test]
fn five_outputs_share_one_exact_rendition_and_payload() {
    let shared_profile = profile(1_920, 1_080, 6_000_000);
    let requests: Vec<_> = (1..=5)
        .map(|id| DestinationRenditions::single(destination_id(id), shared_profile.clone()))
        .collect();
    let plan = RenditionPlanner::plan(&requests).unwrap();
    assert_eq!(plan.renditions().len(), 1);
    assert_eq!(plan.renditions()[0].destinations().len(), 5);

    let mut outputs = OutputSet::new();
    let mut sinks: Vec<_> = (1..=5).map(|_| FakeSink::default()).collect();
    for id in 1..=5 {
        let protocol = match id {
            1 | 5 => OutputProtocol::Rtmp,
            2 => OutputProtocol::Rtmps,
            3 => OutputProtocol::Hls,
            _ => OutputProtocol::LiveLan,
        };
        outputs.add_destination(config(id, 2, protocol)).unwrap();
        outputs.start(destination_id(id)).unwrap();
        assert_eq!(
            outputs
                .poll(destination_id(id), 0, &mut sinks[usize::from(id - 1)])
                .unwrap(),
            PollEvent::Connected
        );
    }

    let encoded = packet(plan.renditions()[0].id(), 0);
    let original_payload = encoded.shared_payload();
    let outcomes = outputs.enqueue_rendition(&plan, &encoded).unwrap();
    assert_eq!(outcomes.len(), 5);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.status == EnqueueStatus::Accepted)
    );
    for id in 1..=5 {
        assert_eq!(
            outputs
                .poll(destination_id(id), 1, &mut sinks[usize::from(id - 1)])
                .unwrap(),
            PollEvent::PacketSent { sequence: 0 }
        );
        assert!(Arc::ptr_eq(
            &original_payload,
            &sinks[usize::from(id - 1)].payloads[0]
        ));
        assert_eq!(
            outputs
                .telemetry(destination_id(id))
                .unwrap()
                .packets_sent(),
            1
        );
    }
}

#[test]
fn rendition_planner_shares_exact_profiles_only() {
    let exact = profile(1_920, 1_080, 6_000_000);
    let different_bitrate = profile(1_920, 1_080, 6_000_001);
    let plan = RenditionPlanner::plan(&[
        DestinationRenditions::single(destination_id(1), exact.clone()),
        DestinationRenditions::single(destination_id(2), exact),
        DestinationRenditions::single(destination_id(3), different_bitrate),
    ])
    .unwrap();
    assert_eq!(plan.renditions().len(), 2);
    assert_eq!(
        plan.renditions()[0].destinations(),
        [destination_id(1), destination_id(2)]
    );
    assert_eq!(plan.renditions()[1].destinations(), [destination_id(3)]);
}

#[test]
fn reconnect_drops_interframes_and_resumes_at_queued_random_access() {
    let plan = RenditionPlanner::plan(&[
        DestinationRenditions::single(destination_id(1), profile(1_920, 1_080, 6_000_000)),
        DestinationRenditions::single(destination_id(2), profile(1_920, 1_080, 6_000_000)),
    ])
    .unwrap();
    let rendition = plan.renditions()[0].id();
    let mut outputs = OutputSet::new();
    outputs
        .add_destination(config(1, 4, OutputProtocol::Rtmp))
        .unwrap();
    outputs
        .add_destination(config(2, 2, OutputProtocol::Rtmp))
        .unwrap();
    outputs.start(destination_id(1)).unwrap();
    outputs.start(destination_id(2)).unwrap();

    let write_failure = SinkError::new(FailureStage::Write, Some(54), "reset", true);
    let mut sink = FakeSink::with_write_results([
        Err(write_failure),
        Ok(SinkWrite::Sent(SendObservation::default())),
    ]);
    let mut healthy = FakeSink::default();
    assert_eq!(
        outputs.poll(destination_id(1), 0, &mut sink).unwrap(),
        PollEvent::Connected
    );
    outputs.poll(destination_id(2), 0, &mut healthy).unwrap();
    outputs
        .enqueue_rendition(&plan, &packet(rendition, 1))
        .unwrap();

    assert_eq!(
        outputs.poll(destination_id(1), 10, &mut sink).unwrap(),
        PollEvent::ReconnectScheduled { retry_at_ms: 110 }
    );
    assert_eq!(
        outputs.poll(destination_id(2), 10, &mut healthy).unwrap(),
        PollEvent::PacketSent { sequence: 1 }
    );
    for sequence in 2..=4 {
        outputs
            .enqueue(
                destination_id(1),
                packet_with_random_access(
                    rendition,
                    sequence,
                    sequence == 3,
                    usize::try_from(sequence).unwrap(),
                ),
            )
            .unwrap();
    }
    assert_eq!(
        outputs.state(destination_id(2)),
        Some(DestinationState::Live)
    );
    assert_eq!(
        outputs.connection_target(destination_id(1)),
        Some(ConnectionTarget::Backup)
    );
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(4));
    assert_eq!(
        outputs.poll(destination_id(1), 109, &mut sink).unwrap(),
        PollEvent::WaitingToReconnect { retry_at_ms: 110 }
    );
    assert_eq!(
        outputs.poll(destination_id(1), 110, &mut sink).unwrap(),
        PollEvent::AwaitingRandomAccess {
            dropped_packets: 2,
            dropped_bytes: 3,
        }
    );
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(2));
    assert_eq!(outputs.queued_bytes(destination_id(1)), Some(7));
    assert_eq!(
        outputs.poll(destination_id(1), 111, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 3 }
    );
    assert_eq!(
        outputs.poll(destination_id(1), 112, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 4 }
    );
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(0));
    assert_eq!(
        sink.connection_hosts,
        ["stream.example.test", "backup.example.test"]
    );
    assert_eq!(sink.sequences, [3, 4]);
    let telemetry = outputs.telemetry(destination_id(1)).unwrap();
    assert_eq!(telemetry.reconnects(), 1);
    assert_eq!(telemetry.recovery_dropped_packets(), 2);
    assert_eq!(telemetry.recovery_dropped_bytes(), 3);
    assert_eq!(telemetry.failure_count(), 1);
    assert_eq!(sink.disconnects, 1);
}

#[test]
fn recovery_queue_priority_accepts_first_random_access() {
    let (mut outputs, mut sink, id) = empty_recovery_queue(3);
    for sequence in 2..=4 {
        outputs
            .enqueue(
                destination_id(1),
                packet_with_random_access(id, sequence, false, usize::try_from(sequence).unwrap()),
            )
            .unwrap();
    }
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(3));
    assert_eq!(
        outputs
            .enqueue(destination_id(1), packet_with_random_access(id, 5, true, 5),)
            .unwrap(),
        EnqueueStatus::Accepted
    );
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(1));
    assert_eq!(outputs.queued_bytes(destination_id(1)), Some(5));
    let telemetry = outputs.telemetry(destination_id(1)).unwrap();
    assert_eq!(telemetry.recovery_dropped_packets(), 4);
    assert_eq!(telemetry.recovery_dropped_bytes(), 10);
    assert_eq!(telemetry.backpressure_events(), 0);
    assert_eq!(
        outputs.poll(destination_id(1), 111, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 5 }
    );
    assert_eq!(sink.sequences, [5]);

    outputs.stop(destination_id(1), &mut sink).unwrap();
    sink.connect_results.push_back(Err(SinkError::new(
        FailureStage::Connect,
        Some(61),
        "refused",
        true,
    )));
    outputs.start(destination_id(1)).unwrap();
    outputs
        .enqueue(
            destination_id(1),
            packet_with_random_access(id, 6, false, 6),
        )
        .unwrap();
    assert_eq!(
        outputs.poll(destination_id(1), 200, &mut sink).unwrap(),
        PollEvent::ReconnectScheduled { retry_at_ms: 300 }
    );
    assert_eq!(
        outputs.poll(destination_id(1), 300, &mut sink).unwrap(),
        PollEvent::AwaitingRandomAccess {
            dropped_packets: 1,
            dropped_bytes: 6,
        }
    );
    assert_eq!(
        outputs.state(destination_id(1)),
        Some(DestinationState::AwaitingRandomAccess)
    );
    outputs
        .enqueue(destination_id(1), packet_with_random_access(id, 7, true, 7))
        .unwrap();
    assert_eq!(
        outputs.poll(destination_id(1), 301, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 7 }
    );
    assert_eq!(sink.sequences, [5, 7]);
}

fn empty_recovery_queue(capacity: usize) -> (OutputSet, FakeSink, RenditionId) {
    let id = rendition_id(1);
    let mut outputs = OutputSet::new();
    outputs
        .add_destination(config(1, capacity, OutputProtocol::Rtmp))
        .unwrap();
    outputs.start(destination_id(1)).unwrap();
    let mut sink = FakeSink::with_write_results([Err(SinkError::new(
        FailureStage::Write,
        Some(54),
        "reset",
        true,
    ))]);

    outputs.poll(destination_id(1), 0, &mut sink).unwrap();
    outputs
        .enqueue(
            destination_id(1),
            packet_with_random_access(id, 1, false, 1),
        )
        .unwrap();
    assert_eq!(
        outputs.poll(destination_id(1), 10, &mut sink).unwrap(),
        PollEvent::ReconnectScheduled { retry_at_ms: 110 }
    );
    assert_eq!(
        outputs.poll(destination_id(1), 110, &mut sink).unwrap(),
        PollEvent::AwaitingRandomAccess {
            dropped_packets: 1,
            dropped_bytes: 1,
        }
    );
    (outputs, sink, id)
}

#[test]
fn recovery_queue_priority_preserves_queued_random_access() {
    let (mut outputs, mut sink, id) = empty_recovery_queue(3);
    outputs
        .enqueue(destination_id(1), packet_with_random_access(id, 2, true, 2))
        .unwrap();
    for sequence in 3..=4 {
        outputs
            .enqueue(
                destination_id(1),
                packet_with_random_access(id, sequence, false, usize::try_from(sequence).unwrap()),
            )
            .unwrap();
    }
    let rejected = outputs
        .enqueue(destination_id(1), packet_with_random_access(id, 5, true, 5))
        .unwrap();
    let EnqueueStatus::Backpressure(rejected) = rejected else {
        panic!("protected recovery packet did not keep normal backpressure");
    };
    assert_eq!(rejected.sequence(), 5);
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(3));
    assert_eq!(outputs.queued_bytes(destination_id(1)), Some(9));
    let telemetry = outputs.telemetry(destination_id(1)).unwrap();
    assert_eq!(telemetry.recovery_dropped_packets(), 1);
    assert_eq!(telemetry.recovery_dropped_bytes(), 1);
    assert_eq!(telemetry.backpressure_events(), 1);
    assert_eq!(
        outputs.poll(destination_id(1), 111, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 2 }
    );
    assert_eq!(sink.sequences, [2]);
}

#[test]
fn non_retryable_primary_connection_failure_never_contacts_backup() {
    let mut outputs = OutputSet::new();
    outputs
        .add_destination(config(1, 1, OutputProtocol::Rtmp))
        .unwrap();
    outputs.start(destination_id(1)).unwrap();
    let mut sink = FakeSink::with_connect_results([Err(SinkError::new(
        FailureStage::Authentication,
        Some(401),
        "rejected",
        false,
    ))]);

    assert_eq!(
        outputs.poll(destination_id(1), 0, &mut sink).unwrap(),
        PollEvent::Failed
    );
    assert_eq!(
        outputs.connection_target(destination_id(1)),
        Some(ConnectionTarget::Primary)
    );
    assert_eq!(
        outputs.poll(destination_id(1), 1_000, &mut sink).unwrap(),
        PollEvent::Idle
    );
    assert_eq!(sink.connection_hosts, ["stream.example.test"]);
}

#[test]
fn bounded_queue_and_sink_congestion_retain_order() {
    let mut outputs = OutputSet::new();
    outputs
        .add_destination(config(1, 1, OutputProtocol::Rtmp))
        .unwrap();
    outputs.start(destination_id(1)).unwrap();
    let mut sink = FakeSink::with_write_results([
        Ok(SinkWrite::Congested(CongestionObservation {
            round_trip_time_ms: Some(200),
            available_bitrate_bps: Some(500_000),
        })),
        Ok(SinkWrite::Sent(SendObservation::default())),
    ]);
    outputs.poll(destination_id(1), 0, &mut sink).unwrap();
    let id = rendition_id(1);
    assert_eq!(
        outputs.enqueue(destination_id(1), packet(id, 1)).unwrap(),
        EnqueueStatus::Accepted
    );
    let rejected = outputs.enqueue(destination_id(1), packet(id, 2)).unwrap();
    let EnqueueStatus::Backpressure(rejected) = rejected else {
        panic!("full destination did not apply backpressure");
    };
    assert_eq!(rejected.sequence(), 2);
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(1));
    assert_eq!(
        outputs.poll(destination_id(1), 1, &mut sink).unwrap(),
        PollEvent::Congested
    );
    assert_eq!(outputs.queue_depth(destination_id(1)), Some(1));
    assert_eq!(
        outputs.poll(destination_id(1), 2, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 1 }
    );
    assert_eq!(sink.sequences, [1]);
    let telemetry = outputs.telemetry(destination_id(1)).unwrap();
    assert_eq!(telemetry.backpressure_events(), 1);
    assert!(telemetry.congestion_events() >= 1);
    assert_eq!(telemetry.queue_high_water_packets(), 1);
}

#[test]
fn dns_and_tls_failures_are_classified_and_retained() {
    let mut outputs = OutputSet::new();
    outputs
        .add_destination(config(1, 1, OutputProtocol::Rtmps))
        .unwrap();
    outputs
        .add_destination(config(2, 1, OutputProtocol::Rtmps))
        .unwrap();
    outputs.start(destination_id(1)).unwrap();
    outputs.start(destination_id(2)).unwrap();
    let mut dns = FakeSink::with_connect_results([Err(SinkError::new(
        FailureStage::Dns,
        Some(-2),
        "name not found",
        true,
    ))]);
    let mut tls = FakeSink::with_connect_results([Err(SinkError::new(
        FailureStage::Tls,
        Some(42),
        "certificate expired",
        false,
    ))]);
    assert_eq!(
        outputs.poll(destination_id(1), 50, &mut dns).unwrap(),
        PollEvent::ReconnectScheduled { retry_at_ms: 150 }
    );
    assert_eq!(
        outputs.poll(destination_id(2), 50, &mut tls).unwrap(),
        PollEvent::Failed
    );
    let dns_record = outputs
        .telemetry(destination_id(1))
        .unwrap()
        .latest_failure()
        .unwrap();
    assert_eq!(dns_record.stage, FailureStage::Dns);
    assert!(dns_record.retryable);
    let tls_record = outputs
        .telemetry(destination_id(2))
        .unwrap()
        .latest_failure()
        .unwrap();
    assert_eq!(tls_record.stage, FailureStage::Tls);
    assert_eq!(tls_record.message, "certificate expired");
    assert!(!tls_record.retryable);
}

#[test]
fn impairment_model_is_deterministic_and_reports_telemetry() {
    let model = ImpairmentModel::new(
        100,
        20,
        1_000_000,
        1_000_000,
        1_000_000,
        Some(750_000),
        true,
        true,
        true,
    )
    .unwrap();
    let first = model.evaluate(7);
    assert_eq!(first, model.evaluate(7));
    assert!((80..=120).contains(&first.delay_ms));
    assert!(first.dropped && first.reordered && first.duplicated);
    let mut telemetry = ImpairmentTelemetry::default();
    telemetry.observe(first);
    assert_eq!(telemetry.evaluated_packets(), 1);
    assert_eq!(telemetry.dropped_packets(), 1);
    assert_eq!(telemetry.disconnect_events(), 1);
    assert_eq!(telemetry.dns_failures(), 1);
    assert_eq!(telemetry.tls_failures(), 1);
    assert_eq!(telemetry.bandwidth_limit_bps(), Some(750_000));
}

#[test]
fn abr_validation_and_hls_sequences_are_aligned_and_bounded() {
    let low = profile(1_280, 720, 3_000_000);
    let high = profile(1_920, 1_080, 6_000_000);
    let ladder = AbrLadder::new(vec![
        AbrVariant::new("720p", low.clone()).unwrap(),
        AbrVariant::new("1080p", high.clone()).unwrap(),
    ])
    .unwrap();
    let plan = RenditionPlanner::plan(&[DestinationRenditions::ladder(destination_id(1), ladder)])
        .unwrap();
    assert_eq!(plan.renditions().len(), 2);
    let low_id = plan.renditions()[0].id();
    let high_id = plan.renditions()[1].id();

    let invalid_high = RenditionProfile::new(
        VideoRendition::new(
            CodecId::new("video/h264").unwrap(),
            "high",
            VideoDimensions::new(1_920, 1_080).unwrap(),
            FrameRate::new(30, 1).unwrap(),
            ColorDescription::Rec709Limited,
            7_000_000,
            120,
        )
        .unwrap()
        .with_codec_setting("b_frames", "2")
        .unwrap(),
        high.audio().clone(),
        high.timing(),
    );
    assert_eq!(
        AbrLadder::new(vec![
            AbrVariant::new("low", low).unwrap(),
            AbrVariant::new("bad", invalid_high).unwrap(),
        ]),
        Err(RenditionError::AbrIncompatibleProfiles)
    );

    let variants = vec![
        HlsVariantMetadata::new(
            low_id,
            "720p",
            3_128_000,
            VideoDimensions::new(1_280, 720).unwrap(),
            "avc1.64002a,mp4a.40.2",
            "720p.m3u8",
        )
        .unwrap(),
        HlsVariantMetadata::new(
            high_id,
            "1080p",
            6_128_000,
            VideoDimensions::new(1_920, 1_080).unwrap(),
            "avc1.64002a,mp4a.40.2",
            "1080p.m3u8",
        )
        .unwrap(),
    ];
    let mut hls =
        HlsAbrCoordinator::new(variants, 7, duration(2), 10, 2, HlsPlaylistType::Live).unwrap();
    hls.append(segment(low_id, 10, 0, 0)).unwrap();
    hls.append(segment(high_id, 10, 0, 0)).unwrap();
    hls.append(segment(low_id, 11, 2, 0)).unwrap();
    let misaligned = HlsSegmentMetadata::new(
        high_id,
        11,
        0,
        NormalizedTimestamp::from_nanos(2_100_000_000),
        duration(2),
        1_000,
        true,
        "high-11.ts",
    )
    .unwrap();
    assert_eq!(hls.append(misaligned), Err(HlsError::AbrSequenceMismatch));
    hls.append(segment(high_id, 11, 2, 0)).unwrap();
    hls.append(segment(low_id, 12, 4, 1)).unwrap();
    hls.append(segment(high_id, 12, 4, 1)).unwrap();
    let metadata = hls.playlist(low_id).unwrap();
    assert_eq!(metadata.media_sequence(), 11);
    assert_eq!(metadata.discontinuity_sequence(), 0);
    assert_eq!(metadata.segments().len(), 2);
    assert_eq!(metadata.segments()[1].discontinuity_sequence(), 1);
    hls.finish();
    assert!(hls.playlist(high_id).unwrap().is_end_list());
}

fn segment(
    rendition: RenditionId,
    sequence: u64,
    start_seconds: i64,
    discontinuity: u64,
) -> HlsSegmentMetadata {
    HlsSegmentMetadata::new(
        rendition,
        sequence,
        discontinuity,
        NormalizedTimestamp::from_nanos(start_seconds * 1_000_000_000),
        duration(2),
        1_000,
        true,
        format!("{}-{sequence}.ts", rendition.get()),
    )
    .unwrap()
}

/// A destination with exactly two retries after the first attempt.
fn budget_config(id: u8) -> DestinationConfig {
    DestinationConfig::new(
        destination_id(id),
        OutputProtocol::Rtmp,
        Endpoint::new("stream.example.test", 1_935, "/live").unwrap(),
        None,
        None,
        None,
        QueueCapacity::new(4).unwrap(),
        ReconnectPolicy::new(100, 800, 2, Some(2)).unwrap(),
    )
    .unwrap()
}

/// One successful admission plus one retryable failure per budget cycle.
fn budget_sink(unproven: bool, cycles: usize) -> FakeSink {
    let failure = || SinkError::new(FailureStage::Write, Some(104), "reset", true);
    let mut write_results = VecDeque::new();
    for _ in 0..cycles {
        write_results.push_back(Ok(SinkWrite::Sent(SendObservation::default())));
        write_results.push_back(Err(failure()));
    }
    FakeSink {
        unproven,
        write_results,
        ..FakeSink::default()
    }
}

#[test]
fn an_unproven_session_never_clears_the_reconnect_budget() {
    let mut outputs = OutputSet::new();
    outputs.add_destination(budget_config(1)).unwrap();
    let id = destination_id(1);
    let rendition = rendition_id(1);
    let mut sink = budget_sink(true, 3);
    outputs.start(id).unwrap();
    assert_eq!(
        outputs.poll(id, 0, &mut sink).unwrap(),
        PollEvent::Connected
    );

    // Each cycle delivers one packet and then hits one retryable failure; the
    // failed packet replays after the reconnect because it was never popped.
    outputs
        .enqueue(id, packet_with_random_access(rendition, 0, true, 1))
        .unwrap();
    assert_eq!(
        outputs.poll(id, 1, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 0 }
    );
    let mut now = 2;
    for cycle in 0..3_u64 {
        outputs
            .enqueue(
                id,
                packet_with_random_access(rendition, 10 + cycle, true, 1),
            )
            .unwrap();
        let event = outputs.poll(id, now, &mut sink).unwrap();
        if cycle < 2 {
            let PollEvent::ReconnectScheduled { retry_at_ms } = event else {
                panic!("expected a scheduled retry, got {event:?}");
            };
            now = retry_at_ms;
            // The reconnect stays pending because this sink never proves
            // delivery, so the budget keeps counting failures.
            let event = outputs.poll(id, now, &mut sink).unwrap();
            assert!(
                matches!(event, PollEvent::AwaitingRandomAccess { .. }),
                "{event:?}"
            );
            now += 1;
            assert_eq!(
                outputs.poll(id, now, &mut sink).unwrap(),
                PollEvent::PacketSent {
                    sequence: 10 + cycle
                },
                "the failed packet replays after the reconnect"
            );
            now += 1;
        } else {
            assert_eq!(
                event,
                PollEvent::Failed,
                "the third failure exhausts the two-retry budget"
            );
        }
    }
    let telemetry = outputs.telemetry(id).unwrap();
    assert_eq!(telemetry.connect_attempts(), 3);
    assert_eq!(telemetry.reconnects(), 0);
    assert!(telemetry.failures().all(|failure| failure.retryable));
}

#[test]
fn a_proven_session_clears_the_reconnect_budget_and_counts_the_recovery() {
    let mut outputs = OutputSet::new();
    outputs.add_destination(budget_config(1)).unwrap();
    let id = destination_id(1);
    let rendition = rendition_id(1);
    let mut sink = budget_sink(false, 3);
    outputs.start(id).unwrap();
    assert_eq!(
        outputs.poll(id, 0, &mut sink).unwrap(),
        PollEvent::Connected
    );

    outputs
        .enqueue(id, packet_with_random_access(rendition, 0, true, 1))
        .unwrap();
    assert_eq!(
        outputs.poll(id, 1, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 0 }
    );
    let mut now = 2;
    for cycle in 0..2_u64 {
        outputs
            .enqueue(
                id,
                packet_with_random_access(rendition, 10 + cycle, true, 1),
            )
            .unwrap();
        let PollEvent::ReconnectScheduled { retry_at_ms } =
            outputs.poll(id, now, &mut sink).unwrap()
        else {
            panic!("a proven session bounds each failure to one attempt");
        };
        now = retry_at_ms;
        let event = outputs.poll(id, now, &mut sink).unwrap();
        assert!(
            matches!(event, PollEvent::AwaitingRandomAccess { .. }),
            "{event:?}"
        );
        now += 1;
        assert_eq!(
            outputs.poll(id, now, &mut sink).unwrap(),
            PollEvent::PacketSent {
                sequence: 10 + cycle
            }
        );
        now += 1;
    }
    // A third failure still costs only its own attempt: every reconnect that
    // delivered a packet cleared the budget.
    outputs
        .enqueue(id, packet_with_random_access(rendition, 12, true, 1))
        .unwrap();
    let PollEvent::ReconnectScheduled { retry_at_ms } = outputs.poll(id, now, &mut sink).unwrap()
    else {
        panic!("a proven session never exhausts the budget");
    };
    let telemetry = outputs.telemetry(id).unwrap();
    assert_eq!(telemetry.connect_attempts(), 3);
    assert_eq!(
        telemetry.reconnects(),
        2,
        "both reconnects after the first proven session delivered a packet"
    );
    assert_eq!(
        outputs.state(id),
        Some(DestinationState::WaitingToReconnect {
            attempt: 1,
            retry_at_ms,
        }),
        "a proven session never exhausts the budget"
    );
}

#[test]
fn hls_media_playlist_renders_and_round_trips_through_bounded_parser() {
    let id = rendition_id(1);
    let mut playlist = HlsPlaylist::new(7, id, duration(2), 10, 4, HlsPlaylistType::Event).unwrap();
    for (sequence, disc, start_nanos, dur_nanos, uri) in [
        (10, 1, 0, 2_000_000_000, "seg-00010.ts"),
        (11, 1, 2_000_000_000, 1_500_000_000, "seg-00011.ts"),
        (12, 2, 3_500_000_000, 2_000_000_000, "seg-00012.ts"),
    ] {
        playlist
            .append(
                HlsSegmentMetadata::new(
                    id,
                    sequence,
                    disc,
                    NormalizedTimestamp::from_nanos(start_nanos),
                    NormalizedDuration::from_nanos(dur_nanos).unwrap(),
                    4_096,
                    true,
                    uri,
                )
                .unwrap(),
            )
            .unwrap();
    }
    playlist.finish();

    let metadata = playlist.metadata();
    let rendered = metadata.render_m3u8();
    assert_eq!(
        rendered,
        concat!(
            "#EXTM3U\n",
            "#EXT-X-VERSION:7\n",
            "#EXT-X-TARGETDURATION:2\n",
            "#EXT-X-MEDIA-SEQUENCE:10\n",
            "#EXT-X-DISCONTINUITY-SEQUENCE:1\n",
            "#EXT-X-PLAYLIST-TYPE:EVENT\n",
            "#EXT-X-INDEPENDENT-SEGMENTS\n",
            "#EXTINF:2.000000,\n",
            "seg-00010.ts\n",
            "#EXTINF:1.500000,\n",
            "seg-00011.ts\n",
            "#EXT-X-DISCONTINUITY\n",
            "#EXTINF:2.000000,\n",
            "seg-00012.ts\n",
            "#EXT-X-ENDLIST\n",
        )
    );

    let parsed = HlsPlaylistMetadata::parse_m3u8(&rendered, id, 4_096).unwrap();
    assert_eq!(parsed, metadata);
    assert_eq!(parse_media_m3u8(&rendered, id, 4_096).unwrap(), metadata);
    assert_eq!(parsed.render_m3u8(), rendered);
}

#[test]
fn hls_media_playlist_parser_rejects_oversized_and_malformed_inputs() {
    let id = rendition_id(1);
    let oversized = format!("#EXTM3U\n{}", "a".repeat(MAX_M3U8_BYTES));
    assert!(matches!(
        HlsPlaylistMetadata::parse_m3u8(&oversized, id, 1_024),
        Err(HlsError::MalformedPlaylist(_))
    ));
    assert_eq!(
        HlsPlaylistMetadata::parse_m3u8("#EXTM3U\n#EXT-X-TARGETDURATION:2\n", id, 0),
        Err(HlsError::EmptySegment)
    );
    for malformed in [
        "",
        "not an m3u8",
        "#EXTM3U\n#EXT-X-VERSION:3\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000000\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000000,\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.000000,\nseg.ts\n#EXT-X-ENDLIST\n#EXTINF:2.000000,\nseg2.ts\n",
    ] {
        assert!(
            matches!(
                HlsPlaylistMetadata::parse_m3u8(malformed, id, 1_024),
                Err(HlsError::MalformedPlaylist(_))
            ),
            "expected MalformedPlaylist for: {malformed:?}"
        );
    }
    assert_eq!(
        HlsPlaylistMetadata::parse_m3u8(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:3.000000,\nseg-0.ts\n",
            id,
            1_024,
        ),
        Err(HlsError::DurationExceedsTarget)
    );
}

#[test]
fn hls_master_playlist_round_trips_and_ffmpeg_variants_align_in_abr_coordinator() {
    use core::fmt::Write as _;

    let low_id = rendition_id(1);
    let high_id = rendition_id(2);
    let variants = vec![
        HlsVariantMetadata::new(
            low_id,
            "360p",
            1_240_800,
            VideoDimensions::new(640, 360).unwrap(),
            "avc1.64001e,mp4a.40.2",
            "0/index.m3u8",
        )
        .unwrap(),
        HlsVariantMetadata::new(
            high_id,
            "720p",
            3_128_000,
            VideoDimensions::new(1_280, 720).unwrap(),
            "avc1.64002a,mp4a.40.2",
            "1/index.m3u8",
        )
        .unwrap(),
    ];
    let mut coordinator = HlsAbrCoordinator::new(
        variants.clone(),
        6,
        duration(2),
        0,
        6,
        HlsPlaylistType::Live,
    )
    .unwrap();

    let rendered_master = coordinator.render_master_m3u8();
    let parsed_master = parse_master_m3u8(&rendered_master).unwrap();
    assert_eq!(parsed_master.len(), 2);
    for (parsed, expected) in parsed_master.iter().zip(&variants) {
        assert_eq!(parsed.bandwidth_bps(), expected.bandwidth_bps());
        assert_eq!(parsed.resolution(), expected.dimensions());
        assert_eq!(parsed.codecs(), Some(expected.codecs()));
        assert_eq!(parsed.playlist_uri(), expected.playlist_uri());
    }

    let ffmpeg_master = concat!(
        "#EXTM3U\n",
        "#EXT-X-VERSION:3\n",
        "#EXT-X-INDEPENDENT-SEGMENTS\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=1240800,RESOLUTION=640x360,CODECS=\"avc1.64001e,mp4a.40.2\"\n",
        "0/index.m3u8\n\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=3128000,RESOLUTION=1280x720,CODECS=\"avc1.64002a,mp4a.40.2\"\n",
        "1/index.m3u8\n",
    );
    assert_eq!(parse_master_m3u8(ffmpeg_master).unwrap(), parsed_master);
    assert_eq!(
        parse_master_m3u8("#EXTM3U\n#EXT-X-VERSION:6\n"),
        Err(HlsError::InvalidVariantCount)
    );
    let mut nine_variants = String::from("#EXTM3U\n#EXT-X-VERSION:6\n");
    for index in 0..9 {
        let _ = write!(
            nine_variants,
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION=640x360\n{index}/index.m3u8\n",
            1_000_000 + index * 100_000
        );
    }
    assert_eq!(
        parse_master_m3u8(&nine_variants),
        Err(HlsError::InvalidVariantCount)
    );

    let variant_0_m3u8 = concat!(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
        "#EXT-X-INDEPENDENT-SEGMENTS\n#EXTINF:2.000000,\n0/segment-00000.ts\n",
        "#EXTINF:2.000000,\n0/segment-00001.ts\n",
    );
    let variant_1_m3u8 = concat!(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
        "#EXT-X-INDEPENDENT-SEGMENTS\n#EXTINF:2.000000,\n1/segment-00000.ts\n",
        "#EXTINF:2.000000,\n1/segment-00001.ts\n",
    );
    let parsed_0 = HlsPlaylistMetadata::parse_m3u8(variant_0_m3u8, low_id, 8_192).unwrap();
    let parsed_1 = HlsPlaylistMetadata::parse_m3u8(variant_1_m3u8, high_id, 16_384).unwrap();
    for (seg_0, seg_1) in parsed_0.segments().iter().zip(parsed_1.segments()) {
        coordinator.append(seg_0.clone()).unwrap();
        coordinator.append(seg_1.clone()).unwrap();
    }
    assert_eq!(coordinator.playlist(low_id).unwrap(), parsed_0);
    assert_eq!(coordinator.playlist(high_id).unwrap(), parsed_1);
}
