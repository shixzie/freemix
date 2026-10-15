//! Titles side tab: the operator's title editor.
//!
//! The panel owns the transport-free [`TitleEditor`]. Drafts are local
//! keystroke state, so they live on the UI thread and stay responsive while
//! the Studio worker waits on the network. The worker owns the protocol
//! bridge: a requested [`TitleCommand`] leaves as [`StudioIntent::Title`],
//! and the worker's answers come back as [`TitleUpdate`]s in
//! [`StudioUiState::title_updates`], which the host hands to
//! [`crate::StudioShell::receive_title_updates`] exactly once. Every frame
//! then syncs the editor with the replicated [`StudioUiState::project_state`].
//!
//! Field types, blocked reasons, rejections, field errors, and operation
//! labels are worded here from the console catalog; the model's `Display`
//! text is never shown.

use core::fmt::Write as _;

use egui::{
    Button, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Response, RichText, Sense,
    Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2,
};
use fm_protocol::{TitleFieldValue, WireTitleFieldId};
use fm_types::InputId;
use fm_ui_model::ClientView;
use fm_ui_model::title_editor::{
    FieldError, FieldPresentation, FieldType, PreviewSource, TitleAccess, TitleBlocked,
    TitleEditor, TitleEditorPresentation, TitleListEntry, TitleNotice, TitleOperation,
    TitlePreview, TitleRejection, TitleReply, format_field_value, parse_field_input,
};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::label_for_input;
use crate::theme::{
    ACCENT_FILL, AMBER, AMBER_FILL, ERROR, MONITOR_BLACK, PROGRAM, PROGRAM_FILL, Tone,
    accent_color, border_color, border_strong_color, caption, elided, empty_state, field_label,
    graphite_color, mono, muted_color, paint_lamp, raised_color, status_chip_painted, text_color,
    text_dim_color, widget_hover_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// A title-editor outcome the Studio worker relays to the UI-owned editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleUpdate {
    /// The authority answered a title command; apply it to the editor.
    Reply(TitleReply),
    /// The command's result will never arrive (not sent, connection lost,
    /// or terminally uncertain); unblock the title and keep its drafts.
    Abandoned { request: u64 },
}

impl TitleUpdate {
    /// The editor request this update answers.
    #[must_use]
    pub const fn request(&self) -> u64 {
        match self {
            Self::Reply(reply) => reply.request(),
            Self::Abandoned { request } => *request,
        }
    }
}

/// The title editor and the panel's local selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TitlesPanel {
    editor: TitleEditor,
    selected: Option<InputId>,
    /// The values the last preview armed, shown while they stay armed.
    preview: Option<TitlePreview>,
    /// Move keyboard focus to the editor's first field on the next draw,
    /// after a title row was selected.
    focus_field: bool,
    /// The title whose `Animate…` key or menu item was chosen this frame.
    animate: Option<InputId>,
}

/// The text field of `field` in `title`'s editor.
fn field_edit_id(title: InputId, field: WireTitleFieldId) -> egui::Id {
    egui::Id::new(("studio-title-field", title.get().get(), field.get().get()))
}

/// The editor access for `state`. Commands need a ready connection with
/// synchronized state; the edit and air permissions survive a disconnect
/// (`previous`) so the operator can keep typing while Studio reconnects.
pub(crate) fn title_access(state: &StudioUiState, previous: TitleAccess) -> TitleAccess {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.project_state.is_some()
        && state.view.is_some();
    if ready {
        TitleAccess::new(
            true,
            state.title_access.can_edit(),
            state.title_access.can_air(),
        )
    } else {
        TitleAccess::new(false, previous.can_edit(), previous.can_air())
    }
}

impl TitlesPanel {
    /// Applies worker answers in arrival order. Replies to requests the
    /// editor no longer waits for (already abandoned) are ignored.
    pub(crate) fn receive(&mut self, updates: impl IntoIterator<Item = TitleUpdate>) {
        for update in updates {
            match update {
                TitleUpdate::Reply(reply) => {
                    let _ = self.editor.apply_reply(reply);
                }
                TitleUpdate::Abandoned { request } => {
                    self.editor.abandon(request);
                }
            }
        }
    }

    /// Unblocks every title awaiting an answer from a worker that is gone.
    pub(crate) fn abandon_all(&mut self) {
        self.editor.abandon_all();
    }

    /// Updates access from the connection and observes the replicated
    /// titles. Without synchronized state the drafts are kept as they are.
    pub(crate) fn sync(&mut self, state: &StudioUiState) {
        self.editor
            .set_access(title_access(state, self.editor.access()));
        if let Some(project) = &state.project_state {
            // A refused snapshot keeps that title's last known values.
            let _ = self.editor.sync(project);
        }
        let list = self.editor.list();
        if !self
            .selected
            .is_some_and(|selected| list.iter().any(|entry| entry.title == selected))
        {
            self.selected = list.first().map(|entry| entry.title);
        }
        let previewed = self.selected.and_then(|title| {
            self.editor
                .presentation(title)
                .ok()
                .filter(|presentation| presentation.previewed)
                .map(|_| title)
        });
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| Some(preview.title) != previewed)
        {
            self.preview = None;
        }
    }

    /// The selected title, if any.
    pub(crate) const fn selected(&self) -> Option<InputId> {
        self.selected
    }

    /// The title the operator asked to animate this frame, if any.
    pub(crate) const fn take_animate_request(&mut self) -> Option<InputId> {
        self.animate.take()
    }

    #[cfg(test)]
    pub(crate) const fn editor(&self) -> &TitleEditor {
        &self.editor
    }

    #[cfg(test)]
    pub(crate) const fn editor_mut(&mut self) -> &mut TitleEditor {
        &mut self.editor
    }

    pub(crate) fn draw(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        ui.add(egui::Label::new(caption(l.text(Msg::TitlesIntro))).wrap());
        ui.add_space(2.0);
        let list = self.editor.list();
        if list.is_empty() {
            let (title, detail) = titles_empty_text(state);
            empty_state(ui, l.text(title), l.text(detail));
            return;
        }
        let view = state.view.as_ref();
        for entry in &list {
            let row = TitleRow::new(l, entry, view, Some(entry.title) == self.selected);
            let (response, animate) = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let width = (ui.available_width() - ANIMATE_KEY_WIDTH - 4.0).max(120.0);
                    let response = draw_title_row(ui, &row, width);
                    let animate = animate_key(ui, l);
                    (response, animate)
                })
                .inner;
            ui.add_space(2.0);
            let mut chosen = animate.clicked();
            response.context_menu(|ui| {
                if ui
                    .button(l.text(Msg::TitleAnimate))
                    .on_hover_text(animate_hover(l))
                    .clicked()
                {
                    chosen = true;
                    ui.close();
                }
            });
            if response.clicked() {
                self.selected = Some(entry.title);
                self.focus_field = true;
            }
            if chosen {
                self.selected = Some(entry.title);
                self.animate = Some(entry.title);
            }
        }
        ui.add_space(4.0);
        let Some(title) = self.selected else {
            return;
        };
        let Ok(presentation) = self.editor.presentation(title) else {
            return;
        };
        self.draw_editor(ui, state, &presentation, intents);
        // A selected title opens with its first field ready for typing.
        if std::mem::take(&mut self.focus_field)
            && presentation.can_edit
            && let Some(first) = presentation.fields.first()
        {
            ui.memory_mut(|memory| memory.request_focus(field_edit_id(title, first.id)));
        }
    }

    #[allow(clippy::too_many_lines)] // one immediate-mode card
    fn draw_editor(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        presentation: &TitleEditorPresentation,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        let title = presentation.title;
        let view = state.view.as_ref();
        let badge = title_air_badge(view, title, presentation.on_air);
        Frame::new()
            .fill(raised_color())
            .stroke(Stroke::new(
                crate::theme::line_width(),
                if badge == TitleAirBadge::OnAir {
                    PROGRAM.gamma_multiply(0.6)
                } else {
                    border_color()
                },
            ))
            .corner_radius(CornerRadius::same(5))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);
                draw_editor_header(ui, l, presentation, badge, view);
                if presentation.stale || presentation.notice == Some(TitleNotice::Conflict) {
                    self.draw_conflict_banner(ui, l, presentation);
                } else if let Some(TitleNotice::Rejected(reason)) = presentation.notice {
                    banner(ui, ERROR, &rejected_text(l, reason));
                }
                ui.add_space(2.0);
                for field in &presentation.fields {
                    self.draw_field(ui, l, presentation, field);
                }
                if presentation.previewed
                    && let Some(preview) = self.preview.as_ref().filter(|p| p.title == title)
                {
                    draw_preview(ui, l, preview);
                }
                ui.add_space(4.0);
                ui.separator();
                self.draw_actions(ui, state, presentation, intents);
                if presentation.pending {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0).color(AMBER));
                        ui.label(
                            RichText::new(l.text(Msg::TitlePending))
                                .size(crate::theme::legible(11.0))
                                .color(AMBER),
                        );
                    });
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            });
    }

    fn draw_conflict_banner(
        &mut self,
        ui: &mut Ui,
        l: Locale,
        presentation: &TitleEditorPresentation,
    ) {
        let title = presentation.title;
        banner(ui, AMBER, l.text(Msg::TitleConflictBanner));
        ui.horizontal(|ui| {
            let half = ((ui.available_width() - 6.0) / 2.0).max(60.0);
            let blocked = presentation
                .pending
                .then_some(Msg::TitleBlockedPending.en());
            if console_key(
                ui,
                l.text(Msg::TitleRebase),
                half,
                blocked.is_none(),
                KeyTone::Accent,
            )
            .hint(
                l.text(Msg::TitleRebaseHover),
                blocked.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            {
                let _ = self.editor.rebase(title);
            }
            if console_key(
                ui,
                l.text(Msg::TitleDiscardAll),
                half,
                true,
                KeyTone::Neutral,
            )
            .hint(l.text(Msg::TitleDiscardAllHover), "")
            .clicked()
            {
                let _ = self.editor.discard_all(title);
            }
        });
    }

    fn draw_field(
        &mut self,
        ui: &mut Ui,
        l: Locale,
        presentation: &TitleEditorPresentation,
        field: &FieldPresentation,
    ) {
        let title = presentation.title;
        let can_edit = presentation.can_edit;
        let marker = if field.error.is_some() {
            ERROR
        } else if field.dirty {
            accent_color()
        } else {
            Color32::TRANSPARENT
        };
        let top = ui.cursor().min;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&field.name)
                    .size(crate::theme::legible(11.5))
                    .strong()
                    .color(text_color()),
            );
            ui.label(
                RichText::new(l.text(field_type_label(field.field_type)))
                    .size(crate::theme::legible(9.0))
                    .extra_letter_spacing(0.8)
                    .color(muted_color()),
            );
            if field.changed_upstream && !field.dirty {
                ui.label(
                    RichText::new("●")
                        .size(crate::theme::legible(9.0))
                        .color(AMBER),
                )
                .on_hover_text(l.text(Msg::TitleFieldChanged));
            }
            if field.dirty {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            Button::new(
                                RichText::new(l.text(Msg::TitleFieldDiscard))
                                    .size(crate::theme::legible(10.5)),
                            )
                            .min_size(Vec2::new(0.0, 18.0)),
                        )
                        .on_hover_text(l.text(Msg::TitleFieldDiscardHover))
                        .clicked()
                    {
                        let _ = self.editor.discard_field(title, field.id);
                    }
                    ui.label(
                        RichText::new(l.text(Msg::TitleEdited))
                            .size(crate::theme::legible(9.0))
                            .extra_letter_spacing(0.8)
                            .color(accent_color()),
                    );
                });
            }
        });
        self.draw_field_input(ui, l, title, can_edit, field);
        let bottom = ui.min_rect().max.y;
        if marker != Color32::TRANSPARENT {
            let bar = egui::Rect::from_min_max(
                pos2(top.x - 6.0, top.y + 1.0),
                pos2(top.x - 4.0, bottom - 1.0),
            );
            ui.painter().rect_filled(bar, CornerRadius::same(1), marker);
        }
        if let Some(error) = field.error {
            ui.label(
                RichText::new(field_error_text(l, error))
                    .size(crate::theme::legible(10.5))
                    .color(ERROR),
            );
        }
        if let Some(conflict) = &field.conflict {
            Frame::new()
                .fill(AMBER.gamma_multiply(0.06))
                .stroke(Stroke::new(1.0, AMBER.gamma_multiply(0.35)))
                .corner_radius(CornerRadius::same(3))
                .inner_margin(Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    compare_row(
                        ui,
                        l.text(Msg::TitleMine),
                        accent_color(),
                        Some(&field.input),
                        None,
                    );
                    let theirs = conflict.theirs.as_ref().map(format_field_value);
                    compare_row(
                        ui,
                        l.text(Msg::TitleTheirs),
                        AMBER,
                        theirs.as_deref(),
                        conflict.theirs.as_ref(),
                    );
                });
        }
        ui.add_space(3.0);
    }

    /// The field's input row: a colour picker for colours, then the text edit.
    fn draw_field_input(
        &mut self,
        ui: &mut Ui,
        l: Locale,
        title: InputId,
        can_edit: bool,
        field: &FieldPresentation,
    ) {
        let edit_reason = Msg::ReasonNoEditProject.en();
        let mut input = field.input.clone();
        let id = field_edit_id(title, field.id);
        let changed = ui
            .horizontal(|ui| {
                let mut changed = false;
                if field.field_type == FieldType::Color {
                    let shown = parse_field_input(FieldType::Color, &field.input)
                        .ok()
                        .or_else(|| field.live_value.clone());
                    let mut color = shown.as_ref().map_or(Color32::TRANSPARENT, swatch_color);
                    let response = ui
                        .add_enabled_ui(can_edit, |ui| {
                            egui::color_picker::color_edit_button_srgba(
                                ui,
                                &mut color,
                                egui::color_picker::Alpha::OnlyBlend,
                            )
                        })
                        .inner
                        .hint(l.text(Msg::TitleColorPickHover), edit_reason);
                    if response.changed() {
                        let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
                        let _ = self.editor.set_value(
                            title,
                            field.id,
                            &TitleFieldValue::Color {
                                red,
                                green,
                                blue,
                                alpha,
                            },
                        );
                    }
                }
                let hint = match field.field_type {
                    FieldType::Color => l.text(Msg::TitleColorHint),
                    FieldType::Image => l.text(Msg::TitleImageHint),
                    FieldType::Text | FieldType::Number => "",
                };
                let mut edit = TextEdit::singleline(&mut input)
                    .id(id)
                    .hint_text(hint)
                    .desired_width(ui.available_width());
                if matches!(field.field_type, FieldType::Number | FieldType::Color) {
                    edit = edit.font(egui::TextStyle::Monospace);
                }
                let hover = l.text(match field.field_type {
                    FieldType::Text => Msg::TitleTextFieldHover,
                    FieldType::Number => Msg::TitleNumberFieldHover,
                    FieldType::Color => Msg::TitleColorFieldHover,
                    FieldType::Image => Msg::TitleImageFieldHover,
                });
                let response = ui.add_enabled(can_edit, edit).hint(hover, edit_reason);
                changed |= response.changed();
                changed
            })
            .inner;
        if changed {
            let _ = self.editor.set_input(title, field.id, &input);
        }
    }

    #[allow(clippy::too_many_lines)] // one row of console keys per action group
    fn draw_actions(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        presentation: &TitleEditorPresentation,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        let title = presentation.title;
        let gap = 6.0;
        let full = ui.available_width();
        let half = ((full - gap) / 2.0).floor().max(60.0);
        let operation = |operation: TitleOperation| {
            presentation
                .operations
                .iter()
                .find(|candidate| candidate.operation == operation)
                .map_or(Err(TitleBlocked::NotReady), |candidate| candidate.state)
        };
        let mut request = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let preview_blocked = if presentation.stale {
                Some(TitleBlocked::Stale)
            } else if presentation
                .fields
                .iter()
                .any(|field| field.error.is_some())
            {
                Some(TitleBlocked::InvalidDraft)
            } else {
                None
            };
            let tone = if presentation.previewed {
                KeyTone::Accent
            } else {
                KeyTone::Neutral
            };
            if console_key(
                ui,
                l.text(Msg::TitlePreviewKey),
                half,
                preview_blocked.is_none(),
                tone,
            )
            .hint(
                l.text(Msg::TitlePreviewHover),
                preview_blocked.map_or("", |blocked| {
                    blocked_reason(blocked, TitleOperation::Take, state)
                }),
            )
            .clicked()
            {
                self.preview = self.editor.preview(title).ok();
            }
            request = request.or(operation_key(
                ui,
                l,
                TitleOperation::Save,
                operation(TitleOperation::Save),
                half,
                presentation,
                state,
            ));
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.label(field_label(l.text(Msg::TitleChannel)))
                .on_hover_text(l.text(Msg::TitleChannelHover));
            let mut channel = presentation.take_channel;
            egui::ComboBox::from_id_salt(("studio-title-channel", title.get().get()))
                .selected_text(RichText::new(format!("O{channel}")).size(12.0).strong())
                .width(58.0)
                .show_ui(ui, |ui| {
                    for number in 1_u8..=8 {
                        ui.selectable_value(&mut channel, number, format!("O{number}"))
                            .on_hover_text(l.format(
                                Msg::TitleChannelOptionHover,
                                &[("channel", Arg::Plain(number.into()))],
                            ));
                    }
                })
                .response
                .on_hover_text(l.text(Msg::TitleChannelHover));
            if channel != presentation.take_channel {
                let _ = self.editor.set_take_channel(title, channel);
            }
            let width = ui.available_width().max(60.0);
            request = request.or(operation_key(
                ui,
                l,
                TitleOperation::Take,
                operation(TitleOperation::Take),
                width,
                presentation,
                state,
            ));
        });
        if let Some(busy) = view_channel_busy(state.view.as_ref(), presentation.take_channel, title)
            && let Some(view) = state.view.as_ref()
        {
            ui.add(
                egui::Label::new(caption(l.format(
                    Msg::TitleChannelBusy,
                    &[
                        ("channel", Arg::Plain(u64::from(presentation.take_channel))),
                        ("name", Arg::Str(&label_for_input(l, view, busy))),
                    ],
                )))
                .wrap(),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for op in [TitleOperation::Update, TitleOperation::OffAir] {
                request = request.or(operation_key(
                    ui,
                    l,
                    op,
                    operation(op),
                    half,
                    presentation,
                    state,
                ));
            }
        });
        if let Some(op) = request
            && let Ok(command) = self.editor.request(title, op)
        {
            intents.push(StudioIntent::Title(command));
        }
    }
}

/// The empty-state title and detail when the editor knows no titles.
pub(crate) fn titles_empty_text(state: &StudioUiState) -> (Msg, Msg) {
    match &state.project_state {
        None => (Msg::TitlesNone, Msg::TitlesNotSynced),
        Some(project) if !project.titles().is_empty() => (Msg::TitlesNone, Msg::TitlesWaiting),
        Some(_) => (Msg::TitlesNone, Msg::TitlesNoneDetail),
    }
}

/// A title's air state from the desired and confirmed routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TitleAirBadge {
    /// Desired on Program, in the desired manual transition, or keyed on a
    /// desired active overlay.
    OnAir,
    /// No longer desired on air, but still fading out or still shown by the
    /// confirmed (realized) routing.
    Clearing,
    Off,
}

impl TitleAirBadge {
    pub(crate) const fn label(self) -> Msg {
        match self {
            Self::OnAir => Msg::OverlayOnAir,
            Self::Clearing => Msg::OverlayClearing,
            Self::Off => Msg::TitleOff,
        }
    }

    pub(crate) const fn tone(self) -> Tone {
        match self {
            Self::OnAir => Tone::OnAir,
            Self::Clearing => Tone::Pending,
            Self::Off => Tone::Idle,
        }
    }
}

/// `desired_on_air` is the replicated rule the editor uses (a desired
/// overlay that is shown or still fading counts as on air).
pub(crate) fn title_air_badge(
    view: Option<&ClientView>,
    title: InputId,
    desired_on_air: bool,
) -> TitleAirBadge {
    let Some(view) = view else {
        return if desired_on_air {
            TitleAirBadge::OnAir
        } else {
            TitleAirBadge::Off
        };
    };
    let manual = match view.switcher.desired_manual_transition {
        fm_ui_model::ManualTransitionStatus::Active(transition) => {
            transition.from == title || transition.to == title
        }
        fm_ui_model::ManualTransitionStatus::Inactive => false,
    };
    let desired_active = view.switcher.desired.program == title
        || manual
        || view
            .desired_overlays
            .iter()
            .any(|overlay| overlay.active && overlay.source == Some(title));
    let realized_manual = match view.switcher.realized_manual_transition {
        fm_ui_model::ManualTransitionStatus::Active(transition) => {
            transition.from == title || transition.to == title
        }
        fm_ui_model::ManualTransitionStatus::Inactive => false,
    };
    let realized_shown = view.switcher.realized.program == title
        || realized_manual
        || view.realized_overlays.iter().any(|overlay| {
            overlay.source == Some(title) && (overlay.active || overlay.opacity > 0)
        });
    if desired_active {
        TitleAirBadge::OnAir
    } else if desired_on_air || realized_shown {
        TitleAirBadge::Clearing
    } else {
        TitleAirBadge::Off
    }
}

/// Where a title is desired on air: Program, or the lowest overlay channel
/// keying it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TitleLocation {
    Program,
    Overlay(u8),
}

pub(crate) fn title_location(view: Option<&ClientView>, title: InputId) -> Option<TitleLocation> {
    let view = view?;
    if view.switcher.desired.program == title {
        return Some(TitleLocation::Program);
    }
    view.desired_overlays
        .iter()
        .filter(|overlay| overlay.active && overlay.source == Some(title))
        .map(|overlay| overlay.channel)
        .min()
        .map(TitleLocation::Overlay)
}

fn location_text(l: Locale, location: TitleLocation) -> String {
    match location {
        TitleLocation::Program => l.text(Msg::TitleOnProgram).to_owned(),
        TitleLocation::Overlay(channel) => l.format(
            Msg::TitleOnChannel,
            &[("channel", Arg::Plain(u64::from(channel)))],
        ),
    }
}

/// Another source the take channel currently shows, which a Take replaces.
fn view_channel_busy(view: Option<&ClientView>, channel: u8, title: InputId) -> Option<InputId> {
    view?
        .desired_overlays
        .iter()
        .find(|overlay| overlay.channel == channel && overlay.active)
        .and_then(|overlay| overlay.source)
        .filter(|source| *source != title)
}

pub(crate) const fn field_type_label(field_type: FieldType) -> Msg {
    match field_type {
        FieldType::Text => Msg::FieldTypeText,
        FieldType::Number => Msg::FieldTypeNumber,
        FieldType::Color => Msg::FieldTypeColor,
        FieldType::Image => Msg::FieldTypeImage,
    }
}

/// An operation's key label and its enabled tooltip.
pub(crate) const fn operation_label(operation: TitleOperation) -> (Msg, Msg) {
    match operation {
        TitleOperation::Save => (Msg::TitleOpSave, Msg::TitleOpSaveHover),
        TitleOperation::Take => (Msg::TitleOpTake, Msg::TitleOpTakeHover),
        TitleOperation::Update => (Msg::TitleOpUpdate, Msg::TitleOpUpdateHover),
        TitleOperation::OffAir => (Msg::TitleOpOffAir, Msg::TitleOpOffAirHover),
    }
}

/// Why `operation` is unavailable, as catalog English for
/// [`Locale::tr`]. Connection and permission blocks name the same session
/// gate as every other console control.
pub(crate) fn blocked_reason(
    blocked: TitleBlocked,
    operation: TitleOperation,
    state: &StudioUiState,
) -> &'static str {
    let capability = match operation {
        TitleOperation::Save | TitleOperation::Update => Capability::EditProject,
        TitleOperation::Take | TitleOperation::OffAir => Capability::Transition,
    };
    match blocked {
        TitleBlocked::NotReady => unavailable_reason(state, capability),
        TitleBlocked::NotPermitted => match operation {
            TitleOperation::Save | TitleOperation::Update => Msg::ReasonNoEditProject.en(),
            TitleOperation::Take if state.title_access.can_air() => Msg::ReasonNoEditProject.en(),
            TitleOperation::Take | TitleOperation::OffAir => Msg::ReasonNoTransition.en(),
        },
        TitleBlocked::Pending => Msg::TitleBlockedPending.en(),
        TitleBlocked::Stale => Msg::TitleBlockedStale.en(),
        TitleBlocked::InvalidDraft => Msg::TitleBlockedInvalid.en(),
        TitleBlocked::ContentTooLarge => Msg::TitleRejTooLarge.en(),
        TitleBlocked::NotPreviewed => Msg::TitleBlockedNotPreviewed.en(),
        TitleBlocked::NoChanges => Msg::TitleBlockedNoChanges.en(),
        TitleBlocked::OnAir => Msg::TitleBlockedOnAir.en(),
        TitleBlocked::OffAir => Msg::TitleBlockedOffAir.en(),
    }
}

pub(crate) const fn rejection_label(reason: TitleRejection) -> Msg {
    match reason {
        TitleRejection::UnknownTitle => Msg::TitleRejUnknownTitle,
        TitleRejection::UnknownField(_) => Msg::TitleRejUnknownField,
        TitleRejection::DuplicateField(_) => Msg::TitleRejDuplicateField,
        TitleRejection::TypeMismatch { .. } => Msg::TitleRejTypeMismatch,
        TitleRejection::InvalidValue(_) => Msg::TitleRejInvalidValue,
        TitleRejection::InvalidFields => Msg::TitleRejInvalidFields,
        TitleRejection::ContentTooLarge => Msg::TitleRejTooLarge,
        TitleRejection::AlreadyOnAir => Msg::TitleRejAlreadyOnAir,
        TitleRejection::NotOnAir => Msg::TitleRejNotOnAir,
        TitleRejection::OnProgram => Msg::TitleRejOnProgram,
        TitleRejection::OnMultipleOverlays => Msg::TitleRejMultipleOverlays,
        TitleRejection::Locked => Msg::TitleRejLocked,
        TitleRejection::NotPermitted => Msg::TitleRejNotPermitted,
        TitleRejection::RevisionExhausted => Msg::TitleRejRevisionExhausted,
        TitleRejection::Other => Msg::TitleRejOther,
    }
}

pub(crate) fn rejected_text(l: Locale, reason: TitleRejection) -> String {
    l.format(
        Msg::TitleRejectedBanner,
        &[("reason", Arg::Str(l.text(rejection_label(reason))))],
    )
}

pub(crate) fn field_error_text(l: Locale, error: FieldError) -> String {
    let max = |msg, max_chars: usize| {
        l.format(
            msg,
            &[(
                "max",
                Arg::Count(u64::try_from(max_chars).unwrap_or(u64::MAX)),
            )],
        )
    };
    match error {
        FieldError::TextTooLong { max_chars } => max(Msg::FieldErrTextTooLong, max_chars),
        FieldError::InvalidNumber => l.text(Msg::FieldErrNumber).to_owned(),
        FieldError::InvalidColor => l.text(Msg::FieldErrColor).to_owned(),
        FieldError::EmptyImageAsset => l.text(Msg::FieldErrEmptyImage).to_owned(),
        FieldError::ImageAssetTooLong { max_chars } => max(Msg::FieldErrImageTooLong, max_chars),
        FieldError::TypeMismatch { expected, actual } => l.format(
            Msg::FieldErrTypeMismatch,
            &[
                (
                    "expected",
                    Arg::Str(&l.text(field_type_label(expected)).to_lowercase()),
                ),
                (
                    "actual",
                    Arg::Str(&l.text(field_type_label(actual)).to_lowercase()),
                ),
            ],
        ),
        FieldError::FieldRemoved => l.text(Msg::FieldErrRemoved).to_owned(),
    }
}

fn swatch_color(value: &TitleFieldValue) -> Color32 {
    match value {
        TitleFieldValue::Color {
            red,
            green,
            blue,
            alpha,
        } => Color32::from_rgba_unmultiplied(*red, *green, *blue, *alpha),
        _ => Color32::TRANSPARENT,
    }
}

/// The operator's name for a title: its input name as the input bank shows
/// it, else the template name the daemon reports.
pub(crate) fn title_display_name(
    l: Locale,
    view: Option<&ClientView>,
    title: InputId,
    template: &str,
) -> String {
    view.filter(|view| view.inputs.contains(&title))
        .map_or_else(
            || template.to_owned(),
            |view| label_for_input(l, view, title),
        )
}

/// One list row's facts.
struct TitleRow<'a> {
    entry: &'a TitleListEntry,
    name: String,
    badge: TitleAirBadge,
    location: Option<TitleLocation>,
    selected: bool,
}

impl<'a> TitleRow<'a> {
    fn new(
        l: Locale,
        entry: &'a TitleListEntry,
        view: Option<&ClientView>,
        selected: bool,
    ) -> Self {
        Self {
            entry,
            name: title_display_name(l, view, entry.title, &entry.name),
            badge: title_air_badge(view, entry.title, entry.on_air),
            location: title_location(view, entry.title),
            selected,
        }
    }

    /// The second line: where the title is on air, else its template.
    fn detail(&self, l: Locale) -> Option<String> {
        if self.badge == TitleAirBadge::OnAir
            && let Some(location) = self.location
        {
            return Some(location_text(l, location));
        }
        (!self.entry.name.eq_ignore_ascii_case(&self.name)).then(|| self.entry.name.clone())
    }
}

const TITLE_ROW_HEIGHT: f32 = 34.0;
const BADGE_WIDTH: f32 = 84.0;
/// Width of the `Animate…` key beside each title row.
const ANIMATE_KEY_WIDTH: f32 = 72.0;

/// The hover help of the `Animate…` key and menu item.
fn animate_hover(l: Locale) -> String {
    l10n::with_key(
        l,
        l.text(Msg::TitleAnimateHover),
        crate::studio_animation::ANIMATION_SHORTCUT,
    )
}

/// The `Animate…` key beside a title row.
fn animate_key(ui: &mut Ui, l: Locale) -> Response {
    ui.add(
        Button::new(
            RichText::new(l.text(Msg::TitleAnimate))
                .size(crate::theme::legible(10.5))
                .color(text_color()),
        )
        .min_size(Vec2::new(ANIMATE_KEY_WIDTH, TITLE_ROW_HEIGHT)),
    )
    .on_hover_text(animate_hover(l))
}

/// One fixed-height, clickable title row `width` wide: lamp, name, edit
/// markers, and the air badge.
fn draw_title_row(ui: &mut Ui, row: &TitleRow<'_>, width: f32) -> Response {
    let l = l10n::locale(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, TITLE_ROW_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let tone = row.badge.tone();
        let fill = if response.hovered() {
            widget_hover_color()
        } else if row.selected {
            raised_color()
        } else {
            graphite_color()
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        let stroke = if response.has_focus() {
            Stroke::new(crate::theme::focus_ring_width(), accent_color())
        } else if row.selected {
            Stroke::new(1.5, accent_color().gamma_multiply(0.8))
        } else if tone == Tone::Idle {
            Stroke::new(crate::theme::line_width(), border_color())
        } else {
            Stroke::new(1.0, tone.color().gamma_multiply(0.55))
        };
        painter.rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
        paint_lamp(painter, pos2(rect.min.x + 12.0, rect.center().y), 3.5, tone);
        let badge_rect = egui::Rect::from_min_size(
            pos2(rect.max.x - 8.0 - BADGE_WIDTH, rect.center().y - 9.0),
            Vec2::new(BADGE_WIDTH, 18.0),
        );
        status_chip_painted(painter, badge_rect, row.badge.label().en(), tone);
        let mut markers = String::new();
        if row.entry.stale {
            markers.push_str(l.text(Msg::TitleConflict));
        } else if row.entry.dirty {
            markers.push_str(l.text(Msg::TitleEdited));
        }
        let marker_color = if row.entry.stale {
            AMBER
        } else {
            accent_color()
        };
        let marker = elided(
            painter,
            &markers,
            FontId::new(9.0, FontFamily::Proportional),
            marker_color,
            70.0,
        );
        let marker_x = badge_rect.min.x - 8.0 - marker.size().x;
        let name_width = (marker_x - (rect.min.x + 24.0) - 6.0).max(20.0);
        let name = elided(
            painter,
            &row.name,
            FontId::new(12.5, FontFamily::Proportional),
            text_color(),
            name_width,
        );
        let detail = row.detail(l);
        let name_y = if detail.is_some() {
            rect.min.y + 11.0
        } else {
            rect.center().y
        };
        painter.galley(
            pos2(rect.min.x + 24.0, name_y - name.size().y / 2.0),
            name,
            text_color(),
        );
        if let Some(detail) = detail {
            let detail = elided(
                painter,
                &detail,
                FontId::new(9.5, FontFamily::Proportional),
                text_dim_color(),
                name_width,
            );
            painter.galley(
                pos2(rect.min.x + 24.0, rect.min.y + 24.0 - detail.size().y / 2.0),
                detail,
                text_dim_color(),
            );
        }
        if !markers.is_empty() {
            painter.galley(
                pos2(marker_x, rect.center().y - marker.size().y / 2.0),
                marker,
                marker_color,
            );
        }
    }
    response.on_hover_text(l.format(Msg::TitleRowHover, &[("name", Arg::Str(&row.name))]))
}

fn draw_editor_header(
    ui: &mut Ui,
    l: Locale,
    presentation: &TitleEditorPresentation,
    badge: TitleAirBadge,
    view: Option<&ClientView>,
) {
    let name = title_display_name(l, view, presentation.title, &presentation.name);
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(RichText::new(&name).size(13.5).strong().color(text_color()))
                .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                mono(l.format(
                    Msg::RevisionShort,
                    &[("revision", Arg::Plain(presentation.live_revision))],
                ))
                .size(crate::theme::legible(10.5))
                .color(muted_color()),
            );
            crate::theme::status_chip(ui, badge.label().en(), badge.tone(), BADGE_WIDTH);
            if badge == TitleAirBadge::OnAir
                && let Some(location) = title_location(view, presentation.title)
            {
                ui.label(
                    RichText::new(location_text(l, location))
                        .size(crate::theme::legible(10.5))
                        .color(text_dim_color()),
                );
            }
        });
    });
    if !name.eq_ignore_ascii_case(&presentation.name) {
        ui.label(caption(&presentation.name));
    }
}

/// A full-width tinted notice line.
fn banner(ui: &mut Ui, color: Color32, text: &str) {
    Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .size(crate::theme::legible(11.0))
                        .color(color),
                )
                .wrap(),
            );
        });
}

/// `MINE  value` / `THEIRS  value` in a conflict comparison.
fn compare_row(
    ui: &mut Ui,
    label: &str,
    color: Color32,
    value: Option<&str>,
    swatch: Option<&TitleFieldValue>,
) {
    ui.horizontal(|ui| {
        fixed_column(ui, 52.0, |ui| {
            ui.label(
                RichText::new(label)
                    .size(crate::theme::legible(9.5))
                    .strong()
                    .extra_letter_spacing(0.8)
                    .color(color),
            );
        });
        if let Some(TitleFieldValue::Color { .. }) = swatch {
            paint_swatch(ui, swatch.map_or(Color32::TRANSPARENT, swatch_color));
        }
        match value {
            Some(value) => ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .size(crate::theme::legible(11.0))
                        .color(text_color()),
                )
                .truncate(),
            ),
            None => ui.label(
                RichText::new(l10n::locale(ui.ctx()).text(Msg::TitleFieldRemovedValue))
                    .size(crate::theme::legible(11.0))
                    .italics()
                    .color(muted_color()),
            ),
        };
    });
}

/// A leading-aligned column of fixed width, so rows line up.
fn fixed_column(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui)) {
    let height = ui.spacing().interact_size.y.min(16.0);
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        l10n::leading_layout(l10n::locale(ui.ctx())),
        |ui| {
            ui.set_min_width(width);
            add_contents(ui);
        },
    );
}

fn paint_swatch(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), MONITOR_BLACK);
    painter.rect_filled(rect, CornerRadius::same(2), color);
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(crate::theme::line_width(), border_strong_color()),
        StrokeKind::Inside,
    );
}

/// The armed values a Take or Update would air.
fn draw_preview(ui: &mut Ui, l: Locale, preview: &TitlePreview) {
    Frame::new()
        .fill(accent_color().gamma_multiply(0.06))
        .stroke(Stroke::new(1.0, accent_color().gamma_multiply(0.4)))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(l.text(Msg::TitlePreviewed))
                        .size(crate::theme::legible(9.5))
                        .strong()
                        .extra_letter_spacing(1.0)
                        .color(accent_color()),
                );
                ui.label(caption(l.text(Msg::TitlePreviewHeading)));
            });
            for field in &preview.fields {
                ui.horizontal(|ui| {
                    fixed_column(ui, 96.0, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&field.name)
                                    .size(crate::theme::legible(10.5))
                                    .color(text_dim_color()),
                            )
                            .truncate(),
                        );
                    });
                    if matches!(field.value, TitleFieldValue::Color { .. }) {
                        paint_swatch(ui, swatch_color(&field.value));
                    }
                    let draft = field.source == PreviewSource::Draft;
                    let mut text = format_field_value(&field.value);
                    if draft {
                        let _ = write!(text, "  · {}", l.text(Msg::TitleFromDraft));
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .size(crate::theme::legible(11.0))
                                .color(if draft { accent_color() } else { text_color() }),
                        )
                        .truncate(),
                    );
                });
            }
        });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyTone {
    Neutral,
    Accent,
    /// Lit red: an on-air action on an on-air title.
    OnAir,
    Amber,
}

/// A fixed-size console key; disabled keys keep their size and dim.
fn console_key(ui: &mut Ui, text: &str, width: f32, enabled: bool, tone: KeyTone) -> Response {
    let (fill, stroke) = match tone {
        KeyTone::Neutral => (crate::theme::widget_color(), border_strong_color()),
        KeyTone::Accent => (ACCENT_FILL, accent_color().gamma_multiply(0.7)),
        KeyTone::OnAir => (PROGRAM_FILL, PROGRAM.gamma_multiply(0.8)),
        KeyTone::Amber => (AMBER_FILL, AMBER.gamma_multiply(0.7)),
    };
    let (fill, stroke) = if enabled {
        (fill, stroke)
    } else {
        (crate::theme::widget_color(), border_color())
    };
    ui.add_enabled(
        enabled,
        Button::new(
            RichText::new(text.to_uppercase())
                .size(crate::theme::legible(11.0))
                .strong()
                .extra_letter_spacing(0.6)
                .color(text_color()),
        )
        .truncate()
        .fill(fill)
        .stroke(Stroke::new(crate::theme::line_width(), stroke))
        .corner_radius(CornerRadius::same(crate::theme::RADIUS))
        .min_size(Vec2::new(width, 26.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Draws one operation key with its enabled or blocked tooltip and returns
/// the operation when clicked.
fn operation_key(
    ui: &mut Ui,
    l: Locale,
    operation: TitleOperation,
    availability: Result<(), TitleBlocked>,
    width: f32,
    presentation: &TitleEditorPresentation,
    state: &StudioUiState,
) -> Option<TitleOperation> {
    let (label, hover) = operation_label(operation);
    let enabled = availability.is_ok();
    let tone = match operation {
        TitleOperation::Take | TitleOperation::Update => KeyTone::Accent,
        TitleOperation::OffAir if presentation.on_air => KeyTone::OnAir,
        TitleOperation::Save if presentation.dirty => KeyTone::Amber,
        TitleOperation::Save | TitleOperation::OffAir => KeyTone::Neutral,
    };
    let action = if operation == TitleOperation::Take {
        l.format(
            hover,
            &[("channel", Arg::Plain(u64::from(presentation.take_channel)))],
        )
    } else {
        l.text(hover).to_owned()
    };
    let reason = availability
        .err()
        .map_or("", |blocked| blocked_reason(blocked, operation, state));
    let response = console_key(ui, l.text(label), width, enabled, tone).hint(&action, reason);
    response.clicked().then_some(operation)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{
        EngineIdentity, FadeToBlackPosition, FadeToBlackState, InputStatus, OverlayBorderPreset,
        OverlayPositionPreset, OverlayTransitionKind, RecordingStatus, SnapshotMessage,
        TitleFieldStatus, TitleStateMessage, TitleSummary, WireInputId, WireRecorderSlot,
        WireTitleFieldId,
    };
    use fm_types::ProjectId;
    use fm_ui_model::i18n::Language;
    use fm_ui_model::title_editor::{TitleCommandBody, TitleSnapshot};
    use fm_ui_model::{ClientModel, OverlayStatus, ProjectSnapshot};

    use super::*;
    use crate::StudioShell;
    use crate::panels::SideTab;

    const TITLE: u128 = 3;
    const ES: Locale = Locale::new(Language::Spanish);

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn overlays() -> Vec<OverlayStatus> {
        (1..=8)
            .map(|channel| OverlayStatus {
                channel,
                source: None,
                active: false,
                opacity: 0,
                transition: OverlayTransitionKind::Cut,
                duration_frames: 1,
                position: OverlayPositionPreset::FullFrame,
                border: OverlayBorderPreset::None,
                queued_sources: Vec::new(),
                included_outputs: Vec::new(),
            })
            .collect()
    }

    fn keyed(source: u128, active: bool, opacity: u8) -> Vec<OverlayStatus> {
        let mut overlays = overlays();
        overlays[1].source = Some(input(source));
        overlays[1].active = active;
        overlays[1].opacity = opacity;
        overlays
    }

    /// A synchronized model whose input 3 is a complete title with text,
    /// number, colour, and image fields; overlay channel 2 shows
    /// `desired`/`realized`.
    #[allow(clippy::too_many_lines)] // one protocol fixture
    fn model(desired: Vec<OverlayStatus>, realized: Vec<OverlayStatus>) -> ClientModel {
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let fade = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        let wire = WireInputId::from_domain;
        let names = ["Camera", "Slides", "Lower third"];
        let protocol = |overlays: Vec<OverlayStatus>| {
            overlays
                .into_iter()
                .map(|overlay| fm_protocol::OverlayStatus {
                    channel: fm_protocol::WireOverlayChannelId::new(overlay.channel).unwrap(),
                    source: overlay.source.map(wire),
                    active: overlay.active,
                    opacity: overlay.opacity,
                    transition: overlay.transition,
                    duration_frames: overlay.duration_frames,
                    position: overlay.position,
                    border: overlay.border,
                    queued_sources: Vec::new(),
                    included_outputs: Vec::new(),
                })
                .collect::<Vec<_>>()
        };
        let snapshot = SnapshotMessage {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 10,
            show_name: "Show".into(),
            inputs: (1..=3)
                .map(|value| InputStatus {
                    input: wire(input(value)),
                    name: names[usize::try_from(value).unwrap() - 1].into(),
                })
                .collect(),
            outputs: Vec::new(),
            input_audio_strips: (1..=3)
                .map(|value| fm_protocol::InputAudioStripStatus {
                    input: wire(input(value)),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .collect(),
            desired_program: wire(input(1)),
            desired_preview: wire(input(2)),
            realized_program: wire(input(1)),
            realized_preview: wire(input(2)),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fade,
            realized_fade_to_black: fade,
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| RecordingStatus {
                    recorder: WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: protocol(desired),
            realized_overlays: protocol(realized),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: vec![TitleSummary {
                input: wire(input(TITLE)),
                field_revision: 2,
            }],
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        };
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        model
            .apply_title_state(TitleStateMessage {
                revision: 10,
                input: WireInputId::from_domain(input(TITLE)),
                name: "Lower third".into(),
                field_revision: 2,
                phase: fm_protocol::TitlePhase::Idle,
                phase_frame: 0,
                phase_frames: 0,
                fields: vec![
                    TitleFieldStatus {
                        field: field(1),
                        name: "Name".into(),
                        value: TitleFieldValue::Text("Ana".into()),
                    },
                    TitleFieldStatus {
                        field: field(2),
                        name: "Score".into(),
                        value: TitleFieldValue::Number(4),
                    },
                    TitleFieldStatus {
                        field: field(3),
                        name: "Bar".into(),
                        value: TitleFieldValue::Color {
                            red: 0x11,
                            green: 0x22,
                            blue: 0x33,
                            alpha: 0xFF,
                        },
                    },
                    TitleFieldStatus {
                        field: field(4),
                        name: "Logo".into(),
                        value: TitleFieldValue::Image {
                            asset_uri: "asset://logo.png".into(),
                        },
                    },
                ],
            })
            .unwrap();
        model
    }

    fn ready_state(model: &ClientModel, edit: bool, air: bool) -> StudioUiState {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(model.view().unwrap())
            .with_switcher_permissions(true, air)
            .with_edit_project_permission(edit);
        state.project_state = model.state().cloned();
        state.title_access = TitleAccess::new(true, edit, air);
        state
    }

    fn synced(state: &StudioUiState) -> TitlesPanel {
        let mut panel = TitlesPanel::default();
        panel.sync(state);
        panel
    }

    fn blocked(panel: &TitlesPanel, operation: TitleOperation) -> Result<(), TitleBlocked> {
        panel
            .editor()
            .presentation(input(TITLE))
            .unwrap()
            .operations
            .iter()
            .find(|candidate| candidate.operation == operation)
            .unwrap()
            .state
    }

    #[test]
    fn access_needs_synchronized_control_and_keeps_permissions_while_reconnecting() {
        let model = model(overlays(), overlays());
        let ready = ready_state(&model, true, false);
        let access = title_access(&ready, TitleAccess::default());
        assert!(access.is_ready() && access.can_edit() && !access.can_air());

        // Ready without synchronized state cannot send.
        let mut unsynced = ready.clone();
        unsynced.project_state = None;
        assert!(!title_access(&unsynced, TitleAccess::default()).is_ready());

        // Reconnecting drops readiness but keeps the granted permissions,
        // so drafts stay editable.
        let mut lost = StudioUiState::new(StudioConnectionStatus::Backoff);
        lost.title_access = TitleAccess::new(false, false, false);
        let kept = title_access(&lost, access);
        assert!(!kept.is_ready() && kept.can_edit() && !kept.can_air());
    }

    #[test]
    fn panel_gates_each_operation_by_session_permission_and_draft_state() {
        let model = model(overlays(), overlays());
        // An operator without edit_project may air but not edit.
        let state = ready_state(&model, false, true);
        let mut panel = synced(&state);
        assert_eq!(
            panel
                .editor()
                .list()
                .first()
                .map(|entry| entry.name.as_str()),
            Some("Lower third")
        );
        assert_eq!(
            blocked(&panel, TitleOperation::Save),
            Err(TitleBlocked::NotPermitted)
        );
        assert_eq!(
            blocked_reason(TitleBlocked::NotPermitted, TitleOperation::Save, &state),
            Msg::ReasonNoEditProject.en()
        );
        assert_eq!(
            blocked(&panel, TitleOperation::Take),
            Err(TitleBlocked::NotPreviewed)
        );
        assert_eq!(
            blocked(&panel, TitleOperation::OffAir),
            Err(TitleBlocked::OffAir)
        );
        assert!(
            panel
                .editor_mut()
                .set_input(input(TITLE), field(1), "Bea")
                .is_err()
        );

        // A graphics session edits, saves, previews, and takes.
        let state = ready_state(&model, true, true);
        let mut panel = synced(&state);
        let editor = panel.editor_mut();
        editor.set_input(input(TITLE), field(1), "Bea").unwrap();
        editor.set_input(input(TITLE), field(2), "x").unwrap();
        assert_eq!(
            blocked(&panel, TitleOperation::Save),
            Err(TitleBlocked::InvalidDraft)
        );
        panel
            .editor_mut()
            .set_input(input(TITLE), field(2), "12")
            .unwrap();
        assert_eq!(blocked(&panel, TitleOperation::Save), Ok(()));
        panel.editor_mut().preview(input(TITLE)).unwrap();
        assert_eq!(blocked(&panel, TitleOperation::Take), Ok(()));
        assert_eq!(
            blocked(&panel, TitleOperation::Update),
            Err(TitleBlocked::OffAir)
        );

        // A take by an air-only session with drafts names edit_project.
        let air_only = ready_state(&model, false, true);
        assert_eq!(
            blocked_reason(TitleBlocked::NotPermitted, TitleOperation::Take, &air_only),
            Msg::ReasonNoEditProject.en()
        );
        let edit_only = ready_state(&model, true, false);
        assert_eq!(
            blocked_reason(TitleBlocked::NotPermitted, TitleOperation::Take, &edit_only),
            Msg::ReasonNoTransition.en()
        );
        // Not ready names the connection gate.
        let connecting = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert_eq!(
            blocked_reason(TitleBlocked::NotReady, TitleOperation::Save, &connecting),
            Msg::ReasonNotReady.en()
        );
    }

    #[test]
    fn replies_and_abandons_resolve_the_pending_request() {
        let model = model(overlays(), overlays());
        let state = ready_state(&model, true, true);
        let mut panel = synced(&state);
        panel
            .editor_mut()
            .set_input(input(TITLE), field(1), "Bea")
            .unwrap();
        let command = panel
            .editor_mut()
            .request(input(TITLE), TitleOperation::Save)
            .unwrap();
        assert!(matches!(
            command.body,
            TitleCommandBody::Save {
                base_revision: 2,
                ..
            }
        ));
        assert_eq!(
            blocked(&panel, TitleOperation::Save),
            Err(TitleBlocked::Pending)
        );
        assert_eq!(
            TitleUpdate::Abandoned {
                request: command.request
            }
            .request(),
            command.request
        );
        panel.receive([TitleUpdate::Abandoned {
            request: command.request,
        }]);
        assert_eq!(blocked(&panel, TitleOperation::Save), Ok(()));
        // The draft survives the abandon.
        assert!(panel.editor().presentation(input(TITLE)).unwrap().dirty);

        let command = panel
            .editor_mut()
            .request(input(TITLE), TitleOperation::Save)
            .unwrap();
        let mut snapshot = TitleSnapshot::from_state(model.state().unwrap(), input(TITLE)).unwrap();
        snapshot.revision = 3;
        snapshot.fields[0].value = TitleFieldValue::Text("Bea".into());
        // An unrelated stale reply changes nothing; the real one applies.
        panel.receive([
            TitleUpdate::Abandoned { request: 999 },
            TitleUpdate::Reply(TitleReply::Applied {
                request: command.request,
                snapshot,
            }),
        ]);
        let presentation = panel.editor().presentation(input(TITLE)).unwrap();
        assert!(!presentation.pending && !presentation.dirty);
        assert_eq!(presentation.live_revision, 3);
    }

    #[test]
    fn air_badges_follow_desired_then_confirmed_routing() {
        let title = input(TITLE);
        let view = |desired, realized| model(desired, realized).view().unwrap();

        let on_air = view(keyed(TITLE, true, 255), keyed(TITLE, true, 255));
        assert_eq!(
            title_air_badge(Some(&on_air), title, true),
            TitleAirBadge::OnAir
        );
        assert_eq!(
            title_location(Some(&on_air), title),
            Some(TitleLocation::Overlay(2))
        );
        // Taken but not yet confirmed still reads on air: the take is desired.
        let taking = view(keyed(TITLE, true, 255), overlays());
        assert_eq!(
            title_air_badge(Some(&taking), title, true),
            TitleAirBadge::OnAir
        );
        // Off air requested while the channel still fades or still shows it.
        let fading = view(keyed(TITLE, false, 120), keyed(TITLE, true, 255));
        assert_eq!(
            title_air_badge(Some(&fading), title, true),
            TitleAirBadge::Clearing
        );
        let confirmed_only = view(overlays(), keyed(TITLE, true, 255));
        assert_eq!(
            title_air_badge(Some(&confirmed_only), title, false),
            TitleAirBadge::Clearing
        );
        let off = view(overlays(), overlays());
        assert_eq!(
            title_air_badge(Some(&off), title, false),
            TitleAirBadge::Off
        );
        assert_eq!(title_location(Some(&off), title), None);
        // Another source on the take channel is named so Take's replace is
        // no surprise.
        let busy = view(keyed(1, true, 255), keyed(1, true, 255));
        assert_eq!(view_channel_busy(Some(&busy), 2, title), Some(input(1)));
        assert_eq!(view_channel_busy(Some(&busy), 3, title), None);
        assert_eq!(view_channel_busy(Some(&on_air), 2, title), None);

        assert_eq!(TitleAirBadge::OnAir.tone(), Tone::OnAir);
        assert_eq!(TitleAirBadge::Clearing.tone(), Tone::Pending);
        assert_eq!(TitleAirBadge::Off.tone(), Tone::Idle);
        let words = |l: Locale| {
            [
                TitleAirBadge::OnAir,
                TitleAirBadge::Clearing,
                TitleAirBadge::Off,
            ]
            .map(|badge| l.text(badge.label()))
        };
        assert_eq!(words(Locale::ENGLISH), ["ON AIR", "CLEARING", "OFF AIR"]);
        assert_eq!(words(ES), ["AL AIRE", "SALIENDO", "FUERA DEL AIRE"]);
        assert_eq!(location_text(ES, TitleLocation::Overlay(2)), "en O2");
        assert_eq!(
            location_text(Locale::ENGLISH, TitleLocation::Program),
            "on Program"
        );
    }

    #[test]
    fn a_realized_manual_transition_still_showing_the_title_reads_clearing() {
        let title = input(TITLE);
        let mut view = model(overlays(), overlays()).view().unwrap();
        // The desired T-bar toward the title was cancelled, but the realized
        // mix still shows it.
        view.switcher.realized_manual_transition =
            fm_ui_model::ManualTransitionStatus::Active(fm_ui_model::ActiveManualTransition {
                kind: fm_protocol::ManualTransitionKind::Fade,
                from: input(1),
                to: title,
                interval_start: fm_protocol::ManualTransitionPosition::START,
                position: fm_protocol::ManualTransitionPosition::new(5_000).unwrap(),
            });
        assert_eq!(
            title_air_badge(Some(&view), title, false),
            TitleAirBadge::Clearing
        );
        assert_eq!(
            title_air_badge(Some(&view), input(2), false),
            TitleAirBadge::Off
        );
    }

    #[test]
    fn sync_follows_an_authority_restarted_at_a_lower_field_revision() {
        let model = model(overlays(), overlays());
        let state = ready_state(&model, true, true);
        let mut panel = synced(&state);
        // Before the restart this console saw the title at revision 5.
        let mut newer = TitleSnapshot::from_state(model.state().unwrap(), input(TITLE)).unwrap();
        newer.revision = 5;
        newer.fields[0].value = TitleFieldValue::Text("Lost".into());
        panel.editor_mut().observe(newer).unwrap();
        // The restarted daemon reports the saved revision 2 again.
        panel.sync(&state);
        let presentation = panel.editor().presentation(input(TITLE)).unwrap();
        assert_eq!(presentation.live_revision, 2);
        assert_eq!(presentation.base_revision, 2);
        assert_eq!(presentation.fields[0].input, "Ana");
        panel
            .editor_mut()
            .set_input(input(TITLE), field(1), "Bea")
            .unwrap();
        let command = panel
            .editor_mut()
            .request(input(TITLE), TitleOperation::Save)
            .unwrap();
        assert!(matches!(
            command.body,
            TitleCommandBody::Save {
                base_revision: 2,
                ..
            }
        ));
        // A stopped worker answers nothing: its pending titles unblock.
        let mut shell = StudioShell {
            titles: panel,
            ..StudioShell::default()
        };
        shell.abandon_title_requests();
        assert_eq!(blocked(&shell.titles, TitleOperation::Save), Ok(()));
    }

    #[test]
    fn content_over_the_title_bound_names_the_reason() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        let reason = blocked_reason(TitleBlocked::ContentTooLarge, TitleOperation::Save, &state);
        assert_eq!(reason, Msg::TitleRejTooLarge.en());
        assert_ne!(ES.tr(reason), reason);
    }

    #[test]
    fn labels_reasons_rejections_and_errors_are_localized() {
        let types = |l: Locale| {
            [
                FieldType::Text,
                FieldType::Number,
                FieldType::Color,
                FieldType::Image,
            ]
            .map(|field_type| l.text(field_type_label(field_type)))
        };
        assert_eq!(
            types(Locale::ENGLISH),
            ["TEXT", "NUMBER", "COLOUR", "IMAGE"]
        );
        assert_eq!(types(ES), ["TEXTO", "NÚMERO", "COLOR", "IMAGEN"]);
        let operations = |l: Locale| TitleOperation::ALL.map(|op| l.text(operation_label(op).0));
        assert_eq!(
            operations(Locale::ENGLISH),
            ["Save", "Take", "Update live", "Off air"]
        );
        assert_eq!(
            operations(ES),
            ["Guardar", "Tomar", "Actualizar en vivo", "Retirar del aire"]
        );
        assert_eq!(
            ES.format(
                operation_label(TitleOperation::Take).1,
                &[("channel", Arg::Plain(3))]
            ),
            "Llevar al aire el título previsualizado en O3"
        );

        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        let reason = blocked_reason(TitleBlocked::Stale, TitleOperation::Take, &state);
        assert_eq!(
            reason,
            "the title changed on the server; rebase or discard your edits first"
        );
        assert_eq!(
            l10n::disabled_text(ES, ES.text(Msg::TitleOpTake), reason),
            "Tomar\nNo disponible: el título cambió en el servidor; rebase o descarte sus ediciones primero"
        );
        for blocked in [
            TitleBlocked::Pending,
            TitleBlocked::Stale,
            TitleBlocked::InvalidDraft,
            TitleBlocked::NotPreviewed,
            TitleBlocked::NoChanges,
            TitleBlocked::OnAir,
            TitleBlocked::OffAir,
        ] {
            let english = blocked_reason(blocked, TitleOperation::Save, &state);
            // Every reason is catalog text the console words in Spanish.
            assert_ne!(ES.tr(english), english, "{blocked:?}");
        }

        assert_eq!(
            rejected_text(Locale::ENGLISH, TitleRejection::OnProgram),
            "Refused: the title is on Program; switch Program away instead"
        );
        assert_eq!(
            rejected_text(ES, TitleRejection::Locked),
            "Rechazado: el proyecto o el título está bloqueado para edición"
        );
        assert_eq!(
            field_error_text(
                Locale::ENGLISH,
                FieldError::TextTooLong { max_chars: 1_024 }
            ),
            "Text is longer than 1,024 characters."
        );
        assert_eq!(
            field_error_text(ES, FieldError::InvalidColor),
            "Introduzca un color como #RRGGBB o #RRGGBBAA."
        );
        assert_eq!(
            field_error_text(
                ES,
                FieldError::TypeMismatch {
                    expected: FieldType::Number,
                    actual: FieldType::Text,
                }
            ),
            "Este campo admite número, no texto."
        );
    }

    #[test]
    fn empty_states_say_why_no_title_is_listed() {
        let disconnected = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(
            titles_empty_text(&disconnected),
            (Msg::TitlesNone, Msg::TitlesNotSynced)
        );
        let model = model(overlays(), overlays());
        let state = ready_state(&model, true, true);
        assert_eq!(
            titles_empty_text(&state),
            (Msg::TitlesNone, Msg::TitlesWaiting)
        );
    }

    fn painted_text(output: &egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    fn draw(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
    ) -> Vec<String> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-titles-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        assert!(intents.is_empty(), "{intents:?}");
        painted_text(&context.end_pass())
    }

    #[test]
    fn selecting_a_title_focuses_its_first_field_when_editable() {
        let model = model(keyed(TITLE, true, 255), keyed(TITLE, true, 255));
        let context = egui::Context::default();
        let mut shell = StudioShell {
            side_tab: SideTab::Titles,
            ..StudioShell::default()
        };
        let viewer = ready_state(&model, false, true);
        draw(&context, &mut shell, &viewer);
        // A session that cannot edit keeps focus where it was.
        shell.titles.focus_field = true;
        draw(&context, &mut shell, &viewer);
        assert_eq!(context.memory(egui::Memory::focused), None);
        assert!(!shell.titles.focus_field);

        let editor = ready_state(&model, true, true);
        shell.titles.focus_field = true;
        draw(&context, &mut shell, &editor);
        draw(&context, &mut shell, &editor);
        assert_eq!(
            context.memory(egui::Memory::focused),
            Some(field_edit_id(input(TITLE), field(1)))
        );
    }

    #[test]
    fn the_titles_tab_draws_badges_conflicts_and_keys_in_both_languages() {
        let model = model(keyed(TITLE, true, 255), keyed(TITLE, true, 255));
        let state = ready_state(&model, true, true);
        let context = egui::Context::default();
        let mut shell = StudioShell {
            side_tab: SideTab::Titles,
            ..StudioShell::default()
        };
        let english = draw(&context, &mut shell, &state);
        for expected in [
            "TITLES",
            "Lower third",
            "ON AIR",
            "on O2",
            "Name",
            "COLOUR",
            "UPDATE LIVE",
            "OFF AIR",
            "REV 2",
        ] {
            assert!(
                english.iter().any(|text| text == expected),
                "missing {expected:?} in {english:?}"
            );
        }
        // A conflicting draft shows mine against theirs with Rebase.
        let editor = shell.titles.editor_mut();
        editor.set_input(input(TITLE), field(1), "Bea").unwrap();
        let mut theirs = TitleSnapshot::from_state(model.state().unwrap(), input(TITLE)).unwrap();
        theirs.revision = 3;
        theirs.fields[0].value = TitleFieldValue::Text("Cy".into());
        editor.observe(theirs).unwrap();
        shell.set_locale(ES);
        let mut unsynced = state.clone();
        unsynced.project_state = None;
        let spanish = draw(&context, &mut shell, &unsynced);
        for expected in [
            "TÍTULOS",
            "AL AIRE",
            "CONFLICTO",
            "MÍO",
            "SUYO",
            "Bea",
            "Cy",
            "REBASAR",
            "DESCARTAR TODO",
            "ACTUALIZAR EN VIVO",
            "TOMAR",
        ] {
            assert!(
                spanish.iter().any(|text| text == expected),
                "missing {expected:?} in {spanish:?}"
            );
        }
    }
}
