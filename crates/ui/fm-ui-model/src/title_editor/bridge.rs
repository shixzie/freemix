//! The title editor on the live wire protocol.
//!
//! [`TitleProtocolBridge`] turns editor [`TitleCommand`]s into
//! [`CommandPayload`]s and maps [`CommandResult`]s back to [`TitleReply`]s.
//! A command result arrives before the events it caused, so the bridge holds
//! every outcome until the replicated [`ClientModel`] has applied the
//! revision it names (the accepted revision, or the server's current
//! revision for a refusal). The reply then carries the title exactly as the
//! replicated state shows it: an applied `Take` is on air, and a conflict
//! shows the other editor's values.

use core::fmt;
use std::collections::BTreeMap;

use fm_protocol::{
    CommandPayload, CommandResult, FieldIssue, TitleAirRequirement, TitleFieldUpdate,
    TitleFieldValue, WireInputId, WireOverlayChannelId, WireTitleFieldId,
};
use fm_types::InputId;

use super::{TitleCommand, TitleCommandBody, TitleRejection, TitleReply, TitleSnapshot};
use crate::ClientModel;

/// A `Take` names a channel number the protocol has no overlay channel for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTakeChannel(pub u8);

impl fmt::Display for InvalidTakeChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "overlay channel {} does not exist", self.0)
    }
}

impl std::error::Error for InvalidTakeChannel {}

/// Encodes an editor command as its protocol payload: `Save` is
/// `title_fields require=off_air`, `Update` is `title_fields require=on_air`,
/// `Take` is `title_take` on its channel, and `OffAir` is `title_off_air`.
///
/// # Errors
///
/// A `Take` whose channel is not an overlay channel.
pub fn title_payload(command: &TitleCommand) -> Result<CommandPayload, InvalidTakeChannel> {
    let input = WireInputId::from_domain(command.title);
    Ok(match &command.body {
        TitleCommandBody::Save {
            base_revision,
            fields,
        } => CommandPayload::SetTitleFields {
            input,
            base_revision: *base_revision,
            require: TitleAirRequirement::OffAir,
            fields: wire_fields(fields),
        },
        TitleCommandBody::Update {
            base_revision,
            fields,
        } => CommandPayload::SetTitleFields {
            input,
            base_revision: *base_revision,
            require: TitleAirRequirement::OnAir,
            fields: wire_fields(fields),
        },
        TitleCommandBody::Take {
            channel,
            base_revision,
            fields,
        } => CommandPayload::TakeTitle {
            input,
            channel: WireOverlayChannelId::new(*channel).ok_or(InvalidTakeChannel(*channel))?,
            base_revision: *base_revision,
            fields: wire_fields(fields),
        },
        TitleCommandBody::OffAir => CommandPayload::TitleOffAir { input },
    })
}

fn wire_fields(fields: &[(WireTitleFieldId, TitleFieldValue)]) -> Vec<TitleFieldUpdate> {
    fields
        .iter()
        .map(|(field, value)| TitleFieldUpdate {
            field: *field,
            value: value.clone(),
        })
        .collect()
}

/// How a refused title command reads to the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireTitleOutcome {
    /// `title_revision_conflict`: the edit's base revision is stale.
    Conflict,
    Rejected(TitleRejection),
}

/// Classifies a refused title command by its result `code` and the stable
/// code of its first field issue.
#[must_use]
pub fn classify_rejection(code: &str, fields: &[FieldIssue]) -> WireTitleOutcome {
    if code == "permission_denied" {
        return WireTitleOutcome::Rejected(TitleRejection::NotPermitted);
    }
    let reason = match fields.first().map(|issue| issue.code.as_str()) {
        Some("title_revision_conflict") => return WireTitleOutcome::Conflict,
        Some("unknown_title") => TitleRejection::UnknownTitle,
        Some(
            "unknown_field" | "duplicate_field" | "field_type_mismatch" | "invalid_field_value",
        ) => TitleRejection::InvalidFields,
        Some("title_content_too_large") => TitleRejection::ContentTooLarge,
        Some("title_on_air") => TitleRejection::AlreadyOnAir,
        Some("title_off_air") => TitleRejection::NotOnAir,
        Some("title_on_program") => TitleRejection::OnProgram,
        Some("title_on_multiple_overlays") => TitleRejection::OnMultipleOverlays,
        Some("title_revision_exhausted") => TitleRejection::RevisionExhausted,
        Some("project_locked" | "input_locked") => TitleRejection::Locked,
        _ => TitleRejection::Other,
    };
    WireTitleOutcome::Rejected(reason)
}

/// What [`TitleProtocolBridge::apply_result`] did with a command result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeIntake {
    /// The result answers no tracked title command.
    NotTitle,
    /// The reply waits for the replicated state to reach its revision;
    /// [`TitleProtocolBridge::poll`] releases it.
    Held,
    Reply(TitleReply),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    Applied,
    Conflict,
    Rejected(TitleRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Sent {
    request: u64,
    title: InputId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Held {
    request: u64,
    title: InputId,
    revision: u64,
    verdict: Verdict,
}

/// Correlates one editor's title commands with protocol command ids and
/// results.
///
/// The live path is:
///
/// 1. `editor.request(..)` builds a [`TitleCommand`];
/// 2. [`title_payload`] encodes it, the transport queues it under a command
///    id, and [`Self::track`] records that id;
/// 3. each [`CommandResult`] goes to [`Self::apply_result`], and after every
///    applied snapshot, `title_state`, or event [`Self::poll`] releases held
///    replies; each released reply goes to `editor.apply_reply`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TitleProtocolBridge {
    sent: BTreeMap<String, Sent>,
    held: Vec<Held>,
}

impl TitleProtocolBridge {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `command` was queued under protocol `command_id`.
    pub fn track(&mut self, command_id: impl Into<String>, command: &TitleCommand) {
        self.sent.insert(
            command_id.into(),
            Sent {
                request: command.request,
                title: command.title,
            },
        );
    }

    /// Maps a command result. Results for untracked ids are
    /// [`BridgeIntake::NotTitle`]; a title result is released at once when
    /// `model` already shows its revision and held otherwise.
    pub fn apply_result(&mut self, result: &CommandResult, model: &ClientModel) -> BridgeIntake {
        let (id, held) = match result {
            CommandResult::Accepted { id, revision, .. } => (id, (*revision, Verdict::Applied)),
            CommandResult::Rejected {
                id,
                code,
                fields,
                current_revision,
                ..
            } => (
                id,
                (
                    *current_revision,
                    match classify_rejection(code, fields) {
                        WireTitleOutcome::Conflict => Verdict::Conflict,
                        WireTitleOutcome::Rejected(reason) => Verdict::Rejected(reason),
                    },
                ),
            ),
        };
        let Some(sent) = self.sent.remove(id) else {
            return BridgeIntake::NotTitle;
        };
        let (revision, verdict) = held;
        let held = Held {
            request: sent.request,
            title: sent.title,
            revision,
            verdict,
        };
        if let Some(reply) = release(held, model) {
            BridgeIntake::Reply(reply)
        } else {
            self.held.push(held);
            BridgeIntake::Held
        }
    }

    /// Releases every held reply whose revision `model` has applied, in the
    /// order their results arrived.
    pub fn poll(&mut self, model: &ClientModel) -> Vec<TitleReply> {
        let mut released = Vec::new();
        self.held.retain(|held| match release(*held, model) {
            Some(reply) => {
                released.push(reply);
                false
            }
            None => true,
        });
        released
    }

    /// Forgets a command whose result will never arrive and returns its
    /// editor request, to pass to `TitleEditor::abandon`.
    pub fn abandon(&mut self, command_id: &str) -> Option<u64> {
        self.sent.remove(command_id).map(|sent| sent.request)
    }

    /// Commands sent or held without a released reply.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.sent.len() + self.held.len()
    }
}

/// The reply for `held` once `model` has applied its revision and holds the
/// title's values.
fn release(held: Held, model: &ClientModel) -> Option<TitleReply> {
    let state = model.state()?;
    let applied = model.reconnect_cursor()?.revision.get();
    if applied < held.revision {
        return None;
    }
    let Held {
        request,
        title,
        verdict,
        ..
    } = held;
    let Some(status) = state.title(title) else {
        let reason = match verdict {
            Verdict::Rejected(reason) => reason,
            Verdict::Applied | Verdict::Conflict => TitleRejection::UnknownTitle,
        };
        return Some(TitleReply::Rejected {
            request,
            reason,
            current: None,
        });
    };
    let snapshot = TitleSnapshot::from_status(status, state.title_on_air(title))?;
    Some(match verdict {
        Verdict::Applied => TitleReply::Applied { request, snapshot },
        Verdict::Conflict => TitleReply::Conflict {
            request,
            current: snapshot,
        },
        Verdict::Rejected(reason) => TitleReply::Rejected {
            request,
            reason,
            current: Some(snapshot),
        },
    })
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_types::ProjectId;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn issue(code: &str) -> Vec<FieldIssue> {
        vec![FieldIssue {
            field: "input".into(),
            code: code.into(),
            message: String::new(),
        }]
    }

    fn command(body: TitleCommandBody) -> TitleCommand {
        TitleCommand {
            request: 1,
            title: input(3),
            body,
        }
    }

    #[test]
    fn operations_encode_as_their_protocol_commands() {
        let fields = vec![(field(1), TitleFieldValue::Number(4))];
        let wire = vec![TitleFieldUpdate {
            field: field(1),
            value: TitleFieldValue::Number(4),
        }];
        let input = WireInputId::from_domain(input(3));
        assert_eq!(
            title_payload(&command(TitleCommandBody::Save {
                base_revision: 2,
                fields: fields.clone(),
            })),
            Ok(CommandPayload::SetTitleFields {
                input,
                base_revision: 2,
                require: TitleAirRequirement::OffAir,
                fields: wire.clone(),
            })
        );
        assert_eq!(
            title_payload(&command(TitleCommandBody::Update {
                base_revision: 2,
                fields: fields.clone(),
            })),
            Ok(CommandPayload::SetTitleFields {
                input,
                base_revision: 2,
                require: TitleAirRequirement::OnAir,
                fields: wire.clone(),
            })
        );
        assert_eq!(
            title_payload(&command(TitleCommandBody::Take {
                channel: 3,
                base_revision: 2,
                fields: fields.clone(),
            })),
            Ok(CommandPayload::TakeTitle {
                input,
                channel: WireOverlayChannelId::new(3).unwrap(),
                base_revision: 2,
                fields: wire,
            })
        );
        assert_eq!(
            title_payload(&command(TitleCommandBody::Take {
                channel: 0,
                base_revision: 2,
                fields,
            })),
            Err(InvalidTakeChannel(0))
        );
        assert_eq!(
            title_payload(&command(TitleCommandBody::OffAir)),
            Ok(CommandPayload::TitleOffAir { input })
        );
    }

    #[test]
    fn refusal_codes_classify_into_editor_outcomes() {
        let rejected = |code: &str| classify_rejection("conflict", &issue(code));
        assert_eq!(
            rejected("title_revision_conflict"),
            WireTitleOutcome::Conflict
        );
        for (code, reason) in [
            ("unknown_title", TitleRejection::UnknownTitle),
            ("field_type_mismatch", TitleRejection::InvalidFields),
            ("title_content_too_large", TitleRejection::ContentTooLarge),
            ("title_on_air", TitleRejection::AlreadyOnAir),
            ("title_off_air", TitleRejection::NotOnAir),
            ("title_on_program", TitleRejection::OnProgram),
            (
                "title_on_multiple_overlays",
                TitleRejection::OnMultipleOverlays,
            ),
            (
                "title_revision_exhausted",
                TitleRejection::RevisionExhausted,
            ),
            ("project_locked", TitleRejection::Locked),
            ("something_new", TitleRejection::Other),
        ] {
            assert_eq!(rejected(code), WireTitleOutcome::Rejected(reason), "{code}");
        }
        assert_eq!(
            classify_rejection("permission_denied", &[]),
            WireTitleOutcome::Rejected(TitleRejection::NotPermitted)
        );
        assert_eq!(
            classify_rejection("deadline_exceeded", &[]),
            WireTitleOutcome::Rejected(TitleRejection::Other)
        );
    }

    #[test]
    fn untracked_results_pass_and_results_hold_without_state() {
        let model = ClientModel::new(ProjectId::new(NonZeroU128::new(1).unwrap()));
        let mut bridge = TitleProtocolBridge::new();
        let accepted = CommandResult::Accepted {
            id: "c1".into(),
            revision: 4,
            scheduled_frame: None,
        };
        assert_eq!(
            bridge.apply_result(&accepted, &model),
            BridgeIntake::NotTitle
        );
        bridge.track("c1", &command(TitleCommandBody::OffAir));
        assert_eq!(bridge.outstanding(), 1);
        assert_eq!(bridge.apply_result(&accepted, &model), BridgeIntake::Held);
        assert_eq!(bridge.poll(&model), Vec::new());
        assert_eq!(bridge.outstanding(), 1);

        bridge.track("c2", &command(TitleCommandBody::OffAir));
        assert_eq!(bridge.abandon("c2"), Some(1));
        assert_eq!(bridge.abandon("c2"), None);
    }
}
