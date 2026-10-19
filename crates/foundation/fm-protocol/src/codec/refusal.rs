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
//!   `preset`, `title`, `playlist_entry`, `cue_list`, `cue`;
//! * `invalid_name:<max bytes>`;
//! * `input_locked:<input>`;
//! * `input_in_use:<input>:<usage>`, `<usage>` one of `program`, `preview`,
//!   `transition`, `manual_transition`, `scene_on_air`, `overlay:<channel>`,
//!   `overlay_queue:<channel>`, `stinger:<slot>`, `scene_layer:<scene>`,
//!   `scene_audio_source:<scene input>`, `alias_target:<alias input>`,
//!   `playlist_entry:<entry>`, `cue_action:<list>:<cue>`;
//! * `display_in_use:<slot>`;
//! * `bounds_exceeded:<field>:<max>`, `<field>` a `[a-z0-9_]` token of at
//!   most [`fm_command::MAX_REFUSAL_FIELD_BYTES`] bytes;
//! * `stinger_realizing:<slot>`;
//! * `playlist_take_scheduled:<frame>`;
//! * `cue_scheduled:<frame>`, `no_cue_on_standby`, `cue_running:<cue>`
//!   (Protocol 2.50);
//! * `script_failed:<failure>:<line>:<column>` (Protocol 2.58);
//! * `script_running:<script>` (Protocol 2.62).
//!
//! The fault a deferred script part or cue action keeps (Protocol 2.63)
//! carries the same token, in lowercase hex inside its run or cue state
//! (see [`encode_fault_detail`]).

use fm_command::{FaultDetail, RefusalReason};

use crate::CodecError;

/// Encodes a refusal reason as its `reason` field value (its
/// [`RefusalReason::token`]).
///
/// # Errors
///
/// Refuses a zero slot or channel and a malformed bounds field token.
pub fn encode_refusal_reason(reason: &RefusalReason) -> Result<String, CodecError> {
    reason.token().ok_or_else(|| CodecError::InvalidField {
        field: "reason",
        value: format!("{reason:?}"),
    })
}

/// Decodes a `reason` field value, the exact inverse of
/// [`encode_refusal_reason`].
///
/// # Errors
///
/// Refuses an unknown kind, a missing, extra, or non-canonical parameter.
pub fn decode_refusal_reason(value: &str) -> Result<RefusalReason, CodecError> {
    RefusalReason::from_token(value).ok_or_else(|| CodecError::InvalidField {
        field: "reason",
        value: value.to_owned(),
    })
}

/// Encodes the detail of a deferred fault (Protocol 2.63) as two tokens
/// joined by `separator`: the reason's token in lowercase hex (`-` for
/// none), then the message in lowercase hex.
///
/// # Errors
///
/// Refuses an invalid detail (see [`FaultDetail::is_valid`]), naming
/// `field`.
pub(super) fn encode_fault_detail(
    field: &'static str,
    detail: &FaultDetail,
    separator: char,
) -> Result<String, CodecError> {
    if !detail.is_valid() {
        return Err(CodecError::InvalidField {
            field,
            value: format!("{detail:?}"),
        });
    }
    let reason = detail.reason.as_ref().and_then(RefusalReason::token);
    Ok(format!(
        "{}{separator}{}",
        reason.map_or_else(|| "-".to_owned(), |reason| super::cue::hex(&reason)),
        super::cue::hex(&detail.message)
    ))
}

/// Decodes the two tokens of [`encode_fault_detail`].
///
/// # Errors
///
/// Refuses non-canonical hex, an unknown or malformed reason, and an
/// invalid message (see [`FaultDetail::message_is_valid`]), naming `field`.
pub(super) fn decode_fault_detail(
    field: &'static str,
    reason: &str,
    message: &str,
) -> Result<FaultDetail, CodecError> {
    let invalid = || CodecError::InvalidField {
        field,
        value: format!("{reason}:{message}"),
    };
    let reason = match reason {
        "-" => None,
        reason => Some(
            RefusalReason::from_token(&super::cue::unhex(field, reason)?).ok_or_else(invalid)?,
        ),
    };
    // Longer than the bound in hex never decodes to a valid message.
    if message.len() > 2 * fm_command::MAX_FAULT_MESSAGE_BYTES {
        return Err(invalid());
    }
    let message = super::cue::unhex(field, message)?;
    if !FaultDetail::message_is_valid(&message) {
        return Err(invalid());
    }
    Ok(FaultDetail { reason, message })
}
