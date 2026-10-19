//! Native realization of system audio inputs (AU-011).
//!
//! Each open system audio input of a native project owns one audio-only
//! [`NetworkReceiver`] ([`NetworkInputConfig::audio_device`]): a supervised
//! `ffmpeg` child that captures the named `PulseAudio`/`PipeWire` source
//! (`-f pulse`) or ALSA device (`-f alsa`) at the program sample rate into
//! the receiver's bounded buffer, and reopens it with backoff, without
//! ever giving up, when it fails to open or stops delivering. The session
//! loop never waits on the device: once per engine frame it pulls each
//! receiver ([`NativeSystemAudio::pull`]) and hands the frame's audio to
//! the Master as that frame's pushed audio. The input has no picture: it
//! keeps the transparent black picture it starts on. While the device is
//! not live its audio fades to silence.
//!
//! A closed input (IN-026) stops its receiver and is silent until it
//! reopens. A replaced device restarts the receiver. Only Linux captures
//! system audio; elsewhere every system audio input reports `failed` with
//! an unsupported-platform error, and a live add is refused (see
//! [`crate::native_catalogue`]).
//!
//! Each input's status reaches clients as `system_audio_status` runtime
//! events ([`NativeSystemAudio::statuses`]): at once when its state or
//! reopen count changes, and a changed error text at most once a second.
//! The daemon logs each state change on standard error.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, PulledFrame};
use fm_frame::{ClockDomainId, SampleRate};
use fm_protocol::{SystemAudioConfig, SystemAudioState, SystemAudioStatus};
use fm_types::network_stream::{
    GiveUp, NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus, ReconnectPolicy,
};
use fm_types::{FrameRate, InputId};

use crate::native_streams::{Format, PulledStream};

/// Longest a changed error text waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

/// Whether this platform captures system audio.
pub(crate) const SUPPORTED: bool = cfg!(target_os = "linux");

/// The error every system audio input reports where [`SUPPORTED`] is false.
pub(crate) const UNSUPPORTED: &str = "system audio capture is not supported on this platform";

/// One system audio input a native daemon captures.
pub(crate) struct SystemAudioInput {
    pub(crate) input: InputId,
    pub(crate) config: SystemAudioConfig,
    /// The input is closed (IN-026): nothing captures until it reopens.
    pub(crate) closed: bool,
}

struct Source {
    config: SystemAudioConfig,
    receiver: Option<NetworkReceiver>,
    /// Why the receiver could not start, if it could not.
    start_error: Option<String>,
    closed: bool,
    reported: Option<(SystemAudioStatus, Instant)>,
    /// The reopen count of the receiver connection last seen live: that
    /// connection's brief stalls and rebuffering stay live (see
    /// [`system_audio_status`]).
    live_connection: Option<u32>,
}

/// The system audio inputs of one native daemon and their receivers.
pub(crate) struct NativeSystemAudio {
    sources: BTreeMap<InputId, Source>,
    format: Format,
    /// The engine frame last pulled for.
    last_frame: Option<u64>,
}

/// The receive settings of a system audio device: a short buffer, a
/// two-second no-audio timeout, and reopening every 0.5 to 5 s for good.
fn settings() -> NetworkStreamSettings {
    NetworkStreamSettings {
        jitter_buffer_ms: 120,
        reconnect: ReconnectPolicy {
            backoff_min_ms: 500,
            backoff_max_ms: 5_000,
            give_up: GiveUp::Never,
        },
        timeout_ms: 2_000,
        ..NetworkStreamSettings::default()
    }
}

impl NativeSystemAudio {
    /// Starts a receiver for every open input.
    pub(crate) fn start(
        inputs: Vec<SystemAudioInput>,
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
        sample_rate: SampleRate,
    ) -> Self {
        let mut audio = Self {
            sources: BTreeMap::new(),
            format: Format {
                rate,
                clock_domain,
                dimensions,
                sample_rate,
            },
            last_frame: None,
        };
        for SystemAudioInput {
            input,
            config,
            closed,
        } in inputs
        {
            let mut source = Source {
                config,
                receiver: None,
                start_error: None,
                closed,
                reported: None,
                live_connection: None,
            };
            if !closed {
                audio.open(input, &mut source);
            }
            audio.sources.insert(input, source);
        }
        audio
    }

    fn open(&self, input: InputId, source: &mut Source) {
        source.reported = None;
        source.live_connection = None;
        if !SUPPORTED {
            source.receiver = None;
            source.start_error = Some(UNSUPPORTED.to_owned());
            return;
        }
        let mut config = NetworkInputConfig::audio_device(
            source.config.backend.token(),
            source.config.device.clone(),
            settings(),
            self.format.rate,
        );
        config.sample_rate = self.format.sample_rate.hertz();
        config.channels = 2;
        match NetworkReceiver::start(config) {
            Ok(receiver) => {
                source.receiver = Some(receiver);
                source.start_error = None;
            }
            Err(error) => {
                eprintln!(
                    "FREEMIXD_SYSTEM_AUDIO\tv=1\tinput={input}\tstart_failed\tbackend={}",
                    source.config.backend.token()
                );
                source.receiver = None;
                source.start_error = Some(error.to_string());
            }
        }
    }

    /// Every system audio input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.sources.keys().copied().collect()
    }

    /// Follows the project: closes, reopens, and re-targets inputs as
    /// `configs` and `closed` say. A changed device reopens.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, SystemAudioConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
        let mut sources = std::mem::take(&mut self.sources);
        sources.retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        for (input, config) in configs {
            let closed = closed(*input);
            let source = sources.entry(*input).or_insert_with(|| Source {
                config: config.clone(),
                receiver: None,
                start_error: None,
                closed: true,
                reported: None,
                live_connection: None,
            });
            let changed = source.config != *config;
            if changed {
                source.config = config.clone();
            }
            if closed {
                if !source.closed {
                    source.receiver = None;
                    source.start_error = None;
                }
                source.closed = true;
                continue;
            }
            if source.closed
                || changed
                || (source.receiver.is_none() && source.start_error.is_none())
            {
                source.closed = false;
                // Release the device before it is opened again.
                source.receiver = None;
                self.open(*input, source);
            }
        }
        self.sources = sources;
    }

    /// Pulls every open input's audio for engine frame `frame`, once per
    /// frame. Never blocks on a device.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        if self.last_frame.is_some_and(|last| frame <= last) || self.sources.is_empty() {
            return Vec::new();
        }
        self.last_frame = Some(frame);
        let mut pulled = Vec::with_capacity(self.sources.len());
        for (&input, source) in &self.sources {
            if source.closed {
                continue;
            }
            let audio = match &source.receiver {
                Some(receiver) => {
                    let PulledFrame { audio, .. } = receiver.pull();
                    self.format.audio(frame, audio)
                }
                None => self.format.silence(frame),
            };
            pulled.push(PulledStream {
                input,
                video: None,
                audio,
            });
        }
        pulled
    }

    /// The statuses due for a report at `now`: a changed state, attempt,
    /// or reopen count at once, a changed error at most once a second.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, SystemAudioStatus)> {
        let mut due = Vec::new();
        for (&input, source) in &mut self.sources {
            let status = match (&source.receiver, &source.start_error) {
                _ if source.closed => SystemAudioStatus::default(),
                (Some(receiver), _) => {
                    system_audio_status(&receiver.status(), &mut source.live_connection)
                }
                (None, Some(error)) => SystemAudioStatus {
                    state: SystemAudioState::Failed,
                    reopens: 0,
                    last_error: Some(error.clone()),
                },
                (None, None) => SystemAudioStatus::default(),
            };
            let report = match &source.reported {
                None => true,
                Some((reported, at)) => {
                    attempt_of(reported.state) != attempt_of(status.state)
                        || reported.reopens != status.reopens
                        || (reported.last_error.is_none() && status.last_error.is_some())
                        || (reported.last_error != status.last_error
                            && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
                }
            };
            if !report {
                continue;
            }
            if source
                .reported
                .as_ref()
                .is_none_or(|(reported, _)| reported.state.token() != status.state.token())
            {
                eprintln!(
                    "FREEMIXD_SYSTEM_AUDIO\tv=1\tinput={input}\tstate={}\treopens={}\tbackend={}",
                    status.state.token(),
                    status.reopens,
                    source.config.backend.token()
                );
            }
            source.reported = Some((status.clone(), now));
            due.push((input, status));
        }
        due
    }
}

/// The state token and reopen attempt of `state`, which a report compares
/// (the countdown to the next attempt changes every instant).
fn attempt_of(state: SystemAudioState) -> (&'static str, u32) {
    match state {
        SystemAudioState::Reopening { attempt, .. } => ("reopening", attempt),
        other => (other.token(), 0),
    }
}

/// The system audio status of an audio-only receiver's status: opening
/// until the device first delivers, live while it delivers, reopening
/// between attempts.
///
/// A connection that went live stays live through its brief stalls and
/// rebuffering: the device is open, and a stall that outlasts the timeout
/// ends the connection, which then reports reopening. `live_connection`
/// remembers the reopen count of the connection seen live; a reopen (a
/// higher count) opens again.
fn system_audio_status(
    status: &NetworkStreamStatus,
    live_connection: &mut Option<u32>,
) -> SystemAudioStatus {
    let opened = *live_connection == Some(status.reconnects);
    SystemAudioStatus {
        state: match status.state {
            NetworkStreamState::Idle => SystemAudioState::Idle,
            NetworkStreamState::Buffering | NetworkStreamState::Stalled if opened => {
                SystemAudioState::Live
            }
            NetworkStreamState::Connecting
            | NetworkStreamState::Listening
            | NetworkStreamState::Buffering
            | NetworkStreamState::Stalled => SystemAudioState::Opening,
            NetworkStreamState::Live => {
                *live_connection = Some(status.reconnects);
                SystemAudioState::Live
            }
            NetworkStreamState::Reconnecting {
                attempt,
                next_in_ms,
            } => SystemAudioState::Reopening {
                attempt,
                next_in_ms,
            },
            NetworkStreamState::Failed => SystemAudioState::Failed,
        },
        reopens: status.reconnects,
        last_error: status.last_error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_protocol::SystemAudioBackend;

    use super::*;

    fn input() -> InputId {
        InputId::new(NonZeroU128::new(7).unwrap())
    }

    fn config() -> SystemAudioConfig {
        SystemAudioConfig::new(SystemAudioBackend::Alsa, "freemix_no_such_pcm")
    }

    fn audio(closed: bool) -> NativeSystemAudio {
        NativeSystemAudio::start(
            vec![SystemAudioInput {
                input: input(),
                config: config(),
                closed,
            }],
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            (32, 18),
            SampleRate::new(48_000).unwrap(),
        )
    }

    #[test]
    fn receiver_states_map_to_system_audio_states() {
        for (state, expected) in [
            (NetworkStreamState::Idle, SystemAudioState::Idle),
            (NetworkStreamState::Connecting, SystemAudioState::Opening),
            (NetworkStreamState::Buffering, SystemAudioState::Opening),
            (NetworkStreamState::Stalled, SystemAudioState::Opening),
            (NetworkStreamState::Live, SystemAudioState::Live),
            (
                NetworkStreamState::Reconnecting {
                    attempt: 3,
                    next_in_ms: 40,
                },
                SystemAudioState::Reopening {
                    attempt: 3,
                    next_in_ms: 40,
                },
            ),
            (NetworkStreamState::Failed, SystemAudioState::Failed),
        ] {
            let status = system_audio_status(
                &NetworkStreamStatus {
                    state,
                    reconnects: 4,
                    last_error: Some("gone".into()),
                    ..NetworkStreamStatus::default()
                },
                &mut None,
            );
            assert_eq!(status.state, expected);
            assert_eq!(status.reopens, 4);
            assert_eq!(status.last_error.as_deref(), Some("gone"));
        }
    }

    /// A live device that briefly stalls and rebuffers, with no reopen,
    /// stays live instead of flapping to opening; once its connection ends
    /// and reopens, the new connection opens again until it delivers.
    #[test]
    fn a_live_device_stays_live_through_a_brief_stall() {
        let mut live_connection = None;
        let mut state = |state, reconnects| {
            system_audio_status(
                &NetworkStreamStatus {
                    state,
                    reconnects,
                    ..NetworkStreamStatus::default()
                },
                &mut live_connection,
            )
            .state
        };
        assert_eq!(
            state(NetworkStreamState::Connecting, 0),
            SystemAudioState::Opening
        );
        assert_eq!(
            state(NetworkStreamState::Buffering, 0),
            SystemAudioState::Opening
        );
        assert_eq!(state(NetworkStreamState::Live, 0), SystemAudioState::Live);
        // A stall and its rebuffering within the same connection.
        assert_eq!(
            state(NetworkStreamState::Stalled, 0),
            SystemAudioState::Live
        );
        assert_eq!(
            state(NetworkStreamState::Buffering, 0),
            SystemAudioState::Live
        );
        assert_eq!(state(NetworkStreamState::Live, 0), SystemAudioState::Live);
        // The stall outlasts the timeout: the receiver reopens.
        assert_eq!(
            state(
                NetworkStreamState::Reconnecting {
                    attempt: 1,
                    next_in_ms: 500
                },
                1
            ),
            SystemAudioState::Reopening {
                attempt: 1,
                next_in_ms: 500
            }
        );
        assert_eq!(
            state(NetworkStreamState::Connecting, 1),
            SystemAudioState::Opening
        );
        assert_eq!(
            state(NetworkStreamState::Buffering, 1),
            SystemAudioState::Opening
        );
        assert_eq!(state(NetworkStreamState::Live, 1), SystemAudioState::Live);
        assert_eq!(
            state(NetworkStreamState::Stalled, 1),
            SystemAudioState::Live
        );
        // A connection the reporter never saw live (its reopen happened
        // between two reports) still opens.
        assert_eq!(
            state(NetworkStreamState::Buffering, 2),
            SystemAudioState::Opening
        );
    }

    #[test]
    fn a_closed_input_is_idle_and_silent_until_it_opens() {
        let mut audio = audio(true);
        assert!(audio.pull(1).is_empty());
        assert_eq!(
            audio.statuses(Instant::now()),
            [(input(), SystemAudioStatus::default())]
        );
        assert_eq!(audio.statuses(Instant::now() + STATUS_INTERVAL), []);
        audio.sync(&[(input(), config())], |_| false);
        let source = &audio.sources[&input()];
        assert_eq!(source.receiver.is_some(), SUPPORTED);
        let pulled = audio.pull(2);
        assert_eq!(pulled.len(), 1);
        assert!(pulled[0].video.is_none(), "no picture");
        let block = pulled[0].audio.as_ref().expect("the frame's audio");
        assert_eq!(block.sample_count(), 1_920);
        assert!(block.planes().iter().flatten().all(|sample| *sample == 0.0));
        assert!(audio.pull(2).is_empty(), "once per frame");
        let statuses = audio.statuses(Instant::now());
        assert_eq!(statuses.len(), 1);
        if SUPPORTED {
            assert_eq!(statuses[0].1.state, SystemAudioState::Opening);
        } else {
            assert_eq!(statuses[0].1.state, SystemAudioState::Failed);
            assert_eq!(statuses[0].1.last_error.as_deref(), Some(UNSUPPORTED));
        }
        // A removed input stops.
        audio.sync(&[], |_| false);
        assert_eq!(audio.inputs(), []);
    }
}
