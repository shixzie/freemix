//! Still image and slideshow input configuration (IN-008, IN-009).
//!
//! An image input shows one picture file from the project's assets, placed
//! in the show raster by an [`OutputScaling`] policy (fit, fill, stretch, or
//! 1:1). A slideshow input shows the pictures of an asset folder (listed in
//! natural file-name order) or of an explicit list, one at a time, each for
//! a number of frames, with a cut or a fade between them, an optional Ken
//! Burns pan and zoom, in authored or seeded random order, looping or not,
//! advancing on its own or only when told to. A slideshow is a paged input
//! (see [`crate::paged`]): next, previous, first, last, and go-to-slide
//! steer it.
//!
//! These are the authored settings. Which pictures a folder holds is
//! runtime state, listed by the server.

use std::fmt;

use crate::OutputScaling;
use crate::paged::PagedNavigation;

/// Prefix of every project asset URI.
pub const ASSET_URI_PREFIX: &str = "asset://";
/// Longest image asset URI, in bytes (the project asset URI bound).
pub const MAX_IMAGE_URI_BYTES: usize = 1024;
/// Longest slideshow folder asset URI, in bytes.
pub const MAX_SLIDESHOW_FOLDER_BYTES: usize = 256;
/// Most slideshow inputs one show holds.
pub const MAX_SLIDESHOW_INPUTS: usize = 8;
/// Most entries of an explicit slideshow list.
pub const MAX_SLIDESHOW_ITEMS: usize = 64;
/// Largest total of an explicit list's URIs and captions, in bytes: small
/// enough that the list, escaped twice on the wire, fits one field. A
/// folder slideshow shows larger sets.
pub const MAX_SLIDESHOW_ITEMS_BYTES: usize = 4 * 1024;
/// Longest slide caption, in bytes.
pub const MAX_SLIDE_CAPTION_BYTES: usize = 256;
/// Most pictures one slideshow folder lists; a larger folder shows its
/// first pictures in name order.
pub const MAX_SLIDESHOW_SLIDES: usize = 10_000;
/// Longest hold of one slide, in frames.
pub const MAX_SLIDE_FRAMES: u32 = 1_000_000;
/// Longest slide fade, in frames.
pub const MAX_SLIDE_TRANSITION_FRAMES: u32 = 3_600;
/// The hold a new slideshow gives each slide: five seconds at 30 fps.
pub const DEFAULT_SLIDE_FRAMES: u32 = 150;
/// The fade a new slideshow uses: one second at 30 fps.
pub const DEFAULT_SLIDE_TRANSITION_FRAMES: u32 = 30;
/// File extensions (lower case) of the picture files an image or slideshow
/// input shows.
pub const IMAGE_FILE_EXTENSIONS: [&str; 9] = [
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "svg",
];

/// Whether `name` (a URI, key, or path) ends in a picture extension.
#[must_use]
pub fn is_image_file_name(name: &str) -> bool {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && IMAGE_FILE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
    })
}

/// Checks an `asset://` URI's syntax: the prefix, then a relative key of
/// `/`-separated non-empty segments without `.`/`..`, backslashes, NUL,
/// `?`, `#`, `%`, `:`, or control characters, at most `maximum` bytes in
/// all.
#[must_use]
pub fn asset_uri_is_valid(uri: &str, maximum: usize) -> bool {
    if uri.len() > maximum {
        return false;
    }
    let Some(key) = uri.strip_prefix(ASSET_URI_PREFIX) else {
        return false;
    };
    !(key.is_empty()
        || key.starts_with('/')
        || key.contains(['\\', '\0', '?', '#', '%', ':'])
        || key.chars().any(char::is_control)
        || key
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | "..")))
}

/// One image input: its picture and how it is placed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImageInputConfig {
    /// The picture file (`asset://<key>`).
    pub asset_uri: String,
    /// How the picture is placed in the show raster.
    pub fit: OutputScaling,
}

impl ImageInputConfig {
    #[must_use]
    pub fn new(asset_uri: impl Into<String>, fit: OutputScaling) -> Self {
        Self {
            asset_uri: asset_uri.into(),
            fit,
        }
    }

    /// Checks the URI and its picture extension.
    ///
    /// # Errors
    ///
    /// Returns [`StillInputError::InvalidUri`] or
    /// [`StillInputError::UnsupportedType`].
    pub fn validate(&self) -> Result<(), StillInputError> {
        validate_image_uri(&self.asset_uri, false)
    }

    /// As [`Self::validate`], but also accepts an absolute host path, which
    /// a client's command names for the server to import (see
    /// [`request_uri_is_valid`]).
    ///
    /// # Errors
    ///
    /// As [`Self::validate`].
    pub fn validate_request(&self) -> Result<(), StillInputError> {
        validate_image_uri(&self.asset_uri, true)
    }
}

/// Whether `uri` is a valid `asset://` URI or, as a client's command may
/// name one for the server to import into the project, an absolute host
/// path (`/…`, `C:\…`, or `\\server\…`) without control characters, at most
/// `maximum` bytes. The show itself only ever holds asset URIs.
#[must_use]
pub fn request_uri_is_valid(uri: &str, maximum: usize) -> bool {
    if asset_uri_is_valid(uri, maximum) {
        return true;
    }
    let bytes = uri.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    uri.len() <= maximum
        && !uri.chars().any(char::is_control)
        && (uri.starts_with('/') || uri.starts_with("\\\\") || drive)
}

fn validate_image_uri(uri: &str, request: bool) -> Result<(), StillInputError> {
    let valid = if request {
        request_uri_is_valid(uri, MAX_IMAGE_URI_BYTES)
    } else {
        asset_uri_is_valid(uri, MAX_IMAGE_URI_BYTES)
    };
    if !valid {
        return Err(StillInputError::InvalidUri);
    }
    if !is_image_file_name(uri) {
        return Err(StillInputError::UnsupportedType);
    }
    Ok(())
}

/// How one slide gives way to the next.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SlideTransitionStyle {
    /// The next slide replaces the current one on the next frame.
    Cut,
    /// The two slides cross-fade over the transition frames.
    #[default]
    Fade,
}

impl SlideTransitionStyle {
    pub const ALL: [Self; 2] = [Self::Cut, Self::Fade];

    /// `cut` or `fade`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Fade => "fade",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.token() == token)
    }
}

/// The order slides play in.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SlideOrder {
    /// The listed order.
    #[default]
    Ordered,
    /// A seeded shuffle per pass that never repeats a slide within a pass:
    /// the same seed and list always play the same order.
    Random { seed: u64 },
}

impl SlideOrder {
    /// `ordered` or `random`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::Random { .. } => "random",
        }
    }
}

/// One entry of an explicit slideshow list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SlideshowItem {
    /// The picture file (`asset://<key>`).
    pub asset_uri: String,
    /// The caption shown for the slide instead of its file name.
    pub caption: Option<String>,
}

impl SlideshowItem {
    #[must_use]
    pub fn new(asset_uri: impl Into<String>) -> Self {
        Self {
            asset_uri: asset_uri.into(),
            caption: None,
        }
    }

    #[must_use]
    pub fn with_caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

/// Where a slideshow's pictures come from.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SlideshowSource {
    /// Every picture file directly inside an asset folder
    /// (`asset://<folder key>`), in natural name order, followed live.
    Folder { asset_uri: String },
    /// An explicit list of pictures, in order.
    List(Vec<SlideshowItem>),
}

/// One slideshow input's authored settings.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SlideshowConfig {
    pub source: SlideshowSource,
    /// How long each slide holds, in frames (`1..=`[`MAX_SLIDE_FRAMES`]).
    pub slide_frames: u32,
    pub transition: SlideTransitionStyle,
    /// The fade length in frames (`1..=`[`MAX_SLIDE_TRANSITION_FRAMES`]);
    /// kept, but unused, by a cut.
    pub transition_frames: u32,
    /// A slow pan and zoom across every slide.
    pub ken_burns: bool,
    pub order: SlideOrder,
    /// Start again after the last slide instead of holding it.
    pub loop_show: bool,
    /// Move to the next slide when a slide's hold ends; off, a slide holds
    /// until the operator moves on.
    pub auto_advance: bool,
    /// How each picture is placed in the show raster.
    pub fit: OutputScaling,
}

impl SlideshowConfig {
    /// A looping, auto-advancing, ordered slideshow with default timing.
    #[must_use]
    pub const fn new(source: SlideshowSource) -> Self {
        Self {
            source,
            slide_frames: DEFAULT_SLIDE_FRAMES,
            transition: SlideTransitionStyle::Fade,
            transition_frames: DEFAULT_SLIDE_TRANSITION_FRAMES,
            ken_burns: false,
            order: SlideOrder::Ordered,
            loop_show: true,
            auto_advance: true,
            fit: OutputScaling::Fit,
        }
    }

    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// Returns the first [`StillInputError`] found.
    pub fn validate(&self) -> Result<(), StillInputError> {
        self.validate_with(false)
    }

    /// As [`Self::validate`], but also accepts absolute host paths, which a
    /// client's command names for the server to import (see
    /// [`request_uri_is_valid`]).
    ///
    /// # Errors
    ///
    /// As [`Self::validate`].
    pub fn validate_request(&self) -> Result<(), StillInputError> {
        self.validate_with(true)
    }

    fn validate_with(&self, request: bool) -> Result<(), StillInputError> {
        match &self.source {
            SlideshowSource::Folder { asset_uri } => {
                let valid = if request {
                    request_uri_is_valid(asset_uri, MAX_SLIDESHOW_FOLDER_BYTES)
                } else {
                    asset_uri_is_valid(asset_uri, MAX_SLIDESHOW_FOLDER_BYTES)
                };
                if !valid {
                    return Err(StillInputError::InvalidFolder);
                }
            }
            SlideshowSource::List(items) => {
                if items.is_empty() {
                    return Err(StillInputError::EmptyList);
                }
                if items.len() > MAX_SLIDESHOW_ITEMS {
                    return Err(StillInputError::TooManyItems);
                }
                let mut bytes = 0usize;
                for item in items {
                    validate_image_uri(&item.asset_uri, request)?;
                    bytes += item.asset_uri.len();
                    if let Some(caption) = &item.caption {
                        if caption.is_empty()
                            || caption.len() > MAX_SLIDE_CAPTION_BYTES
                            || caption.chars().any(char::is_control)
                        {
                            return Err(StillInputError::InvalidCaption);
                        }
                        bytes += caption.len();
                    }
                }
                if bytes > MAX_SLIDESHOW_ITEMS_BYTES {
                    return Err(StillInputError::TooManyItems);
                }
            }
        }
        if !(1..=MAX_SLIDE_FRAMES).contains(&self.slide_frames) {
            return Err(StillInputError::InvalidSlideFrames);
        }
        if !(1..=MAX_SLIDE_TRANSITION_FRAMES).contains(&self.transition_frames) {
            return Err(StillInputError::InvalidTransitionFrames);
        }
        Ok(())
    }

    /// The fade length in frames, `0` for a cut.
    #[must_use]
    pub const fn effective_transition_frames(&self) -> u32 {
        match self.transition {
            SlideTransitionStyle::Cut => 0,
            SlideTransitionStyle::Fade => self.transition_frames,
        }
    }
}

/// Why an image or slideshow configuration is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StillInputError {
    /// A picture URI is not a strict `asset://` URI.
    InvalidUri,
    /// A picture URI does not end in a picture extension.
    UnsupportedType,
    /// The folder is not a strict `asset://` folder URI of at most
    /// [`MAX_SLIDESHOW_FOLDER_BYTES`].
    InvalidFolder,
    /// An explicit list is empty.
    EmptyList,
    /// An explicit list holds too many entries or bytes.
    TooManyItems,
    /// A caption is empty, too long, or holds control characters.
    InvalidCaption,
    /// The slide hold is outside `1..=MAX_SLIDE_FRAMES`.
    InvalidSlideFrames,
    /// The fade is outside `1..=MAX_SLIDE_TRANSITION_FRAMES`.
    InvalidTransitionFrames,
}

impl StillInputError {
    /// The stable code of the error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidUri => "invalid_uri",
            Self::UnsupportedType => "unsupported_type",
            Self::InvalidFolder => "invalid_folder",
            Self::EmptyList => "empty_list",
            Self::TooManyItems => "too_many_items",
            Self::InvalidCaption => "invalid_caption",
            Self::InvalidSlideFrames => "invalid_slide_frames",
            Self::InvalidTransitionFrames => "invalid_transition_frames",
        }
    }

    /// The field the error is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidUri | Self::UnsupportedType => "uri",
            Self::InvalidFolder => "folder",
            Self::EmptyList | Self::TooManyItems | Self::InvalidCaption => "items",
            Self::InvalidSlideFrames => "slide_frames",
            Self::InvalidTransitionFrames => "transition_frames",
        }
    }
}

impl fmt::Display for StillInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUri => "a picture must be a project asset:// URI",
            Self::UnsupportedType => {
                "the file is not a supported picture (PNG, JPEG, WebP, GIF, BMP, TIFF, or SVG)"
            }
            Self::InvalidFolder => {
                "a slideshow folder must be a project asset:// folder of at most 256 bytes"
            }
            Self::EmptyList => "a slideshow list needs at least one picture",
            Self::TooManyItems => {
                "a slideshow list holds at most 128 pictures and 12 KiB of URIs and captions"
            }
            Self::InvalidCaption => "a slide caption is 1 to 256 bytes without control characters",
            Self::InvalidSlideFrames => "a slide holds for 1 to 1000000 frames",
            Self::InvalidTransitionFrames => "a slide fade lasts 1 to 3600 frames",
        })
    }
}

impl std::error::Error for StillInputError {}

/// Longest slide label (a caption or file name), in UTF-8 bytes.
pub const MAX_SLIDE_LABEL_BYTES: usize = 256;

/// Which slide a slideshow shows and how many it lists.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct SlideshowPage {
    /// The zero-based index of the slide on air; `0` when `count` is `0`.
    pub index: u32,
    /// How many slides the slideshow lists; `0` while it has none (an
    /// empty or missing folder).
    pub count: u32,
}

impl SlideshowPage {
    /// Whether the page is consistent: an index inside a non-empty set, or
    /// `0` of `0`, within [`MAX_SLIDESHOW_SLIDES`].
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.count as usize <= MAX_SLIDESHOW_SLIDES
            && if self.count == 0 {
                self.index == 0
            } else {
                self.index < self.count
            }
    }
}

/// The page a navigation step lands on in a slideshow: like
/// [`PagedNavigation::target`], except that a looping slideshow wraps
/// from its last slide to its first and back.
#[must_use]
pub fn slideshow_target(
    navigation: PagedNavigation,
    current: u32,
    count: u32,
    looping: bool,
) -> u32 {
    let last = count.saturating_sub(1);
    match navigation {
        PagedNavigation::Next if looping && count > 0 && current >= last => 0,
        PagedNavigation::Previous if looping && count > 0 && current == 0 => last,
        other => other.target(current, count),
    }
}

/// Whether `label` is a usable slide label: at most
/// [`MAX_SLIDE_LABEL_BYTES`] without control characters.
#[must_use]
pub fn slide_label_is_valid(label: &str) -> bool {
    label.len() <= MAX_SLIDE_LABEL_BYTES && !label.chars().any(char::is_control)
}

/// `label` cut to a valid slide label: control characters dropped and the
/// text truncated on a character boundary to [`MAX_SLIDE_LABEL_BYTES`].
#[must_use]
pub fn bound_slide_label(label: &str) -> String {
    let mut bounded = String::with_capacity(label.len().min(MAX_SLIDE_LABEL_BYTES));
    for character in label.chars().filter(|character| !character.is_control()) {
        if bounded.len() + character.len_utf8() > MAX_SLIDE_LABEL_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(uris: &[&str]) -> SlideshowConfig {
        SlideshowConfig::new(SlideshowSource::List(
            uris.iter().map(|uri| SlideshowItem::new(*uri)).collect(),
        ))
    }

    #[test]
    fn image_uris_need_an_asset_key_and_a_picture_extension() {
        assert!(
            ImageInputConfig::new("asset://logo.PNG", OutputScaling::Fit)
                .validate()
                .is_ok()
        );
        assert_eq!(
            ImageInputConfig::new("/tmp/logo.png", OutputScaling::Fit).validate(),
            Err(StillInputError::InvalidUri)
        );
        assert_eq!(
            ImageInputConfig::new("asset://../logo.png", OutputScaling::Fit).validate(),
            Err(StillInputError::InvalidUri)
        );
        assert_eq!(
            ImageInputConfig::new("asset://clip.mp4", OutputScaling::Fit).validate(),
            Err(StillInputError::UnsupportedType)
        );
    }

    #[test]
    fn slideshow_bounds_are_enforced() {
        assert!(
            list(&["asset://a.jpg", "asset://b.webp"])
                .validate()
                .is_ok()
        );
        assert_eq!(list(&[]).validate(), Err(StillInputError::EmptyList));
        let folder = SlideshowConfig::new(SlideshowSource::Folder {
            asset_uri: "asset://photos/trip".into(),
        });
        assert!(folder.validate().is_ok());
        let bad_folder = SlideshowConfig::new(SlideshowSource::Folder {
            asset_uri: format!("asset://{}", "a".repeat(300)),
        });
        assert_eq!(bad_folder.validate(), Err(StillInputError::InvalidFolder));
        let mut config = folder.clone();
        config.slide_frames = 0;
        assert_eq!(config.validate(), Err(StillInputError::InvalidSlideFrames));
        let mut config = folder.clone();
        config.transition_frames = MAX_SLIDE_TRANSITION_FRAMES + 1;
        assert_eq!(
            config.validate(),
            Err(StillInputError::InvalidTransitionFrames)
        );
        let many = (0..=MAX_SLIDESHOW_ITEMS)
            .map(|index| format!("asset://{index}.png"))
            .collect::<Vec<_>>();
        let many = many.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(list(&many).validate(), Err(StillInputError::TooManyItems));
        let captioned = SlideshowConfig::new(SlideshowSource::List(vec![
            SlideshowItem::new("asset://a.png").with_caption("a\nb"),
        ]));
        assert_eq!(captioned.validate(), Err(StillInputError::InvalidCaption));
        let mut cut = folder;
        cut.transition = SlideTransitionStyle::Cut;
        assert_eq!(cut.effective_transition_frames(), 0);
    }

    #[test]
    fn slideshow_pages_labels_and_looping_targets_are_bounded() {
        use crate::paged::PagedNavigation::{Goto, Next, Previous};
        assert!(SlideshowPage::default().is_valid());
        assert!(SlideshowPage { index: 2, count: 3 }.is_valid());
        assert!(!SlideshowPage { index: 3, count: 3 }.is_valid());
        assert!(!SlideshowPage { index: 1, count: 0 }.is_valid());
        let long = "\u{e9}".repeat(200);
        let bounded = bound_slide_label(&format!("a\nb{long}"));
        assert!(slide_label_is_valid(&bounded));
        assert!(bounded.starts_with("ab"));
        assert!(!slide_label_is_valid("a\tb"));
        assert_eq!(slideshow_target(Next, 2, 3, true), 0);
        assert_eq!(slideshow_target(Next, 2, 3, false), 2);
        assert_eq!(slideshow_target(Previous, 0, 3, true), 2);
        assert_eq!(slideshow_target(Previous, 0, 3, false), 0);
        assert_eq!(slideshow_target(Goto(9), 0, 3, true), 2);
        assert_eq!(slideshow_target(Next, 0, 0, true), 0);
    }
}
