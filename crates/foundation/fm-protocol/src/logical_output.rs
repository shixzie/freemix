//! Logical output routing on the wire (SW-012, Protocol 2.157).
//!
//! The show's four logical outputs are carried in a snapshot's
//! `logical_outputs` list and changed with `logical_output_set` or
//! `logical_output_clear`. Each entry is
//! `<slot>~<output>~<source>~<overlays>~<audio>`:
//!
//! * `source` is `program`, `preview`, `input:<id>`, `mix:<1..=16>`, or
//!   `multiview:<cell>/<cell>/...` with at most 16 cells. A cell is
//!   `program`, `preview`, `input:<id>`, `mix:<1..=16>`, or
//!   `output:<slot>`.
//! * `overlays` is `all`, `clean`, `routed`, or `channels:<1/3/...>` with
//!   at most eight one-based channels in ascending order.
//! * `audio` is `master`, `follow_video`, or `bus:<id>`.
//!
//! Entries are strictly ascending by slot. The engine and project validate
//! references, duplicates, and multiview recursion.

use core::fmt;
use core::num::NonZeroU128;

use fm_types::{BusId, InputId, OutputId};

/// Number of independently routed logical outputs.
pub const MAX_WIRE_LOGICAL_OUTPUTS: usize = 4;
/// Maximum cells in one logical-output multiview.
pub const MAX_WIRE_LOGICAL_MULTIVIEW_CELLS: usize = 16;
/// Highest valid mix number (Main is 1).
pub const MAX_WIRE_LOGICAL_MIX: u8 = 16;

/// One logical output slot, numbered 1 through 4.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireLogicalOutputSlot(u8);

impl WireLogicalOutputSlot {
    pub const ALL: [Self; MAX_WIRE_LOGICAL_OUTPUTS] = [Self(1), Self(2), Self(3), Self(4)];

    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= MAX_WIRE_LOGICAL_OUTPUTS {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

impl fmt::Display for WireLogicalOutputSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One cell of a logical-output multiview.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WireLogicalOutputCell {
    Program,
    Preview,
    Input(super::WireInputId),
    Mix(u8),
    Output(WireLogicalOutputSlot),
}

/// What a logical output shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireLogicalOutputSource {
    Program,
    Preview,
    Input(super::WireInputId),
    Mix(u8),
    MultiView(Vec<WireLogicalOutputCell>),
}

/// Which overlay channels a logical output composites.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WireLogicalOutputOverlays {
    #[default]
    All,
    Clean,
    Channels(Vec<crate::WireOverlayChannelId>),
    Routed,
}

/// Which audio a logical output carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WireLogicalAudioMap {
    #[default]
    Master,
    Bus(BusId),
    FollowVideo,
}

/// One configured logical output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireLogicalOutputConfig {
    pub slot: WireLogicalOutputSlot,
    pub output: crate::WireOutputId,
    pub source: WireLogicalOutputSource,
    pub overlays: WireLogicalOutputOverlays,
    pub audio: WireLogicalAudioMap,
}

/// Parses a canonical logical output slot from its one-based number.
#[must_use]
pub fn logical_output_slot_from_number(number: u8) -> Option<WireLogicalOutputSlot> {
    WireLogicalOutputSlot::new(number)
}

/// Parses a canonical input id.
#[must_use]
pub fn logical_input_from_number(number: u128) -> Option<super::WireInputId> {
    NonZeroU128::new(number).map(super::WireInputId::new)
}

/// Parses a canonical output id.
#[must_use]
pub fn logical_output_from_number(number: u128) -> Option<crate::WireOutputId> {
    NonZeroU128::new(number).map(crate::WireOutputId::new)
}

/// Parses a canonical audio bus id.
#[must_use]
pub fn logical_bus_from_number(number: u128) -> Option<BusId> {
    NonZeroU128::new(number).map(BusId::new)
}

/// The domain input id of a wire input.
#[must_use]
pub fn logical_input_to_domain(input: super::WireInputId) -> InputId {
    input.to_domain()
}

/// The domain output id of a wire output.
#[must_use]
pub fn logical_output_to_domain(output: crate::WireOutputId) -> OutputId {
    output.to_domain()
}
