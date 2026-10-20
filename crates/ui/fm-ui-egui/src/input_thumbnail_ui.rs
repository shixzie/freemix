//! Live input bank thumbnails (Protocol 2.59).
//!
//! While the input bank is drawn, the shell asks the server for its input
//! thumbnail feed ([`crate::StudioIntent::InputThumbnails`]) and drops the
//! subscription when the bank is no longer shown. Each received thumbnail is
//! uploaded once per decoded pixel buffer and painted on its tile with a
//! `FEED` tag. A thumbnail goes stale when none has arrived for
//! [`THUMBNAIL_LATE_AFTER`] or, for a live source (a network stream or a
//! capture input), when the server reports that its picture has not changed
//! for [`THUMBNAIL_STALE_AFTER`]: the picture is then dimmed and tagged
//! `STALE` with its age. Delivery gets the longer allowance because the
//! server shares a per-connection budget across every input, so a busy bank
//! legitimately refreshes each tile only every few seconds. A system audio input has no picture; its tile shows a meter-style
//! placeholder driven by the input's audio meters instead.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Painter, Rect, TextureHandle,
    TextureId, TextureOptions, pos2,
};
use fm_protocol::{InputAudioMeters, InputThumbnailImage, MonitorFrameOrigin, WireInputId};

use crate::l10n::{Arg, Locale, Msg};

/// A live source whose picture the server reports unchanged for this long
/// is shown dimmed and tagged `STALE`.
pub(crate) const THUMBNAIL_STALE_AFTER: Duration = Duration::from_secs(3);

/// A thumbnail with no newer one for this long is shown dimmed and tagged
/// `STALE`, whatever its source: the server's keepalive is far shorter.
pub(crate) const THUMBNAIL_LATE_AFTER: Duration = Duration::from_secs(10);

/// Repaint cadence while the bank shows thumbnails, so staleness shows even
/// when no new state arrives.
pub(crate) const THUMBNAIL_REPAINT_INTERVAL: Duration = Duration::from_millis(500);

/// One uploaded thumbnail ready to paint on a tile.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TileThumbnail {
    pub(crate) texture: TextureId,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) origin: MonitorFrameOrigin,
    /// How long the picture has been out of date (see [`staleness`]).
    pub(crate) staleness: Duration,
    /// Whether the picture is shown dimmed and tagged `STALE` (see
    /// [`is_stale`]).
    pub(crate) stale: bool,
}

impl TileThumbnail {
    pub(crate) fn stale(self) -> bool {
        self.stale
    }
}

/// Whether a thumbnail is stale at `now`: none newer has arrived for
/// [`THUMBNAIL_LATE_AFTER`], or a live source's picture had already gone
/// unchanged for [`THUMBNAIL_STALE_AFTER`] when the server sent it.
pub(crate) fn is_stale(
    image: &InputThumbnailImage,
    received_at: Instant,
    now: Instant,
    live_source: bool,
) -> bool {
    now.saturating_duration_since(received_at) >= THUMBNAIL_LATE_AFTER
        || (live_source && Duration::from_millis(u64::from(image.age_ms)) >= THUMBNAIL_STALE_AFTER)
}

/// How out of date a thumbnail is at `now`: the time since it arrived, plus,
/// for a live source, how long its picture had already gone unchanged when
/// the server sent it. A still or generated source legitimately keeps one
/// picture, so its server-side age is ignored.
pub(crate) fn staleness(
    image: &InputThumbnailImage,
    received_at: Instant,
    now: Instant,
    live_source: bool,
) -> Duration {
    let since = now.saturating_duration_since(received_at);
    if live_source {
        since.saturating_add(Duration::from_millis(u64::from(image.age_ms)))
    } else {
        since
    }
}

/// Uploaded textures for the latest thumbnail of each input.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ThumbnailTextures {
    slots: BTreeMap<WireInputId, (Arc<[u8]>, TextureHandle)>,
}

impl std::fmt::Debug for ThumbnailTextures {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_map()
            .entries(
                self.slots
                    .iter()
                    .map(|(input, (_, handle))| (input, handle.id())),
            )
            .finish()
    }
}

fn well_formed(image: &InputThumbnailImage) -> bool {
    image.width > 0
        && image.height > 0
        && image.rgba.len() == usize::from(image.width) * usize::from(image.height) * 4
}

impl ThumbnailTextures {
    /// Uploads changed thumbnails and frees the textures of inputs that no
    /// longer have one.
    pub(crate) fn sync(
        &mut self,
        context: &egui::Context,
        images: &BTreeMap<WireInputId, (InputThumbnailImage, Instant)>,
    ) {
        self.slots.retain(|input, _| {
            images
                .get(input)
                .is_some_and(|(image, _)| well_formed(image))
        });
        for (input, (image, _)) in images {
            if !well_formed(image) {
                continue;
            }
            if self
                .slots
                .get(input)
                .is_some_and(|(pixels, _)| Arc::ptr_eq(pixels, &image.rgba))
            {
                continue;
            }
            let color = ColorImage::from_rgba_unmultiplied(
                [usize::from(image.width), usize::from(image.height)],
                &image.rgba,
            );
            if let Some((pixels, handle)) = self.slots.get_mut(input) {
                handle.set(color, TextureOptions::LINEAR);
                *pixels = Arc::clone(&image.rgba);
            } else {
                let handle = context.load_texture(
                    format!("fm-input-thumbnail-{input}"),
                    color,
                    TextureOptions::LINEAR,
                );
                self.slots.insert(*input, (Arc::clone(&image.rgba), handle));
            }
        }
    }

    /// The paintable thumbnail of `input`, if its uploaded texture matches
    /// the state's current image.
    pub(crate) fn tile(
        &self,
        input: WireInputId,
        images: &BTreeMap<WireInputId, (InputThumbnailImage, Instant)>,
        now: Instant,
        live_source: bool,
    ) -> Option<TileThumbnail> {
        let (pixels, handle) = self.slots.get(&input)?;
        let (image, received_at) = images.get(&input)?;
        Arc::ptr_eq(pixels, &image.rgba).then(|| TileThumbnail {
            texture: handle.id(),
            width: image.width,
            height: image.height,
            origin: image.origin,
            staleness: staleness(image, *received_at, now, live_source),
            stale: is_stale(image, *received_at, now, live_source),
        })
    }

    /// The uploaded texture of `input`'s current thumbnail, if any.
    pub(crate) fn texture(
        &self,
        input: WireInputId,
        images: &BTreeMap<WireInputId, (InputThumbnailImage, Instant)>,
    ) -> Option<TextureId> {
        let (pixels, handle) = self.slots.get(&input)?;
        let (image, _) = images.get(&input)?;
        Arc::ptr_eq(pixels, &image.rgba).then(|| handle.id())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}

/// The tile tag for an own thumbnail, in the console language.
pub(crate) fn thumbnail_tag(l: Locale, thumbnail: TileThumbnail) -> String {
    if thumbnail.stale() {
        #[allow(clippy::cast_possible_truncation)] // ages are far below u64::MAX ms
        let age = l.seconds_tenths(thumbnail.staleness.as_millis() as u64);
        l.format(Msg::ThumbStale, &[("age", Arg::Str(&age))])
    } else {
        l.text(match thumbnail.origin {
            MonitorFrameOrigin::Simulated => Msg::ThumbFeedSim,
            MonitorFrameOrigin::Native => Msg::ThumbFeed,
        })
        .to_owned()
    }
}

/// The tile tooltip line naming an own thumbnail's source and freshness.
pub(crate) fn thumbnail_hover(l: Locale, thumbnail: TileThumbnail) -> String {
    if thumbnail.stale() {
        #[allow(clippy::cast_possible_truncation)] // ages are far below u64::MAX ms
        let age = l.seconds_tenths(thumbnail.staleness.as_millis() as u64);
        l.format(Msg::ThumbnailFeedStale, &[("age", Arg::Str(&age))])
    } else {
        l.text(Msg::ThumbnailFeed).to_owned()
    }
}

/// Paints a system audio input's placeholder: one vertical meter bar per
/// channel (RMS filled, peak as a tick) over the picture area, and, with
/// `caption`, an `audio only` caption (a status badge may take its place).
pub(crate) fn paint_audio_placeholder(
    painter: &Painter,
    thumb: Rect,
    l: Locale,
    meters: Option<&InputAudioMeters>,
    caption: bool,
) {
    let channels = meters.map_or(2, |meters| meters.channels.len().clamp(1, 8));
    let area = Rect::from_min_max(
        pos2(thumb.center().x - 40.0, thumb.min.y + 30.0),
        pos2(thumb.center().x + 40.0, thumb.max.y - 22.0),
    );
    if area.height() <= 4.0 {
        return;
    }
    #[allow(clippy::cast_precision_loss)] // at most eight channels
    let width = (area.width() / channels as f32 - 4.0).max(2.0);
    for channel in 0..channels {
        #[allow(clippy::cast_precision_loss)] // at most eight channels
        let x = area.min.x + channel as f32 * (width + 4.0);
        let bar = Rect::from_min_max(pos2(x, area.min.y), pos2(x + width, area.max.y));
        painter.rect_filled(bar, CornerRadius::same(1), Color32::from_rgb(24, 27, 32));
        let Some(reading) = meters.and_then(|meters| meters.channels.get(channel)) else {
            continue;
        };
        let level = |millionths: u32| meter_fraction(millionths) * bar.height();
        let rms = level(reading.rms_millionths);
        painter.rect_filled(
            Rect::from_min_max(pos2(bar.min.x, bar.max.y - rms), bar.max),
            CornerRadius::same(1),
            Color32::from_rgb(46, 160, 92),
        );
        let peak = bar.max.y - level(reading.peak_millionths);
        painter.line_segment(
            [pos2(bar.min.x, peak), pos2(bar.max.x, peak)],
            egui::Stroke::new(1.5, Color32::from_rgb(220, 200, 80)),
        );
    }
    if !caption {
        return;
    }
    painter.text(
        pos2(thumb.center().x, thumb.max.y - 10.0),
        Align2::CENTER_CENTER,
        l.text(Msg::ThumbAudioOnly),
        FontId::new(crate::theme::legible(9.0), FontFamily::Proportional),
        crate::theme::faded(crate::theme::muted_color(), 0.8),
    );
}

/// A linear amplitude in millionths on a 60 dB meter scale, `0.0..=1.0`.
fn meter_fraction(millionths: u32) -> f32 {
    if millionths == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)] // a meter reading needs no more
    let amplitude = millionths as f32 / 1_000_000.0;
    let db = 20.0 * amplitude.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use super::*;

    fn image(input: u128, age_ms: u32) -> InputThumbnailImage {
        InputThumbnailImage {
            input: WireInputId::from_domain(fm_types::InputId::new(
                NonZeroU128::new(input).unwrap(),
            )),
            sequence: 1,
            origin: MonitorFrameOrigin::Native,
            age_ms,
            width: 2,
            height: 1,
            rgba: vec![255; 8].into(),
        }
    }

    #[test]
    fn live_sources_count_the_servers_unchanged_age_and_stills_do_not() {
        let received = Instant::now();
        let now = received + Duration::from_millis(500);
        let frozen = image(1, 4_000);
        assert_eq!(
            staleness(&frozen, received, now, true),
            Duration::from_millis(4_500)
        );
        assert_eq!(
            staleness(&frozen, received, now, false),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn a_slow_but_moving_feed_is_not_stale() {
        let received = Instant::now();
        let moving = image(1, 0);
        assert!(!is_stale(
            &moving,
            received,
            received + Duration::from_secs(4),
            true
        ));
        assert!(!is_stale(
            &moving,
            received,
            received + Duration::from_secs(9),
            false
        ));
        assert!(is_stale(
            &moving,
            received,
            received + THUMBNAIL_LATE_AFTER,
            true
        ));
        assert!(is_stale(
            &moving,
            received,
            received + THUMBNAIL_LATE_AFTER,
            false
        ));
        let frozen = image(1, 3_000);
        assert!(is_stale(&frozen, received, received, true));
        assert!(
            !is_stale(&frozen, received, received, false),
            "stills keep one picture"
        );
    }

    #[test]
    fn textures_upload_once_per_buffer_and_free_absent_inputs() {
        let context = egui::Context::default();
        let mut textures = ThumbnailTextures::default();
        let now = Instant::now();
        let first = image(1, 0);
        let mut images = BTreeMap::new();
        images.insert(first.input, (first.clone(), now));
        textures.sync(&context, &images);
        let tile = textures.tile(first.input, &images, now, true).unwrap();
        assert!(!tile.stale());
        textures.sync(&context, &images);
        assert_eq!(
            textures
                .tile(first.input, &images, now, true)
                .unwrap()
                .texture,
            tile.texture
        );
        images.clear();
        textures.sync(&context, &images);
        assert!(textures.is_empty());
        assert_eq!(textures.len(), 0);
    }

    #[test]
    fn tags_name_the_origin_or_the_staleness() {
        let l = Locale::ENGLISH;
        let mut tile = TileThumbnail {
            texture: TextureId::default(),
            width: 160,
            height: 90,
            origin: MonitorFrameOrigin::Simulated,
            staleness: Duration::from_millis(100),
            stale: false,
        };
        assert_eq!(thumbnail_tag(l, tile), "FEED · SIM");
        tile.origin = MonitorFrameOrigin::Native;
        assert_eq!(thumbnail_tag(l, tile), "FEED");
        tile.staleness = Duration::from_millis(4_200);
        tile.stale = true;
        assert_eq!(thumbnail_tag(l, tile), "STALE 4.2 s");
    }

    #[test]
    fn meter_scale_spans_sixty_decibels() {
        assert!(meter_fraction(0).abs() < f32::EPSILON);
        assert!((meter_fraction(1_000_000) - 1.0).abs() < 1e-6);
        assert!((meter_fraction(1_000) - 0.0).abs() < 1e-3);
    }
}
