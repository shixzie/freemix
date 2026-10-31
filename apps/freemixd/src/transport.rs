//! Receipts and the control transport: persisted and runtime idempotency
//! receipts, refusal and session-error mapping, the diagnostics answer, the
//! line writer and its bounded pending write, the message reader, and the
//! client error classifiers.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

pub(crate) fn persisted_engine_receipt(
    (key, receipt): &(IdempotencyKey, CommandReceipt<EngineAcceptance>),
) -> IdempotencyReceipt {
    match receipt {
        CommandReceipt::Accepted {
            command_id,
            acceptance,
        } => IdempotencyReceipt::accepted(
            key.as_str(),
            command_id.as_str(),
            acceptance.revision.get(),
            acceptance.result.target_frame.get(),
        ),
        CommandReceipt::Rejected {
            command_id,
            rejection,
        } => IdempotencyReceipt::rejected(
            key.as_str(),
            command_id.as_str(),
            rejection.current_revision.get(),
            rejection.rejection.code.as_str(),
            &rejection.rejection.message,
            rejection.rejection.retryable,
        ),
    }
}

pub(crate) fn persisted_result(
    command: &CommandMessage,
    result: &CommandResult,
) -> AppResult<IdempotencyReceipt> {
    match result {
        CommandResult::Accepted {
            id,
            revision,
            scheduled_frame: Some(frame),
        } => Ok(IdempotencyReceipt::accepted(
            &command.idempotency_key,
            id,
            *revision,
            *frame,
        )),
        CommandResult::Accepted {
            scheduled_frame: None,
            ..
        } => Err(AppFailure("accepted engine command has no scheduled frame".into()).into()),
        CommandResult::Rejected {
            id,
            code,
            message,
            current_revision,
            retryable,
            ..
        } => Ok(IdempotencyReceipt::rejected(
            &command.idempotency_key,
            id,
            *current_revision,
            code,
            message,
            *retryable,
        )),
    }
}

pub(crate) fn runtime_receipt(
    receipt: &IdempotencyReceipt,
) -> AppResult<(IdempotencyKey, CommandReceipt<EngineAcceptance>)> {
    let command_id = CommandId::new(receipt.command_id());
    let runtime = match receipt.outcome() {
        ReceiptOutcome::Accepted {
            revision,
            target_frame,
        } => CommandReceipt::Accepted {
            command_id,
            acceptance: AcceptedReceipt {
                revision: Revision::new(*revision),
                result: EngineAcceptance {
                    target_frame: (*target_frame).into(),
                },
            },
        },
        ReceiptOutcome::Rejected {
            current_revision,
            code,
            message,
            retryable,
        } => CommandReceipt::Rejected {
            command_id,
            rejection: RejectedReceipt {
                rejection: {
                    let code = runtime_rejection_code(code)?;
                    // Persisted receipts keep code and message; the reasons
                    // a code alone implies are restored (Protocol 2.45).
                    let rejection = Rejection::new(code, message).retryable(*retryable);
                    match fm_command::RefusalReason::implied_by(code, *current_revision) {
                        Some(reason) => rejection.with_reason(reason),
                        None => rejection,
                    }
                },
                current_revision: Revision::new(*current_revision),
            },
        },
    };
    Ok((IdempotencyKey::new(receipt.key()), runtime))
}

pub(crate) fn runtime_rejection_code(code: &str) -> AppResult<RejectionCode> {
    match code {
        "permission_denied" => Ok(RejectionCode::PermissionDenied),
        "deadline_exceeded" => Ok(RejectionCode::DeadlineExceeded),
        "revision_conflict" => Ok(RejectionCode::RevisionConflict),
        "invalid_command" => Ok(RejectionCode::InvalidCommand),
        "not_found" => Ok(RejectionCode::NotFound),
        "conflict" => Ok(RejectionCode::Conflict),
        "unavailable" => Ok(RejectionCode::Unavailable),
        "resource_exhausted" => Ok(RejectionCode::ResourceExhausted),
        "internal" => Ok(RejectionCode::Internal),
        _ => Err(AppFailure(format!("project contains unknown rejection code `{code}`")).into()),
    }
}

pub(crate) fn session_rejection(
    command: &CommandMessage,
    error: &SessionError,
    current_revision: u64,
) -> CommandResult {
    let (code, retryable, reason) = match error {
        SessionError::Authorization(_) => (
            "permission_denied",
            false,
            Some(fm_protocol::RefusalReason::PermissionDenied),
        ),
        SessionError::ProtocolMismatch { .. } => ("protocol_mismatch", false, None),
        SessionError::CommandTooLarge { .. }
        | SessionError::TooManyInflightCommands
        | SessionError::InboundRateLimited
        | SessionError::TitlePreviewRateLimited
        | SessionError::OutboundRateLimited => (
            "resource_exhausted",
            true,
            Some(fm_protocol::RefusalReason::RateLimited),
        ),
        _ => ("session_error", false, None),
    };
    CommandResult::Rejected {
        id: command.id.clone(),
        code: code.into(),
        message: error.to_string(),
        fields: Vec::new(),
        current_revision,
        retryable,
        reason,
    }
}

pub(crate) fn wall_now() -> fm_clock::production::UtcInstant {
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    fm_clock::production::UtcInstant::new(
        i64::try_from(wall.as_secs()).unwrap_or(i64::MAX),
        wall.subsec_nanos(),
    )
    .unwrap_or(fm_clock::production::UtcInstant::UNIX_EPOCH)
}

/// Applies one client production-clock control to the live registry and
/// answers with the clock's reading afterwards or the engine's refusal
/// (SW-013, Protocol 2.150).
pub(crate) fn clock_control_response(
    control: &SharedControl,
    request: fm_protocol::ClockControlRequest,
) -> fm_protocol::ClockControlResponse {
    let result =
        control
            .borrow_mut()
            .control_production_clock(wall_now(), &request.name, request.operation);
    let (accepted, error, reading) = match result {
        Ok(reading) => (true, None, Some(reading)),
        Err(error) => (false, Some(error), None),
    };
    fm_protocol::ClockControlResponse {
        protocol: request.protocol,
        request_id: request.request_id,
        name: request.name,
        accepted,
        error,
        reading,
    }
}

pub(crate) fn diagnostics_response(
    control: &SharedControl,
    request: DiagnosticsRequest,
    source_sync: Vec<fm_protocol::SourceSyncSample>,
    health: fm_protocol::DaemonHealth,
) -> AppResult<DiagnosticsResponse> {
    let diagnostics = control.borrow().diagnostics();
    let production_clocks = control.borrow().production_clock_statuses(wall_now());
    Ok(DiagnosticsResponse {
        protocol: request.protocol,
        request_id: request.request_id,
        engine: diagnostics.engine,
        current_revision: diagnostics.current_revision,
        oldest_retained_revision: diagnostics.oldest_retained_revision,
        newest_retained_revision: diagnostics.newest_retained_revision,
        subscriber_count: u32::try_from(diagnostics.subscriber_count)?,
        retained_events_limit: u32::try_from(diagnostics.limits.retained_events)?,
        subscriber_limit: u32::try_from(diagnostics.limits.max_subscribers)?,
        subscriber_queue_limit: u32::try_from(diagnostics.limits.subscriber_queue)?,
        source_sync,
        production_clocks,
        health,
    })
}

pub(crate) fn structured_session_error(error: &SessionError) -> StructuredError {
    let (code, retryable) = match error {
        SessionError::Authorization(_) => ("permission_denied", false),
        SessionError::ProtocolMismatch { .. } => ("protocol_mismatch", false),
        SessionError::CommandTooLarge { .. }
        | SessionError::InboundRateLimited
        | SessionError::TitlePreviewRateLimited
        | SessionError::OutboundRateLimited => ("resource_exhausted", true),
        _ => ("session_error", false),
    };
    StructuredError {
        code: code.into(),
        message: error.to_string(),
        fields: Vec::new(),
        retryable,
    }
}

#[cfg(test)]
pub(crate) fn write_session_message(
    writer: &mut TcpStream,
    session: &mut Session,
    message: &WireMessage,
) -> AppResult<()> {
    let line = encode_line(message)?;
    session.queue_outbound(line.len(), now_millis()?)?;
    write_client_bytes(writer, line.as_bytes())?;
    session.outbound_delivered()?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_session_error(
    writer: &mut TcpStream,
    session: &mut Session,
    code: &str,
    message: &str,
) -> AppResult<()> {
    write_session_message(writer, session, &error_message(code, message))
}

#[cfg(test)]
pub(crate) fn write_error(writer: &mut TcpStream, code: &str, message: &str) -> AppResult<()> {
    write_message(writer, &error_message(code, message))
}

pub(crate) fn error_message(code: &str, message: &str) -> WireMessage {
    WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: code.into(),
            message: message.into(),
            fields: Vec::new(),
            retryable: false,
        },
    })
}

pub(crate) fn shutdown_message() -> WireMessage {
    WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: "server_shutting_down".into(),
            message: "server is shutting down".into(),
            fields: Vec::new(),
            retryable: true,
        },
    })
}

#[cfg(test)]
pub(crate) fn write_message(writer: &mut TcpStream, message: &WireMessage) -> AppResult<()> {
    let line = encode_line(message)?;
    write_client_bytes(writer, line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_client_bytes(writer: &mut TcpStream, bytes: &[u8]) -> AppResult<()> {
    let mut pending = PendingWrite::new(bytes.to_vec());
    while !pending.write_once(writer).map_err(client_write_error)? {}
    writer.flush().map_err(client_write_error)?;
    Ok(())
}

pub(crate) struct PendingWrite {
    pub(crate) bytes: Vec<u8>,
    pub(crate) offset: usize,
}

impl PendingWrite {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        assert!(!bytes.is_empty(), "encoded records are nonempty");
        Self { bytes, offset: 0 }
    }

    pub(crate) fn write_once(&mut self, writer: &mut impl Write) -> std::io::Result<bool> {
        let written = writer.write(&self.bytes[self.offset..])?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        self.offset += written;
        Ok(self.offset == self.bytes.len())
    }

    pub(crate) fn started(&self) -> bool {
        self.offset != 0
    }
}

#[cfg(test)]
pub(crate) fn client_write_error(error: std::io::Error) -> Box<dyn Error> {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        Box::new(ClientWriteTimeout(error.kind()))
    } else {
        Box::new(error)
    }
}

#[test]
pub(crate) fn pending_write_preserves_offset_across_partial_writes() {
    struct PartialWriter {
        bytes: Vec<u8>,
        limit: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = bytes.len().min(self.limit);
            self.bytes.extend_from_slice(&bytes[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let expected = b"encoded record";
    let mut pending = PendingWrite::new(expected.to_vec());
    let mut writer = PartialWriter {
        bytes: Vec::new(),
        limit: 3,
    };

    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(pending.write_once(&mut writer).unwrap());
    assert_eq!(writer.bytes, expected);
}

pub(crate) fn handshake_code(error: &HandshakeError<Infallible>) -> &'static str {
    match error {
        HandshakeError::ProtocolMismatch => "protocol_mismatch",
        HandshakeError::NotReady(_) => "not_ready",
        HandshakeError::DevelopmentPrincipalDenied | HandshakeError::RoleDenied(_) => {
            "permission_denied"
        }
        HandshakeError::InvalidControlSync => "invalid_control_sync",
        HandshakeError::Control(never) => match *never {},
    }
}

#[cfg(test)]
pub(crate) struct MessageReader {
    pub(crate) stream: TcpStream,
    pub(crate) decoder: LineDecoder,
    pub(crate) pending: VecDeque<WireMessage>,
}

#[cfg(test)]
impl MessageReader {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            decoder: LineDecoder::new(),
            pending: VecDeque::new(),
        }
    }

    pub(crate) fn read_message_with_idle(
        &mut self,
        mut idle: impl FnMut() -> AppResult<bool>,
    ) -> AppResult<Option<WireMessage>> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(Some(message));
        }
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let read = match self.stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if !idle()? {
                        return Ok(None);
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if read == 0 {
                let decoder = std::mem::replace(&mut self.decoder, LineDecoder::new());
                decoder.finish().map_err(ClientProtocolError)?;
                return Ok(None);
            }
            self.pending.extend(
                self.decoder
                    .push(&chunk[..read])
                    .map_err(ClientProtocolError)?,
            );
            if let Some(message) = self.pending.pop_front() {
                return Ok(Some(message));
            }
            if !idle()? {
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn is_client_disconnect(error: &(dyn Error + 'static)) -> bool {
    if error.downcast_ref::<ClientWriteTimeout>().is_some() {
        return true;
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
        );
    }
    error.source().is_some_and(is_client_disconnect)
}

#[cfg(test)]
pub(crate) fn is_client_protocol_error(error: &(dyn Error + 'static)) -> bool {
    error.downcast_ref::<ClientProtocolError>().is_some()
}

pub(crate) fn is_client_session_termination(error: &(dyn Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<SessionError>() {
        return matches!(
            error,
            SessionError::OutboundRateLimited
                | SessionError::Disconnected(DisconnectReason::SlowClient)
        );
    }
    error.source().is_some_and(is_client_session_termination)
}
