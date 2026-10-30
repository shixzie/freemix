//! AU-006: a strip or the Master bus carries a live, ordered plugin chain
//! that the media plane renders; unsupported plugins and unknown strips are
//! refused, and clearing a chain removes it.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    Engine, EngineAudioMixerEdit, EngineAudioMixerUpdate, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, ShowState,
};
use fm_plugin_host_api::{
    PluginChain, PluginId, PluginSlot, ProtocolLimits, REFERENCE_POLARITY, REFERENCE_TRIM,
    StateSnapshot, StateVersion, state_bytes,
};
use fm_types::strip_dsp::StripDspTarget;
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "plugins",
        (1..=2)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn trim(milli_db: i32) -> PluginSlot {
    let mut slot = PluginSlot::new(PluginId::new(REFERENCE_TRIM));
    let snapshot = StateSnapshot::new(
        slot.plugin().clone(),
        StateVersion::new(1),
        state_bytes(milli_db),
        &ProtocolLimits::default(),
    )
    .unwrap();
    slot.store_snapshot(snapshot).unwrap();
    slot
}

fn chain() -> PluginChain {
    let mut chain = PluginChain::new();
    chain.push(trim(-12_000)).unwrap();
    chain
        .push(PluginSlot::new(PluginId::new(REFERENCE_POLARITY)))
        .unwrap();
    chain
}

fn command(target: StripDspTarget, chain: PluginChain) -> EngineCommand {
    EngineCommand::AudioMixer(EngineAudioMixerEdit::SetPluginChain {
        target,
        chain: Box::new(chain),
    })
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = execute(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
    outcome
}

#[test]
fn a_plugin_chain_is_live_and_reaches_the_frame() {
    let mut engine = engine();
    let input_chain = chain();
    accept(
        &mut engine,
        "input",
        command(StripDspTarget::Input(input(1)), input_chain.clone()),
    );
    assert_eq!(
        engine.show().input_audio_plugins(input(1)),
        &input_chain,
        "the strip holds the chain"
    );
    // A live chain is not an undoable project edit.
    assert_eq!(engine.undo_entries().len(), 0);
    assert_eq!(
        EngineCommand::AudioMixer(EngineAudioMixerEdit::SetPluginChain {
            target: StripDspTarget::Input(input(1)),
            chain: Box::new(input_chain.clone()),
        })
        .kind(),
        EngineCommandKind::LiveOperation
    );
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::PluginChain {
            target: StripDspTarget::Input(input(1)),
            chain: Box::new(input_chain),
        }]
    );

    // The Master bus takes a chain too; it renders after the summed strips.
    let master = chain();
    accept(
        &mut engine,
        "master",
        command(StripDspTarget::Master, master.clone()),
    );
    assert_eq!(engine.show().master_audio_plugins(), &master);
}

#[test]
fn closing_a_chain_or_naming_an_unknown_strip_is_refused() {
    let mut engine = engine();
    let mut unsupported = PluginChain::new();
    unsupported
        .push(PluginSlot::new(PluginId::new("vendor.vst3")))
        .unwrap();
    let outcome = execute(
        &mut engine,
        "vendor",
        command(StripDspTarget::Input(input(1)), unsupported),
    );
    let rejection = outcome
        .receipt
        .rejected()
        .expect("refused")
        .rejection
        .clone();
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(rejection.fields[0].field, "plugins");
    assert_eq!(rejection.fields[0].code, "unsupported_plugin");

    let mut invalid = PluginChain::new();
    invalid.push(trim(24_001)).unwrap();
    let outcome = execute(
        &mut engine,
        "state",
        command(StripDspTarget::Input(input(1)), invalid),
    );
    let rejection = outcome
        .receipt
        .rejected()
        .expect("refused")
        .rejection
        .clone();
    assert_eq!(rejection.fields[0].field, "plugins");
    assert_eq!(rejection.fields[0].code, "invalid_state");

    let outcome = execute(
        &mut engine,
        "unknown",
        command(StripDspTarget::Input(input(9)), chain()),
    );
    let rejection = outcome
        .receipt
        .rejected()
        .expect("refused")
        .rejection
        .clone();
    assert_eq!(rejection.fields[0].code, "unknown_input");

    // Clearing a strip's chain removes it, so the default empty chain shows.
    accept(
        &mut engine,
        "set",
        command(StripDspTarget::Input(input(2)), chain()),
    );
    accept(
        &mut engine,
        "clear",
        command(StripDspTarget::Input(input(2)), PluginChain::new()),
    );
    assert!(engine.show().input_audio_plugins(input(2)).is_empty());
    assert!(
        engine
            .show()
            .input_audio_plugin_chains()
            .get(&input(2))
            .is_none()
    );
}
