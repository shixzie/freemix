#[test]
fn restore_engine_seeds_stream_inventory_and_desired_running() {
    let durable = test_project_with_streams([true, false]);
    let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();

    for switcher in [
        snapshot.show().desired_switcher(),
        snapshot.realized_switcher(),
    ] {
        assert_eq!(switcher.streams().len(), 2);
        // Project vector order is preserved.
        assert_eq!(switcher.streams()[0].name(), "Destination 1");
        assert_eq!(
            switcher.streams()[1].id(),
            SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(STREAM_TARGET_B).unwrap())
        );
        assert_eq!(switcher.streams()[1].name(), "Destination 2");
        assert_eq!(
            switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_A).unwrap()
            )),
            Some(true)
        );
        assert_eq!(
            switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_B).unwrap()
            )),
            Some(false)
        );
        assert_eq!(
            switcher.running_stream_targets(),
            &std::collections::BTreeSet::from([SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(STREAM_TARGET_A).unwrap()
            )])
        );
    }
    // Authored running flags survive a restore round trip unchanged.
    assert!(durable.project().stream_targets()[0].running());
    assert!(!durable.project().stream_targets()[1].running());
}

#[test]
fn stream_commands_persist_desired_running_and_reject_unknown_targets() {
    let mut durable = test_project_with_streams([false, false]);
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let wire_target =
        || fm_protocol::WireStreamTargetId::new(NonZeroU128::new(STREAM_TARGET_A).unwrap());
    let execute = |control: &mut ControlService<Policy>,
                   durable: &mut StoredProject,
                   id: &str,
                   key: &str,
                   payload| {
        execute_durable_command(
            control,
            &CountingSaver::default(),
            durable,
            &operator(),
            &server,
            &test_command(id, key, payload),
            0,
        )
    };

    let started = execute(
        &mut control,
        &mut durable,
        "stream-on",
        "stream-on-key",
        CommandPayload::StreamStart {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        started.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert!(matches!(
        started.submission.output.events.as_slice(),
        [fm_protocol::EventMessage {
            payload: fm_protocol::EventPayload::StreamsChanged { .. },
            ..
        }]
    ));
    assert_eq!(durable.position().frames_rendered, 1);
    assert!(durable.project().stream_targets()[0].running());

    let rejected = execute(
        &mut control,
        &mut durable,
        "stream-unknown",
        "stream-unknown-key",
        CommandPayload::StreamStart {
            target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(9_999).unwrap()),
        },
    )
    .unwrap();
    assert!(matches!(
        rejected.submission.output.result,
        CommandResult::Rejected { ref code, .. } if code == "not_found"
    ));
    assert!(!durable.project().stream_targets()[1].running());

    let stopped = execute(
        &mut control,
        &mut durable,
        "stream-off",
        "stream-off-key",
        CommandPayload::StreamStop {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        stopped.submission.output.result,
        CommandResult::Accepted { revision: 2, .. }
    ));
    assert!(!durable.project().stream_targets()[0].running());
    assert_eq!(durable.position().frames_rendered, 2);

    // The replayed start answers from its original receipt and changes
    // nothing about the stored project.
    let replayed = execute(
        &mut control,
        &mut durable,
        "stream-replay",
        "stream-on-key",
        CommandPayload::StreamStart {
            target: wire_target(),
        },
    )
    .unwrap();
    assert!(matches!(
        replayed.submission.output.result,
        CommandResult::Accepted { ref id, revision: 1, .. } if id == "stream-on"
    ));
    assert!(!durable.project().stream_targets()[0].running());

    // The persisted project alone reproduces the live engine's desired
    // stream state on restore.
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
}

/// A plain project whose authored per-slot desired recording flags are
/// `desired` (slot 1 then slot 2).
fn test_project_with_recording(desired: [bool; 2]) -> StoredProject {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    for (slot, active) in ModelRecorderSlot::ALL.into_iter().zip(desired) {
        project.set_recording_desired(slot, active);
    }
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap()
}

/// The wire recorder slot for `number`, which tests only call with 1 or 2.
fn wire_recorder(number: u8) -> fm_protocol::WireRecorderSlot {
    fm_protocol::WireRecorderSlot::new(number).expect("test recorder slots are 1 or 2")
}

#[test]
fn operator_state_text_lists_every_stream_and_both_recorder_flags() {
    let control = test_control(&test_project_with_streams([true, false]));
    let text = operator_state_text(&control, None);

    assert!(text.starts_with("streams=2\n"));
    assert!(text.contains(&format!(
        "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
    )));
    assert!(text.contains(&format!(
        "stream target={STREAM_TARGET_B} name=Destination 2 desired=false"
    )));
    assert!(text.contains("recorders=desired=false,false"));
    for leaked in ["ingest.example.com", "unit-secret-key", "/live"] {
        assert!(!text.contains(leaked), "operator state leaked {leaked}");
    }
}

#[test]
fn operator_state_text_carries_both_desired_recorder_flags() {
    let control = test_control(&test_project_with_recording([true, false]));
    let text = operator_state_text(&control, None);
    assert!(text.contains("recorders=desired=true,false"));

    let control = test_control(&test_project_with_recording([false, true]));
    let text = operator_state_text(&control, None);
    assert!(text.contains("recorders=desired=false,true"));
}

#[test]
fn operator_state_text_truncates_over_the_cap_with_a_marker() {
    let streams: Vec<StreamStatus> = (0..64)
        .map(|index| StreamStatus {
            target: WireStreamTargetId::new(NonZeroU128::new(10_000 + index).unwrap()),
            name: "D".repeat(128),
            desired_running: index % 2 == 0,
            realized: StreamRealizedState::Stopped,
            detail: None,
        })
        .collect();
    let text = render_operator_state(&streams, &[], None);
    assert!(text.len() <= OPERATOR_STATE_MAX_BYTES);
    assert!(text.ends_with(OPERATOR_STATE_TRUNCATION_MARKER));
}

#[test]
fn operator_state_publishes_only_when_a_listener_is_attached() {
    let control = Rc::new(RefCell::new(test_control(&test_project_with_streams([
        true, false,
    ]))));
    let mut process = register_process_shutdown().unwrap();
    process.publish_operator_state(None);
    assert!(process.status.operator_state().is_none());

    process.attach_control(&control);
    process.publish_operator_state(None);
    let published = process
        .status
        .operator_state()
        .expect("operator state was published");
    assert!(published.contains(&format!(
        "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
    )));
}

#[test]
fn startup_reconcile_opens_only_slots_with_a_true_desired_flag() {
    // An unconfigured slot is never selected, even when desired.
    assert_eq!(
        startup_recording_slots([false, false], [true, true]),
        [false, false]
    );
    // A configured slot whose own flag is false stays closed even when
    // the other slot is recording.
    assert_eq!(
        startup_recording_slots([true, true], [false, false]),
        [false, false]
    );
    assert_eq!(
        startup_recording_slots([true, true], [true, false]),
        [true, false]
    );
    assert_eq!(
        startup_recording_slots([true, true], [false, true]),
        [false, true]
    );
    assert_eq!(
        startup_recording_slots([true, true], [true, true]),
        [true, true]
    );
    // An unconfigured slot cannot be selected by its desired flag alone.
    assert_eq!(
        startup_recording_slots([true, false], [true, true]),
        [true, false]
    );
}

#[test]
fn recording_commands_route_to_only_the_addressed_slot() {
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStart {
            recorder: wire_recorder(1),
        }),
        Some((true, 0))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStop {
            recorder: wire_recorder(1),
        }),
        Some((false, 0))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStart {
            recorder: wire_recorder(2),
        }),
        Some((true, 1))
    );
    assert_eq!(
        recording_command_slot(&CommandPayload::RecordStop {
            recorder: wire_recorder(2),
        }),
        Some((false, 1))
    );
    // Every other command family leaves recording realization untouched.
    assert_eq!(recording_command_slot(&CommandPayload::Cut), None);
    assert_eq!(
        recording_command_slot(&CommandPayload::StreamStart {
            target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
        }),
        None
    );
}

#[test]
fn restore_engine_seeds_each_recording_desired_flag_into_both_switchers() {
    for desired_flags in [[false, false], [true, false], [false, true], [true, true]] {
        let durable = test_project_with_recording(desired_flags);
        let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
        // Idle restore demands desired/realized agreement, so BOTH
        // switchers must carry every persisted slot flag or startup would
        // fail.
        for (index, slot) in ModelRecorderSlot::ALL.into_iter().enumerate() {
            let recorder = switcher_recorder_slot(slot);
            assert_eq!(
                snapshot
                    .show()
                    .desired_switcher()
                    .recording_desired(recorder),
                desired_flags[index],
                "slot {} desired flag was not seeded into the show switcher",
                slot.get()
            );
            assert_eq!(
                snapshot.realized_switcher().recording_desired(recorder),
                desired_flags[index],
                "slot {} desired flag was not seeded into the realized switcher",
                slot.get()
            );
        }
    }
}

/// Executes `command` as the native daemon does, realizing each of its
/// frames one pacer frame at a time and noting the Program input of each.
fn native_paced_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    server: &ServerIdentity,
    command: &CommandMessage,
    realized: &mut Vec<(u64, InputId, Option<InputId>)>,
) -> CommandResult {
    execute_durable_command_with_ticks(
        control,
        store,
        durable,
        &operator(),
        server,
        command,
        0,
        None,
        Realize::All,
        |control, server| {
            let outcome = control.tick(server)?;
            realized.push((
                outcome.frame.frame.get(),
                outcome.frame.program.primary,
                outcome.frame.program.secondary,
            ));
            Ok(outcome.runtime_events)
        },
    )
    .unwrap()
    .submission
    .output
    .result
}

#[test]
#[allow(clippy::too_many_lines)]
fn playlist_clock_cuts_program_on_scheduled_frames_under_native_pacing() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use journal::DurableJournal;
    use playlist_clock::{Pacing, PlaylistClock};

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("native-playlist.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let mut clock = PlaylistClock::default();
    let start_frame;
    let mut advances = Vec::new();
    {
        let journal = DurableJournal::open(&store).unwrap();
        let entries = vec![
            PlaylistEntrySpec {
                id: 1,
                input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                duration_frames: 10,
                follow_media: false,
                transition: PlaylistTransitionKind::Cut,
                transition_frames: 0,
                stinger_slot: None,
                enabled: true,
            },
            PlaylistEntrySpec {
                id: 2,
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                duration_frames: 10,
                follow_media: false,
                transition: PlaylistTransitionKind::Fade,
                transition_frames: 5,
                stinger_slot: None,
                enabled: true,
            },
        ];
        let set = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries,
                },
            ),
            &mut realized,
        );
        assert!(matches!(set, CommandResult::Accepted { .. }), "{set:?}");
        // The native pacer realizes idle frames between commands; nothing
        // is pending, so the clock stays quiet.
        for _ in 0..3 {
            assert_eq!(clock.due(&control, Pacing::Engine), None);
            let frame = control.tick(&server).unwrap().frame;
            realized.push((
                frame.frame.get(),
                frame.program.primary,
                frame.program.secondary,
            ));
        }
        let start = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &test_command(
                "start",
                "start-key",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::AfterFrames(10),
                        from: None,
                    },
                },
            ),
            &mut realized,
        );
        assert!(matches!(start, CommandResult::Accepted { .. }), "{start:?}");
        start_frame = control
            .playlist_advance_frame()
            .expect("the start is armed");
        assert!(start_frame > control.frame_cursor());

        // One pass per pacer frame: the clock submits the advance only
        // when its boundary is the next frame to realize, else the
        // pacer realizes an idle frame.
        while control.frame_cursor() < start_frame + 40 {
            if let Some(advance) = clock.due(&control, Pacing::Engine) {
                let frame = advance.frame;
                assert_eq!(frame, control.frame_cursor(), "never early, never late");
                let result = native_paced_command(
                    &mut control,
                    &journal,
                    &mut durable,
                    &server,
                    &advance.command(),
                    &mut realized,
                );
                clock.observe(&advance, &result);
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{result:?}"
                );
                advances.push(frame);
            } else {
                let frame = control.tick(&server).unwrap().frame;
                realized.push((
                    frame.frame.get(),
                    frame.program.primary,
                    frame.program.secondary,
                ));
            }
        }
    }
    assert_eq!(
        advances,
        [start_frame, start_frame + 10, start_frame + 20],
        "entry 1 airs, entry 2 airs, the playlist finishes"
    );
    // Frames are realized once each, in order, with no gap.
    assert!(realized.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1));
    let program_at = |frame: u64| {
        realized
            .iter()
            .find(|(realized, ..)| *realized == frame)
            .map(|&(_, primary, secondary)| (primary, secondary))
            .unwrap()
    };
    let (one, two) = (test_input_id(1), test_input_id(2));
    assert_eq!(program_at(start_frame - 1), (one, None));
    // Entry 1 cuts input 2 to Program on exactly its frame.
    assert_eq!(program_at(start_frame), (two, None));
    assert_eq!(program_at(start_frame + 9), (two, None));
    // Entry 2 fades input 1 in from its frame over five frames.
    assert_eq!(program_at(start_frame + 10).1, Some(one));
    assert_eq!(program_at(start_frame + 15), (one, None));
    assert_eq!(program_at(start_frame + 30), (one, None));
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Finished { last: Some(2) }
    ));

    // The journal holds the clock's advances, so replay alone reproduces
    // the playlist and Program routing the operator saw.
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 5);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
    assert_eq!(replayed.runtime_routing(), durable.runtime_routing());
    assert_eq!(replayed.position(), durable.position());
}

#[test]
fn a_late_native_playlist_advance_replays_on_the_frame_it_ran() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use journal::DurableJournal;
    use playlist_clock::{Pacing, PlaylistClock, advance_command};

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("late-playlist.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let late_frame;
    {
        let journal = DurableJournal::open(&store).unwrap();
        for (id, payload) in [
            (
                "set",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 50,
                        follow_media: false,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        stinger_slot: None,
                        enabled: true,
                    }],
                },
            ),
            (
                "start",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::AfterFrames(5),
                        from: None,
                    },
                },
            ),
        ] {
            let result = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &test_command(id, &format!("{id}-key"), payload),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
        }
        let due = control.playlist_advance_frame().unwrap();
        // The pacer realizes frames past the boundary without a clock
        // pass (as the recorder startup barrier does), so the advance
        // runs on the next frame instead.
        while control.frame_cursor() < due + 7 {
            control.tick(&server).unwrap();
        }
        let mut clock = PlaylistClock::default();
        let advance = clock.due(&control, Pacing::Engine).unwrap();
        let (frame, revision) = (advance.frame, advance.revision);
        assert_eq!(frame, due + 7);
        late_frame = frame;
        let result = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &advance_command(frame, revision),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
    }
    // The entry keeps its scheduled start, so later boundaries do not
    // shift; only its cut lands late.
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, started_frame, .. }
            if started_frame == late_frame - 7
    ));
    assert_eq!(realized.last().unwrap().0, late_frame);
    assert_eq!(realized.last().unwrap().1, test_input_id(2));
    let scan = store.scan_journal().unwrap();
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
}

/// The simulated daemon's frame loop realizes every frame a command is
/// durable past, a scene's In phase as well as a timed transition,
/// although the phase is no work in flight: the scene plays to the end
/// of its phase at wall time instead of holding its first frame until
/// the next command.
#[test]
fn the_simulated_pacer_plays_a_scene_phase_to_where_the_command_settles() {
    use fm_model::{Rgba8 as TestRgba8, Scene as TestScene};
    use fm_types::SceneId as TestSceneId;

    let mut project = test_project().project().clone();
    let scene_id = TestSceneId::new(NonZeroU128::new(1).unwrap());
    project.add_input(Input {
        id: test_input_id(3),
        name: "Scene".into(),
        kind: InputKind::Scene {
            scene_id,
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(TestScene {
        id: scene_id,
        name: "Animated".into(),
        background: TestRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation {
            clip: fm_model::SceneClip::new(fm_model::default_scene_timebase(), 12, 0).unwrap(),
            revision: 0,
        },
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(3)));
    let manifest = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(3)),
            realized_preview_id: Some(test_input_id(3)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let cut = execute_durable_command_with_ticks(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command("cut", "cut-key", CommandPayload::Cut),
        0,
        None,
        Realize::First,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )
    .unwrap();
    assert!(matches!(
        cut.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    // The Cut realized its first frame; the scene's In phase runs on.
    let settled = durable.position().frames_rendered;
    assert_eq!(control.frame_cursor(), 1);
    assert!(settled > 2, "{settled}");
    assert!(!control.work_in_flight());
    assert!(control.scenes_in_motion());

    let rate = durable.project().settings().frame_rate;
    let mut pacer = sessions::SimulatedPacer::default();
    let start = std::time::Instant::now();
    pacer
        .poll(&mut control, &server, rate, settled, start)
        .unwrap();
    // Wall time paces it: one frame (40 ms at 25 fps) at a time.
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_millis(40),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), 2);
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), settled);
    assert!(!control.scenes_in_motion());
    // Settled, it renders no idle frames.
    pacer
        .poll(
            &mut control,
            &server,
            rate,
            settled,
            start + Duration::from_secs(20),
        )
        .unwrap();
    assert_eq!(control.frame_cursor(), settled);
}

/// A native command realizes its first frame and leaves the rest of a
/// timed transition to the frame loop: it is durable as the project the
/// transition ends on, a command committed while the transition runs is
/// accepted (or refused, as a Cut is while one is in flight) on that
/// frame, and journal replay ticks to the frame each recorded command
/// was committed at, landing on every recorded position.
#[test]
#[allow(clippy::too_many_lines)]
fn a_command_committed_mid_transition_replays_onto_its_frame() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("mid-transition.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let execute = |control: &mut ControlService<Policy>,
                   durable: &mut StoredProject,
                   journal: &DurableJournal,
                   id: &str,
                   payload: CommandPayload| {
        execute_durable_command_with_ticks(
            control,
            journal,
            durable,
            &operator(),
            &server,
            &test_command(id, &format!("{id}-key"), payload),
            0,
            None,
            Realize::First,
            |control, server| Ok(control.tick(server)?.runtime_events),
        )
        .unwrap()
        .submission
        .output
        .result
    };
    let mut positions = Vec::new();
    {
        let journal = DurableJournal::open(&store).unwrap();
        for _ in 0..3 {
            control.tick(&server).unwrap();
        }
        let began = control.frame_cursor();
        let fade = execute(
            &mut control,
            &mut durable,
            &journal,
            "fade",
            CommandPayload::Fade {
                duration_frames: 10,
            },
        );
        assert!(matches!(fade, CommandResult::Accepted { .. }), "{fade:?}");
        // Only the first frame was realized; the project is durable as
        // the transition ends.
        assert_eq!(control.frame_cursor(), began + 1);
        assert!(control.work_in_flight());
        assert_eq!(durable.position().frames_rendered, began + 10);
        positions.push(durable.position());
        // The frame loop realizes the transition; another client's
        // edit lands on one of its frames and settles where it ends.
        for _ in 0..3 {
            control.tick(&server).unwrap();
        }
        let renamed = execute(
            &mut control,
            &mut durable,
            &journal,
            "rename",
            CommandPayload::RenameInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                name: "Renamed mid transition".into(),
            },
        );
        assert!(
            matches!(renamed, CommandResult::Accepted { .. }),
            "{renamed:?}"
        );
        assert_eq!(durable.position().frames_rendered, began + 10);
        positions.push(durable.position());
        let cut = execute(
            &mut control,
            &mut durable,
            &journal,
            "cut",
            CommandPayload::Cut,
        );
        assert!(
            matches!(&cut, CommandResult::Rejected { code, .. } if code == "conflict"),
            "{cut:?}"
        );
        while control.work_in_flight() {
            control.tick(&server).unwrap();
        }
        assert_eq!(control.frame_cursor(), began + 10);
        assert_eq!(
            stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap())
                .unwrap()
                .position(),
            durable.position()
        );
        control.tick(&server).unwrap();
        let after = execute(
            &mut control,
            &mut durable,
            &journal,
            "cut-after",
            CommandPayload::Cut,
        );
        assert!(matches!(after, CommandResult::Accepted { .. }), "{after:?}");
        positions.push(durable.position());
    }
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), positions.len());
    for (batch, position) in scan.batches().iter().zip(&positions) {
        assert_eq!(
            journal::decode_mutation(batch.payload()).unwrap().position,
            *position
        );
    }
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.project(), durable.project());
}

/// Idle frames a native daemon realizes after its last command survive a
/// crash through the progress marker: markers land a quarter second of
/// frames apart, replay and recovery tick idle up to the newest one and land
/// on its whole position, and a fold moves idle progress into the manifest.
#[test]
fn idle_progress_markers_replay_past_the_last_command_and_fold_into_the_manifest() {
    use fm_protocol::{PlaylistEntrySpec, PlaylistLoopMode, PlaylistTransitionKind};
    use journal::{
        DurableJournal, PROGRESS_FOLD_SECONDS, decode_progress, frames_in, frames_in_millis,
    };

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("progress.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let rate = manifest.project().settings().frame_rate;
    let control = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let marker_frames = frames_in_millis(journal::PROGRESS_MARKER_MILLIS, rate);
    {
        let journal = DurableJournal::open(&store).unwrap();
        let result = native_paced_command(
            &mut control.borrow_mut(),
            &journal,
            &mut durable,
            &server,
            &test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 50,
                        follow_media: false,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        stinger_slot: None,
                        enabled: true,
                    }],
                },
            ),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
        let command_end = durable.position().frames_rendered;
        let mut progress = NativeProgress::new(&durable);
        // The pacer realizes idle frames; the loop polls after each.
        let mut markers = Vec::new();
        for _ in 0..(marker_frames * 2 + 7) {
            control.borrow_mut().tick(&server).unwrap();
            progress.poll(&control, &journal, &mut durable);
            // Markers are written on the journal's worker.
            journal.flush().unwrap();
            if let Some(marker) = store_progress(&store)
                && markers.last() != Some(&marker.frames_rendered)
            {
                markers.push(marker.frames_rendered);
            }
        }
        assert_eq!(
            markers,
            [
                command_end + marker_frames,
                command_end + 2 * marker_frames,
                command_end + 3 * marker_frames
            ],
            "one marker per marker interval of realized frames"
        );
        assert_eq!(
            durable.position().frames_rendered,
            command_end,
            "markers leave the manifest alone"
        );
    }

    // Replay (and so startup recovery) resumes at the newest marker, on
    // its whole position, not where the last command ended.
    let scan = store.scan_journal().unwrap();
    let marker = decode_progress(scan.progress().unwrap().payload()).unwrap();
    let replayed = replay_journal(
        &manifest,
        &BundleAssets::default(),
        scan.batches(),
        Some(marker),
    )
    .unwrap();
    assert_eq!(replayed.position(), marker);
    assert_eq!(replayed.project().playlist(), durable.project().playlist());
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(recovered.position(), marker);
    assert_eq!(store.load().unwrap().position(), marker);
    // The recovered manifest holds the marker, so it has nothing left to
    // replay and a second recovery changes nothing.
    assert_eq!(load_and_recover(&store, false).unwrap().position(), marker);

    // A fold checkpoints idle progress into the manifest itself.
    let control = Rc::new(RefCell::new(test_control(&recovered)));
    let server = test_server(&control.borrow());
    let mut durable = recovered;
    let journal = DurableJournal::open(&store).unwrap();
    let mut progress = NativeProgress::new(&durable);
    let fold_at = marker.frames_rendered + frames_in(PROGRESS_FOLD_SECONDS, rate);
    while control.borrow().frame_cursor() < fold_at {
        control.borrow_mut().tick(&server).unwrap();
        progress.poll(&control, &journal, &mut durable);
        // Frames come far faster than a real pacer's, so each write is
        // waited for: a poll finding the worker busy defers its write.
        journal.flush().unwrap();
    }
    assert_eq!(durable.position().frames_rendered, fold_at);
    assert_eq!(store.load().unwrap().position(), durable.position());
}

/// The early checkpoint runs on the journal's persistence worker, and a
/// command recorded right after it is handed over still lands after it:
/// the fold covers exactly the mutations recorded before it was asked
/// for, the later append stays in the journal above the new checkpoint,
/// and recovery holds every recorded command.
#[test]
fn a_background_checkpoint_folds_what_preceded_it_and_later_appends_stay_journaled() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("background.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let mut control = test_control(&manifest);
    let server = test_server(&control);
    let mut durable = manifest.clone();
    let mut realized = Vec::new();
    let cut = |index: u64| {
        test_command(
            &format!("cut-{index}"),
            &format!("cut-{index}-key"),
            CommandPayload::Cut,
        )
    };
    let early = 48;
    {
        let journal = DurableJournal::open(&store).unwrap();
        for index in 1..=early {
            let result = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &cut(index),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
        }
        assert_eq!(durable.position().revision, early);
        journal.checkpoint_if_due(&durable).unwrap();
        // Handed over behind the checkpoint, whether or not it finished.
        let result = native_paced_command(
            &mut control,
            &journal,
            &mut durable,
            &server,
            &cut(early + 1),
            &mut realized,
        );
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
        journal.flush().unwrap();
        assert!(!journal.busy());
        assert_eq!(store.load().unwrap().position().revision, early);
        // Dropped without settling, as a crash would leave it.
    }
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.checkpoint_sequence(), early);
    assert_eq!(scan.batches().len(), 1);
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(recovered.position(), durable.position());
}

fn store_progress(store: &ProjectStore) -> Option<ProjectPosition> {
    // The newest current marker, read beside the open writer.
    store
        .scan_journal()
        .ok()?
        .progress()
        .map(|progress| journal::decode_progress(progress.payload()).unwrap())
}

/// Replay refuses, before ticking anything, a journal that asks it to
/// tick idle frames past its bound: a damaged frame number becomes a
/// clear startup error instead of a startup that never finishes.
#[test]
fn replay_refuses_idle_frames_past_its_bound() {
    let manifest = test_project();
    let rate = manifest.project().settings().frame_rate;
    let limit = journal::frames_in(journal::MAX_REPLAY_IDLE_SECONDS, rate);
    let mut budget = IdleReplayBudget::new(rate);
    assert!(budget.check(10, 10 + limit).is_ok());
    assert!(budget.check(10, 11 + limit).is_err());
    budget.charge(limit - 5);
    assert!(budget.check(0, 5).is_ok());
    assert!(budget.check(0, 6).is_err());

    let damaged = ProjectPosition {
        frames_rendered: u64::MAX / 2,
        ..manifest.position()
    };
    let started = Instant::now();
    let error = replay_journal(&manifest, &BundleAssets::default(), &[], Some(damaged))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("progress marker") && error.contains("idle frames"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused before ticking"
    );
}

/// A playlist advance refused at `revision` for a reason that cannot
/// clear by itself.
fn refused_advance(revision: u64) -> CommandResult {
    CommandResult::Rejected {
        id: "advance".into(),
        code: "invalid_command".into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    }
}

#[test]
fn playlist_clock_waits_for_a_new_revision_after_a_final_refusal() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use playlist_clock::{Pacing, PlaylistClock};

    let mut control = test_control(&test_project());
    let principal = operator();
    let set = control
        .submit(
            &principal,
            test_command(
                "set",
                "set-key",
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::Off,
                    entries: vec![PlaylistEntrySpec {
                        id: 1,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                        duration_frames: 10,
                        follow_media: false,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        stinger_slot: None,
                        enabled: true,
                    }],
                },
            ),
            0,
        )
        .unwrap();
    assert!(set.is_accepted());
    let mut clock = PlaylistClock::default();
    // Nothing pending: under native pacing no wall-clock anchor exists,
    // so snapshots report the engine cursor itself.
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    let rate = FrameRate::new(25, 1).unwrap();
    let start = control
        .submit(
            &principal,
            test_command(
                "start",
                "start-key",
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::Now,
                        from: None,
                    },
                },
            ),
            0,
        )
        .unwrap();
    assert!(start.is_accepted());
    let server = test_server(&control);
    control.tick(&server).unwrap();
    let due_frame = control.playlist_advance_frame().unwrap();
    while control.frame_cursor() < due_frame {
        assert_eq!(clock.due(&control, Pacing::Engine), None, "not yet due");
        control.tick(&server).unwrap();
    }
    let advance = clock.due(&control, Pacing::Engine).unwrap();
    let (frame, revision) = (advance.frame, advance.revision);
    assert_eq!(frame, due_frame);
    assert_eq!(
        clock.observed_frame(control.frame_cursor(), rate, Instant::now()),
        control.frame_cursor()
    );
    clock.observe(&advance, &refused_advance(revision));
    // A refusal that cannot clear by itself is not retried however long
    // the show runs at this revision.
    for _ in 0..3 * clock_retry::SLOW_RETRY_FRAMES {
        control.tick(&server).unwrap();
        assert_eq!(clock.due(&control, Pacing::Engine), None);
    }
    assert_eq!(clock.due(&control, Pacing::Engine), None);
    // Another accepted command changes the revision, so the clock tries
    // again; Stop, Pause and Set are accepted on native daemons too.
    let stop = test_command(
        "stop",
        "stop-key",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Stop,
        },
    );
    assert_eq!(
        playlist_clock::playlist_command_refusal(&stop, revision),
        None
    );
    assert!(control.submit(&principal, stop, 0).unwrap().is_accepted());
    assert_eq!(clock.due(&control, Pacing::Engine), None, "stopped");
    assert!(
        playlist_clock::playlist_command_refusal(
            &test_command(
                "client",
                "client-key",
                CommandPayload::PlaylistAdvance { frame }
            ),
            revision,
        )
        .is_some(),
        "only the clock advances the playlist"
    );
}

/// The commands that set cue list `7`, whose one cue waits `wait`
/// frames and then cuts input 1 to Program, and GO it.
fn waiting_cue_commands(wait: u32) -> [CommandMessage; 2] {
    use fm_protocol::{
        CueListSpec, CueOperation, TransitionFavoriteEffect, WireCue, WireCueAction,
    };
    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Open".into(),
            auto_follow: false,
            actions: vec![
                WireCueAction::Wait { frames: wait },
                WireCueAction::Take {
                    input: Some(fm_types::InputTarget::Fixed(
                        fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    )),
                    effect: TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                },
            ],
        }],
    };
    [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default(),
                },
            },
        ),
    ]
}

/// Submits [`waiting_cue_commands`] on `control`: returns the frame the
/// wait ends on.
fn go_waiting_cue(control: &mut ControlService<Policy>, wait: u32) -> u64 {
    for command in waiting_cue_commands(wait) {
        let id = command.id.clone();
        let submitted = control.submit(&operator(), command, 0).unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    let (list, due) = control.cue_advance_frame().expect("the cue waits");
    assert_eq!(list, 7);
    due
}

/// The bug this guards: on the simulated runtime the media clock
/// rendered idle frames past a waiting cue's due frame. A cue that rolls
/// a clip and then waits ("play, wait, take") anchors its wait to the
/// frame before its GO, while the media clock anchors to the frame after
/// it, so the media clock always ran ahead: the cue's advance named the
/// next frame on every pass, wall time never reached it, and the cut
/// never came while the clip played. The media clock now stops before a
/// waiting cue's frame, as it does before a playlist boundary, so the
/// cue clock fires the cue on exactly its due frame.
#[test]
#[allow(clippy::too_many_lines)]
fn the_media_clock_never_renders_past_a_waiting_cue_frame() {
    use fm_protocol::{
        CueListSpec, CueOperation, TransitionFavoriteEffect, WireCue, WireCueAction,
        WireCueMediaOperation,
    };
    use playlist_clock::Pacing;

    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mov".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    attach_synthetic_media(&mut control, durable.project()).unwrap();
    let server = test_server(&control);
    let rate = durable.project().settings().frame_rate;
    let frame = Duration::from_millis(40);
    let mut cues = cue_clock::CueClock::default();
    let mut media = media_clock::MediaClock::default();
    let start = Instant::now();
    // One idle pass before the GO, as the session loop runs.
    assert_eq!(
        cues.next(&mut control, Pacing::Wall { now: start, rate }, || true, 0)
            .unwrap(),
        None
    );
    media.poll(&mut control, &server, rate, start).unwrap();

    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Roll".into(),
            auto_follow: false,
            actions: vec![
                WireCueAction::Media {
                    input: fm_types::InputTarget::Fixed(fm_protocol::WireInputId::from_domain(
                        test_input_id(3),
                    )),
                    operation: WireCueMediaOperation::Play,
                },
                WireCueAction::Wait { frames: 10 },
                WireCueAction::Take {
                    input: Some(fm_types::InputTarget::Fixed(
                        fm_protocol::WireInputId::from_domain(test_input_id(2)),
                    )),
                    effect: TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                },
            ],
        }],
    };
    for command in [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default(),
                },
            },
        ),
    ] {
        let id = command.id.clone();
        // As the session loop executes a command: realizing its frames.
        let mut durable = durable.clone();
        let delivery = execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        )
        .unwrap();
        assert!(
            delivery.submission.is_accepted(),
            "{id}: {:?}",
            delivery.submission.output.result
        );
    }
    assert!(control.media_advancing(), "the clip rolls");
    let (_, due) = control.cue_advance_frame().expect("the cue waits");

    let mut fired = None;
    for pass in 1..=50 {
        let at = start + frame * pass;
        if let Some(advance) = cues
            .next(&mut control, Pacing::Wall { now: at, rate }, || true, 0)
            .unwrap()
        {
            fired = Some(advance);
            break;
        }
        media.poll(&mut control, &server, rate, at).unwrap();
        assert!(
            control.frame_cursor() < due,
            "pass {pass}: the media clock rendered frame {due}, the cue's"
        );
    }
    let advance = fired.expect("the cue fired while the clip played");
    assert_eq!((advance.list, advance.frame), (7, due), "on its frame");
    let ran = control.submit(&operator(), advance.command(), 0).unwrap();
    assert!(ran.is_accepted(), "{:?}", ran.output.result);
}

/// A client's playlist control on the simulated runtime first catches
/// the engine up to the playlist's wall-clock frame; it must stop before
/// a waiting cue's frame as the cue clock's own catch-up does, or the
/// cue's advance comes late, on whatever frame the control left.
#[test]
fn a_playlist_catch_up_never_renders_a_waiting_cue_frame() {
    use playlist_clock::{Pacing, PlaylistClock};

    let durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let rate = durable.project().settings().frame_rate;
    for command in playlist_commands(500, fm_protocol::PlaylistStart::Now) {
        assert!(
            control
                .submit(&operator(), command, 0)
                .unwrap()
                .is_accepted()
        );
    }
    control.tick(&server).unwrap();
    let start = Instant::now();
    let mut clock = PlaylistClock::default();
    assert_eq!(
        clock
            .next(&mut control, Pacing::Wall { now: start, rate }, || true, 0)
            .unwrap(),
        None
    );
    let due = go_waiting_cue(&mut control, 5);
    let boundary = control.playlist_advance_frame().unwrap();
    assert!(boundary > due + 20, "boundary {boundary}, cue due {due}");
    let later = start + Duration::from_millis(40) * u32::try_from(due + 10).unwrap();
    clock.catch_up(&mut control, &server, rate, later).unwrap();
    assert_eq!(control.frame_cursor(), due - 1, "caught up to the cue");
    assert_eq!(control.cue_advance_frame(), Some((7, due)), "not late");
}

/// A journal that fails every record while `available` is false, as a
/// failing or full show disk does, and keeps what it recorded.
/// Each record (one command's, or a whole group's) is one attempt: one
/// append and disk sync on a real journal.
struct FlakyJournal {
    available: Cell<bool>,
    attempts: Cell<u32>,
    recorded: RefCell<Vec<CommandMessage>>,
    /// Every group commit recorded, in order.
    groups: RefCell<Vec<Vec<journal::GroupRecord>>>,
}

impl FlakyJournal {
    fn new() -> Self {
        Self {
            available: Cell::new(true),
            attempts: Cell::new(0),
            recorded: RefCell::new(Vec::new()),
            groups: RefCell::new(Vec::new()),
        }
    }
}

impl DurableStore for FlakyJournal {
    fn groups_commits(&self) -> bool {
        true
    }

    fn record_group(
        &self,
        _base: &StoredProject,
        records: &[journal::GroupRecord],
        _last: &StoredProject,
    ) -> AppResult<()> {
        self.attempts.set(self.attempts.get() + 1);
        if !self.available.get() {
            return Err(AppFailure("injected journal unavailability".into()).into());
        }
        self.recorded
            .borrow_mut()
            .extend(records.iter().map(|record| record.command.clone()));
        self.groups.borrow_mut().push(records.to_vec());
        Ok(())
    }

    fn record(
        &self,
        command: &CommandMessage,
        _now_millis: u64,
        _began_frame: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        self.attempts.set(self.attempts.get() + 1);
        if !self.available.get() {
            return Err(AppFailure("injected journal unavailability".into()).into());
        }
        self.recorded.borrow_mut().push(command.clone());
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }
}

fn group_members(commands: &[CommandMessage], server: &ServerIdentity) -> Vec<GroupMember> {
    commands
        .iter()
        .map(|command| GroupMember {
            principal: operator(),
            server: server.clone(),
            command: command.clone(),
        })
        .collect()
}

/// Five clients' commands for one pass: two renames of one input whose
/// order decides its name, a cut that expects the revision before both
/// (refused as stale, so never journaled), then a fade and a cut.
fn group_commands() -> Vec<CommandMessage> {
    let rename = |name: &str| CommandPayload::RenameInput {
        input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
        name: name.into(),
    };
    let mut stale = test_command("stale", "stale-key", CommandPayload::Cut);
    stale.expected_revision = Some(0);
    vec![
        test_command("rename-first", "rename-first-key", rename("First")),
        test_command("rename-second", "rename-second-key", rename("Second")),
        stale,
        test_command(
            "fade",
            "fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        // Accepted on the fade's second frame, which the frame loop
        // would realize next (a Cut there is refused as in flight).
        test_command("rename-third", "rename-third-key", rename("Third")),
    ]
}

/// A native group commit's members all begin on the frame the pacer
/// realizes next and realize none of their own: each runs on the state
/// the one before it left (a cut during the group's fade is refused as
/// in flight, as it would be one by one), every record names that one
/// frame, the frame is due once for the group, and replaying the
/// group's journal reproduces every member's acknowledged position and
/// the routing the group left.
#[test]
fn native_group_members_begin_on_one_frame_and_replay_exactly() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("native-group.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    // Idle frames the pacer realized before the group.
    for _ in 0..3 {
        control.borrow_mut().tick(&server).unwrap();
    }
    let frame = control.borrow().frame_cursor();
    let rename = |name: &str| CommandPayload::RenameInput {
        input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
        name: name.into(),
    };
    let commands = [
        test_command("cut-1", "cut-1-key", CommandPayload::Cut),
        test_command("cut-2", "cut-2-key", CommandPayload::Cut),
        test_command("rename", "rename-key", rename("Grouped")),
        test_command("cut-3", "cut-3-key", CommandPayload::Cut),
        test_command(
            "fade",
            "fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        test_command("in-flight", "in-flight-key", CommandPayload::Cut),
    ];
    let members = group_members(&commands, &server);
    let mut durable = manifest.clone();
    let journal = DurableJournal::open(&store).unwrap();
    let mut speculation = control.borrow().speculate();
    let recorder = GroupRecorder::new(&journal);
    let mut staged = durable.clone();
    let mut frame_due = false;
    let results: Vec<CommandResult> = members
        .iter()
        .map(|member| {
            execute_native_group_member(
                &mut speculation,
                &recorder,
                &mut staged,
                member,
                0,
                None,
                &mut frame_due,
            )
            .unwrap()
            .result
        })
        .collect();
    assert!(frame_due);
    let revisions: Vec<Option<u64>> = results
        .iter()
        .map(|result| match result {
            CommandResult::Accepted { revision, .. } => Some(*revision),
            CommandResult::Rejected { .. } => None,
        })
        .collect();
    assert_eq!(
        revisions,
        [Some(1), Some(2), Some(3), Some(4), Some(5), None],
        "{results:?}"
    );
    let records = recorder.records.take();
    assert_eq!(
        records
            .iter()
            .filter_map(journal::GroupRecord::appended_revision)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5],
        "the refused cut is only a receipt"
    );
    assert!(records.iter().all(|record| record.began_frame == frame));
    assert_eq!(
        speculation.frame_cursor(),
        frame,
        "no member realized a frame"
    );

    journal.record_group(&durable, &records, &staged).unwrap();
    let _ = control.borrow_mut().adopt(speculation);
    durable = staged;
    // The group's one frame, then the fade's rest, as the pacer
    // realizes them.
    while control.borrow().work_in_flight() || control.borrow().frame_cursor() == frame {
        control.borrow_mut().tick(&server).unwrap();
    }
    drop(journal);

    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 5);
    let replayed =
        replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
    assert_eq!(replayed.position(), durable.position());
    assert_eq!(replayed.runtime_routing(), durable.runtime_routing());
    assert_eq!(replayed.project(), durable.project());
}

/// A native group member naming host media (here an image input's
/// picture) brings it into the bundle first, as the same command alone
/// does on either daemon and in a simulated group: it is journaled with
/// the picture's `asset://` URI, not refused as naming no project asset.
#[test]
fn a_native_group_member_imports_the_host_media_it_names() {
    use journal::DurableJournal;

    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host");
    fs::create_dir_all(&host).unwrap();
    let picture = host.join("logo.png");
    image::RgbaImage::new(2, 2).save(&picture).unwrap();
    let store = ProjectStore::new(directory.path().join("native-import.freemix")).unwrap();
    let manifest = test_project();
    store.save(&manifest).unwrap();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&manifest)));
    let server = test_server(&control.borrow());
    let input = fm_protocol::WireInputId::from_domain(test_input_id(9));
    let commands = [test_command(
        "add-logo",
        "add-logo-key",
        CommandPayload::AddInput {
            input,
            name: "Logo".into(),
            source: fm_protocol::LiveInputSource::Image(Box::new(
                fm_types::still::ImageInputConfig::new(
                    picture.to_string_lossy(),
                    fm_types::OutputScaling::Fill,
                ),
            )),
        },
    )];
    let members = group_members(&commands, &server);
    let journal = DurableJournal::open(&store).unwrap();
    let mut speculation = control.borrow().speculate();
    let recorder = GroupRecorder::new(&journal);
    let mut staged = manifest.clone();
    let mut frame_due = false;
    let result = execute_native_group_member(
        &mut speculation,
        &recorder,
        &mut staged,
        &members[0],
        0,
        None,
        &mut frame_due,
    )
    .unwrap()
    .result;
    assert!(
        matches!(result, CommandResult::Accepted { revision: 1, .. }),
        "{result:?}"
    );
    let added = staged
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == test_input_id(9))
        .expect("the image input");
    assert_eq!(added.kind.file_asset_uris(), ["asset://logo.png"]);
    assert!(store.assets_root().join("logo.png").is_file());
    let records = recorder.records.take();
    let CommandPayload::AddInput {
        source: fm_protocol::LiveInputSource::Image(image),
        ..
    } = &records[0].command.payload
    else {
        panic!("{:?}", records[0].command);
    };
    assert_eq!(image.asset_uri, "asset://logo.png");
}

/// The receipts of accepted commands: a refusal's receipt rides the
/// next checkpoint, so a crash before it may lose it by design.
fn accepted_receipts(project: &StoredProject) -> Vec<&fm_persistence::IdempotencyReceipt> {
    project
        .idempotency_receipts()
        .iter()
        .filter(|receipt| {
            matches!(
                receipt.outcome(),
                fm_persistence::ReceiptOutcome::Accepted { .. }
            )
        })
        .collect()
}

fn durable_revisions(subscription: &fm_control::Subscription) -> Vec<u64> {
    std::iter::from_fn(|| subscription.try_recv().ok())
        .filter_map(|event| match event {
            fm_control::LiveEvent::Durable(event) => Some(event.cursor.revision),
            fm_control::LiveEvent::Runtime(_) => None,
        })
        .collect()
}

/// Several clients' commands in one pass become durable with one
/// append, in the order the pass took them: each executes against the
/// state the one before it left, exactly as one at a time, the refused
/// one is not journaled, and the live service and its subscribers see
/// the group only after its append.
#[test]
fn a_group_commit_records_several_clients_commands_with_one_append_in_order() {
    let commands = group_commands();
    let one_at_a_time = FlakyJournal::new();
    let mut sequential = test_project();
    let sequential_control: SharedControl = Rc::new(RefCell::new(test_control(&sequential)));
    let server = test_server(&sequential_control.borrow());
    let expected: Vec<CommandResult> = commands
        .iter()
        .map(|command| {
            execute_authority_command(
                &sequential_control,
                &one_at_a_time,
                &mut sequential,
                &operator(),
                &server,
                command,
                0,
                None,
                None,
            )
            .unwrap()
            .result
        })
        .collect();
    assert_eq!(one_at_a_time.attempts.get(), 5, "one append per command");

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let subscription = control.borrow_mut().subscribe().unwrap();
    let mut admitted_at = Vec::new();
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &group_members(&commands, &server),
        None,
        None,
        |member, speculation, _, _| {
            admitted_at.push((member, speculation.diagnostics().current_revision));
            Ok(None)
        },
    )
    .unwrap();

    assert_eq!(store.attempts.get(), 1, "one append for the whole group");
    assert_eq!(
        admitted_at,
        [(0, 0), (1, 1), (2, 2), (3, 2), (4, 3)],
        "each member runs against the state the one before it left"
    );
    let results: Vec<CommandResult> = outcomes
        .into_iter()
        .map(|outcome| {
            assert!(outcome.admitted);
            outcome.delivery.result
        })
        .collect();
    assert_eq!(results, expected);
    assert!(matches!(&results[2], CommandResult::Rejected { .. }));
    let groups = store.groups.borrow();
    let [group] = groups.as_slice() else {
        panic!("one group expected, got {}", groups.len());
    };
    let journaled: Vec<(&str, Option<u64>)> = group
        .iter()
        .map(|record| (record.command.id.as_str(), record.appended_revision()))
        .collect();
    assert_eq!(
        journaled,
        [
            ("rename-first", Some(1)),
            ("rename-second", Some(2)),
            ("stale", None),
            ("fade", Some(3)),
            ("rename-third", Some(4)),
        ]
    );
    assert_eq!(durable.position(), sequential.position());
    assert_eq!(durable.project(), sequential.project());
    assert_eq!(durable.project().inputs()[0].name, "Third");
    assert_eq!(control.borrow().diagnostics().current_revision, 4);
    assert_eq!(durable_revisions(&subscription), [1, 2, 3, 4]);
}

/// The group's append fails: no command of it applies (no revision, no
/// receipt, no event), every client is refused as not durable and may
/// retry, and the retried group runs afresh.
#[test]
fn a_group_commit_whose_append_fails_applies_none_of_its_commands() {
    let store = FlakyJournal::new();
    store.available.set(false);
    let mut durable = test_project();
    let before = durable.clone();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let subscription = control.borrow_mut().subscribe().unwrap();
    let members = group_members(&group_commands(), &server);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |_, _, _, _| Ok(None),
    )
    .unwrap();

    assert_eq!(store.attempts.get(), 1);
    assert_eq!(outcomes.len(), 5);
    for (outcome, member) in outcomes.iter().zip(&members) {
        assert!(
            matches!(
                &outcome.delivery.result,
                CommandResult::Rejected {
                    id,
                    code,
                    retryable: true,
                    current_revision: 0,
                    reason: Some(fm_protocol::RefusalReason::NotDurable),
                    ..
                } if *id == member.command.id && code == "unavailable"
            ),
            "{:?}",
            outcome.delivery.result
        );
    }
    assert_eq!(durable, before);
    assert_eq!(control.borrow().diagnostics().current_revision, 0);
    assert_eq!(durable_revisions(&subscription), Vec::<u64>::new());
    assert!(store.groups.borrow().is_empty());

    // Nothing was kept, receipts included: the retried group executes.
    store.available.set(true);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |_, _, _, _| Ok(None),
    )
    .unwrap();
    let revisions: Vec<Option<u64>> = outcomes
        .iter()
        .map(|outcome| match &outcome.delivery.result {
            CommandResult::Accepted { revision, .. } => Some(*revision),
            CommandResult::Rejected { .. } => None,
        })
        .collect();
    assert_eq!(revisions, [Some(1), Some(2), None, Some(3), Some(4)]);
    assert_eq!(store.attempts.get(), 2);
    assert_eq!(durable_revisions(&subscription), [1, 2, 3, 4]);
}

/// A group whose append fails answers only what the failure undid as not
/// durable. A retried command whose acceptance was durable before the
/// group is answered from its receipt exactly as it would be alone (it
/// was acknowledged once and must never be denied afterwards), and a
/// session's refusal names the revision the daemon is really at, not
/// one the dropped group reached.
#[test]
fn a_failed_group_commit_keeps_answers_that_did_not_depend_on_it() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let earlier = test_command("earlier", "earlier-key", CommandPayload::Cut);
    let alone = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &operator(),
        &server,
        &earlier,
        0,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        alone.result,
        CommandResult::Accepted { revision: 1, .. }
    ));

    store.available.set(false);
    let fresh = test_command("fresh", "fresh-key", CommandPayload::Cut);
    let limited = test_command("limited", "limited-key", CommandPayload::Cut);
    let members = group_members(&[fresh, earlier.clone(), limited.clone()], &server);
    let outcomes = execute_command_group(
        &control,
        &store,
        &mut durable,
        &members,
        None,
        None,
        |member, speculation, _, _| {
            Ok((member == 2).then(|| {
                session_rejection(
                    &limited,
                    &SessionError::InboundRateLimited,
                    speculation.diagnostics().current_revision,
                )
            }))
        },
    )
    .unwrap();

    assert!(
        matches!(
            &outcomes[0].delivery.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::NotDurable),
                current_revision: 1,
                ..
            }
        ),
        "{:?}",
        outcomes[0].delivery.result
    );
    assert!(
        matches!(
            &outcomes[1].delivery.result,
            CommandResult::Accepted { id, revision: 1, .. } if *id == earlier.id
        ),
        "{:?}",
        outcomes[1].delivery.result
    );
    assert!(
        matches!(
            &outcomes[2].delivery.result,
            CommandResult::Rejected {
                current_revision: 1,
                ..
            }
        ),
        "{:?}",
        outcomes[2].delivery.result
    );
    assert_eq!(control.borrow().diagnostics().current_revision, 1);
}

/// A crash while a group's append was being written: the log holds the
/// command before the group and a torn tail of the group. Recovery lands
/// exactly on the state before the group, none of it partially applied;
/// with the group's commit whole, it lands exactly on the state after
/// the group.
#[test]
fn a_group_commit_torn_by_a_crash_recovers_whole_or_not_at_all() {
    for torn in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        store.save(&test_project()).unwrap();
        let mut durable = load_and_recover(&store, false).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
        let server = test_server(&control.borrow());
        let alone = execute_authority_command(
            &control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "alone",
                "alone-key",
                CommandPayload::Fade { duration_frames: 4 },
            ),
            0,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            alone.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        // The frame loop realizes the rest of the fade before the group.
        while control.borrow().work_in_flight() {
            control.borrow_mut().tick(&server).unwrap();
        }
        let before_group = durable.clone();
        let log = store.journal_path().join("journal.db-wal");
        let log_before = std::fs::metadata(&log).unwrap().len();
        let outcomes = execute_command_group(
            &control,
            &journal,
            &mut durable,
            &group_members(&group_commands(), &server),
            None,
            None,
            |_, _, _, _| Ok(None),
        )
        .unwrap();
        assert!(matches!(
            outcomes[4].delivery.result,
            CommandResult::Accepted { revision: 5, .. }
        ));
        // Killed: no checkpoint, no clean stop.
        drop(journal);
        let log_after = std::fs::metadata(&log).unwrap().len();
        assert!(log_after > log_before, "the group was appended to the log");
        if torn {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(log_after - 7)
                .unwrap();
        }

        let recovered = load_and_recover(&store, false).unwrap();
        let expected = if torn { &before_group } else { &durable };
        assert_eq!(recovered.position(), expected.position(), "torn: {torn}");
        assert_eq!(recovered.project(), expected.project(), "torn: {torn}");
        assert_eq!(
            accepted_receipts(&recovered),
            accepted_receipts(expected),
            "torn: {torn}"
        );
        assert_eq!(
            accepted_receipts(&recovered).len(),
            if torn { 1 } else { 5 }
        );
    }
}

/// The daemon's clock pass on the simulated runtime, through durable
/// execution: a waiting cue's advance the journal could not record
/// (refused, retryable, nothing committed) is retried on the next frame
/// with no other command, and fires once the journal records again. The
/// accepted retry is the only advance journaled, for the cue's own frame.
#[test]
#[allow(clippy::too_many_lines)]
fn cue_clock_poll_retries_an_advance_the_journal_could_not_record() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    for command in waiting_cue_commands(5) {
        let delivery = execute_authority_command(
            &control,
            &store,
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{:?}",
            delivery.result
        );
    }
    let revision = control.borrow().diagnostics().current_revision;
    let (_, due) = control.borrow().cue_advance_frame().unwrap();
    let cursor = control.borrow().frame_cursor();
    let frame = Duration::from_millis(40);
    let mut clock = cue_clock::CueClock::default();
    let mut poll = |clock: &mut cue_clock::CueClock, at: Instant, window: bool| {
        clock
            .poll(
                &control,
                &store,
                &mut durable,
                &server,
                None,
                None,
                at,
                window,
            )
            .unwrap();
    };
    let start = Instant::now();
    poll(&mut clock, start, true);
    assert_eq!(store.attempts.get(), 2, "not due yet");

    store.available.set(false);
    let reached = start + frame * u32::try_from(due - cursor).unwrap();
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "the first attempt was refused");
    assert_eq!(control.borrow().diagnostics().current_revision, revision);
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "not again on the same frame");
    poll(&mut clock, reached + frame, true);
    assert_eq!(store.attempts.get(), 4, "retried on the next frame");

    store.available.set(true);
    poll(&mut clock, reached + frame * 2, false);
    assert_eq!(store.attempts.get(), 4, "no retry without its window");
    assert_eq!(control.borrow().cue_advance_frame(), Some((7, due)));
    poll(&mut clock, reached + frame * 2, true);
    assert_eq!(
        control.borrow().diagnostics().current_revision,
        revision + 1
    );
    assert_eq!(control.borrow().cue_advance_frame(), None, "the cue ran");
    assert_eq!(durable.position().revision, revision + 1);
    let recorded = store.recorded.borrow();
    assert_eq!(recorded.len(), 3, "only the accepted retry is journaled");
    let advance = recorded.last().unwrap();
    assert_eq!(
        advance.payload,
        CommandPayload::CueAdvance {
            list: 7,
            frame: due
        }
    );
    assert_eq!(
        advance.idempotency_key,
        format!("cue-clock-{revision}-7-{due}-retry-2")
    );
}

/// The bug this guards: a cue advance refused while a playlist take was
/// held for a later frame stalled the waiting cue until some other
/// command changed the revision, although the hold clears by itself.
/// The clock now retries on the next frames: each retry is prepared and
/// dropped while the refusal stands (no receipt, no revision), and the
/// first one the show accepts fires the cue, late, on the frame it names.
#[test]
#[allow(clippy::too_many_lines)]
fn cue_clock_retries_an_advance_refused_by_a_held_playlist_take() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use cue_clock::CueClock;
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
        PlaylistTransitionKind,
    };
    use playlist_clock::Pacing;

    let mut control = test_control(&test_project());
    let principal = operator();
    let server = test_server(&control);
    for (id, payload) in [
        (
            "set-playlist",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::Off,
                entries: [(1, 2), (2, 1)]
                    .map(|(id, input)| PlaylistEntrySpec {
                        id,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(input)),
                        duration_frames: 80,
                        follow_media: false,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        stinger_slot: None,
                        enabled: true,
                    })
                    .to_vec(),
            },
        ),
        (
            "start",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Start {
                    at: PlaylistStart::Now,
                    from: None,
                },
            },
        ),
    ] {
        let submitted = control
            .submit(
                &principal,
                test_command(id, &format!("{id}-key"), payload),
                0,
            )
            .unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    control.tick(&server).unwrap();
    let due = go_waiting_cue(&mut control, 5);
    let boundary = control
        .playlist_advance_frame()
        .expect("a boundary is pending");
    assert!(boundary > due + 40, "boundary {boundary}, cue due {due}");

    let mut clock = CueClock::default();
    while control.frame_cursor() < due {
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || true, 0)
                .unwrap(),
            None
        );
        control.tick(&server).unwrap();
    }
    // The playlist's take is accepted ahead, for its boundary, so the
    // show holds every other command until then.
    let take = control
        .submit(
            &principal,
            playlist_clock::advance_command(boundary, control.diagnostics().current_revision),
            0,
        )
        .unwrap();
    assert!(take.is_accepted(), "{:?}", take.output.result);
    let revision = control.diagnostics().current_revision;
    // A first attempt keeps its frame: it never asks for the window a
    // retry waits for (a client command's, under native pacing).
    let advance = clock
        .next(
            &mut control,
            Pacing::Engine,
            || panic!("a first attempt asked for a retry window"),
            0,
        )
        .unwrap()
        .expect("the wait ended");
    assert_eq!(
        (advance.list, advance.revision, advance.retry),
        (7, revision, 0)
    );
    let refused = control.submit(&principal, advance.command(), 0).unwrap();
    assert!(
        matches!(
            &refused.output.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::PlaylistTakeScheduled { frame }),
                ..
            } if *frame == boundary
        ),
        "{:?}",
        refused.output.result
    );
    clock.observe(&advance, &refused.output.result);
    // Whether the show holds a receipt for the clock's attempt `retry`
    // at `frame`: preparing the same key again replays it.
    let has_receipt = |control: &mut ControlService<Policy>, frame: u64, retry: u32| {
        let mut command = cue_clock::advance_command(7, frame, revision);
        if retry > 0 {
            command.idempotency_key = format!("{}-retry-{retry}", command.idempotency_key);
        }
        matches!(
            control.prepare_submit(&operator(), command, 0).unwrap(),
            PrepareSubmitOutcome::Replayed(_)
        )
    };
    assert!(has_receipt(&mut control, advance.frame, 0));
    // Not again on the same frame.
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );

    // Every frame until the hold clears, the retry is prepared, still
    // refused, and dropped: no receipt and no revision. A pass without a
    // retry window (a session command waiting) does not try at all.
    let mut probes = 0_u32;
    let retried = loop {
        assert!(
            control.frame_cursor() <= boundary + SLOW_RETRY_FRAMES + 1,
            "never retried"
        );
        control.tick(&server).unwrap();
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || false, 0)
                .unwrap(),
            None,
            "a retry ran without its window"
        );
        let attempting = clock.due(&control, Pacing::Engine).is_some();
        if let Some(retried) = clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap()
        {
            break retried;
        }
        assert_eq!(control.diagnostics().current_revision, revision);
        if attempting {
            probes += 1;
            let (_, frame) = control.cue_advance_frame().unwrap();
            assert!(!has_receipt(&mut control, frame, probes), "probe {probes}");
        }
    };
    assert!(control.frame_cursor() >= boundary, "the hold cleared first");
    // The fast retries ran out while the take was held, so the clock
    // backed off: far fewer probes than held frames.
    assert!(probes > FAST_RETRIES, "{probes} probes");
    assert!(
        u64::from(probes) < boundary - due,
        "{probes} probes over {} held frames",
        boundary - due
    );
    assert_eq!((retried.list, retried.revision), (7, revision));
    assert_eq!(retried.retry, probes + 1);
    // Late: the retry names the next frame, exactly as replay finds it.
    assert_eq!(control.cue_advance_frame(), Some((7, retried.frame)));
    assert!(retried.frame > boundary);
    let fired = control.submit(&principal, retried.command(), 0).unwrap();
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    clock.observe(&retried, &fired.output.result);
    control.tick(&server).unwrap();
    control.tick(&server).unwrap();
    assert_eq!(control.cue_advance_frame(), None, "the cue ran");
    assert_eq!(
        live_engine_snapshot(&mut control)
            .realized_switcher()
            .program(),
        test_input_id(1),
        "its cut is on Program"
    );
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );
}

/// A refusal that cannot clear by itself is not retried at the same
/// revision; a retryable one is retried on every frame at first and then
/// every `SLOW_RETRY_FRAMES`; another accepted command (a new revision)
/// makes the next advance a first attempt again.
#[test]
fn cue_clock_backs_off_retries_and_waits_for_a_new_revision_after_a_final_refusal() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use cue_clock::CueClock;
    use playlist_clock::Pacing;

    let refusal = |code: &str, retryable: bool| CommandResult::Rejected {
        id: "advance".into(),
        code: code.into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: 0,
        retryable,
        reason: None,
    };
    let mut control = test_control(&test_project());
    let server = test_server(&control);
    let due = go_waiting_cue(&mut control, 3);
    while control.frame_cursor() < due {
        control.tick(&server).unwrap();
    }
    let mut clock = CueClock::default();

    // Final: nothing until the revision changes.
    let first = clock
        .next(&mut control, Pacing::Engine, || true, 0)
        .unwrap()
        .unwrap();
    assert_eq!(first.retry, 0);
    clock.observe(&first, &refusal("invalid_command", false));
    for _ in 0..3 * SLOW_RETRY_FRAMES {
        control.tick(&server).unwrap();
        assert_eq!(clock.due(&control, Pacing::Engine), None);
    }
    let cut = control
        .submit(
            &operator(),
            test_command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(cut.is_accepted());
    let fresh = clock.due(&control, Pacing::Engine).expect("a new revision");
    assert_eq!((fresh.retry, fresh.revision), (0, first.revision + 1));

    // Retryable: every frame for the fast retries, then backed off.
    let fast = usize::try_from(FAST_RETRIES).unwrap();
    let mut attempt = fresh;
    let mut gaps = Vec::new();
    let mut last = control.frame_cursor();
    while gaps.len() < fast + 3 {
        clock.observe(&attempt, &refusal("unavailable", true));
        attempt = loop {
            control.tick(&server).unwrap();
            if let Some(next) = clock.due(&control, Pacing::Engine) {
                break next;
            }
        };
        gaps.push(control.frame_cursor() - last);
        last = control.frame_cursor();
        assert_eq!(attempt.retry, u32::try_from(gaps.len()).unwrap());
    }
    assert!(gaps[..fast].iter().all(|gap| *gap == 1), "{gaps:?}");
    assert!(
        gaps[fast..].iter().all(|gap| *gap == SLOW_RETRY_FRAMES),
        "{gaps:?}"
    );
}

/// The commands that set a two-entry playlist (input 2 for `frames`
/// frames, then input 1 for `frames`, cut) and start it `at`.
fn playlist_commands(frames: u64, at: fm_protocol::PlaylistStart) -> [CommandMessage; 2] {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistTransitionKind,
    };
    [
        test_command(
            "set-playlist",
            "set-playlist-key",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::Off,
                entries: [(1, 2), (2, 1)]
                    .map(|(id, input)| PlaylistEntrySpec {
                        id,
                        input: fm_protocol::WireInputId::from_domain(test_input_id(input)),
                        duration_frames: frames,
                        follow_media: false,
                        transition: PlaylistTransitionKind::Cut,
                        transition_frames: 0,
                        stinger_slot: None,
                        enabled: true,
                    })
                    .to_vec(),
            },
        ),
        test_command(
            "start",
            "start-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Start { at, from: None },
            },
        ),
    ]
}

/// Executes `commands` through durable execution, each accepted.
fn execute_accepted(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    server: &ServerIdentity,
    commands: impl IntoIterator<Item = CommandMessage>,
) {
    for command in commands {
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &operator(),
            server,
            &command,
            0,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{}: {:?}",
            command.id,
            delivery.result
        );
    }
}

/// The playlist clock's pass on the simulated runtime, through durable
/// execution: a boundary's advance the journal could not record
/// (refused, retryable, nothing committed) is retried on the next frame
/// with no other command, and takes the entry once the journal records
/// again. The accepted retry is the only advance journaled, for the
/// boundary's own frame.
#[test]
fn playlist_clock_poll_retries_an_advance_the_journal_could_not_record() {
    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(80, fm_protocol::PlaylistStart::AfterFrames(5)),
    );
    let revision = control.borrow().diagnostics().current_revision;
    let due = control.borrow().playlist_advance_frame().unwrap();
    let cursor = control.borrow().frame_cursor();
    let frame = Duration::from_millis(40);
    let mut clock = playlist_clock::PlaylistClock::default();
    let mut poll = |clock: &mut playlist_clock::PlaylistClock, at: Instant, window: bool| {
        clock
            .poll(
                &control,
                &store,
                &mut durable,
                &server,
                None,
                None,
                at,
                window,
            )
            .unwrap();
    };
    let start = Instant::now();
    poll(&mut clock, start, true);
    assert_eq!(store.attempts.get(), 2, "not due yet");

    store.available.set(false);
    let reached = start + frame * u32::try_from(due - cursor).unwrap();
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "the first attempt was refused");
    assert_eq!(control.borrow().diagnostics().current_revision, revision);
    poll(&mut clock, reached, true);
    assert_eq!(store.attempts.get(), 3, "not again on the same frame");
    poll(&mut clock, reached + frame, true);
    assert_eq!(store.attempts.get(), 4, "retried on the next frame");

    store.available.set(true);
    poll(&mut clock, reached + frame * 2, false);
    assert_eq!(store.attempts.get(), 4, "no retry without its window");
    assert_eq!(control.borrow().playlist_advance_frame(), Some(due));
    poll(&mut clock, reached + frame * 2, true);
    assert_eq!(
        control.borrow().diagnostics().current_revision,
        revision + 1
    );
    assert!(matches!(
        control.borrow().playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, .. }
    ));
    assert_eq!(durable.position().revision, revision + 1);
    let recorded = store.recorded.borrow();
    assert_eq!(recorded.len(), 3, "only the accepted retry is journaled");
    let advance = recorded.last().unwrap();
    assert_eq!(
        advance.payload,
        CommandPayload::PlaylistAdvance { frame: due }
    );
    assert_eq!(
        advance.idempotency_key,
        format!("playlist-clock-{revision}-{due}-retry-2")
    );
}

/// The bug this guards: a playlist advance refused while a cue's frame
/// was held for later stalled the playlist on an idle show until some
/// other command changed the revision, although the hold clears by
/// itself. The clock now retries on the next frames: each retry is
/// prepared and dropped while the refusal stands (no receipt, no
/// revision), and the first one the show accepts takes the entry, late,
/// on the frame it names.
#[test]
#[allow(clippy::too_many_lines)]
fn playlist_clock_retries_an_advance_refused_by_a_held_cue_frame() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use playlist_clock::{Pacing, PlaylistClock};

    let mut control = test_control(&test_project());
    let principal = operator();
    let server = test_server(&control);
    for command in playlist_commands(200, fm_protocol::PlaylistStart::AfterFrames(5)) {
        let id = command.id.clone();
        let submitted = control.submit(&principal, command, 0).unwrap();
        assert!(
            submitted.is_accepted(),
            "{id}: {:?}",
            submitted.output.result
        );
    }
    let boundary = control
        .playlist_advance_frame()
        .expect("the start is armed");
    let due = go_waiting_cue(&mut control, 80);
    assert!(due > boundary + 40, "cue due {due}, boundary {boundary}");
    // The cue's advance is accepted ahead, for its frame, so the show
    // holds every other command until then.
    let held = control
        .submit(
            &principal,
            cue_clock::advance_command(7, due, control.diagnostics().current_revision),
            0,
        )
        .unwrap();
    assert!(held.is_accepted(), "{:?}", held.output.result);
    let revision = control.diagnostics().current_revision;

    let mut clock = PlaylistClock::default();
    while control.frame_cursor() < boundary {
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || true, 0)
                .unwrap(),
            None
        );
        control.tick(&server).unwrap();
    }
    // A first attempt keeps its frame: it never asks for the window a
    // retry waits for (a client command's, under native pacing).
    let advance = clock
        .next(
            &mut control,
            Pacing::Engine,
            || panic!("a first attempt asked for a retry window"),
            0,
        )
        .unwrap()
        .expect("the boundary is reached");
    assert_eq!(
        (advance.frame, advance.revision, advance.retry),
        (boundary, revision, 0)
    );
    let refused = control.submit(&principal, advance.command(), 0).unwrap();
    assert!(
        matches!(
            &refused.output.result,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::CueScheduled { frame }),
                ..
            } if *frame == due
        ),
        "{:?}",
        refused.output.result
    );
    clock.observe(&advance, &refused.output.result);
    // Whether the show holds a receipt for the clock's attempt `retry`
    // at `frame`: preparing the same key again replays it.
    let has_receipt = |control: &mut ControlService<Policy>, frame: u64, retry: u32| {
        let mut command = playlist_clock::advance_command(frame, revision);
        if retry > 0 {
            command.idempotency_key = format!("{}-retry-{retry}", command.idempotency_key);
        }
        matches!(
            control.prepare_submit(&operator(), command, 0).unwrap(),
            PrepareSubmitOutcome::Replayed(_)
        )
    };
    assert!(has_receipt(&mut control, advance.frame, 0));
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None,
        "not again on the same frame"
    );

    // Every frame until the hold clears, the retry is prepared, still
    // refused, and dropped: no receipt and no revision. A pass without a
    // retry window does not try at all.
    let mut probes = 0_u32;
    let retried = loop {
        assert!(
            control.frame_cursor() <= due + SLOW_RETRY_FRAMES + 1,
            "never retried"
        );
        control.tick(&server).unwrap();
        assert_eq!(
            clock
                .next(&mut control, Pacing::Engine, || false, 0)
                .unwrap(),
            None,
            "a retry ran without its window"
        );
        let attempting = clock.due(&control, Pacing::Engine).is_some();
        if let Some(retried) = clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap()
        {
            break retried;
        }
        assert_eq!(control.diagnostics().current_revision, revision);
        if attempting {
            probes += 1;
            let frame = control.playlist_advance_frame().unwrap();
            assert!(!has_receipt(&mut control, frame, probes), "probe {probes}");
        }
    };
    assert!(control.frame_cursor() >= due, "the hold cleared first");
    assert!(probes > FAST_RETRIES, "{probes} probes");
    assert!(
        u64::from(probes) < due - boundary,
        "{probes} probes over {} held frames",
        due - boundary
    );
    assert_eq!(retried.revision, revision);
    assert_eq!(retried.retry, probes + 1);
    // Late: the retry names the next frame, exactly as replay finds it.
    assert_eq!(control.playlist_advance_frame(), Some(retried.frame));
    assert!(retried.frame > due);
    let taken = control.submit(&principal, retried.command(), 0).unwrap();
    assert!(taken.is_accepted(), "{:?}", taken.output.result);
    clock.observe(&retried, &taken.output.result);
    control.tick(&server).unwrap();
    control.tick(&server).unwrap();
    assert!(matches!(
        control.playlist_state().unwrap().state,
        fm_protocol::PlaylistRunState::Playing { entry: 1, .. }
    ));
    assert_eq!(
        live_engine_snapshot(&mut control)
            .realized_switcher()
            .program(),
        test_input_id(2),
        "the entry's input is on Program, after the cue's cut"
    );
    assert_eq!(
        clock
            .next(&mut control, Pacing::Engine, || true, 0)
            .unwrap(),
        None
    );
}

/// A retryable refusal is retried on every frame at first and then
/// every `SLOW_RETRY_FRAMES`; the policy is the cue clock's.
#[test]
fn playlist_clock_backs_off_retries() {
    use clock_retry::{FAST_RETRIES, SLOW_RETRY_FRAMES};
    use playlist_clock::{Pacing, PlaylistClock};

    let refusal = CommandResult::Rejected {
        id: "advance".into(),
        code: "unavailable".into(),
        message: "refused".into(),
        fields: Vec::new(),
        current_revision: 0,
        retryable: true,
        reason: None,
    };
    let mut control = test_control(&test_project());
    let server = test_server(&control);
    for command in playlist_commands(10, fm_protocol::PlaylistStart::AfterFrames(3)) {
        assert!(
            control
                .submit(&operator(), command, 0)
                .unwrap()
                .is_accepted()
        );
    }
    let boundary = control.playlist_advance_frame().unwrap();
    while control.frame_cursor() < boundary {
        control.tick(&server).unwrap();
    }
    let mut clock = PlaylistClock::default();
    let mut attempt = clock.due(&control, Pacing::Engine).expect("due");
    assert_eq!(attempt.retry, 0);
    let fast = usize::try_from(FAST_RETRIES).unwrap();
    let mut gaps = Vec::new();
    let mut last = control.frame_cursor();
    while gaps.len() < fast + 3 {
        clock.observe(&attempt, &refusal);
        attempt = loop {
            control.tick(&server).unwrap();
            if let Some(next) = clock.due(&control, Pacing::Engine) {
                break next;
            }
        };
        gaps.push(control.frame_cursor() - last);
        last = control.frame_cursor();
        assert_eq!(attempt.retry, u32::try_from(gaps.len()).unwrap());
        // Late: each retry names the frame the show has reached.
        assert_eq!(attempt.frame, control.frame_cursor());
    }
    assert!(gaps[..fast].iter().all(|gap| *gap == 1), "{gaps:?}");
    assert!(
        gaps[fast..].iter().all(|gap| *gap == SLOW_RETRY_FRAMES),
        "{gaps:?}"
    );
}

/// A media end trigger whose `playlist_control next` the journal could
/// not record is retried, on the simulated runtime by wall time, while
/// the airing it ends is still on, and only the accepted retry is
/// journaled. A trigger whose airing an operator ended first is dropped.
/// The playlist loops, so the last entry has a next entry too.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_the_journal_could_not_record_is_retried_while_its_airing_is_on() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{PlaylistLoopMode, PlaylistOperation, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = test_project();
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    let server = test_server(&control.borrow());
    let mut commands = playlist_commands(500, fm_protocol::PlaylistStart::Now);
    if let CommandPayload::SetPlaylist { loop_mode, .. } = &mut commands[0].payload {
        *loop_mode = PlaylistLoopMode::All;
    }
    execute_accepted(&control, &store, &mut durable, &server, commands);
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 1, take, .. } = airing(&control) else {
        panic!("entry 1 airs: {:?}", airing(&control));
    };
    let trigger = MediaEndTrigger {
        input: test_input_id(2),
        frame: control.borrow().frame_cursor(),
    };
    assert_eq!(
        media_clock::airing_take(&control.borrow(), trigger),
        Some(take)
    );
    let revision = control.borrow().diagnostics().current_revision;
    let recorded_before = store.recorded.borrow().len();

    store.available.set(false);
    let first = execute_authority_command(
        &control,
        &store,
        &mut durable,
        &development_principal().unwrap(),
        &server,
        &media_clock::end_trigger_command(trigger, revision),
        0,
        None,
        None,
    )
    .unwrap();
    assert!(
        matches!(
            first.result,
            CommandResult::Rejected {
                retryable: true,
                ..
            }
        ),
        "{:?}",
        first.result
    );
    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &first.result,
        start,
    );
    let attempts = store.attempts.get();
    let frame = Duration::from_millis(40);
    let mut poll = |feed: &mut media_clock::EndTriggerFeed, at: Instant, window: bool| {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            at,
            window,
        )
        .unwrap();
    };
    poll(&mut feed, start, true);
    assert_eq!(
        store.attempts.get(),
        attempts,
        "not again on the same frame"
    );
    poll(&mut feed, start + frame, true);
    assert_eq!(store.attempts.get(), attempts + 1, "retried a frame later");
    store.available.set(true);
    poll(&mut feed, start + frame * 2, false);
    assert_eq!(
        store.attempts.get(),
        attempts + 1,
        "no retry without its window"
    );
    poll(&mut feed, start + frame * 2, true);
    assert_eq!(store.attempts.get(), attempts + 2);
    poll(&mut feed, start + frame * 40, true);
    assert_eq!(store.attempts.get(), attempts + 2, "fed once");
    assert!(
        matches!(airing(&control), PlaylistRunState::Playing { entry: 2, .. }),
        "{:?}",
        airing(&control)
    );
    {
        let recorded = store.recorded.borrow();
        assert_eq!(recorded.len(), recorded_before + 1);
        let next = recorded.last().unwrap();
        assert_eq!(
            next.payload,
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Next
            }
        );
        assert_eq!(
            next.idempotency_key,
            format!(
                "media-end-{}-{}-r{revision}-retry-2",
                trigger.input, trigger.frame
            )
        );
    }

    // Entry 2's end trigger is refused too, but the operator stops the
    // playlist before a retry: the trigger no longer applies.
    let PlaylistRunState::Playing { take, .. } = airing(&control) else {
        unreachable!()
    };
    let trigger = MediaEndTrigger {
        input: test_input_id(1),
        frame: control.borrow().frame_cursor(),
    };
    let revision = control.borrow().diagnostics().current_revision;
    let refused = CommandResult::Rejected {
        id: "next".into(),
        code: "conflict".into(),
        message: "held".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    };
    let later = start + frame * 50;
    let cursor = control.borrow().frame_cursor();
    feed.observe_first(
        &control.borrow(),
        trigger,
        take,
        revision,
        cursor,
        &refused,
        later,
    );
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [test_command(
            "stop",
            "stop-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Stop,
            },
        )],
    );
    let attempts = store.attempts.get();
    for passes in 1..=3 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            later + frame * passes,
            true,
        )
        .unwrap();
    }
    assert_eq!(store.attempts.get(), attempts, "dropped with its airing");
    assert!(matches!(airing(&control), PlaylistRunState::Stopped));
}

/// A refused end trigger is owed only while its clip is still at its
/// end: an operator who seeks the clip back while the playlist still airs
/// the same take has it playing again, and the retry is dropped instead
/// of cutting away from it. Left at its end, the same refusal is retried
/// and advances the playlist.
#[test]
#[allow(clippy::too_many_lines)]
fn an_end_trigger_retry_is_dropped_once_its_clip_is_sought_back() {
    use fm_control::MediaEndTrigger;
    use fm_protocol::{MediaOperation, MediaSeekTarget, PlaylistRunState};

    let store = FlakyJournal::new();
    let mut durable = media_test_project("asset://clip.mov");
    let control: SharedControl = Rc::new(RefCell::new(test_control(&durable)));
    attach_synthetic_media(&mut control.borrow_mut(), durable.project()).unwrap();
    let server = test_server(&control.borrow());
    // Entry 1 airs media input 2; entry 2 (media input 1) is next.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        playlist_commands(5_000, fm_protocol::PlaylistStart::Now),
    );
    control.borrow_mut().tick(&server).unwrap();
    let airing = |control: &SharedControl| control.borrow().playlist_state().unwrap().state;
    let PlaylistRunState::Playing { entry: 1, take, .. } = airing(&control) else {
        panic!("entry 1 airs: {:?}", airing(&control));
    };
    let clip = test_input_id(2);
    let end = control
        .borrow()
        .media_status(clip)
        .expect("a media input")
        .playable_end;
    let media = |id: &str, operation: MediaOperation| {
        test_command(
            id,
            &format!("{id}-key"),
            CommandPayload::Media {
                input: fm_protocol::WireInputId::from_domain(clip),
                operation,
            },
        )
    };
    // The clip paused inside its 20-frame end-trigger lead.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [
            media("lead", MediaOperation::EndTriggerLead(Some(20))),
            media(
                "cue-end",
                MediaOperation::Seek(MediaSeekTarget::Frame(end - 8)),
            ),
            media("play", MediaOperation::Play),
            media("pause", MediaOperation::Pause),
        ],
    );
    control.borrow_mut().take_media_end_triggers();
    assert!(media_clock::end_reached(&control.borrow(), clip));

    let refused = |revision: u64| CommandResult::Rejected {
        id: "next".into(),
        code: "conflict".into(),
        message: "held".into(),
        fields: Vec::new(),
        current_revision: revision,
        retryable: false,
        reason: None,
    };
    let trigger = MediaEndTrigger {
        input: clip,
        frame: control.borrow().frame_cursor(),
    };
    let mut feed = media_clock::EndTriggerFeed::default();
    let start = Instant::now();
    let frame = Duration::from_millis(40);
    let observe_refusal = |feed: &mut media_clock::EndTriggerFeed, at: Instant| {
        let control = control.borrow();
        let revision = control.diagnostics().current_revision;
        let cursor = control.frame_cursor();
        feed.observe_first(
            &control,
            trigger,
            take,
            revision,
            cursor,
            &refused(revision),
            at,
        );
    };
    observe_refusal(&mut feed, start);
    assert!(feed.pending());

    // The operator rewinds the clip: it plays again, so no cut.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [media(
            "rewind",
            MediaOperation::Seek(MediaSeekTarget::Frame(0)),
        )],
    );
    assert!(!media_clock::end_reached(&control.borrow(), clip));
    let attempts = store.attempts.get();
    for pass in 1..=30 {
        feed.poll(
            &control,
            &store,
            &mut durable,
            &server,
            None,
            None,
            start + frame * pass,
            true,
        )
        .unwrap();
    }
    assert!(!feed.pending(), "dropped once the clip played again");
    assert_eq!(store.attempts.get(), attempts, "nothing executed");
    assert!(
        matches!(
            airing(&control),
            PlaylistRunState::Playing { entry: 1, take: now, .. } if now == take
        ),
        "{:?}",
        airing(&control)
    );

    // At its end again, the same refusal is retried and cuts.
    execute_accepted(
        &control,
        &store,
        &mut durable,
        &server,
        [media(
            "cue-end-again",
            MediaOperation::Seek(MediaSeekTarget::Frame(end - 8)),
        )],
    );
    control.borrow_mut().take_media_end_triggers();
    assert!(media_clock::end_reached(&control.borrow(), clip));
    let later = start + frame * 40;
    observe_refusal(&mut feed, later);
    assert!(feed.pending());
    feed.poll(
        &control,
        &store,
        &mut durable,
        &server,
        None,
        None,
        later + frame,
        true,
    )
    .unwrap();
    assert!(!feed.pending());
    assert!(
        matches!(airing(&control), PlaylistRunState::Playing { entry: 2, .. }),
        "{:?}",
        airing(&control)
    );
}
