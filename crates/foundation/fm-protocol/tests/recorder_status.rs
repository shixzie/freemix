//! Protocol 2.66 `recorder_status`: the exact lossy realized recorder record,
//! with Protocol 2.67 machine-readable failures.

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CodecError, OutputFailure, OutputFailureCode, ProtocolVersion,
    RecorderRealizedState, RecorderStatusMessage, RecorderStatusSample, ServerIdentity,
    WireMessage, WireRecorderSlot, decode_line, encode_line,
};

const HEAD: &str = "recorder_status\tengine_id=engine-a\tproject_id=project-a\tstate_epoch=7\tlog_id=log-a\tsequence=12";

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".into(),
        project_id: "project-a".into(),
        state_epoch: 7,
        log_id: "log-a".into(),
    }
}

fn sample(
    slot: u8,
    realized: RecorderRealizedState,
    elapsed_ms: u64,
    failure: Option<OutputFailure>,
) -> RecorderStatusSample {
    RecorderStatusSample {
        recorder: WireRecorderSlot::new(slot).unwrap(),
        realized,
        elapsed_ms,
        failure,
    }
}

fn message(samples: Vec<RecorderStatusSample>) -> WireMessage {
    WireMessage::RecorderStatus(RecorderStatusMessage {
        server: server(),
        sequence: 12,
        samples,
    })
}

#[test]
fn recorder_status_is_exact() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 130));
    for (samples, recorders) in [
        (
            vec![
                sample(1, RecorderRealizedState::Recording, 61_250, None),
                sample(2, RecorderRealizedState::Unavailable, 0, None),
            ],
            "1%3Arecording%3A61250%3A%3B2%3Aunavailable%3A0%3A",
        ),
        (
            vec![
                sample(1, RecorderRealizedState::Starting, 0, None),
                sample(
                    2,
                    RecorderRealizedState::Failed,
                    0,
                    Some(OutputFailureCode::DiskFull.into()),
                ),
            ],
            "1%3Astarting%3A0%3A%3B2%3Afailed%3A0%3Adisk_full",
        ),
        (
            vec![sample(
                1,
                RecorderRealizedState::Failed,
                0,
                Some(
                    OutputFailure::new(OutputFailureCode::FfmpegExited).with_exit_status(Some(-9)),
                ),
            )],
            "1%3Afailed%3A0%3Affmpeg_exited.-9",
        ),
        (
            vec![sample(2, RecorderRealizedState::Off, 0, None)],
            "2%3Aoff%3A0%3A",
        ),
    ] {
        let message = message(samples);
        let line = encode_line(&message).unwrap();
        assert_eq!(line, format!("{HEAD}\trecorders={recorders}\n"));
        assert_eq!(decode_line(&line).unwrap(), message);
    }
    for state in RecorderRealizedState::ALL {
        assert_eq!(
            RecorderRealizedState::from_token(state.token()),
            Some(state)
        );
    }
}

#[test]
fn recorder_status_rejects_out_of_contract_samples() {
    let invalid = |value: &str| CodecError::InvalidField {
        field: "recorders",
        value: value.to_owned(),
    };
    // Elapsed time only while recording; a failure exactly when failed.
    for (samples, value) in [
        (
            vec![sample(1, RecorderRealizedState::Off, 5, None)],
            "1".to_owned(),
        ),
        (
            vec![sample(1, RecorderRealizedState::Failed, 0, None)],
            "1".to_owned(),
        ),
        (
            vec![sample(
                1,
                RecorderRealizedState::Recording,
                5,
                Some(OutputFailureCode::Unknown.into()),
            )],
            "1".to_owned(),
        ),
        (
            vec![
                sample(2, RecorderRealizedState::Off, 0, None),
                sample(1, RecorderRealizedState::Off, 0, None),
            ],
            "1".to_owned(),
        ),
        (Vec::new(), "empty".to_owned()),
    ] {
        assert_eq!(encode_line(&message(samples)), Err(invalid(&value)));
    }
    for (recorders, error) in [
        ("", invalid("")),
        ("3%3Aoff%3A0%3A", invalid("3:off:0:")),
        ("01%3Aoff%3A0%3A", invalid("01:off:0:")),
        ("1%3Aon%3A0%3A", invalid("1:on:0:")),
        ("1%3Aoff%3A07%3A", invalid("1:off:07:")),
        ("1%3Aoff%3A0", invalid("1:off:0")),
        ("1%3Aoff%3A5%3A", invalid("1:off:5:")),
        ("1%3Afailed%3A0%3A", invalid("1:failed:0:")),
        // Only a known code, with at most a canonical decimal exit status.
        (
            "1%3Afailed%3A0%3Adisk%20full",
            invalid("1:failed:0:disk full"),
        ),
        ("1%3Afailed%3A0%3Astart", invalid("1:failed:0:start")),
        (
            "1%3Afailed%3A0%3Adisk_full.",
            invalid("1:failed:0:disk_full."),
        ),
        (
            "1%3Afailed%3A0%3Adisk_full.01",
            invalid("1:failed:0:disk_full.01"),
        ),
        (
            "1%3Afailed%3A0%3Adisk_full.1.2",
            invalid("1:failed:0:disk_full.1.2"),
        ),
        ("1%3Aoff%3A0%3A%3B1%3Aoff%3A0%3A", invalid("1:off:0:")),
        (
            "1%3Aoff%3A0%3A%3B2%3Aoff%3A0%3A%3B2%3Aoff%3A0%3A",
            CodecError::TooManyItems("recorders"),
        ),
    ] {
        assert_eq!(
            decode_line(&format!("{HEAD}\trecorders={recorders}\n")),
            Err(error),
            "{recorders}"
        );
    }
    assert!(decode_line(&format!("{HEAD}\n")).is_err());
}
