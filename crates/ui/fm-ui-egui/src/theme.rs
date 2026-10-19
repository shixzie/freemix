//! Broadcast-console theme: palette, `egui` style, and small shared painters.
//!
//! One accent color marks focus and selection; red is reserved for Program
//! and on-air state, green for Preview, amber for warnings and pending state.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Response, RichText, Sense, Stroke,
    StrokeKind, TextStyle, Ui, Vec2, style::HandleShape,
};

use crate::{StudioConnectionStatus, TallyState};

/// Width of the keyboard focus ring, in points.
pub(crate) const FOCUS_RING_WIDTH: f32 = 2.0;
/// Width of the keyboard focus ring in the high-contrast theme.
pub(crate) const HIGH_CONTRAST_FOCUS_RING_WIDTH: f32 = 3.0;

/// Width of the keyboard focus ring in the theme in effect.
pub(crate) fn focus_ring_width() -> f32 {
    if high_contrast() {
        HIGH_CONTRAST_FOCUS_RING_WIDTH
    } else {
        FOCUS_RING_WIDTH
    }
}

/// Width of panel, card, key, and field outlines in the theme in effect:
/// one pixel normally, two in high contrast so every boundary is visible.
pub(crate) fn line_width() -> f32 {
    if high_contrast() { 2.0 } else { 1.0 }
}

/// Points added to small text (under 12 pt) in the high-contrast theme.
pub(crate) const HIGH_CONTRAST_TEXT_BOOST: f32 = 1.5;

/// The size to draw small text of nominal `size` at: captions, field
/// labels, chips, and badges grow by [`HIGH_CONTRAST_TEXT_BOOST`] in high
/// contrast; body text and larger sizes are unchanged.
pub(crate) fn legible(size: f32) -> f32 {
    if high_contrast() && size < 12.0 {
        size + HIGH_CONTRAST_TEXT_BOOST
    } else {
        size
    }
}

/// `color` faded by `factor` for secondary marks in the standard theme. In
/// high contrast palette colours are never faded, so dim legends (tile
/// ordinals, `no thumbnail`, hints) keep the palette's contrast.
pub(crate) fn faded(color: Color32, factor: f32) -> Color32 {
    if high_contrast() {
        color
    } else {
        color.gamma_multiply(factor)
    }
}

/// Outlines the widget holding keyboard focus with the accent ring, above
/// whatever the widget painted itself, so focus is visible on every button
/// and field, including custom-painted console keys whose own fill and
/// stroke hide egui's focused style. Buttons only take focus from the
/// keyboard (Tab), so a pointer click never leaves a ring behind.
pub(crate) fn paint_focus_ring(context: &egui::Context) {
    let Some(id) = context.memory(egui::Memory::focused) else {
        return;
    };
    let Some(response) = context.read_response(id) else {
        return;
    };
    let rect = response.interact_rect;
    if !rect.is_positive() {
        return;
    }
    let order = response.layer_id.order.max(egui::Order::Foreground);
    let painter = context.layer_painter(egui::LayerId::new(
        order,
        egui::Id::new("studio-focus-ring"),
    ));
    painter.rect_stroke(
        rect.expand(1.0),
        CornerRadius::same(RADIUS),
        Stroke::new(focus_ring_width(), accent_color()),
        StrokeKind::Outside,
    );
}

/// The neutral console colours: surfaces, borders, text, and the accent.
/// Tally and status hues ([`PROGRAM`], [`PREVIEW`], [`AMBER`], [`ERROR`])
/// are shared by every palette so their meaning never changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Palette {
    /// Application background behind every panel.
    pub(crate) graphite: Color32,
    /// Raised card surface inside panels.
    pub(crate) raised: Color32,
    /// Side, top, and status bar surface.
    pub(crate) panel: Color32,
    /// Idle widget surface.
    pub(crate) widget: Color32,
    /// Hovered widget surface.
    pub(crate) widget_hover: Color32,
    /// Pressed widget surface.
    pub(crate) widget_active: Color32,
    /// Subtle one-pixel border.
    pub(crate) border: Color32,
    /// Stronger border for hovered or emphasized frames.
    pub(crate) border_strong: Color32,
    /// Primary text.
    pub(crate) text: Color32,
    /// Secondary text such as section labels.
    pub(crate) text_dim: Color32,
    /// Tertiary text and inactive state.
    pub(crate) muted: Color32,
    /// The single interface accent, also the keyboard focus ring.
    pub(crate) accent: Color32,
    /// Text field and well background.
    pub(crate) extreme_bg: Color32,
    /// Faint striping background.
    pub(crate) faint_bg: Color32,
}

/// The standard graphite console palette.
pub(crate) const STANDARD_PALETTE: Palette = Palette {
    graphite: Color32::from_rgb(12, 13, 15),
    raised: Color32::from_rgb(22, 24, 28),
    panel: Color32::from_rgb(17, 19, 22),
    widget: Color32::from_rgb(34, 38, 44),
    widget_hover: Color32::from_rgb(46, 51, 59),
    widget_active: Color32::from_rgb(58, 64, 74),
    border: Color32::from_rgb(38, 42, 49),
    border_strong: Color32::from_rgb(64, 71, 82),
    text: Color32::from_rgb(222, 226, 230),
    text_dim: Color32::from_rgb(150, 158, 168),
    muted: Color32::from_rgb(108, 116, 126),
    accent: Color32::from_rgb(66, 150, 250),
    extreme_bg: Color32::from_rgb(8, 9, 11),
    faint_bg: Color32::from_rgb(20, 22, 26),
};

/// The high-contrast palette, an accessibility mode rather than a tint:
/// pure black surfaces (panels, cards, keys, and fields are told apart by
/// their outlines, not by shades of grey), light-grey outlines drawn
/// [`line_width`] wide, near-white text and muted text, and a bright accent
/// for selection and the wider focus ring. Every text colour reaches the
/// WCAG AA ratio (4.5:1) and every outline 3:1 on every surface
/// (unit-tested).
pub(crate) const HIGH_CONTRAST_PALETTE: Palette = Palette {
    graphite: Color32::from_rgb(0, 0, 0),
    raised: Color32::from_rgb(0, 0, 0),
    panel: Color32::from_rgb(0, 0, 0),
    widget: Color32::from_rgb(0, 0, 0),
    widget_hover: Color32::from_rgb(40, 40, 40),
    widget_active: Color32::from_rgb(64, 64, 64),
    border: Color32::from_rgb(200, 200, 200),
    border_strong: Color32::from_rgb(245, 245, 245),
    text: Color32::from_rgb(255, 255, 255),
    text_dim: Color32::from_rgb(242, 242, 242),
    muted: Color32::from_rgb(214, 214, 214),
    accent: Color32::from_rgb(96, 208, 255),
    extreme_bg: Color32::from_rgb(0, 0, 0),
    faint_bg: Color32::from_rgb(16, 16, 16),
};

thread_local! {
    /// Whether this thread's console draws with the high-contrast palette.
    /// The UI is drawn on one thread; tests each get their own.
    static HIGH_CONTRAST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Selects the palette for this thread's subsequent drawing.
pub(crate) fn set_high_contrast(high_contrast: bool) {
    HIGH_CONTRAST.with(|cell| cell.set(high_contrast));
}

/// Whether this thread draws with the high-contrast theme.
pub(crate) fn high_contrast() -> bool {
    HIGH_CONTRAST.with(std::cell::Cell::get)
}

/// The palette in effect.
pub(crate) fn palette() -> &'static Palette {
    if high_contrast() {
        &HIGH_CONTRAST_PALETTE
    } else {
        &STANDARD_PALETTE
    }
}

/// Application background behind every panel.
pub(crate) fn graphite_color() -> Color32 {
    palette().graphite
}
/// Raised card surface inside panels.
pub(crate) fn raised_color() -> Color32 {
    palette().raised
}
/// Side, top, and status bar surface.
pub(crate) fn panel_color() -> Color32 {
    palette().panel
}
/// Idle widget surface.
pub(crate) fn widget_color() -> Color32 {
    palette().widget
}
/// Hovered widget surface.
pub(crate) fn widget_hover_color() -> Color32 {
    palette().widget_hover
}
/// Pressed widget surface.
pub(crate) fn widget_active_color() -> Color32 {
    palette().widget_active
}
/// Subtle one-pixel border.
pub(crate) fn border_color() -> Color32 {
    palette().border
}
/// Stronger border for hovered or emphasized frames.
pub(crate) fn border_strong_color() -> Color32 {
    palette().border_strong
}
/// Primary text.
pub(crate) fn text_color() -> Color32 {
    palette().text
}
/// Secondary text such as section labels.
pub(crate) fn text_dim_color() -> Color32 {
    palette().text_dim
}
/// Tertiary text and inactive state.
pub(crate) fn muted_color() -> Color32 {
    palette().muted
}
/// The single interface accent.
pub(crate) fn accent_color() -> Color32 {
    palette().accent
}

/// Monitor and thumbnail black.
pub(crate) const MONITOR_BLACK: Color32 = Color32::from_rgb(5, 6, 7);
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

/// Applies the console theme (standard or high contrast) when it changes and
/// keeps the dark theme pinned.
pub(crate) fn apply_console_visuals(context: &egui::Context, high_contrast: bool) {
    set_high_contrast(high_contrast);
    let palette = palette();
    let applied = context.global_style().visuals.panel_fill == palette.panel
        && context.global_style().visuals.window_stroke.color == palette.border_strong
        && context.options(|options| options.theme_preference == egui::ThemePreference::Dark);
    if applied {
        return;
    }
    context.set_theme(egui::ThemePreference::Dark);
    context.all_styles_mut(console_style);
}

fn console_style(style: &mut egui::Style) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = panel_color();
    visuals.window_fill = raised_color();
    visuals.window_stroke = Stroke::new(line_width(), border_strong_color());
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);
    visuals.extreme_bg_color = palette().extreme_bg;
    visuals.faint_bg_color = palette().faint_bg;
    visuals.code_bg_color = widget_color();
    visuals.warn_fg_color = AMBER;
    visuals.error_fg_color = ERROR;
    visuals.hyperlink_color = accent_color();
    visuals.selection.bg_fill = ACCENT_FILL;
    visuals.selection.stroke = Stroke::new(line_width(), accent_color());
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = HandleShape::Rect { aspect_ratio: 0.6 };
    // Disabled controls stay legible in high contrast.
    visuals.disabled_alpha = if high_contrast() { 0.6 } else { 0.45 };
    visuals.striped = false;
    visuals.indent_has_left_vline = false;

    let radius = CornerRadius::same(RADIUS);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = raised_color();
    widgets.noninteractive.weak_bg_fill = raised_color();
    widgets.noninteractive.bg_stroke = Stroke::new(line_width(), border_color());
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, text_dim_color());
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = widget_color();
    widgets.inactive.weak_bg_fill = widget_color();
    widgets.inactive.bg_stroke = Stroke::new(line_width(), border_color());
    widgets.inactive.fg_stroke = Stroke::new(1.0, text_color());
    widgets.inactive.corner_radius = radius;
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = widget_hover_color();
    widgets.hovered.weak_bg_fill = widget_hover_color();
    widgets.hovered.bg_stroke = Stroke::new(line_width(), border_strong_color());
    widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = widget_active_color();
    widgets.active.weak_bg_fill = widget_active_color();
    widgets.active.bg_stroke = Stroke::new(line_width(), accent_color());
    widgets.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    widgets.active.corner_radius = radius;
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = widget_hover_color();
    widgets.open.weak_bg_fill = widget_hover_color();
    widgets.open.bg_stroke = Stroke::new(line_width(), accent_color());
    widgets.open.fg_stroke = Stroke::new(1.0, text_color());
    widgets.open.corner_radius = radius;

    style.visuals = visuals;
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(legible(10.0), FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(
                if high_contrast() { 13.0 } else { 12.5 },
                FontFamily::Proportional,
            ),
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
    // Floating bars that stay visible (dimmed) while idle and reserve their
    // own lane: an operator must see at a glance that the input bank, the
    // side panel, or the mixer holds more than fits, not discover it by
    // hovering.
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 8.0,
        floating_width: 4.0,
        floating_allocated_width: 6.0,
        dormant_background_opacity: 0.35,
        dormant_handle_opacity: 0.8,
        ..egui::style::ScrollStyle::floating()
    };
    style.animation_time = 0.08;
}

/// Uppercase, letter-spaced section label (the console's "small caps").
pub(crate) fn section_label(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(legible(10.5))
        .strong()
        .extra_letter_spacing(1.2)
        .color(text_dim_color())
}

/// Nominal text size of console captions and hint lines under a control.
pub(crate) const CAPTION_SIZE: f32 = 10.5;
/// Nominal text size of small field labels beside a control (`POS`,
/// `DURATION`).
pub(crate) const FIELD_LABEL_SIZE: f32 = 10.0;

/// A small dim field label beside a control, letter-spaced like a console
/// legend.
pub(crate) fn field_label(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(legible(FIELD_LABEL_SIZE))
        .extra_letter_spacing(0.6)
        .color(muted_color())
}

/// A muted caption line that explains a panel or control.
pub(crate) fn caption(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .size(legible(CAPTION_SIZE))
        .color(muted_color())
}

/// Monospace numeric text for timecode, revisions, and counters.
pub(crate) fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

/// A raised card with a subtle border.
pub(crate) fn card() -> Frame {
    Frame::new()
        .fill(raised_color())
        .stroke(Stroke::new(line_width(), border_color()))
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
/// The width [`pill`] takes for `text`.
pub(crate) fn pill_width(ui: &Ui, text: &str) -> f32 {
    let font = FontId::new(legible(10.0), FontFamily::Proportional);
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
        + 14.0
}

pub(crate) fn pill(ui: &mut Ui, text: &str, color: Color32, filled: bool) -> Response {
    let font = FontId::new(legible(10.0), FontFamily::Proportional);
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
    pub(crate) fn color(self) -> Color32 {
        match self {
            Self::Idle => muted_color(),
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
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(line_width().min(1.5), border_strong_color()),
        );
    }
}

/// Paints a padlock glyph centered on `center`; `scale` 1.0 is 11 x 15 px.
/// An open padlock lifts and offsets its shackle.
pub(crate) fn paint_lock_glyph(
    painter: &egui::Painter,
    center: egui::Pos2,
    scale: f32,
    color: Color32,
    locked: bool,
) {
    let body = egui::Rect::from_center_size(
        center + Vec2::new(0.0, 3.0 * scale),
        Vec2::new(11.0 * scale, 8.0 * scale),
    );
    let lift = if locked { 0.0 } else { 3.0 * scale };
    let shackle = egui::Rect::from_min_max(
        egui::pos2(
            body.left() + 2.0 * scale + lift,
            body.top() - 6.0 * scale - lift,
        ),
        egui::pos2(
            body.right() - 2.0 * scale + lift,
            body.top() + 3.0 * scale - lift,
        ),
    );
    painter.rect_stroke(
        shackle,
        CornerRadius::same(3),
        Stroke::new(1.6 * scale.max(0.75), color),
        StrokeKind::Middle,
    );
    painter.rect_filled(body, CornerRadius::same(1), color);
}

/// Size of the small padlock badge shown on locked editable controls.
pub(crate) const LOCK_BADGE_SIZE: Vec2 = Vec2::new(20.0, 18.0);

/// Paints the small amber padlock badge into `rect`.
pub(crate) fn paint_lock_badge(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::same(3), AMBER.gamma_multiply(0.14));
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(1.0, AMBER.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    paint_lock_glyph(painter, rect.center(), 0.72, AMBER, true);
}

/// A small amber padlock badge marking a control blocked by an edit lock.
pub(crate) fn lock_badge(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(LOCK_BADGE_SIZE, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_lock_badge(ui.painter(), rect);
    }
    response
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
    // Chips carry catalog status words (`RECORDING`, `LOCKED`, ...) as
    // English; they are worded in the console's language here.
    let galley = elided(
        painter,
        crate::l10n::locale(painter.ctx()).tr(text),
        FontId::new(
            legible(9.5).min(rect.height() - 4.0),
            FontFamily::Proportional,
        ),
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
                .size(legible(11.0))
                .strong()
                .extra_letter_spacing(1.0)
                .color(text_dim_color()),
        );
        ui.add(
            egui::Label::new(
                RichText::new(detail)
                    .size(legible(10.5))
                    .color(muted_color()),
            )
            .wrap(),
        );
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
            .color(text_color()),
    )
    .fill(fill)
    .stroke(Stroke::new(line_width(), stroke))
    .corner_radius(CornerRadius::same(RADIUS))
    .min_size(size)
}

/// Smallest side of a pointer target, in points (WCAG 2.5.8 target size).
pub(crate) const MIN_TARGET: f32 = 24.0;

/// The `×` close key of dialogs and toasts: a 24 x 24 target that shows its
/// frame on hover and keyboard focus.
pub(crate) fn close_key(size: f32) -> egui::Button<'static> {
    egui::Button::new(RichText::new("×").size(size).color(text_dim_color()))
        .frame_when_inactive(false)
        .min_size(Vec2::splat(MIN_TARGET))
}

/// A progressive-disclosure key: a right-pointing triangle and `label` while
/// the advanced controls it governs are hidden, a down-pointing one while
/// they show. Flips `open` on a click and explains itself on hover (`show` /
/// `hide` say what a click does).
pub(crate) fn disclosure(
    ui: &mut Ui,
    open: &mut bool,
    label: &str,
    (show, hide): (&str, &str),
) -> Response {
    // The triangle is painted rather than typed: the console fonts have no
    // reliable glyph for it.
    let color = if *open {
        text_color()
    } else {
        text_dim_color()
    };
    let response = ui
        .add(
            egui::Button::new(
                RichText::new(format!("    {label}"))
                    .size(legible(9.5))
                    .strong()
                    .extra_letter_spacing(0.6)
                    .color(color),
            )
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(line_width(), border_color()))
            .min_size(Vec2::new(0.0, 18.0)),
        )
        .on_hover_text(if *open { hide } else { show });
    let centre = response.rect.left_center() + Vec2::new(10.0, 0.0);
    let half = 3.5;
    let points = if *open {
        vec![
            centre + Vec2::new(-half, -half * 0.6),
            centre + Vec2::new(half, -half * 0.6),
            centre + Vec2::new(0.0, half * 0.8),
        ]
    } else {
        vec![
            centre + Vec2::new(-half * 0.6, -half),
            centre + Vec2::new(half * 0.8, 0.0),
            centre + Vec2::new(-half * 0.6, half),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    if response.clicked() {
        *open = !*open;
    }
    response
}

/// Compact toolbar button.
pub(crate) fn small_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(legible(11.0))).min_size(Vec2::new(0.0, 20.0))
}

/// A painted key glyph. The console fonts have no reliable `▲`, `▼`, `▶`,
/// or `✕`, so list keys paint their symbol instead of typing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyIcon {
    /// Move the row earlier: an up-pointing triangle.
    Up,
    /// Move the row later: a down-pointing triangle.
    Down,
    /// Remove the row: a diagonal cross.
    Remove,
    /// Put the row on standby: a filled dot.
    Dot,
    /// Fire or play the row now, or show what is folded under it: a
    /// right-pointing triangle.
    Play,
}

/// Paints `icon` centred in `rect`, about `extent` points across.
pub(crate) fn paint_key_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: KeyIcon,
    extent: f32,
    color: Color32,
) {
    let centre = rect.center();
    let half = extent / 2.0;
    match icon {
        KeyIcon::Up | KeyIcon::Down => {
            let sign = if icon == KeyIcon::Up { 1.0 } else { -1.0 };
            let points = vec![
                centre + Vec2::new(-half, sign * half * 0.6),
                centre + Vec2::new(half, sign * half * 0.6),
                centre + Vec2::new(0.0, -sign * half * 0.75),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        KeyIcon::Dot => {
            painter.circle_filled(centre, half * 0.55, color);
        }
        KeyIcon::Play => {
            let points = vec![
                centre + Vec2::new(-half * 0.6, -half * 0.75),
                centre + Vec2::new(half * 0.75, 0.0),
                centre + Vec2::new(-half * 0.6, half * 0.75),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        KeyIcon::Remove => {
            let stroke = Stroke::new(1.6, color);
            let d = half * 0.75;
            painter.line_segment(
                [centre + Vec2::new(-d, -d), centre + Vec2::new(d, d)],
                stroke,
            );
            painter.line_segment(
                [centre + Vec2::new(d, -d), centre + Vec2::new(-d, d)],
                stroke,
            );
        }
    }
}

/// Adds `button` (built with empty text) with `icon` painted over it,
/// named `label` for assistive technology and the hover audit.
pub(crate) fn add_icon_button(
    ui: &mut Ui,
    enabled: bool,
    button: egui::Button<'_>,
    icon: KeyIcon,
    label: &str,
) -> Response {
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let color = if enabled {
        ui.style().interact(&response).text_color()
    } else {
        ui.visuals()
            .widgets
            .noninteractive
            .fg_stroke
            .color
            .gamma_multiply(0.45)
    };
    let extent = (response.rect.height() * 0.42).clamp(7.0, 11.0);
    paint_key_icon(ui.painter(), response.rect, icon, extent, color);
    response
}

pub(crate) fn routing_color(desired: fm_types::InputId, realized: fm_types::InputId) -> Color32 {
    if desired == realized {
        muted_color()
    } else {
        AMBER
    }
}

pub(crate) fn connection_color(status: StudioConnectionStatus) -> Color32 {
    match status {
        StudioConnectionStatus::Ready => PREVIEW,
        StudioConnectionStatus::Failed | StudioConnectionStatus::ProtocolMismatch => ERROR,
        StudioConnectionStatus::Disconnected => muted_color(),
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => AMBER,
    }
}

/// Tally border color: realized roles win over desired-only roles.
pub(crate) fn tally_color(tally: TallyState) -> Color32 {
    if tally.is_realized_program() {
        PROGRAM
    } else if tally.is_realized_preview() {
        PREVIEW
    } else if tally.is_desired_program() {
        Color32::from_rgb(200, 96, 66)
    } else if tally.is_desired_preview() {
        Color32::from_rgb(92, 170, 118)
    } else {
        border_strong_color()
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

    /// WCAG 2.x relative luminance of an sRGB colour.
    fn relative_luminance(color: Color32) -> f64 {
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    /// WCAG 2.x contrast ratio between two colours (1 to 21).
    fn contrast_ratio(a: Color32, b: Color32) -> f64 {
        let (a, b) = (relative_luminance(a), relative_luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn surfaces(palette: &Palette) -> [(&'static str, Color32); 9] {
        [
            ("graphite", palette.graphite),
            ("raised", palette.raised),
            ("panel", palette.panel),
            ("widget", palette.widget),
            ("widget_hover", palette.widget_hover),
            ("widget_active", palette.widget_active),
            ("extreme_bg", palette.extreme_bg),
            ("faint_bg", palette.faint_bg),
            ("monitor_black", MONITOR_BLACK),
        ]
    }

    #[test]
    fn contrast_ratio_matches_the_wcag_reference_points() {
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(Color32::BLACK, Color32::BLACK) - 1.0).abs() < 1e-9);
        // #767676 on white is the classic 4.54:1 AA reference.
        let grey = Color32::from_rgb(0x76, 0x76, 0x76);
        assert!((contrast_ratio(grey, Color32::WHITE) - 4.54).abs() < 0.01);
    }

    #[test]
    fn high_contrast_text_meets_wcag_aa_on_every_surface() {
        let palette = &HIGH_CONTRAST_PALETTE;
        for (text_name, text) in [
            ("text", palette.text),
            ("text_dim", palette.text_dim),
            ("muted", palette.muted),
            ("accent", palette.accent),
        ] {
            for (surface_name, surface) in surfaces(palette) {
                let ratio = contrast_ratio(text, surface);
                assert!(
                    ratio >= 4.5,
                    "{text_name} on {surface_name} is {ratio:.2}:1, below WCAG AA 4.5:1"
                );
            }
        }
        // Control outlines and the focus ring reach the 3:1 non-text ratio
        // against the surfaces they sit on.
        for (line_name, line) in [
            ("border", palette.border),
            ("border_strong", palette.border_strong),
            ("accent", palette.accent),
        ] {
            for (surface_name, surface) in surfaces(palette) {
                let ratio = contrast_ratio(line, surface);
                assert!(
                    ratio >= 3.0,
                    "{line_name} on {surface_name} is {ratio:.2}:1"
                );
            }
        }
        // Tally hues are shared by both palettes and stay distinguishable
        // (3:1) on the high-contrast panel surfaces.
        for tally in [PROGRAM, PREVIEW, AMBER, ERROR] {
            for surface in [palette.graphite, palette.panel, palette.raised] {
                assert!(contrast_ratio(tally, surface) >= 3.0, "{tally:?}");
            }
        }
        // Every high-contrast text colour beats its standard counterpart.
        let standard = &STANDARD_PALETTE;
        for (high, normal, surface) in [
            (palette.text, standard.text, palette.raised),
            (palette.text_dim, standard.text_dim, palette.raised),
            (palette.muted, standard.muted, palette.raised),
        ] {
            assert!(contrast_ratio(high, surface) > contrast_ratio(normal, standard.raised));
        }
    }

    #[test]
    fn high_contrast_is_a_distinct_accessibility_mode() {
        let palette = &HIGH_CONTRAST_PALETTE;
        let standard = &STANDARD_PALETTE;
        // Panels, cards, keys, and fields are pure black; they are told
        // apart by outlines.
        for (name, surface) in [
            ("graphite", palette.graphite),
            ("raised", palette.raised),
            ("panel", palette.panel),
            ("widget", palette.widget),
            ("extreme_bg", palette.extreme_bg),
        ] {
            assert_eq!(surface, Color32::BLACK, "{name}");
        }
        // Outlines are light: at least 3:1 against every surface (WCAG
        // non-text contrast), and far above the standard theme's subtle
        // borders on pure black.
        for (line_name, line) in [
            ("border", palette.border),
            ("border_strong", palette.border_strong),
        ] {
            for (surface_name, surface) in surfaces(palette) {
                let ratio = contrast_ratio(line, surface);
                assert!(
                    ratio >= 3.0,
                    "{line_name} on {surface_name} is {ratio:.2}:1"
                );
            }
            assert!(contrast_ratio(line, Color32::BLACK) >= 10.0, "{line_name}");
        }
        assert!(contrast_ratio(standard.border, standard.raised) < 1.5);
        // Muted text, the dimmest text colour, still reads at WCAG AA on
        // every surface, and is near-white rather than grey on black.
        for (surface_name, surface) in surfaces(palette) {
            let ratio = contrast_ratio(palette.muted, surface);
            assert!(ratio >= 4.5, "muted on {surface_name} is {ratio:.2}:1");
        }
        assert!(contrast_ratio(palette.muted, Color32::BLACK) >= 12.0);
        assert!(contrast_ratio(palette.text_dim, Color32::BLACK) >= 17.0);

        // Outlines, focus ring, and small text grow; body text does not.
        set_high_contrast(false);
        let (line, ring) = (line_width(), focus_ring_width());
        let (caption, label, chip) = (
            legible(CAPTION_SIZE),
            legible(FIELD_LABEL_SIZE),
            legible(9.5),
        );
        assert_eq!(
            (caption, label, chip),
            (CAPTION_SIZE, FIELD_LABEL_SIZE, 9.5)
        );
        assert_eq!(faded(palette.muted, 0.5), palette.muted.gamma_multiply(0.5));
        set_high_contrast(true);
        assert!(line_width() >= 2.0 && line_width() > line);
        assert!(focus_ring_width() > ring);
        assert!(legible(CAPTION_SIZE) >= caption + 1.0);
        assert!(legible(FIELD_LABEL_SIZE) >= label + 1.0);
        assert!(legible(9.5) >= chip + 1.0);
        assert!((legible(13.0) - 13.0).abs() < f32::EPSILON);
        assert_eq!(faded(palette.muted, 0.5), palette.muted);
        set_high_contrast(false);
    }

    #[test]
    fn the_palette_switch_is_per_thread_and_keeps_tally_hues() {
        set_high_contrast(false);
        assert_eq!(text_color(), STANDARD_PALETTE.text);
        set_high_contrast(true);
        assert_eq!(text_color(), HIGH_CONTRAST_PALETTE.text);
        assert_eq!(muted_color(), HIGH_CONTRAST_PALETTE.muted);
        assert_eq!(Tone::OnAir.color(), PROGRAM);
        assert_eq!(Tone::Ready.color(), PREVIEW);
        assert_eq!(Tone::Idle.color(), HIGH_CONTRAST_PALETTE.muted);
        std::thread::spawn(|| assert_eq!(text_color(), STANDARD_PALETTE.text))
            .join()
            .unwrap();
        set_high_contrast(false);
    }

    #[test]
    fn tones_map_to_the_console_palette() {
        assert_eq!(Tone::OnAir.color(), PROGRAM);
        assert_eq!(Tone::Ready.color(), PREVIEW);
        assert_eq!(Tone::Pending.color(), AMBER);
        assert_eq!(Tone::Fault.color(), ERROR);
        assert_eq!(Tone::Idle.color(), muted_color());
        assert!(!Tone::Idle.lit());
        assert!(Tone::OnAir.lit() && Tone::Pending.lit());
    }
}
