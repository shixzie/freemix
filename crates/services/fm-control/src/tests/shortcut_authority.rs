//! RC-004 authority: editing a shortcut is a project edit; firing one
//! needs Transition plus the class of every action it performs, or what
//! the cue GO or script run it stands for needs, checked up front so a
//! shortcut never does what the firing session could not do directly.

use fm_auth::{AuthorizationDenial, DenialReason};
use fm_protocol::{
    CueListSpec, ScriptSpec, ShortcutSpec, TransitionFavoriteEffect, WireCue, WireCueAction,
    WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
};

use super::*;

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
const AUDIO_ONLY: &[CommandClass] = &[CommandClass::ControlAudio];

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

fn spec(shortcut: u64, action: WireShortcutAction) -> ShortcutSpec {
    ShortcutSpec {
        name: format!("Shortcut {shortcut}"),
        enabled: true,
        key: Some(WireShortcutKey {
            key: format!("F{shortcut}"),
            command: false,
            shift: false,
            alt: false,
        }),
        osc: Some(format!("s{shortcut}")),
        action,
    }
}

fn set_shortcut(shortcut: u64, action: WireShortcutAction) -> CommandPayload {
    CommandPayload::SetShortcut {
        shortcut,
        spec: Box::new(spec(shortcut, action)),
    }
}

fn fire(shortcut: u64) -> CommandPayload {
    CommandPayload::FireShortcut { shortcut }
}

fn muted(control: &ControlService<Grants>, value: u128) -> bool {
    control
        .engine
        .show()
        .input_audio_strip(input(value))
        .unwrap()
        .muted
}

fn program(control: &ControlService<Grants>) -> InputId {
    control.engine.show().desired_switcher().program()
}

#[test]
fn editing_a_shortcut_is_a_project_edit() {
    let mut control = granted(TRANSITION_AND_AUDIO);
    let refused = submit(
        &mut control,
        &session(),
        "set",
        set_shortcut(1, WireShortcutAction::Actions(vec![cut(2)])),
    );
    assert_denied(&refused, "EditProject");
    let refused = submit(
        &mut control,
        &session(),
        "remove",
        CommandPayload::RemoveShortcut { shortcut: 1 },
    );
    assert_denied(&refused, "EditProject");
    assert!(control.engine.show().shortcuts().is_empty());

    let set = submit(
        &mut control,
        &admin(),
        "admin-set",
        set_shortcut(1, WireShortcutAction::Actions(vec![cut(2)])),
    );
    assert!(set.is_accepted(), "{:?}", set.output.result);
    assert!(matches!(
        &set.output.events[0].payload,
        EventPayload::ShortcutChanged(status) if status.id == 1
    ));
    assert_eq!(control.shortcut_states().len(), 1);
}

#[test]
fn a_fire_needs_the_class_of_every_action() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "set",
        set_shortcut(1, WireShortcutAction::Actions(vec![cut(2), mute(3)])),
    );
    let refused = submit(&mut control, &session(), "fire", fire(1));
    assert_denied(&refused, "ControlAudio");
    // Nothing ran: not even the cut before the mute.
    assert!(!muted(&control, 3));
    assert_eq!(program(&control), input(1));

    // Audio alone is not enough: a fire is a transition-class command.
    let mut audio = granted(AUDIO_ONLY);
    submit(
        &mut audio,
        &admin(),
        "set",
        set_shortcut(1, WireShortcutAction::Actions(vec![mute(3)])),
    );
    assert_denied(
        &submit(&mut audio, &session(), "fire", fire(1)),
        "Transition",
    );
    assert!(!muted(&audio, 3));

    let mut full = granted(TRANSITION_AND_AUDIO);
    submit(
        &mut full,
        &admin(),
        "set",
        set_shortcut(1, WireShortcutAction::Actions(vec![cut(2), mute(3)])),
    );
    let fired = submit(&mut full, &session(), "fire", fire(1));
    assert!(fired.is_accepted(), "{:?}", fired.output.result);
    let EventPayload::ShortcutFired(change) = &fired.output.events[0].payload else {
        panic!("expected shortcut_fired: {:?}", fired.output.events);
    };
    assert_eq!((change.shortcut, change.actions), (1, 2));
    assert!(muted(&full, 3));
    assert_eq!(program(&full), input(2));
}

#[test]
fn a_cue_or_script_fire_needs_what_its_command_needs() {
    let mut control = granted(TRANSITION_ONLY);
    submit(
        &mut control,
        &admin(),
        "list",
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues: vec![WireCue {
                    id: 10,
                    name: "Open".into(),
                    auto_follow: false,
                    actions: vec![cut(2), mute(3)],
                }],
            }),
        },
    );
    submit(
        &mut control,
        &admin(),
        "script",
        CommandPayload::SetScript {
            script: 4,
            spec: Box::new(ScriptSpec {
                name: "Mute".into(),
                source: "mute 3".into(),
            }),
        },
    );
    submit(
        &mut control,
        &admin(),
        "go",
        set_shortcut(
            1,
            WireShortcutAction::Cue {
                list: 7,
                operation: WireShortcutCueOperation::Go { cue: None },
            },
        ),
    );
    submit(
        &mut control,
        &admin(),
        "run",
        set_shortcut(2, WireShortcutAction::Script { script: 4 }),
    );
    submit(
        &mut control,
        &admin(),
        "stop",
        set_shortcut(
            3,
            WireShortcutAction::Cue {
                list: 7,
                operation: WireShortcutCueOperation::Stop,
            },
        ),
    );
    assert_denied(
        &submit(&mut control, &session(), "fire-go", fire(1)),
        "ControlAudio",
    );
    assert_denied(
        &submit(&mut control, &session(), "fire-run", fire(2)),
        "ControlAudio",
    );
    assert!(!muted(&control, 3));
    assert_eq!(program(&control), input(1));
    // Stopping a list performs no action.
    let stopped = submit(&mut control, &session(), "fire-stop", fire(3));
    assert!(stopped.is_accepted(), "{:?}", stopped.output.result);
}

#[test]
fn an_unknown_or_disabled_fire_is_refused_with_its_reason() {
    let mut control = granted(TRANSITION_ONLY);
    let unknown = submit(&mut control, &admin(), "unknown", fire(9));
    let CommandResult::Rejected { code, reason, .. } = &unknown.output.result else {
        panic!("expected a refusal: {:?}", unknown.output.result);
    };
    assert_eq!(code, "not_found");
    assert!(
        matches!(
            reason,
            Some(fm_protocol::RefusalReason::UnknownId {
                kind: fm_protocol::IdKind::Shortcut,
                id: 9
            })
        ),
        "{reason:?}"
    );

    let mut disabled = spec(1, WireShortcutAction::Actions(vec![cut(2)]));
    disabled.enabled = false;
    submit(
        &mut control,
        &admin(),
        "set",
        CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(disabled),
        },
    );
    let refused = submit(&mut control, &admin(), "disabled", fire(1));
    let CommandResult::Rejected { code, message, .. } = &refused.output.result else {
        panic!("expected a refusal: {:?}", refused.output.result);
    };
    assert_eq!(code, "conflict");
    assert!(message.contains("disabled"), "{message}");
    assert_eq!(program(&control), input(1));
}
