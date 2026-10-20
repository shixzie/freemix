//! Paged inputs (IN-014): document inputs stepped page by page.
//!
//! A document input's current page is durable show state
//! ([`crate::ShowState::paged_page`]): a new or replaced document starts on
//! its first page, and the generic page step
//! ([`crate::EngineCommand::PageNavigate`], shared with slideshows) moves it. Its page count is
//! engine runtime: the media plane reads the document and reports the count
//! ([`crate::Engine::set_paged_count`]); it is never snapshotted. A
//! navigation is resolved against that count by
//! [`PagedNavigation::target`], so it clamps at both ends and every paged
//! kind steps the same way. Navigation is refused (retryably) while the
//! count is unknown, so the page never points past a document the engine
//! has not seen. Journal replay sets every document's count before it
//! replays a command, so a replayed navigation lands where it first did.
//!
//! Navigation is a live operator control: it honours no padlock and has no
//! undo entry.

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_types::InputId;
use fm_types::paged::PagedNavigation;

use crate::ShowState;

/// A paged input's page and what the engine knows of its page count.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EnginePagedStatus {
    pub input: InputId,
    /// The persisted zero-based page.
    pub page: u32,
    /// The source's page count, once reported.
    pub count: Option<u32>,
}

/// The page `navigation` moves document input `input` to in `state`, given
/// its reported page `count`, or `None` when `input` is not a document.
pub(crate) fn accept(
    state: &ShowState,
    input: InputId,
    navigation: PagedNavigation,
    count: Option<u32>,
) -> Option<Result<u32, Rejection>> {
    let page = state.paged_page(input)?;
    Some(document_target(input, navigation, page, count))
}

fn document_target(
    input: InputId,
    navigation: PagedNavigation,
    page: u32,
    count: Option<u32>,
) -> Result<u32, Rejection> {
    let Some(count) = count.filter(|count| *count > 0) else {
        return Err(Rejection::new(
            RejectionCode::Unavailable,
            format!(
                "input {input}'s pages are not known yet; try again once the document has loaded"
            ),
        )
        .with_field(FieldIssue::new(
            "input",
            "pages_unknown",
            "the document has not loaded yet",
        ))
        .retryable(true));
    };
    Ok(navigation.target(page, count))
}
