//! Sharing presets between projects through files (GX-011, SW-019).
//!
//! `preset-export` writes one of the project's user animation presets to a
//! `.fmpreset` file and `preset-import` adds one to the library
//! (`preset-save` adds one title element's keys, and the animation preset
//! commands apply a library preset as `user:<id>`);
//! `grade-export` writes a library grade (with its LUT's `.cube` text
//! embedded, or beside the file when it is large) to a `.fmgrade` file and
//! `grade-import` adds one to the grade library, importing its LUT into the
//! bundle first. Imports are project edits: they refuse a locked project
//! and rename an imported preset or grade whose name is taken (`Name (2)`).

use std::{num::NonZeroU32, path::Path};

use fm_grade::GradeLut;
use fm_model::fm_animation::{Phase, PhaseMotion};
use fm_model::{AnimationPresetId, GradeId, ProjectGrade, UserAnimationPreset, lut_id_from_uri};
use fm_persistence::{
    AnimationPresetFile, ExportedGradeLut, GradeExportLut, ProjectStore,
    read_animation_preset_file, read_grade_preset_file, write_animation_preset_file,
    write_grade_preset_file,
};

use crate::app::{AppFailure, AppResult, inspect_stored_project, update_project};

fn nonzero(value: u32, what: &str) -> AppResult<NonZeroU32> {
    NonZeroU32::new(value).ok_or_else(|| AppFailure(format!("{what} id must be nonzero")).into())
}

/// `preset-export`: writes library preset `preset` to `file`.
pub fn export_animation(path: &Path, preset: u32, file: &Path) -> AppResult<()> {
    let stored = inspect_stored_project(path)?;
    let id = AnimationPresetId::new(nonzero(preset, "preset")?);
    let preset = stored
        .project()
        .animation_presets()
        .get(id)
        .ok_or_else(|| AppFailure(format!("animation preset {id} does not exist")))?;
    write_animation_preset_file(
        file,
        &AnimationPresetFile {
            name: preset.name.clone(),
            phase: preset.phase,
            motion: preset.motion.clone(),
        },
    )
    .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    println!(
        "preset-export: preset {id} {:?} to {}",
        preset.name,
        file.display()
    );
    Ok(())
}

/// The motion of the project preset a `user:<id>` preset argument names,
/// or `None` for any other argument (a built-in preset id).
pub fn user_motion(path: &Path, preset: &str) -> AppResult<Option<PhaseMotion>> {
    let Some(id) = preset.strip_prefix("user:") else {
        return Ok(None);
    };
    let id = id
        .parse::<u32>()
        .ok()
        .and_then(NonZeroU32::new)
        .map(AnimationPresetId::new)
        .ok_or_else(|| AppFailure(format!("`{preset}` must be user:<nonzero preset id>")))?;
    let stored = inspect_stored_project(path)?;
    let preset = stored
        .project()
        .animation_presets()
        .get(id)
        .ok_or_else(|| AppFailure(format!("animation preset {id} does not exist")))?;
    Ok(Some(preset.motion.clone()))
}

/// Adds `motion` (saved from `phase`) to the library as `name` (renamed
/// when taken), printing the result as `command`.
pub fn add_animation(
    path: &Path,
    name: &str,
    phase: Phase,
    motion: &PhaseMotion,
    command: &str,
) -> AppResult<()> {
    update_project(path, |project| {
        if project.project_locked() {
            return Err(AppFailure("project is locked".into()).into());
        }
        let library = project.animation_presets();
        let id = library
            .next_id()
            .ok_or_else(|| AppFailure("no animation preset id is available".into()))?;
        let name = library.unused_name(name.trim());
        let frames = motion.frames();
        project
            .animation_presets_mut()
            .insert(
                None,
                UserAnimationPreset {
                    id,
                    name: name.clone(),
                    phase,
                    motion: motion.clone(),
                },
            )
            .map_err(|error| AppFailure(error.to_string()))?;
        let phase = match phase {
            Phase::In => "in",
            Phase::Out => "out",
        };
        println!("{command}: preset {id} {name:?} phase={phase} frames={frames}");
        Ok(())
    })
}

/// `preset-import`: adds the preset in `file` to the library, as `name`
/// when given.
pub fn import_animation(path: &Path, file: &Path, name: Option<&str>) -> AppResult<()> {
    let shared = read_animation_preset_file(file)
        .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    add_animation(
        path,
        name.unwrap_or(&shared.name),
        shared.phase,
        &shared.motion,
        "preset-import",
    )
}

/// `grade-export`: writes library grade `grade` to `file`.
pub fn export_grade(path: &Path, grade: u32, file: &Path) -> AppResult<()> {
    let stored = inspect_stored_project(path)?;
    let project = stored.project();
    let id = GradeId::new(nonzero(grade, "grade")?);
    let entry = project
        .grades()
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| AppFailure(format!("library grade {id} does not exist")))?;
    let lut = match &entry.grade.lut {
        None => None,
        Some(reference) => {
            let lut = lut_id_from_uri(&reference.asset_uri)
                .and_then(|lut| project.luts().iter().find(|entry| entry.id == lut))
                .ok_or_else(|| {
                    AppFailure(format!("LUT {} is not registered", reference.asset_uri))
                })?;
            let (text, _) = ProjectStore::new(path)?
                .load_lut_source(lut)
                .map_err(|error| AppFailure(format!("LUT {}: {error}", lut.id)))?;
            Some((lut.name.clone(), reference.strength_permille, text))
        }
    };
    let exported = write_grade_preset_file(
        file,
        &entry.name,
        &entry.grade,
        lut.as_ref().map(|(name, strength, text)| GradeExportLut {
            name,
            strength_permille: *strength,
            text: Some(text),
        }),
    )
    .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    let lut = match exported {
        None => String::new(),
        Some(ExportedGradeLut::Embedded) => " (LUT embedded)".into(),
        Some(ExportedGradeLut::Sibling { path, .. }) => {
            format!(" (LUT written to {})", path.display())
        }
    };
    println!(
        "grade-export: grade {id} {:?} to {}{lut}",
        entry.name,
        file.display()
    );
    Ok(())
}

/// `grade-import`: adds the grade in `file` to the library (importing its
/// LUT into the bundle), as `name` when given.
pub fn import_grade(path: &Path, file: &Path, name: Option<&str>) -> AppResult<()> {
    let shared = read_grade_preset_file(file)
        .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    update_project(path, |project| {
        if project.project_locked() {
            return Err(AppFailure("project is locked".into()).into());
        }
        let id =
            fm_model::next_free_id(project.grades().iter().map(|entry| entry.id.get()), |_| {
                false
            })
            .map(GradeId::new)
            .ok_or_else(|| AppFailure("no grade id is available".into()))?;
        let mut grade = shared.grade.clone();
        if let Some(lut) = &shared.lut {
            let lut_name = unused(&lut.name, |candidate| {
                project.luts().iter().any(|entry| entry.name == candidate)
            });
            let imported = ProjectStore::new(path)?
                .import_lut_text(project, &lut.text, &lut_name)
                .map_err(|error| AppFailure(format!("LUT {:?}: {error}", lut.name)))?;
            println!(
                "grade-import: LUT {} {:?} as {}",
                imported.lut.id,
                imported.lut.name,
                imported.lut.asset_uri()
            );
            grade.lut = Some(GradeLut::new(
                imported.lut.asset_uri(),
                lut.strength_permille,
            ));
        }
        let name = unused(name.unwrap_or(&shared.name), |candidate| {
            project.grades().iter().any(|entry| entry.name == candidate)
        });
        project
            .add_grade(ProjectGrade {
                id,
                name: name.clone(),
                grade,
            })
            .map_err(|error| AppFailure(error.to_string()))?;
        println!("grade-import: library grade {id} {name:?}");
        Ok(())
    })
}

/// `name`, or `name (2)`, `name (3)`, ... : the first `taken` refuses not.
fn unused(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let name = name.trim();
    if !taken(name) {
        return name.to_owned();
    }
    (2..=u32::MAX)
        .map(|count| format!("{name} ({count})"))
        .find(|candidate| !taken(candidate))
        .expect("a finite project leaves a free suffix")
}
