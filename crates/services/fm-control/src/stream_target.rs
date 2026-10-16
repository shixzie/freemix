//! Stream destinations between the engine and the wire (Protocol 2.52,
//! OR-006).
//!
//! Everything this module reports carries the destination's configuration
//! without its stream key: only whether one is stored.

use fm_engine::{
    EngineCommand, EngineEvent, EngineStreamControl, EngineStreamTargetSpec, ShowState,
};
use fm_model::{StartupPolicy, StreamProtocol, StreamTarget};
use fm_protocol::{
    CommandPayload, EventPayload, SecretEdit, StreamReconnect, StreamTargetConfig,
    StreamTargetEntry, StreamTargetProtocol, WireOutputId, WireStreamTargetId,
};
use fm_switcher::StreamTargetId;

const fn domain_target(target: WireStreamTargetId) -> StreamTargetId {
    StreamTargetId::from_non_zero(target.get())
}

const fn model_protocol(protocol: StreamTargetProtocol) -> StreamProtocol {
    match protocol {
        StreamTargetProtocol::Rtmp => StreamProtocol::Rtmp,
        StreamTargetProtocol::Rtmps => StreamProtocol::Rtmps,
        StreamTargetProtocol::Srt => StreamProtocol::Srt,
        StreamTargetProtocol::Hls => StreamProtocol::Hls,
    }
}

const fn wire_protocol(protocol: StreamProtocol) -> StreamTargetProtocol {
    match protocol {
        StreamProtocol::Rtmp => StreamTargetProtocol::Rtmp,
        StreamProtocol::Rtmps => StreamTargetProtocol::Rtmps,
        StreamProtocol::Srt => StreamTargetProtocol::Srt,
        StreamProtocol::Hls => StreamTargetProtocol::Hls,
    }
}

/// The engine command for a wire stream group, retry, or destination edit,
/// if `payload` is one. A destination edit resolves its stream-key edit
/// against the stored key and keeps an authored HLS ladder.
pub(super) fn engine_command(show: &ShowState, payload: &CommandPayload) -> Option<EngineCommand> {
    Some(match payload {
        CommandPayload::StreamStartAll => {
            EngineCommand::StreamControl(EngineStreamControl::StartAll)
        }
        CommandPayload::StreamStopAll => EngineCommand::StreamControl(EngineStreamControl::StopAll),
        CommandPayload::StreamRetry { target } => {
            EngineCommand::StreamControl(EngineStreamControl::Retry {
                target: domain_target(*target),
            })
        }
        CommandPayload::RemoveStreamTarget { target } => EngineCommand::SetStreamTarget {
            target: domain_target(*target),
            spec: None,
        },
        CommandPayload::SetStreamTarget {
            target,
            config,
            stream_key,
        } => {
            let domain = domain_target(*target);
            let current = show.stream_target(domain);
            let protocol = model_protocol(config.protocol);
            EngineCommand::SetStreamTarget {
                target: domain,
                spec: Some(Box::new(EngineStreamTargetSpec {
                    name: config.name.clone(),
                    protocol,
                    endpoint: config.endpoint.clone(),
                    backup_endpoint: config.backup_endpoint.clone(),
                    key: stream_key.apply(
                        current
                            .and_then(StreamTarget::key)
                            .map(fm_model::StreamKey::expose_secret),
                    ),
                    output: config.output.to_domain(),
                    startup: if config.reconcile_on_startup {
                        StartupPolicy::ReconcileDesiredState
                    } else {
                        StartupPolicy::Stopped
                    },
                    video_bitrate_kbps: config.video_bitrate_kbps,
                    reconnect_initial_delay_ms: config.reconnect.initial_delay_ms,
                    reconnect_max_delay_ms: config.reconnect.max_delay_ms,
                    reconnect_max_attempts: config.reconnect.max_attempts,
                    ladder: current.and_then(StreamTarget::ladder).cloned(),
                })),
            }
        }
        _ => return None,
    })
}

/// The wire configuration of one destination.
pub(crate) fn wire_config(target: &StreamTarget) -> StreamTargetConfig {
    let reconnect = target.reconnect();
    StreamTargetConfig {
        name: target.name().to_owned(),
        protocol: wire_protocol(target.protocol()),
        endpoint: target.endpoint().as_str().to_owned(),
        backup_endpoint: target
            .backup_endpoint()
            .map(|endpoint| endpoint.as_str().to_owned()),
        output: WireOutputId::from_domain(target.output()),
        reconcile_on_startup: matches!(target.startup(), StartupPolicy::ReconcileDesiredState),
        video_bitrate_kbps: target.video_bitrate_kbps(),
        reconnect: StreamReconnect {
            initial_delay_ms: reconnect.initial_delay_ms(),
            max_delay_ms: reconnect.max_delay_ms(),
            max_attempts: reconnect.max_attempts(),
        },
    }
}

/// One destination as clients see it: no key, only whether one is stored.
pub(crate) fn wire_entry(target: &StreamTarget) -> StreamTargetEntry {
    StreamTargetEntry {
        target: WireStreamTargetId::new(target.id().get()),
        config: wire_config(target),
        has_key: target.key().is_some(),
        ladder_rungs: target.ladder().map_or(0, |ladder| {
            u8::try_from(ladder.rungs().len()).unwrap_or(u8::MAX)
        }),
    }
}

/// The wire payload of an undoable destination edit, with its exact key for
/// the journal and history.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetStreamTarget { target, spec } = command else {
        return None;
    };
    let target = WireStreamTargetId::new(target.get());
    Some(match spec {
        None => CommandPayload::RemoveStreamTarget { target },
        Some(spec) => CommandPayload::SetStreamTarget {
            target,
            config: Box::new(StreamTargetConfig {
                name: spec.name.clone(),
                protocol: wire_protocol(spec.protocol),
                endpoint: spec.endpoint.clone(),
                backup_endpoint: spec.backup_endpoint.clone(),
                output: WireOutputId::from_domain(spec.output),
                reconcile_on_startup: matches!(spec.startup, StartupPolicy::ReconcileDesiredState),
                video_bitrate_kbps: spec.video_bitrate_kbps,
                reconnect: StreamReconnect {
                    initial_delay_ms: spec.reconnect_initial_delay_ms,
                    max_delay_ms: spec.reconnect_max_delay_ms,
                    max_attempts: spec.reconnect_max_attempts,
                },
            }),
            stream_key: SecretEdit::exact(spec.key.as_deref()),
        },
    })
}

/// The durable event an accepted destination edit publishes, without its
/// key.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    let EngineEvent::StreamTargetChanged {
        target,
        config,
        streams,
    } = event
    else {
        return None;
    };
    Some(EventPayload::StreamTargetChanged {
        target: WireStreamTargetId::new(target.get()),
        entry: config.as_deref().map(|config| Box::new(wire_entry(config))),
        streams: streams
            .iter()
            .map(|stream| fm_protocol::StreamStatus {
                target: WireStreamTargetId::new(stream.target.get()),
                name: stream.name.clone(),
                desired_running: stream.desired_running,
                realized: fm_protocol::StreamRealizedState::Stopped,
                detail: None,
            })
            .collect(),
    })
}

/// Every configured destination in authored order, without its key.
pub(super) fn wire_entries(show: &ShowState) -> Vec<StreamTargetEntry> {
    show.stream_targets().iter().map(wire_entry).collect()
}
