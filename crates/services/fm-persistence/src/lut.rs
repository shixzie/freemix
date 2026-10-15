//! Imported `.cube` LUTs bundled at `assets/luts/<id>.cube`.
//!
//! Import reads the source once under [`MAX_LUT_FILE_BYTES`], parses it with
//! [`fm_grade::parse_cube`] (which bounds the 3D edge and 1D length), and
//! writes exactly the bytes it parsed, so the bundled file is the file that
//! was validated. Loading re-reads under the same cap and refuses a file
//! whose kind or size differs from the project's record.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Read as _},
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

use fm_grade::{CubeError, CubeKind, CubeLut, CubeWarning, parse_cube};
use fm_model::{GradeEditError, LutId, MAX_LUT_FILE_BYTES, Project, ProjectLut, lut_asset_key};

use crate::{AssetResolveError, ProjectStore, fsutil};

/// A LUT copied into the bundle and registered on the project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedLut {
    pub lut: ProjectLut,
    /// Bytes written to `assets/luts/<id>.cube`.
    pub bytes: u64,
    /// Non-fatal parser observations.
    pub warnings: Vec<CubeWarning>,
}

/// A LUT that cannot be imported or loaded.
#[derive(Debug)]
pub enum LutError {
    Io(io::Error),
    NotRegularFile(PathBuf),
    /// The file exceeds [`MAX_LUT_FILE_BYTES`].
    TooLarge {
        maximum: u64,
    },
    NotUtf8,
    Parse(CubeError),
    /// The bundled file does not match the kind and size the project records.
    Mismatch {
        expected: (CubeKind, u32),
        found: (CubeKind, usize),
    },
    Asset(AssetResolveError),
    Project(GradeEditError),
    /// Every candidate id is taken.
    NoAvailableId,
}

impl fmt::Display for LutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "LUT I/O failed: {error}"),
            Self::NotRegularFile(path) => {
                write!(formatter, "{} is not a regular file", path.display())
            }
            Self::TooLarge { maximum } => {
                write!(formatter, "LUT file exceeds the {maximum}-byte limit")
            }
            Self::NotUtf8 => formatter.write_str("LUT file is not UTF-8 text"),
            Self::Parse(error) => write!(formatter, "invalid .cube LUT: {error}"),
            Self::Mismatch { expected, found } => write!(
                formatter,
                "bundled LUT is {:?} size {}, but the project records {:?} size {}",
                found.0, found.1, expected.0, expected.1
            ),
            Self::Asset(error) => write!(formatter, "LUT asset: {error}"),
            Self::Project(error) => error.fmt(formatter),
            Self::NoAvailableId => formatter.write_str("no LUT id is available"),
        }
    }
}

impl Error for LutError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Asset(error) => Some(error),
            Self::Project(error) => Some(error),
            Self::NotRegularFile(_)
            | Self::TooLarge { .. }
            | Self::NotUtf8
            | Self::Mismatch { .. }
            | Self::NoAvailableId => None,
        }
    }
}

impl From<io::Error> for LutError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads at most [`MAX_LUT_FILE_BYTES`] of UTF-8 text from a regular file.
fn read_bounded(path: &Path) -> Result<String, LutError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(LutError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > MAX_LUT_FILE_BYTES {
        return Err(LutError::TooLarge {
            maximum: MAX_LUT_FILE_BYTES,
        });
    }
    // The file may grow after the metadata check; never read past the cap.
    let mut bytes = Vec::new();
    file.take(MAX_LUT_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LUT_FILE_BYTES {
        return Err(LutError::TooLarge {
            maximum: MAX_LUT_FILE_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|_| LutError::NotUtf8)
}

fn recorded_size(lut: &CubeLut) -> Result<u32, LutError> {
    u32::try_from(lut.size()).map_err(|_| LutError::TooLarge {
        maximum: MAX_LUT_FILE_BYTES,
    })
}

impl ProjectStore {
    /// Copies a `.cube` file into the bundle as `assets/luts/<id>.cube` and
    /// registers it on `project` under `name` with the next free id.
    ///
    /// The project is changed only when the file was written; the caller
    /// saves it. A refused import leaves no file behind.
    ///
    /// # Errors
    ///
    /// Returns [`LutError`] for an unreadable, oversized, non-UTF-8, or
    /// invalid `.cube` file, for a name or LUT count the project refuses, and
    /// for I/O failures.
    pub fn import_lut(
        &self,
        project: &mut Project,
        source: &Path,
        name: &str,
    ) -> Result<ImportedLut, LutError> {
        let text = read_bounded(source)?;
        let (parsed, warnings) = parse_cube(&text).map_err(LutError::Parse)?;
        let size = recorded_size(&parsed)?;
        let first = project
            .luts()
            .iter()
            .map(|lut| lut.id.get().get())
            .max()
            .unwrap_or(0);
        let assets_root = self.assets_root();
        let mut candidate = first;
        let (id, destination) = loop {
            candidate = candidate.checked_add(1).ok_or(LutError::NoAvailableId)?;
            let id = LutId::new(NonZeroU32::new(candidate).ok_or(LutError::NoAvailableId)?);
            // An orphaned file from an earlier, unsaved import keeps its id.
            if fs::symlink_metadata(assets_root.join(lut_asset_key(id))).is_err() {
                let destination = fsutil::create_parents_within(&assets_root, &lut_asset_key(id))?;
                break (id, destination);
            }
        };
        let lut = ProjectLut {
            id,
            name: name.to_owned(),
            kind: parsed.kind(),
            size,
        };
        let mut candidate_project = project.clone();
        candidate_project
            .add_lut(lut.clone())
            .map_err(LutError::Project)?;
        fsutil::write_atomically(&destination, text.as_bytes())?;
        *project = candidate_project;
        Ok(ImportedLut {
            lut,
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
            warnings,
        })
    }

    /// Loads and parses one registered LUT from the bundle.
    ///
    /// # Errors
    ///
    /// Returns [`LutError`] when the file does not resolve inside `assets/`,
    /// exceeds the size cap, does not parse, or differs from the recorded
    /// kind and size.
    pub fn load_lut(&self, lut: &ProjectLut) -> Result<Arc<CubeLut>, LutError> {
        self.load_lut_source(lut).map(|(_, parsed)| parsed)
    }

    /// Loads one registered LUT from the bundle with its exact text.
    ///
    /// # Errors
    ///
    /// Returns [`LutError`] exactly like [`Self::load_lut`].
    pub fn load_lut_source(&self, lut: &ProjectLut) -> Result<(Arc<str>, Arc<CubeLut>), LutError> {
        let path = self
            .resolve_asset_uri(&lut.asset_uri())
            .map_err(LutError::Asset)?;
        let text = read_bounded(&path)?;
        let (parsed, _) = parse_cube(&text).map_err(LutError::Parse)?;
        if parsed.kind() != lut.kind || recorded_size(&parsed)? != lut.size {
            return Err(LutError::Mismatch {
                expected: (lut.kind, lut.size),
                found: (parsed.kind(), parsed.size()),
            });
        }
        Ok((Arc::from(text), Arc::new(parsed)))
    }

    /// Writes `.cube` text the caller already validated (for example a LUT
    /// imported through the engine) to `assets/luts/<id>.cube`, atomically
    /// replacing any earlier file at that id.
    ///
    /// # Errors
    ///
    /// Returns [`LutError::TooLarge`] above [`MAX_LUT_FILE_BYTES`] and
    /// [`LutError::Io`] when the file cannot be written.
    pub fn write_lut_source(&self, id: LutId, text: &str) -> Result<(), LutError> {
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_LUT_FILE_BYTES {
            return Err(LutError::TooLarge {
                maximum: MAX_LUT_FILE_BYTES,
            });
        }
        let destination = fsutil::create_parents_within(&self.assets_root(), &lut_asset_key(id))?;
        fsutil::write_atomically(&destination, text.as_bytes())?;
        Ok(())
    }

    /// Loads every LUT a grade of `project` references.
    ///
    /// # Errors
    ///
    /// Returns the first LUT that fails to load, with its error.
    pub fn load_grade_luts(
        &self,
        project: &Project,
    ) -> Result<BTreeMap<LutId, Arc<CubeLut>>, (LutId, LutError)> {
        let grading = project.grading();
        grading
            .referenced_luts()
            .into_iter()
            .filter_map(|id| grading.lut(id))
            .map(|lut| {
                self.load_lut(lut)
                    .map(|loaded| (lut.id, loaded))
                    .map_err(|error| (lut.id, error))
            })
            .collect()
    }
}
