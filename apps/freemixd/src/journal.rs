//! Where an accepted command becomes durable.
//!
//! The daemon appends every accepted command to the bundle's mutation journal
//! and waits for that append to be durable *before* the acceptance reaches the
//! client and before any runtime realization is published. The manifest is no
//! longer rewritten per command; it is rewritten at a checkpoint, which folds
//! the recorded mutations into it and compacts the journal.
//!
//! Frames a native daemon realizes between commands move no revision, so no
//! mutation records them. A progress marker does (see [`ProgressCadence`]):
//! a single overwritten journal row holding the position the show reached
//! above the newest mutation, written at a bounded cadence, which replay
//! ticks the engine idle up to after the last recorded command.

use std::cell::Cell;

use fm_persistence::{JournalWriter, MutationBatch, ProjectPosition, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, ProtocolVersion, WireMessage, decode_line,
    encode_line,
};
use fm_types::FrameRate;

use crate::{AppFailure, AppResult};

/// Mutations allowed between two manifest checkpoints.
///
/// A count, not an interval or an idle frame: the count is the quantity the
/// journal itself bounds (`MAX_UNAPPLIED_JOURNAL_BATCHES`,
/// `MAX_UNAPPLIED_JOURNAL_BYTES`) and the quantity crash recovery pays for, so
/// bounding it here is the same bound stated once at the point that controls
/// it. It also needs no timer thread and reads no wall clock, so a checkpoint
/// lands at the same place in a show as in a test. Sixty-four commands is far
/// below both journal limits — each command is at most one 64 KiB protocol
/// line — while amortising the manifest rewrite 64:1 during a show.
const MAX_MUTATIONS_PER_CHECKPOINT: u64 = 64;

/// Bytes of protocol version that open a payload.
const PROTOCOL_BYTES: usize = 4;
/// Bytes of submission time that follow it.
const SUBMITTED_AT_BYTES: usize = 8;
/// Bytes of the position the command left the project at.
const POSITION_BYTES: usize = 6 * 8;
/// Bytes of record header that precede the encoded command in a payload.
const HEADER_BYTES: usize = PROTOCOL_BYTES + SUBMITTED_AT_BYTES + POSITION_BYTES;

/// Makes an accepted command durable.
pub(crate) trait DurableStore {
    /// Records the mutation from `previous` to `updated`.
    ///
    /// Returns only once that mutation will survive loss of power. Everything
    /// that can fail happens before that point, so a caller that sees an error
    /// must refuse the command rather than acknowledge it.
    fn record(
        &self,
        command: &CommandMessage,
        now_millis: u64,
        previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()>;

    /// Folds every recorded mutation into the manifest and compacts the
    /// journal. `project` must hold the revision of the newest recorded
    /// mutation, which is what the journal checkpoint is validated against.
    fn checkpoint(&self, project: &StoredProject) -> AppResult<()>;

    /// Durably records that the show reached `position` without a mutation:
    /// idle frames realized above the newest recorded mutation. `position`
    /// must hold that mutation's revision. Stores without a journal record
    /// nothing.
    fn record_progress(&self, _position: ProjectPosition) -> AppResult<()> {
        Ok(())
    }
}

/// The live journal of a served project.
///
/// The journal database is opened once, at startup, and held for the daemon's
/// run: it is a single-writer resource this process owns, and reopening it per
/// command made an accepted command cost what the whole show had cost so far.
pub(crate) struct DurableJournal<'store> {
    writer: JournalWriter<'store>,
    pending: Cell<u64>,
}

impl<'store> DurableJournal<'store> {
    pub(crate) const fn new(writer: JournalWriter<'store>) -> Self {
        Self {
            writer,
            pending: Cell::new(0),
        }
    }

    /// Folds anything recorded since the last checkpoint into the manifest.
    ///
    /// Nothing recorded means the manifest already holds `project`, so this
    /// does not rewrite it.
    pub(crate) fn settle(&self, project: &StoredProject) -> AppResult<()> {
        if self.pending.get() == 0 {
            return Ok(());
        }
        self.checkpoint(project)
    }
}

impl DurableStore for DurableJournal<'_> {
    fn record(
        &self,
        command: &CommandMessage,
        now_millis: u64,
        previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()> {
        let Some(revision) = single_revision_step(previous, updated) else {
            // The command advanced no revision, so there is no mutation batch
            // to append: a refused command records only its receipt, and a
            // journal batch must move the revision by exactly one. The manifest
            // is the only place that receipt can become durable — but rewriting
            // the whole manifest for it made a refusal the most expensive
            // command the daemon serves, and a client retrying with a stale
            // `--expect` after a reconnect is the ordinary case, not the rare
            // one. The receipt rides the same checkpoint the appends do; a
            // crash before that checkpoint loses it, and re-evaluating a
            // command the daemon refused answers it the same way again.
            self.pending.set(self.pending.get().saturating_add(1));
            if self.pending.get() >= MAX_MUTATIONS_PER_CHECKPOINT {
                return self.checkpoint(updated);
            }
            return Ok(());
        };
        // Checkpoint *before* the append, never after. Everything that can fail
        // then fails while the command is still refusable; a checkpoint after a
        // durable append would have to choose between reporting its failure and
        // telling the truth about a command that already survives a crash.
        if self.pending.get() >= MAX_MUTATIONS_PER_CHECKPOINT {
            self.checkpoint(previous)?;
        }
        let sequence = self
            .writer
            .head_sequence()
            .checked_add(1)
            .ok_or_else(|| AppFailure("journal sequence overflow".into()))?;
        self.writer.append_batch(&MutationBatch::new(
            sequence,
            previous.position().revision,
            revision,
            encode_mutation(command, now_millis, updated.position())?,
        ))?;
        self.pending.set(self.pending.get().saturating_add(1));
        Ok(())
    }

    fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
        // `checkpoint_and_compact` saves the manifest, then advances the
        // checkpoint, discards the applied batches and truncates the
        // write-ahead log; it refuses a manifest whose revision is not the one
        // it is checkpointing.
        self.writer
            .checkpoint_and_compact(project, self.writer.head_sequence())?;
        self.pending.set(0);
        Ok(())
    }

    fn record_progress(&self, position: ProjectPosition) -> AppResult<()> {
        self.writer.record_progress(&encode_progress(position))?;
        Ok(())
    }
}

/// Replay reruns commands that are already durable, so it records nothing.
pub(crate) struct ReplayedMutations;

impl DurableStore for ReplayedMutations {
    fn record(
        &self,
        _command: &CommandMessage,
        _now_millis: u64,
        _previous: &StoredProject,
        _updated: &StoredProject,
    ) -> AppResult<()> {
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }
}

/// The revision a single journal batch would carry, if this is one.
///
/// The journal defines a batch as exactly one revision step. A command that
/// moved the revision by anything else — a refusal, which moves it by none —
/// has no batch to append.
fn single_revision_step(previous: &StoredProject, updated: &StoredProject) -> Option<u64> {
    let next = previous.position().revision.checked_add(1)?;
    (updated.position().revision == next).then_some(next)
}

/// One recorded mutation, exactly as the daemon that accepted it wrote it.
#[derive(Debug)]
pub(crate) struct RecordedMutation {
    pub(crate) command: CommandMessage,
    pub(crate) submitted_at_millis: u64,
    /// The position the command left the project at. Replay must reproduce
    /// this, all of it, or it is not the history the operator was told about.
    pub(crate) position: ProjectPosition,
}

/// Encodes an accepted command, the instant it was submitted, the position it
/// produced, and the protocol version that built the payload.
///
/// The submission time is part of the mutation, not context: it decides
/// deadline and receipt outcomes, so replaying the command without it would not
/// reproduce the state the operator was told about. The position is recorded
/// for the same reason and covers what the revision alone cannot — the frame
/// cursor and the clock advance between commands, which native realization
/// moves on wall time. The protocol version is recorded because the payload is
/// the command re-encoded by the build that accepted it, and under this
/// project's no-compatibility rule a wire change between a crash and the
/// restart makes every unapplied batch unreadable; saying so beats decoding
/// something else.
fn encode_mutation(
    command: &CommandMessage,
    now_millis: u64,
    position: ProjectPosition,
) -> AppResult<Vec<u8>> {
    let line = encode_line(&WireMessage::Command(command.clone()))?;
    let mut payload = Vec::with_capacity(HEADER_BYTES + line.len());
    payload.extend_from_slice(&CURRENT_PROTOCOL_VERSION.major.to_le_bytes());
    payload.extend_from_slice(&CURRENT_PROTOCOL_VERSION.minor.to_le_bytes());
    payload.extend_from_slice(&now_millis.to_le_bytes());
    for field in position_fields(position) {
        payload.extend_from_slice(&field.to_le_bytes());
    }
    payload.extend_from_slice(line.as_bytes());
    Ok(payload)
}

/// Decodes what [`encode_mutation`] wrote.
///
/// # Errors
///
/// Returns an error for a payload written by another protocol version or one
/// this daemon did not write, which is damage rather than history and is never
/// applied.
pub(crate) fn decode_mutation(payload: &[u8]) -> AppResult<RecordedMutation> {
    let (header, line) = payload.split_at_checked(HEADER_BYTES).ok_or_else(|| {
        AppFailure(format!(
            "journal mutation is {} bytes, shorter than its {HEADER_BYTES}-byte header",
            payload.len()
        ))
    })?;
    let protocol = ProtocolVersion::new(
        u16::from_le_bytes([header[0], header[1]]),
        u16::from_le_bytes([header[2], header[3]]),
    );
    if protocol != CURRENT_PROTOCOL_VERSION {
        return Err(AppFailure(format!(
            "journal mutation was recorded by protocol version {protocol} and this build speaks \
             {CURRENT_PROTOCOL_VERSION}: the recorded command cannot be replayed"
        ))
        .into());
    }
    let mut fields = header[PROTOCOL_BYTES..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk));
    let mut next = || fields.next().expect("the header holds seven values");
    let submitted_at_millis = next();
    let position = ProjectPosition {
        revision: next(),
        state_epoch: next(),
        event_sequence: next(),
        frames_rendered: next(),
        runtime_generation: next(),
        clock_time_nanos: next(),
    };
    let line = std::str::from_utf8(line)
        .map_err(|error| AppFailure(format!("journal mutation is not valid UTF-8: {error}")))?;
    match decode_line(line)? {
        WireMessage::Command(command) => Ok(RecordedMutation {
            command,
            submitted_at_millis,
            position,
        }),
        _ => Err(AppFailure("journal mutation does not hold a command".into()).into()),
    }
}

/// Encodes a progress marker: the position the show reached, nothing else.
///
/// Unlike a mutation, a marker carries no command, so nothing in it depends
/// on the wire protocol.
fn encode_progress(position: ProjectPosition) -> Vec<u8> {
    position_fields(position)
        .iter()
        .flat_map(|field| field.to_le_bytes())
        .collect()
}

/// Decodes what [`encode_progress`] wrote.
///
/// # Errors
///
/// Returns an error for a payload of any other length, which this daemon did
/// not write.
pub(crate) fn decode_progress(payload: &[u8]) -> AppResult<ProjectPosition> {
    let (fields, []) = payload.as_chunks::<8>() else {
        return Err(progress_length_error(payload));
    };
    let [
        revision,
        state_epoch,
        event_sequence,
        frames_rendered,
        runtime_generation,
        clock_time_nanos,
    ] = fields
    else {
        return Err(progress_length_error(payload));
    };
    Ok(ProjectPosition {
        revision: u64::from_le_bytes(*revision),
        state_epoch: u64::from_le_bytes(*state_epoch),
        event_sequence: u64::from_le_bytes(*event_sequence),
        frames_rendered: u64::from_le_bytes(*frames_rendered),
        runtime_generation: u64::from_le_bytes(*runtime_generation),
        clock_time_nanos: u64::from_le_bytes(*clock_time_nanos),
    })
}

fn progress_length_error(payload: &[u8]) -> Box<dyn std::error::Error> {
    AppFailure(format!(
        "journal progress marker is {} bytes, not the {POSITION_BYTES}-byte position this \
         daemon writes",
        payload.len()
    ))
    .into()
}

/// Seconds of show between two progress markers.
///
/// This is how much idle progress a SIGKILL can lose: a restarted daemon
/// resumes at most this far behind the last frame it realized (a playlist
/// entry on air runs at most this much longer). Each marker is one small
/// journal commit — the same cost as one accepted command — so a one-second
/// cadence bounds the journal's commit rate at one per second while idle.
pub(crate) const PROGRESS_MARKER_SECONDS: u64 = 1;

/// Seconds of show between two folds of idle progress into the manifest.
///
/// Replay ticks every idle frame since the manifest's position, so without
/// a fold the recovery cost of an idle show grows with the show. A fold is a
/// manifest checkpoint — far dearer than a marker — so it is rare: five
/// minutes keeps a recovery's idle replay to at most that much show (well
/// under a second of ticking) while costing one manifest write per five
/// minutes.
pub(crate) const PROGRESS_FOLD_SECONDS: u64 = 300;

/// Show time journal replay may tick idle before it refuses the journal.
///
/// Idle ticks are bounded by recorded frame numbers, so a damaged journal
/// naming an absurd frame would otherwise keep startup ticking for hours or
/// forever. With markers every [`PROGRESS_MARKER_SECONDS`] and folds every
/// [`PROGRESS_FOLD_SECONDS`], an intact journal asks for minutes; a whole
/// day is far beyond any show that checkpointed as designed and still
/// replays in seconds.
pub(crate) const MAX_REPLAY_IDLE_SECONDS: u64 = 24 * 60 * 60;

/// Whole frames in `seconds` of show at `rate`, rounded down, at least one.
pub(crate) fn frames_in(seconds: u64, rate: FrameRate) -> u64 {
    let frames =
        u128::from(seconds) * u128::from(rate.numerator()) / u128::from(rate.denominator());
    u64::try_from(frames).unwrap_or(u64::MAX).max(1)
}

/// What the progress cadence asks for at a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgressDue {
    /// Nothing to record.
    Nothing,
    /// Write a progress marker.
    Marker,
    /// Fold idle progress into the manifest (falls back to a marker while
    /// the engine is not at an idle boundary).
    Fold,
}

/// Decides when the native session loop records idle progress.
///
/// Counted in realized frames, not wall time, so it needs no timer and
/// lands at the same frames in a show as in a test. Nothing is recorded
/// while the show has not moved past the newest durable position: a command
/// that just executed already recorded where it ended.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProgressCadence {
    marker_frames: u64,
    fold_frames: u64,
    marked: u64,
    folded: u64,
}

impl ProgressCadence {
    /// A cadence for a show at `rate` whose durable position is at `frame`.
    pub(crate) fn new(rate: FrameRate, frame: u64) -> Self {
        Self::with_intervals(
            frames_in(PROGRESS_MARKER_SECONDS, rate),
            frames_in(PROGRESS_FOLD_SECONDS, rate),
            frame,
        )
    }

    const fn with_intervals(marker_frames: u64, fold_frames: u64, frame: u64) -> Self {
        Self {
            marker_frames,
            fold_frames,
            marked: frame,
            folded: frame,
        }
    }

    /// What to record with the engine cursor at `cursor` and the newest
    /// durable position at `durable_frame`.
    pub(crate) const fn due(&self, cursor: u64, durable_frame: u64) -> ProgressDue {
        if cursor <= durable_frame {
            ProgressDue::Nothing
        } else if cursor.saturating_sub(self.folded) >= self.fold_frames {
            ProgressDue::Fold
        } else if cursor.saturating_sub(self.marked) >= self.marker_frames {
            ProgressDue::Marker
        } else {
            ProgressDue::Nothing
        }
    }

    /// A marker was written (or attempted) at `cursor`. A failed attempt
    /// also waits a whole interval, so a failing disk is retried at the
    /// marker cadence rather than on every frame.
    pub(crate) const fn marked(&mut self, cursor: u64) {
        self.marked = cursor;
    }

    /// Progress was folded into the manifest (or the fold failed) at
    /// `cursor`; a fold also stands for a marker.
    pub(crate) const fn folded(&mut self, cursor: u64) {
        self.folded = cursor;
        self.marked = cursor;
    }
}

/// The recorded position, in the order [`decode_mutation`] reads it back.
const fn position_fields(position: ProjectPosition) -> [u64; 6] {
    [
        position.revision,
        position.state_epoch,
        position.event_sequence,
        position.frames_rendered,
        position.runtime_generation,
        position.clock_time_nanos,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use fm_protocol::CommandPayload;

    fn test_command() -> CommandMessage {
        CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: "round-trip".into(),
            idempotency_key: "round-trip-key".into(),
            expected_revision: Some(7),
            deadline_ms: Some(1_500),
            payload: CommandPayload::Fade { duration_frames: 4 },
        }
    }

    fn test_position() -> ProjectPosition {
        ProjectPosition {
            revision: 8,
            state_epoch: 1,
            event_sequence: 21,
            frames_rendered: 604,
            runtime_generation: 3,
            clock_time_nanos: 10_066_666_666,
        }
    }

    /// The recorded mutation must come back exactly: the submission time,
    /// because replay that guessed it would not reproduce deadline and receipt
    /// outcomes, and the whole position, because the revision alone cannot see
    /// a frame cursor that moved between commands.
    #[test]
    fn a_recorded_mutation_round_trips_with_its_submission_time_and_position() {
        let payload = encode_mutation(&test_command(), 1_700_000_000_123, test_position()).unwrap();

        let decoded = decode_mutation(&payload).unwrap();
        assert_eq!(decoded.command, test_command());
        assert_eq!(decoded.submitted_at_millis, 1_700_000_000_123);
        assert_eq!(decoded.position, test_position());
        assert!(decode_mutation(&payload[..4]).is_err());
        assert!(decode_mutation(&payload[..HEADER_BYTES]).is_err());
    }

    /// A progress marker carries the whole position and nothing else; a
    /// payload of any other length is refused.
    #[test]
    fn a_progress_marker_round_trips_its_position() {
        let payload = encode_progress(test_position());
        assert_eq!(payload.len(), POSITION_BYTES);
        assert_eq!(decode_progress(&payload).unwrap(), test_position());
        assert!(decode_progress(&payload[..POSITION_BYTES - 8]).is_err());
        assert!(decode_progress(&[payload.clone(), vec![0; 8]].concat()).is_err());
        assert!(decode_progress(&payload[..POSITION_BYTES - 1]).is_err());
    }

    /// Markers come one interval apart in realized frames, a fold every fold
    /// interval stands for a marker, and nothing is recorded while the show
    /// has not passed the newest durable position.
    #[test]
    fn the_progress_cadence_marks_and_folds_on_frame_intervals() {
        let mut cadence = ProgressCadence::with_intervals(25, 100, 10);
        assert_eq!(cadence.due(10, 10), ProgressDue::Nothing);
        assert_eq!(cadence.due(34, 10), ProgressDue::Nothing);
        assert_eq!(cadence.due(35, 10), ProgressDue::Marker);
        // A command that ended at or after the cursor already recorded it.
        assert_eq!(cadence.due(35, 35), ProgressDue::Nothing);
        cadence.marked(35);
        assert_eq!(cadence.due(59, 10), ProgressDue::Nothing);
        assert_eq!(cadence.due(60, 10), ProgressDue::Marker);
        cadence.marked(60);
        assert_eq!(cadence.due(110, 10), ProgressDue::Fold);
        cadence.folded(110);
        assert_eq!(cadence.due(134, 110), ProgressDue::Nothing);
        assert_eq!(cadence.due(135, 110), ProgressDue::Marker);
        assert_eq!(cadence.due(210, 110), ProgressDue::Fold);

        let rate = FrameRate::new(30_000, 1_001).unwrap();
        let cadence = ProgressCadence::new(rate, 0);
        assert_eq!(cadence.due(29, 0), ProgressDue::Marker);
        assert_eq!(cadence.due(28, 0), ProgressDue::Nothing);
        assert_eq!(frames_in(PROGRESS_FOLD_SECONDS, rate), 8_991);
        assert_eq!(frames_in(0, rate), 1);
    }

    /// The payload is the command re-encoded by the build that accepted it, so
    /// a wire change between a crash and the restart makes it a different
    /// message. Recovery must say so and name both versions rather than decode
    /// whatever the new codec makes of the old bytes.
    #[test]
    fn a_mutation_from_another_protocol_version_is_refused_by_version() {
        let mut payload =
            encode_mutation(&test_command(), 1_700_000_000_123, test_position()).unwrap();
        payload[2] = payload[2].wrapping_add(1);

        let error = decode_mutation(&payload).unwrap_err().to_string();
        assert!(
            error.contains(&CURRENT_PROTOCOL_VERSION.to_string()) && error.contains("protocol"),
            "the mismatch must name both versions: {error}"
        );
    }
}
