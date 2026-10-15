//! "My presets" (GX-011, Protocol 2.34): the project's reusable user
//! animation presets in the Advanced-mode inspector.
//!
//! The row saves the selected element's (or layer's) keys in the shown
//! phase as a named preset, and renames, deletes, exports, or imports the
//! preset picked in its list. The widget never talks to a server or reads a
//! file: each action becomes a [`LibraryRequest`] the caller takes from
//! [`super::AnimationTimelineState::library_requests`]. Saved presets also
//! appear in the Simple-mode picker under "My presets"
//! ([`super::tracks`]), previewed on hover like the built-ins.

use core::num::NonZeroU32;
use std::sync::Arc;

use egui::{ComboBox, TextEdit, Ui};
use fm_animation::{Phase, PhaseMotion};
use fm_protocol::{MAX_ANIMATION_PRESET_NAME_BYTES, MAX_ANIMATION_PRESETS};
use fm_ui_model::animation_editor::{AnimDocument, PresetSource};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::{AnimationTimelineState, text};
use crate::gating::Hint;
use crate::l10n;
use crate::theme::{section_label, small_button};

/// One preset of the project's library, as the caller replicates it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserPresetItem {
    pub preset: NonZeroU32,
    pub name: String,
    /// The phase it was saved from; the Simple picker offers it in that
    /// phase only.
    pub phase: Phase,
    pub motion: Arc<PhaseMotion>,
}

impl UserPresetItem {
    /// The preset as the picker and the document apply it.
    #[must_use]
    pub fn source(&self) -> PresetSource {
        PresetSource::User {
            preset: self.preset,
            name: self.name.clone(),
            motion: Arc::clone(&self.motion),
        }
    }
}

/// One "My presets" action for the caller to carry out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryRequest {
    /// Adds `motion` (saved from `phase`) to the library as `name`.
    Save {
        name: String,
        phase: Phase,
        motion: PhaseMotion,
    },
    Rename {
        preset: NonZeroU32,
        name: String,
    },
    Delete {
        preset: NonZeroU32,
    },
    /// Writes the preset to a `.fmpreset` file at `path`.
    Export {
        preset: NonZeroU32,
        path: String,
    },
    /// Reads a `.fmpreset` file and adds it to the library.
    Import {
        path: String,
    },
}

/// What the operator typed and picked in the row, kept between frames.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LibraryEdit {
    /// The name a save or rename uses.
    pub name: String,
    /// The preset rename, delete, and export act on.
    pub pick: Option<NonZeroU32>,
    /// The `.fmpreset` path export and import use.
    pub path: String,
}

/// Why `name` cannot name a preset (catalog English), if it cannot: empty,
/// too long or with control characters, or taken by another preset than
/// `except`.
#[must_use]
pub fn preset_name_problem(
    name: &str,
    presets: &[UserPresetItem],
    except: Option<NonZeroU32>,
) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some(Msg::AnimReasonPresetName.en())
    } else if name.len() > MAX_ANIMATION_PRESET_NAME_BYTES || name.chars().any(char::is_control) {
        Some(Msg::AnimReasonPresetNameInvalid.en())
    } else if presets
        .iter()
        .any(|item| item.name == name && Some(item.preset) != except)
    {
        Some(Msg::AnimReasonPresetNameTaken.en())
    } else {
        None
    }
}

/// The element a save captures: the target of the first selected key, else
/// of the active row.
pub(super) fn save_target<T: Copy + Ord>(
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
) -> Option<T> {
    document
        .selection()
        .iter()
        .next()
        .map(|key| key.track.target)
        .or_else(|| state.active_row.map(|row| row.target))
}

/// The "My presets" row.
#[allow(clippy::too_many_lines)] // one row: name, save, list, rename, delete, file
pub(super) fn draw<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    let library_lock = state.library_reason.map(|reason| l.tr(reason));
    let idle = document.drag().is_none();
    let busy = l.text(Msg::AnimReasonDrag);
    let phase = document.phase();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(section_label(l.text(Msg::AnimMyPresets)))
            .on_hover_text(l.text(Msg::AnimMyPresetsHover));
        ui.add_enabled(
            library_lock.is_none(),
            TextEdit::singleline(&mut state.library.name)
                .hint_text(l.text(Msg::AnimPresetNameHint))
                .desired_width(130.0),
        )
        .hint_in(
            l,
            l.text(Msg::AnimPresetNameHover),
            library_lock.unwrap_or_default(),
        );
        let typed = state.library.name.trim().to_owned();

        // Save the selected element's keys in the shown phase.
        let target = save_target(document, state);
        let target_name = target
            .and_then(|target| document.target(target))
            .map(|outline| outline.name.clone())
            .unwrap_or_default();
        let capture = target.map(|target| document.capture_motion(target, phase));
        let save_reason = library_lock
            .or_else(|| (!idle).then_some(busy))
            .or_else(|| match &capture {
                None => Some(l.text(Msg::AnimReasonNoSaveTarget)),
                Some(Err(_)) => Some(l.text(Msg::AnimReasonNothingToSave)),
                Some(Ok(_)) => None,
            })
            .or_else(|| {
                (state.user_presets.len() >= usize::from(MAX_ANIMATION_PRESETS))
                    .then(|| l.text(Msg::AnimReasonLibraryFull))
            })
            .or_else(|| preset_name_problem(&typed, &state.user_presets, None).map(|r| l.tr(r)));
        let save = ui
            .add_enabled(
                save_reason.is_none(),
                small_button(l.text(Msg::AnimPresetSave)),
            )
            .hint_in(
                l,
                &l.format(
                    Msg::AnimPresetSaveHover,
                    &[
                        ("name", Arg::Str(&target_name)),
                        ("phase", Arg::Str(text::phase_label(l, phase))),
                    ],
                ),
                save_reason.unwrap_or_default(),
            );
        if save.clicked()
            && let Some(Ok(motion)) = capture
        {
            state.library_requests.push(LibraryRequest::Save {
                name: typed.clone(),
                phase,
                motion,
            });
            state.library.name.clear();
        }

        ui.separator();
        // The library list: every preset, with the phase it was saved from.
        if state
            .library
            .pick
            .is_some_and(|pick| !state.user_presets.iter().any(|item| item.preset == pick))
        {
            state.library.pick = None;
        }
        let picked = state
            .library
            .pick
            .and_then(|pick| state.user_presets.iter().find(|item| item.preset == pick))
            .cloned();
        let empty = state.user_presets.is_empty();
        let mut chosen = None;
        let combo = ui
            .add_enabled_ui(!empty, |ui| {
                ComboBox::from_id_salt("anim-user-preset-pick")
                    .selected_text(picked.as_ref().map_or_else(
                        || l.text(Msg::AnimPresetLibraryPick).to_owned(),
                        |item| crate::theme::ellipsize(&item.name, 18),
                    ))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for item in &state.user_presets {
                            let label = l.format(
                                Msg::AnimPresetLibraryItem,
                                &[
                                    ("name", Arg::Str(&item.name)),
                                    ("phase", Arg::Str(text::phase_label(l, item.phase))),
                                ],
                            );
                            if ui
                                .selectable_label(picked.as_ref() == Some(item), label)
                                .on_hover_text(user_preset_hover(l, item))
                                .clicked()
                            {
                                chosen = Some(item.preset);
                            }
                        }
                    })
                    .response
            })
            .inner;
        let _ = combo.hint_in(
            l,
            l.text(Msg::AnimPresetLibraryPickHover),
            l.text(Msg::AnimReasonNoUserPresets),
        );
        if chosen.is_some() {
            state.library.pick = chosen;
        }
        let pick_reason = library_lock.or_else(|| {
            picked
                .is_none()
                .then(|| l.text(Msg::AnimReasonNoPresetPicked))
        });
        let rename_reason = pick_reason.or_else(|| {
            preset_name_problem(
                &typed,
                &state.user_presets,
                picked.as_ref().map(|item| item.preset),
            )
            .map(|r| l.tr(r))
        });
        let rename = ui
            .add_enabled(
                rename_reason.is_none(),
                small_button(l.text(Msg::AnimPresetRename)),
            )
            .hint_in(
                l,
                l.text(Msg::AnimPresetRenameHover),
                rename_reason.unwrap_or_default(),
            );
        if rename.clicked()
            && let Some(item) = &picked
        {
            state.library_requests.push(LibraryRequest::Rename {
                preset: item.preset,
                name: typed,
            });
            state.library.name.clear();
        }
        let delete = ui
            .add_enabled(
                pick_reason.is_none(),
                small_button(l.text(Msg::AnimPresetDelete)),
            )
            .hint_in(
                l,
                l.text(Msg::AnimPresetDeleteHover),
                pick_reason.unwrap_or_default(),
            );
        if delete.clicked()
            && let Some(item) = &picked
        {
            state.library_requests.push(LibraryRequest::Delete {
                preset: item.preset,
            });
            state.library.pick = None;
        }

        ui.separator();
        // Sharing through `.fmpreset` files.
        ui.add(
            TextEdit::singleline(&mut state.library.path)
                .hint_text(l.text(Msg::AnimPresetPathHint))
                .desired_width(180.0),
        )
        .on_hover_text(l.text(Msg::AnimPresetPathHover));
        let path = state.library.path.trim().to_owned();
        let no_path = path.is_empty().then(|| l.text(Msg::AnimReasonNoPath));
        let export_reason = picked
            .is_none()
            .then(|| l.text(Msg::AnimReasonNoPresetPicked))
            .or(no_path);
        let export = ui
            .add_enabled(
                export_reason.is_none(),
                small_button(l.text(Msg::AnimPresetExport)),
            )
            .hint_in(
                l,
                l.text(Msg::AnimPresetExportHover),
                export_reason.unwrap_or_default(),
            );
        if export.clicked()
            && let Some(item) = &picked
        {
            state.library_requests.push(LibraryRequest::Export {
                preset: item.preset,
                path: path.clone(),
            });
        }
        let import_reason = library_lock.or(no_path).or_else(|| {
            (state.user_presets.len() >= usize::from(MAX_ANIMATION_PRESETS))
                .then(|| l.text(Msg::AnimReasonLibraryFull))
        });
        let import = ui
            .add_enabled(
                import_reason.is_none(),
                small_button(l.text(Msg::AnimPresetImport)),
            )
            .hint_in(
                l,
                l.text(Msg::AnimPresetImportHover),
                import_reason.unwrap_or_default(),
            );
        if import.clicked() {
            state.library_requests.push(LibraryRequest::Import { path });
            state.library.path.clear();
        }
    });
}

/// The hover text of a user preset: its name, the phase it was saved from,
/// and its length.
#[must_use]
pub fn user_preset_hover(l: Locale, item: &UserPresetItem) -> String {
    l.format(
        Msg::AnimUserPresetHover,
        &[
            ("name", Arg::Str(&item.name)),
            ("phase", Arg::Str(text::phase_label(l, item.phase))),
            ("frames", Arg::Str(&l10n::frames(l, item.motion.frames()))),
        ],
    )
}
