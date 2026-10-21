use std::{collections::BTreeMap, num::NonZeroU128};

use fm_model::{
    AudioBus, BusSend, CropRect, Input, InputAudioStrip, InputAudioStripState,
    InputBalanceBasisPoints, InputDelaySamples, InputGainMilliDb, InputKind, InputPanBasisPoints,
    Layer, LayerAnchor, LayerEffect, LayerGeometry, MainMix, MixFeed, Output, Playlist,
    PlaylistEntry, PlaylistLoop, PlaylistRunState, PlaylistTransition, PlaylistTransitionKind,
    Project, ProjectGrading, ProjectSettings, RECORDER_SLOT_COUNT, RecorderSlot, RectMask,
    RestartPolicy, Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo,
    SolidColor, SourceRef, SrtPassphrase, StartupPolicy, StingerAudioPolicy, StingerConfig,
    StingerMissingMediaFallback, StingerSlotNumber, StreamEndpoint, StreamKey, StreamLadder,
    StreamLadderRung, StreamProtocol, StreamReconnectPolicy, StreamTarget, StreamTargetId,
    TitleFieldEntry, TitleFieldId, TitleFieldValue, TitleInput,
};
use fm_types::{
    AudioFormat, BusId, Channel, ChannelLayout, ChromaLocation, ColorMetadata, ColorPrimaries,
    DiscreteChannel, FrameRate, InputId, MatrixCoefficients, OutputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, SignalRange, TransferFunction, VideoDimensions,
    VideoFormat,
};

use crate::{
    CURRENT_SCHEMA_VERSION, FadeToBlackState, IdempotencyReceipt, ManualTransitionKind,
    ManualTransitionState, ProjectPosition, ProjectValidationError, RuntimeFadeToBlack,
    RuntimeManualTransitions, RuntimeOverlayBorder, RuntimeOverlayChannel, RuntimeOverlayPosition,
    RuntimeOverlayTransition, RuntimeOverlays, RuntimeRouting, StoredProject,
};

use super::{DecodeError, Reader, Value};

pub(crate) fn decode(source: &str) -> Result<StoredProject, DecodeError> {
    let mut root = Object::new(Reader::new(source).document()?, "manifest")?;
    let schema = root.u32("schema_version")?;
    if schema != CURRENT_SCHEMA_VERSION {
        return Err(DecodeError::Validation(
            ProjectValidationError::UnsupportedSchema {
                found: schema,
                supported: CURRENT_SCHEMA_VERSION,
            },
        ));
    }
    let project = ProjectDto::parse(root.take("project")?)?.into_domain();
    let (routing, manual_transitions, fade_to_black, overlays, position, receipts) =
        parse_runtime(root.take("runtime")?)?;
    root.finish()?;
    StoredProject::from_project_with_complete_runtime_state(
        project,
        routing,
        manual_transitions,
        fade_to_black,
        overlays,
        position,
        receipts,
    )
    .map_err(DecodeError::Validation)
}

struct ProjectDto {
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
    edit_locks: EditLocks,
    playlist: Playlist,
    cue_lists: Vec<fm_model::CueList>,
    grading: ProjectGrading,
    animation_presets: fm_model::AnimationPresetLibrary,
    transition_favorites: fm_model::TransitionFavorites,
    input_lifecycle: super::input_lifecycle::ParsedInputLifecycle,
    audio_groups: fm_model::AudioGroups,
    audio_solo_mode: fm_model::AudioSoloMode,
    audio_monitor_output: fm_model::AudioMonitorOutput,
    master_audio_dsp: fm_types::strip_dsp::StripDsp,
    fade_to_black_excluded_outputs: std::collections::BTreeSet<fm_types::OutputId>,
    fade_to_black_output_durations: Vec<(fm_types::OutputId, u32)>,
    fade_to_black_curve: fm_types::FadeToBlackCurve,
    display_outputs: Vec<(
        fm_types::display::DisplayOutputSlot,
        fm_types::display::DisplayOutputConfig,
    )>,
    live_lan: fm_types::live_lan::LiveLanConfig,
    scripts: Vec<fm_model::Script>,
    script_run: fm_model::ScriptRunState,
    shortcuts: Vec<fm_model::Shortcut>,
    media_lists: std::collections::BTreeMap<fm_types::InputId, fm_model::MediaList>,
    title_timelines: std::collections::BTreeMap<fm_types::InputId, fm_model::TitleTimeline>,
    screen_capture_restore_tokens: Vec<(fm_types::InputId, String)>,
}

struct EditLocks {
    project: bool,
    inputs: Vec<InputId>,
}

impl ProjectDto {
    fn parse(value: Value) -> Result<Self, DecodeError> {
        let mut object = Object::new(value, "project")?;
        let id = ProjectId::new(object.nonzero_u128("id")?);
        let name = object.string("name")?;
        let settings = parse_settings(object.take("settings")?)?;
        let inputs = parse_array(object.take("inputs")?, "inputs", parse_input)?;
        let dto = Self {
            id,
            name,
            settings,
            inputs,
            input_audio_strips: parse_array(
                object.take("input_audio_strips")?,
                "input_audio_strips",
                parse_input_audio_strip,
            )?,
            scenes: parse_array(object.take("scenes")?, "scenes", parse_scene)?,
            audio_buses: parse_array(object.take("audio_buses")?, "audio_buses", parse_bus)?,
            outputs: parse_array(object.take("outputs")?, "outputs", parse_output)?,
            stream_targets: parse_array(
                object.take("stream_targets")?,
                "stream_targets",
                parse_stream_target,
            )?,
            main_mix: parse_optional(object.take("main_mix")?, parse_main_mix)?,
            recording_desired: parse_recording_desired(object.take("recording_desired")?)?,
            stingers: parse_array(object.take("stingers")?, "stingers", parse_stinger)?,
            restart_policy: parse_restart_policy(object.take("restart_policy")?)?,
            edit_locks: parse_edit_locks(object.take("edit_locks")?)?,
            playlist: parse_playlist(object.take("playlist")?)?,
            cue_lists: super::cue::parse_cue_lists(&mut object)?,
            grading: super::grade::parse_grading(&mut object)?,
            animation_presets: super::animation_preset::parse_animation_presets(&mut object)?,
            transition_favorites: super::transition_favorite::parse_transition_favorites(
                &mut object,
            )?,
            input_lifecycle: super::input_lifecycle::parse_input_lifecycle(&mut object)?,
            audio_groups: super::audio_mixer::parse_audio_groups(&mut object)?,
            audio_solo_mode: super::audio_mixer::parse_audio_solo_mode(&mut object)?,
            audio_monitor_output: super::audio_mixer::parse_audio_monitor_output(&mut object)?,
            master_audio_dsp: super::strip_dsp::parse_strip_dsp(
                object.take("master_audio_dsp")?,
                "master_audio_dsp",
            )?,
            fade_to_black_excluded_outputs:
                super::audio_mixer::parse_fade_to_black_excluded_outputs(&mut object)?,
            fade_to_black_output_durations:
                super::audio_mixer::parse_fade_to_black_output_durations(&mut object)?,
            fade_to_black_curve: super::audio_mixer::parse_fade_to_black_curve(&mut object)?,
            display_outputs: super::display_output::parse_display_outputs(&mut object)?,
            live_lan: super::live_lan::parse_live_lan(&mut object)?,
            scripts: super::script::parse_scripts(&mut object)?,
            script_run: super::script::parse_script_run(&mut object)?,
            shortcuts: super::shortcut::parse_shortcuts(&mut object)?,
            media_lists: super::media_list::parse_media_lists(&mut object)?,
            title_timelines: super::title_timeline::parse_title_timelines(&mut object)?,
            screen_capture_restore_tokens:
                super::screen_capture_tokens::parse_screen_capture_restore_tokens(&mut object)?,
        };
        object.finish()?;
        for input in &dto.inputs {
            if dto
                .input_audio_strips
                .iter()
                .filter(|strip| strip.input == input.id)
                .count()
                != 1
            {
                return Err(syntax(
                    "input_audio_strips must contain exactly one strip for every input",
                ));
            }
        }
        if dto
            .input_audio_strips
            .iter()
            .any(|strip| !dto.inputs.iter().any(|input| input.id == strip.input))
        {
            return Err(syntax(
                "input_audio_strips must not reference an unknown input",
            ));
        }
        Ok(dto)
    }

    fn into_domain(self) -> Project {
        let mut project = Project::new(self.id, self.name, self.settings)
            .with_restart_policy(self.restart_policy);
        for input in self.inputs {
            project.add_input(input);
        }
        for strip in self.input_audio_strips {
            assert!(
                project.set_input_audio_strip(strip.input, strip.state),
                "validated input audio strip must reference an input"
            );
            project.set_input_audio_group(strip.input, strip.group);
            // An unusable label is reported by project validation.
            project.restore_input_audio_label(strip.input, strip.label);
            project
                .set_input_audio_dsp(strip.input, strip.dsp)
                .expect("a parsed strip DSP chain is validated");
        }
        project
            .set_master_audio_dsp(self.master_audio_dsp)
            .expect("a parsed Master DSP chain is validated");
        // Group names are reported by project validation.
        project.restore_audio_groups(self.audio_groups);
        project.set_audio_solo_mode(self.audio_solo_mode);
        // An unusable device name is reported by project validation.
        project.restore_audio_monitor_output(self.audio_monitor_output);
        for scene in self.scenes {
            project.add_scene(scene);
        }
        for bus in self.audio_buses {
            project.add_audio_bus(bus);
        }
        for output in self.outputs {
            project.add_output(output);
        }
        for target in self.stream_targets {
            project.add_stream_target(target);
        }
        if let Some(main_mix) = self.main_mix {
            project.set_main_mix(main_mix);
        }
        for (slot, active) in RecorderSlot::ALL.into_iter().zip(self.recording_desired) {
            project.set_recording_desired(slot, active);
        }
        for stinger in self.stingers {
            project.add_stinger(stinger);
        }
        project.set_project_locked(self.edit_locks.project);
        for input in self.edit_locks.inputs {
            // A lock naming an unknown input is reported by project
            // validation when the manifest is assembled.
            project.set_input_locked(input, true);
        }
        // Entry bounds and input references are reported by project
        // validation; whether the state is reachable is checked when an
        // engine restores it.
        project.set_playlist(self.playlist);
        // Cue list bounds, run states, and references are reported by
        // project validation.
        project.set_cue_lists(self.cue_lists);
        // Grade bounds, names, and library and LUT references are reported
        // by project validation.
        project.set_grading(self.grading);
        // Preset ids, names, and the library bound are reported by project
        // validation.
        project.set_animation_presets(self.animation_presets);
        project.set_transition_favorites(self.transition_favorites);
        // Category bounds and dangling input or category references are
        // reported by project validation.
        project.restore_input_categories(self.input_lifecycle.categories);
        for (input, lifecycle) in self.input_lifecycle.entries {
            project.restore_input_lifecycle(input, lifecycle);
        }
        // Unknown outputs are reported by project validation.
        for output in self.fade_to_black_excluded_outputs {
            project.set_fade_to_black_output(output, false);
        }
        // Unknown outputs and out-of-range durations are reported by project
        // validation.
        for (output, duration) in self.fade_to_black_output_durations {
            project.set_fade_to_black_output_duration(output, Some(duration));
        }
        project.set_fade_to_black_curve(self.fade_to_black_curve);
        // Unknown source outputs and displays both slots claim are reported
        // by project validation.
        for (slot, config) in self.display_outputs {
            project.restore_display_output(slot, config);
        }
        // An unknown source output and a viewer limit out of range are
        // reported by project validation.
        project.restore_live_lan(self.live_lan);
        // Script bounds and duplicate ids are reported by project
        // validation.
        project.set_scripts(self.scripts);
        // A run that does not fit the scripts is reported by project
        // validation.
        project.set_script_run(self.script_run);
        // Shortcut bounds, duplicate ids, keys, and OSC names are reported
        // by project validation.
        project.set_shortcuts(self.shortcuts);
        // A list that does not fit its input is reported by project
        // validation.
        project.set_media_lists(self.media_lists);
        // A timeline naming an input that is not a title is reported by
        // project validation.
        project.set_title_timelines(self.title_timelines);
        // A token of an unknown or other input, or an unusable token, is
        // reported by project validation.
        for (input, token) in self.screen_capture_restore_tokens {
            project.restore_screen_capture_restore_token(input, token);
        }
        project
    }
}

fn parse_stinger(value: Value) -> Result<StingerConfig, DecodeError> {
    let mut object = Object::new(value, "stinger")?;
    let slot_number = object.u8("slot")?;
    let slot = StingerSlotNumber::new(slot_number)
        .ok_or_else(|| syntax("stinger slot must be between 1 and 8"))?;
    let stinger = StingerConfig::new(
        slot,
        InputId::new(object.nonzero_u128("media_input")?),
        object.boolean("preload")?,
        object.u32("cut_point_frames")?,
        match object.string("audio_policy")?.as_str() {
            "muted" => StingerAudioPolicy::Muted,
            "stinger_only" => StingerAudioPolicy::StingerOnly,
            "mix_with_program" => StingerAudioPolicy::MixWithProgram,
            value => return Err(unknown_enum("stinger audio policy", value)),
        },
        match object.string("missing_media_fallback")?.as_str() {
            "cut" => StingerMissingMediaFallback::Cut,
            "fade" => StingerMissingMediaFallback::Fade,
            "keep_program" => StingerMissingMediaFallback::KeepProgram,
            value => return Err(unknown_enum("stinger missing-media fallback", value)),
        },
    );
    object.finish()?;
    Ok(stinger)
}

fn parse_input_audio_strip(value: Value) -> Result<InputAudioStrip, DecodeError> {
    let mut object = Object::new(value, "input audio strip")?;
    let input = InputId::new(object.nonzero_u128("input")?);
    let gain_value = object.i32("gain_milli_db")?;
    let gain = InputGainMilliDb::new(gain_value).ok_or_else(|| {
        syntax(format!(
            "input gain_milli_db must be between {} and {}",
            InputGainMilliDb::MIN,
            InputGainMilliDb::MAX
        ))
    })?;
    let strip = InputAudioStrip {
        input,
        state: InputAudioStripState {
            gain,
            balance: InputBalanceBasisPoints::new(object.i32("balance_basis_points")?).ok_or_else(
                || {
                    syntax(format!(
                        "input balance_basis_points must be between {} and {}",
                        InputBalanceBasisPoints::MIN,
                        InputBalanceBasisPoints::MAX
                    ))
                },
            )?,
            delay_samples: InputDelaySamples::new(object.u32("delay_samples")?).ok_or_else(
                || {
                    syntax(format!(
                        "input delay_samples must not exceed {}",
                        InputDelaySamples::MAX
                    ))
                },
            )?,
            pan: InputPanBasisPoints::new(object.i32("pan_basis_points")?).ok_or_else(|| {
                syntax(format!(
                    "input pan_basis_points must be between {} and {}",
                    InputPanBasisPoints::MIN,
                    InputPanBasisPoints::MAX
                ))
            })?,
            muted: object.boolean("muted")?,
            soloed: object.boolean("soloed")?,
            follow_video: object.boolean("follow_video")?,
        },
        label: object.string("label")?,
        group: parse_optional(object.take("group")?, |value| {
            let Value::Number(number) = value else {
                return Err(syntax("input audio strip group must be a number or null"));
            };
            u8::try_from(number)
                .ok()
                .and_then(fm_model::AudioGroupNumber::new)
                .ok_or_else(|| {
                    syntax(format!(
                        "input audio strip group must be 1 through {}",
                        fm_model::AUDIO_GROUP_COUNT
                    ))
                })
        })?,
        dsp: super::strip_dsp::parse_strip_dsp(object.take("dsp")?, "input audio strip dsp")?,
    };
    object.finish()?;
    Ok(strip)
}

fn parse_settings(value: Value) -> Result<ProjectSettings, DecodeError> {
    let mut object = Object::new(value, "settings")?;
    let settings = ProjectSettings {
        frame_rate: parse_frame_rate(object.take("frame_rate")?)?,
        video: parse_video(object.take("video")?)?,
        audio: parse_audio(object.take("audio")?)?,
    };
    object.finish()?;
    Ok(settings)
}

fn parse_frame_rate(value: Value) -> Result<FrameRate, DecodeError> {
    let mut object = Object::new(value, "frame_rate")?;
    let numerator = object.u32("numerator")?;
    let denominator = object.u32("denominator")?;
    object.finish()?;
    FrameRate::new(numerator, denominator).map_err(|error| syntax(error.to_string()))
}

fn parse_video(value: Value) -> Result<VideoFormat, DecodeError> {
    let mut object = Object::new(value, "video")?;
    let width = object.u32("width")?;
    let height = object.u32("height")?;
    let video = VideoFormat {
        dimensions: VideoDimensions::new(width, height)
            .ok_or_else(|| syntax("video dimensions must be nonzero"))?,
        frame_rate: parse_frame_rate(object.take("frame_rate")?)?,
        pixel_format: match object.string("pixel_format")?.as_str() {
            "rgba8" => PixelFormat::Rgba8,
            "bgra8" => PixelFormat::Bgra8,
            "rgba16_float" => PixelFormat::Rgba16Float,
            "nv12" => PixelFormat::Nv12,
            "p010" => PixelFormat::P010,
            "yuv422" => PixelFormat::Yuv422,
            value => return Err(unknown_enum("pixel_format", value)),
        },
        scan: match object.string("scan")?.as_str() {
            "progressive" => ScanMode::Progressive,
            "interlaced_top_field_first" => ScanMode::InterlacedTopFieldFirst,
            "interlaced_bottom_field_first" => ScanMode::InterlacedBottomFieldFirst,
            value => return Err(unknown_enum("scan", value)),
        },
        color: parse_color(object.take("color")?)?,
    };
    object.finish()?;
    Ok(video)
}

fn parse_color(value: Value) -> Result<ColorMetadata, DecodeError> {
    let mut object = Object::new(value, "color")?;
    let primaries = match object.string("primaries")?.as_str() {
        "bt601" => ColorPrimaries::Bt601,
        "bt709" => ColorPrimaries::Bt709,
        "bt2020" => ColorPrimaries::Bt2020,
        "display_p3" => ColorPrimaries::DisplayP3,
        value => return Err(unknown_enum("primaries", value)),
    };
    let transfer = match object.string("transfer")?.as_str() {
        "linear" => TransferFunction::Linear,
        "srgb" => TransferFunction::Srgb,
        "bt709" => TransferFunction::Bt709,
        "bt1886" => TransferFunction::Bt1886,
        "hlg" => TransferFunction::Hlg,
        "pq" => TransferFunction::Pq,
        value => return Err(unknown_enum("transfer", value)),
    };
    let matrix = match object.string("matrix")?.as_str() {
        "identity" => MatrixCoefficients::Identity,
        "bt601" => MatrixCoefficients::Bt601,
        "bt709" => MatrixCoefficients::Bt709,
        "bt2020_non_constant" => MatrixCoefficients::Bt2020NonConstant,
        value => return Err(unknown_enum("matrix", value)),
    };
    let range = match object.string("range")?.as_str() {
        "full" => SignalRange::Full,
        "limited" => SignalRange::Limited,
        value => return Err(unknown_enum("range", value)),
    };
    let chroma_location = match object.string("chroma_location")?.as_str() {
        "left" => ChromaLocation::Left,
        "center" => ChromaLocation::Center,
        "top_left" => ChromaLocation::TopLeft,
        value => return Err(unknown_enum("chroma_location", value)),
    };
    object.finish()?;
    Ok(ColorMetadata {
        primaries,
        transfer,
        matrix,
        range,
        chroma_location,
    })
}

fn parse_audio(value: Value) -> Result<AudioFormat, DecodeError> {
    let mut object = Object::new(value, "audio")?;
    let sample_rate = SampleRate::new(object.u32("sample_rate_hz")?)
        .ok_or_else(|| syntax("audio sample rate must be nonzero"))?;
    let sample_format = match object.string("sample_format")?.as_str() {
        "i16" => SampleFormat::I16,
        "i24" => SampleFormat::I24,
        "i32" => SampleFormat::I32,
        "f32" => SampleFormat::F32,
        "f64" => SampleFormat::F64,
        value => return Err(unknown_enum("sample_format", value)),
    };
    let channels = parse_array(object.take("channels")?, "channels", |value| {
        let Value::String(value) = value else {
            return Err(syntax("channel must be a string"));
        };
        match value.as_str() {
            "mono" => Ok(Channel::Mono),
            "left" => Ok(Channel::Left),
            "right" => Ok(Channel::Right),
            "center" => Ok(Channel::Center),
            "low_frequency" => Ok(Channel::LowFrequency),
            "left_surround" => Ok(Channel::LeftSurround),
            "right_surround" => Ok(Channel::RightSurround),
            "left_rear_surround" => Ok(Channel::LeftRearSurround),
            "right_rear_surround" => Ok(Channel::RightRearSurround),
            value => value
                .strip_prefix("discrete_")
                .filter(|number| {
                    !number.starts_with('0') && number.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|number| number.parse::<u8>().ok())
                .and_then(DiscreteChannel::new)
                .map(Channel::Discrete)
                .ok_or_else(|| unknown_enum("channel", value)),
        }
    })?;
    object.finish()?;
    Ok(AudioFormat {
        sample_rate,
        sample_format,
        channels: ChannelLayout::new(channels)
            .ok_or_else(|| syntax("audio channel layout must contain 1..=16 channels"))?,
    })
}

fn parse_input(value: Value) -> Result<Input, DecodeError> {
    let mut object = Object::new(value, "input")?;
    let input = Input {
        id: InputId::new(object.nonzero_u128("id")?),
        name: object.string("name")?,
        kind: parse_input_kind(object.take("kind")?)?,
        required_capabilities: parse_strings(
            object.take("required_capabilities")?,
            "required_capabilities",
        )?,
    };
    object.finish()?;
    Ok(input)
}

fn parse_input_kind(value: Value) -> Result<InputKind, DecodeError> {
    let mut object = Object::new(value, "input kind")?;
    let kind = match object.string("type")?.as_str() {
        "color" => InputKind::Color,
        "media" => InputKind::Media {
            asset_uri: object.string("asset_uri")?,
            playback: super::media_playback::parse_media_playback(object.take("playback")?)?,
        },
        "device" => super::capture::parse_device_input(&mut object)?,
        "network_stream" => {
            InputKind::NetworkStream(super::network_stream::parse_network_stream(&mut object)?)
        }
        "system_audio" => super::system_audio::parse_system_audio_input(&mut object)?,
        "image" => super::still::parse_image_input(&mut object)?,
        "slideshow" => super::still::parse_slideshow_input(&mut object)?,
        "document" => super::document::parse_document_input(&mut object)?,
        "screen_capture" => super::screen_capture::parse_screen_capture_input(&mut object)?,
        "scene" => InputKind::Scene {
            scene_id: SceneId::new(object.nonzero_u128("scene_id")?),
            audio_source: object.optional_input_id("audio_source")?,
        },
        "simulated" => InputKind::Simulated(SimulatedInput::new(
            parse_simulated_video(object.take("video")?)?,
            parse_simulated_audio(object.take("audio")?)?,
        )),
        "alias" => InputKind::Alias {
            target: InputId::new(object.nonzero_u128("target")?),
            crop: parse_optional(object.take("crop")?, parse_crop)?,
        },
        "mix_feed" => InputKind::MixFeed(match object.string("feed")?.as_str() {
            "program" => MixFeed::Program,
            "preview" => MixFeed::Preview,
            value => return Err(unknown_enum("mix feed", value)),
        }),
        "title" => InputKind::Title(TitleInput {
            template_uri: object.string("template_uri")?,
            field_revision: object.u64("field_revision")?,
            fields: parse_array(object.take("fields")?, "title fields", parse_title_field)?,
        }),
        value => return Err(unknown_enum("input kind", value)),
    };
    object.finish()?;
    Ok(kind)
}

fn parse_title_field(value: Value) -> Result<TitleFieldEntry, DecodeError> {
    let mut object = Object::new(value, "title field")?;
    let id = TitleFieldId::new(object.nonzero_u128("id")?);
    let value = match object.string("type")?.as_str() {
        "text" => TitleFieldValue::Text(object.string("value")?),
        "number" => TitleFieldValue::Number(object.i64("value")?),
        "color" => TitleFieldValue::Color(parse_hex_color(&object.string("value")?)?),
        "image" => TitleFieldValue::Image {
            asset_uri: object.string("asset_uri")?,
        },
        value => return Err(unknown_enum("title field type", value)),
    };
    let revision = object.u64("revision")?;
    object.finish()?;
    Ok(TitleFieldEntry {
        id,
        value,
        revision,
    })
}

/// Parses `#rrggbbaa` (hex digits in either case).
pub(crate) fn parse_hex_color(value: &str) -> Result<Rgba8, DecodeError> {
    let digits = value
        .strip_prefix('#')
        .filter(|digits| digits.len() == 8 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| syntax(format!("color `{value}` must be #rrggbbaa")))?;
    let channel = |index: usize| {
        u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| syntax(format!("color `{value}` must be #rrggbbaa")))
    };
    Ok(Rgba8::new(
        channel(0)?,
        channel(1)?,
        channel(2)?,
        channel(3)?,
    ))
}

fn parse_simulated_video(value: Value) -> Result<SimulatedVideo, DecodeError> {
    let mut object = Object::new(value, "simulated video")?;
    let video = match object.string("type")?.as_str() {
        "bars" => SimulatedVideo::Bars,
        "solid" => SimulatedVideo::Solid(SolidColor::new(
            object.u8("red")?,
            object.u8("green")?,
            object.u8("blue")?,
            object.u8("alpha")?,
        )),
        value => return Err(unknown_enum("simulated video", value)),
    };
    object.finish()?;
    Ok(video)
}

fn parse_simulated_audio(value: Value) -> Result<SimulatedAudio, DecodeError> {
    let mut object = Object::new(value, "simulated audio")?;
    let audio = match object.string("type")?.as_str() {
        "silence" => SimulatedAudio::Silence,
        "sine" => SimulatedAudio::Sine {
            frequency_hz: object.u32("frequency_hz")?,
        },
        value => return Err(unknown_enum("simulated audio", value)),
    };
    object.finish()?;
    Ok(audio)
}

fn parse_scene(value: Value) -> Result<Scene, DecodeError> {
    let mut object = Object::new(value, "scene")?;
    let id = SceneId::new(object.nonzero_u128("id")?);
    let name = object.string("name")?;
    let background = parse_rgba(object.take("background")?)?;
    let scene = Scene {
        id,
        name,
        background,
        layers: parse_array(object.take("layers")?, "layers", parse_layer)?,
        animation: super::scene_animation::parse_scene_animation(object.take("animation")?)?,
    };
    object.finish()?;
    Ok(scene)
}

fn parse_layer(value: Value) -> Result<Layer, DecodeError> {
    let mut object = Object::new(value, "layer")?;
    let id = fm_model::LayerId::new(object.u32("id")?)
        .ok_or_else(|| syntax("layer id must be nonzero"))?;
    let name = object.string("name")?;
    let source = parse_source(object.take("source")?)?;
    let enabled = object.boolean("enabled")?;
    let geometry = parse_geometry(object.take("geometry")?)?;
    let crop = parse_optional(object.take("crop")?, parse_crop)?;
    let mask = parse_optional(object.take("mask")?, parse_mask)?;
    let key = parse_optional(object.take("key")?, parse_layer_key)?;
    let effects = parse_layer_effects(object.take("effects")?)?;
    let opacity = object.u8("opacity")?;
    let z_order = object.i32("z_order")?;
    let layer = Layer {
        id,
        name,
        source,
        enabled,
        geometry,
        crop,
        mask,
        key,
        effects,
        opacity,
        z_order,
    };
    object.finish()?;
    Ok(layer)
}

/// Parses one layer's key (schema 41, IN-020; key/fill and refinements
/// schema 61, SW-008): `{"type": "chroma", "red", "green", "blue",
/// "similarity", "smoothness", "spill", "refinement"}`, `{"type": "luma",
/// "low", "high", "softness", "invert", "refinement"}`, or `{"type":
/// "fill", "key_input", "channel": "luma"|"alpha", "low", "high",
/// "softness", "invert", "refinement"}`, with `low < high` and the
/// refinement in bounds.
fn parse_layer_key(value: Value) -> Result<fm_model::LayerKey, DecodeError> {
    let mut object = Object::new(value, "layer key")?;
    let key = match object.string("type")?.as_str() {
        "chroma" => fm_model::LayerKey::Chroma(fm_model::ChromaLayerKey {
            red: object.u8("red")?,
            green: object.u8("green")?,
            blue: object.u8("blue")?,
            similarity: object.u8("similarity")?,
            smoothness: object.u8("smoothness")?,
            spill: object.u8("spill")?,
            refinement: parse_key_refinement(object.take("refinement")?)?,
        }),
        "luma" => fm_model::LayerKey::Luma(fm_model::LumaLayerKey {
            low: object.u8("low")?,
            high: object.u8("high")?,
            softness: object.u8("softness")?,
            invert: object.boolean("invert")?,
            refinement: parse_key_refinement(object.take("refinement")?)?,
        }),
        "fill" => fm_model::LayerKey::Fill(fm_model::FillLayerKey {
            key_input: InputId::new(object.nonzero_u128("key_input")?),
            channel: match object.string("channel")?.as_str() {
                "luma" => fm_model::KeyChannel::Luma,
                "alpha" => fm_model::KeyChannel::Alpha,
                value => return Err(unknown_enum("key channel", value)),
            },
            low: object.u8("low")?,
            high: object.u8("high")?,
            softness: object.u8("softness")?,
            invert: object.boolean("invert")?,
            refinement: parse_key_refinement(object.take("refinement")?)?,
        }),
        value => return Err(unknown_enum("layer key", value)),
    };
    object.finish()?;
    key.validate()
        .map_err(|error| syntax(format!("layer key is invalid: {error}")))?;
    Ok(key)
}

/// Parses a key refinement (schema 61, SW-008): `{"clip_black",
/// "clip_white", "choke", "edge_softness", "despill", "light_wrap",
/// "light_wrap_width"}` where `despill` is `{"type": "off"}` or
/// `{"type": "green"|"blue", "strength"}` (light wrap since schema 79).
fn parse_key_refinement(value: Value) -> Result<fm_model::KeyRefinement, DecodeError> {
    let mut object = Object::new(value, "key refinement")?;
    let clip_black = object.u8("clip_black")?;
    let clip_white = object.u8("clip_white")?;
    let choke = object.i32("choke")?;
    let choke =
        i8::try_from(choke).map_err(|_| syntax(format!("key choke {choke} is out of range")))?;
    let edge_softness = object.u8("edge_softness")?;
    let mut despill = Object::new(object.take("despill")?, "key despill")?;
    let despill_value = match despill.string("type")?.as_str() {
        "off" => fm_model::KeyDespill::Off,
        "green" => fm_model::KeyDespill::Green {
            strength: despill.u8("strength")?,
        },
        "blue" => fm_model::KeyDespill::Blue {
            strength: despill.u8("strength")?,
        },
        value => return Err(unknown_enum("key despill", value)),
    };
    despill.finish()?;
    let light_wrap = object.u8("light_wrap")?;
    let light_wrap_width = object.u8("light_wrap_width")?;
    object.finish()?;
    Ok(fm_model::KeyRefinement {
        clip_black,
        clip_white,
        choke,
        edge_softness,
        despill: despill_value,
        light_wrap,
        light_wrap_width,
    })
}

/// Parses one layer's ordered effect array.
///
/// `effects` is part of the current contract: the field is required, must be
/// an array of `{"name": <string>, "parameters": [<i32>, ...]}` objects in
/// layer order, at most [`LayerEffect::MAX_EFFECTS_PER_LAYER`], each one of
/// the [`LayerEffect`] kinds with its exact, in-range parameters.
fn parse_layer_effects(value: Value) -> Result<Vec<LayerEffect>, DecodeError> {
    let effects = parse_array(value, "layer effects", parse_layer_effect)?;
    if effects.len() > LayerEffect::MAX_EFFECTS_PER_LAYER {
        return Err(syntax(format!(
            "layer must not carry more than {} effects",
            LayerEffect::MAX_EFFECTS_PER_LAYER
        )));
    }
    Ok(effects)
}

fn parse_layer_effect(value: Value) -> Result<LayerEffect, DecodeError> {
    let mut object = Object::new(value, "layer effect")?;
    let name = object.string("name")?;
    let parameters = parse_array(object.take("parameters")?, "effect parameters", |value| {
        parse_i32(&value, "effect parameter")
    })?;
    object.finish()?;
    let effect = LayerEffect::new(name, parameters);
    effect
        .validate()
        .map_err(|error| syntax(format!("layer effect is invalid: {error}")))?;
    Ok(effect)
}

fn parse_rgba(value: Value) -> Result<Rgba8, DecodeError> {
    let mut object = Object::new(value, "RGBA8 color")?;
    let color = Rgba8::new(
        object.u8("red")?,
        object.u8("green")?,
        object.u8("blue")?,
        object.u8("alpha")?,
    );
    object.finish()?;
    Ok(color)
}

fn parse_geometry(value: Value) -> Result<LayerGeometry, DecodeError> {
    let mut object = Object::new(value, "layer geometry")?;
    let geometry = LayerGeometry::new(
        object.i32("translation_x")?,
        object.i32("translation_y")?,
        object.u32("width")?,
        object.u32("height")?,
        match object.string("rotation")?.as_str() {
            "deg0" => Rotation::Deg0,
            "deg90" => Rotation::Deg90,
            "deg180" => Rotation::Deg180,
            "deg270" => Rotation::Deg270,
            value => return Err(unknown_enum("rotation", value)),
        },
    )
    .with_tilt(object.i32("tilt_millidegrees")?)
    .with_anchor(LayerAnchor::new(
        object.u16("anchor_x")?,
        object.u16("anchor_y")?,
    ));
    object.finish()?;
    Ok(geometry)
}

fn parse_crop(value: Value) -> Result<CropRect, DecodeError> {
    let mut object = Object::new(value, "crop")?;
    let crop = CropRect::new(
        object.u32("x")?,
        object.u32("y")?,
        object.u32("width")?,
        object.u32("height")?,
    );
    object.finish()?;
    Ok(crop)
}

fn parse_mask(value: Value) -> Result<RectMask, DecodeError> {
    let mut object = Object::new(value, "rectangular mask")?;
    let mask = RectMask::new(
        object.u32("x")?,
        object.u32("y")?,
        object.u32("width")?,
        object.u32("height")?,
    )
    .inverted(object.boolean("invert")?);
    object.finish()?;
    Ok(mask)
}

fn parse_source(value: Value) -> Result<SourceRef, DecodeError> {
    let mut object = Object::new(value, "source")?;
    let source = match object.string("type")?.as_str() {
        "input" => SourceRef::Input(InputId::new(object.nonzero_u128("id")?)),
        "scene" => SourceRef::Scene(SceneId::new(object.nonzero_u128("id")?)),
        value => return Err(unknown_enum("source", value)),
    };
    object.finish()?;
    Ok(source)
}

fn parse_bus(value: Value) -> Result<AudioBus, DecodeError> {
    let mut object = Object::new(value, "audio bus")?;
    let bus = AudioBus {
        id: BusId::new(object.nonzero_u128("id")?),
        name: object.string("name")?,
        sends: parse_array(object.take("sends")?, "sends", |value| {
            let mut send = Object::new(value, "bus send")?;
            let destination = BusId::new(send.nonzero_u128("destination")?);
            send.finish()?;
            Ok(BusSend { destination })
        })?,
    };
    object.finish()?;
    Ok(bus)
}

fn parse_output(value: Value) -> Result<Output, DecodeError> {
    let mut object = Object::new(value, "output")?;
    let output = Output {
        id: OutputId::new(object.nonzero_u128("id")?),
        name: object.string("name")?,
        video_source: SceneId::new(object.nonzero_u128("video_source")?),
        audio_source: BusId::new(object.nonzero_u128("audio_source")?),
        startup: match object.string("startup")?.as_str() {
            "stopped" => StartupPolicy::Stopped,
            "reconcile_desired_state" => StartupPolicy::ReconcileDesiredState,
            value => return Err(unknown_enum("startup", value)),
        },
        required_capabilities: parse_strings(
            object.take("required_capabilities")?,
            "required_capabilities",
        )?,
    };
    object.finish()?;
    Ok(output)
}

/// Parses one streaming destination.
///
/// Every rejection message below is built from a typed model error that
/// carries no endpoint or key text, so a manifest with a bad stream key is
/// refused without the key appearing in the failure. `key` is a string for
/// RTMP/RTMPS/SRT and `null` for HLS; [constructor](StreamTarget::new) refuses
/// either pairing reversed. `ladder` is `null` for a single-rendition
/// destination and an array of rungs for an HLS ladder; the
/// [builder](StreamTarget::with_ladder) refuses one on any other protocol.
/// `srt_latency_ms` is a number exactly for SRT and `srt_passphrase` an
/// optional SRT-only secret (schema 51).
fn parse_stream_target(value: Value) -> Result<StreamTarget, DecodeError> {
    let mut object = Object::new(value, "stream target")?;
    let id = StreamTargetId::new(object.nonzero_u128("id")?);
    let name = object.string("name")?;
    let protocol = match object.string("protocol")?.as_str() {
        "rtmp" => StreamProtocol::Rtmp,
        "rtmps" => StreamProtocol::Rtmps,
        "srt" => StreamProtocol::Srt,
        "hls" => StreamProtocol::Hls,
        value => return Err(unknown_enum("stream target protocol", value)),
    };
    let endpoint = StreamEndpoint::parse_for(protocol, &object.string("endpoint")?)
        .map_err(|error| syntax(format!("field `endpoint` is invalid: {error}")))?;
    let backup_endpoint = object
        .optional_string("backup_endpoint")?
        .map(|text| {
            StreamEndpoint::parse_for(protocol, &text)
                .map_err(|error| syntax(format!("field `backup_endpoint` is invalid: {error}")))
        })
        .transpose()?;
    let key = object
        .optional_string("key")?
        .map(|secret| {
            StreamKey::parse(&secret)
                .map_err(|error| syntax(format!("field `key` is invalid: {error}")))
        })
        .transpose()?;
    let startup = match object.string("startup")?.as_str() {
        "stopped" => StartupPolicy::Stopped,
        "reconcile_desired_state" => StartupPolicy::ReconcileDesiredState,
        value => return Err(unknown_enum("startup", value)),
    };
    let output = OutputId::new(object.nonzero_u128("output")?);
    let running = object.boolean("running")?;
    let video_bitrate_kbps = object.u32("video_bitrate_kbps")?;
    let ladder = parse_optional(object.take("ladder")?, parse_stream_ladder)?;
    let reconnect = parse_stream_reconnect(object.take("reconnect")?)?;
    let srt_latency_ms = parse_optional(object.take("srt_latency_ms")?, |value| {
        let Value::Number(value) = value else {
            return Err(syntax(
                "field `srt_latency_ms` must be an unsigned integer or null".to_owned(),
            ));
        };
        u32::try_from(value).map_err(|_| syntax("field `srt_latency_ms` exceeds u32".to_owned()))
    })?;
    let srt_passphrase = object
        .optional_string("srt_passphrase")?
        .map(|secret| {
            SrtPassphrase::parse(&secret)
                .map_err(|error| syntax(format!("field `srt_passphrase` is invalid: {error}")))
        })
        .transpose()?;
    object.finish()?;
    // SRT carries its latency exactly, and nothing else carries one.
    if srt_latency_ms.is_some() != (protocol == StreamProtocol::Srt) {
        return Err(syntax(
            "field `srt_latency_ms` must be a number for SRT and null otherwise".to_owned(),
        ));
    }
    let target = StreamTarget::new(id, name, protocol, endpoint, key, output)
        .and_then(|target| target.with_backup_endpoint(backup_endpoint))
        .map_err(|error| syntax(format!("stream target is invalid: {error}")))?;
    let target = target
        .with_video_bitrate(video_bitrate_kbps)
        .map_err(|error| syntax(format!("stream target video bitrate is invalid: {error}")))?;
    let target = target
        .with_ladder(ladder)
        .and_then(|target| match srt_latency_ms {
            Some(latency) => target.with_srt_latency_ms(latency),
            None => Ok(target),
        })
        .and_then(|target| target.with_srt_passphrase(srt_passphrase))
        .map_err(|error| syntax(format!("stream target is invalid: {error}")))?;
    Ok(target
        .with_startup(startup)
        .with_reconnect(reconnect)
        .set_running(running))
}

/// Parses one destination's authored reconnect policy (schema 49):
/// `{"initial_delay_ms": u32, "max_delay_ms": u32, "max_attempts": u32|null}`.
fn parse_stream_reconnect(value: Value) -> Result<StreamReconnectPolicy, DecodeError> {
    let mut object = Object::new(value, "stream reconnect policy")?;
    let initial_delay_ms = object.u32("initial_delay_ms")?;
    let max_delay_ms = object.u32("max_delay_ms")?;
    let max_attempts = parse_optional(object.take("max_attempts")?, |value| {
        let Value::Number(value) = value else {
            return Err(syntax(
                "field `max_attempts` must be an unsigned integer or null".to_owned(),
            ));
        };
        u32::try_from(value).map_err(|_| syntax("field `max_attempts` exceeds u32".to_owned()))
    })?;
    object.finish()?;
    StreamReconnectPolicy::new(initial_delay_ms, max_delay_ms, max_attempts)
        .map_err(|error| syntax(format!("field `reconnect` is invalid: {error}")))
}

/// Parses one authored ladder: an array of rung objects from lowest to
/// highest.
fn parse_stream_ladder(value: Value) -> Result<StreamLadder, DecodeError> {
    let rungs = parse_array(value, "stream ladder", parse_stream_ladder_rung)?;
    StreamLadder::new(rungs).map_err(|error| syntax(format!("field `ladder` is invalid: {error}")))
}

fn parse_stream_ladder_rung(value: Value) -> Result<StreamLadderRung, DecodeError> {
    let mut object = Object::new(value, "stream ladder rung")?;
    let rung = StreamLadderRung::new(
        object.u32("video_bitrate_kbps")?,
        object.u32("width")?,
        object.u32("height")?,
    )
    .map_err(|error| syntax(format!("field `ladder` is invalid: {error}")))?;
    object.finish()?;
    Ok(rung)
}

fn parse_main_mix(value: Value) -> Result<MainMix, DecodeError> {
    let mut object = Object::new(value, "main mix")?;
    let mix = MainMix::new(
        InputId::new(object.nonzero_u128("desired_program")?),
        InputId::new(object.nonzero_u128("desired_preview")?),
    );
    object.finish()?;
    Ok(mix)
}

/// Parses the desired recording flags for every recorder slot.
///
/// The field is an array of exactly [`RECORDER_SLOT_COUNT`] booleans in slot
/// order: a missing field, a non-array, any other length, and non-boolean
/// entries are all malformed.
fn parse_recording_desired(value: Value) -> Result<[bool; RECORDER_SLOT_COUNT], DecodeError> {
    let desired = parse_array(value, "recording_desired", |value| {
        let Value::Bool(value) = value else {
            return Err(syntax("recording_desired entries must be booleans"));
        };
        Ok(value)
    })?;
    desired.try_into().map_err(|desired: Vec<bool>| {
        syntax(format!(
            "expected {RECORDER_SLOT_COUNT} recording_desired entries, found {}",
            desired.len()
        ))
    })
}

/// Parses the persisted edit padlock: the project-wide flag and the locked
/// input identifiers as a strictly ascending (canonical, duplicate-free)
/// array of nonzero integers. Whether each input exists is checked by
/// project validation.
fn parse_edit_locks(value: Value) -> Result<EditLocks, DecodeError> {
    let mut object = Object::new(value, "edit locks")?;
    let project = object.boolean("project")?;
    let inputs = parse_array(object.take("inputs")?, "edit_locks.inputs", |value| {
        nonzero_u128(&value, "edit_locks.inputs entry").map(InputId::new)
    })?;
    object.finish()?;
    if inputs.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(syntax("edit_locks.inputs must be strictly ascending"));
    }
    Ok(EditLocks { project, inputs })
}

/// Parses the persisted playlist: loop mode, entries, automation state, the
/// next take number, and the last observed frame.
fn parse_playlist(value: Value) -> Result<Playlist, DecodeError> {
    let mut object = Object::new(value, "playlist")?;
    let loop_mode = match object.string("loop")?.as_str() {
        "off" => PlaylistLoop::Off,
        "all" => PlaylistLoop::All,
        "one" => PlaylistLoop::One,
        value => return Err(unknown_enum("playlist loop", value)),
    };
    let entries = parse_array(
        object.take("entries")?,
        "playlist.entries",
        parse_playlist_entry,
    )?;
    let state = parse_playlist_state(object.take("state")?)?;
    let next_take = object.u64("next_take")?;
    let observed_frame = object.u64("observed_frame")?;
    object.finish()?;
    Ok(Playlist {
        loop_mode,
        entries,
        state,
        next_take,
        observed_frame,
    })
}

fn parse_playlist_entry(value: Value) -> Result<PlaylistEntry, DecodeError> {
    let mut object = Object::new(value, "playlist entry")?;
    let id = object.u64("id")?;
    let input = InputId::new(object.nonzero_u128("input")?);
    let duration_frames = object.u64("duration_frames")?;
    let mut transition = Object::new(object.take("transition")?, "playlist transition")?;
    let kind = match transition.string("kind")?.as_str() {
        "cut" => PlaylistTransitionKind::Cut,
        "fade" => PlaylistTransitionKind::Fade,
        "alpha_fade" => PlaylistTransitionKind::AlphaFade,
        "wipe" => PlaylistTransitionKind::Wipe,
        "slide" => PlaylistTransitionKind::Slide,
        "zoom" => PlaylistTransitionKind::Zoom,
        value => return Err(unknown_enum("playlist transition kind", value)),
    };
    let transition_frames = transition.u32("duration_frames")?;
    transition.finish()?;
    let enabled = object.boolean("enabled")?;
    object.finish()?;
    Ok(PlaylistEntry {
        id,
        input,
        duration_frames,
        transition: PlaylistTransition {
            kind,
            duration_frames: transition_frames,
        },
        enabled,
    })
}

fn parse_playlist_state(value: Value) -> Result<PlaylistRunState, DecodeError> {
    let mut object = Object::new(value, "playlist state")?;
    let state = match object.string("type")?.as_str() {
        "stopped" => PlaylistRunState::Stopped,
        "armed" => PlaylistRunState::Armed {
            start_frame: object.u64("start_frame")?,
            from: parse_optional(object.take("from")?, |value| {
                entry_id(&value, "playlist state from")
            })?,
        },
        "playing" => PlaylistRunState::Playing {
            entry: object.u64("entry")?,
            take: object.u64("take")?,
            cycle: object.u64("cycle")?,
            started_frame: object.u64("started_frame")?,
        },
        "paused" => PlaylistRunState::Paused {
            entry: object.u64("entry")?,
            take: object.u64("take")?,
            cycle: object.u64("cycle")?,
            elapsed_frames: object.u64("elapsed_frames")?,
        },
        "finished" => PlaylistRunState::Finished {
            last: parse_optional(object.take("last")?, |value| {
                entry_id(&value, "playlist state last")
            })?,
        },
        value => return Err(unknown_enum("playlist state", value)),
    };
    object.finish()?;
    Ok(state)
}

fn entry_id(value: &Value, context: &str) -> Result<u64, DecodeError> {
    let Value::Number(value) = value else {
        return Err(syntax(format!(
            "{context} must be an unsigned integer or null"
        )));
    };
    u64::try_from(*value).map_err(|_| syntax(format!("{context} exceeds u64")))
}

fn parse_restart_policy(value: Value) -> Result<RestartPolicy, DecodeError> {
    let mut object = Object::new(value, "restart policy")?;
    let policy = match object.string("type")?.as_str() {
        "never" => RestartPolicy::Never,
        "always" => RestartPolicy::Always,
        "on_failure" => RestartPolicy::OnFailure {
            max_attempts: object.u8("max_attempts")?,
        },
        value => return Err(unknown_enum("restart policy", value)),
    };
    object.finish()?;
    Ok(policy)
}

type ParsedRuntime = (
    RuntimeRouting,
    RuntimeManualTransitions,
    RuntimeFadeToBlack,
    RuntimeOverlays,
    ProjectPosition,
    Vec<IdempotencyReceipt>,
);

fn parse_runtime(value: Value) -> Result<ParsedRuntime, DecodeError> {
    let mut object = Object::new(value, "runtime")?;
    let routing = parse_routing(object.take("routing")?)?;
    let manual_transitions = parse_manual_transitions(object.take("manual_transitions")?)?;
    let fade_to_black = parse_fade_to_black(object.take("fade_to_black")?)?;
    let overlays = parse_overlays(object.take("overlays")?)?;
    let position = parse_position(object.take("position")?)?;
    let receipts = parse_array(
        object.take("idempotency_receipts")?,
        "idempotency_receipts",
        parse_receipt,
    )?;
    object.finish()?;
    Ok((
        routing,
        manual_transitions,
        fade_to_black,
        overlays,
        position,
        receipts,
    ))
}

fn parse_overlays(value: Value) -> Result<RuntimeOverlays, DecodeError> {
    let mut object = Object::new(value, "overlays")?;
    let desired = parse_overlay_channels(object.take("desired")?)?;
    let realized = parse_overlay_channels(object.take("realized")?)?;
    object.finish()?;
    Ok(RuntimeOverlays { desired, realized })
}

fn parse_overlay_channels(value: Value) -> Result<[RuntimeOverlayChannel; 8], DecodeError> {
    let channels = parse_array(value, "overlay channels", |value| {
        let mut object = Object::new(value, "overlay channel")?;
        let source = object.optional_input_id("source")?;
        let active = object.boolean("active")?;
        let transition = object.string("transition")?;
        let transition = RuntimeOverlayTransition::from_token(&transition)
            .ok_or_else(|| syntax(format!("unknown overlay transition `{transition}`")))?;
        let duration_frames = object.u32("duration_frames")?;
        let position = match object.string("position")?.as_str() {
            "full_frame" => RuntimeOverlayPosition::FullFrame,
            "top_left" => RuntimeOverlayPosition::TopLeft,
            "top_right" => RuntimeOverlayPosition::TopRight,
            "bottom_left" => RuntimeOverlayPosition::BottomLeft,
            "bottom_right" => RuntimeOverlayPosition::BottomRight,
            value => return Err(syntax(format!("unknown overlay position `{value}`"))),
        };
        let border = match object.string("border")?.as_str() {
            "none" => RuntimeOverlayBorder::None,
            "thin_white" => RuntimeOverlayBorder::ThinWhite,
            "thick_white" => RuntimeOverlayBorder::ThickWhite,
            value => return Err(syntax(format!("unknown overlay border `{value}`"))),
        };
        let queued_sources = parse_array(
            object.take("queued_sources")?,
            "overlay queued sources",
            |value| Ok(InputId::new(nonzero_u128(&value, "input ID")?)),
        )?;
        let included_outputs = parse_array(
            object.take("included_outputs")?,
            "overlay included outputs",
            |value| Ok(OutputId::new(nonzero_u128(&value, "output ID")?)),
        )?;
        object.finish()?;
        Ok(RuntimeOverlayChannel {
            source,
            active,
            transition,
            duration_frames,
            position,
            border,
            queued_sources,
            included_outputs,
        })
    })?;
    channels.try_into().map_err(|channels: Vec<_>| {
        syntax(format!(
            "expected 8 overlay channels, found {}",
            channels.len()
        ))
    })
}

fn parse_fade_to_black(value: Value) -> Result<RuntimeFadeToBlack, DecodeError> {
    let mut object = Object::new(value, "fade to black")?;
    let state = RuntimeFadeToBlack {
        desired: parse_fade_to_black_state(object.take("desired")?)?,
        realized: parse_fade_to_black_state(object.take("realized")?)?,
    };
    object.finish()?;
    Ok(state)
}

fn parse_fade_to_black_state(value: Value) -> Result<FadeToBlackState, DecodeError> {
    let mut object = Object::new(value, "fade to black state")?;
    let state = FadeToBlackState::new(
        object.boolean("target_active")?,
        object.u16("position_numerator")?,
    );
    object.finish()?;
    Ok(state)
}

fn parse_manual_transitions(value: Value) -> Result<RuntimeManualTransitions, DecodeError> {
    let mut object = Object::new(value, "manual transitions")?;
    let transitions = RuntimeManualTransitions {
        desired: parse_optional(object.take("desired")?, parse_manual_transition)?,
        realized: parse_optional(object.take("realized")?, parse_manual_transition)?,
    };
    object.finish()?;
    Ok(transitions)
}

fn parse_manual_transition(value: Value) -> Result<ManualTransitionState, DecodeError> {
    let mut object = Object::new(value, "manual transition")?;
    let token = object.string("kind")?;
    let clip_frames = object.u32("clip_frames")?;
    let kind = ManualTransitionKind::from_token(&token, clip_frames)
        .ok_or_else(|| unknown_enum("manual transition kind", &token))?;
    let from_id = InputId::new(object.nonzero_u128("from_id")?);
    let to_id = InputId::new(object.nonzero_u128("to_id")?);
    let interval_start_basis_points = object.u16("interval_start_basis_points")?;
    let position_basis_points = object.u16("position_basis_points")?;
    object.finish()?;
    ManualTransitionState::new(
        kind,
        from_id,
        to_id,
        interval_start_basis_points,
        position_basis_points,
    )
    .ok_or_else(|| syntax("manual transition position must not exceed 10000 basis points"))
}

fn parse_routing(value: Value) -> Result<RuntimeRouting, DecodeError> {
    let mut object = Object::new(value, "routing")?;
    let routing = RuntimeRouting {
        desired_program_id: object.optional_input_id("desired_program_id")?,
        realized_program_id: object.optional_input_id("realized_program_id")?,
        desired_preview_id: object.optional_input_id("desired_preview_id")?,
        realized_preview_id: object.optional_input_id("realized_preview_id")?,
    };
    object.finish()?;
    Ok(routing)
}

fn parse_position(value: Value) -> Result<ProjectPosition, DecodeError> {
    let mut object = Object::new(value, "position")?;
    let position = ProjectPosition {
        revision: object.u64("revision")?,
        state_epoch: object.u64("state_epoch")?,
        event_sequence: object.u64("event_sequence")?,
        frames_rendered: object.u64("frames_rendered")?,
        runtime_generation: object.u64("runtime_generation")?,
        clock_time_nanos: object.u64("clock_time_nanos")?,
    };
    object.finish()?;
    Ok(position)
}

fn parse_receipt(value: Value) -> Result<IdempotencyReceipt, DecodeError> {
    let mut object = Object::new(value, "receipt")?;
    let key = object.string("key")?;
    let command_id = object.string("command_id")?;
    let receipt = match object.string("outcome")?.as_str() {
        "accepted" => IdempotencyReceipt::accepted(
            key,
            command_id,
            object.u64("revision")?,
            object.u64("target_frame")?,
        ),
        "rejected" => IdempotencyReceipt::rejected(
            key,
            command_id,
            object.u64("current_revision")?,
            object.string("code")?,
            object.string("message")?,
            object.boolean("retryable")?,
        ),
        value => return Err(unknown_enum("receipt outcome", value)),
    };
    object.finish()?;
    Ok(receipt)
}

pub(crate) struct Object {
    context: &'static str,
    values: BTreeMap<String, Value>,
}

impl Object {
    pub(crate) fn new(value: Value, context: &'static str) -> Result<Self, DecodeError> {
        let Value::Object(values) = value else {
            return Err(syntax(format!("{context} must be an object")));
        };
        Ok(Self { context, values })
    }

    pub(crate) fn take(&mut self, field: &str) -> Result<Value, DecodeError> {
        self.values
            .remove(field)
            .ok_or_else(|| syntax(format!("missing field `{field}` in {}", self.context)))
    }

    /// Removes `field` when present; `None` when the key is absent.
    pub(crate) fn take_optional(&mut self, field: &str) -> Option<Value> {
        self.values.remove(field)
    }

    pub(crate) fn string(&mut self, field: &str) -> Result<String, DecodeError> {
        let Value::String(value) = self.take(field)? else {
            return Err(syntax(format!("field `{field}` must be a string")));
        };
        Ok(value)
    }

    pub(crate) fn boolean(&mut self, field: &str) -> Result<bool, DecodeError> {
        let Value::Bool(value) = self.take(field)? else {
            return Err(syntax(format!("field `{field}` must be a boolean")));
        };
        Ok(value)
    }

    fn number(&mut self, field: &str) -> Result<u128, DecodeError> {
        let Value::Number(value) = self.take(field)? else {
            return Err(syntax(format!(
                "field `{field}` must be an unsigned integer"
            )));
        };
        Ok(value)
    }

    pub(crate) fn u8(&mut self, field: &str) -> Result<u8, DecodeError> {
        u8::try_from(self.number(field)?).map_err(|_| syntax(format!("field `{field}` exceeds u8")))
    }

    pub(crate) fn u32(&mut self, field: &str) -> Result<u32, DecodeError> {
        u32::try_from(self.number(field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u32")))
    }

    pub(crate) fn u16(&mut self, field: &str) -> Result<u16, DecodeError> {
        u16::try_from(self.number(field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u16")))
    }

    pub(crate) fn i32(&mut self, field: &str) -> Result<i32, DecodeError> {
        match self.take(field)? {
            Value::Number(value) => {
                i32::try_from(value).map_err(|_| syntax(format!("field `{field}` exceeds i32")))
            }
            Value::NegativeNumber(value) => {
                let magnitude = i128::try_from(value)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i32")))?;
                i32::try_from(-magnitude)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i32")))
            }
            _ => Err(syntax(format!("field `{field}` must be an integer"))),
        }
    }

    pub(crate) fn i64(&mut self, field: &str) -> Result<i64, DecodeError> {
        match self.take(field)? {
            Value::Number(value) => {
                i64::try_from(value).map_err(|_| syntax(format!("field `{field}` exceeds i64")))
            }
            Value::NegativeNumber(value) => {
                let magnitude = i128::try_from(value)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i64")))?;
                i64::try_from(-magnitude)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i64")))
            }
            _ => Err(syntax(format!("field `{field}` must be an integer"))),
        }
    }

    pub(crate) fn u64(&mut self, field: &str) -> Result<u64, DecodeError> {
        u64::try_from(self.number(field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u64")))
    }

    pub(crate) fn nonzero_u128(&mut self, field: &str) -> Result<NonZeroU128, DecodeError> {
        NonZeroU128::new(self.number(field)?)
            .ok_or_else(|| syntax(format!("field `{field}` must be nonzero")))
    }

    fn optional_string(&mut self, field: &str) -> Result<Option<String>, DecodeError> {
        match self.take(field)? {
            Value::Null => Ok(None),
            Value::String(value) => Ok(Some(value)),
            _ => Err(syntax(format!("field `{field}` must be a string or null"))),
        }
    }

    fn optional_input_id(&mut self, field: &str) -> Result<Option<InputId>, DecodeError> {
        match self.take(field)? {
            Value::Null => Ok(None),
            Value::Number(value) => NonZeroU128::new(value)
                .map(InputId::new)
                .map(Some)
                .ok_or_else(|| syntax(format!("field `{field}` must be nonzero or null"))),
            _ => Err(syntax(format!(
                "field `{field}` must be an unsigned integer or null"
            ))),
        }
    }

    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        if let Some(field) = self.values.keys().next() {
            Err(syntax(format!(
                "unknown field `{field}` in {}",
                self.context
            )))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn parse_array<T>(
    value: Value,
    context: &'static str,
    parse: impl FnMut(Value) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let Value::Array(values) = value else {
        return Err(syntax(format!("{context} must be an array")));
    };
    values.into_iter().map(parse).collect()
}

fn parse_i32(value: &Value, context: &str) -> Result<i32, DecodeError> {
    match value {
        Value::Number(value) => {
            i32::try_from(*value).map_err(|_| syntax(format!("{context} exceeds i32")))
        }
        Value::NegativeNumber(value) => {
            let magnitude =
                i128::try_from(*value).map_err(|_| syntax(format!("{context} exceeds i32")))?;
            i32::try_from(-magnitude).map_err(|_| syntax(format!("{context} exceeds i32")))
        }
        _ => Err(syntax(format!("{context} must be an integer"))),
    }
}

fn parse_strings(value: Value, context: &'static str) -> Result<Vec<String>, DecodeError> {
    parse_array(value, context, |value| {
        let Value::String(value) = value else {
            return Err(syntax(format!("{context} entries must be strings")));
        };
        Ok(value)
    })
}

pub(crate) fn parse_optional<T>(
    value: Value,
    parse: impl FnOnce(Value) -> Result<T, DecodeError>,
) -> Result<Option<T>, DecodeError> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        parse(value).map(Some)
    }
}

fn nonzero_u128(value: &Value, context: &str) -> Result<NonZeroU128, DecodeError> {
    let Value::Number(value) = value else {
        return Err(syntax(format!("{context} must be an unsigned integer")));
    };
    NonZeroU128::new(*value).ok_or_else(|| syntax(format!("{context} must be nonzero")))
}

pub(crate) fn unknown_enum(field: &str, value: &str) -> DecodeError {
    syntax(format!("unknown {field} value `{value}`"))
}

pub(crate) fn syntax(message: impl Into<String>) -> DecodeError {
    DecodeError::Syntax {
        offset: 0,
        message: message.into(),
    }
}
