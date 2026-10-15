//! The `title_layout_state` records that follow a snapshot (Protocol 2.30,
//! GX-011): each title template's canvas and element outlines, derived from
//! its `fm_titles` template, so a remote animation editor can list,
//! outline, and apply presets to every element.

use fm_engine::Engine;
use fm_protocol::{
    MAX_TITLE_CANVAS_SIDE, MAX_TITLE_ELEMENT_NAME_BYTES, MAX_TITLE_LAYOUT_ELEMENTS,
    TitleLayoutStateMessage, WireInputId, WireTitleElementId, WireTitleElementKind,
    WireTitleElementOutline, WireTitleLayout,
};
use fm_titles::fm_animation::Rgba;
use fm_titles::{ElementKind, TitleTemplate};

/// `name` as a wire display name: control characters become spaces and the
/// result is cut at a character boundary within
/// [`MAX_TITLE_ELEMENT_NAME_BYTES`].
fn display_name(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if out.len() + character.len_utf8() > MAX_TITLE_ELEMENT_NAME_BYTES {
            break;
        }
        out.push(character);
    }
    out
}

const fn element_kind(kind: &ElementKind) -> WireTitleElementKind {
    match kind {
        ElementKind::Text { .. } => WireTitleElementKind::Text,
        ElementKind::Rectangle => WireTitleElementKind::Rectangle,
        ElementKind::Image { .. } => WireTitleElementKind::Image,
        ElementKind::Clock(_) => WireTitleElementKind::Clock,
        ElementKind::ProductionClock(_) => WireTitleElementKind::ProductionClock,
        ElementKind::Ticker(_) => WireTitleElementKind::Ticker,
    }
}

/// `template`'s wire layout: its canvas and its first
/// [`MAX_TITLE_LAYOUT_ELEMENTS`] elements in template order, each with its
/// display name, kind, bounds, opacity, fill, and visibility. A canvas
/// beyond [`MAX_TITLE_CANVAS_SIDE`] (which `fm_titles` cannot render) is
/// clamped to it.
#[must_use]
pub fn title_layout_to_wire(template: &TitleTemplate) -> WireTitleLayout {
    let side = |value: u32| value.clamp(1, MAX_TITLE_CANVAS_SIDE);
    WireTitleLayout {
        width: side(template.width),
        height: side(template.height),
        elements: template
            .elements
            .iter()
            .take(MAX_TITLE_LAYOUT_ELEMENTS)
            .map(|element| {
                let fill = element.style.fill;
                WireTitleElementOutline {
                    id: WireTitleElementId::new(element.id.get()),
                    name: display_name(&element.name),
                    kind: element_kind(&element.kind),
                    x: element.bounds.x,
                    y: element.bounds.y,
                    width: element.bounds.width,
                    height: element.bounds.height,
                    opacity: element.style.opacity,
                    fill: Rgba::new(fill.r, fill.g, fill.b, fill.a),
                    visible: element.visible,
                }
            })
            .collect(),
    }
}

/// One `title_layout_state` per announced title, in snapshot order; a
/// title without a loaded template reports the empty layout.
pub fn title_layout_states(engine: &Engine) -> Vec<TitleLayoutStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .titles()
        .iter()
        .map(|(input, title)| TitleLayoutStateMessage {
            revision,
            input: WireInputId::from_domain(*input),
            layout: Box::new(
                title
                    .template
                    .as_ref()
                    .map(|template| title_layout_to_wire(&template.template))
                    .unwrap_or_default(),
            ),
        })
        .collect()
}
