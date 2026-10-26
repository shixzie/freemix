//! Exact title previews (GX-011 stage 4a, Protocol 2.29).
//!
//! A `title_preview_request` asks the authority to render one title input
//! exactly as it would draw it on air: its persisted field values, its
//! template as the engine holds it now (or with the request's unsaved draft
//! clip instead of the template's animation), sampled at one point of its
//! animation, through the same `fm_titles` reference renderer
//! (`render_sampled`) and nearest-neighbor scaling the monitor tap and the
//! native title worker use. Production clocks read the engine frame cursor.
//!
//! The answer is one four-channel QOI stream of straight RGBA8 pixels at the
//! requested size (at most 640x360) split into `title_preview_frame` parts,
//! written to a raw peer through its own latest-wins lossy slot, so a newer
//! preview replaces one not yet started. The session loop only resolves a
//! preview ([`prepare_preview`]); it renders on a worker thread
//! ([`TitlePreviewWorker`]), latest wins per peer, so a slow template never
//! delays native frames, command results, or heartbeats. The per-session
//! `title_previews` rate (fm-server) bounds the work. Exact scene previews
//! (Protocol 2.32, [`crate::scene_preview`]) share the worker, the lossy
//! slot, and the rate.
//!
//! Protocol 2.90 (GX-006) adds draft field values: a request's `fields`
//! replace the persisted values of those fields for this render only, each
//! checked against the template's field type and the title field limits,
//! so the browser title editor shows its unsaved edits before Take or
//! Update live. A Web session's request is answered through its paced
//! feed (see [`crate::web_feed`]), at most 320x180, and may name only the
//! fields the template exposes to browsers. The rendered image draws a
//! hidden field at its template default, never its stored value, so a
//! browser's pixels reveal no more than its `title_state` records.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use fm_auth::Policy;
use fm_control::{ControlService, title_clip_from_wire};
use fm_model::{
    Project, Rgba8, TitleFieldId, TitleFieldValue as ModelValue, TitleInput, validate_title_fields,
};
use fm_protocol::{
    MAX_WEB_TITLE_PREVIEW_HEIGHT, MAX_WEB_TITLE_PREVIEW_WIDTH, ServerIdentity, StructuredError,
    TitleFieldUpdate, TitleFieldValue, TitlePreviewFrameMessage, TitlePreviewRequest,
    TitlePreviewSample, WireInputId, encode_qoi_rgba, title_preview_parts,
};
use fm_titles::fm_animation::{Clip, ClipPosition, ClipState, Phase};
use fm_titles::{FieldType, TitleTemplate};
use fm_types::InputId;

use crate::title_render::{self, TitleAssets};

pub(crate) fn refusal(code: &str, message: impl Into<String>) -> StructuredError {
    StructuredError {
        code: code.into(),
        message: message.into(),
        fields: Vec::new(),
        retryable: false,
    }
}

/// The pose `sample` names on `clip` (a title's or a scene's), refused past
/// the end of its phase.
pub(crate) fn sample_pose<T: Copy + Ord>(
    sample: TitlePreviewSample,
    clip: &Clip<T>,
) -> Result<ClipState, StructuredError> {
    let within = |phase: Phase, frame: u32| {
        if frame > clip.phase_frames(phase) {
            Err(refusal(
                "invalid_request",
                format!(
                    "preview frame {frame} is past the {} frame phase",
                    clip.phase_frames(phase)
                ),
            ))
        } else {
            Ok(ClipPosition::frame(frame))
        }
    };
    Ok(match sample {
        TitlePreviewSample::Static => ClipState::Static,
        TitlePreviewSample::Hold => ClipState::Hold,
        TitlePreviewSample::In { frame } => ClipState::In(within(Phase::In, frame)?),
        TitlePreviewSample::Out { frame } => ClipState::Out(within(Phase::Out, frame)?),
    })
}

/// One exact preview, resolved on the session loop from the engine and the
/// durable project as of its request, and rendered off it
/// ([`PreviewJob::render`] on a [`LatestWorker`]).
pub(crate) struct PreviewJob {
    server: ServerIdentity,
    request_id: String,
    wire_input: WireInputId,
    input: InputId,
    title: TitleInput,
    template: Arc<TitleTemplate>,
    time: title_render::TitleTime,
    animation_revision: u64,
    draft: bool,
    width: u16,
    height: u16,
}

impl PreviewJob {
    /// Renders the preview and splits it into the `title_preview_frame`
    /// parts its server sends.
    ///
    /// # Errors
    ///
    /// A `render_failed` refusal when the render or its encoding fails.
    pub(crate) fn render(
        &self,
        assets: &TitleAssets,
    ) -> Result<Vec<TitlePreviewFrameMessage>, StructuredError> {
        let input = self.input;
        let pixels = title_render::render_title_preview(
            assets,
            &self.template,
            input,
            &self.title,
            self.time,
            u32::from(self.width),
            u32::from(self.height),
        )
        .ok_or_else(|| refusal("render_failed", format!("title {input} did not render")))?;
        let qoi = encode_qoi_rgba(self.width, self.height, &pixels)
            .map_err(|error| refusal("render_failed", error.to_string()))?;
        Ok(title_preview_parts(
            &self.server,
            &self.request_id,
            self.wire_input,
            self.animation_revision,
            self.draft,
            self.width,
            self.height,
            &qoi,
        ))
    }
}

/// Resolves the exact preview `request` asks for: everything but the
/// render, which [`PreviewJob::render`] does off the session loop.
///
/// # Errors
///
/// A `not_found` refusal for an input that is not a title with a loaded
/// template, and `invalid_request` for a draft clip the template refuses or
/// a frame past its phase.
pub(crate) fn prepare_preview(
    control: &ControlService<Policy>,
    project: &Project,
    server: &ServerIdentity,
    request: &TitlePreviewRequest,
    web: bool,
) -> Result<PreviewJob, StructuredError> {
    let input = request.input.to_domain();
    // A browser asks for a small image only (Protocol 2.90).
    if web
        && (request.width > MAX_WEB_TITLE_PREVIEW_WIDTH
            || request.height > MAX_WEB_TITLE_PREVIEW_HEIGHT)
    {
        return Err(refusal(
            "invalid_request",
            format!(
                "a web preview is at most {MAX_WEB_TITLE_PREVIEW_WIDTH}x\
                 {MAX_WEB_TITLE_PREVIEW_HEIGHT}"
            ),
        ));
    }
    let mut title = title_render::title_of_input(project, input)
        .ok_or_else(|| refusal("not_found", format!("input {input} is not a title")))?
        .clone();
    let engine = control
        .title_template(input)
        .ok_or_else(|| refusal("not_found", format!("title {input} has no loaded template")))?;
    if web {
        withhold_hidden_fields(&mut title, &engine.template);
    }
    apply_draft_fields(&mut title, &engine.template, &request.fields, web)?;
    let animation_revision = engine.animation().revision;
    let template: Arc<TitleTemplate> = match &request.clip {
        None => Arc::clone(&engine.template),
        Some(draft) => {
            let draft = title_clip_from_wire(draft)
                .map_err(|error| refusal("invalid_request", error.to_string()))?;
            let mut template = (*engine.template).clone();
            template
                .edit_animation(|clip| {
                    *clip = draft;
                    Ok(())
                })
                .map_err(|error| refusal("invalid_request", error.to_string()))?;
            Arc::new(template)
        }
    };
    let pose = sample_pose(request.sample, &template.animation.clip)?;
    let time = title_render::TitleTime::now(
        control.frame_cursor(),
        project.settings().frame_rate,
        None,
        pose,
    );
    Ok(PreviewJob {
        server: server.clone(),
        request_id: request.request_id.clone(),
        wire_input: request.input,
        input,
        title,
        template,
        time,
        animation_revision,
        draft: request.clip.is_some() || !request.fields.is_empty(),
        width: request.width,
        height: request.height,
    })
}

/// Drops `title`'s stored values of the fields `template` hides from
/// browsers, so a Web preview draws them at the template default.
fn withhold_hidden_fields(title: &mut TitleInput, template: &TitleTemplate) {
    title.fields.retain(|entry| {
        template
            .fields
            .iter()
            .find(|definition| definition.id.get() == entry.id.get())
            .is_none_or(|definition| definition.exposed)
    });
}

/// Replaces `title`'s persisted values with a preview's unsaved drafts
/// (Protocol 2.90). Each draft must name a template field of the same type
/// (and, from a browser, one the template exposes to it), and the result
/// must stay within the title bounds.
fn apply_draft_fields(
    title: &mut TitleInput,
    template: &TitleTemplate,
    drafts: &[TitleFieldUpdate],
    web: bool,
) -> Result<(), StructuredError> {
    for draft in drafts {
        let id = TitleFieldId::new(draft.field.get());
        let definition = template
            .fields
            .iter()
            .find(|definition| definition.id.get() == draft.field.get())
            .filter(|definition| !web || definition.exposed)
            .ok_or_else(|| {
                refusal(
                    "not_found",
                    format!("the title has no field {}", draft.field),
                )
            })?;
        let value = match (&draft.value, definition.field_type()) {
            (TitleFieldValue::Text(text), FieldType::Text) => ModelValue::Text(text.clone()),
            (TitleFieldValue::Number(number), FieldType::Number) => ModelValue::Number(*number),
            (
                TitleFieldValue::Color {
                    red,
                    green,
                    blue,
                    alpha,
                },
                FieldType::Color,
            ) => ModelValue::Color(Rgba8::new(*red, *green, *blue, *alpha)),
            (TitleFieldValue::Image { asset_uri }, FieldType::Image) => ModelValue::Image {
                asset_uri: asset_uri.clone(),
            },
            _ => {
                return Err(refusal(
                    "invalid_request",
                    format!("draft field {} has the wrong type", draft.field),
                ));
            }
        };
        match title.fields.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => entry.value = value,
            None => {
                return Err(refusal(
                    "not_found",
                    format!("the title has no field {}", draft.field),
                ));
            }
        }
    }
    validate_title_fields(title.field_revision, &title.fields)
        .map_err(|error| refusal("invalid_request", error.to_string()))
}

/// A worker thread that runs jobs off the session loop, at most one
/// pending job per peer: a newer job replaces one that has not started,
/// and a newer answer replaces one the loop has not taken yet. Previews
/// render here so a slow template never delays a native frame, a command
/// result, or a heartbeat.
pub(crate) struct LatestWorker<J, R> {
    shared: Arc<WorkerShared<J, R>>,
    worker: Option<JoinHandle<()>>,
}

struct WorkerShared<J, R> {
    slot: Mutex<WorkerSlot<J, R>>,
    wake: Condvar,
}

struct WorkerSlot<J, R> {
    pending: BTreeMap<u64, J>,
    finished: BTreeMap<u64, R>,
    shutdown: bool,
}

impl<J, R> WorkerShared<J, R> {
    fn lock(&self) -> MutexGuard<'_, WorkerSlot<J, R>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<J: Send + 'static, R: Send + 'static> LatestWorker<J, R> {
    /// Starts the worker thread `name`, running `run` on each job.
    ///
    /// # Errors
    ///
    /// Returns the operating system error when the thread cannot start.
    pub(crate) fn spawn(
        name: &str,
        mut run: impl FnMut(J) -> R + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(WorkerShared {
            slot: Mutex::new(WorkerSlot {
                pending: BTreeMap::new(),
                finished: BTreeMap::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                loop {
                    let (peer, job) = {
                        let mut slot = worker_shared.lock();
                        loop {
                            if slot.shutdown {
                                return;
                            }
                            if let Some(next) = slot.pending.pop_first() {
                                break next;
                            }
                            slot = worker_shared
                                .wake
                                .wait(slot)
                                .unwrap_or_else(PoisonError::into_inner);
                        }
                    };
                    let answer = run(job);
                    worker_shared.lock().finished.insert(peer, answer);
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Queues `job` for `peer`, replacing its job that has not started.
    /// Never waits for a running job.
    pub(crate) fn submit(&self, peer: u64, job: J) {
        self.shared.lock().pending.insert(peer, job);
        self.shared.wake.notify_one();
    }

    /// The answers finished since the last call, one per peer.
    pub(crate) fn take_finished(&self) -> BTreeMap<u64, R> {
        std::mem::take(&mut self.shared.lock().finished)
    }
}

impl<J, R> Drop for LatestWorker<J, R> {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// One exact preview to render: a title's or (Protocol 2.32) a scene's.
pub(crate) enum PreviewWork {
    Title(Box<PreviewJob>),
    Scene(Box<crate::scene_preview::ScenePreviewJob>),
}

/// Which preview record kind an answer's parts are sent as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviewKind {
    /// `title_preview_frame`.
    Title,
    /// `scene_preview_frame`.
    Scene,
}

/// The answer to the newest preview of one peer: its request id, kind, and
/// parts, or the refusal its render met.
pub(crate) struct PreviewAnswer {
    pub(crate) request_id: String,
    pub(crate) kind: PreviewKind,
    pub(crate) parts: Result<Vec<TitlePreviewFrameMessage>, StructuredError>,
}

/// The preview worker of a session loop. Title and scene previews share
/// it, latest wins per peer across both.
pub(crate) type TitlePreviewWorker = LatestWorker<PreviewWork, PreviewAnswer>;

/// Starts the preview worker rendering titles with `assets`.
///
/// # Errors
///
/// Returns the operating system error when the thread cannot start.
pub(crate) fn spawn_preview_worker(
    assets: Arc<TitleAssets>,
) -> std::io::Result<TitlePreviewWorker> {
    LatestWorker::spawn(
        "freemixd-title-preview",
        move |work: PreviewWork| match work {
            PreviewWork::Title(job) => PreviewAnswer {
                parts: job.render(&assets),
                request_id: job.request_id,
                kind: PreviewKind::Title,
            },
            PreviewWork::Scene(job) => PreviewAnswer {
                parts: job.render(),
                request_id: job.request_id,
                kind: PreviewKind::Scene,
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::LatestWorker;

    fn wait_for<R>(
        worker: &LatestWorker<u32, R>,
        mut done: impl FnMut(&LatestWorker<u32, R>) -> bool,
    ) where
        R: Send + 'static,
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(worker) {
            assert!(Instant::now() < deadline, "worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn field(id: u128) -> fm_protocol::WireTitleFieldId {
        fm_protocol::WireTitleFieldId::new(core::num::NonZeroU128::new(id).unwrap())
    }

    fn draft(id: u128, value: fm_protocol::TitleFieldValue) -> fm_protocol::TitleFieldUpdate {
        fm_protocol::TitleFieldUpdate {
            field: field(id),
            value,
        }
    }

    /// Protocol 2.90: a preview's draft values replace the persisted ones
    /// for the render only, typed as the template declares; a browser may
    /// name only exposed fields, and the title bounds still hold.
    #[test]
    fn draft_fields_are_typed_bounded_and_exposed_only_to_browsers() {
        use fm_model::{TitleFieldEntry, TitleFieldId, TitleFieldValue as Model, TitleInput};
        use fm_protocol::TitleFieldValue as Wire;
        use fm_titles::{FieldDefinition, FieldId, FieldValue, TemplateId, TitleTemplate};

        let id = |value| core::num::NonZeroU128::new(value).unwrap();
        let definition = |value, name: &str, exposed| FieldDefinition {
            id: FieldId::new(id(value)),
            name: name.into(),
            default: FieldValue::Text(String::new()),
            exposed,
        };
        let template = TitleTemplate {
            id: TemplateId::new(id(1)),
            name: "Lower".into(),
            width: 640,
            height: 360,
            background: fm_titles::Color::TRANSPARENT,
            fields: vec![definition(1, "Name", true), definition(2, "Note", false)],
            elements: Vec::new(),
            animation: fm_titles::TitleAnimation::default(),
        };
        let title = || TitleInput {
            template_uri: "asset://titles/lower.fmtitle".into(),
            field_revision: 3,
            fields: [1, 2]
                .map(|value| TitleFieldEntry {
                    id: TitleFieldId::new(id(value)),
                    value: Model::Text("saved".into()),
                    revision: 0,
                })
                .to_vec(),
        };
        let code = |result: Result<(), fm_protocol::StructuredError>| result.unwrap_err().code;

        let mut drafted = title();
        super::apply_draft_fields(
            &mut drafted,
            &template,
            &[draft(1, Wire::Text("Ana".into()))],
            true,
        )
        .unwrap();
        assert_eq!(drafted.fields[0].value, Model::Text("Ana".into()));
        assert_eq!(drafted.fields[1].value, Model::Text("saved".into()));
        assert_eq!(drafted.field_revision, 3);
        // The hidden field: unknown to a browser, a draft to Studio.
        let hidden = [draft(2, Wire::Text("x".into()))];
        assert_eq!(
            code(super::apply_draft_fields(
                &mut title(),
                &template,
                &hidden,
                true
            )),
            "not_found"
        );
        super::apply_draft_fields(&mut title(), &template, &hidden, false).unwrap();
        assert_eq!(
            code(super::apply_draft_fields(
                &mut title(),
                &template,
                &[draft(9, Wire::Text("x".into()))],
                false
            )),
            "not_found"
        );
        assert_eq!(
            code(super::apply_draft_fields(
                &mut title(),
                &template,
                &[draft(1, Wire::Number(4))],
                true
            )),
            "invalid_request"
        );
        let long = "x".repeat(fm_model::MAX_TITLE_TEXT_CHARS + 1);
        assert_eq!(
            code(super::apply_draft_fields(
                &mut title(),
                &template,
                &[draft(1, Wire::Text(long))],
                true
            )),
            "invalid_request"
        );
    }

    /// GX-006: a Web preview drops the stored values of fields the template
    /// hides, so they draw at the template default; exposed fields and
    /// fields the template does not name keep theirs.
    #[test]
    fn web_previews_withhold_hidden_field_values() {
        use fm_model::{TitleFieldEntry, TitleFieldId, TitleFieldValue as Model, TitleInput};
        use fm_titles::{FieldDefinition, FieldId, FieldValue, TemplateId, TitleTemplate};

        let id = |value| core::num::NonZeroU128::new(value).unwrap();
        let definition = |value, exposed| FieldDefinition {
            id: FieldId::new(id(value)),
            name: format!("Field {value}"),
            default: FieldValue::Text("default".into()),
            exposed,
        };
        let template = TitleTemplate {
            id: TemplateId::new(id(1)),
            name: "Lower".into(),
            width: 640,
            height: 360,
            background: fm_titles::Color::TRANSPARENT,
            fields: vec![definition(1, true), definition(2, false)],
            elements: Vec::new(),
            animation: fm_titles::TitleAnimation::default(),
        };
        let mut title = TitleInput {
            template_uri: "asset://titles/lower.fmtitle".into(),
            field_revision: 0,
            fields: [1, 2, 3]
                .map(|value| TitleFieldEntry {
                    id: TitleFieldId::new(id(value)),
                    value: Model::Text("secret".into()),
                    revision: 0,
                })
                .to_vec(),
        };
        super::withhold_hidden_fields(&mut title, &template);
        let kept: Vec<_> = title
            .fields
            .iter()
            .map(|entry| entry.id.get().get())
            .collect();
        assert_eq!(kept, [1, 3]);
    }

    /// Regression: previews render off the session loop. Submitting never
    /// waits for a running render, a newer job replaces a peer's job that
    /// has not started, and every peer is answered.
    #[test]
    fn preview_jobs_run_off_the_caller_and_the_latest_job_per_peer_wins() {
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let worker = LatestWorker::spawn("test-preview", move |job: u32| {
            started_tx.send(job).unwrap();
            release_rx.recv().unwrap();
            job * 10
        })
        .unwrap();
        worker.submit(1, 1);
        assert_eq!(started.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
        // The worker is busy rendering job 1: these return at once, and
        // the newer of peer 1's pending jobs replaces the older.
        let submitted = Instant::now();
        worker.submit(1, 2);
        worker.submit(2, 5);
        worker.submit(1, 3);
        assert!(submitted.elapsed() < Duration::from_secs(5));
        assert!(worker.take_finished().is_empty());
        for _ in 0..3 {
            release.send(()).unwrap();
        }
        let mut answers = std::collections::BTreeMap::new();
        wait_for(&worker, |worker| {
            answers.extend(worker.take_finished());
            answers.len() == 2 && answers.get(&1) == Some(&30)
        });
        assert_eq!(answers.get(&2), Some(&50));
        let ran: Vec<u32> = started.try_iter().collect();
        assert_eq!(ran, vec![3, 5]);
    }
}
