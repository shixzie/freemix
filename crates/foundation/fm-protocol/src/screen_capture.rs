//! Screen and window capture inputs (IN-011).
//!
//! A screen capture input shows a screen (monitor) of the server's desktop
//! or one application window, captured on the server.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::AddInput`] | `input_add` | `input`, `name`, `kind=screen_capture`, config fields | project edit |
//!
//! Config fields: `target` (`screen|window`); for a screen `screen` (its
//! zero-based index); for a window `app`, `title`, `title_match`
//! (`exact|contains|any`), and an optional `window_id` hint; then `crop`
//! (`<left>.<top>.<right>.<bottom>` insets in target pixels), `cursor`
//! (`0|1`), `fps` (1 to 60), and `stall` (`freeze|black`, what the input
//! shows while its target is gone). See [`ScreenCaptureConfig`].
//!
//! The add is an undoable project edit that honours the project padlock;
//! `event=input_added` carries `kind=screen_capture` and the config
//! fields. A screen capture input's configuration is changed by replacing
//! its source (`input_replace_source`) with
//! `screen_capture:<compact config>`. A daemon that cannot realize screen
//! capture inputs refuses a live add with the typed reason
//! `runtime_unsupported`.
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=screen_capture_status` reports one input's
//! [`ScreenCaptureStatus`] whenever it changes: `input`, `state`
//! (`idle|opening|live|target_gone|reopening|unsupported|failed`),
//! `attempt` and `next_in_ms` (only for `reopening`), `reopens`, `width`
//! and `height` (the captured target's size, zero when none), an optional
//! `window_id`, and an optional `last_error`. A snapshot carries
//! `screen_capture_inputs`: every screen capture input, ascending, with its
//! config and latest status (without the error text).
//!
//! A client lists what the server can capture with
//! `screen_capture_targets_request` (`request_id`) and receives
//! `screen_capture_targets` with the same `request_id`: `supported`
//! (`0|1`), `backend`, an optional `error`, `screens`, and `windows`
//! ([`ScreenCaptureInventory`]). The server lists off its frame loop and
//! reuses a listing younger than a second, so it may answer only a peer's
//! newest request.
//!
//! * `screens` is a comma-separated list, strictly ascending by index, of
//!   `<index>~<primary 0|1>~<x>~<y>~<width>~<height>~<name length>:<name>`.
//! * `windows` is a comma-separated list, each id once, of
//!   `<id>~<pid|->~<x>~<y>~<width>~<height>~<app length>~<title length>:
//!   <app><title>`.
//!
//! Positions are signed decimals; every text is written raw after its
//! canonical byte length.

pub use fm_types::screen_capture::{
    DEFAULT_SCREEN_CAPTURE_FPS, MAX_SCREEN_CAPTURE_APP_BYTES, MAX_SCREEN_CAPTURE_CROP,
    MAX_SCREEN_CAPTURE_ERROR_BYTES, MAX_SCREEN_CAPTURE_FPS, MAX_SCREEN_CAPTURE_INPUTS,
    MAX_SCREEN_CAPTURE_SCREEN_INDEX, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES,
    MAX_SCREEN_CAPTURE_SCREENS, MAX_SCREEN_CAPTURE_TITLE_BYTES, MAX_SCREEN_CAPTURE_WINDOWS,
    MIN_SCREEN_CAPTURE_FPS, ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureError,
    ScreenCaptureInventory, ScreenCaptureState, ScreenCaptureStatus, ScreenCaptureTarget,
    ScreenInfo, TitleMatch, WindowInfo, WindowRule, bound_screen_capture_error,
};

use crate::WireInputId;

/// A client's request for what the server can capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenCaptureTargetsRequest {
    pub request_id: String,
}

/// What the server can capture, answering a
/// [`ScreenCaptureTargetsRequest`] with the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenCaptureTargetsMessage {
    pub request_id: String,
    pub inventory: ScreenCaptureInventory,
}

/// One screen capture input in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenCaptureEntry {
    pub input: WireInputId,
    pub config: ScreenCaptureConfig,
    /// The latest status; a snapshot carries no error text.
    pub status: ScreenCaptureStatus,
}
