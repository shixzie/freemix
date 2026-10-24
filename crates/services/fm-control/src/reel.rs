//! Highlight reels between the engine and the wire (OR-013).

use fm_auth::CommandClass;
use fm_engine::{
    EngineCommand, EngineEvent, EngineReelExportState, EngineReelOperation, EngineReelStatus,
};
use fm_protocol::{
    CommandPayload, EventPayload, ReelExportState, ReelExportStatus, ReelOperation, ReelSpec,
    ReelStatus, ReelStatusMessage, ServerIdentity,
};

/// The engine operation of a wire reel command.
pub(super) fn engine_operation(operation: &ReelOperation) -> EngineReelOperation {
    match operation {
        ReelOperation::Set(spec) => EngineReelOperation::Set(spec.clone()),
        ReelOperation::Delete { reel } => EngineReelOperation::Delete { reel: *reel },
        ReelOperation::Export {
            job,
            reel,
            destination,
        } => EngineReelOperation::Export {
            job: *job,
            reel: *reel,
            destination: destination.clone(),
        },
        ReelOperation::CancelExport { job } => EngineReelOperation::CancelExport { job: *job },
        ReelOperation::ForgetExport { job } => EngineReelOperation::ForgetExport { job: *job },
    }
}

/// The wire form of an engine reel operation.
pub(super) fn wire_operation(operation: &EngineReelOperation) -> ReelOperation {
    match operation {
        EngineReelOperation::Set(spec) => ReelOperation::Set(spec.clone()),
        EngineReelOperation::Delete { reel } => ReelOperation::Delete { reel: *reel },
        EngineReelOperation::Export {
            job,
            reel,
            destination,
        } => ReelOperation::Export {
            job: *job,
            reel: *reel,
            destination: destination.clone(),
        },
        EngineReelOperation::CancelExport { job } => ReelOperation::CancelExport { job: *job },
        EngineReelOperation::ForgetExport { job } => ReelOperation::ForgetExport { job: *job },
    }
}

/// The wire payload of a reel command (for the journal and history).
pub(super) fn wire_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::Reel(operation) = command else {
        return None;
    };
    Some(CommandPayload::Reel(wire_operation(operation)))
}

/// The durable event an accepted reel command publishes.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::ReelChanged { operation } = event else {
        return None;
    };
    match operation {
        EngineReelOperation::Set(spec) => Some(EventPayload::ReelChanged(Box::new(ReelStatus {
            id: spec.id,
            spec: spec.as_ref().clone(),
            total_frames: None,
            error: None,
        }))),
        EngineReelOperation::Delete { reel } => Some(EventPayload::ReelRemoved { reel: *reel }),
        EngineReelOperation::Export { .. }
        | EngineReelOperation::CancelExport { .. }
        | EngineReelOperation::ForgetExport { .. } => None,
    }
}

/// The target-free class of a reel command: a reel edit is a project edit,
/// export control is an operator control like a media transport (OR-013).
pub(super) const fn command_class(operation: &ReelOperation) -> CommandClass {
    if operation.is_live() {
        CommandClass::Transition
    } else {
        CommandClass::EditProject
    }
}

/// The runtime (and journal) domain of a reel command (OR-013): a reel edit
/// is a project edit, export control is live.
pub(super) const fn journal_channel(operation: &EngineReelOperation) -> &'static str {
    if operation.is_live() {
        "reel"
    } else {
        "project"
    }
}

/// The reel status as clients see it.
///
/// The engine status summarises each reel by id; the wire carries the whole
/// stored definition, which the caller reads from the show.
pub(super) fn status_message(
    status: EngineReelStatus,
    reels: &[ReelSpec],
    server: &ServerIdentity,
    sequence: u64,
    request_id: Option<String>,
) -> ReelStatusMessage {
    let EngineReelStatus {
        reels: summaries,
        exports,
    } = status;
    ReelStatusMessage {
        server: server.clone(),
        sequence,
        request_id,
        reels: reels
            .iter()
            .map(|spec| {
                let summary = summaries.iter().find(|summary| summary.id == spec.id);
                ReelStatus {
                    id: spec.id,
                    spec: spec.clone(),
                    total_frames: summary.and_then(|summary| summary.total_frames),
                    error: summary.and_then(|summary| summary.error.clone()),
                }
            })
            .collect(),
        exports: exports
            .into_iter()
            .map(|export| ReelExportStatus {
                job: export.job,
                reel: export.reel,
                destination: export.destination,
                state: export_state(export.state),
                completed_frames: export.completed_frames,
                total_frames: Some(export.total_frames),
                bytes: export.bytes,
                failure: export.failure,
            })
            .collect(),
    }
}

/// The wire lifecycle of an engine export job.
const fn export_state(state: EngineReelExportState) -> ReelExportState {
    match state {
        EngineReelExportState::Queued => ReelExportState::Queued,
        EngineReelExportState::Running => ReelExportState::Running,
        EngineReelExportState::Finalizing => ReelExportState::Finalizing,
        EngineReelExportState::Completed => ReelExportState::Completed,
        EngineReelExportState::Failed => ReelExportState::Failed,
        EngineReelExportState::Cancelled => ReelExportState::Cancelled,
    }
}
