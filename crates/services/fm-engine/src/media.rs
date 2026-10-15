//! Media-file playback (IN-003).
//!
//! A media input's persisted playback settings live in the show
//! ([`crate::ShowState::media_settings`]); its transport is engine runtime:
//! one [`fm_playback::MediaPlayback`] per media input whose source is known,
//! advanced exactly once per engine frame. Runtime state is never
//! snapshotted: a restored engine cues every source at its start frame,
//! stopped.
//!
//! Media commands are accepted independently of the source (the input must
//! be a media input and a settings change must stay in bounds) and realized
//! on their target frame, where seeks and trims clamp to what the source
//! holds. A source is either *external* (a decoder reports its frame count,
//! growth, completion, and faults through [`crate::Engine::update_media_source`]
//! and [`crate::Engine::set_media_fault`]) or *synthetic* (the simulated
//! engine grows and completes it deterministically, one frame per tick, so
//! transport, events, and growing-file behaviour are exercised without a
//! decoder).

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{MediaAudioSpeedPolicy, MediaEndAction, MediaPlaybackSettings};
use fm_playback::{
    AudioSpeedPolicy, AudioTrack, FrameIndex, MediaControlError, MediaPlayback, MediaSourceConfig,
    MediaState, MediaTick, PlaybackRate, RationalSeconds,
};
use fm_types::{FrameRate, InputId};

use crate::ShowState;

/// Most audio tracks the engine reports for one source.
pub const MAX_MEDIA_TRACKS: usize = 32;
/// Longest reported track language tag, in UTF-8 bytes.
pub const MAX_MEDIA_TRACK_LANGUAGE_BYTES: usize = 32;
/// Longest reported track title, in UTF-8 bytes.
pub const MAX_MEDIA_TRACK_TITLE_BYTES: usize = 128;
/// Seconds of source a synthetic media input holds once complete.
pub const SYNTHETIC_MEDIA_SECONDS: u64 = 10;
/// Seconds a growing synthetic source starts with.
pub const SYNTHETIC_GROWING_START_SECONDS: u64 = 2;

/// Where a media seek lands.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngineMediaSeek {
    /// An exact source frame.
    Frame(u64),
    /// The source frame displayed at `numerator / denominator` seconds.
    Time { numerator: u64, denominator: u64 },
}

/// One media command (IN-003).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngineMediaOperation {
    Play,
    Pause,
    Stop,
    Seek(EngineMediaSeek),
    /// Declares a growing source finished.
    CompleteSource,
    Trim {
        trim_in: u64,
        trim_out: Option<u64>,
    },
    Rate {
        speed_basis_points: u32,
    },
    EndAction(MediaEndAction),
    EndTriggerLead(Option<u32>),
    AudioTrack(Option<u32>),
    AudioPolicy(MediaAudioSpeedPolicy),
}

impl EngineMediaOperation {
    /// Transport operations are live operator controls; the others edit the
    /// input's persisted settings (padlocked like any project edit).
    #[must_use]
    pub const fn is_transport(self) -> bool {
        matches!(
            self,
            Self::Play | Self::Pause | Self::Stop | Self::Seek(_) | Self::CompleteSource
        )
    }
}

/// A media command for one input.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EngineMediaCommand {
    pub input: InputId,
    pub operation: EngineMediaOperation,
}

/// Why a media input cannot play.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngineMediaFault {
    SourceMissing,
    SourceTruncated,
    SourceReplaced,
    SourceRewritten,
    SourceChanged,
    Unsupported,
    DecodeFailed,
    ToolUnavailable,
}

impl EngineMediaFault {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceTruncated => "source_truncated",
            Self::SourceReplaced => "source_replaced",
            Self::SourceRewritten => "source_rewritten",
            Self::SourceChanged => "source_changed",
            Self::Unsupported => "unsupported",
            Self::DecodeFailed => "decode_failed",
            Self::ToolUnavailable => "tool_unavailable",
        }
    }
}

/// What the engine knows about a media input's source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineMediaSource {
    pub source_rate: FrameRate,
    /// Frames currently known to exist.
    pub frames: u64,
    /// `false` while the source is still being written.
    pub complete: bool,
    pub tracks: Vec<AudioTrack>,
    /// Whether the engine itself grows and completes the source (the
    /// simulated engine); an external source is driven by its decoder.
    pub synthetic: bool,
}

impl EngineMediaSource {
    /// A decoder-backed source. Track metadata is sanitized to the reported
    /// bounds: at most [`MAX_MEDIA_TRACKS`] tracks with unique stream
    /// indexes (ascending), and bounded language and title text without
    /// control characters.
    #[must_use]
    pub fn external(
        source_rate: FrameRate,
        frames: u64,
        complete: bool,
        tracks: Vec<AudioTrack>,
    ) -> Self {
        Self {
            source_rate,
            frames,
            complete,
            tracks: sanitize_tracks(tracks),
            synthetic: false,
        }
    }

    /// The simulated source of a media input: [`SYNTHETIC_MEDIA_SECONDS`] of
    /// frames at `frame_rate` with an English (default) and a Spanish
    /// stereo track. A growing input starts with
    /// [`SYNTHETIC_GROWING_START_SECONDS`] and grows one frame per tick.
    #[must_use]
    pub fn synthetic(settings: &MediaPlaybackSettings, frame_rate: FrameRate) -> Self {
        let growing = settings.growing.is_some();
        Self {
            source_rate: frame_rate,
            frames: seconds_to_frames(
                if growing {
                    SYNTHETIC_GROWING_START_SECONDS
                } else {
                    SYNTHETIC_MEDIA_SECONDS
                },
                frame_rate,
            ),
            complete: !growing,
            tracks: vec![
                AudioTrack {
                    stream_index: 1,
                    sample_rate: 48_000,
                    channels: 2,
                    language: Some("eng".into()),
                    title: Some("Program".into()),
                    default: true,
                },
                AudioTrack {
                    stream_index: 2,
                    sample_rate: 48_000,
                    channels: 2,
                    language: Some("spa".into()),
                    title: Some("Programa".into()),
                    default: false,
                },
            ],
            synthetic: true,
        }
    }
}

fn seconds_to_frames(seconds: u64, rate: FrameRate) -> u64 {
    seconds * u64::from(rate.numerator()) / u64::from(rate.denominator())
}

fn bounded_text(text: Option<String>, maximum: usize) -> Option<String> {
    let mut clean: String = text?.chars().filter(|c| !c.is_control()).collect();
    if clean.len() > maximum {
        let mut end = maximum;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    (!clean.is_empty()).then_some(clean)
}

fn sanitize_tracks(mut tracks: Vec<AudioTrack>) -> Vec<AudioTrack> {
    tracks.sort_by_key(|track| track.stream_index);
    tracks.dedup_by_key(|track| track.stream_index);
    tracks.truncate(MAX_MEDIA_TRACKS);
    for track in &mut tracks {
        track.language = bounded_text(track.language.take(), MAX_MEDIA_TRACK_LANGUAGE_BYTES);
        track.title = bounded_text(track.title.take(), MAX_MEDIA_TRACK_TITLE_BYTES);
    }
    tracks
}

/// A media input's complete replicated state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineMediaStatus {
    pub input: InputId,
    pub settings: MediaPlaybackSettings,
    /// Whether a source is open.
    pub available: bool,
    pub state: MediaState,
    /// The source frame under the playhead.
    pub position: u64,
    /// The playhead's exact time since source frame zero.
    pub position_time: RationalSeconds,
    /// The exclusive end of the playable range.
    pub playable_end: u64,
    /// Frames known to exist.
    pub frames: u64,
    pub complete: bool,
    pub loops: u64,
    pub audio_track: Option<u32>,
    pub tracks: Vec<AudioTrack>,
    pub source_rate: Option<FrameRate>,
    pub fault: Option<EngineMediaFault>,
}

/// One media input on one engine frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineMediaFrame {
    pub input: InputId,
    /// The transport's decisions for this frame; `None` while no source is
    /// open.
    pub tick: Option<MediaTick>,
    /// The media commands realized on this frame, in acceptance order.
    pub operations: Vec<EngineMediaOperation>,
    /// The fault reported on this frame, once.
    pub faulted: Option<EngineMediaFault>,
    /// A [`EngineMediaOperation::CompleteSource`] was realized for an
    /// external source: its decoder should finish the file and report the
    /// final frame count.
    pub completion_requested: bool,
}

/// A media input's runtime transport. Never snapshotted.
#[derive(Clone, Debug, Default)]
pub(crate) struct MediaRuntime {
    source: Option<EngineMediaSource>,
    playback: Option<MediaPlayback>,
    fault: Option<EngineMediaFault>,
    fault_reported: bool,
    /// Accepted operations not yet realized, in acceptance order.
    pending: Vec<EngineMediaOperation>,
    /// Ticks a complete-length synthetic growing source has not grown.
    idle_ticks: u32,
    /// The input source the open source was opened for (IN-026): a
    /// replaced source releases it.
    pub(crate) opened_for: Option<fm_model::InputSourceSpec>,
}

impl MediaRuntime {
    /// Opens `source` (or closes the source) with `settings`, cued at the
    /// start frame and stopped.
    pub(crate) fn open(
        &mut self,
        source: Option<EngineMediaSource>,
        settings: &MediaPlaybackSettings,
        output_rate: FrameRate,
    ) -> Result<(), MediaControlError> {
        self.playback = match &source {
            Some(source) => Some(create_playback(source, settings, output_rate)?),
            None => None,
        };
        self.source = source;
        self.fault = None;
        self.fault_reported = false;
        self.idle_ticks = 0;
        Ok(())
    }

    pub(crate) fn fault(&mut self, fault: EngineMediaFault) {
        self.source = None;
        self.playback = None;
        self.fault = Some(fault);
        self.fault_reported = false;
    }

    /// Reports an external source's growth or completion.
    pub(crate) fn update_source(&mut self, frames: u64, complete: bool) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        match playback.update_source(frames, complete) {
            Ok(()) => {
                if let Some(source) = &mut self.source {
                    source.frames = frames;
                    source.complete = complete;
                }
            }
            Err(MediaControlError::SourceTruncated { .. }) => {
                self.fault(EngineMediaFault::SourceTruncated);
            }
            Err(_) => self.fault(EngineMediaFault::SourceChanged),
        }
    }

    pub(crate) fn accept(&mut self, operation: EngineMediaOperation) {
        self.pending.push(operation);
    }

    /// Releases the source of a closed input (IN-026): the transport is
    /// gone until the media plane opens the source again.
    pub(crate) fn release(&mut self) {
        self.source = None;
        self.playback = None;
        self.idle_ticks = 0;
        self.opened_for = None;
    }

    /// Whether a source is open.
    pub(crate) const fn is_open(&self) -> bool {
        self.source.is_some()
    }

    /// Whether the transport changes on the next tick absent commands. A
    /// held (paused or closed) transport changes only by commands and
    /// faults.
    pub(crate) fn advancing(&self, held: bool) -> bool {
        !self.pending.is_empty()
            || (self.fault.is_some() && !self.fault_reported)
            || !held
                && self.playback.as_ref().is_some_and(|playback| {
                    matches!(playback.state(), MediaState::Playing | MediaState::Waiting)
                        || self
                            .source
                            .as_ref()
                            .is_some_and(|source| source.synthetic && !source.complete)
                })
    }

    /// Realizes the oldest pending operation; returns whether it asked an
    /// external source to complete.
    fn realize_next(&mut self) -> (EngineMediaOperation, bool) {
        let operation = self.pending.remove(0);
        let mut completion_requested = false;
        if let Some(playback) = &mut self.playback {
            if operation == EngineMediaOperation::CompleteSource {
                match &mut self.source {
                    Some(source) if source.synthetic => {
                        let frames = playback.available_frames();
                        if playback.update_source(frames, true).is_ok() {
                            source.complete = true;
                        }
                    }
                    Some(source) => completion_requested = !source.complete,
                    None => {}
                }
            } else {
                realize(playback, operation);
            }
        }
        (operation, completion_requested)
    }

    /// Realizes `operations` (the actions scheduled on this frame) and
    /// advances one frame, unless `held` (a paused or closed input, IN-026):
    /// then the transport and a synthetic source stand still.
    pub(crate) fn tick(
        &mut self,
        input: InputId,
        operations: usize,
        settings: &MediaPlaybackSettings,
        held: bool,
    ) -> Option<EngineMediaFrame> {
        let mut realized = Vec::with_capacity(operations);
        let mut completion_requested = false;
        for _ in 0..operations.min(self.pending.len()) {
            let (operation, requested) = self.realize_next();
            realized.push(operation);
            completion_requested |= requested;
        }
        let tick = if held {
            None
        } else {
            self.grow_synthetic(settings);
            self.playback.as_mut().map(MediaPlayback::tick)
        };
        let faulted = match self.fault {
            Some(fault) if !self.fault_reported => {
                self.fault_reported = true;
                Some(fault)
            }
            _ => None,
        };
        if tick.is_none() && realized.is_empty() && faulted.is_none() {
            return None;
        }
        Some(EngineMediaFrame {
            input,
            tick,
            operations: realized,
            faulted,
            completion_requested,
        })
    }

    fn grow_synthetic(&mut self, settings: &MediaPlaybackSettings) {
        let (Some(source), Some(playback)) = (&mut self.source, &mut self.playback) else {
            return;
        };
        if !source.synthetic || source.complete {
            return;
        }
        let total = seconds_to_frames(SYNTHETIC_MEDIA_SECONDS, source.source_rate);
        if source.frames < total {
            source.frames += 1;
            let _ = playback.update_source(source.frames, false);
            return;
        }
        let idle = settings
            .growing
            .map_or(0, |growing| growing.idle_completion_refreshes);
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        if idle > 0
            && self.idle_ticks >= idle
            && playback.update_source(source.frames, true).is_ok()
        {
            source.complete = true;
        }
    }

    /// The status after every accepted operation is realized.
    pub(crate) fn status(
        &self,
        input: InputId,
        settings: &MediaPlaybackSettings,
    ) -> EngineMediaStatus {
        let mut projected = self.clone();
        while !projected.pending.is_empty() {
            projected.realize_next();
        }
        projected.current_status(input, settings)
    }

    fn current_status(
        &self,
        input: InputId,
        settings: &MediaPlaybackSettings,
    ) -> EngineMediaStatus {
        let zero = RationalSeconds::new(0, 1).expect("one is a nonzero denominator");
        match (&self.playback, &self.source) {
            (Some(playback), Some(source)) => EngineMediaStatus {
                input,
                settings: *settings,
                available: true,
                state: playback.state(),
                position: playback.position().get(),
                position_time: playback.position_time(),
                playable_end: playback.playable_end().get(),
                frames: playback.available_frames(),
                complete: playback.is_complete(),
                loops: playback.loop_count(),
                audio_track: playback.selected_audio(),
                tracks: source.tracks.clone(),
                source_rate: Some(source.source_rate),
                fault: None,
            },
            _ => EngineMediaStatus {
                input,
                settings: *settings,
                available: false,
                state: MediaState::Stopped,
                position: 0,
                position_time: zero,
                playable_end: 0,
                frames: 0,
                complete: false,
                loops: 0,
                audio_track: None,
                tracks: Vec::new(),
                source_rate: None,
                fault: self.fault,
            },
        }
    }

    /// The stream indexes of the open source's tracks, or `None` while no
    /// source is open.
    pub(crate) fn track_indexes(&self) -> Option<Vec<u32>> {
        self.source.as_ref().map(|source| {
            source
                .tracks
                .iter()
                .map(|track| track.stream_index)
                .collect()
        })
    }
}

const fn end_action(action: MediaEndAction) -> fm_playback::MediaEndAction {
    match action {
        MediaEndAction::Stop => fm_playback::MediaEndAction::Stop,
        MediaEndAction::Hold => fm_playback::MediaEndAction::HoldLastFrame,
        MediaEndAction::Loop => fm_playback::MediaEndAction::Loop,
    }
}

const fn audio_policy(policy: MediaAudioSpeedPolicy) -> AudioSpeedPolicy {
    match policy {
        MediaAudioSpeedPolicy::Mute => AudioSpeedPolicy::Mute,
        MediaAudioSpeedPolicy::Resample => AudioSpeedPolicy::Resample,
    }
}

/// The track a source plays: the requested stream when the source has it,
/// otherwise its default track, otherwise its first.
fn chosen_track(tracks: &[AudioTrack], requested: Option<u32>) -> Option<u32> {
    requested
        .filter(|index| tracks.iter().any(|track| track.stream_index == *index))
        .or_else(|| {
            tracks
                .iter()
                .find(|track| track.default)
                .or_else(|| tracks.first())
                .map(|track| track.stream_index)
        })
}

fn create_playback(
    source: &EngineMediaSource,
    settings: &MediaPlaybackSettings,
    output_rate: FrameRate,
) -> Result<MediaPlayback, MediaControlError> {
    let mut playback = MediaPlayback::new(MediaSourceConfig {
        source_rate: source.source_rate,
        output_rate,
        available_frames: source.frames,
        complete: source.complete,
        audio_tracks: source.tracks.clone(),
        selected_audio: chosen_track(&source.tracks, settings.audio_stream),
    })?;
    playback.set_rate(PlaybackRate::from_basis_points(
        settings.speed_basis_points,
    )?)?;
    playback.set_end_action(end_action(settings.end_action));
    playback.set_end_trigger_lead(settings.end_trigger_lead.map(u64::from));
    playback.set_audio_policy(audio_policy(settings.audio_policy));
    playback.set_live_edge_offset(
        settings
            .growing
            .map_or(0, |growing| u64::from(growing.live_edge_offset_frames)),
    );
    // Cue the start frame without a seek event: trimming in at the start
    // frame moves the playhead there, and the real trim keeps it.
    let (trim_in, trim_out) = clamp_trim(&playback, settings.trim_in, settings.trim_out);
    let start = settings.start_frame.clamp(
        trim_in,
        trim_out.map_or(u64::MAX, |out| out.saturating_sub(1)),
    );
    let _ = playback.set_trim(
        FrameIndex::new(start),
        trim_out.filter(|out| *out > start).map(FrameIndex::new),
    );
    let _ = playback.set_trim(FrameIndex::new(trim_in), trim_out.map(FrameIndex::new));
    Ok(playback)
}

/// The trim a source can hold: a complete source clamps both points to its
/// frames.
fn clamp_trim(playback: &MediaPlayback, trim_in: u64, trim_out: Option<u64>) -> (u64, Option<u64>) {
    if !playback.is_complete() {
        return (trim_in, trim_out);
    }
    let frames = playback.available_frames();
    if frames == 0 {
        return (0, None);
    }
    let trim_in = trim_in.min(frames - 1);
    let trim_out = trim_out
        .map(|out| out.min(frames))
        .filter(|out| *out > trim_in && *out < frames);
    (trim_in, trim_out)
}

fn realize(playback: &mut MediaPlayback, operation: EngineMediaOperation) {
    match operation {
        EngineMediaOperation::Play => playback.play(),
        EngineMediaOperation::Pause => playback.pause(),
        EngineMediaOperation::Stop => playback.stop(),
        EngineMediaOperation::Seek(target) => {
            let frame = match target {
                EngineMediaSeek::Frame(frame) => frame,
                EngineMediaSeek::Time {
                    numerator,
                    denominator,
                } => {
                    let rate = playback.source_rate();
                    let frame = u128::from(numerator) * u128::from(rate.numerator())
                        / (u128::from(denominator.max(1)) * u128::from(rate.denominator()));
                    u64::try_from(frame).unwrap_or(u64::MAX)
                }
            };
            let start = playback.trim().0.get();
            let end = playback.playable_end().get();
            if end > start {
                let _ = playback.seek(FrameIndex::new(frame.clamp(start, end - 1)));
            }
        }
        EngineMediaOperation::CompleteSource => {}
        EngineMediaOperation::Trim { trim_in, trim_out } => {
            let (trim_in, trim_out) = clamp_trim(playback, trim_in, trim_out);
            let _ = playback.set_trim(FrameIndex::new(trim_in), trim_out.map(FrameIndex::new));
        }
        EngineMediaOperation::Rate { speed_basis_points } => {
            if let Ok(rate) = PlaybackRate::from_basis_points(speed_basis_points) {
                let _ = playback.set_rate(rate);
            }
        }
        EngineMediaOperation::EndAction(action) => playback.set_end_action(end_action(action)),
        EngineMediaOperation::EndTriggerLead(lead) => {
            playback.set_end_trigger_lead(lead.map(u64::from));
        }
        EngineMediaOperation::AudioTrack(track) => {
            let _ = playback.select_audio_track(chosen_track(playback.audio_tracks(), track));
        }
        EngineMediaOperation::AudioPolicy(policy) => {
            playback.set_audio_policy(audio_policy(policy));
        }
    }
}

fn invalid(field: &str, message: String) -> Rejection {
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        field,
        "out_of_range",
        message,
    ))
}

/// Validates `command` against `state` and returns the input's settings
/// afterwards. `tracks` lists the open source's stream indexes (`None`
/// while no source is open): an audio track outside it is refused.
pub(crate) fn accept(
    state: &ShowState,
    command: &EngineMediaCommand,
    tracks: Option<&[u32]>,
) -> Result<MediaPlaybackSettings, Rejection> {
    let input = command.input;
    let Some(settings) = state.media_settings(input) else {
        let message = if state.inputs().contains(&input) {
            format!("input {input} is not a media input")
        } else {
            format!("input {input} is not part of this show")
        };
        return Err(
            Rejection::new(RejectionCode::InvalidCommand, message.clone())
                .with_field(FieldIssue::new("input", "invalid", message)),
        );
    };
    let mut next = *settings;
    match command.operation {
        EngineMediaOperation::Seek(EngineMediaSeek::Time { denominator: 0, .. }) => {
            return Err(invalid(
                "time",
                "a seek time needs a nonzero denominator".into(),
            ));
        }
        EngineMediaOperation::Play
        | EngineMediaOperation::Pause
        | EngineMediaOperation::Stop
        | EngineMediaOperation::CompleteSource
        | EngineMediaOperation::Seek(_) => {}
        EngineMediaOperation::Trim { trim_in, trim_out } => {
            next = next
                .with_trim(trim_in, trim_out)
                .map_err(|error| invalid("trim_out", error.to_string()))?;
        }
        EngineMediaOperation::Rate { speed_basis_points } => {
            next.speed_basis_points = speed_basis_points;
        }
        EngineMediaOperation::EndAction(action) => next.end_action = action,
        EngineMediaOperation::EndTriggerLead(lead) => next.end_trigger_lead = lead,
        EngineMediaOperation::AudioTrack(track) => {
            if let (Some(index), Some(tracks)) = (track, tracks)
                && !tracks.contains(&index)
            {
                return Err(invalid(
                    "audio_track",
                    format!("input {input} has no audio stream {index}"),
                ));
            }
            next.audio_stream = track;
        }
        EngineMediaOperation::AudioPolicy(policy) => next.audio_policy = policy,
    }
    next.validate()
        .map_err(|error| invalid(error.field(), error.to_string()))?;
    Ok(next)
}
