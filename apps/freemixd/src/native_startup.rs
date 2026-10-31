//! The native daemon's startup: resolving and preflighting sources, stingers,
//! captures, network streams, system audio, screen capture, and the frame
//! realization types the composition root hands to the realizer.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

#[cfg(feature = "native-media")]
pub(crate) struct NativeSourceResolution {
    pub(crate) sources: Vec<NativeResolvedSource>,
    #[cfg(target_os = "macos")]
    pub(crate) cameras: NativeCameraInputs,
}

/// A frame begun but not finished: both realizers have its jobs and the
/// control loop will collect them after servicing peers (structural item 1).
#[cfg(feature = "native-media")]
pub(crate) struct PendingFrame {
    pub(crate) deadline: Instant,
    pub(crate) started: Instant,
    pub(crate) command: bool,
    pub(crate) monitor_frame: Option<(u64, InputId)>,
    pub(crate) preview_source: Option<FrameResult>,
    pub(crate) shows_preview: bool,
    pub(crate) fade_to_black_excluded: Vec<fm_types::OutputId>,
    pub(crate) trace_before: Option<pacing_trace::CpuCounters>,
    pub(crate) events: Vec<RuntimeEventMessage>,
}

#[cfg(feature = "native-media")]
#[derive(Debug)]
pub(crate) enum NativeRealizationError {
    Video(NativeSourceRenderError),
    Audio(NativeMasterError),
    /// A realizer thread (mixer or compose) stopped or refused its command
    /// (structural item 1).
    RealizerThread(String),
}

#[cfg(feature = "native-media")]
impl fmt::Display for NativeRealizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video(error) => error.fmt(formatter),
            Self::Audio(error) => error.fmt(formatter),
            Self::RealizerThread(error) => formatter.write_str(error),
        }
    }
}

#[cfg(feature = "native-media")]
impl Error for NativeRealizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Video(error) => Some(error),
            Self::Audio(error) => Some(error),
            Self::RealizerThread(_) => None,
        }
    }
}

/// Time a command needs before its frame's deadline besides its journal
/// append: preparing, staging, and committing it.
#[cfg(feature = "native-media")]
const COMMAND_STAGING_MARGIN: Duration = Duration::from_millis(2);

/// The decision of [`NativeDaemon::command_window`]: open with `left` of a
/// frame `interval` before the next frame (`next_frame`) is due and recent
/// journal appends taking up to `append_peak`, given the frame a command
/// was first held back for (`deferred`, updated).
#[cfg(feature = "native-media")]
pub(crate) fn command_window_open(
    left: Duration,
    interval: Duration,
    append_peak: Option<Duration>,
    next_frame: FrameNumber,
    deferred: &mut Option<FrameNumber>,
) -> bool {
    let needed = append_peak.map_or(interval / 2, |peak| {
        peak.saturating_add(COMMAND_STAGING_MARGIN).min(interval)
    });
    if left >= needed && left > Duration::ZERO {
        *deferred = None;
        return true;
    }
    match *deferred {
        Some(held) if next_frame > held => {
            *deferred = None;
            true
        }
        Some(_) => false,
        None => {
            *deferred = Some(next_frame);
            false
        }
    }
}

#[cfg(feature = "native-media")]
pub(crate) fn validate_native_stinger_sources(
    stored: &StoredProject,
    sources: &[NativeResolvedSource],
) -> AppResult<()> {
    for config in stored
        .project()
        .stingers()
        .iter()
        .filter(|config| config.preload)
    {
        let source = sources
            .iter()
            .find(|source| source.input() == config.media_input)
            .ok_or_else(|| {
                AppFailure(format!(
                    "native Stinger slot {} media input {} is unavailable",
                    config.slot.number(),
                    config.media_input
                ))
            })?;
        match source {
            NativeResolvedSource::RetainedFrame { .. }
            | NativeResolvedSource::LocalVideo { .. } => {}
            NativeResolvedSource::LiveFrame { .. } => {
                return Err(AppFailure(format!(
                    "native Stinger slot {} media input {} is live and cannot be preloaded",
                    config.slot.number(),
                    config.media_input
                ))
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(feature = "native-media")]
pub(crate) fn partition_native_source_limits(
    width: u32,
    height: u32,
    stinger_sources: usize,
) -> AppResult<(NativeSourceLimits, NativeSourceLimits)> {
    let defaults = NativeSourceLimits::default();
    let stinger_ring_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(8))
        .and_then(|bytes| bytes.checked_mul(u64::from(defaults.max_video_frames_per_source.get())))
        .and_then(|bytes| bytes.checked_mul(u64::try_from(stinger_sources).unwrap_or(u64::MAX)))
        .ok_or_else(|| AppFailure("native Stinger ring byte partition overflow".into()))?;
    if stinger_ring_bytes > defaults.max_retained_rgba16f_bytes {
        return Err(AppFailure(format!(
            "native Stinger rings require {stinger_ring_bytes} retained bytes, exceeding aggregate limit {}",
            defaults.max_retained_rgba16f_bytes
        ))
        .into());
    }
    Ok((
        NativeSourceLimits {
            max_retained_rgba16f_bytes: defaults
                .max_retained_rgba16f_bytes
                .saturating_sub(stinger_ring_bytes),
            ..defaults
        },
        NativeSourceLimits {
            max_retained_rgba16f_bytes: stinger_ring_bytes,
            ..defaults
        },
    ))
}

#[cfg(feature = "native-media")]
pub(crate) fn preflight_native_video(
    runtime: &NativeMediaRuntime,
    adapter: Option<&Adapter>,
    sources: Vec<NativeResolvedSource>,
    stinger_sources: &[NativeResolvedSource],
    stored: &StoredProject,
) -> AppResult<(
    freemixd::native_media::NativeSourcePlayback,
    freemixd::native_media::NativeSourcePlayback,
)> {
    let (stingers, playback_limit) =
        preflight_native_stinger_video(runtime, adapter, stinger_sources, stored)?;
    let playback = runtime.preflight_resolved_source_playback_mixed_blocking(
        adapter,
        sources,
        native_clock_domain(),
        StreamSelector::Best,
        NativeSourceLimits {
            max_retained_rgba16f_bytes: playback_limit,
            ..NativeSourceLimits::default()
        },
    )?;
    let expected = stored.project().settings().video.dimensions;
    validate_native_video_dimensions("media source", playback.registry(), expected)?;
    Ok((playback, stingers))
}

/// Starts the capture backend and a supervisor for every capture input of
/// `stored` (IN-001), or `None` without a capture backend. A device that is
/// absent or fails is reported through its input's status; none stops the
/// daemon.
#[cfg(feature = "native-media")]
pub(crate) fn start_native_captures(
    stored: &StoredProject,
) -> AppResult<Option<native_captures::NativeCaptures>> {
    let Some(backend) = native_capture_backend() else {
        return Ok(None);
    };
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::Device {
                stable_key,
                capture,
            } => Some(native_captures::CaptureInput {
                input: input.id,
                stable_key: stable_key.clone(),
                settings: capture.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_captures::NativeCaptures::start(
        &backend,
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    )
    .map(Some)
    .map_err(|error| AppFailure(error).into())
}

/// Starts a receiver for every open network stream input of `stored`
/// (IN-015). A receiver that cannot reach its stream keeps reconnecting;
/// none stops the daemon.
#[cfg(feature = "native-media")]
pub(crate) fn start_native_network_streams(
    stored: &StoredProject,
) -> native_streams::NativeNetworkStreams {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::NetworkStream(config) => Some(native_streams::NetworkStreamInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_streams::NativeNetworkStreams::start(
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
        u16::try_from(settings.audio.channels.channels().len()).unwrap_or(2),
    )
}

/// Starts a receiver for every open system audio input of `stored`
/// (AU-011). A device that does not open keeps reopening; none stops the
/// daemon.
#[cfg(feature = "native-media")]
pub(crate) fn start_native_system_audio(
    stored: &StoredProject,
) -> native_system_audio::NativeSystemAudio {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::SystemAudio(config) => Some(native_system_audio::SystemAudioInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_system_audio::NativeSystemAudio::start(
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    )
}

/// Starts watching the desktop for every screen capture input of `stored`
/// (IN-011). A target that is not on the desktop waits for it; none stops
/// the daemon.
#[cfg(feature = "native-media")]
pub(crate) fn start_native_screen_capture(
    stored: &StoredProject,
) -> native_screen_capture::NativeScreenCapture {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::ScreenCapture(config) => Some(native_screen_capture::ScreenCaptureInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    let mut capture = native_screen_capture::NativeScreenCapture::start(
        desktop_capture::DesktopBackend::platform(),
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    );
    // A Wayland desktop shares the targets it granted before again.
    capture.set_restore_tokens(project.screen_capture_restore_tokens().clone());
    capture
}

/// Starts the media-file inputs of `stored` that play through their engine
/// transport (every local-video media input no Stinger names) and swaps each
/// one's resolved source for a live source seeded with its startup picture.
/// Returns the inputs, whose Master audio becomes pushed.
#[cfg(feature = "native-media")]
pub(crate) fn resolve_native_file_sources(
    adapter: Option<&Adapter>,
    stored: &StoredProject,
    sources: &mut [NativeResolvedSource],
) -> AppResult<(native_files::NativeMediaFiles, Vec<InputId>)> {
    let project = stored.project();
    let stinger_media = project
        .stingers()
        .iter()
        .map(|config| config.media_input)
        .collect::<std::collections::BTreeSet<_>>();
    let inputs = sources
        .iter()
        .filter_map(|source| match source {
            NativeResolvedSource::LocalVideo { input, path } if !stinger_media.contains(input) => {
                let settings = *project.media_playback(*input)?;
                Some(native_files::MediaFileInput {
                    input: *input,
                    path: path.clone(),
                    settings,
                    closed: project.input_lifecycle(*input).closed,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let ids = inputs.iter().map(|input| input.input).collect::<Vec<_>>();
    let dimensions = project.settings().video.dimensions;
    let (files, seeds) = match adapter {
        Some(adapter) => native_files::NativeMediaFiles::start(
            adapter,
            inputs,
            project.settings().frame_rate,
            native_clock_domain(),
            (dimensions.width(), dimensions.height()),
        )
        .map_err(AppFailure)?,
        None if inputs.is_empty() => native_files::NativeMediaFiles::start_empty(
            project.settings().frame_rate,
            native_clock_domain(),
            (dimensions.width(), dimensions.height()),
        ),
        None => {
            return Err(AppFailure("native media files require ffmpeg".into()).into());
        }
    };
    for (input, frame) in seeds {
        if let Some(source) = sources.iter_mut().find(|source| source.input() == input) {
            *source = NativeResolvedSource::LiveFrame { input, frame };
        }
    }
    Ok((files, ids))
}

#[cfg(feature = "native-media")]
pub(crate) fn preflight_native_stinger_video(
    runtime: &NativeMediaRuntime,
    adapter: Option<&Adapter>,
    sources: &[NativeResolvedSource],
    stored: &StoredProject,
) -> AppResult<(freemixd::native_media::NativeSourcePlayback, u64)> {
    let stinger_inputs = stored
        .project()
        .stingers()
        .iter()
        .filter(|config| config.preload)
        .map(|config| config.media_input)
        .fold(Vec::new(), |mut inputs, input| {
            if !inputs.contains(&input) {
                inputs.push(input);
            }
            inputs
        });
    let stinger_sources = sources
        .iter()
        .filter(|source| stinger_inputs.contains(&source.input()))
        .cloned()
        .collect::<Vec<_>>();
    let expected = stored.project().settings().video.dimensions;
    let (playback_limits, stinger_limits) =
        partition_native_source_limits(expected.width(), expected.height(), stinger_sources.len())?;
    let mut stingers = runtime.preflight_resolved_source_playback_mixed_blocking(
        adapter,
        stinger_sources,
        native_clock_domain(),
        StreamSelector::Best,
        stinger_limits,
    )?;
    for input in stinger_inputs {
        stingers.enable_stinger_source(input)?;
    }
    validate_native_video_dimensions("Stinger", stingers.registry(), expected)?;
    Ok((stingers, playback_limits.max_retained_rgba16f_bytes))
}

#[cfg(feature = "native-media")]
pub(crate) fn validate_native_video_dimensions(
    label: &str,
    registry: &freemixd::native_media::NativeSourceRegistry,
    expected: fm_frame::VideoDimensions,
) -> AppResult<()> {
    if registry
        .dimensions()
        .is_some_and(|dimensions| dimensions != (expected.width(), expected.height()))
    {
        return Err(AppFailure(format!(
            "native {label} dimensions must match project output {}x{}",
            expected.width(),
            expected.height()
        ))
        .into());
    }
    Ok(())
}

#[cfg(feature = "native-media")]
pub(crate) fn validate_native_audio_modes(stored: &StoredProject) -> AppResult<()> {
    if let Some(input) = stored.project().inputs().iter().find(|input| {
        matches!(
            &input.kind,
            InputKind::Simulated(fm_model::SimulatedInput {
                audio: SimulatedAudio::Sine { .. },
                ..
            })
        )
    }) {
        return Err(AppFailure(format!(
            "native generator input {} requires unsupported simulated sine audio",
            input.id
        ))
        .into());
    }
    Ok(())
}

/// Every live source's drift loop, unordered (Protocol 2.76).
#[cfg(feature = "native-media")]
pub(crate) fn live_sync_samples(
    network: &native_streams::NativeNetworkStreams,
    system_audio: &native_system_audio::NativeSystemAudio,
    captures: Option<&native_captures::NativeCaptures>,
    screen_capture: &native_screen_capture::NativeScreenCapture,
) -> Vec<fm_protocol::SourceSyncSample> {
    let mut samples = network.sync_samples();
    samples.extend(system_audio.sync_samples());
    samples.extend(screen_capture.sync_samples());
    if let Some(captures) = captures {
        samples.extend(captures.sync_samples());
    }
    samples
}

/// The one picture generator input `input` shows: bars or a solid colour
/// at the project's size, dated sequence zero.
#[cfg(feature = "native-media")]
pub(crate) fn native_generator_frame(
    input: InputId,
    simulated: &fm_model::SimulatedInput,
    settings: &fm_model::ProjectSettings,
) -> AppResult<fm_frame::CpuVideoFrame> {
    let pattern = match simulated.video {
        SimulatedVideo::Bars => SourcePattern::Bars,
        SimulatedVideo::Solid(color) => {
            SourcePattern::Solid(Rgba8::new(color.red, color.green, color.blue, color.alpha))
        }
    };
    let dimensions = settings.video.dimensions;
    let mut source = SimulatedVideoSource::new(
        dimensions.width(),
        dimensions.height(),
        settings.frame_rate,
        native_clock_domain(),
        pattern,
    )
    .map_err(|error| {
        AppFailure(format!(
            "native generator construction failed for input {input}: {error}"
        ))
    })?;
    source
        .next_frame()
        .map_err(|error| {
            AppFailure(format!(
                "native generator frame failed for input {input}: {error}"
            ))
        })?
        .ok_or_else(|| {
            AppFailure(format!(
                "native generator produced signal loss for input {input}"
            ))
            .into()
        })
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Metal
}

#[cfg(all(feature = "native-media", target_os = "windows"))]
pub(crate) const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Dx12
}

#[cfg(all(feature = "native-media", target_os = "linux"))]
pub(crate) const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Vulkan
}

#[cfg(feature = "native-media")]
pub(crate) fn host_deadline_offset(
    pacer: &FramePacer,
) -> Result<Duration, fm_scheduler::PacingError> {
    Ok(Duration::from_nanos(pacer.next_deadline()?.at_ns))
}

#[cfg(feature = "native-media")]
pub(crate) fn native_clock_domain() -> MediaClockDomainId {
    MediaClockDomainId::new(NonZeroU128::MIN)
}
