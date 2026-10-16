//! Acquisition contracts: what a capture backend opens for one device and
//! format, and what it hands the supervisor each program frame.

use fm_types::FrameRate;
use fm_types::capture::{CaptureDevice, CaptureFormat};

/// What to acquire: one device in one format, delivered at the program
/// picture size and rate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquireRequest {
    pub device: CaptureDevice,
    pub format: CaptureFormat,
    /// The audio device read beside the pictures, if any.
    pub audio: Option<String>,
    /// Program picture size every picture is scaled to.
    pub width: u32,
    pub height: u32,
    /// Program frame rate: one picture per program frame.
    pub rate: FrameRate,
    /// Program audio sample rate; audio is stereo.
    pub sample_rate: u32,
}

/// One program frame of an acquisition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcquiredFrame {
    /// A new RGBA picture (`width * 4` bytes per row), or `None` to keep
    /// the current one.
    pub picture: Option<Vec<u8>>,
    /// This frame's planar audio, if the acquisition has audio.
    pub audio: Option<Vec<Vec<f32>>>,
}

/// How an acquisition is doing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquisitionHealth {
    /// Opening the device; no picture yet.
    Starting,
    /// Pictures arrive.
    Live,
    /// Opened, but pictures stopped; the acquisition is reopening the
    /// device by itself.
    Stalled,
    /// The acquisition ended and will not deliver again.
    Failed(String),
}

/// One running acquisition. Dropping it stops it.
pub trait Acquisition: Send {
    /// Takes one program frame; never blocks on the device.
    fn pull(&mut self) -> AcquiredFrame;
    fn health(&self) -> AcquisitionHealth;
    /// Whether audio is acquired with the pictures.
    fn audio_active(&self) -> bool;
    /// The last device error, if any.
    fn last_error(&self) -> Option<String>;
}

/// Opens acquisitions.
pub trait Acquirer: Send + Sync {
    /// Starts acquiring `request`.
    ///
    /// # Errors
    ///
    /// Returns why the acquisition cannot start at all (a device that does
    /// not deliver is reported through [`Acquisition::health`] instead).
    fn start(&self, request: &AcquireRequest) -> Result<Box<dyn Acquisition>, String>;
}
