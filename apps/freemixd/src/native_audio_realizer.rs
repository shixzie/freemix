//! The native realizer's audio handoff (structural item 1).
//!
//! The control pass loop keeps `fm-control`, peers, the journal, and the
//! durable project. [`NativeAudioHandle`] moves the `NativeMasterRuntime`
//! (the audio mixer, its decoders, plugin racks, meters, and stinger playback)
//! to one dedicated thread. A frame submits its audio job before the GPU
//! compose and collects the outcome after it, so mixing overlaps composing;
//! live pushed audio and the pre-frame coverage wait travel the same ordered
//! channel, so the mixer sees every frame's input in the order the engine
//! realized it.
//!
//! The channel is bounded: pushes and jobs are one ordered stream, and a
//! render has exactly one outstanding reply. An audio failure is sticky and
//! returned by the next coverage wait or render, as the in-thread mixer's
//! `failed` flag was.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

use fm_audio::{ChannelMeter, DspDynamicsReading, LoudnessReading};
use fm_engine::FrameResult;
use fm_frame::AudioBlock;
use fm_sim::SinkTelemetry;
use fm_types::{InputId, OutputId};
use freemixd::native_media::{
    NativeAudioMeters, NativeAudioTelemetry, NativeMasterRuntime, NativeProjectPlan,
    NativeStingerAudioRuntime,
};

/// Commands one audio thread carries, in order.
enum AudioCommand {
    /// A live source's block for `frame`.
    Push {
        input: InputId,
        frame: u64,
        block: AudioBlock,
    },
    /// Waits for the frame's audio sources to be covered before realization.
    Service {
        frame: FrameResult,
        plan: NativeProjectPlan,
        reply: SyncSender<Result<bool, String>>,
    },
    /// Mixes one frame, applying its strip and mixer updates first.
    Render {
        job: AudioRenderJob,
        reply: SyncSender<Result<AudioRenderOutcome, String>>,
    },
    /// Applies a live catalogue change to the mixer's sources.
    FollowCatalogue {
        plan: NativeProjectPlan,
        replaced: BTreeSet<InputId>,
        pushed: Vec<InputId>,
        reply: SyncSender<Result<(), String>>,
    },
    /// Installs a Stinger's audio and returns the retired one.
    ReplaceStingerAudio {
        replacement: NativeStingerAudioRuntime,
        reply: SyncSender<NativeStingerAudioRuntime>,
    },
    /// Which inputs have a decoded audio source.
    DecodedInputs {
        reply: SyncSender<BTreeSet<InputId>>,
    },
    Shutdown,
}

/// One frame's audio work: the engine frame, the plan after its strip and
/// mixer updates, those updates, and the inputs whose rendered audio the
/// replay recorder takes.
pub(crate) struct AudioRenderJob {
    pub frame: FrameResult,
    pub plan: NativeProjectPlan,
    pub strip_updates: Vec<(InputId, fm_engine::EngineInputAudioStripState)>,
    pub mixer_updates: Vec<fm_engine::EngineAudioMixerUpdate>,
    pub replay_inputs: Vec<InputId>,
}

/// The audio thread's owned copy of one frame's meters.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OwnedAudioMeters {
    pub frame: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub channels: usize,
    pub master: Vec<ChannelMeter>,
    pub inputs: Vec<InputId>,
    pub input_meters: Vec<ChannelMeter>,
    pub monitor: Vec<ChannelMeter>,
    pub monitor_solo: bool,
    pub monitor_planes: Vec<Vec<f32>>,
    pub sample_rate: u32,
    pub dynamics: Vec<(Option<InputId>, DspDynamicsReading)>,
    pub loudness: Vec<(Option<InputId>, LoudnessReading)>,
    pub aux: Vec<ChannelMeter>,
    /// Every aux bus's samples, bus-major (AU-002, Protocol 2.135).
    pub aux_planes: Vec<Vec<f32>>,
}

/// One frame's mixed audio and the status the control thread publishes.
pub(crate) struct AudioRenderOutcome {
    pub block: AudioBlock,
    pub live: Option<AudioBlock>,
    pub overrides: Vec<(OutputId, AudioBlock)>,
    pub meters: Option<OwnedAudioMeters>,
    /// Rendered audio of every `replay_inputs` entry that has any (OR-012).
    pub rendered: BTreeMap<InputId, Vec<Vec<f32>>>,
}

/// The mixer's retention, sink, and telemetry counters after a command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AudioSnapshot {
    pub telemetry: NativeAudioTelemetry,
    pub retained_blocks: usize,
    pub retained_samples: usize,
    pub retained_bytes: usize,
    pub sink_depth: usize,
    pub sink: SinkTelemetry,
}

impl AudioSnapshot {
    fn of(master: &NativeMasterRuntime) -> Self {
        Self {
            telemetry: master.audio_telemetry(),
            retained_blocks: master.retained_blocks(),
            retained_samples: master.retained_samples(),
            retained_bytes: master.retained_bytes(),
            sink_depth: master.sink_len(),
            sink: master.sink_telemetry(),
        }
    }
}

/// The control side's handle to the audio thread.
pub(crate) struct NativeAudioHandle {
    commands: SyncSender<AudioCommand>,
    /// The one outstanding render reply, once its job is submitted.
    pending_render: Option<Receiver<Result<AudioRenderOutcome, String>>>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
    thread: Option<JoinHandle<()>>,
}

impl NativeAudioHandle {
    /// Moves `master` to a dedicated thread named `name`.
    ///
    /// # Errors
    ///
    /// Returns the thread-spawn failure.
    pub(crate) fn spawn(master: NativeMasterRuntime, name: &str) -> std::io::Result<Self> {
        // Pushes of one frame's live sources plus the frame's service and
        // render; a slow mixer applies backpressure to the sender instead of
        // dropping audio.
        const QUEUE: usize = 512;
        let (commands, receiver) = sync_channel(QUEUE);
        let snapshot = Arc::new(Mutex::new(AudioSnapshot::of(&master)));
        let worker_snapshot = Arc::clone(&snapshot);
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || audio_thread(master, receiver, &worker_snapshot))?;
        Ok(Self {
            commands,
            pending_render: None,
            snapshot,
            thread: Some(thread),
        })
    }

    /// The latest counters, without waiting for the mixer.
    pub(crate) fn snapshot(&self) -> AudioSnapshot {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies a live catalogue change to the mixer's sources.
    ///
    /// # Errors
    ///
    /// Returns the mixer's typed failure or its thread stopping.
    pub(crate) fn follow_catalogue(
        &mut self,
        plan: &NativeProjectPlan,
        replaced: &BTreeSet<InputId>,
        pushed: &[InputId],
    ) -> Result<(), String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(AudioCommand::FollowCatalogue {
                plan: plan.clone(),
                replaced: replaced.clone(),
                pushed: pushed.to_vec(),
                reply,
            })
            .map_err(|_| "the audio thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the audio thread stopped".to_owned())?
    }

    /// Installs a Stinger's audio, returning the retired runtime.
    ///
    /// # Errors
    ///
    /// Returns the audio thread stopping.
    pub(crate) fn replace_stinger_audio(
        &mut self,
        replacement: NativeStingerAudioRuntime,
    ) -> Result<NativeStingerAudioRuntime, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(AudioCommand::ReplaceStingerAudio { replacement, reply })
            .map_err(|_| "the audio thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the audio thread stopped".to_owned())
    }

    /// Every input the mixer currently decodes.
    ///
    /// # Errors
    ///
    /// Returns the audio thread stopping.
    pub(crate) fn decoded_inputs(&self) -> Result<BTreeSet<InputId>, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(AudioCommand::DecodedInputs { reply })
            .map_err(|_| "the audio thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the audio thread stopped".to_owned())
    }

    /// Queues one live source's block for `frame`.
    ///
    /// # Errors
    ///
    /// Returns the mixer thread's disconnect.
    pub(crate) fn push_frame_audio(
        &mut self,
        input: InputId,
        frame: u64,
        block: &AudioBlock,
    ) -> Result<(), String> {
        self.commands
            .send(AudioCommand::Push {
                input,
                frame,
                block: block.clone(),
            })
            .map_err(|_| "the audio thread stopped".to_owned())
    }

    /// Waits until `frame`'s audio sources are covered.
    ///
    /// # Errors
    ///
    /// Returns the mixer's typed failure or its thread stopping.
    pub(crate) fn service_next_frame(
        &mut self,
        frame: &FrameResult,
        plan: &NativeProjectPlan,
    ) -> Result<bool, String> {
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(AudioCommand::Service {
                frame: frame.clone(),
                plan: plan.clone(),
                reply,
            })
            .map_err(|_| "the audio thread stopped".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the audio thread stopped".to_owned())?
    }

    /// Submits one frame's mix; [`Self::collect_render`] takes its outcome.
    ///
    /// # Errors
    ///
    /// Returns the audio thread stopping or a render already outstanding.
    pub(crate) fn submit_render(&mut self, job: AudioRenderJob) -> Result<(), String> {
        if self.pending_render.is_some() {
            return Err("a previous audio render was not collected".to_owned());
        }
        let (reply, answer) = sync_channel(0);
        self.commands
            .send(AudioCommand::Render { job, reply })
            .map_err(|_| "the audio thread stopped".to_owned())?;
        self.pending_render = Some(answer);
        Ok(())
    }

    /// Waits for the submitted frame's audio.
    ///
    /// # Errors
    ///
    /// Returns the mixer's typed failure, its thread stopping, or no render
    /// outstanding.
    pub(crate) fn collect_render(&mut self) -> Result<AudioRenderOutcome, String> {
        let answer = self
            .pending_render
            .take()
            .ok_or_else(|| "no audio render was submitted".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the audio thread stopped".to_owned())?
    }
}

impl Drop for NativeAudioHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(AudioCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn audio_thread(
    mut master: NativeMasterRuntime,
    commands: Receiver<AudioCommand>,
    snapshot: &Arc<Mutex<AudioSnapshot>>,
) {
    let mut failed: Option<String> = None;
    for command in commands {
        match command {
            AudioCommand::Push {
                input,
                frame,
                block,
            } => {
                if failed.is_none()
                    && let Err(error) = master.push_frame_audio(input, frame, &block)
                {
                    failed = Some(error.to_string());
                }
            }
            AudioCommand::Service { frame, plan, reply } => {
                let answer = match &failed {
                    Some(error) => Err(error.clone()),
                    None => master
                        .service_project_next_frame(&frame, &plan)
                        .map_err(|error| {
                            let error = error.to_string();
                            failed = Some(error.clone());
                            error
                        }),
                };
                publish_snapshot(&master, snapshot);
                let _ = reply.send(answer);
            }
            AudioCommand::Render { job, reply } => {
                let answer = match &failed {
                    Some(error) => Err(error.clone()),
                    None => render(&mut master, job).map_err(|error| {
                        let error = error.to_string();
                        failed = Some(error.clone());
                        error
                    }),
                };
                publish_snapshot(&master, snapshot);
                let _ = reply.send(answer);
            }
            AudioCommand::FollowCatalogue {
                plan,
                replaced,
                pushed,
                reply,
            } => {
                let answer = match &failed {
                    Some(error) => Err(error.clone()),
                    None => master
                        .follow_catalogue(&plan, &replaced, &pushed)
                        .map_err(|error| {
                            let error = error.to_string();
                            failed = Some(error.clone());
                            error
                        }),
                };
                publish_snapshot(&master, snapshot);
                let _ = reply.send(answer);
            }
            AudioCommand::ReplaceStingerAudio { replacement, reply } => {
                let retired = master.replace_stinger_audio(replacement);
                publish_snapshot(&master, snapshot);
                let _ = reply.send(retired);
            }
            AudioCommand::DecodedInputs { reply } => {
                let _ = reply.send(master.decoded_inputs().collect());
            }
            AudioCommand::Shutdown => break,
        }
    }
}

fn render(
    master: &mut NativeMasterRuntime,
    job: AudioRenderJob,
) -> Result<AudioRenderOutcome, freemixd::native_media::NativeMasterError> {
    for (input, state) in &job.strip_updates {
        master.set_input_audio_strip(*input, crate::model_audio_strip_state(*state))?;
    }
    for update in &job.mixer_updates {
        master.apply_audio_mixer_update(update)?;
    }
    let block = master.render_project_frame_audio(&job.frame, &job.plan)?;
    let live = master.take_live_audio();
    let overrides = master.take_override_audio();
    let meters = master
        .take_audio_meters()
        .map(|meters| owned_meters(&meters));
    let rendered = job
        .replay_inputs
        .into_iter()
        .filter_map(|input| {
            master
                .rendered_input_audio(input)
                .map(|planes| (input, planes))
        })
        .collect();
    Ok(AudioRenderOutcome {
        block,
        live,
        overrides,
        meters,
        rendered,
    })
}

fn owned_meters(meters: &NativeAudioMeters<'_>) -> OwnedAudioMeters {
    OwnedAudioMeters {
        frame: meters.frame,
        start_sample: meters.start_sample,
        end_sample: meters.end_sample,
        channels: meters.channels,
        master: meters.master.to_vec(),
        inputs: meters.inputs.to_vec(),
        input_meters: meters.input_meters.to_vec(),
        monitor: meters.monitor.to_vec(),
        monitor_solo: meters.monitor_solo,
        monitor_planes: meters.monitor_planes.to_vec(),
        sample_rate: meters.sample_rate,
        dynamics: meters.dynamics.to_vec(),
        loudness: meters.loudness.to_vec(),
        aux: meters.aux.to_vec(),
        aux_planes: meters.aux_planes.to_vec(),
    }
}

fn publish_snapshot(master: &NativeMasterRuntime, snapshot: &Arc<Mutex<AudioSnapshot>>) {
    *snapshot.lock().unwrap_or_else(PoisonError::into_inner) = AudioSnapshot::of(master);
}
