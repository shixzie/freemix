//! Mixer labels, strip groups, the solo mode, the monitor output, and the
//! Fade-to-Black output selection between the engine and the wire
//! (Protocols 2.40, 2.44, and 2.53, AU-001 and SW-004), and the strip and
//! Master DSP chains (Protocol 2.75, AU-005) and the loudness reset
//! (Protocol 2.92).

use fm_engine::{
    Engine, EngineAudioGroupState, EngineAudioMixerEdit, EngineCommand, EngineEvent, ShowState,
};
use fm_model::{AudioGroupNumber, AudioSoloMode};
use fm_plugin_host_api::{
    PluginChain, PluginId, PluginSlot, ProtocolLimits, StateSnapshot, StateVersion,
};
use fm_protocol::AuxBusStatus;
use fm_protocol::{
    AudioDspTarget, AudioGroupStatus, AudioMixerOperation, AudioMixerStatus, AudioMonitorStatus,
    AudioPluginStatusMessage, AudioPluginStatusRequest, AudioStripDspStateMessage,
    AudioStripSendStateMessage, CommandPayload, EventPayload, InputAudioLabel, PluginChainSlot,
    ServerIdentity, WireInputId, WireOutputId,
};
use fm_types::strip_dsp::StripDspTarget;

use crate::grade::{domain_gesture, wire_gesture};

/// The engine DSP target for a wire one.
#[must_use]
pub(super) const fn dsp_target(target: AudioDspTarget) -> StripDspTarget {
    match target {
        AudioDspTarget::Master => StripDspTarget::Master,
        AudioDspTarget::Input(input) => StripDspTarget::Input(input.to_domain()),
    }
}

/// The wire DSP target for an engine one.
#[must_use]
pub(super) const fn wire_dsp_target(target: StripDspTarget) -> AudioDspTarget {
    match target {
        StripDspTarget::Master => AudioDspTarget::Master,
        StripDspTarget::Input(input) => AudioDspTarget::Input(WireInputId::from_domain(input)),
    }
}

/// One `audio_strip_dsp_state` per input whose chain is not the default,
/// ascending, at the engine's revision (Protocol 2.75).
pub(super) fn strip_dsp_states(engine: &Engine) -> Vec<AudioStripDspStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .input_audio_dsps()
        .iter()
        .map(|(input, chain)| AudioStripDspStateMessage {
            revision,
            input: WireInputId::from_domain(*input),
            chain: *chain,
        })
        .collect()
}

/// How many strips carry sends beyond the plain enable mask (2.130).
fn strip_send_states_len(show: &ShowState) -> usize {
    show.input_audio_strips()
        .values()
        .filter(|state| {
            state.sends != fm_types::audio_bus::AudioSends::from_mask(state.sends.mask())
        })
        .count()
}

/// One `audio_strip_send_state` per input whose sends are not the plain
/// enable mask, ascending (2.130, AU-002).
pub(super) fn strip_send_states(engine: &Engine) -> Vec<AudioStripSendStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .input_audio_strips()
        .iter()
        .filter(|(_, state)| {
            state.sends != fm_types::audio_bus::AudioSends::from_mask(state.sends.mask())
        })
        .map(|(&input, state)| AudioStripSendStateMessage {
            revision,
            input: WireInputId::from_domain(input),
            sends: state.sends,
        })
        .collect()
}

/// One target's chain and the installed plugins, answering a status request
/// (2.129, AU-006).
pub(super) fn plugin_status(
    engine: &Engine,
    authority: &ServerIdentity,
    request: &AudioPluginStatusRequest,
) -> AudioPluginStatusMessage {
    let target = dsp_target(request.target);
    let chain = match target {
        StripDspTarget::Master => engine.show().master_audio_plugins(),
        StripDspTarget::Input(input) => engine.show().input_audio_plugins(input),
    };
    AudioPluginStatusMessage {
        server: authority.clone(),
        request_id: request.request_id.clone(),
        target: request.target,
        available: fm_plugin_host_api::REFERENCE_PLUGINS
            .iter()
            .map(|plugin| (*plugin).to_owned())
            .collect(),
        slots: wire_plugin_chain(chain),
    }
}

/// The engine chain for wire slots the codec already bounded.
fn engine_plugin_chain(slots: &[PluginChainSlot]) -> PluginChain {
    let limits = ProtocolLimits::default();
    let mut chain = PluginChain::new();
    for slot in slots {
        let plugin = PluginId::new(slot.plugin.clone());
        let mut engine_slot = PluginSlot::new(plugin.clone());
        engine_slot.set_bypassed(slot.bypassed);
        engine_slot
            .set_latency(slot.latency_frames)
            .expect("the codec bounds a slot's latency");
        if !slot.state.is_empty() {
            let snapshot =
                StateSnapshot::new(plugin, StateVersion::new(0), slot.state.clone(), &limits)
                    .expect("the codec bounds a slot's state");
            engine_slot
                .store_snapshot(snapshot)
                .expect("the snapshot names the slot's plugin");
        }
        chain
            .push(engine_slot)
            .expect("the codec bounds a chain's slots and identifiers");
    }
    chain
}

/// The wire slots of one engine chain.
fn wire_plugin_chain(chain: &PluginChain) -> Vec<PluginChainSlot> {
    chain
        .slots()
        .iter()
        .map(|slot| PluginChainSlot {
            plugin: slot.plugin().as_str().to_owned(),
            bypassed: slot.is_bypassed(),
            latency_frames: slot.latency_frames(),
            state: slot
                .snapshot()
                .map_or_else(Vec::new, |snapshot| snapshot.data().to_vec()),
        })
        .collect()
}

fn group_number(group: u8) -> AudioGroupNumber {
    AudioGroupNumber::new(group).expect("the codec bounds wire groups")
}

/// The model solo mode for a wire solo mode.
#[must_use]
pub(super) const fn solo_mode(mode: fm_protocol::AudioSoloMode) -> AudioSoloMode {
    match mode {
        fm_protocol::AudioSoloMode::InPlace => AudioSoloMode::InPlace,
        fm_protocol::AudioSoloMode::Pfl => AudioSoloMode::Pfl,
        fm_protocol::AudioSoloMode::Afl => AudioSoloMode::Afl,
    }
}

/// The wire solo mode for a model solo mode.
#[must_use]
pub(super) const fn wire_solo_mode(mode: AudioSoloMode) -> fm_protocol::AudioSoloMode {
    match mode {
        AudioSoloMode::InPlace => fm_protocol::AudioSoloMode::InPlace,
        AudioSoloMode::Pfl => fm_protocol::AudioSoloMode::Pfl,
        AudioSoloMode::Afl => fm_protocol::AudioSoloMode::Afl,
    }
}

/// The engine command for a wire mixer operation.
pub(super) fn engine_command(operation: &AudioMixerOperation) -> EngineCommand {
    EngineCommand::AudioMixer(match operation {
        AudioMixerOperation::SetLabel { input, label } => EngineAudioMixerEdit::SetLabel {
            input: input.to_domain(),
            label: label.clone(),
        },
        AudioMixerOperation::SetGroup { input, group } => EngineAudioMixerEdit::SetGroup {
            input: input.to_domain(),
            group: group.map(group_number),
        },
        AudioMixerOperation::RenameGroup { group, name } => EngineAudioMixerEdit::RenameGroup {
            group: group_number(*group),
            name: name.clone(),
        },
        AudioMixerOperation::SetGroupLevel {
            group,
            gain_millidb,
            muted,
        } => EngineAudioMixerEdit::SetGroupLevel {
            group: group_number(*group),
            state: EngineAudioGroupState {
                gain_millidb: *gain_millidb,
                muted: *muted,
            },
        },
        AudioMixerOperation::SetGroupSolo { group, soloed } => EngineAudioMixerEdit::SetGroupSolo {
            group: group_number(*group),
            soloed: *soloed,
        },
        AudioMixerOperation::SetGroupParent { group, parent } => {
            EngineAudioMixerEdit::SetGroupParent {
                group: group_number(*group),
                parent: parent.map(group_number),
            }
        }
        AudioMixerOperation::SetSoloMode { mode } => EngineAudioMixerEdit::SetSoloMode {
            mode: solo_mode(*mode),
        },
        AudioMixerOperation::SetMonitorDevice { device } => {
            EngineAudioMixerEdit::SetMonitorDevice {
                device: device.clone(),
            }
        }
        AudioMixerOperation::SetMonitorLevel {
            gain_millidb,
            muted,
        } => EngineAudioMixerEdit::SetMonitorLevel {
            state: EngineAudioGroupState {
                gain_millidb: *gain_millidb,
                muted: *muted,
            },
        },
        AudioMixerOperation::SetStripDsp {
            target,
            chain,
            gesture,
        } => EngineAudioMixerEdit::SetStripDsp {
            target: dsp_target(*target),
            chain: chain.clone(),
            gesture: gesture.map(domain_gesture),
        },
        AudioMixerOperation::ResetLoudness { target } => EngineAudioMixerEdit::ResetLoudness {
            target: dsp_target(*target),
        },
        AudioMixerOperation::RenameAuxBus { bus, name } => EngineAudioMixerEdit::RenameAuxBus {
            bus: *bus,
            name: name.clone(),
        },
        AudioMixerOperation::SetAuxBusLevel {
            bus,
            gain_millidb,
            muted,
        } => EngineAudioMixerEdit::SetAuxBusLevel {
            bus: *bus,
            state: EngineAudioGroupState {
                gain_millidb: *gain_millidb,
                muted: *muted,
            },
        },
        AudioMixerOperation::SetAuxBusDsp { bus, chain } => EngineAudioMixerEdit::SetAuxBusDsp {
            bus: *bus,
            chain: chain.clone(),
        },
        AudioMixerOperation::SetOutputBus { output, bus } => {
            EngineAudioMixerEdit::SetOutputAuxBus {
                output: output.to_domain(),
                bus: *bus,
            }
        }
        AudioMixerOperation::SetPluginChain { target, slots } => {
            EngineAudioMixerEdit::SetPluginChain {
                target: dsp_target(*target),
                chain: Box::new(engine_plugin_chain(slots)),
            }
        }
    })
}

/// The wire operation for an engine mixer edit.
pub(super) fn wire_operation(edit: &EngineAudioMixerEdit) -> AudioMixerOperation {
    match edit {
        EngineAudioMixerEdit::SetLabel { input, label } => AudioMixerOperation::SetLabel {
            input: WireInputId::from_domain(*input),
            label: label.clone(),
        },
        EngineAudioMixerEdit::SetGroup { input, group } => AudioMixerOperation::SetGroup {
            input: WireInputId::from_domain(*input),
            group: group.map(AudioGroupNumber::get),
        },
        EngineAudioMixerEdit::RenameGroup { group, name } => AudioMixerOperation::RenameGroup {
            group: group.get(),
            name: name.clone(),
        },
        EngineAudioMixerEdit::SetGroupLevel { group, state } => {
            AudioMixerOperation::SetGroupLevel {
                group: group.get(),
                gain_millidb: state.gain_millidb,
                muted: state.muted,
            }
        }
        EngineAudioMixerEdit::SetGroupSolo { group, soloed } => AudioMixerOperation::SetGroupSolo {
            group: group.get(),
            soloed: *soloed,
        },
        EngineAudioMixerEdit::SetGroupParent { group, parent } => {
            AudioMixerOperation::SetGroupParent {
                group: group.get(),
                parent: parent.map(AudioGroupNumber::get),
            }
        }
        EngineAudioMixerEdit::SetSoloMode { mode } => AudioMixerOperation::SetSoloMode {
            mode: wire_solo_mode(*mode),
        },
        EngineAudioMixerEdit::SetMonitorDevice { device } => {
            AudioMixerOperation::SetMonitorDevice {
                device: device.clone(),
            }
        }
        EngineAudioMixerEdit::SetMonitorLevel { state } => AudioMixerOperation::SetMonitorLevel {
            gain_millidb: state.gain_millidb,
            muted: state.muted,
        },
        EngineAudioMixerEdit::SetPluginChain { target, chain } => {
            AudioMixerOperation::SetPluginChain {
                target: wire_dsp_target(*target),
                slots: wire_plugin_chain(chain),
            }
        }
        EngineAudioMixerEdit::SetStripDsp {
            target,
            chain,
            gesture,
        } => AudioMixerOperation::SetStripDsp {
            target: wire_dsp_target(*target),
            chain: chain.clone(),
            gesture: gesture.map(wire_gesture),
        },
        EngineAudioMixerEdit::ResetLoudness { target } => AudioMixerOperation::ResetLoudness {
            target: wire_dsp_target(*target),
        },
        EngineAudioMixerEdit::RenameAuxBus { bus, name } => AudioMixerOperation::RenameAuxBus {
            bus: *bus,
            name: name.clone(),
        },
        EngineAudioMixerEdit::SetAuxBusLevel { bus, state } => {
            AudioMixerOperation::SetAuxBusLevel {
                bus: *bus,
                gain_millidb: state.gain_millidb,
                muted: state.muted,
            }
        }
        EngineAudioMixerEdit::SetAuxBusDsp { bus, chain } => AudioMixerOperation::SetAuxBusDsp {
            bus: *bus,
            chain: chain.clone(),
        },
        EngineAudioMixerEdit::SetOutputAuxBus { output, bus } => {
            AudioMixerOperation::SetOutputBus {
                output: WireOutputId::from_domain(*output),
                bus: *bus,
            }
        }
    }
}

/// The wire payload of an undoable mixer or Fade-to-Black output edit.
pub(super) fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    match command {
        EngineCommand::AudioMixer(edit) => Some(CommandPayload::AudioMixer(wire_operation(edit))),
        EngineCommand::SetFadeToBlackOutput { output, affected } => {
            Some(CommandPayload::SetFadeToBlackOutput {
                output: WireOutputId::from_domain(*output),
                affected: *affected,
            })
        }
        EngineCommand::SetFadeToBlackOutputDuration {
            output,
            duration_frames,
        } => Some(CommandPayload::SetFadeToBlackOutputDuration {
            output: WireOutputId::from_domain(*output),
            duration_frames: *duration_frames,
        }),
        EngineCommand::SetFadeToBlackCurve { curve } => {
            Some(CommandPayload::SetFadeToBlackCurve { curve: *curve })
        }
        _ => None,
    }
}

/// The durable event an accepted mixer or Fade-to-Black output edit
/// publishes, if `event` is one.
pub(super) fn changed(event: &EngineEvent) -> Option<EventPayload> {
    match event {
        EngineEvent::AudioMixerChanged(edit) => {
            Some(EventPayload::AudioMixerChanged(wire_operation(edit)))
        }
        EngineEvent::FadeToBlackOutputChanged { output, affected } => {
            Some(EventPayload::FadeToBlackOutputChanged {
                output: WireOutputId::from_domain(*output),
                affected: *affected,
            })
        }
        EngineEvent::FadeToBlackOutputDurationChanged {
            output,
            duration_frames,
        } => Some(EventPayload::FadeToBlackOutputDurationChanged {
            output: WireOutputId::from_domain(*output),
            duration_frames: *duration_frames,
        }),
        EngineEvent::FadeToBlackCurveChanged { curve } => {
            Some(EventPayload::FadeToBlackCurveChanged { curve: *curve })
        }
        _ => None,
    }
}

/// The snapshot's groups, solo mode, monitor output, and labels.
pub(super) fn wire_status(show: &ShowState) -> AudioMixerStatus {
    AudioMixerStatus {
        groups: show.audio_groups().clone().map(|group| AudioGroupStatus {
            name: group.name,
            gain_millidb: group.state.gain_millidb,
            muted: group.state.muted,
            soloed: group.soloed,
            parent: group.parent.map(AudioGroupNumber::get),
        }),
        solo_mode: wire_solo_mode(show.audio_solo_mode()),
        monitor: AudioMonitorStatus {
            device: show.audio_monitor().device.clone(),
            gain_millidb: show.audio_monitor().state.gain_millidb,
            muted: show.audio_monitor().state.muted,
        },
        labels: show
            .input_audio_labels()
            .iter()
            .map(|(input, label)| InputAudioLabel {
                input: WireInputId::from_domain(*input),
                label: label.clone(),
            })
            .collect(),
        master_dsp: *show.master_audio_dsp(),
        strip_dsp_states: u16::try_from(show.input_audio_dsps().len())
            .expect("one chain per input, at most the input bound"),
        send_states: u16::try_from(strip_send_states_len(show))
            .expect("one send record per input, at most the input bound"),
        aux_buses: show.aux_buses().clone().map(|bus| AuxBusStatus {
            name: bus.name,
            gain_millidb: bus.state.gain_millidb,
            muted: bus.state.muted,
        }),
    }
}

/// The outputs with their own Fade-to-Black duration, ascending.
pub(super) fn wire_output_durations(show: &ShowState) -> Vec<(WireOutputId, u32)> {
    show.desired_switcher()
        .fade_to_black_output_durations()
        .map(|(output, frames)| (WireOutputId::from_domain(output), frames))
        .collect()
}

/// The outputs Fade to Black leaves live, ascending.
pub(super) fn wire_excluded_outputs(show: &ShowState) -> Vec<WireOutputId> {
    show.desired_switcher()
        .fade_to_black_excluded_outputs()
        .iter()
        .copied()
        .map(WireOutputId::from_domain)
        .collect()
}
