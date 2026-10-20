//! Screen and window capture on a Wayland desktop (IN-011).
//!
//! A Wayland compositor shares a screen or a window only through the
//! xdg-desktop-portal `ScreenCast` interface, after the operator picks it
//! in the desktop's own sharing dialog. The share arrives as a `PipeWire`
//! video stream. This module drives that path:
//!
//! - [`portal`] (Linux only): the D-Bus conversation in pure Rust (`zbus`):
//!   `CreateSession`, `SelectSources` (a monitor or a window, the cursor
//!   mode, and `persist_mode` with the restore token of the last grant),
//!   `Start` (the dialog: the share waits for the operator), and
//!   `OpenPipeWireRemote` (the `PipeWire` connection the grant opens).
//! - `pipewire` (Linux, `--cfg freemix_pipewire` builds only): the stream,
//!   negotiated as `BGRx`, `RGBx`, `xRGB`, or `xBGR` shared-memory pictures
//!   of any size (DMA-BUF is not offered, so the compositor copies).
//! - [`convert`]: a shared picture's crop region into the program picture
//!   as opaque RGBA, letterboxed like every other capture.
//! - [`capture`]: [`PortalCapture`], one input's share on a worker thread
//!   behind the [`PortalBackend`] seam: the status a client sees (opening,
//!   waiting for the operator's pick, live, the target gone, reopening,
//!   denied, unsupported), the newest picture, and the newest restore
//!   token, which the server keeps beside the input so a reopened project
//!   shares the same target again without asking where the desktop allows.
//!
//! A window cannot be named in advance on Wayland: the dialog picks it, so
//! a window target only asks for a window (its rule is not matched), and a
//! screen target only for a screen (its index is not used). Crop, cursor,
//! frame rate cap, and stall slate mean what they mean on X11; the crop
//! applies to the shared picture after capture.

pub mod capture;
pub mod convert;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(all(target_os = "linux", freemix_pipewire))]
mod pipewire;
#[cfg(target_os = "linux")]
pub mod portal;

pub use capture::{CaptureSettings, PortalCapture};
pub use convert::{PixelLayout, SourcePicture, letterbox};
#[cfg(target_os = "linux")]
pub use desktop::{DesktopPortal, inventory};
use fm_types::screen_capture::ScreenCaptureTarget;

/// Whether this build reads `PipeWire` streams (`--cfg freemix_pipewire` on
/// Linux), which Wayland capture needs.
pub const PIPEWIRE_BUILT: bool = cfg!(all(target_os = "linux", freemix_pipewire));

/// Why a Wayland session cannot be captured by a build without `PipeWire`.
pub const NO_PIPEWIRE: &str = "Wayland session: this server was built without PipeWire; \
     rebuild it with RUSTFLAGS=\"--cfg freemix_pipewire\" or run it in an X11 session";
/// The operator dismissed the desktop's sharing dialog.
pub const DENIED_CANCELLED: &str = "sharing was cancelled in the sharing dialog on the server's \
     desktop; pick again to ask once more";
/// The desktop refused without a dialog answer.
pub const DENIED_REFUSED: &str = "the server's desktop refused to share a screen or window; pick \
     again to ask once more";
/// The desktop ended a share that was running.
pub const STOPPED_ON_DESKTOP: &str = "sharing was stopped on the server's desktop; pick again to \
     share once more";
/// The desktop's dialog cannot share windows.
pub const NO_WINDOW_SHARING: &str = "this desktop's sharing dialog cannot share single windows; \
     capture a screen instead";
/// The desktop's dialog cannot share screens.
pub const NO_SCREEN_SHARING: &str = "this desktop's sharing dialog cannot share screens";
/// The desktop cannot draw the pointer into the shared picture.
pub const NO_EMBEDDED_CURSOR: &str = "this desktop cannot draw the mouse pointer into a shared \
     picture; it is captured without the pointer";

/// What a share asks the desktop's dialog for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceKind {
    /// A screen (a monitor).
    Monitor,
    /// One application window.
    Window,
}

impl SourceKind {
    /// The kind a configured target asks for.
    #[must_use]
    pub const fn of(target: &ScreenCaptureTarget) -> Self {
        match target {
            ScreenCaptureTarget::Screen { .. } => Self::Monitor,
            ScreenCaptureTarget::Window(_) => Self::Window,
        }
    }

    /// The portal's source type bit (`1` monitor, `2` window).
    #[must_use]
    pub const fn portal_bit(self) -> u32 {
        match self {
            Self::Monitor => 1,
            Self::Window => 2,
        }
    }
}

/// One share's request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareRequest {
    pub kind: SourceKind,
    /// Draw the mouse pointer into the picture.
    pub cursor: bool,
    /// The restore token of the last grant, to share the same target again
    /// without the dialog where the desktop allows it.
    pub restore_token: Option<String>,
}

/// How one share ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShareEnd {
    /// The capture is being dropped.
    Stopped,
    /// The operator or the desktop refused or stopped the share; it is not
    /// asked again on its own.
    Denied(String),
    /// The shared target went away (a closed window, a stream that ended):
    /// the share is asked for again.
    Ended(String),
    /// This desktop cannot share what was asked for.
    Unsupported(String),
    /// The share failed; it is retried with backoff.
    Failed(String),
}

/// What a running share tells its capture.
pub trait ShareSink {
    /// The desktop's dialog now waits for the operator.
    fn awaiting_selection(&mut self);
    /// The desktop granted the share with this restore token (`None` when
    /// it keeps none), and possibly a note about what it could not honour.
    fn granted(&mut self, restore_token: Option<String>, note: Option<&str>);
    /// The shared stream runs. A desktop may send its first picture only
    /// once something on it changes.
    fn streaming(&mut self);
    /// One shared picture.
    fn frame(&mut self, picture: &SourcePicture<'_>);
    /// Called between pictures, at least every tenth of a second while a
    /// share streams: a picture the rate cap held back is shown once due.
    fn idle(&mut self);
    /// Whether the capture is being dropped: a share checks at least every
    /// tenth of a second and ends with [`ShareEnd::Stopped`].
    fn stopping(&self) -> bool;
}

/// Shares a screen or window of a desktop: the seam between a
/// [`PortalCapture`] and the desktop ([`DesktopPortal`] on Linux, a fake in
/// tests).
pub trait PortalBackend: Send + 'static {
    /// Runs one share until it ends, reporting through `sink`.
    fn share(&mut self, request: &ShareRequest, sink: &mut dyn ShareSink) -> ShareEnd;
}

/// The cursor mode to ask for (`1` hidden, `2` embedded, `4` metadata)
/// among the `available` modes, and whether the pointer is drawn as asked.
/// A desktop that lists no modes is asked for none (`None`).
#[must_use]
pub const fn cursor_mode(available: Option<u32>, cursor: bool) -> (Option<u32>, bool) {
    let Some(available) = available else {
        return (None, true);
    };
    if cursor {
        if available & 2 != 0 {
            (Some(2), true)
        } else if available & 1 != 0 {
            (Some(1), false)
        } else {
            (None, false)
        }
    } else if available & 1 != 0 {
        (Some(1), true)
    } else {
        // Only an embedded or metadata pointer: it is shown.
        (None, false)
    }
}

#[cfg(test)]
mod tests {
    use fm_types::screen_capture::WindowRule;

    use super::*;

    #[test]
    fn cursor_modes_follow_what_the_desktop_offers() {
        assert_eq!(cursor_mode(Some(7), true), (Some(2), true));
        assert_eq!(cursor_mode(Some(7), false), (Some(1), true));
        assert_eq!(cursor_mode(Some(5), true), (Some(1), false));
        assert_eq!(cursor_mode(Some(2), false), (None, false));
        assert_eq!(cursor_mode(None, true), (None, true));
    }

    #[test]
    fn every_reason_fits_a_status() {
        for reason in [
            NO_PIPEWIRE,
            DENIED_CANCELLED,
            DENIED_REFUSED,
            STOPPED_ON_DESKTOP,
            NO_WINDOW_SHARING,
            NO_SCREEN_SHARING,
            NO_EMBEDDED_CURSOR,
        ] {
            assert!(
                reason.len() <= fm_types::screen_capture::MAX_SCREEN_CAPTURE_ERROR_BYTES,
                "{reason}"
            );
        }
    }

    #[test]
    fn targets_ask_for_their_kind() {
        assert_eq!(
            SourceKind::of(&ScreenCaptureTarget::Screen { index: 3 }),
            SourceKind::Monitor
        );
        let window = SourceKind::of(&ScreenCaptureTarget::Window(WindowRule::picked()));
        assert_eq!(window, SourceKind::Window);
        assert_eq!(
            (SourceKind::Monitor.portal_bit(), window.portal_bit()),
            (1, 2)
        );
    }
}
