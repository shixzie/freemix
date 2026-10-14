//! Preview and Program monitor wells.
//!
//! The wells show replicated routing state, never video: each one is a dark
//! 16:9 surface with the routed source name centered and an explicit caption
//! that video preview delivery is still pending.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
    pos2,
};
use fm_types::InputId;
use fm_ui_model::{BusSelection, ClientView, SwitcherState};

use crate::theme::{
    BORDER, GRAPHITE_RAISED, MONITOR_BLACK, MUTED, PREVIEW, PROGRAM, TEXT, TEXT_DIM, routing_color,
};

/// Height of the tally-colored header strip above each monitor.
pub(crate) const MONITOR_HEADER_HEIGHT: f32 = 26.0;

pub(crate) struct MonitorRouting {
    pub(crate) desired: (InputId, String),
    pub(crate) realized: (InputId, String),
}

pub(crate) fn monitor_routing(
    switcher: SwitcherState,
    inputs: &[InputId],
    input_names: &[String],
    select: fn(BusSelection) -> InputId,
) -> MonitorRouting {
    let desired = select(switcher.desired);
    let realized = select(switcher.realized);
    MonitorRouting {
        desired: (desired, input_name(inputs, input_names, desired)),
        realized: (realized, input_name(inputs, input_names, realized)),
    }
}

pub(crate) fn label_for_input(view: &ClientView, input: InputId) -> String {
    input_name(&view.inputs, &view.input_names, input)
}

pub(crate) fn input_name(inputs: &[InputId], input_names: &[String], input: InputId) -> String {
    inputs
        .iter()
        .position(|candidate| *candidate == input)
        .map_or_else(
            || format!("Input {input}"),
            |index| input_names[index].clone(),
        )
}

/// Which bus a monitor well shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Bus {
    Preview,
    Program,
}

impl Bus {
    const fn label(self) -> &'static str {
        match self {
            Self::Preview => "PREVIEW",
            Self::Program => "PROGRAM",
        }
    }

    const fn color(self) -> Color32 {
        match self {
            Self::Preview => PREVIEW,
            Self::Program => PROGRAM,
        }
    }

    fn routing(self, view: &ClientView) -> MonitorRouting {
        monitor_routing(
            view.switcher,
            &view.inputs,
            &view.input_names,
            match self {
                Self::Preview => |selection: BusSelection| selection.preview,
                Self::Program => |selection: BusSelection| selection.program,
            },
        )
    }
}

/// Draws one monitor well of exactly `width` points, 16:9 plus its header.
#[allow(clippy::too_many_lines)] // one immediate-mode region
pub(crate) fn draw_monitor(ui: &mut Ui, bus: Bus, view: Option<&ClientView>, width: f32) {
    let routing = view.map(|view| bus.routing(view));
    let fade_to_black = bus == Bus::Program
        && view.is_some_and(|view| view.switcher.desired_fade_to_black.target_active);
    let video_height = (width * 9.0 / 16.0).round();
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, MONITOR_HEADER_HEIGHT + video_height),
        Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect.expand(1.0));
    let color = bus.color();
    let header = Rect::from_min_size(rect.min, Vec2::new(width, MONITOR_HEADER_HEIGHT));
    let video = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);

    painter.rect_filled(
        header,
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        GRAPHITE_RAISED,
    );
    // Tally bar along the top edge of the header.
    painter.rect_filled(
        Rect::from_min_size(header.min, Vec2::new(width, 3.0)),
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        color,
    );
    painter.text(
        pos2(header.min.x + 10.0, header.center().y + 1.0),
        Align2::LEFT_CENTER,
        bus.label(),
        FontId::new(12.0, FontFamily::Proportional),
        color,
    );
    if bus == Bus::Program
        && let Some(view) = view
    {
        // Confirmed active overlay channels keyed over Program (control state).
        let mut x = header.min.x + 80.0;
        for overlay in view
            .realized_overlays
            .iter()
            .filter(|overlay| overlay.active)
        {
            let badge =
                Rect::from_min_size(pos2(x, header.center().y - 7.0), Vec2::new(26.0, 15.0));
            painter.rect_filled(badge, CornerRadius::same(3), PROGRAM.gamma_multiply(0.25));
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("O{}", overlay.channel),
                FontId::new(10.0, FontFamily::Proportional),
                PROGRAM,
            );
            x += 30.0;
        }
    }
    if let Some(routing) = &routing {
        painter.text(
            pos2(header.max.x - 10.0, header.center().y + 1.0),
            Align2::RIGHT_CENTER,
            &routing.realized.1,
            FontId::new(12.0, FontFamily::Proportional),
            TEXT,
        );
    }

    painter.rect_filled(video, CornerRadius::ZERO, MONITOR_BLACK);
    draw_safe_area(&painter, video);
    let center = video.center();
    let title_size = (width / 22.0).clamp(15.0, 30.0);
    match &routing {
        Some(routing) => {
            painter.text(
                center - Vec2::new(0.0, title_size * 0.6),
                Align2::CENTER_CENTER,
                &routing.realized.1,
                FontId::new(title_size, FontFamily::Proportional),
                TEXT_DIM,
            );
            if routing.desired.0 != routing.realized.0 {
                painter.text(
                    center + Vec2::new(0.0, title_size * 0.45),
                    Align2::CENTER_CENTER,
                    format!("DESIRED: {}", routing.desired.1),
                    FontId::new(12.0, FontFamily::Proportional),
                    routing_color(routing.desired.0, routing.realized.0),
                );
            }
        }
        None => {
            painter.text(
                center - Vec2::new(0.0, title_size * 0.6),
                Align2::CENTER_CENTER,
                "ROUTING STATE UNAVAILABLE",
                FontId::new(title_size * 0.7, FontFamily::Proportional),
                MUTED,
            );
        }
    }
    painter.text(
        pos2(center.x, video.max.y - 30.0),
        Align2::CENTER_CENTER,
        "NO VIDEO FEED · preview delivery pending",
        FontId::new(10.5, FontFamily::Proportional),
        MUTED,
    );
    painter.text(
        pos2(center.x, video.max.y - 16.0),
        Align2::CENTER_CENTER,
        "routing state, not output video proof",
        FontId::new(9.5, FontFamily::Proportional),
        MUTED.gamma_multiply(0.75),
    );
    if fade_to_black {
        let badge = Rect::from_min_size(video.min + Vec2::new(10.0, 10.0), Vec2::new(44.0, 18.0));
        painter.rect_filled(badge, CornerRadius::same(3), PROGRAM);
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            "FTB",
            FontId::new(11.0, FontFamily::Proportional),
            Color32::WHITE,
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, color.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    if let Some(routing) = routing {
        response.on_hover_text(format!(
            "{} desired: {}\n{} realized: {}\nRouting state only; video preview delivery is pending.",
            bus.label(),
            routing.desired.1,
            bus.label(),
            routing.realized.1
        ));
    }
}

/// Faint title-safe corner marks so the empty well reads as a monitor.
fn draw_safe_area(painter: &egui::Painter, video: Rect) {
    let guide = Stroke::new(1.0, BORDER);
    let safe = video.shrink2(video.size() * 0.1);
    let arm = (safe.width().min(safe.height()) * 0.08).max(8.0);
    for (corner, dx, dy) in [
        (safe.left_top(), arm, arm),
        (safe.right_top(), -arm, arm),
        (safe.left_bottom(), arm, -arm),
        (safe.right_bottom(), -arm, -arm),
    ] {
        painter.line_segment([corner, corner + Vec2::new(dx, 0.0)], guide);
        painter.line_segment([corner, corner + Vec2::new(0.0, dy)], guide);
    }
}
