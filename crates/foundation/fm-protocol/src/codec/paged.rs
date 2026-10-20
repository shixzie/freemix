//! Paged input codec (IN-014).
//!
//! * Status fields: `input`, `paged_kind` (`document`), `page`, `count`,
//!   `state` (`loading|converting|ready|unavailable|failed`), and optional
//!   `title` and `message`. A known count bounds the page.
//! * `paged_changed` event: `action`, `target` exactly when `action=goto`,
//!   then the status fields.
//! * `paged_page_request`: `request_id`, `input`, `page`, `thumbnail`
//!   (`0|1`). `paged_page`: `request_id`, `input`, `page`, `content`
//!   (`notes|thumbnail|unavailable`), then `notes` for notes, or `width`,
//!   `height`, and base64url `data` for a thumbnail.

use fm_types::paged::{DocumentState, MAX_PAGED_PAGES, PagedNavigation};

use crate::{
    CodecError, PagedKind, PagedPageContent, PagedPageMessage, PagedPageRequest, PagedStatus,
    paged_notes_valid, paged_thumbnail_size_valid,
};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn page_number(field: &'static str, value: &str) -> Result<u32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u32::try_from(number).ok())
        .filter(|page| *page < MAX_PAGED_PAGES)
        .ok_or_else(|| invalid(field, value))
}

fn count_number(value: &str) -> Result<u32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u32::try_from(number).ok())
        .filter(|count| *count <= MAX_PAGED_PAGES)
        .ok_or_else(|| invalid("count", value))
}

/// Encodes a status's fields.
pub(super) fn encode_status(status: &PagedStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    if !status.is_valid() {
        return Err(invalid("page", status.page.to_string()));
    }
    push("input", status.input.to_string())?;
    push("paged_kind", status.kind.token().to_owned())?;
    push("page", status.page.to_string())?;
    push("count", status.count.to_string())?;
    push("state", status.state.token().to_owned())?;
    if let Some(title) = &status.title {
        push("title", title.clone())?;
    }
    if let Some(message) = &status.message {
        push("message", message.clone())?;
    }
    Ok(())
}

/// Decodes a status's fields.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<PagedStatus, CodecError> {
    let input = parse_canonical_input(&fields.required("input")?)
        .ok_or_else(|| invalid("input", "not a canonical input"))?;
    let kind_token = fields.required("paged_kind")?;
    let kind =
        PagedKind::from_token(&kind_token).ok_or_else(|| invalid("paged_kind", kind_token))?;
    let page = page_number("page", &fields.required("page")?)?;
    let count = count_number(&fields.required("count")?)?;
    let state_token = fields.required("state")?;
    let state =
        DocumentState::from_token(&state_token).ok_or_else(|| invalid("state", state_token))?;
    let status = PagedStatus {
        input,
        kind,
        page,
        count,
        state,
        title: fields.optional("title"),
        message: fields.optional("message"),
    };
    if !status.is_valid() {
        return Err(invalid("page", page.to_string()));
    }
    Ok(status)
}

/// Encodes a navigation as `action` and, for `goto`, `page_field`.
pub(super) fn encode_navigation(
    navigation: PagedNavigation,
    page_field: &'static str,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("action", navigation.token().to_owned())?;
    if let PagedNavigation::Goto(page) = navigation {
        if page >= MAX_PAGED_PAGES {
            return Err(invalid(page_field, page.to_string()));
        }
        push(page_field, page.to_string())?;
    }
    Ok(())
}

/// Decodes `action` and, for `goto`, `page_field`.
pub(super) fn decode_navigation(
    fields: &mut dyn FieldSource,
    page_field: &'static str,
) -> Result<PagedNavigation, CodecError> {
    let action = fields.required("action")?;
    let page = fields
        .optional(page_field)
        .map(|value| page_number(page_field, &value))
        .transpose()?;
    PagedNavigation::from_token(&action, page).ok_or_else(|| invalid("action", action))
}

/// Encodes a `paged_page_request`'s fields.
pub(super) fn encode_request(
    request: &PagedPageRequest,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    super::validate_request_id(&request.request_id)?;
    if request.page >= MAX_PAGED_PAGES {
        return Err(invalid("page", request.page.to_string()));
    }
    push("request_id", request.request_id.clone())?;
    push("input", request.input.to_string())?;
    push("page", request.page.to_string())?;
    push(
        "thumbnail",
        if request.thumbnail { "1" } else { "0" }.to_owned(),
    )
}

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(invalid(field, other)),
    }
}

/// Decodes a `paged_page_request`.
pub(super) fn decode_request(fields: &mut dyn FieldSource) -> Result<PagedPageRequest, CodecError> {
    let request_id = fields.required("request_id")?;
    super::validate_request_id(&request_id)?;
    Ok(PagedPageRequest {
        request_id,
        input: parse_canonical_input(&fields.required("input")?)
            .ok_or_else(|| invalid("input", "not a canonical input"))?,
        page: page_number("page", &fields.required("page")?)?,
        thumbnail: flag("thumbnail", &fields.required("thumbnail")?)?,
    })
}

/// Encodes a `paged_page` answer's fields.
pub(super) fn encode_page(
    message: &PagedPageMessage,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    super::validate_request_id(&message.request_id)?;
    if message.page >= MAX_PAGED_PAGES {
        return Err(invalid("page", message.page.to_string()));
    }
    push("request_id", message.request_id.clone())?;
    push("input", message.input.to_string())?;
    push("page", message.page.to_string())?;
    match &message.content {
        PagedPageContent::Notes(notes) => {
            if !paged_notes_valid(notes) {
                return Err(invalid("notes", format!("{} bytes", notes.len())));
            }
            push("content", "notes".to_owned())?;
            push("notes", notes.clone())
        }
        PagedPageContent::Thumbnail {
            width,
            height,
            data,
        } => {
            if !paged_thumbnail_size_valid(*width, *height)
                || data.is_empty()
                || data.len() > crate::MAX_PAGED_THUMBNAIL_BYTES
            {
                return Err(invalid("data", format!("{width}x{height}")));
            }
            push("content", "thumbnail".to_owned())?;
            push("width", width.to_string())?;
            push("height", height.to_string())?;
            push("data", crate::monitor_image::base64url_encode(data))
        }
        PagedPageContent::Unavailable => push("content", "unavailable".to_owned()),
    }
}

fn dimension(field: &'static str, value: &str) -> Result<u16, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u16::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

/// Decodes a `paged_page` answer.
pub(super) fn decode_page(fields: &mut dyn FieldSource) -> Result<PagedPageMessage, CodecError> {
    let request_id = fields.required("request_id")?;
    super::validate_request_id(&request_id)?;
    let input = parse_canonical_input(&fields.required("input")?)
        .ok_or_else(|| invalid("input", "not a canonical input"))?;
    let page = page_number("page", &fields.required("page")?)?;
    let content = match fields.required("content")?.as_str() {
        "notes" => {
            let notes = fields.required("notes")?;
            if !paged_notes_valid(&notes) {
                return Err(invalid("notes", format!("{} bytes", notes.len())));
            }
            PagedPageContent::Notes(notes)
        }
        "thumbnail" => {
            let width = dimension("width", &fields.required("width")?)?;
            let height = dimension("height", &fields.required("height")?)?;
            let encoded = fields.required("data")?;
            let limit = (crate::MAX_PAGED_THUMBNAIL_BYTES * 4).div_ceil(3);
            let data = if encoded.len() > limit {
                None
            } else {
                crate::monitor_image::base64url_decode(&encoded)
            }
            .filter(|data| !data.is_empty())
            .ok_or_else(|| invalid("data", format!("{} characters", encoded.len())))?;
            if !paged_thumbnail_size_valid(width, height) {
                return Err(invalid("width", format!("{width}x{height}")));
            }
            PagedPageContent::Thumbnail {
                width,
                height,
                data,
            }
        }
        "unavailable" => PagedPageContent::Unavailable,
        other => return Err(invalid("content", other)),
    };
    Ok(PagedPageMessage {
        request_id,
        input,
        page,
        content,
    })
}
