//! Media import, missing-media relink, and asset-key portability helpers.
//!
//! Project media inputs always reference `asset://<key>` URIs that resolve
//! beneath the bundle's `assets/` directory, so a bundle never stores an
//! absolute host path. Importing copies an external file into `assets/`;
//! relinking repairs a media input whose asset is missing either by pointing
//! it at a same-named file already inside `assets/` or by importing a
//! same-named file found in caller-supplied search directories.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::{Path, PathBuf},
};

use fm_model::InputKind;
use fm_types::InputId;

use crate::{
    AssetAuditIssue, AssetAuditReason, ProjectStore, ProjectValidationError, StoreError,
    StoredProject,
    asset::{ASSET_URI_PREFIX, asset_uri_key},
    fsutil,
};

/// Most numbered suffixes tried before key allocation gives up.
const MAX_KEY_SUFFIX: u32 = 10_000;
/// Longest file-name segment produced for an imported or portable key.
const MAX_GENERATED_SEGMENT_BYTES: usize = 200;
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A failure of a bundle-level operation.
#[derive(Debug)]
pub enum BundleError {
    Store(StoreError),
    Io(io::Error),
    Validation(ProjectValidationError),
    /// A media source path is not a regular file.
    NotRegularFile(PathBuf),
    /// No free asset key could be allocated for `key`.
    NoAvailableKey {
        key: String,
    },
    /// A plan entry no longer matches the project or the filesystem.
    StalePlan {
        input: InputId,
    },
    /// An asset key is occupied by a file with different contents.
    Collision {
        key: String,
    },
    /// The destination bundle path already exists; it is never overwritten.
    DestinationExists(PathBuf),
    /// The destination would be created inside the source bundle.
    DestinationInsideSource(PathBuf),
    /// Referenced media could not be resolved, so the result would not be
    /// self-contained.
    UnresolvedAssets(Vec<AssetAuditIssue>),
    /// A title template, font, or image key would have to be renamed in the
    /// destination, which would require rewriting the stored template and
    /// title fields; that is not supported, so the operation is refused
    /// instead of dropping or silently rebinding the asset.
    TitleAssetRewriteUnsupported {
        uri: String,
    },
    /// An imported LUT's bundled file could not be resolved.
    UnresolvedLut {
        uri: String,
    },
    /// The source journal holds acknowledged batches the manifest lacks.
    UnappliedJournal {
        batches: usize,
    },
    /// A template must be instantiated under a different project id.
    TemplateIdentityReused,
    /// The new project name is blank.
    EmptyName,
    /// Fresh ids for appended entities would overflow.
    IdSpaceExhausted,
    /// Appended grades do not fit the target's grading (for example a full
    /// library or LUT table).
    Grading(fm_model::GradeEditError),
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::Validation(error) => error.fmt(formatter),
            Self::NotRegularFile(path) => {
                write!(formatter, "`{}` is not a regular file", path.display())
            }
            Self::NoAvailableKey { key } => {
                write!(formatter, "no free asset key is available for `{key}`")
            }
            Self::StalePlan { input } => {
                write!(formatter, "relink plan for input {input} is stale")
            }
            Self::Collision { key } => write!(
                formatter,
                "asset `{key}` already exists with different contents"
            ),
            Self::DestinationExists(path) => write!(
                formatter,
                "destination `{}` already exists and is never overwritten",
                path.display()
            ),
            Self::DestinationInsideSource(path) => write!(
                formatter,
                "destination `{}` lies inside the source bundle",
                path.display()
            ),
            Self::UnresolvedAssets(issues) => write!(
                formatter,
                "{} media input(s) reference unresolved assets",
                issues.len()
            ),
            Self::TitleAssetRewriteUnsupported { uri } => write!(
                formatter,
                "title asset `{uri}` would need a different key in the destination; \
                 rewriting title asset keys is not supported"
            ),
            Self::UnresolvedLut { uri } => {
                write!(formatter, "imported LUT `{uri}` cannot be resolved")
            }
            Self::UnappliedJournal { batches } => write!(
                formatter,
                "the source journal holds {batches} unapplied batch(es); checkpoint it first"
            ),
            Self::TemplateIdentityReused => {
                formatter.write_str("a project from a template needs a fresh project id")
            }
            Self::EmptyName => formatter.write_str("project name must not be blank"),
            Self::IdSpaceExhausted => formatter.write_str("no fresh entity ids remain"),
            Self::Grading(error) => write!(formatter, "appended grades do not fit: {error}"),
        }
    }
}

impl Error for BundleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for BundleError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StoreError> for BundleError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<ProjectValidationError> for BundleError {
    fn from(error: ProjectValidationError) -> Self {
        Self::Validation(error)
    }
}

/// How an asset reached its key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetDisposition {
    /// The bytes were copied to a previously free key.
    Copied { bytes: u64 },
    /// An identical file already occupied the key and was reused.
    Reused,
}

/// An external file imported into a bundle's `assets/` directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedAsset {
    pub asset_uri: String,
    pub disposition: AssetDisposition,
}

/// Where to look for missing media.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelinkSearch {
    /// Directories searched in priority order, after the bundle's own assets.
    pub directories: Vec<PathBuf>,
    /// Directory levels below each search root that are visited.
    pub max_depth: usize,
    /// Directory entries inspected across the whole search.
    pub max_entries: usize,
}

impl RelinkSearch {
    #[must_use]
    pub const fn new(directories: Vec<PathBuf>) -> Self {
        Self {
            directories,
            max_depth: 8,
            max_entries: 100_000,
        }
    }
}

/// The proposed repair for one media input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelinkResolution {
    /// A same-named asset already inside the bundle: point the input at it.
    RewriteUri { asset_uri: String },
    /// A same-named external file: copy it to the input's original key.
    Import { source: PathBuf, bytes: u64 },
    /// A same-named external folder with pictures: copy its pictures to the
    /// input's original folder key (a folder slideshow).
    ImportFolder { source: PathBuf, bytes: u64 },
    /// Several equally ranked candidates with different contents. Nothing is
    /// chosen automatically; candidates are sorted.
    Ambiguous { candidates: Vec<PathBuf> },
    /// No candidate was found.
    NotFound,
    /// The reference cannot be repaired by searching: its URI is invalid, it
    /// resolves outside the assets directory, or it names a non-file.
    Unrelinkable,
}

/// One unresolved media input and its proposed repair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelinkEntry {
    pub input_id: InputId,
    pub asset_uri: String,
    pub reason: AssetAuditReason,
    pub resolution: RelinkResolution,
}

/// A deterministic report of every unresolved media input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelinkPlan {
    /// Entries sorted by input id.
    pub entries: Vec<RelinkEntry>,
    /// Whether [`RelinkSearch::max_entries`] cut the search short.
    pub search_truncated: bool,
}

/// The project after applying a relink plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelinkOutcome {
    pub project: StoredProject,
    /// Inputs that now resolve, sorted.
    pub relinked: Vec<InputId>,
    /// Inputs left unresolved (ambiguous, not found, or unrelinkable), sorted.
    pub unresolved: Vec<InputId>,
    pub imported_bytes: u64,
}

impl ProjectStore {
    /// Copies the external regular file `source` into this bundle's assets.
    ///
    /// The key is derived from the file name, made portable (Windows-reserved
    /// characters and names, control characters, and characters the asset URI
    /// syntax rejects are replaced with `_`; a name that is not valid UTF-8 is
    /// converted lossily). When the key is occupied by an identical file it is
    /// reused; otherwise `name-2.ext`, `name-3.ext`, ... are tried. An existing
    /// asset is never overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::NotRegularFile`], [`BundleError::NoAvailableKey`],
    /// or an I/O error.
    pub fn import_media_file(&self, source: &Path) -> Result<ImportedAsset, BundleError> {
        if !fs::metadata(source)?.is_file() {
            return Err(BundleError::NotRegularFile(source.to_path_buf()));
        }
        let base = imported_key(source.file_name().unwrap_or_else(|| OsStr::new("media")));
        let (key, disposition) = place_file(&self.assets_root(), source, &base, |_| false)?;
        Ok(ImportedAsset {
            asset_uri: asset_uri(&key),
            disposition,
        })
    }

    /// Copies every picture file directly inside the external folder
    /// `source` (regular files with a picture extension, at most
    /// `max_pictures` in name order) into a new folder of this bundle's
    /// assets, for a folder slideshow (IN-009).
    ///
    /// The folder key is derived from the folder name as
    /// [`Self::import_media_file`] derives a file key; an occupied key is
    /// never reused, `name-2`, `name-3`, ... are tried instead. The
    /// returned URI names the folder.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::NotRegularFile`] when `source` is not a
    /// folder, [`BundleError::NoAvailableKey`], or an I/O error (after
    /// removing the partial copy).
    pub fn import_picture_folder(
        &self,
        source: &Path,
        max_pictures: usize,
    ) -> Result<ImportedAsset, BundleError> {
        if !fs::metadata(source)?.is_dir() {
            return Err(BundleError::NotRegularFile(source.to_path_buf()));
        }
        let mut names = fs::read_dir(source)?
            .filter_map(Result::ok)
            .filter(|entry| fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file()))
            .map(|entry| entry.file_name())
            .filter(|name| fm_types::still::is_image_file_name(&name.to_string_lossy()))
            .collect::<Vec<_>>();
        names.sort();
        names.truncate(max_pictures);
        let base = imported_key(source.file_name().unwrap_or_else(|| OsStr::new("slides")));
        let assets_root = self.assets_root();
        fs::create_dir_all(&assets_root)?;
        for attempt in 1..=MAX_KEY_SUFFIX {
            let key = if attempt <= 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            if asset_uri_key(&asset_uri(&key)).is_err() {
                continue;
            }
            let folder = assets_root.join(&key);
            match fs::create_dir(&folder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
            let mut bytes = 0_u64;
            let mut taken = BTreeSet::new();
            for name in &names {
                let file = imported_key(name);
                if !taken.insert(file.clone()) {
                    continue;
                }
                match fsutil::copy_new_file(&source.join(name), &folder.join(&file)) {
                    Ok(copied) => bytes = bytes.saturating_add(copied),
                    Err(error) => {
                        let _ = fs::remove_dir_all(&folder);
                        return Err(error.into());
                    }
                }
            }
            return Ok(ImportedAsset {
                asset_uri: asset_uri(&key),
                disposition: AssetDisposition::Copied { bytes },
            });
        }
        Err(BundleError::NoAvailableKey { key: base })
    }

    /// Removes the asset an [`Self::import_media_file`] copy (or the folder
    /// an [`Self::import_picture_folder`] copy) created at `asset_uri`, when
    /// an import is abandoned. A file that is already gone is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns an [`io::ErrorKind::InvalidInput`] error for a URI that is
    /// not an asset URI, or another I/O error.
    pub fn remove_imported_media(&self, asset_uri: &str) -> Result<(), BundleError> {
        let key = asset_uri_key(asset_uri).map_err(|error| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("{asset_uri}: {error}"))
        })?;
        let path = self.assets_root().join(key);
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            return fs::remove_dir_all(path).map_err(Into::into);
        }
        match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// Plans repairs for every media input whose asset does not resolve.
    ///
    /// Candidates are files whose name equals the final segment of the
    /// missing key. They are ranked by source (the bundle's own `assets/`
    /// first, then `search.directories` in order), then depth below that
    /// source, then path. The best-ranked group is chosen only when all of its
    /// files have identical contents; otherwise the entry is ambiguous.
    /// Symlinked directories are not descended into; symlinked files are
    /// considered by their targets.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a search root cannot be read.
    #[allow(clippy::too_many_lines)] // one candidate group per ranked directory
    pub fn plan_media_relink(
        &self,
        project: &StoredProject,
        search: &RelinkSearch,
    ) -> Result<RelinkPlan, BundleError> {
        // Title assets are not media: they are repaired by re-importing the
        // template, never by name search.
        let issues = self
            .audit_assets(project)
            .into_iter()
            .filter(|issue| media_uri(project, issue.input_id).is_some())
            .collect::<Vec<_>>();
        let mut wanted_files = BTreeSet::new();
        let mut wanted_folders = BTreeSet::new();
        for issue in &issues {
            if issue.reason != AssetAuditReason::MissingAsset {
                continue;
            }
            let Some(name) = media_uri(project, issue.input_id)
                .and_then(|uri| asset_uri_key(uri).ok())
                .and_then(|key| key.rsplit('/').next())
            else {
                continue;
            };
            if media_is_folder(project, issue.input_id) {
                wanted_folders.insert(OsString::from(name));
            } else {
                wanted_files.insert(OsString::from(name));
            }
        }

        let mut budget = search.max_entries;
        let mut truncated = false;
        let mut found: BTreeMap<OsString, Vec<Candidate>> = BTreeMap::new();
        let mut found_folders: BTreeMap<OsString, Vec<Candidate>> = BTreeMap::new();
        if !wanted_files.is_empty() || !wanted_folders.is_empty() {
            let assets_root = fs::canonicalize(self.assets_root()).ok();
            let mut roots = vec![(0usize, assets_root.clone())];
            for (index, directory) in search.directories.iter().enumerate() {
                roots.push((index + 1, Some(fs::canonicalize(directory)?)));
            }
            for (rank, directory) in roots {
                let Some(directory) = directory else {
                    continue;
                };
                if rank > 0
                    && assets_root
                        .as_ref()
                        .is_some_and(|root| directory.starts_with(root))
                {
                    continue;
                }
                if !wanted_files.is_empty() {
                    truncated |= collect_named(
                        &directory,
                        rank,
                        search.max_depth,
                        &wanted_files,
                        &mut budget,
                        &mut found,
                    )?;
                }
                if !wanted_folders.is_empty() {
                    truncated |= collect_named_folders(
                        &directory,
                        rank,
                        search.max_depth,
                        &wanted_folders,
                        &mut budget,
                        &mut found_folders,
                    )?;
                }
            }
        }

        let assets_root = fs::canonicalize(self.assets_root()).ok();
        let mut entries = Vec::with_capacity(issues.len());
        for issue in issues {
            let asset_uri = media_uri(project, issue.input_id)
                .unwrap_or_default()
                .to_owned();
            let folder = media_is_folder(project, issue.input_id);
            let resolution = if issue.reason == AssetAuditReason::MissingAsset {
                let name = asset_uri_key(&asset_uri)
                    .ok()
                    .and_then(|key| key.rsplit('/').next())
                    .unwrap_or_default();
                if folder {
                    let candidates = found_folders
                        .get(OsStr::new(name))
                        .map_or(&[][..], Vec::as_slice);
                    choose_folder(candidates, assets_root.as_deref())?
                } else {
                    let candidates = found.get(OsStr::new(name)).map_or(&[][..], Vec::as_slice);
                    choose(candidates, assets_root.as_deref())?
                }
            } else {
                RelinkResolution::Unrelinkable
            };
            entries.push(RelinkEntry {
                input_id: issue.input_id,
                asset_uri,
                reason: issue.reason,
                resolution,
            });
        }
        Ok(RelinkPlan {
            entries,
            search_truncated: truncated,
        })
    }

    /// Applies `plan` to `project`, copying imported files to their original
    /// keys and rewriting in-bundle URIs. The returned project is not saved.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::StalePlan`] when an entry no longer matches the
    /// project or its source file changed size, [`BundleError::Collision`]
    /// when the original key is now occupied by different contents, and
    /// validation or I/O errors. Files copied before a failure stay in place.
    #[allow(clippy::too_many_lines)] // one validation and copy step per entry
    pub fn apply_media_relink(
        &self,
        project: &StoredProject,
        plan: &RelinkPlan,
    ) -> Result<RelinkOutcome, BundleError> {
        let mut domain = project.project().clone();
        let mut relinked = Vec::new();
        let mut unresolved = Vec::new();
        let mut imported_bytes = 0;
        for entry in &plan.entries {
            if media_uri(project, entry.input_id) != Some(entry.asset_uri.as_str()) {
                return Err(BundleError::StalePlan {
                    input: entry.input_id,
                });
            }
            match &entry.resolution {
                RelinkResolution::RewriteUri { asset_uri } => {
                    domain
                        .relink_media_input(entry.input_id, asset_uri.clone())
                        .map_err(|_| BundleError::StalePlan {
                            input: entry.input_id,
                        })?;
                    relinked.push(entry.input_id);
                }
                RelinkResolution::Import { source, bytes } => {
                    if fs::metadata(source).map(|metadata| metadata.len()).ok() != Some(*bytes) {
                        return Err(BundleError::StalePlan {
                            input: entry.input_id,
                        });
                    }
                    let key = asset_uri_key(&entry.asset_uri)
                        .map_err(|_| BundleError::StalePlan {
                            input: entry.input_id,
                        })?
                        .to_owned();
                    let destination = fsutil::create_parents_within(&self.assets_root(), &key)?;
                    match fsutil::copy_new_file(source, &destination) {
                        Ok(copied) => imported_bytes += copied,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            if !fs::metadata(&destination).is_ok_and(|metadata| metadata.is_file())
                                || !fsutil::same_contents(source, &destination)?
                            {
                                return Err(BundleError::Collision { key });
                            }
                        }
                        Err(error) => return Err(error.into()),
                    }
                    relinked.push(entry.input_id);
                }
                RelinkResolution::ImportFolder { source, bytes } => {
                    let pictures = folder_pictures(source)?;
                    if pictures.iter().map(|(_, length)| length).sum::<u64>() != *bytes {
                        return Err(BundleError::StalePlan {
                            input: entry.input_id,
                        });
                    }
                    let key = asset_uri_key(&entry.asset_uri)
                        .map_err(|_| BundleError::StalePlan {
                            input: entry.input_id,
                        })?
                        .to_owned();
                    let parent = fsutil::create_parents_within(&self.assets_root(), &key)?
                        .parent()
                        .map_or_else(|| self.assets_root(), Path::to_path_buf);
                    let destination = parent.join(key.rsplit('/').next().unwrap_or_default());
                    fs::create_dir_all(&destination)?;
                    if !fs::metadata(&destination).is_ok_and(|metadata| metadata.is_dir()) {
                        return Err(BundleError::Collision { key });
                    }
                    for (name, _) in &pictures {
                        let target = destination.join(name);
                        match fsutil::copy_new_file(&source.join(name), &target) {
                            Ok(copied) => imported_bytes += copied,
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                                if !fsutil::same_contents(&source.join(name), &target)? {
                                    return Err(BundleError::Collision { key });
                                }
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    relinked.push(entry.input_id);
                }
                RelinkResolution::Ambiguous { .. }
                | RelinkResolution::NotFound
                | RelinkResolution::Unrelinkable => unresolved.push(entry.input_id),
            }
        }
        let project = project.with_project(domain)?;
        for input in &relinked {
            let uri = media_uri(&project, *input).unwrap_or_default();
            let resolved = if media_is_folder(&project, *input) {
                self.resolve_asset_folder(uri)
            } else {
                self.resolve_asset_uri(uri)
            };
            if resolved.is_err() {
                return Err(BundleError::StalePlan { input: *input });
            }
        }
        relinked.sort_unstable();
        unresolved.sort_unstable();
        Ok(RelinkOutcome {
            project,
            relinked,
            unresolved,
            imported_bytes,
        })
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    source_rank: usize,
    depth: usize,
    path: PathBuf,
}

pub(crate) fn media_uri(project: &StoredProject, input: InputId) -> Option<&str> {
    project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .and_then(|input| match &input.kind {
            InputKind::Media { asset_uri, .. }
            | InputKind::Document(fm_model::DocumentInput { asset_uri, .. }) => {
                Some(asset_uri.as_str())
            }
            // A folder slideshow's source is a directory asset (IN-009).
            InputKind::Slideshow(config) => match &config.source {
                fm_types::still::SlideshowSource::Folder { asset_uri } => Some(asset_uri.as_str()),
                fm_types::still::SlideshowSource::List(_) => None,
            },
            _ => None,
        })
}

/// Whether `input` names a slideshow folder rather than a media file.
pub(crate) fn media_is_folder(project: &StoredProject, input: InputId) -> bool {
    project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .is_some_and(|input| {
            matches!(
                &input.kind,
                InputKind::Slideshow(config)
                    if matches!(
                        &config.source,
                        fm_types::still::SlideshowSource::Folder { .. }
                    )
            )
        })
}

/// Walks `root` breadth-first in sorted order, collecting files whose names
/// are wanted. Returns whether the entry budget ran out.
fn collect_named(
    root: &Path,
    source_rank: usize,
    max_depth: usize,
    wanted: &BTreeSet<OsString>,
    budget: &mut usize,
    found: &mut BTreeMap<OsString, Vec<Candidate>>,
) -> io::Result<bool> {
    let mut level = vec![root.to_path_buf()];
    for depth in 0..=max_depth {
        let mut next = Vec::new();
        for directory in level {
            let mut children = match fs::read_dir(&directory) {
                Ok(entries) => entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()?,
                Err(error) if depth > 0 => {
                    // An unreadable subdirectory does not abort the search.
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            };
            children.sort();
            for child in children {
                if *budget == 0 {
                    return Ok(true);
                }
                *budget -= 1;
                let Ok(link) = fs::symlink_metadata(&child) else {
                    continue;
                };
                if link.is_dir() {
                    next.push(child);
                    continue;
                }
                let Some(name) = child.file_name() else {
                    continue;
                };
                if wanted.contains(name) && fs::metadata(&child).is_ok_and(|meta| meta.is_file()) {
                    found
                        .entry(name.to_os_string())
                        .or_default()
                        .push(Candidate {
                            source_rank,
                            depth,
                            path: child,
                        });
                }
            }
        }
        level = next;
    }
    Ok(false)
}

/// The picture files directly inside `folder`, as `(name, length)`, sorted.
fn folder_pictures(folder: &Path) -> io::Result<Vec<(OsString, u64)>> {
    let mut pictures = fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(fm_types::still::is_image_file_name)
        })
        .map(|entry| Ok((entry.file_name(), entry.metadata()?.len())))
        .collect::<io::Result<Vec<_>>>()?;
    pictures.sort();
    Ok(pictures)
}

/// Walks `root` breadth-first in sorted order, collecting directories whose
/// names are wanted and that hold at least one picture. Returns whether the
/// entry budget ran out.
fn collect_named_folders(
    root: &Path,
    source_rank: usize,
    max_depth: usize,
    wanted: &BTreeSet<OsString>,
    budget: &mut usize,
    found: &mut BTreeMap<OsString, Vec<Candidate>>,
) -> io::Result<bool> {
    let mut level = vec![root.to_path_buf()];
    for depth in 0..=max_depth {
        let mut next = Vec::new();
        for directory in level {
            let mut children = match fs::read_dir(&directory) {
                Ok(entries) => entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()?,
                Err(error) if depth > 0 => {
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            };
            children.sort();
            for child in children {
                if *budget == 0 {
                    return Ok(true);
                }
                *budget -= 1;
                let Ok(link) = fs::symlink_metadata(&child) else {
                    continue;
                };
                if !link.is_dir() {
                    continue;
                }
                next.push(child.clone());
                let Some(name) = child.file_name() else {
                    continue;
                };
                if wanted.contains(name)
                    && folder_pictures(&child).is_ok_and(|pictures| !pictures.is_empty())
                {
                    found
                        .entry(name.to_os_string())
                        .or_default()
                        .push(Candidate {
                            source_rank,
                            depth,
                            path: child,
                        });
                }
            }
        }
        level = next;
    }
    Ok(false)
}

fn choose_folder(
    candidates: &[Candidate],
    assets_root: Option<&Path>,
) -> Result<RelinkResolution, BundleError> {
    let mut usable = candidates
        .iter()
        .filter(|candidate| {
            candidate.source_rank != 0 || in_bundle_key(candidate, assets_root).is_some()
        })
        .collect::<Vec<_>>();
    usable.sort_by(|left, right| {
        (left.source_rank, left.depth, &left.path).cmp(&(
            right.source_rank,
            right.depth,
            &right.path,
        ))
    });
    let Some(best) = usable.first() else {
        return Ok(RelinkResolution::NotFound);
    };
    let tier = usable
        .iter()
        .take_while(|candidate| {
            candidate.source_rank == best.source_rank && candidate.depth == best.depth
        })
        .collect::<Vec<_>>();
    let best_pictures = folder_pictures(&best.path)?;
    for other in &tier[1..] {
        if folder_pictures(&other.path)? != best_pictures {
            return Ok(RelinkResolution::Ambiguous {
                candidates: tier
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect(),
            });
        }
    }
    if let Some(key) = in_bundle_key(best, assets_root) {
        return Ok(RelinkResolution::RewriteUri {
            asset_uri: asset_uri(&key),
        });
    }
    Ok(RelinkResolution::ImportFolder {
        source: best.path.clone(),
        bytes: best_pictures.iter().map(|(_, bytes)| bytes).sum(),
    })
}

fn choose(
    candidates: &[Candidate],
    assets_root: Option<&Path>,
) -> Result<RelinkResolution, BundleError> {
    let mut usable = candidates
        .iter()
        .filter(|candidate| {
            candidate.source_rank != 0 || in_bundle_key(candidate, assets_root).is_some()
        })
        .collect::<Vec<_>>();
    usable.sort_by(|left, right| {
        (left.source_rank, left.depth, &left.path).cmp(&(
            right.source_rank,
            right.depth,
            &right.path,
        ))
    });
    let Some(best) = usable.first() else {
        return Ok(RelinkResolution::NotFound);
    };
    let tier = usable
        .iter()
        .take_while(|candidate| {
            candidate.source_rank == best.source_rank && candidate.depth == best.depth
        })
        .collect::<Vec<_>>();
    for other in &tier[1..] {
        if !fsutil::same_contents(&best.path, &other.path)? {
            return Ok(RelinkResolution::Ambiguous {
                candidates: tier
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect(),
            });
        }
    }
    if let Some(key) = in_bundle_key(best, assets_root) {
        return Ok(RelinkResolution::RewriteUri {
            asset_uri: asset_uri(&key),
        });
    }
    Ok(RelinkResolution::Import {
        source: best.path.clone(),
        bytes: fs::metadata(&best.path)?.len(),
    })
}

fn in_bundle_key(candidate: &Candidate, assets_root: Option<&Path>) -> Option<String> {
    if candidate.source_rank != 0 {
        return None;
    }
    let assets_root = assets_root?;
    // A symlinked file inside `assets/` is usable only if its target is too;
    // otherwise the rewritten URI would never resolve.
    if !fs::canonicalize(&candidate.path).is_ok_and(|resolved| resolved.starts_with(assets_root)) {
        return None;
    }
    let relative = candidate.path.strip_prefix(assets_root).ok()?;
    let key = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    asset_uri_key(&asset_uri(&key)).ok()?;
    Some(key)
}

pub(crate) fn asset_uri(key: &str) -> String {
    format!("{ASSET_URI_PREFIX}{key}")
}

/// Copies `source` to the first free key in `base`'s suffix family, reusing
/// an identical occupant. `reserved` vetoes keys already claimed elsewhere.
pub(crate) fn place_file(
    assets_root: &Path,
    source: &Path,
    base: &str,
    reserved: impl Fn(&str) -> bool,
) -> Result<(String, AssetDisposition), BundleError> {
    for attempt in 1..=MAX_KEY_SUFFIX {
        let key = suffixed_key(base, attempt);
        if reserved(&key) || asset_uri_key(&asset_uri(&key)).is_err() {
            continue;
        }
        let destination = match fsutil::create_parents_within(assets_root, &key) {
            Ok(destination) => destination,
            // A path component is occupied by a file, or escapes the bundle.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput
                        | io::ErrorKind::AlreadyExists
                        | io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        match fsutil::copy_new_file(source, &destination) {
            Ok(bytes) => return Ok((key, AssetDisposition::Copied { bytes })),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::symlink_metadata(&destination).is_ok_and(|meta| meta.is_file())
                    && fsutil::same_contents(source, &destination)?
                {
                    return Ok((key, AssetDisposition::Reused));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(BundleError::NoAvailableKey {
        key: base.to_owned(),
    })
}

/// `name.ext` for attempt 1, then `name-2.ext`, `name-3.ext`, ...
pub(crate) fn suffixed_key(key: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return key.to_owned();
    }
    let (directory, name) = key
        .rsplit_once('/')
        .map_or(("", key), |(dir, name)| (dir, name));
    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => name.split_at(index),
        _ => (name, ""),
    };
    let name = format!("{stem}-{attempt}{extension}");
    if directory.is_empty() {
        name
    } else {
        format!("{directory}/{name}")
    }
}

/// The `asset://` URI [`ProjectStore::import_media_file`] tries first for
/// host file `source`: its file name made portable. The import itself may
/// settle on a numbered variant when that key holds other contents.
#[must_use]
pub fn media_import_asset_uri(source: &Path) -> String {
    asset_uri(&imported_key(
        source.file_name().unwrap_or_else(|| OsStr::new("media")),
    ))
}

/// Derives a portable single-segment key from a host file name.
pub(crate) fn imported_key(name: &OsStr) -> String {
    let lossy = name.to_string_lossy();
    let segment = portable_segment(&lossy);
    if segment.is_empty() {
        "media".to_owned()
    } else {
        segment
    }
}

/// Rewrites every segment of a valid key so it is a legal file name on
/// Windows, macOS, and Linux and still a valid asset key.
pub(crate) fn portable_key(key: &str) -> String {
    key.split('/')
        .map(|segment| {
            let segment = portable_segment(segment);
            if segment.is_empty() {
                "_".to_owned()
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn portable_segment(segment: &str) -> String {
    let mut output: String = segment
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '#' | '%' | '\u{FFFD}'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    if output.len() > MAX_GENERATED_SEGMENT_BYTES {
        let mut end = MAX_GENERATED_SEGMENT_BYTES;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
    }
    // Windows strips trailing dots and spaces; this also turns `.` and `..`
    // into ordinary names.
    if output.ends_with(['.', ' ']) {
        output.pop();
        output.push('_');
    }
    let stem = output.split('.').next().unwrap_or_default();
    if WINDOWS_RESERVED_NAMES.contains(&stem.to_ascii_lowercase().as_str()) {
        output.insert(0, '_');
    }
    output
}

/// Case-folded key space used to detect keys that would collide on a
/// case-insensitive filesystem, including file/directory prefix clashes.
#[derive(Default)]
pub(crate) struct FoldedKeys {
    files: BTreeSet<String>,
    directories: BTreeSet<String>,
}

impl FoldedKeys {
    pub(crate) fn conflicts(&self, key: &str) -> bool {
        let folded = key.to_lowercase();
        self.files.contains(&folded)
            || self.directories.contains(&folded)
            || prefixes(&folded).any(|prefix| self.files.contains(prefix))
    }

    pub(crate) fn insert(&mut self, key: &str) {
        let folded = key.to_lowercase();
        for prefix in prefixes(&folded) {
            self.directories.insert(prefix.to_owned());
        }
        self.files.insert(folded);
    }
}

fn prefixes(key: &str) -> impl Iterator<Item = &str> {
    key.match_indices('/').map(|(index, _)| &key[..index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_keep_directory_and_extension() {
        assert_eq!(suffixed_key("clip.mov", 1), "clip.mov");
        assert_eq!(suffixed_key("clip.mov", 2), "clip-2.mov");
        assert_eq!(suffixed_key("a.b/clip", 3), "a.b/clip-3");
        assert_eq!(suffixed_key("dir/.hidden", 2), "dir/.hidden-2");
        assert_eq!(suffixed_key("x/archive.tar.gz", 2), "x/archive.tar-2.gz");
    }

    #[test]
    fn portable_keys_avoid_windows_hazards() {
        assert_eq!(portable_key("logos/CON.png"), "logos/_CON.png");
        assert_eq!(portable_key("a<b>|c*.png"), "a_b__c_.png");
        assert_eq!(portable_key("trailing./dot "), "trailing_/dot_");
        assert_eq!(portable_key("plain/clip.mov"), "plain/clip.mov");
        assert_eq!(portable_key("lpt9"), "_lpt9");
        assert_eq!(portable_key("lpt10"), "lpt10");
    }

    #[test]
    fn imported_names_are_valid_keys() {
        for name in ["..", ".", "", "a:b?c#d%e", "\u{FFFD}x", "clip.mov"] {
            let key = imported_key(OsStr::new(name));
            assert!(
                asset_uri_key(&asset_uri(&key)).is_ok(),
                "{name:?} -> {key:?}"
            );
        }
        let long = "x".repeat(400);
        assert_eq!(
            imported_key(OsStr::new(&long)).len(),
            MAX_GENERATED_SEGMENT_BYTES
        );
    }

    #[test]
    fn folded_keys_detect_case_and_prefix_clashes() {
        let mut keys = FoldedKeys::default();
        keys.insert("Media/Logo.png");
        assert!(keys.conflicts("media/logo.PNG"));
        assert!(keys.conflicts("MEDIA"));
        assert!(!keys.conflicts("media/other.png"));
        keys.insert("clip");
        assert!(keys.conflicts("Clip/inner.mov"));
    }
}
