//! Sharing presets between projects through files (GX-011, SW-019):
//! `.fmpreset` animation presets from the Animation window's "My presets"
//! row, `.fmgrade` grades from the Colour window, and `.fmshortcuts`
//! shortcut sets from the Shortcuts window (RC-004).
//!
//! This crate does no I/O: each export or import becomes a
//! [`PresetFileRequest`] the host takes with
//! [`StudioShell::take_preset_file_requests`], carries out (the native host
//! uses `fm-persistence`), and answers with
//! [`StudioShell::receive_preset_file`], which toasts the outcome and
//! returns the commands an import sends: `animation_preset_add` for a
//! preset, `grade_lut_import` plus `grade_library_add` for a grade, and
//! one `shortcut_set` per new or redefined shortcut for a shortcut set
//! (planned by `fm_preset_file::shortcuts::plan_shortcut_import`, which
//! refuses the whole set when a key or OSC name would clash; a key the
//! console keeps for itself is refused here too).
//! Imported names that are taken get a numeric suffix (`Name (2)`, and
//! `Name (3)` for a taken `Name (2)`), as `fm_preset_file::unused_name`
//! picks it. A grade export that would overwrite an existing sibling
//! `.cube` is answered with [`PresetFileAnswer::LutFileExists`]; the Colour
//! window then asks before exporting again with `replace_lut`.

use core::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use fm_animation::{Phase, PhaseMotion};
use fm_grade::{Grade, GradeLut};
use fm_preset_file::unused_name;
use fm_protocol::{CommandPayload, MAX_GRADE_LUT_IMPORT_BYTES, ShortcutSpec};
use fm_ui_model::grade_editor::{
    GradeCommand, library_name_problem, lut_name_problem, lut_uri, next_library_id, next_lut_id,
};
use fm_ui_model::i18n::{Arg, Msg};

use crate::toasts::ToastKind;
use crate::user_shortcuts::ShortcutIntent;
use crate::{StudioIntent, StudioShell, StudioUiState};

/// The LUT of a grade being exported, as the project registers it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportLut {
    /// The project LUT id; the host reads its `.cube` text from the
    /// project bundle when it can.
    pub lut: NonZeroU32,
    pub name: String,
    pub strength_permille: u16,
}

/// A preset file to write or read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetFileRequest {
    /// Writes one library preset to a `.fmpreset` file.
    ExportAnimation {
        path: PathBuf,
        name: String,
        phase: Phase,
        motion: Arc<PhaseMotion>,
    },
    /// Reads a `.fmpreset` file into the preset library.
    ImportAnimation { path: PathBuf },
    /// Writes a grade (without its LUT reference) and its LUT to a
    /// `.fmgrade` file. An existing sibling `.cube` is only overwritten
    /// with `replace_lut`, which the operator confirms.
    ExportGrade {
        path: PathBuf,
        name: String,
        grade: Box<Grade>,
        lut: Option<ExportLut>,
        replace_lut: bool,
    },
    /// Reads a `.fmgrade` file into the grade library.
    ImportGrade { path: PathBuf },
    /// Writes the project's shortcuts to a `.fmshortcuts` file (RC-004).
    ExportShortcuts {
        path: PathBuf,
        name: String,
        shortcuts: Vec<ShortcutSpec>,
    },
    /// Reads a `.fmshortcuts` file into the project's shortcuts.
    ImportShortcuts { path: PathBuf },
    /// Writes one console key profile's `.fmkeys` text (RC-004).
    ExportKeyProfile { path: PathBuf, text: String },
    /// Reads a `.fmkeys` file into the console's key profiles.
    ImportKeyProfile { path: PathBuf },
}

impl PresetFileRequest {
    /// The file the request names.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        match self {
            Self::ExportAnimation { path, .. }
            | Self::ImportAnimation { path }
            | Self::ExportGrade { path, .. }
            | Self::ImportGrade { path }
            | Self::ExportShortcuts { path, .. }
            | Self::ImportShortcuts { path }
            | Self::ExportKeyProfile { path, .. }
            | Self::ImportKeyProfile { path } => path,
        }
    }

    /// The toast that says the file failed.
    const fn failed(&self) -> Msg {
        match self {
            Self::ExportGrade { .. } | Self::ImportGrade { .. } => Msg::ColorGradeFileFailed,
            Self::ExportShortcuts { .. } | Self::ImportShortcuts { .. } => {
                Msg::UserShortcutFileFailed
            }
            Self::ExportKeyProfile { .. } | Self::ImportKeyProfile { .. } => {
                Msg::KeyProfilesFileFailed
            }
            Self::ExportAnimation { .. } | Self::ImportAnimation { .. } => {
                Msg::AnimPresetFileFailed
            }
        }
    }
}

/// Where an exported grade's LUT went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportedLut {
    /// Inside the `.fmgrade`.
    Embedded,
    /// In the `.cube` file `path` beside it; `written` is false when the
    /// host could not read the project's LUT and the operator must copy it
    /// there.
    Sibling { path: PathBuf, written: bool },
}

/// A LUT read with an imported grade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedLut {
    pub name: String,
    pub strength_permille: u16,
    /// The exact `.cube` text.
    pub text: String,
}

/// What the host did for a [`PresetFileRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetFileAnswer {
    /// An export was written; `lut` says where a grade's LUT went.
    Exported { lut: Option<ExportedLut> },
    /// A `.fmpreset` was read.
    Animation {
        name: String,
        phase: Phase,
        motion: PhaseMotion,
    },
    /// A `.fmgrade` was read (its grade without a LUT reference).
    Grade {
        name: String,
        grade: Box<Grade>,
        lut: Option<ImportedLut>,
    },
    /// A `.fmshortcuts` was read.
    Shortcuts(Vec<ShortcutSpec>),
    /// A `.fmkeys` file's text was read.
    KeyProfile(String),
    /// A grade export was refused because its LUT would overwrite the
    /// existing `.cube` file `path`; nothing was written.
    LutFileExists { path: PathBuf },
    /// The file could not be written or read, or does not decode.
    Failed(String),
}

/// A grade export waiting for the operator to replace or keep the existing
/// sibling `.cube` file `lut`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingLutReplace {
    /// The refused export, sent again with `replace_lut` on confirmation.
    pub(crate) request: PresetFileRequest,
    pub(crate) lut: PathBuf,
}

impl StudioShell {
    /// Takes the preset files the operator asked to export or import. The
    /// host carries out each and answers with
    /// [`Self::receive_preset_file`].
    pub fn take_preset_file_requests(&mut self) -> Vec<PresetFileRequest> {
        let mut requests = std::mem::take(&mut self.animation.file_requests);
        requests.append(&mut self.color.grade_file_requests);
        requests.append(&mut self.user_shortcuts.file_requests);
        requests.append(&mut self.key_profiles_window.file_requests);
        requests
    }

    /// Answers one [`PresetFileRequest`] against the replicated project in
    /// `state`: toasts the outcome and returns the commands an import
    /// sends.
    #[allow(clippy::too_many_lines)] // one arm per request and answer
    pub fn receive_preset_file(
        &mut self,
        state: &StudioUiState,
        request: &PresetFileRequest,
        answer: PresetFileAnswer,
    ) -> Vec<StudioIntent> {
        let l = self.locale;
        let file = request.path().display().to_string();
        let file_arg = ("file", Arg::Str(&file));
        let failed = request.failed();
        let mut intents = Vec::new();
        match (request, answer) {
            (_, PresetFileAnswer::Failed(error)) => {
                self.toasts.push(
                    ToastKind::Error,
                    l.format(failed, &[file_arg, ("error", Arg::Str(&error))]),
                );
            }
            (
                PresetFileRequest::ExportAnimation { name, .. },
                PresetFileAnswer::Exported { .. },
            ) => {
                self.toasts.push(
                    ToastKind::Success,
                    l.format(
                        Msg::AnimPresetExported,
                        &[("name", Arg::Str(name)), file_arg],
                    ),
                );
            }
            (PresetFileRequest::ExportGrade { name, .. }, PresetFileAnswer::Exported { lut }) => {
                let name_arg = ("name", Arg::Str(name));
                let (kind, message) = match lut {
                    None | Some(ExportedLut::Embedded) => (
                        ToastKind::Success,
                        l.format(Msg::ColorGradeExported, &[name_arg, file_arg]),
                    ),
                    Some(ExportedLut::Sibling { path, written }) => {
                        let lut = path.display().to_string();
                        let lut_arg = ("lut", Arg::Str(&lut));
                        if written {
                            (
                                ToastKind::Success,
                                l.format(
                                    Msg::ColorGradeExportedLut,
                                    &[name_arg, file_arg, lut_arg],
                                ),
                            )
                        } else {
                            (
                                ToastKind::Notice,
                                l.format(
                                    Msg::ColorGradeExportedLutMissing,
                                    &[name_arg, file_arg, lut_arg],
                                ),
                            )
                        }
                    }
                };
                self.toasts.push(kind, message);
            }
            (
                PresetFileRequest::ExportGrade { name, .. },
                PresetFileAnswer::LutFileExists { path },
            ) => {
                let lut = path.display().to_string();
                self.toasts.push(
                    ToastKind::Notice,
                    l.format(
                        Msg::ColorGradeLutExists,
                        &[("name", Arg::Str(name)), ("lut", Arg::Str(&lut))],
                    ),
                );
                self.color.lut_replace = Some(PendingLutReplace {
                    request: request.clone(),
                    lut: path,
                });
            }
            (
                PresetFileRequest::ImportAnimation { .. },
                PresetFileAnswer::Animation {
                    name,
                    phase,
                    motion,
                },
            ) => match self.import_animation_preset(state, &name, phase, motion) {
                Ok(intent) => {
                    self.toasts.push(
                        ToastKind::Pending,
                        l.format(
                            Msg::AnimPresetImporting,
                            &[("name", Arg::Str(&name)), file_arg],
                        ),
                    );
                    intents.push(intent);
                }
                Err(reason) => self.toasts.push(
                    ToastKind::Error,
                    l.format(failed, &[file_arg, ("error", Arg::Str(l.tr(reason)))]),
                ),
            },
            (
                PresetFileRequest::ImportGrade { .. },
                PresetFileAnswer::Grade { name, grade, lut },
            ) => match self.import_grade(state, &name, *grade, lut) {
                Ok(mut commands) => {
                    self.toasts.push(
                        ToastKind::Pending,
                        l.format(
                            Msg::ColorGradeImporting,
                            &[("name", Arg::Str(&name)), file_arg],
                        ),
                    );
                    intents.append(&mut commands);
                }
                Err(message) => self.toasts.push(ToastKind::Error, message),
            },
            (
                PresetFileRequest::ExportShortcuts { shortcuts, .. },
                PresetFileAnswer::Exported { .. },
            ) => {
                self.toasts.push(
                    ToastKind::Success,
                    l.format(
                        Msg::UserShortcutExported,
                        &[("count", Arg::Count(shortcuts.len() as u64)), file_arg],
                    ),
                );
            }
            (PresetFileRequest::ImportShortcuts { .. }, PresetFileAnswer::Shortcuts(imported)) => {
                match crate::user_shortcuts::import_plan(state, &imported) {
                    Ok(plan) => {
                        self.toasts.push(
                            ToastKind::Pending,
                            l.format(
                                Msg::UserShortcutImporting,
                                &[
                                    file_arg,
                                    ("added", Arg::Plain(plan.added as u64)),
                                    ("replaced", Arg::Plain(plan.replaced as u64)),
                                    ("unchanged", Arg::Plain(plan.unchanged as u64)),
                                ],
                            ),
                        );
                        intents.extend(plan.sets.into_iter().map(|set| {
                            StudioIntent::Shortcut(ShortcutIntent::Set {
                                shortcut: set.id,
                                spec: Box::new(set.spec),
                            })
                        }));
                    }
                    Err(error) => self.toasts.push(
                        ToastKind::Error,
                        l.format(failed, &[file_arg, ("error", Arg::Str(&error.text(l)))]),
                    ),
                }
            }
            (PresetFileRequest::ExportKeyProfile { .. }, PresetFileAnswer::Exported { .. }) => {
                self.toasts.push(
                    ToastKind::Success,
                    l.format(Msg::KeyProfilesExported, &[file_arg]),
                );
            }
            (PresetFileRequest::ImportKeyProfile { .. }, PresetFileAnswer::KeyProfile(text)) => {
                match self.key_profiles.import_text(&text) {
                    Ok(choice) => {
                        let name = self.key_profiles.name(l, choice);
                        self.key_profiles_window.selected = choice;
                        self.key_profiles_window.name.clone_from(&name);
                        self.toasts.push(
                            ToastKind::Success,
                            l.format(
                                Msg::KeyProfilesImported,
                                &[("name", Arg::Str(&name)), file_arg],
                            ),
                        );
                    }
                    Err(error) => {
                        let reason = match error {
                            crate::console_keymap::KeyFileError::Full => {
                                l.text(Msg::KeyProfilesReasonFull)
                            }
                            crate::console_keymap::KeyFileError::Invalid => {
                                l.text(Msg::KeyProfilesImportInvalid)
                            }
                        };
                        self.toasts.push(
                            ToastKind::Error,
                            l.format(failed, &[file_arg, ("error", Arg::Str(reason))]),
                        );
                    }
                }
            }
            // An answer of the wrong kind is the host's mistake.
            (_, _) => self.toasts.push(
                ToastKind::Error,
                l.format(
                    failed,
                    &[file_arg, ("error", Arg::Str("unexpected answer"))],
                ),
            ),
        }
        intents
    }

    /// The `animation_preset_add` for an imported preset.
    fn import_animation_preset(
        &mut self,
        state: &StudioUiState,
        name: &str,
        phase: Phase,
        motion: PhaseMotion,
    ) -> Result<StudioIntent, &'static str> {
        if let Some(reason) = crate::studio_animation::library_lock_reason(state) {
            return Err(reason);
        }
        let library = state
            .project_state
            .as_ref()
            .map(fm_ui_model::ProjectState::animation_presets)
            .ok_or(Msg::AnimReasonPresetsPending.en())?;
        if library.is_full() {
            return Err(Msg::AnimReasonLibraryFull.en());
        }
        let preset = self
            .animation
            .next_preset_id(library)
            .ok_or(Msg::AnimReasonLibraryFull.en())?;
        let name = unused_name(
            name,
            fm_protocol::MAX_ANIMATION_PRESET_NAME_BYTES,
            |candidate| library.by_name(candidate).is_some(),
        );
        Ok(StudioIntent::AnimationPreset(
            CommandPayload::AddAnimationPreset {
                preset,
                name,
                phase,
                motion: Box::new(motion),
                index: None,
            },
        ))
    }

    /// The `grade_lut_import` (when the grade has a LUT) and
    /// `grade_library_add` for an imported grade, or the toast text saying
    /// why it cannot be imported here.
    fn import_grade(
        &mut self,
        state: &StudioUiState,
        name: &str,
        mut grade: Grade,
        lut: Option<ImportedLut>,
    ) -> Result<Vec<StudioIntent>, String> {
        let l = self.locale;
        let refuse = |reason: &'static str| {
            l.format(
                Msg::ColorGradeFileFailed,
                &[("file", Arg::Str(name)), ("error", Arg::Str(l.tr(reason)))],
            )
        };
        if let Some(reason) = crate::studio_color::project_lock_reason(state) {
            return Err(refuse(reason));
        }
        let grading = state
            .project_state
            .as_ref()
            .filter(|project| project.grading_complete())
            .map(fm_ui_model::ProjectState::grading)
            .ok_or_else(|| refuse(Msg::ColorReasonGradingPending.en()))?;
        let grade_id =
            next_library_id(grading).ok_or_else(|| refuse(Msg::ColorReasonLibraryFull.en()))?;
        let mut intents = Vec::new();
        grade.lut = None;
        if let Some(lut) = lut {
            if lut.text.len() > MAX_GRADE_LUT_IMPORT_BYTES {
                return Err(l.format(Msg::ColorGradeLutTooLarge, &[("name", Arg::Str(name))]));
            }
            let id = next_lut_id(grading, self.color.lut_issued)
                .ok_or_else(|| refuse(Msg::ColorReasonLutsFull.en()))?;
            self.color.lut_issued = Some(id);
            let lut_name = unused_name(&lut.name, fm_protocol::MAX_GRADE_NAME_BYTES, |n| {
                lut_name_problem(n, grading).is_some()
            });
            grade.lut = Some(GradeLut {
                asset_uri: lut_uri(id),
                strength_permille: lut.strength_permille,
            });
            intents.push(StudioIntent::Grade(GradeCommand::ImportLut {
                lut: id,
                name: lut_name,
                data: lut.text,
            }));
        }
        let name = unused_name(name, fm_protocol::MAX_GRADE_NAME_BYTES, |n| {
            library_name_problem(n, grading).is_some()
        });
        intents.push(StudioIntent::Grade(GradeCommand::AddLibrary {
            grade: grade_id,
            name,
            value: Box::new(grade),
        }));
        Ok(intents)
    }
}

#[cfg(test)]
mod tests {
    use super::unused_name;

    #[test]
    fn imported_names_count_on_from_a_taken_suffix_within_the_bound() {
        let taken = ["Swoosh", "Swoosh (2)"];
        assert_eq!(unused_name("Fresh", 128, |n| taken.contains(&n)), "Fresh");
        assert_eq!(
            unused_name("Swoosh", 128, |n| taken.contains(&n)),
            "Swoosh (3)"
        );
        assert_eq!(
            unused_name(" Swoosh (2) ", 128, |n| taken.contains(&n)),
            "Swoosh (3)"
        );
        let long = "é".repeat(64); // 128 bytes
        let name = unused_name(&long, 128, |n| n == long);
        assert!(name.len() <= 128 && name.ends_with(" (2)"), "{name}");
    }
}
