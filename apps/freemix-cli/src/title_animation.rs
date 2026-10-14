//! Local title animation commands: apply a preset, stretch a phase, and
//! render one title input at one frame of a phase.
//!
//! Edits rewrite the stored `.fmtitle` template in place (every title input
//! naming it sees the change) and refuse a bundle a running daemon holds,
//! like every other local mutation. Rendering is read-only: it draws the
//! input's own field values at the template's canvas size with the bundle's
//! fonts. Image fields draw the reference renderer's placeholder (the CLI
//! decodes no images) and production clocks have no clock source here, so
//! both are reported rather than drawn.

use std::{fs, path::Path};

use fm_persistence::{ProjectStore, StoredTitleTemplate};
use fm_titles::{
    AnimSample, AssetCatalog, ElementId, FieldId, FieldValue, ImageSource, MAX_FONT_BYTES,
    ReferenceRenderer, TitleId,
    fm_animation::{AnimationPreset, ClipPosition, Phase},
};
use fm_types::InputId;

use crate::{
    app::{AppFailure, AppResult, inspect_stored_project, load_stored_project, write_title_ppm},
    args::TitlePhase,
};

const fn phase(phase: TitlePhase) -> Phase {
    match phase {
        TitlePhase::In => Phase::In,
        TitlePhase::Out => Phase::Out,
    }
}

const fn phase_name(phase: TitlePhase) -> &'static str {
    match phase {
        TitlePhase::In => "in",
        TitlePhase::Out => "out",
    }
}

/// Loads the template at `uri` after refusing a bundle a daemon holds.
fn editable_template(path: &Path, uri: &str) -> AppResult<(ProjectStore, StoredTitleTemplate)> {
    load_stored_project(path)?;
    let store = ProjectStore::new(path)?;
    let stored = store
        .load_title_template(uri)
        .map_err(|error| AppFailure(format!("title template {uri}: {error}")))?;
    Ok((store, stored))
}

fn save(
    command: &str,
    store: &ProjectStore,
    uri: &str,
    stored: &StoredTitleTemplate,
) -> AppResult<()> {
    store
        .replace_title_template(uri, stored)
        .map_err(|error| AppFailure(format!("title template {uri}: {error}")))?;
    let clip = &stored.template.animation.clip;
    println!(
        "{command}: {uri} revision={} in_frames={} out_frames={} tracks={}",
        stored.template.animation.revision,
        clip.in_frames(),
        clip.out_frames(),
        clip.tracks().len()
    );
    Ok(())
}

pub fn preset(
    path: &Path,
    uri: &str,
    element: u128,
    title_phase: TitlePhase,
    preset: &str,
    frames: u32,
) -> AppResult<()> {
    let Some(preset) = AnimationPreset::from_id(preset) else {
        let known = AnimationPreset::ALL.map(AnimationPreset::id).join(", ");
        return Err(AppFailure(format!(
            "unknown animation preset `{preset}` (one of {known})"
        ))
        .into());
    };
    let element = ElementId::new(
        core::num::NonZeroU128::new(element)
            .ok_or_else(|| AppFailure("element id must be nonzero".into()))?,
    );
    let (store, mut stored) = editable_template(path, uri)?;
    stored
        .template
        .apply_animation_preset(element, phase(title_phase), preset, frames)
        .map_err(|error| AppFailure(error.to_string()))?;
    save("title-animation-preset", &store, uri, &stored)
}

pub fn stretch(path: &Path, uri: &str, title_phase: TitlePhase, frames: u32) -> AppResult<()> {
    let (store, mut stored) = editable_template(path, uri)?;
    stored
        .template
        .stretch_animation(phase(title_phase), frames)
        .map_err(|error| AppFailure(error.to_string()))?;
    save("title-animation-stretch", &store, uri, &stored)
}

pub fn render_frame(
    path: &Path,
    input: u128,
    title_phase: TitlePhase,
    frame: u32,
    output: &Path,
) -> AppResult<()> {
    let id = InputId::new(
        core::num::NonZeroU128::new(input)
            .ok_or_else(|| AppFailure("input id must be nonzero".into()))?,
    );
    let project = inspect_stored_project(path)?;
    let title = project
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == id)
        .and_then(|candidate| match &candidate.kind {
            fm_model::InputKind::Title(title) => Some(title.clone()),
            _ => None,
        })
        .ok_or_else(|| AppFailure(format!("input {input} is not a title input")))?;
    let store = ProjectStore::new(path)?;
    let stored = store
        .load_title_template(&title.template_uri)
        .map_err(|error| AppFailure(format!("title template {}: {error}", title.template_uri)))?;
    let mut catalog = AssetCatalog::new();
    for font in &stored.fonts {
        let file = store
            .resolve_asset_uri(&font.asset_uri)
            .map_err(|error| AppFailure(format!("font {}: {error}", font.asset_uri)))?;
        let size = fs::metadata(&file)?.len();
        if size > u64::try_from(MAX_FONT_BYTES).unwrap_or(u64::MAX) {
            return Err(AppFailure(format!("font {} is too large", font.asset_uri)).into());
        }
        catalog = catalog
            .with_font_bytes(font.family.clone(), fs::read(&file)?)
            .map_err(|error| AppFailure(format!("font {}: {error}", font.asset_uri)))?;
    }
    let mut scene = stored
        .template
        .instantiate(TitleId::new(id.get()))
        .map_err(|error| AppFailure(format!("title template does not instantiate: {error}")))?;
    let updates = title
        .fields
        .iter()
        .map(|entry| (FieldId::new(entry.id.get()), field_value(&entry.value)))
        .collect::<Vec<_>>();
    scene
        .update_fields(scene.revision(), &updates)
        .map_err(|error| AppFailure(format!("title fields do not match the template: {error}")))?;
    let position = ClipPosition::frame(frame);
    let sample = match title_phase {
        TitlePhase::In => AnimSample::In(position),
        TitlePhase::Out => AnimSample::Out(position),
    };
    let rendered = ReferenceRenderer
        .render_sampled(&scene, 0, &catalog, sample)
        .map_err(|error| AppFailure(format!("title render failed: {error}")))?;
    write_title_ppm(output, &rendered.frame)?;
    println!(
        "title-render-frame: input {input} phase={} frame={frame} {}x{} to {}",
        phase_name(title_phase),
        rendered.frame.width(),
        rendered.frame.height(),
        output.display()
    );
    for degraded in &rendered.report.degraded {
        println!("degraded: {degraded}");
    }
    for missing in &rendered.report.missing_images {
        println!(
            "placeholder image: field {} {}",
            missing.field, missing.asset
        );
    }
    Ok(())
}

fn field_value(value: &fm_model::TitleFieldValue) -> FieldValue {
    match value {
        fm_model::TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
        fm_model::TitleFieldValue::Number(number) => FieldValue::Number(*number),
        fm_model::TitleFieldValue::Color(color) => FieldValue::Color(fm_titles::Color::new(
            color.red,
            color.green,
            color.blue,
            color.alpha,
        )),
        fm_model::TitleFieldValue::Image { asset_uri } => {
            FieldValue::Image(ImageSource::new(asset_uri))
        }
    }
}
