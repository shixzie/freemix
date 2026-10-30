use core::fmt;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use fm_automation::{Playlist, PlaylistConfig};
use fm_model::{GradeEditError, LutId, ProjectGrading};
use fm_switcher::{
    DesiredStream, InputUsage, StingerDescriptor, StingerSlotId, StreamTargetId, SwitcherEvent,
    SwitcherState, TBarState,
};
use fm_titles::{TitleAirTiming, TitleAnimation, TitleFieldSet, TitleTemplate};

use crate::{EngineScene, EngineTitleTemplate};
use fm_types::{
    InputId, InputOrderError, MAX_INPUT_NAME_BYTES, OutputId, RenameInputError, SceneId,
    validate_input_name,
};

use crate::{
    CompiledProjectGrades, EngineLut, GradeCompileError, InputCatalogueError, ShowError,
    grade_edit::checked_grading,
};

/// Largest number of inputs a show may hold once inputs are added live.
///
/// Equals the protocol's list bound, so every snapshot and event list of
/// inputs or audio strips stays encodable.
pub const MAX_SHOW_INPUTS: usize = 256;

/// The chain of every strip without one, so callers can borrow a chain.
static EMPTY_PLUGIN_CHAIN: fm_plugin_host_api::PluginChain = fm_plugin_host_api::PluginChain::new();

/// The source an input is added with while the show is live.
///
/// The generators are exactly the sources the simulated daemon renders: a
/// solid RGBA color and color bars, both with silent audio. A network
/// stream (IN-015) is received by URL, and a capture input (IN-001)
/// acquires a local device by its stable key. The engine carries the
/// source opaquely so an undone add can be redone exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineInputSource {
    Solid {
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    Bars,
    NetworkStream(Box<fm_types::network_stream::NetworkStreamConfig>),
    Capture(Box<fm_types::capture::CaptureInputConfig>),
    /// A system audio capture source (AU-011).
    SystemAudio(Box<fm_types::system_audio::SystemAudioConfig>),
    /// A picture file (IN-008).
    Image(Box<fm_types::still::ImageInputConfig>),
    /// A slideshow (IN-009).
    Slideshow(Box<fm_types::still::SlideshowConfig>),
    /// A screen or window capture (IN-011).
    ScreenCapture(Box<fm_types::screen_capture::ScreenCaptureConfig>),
    /// A replay player (OR-012).
    Replay,
}

impl EngineInputSource {
    /// The replaceable source this is (IN-026).
    #[must_use]
    pub fn spec(&self) -> fm_model::InputSourceSpec {
        let video = match self {
            Self::Solid {
                red,
                green,
                blue,
                alpha,
            } => fm_model::GeneratorVideo::Solid {
                rgba: [*red, *green, *blue, *alpha],
            },
            Self::Bars => fm_model::GeneratorVideo::Bars,
            Self::NetworkStream(config) => {
                return fm_model::InputSourceSpec::Network((**config).clone());
            }
            Self::Capture(config) => {
                return fm_model::InputSourceSpec::Device {
                    stable_key: config.stable_key.clone(),
                };
            }
            Self::SystemAudio(config) => {
                return fm_model::InputSourceSpec::SystemAudio((**config).clone());
            }
            Self::Image(config) => return fm_model::InputSourceSpec::Image((**config).clone()),
            Self::Slideshow(config) => {
                return fm_model::InputSourceSpec::Slideshow((**config).clone());
            }
            Self::ScreenCapture(config) => {
                return fm_model::InputSourceSpec::ScreenCapture((**config).clone());
            }
            Self::Replay => return fm_model::InputSourceSpec::Replay,
        };
        fm_model::InputSourceSpec::Generator {
            video,
            tone_hz: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineInputAudioStripState {
    pub gain_millidb: i32,
    pub balance_basis_points: i32,
    /// Mono source position (AU-001); balance trims a stereo signal.
    pub pan_basis_points: i32,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
    pub delay_samples: u32,
    /// Master and the aux buses the strip feeds (AU-002). A live control.
    pub buses: fm_types::audio_bus::AudioBusMask,
}

impl Default for EngineInputAudioStripState {
    fn default() -> Self {
        Self {
            gain_millidb: 0,
            balance_basis_points: 0,
            pan_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
            buses: fm_types::audio_bus::AudioBusMask::MASTER,
        }
    }
}

/// The live field state of one title input.
///
/// `name` is the title template's display name; `fields` holds the typed,
/// revisioned values that title commands edit; `timing` is the template
/// animation's In and Out phase lengths, which [`crate::EngineCommand::TakeTitle`]
/// and [`crate::EngineCommand::TitleOffAir`] play on air. `template`, when
/// present, is the shared template whose animation
/// [`crate::EngineCommand::TitleAnimation`] edits (see
/// [`crate::EngineTitleTemplate`]). Rendering stays with the media plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineTitle {
    pub name: String,
    pub fields: TitleFieldSet,
    pub timing: TitleAirTiming,
    pub template: Option<EngineTitleTemplate>,
}

impl EngineTitle {
    /// A title with no animation (zero-length In and Out phases).
    #[must_use]
    pub fn new(name: impl Into<String>, fields: TitleFieldSet) -> Self {
        Self {
            name: name.into(),
            fields,
            timing: TitleAirTiming::default(),
            template: None,
        }
    }

    /// The title with its stored template, shared under `key` (the stored
    /// template URI) with every title input naming the same key, and the
    /// template animation's phase lengths.
    #[must_use]
    pub fn with_template(
        mut self,
        key: impl Into<std::sync::Arc<str>>,
        template: TitleTemplate,
    ) -> Self {
        let template = EngineTitleTemplate::new(key, template);
        self.timing = template.timing();
        self.template = Some(template);
        self
    }

    /// The template's animation, when the title has a template.
    #[must_use]
    pub fn animation(&self) -> Option<&TitleAnimation> {
        self.template.as_ref().map(EngineTitleTemplate::animation)
    }

    /// The title with its template animation's phase lengths.
    #[must_use]
    pub const fn with_timing(mut self, timing: TitleAirTiming) -> Self {
        self.timing = timing;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShowState {
    name: String,
    inputs: Vec<InputId>,
    input_names: Vec<String>,
    outputs: Vec<(OutputId, String)>,
    input_audio_strips: BTreeMap<InputId, EngineInputAudioStripState>,
    desired_switcher: SwitcherState,
    project_locked: bool,
    locked_inputs: BTreeSet<InputId>,
    titles: BTreeMap<InputId, EngineTitle>,
    /// Every Scene input and the scene it shows (GX-011 stage 5).
    scene_inputs: BTreeMap<InputId, SceneId>,
    /// Every alias input and the input it renders (SW-006 cycle checks).
    input_aliases: BTreeMap<InputId, InputId>,
    /// Every registered project scene (each scene a Scene input shows, and
    /// with [`Self::set_scene`] any other), with its animation.
    scenes: BTreeMap<SceneId, EngineScene>,
    playlist: Playlist,
    /// The operator cue lists and their run states (RC-017).
    cue_lists: Vec<fm_model::CueList>,
    /// The project scripts in display order (RC-011).
    scripts: Vec<fm_model::Script>,
    /// The script run waiting at a `wait`, or the last run's fault (RC-011,
    /// schema 56).
    script_run: fm_model::ScriptRunState,
    /// The user shortcuts in display order (RC-004).
    shortcuts: Vec<fm_model::Shortcut>,
    grading: ProjectGrading,
    luts: BTreeMap<LutId, EngineLut>,
    animation_presets: fm_model::AnimationPresetLibrary,
    transition_favorites: fm_model::TransitionFavorites,
    /// Every media input's playback settings (IN-003).
    media: BTreeMap<InputId, fm_model::MediaPlaybackSettings>,
    /// Every media list input's list (IN-006): each is a media input above
    /// whose source is its active item's file.
    media_lists: BTreeMap<InputId, fm_model::MediaList>,
    /// The project's input category list (IN-026).
    input_categories: Vec<fm_model::InputCategory>,
    /// Every input whose category, colour, pause, or close differs from the
    /// default (IN-026).
    input_lifecycle: BTreeMap<InputId, fm_model::InputLifecycle>,
    /// The replaceable source of every input that has one (IN-026).
    input_sources: BTreeMap<InputId, fm_model::InputSourceSpec>,
    /// Every paged (document) input's current zero-based page (IN-014):
    /// exactly the inputs whose source is a document.
    paged: BTreeMap<InputId, u32>,
    /// Every capture device input's settings (IN-001): exactly the inputs
    /// whose source is a device.
    capture: BTreeMap<InputId, fm_types::capture::CaptureSettings>,
    /// Every non-empty strip label (AU-001).
    input_audio_labels: BTreeMap<InputId, String>,
    /// Every strip's group membership (AU-001).
    input_audio_groups: BTreeMap<InputId, fm_model::AudioGroupNumber>,
    /// Every strip whose DSP chain differs from the default (AU-005).
    input_audio_dsp: BTreeMap<InputId, fm_types::strip_dsp::StripDsp>,
    /// Every strip's non-empty plugin chain (AU-006), runtime state.
    input_audio_plugins: BTreeMap<InputId, fm_plugin_host_api::PluginChain>,
    /// The Master bus DSP chain (AU-005).
    master_audio_dsp: fm_types::strip_dsp::StripDsp,
    /// The Master bus plugin chain (AU-006), runtime state.
    master_audio_plugins: fm_plugin_host_api::PluginChain,
    /// The strip groups, group 1 first (AU-001).
    audio_groups: [crate::EngineAudioGroup; fm_model::AUDIO_GROUP_COUNT],
    /// The aux buses, bus A first (AU-002).
    aux_buses: [crate::EngineAuxBus; fm_model::AUX_BUS_COUNT],
    /// The outputs that hear an aux bus instead of Master (AU-002).
    output_aux_buses: BTreeMap<fm_types::OutputId, fm_model::AuxBusNumber>,
    /// How a soloed strip is heard (AU-001).
    audio_solo_mode: fm_model::AudioSoloMode,
    audio_monitor: crate::EngineAudioMonitor,
    /// The configured fullscreen display outputs by slot (OR-011).
    display_outputs:
        BTreeMap<fm_types::display::DisplayOutputSlot, fm_types::display::DisplayOutputConfig>,
    /// The `LiveLAN` viewer stream's configuration (OR-017).
    live_lan: fm_types::live_lan::LiveLanConfig,
    /// What is recorded for replay (OR-012).
    replay: fm_types::replay::ReplaySettings,
    /// The show's own health alert rule settings, in rule order (RC-015).
    alert_rules: Vec<fm_types::alert_rules::AlertRuleSetting>,
    /// The project's highlight reels, in display order (OR-013).
    reels: Vec<fm_types::reel::ReelSpec>,
    /// Every configured stream destination's authored configuration in
    /// authored order (OR-006), parallel to the switcher's stream inventory.
    /// The switcher owns the desired running flags; each entry's own
    /// `running` field is not read.
    stream_targets: Vec<fm_model::StreamTarget>,
}

impl ShowState {
    /// Creates a durable show and its desired switcher state.
    ///
    /// # Errors
    ///
    /// Returns a [`ShowError`] for an invalid show/input name, an empty or
    /// duplicate input set, or unavailable initial switcher selections.
    ///
    /// # Panics
    ///
    /// Never: the initial playlist is empty, which is always valid.
    pub fn new(
        name: impl Into<String>,
        inputs: Vec<(InputId, String)>,
        program: InputId,
        preview: InputId,
    ) -> Result<Self, ShowError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ShowError::EmptyName);
        }
        if inputs.is_empty() {
            return Err(ShowError::NoInputs);
        }
        if inputs.iter().any(|(_, name)| name.trim().is_empty()) {
            return Err(ShowError::EmptyInputName);
        }
        if inputs
            .iter()
            .any(|(_, name)| name.len() > MAX_INPUT_NAME_BYTES)
        {
            return Err(ShowError::InputNameTooLong);
        }
        let distinct_names: HashSet<_> = inputs.iter().map(|(_, name)| name.as_str()).collect();
        if distinct_names.len() != inputs.len() {
            return Err(ShowError::DuplicateInputName);
        }
        let distinct: HashSet<_> = inputs.iter().map(|(input, _)| *input).collect();
        if distinct.len() != inputs.len() {
            return Err(ShowError::DuplicateInput);
        }
        let input_ids = inputs.iter().map(|(input, _)| *input).collect::<Vec<_>>();
        let desired_switcher =
            SwitcherState::new(input_ids.clone(), program, preview).map_err(ShowError::Switcher)?;
        Ok(Self {
            name,
            input_audio_strips: input_ids
                .iter()
                .copied()
                .map(|input| (input, EngineInputAudioStripState::default()))
                .collect(),
            inputs: input_ids,
            input_names: inputs.into_iter().map(|(_, name)| name).collect(),
            outputs: Vec::new(),
            desired_switcher,
            project_locked: false,
            locked_inputs: BTreeSet::new(),
            titles: BTreeMap::new(),
            scene_inputs: BTreeMap::new(),
            input_aliases: BTreeMap::new(),
            scenes: BTreeMap::new(),
            playlist: Playlist::new(Vec::new(), PlaylistConfig::default())
                .expect("an empty playlist is valid"),
            cue_lists: Vec::new(),
            scripts: Vec::new(),
            script_run: fm_model::ScriptRunState::default(),
            shortcuts: Vec::new(),
            grading: ProjectGrading::default(),
            luts: BTreeMap::new(),
            animation_presets: fm_model::AnimationPresetLibrary::default(),
            transition_favorites: fm_model::TransitionFavorites::default(),
            media: BTreeMap::new(),
            media_lists: BTreeMap::new(),
            input_categories: Vec::new(),
            input_lifecycle: BTreeMap::new(),
            input_sources: BTreeMap::new(),
            paged: BTreeMap::new(),
            capture: BTreeMap::new(),
            input_audio_labels: BTreeMap::new(),
            input_audio_groups: BTreeMap::new(),
            input_audio_dsp: BTreeMap::new(),
            input_audio_plugins: BTreeMap::new(),
            master_audio_plugins: fm_plugin_host_api::PluginChain::new(),
            master_audio_dsp: fm_types::strip_dsp::StripDsp::DEFAULT,
            audio_groups: fm_model::AudioGroupNumber::ALL.map(crate::EngineAudioGroup::default_for),
            aux_buses: fm_model::AuxBusNumber::ALL.map(crate::EngineAuxBus::default_for),
            output_aux_buses: BTreeMap::new(),
            audio_solo_mode: fm_model::AudioSoloMode::InPlace,
            audio_monitor: crate::EngineAudioMonitor::default(),
            display_outputs: BTreeMap::new(),
            live_lan: fm_types::live_lan::LiveLanConfig::default(),
            replay: fm_types::replay::ReplaySettings::default(),
            alert_rules: Vec::new(),
            reels: Vec::new(),
            stream_targets: Vec::new(),
        })
    }

    /// Sets the fixed output roster that overlay inclusion can target.
    ///
    /// # Errors
    ///
    /// Returns an error when a name is blank or when an identifier or
    /// case-insensitive name is repeated.
    pub fn with_outputs(mut self, outputs: Vec<(OutputId, String)>) -> Result<Self, ShowError> {
        let mut identifiers = HashSet::with_capacity(outputs.len());
        let mut names = HashSet::with_capacity(outputs.len());
        for (output, name) in &outputs {
            if name.trim().is_empty() {
                return Err(ShowError::EmptyOutputName);
            }
            if !identifiers.insert(*output) {
                return Err(ShowError::DuplicateOutput(*output));
            }
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(ShowError::DuplicateOutputName);
            }
        }
        self.outputs = outputs;
        Ok(self)
    }

    /// Replaces the bounded desired stream inventory carried by the switcher.
    ///
    /// # Errors
    ///
    /// Returns a [`ShowError::Switcher`] error when more than
    /// [`fm_switcher::MAX_STREAM_COUNT`] targets are supplied, an identifier
    /// repeats, or a name is blank or exceeds
    /// [`fm_switcher::MAX_STREAM_NAME_BYTES`] bytes.
    pub fn with_streams(
        mut self,
        streams: impl IntoIterator<Item = (StreamTargetId, String)>,
    ) -> Result<Self, ShowError> {
        self.desired_switcher = self
            .desired_switcher
            .with_streams(streams)
            .map_err(ShowError::Switcher)?;
        Ok(self)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn inputs(&self) -> &[InputId] {
        &self.inputs
    }

    /// Returns canonical input names in the same order as [`Self::inputs`].
    #[must_use]
    pub fn input_names(&self) -> &[String] {
        &self.input_names
    }

    #[must_use]
    pub fn input_name(&self, input: InputId) -> Option<&str> {
        self.inputs
            .iter()
            .position(|candidate| *candidate == input)
            .map(|index| self.input_names[index].as_str())
    }

    #[must_use]
    pub fn outputs(&self) -> &[(OutputId, String)] {
        &self.outputs
    }

    /// Replaces the stream destinations (OR-006): their authored
    /// configurations and the switcher's inventory, in the given order.
    /// Desired running flags are left to [`Self::set_stream_running`].
    ///
    /// # Errors
    ///
    /// The errors of [`Self::with_streams`].
    pub fn with_stream_targets(
        self,
        targets: &[fm_model::StreamTarget],
    ) -> Result<Self, ShowError> {
        let mut show = self.with_streams(targets.iter().map(|target| {
            (
                StreamTargetId::from_non_zero(target.id().get()),
                target.name().to_owned(),
            )
        }))?;
        show.stream_targets = targets.to_vec();
        Ok(show)
    }

    /// Every configured stream destination's authored configuration in
    /// authored order (OR-006). Empty when the show was seeded with
    /// [`Self::with_streams`] alone.
    #[must_use]
    pub fn stream_targets(&self) -> &[fm_model::StreamTarget] {
        &self.stream_targets
    }

    /// Stream destination `target`'s authored configuration (OR-006).
    #[must_use]
    pub fn stream_target(&self, target: StreamTargetId) -> Option<&fm_model::StreamTarget> {
        self.stream_targets
            .iter()
            .find(|candidate| candidate.id().get() == target.get())
    }

    /// Adds, replaces, or (`None`) removes destination `target` in both the
    /// configuration list and the desired switcher's inventory. The caller
    /// has already validated the edit.
    pub(crate) fn set_stream_target_unchecked(
        &mut self,
        target: StreamTargetId,
        config: Option<fm_model::StreamTarget>,
        position: Option<usize>,
    ) -> Result<(), ShowError> {
        let Some(config) = config else {
            self.desired_switcher
                .remove_stream(target)
                .map_err(ShowError::Switcher)?;
            self.stream_targets
                .retain(|candidate| candidate.id().get() != target.get());
            return Ok(());
        };
        self.desired_switcher
            .upsert_stream_at(target, config.name().to_owned(), position)
            .map_err(ShowError::Switcher)?;
        if let Some(index) = self
            .stream_targets
            .iter()
            .position(|candidate| candidate.id().get() == target.get())
        {
            self.stream_targets[index] = config;
            if let Some(position) = position {
                let moved = self.stream_targets.remove(index);
                let position = position.min(self.stream_targets.len());
                self.stream_targets.insert(position, moved);
            }
        } else {
            let index = position.map_or(self.stream_targets.len(), |position| {
                position.min(self.stream_targets.len())
            });
            self.stream_targets.insert(index, config);
        }
        Ok(())
    }

    #[must_use]
    pub fn streams(&self) -> &[DesiredStream] {
        self.desired_switcher.streams()
    }

    /// Returns the desired running flag for one inventoried stream target.
    #[must_use]
    pub fn stream_running(&self, target: StreamTargetId) -> Option<bool> {
        self.desired_switcher.stream_running(target)
    }

    /// Sets the desired running flag for one inventoried stream target.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::Switcher`] with
    /// [`SwitcherError::UnknownStreamTarget`] when the target is unknown.
    pub fn set_stream_running(
        &mut self,
        target: StreamTargetId,
        running: bool,
    ) -> Result<Vec<SwitcherEvent>, ShowError> {
        self.desired_switcher
            .set_stream_running(target, running)
            .map_err(ShowError::Switcher)
    }

    /// Renames one durable input while preserving the exact supplied text.
    ///
    /// # Errors
    ///
    /// Returns a [`RenameInputError`] when the input is unknown or the name is
    /// blank, too long, or already used by another input.
    pub fn rename_input(&mut self, input: InputId, name: String) -> Result<(), RenameInputError> {
        let index = self
            .inputs
            .iter()
            .position(|candidate| *candidate == input)
            .ok_or(RenameInputError::UnknownInput(input))?;
        validate_input_name(&name)?;
        if self
            .input_names
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| candidate_index != index && candidate == &name)
        {
            return Err(RenameInputError::DuplicateName);
        }
        self.input_names[index] = name;
        Ok(())
    }

    /// Reorders show inputs to match the supplied order.
    ///
    /// # Errors
    ///
    /// Returns [`InputOrderError`] when the order is not a permutation of
    /// exactly the current show inputs.
    ///
    /// # Panics
    ///
    /// Panics only if a validated order fails to resolve a canonical input
    /// name, which cannot happen once validation succeeds.
    pub fn reorder_inputs(&mut self, inputs: Vec<InputId>) -> Result<(), InputOrderError> {
        let mut desired_switcher = self.desired_switcher.clone();
        desired_switcher.reorder_inputs(inputs.clone())?;
        fm_types::validate_input_order(&self.inputs, &inputs)?;
        let names = inputs
            .iter()
            .map(|input| {
                self.input_name(*input)
                    .expect("validated input order contains only show inputs")
                    .to_owned()
            })
            .collect();
        self.inputs = inputs;
        self.input_names = names;
        self.desired_switcher = desired_switcher;
        Ok(())
    }

    /// Appends a new input with a default Master strip and no padlock.
    ///
    /// # Errors
    ///
    /// Returns an [`InputCatalogueError`] when the identifier already
    /// exists, the name is blank, too long, or already used, or the show
    /// already holds [`MAX_SHOW_INPUTS`] inputs. The show is unchanged.
    pub fn add_input(&mut self, input: InputId, name: String) -> Result<(), InputCatalogueError> {
        if self.inputs.contains(&input) {
            return Err(InputCatalogueError::DuplicateInput(input));
        }
        validate_input_name(&name).map_err(|error| match error {
            RenameInputError::NameTooLong => InputCatalogueError::NameTooLong,
            RenameInputError::EmptyName
            | RenameInputError::UnknownInput(_)
            | RenameInputError::DuplicateName => InputCatalogueError::EmptyName,
        })?;
        if self.input_names.contains(&name) {
            return Err(InputCatalogueError::DuplicateName);
        }
        if self.inputs.len() >= MAX_SHOW_INPUTS {
            return Err(InputCatalogueError::TooManyInputs {
                maximum: MAX_SHOW_INPUTS,
            });
        }
        self.desired_switcher
            .add_input(input)
            .map_err(|_| InputCatalogueError::DuplicateInput(input))?;
        self.inputs.push(input);
        self.input_names.push(name);
        self.input_audio_strips
            .insert(input, EngineInputAudioStripState::default());
        Ok(())
    }

    /// Returns the first desired switcher reference that keeps `input` from
    /// being removed.
    #[must_use]
    pub fn input_usage(&self, input: InputId) -> Option<InputUsage> {
        self.desired_switcher.input_usage(input)
    }

    /// Removes an input together with its Master strip and padlock.
    ///
    /// # Errors
    ///
    /// Returns [`InputCatalogueError::UnknownInput`] for an input outside the
    /// show, [`InputCatalogueError::ReplaySource`] while it is recorded for
    /// replay, [`InputCatalogueError::ReelClip`] while a highlight reel clip
    /// plays it, [`InputCatalogueError::InUse`] while the desired switcher
    /// references it, [`InputCatalogueError::InPlaylist`] and
    /// [`InputCatalogueError::InCueList`] while a playlist entry or a cue
    /// names it, and [`InputCatalogueError::BoundByCueRun`] while a cue run
    /// in progress (or an armed GO) bound it to a dynamic target it has yet
    /// to act on. The show is unchanged on error.
    pub fn remove_input(&mut self, input: InputId) -> Result<(), InputCatalogueError> {
        let index = self
            .inputs
            .iter()
            .position(|candidate| *candidate == input)
            .ok_or(InputCatalogueError::UnknownInput(input))?;
        if self.replay.sources.contains(&input) {
            return Err(InputCatalogueError::ReplaySource(input));
        }
        if let Some(reel) = self
            .reels
            .iter()
            .find(|reel| reel.clips.iter().any(|clip| clip.angle == input))
        {
            return Err(InputCatalogueError::ReelClip {
                input,
                reel: reel.id,
            });
        }
        if let Some(entry) = self
            .playlist
            .entries()
            .iter()
            .find(|entry| entry.input == input)
        {
            return Err(InputCatalogueError::InPlaylist {
                input,
                entry: entry.id.get(),
            });
        }
        if let Some((list, cue)) = self.cue_lists.iter().find_map(|list| {
            list.cues
                .iter()
                .find(|cue| {
                    cue.actions
                        .iter()
                        .any(|action| action.input() == Some(input))
                })
                .map(|cue| (list.id, cue.id))
        }) {
            return Err(InputCatalogueError::InCueList { input, list, cue });
        }
        // A run in progress (or an armed GO) that bound the input to a
        // dynamic target still to act on it holds it too (RC-004).
        if let Some((list, cue)) = self
            .cue_lists
            .iter()
            .find_map(|list| crate::cue::run_binding(list, input).map(|cue| (list.id, cue)))
        {
            return Err(InputCatalogueError::BoundByCueRun { input, list, cue });
        }
        self.desired_switcher
            .remove_input(input)
            .map_err(|error| match error {
                fm_switcher::SwitcherError::InputInUse { usage, .. } => {
                    InputCatalogueError::InUse { input, usage }
                }
                _ => InputCatalogueError::UnknownInput(input),
            })?;
        self.inputs.remove(index);
        self.input_names.remove(index);
        self.input_audio_strips.remove(&input);
        self.locked_inputs.remove(&input);
        self.titles.remove(&input);
        self.scene_inputs.remove(&input);
        self.input_aliases.remove(&input);
        self.grading.inputs.remove(&input);
        self.media.remove(&input);
        self.media_lists.remove(&input);
        self.input_lifecycle.remove(&input);
        self.input_sources.remove(&input);
        self.paged.remove(&input);
        self.capture.remove(&input);
        self.input_audio_labels.remove(&input);
        self.input_audio_groups.remove(&input);
        self.input_audio_dsp.remove(&input);
        self.input_audio_plugins.remove(&input);
        Ok(())
    }

    #[must_use]
    pub const fn desired_switcher(&self) -> &SwitcherState {
        &self.desired_switcher
    }

    #[must_use]
    pub fn input_audio_strip(&self, input: InputId) -> Option<EngineInputAudioStripState> {
        self.input_audio_strips.get(&input).copied()
    }

    #[must_use]
    pub fn input_audio_strips(&self) -> &BTreeMap<InputId, EngineInputAudioStripState> {
        &self.input_audio_strips
    }

    /// Sets the exact desired Master strip state for one show input.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this show.
    pub fn set_input_audio_strip(
        &mut self,
        input: InputId,
        state: EngineInputAudioStripState,
    ) -> Result<(), ShowError> {
        let strip = self
            .input_audio_strips
            .get_mut(&input)
            .ok_or(ShowError::UnknownInput(input))?;
        *strip = state;
        Ok(())
    }

    /// The operator label of `input`'s strip; empty when unlabelled or
    /// unknown.
    #[must_use]
    pub fn input_audio_label(&self, input: InputId) -> &str {
        self.input_audio_labels
            .get(&input)
            .map_or("", String::as_str)
    }

    /// Every non-empty strip label, ascending by input.
    #[must_use]
    pub const fn input_audio_labels(&self) -> &BTreeMap<InputId, String> {
        &self.input_audio_labels
    }

    /// The group `input`'s strip belongs to, if any.
    #[must_use]
    pub fn input_audio_group(&self, input: InputId) -> Option<fm_model::AudioGroupNumber> {
        self.input_audio_groups.get(&input).copied()
    }

    /// Every strip's group membership, ascending by input.
    #[must_use]
    pub const fn input_audio_groups(&self) -> &BTreeMap<InputId, fm_model::AudioGroupNumber> {
        &self.input_audio_groups
    }

    /// The DSP chain of `input`'s strip; the default chain when unknown.
    #[must_use]
    pub fn input_audio_dsp(&self, input: InputId) -> fm_types::strip_dsp::StripDsp {
        self.input_audio_dsp
            .get(&input)
            .copied()
            .unwrap_or_default()
    }

    /// Every strip whose DSP chain differs from the default, ascending.
    #[must_use]
    pub const fn input_audio_dsps(&self) -> &BTreeMap<InputId, fm_types::strip_dsp::StripDsp> {
        &self.input_audio_dsp
    }

    /// Replaces the DSP chain of `input`'s strip (AU-005).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] for an input outside the show and
    /// [`ShowError::InvalidStripDsp`] for an out-of-range chain.
    pub fn set_input_audio_dsp(
        &mut self,
        input: InputId,
        chain: fm_types::strip_dsp::StripDsp,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        chain.validate().map_err(ShowError::InvalidStripDsp)?;
        if chain.is_default() {
            self.input_audio_dsp.remove(&input);
            self.input_audio_plugins.remove(&input);
        } else {
            self.input_audio_dsp.insert(input, chain);
        }
        Ok(())
    }

    /// `input`'s plugin chain (AU-006); empty for a strip without one.
    #[must_use]
    pub fn input_audio_plugins(&self, input: InputId) -> &fm_plugin_host_api::PluginChain {
        self.input_audio_plugins
            .get(&input)
            .unwrap_or(&EMPTY_PLUGIN_CHAIN)
    }

    /// Every strip's non-empty plugin chain (AU-006), ascending.
    #[must_use]
    pub const fn input_audio_plugin_chains(
        &self,
    ) -> &BTreeMap<InputId, fm_plugin_host_api::PluginChain> {
        &self.input_audio_plugins
    }

    /// Replaces `input`'s plugin chain (AU-006); an empty chain is removed.
    pub fn set_input_audio_plugins(
        &mut self,
        input: InputId,
        chain: fm_plugin_host_api::PluginChain,
    ) {
        if chain.is_empty() {
            self.input_audio_plugins.remove(&input);
        } else {
            self.input_audio_plugins.insert(input, chain);
        }
    }

    /// The Master bus plugin chain (AU-006).
    #[must_use]
    pub const fn master_audio_plugins(&self) -> &fm_plugin_host_api::PluginChain {
        &self.master_audio_plugins
    }

    /// Replaces the Master bus plugin chain (AU-006).
    pub fn set_master_audio_plugins(&mut self, chain: fm_plugin_host_api::PluginChain) {
        self.master_audio_plugins = chain;
    }

    /// The Master bus DSP chain (AU-005).
    #[must_use]
    pub const fn master_audio_dsp(&self) -> &fm_types::strip_dsp::StripDsp {
        &self.master_audio_dsp
    }

    /// Replaces the Master bus DSP chain; callers validate it.
    pub const fn set_master_audio_dsp(&mut self, chain: fm_types::strip_dsp::StripDsp) {
        self.master_audio_dsp = chain;
    }

    /// The strip groups, group 1 first.
    #[must_use]
    pub const fn audio_groups(&self) -> &[crate::EngineAudioGroup; fm_model::AUDIO_GROUP_COUNT] {
        &self.audio_groups
    }

    #[must_use]
    pub const fn audio_group(&self, group: fm_model::AudioGroupNumber) -> &crate::EngineAudioGroup {
        &self.audio_groups[group.index()]
    }

    pub(crate) const fn audio_group_mut(
        &mut self,
        group: fm_model::AudioGroupNumber,
    ) -> &mut crate::EngineAudioGroup {
        &mut self.audio_groups[group.index()]
    }

    /// The aux buses, bus A first (AU-002).
    #[must_use]
    pub const fn aux_buses(&self) -> &[crate::EngineAuxBus; fm_model::AUX_BUS_COUNT] {
        &self.aux_buses
    }

    #[must_use]
    pub const fn aux_bus(&self, bus: fm_model::AuxBusNumber) -> &crate::EngineAuxBus {
        &self.aux_buses[bus.index()]
    }

    pub(crate) const fn aux_bus_mut(
        &mut self,
        bus: fm_model::AuxBusNumber,
    ) -> &mut crate::EngineAuxBus {
        &mut self.aux_buses[bus.index()]
    }

    /// Restores one aux bus.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidAudioMixer`] for an unusable name and
    /// [`ShowError::InvalidAudioGroupGain`] for an out-of-range fader.
    pub fn set_aux_bus(
        &mut self,
        bus: fm_model::AuxBusNumber,
        value: crate::EngineAuxBus,
    ) -> Result<(), ShowError> {
        fm_model::validate_audio_group_name(&value.name).map_err(ShowError::InvalidAudioMixer)?;
        crate::audio_mixer::validate_group_state(value.state)
            .map_err(|_| ShowError::InvalidAudioGroupGain(value.state.gain_millidb))?;
        self.aux_buses[bus.index()] = value;
        Ok(())
    }

    /// The outputs that hear an aux bus instead of Master, ascending.
    #[must_use]
    pub const fn output_aux_buses(&self) -> &BTreeMap<fm_types::OutputId, fm_model::AuxBusNumber> {
        &self.output_aux_buses
    }

    /// The aux bus `output` hears, or `None` for Master.
    #[must_use]
    pub fn output_aux_bus(&self, output: fm_types::OutputId) -> Option<fm_model::AuxBusNumber> {
        self.output_aux_buses.get(&output).copied()
    }

    /// Makes `output` hear `bus`, or Master for `None`; the caller checks
    /// that the output exists.
    pub fn set_output_aux_bus(
        &mut self,
        output: fm_types::OutputId,
        bus: Option<fm_model::AuxBusNumber>,
    ) {
        match bus {
            Some(bus) => self.output_aux_buses.insert(output, bus),
            None => self.output_aux_buses.remove(&output),
        };
    }

    /// Sets the operator label of `input`'s strip; empty clears it.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] for an input outside the show and
    /// [`ShowError::InvalidAudioMixer`] for an unusable label.
    pub fn set_input_audio_label(
        &mut self,
        input: InputId,
        label: String,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        fm_model::validate_audio_strip_label(&label).map_err(ShowError::InvalidAudioMixer)?;
        if label.is_empty() {
            self.input_audio_labels.remove(&input);
        } else {
            self.input_audio_labels.insert(input, label);
        }
        Ok(())
    }

    /// Assigns `input`'s strip to `group`, or to none.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] for an input outside the show.
    pub fn set_input_audio_group(
        &mut self,
        input: InputId,
        group: Option<fm_model::AudioGroupNumber>,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        match group {
            Some(group) => self.input_audio_groups.insert(input, group),
            None => self.input_audio_groups.remove(&input),
        };
        Ok(())
    }

    /// Restores one strip group's name, fader, mute, solo, and parent.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidAudioMixer`] for an unusable name,
    /// [`ShowError::InvalidAudioGroupGain`] for an out-of-range fader, and
    /// [`ShowError::AudioGroupCycle`] for a parent that would nest the group
    /// inside itself.
    pub fn set_audio_group(
        &mut self,
        group: fm_model::AudioGroupNumber,
        value: crate::EngineAudioGroup,
    ) -> Result<(), ShowError> {
        fm_model::validate_audio_group_name(&value.name).map_err(ShowError::InvalidAudioMixer)?;
        crate::audio_mixer::validate_group_state(value.state)
            .map_err(|_| ShowError::InvalidAudioGroupGain(value.state.gain_millidb))?;
        if !crate::audio_mixer::audio_group_parent_is_acyclic(
            &self.audio_groups,
            group,
            value.parent,
        ) {
            return Err(ShowError::AudioGroupCycle(group));
        }
        self.audio_groups[group.index()] = value;
        Ok(())
    }

    /// How a soloed strip is heard (AU-001).
    #[must_use]
    pub const fn audio_solo_mode(&self) -> fm_model::AudioSoloMode {
        self.audio_solo_mode
    }

    /// Restores the solo mode.
    pub const fn set_audio_solo_mode(&mut self, mode: fm_model::AudioSoloMode) {
        self.audio_solo_mode = mode;
    }

    /// The monitor output: its device and live level (AU-001).
    #[must_use]
    pub const fn audio_monitor(&self) -> &crate::EngineAudioMonitor {
        &self.audio_monitor
    }

    pub(crate) const fn audio_monitor_mut(&mut self) -> &mut crate::EngineAudioMonitor {
        &mut self.audio_monitor
    }

    /// Restores the monitor output.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidAudioMonitorDevice`] for an unusable device name
    /// or [`ShowError::InvalidAudioMonitorGain`] for a level outside the strip range.
    /// Nothing changes on error.
    pub fn set_audio_monitor(
        &mut self,
        monitor: crate::EngineAudioMonitor,
    ) -> Result<(), ShowError> {
        if let Some(device) = &monitor.device {
            fm_model::validate_audio_monitor_device(device)
                .map_err(|_| ShowError::InvalidAudioMonitorDevice)?;
        }
        crate::audio_mixer::validate_group_state(monitor.state)
            .map_err(|_| ShowError::InvalidAudioMonitorGain(monitor.state.gain_millidb))?;
        self.audio_monitor = monitor;
        Ok(())
    }

    /// What is recorded for replay (OR-012).
    #[must_use]
    pub const fn replay(&self) -> &fm_types::replay::ReplaySettings {
        &self.replay
    }

    /// Replaces the replay settings, checked against the show's inputs
    /// exactly as [`fm_model::Project::set_replay`] checks them: a source
    /// must be an input of the show and not a Replay input.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::replay::ReplaySettingsError`]. Nothing
    /// changes on error.
    pub fn set_replay(
        &mut self,
        settings: fm_types::replay::ReplaySettings,
    ) -> Result<(), fm_types::replay::ReplaySettingsError> {
        settings.check(|input| {
            if !self.inputs.contains(&input) {
                fm_types::replay::ReplaySourceCheck::Unknown
            } else if matches!(
                self.input_sources.get(&input),
                Some(fm_model::InputSourceSpec::Replay)
            ) {
                fm_types::replay::ReplaySourceCheck::Unrecordable
            } else {
                fm_types::replay::ReplaySourceCheck::Recordable
            }
        })?;
        self.replay = settings;
        Ok(())
    }

    /// The project's highlight reels, in display order (OR-013).
    #[must_use]
    pub fn reels(&self) -> &[fm_types::reel::ReelSpec] {
        &self.reels
    }

    /// The reel with `id`.
    #[must_use]
    pub fn reel(&self, id: u64) -> Option<&fm_types::reel::ReelSpec> {
        self.reels.iter().find(|reel| reel.id == id)
    }

    /// Replaces the reel with `spec.id` or appends `spec`, and returns the
    /// previous reel when one was replaced.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::reel::ReelSpecError`] of a reel with no
    /// clips, too many clips, a bad name, an angle that is not a recordable
    /// input of the show, an unsupported speed, or a trim out of range, and
    /// [`fm_types::reel::ReelSpecError::TooManyReels`] when appending a reel
    /// beyond [`fm_types::reel::MAX_REELS`]. Nothing changes on error.
    pub fn set_reel(
        &mut self,
        spec: fm_types::reel::ReelSpec,
    ) -> Result<Option<fm_types::reel::ReelSpec>, fm_types::reel::ReelSpecError> {
        spec.check(|input| {
            self.inputs.contains(&input)
                && !matches!(
                    self.input_sources.get(&input),
                    Some(fm_model::InputSourceSpec::Replay)
                )
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

    /// Restores persisted reels without checking them; problems are
    /// reported by the project's validation.
    pub fn restore_reels(&mut self, reels: Vec<fm_types::reel::ReelSpec>) {
        self.reels = reels;
    }

    /// The `LiveLAN` viewer stream's configuration (OR-017).
    #[must_use]
    pub const fn live_lan(&self) -> &fm_types::live_lan::LiveLanConfig {
        &self.live_lan
    }

    /// Replaces the `LiveLAN` configuration, checked against the show's
    /// outputs exactly as [`fm_model::Project::set_live_lan`] checks it.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::live_lan::LiveLanConfigError`]. Nothing
    /// changes on error.
    pub fn set_live_lan(
        &mut self,
        config: fm_types::live_lan::LiveLanConfig,
    ) -> Result<(), fm_types::live_lan::LiveLanConfigError> {
        let outputs = self
            .outputs
            .iter()
            .map(|(output, _)| *output)
            .collect::<Vec<_>>();
        config.check(&outputs)?;
        self.live_lan = config;
        Ok(())
    }

    /// The show's own health alert rule settings, in rule order (RC-015);
    /// a rule without one uses its default.
    #[must_use]
    pub fn alert_rules(&self) -> &[fm_types::alert_rules::AlertRuleSetting] {
        &self.alert_rules
    }

    /// Sets one health alert rule's setting, checked exactly as
    /// [`fm_model::Project::set_alert_rule`] checks it; the rule's default
    /// removes the show's own setting.
    ///
    /// # Errors
    ///
    /// Returns the [`fm_types::alert_rules::AlertRuleError`]. Nothing
    /// changes on error.
    pub fn set_alert_rule(
        &mut self,
        setting: fm_types::alert_rules::AlertRuleSetting,
    ) -> Result<(), fm_types::alert_rules::AlertRuleError> {
        setting.check()?;
        fm_types::alert_rules::put_setting(&mut self.alert_rules, setting);
        Ok(())
    }

    /// The configured fullscreen display outputs by slot (OR-011).
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

    /// Configures (`Some`) or clears (`None`) one display output slot,
    /// checked against the show's outputs and the other slot exactly as
    /// [`fm_model::Project::set_display_output`] checks it.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidDisplayOutput`] with the refusal code.
    /// Nothing changes on error.
    pub fn set_display_output(
        &mut self,
        slot: fm_types::display::DisplayOutputSlot,
        config: Option<fm_types::display::DisplayOutputConfig>,
    ) -> Result<(), ShowError> {
        let Some(config) = config else {
            self.display_outputs.remove(&slot);
            return Ok(());
        };
        let outputs = self
            .outputs
            .iter()
            .map(|(output, _)| *output)
            .collect::<Vec<_>>();
        fm_model::check_display_output(
            slot,
            &config,
            &outputs,
            self.display_outputs
                .iter()
                .map(|(slot, config)| (*slot, config)),
        )
        .map_err(|error| ShowError::InvalidDisplayOutput(slot, error.code()))?;
        self.display_outputs.insert(slot, config);
        Ok(())
    }

    /// Restores which outputs Fade to Black leaves live (SW-004).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownOutput`] for an output outside the show's
    /// output roster.
    pub fn restore_fade_to_black_excluded_outputs(
        &mut self,
        excluded: impl IntoIterator<Item = OutputId>,
    ) -> Result<(), ShowError> {
        for output in excluded {
            if !self
                .outputs
                .iter()
                .any(|(candidate, _)| *candidate == output)
            {
                return Err(ShowError::UnknownOutput(output));
            }
            let _ = self
                .desired_switcher
                .set_fade_to_black_output(output, false);
        }
        Ok(())
    }

    /// Restores the outputs that fade over their own Fade-to-Black duration
    /// and the curve every move eases along (SW-004).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownOutput`] for an output outside the show's
    /// output roster, or [`ShowError::InvalidFadeToBlackDuration`] for a
    /// duration outside 1 through 3600 frames.
    pub fn restore_fade_to_black_timing(
        &mut self,
        durations: impl IntoIterator<Item = (OutputId, u32)>,
        curve: fm_switcher::FadeToBlackCurve,
    ) -> Result<(), ShowError> {
        for (output, duration) in durations {
            if !self
                .outputs
                .iter()
                .any(|(candidate, _)| *candidate == output)
            {
                return Err(ShowError::UnknownOutput(output));
            }
            self.desired_switcher
                .set_fade_to_black_output_duration(output, Some(duration))
                .map_err(|_| ShowError::InvalidFadeToBlackDuration(duration))?;
        }
        let _ = self.desired_switcher.set_fade_to_black_curve(curve);
        Ok(())
    }

    /// Restores exact desired manual-transition state from a checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::Switcher`] when the checkpoint is inconsistent
    /// with the desired Program/Preview routing.
    pub fn restore_manual_transition(&mut self, state: TBarState) -> Result<(), ShowError> {
        self.desired_switcher
            .restore_t_bar(state)
            .map_err(ShowError::Switcher)
    }

    /// Restores a settled desired Fade-to-Black endpoint from a checkpoint.
    pub fn restore_fade_to_black(&mut self, active: bool) {
        self.desired_switcher.restore_settled_fade_to_black(active);
    }

    /// Configures one durable Stinger slot.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::Switcher`] when the descriptor references an input
    /// outside this show.
    pub fn configure_stinger(
        &mut self,
        slot: StingerSlotId,
        descriptor: StingerDescriptor,
    ) -> Result<(), ShowError> {
        self.desired_switcher
            .configure_stinger(slot, descriptor)
            .map_err(ShowError::Switcher)
    }

    /// Removes one durable Stinger slot and its readiness state.
    pub fn remove_stinger(&mut self, slot: StingerSlotId) {
        self.desired_switcher.remove_stinger(slot);
    }

    /// Records whether one configured Stinger media input is ready.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::Switcher`] when the slot is unconfigured.
    pub fn preload_stinger(
        &mut self,
        slot: StingerSlotId,
        media_available: bool,
    ) -> Result<SwitcherEvent, ShowError> {
        self.desired_switcher
            .preload_stinger(slot, media_available)
            .map_err(ShowError::Switcher)
    }

    /// Returns whether the project-wide edit padlock is engaged.
    #[must_use]
    pub const fn is_project_locked(&self) -> bool {
        self.project_locked
    }

    /// Returns whether one input's edit padlock is engaged.
    #[must_use]
    pub fn is_input_locked(&self, input: InputId) -> bool {
        self.locked_inputs.contains(&input)
    }

    /// Returns the inputs whose edit padlock is engaged, in identifier order.
    #[must_use]
    pub const fn locked_inputs(&self) -> &BTreeSet<InputId> {
        &self.locked_inputs
    }

    /// Engages or releases the project-wide edit padlock.
    pub const fn set_project_locked(&mut self, locked: bool) {
        self.project_locked = locked;
    }

    /// Engages or releases one input's edit padlock.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this show.
    pub fn set_input_locked(&mut self, input: InputId, locked: bool) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        if locked {
            self.locked_inputs.insert(input);
        } else {
            self.locked_inputs.remove(&input);
        }
        Ok(())
    }

    /// Makes `input` a title input with `title` as its live field state,
    /// replacing any title it already had.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this show.
    pub fn set_title(&mut self, input: InputId, title: EngineTitle) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        self.titles.insert(input, title);
        Ok(())
    }

    /// Returns the live field state of a title input.
    #[must_use]
    pub fn title(&self, input: InputId) -> Option<&EngineTitle> {
        self.titles.get(&input)
    }

    /// Returns every title input's live field state, in identifier order.
    #[must_use]
    pub const fn titles(&self) -> &BTreeMap<InputId, EngineTitle> {
        &self.titles
    }

    pub(crate) fn title_mut(&mut self, input: InputId) -> Option<&mut EngineTitle> {
        self.titles.get_mut(&input)
    }

    /// Makes `input` a Scene input showing `scene`, registering (or
    /// replacing) the scene, its animation, and the `canvas_width` x
    /// `canvas_height` project canvas its presets measure against. Every
    /// Scene input naming the same scene shares it.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this show.
    pub fn set_scene_input(
        &mut self,
        input: InputId,
        scene: fm_model::Scene,
        canvas_width: u32,
        canvas_height: u32,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        self.scene_inputs.insert(input, scene.id);
        self.scenes.insert(
            scene.id,
            EngineScene::new(scene, canvas_width, canvas_height),
        );
        Ok(())
    }

    /// Registers (or replaces) `scene`, its animation, and the
    /// `canvas_width` x `canvas_height` project canvas, whether or not a
    /// Scene input shows it, so its layers' grades can be edited (SW-019).
    pub fn set_scene(&mut self, scene: fm_model::Scene, canvas_width: u32, canvas_height: u32) {
        self.scenes.insert(
            scene.id,
            EngineScene::new(scene, canvas_width, canvas_height),
        );
    }

    /// The scene Scene input `input` shows.
    #[must_use]
    pub fn scene_input(&self, input: InputId) -> Option<&EngineScene> {
        self.scenes.get(self.scene_inputs.get(&input)?)
    }

    /// Every Scene input and the scene it shows, in identifier order.
    #[must_use]
    pub const fn scene_inputs(&self) -> &BTreeMap<InputId, SceneId> {
        &self.scene_inputs
    }

    /// Records that alias input `input` renders exactly what `target`
    /// renders, so a scene layer drawing the alias draws its target (the
    /// live layer edits' cycle check follows it).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when either input is outside
    /// this show.
    pub fn set_input_alias(&mut self, input: InputId, target: InputId) -> Result<(), ShowError> {
        for id in [input, target] {
            if !self.inputs.contains(&id) {
                return Err(ShowError::UnknownInput(id));
            }
        }
        self.input_aliases.insert(input, target);
        Ok(())
    }

    /// Every alias input and the input it renders, in identifier order.
    #[must_use]
    pub const fn input_aliases(&self) -> &BTreeMap<InputId, InputId> {
        &self.input_aliases
    }

    /// The input `input` renders: itself, or for an alias input the end of
    /// its alias chain (a chain that loops back ends where it repeats).
    #[must_use]
    pub fn input_rendered(&self, input: InputId) -> InputId {
        let mut current = input;
        let mut seen = BTreeSet::new();
        while let Some(target) = self.input_aliases.get(&current) {
            if !seen.insert(current) {
                break;
            }
            current = *target;
        }
        current
    }

    /// Every registered scene, in identifier order.
    #[must_use]
    pub const fn scenes(&self) -> &BTreeMap<SceneId, EngineScene> {
        &self.scenes
    }

    /// One scene by id.
    #[must_use]
    pub fn scene(&self, scene: SceneId) -> Option<&EngineScene> {
        self.scenes.get(&scene)
    }

    pub(crate) fn scene_mut(&mut self, scene: SceneId) -> Option<&mut EngineScene> {
        self.scenes.get_mut(&scene)
    }

    /// Returns the playlist definition and its automation state. Playlist
    /// time is measured in engine frames.
    #[must_use]
    pub const fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    /// Installs a restored playlist.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when an entry names an input
    /// outside this show.
    pub fn set_playlist(&mut self, playlist: Playlist) -> Result<(), ShowError> {
        if let Some(entry) = playlist
            .entries()
            .iter()
            .find(|entry| !self.inputs.contains(&entry.input))
        {
            return Err(ShowError::UnknownInput(entry.input));
        }
        self.playlist = playlist;
        Ok(())
    }

    pub(crate) fn replace_playlist(&mut self, playlist: Playlist) {
        self.playlist = playlist;
    }

    /// The operator cue lists, in display order, with their run states
    /// (RC-017). Cue time is measured in engine frames.
    #[must_use]
    pub fn cue_lists(&self) -> &[fm_model::CueList] {
        &self.cue_lists
    }

    /// The cue list with `id`.
    #[must_use]
    pub fn cue_list(&self, id: u64) -> Option<&fm_model::CueList> {
        self.cue_lists.iter().find(|list| list.id == id)
    }

    /// Installs restored cue lists.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidCueList`] when a list breaks a bound or
    /// names an input, title, stream target, or script outside this show
    /// (so restore the scripts first).
    pub fn set_cue_lists(&mut self, lists: Vec<fm_model::CueList>) -> Result<(), ShowError> {
        if fm_model::validate_cue_lists(&lists).is_err() {
            return Err(ShowError::InvalidCueList("definition"));
        }
        for list in &lists {
            crate::cue::check_references(self, &list.cues)
                .map_err(|_| ShowError::InvalidCueList("reference"))?;
        }
        self.cue_lists = lists;
        Ok(())
    }

    pub(crate) fn cue_lists_mut(&mut self) -> &mut Vec<fm_model::CueList> {
        &mut self.cue_lists
    }

    /// The project scripts, in display order (RC-011).
    #[must_use]
    pub fn scripts(&self) -> &[fm_model::Script] {
        &self.scripts
    }

    /// The script with `id`.
    #[must_use]
    pub fn script(&self, id: u64) -> Option<&fm_model::Script> {
        self.scripts.iter().find(|script| script.id == id)
    }

    /// Installs restored scripts.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidScripts`] when they break a bound.
    pub fn set_scripts(&mut self, scripts: Vec<fm_model::Script>) -> Result<(), ShowError> {
        fm_model::validate_scripts(&scripts).map_err(|_| ShowError::InvalidScripts)?;
        self.scripts = scripts;
        Ok(())
    }

    pub(crate) fn scripts_mut(&mut self) -> &mut Vec<fm_model::Script> {
        &mut self.scripts
    }

    /// The script run waiting at a `wait`, or the last run's fault (RC-011,
    /// schema 56).
    #[must_use]
    pub const fn script_run(&self) -> &fm_model::ScriptRunState {
        &self.script_run
    }

    /// Installs a restored run slot, checked against the scripts (so
    /// restore them first) at the show's frame `rate`.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidScripts`] when the run does not fit the
    /// scripts (see [`fm_model::validate_script_run`]).
    pub fn set_script_run(
        &mut self,
        script_run: fm_model::ScriptRunState,
        rate: fm_types::FrameRate,
    ) -> Result<(), ShowError> {
        fm_model::validate_script_run(&script_run, &self.scripts, rate)
            .map_err(|_| ShowError::InvalidScripts)?;
        self.script_run = script_run;
        Ok(())
    }

    pub(crate) fn script_run_mut(&mut self) -> &mut fm_model::ScriptRunState {
        &mut self.script_run
    }

    /// The user shortcuts, in display order (RC-004).
    #[must_use]
    pub fn shortcuts(&self) -> &[fm_model::Shortcut] {
        &self.shortcuts
    }

    /// The shortcut with `id`.
    #[must_use]
    pub fn shortcut(&self, id: u64) -> Option<&fm_model::Shortcut> {
        self.shortcuts.iter().find(|shortcut| shortcut.id == id)
    }

    /// Installs restored shortcuts. What they name is checked when they
    /// are fired, not here.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidShortcuts`] when they break a bound.
    pub fn set_shortcuts(&mut self, shortcuts: Vec<fm_model::Shortcut>) -> Result<(), ShowError> {
        fm_model::validate_shortcuts(&shortcuts).map_err(|_| ShowError::InvalidShortcuts)?;
        self.shortcuts = shortcuts;
        Ok(())
    }

    pub(crate) fn shortcuts_mut(&mut self) -> &mut Vec<fm_model::Shortcut> {
        &mut self.shortcuts
    }

    /// Returns the project grading: the grade library, imported LUTs, and
    /// the per-input and program grades.
    #[must_use]
    pub const fn grading(&self) -> &ProjectGrading {
        &self.grading
    }

    /// Returns every imported LUT whose text the show holds, by id. A LUT
    /// registered in [`Self::grading`] without an entry here (its bundled
    /// file was missing at restore) is bypassed when rendering.
    #[must_use]
    pub const fn luts(&self) -> &BTreeMap<LutId, EngineLut> {
        &self.luts
    }

    /// Installs a restored grading and the LUTs loaded for it.
    ///
    /// # Errors
    ///
    /// Returns the first [`GradeEditError`] an edit building the same
    /// grading would report, [`GradeEditError::UnknownInput`] for a grade on
    /// an input outside this show, and [`GradeEditError::UnknownLut`] for a
    /// loaded LUT that is not registered. The show is unchanged on error.
    pub fn restore_grading(
        &mut self,
        grading: ProjectGrading,
        luts: BTreeMap<LutId, EngineLut>,
    ) -> Result<(), GradeEditError> {
        self.grading = checked_grading(&self.inputs, grading, &luts)?;
        self.luts = luts;
        Ok(())
    }

    /// Compiles the per-input and program grades against the held LUTs.
    ///
    /// # Errors
    ///
    /// Returns [`GradeCompileError`] for a grade outside its bounds, which
    /// accepted edits never produce.
    pub fn compiled_grades(&self) -> Result<CompiledProjectGrades, GradeCompileError> {
        let luts = self
            .luts
            .iter()
            .map(|(id, lut)| (*id, std::sync::Arc::clone(lut.cube())))
            .collect();
        CompiledProjectGrades::compile_grading(&self.grading, &luts)
    }

    /// The project's animation preset library (GX-011).
    #[must_use]
    pub const fn animation_presets(&self) -> &fm_model::AnimationPresetLibrary {
        &self.animation_presets
    }

    /// Installs a restored animation preset library.
    ///
    /// # Errors
    ///
    /// Returns the first [`fm_model::AnimationPresetError`] inserting the
    /// presets in order reports. The show is unchanged on error.
    pub fn restore_animation_presets(
        &mut self,
        library: fm_model::AnimationPresetLibrary,
    ) -> Result<(), fm_model::AnimationPresetError> {
        if let Some((_, error)) = library.violations().into_iter().next() {
            return Err(error);
        }
        self.animation_presets = library;
        Ok(())
    }

    /// Makes `input` a media input with `settings`, replacing any settings
    /// it already had.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this
    /// show and [`ShowError::InvalidMediaPlayback`] for out-of-bounds
    /// settings.
    pub fn set_media_input(
        &mut self,
        input: InputId,
        settings: fm_model::MediaPlaybackSettings,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        settings
            .validate()
            .map_err(|error| ShowError::InvalidMediaPlayback(input, error))?;
        self.media.insert(input, settings);
        Ok(())
    }

    /// Media input `input`'s playback settings, or `None` when `input` is
    /// not a media input.
    #[must_use]
    pub fn media_settings(&self, input: InputId) -> Option<&fm_model::MediaPlaybackSettings> {
        self.media.get(&input)
    }

    /// Every media input's playback settings, ascending by input.
    #[must_use]
    pub const fn media_inputs(&self) -> &BTreeMap<InputId, fm_model::MediaPlaybackSettings> {
        &self.media
    }

    /// Replaces an existing media input's settings (validated by the
    /// accepting command).
    pub(crate) fn set_media_settings(
        &mut self,
        input: InputId,
        settings: fm_model::MediaPlaybackSettings,
    ) {
        if let Some(current) = self.media.get_mut(&input) {
            *current = settings;
        }
    }

    /// The project's input category list, in display order (IN-026).
    #[must_use]
    pub fn input_categories(&self) -> &[fm_model::InputCategory] {
        &self.input_categories
    }

    /// One input's lifecycle (the default for an input without an entry).
    #[must_use]
    pub fn input_lifecycle(&self, input: InputId) -> fm_model::InputLifecycle {
        self.input_lifecycle
            .get(&input)
            .copied()
            .unwrap_or_default()
    }

    /// Every input whose lifecycle differs from the default.
    #[must_use]
    pub const fn input_lifecycles(&self) -> &BTreeMap<InputId, fm_model::InputLifecycle> {
        &self.input_lifecycle
    }

    /// Restores the project's category list and per-input lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidInputLifecycle`] for a broken category
    /// list, an unknown input, or an unknown category. The show is
    /// unchanged on error.
    pub fn restore_input_lifecycle(
        &mut self,
        categories: Vec<fm_model::InputCategory>,
        entries: &BTreeMap<InputId, fm_model::InputLifecycle>,
    ) -> Result<(), ShowError> {
        fm_model::validate_input_categories(&categories)
            .map_err(|error| ShowError::InvalidInputLifecycle(error.code()))?;
        for (input, lifecycle) in entries {
            if !self.inputs.contains(input) {
                return Err(ShowError::UnknownInput(*input));
            }
            if let Some(category) = lifecycle.category
                && !categories.iter().any(|candidate| candidate.id == category)
            {
                return Err(ShowError::InvalidInputLifecycle("unknown_category"));
            }
        }
        self.input_categories = categories;
        self.input_lifecycle = entries
            .iter()
            .filter(|(_, lifecycle)| !lifecycle.is_default())
            .map(|(input, lifecycle)| (*input, *lifecycle))
            .collect();
        Ok(())
    }

    pub(crate) fn set_input_categories_unchecked(
        &mut self,
        categories: Vec<fm_model::InputCategory>,
    ) {
        self.input_categories = categories;
    }

    pub(crate) fn set_input_lifecycle_unchecked(
        &mut self,
        input: InputId,
        lifecycle: fm_model::InputLifecycle,
    ) {
        if lifecycle.is_default() {
            self.input_lifecycle.remove(&input);
        } else {
            self.input_lifecycle.insert(input, lifecycle);
        }
    }

    /// Records input `input`'s replaceable source (see
    /// [`fm_model::InputSourceSpec::from_kind`]).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] when the input is outside this
    /// show.
    pub fn set_input_source(
        &mut self,
        input: InputId,
        source: fm_model::InputSourceSpec,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        self.set_input_source_unchecked(input, source);
        Ok(())
    }

    /// Input `input`'s replaceable source, when it has one.
    #[must_use]
    pub fn input_source(&self, input: InputId) -> Option<&fm_model::InputSourceSpec> {
        self.input_sources.get(&input)
    }

    /// Every input's replaceable source, ascending by input.
    #[must_use]
    pub const fn input_sources(&self) -> &BTreeMap<InputId, fm_model::InputSourceSpec> {
        &self.input_sources
    }

    /// Network stream input `input`'s configuration (IN-015), or `None`
    /// when `input` is not a network stream.
    #[must_use]
    pub fn network_stream(
        &self,
        input: InputId,
    ) -> Option<&fm_types::network_stream::NetworkStreamConfig> {
        match self.input_sources.get(&input) {
            Some(fm_model::InputSourceSpec::Network(config)) => Some(config),
            _ => None,
        }
    }

    /// Every network stream input and its configuration, ascending by
    /// input (IN-015).
    pub fn network_streams(
        &self,
    ) -> impl Iterator<Item = (InputId, &fm_types::network_stream::NetworkStreamConfig)> {
        self.input_sources
            .iter()
            .filter_map(|(input, source)| match source {
                fm_model::InputSourceSpec::Network(config) => Some((*input, config)),
                _ => None,
            })
    }

    /// Every screen capture input and its configuration, ascending by input
    /// (IN-011).
    pub fn screen_capture_inputs(
        &self,
    ) -> impl Iterator<Item = (InputId, &fm_types::screen_capture::ScreenCaptureConfig)> {
        self.input_sources
            .iter()
            .filter_map(|(input, source)| match source {
                fm_model::InputSourceSpec::ScreenCapture(config) => Some((*input, config)),
                _ => None,
            })
    }

    /// Every Replay input, ascending (OR-012).
    pub fn replay_inputs(&self) -> impl Iterator<Item = InputId> + '_ {
        self.input_sources
            .iter()
            .filter(|(_, source)| matches!(source, fm_model::InputSourceSpec::Replay))
            .map(|(input, _)| *input)
    }

    /// Every system audio input and its device, ascending by input
    /// (AU-011).
    pub fn system_audio_inputs(
        &self,
    ) -> impl Iterator<Item = (InputId, &fm_types::system_audio::SystemAudioConfig)> {
        self.input_sources
            .iter()
            .filter_map(|(input, source)| match source {
                fm_model::InputSourceSpec::SystemAudio(config) => Some((*input, config)),
                _ => None,
            })
    }

    /// Records `source`; a device source keeps the input's capture settings
    /// (defaults for an input that was not a device), any other source
    /// drops them (IN-001).
    pub(crate) fn set_input_source_unchecked(
        &mut self,
        input: InputId,
        source: fm_model::InputSourceSpec,
    ) {
        if matches!(source, fm_model::InputSourceSpec::Device { .. }) {
            self.capture.entry(input).or_default();
        } else {
            self.capture.remove(&input);
        }
        // A new or replaced document starts on its first page (IN-014).
        if matches!(source, fm_model::InputSourceSpec::Document { .. }) {
            if self.input_sources.get(&input) != Some(&source) {
                self.paged.insert(input, 0);
            }
        } else {
            self.paged.remove(&input);
        }
        self.input_sources.insert(input, source);
    }

    /// Paged input `input`'s current page (IN-014), or `None` when `input`
    /// is not a paged input.
    #[must_use]
    pub fn paged_page(&self, input: InputId) -> Option<u32> {
        self.paged.get(&input).copied()
    }

    /// Every paged input's current page, ascending by input (IN-014).
    #[must_use]
    pub const fn paged_inputs(&self) -> &BTreeMap<InputId, u32> {
        &self.paged
    }

    /// Every document input and its asset URI, ascending by input (IN-014).
    pub fn document_inputs(&self) -> impl Iterator<Item = (InputId, &str)> {
        self.input_sources
            .iter()
            .filter_map(|(input, source)| match source {
                fm_model::InputSourceSpec::Document { asset_uri } => {
                    Some((*input, asset_uri.as_str()))
                }
                _ => None,
            })
    }

    /// Restores paged input `input`'s page, as a project restores it; its
    /// source must already be a document (IN-014).
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] for an input outside this show
    /// and [`ShowError::InvalidPagedPage`] for an input that is not paged
    /// or a page past the paged bound.
    pub fn set_paged_page(&mut self, input: InputId, page: u32) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        if !self.paged.contains_key(&input) || page >= fm_types::paged::MAX_PAGED_PAGES {
            return Err(ShowError::InvalidPagedPage(input));
        }
        self.paged.insert(input, page);
        Ok(())
    }

    pub(crate) fn set_paged_page_unchecked(&mut self, input: InputId, page: u32) {
        if let Some(current) = self.paged.get_mut(&input) {
            *current = page;
        }
    }

    /// Capture input `input`'s settings (IN-001), or `None` when `input` is
    /// not a capture device input.
    #[must_use]
    pub fn capture_settings(&self, input: InputId) -> Option<&fm_types::capture::CaptureSettings> {
        self.capture.get(&input)
    }

    /// Every capture device input with its stable key and settings,
    /// ascending by input (IN-001).
    pub fn capture_inputs(
        &self,
    ) -> impl Iterator<Item = (InputId, &str, &fm_types::capture::CaptureSettings)> {
        self.capture
            .iter()
            .filter_map(|(input, settings)| match self.input_sources.get(input) {
                Some(fm_model::InputSourceSpec::Device { stable_key }) => {
                    Some((*input, stable_key.as_str(), settings))
                }
                _ => None,
            })
    }

    /// Replaces capture input `input`'s settings (IN-001), as a project
    /// restores them; its source must already be a device.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::UnknownInput`] for an input outside this show
    /// and [`ShowError::InvalidCaptureSettings`] for an input that is not a
    /// capture device or invalid settings.
    pub fn set_capture_settings(
        &mut self,
        input: InputId,
        settings: fm_types::capture::CaptureSettings,
    ) -> Result<(), ShowError> {
        if !self.inputs.contains(&input) {
            return Err(ShowError::UnknownInput(input));
        }
        if !self.capture.contains_key(&input) || settings.validate().is_err() {
            return Err(ShowError::InvalidCaptureSettings(input));
        }
        self.capture.insert(input, settings);
        Ok(())
    }

    pub(crate) fn set_capture_settings_unchecked(
        &mut self,
        input: InputId,
        settings: fm_types::capture::CaptureSettings,
    ) {
        self.capture.insert(input, settings);
    }

    pub(crate) fn remove_media_input(&mut self, input: InputId) {
        self.media.remove(&input);
        self.media_lists.remove(&input);
    }

    /// Every media list input's list, by input (IN-006).
    #[must_use]
    pub const fn media_lists(&self) -> &BTreeMap<InputId, fm_model::MediaList> {
        &self.media_lists
    }

    /// Media list input `input`'s list.
    #[must_use]
    pub fn media_list(&self, input: InputId) -> Option<&fm_model::MediaList> {
        self.media_lists.get(&input)
    }

    /// Restores every media list (IN-006): each must be valid and belong to
    /// a media input of the show playing its active item's file.
    ///
    /// # Errors
    ///
    /// Returns [`ShowError::InvalidMediaList`] for a list that does not fit
    /// its input; the show is unchanged on error.
    pub fn restore_media_lists(
        &mut self,
        lists: BTreeMap<InputId, fm_model::MediaList>,
    ) -> Result<(), ShowError> {
        if lists.len() > fm_model::MAX_MEDIA_LISTS
            && let Some(input) = lists.keys().next()
        {
            return Err(ShowError::InvalidMediaList(*input));
        }
        for (input, list) in &lists {
            let plays_active = matches!(
                self.input_sources.get(input),
                Some(fm_model::InputSourceSpec::Media { asset_uri })
                    if list.active_item().is_some_and(|item| item.asset_uri == *asset_uri)
            );
            if list.validate().is_err() || !self.media.contains_key(input) || !plays_active {
                return Err(ShowError::InvalidMediaList(*input));
            }
        }
        self.media_lists = lists;
        Ok(())
    }

    pub(crate) fn insert_media_list(&mut self, input: InputId, list: fm_model::MediaList) {
        self.media_lists.insert(input, list);
    }

    pub(crate) fn insert_media_input(
        &mut self,
        input: InputId,
        settings: fm_model::MediaPlaybackSettings,
    ) {
        self.media.insert(input, settings);
    }

    pub(crate) fn insert_title(&mut self, input: InputId, title: EngineTitle) {
        self.titles.insert(input, title);
    }

    pub(crate) fn insert_scene_input(&mut self, input: InputId, scene: SceneId) {
        self.scene_inputs.insert(input, scene);
    }

    pub(crate) fn insert_input_alias(&mut self, input: InputId, target: InputId) {
        self.input_aliases.insert(input, target);
    }

    pub(crate) fn scene_of_input(&self, input: InputId) -> Option<SceneId> {
        self.scene_inputs.get(&input).copied()
    }

    pub(crate) const fn animation_presets_mut(&mut self) -> &mut fm_model::AnimationPresetLibrary {
        &mut self.animation_presets
    }

    /// The project's favourite transition buttons (SW-002).
    #[must_use]
    pub const fn transition_favorites(&self) -> &fm_model::TransitionFavorites {
        &self.transition_favorites
    }

    /// Installs restored favourite transition buttons; each is valid by
    /// construction.
    pub const fn restore_transition_favorites(&mut self, favorites: fm_model::TransitionFavorites) {
        self.transition_favorites = favorites;
    }

    /// Assigns one favourite, refusing a slot outside
    /// `1..=`[`fm_model::TRANSITION_FAVORITE_COUNT`].
    pub(crate) fn set_transition_favorite(
        &mut self,
        slot: u8,
        favorite: fm_model::TransitionFavorite,
    ) -> Result<(), fm_command::Rejection> {
        self.transition_favorites
            .set(slot, favorite)
            .map(|_| ())
            .ok_or_else(|| {
                let message = format!(
                    "favourite transition slot {slot} must be 1 through {}",
                    fm_model::TRANSITION_FAVORITE_COUNT
                );
                fm_command::Rejection::new(
                    fm_command::RejectionCode::InvalidCommand,
                    message.clone(),
                )
                .with_field(fm_command::FieldIssue::new(
                    "slot",
                    "out_of_range",
                    message,
                ))
            })
    }

    pub(crate) fn grading_parts_mut(
        &mut self,
    ) -> (
        &[InputId],
        &mut ProjectGrading,
        &mut BTreeMap<LutId, EngineLut>,
    ) {
        (&self.inputs, &mut self.grading, &mut self.luts)
    }

    pub const fn desired_switcher_mut(&mut self) -> &mut SwitcherState {
        &mut self.desired_switcher
    }
}

impl fmt::Display for ShowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(formatter)
    }
}

/// Stores `item` in an authored project list (shortcuts, scripts, cue
/// lists): it replaces the entry at `existing`, or is new. `Some(position)`
/// places it at that index, clamped to the end of the list; `None` keeps an
/// existing entry where it is and appends a new one.
pub(crate) fn place_in_list<T>(
    list: &mut Vec<T>,
    existing: Option<usize>,
    item: T,
    position: Option<usize>,
) {
    if let Some(index) = existing {
        list[index] = item;
        if let Some(position) = position {
            let moved = list.remove(index);
            let position = position.min(list.len());
            list.insert(position, moved);
        }
    } else {
        let index = position.map_or(list.len(), |position| position.min(list.len()));
        list.insert(index, item);
    }
}

/// The index an entry ends up at in a list of `len` entries where it is
/// at `existing` (see [`place_in_list`]), so a recorded edit replays
/// exactly.
pub(crate) fn placed_index(len: usize, existing: Option<usize>, position: Option<usize>) -> usize {
    let last = existing.map_or(len, |_| len - 1);
    position.map_or(existing.unwrap_or(last), |position| position.min(last))
}
