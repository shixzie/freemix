//! Screen and window capture value types (IN-011).
//!
//! A screen capture input shows a local display (a monitor of the server's
//! desktop) or one application window, captured on the server. Its
//! [`ScreenCaptureConfig`] names the [`ScreenCaptureTarget`], the
//! [`ScreenCaptureCrop`] trimmed from the target's edges, whether the mouse
//! cursor is drawn, a capture rate cap, and the slate shown while the
//! target is gone.
//!
//! A window is named by a [`WindowRule`] rather than by its window-system
//! id alone: the application (X11 class, macOS bundle or owner name,
//! Windows process name) and the title, matched exactly or as a substring,
//! plus the id it last had as a hint. A window that closes and opens again
//! under a new id is found again by the rule, and a window whose title
//! changes is kept while its id still lives.
//!
//! The runtime side reports each input's [`ScreenCaptureStatus`]: opening,
//! live, target gone (the window closed or the screen went away; the input
//! keeps its slate and recovers when the target reappears), reopening
//! after a capture failure, unsupported (this server cannot capture, for
//! example a Wayland session), or failed. The server lists what it can
//! capture as a [`ScreenCaptureInventory`] of screens and windows.

use std::fmt;

use crate::network_stream::StallSlate;

/// Most screen capture inputs one show holds.
pub const MAX_SCREEN_CAPTURE_INPUTS: usize = 8;
/// The longest application rule or listed application name, in bytes.
pub const MAX_SCREEN_CAPTURE_APP_BYTES: usize = 64;
/// The longest window title rule or listed title, in bytes.
pub const MAX_SCREEN_CAPTURE_TITLE_BYTES: usize = 128;
/// The longest listed screen name, in bytes.
pub const MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES: usize = 64;
/// The longest status or inventory error text, in bytes.
pub const MAX_SCREEN_CAPTURE_ERROR_BYTES: usize = 160;
/// The highest screen index a target names (zero-based).
pub const MAX_SCREEN_CAPTURE_SCREEN_INDEX: u16 = 63;
/// The largest crop inset from one edge, in target pixels.
pub const MAX_SCREEN_CAPTURE_CROP: u16 = 16_384;
/// The capture rate cap bounds, in frames per second.
pub const MIN_SCREEN_CAPTURE_FPS: u8 = 1;
pub const MAX_SCREEN_CAPTURE_FPS: u8 = 60;
/// The capture rate cap a new input starts with.
pub const DEFAULT_SCREEN_CAPTURE_FPS: u8 = 30;
/// Most screens one inventory lists.
pub const MAX_SCREEN_CAPTURE_SCREENS: usize = 16;
/// Most windows one inventory lists on the wire (the front-most ones).
pub const MAX_SCREEN_CAPTURE_WINDOWS: usize = 48;
/// Most windows a desktop listing keeps for matching capture targets: a
/// target deeper in the stacking order than the wire list reaches is still
/// found again.
pub const MAX_SCREEN_CAPTURE_MATCHED_WINDOWS: usize = 512;
/// The largest target side a capture accepts, in pixels.
pub const MAX_SCREEN_CAPTURE_SIDE: u32 = 16_384;

/// How a [`WindowRule`] matches a window's title.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TitleMatch {
    /// The title equals the rule's title.
    #[default]
    Exact,
    /// The title contains the rule's title (case-insensitively).
    Contains,
    /// Any title: only the application is matched.
    Any,
}

impl TitleMatch {
    pub const ALL: [Self; 3] = [Self::Exact, Self::Contains, Self::Any];

    /// The match's token: `exact`, `contains`, or `any`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Contains => "contains",
            Self::Any => "any",
        }
    }

    /// The match a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Which window a window capture follows.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct WindowRule {
    /// The application: the X11 `WM_CLASS` class (or instance), the macOS
    /// application name, or the Windows process name; matched without
    /// regard to ASCII case. Empty matches any application.
    pub app: String,
    /// The title text, matched as [`Self::title_match`] says.
    pub title: String,
    pub title_match: TitleMatch,
    /// The window-system id the window last had. A live window with this
    /// id whose application still matches is kept even when its title
    /// changed; otherwise the rule picks the first matching window.
    pub window_id: Option<u64>,
}

impl WindowRule {
    /// A rule that follows the window `app` titled exactly `title`, which
    /// now has `window_id`.
    #[must_use]
    pub fn exact(app: impl Into<String>, title: impl Into<String>, window_id: Option<u64>) -> Self {
        Self {
            app: app.into(),
            title: title.into(),
            title_match: TitleMatch::Exact,
            window_id,
        }
    }

    /// Whether a window of `app` titled `title` satisfies the rule. The
    /// window's texts are compared as a listing carries them (see
    /// [`bound_screen_capture_text`]), so a rule picked from a listing
    /// matches a desktop title longer than the wire carries or holding
    /// control characters.
    #[must_use]
    pub fn matches(&self, app: &str, title: &str) -> bool {
        self.matches_app(app)
            && match self.title_match {
                TitleMatch::Exact => {
                    bound_screen_capture_text(title, MAX_SCREEN_CAPTURE_TITLE_BYTES) == self.title
                }
                TitleMatch::Contains => bound_screen_capture_text(title, usize::MAX)
                    .to_lowercase()
                    .contains(&self.title.to_lowercase()),
                TitleMatch::Any => true,
            }
    }

    /// Whether a window of `app` satisfies the application part of the
    /// rule (the application compared as a listing carries it).
    #[must_use]
    pub fn matches_app(&self, app: &str) -> bool {
        self.app.is_empty()
            || bound_screen_capture_text(app, MAX_SCREEN_CAPTURE_APP_BYTES)
                .eq_ignore_ascii_case(&self.app)
    }
}

/// What a screen capture input captures.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ScreenCaptureTarget {
    /// The zero-based screen (monitor) `index` of the server's desktop.
    Screen { index: u16 },
    /// The window a rule follows.
    Window(WindowRule),
}

impl Default for ScreenCaptureTarget {
    fn default() -> Self {
        Self::Screen { index: 0 }
    }
}

impl ScreenCaptureTarget {
    /// The target's kind token: `screen` or `window`.
    #[must_use]
    pub const fn kind_token(&self) -> &'static str {
        match self {
            Self::Screen { .. } => "screen",
            Self::Window(_) => "window",
        }
    }
}

/// Pixels trimmed from each edge of the target before it is scaled to the
/// program picture. Insets follow the target's edges, so a crop that hides
/// a window's title bar keeps hiding it when the window is resized.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScreenCaptureCrop {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl ScreenCaptureCrop {
    /// No crop.
    pub const NONE: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    /// Whether the crop trims nothing.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }

    /// The region of a `width` x `height` target this crop keeps, as
    /// `(x, y, width, height)`, or `None` when it keeps less than 2x2
    /// pixels. Capture sizes are even (most encoders and scalers want
    /// them), so an odd kept side loses its last column or row.
    #[must_use]
    pub fn region(self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let kept_width = width
            .checked_sub(u32::from(self.left))?
            .checked_sub(u32::from(self.right))?
            & !1;
        let kept_height = height
            .checked_sub(u32::from(self.top))?
            .checked_sub(u32::from(self.bottom))?
            & !1;
        (kept_width >= 2 && kept_height >= 2).then_some((
            u32::from(self.left),
            u32::from(self.top),
            kept_width,
            kept_height,
        ))
    }
}

/// One screen capture input's configuration.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ScreenCaptureConfig {
    pub target: ScreenCaptureTarget,
    pub crop: ScreenCaptureCrop,
    /// Draw the mouse cursor into the picture.
    pub cursor: bool,
    /// The most frames per second captured
    /// ([`MIN_SCREEN_CAPTURE_FPS`]..=[`MAX_SCREEN_CAPTURE_FPS`]); the
    /// program rate also caps it.
    pub fps: u8,
    /// What the input shows while its target is gone or capture fails:
    /// the last picture or opaque black.
    pub slate: StallSlate,
}

impl Default for ScreenCaptureConfig {
    fn default() -> Self {
        Self {
            target: ScreenCaptureTarget::default(),
            crop: ScreenCaptureCrop::NONE,
            cursor: true,
            fps: DEFAULT_SCREEN_CAPTURE_FPS,
            slate: StallSlate::Freeze,
        }
    }
}

impl ScreenCaptureConfig {
    /// A capture of `target` with the default crop, cursor, rate, and
    /// slate.
    #[must_use]
    pub fn new(target: ScreenCaptureTarget) -> Self {
        Self {
            target,
            ..Self::default()
        }
    }

    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// Returns the first rule the configuration breaks.
    pub fn validate(&self) -> Result<(), ScreenCaptureError> {
        match &self.target {
            ScreenCaptureTarget::Screen { index } => {
                if *index > MAX_SCREEN_CAPTURE_SCREEN_INDEX {
                    return Err(ScreenCaptureError::InvalidScreen);
                }
            }
            ScreenCaptureTarget::Window(rule) => validate_window_rule(rule)?,
        }
        let crop = self.crop;
        if [crop.left, crop.top, crop.right, crop.bottom]
            .into_iter()
            .any(|inset| inset > MAX_SCREEN_CAPTURE_CROP)
        {
            return Err(ScreenCaptureError::InvalidCrop);
        }
        if !(MIN_SCREEN_CAPTURE_FPS..=MAX_SCREEN_CAPTURE_FPS).contains(&self.fps) {
            return Err(ScreenCaptureError::InvalidFps);
        }
        Ok(())
    }
}

/// Whether `text` is a usable rule or listed text of at most `max` bytes:
/// no control characters and no leading or trailing whitespace.
#[must_use]
pub fn is_screen_capture_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control) && text.trim() == text
}

/// Checks a window rule: bounded texts, an application or a title to match
/// (a rule matching any title names an application, a rule matching a
/// title names a non-empty title), and a non-zero id hint.
///
/// # Errors
///
/// Returns [`ScreenCaptureError::InvalidWindowRule`] when a rule breaks.
pub fn validate_window_rule(rule: &WindowRule) -> Result<(), ScreenCaptureError> {
    let texts = is_screen_capture_text(&rule.app, MAX_SCREEN_CAPTURE_APP_BYTES)
        && is_screen_capture_text(&rule.title, MAX_SCREEN_CAPTURE_TITLE_BYTES);
    let names_something = match rule.title_match {
        TitleMatch::Any => !rule.app.is_empty() && rule.title.is_empty(),
        TitleMatch::Exact | TitleMatch::Contains => !rule.title.is_empty(),
    };
    if texts && names_something && rule.window_id != Some(0) {
        Ok(())
    } else {
        Err(ScreenCaptureError::InvalidWindowRule)
    }
}

/// Why a screen capture configuration is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScreenCaptureError {
    /// The screen index is above [`MAX_SCREEN_CAPTURE_SCREEN_INDEX`].
    InvalidScreen,
    /// The window rule's texts are unusable or it matches nothing.
    InvalidWindowRule,
    /// A crop inset is above [`MAX_SCREEN_CAPTURE_CROP`].
    InvalidCrop,
    /// The rate cap is outside its bounds.
    InvalidFps,
}

impl ScreenCaptureError {
    /// The stable code of the error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidScreen => "invalid_screen",
            Self::InvalidWindowRule => "invalid_window_rule",
            Self::InvalidCrop => "invalid_crop",
            Self::InvalidFps => "invalid_fps",
        }
    }

    /// The field the error is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidScreen => "screen",
            Self::InvalidWindowRule => "title",
            Self::InvalidCrop => "crop",
            Self::InvalidFps => "fps",
        }
    }
}

impl fmt::Display for ScreenCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidScreen => "a screen capture names a screen from 0 to 63",
            Self::InvalidWindowRule => {
                "a window capture names a title (or an application to match any title) of \
                 bounded text without control characters or surrounding spaces"
            }
            Self::InvalidCrop => "a screen capture crop trims at most 16384 pixels from an edge",
            Self::InvalidFps => "a screen capture rate cap is 1 to 60 frames per second",
        })
    }
}

impl std::error::Error for ScreenCaptureError {}

/// A screen capture input's capture state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ScreenCaptureState {
    /// Nothing captures: the input is closed, or the daemon does not
    /// realize screen capture inputs.
    #[default]
    Idle,
    /// The target was found and its capture is starting.
    Opening,
    /// The target delivers pictures.
    Live,
    /// No window matches the rule, or the screen is gone: the input shows
    /// its slate and captures again when the target reappears.
    TargetGone,
    /// The capture stopped or failed to start: waiting `next_in_ms` before
    /// attempt `attempt` (one-based).
    Reopening { attempt: u32, next_in_ms: u32 },
    /// This server cannot capture its screens or windows (no capture
    /// backend, or a desktop session such as Wayland that refuses silent
    /// capture); the error says why.
    Unsupported,
    /// The capture cannot start here and is not retried.
    Failed,
}

impl ScreenCaptureState {
    /// The state's token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Opening => "opening",
            Self::Live => "live",
            Self::TargetGone => "target_gone",
            Self::Reopening { .. } => "reopening",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

/// A screen capture input's status.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScreenCaptureStatus {
    pub state: ScreenCaptureState,
    /// Capture restarts since the input opened (failures, resizes, and
    /// a target found again).
    pub reopens: u32,
    /// The captured target's size before the crop, in pixels; zero while
    /// no target is captured.
    pub width: u32,
    pub height: u32,
    /// The window-system id of the window captured, if a window is.
    pub window_id: Option<u64>,
    /// The last error, bounded to [`MAX_SCREEN_CAPTURE_ERROR_BYTES`].
    pub last_error: Option<String>,
}

/// `text` as one line of at most `max` bytes: control characters replaced
/// by spaces, trimmed, cut on a character boundary, and trimmed again.
#[must_use]
pub fn bound_screen_capture_text(text: &str, max: usize) -> String {
    let mut bounded: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if bounded.len() > max {
        let mut end = max;
        while !bounded.is_char_boundary(end) {
            end -= 1;
        }
        bounded.truncate(end);
        bounded.truncate(bounded.trim_end().len());
    }
    bounded
}

/// `text` as a status or inventory error.
#[must_use]
pub fn bound_screen_capture_error(text: &str) -> String {
    bound_screen_capture_text(text, MAX_SCREEN_CAPTURE_ERROR_BYTES)
}

/// One screen (monitor) of the server's desktop.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScreenInfo {
    /// The index a [`ScreenCaptureTarget::Screen`] names.
    pub index: u16,
    /// What the window system calls it (such as `DP-1`); may be empty.
    pub name: String,
    /// Its position on the desktop and its size, in pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl ScreenInfo {
    /// Whether the screen can travel as listed.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.index <= MAX_SCREEN_CAPTURE_SCREEN_INDEX
            && is_screen_capture_text(&self.name, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES)
            && (1..=MAX_SCREEN_CAPTURE_SIDE).contains(&self.width)
            && (1..=MAX_SCREEN_CAPTURE_SIDE).contains(&self.height)
    }
}

/// One top-level application window of the server's desktop.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct WindowInfo {
    /// The window-system id (non-zero).
    pub id: u64,
    /// The application (see [`WindowRule::app`]); may be empty.
    pub app: String,
    /// The title; may be empty.
    pub title: String,
    /// The owning process, when the window system says.
    pub pid: Option<u32>,
    /// Its position on the desktop and its size, in pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowInfo {
    /// Whether the window can travel as listed.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.id != 0
            && is_screen_capture_text(&self.app, MAX_SCREEN_CAPTURE_APP_BYTES)
            && is_screen_capture_text(&self.title, MAX_SCREEN_CAPTURE_TITLE_BYTES)
            && (1..=MAX_SCREEN_CAPTURE_SIDE).contains(&self.width)
            && (1..=MAX_SCREEN_CAPTURE_SIDE).contains(&self.height)
    }

    /// The rule that follows this window: its application and exact title,
    /// with its id as the hint. A window without a title is followed by its
    /// application and any title.
    #[must_use]
    pub fn rule(&self) -> WindowRule {
        if self.title.is_empty() {
            WindowRule {
                app: self.app.clone(),
                title: String::new(),
                title_match: TitleMatch::Any,
                window_id: Some(self.id),
            }
        } else {
            WindowRule::exact(self.app.clone(), self.title.clone(), Some(self.id))
        }
    }
}

/// What a server can capture (Protocol: `screen_capture_targets`).
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScreenCaptureInventory {
    /// The server captures screens or windows at all; when false nothing
    /// is listed and [`Self::error`] says why.
    pub supported: bool,
    /// The capture backend's token (such as `x11`, `screencapturekit`, or
    /// `gdigrab`); empty when unsupported.
    pub backend: String,
    /// The screens, ascending by index, each once.
    pub screens: Vec<ScreenInfo>,
    /// The windows, front to back as far as the window system says, each id
    /// once.
    pub windows: Vec<WindowInfo>,
    /// Why nothing (or not everything) could be listed, if anything went
    /// wrong.
    pub error: Option<String>,
}

impl ScreenCaptureInventory {
    /// The inventory of a server that cannot capture, and why.
    #[must_use]
    pub fn unsupported(reason: &str) -> Self {
        Self {
            supported: false,
            backend: String::new(),
            screens: Vec::new(),
            windows: Vec::new(),
            error: Some(bound_screen_capture_error(reason)).filter(|text| !text.is_empty()),
        }
    }

    /// The inventory bounded to what one record carries: texts bounded,
    /// unusable entries dropped, screens sorted and deduplicated by index,
    /// windows deduplicated by id (the first kept), and each list cut to
    /// its bound. An unsupported inventory lists nothing.
    #[must_use]
    pub fn fitted(&self) -> Self {
        self.bounded(MAX_SCREEN_CAPTURE_WINDOWS)
    }

    /// The inventory bounded as [`Self::fitted`] does, but keeping up to
    /// [`MAX_SCREEN_CAPTURE_MATCHED_WINDOWS`] windows: the listing capture
    /// targets are matched against. Fit it before it goes on the wire.
    #[must_use]
    pub fn fitted_for_matching(&self) -> Self {
        self.bounded(MAX_SCREEN_CAPTURE_MATCHED_WINDOWS)
    }

    fn bounded(&self, max_windows: usize) -> Self {
        let error = self
            .error
            .as_deref()
            .map(bound_screen_capture_error)
            .filter(|text| !text.is_empty());
        if !self.supported {
            return Self {
                error,
                ..Self::unsupported("")
            };
        }
        let mut screens: Vec<ScreenInfo> = self
            .screens
            .iter()
            .map(|screen| ScreenInfo {
                name: bound_screen_capture_text(&screen.name, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES),
                ..screen.clone()
            })
            .filter(ScreenInfo::is_valid)
            .collect();
        screens.sort_by_key(|screen| screen.index);
        screens.dedup_by_key(|screen| screen.index);
        screens.truncate(MAX_SCREEN_CAPTURE_SCREENS);
        let mut windows: Vec<WindowInfo> = Vec::new();
        for window in &self.windows {
            let window = WindowInfo {
                app: bound_screen_capture_text(&window.app, MAX_SCREEN_CAPTURE_APP_BYTES),
                title: bound_screen_capture_text(&window.title, MAX_SCREEN_CAPTURE_TITLE_BYTES),
                ..window.clone()
            };
            if window.is_valid() && windows.iter().all(|listed| listed.id != window.id) {
                windows.push(window);
            }
            if windows.len() == max_windows {
                break;
            }
        }
        Self {
            supported: true,
            backend: bound_screen_capture_text(&self.backend, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES),
            screens,
            windows,
            error,
        }
    }

    /// The inventory a desktop helper's text listing describes (the
    /// Windows and macOS backends), fitted to its bounds. Each line is one
    /// tab-separated record:
    ///
    /// - `S`, index, primary (`1`/`0`), x, y, width, height, name
    /// - `W`, id, pid (`0` when unknown), x, y, width, height, app, title
    ///
    /// Malformed lines are skipped; tabs inside a title are read as spaces.
    /// The windows are bounded for matching
    /// ([`Self::fitted_for_matching`]), not for the wire.
    #[must_use]
    pub fn from_records(listing: &str, backend: &str) -> Self {
        let mut screens = Vec::new();
        let mut windows = Vec::new();
        for line in listing.lines() {
            let fields: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
            match fields.as_slice() {
                ["S", index, primary, x, y, width, height, name] => {
                    let (Ok(index), Ok(x), Ok(y), Ok(width), Ok(height)) = (
                        index.parse(),
                        x.parse(),
                        y.parse(),
                        width.parse(),
                        height.parse(),
                    ) else {
                        continue;
                    };
                    screens.push(ScreenInfo {
                        index,
                        // `\\.\DISPLAY1` reads as `DISPLAY1`.
                        name: name.trim_start_matches(['\\', '.']).to_owned(),
                        x,
                        y,
                        width,
                        height,
                        primary: *primary == "1",
                    });
                }
                ["W", id, pid, x, y, width, height, app, title @ ..] => {
                    let (Ok(id), Ok(x), Ok(y), Ok(width), Ok(height)) = (
                        id.parse(),
                        x.parse(),
                        y.parse(),
                        width.parse(),
                        height.parse(),
                    ) else {
                        continue;
                    };
                    windows.push(WindowInfo {
                        id,
                        app: (*app).to_owned(),
                        title: title.join(" "),
                        pid: pid.parse().ok().filter(|pid| *pid != 0),
                        x,
                        y,
                        width,
                        height,
                    });
                }
                _ => {}
            }
        }
        Self {
            supported: true,
            backend: backend.to_owned(),
            screens,
            windows,
            error: None,
        }
        .fitted_for_matching()
    }

    /// The window `rule` follows: the window with the rule's id hint while
    /// it lives and its application matches, otherwise the first window the
    /// rule matches.
    #[must_use]
    pub fn resolve_window(&self, rule: &WindowRule) -> Option<&WindowInfo> {
        resolve_window(&self.windows, rule)
    }

    /// The screen `index`, if listed.
    #[must_use]
    pub fn screen(&self, index: u16) -> Option<&ScreenInfo> {
        self.screens.iter().find(|screen| screen.index == index)
    }
}

/// The window of `windows` that `rule` follows (see
/// [`ScreenCaptureInventory::resolve_window`]).
#[must_use]
pub fn resolve_window<'w>(windows: &'w [WindowInfo], rule: &WindowRule) -> Option<&'w WindowInfo> {
    rule.window_id
        .and_then(|id| {
            windows
                .iter()
                .find(|window| window.id == id && rule.matches_app(&window.app))
        })
        .or_else(|| {
            windows
                .iter()
                .find(|window| rule.matches(&window.app, &window.title))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, app: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            app: app.into(),
            title: title.into(),
            pid: None,
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        }
    }

    #[test]
    fn configs_validate_their_bounds() {
        assert_eq!(ScreenCaptureConfig::default().validate(), Ok(()));
        let screen = |index| ScreenCaptureConfig::new(ScreenCaptureTarget::Screen { index });
        assert_eq!(screen(63).validate(), Ok(()));
        assert_eq!(
            screen(64).validate(),
            Err(ScreenCaptureError::InvalidScreen)
        );
        let window = |rule: WindowRule| ScreenCaptureConfig::new(ScreenCaptureTarget::Window(rule));
        assert_eq!(
            window(WindowRule::exact("XTerm", "build", None)).validate(),
            Ok(())
        );
        for rule in [
            WindowRule::exact("XTerm", "", None),
            WindowRule::exact("XTerm", " lead", None),
            WindowRule::exact("XTerm", "tab\there", None),
            WindowRule::exact("XTerm", "t".repeat(161), None),
            WindowRule::exact("XTerm", "build", Some(0)),
            WindowRule {
                title_match: TitleMatch::Any,
                ..WindowRule::exact("", "", None)
            },
            WindowRule {
                title_match: TitleMatch::Any,
                ..WindowRule::exact("XTerm", "build", None)
            },
        ] {
            assert_eq!(
                window(rule.clone()).validate(),
                Err(ScreenCaptureError::InvalidWindowRule),
                "{rule:?}"
            );
        }
        let mut config = screen(0);
        config.crop.left = MAX_SCREEN_CAPTURE_CROP + 1;
        assert_eq!(config.validate(), Err(ScreenCaptureError::InvalidCrop));
        for fps in [0, 61] {
            let config = ScreenCaptureConfig { fps, ..screen(0) };
            assert_eq!(config.validate(), Err(ScreenCaptureError::InvalidFps));
        }
    }

    #[test]
    fn crops_follow_the_target_edges_and_keep_even_sizes() {
        let crop = ScreenCaptureCrop {
            left: 10,
            top: 30,
            right: 5,
            bottom: 0,
        };
        assert_eq!(crop.region(641, 481), Some((10, 30, 626, 450)));
        assert_eq!(crop.region(300, 200), Some((10, 30, 284, 170)));
        assert_eq!(crop.region(16, 32), None);
        assert_eq!(ScreenCaptureCrop::NONE.region(3, 3), Some((0, 0, 2, 2)));
    }

    #[test]
    fn rules_keep_a_living_window_and_find_a_reopened_one() {
        let windows = vec![
            window(5, "Firefox", "News - Mozilla Firefox"),
            window(7, "XTerm", "build"),
            window(9, "XTerm", "logs"),
        ];
        let rule = WindowRule::exact("xterm", "logs", Some(7));
        // The hinted window lives and its application matches: kept even
        // though its title no longer matches.
        assert_eq!(resolve_window(&windows, &rule).map(|w| w.id), Some(7));
        // The hinted window closed: the rule finds the matching one.
        let rule = WindowRule::exact("XTerm", "logs", Some(3));
        assert_eq!(resolve_window(&windows, &rule).map(|w| w.id), Some(9));
        // The hint names another application's window: ignored.
        let rule = WindowRule::exact("XTerm", "logs", Some(5));
        assert_eq!(resolve_window(&windows, &rule).map(|w| w.id), Some(9));
        let contains = WindowRule {
            title_match: TitleMatch::Contains,
            ..WindowRule::exact("", "mozilla", None)
        };
        assert_eq!(resolve_window(&windows, &contains).map(|w| w.id), Some(5));
        let any = WindowRule {
            title_match: TitleMatch::Any,
            ..WindowRule::exact("XTerm", "", None)
        };
        assert_eq!(resolve_window(&windows, &any).map(|w| w.id), Some(7));
        assert!(resolve_window(&windows, &WindowRule::exact("XTerm", "gone", None)).is_none());
        assert_eq!(
            windows[1].rule(),
            WindowRule::exact("XTerm", "build", Some(7))
        );
        assert_eq!(
            window(4, "Xmessage", "").rule().title_match,
            TitleMatch::Any
        );
    }

    #[test]
    fn a_rule_picked_from_a_fitted_listing_finds_its_reopened_window() {
        // The desktop's own titles can be longer than the wire carries, or
        // hold control characters; the operator picks from the fitted
        // listing, and the capture resolves against the desktop.
        let long = format!("{} - Mozilla Firefox", "Breaking news ".repeat(20));
        let desktop = vec![
            window(5, "Firefox", &long),
            window(8, "XTerm", "make\tbuild"),
        ];
        let fitted = ScreenCaptureInventory {
            supported: true,
            backend: "x11".into(),
            screens: Vec::new(),
            windows: desktop.clone(),
            error: None,
        }
        .fitted();
        for (index, picked) in fitted.windows.iter().enumerate() {
            let rule = picked.rule();
            assert!(validate_window_rule(&rule).is_ok());
            // The window closed and opened again under a new id.
            let reopened = vec![WindowInfo {
                id: 40 + picked.id,
                ..desktop[index].clone()
            }];
            assert_eq!(
                resolve_window(&reopened, &rule).map(|w| w.id),
                Some(40 + picked.id),
                "{rule:?}"
            );
        }
        let contains = WindowRule {
            title_match: TitleMatch::Contains,
            ..WindowRule::exact("XTerm", "make build", None)
        };
        assert_eq!(resolve_window(&desktop, &contains).map(|w| w.id), Some(8));
    }

    #[test]
    fn inventories_are_fitted_for_the_wire() {
        let inventory = ScreenCaptureInventory {
            supported: true,
            backend: "x11".into(),
            screens: vec![
                ScreenInfo {
                    index: 1,
                    name: "HDMI-1".into(),
                    width: 1280,
                    height: 720,
                    ..ScreenInfo::default()
                },
                ScreenInfo {
                    index: 0,
                    name: " DP-1\n".into(),
                    width: 1920,
                    height: 1080,
                    primary: true,
                    ..ScreenInfo::default()
                },
                ScreenInfo {
                    index: 2,
                    width: 0,
                    height: 0,
                    ..ScreenInfo::default()
                },
            ],
            windows: vec![
                window(7, "XTerm", " build\n"),
                window(7, "XTerm", "again"),
                window(0, "XTerm", "zero"),
                window(8, "XTerm", &"t".repeat(200)),
            ],
            error: Some("  ".into()),
        };
        let fitted = inventory.fitted();
        assert_eq!(
            fitted
                .screens
                .iter()
                .map(|screen| (screen.index, screen.name.as_str()))
                .collect::<Vec<_>>(),
            [(0, "DP-1"), (1, "HDMI-1")]
        );
        assert_eq!(fitted.windows.len(), 2);
        assert_eq!(fitted.windows[0].title, "build");
        assert_eq!(
            fitted.windows[1].title.len(),
            MAX_SCREEN_CAPTURE_TITLE_BYTES
        );
        assert_eq!(fitted.error, None);
        let unsupported = ScreenCaptureInventory::unsupported("Wayland session").fitted();
        assert!(!unsupported.supported);
        assert_eq!(unsupported.error.as_deref(), Some("Wayland session"));
        assert_eq!(unsupported.screens.len(), 0);
    }

    #[test]
    fn a_window_deep_in_the_stack_is_found_while_the_wire_list_stays_bounded() {
        let listing: String = (1..=200)
            .map(|id| format!("W\t{id}\t0\t0\t0\t640\t480\tApp\tWindow {id}\n"))
            .collect();
        let inventory = ScreenCaptureInventory::from_records(&listing, "gdigrab");
        let deep = WindowRule::exact("App", "Window 150", Some(150));
        assert_eq!(inventory.resolve_window(&deep).map(|w| w.id), Some(150));
        let renamed = WindowRule::exact("App", "Window 150", Some(9_999));
        assert_eq!(inventory.resolve_window(&renamed).map(|w| w.id), Some(150));
        assert_eq!(inventory.fitted().windows.len(), MAX_SCREEN_CAPTURE_WINDOWS);
        let huge: String = (1..=(MAX_SCREEN_CAPTURE_MATCHED_WINDOWS + 10))
            .map(|id| format!("W\t{id}\t0\t0\t0\t640\t480\tApp\tWindow {id}\n"))
            .collect();
        assert_eq!(
            ScreenCaptureInventory::from_records(&huge, "gdigrab")
                .windows
                .len(),
            MAX_SCREEN_CAPTURE_MATCHED_WINDOWS
        );
    }

    #[test]
    fn helper_records_parse_screens_and_windows_and_skip_malformed_lines() {
        let inventory = ScreenCaptureInventory::from_records(
            "S\t0\t1\t0\t0\t1920\t1080\t\\\\.\\DISPLAY1\r\n\
             S\t1\t0\t-1280\t0\t1280\t1024\tDisplay 2\n\
             S\tx\t0\t0\t0\t1\t1\tbroken\n\
             W\t66334\t4120\t-8\t-8\t1936\t1056\tPOWERPNT\tDeck\twith tab - PowerPoint\n\
             W\t0\t1\t0\t0\t10\t10\tbad\tzero id\n\
             W\t9\t0\t0\t0\t10\t10\tFinder\t\n\
             garbage\n",
            "gdigrab",
        );
        assert!(inventory.supported);
        assert_eq!(inventory.backend, "gdigrab");
        assert_eq!(inventory.screens.len(), 2);
        assert_eq!(inventory.screens[0].name, "DISPLAY1");
        assert!(inventory.screens[0].primary);
        assert_eq!(inventory.screens[1].x, -1_280);
        assert_eq!(inventory.windows.len(), 2);
        let window = &inventory.windows[0];
        assert_eq!(window.id, 66_334);
        assert_eq!(window.pid, Some(4_120));
        assert_eq!(window.app, "POWERPNT");
        assert_eq!(window.title, "Deck with tab - PowerPoint");
        assert_eq!((window.x, window.y), (-8, -8));
        assert_eq!(inventory.windows[1].pid, None);
    }
}
