//! What a Web session may see and change of a title (GX-006, Protocol 2.90).
//!
//! A title template marks each field exposed to the browser title editor or
//! hidden from it. A Web session (anything the gateway relays) is never sent
//! a hidden field at all: its `title_state` records leave hidden fields out
//! (name, value, and id), its `title_changed` events carry only the exposed
//! values, and its `title_exposure_changed` events name only the exposed
//! fields. A Web session may not name a hidden field in `title_fields` or
//! `title_take` (refused as if the field did not exist) and may not change
//! exposure itself; Studio does that.
//!
//! [`PhasePacer`] decides when the lossy `title_phases` record is sent to
//! the sessions that take it (Web pages and raw Studio sessions).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use fm_protocol::{
    CommandMessage, CommandPayload, CommandResult, EventPayload, TitlePhase, TitlePhasesMessage,
    TitleStateMessage, TitleTimelineStatus, WireInputId, WireMessage, WireTitleFieldId,
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
}

/// A title's state as a Web session sees it: hidden fields left out.
pub(crate) fn web_title_state(mut state: TitleStateMessage) -> TitleStateMessage {
    state.fields.retain(|status| status.exposed);
    state
}

/// A live record as a Web session sees it: title values and exposure
/// without hidden fields. Everything else passes unchanged.
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
        _ => {}
    }
    WireMessage::Event(event)
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
                },
                TitleFieldStatus {
                    field: field(2),
                    name: "Secret".into(),
                    value: TitleFieldValue::Text("producer note".into()),
                    exposed: false,
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
}
