//! Where an accepted command becomes durable.
//!
//! The daemon appends every accepted command to the bundle's mutation journal
//! and waits for that append to be durable *before* the acceptance reaches the
//! client and before any runtime realization is published. The manifest is no
//! longer rewritten per command; it is rewritten at a checkpoint, which folds
//! the recorded mutations into it and compacts the journal. Every write runs
//! on one persistence worker in the order the daemon hands it over, and the
//! daemon waits for a command's append but not for a checkpoint or a
//! progress marker (see [`DurableJournal`]).
//!
//! Frames a native daemon realizes between commands move no revision, so no
//! mutation records them. A progress marker does (see [`ProgressCadence`]):
//! a single overwritten journal row holding the position the show reached
//! above the newest mutation, written at a bounded cadence, which replay
//! ticks the engine idle up to after the last recorded command.
//!
//! A title animation edit (GX-011) also changes the stored template outside
//! `project.json`. Its new animation is staged beside the template as of the
//! edit's revision before the batch is appended
//! ([`DurableStore::stage_title_animations`]), and committed over the
//! template only once a manifest at or after that revision is durable (at
//! every checkpoint, and at startup and after recovery, see
//! [`commit_staged_title_animations`]). The template on disk therefore always
//! matches the newest manifest, and journal replay re-applies exactly the
//! edits the manifest does not hold yet.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fm_persistence::{JournalWriter, MutationBatch, ProjectPosition, ProjectStore, StoredProject};
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
/// Recorded mutations after which a native daemon folds the journal into the
/// manifest right after a command's frame was realized
/// ([`DurableStore::checkpoint_if_due`]), where the manifest rewrite has
/// the most time before the next frame deadline, instead of in front of the
/// frame of the command that reaches [`MAX_MUTATIONS_PER_CHECKPOINT`].
const EARLY_CHECKPOINT_MUTATIONS: u64 = MAX_MUTATIONS_PER_CHECKPOINT * 3 / 4;

/// Recent appends whose durations [`DurableStore::recent_append_peak`]
/// reports the longest of.
const RECENT_APPENDS: usize = 32;

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

    /// Folds the recorded mutations into the manifest ahead of the
    /// [`MAX_MUTATIONS_PER_CHECKPOINT`] bound once enough of them are pending.
    /// `project` must hold the newest recorded mutation's revision, as for
    /// [`Self::checkpoint`]. Nothing depends on it: every recorded mutation is
    /// already durable in the journal, so the journal runs it in the
    /// background, a failure is only reported, and the bounded checkpoint in
    /// front of a later append tries again.
    fn checkpoint_if_due(&self, _project: &StoredProject) -> AppResult<()> {
        Ok(())
    }

    /// Durably records that the show reached `position` without a mutation:
    /// idle frames realized above the newest recorded mutation. `position`
    /// must hold that mutation's revision. Stores without a journal record
    /// nothing. The journal writes the marker in the background and reports
    /// a failure itself: a lost marker loses only idle progress.
    fn record_progress(&self, _position: ProjectPosition) -> AppResult<()> {
        Ok(())
    }

    /// Folds idle progress into the manifest: `project` holds the newest
    /// recorded mutation's revision and the idle position the show reached.
    /// The journal folds in the background and reports a failure itself,
    /// as for [`Self::record_progress`].
    fn fold_progress(&self, project: &StoredProject) -> AppResult<()> {
        self.checkpoint(project)
    }

    /// Whether background persistence work ([`Self::checkpoint_if_due`],
    /// [`Self::record_progress`], [`Self::fold_progress`]) is still running.
    /// A [`Self::record`] handed over meanwhile waits for it.
    fn busy(&self) -> bool {
        false
    }

    /// Stages the template `animations` (stored template URI and the
    /// animation every title naming it shows) an accepted command leaves at
    /// project `revision`, before [`Self::record`] journals that command.
    /// Stores without templates stage nothing.
    fn stage_title_animations(
        &self,
        _revision: u64,
        _animations: &[(String, fm_titles::TitleAnimation)],
    ) -> AppResult<()> {
        Ok(())
    }

    /// Removes what [`Self::stage_title_animations`] staged at `revision`
    /// when the command was not journaled after all.
    ///
    /// A copy left behind must stop the daemon: the next accepted command
    /// reuses `revision`, and the checkpoint that covers it would commit
    /// the refused animation over the template. The next startup's recovery
    /// discards every copy staged after the durable revision.
    fn discard_staged_title_animations(&self, _revision: u64, _uris: &[String]) -> AppResult<()> {
        Ok(())
    }

    /// The longest a recent [`Self::record`] took to make its command
    /// durable (the append and its sync), or `None` before any. A native
    /// daemon dispatches a command only with at least that much of the
    /// frame interval left, so the sync ends before the frame that
    /// realizes the command. Stores that do not sync report nothing.
    fn recent_append_peak(&self) -> Option<Duration> {
        None
    }

    /// Copies the media file at host path `path` into the bundle's assets
    /// and returns its `asset://` URI and whether a new file was written
    /// (`false` when an identical asset was reused) (IN-026 batch import and
    /// source replacement). Stores without a bundle import nothing (`None`).
    fn import_media_path(&self, _path: &std::path::Path) -> AppResult<Option<(String, bool)>> {
        Ok(None)
    }

    /// Removes the asset a [`Self::import_media_path`] copy wrote at
    /// `asset_uri`, when the command it was copied for did not keep it.
    fn discard_imported_media(&self, _asset_uri: &str) -> AppResult<()> {
        Ok(())
    }
}

/// Commits every staged template animation in `store`'s bundle through
/// `project`'s revision (see the module docs), including templates no title
/// names any more: an edit is durable once a manifest covers its revision,
/// whatever happened to its title afterwards. `discard_later` also removes
/// copies staged after it, whose batches will never be replayed. Startup
/// and recovery use this; a running daemon's checkpoints commit only the
/// templates it staged ([`DurableJournal`]).
pub(crate) fn commit_staged_title_animations(
    store: &ProjectStore,
    project: &StoredProject,
    discard_later: bool,
) -> AppResult<()> {
    store
        .commit_every_staged_title_animation(project.position().revision, discard_later)
        .map_err(|error| AppFailure(format!("staged title animation: {error}")))?;
    Ok(())
}

/// The live journal of a served project.
///
/// The journal database is opened once, at startup, and held for the daemon's
/// run: it is a single-writer resource this process owns, and reopening it per
/// command made an accepted command cost what the whole show had cost so far.
///
/// # The persistence worker
///
/// The writer lives on a persistence worker thread, and every operation on
/// the bundle this store makes (journal appends, checkpoints, progress
/// markers, staged template animations, imported files) is a job the worker
/// runs one at a time in the order the daemon handed them over. The worker
/// therefore performs exactly the sequence of storage operations the daemon
/// used to perform on its own thread, against exactly the journal state it
/// used to see. What changed is only which operations the daemon waits for:
///
/// - a command's append (and everything a command stages) is a synchronous
///   job, so [`DurableStore::record`] still returns only once the append is
///   durable, before the command is acknowledged or committed;
/// - folding the journal into the manifest ([`DurableStore::checkpoint_if_due`],
///   [`DurableStore::fold_progress`]) and the progress marker
///   ([`DurableStore::record_progress`]) are background jobs, so the frame
///   loop no longer waits for a manifest rewrite or a marker's sync.
///
/// A background job never stands for an acknowledgement: everything it
/// writes is either already durable in the journal (a checkpoint folds
/// recorded batches) or is idle progress a crash may lose by design. A job
/// handed over after it (the next append) runs after it, so it sees the
/// journal head it was asked about. [`DurableStore::busy`] reports
/// background work in flight; a native daemon holds a session command back
/// while it is, so the command's append does not queue behind a checkpoint
/// in front of the frame it is realized on.
pub(crate) struct DurableJournal {
    jobs: Option<mpsc::Sender<Job>>,
    worker: Option<thread::JoinHandle<()>>,
    finished: mpsc::Receiver<Finished>,
    /// Background jobs handed over whose [`Finished`] was not drained yet.
    in_flight: Cell<usize>,
    /// Mutations (and refusal receipts) since the newest checkpoint asked
    /// for, background or not.
    pending: Cell<u64>,
    /// Background checkpoints asked for, numbering each, so a failure
    /// restores its mutations to `pending` only while it is the newest.
    checkpoints_requested: Cell<u64>,
    /// How long each of the last [`RECENT_APPENDS`] recorded mutations took
    /// to become durable on the worker, oldest first.
    appends: RefCell<VecDeque<Duration>>,
    /// A background progress write failed and has not succeeded since.
    progress_failing: Cell<bool>,
}

/// The environment variable that makes the persistence worker report when
/// each background checkpoint and progress marker begins and ends, so a
/// crash test can kill the daemon while one is being written.
const JOURNAL_TRACE_ENV: &str = "FREEMIXD_JOURNAL_TRACE";

/// A failure on the worker, sent back to the daemon's thread.
enum WorkerError {
    /// The command can never succeed and is refused as decided.
    Refusal(crate::CommandRefusal),
    Failure(String),
}

impl WorkerError {
    fn from_app(error: Box<dyn std::error::Error>) -> Self {
        match error.downcast::<crate::CommandRefusal>() {
            Ok(refusal) => Self::Refusal(*refusal),
            Err(error) => Self::Failure(error.to_string()),
        }
    }

    fn into_app(self) -> Box<dyn std::error::Error> {
        match self {
            Self::Refusal(refusal) => Box::new(refusal),
            Self::Failure(message) => Box::new(AppFailure(message)),
        }
    }
}

type WorkerResult<T> = Result<T, WorkerError>;
type Reply<T> = mpsc::SyncSender<WorkerResult<T>>;

/// What the daemon asks the worker to do, in order.
enum Job {
    Record {
        command: Box<CommandMessage>,
        now_millis: u64,
        base_revision: u64,
        revision: u64,
        position: ProjectPosition,
        /// How long the append took on the worker, excluding the wait in
        /// the queue.
        reply: Reply<Duration>,
    },
    Checkpoint {
        project: Box<StoredProject>,
        /// `None` for a background checkpoint, whose outcome comes back as
        /// [`Finished`].
        reply: Option<Reply<()>>,
        background: Background,
    },
    Progress {
        position: ProjectPosition,
    },
    Stage {
        revision: u64,
        animations: Vec<(String, fm_titles::TitleAnimation)>,
        reply: Reply<()>,
    },
    DiscardStaged {
        revision: u64,
        uris: Vec<String>,
        reply: Reply<()>,
    },
    Import {
        path: std::path::PathBuf,
        reply: Reply<(String, bool)>,
    },
    DiscardImport {
        asset_uri: String,
        reply: Reply<()>,
    },
    /// Answers once every job handed over before it has run.
    Flush {
        reply: Reply<()>,
    },
}

/// What a background job was for, which decides how its outcome is reported.
#[derive(Clone, Copy, Debug)]
enum Background {
    /// Not a background job.
    None,
    /// An early checkpoint covering `mutations` mutations, the `request`th
    /// asked for.
    Checkpoint { mutations: u64, request: u64 },
    /// Idle progress folded into the manifest at `frame`.
    Fold { frame: u64 },
    /// A progress marker at `frame`.
    Marker { frame: u64 },
}

/// A background job's outcome.
struct Finished {
    background: Background,
    result: Result<(), String>,
}

impl DurableJournal {
    /// Opens `store`'s journal writer on a new persistence worker, failing
    /// as opening the writer fails.
    pub(crate) fn open(store: &ProjectStore) -> AppResult<Self> {
        let store = store.clone();
        let (jobs, queue) = mpsc::channel();
        let (finished_sender, finished) = mpsc::channel();
        let (opened_sender, opened) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("freemixd-journal".into())
            .spawn(move || {
                let writer = match store.open_journal_writer() {
                    Ok(writer) => writer,
                    Err(error) => {
                        let _ = opened_sender.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = opened_sender.send(Ok(()));
                Worker {
                    writer,
                    staged: BTreeSet::new(),
                    trace: std::env::var(JOURNAL_TRACE_ENV).as_deref() == Ok("1"),
                }
                .run(&queue, &finished_sender);
            })?;
        match opened.recv() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                let _ = worker.join();
                return Err(AppFailure(message).into());
            }
            Err(_) => {
                let _ = worker.join();
                return Err(AppFailure("the journal worker stopped while opening".into()).into());
            }
        }
        Ok(Self {
            jobs: Some(jobs),
            worker: Some(worker),
            finished,
            in_flight: Cell::new(0),
            pending: Cell::new(0),
            checkpoints_requested: Cell::new(0),
            appends: RefCell::new(VecDeque::new()),
            progress_failing: Cell::new(false),
        })
    }

    /// Folds anything recorded since the last checkpoint into the manifest,
    /// after every background job handed over before it.
    ///
    /// Nothing recorded means the manifest already holds `project`, so this
    /// does not rewrite it.
    pub(crate) fn settle(&self, project: &StoredProject) -> AppResult<()> {
        self.flush()?;
        if self.pending.get() == 0 {
            return Ok(());
        }
        self.checkpoint(project)
    }

    /// Waits for every job handed over so far, reporting the background
    /// ones' outcomes.
    pub(crate) fn flush(&self) -> AppResult<()> {
        self.call(|reply| Job::Flush { reply })
    }

    fn send(&self, job: Job) -> AppResult<()> {
        self.jobs
            .as_ref()
            .and_then(|jobs| jobs.send(job).ok())
            .ok_or_else(|| AppFailure("the journal worker stopped".into()).into())
    }

    /// Hands over a synchronous job and waits for its answer. Every
    /// background job handed over before it has finished by then, and their
    /// outcomes are applied before this returns.
    fn call<T>(&self, job: impl FnOnce(Reply<T>) -> Job) -> AppResult<T> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.send(job(reply))?;
        let answer = answer
            .recv()
            .map_err(|_| AppFailure("the journal worker stopped".into()));
        self.drain();
        answer?.map_err(WorkerError::into_app)
    }

    /// Hands over a background job.
    fn queue(&self, job: Job) -> AppResult<()> {
        self.send(job)?;
        self.in_flight.set(self.in_flight.get() + 1);
        Ok(())
    }

    /// Applies and reports every finished background job.
    fn drain(&self) {
        while let Ok(Finished { background, result }) = self.finished.try_recv() {
            self.in_flight.set(self.in_flight.get().saturating_sub(1));
            match background {
                Background::None => {}
                Background::Checkpoint { mutations, request } => {
                    if let Err(error) = result {
                        // Its mutations are still only in the journal: they
                        // count toward the next checkpoint again, unless a
                        // newer one was asked for and covers them.
                        if request == self.checkpoints_requested.get() {
                            self.pending
                                .set(self.pending.get().saturating_add(mutations));
                        }
                        eprintln!("FREEMIXD_JOURNAL\tv=1\tcheckpoint=deferred\terror={error}");
                    }
                }
                Background::Fold { frame } | Background::Marker { frame } => {
                    match (result, self.progress_failing.get()) {
                        (Ok(()), true) => {
                            self.progress_failing.set(false);
                            eprintln!("FREEMIXD_JOURNAL\tv=1\tprogress=recovered\tframe={frame}");
                        }
                        (Err(error), false) => {
                            self.progress_failing.set(true);
                            eprintln!(
                                "FREEMIXD_JOURNAL\tv=1\tprogress=failed\tframe={frame}\terror={error}"
                            );
                        }
                        // Reported once per failing streak.
                        (Ok(()), false) | (Err(_), true) => {}
                    }
                }
            }
        }
    }

    /// Hands over a background checkpoint of `project`, which holds the
    /// newest recorded mutation's revision.
    fn checkpoint_in_background(
        &self,
        project: &StoredProject,
        fold_frame: Option<u64>,
    ) -> AppResult<()> {
        let request = self.checkpoints_requested.get() + 1;
        let background = fold_frame.map_or(
            Background::Checkpoint {
                mutations: self.pending.get(),
                request,
            },
            |frame| Background::Fold { frame },
        );
        self.queue(Job::Checkpoint {
            project: Box::new(project.clone()),
            reply: None,
            background,
        })?;
        self.checkpoints_requested.set(request);
        self.pending.set(0);
        Ok(())
    }
}

impl Drop for DurableJournal {
    /// Runs every job handed over, background ones included, before the
    /// journal closes.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.drain();
    }
}

impl DurableStore for DurableJournal {
    fn import_media_path(&self, path: &std::path::Path) -> AppResult<Option<(String, bool)>> {
        self.call(|reply| Job::Import {
            path: path.to_path_buf(),
            reply,
        })
        .map(Some)
    }

    fn discard_imported_media(&self, asset_uri: &str) -> AppResult<()> {
        self.call(|reply| Job::DiscardImport {
            asset_uri: asset_uri.to_owned(),
            reply,
        })
    }

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
        let took = self.call(|reply| Job::Record {
            command: Box::new(command.clone()),
            now_millis,
            base_revision: previous.position().revision,
            revision,
            position: updated.position(),
            reply,
        })?;
        self.pending.set(self.pending.get().saturating_add(1));
        let mut appends = self.appends.borrow_mut();
        if appends.len() == RECENT_APPENDS {
            appends.pop_front();
        }
        appends.push_back(took);
        Ok(())
    }

    fn recent_append_peak(&self) -> Option<Duration> {
        self.appends.borrow().iter().copied().max()
    }

    fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
        self.call(|reply| Job::Checkpoint {
            project: Box::new(project.clone()),
            reply: Some(reply),
            background: Background::None,
        })?;
        self.pending.set(0);
        Ok(())
    }

    fn stage_title_animations(
        &self,
        revision: u64,
        animations: &[(String, fm_titles::TitleAnimation)],
    ) -> AppResult<()> {
        if animations.is_empty() {
            return Ok(());
        }
        self.call(|reply| Job::Stage {
            revision,
            animations: animations.to_vec(),
            reply,
        })
    }

    fn discard_staged_title_animations(&self, revision: u64, uris: &[String]) -> AppResult<()> {
        if uris.is_empty() {
            return Ok(());
        }
        self.call(|reply| Job::DiscardStaged {
            revision,
            uris: uris.to_vec(),
            reply,
        })
    }

    fn checkpoint_if_due(&self, project: &StoredProject) -> AppResult<()> {
        self.drain();
        if self.pending.get() < EARLY_CHECKPOINT_MUTATIONS || self.in_flight.get() > 0 {
            return Ok(());
        }
        self.checkpoint_in_background(project, None)
    }

    fn fold_progress(&self, project: &StoredProject) -> AppResult<()> {
        self.checkpoint_in_background(project, Some(project.position().frames_rendered))
    }

    fn record_progress(&self, position: ProjectPosition) -> AppResult<()> {
        self.queue(Job::Progress { position })
    }

    fn busy(&self) -> bool {
        self.drain();
        self.in_flight.get() > 0
    }
}

/// The persistence worker: the journal writer and what it staged.
struct Worker<'store> {
    writer: JournalWriter<'store>,
    /// Every template URI this run staged an animation for. Startup
    /// recovery committed (or discarded) every copy staged before the run,
    /// so these are the only templates a checkpoint can find staged —
    /// including one whose last title was removed after its edit.
    staged: BTreeSet<String>,
    trace: bool,
}

impl Worker<'_> {
    fn run(mut self, queue: &mpsc::Receiver<Job>, finished: &mpsc::Sender<Finished>) {
        while let Ok(job) = queue.recv() {
            match job {
                Job::Record {
                    command,
                    now_millis,
                    base_revision,
                    revision,
                    position,
                    reply,
                } => {
                    let started = Instant::now();
                    let result = self
                        .append(&command, now_millis, base_revision, revision, position)
                        .map(|()| started.elapsed());
                    let _ = reply.send(result);
                }
                Job::Checkpoint {
                    project,
                    reply,
                    background,
                } => {
                    self.trace_event("checkpoint=begin", project.position());
                    let result = self.checkpoint(&project);
                    self.trace_event("checkpoint=end", project.position());
                    match reply {
                        Some(reply) => {
                            let _ = reply.send(result.map_err(WorkerError::from_app));
                        }
                        None => {
                            let _ = finished.send(Finished {
                                background,
                                result: result.map_err(|error| error.to_string()),
                            });
                        }
                    }
                }
                Job::Progress { position } => {
                    self.trace_event("marker=begin", position);
                    let result = self
                        .writer
                        .record_progress(&encode_progress(position))
                        .map_err(|error| error.to_string());
                    self.trace_event("marker=end", position);
                    let _ = finished.send(Finished {
                        background: Background::Marker {
                            frame: position.frames_rendered,
                        },
                        result,
                    });
                }
                Job::Stage {
                    revision,
                    animations,
                    reply,
                } => {
                    let _ = reply.send(self.stage(revision, &animations));
                }
                Job::DiscardStaged {
                    revision,
                    uris,
                    reply,
                } => {
                    let _ = reply.send(self.discard_staged(revision, &uris));
                }
                Job::Import { path, reply } => {
                    let _ = reply.send(self.import(&path));
                }
                Job::DiscardImport { asset_uri, reply } => {
                    let result = self
                        .writer
                        .project_store()
                        .remove_imported_media(&asset_uri)
                        .map_err(|error| {
                            WorkerError::Failure(format!(
                                "imported media cannot be removed: {error}"
                            ))
                        });
                    let _ = reply.send(result);
                }
                Job::Flush { reply } => {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }

    fn trace_event(&self, event: &str, position: ProjectPosition) {
        if self.trace {
            eprintln!(
                "FREEMIXD_JOURNAL\tv=1\t{event}\trevision={}\tframe={}",
                position.revision, position.frames_rendered
            );
        }
    }

    fn import(&self, path: &std::path::Path) -> WorkerResult<(String, bool)> {
        let imported = self
            .writer
            .project_store()
            .import_media_file(path)
            .map_err(|error| {
                WorkerError::Failure(format!("media file cannot be imported: {error}"))
            })?;
        let copied = matches!(
            imported.disposition,
            fm_persistence::AssetDisposition::Copied { .. }
        );
        Ok((imported.asset_uri, copied))
    }

    fn append(
        &self,
        command: &CommandMessage,
        now_millis: u64,
        base_revision: u64,
        revision: u64,
        position: ProjectPosition,
    ) -> WorkerResult<()> {
        // An imported LUT's file is written before its batch, so every
        // manifest and journal that names the LUT finds the file.
        crate::grading::write_imported_lut(self.writer.project_store(), command).map_err(
            |error| match error {
                // Not a durability failure: the same command can never
                // succeed, so it is refused as a conflict.
                fm_persistence::LutError::IdReused(id) => {
                    WorkerError::Refusal(crate::CommandRefusal {
                        code: fm_command::RejectionCode::Conflict,
                        retryable: false,
                        message: error.to_string(),
                        reason: Some(fm_protocol::RefusalReason::DuplicateId {
                            kind: fm_protocol::IdKind::Lut,
                            id: u128::from(id.get().get()),
                        }),
                    })
                }
                error => {
                    WorkerError::Failure(format!("imported LUT could not be written: {error}"))
                }
            },
        )?;
        let fail = |error: &dyn std::fmt::Display| WorkerError::Failure(error.to_string());
        let sequence = self
            .writer
            .head_sequence()
            .checked_add(1)
            .ok_or_else(|| WorkerError::Failure("journal sequence overflow".into()))?;
        let payload =
            encode_mutation(command, now_millis, position).map_err(|error| fail(&error))?;
        self.writer
            .append_batch(&MutationBatch::new(
                sequence,
                base_revision,
                revision,
                payload,
            ))
            .map_err(|error| fail(&error))
    }

    fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
        // `checkpoint_and_compact` saves the manifest, then advances the
        // checkpoint, discards the applied batches and truncates the
        // write-ahead log; it refuses a manifest whose revision is not the one
        // it is checkpointing.
        self.writer
            .checkpoint_and_compact(project, self.writer.head_sequence())?;
        // The manifest now holds every staged animation edit through its
        // revision; later ones still wait for their batches' checkpoint.
        self.writer
            .project_store()
            .commit_staged_title_animations(
                self.staged.iter().map(String::as_str),
                project.position().revision,
                false,
            )
            .map_err(|error| AppFailure(format!("staged title animation: {error}")))?;
        Ok(())
    }

    fn stage(
        &mut self,
        revision: u64,
        animations: &[(String, fm_titles::TitleAnimation)],
    ) -> WorkerResult<()> {
        for (uri, animation) in animations {
            self.staged.insert(uri.clone());
            self.writer
                .project_store()
                .stage_title_animation(uri, revision, animation)
                .map_err(|error| {
                    WorkerError::Failure(format!("title animation could not be staged: {error}"))
                })?;
        }
        Ok(())
    }

    fn discard_staged(&self, revision: u64, uris: &[String]) -> WorkerResult<()> {
        let mut failure = None;
        for uri in uris {
            if let Err(error) = self
                .writer
                .project_store()
                .discard_staged_title_animation(uri, revision)
            {
                failure.get_or_insert(error);
            }
        }
        match failure {
            None => Ok(()),
            Some(error) => Err(WorkerError::Failure(format!(
                "staged title animation at revision {revision} could not be discarded: {error}"
            ))),
        }
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
