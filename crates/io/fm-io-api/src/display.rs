//! Display enumeration and fullscreen display-output contracts.
//!
//! A [`DisplayProvider`] enumerates the host's connected displays as portable
//! [`DisplayInfo`] records: a platform-stable [`DisplayId`], a human name,
//! the desktop position, the current and available [`DisplayMode`]s, and
//! [`HdrCapability`] where the platform exposes it (`Unknown` otherwise).
//!
//! A [`DisplayOutputSet`] holds up to [`MAX_DISPLAY_OUTPUTS`] independent
//! [`DisplayOutputConfig`]s, each choosing a display, a
//! [`DisplayOutputSource`], a [`ModeRequest`], an [`OutputScaling`] policy,
//! and an [`HdrRequest`]. Every display output is borderless and suppresses
//! the cursor; neither is optional.
//!
//! [`DisplayOutputTracker`] is the platform-neutral hot-plug state machine.
//! Each [`DisplayOutputTracker::reconcile`] call takes the newest inventory
//! and returns the outputs whose [`DisplayOutputState`] changed. An output
//! first binds by its selector; once bound it follows that display by stable
//! id, then by unique name (rebinding to a new id when a platform re-numbers
//! a returning display), and never jumps to another display by index. A
//! missing display moves the output to [`WaitReason::DisplayAbsent`] and the
//! output reattaches when the display returns. Two outputs never share one
//! display: earlier outputs claim first and a later output whose display is
//! claimed waits with [`WaitReason::DisplayInUse`].

use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

pub use fm_types::OutputScaling;
pub use fm_types::display::{
    DisplayConfigError, DisplayId, DisplayMode, DisplayOutputConfig, DisplayOutputSlot,
    DisplayOutputSource, DisplaySelector, HdrCapability, HdrRequest, HdrState, MAX_DISPLAY_INDEX,
    MAX_DISPLAY_MODE_SIDE, MAX_DISPLAY_OUTPUTS, MAX_DISPLAY_REFRESH_MILLIHERTZ,
    MAX_DISPLAY_TEXT_BYTES, ModeRequest, WaitReason,
};

/// One connected display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayInfo {
    pub id: DisplayId,
    pub name: String,
    /// Desktop origin of the display in physical pixels.
    pub x: i32,
    pub y: i32,
    pub current_mode: DisplayMode,
    /// Every mode the display advertises; may be empty when unknown.
    pub modes: Vec<DisplayMode>,
    pub hdr: HdrCapability,
}

impl DisplayInfo {
    /// A display advertising only its current mode.
    #[must_use]
    pub fn new(id: DisplayId, name: impl Into<String>, current_mode: DisplayMode) -> Self {
        Self {
            id,
            name: name.into(),
            x: 0,
            y: 0,
            current_mode,
            modes: vec![current_mode],
            hdr: HdrCapability::Unknown,
        }
    }

    fn supports(&self, mode: DisplayMode) -> Option<DisplayMode> {
        if self.current_mode.matches(mode) {
            return Some(self.current_mode);
        }
        self.modes.iter().copied().find(|known| known.matches(mode))
    }
}

/// Sorts an inventory into the deterministic order used by index selection:
/// desktop position, then name, then id.
pub fn sort_displays(displays: &mut [DisplayInfo]) {
    displays.sort_by(|left, right| {
        (left.x, left.y, &left.name, &left.id).cmp(&(right.x, right.y, &right.name, &right.id))
    });
}

/// A platform display enumeration failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayEnumerationError(pub String);

impl fmt::Display for DisplayEnumerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "display enumeration failed: {}", self.0)
    }
}

impl std::error::Error for DisplayEnumerationError {}

/// Enumerates the currently connected displays.
pub trait DisplayProvider {
    /// Returns every connected display in any order.
    ///
    /// # Errors
    ///
    /// Returns a platform enumeration failure.
    fn displays(&mut self) -> Result<Vec<DisplayInfo>, DisplayEnumerationError>;
}

/// A validated, ordered set of one through [`MAX_DISPLAY_OUTPUTS`] outputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputSet(Vec<DisplayOutputConfig>);

impl DisplayOutputSet {
    /// Validates count and rejects two outputs that claim the same display
    /// (see [`DisplaySelector::conflicts_with`]).
    ///
    /// # Errors
    ///
    /// Returns [`DisplayConfigError::Empty`], [`DisplayConfigError::TooMany`],
    /// or [`DisplayConfigError::DuplicateSelector`].
    pub fn new(outputs: Vec<DisplayOutputConfig>) -> Result<Self, DisplayConfigError> {
        if outputs.is_empty() {
            return Err(DisplayConfigError::Empty);
        }
        if outputs.len() > MAX_DISPLAY_OUTPUTS {
            return Err(DisplayConfigError::TooMany {
                maximum: MAX_DISPLAY_OUTPUTS,
                actual: outputs.len(),
            });
        }
        for (index, output) in outputs.iter().enumerate() {
            if outputs[..index]
                .iter()
                .any(|earlier| earlier.display.conflicts_with(&output.display))
            {
                return Err(DisplayConfigError::DuplicateSelector(
                    output.display.clone(),
                ));
            }
        }
        Ok(Self(outputs))
    }

    #[must_use]
    pub fn outputs(&self) -> &[DisplayOutputConfig] {
        &self.0
    }
}

/// The display an active output is shown on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveDisplay {
    pub id: DisplayId,
    pub name: String,
    pub x: i32,
    pub y: i32,
    /// The mode the output is driven at.
    pub mode: DisplayMode,
    /// True when an exact mode was requested but the display does not offer it.
    pub mode_fallback: bool,
    pub hdr: HdrState,
}

/// One output's runtime state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayOutputState {
    Waiting(WaitReason),
    Active(ActiveDisplay),
}

impl DisplayOutputState {
    #[must_use]
    pub const fn active(&self) -> Option<&ActiveDisplay> {
        match self {
            Self::Active(active) => Some(active),
            Self::Waiting(_) => None,
        }
    }
}

/// A state transition reported by [`DisplayOutputTracker::reconcile`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputChange {
    /// Zero-based output position (the slot index).
    pub output: usize,
    pub previous: DisplayOutputState,
    pub current: DisplayOutputState,
}

impl DisplayOutputChange {
    /// True when the output left a display or moved to another one.
    #[must_use]
    pub fn detached(&self) -> bool {
        match (&self.previous, &self.current) {
            (DisplayOutputState::Active(_), DisplayOutputState::Waiting(_)) => true,
            (DisplayOutputState::Active(before), DisplayOutputState::Active(after)) => {
                before.id != after.id
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Binding {
    id: DisplayId,
    name: String,
}

#[derive(Clone, Debug)]
struct TrackedOutput {
    config: DisplayOutputConfig,
    binding: Option<Binding>,
    state: DisplayOutputState,
}

impl TrackedOutput {
    fn new(config: DisplayOutputConfig) -> Self {
        // An id remembered with its name follows the display exactly as a
        // bound output does: by id, then by unique name.
        let binding = match &config.display {
            DisplaySelector::IdOrName { id, name } => Some(Binding {
                id: id.clone(),
                name: name.clone(),
            }),
            _ => None,
        };
        Self {
            config,
            binding,
            state: DisplayOutputState::Waiting(WaitReason::DisplayAbsent),
        }
    }
}

enum Match {
    Found(usize),
    Absent,
    Ambiguous,
}

/// What [`DisplayOutputTracker::configure`] did to one slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayOutputReconfiguration {
    /// Nothing changed.
    Unchanged,
    /// Only the source, scaling, mode, or HDR request changed; the output
    /// keeps its display binding. Mode and HDR are re-resolved on the next
    /// [`DisplayOutputTracker::reconcile`].
    Retargeted,
    /// The slot was empty or chose another display: it waits for its
    /// display, which the next reconcile binds.
    Rebinding,
    /// The slot was cleared.
    Cleared,
}

/// The hot-plug and display-matching state machine for up to
/// [`MAX_DISPLAY_OUTPUTS`] output slots.
///
/// Slot `n` is output `n` of the [`DisplayOutputSet`] the tracker starts
/// with; [`Self::configure`] fills, retargets, or clears a slot live.
#[derive(Clone, Debug)]
pub struct DisplayOutputTracker {
    outputs: Vec<Option<TrackedOutput>>,
    presenter_supports_hdr: bool,
}

impl DisplayOutputTracker {
    /// Starts every output waiting for its display.
    #[must_use]
    pub fn new(set: DisplayOutputSet, presenter_supports_hdr: bool) -> Self {
        Self {
            outputs: set
                .0
                .into_iter()
                .map(|config| Some(TrackedOutput::new(config)))
                .collect(),
            presenter_supports_hdr,
        }
    }

    /// A tracker with every slot empty.
    #[must_use]
    pub const fn empty(presenter_supports_hdr: bool) -> Self {
        Self {
            outputs: Vec::new(),
            presenter_supports_hdr,
        }
    }

    /// The number of slot positions, configured or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outputs.iter().all(Option::is_none)
    }

    #[must_use]
    pub fn config(&self, output: usize) -> Option<&DisplayOutputConfig> {
        self.outputs
            .get(output)
            .and_then(Option::as_ref)
            .map(|tracked| &tracked.config)
    }

    #[must_use]
    pub fn state(&self, output: usize) -> Option<&DisplayOutputState> {
        self.outputs
            .get(output)
            .and_then(Option::as_ref)
            .map(|tracked| &tracked.state)
    }

    /// True once the output has been attached at least once (or was
    /// configured with a remembered id and name).
    #[must_use]
    pub fn is_bound(&self, output: usize) -> bool {
        self.outputs
            .get(output)
            .and_then(Option::as_ref)
            .is_some_and(|tracked| tracked.binding.is_some())
    }

    /// Changes one output's source without touching its display binding.
    pub fn set_source(&mut self, output: usize, source: DisplayOutputSource) -> bool {
        self.outputs
            .get_mut(output)
            .and_then(Option::as_mut)
            .map(|tracked| tracked.config.source = source)
            .is_some()
    }

    /// Fills, retargets, or clears slot `output` (zero-based, below
    /// [`MAX_DISPLAY_OUTPUTS`]) live.
    ///
    /// A configuration that chooses the same display keeps the binding, so
    /// a source or scaling change never moves the output; another display
    /// (or a newly filled slot) starts waiting and binds on the next
    /// [`Self::reconcile`]. Out-of-range slots are ignored.
    pub fn configure(
        &mut self,
        output: usize,
        config: Option<DisplayOutputConfig>,
    ) -> DisplayOutputReconfiguration {
        if output >= MAX_DISPLAY_OUTPUTS {
            return DisplayOutputReconfiguration::Unchanged;
        }
        if self.outputs.len() <= output {
            self.outputs.resize_with(output + 1, || None);
        }
        let slot = &mut self.outputs[output];
        match (slot.as_mut(), config) {
            (None, None) => DisplayOutputReconfiguration::Unchanged,
            (Some(_), None) => {
                *slot = None;
                DisplayOutputReconfiguration::Cleared
            }
            (Some(tracked), Some(config)) if tracked.config.display == config.display => {
                if tracked.config == config {
                    DisplayOutputReconfiguration::Unchanged
                } else {
                    tracked.config = config;
                    DisplayOutputReconfiguration::Retargeted
                }
            }
            (_, Some(config)) => {
                *slot = Some(TrackedOutput::new(config));
                DisplayOutputReconfiguration::Rebinding
            }
        }
    }

    /// Moves every active output to waiting after a failed enumeration.
    pub fn enumeration_failed(&mut self) -> Vec<DisplayOutputChange> {
        let mut changes = Vec::new();
        for (index, slot) in self.outputs.iter_mut().enumerate() {
            let Some(tracked) = slot else {
                continue;
            };
            let next = DisplayOutputState::Waiting(WaitReason::EnumerationFailed);
            if tracked.state != next {
                changes.push(DisplayOutputChange {
                    output: index,
                    previous: std::mem::replace(&mut tracked.state, next.clone()),
                    current: next,
                });
            }
        }
        changes
    }

    /// Applies one inventory snapshot and returns every changed output, in
    /// output order.
    ///
    /// Outputs already bound to a display claim first, so a hot-plugged
    /// display that shifts the index order never lets a waiting output take
    /// the display another output is showing; among bound (and among
    /// unbound) outputs, earlier outputs claim first.
    pub fn reconcile(&mut self, displays: &[DisplayInfo]) -> Vec<DisplayOutputChange> {
        let mut inventory = displays.to_vec();
        sort_displays(&mut inventory);
        let mut claimed = vec![false; inventory.len()];
        let mut changes = Vec::new();
        let bindings: Vec<Option<DisplayId>> = self
            .outputs
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .and_then(|tracked| tracked.binding.as_ref().map(|binding| binding.id.clone()))
            })
            .collect();
        let mut order: Vec<usize> = (0..self.outputs.len()).collect();
        order.sort_by_key(|&index| bindings[index].is_none());
        for index in order {
            let reserved: Vec<&DisplayId> = bindings
                .iter()
                .enumerate()
                .filter(|&(other, _)| other != index)
                .filter_map(|(_, binding)| binding.as_ref())
                .collect();
            let Some(tracked) = self.outputs[index].as_mut() else {
                continue;
            };
            let next = match find_display(tracked, &inventory, &reserved) {
                Match::Absent => DisplayOutputState::Waiting(WaitReason::DisplayAbsent),
                Match::Ambiguous => DisplayOutputState::Waiting(WaitReason::AmbiguousName),
                Match::Found(position) if claimed[position] => {
                    DisplayOutputState::Waiting(WaitReason::DisplayInUse)
                }
                Match::Found(position) => {
                    claimed[position] = true;
                    let display = &inventory[position];
                    tracked.binding = Some(Binding {
                        id: display.id.clone(),
                        name: display.name.clone(),
                    });
                    let (mode, mode_fallback) = match tracked.config.mode {
                        ModeRequest::Current => (display.current_mode, false),
                        ModeRequest::Exact(requested) => display
                            .supports(requested)
                            .map_or((display.current_mode, true), |mode| (mode, false)),
                    };
                    DisplayOutputState::Active(ActiveDisplay {
                        id: display.id.clone(),
                        name: display.name.clone(),
                        x: display.x,
                        y: display.y,
                        mode,
                        mode_fallback,
                        hdr: HdrState::resolve(
                            tracked.config.hdr,
                            display.hdr,
                            self.presenter_supports_hdr,
                        ),
                    })
                }
            };
            if tracked.state != next {
                changes.push(DisplayOutputChange {
                    output: index,
                    previous: std::mem::replace(&mut tracked.state, next.clone()),
                    current: next,
                });
            }
        }
        changes.sort_by_key(|change| change.output);
        changes
    }
}

/// The display `tracked` shows in `inventory`. A bound output follows its
/// display by id, then by unique name among the displays no other output is
/// bound to (`reserved`): an identical monitor showing another output is
/// never mistaken for this output's re-numbered display.
fn find_display(
    tracked: &TrackedOutput,
    inventory: &[DisplayInfo],
    reserved: &[&DisplayId],
) -> Match {
    let by_id = |id: &DisplayId| inventory.iter().position(|display| &display.id == id);
    let by_name = |name: &str, reserved: &[&DisplayId]| {
        let mut found = inventory
            .iter()
            .enumerate()
            .filter(|(_, display)| display.name == name && !reserved.contains(&&display.id));
        match (found.next(), found.next()) {
            (None, _) => Match::Absent,
            (Some((position, _)), None) => Match::Found(position),
            (Some(_), Some(_)) => Match::Ambiguous,
        }
    };
    if let Some(binding) = &tracked.binding {
        return by_id(&binding.id).map_or_else(|| by_name(&binding.name, reserved), Match::Found);
    }
    match &tracked.config.display {
        DisplaySelector::Id(id) => by_id(id).map_or(Match::Absent, Match::Found),
        DisplaySelector::Name(name) => by_name(name, &[]),
        DisplaySelector::IdOrName { id, name } => {
            by_id(id).map_or_else(|| by_name(name, &[]), Match::Found)
        }
        DisplaySelector::Index(index) => {
            if *index < inventory.len() {
                Match::Found(*index)
            } else {
                Match::Absent
            }
        }
    }
}

/// A deterministic in-memory display inventory for tests and diagnostics.
///
/// Clones share one inventory, so a test can keep a handle to plug, unplug,
/// re-number, or re-mode displays while the provider is owned elsewhere.
#[derive(Clone, Debug, Default)]
pub struct FakeDisplays {
    inventory: Arc<Mutex<FakeInventory>>,
}

#[derive(Debug, Default)]
struct FakeInventory {
    displays: Vec<DisplayInfo>,
    fail: Option<String>,
}

impl FakeDisplays {
    #[must_use]
    pub fn new(displays: Vec<DisplayInfo>) -> Self {
        Self {
            inventory: Arc::new(Mutex::new(FakeInventory {
                displays,
                fail: None,
            })),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut FakeInventory) -> R) -> R {
        f(&mut self
            .inventory
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
    }

    /// Connects a display, replacing any display with the same id.
    pub fn connect(&self, display: DisplayInfo) {
        self.with(|inventory| {
            inventory.displays.retain(|known| known.id != display.id);
            inventory.displays.push(display);
        });
    }

    /// Disconnects a display and returns it when it was connected.
    #[must_use = "the removed display can be reconnected later"]
    pub fn disconnect(&self, id: &DisplayId) -> Option<DisplayInfo> {
        self.with(|inventory| {
            let position = inventory
                .displays
                .iter()
                .position(|known| &known.id == id)?;
            Some(inventory.displays.remove(position))
        })
    }

    /// Replaces the whole inventory.
    pub fn replace(&self, displays: Vec<DisplayInfo>) {
        self.with(|inventory| inventory.displays = displays);
    }

    /// Makes enumeration fail with `reason` until cleared with `None`.
    pub fn fail_with(&self, reason: Option<String>) {
        self.with(|inventory| inventory.fail = reason);
    }
}

impl DisplayProvider for FakeDisplays {
    fn displays(&mut self) -> Result<Vec<DisplayInfo>, DisplayEnumerationError> {
        self.with(|inventory| match &inventory.fail {
            Some(reason) => Err(DisplayEnumerationError(reason.clone())),
            None => Ok(inventory.displays.clone()),
        })
    }
}
