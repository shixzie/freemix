//! The native source resolution: turning a stored project's inputs into the
//! media sources the realizer plays (media files, stills, generators,
//! titles, and macOS cameras).
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(clippy::wildcard_imports)] // the binary's items and imports
use super::*;

#[cfg(feature = "native-media")]
#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_native_sources(
    store: &ProjectStore,
    stored: &StoredProject,
    camera_helper: Option<&Path>,
) -> AppResult<NativeSourceResolution> {
    let settings = stored.project().settings();
    let captures = native_capture_backend().is_some();
    let mut sources = Vec::with_capacity(stored.project().inputs().len());
    for input in stored.project().inputs() {
        let source = match &input.kind {
            // A capture input starts on a blank picture; its supervisor
            // ingests every picture after it (IN-001).
            InputKind::Device { .. } if captures => {
                let dimensions = settings.video.dimensions;
                native_streams::startup_picture(
                    (dimensions.width(), dimensions.height()),
                    settings.frame_rate,
                    native_clock_domain(),
                )
                .map(|frame| NativeResolvedSource::LiveFrame {
                    input: input.id,
                    frame,
                })
                .ok_or_else(|| {
                    AppFailure(format!("native capture input {} cannot be timed", input.id)).into()
                })
            }
            InputKind::Media { asset_uri, .. } => resolve_native_media_source(
                store,
                input.id,
                asset_uri,
                native_clock_domain(),
                settings.video.dimensions,
            ),
            InputKind::Simulated(simulated) => {
                native_generator_frame(input.id, simulated, settings).map(|frame| {
                    NativeResolvedSource::RetainedFrame {
                        input: input.id,
                        frame,
                    }
                })
            }
            InputKind::Color => Err(AppFailure(format!(
                "native color input {} has no configured RGBA value",
                input.id
            ))
            .into()),
            // Title inputs are rendered by [`resolve_native_title_sources`].
            InputKind::Device { .. } | InputKind::Scene { .. } | InputKind::Title(_) => continue,
            // A network stream starts on a blank picture; its receiver
            // ingests every picture after it (IN-015). A system audio
            // input keeps its blank picture (AU-011), a document's page
            // worker ingests its page (IN-014), and a screen capture
            // ingests its captures after it (IN-011).
            InputKind::NetworkStream(_)
            | InputKind::Replay
            | InputKind::SystemAudio(_)
            | InputKind::Document(_)
            | InputKind::ScreenCapture(_) => {
                let dimensions = settings.video.dimensions;
                native_streams::startup_picture(
                    (dimensions.width(), dimensions.height()),
                    settings.frame_rate,
                    native_clock_domain(),
                )
                .map(|frame| NativeResolvedSource::LiveFrame {
                    input: input.id,
                    frame,
                })
                .ok_or_else(|| {
                    AppFailure(format!("native network input {} cannot be timed", input.id)).into()
                })
            }
            // A still image is decoded and placed with its fit here, then
            // uploaded once (IN-008).
            InputKind::Image(image) => {
                resolve_native_image_source(store, input.id, image, settings.video.dimensions)
            }
            // A slideshow starts on a blank picture; its slides are composed
            // into it on the GPU (IN-009).
            InputKind::Slideshow(_) => {
                let dimensions = settings.video.dimensions;
                native_streams::startup_picture(
                    (dimensions.width(), dimensions.height()),
                    settings.frame_rate,
                    native_clock_domain(),
                )
                .map(|frame| NativeResolvedSource::LiveFrame {
                    input: input.id,
                    frame,
                })
                .ok_or_else(|| {
                    AppFailure(format!(
                        "native slideshow input {} cannot be timed",
                        input.id
                    ))
                    .into()
                })
            }
            // Model-only so far: refuse rather than play some other picture.
            InputKind::Alias { .. } | InputKind::MixFeed(_) => Err(AppFailure(format!(
                "native alias or program/preview feed input {} is not yet realized",
                input.id
            ))
            .into()),
        }?;
        sources.push(source);
    }

    #[cfg(target_os = "macos")]
    {
        if captures {
            return Ok(NativeSourceResolution {
                sources,
                cameras: NativeCameraInputs::default(),
            });
        }
        let (mut device_sources, cameras) = resolve_macos_camera_sources(stored, camera_helper)?;
        sources.append(&mut device_sources);
        Ok(NativeSourceResolution { sources, cameras })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = camera_helper;
        if !captures
            && let Some(input) = stored
                .project()
                .inputs()
                .iter()
                .find(|input| matches!(input.kind, InputKind::Device { .. }))
        {
            return Err(AppFailure(format!(
                "native device input {} has no adapter on this platform",
                input.id
            ))
            .into());
        }
        Ok(NativeSourceResolution { sources })
    }
}

/// Renders the first frame of every title input and appends it to `sources`
/// as a live frame, returning the worker that re-renders them off the loop.
#[cfg(feature = "native-media")]
pub(crate) fn resolve_native_title_sources(
    store: &ProjectStore,
    stored: &StoredProject,
    sources: &mut Vec<NativeResolvedSource>,
) -> AppResult<native_titles::NativeTitleSources> {
    let project = stored.project();
    let assets = if project
        .inputs()
        .iter()
        .any(|input| matches!(input.kind, InputKind::Title(_)))
    {
        title_render::TitleAssets::load(store, project)
    } else {
        title_render::TitleAssets::default()
    };
    let (titles, seeds) =
        native_titles::NativeTitleSources::start(Arc::new(assets), project, native_clock_domain())
            .map_err(AppFailure)?;
    sources.extend(
        seeds
            .into_iter()
            .map(|(input, frame)| NativeResolvedSource::LiveFrame { input, frame }),
    );
    Ok(titles)
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_macos_camera_sources(
    stored: &StoredProject,
    camera_helper: Option<&Path>,
) -> AppResult<(Vec<NativeResolvedSource>, NativeCameraInputs)> {
    resolve_macos_camera_sources_with_policy(stored, camera_helper, CAMERA_RECOVERY_POLICY)
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_macos_camera_sources_with_policy(
    stored: &StoredProject,
    camera_helper: Option<&Path>,
    recovery_policy: CameraRecoveryPolicy,
) -> AppResult<(Vec<NativeResolvedSource>, NativeCameraInputs)> {
    let device_inputs = stored
        .project()
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::Device { stable_key, .. } => Some((input.id, stable_key.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if device_inputs.is_empty() {
        return Ok((Vec::new(), NativeCameraInputs::default()));
    }
    if device_inputs.len() > NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS {
        return Err(AppFailure(format!(
            "camera input count {} exceeds native source limit {}",
            device_inputs.len(),
            NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS
        ))
        .into());
    }
    let mut keys = std::collections::BTreeSet::new();
    if let Some((input, key)) = device_inputs.iter().find(|(_, key)| !keys.insert(*key)) {
        return Err(AppFailure(format!(
            "camera input {input} duplicates stable key `{}`",
            diagnostic_field(key)
        ))
        .into());
    }

    let adapter = match camera_helper {
        Some(path) => MacosCameraAdapter::discover_with_helper(path)?,
        None => MacosCameraAdapter::discover()?,
    };
    if !adapter.permission().is_granted() {
        return Err(AppFailure(
            "camera permission is not granted; use `freemix-capture-node cameras --request-permission` from an interactive desktop session"
                .into(),
        )
        .into());
    }
    let frame_rate = stored.project().settings().frame_rate;
    let dimensions = stored.project().settings().video.dimensions;
    let mut cameras = NativeCameraInputs::default();
    for (input, stable_key) in device_inputs {
        let mut source = adapter.open_video_source_by_stable_key(stable_key)?;
        let format = source
            .exact_video_format_at_rate(dimensions.width(), dimensions.height(), frame_rate)
            .ok_or_else(|| {
                AppFailure(format!(
                    "camera input {input} has no exact {}x{} at {}/{} fps BGRA mode",
                    dimensions.width(),
                    dimensions.height(),
                    frame_rate.numerator(),
                    frame_rate.denominator()
                ))
            })?;
        let clock_domain = source
            .descriptor()
            .capabilities
            .clocks
            .first()
            .ok_or_else(|| AppFailure(format!("camera input {input} has no advertised clock")))?
            .domain;
        source.open(IoOpenOptions {
            format,
            clock_domain,
            memory_domain: MemoryDomain::Cpu,
            queue_capacity: NonZeroUsize::MIN,
            signal_loss: SignalLossPolicy::Hold,
        })?;
        cameras.inputs.push(NativeCameraInput {
            input,
            supervisor: Arc::new(Mutex::new(CameraSupervisorState {
                frame: CameraFrameSlot::default(),
                snapshot: CameraWorkerSnapshot::running(&source),
            })),
            source: Some(source),
            worker: None,
            recovery_policy,
            ingested_frames: 0,
            ingest_failed: 0,
            preflight_depth: 0,
            preflight_discarded: 0,
            last_ingested_sequence: None,
            last_ingested_discontinuity: false,
        });
    }

    let start_results = thread::scope(|scope| -> AppResult<Vec<(InputId, Result<(), String>)>> {
        let mut handles = Vec::with_capacity(cameras.inputs.len());
        for camera in &mut cameras.inputs {
            let input = camera.input;
            let Some(source) = camera.source.as_mut() else {
                continue;
            };
            handles.push(
                thread::Builder::new()
                    .name(format!("freemix-camera-start-{input}"))
                    .spawn_scoped(scope, move || {
                        (input, source.start().map_err(|error| error.to_string()))
                    })?,
            );
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| AppFailure("camera startup worker panicked".into()).into())
            })
            .collect()
    });
    let start_results = match start_results {
        Ok(results) => results,
        Err(error) => {
            return Err(camera_startup_failure(
                &mut cameras,
                format!("camera startup coordination failed: {error}"),
            ));
        }
    };
    if let Some((input, Err(detail))) = start_results
        .into_iter()
        .find(|(_, result)| result.is_err())
    {
        return Err(camera_startup_failure(
            &mut cameras,
            format!("camera input {input} failed to start: {detail}"),
        ));
    }

    let deadline = Instant::now()
        .checked_add(CAMERA_INITIAL_FRAME_TIMEOUT)
        .ok_or_else(|| AppFailure("camera initial-frame deadline overflow".into()))?;
    let mut initial_frames = BTreeMap::new();
    while initial_frames.len() < cameras.inputs.len() {
        for camera in &mut cameras.inputs {
            if initial_frames.contains_key(&camera.input) {
                continue;
            }
            let transfer = {
                let Some(source) = camera.source.as_mut() else {
                    continue;
                };
                let transfer = source.try_receive();
                let mut state = camera
                    .supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                update_camera_worker_snapshot_inner(&mut state.snapshot, source);
                transfer
            };
            let transfer = match transfer {
                Ok(transfer) => transfer,
                Err(error) => {
                    let detail = format!(
                        "camera input {} initial-frame acquisition failed: {error}",
                        camera.input
                    );
                    return Err(camera_startup_failure(&mut cameras, detail));
                }
            };
            match transfer {
                Some(MediaTransfer::Live(frame)) => {
                    camera.preflight_depth = 1;
                    initial_frames.insert(camera.input, frame);
                }
                Some(MediaTransfer::Fallback { .. }) => {
                    let detail = format!(
                        "camera input {} returned fallback media during startup",
                        camera.input
                    );
                    return Err(camera_startup_failure(&mut cameras, detail));
                }
                None => {}
            }
        }
        if initial_frames.len() == cameras.inputs.len() {
            break;
        }
        if Instant::now() >= deadline {
            let detail = format!(
                "camera initial-frame acquisition timed out with {}/{} sources ready",
                initial_frames.len(),
                cameras.inputs.len()
            );
            return Err(camera_startup_failure(&mut cameras, detail));
        }
        thread::sleep(Duration::from_millis(2));
    }
    let sources = cameras
        .inputs
        .iter()
        .filter_map(|camera| {
            Some(NativeResolvedSource::LiveFrame {
                input: camera.input,
                frame: initial_frames.remove(&camera.input)?,
            })
        })
        .collect();
    cameras.start_workers()?;
    Ok((sources, cameras))
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) fn camera_startup_failure(
    cameras: &mut NativeCameraInputs,
    mut detail: String,
) -> Box<dyn Error> {
    cameras.discard_preflight_frames();
    if cameras
        .cleanup_startup_sources(CAMERA_STARTUP_CLEANUP_TIMEOUT)
        .is_err()
    {
        detail.push_str("; camera startup cleanup also failed");
    }
    Box::new(AppFailure(detail))
}

#[cfg(feature = "native-media")]
pub(crate) fn resolve_native_media_source(
    store: &ProjectStore,
    input: InputId,
    asset_uri: &str,
    clock_domain: MediaClockDomainId,
    output_dimensions: fm_types::VideoDimensions,
) -> AppResult<NativeResolvedSource> {
    // A media file missing from the bundle starts its input offline
    // (`source_missing`) as a media-file input, instead of stopping the
    // daemon: the file worker reports the fault and retries the file when
    // the input reopens (IN-026). A broken bundle still stops startup.
    let missing = || {
        let key = asset_uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)?;
        fm_model::validate_media_asset_uri(asset_uri).ok()?;
        Some(NativeResolvedSource::LocalVideo {
            input,
            path: store.assets_root().join(key),
        })
    };
    let path = match store.resolve_asset_uri(asset_uri) {
        Ok(path) => path,
        Err(fm_persistence::AssetResolveError::AssetUnavailable(error))
            if error.kind() == std::io::ErrorKind::NotFound
                && let Some(source) = missing() =>
        {
            return Ok(source);
        }
        Err(error) => {
            return Err(AppFailure(format!(
                "native media asset resolution failed for input {input}: {error}"
            ))
            .into());
        }
    };
    let mut file = File::open(&path).map_err(|error| {
        AppFailure(format!(
            "native media read failed for input {input}: {:?}",
            error.kind()
        ))
    })?;
    let mut prefix = Vec::with_capacity(fm_codec_image::STILL_SNIFF_BYTES);
    Read::by_ref(&mut file)
        .take(fm_codec_image::STILL_SNIFF_BYTES as u64)
        .read_to_end(&mut prefix)
        .map_err(|error| {
            AppFailure(format!(
                "native media read failed for input {input}: {:?}",
                error.kind()
            ))
        })?;
    if sniff_still_format(&prefix).is_err() {
        return Ok(NativeResolvedSource::LocalVideo { input, path });
    }

    // Any picture size is placed in the raster, so only the decoder's own
    // bounds apply.
    let limits = StillDecodeLimits::default();
    let mut encoded = prefix;
    let remaining = limits
        .max_encoded_bytes
        .saturating_add(1)
        .saturating_sub(encoded.len());
    file.take(u64::try_from(remaining).unwrap_or(u64::MAX))
        .read_to_end(&mut encoded)
        .map_err(|error| {
            AppFailure(format!(
                "native still read failed for input {input}: {:?}",
                error.kind()
            ))
        })?;
    // A still media file fills the raster as large as it fits whole.
    let decoded = fm_codec_image::decode_still_for_output(
        &encoded,
        retained_frame_timing(clock_domain)?,
        limits,
        output_dimensions,
        fm_types::OutputScaling::Fit,
    )
    .map_err(|error| {
        AppFailure(format!(
            "native still decode failed for input {input}: {error}"
        ))
    })?;
    Ok(NativeResolvedSource::RetainedFrame {
        input,
        frame: decoded.frame,
    })
}

/// Decodes image input `input`'s picture placed in the raster with its fit
/// (IN-008). A missing or undecodable file stops startup, as a broken media
/// still does.
#[cfg(feature = "native-media")]
pub(crate) fn resolve_native_image_source(
    store: &ProjectStore,
    input: InputId,
    image: &fm_types::still::ImageInputConfig,
    output_dimensions: fm_types::VideoDimensions,
) -> AppResult<NativeResolvedSource> {
    let path = store.resolve_asset_uri(&image.asset_uri).map_err(|error| {
        AppFailure(format!(
            "native image asset resolution failed for input {input}: {error}"
        ))
    })?;
    let frame =
        stills::decode_picture_file(&path, output_dimensions, image.fit, native_clock_domain())
            .map_err(|fault| {
                AppFailure(format!(
                    "native image decode failed for input {input}: {}",
                    fault.token()
                ))
            })?;
    Ok(NativeResolvedSource::RetainedFrame { input, frame })
}

#[cfg(feature = "native-media")]
pub(crate) fn retained_frame_timing(clock_domain: MediaClockDomainId) -> AppResult<MediaTiming> {
    let time_base = fm_types::TimeBase::new(1, 1_000_000_000)
        .map_err(|error| AppFailure(format!("native still time base is invalid: {error}")))?;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), time_base),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(1)
            .map_err(|error| AppFailure(format!("native still duration is invalid: {error}")))?,
        clock_domain,
        SequenceNumber::new(0),
    )
    .map_err(|error| AppFailure(format!("native still timing is invalid: {error}")).into())
}
