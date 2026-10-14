//! The input bank: thumbnail-style source tiles with tally borders.
//!
//! Click selects Preview; double-click (or the tile's CUT key) selects the
//! source on Preview and then cuts it to Program, using only the existing
//! Preview-selection and Cut intents under their usual gates.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_types::{InputId, MAX_INPUT_NAME_BYTES};

use crate::theme::{
    BORDER_STRONG, GRAPHITE_RAISED, MONITOR_BLACK, MUTED, PROGRAM, TEXT, TEXT_DIM, mono,
    section_header, small_button, tally_badge, tally_color,
};
use crate::{
    InputMoveDirection, RenameDraft, StudioIntent, StudioShell, StudioUiState, TransitionGate,
    preview_selection_available, tally_state, transition_availability,
};

pub(crate) const MIN_TILE_WIDTH: f32 = 156.0;
const MAX_TILE_WIDTH: f32 = 220.0;
const TILE_NAME_HEIGHT: f32 = 22.0;

pub(crate) fn project_editing_available(state: &StudioUiState) -> bool {
    state.connection_status.controls_enabled() && state.can_edit_project && state.view.is_some()
}

/// Returns the number of tile columns that fit the available width.
pub(crate) fn dynamic_columns(available_width: f32, spacing: f32) -> usize {
    let mut columns = 1;
    let mut occupied = MIN_TILE_WIDTH;
    while occupied + spacing + MIN_TILE_WIDTH <= available_width {
        columns += 1;
        occupied += spacing + MIN_TILE_WIDTH;
    }
    columns
}

/// One operator gesture on an input tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TileGesture {
    None,
    /// A single click: select Preview.
    Click,
    /// A double-click or the tile's CUT key: select Preview, then Cut.
    Cut,
}

/// Session gates that apply to tile gestures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TileGates {
    pub(crate) preview: bool,
    pub(crate) transition: bool,
}

/// Translates one tile gesture into intents, honoring the Preview and
/// transition gates independently.
///
/// A click selects Preview. A cut gesture selects Preview and then cuts it to
/// Program, but only when both Preview selection and transitions are
/// available; otherwise it degrades to a plain Preview selection.
pub(crate) fn tile_intents(
    input: InputId,
    gesture: TileGesture,
    gates: TileGates,
) -> Vec<StudioIntent> {
    match gesture {
        TileGesture::Cut if gates.preview && gates.transition => {
            vec![StudioIntent::SelectPreview(input), StudioIntent::Cut]
        }
        TileGesture::Click | TileGesture::Cut if gates.preview => {
            vec![StudioIntent::SelectPreview(input)]
        }
        _ => Vec::new(),
    }
}

impl StudioShell {
    pub(crate) fn reconcile_rename_draft(&mut self, state: &StudioUiState) {
        let Some(draft) = &mut self.rename_draft else {
            return;
        };
        if !project_editing_available(state) {
            self.rename_draft = None;
            return;
        }
        let Some(view) = state.view.as_ref() else {
            self.rename_draft = None;
            return;
        };
        let Some(index) = view.inputs.iter().position(|input| *input == draft.input) else {
            self.rename_draft = None;
            return;
        };
        let current = &view.input_names[index];
        if current != &draft.confirmed_name {
            if draft.name == draft.confirmed_name {
                draft.name.clone_from(current);
            }
            draft.confirmed_name.clone_from(current);
        }
    }

    pub(crate) fn draw_inputs(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let count = state.view.as_ref().map_or(0, |view| view.inputs.len());
        section_header(ui, "Input bank", |ui| {
            ui.label(
                RichText::new("click PVW · double-click CUT · 1–8 keys")
                    .size(10.0)
                    .color(MUTED),
            );
            ui.label(mono(format!("{count} SOURCES")).size(10.5).color(TEXT_DIM));
        });
        ui.add_space(2.0);

        let gate = TransitionGate::from_state(state);
        let enabled = preview_selection_available(gate, state.can_select_preview);
        let cut_enabled = enabled && transition_availability(gate).basic();
        let reorder_enabled = project_editing_available(state);
        ScrollArea::vertical()
            .id_salt("studio-input-bank")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(view) = state.view.as_ref() else {
                    ui.label(RichText::new("INPUT STATE UNAVAILABLE").color(MUTED));
                    return;
                };
                let spacing = 8.0;
                let available = ui.available_width() - 4.0;
                let columns = dynamic_columns(available, spacing).min(view.inputs.len().max(1));
                #[allow(clippy::cast_precision_loss)] // column counts are tiny
                let tile_width = ((available - spacing * (columns as f32 - 1.0)) / columns as f32)
                    .clamp(MIN_TILE_WIDTH, MAX_TILE_WIDTH);
                for (row_index, row) in view.inputs.chunks(columns).enumerate() {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = spacing;
                        for (column, input) in row.iter().copied().enumerate() {
                            let index = row_index * columns + column;
                            ui.vertical(|ui| {
                                ui.set_width(tile_width);
                                self.draw_tile(
                                    ui,
                                    state,
                                    TileContext {
                                        input,
                                        index,
                                        name: &view.input_names[index],
                                        count: view.inputs.len(),
                                        width: tile_width,
                                        tally: tally_state(input, view.switcher),
                                        enabled,
                                        cut_enabled,
                                        reorder_enabled,
                                    },
                                    intents,
                                );
                            });
                        }
                    });
                    ui.add_space(4.0);
                }
            });
    }

    fn draw_tile(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        tile: TileContext<'_>,
        intents: &mut Vec<StudioIntent>,
    ) {
        let TileContext {
            input,
            index,
            name,
            count,
            width,
            tally,
            enabled,
            cut_enabled,
            reorder_enabled,
        } = tile;
        let thumb_height = (width * 9.0 / 16.0).round();
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, thumb_height + TILE_NAME_HEIGHT), sense);
        if ui.is_rect_visible(rect) {
            paint_tile(ui, rect, index, name, tally, enabled, response.hovered());
        }
        let response = response.on_hover_text(format!(
            "{name}\n{}\nClick: select Preview · Double-click: cut to Program",
            tally.operator_label()
        ));
        let gates = TileGates {
            preview: enabled,
            transition: cut_enabled,
        };
        let gesture = if response.double_clicked() {
            TileGesture::Cut
        } else if response.clicked() {
            TileGesture::Click
        } else {
            TileGesture::None
        };
        intents.extend(tile_intents(input, gesture, gates));

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let up = ui
                .add_enabled(reorder_enabled && index > 0, small_button("◀"))
                .on_hover_text("Move this input earlier in the project order");
            if up.clicked() {
                intents.push(StudioIntent::MoveInput {
                    input,
                    direction: InputMoveDirection::Up,
                });
            }
            let down = ui
                .add_enabled(reorder_enabled && index + 1 < count, small_button("▶"))
                .on_hover_text("Move this input later in the project order");
            if down.clicked() {
                intents.push(StudioIntent::MoveInput {
                    input,
                    direction: InputMoveDirection::Down,
                });
            }
            if ui
                .add_enabled(reorder_enabled, small_button("RENAME"))
                .on_hover_text("Rename this input")
                .clicked()
            {
                self.rename_draft = Some(RenameDraft {
                    input,
                    name: name.to_owned(),
                    confirmed_name: name.to_owned(),
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cut = ui
                    .add_enabled(
                        cut_enabled,
                        egui::Button::new(RichText::new("CUT").size(11.0).strong().color(PROGRAM))
                            .stroke(Stroke::new(1.0, PROGRAM.gamma_multiply(0.35)))
                            .min_size(Vec2::new(0.0, 20.0)),
                    )
                    .on_hover_text("Select this input on Preview, then cut it to Program");
                if cut.clicked() {
                    intents.extend(tile_intents(input, TileGesture::Cut, gates));
                }
            });
        });
        if self
            .rename_draft
            .as_ref()
            .is_some_and(|draft| draft.input == input)
        {
            self.draw_rename_editor(ui, state, input, name, intents);
        }
    }

    fn draw_rename_editor(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        input: InputId,
        current: &str,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(draft) = &mut self.rename_draft else {
            return;
        };
        let duplicate = state.view.as_ref().is_some_and(|view| {
            view.inputs
                .iter()
                .enumerate()
                .any(|(index, other)| *other != input && view.input_names[index] == draft.name)
        });
        let valid = !draft.name.trim().is_empty()
            && draft.name.len() <= MAX_INPUT_NAME_BYTES
            && !duplicate
            && draft.name != current;
        let mut close = false;
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(ui.available_width()));
        ui.horizontal(|ui| {
            if ui.add_enabled(valid, small_button("APPLY")).clicked() {
                intents.push(StudioIntent::RenameInput {
                    input,
                    name: draft.name.clone(),
                });
                close = true;
            }
            if ui.add(small_button("CANCEL")).clicked() {
                close = true;
            }
        });
        if close {
            self.rename_draft = None;
        }
    }
}

#[derive(Clone, Copy)]
struct TileContext<'a> {
    input: InputId,
    index: usize,
    name: &'a str,
    count: usize,
    width: f32,
    tally: crate::TallyState,
    enabled: bool,
    cut_enabled: bool,
    reorder_enabled: bool,
}

#[allow(clippy::too_many_lines)] // one painter pass per tile region
fn paint_tile(
    ui: &Ui,
    rect: Rect,
    index: usize,
    name: &str,
    tally: crate::TallyState,
    enabled: bool,
    hovered: bool,
) {
    let painter = ui.painter_at(rect.expand(2.0));
    let radius = CornerRadius::same(4);
    let thumb = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - TILE_NAME_HEIGHT));
    let name_bar = Rect::from_min_max(pos2(rect.min.x, thumb.max.y), rect.max);
    let border = tally_color(tally);
    let has_tally = !tally.is_none();

    painter.rect_filled(rect, radius, MONITOR_BLACK);
    // Large dim ordinal, like a multiview label on an empty source.
    painter.text(
        thumb.center(),
        Align2::CENTER_CENTER,
        format!("{}", index + 1),
        FontId::new(
            (thumb.height() * 0.42).clamp(22.0, 48.0),
            FontFamily::Proportional,
        ),
        if has_tally {
            border.gamma_multiply(0.35)
        } else {
            Color32::from_rgb(40, 44, 50)
        },
    );
    painter.text(
        pos2(thumb.center().x, thumb.max.y - 10.0),
        Align2::CENTER_CENTER,
        "no video feed",
        FontId::new(9.0, FontFamily::Proportional),
        MUTED.gamma_multiply(0.7),
    );
    // Key hint for the numbered Preview shortcuts.
    if index < 8 {
        let key = Rect::from_min_size(thumb.min + Vec2::new(6.0, 6.0), Vec2::new(16.0, 16.0));
        painter.rect_filled(key, CornerRadius::same(3), GRAPHITE_RAISED);
        painter.rect_stroke(
            key,
            CornerRadius::same(3),
            Stroke::new(1.0, BORDER_STRONG),
            StrokeKind::Inside,
        );
        painter.text(
            key.center(),
            Align2::CENTER_CENTER,
            format!("{}", index + 1),
            FontId::new(10.0, FontFamily::Monospace),
            TEXT_DIM,
        );
    }
    if let Some(badge) = tally_badge(tally) {
        let galley = painter.layout_no_wrap(
            badge.to_owned(),
            FontId::new(10.0, FontFamily::Proportional),
            Color32::WHITE,
        );
        let badge_rect = Rect::from_min_size(
            pos2(thumb.max.x - galley.size().x - 16.0, thumb.min.y + 6.0),
            Vec2::new(galley.size().x + 10.0, 16.0),
        );
        painter.rect_filled(badge_rect, CornerRadius::same(3), border);
        painter.galley(
            badge_rect.center() - galley.size() / 2.0,
            galley,
            Color32::WHITE,
        );
    }
    painter.rect_filled(
        name_bar,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 4,
            se: 4,
        },
        if has_tally {
            border.gamma_multiply(0.3)
        } else {
            GRAPHITE_RAISED
        },
    );
    let name_galley = painter.layout(
        name.to_owned(),
        FontId::new(12.0, FontFamily::Proportional),
        TEXT,
        name_bar.width() - 12.0,
    );
    let name_rows_clip = painter.with_clip_rect(name_bar.shrink2(Vec2::new(6.0, 2.0)));
    name_rows_clip.galley(
        pos2(name_bar.min.x + 6.0, name_bar.center().y - 7.5),
        name_galley,
        TEXT,
    );
    let stroke_width = if has_tally { 2.0 } else { 1.0 };
    let stroke_color = if hovered && enabled && !has_tally {
        crate::theme::ACCENT
    } else if hovered && enabled {
        border.gamma_multiply(1.2)
    } else {
        border
    };
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(stroke_width, stroke_color),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    #[test]
    fn tile_gestures_respect_preview_and_transition_gates() {
        let input = InputId::new(NonZeroU128::new(5).unwrap());
        let open = TileGates {
            preview: true,
            transition: true,
        };
        assert_eq!(tile_intents(input, TileGesture::None, open), Vec::new());
        assert_eq!(
            tile_intents(input, TileGesture::Click, open),
            vec![StudioIntent::SelectPreview(input)]
        );
        assert_eq!(
            tile_intents(input, TileGesture::Cut, open),
            vec![StudioIntent::SelectPreview(input), StudioIntent::Cut]
        );
        let preview_only = TileGates {
            transition: false,
            ..open
        };
        assert_eq!(
            tile_intents(input, TileGesture::Cut, preview_only),
            vec![StudioIntent::SelectPreview(input)]
        );
        let closed = TileGates {
            preview: false,
            ..open
        };
        assert_eq!(tile_intents(input, TileGesture::Click, closed), Vec::new());
        assert_eq!(tile_intents(input, TileGesture::Cut, closed), Vec::new());
    }

    #[test]
    fn tile_columns_fit_available_width() {
        assert_eq!(dynamic_columns(100.0, 8.0), 1);
        assert_eq!(dynamic_columns(MIN_TILE_WIDTH * 2.0 + 8.0, 8.0), 2);
        assert_eq!(dynamic_columns(MIN_TILE_WIDTH * 2.0 + 7.0, 8.0), 1);
    }
}
