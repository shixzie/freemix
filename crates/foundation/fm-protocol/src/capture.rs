//! Local capture inputs (Protocol 2.47, IN-001).
//!
//! A capture input acquires video (and optionally audio) from a local
//! camera or capture card named by its adapter-qualified stable key, with
//! [`CaptureSettings`]: the requested device format (or the runtime's pick
//! nearest the program), the audio it hears, and its signal-loss slate.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::AddInput`] | `input_add` | `input`, `name`, `kind=capture`, `device`, the settings fields | project edit |
//! | [`crate::CommandPayload::SetCaptureSettings`] | `input_capture_set` | `input`, the settings fields | project edit |
//!
//! Settings fields: `format` (a [`CaptureFormat`] token such as
//! `1920x1080@30000/1001:yuyv:tff`, or `-` for the runtime's pick), `audio`
//! (`none`, `paired`, or `alsa:<device>`), and `loss` (`hold|black`).
//! `device` is the stable key (1 to [`MAX_CAPTURE_KEY_BYTES`] bytes without
//! control characters). At most [`MAX_CAPTURE_INPUTS`] capture inputs per
//! show.
//!
//! Both are undoable project edits that honour the project and input
//! padlocks. An added capture input is announced by `event=input_added`
//! with `kind=capture` and the same fields; `input_capture_set` by the
//! durable `event=capture_settings_changed` (`input` and the settings
//! fields). A daemon applies a settings change live by reopening the
//! device.
//!
//! Runtime state is not durable. `runtime_event` with `event=capture_status`
//! reports one input's [`CaptureStatus`] whenever it changes: `state`
//! (`idle|starting|live|signal_lost|failed`), `reason`
//! (`unplugged|stalled|no_signal|error` or `-`), `device` (the node
//! acquired, or `-`), `format` (the negotiated format token, or `-`),
//! `audio` (`0|1`), the `frames`, `losses`, `recoveries`, and
//! `renegotiations` counts, and `error` (the last error, at most
//! [`MAX_CAPTURE_ERROR_BYTES`], or `-`). A snapshot carries
//! `capture_inputs`: every capture input, ascending, with its stable key,
//! settings, and latest status (without the error text).
//!
//! The capture devices a daemon can open form its [`CaptureInventory`]. A
//! client asks with `capture_devices_request` (`request_id`) and receives
//! `capture_devices` with the same `request_id`; `runtime_event` with
//! `event=capture_devices` reports the inventory whenever it changes. Both
//! carry `backend` (a lowercase token such as `v4l2`, or `-` when the
//! daemon realizes no capture inputs), `error` (or `-`), and `devices`:
//! [`fit_capture_inventory`] bounds an inventory to what one line carries.

pub use fm_types::capture::{
    CaptureAudio, CaptureDevice, CaptureError, CaptureFormat, CaptureInputConfig, CaptureInventory,
    CaptureLossReason, CaptureLossSlate, CapturePixelFormat, CaptureScan, CaptureSettings,
    CaptureSignalState, CaptureStatus, MAX_CAPTURE_DEVICE_FORMATS, MAX_CAPTURE_DEVICES,
    MAX_CAPTURE_ERROR_BYTES, MAX_CAPTURE_INPUTS, MAX_CAPTURE_KEY_BYTES, MAX_CAPTURE_TEXT_BYTES,
    bounded_capture_text, validate_stable_key,
};

use crate::WireInputId;

/// The longest capture backend token, in bytes.
pub const MAX_CAPTURE_BACKEND_BYTES: usize = 32;

/// One capture input in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureInputEntry {
    pub input: WireInputId,
    /// The device's stable key and the input's settings.
    pub config: CaptureInputConfig,
    /// The latest status; a snapshot carries no error text.
    pub status: CaptureStatus,
}

/// A client's request for the daemon's capture devices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureDevicesRequest {
    pub request_id: String,
}

/// The daemon's capture devices, answering a [`CaptureDevicesRequest`] with
/// the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureDevicesMessage {
    pub request_id: String,
    pub inventory: CaptureInventory,
}

/// Bytes of a `capture_devices` line kept free for the runtime-event
/// envelope (server identity, revision, generation, sequence).
const INVENTORY_ENVELOPE_RESERVE: usize = 2 * 1024;

/// Whether `backend` is a usable backend token: 1 to
/// [`MAX_CAPTURE_BACKEND_BYTES`] bytes of lowercase ASCII letters, digits,
/// and `_`, starting with a letter.
#[must_use]
pub fn is_capture_backend_token(backend: &str) -> bool {
    backend.len() <= MAX_CAPTURE_BACKEND_BYTES
        && backend
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && backend
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Bounds `text` for the wire: control characters replaced, trimmed,
/// bounded to `max` bytes, and `None` when nothing (or only the `-`
/// placeholder) is left.
fn wire_text(text: Option<&str>, max: usize) -> Option<String> {
    text.map(|text| bounded_capture_text(text, max))
        .filter(|text| !text.is_empty() && text != "-")
}

/// `status` bounded to what `event=capture_status` carries: the device
/// node and error text bounded (an empty or `-` text is dropped).
#[must_use]
pub fn wire_capture_status(status: &CaptureStatus) -> CaptureStatus {
    CaptureStatus {
        device: wire_text(status.device.as_deref(), MAX_CAPTURE_TEXT_BYTES),
        last_error: wire_text(status.last_error.as_deref(), MAX_CAPTURE_ERROR_BYTES),
        ..status.clone()
    }
}

/// `inventory` bounded to what one `capture_devices` line carries, with
/// room left for the longest request id and the runtime-event envelope:
/// texts bounded, devices with an unusable key or name or path dropped,
/// devices sorted by stable key (the first of equal keys kept), at most
/// [`MAX_CAPTURE_DEVICES`] devices and [`MAX_CAPTURE_DEVICE_FORMATS`]
/// valid formats each, and then formats dropped from the end of the
/// longest lists until the line fits.
#[must_use]
pub fn fit_capture_inventory(inventory: &CaptureInventory) -> CaptureInventory {
    let mut devices = inventory
        .devices
        .iter()
        .filter(|device| validate_stable_key(&device.stable_key).is_ok())
        .map(|device| CaptureDevice {
            stable_key: device.stable_key.clone(),
            name: bounded_capture_text(&device.name, MAX_CAPTURE_TEXT_BYTES),
            path: bounded_capture_text(&device.path, MAX_CAPTURE_TEXT_BYTES),
            driver: bounded_capture_text(&device.driver, MAX_CAPTURE_TEXT_BYTES),
            bus: bounded_capture_text(&device.bus, MAX_CAPTURE_TEXT_BYTES),
            audio: wire_text(device.audio.as_deref(), MAX_CAPTURE_TEXT_BYTES),
            formats: device
                .formats
                .iter()
                .copied()
                .filter(|format| format.validate().is_ok())
                .take(MAX_CAPTURE_DEVICE_FORMATS)
                .collect(),
        })
        .filter(|device| !device.name.is_empty() && !device.path.is_empty())
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    devices.dedup_by(|later, earlier| later.stable_key == earlier.stable_key);
    devices.truncate(MAX_CAPTURE_DEVICES);
    let mut fitted = CaptureInventory {
        backend: inventory
            .backend
            .clone()
            .filter(|backend| is_capture_backend_token(backend)),
        error: wire_text(inventory.error.as_deref(), MAX_CAPTURE_ERROR_BYTES),
        devices,
    };
    loop {
        let line = crate::encode_line(&crate::WireMessage::CaptureDevices(CaptureDevicesMessage {
            request_id: "r".repeat(crate::codec::MAX_REQUEST_ID_BYTES),
            inventory: fitted.clone(),
        }));
        let excess = match line {
            Ok(line) => {
                (line.len() + INVENTORY_ENVELOPE_RESERVE).saturating_sub(crate::MAX_LINE_BYTES)
            }
            // Only a too-long line fails once the values are bounded.
            Err(_) => crate::MAX_LINE_BYTES / 8,
        };
        if excess == 0 {
            return fitted;
        }
        let mut freed = 0;
        while freed < excess {
            let Some(device) = fitted
                .devices
                .iter_mut()
                .filter(|device| !device.formats.is_empty())
                .max_by_key(|device| device.formats.len())
            else {
                // No formats left: drop the last device instead.
                if fitted.devices.pop().is_none() {
                    return fitted;
                }
                break;
            };
            let Some(format) = device.formats.pop() else {
                break;
            };
            // Each format costs its token, three bytes per escaped
            // separator, and its escaped `;`.
            let token = format.token();
            let escaped = token
                .bytes()
                .filter(|byte| !byte.is_ascii_alphanumeric())
                .count();
            freed += token.len() + 2 * escaped + 3;
        }
    }
}
