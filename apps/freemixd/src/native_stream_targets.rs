//! The native streaming path: destination splitting, rendition groups, the
//! per-target pair queues and sinks, HLS routing, and the stream status
//! samples.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(clippy::wildcard_imports)] // the binary's items and imports
use super::*;

/// Per-target bookkeeping for the native streaming feed path.
///
/// The ledger is separated from the sink so the counting rules — advance the
/// admitted cursor only on a successful enqueue, count every refusal, never
/// block or panic — are testable without a GPU runtime or an `FFmpeg` child.
/// Gating lives on the rendition group's shared cursor; this per-target
/// cursor records how far each target's own admissions reached.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamPairLedger {
    /// One past the highest sequence this target actually admitted.
    pub(crate) next_sequence: u64,
    pub(crate) enqueued_pairs: u64,
    pub(crate) dropped_pairs: u64,
}

#[cfg(feature = "native-media")]
impl StreamPairLedger {
    pub(crate) fn note_admitted(&mut self, sequence: SequenceNumber) {
        self.enqueued_pairs = self.enqueued_pairs.saturating_add(1);
        self.next_sequence = sequence.get().saturating_add(1);
    }

    pub(crate) fn note_dropped(&mut self) {
        self.dropped_pairs = self.dropped_pairs.saturating_add(1);
    }
}

/// Planner-facing state of one shared-rendition group: the destinations whose
/// encoder-relevant settings — authored project output, output protocol, plus
/// authored video bitrate — are identical, so they share ONE readback, ONE
/// packed pair per frame, and ONE sequence cursor. Record format, audio, GOP,
/// and every other encoder input derive from project settings that are
/// identical across targets by construction.
///
/// `variants` pins the group's rendition id inside planner-produced plans:
/// [`RenditionPlanner`] numbers renditions by first-seen unique profile, so
/// the group's real profile sits at exactly its group position among inert
/// sibling rungs (see [`pinned_rendition_variants`]). Sinks refuse packets
/// claiming any other rendition, which makes the id part of the sink
/// contract and the reason the pinning exists.
#[cfg(feature = "native-media")]
pub(crate) struct StreamRenditionCore {
    /// Authored project output this rendition group captures from.
    pub(crate) output: fm_types::OutputId,
    /// Member destination slots in authored order.
    pub(crate) members: Vec<DestinationId>,
    pub(crate) rendition: RenditionId,
    pub(crate) variants: Vec<AbrVariant>,
    /// Smallest sequence the group may still pack; advances once per packed
    /// frame no matter how many member routes take it.
    pub(crate) next_sequence: u64,
}

/// One rendition group's GPU-side state: the single synchronous program
/// readback owner every member shares, and the format the group's pairs pack
/// with.
#[cfg(feature = "native-media")]
pub(crate) struct StreamRenditionGroup {
    pub(crate) core: StreamRenditionCore,
    pub(crate) format: RecordFormat,
    pub(crate) readback: NativeProgramReadback,
    /// The encoder identity every member shares: authored output, output
    /// protocol, and authored video bitrate.
    pub(crate) key: StreamGroupKey,
}

/// A rendition group's encoder identity: authored project output, output
/// protocol, and authored video bitrate in kbps.
#[cfg(feature = "native-media")]
pub(crate) type StreamGroupKey = (fm_types::OutputId, OutputProtocol, u32);

/// Builds one group's exact rendition identity from the project-wide format
/// plus its authored video bitrate. `bitrate_step` offsets the video bitrate
/// by one bit per step for the inert ladder rungs that pin plan numbering;
/// authored bitrates start at 1000 kbps, so a few negative steps never reach
/// zero.
///
/// # Errors
///
/// Returns any identity-validation failure from the planner crate.
#[cfg(feature = "native-media")]
pub(crate) fn rendition_profile(
    format: &RecordFormat,
    video_bitrate_kbps: u32,
    bitrate_step: i32,
) -> AppResult<RenditionProfile> {
    let cadence = format.frame_rate();
    let authored_bps = i64::from(video_bitrate_kbps) * 1_000 + i64::from(bitrate_step);
    assert!(
        authored_bps > 0,
        "authored stream bitrates leave room for ladder-pinning steps"
    );
    let video = VideoRendition::new(
        CodecId::new("video/h264")?,
        "high",
        format.dimensions(),
        FrameRate::new(cadence.numerator(), cadence.denominator())?,
        ColorDescription::Rec709Limited,
        u64::try_from(authored_bps).unwrap_or(u64::MAX),
        gop_frames(cadence),
    )?;
    let channels = u16::try_from(format.channel_layout().channels().len()).unwrap_or(u16::MAX - 1);
    let audio = AudioRendition::new(
        CodecId::new("audio/aac")?,
        "lc",
        u64::from(EncoderSettings::default().audio_bitrate_kbps) * 1_000,
        format.sample_rate().hertz(),
        channels,
        (0..channels).collect(),
    )?;
    Ok(RenditionProfile::new(
        video,
        audio,
        TimingProfile::new(
            TimeBase::new(cadence.numerator(), cadence.denominator())?,
            0,
        ),
    ))
}

/// Two seconds of frames at the project cadence, at least one.
#[cfg(feature = "native-media")]
pub(crate) fn gop_frames(cadence: fm_types::FrameRate) -> u32 {
    let frames = u64::from(cadence.numerator()) * 2 / u64::from(cadence.denominator());
    u32::try_from(frames.max(1)).unwrap_or(u32::MAX)
}

/// Builds one group's planner request variants: with a single rendition
/// group the plan is the plain single-rendition request; with several groups,
/// each group's plan must still number its real rendition at the group's
/// compile-time id because sinks refuse packets from any other rendition, so
/// the real profile embeds at exactly its position among inert sibling rungs
/// whose bitrates strictly increase by one bit per step around the authored
/// one. Only the group's own rung is ever dispatched:
/// [`OutputSet::enqueue_rendition`] fans out solely along the packed
/// rendition's route.
///
/// # Errors
///
/// Returns any identity-validation failure from the planner crate.
#[cfg(feature = "native-media")]
pub(crate) fn pinned_rendition_variants(
    format: &RecordFormat,
    video_bitrate_kbps: u32,
    group_index: usize,
    group_count: usize,
) -> AppResult<Vec<AbrVariant>> {
    let rung = |step: i32, name: String| -> AppResult<AbrVariant> {
        Ok(AbrVariant::new(
            name,
            rendition_profile(format, video_bitrate_kbps, step)?,
        )?)
    };
    if group_count == 1 {
        return Ok(vec![rung(0, "program".to_owned())?]);
    }
    let mut variants = Vec::with_capacity(group_count);
    for index in 0..group_count {
        let step = i32::try_from(index).unwrap_or(i32::MAX)
            - i32::try_from(group_index).unwrap_or(i32::MAX);
        variants.push(rung(step, format!("shared-rung-{}", index + 1))?);
    }
    AbrLadder::new(variants.clone())
        .map_err(|error| AppFailure(format!("pinned rendition ladder: {error}")))?;
    Ok(variants)
}

/// One configured stream target's native realization: its own transport,
/// per-target feed bookkeeping, and membership in one shared-rendition
/// [`StreamRenditionGroup`] that owns the readback, the format, and the
/// packing step for every target sharing its encoder identity.
///
/// Mirrors [`NativeProgramRecorder`]: failures latch per target, are reported
/// once as a sanitized record, and degrade that target only — they never abort
/// daemon startup or the render loop. Connection policy (retry budget,
/// backoff, primary/backup failover) is not owned here at all: every target
/// routes through the daemon's shared [`OutputSet`], and this struct owns only
/// the transport that the set drives.
/// One stream destination's transport: the `FFmpeg` encoder for RTMP, RTMPS,
/// SRT, and HLS, or the in-tree OMT sender for `omt://`. The shared output
/// state machine, raw-pair packing, and retry policy stay identical.
#[cfg(feature = "native-media")]
#[allow(clippy::large_enum_variant)] // one transport per destination, never a collection
pub(crate) enum ProgramStreamSink {
    Ffmpeg(FfmpegRtmpSink),
    Omt(OmtTransportSink),
}

#[cfg(feature = "native-media")]
impl TransportSink for ProgramStreamSink {
    fn connect(
        &mut self,
        config: &DestinationConfig,
        endpoint: &Endpoint,
    ) -> Result<ConnectionObservation, SinkError> {
        match self {
            Self::Ffmpeg(sink) => sink.connect(config, endpoint),
            Self::Omt(sink) => sink.connect(config, endpoint),
        }
    }

    fn write(&mut self, packet: &OutputPacket) -> Result<SinkWrite, SinkError> {
        match self {
            Self::Ffmpeg(sink) => sink.write(packet),
            Self::Omt(sink) => sink.write(packet),
        }
    }

    fn session_proven(&self) -> bool {
        match self {
            Self::Ffmpeg(sink) => sink.session_proven(),
            Self::Omt(sink) => sink.session_proven(),
        }
    }

    fn disconnect(&mut self) {
        match self {
            Self::Ffmpeg(sink) => sink.disconnect(),
            Self::Omt(sink) => sink.disconnect(),
        }
    }
}

#[cfg(feature = "native-media")]
impl ProgramStreamSink {
    pub(crate) fn stream_telemetry(&self) -> Option<StreamTelemetry> {
        match self {
            Self::Ffmpeg(sink) => sink.stream_telemetry(),
            Self::Omt(_) => None,
        }
    }

    pub(crate) const fn unconfirmed_cleanups(&self) -> u64 {
        match self {
            Self::Ffmpeg(sink) => sink.unconfirmed_cleanups(),
            Self::Omt(_) => 0,
        }
    }
}

#[cfg(feature = "native-media")]
pub(crate) struct NativeProgramStream {
    pub(crate) target: SwitcherStreamTargetId,
    /// This target's slot in the shared [`OutputSet`] (1..=N in authored
    /// order).
    pub(crate) destination_id: DestinationId,
    /// Redacted primary destination for diagnostics; mirrors what
    /// `StreamDestination::redacted` renders.
    pub(crate) redacted_destination: String,
    /// The `FFmpeg` transport, whose blocking connects and stops run on a
    /// dedicated worker so the daemon loop never waits on them.
    pub(crate) sink: stream_transport::BackgroundTransport<ProgramStreamSink>,
    pub(crate) ledger: StreamPairLedger,
    /// The latched failure notice for the operator's log, and what peers see
    /// of it (Protocol 2.67); both are set and cleared together.
    pub(crate) first_failure: Option<String>,
    pub(crate) first_failure_code: Option<OutputFailure>,
    /// Set when a frame pair was refused with capacity backpressure since the
    /// last status sample; consumed (and cleared) by the sampler.
    pub(crate) enqueue_backpressure: bool,
    /// Whether this target was ever asked to run. Never-started targets stay
    /// off the wire entirely.
    pub(crate) started: bool,
    /// Uptime and encoded bitrate for the status record.
    pub(crate) meter: StreamLinkMeter,
    /// Configured capacity of the sink's pair queue, for the encoder
    /// statistics sample.
    pub(crate) queue_capacity: usize,
    /// The authored configuration this runtime realizes, with its running
    /// flag cleared: a destination edit that changes it rebuilds the runtime
    /// (OR-006).
    pub(crate) authored: fm_model::StreamTarget,
}

/// Compiles one target's authored video bitrate into the sink's encoder
/// settings; every other encoder knob keeps its sink default.
#[cfg(feature = "native-media")]
pub(crate) fn stream_encoder_settings(video_bitrate_kbps: u32) -> EncoderSettings {
    EncoderSettings {
        video_bitrate_kbps,
        ..EncoderSettings::default()
    }
}

/// One target's authored reconnect policy (OR-006) as the shared output
/// set's backoff: the first retry after the authored initial delay, doubling
/// up to the authored maximum, and giving up after the authored attempt
/// budget (none retries forever), which latches the target failed.
#[cfg(feature = "native-media")]
pub(crate) fn stream_reconnect_policy(policy: fm_model::StreamReconnectPolicy) -> ReconnectPolicy {
    ReconnectPolicy::new(
        u64::from(policy.initial_delay_ms()),
        u64::from(policy.max_delay_ms()),
        PROGRAM_STREAM_RECONNECT_MULTIPLIER,
        policy.max_attempts(),
    )
    .unwrap_or_else(|_| ReconnectPolicy::fallback())
}

/// The record format every stream pair packs with: the project picture,
/// cadence, and audio layout, numbering from `first_sequence`.
#[cfg(feature = "native-media")]
pub(crate) fn stream_record_format(
    settings: &fm_model::ProjectSettings,
    first_sequence: u64,
) -> AppResult<RecordFormat> {
    Ok(RecordFormat::new(
        settings.video.dimensions.width(),
        settings.video.dimensions.height(),
        settings.frame_rate,
        settings.audio.sample_rate,
        settings.audio.channels.clone(),
        SequenceNumber::new(first_sequence),
    )?)
}

/// Compiles one SRT target's authored latency and optional passphrase into
/// the sink's caller-mode options; `None` for every other protocol.
///
/// `fm-model` and the sink validate the same bounds, so a rejected conversion
/// is unreachable; dropping the passphrase instead would publish an
/// encrypted destination in the clear.
#[cfg(feature = "native-media")]
pub(crate) fn stream_srt_options(target: &fm_model::StreamTarget) -> Option<SrtOptions> {
    let latency_ms = target.srt_latency_ms()?;
    SrtOptions::new(
        latency_ms,
        target
            .srt_passphrase()
            .map(fm_model::SrtPassphrase::expose_secret),
    )
    .ok()
}

/// Compiles one target's authored HLS ladder into the sink's own rung type.
///
/// `fm-model` and the sink validate the identical rule set, and the authored
/// value cannot change between them, so a rejected conversion is unreachable;
/// dropping the ladder instead would silently publish one rendition of a
/// multi-bitrate destination.
#[cfg(feature = "native-media")]
pub(crate) fn stream_ladder(target: &fm_model::StreamTarget) -> Option<HlsLadder> {
    target.ladder().and_then(|ladder| {
        HlsLadder::new(
            ladder
                .rungs()
                .iter()
                .map(|rung| HlsLadderRung {
                    video_bitrate_kbps: rung.video_bitrate_kbps(),
                    width: rung.width(),
                    height: rung.height(),
                })
                .collect(),
        )
        .ok()
    })
}

/// Validates one authored target's primary and backup URLs with the codec
/// parser — validity stays where it has always been — and splits them into
/// the endpoint pieces a [`DestinationConfig`] carries.
///
/// # Errors
///
/// Returns an error when either URL would be refused at connect time; startup
/// fails closed rather than latching every target failed at air time.
#[cfg(feature = "native-media")]
pub(crate) fn split_target_endpoints(
    target: &fm_model::StreamTarget,
) -> AppResult<(
    OutputProtocol,
    Endpoint,
    Option<StreamKey>,
    Option<Endpoint>,
)> {
    let primary_url = target.expose_url();
    let backup_url = target.expose_backup_url();
    if target.protocol() != fm_model::StreamProtocol::Omt {
        StreamDestination::parse(&primary_url).map_err(|error| {
            AppFailure(format!(
                "stream target {} destination was refused before startup: {error:?}",
                target.id()
            ))
        })?;
        if let Some(backup) = &backup_url {
            StreamDestination::parse(backup).map_err(|error| {
                AppFailure(format!(
                    "stream target {} backup destination was refused before startup: {error:?}",
                    target.id()
                ))
            })?;
        }
    }
    let (protocol, endpoint, stream_key) = split_destination_url(&primary_url)?;
    let backup_endpoint = backup_url
        .as_deref()
        .map(split_destination_url)
        .transpose()?
        .map(|(_, endpoint, _)| endpoint);
    Ok((protocol, endpoint, stream_key, backup_endpoint))
}

/// Splits one full destination URL into the pieces a [`DestinationConfig`]
/// carries, using the one convention that round-trips through
/// `fm_codec_ffmpeg::StreamDestination::parse` for every protocol:
/// RTMP/RTMPS compose back as `{scheme}://{host}:{port}{path}/{key}` with the
/// application path stored in `Endpoint::path` *without* its final key
/// segment, SRT composes back as `srt://{host}:{port}?streamid={key}` with no
/// path at all, so its mandatory `Endpoint::path` field holds the inert
/// placeholder `"/"`, which the SRT branch ignores, and HLS composes back as
/// `hls://{absolute/dir}` with the directory stored in `Endpoint::path`, no
/// key at all, and the placeholder host and port a local destination never
/// renders. Every network endpoint always names an explicit port because an
/// `Endpoint` cannot be built without one; an authored URL that omits it fails
/// compilation here instead of being guessed at air time. The stream key is
/// revalidated as the redactable single path segment the transport expects.
///
/// # Errors
///
/// Returns an error for any URL shape that cannot be split losslessly under
/// those rules. No error carries endpoint or key text.
#[cfg(feature = "native-media")]
pub(crate) fn split_destination_url(
    url: &str,
) -> AppResult<(OutputProtocol, Endpoint, Option<StreamKey>)> {
    let unsupported = || -> AppResult<(OutputProtocol, Endpoint, Option<StreamKey>)> {
        Err(AppFailure("stream destination URL scheme is unsupported".into()).into())
    };
    let (protocol, rest) = if let Some(rest) = url.strip_prefix("rtmp://") {
        (OutputProtocol::Rtmp, rest)
    } else if let Some(rest) = url.strip_prefix("rtmps://") {
        (OutputProtocol::Rtmps, rest)
    } else if let Some(rest) = url.strip_prefix("srt://") {
        (OutputProtocol::Srt, rest)
    } else if let Some(rest) = url.strip_prefix("hls://") {
        (OutputProtocol::Hls, rest)
    } else if let Some(rest) = url.strip_prefix("omt://") {
        (OutputProtocol::Omt, rest)
    } else {
        return unsupported();
    };
    // A local HLS destination is one absolute directory and carries no key:
    // the endpoint stores the directory in `path`, and the placeholder host
    // and port are never rendered by any transport.
    if protocol == OutputProtocol::Hls {
        let endpoint = Endpoint::local_directory(rest).map_err(|error| {
            AppFailure(format!(
                "stream destination endpoint was refused: {error:?}"
            ))
        })?;
        return Ok((OutputProtocol::Hls, endpoint, None));
    }
    if protocol == OutputProtocol::Omt {
        let (host, port) = rest
            .rsplit_once(':')
            .ok_or_else(|| AppFailure("stream destination endpoint has no explicit port".into()))?;
        if rest.contains('/') {
            return Err(AppFailure("stream destination endpoint has a path".into()).into());
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| AppFailure("stream destination endpoint port is invalid".into()))?;
        let endpoint = Endpoint::new(host, port, "/").map_err(|error| {
            AppFailure(format!(
                "stream destination endpoint was refused: {error:?}"
            ))
        })?;
        return Ok((OutputProtocol::Omt, endpoint, None));
    }
    let (authority, endpoint_path, key_text) = if protocol == OutputProtocol::Srt {
        match rest.split_once('?') {
            Some((authority, query)) => {
                let key = match query.strip_prefix("streamid=") {
                    Some(key) => Some(key),
                    None => {
                        return Err(AppFailure(
                            "stream destination query is not a stream id".into(),
                        )
                        .into());
                    }
                };
                (authority, None, key)
            }
            None => (rest, None, None),
        }
    } else {
        match rest.split_once('/') {
            Some((authority, path)) => {
                // The final path segment is the stream key; what precedes it
                // is the application path.
                let (application, key) = path.rsplit_once('/').ok_or_else(|| {
                    AppFailure("stream destination endpoint has no application path".into())
                })?;
                (authority, Some(format!("/{application}")), Some(key))
            }
            None => {
                return Err(AppFailure(
                    "stream destination endpoint has no application path".into(),
                )
                .into());
            }
        }
    };
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| AppFailure("stream destination endpoint has no explicit port".into()))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| AppFailure("stream destination endpoint port is invalid".into()))?;
    let endpoint =
        Endpoint::new(host, port, endpoint_path.as_deref().unwrap_or("/")).map_err(|error| {
            AppFailure(format!(
                "stream destination endpoint was refused: {error:?}"
            ))
        })?;
    // The key is validated for the protocol that will carry it: an SRT
    // stream id is one query value and may not split into more parameters.
    // Only the typed refusal is rendered, never the key text.
    let stream_key = key_text
        .map(|key| StreamKey::for_protocol(protocol, key))
        .transpose()
        .map_err(|error| AppFailure(format!("stream destination key was refused: {error}")))?;
    Ok((protocol, endpoint, stream_key))
}

/// Renders the destination with its key masked, byte-for-byte like
/// `StreamDestination::redacted` renders the same URL. An HLS destination has
/// no key: its absolute directory is operator-chosen local storage rather than
/// a secret, so it is rendered verbatim as `hls:///absolute/path`.
#[cfg(feature = "native-media")]
pub(crate) fn redacted_destination_url(
    protocol: OutputProtocol,
    endpoint: &Endpoint,
    keyed: bool,
) -> String {
    match protocol {
        OutputProtocol::Rtmp | OutputProtocol::Rtmps => {
            let scheme = if protocol == OutputProtocol::Rtmp {
                "rtmp"
            } else {
                "rtmps"
            };
            if keyed {
                format!(
                    "{scheme}://{}:{}{}/****",
                    endpoint.host(),
                    endpoint.port(),
                    endpoint.path()
                )
            } else {
                format!(
                    "{scheme}://{}:{}{}",
                    endpoint.host(),
                    endpoint.port(),
                    endpoint.path()
                )
            }
        }
        OutputProtocol::Srt => {
            let mut url = format!("srt://{}:{}", endpoint.host(), endpoint.port());
            if keyed {
                url.push_str("?streamid=****");
            }
            url
        }
        // A local HLS destination is its absolute directory: there is no key
        // to mask, and the placeholder host and port are never rendered.
        OutputProtocol::Hls => format!("hls://{}", endpoint.path()),
        OutputProtocol::Omt => format!("omt://{}:{}", endpoint.host(), endpoint.port()),
        // LAN outputs have no daemon streaming transport yet.
        OutputProtocol::LiveLan => String::new(),
    }
}

/// One target's validated destination pieces, split before grouping so
/// rendition grouping happens over complete, already-validated inputs, plus
/// its rendition group's position.
#[cfg(feature = "native-media")]
pub(crate) struct SplitStreamTarget<'a> {
    pub(crate) target: &'a fm_model::StreamTarget,
    pub(crate) protocol: OutputProtocol,
    pub(crate) endpoint: Endpoint,
    pub(crate) stream_key: Option<StreamKey>,
    pub(crate) backup_endpoint: Option<Endpoint>,
    pub(crate) group_index: usize,
}

/// Grouped target splits plus the distinct encoder-identity keys in
/// first-appearance order.
#[cfg(feature = "native-media")]
pub(crate) struct SplitTargets<'a> {
    pub(crate) splits: Vec<SplitStreamTarget<'a>>,
    pub(crate) group_keys: Vec<(fm_types::OutputId, OutputProtocol, u32)>,
}

/// Splits every target's endpoints and assigns rendition groups by first
/// appearance of the encoder-relevant key — authored project output, output
/// protocol, plus authored video bitrate.
///
/// # Errors
///
/// Returns any destination URL that would be refused at connect time.
#[cfg(feature = "native-media")]
pub(crate) fn split_and_group_targets(
    targets: &[fm_model::StreamTarget],
) -> AppResult<SplitTargets<'_>> {
    let mut splits = Vec::with_capacity(targets.len());
    for target in targets {
        let (protocol, endpoint, stream_key, backup_endpoint) = split_target_endpoints(target)?;
        splits.push(SplitStreamTarget {
            target,
            protocol,
            endpoint,
            stream_key,
            backup_endpoint,
            group_index: 0,
        });
    }
    let mut group_keys: Vec<(fm_types::OutputId, OutputProtocol, u32)> = Vec::new();
    for split in &mut splits {
        let key = (
            split.target.output(),
            split.protocol,
            split.target.video_bitrate_kbps(),
        );
        split.group_index = group_keys
            .iter()
            .position(|existing| *existing == key)
            .unwrap_or_else(|| {
                group_keys.push(key);
                group_keys.len() - 1
            });
    }
    Ok(SplitTargets { splits, group_keys })
}

/// The absolute directory of every authored local HLS target, in authored
/// order.
///
/// An HLS target splits into one absolute directory instead of a network
/// endpoint, and the sink creates that leaf directory later, so both the
/// startup preflight and the runtime free-space sampler measure the nearest
/// existing ancestor. Bounded by the engine's five-target cap.
#[cfg(feature = "native-media")]
pub(crate) fn local_output_directories(splits: &[SplitStreamTarget<'_>]) -> Vec<PathBuf> {
    splits
        .iter()
        .filter(|split| split.protocol == OutputProtocol::Hls)
        .map(|split| PathBuf::from(split.endpoint.path()))
        .collect()
}

/// Filters a compiled destination listing down to the local `hls://` roots,
/// keyed by the engine stream target id.
///
/// Only local HLS destinations contribute a root; every network destination is
/// skipped. Duplicate target ids are refused rather than letting the last one
/// win, because two roots under one id would serve the wrong ladder.
///
/// # Errors
///
/// Returns [`hls_server::HlsRouteError::DuplicateTarget`] when two HLS
/// destinations name the same target id.
#[cfg(feature = "native-media")]
pub(crate) fn hls_roots_from_destinations(
    destinations: impl IntoIterator<Item = (OutputProtocol, u128, PathBuf)>,
) -> Result<HlsRoots, hls_server::HlsRouteError> {
    HlsRoots::try_from_targets(
        destinations
            .into_iter()
            .filter_map(|(protocol, target, root)| {
                (protocol == OutputProtocol::Hls).then_some((target, root))
            }),
    )
}

/// Builds the HLS route table from the daemon's compiled stream destinations.
///
/// Each local `hls://` destination is keyed by the same engine stream target id
/// that `FREEMIXD_PREFLIGHT` and `FREEMIXD_STREAM` records carry.
///
/// # Errors
///
/// Returns [`hls_server::HlsRouteError::DuplicateTarget`] if two compiled
/// destinations name the same target id, which the engine's target inventory
/// should already prevent.
#[cfg(feature = "native-media")]
pub(crate) fn hls_roots(
    native: Option<&NativeDaemon>,
) -> Result<HlsRoots, hls_server::HlsRouteError> {
    let destinations = native.into_iter().flat_map(|native| {
        native.streams.iter().filter_map(|stream| {
            let config = native.output_set.config(stream.destination_id)?;
            Some((
                config.protocol(),
                stream.target.get().get(),
                PathBuf::from(config.endpoint().path()),
            ))
        })
    });
    hls_roots_from_destinations(destinations)
}

/// Builds the pending network probe for one compiled destination's endpoint,
/// or `None` when that endpoint cannot or should not be probed.
///
/// A local HLS destination is a directory write with no peer, so it yields
/// `None`. The label must be [`preflight::PRIMARY_ENDPOINT`] or
/// [`preflight::BACKUP_ENDPOINT`]; a backup probe exists only when the
/// destination configured a backup endpoint. The returned probe owns a copy
/// of the host string, and no record ever renders it: the preflight record
/// carries the target id and endpoint label only.
#[cfg(feature = "native-media")]
pub(crate) fn pending_network_probe(
    config: &DestinationConfig,
    target: u128,
    endpoint: &'static str,
) -> Option<preflight::PendingProbe> {
    if config.protocol() == OutputProtocol::Hls {
        return None;
    }
    let endpoint_config = if endpoint == preflight::PRIMARY_ENDPOINT {
        Some(config.endpoint())
    } else if endpoint == preflight::BACKUP_ENDPOINT {
        config.backup_endpoint()
    } else {
        None
    }?;
    Some(preflight::PendingProbe {
        target,
        endpoint,
        host: endpoint_config.host().to_owned(),
        port: endpoint_config.port(),
        timeout: preflight::NETWORK_PREFLIGHT_TIMEOUT,
    })
}

/// Builds one shared state bundle per rendition group: one readback owner for
/// the whole group and the planner variants that pin the group's rendition id
/// inside planner-produced plans. Group ids are their one-based positions.
///
/// # Errors
///
/// Returns any GPU readback-owner or rendition-identity failure.
#[cfg(feature = "native-media")]
pub(crate) fn build_stream_groups(
    runtime: &NativeMediaRuntime,
    format: &RecordFormat,
    width: NonZeroU32,
    height: NonZeroU32,
    group_keys: &[(fm_types::OutputId, OutputProtocol, u32)],
    next_sequence: u64,
) -> AppResult<Vec<StreamRenditionGroup>> {
    let mut groups = Vec::with_capacity(group_keys.len());
    for (group_index, key) in group_keys.iter().enumerate() {
        groups.push(new_stream_group(
            runtime,
            format,
            (width, height),
            *key,
            (group_index, group_keys.len()),
            next_sequence,
        )?);
    }
    Ok(groups)
}

/// Builds one rendition group: its own readback owner, and the planner
/// variants pinning its rendition id at `position.0 + 1` among
/// `position.1` groups.
///
/// # Errors
///
/// Returns any GPU readback-owner or rendition-identity failure.
#[cfg(feature = "native-media")]
pub(crate) fn new_stream_group(
    runtime: &NativeMediaRuntime,
    format: &RecordFormat,
    (width, height): (NonZeroU32, NonZeroU32),
    key: StreamGroupKey,
    (group_index, group_count): (usize, usize),
    next_sequence: u64,
) -> AppResult<StreamRenditionGroup> {
    let (output, _, video_bitrate_kbps) = key;
    let readback = runtime.create_program_readback_blocking(width, height)?;
    Ok(StreamRenditionGroup {
        core: StreamRenditionCore {
            output,
            members: Vec::new(),
            rendition: RenditionId::new(
                NonZeroU32::new(u32::try_from(group_index + 1).unwrap_or(u32::MAX))
                    .unwrap_or(NonZeroU32::MIN),
            ),
            variants: pinned_rendition_variants(
                format,
                video_bitrate_kbps,
                group_index,
                group_count,
            )?,
            next_sequence,
        },
        format: format.clone(),
        readback,
        key,
    })
}

/// The Program audio of one realized frame: the faded mix every output Fade
/// to Black affects hears, while it fades and excludes some output the mix
/// before the fade, and the mix of every output fading over its own
/// duration on a different trajectory (SW-004).
#[cfg(feature = "native-media")]
pub(crate) struct CaptureAudio<'a> {
    pub(crate) faded: AudioBlock,
    pub(crate) live: Option<AudioBlock>,
    pub(crate) excluded: &'a [fm_types::OutputId],
    pub(crate) overrides: &'a [(fm_types::OutputId, AudioBlock)],
}

#[cfg(feature = "native-media")]
impl CaptureAudio<'_> {
    /// The mix a capture of `selected` hears.
    pub(crate) fn for_output(
        &self,
        selected: Option<fm_types::OutputId>,
        project_outputs: &[NativeOutputFrame],
    ) -> &AudioBlock {
        let output = capture_output_id(selected, project_outputs);
        if let Some((_, block)) = self
            .overrides
            .iter()
            .find(|(candidate, _)| Some(*candidate) == output)
        {
            return block;
        }
        match &self.live {
            Some(live) if output.is_some_and(|output| self.excluded.contains(&output)) => live,
            _ => &self.faded,
        }
    }
}

/// The project output a capture of `selected` follows: the selection, or
/// the first realized output when it selects none.
#[cfg(feature = "native-media")]
pub(crate) fn capture_output_id(
    selected: Option<fm_types::OutputId>,
    project_outputs: &[NativeOutputFrame],
) -> Option<fm_types::OutputId> {
    selected.or_else(|| project_outputs.first().map(NativeOutputFrame::output))
}

/// Resolves an optional project output selection against the realized per-output
/// entries for a frame, falling back to `unbound_output` when the project
/// declares no outputs.
#[cfg(any(test, feature = "native-media"))]
pub(crate) fn resolve_project_output_entry<'a, F, T: ?Sized>(
    selected: Option<fm_types::OutputId>,
    project_outputs: &'a [F],
    unbound_output: Option<&'a T>,
    output_id: impl Fn(&F) -> fm_types::OutputId,
    extract: impl Fn(&'a F) -> &'a T,
) -> Option<&'a T> {
    match selected {
        Some(target) => {
            if project_outputs.is_empty() {
                unbound_output
            } else {
                project_outputs
                    .iter()
                    .find(|frame| output_id(frame) == target)
                    .map(extract)
            }
        }
        None => project_outputs.first().map(extract).or(unbound_output),
    }
}

/// Resolves an optional project output selection to the realized texture for
/// the current frame.
#[cfg(feature = "native-media")]
pub(crate) fn resolve_project_output_texture<'a>(
    selected: Option<fm_types::OutputId>,
    project_outputs: &'a [NativeOutputFrame],
    unbound_output: Option<&'a NativeTexture>,
) -> Option<&'a NativeTexture> {
    resolve_project_output_entry(
        selected,
        project_outputs,
        unbound_output,
        NativeOutputFrame::output,
        NativeOutputFrame::texture,
    )
}

/// Resolves a display output's source to the realized texture for the
/// current frame. Program is the first realized project output (or the
/// unbound output); Preview is the Preview composite rendered for display
/// outputs.
#[cfg(feature = "native-media")]
pub(crate) fn resolve_display_source_texture<'a>(
    source: DisplayOutputSource,
    project_outputs: &'a [NativeOutputFrame],
    unbound_output: Option<&'a NativeTexture>,
    preview: Option<&'a NativeTexture>,
) -> Option<&'a NativeTexture> {
    match source {
        DisplayOutputSource::Program => {
            resolve_project_output_texture(None, project_outputs, unbound_output)
        }
        DisplayOutputSource::Output(output) => {
            resolve_project_output_texture(Some(output), project_outputs, unbound_output)
        }
        DisplayOutputSource::Preview => preview,
    }
}

/// `frame` as Preview shows it: the realized Preview input cut in, with no
/// transition, Stinger, overlay, title, or Fade to Black. Scenes and media
/// keep the frame's poses and transport positions.
#[cfg(feature = "native-media")]
pub(crate) fn preview_frame(mut frame: FrameResult, preview: InputId) -> FrameResult {
    frame.program = fm_switcher::ProgramFrame {
        primary: preview,
        secondary: None,
        transition_kind: None,
        mix_numerator: 0,
        mix_denominator: 1,
        mix_start_numerator: 0,
        mix_end_numerator: 0,
    };
    frame.fade_to_black = fm_switcher::FadeToBlackFrame::LIVE;
    frame.fade_to_black_overrides.clear();
    frame.manual_transition = false;
    frame.overlays = std::array::from_fn(|_| fm_switcher::OverlayChannelState::empty());
    frame.titles.clear();
    frame.events.clear();
    frame.input_audio_strip_updates.clear();
    frame.audio_mixer_updates.clear();
    frame
}

#[cfg(feature = "native-media")]
impl NativeProgramStream {
    /// Compiles one runtime per configured project destination in project
    /// vector order, registering each target in the shared output set as
    /// destination slot 1..=N, and groups targets whose encoder-relevant
    /// settings — output protocol plus authored video bitrate — match into
    /// one shared-rendition group owning one readback and one packed pair
    /// per frame.
    ///
    /// Also yields the absolute directory of every authored local HLS target,
    /// in authored order, for the daemon's startup preflight and runtime
    /// free-space sampler.
    ///
    /// Startup fails closed on more targets than the engine inventories or the
    /// output set can carry, or a destination URL that would be refused at
    /// connect time; all three are authoring errors the model validation
    /// mirrors, so refusing before any process spawns keeps them visible
    /// instead of latching every target failed at air time.
    pub(crate) fn compile(
        runtime: &NativeMediaRuntime,
        stored: &StoredProject,
        output_set: &mut OutputSet,
    ) -> AppResult<(Vec<Self>, Vec<StreamRenditionGroup>, Vec<PathBuf>)> {
        let targets = stored.project().stream_targets();
        if targets.len() > fm_switcher::MAX_STREAM_COUNT {
            return Err(AppFailure(format!(
                "stream target count {} exceeds the engine inventory limit {}",
                targets.len(),
                fm_switcher::MAX_STREAM_COUNT
            ))
            .into());
        }
        if targets.len() > fm_io_network::MAX_DESTINATIONS {
            return Err(AppFailure(format!(
                "stream target count {} exceeds the output set limit {}",
                targets.len(),
                fm_io_network::MAX_DESTINATIONS
            ))
            .into());
        }
        let settings = stored.project().settings();
        let frames_rendered = stored.position().frames_rendered;
        // The record format derives from project settings alone, so it is
        // identical across every target by construction; each group packs its
        // pairs with a clone of this one format.
        let format = stream_record_format(settings, frames_rendered)?;
        let split_targets = split_and_group_targets(targets)?;
        let local_output_dirs = local_output_directories(&split_targets.splits);
        let mut groups = build_stream_groups(
            runtime,
            &format,
            NonZeroU32::new(settings.video.dimensions.width()).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(settings.video.dimensions.height()).unwrap_or(NonZeroU32::MIN),
            &split_targets.group_keys,
            frames_rendered,
        )?;
        // One transport per target under its group's rendition identity;
        // sinks refuse packets claiming any other rendition.
        let mut streams = Vec::with_capacity(split_targets.splits.len());
        for (index, split) in split_targets.splits.into_iter().enumerate() {
            // Destination slots are 1..=N in authored order; both bounds were
            // checked above.
            let ordinal = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let destination_id = DestinationId::new(u8::try_from(ordinal).unwrap_or(u8::MAX))
                .unwrap_or(DestinationId::FIRST);
            let group = &mut groups[split.group_index];
            streams.push(Self::realize(
                split,
                destination_id,
                group,
                &format,
                frames_rendered,
                output_set,
            )?);
        }
        Ok((streams, groups, local_output_dirs))
    }

    /// Realizes one validated target as destination slot `destination_id`
    /// in `group`: its own sink under the group's rendition identity, its own
    /// bounded queue and reconnect policy in the shared output set, and fresh
    /// feed bookkeeping starting at `next_sequence`.
    pub(crate) fn realize(
        split: SplitStreamTarget<'_>,
        destination_id: DestinationId,
        group: &mut StreamRenditionGroup,
        format: &RecordFormat,
        next_sequence: u64,
        output_set: &mut OutputSet,
    ) -> AppResult<Self> {
        group.core.members.push(destination_id);
        let mut sink_config = RtmpSinkConfig::new(group.core.rendition, format.clone());
        sink_config.limits.stop_timeout = PROGRAM_STREAM_STOP_TIMEOUT;
        sink_config.limits.kill_timeout = PROGRAM_STREAM_KILL_TIMEOUT;
        sink_config.encoder = stream_encoder_settings(split.target.video_bitrate_kbps());
        sink_config.stream_key = split.stream_key;
        sink_config.ladder = stream_ladder(split.target);
        sink_config.srt = stream_srt_options(split.target);
        let queue_capacity = sink_config.limits.max_outstanding_pairs;
        let config = DestinationConfig::new(
            destination_id,
            split.protocol,
            split.endpoint.clone(),
            split.backup_endpoint,
            // TLS policy was already settled by the protocol rules:
            // plain RTMP and SRT refuse it and RTMPS uses the
            // child's default trust, so no daemon-side TLS exists.
            None,
            None,
            QueueCapacity::new(PROGRAM_STREAM_QUEUE_CAPACITY).unwrap_or(QueueCapacity::DEFAULT),
            stream_reconnect_policy(split.target.reconnect()),
        )
        .map_err(|error| AppFailure(format!("compiled destination: {error}")))?;
        output_set
            .add_destination(config)
            .map_err(|error| AppFailure(format!("output set: {error}")))?;
        Ok(Self {
            target: SwitcherStreamTargetId::from_non_zero(split.target.id().get()),
            destination_id,
            redacted_destination: redacted_destination_url(
                split.protocol,
                &split.endpoint,
                sink_config.stream_key.is_some(),
            ),
            sink: stream_transport::BackgroundTransport::new(
                if split.protocol == OutputProtocol::Omt {
                    ProgramStreamSink::Omt(OmtTransportSink::new(sink_config))
                } else {
                    ProgramStreamSink::Ffmpeg(FfmpegRtmpSink::new(sink_config))
                },
                format!("freemix-native-stream-{}", destination_id.get()),
            ),
            ledger: StreamPairLedger {
                next_sequence,
                ..StreamPairLedger::default()
            },
            first_failure: None,
            first_failure_code: None,
            enqueue_backpressure: false,
            started: false,
            meter: StreamLinkMeter::default(),
            queue_capacity,
            authored: split.target.clone().set_running(false),
        })
    }

    /// Clears a latched failure and reconnects this target from the first
    /// attempt (OR-006 retry): the transport stops, and the next frame
    /// boundary starts it again with a fresh reconnect budget.
    pub(crate) fn retry(&mut self, output_set: &mut OutputSet) {
        self.stop_requested(output_set);
    }

    /// This target's own pair queue against its configured capacity, for the
    /// encoder statistics sample; `None` before the target started, while a
    /// transport job holds the sink, or before its streamer opened.
    pub(crate) fn queue_load(&self) -> Option<(u64, u64)> {
        if !self.started {
            return None;
        }
        let telemetry = self.sink.idle_sink()?.stream_telemetry()?;
        Some((
            saturating_u64(telemetry.outstanding_pairs),
            saturating_u64(self.queue_capacity),
        ))
    }

    /// Marks this target running in the shared output set. Connection trouble
    /// does not latch here: the set owns retrying and failover, and only its
    /// own budget exhaustion latches the target through [`Self::poll`].
    pub(crate) fn start(&mut self, output_set: &mut OutputSet) {
        if self.first_failure.is_some() {
            return;
        }
        self.started = true;
        if let Err(error) = output_set.start(self.destination_id) {
            self.fail(&format!("start:{error}"), output_set);
        }
    }

    /// Stops this target off the render path: the set discards queued pairs
    /// and disconnects the transport under its bounded deadlines.
    /// Bookkeeping stays with the target so a later `StreamStart` can relaunch
    /// it and shutdown reports whole-run counters.
    pub(crate) fn stop_live(&mut self, output_set: &mut OutputSet) {
        let _ = output_set.stop(self.destination_id, &mut self.sink);
    }

    /// Stops this target because the operator asked to. The deliberate stop
    /// also clears a latched failure, so a later `StreamStart` relaunches the
    /// target instead of leaving it failed for the rest of the run.
    pub(crate) fn stop_requested(&mut self, output_set: &mut OutputSet) {
        self.stop_live(output_set);
        self.first_failure = None;
        self.first_failure_code = None;
    }

    /// Whether any transport child this target started was not reaped within
    /// its deadline; shutdown reports this instead of abandoning it silently.
    ///
    /// Waits for the transport worker's outstanding stop, which the sink
    /// bounds by its own stop and kill deadlines; only shutdown calls this.
    pub(crate) fn shutdown_error(&mut self) -> Option<AppResult<()>> {
        let Some(sink) = self.sink.settle() else {
            return Some(Err(AppFailure(format!(
                "stream target {} did not shut down cleanly: transport worker exited",
                self.target
            ))
            .into()));
        };
        let unconfirmed = sink.unconfirmed_cleanups();
        if unconfirmed == 0 {
            return None;
        }
        Some(Err(AppFailure(format!(
            "stream target {} did not shut down cleanly: unconfirmed_cleanups={unconfirmed}",
            self.target
        ))
        .into()))
    }

    /// Advances this target one step in the shared output set after the
    /// frame's enqueues: connects, backs off, fails over, and writes.
    ///
    /// The first failure the set cannot recover from latches the target with
    /// one sanitized notice, exactly like every other feed failure.
    ///
    /// A connect the set is about to make runs on the transport worker: until
    /// it is realized this destination is not polled at all, so the set stays
    /// `Connecting` (or due to reconnect) and the daemon loop keeps running.
    pub(crate) fn poll(&mut self, output_set: &mut OutputSet, now_ms: u64) {
        self.sink.advance();
        if self.first_failure.is_some() {
            return;
        }
        if let Some(ready) = stream_transport::connect_due(output_set, self.destination_id, now_ms)
            .map(|(config, endpoint)| self.sink.prepare_connect(config, endpoint))
            && !ready
        {
            return;
        }
        match output_set.poll(self.destination_id, now_ms, &mut self.sink) {
            Ok(PollEvent::Failed) => {
                match output_set
                    .telemetry(self.destination_id)
                    .and_then(|telemetry| telemetry.latest_failure())
                    .map(|failure| {
                        (
                            format!("transport:{:?}:{}", failure.stage, failure.message),
                            output_failure::stream_failure(failure),
                        )
                    }) {
                    Some((failure, code)) => self.fail_with(&failure, code, output_set),
                    None => self.fail("transport:budget_exhausted", output_set),
                }
            }
            Ok(_) => {}
            Err(error) => self.fail(&format!("transport:{error}"), output_set),
        }
    }

    /// Latches a failure the daemon raised itself (see
    /// [`output_failure::latched_stream_failure`]).
    pub(crate) fn fail(&mut self, failure: &str, output_set: &mut OutputSet) {
        self.fail_with(
            failure,
            output_failure::latched_stream_failure(failure),
            output_set,
        );
    }

    /// Latches the first failure per target with what peers see of it, and
    /// stops its transport under the bounded stop deadline.
    pub(crate) fn fail_with(
        &mut self,
        failure: &str,
        code: OutputFailure,
        output_set: &mut OutputSet,
    ) {
        if self.first_failure.is_some() {
            return;
        }
        self.first_failure = Some(failure.to_owned());
        self.first_failure_code = Some(code);
        eprintln!(
            "FREEMIXD_STREAM_FAILURE\tv=1\ttarget={}\tdestination={}\tfailure={}",
            self.target,
            diagnostic_field(&self.redacted_destination),
            diagnostic_field(failure),
        );
        let _ = output_set.stop(self.destination_id, &mut self.sink);
    }

    /// Emits the single sanitized end-of-run record for this target from the
    /// shared output set's frozen state and telemetry plus the feed ledger.
    ///
    /// Only redacted destinations and network telemetry fields reach the
    /// record: neither the stream key nor child stderr URL text can appear.
    pub(crate) fn emit_final_record(&self, output_set: &OutputSet) {
        let state = output_set.state(self.destination_id);
        let telemetry = output_set.telemetry(self.destination_id);
        let muxed_bytes = telemetry.map_or(0, NetworkTelemetry::bytes_sent);
        let out_time_us = telemetry
            .and_then(fm_io_network::NetworkTelemetry::last_successful_media_timestamp)
            .map_or(0, |timestamp| {
                u64::try_from(timestamp.as_nanos() / 1_000).unwrap_or(u64::MAX)
            });
        let connected = matches!(
            state,
            Some(
                DestinationState::Live
                    | DestinationState::Congested
                    | DestinationState::AwaitingRandomAccess
            )
        );
        let state_name = match state {
            _ if self.first_failure.is_some() => "Failed",
            None => "NotStarted",
            Some(DestinationState::Failed) => "Failed",
            Some(DestinationState::Stopped) => "Stopped",
            Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => {
                "Starting"
            }
            Some(DestinationState::WaitingToReconnect { .. }) => "WaitingToReconnect",
            Some(DestinationState::Congested) => "Congested",
            Some(DestinationState::Live) => "Live",
        };
        let failure_text = self
            .first_failure
            .clone()
            .or_else(|| {
                telemetry
                    .and_then(|telemetry| telemetry.latest_failure())
                    .map(|failure| format!("transport:{:?}:{}", failure.stage, failure.message))
            })
            .unwrap_or_else(|| "none".to_owned());
        eprintln!(
            "FREEMIXD_STREAM\tv=1\ttarget={}\tdestination={}\tstate={state_name}\tmuxed_bytes={muxed_bytes}\tout_time_us={out_time_us}\tconnected={connected}\tenqueued_pairs={}\tdropped_pairs={}\tfailure={}",
            self.target,
            diagnostic_field(&self.redacted_destination),
            self.ledger.enqueued_pairs,
            self.ledger.dropped_pairs,
            diagnostic_field(&failure_text),
        );
    }
}

/// Whether a target's route joins this frame's rendition fan-out: it was
/// asked to run, never latched a failure, and was not deliberately stopped.
/// Stopped and unstarted destinations are excluded from the plan itself, so
/// nothing is ever queued for a route nobody drains.
#[cfg(feature = "native-media")]
pub(crate) fn route_active(stream: &NativeProgramStream, output_set: &OutputSet) -> bool {
    stream.started
        && stream.first_failure.is_none()
        && !matches!(
            output_set.state(stream.destination_id),
            Some(DestinationState::Stopped)
        )
}

/// The active stream targets' aggregate network state, bounded to the
/// configured target count (RC-015).
#[cfg(feature = "native-media")]
pub(crate) fn network_sample(
    output_set: &OutputSet,
    streams: &[NativeProgramStream],
) -> statistics::NetworkSample {
    let mut network = statistics::NetworkSample::default();
    for stream in streams.iter().take(fm_switcher::MAX_STREAM_COUNT) {
        if !route_active(stream, output_set) {
            continue;
        }
        network.active_targets = network.active_targets.saturating_add(1);
        match output_set.state(stream.destination_id) {
            Some(
                DestinationState::Live
                | DestinationState::Congested
                | DestinationState::AwaitingRandomAccess,
            ) => network.connected_targets = network.connected_targets.saturating_add(1),
            Some(DestinationState::WaitingToReconnect { .. }) => {
                network.reconnecting_targets = network.reconnecting_targets.saturating_add(1);
            }
            _ => {}
        }
        network.bitrate_kbps = network
            .bitrate_kbps
            .saturating_add(u64::from(stream.meter.bitrate_kbps));
        if let Some(loss) = output_set
            .telemetry(stream.destination_id)
            .and_then(NetworkTelemetry::packet_loss_ppm)
        {
            network.packet_loss_ppm = Some(
                network
                    .packet_loss_ppm
                    .map_or(loss, |worst| worst.max(loss)),
            );
        }
    }
    network
}

/// The running recorder slots' and started stream targets' encoder queues,
/// bounded to the two slots and the configured target count (RC-015). An
/// output whose queue cannot be read this sample is skipped, never folded as
/// an empty queue.
#[cfg(feature = "native-media")]
pub(crate) fn encoder_sample(
    recorders: &[ProgramRecording; RECORDER_SLOTS],
    streams: &[NativeProgramStream],
) -> statistics::EncoderSample {
    let mut encoder = statistics::EncoderSample::default();
    for slot in recorders {
        if let ProgramRecording::Running(recorder) = slot
            && let Some((outstanding, capacity)) = recorder.queue_load()
        {
            encoder.observe(outstanding, capacity);
        }
    }
    for stream in streams.iter().take(fm_switcher::MAX_STREAM_COUNT) {
        if let Some((outstanding, capacity)) = stream.queue_load() {
            encoder.observe(outstanding, capacity);
        }
    }
    encoder
}

/// One rendition group's share of the frame pipeline.
#[cfg(feature = "native-media")]
pub(crate) enum GroupFrameOutcome {
    /// Nothing to send: the group has no active member route, or this
    /// frame's sequence was already consumed.
    Idle,
    /// One packed pair plus the plan that routes it to the active members.
    Packed {
        plan: RenditionPlan,
        packet: OutputPacket,
    },
    /// The shared step failed; every active member latches its text.
    Failed(Vec<(DestinationId, String)>),
}

/// Gates, plans, and packs one rendition group's pair for this frame — split
/// from the GPU-owning daemon so hermetic tests drive the exact production
/// sequencing with a scripted pack step instead of a readback.
///
/// A group with no active member packs nothing (its readback idles). The
/// sequence cursor advances past this frame once the group commits to
/// packing: a packed pair is never repacked because sinks pad gaps to hold
/// the media clock. Plans cover exactly the active members; planner failures
/// and pack failures return per-member texts to latch.
#[cfg(feature = "native-media")]
pub(crate) fn plan_and_pack_group_frame(
    members: &[DestinationId],
    variants: &[AbrVariant],
    next_sequence: &mut u64,
    sequence: SequenceNumber,
    active: &[bool],
    pack: impl FnOnce() -> Result<OutputPacket, String>,
) -> GroupFrameOutcome {
    let active_members: Vec<DestinationId> = members
        .iter()
        .zip(active)
        .filter_map(|(member, &is_active)| is_active.then_some(*member))
        .collect();
    if active_members.is_empty() || sequence.get() < *next_sequence {
        return GroupFrameOutcome::Idle;
    }
    *next_sequence = sequence.get().saturating_add(1);
    let plan = match build_group_plan(members, variants, active) {
        Ok(plan) => plan,
        Err(error) => {
            return GroupFrameOutcome::Failed(
                active_members
                    .into_iter()
                    .map(|destination| (destination, format!("planner:{error}")))
                    .collect(),
            );
        }
    };
    match pack() {
        Ok(packet) => GroupFrameOutcome::Packed { plan, packet },
        Err(failure) => GroupFrameOutcome::Failed(
            active_members
                .into_iter()
                .map(|destination| (destination, failure.clone()))
                .collect(),
        ),
    }
}

/// Builds the planner request set for one group over its active members:
/// every active member requests the group's variant ladder, so the planner
/// deduplicates them into renditions whose ids match the compile-time group
/// ids (the real profile sits at the group's pinned position).
///
/// # Errors
///
/// Returns [`fm_io_network::RenditionError`] for an empty request set or any
/// other planner rule violation.
#[cfg(feature = "native-media")]
pub(crate) fn build_group_plan(
    members: &[DestinationId],
    variants: &[AbrVariant],
    active: &[bool],
) -> Result<RenditionPlan, fm_io_network::RenditionError> {
    let mut requests = Vec::new();
    for (member, is_active) in members.iter().zip(active) {
        if !is_active {
            continue;
        }
        let request = match variants {
            [only] => DestinationRenditions::single(*member, only.profile().clone()),
            many => DestinationRenditions::ladder(
                *member,
                AbrLadder::new(many.to_vec())
                    .map_err(|_| fm_io_network::RenditionError::AbrBitrateOrder)?,
            ),
        };
        requests.push(request);
    }
    RenditionPlanner::plan(&requests)
}

/// Maps one fanned-out packet's per-route results back onto the member
/// ledgers: an accepted route advances that member's counter only, a refused
/// route drops that member's counter and flags its backpressure, and a route
/// to a destination missing from the set drops without touching anything
/// else. A refused pair is gone — never retried — because the sink pads its
/// gap to hold the media clock.
#[cfg(feature = "native-media")]
pub(crate) fn admit_rendition_outcomes(
    outcomes: &[DestinationEnqueue],
    sequence: SequenceNumber,
    routes: &mut [(DestinationId, &mut StreamPairLedger, &mut bool)],
) {
    for outcome in outcomes {
        let Some((_, ledger, enqueue_backpressure)) = routes
            .iter_mut()
            .find(|(destination, _, _)| *destination == outcome.destination)
        else {
            continue;
        };
        match outcome.status {
            EnqueueStatus::Accepted => ledger.note_admitted(sequence),
            EnqueueStatus::Backpressure(_) => {
                ledger.note_dropped();
                **enqueue_backpressure = true;
            }
            EnqueueStatus::DestinationUnavailable(_) => ledger.note_dropped(),
        }
    }
}

/// The Program readbacks of one frame, shared by every consumer that reads
/// the same output texture at the same size: the stream rendition groups
/// and then the recorder slots.
///
/// A readback is a GPU pass, a copy into a staging buffer, and a blocking
/// wait for the device, all on the frame loop. A recorder and a stream
/// group that both take Program at the project's size used to pay for it
/// twice per frame; they now share one. Owners of the same size convert
/// identically ([`NativeMediaRuntime::create_program_readback_blocking`]),
/// so a shared readback holds exactly the bytes either owner would have
/// read.
#[cfg(feature = "native-media")]
#[derive(Default)]
pub(crate) struct FrameReadbacks<'frame> {
    read: Vec<(&'frame NativeTexture, u32, u32, fm_gpu::DiagnosticReadback)>,
}

#[cfg(feature = "native-media")]
impl<'frame> FrameReadbacks<'frame> {
    pub(crate) fn position(
        &self,
        program: &NativeTexture,
        owner: &NativeProgramReadback,
    ) -> Option<usize> {
        self.read.iter().position(|(texture, width, height, _)| {
            std::ptr::eq(*texture, program) && *width == owner.width() && *height == owner.height()
        })
    }

    /// `program` read back through `owner`, reading it only if no consumer
    /// of this frame read it at this size yet.
    pub(crate) fn read(
        &mut self,
        runtime: &NativeMediaRuntime,
        owner: &mut NativeProgramReadback,
        program: &'frame NativeTexture,
    ) -> Result<&fm_gpu::DiagnosticReadback, freemixd::native_media::NativeMediaError> {
        let index = if let Some(index) = self.position(program, owner) {
            index
        } else {
            let readback = runtime.readback_program_blocking(owner, program)?;
            self.read
                .push((program, owner.width(), owner.height(), readback));
            self.read.len() - 1
        };
        Ok(&self.read[index].3)
    }

    /// Like [`Self::read`], but hands the readback over: a consumer that
    /// keeps the pixels takes a shared readback without copying it, and a
    /// later consumer of the same picture reads it again.
    pub(crate) fn take(
        &mut self,
        runtime: &NativeMediaRuntime,
        owner: &mut NativeProgramReadback,
        program: &NativeTexture,
    ) -> Result<fm_gpu::DiagnosticReadback, freemixd::native_media::NativeMediaError> {
        match self.position(program, owner) {
            Some(index) => Ok(self.read.swap_remove(index).3),
            None => runtime.readback_program_blocking(owner, program),
        }
    }
}

/// The production pack step for one group: ONE synchronous program readback
/// through the shared owner (or the one another consumer of the same
/// picture already made this frame, see [`FrameReadbacks`]), layout validation, and ONE raw pair packed under
/// the group's rendition identity. This is the existing synchronous
/// diagnostic readback contract, not a nonblocking or zero-copy encoder
/// bridge; the failure texts mirror the per-target path they replaced.
#[cfg(feature = "native-media")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn pack_group_pair<'frame>(
    runtime: &NativeMediaRuntime,
    program: &'frame NativeTexture,
    readback: &mut NativeProgramReadback,
    readbacks: &mut FrameReadbacks<'frame>,
    format: &RecordFormat,
    rendition: RenditionId,
    sequence: SequenceNumber,
    audio: &AudioBlock,
) -> Result<OutputPacket, String> {
    // The expensive synchronous step: one per picture and size per frame.
    let frame = readbacks
        .read(runtime, readback, program)
        .map_err(|error| format!("readback:{error}"))?;
    let Some(expected_stride) = frame.width.checked_mul(4) else {
        return Err("readback:stride_overflow".to_owned());
    };
    if frame.width != readback.width()
        || frame.height != readback.height()
        || frame.stride != expected_stride
    {
        return Err("readback:invalid_tight_layout".to_owned());
    }
    raw_pair_packet(
        rendition,
        format,
        sequence.get(),
        &frame.rgba,
        &interleaved_audio_bytes(audio),
    )
    .map_err(|error| format!("frame:{error}"))
}

/// Serializes one [`AudioBlock`] into the sample-major `f32le` span the raw
/// pair packet carries: per sample index, every channel's `f32` back to back.
/// Planes are equal-length by construction.
#[cfg(feature = "native-media")]
pub(crate) fn interleaved_audio_bytes(block: &AudioBlock) -> Vec<u8> {
    let planes = block.planes();
    let samples = planes.first().map_or(0, Vec::len);
    let mut bytes = Vec::with_capacity(samples * planes.len() * size_of::<f32>());
    for index in 0..samples {
        for plane in planes {
            bytes.extend_from_slice(&plane[index].to_le_bytes());
        }
    }
    bytes
}

/// Plain sampling inputs for one configured stream target, extracted so the
/// status mapping stays testable without a GPU runtime or an `FFmpeg` child.
#[cfg(feature = "native-media")]
pub(crate) struct StreamRuntimeSnapshot {
    pub(crate) target: SwitcherStreamTargetId,
    pub(crate) ledger: StreamPairLedger,
    /// Capacity backpressure observed since the previous sample.
    pub(crate) congested: bool,
    /// What peers see of the latched failure, if any.
    pub(crate) first_failure: Option<OutputFailure>,
    /// Whether this target was ever asked to run.
    pub(crate) started: bool,
    /// The shared output set's view of this target.
    pub(crate) state: Option<DestinationState>,
    pub(crate) network: Option<NetworkTelemetry>,
    /// Latest cached local HLS output health for this target's slot; always
    /// `None` for a network target.
    pub(crate) local: Option<StreamLocalStatus>,
    /// How long the current session has delivered media, in milliseconds.
    pub(crate) uptime_ms: u64,
    /// The encoded outgoing bitrate over the last meter window.
    pub(crate) bitrate_kbps: u32,
}

/// How long one bitrate window spans: two seconds smooths the encoder's
/// progress reports without hiding a stalled link for long.
#[cfg(feature = "native-media")]
pub(crate) const STREAM_BITRATE_WINDOW_MS: u64 = 2_000;

/// Per-target link meter for the lossy status record (OR-006): when the
/// current session started delivering media, and the encoded bitrate the
/// child reports muxing over the last window. Everything resets when the
/// session stops delivering.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamLinkMeter {
    pub(crate) live_since_ms: Option<u64>,
    /// `(at_ms, muxed_bytes)` at the start of the current window.
    pub(crate) window: Option<(u64, u64)>,
    pub(crate) bitrate_kbps: u32,
}

#[cfg(feature = "native-media")]
impl StreamLinkMeter {
    /// Advances the meter to `now_ms` and returns `(uptime_ms, bitrate_kbps)`.
    /// `muxed_bytes` is the open session's cumulative encoded output; a value
    /// below the window's start (a new session) restarts the window.
    pub(crate) fn observe(
        &mut self,
        now_ms: u64,
        delivering: bool,
        muxed_bytes: Option<u64>,
    ) -> (u64, u32) {
        if !delivering {
            *self = Self::default();
            return (0, 0);
        }
        let since = *self.live_since_ms.get_or_insert(now_ms);
        if let Some(bytes) = muxed_bytes {
            match self.window {
                Some((at, start)) if bytes >= start => {
                    let elapsed = now_ms.saturating_sub(at);
                    if elapsed >= STREAM_BITRATE_WINDOW_MS {
                        // Bits per millisecond is kilobits per second.
                        let kbps = (bytes - start).saturating_mul(8) / elapsed;
                        self.bitrate_kbps = u32::try_from(kbps).unwrap_or(u32::MAX);
                        self.window = Some((now_ms, bytes));
                    }
                }
                _ => self.window = Some((now_ms, bytes)),
            }
        }
        (now_ms.saturating_sub(since), self.bitrate_kbps)
    }
}

/// One local HLS output's latest bounded health, or `None` when its published
/// playlist cannot be read or parsed.
///
/// A single-rendition sink publishes `<directory>/index.m3u8`. A ladder sink
/// publishes a master playlist at that path whose first variant is a
/// `<rung>/index.m3u8` inside the same directory, so a master is followed to
/// its first variant before parsing. The variant path is validated against the
/// same plain shapes the HLS listener serves — `index.m3u8` or one rung digit
/// `0..=7` plus `index.m3u8` — so `..`, absolute paths, and backslashes are
/// refused before any read.
///
/// `segments` and `target_duration_ms` come from the bounded M3U8 parser and
/// saturate to the wire bounds; `free_bytes` measures the directory's nearest
/// existing ancestor and is zero when the query is unavailable. Every read,
/// parse, or shape failure returns `None` without latching: a missing or
/// malformed playlist is a lossy sample gap, never a target failure.
#[cfg(feature = "native-media")]
pub(crate) fn local_hls_status(directory: &Path) -> Option<StreamLocalStatus> {
    local_hls_status_with(directory, |ancestor| disk::available_bytes(ancestor).ok())
}

/// [`local_hls_status`] with the free-space query `available`, which is
/// given the directory's nearest existing ancestor. A test pins it: other
/// writers on the same file system change the free space between any two
/// real queries.
#[cfg(feature = "native-media")]
pub(crate) fn local_hls_status_with(
    directory: &Path,
    available: impl FnOnce(&Path) -> Option<u64>,
) -> Option<StreamLocalStatus> {
    let text = std::fs::read_to_string(directory.join("index.m3u8")).ok()?;
    let text = match parse_master_m3u8(&text) {
        Ok(variants) => {
            let variant = variants.first()?;
            if !valid_local_hls_variant_path(variant.playlist_uri()) {
                return None;
            }
            std::fs::read_to_string(directory.join(variant.playlist_uri())).ok()?
        }
        Err(_) => text,
    };
    let rendition = RenditionId::new(NonZeroU32::MIN);
    let metadata = HlsPlaylistMetadata::parse_m3u8(&text, rendition, 1).ok()?;
    let segments = u32::try_from(metadata.segments().len()).unwrap_or(u32::MAX);
    let target_duration_ms =
        u32::try_from(metadata.target_duration().as_nanos() / 1_000_000).unwrap_or(u32::MAX);
    Some(StreamLocalStatus {
        segments: segments.min(MAX_STREAM_LOCAL_SEGMENTS),
        target_duration_ms: target_duration_ms.min(MAX_STREAM_LOCAL_TARGET_DURATION_MS),
        free_bytes: disk::existing_ancestor(directory)
            .and_then(available)
            .unwrap_or(0),
    })
}

/// Whether one master-playlist variant path names a playlist inside the output
/// directory: `index.m3u8` or `<rung>/index.m3u8` with one rung digit `0..=7`,
/// exactly the shapes the HLS listener serves. Empty names, `..`, absolute
/// paths, and backslashes are refused.
#[cfg(feature = "native-media")]
pub(crate) fn valid_local_hls_variant_path(relative: &str) -> bool {
    match relative.split_once('/') {
        None => relative == "index.m3u8",
        Some((rung, leaf)) => {
            let mut bytes = rung.bytes();
            matches!(
                (bytes.next(), bytes.next()),
                (Some(byte), None) if byte.is_ascii_digit() && byte <= b'7'
            ) && leaf == "index.m3u8"
        }
    }
}

/// The cached local HLS health for one destination slot, or `None` for a
/// network target or a slot that has no sampled health yet.
#[cfg(feature = "native-media")]
pub(crate) fn cached_local_hls_status(
    output_set: &OutputSet,
    local_output_status: &[Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    destination: DestinationId,
) -> Option<StreamLocalStatus> {
    let config = output_set.config(destination)?;
    if config.protocol() != OutputProtocol::Hls {
        return None;
    }
    local_output_status
        .get(usize::from(destination.get()) - 1)
        .copied()
        .flatten()
}

/// Assembles the per-pass status record: `None` when no target is active or
/// latched, otherwise the message with its monotonic sequence advanced by one
/// from the previous publish (the first publish is sequence 1).
#[cfg(feature = "native-media")]
pub(crate) fn stream_status_message(
    server: &ServerIdentity,
    previous_sequence: u64,
    samples: Vec<StreamStatusSample>,
) -> AppResult<Option<StreamStatusMessage>> {
    if samples.is_empty() {
        return Ok(None);
    }
    let sequence = previous_sequence
        .checked_add(1)
        .ok_or_else(|| AppFailure("stream status sequence exhausted".into()))?;
    Ok(Some(StreamStatusMessage {
        server: server.clone(),
        sequence,
        samples,
    }))
}

#[cfg(feature = "native-media")]
pub(crate) fn native_stream_snapshots(
    output_set: &OutputSet,
    streams: &mut [NativeProgramStream],
    local_output_status: &[Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    now_ms: u64,
) -> Vec<StreamRuntimeSnapshot> {
    streams
        .iter_mut()
        .map(|stream| {
            let state = output_set.state(stream.destination_id);
            let delivering = stream.first_failure.is_none()
                && matches!(
                    state,
                    Some(DestinationState::Live | DestinationState::Congested)
                );
            let muxed_bytes = stream
                .sink
                .idle_sink()
                .and_then(ProgramStreamSink::stream_telemetry)
                .map(|telemetry| telemetry.muxed_bytes);
            let (uptime_ms, bitrate_kbps) = stream.meter.observe(now_ms, delivering, muxed_bytes);
            StreamRuntimeSnapshot {
                congested: std::mem::take(&mut stream.enqueue_backpressure),
                target: stream.target,
                ledger: stream.ledger,
                first_failure: stream.first_failure.as_ref().map(|_| {
                    stream
                        .first_failure_code
                        .unwrap_or(OutputFailure::new(OutputFailureCode::InternalError))
                }),
                started: stream.started,
                state,
                network: output_set.telemetry(stream.destination_id).cloned(),
                local: cached_local_hls_status(
                    output_set,
                    local_output_status,
                    stream.destination_id,
                ),
                uptime_ms,
                bitrate_kbps,
            }
        })
        .collect()
}

/// Builds one lossy status sample per active or latched stream runtime.
///
/// Targets that never started (and never latched a failure) are omitted, so
/// an all-inactive set yields no samples and the daemon publishes nothing.
/// Output is bounded to [`MAX_STREAM_SAMPLES`] and sorted into the strictly
/// ascending target order the wire contract requires.
#[cfg(feature = "native-media")]
pub(crate) fn stream_status_samples(
    runtimes: impl IntoIterator<Item = StreamRuntimeSnapshot>,
) -> Vec<StreamStatusSample> {
    let mut samples: Vec<_> = runtimes
        .into_iter()
        .filter_map(stream_status_sample)
        .collect();
    samples.sort_by_key(|sample| sample.target.get());
    samples.truncate(MAX_STREAM_SAMPLES);
    samples
}

/// Maps one target's runtime onto its protocol sample.
///
/// Counters come from the per-target feed ledger; `connected` and muxed bytes
/// come from the shared output set's telemetry. The first latched failure is
/// sticky and always wins over whatever the transport last reported, mirroring
/// `emit_final_record`. `local` is the cached local HLS output health the
/// snapshot already filtered to HLS targets.
#[cfg(feature = "native-media")]
pub(crate) fn stream_status_sample(runtime: StreamRuntimeSnapshot) -> Option<StreamStatusSample> {
    let StreamRuntimeSnapshot {
        target,
        ledger,
        congested,
        first_failure,
        started,
        state,
        network,
        local,
        uptime_ms,
        bitrate_kbps,
    } = runtime;
    if !started && first_failure.is_none() {
        // Configured but never started: an inactive target stays off the wire.
        return None;
    }
    let network = network.as_ref();
    let failure = first_failure.or_else(|| {
        network
            .and_then(|telemetry| telemetry.latest_failure())
            .map(output_failure::stream_failure)
    });
    Some(StreamStatusSample {
        target: WireStreamTargetId::new(target.get()),
        realized: realized_stream_state(first_failure.is_some(), congested, state),
        connected: matches!(
            state,
            Some(
                DestinationState::Live
                    | DestinationState::Congested
                    | DestinationState::AwaitingRandomAccess
            )
        ) || network.is_some_and(|telemetry| telemetry.packets_sent() > 0),
        muxed_bytes: network.map_or(0, NetworkTelemetry::bytes_sent),
        enqueued_pairs: ledger.enqueued_pairs,
        dropped_pairs: ledger.dropped_pairs,
        reconnects: network.map_or(0, NetworkTelemetry::reconnects),
        uptime_ms,
        bitrate_kbps,
        failure,
        local,
    })
}

/// Maps the shared output set's state onto [`StreamRealizedState`].
///
/// Mapping choices, deliberately:
///
/// - A latched failure wins outright: `Failed`. The set's own `Failed` — a
///   spent retry budget or a terminal cause — maps the same way.
/// - `Starting` covers both the connecting slot and a reconnected route still
///   waiting for its first random-access pair.
/// - A live route is `Live`, or `Congested` when either the transport reports
///   congestion or the feed observed enqueue backpressure this interval.
/// - `WaitingToReconnect` is now reachable wire state: every target routes
///   through the shared output set, whose bounded backoff sits between a lost
///   destination and its next attempt. A stopped slot reports `Stopped`; the
///   cleanly stopped targets then leave the active set entirely.
#[cfg(feature = "native-media")]
pub(crate) fn realized_stream_state(
    failed: bool,
    congested: bool,
    state: Option<DestinationState>,
) -> StreamRealizedState {
    if failed {
        return StreamRealizedState::Failed;
    }
    match state {
        None | Some(DestinationState::Stopped) => StreamRealizedState::Stopped,
        Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => {
            StreamRealizedState::Starting
        }
        Some(DestinationState::Failed) => StreamRealizedState::Failed,
        Some(DestinationState::WaitingToReconnect { .. }) => {
            StreamRealizedState::WaitingToReconnect
        }
        Some(DestinationState::Congested) => StreamRealizedState::Congested,
        Some(DestinationState::Live) if congested => StreamRealizedState::Congested,
        Some(DestinationState::Live) => StreamRealizedState::Live,
    }
}
