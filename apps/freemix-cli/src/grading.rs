//! Local colour grading commands and the grades a render applies.
//!
//! Grades are project edits: they rewrite `project.json` (refusing a bundle a
//! daemon holds or with unapplied journal batches, like every local
//! mutation) and honour the project and per-input padlocks (a scene layer's
//! grade is covered by the padlock of every Scene input showing its scene).
//! Imported LUTs are copied to `assets/luts/<id>.cube` and referenced by id.

use std::{collections::BTreeMap, fmt::Write as _, num::NonZeroU32, path::Path, sync::Arc};

use fm_engine::CompiledProjectGrades;
use fm_grade::{BUILTIN_PRESETS, CubeLut, Grade, GradeLut, builtin_preset};
use fm_model::{
    GradeId, GradeSource, InputKind, LayerId, LutId, Project, ProjectGrade, SceneLayerRef,
    lut_asset_uri,
};
use fm_persistence::ProjectStore;
use fm_types::{InputId, SceneId};

use crate::{
    app::{AppFailure, AppResult, inspect_stored_project, update_project},
    args::{GradeSpec, GradeTargetArg},
};

fn input_id(input: u128) -> AppResult<InputId> {
    Ok(InputId::new(
        core::num::NonZeroU128::new(input)
            .ok_or_else(|| AppFailure("input id must be nonzero".into()))?,
    ))
}

fn layer_target(scene: u128, layer: u32) -> AppResult<SceneLayerRef> {
    Ok(SceneLayerRef::new(
        SceneId::new(
            core::num::NonZeroU128::new(scene)
                .ok_or_else(|| AppFailure("scene id must be nonzero".into()))?,
        ),
        LayerId::new(layer).ok_or_else(|| AppFailure("layer id must be nonzero".into()))?,
    ))
}

fn check_unlocked(project: &Project, target: GradeTargetArg) -> AppResult<()> {
    if project.project_locked() {
        return Err(AppFailure("project is locked".into()).into());
    }
    match target {
        GradeTargetArg::Input(input) if project.is_input_locked(input_id(input)?) => {
            return Err(AppFailure(format!("input {input} is locked")).into());
        }
        GradeTargetArg::Layer { scene, layer } => {
            let target = layer_target(scene, layer)?;
            if let Some(locked) = project.inputs().iter().find(|input| {
                matches!(input.kind, InputKind::Scene { scene_id, .. } if scene_id == target.scene)
                    && project.is_input_locked(input.id)
            }) {
                return Err(AppFailure(format!("input {} is locked", locked.id)).into());
            }
        }
        _ => {}
    }
    Ok(())
}

fn assign(
    project: &mut Project,
    target: GradeTargetArg,
    source: Option<GradeSource>,
) -> AppResult<()> {
    let result = match target {
        GradeTargetArg::Input(input) => project.set_input_grade(input_id(input)?, source),
        GradeTargetArg::Layer { scene, layer } => {
            project.set_layer_grade(layer_target(scene, layer)?, source)
        }
        GradeTargetArg::Program => project.set_program_grade(source),
    };
    result.map_err(|error| AppFailure(error.to_string()).into())
}

/// Builds the grade a [`GradeSpec`] describes over its preset.
fn build_grade(spec: &GradeSpec) -> AppResult<Grade> {
    let mut grade = match &spec.preset {
        None => Grade::IDENTITY,
        Some(id) => builtin_preset(id)
            .ok_or_else(|| {
                let known = BUILTIN_PRESETS
                    .iter()
                    .map(|preset| preset.id)
                    .collect::<Vec<_>>()
                    .join(", ");
                AppFailure(format!("unknown grade preset `{id}` (one of {known})"))
            })?
            .grade
            .clone(),
    };
    if let Some(value) = spec.exposure_millistops {
        grade.exposure_millistops = value;
    }
    if let Some(value) = spec.temperature {
        grade.white_balance.temperature = value;
    }
    if let Some(value) = spec.tint {
        grade.white_balance.tint = value;
    }
    if let Some(value) = spec.contrast {
        grade.contrast.amount = value;
    }
    if let Some(value) = spec.saturation_permille {
        grade.saturation_permille = value;
    }
    if let Some(lut) = spec.lut {
        let id = LutId::new(
            NonZeroU32::new(lut).ok_or_else(|| AppFailure("LUT id must be nonzero".into()))?,
        );
        grade.lut = Some(GradeLut::new(
            lut_asset_uri(id),
            spec.lut_strength_permille.unwrap_or(1000),
        ));
    }
    grade
        .validate()
        .map_err(|error| AppFailure(format!("invalid grade: {error}")))?;
    Ok(grade)
}

fn target_name(target: GradeTargetArg) -> String {
    match target {
        GradeTargetArg::Input(input) => format!("input {input}"),
        GradeTargetArg::Layer { scene, layer } => format!("scene {scene} layer {layer}"),
        GradeTargetArg::Program => "program".into(),
    }
}

pub fn set(
    path: &Path,
    target: GradeTargetArg,
    library: Option<u32>,
    spec: &GradeSpec,
    save_as: Option<String>,
) -> AppResult<()> {
    update_project(path, |project| {
        check_unlocked(project, target)?;
        let source = if let Some(id) = library {
            GradeSource::Library(GradeId::new(
                NonZeroU32::new(id).ok_or_else(|| AppFailure("grade id must be nonzero".into()))?,
            ))
        } else {
            let grade = build_grade(spec)?;
            if let Some(name) = save_as {
                // Above the highest id, wrapping to the lowest free one once
                // a client has taken `u32::MAX`.
                let next = fm_model::next_free_id(
                    project.grades().iter().map(|entry| entry.id.get()),
                    |_| false,
                )
                .ok_or_else(|| AppFailure("no grade id is available".into()))?;
                let id = GradeId::new(next);
                project
                    .add_grade(ProjectGrade { id, name, grade })
                    .map_err(|error| AppFailure(error.to_string()))?;
                println!("grade-set: saved library grade {id}");
                GradeSource::Library(id)
            } else {
                GradeSource::Inline(Box::new(grade))
            }
        };
        assign(project, target, Some(source))?;
        println!("grade-set: graded {}", target_name(target));
        Ok(())
    })
}

pub fn clear(path: &Path, target: GradeTargetArg) -> AppResult<()> {
    update_project(path, |project| {
        check_unlocked(project, target)?;
        assign(project, target, None)?;
        println!("grade-clear: cleared {}", target_name(target));
        Ok(())
    })
}

pub fn import_lut(path: &Path, file: &Path, name: Option<String>) -> AppResult<()> {
    let name = name.unwrap_or_else(|| {
        file.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or_else(|| "LUT".into())
    });
    update_project(path, |project| {
        if project.project_locked() {
            return Err(AppFailure("project is locked".into()).into());
        }
        let imported = ProjectStore::new(path)?
            .import_lut(project, file, &name)
            .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
        println!(
            "grade-import-lut: LUT {} {:?} {:?} size {} ({} bytes) as {}",
            imported.lut.id,
            imported.lut.name,
            imported.lut.kind,
            imported.lut.size,
            imported.bytes,
            imported.lut.asset_uri()
        );
        for warning in &imported.warnings {
            println!("warning: {warning}");
        }
        Ok(())
    })
}

fn describe(grade: &Grade) -> String {
    let mut text = format!(
        "enabled={} exposure={} temperature={} tint={} contrast={} saturation={}",
        grade.enabled,
        grade.exposure_millistops,
        grade.white_balance.temperature,
        grade.white_balance.tint,
        grade.contrast.amount,
        grade.saturation_permille
    );
    if let Some(lut) = &grade.lut {
        let _ = write!(
            text,
            " lut={} strength={}",
            lut.asset_uri, lut.strength_permille
        );
    }
    text
}

fn describe_source(source: &GradeSource) -> String {
    match source {
        GradeSource::Library(id) => format!("library {id}"),
        GradeSource::Inline(grade) => format!("inline {}", describe(grade)),
    }
}

pub fn list(path: &Path) -> AppResult<()> {
    let stored = inspect_stored_project(path)?;
    let project = stored.project();
    for lut in project.luts() {
        println!(
            "lut id={} name={:?} kind={:?} size={} uri={}",
            lut.id,
            lut.name,
            lut.kind,
            lut.size,
            lut.asset_uri()
        );
    }
    for entry in project.grades() {
        println!(
            "grade id={} name={:?} {}",
            entry.id,
            entry.name,
            describe(&entry.grade)
        );
    }
    for (input, source) in &project.grading().inputs {
        println!("input {input}: {}", describe_source(source));
    }
    for (target, source) in &project.grading().layers {
        println!(
            "scene {} layer {}: {}",
            target.scene,
            target.layer,
            describe_source(source)
        );
    }
    match project.program_grade() {
        Some(source) => println!("program: {}", describe_source(source)),
        None => println!("program: ungraded"),
    }
    Ok(())
}

/// Loads the referenced LUTs and compiles the project's grades for a
/// render. A LUT that cannot be loaded is bypassed and reported.
pub(crate) fn compiled_grades(path: &Path, project: &Project) -> AppResult<CompiledProjectGrades> {
    let grading = project.grading();
    if grading.inputs.is_empty() && grading.layers.is_empty() && grading.program.is_none() {
        return Ok(CompiledProjectGrades::default());
    }
    let store = ProjectStore::new(path)?;
    let mut luts: BTreeMap<LutId, Arc<CubeLut>> = BTreeMap::new();
    for id in grading.referenced_luts() {
        let Some(lut) = grading.lut(id) else { continue };
        match store.load_lut(lut) {
            Ok(loaded) => {
                luts.insert(id, loaded);
            }
            Err(error) => println!("degraded: LUT {id} bypassed: {error}"),
        }
    }
    let grades = CompiledProjectGrades::compile(project, &luts)
        .map_err(|error| AppFailure(error.to_string()))?;
    Ok(grades)
}
