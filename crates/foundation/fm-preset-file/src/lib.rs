//! Sharing presets between projects: `.fmpreset` animation presets and
//! `.fmgrade` grades (GX-011, SW-019).
//!
//! This crate is pure format code (strict JSON and plain file reads and
//! writes), so both the project store and client applications such as
//! Studio share one implementation. Its submodules also hold the grade and
//! motion encodings `project.json` uses.
//!
//! Both are strict JSON documents versioned by the project schema
//! ([`SCHEMA_VERSION`]): unknown
//! or missing keys, unknown enum values, and any `schema_version` other than
//! the current one are refused, and documents are capped at
//! [`MAX_PRESET_FILE_BYTES`].
//!
//! ```text
//! {"schema_version": 35, "kind": "animation_preset", "name": "Swoosh",
//!  "phase": "in", "timebase": "30/1", "frames": 15, "width": 400,
//!  "height": 100, "tracks": [...]}
//!
//! {"schema_version": 35, "kind": "grade", "name": "Warm", "grade": GRADE,
//!  "lut": null
//!       | {"name": "Film", "strength_permille": 800, "cube": "<.cube text>"}
//!       | {"name": "Film", "strength_permille": 800, "file": "Warm.cube"}}
//! ```
//!
//! An animation preset carries exactly the body the project library stores
//! (see `project.json`'s `animation_presets`). A grade carries its values
//! with the project-specific LUT reference removed (`GRADE`'s `"lut"` is
//! `null`); its LUT, if any, travels beside it by name and strength, with
//! the `.cube` text embedded when it is at most [`MAX_EMBEDDED_LUT_BYTES`]
//! and otherwise in a sibling file named by `"file"` (a plain `.cube` file
//! name in the same directory). Importing re-registers the LUT under a fresh
//! id in the receiving project.

pub mod grade;
pub mod json;
pub mod motion;

use std::{
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fm_animation::{Phase, PhaseMotion};
use fm_grade::{Grade, parse_cube};

use crate::json::{Object, SyntaxError, escape_string};

/// The project schema version preset files carry; any other is refused.
/// The project store checks it equals its current schema.
pub const SCHEMA_VERSION: u32 = 35;
/// Longest grade name, in bytes (the project grade library bound).
pub const MAX_GRADE_NAME_BYTES: usize = 128;
/// Longest LUT name, in bytes (the project LUT bound).
pub const MAX_LUT_NAME_BYTES: usize = 128;
/// Largest `.cube` file read, in bytes (the `.cube` parser's input cap).
pub const MAX_LUT_FILE_BYTES: u64 = fm_grade::MAX_CUBE_INPUT_BYTES as u64;
/// Longest animation preset name, in bytes (the preset library bound).
pub const MAX_ANIMATION_PRESET_NAME_BYTES: usize = 128;

/// File extension of an animation preset.
pub const ANIMATION_PRESET_EXTENSION: &str = "fmpreset";
/// File extension of a grade preset.
pub const GRADE_PRESET_EXTENSION: &str = "fmgrade";
/// Largest `.fmpreset` or `.fmgrade` document, in bytes.
pub const MAX_PRESET_FILE_BYTES: u64 = 256 * 1024;
/// Largest `.cube` text a `.fmgrade` embeds; a larger LUT goes to a sibling
/// file. Matches the largest LUT one protocol command imports, so an
/// embedded LUT can always be imported through a running daemon.
pub const MAX_EMBEDDED_LUT_BYTES: usize = 32 * 1024;
/// Longest sibling `.cube` file name, in bytes.
const MAX_SIBLING_NAME_BYTES: usize = 255;
/// Longest preset name, in bytes (the animation preset name bound).
const MAX_PRESET_NAME_BYTES: usize = MAX_ANIMATION_PRESET_NAME_BYTES;

/// One animation preset as shared in a `.fmpreset` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationPresetFile {
    pub name: String,
    pub phase: Phase,
    pub motion: PhaseMotion,
}

/// Where a `.fmgrade` keeps its LUT's `.cube` text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeLutSource {
    /// The text itself, at most [`MAX_EMBEDDED_LUT_BYTES`].
    Embedded(String),
    /// A plain `.cube` file name beside the `.fmgrade`.
    Sibling(String),
}

/// The LUT a shared grade uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradeFileLut {
    pub name: String,
    pub strength_permille: u16,
    pub source: GradeLutSource,
}

/// One grade as shared in a `.fmgrade` file. `grade.lut` is always `None`;
/// the LUT travels in `lut`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradePresetFile {
    pub name: String,
    pub grade: Grade,
    pub lut: Option<GradeFileLut>,
}

/// A refused preset file.
#[derive(Debug)]
pub enum PresetFileError {
    Io(io::Error),
    TooLarge {
        bytes: u64,
        maximum: u64,
    },
    NotUtf8,
    Malformed {
        offset: usize,
        message: String,
    },
    UnsupportedSchema {
        found: u32,
        supported: u32,
    },
    WrongKind {
        found: String,
        expected: &'static str,
    },
    /// The document parses but a value is out of bounds.
    Invalid(String),
}

impl fmt::Display for PresetFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "preset file I/O failed: {error}"),
            Self::TooLarge { bytes, maximum } => {
                write!(formatter, "file is {bytes} bytes; the maximum is {maximum}")
            }
            Self::NotUtf8 => formatter.write_str("file is not UTF-8 text"),
            Self::Malformed { offset, message } => {
                write!(formatter, "malformed preset file at {offset}: {message}")
            }
            Self::UnsupportedSchema { found, supported } => write!(
                formatter,
                "preset file schema {found} is unsupported; expected {supported}"
            ),
            Self::WrongKind { found, expected } => {
                write!(formatter, "file is of kind `{found}`, not `{expected}`")
            }
            Self::Invalid(message) => write!(formatter, "invalid preset file: {message}"),
        }
    }
}

impl std::error::Error for PresetFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for PresetFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SyntaxError> for PresetFileError {
    fn from(SyntaxError { offset, message }: SyntaxError) -> Self {
        Self::Malformed { offset, message }
    }
}

fn name_problem(name: &str, maximum: usize) -> Option<String> {
    if name.trim().is_empty() {
        Some("name must not be blank".into())
    } else if name.len() > maximum {
        Some(format!(
            "name is {} bytes; the maximum is {maximum}",
            name.len()
        ))
    } else if name.chars().any(char::is_control) {
        Some("name must not contain control characters".into())
    } else {
        None
    }
}

fn check_name(name: &str, maximum: usize) -> Result<(), PresetFileError> {
    name_problem(name, maximum).map_or(Ok(()), |problem| Err(PresetFileError::Invalid(problem)))
}

/// Whether `name` is a plain `.cube` file name: no directory part, not
/// hidden, at most 255 bytes, without control characters.
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)] // exactly `.cube`, by contract
pub fn is_sibling_lut_name(name: &str) -> bool {
    name.len() <= MAX_SIBLING_NAME_BYTES
        && name.len() > ".cube".len()
        && name.ends_with(".cube")
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

fn header(output: &mut String, kind: &str, name: &str) {
    write!(
        output,
        "{{\"schema_version\": {SCHEMA_VERSION}, \"kind\": \"{kind}\", \"name\": \""
    )
    .expect("writing to a string");
    escape_string(output, name);
    output.push_str("\", ");
}

/// Parses the document root, checking the size, schema, and kind.
fn root(source: &str, kind: &'static str) -> Result<Object, PresetFileError> {
    let length = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if length > MAX_PRESET_FILE_BYTES {
        return Err(PresetFileError::TooLarge {
            bytes: length,
            maximum: MAX_PRESET_FILE_BYTES,
        });
    }
    let mut root = Object::new(json::Reader::new(source).document()?, "preset document")?;
    let schema = root.u32("schema_version")?;
    if schema != SCHEMA_VERSION {
        return Err(PresetFileError::UnsupportedSchema {
            found: schema,
            supported: SCHEMA_VERSION,
        });
    }
    let found = root.string("kind")?;
    if found != kind {
        return Err(PresetFileError::WrongKind {
            found,
            expected: kind,
        });
    }
    Ok(root)
}

/// Encodes a `.fmpreset` document.
///
/// # Errors
///
/// [`PresetFileError::Invalid`] for a blank, oversized, or control-character
/// name.
pub fn encode_animation_preset_file(
    preset: &AnimationPresetFile,
) -> Result<String, PresetFileError> {
    check_name(&preset.name, MAX_PRESET_NAME_BYTES)?;
    let mut output = String::new();
    header(&mut output, "animation_preset", &preset.name);
    motion::write_body(&mut output, preset.phase, &preset.motion);
    output.push_str("}\n");
    Ok(output)
}

/// Decodes a `.fmpreset` document.
///
/// # Errors
///
/// [`PresetFileError`] for an oversized, malformed, wrong-schema,
/// wrong-kind, or invalid document.
pub fn decode_animation_preset_file(source: &str) -> Result<AnimationPresetFile, PresetFileError> {
    let mut root = root(source, "animation_preset")?;
    let name = root.string("name")?;
    let (phase, motion) = motion::parse_body(&mut root)?;
    root.finish()?;
    check_name(&name, MAX_PRESET_NAME_BYTES)?;
    Ok(AnimationPresetFile {
        name,
        phase,
        motion,
    })
}

fn check_grade_file(preset: &GradePresetFile) -> Result<(), PresetFileError> {
    check_name(&preset.name, MAX_GRADE_NAME_BYTES)?;
    if preset.grade.lut.is_some() {
        return Err(PresetFileError::Invalid(
            "a shared grade names its LUT in `lut`, not in the grade".into(),
        ));
    }
    preset
        .grade
        .validate()
        .map_err(|error| PresetFileError::Invalid(format!("grade: {error}")))?;
    if let Some(lut) = &preset.lut {
        check_name(&lut.name, MAX_LUT_NAME_BYTES)?;
        if lut.strength_permille > 1_000 {
            return Err(PresetFileError::Invalid(
                "LUT strength_permille exceeds 1000".into(),
            ));
        }
        match &lut.source {
            GradeLutSource::Embedded(text) => {
                if text.len() > MAX_EMBEDDED_LUT_BYTES {
                    return Err(PresetFileError::Invalid(format!(
                        "embedded LUT is {} bytes; the maximum is {MAX_EMBEDDED_LUT_BYTES}",
                        text.len()
                    )));
                }
                parse_cube(text)
                    .map_err(|error| PresetFileError::Invalid(format!("embedded LUT: {error}")))?;
            }
            GradeLutSource::Sibling(file) => {
                if !is_sibling_lut_name(file) {
                    return Err(PresetFileError::Invalid(format!(
                        "LUT file `{file}` is not a plain .cube file name"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Encodes a `.fmgrade` document.
///
/// # Errors
///
/// [`PresetFileError::Invalid`] for an invalid name, a grade outside its
/// bounds or with a LUT reference of its own, an embedded LUT that is too
/// large or does not parse, or a sibling name that is not a plain `.cube`
/// file name.
pub fn encode_grade_preset_file(preset: &GradePresetFile) -> Result<String, PresetFileError> {
    check_grade_file(preset)?;
    let mut output = String::new();
    header(&mut output, "grade", &preset.name);
    output.push_str("\"grade\": ");
    grade::write_grade(&mut output, &preset.grade);
    output.push_str(", \"lut\": ");
    match &preset.lut {
        None => output.push_str("null"),
        Some(lut) => {
            output.push_str("{\"name\": \"");
            escape_string(&mut output, &lut.name);
            write!(
                output,
                "\", \"strength_permille\": {}, ",
                lut.strength_permille
            )
            .expect("writing to a string");
            let (field, value) = match &lut.source {
                GradeLutSource::Embedded(text) => ("cube", text),
                GradeLutSource::Sibling(file) => ("file", file),
            };
            write!(output, "\"{field}\": \"").expect("writing to a string");
            escape_string(&mut output, value);
            output.push_str("\"}");
        }
    }
    output.push_str("}\n");
    Ok(output)
}

/// Decodes a `.fmgrade` document. A sibling LUT is named, not read.
///
/// # Errors
///
/// [`PresetFileError`] for an oversized, malformed, wrong-schema,
/// wrong-kind, or invalid document.
pub fn decode_grade_preset_file(source: &str) -> Result<GradePresetFile, PresetFileError> {
    let mut root = root(source, "grade")?;
    let name = root.string("name")?;
    let grade = grade::parse_grade(root.take("grade")?)?;
    let lut = json::parse_optional(root.take("lut")?, |value| {
        let mut object = Object::new(value, "grade preset lut")?;
        let name = object.string("name")?;
        let strength_permille = u16::try_from(object.u32("strength_permille")?)
            .map_err(|_| json::syntax("strength_permille exceeds u16"))?;
        let source = match (object.take("cube"), object.take("file")) {
            (Ok(json::Value::String(text)), Err(_)) => GradeLutSource::Embedded(text),
            (Err(_), Ok(json::Value::String(file))) => GradeLutSource::Sibling(file),
            _ => {
                return Err(json::syntax(
                    "a grade preset LUT holds exactly one of `cube` or `file`, a string",
                ));
            }
        };
        object.finish()?;
        Ok(GradeFileLut {
            name,
            strength_permille,
            source,
        })
    })?;
    root.finish()?;
    let preset = GradePresetFile { name, grade, lut };
    check_grade_file(&preset)?;
    Ok(preset)
}

/// Reads at most `maximum` bytes of UTF-8 text from a regular file.
fn read_text(path: &Path, maximum: u64) -> Result<String, PresetFileError> {
    let not_regular = || {
        PresetFileError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ))
    };
    // Checked before opening too: opening a FIFO or device can block.
    if !fs::metadata(path)?.is_file() {
        return Err(not_regular());
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    if metadata.len() > maximum {
        return Err(PresetFileError::TooLarge {
            bytes: metadata.len(),
            maximum,
        });
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > maximum {
        return Err(PresetFileError::TooLarge {
            bytes: length,
            maximum,
        });
    }
    String::from_utf8(bytes).map_err(|_| PresetFileError::NotUtf8)
}

/// Writes a `.fmpreset` file atomically.
///
/// # Errors
///
/// [`PresetFileError`] when the preset does not encode or the file cannot be
/// written.
pub fn write_animation_preset_file(
    path: &Path,
    preset: &AnimationPresetFile,
) -> Result<(), PresetFileError> {
    let document = encode_animation_preset_file(preset)?;
    write_atomically(path, document.as_bytes())?;
    Ok(())
}

/// Reads and decodes a `.fmpreset` file.
///
/// # Errors
///
/// [`PresetFileError`] when the file cannot be read or does not decode.
pub fn read_animation_preset_file(path: &Path) -> Result<AnimationPresetFile, PresetFileError> {
    decode_animation_preset_file(&read_text(path, MAX_PRESET_FILE_BYTES)?)
}

/// A LUT read with its grade: name, strength, and the exact `.cube` text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedGradeLut {
    pub name: String,
    pub strength_permille: u16,
    pub text: String,
}

/// A `.fmgrade` file read with its LUT text (embedded or from the sibling
/// file).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedGradePreset {
    pub name: String,
    /// Without a LUT reference; the importer adds one for the LUT it
    /// registers.
    pub grade: Grade,
    pub lut: Option<LoadedGradeLut>,
}

/// Reads a `.fmgrade` file and, for a sibling LUT, the `.cube` file beside
/// it (at most [`MAX_LUT_FILE_BYTES`], parsed to check it).
///
/// # Errors
///
/// [`PresetFileError`] when either file cannot be read, the document does
/// not decode, or the sibling LUT does not parse.
pub fn read_grade_preset_file(path: &Path) -> Result<LoadedGradePreset, PresetFileError> {
    let preset = decode_grade_preset_file(&read_text(path, MAX_PRESET_FILE_BYTES)?)?;
    let lut = match preset.lut {
        None => None,
        Some(lut) => {
            let text = match lut.source {
                GradeLutSource::Embedded(text) => text,
                GradeLutSource::Sibling(file) => {
                    let sibling = path.parent().unwrap_or(Path::new(".")).join(&file);
                    let text = read_text(&sibling, MAX_LUT_FILE_BYTES).map_err(|error| {
                        PresetFileError::Invalid(format!("LUT file {file}: {error}"))
                    })?;
                    parse_cube(&text).map_err(|error| {
                        PresetFileError::Invalid(format!("LUT file {file}: {error}"))
                    })?;
                    text
                }
            };
            Some(LoadedGradeLut {
                name: lut.name,
                strength_permille: lut.strength_permille,
                text,
            })
        }
    };
    Ok(LoadedGradePreset {
        name: preset.name,
        grade: preset.grade,
        lut,
    })
}

/// The LUT of a grade being exported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GradeExportLut<'a> {
    pub name: &'a str,
    pub strength_permille: u16,
    /// The exact `.cube` text, or `None` when the exporter cannot read it:
    /// the file then names a sibling `.cube` the operator must supply.
    pub text: Option<&'a str>,
}

/// Where an exported grade's LUT went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportedGradeLut {
    /// Inside the `.fmgrade`.
    Embedded,
    /// In the sibling file `path`; `written` is false when the exporter had
    /// no text and the file must be supplied by hand.
    Sibling { path: PathBuf, written: bool },
}

/// The sibling `.cube` name for a `.fmgrade` at `path`: its file stem plus
/// `.cube`, or `lut.cube` when the stem is not a plain name.
#[must_use]
pub fn sibling_lut_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| format!("{stem}.cube"))
        .filter(|name| is_sibling_lut_name(name))
        .unwrap_or_else(|| "lut.cube".to_owned())
}

/// Writes a `.fmgrade` file for `grade` (its own LUT reference is dropped)
/// named `name`, embedding `lut`'s text when it is at most
/// [`MAX_EMBEDDED_LUT_BYTES`] and otherwise writing it to the sibling
/// `.cube` ([`sibling_lut_name`]). A sibling is written before the
/// `.fmgrade`, so a file that names one always finds it.
///
/// # Errors
///
/// [`PresetFileError`] when the grade does not encode or a file cannot be
/// written.
pub fn write_grade_preset_file(
    path: &Path,
    name: &str,
    grade: &Grade,
    lut: Option<GradeExportLut<'_>>,
) -> Result<Option<ExportedGradeLut>, PresetFileError> {
    let mut grade = grade.clone();
    grade.lut = None;
    let mut exported = None;
    let lut = match lut {
        None => None,
        Some(lut) => {
            let source = match lut.text {
                Some(text) if text.len() <= MAX_EMBEDDED_LUT_BYTES => {
                    exported = Some(ExportedGradeLut::Embedded);
                    GradeLutSource::Embedded(text.to_owned())
                }
                text => {
                    let file = sibling_lut_name(path);
                    let sibling = path.parent().unwrap_or(Path::new(".")).join(&file);
                    if let Some(text) = text {
                        write_atomically(&sibling, text.as_bytes())?;
                    }
                    exported = Some(ExportedGradeLut::Sibling {
                        path: sibling,
                        written: text.is_some(),
                    });
                    GradeLutSource::Sibling(file)
                }
            };
            Some(GradeFileLut {
                name: lut.name.to_owned(),
                strength_permille: lut.strength_permille,
                source,
            })
        }
    };
    let document = encode_grade_preset_file(&GradePresetFile {
        name: name.to_owned(),
        grade,
        lut,
    })?;
    write_atomically(path, document.as_bytes())?;
    Ok(exported)
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to a fresh hidden sibling of `path`, syncs it, and renames
/// it over `path`, so readers see the old file or the whole new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().map_or_else(
        || "preset".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}
