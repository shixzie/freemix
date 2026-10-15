//! The docking workspace: lays the [`DockLayout`] tree out over the
//! console area, draws each tile's tab bar (drag a tab to re-dock it,
//! right-click it for more) and the active panel, the splitters between
//! tiles, and every popped-out panel in its own native window.
//!
//! The panels' own drawing functions are unchanged; this module is only
//! the shell around them. Keyboard shortcuts are read in whichever window
//! has focus, so they work wherever a panel lives.

use std::collections::BTreeMap;

use egui::{
    Align, Align2, CentralPanel, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, LayerId,
    Layout, Margin, Order, Panel, Pos2, Rect, Response, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, ViewportBuilder, ViewportClass, ViewportId, WidgetInfo, WidgetType, pos2,
    vec2,
};
use fm_protocol::MonitorTap;
use fm_ui_model::i18n::{Arg, Msg};

use crate::dock::{
    Axis, DockLayout, DockNode, DropZone, LayoutPreset, MAX_RATIO, MIN_RATIO, NamedLayout,
    NodePath, PanelId, ToolWindow, WindowGeometry, check_layout_name,
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
    /// Each docked tile's rect last frame (rounded), by active panel, to
    /// size a panel's window when it is popped out.
    tile_rects: BTreeMap<PanelId, [i32; 4]>,
    /// The panel whose tab is being dragged.
    dragging: Option<PanelId>,
    /// The open Save / Rename layout dialog.
    pub(crate) name_dialog: Option<LayoutNameDialog>,
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
}

/// A painted icon key's glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Icon {
    PopOut,
    DockBack,
    Close,
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    let r = rect.shrink(5.0);
    match icon {
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
        let Some(root) = self.layout.dock.root().cloned() else {
            self.dock_ui.tile_rects.clear();
            self.draw_empty_workspace(ui, area);
            return area;
        };
        let (tiles, splitters) = layout_tiles(&root, area);
        let mut actions = Vec::new();
        for tile in &tiles {
            self.draw_dock_tile(ui, tile, state, frame, intents, &mut actions);
        }
        for splitter in &splitters {
            self.draw_splitter(ui, splitter);
        }
        self.dock_ui.tile_rects = tiles
            .iter()
            .map(|tile| {
                #[allow(clippy::cast_possible_truncation)] // on-screen points
                let rounded = [
                    tile.rect.left().round() as i32,
                    tile.rect.top().round() as i32,
                    tile.rect.width().round() as i32,
                    tile.rect.height().round() as i32,
                ];
                (tile.active_panel(), rounded)
            })
            .collect();
        self.track_tab_drag(ui, &tiles, &mut actions);
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

    fn draw_tab_bar(&self, ui: &mut Ui, tile: &Tile, bar: Rect, actions: &mut Vec<DockAction>) {
        let l = self.locale;
        let active = tile.active_panel();
        let icons_width = 2.0 * (ICON_SIZE + 2.0) + 4.0;
        let close_rect = Rect::from_center_size(
            pos2(bar.right() - 4.0 - ICON_SIZE / 2.0, bar.center().y),
            Vec2::splat(ICON_SIZE),
        );
        let pop_rect = close_rect.translate(vec2(-(ICON_SIZE + 2.0), 0.0));
        let tabs_width = (bar.width() - icons_width - 8.0).max(0.0);
        #[allow(clippy::cast_precision_loss)] // a handful of tabs
        let share = (tabs_width / tile.panels.len() as f32).floor();
        let mut x = bar.left() + 4.0;
        let font = FontId::proportional(crate::theme::legible(10.5));
        for panel in &tile.panels {
            let panel = *panel;
            let label = l.text(panel.title()).to_uppercase();
            let natural = ui
                .painter()
                .layout_no_wrap(label.clone(), font.clone(), text_color())
                .size()
                .x
                + 16.0;
            let width = natural.min(share).max(24.0);
            let rect =
                Rect::from_min_size(pos2(x, bar.top() + 2.0), vec2(width, TAB_BAR_HEIGHT - 2.0));
            x += width + 2.0;
            self.draw_tab(ui, panel, rect, panel == active, &label, &font, actions);
        }
        let name = l.text(active.title());
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
            "{}\n{}\n{}",
            l.text(panel.title()),
            l.text(panel.hover()),
            l.text(Msg::DockTabHover)
        ));
        if response.clicked() {
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

    /// While a tab is dragged: highlights where it would land; on release,
    /// moves it there.
    fn track_tab_drag(&mut self, ui: &Ui, tiles: &[Tile], actions: &mut Vec<DockAction>) {
        let context = ui.ctx();
        let dragged = tiles
            .iter()
            .flat_map(|tile| tile.panels.iter())
            .copied()
            .find(|panel| {
                context.is_being_dragged(Id::new("studio-dock-tab").with(panel.key()))
                    || context.drag_stopped_id()
                        == Some(Id::new("studio-dock-tab").with(panel.key()))
            });
        let Some(panel) = dragged.or(self.dock_ui.dragging) else {
            return;
        };
        let released = context.input(|input| input.pointer.any_released())
            || !context.input(|input| input.pointer.any_down());
        let pointer = context.pointer_latest_pos();
        let target = pointer.and_then(|pos| {
            let tile = tiles.iter().find(|tile| tile.rect.contains(pos))?;
            Some((tile, drop_zone(tile.rect, pos)))
        });
        if released {
            self.dock_ui.dragging = None;
            if let Some((tile, zone)) = target {
                actions.push(DockAction::Move(panel, tile.active_panel(), zone));
            }
            return;
        }
        self.dock_ui.dragging = Some(panel);
        if let Some((tile, zone)) = target {
            let preview = zone_preview(tile.rect, zone).shrink(2.0);
            let overlay =
                context.layer_painter(LayerId::new(Order::Foreground, Id::new("studio-dock-drop")));
            overlay.rect(
                preview,
                CornerRadius::same(6),
                accent_color().gamma_multiply(0.25),
                Stroke::new(2.0, accent_color()),
                StrokeKind::Inside,
            );
            if let Some(pos) = pointer {
                overlay.text(
                    pos + vec2(14.0, 14.0),
                    Align2::LEFT_TOP,
                    self.locale.text(panel.title()),
                    FontId::proportional(12.0),
                    text_color(),
                );
            }
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
        }
    }

    /// Moves `panel` into its own native window, placed over the tile it
    /// leaves when that is known.
    pub(crate) fn pop_out_panel(&mut self, context: &egui::Context, panel: PanelId) {
        let tile = self.dock_ui.tile_rects.get(&panel).copied();
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
        if let PanelId::Preview | PanelId::Program = panel {
            self.draw_monitor_panel(ui, panel, state, frame);
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
                let width = viewport.x.max(min_width);
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
        ui.horizontal_top(|ui| {
            ui.add_space(((size.x - width) / 2.0).max(0.0).floor());
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
        });
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
            PanelId::Transitions => {
                let spare = (ui.available_width() - transitions::TRANSITION_COLUMN_WIDTH).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.add_space((spare / 2.0).floor());
                    fixed_width_column(ui, transitions::TRANSITION_COLUMN_WIDTH, |ui| {
                        transitions::draw_transition_column(ui, self, state, frame.manual, intents);
                    });
                });
            }
            PanelId::InputBank => self.draw_inputs(ui, state, intents),
            PanelId::Mixer => mixer::draw_mixer(
                ui,
                state,
                &mut self.meters,
                (frame.dt_ms, &mut self.layout.show_mixer_details),
                intents,
            ),
            PanelId::Outputs => {
                if let Some(action) = self.draw_recording(ui, state) {
                    intents.push(StudioIntent::Recording(action));
                }
                ui.add_space(6.0);
                crate::outputs::draw_streams(ui, state, intents);
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
        let geometry = *self.dock_ui.spawned.entry(key).or_insert(geometry);
        let size = geometry.map_or(vec2(640.0, 420.0), |geometry| {
            vec2(f32::from(geometry.size.0), f32::from(geometry.size.1))
        });
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{title} — FreeMix Studio"))
            .with_inner_size(size.max(min))
            .with_min_inner_size(min);
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
            let outcome = context.show_viewport_immediate(key.viewport(), builder, |ui, class| {
                let mut dock_back = false;
                popout_header(ui, l, &title, panel.hover(), &mut dock_back);
                CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(panel_color())
                            .inner_margin(Margin::same(10)),
                    )
                    .show(ui, |ui| {
                        self.draw_panel_body(ui, panel, state, frame, intents);
                    });
                self.native_window_keys(ui, class, state, frame.manual, intents);
                window_outcome(ui, class, dock_back)
            });
            if outcome.dock_back {
                self.layout.dock.dock_back(panel);
            } else if let Some(geometry) = outcome.geometry
                && popout.geometry != Some(geometry)
            {
                self.layout.dock.set_geometry(panel, geometry);
            }
        }
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
        } else if let Some(geometry) = outcome.geometry
            && self.layout.tool_popped(tool) != Some(Some(geometry))
        {
            self.layout.set_tool_geometry(tool, geometry);
        }
    }

    /// Forgets the creation geometry of a tool window that is closed, so
    /// it opens where it was last left next time.
    pub(crate) fn forget_tool_popout(&mut self, tool: ToolWindow) {
        self.dock_ui.spawned.remove(&PopKey::Tool(tool));
    }

    /// The console keys, read in a native window that has the keyboard.
    fn native_window_keys(
        &mut self,
        ui: &mut Ui,
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
        self.toggle_windows_on_keys(ui);
        intents.extend(self.keyboard_intents(ui, state, manual));
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
    }

    /// Applies the saved layout at `index`.
    pub(crate) fn apply_saved_layout(&mut self, index: usize) {
        if let Some(saved) = self.layout.saved_layouts.get(index) {
            self.layout.dock = saved.layout.clone();
            self.dock_ui.dragging = None;
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
        let name = dialog.name.trim().to_owned();
        let problem = check_layout_name(&name, &self.layout.saved_layouts, dialog.rename).err();
        let full = dialog.rename.is_none()
            && self.layout.saved_layouts.len() >= crate::dock::MAX_SAVED_LAYOUTS;
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

/// The strip along the top of a native window: its title and Dock back.
fn popout_header(ui: &mut Ui, l: Locale, title: &str, hover: Msg, dock_back: &mut bool) {
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
                ui.label(crate::theme::section_label(title))
                    .on_hover_text(l.text(hover));
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
                        *dock_back = true;
                    }
                });
            });
        });
}

/// What a native window's frame reported.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WindowOutcome {
    dock_back: bool,
    geometry: Option<WindowGeometry>,
}

/// Draws a popped-out tool window's frame (its title and Dock back key)
/// around `add_contents`, inside [`egui::Context::show_viewport_immediate`].
pub(crate) fn tool_popout_frame(
    ui: &mut Ui,
    l: Locale,
    tool: ToolWindow,
    class: ViewportClass,
    add_contents: impl FnOnce(&mut Ui),
) -> WindowOutcome {
    let mut dock_back = false;
    popout_header(ui, l, l.text(tool.title()), tool.hover(), &mut dock_back);
    CentralPanel::default()
        .frame(
            Frame::new()
                .fill(raised_color())
                .inner_margin(Margin::same(12)),
        )
        .show(ui, add_contents);
    window_outcome(ui, class, dock_back)
}

/// Reads a native window's close request and place.
fn window_outcome(ui: &Ui, class: ViewportClass, dock_back: bool) -> WindowOutcome {
    if class == ViewportClass::EmbeddedWindow {
        return WindowOutcome {
            dock_back,
            geometry: None,
        };
    }
    ui.input(|input| {
        let viewport = input.viewport();
        WindowOutcome {
            dock_back: dock_back || viewport.close_requested(),
            geometry: viewport.inner_rect.map(|inner| {
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
