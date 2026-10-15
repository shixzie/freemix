//! Host side of preset sharing (GX-011, SW-019): writes and reads the
//! `.fmpreset` and `.fmgrade` files the Animation and Colour windows ask
//! for, with the strict `fm-preset-file` codecs.
//!
//! A grade export embeds its LUT's `.cube` text read from the supervised
//! project bundle (`assets/luts/<id>.cube`), or writes it beside the
//! `.fmgrade` when it is large. Connected to an independent daemon, Studio
//! cannot read the bundle: the file then names a sibling `.cube` that the
//! operator copies by hand, which the toast says.

use std::path::Path;

use fm_preset_file::{
    AnimationPresetFile, ExportedGradeLut, GradeExportLut, read_animation_preset_file,
    read_grade_preset_file, write_animation_preset_file, write_grade_preset_file,
};
use fm_ui_egui::{ExportedLut, ImportedLut, PresetFileAnswer, PresetFileRequest};

// Every embedded LUT fits one `grade_lut_import`, so Studio can import any
// `.fmgrade` whose LUT is embedded.
const _: () =
    assert!(fm_preset_file::MAX_EMBEDDED_LUT_BYTES == fm_protocol::MAX_GRADE_LUT_IMPORT_BYTES);

/// Largest bundled LUT a grade export reads.
const MAX_BUNDLED_LUT_BYTES: u64 = 64 * 1024 * 1024;

/// The `.cube` text of project LUT `lut` in `bundle`, if it can be read.
fn bundled_lut(bundle: &Path, lut: u32) -> Option<String> {
    let path = bundle
        .join("assets")
        .join("luts")
        .join(format!("{lut}.cube"));
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BUNDLED_LUT_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Carries out one preset file request; `bundle` is the supervised project
/// bundle, if any.
pub(crate) fn carry_out(request: &PresetFileRequest, bundle: Option<&Path>) -> PresetFileAnswer {
    let failed = |error: &dyn std::fmt::Display| PresetFileAnswer::Failed(error.to_string());
    match request {
        PresetFileRequest::ExportAnimation {
            path,
            name,
            phase,
            motion,
        } => match write_animation_preset_file(
            path,
            &AnimationPresetFile {
                name: name.clone(),
                phase: *phase,
                motion: (**motion).clone(),
            },
        ) {
            Ok(()) => PresetFileAnswer::Exported { lut: None },
            Err(error) => failed(&error),
        },
        PresetFileRequest::ImportAnimation { path } => match read_animation_preset_file(path) {
            Ok(preset) => PresetFileAnswer::Animation {
                name: preset.name,
                phase: preset.phase,
                motion: preset.motion,
            },
            Err(error) => failed(&error),
        },
        PresetFileRequest::ExportGrade {
            path,
            name,
            grade,
            lut,
        } => {
            let text = lut
                .as_ref()
                .and_then(|lut| bundle.and_then(|bundle| bundled_lut(bundle, lut.lut.get())));
            let export = lut.as_ref().map(|lut| GradeExportLut {
                name: &lut.name,
                strength_permille: lut.strength_permille,
                text: text.as_deref(),
            });
            match write_grade_preset_file(path, name, grade, export) {
                Ok(exported) => PresetFileAnswer::Exported {
                    lut: exported.map(|exported| match exported {
                        ExportedGradeLut::Embedded => ExportedLut::Embedded,
                        ExportedGradeLut::Sibling { path, written } => {
                            ExportedLut::Sibling { path, written }
                        }
                    }),
                },
                Err(error) => failed(&error),
            }
        }
        PresetFileRequest::ImportGrade { path } => match read_grade_preset_file(path) {
            Ok(preset) => PresetFileAnswer::Grade {
                name: preset.name,
                grade: Box::new(preset.grade),
                lut: preset.lut.map(|lut| ImportedLut {
                    name: lut.name,
                    strength_permille: lut.strength_permille,
                    text: lut.text,
                }),
            },
            Err(error) => failed(&error),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fm_animation::{
        Clip, Easing, Keyframe, MotionTarget, Phase, PhaseMotion, Property, Timebase, Track,
        TrackKey, Value,
    };
    use fm_grade::Grade;
    use fm_types::FrameRate;

    use super::*;

    fn motion() -> PhaseMotion {
        let clip = Clip::from_tracks(
            Timebase::new(FrameRate::new(30, 1).unwrap()),
            12,
            0,
            vec![Track::new(
                TrackKey::new(MotionTarget, Phase::In, Property::Opacity),
                vec![
                    Keyframe::new(0, Value::Scalar(0), Easing::CubicOut),
                    Keyframe::new(12, Value::Scalar(255), Easing::Linear),
                ],
            )],
        )
        .unwrap();
        PhaseMotion::new(clip, 100, 40).unwrap()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "freemix-studio-presets-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn animation_presets_round_trip_through_files() {
        let directory = scratch("animation");
        let path = directory.join("swoosh.fmpreset");
        let export = PresetFileRequest::ExportAnimation {
            path: path.clone(),
            name: "Swoosh".to_owned(),
            phase: Phase::Out,
            motion: Arc::new(motion()),
        };
        assert_eq!(
            carry_out(&export, None),
            PresetFileAnswer::Exported { lut: None }
        );
        assert_eq!(
            carry_out(&PresetFileRequest::ImportAnimation { path }, None),
            PresetFileAnswer::Animation {
                name: "Swoosh".to_owned(),
                phase: Phase::Out,
                motion: motion(),
            }
        );
        let missing = directory.join("missing.fmpreset");
        assert!(matches!(
            carry_out(&PresetFileRequest::ImportAnimation { path: missing }, None),
            PresetFileAnswer::Failed(_)
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn grade_exports_embed_the_bundled_lut_or_name_a_sibling() {
        let directory = scratch("grade");
        let bundle = directory.join("show.freemix");
        std::fs::create_dir_all(bundle.join("assets/luts")).unwrap();
        let cube = "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n";
        std::fs::write(bundle.join("assets/luts/3.cube"), cube).unwrap();
        let mut grade = Grade::IDENTITY;
        grade.saturation_permille = 1_200;
        let request = |path: std::path::PathBuf| PresetFileRequest::ExportGrade {
            path,
            name: "Warm".to_owned(),
            grade: Box::new(grade.clone()),
            lut: Some(fm_ui_egui::ExportLut {
                lut: core::num::NonZeroU32::new(3).unwrap(),
                name: "Film".to_owned(),
                strength_permille: 500,
            }),
        };

        // Supervised: the LUT text is embedded and comes back on import.
        let embedded = directory.join("warm.fmgrade");
        assert_eq!(
            carry_out(&request(embedded.clone()), Some(&bundle)),
            PresetFileAnswer::Exported {
                lut: Some(ExportedLut::Embedded)
            }
        );
        let PresetFileAnswer::Grade {
            name,
            grade: imported,
            lut,
        } = carry_out(&PresetFileRequest::ImportGrade { path: embedded }, None)
        else {
            panic!("the grade imports");
        };
        assert_eq!(name, "Warm");
        assert_eq!(imported.saturation_permille, 1_200);
        assert_eq!(imported.lut, None);
        assert_eq!(
            lut,
            Some(ImportedLut {
                name: "Film".to_owned(),
                strength_permille: 500,
                text: cube.to_owned(),
            })
        );

        // Without the bundle the file names a sibling the operator copies.
        let unbundled = directory.join("cold.fmgrade");
        assert_eq!(
            carry_out(&request(unbundled.clone()), None),
            PresetFileAnswer::Exported {
                lut: Some(ExportedLut::Sibling {
                    path: directory.join("cold.cube"),
                    written: false,
                })
            }
        );
        assert!(matches!(
            carry_out(
                &PresetFileRequest::ImportGrade {
                    path: unbundled.clone()
                },
                None
            ),
            PresetFileAnswer::Failed(_)
        ));
        std::fs::write(directory.join("cold.cube"), cube).unwrap();
        assert!(matches!(
            carry_out(&PresetFileRequest::ImportGrade { path: unbundled }, None),
            PresetFileAnswer::Grade { lut: Some(_), .. }
        ));
        let _ = std::fs::remove_dir_all(directory);
    }
}
