//! Simulated rendering of title inputs for the monitor tap.
//!
//! Title templates and their fonts are loaded once at daemon startup
//! ([`TitleAssets::load`]): one shared template and asset catalog per stored
//! template URI any title input names. A template that does not load, or a
//! font that does not read or parse, is left out: the input then renders
//! nothing (or draws without that font) instead of an invented picture.
//!
//! Image assets load on demand, at render time and therefore off the session
//! loop: every image URI a render needs (a template default or a field value,
//! including one an operator changed at runtime) is read from the project
//! store once, bounded to [`MAX_TITLE_IMAGE_ENCODED_BYTES`] encoded and
//! [`fm_titles::MAX_IMAGE_ASSET_BYTES`] decoded, decoded by `fm-codec-image`
//! (PNG or JPEG), premultiplied, and kept in the template's catalog. At most
//! [`fm_titles::MAX_IMAGE_ASSETS`] images are kept per template and at most
//! [`MAX_TITLE_IMAGE_ATTEMPTS`] distinct URIs are ever tried; an image that
//! does not resolve, read, or decode, or that misses those bounds, keeps the
//! reference renderer's placeholder. `fm-codec-image` is linked only by
//! `native-media` builds: a build without it decodes nothing, and every image
//! element draws the placeholder.
//!
//! A title renders by instantiating its template, applying the input's
//! persisted field values, rendering at the template size with the
//! `fm_titles` reference renderer, and scaling nearest-neighbor onto the
//! fitted monitor grid. The scene time of a title is the frame cursor times
//! the project frame duration, a documented approximation: the simulated
//! daemon has no title playback state, so every title is treated as having
//! started at frame zero.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
use std::sync::{Arc, Mutex, PoisonError};

use fm_model::{InputKind, Project, TitleFieldEntry, TitleFieldValue, TitleInput};
use fm_persistence::ProjectStore;
use fm_titles::{
    AssetCatalog, Color, ElementKind, FieldId, FieldValue, ImageSource, MAX_FONT_BYTES,
    ReferenceRenderer, TitleId, TitleTemplate,
};
use fm_types::InputId;
use fm_video::ImageFrame;

/// Largest encoded image file a title loads.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) const MAX_TITLE_IMAGE_ENCODED_BYTES: usize = 32 * 1024 * 1024;

/// Most distinct image URIs one template ever tries to load, successful or
/// not, so a stream of failing runtime URIs cannot keep reading the disk.
pub(crate) const MAX_TITLE_IMAGE_ATTEMPTS: usize = 4 * fm_titles::MAX_IMAGE_ASSETS;

/// One loaded template and the assets it renders with.
pub(crate) struct LoadedTitleTemplate {
    template: Arc<TitleTemplate>,
    /// Fonts and image assets; decoded images join on demand.
    images: Mutex<TitleImages>,
    /// Whether a rendered picture depends on scene time (clock, ticker, or
    /// animated elements), so the frame cursor belongs in its cache key.
    time_dependent: bool,
}

/// A template's catalog and every image URI it already tried to decode.
struct TitleImages {
    catalog: AssetCatalog,
    attempted: BTreeSet<String>,
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
            images: Mutex::new(TitleImages {
                catalog,
                attempted: BTreeSet::new(),
            }),
            time_dependent,
        }
    }

    pub(crate) const fn time_dependent(&self) -> bool {
        self.time_dependent
    }

    /// The catalog to render `title` with, after loading every image it
    /// needs that was not tried yet. Cloning shares fonts and pixels.
    fn catalog_for(&self, store: Option<&ProjectStore>, title: &TitleInput) -> AssetCatalog {
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = store {
            let defaults = self
                .template
                .fields
                .iter()
                .filter_map(|field| match &field.default {
                    FieldValue::Image(image) => Some(image.asset.as_str()),
                    _ => None,
                });
            let values = title.fields.iter().filter_map(|entry| match &entry.value {
                TitleFieldValue::Image { asset_uri } => Some(asset_uri.as_str()),
                _ => None,
            });
            for uri in defaults.chain(values) {
                images.load(store, uri);
            }
        }
        images.catalog.clone()
    }
}

impl TitleImages {
    /// Decodes `uri` into the catalog once, within the image bounds.
    fn load(&mut self, store: &ProjectStore, uri: &str) {
        if self.attempted.contains(uri) || self.attempted.len() >= MAX_TITLE_IMAGE_ATTEMPTS {
            return;
        }
        self.attempted.insert(uri.to_owned());
        let Some(frame) = decode_image_asset(store, uri) else {
            return;
        };
        // `with_decoded_image` consumes the catalog; a clone shares every
        // font and pixel buffer, so a refusal never loses loaded assets.
        if let Ok(catalog) = self.catalog.clone().with_decoded_image(uri, frame) {
            self.catalog = catalog;
        }
    }
}

/// Every title template the project's title inputs name, keyed by URI.
#[derive(Default)]
pub(crate) struct TitleAssets {
    templates: BTreeMap<String, LoadedTitleTemplate>,
    /// Where image assets load from on demand; `None` loads no image.
    store: Option<ProjectStore>,
}

impl TitleAssets {
    /// Loads each distinct template named by a title input of `project`,
    /// with its fonts and the names of every image asset its defaults and
    /// the inputs' persisted values reference.
    pub(crate) fn load(store: &ProjectStore, project: &Project) -> Self {
        let mut assets = Self {
            store: Some(store.clone()),
            ..Self::default()
        };
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
                let images = loaded
                    .images
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner);
                for entry in &title.fields {
                    if let TitleFieldValue::Image { asset_uri } = &entry.value {
                        images.catalog =
                            std::mem::take(&mut images.catalog).with_image(asset_uri.clone());
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

    /// Loads image assets on demand from `store`.
    #[cfg(all(test, feature = "native-media"))]
    pub(crate) fn with_store(mut self, store: ProjectStore) -> Self {
        self.store = Some(store);
        self
    }

    pub(crate) fn template(&self, uri: &str) -> Option<&LoadedTitleTemplate> {
        self.templates.get(uri)
    }
}

/// Reads and decodes one image asset into premultiplied RGBA8, or `None`
/// when it does not resolve, read, or decode within the title image bounds.
#[cfg(feature = "native-media")]
fn decode_image_asset(store: &ProjectStore, uri: &str) -> Option<ImageFrame> {
    use fm_codec_image::{StillDecodeLimits, decode_still};
    use fm_frame::{
        ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp,
        OriginalTimestamp, SequenceNumber, TimeBase,
    };

    let path = store.resolve_asset_uri(uri).ok()?;
    let limit = u64::try_from(MAX_TITLE_IMAGE_ENCODED_BYTES).ok()?;
    let mut encoded = Vec::new();
    File::open(path)
        .ok()?
        .take(limit.saturating_add(1))
        .read_to_end(&mut encoded)
        .ok()?;
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(0),
            TimeBase::new(1, 1_000_000_000).ok()?,
        ),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(1).ok()?,
        ClockDomainId::new(std::num::NonZeroU128::MIN),
        SequenceNumber::new(0),
    )
    .ok()?;
    let limits = StillDecodeLimits {
        max_encoded_bytes: MAX_TITLE_IMAGE_ENCODED_BYTES,
        max_width: fm_titles::MAX_OUTPUT_WIDTH,
        max_height: fm_titles::MAX_OUTPUT_HEIGHT,
        max_decoded_rgba_bytes: fm_titles::MAX_IMAGE_ASSET_BYTES,
        ..StillDecodeLimits::default()
    };
    let decoded = decode_still(&encoded, timing, limits).ok()?;
    let payload = decoded.frame.payload();
    let dimensions = payload.dimensions();
    let plane = payload.plane(0)?;
    let mut pixels = plane.bytes().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            // Straight to premultiplied, rounded to nearest.
            *channel = u8::try_from((u16::from(*channel) * alpha + 127) / 255).unwrap_or(u8::MAX);
        }
    }
    ImageFrame::new(
        dimensions.width(),
        dimensions.height(),
        plane.stride(),
        pixels,
    )
    .ok()
}

/// Builds without `fm-codec-image` decode no image: every image element
/// keeps the reference renderer's placeholder.
#[cfg(not(feature = "native-media"))]
fn decode_image_asset(_store: &ProjectStore, _uri: &str) -> Option<ImageFrame> {
    None
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
    render_title_at(
        assets,
        input,
        title,
        scene_time_ms(project.settings().frame_rate, frame),
        width,
        height,
    )
}

/// Renders `title` at `scene_time_ms` and scales it to `width`x`height`;
/// see [`render_title`].
pub(crate) fn render_title_at(
    assets: &TitleAssets,
    input: InputId,
    title: &TitleInput,
    scene_time_ms: u64,
    width: u32,
    height: u32,
) -> Option<ImageFrame> {
    let loaded = assets.template(&title.template_uri)?;
    let catalog = loaded.catalog_for(assets.store.as_ref(), title);
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
        .render(&scene, scene_time_ms, &catalog)
        .ok()?;
    if output.frame.width() == width && output.frame.height() == height {
        return Some(output.frame);
    }
    fm_video::scale_nearest(&output.frame, width, height).ok()
}

/// Scene time at `frame`: `frame * 1000 * denominator / numerator`
/// milliseconds of the project frame rate, saturating.
pub(crate) fn scene_time_ms(rate: fm_types::FrameRate, frame: u64) -> u64 {
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

pub(crate) fn field_value(value: &TitleFieldValue) -> FieldValue {
    match value {
        TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
        TitleFieldValue::Number(number) => FieldValue::Number(*number),
        TitleFieldValue::Color(color) => {
            FieldValue::Color(Color::new(color.red, color.green, color.blue, color.alpha))
        }
        TitleFieldValue::Image { asset_uri } => FieldValue::Image(ImageSource::new(asset_uri)),
    }
}

/// The persisted form of one live title field value.
pub(crate) fn model_field_value(value: &FieldValue) -> TitleFieldValue {
    match value {
        FieldValue::Text(text) => TitleFieldValue::Text(text.clone()),
        FieldValue::Number(number) => TitleFieldValue::Number(*number),
        FieldValue::Color(color) => TitleFieldValue::Color(fm_model::Rgba8 {
            red: color.r,
            green: color.g,
            blue: color.b,
            alpha: color.a,
        }),
        FieldValue::Image(image) => TitleFieldValue::Image {
            asset_uri: image.asset.clone(),
        },
    }
}

#[cfg(all(test, feature = "native-media"))]
mod tests {
    use std::num::NonZeroU128;

    use fm_model::{TitleFieldEntry, TitleFieldId};
    use fm_titles::{
        Bounds, Element, ElementId, FieldDefinition, ImageFilter, ImageFit, Style, TemplateId,
    };

    use super::*;

    const URI: &str = "asset://titles/logo.fmtitle";

    /// A 64x36 transparent template with one image element at (8, 8) sized
    /// 16x8 that stretches image field 1, whose default names an asset that
    /// does not exist.
    fn template() -> TitleTemplate {
        let field = FieldId::new(NonZeroU128::MIN);
        TitleTemplate {
            id: TemplateId::new(NonZeroU128::MIN),
            name: "Logo".into(),
            width: 64,
            height: 36,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field,
                name: "Logo".into(),
                default: FieldValue::Image(ImageSource::new("asset://images/missing.png")),
            }],
            elements: vec![Element {
                id: ElementId::new(NonZeroU128::MIN),
                name: "Logo".into(),
                bounds: Bounds {
                    x: 8,
                    y: 8,
                    width: 16,
                    height: 8,
                },
                z_index: 0,
                visible: true,
                style: Style::default(),
                color_field: None,
                kind: ElementKind::Image {
                    field,
                    fit: ImageFit::Stretch,
                    filter: ImageFilter::Nearest,
                },
                animations: Vec::new(),
            }],
        }
    }

    fn title(asset_uri: &str) -> TitleInput {
        TitleInput {
            template_uri: URI.into(),
            field_revision: 1,
            fields: vec![TitleFieldEntry {
                id: TitleFieldId::new(NonZeroU128::MIN),
                value: TitleFieldValue::Image {
                    asset_uri: asset_uri.into(),
                },
            }],
        }
    }

    fn pixel(frame: &ImageFrame, x: u32, y: u32) -> [u8; 4] {
        let pixel = frame.pixel(x, y).unwrap();
        [pixel.r, pixel.g, pixel.b, pixel.a]
    }

    #[test]
    fn a_runtime_image_uri_loads_from_the_store_on_demand_and_premultiplies() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let images = store.assets_root().join("images");
        std::fs::create_dir_all(&images).unwrap();
        image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 100, 50, 128]))
            .save(images.join("runtime.png"))
            .unwrap();
        std::fs::write(images.join("corrupt.png"), b"\x89PNG\r\n\x1a\nnot a png").unwrap();

        let mut assets = TitleAssets::default().with_store(store);
        assets.insert(
            URI,
            LoadedTitleTemplate::new(template(), AssetCatalog::new()),
        );
        let input = InputId::new(NonZeroU128::MIN);

        // A URI first named at runtime decodes, premultiplied, into the
        // element; outside it the template stays transparent.
        let rendered = render_title_at(
            &assets,
            input,
            &title("asset://images/runtime.png"),
            0,
            64,
            36,
        )
        .unwrap();
        for (x, y) in [(8, 8), (23, 15), (16, 12)] {
            assert_eq!(pixel(&rendered, x, y), [100, 50, 25, 128], "({x}, {y})");
        }
        for (x, y) in [(7, 8), (24, 8), (8, 16), (0, 0)] {
            assert_eq!(pixel(&rendered, x, y), [0; 4], "({x}, {y})");
        }

        // An image that does not decode, or does not exist, keeps the
        // reference renderer's placeholder rather than any decoded picture.
        for uri in ["asset://images/corrupt.png", "asset://images/absent.png"] {
            let placeholder = render_title_at(&assets, input, &title(uri), 0, 64, 36).unwrap();
            assert_ne!(pixel(&placeholder, 8, 8), [100, 50, 25, 128], "{uri}");
            assert_ne!(pixel(&placeholder, 8, 8), [0; 4], "{uri}");
        }
        let loaded = assets.template(URI).unwrap();
        let images = loaded.images.lock().unwrap();
        assert!(images.catalog.image("asset://images/runtime.png").is_some());
        assert!(images.catalog.image("asset://images/corrupt.png").is_none());
        assert_eq!(images.attempted.len(), 4);
    }
}
