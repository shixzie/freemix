use fm_automation::{
    CommandIntent, Condition, ConditionContext, EventFilter, InputObservation, InputTrigger,
    InputTriggerBinding, InputTriggerError, InputTriggerSet, MAX_INPUT_TRIGGER_ACTIONS, Predicate,
    RunOutcome, RunState, ScriptEngine, ScriptError, ScriptFailure, ScriptLimits, ScriptLogEvent,
    ScriptMacro, ScriptStep, TimelineObservation, TimeoutAction, Trigger, TriggerAction,
    TriggerEngine, Value,
};
use fm_command::Transaction;
use fm_types::InputId;
use std::collections::BTreeSet;
use std::num::NonZeroU128;

type Cmd = &'static str;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn binding(
    id: &str,
    input: InputId,
    trigger: InputTrigger,
    actions: &[(u64, Cmd)],
) -> InputTriggerBinding<Cmd> {
    InputTriggerBinding {
        id: id.into(),
        input,
        trigger,
        actions: actions
            .iter()
            .map(|(delay, cmd)| TriggerAction::new(*delay, CommandIntent::discrete(*cmd)))
            .collect(),
    }
}

fn fired(set: &mut InputTriggerSet<Cmd>, now_ms: u64) -> Vec<(String, u64, Cmd)> {
    set.poll(now_ms)
        .into_iter()
        .map(|fire| (fire.binding_id, fire.due_ms, fire.intent.into_command()))
        .collect()
}

fn playback(position_ms: u64, duration_ms: u64) -> InputObservation {
    InputObservation {
        playback: Some(TimelineObservation::new(position_ms, duration_ms)),
        ..InputObservation::default()
    }
}

#[test]
fn transition_and_overlay_edges_arm_ordered_cumulative_actions() {
    let camera = input(1);
    let mut set = InputTriggerSet::default();
    set.bind(binding(
        "in",
        camera,
        InputTrigger::TransitionIn,
        &[(0, "a"), (100, "b"), (50, "c")],
    ))
    .unwrap();
    set.bind(binding(
        "out",
        camera,
        InputTrigger::TransitionOut,
        &[(0, "out")],
    ))
    .unwrap();
    set.bind(binding(
        "ov2",
        camera,
        InputTrigger::OverlayIn { channel: Some(2) },
        &[(10, "ov2")],
    ))
    .unwrap();
    set.bind(binding(
        "ov-any-out",
        camera,
        InputTrigger::OverlayOut { channel: None },
        &[(0, "ov-out")],
    ))
    .unwrap();

    // The first observation only establishes a baseline, even when live.
    let live = InputObservation {
        on_program: true,
        ..InputObservation::default()
    };
    assert_eq!(set.observe(camera, live, 0).unwrap().matches.len(), 0);
    assert_eq!(set.pending_len(), 0);

    let report = set
        .observe(camera, InputObservation::default(), 10)
        .unwrap();
    assert_eq!(
        report.matches,
        vec![("out".to_owned(), InputTrigger::TransitionOut)]
    );
    assert_eq!(fired(&mut set, 10), vec![("out".into(), 10, "out")]);
    let report = set
        .observe(
            camera,
            InputObservation {
                on_program: true,
                overlays: BTreeSet::from([1, 2]),
                ..InputObservation::default()
            },
            1_000,
        )
        .unwrap();
    assert_eq!(
        report.matches,
        vec![
            ("in".to_owned(), InputTrigger::TransitionIn),
            (
                "ov2".to_owned(),
                InputTrigger::OverlayIn { channel: Some(2) }
            ),
        ]
    );
    assert_eq!(set.pending_len(), 4);
    assert_eq!(
        fired(&mut set, 1_010),
        vec![("in".into(), 1_000, "a"), ("ov2".into(), 1_010, "ov2")]
    );
    assert_eq!(fired(&mut set, 1_149), vec![("in".into(), 1_100, "b")]);
    assert_eq!(fired(&mut set, 1_150), vec![("in".into(), 1_150, "c")]);

    let report = set
        .observe(
            camera,
            InputObservation {
                on_program: false,
                overlays: BTreeSet::new(),
                ..InputObservation::default()
            },
            2_000,
        )
        .unwrap();
    assert_eq!(
        report.matches,
        vec![
            ("out".to_owned(), InputTrigger::TransitionOut),
            (
                "ov-any-out".to_owned(),
                InputTrigger::OverlayOut { channel: Some(1) }
            ),
            (
                "ov-any-out".to_owned(),
                InputTrigger::OverlayOut { channel: Some(2) }
            ),
        ]
    );
}

#[test]
fn timeline_thresholds_fire_once_per_forward_crossing_and_skip_discontinuities() {
    let clip = input(7);
    let mut set = InputTriggerSet::default();
    for (id, trigger) in [
        ("time", InputTrigger::PlaybackTime { position_ms: 5_000 }),
        (
            "remain",
            InputTrigger::PlaybackRemaining {
                remaining_ms: 2_000,
            },
        ),
        ("done", InputTrigger::Completion),
    ] {
        set.bind(binding(id, clip, trigger, &[(0, "x")])).unwrap();
    }
    let matched = |set: &mut InputTriggerSet<Cmd>, obs, now| {
        set.observe(clip, obs, now)
            .unwrap()
            .matches
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };

    assert_eq!(matched(&mut set, playback(0, 10_000), 0).len(), 0);
    assert_eq!(
        matched(&mut set, playback(4_999, 10_000), 1),
        Vec::<String>::new()
    );
    assert_eq!(matched(&mut set, playback(5_000, 10_000), 2), ["time"]);
    assert_eq!(matched(&mut set, playback(6_000, 10_000), 3).len(), 0);
    // Seeking back re-arms; a seek forward across thresholds is suppressed.
    assert_eq!(matched(&mut set, playback(1_000, 10_000), 4).len(), 0);
    let mut seek = playback(9_000, 10_000);
    seek.discontinuity = true;
    assert_eq!(matched(&mut set, seek, 5).len(), 0);
    assert_eq!(matched(&mut set, playback(1_000, 10_000), 6).len(), 0);
    assert_eq!(
        matched(&mut set, playback(8_500, 10_000), 7),
        ["time", "remain"]
    );
    let mut end = playback(10_000, 10_000);
    end.completions = 1;
    assert_eq!(matched(&mut set, end, 8), ["done"]);
}

#[test]
fn countdown_call_zoom_and_replay_triggers_are_edges() {
    let source = input(3);
    let mut set = InputTriggerSet::default();
    for (id, trigger) in [
        ("cd-done", InputTrigger::CountdownCompleted),
        ("cd-time", InputTrigger::CountdownTime { elapsed_ms: 1_000 }),
        (
            "cd-remain",
            InputTrigger::CountdownRemaining { remaining_ms: 500 },
        ),
        ("call-up", InputTrigger::CallConnected),
        ("call-down", InputTrigger::CallDisconnected),
        ("speaker", InputTrigger::ZoomActiveSpeaker),
        ("replay", InputTrigger::ReplayEventsCompleted),
    ] {
        set.bind(binding(id, source, trigger, &[(0, "x")])).unwrap();
    }
    let countdown = |elapsed| InputObservation {
        countdown: Some(TimelineObservation::new(elapsed, 2_000)),
        ..InputObservation::default()
    };
    let ids = |report: fm_automation::InputObserveReport| {
        report
            .matches
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };
    set.observe(source, countdown(0), 0).unwrap();
    assert_eq!(
        ids(set.observe(source, countdown(1_200), 1).unwrap()),
        ["cd-time"]
    );
    assert_eq!(
        ids(set.observe(source, countdown(1_600), 2).unwrap()),
        ["cd-remain"]
    );
    assert_eq!(
        ids(set.observe(source, countdown(2_000), 3).unwrap()),
        ["cd-done"]
    );
    let mut connected = countdown(2_000);
    connected.call_connected = true;
    connected.zoom_active_speaker = true;
    connected.replay_events_completed = 2;
    assert_eq!(
        ids(set.observe(source, connected, 4).unwrap()),
        ["call-up", "speaker", "replay"]
    );
    assert_eq!(
        ids(set.observe(source, countdown(2_000), 5).unwrap()),
        ["call-down"]
    );
}

#[test]
fn trigger_set_validates_rejects_clock_regression_and_cancels() {
    let a = input(1);
    let b = input(2);
    let mut set = InputTriggerSet::default();
    assert_eq!(
        set.bind(binding("empty", a, InputTrigger::TransitionIn, &[])),
        Err(InputTriggerError::EmptyActions("empty".into()))
    );
    let many: Vec<_> = (0..=MAX_INPUT_TRIGGER_ACTIONS).map(|_| (0, "x")).collect();
    assert!(matches!(
        set.bind(binding("many", a, InputTrigger::TransitionIn, &many)),
        Err(InputTriggerError::TooManyActions { .. })
    ));
    set.bind(binding("a", a, InputTrigger::TransitionIn, &[(100, "a")]))
        .unwrap();
    set.bind(binding("b", b, InputTrigger::TransitionIn, &[(100, "b")]))
        .unwrap();
    assert_eq!(
        set.bind(binding("a", b, InputTrigger::TransitionOut, &[(0, "x")])),
        Err(InputTriggerError::DuplicateId("a".into()))
    );
    let live = InputObservation {
        on_program: true,
        ..InputObservation::default()
    };
    for id in [a, b] {
        set.observe(id, InputObservation::default(), 0).unwrap();
    }
    for id in [a, b] {
        set.observe(id, live.clone(), 10).unwrap();
    }
    assert_eq!(
        set.observe(a, InputObservation::default(), 9),
        Err(InputTriggerError::ClockRegression {
            last_ms: 10,
            now_ms: 9
        })
    );
    assert_eq!(set.pending_len(), 2);
    assert_eq!(set.unbind("a"), Ok(1));
    assert_eq!(set.forget_input(b), 1);
    assert_eq!(fired(&mut set, 1_000).len(), 0);
    // A forgotten input re-baselines rather than firing on its next sample.
    assert_eq!(set.observe(b, live, 20).unwrap().matches.len(), 0);
}

#[test]
fn input_trigger_edges_bridge_to_generic_trigger_engine() {
    let camera = input(42);
    let mut set = InputTriggerSet::<Cmd>::default();
    set.bind(binding(
        "ov",
        camera,
        InputTrigger::OverlayIn { channel: None },
        &[(0, "x")],
    ))
    .unwrap();
    set.observe(camera, InputObservation::default(), 0).unwrap();
    let next = InputObservation {
        overlays: BTreeSet::from([3]),
        ..InputObservation::default()
    };
    let preview = set.preview(camera, &next, 50);
    assert_eq!(preview.len(), 1);
    let event = preview[0].to_automation_event();
    assert_eq!(event.kind, "input.overlay_in");
    assert_eq!(event.source.as_deref(), Some("42"));
    assert_eq!(event.fields.get("channel"), Some(&Value::Number(3)));
    assert_eq!(set.pending_len(), 0, "preview arms nothing");

    let mut engine = TriggerEngine::default();
    let mut filter = EventFilter::new("input.overlay_in");
    filter.source = Some("42".into());
    filter.conditions.push(Condition::new(
        "channel",
        Predicate::Equal(Value::Number(3)),
    ));
    engine
        .insert(Trigger {
            id: "generic".into(),
            filter,
            delay_ms: 5,
            conditions: Vec::new(),
            intents: vec![CommandIntent::discrete("generic")],
        })
        .unwrap();
    engine.ingest(&event).unwrap();
    let fires = engine.poll(55, &ConditionContext::new());
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_for_ms, 55);
}

fn cmd(name: Cmd) -> ScriptStep<Cmd> {
    ScriptStep::Command(CommandIntent::discrete(name))
}

fn script(id: &str, steps: Vec<ScriptStep<Cmd>>) -> ScriptMacro<Cmd> {
    ScriptMacro {
        id: id.into(),
        preconditions: Vec::new(),
        steps,
    }
}

fn commands(poll: &fm_automation::ScriptPoll<Cmd>) -> Vec<(u64, Cmd)> {
    poll.dispatches
        .iter()
        .flat_map(|dispatch| {
            dispatch
                .envelope
                .command
                .iter()
                .map(|intent| (dispatch.logical_ms, intent.clone().into_command()))
        })
        .collect()
}

#[test]
fn macro_install_is_all_or_nothing_and_rejects_cycles_and_depth() {
    let mut engine = ScriptEngine::<Cmd>::new(ScriptLimits {
        max_depth: 3,
        ..ScriptLimits::default()
    });
    let missing = engine.install([
        script("ok", vec![cmd("a")]),
        script("bad", vec![ScriptStep::Call("ghost".into())]),
    ]);
    assert_eq!(
        missing,
        Err(ScriptError::UnknownMacro {
            referenced_by: "bad".into(),
            missing: "ghost".into()
        })
    );
    assert!(
        !engine.is_installed("ok"),
        "batch must not partially install"
    );

    let cycle = engine.install([
        script("x", vec![ScriptStep::Call("y".into())]),
        script("y", vec![ScriptStep::Call("x".into())]),
    ]);
    assert_eq!(
        cycle,
        Err(ScriptError::Recursion {
            cycle: vec!["x".into(), "y".into(), "x".into()]
        })
    );

    // Forward references inside one batch are allowed.
    engine
        .install([
            script("outer", vec![ScriptStep::Call("inner".into())]),
            script("inner", vec![cmd("i")]),
        ])
        .unwrap();
    let too_deep = engine.install([script(
        "deep",
        vec![ScriptStep::If {
            conditions: Vec::new(),
            then: vec![ScriptStep::Call("outer".into())],
            otherwise: Vec::new(),
        }],
    )]);
    assert_eq!(
        too_deep,
        Err(ScriptError::TooDeep {
            id: "deep".into(),
            depth: 4,
            maximum: 3
        })
    );
    assert_eq!(
        engine.install([script("empty", Vec::new())]),
        Err(ScriptError::EmptyMacro("empty".into()))
    );
    assert_eq!(
        engine.install([script("inner", vec![cmd("dup")])]),
        Err(ScriptError::DuplicateId("inner".into()))
    );
    assert_eq!(
        engine.uninstall("inner"),
        Err(ScriptError::InUse {
            id: "inner".into(),
            referenced_by: "outer".into()
        })
    );
}

#[test]
fn macro_runs_ordered_steps_with_drift_free_waits_variables_and_calls() {
    let mut engine = ScriptEngine::<Cmd>::default();
    engine
        .install([
            script(
                "lower-third",
                vec![
                    cmd("title-in"),
                    ScriptStep::Wait { ms: 3_000 },
                    cmd("title-out"),
                ],
            ),
            script(
                "show-open",
                vec![
                    cmd("cut-cam1"),
                    ScriptStep::Atomic(
                        Transaction::new([
                            CommandIntent::discrete("audio-up"),
                            CommandIntent::discrete("record"),
                        ])
                        .unwrap(),
                    ),
                    ScriptStep::SetVariable {
                        name: "segment".into(),
                        value: Value::Number(2),
                    },
                    ScriptStep::Wait { ms: 1_000 },
                    ScriptStep::If {
                        conditions: vec![Condition::new(
                            "var.segment",
                            Predicate::GreaterOrEqual(2),
                        )],
                        then: vec![ScriptStep::Call("lower-third".into())],
                        otherwise: vec![cmd("never")],
                    },
                    cmd("done"),
                ],
            ),
        ])
        .unwrap();
    let state = ConditionContext::new();
    let run = engine.start("show-open", 100, &state).unwrap();
    let first = engine.poll(100, &state).unwrap();
    assert_eq!(
        commands(&first),
        vec![(100, "cut-cam1"), (100, "audio-up"), (100, "record")]
    );
    assert_eq!(first.dispatches[1].envelope.command.len(), 2);
    assert_eq!(first.dispatches[1].envelope.id.as_str(), "show-open:1:2");
    assert_eq!(
        engine.run_state(run),
        Some(RunState::WaitingUntil { until_ms: 1_100 })
    );

    // A late poll dispatches at the logical deadline and does not drift the
    // next wait.
    let second = engine.poll(1_350, &state).unwrap();
    assert_eq!(commands(&second), vec![(1_100, "title-in")]);
    assert_eq!(
        engine.run_state(run),
        Some(RunState::WaitingUntil { until_ms: 4_100 })
    );
    let third = engine.poll(4_100, &state).unwrap();
    assert_eq!(
        commands(&third),
        vec![(4_100, "title-out"), (4_100, "done")]
    );
    assert_eq!(third.finished, vec![(run, RunOutcome::Completed)]);
    assert_eq!(engine.active_runs(), 0);

    let events: Vec<_> = engine.log().map(|entry| entry.event.clone()).collect();
    assert!(events.contains(&ScriptLogEvent::Branch { took_then: true }));
    assert!(events.contains(&ScriptLogEvent::Called {
        macro_id: "lower-third".into()
    }));
    assert_eq!(events.last(), Some(&ScriptLogEvent::Completed));
}

#[test]
fn wait_until_conditions_on_replicated_state_honor_timeouts() {
    let ready = Condition::new("replay.ready", Predicate::Equal(Value::Bool(true)));
    let mut engine = ScriptEngine::<Cmd>::default();
    let wait = |on_timeout| ScriptStep::WaitUntil {
        conditions: vec![ready.clone()],
        timeout_ms: 500,
        on_timeout,
    };
    engine
        .install([
            script("strict", vec![wait(TimeoutAction::Fail), cmd("strict-go")]),
            script(
                "lenient",
                vec![wait(TimeoutAction::Continue), cmd("lenient-go")],
            ),
            script("met", vec![wait(TimeoutAction::Fail), cmd("met-go")]),
        ])
        .unwrap();
    let idle = ConditionContext::new();
    let strict = engine.start("strict", 0, &idle).unwrap();
    engine.start("lenient", 0, &idle).unwrap();
    assert_eq!(engine.poll(0, &idle).unwrap().dispatches.len(), 0);
    assert_eq!(
        engine.run_state(strict),
        Some(RunState::WaitingFor { deadline_ms: 500 })
    );
    engine.start("met", 200, &idle).unwrap();
    assert_eq!(engine.poll(200, &idle).unwrap().dispatches.len(), 0);

    let mut live = ConditionContext::new();
    live.insert("replay.ready".into(), Value::Bool(true));
    let poll = engine.poll(300, &live).unwrap();
    assert_eq!(
        commands(&poll),
        vec![(300, "strict-go"), (300, "lenient-go"), (300, "met-go")]
    );
    assert_eq!(poll.finished.len(), 3);

    let strict = engine.start("strict", 1_000, &idle).unwrap();
    let lenient = engine.start("lenient", 1_000, &idle).unwrap();
    let poll = engine.poll(1_600, &idle).unwrap();
    assert_eq!(commands(&poll), vec![(1_500, "lenient-go")]);
    assert_eq!(
        poll.finished,
        vec![
            (strict, RunOutcome::Failed(ScriptFailure::WaitTimedOut)),
            (lenient, RunOutcome::Completed),
        ]
    );
}

#[test]
fn runs_are_bounded_cancellable_and_logged() {
    let mut engine = ScriptEngine::<Cmd>::new(ScriptLimits {
        max_concurrent_runs: 2,
        max_depth: 4,
        max_steps_per_poll: 2,
        max_log_entries: 4,
    });
    let mut gated = script("gated", vec![cmd("g")]);
    gated.preconditions = vec![Condition::new("armed", Predicate::Exists)];
    engine
        .install([
            script("burst", vec![cmd("1"), cmd("2"), cmd("3")]),
            script("slow", vec![ScriptStep::Wait { ms: 1_000 }, cmd("late")]),
            gated,
        ])
        .unwrap();
    let state = ConditionContext::new();
    assert_eq!(
        engine.start("gated", 0, &state),
        Err(ScriptError::PreconditionFailed("gated".into()))
    );
    let burst = engine.start("burst", 0, &state).unwrap();
    let slow = engine.start("slow", 0, &state).unwrap();
    assert_eq!(
        engine.start("slow", 0, &state),
        Err(ScriptError::ConcurrencyLimit { maximum: 2 })
    );

    // The per-poll step budget makes the burst yield after two steps.
    let first = engine.poll(0, &state).unwrap();
    assert_eq!(commands(&first), vec![(0, "1"), (0, "2")]);
    assert_eq!(engine.run_state(burst), Some(RunState::Ready));
    assert!(engine.cancel(slow, 10));
    assert!(!engine.cancel(slow, 10));
    let second = engine.poll(2_000, &state).unwrap();
    assert_eq!(commands(&second), vec![(0, "3")]);
    assert_eq!(second.finished, vec![(burst, RunOutcome::Completed)]);

    assert_eq!(engine.log().count(), 4);
    assert!(engine.dropped_log_entries() > 0);
    assert_eq!(
        engine.poll(1_999, &state),
        Err(ScriptError::ClockRegression {
            last_ms: 2_000,
            now_ms: 1_999
        })
    );

    let a = engine.start("slow", 3_000, &state).unwrap();
    let b = engine.start("slow", 3_000, &state).unwrap();
    assert_eq!(engine.cancel_macro("slow", 3_001), vec![a, b]);
    assert_eq!(engine.active_runs(), 0);
    assert_eq!(
        engine.drain_log().last().map(|e| &e.event),
        Some(&ScriptLogEvent::Cancelled)
    );
}

#[test]
fn uninstalling_a_running_root_unpins_callees_and_fails_cleanly() {
    let mut engine = ScriptEngine::<Cmd>::default();
    engine
        .install([
            script(
                "root",
                vec![ScriptStep::Wait { ms: 10 }, ScriptStep::Call("leaf".into())],
            ),
            script("leaf", vec![cmd("leaf")]),
        ])
        .unwrap();
    let state = ConditionContext::new();
    let run = engine.start("root", 0, &state).unwrap();
    engine.poll(0, &state).unwrap();
    engine.uninstall("root").unwrap();
    engine.uninstall("leaf").unwrap();
    let poll = engine.poll(10, &state).unwrap();
    assert_eq!(poll.dispatches.len(), 0);
    assert_eq!(
        poll.finished,
        vec![(
            run,
            RunOutcome::Failed(ScriptFailure::MacroMissing("leaf".into()))
        )]
    );
}
