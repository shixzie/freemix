//! The daemon's serve loop: storying the program setup, serving the control
//! sessions, workers, and display outputs, and persisting display overrides.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

pub(crate) enum NativeServeMode {
    Disabled,
    Headless {
        camera_helper: Option<PathBuf>,
        /// Presents the display outputs to a diagnostic inventory host.
        display_inventory: Option<PathBuf>,
        /// `--display-output` slots replacing the project's on load.
        display_overrides: Vec<DisplayOutputConfig>,
    },
    #[cfg(feature = "display-surface")]
    Program(Box<ProgramServeSetup>),
}

impl NativeServeMode {
    /// The `--display-output` slots replacing the project's on load.
    pub(crate) fn display_overrides(&self) -> &[DisplayOutputConfig] {
        match self {
            Self::Disabled => &[],
            Self::Headless {
                display_overrides, ..
            } => display_overrides,
            #[cfg(feature = "display-surface")]
            Self::Program(setup) => &setup.display_overrides,
        }
    }
}

#[cfg(feature = "display-surface")]
pub(crate) struct ProgramServeSetup {
    pub(crate) context: NativeContext,
    pub(crate) outputs: Vec<program_surface::DisplayPresentationSetup>,
    pub(crate) windowed: program_surface::WindowedDisplays,
    pub(crate) display_overrides: Vec<DisplayOutputConfig>,
    pub(crate) camera_helper: Option<PathBuf>,
}

/// The daemon's bound sockets: control, optional web gateway, optional status,
/// and optional HLS.
#[derive(Clone)]
pub(crate) struct ServeListeners {
    pub(crate) listen: SocketAddr,
    pub(crate) web_listen: Option<SocketAddr>,
    /// The gateway's TLS, state directory, and host names.
    pub(crate) web: web::WebSettings,
    pub(crate) status_listen: Option<SocketAddr>,
    pub(crate) hls_listen: Option<SocketAddr>,
    pub(crate) live_lan_listen: Option<SocketAddr>,
}

#[cfg(feature = "display-surface")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn serve_program_worker(
    path: &Path,
    listen: SocketAddr,
    web_listen: Option<SocketAddr>,
    web: web::WebSettings,
    once: bool,
    context: NativeContext,
    outputs: Vec<program_surface::DisplayPresentationSetup>,
    windowed: program_surface::WindowedDisplays,
    display_overrides: Vec<DisplayOutputConfig>,
    camera_helper: Option<PathBuf>,
    record_program: Option<PathBuf>,
    record_output: Option<fm_types::OutputId>,
    record_program_2: Option<PathBuf>,
    record_output_2: Option<fm_types::OutputId>,
    record_wav: bool,
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    record_bitrate_kbps: Option<u32>,
    record_bitrate_kbps_2: Option<u32>,
    record_audio_bitrate_kbps: Option<u32>,
    record_audio_bitrate_kbps_2: Option<u32>,
) -> AppResult<()> {
    serve_inner(
        path,
        ServeListeners {
            listen,
            web_listen,
            web,
            status_listen: None,
            hls_listen: None,
            live_lan_listen: None,
        },
        once,
        NativeServeMode::Program(Box::new(ProgramServeSetup {
            context,
            outputs,
            windowed,
            display_overrides,
            camera_helper,
        })),
        record_program,
        record_output,
        record_program_2,
        record_output_2,
        record_wav,
        record_segment_seconds,
        record_bitrate_kbps,
        record_bitrate_kbps_2,
        record_audio_bitrate_kbps,
        record_audio_bitrate_kbps_2,
        // `--record-fallback-dir` is refused with `--fullscreen-program`.
        None,
        None,
        // `--recover-to-checkpoint` is refused with `--fullscreen-program`.
        false,
    )
}

/// Replaces the project's display-output slots with the `--display-output`
/// overrides, slot 1 first, when any are given (OR-011). The engine then
/// starts with them; [`persist_display_output_overrides`] saves them.
///
/// # Errors
///
/// Returns an [`AppFailure`] when an override shows an output the project
/// does not declare or shares a display with another.
pub(crate) fn apply_display_output_overrides(
    stored: StoredProject,
    overrides: &[DisplayOutputConfig],
) -> AppResult<StoredProject> {
    if overrides.is_empty() {
        return Ok(stored);
    }
    let mut project = stored.project().clone();
    for slot in DisplayOutputSlot::ALL {
        project.clear_display_output(slot);
    }
    for (slot, config) in DisplayOutputSlot::ALL.into_iter().zip(overrides) {
        project
            .set_display_output(slot, config.clone())
            .map_err(|error| AppFailure(format!("--display-output {slot}: {error}")))?;
    }
    Ok(stored
        .with_project(project)
        .map_err(|error| AppFailure(format!("--display-output: {error}")))?)
}

/// Applies the `--display-output` overrides (see
/// [`apply_display_output_overrides`]) and checkpoints the manifest when
/// they changed it, before any command is journaled: commands accepted
/// against the overridden slots must replay against them after a crash.
///
/// # Errors
///
/// Returns the override refusal or a checkpoint failure.
pub(crate) fn persist_display_output_overrides(
    store: &dyn DurableStore,
    stored: &StoredProject,
    overrides: &[DisplayOutputConfig],
) -> AppResult<StoredProject> {
    let overridden = apply_display_output_overrides(stored.clone(), overrides)?;
    if overridden.project().display_outputs() != stored.project().display_outputs() {
        store.checkpoint(&overridden)?;
    }
    Ok(overridden)
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
pub(crate) fn serve_inner(
    path: &Path,
    listeners: ServeListeners,
    once: bool,
    mode: NativeServeMode,
    record_program: Option<PathBuf>,
    record_output: Option<fm_types::OutputId>,
    record_program_2: Option<PathBuf>,
    record_output_2: Option<fm_types::OutputId>,
    record_wav: bool,
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    record_bitrate_kbps: Option<u32>,
    record_bitrate_kbps_2: Option<u32>,
    record_audio_bitrate_kbps: Option<u32>,
    record_audio_bitrate_kbps_2: Option<u32>,
    record_fallback_dir: Option<PathBuf>,
    diagnostic_stop_after: Option<Duration>,
    recover_to_checkpoint: bool,
) -> AppResult<()> {
    let ServeListeners {
        listen,
        web_listen,
        web: web_settings,
        status_listen,
        hls_listen,
        live_lan_listen,
    } = listeners;
    if !listen.ip().is_loopback() {
        return Err(AppFailure(format!(
            "development mode requires a loopback listen address, got {}",
            listen.ip()
        ))
        .into());
    }
    web_settings
        .validate(web_listen)
        .map_err(|error| AppFailure(error.to_string()))?;
    if web_listen.is_some() && once {
        return Err(AppFailure("--web-listen cannot be combined with --once".into()).into());
    }
    if status_listen.is_some_and(|address| !address.ip().is_loopback()) {
        return Err(AppFailure("--status-listen must use a loopback address".into()).into());
    }
    if hls_listen.is_some_and(|address| !address.ip().is_loopback()) {
        return Err(AppFailure("--hls-listen must use a loopback address".into()).into());
    }

    let store = ProjectStore::new(path)?;
    let project = load_and_recover(&store, recover_to_checkpoint)?;
    let bundle_assets = load_bundle_assets(&store, project.project())?;
    // Opened once and held for the run: this process is the journal's only
    // writer, and reopening it per command is what made an accepted command
    // cost what the whole show had cost so far.
    let journal = DurableJournal::open(&store)?;
    let project = persist_display_output_overrides(&journal, &project, mode.display_overrides())?;
    let project_id = project.project().id();
    let engine = restore_engine_with_titles(&project, &bundle_assets)?;
    let identity = format!("project-{project_id}");
    let control = Rc::new(RefCell::new(ControlService::new(
        engine,
        Policy::development(),
        identity.clone(),
        format!("{identity}-log"),
        ControlLimits::default(),
    )));
    // The simulated engine plays every media input from a synthetic source;
    // the native runtime opens each file's decoder.
    if matches!(&mode, NativeServeMode::Disabled) {
        attach_synthetic_media(&mut control.borrow_mut(), project.project())?;
    }
    #[cfg(feature = "display-surface")]
    let fullscreen_active = matches!(&mode, NativeServeMode::Program(_));
    let mut native = match mode {
        NativeServeMode::Disabled => None,
        NativeServeMode::Headless {
            camera_helper,
            display_inventory,
            display_overrides: _,
        } => {
            let mut native = NativeDaemon::start(&store, &project, camera_helper.as_deref())?;
            if let Some(inventory) = display_inventory {
                native.attach_display_inventory(inventory)?;
            }
            Some(native)
        }
        #[cfg(feature = "display-surface")]
        NativeServeMode::Program(setup) => {
            let ProgramServeSetup {
                context,
                outputs,
                windowed,
                display_overrides: _,
                camera_helper,
            } = *setup;
            Some(NativeDaemon::start_with_program(
                &store,
                &project,
                context,
                outputs,
                windowed,
                camera_helper.as_deref(),
            )?)
        }
    };
    if let Some(native) = native.as_mut() {
        native.attach_media_sources(&mut control.borrow_mut())?;
        // Replay records beside the journal unless the show names a root.
        native.attach_replay(
            &mut control.borrow_mut(),
            store.root().join("replay"),
            project.project().settings(),
        );
        native.attach_reel(project.project().settings());
        // Document inputs show their pages from the start (IN-014).
        #[cfg(feature = "native-media")]
        if let Some(pages) = journal.documents() {
            native.documents.observe(project.project(), pages);
        }
        #[cfg(feature = "native-media")]
        native.sync_media_list_prefetch(project.project());
    }
    // Installed audio plugins beside the bundle's journal (AU-006): the
    // built-in reference processors plus every scanned manifest. Scanning
    // lists what is installed; a chain naming a plugin without a processor
    // is still refused when it is applied.
    {
        let catalog = fm_plugin_host_api::PluginCatalog::scan(&store.root().join("plugins"));
        let mut available: Vec<String> = fm_plugin_host_api::REFERENCE_PLUGINS
            .iter()
            .map(|plugin| (*plugin).to_owned())
            .collect();
        available.extend(catalog.identifiers());
        control.borrow_mut().set_installed_audio_plugins(available);
    }
    // Every authored local HLS output is a configured show destination, so it
    // fails closed before serving exactly like the configured recording
    // output below: too little free space in any local output directory
    // refuses startup. The failure names the authored stream target and
    // carries free-space counts and I/O kinds only, never the directory.
    #[cfg(feature = "native-media")]
    if let Some(native) = native.as_ref() {
        native.preflight_local_outputs()?;
    }
    let mut process_shutdown = register_process_shutdown()?;
    let authority = control_server_identity(&control.borrow(), project_id);
    let mut durable = project;
    if record_program.is_some() || record_program_2.is_some() {
        if native.is_none() {
            let flag = if record_program.is_some() {
                "--record-program"
            } else {
                "--record-program-2"
            };
            return Err(AppFailure(format!("{flag} requires --native-media")).into());
        }
        validate_recording_output_selections(durable.project(), record_output, record_output_2)?;
        // Configured recording fails closed before any recorder exists. Each
        // preflight error carries free-space counts and I/O kinds only, never
        // the operator's paths.
        preflight_recording_outputs([
            ("--record-program", record_program.as_deref()),
            ("--record-program-2", record_program_2.as_deref()),
        ])?;
        validate_recording_sessions(
            [
                ("--record-program", record_program.as_deref()),
                ("--record-program-2", record_program_2.as_deref()),
            ],
            record_fallback_dir.as_deref(),
        )?;
        let Some(native) = native.as_mut() else {
            return Err("recording requires the native media runtime".into());
        };
        native.configure_recordings(
            record_program.clone(),
            record_output,
            record_program_2.clone(),
            record_output_2,
            record_wav,
            record_segment_seconds,
            record_bitrate_kbps,
            record_audio_bitrate_kbps,
            record_bitrate_kbps_2,
            record_audio_bitrate_kbps_2,
        );
        native.configure_record_fallback(record_fallback_dir);
        // A crash may have left sessions of these slots open: repair them
        // before anything records next to them or derives a name from them.
        native.recover_recording_sessions();
        // Startup reconciliation is deliberate: each configured recorder only
        // auto-starts when its own restored desired flag says so, so a slot
        // whose flag is false stays closed even when the other is recording.
        // The capability digest below keeps advertising configured support
        // either way; runtime RecordStart commands open segments later on
        // demand.
        if durable
            .project()
            .recording_desired_slots()
            .iter()
            .any(|&active| active)
        {
            // The playlist and cue clocks run inside the startup barrier
            // too: a restored playlist keeps cutting on its scheduled frames
            // and a restored cue wait still ends on its due frame while the
            // recorder starts, exactly as the session loop's clocks do.
            let stored = durable.clone();
            let principal = development_principal()?;
            let mut clock = playlist_clock::PlaylistClock::default();
            let mut cues = cue_clock::CueClock::default();
            let mut before_frame =
                |native: &mut NativeDaemon, control: &mut ControlService<Policy>| {
                    // As in the session loop: the clocks wait while the
                    // frame loop realizes a transition or title phase.
                    if control.work_in_flight() {
                        return Ok(());
                    }
                    if let Some(advance) = clock.next(
                        control,
                        playlist_clock::Pacing::Engine,
                        || !journal.busy() && native.retry_window(journal.recent_append_peak()),
                        now_millis()?,
                    )? {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &advance.command(),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        clock.observe(&advance, &delivery.result);
                    }
                    if let Some(advance) = cues.next(
                        control,
                        playlist_clock::Pacing::Engine,
                        || !journal.busy() && native.retry_window(journal.recent_append_peak()),
                        now_millis()?,
                    )? {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &advance.command(),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        cues.observe(&advance, &delivery.result);
                    }
                    Ok(())
                };
            let primed = native.start_and_prime_configured_recorders(
                &stored,
                &mut control.borrow_mut(),
                &authority,
                Some(&process_shutdown),
                &mut before_frame,
            );
            match primed {
                Ok(true) => {}
                Ok(false) => {
                    checkpoint_native(&control, &authority, &journal, &mut durable)?;
                    native.finalize_recorder()?;
                    return Ok(());
                }
                Err(error) => {
                    // Fail closed for configured recording; whatever already
                    // started is finalized and checkpointed through the same
                    // shutdown path this branch always used.
                    let _ = native.finalize_recorder();
                    return Err(error);
                }
            }
        }
    }
    // Every configured network stream endpoint is probed exactly once here,
    // after the disk preflights and before readiness is advertised. The probe
    // is bounded and never refuses startup, changes destination state, or
    // touches recorder or engine state: an ingest may legitimately come up
    // later, and the shared output set owns reconnection. Each probed endpoint
    // emits one sanitized FREEMIXD_PREFLIGHT record.
    #[cfg(feature = "native-media")]
    if let Some(native) = native.as_ref() {
        for record in native.network_preflight_records() {
            eprintln!("{record}");
        }
    }
    #[cfg(not(feature = "display-surface"))]
    let fullscreen_active = false;
    // Reconciled stream targets start here, after all preflight succeeded and
    // before readiness is advertised. A refusing destination latches its own
    // target and is reported; it never keeps the daemon from becoming ready.
    if let Some(native) = native.as_mut() {
        native.prime_startup_streams(&durable);
    }
    // The immutable handshake digest advertises successfully configured
    // startup support. Late recorder health is reported by FREEMIXD_RECORDER.
    let capabilities_digest = capabilities_digest(
        native.is_some(),
        fullscreen_active,
        native
            .as_ref()
            .is_some_and(NativeDaemon::recording_configured),
    );
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        listen.ip(),
        capabilities_digest,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control)))?;
    let principal = development_principal()?;
    let listener = TcpListener::bind(listen)?;
    listener.set_nonblocking(true)?;
    let web_gateway = if let Some(address) = web_listen {
        let web_listener = TcpListener::bind(address)?;
        web_listener.set_nonblocking(true)?;
        Some(web::WebGateway::bind(
            web_listener,
            &web_settings,
            Some(store.assets_root()),
        )?)
    } else {
        None
    };
    let operator_status = status_listen
        .map(|address| StatusListener::bind(address, &process_shutdown.status))
        .transpose()?;
    if operator_status.is_some() {
        // Operator state is rendered only when a listener consumes it; the
        // attach happens after the bind so a refused listener never leaves a
        // half-wired publisher behind.
        process_shutdown.attach_control(&control);
    }
    // The HLS listener is a read-only view of the compiled `hls://` destination
    // directories. It is refused when no such destination is configured, and
    // its token is required before readiness is advertised, exactly like the
    // status listener's.
    let hls_listener = match hls_listen {
        Some(address) => {
            if native.is_none() {
                return Err(AppFailure("--hls-listen requires --native-media".into()).into());
            }
            #[cfg(feature = "native-media")]
            let roots = hls_roots(native.as_ref()).map_err(|error| {
                AppFailure(format!(
                    "--hls-listen could not map hls:// stream destinations: {error}"
                ))
            })?;
            #[cfg(not(feature = "native-media"))]
            let roots = HlsRoots::try_from_targets(std::iter::empty::<(u128, PathBuf)>()).map_err(
                |error| {
                    AppFailure(format!(
                        "--hls-listen could not map hls:// stream destinations: {error}"
                    ))
                },
            )?;
            if roots.is_empty() {
                return Err(AppFailure(
                    "--hls-listen requires at least one hls:// stream destination".into(),
                )
                .into());
            }
            let token = hls_server::token_from_environment()?;
            let listener =
                HlsServer::start(address, token, roots, Arc::new(AtomicBool::new(false)))?;
            #[cfg(feature = "native-media")]
            if let Some(native) = native.as_mut() {
                native.hls_routes = Some(listener.routes());
            }
            Some(listener)
        }
        None => None,
    };
    // The LiveLAN viewer listener (OR-017) serves whatever the project's
    // LiveLAN settings allow; it may bind a LAN address.
    let live_lan_listener = match live_lan_listen {
        Some(address) => {
            if native.is_none() {
                return Err(AppFailure("--live-lan-listen requires --native-media".into()).into());
            }
            let listener = live_lan::LiveLanServer::start(address)?;
            #[cfg(feature = "native-media")]
            if let Some(native) = native.as_mut() {
                native.attach_live_lan(listener.handle(), durable.project().settings().clone());
            }
            Some(listener)
        }
        None => None,
    };
    server.mark_ready()?;
    process_shutdown.observe_service(server.status());
    let readiness = ReadinessRecord {
        address: listener.local_addr()?,
        project_id,
    };
    println!("{readiness}");
    std::io::stdout().flush()?;
    if let Some(web) = web_gateway.as_ref() {
        println!("FREEMIXD_WEB_READY\tv=1\taddress={}", web.address());
        if let Some(record) = web.tls_record() {
            println!("{record}");
        }
        std::io::stdout().flush()?;
        web.start_accepting();
    }
    if let Some(listener) = operator_status.as_ref() {
        println!(
            "{}",
            StatusReadinessRecord {
                address: listener.address()
            }
        );
        std::io::stdout().flush()?;
    }
    if let Some(listener) = hls_listener.as_ref() {
        println!("FREEMIXD_HLS_READY\tv=1\taddress={}", listener.address());
        std::io::stdout().flush()?;
    }
    if let Some(listener) = live_lan_listener.as_ref() {
        println!(
            "FREEMIXD_LIVE_LAN_READY\tv=1\taddress={}",
            listener.address()
        );
        std::io::stdout().flush()?;
    }
    if let Some(duration) = diagnostic_stop_after {
        process_shutdown.set_diagnostic_deadline(duration)?;
    }

    // Title templates and fonts load once, before serving, for the simulated
    // monitor tap and for exact title previews (which native sessions serve
    // too). Native media loads its own for its title render worker (see
    // `native_titles`).
    let titles = Arc::new(title_render::TitleAssets::load(&store, durable.project()));
    let session_result = sessions::run(
        &listener,
        &server,
        &control,
        &journal,
        &mut durable,
        &principal,
        native.as_mut(),
        &authority,
        &titles,
        &process_shutdown,
        once,
        web_gateway.as_ref(),
    );
    process_shutdown.status.begin_draining();
    let gateway_result = web_gateway.map(web::WebGateway::shutdown);
    let status_result = operator_status.map(StatusListener::shutdown);
    let hls_result = hls_listener.map(HlsServer::shutdown);
    let live_lan_result = live_lan_listener.map(live_lan::LiveLanServer::shutdown);
    let _shutdown_reason = session_result?;
    if let Some(result) = gateway_result {
        result?;
    }
    if let Some(result) = status_result {
        result?;
    }
    if let Some(result) = hls_result {
        result?;
    }
    if let Some(result) = live_lan_result {
        result?;
    }
    if native.is_some() {
        checkpoint_native(&control, &authority, &journal, &mut durable)?;
    } else {
        // A clean stop is the safest checkpoint there is: no command is in
        // flight, so the manifest ends the run holding everything the journal
        // recorded and the next start has nothing to replay.
        journal.settle(&durable)?;
    }
    #[cfg(feature = "display-surface")]
    if fullscreen_active {
        let frames_presented = native
            .as_ref()
            .and_then(NativeDaemon::program_telemetry)
            .map(|telemetry| telemetry.frames_presented)
            .unwrap_or_default();
        eprintln!("FREEMIXD_PROGRAM\tv=1\tframes_presented={frames_presented}");
    }
    #[cfg(feature = "native-media")]
    if let Some((frames_presented, failures, frames_dropped)) = native
        .as_ref()
        .and_then(NativeDaemon::display_output_totals)
    {
        eprintln!(
            "FREEMIXD_DISPLAY_OUTPUTS\tv=1\tframes_presented={frames_presented}\tfailures={failures}\tframes_dropped={frames_dropped}"
        );
    }
    if let Some(native) = &mut native {
        // Streams stop before the recorder so the final records reflect the
        // same shutdown instant, and their results report after the recorder's
        // like camera results do.
        let stream_results = native.finalize_streams();
        let recorder_result = native.finalize_recorder();
        native.emit_camera_source_telemetry();
        let camera_result = native.finalize_cameras();
        native.emit_telemetry();
        recorder_result?;
        camera_result?;
        for result in stream_results {
            result?;
        }
    }
    Ok(())
}
