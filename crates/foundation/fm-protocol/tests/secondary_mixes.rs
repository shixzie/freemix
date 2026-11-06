//! IN-023: secondary-mix commands and configuration round-trip at Protocol
//! 2.158, with live cuts and transitions at 2.159.

use core::num::NonZeroU128;

use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, WireInputId,
    WireMessage, WireMixNumber, WireSecondaryMixConfig, WireSecondaryMixMode,
    WireSecondaryMixSource, decode_line, encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn number(value: u8) -> WireMixNumber {
    WireMixNumber::new(value).unwrap()
}

fn command(payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "01K:test".to_owned(),
        idempotency_key: "operator-7:01K".to_owned(),
        expected_revision: Some(1_842),
        deadline_ms: Some(500),
        payload,
    }
}

fn round_trip(payload: CommandPayload) {
    let message = WireMessage::Command(command(payload));
    let line = encode_line(&message).unwrap();
    assert_eq!(decode_line(&line).unwrap(), message);
}

fn config(
    mode: WireSecondaryMixMode,
    preview: Option<WireSecondaryMixSource>,
) -> WireSecondaryMixConfig {
    WireSecondaryMixConfig {
        number: number(2),
        name: "Clean feed".into(),
        mode,
        program: WireSecondaryMixSource::Input(input(10)),
        preview,
    }
}

#[test]
fn secondary_mix_commands_round_trip() {
    round_trip(CommandPayload::SetSecondaryMix {
        number: number(2),
        config: Box::new(config(
            WireSecondaryMixMode::PreviewProgram,
            Some(WireSecondaryMixSource::Mix(number(1))),
        )),
    });
    round_trip(CommandPayload::SetSecondaryMix {
        number: number(2),
        config: Box::new(config(WireSecondaryMixMode::OutputOnly, None)),
    });
    round_trip(CommandPayload::ClearSecondaryMix { number: number(2) });
}

#[test]
fn live_secondary_mix_commands_round_trip() {
    round_trip(CommandPayload::SecondaryMixCut { number: number(2) });
    round_trip(CommandPayload::SecondaryMixTransition {
        number: number(2),
        kind: AutoTransitionKind::Fade,
        duration_frames: 30,
    });
    round_trip(CommandPayload::SecondaryMixFadeToBlack {
        number: number(2),
        active: true,
        duration_frames: 30,
    });
    round_trip(CommandPayload::SecondaryMixFadeToBlack {
        number: number(2),
        active: false,
        duration_frames: 15,
    });
    round_trip(CommandPayload::SecondaryMixStartTBar {
        number: number(2),
        kind: AutoTransitionKind::Slide,
    });
    round_trip(CommandPayload::SecondaryMixSetTBarPosition {
        number: number(2),
        position: fm_protocol::ManualTransitionPosition::new(5_000).unwrap(),
    });
    round_trip(CommandPayload::SecondaryMixCommitTBar { number: number(2) });
    round_trip(CommandPayload::SecondaryMixCancelTBar { number: number(2) });
}

#[test]
fn an_output_only_mix_with_preview_is_refused() {
    let message = WireMessage::Command(command(CommandPayload::SetSecondaryMix {
        number: number(2),
        config: Box::new(config(
            WireSecondaryMixMode::OutputOnly,
            Some(WireSecondaryMixSource::Input(input(11))),
        )),
    }));
    assert!(encode_line(&message).is_err());
}

#[test]
fn a_preview_program_mix_without_preview_is_refused() {
    let message = WireMessage::Command(command(CommandPayload::SetSecondaryMix {
        number: number(2),
        config: Box::new(config(WireSecondaryMixMode::PreviewProgram, None)),
    }));
    assert!(encode_line(&message).is_err());
}
