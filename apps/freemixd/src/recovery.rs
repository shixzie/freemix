//! Journal replay and checkpoint recovery: opening a stored project at the
//! revision its journal holds, folding replayed mutations back into the
//! manifest, and abandoning history to the last checkpoint.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// Loads the manifest and replays whatever the journal recorded after it.
///
/// The manifest is the last checkpointed state; every mutation accepted since
/// then is in the journal. Recovery therefore loads the manifest, re-executes
/// the recorded commands through the same path that first accepted them, and
/// checkpoints the result, so the daemon opens at the revision the operator was
/// last acknowledged at rather than at the last checkpoint.
///
/// Opening also creates the journal when the project has never had one, so the
/// daemon starts owning a journal whose checkpoint agrees with the manifest and
/// the first command can be appended without re-reading it.
///
/// `recover_to_checkpoint` abandons that history instead of replaying it. See
/// [`abandon_to_checkpoint`].
pub(crate) fn load_and_recover(
    store: &ProjectStore,
    recover_to_checkpoint: bool,
) -> AppResult<StoredProject> {
    let manifest = store.load()?;
    // Finish any checkpoint that saved the manifest but did not commit its
    // staged title animations yet, so templates load as the manifest has
    // them; animations staged later wait for their replayed batches.
    journal::commit_staged_title_animations(store, &manifest, false)?;
    let durable = recover_from_manifest(store, manifest, recover_to_checkpoint)?;
    // Commit what replay folded into the manifest and drop staged copies of
    // batches that were never written or were abandoned.
    journal::commit_staged_title_animations(store, &durable, true)?;
    Ok(durable)
}

pub(crate) fn recover_from_manifest(
    store: &ProjectStore,
    manifest: StoredProject,
    recover_to_checkpoint: bool,
) -> AppResult<StoredProject> {
    if recover_to_checkpoint {
        return abandon_to_checkpoint(store, manifest);
    }
    let scan = store.recover_journal()?;
    for observation in scan.observations() {
        eprintln!("FREEMIXD_JOURNAL\tv=1\tobservation={observation}");
    }
    let progress = scan
        .progress()
        .map(|progress| journal::decode_progress(progress.payload()))
        .transpose()?
        // A marker at or behind the manifest (a fold or a clean stop wrote
        // the manifest after it) has nothing left to replay.
        .filter(|progress| {
            !scan.batches().is_empty()
                || progress.frames_rendered > manifest.position().frames_rendered
        });
    if scan.batches().is_empty() && progress.is_none() {
        return Ok(manifest);
    }
    let journal_head = scan
        .batches()
        .last()
        .map_or_else(|| scan.checkpoint_sequence(), MutationBatch::sequence);

    let bundle_assets = load_bundle_assets(store, manifest.project())?;
    let durable = replay_journal(&manifest, &bundle_assets, scan.batches(), progress)?;
    // Fold the replayed mutations into the manifest and discard them, so the
    // next crash replays only what happened after this point and a second
    // recovery of the same batches is impossible.
    store.checkpoint_and_compact(&durable, journal_head)?;
    eprintln!(
        "FREEMIXD_JOURNAL\tv=1\trecovered_batches={}\tfrom_revision={}\tto_revision={}\tframe={}",
        scan.batches().len(),
        manifest.position().revision,
        durable.position().revision,
        durable.position().frames_rendered
    );
    Ok(durable)
}

/// Throws away everything the journal recorded after the manifest.
///
/// Refusing a replay that does not reproduce what was acknowledged is right,
/// and it leaves an operator minutes from air with a bundle that will not open.
/// Deleting the journal directory was the only way back on air, and it discards
/// the same work with no record that it ever existed. This is that act made
/// deliberate and legible: it is asked for by name on the command line, and
/// every batch it destroys is printed with its sequence, its revision and the
/// command it carried — including the ones too damaged to decode — before the
/// daemon serves anything.
pub(crate) fn abandon_to_checkpoint(
    store: &ProjectStore,
    manifest: StoredProject,
) -> AppResult<StoredProject> {
    let abandoned = store.abandon_unapplied_batches()?;
    for batch in abandoned.batches() {
        let command = journal::decode_mutation(batch.payload()).map_or_else(
            |error| format!("<unreadable: {error}>"),
            |recorded| recorded.command.id,
        );
        eprintln!(
            "FREEMIXD_JOURNAL\tv=1\tabandoned_batch={}\tbase_revision={}\trevision={}\tcommand={command}",
            batch.sequence(),
            batch.base_revision(),
            batch.revision()
        );
    }
    eprintln!(
        "FREEMIXD_JOURNAL\tv=1\trecover_to_checkpoint=1\tabandoned_batches={}\tserving_revision={}",
        abandoned.batches().len(),
        manifest.position().revision
    );
    Ok(manifest)
}

/// Rebuilds the pre-crash state by re-running the recorded commands.
///
/// Nothing in this workspace applies an event to a project: the engine is
/// command-sourced, so the only faithful way to reach the pre-crash state is
/// the path that produced it. Each batch therefore carries the command and the
/// instant it was submitted, and replay runs `execute_durable_command` against
/// an engine restored from the manifest, recording nothing and publishing to
/// nobody.
///
/// Frames a native daemon realized between two commands are ticked idle
/// before the later one (see [`replay_idle_frames`]), and the frames it
/// realized after the last one are ticked idle up to the journal's progress
/// marker, `progress`, when that is ahead (see [`journal::ProgressCadence`]).
/// Every idle frame counts against one bound, [`IdleReplayBudget`], so a
/// damaged frame number refuses startup instead of ticking forever.
///
/// Every batch must land on exactly the position it recorded — not just its
/// revision. A refused command advances no revision and a frame that elapsed
/// between two commands advances no revision either, so the revision alone
/// cannot see a replay whose frame cursor, clock or event stream came back
/// different from the one the operator was acknowledged against. A divergence
/// means the replay is not that history, so it is reported instead of silently
/// becoming the project's state.
pub(crate) fn replay_journal(
    manifest: &StoredProject,
    bundle_assets: &BundleAssets,
    batches: &[MutationBatch],
    progress: Option<ProjectPosition>,
) -> AppResult<StoredProject> {
    let mut durable = manifest.clone();
    let project_id = durable.project().id();
    let identity = format!("project-{project_id}");
    let mut control = ControlService::new(
        restore_engine_with_titles(&durable, bundle_assets)?,
        Policy::development(),
        identity.clone(),
        format!("{identity}-log"),
        ControlLimits::default(),
    );
    let server = control_server_identity(&control, project_id);
    let principal = development_principal()?;
    let mut budget = IdleReplayBudget::new(durable.project().settings().frame_rate);
    // Documents are probed (never converted) so each replayed page step
    // resolves against the page count it first did (IN-014).
    let replayed = ReplayedMutations {
        documents: bundle_assets
            .assets_root
            .as_ref()
            .map(|root| Arc::new(documents::DocumentPages::new(root, None))),
    };
    for batch in batches {
        let recorded = journal::decode_mutation(batch.payload())?;
        if recorded.position.revision != batch.revision() {
            return Err(AppFailure(format!(
                "journal batch {} claims revision {} but its record holds revision {}",
                batch.sequence(),
                batch.revision(),
                recorded.position.revision
            ))
            .into());
        }
        replay_idle_frames(&mut control, &server, &recorded, &mut budget).map_err(|error| {
            AppFailure(format!(
                "journal batch {} (command `{}`): {error}",
                batch.sequence(),
                recorded.command.id
            ))
        })?;
        execute_durable_command_with_ticks(
            &mut control,
            &replayed,
            &mut durable,
            &principal,
            &server,
            &recorded.command,
            recorded.submitted_at_millis,
            None,
            Realize::None,
            |control, server| Ok(control.tick(server)?.runtime_events),
        )?;
        if durable.position() != recorded.position {
            return Err(AppFailure(format!(
                "journal batch {} replayed command `{}` to {:?} instead of the acknowledged {:?}",
                batch.sequence(),
                recorded.command.id,
                durable.position(),
                recorded.position
            ))
            .into());
        }
    }
    if let Some(progress) = progress {
        replay_progress(&mut control, &server, &mut durable, progress, &mut budget)?;
    }
    Ok(durable)
}

/// Ticks the engine idle up to the journal's progress marker.
///
/// The marker records the position a native daemon reached after the last
/// recorded command, so the frames between are idle frames, ticked exactly
/// as the pacer realized them; the result must land on the whole recorded
/// position, like a replayed batch. A marker at or behind the replay names
/// progress a later checkpoint already holds, and is skipped.
pub(crate) fn replay_progress(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    durable: &mut StoredProject,
    progress: ProjectPosition,
    budget: &mut IdleReplayBudget,
) -> AppResult<()> {
    if progress.revision != durable.position().revision
        || progress.frames_rendered <= durable.position().frames_rendered
    {
        return Ok(());
    }
    budget
        .tick_until(control, server, progress.frames_rendered)
        .map_err(|error| AppFailure(format!("journal progress marker: {error}")))?;
    let snapshot = control.idle_engine_snapshot().map_err(|error| {
        AppFailure(format!(
            "journal progress marker at frame {} is not an idle frame boundary: {error}",
            progress.frames_rendered
        ))
    })?;
    let advanced = stored_project_checkpoint(durable, &snapshot)?;
    if advanced.position() != progress {
        return Err(AppFailure(format!(
            "journal progress marker replayed to {:?} instead of the recorded {progress:?}",
            advanced.position()
        ))
        .into());
    }
    *durable = advanced;
    Ok(())
}

/// How many idle frames journal replay may still tick.
///
/// Idle frames carry no command, so replay reaches them only from recorded
/// frame numbers. A damaged journal can name any frame, and ticking to it
/// would hold startup for hours or forever, so the whole replay shares one
/// budget of [`journal::MAX_REPLAY_IDLE_SECONDS`] of show at the project's
/// frame rate, checked *before* ticking: a request past it refuses startup
/// immediately with the frames it asked for.
pub(crate) struct IdleReplayBudget {
    remaining: u64,
    limit: u64,
}

impl IdleReplayBudget {
    pub(crate) fn new(rate: fm_types::FrameRate) -> Self {
        let limit = journal::frames_in(journal::MAX_REPLAY_IDLE_SECONDS, rate);
        Self {
            remaining: limit,
            limit,
        }
    }

    /// Ticks the engine idle until its cursor reaches `target`, refusing a
    /// target the remaining budget does not cover.
    pub(crate) fn tick_until(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        target: u64,
    ) -> AppResult<()> {
        let from = control.frame_cursor();
        self.check(from, target)?;
        while control.frame_cursor() < target {
            control.tick(server)?;
        }
        self.charge(control.frame_cursor().saturating_sub(from));
        Ok(())
    }

    /// Charges `frames` ticked idle frames.
    pub(crate) const fn charge(&mut self, frames: u64) {
        self.remaining = self.remaining.saturating_sub(frames);
    }

    /// Refuses ticking from `cursor` up to `target` when the remaining
    /// budget does not cover it.
    pub(crate) fn check(&self, cursor: u64, target: u64) -> AppResult<()> {
        let frames = target.saturating_sub(cursor);
        if frames > self.remaining {
            return Err(AppFailure(format!(
                "replay would tick {frames} idle frames (frame {cursor} to {target}), past the \
                 bound of {} idle frames ({} hours of show) per recovery; the recorded frame is \
                 implausible, so the journal is damaged (--recover-to-checkpoint discards it)",
                self.limit,
                journal::MAX_REPLAY_IDLE_SECONDS / 3_600
            ))
            .into());
        }
        Ok(())
    }
}

/// Realizes the frames the daemon rendered before `recorded` was committed.
///
/// The native media runtime's pacer realizes a frame on every deadline:
/// idle frames between commands, and every frame of a command's timed
/// transition or title phase after its first, on its frame loop. A command
/// can therefore be committed at a later frame cursor than the previous
/// command left a replay at, or while that command's transition is still
/// running. Those frames carry no command, so the journal records the frame
/// cursor each command was committed at, and replay ticks the engine up to
/// it, exactly as the pacer did; the command itself then realizes nothing
/// (its projection still settles where the daemon's did). A simulated
/// daemon realizes the rest of a command's transition on its own
/// wall-clock frame loop the same way, and renders idle frames only to
/// catch a client's playlist control or a clock's advance up to the
/// show's wall-clock frame (`playlist_clock::PlaylistClock::catch_up`,
/// `catch_up_simulated_advance`); replay ticks all of those the same way.
///
/// A replay that still lands elsewhere is reported by the position check.
pub(crate) fn replay_idle_frames(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    recorded: &journal::RecordedMutation,
    budget: &mut IdleReplayBudget,
) -> AppResult<()> {
    let cursor = control.frame_cursor();
    if recorded.began_frame < cursor {
        return Err(AppFailure(format!(
            "the command was recorded at frame {} but replay is already at frame {cursor}",
            recorded.began_frame
        ))
        .into());
    }
    budget.tick_until(control, server, recorded.began_frame)
}
