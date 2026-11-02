//! Canonical recorder track-file rollover limits (AU-010).
//!
//! A track WAV session can close a file after a fixed span of audio or a
//! fixed number of data bytes and continue in `<name>-NNNN.wav`. The limits
//! are authored per project and stored as a small canonical document beside
//! the track maps, `recorders/limits.tracklimits`:
//!
//! ```text
//! freemix-record-track-limits 1
//! max-seconds=600
//! max-bytes=1099511627776
//! ```
//!
//! Both lines are optional; their absence means that limit is off. The
//! document is canonical so a rewrite is byte-identical, and it is decoded
//! strictly: an unknown key, a duplicate, an out-of-order line, or a value
//! outside the supported range is refused.

use core::fmt;
use core::num::{NonZeroU32, NonZeroU64};

/// Longest track-file rollover span, in seconds (24 hours).
pub const MAX_TRACK_SEGMENT_SECONDS: u32 = 86_400;
/// Largest track-file rollover size, in bytes (1 TiB).
pub const MAX_TRACK_SEGMENT_BYTES: u64 = 1_099_511_627_776;
/// The canonical document's header line.
pub const TRACK_LIMITS_HEADER: &str = "freemix-record-track-limits 1";
/// File name of the limits document inside a bundle's `recorders` directory.
pub const TRACK_LIMITS_FILE_NAME: &str = "limits.tracklimits";

/// Validated track-file rollover limits; every field is optional, so a map
/// can record without either.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackLimits {
    max_segment_seconds: Option<NonZeroU32>,
    max_segment_bytes: Option<NonZeroU64>,
}

/// Why limits could not be built or decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackLimitsError {
    /// The first line is not the current document header.
    Header,
    /// A line is not `key=value`.
    Line,
    /// A line appears twice.
    Duplicate(&'static str),
    /// A line appears after one that must follow it.
    Order,
    /// A line names a key this document does not define.
    UnknownKey,
    /// A value is zero or past its bound.
    Range { field: &'static str, max: u64 },
}

impl fmt::Display for TrackLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => formatter.write_str("track-limits document header is unknown"),
            Self::Line => formatter.write_str("track-limits line is malformed"),
            Self::Duplicate(field) => write!(formatter, "track-limits field {field} is duplicated"),
            Self::Order => formatter.write_str("track-limits fields are out of order"),
            Self::UnknownKey => formatter.write_str("track-limits field is unknown"),
            Self::Range { field, max } => {
                write!(formatter, "track-limits field {field} must be 1..={max}")
            }
        }
    }
}

impl std::error::Error for TrackLimitsError {}

impl TrackLimits {
    /// Builds limits from optional seconds and bytes.
    ///
    /// # Errors
    ///
    /// Refuses zero or past [`MAX_TRACK_SEGMENT_SECONDS`] seconds and zero
    /// or past [`MAX_TRACK_SEGMENT_BYTES`] bytes.
    pub fn new(
        max_segment_seconds: Option<u32>,
        max_segment_bytes: Option<u64>,
    ) -> Result<Self, TrackLimitsError> {
        let seconds = match max_segment_seconds {
            None => None,
            Some(value) if (1..=MAX_TRACK_SEGMENT_SECONDS).contains(&value) => {
                NonZeroU32::new(value)
            }
            Some(_) => {
                return Err(TrackLimitsError::Range {
                    field: "max-seconds",
                    max: u64::from(MAX_TRACK_SEGMENT_SECONDS),
                });
            }
        };
        let bytes = match max_segment_bytes {
            None => None,
            Some(value) if (1..=MAX_TRACK_SEGMENT_BYTES).contains(&value) => NonZeroU64::new(value),
            Some(_) => {
                return Err(TrackLimitsError::Range {
                    field: "max-bytes",
                    max: MAX_TRACK_SEGMENT_BYTES,
                });
            }
        };
        Ok(Self {
            max_segment_seconds: seconds,
            max_segment_bytes: bytes,
        })
    }

    /// The rollover span in seconds, or `None` when the limit is off.
    #[must_use]
    pub const fn max_segment_seconds(self) -> Option<u32> {
        match self.max_segment_seconds {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    /// The rollover size in bytes, or `None` when the limit is off.
    #[must_use]
    pub const fn max_segment_bytes(self) -> Option<u64> {
        match self.max_segment_bytes {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    /// The canonical document, with the fields that are set.
    #[must_use]
    pub fn to_document(self) -> String {
        use core::fmt::Write as _;
        let mut document = format!("{TRACK_LIMITS_HEADER}\n");
        if let Some(seconds) = self.max_segment_seconds {
            let _ = writeln!(document, "max-seconds={}", seconds.get());
        }
        if let Some(bytes) = self.max_segment_bytes {
            let _ = writeln!(document, "max-bytes={}", bytes.get());
        }
        document
    }

    /// Decodes the canonical document strictly, refusing anything an older
    /// or newer writer would not produce.
    ///
    /// # Errors
    ///
    /// Returns the header, line, duplicate, order, key, or range error of
    /// the first deviation.
    pub fn from_document(text: &str) -> Result<Self, TrackLimitsError> {
        // Every line, including the last, ends with a newline; a truncated
        // final line is not a document this writer could have produced.
        let Some(body) = text.strip_suffix('\n') else {
            return Err(TrackLimitsError::Line);
        };
        let mut lines = body.split('\n');
        if lines.next() != Some(TRACK_LIMITS_HEADER) {
            return Err(TrackLimitsError::Header);
        }
        let mut seconds = None;
        let mut bytes = None;
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                return Err(TrackLimitsError::Line);
            };
            let value = value.parse::<u64>().map_err(|_| TrackLimitsError::Line)?;
            match key {
                "max-seconds" => {
                    if seconds.is_some() {
                        return Err(TrackLimitsError::Duplicate("max-seconds"));
                    }
                    if bytes.is_some() {
                        return Err(TrackLimitsError::Order);
                    }
                    if value == 0 || value > u64::from(MAX_TRACK_SEGMENT_SECONDS) {
                        return Err(TrackLimitsError::Range {
                            field: "max-seconds",
                            max: u64::from(MAX_TRACK_SEGMENT_SECONDS),
                        });
                    }
                    seconds = u32::try_from(value).ok();
                }
                "max-bytes" => {
                    if bytes.is_some() {
                        return Err(TrackLimitsError::Duplicate("max-bytes"));
                    }
                    if value == 0 || value > MAX_TRACK_SEGMENT_BYTES {
                        return Err(TrackLimitsError::Range {
                            field: "max-bytes",
                            max: MAX_TRACK_SEGMENT_BYTES,
                        });
                    }
                    bytes = Some(value);
                }
                _ => return Err(TrackLimitsError::UnknownKey),
            }
        }
        Self::new(seconds, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_round_trip_canonically() {
        let limits = TrackLimits::new(Some(600), Some(1 << 30)).unwrap();
        let document = limits.to_document();
        assert_eq!(
            document,
            "freemix-record-track-limits 1\nmax-seconds=600\nmax-bytes=1073741824\n"
        );
        assert_eq!(TrackLimits::from_document(&document).unwrap(), limits);
        assert_eq!(limits.to_document(), document);
    }

    #[test]
    fn an_unlimited_map_writes_only_the_header() {
        let limits = TrackLimits::default();
        assert_eq!(limits.to_document(), "freemix-record-track-limits 1\n");
        assert_eq!(
            TrackLimits::from_document("freemix-record-track-limits 1\n").unwrap(),
            limits
        );
    }

    #[test]
    fn limits_are_strict() {
        for document in [
            "",
            "freemix-record-track-limits 2\n",
            "freemix-record-track-limits 1",
            "freemix-record-track-limits 1\nextra=1\n",
            "freemix-record-track-limits 1\nmax-seconds=0\n",
            "freemix-record-track-limits 1\nmax-seconds=86401\n",
            "freemix-record-track-limits 1\nmax-bytes=0\n",
            "freemix-record-track-limits 1\nmax-bytes=1099511627777\n",
            "freemix-record-track-limits 1\nmax-seconds=10\nmax-seconds=20\n",
            "freemix-record-track-limits 1\nmax-bytes=10\nmax-seconds=20\n",
            "freemix-record-track-limits 1\nmax-seconds=ten\n",
            "freemix-record-track-limits 1\nmax-seconds\n",
        ] {
            assert!(
                TrackLimits::from_document(document).is_err(),
                "accepted {document:?}"
            );
        }
    }

    #[test]
    fn limits_refuse_out_of_range_values() {
        assert!(TrackLimits::new(Some(0), None).is_err());
        assert!(TrackLimits::new(Some(86_401), None).is_err());
        assert!(TrackLimits::new(None, Some(0)).is_err());
        assert!(TrackLimits::new(None, Some(MAX_TRACK_SEGMENT_BYTES + 1)).is_err());
        assert!(TrackLimits::new(Some(86_400), Some(MAX_TRACK_SEGMENT_BYTES)).is_ok());
    }
}
