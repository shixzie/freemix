//! The Wayland screen capture's portal conversation (IN-011).
//!
//! The fake-portal tests serve an `org.freedesktop.portal.Desktop` with a
//! `ScreenCast` interface on a private `dbus-daemon` and drive the real
//! client against it: the requests and their options (types, cursor mode,
//! `persist_mode`, the restore token), the dialog wait, a cancelled
//! dialog, a desktop without window sharing, and the session's `Closed`
//! signal. They skip when `dbus-daemon` is not installed.
//!
//! The end-to-end test needs a real headless Wayland desktop with a
//! screen sharing portal and `PipeWire` (see the IN-011 notes in
//! `docs/spec/implementation-roadmap.md`), with libpipewire on the
//! loader's path at run time (nothing `PipeWire` is needed to build): it
//! runs when `FM_WAYLAND_E2E=1` and the test's session bus
//! (`DBUS_SESSION_BUS_ADDRESS`) is that desktop's. Where libpipewire
//! cannot be loaded, the desktop is checked to report itself unsupported
//! (with the advice to install `PipeWire`) without asking its portal.

#![cfg(target_os = "linux")]
// The fake portal's methods take what the D-Bus interface macro hands them.
#![allow(
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::unused_self
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fm_io_linux::wayland::portal::{Capabilities, Portal, Refusal};
use fm_io_linux::wayland::{DENIED_CANCELLED, NO_WINDOW_SHARING, ShareRequest, SourceKind};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, interface};

/// A private bus, killed on drop.
struct Bus {
    child: Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bus() -> Option<Bus> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut address = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut address)
        .ok()?;
    let address = address.trim().to_owned();
    if address.is_empty() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    Some(Bus { child, address })
}

/// What the fake portal does and saw.
#[derive(Debug, Default)]
struct Script {
    source_types: u32,
    /// The `Start` response code (0 granted, 1 cancelled).
    start_response: u32,
    restore_token: Option<String>,
    seen: Vec<(String, HashMap<String, OwnedValue>)>,
    session: Option<OwnedObjectPath>,
}

struct FakeScreenCast {
    script: Arc<Mutex<Script>>,
}

/// The request path a call's `handle_token` names.
fn request_path(sender: &str, options: &HashMap<String, OwnedValue>) -> OwnedObjectPath {
    let token: &str = options
        .get("handle_token")
        .and_then(|token| token.downcast_ref::<&str>().ok())
        .expect("a handle token");
    let sender = sender.trim_start_matches(':').replace('.', "_");
    OwnedObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    ))
    .unwrap()
}

async fn respond(
    connection: &Connection,
    sender: &str,
    path: &ObjectPath<'_>,
    code: u32,
    results: HashMap<&str, Value<'_>>,
) {
    // A test that ended before the dialog answered has closed its bus.
    let _ = connection
        .emit_signal(
            Some(sender),
            path,
            "org.freedesktop.portal.Request",
            "Response",
            &(code, results),
        )
        .await;
}

#[interface(name = "org.freedesktop.portal.ScreenCast")]
impl FakeScreenCast {
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        let sender = header.sender().unwrap().to_string();
        let path = request_path(&sender, &options);
        let token: &str = options
            .get("session_handle_token")
            .and_then(|token| token.downcast_ref::<&str>().ok())
            .unwrap();
        let session = format!(
            "/org/freedesktop/portal/desktop/session/{}/{token}",
            sender.trim_start_matches(':').replace('.', "_")
        );
        {
            let mut script = self.script.lock().unwrap();
            script.session = Some(OwnedObjectPath::try_from(session.clone()).unwrap());
            script.seen.push(("CreateSession".into(), options));
        }
        respond(
            connection,
            &sender,
            &path,
            0,
            HashMap::from([("session_handle", Value::from(session))]),
        )
        .await;
        path
    }

    async fn select_sources(
        &self,
        session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        assert!(session.as_str().contains("/session/"));
        let sender = header.sender().unwrap().to_string();
        let path = request_path(&sender, &options);
        self.script
            .lock()
            .unwrap()
            .seen
            .push(("SelectSources".into(), options));
        respond(connection, &sender, &path, 0, HashMap::new()).await;
        path
    }

    fn start(
        &self,
        session: OwnedObjectPath,
        parent: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        assert!(session.as_str().contains("/session/") && parent.is_empty());
        let sender = header.sender().unwrap().to_string();
        let path = request_path(&sender, &options);
        let (code, token) = {
            let mut script = self.script.lock().unwrap();
            script.seen.push(("Start".into(), options));
            (script.start_response, script.restore_token.clone())
        };
        let connection = connection.clone();
        let response_path = path.clone();
        // The dialog answers later, as the operator picks.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            async_io::block_on(async {
                let stream: (u32, HashMap<&str, Value<'_>>) =
                    (42, HashMap::from([("source_type", Value::from(1_u32))]));
                let mut results = HashMap::from([("streams", Value::from(vec![stream]))]);
                if let Some(token) = token {
                    results.insert("restore_token", Value::from(token));
                }
                respond(&connection, &sender, &response_path, code, results).await;
            });
        });
        path
    }

    fn open_pipe_wire_remote(
        &self,
        _session: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::zvariant::OwnedFd {
        let (reader, _writer) = std::io::pipe().unwrap();
        zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(reader))
    }

    #[zbus(property, name = "AvailableSourceTypes")]
    fn available_source_types(&self) -> u32 {
        self.script.lock().unwrap().source_types
    }

    #[zbus(property, name = "AvailableCursorModes")]
    fn available_cursor_modes(&self) -> u32 {
        7
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        5
    }
}

struct FakeSession;

#[interface(name = "org.freedesktop.portal.Session")]
impl FakeSession {
    fn close(&self) {}

    #[zbus(signal)]
    async fn closed(
        emitter: &SignalEmitter<'_>,
        details: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;
}

/// A fake portal on `bus` following `script`.
async fn serve(bus: &Bus, script: &Arc<Mutex<Script>>) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FakeScreenCast {
                script: Arc::clone(script),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn option<'a>(
    seen: &'a [(String, HashMap<String, OwnedValue>)],
    call: &str,
    key: &str,
) -> Option<&'a OwnedValue> {
    seen.iter()
        .find(|(name, _)| name == call)
        .and_then(|(_, options)| options.get(key))
}

fn request(kind: SourceKind, token: Option<&str>) -> ShareRequest {
    ShareRequest {
        kind,
        cursor: true,
        restore_token: token.map(str::to_owned),
    }
}

#[test]
fn a_granted_share_asks_with_the_restore_token_and_keeps_the_new_one() {
    let Some(bus) = bus() else {
        eprintln!("skipped: dbus-daemon is not installed");
        return;
    };
    let script = Arc::new(Mutex::new(Script {
        source_types: 3,
        restore_token: Some("restore-2".into()),
        ..Script::default()
    }));
    async_io::block_on(async {
        let server = serve(&bus, &script).await;
        let mut portal = Portal::connect(Some(&bus.address)).await.unwrap();
        let capabilities = portal.capabilities().await.unwrap();
        assert_eq!(
            capabilities,
            Capabilities {
                version: 5,
                source_types: 3,
                cursor_modes: Some(7)
            }
        );
        let mut awaited = 0;
        let grant = portal
            .share(
                &request(SourceKind::Window, Some("restore-1")),
                &capabilities,
                &|| false,
                &mut || awaited += 1,
            )
            .await
            .unwrap();
        assert_eq!(awaited, 1, "the dialog wait is reported once");
        assert_eq!(grant.node_id, 42);
        assert_eq!(grant.restore_token.as_deref(), Some("restore-2"));
        assert_eq!(grant.note, None);
        {
            let script = script.lock().unwrap();
            let seen = &script.seen;
            let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, ["CreateSession", "SelectSources", "Start"]);
            let number = |key| {
                option(seen, "SelectSources", key)
                    .and_then(|value| value.downcast_ref::<u32>().ok())
            };
            assert_eq!(number("types"), Some(2), "a window");
            assert_eq!(number("cursor_mode"), Some(2), "the pointer drawn in");
            assert_eq!(number("persist_mode"), Some(2), "kept until revoked");
            assert_eq!(
                option(seen, "SelectSources", "restore_token")
                    .and_then(|value| value.downcast_ref::<&str>().ok()),
                Some("restore-1")
            );
        }

        // The desktop stops sharing: the session's Closed signal.
        assert!(!portal.closed());
        let session = script.lock().unwrap().session.clone().unwrap();
        server
            .object_server()
            .at(&session, FakeSession)
            .await
            .unwrap();
        let emitter = SignalEmitter::new(&server, session).unwrap();
        FakeSession::closed(&emitter, HashMap::new()).await.unwrap();
        let mut closed = false;
        for _ in 0..50 {
            if portal.closed() {
                closed = true;
                break;
            }
            async_io::Timer::after(Duration::from_millis(20)).await;
        }
        assert!(closed, "the Closed signal is seen");
        portal.close().await;
    });
}

#[test]
fn a_cancelled_dialog_is_denied_and_a_missing_source_type_is_unsupported() {
    let Some(bus) = bus() else {
        eprintln!("skipped: dbus-daemon is not installed");
        return;
    };
    let script = Arc::new(Mutex::new(Script {
        source_types: 1,
        start_response: 1,
        ..Script::default()
    }));
    async_io::block_on(async {
        let _server = serve(&bus, &script).await;
        let mut portal = Portal::connect(Some(&bus.address)).await.unwrap();
        let capabilities = portal.capabilities().await.unwrap();
        let refused = portal
            .share(
                &request(SourceKind::Window, None),
                &capabilities,
                &|| false,
                &mut || {},
            )
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::Unsupported(NO_WINDOW_SHARING.into()));
        assert!(script.lock().unwrap().seen.is_empty(), "nothing asked");

        let refused = portal
            .share(
                &request(SourceKind::Monitor, None),
                &capabilities,
                &|| false,
                &mut || {},
            )
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::Denied(DENIED_CANCELLED.into()));
        let script = script.lock().unwrap();
        assert_eq!(
            option(&script.seen, "SelectSources", "types")
                .and_then(|value| value.downcast_ref::<u32>().ok()),
            Some(1),
            "a screen"
        );
        assert!(
            option(&script.seen, "SelectSources", "restore_token").is_none(),
            "no token to restore"
        );
    });
}

#[test]
fn a_dialog_left_open_is_dismissed_when_the_capture_stops() {
    let Some(bus) = bus() else {
        eprintln!("skipped: dbus-daemon is not installed");
        return;
    };
    let script = Arc::new(Mutex::new(Script {
        source_types: 3,
        ..Script::default()
    }));
    async_io::block_on(async {
        let _server = serve(&bus, &script).await;
        let mut portal = Portal::connect(Some(&bus.address)).await.unwrap();
        let capabilities = portal.capabilities().await.unwrap();
        let stop = std::cell::Cell::new(false);
        let refused = portal
            .share(
                &request(SourceKind::Monitor, None),
                &capabilities,
                &|| stop.get(),
                // Stops as soon as the dialog is up, before it answers.
                &mut || stop.set(true),
            )
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::Stopped);
    });
}

/// A share that must never reach the portal's dialog.
struct Silent;
impl fm_io_linux::wayland::ShareSink for Silent {
    fn awaiting_selection(&mut self) {
        panic!("the portal was asked");
    }
    fn granted(&mut self, _: Option<String>, _: Option<&str>) {}
    fn streaming(&mut self) {}
    fn frame(&mut self, _: &fm_io_linux::wayland::SourcePicture<'_>) {}
    fn idle(&mut self) {}
    fn stopping(&self) -> bool {
        false
    }
}

/// Without libpipewire a Wayland desktop is unsupported, and no share
/// asks its portal (nothing listens on the bus named here).
#[test]
fn a_desktop_without_pipewire_is_unsupported() {
    use fm_io_linux::wayland::{DesktopPortal, NO_PIPEWIRE, PortalBackend, pipewire_available};

    if pipewire_available() {
        eprintln!("skipped: libpipewire is installed here");
        return;
    }
    let nowhere = "unix:path=/nonexistent/freemix-no-bus";
    let inventory = fm_io_linux::wayland::inventory(Some(nowhere));
    assert!(!inventory.supported, "{inventory:?}");
    assert_eq!(inventory.error.as_deref(), Some(NO_PIPEWIRE));
    assert_eq!(
        fm_io_linux::screen_capture::session_from(
            None,
            Some("wayland-0"),
            None,
            pipewire_available
        ),
        fm_io_linux::screen_capture::Session::Unsupported(NO_PIPEWIRE)
    );
    let end = DesktopPortal::with_address(nowhere)
        .share(&request(SourceKind::Monitor, None), &mut Silent);
    assert_eq!(
        end,
        fm_io_linux::wayland::ShareEnd::Unsupported(NO_PIPEWIRE.into())
    );
}

/// A real share of a headless Wayland desktop's screen.
#[test]
fn a_real_desktop_screen_is_captured() {
    if std::env::var("FM_WAYLAND_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set FM_WAYLAND_E2E=1 on a Wayland desktop's session bus");
        return;
    }
    assert!(
        fm_io_linux::wayland::pipewire_available(),
        "libpipewire is not on the loader's path"
    );
    let inventory = fm_io_linux::wayland::inventory(None);
    assert!(inventory.picks_on_desktop(), "{inventory:?}");
    let token = live_picture(None).expect("the desktop keeps a restore token");
    // The grant's restore token shares the same screen again. (This
    // desktop sends a picture only once something on it changes, and
    // nothing does, so the second share is checked to stream.)
    share_again(token);
}

fn share_again(token: String) {
    use fm_io_linux::wayland::{CaptureSettings, DesktopPortal, PortalCapture};
    use fm_types::screen_capture::{ScreenCaptureCrop, ScreenCaptureState};

    let capture = PortalCapture::start(
        Box::new(DesktopPortal::new()),
        ShareRequest {
            kind: SourceKind::Monitor,
            cursor: false,
            restore_token: Some(token),
        },
        CaptureSettings {
            crop: ScreenCaptureCrop::NONE,
            fps: 30,
            width: 320,
            height: 180,
        },
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while capture.status().state != ScreenCaptureState::Live {
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            capture.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Captures the desktop's screen (with `token`, if any) until a picture
/// arrives, checks it, and returns the grant's restore token.
fn live_picture(token: Option<String>) -> Option<String> {
    use fm_io_linux::wayland::{CaptureSettings, DesktopPortal, PortalCapture};
    use fm_types::screen_capture::{ScreenCaptureCrop, ScreenCaptureState};

    let capture = PortalCapture::start(
        Box::new(DesktopPortal::new()),
        ShareRequest {
            kind: SourceKind::Monitor,
            cursor: true,
            restore_token: token,
        },
        CaptureSettings {
            // The crop applies to the shared picture: 40 px off the left.
            crop: ScreenCaptureCrop {
                left: 40,
                top: 0,
                right: 0,
                bottom: 0,
            },
            fps: 30,
            width: 320,
            height: 180,
        },
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let picture = loop {
        if let Some(picture) = capture.take_picture() {
            break picture;
        }
        let status = capture.status();
        assert!(
            std::time::Instant::now() < deadline,
            "no picture: {status:?}"
        );
        assert!(
            !matches!(
                status.state,
                ScreenCaptureState::Denied | ScreenCaptureState::Unsupported
            ),
            "{status:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let status = capture.status();
    assert_eq!(status.state, ScreenCaptureState::Live);
    let token = capture.take_restore_token().flatten();
    eprintln!(
        "shared {}x{}, restore token {token:?}",
        status.width, status.height
    );
    assert_eq!(picture.len(), 320 * 180 * 4);
    // The desktop's solid red background, in the middle of the picture.
    let middle = (90 * 320 + 160) * 4;
    let pixel = &picture[middle..middle + 4];
    assert!(
        pixel[0] > 200 && pixel[1] < 50 && pixel[2] < 50 && pixel[3] == 255,
        "{pixel:?}"
    );
    // 600x360 fitted into 320x180: 300x180 with 10 px bars at the sides.
    assert_eq!(
        &picture[(90 * 320 + 4) * 4..(90 * 320 + 5) * 4],
        &[0, 0, 0, 255]
    );
    token
}
