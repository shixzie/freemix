//! Localized reasons for stream destination and recorder failures
//! (Protocol 2.67 [`OutputFailure`] codes).

use fm_protocol::{OutputFailure, OutputFailureCode};

use super::{Arg, Locale, Msg};

/// Which output a failure belongs to: the folder codes name a recording
/// folder for a recorder and an output folder for a (local HLS) stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailedOutput {
    Recorder,
    Stream,
}

/// The message key for one failure code.
#[must_use]
pub const fn output_failure_msg(code: OutputFailureCode, output: FailedOutput) -> Msg {
    let recorder = matches!(output, FailedOutput::Recorder);
    match code {
        OutputFailureCode::ConnectFailed => Msg::FailConnectFailed,
        OutputFailureCode::ConnectionRefused => Msg::FailConnectionRefused,
        OutputFailureCode::ConnectTimeout => Msg::FailConnectTimeout,
        OutputFailureCode::NetworkUnreachable => Msg::FailNetworkUnreachable,
        OutputFailureCode::HostNotFound => Msg::FailHostNotFound,
        OutputFailureCode::TlsFailed => Msg::FailTlsFailed,
        OutputFailureCode::StreamKeyRejected => Msg::FailStreamKeyRejected,
        OutputFailureCode::ConnectionLost => Msg::FailConnectionLost,
        OutputFailureCode::StreamStalled => Msg::FailStreamStalled,
        OutputFailureCode::ProtocolError => Msg::FailProtocolError,
        OutputFailureCode::ReconnectExhausted => Msg::FailReconnectExhausted,
        OutputFailureCode::FfmpegUnavailable => Msg::FailFfmpegUnavailable,
        OutputFailureCode::FfmpegExited => Msg::FailFfmpegExited,
        OutputFailureCode::DiskFull => Msg::FailDiskFull,
        OutputFailureCode::FolderNotWritable if recorder => Msg::FailRecordingFolderNotWritable,
        OutputFailureCode::FolderNotWritable => Msg::FailOutputFolderNotWritable,
        OutputFailureCode::FolderMissing if recorder => Msg::FailRecordingFolderMissing,
        OutputFailureCode::FolderMissing => Msg::FailOutputFolderMissing,
        OutputFailureCode::WriteError => Msg::FailWriteError,
        OutputFailureCode::StartupTimeout => Msg::FailStartupTimeout,
        OutputFailureCode::FellBehind => Msg::FailFellBehind,
        OutputFailureCode::CaptureFailed => Msg::FailCaptureFailed,
        OutputFailureCode::InternalError => Msg::FailInternalError,
        OutputFailureCode::Unknown => Msg::FailUnknown,
    }
}

impl Locale {
    /// The short localized reason for `failure`, with `FFmpeg`'s exit code
    /// when the daemon reported one, such as `Couldn't connect to the server
    /// (FFmpeg exited with code 145)`.
    #[must_use]
    pub fn output_failure(self, failure: &OutputFailure, output: FailedOutput) -> String {
        let reason = self.text(output_failure_msg(failure.code, output));
        match failure.exit_status {
            Some(code) => self.format(
                Msg::FailWithExitCode,
                &[
                    ("reason", Arg::Str(reason)),
                    ("code", Arg::Str(&code.to_string())),
                ],
            ),
            None => reason.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_distinct_english_and_spanish_reason() {
        let spanish = Locale::new(super::super::Language::Spanish);
        for output in [FailedOutput::Recorder, FailedOutput::Stream] {
            let mut seen = std::collections::BTreeSet::new();
            for code in OutputFailureCode::ALL {
                let failure = OutputFailure::new(code);
                let english = Locale::ENGLISH.output_failure(&failure, output);
                assert!(seen.insert(english.clone()), "{english}");
                assert_ne!(
                    spanish.output_failure(&failure, output),
                    english,
                    "{code:?}"
                );
            }
        }
        let refused =
            OutputFailure::new(OutputFailureCode::ConnectFailed).with_exit_status(Some(145));
        assert_eq!(
            Locale::ENGLISH.output_failure(&refused, FailedOutput::Stream),
            "Couldn't connect to the server (FFmpeg exited with code 145)"
        );
        assert_eq!(
            spanish.output_failure(&refused, FailedOutput::Stream),
            "No se pudo conectar con el servidor (FFmpeg terminó con código 145)"
        );
        assert_eq!(
            Locale::ENGLISH.output_failure(
                &OutputFailure::new(OutputFailureCode::FolderNotWritable),
                FailedOutput::Recorder
            ),
            "Recording folder not writable"
        );
    }
}
