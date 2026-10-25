//! The console's docking layout model: a binary tree of splits whose
//! leaves are tab groups of panels, plus the panels popped out into their
//! own native windows.
//!
//! The model is pure (no drawing): `workspace` lays it out and draws it.
//! Every panel appears at most once, either docked in the tree or popped
//! out; a panel in neither is hidden and is reopened from View › Panels.
//!
//! Layouts persist in `studio.conf` as compact text (see
//! [`DockLayout::encode_tree`] and [`DockLayout::encode_popouts`]); the
//! decoders accept exactly what the encoders write and reject anything
//! else, so a damaged entry falls back to the default layout.

use fm_ui_model::i18n::Msg;

/// One dockable console panel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PanelId {
    Preview,
    Transitions,
    Program,
    InputBank,
    Mixer,
    Outputs,
    Overlays,
    Stingers,
    Titles,
    Playlist,
    Media,
    /// The operator cue list and its GO button (RC-017).
    Cues,
    /// Instant replay: recording, events, and Replay inputs (OR-012).
    Replay,
}

impl PanelId {
    /// Every panel, in View › Panels order.
    pub const ALL: [Self; 13] = [
        Self::Preview,
        Self::Transitions,
        Self::Program,
        Self::InputBank,
        Self::Mixer,
        Self::Outputs,
        Self::Overlays,
        Self::Stingers,
        Self::Titles,
        Self::Playlist,
        Self::Media,
        Self::Cues,
        Self::Replay,
    ];

    /// The panel's name in `studio.conf`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Transitions => "transitions",
            Self::Program => "program",
            Self::InputBank => "inputs",
            Self::Mixer => "mixer",
            Self::Outputs => "outputs",
            Self::Overlays => "overlays",
            Self::Stingers => "stingers",
            Self::Titles => "titles",
            Self::Playlist => "playlist",
            Self::Media => "media",
            Self::Cues => "cues",
            Self::Replay => "replay",
        }
    }

    /// The panel named `key` in `studio.conf`.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|panel| panel.key() == key)
    }

    /// The panel's title on its tab and in View › Panels.
    pub(crate) const fn title(self) -> Msg {
        match self {
            Self::Preview => Msg::PanelPreview,
            Self::Transitions => Msg::ShortcutGroupTransitions,
            Self::Program => Msg::PanelProgram,
            Self::InputBank => Msg::InputBank,
            Self::Mixer => Msg::ViewAudioMixer,
            Self::Outputs => Msg::PanelOutputs,
            Self::Overlays => Msg::TabOverlays,
            Self::Stingers => Msg::TabStingers,
            Self::Titles => Msg::TabTitles,
            Self::Playlist => Msg::TabPlaylist,
            Self::Media => Msg::TabMedia,
            Self::Cues => Msg::TabCues,
            Self::Replay => Msg::TabReplay,
        }
    }

    /// What the panel holds, for its hover help.
    pub(crate) const fn hover(self) -> Msg {
        match self {
            Self::Preview => Msg::PanelPreviewHover,
            Self::Transitions => Msg::PanelTransitionsHover,
            Self::Program => Msg::PanelProgramHover,
            Self::InputBank => Msg::ViewInputBankHover,
            Self::Mixer => Msg::ViewAudioMixerHover,
            Self::Outputs => Msg::PanelOutputsHover,
            Self::Overlays => Msg::TabOverlaysHover,
            Self::Stingers => Msg::TabStingersHover,
            Self::Titles => Msg::TabTitlesHover,
            Self::Playlist => Msg::TabPlaylistHover,
            Self::Media => Msg::TabMediaHover,
            Self::Cues => Msg::TabCuesHover,
            Self::Replay => Msg::TabReplayHover,
        }
    }

    /// The smallest body, in points, at which the panel's content is laid
    /// out without scrolling; a smaller tile scrolls its body instead.
    #[must_use]
    pub const fn min_size(self) -> (f32, f32) {
        match self {
            Self::Preview | Self::Program => (240.0, 160.0),
            // The transition column's narrowest plus the gutter for its
            // scroll bar; it widens with its tile up to its widest.
            Self::Transitions => (212.0, 300.0),
            Self::InputBank => (340.0, 220.0),
            Self::Mixer => (220.0, 300.0),
            Self::Outputs => (300.0, 150.0),
            Self::Overlays
            | Self::Stingers
            | Self::Titles
            | Self::Playlist
            | Self::Media
            | Self::Cues
            | Self::Replay => (300.0, 220.0),
        }
    }

    /// Where a reopened panel goes: beside the first of these panels that
    /// is docked, else along this edge of the whole layout.
    const fn home(self) -> (&'static [(Self, DropZone)], DropZone) {
        const SIDE: [(PanelId, DropZone); 7] = [
            (PanelId::Overlays, DropZone::Center),
            (PanelId::Stingers, DropZone::Center),
            (PanelId::Titles, DropZone::Center),
            (PanelId::Playlist, DropZone::Center),
            (PanelId::Media, DropZone::Center),
            (PanelId::Cues, DropZone::Center),
            (PanelId::Outputs, DropZone::Bottom),
        ];
        match self {
            Self::Preview => (
                &[
                    (Self::Transitions, DropZone::Left),
                    (Self::Program, DropZone::Left),
                ],
                DropZone::Top,
            ),
            Self::Transitions => (
                &[
                    (Self::Preview, DropZone::Right),
                    (Self::Program, DropZone::Left),
                ],
                DropZone::Top,
            ),
            Self::Program => (
                &[
                    (Self::Transitions, DropZone::Right),
                    (Self::Preview, DropZone::Right),
                ],
                DropZone::Top,
            ),
            Self::InputBank => (&[(Self::Mixer, DropZone::Left)], DropZone::Bottom),
            Self::Mixer => (&[(Self::InputBank, DropZone::Right)], DropZone::Bottom),
            Self::Outputs => (
                &[
                    (Self::Overlays, DropZone::Top),
                    (Self::Stingers, DropZone::Top),
                    (Self::Titles, DropZone::Top),
                    (Self::Playlist, DropZone::Top),
                    (Self::Media, DropZone::Top),
                    (Self::Cues, DropZone::Top),
                    (Self::Replay, DropZone::Top),
                ],
                DropZone::Right,
            ),
            Self::Overlays
            | Self::Stingers
            | Self::Titles
            | Self::Playlist
            | Self::Media
            | Self::Cues => (&SIDE, DropZone::Right),
            // Replay shares the default layout's tab group with the outputs
            // it feeds.
            Self::Replay => (&[(Self::Outputs, DropZone::Center)], DropZone::Right),
        }
    }
}

/// How a split lays its two children out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    /// Side by side: the first child on the left.
    Horizontal,
    /// Stacked: the first child on top.
    Vertical,
}

/// Where a dragged panel lands relative to a tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
    /// Into the tile's tab group.
    Center,
}

/// One node of the dock tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DockNode {
    /// A tab group; `active` indexes the shown panel.
    Tabs { panels: Vec<PanelId>, active: usize },
    /// Two children split along `axis`; the first takes `ratio` per mille
    /// of the extent (before minimum sizes are applied).
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<DockNode>,
        second: Box<DockNode>,
    },
}

/// Which child of a split: a path from the root is a list of these.
pub type NodePath = Vec<bool>;

impl DockNode {
    fn tabs(panels: &[PanelId]) -> Self {
        Self::Tabs {
            panels: panels.to_vec(),
            active: 0,
        }
    }

    fn split(axis: Axis, ratio: u16, first: Self, second: Self) -> Self {
        Self::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn contains(&self, panel: PanelId) -> bool {
        match self {
            Self::Tabs { panels, .. } => panels.contains(&panel),
            Self::Split { first, second, .. } => first.contains(panel) || second.contains(panel),
        }
    }

    fn collect(&self, out: &mut Vec<PanelId>) {
        match self {
            Self::Tabs { panels, .. } => out.extend_from_slice(panels),
            Self::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// The tab group holding `panel`.
    fn group_mut(&mut self, panel: PanelId) -> Option<&mut Self> {
        match self {
            Self::Tabs { panels, .. } if panels.contains(&panel) => Some(self),
            Self::Tabs { .. } => None,
            Self::Split { first, second, .. } => first
                .group_mut(panel)
                .or_else(move || second.group_mut(panel)),
        }
    }

    /// Removes `panel`; returns the remaining node, or `None` when nothing
    /// is left. Emptied groups disappear and their split collapses into
    /// the sibling.
    fn without(self, panel: PanelId) -> Option<Self> {
        match self {
            Self::Tabs { mut panels, active } => {
                let Some(index) = panels.iter().position(|p| *p == panel) else {
                    return Some(Self::Tabs { panels, active });
                };
                panels.remove(index);
                if panels.is_empty() {
                    return None;
                }
                // Removing the shown tab shows its left neighbour (the
                // first tab when it was first); later tabs keep theirs.
                let active = if active >= index {
                    active.saturating_sub(1)
                } else {
                    active
                };
                Some(Self::Tabs { panels, active })
            }
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without(panel), second.without(panel)) {
                (Some(first), Some(second)) => Some(Self::split(axis, ratio, first, second)),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    fn node_mut(&mut self, path: &[bool]) -> Option<&mut Self> {
        match path.split_first() {
            None => Some(self),
            Some((second_child, rest)) => match self {
                Self::Split { first, second, .. } => {
                    if *second_child { second } else { first }.node_mut(rest)
                }
                Self::Tabs { .. } => None,
            },
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Tabs { .. } => 1,
            Self::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Tabs { panels, active } => !panels.is_empty() && *active < panels.len(),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => (MIN_RATIO..=MAX_RATIO).contains(ratio) && first.valid() && second.valid(),
        }
    }
}

/// The narrowest share a split gives its first child, per mille.
pub const MIN_RATIO: u16 = 50;
/// The widest share a split gives its first child, per mille.
pub const MAX_RATIO: u16 = 950;
/// The deepest tree accepted from `studio.conf`.
const MAX_DEPTH: usize = 16;

/// A popped-out window's last known place, in points: the position is
/// unknown on platforms that do not report it (Wayland). A maximised
/// window keeps the size and place it returns to when un-maximised.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowGeometry {
    pub size: (u16, u16),
    pub position: Option<(i32, i32)>,
    /// Whether the window was maximised; it opens maximised again.
    pub maximized: bool,
}

/// One monitor's area on the desktop, in whole points (the units of
/// egui's window positions).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl ScreenRect {
    /// A monitor's area from its physical position and size and its scale
    /// factor, at the console's `zoom` (the UI scale). A window on that
    /// monitor reports its place divided by the same factor, so the two
    /// compare in the same units. `None` for an empty monitor or an
    /// unusable scale.
    #[must_use]
    pub fn from_physical(
        position: (i32, i32),
        size: (u32, u32),
        scale_factor: f64,
        zoom: f32,
    ) -> Option<Self> {
        #[allow(clippy::cast_possible_truncation)] // scale factors are small
        let pixels_per_point = scale_factor as f32 * zoom;
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || size.0 == 0 || size.1 == 0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // desktop pixels
        let points = |pixels: f64| (pixels as f32 / pixels_per_point).round() as i32;
        Some(Self {
            x: points(f64::from(position.0)),
            y: points(f64::from(position.1)),
            width: points(f64::from(size.0)),
            height: points(f64::from(size.1)),
        })
    }

    /// This monitor as a left, top, width, and height.
    #[allow(clippy::cast_precision_loss)] // desktop points
    fn points(self) -> (f32, f32, f32, f32) {
        (
            self.x as f32,
            self.y as f32,
            self.width as f32,
            self.height as f32,
        )
    }

    /// The overlap of this monitor with a rect at `x`, `y` of `width` by
    /// `height`, as a width and height (zero when apart).
    fn overlap(self, x: f32, y: f32, width: f32, height: f32) -> (f32, f32) {
        let (left, top, across, down) = self.points();
        let across = (left + across).min(x + width) - left.max(x);
        let down = (top + down).min(y + height) - top.max(y);
        (across.max(0.0), down.max(0.0))
    }
}

impl WindowGeometry {
    /// Smallest kept window side, in points.
    pub const MIN_SIDE: u16 = 120;
    /// Largest kept window side, in points.
    pub const MAX_SIDE: u16 = 8_000;
    /// Furthest kept window position from the origin, in points.
    pub const MAX_OFFSET: i32 = 32_000;
    /// The strip along a window's top, in points, that must show on some
    /// monitor for the operator to grab the window there.
    pub const TITLE_STRIP: f32 = 64.0;
    /// How much of [`Self::TITLE_STRIP`] must show on one monitor, across
    /// and down, for the window to stay where it was saved.
    pub const MIN_VISIBLE: (f32, f32) = (64.0, 32.0);

    /// A geometry from a window's inner size and outer position, clamped
    /// to the kept ranges. An unmeasurable (NaN) side is the smallest kept,
    /// and a position with an unmeasurable coordinate is unknown.
    #[must_use]
    pub fn from_points(size: (f32, f32), position: Option<(f32, f32)>) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
        let clamp_side = |value: f32| {
            if value.is_nan() {
                return Self::MIN_SIDE;
            }
            value
                .round()
                .clamp(f32::from(Self::MIN_SIDE), f32::from(Self::MAX_SIDE)) as u16
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // clamped first
        let offset = |value: f32| {
            value
                .round()
                .clamp(-(Self::MAX_OFFSET as f32), Self::MAX_OFFSET as f32) as i32
        };
        Self {
            size: (clamp_side(size.0), clamp_side(size.1)),
            position: position
                .filter(|(x, y)| !x.is_nan() && !y.is_nan())
                .map(|(x, y)| (offset(x), offset(y))),
            maximized: false,
        }
    }

    /// This geometry, maximised or not.
    #[must_use]
    pub const fn with_maximized(self, maximized: bool) -> Self {
        Self { maximized, ..self }
    }

    /// Where the window should open on the current `monitors`, the
    /// primary monitor first. A window whose title strip no longer shows
    /// on any monitor (one was unplugged, or the desktop was rearranged)
    /// moves to the middle of the primary monitor, keeping its size but
    /// shrinking to fit. A window without a known position only shrinks
    /// to fit the primary monitor. No monitors leaves it as it is.
    #[must_use]
    pub fn onto_screens(self, monitors: &[ScreenRect]) -> Self {
        let Some(primary) = monitors.first().map(|primary| primary.points()) else {
            return self;
        };
        let (left, top, room_across, room_down) = primary;
        let (width, height) = (f32::from(self.size.0), f32::from(self.size.1));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped first
        let fit = |side: f32, room: f32| {
            side.min(room.floor())
                .clamp(f32::from(Self::MIN_SIDE), f32::from(Self::MAX_SIDE)) as u16
        };
        let size = (fit(width, room_across), fit(height, room_down));
        let Some((x, y)) = self.position else {
            return Self { size, ..self };
        };
        #[allow(clippy::cast_precision_loss)] // clamped screen points
        let (x, y) = (x as f32, y as f32);
        let (min_across, min_down) = Self::MIN_VISIBLE;
        let strip = Self::TITLE_STRIP.min(height);
        let visible = monitors.iter().any(|monitor| {
            let (across, down) = monitor.overlap(x, y, width, strip);
            across >= min_across.min(width) && down >= min_down.min(strip)
        });
        if visible {
            return self;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // clamped first
        let centre = |start: f32, room: f32, side: u16| {
            (start + ((room - f32::from(side)) / 2.0).max(0.0))
                .round()
                .clamp(-(Self::MAX_OFFSET as f32), Self::MAX_OFFSET as f32) as i32
        };
        Self {
            size,
            position: Some((
                centre(left, room_across, size.0),
                centre(top, room_down, size.1),
            )),
            maximized: self.maximized,
        }
    }

    fn encode(self) -> String {
        let (width, height) = self.size;
        let maximized = if self.maximized { MAXIMIZED_SUFFIX } else { "" };
        match self.position {
            Some((x, y)) => format!("{width}x{height}@{x},{y}{maximized}"),
            None => format!("{width}x{height}{maximized}"),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let (text, maximized) = match text.strip_suffix(MAXIMIZED_SUFFIX) {
            Some(text) => (text, true),
            None => (text, false),
        };
        let (size, position) = match text.split_once('@') {
            Some((size, position)) => (size, Some(position)),
            None => (text, None),
        };
        let parse_side = |text: &str| {
            text.parse::<u16>()
                .ok()
                .filter(|side| (Self::MIN_SIDE..=Self::MAX_SIDE).contains(side))
        };
        let offset = |text: &str| {
            text.parse::<i32>()
                .ok()
                .filter(|offset| offset.abs() <= Self::MAX_OFFSET)
        };
        let (width, height) = size.split_once('x')?;
        let position = match position {
            Some(position) => {
                let (x, y) = position.split_once(',')?;
                Some((offset(x)?, offset(y)?))
            }
            None => None,
        };
        Some(Self {
            size: (parse_side(width)?, parse_side(height)?),
            position,
            maximized,
        })
    }
}

/// Marks a maximised window's geometry in `studio.conf`.
const MAXIMIZED_SUFFIX: &str = "+max";

/// Something shown in its own native window, and where.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PopOut<K> {
    pub key: K,
    /// `None` until the window has been placed once.
    pub geometry: Option<WindowGeometry>,
}

/// Keys of things that can be popped out, named in `studio.conf`.
pub trait PopOutKey: Copy + Eq + Sized + 'static {
    /// Every key.
    fn all() -> &'static [Self];
    /// The key's name in `studio.conf`.
    fn name(self) -> &'static str;
}

impl PopOutKey for PanelId {
    fn all() -> &'static [Self] {
        &Self::ALL
    }

    fn name(self) -> &'static str {
        self.key()
    }
}

/// A tool window that can float over the console or have its own window.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ToolWindow {
    Color,
    Animation,
    Designer,
}

impl ToolWindow {
    /// Every tool window, in View menu order.
    pub const ALL: [Self; 3] = [Self::Color, Self::Animation, Self::Designer];

    /// The tool's name in `studio.conf`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Animation => "animation",
            Self::Designer => "designer",
        }
    }

    pub(crate) const fn title(self) -> Msg {
        match self {
            Self::Color => Msg::GradeTitle,
            Self::Animation => Msg::AnimSection,
            Self::Designer => Msg::DesignTitle,
        }
    }

    pub(crate) const fn hover(self) -> Msg {
        match self {
            Self::Color => Msg::ToolColorHover,
            Self::Animation => Msg::ToolAnimationHover,
            Self::Designer => Msg::ToolDesignerHover,
        }
    }
}

impl PopOutKey for ToolWindow {
    fn all() -> &'static [Self] {
        &Self::ALL
    }

    fn name(self) -> &'static str {
        self.key()
    }
}

/// Encodes a pop-out list: `none`, or `key[:WxH[@X,Y]]` entries joined
/// by `; `.
#[must_use]
pub fn encode_popouts<K: PopOutKey>(popouts: &[PopOut<K>]) -> String {
    if popouts.is_empty() {
        return "none".to_owned();
    }
    popouts
        .iter()
        .map(|popout| match popout.geometry {
            Some(geometry) => format!("{}:{}", popout.key.name(), geometry.encode()),
            None => popout.key.name().to_owned(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Decodes what [`encode_popouts`] writes; `None` for anything else,
/// including a key listed twice.
#[must_use]
pub fn decode_popouts<K: PopOutKey>(text: &str) -> Option<Vec<PopOut<K>>> {
    let text = text.trim();
    if text == "none" {
        return Some(Vec::new());
    }
    let mut popouts: Vec<PopOut<K>> = Vec::new();
    for entry in text.split(';') {
        let entry = entry.trim();
        let (name, geometry) = match entry.split_once(':') {
            Some((name, geometry)) => (name, Some(WindowGeometry::decode(geometry)?)),
            None => (entry, None),
        };
        let key = K::all().iter().copied().find(|key| key.name() == name)?;
        if popouts.iter().any(|popout| popout.key == key) {
            return None;
        }
        popouts.push(PopOut { key, geometry });
    }
    Some(popouts)
}

/// The Default layout's share for its upper row, in both columns.
const DEFAULT_ROW_SPLIT: u16 = 500;

/// A built-in layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutPreset {
    /// Monitors and transitions on top, the bank and mixer below, outputs
    /// and the side tabs on the right.
    Default,
    /// Fewer, larger tiles for small screens: the bank and mixer share a
    /// tab group, as do outputs and the side tabs.
    Compact,
    /// A wide, tall mixer beside one tab group for everything else below
    /// the monitors.
    AudioFocus,
}

impl LayoutPreset {
    /// Every built-in layout, in View › Layouts order.
    pub const ALL: [Self; 3] = [Self::Default, Self::Compact, Self::AudioFocus];

    pub(crate) const fn title(self) -> Msg {
        match self {
            Self::Default => Msg::LayoutDefault,
            Self::Compact => Msg::LayoutCompact,
            Self::AudioFocus => Msg::LayoutAudioFocus,
        }
    }

    pub(crate) const fn hover(self) -> Msg {
        match self {
            Self::Default => Msg::LayoutDefaultHover,
            Self::Compact => Msg::LayoutCompactHover,
            Self::AudioFocus => Msg::LayoutAudioFocusHover,
        }
    }

    /// The layout's tree.
    #[must_use]
    pub fn layout(self) -> DockLayout {
        use PanelId::{
            Cues, InputBank, Media, Mixer, Outputs, Overlays, Playlist, Preview, Program, Replay,
            Stingers, Titles, Transitions,
        };
        // Shares chosen so Preview and Program stay the same width from
        // 1280 to 1920 points wide, where the transition column goes from
        // its minimum to nearly its widest.
        let monitors = || {
            DockNode::split(
                Axis::Horizontal,
                375,
                DockNode::tabs(&[Preview]),
                DockNode::split(
                    Axis::Horizontal,
                    394,
                    DockNode::tabs(&[Transitions]),
                    DockNode::tabs(&[Program]),
                ),
            )
        };
        let side = [Overlays, Stingers, Titles, Playlist, Media, Cues];
        let root = match self {
            // Both columns split their height at one share, so the lower
            // row's tab bars line up across the console.
            Self::Default => DockNode::split(
                Axis::Horizontal,
                765,
                DockNode::split(
                    Axis::Vertical,
                    DEFAULT_ROW_SPLIT,
                    monitors(),
                    DockNode::split(
                        Axis::Horizontal,
                        600,
                        DockNode::tabs(&[InputBank]),
                        DockNode::tabs(&[Mixer]),
                    ),
                ),
                DockNode::split(
                    Axis::Vertical,
                    DEFAULT_ROW_SPLIT,
                    // Replay sits beside the outputs it feeds, so the side
                    // group still shows every tab whole at 1920 wide.
                    DockNode::tabs(&[Outputs, Replay]),
                    DockNode::tabs(&side),
                ),
            ),
            Self::Compact => DockNode::split(
                Axis::Vertical,
                560,
                monitors(),
                DockNode::split(
                    Axis::Horizontal,
                    620,
                    DockNode::tabs(&[InputBank, Mixer]),
                    DockNode::tabs(&[
                        Outputs, Overlays, Stingers, Titles, Playlist, Media, Cues, Replay,
                    ]),
                ),
            ),
            Self::AudioFocus => DockNode::split(
                Axis::Vertical,
                430,
                monitors(),
                DockNode::split(
                    Axis::Horizontal,
                    380,
                    DockNode::tabs(&[
                        InputBank, Outputs, Overlays, Stingers, Titles, Playlist, Media, Cues,
                        Replay,
                    ]),
                    DockNode::tabs(&[Mixer]),
                ),
            ),
        };
        DockLayout {
            root: Some(root),
            popped: Vec::new(),
        }
    }
}

/// Why a persisted layout could not be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DockError {
    /// The text is not a layout this version writes.
    Malformed,
    /// A panel appears twice (docked twice, or docked and popped out).
    Duplicate,
}

/// The console's docked panels and popped-out windows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockLayout {
    root: Option<DockNode>,
    popped: Vec<PopOut<PanelId>>,
}

impl Default for DockLayout {
    fn default() -> Self {
        LayoutPreset::Default.layout()
    }
}

impl DockLayout {
    /// A layout from parts, validated.
    ///
    /// # Errors
    ///
    /// Returns [`DockError`] for an invalid tree or a panel shown twice.
    pub fn from_parts(
        root: Option<DockNode>,
        popped: Vec<PopOut<PanelId>>,
    ) -> Result<Self, DockError> {
        let layout = Self { root, popped };
        layout.validate()?;
        Ok(layout)
    }

    /// The docked tree; `None` when every panel is hidden or popped out.
    #[must_use]
    pub const fn root(&self) -> Option<&DockNode> {
        self.root.as_ref()
    }

    /// The popped-out panels, in the order they were popped out.
    #[must_use]
    pub fn popped(&self) -> &[PopOut<PanelId>] {
        &self.popped
    }

    /// Every docked panel, in tree order.
    #[must_use]
    pub fn docked(&self) -> Vec<PanelId> {
        let mut panels = Vec::new();
        if let Some(root) = &self.root {
            root.collect(&mut panels);
        }
        panels
    }

    /// Whether `panel` is docked in the tree.
    #[must_use]
    pub fn is_docked(&self, panel: PanelId) -> bool {
        self.root.as_ref().is_some_and(|root| root.contains(panel))
    }

    /// Whether `panel` is in its own window.
    #[must_use]
    pub fn is_popped(&self, panel: PanelId) -> bool {
        self.popped.iter().any(|popout| popout.key == panel)
    }

    /// Whether `panel` is shown anywhere.
    #[must_use]
    pub fn is_visible(&self, panel: PanelId) -> bool {
        self.is_docked(panel) || self.is_popped(panel)
    }

    /// Whether `panel` is shown: docked as its group's active tab, or
    /// popped out.
    #[must_use]
    pub fn is_showing(&self, panel: PanelId) -> bool {
        self.is_popped(panel) || self.active_tabs().contains(&panel)
    }

    /// The active panel of every tab group.
    #[must_use]
    pub fn active_tabs(&self) -> Vec<PanelId> {
        fn walk(node: &DockNode, out: &mut Vec<PanelId>) {
            match node {
                DockNode::Tabs { panels, active } => out.push(panels[*active]),
                DockNode::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            walk(root, &mut out);
        }
        out
    }

    fn validate(&self) -> Result<(), DockError> {
        if let Some(root) = &self.root
            && (!root.valid() || root.depth() > MAX_DEPTH)
        {
            return Err(DockError::Malformed);
        }
        let mut seen = self.docked();
        seen.extend(self.popped.iter().map(|popout| popout.key));
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() == count {
            Ok(())
        } else {
            Err(DockError::Duplicate)
        }
    }

    /// Hides `panel` wherever it is. Returns whether it was shown.
    pub fn close(&mut self, panel: PanelId) -> bool {
        let popped = self.popped.len();
        self.popped.retain(|popout| popout.key != panel);
        let docked = self.is_docked(panel);
        if docked {
            self.root = self.root.take().and_then(|root| root.without(panel));
        }
        docked || popped != self.popped.len()
    }

    /// Makes `panel` the active tab of its group; returns whether it is
    /// docked.
    pub fn activate(&mut self, panel: PanelId) -> bool {
        let Some(DockNode::Tabs { panels, active }) =
            self.root.as_mut().and_then(|root| root.group_mut(panel))
        else {
            return false;
        };
        if let Some(index) = panels.iter().position(|p| *p == panel) {
            *active = index;
        }
        true
    }

    /// Shows `panel` and brings it to the front: a hidden panel is docked
    /// back at its home place.
    pub fn show(&mut self, panel: PanelId) {
        if self.is_popped(panel) || self.activate(panel) {
            return;
        }
        self.insert_home(panel);
    }

    fn insert_home(&mut self, panel: PanelId) {
        let (anchors, edge) = panel.home();
        if let Some((target, zone)) = anchors
            .iter()
            .copied()
            .find(|(target, _)| self.is_docked(*target))
        {
            self.insert(panel, target, zone);
            // Beside its default neighbour, the split takes its default
            // share again.
            let (first, second) = match zone {
                DropZone::Left | DropZone::Top => (panel, target),
                _ => (target, panel),
            };
            if let Some(ratio) = default_ratio(first, second)
                && let Some(path) = self.split_path(first, second)
            {
                self.set_ratio(&path, ratio);
            }
        } else {
            self.insert_at_edge(panel, edge);
        }
    }

    /// The path of the split whose children are exactly the lone tab
    /// groups of `first` and `second`.
    fn split_path(&self, first: PanelId, second: PanelId) -> Option<NodePath> {
        fn walk(node: &DockNode, first: PanelId, second: PanelId, path: &mut NodePath) -> bool {
            let DockNode::Split {
                first: a,
                second: b,
                ..
            } = node
            else {
                return false;
            };
            let lone = |node: &DockNode, panel| matches!(node, DockNode::Tabs { panels, .. } if panels.as_slice() == [panel]);
            if lone(a, first) && lone(b, second) {
                return true;
            }
            path.push(false);
            if walk(a, first, second, path) {
                return true;
            }
            path.pop();
            path.push(true);
            if walk(b, first, second, path) {
                return true;
            }
            path.pop();
            false
        }
        let mut path = Vec::new();
        walk(self.root.as_ref()?, first, second, &mut path).then_some(path)
    }

    /// Docks `panel` (not shown anywhere) along `edge` of the whole layout.
    fn insert_at_edge(&mut self, panel: PanelId, edge: DropZone) {
        let new = DockNode::tabs(&[panel]);
        self.root = Some(match self.root.take() {
            None => new,
            Some(root) => match edge {
                DropZone::Left => DockNode::split(Axis::Horizontal, 240, new, root),
                DropZone::Right => DockNode::split(Axis::Horizontal, 765, root, new),
                DropZone::Top => DockNode::split(Axis::Vertical, 450, new, root),
                DropZone::Bottom | DropZone::Center => {
                    DockNode::split(Axis::Vertical, 600, root, new)
                }
            },
        });
    }

    /// Docks `panel` (not shown anywhere) at `zone` of `target`'s group.
    fn insert(&mut self, panel: PanelId, target: PanelId, zone: DropZone) {
        self.insert_node(DockNode::tabs(&[panel]), target, zone);
    }

    /// Docks tab group `new` (none of whose panels is shown) at `zone` of
    /// `target`'s group: into it, or split beside it.
    fn insert_node(&mut self, new: DockNode, target: PanelId, zone: DropZone) {
        let Some(group) = self.root.as_mut().and_then(|root| root.group_mut(target)) else {
            let mut panels = Vec::new();
            new.collect(&mut panels);
            for panel in panels {
                self.insert_at_edge(panel, zone);
            }
            return;
        };
        if zone == DropZone::Center {
            if let (
                DockNode::Tabs { panels, active },
                DockNode::Tabs {
                    panels: added,
                    active: shown,
                },
            ) = (group, new)
            {
                *active = panels.len() + shown;
                panels.extend(added);
            }
            return;
        }
        let old = core::mem::replace(group, DockNode::tabs(&[]));
        *group = match zone {
            DropZone::Left => DockNode::split(Axis::Horizontal, 500, new, old),
            DropZone::Right => DockNode::split(Axis::Horizontal, 500, old, new),
            DropZone::Top => DockNode::split(Axis::Vertical, 500, new, old),
            DropZone::Bottom | DropZone::Center => DockNode::split(Axis::Vertical, 500, old, new),
        };
    }

    /// Moves docked or popped `panel` to `zone` of `target`'s tab group.
    /// Returns whether the layout changed; dropping a panel onto its own
    /// lone tile, or onto a hidden target, changes nothing.
    pub fn move_panel(&mut self, panel: PanelId, target: PanelId, zone: DropZone) -> bool {
        if !self.is_docked(target) || !self.is_visible(panel) {
            return false;
        }
        if panel == target {
            let alone = matches!(
                self.root.as_mut().and_then(|root| root.group_mut(panel)),
                Some(DockNode::Tabs { panels, .. }) if panels.len() == 1
            );
            if alone || zone == DropZone::Center {
                return false;
            }
            // Split the panel out of its own group, beside the rest.
            let Some(DockNode::Tabs { panels, .. }) =
                self.root.as_mut().and_then(|root| root.group_mut(panel))
            else {
                return false;
            };
            let Some(other) = panels.iter().copied().find(|p| *p != panel) else {
                return false;
            };
            self.close(panel);
            self.insert(panel, other, zone);
            return true;
        }
        if zone == DropZone::Center
            && let Some(DockNode::Tabs { panels, .. }) =
                self.root.as_mut().and_then(|root| root.group_mut(target))
            && panels.contains(&panel)
        {
            return self.activate(panel);
        }
        self.close(panel);
        self.insert(panel, target, zone);
        true
    }

    /// Moves the whole tab group holding docked `panel`, tabs and shown
    /// tab as they are, to `zone` of `target`'s group. Returns whether the
    /// layout changed; dropping a group onto itself changes nothing.
    pub fn move_group(&mut self, panel: PanelId, target: PanelId, zone: DropZone) -> bool {
        let Some(DockNode::Tabs { panels, active }) = self.tab_group(panel) else {
            return false;
        };
        if panels.contains(&target) || !self.is_docked(target) {
            return false;
        }
        if panels.len() == 1 {
            return self.move_panel(panel, target, zone);
        }
        for moved in &panels {
            self.root = self.root.take().and_then(|root| root.without(*moved));
        }
        self.insert_node(DockNode::Tabs { panels, active }, target, zone);
        true
    }

    /// The tab group holding docked `panel`.
    #[must_use]
    pub fn tab_group(&self, panel: PanelId) -> Option<DockNode> {
        fn find(node: &DockNode, panel: PanelId) -> Option<&DockNode> {
            match node {
                DockNode::Tabs { panels, .. } => panels.contains(&panel).then_some(node),
                DockNode::Split { first, second, .. } => {
                    find(first, panel).or_else(|| find(second, panel))
                }
            }
        }
        find(self.root.as_ref()?, panel).cloned()
    }

    /// Every docked tab group's panels and shown tab, in tree order.
    #[must_use]
    pub fn tab_groups(&self) -> Vec<(Vec<PanelId>, usize)> {
        fn walk(node: &DockNode, out: &mut Vec<(Vec<PanelId>, usize)>) {
            match node {
                DockNode::Tabs { panels, active } => out.push((panels.clone(), *active)),
                DockNode::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            walk(root, &mut out);
        }
        out
    }

    /// Moves `panel` into its own window. Returns whether it changed.
    pub fn pop_out(&mut self, panel: PanelId, geometry: Option<WindowGeometry>) -> bool {
        if self.is_popped(panel) {
            return false;
        }
        if self.is_docked(panel) {
            self.root = self.root.take().and_then(|root| root.without(panel));
        }
        self.popped.push(PopOut {
            key: panel,
            geometry,
        });
        true
    }

    /// Returns popped-out `panel` to the layout at its home place.
    pub fn dock_back(&mut self, panel: PanelId) -> bool {
        let popped = self.popped.len();
        self.popped.retain(|popout| popout.key != panel);
        if popped == self.popped.len() {
            return false;
        }
        self.insert_home(panel);
        true
    }

    /// Records where popped-out `panel`'s window is.
    pub fn set_geometry(&mut self, panel: PanelId, geometry: WindowGeometry) {
        if let Some(popout) = self.popped.iter_mut().find(|popout| popout.key == panel) {
            popout.geometry = Some(geometry);
        }
    }

    /// Sets the ratio of the split at `path`, clamped to the kept range.
    pub fn set_ratio(&mut self, path: &[bool], value: u16) {
        if let Some(DockNode::Split { ratio, .. }) =
            self.root.as_mut().and_then(|root| root.node_mut(path))
        {
            *ratio = value.clamp(MIN_RATIO, MAX_RATIO);
        }
    }

    /// The tree as `studio.conf` text: `none`, a tab group
    /// `[key|key*|key]` (`*` marks the active tab, the first when
    /// unmarked), or a split `h500(node,node)` / `v500(node,node)` whose
    /// number is the first child's share per mille.
    #[must_use]
    pub fn encode_tree(&self) -> String {
        fn write(node: &DockNode, out: &mut String) {
            match node {
                DockNode::Tabs { panels, active } => {
                    out.push('[');
                    for (index, panel) in panels.iter().enumerate() {
                        if index > 0 {
                            out.push('|');
                        }
                        out.push_str(panel.key());
                        if index == *active && index > 0 {
                            out.push('*');
                        }
                    }
                    out.push(']');
                }
                DockNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    out.push(match axis {
                        Axis::Horizontal => 'h',
                        Axis::Vertical => 'v',
                    });
                    out.push_str(&ratio.to_string());
                    out.push('(');
                    write(first, out);
                    out.push(',');
                    write(second, out);
                    out.push(')');
                }
            }
        }
        let mut out = String::new();
        match &self.root {
            Some(root) => write(root, &mut out),
            None => out.push_str("none"),
        }
        out
    }

    /// The popped-out panels as `studio.conf` text.
    #[must_use]
    pub fn encode_popouts(&self) -> String {
        encode_popouts(&self.popped)
    }

    /// Decodes [`Self::encode_tree`] and [`Self::encode_popouts`] text.
    ///
    /// # Errors
    ///
    /// Returns [`DockError`] for text this version does not write.
    pub fn decode(tree: &str, popouts: &str) -> Result<Self, DockError> {
        let root = decode_tree(tree.trim())?;
        let popped = decode_popouts(popouts).ok_or(DockError::Malformed)?;
        Self::from_parts(root, popped)
    }

    /// Replaces the tree with decoded `tree` text, keeping the pop-outs.
    ///
    /// # Errors
    ///
    /// Returns [`DockError`] for bad text or a panel shown twice; the
    /// layout is unchanged then.
    pub fn set_tree(&mut self, tree: &str) -> Result<(), DockError> {
        let root = decode_tree(tree.trim())?;
        *self = Self::from_parts(root, self.popped.clone())?;
        Ok(())
    }

    /// Replaces the pop-outs with decoded `popouts` text, keeping the tree.
    ///
    /// # Errors
    ///
    /// Returns [`DockError`] for bad text or a panel shown twice; the
    /// layout is unchanged then.
    pub fn set_popouts(&mut self, popouts: &str) -> Result<(), DockError> {
        let popped = decode_popouts(popouts).ok_or(DockError::Malformed)?;
        *self = Self::from_parts(self.root.clone(), popped)?;
        Ok(())
    }
}

/// The share the default layout gives a split of exactly `first`'s and
/// `second`'s lone tab groups, if it has one.
fn default_ratio(first: PanelId, second: PanelId) -> Option<u16> {
    let layout = DockLayout::default();
    let path = layout.split_path(first, second)?;
    let mut node = layout.root.as_ref()?;
    for second_child in path {
        let DockNode::Split { first, second, .. } = node else {
            return None;
        };
        node = if second_child { second } else { first };
    }
    match node {
        DockNode::Split { ratio, .. } => Some(*ratio),
        DockNode::Tabs { .. } => None,
    }
}

fn decode_tree(text: &str) -> Result<Option<DockNode>, DockError> {
    if text == "none" {
        return Ok(None);
    }
    let mut parser = Parser {
        rest: text,
        depth: 0,
    };
    let node = parser.node().ok_or(DockError::Malformed)?;
    if parser.rest.is_empty() {
        Ok(Some(node))
    } else {
        Err(DockError::Malformed)
    }
}

struct Parser<'a> {
    rest: &'a str,
    depth: usize,
}

impl Parser<'_> {
    fn eat(&mut self, c: char) -> Option<()> {
        self.rest = self.rest.strip_prefix(c)?;
        Some(())
    }

    fn node(&mut self) -> Option<DockNode> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let node = if self.rest.starts_with('[') {
            self.tabs()
        } else {
            self.split()
        };
        self.depth -= 1;
        node
    }

    fn tabs(&mut self) -> Option<DockNode> {
        self.eat('[')?;
        let end = self.rest.find(']')?;
        let (body, rest) = self.rest.split_at(end);
        self.rest = &rest[1..];
        let mut panels = Vec::new();
        let mut active = None;
        for (index, entry) in body.split('|').enumerate() {
            let key = match entry.strip_suffix('*') {
                Some(key) if index > 0 && active.is_none() => {
                    active = Some(index);
                    key
                }
                Some(_) => return None,
                None => entry,
            };
            panels.push(PanelId::from_key(key)?);
        }
        Some(DockNode::Tabs {
            panels,
            active: active.unwrap_or(0),
        })
    }

    fn split(&mut self) -> Option<DockNode> {
        let axis = if self.eat('h').is_some() {
            Axis::Horizontal
        } else {
            self.eat('v')?;
            Axis::Vertical
        };
        let digits = self.rest.find('(')?;
        let (number, rest) = self.rest.split_at(digits);
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let ratio = number.parse::<u16>().ok()?;
        self.rest = rest;
        self.eat('(')?;
        let first = self.node()?;
        self.eat(',')?;
        let second = self.node()?;
        self.eat(')')?;
        Some(DockNode::split(axis, ratio, first, second))
    }
}

/// An operator's saved layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedLayout {
    pub name: String,
    pub layout: DockLayout,
}

/// Longest kept layout name, in characters.
pub const MAX_LAYOUT_NAME_CHARS: usize = 40;
/// Most saved layouts kept.
pub const MAX_SAVED_LAYOUTS: usize = 16;

/// Why a layout name is not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutNameProblem {
    Empty,
    TooLong,
    /// `|` separates the saved fields; control characters break the file.
    BadCharacter,
    Taken,
}

impl LayoutNameProblem {
    pub(crate) const fn message(self) -> Msg {
        match self {
            Self::Empty => Msg::ReasonNameEmpty,
            Self::TooLong => Msg::LayoutNameTooLong,
            Self::BadCharacter => Msg::LayoutNameBadCharacter,
            Self::Taken => Msg::LayoutNameTaken,
        }
    }
}

/// Checks a trimmed layout name against the saved layouts, ignoring the
/// one at `except` (the layout being renamed).
///
/// # Errors
///
/// Returns why the name cannot be used.
pub fn check_layout_name(
    name: &str,
    saved: &[NamedLayout],
    except: Option<usize>,
) -> Result<(), LayoutNameProblem> {
    if name.is_empty() {
        return Err(LayoutNameProblem::Empty);
    }
    if name.chars().count() > MAX_LAYOUT_NAME_CHARS {
        return Err(LayoutNameProblem::TooLong);
    }
    if name != name.trim() || name.chars().any(|c| c == '|' || c.is_control()) {
        return Err(LayoutNameProblem::BadCharacter);
    }
    let taken = saved.iter().enumerate().any(|(index, layout)| {
        Some(index) != except && layout.name.to_lowercase() == name.to_lowercase()
    });
    if taken {
        return Err(LayoutNameProblem::Taken);
    }
    Ok(())
}

impl NamedLayout {
    /// `name | tree | popouts`, the `layout.saved` value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{} | {} | {}",
            self.name,
            self.layout.encode_tree(),
            self.layout.encode_popouts()
        )
    }

    /// Decodes [`Self::encode`] text.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        // Names and pop-out lists hold no `|`; the tree's tab separators
        // sit between the first and the last one.
        let (name, rest) = text.split_once('|')?;
        let (tree, popouts) = rest.rsplit_once('|')?;
        let name = name.trim();
        check_layout_name(name, &[], None).ok()?;
        let layout = DockLayout::decode(tree.trim(), popouts.trim()).ok()?;
        Some(Self {
            name: name.to_owned(),
            layout,
        })
    }
}

#[cfg(test)]
mod tests;
