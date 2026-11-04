/// The last entry of a playlist that does not loop has no next entry:
/// the engine refuses its end trigger's `playlist_control next` as a
/// conflict (with no reason that tells it apart from a passing one), and
/// the feed takes the playlist state's word that it cannot clear. The
/// refusal is final, so no dry-run repeats every second while the clip
/// holds, and nothing is cut later.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_with_no_next_entry_is_final() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{PlaylistOperation, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(500, fm_protocol::PlaylistStart::Now),
    );
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [test_command(
            "next",
            "next-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Next,
            },
        )],
    );
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 2, take, .. } = airing(&control) else {
        panic!("entry 2 airs: {:?}", airing(&control));
    };
    assert!(!media_clock::has_next_entry(&control.borrow()));
    let trigger = MediaEndTrigger {
        input: test_input_id(1),
        frame: control.borrow().frame_cursor(),
    };
    let revision = control.borrow().diagnostics().current_revision;
    let first = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &development_principal().unwrap(),
        &server,
        &media_clock::end_trigger_command(trigger, revision),
        0,
        None,
        None,
    )
    .unwrap();
    let CommandResult::Rejected {
        code,
        retryable,
        reason,
        ..
    } = &first.result
    else {
        panic!("no next entry: {:?}", first.result);
    };
    // What the retry policy alone would retry for as long as it airs.
    assert!(crate::clock_retry::retries(code, *retryable));
    assert_eq!(*reason, None);

    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &first.result,
        start,
    );
    assert!(!feed.pending(), "final, not retried");
    let attempts = store.attempts.get();
    for pass in 1..=60 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            start + Duration::from_millis(40) * pass,
            true,
        )
        .unwrap();
    }
    assert_eq!(store.attempts.get(), attempts);
    assert!(
        matches!(
            airing(&control),
            PlaylistRunState::Playing { entry: 2, take: now, .. } if now == take
        ),
        "{:?}",
        airing(&control)
    );
}

#[test]
fn record_commands_project_both_slot_flags_across_replay_and_checkpoint() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("record-roundtrip.freemix");
    let store = ProjectStore::new(&project_path).unwrap();
    let durable = test_project_with_recording([true, false]);
    store.save(&durable).unwrap();

    // An accepted RecordStop for slot 1 flips only that slot's durable
    // flag and stays only in the journal: a crash right now must still
    // restart with slot 1 stopped and slot 2 untouched.
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let mut durable = durable;
    {
        let journal = DurableJournal::open(&store).unwrap();
        let stopped = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "rec-stop",
                "rec-stop-key",
                CommandPayload::RecordStop {
                    recorder: wire_recorder(1),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            stopped.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        assert_eq!(durable.project().recording_desired_slots(), [false, false]);
    }
    assert_eq!(store.load().unwrap().position().revision, 0);

    // Journal replay alone reproduces the per-slot stopped state.
    let reloaded = ProjectStore::new(&project_path).unwrap();
    let manifest = reloaded.load().unwrap();
    let scan = reloaded.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 1);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position().revision, 1);
    assert_eq!(replayed.project().recording_desired_slots(), [false, false]);

    // A later RecordStart for slot 2 leaves slot 1 stopped, and both flags
    // survive a full checkpoint: the durable mirror never rewrites a slot
    // the command did not address.
    let mut control = test_control(&replayed);
    let server = test_server(&control);
    let mut durable = replayed;
    {
        let journal = DurableJournal::open(&reloaded).unwrap();
        let started = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "rec-start",
                "rec-start-key",
                CommandPayload::RecordStart {
                    recorder: wire_recorder(2),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            started.submission.output.result,
            CommandResult::Accepted { revision: 2, .. }
        ));
        assert_eq!(durable.project().recording_desired_slots(), [false, true]);
        journal.settle(&durable).unwrap();
    }
    let checkpointed = ProjectStore::new(&project_path).unwrap();
    let final_state = checkpointed.load().unwrap();
    assert_eq!(final_state.position().revision, 2);
    assert_eq!(
        checkpointed.scan_journal().unwrap().batches().len(),
        0,
        "checkpoint must have compacted the journal"
    );
    assert_eq!(
        final_state.project().recording_desired_slots(),
        [false, true]
    );
    let restored = restore_engine(&final_state).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    let desired = restored.show().desired_switcher();
    assert_eq!(
        (
            desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[0])),
            desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[1])),
        ),
        (false, true)
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_pair_ledger_advances_only_on_successful_enqueue() {
    let mut ledger = StreamPairLedger {
        next_sequence: 7,
        ..StreamPairLedger::default()
    };

    ledger.note_admitted(SequenceNumber::new(9));
    assert_eq!(ledger.enqueued_pairs, 1);
    assert_eq!(ledger.next_sequence, 10);

    // Refusals only count; they never move the admitted cursor.
    ledger.note_dropped();
    assert_eq!(ledger.dropped_pairs, 1);
    assert_eq!(ledger.next_sequence, 10);

    let mut saturated = StreamPairLedger {
        next_sequence: u64::MAX,
        enqueued_pairs: u64::MAX,
        dropped_pairs: u64::MAX,
    };
    saturated.note_admitted(SequenceNumber::new(u64::MAX));
    saturated.note_dropped();
    assert_eq!(saturated.next_sequence, u64::MAX);
    assert_eq!(saturated.enqueued_pairs, u64::MAX);
    assert_eq!(saturated.dropped_pairs, u64::MAX);
}

#[cfg(feature = "native-media")]
const MOCK_RENDITION: RenditionId = RenditionId::new(std::num::NonZeroU32::new(1).unwrap());

#[cfg(feature = "native-media")]
fn stream_runtime(
    target: u128,
    ledger: StreamPairLedger,
    congested: bool,
    first_failure: Option<OutputFailure>,
    started: bool,
    state: Option<DestinationState>,
    network: Option<NetworkTelemetry>,
) -> StreamRuntimeSnapshot {
    StreamRuntimeSnapshot {
        target: SwitcherStreamTargetId::from_non_zero(
            NonZeroU128::new(target).expect("test target is nonzero"),
        ),
        ledger,
        congested,
        first_failure,
        started,
        state,
        network,
        local: None,
        uptime_ms: 0,
        bitrate_kbps: 0,
    }
}

#[cfg(feature = "native-media")]
fn local_stream_runtime(target: u128, local: StreamLocalStatus) -> StreamRuntimeSnapshot {
    StreamRuntimeSnapshot {
        target: SwitcherStreamTargetId::from_non_zero(
            NonZeroU128::new(target).expect("test target is nonzero"),
        ),
        ledger: StreamPairLedger::default(),
        congested: false,
        first_failure: None,
        started: true,
        state: Some(DestinationState::Live),
        network: None,
        local: Some(local),
        uptime_ms: 0,
        bitrate_kbps: 0,
    }
}

#[cfg(feature = "native-media")]
#[test]
fn stream_status_maps_each_reachable_output_set_state() {
    let sample = |congested, state, network| {
        stream_status_sample(stream_runtime(
            7,
            StreamPairLedger {
                enqueued_pairs: 30,
                dropped_pairs: 4,
                ..StreamPairLedger::default()
            },
            congested,
            None,
            true,
            state,
            network,
        ))
        .expect("started runtimes always sample")
    };

    // Connecting and awaiting random access both mean "starting"; the
    // reconnected route is already connected, just not yet resuming.
    let connecting = sample(false, Some(DestinationState::Connecting), None);
    assert_eq!(connecting.realized, StreamRealizedState::Starting);
    assert!(!connecting.connected);
    let awaiting = sample(false, Some(DestinationState::AwaitingRandomAccess), None);
    assert_eq!(awaiting.realized, StreamRealizedState::Starting);
    assert!(awaiting.connected);

    // Producing: counters come from the ledger and bytes from telemetry.
    let live_set = driven_live_output_set();
    let id = DestinationId::new(1).unwrap();
    let network = live_set.telemetry(id).unwrap().clone();
    let live = sample(false, Some(DestinationState::Live), Some(network.clone()));
    assert_eq!(live.realized, StreamRealizedState::Live);
    assert!(live.connected);
    assert_eq!(
        live.muxed_bytes,
        network.bytes_sent(),
        "muxed bytes are the transport's own counter"
    );
    assert_eq!(live.enqueued_pairs, 30);
    assert_eq!(live.dropped_pairs, 4);
    assert_eq!(live.failure, None);

    // Backpressure observed this interval wins over Live.
    let congested = sample(true, Some(DestinationState::Live), Some(network));
    assert_eq!(congested.realized, StreamRealizedState::Congested);
    // The set's own congestion is congestion on the wire too.
    let transport_congested = sample(
        false,
        Some(DestinationState::Congested),
        Some(NetworkTelemetry::default()),
    );
    assert_eq!(transport_congested.realized, StreamRealizedState::Congested);

    // Waiting to reconnect is now a reachable wire state.
    let waiting = sample(
        false,
        Some(DestinationState::WaitingToReconnect {
            attempt: 2,
            retry_at_ms: 900,
        }),
        None,
    );
    assert_eq!(waiting.realized, StreamRealizedState::WaitingToReconnect);

    // Terminal states.
    let failed_by_state = sample(false, Some(DestinationState::Failed), None);
    assert_eq!(failed_by_state.realized, StreamRealizedState::Failed);
    let failing_set = driven_failing_output_set();
    let failing_network = failing_set.telemetry(id).unwrap().clone();
    let waiting_with_failure = sample(false, failing_set.state(id), Some(failing_network));
    assert_eq!(
        waiting_with_failure.realized,
        StreamRealizedState::WaitingToReconnect,
        "a recorded transient failure does not fail the target"
    );
    assert_eq!(
        waiting_with_failure.failure,
        Some(OutputFailure::new(OutputFailureCode::StreamStalled)),
        "transport failures are machine-readable codes"
    );
    let stopped = sample(false, Some(DestinationState::Stopped), None);
    assert_eq!(stopped.realized, StreamRealizedState::Stopped);
}

#[cfg(feature = "native-media")]
#[test]
fn latched_failure_is_sticky_over_live_state_and_survives_a_gone_slot() {
    let latched = Some(OutputFailure::new(OutputFailureCode::InternalError));
    // The route still streams, but the feed already latched its failure.
    let sample = stream_status_sample(stream_runtime(
        9,
        StreamPairLedger {
            enqueued_pairs: 2,
            ..StreamPairLedger::default()
        },
        false,
        latched,
        true,
        Some(DestinationState::Live),
        None,
    ))
    .expect("latched runtimes stay on the wire");
    assert_eq!(sample.realized, StreamRealizedState::Failed);
    assert_eq!(sample.failure, latched);

    // A start-time failure leaves nothing running; counters still report.
    let start_failed = stream_status_sample(stream_runtime(
        11,
        StreamPairLedger {
            dropped_pairs: 1,
            ..StreamPairLedger::default()
        },
        false,
        Some(OutputFailure::new(OutputFailureCode::FfmpegUnavailable)),
        true,
        Some(DestinationState::Stopped),
        None,
    ))
    .expect("latched runtimes stay on the wire");
    assert_eq!(start_failed.realized, StreamRealizedState::Failed);
    assert!(!start_failed.connected);
    assert_eq!(start_failed.muxed_bytes, 0);
    assert_eq!(start_failed.dropped_pairs, 1);
    assert_eq!(
        start_failed.failure,
        Some(OutputFailure::new(OutputFailureCode::FfmpegUnavailable))
    );
}

#[cfg(feature = "native-media")]
#[test]
fn inactive_targets_are_omitted_and_samples_sort_and_stay_bounded() {
    // A target that was never asked to run never appears.
    let empty = stream_status_samples([stream_runtime(
        5,
        StreamPairLedger::default(),
        false,
        None,
        false,
        Some(DestinationState::Stopped),
        None,
    )]);
    assert!(empty.is_empty(), "an all-inactive set publishes nothing");

    // Mixed set: unsorted input comes out in strictly ascending wire
    // order and inactive targets drop out of the middle.
    let latched =
        Some(OutputFailure::new(OutputFailureCode::FfmpegExited).with_exit_status(Some(1)));
    let mixed = stream_status_samples([
        stream_runtime(
            90,
            StreamPairLedger::default(),
            false,
            None,
            false,
            Some(DestinationState::Stopped),
            None,
        ),
        stream_runtime(
            40,
            StreamPairLedger {
                enqueued_pairs: 1,
                ..StreamPairLedger::default()
            },
            false,
            None,
            true,
            Some(DestinationState::Live),
            None,
        ),
        stream_runtime(
            20,
            StreamPairLedger::default(),
            false,
            latched,
            true,
            Some(DestinationState::Stopped),
            None,
        ),
        stream_runtime(
            10,
            StreamPairLedger::default(),
            false,
            None,
            false,
            None,
            None,
        ),
    ]);
    let targets: Vec<_> = mixed
        .iter()
        .map(|sample| sample.target.get().get())
        .collect();
    assert_eq!(targets, [20, 40]);

    // The bound holds even if a project ever exceeded the engine cap.
    let flooded: Vec<_> = (0..=u128::try_from(MAX_STREAM_SAMPLES).unwrap())
        .map(|index| {
            stream_runtime(
                index + 1,
                StreamPairLedger::default(),
                false,
                latched,
                true,
                Some(DestinationState::Stopped),
                None,
            )
        })
        .collect();
    assert_eq!(flooded.len(), MAX_STREAM_SAMPLES + 1);
    assert_eq!(stream_status_samples(flooded).len(), MAX_STREAM_SAMPLES);
}

/// One hand-written single-rendition media playlist with `segments` whole
/// two-second segments.
#[cfg(feature = "native-media")]
fn media_playlist(segments: u32) -> String {
    use core::fmt::Write as _;
    let mut playlist = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for index in 0..segments {
        let _ = write!(playlist, "#EXTINF:2.000000,\nsegment-{index:05}.ts\n");
    }
    playlist
}

/// One local HLS destination config over an absolute probe directory.
#[cfg(feature = "native-media")]
fn hls_destination(id: u8, directory: &str) -> DestinationConfig {
    DestinationConfig::new(
        DestinationId::new(id).unwrap(),
        OutputProtocol::Hls,
        Endpoint::local_directory(directory).unwrap(),
        None,
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::new(
            PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MULTIPLIER,
            Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_reads_a_single_rendition_playlist() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("index.m3u8"), media_playlist(3)).unwrap();
    let status = local_hls_status(directory.path()).expect("a valid playlist samples");
    assert_eq!(status.segments, 3);
    assert_eq!(status.target_duration_ms, 2_000);
    // Free space is measured on the directory itself, through a pinned
    // query: concurrent tests write to the same file system, so two
    // real queries need not agree.
    let mut queried = None;
    let pinned = local_hls_status_with(directory.path(), |ancestor| {
        queried = Some(ancestor.to_path_buf());
        Some(123_456_789)
    })
    .expect("a valid playlist samples");
    assert_eq!(queried.as_deref(), Some(directory.path()));
    assert_eq!(pinned.free_bytes, 123_456_789);
    let unavailable =
        local_hls_status_with(directory.path(), |_| None).expect("a valid playlist samples");
    assert_eq!(unavailable.free_bytes, 0, "an unavailable query is zero");
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_follows_the_first_master_variant() {
    use fm_frame::VideoDimensions;
    use fm_io_network::{HlsAbrCoordinator, HlsPlaylistType, HlsVariantMetadata};

    let directory = tempfile::tempdir().unwrap();
    let low = test_rendition(1);
    let high = test_rendition(2);
    let coordinator = HlsAbrCoordinator::new(
        vec![
            HlsVariantMetadata::new(
                low,
                "low",
                1_000_000,
                VideoDimensions::new(640, 360).unwrap(),
                "avc1.64001e,mp4a.40.2",
                "0/index.m3u8",
            )
            .unwrap(),
            HlsVariantMetadata::new(
                high,
                "high",
                2_000_000,
                VideoDimensions::new(1_280, 720).unwrap(),
                "avc1.64002a,mp4a.40.2",
                "1/index.m3u8",
            )
            .unwrap(),
        ],
        6,
        NormalizedDuration::from_nanos(2_000_000_000).unwrap(),
        0,
        6,
        HlsPlaylistType::Live,
    )
    .unwrap();
    fs::write(
        directory.path().join("index.m3u8"),
        coordinator.render_master_m3u8(),
    )
    .unwrap();

    // The first variant renders through the real playlist renderer; the
    // second carries different values to prove only the first is followed.
    let first = HlsPlaylistMetadata::parse_m3u8(&media_playlist(2), low, 1).unwrap();
    fs::create_dir(directory.path().join("0")).unwrap();
    fs::write(directory.path().join("0/index.m3u8"), first.render_m3u8()).unwrap();
    let second = HlsPlaylistMetadata::parse_m3u8(&media_playlist(5), high, 1).unwrap();
    fs::create_dir(directory.path().join("1")).unwrap();
    fs::write(directory.path().join("1/index.m3u8"), second.render_m3u8()).unwrap();

    let status = local_hls_status(directory.path()).expect("the master's first variant samples");
    assert_eq!(status.segments, 2);
    assert_eq!(status.target_duration_ms, 2_000);
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_is_none_for_missing_or_invalid_playlists() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "a missing index.m3u8 is a sample gap"
    );

    fs::write(directory.path().join("index.m3u8"), "not a playlist\n").unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "unparseable text is a sample gap"
    );

    // A segment longer than the target duration fails the bounded parser.
    fs::write(
        directory.path().join("index.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:2.000000,\nsegment-00000.ts\n",
    )
    .unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "an invalid playlist is a sample gap"
    );

    // A master whose first variant playlist is missing is also a gap.
    fs::write(
        directory.path().join("index.m3u8"),
        concat!(
            "#EXTM3U\n#EXT-X-VERSION:6\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n",
            "0/index.m3u8\n",
        ),
    )
    .unwrap();
    assert_eq!(
        local_hls_status(directory.path()),
        None,
        "a missing variant is a sample gap"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn local_hls_status_rejects_escaping_master_variants() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("hls");
    fs::create_dir(&directory).unwrap();
    // A valid playlist outside the output directory proves the escaping
    // path is refused before any read, not merely because it is missing.
    fs::write(root.path().join("escape"), media_playlist(4)).unwrap();
    for variant in ["../escape", "/absolute/index.m3u8", "0\\index.m3u8"] {
        fs::write(
            directory.join("index.m3u8"),
            format!(
                "#EXTM3U\n#EXT-X-VERSION:6\n\
                     #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n{variant}\n"
            ),
        )
        .unwrap();
        assert_eq!(local_hls_status(&directory), None, "{variant}");
    }
}

#[cfg(feature = "native-media")]
#[test]
fn cached_local_health_is_projected_only_for_hls_targets() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(hls_destination(2, "/srv/hls-probe"))
        .unwrap();
    let local = StreamLocalStatus {
        segments: 6,
        target_duration_ms: 6_000,
        free_bytes: 1_024,
    };
    let mut cache = [None; fm_switcher::MAX_STREAM_COUNT];
    cache[0] = Some(local);
    cache[1] = Some(local);
    assert_eq!(
        cached_local_hls_status(&set, &cache, DestinationId::new(1).unwrap()),
        None,
        "a network target never projects local health"
    );
    assert_eq!(
        cached_local_hls_status(&set, &cache, DestinationId::new(2).unwrap()),
        Some(local)
    );

    // The mapping carries the filtered cache entry onto the wire sample.
    let samples = stream_status_samples([
        stream_runtime(
            3,
            StreamPairLedger::default(),
            false,
            None,
            true,
            Some(DestinationState::Live),
            None,
        ),
        local_stream_runtime(4, local),
    ]);
    assert_eq!(samples[0].local, None);
    assert_eq!(samples[1].local, Some(local));
}

/// One scripted [`fm_io_network::TransportSink`] against the same public
/// trait the real `FFmpeg` transport implements. Hermetic tests drive the
/// daemon's actual output-set wiring through it: scripted connect and
/// write results, disconnect counts, and which host each attempt chose.
#[cfg(feature = "native-media")]
#[derive(Default)]
struct MockTransportSink {
    connect_results: std::collections::VecDeque<
        Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError>,
    >,
    write_results:
        std::collections::VecDeque<Result<fm_io_network::SinkWrite, fm_io_network::SinkError>>,
    hosts: Vec<String>,
    disconnects: usize,
    /// Every packet the route attempted to write, cloned on arrival, so
    /// rendition-sharing tests can compare payloads across sinks.
    written: Vec<fm_io_network::OutputPacket>,
}

#[cfg(feature = "native-media")]
impl fm_io_network::TransportSink for MockTransportSink {
    fn connect(
        &mut self,
        _config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError> {
        self.hosts.push(endpoint.host().to_owned());
        self.connect_results
            .pop_front()
            .unwrap_or_else(|| Ok(fm_io_network::ConnectionObservation::default()))
    }

    fn write(
        &mut self,
        packet: &fm_io_network::OutputPacket,
    ) -> Result<fm_io_network::SinkWrite, fm_io_network::SinkError> {
        self.written.push(packet.clone());
        self.write_results.pop_front().unwrap_or_else(|| {
            Ok(fm_io_network::SinkWrite::Sent(
                fm_io_network::SendObservation::default(),
            ))
        })
    }

    fn disconnect(&mut self) {
        self.disconnects += 1;
    }
}

#[cfg(feature = "native-media")]
fn mock_destination(id: u8) -> DestinationConfig {
    DestinationConfig::new(
        DestinationId::new(id).unwrap(),
        OutputProtocol::Rtmp,
        Endpoint::new("primary.example.test", 19_35, "/live").unwrap(),
        Some(Endpoint::new("backup.example.test", 19_35, "/live").unwrap()),
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::new(
            PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
            PROGRAM_STREAM_RECONNECT_MULTIPLIER,
            Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn mock_packet(sequence: u64) -> fm_io_network::OutputPacket {
    mock_packet_for(MOCK_RENDITION, sequence, 7)
}

#[cfg(feature = "native-media")]
fn mock_packet_for(
    rendition: RenditionId,
    sequence: u64,
    payload_byte: u8,
) -> fm_io_network::OutputPacket {
    use fm_frame::{NormalizedDuration, NormalizedTimestamp};
    fm_io_network::OutputPacket::new(
        rendition,
        sequence,
        NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 33_333_333),
        NormalizedDuration::from_nanos(33_333_333).unwrap(),
        true,
        vec![payload_byte; 16],
    )
    .unwrap()
}

/// One rendition group's planner state over hermetic project-format
/// settings, mirroring what `NativeProgramStream::compile` builds.
#[cfg(feature = "native-media")]
fn test_group(
    rendition_value: u32,
    members: &[u8],
    video_bitrate_kbps: u32,
    group_index: usize,
    group_count: usize,
) -> StreamRenditionCore {
    StreamRenditionCore {
        output: fm_types::OutputId::new(NonZeroU128::new(1).expect("test output is nonzero")),
        members: members
            .iter()
            .map(|id| DestinationId::new(*id).expect("test member fits a slot"))
            .collect(),
        rendition: RenditionId::new(
            NonZeroU32::new(rendition_value).expect("test rendition is nonzero"),
        ),
        variants: pinned_rendition_variants(
            &test_stream_format(),
            video_bitrate_kbps,
            group_index,
            group_count,
        )
        .expect("test rendition variants validate"),
        next_sequence: 0,
    }
}

/// Drives one group through the exact production frame pipeline — gate,
/// plan, pack, fan out, admit — with the pack step scripted.
#[cfg(feature = "native-media")]
fn run_test_group_frame(
    core: &mut StreamRenditionCore,
    output_set: &mut OutputSet,
    streams: &mut [(DestinationId, StreamPairLedger, bool)],
    sequence: SequenceNumber,
    active: &[bool],
    pack: impl FnOnce() -> Result<fm_io_network::OutputPacket, String>,
) -> GroupFrameOutcome {
    let outcome = plan_and_pack_group_frame(
        &core.members,
        &core.variants,
        &mut core.next_sequence,
        sequence,
        active,
        pack,
    );
    if let GroupFrameOutcome::Packed { plan, packet } = outcome {
        let outcomes = output_set
            .enqueue_rendition(&plan, &packet)
            .expect("the packed rendition is always planned");
        let mut routes: Vec<(DestinationId, &mut StreamPairLedger, &mut bool)> = streams
            .iter_mut()
            .filter(|(destination, _, _)| core.members.contains(destination))
            .map(|(destination, ledger, backpressure)| (*destination, ledger, backpressure))
            .collect();
        admit_rendition_outcomes(&outcomes, sequence, &mut routes);
        return GroupFrameOutcome::Packed { plan, packet };
    }
    outcome
}

/// Drives one real output set to Live with one sent packet, so status
/// assertions read genuine telemetry instead of invented values.
#[cfg(feature = "native-media")]
fn driven_live_output_set() -> OutputSet {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(0)).unwrap();
    set.poll(id, 1, &mut sink).unwrap();
    set
}

/// Drives one real output set into a recorded transient write failure.
#[cfg(feature = "native-media")]
fn driven_failing_output_set() -> OutputSet {
    use fm_io_network::{FailureCause, FailureStage, SinkError};
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        write_results: std::collections::VecDeque::from([Err(SinkError::new(
            FailureStage::Write,
            None,
            "muxed media clock stalled",
            true,
        )
        .with_cause(FailureCause::Stalled))]),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(1)).unwrap();
    set.poll(id, 10, &mut sink).unwrap();
    set
}

#[cfg(feature = "native-media")]
#[test]
fn output_set_reaches_live_through_a_started_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();

    // Before start the slot is stopped; start arms it for connecting.
    assert_eq!(set.state(id), Some(DestinationState::Stopped));
    set.start(id).unwrap();
    assert_eq!(set.state(id), Some(DestinationState::Connecting));
    assert_eq!(
        set.poll(id, 0, &mut sink).unwrap(),
        PollEvent::Connected,
        "the scripted sink accepts the first connect"
    );
    assert_eq!(set.state(id), Some(DestinationState::Live));

    set.enqueue(id, mock_packet(0)).unwrap();
    assert_eq!(
        set.poll(id, 1, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 0 }
    );
    assert_eq!(set.telemetry(id).unwrap().packets_sent(), 1);
    assert_eq!(set.telemetry(id).unwrap().bytes_sent(), 16);
}

#[cfg(feature = "native-media")]
#[test]
fn scripted_write_failure_schedules_reconnection_on_the_backup_endpoint() {
    use fm_io_network::ConnectionTarget;
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        write_results: std::collections::VecDeque::from([Err(fm_io_network::SinkError::new(
            fm_io_network::FailureStage::Write,
            Some(104),
            "connection reset by peer",
            true,
        ))]),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(1)).unwrap();

    // The write failure costs one attempt and flips to the backup.
    let event = set.poll(id, 10, &mut sink).unwrap();
    let PollEvent::ReconnectScheduled { retry_at_ms } = event else {
        panic!("expected a scheduled reconnect, got {event:?}");
    };
    assert_eq!(retry_at_ms, 10 + PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS);
    assert_eq!(
        set.state(id),
        Some(DestinationState::WaitingToReconnect {
            attempt: 1,
            retry_at_ms
        })
    );
    assert_eq!(set.connection_target(id), Some(ConnectionTarget::Backup));

    // Backoff holds until the retry instant, then the backup endpoint is
    // contacted and the queued pair replays after it reopens.
    assert_eq!(
        set.poll(id, retry_at_ms - 1, &mut sink).unwrap(),
        PollEvent::WaitingToReconnect { retry_at_ms }
    );
    assert_eq!(
        set.poll(id, retry_at_ms, &mut sink).unwrap(),
        PollEvent::AwaitingRandomAccess {
            dropped_packets: 0,
            dropped_bytes: 0,
        },
        "a recovery replays from the next random-access packet"
    );
    assert_eq!(
        sink.hosts,
        ["primary.example.test", "backup.example.test"],
        "the first failure must move the destination to its backup"
    );
    // Connecting is not yet proof: the recovery counts and the retry
    // budget clears only once the queued pair is delivered.
    assert_eq!(set.telemetry(id).unwrap().reconnects(), 0);
    assert_eq!(
        set.poll(id, retry_at_ms, &mut sink).unwrap(),
        PollEvent::PacketSent { sequence: 1 }
    );
    assert_eq!(set.telemetry(id).unwrap().reconnects(), 1);
}

/// A target that latched a failure must come back when the operator stops
/// and starts it again, exactly like a recorder slot does.
#[cfg(feature = "native-media")]
#[test]
fn operator_stop_then_start_relaunches_a_failed_stream_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let destination_id = DestinationId::new(1).unwrap();
    let sink_config = RtmpSinkConfig::new(test_rendition(1), test_stream_format());
    let queue_capacity = sink_config.limits.max_outstanding_pairs;
    let mut stream = NativeProgramStream {
        target: SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(7).unwrap()),
        destination_id,
        redacted_destination: "rtmp://primary.example.test:1935/live".to_owned(),
        sink: stream_transport::BackgroundTransport::new(
            ProgramStreamSink::Ffmpeg(FfmpegRtmpSink::new(sink_config)),
            "freemix-native-stream-test".to_owned(),
        ),
        ledger: StreamPairLedger::default(),
        first_failure: None,
        first_failure_code: None,
        enqueue_backpressure: false,
        started: false,
        meter: StreamLinkMeter::default(),
        queue_capacity,
        authored: test_authored_target(7),
    };
    stream.start(&mut set);
    stream.fail("transport:budget_exhausted", &mut set);
    assert!(!route_active(&stream, &set));

    stream.stop_requested(&mut set);
    stream.start(&mut set);

    assert_eq!(
        set.state(destination_id),
        Some(DestinationState::Connecting)
    );
    assert!(route_active(&stream, &set));
}

/// One authored RTMP destination for runtime unit tests.
#[cfg(feature = "native-media")]
fn test_authored_target(id: u128) -> fm_model::StreamTarget {
    fm_model::StreamTarget::new(
        fm_model::StreamTargetId::new(NonZeroU128::new(id).unwrap()),
        format!("Destination {id}"),
        fm_model::StreamProtocol::Rtmp,
        fm_model::StreamEndpoint::parse_for(
            fm_model::StreamProtocol::Rtmp,
            "primary.example.test:1935/live",
        )
        .unwrap(),
        Some(fm_model::StreamKey::parse("live-key-0001").unwrap()),
        fm_types::OutputId::new(NonZeroU128::new(30).unwrap()),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn test_stream_format() -> RecordFormat {
    RecordFormat::new(
        16,
        16,
        FrameRate::new(25, 1).unwrap(),
        SampleRate::new(44_100).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn test_rendition(value: u32) -> RenditionId {
    RenditionId::new(NonZeroU32::new(value).expect("test rendition is nonzero"))
}

#[cfg(feature = "native-media")]
#[test]
fn queue_full_refusals_are_dropped_once_and_reported_as_congestion() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();

    let mut core = test_group(1, &[1], 4_500, 0, 1);
    let mut rows = vec![(id, StreamPairLedger::default(), false)];

    // Capacity is one: the first pair rides, the second is refused,
    // counted as a drop, flagged as backpressure, and never retried.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(0),
        &[true],
        || Ok(mock_packet(0)),
    );
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(1),
        &[true],
        || Ok(mock_packet(1)),
    );

    let (_, ledger, backpressure) = rows[0];
    assert!(backpressure);
    assert_eq!(ledger.enqueued_pairs, 1);
    assert_eq!(ledger.dropped_pairs, 1);
    assert_eq!(set.queue_depth(id), Some(1));
    assert_eq!(set.state(id), Some(DestinationState::Congested));
    let realized = realized_stream_state(false, backpressure, set.state(id));
    assert_eq!(realized, StreamRealizedState::Congested);

    // The group consumed one sequence per frame even though the second
    // pair was refused; the member's admitted cursor advanced only once.
    assert_eq!(core.next_sequence, 2);
    assert_eq!(ledger.next_sequence, 1);
}

#[cfg(feature = "native-media")]
#[test]
fn behind_cursor_frames_are_never_packed_or_fanned_out() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();

    let mut core = test_group(1, &[1], 4_500, 0, 1);
    core.next_sequence = 9;
    let mut rows = vec![(id, StreamPairLedger::default(), false)];

    // A frame behind the shared cursor costs nothing at all: no pack, no
    // fan-out, no counter movement.
    let outcome = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(8),
        &[true],
        || panic!("a consumed sequence must never be packed again"),
    );
    assert!(matches!(outcome, GroupFrameOutcome::Idle));
    assert_eq!(sink.written.len(), 0);
    assert_eq!(set.queue_depth(id), Some(0));
    assert_eq!(rows[0].1.enqueued_pairs, 0);
    assert_eq!(rows[0].1.dropped_pairs, 0);
    assert!(!rows[0].2);
    assert_eq!(core.next_sequence, 9, "an idle frame leaves the cursor");

    // The current sequence packs and queues normally.
    let packed = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(9),
        &[true],
        || Ok(mock_packet(9)),
    );
    assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
    assert_eq!(
        sink.written.len(),
        0,
        "the pair waits for the route to poll"
    );
    assert_eq!(set.queue_depth(id), Some(1));
    assert_eq!(core.next_sequence, 10);
    assert_eq!(rows[0].1.enqueued_pairs, 1);
}

#[cfg(feature = "native-media")]
#[test]
fn stop_disconnects_exactly_once_and_freezes_the_slot() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink::default();
    set.start(id).unwrap();
    set.poll(id, 0, &mut sink).unwrap();
    set.enqueue(id, mock_packet(0)).unwrap();

    set.stop(id, &mut sink).unwrap();
    assert_eq!(sink.disconnects, 1);
    assert_eq!(set.state(id), Some(DestinationState::Stopped));
    assert_eq!(set.queue_depth(id), Some(0));
    // Telemetry survives the stop so final records keep whole-run counts.
    assert_eq!(set.telemetry(id).unwrap().packets_accepted(), 1);
}

/// Two destinations sharing their protocol and authored bitrate form ONE
/// rendition group: the expensive pack step runs exactly once per frame,
/// both sinks receive byte-identical payloads, and each member's ledger
/// advances while the group consumes one sequence per frame.
#[cfg(feature = "native-media")]
#[test]
fn same_bitrate_destinations_share_one_pack_per_frame() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];
    let mut packs = Vec::new();

    for sequence in [41_u64, 42] {
        let packed = run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(sequence),
            &[true, true],
            || {
                packs.push(sequence);
                Ok(mock_packet_for(
                    MOCK_RENDITION,
                    sequence,
                    u8::try_from(sequence).unwrap_or(u8::MAX),
                ))
            },
        );
        assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
        // Mirrors advance_stream_routes: the shared set drains each route
        // after the frame's fan-out.
        let now_ms = sequence * 33;
        set.poll(id_a, now_ms, &mut sink_a).unwrap();
        set.poll(id_b, now_ms, &mut sink_b).unwrap();
    }

    // One pack per frame feeds the whole group.
    assert_eq!(packs, [41, 42]);
    for (sink, name) in [(&sink_a, "a"), (&sink_b, "b")] {
        assert_eq!(sink.written.len(), 2, "{name}");
        for (index, expected) in [41_u64, 42].into_iter().enumerate() {
            let packet = &sink.written[index];
            let expected_byte = u8::try_from(expected).unwrap_or(u8::MAX);
            assert_eq!(packet.sequence(), expected, "{name}");
            assert_eq!(packet.rendition(), MOCK_RENDITION, "{name}");
            assert!(
                packet.payload().iter().all(|&byte| byte == expected_byte),
                "{name} frame {expected} carries the scripted payload"
            );
        }
    }
    assert_eq!(
        sink_a
            .written
            .iter()
            .map(fm_io_network::OutputPacket::payload)
            .collect::<Vec<_>>(),
        sink_b
            .written
            .iter()
            .map(fm_io_network::OutputPacket::payload)
            .collect::<Vec<_>>(),
        "both members receive the very same packed pair"
    );

    // Both members' ledgers advanced per frame; nobody dropped anything.
    for (destination, ledger, backpressure) in &rows {
        assert_eq!(ledger.enqueued_pairs, 2, "{destination}");
        assert_eq!(ledger.dropped_pairs, 0, "{destination}");
        assert!(!backpressure, "{destination}");
        assert_eq!(ledger.next_sequence, 43, "{destination}");
    }
    assert_eq!(core.next_sequence, 43, "one sequence consumed per frame");
}

/// Distinct authored bitrates stay independent: two groups pack once each
/// per frame, every plan numbers its real rendition at the compile-time
/// group id (the inert sibling rungs exist purely to hold that position),
/// and neither sink ever sees the other group's payload.
#[cfg(feature = "native-media")]
#[test]
fn distinct_bitrates_pack_into_two_independent_groups() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    // Authored order: target A at 3000 kbps is group one, target B at
    // 6000 kbps is group two.
    let mut low = test_group(1, &[1], 3_000, 0, 2);
    let mut high = test_group(2, &[2], 6_000, 1, 2);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];
    let (mut low_packs, mut high_packs) = (0_usize, 0_usize);

    let low_outcome = run_test_group_frame(
        &mut low,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true],
        || {
            low_packs += 1;
            Ok(mock_packet_for(test_rendition(1), 7, 1))
        },
    );
    let high_outcome = run_test_group_frame(
        &mut high,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true],
        || {
            high_packs += 1;
            Ok(mock_packet_for(test_rendition(2), 7, 2))
        },
    );
    assert_eq!(
        (low_packs, high_packs),
        (1, 1),
        "each group packs its own pair"
    );
    // Mirrors advance_stream_routes after the frame's fan-out.
    set.poll(id_a, 7, &mut sink_a).unwrap();
    set.poll(id_b, 7, &mut sink_b).unwrap();

    let GroupFrameOutcome::Packed { plan: low_plan, .. } = low_outcome else {
        panic!("the low group packed");
    };
    assert_eq!(
        low_plan.destinations_for(test_rendition(1)),
        Some([id_a].as_slice())
    );
    let GroupFrameOutcome::Packed {
        plan: high_plan, ..
    } = high_outcome
    else {
        panic!("the high group packed");
    };
    assert_eq!(
        high_plan.destinations_for(test_rendition(2)),
        Some([id_b].as_slice()),
        "the inert sibling rung pins the real rendition at the compile-time id"
    );

    // Every sink saw exactly its own group's payload and rendition.
    assert_eq!(sink_a.written.len(), 1);
    assert_eq!(sink_a.written[0].rendition(), test_rendition(1));
    assert!(sink_a.written[0].payload().iter().all(|&byte| byte == 1));
    assert_eq!(sink_b.written.len(), 1);
    assert_eq!(sink_b.written[0].rendition(), test_rendition(2));
    assert!(sink_b.written[0].payload().iter().all(|&byte| byte == 2));

    assert_eq!(rows[0].1.enqueued_pairs, 1);
    assert_eq!(rows[1].1.enqueued_pairs, 1);
    assert!(!rows[0].2 && !rows[1].2);
}

/// One member stopping mid-show leaves the plan, keeps its ledger frozen,
/// and never sees another packet; restarting it resumes under the same
/// group identity with the next deliverable sequences while the surviving
/// member never interrupted.
#[cfg(feature = "native-media")]
#[test]
fn stopped_member_is_skipped_and_restart_resumes_the_group() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];

    // Frame five reaches both members.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(5),
        &[true, true],
        || Ok(mock_packet(5)),
    );
    set.poll(id_a, 5, &mut sink_a).unwrap();
    set.poll(id_b, 5, &mut sink_b).unwrap();
    assert_eq!(sink_a.written.len(), 1);
    assert_eq!(sink_b.written.len(), 1);

    // Target B stops mid-show: it leaves the plan entirely, its ledger
    // freezes, and A keeps streaming without interruption.
    set.stop(id_b, &mut sink_b).unwrap();
    let packed = run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(6),
        &[true, false],
        || Ok(mock_packet(6)),
    );
    let GroupFrameOutcome::Packed { plan, .. } = packed else {
        panic!("the surviving member keeps the group packing");
    };
    assert_eq!(
        plan.destinations_for(MOCK_RENDITION),
        Some([id_a].as_slice()),
        "the plan covers exactly the still-active members"
    );
    set.poll(id_a, 6, &mut sink_a).unwrap();
    assert_eq!(sink_a.written.len(), 2);
    assert_eq!(sink_b.written.len(), 1, "a stopped route receives nothing");
    assert_eq!(rows[0].1.enqueued_pairs, 2);
    assert_eq!(rows[1].1.enqueued_pairs, 1, "the stopped ledger freezes");
    assert_eq!(rows[1].1.dropped_pairs, 0);
    assert!(!rows[1].2);

    // Restarting B resumes under the same group identity with the next
    // deliverable sequences; the sink pads the gap frame six skipped.
    set.start(id_b).unwrap();
    set.poll(id_b, 10, &mut sink_b).unwrap();
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(7),
        &[true, true],
        || Ok(mock_packet(7)),
    );
    set.poll(id_a, 7, &mut sink_a).unwrap();
    set.poll(id_b, 7, &mut sink_b).unwrap();
    assert_eq!(sink_b.written.len(), 2);
    assert_eq!(sink_b.written.last().unwrap().sequence(), 7);
    assert_eq!(rows[1].1.enqueued_pairs, 2);
    assert_eq!(rows[1].1.next_sequence, 8);
    assert_eq!(core.next_sequence, 8);
}

/// Backpressure on one member's route flags and congests only that
/// target; the other member of the same group streams clean.
#[cfg(feature = "native-media")]
#[test]
fn a_congested_route_flags_only_that_target() {
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    set.add_destination(mock_destination(2)).unwrap();
    let id_a = DestinationId::new(1).unwrap();
    let id_b = DestinationId::new(2).unwrap();
    let mut sink_a = MockTransportSink::default();
    let mut sink_b = MockTransportSink::default();
    set.start(id_a).unwrap();
    set.start(id_b).unwrap();
    set.poll(id_a, 0, &mut sink_a).unwrap();
    set.poll(id_b, 0, &mut sink_b).unwrap();

    let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
    let mut rows = vec![
        (id_a, StreamPairLedger::default(), false),
        (id_b, StreamPairLedger::default(), false),
    ];

    // Frame zero fills both single-slot queues.
    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(0),
        &[true, true],
        || Ok(mock_packet(0)),
    );

    // Only B drains between frames; A stays saturated.
    set.poll(id_b, 1, &mut sink_b).unwrap();

    run_test_group_frame(
        &mut core,
        &mut set,
        &mut rows,
        SequenceNumber::new(1),
        &[true, true],
        || Ok(mock_packet(1)),
    );

    let (_, ledger_a, flag_a) = rows[0];
    let (_, ledger_b, flag_b) = rows[1];
    assert!(flag_a);
    assert!(!flag_b, "only the saturated route flags backpressure");
    assert_eq!(ledger_a.enqueued_pairs, 1);
    assert_eq!(ledger_a.dropped_pairs, 1);
    assert_eq!(ledger_b.enqueued_pairs, 2);
    assert_eq!(ledger_b.dropped_pairs, 0);

    // Congestion is per-target on the wire too.
    assert_eq!(set.state(id_a), Some(DestinationState::Congested));
    assert_eq!(
        realized_stream_state(false, flag_a, set.state(id_a)),
        StreamRealizedState::Congested
    );
    assert_eq!(
        realized_stream_state(false, flag_b, set.state(id_b)),
        StreamRealizedState::Live
    );
}

#[cfg(feature = "native-media")]
#[test]
fn exhausted_retry_budget_latches_the_target_failed() {
    use fm_io_network::{FailureStage, SinkError};
    let mut set = OutputSet::new();
    set.add_destination(mock_destination(1)).unwrap();
    let id = DestinationId::new(1).unwrap();
    let mut sink = MockTransportSink {
        connect_results: (0..=PROGRAM_STREAM_RECONNECT_ATTEMPTS)
            .map(|_| {
                Err(SinkError::new(
                    FailureStage::Connect,
                    Some(111),
                    "connection refused",
                    true,
                ))
            })
            .collect(),
        ..MockTransportSink::default()
    };
    set.start(id).unwrap();
    let mut now_ms = 0_u64;
    loop {
        if set.poll(id, now_ms, &mut sink).unwrap() == PollEvent::Failed {
            break;
        }
        now_ms += PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS;
    }
    assert_eq!(set.state(id), Some(DestinationState::Failed));
    let telemetry = set.telemetry(id).unwrap();
    assert_eq!(
        telemetry.connect_attempts(),
        u64::from(PROGRAM_STREAM_RECONNECT_ATTEMPTS) + 1,
        "the initial attempt plus every budgeted retry ran"
    );
    assert!(
        telemetry.latest_failure().unwrap().retryable,
        "refused connections stay transient; only the budget ends them"
    );
}

#[cfg(feature = "native-media")]
#[test]
#[allow(clippy::too_many_lines)] // one case per protocol convention
fn split_destination_url_round_trips_every_protocol_convention() {
    // (authored URL, protocol, host, port, endpoint path, key tail). The
    // key is restated here because the split result deliberately cannot
    // be read back in the clear.
    for (url, protocol, host, port, path, key) in [
        (
            "rtmp://ingest.example.test:1935/live_app/live-key-0001",
            OutputProtocol::Rtmp,
            "ingest.example.test",
            1_935_u16,
            "/live_app",
            Some("live-key-0001"),
        ),
        (
            "rtmps://ingest.example.test:1936/a/b/key12345?token=abcdef",
            OutputProtocol::Rtmps,
            "ingest.example.test",
            1_936,
            "/a/b",
            Some("key12345?token=abcdef"),
        ),
        (
            "srt://ingest.example.test:9710?streamid=live-key-0001",
            OutputProtocol::Srt,
            "ingest.example.test",
            9_710,
            "/",
            Some("live-key-0001"),
        ),
        (
            "srt://ingest.example.test:9710",
            OutputProtocol::Srt,
            "ingest.example.test",
            9_710,
            "/",
            None,
        ),
        (
            "omt://0.0.0.0:6500",
            OutputProtocol::Omt,
            "0.0.0.0",
            6_500,
            "/",
            None,
        ),
        (
            "hls:///var/lib/freemix/live",
            OutputProtocol::Hls,
            // A local destination has no peer: the placeholder host and
            // port are never rendered, and the directory is the path.
            "localhost",
            1,
            "/var/lib/freemix/live",
            None,
        ),
    ] {
        let (split_protocol, endpoint, split_key) = split_destination_url(url).expect(url);
        assert_eq!(split_protocol, protocol, "{url:?}");
        assert_eq!(endpoint.host(), host);
        assert_eq!(u16::from(endpoint.port()), port);
        assert_eq!(endpoint.path(), path);
        assert_eq!(
            split_key.is_some(),
            key.is_some(),
            "{url:?} keeps its key presence"
        );

        // The convention round-trips: composing the pieces back together
        // under the transport's rules yields a URL the codec parser — the
        // single validity source — accepts again.
        let composed = match (protocol, key) {
            (OutputProtocol::Rtmp, Some(key)) => {
                format!("rtmp://{host}:{port}{path}/{key}")
            }
            (OutputProtocol::Rtmps, Some(key)) => {
                format!("rtmps://{host}:{port}{path}/{key}")
            }
            (OutputProtocol::Srt, Some(key)) => format!("srt://{host}:{port}?streamid={key}"),
            (OutputProtocol::Srt, None) => format!("srt://{host}:{port}"),
            (OutputProtocol::Hls, None) => format!("hls://{path}"),
            (OutputProtocol::Hls | OutputProtocol::Omt, Some(_)) => {
                unreachable!("local OMT and HLS destinations carry no stream key")
            }
            (OutputProtocol::Omt, None) => format!("omt://{host}:{port}"),
            (OutputProtocol::LiveLan, _) => {
                unreachable!("no daemon streaming transport for LAN outputs")
            }
            (_, None) => unreachable!("network destinations always carry a key here"),
        };
        if protocol != OutputProtocol::Omt {
            fm_codec_ffmpeg::stream::StreamDestination::parse(&composed)
                .unwrap_or_else(|error| panic!("{composed:?} does not re-parse: {error:?}"));
        }
    }

    // Shapes that cannot be represented fail compilation instead of being
    // guessed at air time.
    for url in [
        "rtmp://host.example.test/app/key12345",
        "rtmp://host.example.test:1935",
        "srt://host.example.test?param=key12345",
        "http://host.example.test:1935/app/key12345",
        "srt://host.example.test:70000?streamid=key12345",
        "hls://relative/dir",
        "hls:///srv/../etc",
        "hls:///srv//live",
        "hls:///srv/live/",
        "hls://",
    ] {
        assert!(
            split_destination_url(url).is_err(),
            "{url:?} must be refused"
        );
    }
}

/// An SRT stream id is one `?streamid=` query value, so query separators
/// in it are refused with the typed reason and never echoed; the same key
/// text stays legal as an RTMP path segment.
#[cfg(feature = "native-media")]
#[test]
fn split_destination_url_validates_the_key_for_its_protocol() {
    for url in [
        "srt://ingest.example.test:9710?streamid=live-key&latency=1",
        "srt://ingest.example.test:9710?streamid=live-key?0001",
    ] {
        let Err(error) = split_destination_url(url) else {
            panic!("{url:?} must be refused");
        };
        let message = error.to_string();
        assert!(
            message.contains("stream destination key was refused")
                && message.contains("SRT stream id"),
            "{message}"
        );
        assert!(!message.contains("live-key"), "{message}");
    }
    let (_, _, key) =
        split_destination_url("rtmp://ingest.example.test:1935/live/live-key&x=1").unwrap();
    assert!(key.is_some());
    // A key no protocol can carry keeps its own reason.
    let error = split_destination_url("srt://ingest.example.test:9710?streamid=abc")
        .unwrap_err()
        .to_string();
    assert!(error.contains("redactable"), "{error}");
}

#[cfg(feature = "native-media")]
#[test]
fn redacted_destination_url_masks_keys_and_renders_hls_paths_verbatim() {
    let network = Endpoint::new("ingest.example.test", 1_935, "/live_app").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmp, &network, true),
        "rtmp://ingest.example.test:1935/live_app/****"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmp, &network, false),
        "rtmp://ingest.example.test:1935/live_app"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Rtmps, &network, true),
        "rtmps://ingest.example.test:1935/live_app/****"
    );
    let srt = Endpoint::new("ingest.example.test", 9_710, "/").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Srt, &srt, true),
        "srt://ingest.example.test:9710?streamid=****"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Srt, &srt, false),
        "srt://ingest.example.test:9710"
    );
    // A local HLS directory is operator-chosen storage, not a secret: it
    // renders verbatim no matter what key presence the caller reports.
    let hls = Endpoint::local_directory("/var/lib/freemix/live").unwrap();
    assert_eq!(
        redacted_destination_url(OutputProtocol::Hls, &hls, false),
        "hls:///var/lib/freemix/live"
    );
    assert_eq!(
        redacted_destination_url(OutputProtocol::Hls, &hls, true),
        "hls:///var/lib/freemix/live"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn pending_network_probe_skips_hls_and_maps_network_endpoints() {
    let rtmp = DestinationConfig::new(
        DestinationId::new(1).unwrap(),
        OutputProtocol::Rtmp,
        Endpoint::new("ingest.example.test", 1_935, "/live").unwrap(),
        Some(Endpoint::new("backup.example.test", 1_935, "/live").unwrap()),
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::disabled(),
    )
    .unwrap();

    let primary = pending_network_probe(&rtmp, 7, preflight::PRIMARY_ENDPOINT)
        .expect("an RTMP destination has a primary endpoint to probe");
    assert_eq!(primary.target, 7);
    assert_eq!(primary.endpoint, preflight::PRIMARY_ENDPOINT);
    assert_eq!(primary.host, "ingest.example.test");
    assert_eq!(u16::from(primary.port), 1_935);
    assert_eq!(primary.timeout, preflight::NETWORK_PREFLIGHT_TIMEOUT);

    let backup = pending_network_probe(&rtmp, 7, preflight::BACKUP_ENDPOINT)
        .expect("a configured backup endpoint is probed too");
    assert_eq!(backup.target, 7);
    assert_eq!(backup.endpoint, preflight::BACKUP_ENDPOINT);
    assert_eq!(backup.host, "backup.example.test");
    assert_eq!(u16::from(backup.port), 1_935);

    // A local HLS destination is a directory write, not a peer, so it has
    // no network endpoint to probe on either slot.
    let hls = DestinationConfig::new(
        DestinationId::new(2).unwrap(),
        OutputProtocol::Hls,
        Endpoint::local_directory("/var/lib/freemix/live").unwrap(),
        None,
        None,
        None,
        QueueCapacity::new(1).unwrap(),
        ReconnectPolicy::disabled(),
    )
    .unwrap();
    assert_eq!(
        pending_network_probe(&hls, 8, preflight::PRIMARY_ENDPOINT),
        None
    );
    assert_eq!(
        pending_network_probe(&hls, 8, preflight::BACKUP_ENDPOINT),
        None
    );
}

#[cfg(feature = "native-media")]
#[test]
fn split_and_group_targets_gives_hls_its_own_rendition_group() {
    use fm_model::{
        DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
        StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output = ModelOutputId::new(NonZeroU128::new(30).unwrap());
    let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
        let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
        let key = match protocol {
            StreamProtocol::Hls | StreamProtocol::Omt => None,
            StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                Some(ModelStreamKey::parse("live-key-0001").unwrap())
            }
        };
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            protocol,
            endpoint,
            key,
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
        target(2, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
        target(3, StreamProtocol::Hls, "/var/lib/freemix/live"),
        target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();
    // One group per distinct output/protocol/bitrate identity in
    // first-appearance order: both RTMP targets share the first, both HLS
    // targets the second, and every HLS target splits with no stream key.
    assert_eq!(
        grouped.group_keys,
        vec![
            (output, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
            (output, OutputProtocol::Hls, DEFAULT_VIDEO_BITRATE_KBPS),
        ]
    );
    assert_eq!(
        grouped
            .splits
            .iter()
            .map(|split| split.group_index)
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 1]
    );
    for split in &grouped.splits {
        assert_eq!(
            split.stream_key.is_some(),
            split.protocol != OutputProtocol::Hls,
            "HLS targets carry no stream key"
        );
        assert_eq!(split.backup_endpoint, None);
    }
    assert_eq!(grouped.splits[2].endpoint.path(), "/var/lib/freemix/live");
}

#[cfg(feature = "native-media")]
#[test]
fn split_and_group_targets_partitions_rendition_groups_by_output_id() {
    use fm_model::{
        DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
        StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output_a = ModelOutputId::new(NonZeroU128::new(30).unwrap());
    let output_b = ModelOutputId::new(NonZeroU128::new(31).unwrap());
    let target = |id: u128, output: ModelOutputId, endpoint: &str| {
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            StreamProtocol::Rtmp,
            ModelStreamEndpoint::parse_for(StreamProtocol::Rtmp, endpoint).unwrap(),
            Some(ModelStreamKey::parse("live-key-0001").unwrap()),
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, output_a, "ingest-a1.example.test:1935/live"),
        target(2, output_a, "ingest-a2.example.test:1935/live"),
        target(3, output_b, "ingest-b1.example.test:1935/live"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();

    assert_eq!(
        grouped.group_keys,
        vec![
            (output_a, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
            (output_b, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
        ]
    );
    assert_eq!(
        grouped
            .splits
            .iter()
            .map(|split| split.group_index)
            .collect::<Vec<_>>(),
        vec![0, 0, 1]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn local_output_directories_extracts_the_hls_directory_of_each_authored_target() {
    use fm_model::{
        StreamEndpoint as ModelStreamEndpoint, StreamKey as ModelStreamKey, StreamProtocol,
        StreamTarget, StreamTargetId,
    };
    use fm_types::OutputId as ModelOutputId;

    let output = ModelOutputId::new(NonZeroU128::new(31).unwrap());
    let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
        let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
        let key = match protocol {
            StreamProtocol::Hls | StreamProtocol::Omt => None,
            StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                Some(ModelStreamKey::parse("live-key-0001").unwrap())
            }
        };
        StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            protocol,
            endpoint,
            key,
            output,
        )
        .unwrap()
    };
    let targets = [
        target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
        target(2, StreamProtocol::Hls, "/var/lib/freemix/live"),
        target(3, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
        target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
    ];
    let grouped = split_and_group_targets(&targets).unwrap();

    // Only local HLS targets contribute a directory, in authored order,
    // and the compiled endpoint already carries it as one absolute path
    // that the sink creates later.
    assert_eq!(
        local_output_directories(&grouped.splits),
        vec![
            PathBuf::from("/var/lib/freemix/live"),
            PathBuf::from("/var/lib/freemix/live-alt"),
        ]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_status_message_sequence_starts_at_one_and_skips_empty_passes() {
    let server = ServerIdentity {
        engine_id: "engine".to_owned(),
        project_id: "project".to_owned(),
        state_epoch: 1,
        log_id: "log".to_owned(),
    };
    assert!(
        stream_status_message(&server, 0, Vec::new())
            .unwrap()
            .is_none(),
        "an empty active set publishes no record"
    );
    let first = stream_status_message(&server, 0, vec![stream_status_placeholder_sample()])
        .unwrap()
        .expect("non-empty set publishes");
    assert_eq!(first.sequence, 1);
    assert_eq!(first.server, server);
    let second = stream_status_message(
        &server,
        first.sequence,
        vec![stream_status_placeholder_sample()],
    )
    .unwrap()
    .expect("non-empty set publishes");
    assert_eq!(second.sequence, 2);
    assert!(encode_line(&WireMessage::StreamStatus(second)).is_ok());
}

#[cfg(feature = "native-media")]
fn stream_status_placeholder_sample() -> StreamStatusSample {
    StreamStatusSample {
        target: WireStreamTargetId::new(NonZeroU128::new(3).unwrap()),
        realized: StreamRealizedState::Starting,
        connected: false,
        muxed_bytes: 0,
        enqueued_pairs: 0,
        dropped_pairs: 0,
        reconnects: 0,
        uptime_ms: 0,
        bitrate_kbps: 0,
        failure: None,
        local: None,
    }
}

#[cfg(feature = "native-media")]
#[test]
fn stream_feed_builds_valid_paired_frames_from_cloned_audio() {
    // Mirrors NativeProgramStream::compile: format from project settings,
    // pairs built with each frame's audio sequence and a cloned block.
    let format = RecordFormat::new(
        16,
        16,
        FrameRate::new(25, 1).unwrap(),
        SampleRate::new(44_100).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    )
    .unwrap();
    let build_pair = |sequence: SequenceNumber| {
        // The engine's absolute-boundary audio contract, restated here
        // because the codec keeps its timing oracle private:
        // sample bounds are floor(n * rate * den / num).
        let hertz = u128::from(format.sample_rate().hertz());
        let boundary = |frames: u64| -> u128 {
            u128::from(frames) * hertz * u128::from(format.frame_rate().denominator())
                / u128::from(format.frame_rate().numerator())
        };
        let start_sample = boundary(sequence.get());
        let end_sample = boundary(sequence.get().checked_add(1).expect("sequence fits"));
        let start_nanos = start_sample * 1_000_000_000 / hertz;
        let duration_nanos = (end_sample * 1_000_000_000 / hertz) - start_nanos;
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(i64::try_from(start_sample).unwrap()),
                fm_frame::TimeBase::new(1, format.sample_rate().hertz()).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).unwrap()),
            NormalizedDuration::from_nanos(u64::try_from(duration_nanos).unwrap()).unwrap(),
            native_clock_domain(),
            sequence,
        )
        .unwrap();
        let samples = usize::try_from(end_sample - start_sample).unwrap();
        AudioBlock::new(
            timing,
            format.sample_rate(),
            format.channel_layout().clone(),
            vec![vec![0.25; samples], vec![-0.25; samples]],
        )
        .unwrap()
    };
    for sequence in [SequenceNumber::new(0), SequenceNumber::new(41)] {
        let audio = build_pair(sequence);
        let cloned = audio.clone();
        let pair = PairedFrame::new(
            &format,
            sequence,
            vec![0; format.rgba_bytes_per_frame()],
            cloned,
        )
        .expect("cloned project audio must pair");
        assert_eq!(pair.sequence(), sequence);
        assert_eq!(pair.format(), &format);
    }
}

/// Regression: the compiled grades are shared across revisions that do
/// not change them, so a cut or a fader move never re-bakes and
/// re-uploads every native grade lattice on the frame loop.
#[test]
fn grade_cache_keeps_grades_across_commands_that_do_not_change_them() {
    let project = test_project();
    let mut control = test_control(&project);
    let principal = operator();
    let graded = control
        .submit(
            &principal,
            test_command(
                "grade",
                "grade-key",
                CommandPayload::SetInputGrade {
                    input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    source: Some(fm_protocol::WireGradeSource::Inline(Box::new(
                        fm_grade::Grade {
                            saturation_permille: 0,
                            ..fm_grade::Grade::IDENTITY
                        },
                    ))),
                    gesture: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(graded.is_accepted());
    let mut cache = grading::GradeCache::default();
    let before = cache.at(&control);
    let cut = control
        .submit(
            &principal,
            test_command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(cut.is_accepted());
    let after = cache.at(&control);
    assert!(Arc::ptr_eq(&before, &after));
}

const ANIMATED_TITLE_URI: &str = "asset://titles/unit.fmtitle";

fn animated_title_template(elements: u128, clip: fm_titles::TitleClip) -> fm_titles::TitleTemplate {
    fm_titles::TitleTemplate {
        id: fm_titles::TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower".into(),
        width: 32,
        height: 18,
        background: fm_titles::Color::TRANSPARENT,
        fields: Vec::new(),
        elements: (1..=elements)
            .map(|element| fm_titles::Element {
                id: fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                name: format!("box {element}"),
                bounds: fm_titles::Bounds {
                    x: 2,
                    y: 2,
                    width: 8,
                    height: 8,
                },
                z_index: 0,
                visible: true,
                style: fm_titles::Style::default(),
                color_field: None,
                kind: fm_titles::ElementKind::Rectangle,
            })
            .collect(),
        animation: fm_titles::TitleAnimation::new(clip, 0),
    }
}

/// [`test_project`] with title input 3 naming [`ANIMATED_TITLE_URI`].
fn animated_title_project() -> StoredProject {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Lower third".into(),
        kind: InputKind::Title(fm_model::TitleInput {
            template_uri: ANIMATED_TITLE_URI.into(),
            field_revision: 0,
            fields: Vec::new(),
        }),
        required_capabilities: Vec::new(),
    });
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

/// Refuses every journal append and every discard of a staged copy.
struct UndiscardableStager {
    discarded: Cell<bool>,
}

impl DurableStore for UndiscardableStager {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        Err(AppFailure("injected journal failure".into()).into())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }

    fn discard_staged_title_animations(&self, _revision: u64, uris: &[String]) -> AppResult<()> {
        assert_eq!(uris, [ANIMATED_TITLE_URI.to_owned()]);
        self.discarded.set(true);
        Err(AppFailure("injected discard failure".into()).into())
    }
}

/// Regression: an animation edit refused after its template was staged
/// left the staged copy behind when discarding it failed. The next
/// command reuses the revision, and the checkpoint covering it committed
/// the refused animation over the template. A copy that cannot be
/// discarded now stops the daemon, whose next start discards it.
#[test]
fn a_staged_title_animation_that_cannot_be_discarded_stops_the_daemon() {
    let rate = FrameRate::new(25, 1).unwrap();
    let empty =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
            .unwrap();
    let mut durable = animated_title_project();
    let assets = BundleAssets {
        titles: [(test_input_id(3), animated_title_template(0, empty))].into(),
        luts: std::collections::BTreeMap::new(),
        assets_root: None,
    };
    let mut control = ControlService::new(
        restore_engine_with_titles(&durable, &assets).unwrap(),
        Policy::development(),
        "unit-engine",
        "unit-log",
        ControlLimits::default(),
    );
    let server = test_server(&control);
    let longer =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
            .unwrap();
    let command = test_command(
        "animate",
        "animate-key",
        CommandPayload::SetTitleAnimation {
            input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
            base_revision: 0,
            clip: Box::new(fm_control::title_clip_to_wire(&longer)),
        },
    );
    let store = UndiscardableStager {
        discarded: Cell::new(false),
    };
    let outcome = execute_durable_command(
        &mut control,
        &store,
        &mut durable,
        &operator(),
        &server,
        &command,
        0,
    );
    assert!(store.discarded.get());
    assert!(
        outcome.is_err(),
        "an undiscardable staged copy must stop the daemon"
    );
    assert_eq!(control.diagnostics().current_revision, 0);
}

/// Regression: an animation edit stays staged beside its template until
/// a checkpoint covers it, and checkpoints committed only the templates
/// a title still named. When the only title naming the template was
/// removed after the edit, the copy was never committed or discarded, and
/// a title added offline later picked the stale edit up on the next
/// start. Every staged copy now commits by the same durable-revision
/// rule, at the checkpoint and at startup recovery alike.
#[test]
fn a_staged_animation_of_a_template_no_title_names_any_more_still_commits() {
    use journal::DurableJournal;

    let rate = FrameRate::new(25, 1).unwrap();
    let empty =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
            .unwrap();
    let longer =
        fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
            .unwrap();
    for checkpoint in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let template_path = store.assets_root().join("titles/unit.fmtitle");
        fs::create_dir_all(template_path.parent().unwrap()).unwrap();
        fs::write(
            &template_path,
            fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                template: animated_title_template(0, empty.clone()),
                fonts: Vec::new(),
                clocks: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let manifest = animated_title_project();
        store.save(&manifest).unwrap();
        let assets = BundleAssets {
            titles: [(test_input_id(3), animated_title_template(0, empty.clone()))].into(),
            luts: std::collections::BTreeMap::new(),
            assets_root: None,
        };
        let mut control = ControlService::new(
            restore_engine_with_titles(&manifest, &assets).unwrap(),
            Policy::development(),
            "unit-engine",
            "unit-log",
            ControlLimits::default(),
        );
        let server = test_server(&control);
        let mut durable = manifest.clone();
        {
            let journal = DurableJournal::open(&store).unwrap();
            for command in [
                test_command(
                    "animate",
                    "animate-key",
                    CommandPayload::SetTitleAnimation {
                        input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                        base_revision: 0,
                        clip: Box::new(fm_control::title_clip_to_wire(&longer)),
                    },
                ),
                test_command(
                    "remove",
                    "remove-key",
                    CommandPayload::RemoveInput {
                        input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                    },
                ),
            ] {
                let outcome = execute_durable_command(
                    &mut control,
                    &journal,
                    &mut durable,
                    &operator(),
                    &server,
                    &command,
                    0,
                )
                .unwrap();
                let result = &outcome.submission.output.result;
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{result:?}"
                );
            }
            if checkpoint {
                journal.settle(&durable).unwrap();
            }
        }
        if !checkpoint {
            load_and_recover(&store, false).unwrap();
        }
        let titles: Vec<String> = fs::read_dir(template_path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(titles, ["unit.fmtitle"], "checkpoint={checkpoint}");
        assert_eq!(
            store
                .load_title_template(ANIMATED_TITLE_URI)
                .unwrap()
                .template
                .animation
                .clip,
            longer,
            "checkpoint={checkpoint}"
        );
    }
}

/// Regression: a client could import a LUT under the id of a removed
/// one, replacing its bundled file, so an older manifest, journal, or
/// autosave naming that id loaded different content. Bundled LUT files
/// are immutable: the import is refused as a non-retryable conflict and
/// the file is untouched; the same text under the same id is accepted.
#[test]
fn a_lut_import_never_replaces_a_bundled_file_with_different_content() {
    use journal::DurableJournal;

    const INVERT: &str = "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";
    const IDENTITY: &str =
        "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    // LUT 1 was imported and removed before: its file stays.
    let one = NonZeroU32::new(1).unwrap();
    store
        .write_lut_source(fm_model::LutId::new(one), INVERT)
        .unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let journal = DurableJournal::open(&store).unwrap();
    let import = |key: &str, data: &str| {
        test_command(
            key,
            &format!("{key}-key"),
            CommandPayload::ImportGradeLut {
                lut: one,
                name: "Look".into(),
                data: data.into(),
            },
        )
    };

    let refused = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &import("reuse", IDENTITY),
        0,
    )
    .unwrap();
    let CommandResult::Rejected {
        code, retryable, ..
    } = &refused.submission.output.result
    else {
        panic!("{:?}", refused.submission.output.result);
    };
    assert_eq!(code, RejectionCode::Conflict.as_str());
    assert!(!retryable);
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(
        fs::read_to_string(store.assets_root().join("luts/1.cube")).unwrap(),
        INVERT
    );

    let same = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &import("same", INVERT),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            same.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        same.submission.output.result
    );
}

/// Regression: a stored template whose animation has no wire encoding
/// (beyond `MAX_TITLE_ANIMATION_BYTES`) made every snapshot's
/// `title_animation_state` unencodable, so no session could connect.
/// Like names beyond the `title_state` bounds, it refuses to start.
#[test]
fn templates_whose_animation_has_no_wire_encoding_refuse_to_start() {
    use fm_titles::fm_animation::{
        Clip, CubicBezier, Easing, Keyframe, MAX_PHASE_FRAMES, Phase, Property, Timebase, Track,
        TrackKey, Value,
    };

    let bezier = Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
    let tracks = 16_u128;
    let clip = Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        MAX_PHASE_FRAMES,
        0,
        (1..=tracks)
            .map(|element| {
                Track::new(
                    TrackKey::new(
                        fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                        Phase::In,
                        Property::PositionX,
                    ),
                    (0..256_u32)
                        .map(|frame| {
                            Keyframe::new(frame * 100, Value::Scalar(i64::from(i32::MIN)), bezier)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
    .unwrap();
    assert!(
        fm_protocol::title_clip_wire_bytes(&fm_control::title_clip_to_wire(&clip))
            > fm_protocol::MAX_TITLE_ANIMATION_BYTES
    );
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template: animated_title_template(tracks, clip),
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let path = store.assets_root().join("titles/unit.fmtitle");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, document).unwrap();

    let error = load_title_templates(&store, animated_title_project().project())
        .expect_err("an animation without a wire encoding refuses to start");
    assert!(error.to_string().contains("animation"), "{error}");
}

/// Opens one track WAV state at the test format with the given options.
#[cfg(feature = "native-media")]
fn open_track_wav(
    map: fm_record::RecorderTrackMap,
    segment: &Path,
    policy: TrackWavPolicy,
    status_path: Option<std::path::PathBuf>,
    bext: Option<fm_record::BextMetadata>,
    ixml: Option<String>,
) -> TrackWavState {
    TrackWavState::create_with_policy(
        map,
        TrackWavOpen {
            segment,
            sample_rate: 48_000,
            channels: 2,
            fallback: None,
            policy,
            status_path,
            bext,
            ixml,
        },
    )
    .unwrap()
}

/// AU-010: an authored track map routes Master and aux bus samples into one
/// interleaved WAV beside the segment.
#[cfg(feature = "native-media")]
#[test]
fn a_track_map_writes_master_and_aux_channels_beside_the_segment() {
    use fm_record::{
        BusInventory, RecordBus, RecordTrack, RecorderTrackMap, TrackSource, read_wav_info,
    };
    use fm_types::audio_bus::AuxBusNumber;

    let directory = tempfile::tempdir().unwrap();
    let segment = directory.path().join("show-0001.mp4");
    let bus = AuxBusNumber::new(1).unwrap();
    let map = RecorderTrackMap::new(
        vec![
            RecordTrack::stereo("Program", TrackSource::master(0), TrackSource::master(1)),
            RecordTrack::mono("Bus A", TrackSource::new(RecordBus::Aux(bus), 1)),
        ],
        BusInventory::new(vec![(RecordBus::Master, 2), (RecordBus::Aux(bus), 2)]).unwrap(),
    )
    .unwrap();
    let project = fm_types::ProjectId::new(std::num::NonZeroU128::new(99).unwrap());
    let frame_rate = fm_types::FrameRate::new(25, 1).unwrap();
    let timecode = fm_types::Timecode::new(0, 0, 1, 0, 25).unwrap();
    let loudness = fm_record::BextLoudness {
        integrated_lufs: -2300,
        range_lu: 750,
        max_true_peak_dbtp: -120,
        max_momentary_lufs: -2250,
        max_short_term_lufs: -2310,
    };
    let metadata = track_wav_metadata(
        project,
        0,
        &segment,
        Some(timecode),
        frame_rate,
        48_000,
        Some(loudness),
    );
    assert_eq!(metadata.bext.time_reference, 48_000);
    assert_eq!(metadata.bext.umid, track_wav_umid(project, 0));
    let mut track = open_track_wav(
        map,
        &segment,
        TrackWavPolicy::default(),
        None,
        Some(metadata.bext.clone()),
        Some(metadata.ixml.clone()),
    );
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(0),
            fm_frame::TimeBase::new(1, 48_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(41_666_667).unwrap(),
        native_clock_domain(),
        SequenceNumber::new(0),
    )
    .unwrap();
    let audio = AudioBlock::new(
        timing,
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        vec![vec![0.25, 0.5], vec![0.5, 0.75]],
    )
    .unwrap();
    let aux = vec![vec![0.0, 0.0], vec![0.75, 0.25]];
    track.write(&audio, &aux, 2).unwrap();
    let finish = track.finish();
    assert!(finish.failures.is_empty(), "{:?}", finish.failures);

    let path = TrackWavState::path(&segment);
    let mut file = File::open(&path).unwrap();
    let info = read_wav_info(&mut file).unwrap();
    assert_eq!(info.spec.channels, 3);
    assert_eq!(info.spec.sample_rate, 48_000);
    assert_eq!(info.frames(), 2, "two frames in a three-channel file");
    let bext = info.spec.bext.as_ref().expect("BWF metadata");
    assert_eq!(bext.description, metadata.bext.description);
    assert_eq!(bext.originator, "FreeMix");
    assert_eq!(bext.time_reference, 48_000);
    assert_eq!(bext.umid, metadata.bext.umid);
    let patched = bext.loudness.expect("measured loudness fields");
    assert_eq!(
        patched.integrated_lufs, loudness.integrated_lufs,
        "two frames cannot measure integrated loudness, so the authored value stays"
    );
    assert_eq!(
        info.spec.ixml.as_deref(),
        Some(metadata.ixml.as_str()),
        "the iXML document is written into the track WAV"
    );
    let data = fs::read(&path).unwrap();
    let offset = usize::try_from(info.data_offset).unwrap();
    let samples: Vec<i16> = data[offset..]
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(samples, [8192, 16_384, 24_576, 16_384, 24_576, 8192]);
}

/// AU-010: the daemon meters the exact track samples and patches the
/// measured loudness into the finalized version-2 bext fields.
#[cfg(feature = "native-media")]
#[test]
fn track_wav_loudness_is_measured_and_patched() {
    use fm_record::read_wav_info;

    let directory = tempfile::tempdir().unwrap();
    let segment = directory.path().join("show.mp4");
    let map = fm_record::RecorderTrackMap::new(
        vec![fm_record::RecordTrack::stereo(
            "Program",
            fm_record::TrackSource::master(0),
            fm_record::TrackSource::master(1),
        )],
        fm_record::BusInventory::new(vec![(RecordBus::Master, 2)]).unwrap(),
    )
    .unwrap();
    let project = fm_types::ProjectId::new(std::num::NonZeroU128::new(99).unwrap());
    let frame_rate = fm_types::FrameRate::new(25, 1).unwrap();
    let bext = track_wav_bext(project, 0, &segment, None, frame_rate, 48_000, None);
    let mut track = open_track_wav(
        map,
        &segment,
        TrackWavPolicy::default(),
        None,
        Some(bext),
        None,
    );
    let chunks = 40_u64;
    let frames = 4_800_usize;
    for chunk in 0..chunks {
        let mut plane = Vec::with_capacity(frames);
        for frame in 0..frames {
            let sample = u64::try_from(frame).unwrap() + chunk * u64::try_from(frames).unwrap();
            #[allow(clippy::cast_precision_loss)]
            let seconds = sample as f32 / 48_000.0;
            plane.push(0.25 * (core::f32::consts::TAU * 1_000.0 * seconds).sin());
        }
        let timing = MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(0),
                fm_frame::TimeBase::new(1, 48_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(0),
            NormalizedDuration::from_nanos(100_000_000).unwrap(),
            native_clock_domain(),
            SequenceNumber::new(chunk),
        )
        .unwrap();
        let audio = AudioBlock::new(
            timing,
            SampleRate::new(48_000).unwrap(),
            ChannelLayout::stereo(),
            vec![plane.clone(), plane],
        )
        .unwrap();
        track.write(&audio, &[], 2).unwrap();
    }
    let finish = track.finish();
    assert!(finish.failures.is_empty(), "{:?}", finish.failures);
    assert!(finish.loudness_files >= 1, "{finish:?}");
    assert_eq!(finish.loudness_failures, 0, "{finish:?}");
    assert_eq!(
        finish.measured.len(),
        usize::try_from(finish.loudness_files).unwrap(),
        "every patched file is reported"
    );

    let path = TrackWavState::path(&segment);
    let mut file = File::open(&path).unwrap();
    let info = read_wav_info(&mut file).unwrap();
    let bext = info.spec.bext.expect("BWF metadata");
    let loudness = bext.loudness.expect("measured loudness");
    assert_eq!(finish.measured[0].path, path);
    assert_eq!(
        finish.measured[0].loudness, loudness,
        "the report carries the values written into bext"
    );
    assert_ne!(loudness.integrated_lufs, i16::MAX, "{loudness:?}");
    assert!(loudness.integrated_lufs < 0, "{loudness:?}");
    assert_ne!(loudness.max_true_peak_dbtp, i16::MAX, "{loudness:?}");
}

/// AU-010: a split-layout map writes one WAV per track with its own channels
/// and samples.
#[cfg(feature = "native-media")]
#[test]
#[allow(clippy::too_many_lines)] // one end-to-end write, read, and metadata pass
fn a_split_track_map_writes_one_file_per_track() {
    use fm_record::{
        BusInventory, RecordBus, RecordTrack, RecorderTrackMap, TrackMapLayout, TrackSource,
        read_wav_info,
    };
    use fm_types::audio_bus::AuxBusNumber;

    let directory = tempfile::tempdir().unwrap();
    let segment = directory.path().join("show-0001.mp4");
    let bus = AuxBusNumber::new(1).unwrap();
    let map = RecorderTrackMap::new(
        vec![
            RecordTrack::stereo("Program", TrackSource::master(0), TrackSource::master(1)),
            RecordTrack::mono("Bus A", TrackSource::new(RecordBus::Aux(bus), 1)),
        ],
        BusInventory::new(vec![(RecordBus::Master, 2), (RecordBus::Aux(bus), 2)]).unwrap(),
    )
    .unwrap()
    .with_layout(TrackMapLayout::SplitPerTrack);
    let project = fm_types::ProjectId::new(std::num::NonZeroU128::new(99).unwrap());
    let frame_rate = fm_types::FrameRate::new(25, 1).unwrap();
    let timecode = fm_types::Timecode::new(0, 0, 1, 0, 25).unwrap();
    let metadata = track_wav_metadata(
        project,
        0,
        &segment,
        Some(timecode),
        frame_rate,
        48_000,
        None,
    );
    let mut track = open_track_wav(
        map,
        &segment,
        TrackWavPolicy::default(),
        None,
        Some(metadata.bext.clone()),
        Some(metadata.ixml.clone()),
    );
    assert_eq!(track.mode(), fm_record::TrackFileMode::SplitPerTrack);
    assert_eq!(
        track
            .paths()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["show-0001_01_Program.wav", "show-0001_02_Bus_A.wav"]
    );
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(0),
            fm_frame::TimeBase::new(1, 48_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(41_666_667).unwrap(),
        native_clock_domain(),
        SequenceNumber::new(0),
    )
    .unwrap();
    let audio = AudioBlock::new(
        timing,
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        vec![vec![0.25, 0.5], vec![0.5, 0.75]],
    )
    .unwrap();
    let aux = vec![vec![0.0, 0.0], vec![0.75, 0.25]];
    track.write(&audio, &aux, 2).unwrap();
    let finish = track.finish();
    assert!(finish.failures.is_empty(), "{:?}", finish.failures);

    let program = track.paths()[0].clone();
    let bus_a = track.paths()[1].clone();
    let mut file = File::open(&program).unwrap();
    let program_info = read_wav_info(&mut file).unwrap();
    assert_eq!(program_info.spec.channels, 2);
    assert_eq!(program_info.frames(), 2);
    assert_eq!(
        program_info
            .spec
            .bext
            .as_ref()
            .map(|bext| bext.description.as_str()),
        Some("FreeMix recorder 1 track map track=Program")
    );
    let mut file = File::open(&bus_a).unwrap();
    let bus_info = read_wav_info(&mut file).unwrap();
    assert_eq!(bus_info.spec.channels, 1);
    assert_eq!(bus_info.frames(), 2);
    assert_eq!(
        bus_info
            .spec
            .bext
            .as_ref()
            .map(|bext| bext.description.as_str()),
        Some("FreeMix recorder 1 track map track=Bus A")
    );
    assert_eq!(
        program_info.spec.ixml.as_deref(),
        Some(metadata.ixml.as_str())
    );
    assert_eq!(bus_info.spec.ixml.as_deref(), Some(metadata.ixml.as_str()));
    let data = fs::read(&bus_a).unwrap();
    let offset = usize::try_from(bus_info.data_offset).unwrap();
    let samples: Vec<i16> = data[offset..]
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(samples, [24_576, 8192]);
}

/// AU-010: startup WAV recovery finds and repairs an interrupted track
/// session by its derived name.
#[cfg(feature = "native-media")]
#[test]
fn startup_recovery_finds_an_interrupted_track_wav_session() {
    use fm_record::{LargeFilePolicy, WavSampleFormat, WavSpec, encode_wav_header};

    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("primary");
    fs::create_dir(&primary).unwrap();
    let segment = primary.join("program.mp4");
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48_000,
        format: WavSampleFormat::Pcm16,
        channel_mask: 0,
        large_file: LargeFilePolicy::Rf64WhenNeeded,
        bext: None,
        ixml: None,
    };
    let mut writer = recording_session::start_track_wav_session(
        "program.tracks",
        &segment,
        None,
        fm_record::ResilientWavConfig::default(),
    )
    .unwrap();
    writer.push(&encode_wav_header(&spec, 0).unwrap()).unwrap();
    writer.push(&vec![0_u8; 48_000 * 4]).unwrap();
    drop(writer);
    assert!(primary.join("program.tracks.wav.fmsession").exists());

    let records = recording_session::recover_interrupted(
        &[recording_session::RecoveryScope {
            slot: 1,
            output: &segment,
        }],
        None,
    );
    assert!(
        records.iter().any(|record| {
            record.contains("FREEMIXD_RECORDING_WAV_RECOVERY")
                && record.contains("\tsession=program.tracks.wav\t")
                && record.contains("\tstatus=recovered\t")
        }),
        "{records:?}"
    );
}

/// AU-010: a track WAV session follows the configured fallback directory
/// when its primary root starts refusing writes.
#[cfg(feature = "native-media")]
#[test]
fn a_track_wav_session_fails_over_to_the_fallback_root() {
    use fm_record::{LargeFilePolicy, WavSampleFormat, WavSpec, encode_wav_header};

    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("primary");
    let fallback = directory.path().join("fallback");
    fs::create_dir(&primary).unwrap();
    fs::create_dir(&fallback).unwrap();
    let segment = primary.join("program.mp4");
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48_000,
        format: WavSampleFormat::Pcm16,
        channel_mask: 0,
        large_file: LargeFilePolicy::Rf64WhenNeeded,
        bext: None,
        ixml: None,
    };
    let mut writer = recording_session::start_track_wav_session(
        "program.tracks",
        &segment,
        Some(&fallback),
        fm_record::ResilientWavConfig::default(),
    )
    .unwrap();
    writer.push(&encode_wav_header(&spec, 0).unwrap()).unwrap();
    writer.push(&vec![0_u8; 48_000 * 4]).unwrap();
    fs::set_permissions(
        &primary,
        std::os::unix::fs::PermissionsExt::from_mode(0o555),
    )
    .unwrap();
    let permissions_bind = fs::File::create(primary.join("probe")).is_err();
    if !permissions_bind {
        fs::set_permissions(
            &primary,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        return;
    }
    writer.request_rotation();
    writer.push(&vec![0_u8; 96_000 * 4]).unwrap();
    let report = writer.finish_report();
    fs::set_permissions(
        &primary,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let fallback_files: Vec<String> = fs::read_dir(&fallback)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        fallback_files
            .iter()
            .any(|name| name.contains("program.tracks")),
        "stats={:?} error={:?} primary={:?} fallback={fallback_files:?}",
        report.stats,
        report.error,
        fs::read_dir(&primary)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    // The session opened its next segment on the fallback when the primary
    // refused a new file. Some paths move roots during open rather than
    // through a recorded write failure, so assert both the moved file and
    // the second segment instead of only the failover counter.
    assert!(report.stats.segments_opened >= 2, "{:?}", report.stats);
    assert!(report.error.is_none(), "{:?}", report.error);
}

/// AU-010: track WAV sessions roll over on a time limit and a byte limit.
#[cfg(feature = "native-media")]
#[test]
fn track_wav_sessions_roll_over_on_time_and_bytes() {
    use fm_record::{BusInventory, RecordBus, RecordTrack, RecorderTrackMap, TrackSource};
    use std::num::NonZeroU64;

    let map = RecorderTrackMap::new(
        vec![RecordTrack::stereo(
            "Program",
            TrackSource::master(0),
            TrackSource::master(1),
        )],
        BusInventory::new(vec![(RecordBus::Master, 2)]).unwrap(),
    )
    .unwrap();
    for (label, policy, frames) in [
        (
            "time",
            TrackWavPolicy {
                max_segment_bytes: None,
                max_segment_duration: Some(std::time::Duration::from_millis(100)),
            },
            4_800_usize,
        ),
        (
            "bytes",
            TrackWavPolicy {
                max_segment_bytes: NonZeroU64::new(100_000),
                max_segment_duration: None,
            },
            48_000_usize,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let segment = directory.path().join("show.mp4");
        let mut track = open_track_wav(map.clone(), &segment, policy, None, None, None);
        for _ in 0..3 {
            let timing = MediaTiming::new(
                OriginalTimestamp::new(
                    MediaTimestamp::new(0),
                    fm_frame::TimeBase::new(1, 48_000).unwrap(),
                ),
                NormalizedTimestamp::from_nanos(0),
                NormalizedDuration::from_nanos(
                    u64::try_from(frames).unwrap() * 1_000_000_000 / 48_000,
                )
                .unwrap(),
                native_clock_domain(),
                SequenceNumber::new(0),
            )
            .unwrap();
            let audio = AudioBlock::new(
                timing,
                SampleRate::new(48_000).unwrap(),
                ChannelLayout::stereo(),
                vec![vec![0.25; frames]; 2],
            )
            .unwrap();
            track.write(&audio, &[], 2).unwrap();
        }
        let report = track.finish();
        assert!(report.failures.is_empty(), "{label}: {:?}", report.failures);
        assert!(report.segments >= 2, "{label}: {report:?}");
        assert!(
            directory.path().join("show.tracks-0002.wav").is_file(),
            "{label}: the second track segment exists"
        );
    }
}

/// AU-010: the daemon-written status names live track files and their state.
#[cfg(feature = "native-media")]
#[test]
fn track_wav_status_reports_live_files() {
    use fm_record::{BusInventory, RecordBus, RecordTrack, RecorderTrackMap, TrackSource};

    let directory = tempfile::tempdir().unwrap();
    let segment = directory.path().join("show.mp4");
    let status = directory.path().join("recorders/1.trackstatus");
    let map = RecorderTrackMap::new(
        vec![RecordTrack::stereo(
            "Program",
            TrackSource::master(0),
            TrackSource::master(1),
        )],
        BusInventory::new(vec![(RecordBus::Master, 2)]).unwrap(),
    )
    .unwrap();
    let mut track = open_track_wav(
        map,
        &segment,
        TrackWavPolicy::default(),
        Some(status.clone()),
        None,
        None,
    );
    let audio = AudioBlock::new(
        MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(0),
                fm_frame::TimeBase::new(1, 48_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(0),
            NormalizedDuration::from_nanos(1_000_000_000).unwrap(),
            native_clock_domain(),
            SequenceNumber::new(0),
        )
        .unwrap(),
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        vec![vec![0.25; 48_000]; 2],
    )
    .unwrap();
    track.write(&audio, &[], 2).unwrap();
    let live = fs::read_to_string(&status).unwrap();
    assert!(
        live.starts_with("freemix-record-track-status 1\nstate=recording\n"),
        "{live}"
    );
    assert!(
        live.lines().any(|line| line.starts_with("file\t")),
        "{live}"
    );
    track.finish();
    let stopped = fs::read_to_string(&status).unwrap();
    assert!(
        stopped.starts_with("freemix-record-track-status 1\nstate=stopped\n"),
        "{stopped}"
    );
}

/// AU-010: the bundle's authored limits become the recorder's rollover
/// policy, and a malformed document refuses startup.
#[cfg(feature = "native-media")]
#[test]
fn bundle_track_limits_reach_the_recorder_policy() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let unlimited = load_record_track_policy(&store).unwrap();
    assert!(unlimited.max_segment_bytes.is_none());
    assert!(unlimited.max_segment_duration.is_none());

    store
        .save_recorder_track_limits(fm_record::TrackLimits::new(Some(30), Some(1 << 20)).unwrap())
        .unwrap();
    let policy = load_record_track_policy(&store).unwrap();
    assert_eq!(policy.max_segment_duration, Some(Duration::from_secs(30)));
    assert_eq!(policy.max_segment_bytes, std::num::NonZeroU64::new(1 << 20));

    fs::write(
        store.recorder_track_limits_path(),
        "freemix-record-track-limits 1\nmax-seconds=0\n",
    )
    .unwrap();
    assert!(load_record_track_policy(&store).is_err());
}

/// AU-010: recorder metadata carries a project UMID and slot instance.
#[cfg(feature = "native-media")]
#[test]
fn track_wav_metadata_carries_a_umid() {
    let project = fm_types::ProjectId::new(std::num::NonZeroU128::new(99).unwrap());
    let umid = track_wav_umid(project, 1);
    assert_eq!(umid.len(), fm_record::UMID_BYTES);
    assert_eq!(&umid[..12], &fm_record::UMID_UNIVERSAL_LABEL[..]);
    assert_eq!(&umid[13..16], &[0, 0, 2]);
    assert_eq!(&umid[16..32], &99_u128.to_be_bytes());
    assert_eq!(&umid[32..], &[0; 32]);

    let frame_rate = fm_types::FrameRate::new(25, 1).unwrap();
    let loudness = fm_record::BextLoudness {
        integrated_lufs: -2300,
        range_lu: 750,
        max_true_peak_dbtp: -120,
        max_momentary_lufs: -2250,
        max_short_term_lufs: -2310,
    };
    let metadata = track_wav_metadata(
        project,
        0,
        std::path::Path::new("/tmp/show-0001.mp4"),
        None,
        frame_rate,
        48_000,
        Some(loudness),
    );
    assert_eq!(metadata.bext.loudness, Some(loudness));
    assert_eq!(metadata.bext.originator, "FreeMix");
    assert_eq!(metadata.bext.originator_reference, "show-0001");
    assert_eq!(metadata.bext.umid, track_wav_umid(project, 0));
    assert!(
        metadata
            .ixml
            .contains("<SCENE>show-0001</SCENE><TAKE>1</TAKE>"),
        "{}",
        metadata.ixml
    );
    assert!(metadata.ixml.contains("<NOTE>Recorder 1 track map</NOTE>"));
    assert!(
        metadata
            .ixml
            .contains("<TIMECODE_RATE>25/1</TIMECODE_RATE>")
    );
}

/// AU-010: one record names every measured field of one patched file.
#[cfg(feature = "native-media")]
#[test]
fn track_loudness_records_name_every_measured_field() {
    let loudness = fm_record::BextLoudness {
        integrated_lufs: -2300,
        range_lu: 750,
        max_true_peak_dbtp: -120,
        max_momentary_lufs: -2250,
        max_short_term_lufs: -2310,
    };
    assert_eq!(
        track_loudness_record(2, std::path::Path::new("/shows/bad\tname.wav"), loudness),
        "FREEMIXD_RECORD_TRACK_LOUDNESS_FILE\tv=1\tslot=2\tfile=/shows/bad name.wav\tintegrated=-2300\trange=750\ttrue_peak=-120\tmomentary=-2250\tshort_term=-2310"
    );
}
