//! Image and slideshow inputs between the engine and the wire (IN-008,
//! IN-009).

use fm_engine::{Engine, EngineSlideshowFrame};
use fm_model::InputSourceSpec;
use fm_protocol::{
    MAX_SLIDESHOW_SLIDES_BYTES, SlideshowPageStatus, SlideshowSlide, SlideshowSlidesMessage,
    SlideshowSlidesRequest, StillStateMessage, WireInputId, WireStillSource,
};

/// The wire source of an image or slideshow input; `None` for any other.
fn still_source(source: &InputSourceSpec) -> Option<WireStillSource> {
    match source {
        InputSourceSpec::Image(config) => Some(WireStillSource::Image(config.clone())),
        InputSourceSpec::Slideshow(config) => Some(WireStillSource::Slideshow(config.clone())),
        _ => None,
    }
}

/// How many `still_state` records follow a snapshot of `engine`.
pub(crate) fn still_state_count(engine: &Engine) -> u16 {
    let count = engine
        .show()
        .input_sources()
        .values()
        .filter(|source| still_source(source).is_some())
        .count();
    u16::try_from(count).expect("show inputs are bounded")
}

/// A slideshow's page status as the wire reports it.
pub(crate) fn page_status(engine: &Engine, input: fm_types::InputId) -> SlideshowPageStatus {
    engine
        .slideshow_status(input)
        .map(|status| SlideshowPageStatus {
            page: status.page,
            label: status.label,
            generation: status.generation,
        })
        .unwrap_or_default()
}

/// The `still_state` records of a snapshot of `engine`, ascending by input.
pub(crate) fn still_states(engine: &Engine) -> Vec<StillStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .input_sources()
        .iter()
        .filter_map(|(input, source)| {
            Some(StillStateMessage {
                revision,
                input: WireInputId::from_domain(*input),
                source: still_source(source)?,
                status: page_status(engine, *input),
            })
        })
        .collect()
}

/// The page status a slideshow frame reports.
pub(crate) fn frame_status(frame: &EngineSlideshowFrame) -> SlideshowPageStatus {
    SlideshowPageStatus {
        page: frame.page,
        label: frame.label.clone(),
        generation: frame.generation,
    }
}

/// Answers a listing request: as many slides from the requested offset as
/// the request and [`MAX_SLIDESHOW_SLIDES_BYTES`] allow.
pub(crate) fn slideshow_slides(
    engine: &Engine,
    request: &SlideshowSlidesRequest,
) -> SlideshowSlidesMessage {
    let input = request.input.to_domain();
    let listing = engine.slideshow_listing(input);
    let generation = engine
        .slideshow_status(input)
        .map_or(0, |status| status.generation);
    let mut slides = Vec::new();
    let mut bytes = 0usize;
    for slide in listing
        .iter()
        .skip(request.offset as usize)
        .take(request.limit as usize)
    {
        let size = slide.asset_uri.len() + slide.caption.as_ref().map_or(0, String::len);
        if bytes + size > MAX_SLIDESHOW_SLIDES_BYTES {
            break;
        }
        bytes += size;
        slides.push(SlideshowSlide {
            asset_uri: slide.asset_uri.to_string(),
            caption: slide.caption.clone(),
        });
    }
    SlideshowSlidesMessage {
        request_id: request.request_id.clone(),
        input: request.input,
        generation,
        offset: request.offset,
        total: u32::try_from(listing.len()).unwrap_or(u32::MAX),
        slides,
    }
}
