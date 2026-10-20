//! The desktop a native daemon captures screens and windows of (IN-011).
//!
//! [`DesktopBackend::platform`] picks this platform's backend once:
//!
//! - **Linux:** the X11 display of `DISPLAY`, listed through `x11rb`
//!   ([`fm_io_linux::screen_capture::X11Lister`]) and captured through
//!   `ffmpeg -f x11grab`. A Wayland session is captured through its
//!   xdg-desktop-portal and `PipeWire` in builds with `PipeWire`
//!   ([`DesktopBackend::Portal`], see [`crate::native_portal_capture`]):
//!   nothing is listed, and the desktop's own dialog picks the screen or
//!   window. In other builds a Wayland session with `XWayland` offers only
//!   X11 windows, and one without it, or no `DISPLAY` at all, is
//!   unsupported.
//! - **Windows:** the interactive desktop, listed through PowerShell and
//!   captured through `ffmpeg -f gdigrab` (see [`crate::desktop_windows`]).
//! - **macOS:** `ScreenCaptureKit`, through the `fm-io-macos` screen
//!   capture helper, piped into `ffmpeg` as raw pictures.
//!
//! Every backend produces the same [`DesktopListing`] and resolves targets
//! with the same rules ([`fm_io_linux::screen_capture::resolve`]).
//!
//! A capture's sound ([`DesktopBackend::audio_plan`]) is planned per
//! platform beside its picture:
//!
//! - **Linux:** system audio is the `PulseAudio`/`PipeWire` default
//!   output's monitor, read by the capture child itself; application audio
//!   is an [`AudioTap`](fm_io_linux::capture_audio::AudioTap) recording the
//!   window process's own playback streams. The sound is keyed by the
//!   window's process (or, without one, its application id), never by the
//!   picture backend, so any desktop listing that names a process works.
//! - **Windows:** a tap running a WASAPI loopback (system) or process
//!   loopback (application, Windows 10 2004 or later) helper.
//! - **macOS:** a tap running the `ScreenCaptureKit` helper's audio
//!   capture (macOS 13 or later).

#[cfg(test)]
use std::sync::{Arc, Mutex};

use fm_codec_ffmpeg::network_input::{DeviceAudioInput, DeviceInput};
use fm_io_linux::capture_audio::TapSource;
use fm_io_linux::screen_capture::{CaptureRegion, DesktopListing, Resolution};
use fm_protocol::{ScreenCaptureAudio, ScreenCaptureConfig, ScreenCaptureInventory, WindowInfo};
use fm_types::FrameRate;

/// Why a platform without a backend cannot capture.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const NO_BACKEND: &str = "screen and window capture is not supported on this platform";

/// Why application audio cannot be captured without a window.
pub(crate) const NO_WINDOW_APP: &str = "application audio needs a window capture";

/// How a capture's sound is captured.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AudioPlan {
    /// The capture is silent.
    Silent,
    /// The capture child opens this audio input itself, keeping these
    /// environment variables.
    Device {
        input: DeviceAudioInput,
        environment: Vec<String>,
    },
    /// A tap gathers the sound and serves it to the capture child.
    Tap(TapSource),
    /// The sound cannot be captured here, and why.
    Unsupported(String),
}

/// A desktop listing function, run on a worker thread.
pub(crate) type Lister = Box<dyn FnMut() -> DesktopListing + Send>;

/// This daemon's desktop capture backend.
#[derive(Clone, Debug)]
pub(crate) enum DesktopBackend {
    /// An X11 display (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    X11 { display: String, wayland: bool },
    /// A Wayland desktop's xdg-desktop-portal (Linux builds with
    /// `PipeWire`).
    #[cfg(target_os = "linux")]
    Portal,
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
                fm_io_linux::screen_capture::Session::Portal => Self::Portal,
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
            #[cfg(target_os = "linux")]
            Self::Portal => fm_protocol::PORTAL_BACKEND,
            #[cfg(windows)]
            Self::Windows => crate::desktop_windows::BACKEND,
            #[cfg(target_os = "macos")]
            Self::Macos { .. } => fm_io_macos::screen_capture::BACKEND,
            Self::Unsupported(_) => "none",
            #[cfg(test)]
            Self::Fake(_) => "fake",
        }
    }

    /// What starts a share of the desktop, when this backend is a Wayland
    /// desktop's portal: its captures run through
    /// [`crate::native_portal_capture`] instead of a listing.
    pub(crate) fn portal(&self) -> Option<crate::native_portal_capture::PortalFactory> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Portal => Some(std::sync::Arc::new(|| {
                Box::new(fm_io_linux::wayland::DesktopPortal::new())
                    as Box<dyn fm_io_linux::wayland::PortalBackend>
            })),
            _ => None,
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
            #[cfg(target_os = "linux")]
            Self::Portal => Box::new(|| DesktopListing {
                inventory: fm_io_linux::wayland::inventory(None),
                desktop: None,
            }),
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
            // A portal capture reads its stream itself (see
            // `crate::native_portal_capture`).
            #[cfg(target_os = "linux")]
            Self::Portal => {
                Self::Unsupported(String::new()).capture_device(region, config, program)
            }
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

impl DesktopBackend {
    /// How the sound `config` asks for is captured beside `region`, whose
    /// window (if a window is captured) the listing describes as `window`.
    pub(crate) fn audio_plan(
        &self,
        config: &ScreenCaptureConfig,
        region: &CaptureRegion,
        window: Option<&WindowInfo>,
    ) -> AudioPlan {
        // Only some platforms place their audio by the captured region.
        let _ = region;
        let audio = config.audio;
        if audio == ScreenCaptureAudio::None {
            return AudioPlan::Silent;
        }
        if audio == ScreenCaptureAudio::Application && window.is_none() {
            return AudioPlan::Unsupported(NO_WINDOW_APP.to_owned());
        }
        match self {
            Self::X11 { .. } => linux_audio_plan(audio, window),
            #[cfg(windows)]
            Self::Windows => crate::desktop_windows::audio_plan(audio, window),
            #[cfg(target_os = "macos")]
            Self::Macos { helper } => macos_audio_plan(helper, audio, region, window),
            // Portal captures run in `native_portal_capture`, which captures
            // their system audio itself; nothing plans it here.
            #[cfg(target_os = "linux")]
            Self::Portal => match audio {
                ScreenCaptureAudio::Application => AudioPlan::Unsupported(
                    crate::native_portal_capture::NO_PORTAL_APP_AUDIO.to_owned(),
                ),
                ScreenCaptureAudio::None | ScreenCaptureAudio::System => {
                    linux_audio_plan(audio, window)
                }
            },
            Self::Unsupported(reason) => AudioPlan::Unsupported(reason.clone()),
            #[cfg(test)]
            Self::Fake(_) => linux_audio_plan(audio, window),
        }
    }
}

/// The Linux plan: the default output's monitor for system audio, the
/// window process's own streams for application audio.
fn linux_audio_plan(audio: ScreenCaptureAudio, window: Option<&WindowInfo>) -> AudioPlan {
    use fm_io_linux::capture_audio::{AppAudioKey, audio_server_environment, pulse_system_audio};
    match audio {
        ScreenCaptureAudio::None => AudioPlan::Silent,
        ScreenCaptureAudio::System => AudioPlan::Device {
            input: pulse_system_audio(),
            environment: audio_server_environment(),
        },
        ScreenCaptureAudio::Application => {
            let Some(window) = window else {
                return AudioPlan::Unsupported(NO_WINDOW_APP.to_owned());
            };
            AudioPlan::Tap(TapSource::pulse_application(AppAudioKey {
                pid: window.pid,
                app: window.app.clone(),
            }))
        }
    }
}

/// The macOS plan: the `ScreenCaptureKit` helper's audio capture.
#[cfg(target_os = "macos")]
fn macos_audio_plan(
    helper: &std::path::Path,
    audio: ScreenCaptureAudio,
    region: &CaptureRegion,
    window: Option<&WindowInfo>,
) -> AudioPlan {
    use fm_io_linux::capture_audio::ProducerCommand;
    use fm_io_macos::screen_capture::{HelperAudio, audio_arguments};
    let target = match audio {
        ScreenCaptureAudio::None => return AudioPlan::Silent,
        // The system output is the same through any display; a window's
        // capture reads it through the first.
        ScreenCaptureAudio::System => HelperAudio::Display(region.screen.unwrap_or(0)),
        ScreenCaptureAudio::Application => {
            match window.and_then(|window| window.pid).filter(|pid| *pid != 0) {
                Some(pid) => HelperAudio::App(pid),
                None => {
                    return AudioPlan::Unsupported("the window's process is unknown".to_owned());
                }
            }
        }
    };
    AudioPlan::Tap(TapSource::Command(ProducerCommand {
        program: helper.to_path_buf(),
        args: audio_arguments(target),
        environment: Vec::new(),
    }))
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
