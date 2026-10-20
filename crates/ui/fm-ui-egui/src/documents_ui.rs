//! Document inputs in the Media side tab (IN-014): one card per PDF or
//! presentation input.
//!
//! Each card shows the replicated status (the document's state chip, its
//! title, and the server's message, such as what to install), the page
//! counter, Previous and Next keys, the current page's presenter notes, and
//! a strip of page thumbnails around the current page; a thumbnail click
//! goes to that page. Turning pages is an operator control
//! (`can_transition`); the page is saved with the project.
//!
//! Notes and thumbnails are not replicated: the card asks the server for
//! the ones it shows (`paged_page_request`) and draws the answers from
//! [`StudioUiState::paged_pages`]. A request is sent once per page, kind,
//! and document state, so a presentation that finishes converting asks
//! again for the thumbnails it could not have before.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Id, RichText, Ui, Vec2};
use fm_protocol::{
    DocumentState, PagedNavigation, PagedPageAnswer, PagedPageKey, PagedPageRequest, PagedStatus,
};
use fm_types::InputId;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::theme::{
    KeyIcon, Tone, add_icon_button, caption, card, mono, section_label, status_chip, text_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// Thumbnails in the strip: the current page and up to this many on each
/// side.
pub const THUMBNAIL_REACH: u32 = 3;
/// The strip's thumbnail size in points (half the wire thumbnail).
const THUMBNAIL_SIZE: Vec2 = Vec2::new(64.0, 36.0);

/// Why one document input's pages cannot be turned, if they cannot: the
/// session gates (operator control), then a page count not known yet.
#[must_use]
pub fn document_navigation_block(
    state: &StudioUiState,
    status: &PagedStatus,
) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    if !ready {
        return Some(unavailable_reason(state, Capability::Transition));
    }
    (status.count == 0).then_some(Msg::DocReasonPagesUnknown.en())
}

/// The pages of the thumbnail strip around `page` in a document of
/// `count` pages, ascending.
#[must_use]
pub fn strip_pages(page: u32, count: u32) -> std::ops::Range<u32> {
    if count == 0 {
        return 0..0;
    }
    let page = page.min(count - 1);
    let first = page.saturating_sub(THUMBNAIL_REACH);
    let end = page
        .saturating_add(THUMBNAIL_REACH)
        .saturating_add(1)
        .min(count);
    first..end
}

/// The `paged_page_request`s a card showing `status` still needs: the
/// current page's notes and the strip's thumbnails that have no answer
/// (an `Unavailable` answer counts as none while the document is ready)
/// and were not already asked for in this document state. Marks them
/// asked in `asked`.
#[must_use]
pub fn wanted_page_requests(
    state: &StudioUiState,
    status: &PagedStatus,
    asked: &mut BTreeSet<(PagedPageKey, DocumentState)>,
) -> Vec<PagedPageRequest> {
    let mut keys = Vec::new();
    if status.count > 0 {
        keys.push(PagedPageKey {
            input: status.input,
            page: status.page,
            thumbnail: false,
        });
        if status.state == DocumentState::Ready {
            keys.extend(
                strip_pages(status.page, status.count).map(|page| PagedPageKey {
                    input: status.input,
                    page,
                    thumbnail: true,
                }),
            );
        }
    }
    keys.into_iter()
        .filter(|key| match state.paged_pages.get(key) {
            None | Some(PagedPageAnswer::Unavailable) => asked.insert((*key, status.state)),
            Some(_) => false,
        })
        .map(|key| PagedPageRequest {
            request_id: format!(
                "studio-page-{}-{}-{}",
                key.input,
                key.page,
                if key.thumbnail { "thumbnail" } else { "notes" }
            ),
            input: key.input,
            page: key.page,
            thumbnail: key.thumbnail,
        })
        .collect()
}

fn state_chip(state: DocumentState) -> (Msg, Tone) {
    match state {
        DocumentState::Loading => (Msg::DocStateLoading, Tone::Pending),
        DocumentState::Converting => (Msg::DocStateConverting, Tone::Pending),
        DocumentState::Ready => (Msg::StingerReady, Tone::Ready),
        DocumentState::Unavailable => (Msg::StreamUnavailable, Tone::Fault),
        DocumentState::Failed => (Msg::ConnFailed, Tone::Fault),
    }
}

/// The document cards of the Media tab; nothing when the project has no
/// document inputs.
pub(crate) fn draw_documents(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let (Some(view), Some(project)) = (state.view.as_ref(), state.project_state.as_ref()) else {
        return;
    };
    let statuses = project.paged_statuses().cloned().collect::<Vec<_>>();
    if statuses.is_empty() {
        return;
    }
    let l = l10n::locale(ui.ctx());
    ui.label(section_label(l.text(Msg::DocHeading)));
    ui.add(egui::Label::new(caption(l.text(Msg::DocIntro))).wrap());
    ui.add_space(2.0);
    let asked_id = Id::new("studio-document-page-requests");
    let mut asked = ui
        .data(|data| data.get_temp::<Arc<BTreeSet<(PagedPageKey, DocumentState)>>>(asked_id))
        .map(|asked| (*asked).clone())
        .unwrap_or_default();
    // Forget what was asked of inputs that are gone.
    asked.retain(|(key, _)| statuses.iter().any(|status| status.input == key.input));
    for status in &statuses {
        intents.extend(
            wanted_page_requests(state, status, &mut asked)
                .into_iter()
                .map(StudioIntent::PagedPage),
        );
        let input = status.input.to_domain();
        let name = input_name(l, &view.inputs, &view.input_names, input);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_card(ui, l, state, (input, &name), status, intents);
        });
        ui.add_space(6.0);
    }
    ui.data_mut(|data| data.insert_temp(asked_id, Arc::new(asked)));
}

#[allow(clippy::too_many_lines)] // one document card
fn draw_card(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    (input, name): (InputId, &str),
    status: &PagedStatus,
    intents: &mut Vec<StudioIntent>,
) {
    let block = document_navigation_block(state, status);
    let mut push = |navigation| intents.push(StudioIntent::PagedInput { input, navigation });

    // Name, state, and title.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(name).strong().color(text_color()));
        let (label, tone) = state_chip(status.state);
        status_chip(ui, l.text(label), tone, 104.0).on_hover_text(l.text(Msg::DocStateHover));
    });
    if let Some(title) = &status.title {
        ui.label(caption(title.as_str()))
            .on_hover_text(l.text(Msg::DocTitleHover));
    }
    if let Some(message) = &status.message {
        ui.add(egui::Label::new(caption(message.as_str())).wrap())
            .on_hover_text(l.text(Msg::DocMessageHover));
    }

    // Previous, the page counter, and Next.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let enabled = block.is_none();
        let reason = block.unwrap_or("");
        let key = || egui::Button::new("").min_size(Vec2::new(32.0, 24.0));
        if add_icon_button(ui, enabled, key(), KeyIcon::Back, l.text(Msg::DocPrevious))
            .hint(l.text(Msg::DocPreviousHover), reason)
            .clicked()
        {
            push(PagedNavigation::Previous);
        }
        let counter = if status.count == 0 {
            l.format(
                Msg::DocPageUnknownCount,
                &[("page", Arg::from(status.page.saturating_add(1)))],
            )
        } else {
            l.format(
                Msg::DocPageCounter,
                &[
                    ("page", Arg::from(status.page.saturating_add(1))),
                    ("count", Arg::from(status.count)),
                ],
            )
        };
        ui.label(mono(counter).size(crate::theme::legible(12.0)))
            .on_hover_text(l.text(Msg::DocPageCounterHover));
        if add_icon_button(ui, enabled, key(), KeyIcon::Play, l.text(Msg::DocNext))
            .hint(l.text(Msg::DocNextHover), reason)
            .clicked()
        {
            push(PagedNavigation::Next);
        }
    });

    // The thumbnail strip around the current page.
    if status.state == DocumentState::Ready && status.count > 0 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
            for page in strip_pages(status.page, status.count) {
                let key = PagedPageKey {
                    input: status.input,
                    page,
                    thumbnail: true,
                };
                let texture = thumbnail_texture(ui, key, state.paged_pages.get(&key));
                let mut button = match &texture {
                    Some(texture) => egui::Button::image(
                        egui::Image::new(texture).fit_to_exact_size(THUMBNAIL_SIZE),
                    ),
                    None => egui::Button::new(mono((page + 1).to_string())),
                }
                .min_size(THUMBNAIL_SIZE);
                if page == status.page {
                    button = button.stroke(egui::Stroke::new(2.0, crate::theme::PROGRAM));
                }
                let response = ui.add_enabled(block.is_none(), button).hint(
                    &l.format(Msg::DocThumbnailHover, &[("page", Arg::from(page + 1))]),
                    block.unwrap_or(""),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        block.is_none(),
                        l.format(Msg::DocThumbnailHover, &[("page", Arg::from(page + 1))]),
                    )
                });
                if response.clicked() && page != status.page {
                    push(PagedNavigation::Goto(page));
                }
            }
        });
    }

    // The current page's presenter notes.
    ui.add_space(2.0);
    ui.label(section_label(l.text(Msg::DocNotes)))
        .on_hover_text(l.text(Msg::DocNotesHover));
    let notes_key = PagedPageKey {
        input: status.input,
        page: status.page,
        thumbnail: false,
    };
    let notes = match state.paged_pages.get(&notes_key) {
        Some(PagedPageAnswer::Notes(notes)) if !notes.is_empty() => notes.clone(),
        Some(PagedPageAnswer::Notes(_)) => l.text(Msg::DocNotesNone).to_owned(),
        Some(PagedPageAnswer::Unavailable | PagedPageAnswer::Thumbnail { .. }) => {
            l.text(Msg::DocNotesUnavailable).to_owned()
        }
        None => l.text(Msg::DocNotesLoading).to_owned(),
    };
    egui::ScrollArea::vertical()
        .id_salt(("studio-document-notes", input))
        .max_height(96.0)
        .show(ui, |ui| {
            ui.add(egui::Label::new(caption(notes)).wrap());
        });
}

/// The texture of one answered thumbnail, uploaded once per answer.
fn thumbnail_texture(
    ui: &Ui,
    key: PagedPageKey,
    answer: Option<&PagedPageAnswer>,
) -> Option<egui::TextureHandle> {
    let Some(PagedPageAnswer::Thumbnail {
        width,
        height,
        rgba,
    }) = answer
    else {
        return None;
    };
    // The answer's pixels identify it: a re-asked page gets a new buffer.
    let id = Id::new((
        "studio-document-thumbnail",
        key,
        Arc::as_ptr(rgba).cast::<u8>() as usize,
    ));
    if let Some(texture) = ui.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return Some(texture);
    }
    let image =
        egui::ColorImage::from_rgba_unmultiplied([usize::from(*width), usize::from(*height)], rgba);
    let texture = ui.ctx().load_texture(
        format!("studio-document-{}-{}", key.input, key.page),
        image,
        egui::TextureOptions::LINEAR,
    );
    ui.data_mut(|data| data.insert_temp(id, texture.clone()));
    Some(texture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_protocol::{PagedKind, WireInputId};

    fn status(page: u32, count: u32, state: DocumentState) -> PagedStatus {
        PagedStatus {
            input: WireInputId::from_domain(InputId::new(std::num::NonZeroU128::new(4).unwrap())),
            kind: PagedKind::Document,
            page,
            count,
            state,
            title: None,
            message: None,
        }
    }

    #[test]
    fn the_strip_centres_on_the_page_and_stays_inside_the_document() {
        assert_eq!(strip_pages(0, 0), 0..0);
        assert_eq!(strip_pages(0, 2), 0..2);
        assert_eq!(strip_pages(5, 20), 2..9);
        assert_eq!(strip_pages(19, 20), 16..20);
        assert_eq!(strip_pages(40, 20), 16..20);
    }

    #[test]
    fn navigation_waits_for_the_session_and_the_page_count() {
        let ready = status(0, 3, DocumentState::Ready);
        let disconnected = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(
            document_navigation_block(&disconnected, &ready),
            Some(Msg::ReasonNoConnection.en())
        );
        let mut state =
            StudioUiState::new(StudioConnectionStatus::Ready).with_switcher_permissions(true, true);
        state.view = Some(crate::tests::recording_view([false, false]));
        assert_eq!(document_navigation_block(&state, &ready), None);
        assert_eq!(
            document_navigation_block(&state, &status(0, 0, DocumentState::Loading)),
            Some(Msg::DocReasonPagesUnknown.en())
        );
        let viewer = StudioUiState {
            view: state.view.clone(),
            ..StudioUiState::new(StudioConnectionStatus::Ready)
        };
        assert!(document_navigation_block(&viewer, &ready).is_some());
    }

    #[test]
    fn pages_are_asked_for_once_per_document_state() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        let mut asked = BTreeSet::new();
        // Without a converter only the notes can be had.
        let unavailable = status(1, 10, DocumentState::Unavailable);
        let first = wanted_page_requests(&state, &unavailable, &mut asked);
        assert_eq!(first.len(), 1);
        assert!(!first[0].thumbnail);
        assert_eq!(first[0].page, 1);
        assert_eq!(
            wanted_page_requests(&state, &unavailable, &mut asked).len(),
            0
        );
        // Ready: the notes again (the answer was unavailable) and the strip.
        state.paged_pages.insert(
            PagedPageKey {
                input: unavailable.input,
                page: 1,
                thumbnail: false,
            },
            PagedPageAnswer::Unavailable,
        );
        let ready = status(1, 10, DocumentState::Ready);
        let second = wanted_page_requests(&state, &ready, &mut asked);
        assert_eq!(second.iter().filter(|request| request.thumbnail).count(), 5);
        assert_eq!(
            second.iter().filter(|request| !request.thumbnail).count(),
            1
        );
        // Answered pages are not asked for again.
        for request in &second {
            state.paged_pages.insert(
                PagedPageKey {
                    input: request.input,
                    page: request.page,
                    thumbnail: request.thumbnail,
                },
                PagedPageAnswer::Notes(String::new()),
            );
        }
        asked.clear();
        assert_eq!(wanted_page_requests(&state, &ready, &mut asked).len(), 0);
        // No page count, nothing to ask.
        assert_eq!(
            wanted_page_requests(&state, &status(0, 0, DocumentState::Loading), &mut asked).len(),
            0
        );
    }
}
