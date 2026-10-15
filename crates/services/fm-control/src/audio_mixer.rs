//! Mixer labels, strip groups, and the Fade-to-Black output selection
//! between the engine and the wire (Protocol 2.40, AU-001 and SW-004).

use fm_engine::{
    EngineAudioGroupState, EngineAudioMixerEdit, EngineCommand, EngineEvent, ShowState,
};
use fm_model::AudioGroupNumber;
use fm_protocol::{
    AudioGroupStatus, AudioMixerOperation, AudioMixerStatus, CommandPayload, EventPayload,
    InputAudioLabel, WireInputId, WireOutputId,
};

fn group_number(group: u8) -> AudioGroupNumber {
    AudioGroupNumber::new(group).expect("the codec bounds wire groups")
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
        _ => None,
    }
}

/// The snapshot's groups and labels.
pub(super) fn wire_status(show: &ShowState) -> AudioMixerStatus {
    AudioMixerStatus {
        groups: show.audio_groups().clone().map(|group| AudioGroupStatus {
            name: group.name,
            gain_millidb: group.state.gain_millidb,
            muted: group.state.muted,
        }),
        labels: show
            .input_audio_labels()
            .iter()
            .map(|(input, label)| InputAudioLabel {
                input: WireInputId::from_domain(*input),
                label: label.clone(),
            })
            .collect(),
    }
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
