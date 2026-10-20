//! The desktop a native daemon captures screens and windows of (IN-011).
//!
//! [`DesktopBackend::platform`] picks this platform's backend once:
//!
//! - **Linux:** the X11 display of `DISPLAY`, listed through `x11rb`
//!   ([`fm_io_linux::screen_capture::X11Lister`]) and captured through
//!   `ffmpeg -f x11grab`. Inside a Wayland session with `XWayland` only X11
//!   windows are offered; a Wayland session without `XWayland`, or no
//!   `DISPLAY` at all, is unsupported (Wayland only allows capture through
//!   the interactive xdg-desktop-portal, which a headless daemon cannot
//!   answer).
//! - **Windows:** the interactive desktop, listed through PowerShell and
//!   captured through `ffmpeg -f gdigrab` (see [`crate::desktop_windows`]).
//! - **macOS:** `ScreenCaptureKit`, through the `fm-io-macos` screen
//!   capture helper, piped into `ffmpeg` as raw pictures.
//!
//! Every backend produces the same [`DesktopListing`] and resolves targets
//! with the same rules ([`fm_io_linux::screen_capture::resolve`]).

#[cfg(test)]
use std::sync::{Arc, Mutex};

use fm_codec_ffmpeg::network_input::DeviceInput;
use fm_io_linux::screen_capture::{CaptureRegion, DesktopListing, Resolution};
use fm_protocol::{ScreenCaptureConfig, ScreenCaptureInventory};
use fm_types::FrameRate;

/// Why a platform without a backend cannot capture.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const NO_BACKEND: &str = "screen and window capture is not supported on this platform";

/// A desktop listing function, run on a worker thread.
pub(crate) type Lister = Box<dyn FnMut() -> DesktopListing + Send>;

/// This daemon's desktop capture backend.
#[derive(Clone, Debug)]
pub(crate) enum DesktopBackend {
    /// An X11 display (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    X11 { display: String, wayland: bool },
    /// The Windows desktop (`gdigrab`).
    #[cfg(windows)]
    Windows,
    /// `ScreenCaptureKit` through the helper at this path (macOS).
    #[cfg(target_os = "macos")]
    Macos { helper: std::path::PathBuf },
    /// Nothing can be captured here, and why.
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported(String),
    /// A test desktop: listings a test sets, captured as solid colour.
    #[cfg(test)]
    Fake(Arc<Mutex<DesktopListing>>),
}

impl DesktopBackend {
    /// This platform's backend.
    pub(crate) fn platform() -> Self {
        #[cfg(target_os = "linux")]
        {
            match fm_io_linux::screen_capture::session() {
                fm_io_linux::screen_capture::Session::X11 { display, wayland } => {
                    Self::X11 { display, wayland }
                }
                fm_io_linux::screen_capture::Session::Unsupported(reason) => {
                    Self::Unsupported(reason.to_owned())
                }
            }
        }
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            match fm_io_macos::screen_capture::helper() {
                Some(helper) => Self::Macos {
                    helper: helper.to_path_buf(),
                },
                None => Self::Unsupported(NO_BACKEND.to_owned()),
            }
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            Self::Unsupported(NO_BACKEND.to_owned())
        }
    }

    /// A test backend listing what `listings` holds.
    #[cfg(test)]
    pub(crate) fn fake(listings: Arc<Mutex<DesktopListing>>) -> Self {
        Self::Fake(listings)
    }

    /// The backend's token, as listings name it.
    pub(crate) fn token(&self) -> &'static str {
        match self {
            Self::X11 { .. } => fm_io_linux::screen_capture::X11_BACKEND,
            #[cfg(windows)]
            Self::Windows => crate::desktop_windows::BACKEND,
            #[cfg(target_os = "macos")]
            Self::Macos { .. } => fm_io_macos::screen_capture::BACKEND,
            Self::Unsupported(_) => "none",
            #[cfg(test)]
            Self::Fake(_) => "fake",
        }
    }

    /// Why this backend cannot capture, when it cannot.
    pub(crate) fn unsupported_reason(&self) -> Option<&str> {
        // Which other variants exist depends on the platform.
        if let Self::Unsupported(reason) = self {
            Some(reason)
        } else {
            None
        }
    }

    /// A function listing the desktop now; it may block and runs off the
    /// session loop.
    pub(crate) fn lister(&self) -> Lister {
        match self {
            #[cfg(target_os = "linux")]
            Self::X11 { display, wayland } => {
                let mut lister =
                    fm_io_linux::screen_capture::X11Lister::new(display.clone(), *wayland);
                Box::new(move || lister.list())
            }
            #[cfg(not(target_os = "linux"))]
            Self::X11 { .. } => Box::new(|| DesktopListing {
                inventory: ScreenCaptureInventory::unsupported(NO_BACKEND),
                desktop: None,
            }),
            #[cfg(windows)]
            Self::Windows => {
                let mut lister = crate::desktop_windows::WindowsLister::default();
                Box::new(move || lister.list())
            }
            #[cfg(target_os = "macos")]
            Self::Macos { helper } => {
                let helper = helper.clone();
                Box::new(move || DesktopListing {
                    inventory: fm_io_macos::screen_capture::list(&helper),
                    // ScreenCaptureKit reads covered and off-screen windows.
                    desktop: None,
                })
            }
            Self::Unsupported(reason) => {
                let reason = reason.clone();
                Box::new(move || DesktopListing {
                    inventory: ScreenCaptureInventory::unsupported(&reason),
                    desktop: None,
                })
            }
            #[cfg(test)]
            Self::Fake(listings) => {
                let listings = Arc::clone(listings);
                Box::new(move || {
                    listings
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                })
            }
        }
    }

    /// The `ffmpeg` input capturing `region` with `config`'s rate and
    /// cursor, capped by the `program` rate.
    pub(crate) fn capture_device(
        &self,
        region: &CaptureRegion,
        config: &ScreenCaptureConfig,
        program: FrameRate,
    ) -> DeviceInput {
        match self {
            Self::X11 { display, .. } => {
                fm_io_linux::screen_capture::x11grab_device(display, region, config, program)
            }
            #[cfg(windows)]
            Self::Windows => crate::desktop_windows::gdigrab_device(region, config, program),
            #[cfg(target_os = "macos")]
            Self::Macos { helper } => macos_device(helper, region, config, program),
            // Never resolved to a region; an empty format refuses to start.
            Self::Unsupported(_) => DeviceInput {
                format: String::new(),
                options: Vec::new(),
                path: String::new(),
                field_order: None,
                audio: None,
                audio_only: false,
                letterbox: true,
                environment: Vec::new(),
                feeder: None,
            },
            #[cfg(test)]
            Self::Fake(_) => DeviceInput {
                format: "lavfi".to_owned(),
                options: Vec::new(),
                path: format!(
                    "color=c=red:s={}x{}:r={},realtime",
                    region.width,
                    region.height,
                    fm_io_linux::screen_capture::capture_rate(config, program)
                ),
                field_order: None,
                audio: None,
                audio_only: false,
                letterbox: true,
                environment: Vec::new(),
                feeder: None,
            },
        }
    }
}

/// How `config`'s target resolves in `listing` on `backend`.
pub(crate) fn resolve(
    backend: &DesktopBackend,
    listing: &DesktopListing,
    config: &ScreenCaptureConfig,
) -> Resolution {
    match backend.unsupported_reason() {
        Some(reason) => Resolution::Unsupported(reason.to_owned()),
        None => fm_io_linux::screen_capture::resolve(listing, config),
    }
}

/// The `ffmpeg -f rawvideo` input reading `region` from the macOS screen
/// capture helper, which writes BGRA pictures of the region's size at the
/// capture rate to the pipe.
#[cfg(target_os = "macos")]
fn macos_device(
    helper: &std::path::Path,
    region: &CaptureRegion,
    config: &ScreenCaptureConfig,
    program: FrameRate,
) -> DeviceInput {
    use fm_io_macos::screen_capture::{HelperTarget, PIXEL_FORMAT, capture_arguments};
    let rate = fm_io_linux::screen_capture::capture_rate(config, program);
    let target = match (region.window_id, region.screen) {
        (Some(id), _) => HelperTarget::Window(id),
        (None, screen) => HelperTarget::Screen(screen.unwrap_or(0)),
    };
    DeviceInput {
        format: "rawvideo".to_owned(),
        options: vec![
            ("pixel_format".to_owned(), PIXEL_FORMAT.to_owned()),
            (
                "video_size".to_owned(),
                format!("{}x{}", region.width, region.height),
            ),
            ("framerate".to_owned(), rate.to_string()),
        ],
        path: "pipe:0".to_owned(),
        field_order: None,
        audio: None,
        audio_only: false,
        letterbox: true,
        environment: Vec::new(),
        feeder: Some(fm_codec_ffmpeg::network_input::DeviceFeeder {
            program: helper.to_path_buf(),
            args: capture_arguments(
                target,
                (region.x, region.y, region.width, region.height),
                rate,
                config.cursor,
            ),
        }),
    }
}
