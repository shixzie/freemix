//! Machine-readable failure codes for stream destinations and program
//! recorders (Protocol 2.67).
//!
//! The daemon's own notices (`FREEMIXD_STREAM_FAILURE`,
//! `FREEMIXD_RECORDER_FAILURE`) keep their detailed, already-redacted text for
//! the operator's log. What reaches peers is only an [`OutputFailure`]: a code
//! from a closed vocabulary and, when known, `FFmpeg`'s exit status. The
//! classifiers below only ever pick a code, so no URL, stream key,
//! passphrase, or path in the text they read can travel further.

use fm_io_network::{FailureCause, FailureRecord, FailureStage};
use fm_protocol::{OutputFailure, OutputFailureCode};

/// The code for one transport failure the shared output set recorded.
pub(crate) fn stream_failure(record: &FailureRecord) -> OutputFailure {
    stream_failure_from(record.stage, record.cause, record.code)
}

/// The code for a transport failure's stage, cause, and exit status.
pub(crate) fn stream_failure_from(
    stage: FailureStage,
    cause: FailureCause,
    exit_status: Option<i64>,
) -> OutputFailure {
    let code = match cause {
        FailureCause::ConnectionRefused => OutputFailureCode::ConnectionRefused,
        FailureCause::TimedOut => OutputFailureCode::ConnectTimeout,
        FailureCause::NetworkUnreachable => OutputFailureCode::NetworkUnreachable,
        FailureCause::NameNotResolved => OutputFailureCode::HostNotFound,
        FailureCause::TlsRejected => OutputFailureCode::TlsFailed,
        FailureCause::CredentialsRejected => OutputFailureCode::StreamKeyRejected,
        FailureCause::ConnectionLost => OutputFailureCode::ConnectionLost,
        FailureCause::Stalled => OutputFailureCode::StreamStalled,
        FailureCause::Unsupported => OutputFailureCode::ProtocolError,
        FailureCause::ToolUnavailable => OutputFailureCode::FfmpegUnavailable,
        FailureCause::PermissionDenied => OutputFailureCode::FolderNotWritable,
        FailureCause::NotFound => OutputFailureCode::FolderMissing,
        FailureCause::StorageFull => OutputFailureCode::DiskFull,
        FailureCause::Internal => OutputFailureCode::InternalError,
        // A generic I/O error says no more than the stage it happened in.
        FailureCause::IoError | FailureCause::Unclassified => match stage {
            FailureStage::Dns => OutputFailureCode::HostNotFound,
            FailureStage::Tls => OutputFailureCode::TlsFailed,
            FailureStage::Authentication => OutputFailureCode::StreamKeyRejected,
            FailureStage::Connect => OutputFailureCode::ConnectFailed,
            FailureStage::Write => OutputFailureCode::ConnectionLost,
            FailureStage::Protocol => OutputFailureCode::ProtocolError,
        },
    };
    OutputFailure::new(code).with_exit_status(exit_status.and_then(|status| status.try_into().ok()))
}

/// The code for a stream target's latched failure the daemon raised itself
/// (`capture:`, `readback:`, `frame:`, `planner:`, `start:`, `fanout:`,
/// `transport:budget_exhausted`), as opposed to one the transport recorded.
pub(crate) fn latched_stream_failure(notice: &str) -> OutputFailure {
    let (stage, detail) = notice.split_once(':').unwrap_or((notice, ""));
    OutputFailure::new(match (stage, detail) {
        ("capture" | "readback" | "frame", _) => OutputFailureCode::CaptureFailed,
        ("transport", "budget_exhausted") => OutputFailureCode::ReconnectExhausted,
        _ => OutputFailureCode::InternalError,
    })
}

/// Text markers of a recorder failure, matched case-insensitively in order
/// against the daemon's notice and `FFmpeg`'s redacted stderr tail. The first
/// match wins: an unusable `FFmpeg` (whose `Debug` names an I/O kind such as
/// `PermissionDenied` too) outranks the storage causes, which outrank the
/// generic ones.
const RECORDER_MARKERS: [(&str, OutputFailureCode); 23] = [
    ("toolunavailable", OutputFailureCode::FfmpegUnavailable),
    ("invalidexecutable", OutputFailureCode::FfmpegUnavailable),
    ("spawn(notfound)", OutputFailureCode::FfmpegUnavailable),
    (
        "spawn(permissiondenied)",
        OutputFailureCode::FfmpegUnavailable,
    ),
    ("no space left", OutputFailureCode::DiskFull),
    ("storagefull", OutputFailureCode::DiskFull),
    ("disk quota exceeded", OutputFailureCode::DiskFull),
    ("quotaexceeded", OutputFailureCode::DiskFull),
    ("permission denied", OutputFailureCode::FolderNotWritable),
    ("permissiondenied", OutputFailureCode::FolderNotWritable),
    (
        "read-only file system",
        OutputFailureCode::FolderNotWritable,
    ),
    ("readonlyfilesystem", OutputFailureCode::FolderNotWritable),
    (
        "operation not permitted",
        OutputFailureCode::FolderNotWritable,
    ),
    (
        "no such file or directory",
        OutputFailureCode::FolderMissing,
    ),
    ("notfound", OutputFailureCode::FolderMissing),
    ("notadirectory", OutputFailureCode::FolderMissing),
    ("not a directory", OutputFailureCode::FolderMissing),
    ("input/output error", OutputFailureCode::WriteError),
    ("i/o error", OutputFailureCode::WriteError),
    ("outputwrite(", OutputFailureCode::WriteError),
    ("outputflush(", OutputFailureCode::WriteError),
    ("outputsync(", OutputFailureCode::WriteError),
    (
        "timed out before readiness",
        OutputFailureCode::StartupTimeout,
    ),
];

/// The code for one recorder failure: `notice` is the daemon's
/// `<stage>:<detail>` notice (`start:`, `rotate:`, `startup:`, `backend:`,
/// `enqueue:`, `capture:`, `readback:`, `frame:`), `stderr` the recorder's
/// redacted `FFmpeg` stderr tail when there is one.
pub(crate) fn recorder_failure(notice: &str, stderr: &str) -> OutputFailure {
    let exit_status = exit_status(notice);
    let (stage, detail) = notice.split_once(':').unwrap_or((notice, ""));
    let fixed = match stage {
        "capture" | "readback" | "frame" => Some(OutputFailureCode::CaptureFailed),
        "startup" if detail == "mux_readiness_timeout" => Some(OutputFailureCode::StartupTimeout),
        "enqueue" if ["QueueFull", "RetainedByteLimit", "open_backlog_full"].contains(&detail) => {
            Some(OutputFailureCode::FellBehind)
        }
        _ => None,
    };
    if let Some(code) = fixed {
        return OutputFailure::new(code);
    }
    let text = format!("{notice}\n{stderr}").to_ascii_lowercase();
    let code = RECORDER_MARKERS
        .into_iter()
        .find(|(marker, _)| text.contains(marker))
        .map_or_else(
            || {
                if text.contains("childexited") || text.contains("earlyexit") {
                    OutputFailureCode::FfmpegExited
                } else if stage == "enqueue" || text.contains("panicked") {
                    OutputFailureCode::InternalError
                } else {
                    OutputFailureCode::Unknown
                }
            },
            |(_, code)| code,
        );
    OutputFailure::new(code).with_exit_status(exit_status)
}

/// The exit status a `Debug`-rendered `ChildExited`/`EarlyExit` carries
/// (`status: Some(<n>)`), if any.
fn exit_status(text: &str) -> Option<i32> {
    let (_, rest) = text.split_once("status: Some(")?;
    let (number, _) = rest.split_once(')')?;
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: OutputFailureCode, exit_status: Option<i32>) -> OutputFailure {
        OutputFailure::new(code).with_exit_status(exit_status)
    }

    #[test]
    fn transport_causes_map_to_codes_and_keep_the_exit_status() {
        let record = |stage, cause, code| FailureRecord {
            at_ms: 0,
            stage,
            cause,
            code,
            message: "rtmp://ingest.example.test/live/secret-key: refused".to_owned(),
            retryable: true,
        };
        for (stage, cause, code, expected) in [
            (
                FailureStage::Connect,
                FailureCause::Unclassified,
                Some(145),
                failure(OutputFailureCode::ConnectFailed, Some(145)),
            ),
            (
                FailureStage::Connect,
                FailureCause::ConnectionRefused,
                Some(1),
                failure(OutputFailureCode::ConnectionRefused, Some(1)),
            ),
            (
                FailureStage::Authentication,
                FailureCause::CredentialsRejected,
                None,
                failure(OutputFailureCode::StreamKeyRejected, None),
            ),
            (
                FailureStage::Connect,
                FailureCause::NetworkUnreachable,
                None,
                failure(OutputFailureCode::NetworkUnreachable, None),
            ),
            (
                FailureStage::Write,
                FailureCause::IoError,
                Some(1),
                failure(OutputFailureCode::ConnectionLost, Some(1)),
            ),
            (
                FailureStage::Connect,
                FailureCause::IoError,
                None,
                failure(OutputFailureCode::ConnectFailed, None),
            ),
            (
                FailureStage::Write,
                FailureCause::StorageFull,
                None,
                failure(OutputFailureCode::DiskFull, None),
            ),
            (
                FailureStage::Protocol,
                FailureCause::PermissionDenied,
                None,
                failure(OutputFailureCode::FolderNotWritable, None),
            ),
            (
                FailureStage::Dns,
                FailureCause::Unclassified,
                None,
                failure(OutputFailureCode::HostNotFound, None),
            ),
            (
                FailureStage::Write,
                FailureCause::Stalled,
                None,
                failure(OutputFailureCode::StreamStalled, None),
            ),
            // An exit status that does not fit the wire's `i32` is dropped.
            (
                FailureStage::Connect,
                FailureCause::Unclassified,
                Some(i64::MAX),
                failure(OutputFailureCode::ConnectFailed, None),
            ),
        ] {
            let mapped = stream_failure(&record(stage, cause, code));
            assert_eq!(mapped, expected, "{stage:?} {cause:?}");
            // The token is all that reaches a peer: nothing of the message.
            let token = mapped.to_string();
            assert!(!token.contains("secret") && !token.contains("ingest"));
            assert_eq!(OutputFailure::parse(&token), Some(mapped));
        }
    }

    #[test]
    fn daemon_raised_stream_failures_have_codes() {
        for (notice, code) in [
            ("capture:missing_output", OutputFailureCode::CaptureFailed),
            ("readback:device lost", OutputFailureCode::CaptureFailed),
            ("frame:bad span", OutputFailureCode::CaptureFailed),
            (
                "transport:budget_exhausted",
                OutputFailureCode::ReconnectExhausted,
            ),
            ("start:UnknownDestination", OutputFailureCode::InternalError),
            ("planner:empty", OutputFailureCode::InternalError),
            ("fanout:closed", OutputFailureCode::InternalError),
        ] {
            assert_eq!(
                latched_stream_failure(notice),
                failure(code, None),
                "{notice}"
            );
        }
    }

    #[test]
    fn recorder_notices_and_stderr_classify_without_their_text() {
        for (notice, stderr, expected) in [
            (
                "start:FFmpeg recorder startup failed: EarlyExit { status: Some(1), stderr: \"/srv/show/secret/p.mp4: No space left on device\" } (Complete cleanup)",
                "",
                failure(OutputFailureCode::DiskFull, Some(1)),
            ),
            (
                "start:Permission denied (os error 13)",
                "",
                failure(OutputFailureCode::FolderNotWritable, None),
            ),
            (
                "rotate:No such file or directory (os error 2)",
                "",
                failure(OutputFailureCode::FolderMissing, None),
            ),
            (
                "backend:Some(ChildExited { status: Some(1) })",
                "[mp4 @ 0x1] /srv/show/p.mp4: Input/output error",
                failure(OutputFailureCode::WriteError, Some(1)),
            ),
            (
                "backend:Some(ChildExited { status: Some(137) })",
                "",
                failure(OutputFailureCode::FfmpegExited, Some(137)),
            ),
            (
                "backend:Some(OutputWrite(StorageFull))",
                "",
                failure(OutputFailureCode::DiskFull, None),
            ),
            (
                "backend:Some(OutputSync(Other))",
                "",
                failure(OutputFailureCode::WriteError, None),
            ),
            (
                "start:FFmpeg recorder startup failed: Spawn(NotFound) (Complete cleanup)",
                "",
                failure(OutputFailureCode::FfmpegUnavailable, None),
            ),
            (
                "startup:mux_readiness_timeout",
                "",
                failure(OutputFailureCode::StartupTimeout, None),
            ),
            (
                "startup:Program recorder mux output timed out before readiness",
                "",
                failure(OutputFailureCode::StartupTimeout, None),
            ),
            (
                "enqueue:QueueFull",
                "",
                failure(OutputFailureCode::FellBehind, None),
            ),
            (
                "enqueue:open_backlog_full",
                "",
                failure(OutputFailureCode::FellBehind, None),
            ),
            (
                "enqueue:FormatMismatch",
                "",
                failure(OutputFailureCode::InternalError, None),
            ),
            (
                "capture:missing_output",
                "",
                failure(OutputFailureCode::CaptureFailed, None),
            ),
            (
                "readback:device lost",
                "",
                failure(OutputFailureCode::CaptureFailed, None),
            ),
            // An `FFmpeg` that may not be run is not a folder problem.
            (
                "start:FFmpeg recorder startup failed: ToolUnavailable(PermissionDenied) (Complete cleanup)",
                "",
                failure(OutputFailureCode::FfmpegUnavailable, None),
            ),
            (
                "start:FFmpeg recorder startup failed: Spawn(PermissionDenied) (Complete cleanup)",
                "",
                failure(OutputFailureCode::FfmpegUnavailable, None),
            ),
            (
                "start:FFmpeg recorder startup failed: OutputMetadata(PermissionDenied) (Complete cleanup)",
                "",
                failure(OutputFailureCode::FolderNotWritable, None),
            ),
            ("", "", failure(OutputFailureCode::Unknown, None)),
        ] {
            let classified = recorder_failure(notice, stderr);
            assert_eq!(classified, expected, "{notice}");
            let token = classified.to_string();
            for leaked in ["/srv", "secret", "p.mp4", "os error"] {
                assert!(!token.contains(leaked), "{token}");
            }
        }
    }
}
