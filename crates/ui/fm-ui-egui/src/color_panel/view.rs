//! The colour panel's pictures: the graded preview with before/after
//! compare, the scopes strip, the looks gallery, and the LUT slot, plus the
//! textures they are drawn from.

use core::time::Duration;

use egui::{
    Color32, ColorImage, CornerRadius, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};
use fm_grade::{BUILTIN_PRESETS, Grade, GradeLut, LUT_URI_PREFIX};
use fm_ui_model::grade_editor::{
    BARS_75, CompareMode, GradeDocument, GradeParam, GradePreview, RgbaImage, ScopeConfig,
    ScopeGrid, ScopeKind, Scopes, chroma, compute_scopes,
};
use fm_ui_model::i18n::{Locale, Msg};

use super::controls::{self, SliderLayout};
use super::{COMPARE_SHORTCUT, ColorPanelState, PanelDrag, PanelRects, PreviewKey, text};
use crate::gating::Hint;
use crate::l10n;
use crate::theme::{
    AMBER, MONITOR_BLACK, accent_color, border_color, legible, line_width, muted_color,
    section_label, text_color,
};

/// Height of the scopes strip.
pub const SCOPES_HEIGHT: f32 = 170.0;

/// Textures and analysis of what the panel shows.
#[derive(Default)]
pub struct ViewCache {
    source: Option<(u64, TextureHandle)>,
    graded: Option<(PreviewKey, TextureHandle)>,
    scopes: Option<(PreviewKey, Scopes)>,
    scope_texture: Option<(PreviewKey, ScopeKind, TextureHandle)>,
    pub(super) thumbs: Vec<(&'static str, Grade, u64, TextureHandle)>,
    size: (u32, u32),
}

impl ViewCache {
    pub(crate) fn shown_key(&self) -> Option<PreviewKey> {
        self.graded.as_ref().map(|(key, _)| *key)
    }

    pub(crate) fn scopes(&self) -> Option<&Scopes> {
        self.scopes.as_ref().map(|(_, scopes)| scopes)
    }
}

fn color_image(image: &RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.pixels(),
    )
}

fn upload(
    context: &egui::Context,
    slot: Option<TextureHandle>,
    name: &str,
    image: ColorImage,
) -> TextureHandle {
    match slot {
        Some(mut handle) => {
            handle.set(image, TextureOptions::LINEAR);
            handle
        }
        None => context.load_texture(name, image, TextureOptions::LINEAR),
    }
}

/// Grades the preview when the draft, source, or LUTs changed: at once for
/// discrete edits, debounced latest-wins while a drag is running.
pub(super) fn refresh(
    context: &egui::Context,
    document: &GradeDocument,
    state: &mut ColorPanelState,
) {
    let Some(source) = state.preview.source().cloned() else {
        return;
    };
    let view = &mut state.view;
    if view
        .source
        .as_ref()
        .is_none_or(|(id, _)| *id != state.preview.source_id())
    {
        let handle = upload(
            context,
            view.source.take().map(|(_, handle)| handle),
            "fm-color-panel-source",
            color_image(&source),
        );
        view.source = Some((state.preview.source_id(), handle));
        view.size = (source.width(), source.height());
    }
    let now = Duration::from_secs_f64(context.input(|input| input.time).max(0.0));
    let key = state.key(document);
    if state.view.shown_key() != Some(key) {
        if state.drag.is_some() {
            state.scheduler.request(key, now);
        } else {
            state.scheduler.request_now(key);
        }
    }
    if let Some(key) = state.scheduler.poll(now)
        && let Ok(Some(graded)) = state.preview.graded(document.grade())
    {
        let view = &mut state.view;
        let handle = upload(
            context,
            view.graded.take().map(|(_, handle)| handle),
            "fm-color-panel-graded",
            color_image(&graded),
        );
        view.graded = Some((key, handle));
        view.scopes = Some((key, compute_scopes(&graded, ScopeConfig::default())));
    }
}

/// The rect of a `size` picture fitted inside `area`, centred.
fn fit(area: Rect, size: (u32, u32)) -> Rect {
    #[allow(clippy::cast_precision_loss)] // Preview sizes are small.
    let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let scale = (area.width() / width).min(area.height() / height);
    Rect::from_center_size(area.center(), vec2(width * scale, height * scale))
}

fn paint_tag(painter: &egui::Painter, at: Pos2, align: egui::Align2, text: &str, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(legible(9.5)), color);
    let rect = align.anchor_size(at, galley.size() + vec2(10.0, 4.0));
    painter.rect_filled(rect, CornerRadius::same(3), Color32::from_black_alpha(170));
    painter.galley(rect.min + vec2(5.0, 2.0), galley, color);
}

/// Compare tabs and the graded picture.
#[allow(clippy::too_many_lines)] // Three compare layouts and the wipe line.
pub(super) fn preview(
    ui: &mut Ui,
    l: Locale,
    document: &GradeDocument,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for mode in CompareMode::ALL {
            let response = controls::tab(
                ui,
                state.compare == mode,
                text::compare_name(l, mode),
                &l10n::with_key(l, text::compare_hover(l, mode), COMPARE_SHORTCUT),
            );
            rects.compare_tabs.push((mode, response.rect));
            if response.clicked() {
                state.compare = mode;
            }
        }
    });
    let (rect, response) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
    let _ = response.on_hover_text(format!(
        "{}\n{}",
        l.text(Msg::GradePreviewHover),
        text::compare_hover(l, state.compare)
    ));
    rects.preview = Some(rect);
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::same(crate::theme::RADIUS),
        MONITOR_BLACK,
    );
    let view = &state.view;
    let (Some((_, source)), Some((_, graded))) = (&view.source, &view.graded) else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            l.text(Msg::GradeNoPicture),
            FontId::proportional(legible(12.0)),
            muted_color(),
        );
        return;
    };
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let original = l.text(Msg::GradeOriginal);
    let after = l.text(Msg::GradeGraded);
    let tag = text_color();
    let mut wipe_line = None;
    match state.compare {
        CompareMode::Off => {
            let picture = fit(rect, view.size);
            painter.image(graded.id(), picture, full, Color32::WHITE);
        }
        CompareMode::Wipe => {
            let picture = fit(rect, view.size);
            let split = state.wipe.clamp(0.0, 1.0);
            let x = picture.min.x + picture.width() * split;
            painter.image(
                source.id(),
                Rect::from_min_max(picture.min, pos2(x, picture.max.y)),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(split, 1.0)),
                Color32::WHITE,
            );
            painter.image(
                graded.id(),
                Rect::from_min_max(pos2(x, picture.min.y), picture.max),
                Rect::from_min_max(pos2(split, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.line_segment(
                [pos2(x, picture.min.y), pos2(x, picture.max.y)],
                Stroke::new(2.0, Color32::WHITE),
            );
            paint_tag(
                &painter,
                picture.left_top() + vec2(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                original,
                tag,
            );
            paint_tag(
                &painter,
                picture.right_top() + vec2(-6.0, 6.0),
                egui::Align2::RIGHT_TOP,
                after,
                tag,
            );
            wipe_line = Some((x, picture));
        }
        CompareMode::SideBySide => {
            let half = vec2(rect.width() / 2.0 - 2.0, rect.height());
            let left = fit(Rect::from_min_size(rect.min, half), view.size);
            let right = fit(
                Rect::from_min_size(rect.min + vec2(rect.width() / 2.0 + 2.0, 0.0), half),
                view.size,
            );
            painter.image(source.id(), left, full, Color32::WHITE);
            painter.image(graded.id(), right, full, Color32::WHITE);
            paint_tag(
                &painter,
                left.left_top() + vec2(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                original,
                tag,
            );
            paint_tag(
                &painter,
                right.left_top() + vec2(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                after,
                tag,
            );
        }
    }
    if document.is_bypassed() {
        paint_tag(
            &painter,
            rect.right_bottom() + vec2(-6.0, -6.0),
            egui::Align2::RIGHT_BOTTOM,
            l.text(Msg::GradeBypassed),
            AMBER,
        );
    }
    if state.preview.lut_missing(document.grade()) {
        paint_tag(
            &painter,
            rect.left_bottom() + vec2(6.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            l.text(Msg::GradeLutMissing),
            AMBER,
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(crate::theme::RADIUS),
        Stroke::new(line_width(), border_color()),
        StrokeKind::Inside,
    );

    if let Some((x, picture)) = wipe_line {
        let handle =
            Rect::from_center_size(pos2(x, picture.center().y), vec2(14.0, picture.height()));
        let response = ui.interact(
            handle,
            ui.id().with("fm-color-wipe"),
            Sense::click_and_drag(),
        );
        response.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Slider, true, l.text(Msg::TransitionWipe))
        });
        let response = response.on_hover_text(l.text(Msg::GradeWipeHandleHover));
        rects.wipe_handle = Some(handle);
        if response.drag_started() {
            state.drag = Some(PanelDrag::Wipe);
        }
        if response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            state.wipe = ((pointer.x - picture.min.x) / picture.width()).clamp(0.0, 1.0);
        }
        if response.drag_stopped() {
            state.drag = None;
        }
        if controls::reset_click(&response) {
            state.wipe = 0.5;
        }
        let hot = response.hovered() || response.dragged();
        ui.painter().circle(
            pos2(x, picture.center().y),
            if hot { 9.0 } else { 7.5 },
            Color32::WHITE,
            Stroke::new(1.5, Color32::from_gray(30)),
        );
        // Left and right arrowheads on the knob.
        let knob = pos2(x, picture.center().y);
        for direction in [-1.0_f32, 1.0] {
            let tip = knob + vec2(direction * 5.0, 0.0);
            ui.painter().add(Shape::convex_polygon(
                vec![
                    tip,
                    tip - vec2(direction * 3.5, 3.0),
                    tip - vec2(direction * 3.5, -3.0),
                ],
                Color32::from_gray(30),
                Stroke::NONE,
            ));
        }
    }
}

/// Scope tabs and the selected scope.
pub(super) fn scopes(
    ui: &mut Ui,
    l: Locale,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(section_label(l.text(Msg::GradeScopes)))
            .on_hover_text(l.text(Msg::GradeScopesHover));
        ui.add_space(6.0);
        for kind in ScopeKind::ALL {
            let response = controls::tab(
                ui,
                state.scope == kind,
                text::scope_name(l, kind),
                &format!(
                    "{}\n{}",
                    text::scope_hover(l, kind),
                    l.text(Msg::GradeScopeShowHover)
                ),
            );
            rects.scope_tabs.push((kind, response.rect));
            if response.clicked() {
                state.scope = kind;
            }
        }
    });
    let (rect, response) = ui.allocate_exact_size(vec2(width, SCOPES_HEIGHT), Sense::hover());
    let _ = response.on_hover_text(text::scope_hover(l, state.scope));
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::same(crate::theme::RADIUS),
        Color32::from_gray(10),
    );
    let inner = rect.shrink(8.0);
    let kind = state.scope;
    let view = &mut state.view;
    let Some((key, scopes)) = &view.scopes else {
        return;
    };
    let graticule = Stroke::new(1.0, Color32::from_gray(48));
    match kind {
        ScopeKind::Histogram => paint_histogram(&painter, inner, scopes),
        ScopeKind::Waveform | ScopeKind::Parade | ScopeKind::Vectorscope => {
            if view
                .scope_texture
                .as_ref()
                .is_none_or(|(shown, shown_kind, _)| shown != key || *shown_kind != kind)
            {
                let image = scope_image(scopes, kind);
                let handle = upload(
                    ui.ctx(),
                    view.scope_texture.take().map(|(_, _, handle)| handle),
                    "fm-color-panel-scope",
                    image,
                );
                view.scope_texture = Some((*key, kind, handle));
            }
            let Some((_, _, texture)) = &view.scope_texture else {
                return;
            };
            let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            if kind == ScopeKind::Vectorscope {
                let square = Rect::from_center_size(inner.center(), Vec2::splat(inner.height()));
                paint_vectorscope_graticule(&painter, square, graticule);
                painter.image(texture.id(), square, full, Color32::WHITE);
            } else {
                for step in 0..=4_u8 {
                    let y = inner.max.y - inner.height() * f32::from(step) / 4.0;
                    painter.line_segment([pos2(inner.min.x, y), pos2(inner.max.x, y)], graticule);
                }
                painter.image(texture.id(), inner, full, Color32::WHITE);
            }
        }
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(crate::theme::RADIUS),
        Stroke::new(line_width(), border_color()),
        StrokeKind::Inside,
    );
}

/// Hit counts as glowing traces: square-root scaled so faint detail shows.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn trace(grid: &ScopeGrid, color: [u8; 3], out: &mut ColorImage, x0: usize) {
    let max = grid.max().max(1) as f32;
    let height = grid.height();
    for y in 0..height {
        for x in 0..grid.width() {
            let count = grid.get(x, y);
            if count == 0 {
                continue;
            }
            let level = (count as f32 / max).sqrt().max(0.25);
            let alpha = (level * 255.0) as u8;
            let row = height - 1 - y;
            out.pixels[row * out.size[0] + x0 + x] =
                Color32::from_rgba_unmultiplied(color[0], color[1], color[2], alpha);
        }
    }
}

fn scope_image(scopes: &Scopes, kind: ScopeKind) -> ColorImage {
    match kind {
        ScopeKind::Parade => {
            let width = scopes.parade[0].width();
            let gap = 4;
            let mut image = ColorImage::filled(
                [width * 3 + gap * 2, scopes.parade[0].height()],
                Color32::TRANSPARENT,
            );
            for (index, (grid, color)) in scopes
                .parade
                .iter()
                .zip([[255, 90, 90], [90, 235, 110], [110, 150, 255]])
                .enumerate()
            {
                trace(grid, color, &mut image, index * (width + gap));
            }
            image
        }
        ScopeKind::Vectorscope => {
            let grid = &scopes.vectorscope;
            let mut image = ColorImage::filled([grid.width(), grid.height()], Color32::TRANSPARENT);
            trace(grid, [210, 255, 200], &mut image, 0);
            image
        }
        _ => {
            let grid = &scopes.waveform;
            let mut image = ColorImage::filled([grid.width(), grid.height()], Color32::TRANSPARENT);
            trace(grid, [170, 255, 180], &mut image, 0);
            image
        }
    }
}

fn paint_vectorscope_graticule(painter: &egui::Painter, square: Rect, stroke: Stroke) {
    let centre = square.center();
    let radius = square.width() / 2.0;
    painter.circle_filled(centre, radius, Color32::from_gray(16));
    painter.circle_stroke(centre, radius, stroke);
    painter.circle_stroke(centre, radius * 0.5, stroke);
    painter.line_segment(
        [centre - vec2(radius, 0.0), centre + vec2(radius, 0.0)],
        stroke,
    );
    painter.line_segment(
        [centre - vec2(0.0, radius), centre + vec2(0.0, radius)],
        stroke,
    );
    // 75% bar targets (white sits at the centre).
    for rgb in &BARS_75[1..] {
        let [cb, cr] = chroma(rgb.map(|code| f32::from(code) / 255.0));
        let at = centre + vec2(cb, -cr) * 2.0 * radius;
        painter.rect_stroke(
            Rect::from_center_size(at, Vec2::splat(9.0)),
            CornerRadius::ZERO,
            Stroke::new(
                1.0,
                Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(0.9),
            ),
            StrokeKind::Middle,
        );
    }
}

#[allow(clippy::cast_precision_loss)] // Bin counts are small.
fn paint_histogram(painter: &egui::Painter, inner: Rect, scopes: &Scopes) {
    let histogram = &scopes.histogram;
    let max = histogram.max().max(1) as f32;
    let bins = histogram.luma.len().max(1);
    let x_of = |index: usize| inner.min.x + inner.width() * index as f32 / bins as f32;
    let height = |count: u32| inner.height() * (count as f32 / max).sqrt();
    let mut mesh = Mesh::default();
    for (index, &count) in histogram.luma.iter().enumerate() {
        let bar = Rect::from_min_max(
            pos2(x_of(index), inner.max.y - height(count)),
            pos2(x_of(index + 1), inner.max.y),
        );
        mesh.add_colored_rect(bar, Color32::from_white_alpha(40));
    }
    painter.add(Shape::mesh(mesh));
    for (bins, color) in [
        (&histogram.red, Color32::from_rgb(255, 90, 90)),
        (&histogram.green, Color32::from_rgb(90, 235, 110)),
        (&histogram.blue, Color32::from_rgb(110, 150, 255)),
    ] {
        let points: Vec<Pos2> = bins
            .iter()
            .enumerate()
            .map(|(index, &count)| {
                pos2(
                    f32::midpoint(x_of(index), x_of(index + 1)),
                    inner.max.y - height(count),
                )
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.5, color)));
    }
}

/// The looks gallery: one live thumbnail per built-in preset.
pub(super) fn looks(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    ui.label(section_label(l.text(Msg::GradeLooks)))
        .on_hover_text(l.text(Msg::GradeLooksHover));
    refresh_thumbs(ui.ctx(), document, &mut state.thumbs, &mut state.view);
    let current = document.matching_preset().map(|preset| preset.id);
    let columns = 3;
    let gap = 8.0;
    #[allow(clippy::cast_precision_loss)] // Three columns.
    let tile = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    let thumb = vec2(tile, tile * 9.0 / 16.0);
    let mut clicked = None;
    for row in BUILTIN_PRESETS.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for preset in row {
                let name = text::preset_name(l, preset.id).unwrap_or(preset.name);
                let lock = state.lock_reason;
                let (rect, response) = ui
                    .add_enabled_ui(lock.is_none(), |ui| {
                        ui.allocate_exact_size(thumb + vec2(0.0, 18.0), Sense::click())
                    })
                    .inner;
                response.widget_info(|| {
                    WidgetInfo::selected(
                        WidgetType::Button,
                        lock.is_none(),
                        current == Some(preset.id),
                        name,
                    )
                });
                let hover = format!(
                    "{}\n{}",
                    text::preset_hover(l, preset.id).unwrap_or(preset.name),
                    l.text(if current == Some(preset.id) {
                        Msg::GradeLookCurrent
                    } else {
                        Msg::GradeLookApply
                    })
                );
                let response = response.hint_in(l, &hover, lock.unwrap_or(""));
                rects.looks.push((preset.id, rect));
                if response.clicked() {
                    clicked = Some(preset);
                }
                let painter = ui.painter_at(rect);
                let picture = Rect::from_min_size(rect.min, thumb);
                painter.rect_filled(picture, CornerRadius::same(3), MONITOR_BLACK);
                if let Some((_, _, _, texture)) =
                    state.view.thumbs.iter().find(|(id, ..)| *id == preset.id)
                {
                    painter.image(
                        texture.id(),
                        picture,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                let selected = current == Some(preset.id);
                painter.rect_stroke(
                    picture,
                    CornerRadius::same(3),
                    Stroke::new(
                        if selected || response.hovered() {
                            2.0
                        } else {
                            line_width()
                        },
                        if selected {
                            accent_color()
                        } else if response.hovered() {
                            text_color()
                        } else {
                            border_color()
                        },
                    ),
                    StrokeKind::Inside,
                );
                painter.text(
                    pos2(rect.center().x, rect.max.y - 1.0),
                    egui::Align2::CENTER_BOTTOM,
                    name,
                    FontId::proportional(legible(10.5)),
                    if selected {
                        accent_color()
                    } else {
                        text_color()
                    },
                );
            }
        });
    }
    if let Some(preset) = clicked
        && let Err(error) = document.apply_preset(preset)
    {
        state.show_error(&error);
    }
}

/// Grades each look's thumbnail with the draft's LUT, re-uploading only
/// what changed.
fn refresh_thumbs(
    context: &egui::Context,
    document: &GradeDocument,
    thumbs: &mut GradePreview,
    view: &mut ViewCache,
) {
    let source = thumbs.source_id();
    for preset in &BUILTIN_PRESETS {
        let mut grade = preset.grade.clone();
        grade.lut.clone_from(&document.grade().lut);
        let index = view.thumbs.iter().position(|(id, ..)| *id == preset.id);
        if index.is_some_and(|index| {
            let (_, shown, shown_source, _) = &view.thumbs[index];
            *shown == grade && *shown_source == source
        }) {
            continue;
        }
        let Ok(Some(image)) = thumbs.graded(&grade) else {
            continue;
        };
        let previous = index.map(|index| view.thumbs.remove(index).3);
        let handle = upload(
            context,
            previous,
            &format!("fm-color-panel-look-{}", preset.id),
            color_image(&image),
        );
        view.thumbs.push((preset.id, grade, source, handle));
    }
}

/// The LUT picker, strength, and remove key.
/// The name `uri` is shown by: its LUT name, else its file name.
fn lut_label<'a>(state: &'a ColorPanelState, uri: &'a str) -> &'a str {
    state.lut_names.get(uri).map_or_else(
        || uri.strip_prefix(LUT_URI_PREFIX).unwrap_or(uri),
        String::as_str,
    )
}

pub(super) fn lut_slot(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    ui.label(section_label(l.text(Msg::GradeLut)))
        .on_hover_text(l.text(Msg::GradeLutHover));
    let current = document.grade().lut.clone();
    let shown = current.as_ref().map_or_else(
        || l.text(Msg::GradeLutNone).to_owned(),
        |lut| lut_label(state, &lut.asset_uri).to_owned(),
    );
    let choices = state.lut_choices.clone();
    let mut picked: Option<Option<String>> = None;
    let lock = state.lock_reason;
    let combo = ui
        .add_enabled_ui(!choices.is_empty() && lock.is_none(), |ui| {
            egui::ComboBox::from_id_salt("fm-color-lut")
                .selected_text(crate::theme::ellipsize(&shown, 28))
                .width(width - 8.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(current.is_none(), l.text(Msg::GradeLutNone))
                        .on_hover_text(l.text(Msg::GradeLutRemoveHover))
                        .clicked()
                    {
                        picked = Some(None);
                    }
                    for uri in &choices {
                        let selected = current.as_ref().is_some_and(|lut| lut.asset_uri == *uri);
                        let name = lut_label(state, uri);
                        if ui
                            .selectable_label(selected, name)
                            .on_hover_text(format!("{uri}\n{}", l.text(Msg::GradeLutPickHover)))
                            .clicked()
                        {
                            picked = Some(Some(uri.clone()));
                        }
                    }
                })
                .response
        })
        .inner;
    let _ = combo.hint_in(
        l,
        l.text(Msg::GradeLutPickHover),
        lock.unwrap_or_else(|| l.text(Msg::GradeReasonNoLuts)),
    );
    if let Some(choice) = picked {
        let strength = current.as_ref().map_or(1000, |lut| lut.strength_permille);
        let lut = choice.map(|uri| GradeLut::new(uri, strength));
        if let Err(error) = document.set_lut(lut) {
            state.show_error(&error);
        }
    }
    let no_lut = document.grade().lut.is_none();
    let reason = no_lut.then(|| l.text(Msg::GradeReasonNoLut));
    let strength = controls::slider(
        ui,
        l,
        document,
        state,
        GradeParam::LutStrength,
        (
            width,
            SliderLayout {
                label: 70.0,
                value: 44.0,
            },
        ),
        reason,
    );
    rects.sliders.push((GradeParam::LutStrength, strength.rect));
    let remove = ui
        .add_enabled(
            !no_lut && lock.is_none(),
            crate::theme::small_button(l.text(Msg::GradeLutRemove)),
        )
        .hint_in(
            l,
            l.text(Msg::GradeLutRemoveHover),
            lock.unwrap_or_else(|| l.text(Msg::GradeReasonNoLut)),
        );
    if remove.clicked()
        && let Err(error) = document.set_lut(None)
    {
        state.show_error(&error);
    }
    if state.preview.lut_missing(document.grade()) {
        ui.label(crate::theme::caption(l.text(Msg::GradeLutMissing)).color(AMBER));
    }
}
