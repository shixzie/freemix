//! Screen and window capture on the desktop the daemon runs on (IN-011).
//!
//! - [`session`]: which capture the desktop session allows: an X11
//!   display (`DISPLAY`) is captured silently; a Wayland session without
//!   `XWayland` refuses silent capture (it only offers capture through the
//!   xdg-desktop-portal's interactive `ScreenCast`, which a headless
//!   daemon cannot answer), and with `XWayland` only X11 windows can be
//!   captured.
//! - [`X11Lister`] (Linux only): the screens (`RandR` monitors, or the
//!   whole root window) and the viewable top-level application windows of
//!   an X11 display, with their class, title, process, and geometry,
//!   through the pure-Rust `x11rb` connection.
//! - [`resolve`]: the capture region of a configured target in a listing:
//!   the screen, or the window a [`WindowRule`] follows, cropped and
//!   clipped to the part of the desktop it occupies.
//! - [`x11grab_device`]: the `ffmpeg -f x11grab` input that captures a
//!   resolved region, letterboxed into the program picture.
//!
//! The listing and the resolution are portable, so the rules are tested on
//! every platform; only the X11 connection is Linux-only.

use fm_codec_ffmpeg::network_input::DeviceInput;
use fm_types::FrameRate;
use fm_types::network_stream::{GiveUp, NetworkStreamSettings, ReconnectPolicy};
use fm_types::screen_capture::{
    ScreenCaptureConfig, ScreenCaptureInventory, ScreenCaptureTarget, WindowInfo, WindowRule,
};

/// The backend token of an X11 desktop.
pub const X11_BACKEND: &str = "x11";

/// Why a Wayland session without `XWayland` cannot be captured.
pub const WAYLAND_UNSUPPORTED: &str = "Wayland session: silent screen and window capture is \
     unsupported; run the server in an X11 session or with XWayland";
/// What a Wayland session with `XWayland` can capture.
pub const XWAYLAND_NOTE: &str = "Wayland session: only XWayland application windows can be \
     captured; screens and native Wayland windows need an X11 session";
/// Why a server without a display cannot be captured.
pub const NO_DISPLAY: &str = "no desktop session: DISPLAY is not set for the server";

/// The desktop session the daemon can capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Session {
    /// An X11 display (`DISPLAY`); `wayland` when it is `XWayland` inside a
    /// Wayland session, where only X11 windows can be captured.
    X11 { display: String, wayland: bool },
    /// No capturable session, and why.
    Unsupported(&'static str),
}

/// The session `display` (`DISPLAY`), `wayland_display`
/// (`WAYLAND_DISPLAY`), and `session_type` (`XDG_SESSION_TYPE`) describe.
#[must_use]
pub fn session_from(
    display: Option<&str>,
    wayland_display: Option<&str>,
    session_type: Option<&str>,
) -> Session {
    let wayland = wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"));
    match display.filter(|value| !value.trim().is_empty()) {
        Some(display) => Session::X11 {
            display: display.to_owned(),
            wayland,
        },
        None if wayland => Session::Unsupported(WAYLAND_UNSUPPORTED),
        None => Session::Unsupported(NO_DISPLAY),
    }
}

/// The session of this process's environment.
#[must_use]
pub fn session() -> Session {
    let var = |name| std::env::var(name).ok();
    session_from(
        var("DISPLAY").as_deref(),
        var("WAYLAND_DISPLAY").as_deref(),
        var("XDG_SESSION_TYPE").as_deref(),
    )
}

/// One listing of a desktop: what it offers and, where only on-screen
/// pixels can be read (X11), the size of the whole desktop (the root
/// window) that window captures are clipped to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DesktopListing {
    pub inventory: ScreenCaptureInventory,
    pub desktop: Option<(u32, u32)>,
}

/// Where a resolved target is captured.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureRegion {
    /// The window captured, or `None` for a screen.
    pub window_id: Option<u64>,
    /// The screen captured, or `None` for a window.
    pub screen: Option<u16>,
    /// The target's position on the desktop.
    pub origin_x: i32,
    pub origin_y: i32,
    /// The target's whole size before the crop.
    pub target_width: u32,
    pub target_height: u32,
    /// The captured rectangle, in target coordinates. Its size is even.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// How a configured target resolves in a listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// The target is present and captured here.
    Found(CaptureRegion),
    /// The target is not present (no matching window, a missing screen, a
    /// minimized or off-screen window, or a crop that keeps nothing), and
    /// why.
    Gone(String),
    /// The target can never be captured here, and why.
    Unsupported(String),
}

/// The capture region of `config`'s target in `listing`.
#[must_use]
pub fn resolve(listing: &DesktopListing, config: &ScreenCaptureConfig) -> Resolution {
    let inventory = &listing.inventory;
    if !inventory.supported {
        return Resolution::Unsupported(
            inventory
                .error
                .clone()
                .unwrap_or_else(|| "screen capture is unsupported here".to_owned()),
        );
    }
    match &config.target {
        ScreenCaptureTarget::Screen { index } => {
            let Some(screen) = inventory.screen(*index) else {
                return if inventory.screens.is_empty() {
                    Resolution::Unsupported(
                        inventory
                            .error
                            .clone()
                            .unwrap_or_else(|| "no screen can be captured here".to_owned()),
                    )
                } else {
                    Resolution::Gone(format!("screen {} is not connected", index + 1))
                };
            };
            match config.crop.region(screen.width, screen.height) {
                Some((x, y, width, height)) => Resolution::Found(CaptureRegion {
                    window_id: None,
                    screen: Some(*index),
                    origin_x: screen.x,
                    origin_y: screen.y,
                    target_width: screen.width,
                    target_height: screen.height,
                    x,
                    y,
                    width,
                    height,
                }),
                None => Resolution::Gone("the crop keeps nothing of the screen".to_owned()),
            }
        }
        ScreenCaptureTarget::Window(rule) => match inventory.resolve_window(rule) {
            Some(window) => window_region(window, config, listing.desktop),
            None => Resolution::Gone(gone_text(rule)),
        },
    }
}

fn gone_text(rule: &WindowRule) -> String {
    match (rule.app.is_empty(), rule.title.is_empty()) {
        (false, true) => format!("no {} window is open", rule.app),
        (true, _) => format!("no window titled \"{}\" is open", rule.title),
        (false, false) => format!("no {} window titled \"{}\" is open", rule.app, rule.title),
    }
}

/// The cropped part of `window` that lies on the `desktop`, when captures
/// are clipped to it.
fn window_region(
    window: &WindowInfo,
    config: &ScreenCaptureConfig,
    desktop: Option<(u32, u32)>,
) -> Resolution {
    let Some((crop_x, crop_y, crop_width, crop_height)) =
        config.crop.region(window.width, window.height)
    else {
        return Resolution::Gone("the crop keeps nothing of the window".to_owned());
    };
    // The visible span of the window along one axis, in window
    // coordinates: X11 refuses to read pixels beyond the screen.
    let visible = |position: i32, size: u32, extent: u32| -> Option<(u32, u32)> {
        let position = i64::from(position);
        let start = (-position).max(0);
        let end = (i64::from(extent) - position).min(i64::from(size));
        if end <= start {
            return None;
        }
        Some((u32::try_from(start).ok()?, u32::try_from(end).ok()?))
    };
    let (Some((min_x, max_x)), Some((min_y, max_y))) = (match desktop {
        Some((desktop_width, desktop_height)) => (
            visible(window.x, window.width, desktop_width),
            visible(window.y, window.height, desktop_height),
        ),
        None => (Some((0, window.width)), Some((0, window.height))),
    }) else {
        return Resolution::Gone("the window is off the screen".to_owned());
    };
    let x = crop_x.max(min_x);
    let y = crop_y.max(min_y);
    let width = ((crop_x + crop_width).min(max_x).saturating_sub(x)) & !1;
    let height = ((crop_y + crop_height).min(max_y).saturating_sub(y)) & !1;
    if width < 2 || height < 2 {
        return Resolution::Gone("the window is off the screen".to_owned());
    }
    Resolution::Found(CaptureRegion {
        window_id: Some(window.id),
        screen: None,
        origin_x: window.x,
        origin_y: window.y,
        target_width: window.width,
        target_height: window.height,
        x,
        y,
        width,
        height,
    })
}

/// The capture rate `config` asks for, capped by the program rate (whole
/// frames per second, at least one).
#[must_use]
pub fn capture_rate(config: &ScreenCaptureConfig, program: FrameRate) -> u32 {
    let program = u64::from(program.numerator()).div_ceil(u64::from(program.denominator()).max(1));
    u32::try_from(program)
        .unwrap_or(u32::MAX)
        .min(u32::from(config.fps))
        .max(1)
}

/// The receive settings of a screen capture: a short buffer, a three-second
/// no-picture timeout, restarting every 0.5 to 3 s for good, and the
/// configured slate while it stalls.
#[must_use]
pub fn capture_settings(config: &ScreenCaptureConfig) -> NetworkStreamSettings {
    NetworkStreamSettings {
        jitter_buffer_ms: 80,
        reconnect: ReconnectPolicy {
            backoff_min_ms: 500,
            backoff_max_ms: 3_000,
            give_up: GiveUp::Never,
        },
        timeout_ms: 3_000,
        stall: config.slate,
        ..NetworkStreamSettings::default()
    }
}

/// The environment an X11 capture child keeps: the display it opens and
/// the authority that admits it.
pub const X11_ENVIRONMENT: [&str; 3] = ["DISPLAY", "XAUTHORITY", "HOME"];

/// The `ffmpeg -f x11grab` input capturing `region` of `display` at
/// `config`'s rate (capped by `program`) with or without the cursor,
/// letterboxed into the program picture.
#[must_use]
pub fn x11grab_device(
    display: &str,
    region: &CaptureRegion,
    config: &ScreenCaptureConfig,
    program: FrameRate,
) -> DeviceInput {
    let mut options = vec![
        (
            "framerate".to_owned(),
            capture_rate(config, program).to_string(),
        ),
        ("draw_mouse".to_owned(), u8::from(config.cursor).to_string()),
    ];
    // A window is grabbed in its own coordinates; a screen from the root
    // window, whose monitors never lie at negative positions.
    let (grab_x, grab_y) = match region.window_id {
        Some(id) => {
            options.push(("window_id".to_owned(), id.to_string()));
            (region.x, region.y)
        }
        None => (
            u32::try_from(region.origin_x).unwrap_or(0) + region.x,
            u32::try_from(region.origin_y).unwrap_or(0) + region.y,
        ),
    };
    options.extend([
        ("grab_x".to_owned(), grab_x.to_string()),
        ("grab_y".to_owned(), grab_y.to_string()),
        (
            "video_size".to_owned(),
            format!("{}x{}", region.width, region.height),
        ),
    ]);
    DeviceInput {
        format: "x11grab".to_owned(),
        options,
        path: display.to_owned(),
        field_order: None,
        audio: None,
        audio_only: false,
        letterbox: true,
        environment: X11_ENVIRONMENT
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        feeder: None,
    }
}

#[cfg(target_os = "linux")]
pub use x11::X11Lister;

#[cfg(target_os = "linux")]
mod x11 {
    //! The X11 desktop listing, through `x11rb`.

    use fm_types::screen_capture::{
        MAX_SCREEN_CAPTURE_MATCHED_WINDOWS, MAX_SCREEN_CAPTURE_SIDE, ScreenCaptureInventory,
        ScreenInfo, WindowInfo,
    };
    use x11rb::connection::Connection;
    use x11rb::errors::ReplyError;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window};
    use x11rb::rust_connection::RustConnection;

    use super::{DesktopListing, X11_BACKEND, XWAYLAND_NOTE};

    /// The most windows examined in one listing (beyond those listed).
    const MAX_EXAMINED: usize = 512;
    /// The longest property read, in 32-bit units.
    const PROPERTY_UNITS: u32 = 1024;

    struct Atoms {
        client_list_stacking: Atom,
        client_list: Atom,
        net_wm_name: Atom,
        utf8_string: Atom,
        net_wm_pid: Atom,
    }

    struct Connected {
        connection: RustConnection,
        root: Window,
        atoms: Atoms,
    }

    /// Lists the screens and windows of one X11 display, keeping its
    /// connection between listings and reconnecting after a failure.
    pub struct X11Lister {
        display: String,
        wayland: bool,
        connected: Option<Connected>,
    }

    impl X11Lister {
        /// A lister of `display`; `wayland` when it is `XWayland` inside a
        /// Wayland session (screens are then not offered).
        #[must_use]
        pub fn new(display: impl Into<String>, wayland: bool) -> Self {
            Self {
                display: display.into(),
                wayland,
                connected: None,
            }
        }

        /// The display this lister reads.
        #[must_use]
        pub fn display(&self) -> &str {
            &self.display
        }

        /// The display's screens and windows now. A display that cannot be
        /// reached lists nothing and says why.
        pub fn list(&mut self) -> DesktopListing {
            for _ in 0..2 {
                if self.connected.is_none() {
                    match connect(&self.display) {
                        Ok(connected) => self.connected = Some(connected),
                        Err(error) => return self.failed(&error),
                    }
                }
                let Some(connected) = &self.connected else {
                    continue;
                };
                match list(connected, self.wayland) {
                    Ok(listing) => return listing,
                    // A broken connection is opened again once.
                    Err(_) => self.connected = None,
                }
            }
            self.failed("the display connection failed")
        }

        fn failed(&self, error: &str) -> DesktopListing {
            DesktopListing {
                inventory: ScreenCaptureInventory {
                    supported: true,
                    backend: X11_BACKEND.to_owned(),
                    screens: Vec::new(),
                    windows: Vec::new(),
                    error: Some(format!("cannot open X11 display {}: {error}", self.display)),
                },
                desktop: None,
            }
        }
    }

    fn connect(display: &str) -> Result<Connected, String> {
        let (connection, screen) =
            x11rb::connect(Some(display)).map_err(|error| error.to_string())?;
        let root = connection
            .setup()
            .roots
            .get(screen)
            .ok_or("the display has no such screen")?
            .root;
        let intern = |name: &str| -> Result<Atom, String> {
            Ok(connection
                .intern_atom(false, name.as_bytes())
                .map_err(|error| error.to_string())?
                .reply()
                .map_err(|error| error.to_string())?
                .atom)
        };
        let atoms = Atoms {
            client_list_stacking: intern("_NET_CLIENT_LIST_STACKING")?,
            client_list: intern("_NET_CLIENT_LIST")?,
            net_wm_name: intern("_NET_WM_NAME")?,
            utf8_string: intern("UTF8_STRING")?,
            net_wm_pid: intern("_NET_WM_PID")?,
        };
        Ok(Connected {
            connection,
            root,
            atoms,
        })
    }

    type Failure = Box<dyn std::error::Error>;

    fn list(connected: &Connected, wayland: bool) -> Result<DesktopListing, Failure> {
        let connection = &connected.connection;
        let root = connection.get_geometry(connected.root)?.reply()?;
        let desktop = (u32::from(root.width), u32::from(root.height));
        let screens = if wayland {
            Vec::new()
        } else {
            screens(connected, desktop)
        };
        let mut windows = Vec::new();
        for window in clients(connected)? {
            if windows.len() == MAX_SCREEN_CAPTURE_MATCHED_WINDOWS {
                break;
            }
            if let Some(info) = window_info(connected, window)? {
                windows.push(info);
            }
        }
        Ok(DesktopListing {
            inventory: ScreenCaptureInventory {
                supported: true,
                backend: X11_BACKEND.to_owned(),
                screens,
                windows,
                error: wayland.then(|| XWAYLAND_NOTE.to_owned()),
            },
            desktop: Some(desktop),
        })
    }

    /// The `RandR` monitors, or the whole desktop as one screen.
    fn screens(connected: &Connected, desktop: (u32, u32)) -> Vec<ScreenInfo> {
        let connection = &connected.connection;
        let monitors = connection
            .randr_get_monitors(connected.root, true)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.monitors)
            .unwrap_or_default();
        let mut screens: Vec<ScreenInfo> = monitors
            .iter()
            .filter(|monitor| monitor.width > 0 && monitor.height > 0)
            .enumerate()
            .filter_map(|(index, monitor)| {
                let name = connection
                    .get_atom_name(monitor.name)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
                    .unwrap_or_default();
                Some(ScreenInfo {
                    index: u16::try_from(index).ok()?,
                    name,
                    x: i32::from(monitor.x),
                    y: i32::from(monitor.y),
                    width: u32::from(monitor.width),
                    height: u32::from(monitor.height),
                    primary: monitor.primary,
                })
            })
            .collect();
        if screens.is_empty() && desktop.0 > 0 && desktop.1 > 0 {
            screens.push(ScreenInfo {
                index: 0,
                name: "screen".to_owned(),
                x: 0,
                y: 0,
                width: desktop.0.min(MAX_SCREEN_CAPTURE_SIDE),
                height: desktop.1.min(MAX_SCREEN_CAPTURE_SIDE),
                primary: true,
            });
        }
        screens
    }

    /// The application windows, front to back: the window manager's
    /// stacking list when there is one, otherwise the root's viewable
    /// children (or the client inside a frame) that name an application.
    fn clients(connected: &Connected) -> Result<Vec<Window>, Failure> {
        let connection = &connected.connection;
        for atom in [
            connected.atoms.client_list_stacking,
            connected.atoms.client_list,
        ] {
            let reply = connection
                .get_property(
                    false,
                    connected.root,
                    atom,
                    AtomEnum::WINDOW,
                    0,
                    PROPERTY_UNITS,
                )?
                .reply()?;
            if let Some(list) = reply.value32() {
                let mut list: Vec<Window> = list.collect();
                if !list.is_empty() {
                    // Bottom to top in the property; front first here.
                    if atom == connected.atoms.client_list_stacking {
                        list.reverse();
                    }
                    list.truncate(MAX_EXAMINED);
                    return Ok(list);
                }
            }
        }
        let tree = connection.query_tree(connected.root)?.reply()?;
        let mut found = Vec::new();
        for &child in tree.children.iter().rev().take(MAX_EXAMINED) {
            // Each window may close while it is examined: it is skipped.
            let Some(attributes) = alive(connection.get_window_attributes(child)?.reply())? else {
                continue;
            };
            if attributes.override_redirect || attributes.map_state != MapState::VIEWABLE {
                continue;
            }
            if has_class(connected, child)? {
                found.push(child);
                continue;
            }
            // A frame of a reparenting window manager holds the client.
            let Some(inner) = alive(connection.query_tree(child)?.reply())? else {
                continue;
            };
            for &grandchild in inner.children.iter().rev() {
                if has_class(connected, grandchild)? {
                    found.push(grandchild);
                    break;
                }
            }
        }
        Ok(found)
    }

    /// A reply about one window, or `None` when the server refused the
    /// request (the window closed meanwhile); only a broken connection
    /// fails the listing.
    fn alive<T>(reply: Result<T, ReplyError>) -> Result<Option<T>, Failure> {
        match reply {
            Ok(reply) => Ok(Some(reply)),
            Err(ReplyError::X11Error(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// A window's property, or `None` when the window closed meanwhile.
    fn property(
        connected: &Connected,
        window: Window,
        atom: impl Into<Atom>,
        kind: impl Into<Atom>,
    ) -> Result<Option<Vec<u8>>, Failure> {
        Ok(alive(
            connected
                .connection
                .get_property(false, window, atom, kind, 0, PROPERTY_UNITS)?
                .reply(),
        )?
        .map(|reply| reply.value))
    }

    fn has_class(connected: &Connected, window: Window) -> Result<bool, Failure> {
        Ok(
            property(connected, window, AtomEnum::WM_CLASS, AtomEnum::STRING)?
                .is_some_and(|class| !class.is_empty()),
        )
    }

    /// A Latin-1 property text as UTF-8.
    fn latin1(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| char::from(*byte)).collect()
    }

    fn window_info(connected: &Connected, window: Window) -> Result<Option<WindowInfo>, Failure> {
        let connection = &connected.connection;
        let Ok(attributes) = connection.get_window_attributes(window)?.reply() else {
            // It closed while it was listed.
            return Ok(None);
        };
        if attributes.map_state != MapState::VIEWABLE {
            return Ok(None);
        }
        let Ok(geometry) = connection.get_geometry(window)?.reply() else {
            return Ok(None);
        };
        let Ok(origin) = connection
            .translate_coordinates(window, connected.root, 0, 0)?
            .reply()
        else {
            return Ok(None);
        };
        // From here on each property is `None` once the window closed.
        let Some(class) = property(connected, window, AtomEnum::WM_CLASS, AtomEnum::STRING)? else {
            return Ok(None);
        };
        // `WM_CLASS` is the instance and the class, each NUL-terminated.
        let mut parts = class
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty());
        let instance = parts.next().map(latin1).unwrap_or_default();
        let app = parts.next().map_or(instance, latin1);
        let Some(utf8_title) = property(
            connected,
            window,
            connected.atoms.net_wm_name,
            connected.atoms.utf8_string,
        )?
        else {
            return Ok(None);
        };
        let title = if utf8_title.is_empty() {
            let Some(name) = property(connected, window, AtomEnum::WM_NAME, AtomEnum::STRING)?
            else {
                return Ok(None);
            };
            latin1(&name)
        } else {
            String::from_utf8_lossy(&utf8_title).into_owned()
        };
        let Some(pid) = property(
            connected,
            window,
            connected.atoms.net_wm_pid,
            AtomEnum::CARDINAL,
        )?
        else {
            return Ok(None);
        };
        let pid = pid
            .get(..4)
            .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .filter(|pid| *pid != 0);
        Ok(Some(WindowInfo {
            id: u64::from(window),
            app,
            title,
            pid,
            x: i32::from(origin.dst_x),
            y: i32::from(origin.dst_y),
            width: u32::from(geometry.width),
            height: u32::from(geometry.height),
        }))
    }
}

#[cfg(test)]
mod tests {
    use fm_types::network_stream::StallSlate;
    use fm_types::screen_capture::{ScreenCaptureCrop, ScreenInfo, TitleMatch};

    use super::*;

    fn listing() -> DesktopListing {
        DesktopListing {
            inventory: ScreenCaptureInventory {
                supported: true,
                backend: X11_BACKEND.into(),
                screens: vec![
                    ScreenInfo {
                        index: 0,
                        name: "DP-1".into(),
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                        primary: true,
                    },
                    ScreenInfo {
                        index: 1,
                        name: "HDMI-1".into(),
                        x: 1920,
                        y: 0,
                        width: 1280,
                        height: 720,
                        primary: false,
                    },
                ],
                windows: vec![
                    WindowInfo {
                        id: 0x40_0007,
                        app: "XTerm".into(),
                        title: "build".into(),
                        pid: Some(42),
                        x: 100,
                        y: 50,
                        width: 641,
                        height: 401,
                    },
                    WindowInfo {
                        id: 0x40_0009,
                        app: "XTerm".into(),
                        title: "edge".into(),
                        pid: None,
                        x: -100,
                        y: 900,
                        width: 400,
                        height: 400,
                    },
                ],
                error: None,
            },
            desktop: Some((3200, 1080)),
        }
    }

    fn window(title: &str) -> ScreenCaptureConfig {
        ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
            "XTerm", title, None,
        )))
    }

    #[test]
    fn sessions_follow_the_environment() {
        assert_eq!(
            session_from(Some(":0"), None, Some("x11")),
            Session::X11 {
                display: ":0".into(),
                wayland: false
            }
        );
        assert_eq!(
            session_from(Some(":1"), Some("wayland-0"), None),
            Session::X11 {
                display: ":1".into(),
                wayland: true
            }
        );
        assert_eq!(
            session_from(None, None, Some("wayland")),
            Session::Unsupported(WAYLAND_UNSUPPORTED)
        );
        assert_eq!(
            session_from(Some(" "), None, None),
            Session::Unsupported(NO_DISPLAY)
        );
    }

    #[test]
    fn screens_resolve_to_their_cropped_desktop_area() {
        let mut config = ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 1 });
        config.crop = ScreenCaptureCrop {
            left: 10,
            top: 20,
            right: 0,
            bottom: 1,
        };
        assert_eq!(
            resolve(&listing(), &config),
            Resolution::Found(CaptureRegion {
                window_id: None,
                screen: Some(1),
                origin_x: 1920,
                origin_y: 0,
                target_width: 1280,
                target_height: 720,
                x: 10,
                y: 20,
                width: 1270,
                height: 698,
            })
        );
        config.target = ScreenCaptureTarget::Screen { index: 4 };
        assert!(matches!(resolve(&listing(), &config), Resolution::Gone(_)));
        let mut wayland = listing();
        wayland.inventory.screens.clear();
        wayland.inventory.error = Some(XWAYLAND_NOTE.into());
        assert_eq!(
            resolve(&wayland, &config),
            Resolution::Unsupported(XWAYLAND_NOTE.into())
        );
        let unsupported = DesktopListing {
            inventory: ScreenCaptureInventory::unsupported(WAYLAND_UNSUPPORTED),
            desktop: None,
        };
        assert_eq!(
            resolve(&unsupported, &window("build")),
            Resolution::Unsupported(WAYLAND_UNSUPPORTED.into())
        );
    }

    #[test]
    fn windows_resolve_by_rule_and_are_clipped_to_the_desktop() {
        assert_eq!(
            resolve(&listing(), &window("build")),
            Resolution::Found(CaptureRegion {
                window_id: Some(0x40_0007),
                screen: None,
                origin_x: 100,
                origin_y: 50,
                target_width: 641,
                target_height: 401,
                x: 0,
                y: 0,
                width: 640,
                height: 400,
            })
        );
        // Partly left of and below the desktop: only its visible part.
        assert_eq!(
            resolve(&listing(), &window("edge")),
            Resolution::Found(CaptureRegion {
                window_id: Some(0x40_0009),
                screen: None,
                origin_x: -100,
                origin_y: 900,
                target_width: 400,
                target_height: 400,
                x: 100,
                y: 0,
                width: 300,
                height: 180,
            })
        );
        assert_eq!(
            resolve(&listing(), &window("gone")),
            Resolution::Gone("no XTerm window titled \"gone\" is open".into())
        );
        let any = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule {
            title_match: TitleMatch::Any,
            ..WindowRule::exact("Firefox", "", None)
        }));
        assert_eq!(
            resolve(&listing(), &any),
            Resolution::Gone("no Firefox window is open".into())
        );
    }

    #[test]
    fn x11grab_inputs_name_the_region_rate_and_cursor() {
        let mut config = window("build");
        config.cursor = false;
        config.fps = 60;
        config.slate = StallSlate::Black;
        let Resolution::Found(region) = resolve(&listing(), &config) else {
            panic!("the window resolves");
        };
        let device = x11grab_device(
            ":99",
            &region,
            &config,
            FrameRate::new(30_000, 1_001).unwrap(),
        );
        assert_eq!(device.format, "x11grab");
        assert_eq!(device.path, ":99");
        assert!(device.letterbox);
        assert_eq!(
            device.options,
            [
                ("framerate".to_owned(), "30".to_owned()),
                ("draw_mouse".to_owned(), "0".to_owned()),
                ("window_id".to_owned(), "4194311".to_owned()),
                ("grab_x".to_owned(), "0".to_owned()),
                ("grab_y".to_owned(), "0".to_owned()),
                ("video_size".to_owned(), "640x400".to_owned()),
            ]
        );
        assert!(device.environment.iter().any(|name| name == "XAUTHORITY"));
        config.fps = 5;
        assert_eq!(capture_rate(&config, FrameRate::new(50, 1).unwrap()), 5);
        // A screen is grabbed from the root at its position.
        let screen = ScreenCaptureConfig {
            crop: ScreenCaptureCrop {
                left: 10,
                top: 20,
                right: 0,
                bottom: 0,
            },
            ..ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index: 1 })
        };
        let Resolution::Found(region) = resolve(&listing(), &screen) else {
            panic!("screen 1 resolves");
        };
        let device = x11grab_device(":0", &region, &screen, FrameRate::new(25, 1).unwrap());
        assert_eq!(
            device.options[2..],
            [
                ("grab_x".to_owned(), "1930".to_owned()),
                ("grab_y".to_owned(), "20".to_owned()),
                ("video_size".to_owned(), "1270x700".to_owned()),
            ]
        );
        // Without a desktop to clip to, a window is captured whole.
        let mut unclipped = listing();
        unclipped.desktop = None;
        let Resolution::Found(region) = resolve(&unclipped, &window("edge")) else {
            panic!("the edge window resolves");
        };
        assert_eq!(
            (region.x, region.y, region.width, region.height),
            (0, 0, 400, 400)
        );
    }
}
