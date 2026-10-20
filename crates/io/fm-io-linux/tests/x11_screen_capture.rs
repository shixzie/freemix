//! IN-011 on a real X server: a private `Xvfb` display, an application
//! window this test creates through `x11rb`, the X11 listing, and
//! `ffmpeg -f x11grab` captures of the window and of the screen.
//!
//! Skips when `Xvfb` or `ffmpeg` is unavailable unless
//! `FM_REQUIRE_FFMPEG=1` (which then requires both).
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::network_input::{
    DeviceFeeder, DeviceInput, NetworkInputConfig, NetworkReceiver, Picture,
};
use fm_io_linux::screen_capture::{
    DesktopListing, Resolution, X11Lister, capture_settings, resolve, x11grab_device,
};
use fm_types::FrameRate;
use fm_types::network_stream::NetworkStreamState;
use fm_types::screen_capture::{
    ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureTarget, TitleMatch, WindowRule,
};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

fn available(program: &str, arg: &str) -> bool {
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn tools_available() -> bool {
    let available = available("ffmpeg", "-version") && available("Xvfb", "-help");
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg or Xvfb is unavailable"
    );
    available
}

/// A private `Xvfb` display, killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    fn start(width: u32, height: u32) -> Self {
        for attempt in 0..40_u32 {
            let number = 200 + (std::process::id() + attempt * 7) % 600;
            if Path::new(&format!("/tmp/.X11-unix/X{number}")).exists() {
                continue;
            }
            let display = format!(":{number}");
            let mut child = Command::new("Xvfb")
                .args([
                    display.as_str(),
                    "-screen",
                    "0",
                    &format!("{width}x{height}x24"),
                    "-noreset",
                    "-nolisten",
                    "tcp",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("Xvfb starts");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                if x11rb::connect(Some(&display)).is_ok() {
                    return Self { child, display };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("no Xvfb display could start");
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An application window of class `class` titled `title`, filled with
/// `rgb`.
struct TestWindow {
    connection: RustConnection,
    window: u32,
}

impl TestWindow {
    fn open(
        display: &str,
        class: &str,
        title: &str,
        rgb: u32,
        geometry: (i16, i16, u16, u16),
    ) -> Self {
        let (connection, screen) = x11rb::connect(Some(display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        let (x, y, width, height) = geometry;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                x,
                y,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(rgb),
            )
            .unwrap();
        let class_value = format!("{}\0{class}\0", class.to_lowercase());
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                class_value.as_bytes(),
            )
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();
        connection.map_window(window).unwrap();
        connection.flush().unwrap();
        connection.sync().unwrap();
        Self { connection, window }
    }

    fn resize(&self, width: u32, height: u32) {
        self.connection
            .configure_window(
                self.window,
                &ConfigureWindowAux::new().width(width).height(height),
            )
            .unwrap();
        self.connection.flush().unwrap();
        self.connection.sync().unwrap();
    }

    fn close(self) {
        self.connection.destroy_window(self.window).unwrap();
        self.connection.flush().unwrap();
        self.connection.sync().unwrap();
    }
}

fn rule(title: &str) -> ScreenCaptureConfig {
    ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
        "FreemixTest",
        title,
        None,
    )))
}

fn wait_listing(lister: &mut X11Lister, done: impl Fn(&DesktopListing) -> bool) -> DesktopListing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listing = lister.list();
        if done(&listing) {
            return listing;
        }
        assert!(
            Instant::now() < deadline,
            "listing never settled: {listing:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Pulls until a new picture arrives and returns it.
fn picture(receiver: &NetworkReceiver) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Picture::New(bytes) = receiver.pull().picture {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "no picture: {:?}",
            receiver.status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = usize::try_from((y * width + x) * 4).unwrap();
    [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
}

fn near(actual: [u8; 4], expected: [u8; 3]) -> bool {
    actual[..3]
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 12)
}

#[test]
#[allow(clippy::too_many_lines)]
fn x11_windows_are_listed_captured_followed_and_lost() {
    if !tools_available() {
        eprintln!("skipping: ffmpeg or Xvfb unavailable");
        return;
    }
    let xvfb = Xvfb::start(640, 360);
    let mut lister = X11Lister::new(xvfb.display.clone(), false);
    let empty = lister.list();
    assert!(empty.inventory.supported, "{empty:?}");
    assert_eq!(empty.desktop, Some((640, 360)), "{empty:?}");
    assert_eq!(empty.inventory.screens.len(), 1, "{empty:?}");
    assert_eq!(
        (
            empty.inventory.screens[0].width,
            empty.inventory.screens[0].height
        ),
        (640, 360)
    );

    // A red window is listed with its class, title, and geometry, and the
    // rule resolves to it.
    let red = TestWindow::open(
        &xvfb.display,
        "FreemixTest",
        "Red board",
        0x00ff_0000,
        (40, 30, 320, 180),
    );
    let listing = wait_listing(&mut lister, |listing| {
        listing
            .inventory
            .windows
            .iter()
            .any(|window| window.title == "Red board")
    });
    let window = listing
        .inventory
        .windows
        .iter()
        .find(|window| window.title == "Red board")
        .unwrap();
    assert_eq!(window.app, "FreemixTest");
    assert_eq!(
        (window.x, window.y, window.width, window.height),
        (40, 30, 320, 180)
    );
    assert_eq!(window.id, u64::from(red.window));
    let mut config = rule("Red board");
    config.crop = ScreenCaptureCrop {
        left: 0,
        top: 20,
        right: 0,
        bottom: 0,
    };
    let Resolution::Found(region) = resolve(&listing, &config) else {
        panic!("the red window resolves");
    };
    assert_eq!((region.width, region.height), (320, 160));

    // Its capture shows red, letterboxed into a 16:9 program picture.
    let rate = FrameRate::new(25, 1).unwrap();
    let device = x11grab_device(&xvfb.display, &region, &config, rate);
    let receiver = NetworkReceiver::start(NetworkInputConfig::device(
        device,
        capture_settings(&config),
        64,
        36,
        rate,
    ))
    .unwrap();
    let bytes = picture(&receiver);
    assert_eq!(bytes.len(), 64 * 36 * 4);
    assert!(
        near(pixel(&bytes, 64, 32, 18), [255, 0, 0]),
        "{:?}",
        pixel(&bytes, 64, 32, 18)
    );
    // 320x160 is 2:1: bars above and below.
    assert!(
        near(pixel(&bytes, 64, 32, 0), [0, 0, 0]),
        "{:?}",
        pixel(&bytes, 64, 32, 0)
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while receiver.status().state != NetworkStreamState::Live {
        assert!(Instant::now() < deadline, "{:?}", receiver.status());
        let _ = receiver.pull();
        std::thread::sleep(Duration::from_millis(20));
    }

    // A resize is seen by the listing (the daemon then restarts the
    // capture at the new size).
    red.resize(200, 100);
    let resized = wait_listing(&mut lister, |listing| {
        listing
            .inventory
            .windows
            .iter()
            .any(|window| window.title == "Red board" && window.width == 200)
    });
    let Resolution::Found(smaller) = resolve(&resized, &config) else {
        panic!("the resized window resolves");
    };
    assert_eq!((smaller.width, smaller.height), (200, 80));

    // Closing the window ends its capture and the rule finds nothing.
    red.close();
    let closed = wait_listing(&mut lister, |listing| {
        listing
            .inventory
            .windows
            .iter()
            .all(|window| window.title != "Red board")
    });
    assert!(matches!(resolve(&closed, &config), Resolution::Gone(_)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while matches!(
        receiver.status().state,
        NetworkStreamState::Live | NetworkStreamState::Buffering
    ) {
        assert!(Instant::now() < deadline, "{:?}", receiver.status());
        let _ = receiver.pull();
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(receiver);

    // A window opened again under a new id is found by the same rule, here
    // one matching any title of the application.
    let blue = TestWindow::open(
        &xvfb.display,
        "FreemixTest",
        "Blue board",
        0x0000_00ff,
        (0, 0, 160, 90),
    );
    let any = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule {
        title_match: TitleMatch::Any,
        ..WindowRule::exact("freemixtest", "", Some(u64::from(blue.window) + 1))
    }));
    let reopened = wait_listing(&mut lister, |listing| {
        matches!(resolve(listing, &any), Resolution::Found(_))
    });
    let Resolution::Found(region) = resolve(&reopened, &any) else {
        unreachable!();
    };
    assert_eq!(region.window_id, Some(u64::from(blue.window)));

    // The whole screen: the blue window sits in its top-left corner, the
    // rest is the black root.
    let screen = ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 });
    let Resolution::Found(region) = resolve(&reopened, &screen) else {
        panic!("screen 0 resolves");
    };
    assert_eq!((region.width, region.height), (640, 360));
    let receiver = NetworkReceiver::start(NetworkInputConfig::device(
        x11grab_device(&xvfb.display, &region, &screen, rate),
        capture_settings(&screen),
        64,
        36,
        rate,
    ))
    .unwrap();
    let bytes = picture(&receiver);
    assert!(
        near(pixel(&bytes, 64, 4, 3), [0, 0, 255]),
        "{:?}",
        pixel(&bytes, 64, 4, 3)
    );
    assert!(
        near(pixel(&bytes, 64, 48, 27), [0, 0, 0]),
        "{:?}",
        pixel(&bytes, 64, 48, 27)
    );
    drop(receiver);
    blue.close();
}

#[test]
fn an_unreachable_display_lists_nothing_and_says_why() {
    let mut lister = X11Lister::new(":4999", false);
    let listing = lister.list();
    assert!(listing.inventory.supported);
    assert_eq!(listing.inventory.windows, []);
    assert!(
        listing
            .inventory
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("cannot open X11 display :4999")),
        "{listing:?}"
    );
}

/// The macOS path without macOS: a helper process writing raw BGRA
/// pictures to its standard output (here `ffmpeg` itself, standing in for
/// the `ScreenCaptureKit` helper) is piped into the capture's
/// `ffmpeg -f rawvideo -i pipe:0`, letterboxed like every desktop capture.
#[test]
fn a_feeder_pipes_raw_pictures_into_the_capture() {
    if !tools_available() {
        return;
    }
    let rate = FrameRate::new(25, 1).unwrap();
    let device = DeviceInput {
        format: "rawvideo".to_owned(),
        options: vec![
            ("pixel_format".to_owned(), "bgra".to_owned()),
            ("video_size".to_owned(), "100x50".to_owned()),
            ("framerate".to_owned(), "25".to_owned()),
        ],
        path: "pipe:0".to_owned(),
        field_order: None,
        audio: None,
        audio_only: false,
        letterbox: true,
        environment: Vec::new(),
        feeder: Some(DeviceFeeder {
            program: "ffmpeg".into(),
            args: [
                "-hide_banner",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x00ff00:s=100x50:r=25",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "bgra",
                "pipe:1",
            ]
            .map(str::to_owned)
            .to_vec(),
        }),
    };
    let config = ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 0 });
    let receiver = NetworkReceiver::start(NetworkInputConfig::device(
        device,
        capture_settings(&config),
        64,
        36,
        rate,
    ))
    .unwrap();
    let bytes = picture(&receiver);
    assert_eq!(bytes.len(), 64 * 36 * 4);
    assert!(
        near(pixel(&bytes, 64, 32, 18), [0, 255, 0]),
        "{:?}",
        pixel(&bytes, 64, 32, 18)
    );
    // 100x50 is 2:1: bars above and below.
    assert!(
        near(pixel(&bytes, 64, 32, 0), [0, 0, 0]),
        "{:?}",
        pixel(&bytes, 64, 32, 0)
    );
}
