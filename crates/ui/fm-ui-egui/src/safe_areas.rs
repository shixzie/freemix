//! Operator safe-area and aspect-ratio guides (SW-014).
//!
//! The guides are an operator-view preference: Program, Preview, recordings,
//! and streams never render them. Action and title safe are the centered 93%
//! and 90% SMPTE rectangles; the center cuts are the largest centered
//! rectangles of one aspect ratio, so a 9:16 cut on a landscape canvas
//! outlines the portrait strip a fill crop keeps.
//!
//! All geometry is derived with truncating integer division from the canvas
//! dimensions and then normalized, so the same preset lands on the same
//! relative place at any console size.

use egui::{Color32, Painter, Rect, Stroke, Vec2, pos2};

use crate::theme::{AMBER, PROGRAM, border_color};

/// Which guides the operator asked to see.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeAreaGuides {
    pub action: bool,
    pub title: bool,
    pub center_4_3: bool,
    pub center_16_9: bool,
    pub center_9_16: bool,
}

impl Default for SafeAreaGuides {
    /// Title safe on, matching the guide the monitor always drew.
    fn default() -> Self {
        Self {
            action: false,
            title: true,
            center_4_3: false,
            center_16_9: false,
            center_9_16: false,
        }
    }
}

impl SafeAreaGuides {
    /// Whether no guide is selected.
    #[must_use]
    pub(crate) const fn is_empty(self) -> bool {
        !(self.action || self.title || self.center_4_3 || self.center_16_9 || self.center_9_16)
    }

    /// Applies one `safe_areas.<name> = on|off` preference line; `false` for
    /// a key this struct does not own.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        let Some(name) = key.strip_prefix("safe_areas.") else {
            return false;
        };
        let on = match value {
            "on" => true,
            "off" => false,
            _ => return false,
        };
        let slot = match name {
            "action" => &mut self.action,
            "title" => &mut self.title,
            "center_4_3" => &mut self.center_4_3,
            "center_16_9" => &mut self.center_16_9,
            "center_9_16" => &mut self.center_9_16,
            _ => return false,
        };
        *slot = on;
        true
    }

    /// The preference lines that reproduce this selection.
    pub fn preference_entries(self) -> Vec<(&'static str, &'static str)> {
        [
            ("safe_areas.action", self.action),
            ("safe_areas.title", self.title),
            ("safe_areas.center_4_3", self.center_4_3),
            ("safe_areas.center_16_9", self.center_16_9),
            ("safe_areas.center_9_16", self.center_9_16),
        ]
        .into_iter()
        .map(|(key, on)| (key, if on { "on" } else { "off" }))
        .collect()
    }

    /// The normalized rectangles to draw on a `width` x `height` canvas, in
    /// operator order: action, title, then the center cuts.
    #[must_use]
    pub(crate) fn rects(self, width: u32, height: u32) -> Vec<(SafeAreaKind, [f32; 4])> {
        let mut rects = Vec::new();
        if width == 0 || height == 0 {
            return rects;
        }
        if self.action {
            rects.push((SafeAreaKind::Action, percent(width, height, 93)));
        }
        if self.title {
            rects.push((SafeAreaKind::Title, percent(width, height, 90)));
        }
        for (enabled, aspect_width, aspect_height) in [
            (self.center_4_3, 4, 3),
            (self.center_16_9, 16, 9),
            (self.center_9_16, 9, 16),
        ] {
            if enabled {
                rects.push((
                    SafeAreaKind::CenterCut,
                    center_cut(width, height, aspect_width, aspect_height),
                ));
            }
        }
        rects
    }
}

/// Which style a guide is drawn in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SafeAreaKind {
    Action,
    Title,
    CenterCut,
}

/// A centered rectangle at `percent` of each canvas dimension, normalized.
fn percent(width: u32, height: u32, percent: u32) -> [f32; 4] {
    let guide_width = width * percent / 100;
    let guide_height = height * percent / 100;
    normalize(
        width,
        height,
        (width - guide_width) / 2,
        (height - guide_height) / 2,
        guide_width,
        guide_height,
    )
}

/// The largest centered rectangle with one aspect ratio, normalized.
fn center_cut(width: u32, height: u32, aspect_width: u32, aspect_height: u32) -> [f32; 4] {
    let wide = u128::from(width) * u128::from(aspect_height);
    let tall = u128::from(height) * u128::from(aspect_width);
    let (cut_width, cut_height) = if wide > tall {
        (
            rounded_ratio(height, aspect_width, aspect_height, width),
            height,
        )
    } else if wide < tall {
        (
            width,
            rounded_ratio(width, aspect_height, aspect_width, height),
        )
    } else {
        (width, height)
    };
    normalize(
        width,
        height,
        (width - cut_width) / 2,
        (height - cut_height) / 2,
        cut_width,
        cut_height,
    )
}

/// `value * numerator / denominator`, rounded half up and clamped into
/// `1..=maximum`.
fn rounded_ratio(value: u32, numerator: u32, denominator: u32, maximum: u32) -> u32 {
    let numerator = u128::from(value) * u128::from(numerator);
    let denominator = u128::from(denominator);
    let rounded = (2 * numerator + denominator) / (2 * denominator);
    u32::try_from(rounded).map_or(maximum, |rounded| rounded.clamp(1, maximum))
}

#[allow(clippy::cast_precision_loss)] // normalized guide geometry needs no more
fn normalize(width: u32, height: u32, x: u32, y: u32, w: u32, h: u32) -> [f32; 4] {
    [
        x as f32 / width as f32,
        y as f32 / height as f32,
        w as f32 / width as f32,
        h as f32 / height as f32,
    ]
}

/// Draws every selected guide over one monitor's canvas area.
pub(crate) fn draw_guides(
    painter: &Painter,
    canvas: Rect,
    guides: SafeAreaGuides,
    size: (u16, u16),
) {
    let stroke = Stroke::new(crate::theme::line_width(), border_color());
    for (kind, [x, y, width, height]) in guides.rects(u32::from(size.0), u32::from(size.1)) {
        let guide = Rect::from_min_size(
            pos2(
                canvas.min.x + canvas.width() * x,
                canvas.min.y + canvas.height() * y,
            ),
            Vec2::new(canvas.width() * width, canvas.height() * height),
        );
        let color = match kind {
            SafeAreaKind::Action => stroke.color,
            SafeAreaKind::Title => AMBER,
            SafeAreaKind::CenterCut => PROGRAM,
        };
        match kind {
            SafeAreaKind::CenterCut => {
                painter.rect_stroke(
                    guide,
                    egui::CornerRadius::ZERO,
                    Stroke::new(stroke.width, color),
                    egui::StrokeKind::Inside,
                );
            }
            SafeAreaKind::Action | SafeAreaKind::Title => {
                let arm = (guide.width().min(guide.height()) * 0.08).max(8.0);
                let color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 190);
                let stroke = Stroke::new(stroke.width, color);
                for (corner, dx, dy) in [
                    (guide.left_top(), arm, arm),
                    (guide.right_top(), -arm, arm),
                    (guide.left_bottom(), arm, -arm),
                    (guide.right_bottom(), -arm, -arm),
                ] {
                    painter.line_segment([corner, corner + Vec2::new(dx, 0.0)], stroke);
                    painter.line_segment([corner, corner + Vec2::new(0.0, dy)], stroke);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_land_at_the_documented_relative_places() {
        let guides = SafeAreaGuides {
            action: true,
            title: true,
            center_4_3: false,
            center_16_9: false,
            center_9_16: true,
        };
        let rects = guides.rects(1920, 1080);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].0, SafeAreaKind::Action);
        assert_eq!(
            rects[0].1,
            [
                67.0 / 1920.0,
                38.0 / 1080.0,
                1785.0 / 1920.0,
                1004.0 / 1080.0
            ]
        );
        assert_eq!(rects[1].0, SafeAreaKind::Title);
        assert_eq!(
            rects[1].1,
            [
                96.0 / 1920.0,
                54.0 / 1080.0,
                1728.0 / 1920.0,
                972.0 / 1080.0
            ]
        );
        assert_eq!(rects[2].0, SafeAreaKind::CenterCut);
        assert_eq!(rects[2].1, [656.0 / 1920.0, 0.0, 608.0 / 1920.0, 1.0]);
    }

    #[test]
    fn a_landscape_cut_on_a_portrait_canvas_centers_vertically() {
        let guides = SafeAreaGuides {
            action: false,
            title: false,
            center_4_3: true,
            center_16_9: true,
            center_9_16: false,
        };
        let rects = guides.rects(1080, 1920);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].1, [0.0, 555.0 / 1920.0, 1.0, 810.0 / 1920.0]);
        assert_eq!(rects[1].1, [0.0, 656.0 / 1920.0, 1.0, 608.0 / 1920.0]);
    }

    #[test]
    fn preference_lines_round_trip_every_switch() {
        let guides = SafeAreaGuides {
            action: true,
            title: false,
            center_4_3: false,
            center_16_9: true,
            center_9_16: true,
        };
        let mut parsed = SafeAreaGuides::default();
        for (key, value) in guides.preference_entries() {
            assert!(parsed.apply_preference(key, value));
        }
        assert_eq!(parsed, guides);
        assert!(!parsed.apply_preference("layout.program", "on"));
    }
}
