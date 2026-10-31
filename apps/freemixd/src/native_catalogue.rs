//! Live input catalogue changes on the native daemon (IN-003, IN-015,
//! IN-026).
//!
//! A native daemon follows every accepted change to its input catalogue
//! while it runs: an added input (`input_add`), a duplicate, a batch import,
//! a source replacement, a removal, and the undo or redo of any of them.
//! After the command commits, and before the next frame is projected,
//! [`NativeDaemon::sync_input_catalogue`] compares the inputs the runtime
//! realizes with the project's and, between two frames:
//!
//! - retires every removed input (and the old source of a replaced one):
//!   its live source frame leaves the source registry, its strip leaves the
//!   Master mixer, and its media-file worker stops and is joined off the
//!   loop; a network stream input's receiver stops when the receivers
//!   follow the project;
//! - starts every added input (and the new source of a replaced one): a
//!   generator renders its one picture here (cheap: a solid or bars
//!   frame); a media file starts its worker, which opens and decodes the
//!   file on its own thread and reports it, or its fault, to the engine; a
//!   still image decodes on a short-lived thread; a network stream input's
//!   receiver connects; a Scene input only needs the recompiled plan. Each
//!   one shows a transparent black picture and silence until its worker
//!   delivers, and joins the Master with the strip the project gives it;
//! - recompiles the native project plan for the new catalogue.
//!
//! None of this touches a frame on air: the engine refuses to remove or
//! close an input Program, a transition, an overlay, or a scene on air
//! uses, and a new input is on no bus until a later command puts it there.
//!
//! Before such a command is accepted, [`NativeCatalogueBudget::refusal`]
//! vets the catalogue it would leave: kinds the native runtime does not
//! realize live (capture devices, titles, colour, alias, and Program or
//! Preview feed inputs, and a generator's test tone) and a catalogue past
//! the native source bounds (at most
//! [`NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS`] inputs and the retained
//! GPU frame budget) are refused with a typed reason, and nothing is
//! staged. A worker that fails after acceptance (a missing or unsupported
//! file, a stream that does not connect) reports through the input's
//! status, never by refusing the command.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

use fm_codec_ffmpeg::{Adapter, Config as FfmpegConfig};
use fm_codec_image::sniff_still_format;
use fm_control::ControlService;
use fm_engine::EngineMediaFault;
use fm_frame::CpuVideoFrame;
use fm_model::{InputKind, Project, SimulatedAudio, SimulatedInput};
use fm_protocol::RefusalReason;
use fm_types::{InputId, SceneId};
use freemixd::native_media::{
    NativeProjectLimits, NativeProjectPlan, NativeResolvedSource, NativeSourceLimits,
};

use super::{
    AppFailure, AppResult, CommandRefusal, NativeDaemon, Policy, RejectionCode,
    native_clock_domain, native_files, native_generator_frame, native_streams,
};

/// What the native runtime realizes for one input: a change of key means a
/// new source. Playback settings, title text, and stream settings are not
/// part of it; they change live without a new source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativeSourceKey {
    Generator(SimulatedInput),
    Media(String),
    Network,
    /// A Replay input (OR-012): its player ingests recorded frames.
    Replay,
    /// A system audio input (AU-011): a changed device restarts its
    /// receiver without a new source.
    SystemAudio,
    /// A screen capture input (IN-011): a changed target or setting
    /// restarts its capture without a new source.
    ScreenCapture,
    Scene {
        scene: SceneId,
        audio: Option<InputId>,
    },
    Title,
    /// A document input (IN-014): stepping its page keeps its source.
    Document(String),
    Device(String),
    Color,
    Alias,
    MixFeed,
    /// A still image input (IN-008): another file or fit decodes anew.
    Image(String, fm_types::OutputScaling),
    /// A slideshow input (IN-009): its slides change live; another fit
    /// starts a new source.
    Slideshow(fm_types::OutputScaling),
}

impl NativeSourceKey {
    fn of(kind: &InputKind) -> Self {
        match kind {
            InputKind::Simulated(simulated) => Self::Generator(*simulated),
            InputKind::Media { asset_uri, .. } => Self::Media(asset_uri.clone()),
            InputKind::NetworkStream(_) => Self::Network,
            InputKind::Replay => Self::Replay,
            InputKind::SystemAudio(_) => Self::SystemAudio,
            InputKind::ScreenCapture(_) => Self::ScreenCapture,
            InputKind::Scene {
                scene_id,
                audio_source,
            } => Self::Scene {
                scene: *scene_id,
                audio: *audio_source,
            },
            InputKind::Title(_) => Self::Title,
            InputKind::Document(document) => Self::Document(document.asset_uri.clone()),
            InputKind::Device { stable_key, .. } => Self::Device(stable_key.clone()),
            InputKind::Color => Self::Color,
            InputKind::Alias { .. } => Self::Alias,
            InputKind::MixFeed(_) => Self::MixFeed,
            InputKind::Image(image) => Self::Image(image.asset_uri.clone(), image.fit),
            InputKind::Slideshow(slideshow) => Self::Slideshow(slideshow.fit),
        }
    }

    /// Whether the input owns a source in the native source registry.
    const fn has_video_source(&self) -> bool {
        !matches!(self, Self::Scene { .. })
    }

    /// Why the native runtime cannot start this source while it runs, if
    /// it cannot.
    fn unsupported_live(&self, input: InputId, capture_live: bool) -> Option<String> {
        match self {
            Self::Generator(SimulatedInput {
                audio: SimulatedAudio::Sine { .. },
                ..
            }) => Some(format!(
                "generator input {input} has a test tone, which the native media runtime does \
                 not realize"
            )),
            Self::Generator(_)
            | Self::Media(_)
            | Self::Network
            | Self::Replay
            | Self::Document(_)
            | Self::Scene { .. }
            | Self::Image(..)
            | Self::Slideshow(_) => None,
            Self::SystemAudio if super::native_system_audio::SUPPORTED => None,
            Self::SystemAudio => Some(format!(
                "system audio input {input} cannot be added: {}",
                super::native_system_audio::UNSUPPORTED
            )),
            Self::ScreenCapture => {
                let backend = crate::desktop_capture::DesktopBackend::platform();
                backend
                    .unsupported_reason()
                    .map(|reason| format!("screen capture input {input} cannot be added: {reason}"))
            }
            Self::Title => Some(format!(
                "title input {input} cannot be added while the native media runtime runs; stop \
                 the daemon and use freemix-cli"
            )),
            Self::Device(_) if capture_live => None,
            Self::Device(_) => Some(format!(
                "capture input {input} cannot be added or replaced while the native media \
                 runtime runs; stop the daemon and use freemix-cli"
            )),
            Self::Color | Self::Alias | Self::MixFeed => Some(format!(
                "input {input} is a colour, alias, or Program/Preview feed input, which the \
                 native media runtime does not realize"
            )),
        }
    }
}

/// Every input of `project` and the source the native runtime realizes
/// for it.
pub(crate) fn catalogue_keys(project: &Project) -> BTreeMap<InputId, NativeSourceKey> {
    project
        .inputs()
        .iter()
        .map(|input| (input.id, NativeSourceKey::of(&input.kind)))
        .collect()
}

/// What a catalogue change may use of the native runtime, captured before
/// the command is staged.
#[derive(Clone, Debug)]
pub(crate) struct NativeCatalogueBudget {
    /// Inputs with a source in the source registry.
    sources: BTreeSet<InputId>,
    max_sources: usize,
    retained_bytes: u64,
    max_retained_bytes: u64,
    /// GPU bytes one live source frame retains.
    frame_bytes: u64,
    /// Inputs whose audio the Master decodes itself (Stinger clips).
    decoded: BTreeSet<InputId>,
    /// The runtime has a capture backend that adds, replaces, and removes
    /// capture inputs live (IN-001).
    capture_live: bool,
}

impl NativeCatalogueBudget {
    /// Why the native runtime refuses to go from `current` to `candidate`,
    /// or `None` when it follows the change live (and when the catalogue
    /// does not change).
    pub(crate) fn refusal(&self, current: &Project, candidate: &Project) -> Option<CommandRefusal> {
        let before = catalogue_keys(current);
        let after = catalogue_keys(candidate);
        if before == after {
            return None;
        }
        let mut freed = 0_usize;
        for (input, key) in &before {
            let next = after.get(input);
            if next == Some(key) {
                continue;
            }
            if self.decoded.contains(input) {
                return Some(unsupported(format!(
                    "input {input} plays a Stinger clip decoded since startup; its source cannot \
                     change while the native media runtime runs"
                )));
            }
            if matches!(key, NativeSourceKey::Device(_)) && !self.capture_live {
                return Some(unsupported(format!(
                    "capture input {input} cannot be removed or replaced while the native media \
                     runtime runs; stop the daemon and use freemix-cli"
                )));
            }
            if next.is_none() && self.sources.contains(input) {
                freed += 1;
            }
        }
        let mut added = 0_usize;
        for (input, key) in &after {
            if before.get(input) == Some(key) {
                continue;
            }
            if let Some(reason) = key.unsupported_live(*input, self.capture_live) {
                return Some(unsupported(reason));
            }
            if key.has_video_source() && !self.sources.contains(input) {
                added += 1;
            }
        }
        let plan = match NativeProjectPlan::compile(candidate, NativeProjectLimits::default()) {
            Ok(plan) => plan,
            Err(error) => {
                return Some(unsupported(format!(
                    "the native media runtime cannot compose the changed inputs: {error}"
                )));
            }
        };
        let kept = self.sources.len().saturating_sub(freed);
        let kept_bytes = self
            .retained_bytes
            .saturating_sub(self.frame_bytes.saturating_mul(freed as u64));
        let headroom = self
            .max_retained_bytes
            .saturating_sub(kept_bytes)
            .checked_div(self.frame_bytes)
            .unwrap_or(u64::MAX);
        let capacity = self
            .max_sources
            .min(kept.saturating_add(usize::try_from(headroom).unwrap_or(usize::MAX)));
        if plan.input_count() > NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS {
            return Some(bounds(NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS));
        }
        if kept.saturating_add(added) > capacity {
            return Some(bounds(capacity));
        }
        None
    }
}

fn unsupported(message: String) -> CommandRefusal {
    CommandRefusal {
        code: RejectionCode::Unavailable,
        retryable: false,
        message,
        reason: Some(RefusalReason::RuntimeUnsupported),
    }
}

fn bounds(max: usize) -> CommandRefusal {
    CommandRefusal {
        code: RejectionCode::ResourceExhausted,
        retryable: false,
        message: format!(
            "the native media runtime realizes at most {max} input sources at the project's \
             picture size"
        ),
        reason: Some(RefusalReason::BoundsExceeded {
            field: "inputs".to_owned(),
            max: max as u64,
        }),
    }
}

/// A still image an input added live decodes off the loop.
pub(crate) struct PendingStill {
    input: InputId,
    result: mpsc::Receiver<Result<CpuVideoFrame, EngineMediaFault>>,
}

impl NativeDaemon {
    /// What a catalogue change may use of this runtime now.
    pub(crate) fn catalogue_budget(&self) -> NativeCatalogueBudget {
        let registry = self
            .playback
            .as_ref()
            .expect("the video realizer holds the playback")
            .registry();
        let limits = registry.limits();
        let (width, height) = self.picture_size;
        NativeCatalogueBudget {
            sources: registry.inputs().collect(),
            max_sources: limits.max_media_inputs,
            retained_bytes: registry.retained_rgba16f_bytes(),
            max_retained_bytes: limits.max_retained_rgba16f_bytes,
            frame_bytes: u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(8)
                .max(1),
            decoded: self.audio.decoded_inputs().unwrap_or_default(),
            capture_live: self.captures.is_some(),
        }
    }

    /// Follows the input catalogue of `project` (the project as the last
    /// command left it); see the module documentation. Does nothing when
    /// the catalogue is unchanged.
    ///
    /// # Errors
    ///
    /// Fails (stopping the daemon) only when a vetted catalogue cannot be
    /// realized: the plan does not compile, a source exceeds its bound, or
    /// the Master cannot take a strip.
    #[allow(clippy::too_many_lines)] // one pass: retire, start, recompile
    pub(crate) fn sync_input_catalogue(
        &mut self,
        control: &mut ControlService<Policy>,
        project: &Project,
    ) -> AppResult<()> {
        let keys = catalogue_keys(project);
        if keys == self.catalogue {
            return Ok(());
        }
        let plan = NativeProjectPlan::compile(project, NativeProjectLimits::default())?;
        let removed = self
            .catalogue
            .keys()
            .copied()
            .filter(|input| !keys.contains_key(input))
            .collect::<Vec<_>>();
        let replaced = keys
            .iter()
            .filter(|(input, key)| self.catalogue.get(input).is_some_and(|old| old != *key))
            .map(|(input, _)| *input)
            .collect::<BTreeSet<_>>();
        let started = keys
            .iter()
            .filter(|(input, key)| self.catalogue.get(input) != Some(*key))
            .map(|(input, key)| (*input, key.clone()))
            .collect::<Vec<_>>();

        // A media list moving to an item of another file (IN-006) holds the
        // last picture of the item before until the new file presents, so
        // the swap never flashes black on air.
        let held = replaced
            .iter()
            .filter(|input| project.media_list(**input).is_some())
            .filter_map(|input| {
                self.playback
                    .as_ref()
                    .expect("the video realizer holds the playback")
                    .latest_frames()
                    .get(*input)
                    .map(|frame| (*input, frame))
            })
            .collect::<BTreeMap<_, _>>();
        let mut resolved = self.resolved_sources.as_ref().clone();
        for input in removed.iter().chain(&replaced) {
            self.files.remove(*input);
            self.stills.retain(|still| still.input != *input);
            self.video.forget_slide(*input).map_err(AppFailure)?;
            resolved.retain(|source| source.input() != *input);
            eprintln!("FREEMIXD_INPUT\tv=1\tinput={input}\tretired");
        }
        for input in &removed {
            self.playback
                .as_mut()
                .expect("the video realizer holds the playback")
                .remove_source(*input);
        }

        let settings = project.settings();
        let dimensions = settings.video.dimensions;
        let size = (dimensions.width(), dimensions.height());
        let blank = || {
            native_files::timing_at(0, settings.frame_rate, native_clock_domain())
                .and_then(|timing| native_files::blank_picture(size, timing))
                .ok_or_else(|| AppFailure("a live input's first picture cannot be timed".into()))
        };
        let mut pushed = Vec::new();
        for (input, key) in started {
            let closed = project.input_lifecycle(input).closed;
            let seed = match &key {
                NativeSourceKey::Scene { .. } => None,
                NativeSourceKey::Generator(simulated) => {
                    let frame = native_generator_frame(input, simulated, settings)?;
                    resolved.push(NativeResolvedSource::RetainedFrame {
                        input,
                        frame: frame.clone(),
                    });
                    Some(frame)
                }
                NativeSourceKey::Media(asset_uri) => {
                    let playback = project.media_playback(input).copied().unwrap_or_default();
                    if replaced.contains(&input) && control.media_source_open(input) {
                        // The new file opens its own transport source.
                        control.set_media_source(input, None).map_err(|error| {
                            AppFailure(format!("media input {input} cannot close: {error}"))
                        })?;
                    }
                    let path = self.asset_path(asset_uri);
                    if path.as_ref().is_some_and(|path| is_still(path)) {
                        self.stills.push(start_still(
                            input,
                            self.assets_root.clone(),
                            asset_uri.clone(),
                            dimensions,
                            fm_types::OutputScaling::Fit,
                        )?);
                        Some(blank()?)
                    } else {
                        let path = path.unwrap_or_else(|| self.assets_root.join(asset_uri));
                        let assets_root = self.assets_root.clone();
                        let seed = self
                            .files
                            .add_live(
                                native_files::MediaFileInput {
                                    input,
                                    path: path.clone(),
                                    settings: playback,
                                    closed,
                                },
                                || {
                                    Adapter::new(FfmpegConfig {
                                        allowed_root: Some(assets_root),
                                        ..FfmpegConfig::default()
                                    })
                                    .ok()
                                },
                            )
                            .map_err(AppFailure)?;
                        let seed = held
                            .get(&input)
                            .and_then(|frame| native_files::redated(frame, seed.timing()))
                            .unwrap_or(seed);
                        resolved.push(NativeResolvedSource::LocalVideo { input, path });
                        pushed.push(input);
                        Some(seed)
                    }
                }
                NativeSourceKey::Device(_) if self.captures.is_some() => {
                    // The capture supervisor starts when the captures
                    // follow the project (IN-001).
                    let frame = native_streams::startup_picture(
                        size,
                        settings.frame_rate,
                        native_clock_domain(),
                    )
                    .ok_or_else(|| {
                        AppFailure(format!("native capture input {input} cannot be timed"))
                    })?;
                    resolved.push(NativeResolvedSource::LiveFrame {
                        input,
                        frame: frame.clone(),
                    });
                    pushed.push(input);
                    Some(frame)
                }
                // A still image decodes off the loop and is uploaded once
                // when it arrives (IN-008).
                NativeSourceKey::Image(asset_uri, fit) => {
                    if self.asset_path(asset_uri).is_some() {
                        self.stills.push(start_still(
                            input,
                            self.assets_root.clone(),
                            asset_uri.clone(),
                            dimensions,
                            *fit,
                        )?);
                    } else {
                        eprintln!("FREEMIXD_INPUT\tv=1\tinput={input}\tunrealized");
                    }
                    Some(blank()?)
                }
                // A slideshow composes its slides into its source each frame
                // they change (IN-009).
                NativeSourceKey::Slideshow(_) => {
                    self.video.forget_slide(input).map_err(AppFailure)?;
                    let frame = blank()?;
                    resolved.push(NativeResolvedSource::LiveFrame {
                        input,
                        frame: frame.clone(),
                    });
                    Some(frame)
                }
                // A document's page worker ingests its page once rendered
                // (IN-014); a screen capture its captures (IN-011).
                NativeSourceKey::Network
                | NativeSourceKey::Replay
                | NativeSourceKey::SystemAudio
                | NativeSourceKey::Document(_)
                | NativeSourceKey::ScreenCapture => {
                    let frame = native_streams::startup_picture(
                        size,
                        settings.frame_rate,
                        native_clock_domain(),
                    )
                    .ok_or_else(|| {
                        AppFailure(format!("native network input {input} cannot be timed"))
                    })?;
                    resolved.push(NativeResolvedSource::LiveFrame {
                        input,
                        frame: frame.clone(),
                    });
                    pushed.push(input);
                    Some(frame)
                }
                // Vetted away before acceptance; an input that still
                // arrives shows nothing rather than another picture.
                NativeSourceKey::Title
                | NativeSourceKey::Device(_)
                | NativeSourceKey::Color
                | NativeSourceKey::Alias
                | NativeSourceKey::MixFeed => {
                    eprintln!("FREEMIXD_INPUT\tv=1\tinput={input}\tunrealized");
                    Some(blank()?)
                }
            };
            if let Some(seed) = seed {
                self.runtime
                    .insert_live_source_blocking(
                        self.playback
                            .as_mut()
                            .expect("the video realizer holds the playback"),
                        input,
                        seed,
                    )
                    .map_err(|error| {
                        AppFailure(format!("native input {input} cannot start: {error}"))
                    })?;
            } else if replaced.contains(&input) {
                self.playback
                    .as_mut()
                    .expect("the video realizer holds the playback")
                    .remove_source(input);
            }
            eprintln!("FREEMIXD_INPUT\tv=1\tinput={input}\tstarted");
        }

        self.audio
            .follow_catalogue(&plan, &replaced, &pushed)
            .map_err(|error| AppFailure(format!("native Master cannot follow inputs: {error}")))?;
        if let Some(mutation) = self.pending_stinger_mutation.as_mut() {
            mutation.project_plan = plan.clone();
        }
        self.project_plan = plan;
        self.resolved_sources = Arc::new(resolved);
        self.catalogue = keys;
        self.invalidate_projection();
        Ok(())
    }

    /// Installs every still image decoded since the last call, and faults
    /// each input whose image did not decode.
    pub(crate) fn apply_live_stills(
        &mut self,
        control: &mut ControlService<Policy>,
    ) -> AppResult<()> {
        let mut index = 0;
        while index < self.stills.len() {
            let outcome = match self.stills[index].result.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => Err(EngineMediaFault::DecodeFailed),
            };
            let input = self.stills.swap_remove(index).input;
            match outcome {
                Ok(frame) => {
                    self.runtime
                        .insert_live_source_blocking(
                            self.playback
                                .as_mut()
                                .expect("the video realizer holds the playback"),
                            input,
                            frame.clone(),
                        )
                        .map_err(|error| {
                            AppFailure(format!("native still input {input} cannot start: {error}"))
                        })?;
                    let mut resolved = self.resolved_sources.as_ref().clone();
                    resolved.retain(|source| source.input() != input);
                    resolved.push(NativeResolvedSource::RetainedFrame { input, frame });
                    self.resolved_sources = Arc::new(resolved);
                }
                Err(fault) => {
                    eprintln!(
                        "FREEMIXD_MEDIA\tv=1\tinput={input}\tstartup_fault={}",
                        fault.token()
                    );
                    control.set_media_fault(input, fault).map_err(|error| {
                        AppFailure(format!("media input {input} cannot fault: {error}"))
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Opens every media list's next item ahead of need (IN-006): the item
    /// Auto Next or Next steps to from the active one ([`next_list_item`]),
    /// when it is another file (an item of the file already open re-cues
    /// in place). A still image is recognized on the standby's own thread
    /// (reading the file never blocks the loop) and holds nothing. A closed
    /// list prefetches nothing; a list that no longer has a next item drops
    /// its standby.
    pub(crate) fn sync_media_list_prefetch(&mut self, project: &Project) {
        let mut wanted = BTreeSet::new();
        for input in project.inputs() {
            let (Some(list), Some(settings)) = (
                project.media_list(input.id),
                project.media_playback(input.id),
            ) else {
                continue;
            };
            if project.input_lifecycle(input.id).closed {
                continue;
            }
            let Some(next) = next_list_item(list) else {
                continue;
            };
            let current = match &input.kind {
                InputKind::Media { asset_uri, .. } => Some(asset_uri.as_str()),
                _ => None,
            };
            if current == Some(next.asset_uri.as_str()) {
                continue;
            }
            let Some(path) = self.asset_path(&next.asset_uri) else {
                continue;
            };
            wanted.insert(input.id);
            let mut settings = *settings;
            settings.trim_in = next.mark_in;
            settings.trim_out = next.mark_out;
            settings.start_frame = next.mark_in;
            let assets_root = self.assets_root.clone();
            self.files.prefetch(input.id, path, &settings, || {
                Adapter::new(FfmpegConfig {
                    allowed_root: Some(assets_root),
                    ..FfmpegConfig::default()
                })
                .ok()
            });
        }
        self.files
            .retain_prefetched(|input| wanted.contains(&input));
    }

    /// Switches every media list whose accepted command moved it to the
    /// item its standby opened (IN-006) before the command's frame is
    /// projected: the input's worker takes the cued player, the engine
    /// opens the transport on it (playing when the list plays), and the
    /// catalogue records the new file, so [`Self::sync_input_catalogue`]
    /// does not open it again. The switch frame then presents the new
    /// item's in mark instead of holding the item before while the file
    /// opens. A standby still opening leaves the switch to the catalogue,
    /// as does a worker that cannot start; a source the engine refuses
    /// faults the input. Never fails.
    pub(crate) fn adopt_prefetched_media(&mut self, control: &mut ControlService<Policy>) {
        let candidates = self.files.prefetched_inputs();
        for input in candidates {
            let Some(asset_uri) = control.media_asset(input).map(str::to_owned) else {
                continue;
            };
            let Some(NativeSourceKey::Media(current)) = self.catalogue.get(&input) else {
                continue;
            };
            if *current == asset_uri || control.input_lifecycle(input).closed {
                continue;
            }
            let (Some(path), Some(settings)) =
                (self.asset_path(&asset_uri), control.media_settings(input))
            else {
                continue;
            };
            let Some(prefetched) = self.files.take_prefetched(input, &path, &settings) else {
                continue;
            };
            let source = prefetched.source.clone();
            // A takeover that fails never stops the daemon: a worker that
            // cannot start leaves the switch to the catalogue (which opens
            // the file the slow way), and a source the engine refuses
            // faults the input there, as a file that does not open does.
            if let Err(error) = self.files.adopt(input, path.clone(), prefetched) {
                eprintln!("FREEMIXD_MEDIA\tv=1\tinput={input}\tprefetch_adopt_failed\t{error}");
                continue;
            }
            self.stills.retain(|still| still.input != input);
            if let Err(error) = control.set_media_source(input, Some(source)) {
                eprintln!("FREEMIXD_MEDIA\tv=1\tinput={input}\tprefetch_refused\t{error}");
            }
            let mut resolved = self.resolved_sources.as_ref().clone();
            resolved.retain(|source| source.input() != input);
            resolved.push(NativeResolvedSource::LocalVideo { input, path });
            self.resolved_sources = Arc::new(resolved);
            self.catalogue
                .insert(input, NativeSourceKey::Media(asset_uri));
            self.invalidate_projection();
            eprintln!("FREEMIXD_MEDIA\tv=1\tinput={input}\tprefetched=1");
        }
    }

    /// The bundle path of media asset `asset_uri`, if it names one.
    fn asset_path(&self, asset_uri: &str) -> Option<PathBuf> {
        bundle_asset_path(&self.assets_root, asset_uri)
    }
}

/// The bundle path of media asset `asset_uri` under `assets_root`, if it
/// names one that does not lead out of the assets: a path that resolves
/// (following links) outside them is `None`, so a link out of the assets is
/// never sniffed, prefetched, or opened. A path that does not resolve (a
/// missing file) is kept, so its input faults as missing and is retried.
fn bundle_asset_path(assets_root: &Path, asset_uri: &str) -> Option<PathBuf> {
    fm_model::validate_media_asset_uri(asset_uri).ok()?;
    let key = asset_uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)?;
    let path = assets_root.join(key);
    if let Ok(resolved) = std::fs::canonicalize(&path) {
        let root = std::fs::canonicalize(assets_root).ok()?;
        if !resolved.starts_with(root) {
            return None;
        }
    }
    Some(path)
}

/// The item a media list steps to from its active one by Auto Next (and,
/// short of the list's end, by Next): the next in play order, else under
/// Loop the first of the next cycle's order, else under Auto First the
/// first; `None` at the end of a list that does neither.
pub(crate) fn next_list_item(list: &fm_model::MediaList) -> Option<&fm_model::MediaListItem> {
    let order = list.play_order();
    let position = list.position().unwrap_or(0);
    let id = if position + 1 < order.len() {
        order[position + 1]
    } else if list.options.loop_list {
        let mut next = list.clone();
        next.cycle = next.cycle.wrapping_add(1);
        *next.play_order().first()?
    } else if list.options.auto_first {
        *order.first()?
    } else {
        return None;
    };
    list.item(id)
}

/// Whether the file at `path` starts like a still image (one read of its
/// first bytes; a file that cannot be read plays, and faults, as a media
/// file).
pub(crate) fn is_still(path: &std::path::Path) -> bool {
    let mut prefix = Vec::with_capacity(fm_codec_image::STILL_SNIFF_BYTES);
    File::open(path)
        .and_then(|file| {
            file.take(fm_codec_image::STILL_SNIFF_BYTES as u64)
                .read_to_end(&mut prefix)
        })
        .is_ok()
        && sniff_still_format(&prefix).is_ok()
}

/// Decodes the still image `asset_uri` of the bundle whose assets are at
/// `assets_root` on its own thread, placed in the show raster with `fit`.
fn start_still(
    input: InputId,
    assets_root: PathBuf,
    asset_uri: String,
    dimensions: fm_types::VideoDimensions,
    fit: fm_types::OutputScaling,
) -> AppResult<PendingStill> {
    let (sender, result) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!("freemix-still-{input}"))
        .spawn(move || {
            // Resolved like every other bundle asset: a link out of the
            // assets never decodes.
            let decoded =
                crate::stills::resolve_picture_path(&assets_root, &asset_uri).and_then(|path| {
                    crate::stills::decode_picture_file(
                        &path,
                        dimensions,
                        fit,
                        native_clock_domain(),
                    )
                });
            let _ = sender.send(decoded);
        })?;
    Ok(PendingStill { input, result })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_model::{Input, ProjectSettings, SimulatedVideo, SolidColor};
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };

    use super::*;

    fn id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn generator(audio: SimulatedAudio) -> InputKind {
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
            audio,
        ))
    }

    fn project(inputs: &[(u128, InputKind)]) -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(7).unwrap()),
            "Catalogue",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(320, 180).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::F32,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        for (input, kind) in inputs {
            project.add_input(Input {
                id: id(*input),
                name: format!("Input {input}"),
                kind: kind.clone(),
                required_capabilities: Vec::new(),
            });
        }
        project
    }

    /// Room for `capacity` live 320x180 frames, `used` of them taken.
    fn budget(used: &[u128], capacity: u64) -> NativeCatalogueBudget {
        let frame_bytes = 320 * 180 * 8;
        NativeCatalogueBudget {
            sources: used.iter().map(|input| id(*input)).collect(),
            max_sources: NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS,
            retained_bytes: frame_bytes * used.len() as u64,
            max_retained_bytes: frame_bytes * capacity,
            frame_bytes,
            decoded: BTreeSet::new(),
            capture_live: false,
        }
    }

    fn reason(refusal: Option<CommandRefusal>) -> Option<RefusalReason> {
        refusal.and_then(|refusal| refusal.reason)
    }

    /// A still added live resolves like every bundle asset: a link out of
    /// the assets never decodes (IN-008).
    #[cfg(unix)]
    #[test]
    fn a_live_still_never_decodes_through_a_link_out_of_the_assets() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), b"not really a png").unwrap();
        let assets = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.png"),
            assets.path().join("escape.png"),
        )
        .unwrap();
        let pending = start_still(
            id(1),
            assets.path().to_path_buf(),
            "asset://escape.png".to_owned(),
            VideoDimensions::new(16, 9).unwrap(),
            fm_types::OutputScaling::Fit,
        )
        .unwrap();
        assert_eq!(
            pending.result.recv().unwrap(),
            Err(EngineMediaFault::SourceMissing)
        );
    }

    /// A media asset that resolves out of the assets, through a link to a
    /// file or to a folder, has no bundle path; one inside them (a link
    /// included) and a missing one keep theirs.
    #[cfg(unix)]
    #[test]
    fn a_media_link_out_of_the_assets_has_no_bundle_path() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.mkv"), b"host file").unwrap();
        let assets = tempfile::tempdir().unwrap();
        std::fs::write(assets.path().join("clip.mkv"), b"clip").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.mkv"),
            assets.path().join("escape.mkv"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), assets.path().join("host")).unwrap();
        std::os::unix::fs::symlink(
            assets.path().join("clip.mkv"),
            assets.path().join("alias.mkv"),
        )
        .unwrap();
        for uri in ["asset://escape.mkv", "asset://host/secret.mkv"] {
            assert_eq!(bundle_asset_path(assets.path(), uri), None, "{uri}");
        }
        for (uri, file) in [
            ("asset://clip.mkv", "clip.mkv"),
            ("asset://alias.mkv", "alias.mkv"),
            ("asset://gone.mkv", "gone.mkv"),
        ] {
            assert_eq!(
                bundle_asset_path(assets.path(), uri),
                Some(assets.path().join(file)),
                "{uri}"
            );
        }
    }

    #[test]
    fn common_kinds_change_live_and_an_unchanged_catalogue_is_not_vetted() {
        let silent = generator(SimulatedAudio::Silence);
        let current = project(&[(1, silent.clone())]);
        let budget = budget(&[1], 8);
        assert!(budget.refusal(&current, &current).is_none());
        let media = InputKind::Media {
            asset_uri: "asset://media/clip.mkv".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        };
        let network = InputKind::NetworkStream(fm_types::network_stream::NetworkStreamConfig::new(
            "udp://127.0.0.1:5000",
        ));
        for added in [silent.clone(), media.clone(), network] {
            let candidate = project(&[(1, silent.clone()), (2, added.clone())]);
            assert!(budget.refusal(&current, &candidate).is_none(), "{added:?}");
        }
        // A system audio input is added live where the platform captures
        // system audio, and refused with the typed reason elsewhere.
        let system_audio = InputKind::SystemAudio(fm_types::system_audio::SystemAudioConfig::new(
            fm_types::system_audio::SystemAudioBackend::Pulse,
            "@DEFAULT_MONITOR@",
        ));
        let candidate = project(&[(1, silent.clone()), (2, system_audio)]);
        assert_eq!(
            reason(budget.refusal(&current, &candidate)),
            (!crate::native_system_audio::SUPPORTED).then_some(RefusalReason::RuntimeUnsupported)
        );
        // A replacement and a removal free what they leave.
        assert!(budget.refusal(&current, &project(&[(1, media)])).is_none());
        assert!(budget.refusal(&current, &project(&[])).is_none());
    }

    #[test]
    fn kinds_the_runtime_does_not_realize_live_are_refused_with_the_typed_reason() {
        let current = project(&[(1, generator(SimulatedAudio::Silence))]);
        let budget = budget(&[1], 8);
        for added in [
            generator(SimulatedAudio::Sine { frequency_hz: 440 }),
            InputKind::Device {
                stable_key: "camera".into(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
            InputKind::Color,
            InputKind::Alias {
                target: id(1),
                crop: None,
            },
        ] {
            let candidate = project(&[(1, generator(SimulatedAudio::Silence)), (2, added.clone())]);
            assert_eq!(
                reason(budget.refusal(&current, &candidate)),
                Some(RefusalReason::RuntimeUnsupported),
                "{added:?}"
            );
        }
        // A capture input is never retired live either, nor a Stinger clip
        // the Master decodes.
        let camera = project(&[(
            1,
            InputKind::Device {
                stable_key: "camera".into(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
        )]);
        assert_eq!(
            reason(budget.refusal(&camera, &project(&[]))),
            Some(RefusalReason::RuntimeUnsupported)
        );
        // With a capture backend, capture inputs are added, replaced, and
        // removed live.
        let mut live = budget.clone();
        live.capture_live = true;
        assert!(live.refusal(&camera, &project(&[])).is_none());
        let with_camera = project(&[
            (1, generator(SimulatedAudio::Silence)),
            (
                2,
                InputKind::Device {
                    stable_key: "v4l2:usb-1#Cam".into(),
                    capture: fm_types::capture::CaptureSettings::default(),
                },
            ),
        ]);
        assert!(live.refusal(&current, &with_camera).is_none());
        let mut decoded = budget;
        decoded.decoded.insert(id(1));
        assert_eq!(
            reason(decoded.refusal(&current, &project(&[]))),
            Some(RefusalReason::RuntimeUnsupported)
        );
    }

    #[test]
    fn a_catalogue_past_the_retained_frame_budget_is_refused_as_bounded() {
        let silent = generator(SimulatedAudio::Silence);
        let current = project(&[(1, silent.clone()), (2, silent.clone())]);
        let full = budget(&[1, 2], 2);
        let grown = project(&[
            (1, silent.clone()),
            (2, silent.clone()),
            (3, silent.clone()),
        ]);
        let refusal = full.refusal(&current, &grown).expect("refused");
        assert_eq!(refusal.code, RejectionCode::ResourceExhausted);
        assert_eq!(
            refusal.reason,
            Some(RefusalReason::BoundsExceeded {
                field: "inputs".into(),
                max: 2,
            })
        );
        // Removing one makes room for another in the same command.
        let swapped = project(&[(1, silent.clone()), (3, silent)]);
        assert!(full.refusal(&current, &swapped).is_none());
    }

    /// The prefetched item is the one Auto Next steps to: the next in play
    /// order, then the next cycle's first under Loop, the first under Auto
    /// First, and nothing at the end of a list that does neither.
    #[test]
    fn the_next_list_item_follows_auto_next() {
        let items = (1..=3)
            .map(|id| fm_model::MediaListItem::new(id, format!("asset://{id}.mkv")))
            .collect();
        let mut list =
            fm_model::MediaList::new(items, fm_model::MediaListOptions::default()).unwrap();
        let next = |list: &fm_model::MediaList| next_list_item(list).map(|item| item.id);
        assert_eq!(next(&list), Some(2));
        list.active = 3;
        assert_eq!(next(&list), None);
        list.options.auto_first = true;
        assert_eq!(next(&list), Some(1));
        list.options.loop_list = true;
        list.options.shuffle_seed = Some(7);
        let final_item = *list.play_order().last().unwrap();
        list.active = final_item;
        let mut wrapped = list.clone();
        wrapped.cycle += 1;
        assert_eq!(next(&list), Some(wrapped.play_order()[0]));
    }
}
