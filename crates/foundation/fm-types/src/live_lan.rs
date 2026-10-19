//! `LiveLAN` value types (OR-017).
//!
//! `LiveLAN` lets people on the local network watch Program, or one chosen
//! project output, in a browser: the server encodes the picture into a
//! low-latency HLS stream and serves a viewer page next to it. The project
//! holds a [`LiveLanConfig`]: whether it is enabled, what it shows, how many
//! viewers it admits, and an optional access [`LiveLanPin`].
//!
//! The PIN is a secret. It has no `Display`, its `Debug` is masked, and it
//! is read only through [`LiveLanPin::expose_secret`].
//!
//! The runtime side reports a [`LiveLanStatus`]: its [`LiveLanState`], the
//! viewer page URL while it is served, the active viewers, and the bytes per
//! second it sends them.

use std::fmt;
use std::num::NonZeroU128;
use std::str::FromStr;

use crate::OutputId;

/// Fewest viewers a `LiveLAN` configuration admits.
pub const MIN_LIVE_LAN_VIEWERS: u8 = 1;
/// Most viewers a `LiveLAN` configuration admits.
pub const MAX_LIVE_LAN_VIEWERS: u8 = 32;
/// Viewers a new configuration admits.
pub const DEFAULT_LIVE_LAN_VIEWERS: u8 = 8;
/// Shortest access PIN, in digits.
pub const MIN_LIVE_LAN_PIN_DIGITS: usize = 4;
/// Longest access PIN, in digits.
pub const MAX_LIVE_LAN_PIN_DIGITS: usize = 12;
/// Longest viewer page URL a status carries, in bytes.
pub const MAX_LIVE_LAN_URL_BYTES: usize = 64;

/// What a `LiveLAN` stream shows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LiveLanSource {
    /// Program, as the first project output (or the unbound output) shows it.
    #[default]
    Program,
    /// One configured project output.
    Output(OutputId),
}

impl LiveLanSource {
    /// The output this source names, if any.
    #[must_use]
    pub const fn output(self) -> Option<OutputId> {
        match self {
            Self::Program => None,
            Self::Output(output) => Some(output),
        }
    }
}

/// `program` or `output:<id>`.
impl fmt::Display for LiveLanSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program => formatter.write_str("program"),
            Self::Output(output) => write!(formatter, "output:{output}"),
        }
    }
}

/// Why a `LiveLAN` source token is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLiveLanSourceError;

impl fmt::Display for ParseLiveLanSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LiveLAN source must be program or output:<id>")
    }
}

impl std::error::Error for ParseLiveLanSourceError {}

impl FromStr for LiveLanSource {
    type Err = ParseLiveLanSourceError;

    /// Parses the canonical [`Display`](fmt::Display) form only: the output
    /// id is a nonzero decimal without leading zeros.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "program" {
            return Ok(Self::Program);
        }
        let digits = value
            .strip_prefix("output:")
            .ok_or(ParseLiveLanSourceError)?;
        if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ParseLiveLanSourceError);
        }
        digits
            .parse::<u128>()
            .ok()
            .and_then(NonZeroU128::new)
            .map(|id| Self::Output(OutputId::new(id)))
            .ok_or(ParseLiveLanSourceError)
    }
}

/// Why an access PIN is refused. No variant carries PIN text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveLanPinError {
    /// Shorter than [`MIN_LIVE_LAN_PIN_DIGITS`] or longer than
    /// [`MAX_LIVE_LAN_PIN_DIGITS`].
    Length,
    /// Not only ASCII digits.
    NotDigits,
}

impl fmt::Display for LiveLanPinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length => write!(
                formatter,
                "LiveLAN PIN must be {MIN_LIVE_LAN_PIN_DIGITS} to {MAX_LIVE_LAN_PIN_DIGITS} digits"
            ),
            Self::NotDigits => formatter.write_str("LiveLAN PIN must be digits only"),
        }
    }
}

impl std::error::Error for LiveLanPinError {}

/// Whether `pin` is a usable access PIN.
#[must_use]
pub fn live_lan_pin_is_valid(pin: &str) -> bool {
    LiveLanPin::parse(pin).is_ok()
}

/// One validated `LiveLAN` access PIN: [`MIN_LIVE_LAN_PIN_DIGITS`] to
/// [`MAX_LIVE_LAN_PIN_DIGITS`] ASCII digits.
///
/// The value is never rendered: there is no `Display`, `Debug` prints
/// `LiveLanPin("****")`, and only [`Self::expose_secret`] reads it.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct LiveLanPin {
    secret: String,
}

impl LiveLanPin {
    /// Validates length and character set.
    ///
    /// # Errors
    ///
    /// Returns a [`LiveLanPinError`]. No variant carries the text.
    pub fn parse(pin: &str) -> Result<Self, LiveLanPinError> {
        if !(MIN_LIVE_LAN_PIN_DIGITS..=MAX_LIVE_LAN_PIN_DIGITS).contains(&pin.len()) {
            return Err(LiveLanPinError::Length);
        }
        if !pin.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(LiveLanPinError::NotDigits);
        }
        Ok(Self {
            secret: pin.to_owned(),
        })
    }

    /// Returns the PIN in the clear.
    ///
    /// Call this only to compare a viewer's attempt or to write the project
    /// bundle. It must not reach a log, a status line, or an error.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for LiveLanPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("LiveLanPin").field(&"****").finish()
    }
}

/// Why a `LiveLAN` configuration is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveLanConfigError {
    /// The viewer limit is outside
    /// [`MIN_LIVE_LAN_VIEWERS`]`..=`[`MAX_LIVE_LAN_VIEWERS`].
    MaxViewers,
    /// The source names an output the project does not hold.
    UnknownOutput(OutputId),
}

impl LiveLanConfigError {
    /// The refusal code clients see.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MaxViewers => "invalid_max_viewers",
            Self::UnknownOutput(_) => "unknown_output",
        }
    }
}

impl fmt::Display for LiveLanConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MaxViewers => write!(
                formatter,
                "LiveLAN viewer limit must be {MIN_LIVE_LAN_VIEWERS} to {MAX_LIVE_LAN_VIEWERS}"
            ),
            Self::UnknownOutput(output) => {
                write!(formatter, "LiveLAN source output {output} does not exist")
            }
        }
    }
}

impl std::error::Error for LiveLanConfigError {}

/// A project's `LiveLAN` configuration.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LiveLanConfig {
    /// Whether the server encodes and serves the stream.
    pub enabled: bool,
    pub source: LiveLanSource,
    /// Most viewers watching at once; a further viewer is turned away.
    pub max_viewers: u8,
    /// The access PIN a viewer must enter, or `None` for open access.
    pub pin: Option<LiveLanPin>,
}

impl Default for LiveLanConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source: LiveLanSource::Program,
            max_viewers: DEFAULT_LIVE_LAN_VIEWERS,
            pin: None,
        }
    }
}

impl LiveLanConfig {
    /// Checks the viewer limit and that the source names one of `outputs`.
    ///
    /// # Errors
    ///
    /// Returns the first [`LiveLanConfigError`].
    pub fn check(&self, outputs: &[OutputId]) -> Result<(), LiveLanConfigError> {
        if !(MIN_LIVE_LAN_VIEWERS..=MAX_LIVE_LAN_VIEWERS).contains(&self.max_viewers) {
            return Err(LiveLanConfigError::MaxViewers);
        }
        if let LiveLanSource::Output(output) = self.source
            && !outputs.contains(&output)
        {
            return Err(LiveLanConfigError::UnknownOutput(output));
        }
        Ok(())
    }
}

/// What the `LiveLAN` stream is doing.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum LiveLanState {
    /// `LiveLAN` is disabled.
    #[default]
    Off,
    /// `LiveLAN` is enabled, but this server cannot serve it (no `LiveLAN`
    /// listener, or no native media).
    Unavailable,
    /// The encoder is starting.
    Starting,
    /// Viewers can watch.
    Live,
    /// The encoder failed; changing the configuration, or disabling and
    /// enabling it again, retries.
    Failed,
}

impl LiveLanState {
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::Unavailable,
        Self::Starting,
        Self::Live,
        Self::Failed,
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Unavailable => "unavailable",
            Self::Starting => "starting",
            Self::Live => "live",
            Self::Failed => "failed",
        }
    }

    /// The state a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.token() == token)
    }
}

/// The `LiveLAN` stream's runtime status.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct LiveLanStatus {
    pub state: LiveLanState,
    /// The viewer page, `http://<address>:<port>/`, while it is served;
    /// empty otherwise. At most [`MAX_LIVE_LAN_URL_BYTES`].
    pub url: String,
    /// Viewers who fetched the stream in the last few seconds.
    pub viewers: u8,
    /// What the server sent viewers over the last two seconds, in kbit/s.
    pub egress_kbps: u32,
}

impl LiveLanStatus {
    /// This status as clients see it while `LiveLAN` is `enabled` or not: a
    /// disabled stream is `off` with nothing served, an enabled one nobody
    /// serves is `unavailable`, and a URL the wire refuses is dropped.
    #[must_use]
    pub fn for_config(&self, enabled: bool) -> Self {
        if !enabled {
            return Self::default();
        }
        let mut status = self.clone();
        if status.state == LiveLanState::Off {
            status.state = LiveLanState::Unavailable;
        }
        if !live_lan_url_is_valid(&status.url) {
            status.url.clear();
        }
        status
    }
}

/// Whether `url` is a usable status URL: empty, or `http://` followed by
/// printable ASCII without spaces, commas, or tildes, ending in `/`, within
/// [`MAX_LIVE_LAN_URL_BYTES`].
#[must_use]
pub fn live_lan_url_is_valid(url: &str) -> bool {
    url.is_empty()
        || (url.len() <= MAX_LIVE_LAN_URL_BYTES
            && url.starts_with("http://")
            && url.ends_with('/')
            && url.len() > "http://".len() + 1
            && url
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b'~'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_round_trip_only_in_canonical_form() {
        for text in ["program", "output:7"] {
            assert_eq!(text.parse::<LiveLanSource>().unwrap().to_string(), text);
        }
        for text in [
            "",
            "Program",
            "output:",
            "output:0",
            "output:07",
            "output:-1",
            "preview",
        ] {
            assert!(text.parse::<LiveLanSource>().is_err(), "{text}");
        }
    }

    #[test]
    fn pins_are_digit_strings_and_never_render() {
        assert!(LiveLanPin::parse("1234").is_ok());
        assert!(LiveLanPin::parse("123456789012").is_ok());
        assert_eq!(LiveLanPin::parse("123"), Err(LiveLanPinError::Length));
        assert_eq!(
            LiveLanPin::parse("1234567890123"),
            Err(LiveLanPinError::Length)
        );
        assert_eq!(LiveLanPin::parse("12a4"), Err(LiveLanPinError::NotDigits));
        let pin = LiveLanPin::parse("9876").unwrap();
        assert!(!format!("{pin:?}").contains("9876"));
        let config = LiveLanConfig {
            pin: Some(pin),
            ..LiveLanConfig::default()
        };
        assert!(!format!("{config:?}").contains("9876"));
    }

    #[test]
    fn configs_check_viewers_and_outputs() {
        let output = OutputId::new(NonZeroU128::new(3).unwrap());
        let mut config = LiveLanConfig::default();
        assert_eq!(config.check(&[]), Ok(()));
        config.max_viewers = 0;
        assert_eq!(config.check(&[]), Err(LiveLanConfigError::MaxViewers));
        config.max_viewers = MAX_LIVE_LAN_VIEWERS + 1;
        assert_eq!(config.check(&[]), Err(LiveLanConfigError::MaxViewers));
        config.max_viewers = MAX_LIVE_LAN_VIEWERS;
        config.source = LiveLanSource::Output(output);
        assert_eq!(
            config.check(&[]),
            Err(LiveLanConfigError::UnknownOutput(output))
        );
        assert_eq!(config.check(&[output]), Ok(()));
    }

    #[test]
    fn urls_and_states() {
        assert!(live_lan_url_is_valid(""));
        assert!(live_lan_url_is_valid("http://192.168.1.4:8090/"));
        assert!(!live_lan_url_is_valid("https://192.168.1.4/"));
        assert!(!live_lan_url_is_valid("http://a b/"));
        assert!(!live_lan_url_is_valid("http:///"));
        for state in LiveLanState::ALL {
            assert_eq!(LiveLanState::from_token(state.token()), Some(state));
        }
    }
}
