//! Fullscreen display outputs (Protocol 2.42, OR-011).
//!
//! A project has two display-output slots. Each configured slot shows one
//! picture (`program`, `preview`, or `output:<id>`) borderless and
//! cursor-hidden on one display, chosen by platform id (optionally with the
//! name it falls back to when the platform re-numbers the display), by
//! name, or by desktop index.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::SetDisplayOutput`] | `set_display_output` | `slot`, `display`, `source`, `mode`, `scaling`, `hdr` | project edit |
//! | [`crate::CommandPayload::ClearDisplayOutput`] | `clear_display_output` | `slot` | project edit |
//!
//! Both are padlocked, undoable project edits announced by the durable
//! `event=display_output_changed` (`slot`, `configured=0|1`, and the
//! configuration when configured). A daemon applies them live: a source,
//! scaling, mode, or HDR change keeps the output on its display, and a new
//! display rebinds it.
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=display_output_status` reports one slot's state (`waiting` with a
//! `reason`, or `active` with the display id, name, mode, mode fallback, and
//! HDR state) whenever it changes, and `event=display_inventory` reports
//! the connected displays whenever they change. A client asks for the
//! inventory with `display_inventory_request` and receives
//! `display_inventory` with the same `request_id`.
//!
//! A snapshot carries `display_outputs` (each configured slot) and
//! `display_output_states` (each configured slot's runtime state).

use crate::{CodecError, WireOutputId};

pub use fm_types::display::{
    DisplayId, DisplayMode, DisplayOutputConfig, DisplayOutputSlot, DisplayOutputSource,
    DisplaySelector, HdrCapability, HdrRequest, HdrState, MAX_DISPLAY_OUTPUTS, ModeRequest,
    WaitReason,
};

/// Most displays one inventory lists.
pub const MAX_INVENTORY_DISPLAYS: usize = 16;
/// Most modes one inventory display lists.
pub const MAX_INVENTORY_DISPLAY_MODES: usize = 128;
/// Longest inventory enumeration error, in UTF-8 bytes.
pub const MAX_DISPLAY_INVENTORY_ERROR_BYTES: usize = 256;

/// One configured display-output slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputEntry {
    pub slot: DisplayOutputSlot,
    pub config: DisplayOutputConfig,
}

/// The display an active output shows on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireActiveDisplay {
    pub id: DisplayId,
    pub name: String,
    /// The mode the output is driven at.
    pub mode: DisplayMode,
    /// True when an exact mode was requested but the display does not offer
    /// it, so the output runs at the display's current mode.
    pub mode_fallback: bool,
    pub hdr: HdrState,
}

/// One slot's runtime state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireDisplayOutputState {
    Waiting(WaitReason),
    Active(WireActiveDisplay),
}

/// One configured slot's runtime state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputStatus {
    pub slot: DisplayOutputSlot,
    pub state: WireDisplayOutputState,
}

/// One connected display as the daemon enumerates it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayInventoryEntry {
    pub id: DisplayId,
    pub name: String,
    /// Desktop origin in physical pixels.
    pub x: i32,
    pub y: i32,
    pub current_mode: DisplayMode,
    /// Every advertised mode, at most [`MAX_INVENTORY_DISPLAY_MODES`].
    pub modes: Vec<DisplayMode>,
    pub hdr: HdrCapability,
}

/// The connected displays.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayInventory {
    /// Whether the daemon can present display outputs at all (see
    /// [`WaitReason::NoPresenter`]). A daemon without a presenter lists no
    /// displays.
    pub presenter: bool,
    /// Why the last enumeration failed, if it did.
    pub error: Option<String>,
    /// Displays in desktop order, at most [`MAX_INVENTORY_DISPLAYS`].
    pub displays: Vec<DisplayInventoryEntry>,
}

/// A client's request for the current display inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayInventoryRequest {
    pub request_id: String,
}

/// The current display inventory, answering a
/// [`DisplayInventoryRequest`] with the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayInventoryMessage {
    pub request_id: String,
    pub inventory: DisplayInventory,
}

/// Whether `source` names one of `outputs` (or is Program or Preview).
#[must_use]
pub fn display_source_is_known(source: DisplayOutputSource, outputs: &[WireOutputId]) -> bool {
    match source {
        DisplayOutputSource::Program | DisplayOutputSource::Preview => true,
        DisplayOutputSource::Output(output) => {
            outputs.iter().any(|known| known.to_domain() == output)
        }
    }
}

pub(crate) fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}
