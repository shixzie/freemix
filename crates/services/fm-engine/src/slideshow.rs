//! Slideshow inputs and paged navigation (IN-009).
//!
//! A slideshow input's authored settings are its replaceable source
//! ([`fm_model::InputSourceSpec::Slideshow`]); which pictures it shows is
//! its *listing*: the explicit list's entries, or the pictures the server
//! found in its asset folder, reported through
//! [`crate::Engine::set_slideshow_listing`]. The transport is engine
//! runtime: one [`fm_playback::Slideshow`] per slideshow input with a
//! non-empty listing, advanced exactly once per engine frame (a paused or
//! closed input holds). Runtime state is never snapshotted: a restored
//! slideshow starts on its first slide.
//!
//! Paged navigation ([`fm_types::paged::PagedNavigation`]) is a live
//! operation accepted for the next frame and realized on it, so journal
//! replay reproduces every slide change on its exact frame. Acceptance
//! depends only on the show (the input is a slideshow), never on the
//! runtime listing, so a replayed journal accepts exactly what the live
//! show did; a navigation of an empty slideshow does nothing, and a go-to
//! past the end lands on the last slide. Every slide
//! carries a Ken Burns motion in a normalized [`KEN_BURNS_SPACE`] square;
//! a renderer reads each view's crop as fractions of the slide's fitted
//! picture and scales the crop to fill the raster.
//!
//! Each frame reports every slideshow input's views and page
//! ([`EngineSlideshowFrame`]); `changed` marks a frame whose page, label,
//! or listing differs from the previous one, which is when clients hear
//! about it.

use std::collections::BTreeMap;
use std::sync::Arc;

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_playback::{
    CropRect, KenBurns, PixelRect, Slide, SlideId, SlideOrder as PlaybackOrder, SlideTransition,
    Slideshow, SlideshowConfig as PlaybackConfig,
};
use fm_types::InputId;
use fm_types::paged::PagedNavigation;
use fm_types::still::{
    MAX_SLIDE_CAPTION_BYTES, MAX_SLIDESHOW_SLIDES, SlideOrder, SlideTransitionStyle,
    SlideshowConfig, SlideshowPage, SlideshowSource, bound_slide_label, slideshow_target,
};

use crate::ShowState;

/// The side of the normalized square every Ken Burns crop is expressed in.
pub const KEN_BURNS_SPACE: u32 = 10_000;
/// The side of the zoomed Ken Burns rectangle: about a 1.15x zoom.
const KEN_BURNS_ZOOMED: u32 = 8_700;

/// One picture a slideshow lists.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EngineSlide {
    /// The picture file (`asset://<key>`).
    pub asset_uri: Arc<str>,
    /// The caption the slide shows instead of its file name.
    pub caption: Option<String>,
}

impl EngineSlide {
    #[must_use]
    pub fn new(asset_uri: impl Into<Arc<str>>, caption: Option<String>) -> Self {
        Self {
            asset_uri: asset_uri.into(),
            caption,
        }
    }

    /// The slide's file name: the last segment of its URI.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.asset_uri.rsplit('/').next().unwrap_or(&self.asset_uri)
    }

    /// What the slide is called: its caption, else its file name, as a
    /// bounded page label.
    #[must_use]
    pub fn label(&self) -> String {
        bound_slide_label(self.caption.as_deref().unwrap_or_else(|| self.file_name()))
    }
}

/// An exact crop in the [`KEN_BURNS_SPACE`] square:
/// `numerator / (denominator * KEN_BURNS_SPACE)` of the picture per edge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EngineSlideCrop {
    pub x: u64,
    pub y: u64,
    pub width: u64,
    pub height: u64,
    pub denominator: u64,
}

impl EngineSlideCrop {
    /// The whole picture.
    pub const FULL: Self = Self {
        x: 0,
        y: 0,
        width: KEN_BURNS_SPACE as u64,
        height: KEN_BURNS_SPACE as u64,
        denominator: 1,
    };

    const fn of(crop: CropRect) -> Self {
        Self {
            x: crop.x_numerator(),
            y: crop.y_numerator(),
            width: crop.width_numerator(),
            height: crop.height_numerator(),
            denominator: crop.denominator(),
        }
    }

    /// The crop as `[x, y, width, height]` fractions of the picture.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fractions(self) -> [f64; 4] {
        let scale = (self.denominator.max(1) * u64::from(KEN_BURNS_SPACE)) as f64;
        [
            self.x as f64 / scale,
            self.y as f64 / scale,
            self.width as f64 / scale,
            self.height as f64 / scale,
        ]
    }

    /// Whether the crop is the whole picture.
    #[must_use]
    pub fn is_full(self) -> bool {
        self.x == 0
            && self.y == 0
            && self.width == self.height
            && self.width == self.denominator * u64::from(KEN_BURNS_SPACE)
    }
}

/// One slide on air in one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSlideView {
    /// The slide's position in the listing.
    pub index: u32,
    pub slide: EngineSlide,
    pub crop: EngineSlideCrop,
}

/// One slideshow input on one engine frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSlideshowFrame {
    pub input: InputId,
    /// The slide shown (fading in during a transition); `None` while the
    /// slideshow lists no pictures.
    pub incoming: Option<EngineSlideView>,
    /// The slide fading out, during a transition.
    pub outgoing: Option<EngineSlideView>,
    /// The incoming slide's weight in basis points (10 000 on a hold).
    pub mix_basis_points: u16,
    /// The transition bringing the incoming slide in.
    pub transition: SlideTransitionStyle,
    pub page: SlideshowPage,
    /// The incoming slide's label; empty without slides.
    pub label: String,
    /// Increases whenever the listing changes.
    pub generation: u64,
    /// The page, label, or listing differs from the previous frame's, or
    /// a navigation was realized on this frame.
    pub changed: bool,
}

/// A slideshow input's replicated runtime state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSlideshowStatus {
    pub input: InputId,
    pub config: SlideshowConfig,
    pub page: SlideshowPage,
    pub label: String,
    pub generation: u64,
}

/// Why a slideshow listing cannot be installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineSlideshowError {
    /// The input is not a slideshow input of the show.
    NotSlideshowInput(InputId),
    /// The slideshow shows an explicit list, which it lists itself.
    NotFolderSource(InputId),
}

impl std::fmt::Display for EngineSlideshowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSlideshowInput(input) => {
                write!(formatter, "input {input} is not a slideshow input")
            }
            Self::NotFolderSource(input) => {
                write!(formatter, "slideshow input {input} does not show a folder")
            }
        }
    }
}

impl std::error::Error for EngineSlideshowError {}

/// A slideshow input's runtime transport. Never snapshotted.
#[derive(Clone, Debug, Default)]
pub(crate) struct SlideshowRuntime {
    /// The settings the transport was built for.
    config: Option<SlideshowConfig>,
    listing: Vec<EngineSlide>,
    /// The listing's slide ids in listing order.
    order: Vec<SlideId>,
    /// Every slide that may be on air, by id (listed or retiring).
    meta: BTreeMap<SlideId, EngineSlide>,
    show: Option<Slideshow>,
    /// The transport frame shown last, which a held slideshow repeats.
    last: Option<fm_playback::SlideshowFrame>,
    /// Accepted navigations not yet realized, in acceptance order.
    pending: Vec<PagedNavigation>,
    generation: u64,
    /// The page, label, and generation the previous frame reported.
    reported: Option<(SlideshowPage, String, u64)>,
}

impl SlideshowRuntime {
    pub(crate) fn accept(&mut self, navigation: PagedNavigation) {
        self.pending.push(navigation);
    }

    /// Installs a folder's listing (a source change installs the list's).
    pub(crate) fn set_listing(&mut self, listing: Vec<EngineSlide>) {
        let mut listing = listing;
        listing.truncate(MAX_SLIDESHOW_SLIDES);
        for slide in &mut listing {
            if let Some(caption) = &slide.caption {
                let bounded = bound_slide_label(caption);
                slide.caption = (!bounded.is_empty() && bounded.len() <= MAX_SLIDE_CAPTION_BYTES)
                    .then_some(bounded);
            }
        }
        if listing == self.listing {
            return;
        }
        self.listing = listing;
        self.generation = self.generation.wrapping_add(1);
        self.resync();
    }

    /// Follows the input's current settings: a changed explicit list
    /// installs its entries, a changed folder forgets the old listing until
    /// the server lists the new one, and changed timing rebuilds the
    /// transport on the slide on air.
    pub(crate) fn follow(&mut self, config: &SlideshowConfig) {
        if self.config.as_ref() == Some(config) {
            return;
        }
        let previous = self.config.replace(config.clone());
        match (
            &config.source,
            previous.as_ref().map(|config| &config.source),
        ) {
            (SlideshowSource::List(items), _) => {
                let listing = items
                    .iter()
                    .map(|item| EngineSlide::new(item.asset_uri.as_str(), item.caption.clone()))
                    .collect();
                self.set_listing(listing);
            }
            (
                SlideshowSource::Folder { asset_uri },
                Some(SlideshowSource::Folder { asset_uri: old }),
            ) if asset_uri == old => {}
            (SlideshowSource::Folder { .. }, _) => self.set_listing(Vec::new()),
        }
        let current = self.show.as_ref().map(Slideshow::current);
        self.show = None;
        self.rebuild(current);
    }

    /// The slides of the listing with their stable ids.
    fn slides(&mut self) -> Vec<Slide> {
        let mut seen = BTreeMap::<Arc<str>, u32>::new();
        let mut used = std::collections::BTreeSet::new();
        let mut slides = Vec::with_capacity(self.listing.len());
        for slide in &self.listing {
            let occurrence = seen.entry(slide.asset_uri.clone()).or_default();
            let mut id = slide_id(&slide.asset_uri, *occurrence);
            *occurrence += 1;
            // A hash collision (practically never) takes the next free id,
            // which the listing's order makes deterministic too.
            while !used.insert(id) {
                id = SlideId::new(id.get().wrapping_add(1));
            }
            let mut built = Slide::new(id, slide.file_name(), KEN_BURNS_SPACE, KEN_BURNS_SPACE);
            if self.config.as_ref().is_some_and(|config| config.ken_burns) {
                built = built.with_ken_burns(ken_burns(id));
            }
            slides.push(built);
        }
        self.order = slides.iter().map(Slide::id).collect();
        let on_air = self
            .show
            .as_ref()
            .map(Slideshow::on_air)
            .unwrap_or_default();
        self.meta.retain(|id, _| on_air.contains(id));
        for (id, slide) in self.order.iter().zip(&self.listing) {
            self.meta.insert(*id, slide.clone());
        }
        slides
    }

    /// Rebuilds the transport from the listing, holding `current` when it
    /// is still listed.
    fn rebuild(&mut self, current: Option<SlideId>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let slides = self.slides();
        if slides.is_empty() {
            self.show = None;
            return;
        }
        let playback = playback_config(&config);
        let show = match current.filter(|id| slides.iter().any(|slide| slide.id() == *id)) {
            Some(first) => Slideshow::new_at(slides, playback, first),
            None => Slideshow::new(slides, playback),
        };
        self.show = show.ok();
    }

    /// Installs a changed listing into the running transport, keeping the
    /// slide on air.
    fn resync(&mut self) {
        let slides = self.slides();
        if slides.is_empty() {
            self.show = None;
            return;
        }
        match &mut self.show {
            Some(show) => {
                if show.sync(slides).is_err() {
                    let current = show.current();
                    self.rebuild(Some(current));
                }
            }
            None => self.rebuild(None),
        }
    }

    fn page(&self) -> SlideshowPage {
        let Some(show) = &self.show else {
            return SlideshowPage::default();
        };
        let count = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
        SlideshowPage {
            index: self
                .listed_index(show.current())
                .min(count.saturating_sub(1)),
            count,
        }
    }

    /// The listing position of `id`; a retiring slide reports where it
    /// stood in the transport's list.
    fn listed_index(&self, id: SlideId) -> u32 {
        let index = self
            .order
            .iter()
            .position(|candidate| *candidate == id)
            .or_else(|| self.show.as_ref().and_then(|show| show.index_of(id)))
            .unwrap_or(0);
        u32::try_from(index).unwrap_or(u32::MAX)
    }

    /// The slide `navigation` lands on from the slide on air.
    fn target(&self, navigation: PagedNavigation) -> Option<SlideId> {
        self.show.as_ref()?;
        let config = self.config.as_ref()?;
        let page = self.page();
        let index = slideshow_target(navigation, page.index, page.count, config.loop_show);
        self.order.get(usize::try_from(index).ok()?).copied()
    }

    fn view(&self, view: fm_playback::SlideView) -> Option<EngineSlideView> {
        Some(EngineSlideView {
            index: self.listed_index(view.id),
            slide: self.meta.get(&view.id)?.clone(),
            crop: EngineSlideCrop::of(view.crop),
        })
    }

    /// Realizes `navigations` of the pending ones, then advances one frame
    /// unless `held`.
    pub(crate) fn tick(
        &mut self,
        input: InputId,
        config: &SlideshowConfig,
        navigations: usize,
        held: bool,
    ) -> EngineSlideshowFrame {
        self.follow(config);
        let realized: Vec<PagedNavigation> = self
            .pending
            .drain(..navigations.min(self.pending.len()))
            .collect();
        for navigation in &realized {
            // In random order, Next takes the slide auto-advance would,
            // starting the next cycle at the end of this one.
            if *navigation == PagedNavigation::Next
                && self
                    .config
                    .as_ref()
                    .is_some_and(|config| matches!(config.order, SlideOrder::Random { .. }))
                && let Some(show) = &mut self.show
                && show.jump_next().is_some()
            {
                continue;
            }
            if let Some(target) = self.target(*navigation)
                && let Some(show) = &mut self.show
            {
                let _ = show.jump(target);
            }
        }
        let generation_changed = self
            .reported
            .as_ref()
            .is_some_and(|(_, _, generation)| *generation != self.generation);
        let frame = match &mut self.show {
            // A held (paused or closed) slideshow repeats its last frame
            // until a navigation or listing change moves it.
            Some(_)
                if held && realized.is_empty() && !generation_changed && self.last.is_some() =>
            {
                self.last
            }
            Some(show) => Some(show.tick()),
            None => None,
        };
        self.last.clone_from(&frame);
        let (incoming, outgoing, mix) = match frame {
            Some(frame) => (Some(frame.incoming), frame.outgoing, frame.mix_basis_points),
            None => (None, None, fm_playback::FULL_MIX_BASIS_POINTS),
        };
        let incoming = incoming.and_then(|view| self.view(view));
        let outgoing = outgoing.and_then(|view| self.view(view));
        let page = incoming
            .as_ref()
            .map_or_else(SlideshowPage::default, |view| SlideshowPage {
                index: view.index.min(self.page().count.saturating_sub(1)),
                count: self.page().count,
            });
        let label = incoming
            .as_ref()
            .map(|view| view.slide.label())
            .unwrap_or_default();
        let reported = (page, label.clone(), self.generation);
        let changed = !realized.is_empty() || self.reported.as_ref() != Some(&reported);
        self.reported = Some(reported);
        EngineSlideshowFrame {
            input,
            incoming,
            outgoing,
            mix_basis_points: mix,
            transition: config.transition,
            page,
            label,
            generation: self.generation,
            changed,
        }
    }

    /// Whether the next frame differs from this one absent new commands.
    pub(crate) fn advancing(&self, held: bool) -> bool {
        // A navigation, a first frame, or a listing the last frame did not
        // report yet shows on the next frame even while held.
        if !self.pending.is_empty()
            || self
                .reported
                .as_ref()
                .is_none_or(|(_, _, generation)| *generation != self.generation)
        {
            return true;
        }
        if held {
            return false;
        }
        let Some(show) = &self.show else {
            return false;
        };
        let config = show.config();
        config.auto_advance && show.len() > 1
            || show.on_air().len() > 1
            || self.config.as_ref().is_some_and(|config| config.ken_burns)
    }

    pub(crate) fn status(&self, input: InputId, config: &SlideshowConfig) -> EngineSlideshowStatus {
        // What the last frame showed; before the first frame, what the
        // first will show.
        let (page, label) = match &self.reported {
            Some((page, label, _)) => (*page, label.clone()),
            None => (
                self.page(),
                self.show
                    .as_ref()
                    .and_then(|show| self.meta.get(&show.current()))
                    .map(EngineSlide::label)
                    .unwrap_or_default(),
            ),
        };
        EngineSlideshowStatus {
            input,
            config: config.clone(),
            page,
            label,
            generation: self.generation,
        }
    }

    /// The pictures the last frame showed (incoming, outgoing, mix), or
    /// `None` before the first frame or with nothing listed.
    pub(crate) fn shown(&self) -> Option<(Option<EngineSlideView>, Option<EngineSlideView>, u16)> {
        let last = self.last.as_ref()?;
        Some((
            self.view(last.incoming),
            last.outgoing.and_then(|view| self.view(view)),
            last.mix_basis_points,
        ))
    }

    pub(crate) fn listing(&self) -> &[EngineSlide] {
        &self.listing
    }
}

fn playback_config(config: &SlideshowConfig) -> PlaybackConfig {
    let transition = match config.effective_transition_frames() {
        0 => SlideTransition::Cut,
        frames => SlideTransition::Crossfade { frames },
    };
    PlaybackConfig {
        transition,
        order: match config.order {
            SlideOrder::Ordered => PlaybackOrder::Ordered,
            SlideOrder::Random { seed } => PlaybackOrder::Random { seed },
        },
        loop_show: config.loop_show,
        auto_advance: config.auto_advance,
        ..PlaybackConfig::new(u64::from(config.slide_frames))
    }
}

/// The stable id of the `occurrence`th listing of `asset_uri`: a hash of
/// the file and the occurrence, never the order listings were seen in, so
/// a replayed show (which may see its folder's listing grow differently)
/// gives every slide the same id and Ken Burns motion.
fn slide_id(asset_uri: &str, occurrence: u32) -> SlideId {
    // 128-bit FNV-1a: stable across builds and platforms.
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in asset_uri
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(occurrence.to_le_bytes())
    {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    SlideId::new(hash)
}

/// Sorts a folder listing into its one deterministic order: natural order
/// of file names, then of whole URIs, then captions.
pub(crate) fn sort_folder_listing(listing: &mut [EngineSlide]) {
    listing.sort_by(|left, right| {
        fm_playback::natural_cmp(left.file_name(), right.file_name())
            .then_with(|| left.asset_uri.cmp(&right.asset_uri))
            .then_with(|| left.caption.cmp(&right.caption))
    });
}

/// A slide's Ken Burns motion: a zoom in or out between the whole picture
/// and a corner, chosen by the slide's id.
fn ken_burns(id: SlideId) -> KenBurns {
    let full = PixelRect::new(0, 0, KEN_BURNS_SPACE, KEN_BURNS_SPACE);
    let offset = KEN_BURNS_SPACE - KEN_BURNS_ZOOMED;
    let (x, y) = match id.get() % 4 {
        0 => (0, 0),
        1 => (offset, offset),
        2 => (offset, 0),
        _ => (0, offset),
    };
    let zoomed = PixelRect::new(x, y, KEN_BURNS_ZOOMED, KEN_BURNS_ZOOMED);
    if (id.get() / 4).is_multiple_of(2) {
        KenBurns {
            start: full,
            end: zoomed,
        }
    } else {
        KenBurns {
            start: zoomed,
            end: full,
        }
    }
}

/// The slideshow settings of `input`, or a refusal naming why it is not a
/// paged input.
pub(crate) fn paged_config(
    state: &ShowState,
    input: InputId,
) -> Result<&SlideshowConfig, Rejection> {
    match state.input_source(input) {
        Some(fm_model::InputSourceSpec::Slideshow(config)) => Ok(config),
        _ if !state.inputs().contains(&input) => {
            let message = format!("input {input} does not exist");
            Err(Rejection::new(RejectionCode::NotFound, message.clone())
                .with_field(FieldIssue::new("input", "unknown_input", message)))
        }
        _ => {
            let message = format!("input {input} is not a paged input (a slideshow or a document)");
            Err(
                Rejection::new(RejectionCode::InvalidCommand, message.clone())
                    .with_field(FieldIssue::new("input", "not_paged", message)),
            )
        }
    }
}
