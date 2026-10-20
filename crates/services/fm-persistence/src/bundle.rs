//! Self-contained portable bundles and projects instantiated from templates.
//!
//! Both operations copy a source bundle's durable manifest and exactly the
//! assets its media and title inputs reference (title templates, their fonts
//! and image defaults, and title image values) plus every imported grading
//! LUT into a brand-new destination bundle. Title and LUT asset keys are
//! never rewritten: a title key that would need a
//! portable rename refuses the export with
//! [`BundleError::TitleAssetRewriteUnsupported`].
//! The destination is assembled in a hidden staging sibling and published with
//! one directory rename, so a failure never leaves a partial bundle at the
//! destination, and an existing destination is never overwritten.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use fm_types::{InputId, ProjectId};

use crate::{
    BundleError, ProjectStore, StoredProject,
    asset::asset_uri_key,
    fsutil,
    media::{FoldedKeys, asset_uri, portable_key, suffixed_key},
    store::{next_temp_sequence, parent_directory, sync_directory},
    title_template::template_issue,
};

const MAX_PORTABLE_SUFFIX: u32 = 10_000;

/// One asset copied into a new bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundledAsset {
    /// URI in the source project.
    pub source_uri: String,
    /// URI in the new bundle; differs when the key had to be made portable.
    pub bundled_uri: String,
    pub bytes: u64,
}

/// The project written to a new bundle and how its media got there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleExport {
    pub project: StoredProject,
    /// Distinct assets copied, sorted by source URI.
    pub assets: Vec<BundledAsset>,
    /// Media inputs whose URI was rewritten, sorted.
    pub rewritten_inputs: Vec<InputId>,
}

impl ProjectStore {
    /// Writes a self-contained copy of this bundle to `destination`.
    ///
    /// Only `project.json` and the referenced assets are copied: the journal,
    /// autosaves, and unreferenced files are not. Each distinct asset is
    /// copied once, symlinked assets are materialized as regular files, and
    /// keys are rewritten when needed so they are legal and distinct on
    /// case-insensitive and Windows filesystems (`Logo.png` and `logo.png`
    /// become `Logo.png` and `logo-2.png`). Runtime state and history are kept.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::DestinationExists`],
    /// [`BundleError::DestinationInsideSource`],
    /// [`BundleError::UnappliedJournal`], [`BundleError::UnresolvedAssets`]
    /// (including assets that resolve outside `assets/`), or store, validation,
    /// and I/O errors. Nothing is left at `destination` on error.
    pub fn export_portable_bundle(
        &self,
        destination: &ProjectStore,
    ) -> Result<BundleExport, BundleError> {
        export_bundle(self, destination, Ok)
    }

    /// Creates a new project bundle at `destination` from this template
    /// bundle.
    ///
    /// The new project gets `project_id` and `name`; its durable position is
    /// reset to the origin and idempotency receipts are dropped, because the
    /// template's command history does not belong to the new project.
    /// Project-local input, scene, bus, and output ids are kept. Media is
    /// copied exactly as for [`ProjectStore::export_portable_bundle`].
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::TemplateIdentityReused`] when `project_id`
    /// equals the template's id, [`BundleError::EmptyName`] for a blank name,
    /// and every error of [`ProjectStore::export_portable_bundle`].
    pub fn instantiate_template(
        &self,
        destination: &ProjectStore,
        project_id: ProjectId,
        name: &str,
    ) -> Result<BundleExport, BundleError> {
        export_bundle(self, destination, |stored| {
            if stored.project().id() == project_id {
                return Err(BundleError::TemplateIdentityReused);
            }
            let mut project = stored.project().clone().with_id(project_id);
            project
                .rename_project(name.to_owned())
                .map_err(|_| BundleError::EmptyName)?;
            Ok(stored.without_history().with_project(project)?)
        })
    }
}

fn export_bundle(
    source: &ProjectStore,
    destination: &ProjectStore,
    transform: impl FnOnce(StoredProject) -> Result<StoredProject, BundleError>,
) -> Result<BundleExport, BundleError> {
    let stored = source.load()?;
    let journal = source.scan_journal()?;
    if !journal.batches().is_empty() {
        return Err(BundleError::UnappliedJournal {
            batches: journal.batches().len(),
        });
    }
    let stored = transform(stored)?;
    let issues = source.audit_assets(&stored);
    if !issues.is_empty() {
        return Err(BundleError::UnresolvedAssets(issues));
    }

    let target = destination.root();
    if fs::symlink_metadata(target).is_ok() {
        return Err(BundleError::DestinationExists(target.to_path_buf()));
    }
    let parent = parent_directory(target);
    fs::create_dir_all(parent)?;
    if fs::canonicalize(parent)?.starts_with(fs::canonicalize(source.root())?) {
        return Err(BundleError::DestinationInsideSource(target.to_path_buf()));
    }

    let title_uris = source
        .title_asset_uris(stored.project())
        .map_err(|(input, error)| {
            BundleError::UnresolvedAssets(vec![template_issue(input, &error)])
        })?;
    // Imported LUTs are named by id (`luts/<id>.cube`), so their keys are
    // already portable and, like title keys, are never rewritten.
    let lut_uris = stored
        .project()
        .luts()
        .iter()
        .map(fm_model::ProjectLut::asset_uri)
        .collect::<BTreeSet<_>>();
    if let Some(uri) = lut_uris
        .iter()
        .find(|uri| source.resolve_asset_uri(uri).is_err())
    {
        return Err(BundleError::UnresolvedLut { uri: uri.clone() });
    }
    let fixed_uris = title_uris
        .iter()
        .chain(&lut_uris)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mapping = portable_mapping(&stored, &fixed_uris)?;
    if let Some((uri, _)) = mapping
        .iter()
        .find(|(uri, bundled)| fixed_uris.contains(*uri) && uri != bundled)
    {
        return Err(BundleError::TitleAssetRewriteUnsupported { uri: uri.clone() });
    }
    let mut project = stored.project().clone();
    let mut rewritten_inputs = Vec::new();
    for input in stored.project().inputs() {
        // A media file, an image, and an explicit slideshow list's
        // pictures all move to their portable keys.
        if project.map_input_file_assets(input.id, |uri| mapping.get(uri).cloned()) {
            rewritten_inputs.push(input.id);
        }
    }
    let project = stored.with_project(project)?;
    rewritten_inputs.sort_unstable();

    let staging = StagingDirectory::create(target)?;
    let staging_store = ProjectStore::new(staging.path.clone())?;
    let mut assets = Vec::with_capacity(mapping.len());
    for (source_uri, bundled_uri) in &mapping {
        let resolved = source
            .resolve_asset_uri(source_uri)
            .map_err(|_| BundleError::UnresolvedAssets(source.audit_assets(&stored)))?;
        let key = asset_uri_key(bundled_uri).map_err(|_| BundleError::NoAvailableKey {
            key: bundled_uri.clone(),
        })?;
        let destination = fsutil::create_parents_within(&staging_store.assets_root(), key)?;
        let bytes = fsutil::copy_new_file(&resolved, &destination)?;
        assets.push(BundledAsset {
            source_uri: source_uri.clone(),
            bundled_uri: bundled_uri.clone(),
            bytes,
        });
    }
    staging_store.save(&project)?;
    staging.publish(target)?;
    Ok(BundleExport {
        project,
        assets,
        rewritten_inputs,
    })
}

/// Maps each distinct referenced URI to its portable URI. Title keys cannot
/// be rewritten, so they are claimed first; then each group is processed in
/// sorted order, so the first of a case-folded clash keeps its name.
fn portable_mapping(
    stored: &StoredProject,
    title_uris: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, BundleError> {
    let mut uris = title_uris
        .iter()
        .map(|uri| (uri.clone(), String::new()))
        .collect::<BTreeMap<_, _>>();
    for input in stored.project().inputs() {
        for uri in input.kind.file_asset_uris() {
            uris.insert(uri.to_owned(), String::new());
        }
    }
    let (titles, media): (Vec<_>, Vec<_>) = uris
        .iter_mut()
        .partition(|(uri, _)| title_uris.contains(*uri));
    let mut taken = FoldedKeys::default();
    for (uri, bundled) in titles.into_iter().chain(media) {
        let key =
            asset_uri_key(uri).map_err(|_| BundleError::NoAvailableKey { key: uri.clone() })?;
        let base = portable_key(key);
        let chosen = (1..=MAX_PORTABLE_SUFFIX)
            .map(|attempt| suffixed_key(&base, attempt))
            .find(|candidate| {
                !taken.conflicts(candidate) && asset_uri_key(&asset_uri(candidate)).is_ok()
            })
            .ok_or_else(|| BundleError::NoAvailableKey { key: base.clone() })?;
        taken.insert(&chosen);
        *bundled = asset_uri(&chosen);
    }
    Ok(uris)
}

/// A hidden sibling directory removed on drop unless published.
struct StagingDirectory {
    path: PathBuf,
    published: bool,
}

impl StagingDirectory {
    fn new(target: &Path) -> Self {
        let name = target.file_name().map_or_else(
            || "bundle".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        // The `.freemix` extension lets the staging path act as a store root.
        let path = parent_directory(target).join(format!(
            ".{name}.staging-{}-{}.freemix",
            std::process::id(),
            next_temp_sequence()
        ));
        Self {
            path,
            published: false,
        }
    }

    /// Creates a fresh staging directory. A leftover directory of the same
    /// name (from an earlier process with this process id) is skipped, never
    /// adopted or removed.
    fn create(target: &Path) -> std::io::Result<Self> {
        const MAX_ATTEMPTS: u32 = 64;
        for _ in 0..MAX_ATTEMPTS {
            let mut staging = Self::new(target);
            match fs::create_dir(&staging.path) {
                Ok(()) => return Ok(staging),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Not ours: disarm cleanup before dropping.
                    staging.published = true;
                }
                Err(error) => {
                    staging.published = true;
                    return Err(error);
                }
            }
        }
        Err(std::io::Error::other("no free staging directory name"))
    }

    fn publish(mut self, target: &Path) -> Result<(), BundleError> {
        // `rename` would replace an empty directory created meanwhile, so the
        // never-overwrite guarantee is rechecked immediately before it.
        if fs::symlink_metadata(target).is_ok() {
            return Err(BundleError::DestinationExists(target.to_path_buf()));
        }
        fs::rename(&self.path, target)?;
        self.published = true;
        sync_directory(parent_directory(target))?;
        Ok(())
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_skips_and_keeps_a_leftover_directory_with_the_same_name() {
        let parent = std::env::temp_dir().join(format!(
            "fm-persistence-staging-{}-{}",
            std::process::id(),
            next_temp_sequence()
        ));
        fs::create_dir(&parent).unwrap();
        let target = parent.join("show.freemix");
        // Leftovers from an earlier process that had this process id.
        let next = next_temp_sequence() + 1;
        let leftovers = (next..next + 32)
            .map(|sequence| {
                let leftover = parent.join(format!(
                    ".show.freemix.staging-{}-{sequence}.freemix",
                    std::process::id()
                ));
                fs::create_dir(&leftover).unwrap();
                fs::write(leftover.join("marker"), b"keep").unwrap();
                leftover
            })
            .collect::<Vec<_>>();

        let staging = StagingDirectory::create(&target).unwrap();
        assert!(staging.path.is_dir());
        assert!(!leftovers.contains(&staging.path));
        drop(staging);
        assert!(
            leftovers
                .iter()
                .all(|leftover| leftover.join("marker").exists())
        );
        fs::remove_dir_all(parent).unwrap();
    }
}
