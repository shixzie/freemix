//! Wire mapping of the cue list commands, events, and snapshot records
//! (Protocol 2.51, RC-017).

use fm_engine::{Engine, EngineCommand, EngineCueListDefinition, EngineCueOperation};
use fm_model::{
    Cue, CueAction, CueList, CueMediaOperation, CueRunState, RecorderSlot, StreamTargetId,
    TitleFieldId,
};
use fm_protocol::{
    CommandPayload, CueListSpec, CueListStateMessage, CueListStatus, CueMediaTransport,
    CueOperation, CuePendingStatus, CueRunChange, CueRunStatus, CueTitleText, EventPayload,
    RecordingStatus, WireCue, WireCueAction, WireCueMediaOperation, WireInputId,
    WireOverlayChannelId, WireRecorderSlot, WireStreamTargetId, WireTitleFieldId,
};
use fm_titles::FieldId;
use fm_types::InputId;

// The wire and the model agree on every cue list bound.
const _: () = {
    assert!(fm_protocol::MAX_CUE_LISTS == fm_model::MAX_CUE_LISTS);
    assert!(fm_protocol::MAX_CUES == fm_model::MAX_CUES);
    assert!(fm_protocol::MAX_CUE_ACTIONS == fm_model::MAX_CUE_ACTIONS);
    assert!(fm_protocol::MAX_CUE_LIST_ACTIONS == fm_model::MAX_CUE_LIST_ACTIONS);
    assert!(fm_protocol::MAX_CUE_NAME_BYTES == fm_model::MAX_CUE_NAME_BYTES);
    assert!(fm_protocol::MAX_CUE_TEXT_BYTES == fm_model::MAX_CUE_TEXT_BYTES);
    assert!(fm_protocol::MAX_CUE_LIST_TEXT_BYTES == fm_model::MAX_CUE_LIST_TEXT_BYTES);
    assert!(fm_protocol::MAX_CUE_WAIT_FRAMES == fm_model::MAX_CUE_WAIT_FRAMES);
    assert!(fm_protocol::MAX_CUE_DELAY_MS == fm_model::MAX_CUE_DELAY_MS);
    assert!(fm_protocol::MAX_CUE_TRANSITION_FRAMES == fm_model::MAX_CUE_TRANSITION_FRAMES);
};

fn channel(channel: WireOverlayChannelId) -> u8 {
    channel.number()
}

fn wire_channel(channel: u8) -> WireOverlayChannelId {
    WireOverlayChannelId::new(channel).expect("validated cue overlay channels are 1..=8")
}

/// The project form of a wire action.
#[must_use]
pub fn domain_action(action: &WireCueAction) -> CueAction {
    match action {
        WireCueAction::Preview { input } => CueAction::Preview {
            input: input.to_domain(),
        },
        WireCueAction::Take {
            input,
            effect,
            duration_frames,
        } => CueAction::Take {
            input: input.map(WireInputId::to_domain),
            effect: crate::transition::domain_effect(*effect),
            duration_frames: *duration_frames,
        },
        WireCueAction::OverlayOn {
            channel: number,
            input,
        } => CueAction::OverlayOn {
            channel: channel(*number),
            input: input.to_domain(),
        },
        WireCueAction::OverlayOff { channel: number } => CueAction::OverlayOff {
            channel: channel(*number),
        },
        WireCueAction::TitleOn {
            input,
            channel: number,
        } => CueAction::TitleOn {
            input: input.to_domain(),
            channel: channel(*number),
        },
        WireCueAction::TitleText { input, field, text } => CueAction::TitleText {
            input: input.to_domain(),
            field: TitleFieldId::new(field.get()),
            text: text.clone(),
        },
        WireCueAction::TitleOff { input } => CueAction::TitleOff {
            input: input.to_domain(),
        },
        WireCueAction::FadeToBlack {
            active,
            duration_frames,
        } => CueAction::FadeToBlack {
            active: *active,
            duration_frames: *duration_frames,
        },
        WireCueAction::PlaylistStart => CueAction::PlaylistStart,
        WireCueAction::PlaylistStop => CueAction::PlaylistStop,
        WireCueAction::AudioGain {
            input,
            gain_millidb,
        } => CueAction::AudioGain {
            input: input.to_domain(),
            gain_millidb: *gain_millidb,
        },
        WireCueAction::AudioMute { input, muted } => CueAction::AudioMute {
            input: input.to_domain(),
            muted: *muted,
        },
        WireCueAction::Record { recorder, start } => CueAction::Record {
            recorder: RecorderSlot::new(recorder.get()).expect("wire recorder slots match"),
            start: *start,
        },
        WireCueAction::Stream { target, start } => CueAction::Stream {
            target: StreamTargetId::new(target.get()),
            start: *start,
        },
        WireCueAction::Media { input, operation } => CueAction::Media {
            input: input.to_domain(),
            operation: match *operation {
                WireCueMediaOperation::Play => CueMediaOperation::Play,
                WireCueMediaOperation::Pause => CueMediaOperation::Pause,
                WireCueMediaOperation::Seek { frame } => CueMediaOperation::Seek { frame },
                WireCueMediaOperation::Restart => CueMediaOperation::Restart,
            },
        },
        WireCueAction::Wait { frames } => CueAction::Wait { frames: *frames },
        WireCueAction::Delay { millis } => CueAction::Delay { millis: *millis },
    }
}

/// The wire form of a project action.
#[must_use]
pub fn wire_action(action: &CueAction) -> WireCueAction {
    match action {
        CueAction::Preview { input } => WireCueAction::Preview {
            input: WireInputId::from_domain(*input),
        },
        CueAction::Take {
            input,
            effect,
            duration_frames,
        } => WireCueAction::Take {
            input: input.map(WireInputId::from_domain),
            effect: crate::transition::wire_effect(*effect),
            duration_frames: *duration_frames,
        },
        CueAction::OverlayOn { channel, input } => WireCueAction::OverlayOn {
            channel: wire_channel(*channel),
            input: WireInputId::from_domain(*input),
        },
        CueAction::OverlayOff { channel } => WireCueAction::OverlayOff {
            channel: wire_channel(*channel),
        },
        CueAction::TitleOn { input, channel } => WireCueAction::TitleOn {
            input: WireInputId::from_domain(*input),
            channel: wire_channel(*channel),
        },
        CueAction::TitleText { input, field, text } => WireCueAction::TitleText {
            input: WireInputId::from_domain(*input),
            field: WireTitleFieldId::new(field.get()),
            text: text.clone(),
        },
        CueAction::TitleOff { input } => WireCueAction::TitleOff {
            input: WireInputId::from_domain(*input),
        },
        CueAction::FadeToBlack {
            active,
            duration_frames,
        } => WireCueAction::FadeToBlack {
            active: *active,
            duration_frames: *duration_frames,
        },
        CueAction::PlaylistStart => WireCueAction::PlaylistStart,
        CueAction::PlaylistStop => WireCueAction::PlaylistStop,
        CueAction::AudioGain {
            input,
            gain_millidb,
        } => WireCueAction::AudioGain {
            input: WireInputId::from_domain(*input),
            gain_millidb: *gain_millidb,
        },
        CueAction::AudioMute { input, muted } => WireCueAction::AudioMute {
            input: WireInputId::from_domain(*input),
            muted: *muted,
        },
        CueAction::Record { recorder, start } => WireCueAction::Record {
            recorder: WireRecorderSlot::new(recorder.get()).expect("model recorder slots match"),
            start: *start,
        },
        CueAction::Stream { target, start } => WireCueAction::Stream {
            target: WireStreamTargetId::new(target.get()),
            start: *start,
        },
        CueAction::Media { input, operation } => WireCueAction::Media {
            input: WireInputId::from_domain(*input),
            operation: match *operation {
                CueMediaOperation::Play => WireCueMediaOperation::Play,
                CueMediaOperation::Pause => WireCueMediaOperation::Pause,
                CueMediaOperation::Seek { frame } => WireCueMediaOperation::Seek { frame },
                CueMediaOperation::Restart => WireCueMediaOperation::Restart,
            },
        },
        CueAction::Wait { frames } => WireCueAction::Wait { frames: *frames },
        CueAction::Delay { millis } => WireCueAction::Delay { millis: *millis },
    }
}

/// The project form of a wire cue.
#[must_use]
pub fn domain_cue(cue: &WireCue) -> Cue {
    Cue {
        id: cue.id,
        name: cue.name.clone(),
        actions: cue.actions.iter().map(domain_action).collect(),
        auto_follow: cue.auto_follow,
    }
}

/// The wire form of a project cue.
#[must_use]
pub fn wire_cue(cue: &Cue) -> WireCue {
    WireCue {
        id: cue.id,
        name: cue.name.clone(),
        auto_follow: cue.auto_follow,
        actions: cue.actions.iter().map(wire_action).collect(),
    }
}

/// The wire form of a definition.
#[must_use]
pub fn wire_spec(name: &str, cues: &[Cue]) -> CueListSpec {
    CueListSpec {
        name: name.to_owned(),
        cues: cues.iter().map(wire_cue).collect(),
    }
}

fn action_index(index: usize) -> u16 {
    u16::try_from(index).expect("cue action indices fit the bounded action count")
}

/// The wire form of a run state.
#[must_use]
pub fn wire_state(state: &CueRunState) -> CueRunStatus {
    CueRunStatus {
        standby: state.standby,
        live: state.live,
        pending: state.pending.map(|pending| CuePendingStatus {
            cue: pending.cue,
            action: action_index(pending.action),
            due_frame: pending.due_frame,
        }),
        played: state.played.clone(),
        fault: state
            .fault
            .map(|fault| (fault.cue, action_index(fault.action))),
    }
}

/// The wire form of a whole list.
#[must_use]
pub fn wire_list(list: &CueList) -> CueListStatus {
    CueListStatus {
        id: list.id,
        spec: wire_spec(&list.name, &list.cues),
        state: wire_state(&list.state),
    }
}

const fn domain_operation(operation: CueOperation) -> EngineCueOperation {
    match operation {
        CueOperation::Go { cue } => EngineCueOperation::Go { cue },
        CueOperation::Standby { cue } => EngineCueOperation::Standby { cue },
        CueOperation::Back => EngineCueOperation::Back,
        CueOperation::Skip => EngineCueOperation::Skip,
        CueOperation::Stop => EngineCueOperation::Stop,
        CueOperation::Reset => EngineCueOperation::Reset,
    }
}

/// Maps the cue list commands.
pub fn cue_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetCueList { list, spec } => EngineCommand::SetCueList {
            list: *list,
            definition: Some(EngineCueListDefinition {
                name: spec.name.clone(),
                cues: spec.cues.iter().map(domain_cue).collect(),
            }),
        },
        CommandPayload::RemoveCueList { list } => EngineCommand::SetCueList {
            list: *list,
            definition: None,
        },
        CommandPayload::CueControl { list, operation } => EngineCommand::CueControl {
            list: *list,
            operation: domain_operation(*operation),
        },
        CommandPayload::CueAdvance { list, frame } => EngineCommand::CueAdvance {
            list: *list,
            frame: *frame,
        },
        _ => unreachable!("only cue list commands are delegated"),
    }
}

/// The wire form of a list edit (for undo and redo previews).
pub fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetCueList { list, definition } = command else {
        return None;
    };
    Some(match definition {
        Some(definition) => CommandPayload::SetCueList {
            list: *list,
            spec: Box::new(wire_spec(&definition.name, &definition.cues)),
        },
        None => CommandPayload::RemoveCueList { list: *list },
    })
}

/// How many `cue_list_state` records follow a snapshot.
pub fn cue_list_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().cue_lists().len()).expect("cue lists are bounded")
}

/// The `cue_list_state` records after a snapshot, in project order.
pub fn cue_list_states(engine: &Engine) -> Vec<CueListStateMessage> {
    engine
        .show()
        .cue_lists()
        .iter()
        .map(|list| CueListStateMessage {
            revision: engine.revision().get(),
            frame: engine.frame_cursor().get(),
            list: wire_list(list),
        })
        .collect()
}

/// The event of an accepted list edit.
pub fn cue_list_changed(engine: &Engine, list: u64) -> EventPayload {
    engine
        .show()
        .cue_list(list)
        .map_or(EventPayload::CueListRemoved { list }, |found| {
            EventPayload::CueListChanged(Box::new(wire_list(found)))
        })
}

/// The event of an accepted live operation or advance.
pub fn cue_run_changed(
    engine: &Engine,
    list: u64,
    frame: u64,
    fired: &[u64],
    title_texts: &[(InputId, FieldId, String)],
    applied: &[EngineCommand],
) -> EventPayload {
    let state = engine
        .show()
        .cue_list(list)
        .map(|list| wire_state(&list.state))
        .unwrap_or_default();
    let titles = title_texts_of(engine, title_texts);
    let switcher = engine.show().desired_switcher();
    EventPayload::CueRunChanged(Box::new(CueRunChange {
        list,
        frame,
        state,
        fired: fired.to_vec(),
        program: WireInputId::from_domain(switcher.program()),
        preview: WireInputId::from_domain(switcher.preview()),
        manual_transition: crate::protocol_manual_status(engine.desired_manual_transition()),
        fade_to_black: crate::protocol_fade_to_black_state(engine.desired_fade_to_black()),
        overlays: crate::protocol_overlays(switcher.overlays()),
        input_audio_strips: crate::protocol_input_audio_strips(engine),
        streams: crate::protocol_streams(engine),
        recordings: recordings(engine),
        playlist: crate::playlist::wire_run_state(engine),
        titles,
        media: media_transports(engine, applied),
    }))
}

/// The last text each field received, in first-set order, with the
/// field revision after the command.
pub(crate) fn title_texts_of(
    engine: &Engine,
    title_texts: &[(InputId, FieldId, String)],
) -> Vec<CueTitleText> {
    // The last text each field received, in first-set order.
    let mut titles: Vec<CueTitleText> = Vec::new();
    for (input, field, text) in title_texts {
        let input = WireInputId::from_domain(*input);
        let field = WireTitleFieldId::new(field.get());
        match titles
            .iter_mut()
            .find(|title| title.input == input && title.field == field)
        {
            Some(title) => title.text.clone_from(text),
            None => titles.push(CueTitleText {
                input,
                field_revision: 0,
                field,
                text: text.clone(),
            }),
        }
    }
    for title in &mut titles {
        title.field_revision = engine
            .show()
            .title(title.input.to_domain())
            .map_or(0, |found| found.fields.revision());
    }
    titles
}

/// The desired recorder states.
pub(crate) fn recordings(engine: &Engine) -> Vec<RecordingStatus> {
    let switcher = engine.show().desired_switcher();
    fm_switcher::RecorderSlot::ALL
        .into_iter()
        .map(|recorder| RecordingStatus {
            recorder: crate::wire_recorder_slot(recorder),
            desired_active: switcher.recording_desired(recorder),
        })
        .collect()
}

/// The transports of every media input `applied` addressed, ascending.
pub(crate) fn media_transports(
    engine: &Engine,
    applied: &[EngineCommand],
) -> Vec<CueMediaTransport> {
    let inputs: std::collections::BTreeSet<InputId> = applied
        .iter()
        .filter_map(|command| match command {
            EngineCommand::Media(media) => Some(media.input),
            _ => None,
        })
        .collect();
    inputs
        .into_iter()
        .map(|input| {
            let status = crate::media::status(engine, input);
            CueMediaTransport {
                input: status.input,
                state: status.transport.state,
                position: status.transport.position,
                position_time: status.transport.position_time,
            }
        })
        .collect()
}
