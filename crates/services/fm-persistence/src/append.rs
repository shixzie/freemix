//! Appending another project's inputs and scenes into a bundle.
//!
//! Every input and scene of the source project is appended under fresh ids
//! allocated above the target's highest id, in source order. Scene layers,
//! scene-input links, and scene audio sources are remapped to the fresh ids,
//! and per-input audio strips travel with their inputs. Name clashes are
//! resolved with ` (2)`, ` (3)`, ... suffixes and reported. Referenced media
//! is copied into the target bundle's assets, reusing identical files and
//! renaming keys that would clash with different contents.
//!
//! Outputs, audio buses, stream targets, stingers, the main mix, recording
//! flags, and runtime state are not appended; the report counts what was
//! left behind so a caller can tell the operator.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    num::NonZeroU128,
};

use fm_model::{InputKind, Project, Scene, SourceRef};
use fm_types::{InputId, MAX_INPUT_NAME_BYTES, SceneId};

use crate::{
    AssetDisposition, BundleError, ProjectStore, StoredProject,
    asset::asset_uri_key,
    media::{asset_uri, place_file, suffixed_key},
};

const MAX_NAME_SUFFIX: u32 = 10_000;

/// An appended entity whose name was changed to avoid a clash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenamedEntity<Id> {
    /// Id in the source project.
    pub source_id: Id,
    pub from: String,
    pub to: String,
}

/// How one referenced source asset was carried into the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendedAssetDisposition {
    /// Copied to, or reused at, `target_uri`.
    Placed(AssetDisposition),
    /// The source asset did not resolve; the URI was kept (or renamed when the
    /// target already used the key) and remains unresolved in the target.
    MissingInSource,
}

/// One distinct source media URI and where it went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendedAsset {
    pub source_uri: String,
    pub target_uri: String,
    pub disposition: AppendedAssetDisposition,
}

/// Source content that append does not carry over.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SkippedContent {
    pub outputs: usize,
    pub audio_buses: usize,
    pub stream_targets: usize,
    pub stingers: usize,
    pub main_mix: bool,
}

/// Everything an append changed or declined to change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendReport {
    /// Source input id to new target id, in source order.
    pub inputs: Vec<(InputId, InputId)>,
    /// Source scene id to new target id, in source order.
    pub scenes: Vec<(SceneId, SceneId)>,
    pub renamed_inputs: Vec<RenamedEntity<InputId>>,
    pub renamed_scenes: Vec<RenamedEntity<SceneId>>,
    /// Distinct source media URIs, sorted.
    pub assets: Vec<AppendedAsset>,
    pub skipped: SkippedContent,
}

impl ProjectStore {
    /// Appends `source`'s inputs and scenes to `target`, a project stored in
    /// this bundle, and copies their media into this bundle's assets.
    ///
    /// The returned project is validated but not saved; target runtime state
    /// and position are unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::UnappliedJournal`] when the source journal holds
    /// unapplied batches, [`BundleError::IdSpaceExhausted`],
    /// [`BundleError::NoAvailableKey`], and store, validation, or I/O errors.
    /// Media copied before a later failure stays in the assets directory.
    pub fn append_project(
        &self,
        target: &StoredProject,
        source: &ProjectStore,
    ) -> Result<(StoredProject, AppendReport), BundleError> {
        let journal = source.scan_journal()?;
        if !journal.batches().is_empty() {
            return Err(BundleError::UnappliedJournal {
                batches: journal.batches().len(),
            });
        }
        let incoming = source.load()?;
        let incoming = incoming.project();
        let mut project = target.project().clone();

        let mut next_input = next_id(project.inputs().iter().map(|input| input.id.get()));
        let mut next_scene = next_id(project.scenes().iter().map(|scene| scene.id.get()));
        let mut input_map = BTreeMap::new();
        let mut scene_map = BTreeMap::new();
        let mut report = AppendReport {
            inputs: Vec::new(),
            scenes: Vec::new(),
            renamed_inputs: Vec::new(),
            renamed_scenes: Vec::new(),
            assets: Vec::new(),
            skipped: skipped(incoming),
        };
        for input in incoming.inputs() {
            let id = InputId::new(allocate(&mut next_input)?);
            input_map.insert(input.id, id);
            report.inputs.push((input.id, id));
        }
        for scene in incoming.scenes() {
            let id = SceneId::new(allocate(&mut next_scene)?);
            scene_map.insert(scene.id, id);
            report.scenes.push((scene.id, id));
        }

        let assets = self.append_assets(incoming, source)?;
        let uri_map = assets
            .iter()
            .map(|asset| (asset.source_uri.clone(), asset.target_uri.clone()))
            .collect::<BTreeMap<_, _>>();
        report.assets = assets;

        append_scenes(&mut project, incoming, &input_map, &scene_map, &mut report)?;
        append_inputs(
            &mut project,
            incoming,
            &IdMaps {
                inputs: &input_map,
                scenes: &scene_map,
                uris: &uri_map,
            },
            &mut report,
        )?;

        let appended = target.with_project(project)?;
        Ok((appended, report))
    }

    fn append_assets(
        &self,
        incoming: &Project,
        source: &ProjectStore,
    ) -> Result<Vec<AppendedAsset>, BundleError> {
        let uris = incoming
            .inputs()
            .iter()
            .filter_map(|input| match &input.kind {
                InputKind::Media { asset_uri } => Some(asset_uri.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let assets_root = self.assets_root();
        let mut assets = Vec::with_capacity(uris.len());
        for uri in uris {
            let Ok(key) = asset_uri_key(&uri) else {
                // A syntactically invalid URI can never resolve in any
                // bundle, so it is kept verbatim and reported as missing.
                assets.push(AppendedAsset {
                    target_uri: uri.clone(),
                    source_uri: uri,
                    disposition: AppendedAssetDisposition::MissingInSource,
                });
                continue;
            };
            let (target_uri, disposition) = if let Ok(resolved) = source.resolve_asset_uri(&uri) {
                let (key, disposition) = place_file(&assets_root, &resolved, key, |_| false)?;
                (
                    asset_uri(&key),
                    AppendedAssetDisposition::Placed(disposition),
                )
            } else {
                // Keep the key only if nothing in the target occupies it,
                // so a missing source asset never silently binds to an
                // unrelated target file.
                let key = (1..=MAX_NAME_SUFFIX)
                    .map(|attempt| suffixed_key(key, attempt))
                    .find(|candidate| {
                        asset_uri_key(&asset_uri(candidate)).is_ok()
                            && fs::symlink_metadata(assets_root.join(candidate)).is_err()
                    })
                    .ok_or_else(|| BundleError::NoAvailableKey {
                        key: key.to_owned(),
                    })?;
                (asset_uri(&key), AppendedAssetDisposition::MissingInSource)
            };
            assets.push(AppendedAsset {
                source_uri: uri,
                target_uri,
                disposition,
            });
        }
        Ok(assets)
    }
}

struct IdMaps<'a> {
    inputs: &'a BTreeMap<InputId, InputId>,
    scenes: &'a BTreeMap<SceneId, SceneId>,
    uris: &'a BTreeMap<String, String>,
}

fn append_scenes(
    project: &mut Project,
    incoming: &Project,
    input_map: &BTreeMap<InputId, InputId>,
    scene_map: &BTreeMap<SceneId, SceneId>,
    report: &mut AppendReport,
) -> Result<(), BundleError> {
    let mut scene_names = project
        .scenes()
        .iter()
        .map(|scene| scene.name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for scene in incoming.scenes() {
        let name = unique_name(&scene.name, usize::MAX, |candidate| {
            scene_names.contains(&candidate.to_ascii_lowercase())
        })?;
        scene_names.insert(name.to_ascii_lowercase());
        if name != scene.name {
            report.renamed_scenes.push(RenamedEntity {
                source_id: scene.id,
                from: scene.name.clone(),
                to: name.clone(),
            });
        }
        let mut layers = scene.layers.clone();
        for layer in &mut layers {
            layer.source = match layer.source {
                SourceRef::Input(id) => SourceRef::Input(input_map[&id]),
                SourceRef::Scene(id) => SourceRef::Scene(scene_map[&id]),
            };
        }
        project.add_scene(Scene {
            id: scene_map[&scene.id],
            name,
            background: scene.background,
            layers,
        });
    }

    Ok(())
}

fn append_inputs(
    project: &mut Project,
    incoming: &Project,
    maps: &IdMaps<'_>,
    report: &mut AppendReport,
) -> Result<(), BundleError> {
    let mut input_names = project
        .inputs()
        .iter()
        .map(|input| input.name.clone())
        .collect::<BTreeSet<_>>();
    for input in incoming.inputs() {
        let name = unique_name(&input.name, MAX_INPUT_NAME_BYTES, |candidate| {
            input_names.contains(candidate)
        })?;
        input_names.insert(name.clone());
        if name != input.name {
            report.renamed_inputs.push(RenamedEntity {
                source_id: input.id,
                from: input.name.clone(),
                to: name.clone(),
            });
        }
        let kind = match &input.kind {
            InputKind::Media { asset_uri: uri } => InputKind::Media {
                asset_uri: maps.uris.get(uri).cloned().unwrap_or_else(|| uri.clone()),
            },
            InputKind::Scene {
                scene_id,
                audio_source,
            } => InputKind::Scene {
                scene_id: maps.scenes[scene_id],
                audio_source: audio_source.map(|id| maps.inputs[&id]),
            },
            other => other.clone(),
        };
        let id = maps.inputs[&input.id];
        project.add_input(fm_model::Input {
            id,
            name,
            kind,
            required_capabilities: input.required_capabilities.clone(),
        });
        if let Some(strip) = incoming.input_audio_strip(input.id) {
            project.set_input_audio_strip(id, strip);
        }
    }

    Ok(())
}

fn skipped(project: &Project) -> SkippedContent {
    SkippedContent {
        outputs: project.outputs().len(),
        audio_buses: project.audio_buses().len(),
        stream_targets: project.stream_targets().len(),
        stingers: project.stingers().len(),
        main_mix: project.main_mix().is_some(),
    }
}

fn next_id(existing: impl Iterator<Item = NonZeroU128>) -> Option<NonZeroU128> {
    existing
        .max()
        .map_or(NonZeroU128::new(1), |max| max.checked_add(1))
}

fn allocate(next: &mut Option<NonZeroU128>) -> Result<NonZeroU128, BundleError> {
    let id = next.ok_or(BundleError::IdSpaceExhausted)?;
    *next = id.checked_add(1);
    Ok(id)
}

/// `name`, then `name (2)`, `name (3)`, ... truncating the base on a char
/// boundary so the result fits in `max_bytes`.
fn unique_name(
    name: &str,
    max_bytes: usize,
    taken: impl Fn(&str) -> bool,
) -> Result<String, BundleError> {
    if !taken(name) {
        return Ok(name.to_owned());
    }
    for attempt in 2..=MAX_NAME_SUFFIX {
        let suffix = format!(" ({attempt})");
        let mut end = name.len().min(max_bytes.saturating_sub(suffix.len()));
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = format!("{}{suffix}", &name[..end]);
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    Err(BundleError::NoAvailableKey {
        key: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names_suffix_and_respect_byte_limit() {
        let taken = ["Cam", "Cam (2)"];
        assert_eq!(
            unique_name("Cam", 128, |name| taken.contains(&name)).unwrap(),
            "Cam (3)"
        );
        assert_eq!(unique_name("New", 128, |_| false).unwrap(), "New");
        let long = "é".repeat(64); // 128 bytes
        let renamed = unique_name(&long, 128, |name| name == long).unwrap();
        assert!(renamed.len() <= 128);
        assert!(renamed.ends_with(" (2)"));
    }

    #[test]
    fn id_allocation_reports_exhaustion() {
        let mut next = next_id([NonZeroU128::MAX].into_iter());
        assert!(matches!(
            allocate(&mut next),
            Err(BundleError::IdSpaceExhausted)
        ));
        let mut next = next_id(std::iter::empty());
        assert_eq!(allocate(&mut next).unwrap().get(), 1);
        assert_eq!(allocate(&mut next).unwrap().get(), 2);
    }
}
