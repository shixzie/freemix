//! Media-file playback on the wire (IN-003, Protocol 2.37): the `media_*`
//! commands, the `media_changed` event, the `media_state` follow-on records,
//! and the `media` runtime events.

use fm_engine::fm_playback::{MediaEvent as PlaybackEvent, MediaState};
use fm_engine::{
    Engine, EngineCommand, EngineMediaCommand, EngineMediaFault, EngineMediaFrame,
    EngineMediaOperation, EngineMediaSeek, EngineMediaStatus,
};
use fm_model::{MediaAudioSpeedPolicy, MediaEndAction as ModelEndAction};
use fm_protocol::{
    EventPayload, MAX_MEDIA_EVENTS, MediaAudioPolicy, MediaAudioTrack, MediaEndAction, MediaEvent,
    MediaFault, MediaGrowingStatus, MediaOperation, MediaSeekTarget, MediaSettingsStatus,
    MediaStateMessage, MediaStatus, MediaTime, MediaTransportState, MediaTransportStatus,
    WireInputId,
};
use fm_types::InputId;

const fn wire_end_action(action: ModelEndAction) -> MediaEndAction {
    match action {
        ModelEndAction::Stop => MediaEndAction::Stop,
        ModelEndAction::Hold => MediaEndAction::Hold,
        ModelEndAction::Loop => MediaEndAction::Loop,
    }
}

const fn domain_end_action(action: MediaEndAction) -> ModelEndAction {
    match action {
        MediaEndAction::Stop => ModelEndAction::Stop,
        MediaEndAction::Hold => ModelEndAction::Hold,
        MediaEndAction::Loop => ModelEndAction::Loop,
    }
}

const fn wire_audio_policy(policy: MediaAudioSpeedPolicy) -> MediaAudioPolicy {
    match policy {
        MediaAudioSpeedPolicy::Mute => MediaAudioPolicy::Mute,
        MediaAudioSpeedPolicy::Resample => MediaAudioPolicy::Resample,
    }
}

const fn domain_audio_policy(policy: MediaAudioPolicy) -> MediaAudioSpeedPolicy {
    match policy {
        MediaAudioPolicy::Mute => MediaAudioSpeedPolicy::Mute,
        MediaAudioPolicy::Resample => MediaAudioSpeedPolicy::Resample,
    }
}

/// The wire form of an engine media fault.
#[must_use]
pub const fn wire_media_fault(fault: EngineMediaFault) -> MediaFault {
    match fault {
        EngineMediaFault::SourceMissing => MediaFault::SourceMissing,
        EngineMediaFault::SourceTruncated => MediaFault::SourceTruncated,
        EngineMediaFault::SourceReplaced => MediaFault::SourceReplaced,
        EngineMediaFault::SourceRewritten => MediaFault::SourceRewritten,
        EngineMediaFault::SourceChanged => MediaFault::SourceChanged,
        EngineMediaFault::Unsupported => MediaFault::Unsupported,
        EngineMediaFault::DecodeFailed => MediaFault::DecodeFailed,
        EngineMediaFault::ToolUnavailable => MediaFault::ToolUnavailable,
    }
}

/// The engine form of a wire media operation.
#[must_use]
pub const fn domain_media_operation(operation: MediaOperation) -> EngineMediaOperation {
    match operation {
        MediaOperation::Play => EngineMediaOperation::Play,
        MediaOperation::Pause => EngineMediaOperation::Pause,
        MediaOperation::Stop => EngineMediaOperation::Stop,
        MediaOperation::Seek(MediaSeekTarget::Frame(frame)) => {
            EngineMediaOperation::Seek(EngineMediaSeek::Frame(frame))
        }
        MediaOperation::Seek(MediaSeekTarget::Time {
            numerator,
            denominator,
        }) => EngineMediaOperation::Seek(EngineMediaSeek::Time {
            numerator,
            denominator,
        }),
        MediaOperation::CompleteSource => EngineMediaOperation::CompleteSource,
        MediaOperation::Trim { trim_in, trim_out } => {
            EngineMediaOperation::Trim { trim_in, trim_out }
        }
        MediaOperation::Rate { speed_basis_points } => {
            EngineMediaOperation::Rate { speed_basis_points }
        }
        MediaOperation::EndAction(action) => {
            EngineMediaOperation::EndAction(domain_end_action(action))
        }
        MediaOperation::EndTriggerLead(lead) => EngineMediaOperation::EndTriggerLead(lead),
        MediaOperation::AudioTrack(track) => EngineMediaOperation::AudioTrack(track),
        MediaOperation::AudioPolicy(policy) => {
            EngineMediaOperation::AudioPolicy(domain_audio_policy(policy))
        }
    }
}

/// The wire form of an engine media operation.
#[must_use]
pub const fn wire_media_operation(operation: EngineMediaOperation) -> MediaOperation {
    match operation {
        EngineMediaOperation::Play => MediaOperation::Play,
        EngineMediaOperation::Pause => MediaOperation::Pause,
        EngineMediaOperation::Stop => MediaOperation::Stop,
        EngineMediaOperation::Seek(EngineMediaSeek::Frame(frame)) => {
            MediaOperation::Seek(MediaSeekTarget::Frame(frame))
        }
        EngineMediaOperation::Seek(EngineMediaSeek::Time {
            numerator,
            denominator,
        }) => MediaOperation::Seek(MediaSeekTarget::Time {
            numerator,
            denominator,
        }),
        EngineMediaOperation::CompleteSource => MediaOperation::CompleteSource,
        EngineMediaOperation::Trim { trim_in, trim_out } => {
            MediaOperation::Trim { trim_in, trim_out }
        }
        EngineMediaOperation::Rate { speed_basis_points } => {
            MediaOperation::Rate { speed_basis_points }
        }
        EngineMediaOperation::EndAction(action) => {
            MediaOperation::EndAction(wire_end_action(action))
        }
        EngineMediaOperation::EndTriggerLead(lead) => MediaOperation::EndTriggerLead(lead),
        EngineMediaOperation::AudioTrack(track) => MediaOperation::AudioTrack(track),
        EngineMediaOperation::AudioPolicy(policy) => {
            MediaOperation::AudioPolicy(wire_audio_policy(policy))
        }
    }
}

pub(crate) fn media_engine_command(input: WireInputId, operation: MediaOperation) -> EngineCommand {
    EngineCommand::Media(EngineMediaCommand {
        input: input.to_domain(),
        operation: domain_media_operation(operation),
    })
}

const fn wire_state(state: MediaState) -> MediaTransportState {
    match state {
        MediaState::Stopped => MediaTransportState::Stopped,
        MediaState::Playing => MediaTransportState::Playing,
        MediaState::Paused => MediaTransportState::Paused,
        MediaState::Waiting => MediaTransportState::Waiting,
        MediaState::Ended => MediaTransportState::Ended,
    }
}

/// The wire form of a media input's status.
#[must_use]
pub fn wire_media_status(status: &EngineMediaStatus) -> MediaStatus {
    let settings = &status.settings;
    MediaStatus {
        input: WireInputId::from_domain(status.input),
        settings: MediaSettingsStatus {
            trim_in: settings.trim_in,
            trim_out: settings.trim_out,
            end_action: wire_end_action(settings.end_action),
            end_trigger_lead: settings.end_trigger_lead,
            speed_basis_points: settings.speed_basis_points,
            audio_policy: wire_audio_policy(settings.audio_policy),
            audio_stream: settings.audio_stream,
            growing: settings.growing.map(|growing| MediaGrowingStatus {
                live_edge_offset_frames: growing.live_edge_offset_frames,
                idle_completion_refreshes: growing.idle_completion_refreshes,
            }),
            start_frame: settings.start_frame,
        },
        transport: MediaTransportStatus {
            available: status.available,
            state: wire_state(status.state),
            position: status.position,
            position_time: MediaTime {
                numerator: status.position_time.numerator(),
                denominator: status.position_time.denominator(),
            }
            .reduced(),
            playable_end: status.playable_end,
            frames: status.frames,
            complete: status.complete,
            loops: status.loops,
            audio_track: status.audio_track,
            tracks: status
                .tracks
                .iter()
                .map(|track| MediaAudioTrack {
                    stream_index: track.stream_index,
                    sample_rate: track.sample_rate,
                    channels: track.channels,
                    language: track.language.clone(),
                    title: track.title.clone(),
                    default: track.default,
                })
                .collect(),
            source_rate: status.source_rate,
            fault: status.fault.map(wire_media_fault),
        },
    }
}

fn status(engine: &Engine, input: InputId) -> MediaStatus {
    wire_media_status(
        &engine
            .media_status(input)
            .expect("media events name media inputs"),
    )
}

/// The `media_changed` event for an accepted media command.
pub(crate) fn media_changed(
    engine: &Engine,
    input: InputId,
    frame: u64,
    operation: EngineMediaOperation,
) -> EventPayload {
    EventPayload::MediaChanged {
        frame,
        operation: wire_media_operation(operation),
        status: Box::new(status(engine, input)),
    }
}

/// How many `media_state` records follow a snapshot of `engine`.
pub(crate) fn media_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().media_inputs().len()).expect("show inputs are bounded")
}

/// The `media_state` records of a snapshot of `engine`, ascending by input.
pub(crate) fn media_states(engine: &Engine) -> Vec<MediaStateMessage> {
    let revision = engine.revision().get();
    let frame = engine.frame_cursor().get();
    engine
        .media_statuses()
        .iter()
        .map(|status| MediaStateMessage {
            revision,
            frame,
            status: wire_media_status(status),
        })
        .collect()
}

fn wire_event(event: &PlaybackEvent) -> MediaEvent {
    match event {
        PlaybackEvent::Seeked { frame } => MediaEvent::Seeked { frame: frame.get() },
        PlaybackEvent::EndTrigger { remaining_frames } => MediaEvent::EndTrigger {
            remaining_frames: *remaining_frames,
        },
        PlaybackEvent::Ended { action } => MediaEvent::Ended {
            action: match action {
                fm_engine::fm_playback::MediaEndAction::Stop => MediaEndAction::Stop,
                fm_engine::fm_playback::MediaEndAction::HoldLastFrame => MediaEndAction::Hold,
                fm_engine::fm_playback::MediaEndAction::Loop => MediaEndAction::Loop,
            },
        },
        PlaybackEvent::Looped { count } => MediaEvent::Looped { count: *count },
        PlaybackEvent::LiveEdgeReached { frame } => {
            MediaEvent::LiveEdgeReached { frame: frame.get() }
        }
        PlaybackEvent::LiveEdgeResumed { frame } => MediaEvent::Resumed { frame: frame.get() },
        PlaybackEvent::SourceGrew { available_frames } => MediaEvent::SourceGrew {
            frames: *available_frames,
        },
        PlaybackEvent::SourceCompleted { frames } => {
            MediaEvent::SourceCompleted { frames: *frames }
        }
        PlaybackEvent::AudioTrackChanged { from, to } => MediaEvent::AudioTrackChanged {
            from: *from,
            to: *to,
        },
    }
}

/// The wire events of one media frame: the transport's events, then a
/// fault, at most [`MAX_MEDIA_EVENTS`] (a growing source reports at most its
/// latest growth).
#[must_use]
pub fn wire_media_events(frame: &EngineMediaFrame) -> Vec<MediaEvent> {
    let mut events: Vec<MediaEvent> = Vec::new();
    for event in frame.tick.iter().flat_map(|tick| tick.events.iter()) {
        let event = wire_event(event);
        if let MediaEvent::SourceGrew { .. } = event {
            events.retain(|kept| !matches!(kept, MediaEvent::SourceGrew { .. }));
        }
        events.push(event);
    }
    if let Some(fault) = frame.faulted {
        events.push(MediaEvent::Faulted {
            fault: wire_media_fault(fault),
        });
    }
    if events.len() > MAX_MEDIA_EVENTS {
        events.drain(..events.len() - MAX_MEDIA_EVENTS);
    }
    events
}

/// Whether a media frame is published as a `media` runtime event: when
/// something happened (an event, a realized command, a fault, a state
/// change) or, while the transport advances, once a second as a position
/// sample.
pub(crate) fn publishes(
    frame: &EngineMediaFrame,
    previous_state: Option<MediaState>,
    frame_number: u64,
    frames_per_second: u64,
) -> bool {
    let state = frame.tick.as_ref().map(|tick| tick.state);
    frame.faulted.is_some()
        || !frame.operations.is_empty()
        || frame
            .tick
            .as_ref()
            .is_some_and(|tick| !tick.events.is_empty())
        || state != previous_state
        || (state.is_some_and(|state| matches!(state, MediaState::Playing | MediaState::Waiting))
            && frame_number.is_multiple_of(frames_per_second.max(1)))
}

/// The status a `media` runtime event carries for `input`.
pub(crate) fn runtime_status(engine: &Engine, input: InputId) -> MediaStatus {
    status(engine, input)
}
