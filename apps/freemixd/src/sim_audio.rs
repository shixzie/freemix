//! The simulated daemon's audio render plane (AU-005).
//!
//! The native daemon renders its Program bus with [`fm_audio::MasterMixer`],
//! applying the persisted per-strip and Master DSP chains, and publishes the
//! meters of every rendered frame. The simulated daemon has no decoders, so
//! this runtime drives the deterministic [`fm_sim::SimulatedAudioPlane`] with
//! the same persisted configuration instead: one simulated tone (or silence)
//! per input, the project's strip faders, pans, delays, groups, solo mode,
//! and DSP chains, and the Master chain.
//!
//! The simulated engine renders no idle frames, so the audio plane keeps its
//! own wall clock. The clock is anchored at the durable position's rendered
//! frame when the daemon starts, and [`SimulatedAudioRuntime::poll`] renders
//! every frame that clock has reached while a peer demands meters. Meter
//! demand never advances the engine, writes the journal, or realizes a
//! command: it only renders this synthetic audio plane.

use std::{collections::VecDeque, num::NonZeroU128, time::Instant};

use fm_frame::ClockDomainId;
use fm_model::{AudioGroupNumber, InputKind, Project, SimulatedAudio};
use fm_sim::{
    SimulatedAudioConfig, SimulatedAudioError, SimulatedAudioMeters, SimulatedAudioPlane,
    SimulatedAudioStrip, SimulatedGroupState, SimulatedStripState, SimulatedTone,
};
use fm_switcher::ProgramFrame;
use fm_types::strip_dsp::StripDspTarget;

/// The most show frames one poll renders before re-anchoring the wall clock.
///
/// A peer that starts demanding meters after a long idle period is caught up
/// at four frames per pass; the clock is then re-anchored at the frame that
/// was reached, so the backlog never grows without bound.
const MAX_FRAMES_PER_POLL: u64 = 4;

/// The most loudness resets held for the next render. The control service
/// bounds its own queue the same way; this only bounds the interval between
/// taking them and rendering the frame they apply to.
const MAX_PENDING_RESETS: usize = 64;

/// The simulated daemon's audio render plane and wall clock.
pub struct SimulatedAudioRuntime {
    plane: SimulatedAudioPlane,
    /// The project revision `plane` reflects, when one was applied.
    revision: Option<u64>,
    format: fm_types::AudioFormat,
    frame_rate: fm_types::FrameRate,
    /// The next show frame the wall clock renders.
    next_frame: u64,
    /// The frame and instant the wall clock was last anchored at.
    anchor_frame: u64,
    anchor_at: Instant,
    /// Loudness resets realized by the engine, applied to the next render.
    pending_resets: VecDeque<StripDspTarget>,
}

impl SimulatedAudioRuntime {
    /// Builds the plane for `project`, anchored at `frames_rendered` (the
    /// durable position's rendered frame).
    ///
    /// # Errors
    ///
    /// Returns a typed configuration error when the project's audio format,
    /// frame rate, strips, groups, chains, or tones are unusable.
    pub fn new(project: &Project, frames_rendered: u64) -> Result<Self, SimulatedAudioError> {
        let config = config_from_project(project);
        let format = config.format.clone();
        let frame_rate = config.frame_rate;
        let plane = SimulatedAudioPlane::new(config, clock_domain())?;
        Ok(Self {
            plane,
            revision: None,
            format,
            frame_rate,
            next_frame: frames_rendered,
            anchor_frame: frames_rendered,
            anchor_at: Instant::now(),
            pending_resets: VecDeque::new(),
        })
    }

    /// Renders every show frame the wall clock has reached while `demand`
    /// holds, and returns the last rendered frame's meters.
    ///
    /// At most [`MAX_FRAMES_PER_POLL`] frames render per call. A poll that
    /// reaches the cap while still behind re-anchors the clock at the frame
    /// it reached, so a long backlog is worked off at the show's pace rather
    /// than rendered in one burst. `resets` restarts the integrated loudness
    /// of the named buses before the next rendered frame; they are held
    /// until a frame actually renders.
    ///
    /// A poll without demand renders nothing and returns `None`; it never
    /// advances the engine.
    pub fn poll<'a>(
        &'a mut self,
        project: &Project,
        revision: u64,
        program_frame: ProgramFrame,
        resets: Vec<StripDspTarget>,
        demand: bool,
        now: Instant,
    ) -> Option<SimulatedAudioMeters<'a>> {
        for target in resets {
            if self.pending_resets.len() == MAX_PENDING_RESETS {
                self.pending_resets.pop_front();
            }
            self.pending_resets.push_back(target);
        }
        if !demand {
            return None;
        }
        if self.sync(project, revision).is_err() {
            return None;
        }
        let target = crate::playlist_clock::wall_frame(
            self.anchor_frame,
            self.anchor_at,
            now,
            self.frame_rate,
        );
        let mut rendered = 0_u64;
        let mut rendered_any = false;
        let mut resets_applied = false;
        while self.next_frame < target && rendered < MAX_FRAMES_PER_POLL {
            let frame = self.next_frame;
            let resets: Vec<StripDspTarget> = if resets_applied {
                Vec::new()
            } else {
                self.pending_resets.iter().copied().collect()
            };
            if self.plane.render(program_frame, frame, &resets).is_ok() {
                rendered_any = true;
                if !resets_applied {
                    self.pending_resets.clear();
                    resets_applied = true;
                }
            }
            self.next_frame = frame.saturating_add(1);
            rendered += 1;
        }
        if self.next_frame < target {
            // Still behind after a full pass: anchor at what was reached so
            // the remaining backlog is paced by the wall clock.
            self.anchor_frame = self.next_frame;
            self.anchor_at = now;
        }
        rendered_any.then(|| self.plane.take_meters())
    }

    /// Re-applies `project` when its revision changed.
    fn sync(&mut self, project: &Project, revision: u64) -> Result<(), SimulatedAudioError> {
        if self.revision == Some(revision) {
            return Ok(());
        }
        let config = config_from_project(project);
        if self.plane.config() == &config {
            self.revision = Some(revision);
            return Ok(());
        }
        if config.format == self.format && config.frame_rate == self.frame_rate {
            self.plane.resync(config)?;
        } else {
            // A project format change cannot be applied to the running
            // mixer; start a fresh plane on the same wall clock.
            self.plane = SimulatedAudioPlane::new(config.clone(), clock_domain())?;
            self.format = config.format;
            self.frame_rate = config.frame_rate;
        }
        self.revision = Some(revision);
        Ok(())
    }
}

/// The plane configuration of one project revision.
///
/// Every input strip is mapped: its fader, balance, pan, delay, mute, solo,
/// and follow-video state; its persisted DSP chain; its group; and, for a
/// simulated input with a sine, a tone at that frequency. Non-simulated
/// inputs are silent, as the simulated daemon has no decoder for them.
fn config_from_project(project: &Project) -> SimulatedAudioConfig {
    let settings = project.settings();
    let mut config = SimulatedAudioConfig::new(settings.audio.clone(), settings.frame_rate);
    config.master_dsp = *project.master_audio_dsp();
    config.solo_mode = solo_mode(project.audio_solo_mode());
    config.groups = std::array::from_fn(|index| {
        let group = project.audio_groups().get(AudioGroupNumber::ALL[index]);
        SimulatedGroupState {
            gain_millidb: group.gain.get(),
            muted: group.muted,
            soloed: group.soloed,
            parent: group.parent.map(AudioGroupNumber::get),
        }
    });
    config.strips = project
        .input_audio_strips()
        .iter()
        .map(|strip| SimulatedAudioStrip {
            input: strip.input,
            state: SimulatedStripState {
                gain_millidb: strip.state.gain.get(),
                balance_basis_points: strip.state.balance.get(),
                pan_basis_points: strip.state.pan.get(),
                delay_samples: strip.state.delay_samples.get(),
                muted: strip.state.muted,
                soloed: strip.state.soloed,
                follow_video: strip.state.follow_video,
            },
            dsp: strip.dsp,
            tone: tone(project, strip.input),
            group: strip.group.map(AudioGroupNumber::get),
        })
        .collect();
    config
}

/// The simulated tone `input` plays, or `None` for silence.
#[allow(clippy::cast_precision_loss)] // sine frequencies are bounded to 20 000 Hz
fn tone(project: &Project, input: fm_types::InputId) -> Option<SimulatedTone> {
    let source = project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)?;
    let InputKind::Simulated(simulated) = &source.kind else {
        return None;
    };
    match simulated.audio {
        SimulatedAudio::Sine { frequency_hz } => Some(SimulatedTone::new(frequency_hz as f32)),
        SimulatedAudio::Silence => None,
    }
}

/// The mixer solo mode for a model solo mode (AU-001).
const fn solo_mode(mode: fm_model::AudioSoloMode) -> fm_audio::SoloMode {
    match mode {
        fm_model::AudioSoloMode::InPlace => fm_audio::SoloMode::InPlace,
        fm_model::AudioSoloMode::Pfl => fm_audio::SoloMode::Pfl,
        fm_model::AudioSoloMode::Afl => fm_audio::SoloMode::Afl,
    }
}

/// The audio plane's clock domain: the simulated daemon's one Program clock.
fn clock_domain() -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::MIN)
}
