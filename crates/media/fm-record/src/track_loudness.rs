//! Canonical recorder track loudness metadata (AU-010).
//!
//! The project may state the loudness figures that every recorder track WAV
//! carries in its version-2 BWF `bext` chunk, measured upstream:
//!
//! ```text
//! freemix-record-track-loudness 1
//! integrated=-23.00
//! range=7.50
//! true-peak=-1.20
//! momentary=-22.50
//! short-term=-23.10
//! ```
//!
//! Every field is required, in that order. Values are hundredths of LUFS,
//! LU, or dBTP and must fit `i16`; the document is canonical so a rewrite is
//! byte-identical and is decoded strictly.

use core::fmt;

use crate::wav::BextLoudness;

/// The canonical document's header line.
pub const TRACK_LOUDNESS_HEADER: &str = "freemix-record-track-loudness 1";
/// File name of the loudness document inside a bundle's `recorders`
/// directory.
pub const TRACK_LOUDNESS_FILE_NAME: &str = "loudness.trackloudness";

/// The document's field order.
const FIELDS: [&str; 5] = [
    "integrated",
    "range",
    "true-peak",
    "momentary",
    "short-term",
];

/// Why a loudness document could not be decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackLoudnessError {
    /// The first line is not the current document header.
    Header,
    /// A line is not `key=value`.
    Line,
    /// A required field is missing.
    Missing(&'static str),
    /// A field appears twice.
    Duplicate(&'static str),
    /// A field appears after one that must follow it.
    Order,
    /// A line names a key this document does not define.
    UnknownKey,
    /// A value is not a signed decimal with at most two fraction digits, or
    /// it does not fit the hundredths range.
    Value(&'static str),
}

impl fmt::Display for TrackLoudnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => formatter.write_str("track-loudness document header is unknown"),
            Self::Line => formatter.write_str("track-loudness line is malformed"),
            Self::Missing(field) => write!(formatter, "track-loudness field {field} is missing"),
            Self::Duplicate(field) => {
                write!(formatter, "track-loudness field {field} is duplicated")
            }
            Self::Order => formatter.write_str("track-loudness fields are out of order"),
            Self::UnknownKey => formatter.write_str("track-loudness field is unknown"),
            Self::Value(field) => write!(formatter, "track-loudness field {field} is invalid"),
        }
    }
}

impl std::error::Error for TrackLoudnessError {}

/// The canonical document for `loudness`.
#[must_use]
pub fn encode_track_loudness(loudness: BextLoudness) -> String {
    format!(
        "{TRACK_LOUDNESS_HEADER}\n\
         integrated={}\n\
         range={}\n\
         true-peak={}\n\
         momentary={}\n\
         short-term={}\n",
        hundredths(loudness.integrated_lufs),
        hundredths(loudness.range_lu),
        hundredths(loudness.max_true_peak_dbtp),
        hundredths(loudness.max_momentary_lufs),
        hundredths(loudness.max_short_term_lufs),
    )
}

/// Decodes the canonical document strictly.
///
/// # Errors
///
/// Returns the header, line, missing, duplicate, order, key, or value error
/// of the first deviation.
pub fn decode_track_loudness(text: &str) -> Result<BextLoudness, TrackLoudnessError> {
    let Some(body) = text.strip_suffix('\n') else {
        return Err(TrackLoudnessError::Line);
    };
    let mut lines = body.split('\n');
    if lines.next() != Some(TRACK_LOUDNESS_HEADER) {
        return Err(TrackLoudnessError::Header);
    }
    let mut values = [0_i16; 5];
    let mut seen = [false; 5];
    let mut next = 0;
    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            return Err(TrackLoudnessError::Line);
        };
        let Some(position) = FIELDS.iter().position(|field| *field == key) else {
            return Err(TrackLoudnessError::UnknownKey);
        };
        if seen[position] {
            return Err(TrackLoudnessError::Duplicate(FIELDS[position]));
        }
        if position != next {
            return Err(TrackLoudnessError::Order);
        }
        values[position] =
            parse_hundredths(value).ok_or(TrackLoudnessError::Value(FIELDS[position]))?;
        seen[position] = true;
        next += 1;
    }
    if next < FIELDS.len() {
        return Err(TrackLoudnessError::Missing(FIELDS[next]));
    }
    Ok(BextLoudness {
        integrated_lufs: values[0],
        range_lu: values[1],
        max_true_peak_dbtp: values[2],
        max_momentary_lufs: values[3],
        max_short_term_lufs: values[4],
    })
}

fn hundredths(value: i16) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let magnitude = value.unsigned_abs();
    format!("{sign}{}.{:02}", magnitude / 100, magnitude % 100)
}

fn parse_hundredths(text: &str) -> Option<i16> {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(digits) => (-1_i32, digits),
        None => (1, text),
    };
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole: i32 = whole.parse().ok()?;
    let fraction: i32 = match fraction.len() {
        0 => 0,
        1 => i32::from(fraction.as_bytes()[0] - b'0') * 10,
        _ => {
            i32::from(fraction.as_bytes()[0] - b'0') * 10 + i32::from(fraction.as_bytes()[1] - b'0')
        }
    };
    i16::try_from(sign * (whole * 100 + fraction)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loudness() -> BextLoudness {
        BextLoudness {
            integrated_lufs: -2300,
            range_lu: 750,
            max_true_peak_dbtp: -120,
            max_momentary_lufs: -2250,
            max_short_term_lufs: -2310,
        }
    }

    #[test]
    fn loudness_round_trips_canonically() {
        let document = encode_track_loudness(loudness());
        assert_eq!(
            document,
            "freemix-record-track-loudness 1\n\
             integrated=-23.00\n\
             range=7.50\n\
             true-peak=-1.20\n\
             momentary=-22.50\n\
             short-term=-23.10\n"
        );
        assert_eq!(decode_track_loudness(&document).unwrap(), loudness());
    }

    #[test]
    fn values_accept_zero_to_two_fraction_digits() {
        let document = "freemix-record-track-loudness 1\nintegrated=-23\nrange=7.5\ntrue-peak=0\nmomentary=-22.5\nshort-term=-23.1\n";
        assert_eq!(
            decode_track_loudness(document).unwrap(),
            BextLoudness {
                integrated_lufs: -2300,
                range_lu: 750,
                max_true_peak_dbtp: 0,
                max_momentary_lufs: -2250,
                max_short_term_lufs: -2310,
            }
        );
    }

    #[test]
    fn loudness_documents_are_strict() {
        for document in [
            "",
            "freemix-record-track-loudness 2\n",
            "freemix-record-track-loudness 1",
            "freemix-record-track-loudness 1\nintegrated=-23.00\n",
            "freemix-record-track-loudness 1\nintegrated=-23.00\nrange=7.50\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\nextra=1\n",
            "freemix-record-track-loudness 1\nrange=7.50\nintegrated=-23.00\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\n",
            "freemix-record-track-loudness 1\nintegrated=-23.00\nintegrated=-24.00\nrange=7.50\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\n",
            "freemix-record-track-loudness 1\nintegrated=-328.00\nrange=7.50\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\n",
            "freemix-record-track-loudness 1\nintegrated=-23.001\nrange=7.50\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\n",
            "freemix-record-track-loudness 1\nintegrated=abc\nrange=7.50\ntrue-peak=-1.20\nmomentary=-22.50\nshort-term=-23.10\n",
        ] {
            assert!(
                decode_track_loudness(document).is_err(),
                "accepted {document:?}"
            );
        }
    }
}
