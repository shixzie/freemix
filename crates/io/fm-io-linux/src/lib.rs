//! Local camera and capture-card inputs on Linux (IN-001).
//!
//! - [`probe`]: what V4L2 reports for each `/dev/video*` node, parsed into
//!   capture devices with YUYV, NV12, and MJPEG formats (sizes, frame
//!   rates, interlaced field order) and a stable key (bus info and card),
//!   and the resolution of a key to a present device.
//! - [`sysfs`]: pairing a video node with the ALSA capture device of the
//!   same hardware.
//! - [`v4l2`] (Linux only): the real probe, through the pure-Rust
//!   `linuxvideo` binding.
//! - [`monitor`]: providers, the polling hot-plug monitor, and hot-plug
//!   diffs.
//! - [`supervisor`]: the per-input state machine (negotiation, signal loss,
//!   recovery under the same input ID).
//! - [`ffmpeg`]: acquisition through a supervised `ffmpeg -f v4l2` child.
//! - [`fake`]: a deterministic fake backend for tests and diagnostics.
//! - [`system_audio`]: the `PulseAudio` and ALSA capture devices a system
//!   audio input can open (AU-011).
//! - [`screen_capture`]: the screens and windows of the X11 desktop and
//!   their capture through `ffmpeg -f x11grab` (IN-011).
//!
//! Everything but [`v4l2`] is portable, so the state machines are tested
//! without hardware on every platform.

#![forbid(unsafe_code)]

pub mod acquire;
pub mod fake;
pub mod ffmpeg;
pub mod monitor;
pub mod probe;
pub mod screen_capture;
pub mod supervisor;
pub mod sysfs;
pub mod system_audio;
#[cfg(target_os = "linux")]
pub mod v4l2;

pub use acquire::{AcquireRequest, AcquiredFrame, Acquirer, Acquisition, AcquisitionHealth};
pub use fake::{FakeAcquirer, FakeCaptureWorld, FakeDevice, FakeProvider};
pub use ffmpeg::FfmpegAcquirer;
pub use monitor::{
    CaptureMonitor, CaptureProvider, DeviceSnapshot, HotplugEvent, V4l2Probe, V4l2Provider, diff,
};
pub use probe::{ProbedDevice, RawFormat, RawIntervals, RawNode, RawSize, V4l2Key, resolve};
pub use supervisor::{CapturePull, CaptureSupervisor, PulledPicture, SupervisorTiming};

/// The provider of this system's capture devices: V4L2 on Linux, none
/// elsewhere.
#[must_use]
pub fn system_provider() -> Option<Box<dyn CaptureProvider>> {
    #[cfg(target_os = "linux")]
    {
        Some(Box::new(V4l2Provider::system()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
