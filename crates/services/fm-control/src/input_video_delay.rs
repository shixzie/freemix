//! Per-input video delays between the engine and the wire (IN-022).

use fm_engine::{EngineCommand, EngineEvent, ShowState};
use fm_protocol::{CommandPayload, EventPayload, WireInputVideoDelay};

/// The engine command of an input-video-delay payload.
pub(super) fn engine_command(payload: &CommandPayload) -> Option<EngineCommand> {
    let CommandPayload::SetInputVideoDelay { input, frames } = payload else {
        return None;
    };
    Some(EngineCommand::SetInputVideoDelay {
        input: input.to_domain(),
        frames: *frames,
    })
}

/// The durable event of an input-video-delay edit, or of its undo or redo.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::InputVideoDelayChanged {
        input,
        frames,
        audio_delay_samples,
    } = event
    else {
        return None;
    };
    Some(EventPayload::InputVideoDelayChanged {
        input: fm_protocol::WireInputId::from_domain(*input),
        frames: *frames,
        audio_delay_samples: *audio_delay_samples,
    })
}

/// The wire command of an undoable input-video-delay edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetInputVideoDelay { input, frames } = command else {
        return None;
    };
    Some(CommandPayload::SetInputVideoDelay {
        input: fm_protocol::WireInputId::from_domain(*input),
        frames: *frames,
    })
}

/// Every input's video delay in ascending input order.
pub(super) fn wire_entries(show: &ShowState) -> Vec<WireInputVideoDelay> {
    show.input_video_delays()
        .map(|(&input, &frames)| WireInputVideoDelay {
            input: fm_protocol::WireInputId::from_domain(input),
            frames,
        })
        .collect()
}
