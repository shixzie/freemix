//! Host side of preset sharing (GX-011, SW-019): writes and reads the
//! `.fmpreset` and `.fmgrade` files the Animation and Colour windows ask
//! for, and the `.fmshortcuts` files of the Shortcuts window (RC-004),
//! with the strict `fm-preset-file` codecs.
//!
//! A grade export embeds its LUT's `.cube` text read from the supervised
//! project bundle (`assets/luts/<id>.cube`), or writes it beside the
//! `.fmgrade` when it is large. Connected to an independent daemon, Studio
//! cannot read the bundle: the file then names a sibling `.cube` that the
//! operator copies by hand, which the toast says. A large LUT never
//! overwrites an existing `.cube` beside the `.fmgrade` unless the operator
//! confirmed it (`replace_lut`); otherwise the answer is
//! [`PresetFileAnswer::LutFileExists`] and the Colour window asks.

use std::path::Path;

use fm_preset_file::shortcuts::{ShortcutSetFile, read_shortcut_set_file, write_shortcut_set_file};
use fm_preset_file::{
    AnimationPresetFile, ExportedGradeLut, GradeExportLut, PresetFileError,
    read_animation_preset_file, read_grade_preset_file, write_animation_preset_file,
    write_grade_preset_file,
};
use fm_ui_egui::{ExportedLut, ImportedLut, PresetFileAnswer, PresetFileRequest};

// Every embedded LUT fits one `grade_lut_import`, so Studio can import any
// `.fmgrade` whose LUT is embedded.
const _: () =
    assert!(fm_preset_file::MAX_EMBEDDED_LUT_BYTES == fm_protocol::MAX_GRADE_LUT_IMPORT_BYTES);

/// Largest bundled LUT a grade export reads.
const MAX_BUNDLED_LUT_BYTES: u64 = 64 * 1024 * 1024;

/// The `.cube` text of project LUT `lut` in `bundle`, if it can be read.
/// A `.fmkeys` file's text, at most [`fm_ui_egui::MAX_KEY_FILE_BYTES`].
fn read_key_profile(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut text = String::new();
    let limit = u64::try_from(fm_ui_egui::MAX_KEY_FILE_BYTES).unwrap_or(u64::MAX) + 1;
    file.take(limit)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > fm_ui_egui::MAX_KEY_FILE_BYTES {
        return Err("the file is too long".to_owned());
    }
    Ok(text)
}

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
            replace_lut,
        } => {
            let text = lut
                .as_ref()
                .and_then(|lut| bundle.and_then(|bundle| bundled_lut(bundle, lut.lut.get())));
            let export = lut.as_ref().map(|lut| GradeExportLut {
                name: &lut.name,
                strength_permille: lut.strength_permille,
                text: text.as_deref(),
            });
            match write_grade_preset_file(path, name, grade, export, *replace_lut) {
                Ok(exported) => PresetFileAnswer::Exported {
                    lut: exported.map(|exported| match exported {
                        ExportedGradeLut::Embedded => ExportedLut::Embedded,
                        ExportedGradeLut::Sibling { path, written } => {
                            ExportedLut::Sibling { path, written }
                        }
                    }),
                },
                Err(PresetFileError::LutFileExists { path }) => {
                    PresetFileAnswer::LutFileExists { path }
                }
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
        PresetFileRequest::ExportShortcuts {
            path,
            name,
            shortcuts,
        } => match write_shortcut_set_file(
            path,
            &ShortcutSetFile {
                name: name.clone(),
                shortcuts: shortcuts.clone(),
            },
        ) {
            Ok(()) => PresetFileAnswer::Exported { lut: None },
            Err(error) => failed(&error),
        },
        PresetFileRequest::ImportShortcuts { path } => match read_shortcut_set_file(path) {
            Ok(set) => PresetFileAnswer::Shortcuts(set.shortcuts),
            Err(error) => failed(&error),
        },
        // Console key profiles (RC-004) are plain text.
        PresetFileRequest::ExportKeyProfile { path, text } => match std::fs::write(path, text) {
            Ok(()) => PresetFileAnswer::Exported { lut: None },
            Err(error) => PresetFileAnswer::Failed(error.to_string()),
        },
        PresetFileRequest::ImportKeyProfile { path } => match read_key_profile(path) {
            Ok(text) => PresetFileAnswer::KeyProfile(text),
            Err(error) => PresetFileAnswer::Failed(error),
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
    fn shortcut_sets_round_trip_through_files() {
        let directory = scratch("shortcuts");
        let path = directory.join("desk.fmshortcuts");
        let shortcuts = vec![fm_protocol::ShortcutSpec {
            name: "Open".into(),
            enabled: true,
            key: Some(fm_protocol::WireShortcutKey {
                key: "F2".into(),
                command: true,
                shift: false,
                alt: false,
                then: None,
                scope: None,
            }),
            osc: Some("open".into()),
            action: fm_protocol::WireShortcutAction::Script { script: 4 },
        }];
        let export = PresetFileRequest::ExportShortcuts {
            path: path.clone(),
            name: "Desk".into(),
            shortcuts: shortcuts.clone(),
        };
        assert_eq!(
            carry_out(&export, None),
            PresetFileAnswer::Exported { lut: None }
        );
        assert_eq!(
            carry_out(&PresetFileRequest::ImportShortcuts { path }, None),
            PresetFileAnswer::Shortcuts(shortcuts)
        );
        let missing = directory.join("missing.fmshortcuts");
        assert!(matches!(
            carry_out(&PresetFileRequest::ImportShortcuts { path: missing }, None),
            PresetFileAnswer::Failed(_)
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn key_profiles_round_trip_through_files() {
        let directory = scratch("keys");
        let path = directory.join("booth.fmkeys");
        let text = "# FreeMix key profile\nname = Booth\nbase = vmix\n".to_owned();
        let export = PresetFileRequest::ExportKeyProfile {
            path: path.clone(),
            text: text.clone(),
        };
        assert_eq!(
            carry_out(&export, None),
            PresetFileAnswer::Exported { lut: None }
        );
        assert_eq!(
            carry_out(&PresetFileRequest::ImportKeyProfile { path }, None),
            PresetFileAnswer::KeyProfile(text)
        );
        let long = directory.join("long.fmkeys");
        std::fs::write(&long, "#".repeat(fm_ui_egui::MAX_KEY_FILE_BYTES + 1)).unwrap();
        assert!(matches!(
            carry_out(&PresetFileRequest::ImportKeyProfile { path: long }, None),
            PresetFileAnswer::Failed(_)
        ));
        let missing = directory.join("missing.fmkeys");
        assert!(matches!(
            carry_out(&PresetFileRequest::ImportKeyProfile { path: missing }, None),
            PresetFileAnswer::Failed(_)
        ));
        let _ = std::fs::remove_dir_all(directory);
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
            replace_lut: false,
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

    #[test]
    fn large_grade_luts_ask_before_replacing_an_existing_sibling() {
        let directory = scratch("grade-replace");
        let bundle = directory.join("show.freemix");
        std::fs::create_dir_all(bundle.join("assets/luts")).unwrap();
        let mut cube = "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n".to_owned();
        while cube.len() <= fm_preset_file::MAX_EMBEDDED_LUT_BYTES {
            cube.insert_str(0, "# padding past the embed limit\n");
        }
        std::fs::write(bundle.join("assets/luts/3.cube"), &cube).unwrap();
        let sibling = directory.join("big.cube");
        std::fs::write(&sibling, "unrelated").unwrap();
        let request = |replace_lut| PresetFileRequest::ExportGrade {
            path: directory.join("big.fmgrade"),
            name: "Big".to_owned(),
            grade: Box::new(Grade::IDENTITY),
            lut: Some(fm_ui_egui::ExportLut {
                lut: core::num::NonZeroU32::new(3).unwrap(),
                name: "Film".to_owned(),
                strength_permille: 1_000,
            }),
            replace_lut,
        };
        assert_eq!(
            carry_out(&request(false), Some(&bundle)),
            PresetFileAnswer::LutFileExists {
                path: sibling.clone()
            }
        );
        assert_eq!(std::fs::read_to_string(&sibling).unwrap(), "unrelated");
        assert!(!directory.join("big.fmgrade").exists());
        assert_eq!(
            carry_out(&request(true), Some(&bundle)),
            PresetFileAnswer::Exported {
                lut: Some(ExportedLut::Sibling {
                    path: sibling.clone(),
                    written: true,
                })
            }
        );
        assert_eq!(std::fs::read_to_string(&sibling).unwrap(), cube);
        let _ = std::fs::remove_dir_all(directory);
    }
}
