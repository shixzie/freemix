//! Input lifecycle codec (Protocol 2.38, IN-026).
//!
//! * A snapshot input is `<id>~<name>~<lifecycle>` where `<lifecycle>` is
//!   `<category id|->:<colour token|->:<paused 0|1>:<closed 0|1>:<offline
//!   reason|->`.
//! * `input_categories` (snapshot field, `input_categories_set` command)
//!   is a comma-separated list of `<id>~<name>` (empty for none).
//! * A source is `solid:<rrggbbaa>`, `bars`, either optionally followed by
//!   `:<tone hz>`, or `media:<uri>`, `device:<key>`, `network:<endpoint>`.
//! * `items` (`input_import_batch`) is a comma-separated list of
//!   `<input>~<name|->~<uri>`.
//! * `added` (event) is a comma-separated list of `<input>~<name>`, and
//!   `refused` of `<item index>:<refusal token>`.
//!
//! Text inside a field is percent-escaped before it is joined, and every
//! number is a canonical unsigned decimal.

use std::collections::BTreeSet;

use crate::{
    AddedInput, CodecError, ImportItemSpec, ImportRefusal, InputCategoryStatus, InputColorLabel,
    InputLifecycleOperation, InputLifecycleStatus, InputOfflineReason, MAX_BATCH_IMPORT_ITEMS,
    MAX_INPUT_CATEGORIES, MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES,
    WireGeneratorVideo, WireInputId, WireInputSource,
};

use super::media::{FieldSource, Push};
use super::value::{
    escape_bounded, input_ids, parse_canonical_input, parse_canonical_u64, parse_input_ids,
    unescape,
};
use super::{MAX_LIST_ITEMS, decode_rgba, encode_rgba, validate_new_input_name};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
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

fn parse_bool(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(field, value)),
    }
}

/// Encodes one input's lifecycle.
pub(super) fn encode_lifecycle_status(status: &InputLifecycleStatus) -> Result<String, CodecError> {
    if status.category == Some(0) {
        return Err(invalid("inputs", "category 0"));
    }
    Ok(format!(
        "{}:{}:{}:{}:{}",
        status
            .category
            .map_or_else(|| "-".to_owned(), |category| category.to_string()),
        status.color.map_or("-", InputColorLabel::token),
        u8::from(status.paused),
        u8::from(status.closed),
        status.offline.map_or("-", InputOfflineReason::token),
    ))
}

/// Decodes one input's lifecycle.
pub(super) fn decode_lifecycle_status(value: &str) -> Result<InputLifecycleStatus, CodecError> {
    let parts: Vec<&str> = value.split(':').collect();
    let [category, color, paused, closed, offline] = parts.as_slice() else {
        return Err(invalid("inputs", value));
    };
    Ok(InputLifecycleStatus {
        category: match *category {
            "-" => None,
            category => Some(parse_u32("inputs", category).and_then(|id| {
                if id == 0 {
                    Err(invalid("inputs", value))
                } else {
                    Ok(id)
                }
            })?),
        },
        color: match *color {
            "-" => None,
            token => {
                Some(InputColorLabel::from_token(token).ok_or_else(|| invalid("inputs", value))?)
            }
        },
        paused: parse_flag("inputs", paused)?,
        closed: parse_flag("inputs", closed)?,
        offline: match *offline {
            "-" => None,
            token => Some(
                InputOfflineReason::from_token(token).ok_or_else(|| invalid("inputs", value))?,
            ),
        },
    })
}

fn validate_category(category: &InputCategoryStatus) -> Result<(), CodecError> {
    if category.id == 0
        || category.name.trim().is_empty()
        || category.name.len() > MAX_INPUT_CATEGORY_NAME_BYTES
        || category.name.chars().any(char::is_control)
    {
        return Err(invalid("categories", category.name.clone()));
    }
    Ok(())
}

/// Encodes a category list (empty for none).
pub(super) fn encode_categories(categories: &[InputCategoryStatus]) -> Result<String, CodecError> {
    if categories.len() > MAX_INPUT_CATEGORIES {
        return Err(CodecError::TooManyItems("categories"));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut encoded = Vec::with_capacity(categories.len());
    for category in categories {
        validate_category(category)?;
        if !ids.insert(category.id) || !names.insert(category.name.to_lowercase()) {
            return Err(invalid("categories", category.name.clone()));
        }
        encoded.push(format!(
            "{}~{}",
            category.id,
            escape_bounded(&category.name)?
        ));
    }
    Ok(encoded.join(","))
}

/// Decodes a category list.
pub(super) fn decode_categories(value: &str) -> Result<Vec<InputCategoryStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').count() > MAX_INPUT_CATEGORIES {
        return Err(CodecError::TooManyItems("categories"));
    }
    let categories = value
        .split(',')
        .map(|entry| {
            let (id, name) = entry
                .split_once('~')
                .ok_or_else(|| invalid("categories", value))?;
            let category = InputCategoryStatus {
                id: parse_u32("categories", id)?,
                name: unescape(name)?,
            };
            validate_category(&category)?;
            Ok(category)
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    // Re-encoding checks unique ids and names.
    encode_categories(&categories)?;
    Ok(categories)
}

fn validate_locator(field: &'static str, locator: &str) -> Result<(), CodecError> {
    if locator.trim().is_empty()
        || locator.len() > MAX_INPUT_SOURCE_BYTES
        || locator.chars().any(char::is_control)
    {
        return Err(invalid(field, locator));
    }
    Ok(())
}

/// Encodes a replaceable source.
pub(super) fn encode_source(source: &WireInputSource) -> Result<String, CodecError> {
    Ok(match source {
        WireInputSource::Generator { video, tone_hz } => {
            let mut encoded = match video {
                WireGeneratorVideo::Solid {
                    rgba: [red, green, blue, alpha],
                } => format!("solid:{}", encode_rgba(*red, *green, *blue, *alpha)),
                WireGeneratorVideo::Bars => "bars".to_owned(),
            };
            if let Some(tone) = tone_hz {
                encoded.push(':');
                encoded.push_str(&tone.to_string());
            }
            encoded
        }
        WireInputSource::Media { uri } => {
            validate_locator("source", uri)?;
            format!("media:{}", escape_bounded(uri)?)
        }
        WireInputSource::Device { stable_key } => {
            validate_locator("source", stable_key)?;
            format!("device:{}", escape_bounded(stable_key)?)
        }
        WireInputSource::Network { endpoint } => {
            validate_locator("source", endpoint)?;
            format!("network:{}", escape_bounded(endpoint)?)
        }
    })
}

/// Decodes a replaceable source.
pub(super) fn decode_source(value: &str) -> Result<WireInputSource, CodecError> {
    let (kind, rest) = value.split_once(':').unwrap_or((value, ""));
    let tone = |text: &str| -> Result<Option<u32>, CodecError> {
        if text.is_empty() {
            Ok(None)
        } else {
            parse_u32("source", text).map(Some)
        }
    };
    let locator = |rest: &str| -> Result<String, CodecError> {
        let text = unescape(rest)?;
        validate_locator("source", &text)?;
        Ok(text)
    };
    Ok(match kind {
        "solid" => {
            let (rgba, tone_text) = rest.split_once(':').unwrap_or((rest, ""));
            if rest.ends_with(':') {
                return Err(invalid("source", value));
            }
            WireInputSource::Generator {
                video: WireGeneratorVideo::Solid {
                    rgba: decode_rgba(rgba).ok_or_else(|| invalid("source", value))?,
                },
                tone_hz: tone(tone_text)?,
            }
        }
        "bars" => {
            if value.ends_with(':') {
                return Err(invalid("source", value));
            }
            WireInputSource::Generator {
                video: WireGeneratorVideo::Bars,
                tone_hz: tone(rest)?,
            }
        }
        "media" => WireInputSource::Media {
            uri: locator(rest)?,
        },
        "device" => WireInputSource::Device {
            stable_key: locator(rest)?,
        },
        "network" => WireInputSource::Network {
            endpoint: locator(rest)?,
        },
        _ => return Err(invalid("source", value)),
    })
}

fn encode_items(items: &[ImportItemSpec]) -> Result<String, CodecError> {
    if items.is_empty() || items.len() > MAX_BATCH_IMPORT_ITEMS {
        return Err(CodecError::TooManyItems("items"));
    }
    let mut inputs = BTreeSet::new();
    let mut encoded = Vec::with_capacity(items.len());
    for item in items {
        if !inputs.insert(item.input.get()) {
            return Err(invalid("items", item.input.to_string()));
        }
        if let Some(name) = &item.name {
            validate_new_input_name(name)?;
        }
        validate_locator("items", &item.uri)?;
        encoded.push(format!(
            "{}~{}~{}",
            item.input,
            match item.name.as_deref() {
                None => "-".to_owned(),
                // A bare `-` means no name.
                Some("-") => "%2D".to_owned(),
                Some(name) => escape_bounded(name)?,
            },
            escape_bounded(&item.uri)?
        ));
    }
    Ok(encoded.join(","))
}

fn decode_items(value: &str) -> Result<Vec<ImportItemSpec>, CodecError> {
    if value.is_empty() || value.split(',').count() > MAX_BATCH_IMPORT_ITEMS {
        return Err(CodecError::TooManyItems("items"));
    }
    let items = value
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('~').collect();
            let [input, name, uri] = parts.as_slice() else {
                return Err(invalid("items", value));
            };
            Ok(ImportItemSpec {
                input: parse_canonical_input(input).ok_or_else(|| invalid("items", value))?,
                name: match *name {
                    "-" => None,
                    name => Some(unescape(name)?),
                },
                uri: unescape(uri)?,
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    encode_items(&items)?;
    Ok(items)
}

/// Writes an operation's fields (without its payload name).
pub(super) fn encode_lifecycle_operation(
    operation: &InputLifecycleOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match operation {
        InputLifecycleOperation::Duplicate {
            source,
            input,
            name,
        } => {
            if source == input {
                return Err(invalid("input", input.to_string()));
            }
            push("source", source.to_string())?;
            push("input", input.to_string())?;
            if let Some(name) = name {
                validate_new_input_name(name)?;
                push("name", name.clone())?;
            }
        }
        InputLifecycleOperation::ReplaceSource { input, source } => {
            push("input", input.to_string())?;
            push("source", encode_source(source)?)?;
        }
        InputLifecycleOperation::SetCategory { input, category } => {
            push("input", input.to_string())?;
            if *category == Some(0) {
                return Err(invalid("category", "0"));
            }
            push(
                "category",
                category.map_or_else(|| "-".to_owned(), |category| category.to_string()),
            )?;
        }
        InputLifecycleOperation::SetColor { input, color } => {
            push("input", input.to_string())?;
            push(
                "color",
                color.map_or("-", InputColorLabel::token).to_owned(),
            )?;
        }
        InputLifecycleOperation::SetPaused { input, paused } => {
            push("input", input.to_string())?;
            push("paused", paused.to_string())?;
        }
        InputLifecycleOperation::SetClosed { input, closed } => {
            push("input", input.to_string())?;
            push("closed", closed.to_string())?;
        }
        InputLifecycleOperation::SetCategories { categories } => {
            push("categories", encode_categories(categories)?)?;
        }
        InputLifecycleOperation::Import { items } => push("items", encode_items(items)?)?,
        InputLifecycleOperation::RemoveInputs { inputs } => push("inputs", input_ids(inputs)?)?,
    }
    Ok(())
}

fn input_field(
    fields: &mut dyn FieldSource,
    name: &'static str,
) -> Result<WireInputId, CodecError> {
    let value = fields.required(name)?;
    parse_canonical_input(&value).ok_or_else(|| invalid(name, value))
}

/// Decodes the operation named by `payload`, or `None` for another payload.
pub(super) fn decode_lifecycle_operation(
    payload: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<InputLifecycleOperation>, CodecError> {
    Ok(Some(match payload {
        "input_duplicate" => {
            let source = input_field(fields, "source")?;
            let input = input_field(fields, "input")?;
            if source == input {
                return Err(invalid("input", input.to_string()));
            }
            let name = fields.optional("name");
            if let Some(name) = &name {
                validate_new_input_name(name)?;
            }
            InputLifecycleOperation::Duplicate {
                source,
                input,
                name,
            }
        }
        "input_replace_source" => InputLifecycleOperation::ReplaceSource {
            input: input_field(fields, "input")?,
            source: decode_source(&fields.required("source")?)?,
        },
        "input_category_set" => {
            let input = input_field(fields, "input")?;
            let value = fields.required("category")?;
            InputLifecycleOperation::SetCategory {
                input,
                category: match value.as_str() {
                    "-" => None,
                    text => {
                        let id = parse_u32("category", text)?;
                        if id == 0 {
                            return Err(invalid("category", value));
                        }
                        Some(id)
                    }
                },
            }
        }
        "input_color_set" => {
            let input = input_field(fields, "input")?;
            let value = fields.required("color")?;
            InputLifecycleOperation::SetColor {
                input,
                color: match value.as_str() {
                    "-" => None,
                    token => Some(
                        InputColorLabel::from_token(token)
                            .ok_or_else(|| invalid("color", token))?,
                    ),
                },
            }
        }
        "input_pause_set" => InputLifecycleOperation::SetPaused {
            input: input_field(fields, "input")?,
            paused: parse_bool("paused", &fields.required("paused")?)?,
        },
        "input_close_set" => InputLifecycleOperation::SetClosed {
            input: input_field(fields, "input")?,
            closed: parse_bool("closed", &fields.required("closed")?)?,
        },
        "input_categories_set" => InputLifecycleOperation::SetCategories {
            categories: decode_categories(&fields.required("categories")?)?,
        },
        "input_import_batch" => InputLifecycleOperation::Import {
            items: decode_items(&fields.required("items")?)?,
        },
        "inputs_remove" => InputLifecycleOperation::RemoveInputs {
            inputs: parse_input_ids(&fields.required("inputs")?)?,
        },
        _ => return Ok(None),
    }))
}

/// Encodes the inputs an event appended.
pub(super) fn encode_added(added: &[AddedInput]) -> Result<String, CodecError> {
    if added.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("added"));
    }
    added
        .iter()
        .map(|item| {
            validate_new_input_name(&item.name)?;
            Ok(format!("{}~{}", item.input, escape_bounded(&item.name)?))
        })
        .collect::<Result<Vec<_>, CodecError>>()
        .map(|items| items.join(","))
}

/// Decodes the inputs an event appended.
pub(super) fn decode_added(value: &str) -> Result<Vec<AddedInput>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').count() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("added"));
    }
    value
        .split(',')
        .map(|entry| {
            let (input, name) = entry
                .split_once('~')
                .ok_or_else(|| invalid("added", value))?;
            let name = unescape(name)?;
            validate_new_input_name(&name)?;
            Ok(AddedInput {
                input: parse_canonical_input(input).ok_or_else(|| invalid("added", value))?,
                name,
            })
        })
        .collect()
}

/// Encodes an event's refused import items.
pub(super) fn encode_refused(refused: &[(u16, ImportRefusal)]) -> Result<String, CodecError> {
    if refused.len() > MAX_BATCH_IMPORT_ITEMS {
        return Err(CodecError::TooManyItems("refused"));
    }
    Ok(refused
        .iter()
        .map(|(index, refusal)| format!("{index}:{}", refusal.token()))
        .collect::<Vec<_>>()
        .join(","))
}

/// Decodes an event's refused import items.
pub(super) fn decode_refused(value: &str) -> Result<Vec<(u16, ImportRefusal)>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').count() > MAX_BATCH_IMPORT_ITEMS {
        return Err(CodecError::TooManyItems("refused"));
    }
    value
        .split(',')
        .map(|entry| {
            let (index, token) = entry
                .split_once(':')
                .ok_or_else(|| invalid("refused", value))?;
            Ok((
                parse_canonical_u64(index)
                    .and_then(|index| u16::try_from(index).ok())
                    .ok_or_else(|| invalid("refused", value))?,
                ImportRefusal::from_token(token).ok_or_else(|| invalid("refused", value))?,
            ))
        })
        .collect()
}
