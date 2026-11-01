//! Stream destination authoring in the engine (OR-006).
//!
//! [`crate::EngineCommand::SetStreamTarget`] adds, reconfigures, or removes
//! one destination: a project edit honouring the project padlock, undone by
//! restoring the previous configuration. The configuration arrives already
//! validated by `fm_model::StreamTarget`'s own constructors (protocol, key,
//! endpoint, bitrate, reconnect policy); what only the show knows is checked
//! here: the inventory bound, a unique name, an existing output, that a
//! running destination is stopped before it changes, and that no cue list
//! starts or stops a destination being removed.
//!
//! No rejection names a stream key or endpoint.

use core::fmt;

use fm_command::{FieldIssue, IdKind, RefusalReason, Rejection, RejectionCode};
use fm_model::{
    CueAction, SrtPassphrase, StartupPolicy, StreamEndpoint, StreamKey, StreamLadder,
    StreamProtocol, StreamReconnectPolicy, StreamTarget, StreamTargetError,
};
use fm_switcher::{MAX_STREAM_COUNT, StreamTargetId};
use fm_types::OutputId;

use crate::ShowState;

/// One destination's requested configuration, before the engine validates it
/// into an [`fm_model::StreamTarget`] (OR-006).
///
/// `key` is the exact stream key the destination will hold (the wire's keep,
/// clear, or replace already resolved against the stored one). `Debug`
/// masks it. `ladder` is the authored HLS ladder an edit keeps.
#[derive(Clone, Eq, PartialEq)]
pub struct EngineStreamTargetSpec {
    pub name: String,
    pub protocol: StreamProtocol,
    pub endpoint: String,
    pub backup_endpoint: Option<String>,
    pub key: Option<String>,
    pub output: OutputId,
    pub startup: StartupPolicy,
    pub video_bitrate_kbps: u32,
    pub reconnect_initial_delay_ms: u32,
    pub reconnect_max_delay_ms: u32,
    pub reconnect_max_attempts: Option<u32>,
    pub ladder: Option<StreamLadder>,
    /// The SRT send latency, `Some` exactly for SRT (schema 51).
    pub srt_latency_ms: Option<u32>,
    /// The exact SRT passphrase the destination will hold, resolved like
    /// `key`. `Debug` masks it.
    pub srt_passphrase: Option<String>,
}

impl fmt::Debug for EngineStreamTargetSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineStreamTargetSpec")
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("backup_endpoint", &self.backup_endpoint)
            .field(
                "key",
                &self.key.as_ref().map(|_| fm_model::REDACTED_STREAM_KEY),
            )
            .field("output", &self.output)
            .field("video_bitrate_kbps", &self.video_bitrate_kbps)
            .field("srt_latency_ms", &self.srt_latency_ms)
            .field(
                "srt_passphrase",
                &self
                    .srt_passphrase
                    .as_ref()
                    .map(|_| fm_model::REDACTED_STREAM_KEY),
            )
            .finish_non_exhaustive()
    }
}

impl EngineStreamTargetSpec {
    /// The exact specification of an existing destination.
    #[must_use]
    pub fn from_target(target: &StreamTarget) -> Self {
        let reconnect = target.reconnect();
        Self {
            name: target.name().to_owned(),
            protocol: target.protocol(),
            endpoint: target.endpoint().as_str().to_owned(),
            backup_endpoint: target
                .backup_endpoint()
                .map(|endpoint| endpoint.as_str().to_owned()),
            key: target.key().map(|key| key.expose_secret().to_owned()),
            output: target.output(),
            startup: target.startup(),
            video_bitrate_kbps: target.video_bitrate_kbps(),
            reconnect_initial_delay_ms: reconnect.initial_delay_ms(),
            reconnect_max_delay_ms: reconnect.max_delay_ms(),
            reconnect_max_attempts: reconnect.max_attempts(),
            ladder: target.ladder().cloned(),
            srt_latency_ms: target.srt_latency_ms(),
            srt_passphrase: target
                .srt_passphrase()
                .map(|passphrase| passphrase.expose_secret().to_owned()),
        }
    }

    /// Validates this specification as destination `id`. A ladder survives
    /// only while the protocol is HLS.
    ///
    /// # Errors
    ///
    /// A rejection naming the refused field; no message carries the key or
    /// endpoint text.
    pub fn build(&self, id: StreamTargetId) -> Result<StreamTarget, Rejection> {
        let invalid = |field: &'static str, code: &'static str, message: String| {
            Rejection::new(RejectionCode::InvalidCommand, message.clone())
                .with_field(FieldIssue::new(field, code, message))
        };
        let endpoint = StreamEndpoint::parse_for(self.protocol, &self.endpoint)
            .map_err(|error| invalid("endpoint", "invalid_endpoint", error.to_string()))?;
        let backup = self
            .backup_endpoint
            .as_deref()
            .map(|text| {
                StreamEndpoint::parse_for(self.protocol, text).map_err(|error| {
                    invalid("backup_endpoint", "invalid_endpoint", error.to_string())
                })
            })
            .transpose()?;
        let key = self
            .key
            .as_deref()
            .map(|key| {
                StreamKey::parse(key)
                    .map_err(|error| invalid("stream_key", "invalid_stream_key", error.to_string()))
            })
            .transpose()?;
        let passphrase = self
            .srt_passphrase
            .as_deref()
            .map(|passphrase| {
                SrtPassphrase::parse(passphrase).map_err(|error| {
                    invalid("srt_passphrase", "invalid_passphrase", error.to_string())
                })
            })
            .transpose()?;
        let target_error = |error: StreamTargetError| {
            let field = match error {
                StreamTargetError::EmptyName | StreamTargetError::NameTooLong => "name",
                StreamTargetError::MissingStreamKey
                | StreamTargetError::UnexpectedStreamKey
                | StreamTargetError::InvalidSrtStreamId => "stream_key",
                StreamTargetError::BackupNotAllowed => "backup_endpoint",
                StreamTargetError::LadderNotAllowed => "ladder",
                StreamTargetError::VideoBitrateOutOfRange => "video_bitrate_kbps",
                StreamTargetError::UrlTooLong | StreamTargetError::EndpointProtocolMismatch => {
                    "endpoint"
                }
                StreamTargetError::SrtOptionsNotAllowed
                | StreamTargetError::SrtLatencyOutOfRange => "srt_latency_ms",
            };
            invalid(field, "invalid_stream_target", error.to_string())
        };
        let reconnect = StreamReconnectPolicy::new(
            self.reconnect_initial_delay_ms,
            self.reconnect_max_delay_ms,
            self.reconnect_max_attempts,
        )
        .map_err(|error| invalid("reconnect", "invalid_reconnect", error.to_string()))?;
        let target = StreamTarget::new(
            fm_model::StreamTargetId::new(id.get()),
            self.name.clone(),
            self.protocol,
            endpoint,
            key,
            self.output,
        )
        .and_then(|target| target.with_backup_endpoint(backup))
        .map_err(target_error)?
        .with_video_bitrate(self.video_bitrate_kbps)
        .map_err(|error| invalid("video_bitrate_kbps", "invalid_bitrate", error.to_string()))?;
        let ladder = self
            .ladder
            .clone()
            .filter(|_| self.protocol == StreamProtocol::Hls);
        let target = target
            .with_ladder(ladder)
            .and_then(|target| match self.srt_latency_ms {
                Some(latency) => target.with_srt_latency_ms(latency),
                None if self.protocol == StreamProtocol::Srt => {
                    Err(StreamTargetError::SrtLatencyOutOfRange)
                }
                None => Ok(target),
            })
            .map_err(target_error)?
            .with_srt_passphrase(passphrase)
            .map_err(|error| {
                invalid("srt_passphrase", "invalid_stream_target", error.to_string())
            })?;
        Ok(target.with_startup(self.startup).with_reconnect(reconnect))
    }
}

fn running_rejection(target: StreamTargetId) -> Rejection {
    Rejection::new(
        RejectionCode::Conflict,
        format!("stream target {target} is running; stop it before changing it"),
    )
    .with_field(FieldIssue::new(
        "target",
        "stream_running",
        "stop the destination before editing or removing it",
    ))
}

fn unknown_rejection(target: StreamTargetId) -> Rejection {
    Rejection::new(
        RejectionCode::NotFound,
        format!("stream target {target} is not part of this show"),
    )
    .with_field(FieldIssue::new(
        "target",
        "unknown_stream_target",
        "the destination does not exist",
    ))
}

/// Validates and applies one destination edit to the desired show,
/// returning the resulting configuration (`None` for a removal). `position`
/// places an added or edited destination at that index of the authored list.
pub(crate) fn apply(
    state: &mut ShowState,
    target: StreamTargetId,
    spec: Option<&EngineStreamTargetSpec>,
    position: Option<usize>,
) -> Result<Option<StreamTarget>, Rejection> {
    let config = spec.map(|spec| spec.build(target)).transpose()?;
    let config = config.as_ref();
    let running = state.stream_running(target);
    if running == Some(true) {
        return Err(running_rejection(target));
    }
    if let Some(config) = config {
        if running.is_none() && state.streams().len() >= MAX_STREAM_COUNT {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                format!(
                    "the show already has {MAX_STREAM_COUNT} stream destinations, the most \
                         it may hold"
                ),
            )
            .with_field(FieldIssue::new(
                "target",
                "too_many_streams",
                "remove a destination before adding another",
            )));
        }
        if !state
            .outputs()
            .iter()
            .any(|(output, _)| *output == config.output())
        {
            return Err(Rejection::new(
                RejectionCode::NotFound,
                format!("show does not contain output {}", config.output()),
            )
            .with_reason(RefusalReason::UnknownId {
                kind: IdKind::Output,
                id: config.output().get().get(),
            }));
        }
        if state.streams().iter().any(|stream| {
            stream.id() != target && stream.name().eq_ignore_ascii_case(config.name())
        }) {
            return Err(Rejection::new(
                RejectionCode::Conflict,
                "another stream destination already uses this name".to_owned(),
            )
            .with_field(FieldIssue::new(
                "name",
                "duplicate_name",
                "destination names must be unique",
            )));
        }
    } else {
        if running.is_none() {
            return Err(unknown_rejection(target));
        }
        if let Some(list) = state.cue_lists().iter().find(|list| {
            list.cues.iter().any(|cue| {
                cue.actions.iter().any(|action| {
                    matches!(action, CueAction::Stream { target: used, .. }
                            if used.get() == target.get())
                })
            })
        }) {
            return Err(Rejection::new(
                RejectionCode::Conflict,
                format!(
                    "stream target {target} is started or stopped by cue list {}",
                    list.id
                ),
            )
            .with_field(FieldIssue::new(
                "target",
                "stream_in_use",
                "remove the destination from every cue list first",
            )));
        }
    }
    state
        .set_stream_target_unchecked(target, config.cloned(), position)
        .map_err(|error| Rejection::new(RejectionCode::InvalidCommand, error.to_string()))?;
    Ok(config.cloned())
}
