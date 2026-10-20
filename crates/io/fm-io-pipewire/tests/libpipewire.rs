//! The stream reader against the real libpipewire (IN-011).
//!
//! These tests load libpipewire at run time, as a server does. They skip
//! where it cannot be loaded (set `FM_REQUIRE_PIPEWIRE=1` to fail
//! instead), and the second one also needs a running `PipeWire` daemon (its
//! socket `$XDG_RUNTIME_DIR/pipewire-0`). The picture path itself is proven
//! end to end through a desktop portal by `fm-io-linux`'s `wayland_portal`
//! test (`FM_WAYLAND_E2E=1`).

#![cfg(target_os = "linux")]

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fm_io_pipewire::pod::video;
use fm_io_pipewire::{StreamEnd, VideoStream, available};

fn loaded() -> bool {
    match available() {
        Ok(()) => true,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_PIPEWIRE").as_deref() != Ok("1"),
                "FM_REQUIRE_PIPEWIRE=1 but {error}"
            );
            eprintln!("skipped: {error}");
            false
        }
    }
}

/// A connection whose far end is gone ends the stream instead of hanging
/// it, and the stream is torn down cleanly.
#[test]
fn a_dead_connection_ends_the_stream() {
    if !loaded() {
        return;
    }
    let (near, far) = UnixStream::pair().unwrap();
    drop(far);
    let stream = VideoStream::connect(OwnedFd::from(near), 42, c"fm-test", &[video::BGRX]);
    let stream = match stream {
        // Refused at once is an end too.
        Err(error) => {
            eprintln!("refused at once: {error}");
            return;
        }
        Ok(stream) => stream,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let end = loop {
        let update = stream.wait(Duration::from_millis(100));
        assert!(update.frame.is_none());
        if let Some(end) = update.end {
            break end;
        }
        assert!(Instant::now() < deadline, "the stream never ended");
    };
    eprintln!("ended: {end:?}");
    assert!(matches!(end, StreamEnd::Failed(_) | StreamEnd::Ended(_)));
}

/// A stream on a live daemon linked to a node that does not exist fails
/// with the session manager's reason (no picture, no hang), and is torn
/// down cleanly, again and again.
#[test]
fn a_missing_node_on_a_live_daemon_fails_the_stream() {
    if !loaded() {
        return;
    }
    let Some(socket) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|directory| PathBuf::from(directory).join("pipewire-0"))
        .filter(|socket| socket.exists())
    else {
        eprintln!("skipped: no PipeWire daemon socket");
        return;
    };
    for _ in 0..3 {
        let connection = UnixStream::connect(&socket).unwrap();
        let stream = VideoStream::connect(
            OwnedFd::from(connection),
            u32::MAX - 1,
            c"fm-test",
            &[video::BGRX, video::RGBX],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let end = loop {
            let update = stream.wait(Duration::from_millis(100));
            assert!(update.frame.is_none());
            if let Some(end) = update.end {
                break end;
            }
            assert!(Instant::now() < deadline, "the stream never ended");
        };
        let StreamEnd::Failed(reason) = end else {
            panic!("{end:?}");
        };
        assert!(reason.contains("target"), "{reason}");
        drop(stream);
    }
}
