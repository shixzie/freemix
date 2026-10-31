//! The native realizer's video compose handoff (structural item 1).
//!
//! The control pass loop keeps `fm-control`, peers, the journal, and the
//! durable project. [`NativeVideoHandle`] runs the GPU compose on a dedicated
//! realizer thread. A job carries the engine frame, the plan, and the source
//! playback registries; the reply returns them with the output textures, so
//! the control thread keeps ingesting sources and presenting outputs while
//! the compose runs off its thread. The channel is bounded and carries one
//! job at a time; a compose is collected before the next submission.
//!
//! The runtime itself is shared as an `Arc<NativeMediaRuntime>` for now:
//! source ingestion, grade uploads, monitor taps, and display presentation
//! still use it on the control thread. Moving those to the realizer is the
//! next stage; this one takes the Program/Preview compose off the control
//! thread and returns every texture it makes.

use std::sync::{
    Arc,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::thread::{self, JoinHandle};

use fm_engine::FrameResult;
use fm_gpu::NativeTexture;
use freemixd::native_media::{
    NativeMediaRuntime, NativeOutputFrame, NativeProjectPlan, NativeSourcePlayback,
    NativeSourceRenderError,
};

/// One compose's inputs.
pub(crate) struct VideoRenderJob {
    pub frame: FrameResult,
    pub plan: NativeProjectPlan,
    pub playback: NativeSourcePlayback,
    pub stingers: NativeSourcePlayback,
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
    pub result: Result<NativeVideoFrame, NativeSourceRenderError>,
}

enum VideoCommand {
    Render {
        job: Box<VideoRenderJob>,
        reply: SyncSender<VideoRenderOutcome>,
    },
    Shutdown,
}

/// The control side's handle to the video realizer thread.
pub(crate) struct NativeVideoHandle {
    commands: SyncSender<VideoCommand>,
    thread: Option<JoinHandle<()>>,
}

impl NativeVideoHandle {
    /// Spawns the compose thread on `runtime`.
    ///
    /// # Errors
    ///
    /// Returns the thread-spawn failure.
    pub(crate) fn spawn(runtime: Arc<NativeMediaRuntime>, name: &str) -> std::io::Result<Self> {
        let (commands, receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || video_thread(&runtime, &receiver))?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    /// Composes one job and waits for its pictures.
    ///
    /// # Errors
    ///
    /// Returns the video thread stopping; a compose failure is in the
    /// outcome, so the caller can restore the registries.
    pub(crate) fn render(&mut self, job: VideoRenderJob) -> Result<VideoRenderOutcome, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(VideoCommand::Render {
                job: Box::new(job),
                reply,
            })
            .map_err(|_| "the video thread stopped".to_owned())?;
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

fn video_thread(runtime: &NativeMediaRuntime, commands: &Receiver<VideoCommand>) {
    for command in commands {
        match command {
            VideoCommand::Render { job, reply } => {
                let job = *job;
                let result = if job.plan.output_ids().is_empty() {
                    runtime
                        .render_project_frame_result_with_stingers_blocking(
                            job.playback.registry(),
                            job.stingers.registry(),
                            &job.plan,
                            &job.frame,
                        )
                        .map(NativeVideoFrame::Program)
                } else {
                    runtime
                        .render_project_outputs_with_stingers_blocking(
                            job.playback.registry(),
                            job.stingers.registry(),
                            &job.plan,
                            &job.frame,
                        )
                        .map(NativeVideoFrame::Outputs)
                };
                let _ = reply.send(VideoRenderOutcome {
                    playback: job.playback,
                    stingers: job.stingers,
                    result,
                });
            }
            VideoCommand::Shutdown => break,
        }
    }
}
