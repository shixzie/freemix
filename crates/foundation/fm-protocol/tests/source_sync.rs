//! Protocol 2.76 `source_sync_status` and the `diagnostics_response`
//! `source_sync` field: the exact lossy live source drift telemetry record
//! (AU-004).

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, DiagnosticsResponse, EngineIdentity,
    MAX_SOURCE_SYNC_MPPM, MAX_SOURCE_SYNC_SAMPLES, ProtocolVersion, ServerIdentity,
    SourceClockState, SourceSyncKind, SourceSyncSample, SourceSyncStatusMessage, WireInputId,
    WireMessage, decode_line, encode_line,
};

const HEAD: &str = "source_sync_status\tengine_id=engine-a\tproject_id=project-a\tstate_epoch=7\tlog_id=log-a\tsequence=12";

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".into(),
        project_id: "project-a".into(),
        state_epoch: 7,
        log_id: "log-a".into(),
    }
}

fn input(id: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(id).unwrap())
}

fn sample(id: u128) -> SourceSyncSample {
    SourceSyncSample {
        input: input(id),
        kind: SourceSyncKind::Capture,
        state: SourceClockState::Running,
        input_rate_hz: 44_100,
        output_rate_hz: 48_000,
        drift_mppm: -12_345,
        correction_mppm: 12_001,
        buffer_us: 81_250,
        target_us: 80_000,
        resyncs: 1,
        underruns: 2,
        overruns: 3,
        picture_drops: 4,
        picture_repeats: 5,
        restarts: 6,
    }
}

fn message(sources: Vec<SourceSyncSample>) -> WireMessage {
    WireMessage::SourceSyncStatus(SourceSyncStatusMessage {
        server: server(),
        sequence: 12,
        sources,
    })
}

const ENTRY: &str = "7%3Acapture%3Arunning%3A44100%3A48000%3A-12345%3A12001%3A81250%3A80000%3A1%3A2%3A3%3A4%3A5%3A6";

#[test]
fn source_sync_status_is_exact() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 147));
    let stream = SourceSyncSample {
        kind: SourceSyncKind::Stream,
        state: SourceClockState::Priming,
        input_rate_hz: 48_000,
        drift_mppm: 0,
        correction_mppm: 0,
        resyncs: 0,
        ..sample(9)
    };
    let system = SourceSyncSample {
        kind: SourceSyncKind::SystemAudio,
        state: SourceClockState::Recovering,
        drift_mppm: MAX_SOURCE_SYNC_MPPM,
        correction_mppm: -MAX_SOURCE_SYNC_MPPM,
        ..sample(10)
    };
    let screen = SourceSyncSample {
        kind: SourceSyncKind::ScreenCapture,
        ..sample(12)
    };
    for (sources, field) in [
        (Vec::new(), String::new()),
        (vec![sample(7)], ENTRY.to_owned()),
        (
            vec![sample(7), stream, system],
            format!(
                "{ENTRY}%3B9%3Astream%3Apriming%3A48000%3A48000%3A0%3A0%3A81250%3A80000%3A0%3A2%3A3%3A4%3A5%3A6\
                 %3B10%3Asystem_audio%3Arecovering%3A44100%3A48000%3A1000000000%3A-1000000000%3A81250%3A80000%3A1%3A2%3A3%3A4%3A5%3A6"
            ),
        ),
        (
            vec![screen],
            ENTRY.replacen("7%3Acapture", "12%3Ascreen_capture", 1),
        ),
    ] {
        let message = message(sources);
        let line = encode_line(&message).unwrap();
        assert_eq!(line, format!("{HEAD}\tsources={field}\n"));
        assert_eq!(decode_line(&line).unwrap(), message);
    }
    for kind in SourceSyncKind::ALL {
        assert_eq!(SourceSyncKind::from_token(kind.token()), Some(kind));
    }
    for state in SourceClockState::ALL {
        assert_eq!(SourceClockState::from_token(state.token()), Some(state));
    }
}

#[test]
fn source_sync_sample_derives_ratio_and_milliseconds() {
    let sample = sample(7);
    assert!((sample.drift_ppm() + 12.345).abs() < 1e-9);
    assert!((sample.correction_ppm() - 12.001).abs() < 1e-9);
    let ratio = 44_100.0 / 48_000.0 * (1.0 + 12.001e-6);
    assert!((sample.resampling_ratio() - ratio).abs() < 1e-12);
    assert!((sample.buffer_ms() - 81.25).abs() < 1e-9);
    assert!((sample.target_ms() - 80.0).abs() < 1e-9);
    assert_eq!(sample.faults(), 6);
}

#[test]
fn source_sync_status_rejects_out_of_contract_samples() {
    let invalid = |value: &str| CodecError::InvalidField {
        field: "sources",
        value: value.to_owned(),
    };
    for (sources, value) in [
        (vec![sample(8), sample(7)], "7"),
        (vec![sample(7), sample(7)], "7"),
        (
            vec![SourceSyncSample {
                input_rate_hz: 0,
                ..sample(7)
            }],
            "7",
        ),
        (
            vec![SourceSyncSample {
                output_rate_hz: 768_001,
                ..sample(7)
            }],
            "7",
        ),
        (
            vec![SourceSyncSample {
                drift_mppm: MAX_SOURCE_SYNC_MPPM + 1,
                ..sample(7)
            }],
            "7",
        ),
        (
            vec![SourceSyncSample {
                correction_mppm: -MAX_SOURCE_SYNC_MPPM - 1,
                ..sample(7)
            }],
            "7",
        ),
    ] {
        assert_eq!(encode_line(&message(sources)), Err(invalid(value)));
    }
    let too_many = (1..=u128::try_from(MAX_SOURCE_SYNC_SAMPLES).unwrap() + 1)
        .map(sample)
        .collect();
    assert_eq!(
        encode_line(&message(too_many)),
        Err(CodecError::TooManyItems("sources"))
    );
    let decoded = |field: &str| decode_line(&format!("{HEAD}\tsources={field}\n"));
    for field in [
        // Non-canonical numbers and unknown tokens.
        "07:capture:running:44100:48000:0:0:0:0:0:0:0:0:0:0",
        "0:capture:running:44100:48000:0:0:0:0:0:0:0:0:0:0",
        "7:camera:running:44100:48000:0:0:0:0:0:0:0:0:0:0",
        "7:capture:playing:44100:48000:0:0:0:0:0:0:0:0:0:0",
        "7:capture:running:044100:48000:0:0:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:-0:0:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:+5:0:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:0:01:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:0:0:-1:0:0:0:0:0:0:0",
        // Out of range.
        "7:capture:running:0:48000:0:0:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:1000000001:0:0:0:0:0:0:0:0:0",
        // Wrong arity.
        "7:capture:running:44100:48000:0:0:0:0:0:0:0:0:0",
        "7:capture:running:44100:48000:0:0:0:0:0:0:0:0:0:0:0",
    ] {
        let escaped = field.replace(':', "%3A");
        assert_eq!(decoded(&escaped), Err(invalid(field)), "{field}");
    }
    let one = "7:capture:running:44100:48000:0:0:0:0:0:0:0:0:0:0";
    let two = "8:capture:running:44100:48000:0:0:0:0:0:0:0:0:0:0";
    assert_eq!(
        decoded(
            &format!("{two};{one}")
                .replace(':', "%3A")
                .replace(';', "%3B")
        ),
        Err(invalid(one))
    );
    assert_eq!(
        decoded(
            &format!("{one};{one}")
                .replace(':', "%3A")
                .replace(';', "%3B")
        ),
        Err(invalid(one))
    );
    assert!(decode_line(&format!("{HEAD}\n")).is_err());
}

#[test]
fn diagnostics_response_carries_the_source_sync_samples() {
    let response = |source_sync| {
        WireMessage::DiagnosticsResponse(DiagnosticsResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: "diag-1".into(),
            engine: EngineIdentity {
                engine_id: "engine-a".into(),
                state_epoch: 7,
                log_id: "log-a".into(),
            },
            current_revision: 3,
            oldest_retained_revision: Some(1),
            newest_retained_revision: Some(3),
            subscriber_count: 1,
            retained_events_limit: 128,
            subscriber_limit: 16,
            subscriber_queue_limit: 64,
            source_sync,
            health: fm_protocol::DaemonHealth::default(),
        })
    };
    let empty = response(Vec::new());
    let line = encode_line(&empty).unwrap();
    assert!(
        line.ends_with(
            "\tsubscriber_queue_limit=64\tsource_sync=\tuptime_ms=0\talerts=\tnotifications=\tsilences=\n"
        ),
        "{line}"
    );
    assert_eq!(decode_line(&line).unwrap(), empty);
    let one = response(vec![sample(7)]);
    let line = encode_line(&one).unwrap();
    assert!(line.contains(&format!("\tsource_sync={ENTRY}\t")), "{line}");
    assert_eq!(decode_line(&line).unwrap(), one);
    // The field is required.
    let missing = line.replace(&format!("\tsource_sync={ENTRY}"), "");
    assert_eq!(
        decode_line(&missing),
        Err(CodecError::MissingField("source_sync"))
    );
    assert_eq!(
        encode_line(&response(vec![sample(8), sample(7)])),
        Err(CodecError::InvalidField {
            field: "source_sync",
            value: "7".into(),
        })
    );
}
