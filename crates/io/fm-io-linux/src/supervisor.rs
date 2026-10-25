//! The per-input capture state machine: device resolution, format
//! negotiation, acquisition, signal loss, and recovery under one input ID.
//!
//! A [`CaptureSupervisor`] follows one capture input. Each program frame
//! the owner calls [`CaptureSupervisor::update`] with the latest device
//! enumeration and then [`CaptureSupervisor::pull`] for the frame's
//! picture and audio. The supervisor never blocks.
//!
//! - **Resolution.** The input's stable key is resolved against the
//!   enumeration ([`crate::probe::resolve`]): the exact key, else the same
//!   card on another port, else a node path.
//! - **Negotiation.** The format opened is
//!   [`fm_types::capture::negotiate`]d from the device's offered formats,
//!   the requested format, and the program size and rate. When the device's
//!   offered formats change (a capture card's source changed mode) and the
//!   negotiated format changes with them, acquisition restarts in the new
//!   format (`renegotiations`).
//! - **Signal loss.** The signal is lost when the device leaves the
//!   enumeration (`unplugged`), reports no input signal (`no_signal`),
//!   delivers no picture for [`SupervisorTiming::stall_after`] while live,
//!   or none within [`SupervisorTiming::start_grace`] of opening
//!   (`stalled`), or the acquisition fails (`error`, retried after
//!   [`SupervisorTiming::retry_after`]). The input then shows its loss
//!   slate: the last picture (`hold`) or one opaque black picture
//!   (`black`); its audio stops.
//! - **Recovery.** When the device is back (at the same or a new node), it
//!   is reacquired; the first new picture makes the input live again
//!   (`recoveries`). The input ID never changes.

use std::time::{Duration, Instant};

use fm_types::capture::{
    CaptureFormat, CaptureLossReason, CaptureLossSlate, CaptureSettings, CaptureSignalState,
    CaptureStatus, CaptureTarget, MAX_CAPTURE_ERROR_BYTES, bounded_capture_text, negotiate,
};

use crate::acquire::{AcquireRequest, Acquirer, Acquisition, AcquisitionHealth};
use crate::probe::{ProbedDevice, resolve};

/// The supervisor's timeouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisorTiming {
    /// How long a live input may go without a picture before its signal
    /// is lost.
    pub stall_after: Duration,
    /// How long an opening device may take to deliver its first picture.
    pub start_grace: Duration,
    /// How long to wait before reopening after a failed acquisition.
    pub retry_after: Duration,
}

impl Default for SupervisorTiming {
    fn default() -> Self {
        Self {
            stall_after: Duration::from_secs(1),
            start_grace: Duration::from_secs(6),
            retry_after: Duration::from_secs(1),
        }
    }
}

/// What one program frame shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PulledPicture {
    /// Keep the current picture.
    Keep,
    /// A new RGBA picture.
    New(Vec<u8>),
    /// Show opaque black (the loss slate, once per loss).
    Black,
}

/// One program frame of a capture input.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturePull {
    pub picture: PulledPicture,
    /// This frame's planar audio, or `None` for silence.
    pub audio: Option<Vec<Vec<f32>>>,
}

/// The device and format an acquisition was opened with.
#[derive(Clone, Debug)]
struct Active {
    path: String,
    format: CaptureFormat,
    started_at: Instant,
    last_picture: Option<Instant>,
}

/// One capture input's supervisor.
pub struct CaptureSupervisor {
    key: String,
    settings: CaptureSettings,
    target: CaptureTarget,
    sample_rate: u32,
    timing: SupervisorTiming,
    acquisition: Option<Box<dyn Acquisition>>,
    active: Option<Active>,
    retry_at: Option<Instant>,
    status: CaptureStatus,
    /// The input was live at least once.
    ever_live: bool,
    slate_pending: bool,
    closed: bool,
}

impl std::fmt::Debug for CaptureSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureSupervisor")
            .field("key", &self.key)
            .field("settings", &self.settings)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl CaptureSupervisor {
    /// A supervisor of the device `key` with `settings`, delivering at the
    /// program `target`. It starts acquiring at the first
    /// [`Self::update`].
    #[must_use]
    pub fn new(
        key: String,
        settings: CaptureSettings,
        target: CaptureTarget,
        timing: SupervisorTiming,
    ) -> Self {
        Self {
            key,
            settings,
            target,
            sample_rate: 48_000,
            timing,
            acquisition: None,
            active: None,
            retry_at: None,
            status: CaptureStatus {
                state: CaptureSignalState::Starting,
                ..CaptureStatus::default()
            },
            ever_live: false,
            slate_pending: false,
            closed: false,
        }
    }

    /// Delivers audio at `hertz` (48 kHz by default).
    #[must_use]
    pub const fn with_sample_rate(mut self, hertz: u32) -> Self {
        self.sample_rate = hertz;
        self
    }

    /// The device key this supervisor follows.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The open acquisition's audio clock drift telemetry (AU-004).
    #[must_use]
    pub fn sync_telemetry(&self) -> Option<fm_codec_ffmpeg::network_input::LiveSourceTelemetry> {
        self.acquisition.as_ref()?.sync_telemetry()
    }

    /// The settings this supervisor applies.
    #[must_use]
    pub const fn settings(&self) -> &CaptureSettings {
        &self.settings
    }

    /// The input's status now.
    #[must_use]
    pub const fn status(&self) -> &CaptureStatus {
        &self.status
    }

    /// Changes the settings; a changed format or audio choice reopens the
    /// device at the next update.
    pub fn set_settings(&mut self, settings: CaptureSettings) {
        if settings != self.settings {
            let reopen =
                settings.format != self.settings.format || settings.audio != self.settings.audio;
            self.settings = settings;
            if reopen {
                self.stop();
            }
        }
    }

    /// Closes (IN-026) or reopens the input. A closed input acquires
    /// nothing and is idle.
    pub fn set_closed(&mut self, closed: bool) {
        if closed == self.closed {
            return;
        }
        self.closed = closed;
        self.stop();
        self.slate_pending = false;
        self.retry_at = None;
        self.status.state = if closed {
            CaptureSignalState::Idle
        } else {
            CaptureSignalState::Starting
        };
        self.status.reason = None;
        self.status.device = None;
        self.status.format = None;
        self.status.audio = false;
    }

    fn stop(&mut self) {
        self.acquisition = None;
        self.active = None;
    }

    /// Records a lost signal for `reason`, showing the loss slate once.
    fn lose(&mut self, reason: CaptureLossReason, error: Option<String>) {
        if self.status.state != CaptureSignalState::SignalLost {
            if self.ever_live {
                self.status.losses = self.status.losses.saturating_add(1);
            }
            if self.settings.loss == CaptureLossSlate::Black {
                self.slate_pending = true;
            }
        }
        self.status.state = CaptureSignalState::SignalLost;
        self.status.reason = Some(reason);
        if let Some(error) = error {
            self.status.last_error = Some(bounded_capture_text(&error, MAX_CAPTURE_ERROR_BYTES));
        }
    }

    /// Follows the device enumeration `devices` at `now`: resolves the
    /// device, (re)starts, renegotiates, or drops the acquisition, and
    /// detects lost signals.
    #[allow(clippy::too_many_lines)] // one pass: resolve, negotiate, start, watch
    pub fn update(&mut self, acquirer: &dyn Acquirer, devices: &[ProbedDevice], now: Instant) {
        if self.closed {
            return;
        }
        let Some(device) = resolve(&self.key, devices) else {
            self.stop();
            self.status.device = None;
            self.status.audio = false;
            self.lose(
                CaptureLossReason::Unplugged,
                Some("device not present".to_owned()),
            );
            return;
        };
        let Some(format) = negotiate(
            &device.info.formats,
            self.settings.format.as_ref(),
            self.target,
        ) else {
            self.stop();
            self.status.state = CaptureSignalState::Failed;
            self.status.reason = None;
            self.status.format = None;
            self.status.last_error = Some("the device offers no usable format".to_owned());
            return;
        };
        if let Some(active) = &self.active {
            if active.path != device.info.path {
                // Re-enumerated at another node: reopen there.
                self.stop();
            } else if active.format != format {
                self.status.renegotiations = self.status.renegotiations.saturating_add(1);
                self.stop();
            }
        }
        if device.no_signal {
            self.lose(
                CaptureLossReason::NoSignal,
                Some("the device reports no input signal".to_owned()),
            );
        } else if self.status.reason == Some(CaptureLossReason::NoSignal) {
            // The source is back; the next picture makes the input live.
            self.status.reason = Some(CaptureLossReason::Stalled);
        }
        if self.acquisition.is_none() && self.retry_at.is_none_or(|at| now >= at) {
            self.retry_at = None;
            let request = AcquireRequest {
                device: device.info.clone(),
                format,
                audio: match &self.settings.audio {
                    fm_types::capture::CaptureAudio::None => None,
                    fm_types::capture::CaptureAudio::Paired => device.info.audio.clone(),
                    fm_types::capture::CaptureAudio::Alsa(name) => Some(name.clone()),
                },
                width: self.target.width,
                height: self.target.height,
                rate: self.target.rate,
                sample_rate: self.sample_rate,
            };
            match acquirer.start(&request) {
                Ok(acquisition) => {
                    self.acquisition = Some(acquisition);
                    self.active = Some(Active {
                        path: device.info.path.clone(),
                        format,
                        started_at: now,
                        last_picture: None,
                    });
                    self.status.device = Some(device.info.path.clone());
                    self.status.format = Some(format);
                    if self.status.state != CaptureSignalState::SignalLost {
                        self.status.state = CaptureSignalState::Starting;
                    }
                }
                Err(error) => {
                    self.retry_at = Some(now + self.timing.retry_after);
                    self.lose(CaptureLossReason::Error, Some(error));
                    return;
                }
            }
        }
        let Some(acquisition) = &self.acquisition else {
            return;
        };
        self.status.audio = acquisition.audio_active();
        if let AcquisitionHealth::Failed(error) = acquisition.health() {
            self.stop();
            self.retry_at = Some(now + self.timing.retry_after);
            self.lose(CaptureLossReason::Error, Some(error));
            return;
        }
        let Some(active) = &self.active else {
            return;
        };
        let silent_for =
            now.saturating_duration_since(active.last_picture.unwrap_or(active.started_at));
        let limit = if active.last_picture.is_some() {
            self.timing.stall_after
        } else {
            self.timing.start_grace
        };
        if silent_for >= limit && self.status.state != CaptureSignalState::SignalLost {
            let error = acquisition
                .last_error()
                .or_else(|| Some(format!("no picture for {} ms", silent_for.as_millis())));
            self.lose(CaptureLossReason::Stalled, error);
        }
    }

    /// Takes this program frame's picture and audio at `now`.
    pub fn pull(&mut self, now: Instant) -> CapturePull {
        let slate = |pending: &mut bool| {
            if std::mem::take(pending) {
                PulledPicture::Black
            } else {
                PulledPicture::Keep
            }
        };
        if self.closed {
            return CapturePull {
                picture: PulledPicture::Keep,
                audio: None,
            };
        }
        let Some(acquisition) = &mut self.acquisition else {
            return CapturePull {
                picture: slate(&mut self.slate_pending),
                audio: None,
            };
        };
        let frame = acquisition.pull();
        let Some(picture) = frame.picture else {
            let lost = self.status.state == CaptureSignalState::SignalLost;
            return CapturePull {
                picture: slate(&mut self.slate_pending),
                audio: if lost { None } else { frame.audio },
            };
        };
        if let Some(active) = &mut self.active {
            active.last_picture = Some(now);
        }
        self.status.frames = self.status.frames.saturating_add(1);
        if self.status.reason == Some(CaptureLossReason::NoSignal) {
            // A card without a source may still send pictures: they show,
            // but the input stays lost until the source returns.
            self.slate_pending = false;
            return CapturePull {
                picture: PulledPicture::New(picture),
                audio: None,
            };
        }
        if self.status.state != CaptureSignalState::Live {
            if self.ever_live && self.status.state == CaptureSignalState::SignalLost {
                self.status.recoveries = self.status.recoveries.saturating_add(1);
            }
            self.status.state = CaptureSignalState::Live;
            self.status.reason = None;
            self.status.last_error = None;
            self.slate_pending = false;
            self.ever_live = true;
        }
        CapturePull {
            picture: PulledPicture::New(picture),
            audio: frame.audio,
        }
    }
}
