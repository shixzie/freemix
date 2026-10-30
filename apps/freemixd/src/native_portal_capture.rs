//! Screen capture inputs of a Wayland desktop, through its
//! xdg-desktop-portal (IN-011).
//!
//! On a Wayland desktop ([`crate::desktop_capture::DesktopBackend::Portal`])
//! [`crate::native_screen_capture::NativeScreenCapture`] hands its inputs to
//! one [`PortalSources`]: each open input runs one
//! [`fm_io_linux::wayland::PortalCapture`], which asks the desktop to share
//! a screen (a screen target) or a window (a window target) and turns the
//! shared `PipeWire` pictures into program pictures (cropped, letterboxed,
//! at most the capture rate). Nothing is listed: the desktop's own sharing
//! dialog picks the target, and the input is `awaiting_selection` while it
//! waits on the server's desktop. A cancelled or refused dialog, or sharing
//! stopped on the desktop, is `denied` until the operator picks again
//! ([`PortalSources::pick`]); a shared window that closes is `target_gone`
//! and asked for again. Where asking again would reopen the dialog (no
//! restore token), a share that ended or failed is `reopening` for half a
//! minute and longer instead, so the operator is not prompted every few
//! seconds; a pick asks at once.
//!
//! Each grant's restore token is kept per input
//! ([`PortalSources::take_restore_tokens`]) and the session loop stores it
//! in the project (see [`keep_restore_tokens`]), so a reopened project
//! shares the same targets again without the dialog where the desktop
//! allows it. A new pick forgets the input's token first, so the dialog
//! asks again. An input's next share waits for its last one to close (which
//! can take seconds), so one input never runs two shares or dialogs.
//!
//! A capture's sound does not depend on how its picture is shared: it is
//! keyed by the server's audio system, not by the window system. **System
//! audio** is the `PulseAudio`/`PipeWire` default output's monitor, exactly
//! as on X11, read by an audio-only receiver (`SystemSound`) that runs
//! while the share is live: it starts when the share goes live and stops
//! when the share is dropped (picked again, closed, or its kind or sound
//! changed) or stops being live (denied, its target gone or paused, or
//! asked for again), starting anew if it is live again. Its sound feeds
//! the input's own Master strip while the picture is live, with the
//! receiver's own drift loop (AU-004); its state travels in the input's
//! status as on X11. **Application audio** stays `unsupported`
//! ([`NO_PORTAL_APP_AUDIO`]): a portal grant names neither the process nor
//! the application of the window picked in its dialog, and its restore
//! data is private to the desktop's portal backend, so no playback stream
//! can be matched to it reliably.
//!
//! Statuses are reported like every other screen capture's: at once when
//! the state, size, restart count, or audio state changes, a changed error
//! at most once a second, and each state change logged as a
//! `FREEMIXD_SCREEN_CAPTURE` record (`backend=portal`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, PulledFrame};
use fm_io_linux::screen_capture::capture_rate;
use fm_io_linux::wayland::{
    CaptureSettings, PortalBackend, PortalCapture, ShareRequest, SourceKind,
};
use fm_persistence::StoredProject;
use fm_protocol::{
    ScreenCaptureAudio, ScreenCaptureAudioState, ScreenCaptureConfig, ScreenCaptureState,
    ScreenCaptureStatus, SystemAudioState, bound_screen_capture_error,
};
use fm_types::InputId;
use fm_types::network_stream::StallSlate;

use crate::native_files::{blank_picture, output_timing};
use crate::native_streams::{Format, PulledStream};
use crate::native_sync::{SourceKind as SyncKind, SyncLog, wire_sample};

/// Starts one share of the desktop (the real portal, or a fake in tests).
pub(crate) type PortalFactory = Arc<dyn Fn() -> Box<dyn PortalBackend> + Send + Sync>;

/// The audio-only receiver of a capture's system audio in a format (the
/// default output's monitor, or a test tone in tests).
pub(crate) type SoundFactory = fn(&Format) -> NetworkInputConfig;

/// How long restore tokens the project refused wait to be offered again.
pub(crate) const TOKEN_RETRY: Duration = Duration::from_secs(5);

/// Longest a changed error text waits before it is reported.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

struct Source {
    config: ScreenCaptureConfig,
    closed: bool,
    capture: Option<PortalCapture>,
    /// The input's last capture, stopped but perhaps still closing its
    /// share: the next capture asks the desktop only once it has ended.
    retired: Option<PortalCapture>,
    /// Shares started for this input after its first.
    restarts: u32,
    /// A captured picture is on the input (what a black slate replaces).
    showing: bool,
    /// A blank picture is due: the input closed.
    blank: bool,
    /// The share's system audio, once the share went live.
    sound: Option<SystemSound>,
    reported: Option<(ScreenCaptureStatus, Instant)>,
}

impl Source {
    /// Stops the input's capture; its successor waits for it to end.
    fn retire(&mut self) {
        if let Some(capture) = self.capture.take() {
            self.retired = Some(capture);
        }
    }
}

/// A share's system audio: an audio-only receiver of the default output's
/// monitor.
struct SystemSound {
    receiver: Option<NetworkReceiver>,
    /// Why the receiver could not start, if it could not.
    start_error: Option<String>,
    /// The reopen count of the receiver connection last seen live.
    live_connection: Option<u32>,
    /// When its clock drift was last logged (AU-004).
    sync_log: SyncLog,
}

impl SystemSound {
    fn start(input: InputId, config: NetworkInputConfig) -> Self {
        let (receiver, start_error) = match NetworkReceiver::start(config) {
            Ok(receiver) => (Some(receiver), None),
            Err(error) => {
                eprintln!(
                    "FREEMIXD_SCREEN_CAPTURE_AUDIO\tv=1\tinput={input}\tstart_failed\t\
                     backend=portal"
                );
                (None, Some(error.to_string()))
            }
        };
        Self {
            receiver,
            start_error,
            live_connection: None,
            sync_log: SyncLog::default(),
        }
    }

    /// The audio state and error the receiver is in.
    fn state(&mut self) -> (ScreenCaptureAudioState, Option<String>) {
        let bounded =
            |text: &str| Some(bound_screen_capture_error(text)).filter(|text| !text.is_empty());
        let Some(receiver) = &self.receiver else {
            return (
                ScreenCaptureAudioState::Failed,
                self.start_error.as_deref().and_then(bounded),
            );
        };
        let status = crate::native_system_audio::system_audio_status(
            &receiver.status(),
            &mut self.live_connection,
        );
        match status.state {
            SystemAudioState::Live => (ScreenCaptureAudioState::Live, None),
            SystemAudioState::Idle | SystemAudioState::Opening => {
                (ScreenCaptureAudioState::Opening, None)
            }
            SystemAudioState::Reopening { .. } | SystemAudioState::Failed => (
                ScreenCaptureAudioState::Failed,
                bounded(
                    status
                        .last_error
                        .as_deref()
                        .unwrap_or(NO_SYSTEM_AUDIO_OPENED),
                ),
            ),
        }
    }
}

/// The default output's monitor of the `PulseAudio`/`PipeWire` server, as
/// an audio-only receiver at the program rate.
pub(crate) fn pulse_monitor(format: &Format) -> NetworkInputConfig {
    let pulse = fm_io_linux::capture_audio::pulse_system_audio();
    let mut config = NetworkInputConfig::audio_device(
        pulse.format,
        pulse.path,
        crate::native_system_audio::settings(),
        format.rate,
    );
    if let Some(device) = &mut config.device {
        device.options = pulse.options;
    }
    config.sample_rate = format.sample_rate.hertz();
    config.channels = 2;
    config
}

/// The screen capture inputs of a daemon on a Wayland desktop.
pub(crate) struct PortalSources {
    factory: PortalFactory,
    sound: SoundFactory,
    format: Format,
    sources: BTreeMap<InputId, Source>,
    /// Each input's latest restore token.
    tokens: BTreeMap<InputId, String>,
    tokens_changed: bool,
    /// The project refused the tokens last taken: they are offered again
    /// from then on.
    tokens_retry_at: Option<Instant>,
}

/// Why a desktop-portal capture carries no application audio.
pub(crate) const NO_PORTAL_APP_AUDIO: &str = "a Wayland sharing dialog does not say which \
     application a window belongs to, so its sound cannot be told apart; use system audio";
/// Why system audio failed without an error of its own.
const NO_SYSTEM_AUDIO_OPENED: &str =
    "the system audio did not open (is PulseAudio or PipeWire running?)";

/// What a capture asks of the desktop for `config`.
fn request(config: &ScreenCaptureConfig, token: Option<&String>) -> ShareRequest {
    ShareRequest {
        kind: SourceKind::of(&config.target),
        cursor: config.cursor,
        restore_token: token.cloned(),
    }
}

impl PortalSources {
    pub(crate) fn new(factory: PortalFactory, format: Format) -> Self {
        Self::with_sound(factory, pulse_monitor, format)
    }

    /// Captures system audio through `sound` instead of the default
    /// output's monitor.
    pub(crate) fn with_sound(factory: PortalFactory, sound: SoundFactory, format: Format) -> Self {
        Self {
            factory,
            sound,
            format,
            sources: BTreeMap::new(),
            tokens: BTreeMap::new(),
            tokens_changed: false,
            tokens_retry_at: None,
        }
    }

    fn settings(&self, config: &ScreenCaptureConfig) -> CaptureSettings {
        CaptureSettings {
            crop: config.crop,
            fps: capture_rate(config, self.format.rate),
            width: self.format.dimensions.0,
            height: self.format.dimensions.1,
        }
    }

    /// Takes the restore tokens the project keeps (before any capture
    /// starts).
    pub(crate) fn set_restore_tokens(&mut self, tokens: BTreeMap<InputId, String>) {
        self.tokens = tokens;
        self.tokens_changed = false;
    }

    /// Every screen capture input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.sources.keys().copied().collect()
    }

    /// Follows the project: closes, reopens, adds, removes, and
    /// reconfigures inputs. A new target kind or pointer setting asks the
    /// desktop again; a new crop or rate applies to the next pictures.
    pub(crate) fn sync(
        &mut self,
        configs: &[(InputId, ScreenCaptureConfig)],
        closed: impl Fn(InputId) -> bool,
    ) {
        self.sources
            .retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        let before = self.tokens.len();
        self.tokens
            .retain(|input, _| configs.iter().any(|(candidate, _)| candidate == input));
        self.tokens_changed |= self.tokens.len() != before;
        for (input, config) in configs {
            let closed = closed(*input);
            let settings = self.settings(config);
            let source = self.sources.entry(*input).or_insert_with(|| Source {
                config: config.clone(),
                closed,
                capture: None,
                retired: None,
                restarts: 0,
                showing: false,
                blank: false,
                sound: None,
                reported: None,
            });
            if source.config != *config {
                let old = &source.config;
                if old.audio != config.audio {
                    source.sound = None;
                }
                if SourceKind::of(&old.target) != SourceKind::of(&config.target) {
                    // A token restores a screen or a window, not the other.
                    self.tokens_changed |= self.tokens.remove(input).is_some();
                    source.retire();
                } else if old.cursor != config.cursor {
                    source.retire();
                } else if let Some(capture) = &source.capture {
                    capture.set_settings(settings);
                }
                source.config = config.clone();
            }
            if closed && !source.closed {
                source.retire();
                source.showing = false;
                source.blank = true;
            }
            if source.capture.is_none() {
                source.sound = None;
            }
            if !closed && source.closed {
                source.blank = false;
            }
            source.closed = closed;
        }
    }

    /// Starts a share for every open input without one, and the system
    /// audio of every share that went live without it; stops the system
    /// audio of every share that is not live (denied, gone, paused, or
    /// asked for again).
    fn follow(&mut self) {
        for (input, source) in &mut self.sources {
            if source.closed {
                continue;
            }
            if let Some(capture) = &source.capture {
                if capture.status().state != ScreenCaptureState::Live {
                    source.sound = None;
                } else if source.config.audio == ScreenCaptureAudio::System
                    && source.sound.is_none()
                {
                    source.sound = Some(SystemSound::start(*input, (self.sound)(&self.format)));
                }
                continue;
            }
            if source.reported.is_some() {
                source.restarts = source.restarts.saturating_add(1);
            }
            let settings = CaptureSettings {
                crop: source.config.crop,
                fps: capture_rate(&source.config, self.format.rate),
                width: self.format.dimensions.0,
                height: self.format.dimensions.1,
            };
            source.capture = Some(PortalCapture::start_after(
                source.retired.take(),
                (self.factory)(),
                request(&source.config, self.tokens.get(input)),
                settings,
            ));
        }
    }

    /// Asks the desktop to pick `input`'s screen or window again: its
    /// restore token is forgotten and its share starts over. Returns
    /// whether `input` is one of these inputs.
    pub(crate) fn pick(&mut self, input: InputId) -> bool {
        let Some(source) = self.sources.get_mut(&input) else {
            return false;
        };
        self.tokens_changed |= self.tokens.remove(&input).is_some();
        source.retire();
        source.sound = None;
        true
    }

    /// Pulls every input's picture for engine frame `frame`.
    pub(crate) fn pull(&mut self, frame: u64) -> Vec<PulledStream> {
        self.follow();
        let format = self.format;
        let mut pulled = Vec::with_capacity(self.sources.len());
        for (&input, source) in &mut self.sources {
            let live = source
                .capture
                .as_ref()
                .is_some_and(|capture| capture.status().state == ScreenCaptureState::Live);
            let video = if std::mem::take(&mut source.blank) {
                output_timing(frame, format.rate, format.clock_domain)
                    .and_then(|timing| blank_picture(format.dimensions, timing))
            } else {
                source.capture.as_ref().and_then(|capture| {
                    if let Some(bytes) = capture.take_picture() {
                        source.showing = true;
                        return format.picture(frame, bytes);
                    }
                    let lost = source.showing && !live && source.config.slate == StallSlate::Black;
                    if lost {
                        source.showing = false;
                        format.black(frame)
                    } else {
                        None
                    }
                })
            };
            if source.closed && video.is_none() {
                continue;
            }
            // The receiver is drained every frame; its sound is heard while
            // the picture is live.
            let sound = source
                .sound
                .as_ref()
                .and_then(|sound| sound.receiver.as_ref())
                .map(|receiver| {
                    let PulledFrame { audio, .. } = receiver.pull();
                    audio
                });
            let audio = match sound {
                Some(planes) if live => format.audio(frame, planes),
                _ => format.silence(frame),
            };
            pulled.push(PulledStream {
                input,
                video,
                audio,
            });
        }
        pulled
    }

    /// Collects the restore tokens the captures were granted.
    fn collect_tokens(&mut self) {
        for (input, source) in &self.sources {
            let Some(token) = source
                .capture
                .as_ref()
                .and_then(PortalCapture::take_restore_token)
            else {
                continue;
            };
            let changed = match token {
                Some(token) => self.tokens.insert(*input, token.clone()) != Some(token),
                None => self.tokens.remove(input).is_some(),
            };
            self.tokens_changed |= changed;
        }
    }

    /// Every input's restore token, when any changed since the last call
    /// (or the project refused them, once [`TOKEN_RETRY`] has passed).
    pub(crate) fn take_restore_tokens(
        &mut self,
        now: Instant,
    ) -> Option<BTreeMap<InputId, String>> {
        self.collect_tokens();
        if self.tokens_retry_at.is_some_and(|at| now < at) {
            return None;
        }
        self.tokens_retry_at = None;
        std::mem::take(&mut self.tokens_changed).then(|| self.tokens.clone())
    }

    /// The project refused the tokens last taken: they are offered again
    /// after [`TOKEN_RETRY`] (sooner would only repeat the refusal).
    pub(crate) fn restore_tokens_refused(&mut self, now: Instant) {
        self.tokens_changed = true;
        self.tokens_retry_at = Some(now + TOKEN_RETRY);
    }

    /// The statuses due for a report at `now`.
    pub(crate) fn statuses(&mut self, now: Instant) -> Vec<(InputId, ScreenCaptureStatus)> {
        self.follow();
        self.collect_tokens();
        let mut due = Vec::new();
        for (&input, source) in &mut self.sources {
            if let Some(sound) = &mut source.sound {
                sound.sync_log.report(
                    SyncKind::ScreenCapture,
                    input,
                    sound
                        .receiver
                        .as_ref()
                        .and_then(NetworkReceiver::sync_telemetry)
                        .as_ref(),
                    now,
                );
            }
            let status = match (&source.capture, source.closed) {
                (Some(capture), false) => {
                    let mut status = capture.status();
                    status.reopens = status.reopens.saturating_add(source.restarts);
                    (status.audio, status.audio_error) = audio_status(source, status.state);
                    status
                }
                _ => ScreenCaptureStatus::default(),
            };
            let report = source.reported.as_ref().is_none_or(|(reported, at)| {
                attempt_of(reported.state) != attempt_of(status.state)
                    || reported.reopens != status.reopens
                    || reported.width != status.width
                    || reported.height != status.height
                    || reported.audio != status.audio
                    || (reported.last_error.is_none() && status.last_error.is_some())
                    || (reported.audio_error.is_none() && status.audio_error.is_some())
                    || ((reported.last_error != status.last_error
                        || reported.audio_error != status.audio_error)
                        && now.saturating_duration_since(*at) >= STATUS_INTERVAL)
            });
            if !report {
                continue;
            }
            if source
                .reported
                .as_ref()
                .is_none_or(|(reported, _)| reported.state.token() != status.state.token())
            {
                eprintln!(
                    "FREEMIXD_SCREEN_CAPTURE\tv=1\tinput={input}\tstate={}\treopens={}\t\
                     size={}x{}\ttarget={}\tbackend=portal",
                    status.state.token(),
                    status.reopens,
                    status.width,
                    status.height,
                    source.config.target.kind_token()
                );
            }
            if source
                .reported
                .as_ref()
                .is_none_or(|(reported, _)| reported.audio != status.audio)
                && status.audio != ScreenCaptureAudioState::Off
            {
                eprintln!(
                    "FREEMIXD_SCREEN_CAPTURE_AUDIO\tv=1\tinput={input}\taudio={}\tstate={}\t\
                     backend=portal",
                    source.config.audio.token(),
                    status.audio.token(),
                );
            }
            source.reported = Some((status.clone(), now));
            due.push((input, status));
        }
        due
    }

    /// Every open share's system audio drift loop as wire samples
    /// (Protocol 2.76).
    pub(crate) fn sync_samples(&self) -> Vec<fm_protocol::SourceSyncSample> {
        self.sources
            .iter()
            .filter(|(_, source)| !source.closed)
            .filter_map(|(&input, source)| {
                let receiver = source.sound.as_ref()?.receiver.as_ref()?;
                wire_sample(SyncKind::ScreenCapture, input, &receiver.sync_telemetry()?)
            })
            .collect()
    }
}

/// The audio state of a share whose picture is in `picture`: system audio
/// is off unless the picture is live; application audio is unsupported.
fn audio_status(
    source: &mut Source,
    picture: ScreenCaptureState,
) -> (ScreenCaptureAudioState, Option<String>) {
    match source.config.audio {
        ScreenCaptureAudio::None => (ScreenCaptureAudioState::Off, None),
        ScreenCaptureAudio::Application => (
            ScreenCaptureAudioState::Unsupported,
            Some(NO_PORTAL_APP_AUDIO.to_owned()),
        ),
        ScreenCaptureAudio::System => match &mut source.sound {
            Some(sound) if picture == ScreenCaptureState::Live => sound.state(),
            None if picture == ScreenCaptureState::Live => (ScreenCaptureAudioState::Opening, None),
            _ => (ScreenCaptureAudioState::Off, None),
        },
    }
}

/// The state token and reopen attempt of `state`, which a report compares.
fn attempt_of(state: ScreenCaptureState) -> (&'static str, u32) {
    match state {
        ScreenCaptureState::Reopening { attempt, .. } => ("reopening", attempt),
        other => (other.token(), 0),
    }
}

/// What [`keep_restore_tokens`] made of the tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kept {
    /// The project changed and is due to be written.
    Changed,
    /// The project already kept them.
    Unchanged,
    /// The project refused them: they are to be offered again
    /// ([`PortalSources::restore_tokens_refused`]).
    Refused,
}

/// Keeps `tokens` as the screen capture restore tokens of the `durable`
/// project.
pub(crate) fn keep_restore_tokens(
    durable: &mut StoredProject,
    tokens: BTreeMap<InputId, String>,
) -> Kept {
    let mut project = durable.project().clone();
    if !project.set_screen_capture_restore_tokens(tokens) {
        return Kept::Unchanged;
    }
    match durable.with_project(project) {
        Ok(updated) => {
            *durable = updated;
            Kept::Changed
        }
        Err(error) => {
            eprintln!("FREEMIXD_SCREEN_CAPTURE\tv=1\trestore_tokens=refused\terror={error}");
            Kept::Refused
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::num::NonZeroU128;
    use std::sync::Mutex;

    use fm_frame::{ClockDomainId, SampleRate};
    use fm_io_linux::wayland::{PixelLayout, ShareEnd, ShareSink, SourcePicture};
    use fm_protocol::{ScreenCaptureTarget, WindowRule};
    use fm_types::FrameRate;

    use super::*;

    /// What one fake share does.
    #[derive(Clone, Debug)]
    enum Share {
        /// The dialog waits until the capture is dropped.
        Wait,
        /// Granted with this token, then red pictures until dropped.
        Grant(Option<&'static str>),
        /// Refused.
        Deny,
        /// Granted, red pictures for a while, then stopped on the desktop.
        Stop,
    }

    #[derive(Default)]
    struct Script {
        shares: VecDeque<Share>,
        requests: Vec<ShareRequest>,
        /// How long a share takes to close once stopped.
        close: Duration,
        /// The shares open now, and the most ever open at once.
        open: usize,
        most_open: usize,
    }

    struct Fake(Arc<Mutex<Script>>);

    impl PortalBackend for Fake {
        fn share(&mut self, request: &ShareRequest, sink: &mut dyn ShareSink) -> ShareEnd {
            let share = {
                let mut script = self.0.lock().unwrap();
                script.requests.push(request.clone());
                script.open += 1;
                script.most_open = script.most_open.max(script.open);
                script.shares.pop_front().unwrap_or(Share::Wait)
            };
            let end = Self::run(&share, sink);
            let close = self.0.lock().unwrap().close;
            std::thread::sleep(close);
            self.0.lock().unwrap().open -= 1;
            end
        }
    }

    impl Fake {
        fn run(share: &Share, sink: &mut dyn ShareSink) -> ShareEnd {
            match share {
                Share::Deny => {
                    sink.awaiting_selection();
                    return ShareEnd::Denied(fm_io_linux::wayland::DENIED_CANCELLED.into());
                }
                Share::Wait => sink.awaiting_selection(),
                Share::Grant(token) => sink.granted(token.map(str::to_owned), None),
                Share::Stop => sink.granted(None, None),
            }
            let until = Instant::now() + Duration::from_millis(300);
            let data: Vec<u8> = (0..16).flat_map(|_| [0, 0, 255, 0]).collect();
            while !sink.stopping() {
                if matches!(share, Share::Stop) && Instant::now() >= until {
                    return ShareEnd::Denied(fm_io_linux::wayland::STOPPED_ON_DESKTOP.into());
                }
                if matches!(share, Share::Grant(_) | Share::Stop) {
                    sink.frame(&SourcePicture {
                        data: &data,
                        width: 4,
                        height: 4,
                        stride: 16,
                        layout: PixelLayout::Bgrx,
                    });
                }
                sink.idle();
                std::thread::sleep(Duration::from_millis(5));
            }
            ShareEnd::Stopped
        }
    }

    fn input(id: u128) -> InputId {
        InputId::new(NonZeroU128::new(id).unwrap())
    }

    fn sources(shares: Vec<Share>) -> (PortalSources, Arc<Mutex<Script>>) {
        sources_with(shares, pulse_monitor)
    }

    fn sources_with(
        shares: Vec<Share>,
        sound: SoundFactory,
    ) -> (PortalSources, Arc<Mutex<Script>>) {
        let script = Arc::new(Mutex::new(Script {
            shares: shares.into(),
            ..Script::default()
        }));
        let fake = Arc::clone(&script);
        let factory: PortalFactory =
            Arc::new(move || Box::new(Fake(Arc::clone(&fake))) as Box<dyn PortalBackend>);
        let format = Format {
            rate: FrameRate::new(30, 1).unwrap(),
            clock_domain: ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            dimensions: (8, 8),
            sample_rate: SampleRate::new(48_000).unwrap(),
        };
        (PortalSources::with_sound(factory, sound, format), script)
    }

    /// A 440 Hz tone in real time, standing in for the default output's
    /// monitor.
    fn tone(format: &Format) -> NetworkInputConfig {
        let mut config = NetworkInputConfig::audio_device(
            "lavfi",
            "sine=frequency=440:sample_rate=48000,arealtime",
            crate::native_system_audio::settings(),
            format.rate,
        );
        config.sample_rate = format.sample_rate.hertz();
        config.channels = 2;
        config
    }

    /// Whether `ffmpeg` runs; its absence fails under `FM_REQUIRE_FFMPEG=1`.
    fn ffmpeg_available() -> bool {
        let available = std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(
            available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
            "FM_REQUIRE_FFMPEG=1 but ffmpeg is unavailable"
        );
        available
    }

    fn window() -> ScreenCaptureConfig {
        ScreenCaptureConfig {
            slate: StallSlate::Black,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::picked()))
        }
    }

    fn settle(
        sources: &mut PortalSources,
        input: InputId,
        state: impl Fn(ScreenCaptureState) -> bool,
    ) -> ScreenCaptureStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        loop {
            for (reported, status) in sources.statuses(Instant::now()) {
                if reported == input {
                    last = Some(status);
                }
            }
            if let Some(status) = last.as_ref().filter(|status| state(status.state)) {
                return status.clone();
            }
            assert!(Instant::now() < deadline, "{last:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_share_waits_for_the_pick_goes_live_and_keeps_its_token() {
        let (mut sources, script) = sources(vec![Share::Wait]);
        sources.set_restore_tokens(BTreeMap::from([(input(1), "old".to_owned())]));
        sources.sync(&[(input(1), window())], |_| false);
        assert_eq!(sources.inputs(), vec![input(1)]);
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        assert_eq!(
            script.lock().unwrap().requests[0],
            ShareRequest {
                kind: SourceKind::Window,
                cursor: true,
                restore_token: Some("old".into())
            }
        );
        assert_eq!(sources.take_restore_tokens(Instant::now()), None);

        // Pick again: the token is forgotten and the dialog asks anew.
        script
            .lock()
            .unwrap()
            .shares
            .push_back(Share::Grant(Some("new")));
        assert!(sources.pick(input(1)));
        assert!(!sources.pick(input(2)));
        assert_eq!(
            sources.take_restore_tokens(Instant::now()),
            Some(BTreeMap::new())
        );
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Live
        });
        assert_eq!((status.width, status.height, status.reopens), (4, 4, 1));
        assert_eq!(status.audio, ScreenCaptureAudioState::Off);
        assert_eq!(script.lock().unwrap().requests[1].restore_token, None);
        assert_eq!(
            sources.take_restore_tokens(Instant::now()),
            Some(BTreeMap::from([(input(1), "new".to_owned())]))
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        for frame in 1.. {
            let pulled = sources.pull(frame);
            if pulled.into_iter().any(|stream| stream.video.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "no picture");
            std::thread::sleep(Duration::from_millis(5));
        }

        // Closing shows a blank picture and stops the share.
        sources.sync(&[(input(1), window())], |_| true);
        let pulled = sources.pull(1_000_000);
        assert!(pulled[0].video.is_some());
        assert_eq!(
            settle(&mut sources, input(1), |state| state
                == ScreenCaptureState::Idle)
            .state,
            ScreenCaptureState::Idle
        );
    }

    #[test]
    fn a_share_stopped_on_the_desktop_stops_its_system_audio() {
        let (mut sources, _script) = sources_with(vec![Share::Stop], tone);
        let system = ScreenCaptureConfig {
            audio: ScreenCaptureAudio::System,
            ..window()
        };
        sources.sync(&[(input(1), system)], |_| false);
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Live
        });
        // The next pass starts the reader of the live share.
        sources.statuses(Instant::now());
        assert!(
            sources.sources[&input(1)].sound.is_some(),
            "heard while live"
        );
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Denied
        });
        // The share may have been denied after this pass followed it; the
        // next pass stops the reader of a share that is not live.
        sources.statuses(Instant::now());
        assert!(
            sources.sources[&input(1)].sound.is_none(),
            "the system audio reader stopped"
        );
    }

    #[test]
    fn refused_restore_tokens_are_offered_again() {
        let (mut sources, _script) = sources(vec![Share::Grant(Some("granted"))]);
        sources.sync(&[(input(1), window())], |_| false);
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Live
        });
        let now = Instant::now();
        let granted = Some(BTreeMap::from([(input(1), "granted".to_owned())]));
        assert_eq!(sources.take_restore_tokens(now), granted);
        assert_eq!(sources.take_restore_tokens(now), None, "once per change");
        // The project refused them: not offered at once, but again later.
        sources.restore_tokens_refused(now);
        assert_eq!(sources.take_restore_tokens(now), None);
        assert_eq!(
            sources.take_restore_tokens(now + TOKEN_RETRY.saturating_sub(Duration::from_millis(1))),
            None
        );
        assert_eq!(sources.take_restore_tokens(now + TOKEN_RETRY), granted);
        assert_eq!(sources.take_restore_tokens(now + TOKEN_RETRY), None);
    }

    #[test]
    fn a_new_pick_asks_only_once_the_last_share_closed() {
        let (mut sources, script) = sources(Vec::new());
        script.lock().unwrap().close = Duration::from_millis(400);
        sources.sync(&[(input(1), window())], |_| false);
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        // Picked again twice in a row while the first share closes.
        assert!(sources.pick(input(1)));
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Opening
        });
        assert!(sources.pick(input(1)));
        settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        let script = script.lock().unwrap();
        assert_eq!(script.most_open, 1, "one share at a time");
        assert_eq!(script.requests.len(), 2, "the dropped pick never asked");
    }

    #[test]
    fn a_denied_share_waits_for_a_pick_and_a_new_kind_forgets_the_token() {
        let (mut sources, script) = sources(vec![Share::Deny]);
        sources.set_restore_tokens(BTreeMap::from([(input(1), "old".to_owned())]));
        sources.sync(&[(input(1), window())], |_| false);
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Denied
        });
        assert!(
            status
                .last_error
                .is_some_and(|error| error.starts_with("sharing was cancelled"))
        );
        std::thread::sleep(Duration::from_millis(100));
        sources.statuses(Instant::now());
        assert_eq!(script.lock().unwrap().requests.len(), 1, "not asked again");

        // A screen instead of a window: the window's token is no use.
        // Its system audio is off until the share is live.
        let screen = ScreenCaptureConfig {
            audio: ScreenCaptureAudio::System,
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 })
        };
        sources.sync(&[(input(1), screen)], |_| false);
        assert_eq!(
            sources.take_restore_tokens(Instant::now()),
            Some(BTreeMap::new())
        );
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        assert_eq!(status.audio, ScreenCaptureAudioState::Off);
        assert_eq!(status.audio_error, None);
        let script = script.lock().unwrap();
        assert_eq!(script.requests[1].kind, SourceKind::Monitor);
        assert_eq!(script.requests[1].restore_token, None);
        drop(script);

        // A removed input's token goes with it.
        sources.set_restore_tokens(BTreeMap::from([(input(1), "kept".to_owned())]));
        sources.sync(&[], |_| false);
        assert_eq!(
            sources.take_restore_tokens(Instant::now()),
            Some(BTreeMap::new())
        );
        assert_eq!(sources.inputs(), Vec::<InputId>::new());
    }

    #[test]
    fn a_live_share_hears_the_system_audio_and_not_an_application() {
        if !ffmpeg_available() {
            eprintln!("skipped: ffmpeg is unavailable");
            return;
        }
        let (mut sources, _script) = sources_with(vec![Share::Wait, Share::Grant(None)], tone);
        let system = ScreenCaptureConfig {
            audio: ScreenCaptureAudio::System,
            ..window()
        };
        sources.sync(&[(input(1), system.clone())], |_| false);
        // Waiting for the pick: no sound yet.
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::AwaitingSelection
        });
        assert_eq!(status.audio, ScreenCaptureAudioState::Off);
        let pulled = sources.pull(1);
        assert!(
            pulled[0].audio.as_ref().is_some_and(|block| block
                .planes()
                .iter()
                .flatten()
                .all(|s| *s == 0.0))
        );

        // Granted: the tone plays on the input's strip once its receiver
        // is live.
        assert!(sources.pick(input(1)));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut frame = 2;
        let mut heard = false;
        let mut live = false;
        while !(heard && live) {
            for (reported, status) in sources.statuses(Instant::now()) {
                if reported == input(1) {
                    live = status.audio == ScreenCaptureAudioState::Live;
                }
            }
            for stream in sources.pull(frame) {
                heard |= stream.audio.as_ref().is_some_and(|block| {
                    block.planes().len() == 2
                        && block.planes().iter().flatten().any(|s| s.abs() > 0.01)
                });
            }
            frame += 1;
            assert!(
                Instant::now() < deadline,
                "heard {heard}, live {live}: {:?}",
                sources.statuses(Instant::now())
            );
            std::thread::sleep(Duration::from_millis(33));
        }

        // Application audio is unsupported, with the reason; the tone stops.
        let application = ScreenCaptureConfig {
            audio: ScreenCaptureAudio::Application,
            ..window()
        };
        sources.sync(&[(input(1), application)], |_| false);
        let status = settle(&mut sources, input(1), |state| {
            state == ScreenCaptureState::Live
        });
        assert_eq!(status.audio, ScreenCaptureAudioState::Unsupported);
        assert_eq!(status.audio_error.as_deref(), Some(NO_PORTAL_APP_AUDIO));
        assert_eq!(sources.sync_samples().len(), 0);
        let pulled = sources.pull(frame);
        assert!(
            pulled[0].audio.as_ref().is_some_and(|block| block
                .planes()
                .iter()
                .flatten()
                .all(|s| *s == 0.0))
        );
    }
}
