//! Screen capture input codec (IN-011); the contract is in
//! [`crate::screen_capture`].
//!
//! * Config fields: `target`, `screen` or `app`/`title`/`title_match`/
//!   optional `window_id`, `crop`, `cursor`, `fps`, `stall`.
//! * A compact config (the replacement source and snapshot entries) is
//!   `<left>.<top>.<right>.<bottom>~<cursor>~<fps>~<stall>~screen~<index>`
//!   or `...~window~<title_match>~<window_id|->~<app length>~<title
//!   length>:<app><title>`.
//! * A snapshot's `screen_capture_inputs` is a comma-separated, strictly
//!   ascending list of `<input>~<state>~<attempt>~<next_in_ms>~<reopens>~
//!   <width>~<height>~<window_id|->~<compact config>`.

use core::fmt::Write as _;

use fm_types::network_stream::StallSlate;
use fm_types::screen_capture::{
    MAX_SCREEN_CAPTURE_APP_BYTES, MAX_SCREEN_CAPTURE_ERROR_BYTES, MAX_SCREEN_CAPTURE_INPUTS,
    MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES, MAX_SCREEN_CAPTURE_SCREENS,
    MAX_SCREEN_CAPTURE_TITLE_BYTES, MAX_SCREEN_CAPTURE_WINDOWS, ScreenCaptureConfig,
    ScreenCaptureCrop, ScreenCaptureInventory, ScreenCaptureState, ScreenCaptureStatus,
    ScreenCaptureTarget, ScreenInfo, TitleMatch, WindowInfo, WindowRule, is_screen_capture_text,
};

use crate::{CodecError, ScreenCaptureEntry, WireInputId};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

const ENTRIES: &str = "screen_capture_inputs";
const SCREENS: &str = "screens";
const WINDOWS: &str = "windows";

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn canonical<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| T::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

/// A canonical signed decimal: an optional `-` before a canonical
/// unsigned decimal, never `-0`.
fn signed(field: &'static str, value: &str) -> Result<i32, CodecError> {
    let parsed = match value.strip_prefix('-') {
        Some(magnitude) if magnitude != "0" => parse_canonical_u64(magnitude)
            .and_then(|magnitude| i64::try_from(magnitude).ok())
            .and_then(|magnitude| i32::try_from(-magnitude).ok()),
        Some(_) => None,
        None => parse_canonical_u64(value).and_then(|number| i32::try_from(number).ok()),
    };
    parsed.ok_or_else(|| invalid(field, value))
}

fn flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn check_config(config: &ScreenCaptureConfig) -> Result<(), CodecError> {
    config
        .validate()
        .map_err(|error| invalid(error.field(), error.code()))
}

fn encode_crop(crop: ScreenCaptureCrop) -> String {
    format!("{}.{}.{}.{}", crop.left, crop.top, crop.right, crop.bottom)
}

fn decode_crop(field: &'static str, value: &str) -> Result<ScreenCaptureCrop, CodecError> {
    let mut parts = value.split('.');
    let mut next = || -> Result<u16, CodecError> {
        canonical(field, parts.next().ok_or_else(|| invalid(field, value))?)
    };
    let crop = ScreenCaptureCrop {
        left: next()?,
        top: next()?,
        right: next()?,
        bottom: next()?,
    };
    if parts.next().is_some() {
        return Err(invalid(field, value));
    }
    Ok(crop)
}

fn slate(field: &'static str, value: &str) -> Result<StallSlate, CodecError> {
    StallSlate::from_token(value).ok_or_else(|| invalid(field, value))
}

fn title_match(field: &'static str, value: &str) -> Result<TitleMatch, CodecError> {
    TitleMatch::from_token(value).ok_or_else(|| invalid(field, value))
}

fn window_id(field: &'static str, value: &str) -> Result<Option<u64>, CodecError> {
    if value == "-" {
        Ok(None)
    } else {
        canonical(field, value).map(Some)
    }
}

/// Encodes a config's fields.
pub(super) fn encode_config(
    config: &ScreenCaptureConfig,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_config(config)?;
    push("target", config.target.kind_token().to_owned())?;
    match &config.target {
        ScreenCaptureTarget::Screen { index } => push("screen", index.to_string())?,
        ScreenCaptureTarget::Window(rule) => {
            push("app", rule.app.clone())?;
            push("title", rule.title.clone())?;
            push("title_match", rule.title_match.token().to_owned())?;
            if let Some(id) = rule.window_id {
                push("window_id", id.to_string())?;
            }
        }
    }
    push("crop", encode_crop(config.crop))?;
    push("cursor", u8::from(config.cursor).to_string())?;
    push("fps", config.fps.to_string())?;
    push("stall", config.slate.token().to_owned())
}

/// Decodes a config's fields.
pub(super) fn decode_config(
    fields: &mut dyn FieldSource,
) -> Result<ScreenCaptureConfig, CodecError> {
    let target = match fields.required("target")?.as_str() {
        "screen" => ScreenCaptureTarget::Screen {
            index: canonical("screen", &fields.required("screen")?)?,
        },
        "window" => ScreenCaptureTarget::Window(WindowRule {
            app: fields.required("app")?,
            title: fields.required("title")?,
            title_match: title_match("title_match", &fields.required("title_match")?)?,
            window_id: match fields.optional("window_id") {
                Some(id) => Some(canonical("window_id", &id)?),
                None => None,
            },
        }),
        other => return Err(invalid("target", other)),
    };
    let config = ScreenCaptureConfig {
        target,
        crop: decode_crop("crop", &fields.required("crop")?)?,
        cursor: flag("cursor", &fields.required("cursor")?)?,
        fps: canonical("fps", &fields.required("fps")?)?,
        slate: slate("stall", &fields.required("stall")?)?,
    };
    check_config(&config)?;
    Ok(config)
}

/// The compact form of a config.
pub(super) fn encode_compact(config: &ScreenCaptureConfig) -> Result<String, CodecError> {
    check_config(config)?;
    let mut text = format!(
        "{}~{}~{}~{}~",
        encode_crop(config.crop),
        u8::from(config.cursor),
        config.fps,
        config.slate.token()
    );
    match &config.target {
        ScreenCaptureTarget::Screen { index } => {
            write!(text, "screen~{index}").expect("writing to a string cannot fail");
        }
        ScreenCaptureTarget::Window(rule) => {
            write!(
                text,
                "window~{}~{}~{}~{}:{}{}",
                rule.title_match.token(),
                rule.window_id
                    .map_or_else(|| "-".to_owned(), |id| id.to_string()),
                rule.app.len(),
                rule.title.len(),
                rule.app,
                rule.title
            )
            .expect("writing to a string cannot fail");
        }
    }
    Ok(text)
}

/// Splits `count` `~`-ended parts off the front of `text`.
fn split_parts<const N: usize>(text: &str) -> Option<([&str; N], &str)> {
    let mut parts = [""; N];
    let mut after = text;
    for part in &mut parts {
        let (value, tail) = after.split_once('~')?;
        *part = value;
        after = tail;
    }
    Some((parts, after))
}

fn text_length(text: &str, max: usize) -> Option<usize> {
    parse_canonical_u64(text)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= max)
}

/// Decodes one compact config off the front of `text`: the config and
/// what follows it. A screen target's index ends at the first `,` or at
/// the end of `text`.
pub(super) fn decode_compact<'t>(
    field: &'static str,
    text: &'t str,
) -> Result<(ScreenCaptureConfig, &'t str), CodecError> {
    let bad = || invalid(field, text);
    let ([crop, cursor, fps, stall_token, kind], after) = split_parts::<5>(text).ok_or_else(bad)?;
    let (target, rest) = match kind {
        "screen" => {
            let end = after.find(',').unwrap_or(after.len());
            (
                ScreenCaptureTarget::Screen {
                    index: canonical(field, &after[..end])?,
                },
                &after[end..],
            )
        }
        "window" => {
            let ([match_token, id, app_length], after) = split_parts::<3>(after).ok_or_else(bad)?;
            let (title_length, after) = after.split_once(':').ok_or_else(bad)?;
            let app_length =
                text_length(app_length, MAX_SCREEN_CAPTURE_APP_BYTES).ok_or_else(bad)?;
            let title_length =
                text_length(title_length, MAX_SCREEN_CAPTURE_TITLE_BYTES).ok_or_else(bad)?;
            let app = after.get(..app_length).ok_or_else(bad)?;
            let after = &after[app_length..];
            let title = after.get(..title_length).ok_or_else(bad)?;
            (
                ScreenCaptureTarget::Window(WindowRule {
                    app: app.to_owned(),
                    title: title.to_owned(),
                    title_match: title_match(field, match_token)?,
                    window_id: window_id(field, id)?,
                }),
                &after[title_length..],
            )
        }
        _ => return Err(bad()),
    };
    let config = ScreenCaptureConfig {
        target,
        crop: decode_crop(field, crop)?,
        cursor: flag(field, cursor)?,
        fps: canonical(field, fps)?,
        slate: slate(field, stall_token)?,
    };
    check_config(&config)?;
    Ok((config, rest))
}

fn state_parts(state: ScreenCaptureState) -> (&'static str, u32, u32) {
    match state {
        ScreenCaptureState::Reopening {
            attempt,
            next_in_ms,
        } => ("reopening", attempt, next_in_ms),
        other => (other.token(), 0, 0),
    }
}

fn parse_state(
    field: &'static str,
    token: &str,
    attempt: u32,
    next_in_ms: u32,
) -> Result<ScreenCaptureState, CodecError> {
    let state = match token {
        "idle" => ScreenCaptureState::Idle,
        "opening" => ScreenCaptureState::Opening,
        "live" => ScreenCaptureState::Live,
        "target_gone" => ScreenCaptureState::TargetGone,
        "unsupported" => ScreenCaptureState::Unsupported,
        "failed" => ScreenCaptureState::Failed,
        "reopening" if attempt > 0 => {
            return Ok(ScreenCaptureState::Reopening {
                attempt,
                next_in_ms,
            });
        }
        _ => return Err(invalid(field, token)),
    };
    if attempt != 0 || next_in_ms != 0 {
        return Err(invalid(field, token));
    }
    Ok(state)
}

fn check_error(field: &'static str, error: Option<&str>) -> Result<(), CodecError> {
    match error {
        Some(error)
            if error.is_empty()
                || error.len() > MAX_SCREEN_CAPTURE_ERROR_BYTES
                || error.chars().any(char::is_control) =>
        {
            Err(invalid(field, error))
        }
        _ => Ok(()),
    }
}

fn check_status(field: &'static str, status: &ScreenCaptureStatus) -> Result<(), CodecError> {
    if status.window_id == Some(0) {
        return Err(invalid(field, "window_id 0"));
    }
    if let ScreenCaptureState::Reopening { attempt: 0, .. } = status.state {
        return Err(invalid(field, "reopening attempt 0"));
    }
    Ok(())
}

/// Encodes a `screen_capture_status` runtime event's fields after
/// `event`.
pub(super) fn encode_status(
    input: WireInputId,
    status: &ScreenCaptureStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_error("last_error", status.last_error.as_deref())?;
    check_status("state", status)?;
    let (state, attempt, next_in_ms) = state_parts(status.state);
    push("input", input.to_string())?;
    push("state", state.to_owned())?;
    if state == "reopening" {
        push("attempt", attempt.to_string())?;
        push("next_in_ms", next_in_ms.to_string())?;
    }
    push("reopens", status.reopens.to_string())?;
    push("width", status.width.to_string())?;
    push("height", status.height.to_string())?;
    if let Some(id) = status.window_id {
        push("window_id", id.to_string())?;
    }
    if let Some(error) = &status.last_error {
        push("last_error", error.clone())?;
    }
    Ok(())
}

/// Decodes a `screen_capture_status` runtime event's fields.
pub(super) fn decode_status(
    fields: &mut dyn FieldSource,
) -> Result<(WireInputId, ScreenCaptureStatus), CodecError> {
    let input_text = fields.required("input")?;
    let input = parse_canonical_input(&input_text).ok_or_else(|| invalid("input", input_text))?;
    let state = fields.required("state")?;
    let (attempt, next_in_ms) = if state == "reopening" {
        (
            canonical("attempt", &fields.required("attempt")?)?,
            canonical("next_in_ms", &fields.required("next_in_ms")?)?,
        )
    } else {
        (0, 0)
    };
    let status = ScreenCaptureStatus {
        state: parse_state("state", &state, attempt, next_in_ms)?,
        reopens: canonical("reopens", &fields.required("reopens")?)?,
        width: canonical("width", &fields.required("width")?)?,
        height: canonical("height", &fields.required("height")?)?,
        window_id: match fields.optional("window_id") {
            Some(id) => Some(canonical("window_id", &id)?),
            None => None,
        },
        last_error: fields.optional("last_error"),
    };
    check_error("last_error", status.last_error.as_deref())?;
    check_status("window_id", &status)?;
    Ok((input, status))
}

/// Encodes a snapshot's `screen_capture_inputs`.
pub(super) fn encode_entries(entries: &[ScreenCaptureEntry]) -> Result<String, CodecError> {
    if entries.len() > MAX_SCREEN_CAPTURE_INPUTS {
        return Err(CodecError::TooManyItems(ENTRIES));
    }
    let mut encoded = String::new();
    let mut previous = None;
    for entry in entries {
        if previous.is_some_and(|previous| previous >= entry.input.get()) {
            return Err(invalid(ENTRIES, entry.input.to_string()));
        }
        previous = Some(entry.input.get());
        // Error text travels only in `screen_capture_status`.
        if entry.status.last_error.is_some() {
            return Err(invalid(ENTRIES, "last_error"));
        }
        check_status(ENTRIES, &entry.status)?;
        let (state, attempt, next_in_ms) = state_parts(entry.status.state);
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{state}~{attempt}~{next_in_ms}~{}~{}~{}~{}~{}",
            entry.input,
            entry.status.reopens,
            entry.status.width,
            entry.status.height,
            entry
                .status
                .window_id
                .map_or_else(|| "-".to_owned(), |id| id.to_string()),
            encode_compact(&entry.config)?,
        )
        .expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Decodes a snapshot's `screen_capture_inputs`, the exact inverse of
/// [`encode_entries`].
pub(super) fn decode_entries(value: &str) -> Result<Vec<ScreenCaptureEntry>, CodecError> {
    let mut entries: Vec<ScreenCaptureEntry> = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if !entries.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(ENTRIES, value))?;
        }
        if entries.len() == MAX_SCREEN_CAPTURE_INPUTS {
            return Err(CodecError::TooManyItems(ENTRIES));
        }
        let (
            [
                input,
                state,
                attempt,
                next_in_ms,
                reopens,
                width,
                height,
                id,
            ],
            after,
        ) = split_parts::<8>(rest).ok_or_else(|| invalid(ENTRIES, rest))?;
        let input = parse_canonical_input(input).ok_or_else(|| invalid(ENTRIES, rest))?;
        if entries
            .last()
            .is_some_and(|previous| previous.input.get() >= input.get())
        {
            return Err(invalid(ENTRIES, rest));
        }
        let status = ScreenCaptureStatus {
            state: parse_state(
                ENTRIES,
                state,
                canonical(ENTRIES, attempt)?,
                canonical(ENTRIES, next_in_ms)?,
            )?,
            reopens: canonical(ENTRIES, reopens)?,
            width: canonical(ENTRIES, width)?,
            height: canonical(ENTRIES, height)?,
            window_id: window_id(ENTRIES, id)?,
            last_error: None,
        };
        check_status(ENTRIES, &status)?;
        let (config, after) = decode_compact(ENTRIES, after)?;
        rest = after;
        entries.push(ScreenCaptureEntry {
            input,
            config,
            status,
        });
    }
    Ok(entries)
}

/// Checks an inventory against the exact contract.
fn check_inventory(inventory: &ScreenCaptureInventory) -> Result<(), CodecError> {
    check_error("error", inventory.error.as_deref())?;
    if !is_screen_capture_text(&inventory.backend, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES) {
        return Err(invalid("backend", inventory.backend.clone()));
    }
    if !inventory.supported
        && (!inventory.screens.is_empty()
            || !inventory.windows.is_empty()
            || !inventory.backend.is_empty())
    {
        return Err(invalid("supported", "0"));
    }
    if inventory.screens.len() > MAX_SCREEN_CAPTURE_SCREENS {
        return Err(CodecError::TooManyItems(SCREENS));
    }
    if inventory.windows.len() > MAX_SCREEN_CAPTURE_WINDOWS {
        return Err(CodecError::TooManyItems(WINDOWS));
    }
    for (index, screen) in inventory.screens.iter().enumerate() {
        if !screen.is_valid() || (index > 0 && inventory.screens[index - 1].index >= screen.index) {
            return Err(invalid(SCREENS, screen.index.to_string()));
        }
    }
    for (index, window) in inventory.windows.iter().enumerate() {
        if !window.is_valid()
            || inventory.windows[..index]
                .iter()
                .any(|listed| listed.id == window.id)
        {
            return Err(invalid(WINDOWS, window.id.to_string()));
        }
    }
    Ok(())
}

/// Encodes an inventory as `supported`, `backend`, optional `error`,
/// `screens`, and `windows`.
pub(super) fn encode_inventory(
    inventory: &ScreenCaptureInventory,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    check_inventory(inventory)?;
    push("supported", u8::from(inventory.supported).to_string())?;
    push("backend", inventory.backend.clone())?;
    if let Some(error) = &inventory.error {
        push("error", error.clone())?;
    }
    let mut screens = String::new();
    for screen in &inventory.screens {
        if !screens.is_empty() {
            screens.push(',');
        }
        write!(
            screens,
            "{}~{}~{}~{}~{}~{}~{}:{}",
            screen.index,
            u8::from(screen.primary),
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            screen.name.len(),
            screen.name
        )
        .expect("writing to a string cannot fail");
    }
    push(SCREENS, screens)?;
    let mut windows = String::new();
    for window in &inventory.windows {
        if !windows.is_empty() {
            windows.push(',');
        }
        write!(
            windows,
            "{}~{}~{}~{}~{}~{}~{}~{}:{}{}",
            window.id,
            window
                .pid
                .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
            window.x,
            window.y,
            window.width,
            window.height,
            window.app.len(),
            window.title.len(),
            window.app,
            window.title
        )
        .expect("writing to a string cannot fail");
    }
    push(WINDOWS, windows)
}

/// Decodes an inventory, the exact inverse of [`encode_inventory`].
pub(super) fn decode_inventory(
    fields: &mut dyn FieldSource,
) -> Result<ScreenCaptureInventory, CodecError> {
    let supported = flag("supported", &fields.required("supported")?)?;
    let backend = fields.required("backend")?;
    let error = fields.optional("error");
    let screens_text = fields.required(SCREENS)?;
    let mut screens = Vec::new();
    let mut rest = screens_text.as_str();
    while !rest.is_empty() {
        if !screens.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(SCREENS, screens_text.clone()))?;
        }
        if screens.len() == MAX_SCREEN_CAPTURE_SCREENS {
            return Err(CodecError::TooManyItems(SCREENS));
        }
        let (screen, after) = split_screen(rest)?;
        screens.push(screen);
        rest = after;
    }
    let windows_text = fields.required(WINDOWS)?;
    let mut windows = Vec::new();
    let mut rest = windows_text.as_str();
    while !rest.is_empty() {
        if !windows.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid(WINDOWS, windows_text.clone()))?;
        }
        if windows.len() == MAX_SCREEN_CAPTURE_WINDOWS {
            return Err(CodecError::TooManyItems(WINDOWS));
        }
        let (window, after) = split_window(rest)?;
        windows.push(window);
        rest = after;
    }
    let inventory = ScreenCaptureInventory {
        supported,
        backend,
        screens,
        windows,
        error,
    };
    check_inventory(&inventory)?;
    Ok(inventory)
}

fn split_screen(text: &str) -> Result<(ScreenInfo, &str), CodecError> {
    let bad = || invalid(SCREENS, text);
    let ([index, primary, x, y, width, height], after) = split_parts::<6>(text).ok_or_else(bad)?;
    let (length, after) = after.split_once(':').ok_or_else(bad)?;
    let length = text_length(length, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES).ok_or_else(bad)?;
    let name = after.get(..length).ok_or_else(bad)?;
    Ok((
        ScreenInfo {
            index: canonical(SCREENS, index)?,
            name: name.to_owned(),
            x: signed(SCREENS, x)?,
            y: signed(SCREENS, y)?,
            width: canonical(SCREENS, width)?,
            height: canonical(SCREENS, height)?,
            primary: flag(SCREENS, primary)?,
        },
        &after[length..],
    ))
}

fn split_window(text: &str) -> Result<(WindowInfo, &str), CodecError> {
    let bad = || invalid(WINDOWS, text);
    let ([id, pid, x, y, width, height, app_length], after) =
        split_parts::<7>(text).ok_or_else(bad)?;
    let (title_length, after) = after.split_once(':').ok_or_else(bad)?;
    let app_length = text_length(app_length, MAX_SCREEN_CAPTURE_APP_BYTES).ok_or_else(bad)?;
    let title_length = text_length(title_length, MAX_SCREEN_CAPTURE_TITLE_BYTES).ok_or_else(bad)?;
    let app = after.get(..app_length).ok_or_else(bad)?;
    let after = &after[app_length..];
    let title = after.get(..title_length).ok_or_else(bad)?;
    Ok((
        WindowInfo {
            id: canonical(WINDOWS, id)?,
            app: app.to_owned(),
            title: title.to_owned(),
            pid: if pid == "-" {
                None
            } else {
                Some(canonical(WINDOWS, pid)?)
            },
            x: signed(WINDOWS, x)?,
            y: signed(WINDOWS, y)?,
            width: canonical(WINDOWS, width)?,
            height: canonical(WINDOWS, height)?,
        },
        &after[title_length..],
    ))
}
