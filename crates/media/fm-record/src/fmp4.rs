//! Fragmented MP4 (ISO BMFF) integrity scanning, crash repair, and splitting.
//!
//! A fragmented MP4 written with an empty `moov` and `default_base_moof`
//! fragments, which is what the `FFmpeg` program recorder emits, is
//! crash-tolerant by layout: the init segment (`ftyp` + `moov` with `mvex`) is
//! complete before any media, and every `moof` + `mdat` pair is independently
//! decodable. A writer that is killed, or a host that loses power, mid-write
//! leaves at most one torn fragment at the end of the file.
//!
//! [`scan_fmp4_file`] validates every fragment's sample table against the
//! bytes of its `mdat` and reports exactly which fragments and media spans are
//! intact. [`repair_fmp4_file`] truncates a torn tail back to the last complete
//! fragment, so the result is a playable fragmented MP4. [`Fmp4Splitter`] cuts
//! a live byte stream into relocatable units (the init segment, then whole
//! fragments) so a writer can re-home a fragment into a new segment file after
//! an I/O failure without losing it.
//!
//! Parsing is bounded: the init segment and each `moof` are read into memory
//! only up to [`MAX_FMP4_INIT_BYTES`] and [`MAX_FMP4_MOOF_BYTES`], `mdat`
//! payloads are never read, and sample counts are checked against the bytes
//! that carry them before any per-sample work.

use std::{
    fmt,
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    path::{Path, PathBuf},
};

/// Largest `moov` box (and splitter init unit) accepted.
pub const MAX_FMP4_INIT_BYTES: u64 = 16 * 1024 * 1024;
/// Largest `moof` box accepted.
pub const MAX_FMP4_MOOF_BYTES: u64 = 16 * 1024 * 1024;
/// Most tracks a movie may declare.
pub const MAX_FMP4_TRACKS: usize = 64;
const MAX_TRUN_SAMPLES: u64 = 1 << 24;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

type Fourcc = [u8; 4];

/// One track declared by the init segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmp4Track {
    pub track_id: u32,
    /// Media timescale from `mdhd`, in ticks per second. Never zero.
    pub timescale: u32,
    /// Handler type from `hdlr`, such as `vide` or `soun`.
    pub handler: [u8; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrackDefaults {
    duration: u32,
    size: u32,
}

/// Track layout parsed from a fragmented `moov`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fmp4Movie {
    tracks: Vec<Fmp4Track>,
    defaults: Vec<TrackDefaults>,
}

impl Fmp4Movie {
    /// Tracks in `moov` order.
    #[must_use]
    pub fn tracks(&self) -> &[Fmp4Track] {
        &self.tracks
    }

    fn position(&self, track_id: u32) -> Option<usize> {
        self.tracks
            .iter()
            .position(|track| track.track_id == track_id)
    }
}

/// Media carried by one `traf` of a fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmp4TrackRun {
    pub track_id: u32,
    /// Decode time of the first sample, from `tfdt` or continued from the
    /// previous fragment of the same track.
    pub base_decode_time: u64,
    /// Sum of sample durations in track timescale ticks.
    pub duration: u64,
    pub samples: u64,
    pub data_bytes: u64,
}

/// One validated `moof` + `mdat` fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fmp4Fragment {
    /// `mfhd` sequence number.
    pub sequence_number: u32,
    /// Byte offset of the fragment (including any leading `styp`/`sidx`
    /// boxes for splitter units) in its file or stream.
    pub offset: u64,
    pub length: u64,
    pub runs: Vec<Fmp4TrackRun>,
}

impl Fmp4Fragment {
    /// Longest per-track duration in the fragment, in nanoseconds.
    #[must_use]
    pub fn duration_nanos(&self, movie: &Fmp4Movie) -> u64 {
        self.runs
            .iter()
            .filter_map(|run| {
                movie
                    .position(run.track_id)
                    .map(|index| ticks_to_nanos(run.duration, movie.tracks[index].timescale))
            })
            .max()
            .unwrap_or(0)
    }
}

/// Decode-time span covered by one track across the intact fragments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmp4TrackSpan {
    pub track_id: u32,
    pub timescale: u32,
    pub first_decode_time: u64,
    pub end_decode_time: u64,
    pub samples: u64,
}

impl Fmp4TrackSpan {
    #[must_use]
    pub fn duration_nanos(&self) -> u64 {
        ticks_to_nanos(
            self.end_decode_time.saturating_sub(self.first_decode_time),
            self.timescale,
        )
    }
}

/// What the end of a scanned file looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmp4Integrity {
    /// The file ends with a complete `mfra` index after its last fragment.
    Finalized,
    /// The file ends exactly at a fragment boundary (or right after the init
    /// segment). It is playable as is.
    Complete,
    /// Bytes from `offset` to the end are incomplete or invalid. Truncating
    /// to `offset` leaves a complete file.
    Torn { reason: &'static str, offset: u64 },
    /// The init segment is missing, torn, or not fragmented. Nothing in the
    /// file can be recovered by truncation.
    Unrecoverable { reason: &'static str, offset: u64 },
}

/// Result of scanning one fragmented MP4 file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fmp4Scan {
    pub file_length: u64,
    /// End of the init segment; zero when unrecoverable.
    pub init_length: u64,
    pub movie: Option<Fmp4Movie>,
    pub fragments: u64,
    pub first_sequence: Option<u32>,
    pub last_sequence: Option<u32>,
    /// Count of fragments whose `mfhd` sequence number did not follow the
    /// previous one by exactly one.
    pub sequence_gaps: u64,
    pub spans: Vec<Fmp4TrackSpan>,
    /// Length of the longest playable prefix.
    pub valid_length: u64,
    pub integrity: Fmp4Integrity,
    /// The torn fragment, when its `moof` was complete and consistent but its
    /// `mdat` was cut short, so the lost media span is known exactly.
    pub lost_fragment: Option<Fmp4Fragment>,
}

impl Fmp4Scan {
    fn empty(file_length: u64) -> Self {
        Self {
            file_length,
            init_length: 0,
            movie: None,
            fragments: 0,
            first_sequence: None,
            last_sequence: None,
            sequence_gaps: 0,
            spans: Vec::new(),
            valid_length: 0,
            integrity: Fmp4Integrity::Complete,
            lost_fragment: None,
        }
    }

    /// Whether the scanned prefix up to [`Self::valid_length`] is playable.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        !matches!(self.integrity, Fmp4Integrity::Unrecoverable { .. })
    }

    /// Bytes that are not part of a playable prefix: the torn tail, or the
    /// whole file when unrecoverable.
    #[must_use]
    pub fn unusable_bytes(&self) -> u64 {
        self.file_length.saturating_sub(self.valid_length)
    }

    /// Longest intact track span, in nanoseconds.
    #[must_use]
    pub fn recovered_duration_nanos(&self) -> u64 {
        self.spans
            .iter()
            .map(Fmp4TrackSpan::duration_nanos)
            .max()
            .unwrap_or(0)
    }

    /// Media span of the torn fragment, when it is known.
    #[must_use]
    pub fn lost_duration_nanos(&self) -> Option<u64> {
        let movie = self.movie.as_ref()?;
        self.lost_fragment
            .as_ref()
            .map(|fragment| fragment.duration_nanos(movie))
    }
}

/// Outcome of [`repair_fmp4_file`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fmp4Repair {
    /// The scan taken before any truncation.
    pub scan: Fmp4Scan,
    /// Bytes removed from the end of the file; zero unless the scan was torn.
    pub truncated_bytes: u64,
}

#[derive(Debug)]
pub struct Fmp4Error {
    pub path: PathBuf,
    pub source: io::Error,
}

impl fmt::Display for Fmp4Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for Fmp4Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Scans a fragmented MP4 file without modifying it.
///
/// # Errors
///
/// Returns an error only when the file cannot be opened or read; malformed
/// content is reported through [`Fmp4Scan::integrity`].
pub fn scan_fmp4_file(path: &Path) -> Result<Fmp4Scan, Fmp4Error> {
    let wrap = |source| Fmp4Error {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::File::open(path).map_err(wrap)?;
    let length = file.metadata().map_err(wrap)?.len();
    scan_fmp4(&mut file, length).map_err(wrap)
}

/// Scans a fragmented MP4 file and truncates a torn tail back to the last
/// complete fragment, then syncs the file.
///
/// Complete, finalized, and unrecoverable files are left untouched.
///
/// # Errors
///
/// Returns an error when the file cannot be opened, read, truncated, or
/// synced.
pub fn repair_fmp4_file(path: &Path) -> Result<Fmp4Repair, Fmp4Error> {
    let wrap = |source| Fmp4Error {
        path: path.to_path_buf(),
        source,
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(wrap)?;
    let length = file.metadata().map_err(wrap)?.len();
    let scan = scan_fmp4(&mut file, length).map_err(wrap)?;
    let truncated_bytes = if matches!(scan.integrity, Fmp4Integrity::Torn { .. }) {
        file.set_len(scan.valid_length).map_err(wrap)?;
        file.sync_all().map_err(wrap)?;
        length - scan.valid_length
    } else {
        0
    };
    Ok(Fmp4Repair {
        scan,
        truncated_bytes,
    })
}

/// Scans `length` bytes of fragmented MP4 from `reader`.
///
/// # Errors
///
/// Returns read and seek errors only.
#[allow(clippy::too_many_lines)]
pub fn scan_fmp4<R: Read + Seek>(reader: &mut R, length: u64) -> io::Result<Fmp4Scan> {
    let mut scan = Fmp4Scan::empty(length);
    let mut offset = 0u64;
    let mut seen_ftyp = false;
    let unrecoverable = |mut scan: Fmp4Scan, reason, offset| {
        scan.integrity = Fmp4Integrity::Unrecoverable { reason, offset };
        scan.valid_length = 0;
        scan.init_length = 0;
        scan
    };
    let movie = loop {
        let header = match read_top_header(reader, offset, length)? {
            Header::Box(header) => header,
            Header::Incomplete => {
                return Ok(unrecoverable(scan, "init segment is incomplete", offset));
            }
            Header::Invalid(reason) => return Ok(unrecoverable(scan, reason, offset)),
        };
        let Some(end) = offset.checked_add(header.size).filter(|end| *end <= length) else {
            return Ok(unrecoverable(scan, "init segment is incomplete", offset));
        };
        match &header.kind {
            b"ftyp" if !seen_ftyp && offset == 0 => seen_ftyp = true,
            _ if !seen_ftyp => {
                return Ok(unrecoverable(scan, "file does not start with ftyp", offset));
            }
            b"moov" => {
                if header.size > MAX_FMP4_INIT_BYTES {
                    return Ok(unrecoverable(scan, "moov exceeds its bound", offset));
                }
                let payload = read_payload(reader, offset, header)?;
                match parse_moov(&payload) {
                    Ok(movie) => {
                        offset = end;
                        break movie;
                    }
                    Err(reason) => return Ok(unrecoverable(scan, reason, offset)),
                }
            }
            b"moof" | b"mdat" | b"mfra" | b"ftyp" => {
                return Ok(unrecoverable(
                    scan,
                    "media precedes a fragmented moov",
                    offset,
                ));
            }
            _ => {}
        }
        offset = end;
    };
    scan.init_length = offset;
    scan.valid_length = offset;
    let mut next_decode = vec![0u64; movie.tracks.len()];
    let mut spans = SpanAccumulator::new(movie.tracks.len());
    let integrity = loop {
        let torn = |reason, scan: &Fmp4Scan| Fmp4Integrity::Torn {
            reason,
            offset: scan.valid_length,
        };
        if offset == length {
            break if offset == scan.valid_length {
                Fmp4Integrity::Complete
            } else {
                torn("trailing boxes without a fragment", &scan)
            };
        }
        let header = match read_top_header(reader, offset, length)? {
            Header::Box(header) => header,
            Header::Incomplete => break torn("torn box header", &scan),
            Header::Invalid(reason) => break torn(reason, &scan),
        };
        let Some(end) = offset.checked_add(header.size) else {
            break torn("box size overflows", &scan);
        };
        match &header.kind {
            b"moof" => {
                if header.size > MAX_FMP4_MOOF_BYTES {
                    break torn("moof exceeds its bound", &scan);
                }
                if end > length {
                    break torn("torn moof", &scan);
                }
                let moof = read_payload(reader, offset, header)?;
                let mdat = match read_top_header(reader, end, length)? {
                    Header::Box(mdat) if &mdat.kind == b"mdat" => mdat,
                    Header::Box(_) => break torn("moof is not followed by mdat", &scan),
                    Header::Incomplete => break torn("torn mdat header", &scan),
                    Header::Invalid(reason) => break torn(reason, &scan),
                };
                let Some(mdat_end) = end.checked_add(mdat.size) else {
                    break torn("box size overflows", &scan);
                };
                let mut trial = next_decode.clone();
                let parsed = parse_moof(
                    &moof,
                    offset,
                    end + mdat.header_len..mdat_end,
                    &movie,
                    &mut trial,
                    true,
                );
                if mdat_end > length {
                    scan.lost_fragment = parsed.ok().map(|(sequence_number, runs)| Fmp4Fragment {
                        sequence_number,
                        offset,
                        length: mdat_end - offset,
                        runs,
                    });
                    break torn("torn mdat", &scan);
                }
                let (sequence, runs) = match parsed {
                    Ok(parsed) => parsed,
                    Err(reason) => break torn(reason, &scan),
                };
                next_decode = trial;
                spans.absorb(&movie, &runs);
                if let Some(last) = scan.last_sequence
                    && last.checked_add(1) != Some(sequence)
                {
                    scan.sequence_gaps += 1;
                }
                scan.first_sequence.get_or_insert(sequence);
                scan.last_sequence = Some(sequence);
                scan.fragments += 1;
                offset = mdat_end;
                scan.valid_length = offset;
            }
            b"mfra" => {
                if end > length {
                    break torn("torn mfra", &scan);
                }
                scan.valid_length = end;
                break if end == length {
                    Fmp4Integrity::Finalized
                } else {
                    torn("data after mfra", &scan)
                };
            }
            b"mdat" => break torn("mdat without moof", &scan),
            b"ftyp" | b"moov" => break torn("init box inside the fragment sequence", &scan),
            _ => {
                if end > length {
                    break torn("torn box", &scan);
                }
                offset = end;
            }
        }
    };
    scan.integrity = integrity;
    scan.spans = spans.finish(&movie);
    scan.movie = Some(movie);
    Ok(scan)
}

/// A relocatable unit cut from a fragmented MP4 byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fmp4Unit {
    /// Every byte before the first `moof`: `ftyp`, `moov`, and any free boxes.
    Init { bytes: Vec<u8>, movie: Fmp4Movie },
    /// One validated fragment, including any `styp`/`sidx`/`prft`/`emsg`
    /// boxes that preceded its `moof`. Data offsets inside it are relative to
    /// its own `moof`, so it can be appended to any segment after the init
    /// unit.
    Fragment {
        bytes: Vec<u8>,
        fragment: Fmp4Fragment,
    },
    /// A trailing `mfra` index. Its offsets describe the original stream, so a
    /// writer that re-homes fragments must not copy it into a segment.
    Index { bytes: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// The stream is not a relocatable fragmented MP4 at `offset`.
    Invalid { reason: &'static str, offset: u64 },
    /// A unit would exceed the splitter's byte bound.
    UnitTooLarge { offset: u64, bytes: u64, limit: u64 },
    /// The stream ended inside a unit.
    Truncated { buffered: usize },
    /// An earlier error stopped the splitter.
    Poisoned,
}

impl fmt::Display for SplitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { reason, offset } => {
                write!(
                    formatter,
                    "invalid fragmented MP4 at byte {offset}: {reason}"
                )
            }
            Self::UnitTooLarge {
                offset,
                bytes,
                limit,
            } => write!(
                formatter,
                "fragmented MP4 unit at byte {offset} needs {bytes} bytes, above the {limit}-byte bound"
            ),
            Self::Truncated { buffered } => {
                write!(
                    formatter,
                    "stream ended inside a unit with {buffered} bytes buffered"
                )
            }
            Self::Poisoned => formatter.write_str("splitter stopped after an earlier error"),
        }
    }
}

impl std::error::Error for SplitError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Init { seen_ftyp: bool },
    Fragments,
    Done,
}

/// Incremental, bounded splitter from a fragmented MP4 byte stream into
/// [`Fmp4Unit`]s.
#[derive(Debug)]
pub struct Fmp4Splitter {
    buffer: Vec<u8>,
    scan_pos: usize,
    phase: Phase,
    movie: Option<Fmp4Movie>,
    next_decode: Vec<u64>,
    stream_offset: u64,
    max_unit_bytes: u64,
    poisoned: bool,
}

impl Fmp4Splitter {
    /// Creates a splitter that refuses any unit larger than `max_unit_bytes`
    /// (at least 64 KiB is enforced).
    #[must_use]
    pub fn new(max_unit_bytes: u64) -> Self {
        Self {
            buffer: Vec::new(),
            scan_pos: 0,
            phase: Phase::Init { seen_ftyp: false },
            movie: None,
            next_decode: Vec::new(),
            stream_offset: 0,
            max_unit_bytes: max_unit_bytes.max(64 * 1024),
            poisoned: false,
        }
    }

    /// Bytes held for an incomplete unit.
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// The parsed movie once the init unit has been seen.
    #[must_use]
    pub fn movie(&self) -> Option<&Fmp4Movie> {
        self.movie.as_ref()
    }

    /// Appends stream bytes and moves every completed unit into `out`.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is not a relocatable fragmented MP4 or
    /// a unit exceeds the bound. Errors are sticky.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Fmp4Unit>) -> Result<(), SplitError> {
        if self.poisoned {
            return Err(SplitError::Poisoned);
        }
        self.buffer.extend_from_slice(bytes);
        let result = self.drain(out);
        if result.is_err() {
            self.poisoned = true;
            self.buffer = Vec::new();
        }
        result
    }

    /// Ends the stream, emitting an init unit for a stream that never produced
    /// a fragment.
    ///
    /// # Errors
    ///
    /// Returns [`SplitError::Truncated`] when bytes of an incomplete unit
    /// remain, or [`SplitError::Poisoned`] after an earlier error.
    pub fn finish(&mut self, out: &mut Vec<Fmp4Unit>) -> Result<(), SplitError> {
        if self.poisoned {
            return Err(SplitError::Poisoned);
        }
        if let (Phase::Init { .. }, Some(movie)) = (self.phase, &self.movie)
            && self.scan_pos == self.buffer.len()
        {
            let movie = movie.clone();
            let bytes = self.take(self.scan_pos);
            out.push(Fmp4Unit::Init { bytes, movie });
            self.phase = Phase::Fragments;
        }
        if self.buffer.is_empty() {
            Ok(())
        } else {
            self.poisoned = true;
            Err(SplitError::Truncated {
                buffered: self.buffer.len(),
            })
        }
    }

    fn take(&mut self, length: usize) -> Vec<u8> {
        let rest = self.buffer.split_off(length);
        self.stream_offset += length as u64;
        self.scan_pos = 0;
        std::mem::replace(&mut self.buffer, rest)
    }

    fn invalid(&self, reason: &'static str, at: usize) -> SplitError {
        SplitError::Invalid {
            reason,
            offset: self.stream_offset + at as u64,
        }
    }

    fn bounded_end(&self, start: usize, size: u64) -> Result<usize, SplitError> {
        let end = (start as u64).saturating_add(size);
        if end > self.max_unit_bytes {
            return Err(SplitError::UnitTooLarge {
                offset: self.stream_offset,
                bytes: end,
                limit: self.max_unit_bytes,
            });
        }
        usize::try_from(end).map_err(|_| self.invalid("box size overflows", start))
    }

    #[allow(clippy::too_many_lines)]
    fn drain(&mut self, out: &mut Vec<Fmp4Unit>) -> Result<(), SplitError> {
        loop {
            let at = self.scan_pos;
            let header = match parse_top_header(&self.buffer[at..]) {
                Header::Incomplete => return Ok(()),
                Header::Invalid(reason) => return Err(self.invalid(reason, at)),
                Header::Box(header) => header,
            };
            if self.phase == Phase::Done {
                return Err(self.invalid("data after mfra", at));
            }
            if let Phase::Init { seen_ftyp } = self.phase {
                if &header.kind == b"moof" {
                    let Some(movie) = self.movie.clone() else {
                        return Err(self.invalid("media precedes a fragmented moov", at));
                    };
                    let bytes = self.take(at);
                    out.push(Fmp4Unit::Init { bytes, movie });
                    self.phase = Phase::Fragments;
                    continue;
                }
                let end = self.bounded_end(at, header.size)?;
                if self.buffer.len() < end {
                    return Ok(());
                }
                match &header.kind {
                    b"ftyp" if !seen_ftyp && at == 0 => {
                        self.phase = Phase::Init { seen_ftyp: true };
                    }
                    _ if !seen_ftyp => {
                        return Err(self.invalid("stream does not start with ftyp", at));
                    }
                    b"moov" if self.movie.is_none() => {
                        let payload = &self.buffer[at + header.header_len_usize()..end];
                        let movie =
                            parse_moov(payload).map_err(|reason| self.invalid(reason, at))?;
                        self.next_decode = vec![0; movie.tracks.len()];
                        self.movie = Some(movie);
                    }
                    b"moov" | b"ftyp" | b"mdat" | b"mfra" => {
                        return Err(self.invalid("unexpected box in the init segment", at));
                    }
                    _ => {}
                }
                self.scan_pos = end;
                continue;
            }
            let end = self.bounded_end(at, header.size)?;
            match &header.kind {
                b"moof" => {
                    if self.buffer.len() < end {
                        return Ok(());
                    }
                    let mdat = match parse_top_header(&self.buffer[end..]) {
                        Header::Incomplete => return Ok(()),
                        Header::Invalid(reason) => return Err(self.invalid(reason, end)),
                        Header::Box(mdat) if &mdat.kind == b"mdat" => mdat,
                        Header::Box(_) => {
                            return Err(self.invalid("moof is not followed by mdat", end));
                        }
                    };
                    let mdat_end = self.bounded_end(end, mdat.size)?;
                    if self.buffer.len() < mdat_end {
                        return Ok(());
                    }
                    let Some(movie) = self.movie.as_ref() else {
                        return Err(self.invalid("fragment before moov", at));
                    };
                    let mut trial = self.next_decode.clone();
                    let (sequence_number, runs) = parse_moof(
                        &self.buffer[at + header.header_len_usize()..end],
                        at as u64,
                        (end as u64 + mdat.header_len)..mdat_end as u64,
                        movie,
                        &mut trial,
                        false,
                    )
                    .map_err(|reason| self.invalid(reason, at))?;
                    self.next_decode = trial;
                    let offset = self.stream_offset;
                    let bytes = self.take(mdat_end);
                    out.push(Fmp4Unit::Fragment {
                        fragment: Fmp4Fragment {
                            sequence_number,
                            offset,
                            length: bytes.len() as u64,
                            runs,
                        },
                        bytes,
                    });
                }
                b"mfra" => {
                    if self.buffer.len() < end {
                        return Ok(());
                    }
                    let bytes = self.take(end);
                    out.push(Fmp4Unit::Index { bytes });
                    self.phase = Phase::Done;
                }
                b"mdat" => return Err(self.invalid("mdat without moof", at)),
                b"ftyp" | b"moov" => {
                    return Err(self.invalid("init box inside the fragment sequence", at));
                }
                _ => {
                    if self.buffer.len() < end {
                        return Ok(());
                    }
                    self.scan_pos = end;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BoxHeader {
    kind: Fourcc,
    header_len: u64,
    size: u64,
}

impl BoxHeader {
    #[allow(clippy::cast_possible_truncation)]
    fn header_len_usize(self) -> usize {
        // Always 8 or 16.
        self.header_len as usize
    }
}

enum Header {
    Incomplete,
    Invalid(&'static str),
    Box(BoxHeader),
}

fn valid_fourcc(kind: Fourcc) -> bool {
    kind.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

fn parse_top_header(bytes: &[u8]) -> Header {
    if bytes.len() < 8 {
        return Header::Incomplete;
    }
    let size32 = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let kind = [bytes[4], bytes[5], bytes[6], bytes[7]];
    if !valid_fourcc(kind) {
        return Header::Invalid("invalid box type");
    }
    let (size, header_len) = match size32 {
        0 => return Header::Invalid("open-ended top-level box"),
        1 => {
            let Some(large) = bytes.get(8..16) else {
                return Header::Incomplete;
            };
            let mut raw = [0u8; 8];
            raw.copy_from_slice(large);
            (u64::from_be_bytes(raw), 16)
        }
        size => (u64::from(size), 8),
    };
    if size < header_len {
        return Header::Invalid("box size is smaller than its header");
    }
    Header::Box(BoxHeader {
        kind,
        header_len,
        size,
    })
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn read_top_header<R: Read + Seek>(reader: &mut R, offset: u64, length: u64) -> io::Result<Header> {
    let mut bytes = [0u8; 16];
    let wanted = usize::try_from(length.saturating_sub(offset).min(16)).unwrap_or(16);
    let read = read_at(reader, offset, &mut bytes[..wanted])?;
    Ok(parse_top_header(&bytes[..read]))
}

fn read_payload<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    header: BoxHeader,
) -> io::Result<Vec<u8>> {
    let length = usize::try_from(header.size - header.header_len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "box exceeds memory"))?;
    let mut payload = vec![0u8; length];
    let read = read_at(reader, offset + header.header_len, &mut payload)?;
    if read != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "file shrank while it was scanned",
        ));
    }
    Ok(payload)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

const TRUNCATED: &str = "truncated box payload";

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], &'static str> {
        let end = self.position.checked_add(length).ok_or(TRUNCATED)?;
        let slice = self.bytes.get(self.position..end).ok_or(TRUNCATED)?;
        self.position = end;
        Ok(slice)
    }

    fn skip(&mut self, length: usize) -> Result<(), &'static str> {
        self.take(length).map(|_| ())
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, &'static str> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        let bytes = self.take(8)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(raw))
    }

    fn fourcc(&mut self) -> Result<Fourcc, &'static str> {
        let bytes = self.take(4)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

struct Children<'a> {
    bytes: &'a [u8],
    position: usize,
}

fn children(bytes: &[u8]) -> Children<'_> {
    Children { bytes, position: 0 }
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<(Fourcc, &'a [u8]), &'static str>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.position >= self.bytes.len() {
            return None;
        }
        let result = self.next_child();
        if result.is_err() {
            self.position = self.bytes.len();
        }
        Some(result)
    }
}

impl<'a> Children<'a> {
    fn next_child(&mut self) -> Result<(Fourcc, &'a [u8]), &'static str> {
        let rest = &self.bytes[self.position..];
        let mut cursor = Cursor::new(rest);
        let size32 = cursor.u32().map_err(|_| "truncated child box header")?;
        let kind = cursor.fourcc().map_err(|_| "truncated child box header")?;
        let (size, header_len) = match size32 {
            0 => (rest.len() as u64, 8u64),
            1 => (cursor.u64().map_err(|_| "truncated child box header")?, 16),
            size => (u64::from(size), 8),
        };
        if size < header_len || size > rest.len() as u64 {
            return Err("child box overruns its parent");
        }
        let (header_len, size) = (
            usize::try_from(header_len).map_err(|_| TRUNCATED)?,
            usize::try_from(size).map_err(|_| TRUNCATED)?,
        );
        self.position += size;
        Ok((kind, &rest[header_len..size]))
    }
}

fn full_box_version_flags(cursor: &mut Cursor<'_>) -> Result<(u8, u32), &'static str> {
    let word = cursor.u32()?;
    Ok(((word >> 24) as u8, word & 0x00ff_ffff))
}

fn parse_moov(payload: &[u8]) -> Result<Fmp4Movie, &'static str> {
    let mut tracks = Vec::new();
    let mut trex = Vec::new();
    let mut fragmented = false;
    for child in children(payload) {
        let (kind, body) = child?;
        match &kind {
            b"trak" => {
                if tracks.len() == MAX_FMP4_TRACKS {
                    return Err("moov declares too many tracks");
                }
                tracks.push(parse_trak(body)?);
            }
            b"mvex" => {
                fragmented = true;
                for child in children(body) {
                    let (kind, body) = child?;
                    if &kind == b"trex" {
                        if trex.len() == MAX_FMP4_TRACKS {
                            return Err("mvex declares too many trex boxes");
                        }
                        trex.push(parse_trex(body)?);
                    }
                }
            }
            _ => {}
        }
    }
    if !fragmented {
        return Err("moov lacks mvex; not a fragmented MP4");
    }
    if tracks.is_empty() {
        return Err("moov declares no tracks");
    }
    for (index, track) in tracks.iter().enumerate() {
        if tracks[..index]
            .iter()
            .any(|other: &Fmp4Track| other.track_id == track.track_id)
        {
            return Err("moov declares a duplicate track id");
        }
    }
    let defaults = tracks
        .iter()
        .map(|track| {
            trex.iter()
                .find(|(track_id, _)| *track_id == track.track_id)
                .map(|(_, defaults)| *defaults)
                .unwrap_or_default()
        })
        .collect();
    Ok(Fmp4Movie { tracks, defaults })
}

fn parse_trak(payload: &[u8]) -> Result<Fmp4Track, &'static str> {
    let mut track_id = None;
    let mut timescale = None;
    let mut handler = None;
    for child in children(payload) {
        let (kind, body) = child?;
        match &kind {
            b"tkhd" => {
                let mut cursor = Cursor::new(body);
                let (version, _) = full_box_version_flags(&mut cursor)?;
                cursor.skip(if version == 1 { 16 } else { 8 })?;
                track_id = Some(cursor.u32()?);
            }
            b"mdia" => {
                for child in children(body) {
                    let (kind, body) = child?;
                    let mut cursor = Cursor::new(body);
                    match &kind {
                        b"mdhd" => {
                            let (version, _) = full_box_version_flags(&mut cursor)?;
                            cursor.skip(if version == 1 { 16 } else { 8 })?;
                            timescale = Some(cursor.u32()?);
                        }
                        b"hdlr" => {
                            cursor.skip(8)?;
                            handler = Some(cursor.fourcc()?);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let track_id = track_id
        .filter(|id| *id != 0)
        .ok_or("trak lacks a valid tkhd")?;
    let timescale = timescale
        .filter(|scale| *scale != 0)
        .ok_or("trak lacks a valid mdhd timescale")?;
    let handler = handler.ok_or("trak lacks hdlr")?;
    Ok(Fmp4Track {
        track_id,
        timescale,
        handler,
    })
}

fn parse_trex(payload: &[u8]) -> Result<(u32, TrackDefaults), &'static str> {
    let mut cursor = Cursor::new(payload);
    full_box_version_flags(&mut cursor)?;
    let track_id = cursor.u32()?;
    cursor.skip(4)?;
    let duration = cursor.u32()?;
    let size = cursor.u32()?;
    Ok((track_id, TrackDefaults { duration, size }))
}

const TFHD_BASE_DATA_OFFSET: u32 = 0x1;
const TFHD_SAMPLE_DESCRIPTION: u32 = 0x2;
const TFHD_DEFAULT_DURATION: u32 = 0x8;
const TFHD_DEFAULT_SIZE: u32 = 0x10;
const TFHD_DEFAULT_FLAGS: u32 = 0x20;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x2_0000;
const TRUN_DATA_OFFSET: u32 = 0x1;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x4;
const TRUN_SAMPLE_DURATION: u32 = 0x100;
const TRUN_SAMPLE_SIZE: u32 = 0x200;
const TRUN_SAMPLE_FLAGS: u32 = 0x400;
const TRUN_SAMPLE_CTO: u32 = 0x800;

/// Parses a `moof` payload and validates every track run's data range against
/// `mdat_payload`. Offsets are in the same coordinate space as `moof_start`.
fn parse_moof(
    payload: &[u8],
    moof_start: u64,
    mdat_payload: Range<u64>,
    movie: &Fmp4Movie,
    next_decode: &mut [u64],
    allow_absolute_base: bool,
) -> Result<(u32, Vec<Fmp4TrackRun>), &'static str> {
    let mut sequence = None;
    let mut runs = Vec::new();
    let mut data_cursor = moof_start;
    for child in children(payload) {
        let (kind, body) = child?;
        match &kind {
            b"mfhd" => {
                let mut cursor = Cursor::new(body);
                full_box_version_flags(&mut cursor)?;
                sequence = Some(cursor.u32()?);
            }
            b"traf" => {
                if runs.len() == MAX_FMP4_TRACKS {
                    return Err("moof carries too many track fragments");
                }
                runs.push(parse_traf(
                    body,
                    moof_start,
                    &mdat_payload,
                    movie,
                    next_decode,
                    &mut data_cursor,
                    allow_absolute_base,
                )?);
            }
            _ => {}
        }
    }
    let sequence = sequence.ok_or("moof lacks mfhd")?;
    if runs.is_empty() {
        return Err("moof carries no track fragment");
    }
    Ok((sequence, runs))
}

#[allow(clippy::too_many_lines)]
fn parse_traf(
    payload: &[u8],
    moof_start: u64,
    mdat_payload: &Range<u64>,
    movie: &Fmp4Movie,
    next_decode: &mut [u64],
    data_cursor: &mut u64,
    allow_absolute_base: bool,
) -> Result<Fmp4TrackRun, &'static str> {
    let mut tfhd = None;
    let mut tfdt = None;
    let mut truns = Vec::new();
    for child in children(payload) {
        let (kind, body) = child?;
        match &kind {
            b"tfhd" => tfhd = Some(body),
            b"tfdt" => {
                let mut cursor = Cursor::new(body);
                let (version, _) = full_box_version_flags(&mut cursor)?;
                tfdt = Some(if version == 1 {
                    cursor.u64()?
                } else {
                    u64::from(cursor.u32()?)
                });
            }
            b"trun" => {
                if truns.len() == 1024 {
                    return Err("traf carries too many track runs");
                }
                truns.push(body);
            }
            _ => {}
        }
    }
    let mut cursor = Cursor::new(tfhd.ok_or("traf lacks tfhd")?);
    let (_, flags) = full_box_version_flags(&mut cursor)?;
    let track_id = cursor.u32()?;
    let index = movie
        .position(track_id)
        .ok_or("traf names an unknown track")?;
    let base_data_offset = if flags & TFHD_BASE_DATA_OFFSET != 0 {
        Some(cursor.u64()?)
    } else {
        None
    };
    if flags & TFHD_SAMPLE_DESCRIPTION != 0 {
        cursor.skip(4)?;
    }
    let default_duration = if flags & TFHD_DEFAULT_DURATION != 0 {
        cursor.u32()?
    } else {
        movie.defaults[index].duration
    };
    let default_size = if flags & TFHD_DEFAULT_SIZE != 0 {
        cursor.u32()?
    } else {
        movie.defaults[index].size
    };
    if flags & TFHD_DEFAULT_FLAGS != 0 {
        cursor.skip(4)?;
    }
    let base = match base_data_offset {
        Some(_) if !allow_absolute_base => {
            return Err("explicit base data offset is not relocatable");
        }
        Some(absolute) => absolute,
        None if flags & TFHD_DEFAULT_BASE_IS_MOOF != 0 => moof_start,
        None => *data_cursor,
    };
    let mut next_data = base;
    let mut duration = 0u64;
    let mut samples = 0u64;
    let mut data_bytes = 0u64;
    for trun in truns {
        let mut cursor = Cursor::new(trun);
        let (_, flags) = full_box_version_flags(&mut cursor)?;
        let count = u64::from(cursor.u32()?);
        if count > MAX_TRUN_SAMPLES {
            return Err("trun sample count exceeds its bound");
        }
        let start = if flags & TRUN_DATA_OFFSET != 0 {
            base.checked_add_signed(i64::from(cursor.i32()?))
                .ok_or("trun data offset is out of range")?
        } else {
            next_data
        };
        if flags & TRUN_FIRST_SAMPLE_FLAGS != 0 {
            cursor.skip(4)?;
        }
        let per_sample = 4 * u64::from((flags & 0xf00).count_ones());
        if count.saturating_mul(per_sample) > cursor.remaining() as u64 {
            return Err("trun sample table is truncated");
        }
        let mut run_size = 0u64;
        for _ in 0..count {
            let sample_duration = if flags & TRUN_SAMPLE_DURATION != 0 {
                cursor.u32()?
            } else {
                default_duration
            };
            let sample_size = if flags & TRUN_SAMPLE_SIZE != 0 {
                cursor.u32()?
            } else {
                default_size
            };
            if flags & TRUN_SAMPLE_FLAGS != 0 {
                cursor.skip(4)?;
            }
            if flags & TRUN_SAMPLE_CTO != 0 {
                cursor.skip(4)?;
            }
            duration = duration.saturating_add(u64::from(sample_duration));
            run_size += u64::from(sample_size);
        }
        let end = start
            .checked_add(run_size)
            .ok_or("trun data range overflows")?;
        if run_size > 0 && (start < mdat_payload.start || end > mdat_payload.end) {
            return Err("track run data lies outside its mdat");
        }
        next_data = end;
        samples += count;
        data_bytes += run_size;
    }
    *data_cursor = next_data;
    let base_decode_time = tfdt.unwrap_or(next_decode[index]);
    next_decode[index] = base_decode_time.saturating_add(duration);
    Ok(Fmp4TrackRun {
        track_id,
        base_decode_time,
        duration,
        samples,
        data_bytes,
    })
}

struct SpanAccumulator {
    spans: Vec<Option<(u64, u64, u64)>>,
}

impl SpanAccumulator {
    fn new(tracks: usize) -> Self {
        Self {
            spans: vec![None; tracks],
        }
    }

    fn absorb(&mut self, movie: &Fmp4Movie, runs: &[Fmp4TrackRun]) {
        for run in runs {
            let Some(index) = movie.position(run.track_id) else {
                continue;
            };
            let end = run.base_decode_time.saturating_add(run.duration);
            let span = self.spans[index].get_or_insert((run.base_decode_time, end, 0));
            span.0 = span.0.min(run.base_decode_time);
            span.1 = span.1.max(end);
            span.2 += run.samples;
        }
    }

    fn finish(self, movie: &Fmp4Movie) -> Vec<Fmp4TrackSpan> {
        self.spans
            .into_iter()
            .zip(&movie.tracks)
            .filter_map(|(span, track)| {
                span.map(|(first, end, samples)| Fmp4TrackSpan {
                    track_id: track.track_id,
                    timescale: track.timescale,
                    first_decode_time: first,
                    end_decode_time: end,
                    samples,
                })
            })
            .collect()
    }
}

fn ticks_to_nanos(ticks: u64, timescale: u32) -> u64 {
    if timescale == 0 {
        return 0;
    }
    let nanos = u128::from(ticks) * NANOS_PER_SECOND / u128::from(timescale);
    u64::try_from(nanos).unwrap_or(u64::MAX)
}
