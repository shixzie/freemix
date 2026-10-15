//! Fullscreen display-output value types (OR-011).
//!
//! These portable values describe what a borderless fullscreen display
//! output shows and where: a [`DisplaySelector`] choosing the display, a
//! [`DisplayOutputSource`], a [`ModeRequest`], an [`crate::OutputScaling`]
//! policy, and an [`HdrRequest`], grouped as a [`DisplayOutputConfig`] and
//! stored by project [`DisplayOutputSlot`]. The runtime side reports each
//! output's [`WaitReason`] or the [`HdrState`] it presents with. Display
//! enumeration and the hot-plug state machine live in `fm-io-api`.

use std::{fmt, num::NonZeroU128, str::FromStr};

use crate::{OutputId, OutputScaling};

/// The maximum number of simultaneous display outputs.
pub const MAX_DISPLAY_OUTPUTS: usize = 2;

/// The longest display id or display name, in UTF-8 bytes.
pub const MAX_DISPLAY_TEXT_BYTES: usize = 256;

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
        validate_display_text(&value)?;
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

/// Checks a display id or name: nonempty, at most
/// [`MAX_DISPLAY_TEXT_BYTES`] bytes, without commas or control characters.
///
/// # Errors
///
/// Returns [`DisplayConfigError::InvalidText`] otherwise.
pub fn validate_display_text(value: &str) -> Result<(), DisplayConfigError> {
    if value.is_empty()
        || value.len() > MAX_DISPLAY_TEXT_BYTES
        || value.chars().any(|c| c.is_control() || c == ',')
    {
        return Err(DisplayConfigError::InvalidText(value.to_owned()));
    }
    Ok(())
}

/// The largest display mode side in pixels.
pub const MAX_DISPLAY_MODE_SIDE: u32 = 16_384;
/// The largest display refresh rate in millihertz (1 kHz).
pub const MAX_DISPLAY_REFRESH_MILLIHERTZ: u32 = 1_000_000;

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

    /// True for a usable requested mode: both sides in
    /// `1..=`[`MAX_DISPLAY_MODE_SIDE`] and a refresh of at most
    /// [`MAX_DISPLAY_REFRESH_MILLIHERTZ`] (zero when unspecified).
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.width >= 1
            && self.width <= MAX_DISPLAY_MODE_SIDE
            && self.height >= 1
            && self.height <= MAX_DISPLAY_MODE_SIDE
            && self.refresh_millihertz <= MAX_DISPLAY_REFRESH_MILLIHERTZ
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
        let refresh_millihertz = match refresh {
            None => 0,
            Some(text) => parse_refresh(text).ok_or_else(invalid)?,
        };
        let mode = Self::new(width, height, refresh_millihertz);
        if mode.is_valid() {
            Ok(mode)
        } else {
            Err(invalid())
        }
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

impl FromStr for HdrCapability {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "sdr" => Ok(Self::Sdr),
            "hdr" => Ok(Self::Hdr),
            other => Err(DisplayConfigError::InvalidHdr(other.to_owned())),
        }
    }
}

/// The largest display index a selector may name.
pub const MAX_DISPLAY_INDEX: usize = 255;

/// How a configured output chooses its display before it is first bound.
///
/// Once an output has been shown on a display it follows that display by
/// platform id and then by unique name, never by index. [`Self::IdOrName`]
/// starts out that way: it binds by id and, when no display has the id,
/// by a unique display with the name (a platform that re-numbered the
/// display between runs), which is how a project remembers a display the
/// operator picked from the inventory.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DisplaySelector {
    Id(DisplayId),
    Name(String),
    /// Zero-based position in desktop order (position, then name, then id)
    /// at first binding, at most [`MAX_DISPLAY_INDEX`].
    Index(usize),
    /// A platform id, falling back to a unique display with the name.
    IdOrName {
        id: DisplayId,
        name: String,
    },
}

impl DisplaySelector {
    /// The platform id this selector names, if any.
    #[must_use]
    pub const fn id(&self) -> Option<&DisplayId> {
        match self {
            Self::Id(id) | Self::IdOrName { id, .. } => Some(id),
            Self::Name(_) | Self::Index(_) => None,
        }
    }

    /// The display name this selector names, if any.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Name(name) | Self::IdOrName { name, .. } => Some(name),
            Self::Id(_) | Self::Index(_) => None,
        }
    }

    /// The display index this selector names, if any.
    #[must_use]
    pub const fn index(&self) -> Option<usize> {
        match self {
            Self::Index(index) => Some(*index),
            _ => None,
        }
    }

    /// Builds a selector from the optional fields a project or protocol
    /// record carries: an id (with an optional name fallback), a name, or
    /// an index. Exactly one of id, name, or index chooses the display; a
    /// name may accompany an id but an index stands alone.
    ///
    /// # Errors
    ///
    /// Returns [`DisplayConfigError::InvalidSelector`] for no field, an
    /// index combined with an id or name, or an index above
    /// [`MAX_DISPLAY_INDEX`], and [`DisplayConfigError::InvalidText`] for an
    /// unusable id or name.
    pub fn from_parts(
        id: Option<&str>,
        name: Option<&str>,
        index: Option<usize>,
    ) -> Result<Self, DisplayConfigError> {
        let describe = || {
            format!(
                "id={} name={} index={}",
                id.unwrap_or("-"),
                name.unwrap_or("-"),
                index.map_or_else(|| "-".to_owned(), |index| index.to_string())
            )
        };
        match (id, name, index) {
            (Some(id), None, None) => DisplayId::new(id).map(Self::Id),
            (Some(id), Some(name), None) => {
                validate_display_text(name)?;
                Ok(Self::IdOrName {
                    id: DisplayId::new(id)?,
                    name: name.to_owned(),
                })
            }
            (None, Some(name), None) => {
                validate_display_text(name)?;
                Ok(Self::Name(name.to_owned()))
            }
            (None, None, Some(index)) if index <= MAX_DISPLAY_INDEX => Ok(Self::Index(index)),
            _ => Err(DisplayConfigError::InvalidSelector(describe())),
        }
    }

    /// Whether two selectors claim the same display in one configuration:
    /// they share a platform id, or (without ids) a name or an index. Two
    /// identical monitors may share a name, so selectors that carry
    /// different ids never conflict.
    #[must_use]
    pub fn conflicts_with(&self, other: &Self) -> bool {
        match (self.id(), other.id()) {
            (Some(left), Some(right)) => left == right,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => match (self, other) {
                (Self::Name(left), Self::Name(right)) => left == right,
                (Self::Index(left), Self::Index(right)) => left == right,
                _ => false,
            },
        }
    }
}

impl fmt::Display for DisplaySelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "id:{id}"),
            Self::Name(name) => write!(formatter, "name:{name}"),
            Self::Index(index) => write!(formatter, "index:{index}"),
            Self::IdOrName { id, name } => write!(formatter, "id:{id}|name:{name}"),
        }
    }
}

impl FromStr for DisplaySelector {
    type Err = DisplayConfigError;

    /// Parses `id:<id>`, `id:<id>|name:<name>`, `name:<name>`,
    /// `index:<n>`, or a bare index.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(id) = value.strip_prefix("id:") {
            if let Some((id, name)) = id.split_once("|name:") {
                return Self::from_parts(Some(id), Some(name), None);
            }
            return DisplayId::new(id).map(Self::Id);
        }
        if let Some(name) = value.strip_prefix("name:") {
            validate_display_text(name)?;
            return Ok(Self::Name(name.to_owned()));
        }
        let index = value.strip_prefix("index:").unwrap_or(value);
        index
            .parse::<usize>()
            .ok()
            .filter(|index| *index <= MAX_DISPLAY_INDEX)
            .map(Self::Index)
            .ok_or_else(|| DisplayConfigError::InvalidSelector(value.to_owned()))
    }
}

/// One of the project's two fullscreen display-output slots (`1..=2`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayOutputSlot(u8);

impl DisplayOutputSlot {
    /// Every slot, slot 1 first.
    pub const ALL: [Self; MAX_DISPLAY_OUTPUTS] = [Self(1), Self(2)];

    /// A slot number in `1..=`[`MAX_DISPLAY_OUTPUTS`].
    #[must_use]
    pub const fn new(slot: u8) -> Option<Self> {
        if slot >= 1 && slot as usize <= MAX_DISPLAY_OUTPUTS {
            Some(Self(slot))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The zero-based position of the slot.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

impl fmt::Display for DisplayOutputSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
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
    /// This daemon has no display presenter: it runs simulated, was built
    /// without display surfaces, or started without display outputs. The
    /// configuration is kept and takes effect on the next native start
    /// with a presenter.
    NoPresenter,
}

impl WaitReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisplayAbsent => "display-absent",
            Self::DisplayInUse => "display-in-use",
            Self::AmbiguousName => "ambiguous-name",
            Self::EnumerationFailed => "enumeration-failed",
            Self::NoPresenter => "no-presenter",
        }
    }

    /// Every reason in a stable order.
    pub const ALL: [Self; 5] = [
        Self::DisplayAbsent,
        Self::DisplayInUse,
        Self::AmbiguousName,
        Self::EnumerationFailed,
        Self::NoPresenter,
    ];
}

impl FromStr for WaitReason {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
            .ok_or_else(|| DisplayConfigError::InvalidField(value.to_owned()))
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

    /// Every state in a stable order.
    pub const ALL: [Self; 5] = [
        Self::Sdr,
        Self::SdrDisplayNotCapable,
        Self::SdrCapabilityUnknown,
        Self::SdrPresenterLimited,
        Self::Hdr,
    ];

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

impl FromStr for HdrState {
    type Err = DisplayConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
            .ok_or_else(|| DisplayConfigError::InvalidHdr(value.to_owned()))
    }
}
