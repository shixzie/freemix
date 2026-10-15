//! Exact scene previews (GX-011, Protocol 2.32).
//!
//! A `scene_preview_request` asks the authority to compose one Scene input
//! exactly as it shows it: the scene the engine holds now (or with the
//! request's unsaved draft clip instead of its animation), posed at one
//! point of its animation ([`fm_model::Scene::posed`], arbitrary rotation
//! included), composed by the same CPU reference compositor and project-to-
//! grid mapping the simulated monitor tap uses
//! ([`scene_render::render_scene_model`]) directly at the requested size,
//! from the project's simulated sources at the engine frame cursor.
//!
//! The answer is one four-channel QOI stream of straight RGBA8 pixels (at
//! most 640x360) split into `scene_preview_frame` parts, rendered off the
//! session loop by the same latest-wins worker, lossy slot, and per-session
//! preview rate as title previews ([`crate::title_preview`]). A scene the
//! reference compositor cannot draw honestly (a layer naming a nested scene
//! or a non-simulated input, an effect it rejects) is refused with
//! `render_failed` rather than approximated.

use fm_auth::Policy;
use fm_control::{ControlService, scene_clip_from_wire};
use fm_model::Project;
use fm_model::fm_animation::ClipState;
use fm_protocol::{
    ScenePreviewFrameMessage, ScenePreviewRequest, ServerIdentity, StructuredError, WireInputId,
    encode_qoi_rgba, title_preview_parts,
};
use fm_types::InputId;

use crate::scene_render;
use crate::title_preview::{refusal, sample_pose};

/// One exact scene preview, resolved on the session loop and composed off
/// it ([`ScenePreviewJob::render`]).
pub(crate) struct ScenePreviewJob {
    server: ServerIdentity,
    pub(crate) request_id: String,
    wire_input: WireInputId,
    input: InputId,
    /// The project whose inputs feed the layers.
    project: Project,
    /// The scene as the engine holds it, with any draft clip.
    scene: fm_model::Scene,
    pose: ClipState,
    frame: u64,
    animation_revision: u64,
    draft: bool,
    width: u16,
    height: u16,
}

impl ScenePreviewJob {
    /// Composes the preview and splits it into the `scene_preview_frame`
    /// parts its server sends.
    ///
    /// # Errors
    ///
    /// A `render_failed` refusal when the scene cannot be composed exactly
    /// or its encoding fails.
    pub(crate) fn render(&self) -> Result<Vec<ScenePreviewFrameMessage>, StructuredError> {
        let input = self.input;
        let unrenderable = || {
            refusal(
                "render_failed",
                format!(
                    "scene {input} cannot be composed exactly (only simulated sources are \
                     previewed)"
                ),
            )
        };
        let pipeline = scene_render::simulated_pipeline(
            &self.project,
            u32::from(self.width),
            u32::from(self.height),
        )
        .ok_or_else(unrenderable)?;
        let image = scene_render::render_scene_model(
            &self.project,
            &self.scene,
            &pipeline,
            self.frame,
            self.pose,
        )
        .ok_or_else(unrenderable)?;
        let rgba = straight_rgba(image.pixels(), image.stride(), self.width, self.height)
            .ok_or_else(unrenderable)?;
        let qoi = encode_qoi_rgba(self.width, self.height, &rgba)
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

/// The composed premultiplied picture as tightly packed straight RGBA8
/// (each channel `round(c * 255 / a)`; transparent pixels stay zero).
fn straight_rgba(pixels: &[u8], stride: usize, width: u16, height: u16) -> Option<Vec<u8>> {
    let row_bytes = usize::from(width) * 4;
    let mut rgba = Vec::with_capacity(row_bytes * usize::from(height));
    for row in pixels.chunks(stride).take(usize::from(height)) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            let alpha = pixel[3];
            let straight = |channel: u8| match alpha {
                0 => 0,
                u8::MAX => channel,
                _ => {
                    let value =
                        (u16::from(channel) * 255 + u16::from(alpha) / 2) / u16::from(alpha);
                    u8::try_from(value).unwrap_or(u8::MAX)
                }
            };
            rgba.extend_from_slice(&[
                straight(pixel[0]),
                straight(pixel[1]),
                straight(pixel[2]),
                alpha,
            ]);
        }
    }
    (rgba.len() == row_bytes * usize::from(height)).then_some(rgba)
}

/// Resolves the exact scene preview `request` asks for: everything but the
/// composition, which [`ScenePreviewJob::render`] does off the session
/// loop.
///
/// # Errors
///
/// A `not_found` refusal for an input that is not a Scene input, and
/// `invalid_request` for a draft clip the scene refuses or a frame past its
/// phase.
pub(crate) fn prepare_scene_preview(
    control: &ControlService<Policy>,
    project: &Project,
    server: &ServerIdentity,
    request: &ScenePreviewRequest,
) -> Result<ScenePreviewJob, StructuredError> {
    let input = request.input.to_domain();
    let engine = control
        .scene_input(input)
        .ok_or_else(|| refusal("not_found", format!("input {input} is not a Scene input")))?;
    let animation_revision = engine.animation().revision;
    let mut scene = (*engine.scene).clone();
    if let Some(draft) = &request.clip {
        let draft = scene_clip_from_wire(draft)
            .map_err(|error| refusal("invalid_request", error.to_string()))?;
        scene
            .edit_animation(|clip| {
                *clip = draft;
                Ok(())
            })
            .map_err(|error| refusal("invalid_request", error.to_string()))?;
    }
    let pose = sample_pose(request.sample, &scene.animation.clip)?;
    Ok(ScenePreviewJob {
        server: server.clone(),
        request_id: request.request_id.clone(),
        wire_input: request.input,
        input,
        project: project.clone(),
        scene,
        pose,
        frame: control.frame_cursor(),
        animation_revision,
        draft: request.clip.is_some(),
        width: request.width,
        height: request.height,
    })
}

#[cfg(test)]
mod tests {
    use super::straight_rgba;

    #[test]
    fn premultiplied_pixels_are_sent_straight() {
        let pixels = [255, 0, 0, 255, 64, 32, 0, 128, 0, 0, 0, 0, 9, 9, 9, 9];
        assert_eq!(
            straight_rgba(&pixels, 8, 2, 2).unwrap(),
            vec![
                255, 0, 0, 255, 128, 64, 0, 128, 0, 0, 0, 0, 255, 255, 255, 9
            ]
        );
        assert_eq!(straight_rgba(&pixels, 8, 3, 2), None);
    }
}
