//! The typed refusal reason of a rejected `command_result` (Protocol 2.45).
//!
//! `reason` is optional; when present it is one colon-separated token list,
//! the kind first and then its typed parameters, every number a canonical
//! unsigned decimal (slots and channels nonzero):
//!
//! * `permission_denied`, `deadline_exceeded`, `duplicate_name`,
//!   `project_locked`, `nothing_to_undo`, `nothing_to_redo`,
//!   `history_conflict`, `history_drift`, `not_durable`, `rate_limited`,
//!   `runtime_unsupported`;
//! * `revision_conflict:<current revision>`;
//! * `unknown_id:<kind>:<id>` and `duplicate_id:<kind>:<id>`, `<kind>` one
//!   of `input`, `output`, `scene`, `layer`, `category`, `grade`, `lut`,
//!   `preset`, `title`, `playlist_entry`;
//! * `invalid_name:<max bytes>`;
//! * `input_locked:<input>`;
//! * `input_in_use:<input>:<usage>`, `<usage>` one of `program`, `preview`,
//!   `transition`, `manual_transition`, `scene_on_air`, `overlay:<channel>`,
//!   `overlay_queue:<channel>`, `stinger:<slot>`, `scene_layer:<scene>`,
//!   `scene_audio_source:<scene input>`, `alias_target:<alias input>`,
//!   `playlist_entry:<entry>`;
//! * `display_in_use:<slot>`;
//! * `bounds_exceeded:<field>:<max>`, `<field>` a `[a-z0-9_]` token of at
//!   most [`fm_command::MAX_REFUSAL_FIELD_BYTES`] bytes;
//! * `stinger_realizing:<slot>`;
//! * `playlist_take_scheduled:<frame>`.

use fm_command::{IdKind, InputUse, RefusalReason};

use crate::CodecError;

use super::value::parse_canonical_u64;

const fn kind_token(kind: IdKind) -> &'static str {
    match kind {
        IdKind::Input => "input",
        IdKind::Output => "output",
        IdKind::Scene => "scene",
        IdKind::Layer => "layer",
        IdKind::Category => "category",
        IdKind::Grade => "grade",
        IdKind::Lut => "lut",
        IdKind::Preset => "preset",
        IdKind::Title => "title",
        IdKind::PlaylistEntry => "playlist_entry",
    }
}

fn parse_kind(token: &str) -> Option<IdKind> {
    IdKind::ALL
        .into_iter()
        .find(|kind| kind_token(*kind) == token)
}

fn usage_token(usage: InputUse) -> String {
    match usage {
        InputUse::Program => "program".to_owned(),
        InputUse::Preview => "preview".to_owned(),
        InputUse::Transition => "transition".to_owned(),
        InputUse::ManualTransition => "manual_transition".to_owned(),
        InputUse::SceneOnAir => "scene_on_air".to_owned(),
        InputUse::Overlay { channel } => format!("overlay:{channel}"),
        InputUse::OverlayQueue { channel } => format!("overlay_queue:{channel}"),
        InputUse::Stinger { slot } => format!("stinger:{slot}"),
        InputUse::SceneLayer { scene } => format!("scene_layer:{scene}"),
        InputUse::SceneAudioSource { scene_input } => format!("scene_audio_source:{scene_input}"),
        InputUse::AliasTarget { alias } => format!("alias_target:{alias}"),
        InputUse::PlaylistEntry { entry } => format!("playlist_entry:{entry}"),
    }
}

/// Encodes a refusal reason as its `reason` field value.
///
/// # Errors
///
/// Refuses a zero slot or channel and a malformed bounds field token.
pub fn encode_refusal_reason(reason: &RefusalReason) -> Result<String, CodecError> {
    let invalid = || CodecError::InvalidField {
        field: "reason",
        value: format!("{reason:?}"),
    };
    let nonzero = |value: u8| {
        if value == 0 {
            Err(invalid())
        } else {
            Ok(value)
        }
    };
    let parameters = match reason {
        RefusalReason::PermissionDenied
        | RefusalReason::DeadlineExceeded
        | RefusalReason::DuplicateName
        | RefusalReason::ProjectLocked
        | RefusalReason::NothingToUndo
        | RefusalReason::NothingToRedo
        | RefusalReason::HistoryConflict
        | RefusalReason::HistoryDrift
        | RefusalReason::NotDurable
        | RefusalReason::RateLimited
        | RefusalReason::RuntimeUnsupported => None,
        RefusalReason::RevisionConflict { current } => Some(current.to_string()),
        RefusalReason::UnknownId { kind, id } | RefusalReason::DuplicateId { kind, id } => {
            Some(format!("{}:{id}", kind_token(*kind)))
        }
        RefusalReason::InvalidName { max_bytes } => Some(max_bytes.to_string()),
        RefusalReason::InputLocked { input } => Some(input.to_string()),
        RefusalReason::InputInUse { input, usage } => {
            if let InputUse::Overlay { channel }
            | InputUse::OverlayQueue { channel }
            | InputUse::Stinger { slot: channel } = usage
            {
                nonzero(*channel)?;
            }
            Some(format!("{input}:{}", usage_token(*usage)))
        }
        RefusalReason::DisplayInUse { slot } | RefusalReason::StingerRealizing { slot } => {
            Some(nonzero(*slot)?.to_string())
        }
        RefusalReason::BoundsExceeded { field, max } => {
            if !RefusalReason::field_is_valid(field) {
                return Err(invalid());
            }
            Some(format!("{field}:{max}"))
        }
        RefusalReason::PlaylistTakeScheduled { frame } => Some(frame.to_string()),
    };
    Ok(match parameters {
        Some(parameters) => format!("{}:{parameters}", reason.kind()),
        None => reason.kind().to_owned(),
    })
}

fn parse_u128(value: &str) -> Option<u128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn parse_nonzero_u8(value: &str) -> Option<u8> {
    parse_canonical_u64(value)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value != 0)
}

fn parse_usage(parts: &[&str]) -> Option<InputUse> {
    Some(match parts {
        ["program"] => InputUse::Program,
        ["preview"] => InputUse::Preview,
        ["transition"] => InputUse::Transition,
        ["manual_transition"] => InputUse::ManualTransition,
        ["scene_on_air"] => InputUse::SceneOnAir,
        ["overlay", channel] => InputUse::Overlay {
            channel: parse_nonzero_u8(channel)?,
        },
        ["overlay_queue", channel] => InputUse::OverlayQueue {
            channel: parse_nonzero_u8(channel)?,
        },
        ["stinger", slot] => InputUse::Stinger {
            slot: parse_nonzero_u8(slot)?,
        },
        ["scene_layer", scene] => InputUse::SceneLayer {
            scene: parse_u128(scene)?,
        },
        ["scene_audio_source", input] => InputUse::SceneAudioSource {
            scene_input: parse_u128(input)?,
        },
        ["alias_target", alias] => InputUse::AliasTarget {
            alias: parse_u128(alias)?,
        },
        ["playlist_entry", entry] => InputUse::PlaylistEntry {
            entry: parse_canonical_u64(entry)?,
        },
        _ => return None,
    })
}

/// Decodes a `reason` field value, the exact inverse of
/// [`encode_refusal_reason`].
///
/// # Errors
///
/// Refuses an unknown kind, a missing, extra, or non-canonical parameter.
pub fn decode_refusal_reason(value: &str) -> Result<RefusalReason, CodecError> {
    let parts = value.split(':').take(5).collect::<Vec<_>>();
    let reason =
        match parts.as_slice() {
            ["permission_denied"] => Some(RefusalReason::PermissionDenied),
            ["deadline_exceeded"] => Some(RefusalReason::DeadlineExceeded),
            ["duplicate_name"] => Some(RefusalReason::DuplicateName),
            ["project_locked"] => Some(RefusalReason::ProjectLocked),
            ["nothing_to_undo"] => Some(RefusalReason::NothingToUndo),
            ["nothing_to_redo"] => Some(RefusalReason::NothingToRedo),
            ["history_conflict"] => Some(RefusalReason::HistoryConflict),
            ["history_drift"] => Some(RefusalReason::HistoryDrift),
            ["not_durable"] => Some(RefusalReason::NotDurable),
            ["rate_limited"] => Some(RefusalReason::RateLimited),
            ["runtime_unsupported"] => Some(RefusalReason::RuntimeUnsupported),
            ["revision_conflict", current] => parse_canonical_u64(current)
                .map(|current| RefusalReason::RevisionConflict { current }),
            ["unknown_id", kind, id] => parse_kind(kind)
                .zip(parse_u128(id))
                .map(|(kind, id)| RefusalReason::UnknownId { kind, id }),
            ["duplicate_id", kind, id] => parse_kind(kind)
                .zip(parse_u128(id))
                .map(|(kind, id)| RefusalReason::DuplicateId { kind, id }),
            ["invalid_name", max] => parse_canonical_u64(max)
                .and_then(|max| u32::try_from(max).ok())
                .map(|max_bytes| RefusalReason::InvalidName { max_bytes }),
            ["input_locked", input] => {
                parse_u128(input).map(|input| RefusalReason::InputLocked { input })
            }
            ["input_in_use", input, usage @ ..] => parse_u128(input)
                .zip(parse_usage(usage))
                .map(|(input, usage)| RefusalReason::InputInUse { input, usage }),
            ["display_in_use", slot] => {
                parse_nonzero_u8(slot).map(|slot| RefusalReason::DisplayInUse { slot })
            }
            ["bounds_exceeded", field, max] if RefusalReason::field_is_valid(field) => {
                parse_canonical_u64(max).map(|max| RefusalReason::BoundsExceeded {
                    field: (*field).to_owned(),
                    max,
                })
            }
            ["stinger_realizing", slot] => {
                parse_nonzero_u8(slot).map(|slot| RefusalReason::StingerRealizing { slot })
            }
            ["playlist_take_scheduled", frame] => parse_canonical_u64(frame)
                .map(|frame| RefusalReason::PlaylistTakeScheduled { frame }),
            _ => None,
        };
    reason.ok_or_else(|| CodecError::InvalidField {
        field: "reason",
        value: value.to_owned(),
    })
}
