//! Local `.freemix` bundle operations: portable export, templates, append,
//! media import and relink, autosave and recovery, and recent projects.
//!
//! Every command works on bundle directories directly, like the other local
//! commands; nothing here talks to a daemon. Commands that replace
//! `project.json` load the target through the same journal-checked path as
//! every other local mutation, so they refuse a bundle whose journal holds
//! unapplied batches or is held open by a running daemon.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_model::{InputKind, TitleInput};
use fm_persistence::{
    AppendedAssetDisposition, AssetAuditReason, AssetDisposition, AutosaveRejection, BundleError,
    BundleExport, MAX_RECENT_PROJECTS, ProjectStore, RecentProjectStatus, RecentProjects,
    RelinkResolution, RelinkSearch, StoredProject, title_field_defaults,
};

use crate::app::{
    AppFailure, AppResult, add_input_with_kind, generate_project_id, input_id,
    inspect_stored_project, load_stored_project,
};

pub fn export_bundle(source: &Path, destination: &Path) -> AppResult<()> {
    let export = ProjectStore::new(source)?
        .export_portable_bundle(&ProjectStore::new(destination)?)
        .map_err(|error| bundle_failure("export", &error))?;
    print_bundle_export("project-export-bundle", destination, &export);
    Ok(())
}

pub fn from_template(template: &Path, destination: &Path, name: &str) -> AppResult<()> {
    let export = ProjectStore::new(template)?
        .instantiate_template(
            &ProjectStore::new(destination)?,
            generate_project_id()?,
            name,
        )
        .map_err(|error| bundle_failure("template instantiation", &error))?;
    print_bundle_export("project-from-template", destination, &export);
    Ok(())
}

fn print_bundle_export(command: &str, destination: &Path, export: &BundleExport) {
    let bytes: u64 = export.assets.iter().map(|asset| asset.bytes).sum();
    println!(
        "{command}: wrote {} project_id={} name={:?} assets={} bytes={bytes}",
        destination.display(),
        export.project.project().id(),
        export.project.project().name(),
        export.assets.len(),
    );
    for asset in &export.assets {
        if asset.source_uri == asset.bundled_uri {
            println!("asset {} bytes={}", asset.bundled_uri, asset.bytes);
        } else {
            println!(
                "asset {} -> {} bytes={}",
                asset.source_uri, asset.bundled_uri, asset.bytes
            );
        }
    }
    for input in &export.rewritten_inputs {
        println!("rewritten input={input}");
    }
}

/// Explains bundle failures that would otherwise print only a count.
fn bundle_failure(operation: &str, error: &BundleError) -> AppFailure {
    let mut message = format!("{operation} failed: {error}");
    if let BundleError::UnresolvedAssets(issues) = error {
        for issue in issues {
            let _ = write!(
                message,
                "\n  input={} reason={}",
                issue.input_id,
                audit_reason_name(issue.reason)
            );
        }
        message.push_str("\n  run `freemix-cli project-relink` to repair missing media");
    }
    AppFailure(message)
}

pub const fn audit_reason_name(reason: AssetAuditReason) -> &'static str {
    match reason {
        AssetAuditReason::InvalidUri => "invalid-uri",
        AssetAuditReason::MissingAsset => "missing-asset",
        AssetAuditReason::OutsideAssetsRoot => "outside-assets-root",
        AssetAuditReason::NotRegularFile => "not-regular-file",
        AssetAuditReason::UnreadableTitleTemplate => "unreadable-title-template",
    }
}

pub fn append(target: &Path, source: &Path) -> AppResult<()> {
    let store = ProjectStore::new(target)?;
    let stored = load_stored_project(target)?;
    let (appended, report) = store
        .append_project(&stored, &ProjectStore::new(source)?)
        .map_err(|error| bundle_failure("append", &error))?;
    store.save(&appended)?;

    println!(
        "project-append: appended {} input(s) and {} scene(s) from {} into {}",
        report.inputs.len(),
        report.scenes.len(),
        source.display(),
        target.display(),
    );
    for (from, to) in &report.inputs {
        println!("input {from} -> {to}");
    }
    for (from, to) in &report.scenes {
        println!("scene {from} -> {to}");
    }
    for renamed in &report.renamed_inputs {
        println!(
            "renamed input {}: {:?} -> {:?}",
            renamed.source_id, renamed.from, renamed.to
        );
    }
    for renamed in &report.renamed_scenes {
        println!(
            "renamed scene {}: {:?} -> {:?}",
            renamed.source_id, renamed.from, renamed.to
        );
    }
    for asset in &report.assets {
        let disposition = match asset.disposition {
            AppendedAssetDisposition::Placed(AssetDisposition::Copied { bytes }) => {
                format!("copied bytes={bytes}")
            }
            AppendedAssetDisposition::Placed(AssetDisposition::Reused) => "reused".to_owned(),
            AppendedAssetDisposition::MissingInSource => "missing-in-source".to_owned(),
        };
        println!(
            "asset {} -> {} {disposition}",
            asset.source_uri, asset.target_uri
        );
    }
    let skipped = report.skipped;
    if skipped.outputs + skipped.audio_buses + skipped.stream_targets + skipped.stingers > 0
        || skipped.main_mix
    {
        println!(
            "not appended: outputs={} audio_buses={} stream_targets={} stingers={} main_mix={}",
            skipped.outputs,
            skipped.audio_buses,
            skipped.stream_targets,
            skipped.stingers,
            skipped.main_mix,
        );
    }
    Ok(())
}

pub fn import_media(
    path: &Path,
    file: &Path,
    input: Option<u128>,
    name: Option<String>,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    // Refuse to create an assets directory in something that is not a bundle,
    // and validate the input id before copying anything.
    let stored = load_stored_project(path)?;
    let input = input.map(input_id).transpose()?;
    if let Some(input) = input
        && stored
            .project()
            .inputs()
            .iter()
            .any(|existing| existing.id == input)
    {
        return Err(AppFailure(format!("input {input} already exists")).into());
    }
    let imported = store
        .import_media_file(file)
        .map_err(|error| bundle_failure("media import", &error))?;
    match imported.disposition {
        AssetDisposition::Copied { bytes } => println!(
            "project-import-media: copied {} to {} bytes={bytes}",
            file.display(),
            imported.asset_uri
        ),
        AssetDisposition::Reused => println!(
            "project-import-media: reused identical {} for {}",
            imported.asset_uri,
            file.display()
        ),
    }
    if let Some(input) = input {
        let name = name.unwrap_or_else(|| {
            file.file_stem().map_or_else(
                || "Media".to_owned(),
                |stem| stem.to_string_lossy().into_owned(),
            )
        });
        add_input_with_kind(
            path,
            input,
            name,
            InputKind::Media {
                asset_uri: imported.asset_uri,
            },
        )?;
    }
    Ok(())
}

/// Imports a title template with its fonts and adds a title input showing the
/// template's default field values.
pub fn title_input_add(
    path: &Path,
    input: u128,
    name: String,
    template: &Path,
    fonts: &[(String, PathBuf)],
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    // Refuse a non-bundle or a taken id before copying anything.
    let stored = load_stored_project(path)?;
    let input = input_id(input)?;
    if stored
        .project()
        .inputs()
        .iter()
        .any(|existing| existing.id == input)
    {
        return Err(AppFailure(format!("input {input} already exists")).into());
    }
    let imported = store
        .import_title_template(template, fonts)
        .map_err(|error| AppFailure(format!("title template import failed: {error}")))?;
    println!(
        "title-input-add: template {} {}",
        imported.template_uri,
        disposition_name(imported.disposition)
    );
    for font in &imported.fonts {
        println!(
            "font {:?} {} {}",
            font.family,
            font.asset_uri,
            disposition_name(font.disposition)
        );
    }
    add_input_with_kind(
        path,
        input,
        name,
        InputKind::Title(TitleInput {
            template_uri: imported.template_uri,
            field_revision: 0,
            fields: title_field_defaults(&imported.stored.template),
        }),
    )
}

fn disposition_name(disposition: AssetDisposition) -> String {
    match disposition {
        AssetDisposition::Copied { bytes } => format!("copied bytes={bytes}"),
        AssetDisposition::Reused => "reused".to_owned(),
    }
}

pub fn relink(path: &Path, search: Vec<PathBuf>, apply: bool) -> AppResult<()> {
    for directory in &search {
        if !directory.is_dir() {
            return Err(AppFailure(format!(
                "relink search path `{}` is not a directory",
                directory.display()
            ))
            .into());
        }
    }
    let store = ProjectStore::new(path)?;
    let stored = if apply {
        load_stored_project(path)?
    } else {
        inspect_stored_project(path)?
    };
    let plan = store
        .plan_media_relink(&stored, &RelinkSearch::new(search))
        .map_err(|error| bundle_failure("relink planning", &error))?;
    if plan.entries.is_empty() {
        println!("project-relink: no unresolved media");
        return Ok(());
    }
    for entry in &plan.entries {
        let resolution = match &entry.resolution {
            RelinkResolution::RewriteUri { asset_uri } => format!("rewrite -> {asset_uri}"),
            RelinkResolution::Import { source, bytes } => {
                format!("import {} bytes={bytes}", source.display())
            }
            RelinkResolution::Ambiguous { candidates } => format!(
                "ambiguous candidates=[{}]",
                candidates
                    .iter()
                    .map(|candidate| candidate.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RelinkResolution::NotFound => "not-found".to_owned(),
            RelinkResolution::Unrelinkable => "unrelinkable".to_owned(),
        };
        println!(
            "input={} asset={} reason={} {resolution}",
            entry.input_id,
            entry.asset_uri,
            audit_reason_name(entry.reason)
        );
    }
    if plan.search_truncated {
        println!("warning: search stopped at its depth or entry bound; results may be incomplete");
    }
    let resolvable = plan
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.resolution,
                RelinkResolution::RewriteUri { .. } | RelinkResolution::Import { .. }
            )
        })
        .count();
    if !apply {
        println!(
            "project-relink: preview only; {resolvable} of {} input(s) can be relinked; rerun with --apply to write changes",
            plan.entries.len()
        );
        return Ok(());
    }
    let outcome = store
        .apply_media_relink(&stored, &plan)
        .map_err(|error| bundle_failure("relink", &error))?;
    if !outcome.relinked.is_empty() {
        store.save(&outcome.project)?;
    }
    println!(
        "project-relink: relinked={} unresolved={} imported_bytes={}",
        outcome.relinked.len(),
        outcome.unresolved.len(),
        outcome.imported_bytes
    );
    Ok(())
}

pub fn autosave(path: &Path, keep: usize) -> AppResult<()> {
    // Snapshots the durable manifest without opening the journal, so this also
    // works beside a running daemon.
    let stored = inspect_stored_project(path)?;
    let written = ProjectStore::new(path)?.write_autosave(&stored, keep)?;
    println!(
        "project-autosave: wrote generation {} to {} revision={} keep={keep}",
        written.generation,
        written.path.display(),
        stored.position().revision,
    );
    if !written.pruned.is_empty() {
        println!(
            "pruned generations: {}",
            written
                .pruned
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

pub fn recover(path: &Path, restore: bool) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let generations = store.list_autosaves()?;
    println!(
        "project-recover: {} autosave generation(s) in {}",
        generations.len(),
        store.autosave_path().display()
    );
    let recovery = store.recover_autosave()?;
    for (generation, rejection) in &recovery.rejected {
        let reason = match rejection {
            AutosaveRejection::Unreadable(_) => "unreadable",
            AutosaveRejection::ForeignProject { .. } => "foreign-project",
        };
        println!("rejected generation={generation} reason={reason}: {rejection}");
    }
    let Some((generation, snapshot)) = recovery.recovered else {
        if restore {
            return Err(AppFailure("no usable autosave to restore".into()).into());
        }
        println!("no usable autosave");
        return Ok(());
    };
    let manifest = inspect_stored_project(path);
    println!(
        "recoverable generation={generation} revision={} project_id={} name={:?} differs_from_manifest={}",
        snapshot.position().revision,
        snapshot.project().id(),
        snapshot.project().name(),
        recovery.differs_from_manifest,
    );
    match &manifest {
        Ok(manifest) => println!("manifest revision={}", manifest.position().revision),
        Err(error) => println!("manifest unusable: {error}"),
    }
    if !restore {
        if recovery.differs_from_manifest {
            println!("rerun with --restore to replace project.json with this autosave");
        }
        return Ok(());
    }
    if !recovery.differs_from_manifest {
        println!("project-recover: manifest already matches; nothing restored");
        return Ok(());
    }
    require_restorable(&store, manifest.is_ok(), &snapshot)?;
    store.save(&snapshot)?;
    println!("project-recover: restored generation {generation} to project.json");
    Ok(())
}

/// Refuses a restore that would leave the journal ahead of the manifest or
/// discard batches the manifest has not absorbed yet.
fn require_restorable(
    store: &ProjectStore,
    manifest_usable: bool,
    snapshot: &StoredProject,
) -> AppResult<()> {
    if !store.journal_path().try_exists()? {
        return Ok(());
    }
    if !manifest_usable {
        return Err(AppFailure(
            "restore refused: the bundle has a journal but its manifest is unusable, so the autosave cannot be reconciled with it"
                .into(),
        )
        .into());
    }
    let scan = store.scan_journal()?;
    if !scan.batches().is_empty() {
        return Err(AppFailure(format!(
            "restore refused: {} unapplied journal batch(es) remain; run journal-recover first",
            scan.batches().len()
        ))
        .into());
    }
    if snapshot.position().revision < scan.checkpoint_revision() {
        return Err(AppFailure(format!(
            "restore refused: autosave revision {} is older than the journal checkpoint revision {}",
            snapshot.position().revision,
            scan.checkpoint_revision()
        ))
        .into());
    }
    Ok(())
}

pub fn list_recent(file: &Path) -> AppResult<()> {
    let recent = RecentProjects::load(file, MAX_RECENT_PROJECTS)?;
    if recent.entries().is_empty() {
        println!("recent-projects: none");
        return Ok(());
    }
    for (entry, status) in recent.statuses() {
        println!(
            "{} name={:?} opened_at_unix_ms={} status={}",
            entry.path.display(),
            entry.name,
            entry.opened_at_unix_ms,
            recent_status_name(status)
        );
    }
    Ok(())
}

pub fn add_recent(file: &Path, project: &Path, capacity: usize) -> AppResult<()> {
    let mut recent = RecentProjects::load(file, capacity)?;
    let stored = inspect_stored_project(project)?;
    let opened_at = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .unwrap_or(u64::MAX);
    recent.record(
        &ProjectStore::new(project)?,
        stored.project().name(),
        opened_at,
    )?;
    recent.save(file)?;
    println!(
        "recent-projects-add: recorded {:?}; {} of {} entries",
        stored.project().name(),
        recent.entries().len(),
        recent.capacity()
    );
    Ok(())
}

pub fn remove_recent(file: &Path, project: &Path) -> AppResult<()> {
    let mut recent = RecentProjects::load(file, MAX_RECENT_PROJECTS)?;
    if !recent.remove(project) {
        return Err(AppFailure(format!(
            "{} is not in the recent-projects list",
            project.display()
        ))
        .into());
    }
    recent.save(file)?;
    println!("recent-projects-remove: removed {}", project.display());
    Ok(())
}

pub fn prune_recent(file: &Path) -> AppResult<()> {
    let mut recent = RecentProjects::load(file, MAX_RECENT_PROJECTS)?;
    let removed = recent.prune_unavailable();
    if !removed.is_empty() {
        recent.save(file)?;
    }
    for entry in &removed {
        println!("pruned {}", entry.path.display());
    }
    println!(
        "recent-projects-prune: removed {} unavailable entr{}",
        removed.len(),
        if removed.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

const fn recent_status_name(status: RecentProjectStatus) -> &'static str {
    match status {
        RecentProjectStatus::Available => "available",
        RecentProjectStatus::Missing => "missing",
        RecentProjectStatus::NotABundle => "not-a-bundle",
    }
}
