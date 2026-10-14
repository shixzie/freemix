//! Bounded, rotating autosave snapshots inside a project bundle.
//!
//! Each autosave is a complete manifest in the exact current `project.json`
//! format, written atomically to `autosave/autosave-<generation>.json`.
//! Generations increase monotonically; after each write only the newest
//! retained generations are kept. Recovery walks generations from newest to
//! oldest and returns the first that strictly parses, validates, and belongs
//! to the bundle's project, reporting every newer generation it had to skip.

use std::{error::Error, fmt, fs, io, path::PathBuf};

use fm_types::ProjectId;

use crate::{
    ProjectStore, StoreError, StoredProject, fsutil, json,
    store::{enforce_manifest_size, read_manifest_file},
};

const AUTOSAVE_DIRECTORY: &str = "autosave";
const AUTOSAVE_PREFIX: &str = "autosave-";
const AUTOSAVE_SUFFIX: &str = ".json";
const GENERATION_DIGITS: usize = 20;
/// Generations retained when the caller does not choose otherwise.
pub const DEFAULT_AUTOSAVE_GENERATIONS: usize = 5;
/// Upper bound on retained autosave generations.
pub const MAX_AUTOSAVE_GENERATIONS: usize = 64;

/// One autosave file found in the bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutosaveGeneration {
    pub generation: u64,
    pub path: PathBuf,
}

/// Result of writing one autosave generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutosaveWrite {
    pub generation: u64,
    pub path: PathBuf,
    /// Older generations removed to honor the retention bound, oldest first.
    pub pruned: Vec<u64>,
}

/// Why recovery skipped an autosave generation.
#[derive(Debug)]
pub enum AutosaveRejection {
    /// The file could not be read, parsed, or validated.
    Unreadable(StoreError),
    /// The snapshot is valid but names a different project than the manifest.
    ForeignProject {
        found: ProjectId,
        expected: ProjectId,
    },
}

impl fmt::Display for AutosaveRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(error) => write!(formatter, "autosave is unusable: {error}"),
            Self::ForeignProject { found, expected } => write!(
                formatter,
                "autosave belongs to project {found}, not project {expected}"
            ),
        }
    }
}

/// The newest usable autosave and the newer generations that were skipped.
#[derive(Debug)]
pub struct AutosaveRecovery {
    /// Newest valid generation and its snapshot, if any.
    pub recovered: Option<(u64, StoredProject)>,
    /// Generations newer than the recovered one (or all, if none recovered)
    /// that were rejected, newest first.
    pub rejected: Vec<(u64, AutosaveRejection)>,
    /// Whether the recovered snapshot differs from the durable manifest. True
    /// when the manifest itself is missing or unusable.
    pub differs_from_manifest: bool,
}

/// A failure to write or enumerate autosaves.
#[derive(Debug)]
pub enum AutosaveError {
    /// The retention bound is outside `1..=MAX_AUTOSAVE_GENERATIONS`.
    InvalidRetention {
        requested: usize,
    },
    /// The next generation number would overflow.
    GenerationExhausted,
    Store(StoreError),
}

impl fmt::Display for AutosaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRetention { requested } => write!(
                formatter,
                "autosave retention {requested} is outside 1..={MAX_AUTOSAVE_GENERATIONS}"
            ),
            Self::GenerationExhausted => formatter.write_str("autosave generations exhausted"),
            Self::Store(error) => error.fmt(formatter),
        }
    }
}

impl Error for AutosaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::InvalidRetention { .. } | Self::GenerationExhausted => None,
        }
    }
}

impl From<io::Error> for AutosaveError {
    fn from(error: io::Error) -> Self {
        Self::Store(StoreError::Io(error))
    }
}

impl ProjectStore {
    /// Directory inside the bundle holding autosave generations.
    #[must_use]
    pub fn autosave_path(&self) -> PathBuf {
        self.root().join(AUTOSAVE_DIRECTORY)
    }

    /// Lists well-named autosave generations, oldest first.
    ///
    /// Files that do not match the exact generation naming scheme, including
    /// interrupted temporary files, are ignored.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors other than a missing autosave directory.
    pub fn list_autosaves(&self) -> Result<Vec<AutosaveGeneration>, AutosaveError> {
        let directory = self.autosave_path();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut generations = Vec::new();
        for entry in entries {
            let entry = entry?;
            if let Some(generation) = entry.file_name().to_str().and_then(parse_generation) {
                generations.push(AutosaveGeneration {
                    generation,
                    path: entry.path(),
                });
            }
        }
        generations.sort_unstable_by_key(|generation| generation.generation);
        Ok(generations)
    }

    /// Atomically writes `project` as the next autosave generation and prunes
    /// all but the newest `retain` generations.
    ///
    /// The snapshot is validated before anything touches the filesystem. A
    /// failed write leaves every existing generation intact.
    ///
    /// # Errors
    ///
    /// Returns an invalid retention bound, validation, size-limit, generation
    /// overflow, or filesystem error.
    pub fn write_autosave(
        &self,
        project: &StoredProject,
        retain: usize,
    ) -> Result<AutosaveWrite, AutosaveError> {
        if !(1..=MAX_AUTOSAVE_GENERATIONS).contains(&retain) {
            return Err(AutosaveError::InvalidRetention { requested: retain });
        }
        project
            .validate()
            .map_err(|error| AutosaveError::Store(StoreError::Validation(error)))?;
        let manifest = json::encode(project);
        enforce_manifest_size(manifest.len().try_into().unwrap_or(u64::MAX))
            .map_err(AutosaveError::Store)?;

        let directory = self.autosave_path();
        fs::create_dir_all(&directory)?;
        let existing = self.list_autosaves()?;
        let generation = existing
            .last()
            .map_or(Some(1), |last| last.generation.checked_add(1))
            .ok_or(AutosaveError::GenerationExhausted)?;
        let path = directory.join(generation_name(generation));
        fsutil::write_atomically(&path, manifest.as_bytes())?;

        let keep_existing = retain - 1;
        let prune_count = existing.len().saturating_sub(keep_existing);
        let mut pruned = Vec::with_capacity(prune_count);
        for old in &existing[..prune_count] {
            match fs::remove_file(&old.path) {
                Ok(()) => pruned.push(old.generation),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !pruned.is_empty() {
            crate::store::sync_directory(&directory).map_err(AutosaveError::Store)?;
        }
        Ok(AutosaveWrite {
            generation,
            path,
            pruned,
        })
    }

    /// Finds the newest autosave that strictly loads and belongs to this
    /// bundle's project.
    ///
    /// When the durable manifest loads, snapshots of a different project id
    /// are rejected. When it does not, any valid snapshot is accepted and
    /// `differs_from_manifest` is true.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors from enumerating the autosave directory.
    /// Individual unusable generations are reported, not returned as errors.
    pub fn recover_autosave(&self) -> Result<AutosaveRecovery, AutosaveError> {
        let manifest = self.load().ok();
        let mut rejected = Vec::new();
        for candidate in self.list_autosaves()?.into_iter().rev() {
            match read_manifest_file(&candidate.path) {
                Err(error) => {
                    rejected.push((candidate.generation, AutosaveRejection::Unreadable(error)));
                }
                Ok(snapshot) => {
                    if let Some(manifest) = &manifest
                        && manifest.project().id() != snapshot.project().id()
                    {
                        rejected.push((
                            candidate.generation,
                            AutosaveRejection::ForeignProject {
                                found: snapshot.project().id(),
                                expected: manifest.project().id(),
                            },
                        ));
                        continue;
                    }
                    let differs_from_manifest = manifest.as_ref() != Some(&snapshot);
                    return Ok(AutosaveRecovery {
                        recovered: Some((candidate.generation, snapshot)),
                        rejected,
                        differs_from_manifest,
                    });
                }
            }
        }
        Ok(AutosaveRecovery {
            recovered: None,
            rejected,
            differs_from_manifest: false,
        })
    }
}

fn generation_name(generation: u64) -> String {
    format!("{AUTOSAVE_PREFIX}{generation:0GENERATION_DIGITS$}{AUTOSAVE_SUFFIX}")
}

fn parse_generation(name: &str) -> Option<u64> {
    let digits = name
        .strip_prefix(AUTOSAVE_PREFIX)?
        .strip_suffix(AUTOSAVE_SUFFIX)?;
    if digits.len() != GENERATION_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|generation| *generation != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn generation_names_round_trip_and_reject_lookalikes() {
        assert_eq!(generation_name(7), "autosave-00000000000000000007.json");
        assert_eq!(parse_generation(&generation_name(7)), Some(7));
        assert_eq!(parse_generation(&generation_name(u64::MAX)), Some(u64::MAX));
        for name in [
            "autosave-7.json",
            "autosave-00000000000000000000.json",
            "autosave-0000000000000000000a.json",
            "autosave-99999999999999999999.json",
            ".autosave-00000000000000000007.json.tmp-1-2",
            "autosave-00000000000000000007.json.bak",
        ] {
            assert_eq!(parse_generation(name), None, "{name}");
        }
    }

    #[test]
    fn autosave_directory_is_inside_bundle() {
        let store = ProjectStore::new("show.freemix").unwrap();
        assert_eq!(store.autosave_path(), Path::new("show.freemix/autosave"));
    }
}
