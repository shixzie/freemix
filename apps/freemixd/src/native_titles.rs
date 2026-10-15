//! Native realization of title inputs.
//!
//! Every title input of a native project is a live source frame: the
//! `fm_titles` reference renderer draws the title (see [`title_render`]) at
//! its template size, the picture is scaled nearest-neighbor to the project
//! video dimensions, converted from the renderer's premultiplied RGBA8 to
//! the straight-alpha, full-range sRGB BT.709 RGBA8 the native normalizer
//! imports, and held by the native source registry as the input's latest
//! frame. The template's transparency survives, so a title works as a full
//! frame source (over the compositor's black) and as an overlay or Scene
//! layer.
//!
//! The first frame of each title renders synchronously at startup, before
//! readiness, so the source registry is seeded exactly like a still. After
//! that rendering never runs on the session loop: [`NativeTitleSources`]
//! owns one worker thread, the loop only hands it changed title state
//! ([`NativeTitleSources::observe`], a value comparison) and collects
//! finished frames ([`NativeTitleSources::take_completed`]) for ingestion.
//! Handoff is latest-wins in both directions: a title changed twice before
//! the worker sees it renders once, and a finished frame not yet ingested is
//! replaced by a newer one, so nothing queues.
//!
//! A title re-renders when its persisted state (template, field revision, or
//! field values) changes. A time-dependent template (a clock or ticker
//! element) also re-renders on a bounded refresh: at most once per
//! project frame interval and never more often than every
//! [`MIN_ANIMATED_TITLE_REFRESH`] (25 Hz), on the one worker. A render
//! slower than that just lowers the refresh rate; it never delays a frame.
//! Scene time is the most recent frame cursor the loop published times the
//! project frame duration, the same approximation the simulated monitor tap
//! documents: every title is treated as having started at frame zero.
//!
//! Keyframe animation is frame-accurate. Before each frame is realized the
//! loop hands the worker the pose the engine projects for it
//! ([`fm_engine::FrameResult::titles`]; the Hold pose for any title it does
//! not track) with [`NativeTitleSources::observe_frame`]: a title whose pose
//! changed (every frame of its In or Out phase, and once when it settles)
//! renders that pose at that frame. [`NativeTitleSources::covered`] then
//! holds the frame until every title the engine tracks on an overlay has
//! ingested the pose of exactly that frame, so the first In frame, each
//! phase frame, and the last Out frame go to air as projected. A frame is
//! held at most [`MAX_TITLE_POSE_WAIT`] past its host deadline and then
//! shows the latest pose ingested; such frames are counted. The bound is
//! from the deadline, so a renderer slower than the frame rate costs the
//! output clock at most that bound in total, not on every phase frame.
//!
//! Production clock elements read their template's clocks in the daemon's
//! clock domain (see [`title_render::TitleTime`]): the monotonic clock is the
//! published frame on the project frame grid and time of day reads the
//! system wall clock at the render. A since-take stopwatch counts from the
//! frame a title was first projected on Program (either side of a
//! transition) or on an active overlay channel, which the session loop
//! publishes with [`NativeTitleSources::observe_frame`]; it rests at zero
//! while the title is off air. Production clock templates are
//! time-dependent, so they refresh at the same bounded rate.
//!
//! A re-render that fails (for example a field value the template no longer
//! instantiates) keeps the last frame on air and is counted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_engine::FrameResult;
use fm_frame::{
    AlphaMode, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries, CpuVideoFrame,
    CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    SignalRange, TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_model::{InputKind, Project, TitleInput};
use fm_titles::AnimSample;
use fm_types::{FrameRate, InputId};
use fm_video::ImageFrame;

use crate::title_render::{self, TitleAssets};

/// The pose the engine projects for each title it tracks on `projected`.
fn projected_poses(projected: &FrameResult) -> Vec<(InputId, AnimSample)> {
    projected
        .titles
        .iter()
        .map(|title| (title.input, title.frame.sample))
        .collect()
}

/// Shortest interval between two renders of a time-dependent title.
pub(crate) const MIN_ANIMATED_TITLE_REFRESH: Duration = Duration::from_millis(40);

/// Longest a projected frame waits for an animating title's pose to be
/// rendered and ingested before it is realized with the previous pose.
pub(crate) const MAX_TITLE_POSE_WAIT: Duration = Duration::from_millis(100);

/// How a native title frame is sized and timed.
#[derive(Clone, Copy)]
struct TitleFrameFormat {
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    clock_domain: ClockDomainId,
}

impl TitleFrameFormat {
    fn frame_duration_ns(self) -> u64 {
        let numerator = u128::from(self.frame_rate.numerator()).max(1);
        let nanos = u128::from(self.frame_rate.denominator()) * 1_000_000_000 / numerator;
        u64::try_from(nanos).unwrap_or(u64::MAX).max(1)
    }

    /// Timing of the `sequence`th frame of one title: monotonic PTS and
    /// sequence in the native clock domain, one frame interval long.
    fn timing(self, sequence: u64) -> Option<MediaTiming> {
        let duration = self.frame_duration_ns();
        let pts = i64::try_from(sequence.checked_mul(duration)?).ok()?;
        MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(pts),
                TimeBase::new(1, 1_000_000_000).ok()?,
            ),
            NormalizedTimestamp::from_nanos(pts),
            NormalizedDuration::from_nanos(duration).ok()?,
            self.clock_domain,
            SequenceNumber::new(sequence),
        )
        .ok()
    }

    /// Renders `title` in `pose` at `frame`, on air since `taken_frame`,
    /// into a native source frame.
    fn render(
        self,
        assets: &TitleAssets,
        input: InputId,
        title: &TitleInput,
        (frame, taken_frame, pose): (u64, Option<u64>, AnimSample),
        sequence: u64,
    ) -> Option<CpuVideoFrame> {
        let image = title_render::render_title_at(
            assets,
            input,
            title,
            title_render::TitleTime::now(frame, self.frame_rate, taken_frame, pose),
            self.width,
            self.height,
        )?;
        title_source_frame(&image, self.timing(sequence)?)
    }
}

/// Converts a premultiplied RGBA8 picture into a tightly packed,
/// straight-alpha, full-range sRGB BT.709 RGBA8 source frame.
pub(crate) fn title_source_frame(image: &ImageFrame, timing: MediaTiming) -> Option<CpuVideoFrame> {
    let width = usize::try_from(image.width()).ok()?;
    let row_bytes = width.checked_mul(4)?;
    let mut pixels =
        Vec::with_capacity(row_bytes.checked_mul(usize::try_from(image.height()).ok()?)?);
    for row in image.pixels().chunks(image.stride()) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            pixels.extend_from_slice(&unpremultiply(*pixel));
        }
    }
    let plane = CpuVideoPlane::new(row_bytes, pixels).ok()?;
    let payload = CpuVideoPayload::new(
        PixelFormat::Rgba8,
        VideoDimensions::new(image.width(), image.height())?,
        vec![plane],
    )
    .ok()?;
    CpuVideoFrame::new(timing, payload)
        .with_metadata(VideoFrameMetadata::new(
            ColorMetadata {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Srgb,
                matrix: MatrixCoefficients::Identity,
                range: SignalRange::Full,
                chroma_location: ChromaLocation::Center,
            },
            Some(AlphaMode::Straight),
        ))
        .ok()
}

/// Premultiplied to straight alpha, rounded to nearest; a fully transparent
/// pixel is transparent black.
fn unpremultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    match alpha {
        0 => [0; 4],
        u8::MAX => [red, green, blue, alpha],
        _ => {
            let alpha_wide = u16::from(alpha);
            let channel = |value: u8| {
                let straight = (u16::from(value) * 255 + alpha_wide / 2) / alpha_wide;
                u8::try_from(straight).unwrap_or(u8::MAX)
            };
            [channel(red), channel(green), channel(blue), alpha]
        }
    }
}

/// State shared by the session loop and the title worker.
struct Shared {
    state: Mutex<WorkerState>,
    wake: Condvar,
    /// The engine frame cursor the loop last published.
    frame: AtomicU64,
}

#[derive(Default)]
struct WorkerState {
    /// The latest persisted state of every title input.
    desired: BTreeMap<InputId, TitleInput>,
    /// Titles whose state changed since the worker last rendered them.
    dirty: BTreeSet<InputId>,
    /// Finished frames not yet ingested, latest-wins per input, with the
    /// pose each was rendered in.
    completed: BTreeMap<InputId, (AnimSample, CpuVideoFrame)>,
    /// The frame each title input on air was first projected there.
    taken: BTreeMap<InputId, u64>,
    /// The pose, and the projected frame it is for, each title renders in
    /// next; a title absent renders its Hold pose at the published frame.
    poses: BTreeMap<InputId, (AnimSample, u64)>,
    /// The pose of the frame each title last ingested.
    ingested: BTreeMap<InputId, AnimSample>,
    /// The last projected frame counted in `late_poses`.
    late_frame: Option<u64>,
    renders: u64,
    failed_renders: u64,
    /// Frames realized before an animating title's pose was ingested.
    late_poses: u64,
    shutdown: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The native title sources of one daemon and their render worker.
pub(crate) struct NativeTitleSources {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    /// The state last handed to the worker, compared on the loop.
    submitted: BTreeMap<InputId, TitleInput>,
}

impl NativeTitleSources {
    /// Renders the first frame of every title input of `project` and starts
    /// the render worker when there is any.
    ///
    /// # Errors
    ///
    /// Names the first title input whose template was not loaded or whose
    /// first frame does not render.
    pub(crate) fn start(
        assets: Arc<TitleAssets>,
        project: &Project,
        clock_domain: ClockDomainId,
    ) -> Result<(Self, Vec<(InputId, CpuVideoFrame)>), String> {
        let dimensions = project.settings().video.dimensions;
        let format = TitleFrameFormat {
            width: dimensions.width(),
            height: dimensions.height(),
            frame_rate: project.settings().frame_rate,
            clock_domain,
        };
        let mut seeds = Vec::new();
        let mut submitted = BTreeMap::new();
        let mut animated = BTreeSet::new();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            let loaded = assets.template(&title.template_uri).ok_or_else(|| {
                format!(
                    "native title input {} template `{}` is not loaded",
                    input.id, title.template_uri
                )
            })?;
            if loaded.time_dependent() {
                animated.insert(input.id);
            }
            let frame = format
                .render(&assets, input.id, title, (0, None, AnimSample::Hold), 0)
                .ok_or_else(|| format!("native title input {} did not render", input.id))?;
            seeds.push((input.id, frame));
            submitted.insert(input.id, title.clone());
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(WorkerState {
                desired: submitted.clone(),
                ..WorkerState::default()
            }),
            wake: Condvar::new(),
            frame: AtomicU64::new(0),
        });
        let worker = if submitted.is_empty() {
            None
        } else {
            let refresh =
                Duration::from_nanos(format.frame_duration_ns()).max(MIN_ANIMATED_TITLE_REFRESH);
            let worker_shared = Arc::clone(&shared);
            let sequences = submitted.keys().map(|input| (*input, 0)).collect();
            Some(
                thread::Builder::new()
                    .name("freemix-native-titles".to_owned())
                    .spawn(move || {
                        run_worker(
                            &worker_shared,
                            &assets,
                            format,
                            &animated,
                            sequences,
                            refresh,
                        );
                    })
                    .map_err(|error| format!("native title worker did not start: {error}"))?,
            )
        };
        Ok((
            Self {
                shared,
                worker,
                submitted,
            },
            seeds,
        ))
    }

    /// Hands every title input whose persisted state differs from what the
    /// worker last received to the worker. Never renders.
    pub(crate) fn observe(&mut self, project: &Project) {
        let mut changed = Vec::new();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            if let Some(submitted) = self.submitted.get_mut(&input.id)
                && submitted != title
            {
                submitted.clone_from(title);
                changed.push((input.id, title.clone()));
            }
        }
        if changed.is_empty() {
            return;
        }
        let mut state = self.shared.lock();
        for (input, title) in changed {
            state.desired.insert(input, title);
            state.dirty.insert(input);
        }
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Publishes the engine frame cursor time-dependent titles render at.
    pub(crate) fn set_frame(&self, frame: u64) {
        self.shared.frame.store(frame, Ordering::Relaxed);
    }

    /// Dates every title input this projected frame puts on air (either
    /// Program endpoint or an active overlay channel's source) to its frame,
    /// keeping the date of a title already on air, and forgets every title
    /// it does not show; hands the worker every title whose animation pose
    /// for this frame differs from the one it last rendered. Never renders.
    pub(crate) fn observe_frame(&self, projected: &FrameResult) {
        let program = &projected.program;
        let overlays = projected
            .overlays
            .iter()
            .filter(|channel| channel.is_active() || channel.is_transitioning())
            .filter_map(fm_switcher::OverlayChannelState::source);
        self.observe_on_air(
            projected.frame.get(),
            std::iter::once(program.primary)
                .chain(program.secondary)
                .chain(overlays),
        );
        self.observe_poses(projected.frame.get(), &projected_poses(projected));
    }

    /// Requests `poses` (every other title holds) for `frame`.
    fn observe_poses(&self, frame: u64, poses: &[(InputId, AnimSample)]) {
        if self.worker.is_none() {
            return;
        }
        let pose_of = |input: &InputId| {
            poses
                .iter()
                .find(|(title, _)| title == input)
                .map_or(AnimSample::Hold, |(_, pose)| *pose)
        };
        let mut state = self.shared.lock();
        let mut changed = false;
        for input in self.submitted.keys() {
            let pose = pose_of(input);
            let requested = state
                .poses
                .get(input)
                .map_or(AnimSample::Hold, |(pose, _)| *pose);
            if requested != pose {
                state.poses.insert(*input, (pose, frame));
                state.dirty.insert(*input);
                changed = true;
            }
        }
        drop(state);
        if changed {
            self.shared.wake.notify_one();
        }
    }

    /// Whether `projected`, due at host instant `deadline`, may be realized:
    /// every title the engine tracks on an overlay has ingested the pose of
    /// exactly this frame, or the frame is [`MAX_TITLE_POSE_WAIT`] past its
    /// deadline. The bound is measured from the deadline, not from when the
    /// frame started waiting, so a renderer that cannot keep up delays the
    /// output by at most the bound in total rather than on every frame.
    pub(crate) fn covered(&self, projected: &FrameResult, deadline: Instant) -> bool {
        self.covered_at(projected.frame.get(), &projected_poses(projected), deadline)
    }

    fn covered_at(&self, frame: u64, poses: &[(InputId, AnimSample)], deadline: Instant) -> bool {
        if self.worker.is_none() {
            return true;
        }
        let mut state = self.shared.lock();
        let pending = poses.iter().any(|(input, pose)| {
            self.submitted.contains_key(input)
                && state
                    .ingested
                    .get(input)
                    .map_or(AnimSample::Hold, |ingested| *ingested)
                    != *pose
        });
        if !pending {
            return true;
        }
        if Instant::now() < deadline + MAX_TITLE_POSE_WAIT {
            return false;
        }
        // Count each frame realized with a stale pose once.
        if state.late_frame != Some(frame) {
            state.late_frame = Some(frame);
            state.late_poses = state.late_poses.saturating_add(1);
        }
        true
    }

    fn observe_on_air(&self, frame: u64, on_air: impl Iterator<Item = InputId>) {
        if self.worker.is_none() {
            return;
        }
        let on_air = on_air
            .filter(|input| self.submitted.contains_key(input))
            .collect::<BTreeSet<_>>();
        let mut state = self.shared.lock();
        state.taken.retain(|input, _| on_air.contains(input));
        for input in on_air {
            state.taken.entry(input).or_insert(frame);
        }
    }

    /// Takes every finished frame not yet ingested, in input order, noting
    /// the pose each shows.
    pub(crate) fn take_completed(&self) -> Vec<(InputId, CpuVideoFrame)> {
        if self.worker.is_none() {
            return Vec::new();
        }
        let mut state = self.shared.lock();
        let completed = std::mem::take(&mut state.completed);
        completed
            .into_iter()
            .map(|(input, (pose, frame))| {
                state.ingested.insert(input, pose);
                (input, frame)
            })
            .collect()
    }

    /// Frames realized before an animating title's pose was ingested.
    #[cfg(test)]
    pub(crate) fn late_poses(&self) -> u64 {
        self.shared.lock().late_poses
    }

    /// The pose of the frame `input` last ingested.
    #[cfg(test)]
    pub(crate) fn ingested_pose(&self, input: InputId) -> Option<AnimSample> {
        self.shared.lock().ingested.get(&input).copied()
    }

    /// Worker renders so far, successful and failed.
    #[cfg(test)]
    pub(crate) fn render_counts(&self) -> (u64, u64) {
        let state = self.shared.lock();
        (state.renders, state.failed_renders)
    }

    /// The take frame of every title on air.
    #[cfg(test)]
    pub(crate) fn taken_frames(&self) -> Vec<(InputId, u64)> {
        self.shared
            .lock()
            .taken
            .iter()
            .map(|(input, frame)| (*input, *frame))
            .collect()
    }
}

impl Drop for NativeTitleSources {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            // Bounded by one in-flight render.
            let _ = worker.join();
        }
    }
}

fn run_worker(
    shared: &Shared,
    assets: &TitleAssets,
    format: TitleFrameFormat,
    animated: &BTreeSet<InputId>,
    mut sequences: BTreeMap<InputId, u64>,
    refresh: Duration,
) {
    let mut next_refresh = Instant::now() + refresh;
    // Each failing title is reported once until it renders again, so a
    // failing time-dependent title cannot flood the log.
    let mut failing = BTreeSet::new();
    loop {
        let work = {
            let mut state = shared.lock();
            let refresh_due = loop {
                if state.shutdown {
                    return;
                }
                let now = Instant::now();
                let refresh_due = !animated.is_empty() && now >= next_refresh;
                if refresh_due || !state.dirty.is_empty() {
                    break refresh_due;
                }
                state = if animated.is_empty() {
                    shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner)
                } else {
                    shared
                        .wake
                        .wait_timeout(state, next_refresh.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                };
            };
            let mut inputs = std::mem::take(&mut state.dirty);
            if refresh_due {
                inputs.extend(animated.iter().copied());
                next_refresh = Instant::now() + refresh;
            }
            let published = shared.frame.load(Ordering::Relaxed);
            inputs
                .into_iter()
                .filter_map(|input| {
                    let title = state.desired.get(&input)?.clone();
                    // A pose mid-phase renders at the frame it is for; a
                    // holding title at the published frame.
                    let (pose, frame) = match state.poses.get(&input) {
                        Some((pose, frame)) if *pose != AnimSample::Hold => (*pose, *frame),
                        _ => (AnimSample::Hold, published),
                    };
                    Some((
                        input,
                        title,
                        (frame, state.taken.get(&input).copied(), pose),
                    ))
                })
                .collect::<Vec<_>>()
        };
        for (input, title, at) in work {
            let Some(sequence) = sequences.get_mut(&input) else {
                continue;
            };
            *sequence = sequence.saturating_add(1);
            let rendered = format.render(assets, input, &title, at, *sequence);
            let mut state = shared.lock();
            state.renders = state.renders.saturating_add(1);
            if let Some(rendered) = rendered {
                state.completed.insert(input, (at.2, rendered));
                failing.remove(&input);
            } else {
                state.failed_renders = state.failed_renders.saturating_add(1);
                if failing.insert(input) {
                    eprintln!(
                        "native title input {input} did not re-render; its last frame stays on air"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_model::{Input, ProjectSettings, TitleFieldEntry, TitleFieldId, TitleFieldValue};
    use fm_titles::{
        AssetCatalog, Bounds, ClockDirection, ClockFormat, ClockSpec, Color, Element, ElementId,
        ElementKind, FieldDefinition, FieldId, FieldValue, FontStyle, Style, TemplateId,
        TitleTemplate,
    };
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata as ProjectColor, PixelFormat as ProjectPixels,
        ProjectId, SampleFormat, SampleRate, ScanMode, VideoDimensions as ProjectDimensions,
        VideoFormat,
    };

    use super::*;
    use crate::title_render::LoadedTitleTemplate;

    const BAR_URI: &str = "asset://titles/bar.fmtitle";
    const CLOCK_URI: &str = "asset://titles/clock.fmtitle";
    const FADE_URI: &str = "asset://titles/fade.fmtitle";
    const WAIT: Duration = Duration::from_secs(10);

    fn field(value: u128) -> FieldId {
        FieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn element(id: u128, kind: ElementKind, font: Option<FontStyle>) -> Element {
        Element {
            id: ElementId::new(NonZeroU128::new(id).unwrap()),
            name: format!("Element {id}"),
            bounds: Bounds {
                x: 16,
                y: 8,
                width: 32,
                height: 16,
            },
            z_index: 0,
            visible: true,
            style: Style {
                font,
                ..Style::default()
            },
            color_field: None,
            kind,
        }
    }

    /// A 160x90 transparent template with one rectangle at (16, 8) sized
    /// 32x16, tinted by color field 1. With `clock`, a font-less scene clock
    /// makes it time-dependent without changing its picture.
    fn template(clock: bool) -> TitleTemplate {
        let mut bar = element(1, ElementKind::Rectangle, None);
        bar.color_field = Some(field(1));
        let mut elements = vec![bar];
        if clock {
            elements.push(element(
                2,
                ElementKind::Clock(ClockSpec {
                    direction: ClockDirection::CountUp,
                    start_value_ms: 0,
                    starts_at_ms: 0,
                    format: ClockFormat::MinutesSeconds,
                }),
                Some(FontStyle {
                    family: "Unavailable".into(),
                    size_px: 12,
                }),
            ));
        }
        TitleTemplate {
            id: TemplateId::new(NonZeroU128::new(1).unwrap()),
            name: "Bar".into(),
            width: 160,
            height: 90,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field(1),
                name: "Bar".into(),
                default: FieldValue::Color(Color::new(1, 2, 3, 255)),
            }],
            elements,
            animation: fm_titles::TitleAnimation::default(),
        }
    }

    fn assets() -> Arc<TitleAssets> {
        let mut assets = TitleAssets::default();
        assets.insert(
            BAR_URI,
            LoadedTitleTemplate::new(template(false), AssetCatalog::new()),
        );
        assets.insert(
            CLOCK_URI,
            LoadedTitleTemplate::new(template(true), AssetCatalog::new()),
        );
        assets.insert(
            FADE_URI,
            LoadedTitleTemplate::new(fade_template(), AssetCatalog::new()),
        );
        Arc::new(assets)
    }

    /// The bar fading in: opacity 0 -> 255 over a ten-frame In phase.
    fn fade_template() -> TitleTemplate {
        use fm_titles::fm_animation::{Easing, Keyframe, Phase, Property, Track, TrackKey, Value};

        let mut template = template(false);
        let bar = template.elements[0].id;
        template
            .edit_animation(|clip| {
                clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
                clip.set_track(Track::new(
                    TrackKey::new(bar, Phase::In, Property::Opacity),
                    vec![
                        Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                        Keyframe::new(10, Value::Scalar(255), Easing::Linear),
                    ],
                ))
            })
            .unwrap();
        template
    }

    /// The alpha of the 320x180 frame inside the scaled bar.
    fn bar_alpha(frame: &CpuVideoFrame) -> u8 {
        let plane = frame.payload().plane(0).unwrap();
        plane.bytes()[20 * plane.stride() + 40 * 4 + 3]
    }

    fn title(uri: &str, revision: u64, bar: [u8; 4]) -> TitleInput {
        TitleInput {
            template_uri: uri.into(),
            field_revision: revision,
            fields: vec![TitleFieldEntry {
                id: TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                value: TitleFieldValue::Color(fm_model::Rgba8::new(bar[0], bar[1], bar[2], bar[3])),
            }],
        }
    }

    /// A 320x180 project at 25 fps whose titles are the given inputs.
    fn project(titles: &[(u128, TitleInput)]) -> Project {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Titles",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: ProjectDimensions::new(320, 180).unwrap(),
                    frame_rate,
                    pixel_format: ProjectPixels::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ProjectColor::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::I16,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        for (id, title) in titles {
            project.add_input(Input {
                id: input_id(*id),
                name: format!("Title {id}"),
                kind: InputKind::Title(title.clone()),
                required_capabilities: Vec::new(),
            });
        }
        project
    }

    fn domain() -> ClockDomainId {
        ClockDomainId::new(NonZeroU128::MIN)
    }

    /// Asserts the 320x180 frame is `inside` exactly on the scaled rectangle
    /// (32, 16)..(96, 48) and transparent black everywhere else.
    fn assert_bar(frame: &CpuVideoFrame, inside: [u8; 4]) {
        let payload = frame.payload();
        assert_eq!(payload.format(), PixelFormat::Rgba8);
        assert_eq!(
            (payload.dimensions().width(), payload.dimensions().height()),
            (320, 180)
        );
        assert_eq!(
            frame.metadata().unwrap().alpha_mode(),
            Some(AlphaMode::Straight)
        );
        let plane = payload.plane(0).unwrap();
        assert_eq!(plane.stride(), 320 * 4);
        for y in 0..180_usize {
            for x in 0..320_usize {
                let offset = y * plane.stride() + x * 4;
                let expected = if (32..96).contains(&x) && (16..48).contains(&y) {
                    inside
                } else {
                    [0; 4]
                };
                assert_eq!(
                    plane.bytes()[offset..offset + 4],
                    expected,
                    "pixel ({x}, {y})"
                );
            }
        }
    }

    fn wait_for_frame(sources: &NativeTitleSources) -> Vec<(InputId, CpuVideoFrame)> {
        let deadline = Instant::now() + WAIT;
        loop {
            let completed = sources.take_completed();
            if !completed.is_empty() {
                return completed;
            }
            assert!(Instant::now() < deadline, "no title frame was rendered");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn startup_renders_each_title_as_an_exact_straight_alpha_source_frame() {
        let project = project(&[
            (20, title(BAR_URI, 0, [200, 40, 10, 255])),
            (21, title(BAR_URI, 3, [255, 128, 0, 128])),
        ]);
        let (_sources, seeds) = NativeTitleSources::start(assets(), &project, domain()).unwrap();
        assert_eq!(
            seeds.iter().map(|(input, _)| *input).collect::<Vec<_>>(),
            vec![input_id(20), input_id(21)]
        );
        assert_bar(&seeds[0].1, [200, 40, 10, 255]);
        // Half-transparent paint keeps its alpha; the premultiplied render
        // (128, 64, 0, 128) converts back to straight color.
        assert_bar(&seeds[1].1, [255, 128, 0, 128]);
        let timing = seeds[0].1.timing();
        assert_eq!(timing.sequence().get(), 0);
        assert_eq!(timing.presentation_timestamp().as_nanos(), 0);
        assert_eq!(timing.duration().as_nanos(), 40_000_000);
        assert_eq!(timing.clock_domain(), domain());
    }

    #[test]
    fn titles_are_dated_to_the_projected_frame_they_go_on_air_until_they_leave_it() {
        let project = project(&[
            (20, title(BAR_URI, 0, [1, 2, 3, 255])),
            (21, title(CLOCK_URI, 0, [1, 2, 3, 255])),
        ]);
        let (sources, _) = NativeTitleSources::start(assets(), &project, domain()).unwrap();
        // Inputs that are not titles are never dated.
        sources.observe_on_air(5, [input_id(20), input_id(99)].into_iter());
        assert_eq!(sources.taken_frames(), vec![(input_id(20), 5)]);
        // A title already on air keeps its take frame.
        sources.observe_on_air(7, [input_id(21), input_id(20)].into_iter());
        assert_eq!(
            sources.taken_frames(),
            vec![(input_id(20), 5), (input_id(21), 7)]
        );
        // Leaving air forgets the take; the next take starts over.
        sources.observe_on_air(9, [input_id(21)].into_iter());
        assert_eq!(sources.taken_frames(), vec![(input_id(21), 7)]);
        sources.observe_on_air(11, [input_id(20), input_id(21)].into_iter());
        assert_eq!(
            sources.taken_frames(),
            vec![(input_id(20), 11), (input_id(21), 7)]
        );
        sources.observe_on_air(12, std::iter::empty());
        assert_eq!(sources.taken_frames(), Vec::new());
    }

    #[test]
    fn a_field_change_renders_a_new_frame_off_the_loop_and_an_unchanged_title_does_not() {
        let original = project(&[(20, title(BAR_URI, 0, [200, 40, 10, 255]))]);
        let (mut sources, _) = NativeTitleSources::start(assets(), &original, domain()).unwrap();

        sources.observe(&original);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(sources.take_completed().len(), 0);
        assert_eq!(sources.render_counts(), (0, 0));

        let changed = project(&[(20, title(BAR_URI, 1, [5, 150, 250, 255]))]);
        sources.observe(&changed);
        let completed = wait_for_frame(&sources);
        assert_eq!(completed.len(), 1);
        let (input, frame) = &completed[0];
        assert_eq!(*input, input_id(20));
        assert_bar(frame, [5, 150, 250, 255]);
        assert_eq!(frame.timing().sequence().get(), 1);
        assert_eq!(
            frame.timing().presentation_timestamp().as_nanos(),
            40_000_000
        );
        assert_eq!(sources.render_counts(), (1, 0));

        // The same state handed over again renders nothing more.
        sources.observe(&changed);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(sources.take_completed().len(), 0);
        assert_eq!(sources.render_counts(), (1, 0));
    }

    #[test]
    fn a_time_dependent_title_refreshes_at_a_bounded_rate_with_monotonic_timing() {
        let project = project(&[(20, title(CLOCK_URI, 0, [9, 8, 7, 255]))]);
        let (sources, seeds) = NativeTitleSources::start(assets(), &project, domain()).unwrap();
        assert_bar(&seeds[0].1, [9, 8, 7, 255]);
        sources.set_frame(250);
        let first = wait_for_frame(&sources);
        let started = Instant::now();
        let second = wait_for_frame(&sources);
        assert!(started.elapsed() >= MIN_ANIMATED_TITLE_REFRESH / 2);
        let sequence = |frames: &[(InputId, CpuVideoFrame)]| frames[0].1.timing().sequence().get();
        assert!(sequence(&second) > sequence(&first));
        assert!(
            second[0].1.timing().presentation_timestamp().as_nanos()
                > first[0].1.timing().presentation_timestamp().as_nanos()
        );
        assert_bar(&second[0].1, [9, 8, 7, 255]);
    }

    #[test]
    fn animating_titles_render_each_projected_pose_and_hold_the_frame_until_ingested() {
        use fm_titles::fm_animation::{ClipPosition, ClipState};

        let project = project(&[(20, title(FADE_URI, 0, [200, 100, 50, 255]))]);
        let (sources, seeds) = NativeTitleSources::start(assets(), &project, domain()).unwrap();
        // Off air the title is seeded in its Hold pose: fully faded in.
        assert_eq!(bar_alpha(&seeds[0].1), 255);
        let first = [(input_id(20), ClipState::In(ClipPosition::frame(0)))];
        // The take frame is held, even past its deadline, until its pose is
        // ingested.
        let due = Instant::now();
        assert!(!sources.covered_at(5, &first, due));
        sources.observe_poses(5, &first);
        let rendered = wait_for_frame(&sources);
        assert_eq!(bar_alpha(&rendered[0].1), 0);
        assert!(sources.covered_at(5, &first, due));

        // Half way through the In phase.
        let half = [(input_id(20), ClipState::In(ClipPosition::frame(5)))];
        sources.observe_poses(10, &half);
        assert_eq!(bar_alpha(&wait_for_frame(&sources)[0].1), 127);
        assert!(sources.covered_at(10, &half, Instant::now()));

        // Settled: the Hold pose renders once, then nothing while it holds.
        sources.observe_poses(15, &[(input_id(20), ClipState::Hold)]);
        assert_eq!(bar_alpha(&wait_for_frame(&sources)[0].1), 255);
        let renders = sources.render_counts();
        sources.observe_poses(16, &[]);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(sources.take_completed().len(), 0);
        assert_eq!(sources.render_counts(), renders);

        // A pose never rendered stops holding the frame once it is the
        // bound past its deadline, and is counted late once.
        let stalled = [(input_id(20), ClipState::In(ClipPosition::frame(9)))];
        let due = Instant::now();
        assert!(!sources.covered_at(20, &stalled, due));
        thread::sleep(MAX_TITLE_POSE_WAIT);
        assert!(sources.covered_at(20, &stalled, due));
        assert!(sources.covered_at(20, &stalled, due));
        assert_eq!(sources.late_poses(), 1);
    }

    #[test]
    fn a_renderer_that_cannot_keep_up_delays_the_output_by_the_bound_once() {
        use fm_titles::fm_animation::{ClipPosition, ClipState};

        let project = project(&[(20, title(FADE_URI, 0, [200, 100, 50, 255]))]);
        let (sources, _) = NativeTitleSources::start(assets(), &project, domain()).unwrap();
        // Frames 0..5 of an In phase at 50 fps whose poses are never
        // ingested (the renderer is slower than the frame rate). The first
        // frame waits out the bound past its deadline; the output is then
        // already that late, so the following frames, whose deadlines have
        // all passed by the bound, do not each wait it again.
        let origin = Instant::now();
        let period = Duration::from_millis(20);
        let deadline = |frame: u32| origin + period * frame;
        let pose = |frame: u32| [(input_id(20), ClipState::In(ClipPosition::frame(frame)))];
        assert!(!sources.covered_at(0, &pose(0), deadline(0)));
        thread::sleep(MAX_TITLE_POSE_WAIT + period * 5);
        let started = Instant::now();
        for frame in 0..5 {
            assert!(
                sources.covered_at(u64::from(frame), &pose(frame), deadline(frame)),
                "frame {frame} waits afresh"
            );
        }
        assert!(started.elapsed() < MAX_TITLE_POSE_WAIT);
        assert_eq!(sources.late_poses(), 5);
    }

    #[test]
    fn a_title_without_a_loaded_template_refuses_to_start() {
        let project = project(&[(
            20,
            title("asset://titles/missing.fmtitle", 0, [1, 1, 1, 255]),
        )]);
        let Err(error) = NativeTitleSources::start(assets(), &project, domain()) else {
            panic!("a missing template must refuse");
        };
        assert!(error.contains("native title input"), "{error}");
        assert!(error.contains("is not loaded"), "{error}");
    }

    #[test]
    fn unpremultiply_restores_straight_color() {
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0; 4]);
        assert_eq!(unpremultiply([10, 20, 30, 255]), [10, 20, 30, 255]);
        assert_eq!(unpremultiply([64, 32, 0, 128]), [128, 64, 0, 128]);
        assert_eq!(unpremultiply([1, 1, 1, 1]), [255, 255, 255, 1]);
    }
}

/// Startup resolution and, where a native adapter exists, end-to-end
/// realization through the native source registry and overlay path.
#[cfg(test)]
mod daemon_tests {
    use std::num::{NonZeroU32, NonZeroU128};

    use fm_auth::Policy;
    use fm_control::{ControlLimits, ControlService};
    use fm_model::{
        Input, MainMix, Output, ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo,
        SolidColor, StartupPolicy, TitleFieldEntry, TitleFieldId, TitleFieldValue,
    };
    use fm_persistence::{
        ProjectPosition, ProjectStore, RuntimeRouting, StoredProject, StoredTitleTemplate,
    };
    use fm_switcher::{OverlayChannelId, SwitcherState};
    use fm_titles::{
        Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
        Style, TemplateId, TitleTemplate,
    };
    use fm_types::{
        AudioFormat, BusId, ChannelLayout, ColorMetadata as ProjectColor, OutputId,
        PixelFormat as ProjectPixels, ProjectId, SampleFormat, SampleRate, ScanMode,
        VideoDimensions as ProjectDimensions, VideoFormat,
    };
    use freemixd::native_media::{
        NativeMediaRuntime, NativeProjectLimits, NativeProjectPlan, NativeResolvedSource,
        NativeSourceLimits,
    };

    use super::*;

    const TEMPLATE_KEY: &str = "titles/bar.fmtitle";
    const WAIT: Duration = Duration::from_secs(10);

    fn id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn color(bar: [u8; 4]) -> Vec<TitleFieldEntry> {
        vec![TitleFieldEntry {
            id: TitleFieldId::new(NonZeroU128::MIN),
            value: TitleFieldValue::Color(fm_model::Rgba8::new(bar[0], bar[1], bar[2], bar[3])),
        }]
    }

    /// Writes a 160x90 transparent template whose rectangle at (16, 8)
    /// sized 32x16 is tinted by color field 1.
    fn write_template(store: &ProjectStore) {
        write_template_animated(store, fm_titles::TitleAnimation::default());
    }

    /// [`write_template`] with `animation`.
    fn write_template_animated(store: &ProjectStore, animation: fm_titles::TitleAnimation) {
        let field = FieldId::new(NonZeroU128::MIN);
        let template = TitleTemplate {
            id: TemplateId::new(NonZeroU128::MIN),
            name: "Bar".into(),
            width: 160,
            height: 90,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field,
                name: "Bar".into(),
                default: FieldValue::Color(Color::new(1, 2, 3, 255)),
            }],
            elements: vec![Element {
                id: ElementId::new(NonZeroU128::MIN),
                name: "Bar".into(),
                bounds: Bounds {
                    x: 16,
                    y: 8,
                    width: 32,
                    height: 16,
                },
                z_index: 0,
                visible: true,
                style: Style::default(),
                color_field: Some(field),
                kind: ElementKind::Rectangle,
            }],
            animation,
        };
        let document = fm_persistence::encode_title_template(&StoredTitleTemplate {
            template,
            fonts: Vec::new(),
            clocks: Vec::new(),
        })
        .unwrap();
        let path = store.assets_root().join(TEMPLATE_KEY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, document).unwrap();
    }

    /// A 320x180 project: input 1 is a solid red generator on Program and
    /// input 2 a title.
    fn stored_project(bar: [u8; 4]) -> StoredProject {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Native titles",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: ProjectDimensions::new(320, 180).unwrap(),
                    frame_rate,
                    pixel_format: ProjectPixels::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ProjectColor::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::I16,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(Input {
            id: id(1),
            name: "Red".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(255, 0, 0, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: id(2),
            name: "Title".into(),
            kind: InputKind::Title(TitleInput {
                template_uri: format!("asset://{TEMPLATE_KEY}"),
                field_revision: 0,
                fields: color(bar),
            }),
            required_capabilities: Vec::new(),
        });
        project.set_main_mix(MainMix::new(id(1), id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(id(1)),
                realized_program_id: Some(id(1)),
                desired_preview_id: Some(id(2)),
                realized_preview_id: Some(id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    fn output() -> OutputId {
        OutputId::new(NonZeroU128::new(10).unwrap())
    }

    fn store() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        write_template(&store);
        (directory, store)
    }

    fn resolve(
        store: &ProjectStore,
        stored: &StoredProject,
    ) -> (Vec<NativeResolvedSource>, NativeTitleSources) {
        let mut sources = crate::resolve_native_sources(store, stored, None)
            .unwrap()
            .sources;
        let titles = crate::resolve_native_title_sources(store, stored, &mut sources).unwrap();
        (sources, titles)
    }

    #[test]
    fn native_resolution_realizes_a_stored_title_as_an_exact_live_frame() {
        let (_directory, store) = store();
        let stored = stored_project([200, 40, 10, 255]);
        let (sources, _titles) = resolve(&store, &stored);
        assert_eq!(
            sources
                .iter()
                .map(NativeResolvedSource::input)
                .collect::<Vec<_>>(),
            vec![id(1), id(2)]
        );
        let NativeResolvedSource::LiveFrame { input, frame } = &sources[1] else {
            panic!("a title must enter the live frame lane");
        };
        assert_eq!(*input, id(2));
        let plane = frame.payload().plane(0).unwrap();
        for y in 0..180_usize {
            for x in 0..320_usize {
                let offset = y * plane.stride() + x * 4;
                let expected = if (32..96).contains(&x) && (16..48).contains(&y) {
                    [200, 40, 10, 255]
                } else {
                    [0; 4]
                };
                assert_eq!(plane.bytes()[offset..offset + 4], expected, "({x}, {y})");
            }
        }
    }

    /// Reads one RGBA8 pixel of the Program readback.
    fn pixel(pixels: &fm_gpu::DiagnosticReadback, x: usize, y: usize) -> [u8; 4] {
        let offset = y * usize::try_from(pixels.stride).unwrap() + x * 4;
        pixels.rgba[offset..offset + 4].try_into().unwrap()
    }

    fn assert_close(actual: [u8; 4], expected: [u8; 4], label: &str) {
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 1),
            "{label}: {actual:?} != {expected:?}"
        );
    }

    /// Ingests the title through the native registry, keys it over Program
    /// as an overlay on the project output, re-renders it after a field
    /// change, and reads back exact pictures. Skips (passing) where no
    /// native adapter exists.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_title_overlay_composites_over_program_and_follows_field_changes() {
        let Ok(runtime) = NativeMediaRuntime::new_blocking([crate::platform_native_backend()])
        else {
            eprintln!("skipping: no native adapter");
            return;
        };
        let (_directory, store) = store();
        let stored = stored_project([0, 0, 255, 255]);
        let (sources, mut titles) = resolve(&store, &stored);
        let mut playback = runtime
            .preflight_resolved_source_playback_mixed_blocking(
                None,
                sources,
                crate::native_clock_domain(),
                fm_codec_ffmpeg::StreamSelector::Best,
                NativeSourceLimits::default(),
            )
            .unwrap();
        // Overlays realize per project output; the plan alone gains one.
        let mut planned = stored.project().clone();
        planned.add_output(Output {
            id: output(),
            name: "Program".into(),
            video_source: fm_types::SceneId::new(NonZeroU128::new(99).unwrap()),
            audio_source: BusId::new(NonZeroU128::new(99).unwrap()),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        });
        let plan = NativeProjectPlan::compile(&planned, NativeProjectLimits::default()).unwrap();
        let templates = crate::load_bundle_assets(&store, stored.project()).unwrap();
        let control = ControlService::new(
            crate::restore_engine_with_titles(&stored, &templates).unwrap(),
            Policy::development(),
            "unit-engine",
            "unit-log",
            ControlLimits::default(),
        );
        let mut frame = control.project_next_frame().unwrap();
        let channel = OverlayChannelId::new(1).unwrap();
        let mut switcher = SwitcherState::new(vec![id(1), id(2)], id(1), id(2)).unwrap();
        switcher.take_overlay(channel, id(2)).unwrap();
        let _ = switcher.set_overlay_output_inclusion(channel, output(), true);
        frame.overlays = switcher.overlays().clone();
        let mut readback = runtime
            .create_program_readback_blocking(
                NonZeroU32::new(320).unwrap(),
                NonZeroU32::new(180).unwrap(),
            )
            .unwrap();
        let mut render = |playback: &freemixd::native_media::NativeSourcePlayback| {
            let outputs = runtime
                .render_project_outputs_with_stingers_blocking(
                    playback.registry(),
                    playback.registry(),
                    &plan,
                    &frame,
                )
                .unwrap();
            assert_eq!(outputs.len(), 1);
            runtime
                .readback_program_blocking(&mut readback, outputs[0].texture())
                .unwrap()
        };

        let first = render(&playback);
        assert_close(pixel(&first, 40, 20), [0, 0, 255, 255], "title bar");
        assert_close(pixel(&first, 95, 47), [0, 0, 255, 255], "title bar corner");
        assert_close(
            pixel(&first, 5, 5),
            [255, 0, 0, 255],
            "Program around the title",
        );
        assert_close(
            pixel(&first, 96, 48),
            [255, 0, 0, 255],
            "Program past the bar",
        );

        let mut changed = stored.project().clone();
        changed
            .set_title_fields(id(2), 1, color([0, 255, 0, 255]))
            .unwrap();
        titles.observe(&changed);
        let deadline = Instant::now() + WAIT;
        let completed = loop {
            let completed = titles.take_completed();
            if !completed.is_empty() {
                break completed;
            }
            assert!(Instant::now() < deadline, "the title did not re-render");
            thread::sleep(Duration::from_millis(2));
        };
        for (input, frame) in completed {
            runtime
                .ingest_live_video_frame_blocking(&mut playback, input, frame)
                .unwrap();
        }
        let second = render(&playback);
        assert_close(
            pixel(&second, 40, 20),
            [0, 255, 0, 255],
            "changed title bar",
        );
        assert_close(
            pixel(&second, 5, 5),
            [255, 0, 0, 255],
            "Program around the title",
        );
    }

    /// The bar fades in over a 10-frame In phase and out over a 5-frame Out
    /// phase of its default 30/1 timebase: 9 and 5 frames of the 25/1
    /// project.
    fn fading_animation() -> fm_titles::TitleAnimation {
        use fm_titles::fm_animation::{Easing, Keyframe, Phase, Property, Track, TrackKey, Value};

        let mut template = TitleTemplate {
            id: TemplateId::new(NonZeroU128::MIN),
            name: "Bar".into(),
            width: 160,
            height: 90,
            background: Color::TRANSPARENT,
            fields: Vec::new(),
            elements: vec![Element {
                id: ElementId::new(NonZeroU128::MIN),
                name: "Bar".into(),
                bounds: Bounds {
                    x: 16,
                    y: 8,
                    width: 32,
                    height: 16,
                },
                z_index: 0,
                visible: true,
                style: Style::default(),
                color_field: None,
                kind: ElementKind::Rectangle,
            }],
            animation: fm_titles::TitleAnimation::default(),
        };
        let key = |frame, value| Keyframe::new(frame, Value::Scalar(value), Easing::Linear);
        let bar = ElementId::new(NonZeroU128::MIN);
        template
            .edit_animation(|clip| {
                clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
                clip.set_phase_frames_keeping_keys(Phase::Out, 5)?;
                clip.set_track(Track::new(
                    TrackKey::new(bar, Phase::In, Property::Opacity),
                    vec![key(0, 0), key(10, 255)],
                ))?;
                clip.set_track(Track::new(
                    TrackKey::new(bar, Phase::Out, Property::Opacity),
                    vec![key(0, 255), key(5, 0)],
                ))
            })
            .unwrap();
        template.animation
    }

    /// Takes an animated title onto an overlay through the engine and
    /// realizes every frame the way the session loop does: observe the
    /// projected frame, hold it until its pose is ingested, render, then
    /// tick. Reads back the bar on each frame of the In phase, the hold,
    /// and the Out phase. Skips (passing) where no native adapter exists.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_title_overlay_plays_its_in_and_out_phases_frame_accurately() {
        use fm_engine::EngineCommand;
        use fm_titles::TitleAirPhase;

        let Ok(runtime) = NativeMediaRuntime::new_blocking([crate::platform_native_backend()])
        else {
            eprintln!("skipping: no native adapter");
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        write_template_animated(&store, fading_animation());
        let stored = stored_project([0, 0, 255, 255]);
        let (sources, titles) = resolve(&store, &stored);
        let mut playback = runtime
            .preflight_resolved_source_playback_mixed_blocking(
                None,
                sources,
                crate::native_clock_domain(),
                fm_codec_ffmpeg::StreamSelector::Best,
                NativeSourceLimits::default(),
            )
            .unwrap();
        let mut planned = stored.project().clone();
        planned.add_output(Output {
            id: output(),
            name: "Program".into(),
            video_source: fm_types::SceneId::new(NonZeroU128::new(99).unwrap()),
            audio_source: BusId::new(NonZeroU128::new(99).unwrap()),
            startup: StartupPolicy::Stopped,
            required_capabilities: Vec::new(),
        });
        let plan = NativeProjectPlan::compile(&planned, NativeProjectLimits::default()).unwrap();
        let templates = crate::load_bundle_assets(&store, stored.project()).unwrap();
        let mut engine = crate::restore_engine_with_titles(&stored, &templates).unwrap();
        let channel = OverlayChannelId::new(1).unwrap();
        // The engine's show has no outputs, so the overlay is mirrored onto a
        // switcher that includes it in the plan's output.
        let mut mirror = SwitcherState::new(vec![id(1), id(2)], id(1), id(2)).unwrap();
        let _ = mirror.set_overlay_output_inclusion(channel, output(), true);
        let mut readback = runtime
            .create_program_readback_blocking(
                NonZeroU32::new(320).unwrap(),
                NonZeroU32::new(180).unwrap(),
            )
            .unwrap();
        let command = |engine: &mut fm_engine::Engine, key: &str, command| {
            let outcome = engine
                .execute(
                    fm_command::CommandEnvelope::new(
                        format!("command-{key}"),
                        fm_command::IdempotencyKey::new(key),
                        command,
                    ),
                    0,
                )
                .unwrap();
            assert!(outcome.receipt.accepted().is_some(), "{key}");
        };
        // Realizes one frame; returns its phase and the bar's blue channel
        // over the red Program.
        let mut realize = |engine: &mut fm_engine::Engine,
                           playback: &mut freemixd::native_media::NativeSourcePlayback|
         -> (Option<TitleAirPhase>, u8) {
            let mut frame = engine.clone().tick().unwrap();
            titles.observe_frame(&frame);
            let pose = frame.titles.first().map(|title| title.frame.sample);
            let deadline = Instant::now() + WAIT;
            loop {
                for (input, picture) in titles.take_completed() {
                    runtime
                        .ingest_live_video_frame_blocking(playback, input, picture)
                        .unwrap();
                }
                if pose.is_none_or(|pose| titles.ingested_pose(id(2)) == Some(pose)) {
                    break;
                }
                assert!(Instant::now() < deadline, "the pose was never rendered");
                thread::sleep(Duration::from_millis(2));
            }
            assert!(titles.covered(&frame, Instant::now()));
            if frame.overlays[channel.index()].is_active() {
                mirror.take_overlay(channel, id(2)).unwrap();
            } else {
                let _ = mirror.apply(fm_switcher::SwitcherCommand::OverlayOff(channel));
            }
            frame.overlays = mirror.overlays().clone();
            let outputs = runtime
                .render_project_outputs_with_stingers_blocking(
                    playback.registry(),
                    playback.registry(),
                    &plan,
                    &frame,
                )
                .unwrap();
            let picture = runtime
                .readback_program_blocking(&mut readback, outputs[0].texture())
                .unwrap();
            assert_close(pixel(&picture, 5, 5), [255, 0, 0, 255], "Program around");
            engine.tick().unwrap();
            (
                frame.titles.first().map(|title| title.frame.phase),
                pixel(&picture, 40, 20)[2],
            )
        };

        command(
            &mut engine,
            "take",
            EngineCommand::TakeTitle {
                input: id(2),
                channel,
                base_revision: 0,
                fields: Vec::new(),
            },
        );
        let entering = (0..11)
            .map(|_| realize(&mut engine, &mut playback))
            .collect::<Vec<_>>();
        // The take frame shows In clip frame 0: the bar is invisible.
        assert_eq!(entering[0], (Some(TitleAirPhase::In), 0));
        // Blue rises strictly through the nine In frames, then holds full.
        for pair in entering[..9].windows(2) {
            assert_eq!(pair[1].0, Some(TitleAirPhase::In));
            assert!(pair[1].1 > pair[0].1, "{entering:?}");
        }
        assert!(entering[8].1 < 254, "{entering:?}");
        for held in &entering[9..] {
            assert_eq!(held.0, Some(TitleAirPhase::Hold));
            assert!(held.1 >= 254, "{entering:?}");
        }

        command(
            &mut engine,
            "off",
            EngineCommand::TitleOffAir { input: id(2) },
        );
        let leaving = (0..6)
            .map(|_| realize(&mut engine, &mut playback))
            .collect::<Vec<_>>();
        // Out frame 0 is fully opaque, then blue falls strictly through the
        // five Out frames; the next frame has no overlay at all.
        assert_eq!(leaving[0].0, Some(TitleAirPhase::Out));
        assert!(leaving[0].1 >= 254, "{leaving:?}");
        for pair in leaving[..5].windows(2) {
            assert_eq!(pair[1].0, Some(TitleAirPhase::Out));
            assert!(pair[1].1 < pair[0].1, "{leaving:?}");
        }
        assert_eq!(leaving[5], (None, 0));
        assert_eq!(titles.late_poses(), 0);
        drop(directory);
    }
}
