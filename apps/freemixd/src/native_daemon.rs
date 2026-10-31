//! The native daemon's composition: its per-frame runtime, camera and media
//! supervision, recorders, streams, displays, and monitors.
//!
//! Extracted from `main.rs` as a mechanical move. The type is still part of
//! the `freemixd` binary; the state it composes lives in the sibling modules.

#[allow(clippy::wildcard_imports)] // the binary's items and imports
use super::*;

#[cfg(feature = "native-media")]
#[allow(clippy::struct_excessive_bools)] // each bool is a distinct native-mode operator or media option
pub(crate) struct NativeDaemon {
    pub(crate) pacer: FramePacer,
    pub(crate) pacer_start_offset: Duration,
    /// When the runtime started: the clock stream reconnects and backoff
    /// are scheduled on.
    pub(crate) origin: Instant,
    /// When the pacer's first frame was due: the first time the frame loop
    /// asked for a frame, not when the runtime started. Startup work between
    /// the two (recorder and stream workers starting, listeners binding)
    /// would otherwise leave the first frames late and realized back to back
    /// to catch up. `None` until the frame loop first runs.
    pub(crate) pacing_origin: Option<Instant>,
    /// The frame a client command was first held back for (see
    /// [`Self::command_window`]); it is dispatched once that frame was
    /// realized.
    pub(crate) command_deferred_at: Option<FrameNumber>,
    /// The frame a daemon clock's retry was first held back for (see
    /// [`Self::retry_window`]), kept apart from client commands' hold so a
    /// retry never opens the window early for a client.
    pub(crate) retry_deferred_at: Option<FrameNumber>,
    /// Every realized frame's lateness, when `FREEMIXD_PACING_TRACE=1`.
    pub(crate) pacing_trace: Option<pacing_trace::PacingTrace>,
    pub(crate) latest_output: Option<NativeTexture>,
    pub(crate) latest_project_outputs: Vec<NativeOutputFrame>,
    /// The audio mixer and its decoders on their own thread (structural
    /// item 1): one bounded, ordered handoff for pushes, coverage waits,
    /// and per-frame mixes.
    pub(crate) audio: native_audio_realizer::NativeAudioHandle,
    pub(crate) project_plan: NativeProjectPlan,
    /// Compiled grades per engine revision, computed here and uploaded by
    /// the video realizer.
    pub(crate) grade_cache: grading::GradeCache,
    /// The GPU realizer thread; it owns the playback and stinger registries.
    pub(crate) video: native_video_realizer::NativeVideoHandle,
    /// A shared handle to the realizer's newest CPU source pictures.
    pub(crate) latest_frames: freemixd::native_media::NativeLatestFrames,
    /// A frame submitted to the audio and video realizers, waiting for the
    /// pass's peer work to finish before its consumers run.
    pub(crate) pending_frame: Option<PendingFrame>,
    /// A Preview composite submitted to the video realizer, collected at the
    /// next pass's start.
    pub(crate) pending_preview_render: bool,
    pub(crate) projected_frame: Option<FrameResult>,
    pub(crate) runtime: Arc<NativeMediaRuntime>,
    pub(crate) resolved_sources: Arc<Vec<NativeResolvedSource>>,
    pub(crate) assets_root: PathBuf,
    pub(crate) pending_stinger_mutation: Option<NativeStingerMutation>,
    pub(crate) stinger_retirements: NativeStingerRetirements,
    /// Realization state of every configured recorder slot: slot 0 is
    /// `--record-program` and slot 1 is `--record-program-2`. Each slot owns
    /// its own segment, sidecar, failure latch, and report.
    pub(crate) recorders: [ProgramRecording; RECORDER_SLOTS],
    /// Configured `--record-program` output path; recording configuration is
    /// independent of whether a segment is currently open.
    pub(crate) record_program: Option<PathBuf>,
    /// Configured `--record-program-2` output path: the second recorder slot,
    /// configured exactly like [`Self::record_program`] and deriving its own
    /// segment names.
    pub(crate) record_program_2: Option<PathBuf>,
    /// Optional project output selection for each recorder slot (`None` selects
    /// the first realized project output, or the unbound output when the
    /// project declares no outputs).
    pub(crate) record_outputs: [Option<fm_types::OutputId>; RECORDER_SLOTS],
    /// Whether each segment also writes a `--record-wav` linear-PCM sidecar
    /// next to the MP4. Applies to every configured recorder slot.
    pub(crate) record_wav: bool,
    /// Optional per-slot frame-boundary segment rotation target in seconds
    /// (`--record-segment-seconds` is slot 0 and `--record-segment-seconds-2`
    /// is slot 1). A slot whose entry is `None` never rotates automatically,
    /// even when the other slot is configured to.
    pub(crate) record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    /// Optional per-slot `libx264` video bitrate in kbps
    /// (`--record-bitrate-kbps` is slot 0 and `--record-bitrate-kbps-2` is
    /// slot 1). `None` keeps that slot's encoder default.
    pub(crate) record_bitrates: [Option<u32>; RECORDER_SLOTS],
    /// Optional per-slot AAC audio bitrate in kbps
    /// (`--record-audio-bitrate-kbps` is slot 0 and
    /// `--record-audio-bitrate-kbps-2` is slot 1). `None` keeps that slot's
    /// encoder default.
    pub(crate) record_audio_bitrates: [Option<u32>; RECORDER_SLOTS],
    /// Optional `--record-fallback-dir` plus the segment names still being
    /// started off the loop; every slot's name derivation consults it.
    pub(crate) segment_names: recording_session::SegmentNames,
    /// Hysteretic free-space alert for the configured program outputs.
    pub(crate) disk_alert: disk::DiskSpaceAlert,
    /// Latest successfully sampled free bytes across the configured outputs:
    /// every configured recording output's parent and every local HLS output
    /// directory; `None` until the first sample succeeds.
    pub(crate) disk_free_bytes: Option<u64>,
    /// Latest sampled free bytes on each recorder slot's output filesystem
    /// (2.140); `None` for a slot with no configured output or a failed
    /// query.
    pub(crate) recorder_free: [Option<u64>; RECORDER_SLOTS],
    /// Latest estimated seconds of recording left for each slot, from its
    /// free space and configured bitrate (2.143); `None` for a slot with no
    /// output, no known rate, or a failed query.
    pub(crate) recorder_remaining: [Option<u64>; RECORDER_SLOTS],
    /// When the last disk sample was attempted; `None` makes the first sample
    /// evaluate immediately instead of after one full interval.
    pub(crate) disk_sampled_at: Option<Instant>,
    /// Absolute output directory of every authored local HLS stream target, in
    /// authored order; empty when the show has no local outputs. The HLS sink
    /// creates each leaf directory later, so preflight and sampling resolve the
    /// nearest existing ancestor. Bounded by the engine's five-target cap.
    pub(crate) local_output_dirs: Vec<PathBuf>,
    /// Latest sampled local HLS output health per destination slot, indexed by
    /// the slot's one-based [`DestinationId`] minus one. Network targets and
    /// local outputs whose playlist cannot currently be read stay `None`, and
    /// their next wire sample carries no local field.
    pub(crate) local_output_status: [Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    /// The `--hls-listen` route table, when the listener runs: every stream
    /// destination edit (OR-006) republishes it from the realized
    /// destinations, so a local HLS destination added while running is served
    /// at once and a removed one is not served any longer.
    pub(crate) hls_routes: Option<hls_server::HlsRouteTable>,
    /// When the last local HLS output sample was attempted; `None` makes the
    /// first sample evaluate immediately instead of after one full interval.
    pub(crate) local_output_sampled_at: Option<Instant>,
    /// Segments stopped off the render path, awaiting bounded finalization.
    pub(crate) recording_finalizers: Vec<ProgramRecordingFinalizer>,
    pub(crate) streams: Vec<NativeProgramStream>,
    /// One group per distinct encoder-relevant identity (output protocol plus
    /// authored video bitrate): its members share one readback owner, one
    /// packed pair per frame, and one sequence cursor, fanned out through the
    /// shared [`OutputSet`]'s rendition planner.
    pub(crate) stream_groups: Vec<StreamRenditionGroup>,
    /// Shared failover state machine for every stream target; per-target
    /// transports stay owned by each [`NativeProgramStream`].
    pub(crate) output_set: OutputSet,
    /// `StreamStart` acceptances waiting for the next frame boundary.
    pub(crate) pending_stream_starts: Vec<SwitcherStreamTargetId>,
    pub(crate) streams_finalized: bool,
    pub(crate) telemetry: NativeRuntimeTelemetry,
    pub(crate) telemetry_emitted: bool,
    pub(crate) audio_meter_sequence: u64,
    pub(crate) pending_audio_meters: Option<AudioMetersMessage>,
    /// The headphone output: the monitor bus on a local audio device at its
    /// own level (AU-001).
    pub(crate) monitor_output: freemixd::monitor_output::MonitorOutputSlot,
    /// Each aux bus on its own local audio device (AU-002, Protocol 2.135),
    /// bus A first, at unity: the mixer already applied the bus fader and
    /// mute.
    pub(crate) aux_bus_outputs:
        [freemixd::monitor_output::MonitorOutputSlot; fm_model::AUX_BUS_COUNT],
    /// Each published aux bus's OMT audio sender (AU-002, Protocol 2.138),
    /// bus A first; `None` for a bus that is not published. The sender binds
    /// `0.0.0.0` on the bus's fixed port and is fed the bus's mixed samples
    /// at every audio-meter interval.
    pub(crate) aux_bus_omt: [Option<fm_io_omt::Sender>; fm_model::AUX_BUS_COUNT],
    pub(crate) stream_status_sequence: u64,
    pub(crate) pending_stream_status: Option<StreamStatusMessage>,
    /// Realized recorder slot bookkeeping behind `recorder_status`
    /// (Protocol 2.66).
    pub(crate) recorder_status: recorder_status::RecorderStatusTracker,
    /// Live source drift telemetry records (Protocol 2.76, AU-004).
    pub(crate) source_sync_status: native_sync::SourceSyncTracker,
    /// The monitor taps' frame interval, or `None` when the fitted tap size
    /// is unusable; the control loop schedules Preview tap frames from it.
    pub(crate) monitor_interval: Option<u64>,
    /// Whether any peer wants monitor frames.
    pub(crate) monitor_demand: bool,
    /// Which due Preview tap frames a late loop still composites, mirroring
    /// the realizer's own capture pacing.
    pub(crate) preview_pacing: display_outputs::PresentationPacing,
    /// Due Preview frames the control loop paced out before the realizer saw
    /// them; added to the realizer's snapshots so telemetry stays truthful.
    pub(crate) monitor_control_skips: u64,
    /// Encoded monitor frames the realizer produced, waiting to be published.
    pub(crate) pending_monitor_frames: Vec<(MonitorTap, Vec<u8>)>,
    /// Title inputs realized as live source frames rendered off the loop;
    /// see [`native_titles`].
    pub(crate) titles: native_titles::NativeTitleSources,
    /// Document inputs realized as live source frames of their current
    /// page, rendered off the loop (IN-014); see [`native_documents`].
    pub(crate) documents: native_documents::NativeDocumentSources,
    /// Transport-controlled media-file inputs decoded off the loop; see
    /// [`native_files`].
    pub(crate) files: native_files::NativeMediaFiles,
    /// Network stream inputs and their receivers (IN-015).
    pub(crate) network: native_streams::NativeNetworkStreams,
    /// System audio inputs and their audio-only receivers (AU-011).
    pub(crate) system_audio: native_system_audio::NativeSystemAudio,
    /// Screen and window capture inputs and their desktop watch (IN-011).
    pub(crate) screen_capture: native_screen_capture::NativeScreenCapture,
    /// Local camera and capture-card inputs and their device backend
    /// (IN-001); `None` without a capture backend.
    pub(crate) captures: Option<native_captures::NativeCaptures>,
    /// The source realized for every input, which live catalogue changes
    /// follow (see [`native_catalogue`]).
    pub(crate) catalogue: std::collections::BTreeMap<InputId, native_catalogue::NativeSourceKey>,
    /// Still images of inputs added live, decoding off the loop.
    pub(crate) stills: Vec<native_catalogue::PendingStill>,
    /// The project's picture size, in pixels.
    pub(crate) picture_size: (u32, u32),
    #[cfg(target_os = "macos")]
    pub(crate) cameras: NativeCameraInputs,
    #[cfg(target_os = "macos")]
    pub(crate) camera_telemetry_frozen: bool,
    /// Borderless fullscreen display outputs, one presentation per slot
    /// window (empty without the windowed presenter).
    #[cfg(feature = "display-surface")]
    pub(crate) displays: Vec<program_surface::ProgramPresentation>,
    /// The windowed presenter's live configuration and report channel.
    #[cfg(feature = "display-surface")]
    pub(crate) windowed: Option<program_surface::WindowedDisplays>,
    /// Display outputs presented to the diagnostic inventory host.
    pub(crate) display_inventory: Option<display_outputs::InventoryDisplayHost>,
    /// What the presenter was last told to show (OR-011).
    pub(crate) display_sync: display_outputs::DisplayOutputSync,
    /// Which realized frames the display outputs present.
    pub(crate) display_pacing: display_outputs::PresentationPacing,
    /// The Preview composite, rendered only while a display output shows
    /// Preview and on the Preview monitor tap's due frames.
    pub(crate) latest_preview: Option<NativeTexture>,
    /// The newest Preview composite waiting for slack after a pass's peer
    /// work, so it never sits between Program and the next frame deadline.
    /// One slot: a newer frame replaces an older one.
    pub(crate) pending_preview: native_realizer::NativePreviewQueue,
    /// Whether the last Preview composite failed (reported once per run of
    /// failures).
    pub(crate) preview_failed: bool,
    /// The `LiveLAN` viewer stream (OR-017), while `--live-lan-listen` runs.
    pub(crate) live_lan: Option<native_live_lan::NativeLiveLan>,
    /// The instant replay recording and Replay inputs (OR-012).
    pub(crate) replay: Option<native_replay::NativeReplay>,
    /// The highlight reel exports (OR-013).
    pub(crate) reel: Option<native_reel::NativeReel>,
}

/// Binds an OMT audio sender for every aux bus the project publishes
/// (AU-002, Protocol 2.138); buses A–G listen on their fixed port. A port
/// that cannot be bound leaves the bus unpublished, like an unusable local
/// output device: the show keeps running.
#[cfg(feature = "native-media")]
fn bind_aux_bus_omt_senders(
    project: &fm_model::Project,
) -> [Option<fm_io_omt::Sender>; fm_model::AUX_BUS_COUNT] {
    let mut senders: [Option<fm_io_omt::Sender>; fm_model::AUX_BUS_COUNT] =
        std::array::from_fn(|_| None);
    for (bus, value) in project.aux_buses().iter() {
        if value.omt {
            set_aux_bus_omt_sender(&mut senders[bus.index()], bus, true);
        }
    }
    senders
}

/// Starts or stops aux bus `bus`'s OMT audio sender, logging a bind failure
/// once; a later enable retries the bind.
#[cfg(feature = "native-media")]
fn set_aux_bus_omt_sender(
    slot: &mut Option<fm_io_omt::Sender>,
    bus: fm_model::AuxBusNumber,
    omt: bool,
) {
    if !omt {
        *slot = None;
        return;
    }
    if slot.is_some() {
        return;
    }
    let port = fm_model::aux_omt_port(bus);
    match fm_io_omt::Sender::bind(("0.0.0.0", port)) {
        Ok(sender) => *slot = Some(sender),
        Err(error) => {
            eprintln!(
                "aux bus {} OMT sender cannot bind 0.0.0.0:{port}: {error}",
                bus.letter()
            );
        }
    }
}

#[cfg(feature = "native-media")]
impl NativeDaemon {
    pub(crate) fn invalidate_projection(&mut self) {
        self.projected_frame = None;
    }

    pub(crate) fn start(
        store: &ProjectStore,
        stored: &StoredProject,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        Self::start_with_optional_context(store, stored, None, camera_helper)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_with_optional_context(
        store: &ProjectStore,
        stored: &StoredProject,
        context: Option<NativeContext>,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        let project_plan =
            NativeProjectPlan::compile(stored.project(), NativeProjectLimits::default())?;
        validate_native_audio_modes(stored)?;
        #[cfg(target_os = "macos")]
        let mut resolution = resolve_native_sources(store, stored, camera_helper)?;
        #[cfg(not(target_os = "macos"))]
        let resolution = resolve_native_sources(store, stored, camera_helper)?;
        let mut sources = resolution.sources;
        let titles = resolve_native_title_sources(store, stored, &mut sources)?;
        validate_native_stinger_sources(stored, &sources)?;
        let requires_ffmpeg = sources
            .iter()
            .any(|source| matches!(source, NativeResolvedSource::LocalVideo { .. }));
        let adapter = requires_ffmpeg
            .then(|| {
                Adapter::new(FfmpegConfig {
                    allowed_root: Some(store.assets_root()),
                    ..FfmpegConfig::default()
                })
            })
            .transpose()?;
        if let Some(adapter) = &adapter {
            let capabilities = adapter.capabilities();
            if !matches!(capabilities.ffmpeg, ToolAvailability::Available { .. })
                || !matches!(capabilities.ffprobe, ToolAvailability::Available { .. })
            {
                return Err(AppFailure(
                    "native video playback requires available ffmpeg and ffprobe capabilities"
                        .into(),
                )
                .into());
            }
        }
        // Stinger clips (and hot Stinger mutations) keep their clip-local
        // sequential decode; every other media file plays through its
        // engine-driven transport as a live source.
        let resolved_sources = sources.clone();
        let (files, file_inputs) =
            resolve_native_file_sources(adapter.as_ref(), stored, &mut sources)?;

        let master = NativeMasterRuntime::preflight_project_local_blocking(
            adapter.as_ref(),
            &sources,
            &project_plan,
            &stored.project().settings().audio,
            stored.project().settings().frame_rate,
            native_clock_domain(),
            stored.position().frames_rendered,
            NativeAudioLimits::default(),
        )?;
        let runtime = Arc::new(match context {
            Some(context) => NativeMediaRuntime::from_context_blocking(context)?,
            None => NativeMediaRuntime::new_blocking([platform_native_backend()])?,
        });
        let mut master = master;
        for input in file_inputs {
            master.attach_pushed_audio(input)?;
        }
        let network = start_native_network_streams(stored);
        for input in network.inputs() {
            master.attach_pushed_audio(input)?;
        }
        let system_audio = start_native_system_audio(stored);
        for input in system_audio.inputs() {
            master.attach_pushed_audio(input)?;
        }
        let screen_capture = start_native_screen_capture(stored);
        for input in screen_capture.inputs() {
            master.attach_pushed_audio(input)?;
        }
        let captures = start_native_captures(stored)?;
        for input in captures
            .iter()
            .flat_map(native_captures::NativeCaptures::inputs)
        {
            master.attach_pushed_audio(input)?;
        }
        // Replay inputs play their recorded audio (OR-012).
        for input in stored
            .project()
            .inputs()
            .iter()
            .filter(|input| matches!(input.kind, fm_model::InputKind::Replay))
        {
            master.attach_pushed_audio(input.id)?;
        }
        // The audio thread starts once every pushed source is attached, so
        // it owns the whole mixer and its decode workers from the first
        // frame; the control loop only sends it ordered commands.
        let audio =
            native_audio_realizer::NativeAudioHandle::spawn(master, "freemix-audio-realizer")?;
        let (playback, stingers) = preflight_native_video(
            &runtime,
            adapter.as_ref(),
            sources,
            &resolved_sources,
            stored,
        )?;
        // Stream runtimes compile after video/audio preflight so a destination
        // problem is reported against an otherwise healthy show. Starting one
        // is deferred to readiness; failures there latch per target.
        let mut output_set = OutputSet::new();
        let (streams, stream_groups, local_output_dirs) =
            NativeProgramStream::compile(&runtime, stored, &mut output_set)?;
        let slides = native_slides::NativeSlides::new(
            store.assets_root(),
            stored.project().settings().video.dimensions,
        );
        let replay_format = native_replay::ReplayFormat {
            width: stored.project().settings().video.dimensions.width(),
            height: stored.project().settings().video.dimensions.height(),
            rate: stored.project().settings().frame_rate,
            clock_domain: native_clock_domain(),
        };
        let latest_frames = playback.latest_frames();
        let monitor = NativeMonitorTap::new(&runtime, stored.project().settings());
        let monitor_interval = monitor.dimensions.map(|_| monitor.interval());
        let video = native_video_realizer::NativeVideoHandle::spawn(
            Arc::clone(&runtime),
            playback,
            stingers,
            slides,
            replay_format,
            monitor,
            "freemix-video-realizer",
        )?;
        #[cfg(target_os = "macos")]
        resolution.cameras.mark_preflight_frames_ingested();
        let pacer = FramePacer::restore(
            stored.project().settings().frame_rate,
            0,
            FrameNumber::new(stored.position().frames_rendered),
        )?;
        let pacer_start_offset = host_deadline_offset(&pacer)?;
        let origin = Instant::now();
        let telemetry = NativeRuntimeTelemetry::new(origin, runtime.context().adapter_info());
        Ok(Self {
            pacer,
            pacer_start_offset,
            origin,
            pacing_origin: None,
            command_deferred_at: None,
            retry_deferred_at: None,
            pacing_trace: pacing_trace::PacingTrace::from_environment(),
            latest_output: None,
            latest_project_outputs: Vec::new(),
            audio,
            project_plan,
            grade_cache: grading::GradeCache::default(),
            video,
            latest_frames,
            pending_frame: None,
            pending_preview_render: false,
            projected_frame: None,
            runtime,
            resolved_sources: Arc::new(resolved_sources),
            assets_root: store.assets_root().clone(),
            pending_stinger_mutation: None,
            stinger_retirements: NativeStingerRetirements::start()?,
            recorders: [ProgramRecording::Off, ProgramRecording::Off],
            record_program: None,
            record_program_2: None,
            record_outputs: [None, None],
            record_wav: false,
            record_segment_seconds: [None; RECORDER_SLOTS],
            record_bitrates: [None, None],
            record_audio_bitrates: [None, None],
            segment_names: recording_session::SegmentNames::default(),
            disk_alert: disk::DiskSpaceAlert::new(),
            disk_free_bytes: None,
            recorder_free: [None; RECORDER_SLOTS],
            recorder_remaining: [None; RECORDER_SLOTS],
            disk_sampled_at: None,
            local_output_dirs,
            local_output_status: [None; fm_switcher::MAX_STREAM_COUNT],
            hls_routes: None,
            local_output_sampled_at: None,
            recording_finalizers: Vec::new(),
            streams,
            stream_groups,
            output_set,
            pending_stream_starts: Vec::new(),
            streams_finalized: false,
            telemetry,
            telemetry_emitted: false,
            audio_meter_sequence: 0,
            pending_audio_meters: None,
            monitor_output: {
                let monitor = stored.project().audio_monitor_output();
                freemixd::monitor_output::MonitorOutputSlot::new(
                    "ffmpeg".into(),
                    monitor.device.clone(),
                    monitor.gain.get(),
                    monitor.muted,
                )
            },
            aux_bus_outputs: std::array::from_fn(|index| {
                let bus = stored.project().aux_buses().get(
                    fm_model::AuxBusNumber::new(u8::try_from(index + 1).unwrap())
                        .expect("aux bus numbers are 1 through 7"),
                );
                freemixd::monitor_output::MonitorOutputSlot::new(
                    "ffmpeg".into(),
                    bus.device.clone(),
                    0,
                    false,
                )
            }),
            aux_bus_omt: bind_aux_bus_omt_senders(stored.project()),
            stream_status_sequence: 0,
            pending_stream_status: None,
            recorder_status: recorder_status::RecorderStatusTracker::default(),
            source_sync_status: native_sync::SourceSyncTracker::default(),
            monitor_interval,
            monitor_demand: false,
            preview_pacing: display_outputs::PresentationPacing::default(),
            monitor_control_skips: 0,
            pending_monitor_frames: Vec::new(),
            titles,
            documents: native_documents::NativeDocumentSources::start(native_clock_domain())?,
            files,
            network,
            system_audio,
            screen_capture,
            captures,
            catalogue: native_catalogue::catalogue_keys(stored.project()),
            stills: Vec::new(),
            picture_size: {
                let dimensions = stored.project().settings().video.dimensions;
                (dimensions.width(), dimensions.height())
            },
            #[cfg(target_os = "macos")]
            cameras: resolution.cameras,
            #[cfg(target_os = "macos")]
            camera_telemetry_frozen: false,
            #[cfg(feature = "display-surface")]
            displays: Vec::new(),
            #[cfg(feature = "display-surface")]
            windowed: None,
            display_inventory: None,
            display_sync: display_outputs::DisplayOutputSync::default(),
            display_pacing: display_outputs::PresentationPacing::default(),
            latest_preview: None,
            pending_preview: native_realizer::NativePreviewQueue::new(),
            preview_failed: false,
            live_lan: None,
            replay: None,
            reel: None,
        })
    }

    /// Records replay sources into `root` (unless the show names a root)
    /// and plays Replay inputs from the files (OR-012).
    pub(crate) fn attach_replay(
        &mut self,
        control: &mut ControlService<Policy>,
        root: PathBuf,
        settings: &fm_model::ProjectSettings,
    ) {
        control.set_replay_default_root(root);
        control.set_replay_feed(fm_engine::EngineReplayFeed::External);
        let video = settings.video.dimensions;
        self.replay = Some(native_replay::NativeReplay::new(
            native_replay::ReplayFormat {
                width: video.width(),
                height: video.height(),
                rate: settings.frame_rate,
                clock_domain: native_clock_domain(),
            },
        ));
    }

    /// Exports highlight reels to their destinations (OR-013).
    pub(crate) fn attach_reel(&mut self, settings: &fm_model::ProjectSettings) {
        let video = settings.video.dimensions;
        self.reel = Some(native_reel::NativeReel::new(native_replay::ReplayFormat {
            width: video.width(),
            height: video.height(),
            rate: settings.frame_rate,
            clock_domain: native_clock_domain(),
        }));
    }

    /// Streams `LiveLAN` to the running `--live-lan-listen` listener (OR-017).
    pub(crate) fn attach_live_lan(
        &mut self,
        handle: live_lan::LiveLanHandle,
        settings: fm_model::ProjectSettings,
    ) {
        self.live_lan = Some(native_live_lan::NativeLiveLan::new(handle, settings));
    }

    #[cfg(feature = "display-surface")]
    pub(crate) fn start_with_program(
        store: &ProjectStore,
        stored: &StoredProject,
        context: NativeContext,
        outputs: Vec<program_surface::DisplayPresentationSetup>,
        windowed: program_surface::WindowedDisplays,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        let mut native =
            Self::start_with_optional_context(store, stored, Some(context), camera_helper)?;
        native.displays = outputs
            .into_iter()
            .map(program_surface::ProgramPresentation::new)
            .collect();
        native.windowed = Some(windowed);
        Ok(native)
    }

    /// Presents the project's display outputs to the diagnostic display
    /// inventory read from `inventory` instead of real windows.
    #[allow(clippy::unnecessary_wraps)] // matches the non-native stub
    pub(crate) fn attach_display_inventory(&mut self, inventory: PathBuf) -> AppResult<()> {
        self.display_inventory = Some(display_outputs::InventoryDisplayHost::new(
            display_outputs::InventoryFile::new(inventory),
        ));
        Ok(())
    }

    /// What the presenter last observed, or `None` without a presenter.
    pub(crate) fn display_report(&self) -> Option<display_outputs::DisplayReport> {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed {
            return Some(windowed.report());
        }
        self.display_inventory
            .as_ref()
            .map(|host| host.report().clone())
    }

    /// Brings the presenter to the project's display-output slots and tells
    /// the control service what it shows (OR-011). Without a presenter every
    /// slot keeps waiting with `no-presenter`.
    pub(crate) fn sync_display_outputs(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        let desired = control.snapshot().snapshot.display_outputs.clone();
        for (slot, config) in self.display_sync.changes(&desired) {
            #[cfg(feature = "display-surface")]
            if let Some(windowed) = &self.windowed {
                if let Some(display) = self.displays.get_mut(slot.index()) {
                    display.set_config(config.as_ref());
                }
                windowed.configure(slot, config.clone());
            }
            if let Some(host) = &mut self.display_inventory {
                host.configure(slot, config);
            }
        }
        let Some(report) = self.display_report() else {
            return Ok(());
        };
        for (slot, state) in self.display_sync.wire_states(&report) {
            control
                .set_display_output_status(server, slot, state)
                .map_err(|error| AppFailure(format!("display output {slot} status: {error}")))?;
        }
        if let Some(inventory) = self.display_sync.inventory(&report) {
            control
                .set_display_inventory(server, inventory)
                .map_err(|error| AppFailure(format!("display inventory: {error}")))?;
        }
        Ok(())
    }

    /// Reports each network stream input's receive status as it changes
    /// (IN-015): a state change at once, other changes at most once a
    /// second. The `LiveLAN` status (OR-017) is reported the same way.
    pub(crate) fn report_network_streams(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        if let Some(live_lan) = &mut self.live_lan {
            let status = live_lan.status(control.live_lan().enabled);
            control
                .set_live_lan_status(server, &status)
                .map_err(|error| AppFailure(format!("LiveLAN status: {error}")))?;
        }
        for (input, status) in self.network.statuses(Instant::now()) {
            control
                .set_network_stream_status(server, input, status)
                .map_err(|error| AppFailure(format!("network input {input} status: {error}")))?;
        }
        // System audio inputs (AU-011): each input's capture status.
        for (input, status) in self.system_audio.statuses(Instant::now()) {
            control
                .set_system_audio_status(server, input, &status)
                .map_err(|error| {
                    AppFailure(format!("system audio input {input} status: {error}"))
                })?;
        }
        // Screen capture inputs (IN-011): each input's capture status.
        for (input, status) in self.screen_capture.statuses(Instant::now()) {
            control
                .set_screen_capture_status(server, input, &status)
                .map_err(|error| {
                    AppFailure(format!("screen capture input {input} status: {error}"))
                })?;
        }
        // Capture inputs (IN-001): each input's acquisition status, and the
        // device inventory whenever it changes.
        if let Some(captures) = &mut self.captures {
            for (input, status) in captures.statuses(Instant::now()) {
                control
                    .set_capture_status(server, input, &status)
                    .map_err(|error| {
                        AppFailure(format!("capture input {input} status: {error}"))
                    })?;
            }
            if let Some(inventory) = captures.inventory() {
                control
                    .set_capture_inventory(server, &inventory)
                    .map_err(|error| AppFailure(format!("capture inventory: {error}")))?;
            }
        }
        Ok(())
    }

    /// Whether an active display output shows Preview, which then needs
    /// its own composite; a slot waiting for its display (or a presenter)
    /// shows nothing.
    pub(crate) fn display_shows_preview(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed
            && self.displays.iter().enumerate().any(|(index, display)| {
                display.source() == Some(DisplayOutputSource::Preview)
                    && DisplayOutputSlot::ALL
                        .get(index)
                        .is_some_and(|slot| windowed.active(*slot))
            })
        {
            return true;
        }
        self.display_inventory
            .as_ref()
            .is_some_and(|host| host.shows(DisplayOutputSource::Preview))
    }

    /// A Preview composite that would not finish this margin before the next
    /// Program deadline waits for the next pass.
    pub(crate) const PREVIEW_SLACK: Duration = Duration::from_millis(2);

    pub(crate) fn service_deferred_preview(&mut self, server: &ServerIdentity) -> AppResult<()> {
        let Some(job) = self.pending_preview.take() else {
            return Ok(());
        };
        let Ok(deadline) = self.next_deadline() else {
            return Ok(());
        };
        // A composite that would not finish a comfortable margin before the
        // next frame waits instead; Program's deadline wins.
        if Instant::now() + Self::PREVIEW_SLACK >= deadline || self.pending_preview_render {
            self.pending_preview.requeue(job);
            return Ok(());
        }
        self.video
            .submit_preview(native_video_realizer::VideoRenderJob {
                frame: preview_frame(job.frame, job.preview),
                plan: job.plan,
                grades: job.grades,
                grade_revision: job.grade_revision,
                slides: None,
                monitor: native_video_realizer::MonitorJob {
                    server: server.clone(),
                    program_frame: None,
                    preview_tap_frame: job.tap_frame,
                    preview: job.preview,
                    demand: self.monitor_demand,
                },
            })
            .map_err(|error| AppFailure(format!("Preview realizer: {error}")))?;
        self.pending_preview_render = true;
        Ok(())
    }

    /// Collects a Preview composite submitted at the end of the last pass.
    pub(crate) fn poll_deferred_preview(&mut self) -> AppResult<()> {
        if !std::mem::take(&mut self.pending_preview_render) {
            return Ok(());
        }
        let outcome = self
            .video
            .collect_preview()
            .map_err(|error| AppFailure(format!("Preview realizer: {error}")))?;
        self.apply_monitor_outcome(outcome.monitor_frames, outcome.monitor_counters);
        match outcome.result {
            Ok(texture) => {
                self.latest_preview = Some(texture);
                self.preview_failed = false;
            }
            Err(error) => {
                if !self.preview_failed {
                    eprintln!(
                        "FREEMIXD_DISPLAY_PREVIEW\tv=1\terror={}",
                        error.to_string().replace(['\t', '\n', '\r'], "_")
                    );
                }
                self.preview_failed = true;
                self.latest_preview = None;
            }
        }
        Ok(())
    }

    /// Presents every display output's own source after the frame.
    #[cfg_attr(
        not(feature = "display-surface"),
        allow(clippy::unnecessary_wraps) // windowed presentation can fail
    )]
    pub(crate) fn present_display_outputs(&mut self) -> AppResult<()> {
        #[cfg(feature = "display-surface")]
        for display in &mut self.displays {
            // An empty slot's window stays hidden; its surface is still
            // serviced every pass by `tick_if_due_collect`.
            let Some(source) = display.source() else {
                continue;
            };
            let Some(texture) = resolve_display_source_texture(
                source,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                self.latest_preview.as_ref(),
            ) else {
                continue;
            };
            display
                .present_latest(self.runtime.context(), texture)
                .map_err(|error| -> Box<dyn Error> { Box::new(AppFailure(error)) })?;
        }
        if let Some(host) = &mut self.display_inventory {
            host.poll(&self.runtime, Instant::now());
            let project_outputs = &self.latest_project_outputs;
            let unbound = self.latest_output.as_ref();
            let preview = self.latest_preview.as_ref();
            host.present(&self.runtime, |source| {
                resolve_display_source_texture(source, project_outputs, unbound, preview)
            });
        }
        Ok(())
    }

    /// Frames presented by every display output, failed display GPU
    /// operations, and realized frames the display outputs dropped because
    /// the frame loop was behind, or `None` without display outputs.
    pub(crate) fn display_output_totals(&self) -> Option<(u64, u64, u64)> {
        let dropped = self.display_pacing.dropped();
        #[cfg(feature = "display-surface")]
        if !self.displays.is_empty() {
            return Some((
                self.displays
                    .iter()
                    .map(|display| display.telemetry().frames_presented)
                    .fold(0, u64::saturating_add),
                0,
                dropped,
            ));
        }
        self.display_inventory.as_ref().map(|host| {
            let (presented, failures) = host.totals();
            (presented, failures, dropped)
        })
    }

    /// Whether any display output is active and so presents frames.
    pub(crate) fn display_presenting(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed
            && self.displays.iter().enumerate().any(|(index, display)| {
                display.source().is_some()
                    && DisplayOutputSlot::ALL
                        .get(index)
                        .is_some_and(|slot| windowed.active(*slot))
            })
        {
            return true;
        }
        self.display_inventory
            .as_ref()
            .is_some_and(display_outputs::InventoryDisplayHost::presenting)
    }

    /// Begins opening one slot's next segment for a configured output: the
    /// session and `FFmpeg` start on a worker, exactly as for a runtime
    /// `RecordStart`, and [`Self::prime_recorder`] awaits the segment.
    ///
    /// The slot index is zero-based (`--record-program` is 0 and
    /// `--record-program-2` is 1); the recorder itself reports the matching
    /// 1-based slot number.
    pub(crate) fn start_recorder(
        &mut self,
        slot: usize,
        stored: &StoredProject,
        path: &Path,
    ) -> AppResult<()> {
        let names = self.segment_names.clone();
        let segment = next_record_segment_path(path, |candidate| names.taken(candidate))?;
        self.recorders[slot] = ProgramRecording::Running(Box::new(NativeProgramRecorder::open(
            &self.runtime,
            stored,
            stored.position().frames_rendered,
            &segment,
            &names,
            self.record_wav,
            self.record_bitrates[slot],
            self.record_audio_bitrates[slot],
            slot + 1,
        )?));
        Ok(())
    }

    /// Records the optional `--record-fallback-dir`: every slot's sessions
    /// fail over to it, and its files count when deriving segment names.
    pub(crate) fn configure_record_fallback(&mut self, fallback: Option<PathBuf>) {
        self.segment_names = recording_session::SegmentNames::new(fallback);
    }

    /// Repairs every recording session of the configured slots that a crash
    /// left open, reporting each one on stderr as `FREEMIXD_RECORDING_RECOVERY`.
    ///
    /// Runs once at startup, after the recording configuration is known and
    /// before any recorder starts, so a recovered session's name is never
    /// reused and its torn segment is repaired before anything records next to
    /// it. Recovery never refuses startup: a session that cannot be recovered
    /// is reported and keeps its name.
    pub(crate) fn recover_recording_sessions(&self) {
        let outputs = [
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
        ];
        let scopes = outputs
            .into_iter()
            .enumerate()
            .filter_map(|(slot, output)| {
                output.map(|output| recording_session::RecoveryScope {
                    slot: slot + 1,
                    output,
                })
            })
            .collect::<Vec<_>>();
        for record in recording_session::recover_interrupted(&scopes, self.segment_names.fallback())
        {
            eprintln!("{record}");
        }
    }

    /// Records the configured output paths and optional project output
    /// selections so runtime `RecordStart` commands can derive later segment
    /// names from them even when startup itself does not open a segment.
    ///
    /// Slot 0 is `--record-program` (`--record-output`) and slot 1 is
    /// `--record-program-2` (`--record-output-2`); the shared `record_wav`
    /// flag applies to every configured slot, while each segment duration and
    /// bitrate pair applies only to its own slot. The whole-run telemetry
    /// reports recording as configured whenever at least one slot has a
    /// destination.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn configure_recordings(
        &mut self,
        primary: Option<PathBuf>,
        primary_output: Option<fm_types::OutputId>,
        secondary: Option<PathBuf>,
        secondary_output: Option<fm_types::OutputId>,
        wav: bool,
        segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
        primary_bitrate: Option<u32>,
        primary_audio_bitrate: Option<u32>,
        secondary_bitrate: Option<u32>,
        secondary_audio_bitrate: Option<u32>,
    ) {
        self.record_program = primary;
        self.record_program_2 = secondary;
        self.record_outputs = [primary_output, secondary_output];
        self.record_wav = wav;
        self.record_segment_seconds = segment_seconds;
        self.record_bitrates = [primary_bitrate, secondary_bitrate];
        self.record_audio_bitrates = [primary_audio_bitrate, secondary_audio_bitrate];
        self.telemetry.recorder_configured = self.recording_configured();
    }

    /// Whether program recording support is CONFIGURED for any slot.
    ///
    /// Deliberately independent of segment activity: the immutable handshake
    /// digest advertises configured startup support, while `FREEMIXD_RECORDER`
    /// reports per-segment runtime health.
    pub(crate) fn recording_configured(&self) -> bool {
        self.record_program.is_some() || self.record_program_2.is_some()
    }

    /// Starts and primes every configured recorder slot whose restored
    /// desired flag is true for daemon startup.
    ///
    /// Startup recording is fail-closed exactly like the single-slot daemon:
    /// the first slot that cannot start or reach mux readiness fails startup,
    /// and the caller finalizes whatever already started through the regular
    /// shutdown path. A configured slot whose restored flag is false stays
    /// closed even when the other slot is recording. `Ok(false)` means a
    /// requested shutdown interrupted a barrier.
    /// `before_frame` runs before every frame the startup barrier lets the
    /// pacer realize, so the playlist clock keeps cutting on schedule while a
    /// recorder starts.
    pub(crate) fn start_and_prime_configured_recorders(
        &mut self,
        stored: &StoredProject,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        shutdown: Option<&ProcessShutdown>,
        before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        let paths = [self.record_program.clone(), self.record_program_2.clone()];
        let configured = paths.each_ref().map(Option::is_some);
        let selected =
            startup_recording_slots(configured, stored.project().recording_desired_slots());
        for (slot, path) in paths.into_iter().enumerate() {
            let Some(path) = path else {
                continue;
            };
            if !selected[slot] {
                continue;
            }
            self.start_recorder(slot, stored, &path)?;
        }
        for slot in 0..RECORDER_SLOTS {
            if self.recorders[slot].is_running()
                && !self.prime_recorder(slot, control, server, shutdown, before_frame)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Preflights every authored local HLS output directory, refusing startup
    /// with a sanitized failure when one has too little free space.
    ///
    /// A local output is a configured show destination, so it is held to the
    /// same floor as the configured recording output. The directory is
    /// measured through its nearest existing ancestor because the HLS sink
    /// creates the leaf later. The failure names the authored stream target id
    /// and carries free-space counts or an I/O kind only; it never renders the
    /// directory.
    pub(crate) fn preflight_local_outputs(&self) -> AppResult<()> {
        for stream in &self.streams {
            let config = self
                .output_set
                .config(stream.destination_id)
                .ok_or_else(|| AppFailure("a compiled stream owns an output-set slot".into()))?;
            if config.protocol() != OutputProtocol::Hls {
                continue;
            }
            disk::preflight_local_output(Path::new(config.endpoint().path())).map_err(|error| {
                match error {
                    disk::DiskPreflightError::BelowMinimum { available } => AppFailure(format!(
                        "stream target {} local output has insufficient free space: requires at \
                         least {} free bytes, found {available}",
                        stream.target,
                        disk::RECORDING_MINIMUM_FREE_BYTES
                    )),
                    disk::DiskPreflightError::Unavailable(kind) => AppFailure(format!(
                        "stream target {} local output free space is unavailable: {kind}",
                        stream.target
                    )),
                }
            })?;
        }
        Ok(())
    }

    /// Probes every configured network stream endpoint exactly once, in
    /// authored order with each target's primary before its backup, and
    /// renders one sanitized record per probed endpoint.
    ///
    /// The probe is bounded and purely informational: it reads the compiled
    /// destination configuration, never mutates output-set, recorder, or
    /// engine state, and cannot fail startup. A destination that cannot be
    /// reached right now is reported and left alone — an ingest may
    /// legitimately come up later, and the shared output set owns
    /// reconnection. Local HLS destinations have no peer and contribute
    /// nothing. The record set is bounded by twice the five-target cap.
    pub(crate) fn network_preflight_records(&self) -> Vec<String> {
        let mut pending = Vec::with_capacity(self.streams.len().saturating_mul(2));
        for stream in &self.streams {
            let Some(config) = self.output_set.config(stream.destination_id) else {
                continue;
            };
            let target = stream.target.get().get();
            for endpoint in [preflight::PRIMARY_ENDPOINT, preflight::BACKUP_ENDPOINT] {
                if let Some(probe) = pending_network_probe(config, target, endpoint) {
                    pending.push(probe);
                }
            }
        }
        preflight::probe_targets(pending)
            .iter()
            .map(preflight::probe_record)
            .collect()
    }

    /// Samples the free space shared by the configured outputs at a bounded
    /// cadence and reports alert edges.
    ///
    /// The render loop calls this every frame; the gate is two optional field
    /// checks plus one `Instant` comparison, and a sample reaches the
    /// filesystem at most once per [`DISK_SAMPLE_INTERVAL`]. Every configured
    /// recording output contributes its parent directory, and every authored
    /// local HLS output contributes the nearest existing ancestor of its
    /// directory — the sink creates the leaf later — with the minimum across
    /// the queries that succeed driving the alert. Query failures stay silent:
    /// a failed query is skipped, and when no query succeeds the last sampled
    /// value and alert state stay in place. The first call while a recording
    /// or a local output is configured evaluates immediately, so a low value
    /// present at startup fires at the first frame boundary.
    pub(crate) fn sample_disk_space(&mut self, now: Instant) {
        let recording_parents = [
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
        ];
        if recording_parents.iter().all(Option::is_none) && self.local_output_dirs.is_empty() {
            return;
        }
        if self
            .disk_sampled_at
            .is_some_and(|last| now.saturating_duration_since(last) < DISK_SAMPLE_INTERVAL)
        {
            return;
        }
        self.disk_sampled_at = Some(now);
        let recording_outputs = [
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
        ];
        let local_ancestors = self
            .local_output_dirs
            .iter()
            .filter_map(|directory| disk::existing_ancestor(directory));
        let mut minimum: Option<u64> = None;
        let mut recording_minimum: Option<u64> = None;
        let mut recorder_free = [None; RECORDER_SLOTS];
        for (slot, output) in recording_outputs.into_iter().enumerate() {
            let Some(output) = output else {
                continue;
            };
            if let Ok(available) = disk::available_bytes(recording_parent(output)) {
                recorder_free[slot] = Some(available);
                recording_minimum =
                    Some(recording_minimum.map_or(available, |current| current.min(available)));
                minimum = Some(minimum.map_or(available, |current| current.min(available)));
            }
        }
        self.recorder_free = recorder_free;
        let mut recorder_remaining = [None; RECORDER_SLOTS];
        for (slot, recorder) in self.recorders.iter().enumerate() {
            let ProgramRecording::Running(recorder) = recorder else {
                continue;
            };
            if let Some(free) = recorder_free[slot] {
                recorder_remaining[slot] =
                    Some(free.saturating_mul(8) / recorder.bitrate_bits_per_second.max(1));
            }
        }
        self.recorder_remaining = recorder_remaining;
        for path in local_ancestors {
            if let Ok(available) = disk::available_bytes(path) {
                minimum = Some(minimum.map_or(available, |current| current.min(available)));
            }
        }
        let Some(available) = minimum else {
            return;
        };
        self.disk_free_bytes = Some(available);
        // A program recorder stops at the recording floor exactly like the
        // replay root does (OR-014): the segment is finalized cleanly and
        // the slot reports a disk-full failure.
        if recording_minimum.is_some_and(|free| free < disk::RECORDING_MINIMUM_FREE_BYTES) {
            for slot in &mut self.recorders {
                if let ProgramRecording::Running(recorder) = slot {
                    recorder.fail("storage:low_space".to_owned(), false);
                }
            }
        }
        // A local HLS output whose filesystem fell below the recording floor
        // stops at once with a disk-full failure (OR-001), like a recorder.
        let low_streams = self
            .streams
            .iter()
            .filter_map(|stream| {
                let config = self.output_set.config(stream.destination_id)?;
                if config.protocol() != OutputProtocol::Hls {
                    return None;
                }
                let directory = PathBuf::from(config.endpoint().path());
                let free = disk::existing_ancestor(&directory)
                    .and_then(|path| disk::available_bytes(path).ok())?;
                (free < disk::RECORDING_MINIMUM_FREE_BYTES).then_some(stream.target)
            })
            .collect::<Vec<_>>();
        for target in low_streams {
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|stream| stream.target == target)
            {
                stream.fail("storage:low_space", &mut self.output_set);
            }
        }
        match self.disk_alert.observe(available) {
            fm_observability::AlertTransition::Fired => {
                eprintln!("FREEMIXD_DISK\tv=1\tstate=low\tfree_bytes={available}");
            }
            fm_observability::AlertTransition::Cleared => {
                eprintln!("FREEMIXD_DISK\tv=1\tstate=normal\tfree_bytes={available}");
            }
            fm_observability::AlertTransition::None => {}
        }
    }

    /// Samples every local HLS target's published playlist health at the same
    /// bounded cadence as [`Self::sample_disk_space`].
    ///
    /// The render loop calls this every frame; the gate is one empty check plus
    /// one `Instant` comparison, and a playlist reaches the filesystem at most
    /// once per [`DISK_SAMPLE_INTERVAL`]. Each local `hls://` destination is
    /// read through [`local_hls_status`] into its destination slot; a target
    /// whose playlist cannot be read now stores `None`, so its next wire sample
    /// simply carries no local field. Network targets are skipped and keep
    /// `None` entries.
    pub(crate) fn sample_local_output_status(&mut self, now: Instant) {
        if self.local_output_dirs.is_empty() {
            return;
        }
        if self
            .local_output_sampled_at
            .is_some_and(|last| now.saturating_duration_since(last) < DISK_SAMPLE_INTERVAL)
        {
            return;
        }
        self.local_output_sampled_at = Some(now);
        for stream in &self.streams {
            let Some(config) = self.output_set.config(stream.destination_id) else {
                continue;
            };
            if config.protocol() != OutputProtocol::Hls {
                continue;
            }
            let slot = usize::from(stream.destination_id.get()) - 1;
            if let Some(entry) = self.local_output_status.get_mut(slot) {
                *entry = local_hls_status(Path::new(config.endpoint().path()));
            }
        }
    }

    /// Waits for one recorder slot's segment to open and then for its first
    /// muxed pair within that slot's own startup timeout, realizing Program
    /// frames on their deadlines meanwhile: the segment opens on its worker
    /// and the frames realized while it opens queue in its backlog, so the
    /// frame loop never waits on `FFmpeg`.
    ///
    /// Returns `Ok(true)` when the slot is ready or was never started,
    /// `Ok(false)` when a requested shutdown interrupted the barrier, and an
    /// error when the slot failed to open, failed, or exhausted its deadline.
    /// Every failure latches only the addressed slot. Only configured startup
    /// recording, which is fail-closed and completes before readiness, waits
    /// on this; a runtime `RecordStart` is primed by
    /// [`Self::advance_recordings`] without stalling the daemon loop.
    pub(crate) fn prime_recorder(
        &mut self,
        slot: usize,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        shutdown: Option<&ProcessShutdown>,
        before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        if !self.recorders[slot].is_running() {
            return Ok(true);
        }
        loop {
            if requested_daemon_shutdown(Some(&*self), shutdown).is_some() {
                return Ok(false);
            }
            let ProgramRecording::Running(recorder) = &mut self.recorders[slot] else {
                unreachable!("recorder presence was checked");
            };
            // The open worker's own connect timeout bounds the open; the
            // barrier budget starts once the recorder exists, as it did
            // when the open blocked.
            if let OpenPoll::Failed(failure) = recorder.poll_open() {
                self.recorders[slot] = ProgramRecording::Off;
                let error = failure.strip_prefix("start:").unwrap_or(&failure);
                return Err(AppFailure(error.to_owned()).into());
            }
            recorder.pump_backlog();
            match recorder.startup_decision()? {
                StartupPairDecision::Ready => {
                    recorder.priming = None;
                    return Ok(true);
                }
                StartupPairDecision::Failed => unreachable!("failure is returned as an error"),
                StartupPairDecision::Pending => {}
            }
            if recorder
                .priming
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(recorder.fail_startup_timeout());
            }
            before_frame(self, control)?;
            self.tick_if_due(control, server)?;
            thread::sleep(NATIVE_IO_POLL_INTERVAL);
        }
    }

    /// Starts sinks for every target whose authored intent is to be running
    /// once the daemon reconciles desired state.
    ///
    /// Called at readiness, mirroring the recorder's startup priming but
    /// deliberately without a barrier: a destination that refuses to open must
    /// degrade its own target, not delay or abort daemon startup.
    pub(crate) fn prime_startup_streams(&mut self, stored: &StoredProject) {
        for target in stored.project().stream_targets() {
            if !target.running() || target.startup() != StartupPolicy::ReconcileDesiredState {
                continue;
            }
            let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|stream| stream.target == target_id)
            {
                stream.start(&mut self.output_set);
            }
        }
    }

    /// Schedules live sink changes for an accepted StreamStart/StreamStop.
    ///
    /// Starts wait for the next frame boundary; stops take effect off the
    /// render path immediately: the shared output set drops queued pairs and
    /// disconnects the transport under its bounded deadlines.
    ///
    /// The group start and stop (OR-006) do exactly that for every
    /// destination and nothing else: recorders are separate workers no stream
    /// command reaches. A retry stops the destination now, clearing a latched
    /// failure, and starts it again at the next frame boundary with a fresh
    /// reconnect budget.
    pub(crate) fn observe_stream_command(
        &mut self,
        command: &CommandMessage,
        result: &CommandResult,
    ) {
        if !matches!(result, CommandResult::Accepted { .. }) {
            return;
        }
        let target_id =
            |target: WireStreamTargetId| SwitcherStreamTargetId::from_non_zero(target.get());
        match command.payload {
            CommandPayload::StreamStart { target } => self.request_stream_start(target_id(target)),
            CommandPayload::StreamStop { target } => self.request_stream_stop(target_id(target)),
            CommandPayload::StreamRetry { target } => {
                let target = target_id(target);
                if let Some(stream) = self
                    .streams
                    .iter_mut()
                    .find(|stream| stream.target == target)
                {
                    stream.retry(&mut self.output_set);
                }
                self.request_stream_start(target);
            }
            CommandPayload::StreamStartAll => {
                let targets: Vec<_> = self.streams.iter().map(|stream| stream.target).collect();
                for target in targets {
                    self.request_stream_start(target);
                }
            }
            CommandPayload::StreamStopAll => {
                let targets: Vec<_> = self.streams.iter().map(|stream| stream.target).collect();
                for target in targets {
                    self.request_stream_stop(target);
                }
            }
            _ => {}
        }
    }

    /// Queues one destination's start for the next frame boundary.
    pub(crate) fn request_stream_start(&mut self, target: SwitcherStreamTargetId) {
        if !self.pending_stream_starts.contains(&target) {
            self.pending_stream_starts.push(target);
        }
    }

    /// Stops one destination off the render path now.
    pub(crate) fn request_stream_stop(&mut self, target: SwitcherStreamTargetId) {
        self.pending_stream_starts
            .retain(|pending| *pending != target);
        if let Some(stream) = self
            .streams
            .iter_mut()
            .find(|stream| stream.target == target)
        {
            stream.stop_requested(&mut self.output_set);
        }
    }

    /// Follows the stored project's stream destinations after a committed
    /// command (OR-006): a removed or edited destination's runtime retires —
    /// the engine refuses both while it is desired running, so its transport
    /// is already stopping — and every destination without a runtime gets
    /// one in a free output-set slot, joining the rendition group of its
    /// encoder identity or a new one. Running destinations, their groups, and
    /// recorders are untouched.
    ///
    /// An authored destination the codec parser refuses is unreachable (the
    /// model validates the same rules) and is reported and left unrealized
    /// rather than stopping the show.
    pub(crate) fn sync_stream_targets(
        &mut self,
        project: &fm_model::Project,
        frames_rendered: u64,
    ) -> AppResult<()> {
        let targets = project.stream_targets();
        let mut index = 0;
        while index < self.streams.len() {
            let current = &self.streams[index];
            if targets
                .iter()
                .any(|target| target.clone().set_running(false) == current.authored)
            {
                index += 1;
                continue;
            }
            let mut retired = self.streams.remove(index);
            self.pending_stream_starts
                .retain(|pending| *pending != retired.target);
            retired.stop_live(&mut self.output_set);
            if retired.started {
                retired.emit_final_record(&self.output_set);
            }
            let _ = self.output_set.remove_destination(retired.destination_id);
            self.local_output_status[usize::from(retired.destination_id.get()) - 1] = None;
            for group in &mut self.stream_groups {
                group
                    .core
                    .members
                    .retain(|member| *member != retired.destination_id);
            }
            // Dropping the transport hands any outstanding stop to its worker
            // thread, which reaps the child off the daemon loop.
        }
        for target in targets {
            let switcher_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
            if self
                .streams
                .iter()
                .any(|stream| stream.target == switcher_id)
            {
                continue;
            }
            let (protocol, endpoint, stream_key, backup_endpoint) =
                match split_target_endpoints(target) {
                    Ok(split) => split,
                    Err(error) => {
                        eprintln!(
                            "FREEMIXD_STREAM_FAILURE\tv=1\ttarget={switcher_id}\tfailure={}",
                            diagnostic_field(&format!("configure:{error}"))
                        );
                        continue;
                    }
                };
            let Some(destination_id) = (1..=fm_io_network::MAX_DESTINATIONS)
                .filter_map(|slot| DestinationId::new(u8::try_from(slot).ok()?).ok())
                .find(|slot| !self.output_set.contains(*slot))
            else {
                // The engine caps destinations at the output set's size.
                continue;
            };
            let format = stream_record_format(project.settings(), frames_rendered)?;
            let key = (target.output(), protocol, target.video_bitrate_kbps());
            let group_index = self.stream_group_for(key, &format, frames_rendered)?;
            let stream = NativeProgramStream::realize(
                SplitStreamTarget {
                    target,
                    protocol,
                    endpoint,
                    stream_key,
                    backup_endpoint,
                    group_index,
                },
                destination_id,
                &mut self.stream_groups[group_index],
                &format,
                frames_rendered,
                &mut self.output_set,
            )?;
            self.streams.push(stream);
        }
        self.local_output_dirs = self.local_hls_directories();
        self.publish_hls_routes();
        Ok(())
    }

    /// Republishes the `--hls-listen` route table from the realized
    /// destinations, if the listener runs. The engine's unique target ids
    /// make a duplicate unreachable; one is reported and keeps the previous
    /// table rather than serving the wrong directory.
    pub(crate) fn publish_hls_routes(&self) {
        let Some(routes) = &self.hls_routes else {
            return;
        };
        match hls_roots(Some(self)) {
            Ok(roots) => routes.replace(roots),
            Err(error) => eprintln!(
                "FREEMIXD_STREAM_FAILURE\tv=1\ttarget=hls\tfailure={}",
                diagnostic_field(&format!("hls_routes:{error}"))
            ),
        }
    }

    /// The group a new destination with encoder identity `key` joins: the
    /// group already carrying that identity, else an emptied group rebuilt
    /// for it in place, else a new group. Rendition ids stay each group's
    /// one-based position, so running members never change rendition.
    pub(crate) fn stream_group_for(
        &mut self,
        key: StreamGroupKey,
        format: &RecordFormat,
        frames_rendered: u64,
    ) -> AppResult<usize> {
        if let Some(index) = self.stream_groups.iter().position(|group| group.key == key) {
            return Ok(index);
        }
        let (width, height) = self.picture_size;
        let size = (
            NonZeroU32::new(width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(height).unwrap_or(NonZeroU32::MIN),
        );
        if let Some(index) = self
            .stream_groups
            .iter()
            .position(|group| group.core.members.is_empty())
        {
            self.stream_groups[index] = new_stream_group(
                &self.runtime,
                format,
                size,
                key,
                (index, index + 1),
                frames_rendered,
            )?;
            return Ok(index);
        }
        let index = self.stream_groups.len();
        self.stream_groups.push(new_stream_group(
            &self.runtime,
            format,
            size,
            key,
            (index, index + 1),
            frames_rendered,
        )?);
        Ok(index)
    }

    /// The absolute directory of every realized local HLS destination.
    pub(crate) fn local_hls_directories(&self) -> Vec<PathBuf> {
        self.streams
            .iter()
            .filter_map(|stream| self.output_set.config(stream.destination_id))
            .filter(|config| config.protocol() == OutputProtocol::Hls)
            .map(|config| PathBuf::from(config.endpoint().path()))
            .collect()
    }

    /// Consumes pending `StreamStart` requests at a frame boundary.
    pub(crate) fn start_pending_streams(&mut self) {
        for target in std::mem::take(&mut self.pending_stream_starts) {
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|stream| stream.target == target)
            {
                stream.start(&mut self.output_set);
            }
        }
    }

    /// Stops every stream under a bounded deadline and emits exactly one
    /// sanitized final record per configured target. Collected failures are
    /// returned so shutdown can report them after the recorder result instead
    /// of abandoning the rest of teardown.
    pub(crate) fn finalize_streams(&mut self) -> Vec<AppResult<()>> {
        if self.streams_finalized {
            return Vec::new();
        }
        self.streams_finalized = true;
        let mut results = Vec::new();
        for index in 0..self.streams.len() {
            self.streams[index].stop_live(&mut self.output_set);
            if let Some(error) = self.streams[index].shutdown_error() {
                results.push(error);
            }
            self.streams[index].emit_final_record(&self.output_set);
        }
        results
    }

    pub(crate) fn tick_if_due(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        let _ = self.tick_if_due_collect(control, server)?;
        Ok(())
    }

    pub(crate) fn take_audio_meters(&mut self) -> Option<AudioMetersMessage> {
        self.pending_audio_meters.take()
    }

    /// Consumes the latest-wins stream status record built at the last frame
    /// boundary, mirroring [`Self::take_audio_meters`].
    pub(crate) fn take_stream_status(&mut self) -> Option<StreamStatusMessage> {
        self.pending_stream_status.take()
    }

    /// Asks the desktop to pick screen capture input `input`'s target again
    /// (IN-011); `false` when it is not a screen capture input.
    pub(crate) fn pick_screen_capture(&mut self, input: InputId) -> bool {
        self.screen_capture.pick(input)
    }

    /// Every screen capture input's restore token, when a Wayland desktop
    /// granted or dropped one since the last call, or the project refused
    /// them a while ago (IN-011).
    pub(crate) fn take_screen_capture_restore_tokens(
        &mut self,
        now: Instant,
    ) -> Option<std::collections::BTreeMap<InputId, String>> {
        self.screen_capture.take_restore_tokens(now)
    }

    /// The project refused the screen capture restore tokens last taken:
    /// they are offered again later (IN-011).
    pub(crate) fn screen_capture_restore_tokens_refused(&mut self, now: Instant) {
        self.screen_capture.restore_tokens_refused(now);
    }

    /// The realized recorder slots (Protocol 2.66), at most every
    /// [`recorder_status::RECORDER_STATUS_INTERVAL`].
    pub(crate) fn take_recorder_status(
        &mut self,
        server: &ServerIdentity,
    ) -> Option<RecorderStatusMessage> {
        let configured = [
            self.record_program.is_some(),
            self.record_program_2.is_some(),
        ];
        let views: [recorder_status::SlotView; RECORDER_SLOTS] =
            std::array::from_fn(|slot| match &self.recorders[slot] {
                ProgramRecording::Off if configured[slot] => recorder_status::SlotView::Off,
                ProgramRecording::Off => recorder_status::SlotView::Unconfigured,
                ProgramRecording::Running(recorder) => {
                    if recorder.capture.first_failure.is_some() {
                        recorder_status::SlotView::Failed(
                            recorder
                                .capture
                                .code
                                .unwrap_or(OutputFailure::new(OutputFailureCode::Unknown)),
                        )
                    } else if recorder.is_settling() {
                        recorder_status::SlotView::Settling
                    } else {
                        recorder_status::SlotView::Running
                    }
                }
            });
        self.recorder_status.poll(
            server,
            views,
            self.recorder_free,
            self.recorder_remaining,
            Instant::now(),
        )
    }

    /// Every running live source's drift loop (Protocol 2.76, AU-004), in
    /// input order.
    pub(crate) fn source_sync_samples(&self) -> Vec<fm_protocol::SourceSyncSample> {
        native_sync::wire_samples(live_sync_samples(
            &self.network,
            &self.system_audio,
            self.captures.as_ref(),
            &self.screen_capture,
        ))
    }

    /// The live source drift telemetry record (Protocol 2.76), at most every
    /// [`native_sync::SOURCE_SYNC_STATUS_INTERVAL`] while any live source
    /// runs.
    pub(crate) fn take_source_sync_status(
        &mut self,
        server: &ServerIdentity,
    ) -> Option<fm_protocol::SourceSyncStatusMessage> {
        let (network, system_audio) = (&self.network, &self.system_audio);
        let captures = self.captures.as_ref();
        let screen_capture = &self.screen_capture;
        self.source_sync_status.poll(
            server,
            || live_sync_samples(network, system_audio, captures, screen_capture),
            Instant::now(),
        )
    }

    /// Whether any eligible session currently wants monitor frames; without
    /// demand no readback runs.
    pub(crate) fn set_monitor_demand(&mut self, demand: bool) {
        self.monitor_demand = demand;
    }

    /// A snapshot of the newest CPU frame of every Program source, for an
    /// exact scene preview composed off the loop (shared frames, no copy,
    /// no GPU work).
    pub(crate) fn live_source_frames(&self) -> scene_preview::LiveSourceFrames {
        scene_preview::LiveSourceFrames::snapshot(&self.latest_frames)
    }

    /// Consumes every tap's latest encoded monitor frame group, Preview
    /// first.
    pub(crate) fn take_monitor_frames(&mut self) -> Vec<(MonitorTap, Vec<u8>)> {
        std::mem::take(&mut self.pending_monitor_frames)
    }

    /// Applies one realizer outcome's monitor frames and counters, folding in
    /// the due frames the control loop paced out before submitting.
    pub(crate) fn apply_monitor_outcome(
        &mut self,
        frames: Vec<(MonitorTap, Vec<u8>)>,
        mut counters: [NativeTapCounters; 2],
    ) {
        let skipped = &mut counters[MonitorTap::Preview.index()].skipped;
        *skipped = skipped.saturating_add(self.monitor_control_skips);
        self.telemetry.set_monitor_taps(counters);
        self.pending_monitor_frames.extend(frames);
    }

    /// Monotonic milliseconds since the native runtime's origin — the clock
    /// the shared output set schedules reconnects and backoff on.
    pub(crate) fn stream_clock_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Feeds the selected program texture and audio block to every running
    /// recorder slot.
    ///
    /// Each running slot resolves its own selected [`fm_types::OutputId`]
    /// independently; a slot whose output is missing latches with
    /// `"capture:missing_output"`. Every running slot that resolves a
    /// texture receives a clone of the block its output hears, mirroring
    /// how the stream fan-out clones one block per target. An idle or
    /// missing-output slot clones nothing, and each slot keeps its own
    /// readback owner. Takes the recorder
    /// pieces separately so the caller can hold immutable borrows of other
    /// daemon fields.
    pub(crate) fn capture_recorders(
        recorders: &mut [ProgramRecording; RECORDER_SLOTS],
        record_outputs: &[Option<fm_types::OutputId>; RECORDER_SLOTS],
        runtime: &NativeMediaRuntime,
        project_outputs: &[NativeOutputFrame],
        unbound_output: Option<&NativeTexture>,
        readbacks: &mut FrameReadbacks<'_>,
        audio: &CaptureAudio<'_>,
    ) {
        let mut resolved: [Option<&NativeTexture>; RECORDER_SLOTS] = [None, None];
        for (slot_index, slot) in recorders.iter_mut().enumerate() {
            let ProgramRecording::Running(recorder) = slot else {
                continue;
            };
            if let Some(texture) = resolve_project_output_texture(
                record_outputs[slot_index],
                project_outputs,
                unbound_output,
            ) {
                resolved[slot_index] = Some(texture);
            } else {
                recorder.fail("capture:missing_output".to_owned(), true);
            }
        }
        // A slot recording an output Fade to Black leaves live records the
        // mix before the fade, one fading over its own duration its own mix,
        // and every other slot the faded mix (SW-004).
        for (slot_index, slot) in recorders.iter_mut().enumerate() {
            let ProgramRecording::Running(recorder) = slot else {
                continue;
            };
            let Some(program) = resolved[slot_index] else {
                continue;
            };
            let block = audio
                .for_output(record_outputs[slot_index], project_outputs)
                .clone();
            recorder.capture(runtime, program, readbacks, block);
        }
    }

    /// Packs one pair per rendition group per frame and fans it out through
    /// the planner: destinations whose encoder-relevant settings match cost
    /// one readback and one packed pair in total, while every member route
    /// stays independently queued, counted, and flagged in the shared output
    /// set.
    ///
    /// Sequential by design: the groups capture before the recorders, each
    /// group performs its one synchronous readback (shared through
    /// `readbacks` with every other consumer of the same picture and size
    /// this frame), and no path here blocks the render loop beyond the
    /// existing diagnostic readback contract. A shared-step failure
    /// latches every active member of that group; a refused route degrades
    /// only its own target's counters. Takes the stream pieces separately so
    /// the caller can hold immutable borrows of other daemon fields.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) fn capture_streams<'frame>(
        streams: &mut [NativeProgramStream],
        stream_groups: &mut [StreamRenditionGroup],
        output_set: &mut OutputSet,
        runtime: &NativeMediaRuntime,
        project_outputs: &'frame [NativeOutputFrame],
        unbound_output: Option<&'frame NativeTexture>,
        readbacks: &mut FrameReadbacks<'frame>,
        capture_audio: &CaptureAudio<'_>,
    ) {
        let sequence = capture_audio.faded.timing().sequence();
        for group in stream_groups.iter_mut() {
            // A target streaming an output Fade to Black leaves live streams
            // the mix before the fade (SW-004).
            let audio = capture_audio.for_output(Some(group.core.output), project_outputs);
            let members = group.core.members.clone();
            // A route joins this frame only when it was asked to run, never
            // latched a failure, and was not deliberately stopped.
            let active: Vec<bool> = members
                .iter()
                .map(|destination| {
                    streams
                        .iter()
                        .find(|stream| stream.destination_id == *destination)
                        .is_some_and(|stream| route_active(stream, output_set))
                })
                .collect();
            if !active.iter().any(|&is_active| is_active) {
                continue;
            }
            let Some(program) = resolve_project_output_texture(
                Some(group.core.output),
                project_outputs,
                unbound_output,
            ) else {
                for (destination, &is_active) in members.iter().zip(&active) {
                    if is_active
                        && let Some(stream) = streams
                            .iter_mut()
                            .find(|stream| stream.destination_id == *destination)
                    {
                        stream.fail("capture:missing_output", output_set);
                    }
                }
                continue;
            };
            let rendition = group.core.rendition;
            let outcome = {
                let StreamRenditionGroup {
                    core,
                    readback,
                    format,
                    ..
                } = group;
                let StreamRenditionCore {
                    members,
                    variants,
                    next_sequence,
                    ..
                } = core;
                plan_and_pack_group_frame(
                    members,
                    variants,
                    next_sequence,
                    sequence,
                    &active,
                    || {
                        pack_group_pair(
                            runtime, program, readback, readbacks, format, rendition, sequence,
                            audio,
                        )
                    },
                )
            };
            match outcome {
                GroupFrameOutcome::Idle => {}
                GroupFrameOutcome::Failed(failures) => {
                    for (destination, failure) in failures {
                        if let Some(stream) = streams
                            .iter_mut()
                            .find(|stream| stream.destination_id == destination)
                        {
                            stream.fail(&failure, output_set);
                        }
                    }
                }
                GroupFrameOutcome::Packed { plan, packet } => {
                    let mut routes: Vec<(DestinationId, &mut StreamPairLedger, &mut bool)> =
                        streams
                            .iter_mut()
                            .filter(|stream| members.contains(&stream.destination_id))
                            .map(|stream| {
                                (
                                    stream.destination_id,
                                    &mut stream.ledger,
                                    &mut stream.enqueue_backpressure,
                                )
                            })
                            .collect();
                    match output_set.enqueue_rendition(&plan, &packet) {
                        Ok(outcomes) => admit_rendition_outcomes(&outcomes, sequence, &mut routes),
                        Err(error) => {
                            // Unreachable short of a bug: every planned
                            // destination exists in this set, so degrade the
                            // group's members instead of panicking on air.
                            let fanned: Vec<DestinationId> = routes
                                .iter()
                                .map(|(destination, _, _)| *destination)
                                .collect();
                            for destination in fanned {
                                if let Some(stream) = streams
                                    .iter_mut()
                                    .find(|stream| stream.destination_id == destination)
                                {
                                    stream.fail(&format!("fanout:{error}"), output_set);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Advances every target one step in the shared output set after each
    /// frame's enqueue loop, so connects, backoff, failover, and writes
    /// progress at the render cadence, then builds the latest-wins stream
    /// status record. An empty active set publishes no record at all.
    pub(crate) fn advance_stream_routes(&mut self, server: &ServerIdentity) -> AppResult<()> {
        let now_ms = self.stream_clock_ms();
        for stream in &mut self.streams {
            stream.poll(&mut self.output_set, now_ms);
        }
        let status_samples = stream_status_samples(native_stream_snapshots(
            &self.output_set,
            &mut self.streams,
            &self.local_output_status,
            now_ms,
        ));
        if let Some(status) =
            stream_status_message(server, self.stream_status_sequence, status_samples)?
        {
            self.stream_status_sequence = status.sequence;
            self.pending_stream_status = Some(status);
        }
        Ok(())
    }

    /// Schedules recording realization for an accepted RecordStart/RecordStop
    /// addressed to one recorder slot.
    ///
    /// A start begins opening the next segment of the addressed configured
    /// slot without blocking: the segment file and `FFmpeg` start on a worker,
    /// and [`Self::advance_recordings`] then awaits the same bounded mux
    /// readiness barrier used at startup while the daemon loop keeps
    /// realizing frames and serving every session. The session loop holds the
    /// command's acknowledgement until [`Self::recording_settling`] clears, so
    /// durability and the engine commit have already settled and a client is
    /// merely delayed, never acknowledged for unrealized state. The addressed
    /// slot is realized independently of the other: a failure latches only
    /// that slot's attempt unavailable and reports its own sanitized notice
    /// without aborting the show or the other slot.
    ///
    /// A stop retires only the addressed slot's open segment off the hot path:
    /// the slot drops to [`ProgramRecording::Off`] at once and finalization
    /// runs on a dedicated worker, exactly like stinger retirement, with
    /// shutdown joining within the recorder's own stop budget. The other
    /// slot's segment keeps running.
    pub(crate) fn observe_recording_command(
        &mut self,
        command: &CommandMessage,
        result: &CommandResult,
        stored: &StoredProject,
        frame: u64,
    ) {
        let Some((active, slot)) = recording_command_slot(&command.payload) else {
            return;
        };
        if !matches!(result, CommandResult::Accepted { .. }) {
            return;
        }
        self.reap_finished_recording_finalizers();
        if active {
            self.open_pending_recording(slot, stored, frame);
        } else {
            self.recorder_status.stopped(slot);
            self.retire_running_recording(slot, None);
        }
    }

    /// Whether `slot`'s segment is still opening or awaiting mux readiness;
    /// an accepted `RecordStart` for it is not acknowledged until this clears.
    pub(crate) fn recording_settling(&self, slot: usize) -> bool {
        self.recorders
            .get(slot)
            .is_some_and(ProgramRecording::is_settling)
    }

    /// Advances every opening or priming segment without blocking: adopts a
    /// recorder the open worker handed over, feeds it the backlog, and
    /// settles the mux-readiness barrier. Called on every daemon loop pass.
    ///
    /// A failed open releases the slot with its `start`/`rotate` notice; a
    /// failed or timed-out barrier cancels the segment and finalizes it off
    /// the loop, reporting `startup:` exactly as the blocking barrier did.
    pub(crate) fn advance_recordings(&mut self) {
        for slot in 0..RECORDER_SLOTS {
            let ProgramRecording::Running(recorder) = &mut self.recorders[slot] else {
                continue;
            };
            if let OpenPoll::Failed(failure) = recorder.poll_open() {
                self.recorders[slot] = ProgramRecording::Off;
                self.recorder_status
                    .failed(slot, output_failure::recorder_failure(&failure, ""));
                eprintln!("{}", recorder_failure_notice(slot + 1, &failure, false));
                continue;
            }
            recorder.pump_backlog();
            recorder.report_failover();
            let Some(deadline) = recorder.priming else {
                continue;
            };
            let failure = match recorder.startup_decision() {
                Ok(StartupPairDecision::Ready) => {
                    recorder.priming = None;
                    continue;
                }
                Ok(StartupPairDecision::Pending) if Instant::now() < deadline => continue,
                Ok(StartupPairDecision::Pending) => recorder.fail_startup_timeout(),
                Ok(StartupPairDecision::Failed) => {
                    unreachable!("failure is returned as an error")
                }
                Err(error) => error,
            };
            // The barrier failed; latch this slot's attempt unavailable. The
            // failure notice was already emitted by the capture policy or the
            // timeout helper; cancel and report the partial segment.
            recorder.priming = None;
            recorder.request_cancel();
            let code = recorder.capture.code.unwrap_or_else(|| {
                output_failure::recorder_failure(&format!("startup:{failure}"), "")
            });
            self.recorder_status.failed(slot, code);
            self.retire_running_recording(
                slot,
                Some(recorder_failure_notice(
                    slot + 1,
                    &format!("startup:{failure}"),
                    false,
                )),
            );
        }
    }

    /// Begins opening one slot's next segment after an accepted `RecordStart`.
    ///
    /// An unconfigured slot derives nothing and stays untouched. Every failure
    /// latches only THIS slot's attempt unavailable and reports a sanitized
    /// notice naming the slot; the show and the other slot keep running,
    /// exactly like stream sink failures.
    /// Opens `slot`'s segment at frame `frame`, the engine's frame cursor.
    pub(crate) fn open_pending_recording(
        &mut self,
        slot: usize,
        stored: &StoredProject,
        frame: u64,
    ) {
        if self.recorders[slot].is_running() {
            return;
        }
        self.recorder_status.started(slot);
        let names = self.segment_names.clone();
        let segment = match next_slot_segment_path(
            slot,
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
            |candidate| names.taken(candidate),
        ) {
            Ok(Some(segment)) => segment,
            Ok(None) => return,
            Err(error) => {
                let failure = format!("start:{error}");
                self.recorder_status
                    .failed(slot, output_failure::recorder_failure(&failure, ""));
                eprintln!("{}", recorder_failure_notice(slot + 1, &failure, false));
                return;
            }
        };
        let opened = next_record_segment_path(&segment, |candidate| names.taken(candidate))
            .and_then(|segment| {
                NativeProgramRecorder::open(
                    &self.runtime,
                    stored,
                    frame,
                    &segment,
                    &names,
                    self.record_wav,
                    self.record_bitrates[slot],
                    self.record_audio_bitrates[slot],
                    slot + 1,
                )
            });
        match opened {
            Ok(recorder) => self.recorders[slot] = ProgramRecording::Running(Box::new(recorder)),
            Err(error) => {
                self.recorders[slot] = ProgramRecording::Off;
                let failure = format!("start:{error}");
                self.recorder_status
                    .failed(slot, output_failure::recorder_failure(&failure, ""));
                eprintln!("{}", recorder_failure_notice(slot + 1, &failure, false));
            }
        }
    }

    /// Retires one slot's open segment off the render path after an accepted
    /// `RecordStop`, a failed readiness barrier, or a rotation. The worker
    /// drains the sink cleanly unless it was cancelled, so the finalized file
    /// keeps its full `FREEMIXD_RECORDER` report; `notice` follows that report.
    pub(crate) fn retire_running_recording(&mut self, slot: usize, notice: Option<String>) {
        let ProgramRecording::Running(recorder) =
            std::mem::replace(&mut self.recorders[slot], ProgramRecording::Off)
        else {
            return;
        };
        let mut recorder = recorder;
        // The worker waits for its segment so a failed spawn leaves the
        // recorder here for a synchronous bounded stop.
        let (handoff_sender, handoff_receiver) =
            mpsc::sync_channel::<Box<NativeProgramRecorder>>(1);
        let (done_sender, done) = mpsc::sync_channel(1);
        let worker_notice = notice.clone();
        let spawned = thread::Builder::new()
            .name(format!("freemix-native-recording-finalizer-{}", slot + 1))
            .spawn(move || {
                let Ok(mut recorder) = handoff_receiver.recv() else {
                    return;
                };
                let clean = recorder.stop_and_report().is_ok();
                if let Some(notice) = worker_notice {
                    eprintln!("{notice}");
                }
                let _ = done_sender.send(clean);
            });
        match spawned {
            Ok(worker) => {
                let _ = handoff_sender.send(recorder);
                self.recording_finalizers
                    .push(ProgramRecordingFinalizer { done, worker });
            }
            Err(error) => {
                // No worker available: fall back to a synchronous stop that
                // stays within the same stop/kill budget constants the
                // shutdown path uses.
                eprintln!(
                    "{}",
                    recorder_failure_notice(slot + 1, &format!("finalize:{error}"), false)
                );
                let _ = recorder.stop_and_report();
                if let Some(notice) = notice {
                    eprintln!("{notice}");
                }
            }
        }
    }

    /// Reaps detached finalizers that already reported, so repeated
    /// record-stop/start cycles cannot accumulate workers.
    pub(crate) fn reap_finished_recording_finalizers(&mut self) {
        self.recording_finalizers.retain(|finalizer| {
            !matches!(
                finalizer.done.try_recv(),
                Ok(_) | Err(mpsc::TryRecvError::Disconnected)
            )
        });
    }

    /// Rotates every running recorder slot that has reached its own
    /// [`Self::record_segment_seconds`] entry before capturing `sequence`.
    ///
    /// Completed segments are handed off the render path via
    /// [`Self::retire_running_recording`], finished finalizers are reaped to
    /// keep [`Self::recording_finalizers`] bounded, and the newly opened
    /// segment starts at `first_sequence = sequence` so the current frame is
    /// captured immediately with zero dropped frames between segments.
    pub(crate) fn rotate_due_recorders(&mut self, sequence: SequenceNumber) {
        for slot in 0..RECORDER_SLOTS {
            self.rotate_running_recording(slot, sequence);
        }
    }

    pub(crate) fn rotate_running_recording(&mut self, slot: usize, sequence: SequenceNumber) {
        let (next_format, still_priming) = match &self.recorders[slot] {
            ProgramRecording::Running(recorder)
                if recorder
                    .should_rotate(slot_segment_seconds(self.record_segment_seconds, slot)) =>
            {
                // A segment rotated before it confirmed mux readiness hands
                // the pending barrier to its successor, so the held start
                // acknowledgement still waits for realized output.
                let old = &recorder.format;
                (
                    RecordFormat::new(
                        old.dimensions().width(),
                        old.dimensions().height(),
                        old.frame_rate(),
                        old.sample_rate(),
                        old.channel_layout().clone(),
                        sequence,
                    ),
                    recorder.priming.is_some(),
                )
            }
            ProgramRecording::Running(_) | ProgramRecording::Off => return,
        };
        self.reap_finished_recording_finalizers();
        self.retire_running_recording(slot, None);
        let rotated = next_format.map_err(Into::into).and_then(|format| {
            let timecode = recording_start_timecode(sequence.get(), format.frame_rate());
            let names = &self.segment_names;
            let segment = next_slot_segment_path(
                slot,
                self.record_program.as_deref(),
                self.record_program_2.as_deref(),
                |candidate| names.taken(candidate),
            )?
            .ok_or_else(|| AppFailure("rotating recorder slot has no configured output".into()))?;
            // The next segment opens off the render path; the frame captured
            // right after this rotation waits in its backlog until FFmpeg is
            // connected, so no frame is dropped between segments.
            NativeProgramRecorder::open_with_format(
                &self.runtime,
                format,
                timecode,
                &segment,
                names,
                SegmentOpening {
                    wav: self.record_wav,
                    video_bitrate_kbps: self.record_bitrates[slot],
                    audio_bitrate_kbps: self.record_audio_bitrates[slot],
                    slot: slot + 1,
                    failure_label: "rotate",
                    prime_on_open: still_priming,
                },
            )
        });
        match rotated {
            Ok(recorder) => {
                self.recorders[slot] = ProgramRecording::Running(Box::new(recorder));
            }
            Err(error) => {
                self.recorders[slot] = ProgramRecording::Off;
                let failure = format!("rotate:{error}");
                self.recorder_status
                    .failed(slot, output_failure::recorder_failure(&failure, ""));
                eprintln!("{}", recorder_failure_notice(slot + 1, &failure, false));
            }
        }
    }

    pub(crate) fn tick_if_due_collect(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        self.anchor_pacing();
        #[cfg(feature = "display-surface")]
        for display in &mut self.displays {
            display
                .service_control(self.runtime.context())
                .map_err(|error| -> Box<dyn Error> { Box::new(AppFailure(error)) })?;
        }
        self.apply_file_reports(control)?;
        let covered = self.service_playback(control)?;
        let host_deadline = self.next_deadline()?;
        let now = Instant::now();
        if now < host_deadline {
            return Ok(None);
        }
        if !covered {
            return Ok(None);
        }
        self.telemetry.observe_host_lateness(host_deadline, now);
        self.realize_traced(control, server, host_deadline, now, false)
            .map(Some)
    }

    pub(crate) fn wait_and_tick(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        process_shutdown: Option<&ProcessShutdown>,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        self.anchor_pacing();
        let (host_deadline, observed) = loop {
            if requested_daemon_shutdown(Some(&*self), process_shutdown).is_some() {
                return Ok(None);
            }
            self.apply_file_reports(control)?;
            let covered = self.service_playback(control)?;
            let host_deadline = self.next_deadline()?;
            let now = Instant::now();
            if covered && now >= host_deadline {
                break (host_deadline, now);
            }
            // A due frame still waiting for its sources polls briefly
            // instead of spinning, leaving the host to the workers it
            // waits for.
            let host_wait = host_deadline
                .saturating_duration_since(now)
                .max(NATIVE_SOURCE_WAIT_POLL);
            thread::sleep(host_wait.min(NATIVE_IO_POLL_INTERVAL));
        };
        self.telemetry
            .observe_host_lateness(host_deadline, observed);
        self.realize_traced(control, server, host_deadline, observed, true)
            .map(Some)
    }

    /// Realizes the frame due at `deadline`, started at `started`, and
    /// traces it, with its CPU use, when the pacing trace is on; `command`
    /// says a command's frame (after its journal append) rather than an idle
    /// one.
    pub(crate) fn realize_traced(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        deadline: Instant,
        started: Instant,
        command: bool,
    ) -> AppResult<Vec<RuntimeEventMessage>> {
        // The counters are read outside the realization they bracket. The
        // synchronous path collects at once; the pass loop uses the split
        // `begin_frame_if_due`/`finish_frame` instead so peer work runs
        // between the submission and the collect.
        let before = self
            .pacing_trace
            .as_ref()
            .and_then(|_| pacing_trace::CpuCounters::read());
        self.begin_frame(control, server, deadline, started, command, before)?;
        self.finish_frame(control, server)
    }

    /// Begins the frame due now, if any, and leaves its audio and video jobs
    /// with the realizers; [`Self::finish_frame`] collects them after the
    /// pass's peer work.
    pub(crate) fn begin_frame_if_due(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        self.anchor_pacing();
        #[cfg(feature = "display-surface")]
        for display in &mut self.displays {
            display
                .service_control(self.runtime.context())
                .map_err(|error| -> Box<dyn Error> { Box::new(AppFailure(error)) })?;
        }
        self.apply_file_reports(control)?;
        let covered = self.service_playback(control)?;
        let host_deadline = self.next_deadline()?;
        let now = Instant::now();
        if now < host_deadline || !covered {
            return Ok(());
        }
        self.telemetry.observe_host_lateness(host_deadline, now);
        let before = self
            .pacing_trace
            .as_ref()
            .and_then(|_| pacing_trace::CpuCounters::read());
        self.begin_frame(control, server, host_deadline, now, false, before)
    }

    /// Hands the engine every media-file growth, completion, and fault the
    /// file workers reported. A change voids the projected frame, whose media
    /// ticks it moves.
    pub(crate) fn apply_file_reports(
        &mut self,
        control: &mut ControlService<Policy>,
    ) -> AppResult<()> {
        self.apply_live_stills(control)?;
        let reports = self.files.take_reports();
        if reports.is_empty() {
            return Ok(());
        }
        for (input, report) in reports {
            let applied = match report {
                native_files::FileReport::Source { frames, complete } => {
                    control.update_media_source(input, frames, complete)
                }
                native_files::FileReport::Fault(fault) => {
                    eprintln!(
                        "FREEMIXD_MEDIA\tv=1\tinput={input}\tfault={}",
                        fault.token()
                    );
                    control.set_media_fault(input, fault)
                }
                // A file that reopened opens the transport of an input that
                // had no source at startup (closed or offline then); one
                // with a startup source reopened it already. A source the
                // engine refuses faults the input there; it never stops the
                // daemon.
                native_files::FileReport::Opened(source) => {
                    if !control.input_lifecycle(input).closed
                        && !control.media_source_open(input)
                        && let Err(error) = control.set_media_source(input, Some(source))
                    {
                        eprintln!("FREEMIXD_MEDIA\tv=1\tinput={input}\topen_refused\t{error}");
                    }
                    Ok(())
                }
            };
            applied.map_err(|error| {
                AppFailure(format!(
                    "media input {input} cannot take its source report: {error}"
                ))
            })?;
        }
        self.invalidate_projection();
        Ok(())
    }

    /// Opens every media-file input's transport on its decoder's source.
    pub(crate) fn attach_media_sources(
        &mut self,
        control: &mut ControlService<Policy>,
    ) -> AppResult<()> {
        for (input, source) in self.files.sources() {
            control
                .set_media_source(*input, Some(source.clone()))
                .map_err(|error| AppFailure(format!("media input {input} cannot open: {error}")))?;
        }
        self.sync_input_lifecycle(control)
    }

    /// Releases the decoder of every closed media-file input and reopens
    /// it, with its engine transport, once the input reopens (IN-026). A
    /// paused input holds its frame: the engine stops moving its transport.
    ///
    /// Only reopening reopens the transport: a source a fault closed stays
    /// closed (and its input offline) until the input is closed and
    /// reopened, so later commands do not bring back a transport whose
    /// worker no longer decodes.
    ///
    /// An input without a startup source (closed or offline at startup)
    /// reopens its transport once its worker reports the file open, and an
    /// input whose worker failed stays offline.
    pub(crate) fn sync_input_lifecycle(
        &mut self,
        control: &mut ControlService<Policy>,
    ) -> AppResult<()> {
        self.network
            .sync(&control.network_stream_configs(), |input| {
                control.input_lifecycle(input).closed
            });
        self.system_audio
            .sync(&control.system_audio_inputs(), |input| {
                control.input_lifecycle(input).closed
            });
        self.screen_capture
            .sync(&control.screen_capture_inputs(), |input| {
                control.input_lifecycle(input).closed
            });
        if let Some(captures) = &mut self.captures {
            let inputs = control
                .capture_inputs()
                .into_iter()
                .map(|(input, config)| (input, config.stable_key, config.settings))
                .collect::<Vec<_>>();
            captures.sync(&inputs, |input| control.input_lifecycle(input).closed);
        }
        for input in self.files.inputs() {
            let closed = control.input_lifecycle(input).closed;
            let reopened = self.files.is_closed(input) && !closed;
            self.files.set_closed(input, closed);
            if reopened
                && !self.files.is_failed(input)
                && !control.media_source_open(input)
                && let Some(source) = self.files.source(input).cloned()
            {
                control
                    .set_media_source(input, Some(source))
                    .map_err(|error| {
                        AppFailure(format!("media input {input} cannot reopen: {error}"))
                    })?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn service_playback(&mut self, control: &ControlService<Policy>) -> AppResult<bool> {
        #[cfg(target_os = "macos")]
        {
            let video = &mut self.video;
            self.cameras.poll_with(|input, frame| {
                video
                    .ingest_frame(input, frame)
                    .map_err(|error| AppFailure(error).into())
            })?;
        }
        self.titles.set_frame(self.pacer.next_frame().get());
        for (input, frame) in self.titles.take_completed() {
            // A title removed live keeps rendering until the worker learns
            // of it; the realizer ignores a frame for an input it no longer
            // holds.
            self.video.ingest_frame(input, frame).map_err(AppFailure)?;
        }
        // A document's page, rendered off the loop (IN-014).
        for (input, frame) in self.documents.take_completed() {
            self.video.ingest_frame(input, frame).map_err(AppFailure)?;
        }
        for presented in self.files.take_completed() {
            if let Some(frame) = presented.video {
                self.video
                    .ingest_frame(presented.input, frame)
                    .map_err(AppFailure)?;
            }
            if let Some(audio) = &presented.audio {
                self.audio
                    .push_frame_audio(presented.input, presented.frame, audio)
                    .map_err(AppFailure)?;
            }
        }
        let deadline = control.next_frame_deadline()?;
        let video_covered = self
            .video
            .service_playback(deadline)
            .map_err(|error| AppFailure(format!("native playback: {error}")))?;
        if self.projected_frame.is_none() {
            self.projected_frame = Some(control.project_next_frame()?);
        }
        let replay_deadline = self.next_deadline()?;
        let Some(projected) = self.projected_frame.as_ref() else {
            return Err(AppFailure("next frame projection was initialized".into()).into());
        };
        // Replay inputs show their recorded frames and, at 100%, play their
        // recorded audio (OR-012). The video realizer owns the decode
        // players; the control thread pushes their audio before the frame's
        // audio render.
        let audio_handle = &mut self.audio;
        let video = &mut self.video;
        let replay_covered = if self.replay.is_some() {
            let outcome = video
                .present_replay(projected.clone(), replay_deadline)
                .map_err(|error| AppFailure(format!("replay realizer: {error}")))?;
            for (input, block) in outcome.audio {
                if let Err(error) =
                    audio_handle.push_frame_audio(input, projected.frame.get(), &block)
                {
                    eprintln!(
                        "FREEMIXD_REPLAY_FAILURE\tv=1\tinput={input}\tfailure={}",
                        diagnostic_field(&format!("audio:{error}"))
                    );
                }
            }
            outcome.covered
        } else {
            true
        };
        // Network streams hand over the projected frame's picture and audio
        // from their jitter buffers, once per frame; they never wait.
        let captured = self
            .captures
            .as_mut()
            .map(|captures| captures.pull(projected.frame.get()))
            .unwrap_or_default();
        let system_audio = self.system_audio.pull(projected.frame.get());
        let screen_capture = self.screen_capture.pull(projected.frame.get());
        for pulled in self
            .network
            .pull(projected.frame.get())
            .into_iter()
            .chain(captured)
            .chain(system_audio)
            .chain(screen_capture)
        {
            if let Some(frame) = pulled.video {
                self.video
                    .ingest_frame(pulled.input, frame)
                    .map_err(AppFailure)?;
            }
            if let Some(audio) = &pulled.audio {
                self.audio
                    .push_frame_audio(pulled.input, projected.frame.get(), audio)
                    .map_err(AppFailure)?;
            }
        }
        self.titles.observe_frame(projected);
        self.files.observe_frame(projected);
        // An animating title holds the frame until its projected pose is
        // ingested (bounded), so each In and Out frame airs as projected;
        // a media file until its projected tick is presented (bounded).
        let titles_covered = self.titles.covered(projected, self.next_deadline()?);
        let files_covered = self.files.covered(projected, self.next_deadline()?);
        let audio_covered = self
            .audio
            .service_next_frame(projected, &self.project_plan)
            .map_err(|error| AppFailure(format!("native audio: {error}")))?;
        let stinger_covered = match self.project_plan.stinger_frame_request(projected)? {
            Some(request) => self
                .video
                .service_stinger(request.input, request.deadline)
                .map_err(|error| AppFailure(format!("native stinger: {error}")))?,
            None => true,
        };
        Ok(audio_covered
            && video_covered
            && stinger_covered
            && titles_covered
            && files_covered
            && replay_covered)
    }

    /// Attaches the grades of `control`'s current revision to the project
    /// plan, baking and uploading them only when they changed.
    pub(crate) fn compiled_grades(
        &mut self,
        control: &ControlService<Policy>,
    ) -> (Arc<fm_engine::CompiledProjectGrades>, u64) {
        let compiled = self.grade_cache.at(control);
        let revision = control.snapshot().snapshot.revision;
        (compiled, revision)
    }

    /// The projected frame's slideshow slides and the next ones to decode
    /// ahead, for the video realizer (IN-009).
    pub(crate) fn slideshow_job(
        &self,
        control: &ControlService<Policy>,
    ) -> Option<native_video_realizer::SlideshowJob> {
        let frame = self.projected_frame.as_ref()?;
        if frame.slideshows.is_empty() {
            return None;
        }
        let mut frames = Vec::with_capacity(frame.slideshows.len());
        let mut ahead = Vec::new();
        for slideshow in &frame.slideshows {
            let Some(status) = control.slideshow_status(slideshow.input) else {
                continue;
            };
            let fit = status.config.fit;
            let listing = control.slideshow_listing(slideshow.input);
            if let Some(next) = usize::try_from(slideshow.page.index)
                .ok()
                .and_then(|index| listing.get((index + 1) % listing.len().max(1)))
            {
                ahead.push((std::sync::Arc::clone(&next.asset_uri), fit));
            }
            frames.push((slideshow.clone(), fit));
        }
        Some(native_video_realizer::SlideshowJob { frames, ahead })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn begin_frame(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        deadline: Instant,
        started: Instant,
        command: bool,
        trace_before: Option<pacing_trace::CpuCounters>,
    ) -> AppResult<()> {
        // Accepted StreamStart commands start on this frame boundary, before
        // the frame is produced, so the first fed pair is the next real frame.
        self.start_pending_streams();
        let (compiled_grades, grade_revision) = self.compiled_grades(control);
        let slideshow_job = self.slideshow_job(control);
        let shows_preview = self.display_shows_preview();
        self.project_plan
            .refresh_scene_models(|scene| control.scene_model(scene));
        let video = &mut self.video;
        let audio_handle = &mut self.audio;
        let monitor_output = &mut self.monitor_output;
        let aux_bus_outputs = &mut self.aux_bus_outputs;
        let aux_bus_omt = &mut self.aux_bus_omt;
        let project_plan = &mut self.project_plan;
        let projected_frame = self.projected_frame.as_ref();
        let mut fade_to_black_excluded = Vec::new();
        let monitor_interval = self.monitor_interval;
        let monitor_demand = self.monitor_demand;
        let mut monitor_frame = None;
        let mut preview_source = None;
        let replay_inputs = if self.replay.is_some() {
            control.replay_settings().sources.clone()
        } else {
            Vec::new()
        };
        let outcome = control.tick_with_realizer(server, |frame| {
            debug_assert_eq!(projected_frame, Some(frame));
            monitor_frame = Some((frame.frame.get(), frame.program.primary));
            // The Preview monitor tap composes Preview only on its due frames.
            if shows_preview
                || monitor_due(
                    monitor_interval,
                    monitor_demand,
                    MonitorTap::Preview,
                    frame.frame.get(),
                )
            {
                preview_source = Some(frame.clone());
            }
            for &(input, state) in &frame.input_audio_strip_updates {
                let strip = model_audio_strip_state(state);
                project_plan
                    .set_input_audio_strip(input, strip)
                    .map_err(NativeRealizationError::Audio)?;
            }
            for update in &frame.audio_mixer_updates {
                match *update {
                    fm_engine::EngineAudioMixerUpdate::Membership { input, group } => project_plan
                        .set_input_audio_group(input, group)
                        .map_err(NativeRealizationError::Audio)?,
                    fm_engine::EngineAudioMixerUpdate::GroupLevel { group, state } => {
                        project_plan.set_audio_group_level(group, state.gain_millidb, state.muted);
                    }
                    fm_engine::EngineAudioMixerUpdate::GroupSolo { group, soloed } => {
                        project_plan.set_audio_group_solo(group, soloed);
                    }
                    fm_engine::EngineAudioMixerUpdate::GroupParent { group, parent } => {
                        project_plan.set_audio_group_parent(group, parent);
                    }
                    fm_engine::EngineAudioMixerUpdate::SoloMode(mode) => {
                        project_plan.set_audio_solo_mode(mode);
                    }
                    fm_engine::EngineAudioMixerUpdate::AuxBusLevel { bus, state } => {
                        project_plan.set_aux_bus_level(bus, state.gain_millidb, state.muted);
                    }
                    fm_engine::EngineAudioMixerUpdate::AuxBusDsp { bus, ref chain } => {
                        project_plan.set_aux_bus_dsp(bus, **chain);
                    }
                    fm_engine::EngineAudioMixerUpdate::AuxBusDevice { bus, ref device } => {
                        aux_bus_outputs[bus.index()].set_device(device.clone());
                    }
                    fm_engine::EngineAudioMixerUpdate::AuxBusOmt { bus, omt } => {
                        set_aux_bus_omt_sender(&mut aux_bus_omt[bus.index()], bus, omt);
                    }
                    fm_engine::EngineAudioMixerUpdate::OutputAuxBus { output, bus } => {
                        project_plan.set_output_aux_bus(output, bus);
                    }
                    fm_engine::EngineAudioMixerUpdate::OutputMixMinus { output, input } => {
                        project_plan.set_output_mix_minus(output, input);
                    }
                    fm_engine::EngineAudioMixerUpdate::MonitorDevice(ref device) => {
                        monitor_output.set_device(device.clone());
                    }
                    fm_engine::EngineAudioMixerUpdate::MonitorLevel(state) => {
                        monitor_output.set_level(state.gain_millidb, state.muted);
                    }
                    fm_engine::EngineAudioMixerUpdate::StripDsp { target, chain } => project_plan
                        .set_audio_dsp(target, chain)
                        .map_err(NativeRealizationError::Audio)?,
                    // The runtime applied the chain to its plugin racks; the
                    // meter restarted above and the plan holds no meters.
                    fm_engine::EngineAudioMixerUpdate::PluginChain { .. }
                    | fm_engine::EngineAudioMixerUpdate::LoudnessReset { .. } => {}
                }
            }
            // The mixer starts before the GPU compose and its outcome is
            // collected after it, so the two overlap.
            audio_handle
                .submit_render(native_audio_realizer::AudioRenderJob {
                    frame: frame.clone(),
                    plan: project_plan.clone(),
                    strip_updates: frame.input_audio_strip_updates.clone(),
                    mixer_updates: frame.audio_mixer_updates.clone(),
                    replay_inputs: replay_inputs.clone(),
                })
                .map_err(NativeRealizationError::RealizerThread)?;
            video
                .submit(native_video_realizer::VideoRenderJob {
                    frame: frame.clone(),
                    plan: project_plan.clone(),
                    grades: Arc::clone(&compiled_grades),
                    grade_revision,
                    slides: slideshow_job,
                    monitor: native_video_realizer::MonitorJob {
                        server: server.clone(),
                        program_frame: monitor_frame,
                        preview_tap_frame: None,
                        preview: frame.program.primary,
                        demand: monitor_demand,
                    },
                })
                .map_err(NativeRealizationError::RealizerThread)?;
            fade_to_black_excluded.clone_from(&frame.fade_to_black_excluded_outputs);
            Ok::<(), NativeRealizationError>(())
        })?;
        self.pending_frame = Some(PendingFrame {
            deadline,
            started,
            command,
            monitor_frame,
            preview_source,
            shows_preview,
            fade_to_black_excluded,
            trace_before,
            events: outcome.runtime_events,
        });
        Ok(())
    }

    /// Collects the frame `begin_frame` submitted and runs its consumers,
    /// after the pass serviced its peers.
    ///
    /// # Errors
    ///
    /// Returns a realizer or consumer failure.
    #[allow(clippy::too_many_lines)] // one pass over the frame's consumers
    pub(crate) fn finish_frame(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>> {
        let Some(pending) = self.pending_frame.take() else {
            return Ok(Vec::new());
        };
        let PendingFrame {
            deadline,
            started: frame_started,
            command,
            monitor_frame,
            preview_source,
            shows_preview,
            fade_to_black_excluded,
            trace_before,
            events,
        } = pending;
        let finish_started = Instant::now();
        let (preview_grades, preview_grade_revision) = self.compiled_grades(control);
        let native_video_realizer::VideoRenderOutcome {
            result,
            monitor_frames,
            monitor_counters,
        } = self
            .video
            .collect()
            .map_err(NativeRealizationError::RealizerThread)?;
        self.apply_monitor_outcome(monitor_frames, monitor_counters);
        match result.map_err(|error| match error {
            native_video_realizer::VideoError::Render(error) => {
                NativeRealizationError::Video(error)
            }
            error @ native_video_realizer::VideoError::Grades(_) => {
                NativeRealizationError::RealizerThread(error.to_string())
            }
        })? {
            native_video_realizer::NativeVideoFrame::Program(output) => {
                self.latest_output = Some(output);
                self.latest_project_outputs.clear();
            }
            native_video_realizer::NativeVideoFrame::Outputs(outputs) => {
                self.latest_project_outputs = outputs;
                self.latest_output = None;
            }
        }
        let mixed = self
            .audio
            .collect_render()
            .map_err(NativeRealizationError::RealizerThread)?;
        let audio = Some(mixed.block);
        let live_audio = mixed.live;
        let override_audio = mixed.overrides;
        let audio_meters = mixed.meters;
        let rendered_audio = mixed.rendered;

        if let Some(meters) = audio_meters {
            let sequence = self
                .audio_meter_sequence
                .checked_add(1)
                .ok_or_else(|| AppFailure("audio meter sequence exhausted".into()))?;
            let samples = usize::try_from(meters.end_sample - meters.start_sample)
                .map_err(|_| AppFailure("audio meter interval exceeds memory".into()))?;
            self.monitor_output
                .push(&meters.monitor_planes, samples, meters.sample_rate);
            for (index, output) in self.aux_bus_outputs.iter_mut().enumerate() {
                let start = index * meters.channels;
                output.push(
                    &meters.aux_planes[start..start + meters.channels],
                    samples,
                    meters.sample_rate,
                );
            }
            // Published aux buses go out as OMT audio at their fixed ports,
            // timestamped from the interval's first sample (AU-002, 2.138).
            let timestamp = i64::try_from(
                u128::from(meters.start_sample) * 10_000_000
                    / u128::from(meters.sample_rate.max(1)),
            )
            .unwrap_or(i64::MAX);
            for (index, sender) in self.aux_bus_omt.iter().enumerate() {
                let Some(sender) = sender else { continue };
                let start = index * meters.channels;
                let planes = meters.aux_planes[start..start + meters.channels].to_vec();
                if let Ok(frame) = fm_io_omt::AudioFrame::new(meters.sample_rate, planes) {
                    let _ = sender.send_audio(&frame.at(timestamp));
                }
            }
            let output = self.monitor_output.state();
            self.pending_audio_meters = Some(audio_meters_message(
                server,
                sequence,
                &AudioMeterView::from(&meters),
                output,
            ));
            self.audio_meter_sequence = sequence;
        }
        self.install_stinger_mutation()?;
        self.projected_frame = None;
        self.pacer.advance()?;
        let stream_audio = if self.streams.is_empty() {
            None
        } else {
            audio
                .clone()
                .map(|audio| (audio, live_audio.clone(), override_audio.clone()))
        };
        // The LiveLAN stream (OR-017) takes the frame its source shows, with
        // the audio that output hears.
        let next_sequence = audio
            .as_ref()
            .map(|audio| audio.timing().sequence().get().saturating_add(1));
        let live_lan_capture = self
            .live_lan
            .as_ref()
            .and_then(native_live_lan::NativeLiveLan::capturing)
            .and_then(|source| {
                audio
                    .clone()
                    .map(|audio| (source.output(), audio, live_audio.clone()))
            });
        // Streams read back first and recorders after, so a recorder taking
        // the same picture as a stream group takes the group's readback
        // instead of reading it again.
        if let Some(audio) = &audio {
            self.rotate_due_recorders(audio.timing().sequence());
        }
        let mut readbacks = FrameReadbacks::default();
        if let Some((audio, live, overrides)) = stream_audio {
            Self::capture_streams(
                &mut self.streams,
                &mut self.stream_groups,
                &mut self.output_set,
                &self.runtime,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                &mut readbacks,
                &CaptureAudio {
                    faded: audio,
                    live,
                    excluded: &fade_to_black_excluded,
                    overrides: &overrides,
                },
            );
        }
        if let Some(audio) = audio {
            Self::capture_recorders(
                &mut self.recorders,
                &self.record_outputs,
                &self.runtime,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                &mut readbacks,
                &CaptureAudio {
                    faded: audio,
                    live: live_audio,
                    excluded: &fade_to_black_excluded,
                    overrides: &override_audio,
                },
            );
        }
        if let Some((output, audio, live)) = live_lan_capture
            && let Some(live_lan) = &mut self.live_lan
        {
            let block = CaptureAudio {
                faded: audio,
                live,
                excluded: &fade_to_black_excluded,
                overrides: &override_audio,
            }
            .for_output(output, &self.latest_project_outputs)
            .clone();
            live_lan.capture(
                &self.runtime,
                resolve_project_output_texture(
                    output,
                    &self.latest_project_outputs,
                    self.latest_output.as_ref(),
                ),
                block,
            );
        }
        if let (Some(live_lan), Some(sequence)) = (&mut self.live_lan, next_sequence) {
            live_lan.sync(control.live_lan(), &self.runtime, sequence);
        }
        // The replay recording (OR-012) takes every source's newest picture
        // and its own audio of this frame.
        if let (Some(replay), Some((frame, _))) = (&mut self.replay, monitor_frame) {
            let latest = self.latest_frames.snapshot();
            replay.record(
                control,
                frame,
                &|input| latest.get(&input).cloned(),
                &|input| rendered_audio.get(&input).cloned(),
            );
        }
        // Highlight reel exports (OR-013) take their frames from the engine
        // on the frame loop; the encoder runs off it.
        if let Some(reel) = &mut self.reel {
            reel.sync(control);
        }
        // Display outputs and the Preview monitor tap follow Program's own
        // consumers, so a Preview composite never delays them. A loop that is
        // behind its frame deadlines drops display frames and Preview tap
        // frames rather than delaying the next Program frame and the
        // sessions served between frames.
        let started = Instant::now();
        let behind = started >= self.next_deadline()?;
        let display_due = !self.display_presenting() || self.display_pacing.due(started, behind);
        let realized_preview = control.snapshot().snapshot.realized_preview.to_domain();
        let preview = preview_source.as_ref().map_or(realized_preview, |frame| {
            preview_monitor_input(frame, realized_preview)
        });
        let preview_tap_frame = preview_source
            .as_ref()
            .map(|frame| frame.frame.get())
            .filter(|&frame| {
                monitor_due(
                    self.monitor_interval,
                    self.monitor_demand,
                    MonitorTap::Preview,
                    frame,
                )
            });
        let preview_tap_frame = match preview_tap_frame {
            Some(_) if !self.preview_pacing.due(started, behind) => {
                self.monitor_control_skips = self.monitor_control_skips.saturating_add(1);
                None
            }
            due => due,
        };
        if preview_composite_due(shows_preview, display_due, preview_tap_frame.is_some()) {
            // The composite waits for the pass's slack (see
            // `service_deferred_preview`): a display that shows Preview keeps
            // its last picture for a frame rather than delaying Program.
            if let Some(frame) = preview_source {
                self.pending_preview
                    .submit(native_realizer::NativePreviewJob {
                        frame,
                        preview,
                        tap_frame: preview_tap_frame,
                        plan: self.project_plan.clone(),
                        grades: Arc::clone(&preview_grades),
                        grade_revision: preview_grade_revision,
                    });
            }
        } else if !shows_preview {
            self.latest_preview = None;
            self.pending_preview.clear();
        }
        if preview_tap_frame.is_some() {
            self.preview_pacing.presented(started, Instant::now());
        }
        if display_due {
            self.present_display_outputs()?;
            self.display_pacing.presented(started, Instant::now());
        } else if let Some(host) = &mut self.display_inventory {
            // Hot-plug is still followed on a dropped frame.
            host.poll(&self.runtime, started);
        }
        let now = Instant::now();
        self.sample_local_output_status(now);
        self.advance_stream_routes(server)?;
        self.sample_disk_space(now);
        self.observe_native_telemetry();
        if let Some(trace) = &mut self.pacing_trace {
            let finished = Instant::now();
            let counters = trace_before.zip(pacing_trace::CpuCounters::read());
            trace.frame(
                deadline,
                frame_started,
                (finish_started, finished),
                counters,
                command,
            );
        }
        Ok(events)
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn shutdown_requested(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(display) = self.displays.first() {
            return display.shutdown_requested();
        }
        false
    }

    /// Finalizes every open segment, then joins every detached finalizer
    /// within the recorder's own stop budget so each runtime-stopped segment
    /// has emitted its `FREEMIXD_RECORDER` report before exit.
    ///
    /// Slots finalize independently: one slot's failure is collected and
    /// returned after every other slot has had its bounded stop, never
    /// aborting the remaining teardown.
    pub(crate) fn finalize_recorder(&mut self) -> AppResult<()> {
        let mut first_error = None;
        for recorder in std::mem::take(&mut self.recording_finalizers) {
            let result = match recorder
                .done
                .recv_timeout(PROGRAM_RECORDER_FINALIZE_JOIN_TIMEOUT)
            {
                Ok(clean) => {
                    let ProgramRecordingFinalizer { worker, .. } = recorder;
                    let _ = worker.join();
                    if clean {
                        Ok(())
                    } else {
                        Err(AppFailure("Program recording did not finalize cleanly".into()).into())
                    }
                }
                Err(_) => {
                    // The worker is still inside its bounded stop budget;
                    // detach rather than block shutdown past the budget.
                    Err(AppFailure("program recording finalization did not complete".into()).into())
                }
            };
            if result.is_err() {
                first_error.get_or_insert(result);
            }
        }
        let mut stopped = Vec::new();
        for slot in &mut self.recorders {
            if let ProgramRecording::Running(mut recorder) =
                std::mem::replace(slot, ProgramRecording::Off)
            {
                if let Err(error) = recorder.stop_and_report() {
                    first_error.get_or_insert(Err(error));
                }
                stopped.push(recorder);
            }
        }
        if !stopped.is_empty() {
            self.telemetry
                .observe_recorders(stopped.iter().map(|recorder| &**recorder));
        }
        match first_error {
            Some(result) => result,
            None => Ok(()),
        }
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn finalize_cameras(&mut self) -> AppResult<()> {
        #[cfg(target_os = "macos")]
        {
            self.cameras.shutdown()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }

    #[cfg(feature = "display-surface")]
    pub(crate) fn program_telemetry(&self) -> Option<fm_gpu::PresentationTelemetry> {
        self.displays
            .first()
            .map(program_surface::ProgramPresentation::telemetry)
    }

    pub(crate) fn observe_native_telemetry(&mut self) {
        self.telemetry.observe_audio(self.audio.snapshot());
        #[cfg(target_os = "macos")]
        if !self.camera_telemetry_frozen {
            self.telemetry.camera = aggregate_camera_telemetry(&self.cameras.source_telemetry());
        }
        let running_recorders = self.recorders.iter().filter_map(|slot| match slot {
            ProgramRecording::Running(recorder) => Some(&**recorder),
            ProgramRecording::Off => None,
        });
        self.telemetry.observe_recorders(running_recorders);
        let streams_enqueued = self
            .streams
            .iter()
            .map(|stream| stream.ledger.enqueued_pairs)
            .fold(0_u64, u64::saturating_add);
        let streams_dropped = self
            .streams
            .iter()
            .map(|stream| stream.ledger.dropped_pairs)
            .fold(0_u64, u64::saturating_add);
        self.telemetry
            .observe_streams(streams_enqueued, streams_dropped);
        // The disk sample lags telemetry by at most one frame; `None` until
        // the first sample keeps the record honest about "never sampled".
        self.telemetry.disk_free_bytes = self.disk_free_bytes;
        self.telemetry.disk_alert = self.disk_free_bytes.map(|_| self.disk_alert.state());
        self.telemetry.observe_gpu(self.runtime.context());
    }

    /// Drains the frame loop's observations since the previous call and
    /// reports its whole-run totals, the active stream targets' network
    /// state, the live sources' clock sync, and the running encoders' queues
    /// for the statistics registry (RC-015).
    pub(crate) fn take_statistics_sample(&mut self) -> statistics::NativeStatisticsSample {
        let rate = self.pacer.frame_rate();
        let frame_interval_ms =
            1_000.0 * f64::from(rate.denominator()) / f64::from(rate.numerator().max(1));
        let display_dropped = self
            .display_output_totals()
            .map_or(0, |(_, _, dropped)| dropped);
        let streams_failed = self
            .streams
            .iter()
            .filter(|stream| {
                stream.first_failure.is_some()
                    || matches!(
                        self.output_set.state(stream.destination_id),
                        Some(DestinationState::Failed)
                    )
            })
            .count();
        let recorders_failed = self
            .recorders
            .iter()
            .filter(|slot| match slot {
                ProgramRecording::Running(recorder) => recorder.capture.first_failure.is_some(),
                ProgramRecording::Off => false,
            })
            .count();
        let network = network_sample(&self.output_set, &self.streams);
        let sync_samples = self.source_sync_samples();
        let sync = statistics::SyncSample::from_samples(&sync_samples);
        let encoder = encoder_sample(&self.recorders, &self.streams);
        let telemetry = &mut self.telemetry;
        let tap = |tap: MonitorTap| {
            let counters = telemetry.monitor_taps[tap.index()];
            statistics::TapTotals {
                published: counters.published,
                failed: counters.failed,
                skipped: counters.skipped,
            }
        };
        let monitor = [tap(MonitorTap::Program), tap(MonitorTap::Preview)];
        statistics::NativeStatisticsSample {
            frame_interval_ms,
            lateness_ms: std::mem::take(&mut telemetry.pending_lateness_ms),
            gpu_pass_ms: std::mem::take(&mut telemetry.pending_gpu_pass_ms),
            gpu_timing: matches!(
                telemetry.gpu_support,
                fm_gpu::NativeFullscreenTimingSupport::Supported
            ),
            observations_dropped: telemetry.pending_dropped,
            frames_dropped: telemetry
                .stream_dropped_pairs_total
                .saturating_add(display_dropped),
            audio_sink_depth: telemetry.audio_sink_depth,
            audio_sink_dropped: telemetry.audio_sink_dropped,
            audio_source_stalls: telemetry.audio_source_stalls,
            audio_retained_bytes: telemetry.audio_retained_bytes,
            recorder_outstanding_pairs: telemetry.recorder_outstanding_pairs,
            recorder_retained_bytes: telemetry.recorder_retained_bytes,
            stream_enqueued_pairs: telemetry.stream_enqueued_pairs_total,
            stream_dropped_pairs: telemetry.stream_dropped_pairs_total,
            monitor,
            camera_dropped: telemetry
                .camera
                .native_dropped
                .saturating_add(telemetry.camera.queue_dropped),
            disk_free_bytes: telemetry.disk_free_bytes,
            outputs_failed: u64::try_from(streams_failed.saturating_add(recorders_failed))
                .unwrap_or(u64::MAX),
            network,
            sync,
            encoder,
        }
    }

    pub(crate) fn emit_telemetry(&mut self) {
        if self.telemetry_emitted {
            return;
        }
        self.emit_camera_source_telemetry();
        self.observe_native_telemetry();
        if !self.files.inputs().is_empty() {
            eprintln!(
                "FREEMIXD_MEDIA_FILES\tv=1\tinputs={}\tlate_frames={}",
                self.files.inputs().len(),
                self.files.late_frames()
            );
        }
        #[cfg(feature = "display-surface")]
        let presentation = self
            .displays
            .first_mut()
            .map(program_surface::ProgramPresentation::telemetry_for_shutdown);
        #[cfg(feature = "display-surface")]
        for display in self.displays.iter_mut().skip(1) {
            let _ = display.telemetry_for_shutdown();
        }
        #[cfg(not(feature = "display-surface"))]
        let presentation = None;
        self.telemetry.emit(presentation);
        if let Some(trace) = &self.pacing_trace {
            eprintln!("{}", trace.record());
        }
        self.telemetry_emitted = true;
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn emit_camera_source_telemetry(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if self.camera_telemetry_frozen {
                return;
            }
            if let Some(sources) = self.cameras.emit_source_telemetry() {
                self.telemetry.camera = aggregate_camera_telemetry(&sources);
            }
            self.camera_telemetry_frozen = true;
        }
    }

    /// Starts the pacer's clock at the first frame the loop asks for (see
    /// [`Self::pacing_origin`]); later calls keep it.
    pub(crate) fn anchor_pacing(&mut self) {
        if self.pacing_origin.is_none() {
            self.pacing_origin = Some(Instant::now());
            if let Some(trace) = &mut self.pacing_trace {
                trace.start();
            }
        }
    }

    /// Whether a client command dispatched now leaves its frame on time.
    ///
    /// A native command is journaled and fsynced before the engine commits
    /// it, and is realized on the next frame, so the fsync must end before
    /// that frame's deadline. The window is open while more of the frame
    /// interval is left before the next deadline than the longest of the
    /// journal's recent appends took (`append_peak`, plus a margin for
    /// staging the command; half an interval before any append was timed).
    /// Otherwise the command waits for the frame to be realized (a whole
    /// interval is then left), so it is realized one frame later instead of
    /// making the frame late; the window reopens once that frame was
    /// realized even if realizing it took most of the interval, so no
    /// command waits longer than one frame. On a quick disk almost no
    /// command waits; on a contended one, commands take the frame after
    /// the next boundary. Daemon-issued commands (playlist and cue clocks,
    /// end-of-media triggers) keep their scheduled frames and never ask.
    ///
    /// The journal contract is unchanged: the command is still prepared on
    /// the state it is realized on, durable before it is committed, and
    /// realized on the frame right after its append.
    pub(crate) fn command_window(&mut self, append_peak: Option<Duration>) -> bool {
        let mut deferred = self.command_deferred_at.take();
        let open = self.window_open(append_peak, &mut deferred);
        self.command_deferred_at = deferred;
        open
    }

    /// Whether a daemon clock's retry of a refused advance may run now: the
    /// same window as [`Self::command_window`], since an accepted retry is
    /// journaled and fsynced before its frame like a client command. Only a
    /// clock's first attempt keeps its scheduled frame without asking; a
    /// retry is already late, so it waits (at most one frame) rather than
    /// make a frame late. Its hold is its own, so asking for a retry never
    /// lets a client command skip its wait.
    pub(crate) fn retry_window(&mut self, append_peak: Option<Duration>) -> bool {
        let mut deferred = self.retry_deferred_at.take();
        let open = self.window_open(append_peak, &mut deferred);
        self.retry_deferred_at = deferred;
        open
    }

    /// [`command_window_open`] for the pacer's next deadline, holding with
    /// `deferred`.
    pub(crate) fn window_open(
        &self,
        append_peak: Option<Duration>,
        deferred: &mut Option<FrameNumber>,
    ) -> bool {
        let Ok(deadline) = self.next_deadline() else {
            return true;
        };
        let rate = self.pacer.frame_rate();
        let interval = Duration::from_nanos(
            1_000_000_000_u64.saturating_mul(u64::from(rate.denominator()))
                / u64::from(rate.numerator()),
        );
        command_window_open(
            deadline.saturating_duration_since(Instant::now()),
            interval,
            append_peak,
            self.pacer.next_frame(),
            deferred,
        )
    }

    /// When the pacer's next frame is due. Before the frame loop first runs
    /// ([`Self::anchor_pacing`]) the next frame is due now.
    pub(crate) fn next_deadline(&self) -> AppResult<Instant> {
        let elapsed_offset = host_deadline_offset(&self.pacer)?
            .checked_sub(self.pacer_start_offset)
            .ok_or_else(|| {
                AppFailure("native media pacer moved before its restored cursor".into())
            })?;
        let origin = self.pacing_origin.unwrap_or_else(Instant::now);
        origin.checked_add(elapsed_offset).ok_or_else(|| {
            AppFailure("native media host deadline exceeds Instant range".into()).into()
        })
    }
}

#[cfg(feature = "native-media")]
impl Drop for NativeDaemon {
    fn drop(&mut self) {
        if let Some(mutation) = self.pending_stinger_mutation.take() {
            let _ = self.stinger_retirements.discard(mutation);
        }
        for result in self.finalize_streams() {
            // Drop must never panic; the serve path reports these results.
            drop(result);
        }
        let _ = self.finalize_recorder();
        self.emit_camera_source_telemetry();
        #[cfg(target_os = "macos")]
        let _ = self.cameras.shutdown();
        self.emit_telemetry();
    }
}

#[cfg(not(feature = "native-media"))]
pub(crate) struct NativeDaemon;

#[cfg(not(feature = "native-media"))]
impl NativeDaemon {
    #[allow(clippy::unused_self)]
    pub(crate) fn invalidate_projection(&mut self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn attach_media_sources(
        &mut self,
        _control: &mut ControlService<Policy>,
    ) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    pub(crate) fn attach_replay(
        &mut self,
        _control: &mut ControlService<Policy>,
        _root: PathBuf,
        _settings: &fm_model::ProjectSettings,
    ) {
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    pub(crate) fn attach_reel(&mut self, _settings: &fm_model::ProjectSettings) {}

    pub(crate) fn start(
        _store: &ProjectStore,
        _stored: &StoredProject,
        _camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    pub(crate) fn attach_display_inventory(&mut self, _inventory: PathBuf) -> AppResult<()> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn sync_display_outputs(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn report_network_streams(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_audio_meters(&mut self) -> Option<AudioMetersMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_stream_status(&mut self) -> Option<StreamStatusMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn pick_screen_capture(&mut self, _input: InputId) -> bool {
        false
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_recorder_status(
        &mut self,
        _server: &ServerIdentity,
    ) -> Option<RecorderStatusMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn source_sync_samples(&self) -> Vec<fm_protocol::SourceSyncSample> {
        Vec::new()
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_statistics_sample(&mut self) -> statistics::NativeStatisticsSample {
        statistics::NativeStatisticsSample::default()
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_source_sync_status(
        &mut self,
        _server: &ServerIdentity,
    ) -> Option<fm_protocol::SourceSyncStatusMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn set_monitor_demand(&mut self, _demand: bool) {}

    #[allow(clippy::unused_self)]
    pub(crate) fn live_source_frames(&self) -> scene_preview::LiveSourceFrames {
        scene_preview::LiveSourceFrames::default()
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn take_monitor_frames(&mut self) -> Vec<(MonitorTap, Vec<u8>)> {
        Vec::new()
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn begin_frame_if_due(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn finish_frame(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn wait_and_tick(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
        _process_shutdown: Option<&ProcessShutdown>,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn shutdown_requested(&self) -> bool {
        false
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn recording_settling(&self, _slot: usize) -> bool {
        false
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn command_window(&mut self, _append_peak: Option<Duration>) -> bool {
        true
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn retry_window(&mut self, _append_peak: Option<Duration>) -> bool {
        true
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn advance_recordings(&mut self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn service_deferred_preview(&mut self, _server: &ServerIdentity) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, dead_code)]
    pub(crate) fn tick_if_due(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn poll_deferred_preview(&mut self) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn start_and_prime_configured_recorders(
        &mut self,
        _stored: &StoredProject,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
        _shutdown: Option<&ProcessShutdown>,
        _before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        Ok(true)
    }

    #[allow(clippy::unused_self, clippy::too_many_arguments)]
    pub(crate) fn configure_recordings(
        &mut self,
        _primary: Option<PathBuf>,
        _primary_output: Option<fm_types::OutputId>,
        _secondary: Option<PathBuf>,
        _secondary_output: Option<fm_types::OutputId>,
        _wav: bool,
        _segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
        _primary_bitrate: Option<u32>,
        _primary_audio_bitrate: Option<u32>,
        _secondary_bitrate: Option<u32>,
        _secondary_audio_bitrate: Option<u32>,
    ) {
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn recording_configured(&self) -> bool {
        false
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    pub(crate) fn configure_record_fallback(&mut self, _fallback: Option<PathBuf>) {}

    #[allow(clippy::unused_self)]
    pub(crate) fn recover_recording_sessions(&self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn finalize_recorder(&mut self) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn prime_startup_streams(&mut self, _stored: &StoredProject) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn finalize_streams(&mut self) -> Vec<AppResult<()>> {
        Vec::new()
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn finalize_cameras(&mut self) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn emit_camera_source_telemetry(&mut self) {}

    #[allow(clippy::unused_self)]
    pub(crate) fn emit_telemetry(&mut self) {}
}
