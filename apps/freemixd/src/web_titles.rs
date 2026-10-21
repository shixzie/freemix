//! What a Web session may see and change of a title (GX-006, Protocol 2.90).
//!
//! A title template marks each field exposed to the browser title editor or
//! hidden from it. A Web session (anything the gateway relays) is never sent
//! a hidden field at all: its `title_state` records leave hidden fields out
//! (name, value, and id), its `title_changed` events and the title values
//! of its `cue_run_changed`, `script_ran`, and `shortcut_fired` events carry
//! only the exposed values, and its `title_exposure_changed` events name
//! only the exposed fields. A Web session may not name a hidden field in `title_fields` or
//! `title_take` (refused as if the field did not exist) and may not change
//! exposure itself; Studio does that.
//!
//! Cue lists, shortcuts, and scripts (Protocol 2.104) set title fields too.
//! A Web session's `cue_list_state`, `cue_list_changed`, `shortcut_state`,
//! and `shortcut_changed` carry each title text or number action that sets
//! a field hidden from it as the `TitleHidden` marker
//! ([`fm_protocol::HIDDEN_TITLE_VALUE`]) in place of its value, and its
//! `script_state` and `script_changed` withhold every script's source (a
//! script's verbs cannot be reliably redacted). An action whose target is
//! bound only when it fires (`preview`, `program`, or `selected`) sets a
//! hidden field when any title hides a field of that id: the title it will
//! reach is unknown, so the value is withheld whenever it might be hidden.
//! After a `title_exposure_changed` a Web session is re-sent each cue list
//! and shortcut that sets a title field, as it may now see them.
//!
//! A Web session's `cue_list_set` or `shortcut_set` may send a marker back
//! where the stored definition has the action it stands for (same cue id
//! and action position, same title target, field, and kind, still hidden):
//! the stored action is kept, so redaction never overwrites real data. A
//! marker anywhere else is refused `invalid_command`; an action that sets a
//! hidden field with a value of its own is refused `permission_denied`
//! whatever the value, so an edit cannot test a guess; and `script_set` is
//! refused `permission_denied`.
//!
//! [`PhasePacer`] decides when the lossy `title_phases` record is sent to
//! the sessions that take it (Web pages and raw Studio sessions).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use fm_protocol::{
    CommandMessage, CommandPayload, CommandResult, CueListSpec, CueListStatus, EventPayload,
    ShortcutSpec, ShortcutStatus, TitlePhase, TitlePhasesMessage, TitleStateMessage,
    TitleTimelineStatus, WireCueAction, WireInputId, WireInputTarget, WireMessage,
    WireShortcutAction, WireTitleFieldId,
};

/// How often the daemon looks at the titles' phases.
pub(crate) const TITLE_PHASES_CHECK: Duration = Duration::from_millis(100);
/// How often `title_phases` is re-sent while a title is taken or shown, so
/// a client's extrapolated playhead never drifts far.
pub(crate) const TITLE_PHASES_REFRESH: Duration = Duration::from_secs(1);

/// What a client cannot extrapolate of one title's phase: a change sends a
/// record at once. `phase_frame` advances with the frame and is not part.
type PhaseKey = (WireInputId, TitlePhase, u64, Option<TitleTimelineStatus>);

/// Paces `title_phases`: at most one look per [`TITLE_PHASES_CHECK`], a
/// record when a phase, phase length, or timeline changes, and a refresh
/// every [`TITLE_PHASES_REFRESH`] while any title is taken or shown.
#[derive(Default)]
pub(crate) struct PhasePacer {
    checked: Option<Instant>,
    sent: Option<Instant>,
    key: Vec<PhaseKey>,
}

impl PhasePacer {
    /// The record to send at `now`, if any. `demand` is whether any session
    /// takes the record; `phases` is called at most once per check.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        demand: bool,
        phases: impl FnOnce() -> TitlePhasesMessage,
    ) -> Option<TitlePhasesMessage> {
        if !demand {
            *self = Self::default();
            return None;
        }
        if self
            .checked
            .is_some_and(|checked| now < checked + TITLE_PHASES_CHECK)
        {
            return None;
        }
        self.checked = Some(now);
        let message = phases();
        let key: Vec<PhaseKey> = message
            .titles
            .iter()
            .map(|title| (title.input, title.phase, title.phase_frames, title.timeline))
            .collect();
        let active = message
            .titles
            .iter()
            .any(|title| title.phase != TitlePhase::Idle || title.timeline.is_some());
        let refresh = active
            && self
                .sent
                .is_none_or(|sent| now >= sent + TITLE_PHASES_REFRESH);
        if key == self.key && !refresh {
            return None;
        }
        self.key = key;
        self.sent = Some(now);
        Some(message)
    }
}

/// The fields each title hides from browsers, by input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HiddenFields(BTreeMap<WireInputId, BTreeSet<WireTitleFieldId>>);

impl HiddenFields {
    pub(crate) fn new(hidden: Vec<(WireInputId, Vec<WireTitleFieldId>)>) -> Self {
        Self(
            hidden
                .into_iter()
                .map(|(input, fields)| (input, fields.into_iter().collect()))
                .collect(),
        )
    }

    fn hides(&self, input: WireInputId, field: WireTitleFieldId) -> bool {
        self.0
            .get(&input)
            .is_some_and(|fields| fields.contains(&field))
    }

    /// Whether a cue or shortcut action aimed at `target`'s `field` may set
    /// a hidden field: a fixed title's own hidden field, or for a target
    /// bound only when the action fires, a field of that id hidden on any
    /// title (Protocol 2.104).
    fn hides_target(&self, target: WireInputTarget, field: WireTitleFieldId) -> bool {
        match target.fixed() {
            Some(input) => self.hides(input, field),
            None => self.0.values().any(|fields| fields.contains(&field)),
        }
    }

    /// The marker standing for `action` when it sets a field hidden from a
    /// Web session; `None` when the session may see its value.
    fn marker(&self, action: &WireCueAction) -> Option<WireCueAction> {
        let (target, field, _) = action.title_value_field()?;
        if self.hides_target(target, field) {
            action.hidden_marker()
        } else {
            None
        }
    }

    fn redact(&self, actions: &mut [WireCueAction]) {
        for action in actions {
            if let Some(marker) = self.marker(action) {
                *action = marker;
            }
        }
    }
}

/// A cue list as a Web session sees it: each action setting a field hidden
/// from it carries the marker in place of its value (Protocol 2.104).
pub(crate) fn web_cue_list(list: &mut CueListStatus, hidden: &HiddenFields) {
    for cue in &mut list.spec.cues {
        hidden.redact(&mut cue.actions);
    }
}

/// A shortcut as a Web session sees it (see [`web_cue_list`]).
pub(crate) fn web_shortcut(shortcut: &mut ShortcutStatus, hidden: &HiddenFields) {
    if let WireShortcutAction::Actions(actions) = &mut shortcut.spec.action {
        hidden.redact(actions);
    }
}

/// Whether `actions` set any title field, and so may read differently to
/// a Web session after an exposure change.
pub(crate) fn sets_title_values(actions: &[WireCueAction]) -> bool {
    actions
        .iter()
        .any(|action| action.title_value_field().is_some())
}

/// A title's state as a Web session sees it: hidden fields left out.
pub(crate) fn web_title_state(mut state: TitleStateMessage) -> TitleStateMessage {
    state.fields.retain(|status| status.exposed);
    state
}

/// The `title_state` records that resynchronize a Web session after an
/// event naming `inputs` (see [`resync_inputs`]): each named title still in
/// `states`, as the session may now see it, in input order. They are taken
/// when the event is queued, so they hold every value up to then; a
/// `title_changed` already queued behind them names an older field
/// revision, which a client skips.
pub(crate) fn web_title_states(
    states: Vec<TitleStateMessage>,
    inputs: &[WireInputId],
) -> Vec<TitleStateMessage> {
    states
        .into_iter()
        .filter(|state| inputs.contains(&state.input))
        .map(web_title_state)
        .collect()
}

/// The titles a Web session resynchronizes after `message`, in first-named
/// order: those a `title_exposure_changed` event names (Protocol 2.96), so
/// a newly exposed field's name and value reach the page, and those whose
/// fields a `cue_run_changed`, `script_ran`, or `shortcut_fired` event set,
/// since a page learns title values and field revisions only from
/// `title_changed` and `title_state`. Empty for any other record.
pub(crate) fn resync_inputs(message: &WireMessage) -> Vec<WireInputId> {
    let WireMessage::Event(event) = message else {
        return Vec::new();
    };
    let texts = match &event.payload {
        EventPayload::TitleExposureChanged { inputs, .. } => return inputs.clone(),
        EventPayload::CueRunChanged(change) => &change.titles,
        EventPayload::ScriptRan(change) => &change.titles,
        EventPayload::ShortcutFired(change) => &change.titles,
        _ => return Vec::new(),
    };
    let mut inputs = Vec::new();
    for text in texts {
        if !inputs.contains(&text.input) {
            inputs.push(text.input);
        }
    }
    inputs
}

/// A live record as a Web session sees it: title values (of title changes
/// and of cue, script, and shortcut runs) and exposure without hidden
/// fields. Everything else passes unchanged.
pub(crate) fn web_live_message(message: WireMessage, hidden: &HiddenFields) -> WireMessage {
    let WireMessage::Event(mut event) = message else {
        return message;
    };
    match &mut event.payload {
        EventPayload::TitleChanged { input, fields, .. } => {
            let input = *input;
            fields.retain(|update| !hidden.hides(input, update.field));
        }
        EventPayload::TitleExposureChanged { fields, .. } => {
            fields.retain(|item| item.exposed);
        }
        EventPayload::CueRunChanged(change) => {
            change
                .titles
                .retain(|text| !hidden.hides(text.input, text.field));
        }
        EventPayload::ScriptRan(change) => {
            change
                .titles
                .retain(|text| !hidden.hides(text.input, text.field));
        }
        EventPayload::ShortcutFired(change) => {
            change
                .titles
                .retain(|text| !hidden.hides(text.input, text.field));
        }
        EventPayload::CueListChanged(list) => web_cue_list(list, hidden),
        EventPayload::ShortcutChanged(shortcut) => web_shortcut(shortcut, hidden),
        EventPayload::ScriptChanged(script) => withhold_source(script),
        _ => {}
    }
    WireMessage::Event(event)
}

fn withhold_source(script: &mut fm_protocol::ScriptStatus) {
    script.spec.source.clear();
    script.source_hidden = true;
}

/// A record a Web session is sent (Protocol 2.104): a live event as
/// [`web_live_message`] filters it, and a cue list, shortcut, or script
/// state record redacted as the matching `_changed` event is. A
/// `title_state` must already be a Web session's (see [`web_title_state`]).
pub(crate) fn web_message(message: WireMessage, hidden: &HiddenFields) -> WireMessage {
    match message {
        WireMessage::CueListState(mut state) => {
            web_cue_list(&mut state.list, hidden);
            WireMessage::CueListState(state)
        }
        WireMessage::ShortcutState(mut state) => {
            web_shortcut(&mut state.shortcut, hidden);
            WireMessage::ShortcutState(state)
        }
        WireMessage::ScriptState(mut state) => {
            withhold_source(&mut state.script);
            WireMessage::ScriptState(state)
        }
        message => web_live_message(message, hidden),
    }
}

/// Whether `message` changes which title fields Web sessions see, so the
/// cue lists and shortcuts that set title fields are re-sent to them
/// (Protocol 2.104).
pub(crate) fn exposure_changed(message: &WireMessage) -> bool {
    matches!(
        message,
        WireMessage::Event(event)
            if matches!(event.payload, EventPayload::TitleExposureChanged { .. })
    )
}

/// Why a Web session's edit is refused (Protocol 2.104).
#[derive(Debug, Eq, PartialEq)]
enum EditRefusal {
    /// A marker with no stored hidden value behind it.
    Invalid(String),
    /// A value of its own for a hidden field, or a script edit.
    Denied(String),
}

/// Puts back each stored action a marker in `incoming` stands for:
/// `stored` is the stored definition's actions at the same place (`place`
/// names it in a refusal).
fn resolve_actions(
    incoming: &mut [WireCueAction],
    stored: &[WireCueAction],
    hidden: &HiddenFields,
    place: &str,
) -> Result<(), EditRefusal> {
    for (index, action) in incoming.iter_mut().enumerate() {
        let position = index + 1;
        if let WireCueAction::TitleHidden { input, field, .. } = *action {
            let kept = stored.get(index).filter(|stored| {
                stored.hidden_marker().as_ref() == Some(&*action)
                    && hidden.hides_target(input, field)
            });
            let Some(kept) = kept else {
                return Err(EditRefusal::Invalid(format!(
                    "{place} action {position}: the hidden title value marker stands for no \
                     stored hidden value there"
                )));
            };
            *action = kept.clone();
        } else if let Some((target, field, _)) = action.title_value_field()
            && hidden.hides_target(target, field)
        {
            return Err(EditRefusal::Denied(format!(
                "{place} action {position}: a web session cannot set title field {field} of \
                 {target}, which is hidden from the browser"
            )));
        }
    }
    Ok(())
}

fn resolve_cue_list(
    spec: &mut CueListSpec,
    stored: Option<&CueListSpec>,
    hidden: &HiddenFields,
) -> Result<(), EditRefusal> {
    for cue in &mut spec.cues {
        let stored_actions = stored
            .and_then(|stored| stored.cues.iter().find(|found| found.id == cue.id))
            .map_or(&[][..], |found| found.actions.as_slice());
        let place = format!("cue {}", cue.id);
        resolve_actions(&mut cue.actions, stored_actions, hidden, &place)?;
    }
    Ok(())
}

fn resolve_shortcut(
    spec: &mut ShortcutSpec,
    stored: Option<&ShortcutSpec>,
    hidden: &HiddenFields,
) -> Result<(), EditRefusal> {
    let WireShortcutAction::Actions(actions) = &mut spec.action else {
        return Ok(());
    };
    let stored_actions = match stored.map(|stored| &stored.action) {
        Some(WireShortcutAction::Actions(stored)) => stored.as_slice(),
        _ => &[],
    };
    resolve_actions(actions, stored_actions, hidden, "shortcut")
}

/// The stored definitions a Web session's edit may keep hidden values of.
pub(crate) struct StoredDefinitions {
    pub(crate) cue_lists: Vec<CueListStatus>,
    pub(crate) shortcuts: Vec<ShortcutStatus>,
}

/// Whether `command` is a cue list, shortcut, or script edit that
/// [`web_definition_edit`] must see before a Web session's command runs.
pub(crate) const fn is_definition_edit(command: &CommandMessage) -> bool {
    matches!(
        command.payload,
        CommandPayload::SetCueList { .. }
            | CommandPayload::SetShortcut { .. }
            | CommandPayload::SetScript { .. }
    )
}

/// A Web session's cue list, shortcut, or script edit as it executes
/// (Protocol 2.104): each marker it sends back replaced by the stored
/// action it stands for, or the refusal it meets. `None` lets it run.
pub(crate) fn web_definition_edit(
    command: &mut CommandMessage,
    hidden: &HiddenFields,
    stored: &StoredDefinitions,
    current_revision: u64,
) -> Option<CommandResult> {
    let refusal = match &mut command.payload {
        CommandPayload::SetCueList { list, spec, .. } => {
            let stored = stored
                .cue_lists
                .iter()
                .find(|found| found.id == *list)
                .map(|found| &found.spec);
            resolve_cue_list(spec, stored, hidden).err()
        }
        CommandPayload::SetShortcut { shortcut, spec, .. } => {
            let stored = stored
                .shortcuts
                .iter()
                .find(|found| found.id == *shortcut)
                .map(|found| &found.spec);
            resolve_shortcut(spec, stored, hidden).err()
        }
        CommandPayload::SetScript { .. } => Some(EditRefusal::Denied(
            "a web session cannot edit scripts: their source is withheld from the browser"
                .to_owned(),
        )),
        _ => None,
    }?;
    let (code, message) = match refusal {
        EditRefusal::Invalid(message) => ("invalid_command", message),
        EditRefusal::Denied(message) => ("permission_denied", message),
    };
    Some(CommandResult::Rejected {
        id: command.id.clone(),
        code: code.to_owned(),
        message,
        fields: Vec::new(),
        current_revision,
        retryable: false,
        reason: None,
    })
}

/// The refusal a Web session's command meets before it executes: changing
/// exposure, or naming a field its title hides. `None` lets it run.
pub(crate) fn web_command_refusal(
    command: &CommandMessage,
    hidden: &HiddenFields,
    current_revision: u64,
) -> Option<CommandResult> {
    let refuse = |code: &str, message: String| CommandResult::Rejected {
        id: command.id.clone(),
        code: code.to_owned(),
        message,
        fields: Vec::new(),
        current_revision,
        retryable: false,
        reason: None,
    };
    match &command.payload {
        CommandPayload::SetTitleFieldExposure { .. } => Some(refuse(
            "permission_denied",
            "a web session cannot change which title fields the browser shows".to_owned(),
        )),
        CommandPayload::SetTitleFields { input, fields, .. }
        | CommandPayload::TakeTitle { input, fields, .. } => fields
            .iter()
            .find(|update| hidden.hides(*input, update.field))
            .map(|update| {
                refuse(
                    "not_found",
                    format!("title {input} has no field {}", update.field),
                )
            }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{
        CURRENT_PROTOCOL_VERSION, EditHistoryStatus, EngineIdentity, EventCursor, EventMessage,
        TitleAirRequirement, TitleFieldExposure, TitleFieldStatus, TitleFieldUpdate,
        TitleFieldValue, TitlePhase,
    };

    use super::*;

    fn input(value: u128) -> WireInputId {
        WireInputId::new(NonZeroU128::new(value).unwrap())
    }

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn hidden() -> HiddenFields {
        HiddenFields::new(vec![(input(3), vec![field(2)])])
    }

    fn event(payload: EventPayload) -> WireMessage {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: EngineIdentity {
                    engine_id: "e".into(),
                    state_epoch: 1,
                    log_id: "l".into(),
                },
                revision: 4,
            },
            payload,
            edit_history: EditHistoryStatus {
                undo_depth: 0,
                redo_depth: 0,
                next_undo: None,
                next_redo: None,
            },
        })
    }

    fn update(id: u128, text: &str) -> TitleFieldUpdate {
        TitleFieldUpdate {
            field: field(id),
            value: TitleFieldValue::Text(text.into()),
        }
    }

    #[test]
    fn hidden_fields_never_reach_a_web_session() {
        let state = TitleStateMessage {
            revision: 4,
            input: input(3),
            name: "Lower".into(),
            field_revision: 1,
            phase: TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            timeline: None,
            fields: vec![
                TitleFieldStatus {
                    field: field(1),
                    name: "Name".into(),
                    value: TitleFieldValue::Text("Ana".into()),
                    exposed: true,
                    revision: 0,
                },
                TitleFieldStatus {
                    field: field(2),
                    name: "Secret".into(),
                    value: TitleFieldValue::Text("producer note".into()),
                    exposed: false,
                    revision: 0,
                },
            ],
        };
        let web = web_title_state(state);
        assert_eq!(web.fields.len(), 1);
        let line = fm_protocol::encode_line(&WireMessage::TitleState(web)).unwrap();
        assert!(
            !line.contains("Secret") && !line.contains("producer"),
            "{line}"
        );

        let changed = web_live_message(
            event(EventPayload::TitleChanged {
                input: input(3),
                field_revision: 2,
                fields: vec![update(1, "Ana"), update(2, "producer note")],
                overlays: Vec::new(),
            }),
            &hidden(),
        );
        let WireMessage::Event(EventMessage {
            payload: EventPayload::TitleChanged { fields, .. },
            ..
        }) = &changed
        else {
            panic!("{changed:?}")
        };
        assert_eq!(fields, &[update(1, "Ana")]);
        // Another title's same field id is not hidden.
        let other = web_live_message(
            event(EventPayload::TitleChanged {
                input: input(5),
                field_revision: 2,
                fields: vec![update(2, "kept")],
                overlays: Vec::new(),
            }),
            &hidden(),
        );
        assert!(format!("{other:?}").contains("kept"), "{other:?}");

        let exposure = web_live_message(
            event(EventPayload::TitleExposureChanged {
                inputs: vec![input(3)],
                fields: vec![
                    TitleFieldExposure {
                        field: field(1),
                        exposed: true,
                    },
                    TitleFieldExposure {
                        field: field(2),
                        exposed: false,
                    },
                ],
            }),
            &hidden(),
        );
        let line = fm_protocol::encode_line(&exposure).unwrap();
        assert!(line.contains("\tfields=1%3A1\t"), "{line}");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one fixture per run event
    fn cue_script_and_shortcut_runs_leave_hidden_title_values_out() {
        use fm_protocol::{
            CueRunChange, CueRunStatus, CueTitleText, CueTitleValue, FadeToBlackPosition,
            FadeToBlackState, ManualTransitionStatus, OverlayStatus, PlaylistRunState,
            ScriptRunChange, ShortcutFireChange,
        };
        let titles = || {
            vec![
                CueTitleText {
                    input: input(3),
                    field_revision: 2,
                    field: field(1),
                    value: CueTitleValue::Text("Ana".into()),
                },
                CueTitleText {
                    input: input(3),
                    field_revision: 2,
                    field: field(2),
                    value: CueTitleValue::Text("producer note".into()),
                },
                // Another title's same field id is not hidden.
                CueTitleText {
                    input: input(5),
                    field_revision: 1,
                    field: field(2),
                    value: CueTitleValue::Text("kept".into()),
                },
            ]
        };
        let fade = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        let events = [
            EventPayload::CueRunChanged(Box::new(CueRunChange {
                list: 1,
                frame: 10,
                state: CueRunStatus::default(),
                fired: vec![1],
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade,
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: Vec::new(),
                streams: Vec::new(),
                recordings: Vec::new(),
                playlist: PlaylistRunState::Stopped,
                titles: titles(),
                media: Vec::new(),
                scripts: Vec::new(),
            })),
            EventPayload::ScriptRan(Box::new(ScriptRunChange {
                script: 1,
                frame: 10,
                actions: 2,
                instructions: 5,
                run: fm_protocol::ScriptRunStatus::Idle,
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade,
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: Vec::new(),
                streams: Vec::new(),
                recordings: Vec::new(),
                playlist: PlaylistRunState::Stopped,
                titles: titles(),
                media: Vec::new(),
            })),
            EventPayload::ShortcutFired(Box::new(ShortcutFireChange {
                shortcut: 1,
                frame: 10,
                actions: 2,
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade,
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: Vec::new(),
                streams: Vec::new(),
                recordings: Vec::new(),
                playlist: PlaylistRunState::Stopped,
                titles: titles(),
                media: Vec::new(),
                scripts: Vec::new(),
            })),
        ];
        for payload in events {
            let web = web_live_message(event(payload), &hidden());
            let WireMessage::Event(EventMessage { payload, .. }) = &web else {
                panic!("{web:?}")
            };
            let titles = match payload {
                EventPayload::CueRunChanged(change) => &change.titles,
                EventPayload::ScriptRan(change) => &change.titles,
                EventPayload::ShortcutFired(change) => &change.titles,
                other => panic!("{other:?}"),
            };
            assert_eq!(
                titles
                    .iter()
                    .map(|title| (title.input, title.field))
                    .collect::<Vec<_>>(),
                [(input(3), field(1)), (input(5), field(2))]
            );
            assert!(!format!("{web:?}").contains("producer"), "{web:?}");
        }
    }

    #[test]
    fn an_exposure_change_resynchronizes_the_titles_it_names() {
        let state = |id: u128, exposed: bool| TitleStateMessage {
            revision: 9,
            input: input(id),
            name: "Lower".into(),
            field_revision: 2,
            phase: TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            timeline: None,
            fields: vec![TitleFieldStatus {
                field: field(1),
                name: "Note".into(),
                value: TitleFieldValue::Text("note".into()),
                exposed,
                revision: 2,
            }],
        };
        let exposure = event(EventPayload::TitleExposureChanged {
            inputs: vec![input(3), input(4)],
            fields: Vec::new(),
        });
        let inputs = resync_inputs(&exposure);
        assert_eq!(inputs, [input(3), input(4)]);
        let resynced = web_title_states(
            vec![state(3, true), state(4, false), state(5, true)],
            &inputs,
        );
        // Each named title, as the session may see it; others not at all.
        assert_eq!(
            resynced.iter().map(|state| state.input).collect::<Vec<_>>(),
            [input(3), input(4)]
        );
        assert_eq!(resynced[0].fields.len(), 1);
        assert_eq!(resynced[1].fields.len(), 0);
        let changed = event(EventPayload::TitleChanged {
            input: input(3),
            field_revision: 2,
            fields: Vec::new(),
            overlays: Vec::new(),
        });
        assert_eq!(resync_inputs(&changed), []);
    }

    fn phases(phase: TitlePhase, phase_frame: u64) -> TitlePhasesMessage {
        TitlePhasesMessage {
            frame: 100 + phase_frame,
            titles: vec![fm_protocol::TitlePhaseStatus {
                input: input(3),
                phase,
                phase_frame,
                phase_frames: if phase == TitlePhase::Idle { 0 } else { 20 },
                timeline: None,
            }],
        }
    }

    #[test]
    fn title_phases_are_sent_on_change_and_refreshed_while_shown() {
        let mut pacer = PhasePacer::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        // The first look sends the current phases.
        assert!(
            pacer
                .poll(at(0), true, || phases(TitlePhase::Idle, 0))
                .is_some()
        );
        // Unchanged idle titles are not re-sent, however long.
        assert!(
            pacer
                .poll(at(150), true, || phases(TitlePhase::Idle, 0))
                .is_none()
        );
        assert!(
            pacer
                .poll(at(5000), true, || phases(TitlePhase::Idle, 0))
                .is_none()
        );
        // A take is sent at the next look, but not before the check interval.
        assert!(
            pacer
                .poll(at(5050), true, || phases(TitlePhase::In, 0))
                .is_none()
        );
        assert!(
            pacer
                .poll(at(5100), true, || phases(TitlePhase::In, 2))
                .is_some()
        );
        // An advancing phase frame alone waits for the refresh.
        assert!(
            pacer
                .poll(at(5300), true, || phases(TitlePhase::In, 8))
                .is_none()
        );
        assert!(
            pacer
                .poll(at(6100), true, || phases(TitlePhase::Hold, 0))
                .is_some()
        );
        assert!(
            pacer
                .poll(at(6300), true, || phases(TitlePhase::Hold, 0))
                .is_none()
        );
        assert!(
            pacer
                .poll(at(7100), true, || phases(TitlePhase::Hold, 0))
                .is_some()
        );
        // Without demand nothing is computed; demand again sends at once.
        assert!(pacer.poll(at(7200), false, || unreachable!()).is_none());
        assert!(
            pacer
                .poll(at(7210), true, || phases(TitlePhase::Hold, 0))
                .is_some()
        );
    }

    #[test]
    fn web_commands_cannot_reach_hidden_fields_or_change_exposure() {
        let command = |payload| CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: "c".into(),
            idempotency_key: "k".into(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        };
        let code = |result: Option<CommandResult>| match result {
            Some(CommandResult::Rejected { code, .. }) => Some(code),
            _ => None,
        };
        let save = |fields| {
            command(CommandPayload::SetTitleFields {
                input: input(3),
                base_revision: 1,
                require: TitleAirRequirement::OffAir,
                fields,
            })
        };
        assert_eq!(
            code(web_command_refusal(
                &save(vec![update(1, "a")]),
                &hidden(),
                4
            )),
            None
        );
        assert_eq!(
            code(web_command_refusal(
                &save(vec![update(1, "a"), update(2, "b")]),
                &hidden(),
                4
            ))
            .as_deref(),
            Some("not_found")
        );
        let take = command(CommandPayload::TakeTitle {
            input: input(3),
            channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
            base_revision: 1,
            fields: vec![update(2, "b")],
        });
        assert_eq!(
            code(web_command_refusal(&take, &hidden(), 4)).as_deref(),
            Some("not_found")
        );
        let expose = command(CommandPayload::SetTitleFieldExposure {
            input: input(3),
            field: field(2),
            exposed: true,
        });
        assert_eq!(
            code(web_command_refusal(&expose, &hidden(), 4)).as_deref(),
            Some("permission_denied")
        );
        assert_eq!(
            code(web_command_refusal(
                &command(CommandPayload::Cut),
                &hidden(),
                4
            )),
            None
        );
    }

    fn text(target: WireInputTarget, id: u128, value: &str) -> WireCueAction {
        WireCueAction::TitleText {
            input: target,
            field: field(id),
            text: value.into(),
        }
    }

    fn fixed(value: u128) -> WireInputTarget {
        WireInputTarget::Fixed(input(value))
    }

    /// A list whose cue 1 sets title 3's exposed name and hidden note, then
    /// title 5's field 2 (exposed there), and whose cue 2 sets field 2 of
    /// whichever title is on Preview when it fires, and steps a number.
    fn list() -> CueListStatus {
        CueListStatus {
            id: 7,
            spec: CueListSpec {
                name: "Show".into(),
                cues: vec![
                    fm_protocol::WireCue {
                        id: 1,
                        name: "Names".into(),
                        auto_follow: false,
                        actions: vec![
                            text(fixed(3), 1, "Ana"),
                            text(fixed(3), 2, "producer note"),
                            text(fixed(5), 2, "kept"),
                        ],
                    },
                    fm_protocol::WireCue {
                        id: 2,
                        name: "Dynamic".into(),
                        auto_follow: false,
                        actions: vec![
                            WireCueAction::Preview { input: fixed(5) },
                            text(WireInputTarget::Preview, 2, "dynamic secret"),
                            WireCueAction::TitleNumber {
                                input: fixed(3),
                                field: field(2),
                                add: true,
                                value: 424_242,
                            },
                        ],
                    },
                ],
            },
            state: fm_protocol::CueRunStatus::default(),
        }
    }

    fn shortcut() -> ShortcutStatus {
        ShortcutStatus {
            id: 9,
            spec: ShortcutSpec {
                name: "Note".into(),
                enabled: true,
                key: None,
                osc: None,
                action: WireShortcutAction::Actions(vec![
                    text(fixed(3), 1, "Bo"),
                    text(fixed(3), 2, "shortcut secret"),
                ]),
            },
        }
    }

    fn marker(target: WireInputTarget, id: u128, number: bool) -> WireCueAction {
        WireCueAction::TitleHidden {
            input: target,
            field: field(id),
            number,
        }
    }

    #[test]
    fn cue_lists_shortcuts_and_scripts_reach_a_web_session_without_hidden_values() {
        let records = [
            WireMessage::CueListState(fm_protocol::CueListStateMessage {
                revision: 4,
                frame: 0,
                list: list(),
            }),
            event(EventPayload::CueListChanged(Box::new(list()))),
            WireMessage::ShortcutState(fm_protocol::ShortcutStateMessage {
                revision: 4,
                shortcut: shortcut(),
            }),
            event(EventPayload::ShortcutChanged(Box::new(shortcut()))),
            WireMessage::ScriptState(fm_protocol::ScriptStateMessage {
                revision: 4,
                script: fm_protocol::ScriptStatus::new(
                    1,
                    fm_protocol::ScriptSpec {
                        name: "Open".into(),
                        source: "title_text 3, 2, \"script secret\"".into(),
                    },
                ),
                run: fm_protocol::ScriptRunStatus::Idle,
            }),
            event(EventPayload::ScriptChanged(Box::new(
                fm_protocol::ScriptStatus::new(
                    1,
                    fm_protocol::ScriptSpec {
                        name: "Open".into(),
                        source: "title_text 3, 2, \"script secret\"".into(),
                    },
                ),
            ))),
        ];
        for record in records {
            let web = web_message(record, &hidden());
            let line = fm_protocol::encode_line(&web).unwrap();
            for secret in [
                "producer note",
                "dynamic secret",
                "shortcut secret",
                "script secret",
            ] {
                let hex: String = secret.bytes().fold(String::new(), |mut hex, byte| {
                    use std::fmt::Write as _;
                    write!(hex, "{byte:02x}").unwrap();
                    hex
                });
                assert!(
                    !line.contains(&hex) && !line.contains(secret),
                    "{secret} leaked: {line}"
                );
            }
            assert!(!line.contains("424242"), "{line}");
            assert_eq!(fm_protocol::decode_line(&line).unwrap(), web);
            match web {
                WireMessage::CueListState(fm_protocol::CueListStateMessage { list, .. }) => {
                    assert_web_list(&list);
                }
                WireMessage::Event(EventMessage {
                    payload: EventPayload::CueListChanged(list),
                    ..
                }) => assert_web_list(&list),
                WireMessage::ShortcutState(fm_protocol::ShortcutStateMessage {
                    shortcut, ..
                }) => assert_web_shortcut(&shortcut),
                WireMessage::Event(EventMessage {
                    payload: EventPayload::ShortcutChanged(shortcut),
                    ..
                }) => assert_web_shortcut(&shortcut),
                WireMessage::ScriptState(fm_protocol::ScriptStateMessage { script, .. }) => {
                    assert!(script.source_hidden && script.spec.source.is_empty());
                    assert_eq!(script.spec.name, "Open");
                    assert!(line.contains("\tsource=hidden"), "{line}");
                }
                WireMessage::Event(EventMessage {
                    payload: EventPayload::ScriptChanged(script),
                    ..
                }) => {
                    assert!(script.source_hidden && script.spec.source.is_empty());
                    assert!(line.contains("\tsource=hidden"), "{line}");
                }
                other => panic!("{other:?}"),
            }
        }
        // Without hidden fields only the scripts' sources are withheld.
        let mut open = list();
        web_cue_list(&mut open, &HiddenFields::default());
        assert_eq!(open, list());
    }

    fn assert_web_list(list: &CueListStatus) {
        assert_eq!(
            list.spec.cues[0].actions,
            [
                text(fixed(3), 1, "Ana"),
                marker(fixed(3), 2, false),
                text(fixed(5), 2, "kept"),
            ]
        );
        // A role target may reach title 3, whose field 2 is hidden.
        assert_eq!(
            list.spec.cues[1].actions,
            [
                WireCueAction::Preview { input: fixed(5) },
                marker(WireInputTarget::Preview, 2, false),
                marker(fixed(3), 2, true),
            ]
        );
    }

    fn assert_web_shortcut(shortcut: &ShortcutStatus) {
        assert_eq!(
            shortcut.spec.action,
            WireShortcutAction::Actions(vec![text(fixed(3), 1, "Bo"), marker(fixed(3), 2, false)])
        );
    }

    fn edit(payload: CommandPayload) -> CommandMessage {
        CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: "edit".into(),
            idempotency_key: "edit".into(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }
    }

    fn stored() -> StoredDefinitions {
        StoredDefinitions {
            cue_lists: vec![list()],
            shortcuts: vec![shortcut()],
        }
    }

    fn set_list(spec: CueListSpec) -> CommandMessage {
        edit(CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(spec),
            position: None,
        })
    }

    fn refusal(command: &mut CommandMessage) -> Option<(String, String)> {
        match web_definition_edit(command, &hidden(), &stored(), 4) {
            Some(CommandResult::Rejected { code, message, .. }) => Some((code, message)),
            Some(other) => panic!("{other:?}"),
            None => None,
        }
    }

    #[test]
    fn a_web_edit_keeps_the_hidden_values_its_markers_stand_for() {
        // The list as the page has it, renamed and with a new exposed text.
        let mut web = list();
        web_cue_list(&mut web, &hidden());
        web.spec.name = "Renamed".into();
        web.spec.cues[0].actions[0] = text(fixed(3), 1, "Cy");
        let mut command = set_list(web.spec);
        assert_eq!(refusal(&mut command), None);
        let CommandPayload::SetCueList { spec, .. } = &command.payload else {
            panic!("{command:?}")
        };
        let mut expected = list().spec;
        expected.name = "Renamed".into();
        expected.cues[0].actions[0] = text(fixed(3), 1, "Cy");
        assert_eq!(**spec, expected);

        let mut web = shortcut();
        web_shortcut(&mut web, &hidden());
        web.spec.enabled = false;
        let mut command = edit(CommandPayload::SetShortcut {
            shortcut: 9,
            spec: Box::new(web.spec),
            position: None,
        });
        assert_eq!(refusal(&mut command), None);
        let CommandPayload::SetShortcut { spec, .. } = &command.payload else {
            panic!("{command:?}")
        };
        assert_eq!(spec.action, shortcut().spec.action);
        assert!(!spec.enabled);
    }

    #[test]
    fn a_web_edit_may_not_set_a_hidden_field_nor_place_a_marker_freely() {
        let code = |command: &mut CommandMessage| refusal(command).map(|(code, _)| code);
        let page = || {
            let mut web = list();
            web_cue_list(&mut web, &hidden());
            web.spec
        };
        // A value of its own for the hidden note, even the stored one.
        for value in ["guess", "producer note"] {
            let mut spec = page();
            spec.cues[0].actions[1] = text(fixed(3), 2, value);
            let refused = refusal(&mut set_list(spec)).unwrap();
            assert_eq!(refused.0, "permission_denied");
            assert!(refused.1.contains("cue 1 action 2"), "{}", refused.1);
            assert!(!refused.1.contains("producer"), "{}", refused.1);
        }
        // A role target that may reach the hidden field.
        let mut spec = page();
        spec.cues[1].actions[1] = text(WireInputTarget::Selected, 2, "x");
        assert_eq!(
            code(&mut set_list(spec)).as_deref(),
            Some("permission_denied")
        );
        // A marker moved to another place, title, field, or kind, or in a
        // new cue or list, stands for nothing.
        let moved: [fn(&mut CueListSpec); 5] = [
            |spec| spec.cues[0].actions.swap(1, 2),
            |spec| spec.cues[0].actions[1] = marker(fixed(5), 2, false),
            |spec| spec.cues[0].actions[1] = marker(fixed(3), 1, false),
            |spec| spec.cues[0].actions[1] = marker(fixed(3), 2, true),
            |spec| spec.cues[0].id = 3,
        ];
        for change in moved {
            let mut spec = page();
            change(&mut spec);
            assert_eq!(
                code(&mut set_list(spec)).as_deref(),
                Some("invalid_command")
            );
        }
        let mut command = edit(CommandPayload::SetCueList {
            list: 8,
            spec: Box::new(page()),
            position: None,
        });
        assert_eq!(code(&mut command).as_deref(), Some("invalid_command"));
        // Exposed fields stay editable, and scripts are not.
        let mut spec = page();
        spec.cues[0].actions[2] = text(fixed(5), 2, "new");
        assert_eq!(code(&mut set_list(spec)), None);
        let mut script = edit(CommandPayload::SetScript {
            script: 1,
            spec: Box::new(fm_protocol::ScriptSpec {
                name: "Open".into(),
                source: "cut 2".into(),
            }),
            position: None,
        });
        assert_eq!(code(&mut script).as_deref(), Some("permission_denied"));
    }
}
