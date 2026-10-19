//! Machine-readable output failures (Protocol 2.67).
//!
//! A `stream_status` or `recorder_status` sample reports why a destination or
//! a recorder failed as one [`OutputFailure`]: a stable code from a closed
//! vocabulary and, when the failure was an `FFmpeg` child exiting, its exit
//! status. Clients turn the code into a short localized message; the record
//! never carries free text, so no stream key, passphrase, URL, or path can
//! reach a peer through it.
//!
//! Wire token: `<code>` or `<code>.<exit_status>` (a canonical decimal
//! `i32`), for example `connection_refused` or `connect_failed.145`.

use core::fmt;

/// Why a stream destination or a program recorder failed (Protocol 2.67).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OutputFailureCode {
    /// The destination could not be reached; no sharper cause was observed.
    ConnectFailed,
    /// The server actively refused the connection.
    ConnectionRefused,
    /// The server did not answer within the connect deadline.
    ConnectTimeout,
    /// No network route to the server.
    NetworkUnreachable,
    /// The server name did not resolve.
    HostNotFound,
    /// The secure (TLS) connection failed, for example its certificate.
    TlsFailed,
    /// The server refused the stream key or the credentials.
    StreamKeyRejected,
    /// An established connection broke off (reset, broken pipe, I/O error).
    ConnectionLost,
    /// Media stopped flowing to the destination.
    StreamStalled,
    /// The destination refused the stream's protocol or format.
    ProtocolError,
    /// The automatic reconnect budget ran out.
    ReconnectExhausted,
    /// `FFmpeg` is missing or unusable.
    FfmpegUnavailable,
    /// `FFmpeg` exited without a recognizable cause.
    FfmpegExited,
    /// The output disk is full.
    DiskFull,
    /// The output folder refused writes (permissions, read-only).
    FolderNotWritable,
    /// The output folder does not exist.
    FolderMissing,
    /// Writing the output failed at the storage level.
    WriteError,
    /// The recorder did not produce output in time.
    StartupTimeout,
    /// Frames arrived faster than the output could take them.
    FellBehind,
    /// The program picture could not be captured for the output.
    CaptureFailed,
    /// An internal error in the daemon.
    InternalError,
    /// No recognizable cause.
    Unknown,
}

impl OutputFailureCode {
    /// Every code in wire order.
    pub const ALL: [Self; 22] = [
        Self::ConnectFailed,
        Self::ConnectionRefused,
        Self::ConnectTimeout,
        Self::NetworkUnreachable,
        Self::HostNotFound,
        Self::TlsFailed,
        Self::StreamKeyRejected,
        Self::ConnectionLost,
        Self::StreamStalled,
        Self::ProtocolError,
        Self::ReconnectExhausted,
        Self::FfmpegUnavailable,
        Self::FfmpegExited,
        Self::DiskFull,
        Self::FolderNotWritable,
        Self::FolderMissing,
        Self::WriteError,
        Self::StartupTimeout,
        Self::FellBehind,
        Self::CaptureFailed,
        Self::InternalError,
        Self::Unknown,
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::ConnectFailed => "connect_failed",
            Self::ConnectionRefused => "connection_refused",
            Self::ConnectTimeout => "connect_timeout",
            Self::NetworkUnreachable => "network_unreachable",
            Self::HostNotFound => "host_not_found",
            Self::TlsFailed => "tls_failed",
            Self::StreamKeyRejected => "stream_key_rejected",
            Self::ConnectionLost => "connection_lost",
            Self::StreamStalled => "stream_stalled",
            Self::ProtocolError => "protocol_error",
            Self::ReconnectExhausted => "reconnect_exhausted",
            Self::FfmpegUnavailable => "ffmpeg_unavailable",
            Self::FfmpegExited => "ffmpeg_exited",
            Self::DiskFull => "disk_full",
            Self::FolderNotWritable => "folder_not_writable",
            Self::FolderMissing => "folder_missing",
            Self::WriteError => "write_error",
            Self::StartupTimeout => "startup_timeout",
            Self::FellBehind => "fell_behind",
            Self::CaptureFailed => "capture_failed",
            Self::InternalError => "internal_error",
            Self::Unknown => "unknown",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.token() == token)
    }
}

/// One output failure: a stable code plus the `FFmpeg` exit status when the
/// failure was its child exiting.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutputFailure {
    pub code: OutputFailureCode,
    pub exit_status: Option<i32>,
}

impl OutputFailure {
    /// A failure without an exit status.
    #[must_use]
    pub const fn new(code: OutputFailureCode) -> Self {
        Self {
            code,
            exit_status: None,
        }
    }

    /// The same failure carrying `FFmpeg`'s exit status.
    #[must_use]
    pub const fn with_exit_status(self, exit_status: Option<i32>) -> Self {
        Self {
            code: self.code,
            exit_status,
        }
    }

    /// Parses the wire token `<code>` or `<code>.<exit_status>`, refusing
    /// anything but a known code and a canonical decimal status.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        let (code, status) = match token.split_once('.') {
            Some((code, status)) => (code, Some(status)),
            None => (token, None),
        };
        let code = OutputFailureCode::from_token(code)?;
        let exit_status = match status {
            Some(status) => {
                let value = status.parse::<i32>().ok()?;
                if value.to_string() != status {
                    return None;
                }
                Some(value)
            }
            None => None,
        };
        Some(Self { code, exit_status })
    }
}

impl From<OutputFailureCode> for OutputFailure {
    fn from(code: OutputFailureCode) -> Self {
        Self::new(code)
    }
}

impl fmt::Display for OutputFailure {
    /// The wire token.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code.token())?;
        match self.exit_status {
            Some(status) => write!(formatter, ".{status}"),
            None => Ok(()),
        }
    }
}
