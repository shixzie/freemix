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
//! preview replaces one not yet started. Previews render on the session loop;
//! the per-session `title_previews` rate (fm-server) bounds that work.

use std::sync::Arc;

use fm_auth::Policy;
use fm_control::{ControlService, title_clip_from_wire};
use fm_model::Project;
use fm_protocol::{
    ServerIdentity, StructuredError, TitlePreviewFrameMessage, TitlePreviewRequest,
    TitlePreviewSample, encode_qoi_rgba, title_preview_parts,
};
use fm_titles::fm_animation::{ClipPosition, ClipState, Phase};
use fm_titles::{TitleClip, TitleTemplate};

use crate::title_render::{self, TitleAssets};

fn refusal(code: &str, message: impl Into<String>) -> StructuredError {
    StructuredError {
        code: code.into(),
        message: message.into(),
        fields: Vec::new(),
        retryable: false,
    }
}

/// The pose `sample` names on `clip`, refused past the end of its phase.
fn pose(sample: TitlePreviewSample, clip: &TitleClip) -> Result<ClipState, StructuredError> {
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

/// Renders the exact preview `request` asks for and splits it into the
/// `title_preview_frame` parts `server` sends.
///
/// # Errors
///
/// A `not_found` refusal for an input that is not a title with a loaded
/// template, `invalid_request` for a draft clip the template refuses or a
/// frame past its phase, and `render_failed` when the render fails.
pub(crate) fn render_preview(
    control: &ControlService<Policy>,
    project: &Project,
    assets: &TitleAssets,
    server: &ServerIdentity,
    request: &TitlePreviewRequest,
) -> Result<Vec<TitlePreviewFrameMessage>, StructuredError> {
    let input = request.input.to_domain();
    let title = title_render::title_of_input(project, input)
        .ok_or_else(|| refusal("not_found", format!("input {input} is not a title")))?;
    let engine = control
        .title_template(input)
        .ok_or_else(|| refusal("not_found", format!("title {input} has no loaded template")))?;
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
    let pose = pose(request.sample, &template.animation.clip)?;
    let time = title_render::TitleTime::now(
        control.frame_cursor(),
        project.settings().frame_rate,
        None,
        pose,
    );
    let pixels = title_render::render_title_preview(
        assets,
        &template,
        input,
        title,
        time,
        u32::from(request.width),
        u32::from(request.height),
    )
    .ok_or_else(|| refusal("render_failed", format!("title {input} did not render")))?;
    let qoi = encode_qoi_rgba(request.width, request.height, &pixels)
        .map_err(|error| refusal("render_failed", error.to_string()))?;
    Ok(title_preview_parts(
        server,
        &request.request_id,
        request.input,
        animation_revision,
        request.clip.is_some(),
        request.width,
        request.height,
        &qoi,
    ))
}
