//! Operator cue lists (RC-017): GO runs the standby cue frame-atomically,
//! waits (in frames or milliseconds) land on exact frames, media actions run
//! the media transport, list edits are undoable and padlocked, and inputs a
//! cue names cannot be removed.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RefusalReason, RejectionCode};
use fm_engine::{
    CueScriptRun, EditLockScope, Engine, EngineCommand, EngineCommandOutcome,
    EngineCueListDefinition, EngineCueOperation, EngineEvent, FrameResult, ShowState,
};
use fm_model::{
    Cue, CueAction, CueMediaOperation, MediaPlaybackSettings, StingerSlotNumber, TransitionEffect,
};
use fm_switcher::{OverlayChannelId, SwitcherEvent};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "cue show",
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

fn cut(value: u128) -> CueAction {
    CueAction::Take {
        input: Some(input(value)),
        effect: TransitionEffect::Cut,
        duration_frames: 0,
    }
}

fn cue(id: u64, actions: Vec<CueAction>, auto_follow: bool) -> Cue {
    Cue {
        id,
        name: format!("Cue {id}"),
        actions,
        auto_follow,
    }
}

fn set(list: u64, cues: Vec<Cue>) -> EngineCommand {
    EngineCommand::SetCueList {
        list,
        definition: Some(EngineCueListDefinition {
            name: format!("List {list}"),
            cues,
        }),
    }
}

fn control(list: u64, operation: EngineCueOperation) -> EngineCommand {
    EngineCommand::CueControl { list, operation }
}

const GO: EngineCueOperation = EngineCueOperation::Go { cue: None };

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

fn refusal(outcome: &EngineCommandOutcome) -> (RejectionCode, Option<RefusalReason>) {
    let rejection = &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection;
    (rejection.code, rejection.reason.clone())
}

/// Ticks `frames` frames as the daemon cue clock does: before each frame,
/// the cue advance due on that frame is submitted.
fn drive(engine: &mut Engine, frames: u64) -> Vec<FrameResult> {
    (0..frames)
        .map(|_| {
            let cursor = engine.frame_cursor().get();
            if let Some((list, due)) = engine.cue_advance_frame()
                && due == cursor
            {
                let outcome = submit(
                    engine,
                    &format!("advance-{cursor}"),
                    EngineCommand::CueAdvance {
                        list,
                        frame: cursor,
                    },
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

fn overlay_on(engine: &Engine, channel: u8) -> Option<InputId> {
    let overlay = engine
        .realized_switcher()
        .overlay(OverlayChannelId::new(channel).unwrap());
    overlay.source().filter(|_| overlay.is_active())
}

#[test]
fn go_runs_cues_on_exact_frames_with_waits_and_auto_follow() {
    let mut engine = engine();
    let outcome = submit(
        &mut engine,
        "set",
        set(
            7,
            vec![
                cue(
                    10,
                    vec![
                        cut(2),
                        CueAction::OverlayOn {
                            channel: 1,
                            input: input(4),
                        },
                        CueAction::Wait { frames: 5 },
                        cut(3),
                    ],
                    true,
                ),
                cue(20, vec![CueAction::OverlayOff { channel: 1 }], false),
                cue(30, vec![cut(1)], false),
            ],
        ),
    );
    accepted(&outcome);
    let list = engine.show().cue_list(7).unwrap();
    assert_eq!(list.state.standby, Some(10));
    assert!(matches!(
        outcome.events[0].payload,
        EngineEvent::CueListChanged {
            list: 7,
            removed: false,
            ..
        }
    ));
    engine.tick().unwrap();

    // GO on frame 1: the cut and the overlay land together on that frame.
    let go = submit(&mut engine, "go-1", control(7, GO));
    let go_frame = accepted(&go);
    assert_eq!(go_frame, 1);
    match &go.events[0].payload {
        EngineEvent::CueListChanged { fired, frame, .. } => {
            assert_eq!(fired, &vec![10]);
            assert_eq!(*frame, 1);
        }
        other => panic!("unexpected event {other:?}"),
    }
    let state = &engine.show().cue_list(7).unwrap().state;
    assert_eq!(state.live, Some(10));
    assert_eq!(state.standby, Some(20));
    assert_eq!(state.pending.map(|pending| pending.due_frame), Some(6));
    assert_eq!(engine.cue_advance_frame(), Some((7, 6)));

    // Another live command is held while the cue's deferred actions are
    // scheduled ahead of their frame.
    let frames = drive(&mut engine, 1);
    assert_eq!(program_changes(&frames), vec![(1, input(2))]);
    assert_eq!(overlay_on(&engine, 1), Some(input(4)));

    // Frame 6 runs the rest of cue 10 and auto-follows cue 20 on the same
    // frame: the cut to 3 and the overlay off are realized together.
    let frames = drive(&mut engine, 6);
    assert_eq!(program_changes(&frames), vec![(6, input(3))]);
    assert_eq!(overlay_on(&engine, 1), None);
    let state = &engine.show().cue_list(7).unwrap().state;
    assert_eq!(state.live, Some(20));
    assert_eq!(state.standby, Some(30));
    assert_eq!(state.pending, None);
    assert_eq!(state.played, vec![10, 20]);
    assert_eq!(engine.cue_advance_frame(), None);

    // Back, skip, and jump move only the standby pointer.
    accepted(&submit(
        &mut engine,
        "back",
        control(7, EngineCueOperation::Back),
    ));
    assert_eq!(engine.show().cue_list(7).unwrap().state.standby, Some(20));
    accepted(&submit(
        &mut engine,
        "skip",
        control(7, EngineCueOperation::Skip),
    ));
    assert_eq!(engine.show().cue_list(7).unwrap().state.standby, Some(30));
    accepted(&submit(
        &mut engine,
        "jump",
        control(7, EngineCueOperation::Standby { cue: 30 }),
    ));
    let frame = accepted(&submit(&mut engine, "go-2", control(7, GO)));
    let frames = drive(&mut engine, 4);
    assert_eq!(program_changes(&frames), vec![(frame, input(1))]);
    let state = &engine.show().cue_list(7).unwrap().state;
    assert_eq!(state.standby, None);

    // Past the end, GO is refused with a typed reason.
    assert_eq!(
        refusal(&submit(&mut engine, "go-3", control(7, GO))).1,
        Some(RefusalReason::NoCueOnStandby)
    );
    accepted(&submit(
        &mut engine,
        "reset",
        control(7, EngineCueOperation::Reset),
    ));
    let state = &engine.show().cue_list(7).unwrap().state;
    assert_eq!(state.standby, Some(10));
    assert_eq!(state.played, Vec::<u64>::new());
}

#[test]
fn a_go_with_a_refused_action_changes_nothing() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(
                1,
                vec![
                    CueAction::OverlayOn {
                        channel: 2,
                        input: input(3),
                    },
                    CueAction::Take {
                        input: Some(input(2)),
                        effect: TransitionEffect::Stinger(StingerSlotNumber::new(1).unwrap()),
                        duration_frames: 25,
                    },
                ],
                false,
            )],
        ),
    ));
    engine.tick().unwrap();
    let revision = engine.revision();
    let program = engine.show().desired_switcher().program();
    // Stinger 1 has no media, so its take is refused and the overlay with it.
    let outcome = submit(&mut engine, "go", control(1, GO));
    let _ = refusal(&outcome);
    assert_eq!(engine.revision(), revision);
    assert_eq!(engine.show().desired_switcher().program(), program);
    assert!(
        !engine
            .show()
            .desired_switcher()
            .overlay(OverlayChannelId::new(2).unwrap())
            .is_active()
    );
    assert_eq!(engine.show().cue_list(1).unwrap().state.standby, Some(1));
}

#[test]
fn list_edits_are_undoable_and_padlocked() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(1, vec![cue(1, vec![cut(2)], false)]),
    ));
    accepted(&submit(
        &mut engine,
        "replace",
        set(
            1,
            vec![cue(1, vec![cut(2)], false), cue(2, vec![cut(3)], false)],
        ),
    ));
    assert_eq!(engine.show().cue_list(1).unwrap().cues.len(), 2);
    accepted(&submit(
        &mut engine,
        "undo-1",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().cue_list(1).unwrap().cues.len(), 1);
    accepted(&submit(
        &mut engine,
        "undo-2",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().cue_list(1).is_none());
    accepted(&submit(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().cue_list(1).is_some());

    // GO is a live operation: it leaves the undo history alone.
    let depth = engine.undo_entries().len();
    accepted(&submit(&mut engine, "go", control(1, GO)));
    assert_eq!(engine.undo_entries().len(), depth);

    accepted(&submit(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    assert_eq!(
        refusal(&submit(&mut engine, "locked", set(1, Vec::new()))).1,
        Some(RefusalReason::ProjectLocked)
    );
    // The padlock guards edits, not the show: GO still runs.
    accepted(&submit(
        &mut engine,
        "reset",
        control(1, EngineCueOperation::Reset),
    ));
}

#[test]
fn inputs_named_by_cues_cannot_be_removed_and_unknown_ones_are_refused() {
    let mut engine = engine();
    let unknown = submit(
        &mut engine,
        "unknown",
        set(1, vec![cue(1, vec![cut(9)], false)]),
    );
    assert_eq!(refusal(&unknown).0, RejectionCode::NotFound);
    accepted(&submit(
        &mut engine,
        "set",
        set(1, vec![cue(5, vec![cut(3)], false)]),
    ));
    let removed = submit(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert_eq!(
        refusal(&removed).1,
        Some(RefusalReason::InputInUse {
            input: 3,
            usage: fm_command::InputUse::CueAction { list: 1, cue: 5 },
        })
    );
}

#[test]
fn a_waiting_cue_cannot_be_changed_and_stop_drops_its_rest() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(
                1,
                vec![cut(2), CueAction::Wait { frames: 10 }, cut(3)],
                false,
            )],
        ),
    ));
    engine.tick().unwrap();
    accepted(&submit(&mut engine, "go", control(1, GO)));
    assert_eq!(
        refusal(&submit(&mut engine, "edit", set(1, Vec::new()))).1,
        Some(RefusalReason::CueRunning { cue: 1 })
    );
    accepted(&submit(
        &mut engine,
        "stop",
        control(1, EngineCueOperation::Stop),
    ));
    assert_eq!(engine.cue_advance_frame(), None);
    let frames = drive(&mut engine, 15);
    assert!(
        program_changes(&frames)
            .iter()
            .all(|(_, program)| *program == input(2))
    );
    accepted(&submit(&mut engine, "edit-2", set(1, Vec::new())));
}

/// A show whose input 2 is a synthetic ten-second media clip.
fn media_engine(rate: FrameRate) -> Engine {
    let mut show = ShowState::new(
        "cue media",
        vec![(input(1), "Camera".into()), (input(2), "Clip".into())],
        input(1),
        input(2),
    )
    .unwrap();
    let settings = MediaPlaybackSettings::default();
    show.set_media_input(input(2), settings).unwrap();
    let mut engine = Engine::new(
        show,
        rate,
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    engine
        .set_media_source(
            input(2),
            Some(fm_engine::EngineMediaSource::synthetic(&settings, rate)),
        )
        .unwrap();
    engine
}

fn media(operation: CueMediaOperation) -> CueAction {
    CueAction::Media {
        input: input(2),
        operation,
    }
}

/// A millisecond delay waits `ceil(ms * rate / 1000)` frames (at least one)
/// at the show's rate, decided when its cue fires.
#[test]
fn millisecond_delays_round_up_to_whole_frames_at_the_show_rate() {
    let ntsc = FrameRate::new(30_000, 1_001).unwrap();
    for (millis, rate, frames) in [
        (1_000, FrameRate::new(25, 1).unwrap(), 25),
        (1, FrameRate::new(25, 1).unwrap(), 1),
        (41, FrameRate::new(25, 1).unwrap(), 2),
        (40, FrameRate::new(25, 1).unwrap(), 1),
        (1_000, ntsc, 30),
        (1_001, ntsc, 30),
        (1_002, ntsc, 31),
        (
            fm_model::MAX_CUE_DELAY_MS,
            FrameRate::new(60, 1).unwrap(),
            216_000,
        ),
    ] {
        assert_eq!(fm_model::delay_frames(millis, rate), frames, "{millis} ms");
    }

    let mut engine = media_engine(ntsc);
    let outcome = submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(
                10,
                vec![cut(2), CueAction::Delay { millis: 1_002 }, cut(1)],
                false,
            )],
        ),
    );
    accepted(&outcome);
    engine.tick().unwrap();
    let go = accepted(&submit(&mut engine, "go", control(1, GO)));
    let due = go + 31;
    assert_eq!(engine.cue_advance_frame(), Some((1, due)));
    let remaining = due + 2 - engine.frame_cursor().get();
    let frames = drive(&mut engine, remaining);
    assert_eq!(program_changes(&frames), [(go, input(2)), (due, input(1))]);
}

/// Media actions run the input's transport through the ordinary media
/// command path: restart seeks to the clip's start and plays, a later seek
/// and pause land on the frame of their cue, and the transport is projected
/// at once like a direct media command's.
#[test]
fn media_actions_play_pause_seek_and_restart_the_transport() {
    let mut engine = media_engine(FrameRate::new(25, 1).unwrap());
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![
                cue(10, vec![media(CueMediaOperation::Restart)], false),
                cue(
                    20,
                    vec![
                        media(CueMediaOperation::Seek { frame: 100 }),
                        media(CueMediaOperation::Pause),
                    ],
                    false,
                ),
                cue(30, vec![media(CueMediaOperation::Play)], false),
            ],
        ),
    ));
    engine.tick().unwrap();
    let go = submit(&mut engine, "go-restart", control(1, GO));
    accepted(&go);
    let applied = match &go.events[0].payload {
        EngineEvent::CueListChanged { applied, .. } => applied.clone(),
        other => panic!("unexpected event {other:?}"),
    };
    assert_eq!(
        applied.len(),
        2,
        "restart is a seek and a play: {applied:?}"
    );
    let status = engine.media_status(input(2)).unwrap();
    assert_eq!(status.state, fm_engine::fm_playback::MediaState::Playing);
    assert!(engine.media_advancing());
    for _ in 0..5 {
        engine.tick().unwrap();
    }
    assert_eq!(engine.media_status(input(2)).unwrap().position, 5);

    accepted(&submit(&mut engine, "go-seek", control(1, GO)));
    engine.tick().unwrap();
    engine.tick().unwrap();
    let status = engine.media_status(input(2)).unwrap();
    assert_eq!(status.state, fm_engine::fm_playback::MediaState::Paused);
    assert_eq!(status.position, 100);

    accepted(&submit(&mut engine, "go-play", control(1, GO)));
    for _ in 0..3 {
        engine.tick().unwrap();
    }
    let status = engine.media_status(input(2)).unwrap();
    assert_eq!(status.state, fm_engine::fm_playback::MediaState::Playing);
    assert_eq!(status.position, 103);
}

/// A media action must name a media input.
#[test]
fn media_actions_on_other_inputs_are_refused() {
    let mut engine = media_engine(FrameRate::new(25, 1).unwrap());
    let outcome = submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(
                10,
                vec![CueAction::Media {
                    input: input(1),
                    operation: CueMediaOperation::Play,
                }],
                false,
            )],
        ),
    );
    assert_eq!(refusal(&outcome).0, RejectionCode::InvalidCommand);
    assert_eq!(engine.show().cue_lists().len(), 0);
}

/// A take that auto-follows, with no wait, into another take would put two
/// takes on one frame, which every GO would refuse: the list edit is
/// refused instead, naming both cues.
#[test]
fn an_auto_follow_into_a_second_take_on_one_frame_is_refused_at_edit() {
    let mut engine = engine();
    let outcome = submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(1, vec![cut(2)], true), cue(2, vec![cut(3)], false)],
        ),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(rejection.fields[0].field, "cues");
    assert_eq!(rejection.fields[0].code, "conflicting_follow_takes");
    assert!(rejection.message.contains("cue 1"), "{}", rejection.message);
    assert!(rejection.message.contains("cue 2"), "{}", rejection.message);
    assert!(engine.show().cue_list(1).is_none());

    // With a wait between the takes the chain runs on two frames.
    accepted(&submit(
        &mut engine,
        "set-wait",
        set(
            1,
            vec![
                cue(1, vec![cut(2)], true),
                cue(2, vec![CueAction::Wait { frames: 1 }, cut(3)], false),
            ],
        ),
    ));
    accepted(&submit(&mut engine, "go", control(1, GO)));
    let changes = program_changes(&drive(&mut engine, 4));
    assert_eq!(
        changes.iter().map(|(_, input)| *input).collect::<Vec<_>>(),
        vec![input(2), input(3)]
    );
}

/// List edits move standby onto new cues: the first cue added after the
/// end of a played-out list, and a cue replacing the standby cue at its
/// position.
#[test]
fn list_edits_put_new_cues_on_standby() {
    let overlay = |channel| CueAction::OverlayOff { channel };
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(1, vec![cue(1, vec![overlay(1)], false)]),
    ));
    accepted(&submit(&mut engine, "go", control(1, GO)));
    engine.tick().unwrap();
    assert_eq!(engine.show().cue_list(1).unwrap().state.standby, None);
    accepted(&submit(
        &mut engine,
        "append",
        set(
            1,
            vec![
                cue(1, vec![overlay(1)], false),
                cue(2, vec![overlay(2)], false),
                cue(3, vec![overlay(3)], false),
            ],
        ),
    ));
    assert_eq!(engine.show().cue_list(1).unwrap().state.standby, Some(2));
    accepted(&submit(
        &mut engine,
        "replace",
        set(
            1,
            vec![
                cue(1, vec![overlay(1)], false),
                cue(9, vec![overlay(4)], false),
                cue(3, vec![overlay(3)], false),
            ],
        ),
    ));
    assert_eq!(engine.show().cue_list(1).unwrap().state.standby, Some(9));
}

fn set_script(script: u64, source: &str) -> EngineCommand {
    EngineCommand::SetScript {
        script,
        definition: Some(fm_engine::EngineScriptDefinition {
            name: format!("Script {script}"),
            source: source.to_owned(),
        }),
    }
}

/// A cue's script step (schema 55) runs the script on the step's frame,
/// against the show as the earlier actions left it; the script a cue runs
/// must exist and cannot be removed while a cue names it.
#[test]
fn a_script_step_runs_on_its_frame_against_the_show_then() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "script",
        set_script(5, "if program == 2\n  overlay 1, 4\nend\ncut 3\n"),
    ));
    let unknown = submit(
        &mut engine,
        "unknown",
        set(
            1,
            vec![cue(1, vec![CueAction::RunScript { script: 9 }], false)],
        ),
    );
    assert_eq!(
        refusal(&unknown),
        (
            RejectionCode::NotFound,
            Some(RefusalReason::UnknownId {
                kind: fm_command::IdKind::Script,
                id: 9,
            })
        )
    );
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![cue(
                1,
                vec![
                    cut(2),
                    CueAction::Wait { frames: 3 },
                    CueAction::RunScript { script: 5 },
                ],
                false,
            )],
        ),
    ));
    let removed = submit(
        &mut engine,
        "remove",
        EngineCommand::SetScript {
            script: 5,
            definition: None,
        },
    );
    assert_eq!(refusal(&removed).0, RejectionCode::Conflict);
    assert!(engine.show().script(5).is_some());

    let go = accepted(&submit(&mut engine, "go", control(1, GO)));
    let cursor = engine.frame_cursor().get();
    let frames = drive(&mut engine, go - cursor + 6);
    assert_eq!(
        program_changes(&frames),
        vec![(go, input(2)), (go + 3, input(3))]
    );
    // The script saw the GO's cut to 2 on its own frame, so it keyed 4.
    assert_eq!(overlay_on(&engine, 1), Some(input(4)));
    let state = &engine.show().cue_list(1).unwrap().state;
    assert_eq!((state.pending, state.fault.clone()), (None, None));
}

/// A text field-only lower third, for the frame title-text bound.
fn title_show() -> ShowState {
    use fm_titles::{
        Alignment, Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId,
        FieldValue, Style, TemplateId, TitleFieldSet, TitleTemplate,
    };
    let id = |value: u128| NonZeroU128::new(value).unwrap();
    let template = TitleTemplate {
        id: TemplateId::new(id(9)),
        name: "Lower third".into(),
        width: 64,
        height: 16,
        background: Color::TRANSPARENT,
        fields: vec![FieldDefinition {
            id: FieldId::new(id(1)),
            name: "name".into(),
            default: FieldValue::Text(String::new()),
        }],
        elements: vec![Element {
            id: ElementId::new(id(1)),
            name: "bar".into(),
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 64,
                height: 16,
            },
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::BLACK,
                background: None,
                opacity: 255,
                font: None,
                alignment: Alignment::default(),
            },
            color_field: None,
            kind: ElementKind::Rectangle,
        }],
        animation: fm_titles::TitleAnimation::default(),
    };
    let mut show = ShowState::new(
        "title cue show",
        [1, 2, 3]
            .into_iter()
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    show.set_title(
        input(3),
        fm_engine::EngineTitle::new(
            "Lower third",
            TitleFieldSet::from_template(&template).unwrap(),
        ),
    )
    .unwrap();
    show
}

/// One frame's event carries every title text it set, so a cue frame
/// whose script steps would set more than one script run's whole budget
/// of text is refused whole (the script stops at the title text past the
/// bound, keeping its resource-exhausted code); the script alone is within
/// it.
#[test]
fn a_cue_frame_sets_at_most_one_runs_worth_of_title_text() {
    let mut engine = Engine::new(
        title_show(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    let text = "x".repeat(fm_model::MAX_CUE_TEXT_BYTES);
    accepted(&submit(
        &mut engine,
        "script",
        set_script(
            5,
            &format!("let t = \"{text}\"\nrepeat 64\n  title_text 3, 1, t\nend\n"),
        ),
    ));
    accepted(&submit(
        &mut engine,
        "run",
        EngineCommand::RunScript { script: 5 },
    ));
    let one = CueAction::TitleText {
        input: input(3),
        field: fm_model::TitleFieldId::new(NonZeroU128::MIN),
        text: "y".into(),
    };
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![
                cue(1, vec![CueAction::RunScript { script: 5 }], false),
                cue(2, vec![one, CueAction::RunScript { script: 5 }], false),
            ],
        ),
    ));
    accepted(&submit(&mut engine, "go1", control(1, GO)));
    let revision = engine.revision();
    let refused = submit(&mut engine, "go2", control(1, GO));
    assert_eq!(
        refusal(&refused),
        (
            RejectionCode::ResourceExhausted,
            Some(RefusalReason::ScriptFailed {
                failure: fm_command::ScriptFailureKind::Refused,
                line: 3,
                column: 3,
            })
        )
    );
    let message = &refused.receipt.rejected().unwrap().rejection.message;
    assert!(
        message.contains("8192 bytes of title text on one frame"),
        "{message}"
    );
    assert_eq!(engine.revision(), revision);
}

/// The script steps a cue event reports, in the order they ran.
fn cue_scripts(outcome: &EngineCommandOutcome) -> Vec<CueScriptRun> {
    match &outcome.events[0].payload {
        EngineEvent::CueListChanged { scripts, .. } => scripts.clone(),
        other => panic!("expected a cue event: {other:?}"),
    }
}

/// What a direct script run reports.
fn direct_run(engine: &mut Engine, key: &str, script: u64) -> CueScriptRun {
    let outcome = submit(engine, key, EngineCommand::RunScript { script });
    accepted(&outcome);
    match &outcome.events[0].payload {
        EngineEvent::ScriptRan {
            script,
            actions,
            instructions,
            ..
        } => CueScriptRun {
            script: *script,
            actions: *actions,
            instructions: *instructions,
        },
        other => panic!("expected a script run: {other:?}"),
    }
}

/// The fault of cue 2's failed script step keeps the step's typed reason
/// and message (Protocol 2.63), what a refused GO of the step would have
/// said; an edit that keeps the faulted action keeps the fault and its
/// detail, and a reset clears it.
fn assert_step_fault_keeps_its_reason(cues: &mut Engine) {
    let fault = cues
        .show()
        .cue_list(1)
        .unwrap()
        .state
        .fault
        .clone()
        .expect("the deferred step faulted");
    assert_eq!((fault.cue, fault.action), (2, 1));
    assert_eq!(
        fault.detail.reason,
        Some(fm_command::RefusalReason::ScriptFailed {
            failure: fm_command::ScriptFailureKind::Refused,
            line: 1,
            column: 1,
        })
    );
    assert!(
        fault.detail.message.starts_with("script 6 stopped at")
            && fault
                .detail
                .message
                .ends_with("the show has no input number 9"),
        "{:?}",
        fault.detail
    );
    let definition = cues.show().cue_list(1).unwrap().cues.clone();
    accepted(&submit(cues, "edit", set(1, definition)));
    assert_eq!(cues.show().cue_list(1).unwrap().state.fault, Some(fault));
    accepted(&submit(
        cues,
        "reset",
        control(1, EngineCueOperation::Reset),
    ));
    assert_eq!(cues.show().cue_list(1).unwrap().state.fault, None);
}

/// Protocol 2.61: a cue event reports every script step its frame ran, in
/// order, with the counts a direct `script_run` of the same script on the
/// same show reports: a GO's steps see the earlier steps' actions, and a
/// deferred step is reported by the advance that runs it. A deferred step
/// that fails reports nothing, since nothing of its frame is kept.
#[test]
fn a_cue_run_reports_each_script_step_as_a_direct_run_would() {
    let scripts = [
        (5, "if preview == 2\n  mute 3\n  unmute 3\nend\nmute 1\n"),
        (6, "repeat 2\n  preview 2\nend\n"),
    ];
    let mut cues = engine();
    let mut direct = engine();
    for (script, source) in scripts {
        for engine in [&mut cues, &mut direct] {
            accepted(&submit(
                engine,
                &format!("script-{script}"),
                set_script(script, source),
            ));
        }
    }
    let step = |script| CueAction::RunScript { script };
    accepted(&submit(
        &mut cues,
        "set",
        set(
            1,
            vec![
                cue(
                    1,
                    vec![
                        step(5),
                        step(6),
                        step(5),
                        CueAction::Wait { frames: 2 },
                        step(5),
                    ],
                    false,
                ),
                cue(2, vec![CueAction::Wait { frames: 1 }, step(6)], false),
            ],
        ),
    ));

    let go = submit(&mut cues, "go", control(1, GO));
    let go_frame = accepted(&go);
    let expected = vec![
        direct_run(&mut direct, "run-1", 5),
        direct_run(&mut direct, "run-2", 6),
        direct_run(&mut direct, "run-3", 5),
    ];
    // The first run saw Preview on 4 and the third on 2, which script 6
    // put there on the same frame, so their counts differ.
    assert_ne!(expected[0], expected[2]);
    assert_eq!(cue_scripts(&go), expected);

    while cues.frame_cursor().get() < go_frame + 2 {
        cues.tick().unwrap();
    }
    let advance = submit(
        &mut cues,
        "advance",
        EngineCommand::CueAdvance {
            list: 1,
            frame: go_frame + 2,
        },
    );
    accepted(&advance);
    assert_eq!(
        cue_scripts(&advance),
        vec![direct_run(&mut direct, "run-4", 5)]
    );

    // Cue 2's deferred step fails once script 6 names an input the show
    // does not hold.
    accepted(&submit(&mut cues, "broken", set_script(6, "preview 9\n")));
    let second = submit(&mut cues, "go-2", control(1, GO));
    let second_frame = accepted(&second);
    assert_eq!(cue_scripts(&second), Vec::new());
    while cues.frame_cursor().get() < second_frame + 1 {
        cues.tick().unwrap();
    }
    let failed = submit(
        &mut cues,
        "advance-2",
        EngineCommand::CueAdvance {
            list: 1,
            frame: second_frame + 1,
        },
    );
    accepted(&failed);
    assert_eq!(cue_scripts(&failed), Vec::new());
    assert_step_fault_keeps_its_reason(&mut cues);
}

/// A saved waiting cue was scheduled on a frame before the frame cursor,
/// its due frame that frame plus the wait before its next action, so it
/// lies within that wait of the cursor; a hand-edited one further ahead is
/// refused on restore, so the cue clock (and a local run that plays it to
/// the end) is never held for ever.
#[test]
fn a_restored_cue_due_beyond_its_wait_is_refused() {
    let mut engine = engine();
    let waits = vec![cut(2), CueAction::Wait { frames: 40 }, cut(3)];
    accepted(&submit(
        &mut engine,
        "set-1",
        set(1, vec![cue(10, waits, false)]),
    ));
    let delays = vec![
        cue(20, vec![CueAction::Delay { millis: 1_000 }], true),
        cue(21, Vec::new(), false),
    ];
    accepted(&submit(&mut engine, "set-2", set(2, delays)));
    let go = accepted(&submit(&mut engine, "go-1", control(1, GO)));
    engine.tick().unwrap();
    let delayed = accepted(&submit(&mut engine, "go-2", control(2, GO)));
    engine.tick().unwrap();
    let snapshot = engine.snapshot().expect("idle");
    let cursor = snapshot.frames_rendered();
    let due = |list: u64| {
        snapshot
            .show()
            .cue_list(list)
            .unwrap()
            .state
            .pending
            .expect("the cue waits")
            .due_frame
    };
    assert_eq!((due(1), due(2)), (go + 40, delayed + 25));
    let restore = |show: ShowState| {
        Engine::restore_persisted(
            show,
            snapshot.realized_switcher().clone(),
            snapshot.frame_rate(),
            fm_clock::ClockDomainId::new(NonZeroU128::new(100).unwrap()),
            fm_engine::EngineRestoreState {
                state_epoch: snapshot.state_epoch(),
                revision: snapshot.revision(),
                event_sequence: snapshot.event_sequence(),
                runtime_generation: snapshot.runtime_generation(),
                clock_time: snapshot.clock_time(),
                frame_cursor: fm_scheduler::FrameNumber::new(cursor),
                receipts: snapshot.receipts().to_vec(),
            },
        )
    };
    let restored = restore(snapshot.show().clone()).expect("the saved cues restore");
    assert_eq!(restored.show().cue_lists(), snapshot.show().cue_lists());
    assert_eq!(restored.cue_advance_frame(), Some((2, delayed + 25)));

    let moved = |list: u64, due_frame: u64| {
        let mut show = snapshot.show().clone();
        let mut lists = show.cue_lists().to_vec();
        let found = lists.iter_mut().find(|found| found.id == list).unwrap();
        found.state.pending.as_mut().unwrap().due_frame = due_frame;
        // The lists fit the show (the model checks no frame cursor) ...
        show.set_cue_lists(lists).unwrap();
        show
    };
    // ... and each may be due up to its wait past the cursor ...
    restore(moved(1, cursor + 40)).expect("a wait from the cursor restores");
    restore(moved(2, cursor + 25)).expect("a delay from the cursor restores");
    // ... but no further.
    for (list, due_frame) in [(1, cursor + 41), (2, cursor + 26), (1, u64::MAX)] {
        assert!(
            matches!(
                restore(moved(list, due_frame)).err(),
                Some(fm_engine::SnapshotError::CueRunOutOfReach {
                    list: refused,
                    due_frame: at,
                    frame_cursor,
                }) if refused == list && at == due_frame && frame_cursor == cursor
            ),
            "list {list} due {due_frame}"
        );
    }
}
