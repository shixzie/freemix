//! The native realizer's video compose handoff (structural item 1).
//!
//! The control pass loop keeps `fm-control`, peers, the journal, and the
//! durable project. [`NativeVideoHandle`] runs GPU work on a dedicated
//! realizer thread that owns the source playback and stinger registries, the
//! slideshow cache, and the replay decode players. Control sends source
//! frames, coverage waits, registry queries, replay presentations, and
//! Program/Preview jobs over one bounded ordered channel; the worker ingests
//! and composes, and returns pictures, recorded audio, latest CPU frames, and
//! counters. Grade uploads also happen here, cached by the engine's compiled
//! revision, so Program and Preview share one prepared set.
//!
//! The runtime itself is shared as an `Arc<NativeMediaRuntime>` for now:
//! monitor taps and display presentation still use it on the control thread.
//! Moving those to the realizer is the next stage.

use std::{
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use fm_clock::ClockTime;
use fm_engine::{CompiledProjectGrades, EngineSlideshowFrame, FrameResult};
use fm_frame::{AudioBlock, CpuVideoFrame};
use fm_gpu::NativeTexture;
use fm_types::{InputId, OutputScaling};
use freemixd::native_media::{
    NativeMediaError, NativeMediaRuntime, NativeOutputFrame, NativeProjectGrades,
    NativeProjectPlan, NativeSourceLimits, NativeSourcePlayback, NativeSourceRenderError,
};

use crate::native_replay::{ReplayFormat, ReplayPlayers};
use crate::native_slides::NativeSlides;

/// One compose's inputs.
pub(crate) struct VideoRenderJob {
    pub frame: FrameResult,
    pub plan: NativeProjectPlan,
    /// The engine revision's compiled grades; the worker uploads them once
    /// per revision.
    pub grades: Arc<CompiledProjectGrades>,
    pub grade_revision: u64,
    /// The projected frame's slideshow slides to compose before the render
    /// (IN-009); the worker owns the slide cache.
    pub slides: Option<SlideshowJob>,
}

/// A slideshow's projected slides and the next ones to decode ahead.
pub(crate) struct SlideshowJob {
    pub frames: Vec<(EngineSlideshowFrame, OutputScaling)>,
    pub ahead: Vec<(Arc<str>, OutputScaling)>,
}

/// Either one Program texture (no project outputs) or every project output.
pub(crate) enum NativeVideoFrame {
    Program(NativeTexture),
    Outputs(Vec<NativeOutputFrame>),
}

/// A compose's reply.
pub(crate) struct VideoRenderOutcome {
    pub result: Result<NativeVideoFrame, VideoError>,
}

/// A Preview composite's reply.
pub(crate) struct PreviewRenderOutcome {
    pub result: Result<NativeTexture, VideoError>,
}

/// A replay presentation's reply: the recorded audio to push before the
/// audio render, and whether every player is covered.
pub(crate) struct PresentReplayOutcome {
    pub audio: Vec<(InputId, AudioBlock)>,
    pub covered: bool,
}

/// What the playback registry holds right now.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlaybackBudget {
    pub limit: NativeSourceLimits,
    pub retained_bytes: u64,
}

/// One replay presentation's inputs.
pub(crate) struct PresentReplayJob {
    pub frame: FrameResult,
    pub deadline: Instant,
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
    PresentReplay {
        job: Box<PresentReplayJob>,
        reply: SyncSender<PresentReplayOutcome>,
    },
    Ingest {
        input: InputId,
        frame: CpuVideoFrame,
        reply: SyncSender<Result<(), String>>,
    },
    Service {
        deadline: ClockTime,
        reply: SyncSender<Result<bool, String>>,
    },
    ServiceStinger {
        input: InputId,
        deadline: ClockTime,
        reply: SyncSender<Result<bool, String>>,
    },
    PlaybackBudget {
        reply: SyncSender<PlaybackBudget>,
    },
    InsertLiveSource {
        input: InputId,
        seed: CpuVideoFrame,
        reply: SyncSender<Result<(), String>>,
    },
    RemoveSource {
        input: InputId,
    },
    SetRetainedByteLimit {
        limit: u64,
    },
    ValidateRetainedByteLimit {
        limit: u64,
        reply: SyncSender<bool>,
    },
    ReplaceStingers {
        stingers: NativeSourcePlayback,
        reply: SyncSender<NativeSourcePlayback>,
    },
    ForgetSlide {
        input: InputId,
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
    /// Spawns the realizer thread, moving the playback and stinger registries
    /// and the slideshow and replay state onto it.
    ///
    /// # Errors
    ///
    /// Returns the thread-spawn failure.
    pub(crate) fn spawn(
        runtime: Arc<NativeMediaRuntime>,
        playback: NativeSourcePlayback,
        stingers: NativeSourcePlayback,
        slides: NativeSlides,
        replay: ReplayFormat,
        name: &str,
    ) -> std::io::Result<Self> {
        let (commands, receiver) = sync_channel(4);
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let mut realizer = VideoRealizer {
                    runtime,
                    playback,
                    stingers,
                    slides,
                    replay: ReplayPlayers::new(replay),
                    grades: PreparedGrades(None),
                };
                realizer.run(&receiver);
            })?;
        Ok(Self {
            commands,
            pending: None,
            pending_preview: None,
            thread: Some(thread),
        })
    }

    /// Ingests one live source picture.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed failure or its thread stopping.
    pub(crate) fn ingest_frame(
        &mut self,
        input: InputId,
        frame: CpuVideoFrame,
    ) -> Result<(), String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::Ingest {
                input,
                frame,
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())?
    }

    /// Waits for the frame's sources to be presented.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed failure or its thread stopping.
    pub(crate) fn service_playback(&mut self, deadline: ClockTime) -> Result<bool, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::Service { deadline, reply })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())?
    }

    /// Waits for one Stinger input's picture at `deadline`.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed failure or its thread stopping.
    pub(crate) fn service_stinger(
        &mut self,
        input: InputId,
        deadline: ClockTime,
    ) -> Result<bool, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::ServiceStinger {
                input,
                deadline,
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())?
    }

    /// The playback registry's current limits and retained bytes.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn playback_budget(&mut self) -> Result<PlaybackBudget, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::PlaybackBudget { reply })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Adds a live input's first picture to the registry.
    ///
    /// # Errors
    ///
    /// Returns the renderer's typed failure or its thread stopping.
    pub(crate) fn insert_live_source(
        &mut self,
        input: InputId,
        seed: CpuVideoFrame,
    ) -> Result<(), String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::InsertLiveSource { input, seed, reply })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())?
    }

    /// Drops a removed input's source.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn remove_source(&mut self, input: InputId) -> Result<(), String> {
        self.commands
            .send(VideoCommand::RemoveSource { input })
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Sets the registry's retained-byte limit.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn set_retained_byte_limit(&mut self, limit: u64) -> Result<(), String> {
        self.commands
            .send(VideoCommand::SetRetainedByteLimit { limit })
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Whether `limit` can retain the current registry.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn validate_retained_byte_limit(&mut self, limit: u64) -> Result<bool, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::ValidateRetainedByteLimit { limit, reply })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Replaces the stinger registry, returning the retired one.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn replace_stingers(
        &mut self,
        stingers: NativeSourcePlayback,
    ) -> Result<NativeSourcePlayback, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::ReplaceStingers { stingers, reply })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Presents every Replay input's recorded frame and returns its audio.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn present_replay(
        &mut self,
        frame: FrameResult,
        deadline: Instant,
    ) -> Result<PresentReplayOutcome, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::PresentReplay {
                job: Box::new(PresentReplayJob { frame, deadline }),
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the video thread stopped".to_owned())
    }

    /// Drops a removed input's uploaded slides.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping.
    pub(crate) fn forget_slide(&mut self, input: InputId) -> Result<(), String> {
        self.commands
            .send(VideoCommand::ForgetSlide { input })
            .map_err(|_| "the video thread stopped".to_owned())
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
    /// Returns the video thread stopping.
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
    /// Returns the video thread stopping.
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

/// The realizer-owned media state.
struct VideoRealizer {
    runtime: Arc<NativeMediaRuntime>,
    playback: NativeSourcePlayback,
    stingers: NativeSourcePlayback,
    slides: NativeSlides,
    replay: ReplayPlayers,
    grades: PreparedGrades,
}

impl VideoRealizer {
    #[allow(clippy::too_many_lines)] // one arm per realizer command
    fn run(&mut self, commands: &Receiver<VideoCommand>) {
        for command in commands {
            match command {
                VideoCommand::Render { job, reply } => {
                    let mut job = *job;
                    let result = self
                        .grades
                        .apply(
                            &self.runtime,
                            &mut job.plan,
                            job.grade_revision,
                            &job.grades,
                        )
                        .map_err(VideoError::Grades)
                        .and_then(|()| {
                            if let Some(slideshow) = job.slides.take() {
                                self.slides
                                    .present(
                                        &self.runtime,
                                        &mut self.playback,
                                        &slideshow.frames,
                                        &slideshow.ahead,
                                    )
                                    .map_err(VideoError::Render)?;
                            }
                            if job.plan.output_ids().is_empty() {
                                self.runtime
                                    .render_project_frame_result_with_stingers_blocking(
                                        self.playback.registry(),
                                        self.stingers.registry(),
                                        &job.plan,
                                        &job.frame,
                                    )
                                    .map(NativeVideoFrame::Program)
                                    .map_err(VideoError::Render)
                            } else {
                                self.runtime
                                    .render_project_outputs_with_stingers_blocking(
                                        self.playback.registry(),
                                        self.stingers.registry(),
                                        &job.plan,
                                        &job.frame,
                                    )
                                    .map(NativeVideoFrame::Outputs)
                                    .map_err(VideoError::Render)
                            }
                        });
                    let _ = reply.send(VideoRenderOutcome { result });
                }
                VideoCommand::Preview { job, reply } => {
                    let mut job = *job;
                    let result = self
                        .grades
                        .apply(
                            &self.runtime,
                            &mut job.plan,
                            job.grade_revision,
                            &job.grades,
                        )
                        .map_err(VideoError::Grades)
                        .and_then(|()| {
                            self.runtime
                                .render_project_frame_result_with_stingers_blocking(
                                    self.playback.registry(),
                                    self.stingers.registry(),
                                    &job.plan,
                                    &job.frame,
                                )
                                .map_err(VideoError::Render)
                        });
                    let _ = reply.send(PreviewRenderOutcome { result });
                }
                VideoCommand::PresentReplay { job, reply } => {
                    let mut audio = Vec::new();
                    let covered = self.replay.present(
                        &job.frame,
                        job.deadline,
                        &self.runtime,
                        &mut self.playback,
                        &mut |input, block| audio.push((input, block.clone())),
                    );
                    let _ = reply.send(PresentReplayOutcome { audio, covered });
                }
                VideoCommand::Ingest {
                    input,
                    frame,
                    reply,
                } => {
                    let answer = if self.playback.registry().contains(input) {
                        self.runtime
                            .ingest_live_video_frame_blocking(&mut self.playback, input, frame)
                            .map_err(|error| error.to_string())
                    } else {
                        Ok(())
                    };
                    let _ = reply.send(answer);
                }
                VideoCommand::Service { deadline, reply } => {
                    let answer = self
                        .runtime
                        .service_source_playback_blocking(&mut self.playback, deadline)
                        .map_err(|error| error.to_string());
                    let _ = reply.send(answer);
                }
                VideoCommand::ServiceStinger {
                    input,
                    deadline,
                    reply,
                } => {
                    let answer = self
                        .runtime
                        .service_source_playback_for_input_blocking(
                            &mut self.stingers,
                            input,
                            deadline,
                        )
                        .map_err(|error| error.to_string());
                    let _ = reply.send(answer);
                }
                VideoCommand::PlaybackBudget { reply } => {
                    let registry = self.playback.registry();
                    let _ = reply.send(PlaybackBudget {
                        limit: registry.limits(),
                        retained_bytes: registry.retained_rgba16f_bytes(),
                    });
                }
                VideoCommand::InsertLiveSource { input, seed, reply } => {
                    let answer = self
                        .runtime
                        .insert_live_source_blocking(&mut self.playback, input, seed)
                        .map_err(|error| error.to_string());
                    let _ = reply.send(answer);
                }
                VideoCommand::RemoveSource { input } => {
                    let _ = self.playback.remove_source(input);
                }
                VideoCommand::SetRetainedByteLimit { limit } => {
                    self.playback.set_retained_byte_limit(limit);
                }
                VideoCommand::ValidateRetainedByteLimit { limit, reply } => {
                    let _ = reply.send(self.playback.validate_retained_byte_limit(limit).is_ok());
                }
                VideoCommand::ReplaceStingers { stingers, reply } => {
                    let retired = std::mem::replace(&mut self.stingers, stingers);
                    let _ = reply.send(retired);
                }
                VideoCommand::ForgetSlide { input } => self.slides.forget(input),
                VideoCommand::Shutdown => break,
            }
        }
    }
}
