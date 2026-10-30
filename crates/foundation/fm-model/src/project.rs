use std::collections::{BTreeMap, BTreeSet};

use fm_types::{
    AudioFormat, BusId, FrameRate, InputId, InputOrderError, OutputId, ProjectId, RenameInputError,
    SceneId, VideoFormat, validate_input_name, validate_input_order,
};

use crate::{
    EntityRef, ValidationError, ValidationErrorKind,
    grade::{
        GradeEditError, GradeId, GradeSource, LutId, ProjectGrade, ProjectGrading, ProjectLut,
        SceneLayerRef,
    },
    playlist::Playlist,
    scene_animation::{LayerId, SceneAnimation, SceneAnimationError},
    stream::{StreamTarget, StreamTargetId},
    title::{SetTitleFieldsError, TitleFieldEntry, TitleInput, validate_title_fields},
    validation::validate_project,
};

/// Why [`Project::duplicate_input`] refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateInputError {
    UnknownInput(InputId),
    Add(AddInputError),
}

impl std::fmt::Display for DuplicateInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::Add(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DuplicateInputError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveInputError {
    UnknownInput(InputId),
    DomainReference(InputId),
}

/// The first persisted project reference that keeps an input from being
/// removed. See [`Project::input_reference`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputReference {
    /// The desired main-mix Program source.
    Program,
    /// The desired main-mix Preview source.
    Preview,
    /// The media input of a Stinger slot.
    Stinger(StingerSlotNumber),
    /// A layer source of a Scene.
    SceneLayer(SceneId),
    /// The audio source of a Scene input.
    SceneAudioSource(InputId),
    /// The target of an alias input.
    AliasTarget(InputId),
    /// The input of a playlist entry.
    PlaylistEntry(u64),
    /// An action of cue `cue` in cue list `list` (RC-017).
    CueAction { list: u64, cue: u64 },
    /// One of the inputs recorded for replay (OR-012).
    ReplaySource,
    /// The recorded angle of a highlight reel clip (OR-013).
    ReelClip { reel: u64 },
}

impl std::fmt::Display for InputReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Program => formatter.write_str("the Program source"),
            Self::Preview => formatter.write_str("the Preview source"),
            Self::Stinger(slot) => write!(formatter, "the media of Stinger {}", slot.number()),
            Self::SceneLayer(scene) => write!(formatter, "a layer source of scene {scene}"),
            Self::SceneAudioSource(input) => {
                write!(formatter, "the audio source of Scene input {input}")
            }
            Self::AliasTarget(input) => write!(formatter, "the target of alias input {input}"),
            Self::PlaylistEntry(entry) => write!(formatter, "playlist entry {entry}"),
            Self::CueAction { list, cue } => write!(formatter, "cue {cue} of cue list {list}"),
            Self::ReplaySource => formatter.write_str("a replay source"),
            Self::ReelClip { reel } => write!(formatter, "a clip of reel {reel}"),
        }
    }
}

/// Refusal for [`Project::set_input_locked_checked`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetInputLockError {
    UnknownInput(InputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddInputError {
    DuplicateId(InputId),
    EmptyName,
    NameTooLong,
    DuplicateName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameProjectError {
    EmptyName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioBusSendError {
    UnknownSource(BusId),
    UnknownDestination(BusId),
    SelfSend(BusId),
    DuplicateSend,
    MissingSend,
    Cycle,
    InvalidProject(Vec<ValidationError>),
}

impl std::fmt::Display for AudioBusSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSource(source) => write!(formatter, "unknown source audio bus {source}"),
            Self::UnknownDestination(destination) => {
                write!(formatter, "unknown destination audio bus {destination}")
            }
            Self::SelfSend(bus) => write!(formatter, "audio bus {bus} cannot send to itself"),
            Self::DuplicateSend => formatter.write_str("audio bus send already exists"),
            Self::MissingSend => formatter.write_str("audio bus send does not exist"),
            Self::Cycle => formatter.write_str("audio bus send would create a cycle"),
            Self::InvalidProject(_) => formatter.write_str("domain project failed validation"),
        }
    }
}

impl std::error::Error for AudioBusSendError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetStingerError {
    UnknownInput(InputId),
}

impl std::fmt::Display for SetStingerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown stinger media input {input}"),
        }
    }
}

impl std::error::Error for SetStingerError {}

impl std::fmt::Display for AddInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(input) => write!(formatter, "input {input} already exists"),
            Self::EmptyName => formatter.write_str("input name must not be empty"),
            Self::NameTooLong => write!(
                formatter,
                "input name must not exceed {} bytes",
                fm_types::MAX_INPUT_NAME_BYTES
            ),
            Self::DuplicateName => formatter.write_str("input name is already in use"),
        }
    }
}

impl std::error::Error for AddInputError {}

impl std::fmt::Display for RenameProjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("project name must not be empty"),
        }
    }
}

impl std::error::Error for RenameProjectError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveSceneError {
    UnknownScene(SceneId),
    InputReference { input: InputId, scene: SceneId },
    LayerReference { owner: SceneId, source: SceneId },
    OutputReference { output: OutputId, scene: SceneId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameSceneError {
    UnknownScene(SceneId),
    EmptyName,
    DuplicateName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DuplicateSceneInputError {
    UnknownSourceScene(SceneId),
    DuplicateSceneId(SceneId),
    DuplicateInputId(InputId),
    EmptySceneName,
    DuplicateSceneName,
    EmptyInputName,
    InputNameTooLong,
    DuplicateInputName,
    InvalidProject(Vec<ValidationError>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddSceneInputError {
    DuplicateSceneId(SceneId),
    DuplicateInputId(InputId),
    EmptySceneName,
    DuplicateSceneName,
    EmptyInputName,
    InputNameTooLong,
    DuplicateInputName,
    InvalidProject(Vec<ValidationError>),
}

impl Project {
    fn validate_new_scene_input(
        &self,
        scene: SceneId,
        scene_name: &str,
        input: InputId,
        input_name: &str,
    ) -> Result<(), AddSceneInputError> {
        if self.scenes.iter().any(|candidate| candidate.id == scene) {
            return Err(AddSceneInputError::DuplicateSceneId(scene));
        }
        if self.inputs.iter().any(|candidate| candidate.id == input) {
            return Err(AddSceneInputError::DuplicateInputId(input));
        }
        if scene_name.trim().is_empty() {
            return Err(AddSceneInputError::EmptySceneName);
        }
        if self
            .scenes
            .iter()
            .any(|candidate| candidate.name.eq_ignore_ascii_case(scene_name))
        {
            return Err(AddSceneInputError::DuplicateSceneName);
        }
        if input_name.trim().is_empty() {
            return Err(AddSceneInputError::EmptyInputName);
        }
        if input_name.len() > fm_types::MAX_INPUT_NAME_BYTES {
            return Err(AddSceneInputError::InputNameTooLong);
        }
        if self
            .inputs
            .iter()
            .any(|candidate| candidate.name == input_name)
        {
            return Err(AddSceneInputError::DuplicateInputName);
        }
        Ok(())
    }
}

impl From<AddSceneInputError> for DuplicateSceneInputError {
    fn from(error: AddSceneInputError) -> Self {
        match error {
            AddSceneInputError::DuplicateSceneId(value) => Self::DuplicateSceneId(value),
            AddSceneInputError::DuplicateInputId(value) => Self::DuplicateInputId(value),
            AddSceneInputError::EmptySceneName => Self::EmptySceneName,
            AddSceneInputError::DuplicateSceneName => Self::DuplicateSceneName,
            AddSceneInputError::EmptyInputName => Self::EmptyInputName,
            AddSceneInputError::InputNameTooLong => Self::InputNameTooLong,
            AddSceneInputError::DuplicateInputName => Self::DuplicateInputName,
            AddSceneInputError::InvalidProject(errors) => Self::InvalidProject(errors),
        }
    }
}

impl std::fmt::Display for DuplicateSceneInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSourceScene(scene) => write!(formatter, "unknown source scene {scene}"),
            Self::DuplicateSceneId(scene) => write!(formatter, "scene {scene} already exists"),
            Self::DuplicateInputId(input) => write!(formatter, "input {input} already exists"),
            Self::EmptySceneName => formatter.write_str("scene name must not be empty"),
            Self::DuplicateSceneName => formatter.write_str("scene name is already in use"),
            Self::EmptyInputName => formatter.write_str("input name must not be empty"),
            Self::InputNameTooLong => write!(
                formatter,
                "input name must not exceed {} bytes",
                fm_types::MAX_INPUT_NAME_BYTES
            ),
            Self::DuplicateInputName => formatter.write_str("input name is already in use"),
            Self::InvalidProject(errors) => write!(formatter, "invalid project ({})", errors.len()),
        }
    }
}

impl std::error::Error for DuplicateSceneInputError {}

impl std::fmt::Display for AddSceneInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSceneId(scene) => write!(formatter, "scene {scene} already exists"),
            Self::DuplicateInputId(input) => write!(formatter, "input {input} already exists"),
            Self::EmptySceneName => formatter.write_str("scene name must not be empty"),
            Self::DuplicateSceneName => formatter.write_str("scene name is already in use"),
            Self::EmptyInputName => formatter.write_str("input name must not be empty"),
            Self::InputNameTooLong => write!(
                formatter,
                "input name must not exceed {} bytes",
                fm_types::MAX_INPUT_NAME_BYTES
            ),
            Self::DuplicateInputName => formatter.write_str("input name is already in use"),
            Self::InvalidProject(errors) => write!(formatter, "invalid project ({})", errors.len()),
        }
    }
}

impl std::error::Error for AddSceneInputError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceInputError {
    UnknownInput(InputId),
    /// A scene source names a scene that does not exist.
    MissingScene(SceneId),
    /// An alias source names an input that does not exist.
    MissingInput(InputId),
    /// The replacement would make the input's scene graph reach itself.
    SourceCycle,
    /// The replacement would make this key/fill key input render a scene,
    /// directly or through an alias chain (SW-008).
    SceneKeyInput(InputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelinkMediaInputError {
    UnknownInput(InputId),
    NotMediaInput(InputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetOutputRouteError {
    UnknownOutput(OutputId),
    UnknownScene(SceneId),
    UnknownBus(BusId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetOutputStartupError {
    UnknownOutput(OutputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddAudioBusError {
    DuplicateId(BusId),
    EmptyName,
    DuplicateName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameAudioBusError {
    UnknownBus(BusId),
    EmptyName,
    DuplicateName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddOutputError {
    DuplicateId(OutputId),
    EmptyName,
    DuplicateName,
    UnknownScene(SceneId),
    UnknownBus(BusId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameOutputError {
    UnknownOutput(OutputId),
    EmptyName,
    DuplicateName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveOutputError {
    UnknownOutput(OutputId),
    StreamTargetReference {
        output: OutputId,
        stream_target: StreamTargetId,
    },
    /// A fullscreen display output shows the output (OR-011).
    DisplayOutputReference {
        output: OutputId,
        slot: fm_types::display::DisplayOutputSlot,
    },
    /// `LiveLAN` streams the output (OR-017).
    LiveLanReference {
        output: OutputId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddStreamTargetError {
    /// The project already carries [`crate::MAX_STREAM_TARGETS`].
    TooMany,
    DuplicateId(StreamTargetId),
    DuplicateName,
    UnknownOutput(OutputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateStreamTargetError {
    UnknownStreamTarget(StreamTargetId),
    DuplicateName,
    UnknownOutput(OutputId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveStreamTargetError {
    UnknownStreamTarget(StreamTargetId),
    /// Cue `cue` of cue list `list` starts or stops the target (RC-017).
    CueReference {
        target: StreamTargetId,
        list: u64,
        cue: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveAudioBusError {
    UnknownBus(BusId),
    OutputReference { bus: BusId, output: OutputId },
    OutgoingSend { bus: BusId, destination: BusId },
    IncomingSend { bus: BusId, source: BusId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddSceneLayerError {
    UnknownScene(SceneId),
    EmptyName,
    MissingInput(InputId),
    MissingScene(SceneId),
    SourceCycle,
    /// The added layer is enabled and the scene already draws
    /// [`Scene::MAX_RENDERED_LAYERS`] enabled layers.
    TooManyEnabledLayers,
}

/// Why [`Project::set_scene_animation`] refused an animation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetSceneAnimationError {
    UnknownScene(SceneId),
    Invalid(SceneAnimationError),
}

impl std::fmt::Display for SetSceneAnimationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::Invalid(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SetSceneAnimationError {}

/// Why [`Project::set_scene_layers`] refused a layer list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetSceneLayersError {
    UnknownScene(SceneId),
    Invalid(Vec<crate::ValidationError>),
}

impl std::fmt::Display for SetSceneLayersError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::Invalid(errors) => write!(
                formatter,
                "the scene layers would leave the project invalid ({} errors)",
                errors.len()
            ),
        }
    }
}

impl std::error::Error for SetSceneLayersError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetSceneBackgroundError {
    UnknownScene(SceneId),
    NotPremultiplied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneLayerError {
    UnknownScene(SceneId),
    SameScene(SceneId),
    LayerIndexOutOfRange {
        scene: SceneId,
        index: usize,
        length: usize,
    },
    EmptyName,
    InvalidGeometry {
        scene: SceneId,
        index: usize,
    },
    InvalidCrop {
        scene: SceneId,
        index: usize,
    },
    InvalidMask {
        scene: SceneId,
        index: usize,
    },
    /// The key breaks its bounds (IN-020).
    InvalidKey {
        scene: SceneId,
        index: usize,
        reason: crate::LayerKeyError,
    },
    TooManyEffects {
        scene: SceneId,
        index: usize,
    },
    InvalidEffect {
        scene: SceneId,
        index: usize,
        effect_index: usize,
        reason: LayerEffectError,
    },
    EffectIndexOutOfRange {
        scene: SceneId,
        index: usize,
        effect_index: usize,
        length: usize,
    },
    CropWouldInvalidateMask {
        scene: SceneId,
        index: usize,
    },
    MissingInput(InputId),
    MissingScene(SceneId),
    SourceCycle,
    /// A key/fill key input that is a Scene input (SW-008).
    SceneKeyInput(InputId),
    /// The edit would leave `scene` with more than
    /// [`Scene::MAX_RENDERED_LAYERS`] enabled layers.
    TooManyEnabledLayers {
        scene: SceneId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneInputAudioSourceError {
    MissingTargetInput(InputId),
    NonSceneTargetInput(InputId),
    MissingSourceInput(InputId),
    SourceCycle,
}

impl std::fmt::Display for SceneInputAudioSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTargetInput(input) => write!(formatter, "missing target input {input}"),
            Self::NonSceneTargetInput(input) => {
                write!(formatter, "target input {input} is not a scene input")
            }
            Self::MissingSourceInput(input) => write!(formatter, "missing source input {input}"),
            Self::SourceCycle => {
                formatter.write_str("scene input audio source would create a cycle")
            }
        }
    }
}

impl std::error::Error for SceneInputAudioSourceError {}

impl std::fmt::Display for SceneLayerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::SameScene(scene) => {
                write!(formatter, "source and target scenes must differ: {scene}")
            }
            Self::LayerIndexOutOfRange {
                scene,
                index,
                length,
            } => write!(
                formatter,
                "layer index {index} out of range for scene {scene} with {length} layers"
            ),
            Self::EmptyName => formatter.write_str("scene layer name must not be empty"),
            Self::InvalidGeometry { scene, index } => {
                write!(
                    formatter,
                    "invalid geometry for layer {index} in scene {scene}"
                )
            }
            Self::InvalidCrop { scene, index } => {
                write!(formatter, "invalid crop for layer {index} in scene {scene}")
            }
            Self::InvalidMask { scene, index } => {
                write!(formatter, "invalid mask for layer {index} in scene {scene}")
            }
            Self::InvalidKey {
                scene,
                index,
                reason,
            } => write!(
                formatter,
                "invalid key for layer {index} in scene {scene}: {reason}"
            ),
            Self::TooManyEffects { scene, index } => write!(
                formatter,
                "layer {index} in scene {scene} cannot carry more than {} effects",
                LayerEffect::MAX_EFFECTS_PER_LAYER
            ),
            Self::InvalidEffect {
                scene,
                index,
                effect_index,
                reason,
            } => write!(
                formatter,
                "invalid effect {effect_index} for layer {index} in scene {scene}: {reason}"
            ),
            Self::EffectIndexOutOfRange {
                scene,
                index,
                effect_index,
                length,
            } => write!(
                formatter,
                "effect index {effect_index} out of range for layer {index} in scene {scene} with \
                 {length} effects"
            ),
            Self::CropWouldInvalidateMask { scene, index } => write!(
                formatter,
                "crop would invalidate the mask for layer {index} in scene {scene}"
            ),
            Self::MissingInput(input) => write!(formatter, "missing source input {input}"),
            Self::MissingScene(scene) => write!(formatter, "missing source scene {scene}"),
            Self::SourceCycle => formatter.write_str("scene layer source would create a cycle"),
            Self::SceneKeyInput(input) => write!(
                formatter,
                "key input {input} is a Scene input; a key/fill key needs a non-Scene input"
            ),
            Self::TooManyEnabledLayers { scene } => write!(
                formatter,
                "scene {scene} already draws {} enabled layers, the most the compositor draws",
                Scene::MAX_RENDERED_LAYERS
            ),
        }
    }
}

impl std::error::Error for SceneLayerError {}

impl std::fmt::Display for AddSceneLayerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::EmptyName => formatter.write_str("scene layer name must not be empty"),
            Self::MissingInput(input) => write!(formatter, "missing source input {input}"),
            Self::MissingScene(scene) => write!(formatter, "missing source scene {scene}"),
            Self::SourceCycle => formatter.write_str("scene layer source would create a cycle"),
            Self::TooManyEnabledLayers => write!(
                formatter,
                "the scene already draws {} enabled layers, the most the compositor draws",
                Scene::MAX_RENDERED_LAYERS
            ),
        }
    }
}

impl std::error::Error for AddSceneLayerError {}

impl std::fmt::Display for SetSceneBackgroundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::NotPremultiplied => formatter.write_str("scene background must be premultiplied"),
        }
    }
}

impl std::error::Error for SetSceneBackgroundError {}

impl std::fmt::Display for ReplaceInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::MissingScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::MissingInput(target) => write!(formatter, "unknown alias target input {target}"),
            Self::SourceCycle => formatter.write_str("input source would create a cycle"),
            Self::SceneKeyInput(input) => write!(
                formatter,
                "key input {input} would render a scene; a key/fill key needs a picture input"
            ),
        }
    }
}

impl std::error::Error for ReplaceInputError {}

impl std::fmt::Display for RelinkMediaInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::NotMediaInput(input) => write!(formatter, "input {input} is not a media input"),
        }
    }
}

impl std::error::Error for RelinkMediaInputError {}

impl std::fmt::Display for SetOutputRouteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::UnknownBus(bus) => write!(formatter, "unknown audio bus {bus}"),
        }
    }
}

impl std::error::Error for SetOutputRouteError {}

impl std::fmt::Display for SetOutputStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
        }
    }
}

impl std::error::Error for SetOutputStartupError {}

impl std::fmt::Display for AddAudioBusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(formatter, "duplicate audio bus {id}"),
            Self::EmptyName => formatter.write_str("audio bus name must not be empty"),
            Self::DuplicateName => formatter.write_str("duplicate audio bus name"),
        }
    }
}

impl std::error::Error for AddAudioBusError {}

impl std::fmt::Display for RenameAudioBusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownBus(bus) => write!(formatter, "unknown audio bus {bus}"),
            Self::EmptyName => formatter.write_str("audio bus name must not be empty"),
            Self::DuplicateName => formatter.write_str("duplicate audio bus name"),
        }
    }
}

impl std::error::Error for RenameAudioBusError {}

impl std::fmt::Display for AddOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(formatter, "duplicate output {id}"),
            Self::EmptyName => formatter.write_str("output name must not be empty"),
            Self::DuplicateName => formatter.write_str("duplicate output name"),
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::UnknownBus(bus) => write!(formatter, "unknown audio bus {bus}"),
        }
    }
}

impl std::error::Error for AddOutputError {}

impl std::fmt::Display for RenameOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
            Self::EmptyName => formatter.write_str("output name must not be empty"),
            Self::DuplicateName => formatter.write_str("duplicate output name"),
        }
    }
}

impl std::error::Error for RenameOutputError {}

impl std::fmt::Display for RemoveOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
            Self::StreamTargetReference {
                output,
                stream_target,
            } => write!(
                formatter,
                "output {output} is used by stream destination {stream_target}"
            ),
            Self::DisplayOutputReference { output, slot } => write!(
                formatter,
                "output {output} is shown by display output {slot}"
            ),
            Self::LiveLanReference { output } => {
                write!(formatter, "output {output} is streamed by LiveLAN")
            }
        }
    }
}

impl std::error::Error for RemoveOutputError {}

impl std::fmt::Display for AddStreamTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany => write!(
                formatter,
                "the project already has {} stream destinations, the most it may hold",
                crate::MAX_STREAM_TARGETS
            ),
            Self::DuplicateId(id) => write!(formatter, "duplicate stream destination {id}"),
            Self::DuplicateName => formatter.write_str("duplicate stream destination name"),
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
        }
    }
}

impl std::error::Error for AddStreamTargetError {}

impl std::fmt::Display for UpdateStreamTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStreamTarget(id) => {
                write!(formatter, "unknown stream destination {id}")
            }
            Self::DuplicateName => formatter.write_str("duplicate stream destination name"),
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
        }
    }
}

impl std::error::Error for UpdateStreamTargetError {}

impl std::fmt::Display for RemoveStreamTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStreamTarget(id) => {
                write!(formatter, "unknown stream destination {id}")
            }
            Self::CueReference { target, list, cue } => write!(
                formatter,
                "stream destination {target} is in use by cue {cue} of cue list {list}"
            ),
        }
    }
}

impl std::error::Error for RemoveStreamTargetError {}

impl std::fmt::Display for RemoveAudioBusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownBus(bus) => write!(formatter, "unknown audio bus {bus}"),
            Self::OutputReference { bus, output } => {
                write!(formatter, "audio bus {bus} is used by output {output}")
            }
            Self::OutgoingSend { bus, destination } => {
                write!(
                    formatter,
                    "audio bus {bus} sends to audio bus {destination}"
                )
            }
            Self::IncomingSend { bus, source } => {
                write!(formatter, "audio bus {source} sends to audio bus {bus}")
            }
        }
    }
}

impl std::error::Error for RemoveAudioBusError {}

impl std::fmt::Display for RemoveInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::DomainReference(input) => {
                write!(formatter, "input {input} has a domain reference")
            }
        }
    }
}

impl std::error::Error for RemoveInputError {}

impl std::fmt::Display for SetInputLockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
        }
    }
}

impl std::error::Error for SetInputLockError {}

impl std::fmt::Display for RemoveSceneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "unknown scene {scene}"),
            Self::InputReference { input, scene } => {
                write!(formatter, "input {input} references scene {scene}")
            }
            Self::LayerReference { owner, source } => {
                write!(formatter, "scene {owner} layer references scene {source}")
            }
            Self::OutputReference { output, scene } => {
                write!(formatter, "output {output} references scene {scene}")
            }
        }
    }
}

impl std::error::Error for RemoveSceneError {}

impl std::fmt::Display for RenameSceneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownScene(scene) => write!(formatter, "scene {scene} does not exist"),
            Self::EmptyName => formatter.write_str("scene name must not be empty"),
            Self::DuplicateName => formatter.write_str("scene name is already in use"),
        }
    }
}

impl std::error::Error for RenameSceneError {}

pub const CURRENT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(92);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    schema_version: SchemaVersion,
    id: ProjectId,
    name: String,
    settings: ProjectSettings,
    inputs: Vec<Input>,
    input_audio_strips: Vec<InputAudioStrip>,
    scenes: Vec<Scene>,
    audio_buses: Vec<AudioBus>,
    outputs: Vec<Output>,
    stream_targets: Vec<StreamTarget>,
    main_mix: Option<MainMix>,
    recording_desired: [bool; RECORDER_SLOT_COUNT],
    stingers: Vec<StingerConfig>,
    restart_policy: RestartPolicy,
    locked: bool,
    locked_inputs: BTreeSet<InputId>,
    playlist: Playlist,
    cue_lists: Vec<crate::CueList>,
    scripts: Vec<crate::Script>,
    script_run: crate::ScriptRunState,
    shortcuts: Vec<crate::Shortcut>,
    media_lists: BTreeMap<InputId, crate::MediaList>,
    title_timelines: BTreeMap<InputId, crate::TitleTimeline>,
    grading: ProjectGrading,
    animation_presets: crate::AnimationPresetLibrary,
    transition_favorites: crate::TransitionFavorites,
    input_categories: Vec<crate::InputCategory>,
    input_lifecycle: BTreeMap<InputId, crate::InputLifecycle>,
    audio_groups: crate::AudioGroups,
    aux_buses: crate::AuxBuses,
    output_aux_buses: BTreeMap<OutputId, crate::AuxBusNumber>,
    audio_solo_mode: crate::AudioSoloMode,
    audio_monitor_output: crate::AudioMonitorOutput,
    master_audio_dsp: fm_types::strip_dsp::StripDsp,
    /// The Master bus audio plugin chain (AU-006, schema 91).
    master_audio_plugins: fm_types::audio_plugins::PluginChainState,
    fade_to_black_excluded_outputs: BTreeSet<OutputId>,
    fade_to_black_output_durations: BTreeMap<OutputId, u32>,
    fade_to_black_curve: fm_types::FadeToBlackCurve,
    display_outputs:
        BTreeMap<fm_types::display::DisplayOutputSlot, fm_types::display::DisplayOutputConfig>,
    live_lan: fm_types::live_lan::LiveLanConfig,
    /// The desktop restore token each screen capture input's last screen
    /// sharing grant returned (IN-011, schema 72), by input.
    screen_capture_restore_tokens: BTreeMap<InputId, String>,
    /// What the server records for replay (OR-012, schema 82).
    replay: fm_types::replay::ReplaySettings,
    /// The project's highlight reels (OR-013, schema 83).
    reels: Vec<fm_types::reel::ReelSpec>,
    /// The show's own settings for health alert rules, in rule order, none
    /// equal to its rule's default (RC-015, schema 85).
    alert_rules: Vec<fm_types::alert_rules::AlertRuleSetting>,
}

impl Project {
    #[must_use]
    pub fn new(id: ProjectId, name: impl Into<String>, settings: ProjectSettings) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            settings,
            inputs: Vec::new(),
            input_audio_strips: Vec::new(),
            scenes: Vec::new(),
            audio_buses: Vec::new(),
            outputs: Vec::new(),
            stream_targets: Vec::new(),
            main_mix: None,
            recording_desired: [false; RECORDER_SLOT_COUNT],
            stingers: Vec::new(),
            restart_policy: RestartPolicy::default(),
            locked: false,
            locked_inputs: BTreeSet::new(),
            playlist: Playlist::default(),
            cue_lists: Vec::new(),
            scripts: Vec::new(),
            script_run: crate::ScriptRunState::default(),
            shortcuts: Vec::new(),
            media_lists: BTreeMap::new(),
            title_timelines: BTreeMap::new(),
            grading: ProjectGrading::default(),
            animation_presets: crate::AnimationPresetLibrary::default(),
            transition_favorites: crate::TransitionFavorites::default(),
            input_categories: Vec::new(),
            input_lifecycle: BTreeMap::new(),
            audio_groups: crate::AudioGroups::default(),
            aux_buses: crate::AuxBuses::default(),
            output_aux_buses: BTreeMap::new(),
            audio_solo_mode: crate::AudioSoloMode::InPlace,
            audio_monitor_output: crate::AudioMonitorOutput::default(),
            master_audio_dsp: fm_types::strip_dsp::StripDsp::DEFAULT,
            master_audio_plugins: fm_types::audio_plugins::PluginChainState::new(),
            fade_to_black_excluded_outputs: BTreeSet::new(),
            fade_to_black_output_durations: BTreeMap::new(),
            fade_to_black_curve: fm_types::FadeToBlackCurve::Linear,
            display_outputs: BTreeMap::new(),
            live_lan: fm_types::live_lan::LiveLanConfig::default(),
            screen_capture_restore_tokens: BTreeMap::new(),
            replay: fm_types::replay::ReplaySettings::default(),
            reels: Vec::new(),
            alert_rules: Vec::new(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    #[must_use]
    pub const fn id(&self) -> ProjectId {
        self.id
    }

    /// Returns this project under a different identity, for example when a
    /// new project is instantiated from a template bundle. Every other field,
    /// including the project-local input, scene, bus, and output ids, is kept.
    #[must_use]
    pub const fn with_id(mut self, id: ProjectId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Renames the project while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`RenameProjectError::EmptyName`] when the name is blank.
    pub fn rename_project(&mut self, name: String) -> Result<(), RenameProjectError> {
        if name.trim().is_empty() {
            return Err(RenameProjectError::EmptyName);
        }
        self.name = name;
        Ok(())
    }

    #[must_use]
    pub const fn settings(&self) -> &ProjectSettings {
        &self.settings
    }

    #[must_use]
    pub fn inputs(&self) -> &[Input] {
        &self.inputs
    }

    #[must_use]
    pub fn input_audio_strips(&self) -> &[InputAudioStrip] {
        &self.input_audio_strips
    }

    #[must_use]
    pub fn input_audio_strip(&self, input: InputId) -> Option<InputAudioStripState> {
        self.input_audio_strips
            .iter()
            .find(|strip| strip.input == input)
            .map(|strip| strip.state)
    }

    /// The operator label of `input`'s strip (empty: show the input name).
    #[must_use]
    pub fn input_audio_label(&self, input: InputId) -> Option<&str> {
        self.input_audio_strips
            .iter()
            .find(|strip| strip.input == input)
            .map(|strip| strip.label.as_str())
    }

    /// The strip group `input`'s strip belongs to, if any.
    #[must_use]
    pub fn input_audio_group(&self, input: InputId) -> Option<crate::AudioGroupNumber> {
        self.input_audio_strips
            .iter()
            .find(|strip| strip.input == input)
            .and_then(|strip| strip.group)
    }

    /// Sets the operator label of `input`'s strip; empty clears it.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::UnknownInput`] or
    /// [`SetAudioMixerError::Label`]. Nothing changes on error.
    pub fn set_input_audio_label(
        &mut self,
        input: InputId,
        label: String,
    ) -> Result<(), SetAudioMixerError> {
        crate::validate_audio_strip_label(&label).map_err(SetAudioMixerError::Label)?;
        let strip = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
            .ok_or(SetAudioMixerError::UnknownInput(input))?;
        strip.label = label;
        Ok(())
    }

    /// Restores a persisted strip label without validating it; an unusable
    /// label is reported by [`Self::validate`].
    pub fn restore_input_audio_label(&mut self, input: InputId, label: String) {
        if let Some(strip) = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
        {
            strip.label = label;
        }
    }

    /// The DSP chain of `input`'s strip (AU-005).
    #[must_use]
    pub fn input_audio_dsp(&self, input: InputId) -> Option<&fm_types::strip_dsp::StripDsp> {
        self.input_audio_strips
            .iter()
            .find(|strip| strip.input == input)
            .map(|strip| &strip.dsp)
    }

    /// Replaces the DSP chain of `input`'s strip.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::UnknownInput`] or
    /// [`SetAudioMixerError::Dsp`]. Nothing changes on error.
    pub fn set_input_audio_dsp(
        &mut self,
        input: InputId,
        dsp: fm_types::strip_dsp::StripDsp,
    ) -> Result<(), SetAudioMixerError> {
        dsp.validate().map_err(SetAudioMixerError::Dsp)?;
        let strip = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
            .ok_or(SetAudioMixerError::UnknownInput(input))?;
        strip.dsp = dsp;
        Ok(())
    }

    /// The Master bus DSP chain (AU-005, schema 65).
    #[must_use]
    pub const fn master_audio_dsp(&self) -> &fm_types::strip_dsp::StripDsp {
        &self.master_audio_dsp
    }

    /// Replaces the Master bus DSP chain.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::Dsp`]; nothing changes on error.
    pub fn set_master_audio_dsp(
        &mut self,
        dsp: fm_types::strip_dsp::StripDsp,
    ) -> Result<(), SetAudioMixerError> {
        dsp.validate().map_err(SetAudioMixerError::Dsp)?;
        self.master_audio_dsp = dsp;
        Ok(())
    }

    /// `input`'s audio plugin chain (AU-006, schema 91), or `None` when the
    /// input is not part of this project.
    #[must_use]
    pub fn input_audio_plugins(
        &self,
        input: InputId,
    ) -> Option<&fm_types::audio_plugins::PluginChainState> {
        self.input_audio_strips
            .iter()
            .find(|strip| strip.input == input)
            .map(|strip| &strip.plugins)
    }

    /// Replaces `input`'s audio plugin chain.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::UnknownInput`] or
    /// [`SetAudioMixerError::Plugins`]; nothing changes on error.
    pub fn set_input_audio_plugins(
        &mut self,
        input: InputId,
        chain: fm_types::audio_plugins::PluginChainState,
    ) -> Result<(), SetAudioMixerError> {
        chain.validate().map_err(SetAudioMixerError::Plugins)?;
        let strip = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
            .ok_or(SetAudioMixerError::UnknownInput(input))?;
        strip.plugins = chain;
        Ok(())
    }

    /// The Master bus audio plugin chain (AU-006, schema 91).
    #[must_use]
    pub const fn master_audio_plugins(&self) -> &fm_types::audio_plugins::PluginChainState {
        &self.master_audio_plugins
    }

    /// Replaces the Master bus audio plugin chain.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::Plugins`]; nothing changes on error.
    pub fn set_master_audio_plugins(
        &mut self,
        chain: fm_types::audio_plugins::PluginChainState,
    ) -> Result<(), SetAudioMixerError> {
        chain.validate().map_err(SetAudioMixerError::Plugins)?;
        self.master_audio_plugins = chain;
        Ok(())
    }

    /// Assigns `input`'s strip to `group`, or to none.
    ///
    /// Returns `false` when `input` is not part of this project.
    pub fn set_input_audio_group(
        &mut self,
        input: InputId,
        group: Option<crate::AudioGroupNumber>,
    ) -> bool {
        let Some(strip) = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
        else {
            return false;
        };
        strip.group = group;
        true
    }

    /// The project's strip groups (AU-001, schema 39).
    #[must_use]
    pub const fn audio_groups(&self) -> &crate::AudioGroups {
        &self.audio_groups
    }

    /// Replaces one strip group's name, fader, mute, solo, and parent.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::Label`] for an unusable name and
    /// [`SetAudioMixerError::GroupCycle`] for a parent that would nest the
    /// group inside itself. Nothing changes on error.
    pub fn set_audio_group(
        &mut self,
        group: crate::AudioGroupNumber,
        value: crate::AudioGroup,
    ) -> Result<(), SetAudioMixerError> {
        crate::validate_audio_group_name(&value.name).map_err(SetAudioMixerError::Label)?;
        if !self.audio_groups.parent_is_acyclic(group, value.parent) {
            return Err(SetAudioMixerError::GroupCycle(group));
        }
        self.audio_groups.set(group, value);
        Ok(())
    }

    /// How a soloed strip is heard (AU-001, schema 42).
    #[must_use]
    pub const fn audio_solo_mode(&self) -> crate::AudioSoloMode {
        self.audio_solo_mode
    }

    pub fn set_audio_solo_mode(&mut self, mode: crate::AudioSoloMode) {
        self.audio_solo_mode = mode;
    }

    /// Where the monitor bus is heard, and how loud (AU-001, schema 50).
    #[must_use]
    pub const fn audio_monitor_output(&self) -> &crate::AudioMonitorOutput {
        &self.audio_monitor_output
    }

    /// Replaces the monitor output.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::MonitorDevice`] for an unusable device
    /// name. Nothing changes on error.
    pub fn set_audio_monitor_output(
        &mut self,
        value: crate::AudioMonitorOutput,
    ) -> Result<(), SetAudioMixerError> {
        if let Some(device) = &value.device {
            crate::validate_audio_monitor_device(device)
                .map_err(SetAudioMixerError::MonitorDevice)?;
        }
        self.audio_monitor_output = value;
        Ok(())
    }

    /// Replaces the monitor output; the device is reported by
    /// [`Self::validate`].
    pub fn restore_audio_monitor_output(&mut self, value: crate::AudioMonitorOutput) {
        self.audio_monitor_output = value;
    }

    /// Replaces every strip group; names are reported by
    /// [`Self::validate`].
    pub fn restore_audio_groups(&mut self, groups: crate::AudioGroups) {
        self.audio_groups = groups;
    }

    /// The project's aux buses A–G (AU-002, schema 90).
    #[must_use]
    pub const fn aux_buses(&self) -> &crate::AuxBuses {
        &self.aux_buses
    }

    /// Replaces one aux bus's name, fader, and mute.
    ///
    /// # Errors
    ///
    /// Returns [`SetAudioMixerError::Label`] for an unusable name. Nothing
    /// changes on error.
    pub fn set_aux_bus(
        &mut self,
        bus: crate::AuxBusNumber,
        value: crate::AuxBus,
    ) -> Result<(), SetAudioMixerError> {
        crate::validate_audio_group_name(&value.name).map_err(SetAudioMixerError::Label)?;
        self.aux_buses.set(bus, value);
        Ok(())
    }

    /// Replaces every aux bus; names are reported by [`Self::validate`].
    pub fn restore_aux_buses(&mut self, buses: crate::AuxBuses) {
        self.aux_buses = buses;
    }

    /// The outputs that hear an aux bus instead of Master (AU-002, schema
    /// 90), ascending. Every other output hears Master.
    #[must_use]
    pub const fn output_aux_buses(&self) -> &BTreeMap<OutputId, crate::AuxBusNumber> {
        &self.output_aux_buses
    }

    /// The aux bus `output` hears, or `None` for Master.
    #[must_use]
    pub fn output_aux_bus(&self, output: OutputId) -> Option<crate::AuxBusNumber> {
        self.output_aux_buses.get(&output).copied()
    }

    /// Makes `output` hear aux bus `bus`, or Master for `None`. Returns
    /// `false`, changing nothing, for an unknown output.
    pub fn set_output_aux_bus(
        &mut self,
        output: OutputId,
        bus: Option<crate::AuxBusNumber>,
    ) -> bool {
        if !self.outputs.iter().any(|candidate| candidate.id == output) {
            return false;
        }
        match bus {
            Some(bus) => self.output_aux_buses.insert(output, bus),
            None => self.output_aux_buses.remove(&output),
        };
        true
    }

    /// Replaces every output's aux bus; unknown outputs are reported by
    /// [`Self::validate`].
    pub fn restore_output_aux_buses(&mut self, buses: BTreeMap<OutputId, crate::AuxBusNumber>) {
        self.output_aux_buses = buses;
    }

    /// The project outputs Fade to Black leaves live (SW-004, schema 39).
    /// Every other output, including one added later, fades.
    #[must_use]
    pub const fn fade_to_black_excluded_outputs(&self) -> &BTreeSet<OutputId> {
        &self.fade_to_black_excluded_outputs
    }

    /// Whether Fade to Black fades `output`.
    #[must_use]
    pub fn fade_to_black_affects(&self, output: OutputId) -> bool {
        !self.fade_to_black_excluded_outputs.contains(&output)
    }

    /// The outputs that fade over their own duration instead of the one
    /// each Fade to Black request carries (SW-004, schema 45), ascending.
    #[must_use]
    pub const fn fade_to_black_output_durations(&self) -> &BTreeMap<OutputId, u32> {
        &self.fade_to_black_output_durations
    }

    /// Gives `output` its own Fade-to-Black duration in frames, or with
    /// `None` returns it to the requested one. Unknown outputs and
    /// out-of-range durations are reported by [`Self::validate`].
    pub fn set_fade_to_black_output_duration(
        &mut self,
        output: OutputId,
        duration_frames: Option<u32>,
    ) {
        match duration_frames {
            Some(duration) => {
                self.fade_to_black_output_durations.insert(output, duration);
            }
            None => {
                self.fade_to_black_output_durations.remove(&output);
            }
        }
    }

    /// How automatic Fade-to-Black moves ease (SW-004, schema 45).
    #[must_use]
    pub const fn fade_to_black_curve(&self) -> fm_types::FadeToBlackCurve {
        self.fade_to_black_curve
    }

    pub const fn set_fade_to_black_curve(&mut self, curve: fm_types::FadeToBlackCurve) {
        self.fade_to_black_curve = curve;
    }

    /// The configured fullscreen display outputs by slot (OR-011, schema
    /// 40).
    #[must_use]
    pub const fn display_outputs(
        &self,
    ) -> &BTreeMap<fm_types::display::DisplayOutputSlot, fm_types::display::DisplayOutputConfig>
    {
        &self.display_outputs
    }

    /// One slot's display output, if configured.
    #[must_use]
    pub fn display_output(
        &self,
        slot: fm_types::display::DisplayOutputSlot,
    ) -> Option<&fm_types::display::DisplayOutputConfig> {
        self.display_outputs.get(&slot)
    }

    /// Configures `slot` and returns its previous configuration.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::SetDisplayOutputError`] for an unknown source
    /// output, a display the other slot claims, or an unusable display or
    /// mode. Nothing changes on error.
    pub fn set_display_output(
        &mut self,
        slot: fm_types::display::DisplayOutputSlot,
        config: fm_types::display::DisplayOutputConfig,
    ) -> Result<Option<fm_types::display::DisplayOutputConfig>, crate::SetDisplayOutputError> {
        let outputs = self
            .outputs
            .iter()
            .map(|output| output.id)
            .collect::<Vec<_>>();
        crate::check_display_output(
            slot,
            &config,
            &outputs,
            self.display_outputs
                .iter()
                .map(|(slot, config)| (*slot, config)),
        )?;
        Ok(self.display_outputs.insert(slot, config))
    }

    /// Restores a persisted slot without checking it; problems are reported
    /// by [`Self::validate`].
    pub fn restore_display_output(
        &mut self,
        slot: fm_types::display::DisplayOutputSlot,
        config: fm_types::display::DisplayOutputConfig,
    ) {
        self.display_outputs.insert(slot, config);
    }

    /// Clears `slot` and returns what it held.
    pub fn clear_display_output(
        &mut self,
        slot: fm_types::display::DisplayOutputSlot,
    ) -> Option<fm_types::display::DisplayOutputConfig> {
        self.display_outputs.remove(&slot)
    }

    /// What the server records for replay and how long it keeps it
    /// (OR-012, schema 82).
    #[must_use]
    pub const fn replay(&self) -> &fm_types::replay::ReplaySettings {
        &self.replay
    }

    /// How input `input` may be recorded for replay: an existing input that
    /// is not itself a Replay input.
    #[must_use]
    pub fn replay_source_check(&self, input: InputId) -> fm_types::replay::ReplaySourceCheck {
        match self.inputs.iter().find(|candidate| candidate.id == input) {
            None => fm_types::replay::ReplaySourceCheck::Unknown,
            Some(found) if matches!(found.kind, InputKind::Replay) => {
                fm_types::replay::ReplaySourceCheck::Unrecordable
            }
            Some(_) => fm_types::replay::ReplaySourceCheck::Recordable,
        }
    }

    /// Replaces the replay settings and returns the previous ones.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::replay::ReplaySettingsError`] of settings
    /// that name unknown or Replay inputs, too many or repeated sources, an
    /// out-of-range retention, or an unusable storage root. Nothing changes
    /// on error.
    pub fn set_replay(
        &mut self,
        settings: fm_types::replay::ReplaySettings,
    ) -> Result<fm_types::replay::ReplaySettings, fm_types::replay::ReplaySettingsError> {
        settings.check(|input| self.replay_source_check(input))?;
        Ok(std::mem::replace(&mut self.replay, settings))
    }

    /// Restores persisted replay settings without checking them; problems
    /// are reported by [`Self::validate`].
    pub fn restore_replay(&mut self, settings: fm_types::replay::ReplaySettings) {
        self.replay = settings;
    }

    /// The project's highlight reels, in display order (OR-013, schema 83).
    #[must_use]
    pub fn reels(&self) -> &[fm_types::reel::ReelSpec] {
        &self.reels
    }

    /// Replaces the reel with `spec.id` or appends `spec`, and returns the
    /// previous reel when one was replaced.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::reel::ReelSpecError`] of a reel with no clips,
    /// too many clips, a bad name, an angle that cannot be recorded, an
    /// unsupported speed, or a trim out of range, and
    /// [`fm_types::reel::ReelSpecError::TooManyReels`] when appending a reel
    /// beyond [`fm_types::reel::MAX_REELS`]. Nothing changes on error.
    pub fn set_reel(
        &mut self,
        spec: fm_types::reel::ReelSpec,
    ) -> Result<Option<fm_types::reel::ReelSpec>, fm_types::reel::ReelSpecError> {
        spec.check(|input| {
            self.replay_source_check(input) == fm_types::replay::ReplaySourceCheck::Recordable
        })?;
        if let Some(index) = self.reels.iter().position(|reel| reel.id == spec.id) {
            return Ok(Some(std::mem::replace(&mut self.reels[index], spec)));
        }
        if self.reels.len() >= fm_types::reel::MAX_REELS {
            return Err(fm_types::reel::ReelSpecError::TooManyReels);
        }
        self.reels.push(spec);
        Ok(None)
    }

    /// Removes reel `id` and returns it.
    pub fn remove_reel(&mut self, id: u64) -> Option<fm_types::reel::ReelSpec> {
        let index = self.reels.iter().position(|reel| reel.id == id)?;
        Some(self.reels.remove(index))
    }

    /// Restores persisted reels without checking them; problems are reported
    /// by [`Self::validate`].
    pub fn restore_reels(&mut self, reels: Vec<fm_types::reel::ReelSpec>) {
        self.reels = reels;
    }

    /// The show's own health alert rule settings, in rule order (RC-015,
    /// schema 85); a rule without one uses its default
    /// ([`fm_types::alert_rules::effective_setting`]).
    #[must_use]
    pub fn alert_rules(&self) -> &[fm_types::alert_rules::AlertRuleSetting] {
        &self.alert_rules
    }

    /// Sets one rule's setting (a default setting removes the show's own)
    /// and returns the setting the rule had.
    ///
    /// # Errors
    ///
    /// Returns an [`fm_types::alert_rules::AlertRuleError`] for thresholds
    /// out of range or without a hysteresis band, or a hold too long.
    /// Nothing changes on error.
    pub fn set_alert_rule(
        &mut self,
        setting: fm_types::alert_rules::AlertRuleSetting,
    ) -> Result<fm_types::alert_rules::AlertRuleSetting, fm_types::alert_rules::AlertRuleError>
    {
        setting.check()?;
        let previous = fm_types::alert_rules::effective_setting(&self.alert_rules, setting.rule);
        fm_types::alert_rules::put_setting(&mut self.alert_rules, setting);
        Ok(previous)
    }

    /// Restores persisted alert rule settings without checking them;
    /// problems are reported by [`Self::validate`].
    pub fn restore_alert_rules(&mut self, settings: Vec<fm_types::alert_rules::AlertRuleSetting>) {
        self.alert_rules = settings;
    }

    /// The `LiveLAN` viewer stream's configuration (OR-017, schema 53).
    #[must_use]
    pub const fn live_lan(&self) -> &fm_types::live_lan::LiveLanConfig {
        &self.live_lan
    }

    /// Replaces the `LiveLAN` configuration and returns the previous one.
    ///
    /// # Errors
    ///
    /// Returns a [`fm_types::live_lan::LiveLanConfigError`] for a viewer
    /// limit out of range or an unknown source output. Nothing changes on
    /// error.
    pub fn set_live_lan(
        &mut self,
        config: fm_types::live_lan::LiveLanConfig,
    ) -> Result<fm_types::live_lan::LiveLanConfig, fm_types::live_lan::LiveLanConfigError> {
        let outputs = self
            .outputs
            .iter()
            .map(|output| output.id)
            .collect::<Vec<_>>();
        config.check(&outputs)?;
        Ok(std::mem::replace(&mut self.live_lan, config))
    }

    /// Restores a persisted `LiveLAN` configuration without checking it;
    /// problems are reported by [`Self::validate`].
    pub fn restore_live_lan(&mut self, config: fm_types::live_lan::LiveLanConfig) {
        self.live_lan = config;
    }

    /// Chooses whether Fade to Black fades `output`. Unknown outputs are
    /// reported by [`Self::validate`].
    pub fn set_fade_to_black_output(&mut self, output: OutputId, affected: bool) {
        if affected {
            self.fade_to_black_excluded_outputs.remove(&output);
        } else {
            self.fade_to_black_excluded_outputs.insert(output);
        }
    }

    #[must_use]
    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }

    #[must_use]
    pub fn audio_buses(&self) -> &[AudioBus] {
        &self.audio_buses
    }

    #[must_use]
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    #[must_use]
    pub fn stream_targets(&self) -> &[StreamTarget] {
        &self.stream_targets
    }

    #[must_use]
    pub const fn main_mix(&self) -> Option<MainMix> {
        self.main_mix
    }

    /// Whether recording is desired to be active for `slot`. Authored intent
    /// persisted with the project, not a live recorder status.
    #[must_use]
    pub const fn recording_desired(&self, slot: RecorderSlot) -> bool {
        self.recording_desired[slot.index()]
    }

    /// Every slot's desired recording flag, in slot order.
    #[must_use]
    pub const fn recording_desired_slots(&self) -> [bool; RECORDER_SLOT_COUNT] {
        self.recording_desired
    }

    #[must_use]
    pub fn stingers(&self) -> &[StingerConfig] {
        &self.stingers
    }

    #[must_use]
    pub const fn restart_policy(&self) -> RestartPolicy {
        self.restart_policy
    }

    /// The playlist definition and its persisted automation state.
    #[must_use]
    pub const fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    /// Replaces the playlist without checking it; [`Project::validate`]
    /// reports structural errors and entries naming unknown inputs.
    pub fn set_playlist(&mut self, playlist: Playlist) {
        self.playlist = playlist;
    }

    /// The cue lists, in display order, with their run states (RC-017).
    #[must_use]
    pub fn cue_lists(&self) -> &[crate::CueList] {
        &self.cue_lists
    }

    /// Replaces the cue lists without checking them; [`Project::validate`]
    /// reports structural errors and actions naming unknown inputs or
    /// stream targets.
    pub fn set_cue_lists(&mut self, cue_lists: Vec<crate::CueList>) {
        self.cue_lists = cue_lists;
    }

    /// Every media list input's list, by input (IN-006).
    #[must_use]
    pub const fn media_lists(&self) -> &BTreeMap<InputId, crate::MediaList> {
        &self.media_lists
    }

    /// Media list input `input`'s list.
    #[must_use]
    pub fn media_list(&self, input: InputId) -> Option<&crate::MediaList> {
        self.media_lists.get(&input)
    }

    /// Replaces every media list without checking them; [`Project::validate`]
    /// reports lists that do not fit their inputs.
    pub fn set_media_lists(&mut self, lists: BTreeMap<InputId, crate::MediaList>) {
        self.media_lists = lists;
    }

    /// Where each shown title input's timeline started (GX-001, schema 68),
    /// ascending by input. Runtime state the engine owns.
    #[must_use]
    pub const fn title_timelines(&self) -> &BTreeMap<InputId, crate::TitleTimeline> {
        &self.title_timelines
    }

    /// Replaces the title timelines without checking them;
    /// [`Project::validate`] reports one naming an input that is not a
    /// title.
    pub fn set_title_timelines(&mut self, timelines: BTreeMap<InputId, crate::TitleTimeline>) {
        self.title_timelines = timelines;
    }

    /// Every screen capture input's desktop restore token (IN-011, schema
    /// 72), by input: what a Wayland desktop's screen sharing dialog last
    /// granted, so the same screen or window is shared again without
    /// asking. Not an edit: the server keeps it up to date.
    #[must_use]
    pub const fn screen_capture_restore_tokens(&self) -> &BTreeMap<InputId, String> {
        &self.screen_capture_restore_tokens
    }

    /// Replaces every restore token with `tokens`, keeping only valid tokens
    /// of screen capture inputs (see
    /// [`fm_types::screen_capture::is_screen_capture_restore_token`]).
    /// Returns whether anything changed.
    pub fn set_screen_capture_restore_tokens(&mut self, tokens: BTreeMap<InputId, String>) -> bool {
        let tokens: BTreeMap<InputId, String> = tokens
            .into_iter()
            .filter(|(input, token)| {
                fm_types::screen_capture::is_screen_capture_restore_token(token)
                    && self.inputs.iter().any(|candidate| {
                        candidate.id == *input
                            && matches!(candidate.kind, InputKind::ScreenCapture(_))
                    })
            })
            .collect();
        if tokens == self.screen_capture_restore_tokens {
            return false;
        }
        self.screen_capture_restore_tokens = tokens;
        true
    }

    /// Restores one persisted restore token without checking it;
    /// [`Self::validate`] reports a token of an unknown or other input, or
    /// an unusable token.
    pub fn restore_screen_capture_restore_token(&mut self, input: InputId, token: String) {
        self.screen_capture_restore_tokens.insert(input, token);
    }

    /// The project's scripts, in display order (RC-011, schema 54).
    #[must_use]
    pub fn scripts(&self) -> &[crate::Script] {
        &self.scripts
    }

    /// Replaces the scripts without checking them; [`Project::validate`]
    /// reports structural errors.
    pub fn set_scripts(&mut self, scripts: Vec<crate::Script>) {
        self.scripts = scripts;
    }

    /// The script runs waiting and each script's last fault (RC-011,
    /// schema 56; several runs since schema 60).
    #[must_use]
    pub const fn script_run(&self) -> &crate::ScriptRunState {
        &self.script_run
    }

    /// Replaces the script runs without checking them; [`Project::validate`]
    /// reports runs that do not fit the scripts.
    pub fn set_script_run(&mut self, script_run: crate::ScriptRunState) {
        self.script_run = script_run;
    }

    /// The project's user shortcuts, in display order (RC-004, schema 63).
    #[must_use]
    pub fn shortcuts(&self) -> &[crate::Shortcut] {
        &self.shortcuts
    }

    /// Replaces the shortcuts without checking them; [`Project::validate`]
    /// reports structural errors.
    pub fn set_shortcuts(&mut self, shortcuts: Vec<crate::Shortcut>) {
        self.shortcuts = shortcuts;
    }

    /// Every persisted grading value: the grade library, imported LUTs, and
    /// the per-input and program grades.
    #[must_use]
    pub const fn grading(&self) -> &ProjectGrading {
        &self.grading
    }

    /// Library grades in authored order.
    #[must_use]
    pub fn grades(&self) -> &[ProjectGrade] {
        &self.grading.grades
    }

    /// Imported LUTs in import order.
    #[must_use]
    pub fn luts(&self) -> &[ProjectLut] {
        &self.grading.luts
    }

    /// The grade one input carries, if any.
    #[must_use]
    pub fn input_grade(&self, input: InputId) -> Option<&GradeSource> {
        self.grading.inputs.get(&input)
    }

    /// The grade one scene layer carries, if any.
    #[must_use]
    pub fn layer_grade(&self, layer: SceneLayerRef) -> Option<&GradeSource> {
        self.grading.layers.get(&layer)
    }

    /// The program output grade, if any.
    #[must_use]
    pub const fn program_grade(&self) -> Option<&GradeSource> {
        self.grading.program.as_ref()
    }

    /// Replaces every grading value without checking it; [`Project::validate`]
    /// reports bounds, duplicate ids and names, and dangling references.
    pub fn set_grading(&mut self, grading: ProjectGrading) {
        self.grading = grading;
    }

    /// The animation preset library (GX-011, schema 34).
    #[must_use]
    pub const fn animation_presets(&self) -> &crate::AnimationPresetLibrary {
        &self.animation_presets
    }

    /// Replaces the animation preset library without checking it;
    /// [`Project::validate`] reports its bound, duplicate ids and names, and
    /// invalid names.
    pub fn set_animation_presets(&mut self, presets: crate::AnimationPresetLibrary) {
        self.animation_presets = presets;
    }

    /// The animation preset library, to edit with its checked methods.
    pub const fn animation_presets_mut(&mut self) -> &mut crate::AnimationPresetLibrary {
        &mut self.animation_presets
    }

    /// The favourite transition buttons (SW-002, schema 35).
    #[must_use]
    pub const fn transition_favorites(&self) -> &crate::TransitionFavorites {
        &self.transition_favorites
    }

    /// Replaces the favourite transition buttons; each favourite is valid by
    /// construction.
    pub const fn set_transition_favorites(&mut self, favorites: crate::TransitionFavorites) {
        self.transition_favorites = favorites;
    }

    /// Adds a named grade to the end of the library.
    ///
    /// # Errors
    ///
    /// Refuses a full library, a used id or name, an invalid name, and a grade
    /// that violates a bound or names a LUT that was not imported.
    pub fn add_grade(&mut self, grade: ProjectGrade) -> Result<(), GradeEditError> {
        self.grading.insert_grade(None, grade)
    }

    /// Replaces a library grade's values; every target referencing it follows.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and an invalid grade.
    pub fn replace_grade(
        &mut self,
        id: GradeId,
        grade: fm_grade::Grade,
    ) -> Result<(), GradeEditError> {
        let name = self
            .grading
            .grade(id)
            .ok_or(GradeEditError::UnknownGrade(id))?
            .name
            .clone();
        self.grading.update_grade(id, name, grade)
    }

    /// Removes a library grade no input or program references.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and a grade that is still assigned.
    pub fn remove_grade(&mut self, id: GradeId) -> Result<ProjectGrade, GradeEditError> {
        self.grading.remove_grade(id).map(|(_, grade)| grade)
    }

    /// Registers an imported LUT. The caller places its file at
    /// `assets/luts/<id>.cube`.
    ///
    /// # Errors
    ///
    /// Refuses a full LUT table, a used id or name, an invalid name, and a
    /// size outside the `.cube` bounds for its kind.
    pub fn add_lut(&mut self, lut: ProjectLut) -> Result<(), GradeEditError> {
        self.grading.add_lut(lut)
    }

    /// Unregisters a LUT no grade references. The bundled file is left for
    /// the caller to remove.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and a LUT that is still referenced.
    pub fn remove_lut(&mut self, id: LutId) -> Result<ProjectLut, GradeEditError> {
        self.grading.remove_lut(id)
    }

    /// Sets or clears one input's grade.
    ///
    /// # Errors
    ///
    /// Refuses an unknown input, an unknown library grade, and an invalid
    /// inline grade.
    pub fn set_input_grade(
        &mut self,
        input: InputId,
        grade: Option<GradeSource>,
    ) -> Result<(), GradeEditError> {
        if !self.inputs.iter().any(|candidate| candidate.id == input) {
            return Err(GradeEditError::UnknownInput(input));
        }
        self.grading.set_input(input, grade)
    }

    /// Sets or clears one scene layer's grade.
    ///
    /// # Errors
    ///
    /// Refuses an unknown scene or layer, an unknown library grade, an
    /// invalid inline grade, and a new graded layer beyond
    /// [`crate::MAX_PROJECT_LAYER_GRADES`].
    pub fn set_layer_grade(
        &mut self,
        layer: SceneLayerRef,
        grade: Option<GradeSource>,
    ) -> Result<(), GradeEditError> {
        let scene = self
            .scene(layer.scene)
            .ok_or(GradeEditError::UnknownScene(layer.scene))?;
        if scene.layer_by_id(layer.layer).is_none() {
            return Err(GradeEditError::UnknownLayer(layer));
        }
        self.grading.set_layer(layer, grade)
    }

    /// Sets or clears the program output grade.
    ///
    /// # Errors
    ///
    /// Refuses an unknown library grade and an invalid inline grade.
    pub fn set_program_grade(&mut self, grade: Option<GradeSource>) -> Result<(), GradeEditError> {
        self.grading.set_program(grade)
    }

    /// Whether the project-wide edit padlock is engaged.
    ///
    /// The padlock is persisted operator intent. The model stores it but does
    /// not enforce it: engines restored from the project refuse locked edits,
    /// while direct model mutations (for example offline input removal) are
    /// unaffected.
    #[must_use]
    pub const fn project_locked(&self) -> bool {
        self.locked
    }

    /// Engages or releases the project-wide edit padlock.
    pub const fn set_project_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    /// Inputs whose edit padlock is engaged, in identifier order.
    #[must_use]
    pub const fn locked_inputs(&self) -> &BTreeSet<InputId> {
        &self.locked_inputs
    }

    /// Whether one input's edit padlock is engaged.
    #[must_use]
    pub fn is_input_locked(&self, input: InputId) -> bool {
        self.locked_inputs.contains(&input)
    }

    /// Engages or releases one input's edit padlock without checking that the
    /// input exists; [`Project::validate`] reports a dangling lock.
    pub fn set_input_locked(&mut self, input: InputId, locked: bool) {
        if locked {
            self.locked_inputs.insert(input);
        } else {
            self.locked_inputs.remove(&input);
        }
    }

    /// Engages or releases one input's edit padlock.
    ///
    /// # Errors
    ///
    /// Returns [`SetInputLockError::UnknownInput`] without mutation when the
    /// input does not exist.
    pub fn set_input_locked_checked(
        &mut self,
        input: InputId,
        locked: bool,
    ) -> Result<(), SetInputLockError> {
        if !self.inputs.iter().any(|candidate| candidate.id == input) {
            return Err(SetInputLockError::UnknownInput(input));
        }
        self.set_input_locked(input, locked);
        Ok(())
    }

    pub fn add_input(&mut self, input: Input) {
        self.input_audio_strips.push(InputAudioStrip {
            input: input.id,
            state: InputAudioStripState::default(),
            label: String::new(),
            group: None,
            dsp: fm_types::strip_dsp::StripDsp::DEFAULT,
            plugins: fm_types::audio_plugins::PluginChainState::new(),
        });
        self.inputs.push(input);
    }

    /// Adds an input after checking id uniqueness and name validity.
    ///
    /// # Errors
    ///
    /// Returns [`AddInputError::DuplicateId`] when the id already exists,
    /// [`AddInputError::EmptyName`] or [`AddInputError::NameTooLong`] when the
    /// name fails validation, and [`AddInputError::DuplicateName`] when another
    /// input already uses the name.
    pub fn add_input_checked(&mut self, input: Input) -> Result<(), AddInputError> {
        if self.inputs.iter().any(|candidate| candidate.id == input.id) {
            return Err(AddInputError::DuplicateId(input.id));
        }
        validate_input_name(&input.name).map_err(|error| match error {
            fm_types::RenameInputError::EmptyName => AddInputError::EmptyName,
            fm_types::RenameInputError::NameTooLong => AddInputError::NameTooLong,
            fm_types::RenameInputError::UnknownInput(_)
            | fm_types::RenameInputError::DuplicateName => unreachable!(),
        })?;
        if self
            .inputs
            .iter()
            .any(|candidate| candidate.name == input.name)
        {
            return Err(AddInputError::DuplicateName);
        }
        self.add_input(input);
        Ok(())
    }

    /// Returns the first persisted reference to `input` that keeps it from
    /// being removed: the main mix, a Stinger slot, a replay source, a reel
    /// clip angle, a Scene layer, a Scene input's audio source, or an alias
    /// input's target.
    #[must_use]
    pub fn input_reference(&self, input: InputId) -> Option<InputReference> {
        if let Some(mix) = self.main_mix {
            if mix.desired_program == input {
                return Some(InputReference::Program);
            }
            if mix.desired_preview == input {
                return Some(InputReference::Preview);
            }
        }
        if let Some(stinger) = self
            .stingers
            .iter()
            .find(|stinger| stinger.media_input == input)
        {
            return Some(InputReference::Stinger(stinger.slot));
        }
        if self.replay.sources.contains(&input) {
            return Some(InputReference::ReplaySource);
        }
        if let Some(reel) = self
            .reels
            .iter()
            .find(|reel| reel.clips.iter().any(|clip| clip.angle == input))
        {
            return Some(InputReference::ReelClip { reel: reel.id });
        }
        if let Some(scene) = self.scenes.iter().find(|scene| {
            scene.layers.iter().any(|layer| {
                layer.source == SourceRef::Input(input)
                    || layer.key.and_then(crate::LayerKey::key_input) == Some(input)
            })
        }) {
            return Some(InputReference::SceneLayer(scene.id));
        }
        self.inputs
            .iter()
            .find_map(|candidate| match candidate.kind {
                InputKind::Scene {
                    audio_source: Some(source),
                    ..
                } if source == input => Some(InputReference::SceneAudioSource(candidate.id)),
                InputKind::Alias { target, .. } if target == input => {
                    Some(InputReference::AliasTarget(candidate.id))
                }
                InputKind::Scene { .. }
                | InputKind::Alias { .. }
                | InputKind::Color
                | InputKind::Media { .. }
                | InputKind::Device { .. }
                | InputKind::NetworkStream(_)
                | InputKind::Replay
                | InputKind::SystemAudio(_)
                | InputKind::Image(_)
                | InputKind::Slideshow(_)
                | InputKind::Document(_)
                | InputKind::ScreenCapture(_)
                | InputKind::MixFeed(_)
                | InputKind::Title(_)
                | InputKind::Simulated(_) => None,
            })
            .or_else(|| {
                self.playlist
                    .entries
                    .iter()
                    .find(|entry| entry.input == input)
                    .map(|entry| InputReference::PlaylistEntry(entry.id))
            })
            .or_else(|| {
                self.cue_lists.iter().find_map(|list| {
                    list.cues
                        .iter()
                        .find(|cue| {
                            cue.actions
                                .iter()
                                .any(|action| action.input() == Some(input))
                        })
                        .map(|cue| InputReference::CueAction {
                            list: list.id,
                            cue: cue.id,
                        })
                })
            })
    }

    /// Removes an input together with its persisted audio strip and edit
    /// padlock. Live removal enforces the padlock in the engine; removing a
    /// locked input offline drops its lock rather than leaving a dangling
    /// reference.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveInputError::UnknownInput`] when the input does not
    /// exist and [`RemoveInputError::DomainReference`] when it is referenced
    /// by the main mix, a stinger, a replay source, a reel clip, a scene
    /// layer, another input's audio source, or an alias input's target.
    pub fn remove_input(&mut self, input: InputId) -> Result<(), RemoveInputError> {
        if !self.inputs.iter().any(|candidate| candidate.id == input) {
            return Err(RemoveInputError::UnknownInput(input));
        }
        if self.input_reference(input).is_some() {
            return Err(RemoveInputError::DomainReference(input));
        }
        self.inputs.retain(|candidate| candidate.id != input);
        self.input_audio_strips.retain(|strip| strip.input != input);
        self.locked_inputs.remove(&input);
        self.grading.inputs.remove(&input);
        self.input_lifecycle.remove(&input);
        self.media_lists.remove(&input);
        self.title_timelines.remove(&input);
        self.screen_capture_restore_tokens.remove(&input);
        Ok(())
    }

    /// The project's input category list, in display order (IN-026).
    #[must_use]
    pub fn input_categories(&self) -> &[crate::InputCategory] {
        &self.input_categories
    }

    /// Replaces the input category list.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::InputLifecycleError`] when the list breaks a
    /// bound or repeats an id or name, or when a category it drops is still
    /// assigned to an input. Nothing changes on error.
    pub fn set_input_categories(
        &mut self,
        categories: Vec<crate::InputCategory>,
    ) -> Result<(), crate::InputLifecycleError> {
        crate::validate_input_categories(&categories)?;
        for (input, lifecycle) in &self.input_lifecycle {
            if let Some(category) = lifecycle.category
                && !categories.iter().any(|candidate| candidate.id == category)
            {
                return Err(crate::InputLifecycleError::CategoryInUse {
                    category,
                    input: *input,
                });
            }
        }
        self.input_categories = categories;
        Ok(())
    }

    /// Every input whose lifecycle differs from the default, by id.
    #[must_use]
    pub const fn input_lifecycles(&self) -> &BTreeMap<InputId, crate::InputLifecycle> {
        &self.input_lifecycle
    }

    /// One input's lifecycle (the default for an input without an entry).
    #[must_use]
    pub fn input_lifecycle(&self, input: InputId) -> crate::InputLifecycle {
        self.input_lifecycle
            .get(&input)
            .copied()
            .unwrap_or_default()
    }

    /// Sets one input's lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`crate::InputLifecycleError::UnknownInput`] or
    /// [`crate::InputLifecycleError::UnknownCategory`]. Nothing changes on
    /// error.
    pub fn set_input_lifecycle(
        &mut self,
        input: InputId,
        lifecycle: crate::InputLifecycle,
    ) -> Result<(), crate::InputLifecycleError> {
        if !self.inputs.iter().any(|candidate| candidate.id == input) {
            return Err(crate::InputLifecycleError::UnknownInput(input));
        }
        if let Some(category) = lifecycle.category
            && !self
                .input_categories
                .iter()
                .any(|candidate| candidate.id == category)
        {
            return Err(crate::InputLifecycleError::UnknownCategory(category));
        }
        self.restore_input_lifecycle(input, lifecycle);
        Ok(())
    }

    /// Sets one input's lifecycle without checking references; a decoder
    /// restores persisted entries this way and [`Project::validate`]
    /// reports a dangling input or category.
    pub fn restore_input_lifecycle(&mut self, input: InputId, lifecycle: crate::InputLifecycle) {
        if lifecycle.is_default() {
            self.input_lifecycle.remove(&input);
        } else {
            self.input_lifecycle.insert(input, lifecycle);
        }
    }

    /// Restores the category list without validation; [`Project::validate`]
    /// reports a broken list.
    pub fn restore_input_categories(&mut self, categories: Vec<crate::InputCategory>) {
        self.input_categories = categories;
    }

    /// Appends a copy of `source` as `input` named `name`: the same kind
    /// and required capabilities, Master strip, category, and colour label.
    /// The copy starts neither paused nor closed and without a padlock or a
    /// grade.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateInputError::UnknownInput`] for an unknown source
    /// and [`DuplicateInputError::Add`] for a taken id or an unusable name.
    /// Nothing changes on error.
    pub fn duplicate_input(
        &mut self,
        source: InputId,
        input: InputId,
        name: String,
    ) -> Result<(), DuplicateInputError> {
        let original = self
            .inputs
            .iter()
            .find(|candidate| candidate.id == source)
            .ok_or(DuplicateInputError::UnknownInput(source))?
            .clone();
        let strip = self.input_audio_strip(source);
        let group = self.input_audio_group(source);
        let lifecycle = self.input_lifecycle(source);
        self.add_input_checked(Input {
            id: input,
            name,
            kind: original.kind,
            required_capabilities: original.required_capabilities,
        })
        .map_err(DuplicateInputError::Add)?;
        if let Some(strip) = strip {
            self.set_input_audio_strip(input, strip);
        }
        // The duplicate keeps the strip's group but not its label, which
        // names the original.
        self.set_input_audio_group(input, group);
        self.restore_input_lifecycle(
            input,
            crate::InputLifecycle {
                category: lifecycle.category,
                color: lifecycle.color,
                paused: false,
                closed: false,
            },
        );
        Ok(())
    }

    /// Renames one input while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`RenameInputError`] when the input is unknown or the name is
    /// blank, too long, or already used by another input.
    pub fn rename_input(&mut self, input: InputId, name: String) -> Result<(), RenameInputError> {
        let index = self
            .inputs
            .iter()
            .position(|candidate| candidate.id == input)
            .ok_or(RenameInputError::UnknownInput(input))?;
        validate_input_name(&name)?;
        if self
            .inputs
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| candidate_index != index && candidate.name == name)
        {
            return Err(RenameInputError::DuplicateName);
        }
        self.inputs[index].name = name;
        Ok(())
    }

    /// Replaces an input's source kind and required capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`ReplaceInputError::UnknownInput`] when the input does not
    /// exist, [`ReplaceInputError::MissingScene`] when a scene source names a
    /// scene that does not exist, and [`ReplaceInputError::SourceCycle`] when
    /// the replacement would make the input's scene graph reach itself, and
    /// [`ReplaceInputError::SceneKeyInput`] when it would make a key/fill
    /// key input render a scene (an alias re-pointed at a Scene input).
    pub fn replace_input_source(
        &mut self,
        input: InputId,
        kind: InputKind,
        required_capabilities: Vec<String>,
    ) -> Result<(), ReplaceInputError> {
        let index = self
            .inputs
            .iter()
            .position(|candidate| candidate.id == input)
            .ok_or(ReplaceInputError::UnknownInput(input))?;
        if let InputKind::Scene { scene_id, .. } = kind
            && !self.scenes.iter().any(|scene| scene.id == scene_id)
        {
            return Err(ReplaceInputError::MissingScene(scene_id));
        }
        if let InputKind::Alias { target, .. } = kind
            && !self.inputs.iter().any(|candidate| candidate.id == target)
        {
            return Err(ReplaceInputError::MissingInput(target));
        }
        let mut candidate = self.clone();
        candidate.inputs[index].kind = kind;
        candidate.inputs[index].required_capabilities = required_capabilities;
        if candidate.validate().is_err_and(|errors| {
            errors.iter().any(|error| {
                matches!(
                    error.kind,
                    ValidationErrorKind::SelfReference | ValidationErrorKind::Cycle
                )
            })
        }) {
            return Err(ReplaceInputError::SourceCycle);
        }
        // A key/fill key input is a leaf picture: refuse a replacement that
        // makes one render a scene (validation would report it).
        if let Some(key_input) = candidate
            .scenes
            .iter()
            .flat_map(|scene| &scene.layers)
            .filter_map(|layer| layer.key.and_then(crate::LayerKey::key_input))
            .find(|key_input| {
                candidate.input_renders_scene(*key_input) && !self.input_renders_scene(*key_input)
            })
        {
            return Err(ReplaceInputError::SceneKeyInput(key_input));
        }
        // A desktop restore token restores a screen or a window: it is
        // kept only while the input still captures the same kind of target.
        let same_target = |kind: &InputKind| match kind {
            InputKind::ScreenCapture(config) => Some(config.target.kind_token()),
            _ => None,
        };
        if same_target(&self.inputs[index].kind).is_none()
            || same_target(&self.inputs[index].kind) != same_target(&candidate.inputs[index].kind)
        {
            candidate.screen_capture_restore_tokens.remove(&input);
        }
        *self = candidate;
        Ok(())
    }

    /// Rewrites the file asset URIs of input `input` (see
    /// [`InputKind::map_file_asset_uris`]): a bundle export or append
    /// moving its media file, image, or listed slides to new keys. Returns
    /// whether anything changed; an unknown input changes nothing.
    pub fn map_input_file_assets(
        &mut self,
        input: InputId,
        map: impl FnMut(&str) -> Option<String>,
    ) -> bool {
        self.inputs
            .iter_mut()
            .find(|candidate| candidate.id == input)
            .is_some_and(|input| input.kind.map_file_asset_uris(map))
    }

    /// Points an existing media or document input (IN-014) at a new asset
    /// URI. For a media list input, every item of the old file moves to the
    /// new one too.
    ///
    /// # Errors
    ///
    /// Returns [`RelinkMediaInputError::UnknownInput`] when the input does not
    /// exist and [`RelinkMediaInputError::NotMediaInput`] when it is not a
    /// media input.
    pub fn relink_media_input(
        &mut self,
        input: InputId,
        asset_uri: String,
    ) -> Result<(), RelinkMediaInputError> {
        let index = self
            .inputs
            .iter()
            .position(|candidate| candidate.id == input)
            .ok_or(RelinkMediaInputError::UnknownInput(input))?;
        let previous = match &mut self.inputs[index].kind {
            InputKind::Media {
                asset_uri: current, ..
            }
            | InputKind::Document(crate::DocumentInput {
                asset_uri: current, ..
            }) => std::mem::replace(current, asset_uri),
            _ => return Err(RelinkMediaInputError::NotMediaInput(input)),
        };
        // A media list input plays its active item's file (IN-006): every
        // item of the relinked file follows it.
        if let Some(list) = self.media_lists.get_mut(&input) {
            let relinked = match &self.inputs[index].kind {
                InputKind::Media { asset_uri, .. } => asset_uri.clone(),
                _ => return Ok(()),
            };
            for item in &mut list.items {
                if item.asset_uri == previous {
                    item.asset_uri.clone_from(&relinked);
                }
            }
        }
        Ok(())
    }

    /// Replaces a media input's playback settings.
    ///
    /// # Errors
    ///
    /// Returns [`crate::SetMediaPlaybackError`] for an unknown or non-media
    /// input or settings outside their bounds. Nothing changes on error.
    pub fn set_media_playback(
        &mut self,
        input: InputId,
        settings: crate::MediaPlaybackSettings,
    ) -> Result<(), crate::SetMediaPlaybackError> {
        settings
            .validate()
            .map_err(crate::SetMediaPlaybackError::Invalid)?;
        let target = self
            .inputs
            .iter_mut()
            .find(|candidate| candidate.id == input)
            .ok_or(crate::SetMediaPlaybackError::UnknownInput(input))?;
        let InputKind::Media { playback, .. } = &mut target.kind else {
            return Err(crate::SetMediaPlaybackError::NotMediaInput(input));
        };
        *playback = settings;
        Ok(())
    }

    /// Replaces capture input `input`'s settings (IN-001).
    ///
    /// # Errors
    ///
    /// Returns [`crate::SetCaptureSettingsError`] for an unknown input, an
    /// input that is not a capture device, or invalid settings. Nothing
    /// changes on error.
    pub fn set_capture_settings(
        &mut self,
        input: InputId,
        settings: fm_types::capture::CaptureSettings,
    ) -> Result<(), crate::SetCaptureSettingsError> {
        settings
            .validate()
            .map_err(crate::SetCaptureSettingsError::Invalid)?;
        let target = self
            .inputs
            .iter_mut()
            .find(|candidate| candidate.id == input)
            .ok_or(crate::SetCaptureSettingsError::UnknownInput(input))?;
        let InputKind::Device { capture, .. } = &mut target.kind else {
            return Err(crate::SetCaptureSettingsError::NotCaptureInput(input));
        };
        *capture = settings;
        Ok(())
    }

    /// The stable key and settings of capture input `input` (IN-001).
    #[must_use]
    pub fn capture_input(
        &self,
        input: InputId,
    ) -> Option<(&str, &fm_types::capture::CaptureSettings)> {
        self.inputs
            .iter()
            .find(|candidate| candidate.id == input)
            .and_then(|candidate| match &candidate.kind {
                InputKind::Device {
                    stable_key,
                    capture,
                } => Some((stable_key.as_str(), capture)),
                _ => None,
            })
    }

    /// The playback settings of media input `input`.
    #[must_use]
    pub fn media_playback(&self, input: InputId) -> Option<&crate::MediaPlaybackSettings> {
        self.inputs
            .iter()
            .find(|candidate| candidate.id == input)
            .and_then(|candidate| match &candidate.kind {
                InputKind::Media { playback, .. } => Some(playback),
                _ => None,
            })
    }

    /// Document input `input`'s file and page (IN-014), or `None` when
    /// `input` is not a document input.
    #[must_use]
    pub fn document(&self, input: InputId) -> Option<&crate::DocumentInput> {
        self.inputs
            .iter()
            .find(|candidate| candidate.id == input)
            .and_then(|candidate| match &candidate.kind {
                InputKind::Document(document) => Some(document),
                _ => None,
            })
    }

    /// Every document input and its file and page, in project order.
    pub fn documents(&self) -> impl Iterator<Item = (InputId, &crate::DocumentInput)> {
        self.inputs.iter().filter_map(|input| match &input.kind {
            InputKind::Document(document) => Some((input.id, document)),
            _ => None,
        })
    }

    /// Moves document input `input` to zero-based `page` (IN-014). Returns
    /// `false` (and changes nothing) for an input that is not a document
    /// input or a page past [`fm_types::paged::MAX_PAGED_PAGES`].
    pub fn set_document_page(&mut self, input: InputId, page: u32) -> bool {
        if page >= fm_types::paged::MAX_PAGED_PAGES {
            return false;
        }
        match self
            .inputs
            .iter_mut()
            .find(|candidate| candidate.id == input)
            .map(|candidate| &mut candidate.kind)
        {
            Some(InputKind::Document(document)) => {
                document.page = page;
                true
            }
            _ => false,
        }
    }

    /// Replaces a title input's field values and field revision.
    ///
    /// # Errors
    ///
    /// Returns [`SetTitleFieldsError::UnknownInput`] when the input does not
    /// exist, [`SetTitleFieldsError::NotTitleInput`] when it is not a title
    /// input, [`SetTitleFieldsError::RevisionRegressed`] when
    /// `field_revision` is older than the stored revision, and
    /// [`SetTitleFieldsError::InvalidFields`] when the fields break a bound,
    /// repeat or are out of id order, or name a blank image. Nothing changes
    /// on error.
    pub fn set_title_fields(
        &mut self,
        input: InputId,
        field_revision: u64,
        fields: Vec<TitleFieldEntry>,
    ) -> Result<(), SetTitleFieldsError> {
        let target = self
            .inputs
            .iter_mut()
            .find(|candidate| candidate.id == input)
            .ok_or(SetTitleFieldsError::UnknownInput(input))?;
        let InputKind::Title(title) = &mut target.kind else {
            return Err(SetTitleFieldsError::NotTitleInput(input));
        };
        if field_revision < title.field_revision {
            return Err(SetTitleFieldsError::RevisionRegressed {
                current: title.field_revision,
                requested: field_revision,
            });
        }
        validate_title_fields(field_revision, &fields)
            .map_err(SetTitleFieldsError::InvalidFields)?;
        *title = TitleInput {
            template_uri: std::mem::take(&mut title.template_uri),
            field_revision,
            fields,
        };
        Ok(())
    }

    /// Reorders the project's inputs to match the supplied id sequence.
    ///
    /// # Errors
    ///
    /// Returns [`InputOrderError::EmptyOrder`] for an empty request,
    /// [`InputOrderError::WrongLength`] on a length mismatch,
    /// [`InputOrderError::UnknownInput`] for an input outside the current set,
    /// and [`InputOrderError::DuplicateInput`] when an input repeats.
    ///
    /// # Panics
    ///
    /// Panics if a validated order references an input missing from the
    /// project; unreachable because [`validate_input_order`] already rejected
    /// unknown ids.
    pub fn reorder_inputs(&mut self, inputs: &[InputId]) -> Result<(), InputOrderError> {
        let current = self.inputs.iter().map(|input| input.id).collect::<Vec<_>>();
        validate_input_order(&current, inputs)?;
        let reordered = inputs
            .iter()
            .map(|input| {
                self.inputs
                    .iter()
                    .find(|candidate| candidate.id == *input)
                    .expect("validated input order contains only project inputs")
                    .clone()
            })
            .collect();
        self.inputs = reordered;
        Ok(())
    }

    /// Replaces the persisted audio strip for an existing input.
    ///
    /// Returns `false` when `input` is not part of this project.
    pub fn set_input_audio_strip(&mut self, input: InputId, state: InputAudioStripState) -> bool {
        let Some(strip) = self
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
        else {
            return false;
        };
        strip.state = state;
        true
    }

    pub fn add_scene(&mut self, scene: Scene) {
        self.scenes.push(scene);
    }

    /// Adds a scene together with its dedicated scene input after checking
    /// id and name invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AddSceneInputError::DuplicateSceneId`] when the scene id or
    /// [`AddSceneInputError::DuplicateInputId`] when the input id already
    /// exists, [`AddSceneInputError::EmptySceneName`],
    /// [`AddSceneInputError::DuplicateSceneName`],
    /// [`AddSceneInputError::EmptyInputName`],
    /// [`AddSceneInputError::InputNameTooLong`], and
    /// [`AddSceneInputError::DuplicateInputName`] for name violations, and
    /// [`AddSceneInputError::InvalidProject`] when the composed project fails
    /// validation.
    pub fn add_scene_input_checked(
        &mut self,
        scene: SceneId,
        scene_name: String,
        input: InputId,
        input_name: String,
    ) -> Result<(), AddSceneInputError> {
        self.validate_new_scene_input(scene, &scene_name, input, &input_name)?;
        let mut candidate = self.clone();
        candidate.add_scene(Scene {
            id: scene,
            name: scene_name,
            background: Rgba8::OPAQUE_BLACK,
            layers: Vec::new(),
            animation: SceneAnimation::default(),
        });
        candidate.add_input(Input {
            id: input,
            name: input_name,
            kind: InputKind::Scene {
                scene_id: scene,
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
        candidate
            .validate()
            .map_err(AddSceneInputError::InvalidProject)?;
        *self = candidate;
        Ok(())
    }

    /// Duplicates a scene and its dedicated scene input under new ids.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateSceneInputError::UnknownSourceScene`] when the
    /// source scene does not exist, the same id and name violations as
    /// [`Project::add_scene_input_checked`]
    /// ([`DuplicateSceneInputError::DuplicateSceneId`],
    /// [`DuplicateSceneInputError::DuplicateInputId`],
    /// [`DuplicateSceneInputError::EmptySceneName`],
    /// [`DuplicateSceneInputError::DuplicateSceneName`],
    /// [`DuplicateSceneInputError::EmptyInputName`],
    /// [`DuplicateSceneInputError::InputNameTooLong`],
    /// [`DuplicateSceneInputError::DuplicateInputName`]), and
    /// [`DuplicateSceneInputError::InvalidProject`] when the composed project
    /// fails validation.
    pub fn duplicate_scene_input_checked(
        &mut self,
        source_scene: SceneId,
        new_scene: SceneId,
        scene_name: String,
        new_input: InputId,
        input_name: String,
    ) -> Result<(), DuplicateSceneInputError> {
        let source = self
            .scenes
            .iter()
            .find(|scene| scene.id == source_scene)
            .ok_or(DuplicateSceneInputError::UnknownSourceScene(source_scene))?;
        self.validate_new_scene_input(new_scene, &scene_name, new_input, &input_name)
            .map_err(DuplicateSceneInputError::from)?;
        let mut candidate = self.clone();
        let mut scene = source.clone();
        scene.id = new_scene;
        scene.name = scene_name;
        candidate.scenes.push(scene);
        // The copy keeps its layers' grades.
        let graded = self
            .grading
            .layers
            .iter()
            .filter(|(target, _)| target.scene == source_scene)
            .map(|(target, source)| (SceneLayerRef::new(new_scene, target.layer), source.clone()))
            .collect::<Vec<_>>();
        candidate.grading.layers.extend(graded);
        candidate.add_input(Input {
            id: new_input,
            name: input_name,
            kind: InputKind::Scene {
                scene_id: new_scene,
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
        candidate
            .validate()
            .map_err(DuplicateSceneInputError::InvalidProject)?;
        *self = candidate;
        Ok(())
    }

    /// Renames one scene while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`RenameSceneError::UnknownScene`] when the scene does not
    /// exist, [`RenameSceneError::EmptyName`] when the name is blank, and
    /// [`RenameSceneError::DuplicateName`] when another scene already uses
    /// the name.
    pub fn rename_scene(&mut self, scene: SceneId, name: String) -> Result<(), RenameSceneError> {
        let index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(RenameSceneError::UnknownScene(scene))?;
        if name.trim().is_empty() {
            return Err(RenameSceneError::EmptyName);
        }
        if self
            .scenes
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| {
                candidate_index != index && candidate.name.eq_ignore_ascii_case(&name)
            })
        {
            return Err(RenameSceneError::DuplicateName);
        }
        self.scenes[index].name = name;
        Ok(())
    }

    /// Removes a scene that no other project entity references.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveSceneError::UnknownScene`] when the scene does not
    /// exist and [`RemoveSceneError::InputReference`],
    /// [`RemoveSceneError::LayerReference`], or
    /// [`RemoveSceneError::OutputReference`] when an input, another scene's
    /// layer, or an output still references it.
    pub fn remove_scene(&mut self, scene: SceneId) -> Result<(), RemoveSceneError> {
        if !self.scenes.iter().any(|candidate| candidate.id == scene) {
            return Err(RemoveSceneError::UnknownScene(scene));
        }
        if let Some(input) = self.inputs.iter().find_map(|input| {
            matches!(input.kind, InputKind::Scene { scene_id, .. } if scene_id == scene)
                .then_some(input.id)
        }) {
            return Err(RemoveSceneError::InputReference { input, scene });
        }
        if let Some((owner, source)) = self.scenes.iter().find_map(|owner| {
            owner.layers.iter().find_map(|layer| {
                matches!(layer.source, SourceRef::Scene(source) if source == scene)
                    .then_some((owner.id, scene))
            })
        }) {
            return Err(RemoveSceneError::LayerReference { owner, source });
        }
        if let Some(output) = self
            .outputs
            .iter()
            .find(|output| output.video_source == scene)
            .map(|output| output.id)
        {
            return Err(RemoveSceneError::OutputReference { output, scene });
        }
        self.scenes.retain(|candidate| candidate.id != scene);
        self.grading.remove_scene_layers(scene);
        Ok(())
    }

    /// Sets a scene's premultiplied background color.
    ///
    /// # Errors
    ///
    /// Returns [`SetSceneBackgroundError::UnknownScene`] when the scene does
    /// not exist and [`SetSceneBackgroundError::NotPremultiplied`] when the
    /// color is not premultiplied.
    pub fn set_scene_background(
        &mut self,
        scene: SceneId,
        background: Rgba8,
    ) -> Result<(), SetSceneBackgroundError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SetSceneBackgroundError::UnknownScene(scene))?;
        if !background.is_premultiplied() {
            return Err(SetSceneBackgroundError::NotPremultiplied);
        }
        self.scenes[scene_index].background = background;
        Ok(())
    }

    /// The scene with id `scene`.
    #[must_use]
    pub fn scene(&self, scene: SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|candidate| candidate.id == scene)
    }

    /// Replaces one scene's animation after checking it animates only that
    /// scene's layers with scene properties.
    ///
    /// # Errors
    ///
    /// [`SetSceneAnimationError::UnknownScene`] or
    /// [`SetSceneAnimationError::Invalid`]; nothing changes on error.
    pub fn set_scene_animation(
        &mut self,
        scene: SceneId,
        animation: SceneAnimation,
    ) -> Result<(), SetSceneAnimationError> {
        let target = self
            .scenes
            .iter_mut()
            .find(|candidate| candidate.id == scene)
            .ok_or(SetSceneAnimationError::UnknownScene(scene))?;
        target
            .check_animation_clip(&animation.clip)
            .map_err(SetSceneAnimationError::Invalid)?;
        target.animation = animation;
        Ok(())
    }

    /// Replaces a scene's whole layer list (as a live scene layer edit left
    /// it), checking the project as a whole: every layer, source, cycle,
    /// animation track, and layer grade must stay valid.
    ///
    /// # Errors
    ///
    /// [`SetSceneLayersError::UnknownScene`] for a missing scene and
    /// [`SetSceneLayersError::Invalid`] with every validation error the new
    /// list would cause; nothing changes on error.
    pub fn set_scene_layers(
        &mut self,
        scene: SceneId,
        layers: Vec<Layer>,
    ) -> Result<(), SetSceneLayersError> {
        let index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SetSceneLayersError::UnknownScene(scene))?;
        let previous = core::mem::replace(&mut self.scenes[index].layers, layers);
        if let Err(errors) = self.validate() {
            self.scenes[index].layers = previous;
            return Err(SetSceneLayersError::Invalid(errors));
        }
        Ok(())
    }

    /// Appends a layer to a scene after checking the scene, name, and source.
    ///
    /// # Errors
    ///
    /// Returns [`AddSceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`AddSceneLayerError::EmptyName`] when the layer name is blank,
    /// [`AddSceneLayerError::MissingInput`] or
    /// [`AddSceneLayerError::MissingScene`] when the source references an
    /// entity that does not exist, and
    /// [`AddSceneLayerError::SourceCycle`] when the source would create a
    /// self-reference or cycle.
    pub fn add_layer_to_scene(
        &mut self,
        scene: SceneId,
        layer: Layer,
    ) -> Result<(), AddSceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(AddSceneLayerError::UnknownScene(scene))?;
        if layer.name.trim().is_empty() {
            return Err(AddSceneLayerError::EmptyName);
        }
        match layer.source {
            SourceRef::Input(input)
                if !self.inputs.iter().any(|candidate| candidate.id == input) =>
            {
                return Err(AddSceneLayerError::MissingInput(input));
            }
            SourceRef::Scene(source_scene)
                if !self
                    .scenes
                    .iter()
                    .any(|candidate| candidate.id == source_scene) =>
            {
                return Err(AddSceneLayerError::MissingScene(source_scene));
            }
            SourceRef::Input(_) | SourceRef::Scene(_) => {}
        }
        if layer.enabled && !self.scenes[scene_index].can_enable_another_layer() {
            return Err(AddSceneLayerError::TooManyEnabledLayers);
        }
        let mut layer = layer;
        // A layer keeps its id unless the scene already uses it (a duplicate
        // or a copy); then it takes the scene's next free id.
        if self.scenes[scene_index].layer_by_id(layer.id).is_some()
            && let Some(id) = self.scenes[scene_index].next_layer_id()
        {
            layer.id = id;
        }
        let mut candidate = self.clone();
        candidate.scenes[scene_index].layers.push(layer);
        if candidate.validate().is_err_and(|errors| {
            errors.iter().any(|error| {
                error.entity == Some(EntityRef::Scene(scene))
                    && error.field == "layers.source"
                    && matches!(
                        error.kind,
                        ValidationErrorKind::SelfReference | ValidationErrorKind::Cycle
                    )
            })
        }) {
            return Err(AddSceneLayerError::SourceCycle);
        }
        *self = candidate;
        Ok(())
    }

    /// Removes one layer from a scene and returns it.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist and [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count.
    pub fn remove_layer_from_scene(
        &mut self,
        scene: SceneId,
        index: usize,
    ) -> Result<Layer, SceneLayerError> {
        let target = self
            .scenes
            .iter_mut()
            .find(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        if index >= target.layers.len() {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length: target.layers.len(),
            });
        }
        let layer = target.layers.remove(index);
        // The removed layer's grade goes with it.
        self.grading
            .layers
            .remove(&SceneLayerRef::new(scene, layer.id));
        // The removed layer's tracks go with it.
        let animated = target
            .animation
            .clip
            .tracks()
            .iter()
            .any(|track| track.key.target == layer.id);
        if animated {
            let keys = target
                .animation
                .clip
                .tracks()
                .iter()
                .filter(|track| track.key.target == layer.id)
                .map(|track| track.key)
                .collect::<Vec<_>>();
            for key in keys {
                target.animation.clip.remove_track(key);
            }
            target.animation.revision = target.animation.revision.saturating_add(1);
        }
        Ok(layer)
    }

    /// Moves one layer within a scene to a new stacking position.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist and [`SceneLayerError::LayerIndexOutOfRange`] when either index
    /// is beyond the scene's layer count.
    pub fn move_scene_layer(
        &mut self,
        scene: SceneId,
        source_index: usize,
        destination_index: usize,
    ) -> Result<(), SceneLayerError> {
        let target = self
            .scenes
            .iter_mut()
            .find(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = target.layers.len();
        if source_index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index: source_index,
                length,
            });
        }
        if destination_index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index: destination_index,
                length,
            });
        }
        if source_index != destination_index {
            let layer = target.layers.remove(source_index);
            target.layers.insert(destination_index, layer);
        }
        Ok(())
    }

    /// Duplicates a scene layer in place under a new name.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::EmptyName`] when
    /// the new name is blank, and [`SceneLayerError::MissingInput`],
    /// [`SceneLayerError::MissingScene`], or
    /// [`SceneLayerError::SourceCycle`] when appending the copy fails.
    pub fn duplicate_scene_layer(
        &mut self,
        scene: SceneId,
        index: usize,
        name: String,
    ) -> Result<(), SceneLayerError> {
        let target = self
            .scenes
            .iter()
            .find(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = target.layers.len();
        let mut layer =
            target
                .layers
                .get(index)
                .cloned()
                .ok_or(SceneLayerError::LayerIndexOutOfRange {
                    scene,
                    index,
                    length,
                })?;
        if name.trim().is_empty() {
            return Err(SceneLayerError::EmptyName);
        }
        layer.name = name;
        self.add_layer_to_scene(scene, layer)
            .map_err(|error| match error {
                AddSceneLayerError::UnknownScene(scene) => SceneLayerError::UnknownScene(scene),
                AddSceneLayerError::EmptyName => SceneLayerError::EmptyName,
                AddSceneLayerError::MissingInput(input) => SceneLayerError::MissingInput(input),
                AddSceneLayerError::MissingScene(scene) => SceneLayerError::MissingScene(scene),
                AddSceneLayerError::SourceCycle => SceneLayerError::SourceCycle,
                AddSceneLayerError::TooManyEnabledLayers => {
                    SceneLayerError::TooManyEnabledLayers { scene }
                }
            })
    }

    /// Copies a layer from one scene into another under a new name.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when either scene does not
    /// exist, [`SceneLayerError::SameScene`] when both scenes are identical,
    /// [`SceneLayerError::LayerIndexOutOfRange`] when the index is beyond the
    /// source's layer count, [`SceneLayerError::EmptyName`] when the new name
    /// is blank, and [`SceneLayerError::MissingInput`],
    /// [`SceneLayerError::MissingScene`], or
    /// [`SceneLayerError::SourceCycle`] when appending the copy fails.
    pub fn copy_scene_layer(
        &mut self,
        source_scene: SceneId,
        index: usize,
        target_scene: SceneId,
        name: String,
    ) -> Result<(), SceneLayerError> {
        let source = self
            .scenes
            .iter()
            .find(|candidate| candidate.id == source_scene)
            .ok_or(SceneLayerError::UnknownScene(source_scene))?;
        let length = source.layers.len();
        let mut layer =
            source
                .layers
                .get(index)
                .cloned()
                .ok_or(SceneLayerError::LayerIndexOutOfRange {
                    scene: source_scene,
                    index,
                    length,
                })?;
        if !self
            .scenes
            .iter()
            .any(|candidate| candidate.id == target_scene)
        {
            return Err(SceneLayerError::UnknownScene(target_scene));
        }
        if source_scene == target_scene {
            return Err(SceneLayerError::SameScene(source_scene));
        }
        if name.trim().is_empty() {
            return Err(SceneLayerError::EmptyName);
        }
        layer.name = name;
        self.add_layer_to_scene(target_scene, layer)
            .map_err(|error| match error {
                AddSceneLayerError::UnknownScene(scene) => SceneLayerError::UnknownScene(scene),
                AddSceneLayerError::EmptyName => SceneLayerError::EmptyName,
                AddSceneLayerError::MissingInput(input) => SceneLayerError::MissingInput(input),
                AddSceneLayerError::MissingScene(scene) => SceneLayerError::MissingScene(scene),
                AddSceneLayerError::SourceCycle => SceneLayerError::SourceCycle,
                AddSceneLayerError::TooManyEnabledLayers => SceneLayerError::TooManyEnabledLayers {
                    scene: target_scene,
                },
            })
    }

    /// Sets one layer's z-order within its scene.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist and [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count.
    pub fn set_scene_layer_z_order(
        &mut self,
        scene: SceneId,
        index: usize,
        z_order: i32,
    ) -> Result<(), SceneLayerError> {
        let target = self
            .scenes
            .iter_mut()
            .find(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = target.layers.len();
        let layer = target
            .layers
            .get_mut(index)
            .ok_or(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            })?;
        layer.z_order = z_order;
        Ok(())
    }

    /// Renames one scene layer while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, and [`SceneLayerError::EmptyName`]
    /// when the name is blank.
    pub fn rename_scene_layer(
        &mut self,
        scene: SceneId,
        index: usize,
        name: String,
    ) -> Result<(), SceneLayerError> {
        let target = self
            .scenes
            .iter_mut()
            .find(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = target.layers.len();
        let layer = target
            .layers
            .get_mut(index)
            .ok_or(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            })?;
        if name.trim().is_empty() {
            return Err(SceneLayerError::EmptyName);
        }
        layer.name = name;
        Ok(())
    }

    /// Repoints one layer's source after checking references and cycles.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::MissingInput`] or
    /// [`SceneLayerError::MissingScene`] when the source references an entity
    /// that does not exist, and [`SceneLayerError::SourceCycle`] when the
    /// source would create a self-reference or cycle.
    pub fn set_scene_layer_source(
        &mut self,
        scene: SceneId,
        index: usize,
        source: SourceRef,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = self.scenes[scene_index].layers.len();
        if index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            });
        }
        match source {
            SourceRef::Input(input)
                if !self.inputs.iter().any(|candidate| candidate.id == input) =>
            {
                return Err(SceneLayerError::MissingInput(input));
            }
            SourceRef::Scene(source_scene)
                if !self
                    .scenes
                    .iter()
                    .any(|candidate| candidate.id == source_scene) =>
            {
                return Err(SceneLayerError::MissingScene(source_scene));
            }
            SourceRef::Input(_) | SourceRef::Scene(_) => {}
        }
        let mut cycle_probe = self.clone();
        cycle_probe.scenes[scene_index].layers[index].source = source;
        cycle_probe.scenes[scene_index].layers =
            vec![cycle_probe.scenes[scene_index].layers[index].clone()];
        if cycle_probe.validate().err().is_some_and(|errors| {
            errors.iter().any(|error| {
                error.kind == ValidationErrorKind::Cycle
                    && error.entity == Some(EntityRef::Scene(scene))
                    && error.field == "layers.source"
            })
        }) {
            return Err(SceneLayerError::SourceCycle);
        }
        self.scenes[scene_index].layers[index].source = source;
        Ok(())
    }

    /// Sets one layer's enabled flag and opacity.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist and [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count.
    pub fn set_scene_layer_appearance(
        &mut self,
        scene: SceneId,
        index: usize,
        enabled: bool,
        opacity: u8,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = self.scenes[scene_index].layers.len();
        if index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            });
        }
        let target = &self.scenes[scene_index];
        if enabled && !target.layers[index].enabled && !target.can_enable_another_layer() {
            return Err(SceneLayerError::TooManyEnabledLayers { scene });
        }
        let layer = &mut self.scenes[scene_index].layers[index];
        layer.enabled = enabled;
        layer.opacity = opacity;
        Ok(())
    }

    /// Sets one layer's geometry after range checks.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, and
    /// [`SceneLayerError::InvalidGeometry`] when a dimension is zero or
    /// exceeds the project limits, the tilt is outside its stored range, or
    /// the anchor lies outside the box.
    pub fn set_scene_layer_geometry(
        &mut self,
        scene: SceneId,
        index: usize,
        geometry: LayerGeometry,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = self.scenes[scene_index].layers.len();
        if index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            });
        }
        if !geometry.is_storable() {
            return Err(SceneLayerError::InvalidGeometry { scene, index });
        }
        self.scenes[scene_index].layers[index].geometry = geometry;
        Ok(())
    }

    /// Sets or clears one layer's crop rectangle.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::InvalidCrop`] when
    /// the crop is empty or exceeds the video dimensions, and
    /// [`SceneLayerError::CropWouldInvalidateMask`] when an existing mask
    /// would fall outside the new crop.
    pub fn set_scene_layer_crop(
        &mut self,
        scene: SceneId,
        index: usize,
        crop: Option<CropRect>,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let target = &self.scenes[scene_index];
        let length = target.layers.len();
        let layer = target
            .layers
            .get(index)
            .ok_or(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            })?;
        if let Some(crop) = crop {
            let dimensions = self.settings.video.dimensions;
            if crop.width == 0
                || crop.height == 0
                || crop
                    .x
                    .checked_add(crop.width)
                    .is_none_or(|right| right > dimensions.width())
                || crop
                    .y
                    .checked_add(crop.height)
                    .is_none_or(|bottom| bottom > dimensions.height())
            {
                return Err(SceneLayerError::InvalidCrop { scene, index });
            }
            if layer.mask.is_some_and(|mask| {
                mask.width == 0
                    || mask.height == 0
                    || mask
                        .x
                        .checked_add(mask.width)
                        .is_none_or(|right| right > crop.width)
                    || mask
                        .y
                        .checked_add(mask.height)
                        .is_none_or(|bottom| bottom > crop.height)
            }) {
                return Err(SceneLayerError::CropWouldInvalidateMask { scene, index });
            }
        }
        self.scenes[scene_index].layers[index].crop = crop;
        Ok(())
    }

    /// Sets or clears one layer's rectangular mask.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, and [`SceneLayerError::InvalidMask`]
    /// when the mask is empty or exceeds the cropped layer area.
    pub fn set_scene_layer_mask(
        &mut self,
        scene: SceneId,
        index: usize,
        mask: Option<RectMask>,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let target = &self.scenes[scene_index];
        let length = target.layers.len();
        let layer = target
            .layers
            .get(index)
            .ok_or(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            })?;
        if let Some(mask) = mask {
            let (width, height) = layer.crop.map_or_else(
                || {
                    (
                        self.settings.video.dimensions.width(),
                        self.settings.video.dimensions.height(),
                    )
                },
                |crop| (crop.width, crop.height),
            );
            if mask.width == 0
                || mask.height == 0
                || mask
                    .x
                    .checked_add(mask.width)
                    .is_none_or(|right| right > width)
                || mask
                    .y
                    .checked_add(mask.height)
                    .is_none_or(|bottom| bottom > height)
            {
                return Err(SceneLayerError::InvalidMask { scene, index });
            }
        }
        self.scenes[scene_index].layers[index].mask = mask;
        Ok(())
    }

    /// Sets or clears one layer's chroma, luma, or key/fill key (IN-020,
    /// SW-008).
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::InvalidKey`]
    /// when the key breaks its bounds, [`SceneLayerError::MissingInput`]
    /// when a key/fill key input does not exist, and
    /// [`SceneLayerError::SceneKeyInput`] when it is a Scene input or an
    /// alias whose chain ends at one; the layer is unchanged on error.
    pub fn set_scene_layer_key(
        &mut self,
        scene: SceneId,
        index: usize,
        key: Option<crate::LayerKey>,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self.scene_layer_position(scene, index)?;
        if let Some(key) = key {
            key.validate()
                .map_err(|reason| SceneLayerError::InvalidKey {
                    scene,
                    index,
                    reason,
                })?;
            if let Some(input) = key.key_input() {
                if !self.inputs.iter().any(|candidate| candidate.id == input) {
                    return Err(SceneLayerError::MissingInput(input));
                }
                if self.input_renders_scene(input) {
                    return Err(SceneLayerError::SceneKeyInput(input));
                }
            }
        }
        self.scenes[scene_index].layers[index].key = key;
        Ok(())
    }

    /// Whether `input` renders a scene: it is a Scene input, or an alias
    /// whose target chain ends at one. A chain that loops or names a
    /// missing input renders no scene.
    #[must_use]
    pub fn input_renders_scene(&self, input: InputId) -> bool {
        let mut current = input;
        for _ in 0..=self.inputs.len() {
            match self
                .inputs
                .iter()
                .find(|candidate| candidate.id == current)
                .map(|candidate| &candidate.kind)
            {
                Some(InputKind::Scene { .. }) => return true,
                Some(InputKind::Alias { target, .. }) => current = *target,
                _ => return false,
            }
        }
        false
    }

    fn scene_layer_position(&self, scene: SceneId, index: usize) -> Result<usize, SceneLayerError> {
        let scene_index = self
            .scenes
            .iter()
            .position(|candidate| candidate.id == scene)
            .ok_or(SceneLayerError::UnknownScene(scene))?;
        let length = self.scenes[scene_index].layers.len();
        if index >= length {
            return Err(SceneLayerError::LayerIndexOutOfRange {
                scene,
                index,
                length,
            });
        }
        Ok(scene_index)
    }

    fn check_layer_effects(
        scene: SceneId,
        index: usize,
        effects: &[LayerEffect],
    ) -> Result<(), SceneLayerError> {
        if effects.len() > LayerEffect::MAX_EFFECTS_PER_LAYER {
            return Err(SceneLayerError::TooManyEffects { scene, index });
        }
        for (effect_index, effect) in effects.iter().enumerate() {
            effect
                .validate()
                .map_err(|reason| SceneLayerError::InvalidEffect {
                    scene,
                    index,
                    effect_index,
                    reason,
                })?;
        }
        Ok(())
    }

    /// Replaces one layer's ordered effect list.
    ///
    /// The new list is checked as a whole before the layer changes, so a
    /// rejection leaves every effect exactly as it was.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::TooManyEffects`]
    /// when the list exceeds [`LayerEffect::MAX_EFFECTS_PER_LAYER`], and
    /// [`SceneLayerError::InvalidEffect`] when one descriptor breaks the
    /// [`LayerEffect`] table.
    pub fn set_scene_layer_effects(
        &mut self,
        scene: SceneId,
        index: usize,
        effects: Vec<LayerEffect>,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self.scene_layer_position(scene, index)?;
        Self::check_layer_effects(scene, index, &effects)?;
        self.scenes[scene_index].layers[index].effects = effects;
        Ok(())
    }

    /// Appends one effect to a layer, preserving the existing order.
    ///
    /// The layer is checked before it changes, so a rejection leaves every
    /// effect exactly as it was.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, [`SceneLayerError::TooManyEffects`]
    /// when the layer already carries [`LayerEffect::MAX_EFFECTS_PER_LAYER`]
    /// effects, and [`SceneLayerError::InvalidEffect`] when the descriptor
    /// breaks the [`LayerEffect`] table.
    pub fn add_scene_layer_effect(
        &mut self,
        scene: SceneId,
        index: usize,
        effect: LayerEffect,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self.scene_layer_position(scene, index)?;
        let length = self.scenes[scene_index].layers[index].effects.len();
        if length == LayerEffect::MAX_EFFECTS_PER_LAYER {
            return Err(SceneLayerError::TooManyEffects { scene, index });
        }
        effect
            .validate()
            .map_err(|reason| SceneLayerError::InvalidEffect {
                scene,
                index,
                effect_index: length,
                reason,
            })?;
        self.scenes[scene_index].layers[index].effects.push(effect);
        Ok(())
    }

    /// Removes one effect from a layer, shifting later effects down.
    ///
    /// # Errors
    ///
    /// Returns [`SceneLayerError::UnknownScene`] when the scene does not
    /// exist, [`SceneLayerError::LayerIndexOutOfRange`] when the index is
    /// beyond the scene's layer count, and
    /// [`SceneLayerError::EffectIndexOutOfRange`] when the effect index is
    /// beyond the layer's effect count.
    pub fn remove_scene_layer_effect(
        &mut self,
        scene: SceneId,
        index: usize,
        effect_index: usize,
    ) -> Result<(), SceneLayerError> {
        let scene_index = self.scene_layer_position(scene, index)?;
        let effects = &mut self.scenes[scene_index].layers[index].effects;
        if effect_index >= effects.len() {
            return Err(SceneLayerError::EffectIndexOutOfRange {
                scene,
                index,
                effect_index,
                length: effects.len(),
            });
        }
        effects.remove(effect_index);
        Ok(())
    }

    pub fn add_audio_bus(&mut self, bus: AudioBus) {
        self.audio_buses.push(bus);
    }

    /// Adds an audio bus after checking id and name invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AddAudioBusError::DuplicateId`] when the id already exists,
    /// [`AddAudioBusError::EmptyName`] when the name is blank, and
    /// [`AddAudioBusError::DuplicateName`] when another bus already uses the
    /// name.
    pub fn add_audio_bus_checked(&mut self, bus: AudioBus) -> Result<(), AddAudioBusError> {
        if self
            .audio_buses
            .iter()
            .any(|candidate| candidate.id == bus.id)
        {
            return Err(AddAudioBusError::DuplicateId(bus.id));
        }
        if bus.name.trim().is_empty() {
            return Err(AddAudioBusError::EmptyName);
        }
        if self
            .audio_buses
            .iter()
            .any(|candidate| candidate.name.eq_ignore_ascii_case(&bus.name))
        {
            return Err(AddAudioBusError::DuplicateName);
        }
        self.audio_buses.push(bus);
        Ok(())
    }

    /// Renames one audio bus while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`RenameAudioBusError::UnknownBus`] when the bus does not
    /// exist, [`RenameAudioBusError::EmptyName`] when the name is blank, and
    /// [`RenameAudioBusError::DuplicateName`] when another bus already uses
    /// the name.
    pub fn rename_audio_bus(
        &mut self,
        bus: BusId,
        name: String,
    ) -> Result<(), RenameAudioBusError> {
        let index = self
            .audio_buses
            .iter()
            .position(|candidate| candidate.id == bus)
            .ok_or(RenameAudioBusError::UnknownBus(bus))?;
        if name.trim().is_empty() {
            return Err(RenameAudioBusError::EmptyName);
        }
        if self
            .audio_buses
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| {
                candidate_index != index && candidate.name.eq_ignore_ascii_case(&name)
            })
        {
            return Err(RenameAudioBusError::DuplicateName);
        }
        self.audio_buses[index].name = name;
        Ok(())
    }

    /// Adds a send from one audio bus to another, rolling back on failure.
    ///
    /// # Errors
    ///
    /// Returns [`AudioBusSendError::UnknownSource`] or
    /// [`AudioBusSendError::UnknownDestination`] when either bus does not
    /// exist, [`AudioBusSendError::SelfSend`] when both ids match,
    /// [`AudioBusSendError::DuplicateSend`] when the send already exists,
    /// [`AudioBusSendError::InvalidProject`] when the project is invalid
    /// before or after the change, and [`AudioBusSendError::Cycle`] when the
    /// send would create a cycle.
    pub fn add_audio_bus_send(
        &mut self,
        source: BusId,
        destination: BusId,
    ) -> Result<(), AudioBusSendError> {
        let source_index = self
            .audio_buses
            .iter()
            .position(|bus| bus.id == source)
            .ok_or(AudioBusSendError::UnknownSource(source))?;
        if !self.audio_buses.iter().any(|bus| bus.id == destination) {
            return Err(AudioBusSendError::UnknownDestination(destination));
        }
        if source == destination {
            return Err(AudioBusSendError::SelfSend(source));
        }
        if self.audio_buses[source_index]
            .sends
            .iter()
            .any(|send| send.destination == destination)
        {
            return Err(AudioBusSendError::DuplicateSend);
        }
        if let Err(errors) = self.validate() {
            return Err(AudioBusSendError::InvalidProject(errors));
        }

        self.audio_buses[source_index]
            .sends
            .push(BusSend { destination });
        if let Err(errors) = self.validate() {
            self.audio_buses[source_index].sends.pop();
            if errors
                .iter()
                .any(|error| error.kind == ValidationErrorKind::Cycle)
            {
                return Err(AudioBusSendError::Cycle);
            }
            return Err(AudioBusSendError::InvalidProject(errors));
        }
        Ok(())
    }

    /// Removes one existing send between two audio buses.
    ///
    /// # Errors
    ///
    /// Returns [`AudioBusSendError::UnknownSource`] or
    /// [`AudioBusSendError::UnknownDestination`] when either bus does not
    /// exist, [`AudioBusSendError::SelfSend`] when both ids match, and
    /// [`AudioBusSendError::MissingSend`] when the send does not exist.
    pub fn remove_audio_bus_send(
        &mut self,
        source: BusId,
        destination: BusId,
    ) -> Result<(), AudioBusSendError> {
        let source_index = self
            .audio_buses
            .iter()
            .position(|bus| bus.id == source)
            .ok_or(AudioBusSendError::UnknownSource(source))?;
        if !self.audio_buses.iter().any(|bus| bus.id == destination) {
            return Err(AudioBusSendError::UnknownDestination(destination));
        }
        if source == destination {
            return Err(AudioBusSendError::SelfSend(source));
        }
        let sends = &mut self.audio_buses[source_index].sends;
        let send_index = sends
            .iter()
            .position(|send| send.destination == destination)
            .ok_or(AudioBusSendError::MissingSend)?;
        sends.remove(send_index);
        Ok(())
    }

    pub fn add_output(&mut self, output: Output) {
        self.outputs.push(output);
    }

    /// Adds an output after checking id, name, and routing invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AddOutputError::DuplicateId`] when the id already exists,
    /// [`AddOutputError::EmptyName`] or [`AddOutputError::DuplicateName`]
    /// for name violations, and [`AddOutputError::UnknownScene`] or
    /// [`AddOutputError::UnknownBus`] when the routed scene or audio bus
    /// does not exist.
    pub fn add_output_checked(&mut self, output: Output) -> Result<(), AddOutputError> {
        if self
            .outputs
            .iter()
            .any(|candidate| candidate.id == output.id)
        {
            return Err(AddOutputError::DuplicateId(output.id));
        }
        if output.name.trim().is_empty() {
            return Err(AddOutputError::EmptyName);
        }
        if self
            .outputs
            .iter()
            .any(|candidate| candidate.name.eq_ignore_ascii_case(&output.name))
        {
            return Err(AddOutputError::DuplicateName);
        }
        if !self
            .scenes
            .iter()
            .any(|candidate| candidate.id == output.video_source)
        {
            return Err(AddOutputError::UnknownScene(output.video_source));
        }
        if !self
            .audio_buses
            .iter()
            .any(|candidate| candidate.id == output.audio_source)
        {
            return Err(AddOutputError::UnknownBus(output.audio_source));
        }
        self.outputs.push(output);
        Ok(())
    }

    /// Retargets one output's video scene and audio bus.
    ///
    /// # Errors
    ///
    /// Returns [`SetOutputRouteError::UnknownOutput`] when the output does
    /// not exist and [`SetOutputRouteError::UnknownScene`] or
    /// [`SetOutputRouteError::UnknownBus`] when the routed scene or audio
    /// bus does not exist.
    pub fn set_output_route(
        &mut self,
        output: OutputId,
        scene: SceneId,
        bus: BusId,
    ) -> Result<(), SetOutputRouteError> {
        let output_index = self
            .outputs
            .iter()
            .position(|candidate| candidate.id == output)
            .ok_or(SetOutputRouteError::UnknownOutput(output))?;
        if !self.scenes.iter().any(|candidate| candidate.id == scene) {
            return Err(SetOutputRouteError::UnknownScene(scene));
        }
        if !self.audio_buses.iter().any(|candidate| candidate.id == bus) {
            return Err(SetOutputRouteError::UnknownBus(bus));
        }
        self.outputs[output_index].video_source = scene;
        self.outputs[output_index].audio_source = bus;
        Ok(())
    }

    /// Sets one output's startup policy.
    ///
    /// # Errors
    ///
    /// Returns [`SetOutputStartupError::UnknownOutput`] when the output does
    /// not exist.
    pub fn set_output_startup(
        &mut self,
        output: OutputId,
        startup: StartupPolicy,
    ) -> Result<(), SetOutputStartupError> {
        let output = self
            .outputs
            .iter_mut()
            .find(|candidate| candidate.id == output)
            .ok_or(SetOutputStartupError::UnknownOutput(output))?;
        output.startup = startup;
        Ok(())
    }

    /// Renames one output while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns [`RenameOutputError::UnknownOutput`] when the output does not
    /// exist, [`RenameOutputError::EmptyName`] when the name is blank, and
    /// [`RenameOutputError::DuplicateName`] when another output already uses
    /// the name.
    pub fn rename_output(
        &mut self,
        output: OutputId,
        name: String,
    ) -> Result<(), RenameOutputError> {
        let index = self
            .outputs
            .iter()
            .position(|candidate| candidate.id == output)
            .ok_or(RenameOutputError::UnknownOutput(output))?;
        if name.trim().is_empty() {
            return Err(RenameOutputError::EmptyName);
        }
        if self
            .outputs
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| {
                candidate_index != index && candidate.name.eq_ignore_ascii_case(&name)
            })
        {
            return Err(RenameOutputError::DuplicateName);
        }
        self.outputs[index].name = name;
        Ok(())
    }

    /// Removes an output that no stream destination, display output, or
    /// `LiveLAN` stream references.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveOutputError::UnknownOutput`] when the output does not
    /// exist and [`RemoveOutputError::StreamTargetReference`] when a stream
    /// destination still uses it.
    pub fn remove_output(&mut self, output: OutputId) -> Result<(), RemoveOutputError> {
        if !self.outputs.iter().any(|candidate| candidate.id == output) {
            return Err(RemoveOutputError::UnknownOutput(output));
        }
        if let Some(target) = self
            .stream_targets
            .iter()
            .find(|target| target.output() == output)
        {
            return Err(RemoveOutputError::StreamTargetReference {
                output,
                stream_target: target.id(),
            });
        }
        if let Some((slot, _)) = self.display_outputs.iter().find(|(_, config)| {
            config.source == fm_types::display::DisplayOutputSource::Output(output)
        }) {
            return Err(RemoveOutputError::DisplayOutputReference {
                output,
                slot: *slot,
            });
        }
        if self.live_lan.source == fm_types::live_lan::LiveLanSource::Output(output) {
            return Err(RemoveOutputError::LiveLanReference { output });
        }
        self.outputs.retain(|candidate| candidate.id != output);
        self.fade_to_black_excluded_outputs.remove(&output);
        self.fade_to_black_output_durations.remove(&output);
        self.output_aux_buses.remove(&output);
        Ok(())
    }

    /// Appends a destination without checking it against the rest of the
    /// project, mirroring [`Project::add_output`]. The decoder uses this and
    /// then validates the whole manifest.
    pub fn add_stream_target(&mut self, target: StreamTarget) {
        self.stream_targets.push(target);
    }

    /// Appends a destination after checking project-wide invariants.
    ///
    /// The destination's own fields were already validated when it was
    /// constructed; what is checked here is what only the project knows: a
    /// unique id, a name no other destination uses, an output that exists,
    /// and room under [`crate::MAX_STREAM_TARGETS`].
    ///
    /// # Errors
    ///
    /// Returns [`AddStreamTargetError`].
    pub fn add_stream_target_checked(
        &mut self,
        target: StreamTarget,
    ) -> Result<(), AddStreamTargetError> {
        if self.stream_targets.len() >= crate::MAX_STREAM_TARGETS {
            return Err(AddStreamTargetError::TooMany);
        }
        if self
            .stream_targets
            .iter()
            .any(|candidate| candidate.id() == target.id())
        {
            return Err(AddStreamTargetError::DuplicateId(target.id()));
        }
        if self
            .stream_targets
            .iter()
            .any(|candidate| candidate.name().eq_ignore_ascii_case(target.name()))
        {
            return Err(AddStreamTargetError::DuplicateName);
        }
        if !self
            .outputs
            .iter()
            .any(|candidate| candidate.id == target.output())
        {
            return Err(AddStreamTargetError::UnknownOutput(target.output()));
        }
        self.stream_targets.push(target);
        Ok(())
    }

    /// Replaces an existing destination's configuration, keeping its id.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateStreamTargetError`] when the id does not exist, another
    /// destination already uses the new name, or the referenced output does
    /// not exist.
    pub fn replace_stream_target(
        &mut self,
        target: StreamTarget,
    ) -> Result<(), UpdateStreamTargetError> {
        let index = self
            .stream_targets
            .iter()
            .position(|candidate| candidate.id() == target.id())
            .ok_or(UpdateStreamTargetError::UnknownStreamTarget(target.id()))?;
        if self
            .stream_targets
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| {
                candidate_index != index && candidate.name().eq_ignore_ascii_case(target.name())
            })
        {
            return Err(UpdateStreamTargetError::DuplicateName);
        }
        if !self
            .outputs
            .iter()
            .any(|candidate| candidate.id == target.output())
        {
            return Err(UpdateStreamTargetError::UnknownOutput(target.output()));
        }
        self.stream_targets[index] = target;
        Ok(())
    }

    /// Moves an existing destination to `index` in the authored order (an
    /// undone removal returns to its old place), clamped to the last place.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateStreamTargetError::UnknownStreamTarget`] when the id
    /// does not exist.
    pub fn move_stream_target(
        &mut self,
        target: StreamTargetId,
        index: usize,
    ) -> Result<(), UpdateStreamTargetError> {
        let from = self
            .stream_targets
            .iter()
            .position(|candidate| candidate.id() == target)
            .ok_or(UpdateStreamTargetError::UnknownStreamTarget(target))?;
        let moved = self.stream_targets.remove(from);
        let index = index.min(self.stream_targets.len());
        self.stream_targets.insert(index, moved);
        Ok(())
    }

    /// The first `(list, cue)` whose actions start or stop stream target
    /// `target` (RC-017).
    #[must_use]
    pub fn stream_target_cue(&self, target: StreamTargetId) -> Option<(u64, u64)> {
        self.cue_lists.iter().find_map(|list| {
            list.cues
                .iter()
                .find(|cue| {
                    cue.actions.iter().any(|action| {
                        matches!(action, crate::CueAction::Stream { target: named, .. } if *named == target)
                    })
                })
                .map(|cue| (list.id, cue.id))
        })
    }

    /// Removes a destination.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveStreamTargetError::UnknownStreamTarget`] when the id
    /// does not exist and [`RemoveStreamTargetError::CueReference`] when a
    /// cue starts or stops it.
    pub fn remove_stream_target(
        &mut self,
        target: StreamTargetId,
    ) -> Result<(), RemoveStreamTargetError> {
        if !self
            .stream_targets
            .iter()
            .any(|candidate| candidate.id() == target)
        {
            return Err(RemoveStreamTargetError::UnknownStreamTarget(target));
        }
        if let Some((list, cue)) = self.stream_target_cue(target) {
            return Err(RemoveStreamTargetError::CueReference { target, list, cue });
        }
        self.stream_targets
            .retain(|candidate| candidate.id() != target);
        Ok(())
    }

    /// Removes an audio bus that no send or output references.
    ///
    /// # Errors
    ///
    /// Returns [`RemoveAudioBusError::UnknownBus`] when the bus does not
    /// exist, [`RemoveAudioBusError::OutputReference`] when an output uses
    /// it, and [`RemoveAudioBusError::OutgoingSend`] or
    /// [`RemoveAudioBusError::IncomingSend`] when a send to or from the bus
    /// still exists.
    pub fn remove_audio_bus(&mut self, bus: BusId) -> Result<(), RemoveAudioBusError> {
        if !self.audio_buses.iter().any(|candidate| candidate.id == bus) {
            return Err(RemoveAudioBusError::UnknownBus(bus));
        }
        if let Some(output) = self
            .outputs
            .iter()
            .find(|output| output.audio_source == bus)
        {
            return Err(RemoveAudioBusError::OutputReference {
                bus,
                output: output.id,
            });
        }
        if let Some(send) = self
            .audio_buses
            .iter()
            .find(|candidate| candidate.id == bus)
            .and_then(|candidate| candidate.sends.first())
        {
            return Err(RemoveAudioBusError::OutgoingSend {
                bus,
                destination: send.destination,
            });
        }
        if let Some(source) = self.audio_buses.iter().find(|candidate| {
            candidate.id != bus && candidate.sends.iter().any(|send| send.destination == bus)
        }) {
            return Err(RemoveAudioBusError::IncomingSend {
                bus,
                source: source.id,
            });
        }
        self.audio_buses.retain(|candidate| candidate.id != bus);
        Ok(())
    }

    pub fn set_main_mix(&mut self, main_mix: MainMix) {
        self.main_mix = Some(main_mix);
    }

    pub fn set_recording_desired(&mut self, slot: RecorderSlot, active: bool) {
        self.recording_desired[slot.index()] = active;
    }

    #[must_use]
    pub const fn with_recording_desired(mut self, slot: RecorderSlot, active: bool) -> Self {
        self.recording_desired[slot.index()] = active;
        self
    }

    pub fn add_stinger(&mut self, stinger: StingerConfig) {
        self.stingers.push(stinger);
    }

    /// Inserts or replaces one Stinger slot while preserving slot order.
    pub fn set_stinger(&mut self, stinger: StingerConfig) {
        if let Some(configured) = self
            .stingers
            .iter_mut()
            .find(|configured| configured.slot == stinger.slot)
        {
            *configured = stinger;
        } else {
            self.stingers.push(stinger);
        }
    }

    /// Inserts or replaces one Stinger slot after checking its media input.
    ///
    /// # Errors
    ///
    /// Returns [`SetStingerError::UnknownInput`] when the stinger's media
    /// input does not exist.
    pub fn set_stinger_checked(&mut self, stinger: StingerConfig) -> Result<(), SetStingerError> {
        if !self
            .inputs
            .iter()
            .any(|input| input.id == stinger.media_input)
        {
            return Err(SetStingerError::UnknownInput(stinger.media_input));
        }
        self.set_stinger(stinger);
        Ok(())
    }

    /// Removes one configured Stinger slot.
    pub fn remove_stinger(&mut self, slot: StingerSlotNumber) -> Option<StingerConfig> {
        let index = self
            .stingers
            .iter()
            .position(|configured| configured.slot == slot)?;
        Some(self.stingers.remove(index))
    }

    #[must_use]
    pub fn with_main_mix(mut self, main_mix: MainMix) -> Self {
        self.set_main_mix(main_mix);
        self
    }

    pub fn set_restart_policy(&mut self, restart_policy: RestartPolicy) {
        self.restart_policy = restart_policy;
    }

    #[must_use]
    pub const fn with_restart_policy(mut self, restart_policy: RestartPolicy) -> Self {
        self.restart_policy = restart_policy;
        self
    }

    /// Validates names, references, capability keys, and graph cycles.
    ///
    /// # Errors
    ///
    /// Returns every discovered [`ValidationError`] so a caller can present one
    /// complete preflight report.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let errors = validate_project(self);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Sets or clears one scene input's audio source, rejecting cycles.
    ///
    /// # Errors
    ///
    /// Returns [`SceneInputAudioSourceError::MissingTargetInput`] when the
    /// target input does not exist,
    /// [`SceneInputAudioSourceError::NonSceneTargetInput`] when it is not a
    /// scene input, [`SceneInputAudioSourceError::MissingSourceInput`] when
    /// the requested source input does not exist, and
    /// [`SceneInputAudioSourceError::SourceCycle`] when the assignment would
    /// create a self-reference or cycle.
    ///
    /// # Panics
    ///
    /// Panics if the validated target input is missing from the candidate
    /// clone; unreachable because the existence check above already rejected
    /// unknown ids.
    pub fn set_scene_input_audio_source(
        &mut self,
        scene_input: InputId,
        audio_source: Option<InputId>,
    ) -> Result<(), SceneInputAudioSourceError> {
        let target = self
            .inputs
            .iter()
            .find(|input| input.id == scene_input)
            .ok_or(SceneInputAudioSourceError::MissingTargetInput(scene_input))?;
        if !matches!(target.kind, InputKind::Scene { .. }) {
            return Err(SceneInputAudioSourceError::NonSceneTargetInput(scene_input));
        }
        if let Some(source) = audio_source
            && !self.inputs.iter().any(|input| input.id == source)
        {
            return Err(SceneInputAudioSourceError::MissingSourceInput(source));
        }
        let mut candidate = self.clone();
        let target = candidate
            .inputs
            .iter_mut()
            .find(|input| input.id == scene_input)
            .expect("validated target input exists in candidate");
        let InputKind::Scene {
            audio_source: current,
            ..
        } = &mut target.kind
        else {
            unreachable!("validated target input is a scene input");
        };
        *current = audio_source;
        if candidate.validate().is_err_and(|errors| {
            errors.iter().any(|error| {
                error.entity == Some(EntityRef::Input(scene_input))
                    && error.field == "kind.scene.audio_source"
                    && matches!(
                        error.kind,
                        ValidationErrorKind::SelfReference | ValidationErrorKind::Cycle
                    )
            })
        }) {
            return Err(SceneInputAudioSourceError::SourceCycle);
        }
        *self = candidate;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSettings {
    pub frame_rate: FrameRate,
    pub video: VideoFormat,
    pub audio: AudioFormat,
}

impl ProjectSettings {
    pub const MIN_FRAME_RATE_FPS: u32 = 1;
    pub const MAX_FRAME_RATE_FPS: u32 = 240;
    /// Widest program canvas. Width and height share one bound, so portrait,
    /// square, and landscape canvases are equally valid; a chroma-subsampled
    /// pixel format additionally requires even subsampled dimensions.
    pub const MAX_VIDEO_WIDTH: u32 = 8_192;
    /// Tallest program canvas; see [`Self::MAX_VIDEO_WIDTH`].
    pub const MAX_VIDEO_HEIGHT: u32 = 8_192;
    pub const MIN_AUDIO_SAMPLE_RATE_HZ: u32 = 8_000;
    pub const MAX_AUDIO_SAMPLE_RATE_HZ: u32 = 384_000;
    pub const MAX_AUDIO_CHANNELS: usize = 8;

    #[must_use]
    pub fn new(frame_rate: FrameRate, output: OutputFormat) -> Self {
        Self {
            frame_rate,
            video: output.video,
            audio: output.audio,
        }
    }

    #[must_use]
    pub fn output_format(&self) -> OutputFormat {
        OutputFormat {
            video: self.video,
            audio: self.audio.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputFormat {
    pub video: VideoFormat,
    pub audio: AudioFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    pub id: InputId,
    pub name: String,
    pub kind: InputKind,
    pub required_capabilities: Vec<String>,
}

/// Exact persisted gain for an input strip, in one-thousandth of a decibel.
///
/// The integer representation is stable across JSON round trips and excludes
/// non-finite floating-point values by construction.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InputGainMilliDb(i32);

impl InputGainMilliDb {
    pub const MIN: i32 = -96_000;
    pub const MAX: i32 = 24_000;
    const _MATCHES_CUE_GAIN: () = assert!(
        Self::MIN == fm_types::CUE_GAIN_MIN_MILLIDB && Self::MAX == fm_types::CUE_GAIN_MAX_MILLIDB
    );
    pub const UNITY: Self = Self(0);

    #[must_use]
    pub const fn new(value: i32) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Exact persisted stereo balance in one-hundredth of a percent.
///
/// `-10000` is full left, zero is centered, and `10000` is full right.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct InputBalanceBasisPoints(i32);

impl InputBalanceBasisPoints {
    pub const MIN: i32 = -10_000;
    pub const MAX: i32 = 10_000;
    pub const CENTER: Self = Self(0);

    #[must_use]
    pub const fn new(value: i32) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Exact persisted mono pan position in one-hundredth of a percent (schema
/// 39).
///
/// `-10000` is hard left, zero is centered, and `10000` is hard right. Pan
/// positions a mono source with the -3 dB constant-power law (see
/// `fm_audio::Pan`); balance trims a stereo signal.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct InputPanBasisPoints(i32);

impl InputPanBasisPoints {
    pub const MIN: i32 = -10_000;
    pub const MAX: i32 = 10_000;
    pub const CENTER: Self = Self(0);

    #[must_use]
    pub const fn new(value: i32) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Exact nonnegative per-input delay in 48 kHz samples.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputDelaySamples(u32);

impl InputDelaySamples {
    pub const MAX: u32 = 48_000;
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Persisted user controls for one input's Master mixer strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputAudioStripState {
    pub gain: InputGainMilliDb,
    pub balance: InputBalanceBasisPoints,
    pub pan: InputPanBasisPoints,
    pub delay_samples: InputDelaySamples,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
    /// Master and the aux buses the strip feeds, with each send's level and
    /// tap point (AU-002, schema 92).
    pub sends: crate::AudioSends,
}

impl Default for InputAudioStripState {
    fn default() -> Self {
        Self {
            gain: InputGainMilliDb::UNITY,
            balance: InputBalanceBasisPoints::CENTER,
            pan: InputPanBasisPoints::CENTER,
            delay_samples: InputDelaySamples::ZERO,
            muted: false,
            soloed: false,
            follow_video: true,
            sends: crate::AudioSends::MASTER,
        }
    }
}

/// Input identity paired with its persisted strip state, label, and group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputAudioStrip {
    pub input: InputId,
    pub state: InputAudioStripState,
    /// Operator label (schema 39); empty shows the input name.
    pub label: String,
    /// The strip group the strip belongs to (schema 39).
    pub group: Option<crate::AudioGroupNumber>,
    /// The strip's DSP chain (schema 65, AU-005).
    pub dsp: fm_types::strip_dsp::StripDsp,
    /// The strip's audio plugin chain (schema 91, AU-006).
    pub plugins: fm_types::audio_plugins::PluginChainState,
}

/// Why a mixer label, group, or group name edit is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetAudioMixerError {
    UnknownInput(InputId),
    Label(crate::AudioLabelError),
    /// The parent would nest this group inside itself.
    GroupCycle(crate::AudioGroupNumber),
    /// The monitor output device name is unusable.
    MonitorDevice(crate::AudioMonitorDeviceError),
    /// A strip DSP chain value is out of range (AU-005).
    Dsp(fm_types::strip_dsp::StripDspError),
    /// An audio plugin chain value is out of range (AU-006).
    Plugins(fm_types::audio_plugins::PluginChainStateError),
}

impl std::fmt::Display for SetAudioMixerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::Label(error) => error.fmt(formatter),
            Self::GroupCycle(group) => {
                write!(formatter, "{group} cannot be nested inside itself")
            }
            Self::MonitorDevice(error) => error.fmt(formatter),
            Self::Dsp(error) => error.fmt(formatter),
            Self::Plugins(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SetAudioMixerError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputKind {
    Color,
    /// A media file (or still image) and how it plays (schema 36, IN-003).
    Media {
        asset_uri: String,
        playback: crate::MediaPlaybackSettings,
    },
    /// A local camera or capture card (schema 44, IN-001): the exact
    /// adapter-qualified stable key from platform discovery, and the
    /// requested format, audio, and loss slate.
    Device {
        stable_key: String,
        capture: fm_types::capture::CaptureSettings,
    },
    /// A network stream received by URL (schema 43, IN-015): RTSP, RTP,
    /// UDP, TCP, or HTTP(S) MPEG-TS, or an HLS playlist, with its jitter
    /// buffer, reconnect policy, timeout, and stall slate.
    NetworkStream(fm_types::network_stream::NetworkStreamConfig),
    /// A local system audio capture source (schema 52, AU-011): a
    /// `PulseAudio`/`PipeWire` source such as a sink monitor, or an ALSA capture
    /// device, named by the operator. Audio-only: it has no picture.
    SystemAudio(fm_types::system_audio::SystemAudioConfig),
    /// A still picture from the project's assets placed by a fit policy
    /// (schema 63, IN-008). Silent.
    Image(fm_types::still::ImageInputConfig),
    /// A slideshow of the pictures in an asset folder or an explicit list
    /// (schema 63, IN-009): a paged input. Silent.
    Slideshow(fm_types::still::SlideshowConfig),
    /// A document shown one page at a time (IN-014): a PDF, or a
    /// presentation converted to PDF, and the page on screen. A paged
    /// input: it renders its own picture and is silent.
    Document(DocumentInput),
    /// A local screen or application window captured on the server
    /// (IN-011): the target (a screen, or a window rule), crop, cursor,
    /// rate cap, and the slate shown while the target is gone.
    ScreenCapture(fm_types::screen_capture::ScreenCaptureConfig),
    Scene {
        scene_id: SceneId,
        audio_source: Option<InputId>,
    },
    Simulated(SimulatedInput),
    /// A virtual input: it renders and hears exactly what `target` renders
    /// and hears, under its own id, name, list position, and audio strip.
    ///
    /// `crop` is the alias's own crop in project pixels of the concrete
    /// source. It is independent: neither inherited from an alias the target
    /// chain passes through nor applied to the target itself.
    Alias {
        target: InputId,
        crop: Option<CropRect>,
    },
    /// The Program or Preview bus fed back as a source, delayed by exactly one
    /// frame: on tick `n` it shows the bus frame committed at the end of tick
    /// `n - 1`, and black before any frame has been committed. The delay is
    /// what lets this input appear on its own bus without a dependency cycle.
    MixFeed(MixFeed),
    /// A title scene instantiated from a stored template. It is a leaf: it
    /// renders its own picture, is silent, and references no other input.
    Title(TitleInput),
    /// A replay player (OR-012, schema 82): a leaf whose picture and sound
    /// are the recorded replay event it plays, at the angle and speed the
    /// operator chose. What it plays is runtime state, never persisted.
    Replay,
}

impl InputKind {
    /// Every project asset file the input shows directly: a media file, an
    /// image, or the pictures of an explicit slideshow list, in order. A
    /// slideshow folder is not a file and is not listed.
    #[must_use]
    pub fn file_asset_uris(&self) -> Vec<&str> {
        match self {
            Self::Media { asset_uri, .. } => vec![asset_uri.as_str()],
            Self::Document(document) => vec![document.asset_uri.as_str()],
            Self::Image(config) => vec![config.asset_uri.as_str()],
            Self::Slideshow(config) => match &config.source {
                fm_types::still::SlideshowSource::List(items) => {
                    items.iter().map(|item| item.asset_uri.as_str()).collect()
                }
                fm_types::still::SlideshowSource::Folder { .. } => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Rewrites every URI [`Self::file_asset_uris`] lists for which `map`
    /// returns a new URI; returns whether anything changed.
    pub fn map_file_asset_uris(&mut self, mut map: impl FnMut(&str) -> Option<String>) -> bool {
        let mut changed = false;
        let mut apply = |uri: &mut String| {
            if let Some(mapped) = map(uri)
                && mapped != *uri
            {
                *uri = mapped;
                changed = true;
            }
        };
        match self {
            Self::Media { asset_uri, .. } => apply(asset_uri),
            Self::Document(document) => apply(&mut document.asset_uri),
            Self::Image(config) => apply(&mut config.asset_uri),
            Self::Slideshow(config) => {
                if let fm_types::still::SlideshowSource::List(items) = &mut config.source {
                    for item in items {
                        apply(&mut item.asset_uri);
                    }
                }
            }
            _ => {}
        }
        changed
    }
}

/// A document input's file and current page (IN-014).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DocumentInput {
    /// The document in the project's assets (`asset://<key>`): a `.pdf`,
    /// `.pptx`, `.odp`, or `.key` file.
    pub asset_uri: String,
    /// The zero-based page on screen, below
    /// [`fm_types::paged::MAX_PAGED_PAGES`]. The runtime shows the last
    /// page of a shorter document.
    pub page: u32,
}

impl DocumentInput {
    #[must_use]
    pub fn new(asset_uri: impl Into<String>, page: u32) -> Self {
        Self {
            asset_uri: asset_uri.into(),
            page,
        }
    }
}

/// The switcher bus a [`InputKind::MixFeed`] input re-enters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MixFeed {
    Program,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulatedInput {
    pub video: SimulatedVideo,
    pub audio: SimulatedAudio,
}

impl SimulatedInput {
    #[must_use]
    pub const fn new(video: SimulatedVideo, audio: SimulatedAudio) -> Self {
        Self { video, audio }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulatedVideo {
    Solid(SolidColor),
    Bars,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl SolidColor {
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulatedAudio {
    Silence,
    Sine { frequency_hz: u32 },
}

impl SimulatedAudio {
    pub const MIN_SINE_FREQUENCY_HZ: u32 = 1;
    pub const MAX_SINE_FREQUENCY_HZ: u32 = 20_000;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MainMix {
    pub desired_program: InputId,
    pub desired_preview: InputId,
}

/// One-based operator-facing Stinger slot number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StingerSlotNumber(u8);

impl StingerSlotNumber {
    pub const COUNT: usize = 8;

    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= 8 {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerAudioPolicy {
    Muted,
    StingerOnly,
    MixWithProgram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerMissingMediaFallback {
    Cut,
    Fade,
    KeepProgram,
}

/// Durable configuration for one of the eight Stinger slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StingerConfig {
    pub slot: StingerSlotNumber,
    pub media_input: InputId,
    pub preload: bool,
    pub cut_point_frames: u32,
    pub audio_policy: StingerAudioPolicy,
    pub missing_media_fallback: StingerMissingMediaFallback,
}

impl StingerConfig {
    #[must_use]
    pub const fn new(
        slot: StingerSlotNumber,
        media_input: InputId,
        preload: bool,
        cut_point_frames: u32,
        audio_policy: StingerAudioPolicy,
        missing_media_fallback: StingerMissingMediaFallback,
    ) -> Self {
        Self {
            slot,
            media_input,
            preload,
            cut_point_frames,
            audio_policy,
            missing_media_fallback,
        }
    }
}

impl MainMix {
    #[must_use]
    pub const fn new(desired_program: InputId, desired_preview: InputId) -> Self {
        Self {
            desired_program,
            desired_preview,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure {
        max_attempts: u8,
    },
    Always,
}

impl RestartPolicy {
    pub const MAX_RESTART_ATTEMPTS: u8 = 100;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    pub background: Rgba8,
    pub layers: Vec<Layer>,
    /// The scene's layer animation (GX-011), keyed by [`Layer::id`].
    pub animation: SceneAnimation,
}

impl Scene {
    /// Maximum layers accepted by the compositor for one execution plan.
    ///
    /// Persisted scenes are intentionally not bounded by this value: schema v3
    /// allowed larger scene lists, so storage must preserve them.
    /// Rendering code must enforce this limit when realizing a scene, and
    /// every layer edit ([`Scene::apply_layer_change`], the offline
    /// `Project` layer operations) refuses to enable a layer (or add an
    /// enabled one) past it, so an edited scene always renders.
    pub const MAX_RENDERED_LAYERS: usize = 64;

    /// How many of the scene's layers are enabled (drawn).
    #[must_use]
    pub fn enabled_layer_count(&self) -> usize {
        self.layers.iter().filter(|layer| layer.enabled).count()
    }

    /// Whether one more layer may be enabled: the scene draws fewer than
    /// [`Self::MAX_RENDERED_LAYERS`] enabled layers.
    #[must_use]
    pub fn can_enable_another_layer(&self) -> bool {
        self.enabled_layer_count() < Self::MAX_RENDERED_LAYERS
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layer {
    /// Stable identity within the scene (see [`LayerId`]).
    pub id: LayerId,
    pub name: String,
    pub source: SourceRef,
    pub enabled: bool,
    pub geometry: LayerGeometry,
    pub crop: Option<CropRect>,
    pub mask: Option<RectMask>,
    /// The layer's chroma or luma key (IN-020), applied after the crop
    /// together with the mask and before the effects.
    pub key: Option<crate::LayerKey>,
    pub effects: Vec<LayerEffect>,
    pub opacity: u8,
    pub z_order: i32,
}

/// One ordered video effect persisted on a scene layer (IN-020).
///
/// An effect is a descriptor name plus exact integer parameters, mirroring the
/// compositor's backend-neutral descriptor, and is one of the reference
/// effects every renderer executes ([`LayerEffect::KINDS`]):
///
/// | Name | Parameters | Bounds |
/// |---|---|---|
/// | `levels` | `[black, white]` | each `0..=255`, `black < white` |
/// | `brightness_contrast` | `[brightness, contrast]` | each `-100..=100` |
/// | `saturation` | `[amount]` | `-100..=100` |
/// | `sharpen` | `[amount]` | `0..=1000` (per mille) |
/// | `flip` | `[horizontal, vertical]` | each `0` or `1` |
///
/// A layer carries at most [`LayerEffect::MAX_EFFECTS_PER_LAYER`] effects,
/// applied in list order after its key and mask and before its grade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerEffect {
    pub name: String,
    pub parameters: Vec<i32>,
}

impl LayerEffect {
    /// Maximum effects one layer may carry.
    pub const MAX_EFFECTS_PER_LAYER: usize = 8;
    /// Every effect name a layer may carry, in their stable wire order.
    pub const KINDS: [&'static str; 6] = [
        "levels",
        "brightness_contrast",
        "saturation",
        "sharpen",
        "flip",
        "deinterlace",
    ];

    /// Creates one effect descriptor without checking it.
    ///
    /// The descriptor is checked when the effect enters a layer
    /// ([`Project::set_scene_layer_effects`],
    /// [`Project::add_scene_layer_effect`], a live
    /// [`crate::SceneLayerChange::Effects`]) and by [`Project::validate`].
    #[must_use]
    pub fn new(name: impl Into<String>, parameters: Vec<i32>) -> Self {
        Self {
            name: name.into(),
            parameters,
        }
    }

    /// The neutral effect of kind `name`: levels `0..255`, zero
    /// brightness/contrast, saturation and sharpening, and no flip. `None`
    /// for an unknown name.
    #[must_use]
    pub fn default_of(name: &str) -> Option<Self> {
        let parameters = match name {
            "levels" => vec![0, 255],
            "brightness_contrast" | "flip" => vec![0, 0],
            "saturation" | "sharpen" | "deinterlace" => vec![0],
            _ => return None,
        };
        Some(Self::new(name, parameters))
    }

    /// The inclusive range of every parameter of effect kind `name`, in
    /// order, or `None` for an unknown name.
    #[must_use]
    pub fn parameter_ranges(name: &str) -> Option<&'static [(i32, i32)]> {
        Some(match name {
            "levels" => &[(0, 255), (0, 255)],
            "brightness_contrast" => &[(-100, 100), (-100, 100)],
            "saturation" => &[(-100, 100)],
            "sharpen" => &[(0, 1000)],
            "flip" => &[(0, 1), (0, 1)],
            "deinterlace" => &[(0, 2)],
            _ => return None,
        })
    }

    /// Checks this descriptor against the table in the type documentation.
    ///
    /// The per-layer effect count is enforced when effects enter a layer, not
    /// here.
    ///
    /// # Errors
    ///
    /// [`LayerEffectError::UnknownEffect`] for a name outside
    /// [`Self::KINDS`], [`LayerEffectError::WrongParameterCount`],
    /// [`LayerEffectError::ParameterOutOfRange`], or
    /// [`LayerEffectError::InvalidLevels`] (`black >= white`).
    pub fn validate(&self) -> Result<(), LayerEffectError> {
        let ranges = Self::parameter_ranges(&self.name).ok_or(LayerEffectError::UnknownEffect)?;
        if self.parameters.len() != ranges.len() {
            return Err(LayerEffectError::WrongParameterCount {
                expected: ranges.len(),
            });
        }
        for (parameter, (value, (minimum, maximum))) in
            self.parameters.iter().zip(ranges).enumerate()
        {
            if !(*minimum..=*maximum).contains(value) {
                return Err(LayerEffectError::ParameterOutOfRange { parameter });
            }
        }
        if self.name == "levels" && self.parameters[0] >= self.parameters[1] {
            return Err(LayerEffectError::InvalidLevels);
        }
        Ok(())
    }
}

/// Failure of one [`LayerEffect`] descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerEffectError {
    /// The name is not one of [`LayerEffect::KINDS`].
    UnknownEffect,
    /// The effect kind takes exactly `expected` parameters.
    WrongParameterCount { expected: usize },
    /// Parameter `parameter` (zero-based) is outside its range.
    ParameterOutOfRange { parameter: usize },
    /// A `levels` effect whose black point is not below its white point.
    InvalidLevels,
}

impl std::fmt::Display for LayerEffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEffect => write!(
                formatter,
                "effect must be one of {}",
                LayerEffect::KINDS.join(", ")
            ),
            Self::WrongParameterCount { expected } => {
                write!(formatter, "effect takes exactly {expected} parameters")
            }
            Self::ParameterOutOfRange { parameter } => {
                write!(formatter, "effect parameter {parameter} is out of range")
            }
            Self::InvalidLevels => {
                formatter.write_str("levels black point must be below its white point")
            }
        }
    }
}

impl std::error::Error for LayerEffectError {}

/// Where a scene layer draws.
///
/// The layer is a `width` x `height` box turned clockwise by `rotation`
/// quarter turns, the top-left of that quarter-turned footprint at the
/// translation. Free rotation extends the quarter turns: the box turns a
/// further `tilt_millidegrees` clockwise (in
/// `MIN_TILT_MILLIDEGREES..MAX_TILT_MILLIDEGREES`) about its anchor point,
/// so the layer's whole clockwise turn is
/// [`LayerGeometry::rotation_millidegrees`] and any angle has exactly one
/// stored form ([`LayerGeometry::with_rotation_millidegrees`]).
///
/// The [`LayerAnchor`] is a point of the box in its own (unturned) frame:
/// the pivot of the free rotation and of animated scale and rotation
/// ([`crate::Scene::posed`]). The default anchor is the box centre.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerGeometry {
    pub translation_x: i32,
    pub translation_y: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: Rotation,
    /// A further clockwise turn about the anchor, in millidegrees, in
    /// `MIN_TILT_MILLIDEGREES..MAX_TILT_MILLIDEGREES`. A posed scene
    /// ([`crate::Scene::posed`]) carries the tilt it draws about the
    /// footprint centre instead, its anchor resolved to the centre.
    pub tilt_millidegrees: i32,
    /// The scale and rotation pivot.
    pub anchor: LayerAnchor,
}

impl LayerGeometry {
    /// Smallest stored tilt (inclusive).
    pub const MIN_TILT_MILLIDEGREES: i32 = -45_000;
    /// Largest stored tilt (exclusive): a turn of 45 degrees or more is the
    /// next quarter turn with a negative tilt.
    pub const MAX_TILT_MILLIDEGREES: i32 = 45_000;

    #[must_use]
    pub const fn new(
        translation_x: i32,
        translation_y: i32,
        width: u32,
        height: u32,
        rotation: Rotation,
    ) -> Self {
        Self {
            translation_x,
            translation_y,
            width,
            height,
            rotation,
            tilt_millidegrees: 0,
            anchor: LayerAnchor::CENTER,
        }
    }

    /// The geometry turned a further `millidegrees` clockwise.
    #[must_use]
    pub const fn with_tilt(mut self, millidegrees: i32) -> Self {
        self.tilt_millidegrees = millidegrees;
        self
    }

    /// The geometry with its scale and rotation pivot at `anchor`.
    #[must_use]
    pub const fn with_anchor(mut self, anchor: LayerAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Whether the tilt is in its stored range.
    #[must_use]
    pub const fn tilt_in_range(self) -> bool {
        self.tilt_millidegrees >= Self::MIN_TILT_MILLIDEGREES
            && self.tilt_millidegrees < Self::MAX_TILT_MILLIDEGREES
    }

    /// The whole clockwise turn, quarter turns plus tilt, in
    /// `0..360_000` millidegrees.
    #[must_use]
    pub const fn rotation_millidegrees(self) -> i32 {
        let quarters: i32 = match self.rotation {
            Rotation::Deg0 => 0,
            Rotation::Deg90 => 1,
            Rotation::Deg180 => 2,
            Rotation::Deg270 => 3,
        };
        (quarters * 90_000 + self.tilt_millidegrees).rem_euclid(360_000)
    }

    /// The geometry with its whole clockwise turn set to `millidegrees`
    /// (reduced modulo a full turn): the nearest quarter turn (half up) and
    /// the tilt left over, in the stored range. The translation is kept, so
    /// the caller places the new footprint.
    #[must_use]
    pub const fn with_rotation_millidegrees(mut self, millidegrees: i32) -> Self {
        let angle = millidegrees.rem_euclid(360_000);
        // Half up: 45 degrees belongs to the next quarter turn.
        let quarters = (angle + 45_000) / 90_000;
        self.tilt_millidegrees = angle - quarters * 90_000;
        self.rotation = match quarters % 4 {
            1 => Rotation::Deg90,
            2 => Rotation::Deg180,
            3 => Rotation::Deg270,
            _ => Rotation::Deg0,
        };
        self
    }

    /// Whether the geometry can be stored on a project layer: both sides in
    /// `1..=` the project video limits, the tilt in range, and the anchor
    /// inside the box.
    #[must_use]
    pub const fn is_storable(self) -> bool {
        self.width != 0
            && self.height != 0
            && self.width <= ProjectSettings::MAX_VIDEO_WIDTH
            && self.height <= ProjectSettings::MAX_VIDEO_HEIGHT
            && self.tilt_in_range()
            && self.anchor.is_valid()
    }
}

/// A layer's scale and rotation pivot: a point of its box in the box's own
/// (unturned) frame, in basis points of its width (`x`) and height (`y`) from
/// the top-left corner, each in `0..=`[`LayerAnchor::MAX`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LayerAnchor {
    pub x: u16,
    pub y: u16,
}

impl LayerAnchor {
    /// The far edge, in basis points.
    pub const MAX: u16 = 10_000;
    /// The box centre: the default pivot.
    pub const CENTER: Self = Self::new(5_000, 5_000);

    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }

    /// Whether both coordinates lie within the box.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.x <= Self::MAX && self.y <= Self::MAX
    }
}

impl Default for LayerAnchor {
    fn default() -> Self {
        Self::CENTER
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRect {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// A hard-edged rectangular mask in half-open post-crop source space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RectMask {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub invert: bool,
}

impl RectMask {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            invert: false,
        }
    }

    #[must_use]
    pub const fn inverted(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgba8 {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Rgba8 {
    pub const OPAQUE_BLACK: Self = Self::new(0, 0, 0, u8::MAX);

    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    #[must_use]
    pub const fn is_premultiplied(self) -> bool {
        self.red <= self.alpha && self.green <= self.alpha && self.blue <= self.alpha
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRef {
    Input(InputId),
    Scene(SceneId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioBus {
    pub id: BusId,
    pub name: String,
    pub sends: Vec<BusSend>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BusSend {
    pub destination: BusId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    pub id: OutputId,
    pub name: String,
    pub video_source: SceneId,
    pub audio_source: BusId,
    pub startup: StartupPolicy,
    pub required_capabilities: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StartupPolicy {
    #[default]
    Stopped,
    ReconcileDesiredState,
}

/// Number of independently controllable program recorder slots.
pub const RECORDER_SLOT_COUNT: usize = 2;

/// One independently controllable program recorder, numbered 1..=2.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecorderSlot(u8);

impl RecorderSlot {
    /// Every recorder slot in ascending slot order.
    pub const ALL: [Self; RECORDER_SLOT_COUNT] = [Self(1), Self(2)];

    /// Creates a recorder slot from its one-based number.
    ///
    /// # Errors
    ///
    /// Returns [`RecorderSlotError::OutOfRange`] when the number is not in
    /// `1..=2`.
    pub const fn new(value: u8) -> Result<Self, RecorderSlotError> {
        if value >= 1 && value <= 2 {
            Ok(Self(value))
        } else {
            Err(RecorderSlotError::OutOfRange)
        }
    }

    /// The one-based slot number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Zero-based array index for this slot.
    const fn index(self) -> usize {
        (self.0 - 1) as usize
    }
}

/// Failure to construct a [`RecorderSlot`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderSlotError {
    /// The slot number is outside `1..=2`.
    OutOfRange,
}

impl std::fmt::Display for RecorderSlotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange => write!(
                formatter,
                "recorder slot must be between 1 and {RECORDER_SLOT_COUNT}"
            ),
        }
    }
}

impl std::error::Error for RecorderSlotError {}
