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
        input: Some(fm_types::InputTarget::Fixed(input(value))),
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
        position: None,
    }
}

fn control(list: u64, operation: EngineCueOperation) -> EngineCommand {
    EngineCommand::CueControl { list, operation }
}

const GO: EngineCueOperation = EngineCueOperation::Go {
    cue: None,
    targets: fm_types::TargetBindings::NONE,
};

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
                            input: fm_types::InputTarget::Fixed(input(4)),
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
                        input: fm_types::InputTarget::Fixed(input(3)),
                    },
                    CueAction::Take {
                        input: Some(fm_types::InputTarget::Fixed(input(2))),
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
        input: fm_types::InputTarget::Fixed(input(2)),
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
                    input: fm_types::InputTarget::Fixed(input(1)),
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
        position: None,
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
            position: None,
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
                cue(
                    2,
                    vec![one.into(), CueAction::RunScript { script: 5 }],
                    false,
                ),
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

fn arm(cue: Option<u64>, in_millis: u32) -> EngineCueOperation {
    EngineCueOperation::Arm {
        cue,
        in_millis,
        targets: fm_types::TargetBindings::default(),
    }
}

/// Protocol 2.64, schema 58: a GO armed for later fires on exactly its due
/// frame as a GO would, survives edits that keep its cue, and is disarmed,
/// stopped, or refused like a GO.
#[test]
fn an_armed_go_fires_on_its_frame_and_keeps_through_edits() {
    let mut engine = engine();
    let cues = |first: CueAction| vec![cue(10, vec![first], false), cue(20, vec![cut(4)], false)];
    accepted(&submit(&mut engine, "set", set(1, cues(cut(2)))));
    engine.tick().unwrap();
    for (key, in_millis) in [("zero", 0), ("long", fm_model::MAX_CUE_ARM_MS + 1)] {
        assert_eq!(
            refusal(&submit(&mut engine, key, control(1, arm(None, in_millis)))).0,
            RejectionCode::InvalidCommand
        );
    }
    assert_eq!(
        refusal(&submit(
            &mut engine,
            "unknown",
            control(1, arm(Some(99), 40))
        ))
        .1,
        Some(RefusalReason::UnknownId {
            kind: fm_command::IdKind::Cue,
            id: 99
        })
    );
    // One second at 25 fps: the standby cue fires 25 frames later.
    let armed_at = accepted(&submit(&mut engine, "arm", control(1, arm(None, 1_000))));
    let due = armed_at + 25;
    let state = &engine.show().cue_list(1).unwrap().state;
    assert_eq!(
        state.armed,
        Some(fm_model::CueArmed {
            cue: 10,
            due_frame: due,
            targets: None
        })
    );
    assert_eq!(state.live, None, "arming fires nothing");
    assert_eq!(engine.cue_advance_frame(), Some((1, due)));

    // The armed cue may change but not go away, nor may its list.
    let dropped = set(1, vec![cue(20, vec![cut(4)], false)]);
    assert_eq!(
        refusal(&submit(&mut engine, "drop", dropped)).1,
        Some(RefusalReason::CueRunning { cue: 10 })
    );
    let removed = EngineCommand::SetCueList {
        list: 1,
        definition: None,
        position: None,
    };
    assert_eq!(
        refusal(&submit(&mut engine, "remove", removed)).1,
        Some(RefusalReason::CueRunning { cue: 10 })
    );
    accepted(&submit(&mut engine, "edit", set(1, cues(cut(3)))));
    let armed = engine.show().cue_list(1).unwrap().state.armed;
    assert_eq!(armed.map(|armed| armed.due_frame), Some(due));

    let ahead = due + 3 - engine.frame_cursor().get();
    let frames = drive(&mut engine, ahead);
    assert_eq!(program_changes(&frames), vec![(due, input(3))]);
    let state = &engine.show().cue_list(1).unwrap().state;
    assert_eq!(state.armed, None);
    assert_eq!(state.live, Some(10));
    assert_eq!(state.standby, Some(20));
    assert_eq!(engine.cue_advance_frame(), None);

    // Disarm and Stop drop an armed GO; nothing fires.
    for (key, drop) in [
        ("disarm", EngineCueOperation::Disarm),
        ("stop", EngineCueOperation::Stop),
    ] {
        accepted(&submit(
            &mut engine,
            &format!("arm-{key}"),
            control(1, arm(Some(20), 200)),
        ));
        assert!(engine.cue_advance_frame().is_some());
        accepted(&submit(&mut engine, key, control(1, drop)));
        assert_eq!(engine.cue_advance_frame(), None);
        let frames = drive(&mut engine, 10);
        assert_eq!(program_changes(&frames), Vec::new());
    }
}

/// An armed GO refused when it fires (here: a cut while the chain's fade is
/// in flight) sets the list's fault, applies nothing of its frame, and
/// leaves a cue waiting in the list waiting.
#[test]
fn a_refused_armed_go_faults_and_keeps_the_waiting_cue() {
    let mut engine = engine();
    let fade = CueAction::Take {
        input: Some(input(2)),
        effect: TransitionEffect::Fade,
        duration_frames: 50,
    };
    let cues = vec![
        cue(
            30,
            vec![fade.into(), CueAction::Wait { frames: 60 }, cut(3)],
            false,
        ),
        cue(40, vec![cut(4)], false),
    ];
    accepted(&submit(&mut engine, "set", set(1, cues)));
    engine.tick().unwrap();
    let go = accepted(&submit(&mut engine, "go", control(1, GO)));
    let armed_at = accepted(&submit(&mut engine, "arm", control(1, arm(Some(40), 200))));
    let due = armed_at + 5;
    assert_eq!(engine.cue_advance_frame(), Some((1, due)));
    let ahead = due + 2 - engine.frame_cursor().get();
    let frames = drive(&mut engine, ahead);
    assert!(
        program_changes(&frames)
            .iter()
            .all(|(_, program)| *program != input(4))
    );
    let state = &engine.show().cue_list(1).unwrap().state;
    assert_eq!(state.armed, None);
    let fault = state.fault.as_ref().expect("the armed GO faulted");
    assert_eq!((fault.cue, fault.action), (40, 0));
    assert!(fault.detail.message.contains("transition"), "{fault:?}");
    assert_eq!(
        state.pending.map(|pending| pending.due_frame),
        Some(go + 60)
    );
    assert_eq!(state.live, Some(30));
}

/// A restored armed GO may be due at most the longest arm delay past the
/// frame cursor (a further one would hold the cue clock for ever).
#[test]
fn a_restored_armed_go_beyond_the_longest_delay_is_refused() {
    let mut engine = engine();
    let cues = vec![cue(10, vec![cut(2)], false)];
    accepted(&submit(&mut engine, "set", set(1, cues)));
    accepted(&submit(&mut engine, "arm", control(1, arm(None, 1_000))));
    engine.tick().unwrap();
    let snapshot = engine.snapshot().expect("idle");
    let cursor = snapshot.frames_rendered();
    let restore = |due_frame: u64| {
        let mut show = snapshot.show().clone();
        let mut lists = show.cue_lists().to_vec();
        lists[0].state.armed.as_mut().unwrap().due_frame = due_frame;
        show.set_cue_lists(lists).unwrap();
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
    let longest = 25 * 86_400;
    let restored = restore(cursor + longest).expect("a day from the cursor restores");
    assert_eq!(restored.cue_advance_frame(), Some((1, cursor + longest)));
    assert!(matches!(
        restore(cursor + longest + 1).err(),
        Some(fm_engine::SnapshotError::CueRunOutOfReach { list: 1, .. })
    ));
}

/// A scoreboard title: text `name` ("Ana"), number `score` (0), color
/// `bar`, and text `count` ("7").
fn score_show() -> ShowState {
    use fm_titles::{
        Alignment, Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId,
        FieldValue, Style, TemplateId, TitleFieldSet, TitleTemplate,
    };
    let id = |value: u128| NonZeroU128::new(value).unwrap();
    let field = |value: u128, name: &str, default: FieldValue| FieldDefinition {
        id: FieldId::new(id(value)),
        name: name.into(),
        default,
    };
    let template = TitleTemplate {
        id: TemplateId::new(id(9)),
        name: "Score".into(),
        width: 64,
        height: 16,
        background: Color::TRANSPARENT,
        fields: vec![
            field(1, "name", FieldValue::Text("Ana".into())),
            field(2, "score", FieldValue::Number(0)),
            field(3, "bar", FieldValue::Color(Color::BLACK)),
            field(4, "count", FieldValue::Text("7".into())),
        ],
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
        "score cue show",
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
        fm_engine::EngineTitle::new("Score", TitleFieldSet::from_template(&template).unwrap()),
    )
    .unwrap();
    show
}

fn number(field: u128, update: fm_model::CueNumberUpdate) -> CueAction {
    CueAction::TitleNumber {
        input: fm_types::InputTarget::Fixed(input(3)),
        field: fm_model::TitleFieldId::new(NonZeroU128::new(field).unwrap()),
        update,
    }
}

fn field_value(engine: &Engine, field: u128) -> fm_titles::FieldValue {
    engine
        .show()
        .title(input(3))
        .unwrap()
        .fields
        .value(fm_titles::FieldId::new(NonZeroU128::new(field).unwrap()))
        .unwrap()
        .clone()
}

/// Schema 59: a data update sets or steps a title's number field, or a
/// text field holding a whole number as decimal text, reading the value
/// the frame's earlier actions left; the event reports each value set.
#[test]
fn title_number_actions_set_and_step_number_and_text_fields() {
    use fm_model::CueNumberUpdate::{Add, Set};
    use fm_titles::{FieldId, FieldValue};

    let mut engine = Engine::new(
        score_show(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![
                cue(
                    1,
                    vec![number(2, Add(1)), number(2, Add(2)), number(4, Add(-10))],
                    false,
                ),
                cue(2, vec![number(2, Set(-5)), number(1, Set(42))], false),
            ],
        ),
    ));
    let outcome = submit(&mut engine, "go1", control(1, GO));
    accepted(&outcome);
    let EngineEvent::CueListChanged { title_texts, .. } = &outcome.events[0].payload else {
        panic!("a GO reports its cue run: {:?}", outcome.events);
    };
    let field = |value: u128| FieldId::new(NonZeroU128::new(value).unwrap());
    assert_eq!(
        title_texts,
        &vec![
            (input(3), field(2), FieldValue::Number(1)),
            (input(3), field(2), FieldValue::Number(3)),
            (input(3), field(4), FieldValue::Text("-3".into())),
        ]
    );
    assert_eq!(field_value(&engine, 2), FieldValue::Number(3));
    assert_eq!(field_value(&engine, 4), FieldValue::Text("-3".into()));
    accepted(&submit(&mut engine, "go2", control(1, GO)));
    assert_eq!(field_value(&engine, 2), FieldValue::Number(-5));
    // Setting a text field needs no number in it.
    assert_eq!(field_value(&engine, 1), FieldValue::Text("42".into()));
}

/// A data update of a missing or unsuitable field is refused when the list
/// is defined; stepping text that is not a whole number, or past the
/// bound, refuses the GO whole.
#[test]
fn title_number_actions_refuse_unsuitable_fields_and_out_of_range_results() {
    use fm_model::CueNumberUpdate::{Add, Set};

    let mut engine = Engine::new(
        score_show(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    for (key, action, code) in [
        ("color", number(3, Add(1)), RejectionCode::InvalidCommand),
        ("missing", number(9, Add(1)), RejectionCode::NotFound),
        (
            "zero-step",
            number(2, Add(0)),
            RejectionCode::InvalidCommand,
        ),
        (
            "too-large",
            number(2, Set(fm_model::MAX_CUE_TITLE_NUMBER + 1)),
            RejectionCode::InvalidCommand,
        ),
    ] {
        let refused = submit(&mut engine, key, set(1, vec![cue(1, vec![action], false)]));
        assert_eq!(refusal(&refused).0, code, "{key}");
    }
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            vec![
                cue(1, vec![number(1, Add(1))], false),
                cue(
                    2,
                    vec![
                        number(2, Set(fm_model::MAX_CUE_TITLE_NUMBER)),
                        number(2, Add(1)),
                    ],
                    false,
                ),
            ],
        ),
    ));
    let revision = engine.revision();
    // "Ana" is not a whole number.
    let refused = submit(&mut engine, "go-name", control(1, GO));
    assert_eq!(refusal(&refused).0, RejectionCode::InvalidCommand);
    let refused = submit(
        &mut engine,
        "go-max",
        control(
            1,
            EngineCueOperation::Go {
                cue: Some(2),
                targets: fm_types::TargetBindings::default(),
            },
        ),
    );
    assert_eq!(
        refusal(&refused),
        (
            RejectionCode::InvalidCommand,
            Some(RefusalReason::bounds(
                "title_number",
                fm_model::MAX_CUE_TITLE_NUMBER.unsigned_abs()
            ))
        )
    );
    assert_eq!(engine.revision(), revision);
    assert_eq!(field_value(&engine, 2), fm_titles::FieldValue::Number(0));
}

/// RC-004: a GO binds its run's dynamic targets once — Preview and Program
/// as they stand before the cue's actions, and the firing console's
/// selection — so a step after a wait acts on the input bound at GO even
/// though Preview changed since; an arm binds when armed; and a GO carrying
/// the journaled bindings replays on the same inputs.
#[test]
#[allow(clippy::too_many_lines)] // one run: GO, arm, and replay
fn a_go_binds_dynamic_targets_for_its_whole_run() {
    use fm_model::{InputTarget, TargetBindings};
    let list = || {
        set(
            1,
            vec![
                cue(
                    10,
                    vec![
                        CueAction::Preview {
                            input: InputTarget::Fixed(input(2)),
                        },
                        CueAction::Wait { frames: 5 },
                        CueAction::OverlayOn {
                            channel: 1,
                            input: InputTarget::Preview,
                        },
                        CueAction::Take {
                            input: Some(InputTarget::Selected),
                            effect: TransitionEffect::Cut,
                            duration_frames: 0,
                        },
                    ],
                    false,
                ),
                cue(
                    20,
                    vec![CueAction::Take {
                        input: Some(InputTarget::Selected),
                        effect: TransitionEffect::Cut,
                        duration_frames: 0,
                    }],
                    false,
                ),
            ],
        )
    };
    let selected = |value| TargetBindings {
        selected: Some(input(value)),
        ..TargetBindings::NONE
    };
    let bound = TargetBindings {
        preview: Some(input(4)),
        program: Some(input(1)),
        selected: Some(input(3)),
    };

    let mut live = engine();
    accepted(&submit(&mut live, "set", list()));
    live.tick().unwrap();
    // Without a selection, the GO is refused whole.
    assert_eq!(
        refusal(&submit(&mut live, "unbound", control(1, GO))).0,
        RejectionCode::InvalidCommand
    );
    assert_eq!(live.show().cue_list(1).unwrap().state.live, None);

    let go = EngineCueOperation::Go {
        cue: None,
        targets: selected(3),
    };
    let fired = accepted(&submit(&mut live, "go", control(1, go)));
    let state = &live.show().cue_list(1).unwrap().state;
    assert_eq!(state.targets, Some(bound));
    let ahead = fired + 8 - live.frame_cursor().get();
    let frames = drive(&mut live, ahead);
    // The overlay shows 4, Preview at GO, not 2, Preview since.
    assert_eq!(overlay_on(&live, 1), Some(input(4)));
    assert_eq!(program_changes(&frames), vec![(fired + 5, input(3))]);

    // An arm binds Preview, Program, and the selection when armed; the cut
    // put the old Program, 1, on Preview.
    let armed_at = accepted(&submit(
        &mut live,
        "arm",
        control(
            1,
            EngineCueOperation::Arm {
                cue: None,
                in_millis: 200,
                targets: selected(4),
            },
        ),
    ));
    let armed = live.show().cue_list(1).unwrap().state.armed.unwrap();
    assert_eq!(
        armed.targets,
        Some(TargetBindings {
            preview: Some(input(1)),
            program: Some(input(3)),
            selected: Some(input(4)),
        })
    );
    let ahead = armed_at + 8 - live.frame_cursor().get();
    let frames = drive(&mut live, ahead);
    assert_eq!(program_changes(&frames), vec![(armed_at + 5, input(4))]);

    // A replay GO carries what the authority bound: Preview is 2 on this
    // engine before the GO, yet the overlay shows the journaled 4.
    let mut replay = engine();
    let preset = || {
        set(
            2,
            vec![cue(
                1,
                vec![CueAction::Preview {
                    input: InputTarget::Fixed(input(2)),
                }],
                false,
            )],
        )
    };
    accepted(&submit(&mut replay, "preset", preset()));
    accepted(&submit(&mut replay, "set", list()));
    replay.tick().unwrap();
    accepted(&submit(&mut replay, "preview", control(2, GO)));
    replay.tick().unwrap();
    assert_eq!(replay.show().desired_switcher().preview(), input(2));
    let go = EngineCueOperation::Go {
        cue: None,
        targets: bound,
    };
    let fired = accepted(&submit(&mut replay, "go", control(1, go)));
    let ahead = fired + 8 - replay.frame_cursor().get();
    let frames = drive(&mut replay, ahead);
    assert_eq!(overlay_on(&replay, 1), Some(input(4)));
    assert_eq!(program_changes(&frames), vec![(fired + 5, input(3))]);
}

/// RC-004: a list edited to name a dynamic target while a run (or an
/// armed GO) that bound none is under way still resolves Preview and
/// Program when the advance runs the edited cue, as the show has them on
/// that frame; they are never unbound, so the run does not fault.
#[test]
fn an_advance_into_a_cue_edited_to_a_dynamic_target_binds_preview_and_program() {
    use fm_model::InputTarget;
    let overlay = |channel| CueAction::OverlayOn {
        channel,
        input: InputTarget::Preview,
    };

    // A run waiting in cue 10, whose follower is edited to show Preview.
    let mut live = engine();
    let waiting = cue(10, vec![CueAction::Wait { frames: 5 }], true);
    accepted(&submit(
        &mut live,
        "set",
        set(1, vec![waiting.clone(), cue(20, vec![cut(2)], false)]),
    ));
    live.tick().unwrap();
    let fired = accepted(&submit(&mut live, "go", control(1, GO)));
    assert_eq!(live.show().cue_list(1).unwrap().state.targets, None);
    drive(&mut live, 2);
    accepted(&submit(
        &mut live,
        "edit",
        set(1, vec![waiting, cue(20, vec![overlay(1)], false)]),
    ));
    let ahead = fired + 8 - live.frame_cursor().get();
    drive(&mut live, ahead);
    let state = &live.show().cue_list(1).unwrap().state;
    assert_eq!(state.fault, None, "{state:?}");
    assert_eq!(state.live, Some(20));
    assert_eq!(overlay_on(&live, 1), Some(input(4)));

    // An armed GO whose cue is edited to show Preview before it fires.
    let mut armed = engine();
    accepted(&submit(
        &mut armed,
        "set",
        set(1, vec![cue(10, vec![cut(2)], false)]),
    ));
    armed.tick().unwrap();
    let armed_at = accepted(&submit(&mut armed, "arm", control(1, arm(Some(10), 200))));
    let state = &armed.show().cue_list(1).unwrap().state;
    assert_eq!(state.armed.unwrap().targets, None);
    accepted(&submit(
        &mut armed,
        "edit",
        set(1, vec![cue(10, vec![overlay(2)], false)]),
    ));
    assert!(armed.show().cue_list(1).unwrap().state.armed.is_some());
    let ahead = armed_at + 8 - armed.frame_cursor().get();
    drive(&mut armed, ahead);
    let state = &armed.show().cue_list(1).unwrap().state;
    assert_eq!(state.fault, None, "{state:?}");
    assert_eq!(state.live, Some(10));
    assert_eq!(overlay_on(&armed, 2), Some(input(4)));
}

/// RC-004: an input a cue run in progress (or an armed GO) bound to a
/// dynamic target it has yet to act on cannot be removed, like an input a
/// cue names: the run would otherwise fault at that action. A replayed GO
/// carrying the journaled bindings holds the same input, an input bound to
/// nothing still to run is free, and Stop or Disarm frees it.
#[test]
fn an_input_a_cue_run_still_binds_cannot_be_removed() {
    use fm_model::{InputTarget, TargetBindings};
    let list = set(
        1,
        vec![
            cue(10, vec![CueAction::Wait { frames: 5 }], true),
            cue(
                20,
                vec![CueAction::OverlayOn {
                    channel: 2,
                    input: InputTarget::Selected,
                }],
                false,
            ),
        ],
    );
    let held = |list, cue| {
        Some(RefusalReason::InputInUse {
            input: 3,
            usage: fm_command::InputUse::CueAction { list, cue },
        })
    };
    let remove = |engine: &mut Engine, key: &str, value| {
        submit(
            engine,
            key,
            EngineCommand::RemoveInput {
                input: input(value),
            },
        )
    };

    // A live GO binds the selection 3; the follower after the wait acts on
    // it, so 3 stays while 2, bound to nothing, goes.
    let go = |selected: TargetBindings| EngineCueOperation::Go {
        cue: None,
        targets: selected,
    };
    let selection = TargetBindings {
        selected: Some(input(3)),
        ..TargetBindings::NONE
    };
    let mut live = engine();
    accepted(&submit(&mut live, "set", list.clone()));
    live.tick().unwrap();
    accepted(&submit(&mut live, "go", control(1, go(selection))));
    drive(&mut live, 2);
    let state = &live.show().cue_list(1).unwrap().state;
    assert!(state.pending.is_some(), "{state:?}");
    let refused = remove(&mut live, "remove-3", 3);
    assert_eq!(refusal(&refused).1, held(1, 20));
    assert!(live.show().inputs().contains(&input(3)));
    accepted(&remove(&mut live, "remove-2", 2));
    // A replay GO carries what the authority bound and holds the same input.
    let bound = live.show().cue_list(1).unwrap().state.targets.unwrap();
    let mut replay = engine();
    accepted(&submit(&mut replay, "set", list.clone()));
    replay.tick().unwrap();
    accepted(&submit(&mut replay, "go", control(1, go(bound))));
    assert_eq!(refusal(&remove(&mut replay, "remove-3", 3)).1, held(1, 20));
    // Stop drops the rest of the run, and with it the binding.
    accepted(&submit(
        &mut live,
        "stop",
        control(1, EngineCueOperation::Stop),
    ));
    live.tick().unwrap();
    accepted(&remove(&mut live, "remove-3-stopped", 3));

    // An armed GO binds when armed and holds the selection until disarmed.
    let mut armed = engine();
    accepted(&submit(&mut armed, "set", list));
    armed.tick().unwrap();
    accepted(&submit(
        &mut armed,
        "arm",
        control(
            1,
            EngineCueOperation::Arm {
                cue: None,
                in_millis: 1000,
                targets: selection,
            },
        ),
    ));
    assert_eq!(refusal(&remove(&mut armed, "remove-3", 3)).1, held(1, 20));
    accepted(&submit(
        &mut armed,
        "disarm",
        control(1, EngineCueOperation::Disarm),
    ));
    armed.tick().unwrap();
    accepted(&remove(&mut armed, "remove-3-disarmed", 3));
}
