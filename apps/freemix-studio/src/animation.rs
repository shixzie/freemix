//! Host side of the Studio Animation window: mapping title animation save
//! refusals and exact preview errors for the UI.

use fm_client::Client;
use fm_protocol::{CommandPayload, CommandResult, ErrorMessage};
use fm_ui_egui::TitleAnimationRefusal;
use fm_ui_model::title_animation::preview_request_id;

/// The refusal of a title animation edit, naming its title and whether it
/// was refused as based on a stale animation revision.
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
        | CommandPayload::StretchTitleAnimation { input, .. } => input.to_domain(),
        _ => return None,
    };
    Some(TitleAnimationRefusal {
        command_id: command_id.to_owned(),
        input,
        conflict: fields.iter().any(|issue| issue.code == "revision_conflict"),
    })
}

/// The exact preview a server error answers, if it answers one.
pub(crate) fn preview_error_id(error: &ErrorMessage) -> Option<&str> {
    error
        .request_id
        .as_deref()
        .filter(|id| preview_request_id(id).is_some())
}
