//! Title field lists, field names, and snapshot title summaries.
//!
//! A field list is one record value of comma-separated items, each
//! `<field id>:<tag>:<value>`:
//!
//! * `t:<len>:<text>` - text, `<len>` its canonical decimal UTF-8 byte length;
//! * `n:<i64>` - a canonical signed decimal number;
//! * `c:<rrggbbaa>` - eight lowercase hex digits;
//! * `i:<len>:<uri>` - a nonblank image asset URI, counted like text.
//!
//! Text and URIs are counted rather than escaped a second time. The record
//! layer already percent-escapes every value once; escaping list items again
//! (as identifier lists do) would cost up to five wire bytes per content byte,
//! and the [`MAX_TITLE_CONTENT_BYTES`] budget would no longer fit one
//! [`super::MAX_FIELD_VALUE_BYTES`] value. Counted, every content byte costs
//! at most three wire bytes, so a full title (128 items, 12 KiB of content)
//! stays under 45 KiB in one value and, with its field names, under one
//! [`super::MAX_LINE_BYTES`] record.

use core::fmt::Write as _;
use core::num::NonZeroU128;

use crate::{
    CodecError, MAX_LIST_ITEMS, MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELD_NAMES_BYTES,
    MAX_TITLE_FIELDS, MAX_TITLE_NAME_BYTES, MAX_TITLE_TEXT_CHARS, TitleAirRequirement,
    TitleFieldUpdate, TitleFieldValue, TitlePhase, TitleShownOn, TitleSummary, TitleTimelineStatus,
    WireInputId, WireTitleFieldId,
};

use super::value::{parse_canonical_input, parse_canonical_u64};
use super::{decode_rgba, encode_rgba};

const FIELDS: &str = "fields";
const FIELD_NAMES: &str = "field_names";
const TITLES: &str = "titles";

/// Enforces the field-list invariants shared by the encoder and decoder:
/// at most [`MAX_TITLE_FIELDS`] items in strictly ascending id order, text
/// within [`MAX_TITLE_TEXT_CHARS`], nonblank image URIs, and text plus URIs
/// within [`MAX_TITLE_CONTENT_BYTES`].
pub(super) fn validate_title_fields(fields: &[TitleFieldUpdate]) -> Result<(), CodecError> {
    if fields.len() > MAX_TITLE_FIELDS {
        return Err(CodecError::TooManyItems(FIELDS));
    }
    let invalid = |value: String| CodecError::InvalidField {
        field: FIELDS,
        value,
    };
    for pair in fields.windows(2) {
        if pair[0].field >= pair[1].field {
            return Err(invalid(pair[1].field.to_string()));
        }
    }
    let mut bytes = 0_usize;
    for update in fields {
        match &update.value {
            TitleFieldValue::Text(text) if text.chars().count() > MAX_TITLE_TEXT_CHARS => {
                return Err(invalid(update.field.to_string()));
            }
            TitleFieldValue::Image { asset_uri } if asset_uri.trim().is_empty() => {
                return Err(invalid(update.field.to_string()));
            }
            _ => {}
        }
        bytes = bytes.saturating_add(update.value.content_bytes());
    }
    if bytes > MAX_TITLE_CONTENT_BYTES {
        return Err(invalid(format!("{bytes} content bytes")));
    }
    Ok(())
}

/// Encodes a validated field list.
pub(super) fn encode_title_fields(fields: &[TitleFieldUpdate]) -> Result<String, CodecError> {
    validate_title_fields(fields)?;
    let mut encoded = String::new();
    for (index, update) in fields.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        let written = match &update.value {
            TitleFieldValue::Text(text) => {
                write!(encoded, "{}:t:{}:{text}", update.field, text.len())
            }
            TitleFieldValue::Number(number) => write!(encoded, "{}:n:{number}", update.field),
            TitleFieldValue::Color {
                red,
                green,
                blue,
                alpha,
            } => write!(
                encoded,
                "{}:c:{}",
                update.field,
                encode_rgba(*red, *green, *blue, *alpha)
            ),
            TitleFieldValue::Image { asset_uri } => {
                write!(
                    encoded,
                    "{}:i:{}:{asset_uri}",
                    update.field,
                    asset_uri.len()
                )
            }
        };
        written.expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Strictly decodes a field list and enforces [`validate_title_fields`].
pub(super) fn decode_title_fields(value: &str) -> Result<Vec<TitleFieldUpdate>, CodecError> {
    let invalid = || CodecError::InvalidField {
        field: FIELDS,
        value: value.to_owned(),
    };
    let mut fields = Vec::new();
    if value.is_empty() {
        return Ok(fields);
    }
    let mut rest = value;
    loop {
        if fields.len() == MAX_TITLE_FIELDS {
            return Err(CodecError::TooManyItems(FIELDS));
        }
        let (id, after) = rest.split_once(':').ok_or_else(invalid)?;
        let field = parse_canonical_nonzero(id)
            .map(WireTitleFieldId::new)
            .ok_or_else(invalid)?;
        let (tag, after) = after.split_once(':').ok_or_else(invalid)?;
        let (value, after) = match tag {
            "t" | "i" => {
                let (raw, after) = take_counted(after).ok_or_else(invalid)?;
                let value = if tag == "t" {
                    TitleFieldValue::Text(raw.to_owned())
                } else {
                    TitleFieldValue::Image {
                        asset_uri: raw.to_owned(),
                    }
                };
                (value, after)
            }
            "n" | "c" => {
                let (raw, after) = after.split_at(after.find(',').unwrap_or(after.len()));
                let value = if tag == "n" {
                    TitleFieldValue::Number(parse_canonical_i64(raw).ok_or_else(invalid)?)
                } else {
                    let [red, green, blue, alpha] = decode_rgba(raw).ok_or_else(invalid)?;
                    TitleFieldValue::Color {
                        red,
                        green,
                        blue,
                        alpha,
                    }
                };
                (value, after)
            }
            _ => return Err(invalid()),
        };
        fields.push(TitleFieldUpdate { field, value });
        if after.is_empty() {
            break;
        }
        rest = after.strip_prefix(',').ok_or_else(invalid)?;
    }
    validate_title_fields(&fields)?;
    Ok(fields)
}

/// Enforces the title display name bound: nonblank and at most
/// [`MAX_TITLE_NAME_BYTES`].
pub(super) fn validate_title_name(name: &str) -> Result<(), CodecError> {
    if name.trim().is_empty() || name.len() > MAX_TITLE_NAME_BYTES {
        Err(CodecError::InvalidField {
            field: "name",
            value: name.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// The wire token of a title animation phase.
pub(super) const fn encode_title_phase(phase: TitlePhase) -> &'static str {
    match phase {
        TitlePhase::Idle => "idle",
        TitlePhase::In => "in",
        TitlePhase::Hold => "hold",
        TitlePhase::Out => "out",
    }
}

/// The wire token of where a title is shown (Protocol 2.77).
pub(super) const fn encode_title_shown(timeline: Option<TitleTimelineStatus>) -> &'static str {
    match timeline {
        None => "none",
        Some(TitleTimelineStatus {
            shown: TitleShownOn::Air,
            ..
        }) => "air",
        Some(TitleTimelineStatus {
            shown: TitleShownOn::Preview,
            ..
        }) => "preview",
    }
}

/// Strictly decodes where a title is shown and its timeline's start: a
/// title not shown carries `since_frame=0`.
pub(super) fn decode_title_timeline(
    shown: &str,
    since_frame: u64,
) -> Result<Option<TitleTimelineStatus>, CodecError> {
    let shown = match shown {
        "none" if since_frame == 0 => return Ok(None),
        "none" => {
            return Err(CodecError::InvalidField {
                field: "since_frame",
                value: since_frame.to_string(),
            });
        }
        "air" => TitleShownOn::Air,
        "preview" => TitleShownOn::Preview,
        _ => {
            return Err(CodecError::InvalidField {
                field: "shown",
                value: shown.to_owned(),
            });
        }
    };
    Ok(Some(TitleTimelineStatus { shown, since_frame }))
}

/// Strictly decodes a title animation phase token.
pub(super) fn decode_title_phase(value: &str) -> Result<TitlePhase, CodecError> {
    match value {
        "idle" => Ok(TitlePhase::Idle),
        "in" => Ok(TitlePhase::In),
        "hold" => Ok(TitlePhase::Hold),
        "out" => Ok(TitlePhase::Out),
        _ => Err(CodecError::InvalidField {
            field: "phase",
            value: value.to_owned(),
        }),
    }
}

/// Enforces the phase progress invariants: zero progress while idle or
/// holding, `phase_frame < phase_frames` while In or Out plays.
pub(super) fn validate_title_phase(
    phase: TitlePhase,
    phase_frame: u64,
    phase_frames: u64,
) -> Result<(), CodecError> {
    let valid = match phase {
        TitlePhase::Idle | TitlePhase::Hold => phase_frame == 0 && phase_frames == 0,
        TitlePhase::In | TitlePhase::Out => phase_frame < phase_frames,
    };
    if valid {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field: "phase_frame",
            value: format!("{phase_frame}/{phase_frames}"),
        })
    }
}

/// Enforces the field-name bounds: every name nonblank and all of them
/// together at most [`MAX_TITLE_FIELD_NAMES_BYTES`].
fn validate_title_field_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), CodecError> {
    let mut bytes = 0_usize;
    for name in names {
        if name.trim().is_empty() {
            return Err(CodecError::InvalidField {
                field: FIELD_NAMES,
                value: name.to_owned(),
            });
        }
        bytes = bytes.saturating_add(name.len());
    }
    if bytes > MAX_TITLE_FIELD_NAMES_BYTES {
        return Err(CodecError::InvalidField {
            field: FIELD_NAMES,
            value: format!("{bytes} bytes"),
        });
    }
    Ok(())
}

/// Encodes field names positionally as comma-separated `<len>:<name>` items.
pub(super) fn encode_title_field_names<'a>(
    names: impl IntoIterator<Item = &'a str> + Clone,
) -> Result<String, CodecError> {
    validate_title_field_names(names.clone())?;
    let mut encoded = String::new();
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        write!(encoded, "{}:{name}", name.len()).expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Strictly decodes exactly `count` positional field names.
pub(super) fn decode_title_field_names(
    value: &str,
    count: usize,
) -> Result<Vec<String>, CodecError> {
    let invalid = || CodecError::InvalidField {
        field: FIELD_NAMES,
        value: value.to_owned(),
    };
    let mut names = Vec::with_capacity(count.min(MAX_TITLE_FIELDS));
    let mut rest = value;
    for index in 0..count {
        if index > 0 {
            rest = rest.strip_prefix(',').ok_or_else(invalid)?;
        }
        let (name, after) = take_counted(rest).ok_or_else(invalid)?;
        names.push(name.to_owned());
        rest = after;
    }
    if !rest.is_empty() {
        return Err(invalid());
    }
    validate_title_field_names(names.iter().map(String::as_str))?;
    Ok(names)
}

/// Splits `<len>:<raw>` off the front of `value`, where `<len>` is the
/// canonical decimal byte length of `raw` and `raw` ends on a char boundary.
fn take_counted(value: &str) -> Option<(&str, &str)> {
    let (length, after) = value.split_once(':')?;
    let length = usize::try_from(parse_canonical_u64(length)?).ok()?;
    let raw = after.get(..length)?;
    Some((raw, &after[length..]))
}

/// Parses a canonical nonzero decimal identifier as written by `Display`.
fn parse_canonical_nonzero(value: &str) -> Option<NonZeroU128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.starts_with('0')
    {
        return None;
    }
    NonZeroU128::new(value.parse().ok()?)
}

/// Parses a canonical signed decimal: `0`, or an optional `-` and digits
/// without a leading zero, exactly as `i64::to_string` writes it.
fn parse_canonical_i64(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.starts_with('0') && value != "0")
    {
        return None;
    }
    value.parse().ok()
}

pub(super) const fn title_air_requirement(requirement: TitleAirRequirement) -> &'static str {
    match requirement {
        TitleAirRequirement::OffAir => "off_air",
        TitleAirRequirement::OnAir => "on_air",
    }
}

pub(super) fn parse_title_air_requirement(
    value: String,
) -> Result<TitleAirRequirement, CodecError> {
    match value.as_str() {
        "off_air" => Ok(TitleAirRequirement::OffAir),
        "on_air" => Ok(TitleAirRequirement::OnAir),
        _ => Err(CodecError::InvalidField {
            field: "require",
            value,
        }),
    }
}

/// Enforces the snapshot title-summary invariants shared by the encoder and
/// decoder: strictly ascending inputs, each an input of the same snapshot.
pub(super) fn validate_title_summaries(
    titles: &[TitleSummary],
    inputs: &[WireInputId],
) -> Result<(), CodecError> {
    if titles.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems(TITLES));
    }
    for pair in titles.windows(2) {
        if pair[0].input.get() >= pair[1].input.get() {
            return Err(CodecError::InvalidField {
                field: TITLES,
                value: pair[1].input.to_string(),
            });
        }
    }
    if let Some(unknown) = titles.iter().find(|title| !inputs.contains(&title.input)) {
        return Err(CodecError::InvalidField {
            field: TITLES,
            value: unknown.input.to_string(),
        });
    }
    Ok(())
}

/// Encodes title summaries as comma-separated `<input>:<field revision>`.
pub(super) fn encode_title_summaries(
    titles: &[TitleSummary],
    inputs: &[WireInputId],
) -> Result<String, CodecError> {
    validate_title_summaries(titles, inputs)?;
    Ok(titles
        .iter()
        .map(|title| format!("{}:{}", title.input, title.field_revision))
        .collect::<Vec<_>>()
        .join(","))
}

/// Strictly decodes title summaries against the snapshot's inputs.
pub(super) fn decode_title_summaries(
    value: &str,
    inputs: &[WireInputId],
) -> Result<Vec<TitleSummary>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_LIST_ITEMS + 1).count() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems(TITLES));
    }
    let titles = value
        .split(',')
        .map(|entry| {
            let invalid = || CodecError::InvalidField {
                field: TITLES,
                value: entry.to_owned(),
            };
            let (input, revision) = entry.split_once(':').ok_or_else(invalid)?;
            Ok(TitleSummary {
                input: parse_canonical_input(input).ok_or_else(invalid)?,
                field_revision: parse_canonical_u64(revision).ok_or_else(invalid)?,
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    validate_title_summaries(&titles, inputs)?;
    Ok(titles)
}
