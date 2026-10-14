//! Broadcast-console theme: palette, `egui` style, and small shared painters.
//!
//! One accent color marks focus and selection; red is reserved for Program
//! and on-air state, green for Preview, amber for warnings and pending state.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Response, RichText, Sense, Stroke,
    StrokeKind, TextStyle, Ui, Vec2, style::HandleShape,
};

use crate::{StudioConnectionStatus, TallyState};

/// Application background behind every panel.
pub(crate) const GRAPHITE: Color32 = Color32::from_rgb(12, 13, 15);
/// Raised card surface inside panels.
pub(crate) const GRAPHITE_RAISED: Color32 = Color32::from_rgb(22, 24, 28);
/// Side, top, and status bar surface.
pub(crate) const PANEL: Color32 = Color32::from_rgb(17, 19, 22);
/// Idle widget surface.
pub(crate) const WIDGET: Color32 = Color32::from_rgb(34, 38, 44);
/// Hovered widget surface.
pub(crate) const WIDGET_HOVER: Color32 = Color32::from_rgb(46, 51, 59);
/// Pressed widget surface.
pub(crate) const WIDGET_ACTIVE: Color32 = Color32::from_rgb(58, 64, 74);
/// Subtle one-pixel border.
pub(crate) const BORDER: Color32 = Color32::from_rgb(38, 42, 49);
/// Stronger border for hovered or emphasized frames.
pub(crate) const BORDER_STRONG: Color32 = Color32::from_rgb(64, 71, 82);
/// Monitor and thumbnail black.
pub(crate) const MONITOR_BLACK: Color32 = Color32::from_rgb(5, 6, 7);
/// Primary text.
pub(crate) const TEXT: Color32 = Color32::from_rgb(222, 226, 230);
/// Secondary text such as section labels.
pub(crate) const TEXT_DIM: Color32 = Color32::from_rgb(150, 158, 168);
/// Tertiary text and inactive state.
pub(crate) const MUTED: Color32 = Color32::from_rgb(108, 116, 126);
/// The single interface accent.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(66, 150, 250);
/// Program bus, on-air, and recording.
pub(crate) const PROGRAM: Color32 = Color32::from_rgb(232, 48, 56);
/// Preview bus and healthy state.
pub(crate) const PREVIEW: Color32 = Color32::from_rgb(38, 196, 100);
/// Warnings and pending state.
pub(crate) const AMBER: Color32 = Color32::from_rgb(240, 168, 40);
/// Errors.
pub(crate) const ERROR: Color32 = Color32::from_rgb(255, 100, 91);

/// Dark fill used behind Program-colored controls.
pub(crate) const PROGRAM_FILL: Color32 = Color32::from_rgb(78, 18, 23);
/// Dark fill used behind amber controls.
pub(crate) const AMBER_FILL: Color32 = Color32::from_rgb(70, 48, 12);
/// Dark fill used behind accent controls.
pub(crate) const ACCENT_FILL: Color32 = Color32::from_rgb(20, 48, 84);

pub(crate) const RADIUS: u8 = 4;

/// Applies the console theme once per context and keeps the dark theme pinned.
pub(crate) fn apply_console_visuals(context: &egui::Context) {
    let applied = context.global_style().visuals.panel_fill == PANEL
        && context.options(|options| options.theme_preference == egui::ThemePreference::Dark);
    if applied {
        return;
    }
    context.set_theme(egui::ThemePreference::Dark);
    context.all_styles_mut(console_style);
}

fn console_style(style: &mut egui::Style) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = GRAPHITE_RAISED;
    visuals.window_stroke = Stroke::new(1.0, BORDER_STRONG);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);
    visuals.extreme_bg_color = Color32::from_rgb(8, 9, 11);
    visuals.faint_bg_color = Color32::from_rgb(20, 22, 26);
    visuals.code_bg_color = WIDGET;
    visuals.warn_fg_color = AMBER;
    visuals.error_fg_color = ERROR;
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = ACCENT_FILL;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = HandleShape::Rect { aspect_ratio: 0.6 };
    visuals.disabled_alpha = 0.45;
    visuals.striped = false;
    visuals.indent_has_left_vline = false;

    let radius = CornerRadius::same(RADIUS);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = GRAPHITE_RAISED;
    widgets.noninteractive.weak_bg_fill = GRAPHITE_RAISED;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = WIDGET;
    widgets.inactive.weak_bg_fill = WIDGET;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = radius;
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = WIDGET_HOVER;
    widgets.hovered.weak_bg_fill = WIDGET_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = WIDGET_ACTIVE;
    widgets.active.weak_bg_fill = WIDGET_ACTIVE;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    widgets.active.corner_radius = radius;
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = WIDGET_HOVER;
    widgets.open.weak_bg_fill = WIDGET_HOVER;
    widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.open.corner_radius = radius;

    style.visuals = visuals;
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(10.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.0, FontFamily::Monospace),
        ),
        (
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Proportional),
        ),
    ]
    .into();
    style.drag_value_text_style = TextStyle::Monospace;
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(8.0, 3.0);
    style.spacing.interact_size = Vec2::new(36.0, 22.0);
    style.spacing.window_margin = Margin::same(10);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.icon_width = 14.0;
    style.spacing.icon_width_inner = 8.0;
    style.spacing.slider_rail_height = 6.0;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = true;
    style.animation_time = 0.08;
}

/// Uppercase, letter-spaced section label (the console's "small caps").
pub(crate) fn section_label(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(10.5)
        .strong()
        .extra_letter_spacing(1.2)
        .color(TEXT_DIM)
}

/// Monospace numeric text for timecode, revisions, and counters.
pub(crate) fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

/// A raised card with a subtle border.
pub(crate) fn card() -> Frame {
    Frame::new()
        .fill(GRAPHITE_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(RADIUS + 2))
        .inner_margin(Margin::same(10))
}

/// Section header row: letter-spaced label plus optional trailing content.
pub(crate) fn section_header(ui: &mut Ui, title: &str, trailing: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(section_label(title));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    });
}

/// A small rounded status pill with colored text on a tinted fill.
pub(crate) fn pill(ui: &mut Ui, text: &str, color: Color32, filled: bool) -> Response {
    let font = FontId::new(10.0, FontFamily::Proportional);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        font,
        if filled { Color32::WHITE } else { color },
    );
    let size = Vec2::new(galley.size().x + 14.0, 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if filled {
            painter.rect_filled(rect, CornerRadius::same(9), color);
        } else {
            painter.rect_filled(rect, CornerRadius::same(9), color.gamma_multiply(0.14));
            painter.rect_stroke(
                rect,
                CornerRadius::same(9),
                Stroke::new(1.0, color.gamma_multiply(0.55)),
                StrokeKind::Inside,
            );
        }
        let text_pos = rect.center() - galley.size() / 2.0;
        painter.galley(text_pos, galley, color);
    }
    response
}

/// A status dot followed by a label.
pub(crate) fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Operator meaning of a status lamp or chip. Every status surface maps its
/// state onto one tone so red, green, and amber mean the same thing console-wide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tone {
    /// Off, idle, or unknown.
    Idle,
    /// Configured and ready (green).
    Ready,
    /// On air: recording, streaming live, or keyed over Program (red).
    OnAir,
    /// Requested but not yet realized, or degraded (amber).
    Pending,
    /// Failed or missing (error red).
    Fault,
}

impl Tone {
    pub(crate) const fn color(self) -> Color32 {
        match self {
            Self::Idle => MUTED,
            Self::Ready => PREVIEW,
            Self::OnAir => PROGRAM,
            Self::Pending => AMBER,
            Self::Fault => ERROR,
        }
    }

    /// Whether the lamp is lit (filled) rather than an unlit outline.
    pub(crate) const fn lit(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// A round status lamp: lit tones glow, idle lamps are an unlit outline.
pub(crate) fn paint_lamp(painter: &egui::Painter, center: egui::Pos2, radius: f32, tone: Tone) {
    let color = tone.color();
    if tone.lit() {
        painter.circle_filled(center, radius + 2.5, color.gamma_multiply(0.22));
        painter.circle_filled(center, radius, color);
    } else {
        painter.circle_filled(center, radius, MONITOR_BLACK);
        painter.circle_stroke(center, radius, Stroke::new(1.0, BORDER_STRONG));
    }
}

/// A fixed-width status chip so stacked rows keep their columns aligned.
pub(crate) fn status_chip(ui: &mut Ui, text: &str, tone: Tone, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        status_chip_painted(ui.painter(), rect, text, tone);
    }
    response
}

/// Paints a status chip into `rect`: solid red when on air, tinted otherwise.
pub(crate) fn status_chip_painted(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    tone: Tone,
) {
    let color = tone.color();
    let solid = tone == Tone::OnAir;
    painter.rect_filled(
        rect,
        CornerRadius::same(3),
        if solid {
            color
        } else {
            color.gamma_multiply(0.14)
        },
    );
    if !solid {
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, color.gamma_multiply(0.5)),
            StrokeKind::Inside,
        );
    }
    let text_color = if solid { Color32::WHITE } else { color };
    let galley = elided(
        painter,
        text,
        FontId::new(9.5, FontFamily::Proportional),
        text_color,
        rect.width() - 8.0,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, text_color);
}

/// Lays out one line of text, ending in `…` when it exceeds `max_width`.
pub(crate) fn elided(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(1.0));
    painter.layout_job(job)
}

/// Shortens `text` to at most `max_chars` characters, ending in `…` when cut.
/// Used where the width is not known in pixels, such as menu labels.
pub(crate) fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A centered empty-state message for a region with no replicated data:
/// a title in dim caps and one muted line saying why.
pub(crate) fn empty_state(ui: &mut Ui, title: &str, detail: &str) {
    ui.add_space((ui.available_height() * 0.3).clamp(12.0, 120.0));
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(title.to_uppercase())
                .size(11.0)
                .strong()
                .extra_letter_spacing(1.0)
                .color(TEXT_DIM),
        );
        ui.add(egui::Label::new(RichText::new(detail).size(10.5).color(MUTED)).wrap());
    });
}

/// Large square console button used by the transition column.
pub(crate) fn console_button(
    text: &str,
    fill: Color32,
    stroke: Color32,
    size: Vec2,
) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_owned())
            .strong()
            .size(13.0)
            .color(TEXT),
    )
    .fill(fill)
    .stroke(Stroke::new(1.0, stroke))
    .corner_radius(CornerRadius::same(RADIUS))
    .min_size(size)
}

/// Compact toolbar button.
pub(crate) fn small_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(11.0)).min_size(Vec2::new(0.0, 20.0))
}

pub(crate) fn routing_color(desired: fm_types::InputId, realized: fm_types::InputId) -> Color32 {
    if desired == realized { MUTED } else { AMBER }
}

pub(crate) const fn connection_color(status: StudioConnectionStatus) -> Color32 {
    match status {
        StudioConnectionStatus::Ready => PREVIEW,
        StudioConnectionStatus::Failed | StudioConnectionStatus::ProtocolMismatch => ERROR,
        StudioConnectionStatus::Disconnected => MUTED,
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => AMBER,
    }
}

/// Tally border color: realized roles win over desired-only roles.
pub(crate) const fn tally_color(tally: TallyState) -> Color32 {
    if tally.is_realized_program() {
        PROGRAM
    } else if tally.is_realized_preview() {
        PREVIEW
    } else if tally.is_desired_program() {
        Color32::from_rgb(200, 96, 66)
    } else if tally.is_desired_preview() {
        Color32::from_rgb(92, 170, 118)
    } else {
        BORDER_STRONG
    }
}

/// Short tile badge for the strongest tally role, if any.
pub(crate) const fn tally_badge(tally: TallyState) -> Option<&'static str> {
    if tally.is_realized_program() {
        Some("PGM")
    } else if tally.is_realized_preview() {
        Some("PVW")
    } else if tally.is_desired_program() {
        Some("PGM…")
    } else if tally.is_desired_preview() {
        Some("PVW…")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_keeps_short_text_and_cuts_long_text_on_characters() {
        assert_eq!(ellipsize("Camera 1", 12), "Camera 1");
        assert_eq!(ellipsize("Camera 1 Wide", 13), "Camera 1 Wide");
        assert_eq!(ellipsize("Camera 1 Wide", 9), "Camera 1…");
        // Multi-byte names are cut on characters, never inside a code point.
        assert_eq!(ellipsize("Kamera Größe Süd", 8), "Kamera…");
        assert_eq!(ellipsize("Ünïcödé", 3), "Ün…");
    }

    #[test]
    fn tones_map_to_the_console_palette() {
        assert_eq!(Tone::OnAir.color(), PROGRAM);
        assert_eq!(Tone::Ready.color(), PREVIEW);
        assert_eq!(Tone::Pending.color(), AMBER);
        assert_eq!(Tone::Fault.color(), ERROR);
        assert_eq!(Tone::Idle.color(), MUTED);
        assert!(!Tone::Idle.lit());
        assert!(Tone::OnAir.lit() && Tone::Pending.lit());
    }
}
