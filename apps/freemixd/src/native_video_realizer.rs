//! The native realizer's video compose handoff (structural item 1).
//!
//! The control pass loop keeps `fm-control`, peers, the journal, and the
//! durable project. [`NativeVideoHandle`] runs the GPU compose and the
//! Preview composite on a dedicated realizer thread. A job carries the engine
//! frame, the plan, the compiled grades, and the source playback registries;
//! the reply returns the registries with the pictures. Grade uploads happen
//! on this thread too: the worker caches the prepared grades by the engine's
//! compiled revision, so the control loop never uploads a grade and a
//! Program and Preview compose share the same prepared textures.
//!
//! The channel is bounded and carries one job at a time per kind; a compose
//! is collected before the next submission of its kind. The runtime itself is
//! shared as an `Arc<NativeMediaRuntime>` for now: source ingestion, monitor
//! taps, and display presentation still use it on the control thread. Moving
//! those to the realizer is the next stage.

use std::{
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

use fm_engine::{CompiledProjectGrades, FrameResult};
use fm_gpu::NativeTexture;
use freemixd::native_media::{
    NativeMediaError, NativeMediaRuntime, NativeOutputFrame, NativeProjectGrades,
    NativeProjectPlan, NativeSourcePlayback, NativeSourceRenderError,
};

/// One compose's inputs.
pub(crate) struct VideoRenderJob {
    pub frame: FrameResult,
    pub plan: NativeProjectPlan,
    pub playback: NativeSourcePlayback,
    pub stingers: NativeSourcePlayback,
    /// The engine revision's compiled grades; the worker uploads them once
    /// per revision.
    pub grades: Arc<CompiledProjectGrades>,
    pub grade_revision: u64,
}

/// Either one Program texture (no project outputs) or every project output.
pub(crate) enum NativeVideoFrame {
    Program(NativeTexture),
    Outputs(Vec<NativeOutputFrame>),
}

/// A compose's reply: the registries come back with the pictures.
pub(crate) struct VideoRenderOutcome {
    pub playback: NativeSourcePlayback,
    pub stingers: NativeSourcePlayback,
    pub result: Result<NativeVideoFrame, VideoError>,
}

/// A Preview composite's reply.
pub(crate) struct PreviewRenderOutcome {
    pub playback: NativeSourcePlayback,
    pub stingers: NativeSourcePlayback,
    pub result: Result<NativeTexture, VideoError>,
}

/// Why a realizer compose failed.
#[derive(Debug)]
pub(crate) enum VideoError {
    /// The grade upload failed.
    Grades(NativeMediaError),
    /// The compositor failed.
    Render(NativeSourceRenderError),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grades(error) => error.fmt(formatter),
            Self::Render(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for VideoError {}

enum VideoCommand {
    Render {
        job: Box<VideoRenderJob>,
        reply: SyncSender<VideoRenderOutcome>,
    },
    Preview {
        job: Box<VideoRenderJob>,
        reply: SyncSender<PreviewRenderOutcome>,
    },
    Shutdown,
}

/// The control side's handle to the video realizer thread.
pub(crate) struct NativeVideoHandle {
    commands: SyncSender<VideoCommand>,
    pending: Option<Receiver<VideoRenderOutcome>>,
    pending_preview: Option<Receiver<PreviewRenderOutcome>>,
    thread: Option<JoinHandle<()>>,
}

impl NativeVideoHandle {
    /// Spawns the compose thread on `runtime`.
    ///
    /// # Errors
    ///
    /// Returns the thread-spawn failure.
    pub(crate) fn spawn(runtime: Arc<NativeMediaRuntime>, name: &str) -> std::io::Result<Self> {
        let (commands, receiver) = sync_channel(2);
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || video_thread(&runtime, &receiver))?;
        Ok(Self {
            commands,
            pending: None,
            pending_preview: None,
            thread: Some(thread),
        })
    }

    /// Submits one Program compose.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping or a compose already submitted.
    pub(crate) fn submit(&mut self, job: VideoRenderJob) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("a previous video compose was not collected".to_owned());
        }
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::Render {
                job: Box::new(job),
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
        self.pending = Some(answer);
        Ok(())
    }

    /// Waits for the submitted Program compose.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping; a compose failure is in the
    /// outcome, so the caller can restore the registries.
    pub(crate) fn collect(&mut self) -> Result<VideoRenderOutcome, String> {
        let answer = self
            .pending
            .take()
            .ok_or_else(|| "no video compose was submitted".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Submits one Preview composite.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping or a Preview already submitted.
    pub(crate) fn submit_preview(&mut self, job: VideoRenderJob) -> Result<(), String> {
        if self.pending_preview.is_some() {
            return Err("a previous Preview was not collected".to_owned());
        }
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::Preview {
                job: Box::new(job),
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
        self.pending_preview = Some(answer);
        Ok(())
    }

    /// Waits for the submitted Preview composite.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping; a compose failure is in the
    /// outcome, so the caller can restore the registries.
    pub(crate) fn collect_preview(&mut self) -> Result<PreviewRenderOutcome, String> {
        let answer = self
            .pending_preview
            .take()
            .ok_or_else(|| "no Preview was submitted".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }
}

impl Drop for NativeVideoHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(VideoCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The realizer's prepared grades, cached by the engine's compiled revision.
struct PreparedGrades(Option<(Arc<CompiledProjectGrades>, NativeProjectGrades)>);

impl PreparedGrades {
    fn apply(
        &mut self,
        runtime: &NativeMediaRuntime,
        plan: &mut NativeProjectPlan,
        revision: u64,
        compiled: &Arc<CompiledProjectGrades>,
    ) -> Result<(), NativeMediaError> {
        let prepared = match &self.0 {
            Some((cached, prepared)) if Arc::ptr_eq(cached, compiled) => prepared.clone(),
            _ => {
                let prepared = runtime.prepare_project_grades_blocking(revision, compiled)?;
                self.0 = Some((Arc::clone(compiled), prepared.clone()));
                prepared
            }
        };
        if *plan.grades() != prepared {
            plan.set_grades(prepared);
        }
        Ok(())
    }
}

fn video_thread(runtime: &NativeMediaRuntime, commands: &Receiver<VideoCommand>) {
    let mut grades = PreparedGrades(None);
    for command in commands {
        match command {
            VideoCommand::Render { job, reply } => {
                let mut job = *job;
                let result = grades
                    .apply(runtime, &mut job.plan, job.grade_revision, &job.grades)
                    .map_err(VideoError::Grades)
                    .and_then(|()| {
                        if job.plan.output_ids().is_empty() {
                            runtime
                                .render_project_frame_result_with_stingers_blocking(
                                    job.playback.registry(),
                                    job.stingers.registry(),
                                    &job.plan,
                                    &job.frame,
                                )
                                .map(NativeVideoFrame::Program)
                                .map_err(VideoError::Render)
                        } else {
                            runtime
                                .render_project_outputs_with_stingers_blocking(
                                    job.playback.registry(),
                                    job.stingers.registry(),
                                    &job.plan,
                                    &job.frame,
                                )
                                .map(NativeVideoFrame::Outputs)
                                .map_err(VideoError::Render)
                        }
                    });
                let _ = reply.send(VideoRenderOutcome {
                    playback: job.playback,
                    stingers: job.stingers,
                    result,
                });
            }
            VideoCommand::Preview { job, reply } => {
                let mut job = *job;
                let result = grades
                    .apply(runtime, &mut job.plan, job.grade_revision, &job.grades)
                    .map_err(VideoError::Grades)
                    .and_then(|()| {
                        runtime
                            .render_project_frame_result_with_stingers_blocking(
                                job.playback.registry(),
                                job.stingers.registry(),
                                &job.plan,
                                &job.frame,
                            )
                            .map_err(VideoError::Render)
                    });
                let _ = reply.send(PreviewRenderOutcome {
                    playback: job.playback,
                    stingers: job.stingers,
                    result,
                });
            }
            VideoCommand::Shutdown => break,
        }
    }
}
