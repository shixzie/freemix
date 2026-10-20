//! Wire mapping of the cue list commands, events, and snapshot records
//! (Protocol 2.51, RC-017).

use fm_engine::{Engine, EngineCommand, EngineCueListDefinition, EngineCueOperation};
use fm_model::{
    Cue, CueAction, CueList, CueMediaOperation, CueNumberUpdate, CueRunState, RecorderSlot,
    StreamTargetId, TargetBindings, TitleFieldId,
};
use fm_protocol::{
    CommandPayload, CueArmedStatus, CueFaultStatus, CueListSpec, CueListStateMessage,
    CueListStatus, CueMediaTransport, CueOperation, CuePendingStatus, CueRunChange, CueRunStatus,
    CueTitleText, CueTitleValue, EventPayload, RecordingStatus, WireCue, WireCueAction,
    WireCueMediaOperation, WireInputId, WireInputTarget, WireOverlayChannelId, WireRecorderSlot,
    WireStreamTargetId, WireTargetBindings, WireTitleFieldId,
};
use fm_titles::FieldId;
use fm_types::InputId;
use fm_types::InputTarget;

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
    assert!(fm_protocol::MAX_CUE_ARM_MS == fm_model::MAX_CUE_ARM_MS);
    assert!(fm_protocol::MAX_CUE_TRANSITION_FRAMES == fm_model::MAX_CUE_TRANSITION_FRAMES);
    assert!(fm_protocol::MAX_CUE_TITLE_NUMBER == fm_model::MAX_CUE_TITLE_NUMBER);
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
            input: domain_target(*input),
        },
        WireCueAction::Take {
            input,
            effect,
            duration_frames,
        } => CueAction::Take {
            input: input.map(domain_target),
            effect: crate::transition::domain_effect(*effect),
            duration_frames: *duration_frames,
        },
        WireCueAction::OverlayOn {
            channel: number,
            input,
        } => CueAction::OverlayOn {
            channel: channel(*number),
            input: domain_target(*input),
        },
        WireCueAction::OverlayOff { channel: number } => CueAction::OverlayOff {
            channel: channel(*number),
        },
        WireCueAction::TitleOn {
            input,
            channel: number,
        } => CueAction::TitleOn {
            input: domain_target(*input),
            channel: channel(*number),
        },
        WireCueAction::TitleText { input, field, text } => CueAction::TitleText {
            input: domain_target(*input),
            field: TitleFieldId::new(field.get()),
            text: text.clone(),
        },
        WireCueAction::TitleNumber {
            input,
            field,
            add,
            value,
        } => CueAction::TitleNumber {
            input: domain_target(*input),
            field: TitleFieldId::new(field.get()),
            update: if *add {
                CueNumberUpdate::Add(*value)
            } else {
                CueNumberUpdate::Set(*value)
            },
        },
        WireCueAction::TitleOff { input } => CueAction::TitleOff {
            input: domain_target(*input),
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
            input: domain_target(*input),
            gain_millidb: *gain_millidb,
        },
        WireCueAction::AudioMute { input, muted } => CueAction::AudioMute {
            input: domain_target(*input),
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
            input: domain_target(*input),
            operation: match *operation {
                WireCueMediaOperation::Play => CueMediaOperation::Play,
                WireCueMediaOperation::Pause => CueMediaOperation::Pause,
                WireCueMediaOperation::Seek { frame } => CueMediaOperation::Seek { frame },
                WireCueMediaOperation::Restart => CueMediaOperation::Restart,
            },
        },
        WireCueAction::Page { input, navigation } => CueAction::Page {
            input: domain_target(*input),
            navigation: *navigation,
        },
        WireCueAction::Wait { frames } => CueAction::Wait { frames: *frames },
        WireCueAction::Delay { millis } => CueAction::Delay { millis: *millis },
        WireCueAction::RunScript { script } => CueAction::RunScript { script: *script },
    }
}

/// The wire form of a project action.
#[must_use]
pub fn wire_action(action: &CueAction) -> WireCueAction {
    match action {
        CueAction::Preview { input } => WireCueAction::Preview {
            input: wire_target(*input),
        },
        CueAction::Take {
            input,
            effect,
            duration_frames,
        } => WireCueAction::Take {
            input: input.map(wire_target),
            effect: crate::transition::wire_effect(*effect),
            duration_frames: *duration_frames,
        },
        CueAction::OverlayOn { channel, input } => WireCueAction::OverlayOn {
            channel: wire_channel(*channel),
            input: wire_target(*input),
        },
        CueAction::OverlayOff { channel } => WireCueAction::OverlayOff {
            channel: wire_channel(*channel),
        },
        CueAction::TitleOn { input, channel } => WireCueAction::TitleOn {
            input: wire_target(*input),
            channel: wire_channel(*channel),
        },
        CueAction::TitleText { input, field, text } => WireCueAction::TitleText {
            input: wire_target(*input),
            field: WireTitleFieldId::new(field.get()),
            text: text.clone(),
        },
        CueAction::TitleNumber {
            input,
            field,
            update,
        } => {
            let (add, value) = match *update {
                CueNumberUpdate::Set(value) => (false, value),
                CueNumberUpdate::Add(delta) => (true, delta),
            };
            WireCueAction::TitleNumber {
                input: wire_target(*input),
                field: WireTitleFieldId::new(field.get()),
                add,
                value,
            }
        }
        CueAction::TitleOff { input } => WireCueAction::TitleOff {
            input: wire_target(*input),
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
            input: wire_target(*input),
            gain_millidb: *gain_millidb,
        },
        CueAction::AudioMute { input, muted } => WireCueAction::AudioMute {
            input: wire_target(*input),
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
            input: wire_target(*input),
            operation: match *operation {
                CueMediaOperation::Play => WireCueMediaOperation::Play,
                CueMediaOperation::Pause => WireCueMediaOperation::Pause,
                CueMediaOperation::Seek { frame } => WireCueMediaOperation::Seek { frame },
                CueMediaOperation::Restart => WireCueMediaOperation::Restart,
            },
        },
        CueAction::Page { input, navigation } => WireCueAction::Page {
            input: wire_target(*input),
            navigation: *navigation,
        },
        CueAction::Wait { frames } => WireCueAction::Wait { frames: *frames },
        CueAction::Delay { millis } => WireCueAction::Delay { millis: *millis },
        CueAction::RunScript { script } => WireCueAction::RunScript { script: *script },
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
        fault: state.fault.as_ref().map(|fault| CueFaultStatus {
            cue: fault.cue,
            action: action_index(fault.action),
            detail: fault.detail.clone(),
        }),
        armed: state.armed.map(|armed| CueArmedStatus {
            cue: armed.cue,
            due_frame: armed.due_frame,
            targets: armed.targets.map(wire_targets),
        }),
        targets: state.targets.map(wire_targets),
    }
}

/// The wire form of a run's bound targets (Protocol 2.89).
fn wire_targets(targets: fm_types::TargetBindings) -> fm_protocol::WireTargetBindings {
    targets.map(fm_protocol::WireInputId::from_domain)
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

/// The project form of a wire action's target (RC-004).
#[must_use]
pub fn domain_target(target: WireInputTarget) -> InputTarget {
    target.map(WireInputId::to_domain)
}

/// The wire form of an action's target (RC-004).
#[must_use]
pub fn wire_target(target: InputTarget) -> WireInputTarget {
    target.map(WireInputId::from_domain)
}

/// The engine form of a fire's target bindings (RC-004).
#[must_use]
pub fn domain_targets(targets: WireTargetBindings) -> TargetBindings {
    targets.map(WireInputId::to_domain)
}

/// `payload` with the dynamic targets of the fire it asks for bound on
/// `engine`'s current show (RC-004), or `None` when it fires nothing
/// naming a dynamic target: a `shortcut_fire` whose actions (or the cue
/// list its GO fires) name one, and a `cue_go` or `cue_arm` of a list
/// naming one. Preview and Program are bound to the show's, unless the
/// payload (a journal replay) carries them: a live client's fire is
/// stripped of them first ([`client_fire_payload`]). The console's
/// selected input is the payload's own. The authority executes and journals the bound
/// payload, so a replay acts on the same inputs.
#[must_use]
pub fn bind_fire_targets(engine: &Engine, payload: &CommandPayload) -> Option<CommandPayload> {
    let show = engine.show();
    let list_is_dynamic = |list: u64| {
        show.cue_list(list).is_some_and(|found| {
            found
                .cues
                .iter()
                .flat_map(|cue| &cue.actions)
                .any(CueAction::is_dynamic)
        })
    };
    let switcher = show.desired_switcher();
    let bind = |targets: WireTargetBindings| WireTargetBindings {
        preview: targets
            .preview
            .or_else(|| Some(WireInputId::from_domain(switcher.preview()))),
        program: targets
            .program
            .or_else(|| Some(WireInputId::from_domain(switcher.program()))),
        selected: targets.selected,
    };
    match payload {
        CommandPayload::FireShortcut { shortcut, targets } => {
            let dynamic = match show.shortcut(*shortcut).map(|found| &found.action) {
                Some(fm_model::ShortcutAction::Actions(actions)) => {
                    actions.iter().any(CueAction::is_dynamic)
                }
                Some(fm_model::ShortcutAction::Cue {
                    list,
                    operation: fm_model::ShortcutCueOperation::Go { .. },
                }) => list_is_dynamic(*list),
                _ => false,
            };
            dynamic.then(|| CommandPayload::FireShortcut {
                shortcut: *shortcut,
                targets: bind(*targets),
            })
        }
        CommandPayload::CueControl {
            list,
            operation: CueOperation::Go { cue, targets },
        } if list_is_dynamic(*list) => Some(CommandPayload::CueControl {
            list: *list,
            operation: CueOperation::Go {
                cue: *cue,
                targets: bind(*targets),
            },
        }),
        CommandPayload::CueControl {
            list,
            operation:
                CueOperation::Arm {
                    cue,
                    in_ms,
                    targets,
                },
        } if list_is_dynamic(*list) => Some(CommandPayload::CueControl {
            list: *list,
            operation: CueOperation::Arm {
                cue: *cue,
                in_ms: *in_ms,
                targets: bind(*targets),
            },
        }),
        _ => None,
    }
}

/// `payload` without the Preview and Program bindings a client sent with a
/// live fire (RC-004), or `None` when it carries none: the authority binds
/// them itself on the show the fire is prepared on (see
/// [`bind_fire_targets`]), so a console cannot steer a role to an input
/// that is no longer on Preview or Program. Only the selection is the
/// client's; the bound fields are the journal's, which replays them.
#[must_use]
pub fn client_fire_payload(payload: &CommandPayload) -> Option<CommandPayload> {
    let selection = |targets: &WireTargetBindings| {
        (targets.preview.is_some() || targets.program.is_some()).then_some(WireTargetBindings {
            selected: targets.selected,
            ..WireTargetBindings::NONE
        })
    };
    match payload {
        CommandPayload::FireShortcut { shortcut, targets } => {
            selection(targets).map(|targets| CommandPayload::FireShortcut {
                shortcut: *shortcut,
                targets,
            })
        }
        CommandPayload::CueControl {
            list,
            operation: CueOperation::Go { cue, targets },
        } => selection(targets).map(|targets| CommandPayload::CueControl {
            list: *list,
            operation: CueOperation::Go { cue: *cue, targets },
        }),
        CommandPayload::CueControl {
            list,
            operation:
                CueOperation::Arm {
                    cue,
                    in_ms,
                    targets,
                },
        } => selection(targets).map(|targets| CommandPayload::CueControl {
            list: *list,
            operation: CueOperation::Arm {
                cue: *cue,
                in_ms: *in_ms,
                targets,
            },
        }),
        _ => None,
    }
}

fn domain_operation(operation: CueOperation) -> EngineCueOperation {
    match operation {
        CueOperation::Go { cue, targets } => EngineCueOperation::Go {
            cue,
            targets: domain_targets(targets),
        },
        CueOperation::Standby { cue } => EngineCueOperation::Standby { cue },
        CueOperation::Back => EngineCueOperation::Back,
        CueOperation::Skip => EngineCueOperation::Skip,
        CueOperation::Stop => EngineCueOperation::Stop,
        CueOperation::Reset => EngineCueOperation::Reset,
        CueOperation::Arm {
            cue,
            in_ms,
            targets,
        } => EngineCueOperation::Arm {
            cue,
            in_millis: in_ms,
            targets: domain_targets(targets),
        },
        CueOperation::Disarm => EngineCueOperation::Disarm,
    }
}

/// Maps the cue list commands.
pub fn cue_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetCueList {
            list,
            spec,
            position,
        } => EngineCommand::SetCueList {
            list: *list,
            definition: Some(EngineCueListDefinition {
                name: spec.name.clone(),
                cues: spec.cues.iter().map(domain_cue).collect(),
            }),
            position: position.map(usize::from),
        },
        CommandPayload::RemoveCueList { list } => EngineCommand::SetCueList {
            list: *list,
            definition: None,
            position: None,
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
    let EngineCommand::SetCueList {
        list,
        definition,
        position,
    } = command
    else {
        return None;
    };
    Some(match definition {
        Some(definition) => CommandPayload::SetCueList {
            list: *list,
            spec: Box::new(wire_spec(&definition.name, &definition.cues)),
            position: position.and_then(|position| u8::try_from(position).ok()),
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

/// What an accepted live operation or advance did, as its engine event
/// reports it.
pub struct CueRun<'a> {
    pub list: u64,
    pub frame: u64,
    pub fired: &'a [u64],
    pub title_texts: &'a [(InputId, FieldId, fm_titles::FieldValue)],
    pub applied: &'a [EngineCommand],
    /// The script steps the frame ran, in order (Protocol 2.61).
    pub scripts: &'a [fm_engine::CueScriptRun],
}

/// The event of an accepted live operation or advance.
pub fn cue_run_changed(engine: &Engine, run: &CueRun<'_>) -> EventPayload {
    let state = engine
        .show()
        .cue_list(run.list)
        .map(|list| wire_state(&list.state))
        .unwrap_or_default();
    let titles = title_texts_of(engine, run.title_texts);
    let switcher = engine.show().desired_switcher();
    EventPayload::CueRunChanged(Box::new(CueRunChange {
        list: run.list,
        frame: run.frame,
        state,
        fired: run.fired.to_vec(),
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
        media: media_transports(engine, run.applied),
        scripts: run
            .scripts
            .iter()
            .map(|ran| fm_protocol::CueScriptRun {
                script: ran.script,
                actions: ran.actions,
                instructions: ran.instructions,
            })
            .collect(),
    }))
}

/// The last value each field received, in first-set order, with the
/// field revision after the command.
pub(crate) fn title_texts_of(
    engine: &Engine,
    title_texts: &[(InputId, FieldId, fm_titles::FieldValue)],
) -> Vec<CueTitleText> {
    // The last value each field received, in first-set order.
    let mut titles: Vec<CueTitleText> = Vec::new();
    for (input, field, value) in title_texts {
        let value = match value {
            fm_titles::FieldValue::Text(text) => CueTitleValue::Text(text.clone()),
            fm_titles::FieldValue::Number(number) => CueTitleValue::Number(*number),
            // A cue sets only text and number fields.
            fm_titles::FieldValue::Image(_) | fm_titles::FieldValue::Color(_) => continue,
        };
        let input = WireInputId::from_domain(*input);
        let field = WireTitleFieldId::new(field.get());
        match titles
            .iter_mut()
            .find(|title| title.input == input && title.field == field)
        {
            Some(title) => title.value = value,
            None => titles.push(CueTitleText {
                input,
                field_revision: 0,
                field,
                value,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> WireInputId {
        WireInputId::from_domain(fm_types::InputId::new(
            core::num::NonZeroU128::new(value).expect("nonzero"),
        ))
    }

    /// A live fire keeps only the console's selection (RC-004); a fire
    /// binding nothing else, and any other payload, is left as sent.
    #[test]
    fn a_client_fire_keeps_only_its_selection() {
        let sent = WireTargetBindings {
            preview: Some(id(1)),
            program: Some(id(2)),
            selected: Some(id(3)),
        };
        let selection = WireTargetBindings {
            selected: Some(id(3)),
            ..WireTargetBindings::NONE
        };
        let go = |targets| CommandPayload::CueControl {
            list: 1,
            operation: CueOperation::Go { cue: None, targets },
        };
        let arm = |targets| CommandPayload::CueControl {
            list: 1,
            operation: CueOperation::Arm {
                cue: Some(2),
                in_ms: 500,
                targets,
            },
        };
        let fire = |targets| CommandPayload::FireShortcut {
            shortcut: 4,
            targets,
        };
        let payloads: [&dyn Fn(WireTargetBindings) -> CommandPayload; 3] = [&go, &arm, &fire];
        for payload in payloads {
            assert_eq!(
                client_fire_payload(&payload(sent)),
                Some(payload(selection))
            );
            assert_eq!(client_fire_payload(&payload(selection)), None);
        }
        assert_eq!(client_fire_payload(&CommandPayload::Cut), None);
    }
}
