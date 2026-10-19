//! RC-011/RC-017 authority: a script run or a cue GO needs the class of
//! every action it performs, checked up front against the session that
//! starts it, so automation never does what the session could not do
//! directly. A GO is checked for the whole chain it arms (its waits and
//! auto-follows run later on the clock); an edit of a list whose cue is
//! waiting needs what it adds to the armed chain. A cue's script step
//! needs every verb its script holds; an edit of a script an armed chain
//! runs needs what its new verbs add.

use fm_auth::{AuthorizationDenial, DenialReason};
use fm_model::{
    CueAction, CueMediaOperation, RecorderSlot, StreamTargetId, TitleFieldId, TransitionEffect,
};
use fm_protocol::{
    CueListSpec, CueOperation, ScriptSpec, TitleAirRequirement, TransitionFavoriteEffect, WireCue,
    WireCueAction, WireOverlayChannelId, WireRecorderSlot, WireStreamTargetId,
};

use super::*;
use crate::action_authority::cue_action_class;

/// The production matrix for Admin; every other principal holds exactly
/// the granted classes.
struct Grants(&'static [CommandClass]);

impl AuthorizationHook for Grants {
    fn authorize(
        &self,
        principal: &Principal,
        command: CommandClass,
    ) -> Result<(), AuthorizationDenial> {
        if principal.roles().contains(&Role::Admin) || self.0.contains(&command) {
            Ok(())
        } else {
            Err(AuthorizationDenial {
                command,
                required: command.required_permission(),
                reason: DenialReason::MissingPermission,
            })
        }
    }
}

const TRANSITION_ONLY: &[CommandClass] = &[CommandClass::Transition];
const TRANSITION_AND_AUDIO: &[CommandClass] =
    &[CommandClass::Transition, CommandClass::ControlAudio];
const EDIT_ONLY: &[CommandClass] = &[CommandClass::EditProject];
const EDIT_AND_TRANSITION: &[CommandClass] = &[CommandClass::EditProject, CommandClass::Transition];

fn granted(classes: &'static [CommandClass]) -> ControlService<Grants> {
    let show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Grants(classes),
        "engine-a",
        "log-a",
        ControlLimits::default(),
    )
}

fn admin() -> Principal {
    principal(Role::Admin)
}

/// A session holding only what [`Grants`] gives it.
fn session() -> Principal {
    principal(Role::Viewer)
}

fn submit(
    control: &mut ControlService<Grants>,
    who: &Principal,
    id: &str,
    payload: CommandPayload,
) -> CommandSubmission {
    let submission = control.submit(who, command(id, id, payload), 0).unwrap();
    control.tick(&server_identity()).unwrap();
    submission
}

fn is_denied(submission: &CommandSubmission) -> bool {
    matches!(
        &submission.output.result,
        CommandResult::Rejected { code, .. } if code == "permission_denied"
    )
}

/// The refusal names the missing permission, carries the typed
/// permission-denied reason, and publishes nothing.
fn assert_denied(submission: &CommandSubmission, missing: &str) {
    let CommandResult::Rejected {
        code,
        message,
        reason,
        ..
    } = &submission.output.result
    else {
        panic!("expected a refusal: {:?}", submission.output.result);
    };
    assert_eq!(code, "permission_denied");
    assert_eq!(reason, &Some(fm_protocol::RefusalReason::PermissionDenied));
    assert!(message.contains(missing), "{message}");
    assert_eq!(submission.output.events, []);
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

fn set_script(script: u64, source: &str) -> CommandPayload {
    CommandPayload::SetScript {
        script,
        spec: Box::new(ScriptSpec {
            name: format!("Script {script}"),
            source: source.to_owned(),
        }),
    }
}

fn run_script() -> CommandPayload {
    CommandPayload::RunScript { script: 1 }
}

fn cue(id: u64, auto_follow: bool, actions: Vec<WireCueAction>) -> WireCue {
    WireCue {
        id,
        name: format!("Cue {id}"),
        auto_follow,
        actions,
    }
}

fn set_list(cues: Vec<WireCue>) -> CommandPayload {
    CommandPayload::SetCueList {
        list: 7,
        spec: Box::new(CueListSpec {
            name: "Show".into(),
            cues,
        }),
    }
}

fn go(cue: Option<u64>) -> CommandPayload {
    CommandPayload::CueControl {
        list: 7,
        operation: CueOperation::Go { cue },
    }
}

fn cut(value: u128) -> WireCueAction {
    WireCueAction::Take {
        input: Some(wire(value)),
        effect: TransitionFavoriteEffect::Cut,
        duration_frames: 0,
    }
}

fn mute(value: u128) -> WireCueAction {
    WireCueAction::AudioMute {
        input: wire(value),
        muted: true,
    }
}

fn strip(control: &ControlService<Grants>, value: u128) -> EngineInputAudioStripState {
    control
        .engine
        .show()
        .input_audio_strip(input(value))
        .unwrap()
}

fn program(control: &ControlService<Grants>) -> InputId {
    control.engine.show().desired_switcher().program()
}

fn audio_strip(value: u128, gain_millidb: i32, muted: bool) -> CommandPayload {
    CommandPayload::SetInputAudioStrip {
        input: wire(value),
        gain_millidb,
        balance_basis_points: 0,
        pan_basis_points: 0,
        muted,
        soloed: false,
        follow_video: false,
        delay_samples: 0,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one case per cue action
fn each_cue_action_needs_the_class_of_its_direct_command() {
    let channel = WireOverlayChannelId::new(1).unwrap();
    let id = NonZeroU128::new(1).unwrap();
    let cases = [
        (
            CueAction::Preview { input: input(2) },
            CommandPayload::SelectPreview { input: wire(2) },
        ),
        (
            CueAction::Take {
                input: Some(input(2)),
                effect: TransitionEffect::Cut,
                duration_frames: 0,
            },
            CommandPayload::Cut,
        ),
        (
            CueAction::OverlayOn {
                channel: 1,
                input: input(2),
            },
            CommandPayload::TakeOverlay {
                channel,
                source: wire(2),
            },
        ),
        (
            CueAction::OverlayOff { channel: 1 },
            CommandPayload::OverlayOff { channel },
        ),
        (
            CueAction::TitleOn {
                input: input(2),
                channel: 1,
            },
            CommandPayload::TakeTitle {
                input: wire(2),
                channel,
                base_revision: 0,
                fields: Vec::new(),
            },
        ),
        (
            CueAction::TitleText {
                input: input(2),
                field: TitleFieldId::new(id),
                text: "x".into(),
            },
            CommandPayload::SetTitleFields {
                input: wire(2),
                base_revision: 0,
                require: TitleAirRequirement::OnAir,
                fields: Vec::new(),
            },
        ),
        (
            CueAction::TitleOff { input: input(2) },
            CommandPayload::TitleOffAir { input: wire(2) },
        ),
        (
            CueAction::FadeToBlack {
                active: true,
                duration_frames: 10,
            },
            CommandPayload::FadeToBlack {
                active: true,
                duration_frames: 10,
            },
        ),
        (
            CueAction::PlaylistStart,
            CommandPayload::PlaylistControl {
                operation: fm_protocol::PlaylistOperation::Stop,
            },
        ),
        (
            CueAction::PlaylistStop,
            CommandPayload::PlaylistControl {
                operation: fm_protocol::PlaylistOperation::Stop,
            },
        ),
        (
            CueAction::AudioGain {
                input: input(2),
                gain_millidb: -6_000,
            },
            audio_strip(2, -6_000, false),
        ),
        (
            CueAction::AudioMute {
                input: input(2),
                muted: true,
            },
            audio_strip(2, 0, true),
        ),
        (
            CueAction::Record {
                recorder: RecorderSlot::new(1).unwrap(),
                start: true,
            },
            CommandPayload::RecordStart {
                recorder: WireRecorderSlot::new(1).unwrap(),
            },
        ),
        (
            CueAction::Stream {
                target: StreamTargetId::new(id),
                start: true,
            },
            CommandPayload::StreamStart {
                target: WireStreamTargetId::new(id),
            },
        ),
        (
            CueAction::Media {
                input: input(2),
                operation: CueMediaOperation::Play,
            },
            CommandPayload::Media {
                input: wire(2),
                operation: fm_protocol::MediaOperation::Play,
            },
        ),
    ];
    for (action, direct) in cases {
        assert_eq!(
            cue_action_class(&action),
            Some(command_class(&direct)),
            "{action:?}"
        );
    }
    assert_eq!(cue_action_class(&CueAction::Wait { frames: 1 }), None);
    assert_eq!(cue_action_class(&CueAction::Delay { millis: 1 }), None);
    // A script step needs the classes of its script's verbs instead.
    assert_eq!(cue_action_class(&CueAction::RunScript { script: 1 }), None);
}

/// The regression: a Transition-only session used to change audio by
/// running a script.
#[test]
fn a_transition_only_session_cannot_change_audio_through_a_script() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_script(1, "cut 2\nmute 3"),
    );
    let before = strip(&control, 3);

    let refused = submit(&mut control, &session(), "run", run_script());
    assert_denied(&refused, "ControlAudio");
    // Nothing ran: not even the cut before the mute.
    assert_eq!(strip(&control, 3), before);
    assert_eq!(program(&control), input(1));

    // Full control is unchanged.
    let ran = submit(&mut control, &admin(), "admin-run", run_script());
    assert!(ran.is_accepted(), "{:?}", ran.output.result);
    assert!(strip(&control, 3).muted);
    assert_eq!(program(&control), input(2));
}

/// The regression: a Transition-only session used to change audio by
/// firing a cue.
#[test]
fn a_transition_only_session_cannot_change_audio_through_a_cue() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_list(vec![
            cue(10, false, vec![cut(2), mute(3)]),
            cue(20, false, vec![cut(1)]),
        ]),
    );
    for (index, payload) in [go(None), go(Some(10))].into_iter().enumerate() {
        let refused = submit(&mut control, &session(), &format!("go{index}"), payload);
        assert_denied(&refused, "ControlAudio");
    }
    assert!(!strip(&control, 3).muted);
    assert_eq!(program(&control), input(1));
    let state = &control.engine.show().cue_list(7).unwrap().state;
    assert_eq!((state.standby, state.live), (Some(10), None));

    // Running a specific cue that only switches is still allowed.
    let fired = submit(&mut control, &session(), "go20", go(Some(20)));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    // Moving standby fires nothing and needs only transition.
    let standby = submit(
        &mut control,
        &session(),
        "standby",
        CommandPayload::CueControl {
            list: 7,
            operation: CueOperation::Standby { cue: 10 },
        },
    );
    assert!(standby.is_accepted(), "{:?}", standby.output.result);

    let admin_go = submit(&mut control, &admin(), "admin-go", go(None));
    assert!(admin_go.is_accepted(), "{:?}", admin_go.output.result);
    assert!(strip(&control, 3).muted);
}

/// Every action category, through a script and through a cue, needs its
/// own class on top of the run's `transition`.
#[test]
fn scripts_and_cues_need_the_class_of_every_action_category() {
    let categories: [(&str, WireCueAction, Option<CommandClass>); 8] = [
        ("cut 3", cut(3), None),
        (
            "preview 3",
            WireCueAction::Preview { input: wire(3) },
            Some(CommandClass::SelectPreview),
        ),
        (
            "overlay 1, 3",
            WireCueAction::OverlayOn {
                channel: WireOverlayChannelId::new(1).unwrap(),
                input: wire(3),
            },
            None,
        ),
        (
            "ftb 10",
            WireCueAction::FadeToBlack {
                active: true,
                duration_frames: 10,
            },
            None,
        ),
        (
            "record_start 1",
            WireCueAction::Record {
                recorder: WireRecorderSlot::new(1).unwrap(),
                start: true,
            },
            None,
        ),
        ("playlist_stop", WireCueAction::PlaylistStop, None),
        (
            "gain 3, -6000",
            WireCueAction::AudioGain {
                input: wire(3),
                gain_millidb: -6_000,
            },
            Some(CommandClass::ControlAudio),
        ),
        ("mute 3", mute(3), Some(CommandClass::ControlAudio)),
    ];
    for (index, (source, action, needs)) in categories.into_iter().enumerate() {
        let mut control = granted(TRANSITION_ONLY);
        submit(&mut control, &admin(), "script", set_script(1, source));
        submit(
            &mut control,
            &admin(),
            "list",
            set_list(vec![cue(10, false, vec![action])]),
        );
        for payload in [run_script(), go(None)] {
            let required = control.required_classes(&payload);
            assert_eq!(required.primary(), CommandClass::Transition);
            let expected: Vec<CommandClass> = action_authority::CLASSES
                .into_iter()
                .filter(|class| {
                    (*class == CommandClass::Transition && needs.is_none()) || Some(*class) == needs
                })
                .collect();
            assert_eq!(required.additional(), expected, "{source} {payload:?}");
            let submission = submit(
                &mut control,
                &session(),
                &format!("run{index}"),
                payload.clone(),
            );
            match needs {
                Some(CommandClass::SelectPreview) => assert_denied(&submission, "SelectPreview"),
                Some(_) => assert_denied(&submission, "ControlAudio"),
                None => assert!(
                    !is_denied(&submission),
                    "{source}: {:?}",
                    submission.output.result
                ),
            }
        }
    }
    // A title text edits project content: a script asking for one needs
    // edit_project before the show is even consulted.
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "script",
        set_script(1, "title_text 2, 1, \"Hello\""),
    );
    assert_denied(
        &submit(&mut control, &session(), "run", run_script()),
        "EditProject",
    );
}

/// The classes come from the run planned on the current show, so a branch
/// that changes audio only on some shows is caught exactly when it would
/// run.
#[test]
fn a_script_is_checked_against_the_branches_it_takes_now() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_script(1, "if program == 2\n  mute 3\nend\ncut 2"),
    );
    let first = submit(&mut control, &session(), "first", run_script());
    assert!(first.is_accepted(), "{:?}", first.output.result);
    assert_eq!(program(&control), input(2));

    let second = submit(&mut control, &session(), "second", run_script());
    assert_denied(&second, "ControlAudio");
    assert!(!strip(&control, 3).muted);
}

/// A GO arms its waits and auto-follows; the whole chain is checked up
/// front, and the clock then runs it on that authority.
#[test]
fn a_go_is_checked_for_the_whole_chain_it_arms() {
    let cues = || {
        vec![
            cue(10, true, vec![cut(2), WireCueAction::Wait { frames: 3 }]),
            cue(20, true, vec![cut(3)]),
            cue(30, false, vec![mute(3)]),
            cue(40, false, vec![cut(1)]),
        ]
    };
    let mut control = granted(TRANSITION_ONLY);
    submit(&mut control, &admin(), "set", set_list(cues()));
    // The mute two follows away is refused up front; nothing fires.
    assert_denied(
        &submit(&mut control, &session(), "go", go(None)),
        "ControlAudio",
    );
    assert_eq!(program(&control), input(1));
    assert_eq!(control.cue_advance_frame(), None);
    // A chain that does not reach it is fine: cue 40 does not follow on.
    let go40 = submit(&mut control, &session(), "go40", go(Some(40)));
    assert!(go40.is_accepted(), "{:?}", go40.output.result);

    // A session that holds audio arms the chain; the clock runs it.
    let mut control = granted(TRANSITION_AND_AUDIO);
    submit(&mut control, &admin(), "set", set_list(cues()));
    let armed = submit(&mut control, &session(), "go", go(None));
    assert!(armed.is_accepted(), "{:?}", armed.output.result);
    let (list, due) = control.cue_advance_frame().unwrap();
    let advance = submit(
        &mut control,
        &admin(),
        "advance",
        CommandPayload::CueAdvance { list, frame: due },
    );
    assert!(advance.is_accepted(), "{:?}", advance.output.result);
    while control.frame_cursor() <= due {
        control.tick(&server_identity()).unwrap();
    }
    assert!(strip(&control, 3).muted);
    let state = &control.engine.show().cue_list(7).unwrap().state;
    assert_eq!((state.live, state.standby), (Some(30), Some(40)));
}

/// An edit of a list whose chain is armed changes what the clock will run:
/// it needs every class it adds to the armed chain, and an undo that would
/// restore such an edit needs it too.
#[test]
fn an_edit_to_an_armed_chain_needs_what_it_adds() {
    let armed = |follow: Vec<WireCueAction>| {
        set_list(vec![
            cue(10, true, vec![cut(2), WireCueAction::Wait { frames: 30 }]),
            cue(20, false, follow),
        ])
    };
    let mut control = granted(EDIT_ONLY);
    submit(&mut control, &admin(), "with-mute", armed(vec![mute(3)]));
    submit(&mut control, &admin(), "without", armed(vec![cut(3)]));
    // Not armed: editing content runs nothing, so it is a plain edit.
    assert_eq!(
        control.required_classes(&armed(vec![mute(3)])).additional(),
        []
    );
    let fired = submit(&mut control, &admin(), "go", go(None));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    assert!(
        control
            .engine
            .show()
            .cue_list(7)
            .unwrap()
            .state
            .pending
            .is_some()
    );

    // Adding a mute to the armed follow needs control_audio.
    assert_denied(
        &submit(&mut control, &session(), "add-mute", armed(vec![mute(3)])),
        "ControlAudio",
    );
    // Changing it within what the GO already authorized is a plain edit.
    let recut = submit(&mut control, &session(), "recut", armed(vec![cut(1)]));
    assert!(recut.is_accepted(), "{:?}", recut.output.result);
    // Undoing that is too; undoing the edit before it would restore the
    // mute into the armed chain and needs control_audio.
    let undo = CommandPayload::Undo {
        expected_edit: None,
    };
    let undone = submit(&mut control, &session(), "undo", undo.clone());
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert!(
        control
            .required_classes(&undo)
            .additional()
            .contains(&CommandClass::ControlAudio)
    );
    assert_denied(
        &submit(&mut control, &session(), "undo-mute", undo),
        "ControlAudio",
    );
}

fn run_step(script: u64) -> WireCueAction {
    WireCueAction::RunScript { script }
}

fn wait(frames: u32) -> WireCueAction {
    WireCueAction::Wait { frames }
}

fn cue_state(control: &ControlService<Grants>) -> fm_model::CueRunState {
    control.engine.show().cue_list(7).unwrap().state.clone()
}

/// Runs the clock through the waiting cue's due frame.
fn advance(control: &mut ControlService<Grants>, id: &str) -> CommandSubmission {
    let (list, due) = control.cue_advance_frame().unwrap();
    let advanced = submit(
        control,
        &admin(),
        id,
        CommandPayload::CueAdvance { list, frame: due },
    );
    while control.frame_cursor() <= due {
        control.tick(&server_identity()).unwrap();
    }
    advanced
}

/// A cue's script step (Protocol 2.60) may run on a later frame against a
/// show nobody knows yet, so a GO needs the class of every verb the script
/// holds, in any branch, not only the branches it would take now.
#[test]
fn a_cue_that_runs_a_script_needs_every_verb_the_script_holds() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "script",
        set_script(1, "if program == 3\n  mute 3\nend\ncut 2"),
    );
    submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![cue(10, false, vec![run_step(1)])]),
    );
    // Run directly, the script only cuts on this show: transition is enough.
    assert_eq!(
        control.required_classes(&run_script()).additional(),
        [CommandClass::Transition]
    );
    // A cue running it needs control_audio for the branch not taken now.
    assert_eq!(
        control.required_classes(&go(None)).additional(),
        [CommandClass::Transition, CommandClass::ControlAudio]
    );
    assert_denied(
        &submit(&mut control, &session(), "go", go(None)),
        "ControlAudio",
    );
    assert_eq!(program(&control), input(1));

    let mut control = granted(TRANSITION_AND_AUDIO);
    submit(
        &mut control,
        &admin(),
        "script",
        set_script(1, "if program == 3\n  mute 3\nend\ncut 2"),
    );
    submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![cue(10, false, vec![run_step(1)])]),
    );
    let fired = submit(&mut control, &session(), "go", go(None));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    assert_eq!(program(&control), input(2));
    assert!(!strip(&control, 3).muted);
}

/// A script step after a wait runs on its due frame, on the show of that
/// frame, on the authority the GO checked up front.
#[test]
fn a_script_step_after_a_wait_runs_on_the_show_of_its_frame() {
    let mut control = granted(TRANSITION_AND_AUDIO);
    submit(
        &mut control,
        &admin(),
        "script",
        set_script(1, "if program == 2\n  mute 3\nend\ncut 3"),
    );
    submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![cue(10, false, vec![cut(2), wait(3), run_step(1)])]),
    );
    let fired = submit(&mut control, &session(), "go", go(None));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    assert_eq!(program(&control), input(2));
    assert!(!strip(&control, 3).muted);
    let advanced = advance(&mut control, "advance");
    assert!(advanced.is_accepted(), "{:?}", advanced.output.result);
    // The script saw the cut to 2 the GO made, so it muted 3.
    assert!(strip(&control, 3).muted);
    assert_eq!(program(&control), input(3));
    let state = cue_state(&control);
    assert_eq!((state.pending, state.fault), (None, None));
}

/// A failing script fails its step: a GO is refused whole with the
/// script's typed reason and nothing of the frame applies; a deferred step
/// stops its cue with the fault set, nothing of that frame applied.
#[test]
fn a_failing_script_fails_its_cue_step_with_a_typed_reason() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "script",
        set_script(1, "cut 3\npreview 9"),
    );
    submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![
            cue(
                10,
                false,
                vec![WireCueAction::Preview { input: wire(3) }, run_step(1)],
            ),
            cue(
                20,
                false,
                vec![
                    cut(2),
                    wait(2),
                    WireCueAction::Preview { input: wire(1) },
                    run_step(1),
                ],
            ),
        ]),
    );
    let refused = submit(&mut control, &admin(), "go", go(None));
    let CommandResult::Rejected {
        message, reason, ..
    } = &refused.output.result
    else {
        panic!("expected a refusal: {:?}", refused.output.result);
    };
    assert_eq!(
        reason,
        &Some(fm_protocol::RefusalReason::ScriptFailed {
            failure: fm_protocol::ScriptFailureKind::Refused,
            line: 2,
            column: 1,
        })
    );
    assert!(
        message.starts_with("cue 10 action 2: script 1 stopped at"),
        "{message}"
    );
    assert_eq!(program(&control), input(1));
    assert_eq!(control.engine.show().desired_switcher().preview(), input(2));
    assert_eq!(cue_state(&control).live, None);

    // Deferred: the GO's cut lands, the advance's frame does not.
    let fired = submit(&mut control, &admin(), "go20", go(Some(20)));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    assert_eq!(program(&control), input(2));
    let preview = control.engine.show().desired_switcher().preview();
    let advanced = advance(&mut control, "advance");
    assert!(advanced.is_accepted(), "{:?}", advanced.output.result);
    assert_eq!(program(&control), input(2));
    assert_eq!(control.engine.show().desired_switcher().preview(), preview);
    let state = cue_state(&control);
    assert_eq!(state.pending, None);
    // The fault keeps what the step was refused with (Protocol 2.63), and
    // `cue_run_changed` carries it.
    let fault = state.fault.expect("the deferred step faulted");
    assert_eq!((fault.cue, fault.action), (20, 3));
    assert_eq!(
        fault.detail.reason,
        Some(fm_protocol::RefusalReason::ScriptFailed {
            failure: fm_protocol::ScriptFailureKind::Refused,
            line: 2,
            column: 1,
        })
    );
    assert!(
        fault.detail.message.starts_with("script 1 stopped at")
            && fault
                .detail
                .message
                .ends_with("the show has no input number 9"),
        "{}",
        fault.detail.message
    );
    let EventPayload::CueRunChanged(change) = &advanced.output.events[0].payload else {
        panic!("expected cue_run_changed: {:?}", advanced.output.events);
    };
    let published = change.state.fault.as_ref().expect("the fault is published");
    assert_eq!((published.cue, published.action), (20, 3));
    assert_eq!(published.detail, fault.detail);
}

/// A script step shares its frame's one take of Program.
#[test]
fn a_script_step_cannot_take_program_twice_on_its_frame() {
    let mut control = granted(TRANSITION_ONLY);
    submit(&mut control, &admin(), "script", set_script(1, "cut 3"));
    submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![cue(10, false, vec![cut(2), run_step(1)])]),
    );
    let refused = submit(&mut control, &admin(), "go", go(None));
    let CommandResult::Rejected { message, .. } = &refused.output.result else {
        panic!("expected a refusal: {:?}", refused.output.result);
    };
    assert!(message.contains("more than once on one frame"), "{message}");
    assert_eq!(program(&control), input(1));
}

/// A list may only run scripts the show holds, a script a cue runs cannot
/// be removed, and an edit of a script an armed chain runs needs what its
/// new verbs add.
#[test]
fn scripts_a_cue_runs_are_held_and_armed_edits_need_what_they_add() {
    let mut control = granted(EDIT_AND_TRANSITION);
    let unknown = submit(
        &mut control,
        &admin(),
        "unknown",
        set_list(vec![cue(10, false, vec![run_step(1)])]),
    );
    let CommandResult::Rejected { code, .. } = &unknown.output.result else {
        panic!("expected a refusal: {:?}", unknown.output.result);
    };
    assert_eq!(code, "not_found");

    submit(&mut control, &admin(), "script", set_script(1, "cut 3"));
    submit(&mut control, &admin(), "other", set_script(2, "cut 3"));
    let listed = submit(
        &mut control,
        &admin(),
        "list",
        set_list(vec![cue(10, false, vec![cut(2), wait(30), run_step(1)])]),
    );
    assert!(listed.is_accepted(), "{:?}", listed.output.result);
    let removal = submit(
        &mut control,
        &admin(),
        "remove",
        CommandPayload::RemoveScript { script: 1 },
    );
    let CommandResult::Rejected { code, fields, .. } = &removal.output.result else {
        panic!("expected a refusal: {:?}", removal.output.result);
    };
    assert_eq!(code, "conflict");
    assert_eq!(fields[0].code, "script_in_use");
    assert!(control.engine.show().script(1).is_some());

    // Not armed: an edit runs nothing, so it is a plain edit.
    assert_eq!(
        control
            .required_classes(&set_script(1, "cut 3\nmute 3"))
            .additional(),
        []
    );
    let fired = submit(&mut control, &admin(), "go", go(None));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    assert!(cue_state(&control).pending.is_some());

    // Armed: adding a mute needs control_audio; staying within the verbs
    // the GO authorized, or editing a script the chain does not run, is a
    // plain edit.
    assert_denied(
        &submit(
            &mut control,
            &session(),
            "add-mute",
            set_script(1, "cut 3\nmute 3"),
        ),
        "ControlAudio",
    );
    let recut = submit(&mut control, &session(), "recut", set_script(1, "cut 1"));
    assert!(recut.is_accepted(), "{:?}", recut.output.result);
    let other = submit(
        &mut control,
        &session(),
        "other-mute",
        set_script(2, "mute 3"),
    );
    assert!(other.is_accepted(), "{:?}", other.output.result);
    // An undo or redo restoring a script the armed chain runs is checked
    // the same: undoing back to the cut adds only transition, redoing the
    // mute needs control_audio.
    let redo_target = submit(
        &mut control,
        &admin(),
        "admin-mute",
        set_script(1, "mute 3"),
    );
    assert!(redo_target.is_accepted(), "{:?}", redo_target.output.result);
    let undo = CommandPayload::Undo {
        expected_edit: None,
    };
    let undone = submit(&mut control, &session(), "undo", undo);
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    let redo = CommandPayload::Redo {
        expected_edit: None,
    };
    assert_denied(
        &submit(&mut control, &session(), "redo", redo),
        "ControlAudio",
    );
}

/// A script that waits (Protocol 2.62) runs its later parts on a show
/// nobody knows yet, so `script_run` needs the class of every verb its
/// source holds, up front; the clock's list-0 advance then resumes it on
/// that authority, reading the show as it is on the due frame, and the
/// wire reports the run state throughout.
#[test]
fn a_waiting_script_needs_every_verb_up_front_and_resumes_on_the_clock() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_script(
            1,
            "cut 2\nwait 5\nif program == 3\n  mute 3\nend\ncut preview",
        ),
    );
    // Only the `cut` runs now, but the parts after the wait may mute.
    let denied = submit(&mut control, &session(), "run", run_script());
    assert_denied(&denied, "ControlAudio");
    assert_eq!(program(&control), input(1));

    let mut control = granted(TRANSITION_AND_AUDIO);
    submit(
        &mut control,
        &admin(),
        "set",
        set_script(
            1,
            "cut 2\nwait 5\nif program == 3\n  mute 3\nend\ncut preview",
        ),
    );
    let ran = submit(&mut control, &session(), "run", run_script());
    let EventPayload::ScriptRan(change) = &ran.output.events[0].payload else {
        panic!("expected script_ran: {:?}", ran.output.events);
    };
    let fm_protocol::ScriptRunStatus::Waiting {
        started_frame,
        due_frame,
        waits: 1,
        wait_frames: 5,
    } = change.run
    else {
        panic!("expected a waiting run: {:?}", change.run);
    };
    assert_eq!(due_frame, started_frame + 5);
    assert_eq!(control.cue_advance_frame(), Some((0, due_frame)));
    assert_eq!(control.script_states()[0].run, change.run);
    // Another run waits for this one.
    let busy = submit(&mut control, &session(), "again", run_script());
    let CommandResult::Rejected { reason, .. } = &busy.output.result else {
        panic!("expected a refusal: {:?}", busy.output.result);
    };
    assert_eq!(
        reason,
        &Some(fm_protocol::RefusalReason::ScriptRunning { script: 1 })
    );

    // The operator takes input 3 while it waits; the rest runs on the
    // clock and sees it.
    submit(
        &mut control,
        &admin(),
        "take-3",
        CommandPayload::SelectPreview { input: wire(3) },
    );
    let cut = submit(&mut control, &session(), "cut-3", CommandPayload::Cut);
    assert!(cut.is_accepted(), "{:?}", cut.output.result);
    assert_eq!(program(&control), input(3));
    let advanced = advance(&mut control, "advance");
    let EventPayload::ScriptRan(change) = &advanced.output.events[0].payload else {
        panic!("expected script_ran: {:?}", advanced.output.events);
    };
    assert_eq!((change.script, change.actions), (1, 3));
    assert_eq!(change.run, fm_protocol::ScriptRunStatus::Idle);
    assert!(strip(&control, 3).muted);
    assert_eq!(program(&control), input(2));
    assert_eq!(control.cue_advance_frame(), None);
}

/// `script_stop` ends a waiting run with `script_run_changed`; a part that
/// fails on its frame is published as a typed fault.
#[test]
fn a_stop_and_a_fault_publish_the_run_state() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_script(1, "wait 3\ncut 7"),
    );
    submit(&mut control, &session(), "run", run_script());
    let stopped = submit(&mut control, &session(), "stop", CommandPayload::StopScript);
    assert!(
        matches!(
            stopped.output.events[0].payload,
            EventPayload::ScriptRunChanged {
                script: 1,
                run: fm_protocol::ScriptRunStatus::Idle,
                ..
            }
        ),
        "{:?}",
        stopped.output.events
    );
    assert_eq!(control.cue_advance_frame(), None);

    submit(&mut control, &session(), "run-again", run_script());
    let advanced = advance(&mut control, "advance");
    assert!(advanced.is_accepted(), "{:?}", advanced.output.result);
    let EventPayload::ScriptRunChanged {
        script: 1,
        frame,
        run,
    } = &advanced.output.events[0].payload
    else {
        panic!("expected script_run_changed: {:?}", advanced.output.events);
    };
    // The fault says what the refused action was refused with (Protocol
    // 2.63): this refusal has no typed reason, only its message.
    assert_eq!(
        run,
        &fm_protocol::ScriptRunStatus::Faulted {
            frame: *frame,
            failure: fm_protocol::ScriptFailureKind::Refused,
            line: 2,
            column: 1,
            detail: fm_command::FaultDetail {
                reason: None,
                message: "the show has no input number 7".to_owned(),
            },
        }
    );
    assert_eq!(&control.script_states()[0].run, run);
    assert_eq!(program(&control), input(1));
}
