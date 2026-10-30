//! Highlight reels (OR-013).
//!
//! The show holds its reels ([`crate::ShowState::reels`]: ordered clips cut
//! from recorded replay events); everything else is engine runtime, never
//! snapshotted and empty after a restart: the compiled reels and the
//! export jobs. A reel is compiled against the captured events
//! ([`fm_replay::HighlightReel`]) on demand, so an event captured after the
//! reel was edited makes it resolvable without touching the project.
//!
//! A reel edit ([`EngineReelOperation::Set`], [`EngineReelOperation::Delete`])
//! is a project edit: it honours the padlock and is undone by restoring the
//! previous reel. Export control ([`EngineReelOperation::Export`],
//! [`EngineReelOperation::CancelExport`], and
//! [`EngineReelOperation::ForgetExport`]) is a live operation: it never
//! enters the undo history, never blocks on the padlock, and its progress is
//! reported through [`crate::Engine::reel_status`], not an event.
//!
//! An export is accepted only when the reel exists, every event it names is
//! completely recorded (a partial event is refused, not deferred), the
//! destination is an absolute path that does not exist yet, the job id is
//! unused, and no other export is active. The accepted export snapshots the
//! compiled reel and protects every segment it reads from retention with a
//! [`fm_replay::ProtectionReference::Export`] reference until it completes,
//! fails, or is cancelled. Nothing is encoded or written here: the media
//! plane asks for bounded chunks of exact recorded frames
//! ([`crate::Engine::reel_export_chunk`]) and reports the finished file's
//! size or failure back.

use std::{
    num::NonZeroU128,
    ops::Range,
    path::{Path, PathBuf},
};

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_replay::{
    CameraId, CompiledReel, EventId, ExportChunk, ExportJobId, HighlightId, HighlightReel,
    ReelClipSpec, ReelError, ReelFrameRate, ReelSpeed, ReelTransition,
};
use fm_types::{
    InputId,
    reel::{ReelMusic, ReelSpec, ReelSpecError, reel_export_path_is_valid},
    replay::ReplaySpeed,
};

use crate::{ShowState, replay::ReplayRuntime};

/// One highlight reel command (OR-013).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EngineReelOperation {
    /// Creates or replaces reel `spec.id`: a project edit, undone by
    /// restoring the previous reel (or removing a new one).
    Set(Box<ReelSpec>),
    /// Removes reel `reel`: a project edit, undone by restoring it. A reel
    /// that does not exist is accepted and changes nothing.
    Delete { reel: u64 },
    /// Exports reel `reel` to `destination` as job `job`. A live operation.
    Export {
        job: u64,
        reel: u64,
        destination: String,
    },
    /// Cancels active export `job` and releases its retention protection.
    CancelExport { job: u64 },
    /// Drops terminal export `job` so its id can be reused.
    ForgetExport { job: u64 },
}

impl EngineReelOperation {
    /// Whether this is a live operation (everything but [`Self::Set`] and
    /// [`Self::Delete`]): never undone, never padlocked.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Export { .. } | Self::CancelExport { .. } | Self::ForgetExport { .. }
        )
    }
}

/// The lifecycle of one reel export as clients see it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineReelExportState {
    /// Accepted, waiting for the recorder to close a segment its clips
    /// read (Protocol 2.123).
    Deferred,
    /// Accepted, not yet resolved.
    Queued,
    /// Frames are being resolved.
    Running,
    /// Every frame is resolved; the media plane is writing the file.
    Finalizing,
    /// The media plane reported the finished file.
    Completed,
    /// The export failed; `failure` says why.
    Failed,
    /// The export was cancelled.
    Cancelled,
}

/// One reel as clients see it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelSummary {
    pub id: u64,
    pub name: String,
    pub clips: u32,
    /// The compiled output length, once every event resolves.
    pub total_frames: Option<u64>,
    /// Why the reel does not compile, if it does not.
    pub error: Option<String>,
}

/// One reel export as clients see it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelExportStatus {
    pub job: u64,
    pub reel: u64,
    pub destination: String,
    pub state: EngineReelExportState,
    pub completed_frames: u64,
    pub total_frames: u64,
    /// The finished file's size, once the media plane reported it.
    pub bytes: Option<u64>,
    /// Why the export failed.
    pub failure: Option<String>,
    /// The reel's music bed as the export was accepted, mixed under the
    /// clips by the media plane.
    pub music: Option<ReelMusic>,
}

/// The reels and their exports as clients see them (OR-013).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelStatus {
    pub reels: Vec<EngineReelSummary>,
    pub exports: Vec<EngineReelExportStatus>,
}

/// Where one exported frame reads its recorded picture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelLayer {
    /// The recorded input (angle).
    pub input: InputId,
    /// The segment file holding the frame.
    pub path: PathBuf,
    /// The frame index inside the segment file.
    pub frame: u64,
    /// Exact mix weight `weight_num / weight_den` in `[0, 1]`.
    pub weight_num: u32,
    pub weight_den: u32,
    /// The clip plays in real time, so its recorded audio plays too.
    pub full_speed: bool,
}

/// One output frame of a reel export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelFrame {
    pub output_frame: u64,
    /// The layers composing the frame, bottom layer first.
    pub layers: Vec<EngineReelLayer>,
}

/// A bounded range of an export's exact output frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineReelChunk {
    pub job: u64,
    pub frames: Range<u64>,
    /// The chunk finished the reel.
    pub completed: bool,
    pub frames_out: Vec<EngineReelFrame>,
}

/// Why a reel plan or export control was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineReelError {
    /// The show holds no reel with this id.
    UnknownReel(u64),
    /// The export job does not exist (or was forgotten).
    UnknownExport(u64),
    /// The export is terminal, or the operation does not apply to its state.
    ExportNotActive(u64),
    /// The export has not resolved every frame yet.
    ExportNotFinalizing(u64),
    /// The export is still active and cannot be forgotten.
    ExportStillActive(u64),
    /// A reel clip names event `0`, which no captured event has.
    UnknownEvent(u32),
    /// A reel clip's speed is not 25, 50, 75, or 100 percent.
    InvalidSpeed(u8),
    /// A resolved layer names a segment or frame the catalog does not hold.
    Unresolved { job: u64, output_frame: u64 },
    /// The reel does not compile against the captured events.
    Plan(ReelError),
    /// The export planner refused a chunk.
    Export(fm_replay::RecordExportError),
}

impl std::fmt::Display for EngineReelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownReel(id) => write!(formatter, "show does not contain reel {id}"),
            Self::UnknownExport(job) => write!(formatter, "unknown reel export job {job}"),
            Self::ExportNotActive(job) => write!(formatter, "reel export job {job} is not active"),
            Self::ExportNotFinalizing(job) => write!(
                formatter,
                "reel export job {job} has not resolved every output frame"
            ),
            Self::ExportStillActive(job) => {
                write!(formatter, "reel export job {job} is still active")
            }
            Self::UnknownEvent(event) => write!(formatter, "unknown replay event {event}"),
            Self::InvalidSpeed(percent) => write!(
                formatter,
                "reel clip speed must be 25, 50, 75, or 100 percent, got {percent}"
            ),
            Self::Unresolved { job, output_frame } => write!(
                formatter,
                "reel export job {job} could not resolve output frame {output_frame}"
            ),
            Self::Plan(error) => error.fmt(formatter),
            Self::Export(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EngineReelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(error) => Some(error),
            Self::Export(error) => Some(error),
            _ => None,
        }
    }
}

/// A validated export, submitted once the command is accepted.
#[derive(Clone, Debug)]
pub(crate) struct ReelExportPlan {
    pub(crate) job: u64,
    pub(crate) reel: u64,
    pub(crate) destination: String,
    pub(crate) compiled: CompiledReel,
    /// The reel's music bed as the export was accepted.
    pub(crate) music: Option<ReelMusic>,
}

/// The runtime work one accepted reel command leaves for the engine: a
/// validated export to submit, a cancellation, or a terminal job to forget.
#[derive(Clone, Debug)]
pub(crate) enum ReelMutation {
    /// A reel edit (`Set` or `Delete`): applied to the show by
    /// [`accept`], nothing to do at runtime.
    Edit,
    /// Submit a planned export, protecting the segments it reads.
    Export(ReelExportPlan),
    /// Cancel active export `job`, releasing its protection.
    Cancel { job: u64 },
    /// Drop terminal export `job` so its id can be reused.
    Forget { job: u64 },
}

/// The nonzero [`EventId`] of replay event `event`, or `None` for `0`.
pub(crate) fn event_id(event: u32) -> Option<EventId> {
    NonZeroU128::new(u128::from(event)).map(EventId::new)
}

/// The nonzero [`ExportJobId`] of export job `job`.
pub(crate) fn export_id(job: u64) -> ExportJobId {
    ExportJobId::new(NonZeroU128::new(u128::from(job)).unwrap_or(NonZeroU128::MIN))
}

/// The numeric export job of `id`.
pub(crate) fn export_number(id: ExportJobId) -> u64 {
    u64::try_from(id.get().get()).unwrap_or(u64::MAX)
}

fn highlight_id(index: usize) -> HighlightId {
    let value = u128::try_from(index).unwrap_or(0) + 1;
    HighlightId::new(NonZeroU128::new(value).unwrap_or(NonZeroU128::MIN))
}

/// `frames` at the show's rate, in nanoseconds.
fn frames_in_nanos(rate: fm_types::FrameRate, frames: u32) -> u64 {
    let nanos = u128::from(frames) * 1_000_000_000 * u128::from(rate.denominator())
        / u128::from(rate.numerator());
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

/// Compiles `spec` against the recorded events, exactly as
/// [`fm_replay::HighlightReel::compile`] does: every clip plays the recorded
/// angle, its trims are frame-exact, clips play at the fixed replay speeds,
/// and a clip with `dissolve_frames` overlaps that many frames of the clip
/// before it in a linear dissolve (schema 84); the others cut.
pub(crate) fn compile(
    runtime: &ReplayRuntime,
    spec: &ReelSpec,
) -> Result<CompiledReel, EngineReelError> {
    let rate = runtime.frame_rate();
    let reel_rate = ReelFrameRate::new(rate.numerator(), rate.denominator()).map_err(|_| {
        EngineReelError::Plan(ReelError::InvalidFrameRate {
            numerator: rate.numerator(),
            denominator: rate.denominator(),
        })
    })?;
    let mut reel = HighlightReel::new(reel_rate);
    for (index, clip) in spec.clips.iter().enumerate() {
        let event = event_id(clip.event).ok_or(EngineReelError::UnknownEvent(clip.event))?;
        let speed = ReplaySpeed::from_percent(clip.speed_percent)
            .ok_or(EngineReelError::InvalidSpeed(clip.speed_percent))?;
        reel.push(ReelClipSpec {
            id: highlight_id(index),
            event_id: event,
            angle: CameraId::new(clip.angle.get()),
            trim_in_nanos: frames_in_nanos(rate, clip.trim_in_frames),
            trim_out_nanos: frames_in_nanos(rate, clip.trim_out_frames),
            speed: ReelSpeed::new(u32::from(speed.percent()), 100)
                .map_err(|_| EngineReelError::InvalidSpeed(clip.speed_percent))?,
            transition_in: match clip.dissolve_frames {
                0 => ReelTransition::Cut,
                frames => ReelTransition::Dissolve { frames },
            },
            program_audio: false,
        })
        .map_err(EngineReelError::Plan)?;
    }
    runtime
        .with_media(|media| reel.compile(media.events))
        .map_err(EngineReelError::Plan)
}

/// Resolves the recorded picture of every layer of `chunk` through the
/// segment catalog: the segment file and the frame index inside it.
pub(crate) fn resolve_chunk(
    runtime: &ReplayRuntime,
    chunk: ExportChunk,
) -> Result<EngineReelChunk, EngineReelError> {
    let job = export_number(chunk.job);
    let frames_out = runtime.with_media(|media| {
        chunk
            .resolved
            .iter()
            .map(|frame| {
                let layers = frame
                    .layers
                    .iter()
                    .map(|layer| {
                        let unresolved = EngineReelError::Unresolved {
                            job,
                            output_frame: frame.output_frame,
                        };
                        let (path, first_stream_frame) = media
                            .segment_origin(layer.segment_id)
                            .ok_or_else(|| unresolved.clone())?;
                        let index = layer
                            .observation
                            .source_frame
                            .get()
                            .checked_sub(first_stream_frame)
                            .ok_or(unresolved)?;
                        Ok(EngineReelLayer {
                            input: InputId::new(layer.plan.camera_id.get()),
                            path,
                            frame: index,
                            weight_num: layer.plan.weight.numerator,
                            weight_den: layer.plan.weight.denominator,
                            full_speed: layer.plan.speed.is_real_time(),
                        })
                    })
                    .collect::<Result<Vec<_>, EngineReelError>>()?;
                Ok(EngineReelFrame {
                    output_frame: frame.output_frame,
                    layers,
                })
            })
            .collect::<Result<Vec<_>, EngineReelError>>()
    })?;
    Ok(EngineReelChunk {
        job,
        frames: chunk.frames,
        completed: chunk.completed,
        frames_out,
    })
}

fn invalid(field: &'static str, code: &'static str, message: String) -> Rejection {
    Rejection::new(RejectionCode::InvalidCommand, message.clone())
        .with_field(FieldIssue::new(field, code, message))
}

fn conflict(field: &'static str, code: &'static str, message: String) -> Rejection {
    Rejection::new(RejectionCode::Conflict, message.clone())
        .with_field(FieldIssue::new(field, code, message))
}

fn not_found(field: &'static str, code: &'static str, message: String) -> Rejection {
    Rejection::new(RejectionCode::NotFound, message.clone())
        .with_field(FieldIssue::new(field, code, message))
}

/// The refusal of a reel the show cannot take.
pub(crate) fn spec_rejection(state: &ShowState, error: ReelSpecError) -> Rejection {
    let message = error.to_string();
    match error {
        ReelSpecError::TooManyReels => conflict("reels", "too_many_reels", message),
        ReelSpecError::TooManyClips => invalid("clips", "too_many_clips", message),
        ReelSpecError::EmptyReel => invalid("clips", "empty_reel", message),
        ReelSpecError::DuplicateReel(_) => invalid("id", "duplicate_reel", message),
        ReelSpecError::Name => invalid("name", "invalid_name", message),
        ReelSpecError::UnrecordedAngle(input) => {
            if state.inputs().contains(&input) {
                invalid("clips", "unrecorded_angle", message)
            } else {
                not_found("clips", "unknown_input", message)
                    .with_reason(crate::error::unknown_input_reason(input))
            }
        }
        ReelSpecError::Speed(_) => invalid("clips", "invalid_speed", message),
        ReelSpecError::Trim => invalid("clips", "trim_out_of_range", message),
        ReelSpecError::Dissolve => invalid("clips", "invalid_dissolve", message),
        ReelSpecError::Music => invalid("music", "invalid_music", message),
    }
}

/// The refusal of a reel that does not compile against the captured events.
pub(crate) fn plan_rejection(error: &EngineReelError) -> Rejection {
    let message = error.to_string();
    match error {
        EngineReelError::UnknownEvent(event) => not_found(
            "event",
            "unknown_event",
            format!("unknown replay event {event}"),
        ),
        EngineReelError::InvalidSpeed(_) => invalid("clips", "invalid_speed", message),
        EngineReelError::Plan(ReelError::UnknownEvent(id)) => not_found(
            "event",
            "unknown_event",
            format!("unknown replay event {id}"),
        ),
        EngineReelError::Plan(ReelError::AngleNotInEvent { event, camera }) => invalid(
            "clips",
            "unknown_angle",
            format!("camera {camera} is not an angle of event {event}"),
        ),
        EngineReelError::Plan(ReelError::TrimLeavesNoMedia(_)) => {
            invalid("clips", "trim_out_of_range", message)
        }
        EngineReelError::Plan(ReelError::ReelTooLong { .. }) => {
            invalid("reel", "reel_too_long", message)
        }
        EngineReelError::Plan(ReelError::TransitionTooLong { .. }) => {
            invalid("clips", "dissolve_too_long", message)
        }
        _ => invalid("reel", "invalid_reel", message),
    }
}

/// The refusal of an export that cannot start because the recorded media
/// is incomplete or unresolvable.
pub(crate) fn export_rejection(error: &fm_replay::RecordExportError) -> Rejection {
    let message = error.to_string();
    match error {
        fm_replay::RecordExportError::CoverageGap { .. } => {
            conflict("clips", "coverage_gap", message)
        }
        fm_replay::RecordExportError::SegmentNotFinalized { .. } => {
            conflict("clips", "segment_open", message)
        }
        fm_replay::RecordExportError::Unresolved { .. } => conflict("clips", "unresolved", message),
        fm_replay::RecordExportError::TooManySegments { .. } => {
            conflict("clips", "too_many_segments", message)
        }
        _ => conflict("reel", "export_refused", message),
    }
}

/// The refusal of a live export control that names an unknown or terminal
/// job.
pub(crate) fn control_rejection(error: &EngineReelError) -> Rejection {
    let message = error.to_string();
    match error {
        EngineReelError::UnknownExport(_) => not_found("job", "unknown_export", message),
        EngineReelError::ExportNotActive(_) => conflict("job", "export_not_active", message),
        EngineReelError::ExportStillActive(_) => conflict("job", "export_still_active", message),
        _ => plan_rejection(error),
    }
}

/// Accepts one reel operation against the show: a reel edit is applied;
/// export control is checked against the runtime when the command is
/// planned.
pub(crate) fn accept(
    state: &mut ShowState,
    operation: &EngineReelOperation,
) -> Result<(), Rejection> {
    match operation {
        EngineReelOperation::Set(spec) => state
            .set_reel((**spec).clone())
            .map(|_| ())
            .map_err(|error| spec_rejection(state, error)),
        EngineReelOperation::Delete { reel } => {
            state.remove_reel(*reel);
            Ok(())
        }
        EngineReelOperation::Export { .. }
        | EngineReelOperation::CancelExport { .. }
        | EngineReelOperation::ForgetExport { .. } => Ok(()),
    }
}

/// The export acceptance checks that only the show decides: the reel
/// exists. The runtime checks the rest (see [`ReplayRuntime::plan_export`]).
pub(crate) fn check_export_reel(state: &ShowState, reel: u64) -> Result<(), Rejection> {
    if state.reel(reel).is_some() {
        Ok(())
    } else {
        Err(not_found(
            "reel",
            "unknown_reel",
            format!("show does not contain reel {reel}"),
        ))
    }
}

/// The refusal of an export destination that is unusable or already exists.
pub(crate) fn check_destination(destination: &str) -> Result<(), Rejection> {
    if !reel_export_path_is_valid(destination) {
        return Err(invalid(
            "destination",
            "invalid_destination",
            format!(
                "a reel export destination must be an absolute path of at most {} bytes",
                fm_types::reel::MAX_REEL_PATH_BYTES
            ),
        ));
    }
    if Path::new(destination).exists() {
        return Err(conflict(
            "destination",
            "destination_exists",
            format!("reel export destination {destination} already exists"),
        ));
    }
    Ok(())
}

/// The refusal of an export job id that is zero or already used.
pub(crate) fn check_job(job: u64, used: bool) -> Result<(), Rejection> {
    if job == 0 {
        return Err(invalid(
            "job",
            "invalid_job",
            "a reel export job id must be nonzero".to_owned(),
        ));
    }
    if used {
        return Err(conflict(
            "job",
            "duplicate_job",
            format!("reel export job {job} already exists"),
        ));
    }
    Ok(())
}

/// The refusal of a second active export.
pub(crate) fn check_no_active_export(active: Option<u64>) -> Result<(), Rejection> {
    match active {
        Some(active) => Err(conflict(
            "job",
            "export_active",
            format!("reel export job {active} is still active"),
        )),
        None => Ok(()),
    }
}
