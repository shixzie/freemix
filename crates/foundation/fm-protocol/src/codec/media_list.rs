//! Media list codec (IN-006).
//!
//! A [`MediaListStatus`] is written as these fields:
//!
//! * `input`: the list input; `active`: the active item id; `cycle`: the
//!   loop cycle; `position`: the active item's zero-based play-order
//!   position.
//! * Options: `auto_next`, `auto_first`, and `loop` (`0|1`),
//!   `shuffle_seed` (`<u64>` or `-`), and (2.95) `transition`
//!   (`cut|fade|dip|wipe`) with `transition_frames`
//!   (`1..=MAX_MEDIA_LIST_TRANSITION_FRAMES`).
//! * `items`: a comma-separated list (1 to [`MAX_MEDIA_LIST_ITEMS`]) of
//!   `<id>~<uri>~<title|->~<mark in>~<mark out|->`, the uri and title
//!   percent-escaped (a title of exactly `-` is written `%2D`).
//! * `order`: the comma-separated item ids in play order.
//! * `lengths` (2.91): one entry per item in authored order, comma
//!   separated: `<frames>@<rate numerator>/<rate denominator>` (frames
//!   `1..=MAX_MEDIA_FRAME`, the rate in lowest terms) for a probed file,
//!   `-` while its length is unknown.
//!
//! The `media_list` command carries `input` and `operation`
//! (`create|set|navigate|advance`). `create` adds `name`, `items`, and the
//! options; `set` the items and options and (2.79) an optional `active`
//! item, one of its items; `navigate` a `navigation` token
//! (`next|previous|first|last|goto|item`) with `index` for `goto` (a
//! play-order position) and `item` (an item id); `advance` the `from` item.
//!
//! Every number is a canonical unsigned decimal.

use std::collections::BTreeSet;

use crate::{
    CodecError, MAX_INPUT_SOURCE_BYTES, MAX_MEDIA_LIST_ITEM_ID, MAX_MEDIA_LIST_ITEMS,
    MAX_MEDIA_LIST_STATES, MAX_MEDIA_LIST_TEXT_BYTES, MAX_MEDIA_LIST_TITLE_BYTES,
    MediaListItemLength, MediaListItemSpec, MediaListNavigation, MediaListOperation,
    MediaListOptionsSpec, MediaListStatus, MediaListTransitionKind, MediaListTransitionSpec,
};

use super::media::{FieldSource, Push};
use super::playlist::{decode_frame_rate, encode_frame_rate};
use super::value::{escape_bounded, parse_canonical_input, parse_canonical_u64, unescape};
use super::{MAX_FIELD_VALUE_BYTES, MAX_LINE_BYTES};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_u64(field: &'static str, value: &str) -> Result<u64, CodecError> {
    parse_canonical_u64(value).ok_or_else(|| invalid(field, value))
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_optional_u64(field: &'static str, value: &str) -> Result<Option<u64>, CodecError> {
    if value == "-" {
        Ok(None)
    } else {
        parse_u64(field, value).map(Some)
    }
}

fn optional(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

/// Checks one item's bounds (its file may be a host path in a command).
fn validate_item(item: &MediaListItemSpec) -> Result<(), CodecError> {
    if item.id == 0 || item.id > MAX_MEDIA_LIST_ITEM_ID {
        return Err(invalid("items", format!("item id {}", item.id)));
    }
    if item.uri.trim().is_empty()
        || item.uri.len() > MAX_INPUT_SOURCE_BYTES
        || item.uri.chars().any(char::is_control)
    {
        return Err(invalid("items", item.uri.clone()));
    }
    if item.title.as_deref().is_some_and(|title| {
        title.trim().is_empty()
            || title.len() > MAX_MEDIA_LIST_TITLE_BYTES
            || title.chars().any(char::is_control)
    }) {
        return Err(invalid("items", format!("item {} title", item.id)));
    }
    if item.mark_out.is_some_and(|out| out <= item.mark_in) {
        return Err(invalid("items", format!("item {} marks", item.id)));
    }
    Ok(())
}

/// Encodes the `items` field.
pub(super) fn encode_items(items: &[MediaListItemSpec]) -> Result<String, CodecError> {
    if items.is_empty() || items.len() > MAX_MEDIA_LIST_ITEMS {
        return Err(CodecError::TooManyItems("items"));
    }
    let text: usize = items
        .iter()
        .map(|item| item.uri.len() + item.title.as_ref().map_or(0, String::len))
        .sum();
    if text > MAX_MEDIA_LIST_TEXT_BYTES {
        return Err(CodecError::FieldValueTooLong);
    }
    let mut ids = BTreeSet::new();
    let mut encoded = Vec::with_capacity(items.len());
    for item in items {
        validate_item(item)?;
        if !ids.insert(item.id) {
            return Err(invalid("items", format!("item id {} repeats", item.id)));
        }
        encoded.push(format!(
            "{}~{}~{}~{}~{}",
            item.id,
            escape_bounded(&item.uri)?,
            match item.title.as_deref() {
                None => "-".to_owned(),
                Some("-") => "%2D".to_owned(),
                Some(title) => escape_bounded(title)?,
            },
            item.mark_in,
            optional(item.mark_out)
        ));
    }
    Ok(encoded.join(","))
}

/// Decodes the `items` field.
pub(super) fn decode_items(value: &str) -> Result<Vec<MediaListItemSpec>, CodecError> {
    if value.is_empty() || value.split(',').count() > MAX_MEDIA_LIST_ITEMS {
        return Err(CodecError::TooManyItems("items"));
    }
    let items = value
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('~').collect();
            let [id, uri, title, mark_in, mark_out] = parts.as_slice() else {
                return Err(invalid("items", entry));
            };
            Ok(MediaListItemSpec {
                id: parse_u32("items", id)?,
                uri: unescape(uri)?,
                title: match *title {
                    "-" => None,
                    title => Some(unescape(title)?),
                },
                mark_in: parse_u64("items", mark_in)?,
                mark_out: parse_optional_u64("items", mark_out)?,
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    encode_items(&items)?;
    Ok(items)
}

fn encode_options(options: MediaListOptionsSpec, push: &mut Push<'_>) -> Result<(), CodecError> {
    push("auto_next", u8::from(options.auto_next).to_string())?;
    push("auto_first", u8::from(options.auto_first).to_string())?;
    push("loop", u8::from(options.loop_list).to_string())?;
    push("shuffle_seed", optional(options.shuffle_seed))?;
    if !options.transition.is_valid() {
        return Err(invalid(
            "transition_frames",
            options.transition.duration_frames.to_string(),
        ));
    }
    push("transition", options.transition.kind.token().to_owned())?;
    push(
        "transition_frames",
        options.transition.duration_frames.to_string(),
    )
}

fn decode_options(fields: &mut dyn FieldSource) -> Result<MediaListOptionsSpec, CodecError> {
    Ok(MediaListOptionsSpec {
        auto_next: parse_flag("auto_next", &fields.required("auto_next")?)?,
        auto_first: parse_flag("auto_first", &fields.required("auto_first")?)?,
        loop_list: parse_flag("loop", &fields.required("loop")?)?,
        shuffle_seed: parse_optional_u64("shuffle_seed", &fields.required("shuffle_seed")?)?,
        transition: {
            let token = fields.required("transition")?;
            let transition = MediaListTransitionSpec {
                kind: MediaListTransitionKind::from_token(&token)
                    .ok_or_else(|| invalid("transition", token))?,
                duration_frames: parse_u32(
                    "transition_frames",
                    &fields.required("transition_frames")?,
                )?,
            };
            if !transition.is_valid() {
                return Err(invalid(
                    "transition_frames",
                    transition.duration_frames.to_string(),
                ));
            }
            transition
        },
    })
}

/// Writes a command's `input`, `operation`, and operation fields (without
/// its payload name).
pub(super) fn encode_command(
    input: crate::WireInputId,
    operation: &MediaListOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("input", input.to_string())?;
    push("operation", operation.token().to_owned())?;
    match operation {
        MediaListOperation::Create {
            name,
            items,
            options,
        } => {
            super::validate_new_input_name(name)?;
            push("name", name.clone())?;
            push("items", encode_items(items)?)?;
            encode_options(*options, push)?;
        }
        MediaListOperation::Set {
            items,
            options,
            active,
        } => {
            push("items", encode_items(items)?)?;
            encode_options(*options, push)?;
            if let Some(active) = active {
                push("active", check_active(*active, items)?.to_string())?;
            }
        }
        MediaListOperation::Navigate(navigation) => {
            push("navigation", navigation.token().to_owned())?;
            if let Some(index) = navigation.index() {
                push("index", index.to_string())?;
            }
        }
        MediaListOperation::Advance { from } => {
            if *from == 0 {
                return Err(invalid("from", "0"));
            }
            push("from", from.to_string())?;
        }
    }
    Ok(())
}

/// Checks a `set`'s `active` item: one of its items.
fn check_active(active: u32, items: &[MediaListItemSpec]) -> Result<u32, CodecError> {
    if items.iter().any(|item| item.id == active) {
        Ok(active)
    } else {
        Err(invalid("active", active.to_string()))
    }
}

/// Decodes a `media_list` command's input and operation.
pub(super) fn decode_command(
    fields: &mut dyn FieldSource,
) -> Result<(crate::WireInputId, MediaListOperation), CodecError> {
    let input = fields.required("input")?;
    let input = parse_canonical_input(&input).ok_or_else(|| invalid("input", input))?;
    let token = fields.required("operation")?;
    let operation = match token.as_str() {
        "create" => {
            let name = fields.required("name")?;
            super::validate_new_input_name(&name)?;
            MediaListOperation::Create {
                name,
                items: decode_items(&fields.required("items")?)?,
                options: decode_options(fields)?,
            }
        }
        "set" => {
            let items = decode_items(&fields.required("items")?)?;
            let options = decode_options(fields)?;
            let active = fields
                .optional("active")
                .map(|active| check_active(parse_u32("active", &active)?, &items))
                .transpose()?;
            MediaListOperation::Set {
                items,
                options,
                active,
            }
        }
        "navigate" => {
            let navigation = fields.required("navigation")?;
            let index = fields
                .optional("index")
                .map(|index| parse_u32("index", &index))
                .transpose()?;
            MediaListOperation::Navigate(
                MediaListNavigation::from_token(&navigation, index)
                    .ok_or_else(|| invalid("navigation", navigation))?,
            )
        }
        "advance" => {
            let from = fields.required("from")?;
            let from = parse_u32("from", &from)?;
            if from == 0 {
                return Err(invalid("from", "0"));
            }
            MediaListOperation::Advance { from }
        }
        _ => return Err(invalid("operation", token)),
    };
    Ok((input, operation))
}

/// Encodes the `lengths` field: one entry per item.
fn encode_lengths(
    lengths: &[Option<MediaListItemLength>],
    items: usize,
) -> Result<String, CodecError> {
    if lengths.len() != items {
        return Err(invalid("lengths", "one length per item"));
    }
    lengths
        .iter()
        .map(|length| match length {
            None => Ok("-".to_owned()),
            Some(length) if length.is_valid() => Ok(format!(
                "{}@{}",
                length.frames,
                encode_frame_rate(length.rate)
            )),
            Some(length) => Err(invalid("lengths", length.frames.to_string())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(","))
}

/// Decodes the `lengths` field (its count is checked against the items).
fn decode_lengths(value: &str) -> Result<Vec<Option<MediaListItemLength>>, CodecError> {
    if value.split(',').count() > MAX_MEDIA_LIST_ITEMS {
        return Err(CodecError::TooManyItems("lengths"));
    }
    value
        .split(',')
        .map(|entry| {
            if entry == "-" {
                return Ok(None);
            }
            let (frames, rate) = entry
                .split_once('@')
                .ok_or_else(|| invalid("lengths", entry))?;
            let length = MediaListItemLength {
                frames: parse_u64("lengths", frames)?,
                rate: decode_frame_rate(rate.to_owned()).map_err(|_| invalid("lengths", entry))?,
            };
            if length.is_valid() {
                Ok(Some(length))
            } else {
                Err(invalid("lengths", entry))
            }
        })
        .collect()
}

/// Whether `status`, written as one record, leaves room for the record's
/// other fields within one line: the longest lists (many long escaped file
/// names) leave no room for their lengths, which the authority then
/// reports as unknown.
#[must_use]
pub fn media_list_status_fits(status: &MediaListStatus) -> bool {
    /// What the record's envelope (its type, revision, frame, operation,
    /// and edit history) may take besides the status.
    const ENVELOPE_BYTES: usize = 8 * 1024;
    let mut bytes = 0_usize;
    encode_status(status, &mut |name, value| {
        // As the record escapes it.
        let escaped: usize = value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                    1
                } else {
                    3
                }
            })
            .sum();
        if escaped > MAX_FIELD_VALUE_BYTES {
            return Err(CodecError::FieldValueTooLong);
        }
        bytes += name.len() + escaped + 2;
        Ok(())
    })
    .is_ok()
        && bytes + ENVELOPE_BYTES <= MAX_LINE_BYTES
}

/// Encodes a whole list status.
pub(super) fn encode_status(
    status: &MediaListStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    let ids: BTreeSet<u32> = status.items.iter().map(|item| item.id).collect();
    let order: BTreeSet<u32> = status.order.iter().copied().collect();
    if !ids.contains(&status.active)
        || order != ids
        || status.order.len() != status.items.len()
        || status.order.get(status.position as usize) != Some(&status.active)
    {
        return Err(invalid("order", "the play order does not fit the items"));
    }
    push("input", status.input.to_string())?;
    push("active", status.active.to_string())?;
    push("cycle", status.cycle.to_string())?;
    push("position", status.position.to_string())?;
    encode_options(status.options, push)?;
    push("items", encode_items(&status.items)?)?;
    push(
        "order",
        status
            .order
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )?;
    push(
        "lengths",
        encode_lengths(&status.lengths, status.items.len())?,
    )
}

/// Decodes a whole list status.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<MediaListStatus, CodecError> {
    let input = fields.required("input")?;
    let status = MediaListStatus {
        input: parse_canonical_input(&input).ok_or_else(|| invalid("input", input))?,
        active: parse_u32("active", &fields.required("active")?)?,
        cycle: parse_u64("cycle", &fields.required("cycle")?)?,
        position: parse_u32("position", &fields.required("position")?)?,
        options: decode_options(fields)?,
        items: decode_items(&fields.required("items")?)?,
        order: fields
            .required("order")?
            .split(',')
            .map(|id| parse_u32("order", id))
            .collect::<Result<Vec<_>, _>>()?,
        lengths: decode_lengths(&fields.required("lengths")?)?,
    };
    // Re-encoding checks that the order and the lengths fit the items.
    encode_status(&status, &mut |_, _| Ok(()))?;
    Ok(status)
}

/// Validates a snapshot's `media_list_states` count.
pub(super) fn parse_state_count(value: &str) -> Result<u16, CodecError> {
    parse_canonical_u64(value)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| *count <= MAX_MEDIA_LIST_STATES)
        .ok_or_else(|| invalid("media_list_states", value))
}

/// Whether `count` is a valid `media_list_states` count.
pub(super) const fn state_count_is_valid(count: u16) -> bool {
    count <= MAX_MEDIA_LIST_STATES
}
