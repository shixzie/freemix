//! Playlist automation driven by the engine frame cursor: programmed GO lands
//! on Program at exactly the scheduled frame.

use core::num::NonZeroU128;

use fm_automation::{
    EntryDuration, LoopMode, PlaylistEntry, PlaylistEntryId, PlaylistState, PlaylistTransition,
    PlaylistTransitionKind,
};
use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandOutcome, EngineEvent,
    EnginePlaylistOperation, EnginePlaylistStart, FrameResult, InputCatalogueError, PlaylistCue,
    ShowState, playlist_from_model, playlist_to_model,
};
use fm_switcher::{SwitcherEvent, TransitionKind};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "playlist show",
            [1, 2, 3, 4]
                .into_iter()
                .map(|value| (input(value), format!("Input {value}")))
                .collect(),
            input(1),
            input(4),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn entry(id: u64, value: u128, frames: u64) -> PlaylistEntry {
    PlaylistEntry::new(
        PlaylistEntryId::new(id),
        input(value),
        EntryDuration::Fixed { ms: frames },
    )
}

fn set_playlist(entries: Vec<PlaylistEntry>, loop_mode: LoopMode) -> EngineCommand {
    EngineCommand::SetPlaylist { entries, loop_mode }
}

fn start(at: EnginePlaylistStart) -> EngineCommand {
    EngineCommand::PlaylistControl(EnginePlaylistOperation::Start { at, from: None })
}

fn submit(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accepted(outcome: &EngineCommandOutcome) -> u64 {
    outcome
        .receipt
        .accepted()
        .unwrap_or_else(|| panic!("command was rejected: {:?}", outcome.receipt))
        .result
        .target_frame
        .get()
}

fn rejected(outcome: &EngineCommandOutcome) -> RejectionCode {
    outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
        .code
}

/// Ticks `frames` frames as the daemon clock does: before each frame, the
/// playlist advance due on that frame is submitted.
fn drive(engine: &mut Engine, frames: u64) -> Vec<FrameResult> {
    (0..frames)
        .map(|_| {
            let cursor = engine.frame_cursor().get();
            if engine.playlist_advance_frame() == Some(cursor) {
                let outcome = submit(
                    engine,
                    &format!("advance-{cursor}"),
                    EngineCommand::PlaylistAdvance { frame: cursor },
                );
                assert_eq!(accepted(&outcome), cursor);
            }
            engine.tick().unwrap()
        })
        .collect()
}

fn program_changes(frames: &[FrameResult]) -> Vec<(u64, InputId)> {
    frames
        .iter()
        .flat_map(|frame| {
            frame.events.iter().filter_map(move |event| match event {
                SwitcherEvent::ProgramChanged { program, .. } => {
                    Some((frame.frame.get(), *program))
                }
                _ => None,
            })
        })
        .collect()
}

#[test]
fn programmed_go_cuts_program_on_exactly_the_scheduled_frames() {
    let mut engine = engine();
    let outcome = submit(
        &mut engine,
        "set",
        set_playlist(vec![entry(1, 2, 10), entry(2, 3, 6)], LoopMode::Off),
    );
    accepted(&outcome);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::PlaylistChanged {
            frame: 0,
            cue: None
        }
    );
    engine.tick().unwrap();
    assert_eq!(engine.playlist_advance_frame(), None);

    // Started on frame 1 with a 7-frame delay: the first GO is frame 8.
    let outcome = submit(
        &mut engine,
        "start",
        start(EnginePlaylistStart::AfterFrames(7)),
    );
    assert_eq!(accepted(&outcome), 1);
    assert_eq!(
        engine.show().playlist().state(),
        PlaylistState::Armed {
            start_at_ms: 8,
            from: None
        }
    );
    assert_eq!(engine.playlist_advance_frame(), Some(8));

    let frames = drive(&mut engine, 30);
    // Entry 1 airs on frame 8, entry 2 on 18, and the list finishes on 24
    // with entry 2's input left on Program.
    assert_eq!(
        program_changes(&frames),
        vec![(8, input(2)), (18, input(3))]
    );
    let on = |frame: u64| {
        frames
            .iter()
            .find(|result| result.frame.get() == frame)
            .unwrap()
            .program
            .primary
    };
    assert_eq!(on(7), input(1));
    assert_eq!(on(8), input(2));
    assert_eq!(on(17), input(2));
    assert_eq!(on(18), input(3));
    assert_eq!(
        engine.show().playlist().state(),
        PlaylistState::Finished {
            last: Some(PlaylistEntryId::new(2))
        }
    );
    assert_eq!(engine.show().desired_switcher().program(), input(3));
    assert_eq!(engine.show().desired_switcher().preview(), input(2));
    assert_eq!(engine.realized_switcher().program(), input(3));
    assert!(engine.snapshot().is_ok());
}

#[test]
fn an_advance_accepted_ahead_is_realized_on_its_frame_and_holds_other_commands() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set_playlist(
            vec![
                entry(1, 2, 20)
                    .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Fade, 5)),
            ],
            LoopMode::Off,
        ),
    ));
    engine.tick().unwrap();
    accepted(&submit(
        &mut engine,
        "start",
        start(EnginePlaylistStart::AtFrame(12)),
    ));
    engine.tick().unwrap();
    assert_eq!(engine.frame_cursor().get(), 2);

    // Any other frame is refused; the due frame is accepted ten frames early.
    for frame in [2, 11, 13] {
        let outcome = submit(
            &mut engine,
            &format!("wrong-{frame}"),
            EngineCommand::PlaylistAdvance { frame },
        );
        assert_eq!(rejected(&outcome), RejectionCode::Conflict);
    }
    let outcome = submit(
        &mut engine,
        "advance",
        EngineCommand::PlaylistAdvance { frame: 12 },
    );
    assert_eq!(accepted(&outcome), 12);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::PlaylistChanged {
            frame: 12,
            cue: Some(PlaylistCue {
                entry: PlaylistEntryId::new(1),
                take: 1,
                input: input(2),
                transition: PlaylistTransition::new(PlaylistTransitionKind::Fade, 5),
            }),
        }
    );
    // The desired switcher already shows the result; nothing else may run
    // before the take is realized.
    assert_eq!(engine.show().desired_switcher().program(), input(2));
    let blocked = submit(&mut engine, "cut", EngineCommand::Cut);
    assert_eq!(rejected(&blocked), RejectionCode::Conflict);

    let frames: Vec<_> = (0..20).map(|_| engine.tick().unwrap()).collect();
    let started = frames
        .iter()
        .find(|frame| {
            frame
                .events
                .iter()
                .any(|event| matches!(event, SwitcherEvent::TransitionStarted { .. }))
        })
        .unwrap();
    assert_eq!(started.frame.get(), 12);
    assert_eq!(started.program.transition_kind, Some(TransitionKind::Fade));
    assert_eq!(started.program.secondary, Some(input(2)));
    assert_eq!(program_changes(&frames), vec![(16, input(2))]);
    // Once realized, ordinary commands are accepted again.
    accepted(&submit(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(3)),
    ));
}

#[test]
fn a_late_boundary_is_taken_on_the_next_frame_at_its_offset() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set_playlist(vec![entry(1, 2, 4), entry(2, 3, 50)], LoopMode::Off),
    ));
    accepted(&submit(
        &mut engine,
        "start",
        start(EnginePlaylistStart::Now),
    ));
    // Frame 0 takes entry 1 at once.
    assert_eq!(engine.show().desired_switcher().program(), input(2));
    for _ in 0..10 {
        engine.tick().unwrap();
    }
    // Entry 1 ended on frame 4; the clock arrives on frame 10 and joins
    // entry 2 six frames in.
    assert_eq!(engine.playlist_advance_frame(), Some(10));
    let outcome = submit(
        &mut engine,
        "late",
        EngineCommand::PlaylistAdvance { frame: 10 },
    );
    assert_eq!(accepted(&outcome), 10);
    assert_eq!(
        engine.show().playlist().state(),
        PlaylistState::Playing {
            entry: PlaylistEntryId::new(2),
            take: 2,
            cycle: 0,
            started_at_ms: 4,
        }
    );
    assert_eq!(engine.playlist_advance_frame(), Some(54));
}

#[test]
fn operations_edits_and_input_removal_respect_the_playlist() {
    let mut engine = engine();
    // Unknown inputs and Stinger transitions are refused.
    let unknown = submit(
        &mut engine,
        "unknown",
        set_playlist(vec![entry(1, 9, 10)], LoopMode::Off),
    );
    assert_eq!(rejected(&unknown), RejectionCode::NotFound);
    let stinger = submit(
        &mut engine,
        "stinger",
        set_playlist(
            vec![
                entry(1, 2, 10)
                    .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Stinger, 5)),
            ],
            LoopMode::Off,
        ),
    );
    assert_eq!(rejected(&stinger), RejectionCode::InvalidCommand);
    let empty = submit(&mut engine, "empty-start", start(EnginePlaylistStart::Now));
    assert_eq!(rejected(&empty), RejectionCode::Conflict);

    accepted(&submit(
        &mut engine,
        "set",
        set_playlist(vec![entry(1, 2, 10), entry(2, 3, 10)], LoopMode::All),
    ));
    engine.tick().unwrap();
    let removal = submit(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert_eq!(rejected(&removal), RejectionCode::Conflict);
    let refusal = &removal.receipt.rejected().unwrap().rejection;
    let in_playlist = InputCatalogueError::InPlaylist {
        input: input(3),
        entry: 2,
    };
    assert_eq!(refusal.message, in_playlist.to_string());
    let usage = fm_command::InputUse::PlaylistEntry { entry: 2 };
    let in_use = fm_command::RefusalReason::InputInUse { input: 3, usage };
    assert_eq!(refusal.reason, Some(in_use));

    accepted(&submit(
        &mut engine,
        "start",
        start(EnginePlaylistStart::Now),
    ));
    engine.tick().unwrap();
    let edit = submit(
        &mut engine,
        "edit-live",
        set_playlist(vec![entry(2, 3, 10)], LoopMode::Off),
    );
    assert_eq!(rejected(&edit), RejectionCode::Conflict);

    // Next and jump cut immediately; pause holds the boundary.
    accepted(&submit(
        &mut engine,
        "next",
        EngineCommand::PlaylistControl(EnginePlaylistOperation::Next),
    ));
    assert_eq!(engine.show().desired_switcher().program(), input(3));
    engine.tick().unwrap();
    accepted(&submit(
        &mut engine,
        "pause",
        EngineCommand::PlaylistControl(EnginePlaylistOperation::Pause),
    ));
    engine.tick().unwrap();
    assert_eq!(engine.playlist_advance_frame(), None);
    accepted(&submit(
        &mut engine,
        "jump",
        EngineCommand::PlaylistControl(EnginePlaylistOperation::Jump(PlaylistEntryId::new(1))),
    ));
    assert_eq!(engine.show().desired_switcher().program(), input(2));
    engine.tick().unwrap();
    accepted(&submit(
        &mut engine,
        "stop",
        EngineCommand::PlaylistControl(EnginePlaylistOperation::Stop),
    ));
    engine.tick().unwrap();
    assert_eq!(engine.show().playlist().state(), PlaylistState::Stopped);

    // Editing is a project edit: the padlock refuses it.
    accepted(&submit(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    engine.tick().unwrap();
    let locked = submit(
        &mut engine,
        "edit-locked",
        set_playlist(Vec::new(), LoopMode::Off),
    );
    assert_eq!(rejected(&locked), RejectionCode::Conflict);
}

#[test]
fn a_playing_playlist_survives_snapshot_restore_and_the_persisted_form() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set_playlist(vec![entry(1, 2, 6), entry(2, 3, 6)], LoopMode::All),
    ));
    accepted(&submit(
        &mut engine,
        "start",
        start(EnginePlaylistStart::Now),
    ));
    drive(&mut engine, 9);
    let playlist = engine.show().playlist().clone();
    assert_eq!(
        playlist.state(),
        PlaylistState::Playing {
            entry: PlaylistEntryId::new(2),
            take: 2,
            cycle: 0,
            started_at_ms: 6,
        }
    );

    let persisted = playlist_to_model(&playlist);
    assert_eq!(playlist_from_model(&persisted).unwrap(), playlist);

    let mut restored = Engine::restore(engine.snapshot().unwrap()).unwrap();
    assert_eq!(restored.show().playlist(), &playlist);
    let mut original = engine;
    let left = program_changes(&drive(&mut original, 12));
    let right = program_changes(&drive(&mut restored, 12));
    assert_eq!(left, vec![(12, input(2)), (18, input(3))]);
    assert_eq!(left, right);

    // A persisted state the engine could not have produced is refused.
    let mut invalid = persisted;
    invalid.observed_frame = 20;
    assert!(playlist_from_model(&invalid).is_err());
}
