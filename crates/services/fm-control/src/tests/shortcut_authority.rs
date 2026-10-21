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
        input: Some(fm_types::InputTarget::Fixed(wire(value))),
        effect: TransitionFavoriteEffect::Cut,
        duration_frames: 0,
    }
}

fn mute(value: u128) -> WireCueAction {
    WireCueAction::AudioMute {
        input: fm_types::InputTarget::Fixed(wire(value)),
        mute: fm_protocol::CueMute::Set(true),
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
            then: None,
            scope: None,
        }),
        osc: Some(format!("s{shortcut}")),
        action,
    }
}

fn set_shortcut(shortcut: u64, action: WireShortcutAction) -> CommandPayload {
    CommandPayload::SetShortcut {
        shortcut,
        spec: Box::new(spec(shortcut, action)),
        position: None,
    }
}

fn fire(shortcut: u64) -> CommandPayload {
    CommandPayload::FireShortcut {
        shortcut,
        targets: fm_protocol::WireTargetBindings::default(),
        value: None,
    }
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
    assert_eq!(control.engine.show().shortcuts(), []);

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
            position: None,
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
            position: None,
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
            position: None,
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

/// RC-004: the authority binds a dynamic fire's Preview and Program into
/// the journaled command, beside the console's selection, so a replay on a
/// restored authority acts on the same inputs; a fire with fixed targets
/// only is journaled as sent.
#[test]
fn a_dynamic_fire_is_journaled_with_the_inputs_it_bound() {
    use fm_protocol::{WireInputTarget, WireTargetBindings};
    let dynamic = WireShortcutAction::Actions(vec![
        WireCueAction::AudioMute {
            input: WireInputTarget::Preview,
            mute: fm_protocol::CueMute::Set(true),
        },
        WireCueAction::Take {
            input: Some(WireInputTarget::Selected),
            effect: TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        },
    ]);
    let setup = |control: &mut ControlService<Grants>| {
        submit(control, &admin(), "set-1", set_shortcut(1, dynamic.clone()));
        let fixed = WireShortcutAction::Actions(vec![mute(3)]);
        submit(control, &admin(), "set-2", set_shortcut(2, fixed));
    };
    let mut control = granted(&[]);
    setup(&mut control);

    let sent = WireTargetBindings {
        selected: Some(wire(3)),
        ..WireTargetBindings::default()
    };
    let message = command(
        "fire-1",
        "fire-1",
        CommandPayload::FireShortcut {
            shortcut: 1,
            targets: sent,
            value: None,
        },
    );
    let staged = prepared(
        control
            .prepare_submit(&admin(), message.clone(), 0)
            .unwrap(),
    );
    let journaled = staged.replay_command(&message);
    let bound = WireTargetBindings {
        selected: Some(wire(3)),
        preview: Some(wire(2)),
        program: Some(wire(1)),
    };
    assert_eq!(
        journaled.payload,
        CommandPayload::FireShortcut {
            shortcut: 1,
            targets: bound,
            value: None,
        }
    );
    assert!(staged.commit().unwrap().is_accepted());
    control.tick(&server_identity()).unwrap();
    assert!(muted(&control, 2));
    assert_eq!(program(&control), input(3));

    // A fixed fire is journaled exactly as sent.
    let fixed = command("fire-2", "fire-2", fire(2));
    let staged = prepared(control.prepare_submit(&admin(), fixed.clone(), 0).unwrap());
    assert_eq!(staged.replay_command(&fixed).payload, fixed.payload);
    drop(staged);

    // Replayed on an authority whose Preview differs, the journaled fire
    // still mutes 2, the input it bound.
    let mut replayed = granted(&[]);
    setup(&mut replayed);
    let to_preview = WireShortcutAction::Actions(vec![WireCueAction::Preview {
        input: WireInputTarget::Fixed(wire(3)),
    }]);
    submit(
        &mut replayed,
        &admin(),
        "set-3",
        set_shortcut(3, to_preview),
    );
    submit(&mut replayed, &admin(), "fire-3", fire(3));
    assert_eq!(
        replayed.engine.show().desired_switcher().preview(),
        input(3)
    );
    let line = fm_protocol::encode_line(&WireMessage::Command(journaled)).unwrap();
    let WireMessage::Command(decoded) = fm_protocol::decode_line(&line).unwrap() else {
        panic!("a journal line is a command");
    };
    let submission = submit(&mut replayed, &admin(), "replay-1", decoded.payload);
    assert!(submission.is_accepted(), "{:?}", submission.output);
    assert!(muted(&replayed, 2));
    assert!(!muted(&replayed, 3));
    assert_eq!(program(&replayed), input(3));
}

/// RC-004 dynamic values: a gain taken from the firing control needs the
/// audio class like a fixed gain, runs with the value the fire carries
/// (journaled as sent, so a replay sets the same gain), and is refused
/// whole on field `value` without one.
#[test]
fn a_fire_carries_the_value_of_its_control() {
    use fm_protocol::{CueGain, CueMute, FireValue, WireInputTarget, WireTargetBindings};
    let fader = WireShortcutAction::Actions(vec![
        WireCueAction::AudioGain {
            input: WireInputTarget::Fixed(wire(3)),
            gain: CueGain::Fired {
                min: -40_000,
                max: 0,
            },
        },
        WireCueAction::AudioMute {
            input: WireInputTarget::Fixed(wire(3)),
            mute: CueMute::Toggle,
        },
    ]);
    let mut control = granted(TRANSITION_ONLY);
    submit(&mut control, &admin(), "set", set_shortcut(1, fader));
    let at = |value| CommandPayload::FireShortcut {
        shortcut: 1,
        targets: WireTargetBindings::default(),
        value: FireValue::new(value),
    };
    assert_denied(
        &submit(&mut control, &session(), "denied", at(250)),
        "ControlAudio",
    );

    let mut control = granted(TRANSITION_AND_AUDIO);
    submit(
        &mut control,
        &admin(),
        "set",
        set_shortcut(
            1,
            WireShortcutAction::Actions(vec![WireCueAction::AudioGain {
                input: WireInputTarget::Fixed(wire(3)),
                gain: CueGain::Fired {
                    min: -40_000,
                    max: 0,
                },
            }]),
        ),
    );
    let refused = submit(&mut control, &session(), "bare", fire(1));
    let CommandResult::Rejected { code, fields, .. } = &refused.output.result else {
        panic!(
            "a fire without a value is refused: {:?}",
            refused.output.result
        );
    };
    assert_eq!(code, "invalid_command");
    assert_eq!(fields[0].field, "value");
    assert_eq!(fields[0].code, "unbound");

    let message = command("fire", "fire", at(250));
    let staged = prepared(
        control
            .prepare_submit(&session(), message.clone(), 0)
            .unwrap(),
    );
    // The value is the client's: it is journaled exactly as sent.
    assert_eq!(staged.replay_command(&message).payload, at(250));
    assert!(staged.commit().unwrap().is_accepted());
    control.tick(&server_identity()).unwrap();
    let gain = |control: &ControlService<Grants>| {
        control
            .engine
            .show()
            .input_audio_strip(input(3))
            .unwrap()
            .gain_millidb
    };
    assert_eq!(gain(&control), -30_000);
    let line = fm_protocol::encode_line(&WireMessage::Command(message)).unwrap();
    assert!(line.contains("\tvalue=250"), "{line}");
    let accepted = submit(&mut control, &session(), "top", at(1_000));
    assert!(accepted.is_accepted(), "{:?}", accepted.output.result);
    assert_eq!(gain(&control), 0);
}
