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
    collections::BTreeSet,
    fmt,
    num::NonZeroU128,
    str::FromStr,
    sync::{Arc, Mutex, PoisonError},
};

use fm_types::{OutputId, OutputScaling};

/// The maximum number of simultaneous display outputs.
pub const MAX_DISPLAY_OUTPUTS: usize = 2;

const MAX_DISPLAY_TEXT_BYTES: usize = 256;

/// A platform-stable display identity (for example a `CGDirectDisplayID`
/// UUID, a `RandR` output id, or a Windows device path).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayId(String);

impl DisplayId {
    /// Creates an identity from nonempty bounded text without control
    /// characters or commas.
    ///
    /// # Errors
    ///
    /// Returns [`DisplayConfigError::InvalidText`] otherwise.
    pub fn new(value: impl Into<String>) -> Result<Self, DisplayConfigError> {
        let value = value.into();
        validate_text(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_text(value: &str) -> Result<(), DisplayConfigError> {
    if value.is_empty()
        || value.len() > MAX_DISPLAY_TEXT_BYTES
        || value.chars().any(|c| c.is_control() || c == ',')
    {
        return Err(DisplayConfigError::InvalidText(value.to_owned()));
    }
    Ok(())
}

/// One display timing: active pixels and refresh rate in millihertz.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz, or zero when the platform does not report it.
    pub refresh_millihertz: u32,
}

impl DisplayMode {
    #[must_use]
    pub const fn new(width: u32, height: u32, refresh_millihertz: u32) -> Self {
        Self {
            width,
            height,
            refresh_millihertz,
        }
    }

    /// True when `other` has the same raster and either refresh is unknown or
    /// both refreshes agree within one hertz (59.94 matches 60).
    #[must_use]
    pub const fn matches(self, other: Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && (self.refresh_millihertz == 0
                || other.refresh_millihertz == 0
                || self.refresh_millihertz.abs_diff(other.refresh_millihertz) < 1_000)
    }
}

impl fmt::Display for DisplayMode {
    /// Formats as `WIDTHxHEIGHT@MILLIHERTZ` (`@0` when unknown).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}x{}@{}",
            self.width, self.height, self.refresh_millihertz
        )
    }
}

impl FromStr for DisplayMode {
    type Err = DisplayConfigError;

    /// Parses `WxH`, `WxH@HZ` (`60`, `59.94`), or `WxH@MILLIHERTZ` (`60000`).
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || DisplayConfigError::InvalidMode(value.to_owned());
        let (raster, refresh) = value
            .split_once('@')
            .map_or((value, None), |(raster, hz)| (raster, Some(hz)));
        let (width, height) = raster.split_once('x').ok_or_else(invalid)?;
        let width = width.parse::<u32>().map_err(|_| invalid())?;
        let height = height.parse::<u32>().map_err(|_| invalid())?;
        if width == 0 || height == 0 {
            return Err(invalid());
        }
        let refresh_millihertz = match refresh {
            None => 0,
            Some(text) => parse_refresh(text).ok_or_else(invalid)?,
        };
        Ok(Self::new(width, height, refresh_millihertz))
    }
}

fn parse_refresh(text: &str) -> Option<u32> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || fraction.len() > 3 {
        return None;
    }
    let whole = whole.parse::<u32>().ok()?;
    if fraction.is_empty() && whole >= 1_000 {
        return Some(whole);
    }
    let mut millis = 0_u32;
    for (index, digit) in fraction.chars().enumerate() {
        let digit = digit.to_digit(10)?;
        millis += digit * 10_u32.pow(2 - u32::try_from(index).ok()?);
    }
    whole.checked_mul(1_000)?.checked_add(millis)
}

/// Whether a display can show HDR, as far as the platform reports.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HdrCapability {
    /// The platform API in use does not expose HDR capability.
    #[default]
    Unknown,
    Sdr,
    Hdr,
}

impl HdrCapability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Sdr => "sdr",
            Self::Hdr => "hdr",
        }
    }
}

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

/// How a configured output chooses its display before it is first bound.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DisplaySelector {
    Id(DisplayId),
    Name(String),
    /// Zero-based position in [`sort_displays`] order at first binding.
    Index(usize),
}

impl fmt::Display for DisplaySelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "id:{id}"),
            Self::Name(name) => write!(formatter, "name:{name}"),
            Self::Index(index) => write!(formatter, "index:{index}"),
        }
    }
}

impl FromStr for DisplaySelector {
    type Err = DisplayConfigError;

    /// Parses `id:<id>`, `name:<name>`, `index:<n>`, or a bare index.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(id) = value.strip_prefix("id:") {
            return DisplayId::new(id).map(Self::Id);
        }
        if let Some(name) = value.strip_prefix("name:") {
            validate_text(name)?;
            return Ok(Self::Name(name.to_owned()));
        }
        let index = value.strip_prefix("index:").unwrap_or(value);
        index
            .parse::<usize>()
            .map(Self::Index)
            .map_err(|_| DisplayConfigError::InvalidSelector(value.to_owned()))
    }
}

/// What a display output shows.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DisplayOutputSource {
    Program,
    Preview,
    /// A configured project output by id.
    Output(OutputId),
}

impl fmt::Display for DisplayOutputSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program => formatter.write_str("program"),
            Self::Preview => formatter.write_str("preview"),
            Self::Output(output) => write!(formatter, "output:{output}"),
        }
    }
}

impl FromStr for DisplayOutputSource {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "program" => Ok(Self::Program),
            "preview" => Ok(Self::Preview),
            other => other
                .strip_prefix("output:")
                .and_then(|id| id.parse::<u128>().ok())
                .and_then(NonZeroU128::new)
                .map(|id| Self::Output(OutputId::new(id)))
                .ok_or_else(|| DisplayConfigError::InvalidSource(other.to_owned())),
        }
    }
}

/// The display mode an output asks for.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ModeRequest {
    /// Use whatever mode the display is currently driven at.
    #[default]
    Current,
    /// Prefer this mode; fall back to the current mode when unsupported.
    Exact(DisplayMode),
}

impl fmt::Display for ModeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current => formatter.write_str("current"),
            Self::Exact(mode) => mode.fmt(formatter),
        }
    }
}

impl FromStr for ModeRequest {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "current" {
            Ok(Self::Current)
        } else {
            value.parse().map(Self::Exact)
        }
    }
}

/// The HDR policy an output asks for.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HdrRequest {
    /// Always present SDR.
    #[default]
    Off,
    /// Present HDR when both the display and the presenter support it.
    Auto,
}

impl FromStr for HdrRequest {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" | "sdr" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            other => Err(DisplayConfigError::InvalidHdr(other.to_owned())),
        }
    }
}

impl fmt::Display for HdrRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Auto => "auto",
        })
    }
}

/// One fullscreen display output's desired configuration.
///
/// Every display output is a borderless fullscreen surface with the cursor
/// hidden; those properties are not configurable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputConfig {
    pub display: DisplaySelector,
    pub source: DisplayOutputSource,
    pub mode: ModeRequest,
    pub scaling: OutputScaling,
    pub hdr: HdrRequest,
}

impl DisplayOutputConfig {
    /// Program on `display`, current mode, Fit, SDR.
    #[must_use]
    pub fn program_on(display: DisplaySelector) -> Self {
        Self {
            display,
            source: DisplayOutputSource::Program,
            mode: ModeRequest::Current,
            scaling: OutputScaling::Fit,
            hdr: HdrRequest::Off,
        }
    }
}

impl fmt::Display for DisplayOutputConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "display={},source={},mode={},scaling={},hdr={}",
            self.display, self.source, self.mode, self.scaling, self.hdr
        )
    }
}

impl FromStr for DisplayOutputConfig {
    type Err = DisplayConfigError;

    /// Parses comma-separated `key=value` fields: required `display`, and
    /// optional `source` (default `program`), `mode` (`current`), `scaling`
    /// (`fit`), and `hdr` (`off`). Each key may appear once.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut display = None;
        let mut source = None;
        let mut mode = None;
        let mut scaling = None;
        let mut hdr = None;
        for field in value.split(',') {
            let (key, text) = field
                .split_once('=')
                .ok_or_else(|| DisplayConfigError::InvalidField(field.to_owned()))?;
            let duplicate = match key {
                "display" => display.replace(text.parse()?).is_some(),
                "source" => source.replace(text.parse()?).is_some(),
                "mode" => mode.replace(text.parse()?).is_some(),
                "scaling" => scaling
                    .replace(
                        text.parse::<OutputScaling>()
                            .map_err(|_| DisplayConfigError::InvalidScaling(text.to_owned()))?,
                    )
                    .is_some(),
                "hdr" => hdr.replace(text.parse()?).is_some(),
                _ => return Err(DisplayConfigError::InvalidField(field.to_owned())),
            };
            if duplicate {
                return Err(DisplayConfigError::DuplicateField(key.to_owned()));
            }
        }
        Ok(Self {
            display: display.ok_or(DisplayConfigError::MissingDisplay)?,
            source: source.unwrap_or(DisplayOutputSource::Program),
            mode: mode.unwrap_or_default(),
            scaling: scaling.unwrap_or_default(),
            hdr: hdr.unwrap_or_default(),
        })
    }
}

/// An invalid display-output configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayConfigError {
    InvalidText(String),
    InvalidMode(String),
    InvalidSelector(String),
    InvalidSource(String),
    InvalidScaling(String),
    InvalidHdr(String),
    InvalidField(String),
    DuplicateField(String),
    MissingDisplay,
    Empty,
    TooMany { maximum: usize, actual: usize },
    DuplicateSelector(DisplaySelector),
}

impl fmt::Display for DisplayConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidText(text) => write!(
                formatter,
                "display text `{text}` must be 1..={MAX_DISPLAY_TEXT_BYTES} bytes without commas or control characters"
            ),
            Self::InvalidMode(text) => write!(
                formatter,
                "invalid display mode `{text}`; expected WIDTHxHEIGHT[@HZ]"
            ),
            Self::InvalidSelector(text) => write!(
                formatter,
                "invalid display selector `{text}`; expected id:<id>, name:<name>, or index:<n>"
            ),
            Self::InvalidSource(text) => write!(
                formatter,
                "invalid display source `{text}`; expected program, preview, or output:<id>"
            ),
            Self::InvalidScaling(text) => write!(
                formatter,
                "invalid scaling `{text}`; expected fit, fill, stretch, or 1:1"
            ),
            Self::InvalidHdr(text) => {
                write!(formatter, "invalid hdr `{text}`; expected off or auto")
            }
            Self::InvalidField(text) => write!(
                formatter,
                "invalid display output field `{text}`; expected display=, source=, mode=, scaling=, or hdr="
            ),
            Self::DuplicateField(key) => {
                write!(formatter, "display output field `{key}` is repeated")
            }
            Self::MissingDisplay => formatter.write_str("display output requires display="),
            Self::Empty => formatter.write_str("no display outputs are configured"),
            Self::TooMany { maximum, actual } => write!(
                formatter,
                "{actual} display outputs configured; at most {maximum} are supported"
            ),
            Self::DuplicateSelector(selector) => write!(
                formatter,
                "two display outputs select the same display `{selector}`"
            ),
        }
    }
}

impl std::error::Error for DisplayConfigError {}

/// A validated, ordered set of one through [`MAX_DISPLAY_OUTPUTS`] outputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayOutputSet(Vec<DisplayOutputConfig>);

impl DisplayOutputSet {
    /// Validates count and rejects two outputs with the same selector.
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
        let mut selectors = BTreeSet::new();
        for output in &outputs {
            if !selectors.insert(output.display.to_string()) {
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

/// Why an output is not currently showing on a display.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WaitReason {
    /// No connected display matches the output.
    DisplayAbsent,
    /// The matching display is claimed by an earlier output.
    DisplayInUse,
    /// More than one connected display has the output's name and none has its id.
    AmbiguousName,
    /// The last enumeration failed; the output keeps waiting until it succeeds.
    EnumerationFailed,
}

impl WaitReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisplayAbsent => "display-absent",
            Self::DisplayInUse => "display-in-use",
            Self::AmbiguousName => "ambiguous-name",
            Self::EnumerationFailed => "enumeration-failed",
        }
    }
}

/// Why an output presents SDR.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HdrState {
    /// SDR was requested.
    Sdr,
    /// HDR was requested but the display reports no HDR capability.
    SdrDisplayNotCapable,
    /// HDR was requested but the platform does not report HDR capability.
    SdrCapabilityUnknown,
    /// HDR was requested and the display is capable, but the presenter is SDR-only.
    SdrPresenterLimited,
    /// HDR is presented.
    Hdr,
}

impl HdrState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sdr => "sdr",
            Self::SdrDisplayNotCapable => "sdr-display-not-capable",
            Self::SdrCapabilityUnknown => "sdr-capability-unknown",
            Self::SdrPresenterLimited => "sdr-presenter-limited",
            Self::Hdr => "hdr",
        }
    }

    #[must_use]
    pub const fn resolve(
        request: HdrRequest,
        display: HdrCapability,
        presenter_supports_hdr: bool,
    ) -> Self {
        match (request, display) {
            (HdrRequest::Off, _) => Self::Sdr,
            (HdrRequest::Auto, HdrCapability::Sdr) => Self::SdrDisplayNotCapable,
            (HdrRequest::Auto, HdrCapability::Unknown) => Self::SdrCapabilityUnknown,
            (HdrRequest::Auto, HdrCapability::Hdr) if presenter_supports_hdr => Self::Hdr,
            (HdrRequest::Auto, HdrCapability::Hdr) => Self::SdrPresenterLimited,
        }
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
    /// Zero-based output position in the [`DisplayOutputSet`].
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

enum Match {
    Found(usize),
    Absent,
    Ambiguous,
}

/// The hot-plug and display-matching state machine for a [`DisplayOutputSet`].
#[derive(Clone, Debug)]
pub struct DisplayOutputTracker {
    outputs: Vec<TrackedOutput>,
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
                .map(|config| TrackedOutput {
                    config,
                    binding: None,
                    state: DisplayOutputState::Waiting(WaitReason::DisplayAbsent),
                })
                .collect(),
            presenter_supports_hdr,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    #[must_use]
    pub fn config(&self, output: usize) -> Option<&DisplayOutputConfig> {
        self.outputs.get(output).map(|tracked| &tracked.config)
    }

    #[must_use]
    pub fn state(&self, output: usize) -> Option<&DisplayOutputState> {
        self.outputs.get(output).map(|tracked| &tracked.state)
    }

    /// True once the output has been attached at least once.
    #[must_use]
    pub fn is_bound(&self, output: usize) -> bool {
        self.outputs
            .get(output)
            .is_some_and(|tracked| tracked.binding.is_some())
    }

    /// Changes one output's source without touching its display binding.
    pub fn set_source(&mut self, output: usize, source: DisplayOutputSource) -> bool {
        self.outputs
            .get_mut(output)
            .map(|tracked| tracked.config.source = source)
            .is_some()
    }

    /// Moves every active output to waiting after a failed enumeration.
    pub fn enumeration_failed(&mut self) -> Vec<DisplayOutputChange> {
        let mut changes = Vec::new();
        for (index, tracked) in self.outputs.iter_mut().enumerate() {
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

    /// Applies one inventory snapshot and returns every changed output.
    pub fn reconcile(&mut self, displays: &[DisplayInfo]) -> Vec<DisplayOutputChange> {
        let mut inventory = displays.to_vec();
        sort_displays(&mut inventory);
        let mut claimed = vec![false; inventory.len()];
        let mut changes = Vec::new();
        for (index, tracked) in self.outputs.iter_mut().enumerate() {
            let next = match find_display(tracked, &inventory) {
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
        changes
    }
}

fn find_display(tracked: &TrackedOutput, inventory: &[DisplayInfo]) -> Match {
    let by_id = |id: &DisplayId| inventory.iter().position(|display| &display.id == id);
    let by_name = |name: &str| {
        let mut found = inventory
            .iter()
            .enumerate()
            .filter(|(_, display)| display.name == name);
        match (found.next(), found.next()) {
            (None, _) => Match::Absent,
            (Some((position, _)), None) => Match::Found(position),
            (Some(_), Some(_)) => Match::Ambiguous,
        }
    };
    if let Some(binding) = &tracked.binding {
        return by_id(&binding.id).map_or_else(|| by_name(&binding.name), Match::Found);
    }
    match &tracked.config.display {
        DisplaySelector::Id(id) => by_id(id).map_or(Match::Absent, Match::Found),
        DisplaySelector::Name(name) => by_name(name),
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
