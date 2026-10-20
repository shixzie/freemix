//! The loudness reset (Protocol 2.92, AU-005) is journaled as its own wire
//! command: the journal line decodes to the exact command, and replaying it
//! on a restored authority is accepted at the same revision with the same
//! announcement and leaves the same show.

use fm_protocol::{AudioDspTarget, AudioMixerOperation, EventMessage, EventPayload};

use super::*;

fn reset(target: AudioDspTarget) -> CommandPayload {
    CommandPayload::AudioMixer(AudioMixerOperation::ResetLoudness { target })
}

#[test]
fn a_journaled_loudness_reset_replays_to_the_same_revision_and_show() {
    let targets = [
        AudioDspTarget::Master,
        AudioDspTarget::Input(WireInputId::from_domain(input(2))),
    ];
    let mut control = service(64, 64);
    let mut journal = Vec::new();
    for (index, target) in targets.into_iter().enumerate() {
        let id = format!("reset-{index}");
        let message = command(&id, &id, reset(target));
        let staged = prepared(
            control
                .prepare_submit(&principal(Role::Operator), message.clone(), 0)
                .unwrap(),
        );
        let journaled = staged.replay_command(&message);
        assert_eq!(journaled.payload, message.payload);
        assert!(staged.commit().unwrap().is_accepted());
        control.tick(&server_identity()).unwrap();
        journal.push(fm_protocol::encode_line(&WireMessage::Command(journaled)).unwrap());
    }
    assert_eq!(control.engine.revision().get(), 2);
    assert_eq!(
        journal[0],
        format!(
            "command\tprotocol={}\tid=reset-0\tidempotency_key=reset-0\t\
             payload=audio_loudness_reset\ttarget=master\n",
            fm_protocol::CURRENT_PROTOCOL_VERSION
        )
    );

    // Replaying the decoded journal on a restored authority accepts both
    // resets at the same revisions and announces them alike.
    let mut replayed = service(64, 64);
    let subscriber = replayed.subscribe().unwrap();
    for line in &journal {
        let WireMessage::Command(decoded) = fm_protocol::decode_line(line).unwrap() else {
            panic!("a journal line is a command");
        };
        let submission = replayed
            .submit(&principal(Role::Operator), decoded, 0)
            .unwrap();
        assert!(submission.is_accepted(), "{:?}", submission.output);
        replayed.tick(&server_identity()).unwrap();
    }
    assert_eq!(replayed.engine.revision(), control.engine.revision());
    assert_eq!(replayed.engine.show(), control.engine.show());
    let announced: Vec<AudioDspTarget> = std::iter::from_fn(|| subscriber.try_recv().ok())
        .filter_map(|event| match event {
            LiveEvent::Durable(EventMessage {
                payload:
                    EventPayload::AudioMixerChanged(AudioMixerOperation::ResetLoudness { target }),
                ..
            }) => Some(target),
            _ => None,
        })
        .collect();
    assert_eq!(announced, targets);
}

#[test]
fn a_viewer_cannot_reset_loudness_and_an_unknown_input_is_refused() {
    let mut control = service(64, 64);
    let submission = control
        .submit(
            &principal(Role::Viewer),
            command("viewer", "viewer", reset(AudioDspTarget::Master)),
            0,
        )
        .unwrap();
    assert!(!submission.is_accepted());
    let submission = control
        .submit(
            &principal(Role::Operator),
            command(
                "unknown",
                "unknown",
                reset(AudioDspTarget::Input(WireInputId::from_domain(input(9)))),
            ),
            0,
        )
        .unwrap();
    assert!(!submission.is_accepted());
    assert_eq!(control.engine.revision().get(), 0);
}
