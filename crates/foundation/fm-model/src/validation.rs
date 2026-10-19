use fm_types::{BusId, InputId, MAX_INPUT_NAME_BYTES, OutputId, PixelFormat, SceneId};

use crate::{
    CropRect, GradeSource, InputKind, Layer, LayerEffect, MAX_GRADE_NAME_BYTES, MAX_LUT_NAME_BYTES,
    MAX_PROJECT_GRADES, MAX_PROJECT_LAYER_GRADES, MAX_PROJECT_LUTS, MAX_STREAM_TARGET_NAME_BYTES,
    Project, ProjectGrading, ProjectSettings, RestartPolicy, SimulatedAudio, SourceRef,
    StreamTargetId, TitleFieldsError, TitleInput,
    cycles::{mark_alias_cycles, mark_audio_input_cycles, mark_bus_cycles, mark_scene_cycles},
    lut_id_from_uri, validate_title_fields,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityRef {
    MainMix,
    Input(InputId),
    Scene(SceneId),
    AudioBus(BusId),
    Output(OutputId),
    StreamTarget(StreamTargetId),
    Grade(crate::GradeId),
    Lut(crate::LutId),
    AnimationPreset(crate::AnimationPresetId),
    /// A project script (RC-011), by id.
    Script(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub entity: Option<EntityRef>,
    pub field: &'static str,
    pub kind: ValidationErrorKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationErrorKind {
    Empty,
    DuplicateId,
    DuplicateName,
    MissingReference(EntityRef),
    SelfReference,
    Cycle,
    MalformedCapabilityKey,
    DuplicateReference,
    OutOfRange,
    FormatMismatch,
    /// Entries that must be strictly ascending are not.
    OutOfOrder,
}

pub(crate) fn validate_project(project: &Project) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    require_name(&mut errors, None, "name", project.name());
    validate_settings(project, &mut errors);
    validate_unique_entities(project, &mut errors);
    validate_inputs(project, &mut errors);
    validate_input_audio_strips(project, &mut errors);
    validate_input_locks(project, &mut errors);
    validate_input_lifecycle(project, &mut errors);
    validate_playlist(project, &mut errors);
    validate_cue_lists(project, &mut errors);
    validate_scripts(project, &mut errors);
    validate_grading(project, &mut errors);
    validate_animation_presets(project, &mut errors);
    validate_main_mix(project, &mut errors);
    validate_stingers(project, &mut errors);
    validate_scenes(project, &mut errors);
    validate_buses(project, &mut errors);
    validate_outputs(project, &mut errors);
    validate_stream_targets(project, &mut errors);
    mark_scene_cycles(project, &mut errors);
    mark_alias_cycles(project, &mut errors);
    mark_audio_input_cycles(project, &mut errors);
    mark_bus_cycles(project, &mut errors);
    errors
}

fn validate_stingers(project: &Project, errors: &mut Vec<ValidationError>) {
    for (index, stinger) in project.stingers().iter().enumerate() {
        if project.stingers()[..index]
            .iter()
            .any(|previous| previous.slot == stinger.slot)
        {
            errors.push(ValidationError {
                entity: None,
                field: "stingers.slot",
                kind: ValidationErrorKind::DuplicateId,
            });
        }
        if !project
            .inputs()
            .iter()
            .any(|input| input.id == stinger.media_input)
        {
            errors.push(ValidationError {
                entity: None,
                field: "stingers.media_input",
                kind: ValidationErrorKind::MissingReference(EntityRef::Input(stinger.media_input)),
            });
        }
    }
}

fn validate_input_audio_strips(project: &Project, errors: &mut Vec<ValidationError>) {
    for input in project.inputs() {
        let count = project
            .input_audio_strips()
            .iter()
            .filter(|strip| strip.input == input.id)
            .count();
        if count != 1 {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input.id)),
                field: "audio_strip",
                kind: ValidationErrorKind::DuplicateReference,
            });
        }
    }
    for strip in project.input_audio_strips() {
        if !project.inputs().iter().any(|input| input.id == strip.input) {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(strip.input)),
                field: "audio_strip.input",
                kind: ValidationErrorKind::MissingReference(EntityRef::Input(strip.input)),
            });
        }
        if crate::validate_audio_strip_label(&strip.label).is_err() {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(strip.input)),
                field: "audio_strip.label",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
    }
    for (_, group) in project.audio_groups().iter() {
        if crate::validate_audio_group_name(&group.name).is_err() {
            errors.push(ValidationError {
                entity: None,
                field: "audio_groups.name",
                kind: if group.name.trim().is_empty() {
                    ValidationErrorKind::Empty
                } else {
                    ValidationErrorKind::OutOfRange
                },
            });
        }
    }
    if !project.audio_groups().is_acyclic() {
        errors.push(ValidationError {
            entity: None,
            field: "audio_groups.parent",
            kind: ValidationErrorKind::Cycle,
        });
    }
    if project
        .audio_monitor_output()
        .device
        .as_deref()
        .is_some_and(|device| crate::validate_audio_monitor_device(device).is_err())
    {
        errors.push(ValidationError {
            entity: None,
            field: "audio_monitor_output.device",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    validate_display_outputs(project, errors);
    validate_live_lan(project, errors);
    for &output in project.fade_to_black_excluded_outputs() {
        if !project
            .outputs()
            .iter()
            .any(|candidate| candidate.id == output)
        {
            errors.push(ValidationError {
                entity: None,
                field: "fade_to_black_excluded_outputs",
                kind: ValidationErrorKind::MissingReference(EntityRef::Output(output)),
            });
        }
    }
    for (&output, &duration) in project.fade_to_black_output_durations() {
        if !project
            .outputs()
            .iter()
            .any(|candidate| candidate.id == output)
        {
            errors.push(ValidationError {
                entity: None,
                field: "fade_to_black_output_durations",
                kind: ValidationErrorKind::MissingReference(EntityRef::Output(output)),
            });
        }
        if duration == 0 || duration > fm_types::MAX_FADE_TO_BLACK_DURATION_FRAMES {
            errors.push(ValidationError {
                entity: None,
                field: "fade_to_black_output_durations",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
    }
}

fn validate_input_locks(project: &Project, errors: &mut Vec<ValidationError>) {
    for &input in project.locked_inputs() {
        if !project
            .inputs()
            .iter()
            .any(|candidate| candidate.id == input)
        {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input)),
                field: "locked_inputs",
                kind: ValidationErrorKind::MissingReference(EntityRef::Input(input)),
            });
        }
    }
}

/// Validates the category list and every lifecycle entry's input and
/// category reference (IN-026).
fn validate_input_lifecycle(project: &Project, errors: &mut Vec<ValidationError>) {
    use crate::InputLifecycleError as Error;
    if let Err(error) = crate::validate_input_categories(project.input_categories()) {
        let (field, kind) = match error {
            Error::DuplicateCategoryId(_) => {
                ("input_categories.id", ValidationErrorKind::DuplicateId)
            }
            Error::DuplicateCategoryName(_) => {
                ("input_categories.name", ValidationErrorKind::DuplicateName)
            }
            Error::EmptyCategoryName => ("input_categories.name", ValidationErrorKind::Empty),
            Error::CategoryNameTooLong { .. } | Error::CategoryNameControlCharacter => {
                ("input_categories.name", ValidationErrorKind::OutOfRange)
            }
            Error::TooManyCategories { .. }
            | Error::UnknownInput(_)
            | Error::UnknownCategory(_)
            | Error::CategoryInUse { .. } => ("input_categories", ValidationErrorKind::OutOfRange),
        };
        push_error(errors, None, field, kind);
    }
    for (input, lifecycle) in project.input_lifecycles() {
        let entity = Some(EntityRef::Input(*input));
        if !project
            .inputs()
            .iter()
            .any(|candidate| candidate.id == *input)
        {
            push_error(
                errors,
                entity,
                "input_lifecycle.input",
                ValidationErrorKind::MissingReference(EntityRef::Input(*input)),
            );
        }
        if let Some(category) = lifecycle.category
            && !project
                .input_categories()
                .iter()
                .any(|candidate| candidate.id == category)
        {
            push_error(
                errors,
                entity,
                "input_lifecycle.category",
                ValidationErrorKind::OutOfRange,
            );
        }
    }
}

fn validate_playlist(project: &Project, errors: &mut Vec<ValidationError>) {
    let playlist = project.playlist();
    if let Err(error) = crate::validate_playlist_entries(&playlist.entries) {
        errors.push(ValidationError {
            entity: None,
            field: "playlist.entries",
            kind: match error {
                crate::PlaylistEntryError::DuplicateEntry(_) => ValidationErrorKind::DuplicateId,
                crate::PlaylistEntryError::TooManyEntries { .. }
                | crate::PlaylistEntryError::InvalidDuration(_)
                | crate::PlaylistEntryError::InvalidTransition(_) => {
                    ValidationErrorKind::OutOfRange
                }
            },
        });
    }
    for entry in &playlist.entries {
        if !project
            .inputs()
            .iter()
            .any(|candidate| candidate.id == entry.input)
        {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(entry.input)),
                field: "playlist.entries.input",
                kind: ValidationErrorKind::MissingReference(EntityRef::Input(entry.input)),
            });
        }
    }
    if playlist.next_take == 0 {
        errors.push(ValidationError {
            entity: None,
            field: "playlist.next_take",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

fn validate_scripts(project: &Project, errors: &mut Vec<ValidationError>) {
    if let Err(error) = crate::validate_scripts(project.scripts()) {
        errors.push(ValidationError {
            entity: None,
            field: "scripts",
            kind: match error {
                crate::ScriptError::DuplicateId(_) => ValidationErrorKind::DuplicateId,
                crate::ScriptError::InvalidName => ValidationErrorKind::Empty,
                crate::ScriptError::InvalidId
                | crate::ScriptError::InvalidSource
                | crate::ScriptError::TooManyScripts => ValidationErrorKind::OutOfRange,
            },
        });
    }
}

fn validate_cue_lists(project: &Project, errors: &mut Vec<ValidationError>) {
    let lists = project.cue_lists();
    if let Err(error) = crate::validate_cue_lists(lists) {
        errors.push(ValidationError {
            entity: None,
            field: "cue_lists",
            kind: match error {
                None | Some(crate::CueListError::DuplicateCue(_)) => {
                    ValidationErrorKind::DuplicateId
                }
                Some(crate::CueListError::InvalidName { .. }) => ValidationErrorKind::Empty,
                Some(_) => ValidationErrorKind::OutOfRange,
            },
        });
    }
    for action in lists
        .iter()
        .flat_map(|list| &list.cues)
        .flat_map(|cue| &cue.actions)
    {
        if let Some(input) = action.input() {
            let found = project
                .inputs()
                .iter()
                .find(|candidate| candidate.id == input);
            let is_title = matches!(
                action,
                crate::CueAction::TitleOn { .. }
                    | crate::CueAction::TitleText { .. }
                    | crate::CueAction::TitleOff { .. }
            );
            let is_media = matches!(action, crate::CueAction::Media { .. });
            match found {
                None => errors.push(ValidationError {
                    entity: Some(EntityRef::Input(input)),
                    field: "cue_lists.cues.actions.input",
                    kind: ValidationErrorKind::MissingReference(EntityRef::Input(input)),
                }),
                Some(found)
                    if (is_title && !matches!(found.kind, InputKind::Title(_)))
                        || (is_media && !matches!(found.kind, InputKind::Media { .. })) =>
                {
                    errors.push(ValidationError {
                        entity: Some(EntityRef::Input(input)),
                        field: "cue_lists.cues.actions.input",
                        kind: ValidationErrorKind::FormatMismatch,
                    });
                }
                Some(_) => {}
            }
        }
        if let crate::CueAction::Stream { target, .. } = action
            && !project
                .stream_targets()
                .iter()
                .any(|candidate| candidate.id() == *target)
        {
            errors.push(ValidationError {
                entity: Some(EntityRef::StreamTarget(*target)),
                field: "cue_lists.cues.actions.target",
                kind: ValidationErrorKind::MissingReference(EntityRef::StreamTarget(*target)),
            });
        }
        if let Some(script) = action.script()
            && !project.scripts().iter().any(|found| found.id == script)
        {
            errors.push(ValidationError {
                entity: Some(EntityRef::Script(script)),
                field: "cue_lists.cues.actions.script",
                kind: ValidationErrorKind::MissingReference(EntityRef::Script(script)),
            });
        }
    }
}

fn validate_grading(project: &Project, errors: &mut Vec<ValidationError>) {
    let grading = project.grading();
    if grading.luts.len() > MAX_PROJECT_LUTS {
        errors.push(ValidationError {
            entity: None,
            field: "luts",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    for (index, lut) in grading.luts.iter().enumerate() {
        let entity = Some(EntityRef::Lut(lut.id));
        let previous = &grading.luts[..index];
        if previous.iter().any(|other| other.id == lut.id) {
            push_error(errors, entity, "luts.id", ValidationErrorKind::DuplicateId);
        }
        if previous.iter().any(|other| other.name == lut.name) {
            push_error(
                errors,
                entity,
                "luts.name",
                ValidationErrorKind::DuplicateName,
            );
        }
        if let Some(kind) = bounded_name_error(&lut.name, MAX_LUT_NAME_BYTES) {
            push_error(errors, entity, "luts.name", kind);
        }
        if !lut.size_in_bounds() {
            push_error(errors, entity, "luts.size", ValidationErrorKind::OutOfRange);
        }
    }
    if grading.grades.len() > MAX_PROJECT_GRADES {
        push_error(errors, None, "grades", ValidationErrorKind::OutOfRange);
    }
    for (index, entry) in grading.grades.iter().enumerate() {
        let entity = Some(EntityRef::Grade(entry.id));
        let previous = &grading.grades[..index];
        if previous.iter().any(|other| other.id == entry.id) {
            push_error(
                errors,
                entity,
                "grades.id",
                ValidationErrorKind::DuplicateId,
            );
        }
        if previous.iter().any(|other| other.name == entry.name) {
            push_error(
                errors,
                entity,
                "grades.name",
                ValidationErrorKind::DuplicateName,
            );
        }
        if let Some(kind) = bounded_name_error(&entry.name, MAX_GRADE_NAME_BYTES) {
            push_error(errors, entity, "grades.name", kind);
        }
        validate_grade_values(errors, grading, &entry.grade, entity, "grades.grade");
    }
    for (input, source) in &grading.inputs {
        let entity = Some(EntityRef::Input(*input));
        if !project
            .inputs()
            .iter()
            .any(|candidate| candidate.id == *input)
        {
            push_error(
                errors,
                entity,
                "input_grades.input",
                ValidationErrorKind::MissingReference(EntityRef::Input(*input)),
            );
        }
        validate_grade_source(errors, grading, source, entity, "input_grades.grade");
    }
    validate_layer_grades(project, errors);
    if let Some(source) = &grading.program {
        validate_grade_source(errors, grading, source, None, "program_grade");
    }
}

/// Validates the animation preset library: its bound, unique ids and
/// names, and every name. Each preset's motion is valid by construction.
fn validate_animation_presets(project: &Project, errors: &mut Vec<ValidationError>) {
    use crate::AnimationPresetError as Error;
    for (id, error) in project.animation_presets().violations() {
        let (field, kind) = match error {
            Error::TooManyPresets { .. } => ("animation_presets", ValidationErrorKind::OutOfRange),
            Error::DuplicateId(_) => ("animation_presets.id", ValidationErrorKind::DuplicateId),
            Error::DuplicateName(_) => {
                ("animation_presets.name", ValidationErrorKind::DuplicateName)
            }
            Error::EmptyName => ("animation_presets.name", ValidationErrorKind::Empty),
            Error::NameTooLong { .. }
            | Error::NameControlCharacter
            | Error::UnknownPreset(_)
            | Error::IndexOutOfRange { .. } => {
                ("animation_presets.name", ValidationErrorKind::OutOfRange)
            }
        };
        push_error(errors, Some(EntityRef::AnimationPreset(id)), field, kind);
    }
}

/// Validates the scene layer grades: the bound, that each scene and its
/// layer exist, and each grade source.
fn validate_layer_grades(project: &Project, errors: &mut Vec<ValidationError>) {
    let grading = project.grading();
    if grading.layers.len() > MAX_PROJECT_LAYER_GRADES {
        push_error(
            errors,
            None,
            "layer_grades",
            ValidationErrorKind::OutOfRange,
        );
    }
    for (target, source) in &grading.layers {
        let entity = Some(EntityRef::Scene(target.scene));
        match project.scene(target.scene) {
            None => push_error(
                errors,
                entity,
                "layer_grades.scene",
                ValidationErrorKind::MissingReference(EntityRef::Scene(target.scene)),
            ),
            Some(scene) if scene.layer_by_id(target.layer).is_none() => push_error(
                errors,
                entity,
                "layer_grades.layer",
                ValidationErrorKind::MissingReference(EntityRef::Scene(target.scene)),
            ),
            Some(_) => {}
        }
        validate_grade_source(errors, grading, source, entity, "layer_grades.grade");
    }
}

fn validate_grade_source(
    errors: &mut Vec<ValidationError>,
    grading: &ProjectGrading,
    source: &GradeSource,
    entity: Option<EntityRef>,
    field: &'static str,
) {
    match source {
        GradeSource::Library(id) => {
            if grading.grade(*id).is_none() {
                push_error(
                    errors,
                    entity,
                    field,
                    ValidationErrorKind::MissingReference(EntityRef::Grade(*id)),
                );
            }
        }
        GradeSource::Inline(grade) => validate_grade_values(errors, grading, grade, entity, field),
    }
}

fn validate_grade_values(
    errors: &mut Vec<ValidationError>,
    grading: &ProjectGrading,
    grade: &fm_grade::Grade,
    entity: Option<EntityRef>,
    field: &'static str,
) {
    if grade.validate().is_err() {
        push_error(errors, entity, field, ValidationErrorKind::OutOfRange);
    }
    if let Some(lut) = &grade.lut {
        match lut_id_from_uri(&lut.asset_uri) {
            None => push_error(errors, entity, field, ValidationErrorKind::FormatMismatch),
            Some(id) if grading.lut(id).is_none() => push_error(
                errors,
                entity,
                field,
                ValidationErrorKind::MissingReference(EntityRef::Lut(id)),
            ),
            Some(_) => {}
        }
    }
}

fn bounded_name_error(name: &str, maximum: usize) -> Option<ValidationErrorKind> {
    if name.trim().is_empty() {
        Some(ValidationErrorKind::Empty)
    } else if name.len() > maximum || name.chars().any(char::is_control) {
        Some(ValidationErrorKind::OutOfRange)
    } else {
        None
    }
}

fn push_error(
    errors: &mut Vec<ValidationError>,
    entity: Option<EntityRef>,
    field: &'static str,
    kind: ValidationErrorKind,
) {
    errors.push(ValidationError {
        entity,
        field,
        kind,
    });
}

fn validate_settings(project: &Project, errors: &mut Vec<ValidationError>) {
    let settings = project.settings();
    let frame_rate = settings.frame_rate;
    let numerator = u64::from(frame_rate.numerator());
    let denominator = u64::from(frame_rate.denominator());
    if numerator < u64::from(ProjectSettings::MIN_FRAME_RATE_FPS) * denominator
        || numerator > u64::from(ProjectSettings::MAX_FRAME_RATE_FPS) * denominator
    {
        errors.push(ValidationError {
            entity: None,
            field: "settings.frame_rate",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    if settings.video.frame_rate != frame_rate {
        errors.push(ValidationError {
            entity: None,
            field: "settings.video.frame_rate",
            kind: ValidationErrorKind::FormatMismatch,
        });
    }
    if settings.video.dimensions.width() > ProjectSettings::MAX_VIDEO_WIDTH
        || settings.video.dimensions.height() > ProjectSettings::MAX_VIDEO_HEIGHT
    {
        errors.push(ValidationError {
            entity: None,
            field: "settings.video.dimensions",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    // The canvas may be any orientation (landscape, portrait, square), but a
    // chroma-subsampled program format needs whole chroma samples: 4:2:0
    // formats require an even width and height, 4:2:2 an even width.
    let (width, height) = (
        settings.video.dimensions.width(),
        settings.video.dimensions.height(),
    );
    let subsampling_fits = match settings.video.pixel_format {
        PixelFormat::Nv12 | PixelFormat::P010 => {
            width.is_multiple_of(2) && height.is_multiple_of(2)
        }
        PixelFormat::Yuv422 => width.is_multiple_of(2),
        PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::Rgba16Float => true,
    };
    if !subsampling_fits {
        errors.push(ValidationError {
            entity: None,
            field: "settings.video.dimensions",
            kind: ValidationErrorKind::FormatMismatch,
        });
    }

    let sample_rate = settings.audio.sample_rate.hertz();
    if !(ProjectSettings::MIN_AUDIO_SAMPLE_RATE_HZ..=ProjectSettings::MAX_AUDIO_SAMPLE_RATE_HZ)
        .contains(&sample_rate)
    {
        errors.push(ValidationError {
            entity: None,
            field: "settings.audio.sample_rate",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    let channels = settings.audio.channels.channels();
    if channels.len() > ProjectSettings::MAX_AUDIO_CHANNELS {
        errors.push(ValidationError {
            entity: None,
            field: "settings.audio.channels",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    if channels
        .iter()
        .enumerate()
        .any(|(index, channel)| channels[..index].contains(channel))
    {
        errors.push(ValidationError {
            entity: None,
            field: "settings.audio.channels",
            kind: ValidationErrorKind::DuplicateReference,
        });
    }

    if let RestartPolicy::OnFailure { max_attempts } = project.restart_policy()
        && !(1..=RestartPolicy::MAX_RESTART_ATTEMPTS).contains(&max_attempts)
    {
        errors.push(ValidationError {
            entity: None,
            field: "restart_policy.max_attempts",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

fn validate_unique_entities(project: &Project, errors: &mut Vec<ValidationError>) {
    duplicates(
        errors,
        project
            .inputs()
            .iter()
            .map(|input| (input.id, input.name.as_str(), EntityRef::Input(input.id))),
        true,
    );
    duplicates(
        errors,
        project
            .scenes()
            .iter()
            .map(|scene| (scene.id, scene.name.as_str(), EntityRef::Scene(scene.id))),
        false,
    );
    duplicates(
        errors,
        project
            .audio_buses()
            .iter()
            .map(|bus| (bus.id, bus.name.as_str(), EntityRef::AudioBus(bus.id))),
        false,
    );
    duplicates(
        errors,
        project.outputs().iter().map(|output| {
            (
                output.id,
                output.name.as_str(),
                EntityRef::Output(output.id),
            )
        }),
        false,
    );
    duplicates(
        errors,
        project.stream_targets().iter().map(|target| {
            (
                target.id(),
                target.name(),
                EntityRef::StreamTarget(target.id()),
            )
        }),
        false,
    );
}

/// A simulated input's sine stays inside the audible range and below the
/// project's Nyquist frequency.
fn validate_simulated_audio(
    project: &Project,
    entity: Option<EntityRef>,
    audio: SimulatedAudio,
    errors: &mut Vec<ValidationError>,
) {
    if let SimulatedAudio::Sine { frequency_hz } = audio {
        let nyquist_hz = project.settings().audio.sample_rate.hertz() / 2;
        if !(SimulatedAudio::MIN_SINE_FREQUENCY_HZ
            ..=SimulatedAudio::MAX_SINE_FREQUENCY_HZ.min(nyquist_hz))
            .contains(&frequency_hz)
        {
            errors.push(ValidationError {
                entity,
                field: "kind.simulated.audio.frequency_hz",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
    }
}

/// A media input's playback settings (schema 36, IN-003).
fn validate_media_playback(
    entity: Option<EntityRef>,
    playback: &crate::MediaPlaybackSettings,
    errors: &mut Vec<ValidationError>,
) {
    if let Err(error) = playback.validate() {
        errors.push(ValidationError {
            entity,
            field: match error.field() {
                "trim" => "kind.media.trim",
                "start_frame" => "kind.media.start_frame",
                "speed_basis_points" => "kind.media.speed_basis_points",
                "end_trigger_lead" => "kind.media.end_trigger_lead",
                "live_edge_offset_frames" => "kind.media.live_edge_offset_frames",
                _ => "kind.media.idle_completion_refreshes",
            },
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

fn validate_inputs(project: &Project, errors: &mut Vec<ValidationError>) {
    for input in project.inputs() {
        let entity = Some(EntityRef::Input(input.id));
        require_name(errors, entity, "name", &input.name);
        if input.name.len() > MAX_INPUT_NAME_BYTES {
            errors.push(ValidationError {
                entity,
                field: "name",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
        match &input.kind {
            // A mix feed reads only the previous tick's committed bus frame,
            // so it has no reference to validate and adds no graph edge.
            InputKind::Color | InputKind::MixFeed(_) => {}
            // A title is a leaf: its template is a bundle asset checked by
            // persistence, so only its own field list is validated here.
            InputKind::Title(title) => validate_title(entity, title, errors),
            InputKind::Media {
                asset_uri,
                playback,
            } => {
                require_name(errors, entity, "asset_uri", asset_uri);
                validate_media_playback(entity, playback, errors);
            }
            InputKind::Device {
                stable_key,
                capture,
            } => validate_capture_input(entity, stable_key, capture, errors),
            InputKind::NetworkStream(config) => validate_network_stream(entity, config, errors),
            InputKind::SystemAudio(config) => validate_system_audio(entity, config, errors),
            InputKind::Scene {
                scene_id,
                audio_source,
            } => {
                if !project.scenes().iter().any(|scene| scene.id == *scene_id) {
                    errors.push(ValidationError {
                        entity,
                        field: "kind.scene.scene_id",
                        kind: ValidationErrorKind::MissingReference(EntityRef::Scene(*scene_id)),
                    });
                }
                if let Some(audio_source) = audio_source {
                    if *audio_source == input.id {
                        errors.push(ValidationError {
                            entity,
                            field: "kind.scene.audio_source",
                            kind: ValidationErrorKind::SelfReference,
                        });
                    } else if !project
                        .inputs()
                        .iter()
                        .any(|other| other.id == *audio_source)
                    {
                        errors.push(ValidationError {
                            entity,
                            field: "kind.scene.audio_source",
                            kind: ValidationErrorKind::MissingReference(EntityRef::Input(
                                *audio_source,
                            )),
                        });
                    }
                }
            }
            InputKind::Alias { target, crop } => {
                if *target == input.id {
                    errors.push(ValidationError {
                        entity,
                        field: "kind.alias.target",
                        kind: ValidationErrorKind::SelfReference,
                    });
                } else if !project.inputs().iter().any(|other| other.id == *target) {
                    errors.push(ValidationError {
                        entity,
                        field: "kind.alias.target",
                        kind: ValidationErrorKind::MissingReference(EntityRef::Input(*target)),
                    });
                }
                if crop.is_some_and(|crop| !crop_fits_project(project, crop)) {
                    errors.push(ValidationError {
                        entity,
                        field: "kind.alias.crop",
                        kind: ValidationErrorKind::OutOfRange,
                    });
                }
            }
            InputKind::Simulated(simulated) => {
                validate_simulated_audio(project, entity, simulated.audio, errors);
            }
        }
        validate_capabilities(
            errors,
            entity,
            input.required_capabilities.iter().map(String::as_str),
        );
    }
    validate_network_stream_count(project, errors);
    validate_network_stream_listen_ports(project, errors);
    validate_capture_input_count(project, errors);
    validate_system_audio_count(project, errors);
}

/// AU-011: a system audio input names a usable device.
fn validate_system_audio(
    entity: Option<EntityRef>,
    config: &fm_types::system_audio::SystemAudioConfig,
    errors: &mut Vec<ValidationError>,
) {
    if config.validate().is_err() {
        errors.push(ValidationError {
            entity,
            field: "kind.system_audio.device",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// AU-011: a bounded number of system audio inputs keeps every snapshot
/// that carries them inside one line.
fn validate_system_audio_count(project: &Project, errors: &mut Vec<ValidationError>) {
    if let Some(input) = project
        .inputs()
        .iter()
        .filter(|input| matches!(input.kind, InputKind::SystemAudio(_)))
        .nth(fm_types::system_audio::MAX_SYSTEM_AUDIO_INPUTS)
    {
        errors.push(ValidationError {
            entity: Some(EntityRef::Input(input.id)),
            field: "kind.system_audio",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// IN-016/IN-017: two network stream inputs never listen on the same local
/// port (an RTMP listener's TCP port, or an SRT listener's, rendezvous's,
/// or local UDP/RTP receiver's UDP port).
fn validate_network_stream_listen_ports(project: &Project, errors: &mut Vec<ValidationError>) {
    let mut bound = std::collections::BTreeSet::new();
    for input in project.inputs() {
        if let InputKind::NetworkStream(config) = &input.kind
            && let Some(endpoint) = config.listen_endpoint()
            && !bound.insert(endpoint)
        {
            errors.push(ValidationError {
                entity: Some(EntityRef::Input(input.id)),
                field: "kind.network_stream.url",
                kind: ValidationErrorKind::DuplicateReference,
            });
        }
    }
}

/// A capture input's stable key and settings are usable (IN-001).
fn validate_capture_input(
    entity: Option<EntityRef>,
    stable_key: &str,
    capture: &fm_types::capture::CaptureSettings,
    errors: &mut Vec<ValidationError>,
) {
    if stable_key.trim().is_empty() {
        errors.push(ValidationError {
            entity,
            field: "stable_key",
            kind: ValidationErrorKind::Empty,
        });
    } else if fm_types::capture::validate_stable_key(stable_key).is_err() {
        errors.push(ValidationError {
            entity,
            field: "stable_key",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    if let Err(error) = capture.validate() {
        errors.push(ValidationError {
            entity,
            field: match error {
                fm_types::capture::CaptureError::AlsaDevice
                | fm_types::capture::CaptureError::Token => "kind.device.capture.audio",
                _ => "kind.device.capture.format",
            },
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// IN-001: a bounded number of capture inputs keeps every snapshot that
/// carries them inside one line.
fn validate_capture_input_count(project: &Project, errors: &mut Vec<ValidationError>) {
    if let Some(input) = project
        .inputs()
        .iter()
        .filter(|input| matches!(input.kind, InputKind::Device { .. }))
        .nth(fm_types::capture::MAX_CAPTURE_INPUTS)
    {
        errors.push(ValidationError {
            entity: Some(EntityRef::Input(input.id)),
            field: "kind.device",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// A network stream input's configuration is usable (IN-015).
fn validate_network_stream(
    entity: Option<EntityRef>,
    config: &fm_types::network_stream::NetworkStreamConfig,
    errors: &mut Vec<ValidationError>,
) {
    if let Err(error) = config.validate() {
        errors.push(ValidationError {
            entity,
            field: match error.field() {
                "url" => "kind.network_stream.url",
                "transport" => "kind.network_stream.transport",
                "jitter_ms" => "kind.network_stream.jitter_ms",
                "backoff" => "kind.network_stream.backoff",
                "give_up" => "kind.network_stream.give_up",
                "mode" => "kind.network_stream.mode",
                "latency_ms" => "kind.network_stream.latency_ms",
                "stream_key" => "kind.network_stream.stream_key",
                "passphrase" => "kind.network_stream.passphrase",
                "stream_id" => "kind.network_stream.stream_id",
                _ => "kind.network_stream.timeout_ms",
            },
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// IN-015: a bounded number of network streams keeps every snapshot that
/// carries them inside one line.
fn validate_network_stream_count(project: &Project, errors: &mut Vec<ValidationError>) {
    if let Some(input) = project
        .inputs()
        .iter()
        .filter(|input| matches!(input.kind, InputKind::NetworkStream(_)))
        .nth(fm_types::network_stream::MAX_NETWORK_STREAM_INPUTS)
    {
        errors.push(ValidationError {
            entity: Some(EntityRef::Input(input.id)),
            field: "kind.network_stream",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

/// A crop is nonempty and lies inside a project-sized source frame.
fn crop_fits_project(project: &Project, crop: CropRect) -> bool {
    let dimensions = project.settings().video.dimensions;
    crop.width != 0
        && crop.height != 0
        && crop
            .x
            .checked_add(crop.width)
            .is_some_and(|right| right <= dimensions.width())
        && crop
            .y
            .checked_add(crop.height)
            .is_some_and(|bottom| bottom <= dimensions.height())
}

fn validate_main_mix(project: &Project, errors: &mut Vec<ValidationError>) {
    let Some(main_mix) = project.main_mix() else {
        return;
    };
    let entity = Some(EntityRef::MainMix);
    for (field, id) in [
        ("main_mix.desired_program", main_mix.desired_program),
        ("main_mix.desired_preview", main_mix.desired_preview),
    ] {
        if !project.inputs().iter().any(|input| input.id == id) {
            errors.push(ValidationError {
                entity,
                field,
                kind: ValidationErrorKind::MissingReference(EntityRef::Input(id)),
            });
        }
    }
    if main_mix.desired_program == main_mix.desired_preview {
        errors.push(ValidationError {
            entity,
            field: "main_mix.desired_preview",
            kind: ValidationErrorKind::DuplicateReference,
        });
    }
}

#[allow(clippy::too_many_lines)] // one pass over every scene rule, layer ids and animation included
fn validate_scenes(project: &Project, errors: &mut Vec<ValidationError>) {
    for scene in project.scenes() {
        let entity = Some(EntityRef::Scene(scene.id));
        require_name(errors, entity, "name", &scene.name);
        if !scene.background.is_premultiplied() {
            errors.push(ValidationError {
                entity,
                field: "background",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
        for (index, layer) in scene.layers.iter().enumerate() {
            if scene.layers[..index]
                .iter()
                .any(|previous| previous.id == layer.id)
            {
                errors.push(ValidationError {
                    entity,
                    field: "layers.id",
                    kind: ValidationErrorKind::DuplicateId,
                });
            }
        }
        if let Err(error) = scene.check_animation_clip(&scene.animation.clip) {
            errors.push(ValidationError {
                entity,
                field: "animation",
                kind: match error {
                    crate::SceneAnimationError::UnknownLayer(_) => {
                        ValidationErrorKind::MissingReference(EntityRef::Scene(scene.id))
                    }
                    _ => ValidationErrorKind::OutOfRange,
                },
            });
        }
        for layer in &scene.layers {
            require_name(errors, entity, "layers.name", &layer.name);
            if !layer.geometry.is_storable() {
                errors.push(ValidationError {
                    entity,
                    field: "layers.geometry",
                    kind: ValidationErrorKind::OutOfRange,
                });
            }
            if layer
                .crop
                .is_some_and(|crop| !crop_fits_project(project, crop))
            {
                errors.push(ValidationError {
                    entity,
                    field: "layers.crop",
                    kind: ValidationErrorKind::OutOfRange,
                });
            }
            let (source_width, source_height) = layer.crop.map_or(
                (
                    project.settings().video.dimensions.width(),
                    project.settings().video.dimensions.height(),
                ),
                |crop| (crop.width, crop.height),
            );
            if layer.mask.is_some_and(|mask| {
                mask.width == 0
                    || mask.height == 0
                    || mask
                        .x
                        .checked_add(mask.width)
                        .is_none_or(|right| right > source_width)
                    || mask
                        .y
                        .checked_add(mask.height)
                        .is_none_or(|bottom| bottom > source_height)
            }) {
                errors.push(ValidationError {
                    entity,
                    field: "layers.mask",
                    kind: ValidationErrorKind::OutOfRange,
                });
            }
            // Unknown effect names are deliberately valid here: the model
            // preserves them and only execution rejects an unrecognized
            // descriptor. The bounds themselves match the compositor plan.
            validate_layer_effects(layer, entity, errors);
            match layer.source {
                SourceRef::Input(id) if !project.inputs().iter().any(|input| input.id == id) => {
                    errors.push(ValidationError {
                        entity,
                        field: "layers.source",
                        kind: ValidationErrorKind::MissingReference(EntityRef::Input(id)),
                    });
                }
                SourceRef::Scene(id) if id == scene.id => errors.push(ValidationError {
                    entity,
                    field: "layers.source",
                    kind: ValidationErrorKind::SelfReference,
                }),
                SourceRef::Scene(id) if !project.scenes().iter().any(|other| other.id == id) => {
                    errors.push(ValidationError {
                        entity,
                        field: "layers.source",
                        kind: ValidationErrorKind::MissingReference(EntityRef::Scene(id)),
                    });
                }
                SourceRef::Input(_) | SourceRef::Scene(_) => {}
            }
        }
    }
}

/// Records one out-of-range error when a layer's effect descriptors break
/// the [`LayerEffect`] table or count, and one when its key breaks its
/// bounds.
fn validate_layer_effects(
    layer: &Layer,
    entity: Option<EntityRef>,
    errors: &mut Vec<ValidationError>,
) {
    if layer.key.is_some_and(|key| key.validate().is_err()) {
        errors.push(ValidationError {
            entity,
            field: "layers.key",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
    if layer.effects.len() > LayerEffect::MAX_EFFECTS_PER_LAYER
        || layer
            .effects
            .iter()
            .any(|effect| effect.validate().is_err())
    {
        errors.push(ValidationError {
            entity,
            field: "layers.effects",
            kind: ValidationErrorKind::OutOfRange,
        });
    }
}

fn validate_buses(project: &Project, errors: &mut Vec<ValidationError>) {
    for bus in project.audio_buses() {
        let entity = Some(EntityRef::AudioBus(bus.id));
        require_name(errors, entity, "name", &bus.name);
        for (index, send) in bus.sends.iter().enumerate() {
            if send.destination == bus.id {
                errors.push(ValidationError {
                    entity,
                    field: "sends.destination",
                    kind: ValidationErrorKind::SelfReference,
                });
            } else if !project
                .audio_buses()
                .iter()
                .any(|other| other.id == send.destination)
            {
                errors.push(ValidationError {
                    entity,
                    field: "sends.destination",
                    kind: ValidationErrorKind::MissingReference(EntityRef::AudioBus(
                        send.destination,
                    )),
                });
            } else if bus.sends[..index]
                .iter()
                .any(|prior| prior.destination == send.destination)
            {
                errors.push(ValidationError {
                    entity,
                    field: "sends.destination",
                    kind: ValidationErrorKind::DuplicateReference,
                });
            }
        }
    }
}

fn validate_outputs(project: &Project, errors: &mut Vec<ValidationError>) {
    for output in project.outputs() {
        let entity = Some(EntityRef::Output(output.id));
        require_name(errors, entity, "name", &output.name);
        if !project
            .scenes()
            .iter()
            .any(|scene| scene.id == output.video_source)
        {
            errors.push(ValidationError {
                entity,
                field: "video_source",
                kind: ValidationErrorKind::MissingReference(EntityRef::Scene(output.video_source)),
            });
        }
        if !project
            .audio_buses()
            .iter()
            .any(|bus| bus.id == output.audio_source)
        {
            errors.push(ValidationError {
                entity,
                field: "audio_source",
                kind: ValidationErrorKind::MissingReference(EntityRef::AudioBus(
                    output.audio_source,
                )),
            });
        }
        validate_capabilities(
            errors,
            entity,
            output.required_capabilities.iter().map(String::as_str),
        );
    }
}

/// Re-checks what only the project knows about a streaming destination.
///
/// Scheme, endpoint shape, credentials and key bounds are enforced when a
/// [`StreamTarget`](crate::StreamTarget) is constructed and cannot be violated
/// by a value that exists, so they are not restated here.
fn validate_stream_targets(project: &Project, errors: &mut Vec<ValidationError>) {
    for target in project.stream_targets() {
        let entity = Some(EntityRef::StreamTarget(target.id()));
        require_name(errors, entity, "name", target.name());
        if target.name().len() > MAX_STREAM_TARGET_NAME_BYTES {
            errors.push(ValidationError {
                entity,
                field: "name",
                kind: ValidationErrorKind::OutOfRange,
            });
        }
        if !project
            .outputs()
            .iter()
            .any(|output| output.id == target.output())
        {
            errors.push(ValidationError {
                entity,
                field: "output",
                kind: ValidationErrorKind::MissingReference(EntityRef::Output(target.output())),
            });
        }
    }
}

fn validate_title(
    entity: Option<EntityRef>,
    title: &TitleInput,
    errors: &mut Vec<ValidationError>,
) {
    require_name(
        errors,
        entity,
        "kind.title.template_uri",
        &title.template_uri,
    );
    if let Err(error) = validate_title_fields(&title.fields) {
        let (field, kind) = match error {
            TitleFieldsError::TooManyFields { .. } | TitleFieldsError::ContentTooLarge { .. } => {
                ("kind.title.fields", ValidationErrorKind::OutOfRange)
            }
            TitleFieldsError::DuplicateField(_) => {
                ("kind.title.fields.id", ValidationErrorKind::DuplicateId)
            }
            TitleFieldsError::OutOfOrder { .. } => {
                ("kind.title.fields.id", ValidationErrorKind::OutOfOrder)
            }
            TitleFieldsError::TextTooLong { .. } => {
                ("kind.title.fields.value", ValidationErrorKind::OutOfRange)
            }
            TitleFieldsError::BlankImageUri(_) => {
                ("kind.title.fields.asset_uri", ValidationErrorKind::Empty)
            }
        };
        errors.push(ValidationError {
            entity,
            field,
            kind,
        });
    }
}

fn require_name(
    errors: &mut Vec<ValidationError>,
    entity: Option<EntityRef>,
    field: &'static str,
    value: &str,
) {
    if value.trim().is_empty() {
        errors.push(ValidationError {
            entity,
            field,
            kind: ValidationErrorKind::Empty,
        });
    }
}

fn duplicates<'a, I, Id>(errors: &mut Vec<ValidationError>, items: I, case_sensitive_names: bool)
where
    I: IntoIterator<Item = (Id, &'a str, EntityRef)>,
    Id: Copy + Eq,
{
    let items: Vec<_> = items.into_iter().collect();
    for (index, (id, name, entity)) in items.iter().copied().enumerate() {
        if items[..index].iter().any(|(prior, _, _)| *prior == id) {
            errors.push(ValidationError {
                entity: Some(entity),
                field: "id",
                kind: ValidationErrorKind::DuplicateId,
            });
        }
        if !name.trim().is_empty()
            && items[..index].iter().any(|(_, prior, _)| {
                if case_sensitive_names {
                    *prior == name
                } else {
                    prior.eq_ignore_ascii_case(name)
                }
            })
        {
            errors.push(ValidationError {
                entity: Some(entity),
                field: "name",
                kind: ValidationErrorKind::DuplicateName,
            });
        }
    }
}

fn validate_capabilities<'a>(
    errors: &mut Vec<ValidationError>,
    entity: Option<EntityRef>,
    keys: impl IntoIterator<Item = &'a str>,
) {
    for key in keys {
        let valid = key.split('.').count() >= 2
            && key.split('.').all(|segment| {
                !segment.is_empty()
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
        if !valid {
            errors.push(ValidationError {
                entity,
                field: "required_capabilities",
                kind: ValidationErrorKind::MalformedCapabilityKey,
            });
        }
    }
}

/// Display outputs (OR-011): usable selectors and modes, existing source
/// outputs, and no display claimed by both slots.
/// The `LiveLAN` configuration (OR-017): a viewer limit in range and a source
/// output the project holds.
fn validate_live_lan(project: &Project, errors: &mut Vec<ValidationError>) {
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    match project.live_lan().check(&outputs) {
        Ok(()) => {}
        Err(fm_types::live_lan::LiveLanConfigError::MaxViewers) => errors.push(ValidationError {
            entity: None,
            field: "live_lan.max_viewers",
            kind: ValidationErrorKind::OutOfRange,
        }),
        Err(fm_types::live_lan::LiveLanConfigError::UnknownOutput(output)) => {
            errors.push(ValidationError {
                entity: None,
                field: "live_lan.source",
                kind: ValidationErrorKind::MissingReference(EntityRef::Output(output)),
            });
        }
    }
}

fn validate_display_outputs(project: &Project, errors: &mut Vec<ValidationError>) {
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    for (slot, config) in project.display_outputs() {
        let others = project
            .display_outputs()
            .iter()
            .filter(|(other, _)| *other < slot)
            .map(|(other, config)| (*other, config));
        let Err(error) = crate::check_display_output(*slot, config, &outputs, others) else {
            continue;
        };
        errors.push(match error {
            crate::SetDisplayOutputError::UnknownOutput(output) => ValidationError {
                entity: None,
                field: "display_outputs.source",
                kind: ValidationErrorKind::MissingReference(EntityRef::Output(output)),
            },
            crate::SetDisplayOutputError::DisplayInUse { .. } => ValidationError {
                entity: None,
                field: "display_outputs.display",
                kind: ValidationErrorKind::DuplicateReference,
            },
            crate::SetDisplayOutputError::InvalidDisplay(_) => ValidationError {
                entity: None,
                field: "display_outputs.display",
                kind: ValidationErrorKind::OutOfRange,
            },
            crate::SetDisplayOutputError::InvalidMode => ValidationError {
                entity: None,
                field: "display_outputs.mode",
                kind: ValidationErrorKind::OutOfRange,
            },
        });
    }
}
