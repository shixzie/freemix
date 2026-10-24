//! The docking workspace: lays the [`DockLayout`] tree out over the
//! console area, draws each tile's tab bar (drag a tab to re-dock it, the
//! grip to move the whole group, double-click a tab to maximise the tile,
//! right-click it for more) and the active panel, the splitters between
//! tiles, and every popped-out panel in its own native window, whose
//! header grip and Dock back to… menu dock it at a chosen place.
//!
//! The panels' own drawing functions are unchanged; this module is only
//! the shell around them. Keyboard shortcuts are read in whichever window
//! has focus, so they work wherever a panel lives.

use std::collections::{BTreeMap, BTreeSet};

use egui::{
    Align, Align2, CentralPanel, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key,
    LayerId, Layout, Margin, Modifiers, Order, Panel, Pos2, Rect, Response, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportClass, ViewportCommand,
    ViewportId, WidgetInfo, WidgetType, pos2, vec2,
};
use fm_protocol::MonitorTap;
use fm_ui_model::i18n::{Arg, Msg};

use crate::dock::{
    Axis, DockLayout, DockNode, DropZone, LayoutPreset, MAX_RATIO, MIN_RATIO, NamedLayout,
    NodePath, PanelId, ScreenRect, ToolWindow, WindowGeometry, check_layout_name,
};
use crate::l10n::Locale;
use crate::monitors::{self, MonitorPicture};
use crate::theme::{
    accent_color, border_color, border_strong_color, graphite_color, line_width, panel_color,
    raised_color, text_color, text_dim_color, widget_hover_color,
};
use crate::toasts::ToastKind;
use crate::{
    ManualTransitionAvailability, StudioIntent, StudioShell, StudioUiState, fixed_width_column,
    mixer, overlays, panels, stinger_config, transitions,
};

/// Height of a tile's tab bar.
pub(crate) const TAB_BAR_HEIGHT: f32 = 26.0;
/// Width of the gap between tiles, which is also the splitter handle.
pub(crate) const GAP: f32 = 8.0;
/// Inner margin of a tile's body.
const TILE_PADDING: f32 = 8.0;
/// Side of a tab bar icon key.
const ICON_SIZE: f32 = 20.0;
/// Width of the drag grip at the start of a tab bar.
pub(crate) const GRIP_WIDTH: f32 = 14.0;
/// Inner size of a native window opened without a known size.
const DEFAULT_WINDOW_SIZE: (f32, f32) = (640.0, 420.0);
/// The places a popped-out panel can dock beside a tile, in menu order.
const DOCK_ZONES: [DropZone; 5] = [
    DropZone::Center,
    DropZone::Left,
    DropZone::Right,
    DropZone::Top,
    DropZone::Bottom,
];

/// What a drag in the workspace carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DragSource {
    /// One panel, dragged by its tab.
    Tab(PanelId),
    /// The whole tab group holding this panel, dragged by its grip.
    Group(PanelId),
}

/// Where a popped-out panel would dock, previewed in the main window
/// while its window's grip is dragged over it or a Dock back to… place
/// is hovered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteDrop {
    pub(crate) panel: PanelId,
    pub(crate) target: PanelId,
    pub(crate) zone: DropZone,
}

/// The menu name of a docked tab group: its panels' titles.
fn group_title(l: Locale, panels: &[PanelId]) -> String {
    panels
        .iter()
        .map(|panel| l.text(panel.title()))
        .collect::<Vec<_>>()
        .join(" · ")
}

const fn zone_title(zone: DropZone) -> Msg {
    match zone {
        DropZone::Center => Msg::DockZoneCenter,
        DropZone::Left => Msg::DockZoneLeft,
        DropZone::Right => Msg::DockZoneRight,
        DropZone::Top => Msg::DockZoneTop,
        DropZone::Bottom => Msg::DockZoneBottom,
    }
}

/// Whether dropping `source` at `zone` of `tile` would change nothing:
/// a group onto itself, or a lone tab onto its own tile or a tab into its
/// own group.
pub(crate) fn drop_is_noop(source: DragSource, tile: &Tile, zone: DropZone) -> bool {
    match source {
        DragSource::Tab(panel) => {
            tile.panels.contains(&panel) && (tile.panels.len() == 1 || zone == DropZone::Center)
        }
        DragSource::Group(panel) => tile.panels.contains(&panel),
    }
}

/// `pos` in a popped-out window's points as a position in the main
/// window's points: the same in an embedded window, else through both
/// windows' places on screen when the platform reports them.
pub(crate) fn to_main_window(
    pos: Pos2,
    embedded: bool,
    window_origin: Option<Pos2>,
    main_origin: Option<Pos2>,
) -> Option<Pos2> {
    if embedded {
        return Some(pos);
    }
    Some(pos + (window_origin? - main_origin?))
}

/// Something shown in its own native window.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PopKey {
    Panel(PanelId),
    Tool(ToolWindow),
}

impl PopKey {
    fn viewport(self) -> ViewportId {
        match self {
            Self::Panel(panel) => ViewportId::from_hash_of(("freemix-panel", panel.key())),
            Self::Tool(tool) => ViewportId::from_hash_of(("freemix-tool", tool.key())),
        }
    }
}

/// The Save layout / Rename layout dialog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LayoutNameDialog {
    /// The saved layout being renamed; `None` saves the current layout.
    pub(crate) rename: Option<usize>,
    pub(crate) name: String,
    pub(crate) focus_pending: bool,
}

/// Docking state that lives only in this session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DockUi {
    /// The geometry each open native window was created with; kept
    /// unchanged while it is open so egui never moves it back.
    spawned: BTreeMap<PopKey, Option<WindowGeometry>>,
    /// Each shown tile's rect last frame (rounded), by active panel.
    tile_rects: BTreeMap<PanelId, [i32; 4]>,
    /// Each docked panel's tile rect last frame (rounded) as the layout
    /// places it with no tile maximised, to size and place the panel's
    /// window when it is popped out.
    natural_rects: BTreeMap<PanelId, [i32; 4]>,
    /// The monitors, primary first, in points; saved windows are moved
    /// onto them when they open. Empty when the platform lists none.
    pub(crate) screens: Vec<ScreenRect>,
    /// Popped-out panels whose windows come to the front this frame.
    focus: BTreeSet<PanelId>,
    /// What is being dragged across the workspace.
    dragging: Option<DragSource>,
    /// The tile shown alone over the whole workspace, by a panel in it.
    pub(crate) maximized: Option<PanelId>,
    /// The maximised tile's panels and every docked panel when the
    /// workspace last drew it, to notice a panel docked out of sight.
    maximized_seen: Option<MaximizedSeen>,
    /// Where a popped-out panel would dock, previewed in the main window.
    remote_drop: Option<RemoteDrop>,
    /// The main window's inside top-left on screen, in whole points, when
    /// the platform reports it.
    main_origin: Option<[i32; 2]>,
    /// The open Save / Rename layout dialog.
    pub(crate) name_dialog: Option<LayoutNameDialog>,
}

/// What the workspace last drew while a tile was maximised.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MaximizedSeen {
    group: Vec<PanelId>,
    docked: Vec<PanelId>,
}

/// The panel whose tile stays maximised after the layout changed from
/// `seen`: the tile follows its remaining tabs when `maximized` is closed
/// or popped out, and every tile comes back (`None`) when a panel is
/// docked outside it, where it would be out of sight.
fn follow_maximized(
    dock: &DockLayout,
    maximized: PanelId,
    seen: Option<&MaximizedSeen>,
) -> Option<(PanelId, Vec<PanelId>)> {
    let panel = if dock.is_docked(maximized) {
        maximized
    } else {
        seen?.group.iter().copied().find(|p| dock.is_docked(*p))?
    };
    let Some(DockNode::Tabs { panels: group, .. }) = dock.tab_group(panel) else {
        return None;
    };
    if let Some(seen) = seen {
        let hidden = dock
            .docked()
            .into_iter()
            .any(|p| !group.contains(&p) && (seen.group.contains(&p) || !seen.docked.contains(&p)));
        if hidden {
            return None;
        }
    }
    Some((panel, group))
}

/// A laid-out tab group.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tile {
    pub(crate) panels: Vec<PanelId>,
    pub(crate) active: usize,
    pub(crate) rect: Rect,
}

impl Tile {
    pub(crate) fn active_panel(&self) -> PanelId {
        self.panels[self.active]
    }

    fn body(&self) -> Rect {
        Rect::from_min_max(
            pos2(self.rect.left(), self.rect.top() + TAB_BAR_HEIGHT),
            self.rect.max,
        )
        .shrink(TILE_PADDING)
    }
}

/// A laid-out split's handle.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Splitter {
    pub(crate) path: NodePath,
    pub(crate) axis: Axis,
    /// The handle between the two children.
    pub(crate) handle: Rect,
    /// The whole split.
    pub(crate) span: Rect,
    pub(crate) min_first: f32,
    pub(crate) min_second: f32,
}

/// The smallest rect `node` lays out in without scrolling a body.
pub(crate) fn min_size(node: &DockNode) -> Vec2 {
    match node {
        DockNode::Tabs { panels, .. } => {
            let (width, height) = panels
                .iter()
                .map(|panel| panel.min_size())
                .fold((0.0_f32, 0.0_f32), |(w, h), (pw, ph)| {
                    (w.max(pw), h.max(ph))
                });
            vec2(
                width + 2.0 * TILE_PADDING,
                height + TAB_BAR_HEIGHT + 2.0 * TILE_PADDING,
            )
        }
        DockNode::Split {
            axis,
            first,
            second,
            ..
        } => {
            let (a, b) = (min_size(first), min_size(second));
            match axis {
                Axis::Horizontal => vec2(a.x + GAP + b.x, a.y.max(b.y)),
                Axis::Vertical => vec2(a.x.max(b.x), a.y + GAP + b.y),
            }
        }
    }
}

/// The first child's extent of a split `extent` long: its ratio, kept
/// within both children's minimums when they fit, else shared in
/// proportion to them.
pub(crate) fn first_extent(extent: f32, ratio: u16, min_first: f32, min_second: f32) -> f32 {
    let available = (extent - GAP).max(0.0);
    if min_first + min_second <= available {
        (available * f32::from(ratio) / 1_000.0)
            .clamp(min_first, available - min_second)
            .round()
    } else if min_first + min_second > 0.0 {
        (available * min_first / (min_first + min_second)).round()
    } else {
        (available / 2.0).round()
    }
}

/// Lays `node` out over `rect`: every tab group's rect and every split's
/// handle.
pub(crate) fn layout_tiles(node: &DockNode, rect: Rect) -> (Vec<Tile>, Vec<Splitter>) {
    fn walk(
        node: &DockNode,
        rect: Rect,
        path: &mut NodePath,
        tiles: &mut Vec<Tile>,
        splitters: &mut Vec<Splitter>,
    ) {
        match node {
            DockNode::Tabs { panels, active } => tiles.push(Tile {
                panels: panels.clone(),
                active: *active,
                rect,
            }),
            DockNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (min_a, min_b) = (min_size(first), min_size(second));
                let (first_rect, handle, second_rect, min_first, min_second) = match axis {
                    Axis::Horizontal => {
                        let width = first_extent(rect.width(), *ratio, min_a.x, min_b.x);
                        let split = rect.left() + width;
                        (
                            Rect::from_min_max(rect.min, pos2(split, rect.bottom())),
                            Rect::from_min_max(
                                pos2(split, rect.top()),
                                pos2(split + GAP, rect.bottom()),
                            ),
                            Rect::from_min_max(pos2(split + GAP, rect.top()), rect.max),
                            min_a.x,
                            min_b.x,
                        )
                    }
                    Axis::Vertical => {
                        let height = first_extent(rect.height(), *ratio, min_a.y, min_b.y);
                        let split = rect.top() + height;
                        (
                            Rect::from_min_max(rect.min, pos2(rect.right(), split)),
                            Rect::from_min_max(
                                pos2(rect.left(), split),
                                pos2(rect.right(), split + GAP),
                            ),
                            Rect::from_min_max(pos2(rect.left(), split + GAP), rect.max),
                            min_a.y,
                            min_b.y,
                        )
                    }
                };
                splitters.push(Splitter {
                    path: path.clone(),
                    axis: *axis,
                    handle,
                    span: rect,
                    min_first,
                    min_second,
                });
                path.push(false);
                walk(first, first_rect, path, tiles, splitters);
                path.pop();
                path.push(true);
                walk(second, second_rect, path, tiles, splitters);
                path.pop();
            }
        }
    }
    let mut tiles = Vec::new();
    let mut splitters = Vec::new();
    walk(node, rect, &mut Vec::new(), &mut tiles, &mut splitters);
    (tiles, splitters)
}

/// Where a tab dropped at `pos` over `rect` lands: the middle (or the tab
/// bar) tabs it in, else the nearest edge splits beside it.
pub(crate) fn drop_zone(rect: Rect, pos: Pos2) -> DropZone {
    if pos.y < rect.top() + TAB_BAR_HEIGHT {
        return DropZone::Center;
    }
    let u = (pos.x - rect.left()) / rect.width().max(1.0);
    let v = (pos.y - rect.top()) / rect.height().max(1.0);
    if (0.3..=0.7).contains(&u) && (0.3..=0.7).contains(&v) {
        return DropZone::Center;
    }
    let edges = [
        (u, DropZone::Left),
        (1.0 - u, DropZone::Right),
        (v, DropZone::Top),
        (1.0 - v, DropZone::Bottom),
    ];
    edges
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(DropZone::Center, |(_, zone)| zone)
}

/// The part of `rect` a drop at `zone` would give the panel.
pub(crate) fn zone_preview(rect: Rect, zone: DropZone) -> Rect {
    let half = rect.size() / 2.0;
    match zone {
        DropZone::Left => Rect::from_min_size(rect.min, vec2(half.x, rect.height())),
        DropZone::Right => Rect::from_min_max(pos2(rect.center().x, rect.top()), rect.max),
        DropZone::Top => Rect::from_min_size(rect.min, vec2(rect.width(), half.y)),
        DropZone::Bottom => Rect::from_min_max(pos2(rect.left(), rect.center().y), rect.max),
        DropZone::Center => rect,
    }
}

/// The width and video height of a monitor well fitted inside `size`
/// (header included): the canvas shape is kept, so a 16:9 canvas shows
/// 16:9 whatever the tile's shape, letterboxed in the spare room.
pub(crate) fn monitor_fit(size: Vec2, canvas: (u16, u16)) -> (f32, f32) {
    let (canvas_width, canvas_height) = if canvas.0 == 0 || canvas.1 == 0 {
        monitors::DEFAULT_CANVAS
    } else {
        canvas
    };
    let aspect = f32::from(canvas_width) / f32::from(canvas_height);
    let video_budget = (size.y - monitors::MONITOR_HEADER_HEIGHT).max(40.0);
    let width = size.x.min(video_budget * aspect).floor().max(80.0);
    (
        width,
        monitors::monitor_video_height(width, canvas, video_budget),
    )
}

/// Where a monitor well `(width, video_height)` sits in a body `size`:
/// its left and top offsets, centring it both ways.
pub(crate) fn monitor_offset(size: Vec2, (width, video_height): (f32, f32)) -> (f32, f32) {
    let height = video_height + monitors::MONITOR_HEADER_HEIGHT;
    (
        ((size.x - width) / 2.0).max(0.0).floor(),
        ((size.y - height) / 2.0).max(0.0).floor(),
    )
}

/// Whether a monitor body `size` has room under a well `video_height`
/// tall for the level strip, with some band left over.
pub(crate) fn monitor_meters_fit(size: Vec2, video_height: f32) -> bool {
    size.y - monitors::MONITOR_HEADER_HEIGHT - video_height >= monitors::MONITOR_METER_HEIGHT + 24.0
}

/// The meter channels a monitor's level strip shows: Program's master mix,
/// or the realized Preview source's own level; `None` without a feed.
fn bus_meters(
    state: &StudioUiState,
    bus: monitors::Bus,
) -> Option<&[fm_protocol::AudioMeterChannel]> {
    let meters = state.audio_meters.as_ref()?;
    match bus {
        monitors::Bus::Program => Some(&meters.master),
        monitors::Bus::Preview => {
            let input = state.view.as_ref()?.switcher.realized.preview;
            let wire = fm_protocol::WireInputId::from_domain(input);
            meters
                .inputs
                .iter()
                .find(|entry| entry.input == wire)
                .map(|entry| entry.channels.as_slice())
        }
    }
}

/// What a frame's panels share: the monitor pictures and gates.
pub(crate) struct PanelFrame {
    pub(crate) manual: ManualTransitionAvailability,
    pub(crate) dt_ms: u32,
    pub(crate) pictures: [Option<MonitorPicture>; 2],
    pub(crate) native_runtime: bool,
}

/// One action picked in a tile's chrome, applied after drawing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DockAction {
    Activate(PanelId),
    Close(PanelId),
    PopOut(PanelId),
    Move(PanelId, PanelId, DropZone),
    MoveGroup(PanelId, PanelId, DropZone),
    ToggleMaximized(PanelId),
}

/// Least height the scrolling part of a short Transitions tile keeps when
/// the Fade to Black block is pinned below it.
const TRANSITIONS_MIN_SCROLL_HEIGHT: f32 = 120.0;

/// Lays out the transition column: it follows the tile's width within its
/// range and is centred in a wider tile, clear of the gutter where a short
/// tile's scroll bar floats.
fn transition_column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let room = ui.available_width() - transitions::SCROLL_GUTTER;
    let width = transitions::column_width(room);
    let spare = (room - width).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space((spare / 2.0).floor());
        fixed_width_column(ui, width, add);
    });
}

/// The tabs a group's tab bar shows: each tab's width, or `None` for a tab
/// with no room (listed under More tabs).
#[derive(Clone, Debug, PartialEq)]
struct TabFit {
    widths: Vec<Option<f32>>,
    /// Whether some tab has no room, so the More tabs key shows.
    overflow: bool,
}

/// Fits tabs of `natural` widths into `room`: every tab at its natural
/// width when they all fit; otherwise the active tab and, in order, as
/// many others as fit whole beside it and a `more`-wide More tabs key.
/// A label is never cut to a few letters; only an active tab wider than
/// the whole room is narrowed.
fn fit_tabs(natural: &[f32], active: usize, room: f32, more: f32) -> TabFit {
    const TAB_GAP: f32 = 2.0;
    let total: f32 = natural.iter().map(|width| width + TAB_GAP).sum();
    if total <= room {
        return TabFit {
            widths: natural.iter().copied().map(Some).collect(),
            overflow: false,
        };
    }
    let room = (room - more - TAB_GAP).max(0.0);
    let mut widths = vec![None; natural.len()];
    let Some(active_width) = natural.get(active) else {
        return TabFit {
            widths,
            overflow: !natural.is_empty(),
        };
    };
    let active_width = active_width.min(room).max(24.0);
    widths[active] = Some(active_width);
    let mut left = room - active_width - TAB_GAP;
    for (index, width) in natural.iter().enumerate() {
        if index != active && width + TAB_GAP <= left {
            widths[index] = Some(*width);
            left -= width + TAB_GAP;
        }
    }
    TabFit {
        widths,
        overflow: true,
    }
}

/// A painted icon key's glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Icon {
    PopOut,
    DockBack,
    Close,
    Restore,
    /// Three dots: more of the same in a menu.
    More,
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    let r = rect.shrink(5.0);
    match icon {
        Icon::More => {
            for step in [-1.0, 0.0, 1.0] {
                painter.circle_filled(r.center() + vec2(step * 4.0, 0.0), 1.5, color);
            }
        }
        Icon::Restore => {
            // Two stacked windows: the tiles come back.
            let back = Rect::from_min_max(
                pos2(r.left() + 3.0, r.top()),
                pos2(r.right(), r.bottom() - 3.0),
            );
            let front = back.translate(vec2(-3.0, 3.0));
            painter.rect_stroke(back, CornerRadius::same(1), stroke, StrokeKind::Inside);
            painter.rect_filled(front, CornerRadius::same(1), panel_color());
            painter.rect_stroke(front, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
        Icon::Close => {
            painter.line_segment([r.left_top(), r.right_bottom()], stroke);
            painter.line_segment([r.right_top(), r.left_bottom()], stroke);
        }
        Icon::PopOut | Icon::DockBack => {
            // A window frame with an arrow leaving it (pop out) or
            // entering it (dock back).
            let frame = Rect::from_min_max(
                pos2(r.left(), r.top() + 3.0),
                pos2(r.right() - 3.0, r.bottom()),
            );
            painter.rect_stroke(frame, CornerRadius::same(1), stroke, StrokeKind::Inside);
            let (from, to) = if icon == Icon::PopOut {
                (frame.center(), r.right_top())
            } else {
                (r.right_top(), frame.center())
            };
            painter.line_segment([from, to], stroke);
            let tip = to;
            let back = (from - to).normalized() * 3.5;
            let side = vec2(-back.y, back.x);
            painter.line_segment([tip, tip + back + side], stroke);
            painter.line_segment([tip, tip + back - side], stroke);
        }
    }
}

/// A small painted key with `hover` help, named `label` for assistive tech.
fn icon_key(ui: &mut Ui, id: Id, rect: Rect, icon: Icon, label: &str, hover: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let color = if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), widget_hover_color());
        text_color()
    } else {
        text_dim_color()
    };
    paint_icon(ui.painter(), rect, icon, color);
    response.on_hover_text(hover)
}

/// Paints a drag grip: two columns of dots.
fn paint_grip(painter: &egui::Painter, rect: Rect, color: Color32) {
    for row in [-1.0, 0.0, 1.0] {
        for column in [-1.0, 1.0] {
            let center = rect.center() + vec2(column * 2.5, row * 4.5);
            painter.circle_filled(center, 1.2, color);
        }
    }
}

/// A drag grip with `hover` help, named `label` for assistive tech.
fn grip(ui: &Ui, id: Id, rect: Rect, label: &str, hover: &str) -> Response {
    let response = ui.interact(rect, id, Sense::drag());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, label));
    let active = response.hovered() || response.dragged();
    if active {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), widget_hover_color());
        ui.ctx().set_cursor_icon(if response.dragged() {
            CursorIcon::Grabbing
        } else {
            CursorIcon::Grab
        });
    }
    paint_grip(
        ui.painter(),
        rect,
        if active {
            text_color()
        } else {
            text_dim_color()
        },
    );
    response.on_hover_text(hover)
}

/// Paints where a drop at `zone` of `tile` would land, labelled `label`
/// beside `at` (the pointer) or in the preview's middle.
fn paint_drop_preview(
    context: &egui::Context,
    tile: Rect,
    zone: DropZone,
    label: &str,
    at: Option<Pos2>,
) {
    let preview = zone_preview(tile, zone).shrink(2.0);
    let overlay =
        context.layer_painter(LayerId::new(Order::Foreground, Id::new("studio-dock-drop")));
    overlay.rect(
        preview,
        CornerRadius::same(6),
        accent_color().gamma_multiply(0.25),
        Stroke::new(2.0, accent_color()),
        StrokeKind::Inside,
    );
    if label.is_empty() {
        return;
    }
    // The label rides on a chip so it reads over any panel content.
    let galley = overlay.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), text_color());
    let size = galley.size() + vec2(16.0, 8.0);
    let chip = match at {
        Some(pos) => Rect::from_min_size(pos + vec2(14.0, 14.0), size),
        None => Rect::from_center_size(preview.center(), size),
    };
    overlay.rect(
        chip,
        CornerRadius::same(4),
        graphite_color(),
        Stroke::new(1.0, accent_color()),
        StrokeKind::Inside,
    );
    overlay.galley(chip.min + vec2(8.0, 4.0), galley, text_color());
}

impl StudioShell {
    /// Lays the dock out over `ui`'s rect and draws it. Returns the rect
    /// over the docked monitors and transition controls (or the whole
    /// area), where the connection card is shown.
    pub(crate) fn draw_workspace(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
    ) -> Rect {
        let area = ui.max_rect();
        #[allow(clippy::cast_possible_truncation)] // on-screen points
        let origin = ui.input(|input| {
            input
                .viewport()
                .inner_rect
                .map(|rect| [rect.min.x.round() as i32, rect.min.y.round() as i32])
        });
        self.dock_ui.main_origin = origin;
        let Some(root) = self.layout.dock.root().cloned() else {
            self.dock_ui.tile_rects.clear();
            self.dock_ui.natural_rects.clear();
            self.dock_ui.maximized = None;
            self.dock_ui.maximized_seen = None;
            self.draw_empty_workspace(ui, area);
            return area;
        };
        // A maximised tile is laid out alone over the whole area.
        let followed = self.dock_ui.maximized.and_then(|panel| {
            follow_maximized(
                &self.layout.dock,
                panel,
                self.dock_ui.maximized_seen.as_ref(),
            )
        });
        self.dock_ui.maximized = followed.as_ref().map(|(panel, _)| *panel);
        self.dock_ui.maximized_seen = followed.map(|(_, group)| MaximizedSeen {
            group,
            docked: self.layout.dock.docked(),
        });
        let maximized = self
            .dock_ui
            .maximized
            .and_then(|panel| self.layout.dock.tab_group(panel));
        let (tiles, splitters) = layout_tiles(maximized.as_ref().unwrap_or(&root), area);
        let mut actions = Vec::new();
        for tile in &tiles {
            self.draw_dock_tile(ui, tile, state, frame, intents, &mut actions);
        }
        for splitter in &splitters {
            self.draw_splitter(ui, splitter);
        }
        self.dock_ui.tile_rects = tiles
            .iter()
            .map(|tile| (tile.active_panel(), rounded_rect(tile.rect)))
            .collect();
        // A maximised tile covers the area; a panel popped out of it takes
        // the size and place its tile has among the others.
        let natural = if maximized.is_some() {
            layout_tiles(&root, area).0
        } else {
            tiles.clone()
        };
        self.dock_ui.natural_rects = natural
            .iter()
            .flat_map(|tile| {
                let rect = rounded_rect(tile.rect);
                tile.panels.iter().map(move |panel| (*panel, rect))
            })
            .collect();
        self.track_tab_drag(ui, &tiles, &mut actions);
        if let Some(drop) = self.dock_ui.remote_drop
            && let Some(tile) = tiles.iter().find(|tile| tile.panels.contains(&drop.target))
        {
            paint_drop_preview(
                ui.ctx(),
                tile.rect,
                drop.zone,
                self.locale.text(drop.panel.title()),
                None,
            );
        }
        for action in actions {
            self.apply_dock_action(ui.ctx(), action);
        }
        let monitors = tiles
            .iter()
            .filter(|tile| {
                matches!(
                    tile.active_panel(),
                    PanelId::Preview | PanelId::Program | PanelId::Transitions
                )
            })
            .map(|tile| tile.rect)
            .reduce(Rect::union);
        monitors.unwrap_or(area)
    }

    fn draw_empty_workspace(&self, ui: &Ui, area: Rect) {
        let l = self.locale;
        ui.painter().text(
            area.center(),
            Align2::CENTER_CENTER,
            l.text(Msg::DockEmpty),
            FontId::proportional(14.0),
            text_dim_color(),
        );
    }

    fn draw_dock_tile(
        &mut self,
        ui: &mut Ui,
        tile: &Tile,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
        actions: &mut Vec<DockAction>,
    ) {
        let rect = tile.rect;
        ui.painter().rect(
            rect,
            CornerRadius::same(6),
            panel_color(),
            Stroke::new(line_width(), border_color()),
            StrokeKind::Inside,
        );
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TAB_BAR_HEIGHT));
        ui.painter().line_segment(
            [bar.left_bottom(), bar.right_bottom()],
            Stroke::new(line_width(), border_color()),
        );
        self.draw_tab_bar(ui, tile, bar, actions);
        let body = tile.body();
        if body.width() < 1.0 || body.height() < 1.0 {
            return;
        }
        let panel = tile.active_panel();
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("studio-dock-body", panel.key()))
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_clip_rect(body.expand(3.0).intersect(ui.clip_rect()));
        self.draw_panel_body(&mut child, panel, state, frame, intents);
    }

    #[allow(clippy::too_many_lines)] // one bar: grip, tabs or More tabs, and keys
    fn draw_tab_bar(&self, ui: &mut Ui, tile: &Tile, bar: Rect, actions: &mut Vec<DockAction>) {
        let l = self.locale;
        let active = tile.active_panel();
        let maximized = self.dock_ui.maximized.is_some();
        let icons = if maximized { 3.0 } else { 2.0 };
        let icons_width = icons * (ICON_SIZE + 2.0) + 4.0;
        let close_rect = Rect::from_center_size(
            pos2(bar.right() - 4.0 - ICON_SIZE / 2.0, bar.center().y),
            Vec2::splat(ICON_SIZE),
        );
        let pop_rect = close_rect.translate(vec2(-(ICON_SIZE + 2.0), 0.0));
        let restore_rect = pop_rect.translate(vec2(-(ICON_SIZE + 2.0), 0.0));
        let grip_rect = Rect::from_min_size(
            pos2(bar.left() + 3.0, bar.top() + 3.0),
            vec2(GRIP_WIDTH, TAB_BAR_HEIGHT - 6.0),
        );
        // Its drag is followed by `track_tab_drag`.
        grip(
            ui,
            Id::new("studio-dock-grip").with(active.key()),
            grip_rect,
            l.text(Msg::DockGrip),
            l.text(Msg::DockGripHover),
        );
        let tabs_left = grip_rect.right() + 3.0;
        let tabs_width = (bar.right() - tabs_left - icons_width - 4.0).max(0.0);
        let font = FontId::proportional(crate::theme::legible(10.5));
        let tabs: Vec<(PanelId, String, f32)> = tile
            .panels
            .iter()
            .map(|panel| {
                let label = l.text(panel.title()).to_uppercase();
                let natural = ui
                    .painter()
                    .layout_no_wrap(label.clone(), font.clone(), text_color())
                    .size()
                    .x
                    + 16.0;
                (*panel, label, natural)
            })
            .collect();
        let widths: Vec<f32> = tabs.iter().map(|(_, _, natural)| *natural).collect();
        let active_index = tile
            .panels
            .iter()
            .position(|panel| *panel == active)
            .unwrap_or(0);
        let fit = fit_tabs(&widths, active_index, tabs_width, ICON_SIZE + 4.0);
        let mut x = tabs_left;
        for (index, (panel, label, _)) in tabs.iter().enumerate() {
            let Some(width) = fit.widths[index] else {
                continue;
            };
            let rect =
                Rect::from_min_size(pos2(x, bar.top() + 2.0), vec2(width, TAB_BAR_HEIGHT - 2.0));
            x += width + 2.0;
            self.draw_tab(ui, *panel, rect, *panel == active, label, &font, actions);
        }
        if fit.overflow {
            let more_rect = Rect::from_center_size(
                pos2(x + ICON_SIZE / 2.0, bar.center().y + 1.0),
                Vec2::splat(ICON_SIZE),
            );
            self.draw_more_tabs(ui, active, &tabs, &fit.widths, more_rect, actions);
        }
        let name = l.text(active.title());
        if maximized
            && icon_key(
                ui,
                Id::new("studio-dock-restore").with(active.key()),
                restore_rect,
                Icon::Restore,
                l.text(Msg::DockRestore),
                l.text(Msg::DockRestoreHover),
            )
            .clicked()
        {
            actions.push(DockAction::ToggleMaximized(active));
        }
        if icon_key(
            ui,
            Id::new("studio-dock-popout").with(active.key()),
            pop_rect,
            Icon::PopOut,
            l.text(Msg::DockPopOut),
            &l.format(Msg::DockPopOutNamed, &[("panel", Arg::Str(name))]),
        )
        .clicked()
        {
            actions.push(DockAction::PopOut(active));
        }
        if icon_key(
            ui,
            Id::new("studio-dock-close").with(active.key()),
            close_rect,
            Icon::Close,
            l.text(Msg::DockClosePanel),
            &l.format(Msg::DockClosePanelNamed, &[("panel", Arg::Str(name))]),
        )
        .clicked()
        {
            actions.push(DockAction::Close(active));
        }
    }

    /// One tab of a tile's tab bar: activates on click, drags to re-dock,
    /// and offers Pop out and Close panel on right-click.
    #[allow(clippy::too_many_arguments)]
    fn draw_tab(
        &self,
        ui: &mut Ui,
        panel: PanelId,
        rect: Rect,
        selected: bool,
        label: &str,
        font: &FontId,
        actions: &mut Vec<DockAction>,
    ) {
        let l = self.locale;
        let response = ui.interact(
            rect,
            Id::new("studio-dock-tab").with(panel.key()),
            Sense::click_and_drag(),
        );
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::Button, true, selected, l.text(panel.title()))
        });
        if selected || response.hovered() {
            ui.painter().rect_filled(
                rect,
                CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                if selected {
                    raised_color()
                } else {
                    widget_hover_color()
                },
            );
        }
        if selected {
            ui.painter().line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                Stroke::new(2.0, accent_color()),
            );
        }
        let color = if selected {
            text_color()
        } else {
            text_dim_color()
        };
        let galley = crate::theme::elided(
            ui.painter(),
            label,
            font.clone(),
            color,
            rect.width() - 10.0,
        );
        ui.painter().galley(
            pos2(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        if response.drag_started() {
            actions.push(DockAction::Activate(panel));
        }
        let response = response.on_hover_text(format!(
            "{}\n{}\n{}\n{}",
            l.text(panel.title()),
            l.text(panel.hover()),
            l.text(Msg::DockTabHover),
            l.text(Msg::DockTabMaximizeHover)
        ));
        if response.double_clicked() {
            actions.push(DockAction::ToggleMaximized(panel));
        } else if response.clicked() {
            actions.push(DockAction::Activate(panel));
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        response.context_menu(|ui| {
            ui.label(crate::theme::section_label(l.text(panel.title())));
            if ui
                .button(l.text(Msg::DockPopOut))
                .on_hover_text(l.text(Msg::DockPopOutHover))
                .clicked()
            {
                actions.push(DockAction::PopOut(panel));
                ui.close();
            }
            if ui
                .button(l.text(Msg::DockClosePanel))
                .on_hover_text(l.text(Msg::DockClosePanelHover))
                .clicked()
            {
                actions.push(DockAction::Close(panel));
                ui.close();
            }
        });
    }

    /// The More tabs key after a group's tabs: lists the panels with no
    /// room for a tab, and shows the one picked.
    fn draw_more_tabs(
        &self,
        ui: &mut Ui,
        active: PanelId,
        tabs: &[(PanelId, String, f32)],
        shown: &[Option<f32>],
        rect: Rect,
        actions: &mut Vec<DockAction>,
    ) {
        let l = self.locale;
        let response = icon_key(
            ui,
            Id::new("studio-dock-more").with(active.key()),
            rect,
            Icon::More,
            l.text(Msg::DockMoreTabs),
            l.text(Msg::DockMoreTabsHover),
        );
        egui::Popup::menu(&response).show(|ui| {
            for ((panel, _, _), width) in tabs.iter().zip(shown) {
                if width.is_some() {
                    continue;
                }
                if ui
                    .button(l.text(panel.title()))
                    .on_hover_text(l.text(panel.hover()))
                    .clicked()
                {
                    actions.push(DockAction::Activate(*panel));
                    ui.close();
                }
            }
        });
    }

    fn draw_splitter(&mut self, ui: &mut Ui, splitter: &Splitter) {
        let l = self.locale;
        let id = Id::new("studio-dock-split").with(&splitter.path);
        let response = ui
            .interact(splitter.handle, id, Sense::drag())
            .on_hover_text(l.text(Msg::DockSplitterHover));
        response.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Other, true, l.text(Msg::DockSplitter))
        });
        let horizontal = splitter.axis == Axis::Horizontal;
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(if horizontal {
                CursorIcon::ResizeColumn
            } else {
                CursorIcon::ResizeRow
            });
            let line = if horizontal {
                [
                    pos2(splitter.handle.center().x, splitter.handle.top() + 6.0),
                    pos2(splitter.handle.center().x, splitter.handle.bottom() - 6.0),
                ]
            } else {
                [
                    pos2(splitter.handle.left() + 6.0, splitter.handle.center().y),
                    pos2(splitter.handle.right() - 6.0, splitter.handle.center().y),
                ]
            };
            ui.painter()
                .line_segment(line, Stroke::new(2.0, accent_color()));
        }
        if response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let span = splitter.span;
            let (start, extent, at) = if horizontal {
                (span.left(), span.width(), pointer.x)
            } else {
                (span.top(), span.height(), pointer.y)
            };
            let available = (extent - GAP).max(1.0);
            let first = (at - start - GAP / 2.0).clamp(
                splitter.min_first,
                (available - splitter.min_second).max(splitter.min_first),
            );
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
            let ratio = (first / available * 1_000.0)
                .round()
                .clamp(f32::from(MIN_RATIO), f32::from(MAX_RATIO)) as u16;
            self.layout.dock.set_ratio(&splitter.path, ratio);
        }
    }

    /// While a tab or a tile's grip is dragged: highlights where it would
    /// land; on release, moves the panel or the whole tab group there.
    fn track_tab_drag(&mut self, ui: &Ui, tiles: &[Tile], actions: &mut Vec<DockAction>) {
        let context = ui.ctx();
        let dragging =
            |id: Id| context.is_being_dragged(id) || context.drag_stopped_id() == Some(id);
        let dragged = tiles.iter().find_map(|tile| {
            tile.panels
                .iter()
                .copied()
                .find(|panel| dragging(Id::new("studio-dock-tab").with(panel.key())))
                .map(DragSource::Tab)
                .or_else(|| {
                    let active = tile.active_panel();
                    dragging(Id::new("studio-dock-grip").with(active.key()))
                        .then_some(DragSource::Group(active))
                })
        });
        let Some(source) = dragged.or(self.dock_ui.dragging) else {
            return;
        };
        let released = context.input(|input| input.pointer.any_released())
            || !context.input(|input| input.pointer.any_down());
        let pointer = context.pointer_latest_pos();
        let target = pointer.and_then(|pos| {
            let tile = tiles.iter().find(|tile| tile.rect.contains(pos))?;
            let zone = drop_zone(tile.rect, pos);
            (!drop_is_noop(source, tile, zone)).then_some((tile, zone))
        });
        if released {
            self.dock_ui.dragging = None;
            if let Some((tile, zone)) = target {
                let target = tile.active_panel();
                actions.push(match source {
                    DragSource::Tab(panel) => DockAction::Move(panel, target, zone),
                    DragSource::Group(panel) => DockAction::MoveGroup(panel, target, zone),
                });
            }
            return;
        }
        self.dock_ui.dragging = Some(source);
        if let Some((tile, zone)) = target {
            let label = match source {
                DragSource::Tab(panel) => self.locale.text(panel.title()).to_owned(),
                DragSource::Group(panel) => self
                    .layout
                    .dock
                    .tab_group(panel)
                    .map(|group| match group {
                        DockNode::Tabs { panels, .. } => group_title(self.locale, &panels),
                        DockNode::Split { .. } => String::new(),
                    })
                    .unwrap_or_default(),
            };
            paint_drop_preview(context, tile.rect, zone, &label, pointer);
        }
    }

    fn apply_dock_action(&mut self, context: &egui::Context, action: DockAction) {
        match action {
            DockAction::Activate(panel) => {
                self.layout.dock.activate(panel);
            }
            DockAction::Close(panel) => {
                self.layout.dock.close(panel);
            }
            DockAction::PopOut(panel) => self.pop_out_panel(context, panel),
            DockAction::Move(panel, target, zone) => {
                self.layout.dock.move_panel(panel, target, zone);
            }
            DockAction::MoveGroup(panel, target, zone) => {
                self.layout.dock.move_group(panel, target, zone);
            }
            DockAction::ToggleMaximized(panel) => {
                self.dock_ui.maximized = match self.dock_ui.maximized {
                    Some(_) => None,
                    None => Some(panel),
                };
            }
        }
    }

    /// Esc restores a maximised tile, unless something else wants it: a
    /// dialog, menu, or text field, or the console keys (Esc cancels a
    /// manual transition first). Call after the console keys are read.
    pub(crate) fn restore_tiles_on_escape(&mut self, ui: &Ui) {
        if self.dock_ui.maximized.is_none()
            || self.console_keys_blocked(ui.ctx())
            || ui.ctx().egui_wants_keyboard_input()
        {
            return;
        }
        if ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            self.dock_ui.maximized = None;
        }
    }

    /// Shows `panel`: a hidden panel docks back home, a docked one becomes
    /// its group's shown tab, and a popped-out one's window comes to the
    /// front.
    pub(crate) fn show_panel(&mut self, panel: PanelId) {
        self.layout.dock.show(panel);
        if self.layout.dock.is_popped(panel) {
            self.dock_ui.focus.insert(panel);
        }
    }

    /// Tells the console which monitors there are, in points, the primary
    /// one first; a saved window that would open off every monitor opens
    /// on the primary one instead.
    pub fn set_screens(&mut self, screens: Vec<ScreenRect>) {
        self.dock_ui.screens = screens;
    }

    /// Moves `panel` into its own native window, sized and placed over the
    /// tile it leaves (as laid out with no tile maximised) when that is
    /// known.
    pub(crate) fn pop_out_panel(&mut self, context: &egui::Context, panel: PanelId) {
        let tile = self.dock_ui.natural_rects.get(&panel).copied();
        let origin = context.input(|input| input.viewport().inner_rect.map(|rect| rect.min));
        #[allow(clippy::cast_precision_loss)] // on-screen points
        let geometry = tile.map(|[x, y, width, height]| {
            let (min_width, min_height) = panel.min_size();
            WindowGeometry::from_points(
                (
                    (width as f32).max(min_width + 2.0 * TILE_PADDING),
                    (height as f32).max(min_height + TAB_BAR_HEIGHT + 2.0 * TILE_PADDING),
                ),
                origin.map(|origin| (origin.x + x as f32, origin.y + y as f32)),
            )
        });
        self.layout.dock.pop_out(panel, geometry);
    }

    /// Draws one panel's content in `ui`, scrolling it when `ui` is smaller
    /// than the panel's minimum; the monitors scale to fit instead.
    pub(crate) fn draw_panel_body(
        &mut self,
        ui: &mut Ui,
        panel: PanelId,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
    ) {
        // A press inside the panel gives it the focus scoped project
        // shortcut keys answer in (RC-004).
        let area = ui.max_rect();
        let layer = ui.layer_id();
        if ui.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| area.contains(pos))
        }) && ui
            .input(|input| input.pointer.interact_pos())
            .is_some_and(|pos| ui.ctx().layer_id_at(pos) == Some(layer))
        {
            self.focused_panel = Some(panel);
            // A popped-out panel embedded in the main window takes the
            // window focus back below.
            self.focused_window = None;
        }
        if let PanelId::Preview | PanelId::Program = panel {
            self.draw_monitor_panel(ui, panel, state, frame);
            return;
        }
        if panel == PanelId::Transitions {
            self.draw_transitions_body(ui, state, frame, intents);
            return;
        }
        let (min_width, min_height) = panel.min_size();
        // The mixer keeps at least one input strip beside Master in view.
        let min_width = if panel == PanelId::Mixer {
            min_width.max(mixer::mixer_width(1))
        } else {
            min_width
        };
        let fills_height = matches!(panel, PanelId::InputBank | PanelId::Mixer);
        ScrollArea::both()
            .id_salt(("studio-dock-scroll", panel.key()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let viewport = ui.available_size();
                // A tall body's vertical scroll bar takes its width from a
                // tile sized to the minimum: give it that width rather than
                // overflow by it and scroll sideways too (the Cues panel's
                // GO key ran under the bar at 1280x720).
                let width = if fills_height {
                    viewport.x.max(min_width)
                } else {
                    viewport
                        .x
                        .max(min_width - ui.spacing().scroll.allocated_width())
                };
                let height = if fills_height {
                    viewport.y.max(min_height)
                } else {
                    f32::INFINITY
                };
                let rect = Rect::from_min_size(ui.cursor().min, vec2(width, height));
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        if fills_height {
                            ui.set_min_size(vec2(width, height));
                        } else {
                            ui.set_min_width(width);
                        }
                        ui.spacing_mut().item_spacing.y = 4.0;
                        self.draw_panel_content(ui, panel, state, frame, intents);
                    },
                );
            });
    }

    fn draw_monitor_panel(
        &self,
        ui: &mut Ui,
        panel: PanelId,
        state: &StudioUiState,
        frame: &PanelFrame,
    ) {
        let bus = if panel == PanelId::Preview {
            monitors::Bus::Preview
        } else {
            monitors::Bus::Program
        };
        let canvas = self.canvas;
        let size = ui.available_size();
        let (width, video_height) = monitor_fit(size, canvas);
        // A tile taller than the picture needs shows the bus's audio under
        // it rather than a bare band; the two are centred together.
        let meters = monitor_meters_fit(size, video_height);
        let strip = if meters {
            monitors::MONITOR_METER_HEIGHT + 4.0
        } else {
            0.0
        };
        // Letterboxed in the tile: centred both ways.
        let (left, top) = monitor_offset(size, (width, video_height + strip));
        ui.add_space(top);
        ui.horizontal_top(|ui| {
            ui.add_space(left);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                monitors::draw_monitor(
                    ui,
                    bus,
                    state.view.as_ref(),
                    monitors::WellShape {
                        width,
                        video_height,
                        canvas,
                    },
                    frame.pictures[bus.tap().index()],
                    frame.native_runtime,
                );
                if meters {
                    monitors::draw_monitor_meters(ui, bus, width, bus_meters(state, bus));
                }
            });
        });
    }

    /// The Transitions tile. The column scrolls in a short tile, but once
    /// it overflows, the Fade to Black block is pinned along the tile's
    /// bottom edge instead, so an open manual `DETAILS` block (or any other
    /// growth) never pushes the emergency key out of view.
    fn draw_transitions_body(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
    ) {
        let (min_width, _) = PanelId::Transitions.min_size();
        // Last frame's heights: the column above the block, and the block.
        let memory = ui.id().with("studio-transitions-heights");
        let (column, block): (f32, f32) = ui.data(|data| data.get_temp(memory)).unwrap_or_default();
        let full = ui.available_rect_before_wrap();
        // Pin only while the whole column overflows and the scrolling part
        // keeps a usable height.
        let pin = block > 0.0
            && column + block > full.height()
            && full.height() - block >= TRANSITIONS_MIN_SCROLL_HEIGHT;
        let scroll_rect = if pin {
            Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - block))
        } else {
            full
        };
        let mut measured_block = block;
        let output = ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(scroll_rect)
                    .layout(Layout::top_down(Align::Min)),
                |ui| {
                    ScrollArea::both()
                        .id_salt(("studio-dock-scroll", PanelId::Transitions.key()))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width().max(min_width));
                            ui.spacing_mut().item_spacing.y = 4.0;
                            transition_column(ui, |ui| {
                                transitions::draw_transition_column(
                                    ui,
                                    self,
                                    state,
                                    frame.manual,
                                    intents,
                                );
                                if !pin {
                                    let top = ui.cursor().min.y;
                                    transitions::draw_fade_to_black_block(ui, self, state, intents);
                                    measured_block = ui.min_rect().max.y - top;
                                }
                            });
                        })
                },
            )
            .inner;
        let mut column_height = output.content_size.y;
        if pin {
            let block_rect = Rect::from_min_max(pos2(full.min.x, full.max.y - block), full.max);
            let pinned = ui.scope_builder(
                UiBuilder::new()
                    .max_rect(block_rect)
                    .layout(Layout::top_down(Align::Min)),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let top = ui.cursor().min.y;
                    transition_column(ui, |ui| {
                        transitions::draw_fade_to_black_block(ui, self, state, intents);
                    });
                    ui.min_rect().max.y - top
                },
            );
            measured_block = pinned.inner;
        } else {
            column_height -= measured_block;
        }
        // A frame that changes whether the block is pinned is redrawn at
        // once rather than on the next input.
        let pin_next = column_height + measured_block > full.height()
            && full.height() - measured_block >= TRANSITIONS_MIN_SCROLL_HEIGHT;
        if pin_next != pin {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|data| data.insert_temp(memory, (column_height, measured_block)));
    }

    /// One panel's own drawing functions, as the fixed console called them.
    fn draw_panel_content(
        &mut self,
        ui: &mut Ui,
        panel: PanelId,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
    ) {
        match panel {
            PanelId::Preview | PanelId::Program => {
                self.draw_monitor_panel(ui, panel, state, frame);
            }
            PanelId::Transitions => self.draw_transitions_body(ui, state, frame, intents),
            PanelId::InputBank => self.draw_inputs(ui, state, intents),
            PanelId::Mixer => {
                let mut fx = None;
                mixer::draw_mixer(
                    ui,
                    state,
                    &mut self.meters,
                    (frame.dt_ms, &mut self.layout.show_mixer_details, &mut fx),
                    intents,
                );
                if let Some(target) = fx {
                    self.strip_fx.open(target);
                }
            }
            PanelId::Outputs => {
                if let Some(action) = self.draw_recording(ui, state) {
                    intents.push(StudioIntent::Recording(action));
                }
                ui.add_space(6.0);
                crate::outputs::draw_streams(ui, state, &mut self.stream_targets, intents);
            }
            PanelId::Overlays => {
                overlays::draw_overlays(ui, state, self.transition_duration_frames, intents);
            }
            PanelId::Stingers => {
                panels::draw_stingers(
                    ui,
                    state,
                    &mut self.stinger_editor,
                    self.transition_duration_frames,
                    intents,
                );
                stinger_config::draw_stinger_configuration(
                    ui,
                    state,
                    &mut self.stinger_editor,
                    self.transition_duration_frames,
                    intents,
                );
            }
            PanelId::Titles => {
                self.titles.draw(ui, state, intents);
                if let Some(title) = self.titles.take_animate_request() {
                    self.open_animation(Some(title));
                }
            }
            PanelId::Playlist => self.playlist.draw(ui, state, intents),
            PanelId::Cues => self.cues.draw(ui, state, intents),
            PanelId::Replay => crate::replay_ui::draw_replay(ui, state, intents),
            PanelId::Media => crate::media_transport::draw_media(ui, state, intents),
        }
    }

    /// The builder for `key`'s native window: the geometry it was created
    /// with this session, so egui never moves or resizes it back.
    fn popout_builder(
        &mut self,
        key: PopKey,
        title: &str,
        geometry: Option<WindowGeometry>,
        min: Vec2,
    ) -> ViewportBuilder {
        let screens = &self.dock_ui.screens;
        let geometry = *self
            .dock_ui
            .spawned
            .entry(key)
            .or_insert_with(|| geometry.map(|geometry| geometry.onto_screens(screens)));
        let size = geometry.map_or(
            vec2(DEFAULT_WINDOW_SIZE.0, DEFAULT_WINDOW_SIZE.1),
            |geometry| vec2(f32::from(geometry.size.0), f32::from(geometry.size.1)),
        );
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{title} — FreeMix Studio"))
            .with_inner_size(size.max(min))
            .with_min_inner_size(min)
            .with_maximized(geometry.is_some_and(|geometry| geometry.maximized));
        if let Some((x, y)) = geometry.and_then(|geometry| geometry.position) {
            #[allow(clippy::cast_precision_loss)] // clamped screen points
            let position = pos2(x as f32, y as f32);
            builder = builder.with_position(position);
        }
        builder
    }

    /// Draws every popped-out panel in its own native window (an embedded
    /// window where the platform has only one). Closing a window, or its
    /// Dock back key, returns the panel to the layout.
    pub(crate) fn draw_panel_popouts(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        frame: &PanelFrame,
        intents: &mut Vec<StudioIntent>,
    ) {
        let popped: Vec<_> = self.layout.dock.popped().to_vec();
        self.dock_ui.spawned.retain(|key, _| match key {
            PopKey::Panel(panel) => popped.iter().any(|popout| popout.key == *panel),
            PopKey::Tool(_) => true,
        });
        self.dock_ui
            .focus
            .retain(|panel| popped.iter().any(|popout| popout.key == *panel));
        // The main window drew last frame's preview; this frame's windows
        // say again what to preview.
        let previous = self.dock_ui.remote_drop.take();
        for popout in popped {
            let panel = popout.key;
            let l = self.locale;
            let title = l.text(panel.title()).to_owned();
            let (min_width, min_height) = panel.min_size();
            let min = vec2(
                min_width + 2.0 * TILE_PADDING,
                min_height + 40.0 + 2.0 * TILE_PADDING,
            );
            let key = PopKey::Panel(panel);
            let builder = self.popout_builder(key, &title, popout.geometry, min);
            let (outcome, header) =
                context.show_viewport_immediate(key.viewport(), builder, |ui, class| {
                    let last = previous.filter(|drop| drop.panel == panel);
                    let header = self.panel_popout_header(ui, panel, class, last);
                    CentralPanel::default()
                        .frame(
                            Frame::new()
                                .fill(panel_color())
                                .inner_margin(Margin::same(10)),
                        )
                        .show(ui, |ui| {
                            self.draw_panel_body(ui, panel, state, frame, intents);
                        });
                    if class == ViewportClass::EmbeddedWindow {
                        // Embedded, the window shares the main window's
                        // input: a press in it gives it the keys scoped
                        // to its panel and its window (RC-004).
                        self.focus_window_on_press(
                            ui,
                            None,
                            crate::user_shortcuts::KeyWindow::Panel(panel),
                        );
                    }
                    self.native_window_keys(ui, panel, class, state, frame.manual, intents);
                    (window_outcome(ui, class, header.dock_back), header)
                });
            if self.dock_ui.focus.remove(&panel) {
                context.send_viewport_cmd_to(key.viewport(), ViewportCommand::Focus);
            }
            if let Some((target, zone)) = header.preview {
                self.dock_ui.remote_drop = Some(RemoteDrop {
                    panel,
                    target,
                    zone,
                });
            }
            if let Some((target, zone)) = header.drop {
                self.layout.dock.move_panel(panel, target, zone);
            } else if outcome.dock_back {
                self.layout.dock.dock_back(panel);
            } else if let Some(geometry) = outcome.kept(popout.geometry)
                && popout.geometry != Some(geometry)
            {
                self.layout.dock.set_geometry(panel, geometry);
            }
        }
        if self.dock_ui.remote_drop != previous {
            context.request_repaint_of(ViewportId::ROOT);
        }
    }

    /// The strip along the top of popped-out `panel`'s window: a grip to
    /// drag it back onto the console, its title, Dock back to… (every
    /// place beside or in a docked tile), and Dock back (home).
    ///
    /// `last` is the drop this window previewed last frame: the grip's
    /// release lands there when the pointer has already left the window's
    /// reach (X11 reports leaving as the button comes up).
    fn panel_popout_header(
        &self,
        ui: &mut Ui,
        panel: PanelId,
        class: ViewportClass,
        last: Option<RemoteDrop>,
    ) -> PopoutHeader {
        let l = self.locale;
        let title = l.text(panel.title());
        let groups = self.layout.dock.tab_groups();
        let mut header = PopoutHeader::default();
        Panel::top(Id::new("studio-popout-header").with(title))
            .exact_size(30.0)
            .frame(
                Frame::new()
                    .fill(graphite_color())
                    .stroke(Stroke::new(line_width(), border_strong_color()))
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let rect = ui.allocate_space(vec2(GRIP_WIDTH, ICON_SIZE)).1;
                    let response = grip(
                        ui,
                        Id::new("studio-popout-grip").with(panel.key()),
                        rect,
                        l.text(Msg::DockPopoutGrip),
                        &l.format(Msg::DockPopoutGripHover, &[("panel", Arg::Str(title))]),
                    );
                    if response.dragged() || response.drag_stopped() {
                        let target = self.remote_target(ui, class);
                        if response.drag_stopped() {
                            header.drop =
                                target.or_else(|| last.map(|drop| (drop.target, drop.zone)));
                        } else {
                            header.preview = target;
                        }
                    }
                    ui.label(crate::theme::section_label(title))
                        .on_hover_text(l.text(panel.hover()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let rect = ui.allocate_space(Vec2::splat(ICON_SIZE)).1;
                        if icon_key(
                            ui,
                            Id::new("studio-popout-dock").with(title),
                            rect,
                            Icon::DockBack,
                            l.text(Msg::DockBack),
                            &l.format(Msg::DockBackNamed, &[("panel", Arg::Str(title))]),
                        )
                        .clicked()
                        {
                            header.dock_back = true;
                        }
                        ui.menu_button(l.text(Msg::DockBackTo), |ui| {
                            ui.set_min_width(200.0);
                            if groups.is_empty() {
                                ui.label(
                                    egui::RichText::new(l.text(Msg::DockBackToNone))
                                        .size(crate::theme::legible(10.5))
                                        .color(text_dim_color()),
                                );
                            }
                            for (panels, active) in &groups {
                                let target = panels[*active];
                                let name = group_title(l, panels);
                                ui.menu_button(&name, |ui| {
                                    for zone in DOCK_ZONES {
                                        let item = ui
                                            .button(l.text(zone_title(zone)))
                                            .on_hover_text(l.format(
                                                Msg::DockZoneHover,
                                                &[("panel", Arg::Str(title))],
                                            ));
                                        if item.hovered() {
                                            header.preview = Some((target, zone));
                                        }
                                        if item.clicked() {
                                            header.drop = Some((target, zone));
                                            ui.close();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    l.format(Msg::DockTargetHover, &[("panels", Arg::Str(&name))]),
                                );
                            }
                        })
                        .response
                        .on_hover_text(
                            l.format(Msg::DockBackToHover, &[("panel", Arg::Str(title))]),
                        );
                    });
                });
            });
        header
    }

    /// The docked tile under the pointer of a popped-out window, and the
    /// zone of it the pointer is over.
    fn remote_target(&self, ui: &Ui, class: ViewportClass) -> Option<(PanelId, DropZone)> {
        let (pointer, origin) = ui.input(|input| {
            (
                input.pointer.latest_pos(),
                input.viewport().inner_rect.map(|rect| rect.min),
            )
        });
        #[allow(clippy::cast_precision_loss)] // on-screen points
        let main_origin = self
            .dock_ui
            .main_origin
            .map(|[x, y]| pos2(x as f32, y as f32));
        let pos = to_main_window(
            pointer?,
            class == ViewportClass::EmbeddedWindow,
            origin,
            main_origin,
        )?;
        self.dock_ui
            .tile_rects
            .iter()
            .find_map(|(panel, [x, y, width, height])| {
                #[allow(clippy::cast_precision_loss)] // on-screen points
                let rect = Rect::from_min_size(
                    pos2(*x as f32, *y as f32),
                    vec2(*width as f32, *height as f32),
                );
                rect.contains(pos).then(|| (*panel, drop_zone(rect, pos)))
            })
    }

    /// The native window for popped-out `tool`, and the builder to show it
    /// with; draw its contents with [`tool_popout_frame`] and hand the
    /// outcome to [`Self::finish_tool_popout`].
    pub(crate) fn tool_viewport(
        &mut self,
        tool: ToolWindow,
        geometry: Option<WindowGeometry>,
    ) -> (ViewportId, ViewportBuilder) {
        let title = self.locale.text(tool.title()).to_owned();
        let key = PopKey::Tool(tool);
        let builder = self.popout_builder(key, &title, geometry, Vec2::splat(360.0));
        (key.viewport(), builder)
    }

    /// Applies what popped-out `tool`'s window reported: Dock back (or
    /// closing the window) puts the tool back over the console; otherwise
    /// its place is remembered.
    pub(crate) fn finish_tool_popout(&mut self, tool: ToolWindow, outcome: WindowOutcome) {
        if outcome.dock_back {
            self.forget_tool_popout(tool);
            self.layout.set_tool_popped(tool, false);
        } else if let Some(previous) = self.layout.tool_popped(tool)
            && let Some(geometry) = outcome.kept(previous)
            && previous != Some(geometry)
        {
            self.layout.set_tool_geometry(tool, geometry);
        }
    }

    /// Forgets the creation geometry of a tool window that is closed, so
    /// it opens where it was last left next time.
    pub(crate) fn forget_tool_popout(&mut self, tool: ToolWindow) {
        self.dock_ui.spawned.remove(&PopKey::Tool(tool));
    }

    /// The console keys, then the project's shortcut keys and mouse
    /// buttons answering in `panel` and its window (RC-004), read in
    /// `panel`'s native pop-out window while it has the keyboard.
    fn native_window_keys(
        &mut self,
        ui: &mut Ui,
        panel: PanelId,
        class: ViewportClass,
        state: &StudioUiState,
        manual: ManualTransitionAvailability,
        intents: &mut Vec<StudioIntent>,
    ) {
        // An embedded window shares the console's input, which the console
        // already read this frame.
        if class == ViewportClass::EmbeddedWindow {
            return;
        }
        self.handle_preference_keys(ui);
        self.toggle_windows_on_keys(ui, state);
        self.console_and_user_keys(ui, state, Some(panel), manual, intents);
        crate::theme::paint_focus_ring(ui.ctx());
    }

    /// Announces a layout entry that could not be read, once.
    pub(crate) fn announce_rejected_layout(&mut self) {
        if self.layout.rejected {
            self.layout.rejected = false;
            let text = self.locale.text(Msg::LayoutRejected).to_owned();
            self.toasts.push(ToastKind::Notice, text);
        }
    }

    /// Applies a built-in layout; the panels' tool windows stay as they are.
    pub(crate) fn apply_preset(&mut self, preset: LayoutPreset) {
        self.layout.dock = preset.layout();
        self.dock_ui.dragging = None;
        self.dock_ui.maximized = None;
    }

    /// Applies the saved layout at `index`.
    pub(crate) fn apply_saved_layout(&mut self, index: usize) {
        if let Some(saved) = self.layout.saved_layouts.get(index) {
            self.layout.dock = saved.layout.clone();
            self.dock_ui.dragging = None;
            self.dock_ui.maximized = None;
        }
    }

    /// Deletes the saved layout at `index`.
    pub(crate) fn delete_saved_layout(&mut self, index: usize) {
        if index < self.layout.saved_layouts.len() {
            self.layout.saved_layouts.remove(index);
        }
    }

    /// Opens the Save layout dialog, or the Rename dialog for `rename`.
    pub(crate) fn open_layout_name_dialog(&mut self, rename: Option<usize>) {
        let name = rename
            .and_then(|index| self.layout.saved_layouts.get(index))
            .map(|saved| saved.name.clone())
            .unwrap_or_default();
        self.dock_ui.name_dialog = Some(LayoutNameDialog {
            rename,
            name,
            focus_pending: true,
        });
    }

    /// Draws the Save / Rename layout dialog when open.
    pub(crate) fn draw_layout_name_dialog(&mut self, context: &egui::Context) {
        let Some(mut dialog) = self.dock_ui.name_dialog.take() else {
            return;
        };
        let l = self.locale;
        let title = if dialog.rename.is_some() {
            Msg::LayoutRenameTitle
        } else {
            Msg::LayoutSaveTitle
        };
        let full = dialog.rename.is_none()
            && self.layout.saved_layouts.len() >= crate::dock::MAX_SAVED_LAYOUTS;
        let saved_layouts = &self.layout.saved_layouts;
        let mut name = String::new();
        let mut submit = false;
        let mut cancel = false;
        let close =
            crate::menu::help_modal(context, "studio-layout-name", l.text(title), 340.0, |ui| {
                ui.label(l.text(Msg::LayoutNameLabel));
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut dialog.name)
                            .char_limit(crate::dock::MAX_LAYOUT_NAME_CHARS)
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text(l.text(Msg::LayoutNameHover));
                if dialog.focus_pending {
                    field.request_focus();
                    dialog.focus_pending = false;
                }
                // Checked as edited this frame, so Enter right after the
                // last key saves the name as typed.
                dialog.name.trim().clone_into(&mut name);
                let problem = check_layout_name(&name, saved_layouts, dialog.rename).err();
                let reason = if full {
                    Some(Msg::LayoutTooMany)
                } else {
                    problem.map(crate::dock::LayoutNameProblem::message)
                };
                if let Some(reason) = reason.filter(|_| !dialog.name.is_empty() || full) {
                    ui.label(
                        egui::RichText::new(l.text(reason))
                            .size(crate::theme::legible(10.5))
                            .color(crate::theme::AMBER),
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let enabled = reason.is_none();
                    let english_reason = reason.map_or("", Msg::en);
                    if ui
                        .add_enabled(enabled, egui::Button::new(l.text(Msg::LayoutNameConfirm)))
                        .on_hover_text(l.text(Msg::LayoutNameConfirmHover))
                        .on_disabled_hover_text(crate::l10n::disabled_text(
                            l,
                            l.text(Msg::LayoutNameConfirmHover),
                            english_reason,
                        ))
                        .clicked()
                        || (enabled
                            && field.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                    {
                        submit = true;
                    }
                    if ui
                        .button(l.text(Msg::CancelKey))
                        .on_hover_text(l.text(Msg::LayoutNameCancelHover))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if submit {
            match dialog.rename {
                Some(index) => {
                    if let Some(saved) = self.layout.saved_layouts.get_mut(index) {
                        saved.name = name;
                    }
                }
                None => self.layout.saved_layouts.push(NamedLayout {
                    name,
                    layout: self.layout.dock.clone(),
                }),
            }
        } else if !(close || cancel) {
            self.dock_ui.name_dialog = Some(dialog);
        }
    }

    /// Whether the layout name dialog is open (it owns the keyboard).
    pub(crate) const fn layout_dialog_open(&self) -> bool {
        self.dock_ui.name_dialog.is_some()
    }

    /// Whether `layout` is the dock in effect.
    pub(crate) fn dock_is(&self, layout: &DockLayout) -> bool {
        self.layout.dock == *layout
    }
}

/// The strip along the top of a tool's native window: its title, and Dock
/// back and Close aligned right. Returns whether Close was clicked.
fn popout_header(
    ui: &mut Ui,
    l: Locale,
    tool: ToolWindow,
    close_hover: &str,
    dock_back: &mut bool,
) -> bool {
    let title = l.text(tool.title());
    let mut close = false;
    Panel::top(Id::new("studio-popout-header").with(tool.key()))
        .exact_size(30.0)
        .frame(
            Frame::new()
                .fill(graphite_color())
                .stroke(Stroke::new(line_width(), border_strong_color()))
                .inner_margin(Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(crate::theme::section_label(title))
                    .on_hover_text(l.text(tool.hover()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let rect = ui.allocate_space(Vec2::splat(ICON_SIZE)).1;
                    close = icon_key(
                        ui,
                        Id::new("studio-popout-close").with(tool.key()),
                        rect,
                        Icon::Close,
                        l.text(Msg::DockClosePanel),
                        close_hover,
                    )
                    .clicked();
                    let rect = ui.allocate_space(Vec2::splat(ICON_SIZE)).1;
                    if icon_key(
                        ui,
                        Id::new("studio-popout-dock").with(tool.key()),
                        rect,
                        Icon::DockBack,
                        l.text(Msg::DockBack),
                        &l.format(Msg::DockBackNamed, &[("panel", Arg::Str(title))]),
                    )
                    .clicked()
                    {
                        *dock_back = true;
                    }
                });
            });
        });
    close
}

/// Height of a floating tool window's header strip.
pub(crate) const TOOL_HEADER_HEIGHT: f32 = 28.0;

/// What a floating tool window's header asked for this frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ToolHeader {
    /// Close the tool (the header's close key).
    pub(crate) close: bool,
    /// Move the tool into its own native window (the Pop out key).
    pub(crate) pop_out: bool,
}

/// The frame of a floating tool window: the console's raised card.
pub(crate) fn tool_window_frame() -> Frame {
    Frame::new()
        .fill(raised_color())
        .stroke(Stroke::new(line_width(), border_strong_color()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 4,
            bottom: 12,
        })
}

/// The rect a floating tool window keeps inside: the work area below the
/// top bar, as last drawn (or the window below the bars before then).
pub(crate) fn tool_area(shell: &StudioShell, context: &egui::Context) -> Rect {
    shell.work_area.unwrap_or_else(|| {
        let viewport = context.viewport_rect();
        Rect::from_min_max(pos2(viewport.left(), viewport.top() + 72.0), viewport.max)
    })
}

/// Draws a floating tool window's body: a header strip (a grip, the
/// tool's title, and Pop out and Close aligned right, like a docked
/// panel's tab bar) above `add_contents`, as wide as what the contents
/// drew. The strip senses no drag, so dragging it moves the window.
pub(crate) fn floating_tool_frame<R>(
    ui: &mut Ui,
    l: Locale,
    tool: ToolWindow,
    close_hover: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> (ToolHeader, R) {
    let top = ui.cursor().min;
    ui.allocate_exact_size(vec2(0.0, TOOL_HEADER_HEIGHT + 6.0), Sense::hover());
    let inner = add_contents(ui);
    let width = (ui.min_rect().right() - top.x).max(4.0 * ICON_SIZE + GRIP_WIDTH);
    let header = Rect::from_min_size(top, vec2(width, TOOL_HEADER_HEIGHT));
    let title = l.text(tool.title());
    let spec = HeaderSpec {
        key: tool.key(),
        title,
        title_hover: Some(l.text(tool.hover())),
        pop_out: Some(l.format(Msg::DockPopOutNamed, &[("panel", Arg::Str(title))])),
        close_hover,
    };
    (tool_header(ui, l, &spec, header), inner)
}

/// Draws a floating window that cannot pop out (such as a strip's FX)
/// with the same header strip as a tool window: a grip, `title`, and
/// Close aligned right. Returns whether Close was clicked.
pub(crate) fn floating_window_frame<R>(
    ui: &mut Ui,
    l: Locale,
    key: &str,
    title: &str,
    close_hover: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> (bool, R) {
    let top = ui.cursor().min;
    ui.allocate_exact_size(vec2(0.0, TOOL_HEADER_HEIGHT + 6.0), Sense::hover());
    let inner = add_contents(ui);
    let width = (ui.min_rect().right() - top.x).max(3.0 * ICON_SIZE + GRIP_WIDTH);
    let header = Rect::from_min_size(top, vec2(width, TOOL_HEADER_HEIGHT));
    let spec = HeaderSpec {
        key,
        title,
        title_hover: None,
        pop_out: None,
        close_hover,
    };
    (tool_header(ui, l, &spec, header).close, inner)
}

/// What a floating window's header strip shows.
struct HeaderSpec<'a> {
    /// The window's stable key, for the header's widget ids.
    key: &'a str,
    title: &'a str,
    title_hover: Option<&'a str>,
    /// The Pop out key's hover, when the window can pop out.
    pop_out: Option<String>,
    close_hover: &'a str,
}

/// Paints the header strip of a floating tool window in `rect`.
fn tool_header(ui: &mut Ui, l: Locale, spec: &HeaderSpec<'_>, rect: Rect) -> ToolHeader {
    let mut asked = ToolHeader::default();
    let painter = ui.painter();
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(line_width(), border_color()),
    );
    let grip_rect = Rect::from_min_size(
        pos2(rect.left(), rect.top() + 5.0),
        vec2(GRIP_WIDTH, TOOL_HEADER_HEIGHT - 10.0),
    );
    paint_grip(painter, grip_rect, text_dim_color());
    let close_rect = Rect::from_center_size(
        pos2(rect.right() - ICON_SIZE / 2.0, rect.center().y),
        Vec2::splat(ICON_SIZE),
    );
    let pop_rect = close_rect.translate(vec2(-(ICON_SIZE + 2.0), 0.0));
    let title_end = if spec.pop_out.is_some() {
        pop_rect.left()
    } else {
        close_rect.left()
    };
    let title_rect = Rect::from_min_max(
        pos2(grip_rect.right() + 6.0, rect.top()),
        pos2(title_end - 6.0, rect.bottom()),
    );
    let mut title_ui = ui.new_child(
        UiBuilder::new()
            .id_salt(("studio-tool-title", spec.key))
            .max_rect(title_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    title_ui.set_clip_rect(title_rect.intersect(ui.clip_rect()));
    let title = title_ui.add(
        egui::Label::new(crate::theme::section_label(spec.title).color(text_color()))
            .selectable(false)
            .truncate(),
    );
    if let Some(hover) = spec.title_hover {
        title.on_hover_text(hover);
    }
    if let Some(hover) = &spec.pop_out
        && icon_key(
            ui,
            Id::new("studio-tool-popout").with(spec.key),
            pop_rect,
            Icon::PopOut,
            l.text(Msg::DockPopOut),
            hover,
        )
        .clicked()
    {
        asked.pop_out = true;
    }
    if icon_key(
        ui,
        Id::new("studio-tool-close").with(spec.key),
        close_rect,
        Icon::Close,
        l.text(Msg::DockClosePanel),
        spec.close_hover,
    )
    .clicked()
    {
        asked.close = true;
    }
    asked
}

/// What a popped-out panel's header asked for this frame.
#[derive(Clone, Copy, Debug, Default)]
struct PopoutHeader {
    /// Dock back home (the Dock back key).
    dock_back: bool,
    /// The place to preview in the main window.
    preview: Option<(PanelId, DropZone)>,
    /// The place to dock now.
    drop: Option<(PanelId, DropZone)>,
}

/// What a native window's frame reported.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WindowOutcome {
    dock_back: bool,
    /// The window's place, when it is neither minimised nor maximised.
    geometry: Option<WindowGeometry>,
    /// Whether the window is maximised.
    maximized: bool,
}

impl WindowOutcome {
    /// The geometry to keep for a window last kept at `previous`: its
    /// place, or while it is maximised the place it returns to when
    /// un-maximised (which a maximised window does not report), flagged
    /// maximised. `None` keeps `previous`.
    fn kept(self, previous: Option<WindowGeometry>) -> Option<WindowGeometry> {
        if self.maximized {
            let restored =
                previous.unwrap_or_else(|| WindowGeometry::from_points(DEFAULT_WINDOW_SIZE, None));
            return Some(restored.with_maximized(true));
        }
        self.geometry
    }
}

/// `rect` in whole points, as left, top, width, height.
#[allow(clippy::cast_possible_truncation)] // on-screen points
fn rounded_rect(rect: Rect) -> [i32; 4] {
    [
        rect.left().round() as i32,
        rect.top().round() as i32,
        rect.width().round() as i32,
        rect.height().round() as i32,
    ]
}

/// Draws a popped-out tool window's frame (its title, and Dock back and
/// Close keys) around `add_contents`, inside
/// [`egui::Context::show_viewport_immediate`]; also returns whether Close
/// was clicked.
pub(crate) fn tool_popout_frame(
    ui: &mut Ui,
    l: Locale,
    tool: ToolWindow,
    class: ViewportClass,
    close_hover: &str,
    add_contents: impl FnOnce(&mut Ui),
) -> (WindowOutcome, bool) {
    let mut dock_back = false;
    let close = popout_header(ui, l, tool, close_hover, &mut dock_back);
    CentralPanel::default()
        .frame(
            Frame::new()
                .fill(raised_color())
                .inner_margin(Margin::same(12)),
        )
        .show(ui, add_contents);
    (window_outcome(ui, class, dock_back), close)
}

/// Reads a native window's close request and place. A minimised window
/// reports no place worth keeping (Windows parks it at -32000, -32000),
/// so its last place stands; a maximised one reports the whole monitor,
/// so only that it is maximised is kept.
fn window_outcome(ui: &Ui, class: ViewportClass, dock_back: bool) -> WindowOutcome {
    if class == ViewportClass::EmbeddedWindow {
        return WindowOutcome {
            dock_back,
            ..WindowOutcome::default()
        };
    }
    ui.input(|input| {
        let viewport = input.viewport();
        let minimized = viewport.minimized == Some(true);
        let maximized = !minimized && viewport.maximized == Some(true);
        WindowOutcome {
            dock_back: dock_back || viewport.close_requested(),
            maximized,
            geometry: viewport
                .inner_rect
                .filter(|_| !minimized && !maximized)
                .map(|inner| {
                    WindowGeometry::from_points(
                        (inner.width(), inner.height()),
                        viewport.outer_rect.map(|outer| (outer.min.x, outer.min.y)),
                    )
                }),
        }
    })
}

/// The monitor pictures and gates every panel of this frame shares.
pub(crate) fn panel_frame(
    shell: &StudioShell,
    state: &StudioUiState,
    manual: ManualTransitionAvailability,
    dt_ms: u32,
) -> PanelFrame {
    let now = std::time::Instant::now();
    let native_runtime = state
        .monitor_images
        .iter()
        .flatten()
        .any(|(image, _)| image.origin == fm_protocol::MonitorFrameOrigin::Native);
    PanelFrame {
        manual,
        dt_ms,
        pictures: MonitorTap::ALL.map(|tap| {
            shell
                .monitor_textures
                .picture(tap, &state.monitor_images, now)
        }),
        native_runtime,
    }
}

#[cfg(test)]
mod tests;
