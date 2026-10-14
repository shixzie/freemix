//! Wire mapping of the playlist commands and the `playlist_changed` event.

use fm_automation::{
    EntryDuration, LoopMode, PlaylistEntry, PlaylistEntryId, PlaylistState, PlaylistTransition,
    PlaylistTransitionKind,
};
use fm_engine::{Engine, EngineCommand, EnginePlaylistOperation, EnginePlaylistStart};
use fm_protocol::{
    CommandPayload, EventPayload, PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation,
    PlaylistRunState, PlaylistStart, PlaylistTransitionKind as WireTransitionKind, WireInputId,
};

// The wire and the engine agree on every playlist bound.
const _: () = {
    assert!(fm_protocol::MAX_PLAYLIST_ENTRIES <= fm_automation::MAX_PLAYLIST_ENTRIES);
    assert!(fm_protocol::MAX_PLAYLIST_ENTRY_FRAMES == fm_automation::MAX_PLAYLIST_ENTRY_MS);
    assert!(
        fm_protocol::MAX_PLAYLIST_TRANSITION_FRAMES == fm_engine::MAX_PLAYLIST_TRANSITION_FRAMES
    );
};

/// Maps the playlist commands.
pub fn playlist_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetPlaylist { loop_mode, entries } => EngineCommand::SetPlaylist {
            entries: entries.iter().map(domain_entry).collect(),
            loop_mode: match loop_mode {
                PlaylistLoopMode::Off => LoopMode::Off,
                PlaylistLoopMode::All => LoopMode::All,
                PlaylistLoopMode::One => LoopMode::One,
            },
        },
        CommandPayload::PlaylistControl { operation } => {
            EngineCommand::PlaylistControl(domain_operation(*operation))
        }
        CommandPayload::PlaylistAdvance { frame } => {
            EngineCommand::PlaylistAdvance { frame: *frame }
        }
        _ => unreachable!("only playlist commands are delegated"),
    }
}

fn domain_entry(entry: &PlaylistEntrySpec) -> PlaylistEntry {
    let kind = match entry.transition {
        WireTransitionKind::Cut => PlaylistTransitionKind::Cut,
        WireTransitionKind::Fade => PlaylistTransitionKind::Fade,
        WireTransitionKind::AlphaFade => PlaylistTransitionKind::AlphaFade,
        WireTransitionKind::Wipe => PlaylistTransitionKind::Wipe,
        WireTransitionKind::Slide => PlaylistTransitionKind::Slide,
        WireTransitionKind::Zoom => PlaylistTransitionKind::Zoom,
    };
    PlaylistEntry::new(
        PlaylistEntryId::new(entry.id),
        entry.input.to_domain(),
        EntryDuration::Fixed {
            ms: entry.duration_frames,
        },
    )
    .with_transition(PlaylistTransition::new(
        kind,
        u64::from(entry.transition_frames),
    ))
    .with_enabled(entry.enabled)
}

const fn domain_operation(operation: PlaylistOperation) -> EnginePlaylistOperation {
    match operation {
        PlaylistOperation::Start { at, from } => EnginePlaylistOperation::Start {
            at: match at {
                PlaylistStart::Now => EnginePlaylistStart::Now,
                PlaylistStart::AfterFrames(frames) => EnginePlaylistStart::AfterFrames(frames),
                PlaylistStart::AtFrame(frame) => EnginePlaylistStart::AtFrame(frame),
            },
            from: match from {
                Some(entry) => Some(PlaylistEntryId::new(entry)),
                None => None,
            },
        },
        PlaylistOperation::Stop => EnginePlaylistOperation::Stop,
        PlaylistOperation::Pause => EnginePlaylistOperation::Pause,
        PlaylistOperation::Resume => EnginePlaylistOperation::Resume,
        PlaylistOperation::Next => EnginePlaylistOperation::Next,
        PlaylistOperation::Previous => EnginePlaylistOperation::Previous,
        PlaylistOperation::Jump { entry } => {
            EnginePlaylistOperation::Jump(PlaylistEntryId::new(entry))
        }
    }
}

/// The complete post-command playlist with the desired Program and Preview.
pub fn playlist_changed(engine: &Engine, frame: u64) -> EventPayload {
    let playlist = engine.show().playlist();
    EventPayload::PlaylistChanged {
        frame,
        loop_mode: match playlist.config().loop_mode {
            LoopMode::Off => PlaylistLoopMode::Off,
            LoopMode::All => PlaylistLoopMode::All,
            LoopMode::One => PlaylistLoopMode::One,
        },
        entries: playlist.entries().iter().map(wire_entry).collect(),
        state: wire_state(playlist.state()),
        program: WireInputId::from_domain(engine.show().desired_switcher().program()),
        preview: WireInputId::from_domain(engine.show().desired_switcher().preview()),
    }
}

fn wire_entry(entry: &PlaylistEntry) -> PlaylistEntrySpec {
    let transition = match entry.transition.kind {
        PlaylistTransitionKind::Cut => WireTransitionKind::Cut,
        PlaylistTransitionKind::Fade => WireTransitionKind::Fade,
        PlaylistTransitionKind::AlphaFade => WireTransitionKind::AlphaFade,
        PlaylistTransitionKind::Wipe => WireTransitionKind::Wipe,
        PlaylistTransitionKind::Slide => WireTransitionKind::Slide,
        PlaylistTransitionKind::Zoom => WireTransitionKind::Zoom,
        PlaylistTransitionKind::Stinger => {
            unreachable!("the engine refuses stinger playlist transitions")
        }
    };
    PlaylistEntrySpec {
        id: entry.id.get(),
        input: WireInputId::from_domain(entry.input),
        duration_frames: entry
            .duration
            .known_ms()
            .expect("engine playlist durations are fixed"),
        transition,
        transition_frames: u32::try_from(entry.transition.duration_ms)
            .expect("engine playlist transitions are bounded"),
        enabled: entry.enabled,
    }
}

const fn wire_state(state: PlaylistState) -> PlaylistRunState {
    match state {
        PlaylistState::Stopped => PlaylistRunState::Stopped,
        PlaylistState::Armed { start_at_ms, from } => PlaylistRunState::Armed {
            start_frame: start_at_ms,
            from: match from {
                Some(entry) => Some(entry.get()),
                None => None,
            },
        },
        PlaylistState::Playing {
            entry,
            take,
            cycle,
            started_at_ms,
        } => PlaylistRunState::Playing {
            entry: entry.get(),
            take,
            cycle,
            started_frame: started_at_ms,
        },
        PlaylistState::Paused {
            entry,
            take,
            cycle,
            elapsed_ms,
        } => PlaylistRunState::Paused {
            entry: entry.get(),
            take,
            cycle,
            elapsed_frames: elapsed_ms,
        },
        PlaylistState::Finished { last } => PlaylistRunState::Finished {
            last: match last {
                Some(entry) => Some(entry.get()),
                None => None,
            },
        },
    }
}
