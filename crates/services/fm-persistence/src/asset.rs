use std::{error::Error, fmt, fs, io, path::PathBuf};

use fm_model::{InputKind, Project};
use fm_types::InputId;

use crate::{ProjectStore, StoredProject};

const ASSETS_DIRECTORY: &str = "assets";
pub(crate) const ASSET_URI_PREFIX: &str = "asset://";
pub(crate) const MAX_ASSET_URI_BYTES: usize = 1024;

impl ProjectStore {
    /// Returns the assets directory beneath the project bundle root.
    #[must_use]
    pub fn assets_root(&self) -> PathBuf {
        self.root().join(ASSETS_DIRECTORY)
    }

    /// Resolves an `asset://` URI to a canonical regular file within the project.
    ///
    /// Resolution does not create the project or assets directories. Asset keys
    /// use `/` separators on every platform and do not support percent decoding.
    ///
    /// # Errors
    ///
    /// Returns an error if the URI is invalid, either path cannot be resolved,
    /// the resolved target escapes the assets directory, or the target is not a
    /// regular file.
    pub fn resolve_asset_uri(&self, uri: &str) -> Result<PathBuf, AssetResolveError> {
        self.resolve_asset_path(uri, false)
    }

    /// Resolves an `asset://` URI to a canonical directory within the project
    /// (a folder slideshow's source).
    ///
    /// # Errors
    ///
    /// Returns an error if the URI is invalid, either path cannot be resolved,
    /// the resolved target escapes the assets directory, or the target is not a
    /// directory.
    pub fn resolve_asset_folder(&self, uri: &str) -> Result<PathBuf, AssetResolveError> {
        self.resolve_asset_path(uri, true)
    }

    fn resolve_asset_path(&self, uri: &str, directory: bool) -> Result<PathBuf, AssetResolveError> {
        let key = asset_uri_key(uri)?;
        let assets_root = fs::canonicalize(self.assets_root())
            .map_err(AssetResolveError::AssetsRootUnavailable)?;
        if !fs::metadata(&assets_root)
            .map_err(AssetResolveError::AssetsRootUnavailable)?
            .is_dir()
        {
            return Err(AssetResolveError::AssetsRootUnavailable(io::Error::new(
                io::ErrorKind::NotADirectory,
                "assets root is not a directory",
            )));
        }

        let candidate =
            fs::canonicalize(assets_root.join(key)).map_err(AssetResolveError::AssetUnavailable)?;
        if !candidate.starts_with(&assets_root) {
            return Err(AssetResolveError::OutsideAssetsRoot);
        }
        let metadata = fs::metadata(&candidate).map_err(AssetResolveError::AssetUnavailable)?;
        let matches = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !matches {
            return Err(AssetResolveError::NotRegularFile);
        }

        Ok(candidate)
    }

    /// The pictures every folder slideshow of `project` shows (IN-009); see
    /// [`Self::folder_picture_uris`]. Sorted and distinct.
    pub(crate) fn folder_slideshow_picture_uris(
        &self,
        project: &Project,
    ) -> std::collections::BTreeSet<String> {
        folder_slideshow_uris(project)
            .iter()
            .flat_map(|folder| self.folder_picture_uris(folder))
            .collect()
    }

    /// The pictures a folder slideshow showing `folder` (`asset://<key>`)
    /// shows: each regular file with a picture extension directly inside
    /// the folder, as the daemon lists it, that resolves within the assets.
    /// Sorted; a missing folder, or one that leads out of the assets, has
    /// none.
    pub(crate) fn folder_picture_uris(&self, folder: &str) -> Vec<String> {
        let Ok(key) = asset_uri_key(folder) else {
            return Vec::new();
        };
        let (Ok(root), Ok(directory)) = (
            fs::canonicalize(self.assets_root()),
            fs::canonicalize(self.assets_root().join(key)),
        ) else {
            return Vec::new();
        };
        if !directory.starts_with(&root) {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut pictures = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| fm_types::still::is_image_file_name(name))
            .map(|name| format!("{folder}/{name}"))
            .filter(|uri| self.resolve_asset_uri(uri).is_ok())
            .collect::<Vec<_>>();
        pictures.sort();
        pictures
    }

    /// Reports media inputs whose project asset cannot be resolved, and title
    /// inputs whose template, template fonts and image defaults, or image
    /// field values cannot be resolved (one issue per unresolved URI; a
    /// template that resolves but does not decode is
    /// [`AssetAuditReason::UnreadableTitleTemplate`]). Issues are sorted by
    /// input id, stably.
    #[must_use]
    pub fn audit_assets(&self, project: &StoredProject) -> Vec<AssetAuditIssue> {
        self.audit_assets_of(project.project())
    }

    pub(crate) fn audit_assets_of(&self, project: &Project) -> Vec<AssetAuditIssue> {
        let mut issues = project
            .inputs()
            .iter()
            .filter_map(|input| {
                let (asset_uri, folder) = match &input.kind {
                    InputKind::Media { asset_uri, .. }
                    | InputKind::Document(fm_model::DocumentInput { asset_uri, .. }) => {
                        (asset_uri, false)
                    }
                    // A folder slideshow's source is a directory asset
                    // (IN-009).
                    InputKind::Slideshow(config) => match &config.source {
                        fm_types::still::SlideshowSource::Folder { asset_uri } => (asset_uri, true),
                        fm_types::still::SlideshowSource::List(_) => return None,
                    },
                    _ => return None,
                };
                let resolved = if folder {
                    self.resolve_asset_folder(asset_uri)
                } else {
                    self.resolve_asset_uri(asset_uri)
                };
                let reason = match resolved {
                    Ok(_) => return None,
                    Err(AssetResolveError::InvalidUri) => AssetAuditReason::InvalidUri,
                    Err(
                        AssetResolveError::AssetsRootUnavailable(_)
                        | AssetResolveError::AssetUnavailable(_),
                    ) => AssetAuditReason::MissingAsset,
                    Err(AssetResolveError::OutsideAssetsRoot) => {
                        AssetAuditReason::OutsideAssetsRoot
                    }
                    Err(AssetResolveError::NotRegularFile) => AssetAuditReason::NotRegularFile,
                };
                Some(AssetAuditIssue {
                    input_id: input.id,
                    reason,
                })
            })
            .collect::<Vec<_>>();
        issues.extend(
            self.audit_title_assets(project)
                .into_iter()
                .map(|(input_id, reason)| AssetAuditIssue { input_id, reason }),
        );
        issues.sort_by_key(|issue| issue.input_id);
        issues
    }
}

/// A project input with an unresolved media or title asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetAuditIssue {
    pub input_id: InputId,
    pub reason: AssetAuditReason,
}

/// A stable reason that a project media or title asset cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetAuditReason {
    InvalidUri,
    MissingAsset,
    OutsideAssetsRoot,
    NotRegularFile,
    /// A title template resolves but is not a valid stored template.
    UnreadableTitleTemplate,
}

/// The distinct asset folders `project`'s folder slideshows show, sorted.
pub(crate) fn folder_slideshow_uris(project: &Project) -> std::collections::BTreeSet<String> {
    project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::Slideshow(config) => match &config.source {
                fm_types::still::SlideshowSource::Folder { asset_uri } => Some(asset_uri.clone()),
                fm_types::still::SlideshowSource::List(_) => None,
            },
            _ => None,
        })
        .collect()
}

/// Validates the strict syntax of a project `asset://` URI without filesystem access.
///
/// # Errors
///
/// Returns [`AssetResolveError::InvalidUri`] when `uri` does not use the
/// supported strict `asset://` syntax.
pub fn validate_asset_uri(uri: &str) -> Result<(), AssetResolveError> {
    asset_uri_key(uri).map(|_| ())
}

pub(crate) fn asset_uri_key(uri: &str) -> Result<&str, AssetResolveError> {
    if uri.len() > MAX_ASSET_URI_BYTES {
        return Err(AssetResolveError::InvalidUri);
    }
    let Some(key) = uri.strip_prefix(ASSET_URI_PREFIX) else {
        return Err(AssetResolveError::InvalidUri);
    };
    if key.is_empty()
        || key.starts_with('/')
        || key.contains(['\\', '\0', '?', '#', '%', ':'])
        || key.chars().any(char::is_control)
        || key
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(AssetResolveError::InvalidUri);
    }

    Ok(key)
}

/// A failure to resolve a project asset URI.
#[derive(Debug)]
pub enum AssetResolveError {
    /// The URI does not use the supported strict `asset://` syntax.
    InvalidUri,
    /// The project assets directory could not be resolved as a directory.
    AssetsRootUnavailable(io::Error),
    /// The requested asset could not be resolved or inspected.
    AssetUnavailable(io::Error),
    /// The requested asset resolves outside the project assets directory.
    OutsideAssetsRoot,
    /// The requested asset exists but is not a regular file.
    NotRegularFile,
}

impl fmt::Display for AssetResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUri => "invalid project asset URI",
            Self::AssetsRootUnavailable(_) => "project assets root is unavailable",
            Self::AssetUnavailable(_) => "project asset is unavailable",
            Self::OutsideAssetsRoot => "project asset resolves outside the assets root",
            Self::NotRegularFile => "project asset is not a regular file",
        })
    }
}

impl Error for AssetResolveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AssetsRootUnavailable(error) | Self::AssetUnavailable(error) => Some(error),
            Self::InvalidUri | Self::OutsideAssetsRoot | Self::NotRegularFile => None,
        }
    }
}
