//! Dependency-light title scenes and a deterministic CPU renderer.
//!
//! # What is real
//!
//! * Templates, typed fields, optimistic scene updates, clock and ticker
//!   runtime, and structural validation.
//! * Keyframe animation through the shared `fm_animation` model: every
//!   template carries one [`TitleAnimation`] clip with In and Out phases in
//!   whole frames of its timebase, and
//!   [`ReferenceRenderer::render_sampled`] draws a scene at any
//!   [`AnimSample`] with per-element position, scale about the box centre,
//!   rotation (offscreen, fixed-point bilinear), crop, color, and opacity.
//!   The plain render methods draw the Hold pose.
//! * On-air animation: [`TitleAir`] plays a template's In phase from the
//!   take frame, holds, and plays its Out phase from the off-air frame,
//!   against output frame indices at any output rate, resampled exactly
//!   (see [`TitleAirTiming`]).
//! * [`TitleFieldSet`]: a title's typed, revisioned field values separated
//!   from layout, with compare-and-set updates bounded by
//!   [`MAX_TITLE_FIELDS`], [`MAX_TITLE_TEXT_CHARS`], and
//!   [`MAX_TITLE_CONTENT_BYTES`], and exact restore of persisted values.
//!   [`TitleScene`] delegates its field state to it.
//! * Production clocks: an [`ElementKind::ProductionClock`] element shows a
//!   named `fm_clock` production clock (timer, time of day in a POSIX zone,
//!   media remaining, session) in its own or an element-level preset, read
//!   from a caller-supplied [`ClockSource`] by
//!   [`ReferenceRenderer::render_with_clocks`]. Every title clock - scene,
//!   scoreboard, and production - is formatted by `fm_clock`'s
//!   `format_duration`/`format_time_of_day`, so they cannot diverge.
//! * Text rendering: glyph outlines are rasterized from caller-supplied font
//!   bytes through `ab_glyph`, with kerned advances, ascent-correct baselines,
//!   per-element pixel sizes, greedy word wrapping, horizontal and vertical
//!   alignment, anti-aliased coverage composited into premultiplied RGBA8, and
//!   clipping at the element box and the frame edge.
//! * Image rendering: a caller-decoded premultiplied RGBA8 frame registered
//!   with [`AssetCatalog::with_decoded_image`] is placed inside the element
//!   bounds by an [`ImageFit`] (stretch, contain, cover, or native size) and
//!   the element alignment, resampled with integer nearest-neighbor or
//!   fixed-point bilinear [`ImageFilter`]ing, clipped to the element box and
//!   the frame edge, and composited source-over. An element whose asset has no
//!   decoded pixels keeps the placeholder.
//! * Bounded work: output size, font size, characters per element, lines per
//!   element, glyphs per frame, and per-glyph rasterization area are capped,
//!   and every fill iterates its intersection with the canvas rather than the
//!   element's declared extent. Reaching a per-element cap degrades that
//!   element and is listed in [`RenderReport::degraded`] so the rest of the
//!   frame still goes to air; only a whole-frame cap is a [`RenderError`]. The
//!   catalog likewise caps image asset count and decoded frame bytes.
//! * Ticker crawl/roll: a live [`Crawl`] moves at an exact rational speed
//!   against an engine frame index, wraps seamlessly with a separator, splices
//!   live item updates in at the next item boundary, and draws over a scene via
//!   [`ReferenceRenderer::render_with_crawls`].
//! * Scoreboards: a typed [`Scoreboard`] with bounded/saturating score and
//!   period actions, a frame-exact [`GameClock`] with a once-only expiry event,
//!   external key/value mapping, and atomic publication onto template fields.
//! * Designer document ([`TitleDesign`]): a bounded tree of elements and
//!   nested groups whose offsets and opacity compose exactly onto children,
//!   rectangular and rounded-rect group [`ClipMask`]s applied per pixel by
//!   [`ReferenceRenderer::render_composed`], an In/Out animation clip whose
//!   group tracks fold onto descendants, a fixed-point [`Easing`] library, a
//!   [`TitlePlayback`] take/hold/off-air state machine, and a
//!   [`DesignEditor`] whose every edit validates atomically with bounded
//!   undo/redo.
//!
//! # What is still missing
//!
//! * Grapheme-level shaping. Text is laid out in codepoint order: no bidi, no
//!   complex-script shaping, no ligatures or other OpenType features.
//! * Image decoding. Callers hand the renderer decoded premultiplied RGBA8
//!   frames; this crate parses no image files and applies no color profiles,
//!   orientation, animation, or mipmapped minification. See
//!   [`REFERENCE_RENDERER_LIMITATIONS`].
//! * GPU rasterization, production effects, and any pipeline wiring. Nothing
//!   in the project schema instantiates these scenes yet. The designer has no
//!   UI, persistence, protocol commands, or anti-aliased mask edges, and its
//!   group transforms are translation and opacity only (no scale/rotation).

mod animation;
mod crawl;
mod design;
mod easing;
mod editor;
mod font;
mod id;
mod image;
mod model;
mod on_air;
mod playback;
pub mod production_clock;
mod render;
mod runtime;
mod scoreboard;
mod text;
mod validation;

pub use animation::{
    AnimSample, AnimationEditError, TitleAnimation, TitleClip, default_title_timebase,
};
pub use crawl::{
    Crawl, CrawlConfig, CrawlError, CrawlItem, CrawlMeasure, CrawlMode, CrawlSchedule,
    CrawlSegment, CrawlSegmentKind, CrawlSpeed, CrawlView, FontCrawlMeasure, MAX_CRAWL_EPOCHS,
    MAX_CRAWL_GAP_PX, MAX_CRAWL_ITEM_CHARS, MAX_CRAWL_ITEMS, MAX_CRAWL_SEPARATOR_CHARS,
    MAX_CRAWL_VISIBLE_SEGMENTS,
};
pub use design::{
    ClipMask, Composition, DesignError, DesignNode, ElementPlacement, GROUP_PROPERTIES, Group,
    GroupTransform, MAX_DESIGN_NAME_CHARS, MAX_DESIGN_NODES, MAX_GROUP_DEPTH, ResolvedClip,
    TitleDesign,
};
pub use easing::{
    CubicBezier, EASE_ONE, Easing, EasingError, EasingPreset, MAX_BEZIER_Y, MIN_BEZIER_Y,
    ease_between, progress_q16,
};
pub use editor::{DEFAULT_UNDO_DEPTH, DesignEdit, DesignEditor, MAX_UNDO_DEPTH, NodeSlot};
/// The shared keyframe model titles animate with.
pub use fm_animation;
pub use font::{FontError, FontFace, MAX_FONT_BYTES};
pub use id::{ElementId, FieldId, TemplateId, TitleId};
pub use model::{
    Alignment, Bounds, Color, Element, ElementKind, FieldDefinition, FieldSetError, FieldType,
    FieldValue, FontStyle, HorizontalAlignment, ImageFilter, ImageFit, ImageSource,
    InstantiationError, MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_TEXT_CHARS, Style,
    TitleFieldSet, TitleScene, TitleTemplate, UpdateError, VerticalAlignment,
};
pub use on_air::{TitleAir, TitleAirFrame, TitleAirPhase, TitleAirTiming};
pub use playback::{PlaybackError, PlaybackOutcome, PlaybackPhase, TitlePlayback};
pub use production_clock::{
    ClockSource, ClockUnresolved, ProductionClockSpec, TitleClock, TitleClockError, TitleClockKind,
    TitleClockTime, evaluate_production_clock, title_clock_registry, validate_title_clocks,
};
pub use render::{
    CrawlLayer, Degradation, DegradedElement, MAX_FONT_SIZE_PX, MAX_GLYPH_RASTER_DIMENSION,
    MAX_GLYPH_RASTER_PIXELS, MAX_GLYPHS_PER_ELEMENT, MAX_GLYPHS_PER_FRAME, MAX_OUTPUT_HEIGHT,
    MAX_OUTPUT_WIDTH, MAX_ROTATED_ELEMENT_PIXELS, REFERENCE_RENDERER_LIMITATIONS,
    ReferenceRenderer, RenderError, RenderOutput, RenderReport,
};
pub use runtime::{
    ClockDirection, ClockFormat, ClockSpec, TickerDirection, TickerSpec, evaluate_clock,
    evaluate_ticker_position,
};
pub use scoreboard::{
    DEFAULT_SHOT_CLOCK_MS, GameClock, GameClockConfig, MAX_GAME_CLOCK_MS, MAX_ROSTER_PLAYERS,
    MAX_SCOREBOARD_ACTIONS, MAX_SCOREBOARD_TEXT_CHARS, Player, Scoreboard, ScoreboardAction,
    ScoreboardBinding, ScoreboardError, ScoreboardEvent, ScoreboardOutcome, ScoreboardPreset,
    ScoreboardRules, TeamSide, TeamState, actions_from_external,
};
pub use validation::{
    AssetCatalog, ImageAssetError, MAX_IMAGE_ASSET_BYTES, MAX_IMAGE_ASSETS, MissingFont,
    MissingImage, ValidationError, ValidationReport, validate_scene, validate_template,
};
