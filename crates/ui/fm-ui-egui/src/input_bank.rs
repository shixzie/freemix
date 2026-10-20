//! The input bank: thumbnail-style source tiles with tally borders.
//!
//! Click selects Preview; double-click (or the tile's CUT key) selects the
//! source on Preview and then cuts it to Program, using only the existing
//! Preview-selection and Cut intents under their usual gates. A `+` tile at
//! the end of the bank opens the Add input dialog; the tile menu offers
//! Remove with a confirmation step and the input lifecycle actions
//! (`input_lifecycle`). A toolbar above the tiles filters them by category;
//! hidden tiles keep their bank position and number key.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{
    InputCategoryStatus, InputLifecycleStatus, MonitorFrameOrigin, MonitorTap, WireInputId,
};
use fm_types::{InputId, MAX_INPUT_NAME_BYTES};
use fm_ui_model::{EditLocks, SwitcherState};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_catalogue::{CatalogueAvailability, MAX_LIVE_INPUTS};
use crate::input_lifecycle::{
    LifecycleMenu, label_color, lifecycle_badges, tile_status_line, visible_inputs,
};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::{FrameFacts, MonitorPicture, letterbox, picture_visible};
use crate::theme::{
    AMBER, LOCK_BADGE_SIZE, MONITOR_BLACK, PREVIEW, PROGRAM, Tone, accent_color,
    border_strong_color, elided, lock_badge, mono, muted_color, paint_lock_badge, pill,
    raised_color, small_button, status_chip, tally_badge, tally_color, text_color, text_dim_color,
};
use crate::{
    InputMoveDirection, RenameDraft, StudioIntent, StudioShell, StudioUiState, TransitionGate,
    edit_lock_summary, input_edit_lock_reason, preview_selection_available, tally_state,
    transition_availability,
};

pub(crate) const MIN_TILE_WIDTH: f32 = 156.0;
const MAX_TILE_WIDTH: f32 = 320.0;
const TILE_NAME_HEIGHT: f32 = 22.0;
/// Vertical space one tile row needs besides its 16:9 thumbnail: the name
/// bar, the key row, and row spacing.
const TILE_ROW_CHROME: f32 = TILE_NAME_HEIGHT + 20.0 + 14.0;

pub(crate) fn project_editing_available(state: &StudioUiState) -> bool {
    state.connection_status.controls_enabled() && state.can_edit_project && state.view.is_some()
}

/// Picks the tile grid for `count` inputs: the largest tiles (up to the
/// maximum) whose rows all fit `available_height`, else the densest grid of
/// minimum-width tiles (the bank scrolls). Returns `(columns, tile_width)`.
#[allow(clippy::cast_precision_loss)] // column and row counts are tiny
pub(crate) fn bank_grid(
    available_width: f32,
    available_height: f32,
    count: usize,
    spacing: f32,
) -> (usize, f32) {
    let max_columns = dynamic_columns(available_width, spacing).min(count.max(1));
    let width_for = |columns: usize| {
        ((available_width - spacing * (columns as f32 - 1.0)) / columns as f32)
            .clamp(MIN_TILE_WIDTH, MAX_TILE_WIDTH)
            .floor()
    };
    (1..=max_columns)
        .map(|columns| (columns, width_for(columns)))
        .find(|(columns, width)| {
            let rows = count.max(1).div_ceil(*columns) as f32;
            rows * ((width * 9.0 / 16.0).round() + TILE_ROW_CHROME) <= available_height
        })
        .unwrap_or_else(|| (max_columns, width_for(max_columns)))
}

/// Returns the number of tile columns that fit the available width.
///
/// A width that is not finite (an unbounded layout) yields one column rather
/// than looping forever.
pub(crate) fn dynamic_columns(available_width: f32, spacing: f32) -> usize {
    if !available_width.is_finite() {
        return 1;
    }
    let mut columns = 1;
    let mut occupied = MIN_TILE_WIDTH;
    while occupied + spacing + MIN_TILE_WIDTH <= available_width {
        columns += 1;
        occupied += spacing + MIN_TILE_WIDTH;
    }
    columns
}

/// Right-column marker for a tile menu item blocked by a padlock.
const LOCKED_MARK: &str = Msg::Locked.en();

/// Why moving the input at `index` one step is blocked by the neighbour it
/// would swap with, if that neighbour has its own padlock. The server refuses
/// any reorder that changes a locked input's position.
pub(crate) fn neighbour_move_lock_reason(
    inputs: &[InputId],
    locks: &EditLocks,
    index: usize,
    direction: InputMoveDirection,
) -> Option<&'static str> {
    let neighbour = match direction {
        InputMoveDirection::Up => index.checked_sub(1)?,
        InputMoveDirection::Down => index + 1,
    };
    let neighbour = *inputs.get(neighbour)?;
    locks
        .is_input_locked(neighbour)
        .then_some(Msg::ReasonNeighbourLocked.en())
}

/// Picks the monitor tap whose current frame shows exactly `input`, if any.
///
/// A tap frame is larger and more frequent than the input's own thumbnail
/// (Protocol 2.59), so a tile shows it in preference to the thumbnail. A
/// tile borrows a tap's frame only when that frame is fresh,
/// shows this input, and this input is the tap's realized source, so a tile
/// never shows another source's picture. Program wins when both match.
pub(crate) fn thumbnail_tap(
    input: InputId,
    switcher: SwitcherState,
    frames: [Option<FrameFacts>; 2],
) -> Option<MonitorTap> {
    let wire = WireInputId::from_domain(input);
    [MonitorTap::Program, MonitorTap::Preview]
        .into_iter()
        .find(|tap| {
            let realized = match tap {
                MonitorTap::Preview => switcher.realized.preview,
                MonitorTap::Program => switcher.realized.program,
            };
            realized == input
                && frames[tap.index()].is_some_and(|frame| {
                    frame.input == wire
                        && picture_visible(frame.origin, frame.input, Some(wire), frame.age)
                })
        })
}

/// Short tag naming where a tile thumbnail came from.
pub(crate) fn thumbnail_tag(tap: MonitorTap, origin: MonitorFrameOrigin) -> &'static str {
    match (tap, origin) {
        (MonitorTap::Program, MonitorFrameOrigin::Simulated) => Msg::ThumbPgmTapSim.en(),
        (MonitorTap::Preview, MonitorFrameOrigin::Simulated) => Msg::ThumbPvwTapSim.en(),
        (MonitorTap::Program, MonitorFrameOrigin::Native) => Msg::ThumbPgmTap.en(),
        (MonitorTap::Preview, MonitorFrameOrigin::Native) => Msg::ThumbPvwTap.en(),
    }
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
        if view.edit_locks.blocks_input_edit(draft.input) {
            // A padlock engaged while the editor was open: drop the draft
            // rather than offer an Apply the server would refuse.
            self.rename_draft = None;
            return;
        }
        let current = &view.input_names[index];
        if current != &draft.confirmed_name {
            if draft.name == draft.confirmed_name {
                draft.name.clone_from(current);
            }
            draft.confirmed_name.clone_from(current);
        }
    }

    #[allow(clippy::too_many_lines)] // one immediate-mode region
    pub(crate) fn draw_inputs(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = self.locale;
        // The live thumbnail feed follows the bank's visibility.
        self.input_bank_drawn = true;
        let count = state.view.as_ref().map(|view| view.inputs.len());
        let locks = edit_lock_summary(state);
        crate::theme::panel_header(ui, |ui| {
            if let Some(chip) = locks.bank_chip(l) {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    status_chip(ui, &chip, Tone::Pending, 92.0).on_hover_text(locks.detail(l));
                    lock_badge(ui).on_hover_text(locks.detail(l));
                });
            }
            ui.label(
                mono(count.map_or_else(
                    || l.text(Msg::SourcesUnknown).to_owned(),
                    |count| l.format(Msg::SourcesCount, &[("count", Arg::from(count))]),
                ))
                .size(crate::theme::legible(10.5))
                .color(text_dim_color()),
            );
            // The gesture hint is dropped first when the header is narrow.
            if ui.available_width() > 250.0 {
                ui.label(
                    RichText::new(l.text(Msg::BankGestureHint))
                        .size(crate::theme::legible(10.0))
                        .color(muted_color()),
                );
            }
        });
        ui.add_space(2.0);
        self.draw_bank_toolbar(ui, state);
        ui.add_space(2.0);

        let gate = TransitionGate::from_state(state);
        let enabled = preview_selection_available(gate, state.can_select_preview);
        let cut_enabled = enabled && transition_availability(gate).basic();
        let reorder_enabled = project_editing_available(state);
        let now = std::time::Instant::now();
        let pictures = MonitorTap::ALL.map(|tap| {
            self.monitor_textures
                .picture(tap, &state.monitor_images, now)
        });
        let bank_height = ui.available_height();
        ScrollArea::vertical()
            .id_salt("studio-input-bank")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(view) = state.view.as_ref() else {
                    crate::theme::empty_state(
                        ui,
                        l.text(Msg::NoInputState),
                        l.text(Msg::NoInputStateDetail),
                    );
                    return;
                };
                let spacing = 8.0;
                let available = ui.available_width() - 4.0;
                // The filter hides tiles but each keeps its bank position;
                // the `+` tile takes one more grid cell after the inputs.
                let mut cell_indices = visible_inputs(view, self.lifecycle.filter);
                cell_indices.push(view.inputs.len());
                let cells = cell_indices.len();
                let (columns, tile_width) = bank_grid(available, bank_height, cells, spacing);
                for row in cell_indices.chunks(columns) {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = spacing;
                        for index in row.iter().copied() {
                            let Some(input) = view.inputs.get(index).copied() else {
                                ui.vertical(|ui| {
                                    ui.set_width(tile_width);
                                    self.draw_add_tile(ui, state, tile_width);
                                });
                                continue;
                            };
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
                                        lifecycle: view.lifecycle(input),
                                        categories: &view.input_categories,
                                        thumbnail: thumbnail_tap(
                                            input,
                                            view.switcher,
                                            pictures.map(|picture| {
                                                picture.map(|picture| picture.facts)
                                            }),
                                        )
                                        .and_then(|tap| {
                                            pictures[tap.index()].map(|picture| (tap, picture))
                                        }),
                                        enabled,
                                        cut_enabled,
                                        reorder_enabled: reorder_enabled
                                            && input_edit_lock_reason(state, input).is_none(),
                                        locked: input_edit_lock_reason(state, input).is_some(),
                                        removal: self.remove_input_gate(state, input),
                                        grade_reason: crate::studio_color::grade_lock_reason(
                                            state,
                                            fm_ui_model::grade_editor::GradeTarget::Input(input),
                                        ),
                                        move_locks: [
                                            InputMoveDirection::Up,
                                            InputMoveDirection::Down,
                                        ]
                                        .map(|direction| {
                                            neighbour_move_lock_reason(
                                                &view.inputs,
                                                &view.edit_locks,
                                                index,
                                                direction,
                                            )
                                        }),
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

    #[allow(clippy::too_many_lines)] // one immediate-mode tile plus its key row
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
            width,
            tally,
            thumbnail,
            enabled,
            cut_enabled,
            ..
        } = tile;
        let l = self.locale;
        let preview_reason = unavailable_reason(state, Capability::SelectPreview);
        let cut_reason = if enabled {
            unavailable_reason(state, Capability::Transition)
        } else {
            preview_reason
        };
        let edit_reason = input_edit_lock_reason(state, input)
            .filter(|_| project_editing_available(state))
            .unwrap_or_else(|| unavailable_reason(state, Capability::EditProject));
        let thumb_height = (width * 9.0 / 16.0).round();
        // Tiles always sense clicks so hover, focus, and the context menu work;
        // the Preview and transition gates still decide which intents result.
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(width, thumb_height + TILE_NAME_HEIGHT),
            Sense::click(),
        );
        // A network stream input's receive state (IN-015).
        let stream = state
            .view
            .as_ref()
            .and_then(|view| view.network_stream(input));
        // A capture input's acquisition state (IN-001).
        let capture = state
            .view
            .as_ref()
            .and_then(|view| view.capture_input(input));
        // A screen capture input's state (IN-011).
        let screen_capture = state
            .view
            .as_ref()
            .and_then(|view| view.screen_capture_input(input));
        // A system audio input's capture state (AU-011).
        let system_audio = state
            .view
            .as_ref()
            .and_then(|view| view.system_audio_input(input));
        // The input's own live thumbnail (Protocol 2.59) when no tap frame
        // shows it; a live source's picture that stopped changing is stale.
        let wire = WireInputId::from_domain(input);
        let own_thumbnail = if thumbnail.is_none() {
            self.thumbnail_textures.tile(
                wire,
                &state.input_thumbnails,
                std::time::Instant::now(),
                stream.is_some() || capture.is_some() || screen_capture.is_some(),
            )
        } else {
            None
        };
        // A system audio input has no picture: its meters stand in for one.
        let audio_placeholder = if system_audio.is_some() {
            AudioPlaceholder::Meters(
                state
                    .audio_meters
                    .as_ref()
                    .and_then(|meters| meters.inputs.iter().find(|meters| meters.input == wire)),
            )
        } else {
            AudioPlaceholder::None
        };
        // A status badge sits where the `no thumbnail` caption goes, so the
        // caption gives way to it.
        let status_badge = stream.is_some_and(|entry| {
            crate::network_stream_ui::stream_badge(l, &entry.status).is_some()
        }) || capture.is_some_and(|entry| {
            crate::capture_input_ui::capture_badge(l, &entry.status).is_some()
        }) || system_audio.is_some_and(|entry| {
            crate::system_audio_ui::system_audio_badge(l, &entry.status).is_some()
        }) || screen_capture.is_some_and(|entry| {
            crate::screen_capture_ui::screen_capture_badge(l, &entry.status).is_some()
        });
        if ui.is_rect_visible(rect) {
            paint_tile(
                ui,
                rect,
                TilePaint {
                    index,
                    name,
                    tally,
                    thumbnail,
                    own_thumbnail,
                    audio_placeholder,
                    lifecycle: tile.lifecycle,
                    enabled,
                    locked: tile.locked,
                    hovered: response.hovered(),
                    focused: response.has_focus(),
                    pressed: response.is_pointer_button_down_on(),
                    status_badge,
                    selected: self.selected_input == Some(input),
                },
            );
        }
        if let Some(entry) = stream
            && ui.is_rect_visible(rect)
        {
            crate::network_stream_ui::paint_stream_badge(
                ui.painter(),
                Rect::from_min_size(rect.min, Vec2::new(width, thumb_height)),
                l,
                &entry.status,
            );
        }
        if let Some(entry) = capture
            && ui.is_rect_visible(rect)
        {
            crate::capture_input_ui::paint_capture_badge(
                ui.painter(),
                Rect::from_min_size(rect.min, Vec2::new(width, thumb_height)),
                l,
                &entry.status,
            );
        }
        if let Some(entry) = system_audio
            && ui.is_rect_visible(rect)
        {
            crate::system_audio_ui::paint_system_audio_badge(
                ui.painter(),
                Rect::from_min_size(rect.min, Vec2::new(width, thumb_height)),
                l,
                &entry.status,
            );
        }
        if let Some(entry) = screen_capture
            && ui.is_rect_visible(rect)
        {
            crate::screen_capture_ui::paint_screen_capture_badge(
                ui.painter(),
                Rect::from_min_size(rect.min, Vec2::new(width, thumb_height)),
                l,
                &entry.status,
            );
        }
        // Its captured sound's state has a badge of its own (IN-011).
        if let Some(entry) = screen_capture
            && ui.is_rect_visible(rect)
        {
            crate::screen_capture_ui::tile_audio_badge(
                ui,
                Rect::from_min_size(rect.min, Vec2::new(width, thumb_height)),
                l,
                entry,
            );
        }
        let shortcut = if index < 8 {
            format!("  [{}]", index + 1)
        } else {
            String::new()
        };
        let source = match (thumbnail, own_thumbnail) {
            (Some((tap, _)), _) => l
                .text(match tap {
                    MonitorTap::Program => Msg::ThumbnailProgram,
                    MonitorTap::Preview => Msg::ThumbnailPreview,
                })
                .to_owned(),
            (None, Some(own)) => crate::input_thumbnail_ui::thumbnail_hover(l, own),
            (None, None) => l.text(Msg::ThumbnailNone).to_owned(),
        };
        let mut status = tile_status_line(l, name, tile.lifecycle, tile.categories);
        if let Some(entry) = stream {
            status.push_str(" · ");
            status.push_str(&crate::network_stream_ui::stream_details(l, entry));
        }
        if let Some(entry) = capture {
            status.push_str(" · ");
            status.push_str(&crate::capture_input_ui::capture_details(l, entry));
        }
        if let Some(entry) = system_audio {
            status.push_str(" · ");
            status.push_str(&crate::system_audio_ui::system_audio_details(l, entry));
        }
        if let Some(entry) = screen_capture {
            status.push_str(" · ");
            status.push_str(&crate::screen_capture_ui::screen_capture_details(l, entry));
        }
        let mut hover = format!(
            "{status}\n{}\n{source}\n{}",
            tally.operator_label(l),
            l.format(Msg::TileGestures, &[("shortcut", Arg::Str(&shortcut))])
        );
        if self.selected_input == Some(input) {
            hover.push('\n');
            hover.push_str(l.text(Msg::TileSelectedNote));
        }
        // Clicking, right-clicking, or tabbing to a tile selects its input
        // on this console (RC-004).
        if response.clicked() || response.secondary_clicked() || response.gained_focus() {
            self.selected_input = Some(input);
        }
        let response = if enabled {
            response.on_hover_text(hover)
        } else {
            response.on_hover_text(l10n::disabled_text(l, &hover, preview_reason))
        };
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
        response.context_menu(|ui| {
            self.draw_tile_menu(
                ui,
                state,
                tile,
                TileReasons {
                    preview: preview_reason,
                    cut: cut_reason,
                    edit: edit_reason,
                },
                intents,
            );
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Rename and reorder live behind one menu (also on right-click)
            // so the bank reads as a grid of sources, not a grid of buttons.
            egui::containers::menu::MenuButton::from_button(
                small_button("···").min_size(Vec2::new(30.0, 20.0)),
            )
            .ui(ui, |ui| {
                self.draw_tile_menu(
                    ui,
                    state,
                    tile,
                    TileReasons {
                        preview: preview_reason,
                        cut: cut_reason,
                        edit: edit_reason,
                    },
                    intents,
                );
            })
            .0
            .on_hover_text(l.text(Msg::TileMoreHover));
            ui.with_layout(l10n::trailing_layout(l), |ui| {
                let cut = ui
                    .add_enabled(
                        cut_enabled,
                        egui::Button::new(
                            RichText::new(l.text(Msg::CutKey))
                                .size(crate::theme::legible(11.0))
                                .strong()
                                .color(text_color()),
                        )
                        // Neutral: red in the bank means "this input is on
                        // Program", never "this key cuts".
                        .stroke(Stroke::new(crate::theme::line_width(), border_strong_color()))
                        .min_size(Vec2::new(56.0, 20.0)),
                    )
                    .hint(l.text(Msg::TileCutHover), cut_reason);
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

    #[allow(clippy::too_many_lines)] // one menu with its footer notes
    fn draw_tile_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        tile: TileContext<'_>,
        reasons: TileReasons,
        intents: &mut Vec<StudioIntent>,
    ) {
        let TileContext {
            input,
            index,
            name,
            count,
            enabled,
            cut_enabled,
            reorder_enabled,
            ..
        } = tile;
        let TileReasons {
            preview: preview_reason,
            cut: cut_reason,
            edit: edit_reason,
        } = reasons;
        let gates = TileGates {
            preview: enabled,
            transition: cut_enabled,
        };
        let l = self.locale;
        ui.set_min_width(230.0);
        ui.set_max_width(260.0);
        draw_tile_menu_header(ui, index, name, tile.tally);
        ui.separator();
        let locked = tile.locked;
        // Marks arrive as catalog English (`LOCKED`, `IN USE`) so their tone
        // can be matched; they are worded in the console language here.
        let item =
            |ui: &mut Ui, enabled: bool, label: &str, shortcut: &str, hover: &str, reason: &str| {
                let mut button = egui::Button::new(label);
                let mark = l.keys(shortcut);
                if let Some(color) = mark_color(shortcut) {
                    button = button.shortcut_text(
                        RichText::new(mark)
                            .size(crate::theme::legible(10.0))
                            .strong()
                            .color(color),
                    );
                } else if shortcut == LOCKED_MARK {
                    button = button.shortcut_text(
                        RichText::new(mark)
                            .size(crate::theme::legible(10.0))
                            .strong()
                            .color(AMBER),
                    );
                } else if !shortcut.is_empty() {
                    button = button.shortcut_text(RichText::new(mark).color(muted_color()));
                }
                ui.add_enabled(enabled, button)
                    .hint(hover, reason)
                    .clicked()
            };
        let key = if index < 8 {
            format!("{}", index + 1)
        } else {
            String::new()
        };
        if item(
            ui,
            enabled,
            l.text(Msg::SelectOnPreview),
            &key,
            &if key.is_empty() {
                l.text(Msg::ShortcutTileClick).to_owned()
            } else {
                l10n::with_key(l, l.text(Msg::ShortcutTileClick), &key)
            },
            preview_reason,
        ) {
            intents.extend(tile_intents(input, TileGesture::Click, gates));
        }
        if item(
            ui,
            cut_enabled,
            l.text(Msg::CutToProgram),
            Msg::KeyDoubleClickShort.en(),
            l.text(Msg::TileCutHover),
            cut_reason,
        ) {
            intents.extend(tile_intents(input, TileGesture::Cut, gates));
        }
        // Grading is a project edit: like Rename and Move, `Grade…` is
        // offered only to a session that may edit this input, and says why
        // not otherwise (View › Colour still opens the window to look).
        if item(
            ui,
            tile.grade_reason.is_none(),
            l.text(Msg::ColorTileItem),
            if locked { LOCKED_MARK } else { "" },
            l.text(Msg::ColorTileHover),
            tile.grade_reason.unwrap_or(""),
        ) {
            self.open_color(Some(fm_ui_model::grade_editor::GradeTarget::Input(input)));
        }
        // A Scene input: its scene in the layer designer, which opens to
        // look even when edits are locked (it says why on every control).
        let scene = state
            .project_state
            .as_ref()
            .and_then(|project| project.scene(input))
            .and_then(|shown| shown.scene);
        if let Some(scene) = scene
            && item(
                ui,
                true,
                l.text(Msg::DesignTileItem),
                crate::SCENE_DESIGNER_SHORTCUT,
                l.text(Msg::DesignTileHover),
                "",
            )
        {
            self.open_designer(Some(scene));
        }
        // A screen capture input: its target picked again on the server's
        // desktop (IN-011).
        let capture = state
            .view
            .as_ref()
            .is_some_and(|view| view.screen_capture_input(input).is_some());
        if capture
            && item(
                ui,
                state.connection_status.controls_enabled() && state.can_edit_project,
                l.text(Msg::ScPickAgain),
                "",
                l.text(Msg::ScPickAgainHover),
                unavailable_reason(state, Capability::EditProject),
            )
        {
            intents.push(StudioIntent::PickScreenCapture { input });
        }
        ui.separator();
        self.draw_lifecycle_menu(
            ui,
            state,
            LifecycleMenu {
                input,
                name,
                edit: (reorder_enabled, edit_reason),
                mark: if locked { LOCKED_MARK } else { "" },
            },
            intents,
        );
        ui.separator();
        let edit_mark = if locked { LOCKED_MARK } else { "" };
        let [left_lock, right_lock] = tile.move_locks;
        if item(
            ui,
            reorder_enabled,
            l.text(Msg::RenameEllipsis),
            edit_mark,
            l.text(Msg::TileRenameHover),
            edit_reason,
        ) {
            self.rename_draft = Some(RenameDraft::new(input, name));
        }
        if item(
            ui,
            reorder_enabled && index > 0 && left_lock.is_none(),
            l.text(Msg::MoveLeft),
            if left_lock.is_some() {
                LOCKED_MARK
            } else {
                edit_mark
            },
            l.text(Msg::TileMoveLeftHover),
            if !reorder_enabled {
                edit_reason
            } else if index == 0 {
                Msg::ReasonInputFirst.en()
            } else {
                left_lock.unwrap_or("")
            },
        ) {
            intents.push(StudioIntent::MoveInput {
                input,
                direction: InputMoveDirection::Up,
            });
        }
        if item(
            ui,
            reorder_enabled && index + 1 < count && right_lock.is_none(),
            l.text(Msg::MoveRight),
            if right_lock.is_some() {
                LOCKED_MARK
            } else {
                edit_mark
            },
            l.text(Msg::TileMoveRightHover),
            if !reorder_enabled {
                edit_reason
            } else if index + 1 >= count {
                Msg::ReasonInputLast.en()
            } else {
                right_lock.unwrap_or("")
            },
        ) {
            intents.push(StudioIntent::MoveInput {
                input,
                direction: InputMoveDirection::Down,
            });
        }
        ui.separator();
        let removal = tile.removal;
        let remove_mark = removal.mark.unwrap_or("");
        if item(
            ui,
            removal.enabled,
            l.text(Msg::RemoveEllipsis),
            remove_mark,
            &l.format(Msg::RemoveInputHover, &[("name", Arg::Str(name))]),
            removal.reason,
        ) {
            self.open_remove_input(input, name);
        }
        let notes = tile_menu_notes(
            l,
            TileGates {
                preview: enabled,
                transition: cut_enabled,
            },
            reorder_enabled,
            (!removal.enabled).then_some(removal.reason),
            reasons,
        );
        if !notes.is_empty() {
            ui.separator();
            // A left-aligned layout so the menu's justified layout does not
            // stretch the wrapped words apart.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                for note in notes {
                    ui.add(
                        egui::Label::new(
                            RichText::new(note)
                                .size(crate::theme::legible(10.5))
                                .color(muted_color()),
                        )
                        .wrap(),
                    );
                }
            });
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
        let edit = ui
            .add(
                egui::TextEdit::singleline(&mut draft.name)
                    .id(egui::Id::new(("studio-rename", input)))
                    .desired_width(ui.available_width()),
            )
            .on_hover_text(l10n::locale(ui.ctx()).text(Msg::RenameFieldHover));
        if draft.focus_pending {
            draft.focus_pending = false;
            edit.request_focus();
        }
        let submitted = edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let l = self.locale;
        let reason = if draft.name.trim().is_empty() {
            Msg::ReasonNameEmpty.en()
        } else if draft.name.len() > MAX_INPUT_NAME_BYTES {
            Msg::ReasonNameTooLong.en()
        } else if duplicate {
            Msg::ReasonNameTaken.en()
        } else {
            Msg::ReasonNameUnchanged.en()
        };
        ui.horizontal(|ui| {
            if (ui
                .add_enabled(valid, small_button(l.text(Msg::ApplyKey)))
                .hint(l.text(Msg::RenameApplyHover), reason)
                .clicked()
                || (submitted && valid))
                && !close
            {
                intents.push(StudioIntent::RenameInput {
                    input,
                    name: draft.name.clone(),
                });
                close = true;
            }
            if ui
                .add(small_button(l.text(Msg::CancelKey)))
                .on_hover_text(l.text(Msg::RenameCancelHover))
                .clicked()
            {
                close = true;
            }
        });
        if close {
            self.rename_draft = None;
        }
    }
}

/// Menu header: the input's key, its name (ellipsized), and its tally role.
fn draw_tile_menu_header(ui: &mut Ui, index: usize, name: &str, tally: crate::TallyState) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (key, _) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
        ui.painter()
            .rect_filled(key, CornerRadius::same(3), raised_color());
        ui.painter().rect_stroke(
            key,
            CornerRadius::same(3),
            Stroke::new(crate::theme::line_width(), border_strong_color()),
            StrokeKind::Inside,
        );
        ui.painter().text(
            key.center(),
            Align2::CENTER_CENTER,
            format!("{}", index + 1),
            FontId::new(10.5, FontFamily::Monospace),
            text_dim_color(),
        );
        let badge = tally_badge(tally);
        let name_width = ui.available_width() - if badge.is_some() { 48.0 } else { 0.0 };
        ui.scope(|ui| {
            ui.set_max_width(name_width.max(60.0));
            ui.add(
                egui::Label::new(RichText::new(name).size(13.0).strong().color(text_color()))
                    .truncate(),
            );
        });
        if let Some(badge) = badge {
            ui.with_layout(l10n::trailing_layout(l), |ui| {
                pill(ui, badge, tally_color(tally), true).on_hover_text(tally.operator_label(l));
            });
        }
    });
}

/// Tone of a tile-menu right-column marker for Remove (`PGM`, `PVW`, ...).
fn mark_color(mark: &str) -> Option<Color32> {
    match mark {
        "PGM" => Some(PROGRAM),
        "PVW" => Some(PREVIEW),
        mark if mark == Msg::InUse.en() || mark == Msg::Pending.en() => Some(AMBER),
        _ => None,
    }
}

impl StudioShell {
    /// The `+` tile at the end of the bank: opens the Add input dialog, or
    /// explains why adding is unavailable.
    fn draw_add_tile(&mut self, ui: &mut Ui, state: &StudioUiState, width: f32) {
        let l = self.locale;
        let gate = self.add_input_gate(state);
        let thumb_height = (width * 9.0 / 16.0).round();
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(width, thumb_height + TILE_NAME_HEIGHT),
            Sense::click(),
        );
        if ui.is_rect_visible(rect) {
            paint_add_tile(
                ui,
                rect,
                gate.enabled,
                gate.locked(),
                response.hovered(),
                response.has_focus(),
            );
        }
        let action = l.text(Msg::AddTileHover);
        let response = if gate.enabled {
            response.on_hover_text(action)
        } else {
            response.on_hover_text(l10n::disabled_text(l, action, gate.reason))
        };
        if response.clicked() && gate.enabled {
            self.open_add_input(state);
        }
        ui.horizontal(|ui| {
            ui.set_min_height(20.0);
            let count = state.view.as_ref().map_or(0, |view| view.inputs.len());
            match self.catalogue.first_add_name() {
                Some(name) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(l.format(Msg::AddingName, &[("name", Arg::Str(name))]))
                                .size(crate::theme::legible(10.0))
                                .strong()
                                .color(AMBER),
                        )
                        .truncate(),
                    );
                }
                None => {
                    ui.label(
                        mono(format!(
                            "{} / {}",
                            l.integer(count as u64),
                            l.integer(MAX_LIVE_INPUTS as u64)
                        ))
                        .size(crate::theme::legible(10.0))
                        .color(muted_color()),
                    )
                    .on_hover_text(l.text(Msg::InputCountHover));
                }
            }
        });
    }
}

/// The add tile's list of kinds along the bottom of `thumb`: one line when
/// it fits, else wrapped and centred, its last line where the one line sits.
fn paint_add_tile_kinds(painter: &egui::Painter, thumb: Rect, kinds: &str, color: Color32) {
    let font = FontId::new(crate::theme::legible(9.0), FontFamily::Proportional);
    let mut job =
        egui::text::LayoutJob::simple(kinds.to_owned(), font, color, thumb.width() - 12.0);
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    let line = galley
        .rows
        .last()
        .map_or(galley.size().y, |row| row.rect().height());
    let bottom = thumb.max.y - 10.0 + line / 2.0;
    painter.galley(
        pos2(thumb.center().x, bottom - galley.size().y),
        galley,
        color,
    );
}

#[allow(clippy::fn_params_excessive_bools)] // independent paint states
#[allow(clippy::too_many_lines)] // one painted tile
fn paint_add_tile(ui: &Ui, rect: Rect, enabled: bool, locked: bool, hovered: bool, focused: bool) {
    let l = l10n::locale(ui.ctx());
    let painter = ui.painter_at(rect.expand(4.0));
    let radius = CornerRadius::same(4);
    let thumb = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - TILE_NAME_HEIGHT));
    let name_bar = Rect::from_min_max(pos2(rect.min.x, thumb.max.y), rect.max);
    let lit = enabled && hovered;
    painter.rect_filled(
        rect,
        radius,
        if crate::theme::high_contrast() {
            if lit {
                crate::theme::widget_hover_color()
            } else {
                crate::theme::graphite_color()
            }
        } else if lit {
            Color32::from_rgb(14, 18, 24)
        } else {
            Color32::from_rgb(9, 10, 12)
        },
    );
    let accent = if lit {
        accent_color()
    } else if enabled {
        accent_color().gamma_multiply(0.75)
    } else {
        muted_color().gamma_multiply(0.6)
    };
    // A dashed outline reads as an empty slot rather than a source.
    let outline = rect.shrink(0.5);
    let corners = [
        outline.left_top(),
        outline.right_top(),
        outline.right_bottom(),
        outline.left_bottom(),
        outline.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(
        &corners,
        Stroke::new(
            crate::theme::line_width(),
            if lit {
                accent_color()
            } else {
                border_strong_color()
            },
        ),
        6.0,
        4.0,
    ));
    let center = thumb.center() + Vec2::new(0.0, 2.0);
    let size = (thumb.height() * 0.34).clamp(22.0, 54.0);
    painter.circle_stroke(center, size / 2.0, Stroke::new(1.5, accent));
    let arm = size * 0.22;
    for (from, to) in [
        (center - Vec2::new(arm, 0.0), center + Vec2::new(arm, 0.0)),
        (center - Vec2::new(0.0, arm), center + Vec2::new(0.0, arm)),
    ] {
        painter.line_segment([from, to], Stroke::new(2.0, accent));
    }
    paint_add_tile_kinds(
        &painter,
        thumb,
        l.text(Msg::AddTileKinds),
        crate::theme::faded(muted_color(), if enabled { 0.9 } else { 0.6 }),
    );
    painter.rect_filled(
        name_bar.shrink(1.0),
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 4,
            se: 4,
        },
        crate::theme::faded(raised_color(), 0.7),
    );
    let lock_room = if locked {
        let badge = Rect::from_center_size(
            pos2(
                name_bar.max.x - 4.0 - LOCK_BADGE_SIZE.x / 2.0,
                name_bar.center().y,
            ),
            Vec2::new(LOCK_BADGE_SIZE.x, LOCK_BADGE_SIZE.y - 2.0),
        );
        paint_lock_badge(&painter, badge);
        LOCK_BADGE_SIZE.x + 6.0
    } else {
        0.0
    };
    let label = elided(
        &painter,
        l.text(Msg::AddInput),
        FontId::new(12.0, FontFamily::Proportional),
        if enabled { text_color() } else { muted_color() },
        name_bar.width() - 12.0 - lock_room,
    );
    painter.galley(
        pos2(
            name_bar.min.x + 6.0,
            name_bar.center().y - label.size().y / 2.0,
        ),
        label,
        text_color(),
    );
    if focused {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(6),
            Stroke::new(crate::theme::focus_ring_width(), accent_color()),
            StrokeKind::Outside,
        );
    }
}

/// One muted line per disabled action group, so a greyed-out menu item is
/// explained without hovering it.
///
/// Remove shares the edit gate: when Rename and Move are unavailable their
/// note (the session or a padlock) covers Remove too, so Remove gets its own
/// note only for its own reasons, such as the input being on Program.
pub(crate) fn tile_menu_notes(
    l: Locale,
    gates: TileGates,
    edit: bool,
    remove_blocked: Option<&str>,
    reasons: TileReasons,
) -> Vec<String> {
    let note = |msg, reason: &str| l.format(msg, &[("reason", Arg::Str(l.tr(reason)))]);
    let mut notes = Vec::new();
    if !gates.preview {
        notes.push(note(Msg::NotePreviewCutUnavailable, reasons.preview));
    } else if !gates.transition {
        notes.push(note(Msg::NoteCutUnavailable, reasons.cut));
    }
    if !edit {
        notes.push(note(Msg::NoteEditUnavailable, reasons.edit));
    } else if let Some(reason) = remove_blocked {
        notes.push(note(Msg::NoteRemoveUnavailable, reason));
    }
    notes
}

/// Why each tile action is unavailable, when it is.
#[derive(Clone, Copy)]
pub(crate) struct TileReasons {
    pub(crate) preview: &'static str,
    pub(crate) cut: &'static str,
    pub(crate) edit: &'static str,
}

#[allow(clippy::struct_excessive_bools)] // independent per-tile gates
#[derive(Clone, Copy)]
struct TileContext<'a> {
    input: InputId,
    index: usize,
    name: &'a str,
    count: usize,
    width: f32,
    tally: crate::TallyState,
    /// Category, colour label, pause, close, and offline state.
    lifecycle: InputLifecycleStatus,
    /// The project's categories, for the hover's category name.
    categories: &'a [InputCategoryStatus],
    thumbnail: Option<(MonitorTap, MonitorPicture)>,
    enabled: bool,
    cut_enabled: bool,
    reorder_enabled: bool,
    /// A padlock blocks renaming or moving this input.
    locked: bool,
    /// Why moving left / right is blocked by a neighbour's padlock, if it is.
    move_locks: [Option<&'static str>; 2],
    /// Whether this input may be removed, and why not.
    removal: CatalogueAvailability,
    /// Why this input's grade cannot be edited (no `edit_project`, not
    /// ready, or a padlock), if it cannot.
    grade_reason: Option<&'static str>,
}

/// What a tile with no picture paints instead of its ordinal.
#[derive(Clone, Copy)]
enum AudioPlaceholder<'a> {
    /// Not an audio-only input: the dim ordinal.
    None,
    /// A system audio input: a meter placeholder from its meters, if any
    /// arrived.
    Meters(Option<&'a fm_protocol::InputAudioMeters>),
}

/// Per-frame facts that change how one tile is painted.
#[allow(clippy::struct_excessive_bools)] // independent interaction states
#[derive(Clone, Copy)]
struct TilePaint<'a> {
    index: usize,
    name: &'a str,
    tally: crate::TallyState,
    thumbnail: Option<(MonitorTap, MonitorPicture)>,
    /// The input's own live thumbnail when no tap frame shows it.
    own_thumbnail: Option<crate::input_thumbnail_ui::TileThumbnail>,
    /// Whether a system audio input's meters stand in for a picture.
    audio_placeholder: AudioPlaceholder<'a>,
    lifecycle: InputLifecycleStatus,
    enabled: bool,
    locked: bool,
    hovered: bool,
    focused: bool,
    pressed: bool,
    /// A stream or capture status badge covers the caption line.
    status_badge: bool,
    /// The console's selected input (RC-004).
    selected: bool,
}

#[allow(clippy::too_many_lines)] // one painter pass per tile region
fn paint_tile(ui: &Ui, rect: Rect, paint: TilePaint<'_>) {
    let TilePaint {
        index,
        name,
        tally,
        thumbnail,
        own_thumbnail,
        audio_placeholder,
        lifecycle,
        enabled,
        locked,
        hovered,
        focused,
        pressed,
        status_badge,
        selected,
    } = paint;
    let painter = ui.painter_at(rect.expand(4.0));
    let radius = CornerRadius::same(4);
    let thumb = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - TILE_NAME_HEIGHT));
    let name_bar = Rect::from_min_max(pos2(rect.min.x, thumb.max.y), rect.max);
    let border = tally_color(tally);
    let has_tally = !tally.is_none();

    painter.rect_filled(
        rect,
        radius,
        if hovered && enabled {
            if crate::theme::high_contrast() {
                crate::theme::widget_hover_color()
            } else {
                Color32::from_rgb(14, 16, 19)
            }
        } else {
            MONITOR_BLACK
        },
    );
    if let Some((tap, picture)) = thumbnail {
        painter.image(
            picture.texture,
            letterbox(thumb, picture.facts.width, picture.facts.height),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let tag = painter.layout_no_wrap(
            l10n::locale(ui.ctx())
                .tr(thumbnail_tag(tap, picture.facts.origin))
                .to_owned(),
            FontId::new(9.0, FontFamily::Proportional),
            text_color(),
        );
        paint_thumb_tag(&painter, thumb, index, tag, Color32::from_black_alpha(190));
    } else if let Some(own) = own_thumbnail {
        painter.image(
            own.texture,
            letterbox(thumb, own.width, own.height),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if own.stale() {
            // An out-of-date picture is dimmed, like a stale monitor well.
            painter.rect_filled(thumb, CornerRadius::same(4), Color32::from_black_alpha(140));
        }
        let tag = painter.layout_no_wrap(
            crate::input_thumbnail_ui::thumbnail_tag(l10n::locale(ui.ctx()), own),
            FontId::new(9.0, FontFamily::Proportional),
            text_color(),
        );
        let fill = if own.stale() {
            crate::theme::faded(AMBER, 0.85)
        } else {
            Color32::from_black_alpha(190)
        };
        paint_thumb_tag(&painter, thumb, index, tag, fill);
    } else if let AudioPlaceholder::Meters(meters) = audio_placeholder {
        crate::input_thumbnail_ui::paint_audio_placeholder(
            &painter,
            thumb,
            l10n::locale(ui.ctx()),
            meters,
            !status_badge,
        );
    } else {
        // Large dim ordinal, like a multiview label on an empty source.
        painter.text(
            thumb.center(),
            Align2::CENTER_CENTER,
            format!("{}", index + 1),
            FontId::new(
                (thumb.height() * 0.42).clamp(22.0, 48.0),
                FontFamily::Proportional,
            ),
            // High contrast draws the ordinal in the palette's muted text
            // (or the full tally hue) instead of a near-black grey.
            if has_tally {
                crate::theme::faded(border, 0.35)
            } else if crate::theme::high_contrast() {
                muted_color()
            } else {
                Color32::from_rgb(40, 44, 50)
            },
        );
        if !status_badge {
            painter.text(
                pos2(thumb.center().x, thumb.max.y - 10.0),
                Align2::CENTER_CENTER,
                l10n::locale(ui.ctx()).text(Msg::NoThumbnail),
                FontId::new(crate::theme::legible(9.0), FontFamily::Proportional),
                crate::theme::faded(muted_color(), 0.7),
            );
        }
    }
    if lifecycle.closed {
        // A closed input's source is released: dim whatever is shown.
        painter.rect_filled(thumb, radius, Color32::from_black_alpha(150));
    }
    if let Some(label) = lifecycle.color {
        // The colour label: a strip down the leading edge of the picture.
        painter.rect_filled(
            Rect::from_min_max(rect.min, pos2(rect.min.x + 4.0, thumb.max.y)),
            CornerRadius {
                nw: 4,
                ne: 0,
                sw: 0,
                se: 0,
            },
            label_color(label),
        );
    }
    if has_tally {
        // Broadcast-style tally strip across the top edge.
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), 4.0)),
            CornerRadius {
                nw: 4,
                ne: 4,
                sw: 0,
                se: 0,
            },
            border,
        );
    }
    // Every tile shows its bank number, as a key cap where a number key
    // previews it (1 to 8) and as a plain dim number past those.
    let key = number_rect(thumb, index);
    if index < 8 {
        painter.rect_filled(key, CornerRadius::same(3), raised_color());
        painter.rect_stroke(
            key,
            CornerRadius::same(3),
            Stroke::new(crate::theme::line_width(), border_strong_color()),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(key, CornerRadius::same(3), Color32::from_black_alpha(150));
    }
    painter.text(
        key.center(),
        Align2::CENTER_CENTER,
        format!("{}", index + 1),
        FontId::new(10.0, FontFamily::Monospace),
        text_dim_color(),
    );
    paint_lifecycle_badges(
        ui,
        &painter,
        thumb,
        lifecycle,
        if tally_badge(tally).is_some() {
            28.0
        } else {
            8.0
        },
    );
    if let Some(badge) = tally_badge(tally) {
        let galley = painter.layout_no_wrap(
            badge.to_owned(),
            FontId::new(10.0, FontFamily::Proportional),
            Color32::WHITE,
        );
        let badge_rect = Rect::from_min_size(
            pos2(thumb.max.x - galley.size().x - 16.0, thumb.min.y + 8.0),
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
            raised_color()
        },
    );
    let lock_room = if locked {
        let badge = Rect::from_center_size(
            pos2(
                name_bar.max.x - 4.0 - LOCK_BADGE_SIZE.x / 2.0,
                name_bar.center().y,
            ),
            Vec2::new(LOCK_BADGE_SIZE.x, LOCK_BADGE_SIZE.y - 2.0),
        );
        paint_lock_badge(&painter, badge);
        LOCK_BADGE_SIZE.x + 6.0
    } else {
        0.0
    };
    // One line, ending in an ellipsis; the tile tooltip carries the full name.
    let name_galley = elided(
        &painter,
        name,
        FontId::new(12.0, FontFamily::Proportional),
        text_color(),
        name_bar.width() - 12.0 - lock_room,
    );
    painter.galley(
        pos2(
            name_bar.min.x + 6.0,
            name_bar.center().y - name_galley.size().y / 2.0,
        ),
        name_galley,
        text_color(),
    );
    let stroke_width = if has_tally { 2.0 } else { 1.0 };
    let stroke_color = if hovered && enabled && !has_tally {
        accent_color().gamma_multiply(0.8)
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
    if pressed && enabled {
        painter.rect_filled(rect, radius, Color32::from_white_alpha(10));
    }
    if selected {
        // The selected input: an accent bar along the name's foot.
        painter.rect_filled(
            Rect::from_min_max(
                pos2(rect.min.x + 4.0, rect.max.y - 3.0),
                pos2(rect.max.x - 4.0, rect.max.y - 1.0),
            ),
            CornerRadius::same(1),
            accent_color(),
        );
    }
    if focused {
        // Keyboard focus ring outside the tally border.
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(6),
            Stroke::new(crate::theme::focus_ring_width(), accent_color()),
            StrokeKind::Outside,
        );
    }
}

/// The lifecycle badges (OFFLINE, CLOSED, PAUSED) in a row along the
/// picture's trailing top corner, `top` below its top edge.
/// Where a thumbnail's source tag (`FEED · SIM`, `PGM TAP`) goes in
/// `thumb`, `size` big: along the top edge after the tile's number key, a
/// corner pictures rarely use, clear of a lower third or caption along the
/// bottom and of the tally and lifecycle badges at the top right.
pub(crate) fn thumb_tag_rect(thumb: Rect, index: usize, size: Vec2) -> Rect {
    let left = number_rect(thumb, index).right() + 4.0;
    Rect::from_min_size(pos2(left, thumb.min.y + 8.0), Vec2::new(size.x + 8.0, 16.0))
}

/// Where a tile's bank number sits in `thumb`: the top-left corner, wide
/// enough for its digits.
fn number_rect(thumb: Rect, index: usize) -> Rect {
    let width = if index < 99 { 16.0 } else { 24.0 };
    Rect::from_min_size(thumb.min + Vec2::new(6.0, 8.0), Vec2::new(width, 16.0))
}

/// Paints a thumbnail's source tag `galley` on `fill` (see
/// [`thumb_tag_rect`]).
fn paint_thumb_tag(
    painter: &egui::Painter,
    thumb: Rect,
    index: usize,
    galley: std::sync::Arc<egui::Galley>,
    fill: Color32,
) {
    let tag_rect = thumb_tag_rect(thumb, index, galley.size());
    painter.rect_filled(tag_rect, CornerRadius::same(3), fill);
    painter.galley(
        pos2(
            tag_rect.min.x + 4.0,
            tag_rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color(),
    );
}

fn paint_lifecycle_badges(
    ui: &Ui,
    painter: &egui::Painter,
    thumb: Rect,
    lifecycle: InputLifecycleStatus,
    top: f32,
) {
    let l = l10n::locale(ui.ctx());
    let mut right = thumb.max.x - 6.0;
    for badge in lifecycle_badges(lifecycle) {
        let galley = painter.layout_no_wrap(
            badge.text(l).to_owned(),
            FontId::new(9.0, FontFamily::Proportional),
            Color32::WHITE,
        );
        let badge_rect = Rect::from_min_size(
            pos2(right - galley.size().x - 10.0, thumb.min.y + top),
            Vec2::new(galley.size().x + 10.0, 15.0),
        );
        painter.rect_filled(badge_rect, CornerRadius::same(3), badge.color());
        painter.galley(
            badge_rect.center() - galley.size() / 2.0,
            galley,
            Color32::WHITE,
        );
        right = badge_rect.min.x - 4.0;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    /// Regression (round 7): the title tile's `FEED · SIM` tag sat along
    /// the bottom, exactly where a lower third is drawn. It sits along the
    /// top, after the number key and short of the tally badge.
    #[test]
    fn the_source_tag_sits_along_the_top_clear_of_the_number_key() {
        let thumb = Rect::from_min_size(pos2(10.0, 20.0), Vec2::new(160.0, 90.0));
        for index in [0, 7, 12] {
            let tag = thumb_tag_rect(thumb, index, Vec2::new(44.0, 11.0));
            assert!(thumb.contains_rect(tag), "{index}: {tag:?}");
            assert!(tag.bottom() < thumb.top() + thumb.height() / 3.0);
            if index < 8 {
                // Right of the 16-point number key at 6 points in.
                assert!(tag.left() >= thumb.left() + 6.0 + 16.0);
            }
        }
    }

    #[test]
    fn tile_menu_notes_explain_each_disabled_group_once() {
        let reasons = TileReasons {
            preview: "no preview permission",
            cut: "a manual T-bar transition is in progress",
            edit: "this session lacks the edit_project permission",
        };
        let open = TileGates {
            preview: true,
            transition: true,
        };
        assert_eq!(
            tile_menu_notes(Locale::ENGLISH, open, true, None, reasons),
            Vec::<String>::new()
        );
        assert_eq!(
            tile_menu_notes(Locale::ENGLISH, open, false, Some("ignored"), reasons),
            [
                "Rename, Move, and Remove unavailable: this session lacks the edit_project permission"
            ]
        );
        assert_eq!(
            tile_menu_notes(
                Locale::ENGLISH,
                open,
                true,
                Some("this input is on Program"),
                reasons
            ),
            ["Remove unavailable: this input is on Program"]
        );
        let no_cut = TileGates {
            transition: false,
            ..open
        };
        assert_eq!(
            tile_menu_notes(Locale::ENGLISH, no_cut, true, None, reasons),
            ["Cut unavailable: a manual T-bar transition is in progress"]
        );
        // Without Preview selection, Cut is covered by the same note.
        let closed = TileGates {
            preview: false,
            transition: false,
        };
        assert_eq!(
            tile_menu_notes(Locale::ENGLISH, closed, false, None, reasons),
            [
                "Preview and Cut unavailable: no preview permission",
                "Rename, Move, and Remove unavailable: this session lacks the edit_project permission",
            ]
        );
    }

    #[test]
    fn tile_menu_notes_are_worded_in_the_console_language() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let reasons = TileReasons {
            preview: Msg::ReasonNoSelectPreview.en(),
            cut: Msg::ReasonManualInFlight.en(),
            edit: Msg::ReasonProjectLocked.en(),
        };
        let closed = TileGates {
            preview: false,
            transition: false,
        };
        assert_eq!(
            tile_menu_notes(spanish, closed, false, None, reasons),
            [
                "Previo y corte no disponibles: esta sesión no tiene el permiso select_preview",
                "Renombrar, mover y quitar no disponibles: el proyecto está bloqueado contra ediciones",
            ]
        );
    }

    #[test]
    fn moves_that_would_shift_a_locked_neighbour_are_blocked() {
        let inputs = [id(1), id(2), id(3)];
        let locks = EditLocks {
            project: false,
            inputs: vec![id(2)],
        };
        let reason = Some("the neighbouring input is locked against moves");
        assert_eq!(
            neighbour_move_lock_reason(&inputs, &locks, 0, InputMoveDirection::Down),
            reason
        );
        assert_eq!(
            neighbour_move_lock_reason(&inputs, &locks, 2, InputMoveDirection::Up),
            reason
        );
        assert_eq!(
            neighbour_move_lock_reason(&inputs, &locks, 0, InputMoveDirection::Up),
            None
        );
        assert_eq!(
            neighbour_move_lock_reason(&inputs, &locks, 2, InputMoveDirection::Down),
            None
        );
        assert_eq!(
            neighbour_move_lock_reason(&inputs, &EditLocks::default(), 1, InputMoveDirection::Up),
            None
        );
    }

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

    fn id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn frame(input: u128, origin: MonitorFrameOrigin, age_ms: u64) -> FrameFacts {
        FrameFacts {
            origin,
            input: WireInputId::from_domain(id(input)),
            width: 640,
            height: 360,
            age: std::time::Duration::from_millis(age_ms),
        }
    }

    fn switcher(program: u128, preview: u128) -> SwitcherState {
        let selection = fm_ui_model::BusSelection::new(id(program), id(preview));
        SwitcherState {
            desired: selection,
            realized: selection,
            desired_manual_transition: fm_ui_model::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_ui_model::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            runtime_generation: None,
        }
    }

    #[test]
    fn tiles_borrow_only_fresh_tap_frames_of_their_own_realized_source() {
        let sim = MonitorFrameOrigin::Simulated;
        let routing = switcher(2, 1);
        let frames = [Some(frame(1, sim, 100)), Some(frame(2, sim, 100))];
        assert_eq!(
            thumbnail_tap(id(1), routing, frames),
            Some(MonitorTap::Preview)
        );
        assert_eq!(
            thumbnail_tap(id(2), routing, frames),
            Some(MonitorTap::Program)
        );
        assert_eq!(thumbnail_tap(id(3), routing, frames), None);
        // Stale frames are never borrowed.
        assert_eq!(
            thumbnail_tap(id(2), routing, [None, Some(frame(2, sim, 1_500))]),
            None
        );
        // A frame of a source the tap no longer realizes is not borrowed.
        assert_eq!(
            thumbnail_tap(id(2), switcher(3, 1), [None, Some(frame(2, sim, 100))]),
            None
        );
        // The Program frame wins when one input is on both buses.
        assert_eq!(
            thumbnail_tap(
                id(4),
                switcher(4, 4),
                [Some(frame(4, sim, 100)), Some(frame(4, sim, 100))]
            ),
            Some(MonitorTap::Program)
        );
        assert_eq!(
            thumbnail_tap(
                id(4),
                switcher(4, 4),
                [Some(frame(4, MonitorFrameOrigin::Native, 100)), None]
            ),
            Some(MonitorTap::Preview)
        );
        assert_eq!(thumbnail_tap(id(1), routing, [None, None]), None);
    }

    #[test]
    fn thumbnail_tags_name_the_tap_and_simulated_origin() {
        assert_eq!(
            thumbnail_tag(MonitorTap::Program, MonitorFrameOrigin::Simulated),
            "PGM TAP · SIM"
        );
        assert_eq!(
            thumbnail_tag(MonitorTap::Preview, MonitorFrameOrigin::Native),
            "PVW TAP"
        );
    }

    #[test]
    fn tile_columns_fit_available_width() {
        assert_eq!(dynamic_columns(100.0, 8.0), 1);
        assert_eq!(dynamic_columns(MIN_TILE_WIDTH * 2.0 + 8.0, 8.0), 2);
        assert_eq!(dynamic_columns(MIN_TILE_WIDTH * 2.0 + 7.0, 8.0), 1);
    }

    #[test]
    fn tile_grid_terminates_for_unbounded_or_nan_widths() {
        // An infinite width used to loop forever adding columns.
        assert_eq!(dynamic_columns(f32::INFINITY, 8.0), 1);
        assert_eq!(dynamic_columns(f32::NAN, 8.0), 1);
        assert_eq!(bank_grid(f32::INFINITY, 400.0, 4, 8.0).0, 1);
    }

    #[test]
    fn bank_grid_grows_tiles_to_fill_tall_areas_and_scrolls_when_short() {
        // A wide, tall bank: four columns of large tiles fit in two rows.
        assert_eq!(bank_grid(1180.0, 560.0, 7, 8.0), (4, 289.0));
        // Huge area: tiles stop at the maximum width.
        assert_eq!(bank_grid(1180.0, 2_000.0, 7, 8.0), (1, MAX_TILE_WIDTH));
        // The 1920x1080 bank keeps four columns.
        assert_eq!(bank_grid(760.0, 480.0, 7, 8.0), (4, 184.0));
        // Too short for any grid: the densest grid of minimum tiles scrolls.
        assert_eq!(bank_grid(330.0, 80.0, 7, 8.0), (2, 161.0));
        assert_eq!(bank_grid(100.0, 80.0, 7, 8.0), (1, MIN_TILE_WIDTH));
        // Never more columns than inputs.
        assert_eq!(bank_grid(1180.0, 100.0, 2, 8.0).0, 2);
    }
}
