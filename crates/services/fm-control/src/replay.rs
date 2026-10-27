//! Instant replay between the engine and the wire (OR-012).

use fm_engine::{
    EngineCommand, EngineEvent, EngineReplayFeed, EngineReplayOperation, EngineReplayStatus,
};
use fm_protocol::{
    CommandPayload, EventPayload, ReplayEventStatus, ReplayFeed, ReplayOperation,
    ReplayPlayerStatus, ReplaySourceStatus, ReplayStatusMessage, ServerIdentity, WireInputId,
};

/// The engine operation of a wire replay command.
pub(super) fn engine_operation(operation: &ReplayOperation) -> EngineReplayOperation {
    match operation {
        ReplayOperation::Configure(settings) => EngineReplayOperation::Configure(settings.clone()),
        ReplayOperation::MarkIn => EngineReplayOperation::MarkIn,
        ReplayOperation::MarkOut => EngineReplayOperation::MarkOut,
        ReplayOperation::CaptureMarks { name } => {
            EngineReplayOperation::CaptureMarks { name: name.clone() }
        }
        ReplayOperation::CaptureLast { seconds } => {
            EngineReplayOperation::CaptureLast { seconds: *seconds }
        }
        ReplayOperation::RenameEvent { event, name } => EngineReplayOperation::RenameEvent {
            event: *event,
            name: name.clone(),
        },
        ReplayOperation::DeleteEvent { event } => {
            EngineReplayOperation::DeleteEvent { event: *event }
        }
        ReplayOperation::Cue {
            input,
            event,
            angle,
        } => EngineReplayOperation::Cue {
            input: input.to_domain(),
            event: *event,
            angle: angle.map(WireInputId::to_domain),
        },
        ReplayOperation::Angle { input, angle } => EngineReplayOperation::Angle {
            input: input.to_domain(),
            angle: angle.to_domain(),
        },
        ReplayOperation::Play { input, speed } => EngineReplayOperation::Play {
            input: input.to_domain(),
            speed: *speed,
        },
        ReplayOperation::Pause { input } => EngineReplayOperation::Pause {
            input: input.to_domain(),
        },
        ReplayOperation::CueIn { input } => EngineReplayOperation::CueIn {
            input: input.to_domain(),
        },
        ReplayOperation::Step { input, frames } => EngineReplayOperation::Step {
            input: input.to_domain(),
            frames: *frames,
        },
    }
}

/// The wire form of an engine replay operation.
pub(super) fn wire_operation(operation: &EngineReplayOperation) -> ReplayOperation {
    match operation {
        EngineReplayOperation::Configure(settings) => ReplayOperation::Configure(settings.clone()),
        EngineReplayOperation::MarkIn => ReplayOperation::MarkIn,
        EngineReplayOperation::MarkOut => ReplayOperation::MarkOut,
        EngineReplayOperation::CaptureMarks { name } => {
            ReplayOperation::CaptureMarks { name: name.clone() }
        }
        EngineReplayOperation::CaptureLast { seconds } => {
            ReplayOperation::CaptureLast { seconds: *seconds }
        }
        EngineReplayOperation::RenameEvent { event, name } => ReplayOperation::RenameEvent {
            event: *event,
            name: name.clone(),
        },
        EngineReplayOperation::DeleteEvent { event } => {
            ReplayOperation::DeleteEvent { event: *event }
        }
        EngineReplayOperation::Cue {
            input,
            event,
            angle,
        } => ReplayOperation::Cue {
            input: WireInputId::from_domain(*input),
            event: *event,
            angle: angle.map(WireInputId::from_domain),
        },
        EngineReplayOperation::Angle { input, angle } => ReplayOperation::Angle {
            input: WireInputId::from_domain(*input),
            angle: WireInputId::from_domain(*angle),
        },
        EngineReplayOperation::Play { input, speed } => ReplayOperation::Play {
            input: WireInputId::from_domain(*input),
            speed: *speed,
        },
        EngineReplayOperation::Pause { input } => ReplayOperation::Pause {
            input: WireInputId::from_domain(*input),
        },
        EngineReplayOperation::CueIn { input } => ReplayOperation::CueIn {
            input: WireInputId::from_domain(*input),
        },
        EngineReplayOperation::Step { input, frames } => ReplayOperation::Step {
            input: WireInputId::from_domain(*input),
            frames: *frames,
        },
    }
}

/// The wire payload of a replay command (for the journal and history).
pub(super) fn wire_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::Replay(operation) = command else {
        return None;
    };
    Some(CommandPayload::Replay(wire_operation(operation)))
}

/// The durable event an accepted replay command publishes.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::ReplayChanged { frame, operation } = event else {
        return None;
    };
    Some(EventPayload::ReplayChanged {
        frame: *frame,
        operation: wire_operation(operation),
    })
}

/// The replay status as clients see it.
pub(super) fn status_message(
    status: EngineReplayStatus,
    server: &ServerIdentity,
    sequence: u64,
    request_id: Option<String>,
) -> ReplayStatusMessage {
    ReplayStatusMessage {
        server: server.clone(),
        sequence,
        request_id,
        feed: match status.feed {
            EngineReplayFeed::Synthetic => ReplayFeed::Synthetic,
            EngineReplayFeed::External => ReplayFeed::Recorded,
        },
        settings: status.settings,
        root: status.storage_root,
        live_frame: status.live_frame,
        mark_in: status.mark_in,
        mark_out: status.mark_out,
        recorded: status
            .sources
            .into_iter()
            .map(|source| ReplaySourceStatus {
                input: WireInputId::from_domain(source.input),
                segments: source.segments,
                bytes: source.bytes,
                first_frame: source.first_frame,
                last_frame: source.last_frame,
            })
            .collect(),
        events: status
            .events
            .into_iter()
            .map(|event| ReplayEventStatus {
                id: event.id,
                name: event.name,
                in_frame: event.in_frame,
                out_frame: event.out_frame,
                angles: event
                    .angles
                    .into_iter()
                    .map(WireInputId::from_domain)
                    .collect(),
                complete: event.complete,
            })
            .collect(),
        players: status
            .players
            .into_iter()
            .map(|player| ReplayPlayerStatus {
                input: WireInputId::from_domain(player.input),
                event: player.event,
                angle: player.angle.map(WireInputId::from_domain),
                speed: player.speed,
                playing: player.playing,
                frame: player.frame,
            })
            .collect(),
    }
}
