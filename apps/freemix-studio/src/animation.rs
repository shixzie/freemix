//! Host side of the Studio Animation window: replicated title layouts as
//! window designs, and title and scene animation save refusals and exact
//! preview errors mapped for the UI. Scene designs need no host step: the
//! window reads each replicated `scene_layout_state` itself.

use fm_client::Client;
use fm_protocol::{CommandPayload, CommandResult, ErrorMessage};
use fm_ui_egui::{StudioShell, StudioUiState, TitleAnimationRefusal};
use fm_ui_model::scene_designer::preview::{KeyPicturePacer, PreviewPacer};
use fm_ui_model::title_animation::{TitleDesign, preview_request_id};

/// The refusal of a title or scene animation edit, naming its input and
/// whether it was refused as based on a stale animation revision.
pub(crate) fn animation_refusal(
    client: &Client,
    command_id: &str,
    result: &CommandResult,
) -> Option<TitleAnimationRefusal> {
    let CommandResult::Rejected { fields, .. } = result else {
        return None;
    };
    let input = match &client.command(command_id)?.command.payload {
        CommandPayload::SetTitleAnimation { input, .. }
        | CommandPayload::ApplyTitleAnimationPreset { input, .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { input, .. }
        | CommandPayload::StretchTitleAnimation { input, .. }
        | CommandPayload::SetSceneAnimation { input, .. }
        | CommandPayload::ApplySceneAnimationPreset { input, .. }
        | CommandPayload::ApplySceneAnimationUserPreset { input, .. }
        | CommandPayload::StretchSceneAnimation { input, .. } => input.to_domain(),
        _ => return None,
    };
    Some(TitleAnimationRefusal {
        command_id: command_id.to_owned(),
        input,
        conflict: fields.iter().any(|issue| issue.code == "revision_conflict"),
    })
}

/// The exact preview a server error answers, if it answers one: the
/// Animation window's or the layer designer's (its canvas picture or the
/// selected layer's input picture).
pub(crate) fn preview_error_id(error: &ErrorMessage) -> Option<&str> {
    error.request_id.as_deref().filter(|id| {
        preview_request_id(id).is_some() || PreviewPacer::owns(id) || KeyPicturePacer::owns(id)
    })
}

/// Hands every replicated title layout to the Animation window as its
/// design (Protocol 2.30), when it is new or changed; titles without a
/// loaded template have no design.
pub(crate) fn publish_title_designs(shell: &mut StudioShell, state: &StudioUiState) {
    let Some(project) = &state.project_state else {
        return;
    };
    for title in project.titles() {
        let Some(design) = title.layout.as_deref().and_then(TitleDesign::from_layout) else {
            continue;
        };
        if shell.title_design(title.input) != Some(&design) {
            shell.set_title_design(title.input, design);
        }
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::{ErrorMessage, StructuredError};

    use super::preview_error_id;

    fn error(request_id: Option<&str>) -> ErrorMessage {
        ErrorMessage {
            request_id: request_id.map(str::to_owned),
            current_revision: None,
            error: StructuredError {
                code: "rate_limited".into(),
                message: "too many previews".into(),
                fields: Vec::new(),
                retryable: true,
            },
        }
    }

    #[test]
    fn animation_and_layer_designer_preview_errors_answer_their_previews() {
        let animation = error(Some("anim-preview-4"));
        assert_eq!(preview_error_id(&animation), Some("anim-preview-4"));
        let designer = error(Some("design-preview-2"));
        assert_eq!(preview_error_id(&designer), Some("design-preview-2"));
        let key_picture = error(Some("design-key-picture-3"));
        assert_eq!(preview_error_id(&key_picture), Some("design-key-picture-3"));
        assert_eq!(preview_error_id(&error(Some("cmd-7"))), None);
        assert_eq!(preview_error_id(&error(None)), None);
    }
}
