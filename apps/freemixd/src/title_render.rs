//! Simulated rendering of title inputs for the monitor tap.
//!
//! Title templates and their fonts are loaded once at daemon startup
//! ([`TitleAssets::load`]): one shared template and asset catalog per stored
//! template URI any title input names. Image fields are registered by name
//! only, so an image element draws the reference renderer's placeholder; no
//! image is decoded yet. A template that does not load, or a font that does
//! not read or parse, is left out: the input then renders nothing (or draws
//! without that font) instead of an invented picture.
//!
//! A title renders by instantiating its template, applying the input's
//! persisted field values, rendering at the template size with the
//! `fm_titles` reference renderer, and scaling nearest-neighbor onto the
//! fitted monitor grid. The scene time of a title is the frame cursor times
//! the project frame duration, a documented approximation: the simulated
//! daemon has no title playback state, so every title is treated as having
//! started at frame zero.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::sync::Arc;

use fm_model::{InputKind, Project, TitleFieldEntry, TitleFieldValue, TitleInput};
use fm_persistence::ProjectStore;
use fm_titles::{
    AssetCatalog, Color, ElementKind, FieldId, FieldValue, ImageSource, MAX_FONT_BYTES,
    ReferenceRenderer, TitleId, TitleTemplate,
};
use fm_types::InputId;
use fm_video::ImageFrame;

/// One loaded template and the assets it renders with.
pub(crate) struct LoadedTitleTemplate {
    template: Arc<TitleTemplate>,
    catalog: AssetCatalog,
    /// Whether a rendered picture depends on scene time (clock, ticker, or
    /// animated elements), so the frame cursor belongs in its cache key.
    time_dependent: bool,
}

impl LoadedTitleTemplate {
    pub(crate) fn new(template: TitleTemplate, catalog: AssetCatalog) -> Self {
        let time_dependent = template.elements.iter().any(|element| {
            !element.animations.is_empty()
                || matches!(
                    element.kind,
                    ElementKind::Clock(_)
                        | ElementKind::Ticker(_)
                        | ElementKind::ProductionClock(_)
                )
        });
        Self {
            template: Arc::new(template),
            catalog,
            time_dependent,
        }
    }

    pub(crate) const fn time_dependent(&self) -> bool {
        self.time_dependent
    }
}

/// Every title template the project's title inputs name, keyed by URI.
#[derive(Default)]
pub(crate) struct TitleAssets {
    templates: BTreeMap<String, LoadedTitleTemplate>,
}

impl TitleAssets {
    /// Loads each distinct template named by a title input of `project`,
    /// with its fonts and the names of every image asset its defaults and
    /// the inputs' persisted values reference.
    pub(crate) fn load(store: &ProjectStore, project: &Project) -> Self {
        let mut assets = Self::default();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            if !assets.templates.contains_key(&title.template_uri) {
                let Ok(stored) = store.load_title_template(&title.template_uri) else {
                    continue;
                };
                let mut catalog = AssetCatalog::new();
                for font in &stored.fonts {
                    let Some(bytes) = read_font(store, &font.asset_uri) else {
                        continue;
                    };
                    // `with_font_bytes` consumes the catalog; parse first so
                    // an unusable font never loses the fonts already loaded.
                    if let Ok(face) = fm_titles::FontFace::from_bytes(bytes) {
                        catalog = catalog.with_font(font.family.clone(), face);
                    }
                }
                for field in &stored.template.fields {
                    if let FieldValue::Image(image) = &field.default {
                        catalog = catalog.with_image(image.asset.clone());
                    }
                }
                assets.templates.insert(
                    title.template_uri.clone(),
                    LoadedTitleTemplate::new(stored.template, catalog),
                );
            }
            if let Some(loaded) = assets.templates.get_mut(&title.template_uri) {
                for entry in &title.fields {
                    if let TitleFieldValue::Image { asset_uri } = &entry.value {
                        loaded.catalog =
                            std::mem::take(&mut loaded.catalog).with_image(asset_uri.clone());
                    }
                }
            }
        }
        assets
    }

    /// Registers an already loaded template under `uri`.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, uri: impl Into<String>, template: LoadedTitleTemplate) {
        self.templates.insert(uri.into(), template);
    }

    fn template(&self, uri: &str) -> Option<&LoadedTitleTemplate> {
        self.templates.get(uri)
    }
}

/// Reads one font asset, refusing anything larger than a font may be.
fn read_font(store: &ProjectStore, uri: &str) -> Option<Vec<u8>> {
    let path = store.resolve_asset_uri(uri).ok()?;
    let limit = u64::try_from(MAX_FONT_BYTES).ok()?;
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_FONT_BYTES).then_some(bytes)
}

/// Returns the title input `input` names, or `None` for any other input.
pub(crate) fn title_of_input(project: &Project, input: InputId) -> Option<&TitleInput> {
    project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .and_then(|input| match &input.kind {
            InputKind::Title(title) => Some(title),
            _ => None,
        })
}

/// Everything one rendered title picture depends on. The frame is present
/// only for a time-dependent template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TitleKey {
    template_uri: String,
    field_revision: u64,
    fields: Vec<TitleFieldEntry>,
    frame: Option<u64>,
}

/// The cache key of `title` rendered at `frame`, or `None` when its template
/// was not loaded.
pub(crate) fn title_key(assets: &TitleAssets, title: &TitleInput, frame: u64) -> Option<TitleKey> {
    let loaded = assets.template(&title.template_uri)?;
    Some(TitleKey {
        template_uri: title.template_uri.clone(),
        field_revision: title.field_revision,
        fields: title.fields.clone(),
        frame: loaded.time_dependent().then_some(frame),
    })
}

/// Renders `title` at `frame` and scales it to `width`x`height`.
///
/// The result is premultiplied RGBA8 that keeps the template's alpha, ready
/// to be layered as an overlay or flattened over black. Returns `None` when
/// the template was not loaded or does not instantiate, a persisted field
/// does not match the template, or rendering fails.
pub(crate) fn render_title(
    assets: &TitleAssets,
    project: &Project,
    input: InputId,
    title: &TitleInput,
    frame: u64,
    width: u32,
    height: u32,
) -> Option<ImageFrame> {
    let loaded = assets.template(&title.template_uri)?;
    let mut scene = loaded
        .template
        .instantiate(TitleId::new(input.get()))
        .ok()?;
    let updates = title
        .fields
        .iter()
        .map(|entry| (FieldId::new(entry.id.get()), field_value(&entry.value)))
        .collect::<Vec<_>>();
    scene.update_fields(scene.revision(), &updates).ok()?;
    let output = ReferenceRenderer
        .render(&scene, scene_time_ms(project, frame), &loaded.catalog)
        .ok()?;
    if output.frame.width() == width && output.frame.height() == height {
        return Some(output.frame);
    }
    fm_video::scale_nearest(&output.frame, width, height).ok()
}

/// Scene time at `frame`: `frame * 1000 * denominator / numerator`
/// milliseconds of the project frame rate, saturating.
pub(crate) fn scene_time_ms(project: &Project, frame: u64) -> u64 {
    let rate = project.settings().frame_rate;
    let numerator = u128::from(rate.numerator()).max(1);
    let millis = u128::from(frame) * 1_000 * u128::from(rate.denominator()) / numerator;
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Composites a premultiplied picture over opaque black.
pub(crate) fn flatten_over_black(frame: &ImageFrame) -> Option<ImageFrame> {
    let row_bytes = usize::try_from(frame.width()).ok()?.checked_mul(4)?;
    let mut pixels = frame.pixels().to_vec();
    for row in pixels.chunks_mut(frame.stride()) {
        let end = row_bytes.min(row.len());
        for pixel in row[..end].as_chunks_mut::<4>().0 {
            // Premultiplied color over black is the color itself.
            pixel[3] = u8::MAX;
        }
    }
    ImageFrame::new(frame.width(), frame.height(), frame.stride(), pixels).ok()
}

fn field_value(value: &TitleFieldValue) -> FieldValue {
    match value {
        TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
        TitleFieldValue::Number(number) => FieldValue::Number(*number),
        TitleFieldValue::Color(color) => {
            FieldValue::Color(Color::new(color.red, color.green, color.blue, color.alpha))
        }
        TitleFieldValue::Image { asset_uri } => FieldValue::Image(ImageSource::new(asset_uri)),
    }
}
