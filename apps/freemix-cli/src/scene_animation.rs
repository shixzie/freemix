//! Local scene layer animation commands (GX-011 stage 5): apply a preset to
//! one layer, stretch a phase, and render one scene at one frame of a phase.
//!
//! Edits rewrite the scene's animation in `project.json` (refusing a bundle
//! a running daemon holds, like every other local mutation) and advance its
//! animation revision. An edit whose clip would exceed the protocol's
//! `MAX_SCENE_ANIMATION_BYTES` wire bound (the bound `fm-control` holds
//! every daemon edit to) is refused and nothing is saved.
//!
//! Rendering is read-only: it poses the scene with `fm_model::Scene::posed`
//! and composes it like `render` does, at the project canvas size, from the
//! simulated sources.

use std::{num::NonZeroU128, path::Path};

use fm_model::{
    LayerId, Scene,
    fm_animation::{AnimationPreset, ClipPosition, ClipState, Phase},
};
use fm_persistence::{ProjectStore, StoredProject};
use fm_types::SceneId;

use crate::{
    app::{AppFailure, AppResult, inspect_stored_project, load_stored_project, write_title_ppm},
    args::{ScenePhase, TitlePhase},
};

const fn phase(phase: TitlePhase) -> Phase {
    match phase {
        TitlePhase::In => Phase::In,
        TitlePhase::Out => Phase::Out,
    }
}

fn scene_id(value: u128) -> AppResult<SceneId> {
    NonZeroU128::new(value)
        .map(SceneId::new)
        .ok_or_else(|| AppFailure("scene ID must be nonzero".into()).into())
}

fn find_scene(project: &fm_model::Project, scene: SceneId) -> AppResult<&Scene> {
    project
        .scene(scene)
        .ok_or_else(|| AppFailure(format!("scene {scene} does not exist")).into())
}

/// Runs `edit` on the scene and saves the project, printing the result.
fn edit(
    command: &str,
    path: &Path,
    scene: u128,
    edit: impl FnOnce(&mut Scene, u32, u32) -> Result<u64, fm_model::SceneAnimationError>,
) -> AppResult<()> {
    let id = scene_id(scene)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    let dimensions = project.settings().video.dimensions;
    let mut target = find_scene(&project, id)?.clone();
    edit(&mut target, dimensions.width(), dimensions.height())
        .map_err(|error| AppFailure(format!("scene {id}: {error}")))?;
    // The same bound the daemon holds every edit to: a clip without a wire
    // encoding could never be carried by an event or snapshot.
    if !fm_control::scene_clip_fits_wire(&target.animation.clip) {
        return Err(AppFailure(format!(
            "scene {id}: the edited animation exceeds the {} byte protocol limit \
             (MAX_SCENE_ANIMATION_BYTES) and was not saved; use fewer keys or tracks",
            fm_protocol::MAX_SCENE_ANIMATION_BYTES
        ))
        .into());
    }
    project
        .set_scene_animation(id, target.animation.clone())
        .map_err(|error| AppFailure(error.to_string()))?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    let clip = &target.animation.clip;
    println!(
        "{command}: scene {id} revision={} in_frames={} out_frames={} tracks={}",
        target.animation.revision,
        clip.in_frames(),
        clip.out_frames(),
        clip.tracks().len()
    );
    Ok(())
}

pub fn preset(
    path: &Path,
    scene: u128,
    layer: u32,
    title_phase: TitlePhase,
    preset: &str,
    frames: u32,
) -> AppResult<()> {
    let layer = LayerId::new(layer).ok_or_else(|| AppFailure("layer id must be nonzero".into()))?;
    // `user:<id>` names one of the project's own presets (GX-011).
    if let Some(motion) = crate::presets::user_motion(path, preset)? {
        return edit("scene-animation-preset", path, scene, |target, _, _| {
            target.apply_animation_motion(layer, phase(title_phase), &motion, frames)
        });
    }
    let Some(preset) = AnimationPreset::from_id(preset) else {
        let known = AnimationPreset::ALL.map(AnimationPreset::id).join(", ");
        return Err(AppFailure(format!(
            "unknown animation preset `{preset}` (one of {known}, or user:<id>)"
        ))
        .into());
    };
    edit(
        "scene-animation-preset",
        path,
        scene,
        |target, width, height| {
            target.apply_animation_preset(layer, phase(title_phase), preset, frames, width, height)
        },
    )
}

pub fn stretch(path: &Path, scene: u128, title_phase: TitlePhase, frames: u32) -> AppResult<()> {
    edit("scene-animation-stretch", path, scene, |target, _, _| {
        target.stretch_animation(phase(title_phase), frames)
    })
}

/// Renders scene `scene` posed at `scene_phase` (frame `frame` of an In or
/// Out phase, or the held In end) at `width`x`height`.
pub fn render_frame(
    path: &Path,
    scene: u128,
    scene_phase: ScenePhase,
    frame: u32,
    output: &Path,
    width: u32,
    height: u32,
) -> AppResult<()> {
    let id = scene_id(scene)?;
    let stored = inspect_stored_project(path)?;
    let project = stored.project();
    find_scene(project, id)?;
    let dimensions = project.settings().video.dimensions;
    let pipeline =
        crate::app::simulated_pipeline(project, dimensions.width(), dimensions.height())?;
    let grades = crate::grading::compiled_grades(path, project)?;
    let (state, name) = match scene_phase {
        ScenePhase::In => (ClipState::In(ClipPosition::frame(frame)), "in"),
        ScenePhase::Hold => (ClipState::Hold, "hold"),
        ScenePhase::Out => (ClipState::Out(ClipPosition::frame(frame)), "out"),
    };
    let rendered = crate::scene_render::render_scene(
        project, id, state, &pipeline, &grades, None, 0, width, height,
    )?;
    write_title_ppm(output, &rendered)?;
    println!(
        "scene-render-frame: scene {id} phase={name} frame={frame} {width}x{height} to {}",
        output.display()
    );
    Ok(())
}
