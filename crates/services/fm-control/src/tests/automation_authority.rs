//! RC-011/RC-017 authority: a script run or a cue GO needs the class of
//! every action it performs, checked up front against the session that
//! starts it, so automation never does what the session could not do
//! directly. A GO is checked for the whole chain it arms (its waits and
//! auto-follows run later on the clock); an edit of a list whose cue is
//! waiting needs what it adds to the armed chain.

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
