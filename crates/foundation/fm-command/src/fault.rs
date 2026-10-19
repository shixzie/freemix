//! Why a deferred part of automation failed (Protocol 2.63, schema 57).
//!
//! A script run's part after a `wait`, or a cue's actions after a wait,
//! run on a later frame with no command to refuse: the run or cue ends and
//! keeps a fault. [`FaultDetail`] is what that fault says about the refusal
//! that ended it, exactly what a refused command would have said: the
//! refused action's typed [`RefusalReason`] (when it has one) and its
//! English message, made one bounded line.

use crate::{RefusalReason, Rejection};

/// Longest [`FaultDetail::message`], in UTF-8 bytes.
pub const MAX_FAULT_MESSAGE_BYTES: usize = 256;

/// The refusal that ended a deferred run or cue.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FaultDetail {
    /// The refused action's typed reason, when the refusal carried one.
    pub reason: Option<RefusalReason>,
    /// The English refusal text: one line of at most
    /// [`MAX_FAULT_MESSAGE_BYTES`], never blank, without control characters
    /// or surrounding whitespace (see [`FaultDetail::message_is_valid`]).
    pub message: String,
}

impl FaultDetail {
    /// The detail of `reason` with `message` made valid (see
    /// [`bound_fault_message`]); a message that bounds to nothing falls back
    /// to the reason's English text, or to a generic one.
    #[must_use]
    pub fn new(reason: Option<RefusalReason>, message: &str) -> Self {
        let mut bounded = bound_fault_message(message);
        if bounded.is_empty() {
            bounded = bound_fault_message(
                &reason
                    .as_ref()
                    .map_or_else(|| "the action was refused".to_owned(), ToString::to_string),
            );
        }
        Self {
            reason,
            message: bounded,
        }
    }

    /// The detail of a refusal: its typed reason and its message.
    #[must_use]
    pub fn of(rejection: &Rejection) -> Self {
        Self::new(rejection.reason.clone(), &rejection.message)
    }

    /// Whether `message` is a valid fault message: not empty, at most
    /// [`MAX_FAULT_MESSAGE_BYTES`], no control characters, and no leading
    /// or trailing whitespace.
    #[must_use]
    pub fn message_is_valid(message: &str) -> bool {
        !message.is_empty()
            && message.len() <= MAX_FAULT_MESSAGE_BYTES
            && message.trim() == message
            && !message.chars().any(char::is_control)
    }

    /// Whether the detail is valid: its message is (see
    /// [`Self::message_is_valid`]) and its reason has a token (see
    /// [`RefusalReason::token`]).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        Self::message_is_valid(&self.message)
            && self
                .reason
                .as_ref()
                .is_none_or(|reason| reason.token().is_some())
    }
}

/// `text` made one line of at most [`MAX_FAULT_MESSAGE_BYTES`]: control
/// characters (newlines included) become spaces, surrounding whitespace is
/// trimmed, and a longer text is cut at a character boundary.
#[must_use]
pub fn bound_fault_message(text: &str) -> String {
    let mut bounded: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if bounded.len() > MAX_FAULT_MESSAGE_BYTES {
        let mut end = MAX_FAULT_MESSAGE_BYTES;
        while !bounded.is_char_boundary(end) {
            end -= 1;
        }
        bounded.truncate(end);
        bounded.truncate(bounded.trim_end().len());
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RejectionCode;

    #[test]
    fn messages_are_bounded_single_lines() {
        assert_eq!(bound_fault_message("  a\nb\tc\u{7}  "), "a b c");
        let long = "é".repeat(MAX_FAULT_MESSAGE_BYTES);
        let bounded = bound_fault_message(&long);
        assert!(bounded.len() <= MAX_FAULT_MESSAGE_BYTES);
        assert!(bounded.len() >= MAX_FAULT_MESSAGE_BYTES - 1);
        assert!(FaultDetail::message_is_valid(&bounded));
        let spaced = format!("{} b", "a".repeat(MAX_FAULT_MESSAGE_BYTES - 1));
        assert_eq!(
            bound_fault_message(&spaced),
            "a".repeat(MAX_FAULT_MESSAGE_BYTES - 1)
        );
        for invalid in [
            "",
            " a",
            "a ",
            "a\nb",
            &"a".repeat(MAX_FAULT_MESSAGE_BYTES + 1),
        ] {
            assert!(!FaultDetail::message_is_valid(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn a_detail_keeps_the_refusal_reason_and_never_goes_blank() {
        let rejection = Rejection::new(RejectionCode::Conflict, "input 3 is on Program\n")
            .with_reason(RefusalReason::ProjectLocked);
        assert_eq!(
            FaultDetail::of(&rejection),
            FaultDetail {
                reason: Some(RefusalReason::ProjectLocked),
                message: "input 3 is on Program".to_owned(),
            }
        );
        assert_eq!(
            FaultDetail::new(Some(RefusalReason::ProjectLocked), " \n ").message,
            "the project padlock is engaged"
        );
        assert_eq!(FaultDetail::new(None, "").message, "the action was refused");
        assert!(FaultDetail::new(None, "x").is_valid());
        assert!(
            !FaultDetail {
                reason: Some(RefusalReason::ScriptRunning { script: 0 }),
                message: "x".to_owned(),
            }
            .is_valid()
        );
    }
}
