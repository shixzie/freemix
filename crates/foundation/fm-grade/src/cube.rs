//! Bounded `.cube` LUT parsing and sampling.

use core::fmt;

/// Largest accepted `.cube` source in bytes.
pub const MAX_CUBE_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Smallest 1D size or 3D edge.
pub const MIN_CUBE_SIZE: usize = 2;
/// Largest 3D edge.
pub const MAX_CUBE_3D_EDGE: usize = 65;
/// Largest 1D size.
pub const MAX_CUBE_1D_SIZE: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CubeKind {
    OneD,
    ThreeD,
}

/// A validated 1D or 3D LUT.
///
/// 3D entries are red-fastest (`index = (blue * edge + green) * edge + red`),
/// matching the `.cube` data order and `fm_color::Lut3D`.
#[derive(Clone, Debug)]
pub struct CubeLut {
    title: Option<String>,
    kind: CubeKind,
    size: usize,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    entries: Vec<[f32; 3]>,
}

impl CubeLut {
    /// Creates a red-fastest 3D LUT.
    ///
    /// # Errors
    /// Returns an error for an edge outside `2..=65`, a wrong entry count, an
    /// invalid domain, or a non-finite entry.
    pub fn new_3d(
        edge: usize,
        entries: Vec<[f32; 3]>,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
    ) -> Result<Self, CubeError> {
        Self::new(CubeKind::ThreeD, edge, entries, domain_min, domain_max)
    }

    /// Creates a per-channel 1D LUT.
    ///
    /// # Errors
    /// Returns an error for a size outside `2..=65536`, a wrong entry count,
    /// an invalid domain, or a non-finite entry.
    pub fn new_1d(
        entries: Vec<[f32; 3]>,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
    ) -> Result<Self, CubeError> {
        let size = entries.len();
        Self::new(CubeKind::OneD, size, entries, domain_min, domain_max)
    }

    fn new(
        kind: CubeKind,
        size: usize,
        entries: Vec<[f32; 3]>,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
    ) -> Result<Self, CubeError> {
        validate_size(kind, size)?;
        validate_domain(domain_min, domain_max)?;
        let expected = expected_entries(kind, size);
        if entries.len() != expected {
            return Err(CubeError::EntryCount {
                expected,
                actual: entries.len(),
            });
        }
        if entries.iter().flatten().any(|value| !value.is_finite()) {
            return Err(CubeError::NonFiniteValue { line: None });
        }
        Ok(Self {
            title: None,
            kind,
            size,
            domain_min,
            domain_max,
            entries,
        })
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub const fn kind(&self) -> CubeKind {
        self.kind
    }

    /// 1D entry count or 3D edge length.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub const fn domain_min(&self) -> [f32; 3] {
        self.domain_min
    }

    #[must_use]
    pub const fn domain_max(&self) -> [f32; 3] {
        self.domain_max
    }

    #[must_use]
    pub fn entries(&self) -> &[[f32; 3]] {
        &self.entries
    }

    /// Samples with the input clamped to the domain: per-channel linear
    /// interpolation for 1D, trilinear for 3D. Arithmetic order matches
    /// `fm_color::Lut1D::sample` / `fm_color::Lut3D::sample` exactly.
    #[must_use]
    pub fn sample(&self, input: [f32; 3]) -> [f32; 3] {
        let position = |channel: usize| {
            normalize(
                input[channel],
                self.domain_min[channel],
                self.domain_max[channel],
            )
        };
        match self.kind {
            CubeKind::OneD => core::array::from_fn(|channel| {
                let (low, high, fraction) = axis(position(channel), self.size);
                self.entries[low][channel] * (1.0 - fraction)
                    + self.entries[high][channel] * fraction
            }),
            CubeKind::ThreeD => {
                let r = axis(position(0), self.size);
                let g = axis(position(1), self.size);
                let b = axis(position(2), self.size);
                let entry = |red: usize, green: usize, blue: usize| {
                    self.entries[(blue * self.size + green) * self.size + red]
                };
                let c00 = lerp(entry(r.0, g.0, b.0), entry(r.1, g.0, b.0), r.2);
                let c10 = lerp(entry(r.0, g.1, b.0), entry(r.1, g.1, b.0), r.2);
                let c01 = lerp(entry(r.0, g.0, b.1), entry(r.1, g.0, b.1), r.2);
                let c11 = lerp(entry(r.0, g.1, b.1), entry(r.1, g.1, b.1), r.2);
                lerp(lerp(c00, c10, g.2), lerp(c01, c11, g.2), b.2)
            }
        }
    }
}

/// Bitwise equality, so `Eq` holds even for signed zeros and is reflexive.
impl PartialEq for CubeLut {
    fn eq(&self, other: &Self) -> bool {
        let bits = |values: &[f32; 3]| values.map(f32::to_bits);
        self.title == other.title
            && self.kind == other.kind
            && self.size == other.size
            && bits(&self.domain_min) == bits(&other.domain_min)
            && bits(&self.domain_max) == bits(&other.domain_max)
            && self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|(left, right)| bits(left) == bits(right))
    }
}

impl Eq for CubeLut {}

fn normalize(value: f32, minimum: f32, maximum: f32) -> f32 {
    ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn axis(position: f32, size: usize) -> (usize, usize, f32) {
    // Sizes are bounded to 65,536 and positions are clamped to [0, 1]; NaN
    // positions saturate to index 0.
    let scaled = position * (size - 1) as f32;
    let low = (scaled.floor() as usize).min(size - 1);
    let high = (low + 1).min(size - 1);
    (low, high, scaled - low as f32)
}

fn lerp(left: [f32; 3], right: [f32; 3], amount: f32) -> [f32; 3] {
    core::array::from_fn(|channel| left[channel] * (1.0 - amount) + right[channel] * amount)
}

fn expected_entries(kind: CubeKind, size: usize) -> usize {
    match kind {
        CubeKind::OneD => size,
        CubeKind::ThreeD => size * size * size,
    }
}

fn validate_size(kind: CubeKind, size: usize) -> Result<(), CubeError> {
    let maximum = match kind {
        CubeKind::OneD => MAX_CUBE_1D_SIZE,
        CubeKind::ThreeD => MAX_CUBE_3D_EDGE,
    };
    if (MIN_CUBE_SIZE..=maximum).contains(&size) {
        Ok(())
    } else {
        Err(CubeError::SizeOutOfRange {
            kind,
            size: u64::try_from(size).unwrap_or(u64::MAX),
        })
    }
}

fn validate_domain(minimum: [f32; 3], maximum: [f32; 3]) -> Result<(), CubeError> {
    let valid = (0..3).all(|channel| {
        minimum[channel].is_finite()
            && maximum[channel].is_finite()
            && minimum[channel] < maximum[channel]
    });
    if valid {
        Ok(())
    } else {
        Err(CubeError::InvalidDomain)
    }
}

/// A non-fatal `.cube` observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CubeWarning {
    /// A header keyword this parser does not interpret; the line is ignored.
    UnknownKeyword { line: usize, keyword: String },
}

impl fmt::Display for CubeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKeyword { line, keyword } => {
                write!(formatter, "line {line}: ignored unknown keyword {keyword}")
            }
        }
    }
}

/// A fatal `.cube` parse or validation error. Lines are 1-based.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CubeError {
    InputTooLarge {
        bytes: usize,
    },
    MalformedKeyword {
        line: usize,
        keyword: &'static str,
    },
    DuplicateKeyword {
        line: usize,
        keyword: &'static str,
    },
    /// Both `LUT_1D_SIZE` and `LUT_3D_SIZE` were declared.
    ConflictingSize {
        line: usize,
    },
    KeywordAfterData {
        line: usize,
    },
    DataBeforeSize {
        line: usize,
    },
    MalformedRow {
        line: usize,
    },
    /// A NaN or infinite value; `line` is `None` for programmatic LUTs.
    NonFiniteValue {
        line: Option<usize>,
    },
    TooManyEntries {
        line: usize,
        expected: usize,
    },
    MissingSize,
    SizeOutOfRange {
        kind: CubeKind,
        size: u64,
    },
    InvalidDomain,
    EntryCount {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for CubeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { bytes } => write!(
                formatter,
                ".cube input has {bytes} bytes; maximum is {MAX_CUBE_INPUT_BYTES}"
            ),
            Self::MalformedKeyword { line, keyword } => {
                write!(formatter, "line {line}: malformed {keyword}")
            }
            Self::DuplicateKeyword { line, keyword } => {
                write!(formatter, "line {line}: duplicate {keyword}")
            }
            Self::ConflictingSize { line } => write!(
                formatter,
                "line {line}: LUT_1D_SIZE and LUT_3D_SIZE are mutually exclusive"
            ),
            Self::KeywordAfterData { line } => {
                write!(formatter, "line {line}: keyword after LUT data")
            }
            Self::DataBeforeSize { line } => {
                write!(formatter, "line {line}: LUT data before a size declaration")
            }
            Self::MalformedRow { line } => write!(
                formatter,
                "line {line}: LUT rows must contain exactly three numbers"
            ),
            Self::NonFiniteValue { line: Some(line) } => {
                write!(formatter, "line {line}: LUT values must be finite")
            }
            Self::NonFiniteValue { line: None } => formatter.write_str("LUT values must be finite"),
            Self::TooManyEntries { line, expected } => write!(
                formatter,
                "line {line}: LUT has more than the {expected} declared entries"
            ),
            Self::MissingSize => formatter.write_str("LUT declares no LUT_1D_SIZE or LUT_3D_SIZE"),
            Self::SizeOutOfRange { kind, size } => {
                let maximum = match kind {
                    CubeKind::OneD => MAX_CUBE_1D_SIZE,
                    CubeKind::ThreeD => MAX_CUBE_3D_EDGE,
                };
                write!(
                    formatter,
                    "{kind:?} LUT size {size} is outside {MIN_CUBE_SIZE}..={maximum}"
                )
            }
            Self::InvalidDomain => {
                formatter.write_str("LUT domain must be finite with DOMAIN_MIN < DOMAIN_MAX")
            }
            Self::EntryCount { expected, actual } => {
                write!(formatter, "LUT has {actual} entries; expected {expected}")
            }
        }
    }
}

impl std::error::Error for CubeError {}

const TITLE: &str = "TITLE";
const LUT_1D_SIZE: &str = "LUT_1D_SIZE";
const LUT_3D_SIZE: &str = "LUT_3D_SIZE";
const DOMAIN_MIN: &str = "DOMAIN_MIN";
const DOMAIN_MAX: &str = "DOMAIN_MAX";
const LUT_1D_INPUT_RANGE: &str = "LUT_1D_INPUT_RANGE";
const LUT_3D_INPUT_RANGE: &str = "LUT_3D_INPUT_RANGE";
/// Reported for a duplicate domain declared through either spelling.
const DOMAIN: &str = "DOMAIN_MIN/DOMAIN_MAX/LUT_*_INPUT_RANGE";

#[derive(Default)]
struct Header {
    title: Option<String>,
    size: Option<(CubeKind, usize)>,
    domain_min: Option<[f32; 3]>,
    domain_max: Option<[f32; 3]>,
    input_range: bool,
}

/// Parses an Adobe/Resolve-style `.cube` LUT.
///
/// Accepted header keywords: `TITLE "text"`, `LUT_1D_SIZE n`,
/// `LUT_3D_SIZE n`, `DOMAIN_MIN r g b`, `DOMAIN_MAX r g b`, and the
/// `LUT_1D_INPUT_RANGE min max` / `LUT_3D_INPUT_RANGE min max` aliases that
/// set the same domain on all channels. `#` starts a comment anywhere on a
/// line, blank lines are ignored, and other keywords produce a
/// [`CubeWarning`]. Headers must precede data; each data row is exactly
/// three finite numbers. 3D edges are `2..=65`, 1D sizes `2..=65536`, and
/// the input is limited to 16 MiB.
///
/// # Errors
/// Returns a typed [`CubeError`] describing the first violation.
pub fn parse_cube(source: &str) -> Result<(CubeLut, Vec<CubeWarning>), CubeError> {
    if source.len() > MAX_CUBE_INPUT_BYTES {
        return Err(CubeError::InputTooLarge {
            bytes: source.len(),
        });
    }
    let mut header = Header::default();
    let mut warnings = Vec::new();
    let mut entries: Vec<[f32; 3]> = Vec::new();
    let mut expected = 0_usize;
    for (index, raw_line) in source.lines().enumerate() {
        let line = index + 1;
        let content = raw_line.split_once('#').map_or(raw_line, |(code, _)| code);
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        let first = content.split_ascii_whitespace().next().unwrap_or_default();
        if first.parse::<f32>().is_ok() {
            let Some((kind, size)) = header.size else {
                return Err(CubeError::DataBeforeSize { line });
            };
            if entries.is_empty() {
                expected = expected_entries(kind, size);
                entries.reserve_exact(expected);
            }
            if entries.len() == expected {
                return Err(CubeError::TooManyEntries { line, expected });
            }
            entries.push(parse_row(content, line)?);
            continue;
        }
        if !entries.is_empty() {
            return Err(CubeError::KeywordAfterData { line });
        }
        parse_keyword(&mut header, &mut warnings, first, content, line)?;
    }
    let (kind, size) = header.size.ok_or(CubeError::MissingSize)?;
    let domain_min = header.domain_min.unwrap_or([0.0; 3]);
    let domain_max = header.domain_max.unwrap_or([1.0; 3]);
    let mut lut = CubeLut::new(kind, size, entries, domain_min, domain_max)?;
    lut.title = header.title;
    Ok((lut, warnings))
}

fn parse_keyword(
    header: &mut Header,
    warnings: &mut Vec<CubeWarning>,
    keyword: &str,
    content: &str,
    line: usize,
) -> Result<(), CubeError> {
    let arguments = content[keyword.len()..].trim();
    match keyword {
        TITLE => {
            if header.title.is_some() {
                return Err(CubeError::DuplicateKeyword {
                    line,
                    keyword: TITLE,
                });
            }
            let title = arguments
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .filter(|title| !title.contains('"'))
                .ok_or(CubeError::MalformedKeyword {
                    line,
                    keyword: TITLE,
                })?;
            header.title = Some(title.to_owned());
        }
        LUT_1D_SIZE | LUT_3D_SIZE => parse_size(header, keyword, arguments, line)?,
        DOMAIN_MIN | DOMAIN_MAX => {
            let name = if keyword == DOMAIN_MIN {
                DOMAIN_MIN
            } else {
                DOMAIN_MAX
            };
            let values = parse_floats::<3>(arguments).ok_or(CubeError::MalformedKeyword {
                line,
                keyword: name,
            })?;
            let slot = if keyword == DOMAIN_MIN {
                &mut header.domain_min
            } else {
                &mut header.domain_max
            };
            if slot.is_some() || header.input_range {
                return Err(CubeError::DuplicateKeyword {
                    line,
                    keyword: DOMAIN,
                });
            }
            *slot = Some(values);
        }
        LUT_1D_INPUT_RANGE | LUT_3D_INPUT_RANGE => {
            parse_input_range(header, keyword, arguments, line)?;
        }
        _ => warnings.push(CubeWarning::UnknownKeyword {
            line,
            keyword: keyword.to_owned(),
        }),
    }
    Ok(())
}

fn parse_size(
    header: &mut Header,
    keyword: &str,
    arguments: &str,
    line: usize,
) -> Result<(), CubeError> {
    let (kind, name) = if keyword == LUT_1D_SIZE {
        (CubeKind::OneD, LUT_1D_SIZE)
    } else {
        (CubeKind::ThreeD, LUT_3D_SIZE)
    };
    if let Some((existing, _)) = header.size {
        return Err(if existing == kind {
            CubeError::DuplicateKeyword {
                line,
                keyword: name,
            }
        } else {
            CubeError::ConflictingSize { line }
        });
    }
    let mut tokens = arguments.split_ascii_whitespace();
    let size = match (tokens.next(), tokens.next()) {
        (Some(token), None) => token
            .parse::<u64>()
            .map_err(|_| CubeError::MalformedKeyword {
                line,
                keyword: name,
            })?,
        _ => {
            return Err(CubeError::MalformedKeyword {
                line,
                keyword: name,
            });
        }
    };
    let size = usize::try_from(size).map_err(|_| CubeError::SizeOutOfRange { kind, size })?;
    validate_size(kind, size)?;
    header.size = Some((kind, size));
    Ok(())
}

fn parse_input_range(
    header: &mut Header,
    keyword: &str,
    arguments: &str,
    line: usize,
) -> Result<(), CubeError> {
    let name = if keyword == LUT_1D_INPUT_RANGE {
        LUT_1D_INPUT_RANGE
    } else {
        LUT_3D_INPUT_RANGE
    };
    let [minimum, maximum] = parse_floats::<2>(arguments).ok_or(CubeError::MalformedKeyword {
        line,
        keyword: name,
    })?;
    if header.input_range || header.domain_min.is_some() || header.domain_max.is_some() {
        return Err(CubeError::DuplicateKeyword {
            line,
            keyword: DOMAIN,
        });
    }
    header.input_range = true;
    header.domain_min = Some([minimum; 3]);
    header.domain_max = Some([maximum; 3]);
    Ok(())
}

/// Exactly `N` whitespace-separated floats (finiteness checked by callers).
fn parse_floats<const N: usize>(arguments: &str) -> Option<[f32; N]> {
    let mut values = [0.0_f32; N];
    let mut tokens = arguments.split_ascii_whitespace();
    for value in &mut values {
        *value = tokens.next()?.parse().ok()?;
    }
    tokens.next().is_none().then_some(values)
}

fn parse_row(content: &str, line: usize) -> Result<[f32; 3], CubeError> {
    let values = parse_floats::<3>(content).ok_or(CubeError::MalformedRow { line })?;
    if values.iter().all(|value| value.is_finite()) {
        Ok(values)
    } else {
        Err(CubeError::NonFiniteValue { line: Some(line) })
    }
}
