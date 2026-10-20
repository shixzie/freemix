//! The `PipeWire` video stream a Wayland desktop shares (IN-011), read
//! through libpipewire loaded at run time.
//!
//! A Wayland screen or window share arrives as a `PipeWire` node on a
//! connection the xdg-desktop-portal opens. [`VideoStream`] links a raw
//! video input stream to that node and copies out its newest pictures.
//! libpipewire (`libpipewire-0.3.so.0`, [`LIBRARY`]) is opened with
//! `dlopen` on first use rather than linked, so building needs no
//! `PipeWire` development files and a server without `PipeWire` still
//! starts: [`available`] says why it cannot read a stream.
//!
//! The crate is the workspace's one `unsafe` boundary to libpipewire:
//!
//! - [`pod`] (safe, portable): the SPA pods negotiated (the raw video
//!   `EnumFormat` offered, the shared-memory `Buffers` asked for, and the
//!   fixed `Format` read back), checked against libspa's own builder.
//! - `sys` (Linux): the exported functions used and the leading fields of
//!   the structures read, loaded with `libloading`.
//! - `stream` (Linux): the thread loop, core, and stream, and the callbacks
//!   copying pictures out.
//!
//! Only plain exported functions are called; nothing dispatches through
//! the SPA interface tables header-only helpers use, so the ABI relied on
//! is that of the functions and the stable leading structure fields.

pub mod pod;
#[cfg(target_os = "linux")]
mod stream;
#[cfg(target_os = "linux")]
mod sys;

#[cfg(target_os = "linux")]
pub use stream::{Frame, StreamEnd, Update, VideoStream};
#[cfg(target_os = "linux")]
pub use sys::{LIBRARY, Unavailable};

/// Whether libpipewire can be used here, loading (and initializing) it on
/// the first call.
///
/// # Errors
///
/// The library is not installed, or lacks a function used here.
#[cfg(target_os = "linux")]
pub fn available() -> Result<(), Unavailable> {
    sys::api().map(|_| ())
}
