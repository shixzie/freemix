//! Paged inputs between the engine and the wire (IN-014).
//!
//! The engine holds each document input's page and its reported page count;
//! the control service holds what the media plane last said about the
//! source (its [`DocumentState`], title, and message). A wire
//! [`PagedStatus`] joins the two.

use std::collections::BTreeMap;

use fm_engine::{Engine, EngineCommand, EnginePagedCommand};
use fm_protocol::{
    DocumentState, EventPayload, MAX_PAGED_MESSAGE_BYTES, MAX_PAGED_TITLE_BYTES, PagedKind,
    PagedNavigation, PagedStateMessage, PagedStatus, WireInputId, bounded_paged_text,
};
use fm_types::InputId;

/// What the media plane reports about one paged input's source.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PagedSourceReport {
    /// The page count, once the document has been read.
    pub count: Option<u32>,
    pub state: DocumentState,
    pub title: Option<String>,
    pub message: Option<String>,
}

/// The report's text, bounded for the wire.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PagedSource {
    pub(super) state: DocumentState,
    pub(super) title: Option<String>,
    pub(super) message: Option<String>,
}

impl PagedSource {
    pub(super) fn from_report(report: &PagedSourceReport) -> Self {
        let bound = |text: &Option<String>, maximum| {
            text.as_deref()
                .map(|text| bounded_paged_text(text, maximum, false))
                .filter(|text| !text.is_empty())
        };
        Self {
            state: report.state,
            title: bound(&report.title, MAX_PAGED_TITLE_BYTES),
            message: bound(&report.message, MAX_PAGED_MESSAGE_BYTES),
        }
    }
}

/// Paged input `input`'s wire status, or `None` when it is not paged.
pub(super) fn wire_status(
    engine: &Engine,
    sources: &BTreeMap<InputId, PagedSource>,
    input: InputId,
) -> Option<PagedStatus> {
    let status = engine.paged_status(input)?;
    let source = sources.get(&input).cloned().unwrap_or_default();
    let count = status.count.unwrap_or(0);
    Some(PagedStatus {
        input: WireInputId::from_domain(input),
        kind: PagedKind::Document,
        page: if count == 0 {
            status.page
        } else {
            status.page.min(count - 1)
        },
        count,
        state: source.state,
        title: source.title,
        message: source.message,
    })
}

/// How many `paged_state` records follow a snapshot.
pub(super) fn paged_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().paged_inputs().len())
        .unwrap_or(u16::MAX)
        .min(fm_protocol::MAX_PAGED_STATES)
}

/// The `paged_state` records after a snapshot, ascending by input.
pub(super) fn paged_states(
    engine: &Engine,
    sources: &BTreeMap<InputId, PagedSource>,
) -> Vec<PagedStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .paged_inputs()
        .keys()
        .take(usize::from(fm_protocol::MAX_PAGED_STATES))
        .filter_map(|input| wire_status(engine, sources, *input))
        .map(|status| PagedStateMessage { revision, status })
        .collect()
}

/// The engine command of a wire `paged_input`.
pub(super) const fn engine_command(
    input: WireInputId,
    navigation: PagedNavigation,
) -> EngineCommand {
    EngineCommand::Paged(EnginePagedCommand {
        input: input.to_domain(),
        navigation,
    })
}

/// The durable event of an accepted `paged_input`.
pub(super) fn paged_changed(
    engine: &Engine,
    sources: &BTreeMap<InputId, PagedSource>,
    input: InputId,
    navigation: PagedNavigation,
) -> Option<EventPayload> {
    let status = wire_status(engine, sources, input)?;
    Some(EventPayload::PagedChanged {
        navigation,
        status: Box::new(status),
    })
}
