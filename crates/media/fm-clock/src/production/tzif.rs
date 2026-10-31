//! IANA `TZif` (zoneinfo) parsing (`SW-013`).
//!
//! A `TZif` file carries a table of UTC transition instants and local-time
//! types. This module parses version 1, 2, 3, and 4 files with 64-bit
//! transitions and answers a UTC instant's offset without reading a system
//! clock. It does not pick a zone by name; callers supply the bytes or a path.

use core::fmt;
use std::{fs, path::Path};

/// Largest `TZif` file accepted, in bytes.
pub const MAX_TZIF_BYTES: u64 = 256 * 1024;
/// Most transitions one file may carry.
const MAX_TRANSITIONS: usize = 100_000;
/// Most local-time types one file may carry.
const MAX_TYPES: usize = 256;
/// Most abbreviation bytes one file may carry.
const MAX_ABBREVIATIONS: usize = 64 * 1024;

/// One local-time type in a `TZif` table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TzifType {
    /// Offset from UTC in seconds.
    pub offset_seconds: i32,
    /// Whether the type observes daylight saving.
    pub is_dst: bool,
    /// The zone abbreviation (for example `EST` or `EDT`).
    pub abbreviation: String,
}

/// A parsed `TZif` zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TzifZone {
    transitions: Vec<i64>,
    transition_types: Vec<u8>,
    types: Vec<TzifType>,
    first_standard: usize,
}

impl TzifZone {
    /// Parses one `TZif` file.
    ///
    /// # Errors
    ///
    /// Returns [`TzifError`] for an oversized, malformed, or out-of-contract
    /// file.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TzifError> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if length > MAX_TZIF_BYTES {
            return Err(TzifError::TooLarge {
                bytes: length,
                maximum: MAX_TZIF_BYTES,
            });
        }
        let mut cursor = Cursor::new(bytes);
        let (version, first) = read_header(&mut cursor)?;
        let (counts, wide) = if version >= b'2' {
            let skip = data_len(&first, false)?;
            cursor.take(skip).ok_or(TzifError::Counts)?;
            let (_, counts) = read_header(&mut cursor)?;
            (counts, true)
        } else {
            (first, false)
        };
        let mut transitions = Vec::with_capacity(counts.timecnt);
        for _ in 0..counts.timecnt {
            transitions.push(if wide {
                cursor.i64().ok_or(TzifError::Counts)?
            } else {
                i64::from(cursor.i32().ok_or(TzifError::Counts)?)
            });
        }
        let mut transition_types = Vec::with_capacity(counts.timecnt);
        for _ in 0..counts.timecnt {
            transition_types.push(cursor.u8().ok_or(TzifError::Counts)?);
        }
        if transition_types
            .iter()
            .any(|index| usize::from(*index) >= counts.typecnt)
        {
            return Err(TzifError::TypeIndex);
        }
        let mut raw_types = Vec::with_capacity(counts.typecnt);
        for _ in 0..counts.typecnt {
            let offset_seconds = cursor.i32().ok_or(TzifError::Counts)?;
            let is_dst = cursor.u8().ok_or(TzifError::Counts)?;
            let abbreviation = cursor.u8().ok_or(TzifError::Counts)?;
            raw_types.push((offset_seconds, is_dst, abbreviation));
        }
        let abbreviations = cursor.take(counts.charcnt).ok_or(TzifError::Counts)?;
        let mut types = Vec::with_capacity(counts.typecnt);
        for (offset_seconds, is_dst, index) in raw_types {
            let rest = abbreviations
                .get(usize::from(index)..)
                .ok_or(TzifError::AbbreviationIndex)?;
            let end = rest
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(TzifError::AbbreviationIndex)?;
            let abbreviation = core::str::from_utf8(&rest[..end])
                .map_err(|_| TzifError::AbbreviationIndex)?
                .to_owned();
            types.push(TzifType {
                offset_seconds,
                is_dst: is_dst != 0,
                abbreviation,
            });
        }
        let first_standard = types.iter().position(|kind| !kind.is_dst).unwrap_or(0);
        Ok(Self {
            transitions,
            transition_types,
            types,
            first_standard,
        })
    }

    /// Reads and parses one `TZif` file.
    ///
    /// # Errors
    ///
    /// Returns [`TzifError::TooLarge`] above [`MAX_TZIF_BYTES`], the file read
    /// error, or a parse error.
    pub fn from_path(path: &Path) -> Result<Self, TzifError> {
        let metadata = fs::metadata(path).map_err(|error| TzifError::Io(error.to_string()))?;
        if metadata.len() > MAX_TZIF_BYTES {
            return Err(TzifError::TooLarge {
                bytes: metadata.len(),
                maximum: MAX_TZIF_BYTES,
            });
        }
        let bytes = fs::read(path).map_err(|error| TzifError::Io(error.to_string()))?;
        Self::from_bytes(&bytes)
    }

    /// The local-time type in effect at `unix_seconds`.
    #[must_use]
    pub fn offset_at(&self, unix_seconds: i64) -> Option<&TzifType> {
        let reached = self
            .transitions
            .partition_point(|transition| *transition <= unix_seconds);
        let index = if reached == 0 {
            self.first_standard
        } else {
            usize::from(*self.transition_types.get(reached - 1)?)
        };
        self.types.get(index).or_else(|| self.types.first())
    }

    /// The local wall-clock seconds at UTC `unix_seconds`, when representable.
    #[must_use]
    pub fn local_at(&self, unix_seconds: i64) -> Option<i64> {
        self.offset_at(unix_seconds)
            .and_then(|kind| unix_seconds.checked_add(i64::from(kind.offset_seconds)))
    }

    /// How many transitions the file carries.
    #[must_use]
    pub fn transition_count(&self) -> usize {
        self.transitions.len()
    }
}

/// Why a `TZif` file is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TzifError {
    TooLarge { bytes: u64, maximum: u64 },
    Header,
    Counts,
    TypeIndex,
    AbbreviationIndex,
    Io(String),
}

impl fmt::Display for TzifError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, maximum } => {
                write!(
                    formatter,
                    "TZif file is {bytes} bytes, maximum is {maximum}"
                )
            }
            Self::Header => formatter.write_str("TZif file has an invalid header"),
            Self::Counts => formatter.write_str("TZif file counts exceed its bytes"),
            Self::TypeIndex => formatter.write_str("TZif transition names an unknown type"),
            Self::AbbreviationIndex => {
                formatter.write_str("TZif type names an unknown abbreviation")
            }
            Self::Io(error) => write!(formatter, "TZif file I/O failed: {error}"),
        }
    }
}

impl std::error::Error for TzifError {}

struct Counts {
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
    leapcnt: usize,
    ttisstdcnt: usize,
    ttisgmtcnt: usize,
}

fn read_header(cursor: &mut Cursor<'_>) -> Result<(u8, Counts), TzifError> {
    if cursor.take(4) != Some(b"TZif") {
        return Err(TzifError::Header);
    }
    let version = cursor.u8().ok_or(TzifError::Header)?;
    cursor.take(15).ok_or(TzifError::Header)?;
    let mut count = || cursor.u32().ok_or(TzifError::Header);
    let counts = Counts {
        ttisgmtcnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
        ttisstdcnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
        leapcnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
        timecnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
        typecnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
        charcnt: usize::try_from(count()?).map_err(|_| TzifError::Counts)?,
    };
    if counts.typecnt == 0
        || counts.typecnt > MAX_TYPES
        || counts.timecnt > MAX_TRANSITIONS
        || counts.charcnt > MAX_ABBREVIATIONS
    {
        return Err(TzifError::Counts);
    }
    Ok((version, counts))
}

fn data_len(counts: &Counts, wide: bool) -> Result<usize, TzifError> {
    let time = if wide { 8 } else { 4 };
    let leap = if wide { 12 } else { 8 };
    counts
        .timecnt
        .checked_mul(time)
        .and_then(|length| length.checked_add(counts.timecnt))
        .and_then(|length| length.checked_add(counts.typecnt.checked_mul(6)?))
        .and_then(|length| length.checked_add(counts.charcnt))
        .and_then(|length| length.checked_add(counts.leapcnt.checked_mul(leap)?))
        .and_then(|length| length.checked_add(counts.ttisstdcnt))
        .and_then(|length| length.checked_add(counts.ttisgmtcnt))
        .ok_or(TzifError::Counts)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(length)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).and_then(|bytes| bytes.first().copied())
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(i32::from_be_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
            .map(i64::from_be_bytes)
    }
}
