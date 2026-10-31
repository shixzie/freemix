//! Title input rendering shared by `freemixd` (simulated monitor tap, exact
//! title previews, native live title sources) and `freemix-cli`
//! (`title-render-frame`), so both draw images and production clocks the
//! same way.
//!
//! Title templates and their fonts are loaded once ([`TitleAssets::load`]):
//! one shared template and asset catalog per stored template URI any title
//! input names. A template that does not load, or a font that does not read
//! or parse, is left out: the input then renders nothing (or draws without
//! that font) instead of an invented picture.
//!
//! Image assets load on demand, at render time (in `freemixd` therefore off
//! the session loop): every image URI a render needs (a template default or
//! a field value, including one an operator changed at runtime) is read from
//! the project store once, bounded to [`MAX_TITLE_IMAGE_ENCODED_BYTES`]
//! encoded and [`fm_titles::MAX_IMAGE_ASSET_BYTES`] decoded, decoded by
//! `fm-codec-image` (PNG or JPEG), premultiplied, and kept in the template's
//! catalog. At most [`fm_titles::MAX_IMAGE_ASSETS`] images are kept per
//! template: a new image past that evicts the least recently drawn one a
//! render does not need, so an operator may keep uploading new pictures.
//! An image that does not resolve, read, or decode, or that misses those
//! bounds, keeps the reference renderer's placeholder, and is not tried
//! again while it is among the last [`MAX_TITLE_IMAGE_ATTEMPTS`] failures. Decoding needs
//! the `images` feature (`freemixd` enables it with `native-media`,
//! `freemix-cli` always): a build without it decodes nothing, and every image
//! element draws the placeholder.
//!
//! A title renders by instantiating its template, applying the input's
//! persisted field values, rendering at the template size with the
//! `fm_titles` reference renderer, and scaling nearest-neighbor onto the
//! requested size. The scene time of a title (what its tickers and scene
//! clocks show) is the frames since its timeline started
//! ([`TitleTime::taken_frame`], the frame the engine took it to Program, an
//! overlay channel, or Preview, see [`fm_engine::EngineTitleShown`]) times
//! the project frame duration, so a ticker starts its crawl on the take
//! (GX-001). A title not shown rests at scene time zero.
//!
//! Keyframe animation is drawn in the pose [`TitleTime::pose`] names (in the
//! daemon, the pose the engine reports for the rendered frame,
//! [`fm_engine::EngineTitleAir`]: the In phase from a title take, the Hold
//! pose while on air, the Out phase from an off-air). The pose is part of
//! [`TitleKey`], so a title mid-phase renders anew on every frame it
//! advances.
//!
//! An accepted animation edit (GX-011) changes the engine's template; the
//! daemon hands the engine's templates over with
//! [`TitleAssets::sync_templates`], and the animation revision is part of
//! [`TitleKey`], so the next render draws the edited animation. An exact
//! preview ([`render_title_preview`]) renders one title with a given
//! template — the engine's, or one carrying a client's unsaved draft.
//!
//! Production clock elements read the clocks their template defines (see
//! [`fm_titles::TitleClock`]) through a registry built for each render from
//! the render's [`TitleTime`]: the monotonic clock is the rendered frame's
//! time on the project frame grid, the timeline the engine paces, so a
//! since-take stopwatch advances exactly one frame duration per frame from
//! the frame the title went on air; time-of-day and countdown-to clocks read
//! [`TitleTime::utc`] (the daemon samples the system wall clock at the
//! render, the CLI takes an explicit instant), resolved through the clock's
//! POSIX zone rules.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Read as _;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use fm_clock::ClockTime;
use fm_clock::production::{TimeReading, UtcInstant};
use fm_model::{InputKind, Project, TitleFieldEntry, TitleFieldValue, TitleInput};
use fm_persistence::ProjectStore;
use fm_titles::{
    AnimSample, AssetCatalog, ClockSource, Color, ElementKind, FieldId, FieldValue, ImageSource,
    MAX_FONT_BYTES, ReferenceRenderer, TitleClock, TitleClockKind, TitleClockTime, TitleId,
    TitleTemplate, title_clock_registry,
};
use fm_types::{FrameRate, InputId};
use fm_video::ImageFrame;

mod mapping;
pub use mapping::{
    TitleMappingError, apply_title_mapping, apply_title_mapping_to_fields,
};

/// Largest encoded image file a title loads.
pub const MAX_TITLE_IMAGE_ENCODED_BYTES: usize = 32 * 1024 * 1024;

/// Most failed image URIs one template remembers (least recently tried
/// forgotten first) so it does not read them from disk again on every
/// render.
pub const MAX_TITLE_IMAGE_ATTEMPTS: usize = 4 * fm_titles::MAX_IMAGE_ASSETS;

/// One loaded template and the assets it renders with.
pub struct LoadedTitleTemplate {
    /// The template as the engine holds it now: replaced, never mutated,
    /// when an animation edit is accepted.
    template: Mutex<Arc<TitleTemplate>>,
    /// Fonts and image assets; decoded images join on demand.
    images: Mutex<TitleImages>,
    /// Whether a rendered picture depends on scene time (a clock or ticker
    /// element), so the frame cursor belongs in its cache key. Keyframe
    /// animation does not count: its pose comes from the engine's title
    /// phases and is keyed on its own ([`TitleKey`]).
    time_dependent: bool,
    /// The production clocks the template defines for its elements.
    clocks: Vec<TitleClock>,
    /// Whether a clock counts from the title's take, so the take frame
    /// belongs in its cache key.
    uses_take: bool,
    /// Whether a scene clock or ticker counts scene time from the title's
    /// take, so the take frame belongs in its cache key too.
    scene_timed: bool,
}

/// A template's catalog, when each of its images was last needed, and the
/// image URIs that recently failed to decode.
struct TitleImages {
    catalog: AssetCatalog,
    /// The render tick each catalog image was last needed at (a name never
    /// needed yet counts as tick zero, so it goes first).
    used: BTreeMap<String, u64>,
    tick: u64,
    /// Failed URIs, least recently tried first, at most
    /// [`MAX_TITLE_IMAGE_ATTEMPTS`].
    failed: VecDeque<String>,
}

impl LoadedTitleTemplate {
    #[must_use]
    pub fn new(template: TitleTemplate, catalog: AssetCatalog) -> Self {
        let time_dependent = template.elements.iter().any(|element| {
            matches!(
                element.kind,
                ElementKind::Clock(_) | ElementKind::Ticker(_) | ElementKind::ProductionClock(_)
            )
        });
        let scene_timed = template
            .elements
            .iter()
            .any(|element| matches!(element.kind, ElementKind::Clock(_) | ElementKind::Ticker(_)));
        Self {
            template: Mutex::new(Arc::new(template)),
            images: Mutex::new(TitleImages {
                catalog,
                used: BTreeMap::new(),
                tick: 0,
                failed: VecDeque::new(),
            }),
            time_dependent,
            clocks: Vec::new(),
            uses_take: false,
            scene_timed,
        }
    }

    /// The template with the production clocks its stored document defines.
    #[must_use]
    pub fn with_clocks(mut self, clocks: Vec<TitleClock>) -> Self {
        self.uses_take = clocks
            .iter()
            .any(|clock| matches!(clock.kind, TitleClockKind::SinceTake { .. }));
        self.clocks = clocks;
        self
    }

    pub const fn time_dependent(&self) -> bool {
        self.time_dependent
    }

    /// The template as the engine holds it now.
    pub fn current(&self) -> Arc<TitleTemplate> {
        Arc::clone(&self.template.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The catalog to render `title` with, after loading every image it
    /// needs that was not tried yet. Cloning shares fonts and pixels.
    fn catalog_for(&self, store: Option<&ProjectStore>, title: &TitleInput) -> AssetCatalog {
        let template = self.current();
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = store {
            let defaults = template
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
            let needed: BTreeSet<&str> = defaults.chain(values).collect();
            images.tick += 1;
            let tick = images.tick;
            for uri in &needed {
                images.used.insert((*uri).to_owned(), tick);
            }
            for uri in &needed {
                images.load(store, uri, &needed);
            }
            // Only the images the catalog holds need a last use.
            let TitleImages { catalog, used, .. } = &mut *images;
            used.retain(|name, _| catalog.has_image(name));
        }
        images.catalog.clone()
    }
}

impl TitleImages {
    /// Decodes `uri` into the catalog unless it is there or recently
    /// failed, within the image bounds, evicting the least recently needed
    /// image this render does not need (`needed`) when the catalog is full.
    fn load(&mut self, store: &ProjectStore, uri: &str, needed: &BTreeSet<&str>) {
        if self.catalog.image(uri).is_some() || self.failed.iter().any(|failed| failed == uri) {
            return;
        }
        let Some(frame) = decode_image_asset(store, uri) else {
            if self.failed.len() >= MAX_TITLE_IMAGE_ATTEMPTS {
                self.failed.pop_front();
            }
            self.failed.push_back(uri.to_owned());
            return;
        };
        if !self.catalog.has_image(uri) && self.catalog.image_count() >= fm_titles::MAX_IMAGE_ASSETS
        {
            let evicted = self
                .catalog
                .image_names()
                .filter(|name| !needed.contains(name))
                .min_by_key(|name| self.used.get(*name).copied().unwrap_or(0))
                .map(str::to_owned);
            if let Some(evicted) = evicted {
                self.catalog = std::mem::take(&mut self.catalog).without_image(&evicted);
                self.used.remove(&evicted);
            }
        }
        // `with_decoded_image` consumes the catalog; a clone shares every
        // font and pixel buffer, so a refusal never loses loaded assets.
        if let Ok(catalog) = self.catalog.clone().with_decoded_image(uri, frame) {
            self.catalog = catalog;
        }
    }
}

/// Every title template the project's title inputs name, keyed by URI.
#[derive(Default)]
pub struct TitleAssets {
    templates: RwLock<BTreeMap<String, Arc<LoadedTitleTemplate>>>,
    /// Where image assets load from on demand; `None` loads no image.
    store: Option<ProjectStore>,
}

/// Loads the stored template `uri` with its fonts and image defaults.
fn load_template(store: &ProjectStore, uri: &str) -> Option<LoadedTitleTemplate> {
    let stored = store.load_title_template(uri).ok()?;
    let mut catalog = AssetCatalog::new();
    for font in &stored.fonts {
        let Some(bytes) = read_font(store, &font.asset_uri) else {
            continue;
        };
        // `with_font_bytes` consumes the catalog; parse first so an unusable
        // font never loses the fonts already loaded.
        if let Ok(face) = fm_titles::FontFace::from_bytes(bytes) {
            catalog = catalog.with_font(font.family.clone(), face);
        }
    }
    for field in &stored.template.fields {
        if let FieldValue::Image(image) = &field.default {
            catalog = catalog.with_image(image.asset.clone());
        }
    }
    Some(LoadedTitleTemplate::new(stored.template, catalog).with_clocks(stored.clocks))
}

impl TitleAssets {
    /// The project store image assets load from, if any.
    #[must_use]
    pub const fn store(&self) -> Option<&ProjectStore> {
        self.store.as_ref()
    }

    /// Loads each distinct template named by a title input of `project`,
    /// with its fonts and the names of every image asset its defaults and
    /// the inputs' persisted values reference.
    pub fn load(store: &ProjectStore, project: &Project) -> Self {
        let mut templates: BTreeMap<String, LoadedTitleTemplate> = BTreeMap::new();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            if !templates.contains_key(&title.template_uri) {
                let Some(loaded) = load_template(store, &title.template_uri) else {
                    continue;
                };
                templates.insert(title.template_uri.clone(), loaded);
            }
            if let Some(loaded) = templates.get_mut(&title.template_uri) {
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
        Self {
            templates: RwLock::new(
                templates
                    .into_iter()
                    .map(|(uri, template)| (uri, Arc::new(template)))
                    .collect(),
            ),
            store: Some(store.clone()),
        }
    }

    /// Registers an already loaded template under `uri`.
    pub fn insert(&mut self, uri: impl Into<String>, template: LoadedTitleTemplate) {
        self.templates
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uri.into(), Arc::new(template));
    }

    /// Loads image assets on demand from `store`.
    #[must_use]
    pub fn with_store(mut self, store: ProjectStore) -> Self {
        self.store = Some(store);
        self
    }

    #[must_use]
    pub fn template(&self, uri: &str) -> Option<Arc<LoadedTitleTemplate>> {
        self.templates
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(uri)
            .cloned()
    }

    /// Every image field of `title` (its value, or the template default)
    /// whose image has no decoded pixels yet, as `(field, asset URI)` in
    /// template field order: what a render draws as the placeholder. Empty
    /// when the title's template is not loaded.
    #[must_use]
    pub fn placeholder_images(&self, title: &TitleInput) -> Vec<(FieldId, String)> {
        let Some(loaded) = self.template(&title.template_uri) else {
            return Vec::new();
        };
        let template = loaded.current();
        let images = loaded.images.lock().unwrap_or_else(PoisonError::into_inner);
        template
            .fields
            .iter()
            .filter_map(|field| {
                let FieldValue::Image(default) = &field.default else {
                    return None;
                };
                let value = title
                    .fields
                    .iter()
                    .find(|entry| FieldId::new(entry.id.get()) == field.id)
                    .and_then(|entry| match &entry.value {
                        TitleFieldValue::Image { asset_uri } => Some(asset_uri.as_str()),
                        _ => None,
                    })
                    .unwrap_or(default.asset.as_str());
                images
                    .catalog
                    .image(value)
                    .is_none()
                    .then(|| (field.id, value.to_owned()))
            })
            .collect()
    }

    /// Takes the engine's current templates (keyed by stored template URI)
    /// and returns the URIs whose animation changed or whose template was
    /// loaded for the first time (a title input added while the daemon
    /// runs). Cheap when nothing changed: the engine shares each template
    /// until an edit replaces it.
    pub fn sync_templates(&self, templates: &[fm_engine::EngineTitleTemplate]) -> Vec<String> {
        let mut changed = Vec::new();
        let mut loaded = self
            .templates
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        for engine in templates {
            let uri = &*engine.key;
            let mut newly_loaded = false;
            if !loaded.contains_key(uri) {
                let Some(store) = self.store.as_ref() else {
                    continue;
                };
                let Some(template) = load_template(store, uri) else {
                    continue;
                };
                loaded.insert(uri.to_owned(), Arc::new(template));
                newly_loaded = true;
            }
            let Some(entry) = loaded.get(uri) else {
                continue;
            };
            let mut current = entry
                .template
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let animation_changed = current.animation != engine.template.animation;
            if Arc::ptr_eq(&current, &engine.template) && !newly_loaded {
                continue;
            }
            *current = Arc::clone(&engine.template);
            if newly_loaded || animation_changed {
                changed.push(uri.to_owned());
            }
        }
        changed
    }
}

/// Reads and decodes one image asset into premultiplied RGBA8, or `None`
/// when it does not resolve, read, or decode within the title image bounds.
#[cfg(feature = "images")]
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
#[cfg(not(feature = "images"))]
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
#[must_use]
pub fn title_of_input(project: &Project, input: InputId) -> Option<&TitleInput> {
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
/// only for a time-dependent template, the take frame only for a template
/// with a since-take clock, a scene clock, or a ticker; the animation pose
/// always.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleKey {
    template_uri: String,
    field_revision: u64,
    fields: Vec<TitleFieldEntry>,
    frame: Option<u64>,
    taken_frame: Option<u64>,
    pose: AnimSample,
    /// The template's animation revision: an accepted edit renders anew.
    animation_revision: u64,
}

/// The cache key of `title` rendered in `pose` at `frame` after going on
/// air at `taken_frame`, or `None` when its template was not loaded.
#[must_use]
pub fn title_key(
    assets: &TitleAssets,
    title: &TitleInput,
    frame: u64,
    taken_frame: Option<u64>,
    pose: AnimSample,
) -> Option<TitleKey> {
    let loaded = assets.template(&title.template_uri)?;
    Some(TitleKey {
        template_uri: title.template_uri.clone(),
        field_revision: title.field_revision,
        fields: title.fields.clone(),
        frame: loaded.time_dependent().then_some(frame),
        taken_frame: taken_frame.filter(|_| loaded.uses_take || loaded.scene_timed),
        pose,
        animation_revision: loaded.current().animation.revision,
    })
}

/// When a title is rendered, in the daemon's clock domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleTime {
    /// The engine frame shown. Scene time, and the monotonic clock of
    /// production clocks, are this frame's start on the project frame grid.
    pub frame: u64,
    pub rate: FrameRate,
    /// Wall-clock UTC for time-of-day and countdown-to clocks.
    pub utc: UtcInstant,
    /// The frame the title's timeline started on while it is shown: taken
    /// to Program, an overlay channel, or Preview (GX-001). Scene time and
    /// since-take clocks count from it; `None` rests them at zero.
    pub taken_frame: Option<u64>,
    /// The keyframe animation pose the engine reports for `frame`.
    pub pose: AnimSample,
}

impl TitleTime {
    /// `frame` of a title on air since `taken_frame` in `pose`, read
    /// against the system wall clock now.
    #[must_use]
    pub fn now(frame: u64, rate: FrameRate, taken_frame: Option<u64>, pose: AnimSample) -> Self {
        Self {
            frame,
            rate,
            utc: utc_now(),
            taken_frame,
            pose,
        }
    }

    /// Scene time: the frames since the title's timeline started, or zero
    /// while it is not shown.
    fn scene_time_ms(self) -> u64 {
        self.taken_frame.map_or(0, |taken| {
            scene_time_ms(self.rate, self.frame.saturating_sub(taken))
        })
    }

    /// The production clock reading of this render.
    fn clock_time(self) -> TitleClockTime {
        TitleClockTime {
            at: TimeReading::new(frame_clock_time(self.rate, self.frame), self.utc),
            taken_at: self
                .taken_frame
                .map(|frame| frame_clock_time(self.rate, frame)),
        }
    }
}

/// The start of `frame` on the frame grid of `rate`, in nanoseconds,
/// saturating.
fn frame_clock_time(rate: FrameRate, frame: u64) -> ClockTime {
    let numerator = u128::from(rate.numerator()).max(1);
    let nanos = u128::from(frame) * 1_000_000_000 * u128::from(rate.denominator()) / numerator;
    ClockTime::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// The system wall clock as a UTC instant; the Unix epoch if the clock is
/// set before it or beyond year 9999.
#[must_use]
pub fn utc_now() -> UtcInstant {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| {
            UtcInstant::new(i64::try_from(since.as_secs()).ok()?, since.subsec_nanos()).ok()
        })
        .unwrap_or(UtcInstant::UNIX_EPOCH)
}

/// Renders `title` at `time` and scales it to `width`x`height`.
///
/// The result is premultiplied RGBA8 that keeps the template's alpha, ready
/// to be layered as an overlay or flattened over black. Returns `None` when
/// the template was not loaded or does not instantiate, a persisted field
/// does not match the template, or rendering fails. Production clock
/// elements read the template's clocks at `time`. Keyframe animation is drawn
/// in `time.pose`.
#[must_use]
pub fn render_title_at(
    assets: &TitleAssets,
    input: InputId,
    title: &TitleInput,
    time: TitleTime,
    width: u32,
    height: u32,
) -> Option<ImageFrame> {
    let loaded = assets.template(&title.template_uri)?;
    render_template_at(
        assets,
        &loaded,
        &loaded.current(),
        input,
        title,
        time,
        width,
        height,
    )
}

/// Renders `title` with `template` (the loaded template's, or a copy with
/// another animation) at `time`, scaled to `width`x`height`; see
/// [`render_title_at`].
#[allow(clippy::too_many_arguments)]
fn render_template_at(
    assets: &TitleAssets,
    loaded: &LoadedTitleTemplate,
    template: &TitleTemplate,
    input: InputId,
    title: &TitleInput,
    time: TitleTime,
    width: u32,
    height: u32,
) -> Option<ImageFrame> {
    let output = render_template_output(assets, loaded, template, input, title, time).ok()?;
    if output.frame.width() == width && output.frame.height() == height {
        return Some(output.frame);
    }
    fm_video::scale_nearest(&output.frame, width, height).ok()
}

/// Why a title did not render ([`render_title_output`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleRenderError {
    /// No template is loaded for the title's template URI.
    TemplateNotLoaded(String),
    /// The template does not instantiate.
    Instantiate(fm_titles::InstantiationError),
    /// A persisted field value does not match the template.
    Fields(fm_titles::UpdateError),
    /// The template's production clocks do not form a registry.
    Clocks(fm_clock::production::ProductionClockError),
    /// The reference renderer failed.
    Render(fm_titles::RenderError),
}

impl std::fmt::Display for TitleRenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TemplateNotLoaded(uri) => write!(formatter, "title template {uri} is not loaded"),
            Self::Instantiate(error) => {
                write!(formatter, "title template does not instantiate: {error}")
            }
            Self::Fields(error) => {
                write!(formatter, "title fields do not match the template: {error}")
            }
            Self::Clocks(error) => write!(formatter, "title production clocks: {error}"),
            Self::Render(error) => write!(formatter, "title render failed: {error}"),
        }
    }
}

impl std::error::Error for TitleRenderError {}

/// Renders `title` at `time` at its template's size, with the render's
/// report (missing fonts and images, degraded elements): exactly the
/// picture [`render_title_at`] scales. Production clock elements read the
/// template's clocks at `time`; keyframe animation is drawn in `time.pose`.
///
/// # Errors
///
/// Returns why the title did not render: its template is not loaded or
/// does not instantiate, a persisted field does not match the template, its
/// clocks do not form a registry, or the reference renderer fails.
pub fn render_title_output(
    assets: &TitleAssets,
    input: InputId,
    title: &TitleInput,
    time: TitleTime,
) -> Result<fm_titles::RenderOutput, TitleRenderError> {
    let loaded = assets
        .template(&title.template_uri)
        .ok_or_else(|| TitleRenderError::TemplateNotLoaded(title.template_uri.clone()))?;
    render_template_output(assets, &loaded, &loaded.current(), input, title, time)
}

/// [`render_title_output`] with `template` (the loaded template's, or a
/// copy with another animation).
fn render_template_output(
    assets: &TitleAssets,
    loaded: &LoadedTitleTemplate,
    template: &TitleTemplate,
    input: InputId,
    title: &TitleInput,
    time: TitleTime,
) -> Result<fm_titles::RenderOutput, TitleRenderError> {
    let catalog = loaded.catalog_for(assets.store.as_ref(), title);
    let mut scene = template
        .instantiate(TitleId::new(input.get()))
        .map_err(TitleRenderError::Instantiate)?;
    let updates = title
        .fields
        .iter()
        .map(|entry| (FieldId::new(entry.id.get()), field_value(&entry.value)))
        .collect::<Vec<_>>();
    scene
        .update_fields(scene.revision(), &updates)
        .map_err(TitleRenderError::Fields)?;
    if loaded.clocks.is_empty() {
        ReferenceRenderer.render_sampled(&scene, time.scene_time_ms(), &catalog, time.pose)
    } else {
        let clock_time = time.clock_time();
        let registry =
            title_clock_registry(&loaded.clocks, clock_time).map_err(TitleRenderError::Clocks)?;
        ReferenceRenderer.render_sampled_with_clocks(
            &scene,
            time.scene_time_ms(),
            &catalog,
            time.pose,
            ClockSource::new(&registry, clock_time.at),
        )
    }
    .map_err(TitleRenderError::Render)
}

/// Renders an exact preview of `title` exactly as [`render_title_at`]
/// would draw it on air, but with `template` (the engine's template, or one
/// carrying an unsaved draft animation), as straight (not premultiplied)
/// RGBA8 pixels, row-major and tightly packed, at `width`x`height`.
///
/// Returns `None` when the title's template was not loaded or the render
/// fails.
#[must_use]
pub fn render_title_preview(
    assets: &TitleAssets,
    template: &TitleTemplate,
    input: InputId,
    title: &TitleInput,
    time: TitleTime,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let loaded = assets.template(&title.template_uri)?;
    let frame = render_template_at(assets, &loaded, template, input, title, time, width, height)?;
    let row_bytes = usize::try_from(frame.width()).ok()?.checked_mul(4)?;
    let mut pixels =
        Vec::with_capacity(row_bytes.checked_mul(usize::try_from(frame.height()).ok()?)?);
    for row in frame.pixels().chunks(frame.stride()) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            pixels.extend_from_slice(&unpremultiply(*pixel));
        }
    }
    Some(pixels)
}

/// Premultiplied to straight alpha, rounded to nearest; a fully transparent
/// pixel is transparent black.
#[must_use]
pub fn unpremultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    match alpha {
        0 => [0; 4],
        u8::MAX => [red, green, blue, alpha],
        _ => {
            let alpha_wide = u16::from(alpha);
            let channel = |value: u8| {
                let straight = (u16::from(value) * 255 + alpha_wide / 2) / alpha_wide;
                u8::try_from(straight).unwrap_or(u8::MAX)
            };
            [channel(red), channel(green), channel(blue), alpha]
        }
    }
}

/// Scene time at `frame`: `frame * 1000 * denominator / numerator`
/// milliseconds of the project frame rate, saturating.
#[must_use]
pub fn scene_time_ms(rate: fm_types::FrameRate, frame: u64) -> u64 {
    let numerator = u128::from(rate.numerator()).max(1);
    let millis = u128::from(frame) * 1_000 * u128::from(rate.denominator()) / numerator;
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Composites a premultiplied picture over opaque black.
#[must_use]
pub fn flatten_over_black(frame: &ImageFrame) -> Option<ImageFrame> {
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

#[must_use]
pub fn field_value(value: &TitleFieldValue) -> FieldValue {
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
#[must_use]
pub fn model_field_value(value: &FieldValue) -> TitleFieldValue {
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

#[cfg(all(test, feature = "images"))]
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
                exposed: true,
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
            }],
            animation: fm_titles::TitleAnimation::default(),
        }
    }

    fn title(asset_uri: &str) -> TitleInput {
        TitleInput {
            template_uri: URI.into(),
            field_revision: 1,
            fields: vec![TitleFieldEntry {
                id: TitleFieldId::new(NonZeroU128::MIN),
                revision: 0,
                value: TitleFieldValue::Image {
                    asset_uri: asset_uri.into(),
                },
            }],
        }
    }

    fn frame_zero() -> TitleTime {
        TitleTime {
            frame: 0,
            rate: fm_types::FrameRate::new(25, 1).unwrap(),
            utc: UtcInstant::UNIX_EPOCH,
            taken_frame: None,
            pose: AnimSample::Hold,
        }
    }

    fn pixel(frame: &ImageFrame, x: u32, y: u32) -> [u8; 4] {
        let pixel = frame.pixel(x, y).unwrap();
        [pixel.r, pixel.g, pixel.b, pixel.a]
    }

    #[test]
    fn animated_titles_render_the_engine_pose_and_are_not_time_dependent() {
        use fm_titles::fm_animation::{
            ClipPosition, ClipState, Easing, Keyframe, Phase, Property, Track, TrackKey, Value,
        };

        let mut animated = template();
        animated.elements[0].kind = ElementKind::Rectangle;
        let element = animated.elements[0].id;
        let track = |phase, value| {
            Track::new(
                TrackKey::new(element, phase, Property::PositionX),
                vec![Keyframe::new(0, Value::Scalar(value), Easing::Linear)],
            )
        };
        animated
            .edit_animation(|clip| {
                // Hold shows the In phase's end (+4); the Out pose (+40)
                // only while the engine reports the Out phase.
                clip.set_track(track(Phase::In, 4))?;
                clip.set_track(track(Phase::Out, 40))
            })
            .unwrap();
        let loaded = LoadedTitleTemplate::new(animated, AssetCatalog::new());
        assert!(!loaded.time_dependent());
        let mut assets = TitleAssets::default();
        assets.insert(URI, loaded);
        let input = InputId::new(NonZeroU128::MIN);
        for frame in [0, 1_000] {
            let rendered = render_title_at(
                &assets,
                input,
                &title("asset://images/absent.png"),
                TitleTime {
                    frame,
                    ..frame_zero()
                },
                64,
                36,
            )
            .unwrap();
            assert_eq!(pixel(&rendered, 11, 8), [0; 4]);
            for (x, y) in [(12, 8), (27, 15)] {
                assert_eq!(pixel(&rendered, x, y), [255; 4], "({x}, {y})");
            }
            assert_eq!(pixel(&rendered, 28, 8), [0; 4]);
        }
        // The Out pose moves the bar 40 pixels right instead of 4, and the
        // pose is part of the cache key.
        let out = ClipState::Out(ClipPosition::frame(0));
        let rendered = render_title_at(
            &assets,
            input,
            &title("asset://images/absent.png"),
            TitleTime {
                pose: out,
                ..frame_zero()
            },
            64,
            36,
        )
        .unwrap();
        for (x, y) in [(12, 8), (47, 8)] {
            assert_eq!(pixel(&rendered, x, y), [0; 4], "({x}, {y})");
        }
        for (x, y) in [(48, 8), (63, 15)] {
            assert_eq!(pixel(&rendered, x, y), [255; 4], "({x}, {y})");
        }
        let absent = title("asset://images/absent.png");
        assert_ne!(
            title_key(&assets, &absent, 0, None, AnimSample::Hold),
            title_key(&assets, &absent, 0, None, out)
        );
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
            frame_zero(),
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
            let placeholder =
                render_title_at(&assets, input, &title(uri), frame_zero(), 64, 36).unwrap();
            assert_ne!(pixel(&placeholder, 8, 8), [100, 50, 25, 128], "{uri}");
            assert_ne!(pixel(&placeholder, 8, 8), [0; 4], "{uri}");
        }
        let loaded = assets.template(URI).unwrap();
        let images = loaded.images.lock().unwrap();
        assert!(images.catalog.image("asset://images/runtime.png").is_some());
        assert!(images.catalog.image("asset://images/corrupt.png").is_none());
        assert!(
            images
                .failed
                .iter()
                .any(|uri| uri == "asset://images/corrupt.png")
        );
        assert!(
            !images
                .failed
                .iter()
                .any(|uri| uri == "asset://images/runtime.png")
        );
    }

    /// An operator may keep uploading pictures: past the catalog's
    /// [`fm_titles::MAX_IMAGE_ASSETS`] images each new one evicts the least
    /// recently drawn, so every picture draws (and an evicted one reloads).
    #[test]
    fn new_images_keep_drawing_past_the_catalog_bound() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let images = store.assets_root().join("images");
        std::fs::create_dir_all(&images).unwrap();
        let count = u8::try_from(fm_titles::MAX_IMAGE_ASSETS + 6).unwrap();
        for index in 0..count {
            image::RgbaImage::from_pixel(4, 2, image::Rgba([index, 255 - index, 7, 255]))
                .save(images.join(format!("picture-{index}.png")))
                .unwrap();
        }
        let mut assets = TitleAssets::default().with_store(store);
        assets.insert(
            URI,
            LoadedTitleTemplate::new(template(), AssetCatalog::new()),
        );
        let input = InputId::new(NonZeroU128::MIN);
        let draw = |index: u8| {
            let uri = format!("asset://images/picture-{index}.png");
            let rendered =
                render_title_at(&assets, input, &title(&uri), frame_zero(), 64, 36).unwrap();
            pixel(&rendered, 8, 8)
        };
        for index in 0..count {
            assert_eq!(draw(index), [index, 255 - index, 7, 255], "picture {index}");
        }
        // The first pictures were evicted; drawn again, they reload.
        assert_eq!(draw(0), [0, 255, 7, 255]);
        let loaded = assets.template(URI).unwrap();
        let held = loaded.images.lock().unwrap().catalog.image_count();
        assert!(held <= fm_titles::MAX_IMAGE_ASSETS, "{held}");
    }

    /// A long show may name any number of image URIs that never load (a
    /// data feed's missing pictures): the cache keeps bookkeeping only for
    /// the images it holds and the bounded failure list.
    #[test]
    fn missing_images_leave_no_bookkeeping_behind() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let mut assets = TitleAssets::default().with_store(store);
        assets.insert(
            URI,
            LoadedTitleTemplate::new(template(), AssetCatalog::new()),
        );
        let input = InputId::new(NonZeroU128::MIN);
        for index in 0..500 {
            let uri = format!("asset://images/missing-{index}.png");
            render_title_at(&assets, input, &title(&uri), frame_zero(), 64, 36).unwrap();
        }
        let loaded = assets.template(URI).unwrap();
        let images = loaded.images.lock().unwrap();
        assert!(images.failed.len() <= MAX_TITLE_IMAGE_ATTEMPTS);
        assert!(
            images.used.len() <= fm_titles::MAX_IMAGE_ASSETS,
            "{}",
            images.used.len()
        );
    }
}

#[cfg(test)]
mod clock_tests {
    //! Production clocks on title inputs, read in the daemon's clock domain.
    //!
    //! Exact-text tests render the clock title and, as the oracle, a plain
    //! text title holding the expected string with the same bounds and style.
    //! They need a real font: `FM_TITLES_FONT`, then well-known system paths,
    //! then a scan of the system font roots; they skip when none is found
    //! unless `FM_REQUIRE_FONT=1`. Every other test is font-free.

    use std::num::NonZeroU128;
    use std::path::{Path, PathBuf};

    use fm_clock::production::{DurationFormat, OverrunPolicy, TimeOfDayFormat, TimeZone};
    use fm_model::ProjectSettings;
    use fm_titles::{
        Bounds, Element, ElementId, FieldDefinition, FontStyle, ProductionClockSpec, Style,
        TemplateId,
    };
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };

    use super::*;

    const FAMILY: &str = "Clock face";
    const WIDTH: u32 = 240;
    const HEIGHT: u32 = 48;
    const BACKGROUND: Color = Color::new(0, 0, 96, 255);

    fn clock_name(text: &str) -> fm_clock::production::ClockName {
        fm_clock::production::ClockName::new(text).unwrap()
    }

    fn utc(hour: u8, minute: u8, second: u8) -> UtcInstant {
        UtcInstant::from_utc_civil(2026, 10, 14, hour, minute, second, 0).unwrap()
    }

    fn rate() -> FrameRate {
        FrameRate::new(25, 1).unwrap()
    }

    fn input() -> InputId {
        InputId::new(NonZeroU128::MIN)
    }

    /// Doors at 23:00 UTC (overrunning), US Eastern time of day, and a
    /// since-take stopwatch.
    fn clocks() -> Vec<TitleClock> {
        vec![
            TitleClock {
                name: clock_name("doors"),
                kind: TitleClockKind::CountdownTo {
                    target: utc(23, 0, 0),
                    overrun: OverrunPolicy::Continue,
                    format: DurationFormat::MinutesSeconds,
                },
            },
            TitleClock {
                name: clock_name("eastern"),
                kind: TitleClockKind::TimeOfDay {
                    zone: TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
                    format: TimeOfDayFormat::Hours24,
                },
            },
            TitleClock {
                name: clock_name("on-air"),
                kind: TitleClockKind::SinceTake {
                    format: DurationFormat::MinutesSeconds,
                },
            },
        ]
    }

    fn element(kind: ElementKind) -> Element {
        Element {
            id: ElementId::new(NonZeroU128::MIN),
            name: "clock".into(),
            bounds: Bounds {
                x: 8,
                y: 4,
                width: WIDTH - 16,
                height: HEIGHT - 8,
            },
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::new(255, 255, 255, 255),
                background: Some(BACKGROUND),
                opacity: 255,
                font: Some(FontStyle {
                    family: FAMILY.into(),
                    size_px: 28,
                }),
                alignment: fm_titles::Alignment::default(),
            },
            color_field: None,
            kind,
        }
    }

    fn template(element: Element, fields: Vec<FieldDefinition>) -> TitleTemplate {
        TitleTemplate {
            id: TemplateId::new(NonZeroU128::MIN),
            name: "Clock".into(),
            width: WIDTH,
            height: HEIGHT,
            background: Color::TRANSPARENT,
            fields,
            elements: vec![element],
            animation: fm_titles::TitleAnimation::default(),
        }
    }

    /// Assets with one template per clock, at `asset://titles/<clock>`.
    fn assets(catalog: &AssetCatalog) -> TitleAssets {
        let mut assets = TitleAssets::default();
        for clock in clocks() {
            let shown = clock.name.clone();
            let element = element(ElementKind::ProductionClock(ProductionClockSpec::new(
                shown.clone(),
            )));
            let own = clocks()
                .into_iter()
                .filter(|candidate| candidate.name == shown)
                .collect();
            assets.insert(
                format!("asset://titles/{shown}"),
                LoadedTitleTemplate::new(template(element, Vec::new()), catalog.clone())
                    .with_clocks(own),
            );
        }
        assets
    }

    fn title(clock: &str) -> TitleInput {
        TitleInput {
            template_uri: format!("asset://titles/{clock}"),
            field_revision: 0,
            fields: Vec::new(),
        }
    }

    fn at(frame: u64, utc: UtcInstant, taken_frame: Option<u64>) -> TitleTime {
        TitleTime {
            frame,
            rate: rate(),
            utc,
            taken_frame,
            pose: AnimSample::Hold,
        }
    }

    #[test]
    fn title_time_maps_frames_onto_the_monotonic_frame_grid() {
        let time = at(50, utc(12, 0, 0), Some(25));
        let clock = time.clock_time();
        assert_eq!(clock.at.monotonic, ClockTime::from_nanos(2_000_000_000));
        assert_eq!(clock.at.utc, utc(12, 0, 0));
        assert_eq!(clock.taken_at, Some(ClockTime::from_nanos(1_000_000_000)));
        // Scene time counts from the take (GX-001): 25 frames at 25/1.
        assert_eq!(time.scene_time_ms(), 1_000);
        // A title not shown rests at scene time zero.
        assert_eq!(at(50, utc(12, 0, 0), None).scene_time_ms(), 0);
        let ntsc = TitleTime {
            rate: FrameRate::new(30_000, 1_001).unwrap(),
            ..at(30, utc(12, 0, 0), None)
        };
        assert_eq!(
            ntsc.clock_time().at.monotonic,
            ClockTime::from_nanos(1_001_000_000)
        );
        assert_eq!(ntsc.clock_time().taken_at, None);
        // The system wall clock is after this code was written.
        assert!(utc_now() > utc(0, 0, 0));
    }

    #[test]
    fn only_since_take_templates_key_their_renders_by_the_take() {
        let assets = assets(&AssetCatalog::new());
        let since = title("on-air");
        let doors = title("doors");
        let key = |title: &TitleInput, taken| {
            title_key(&assets, title, 10, taken, AnimSample::Hold).unwrap()
        };
        assert_ne!(key(&since, None), key(&since, Some(3)));
        assert_ne!(key(&since, Some(3)), key(&since, Some(4)));
        assert_eq!(key(&doors, None), key(&doors, Some(3)));
        // Every production clock template is time-dependent.
        assert_ne!(
            title_key(&assets, &doors, 10, None, AnimSample::Hold),
            title_key(&assets, &doors, 11, None, AnimSample::Hold)
        );
    }

    #[test]
    fn clock_titles_render_their_background_without_a_font() {
        // Without the font every clock element still draws its background
        // box and nothing outside it; the picture does not depend on time.
        let assets = assets(&AssetCatalog::new());
        for clock in ["doors", "eastern", "on-air"] {
            let frame = render_title_at(
                &assets,
                input(),
                &title(clock),
                at(100, utc(22, 59, 30), Some(25)),
                WIDTH,
                HEIGHT,
            )
            .unwrap();
            let pixel = |x, y| {
                let pixel = frame.pixel(x, y).unwrap();
                [pixel.r, pixel.g, pixel.b, pixel.a]
            };
            assert_eq!(pixel(8, 4), [0, 0, 96, 255], "{clock}");
            assert_eq!(pixel(WIDTH - 9, HEIGHT - 5), [0, 0, 96, 255], "{clock}");
            assert_eq!(pixel(7, 4), [0; 4], "{clock}");
            assert_eq!(pixel(WIDTH - 8, HEIGHT - 4), [0; 4], "{clock}");
            let later = render_title_at(
                &assets,
                input(),
                &title(clock),
                at(900, utc(23, 30, 0), None),
                WIDTH,
                HEIGHT,
            )
            .unwrap();
            assert_eq!(frame.pixels(), later.pixels(), "{clock}");
        }
    }

    #[test]
    fn stored_clock_templates_load_with_their_clocks() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let shown = element(ElementKind::ProductionClock(ProductionClockSpec::new(
            clock_name("on-air"),
        )));
        let document =
            fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                template: template(shown, Vec::new()),
                fonts: vec![fm_persistence::TitleFontAsset {
                    family: FAMILY.into(),
                    asset_uri: "asset://fonts/missing.ttf".into(),
                }],
                clocks: clocks().split_off(2),
            })
            .unwrap();
        let path = store.assets_root().join("titles/on-air.fmtitle");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, document).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::MIN),
            "Clocks",
            ProjectSettings {
                frame_rate: rate(),
                video: VideoFormat {
                    dimensions: VideoDimensions::new(320, 180).unwrap(),
                    frame_rate: rate(),
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::I16,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(fm_model::Input {
            id: input(),
            name: "Clock".into(),
            kind: InputKind::Title(TitleInput {
                template_uri: "asset://titles/on-air.fmtitle".into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
            required_capabilities: Vec::new(),
        });
        let assets = TitleAssets::load(&store, &project);
        let loaded = assets.template("asset://titles/on-air.fmtitle").unwrap();
        assert_eq!(loaded.clocks, clocks().split_off(2));
        assert!(loaded.uses_take);
        assert!(loaded.time_dependent());
    }

    #[test]
    fn a_runtime_added_title_template_loads_when_the_engine_first_names_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        // A shape element needs no font, unlike the clock helper's default.
        let mut shape = element(ElementKind::Rectangle);
        shape.style.font = None;
        let runtime = template(shape, Vec::new());
        let document =
            fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                template: runtime.clone(),
                fonts: Vec::new(),
                clocks: Vec::new(),
            })
            .unwrap();
        let path = store.assets_root().join("titles/runtime.fmtitle");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, document).unwrap();

        // The daemon loaded its assets before the input existed.
        let assets = TitleAssets::default().with_store(store);
        let uri = "asset://titles/runtime.fmtitle";
        assert!(assets.template(uri).is_none());
        let changed =
            assets.sync_templates(&[fm_engine::EngineTitleTemplate::new(uri, runtime.clone())]);
        assert_eq!(changed, vec![uri.to_owned()]);
        let loaded = assets.template(uri).expect("loaded on first use");
        assert!(!loaded.time_dependent());
        let frame = render_title_at(
            &assets,
            input(),
            &TitleInput {
                template_uri: uri.into(),
                field_revision: 0,
                fields: Vec::new(),
            },
            at(0, utc(0, 0, 0), None),
            WIDTH,
            HEIGHT,
        )
        .expect("the runtime template renders");
        assert_eq!(frame.width(), WIDTH);
        // The next sync finds the same template and reports nothing.
        assert!(
            assets
                .sync_templates(&[fm_engine::EngineTitleTemplate::new(uri, runtime.clone())])
                .is_empty()
        );
    }

    // -----------------------------------------------------------------------
    // Exact text (font required)
    // -----------------------------------------------------------------------

    /// The picture a plain text title holding `expected` draws.
    fn oracle(catalog: &AssetCatalog, expected: &str) -> Vec<u8> {
        let field = FieldId::new(NonZeroU128::MIN);
        let mut assets = TitleAssets::default();
        assets.insert(
            "asset://titles/oracle",
            LoadedTitleTemplate::new(
                template(
                    element(ElementKind::Text { field }),
                    vec![FieldDefinition {
                        id: field,
                        name: "Text".into(),
                        default: FieldValue::Text(expected.into()),
                        exposed: true,
                    }],
                ),
                catalog.clone(),
            ),
        );
        render_title_at(
            &assets,
            input(),
            &TitleInput {
                template_uri: "asset://titles/oracle".into(),
                field_revision: 0,
                fields: Vec::new(),
            },
            at(0, utc(0, 0, 0), None),
            WIDTH,
            HEIGHT,
        )
        .unwrap()
        .pixels()
        .to_vec()
    }

    #[test]
    fn clock_titles_draw_exactly_the_clock_text_of_the_daemon_time() {
        let Some(catalog) = catalog() else { return };
        let assets = assets(&catalog);
        for (clock, time, expected) in [
            // 30 s before the 23:00 UTC doors, then 65 s after them.
            ("doors", at(0, utc(22, 59, 30), None), "00:30"),
            ("doors", at(0, utc(23, 1, 5), None), "-01:05"),
            // 22:59:30 UTC is 18:59:30 EDT in October.
            ("eastern", at(0, utc(22, 59, 30), None), "18:59:30"),
            // 15:30 UTC on 2026-12-01 is 10:30 EST.
            (
                "eastern",
                at(
                    0,
                    UtcInstant::from_utc_civil(2026, 12, 1, 15, 30, 0, 0).unwrap(),
                    None,
                ),
                "10:30:00",
            ),
            // Taken at frame 25, read at frame 1_600: 63 s at 25 fps.
            ("on-air", at(1_600, utc(0, 0, 0), Some(25)), "01:03"),
            ("on-air", at(1_600, utc(0, 0, 0), None), "00:00"),
        ] {
            let rendered = render_title_at(&assets, input(), &title(clock), time, WIDTH, HEIGHT)
                .unwrap()
                .pixels()
                .to_vec();
            let wanted = oracle(&catalog, expected);
            assert!(wanted != oracle(&catalog, ""), "the font draws {expected}");
            assert!(rendered == wanted, "{clock} at {time:?} is not {expected}");
        }
    }

    const DIRECT_FONTS: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
    ];

    const FONT_ROOTS: &[&str] = &[
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/Library/Fonts",
        "/System/Library/Fonts",
        "C:\\Windows\\Fonts",
    ];

    fn catalog() -> Option<AssetCatalog> {
        let face = std::env::var_os("FM_TITLES_FONT")
            .map(PathBuf::from)
            .into_iter()
            .chain(DIRECT_FONTS.iter().map(PathBuf::from))
            .find_map(|path| parse_font(&path))
            .or_else(|| {
                FONT_ROOTS
                    .iter()
                    .find_map(|root| scan_for_font(Path::new(root), 0))
            });
        let Some(face) = face else {
            assert!(
                std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
                "FM_REQUIRE_FONT=1 but no usable font file was found"
            );
            eprintln!("skipping exact clock text: no font file available");
            return None;
        };
        Some(AssetCatalog::new().with_font(FAMILY, face))
    }

    fn parse_font(path: &Path) -> Option<fm_titles::FontFace> {
        fm_titles::FontFace::from_bytes(std::fs::read(path).ok()?).ok()
    }

    fn scan_for_font(directory: &Path, depth: u32) -> Option<fm_titles::FontFace> {
        if depth > 4 {
            return None;
        }
        let mut entries = std::fs::read_dir(directory)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();
        entries
            .iter()
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("ttf")
                                || extension.eq_ignore_ascii_case("otf")
                        })
            })
            .find_map(|path| parse_font(path))
            .or_else(|| {
                entries
                    .iter()
                    .filter(|path| path.is_dir())
                    .find_map(|path| scan_for_font(path, depth + 1))
            })
    }
}
