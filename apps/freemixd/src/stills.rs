//! Still image and slideshow pictures on either daemon (IN-008, IN-009).
//!
//! * [`SlideshowFolders`] lists every folder slideshow's asset folder on the
//!   scheduler loop: at once for a slideshow it has not listed yet, then
//!   every [`FOLDER_POLL`], and installs a changed listing with
//!   [`ControlService::set_slideshow_listing`]. A listing is runtime state
//!   (never journaled); the engine keeps the slide on air when the listing
//!   still holds it.
//! * [`decode_picture_file`] decodes one picture file for a raster with the
//!   input's fit, with EXIF orientation and ICC conversion to sRGB (see
//!   `fm_codec_image`).
//! * [`StillPictures`] caches decoded pictures by file, fit, and size for the
//!   simulated monitor tap and the input thumbnails, which draw an image
//!   input, or a slideshow's incoming slide over its outgoing one, through
//!   [`compose_slides`] on the CPU. The native daemon draws the same frames
//!   on the GPU instead.

use std::fs::File;
use std::io::Read;
use std::num::NonZeroU128;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_auth::Policy;
use fm_codec_image::{StillDecodeLimits, decode_still_for_output};
use fm_control::ControlService;
use fm_engine::{EngineMediaFault, EngineSlide, EngineSlideCrop, EngineSlideView};
use fm_frame::{
    ClockDomainId, CpuVideoFrame, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SequenceNumber, TimeBase,
};
use fm_persistence::ProjectStore;
use fm_protocol::WireStillSource;
use fm_sim::ImageFrame;
use fm_types::still::{
    MAX_IMAGE_URI_BYTES, MAX_SLIDESHOW_SLIDES, SlideshowSource, asset_uri_is_valid,
    is_image_file_name,
};
use fm_types::{InputId, OutputScaling, VideoDimensions};

/// How often every folder slideshow's folder is listed again.
pub(crate) const FOLDER_POLL: Duration = Duration::from_secs(2);
/// Most decoded pictures one [`StillPictures`] keeps.
const MAX_CACHED_PICTURES: usize = 12;

/// The clock domain a decoded still's (retained, timeless) frame carries.
fn still_clock_domain() -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::new(1).expect("one is nonzero"))
}

/// The timing of a retained still frame.
pub(crate) fn still_timing(clock_domain: ClockDomainId) -> Option<MediaTiming> {
    let time_base = TimeBase::new(1, 1_000_000_000).ok()?;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), time_base),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(1).ok()?,
        clock_domain,
        SequenceNumber::new(0),
    )
    .ok()
}

/// Decodes the picture file at `path` for an `output` raster placed with
/// `fit`: straight-alpha sRGB RGBA8, exactly `output` sized.
///
/// # Errors
///
/// `source_missing` when the file cannot be read, `unsupported` when it does
/// not decode.
pub(crate) fn decode_picture_file(
    path: &Path,
    output: VideoDimensions,
    fit: OutputScaling,
    clock_domain: ClockDomainId,
) -> Result<CpuVideoFrame, EngineMediaFault> {
    let limits = StillDecodeLimits::default();
    let mut encoded = Vec::new();
    File::open(path)
        .map_err(|_| EngineMediaFault::SourceMissing)?
        .take(u64::try_from(limits.max_encoded_bytes.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut encoded)
        .map_err(|_| EngineMediaFault::SourceMissing)?;
    let timing = still_timing(clock_domain).ok_or(EngineMediaFault::DecodeFailed)?;
    decode_still_for_output(&encoded, timing, limits, output, fit)
        .map(|decoded| decoded.frame)
        .map_err(|_| EngineMediaFault::Unsupported)
}

/// The canonical path of picture `asset_uri` in the bundle whose assets
/// are at `assets_root`, as `ProjectStore::resolve_asset_uri` resolves it:
/// a link (or a linked folder) that leads out of the assets never
/// resolves, so a bundle cannot put a host file on air.
///
/// # Errors
///
/// `source_missing` when the URI is not a valid asset URI, the file is
/// missing or not a regular file, or it resolves outside the assets.
pub(crate) fn resolve_picture_path(
    assets_root: &Path,
    asset_uri: &str,
) -> Result<std::path::PathBuf, EngineMediaFault> {
    fm_model::validate_media_asset_uri(asset_uri).map_err(|_| EngineMediaFault::SourceMissing)?;
    let key = asset_uri
        .strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)
        .ok_or(EngineMediaFault::SourceMissing)?;
    let root = std::fs::canonicalize(assets_root).map_err(|_| EngineMediaFault::SourceMissing)?;
    let path =
        std::fs::canonicalize(root.join(key)).map_err(|_| EngineMediaFault::SourceMissing)?;
    if !path.starts_with(&root) || !std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return Err(EngineMediaFault::SourceMissing);
    }
    Ok(path)
}

/// The pictures directly inside folder `folder_uri` (`asset://<key>`) of
/// the bundle at `assets_root`, in natural name order, at most
/// [`MAX_SLIDESHOW_SLIDES`]. Only regular files with a picture extension
/// count (never a link or a folder); a missing folder lists nothing.
#[must_use]
pub(crate) fn list_folder(assets_root: &Path, folder_uri: &str) -> Vec<EngineSlide> {
    if !asset_uri_is_valid(folder_uri, MAX_IMAGE_URI_BYTES) {
        return Vec::new();
    }
    let Some(key) = folder_uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX) else {
        return Vec::new();
    };
    // A folder that is (or sits under) a link leading out of the assets
    // lists nothing, so host file names never reach a client.
    let (Ok(root), Ok(folder)) = (
        std::fs::canonicalize(assets_root),
        std::fs::canonicalize(assets_root.join(key)),
    ) else {
        return Vec::new();
    };
    if !folder.starts_with(&root) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_image_file_name(name))
        .filter(|name| asset_uri_is_valid(&format!("{folder_uri}/{name}"), MAX_IMAGE_URI_BYTES))
        .collect::<Vec<_>>();
    names.sort_by(|left, right| fm_engine::fm_playback::natural_cmp(left, right));
    names.truncate(MAX_SLIDESHOW_SLIDES);
    names
        .into_iter()
        .map(|name| EngineSlide::new(format!("{folder_uri}/{name}"), None))
        .collect()
}

/// Follows every folder slideshow's folder; see the module documentation.
#[derive(Debug, Default)]
pub(crate) struct SlideshowFolders {
    /// The revision and instant of the last full listing.
    listed: Option<(u64, Instant)>,
}

impl SlideshowFolders {
    /// Lists the folders when the project changed or [`FOLDER_POLL`]
    /// passed, and installs each changed listing.
    pub(crate) fn poll(
        &mut self,
        control: &mut ControlService<Policy>,
        assets_root: &Path,
        now: Instant,
    ) {
        let revision = control.snapshot().snapshot.revision;
        let due = self.listed.is_none_or(|(listed, at)| {
            listed != revision || now.saturating_duration_since(at) >= FOLDER_POLL
        });
        if !due {
            return;
        }
        self.listed = Some((revision, now));
        for state in control.still_states() {
            let WireStillSource::Slideshow(config) = &state.source else {
                continue;
            };
            let SlideshowSource::Folder { asset_uri } = &config.source else {
                continue;
            };
            let input = state.input.to_domain();
            let listing = list_folder(assets_root, asset_uri);
            if control.slideshow_listing(input) != listing.as_slice() {
                let _ = control.set_slideshow_listing(input, listing);
            }
        }
    }
}

/// What one slideshow input shows: its incoming slide over its outgoing
/// one, the incoming mix, and the fit both are placed with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SlidesShown {
    pub(crate) incoming: Option<(Arc<str>, EngineSlideCrop)>,
    pub(crate) outgoing: Option<(Arc<str>, EngineSlideCrop)>,
    /// The incoming slide's opacity, in basis points.
    pub(crate) mix: u16,
    pub(crate) fit: OutputScaling,
}

impl SlidesShown {
    /// What `input` showed on the last realized frame, or `None` before its
    /// first frame or with nothing listed.
    pub(crate) fn of(control: &ControlService<Policy>, input: InputId) -> Option<Self> {
        let status = control.slideshow_status(input)?;
        let (incoming, outgoing, mix) = control.slideshow_shown(input)?;
        let slide =
            |view: Option<EngineSlideView>| view.map(|view| (view.slide.asset_uri, view.crop));
        Some(Self {
            incoming: slide(incoming),
            outgoing: slide(outgoing),
            mix,
            fit: status.config.fit,
        })
    }

    /// Every slideshow input's pictures, ascending by input.
    pub(crate) fn all(control: &ControlService<Policy>) -> Vec<(InputId, Self)> {
        control
            .slideshow_statuses()
            .into_iter()
            .filter_map(|status| Some((status.input, Self::of(control, status.input)?)))
            .collect()
    }
}

/// Identity of one decoded picture.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PictureKey {
    asset_uri: Arc<str>,
    fit: OutputScaling,
    width: u32,
    height: u32,
}

/// Decoded pictures by file, fit, and size, most recently used last; a
/// file that does not decode is remembered as `None` so it is not decoded
/// again on every step.
#[derive(Default)]
pub(crate) struct StillPictures {
    entries: Vec<(PictureKey, Option<Arc<ImageFrame>>)>,
}

impl StillPictures {
    /// The picture of `asset_uri` placed with `fit` in a `width`x`height`
    /// raster (straight alpha), decoding it on a miss.
    pub(crate) fn picture(
        &mut self,
        store: &ProjectStore,
        asset_uri: &Arc<str>,
        fit: OutputScaling,
        (width, height): (u32, u32),
    ) -> Option<Arc<ImageFrame>> {
        let key = PictureKey {
            asset_uri: Arc::clone(asset_uri),
            fit,
            width,
            height,
        };
        if let Some(index) = self.entries.iter().position(|(cached, _)| *cached == key) {
            let entry = self.entries.remove(index);
            let picture = entry.1.clone();
            self.entries.push(entry);
            return picture;
        }
        let picture = decode_picture(store, asset_uri, fit, (width, height)).map(Arc::new);
        if self.entries.len() >= MAX_CACHED_PICTURES {
            self.entries.remove(0);
        }
        self.entries.push((key, picture.clone()));
        picture
    }

    /// An image input's picture over black at `size`.
    pub(crate) fn image(
        &mut self,
        store: &ProjectStore,
        asset_uri: &str,
        fit: OutputScaling,
        size: (u32, u32),
    ) -> Option<ImageFrame> {
        let picture = self.picture(store, &Arc::from(asset_uri), fit, size)?;
        compose_slides(size, &[(&picture, EngineSlideCrop::FULL, 10_000)])
    }

    /// A slideshow's incoming slide over its outgoing one, over black, at
    /// `size`. Black while nothing is listed.
    pub(crate) fn slides(
        &mut self,
        store: &ProjectStore,
        shown: &SlidesShown,
        size: (u32, u32),
    ) -> Option<ImageFrame> {
        let outgoing = shown
            .outgoing
            .as_ref()
            .and_then(|(uri, crop)| Some((self.picture(store, uri, shown.fit, size)?, *crop)));
        let incoming = shown
            .incoming
            .as_ref()
            .and_then(|(uri, crop)| Some((self.picture(store, uri, shown.fit, size)?, *crop)));
        let mut layers = Vec::with_capacity(2);
        if let Some((picture, crop)) = &outgoing {
            layers.push((picture, *crop, 10_000));
        }
        if let Some((picture, crop)) = &incoming {
            let mix = if outgoing.is_some() {
                shown.mix
            } else {
                10_000
            };
            layers.push((picture, *crop, mix));
        }
        compose_slides(size, &layers)
    }
}

fn decode_picture(
    store: &ProjectStore,
    asset_uri: &str,
    fit: OutputScaling,
    (width, height): (u32, u32),
) -> Option<ImageFrame> {
    let path = store.resolve_asset_uri(asset_uri).ok()?;
    let output = VideoDimensions::new(width, height)?;
    let frame = decode_picture_file(&path, output, fit, still_clock_domain()).ok()?;
    let payload = frame.payload();
    let plane = payload.plane(0)?;
    let size = payload.dimensions();
    ImageFrame::new(
        size.width(),
        size.height(),
        plane.stride(),
        plane.bytes().to_vec(),
    )
    .ok()
}

/// Draws straight-alpha `layers` (a picture, the part of it shown, and its
/// opacity in basis points), bottom first, over opaque black at `size`.
/// Each layer's crop is scaled to the whole raster (Ken Burns).
pub(crate) fn compose_slides(
    (width, height): (u32, u32),
    layers: &[(&Arc<ImageFrame>, EngineSlideCrop, u16)],
) -> Option<ImageFrame> {
    let stride = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = usize::try_from(height).ok()?;
    let mut pixels = vec![0_u8; stride.checked_mul(rows)?];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = u8::MAX;
    }
    for (picture, crop, mix) in layers {
        let [x, y, crop_width, crop_height] = crop.fractions();
        let (source_width, source_height) = (picture.width(), picture.height());
        let opacity = f64::from(*mix).min(10_000.0) / 10_000.0;
        for row in 0..height {
            let v = y + (f64::from(row) + 0.5) / f64::from(height) * crop_height;
            let source_row = scaled(v, source_height);
            for column in 0..width {
                let u = x + (f64::from(column) + 0.5) / f64::from(width) * crop_width;
                let Some(source) = picture.pixel(scaled(u, source_width), source_row) else {
                    continue;
                };
                let alpha = f64::from(source.a) / 255.0 * opacity;
                let at = row as usize * stride + column as usize * 4;
                for (channel, value) in pixels[at..at + 3]
                    .iter_mut()
                    .zip([source.r, source.g, source.b])
                {
                    let blended = f64::from(*channel) * (1.0 - alpha) + f64::from(value) * alpha;
                    *channel = channel_byte(blended);
                }
            }
        }
    }
    ImageFrame::new(width, height, stride, pixels).ok()
}

/// A blended channel value as a byte, rounded and clamped.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn channel_byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The pixel a normalized coordinate falls in, clamped to the picture.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scaled(fraction: f64, extent: u32) -> u32 {
    let position = (fraction * f64::from(extent)).floor();
    if position <= 0.0 {
        0
    } else {
        (position as u32).min(extent.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32, rgba: [u8; 4]) -> Arc<ImageFrame> {
        let pixels = rgba.repeat((width * height) as usize);
        Arc::new(ImageFrame::new(width, height, width as usize * 4, pixels).unwrap())
    }

    #[test]
    fn folder_listing_is_natural_ordered_pictures_only() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("slides");
        std::fs::create_dir(&folder).unwrap();
        for name in ["10.png", "2.jpg", "1.PNG", "notes.txt", ".hidden", "b.svg"] {
            std::fs::write(folder.join(name), b"x").unwrap();
        }
        std::fs::create_dir(folder.join("nested.png")).unwrap();
        let uris = list_folder(root.path(), "asset://slides")
            .into_iter()
            .map(|slide| slide.asset_uri.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            uris,
            [
                "asset://slides/1.PNG",
                "asset://slides/2.jpg",
                "asset://slides/10.png",
                "asset://slides/b.svg",
            ]
        );
        assert!(matches!(
            list_folder(root.path(), "asset://missing").as_slice(),
            []
        ));
        assert!(matches!(
            list_folder(root.path(), "asset://../etc").as_slice(),
            []
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pictures_and_folders_never_resolve_outside_the_assets() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), b"x").unwrap();
        let root = tempfile::tempdir().unwrap();
        let assets = root.path().join("assets");
        std::fs::create_dir_all(assets.join("slides")).unwrap();
        std::fs::write(assets.join("slides/inside.png"), b"x").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.png"),
            assets.join("slides/escape.png"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), assets.join("elsewhere")).unwrap();

        assert!(resolve_picture_path(&assets, "asset://slides/inside.png").is_ok());
        assert_eq!(
            resolve_picture_path(&assets, "asset://slides/escape.png"),
            Err(EngineMediaFault::SourceMissing)
        );
        assert_eq!(
            resolve_picture_path(&assets, "asset://elsewhere/secret.png"),
            Err(EngineMediaFault::SourceMissing)
        );
        assert_eq!(
            resolve_picture_path(&assets, "asset://../outside.png"),
            Err(EngineMediaFault::SourceMissing)
        );
        // A folder that leads out of the bundle lists nothing.
        assert!(matches!(
            list_folder(&assets, "asset://elsewhere").as_slice(),
            []
        ));
    }

    #[test]
    fn a_fade_blends_incoming_over_outgoing_over_black() {
        let red = picture(4, 2, [255, 0, 0, 255]);
        let blue = picture(4, 2, [0, 0, 255, 255]);
        let frame = compose_slides(
            (4, 2),
            &[
                (&red, EngineSlideCrop::FULL, 10_000),
                (&blue, EngineSlideCrop::FULL, 2_500),
            ],
        )
        .unwrap();
        let pixel = frame.pixel(1, 1).unwrap();
        assert_eq!((pixel.r, pixel.g, pixel.b, pixel.a), (191, 0, 64, 255));
        let transparent = picture(4, 2, [255, 255, 255, 0]);
        let frame =
            compose_slides((4, 2), &[(&transparent, EngineSlideCrop::FULL, 10_000)]).unwrap();
        let pixel = frame.pixel(0, 0).unwrap();
        assert_eq!((pixel.r, pixel.g, pixel.b, pixel.a), (0, 0, 0, 255));
    }

    #[test]
    fn a_crop_scales_part_of_the_picture_to_the_raster() {
        // Left half black, right half white.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            pixels.extend([0, 0, 0, 255].repeat(2));
            pixels.extend([255, 255, 255, 255].repeat(2));
        }
        let split = Arc::new(ImageFrame::new(4, 2, 16, pixels).unwrap());
        let right = EngineSlideCrop {
            x: 5_000,
            y: 0,
            width: 5_000,
            height: 10_000,
            denominator: 1,
        };
        let frame = compose_slides((4, 2), &[(&split, right, 10_000)]).unwrap();
        for column in 0..4 {
            assert_eq!(frame.pixel(column, 0).unwrap().r, 255);
        }
    }
}
