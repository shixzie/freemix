//! Screen and window capture inputs in the console (IN-011): the Add input
//! dialog's capture form, and the capture badge and details on an input's
//! tile.
//!
//! The form lists what the server can capture (asked for when the form
//! opens and on Refresh targets), grouped as Screens and Application
//! windows, each window named by its application and title. Picking a
//! window follows it by its application and exact title (and its id while
//! it lives); the operator may follow it by a title that contains a text,
//! or by any window of the application, instead. Crop insets, the mouse
//! pointer, a frame rate cap, what Program shows while the target is gone,
//! and the Audio captured with the picture complete the form: None, System
//! audio (everything the server plays), or Application audio (only the
//! sound of the application owning the window, offered for a window
//! target). Captured sound feeds the input's own mixer strip. The tile
//! badge reads the input's latest `screen_capture_status` (OPENING, LIVE,
//! TARGET GONE, REOPENING n, UNSUPPORTED, FAILED); the tile's tooltip
//! carries the target, the captured size, the reopen count, the last error,
//! and the audio state (starting, live, no audio stream yet, unsupported
//! here, failed) with its reason.

use egui::{Color32, RichText, Ui};
use fm_protocol::{
    MAX_SCREEN_CAPTURE_CROP, MAX_SCREEN_CAPTURE_FPS, MAX_SCREEN_CAPTURE_TITLE_BYTES,
    MIN_SCREEN_CAPTURE_FPS, ScreenCaptureAudio, ScreenCaptureAudioState, ScreenCaptureConfig,
    ScreenCaptureEntry, ScreenCaptureError, ScreenCaptureInventory, ScreenCaptureState,
    ScreenCaptureStatus, ScreenCaptureTarget, ScreenInfo, TitleMatch, WindowInfo, WindowRule,
};
use fm_types::network_stream::StallSlate;

use crate::StudioUiState;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::network_stream_ui::{
    FieldProblem, field_error, hint_slot, mark_invalid, section_heading_with,
};
use crate::theme::{AMBER, ERROR, muted_color, section_label};

/// The capture form of the Add input dialog: the configuration it adds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScreenCaptureDraft {
    pub(crate) config: ScreenCaptureConfig,
}

/// The form field a problem belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScreenCaptureField {
    Target,
    Title,
    Crop,
    Fps,
    Audio,
}

impl ScreenCaptureField {
    pub(crate) const fn label(self) -> Msg {
        match self {
            Self::Target => Msg::ScTarget,
            Self::Title => Msg::ScTitle,
            Self::Crop => Msg::ScCrop,
            Self::Fps => Msg::ScFps,
            Self::Audio => Msg::ScAudio,
        }
    }
}

impl ScreenCaptureDraft {
    /// Captures `screen`. A screen has no application, so application
    /// audio falls back to system audio.
    pub(crate) fn choose_screen(&mut self, screen: &ScreenInfo) {
        self.config.target = ScreenCaptureTarget::Screen {
            index: screen.index,
        };
        if self.config.audio == ScreenCaptureAudio::Application {
            self.config.audio = ScreenCaptureAudio::System;
        }
    }

    /// Follows `window` by its application and title (keeping how the
    /// title is matched) and by its id while it lives.
    pub(crate) fn choose_window(&mut self, window: &WindowInfo) {
        let title_match = match &self.config.target {
            ScreenCaptureTarget::Window(rule) => rule.title_match,
            ScreenCaptureTarget::Screen { .. } => TitleMatch::Exact,
        };
        self.config.target = ScreenCaptureTarget::Window(WindowRule {
            app: window.app.clone(),
            title: window.title.clone(),
            title_match: if window.title.is_empty() && title_match != TitleMatch::Any {
                TitleMatch::Any
            } else {
                title_match
            },
            window_id: Some(window.id),
        });
    }

    /// The window rule, when a window is captured.
    pub(crate) fn rule_mut(&mut self) -> Option<&mut WindowRule> {
        match &mut self.config.target {
            ScreenCaptureTarget::Window(rule) => Some(rule),
            ScreenCaptureTarget::Screen { .. } => None,
        }
    }

    /// Whether `screen` is the one captured.
    fn is_screen(&self, screen: &ScreenInfo) -> bool {
        self.config.target
            == ScreenCaptureTarget::Screen {
                index: screen.index,
            }
    }

    /// Whether `window` is the one followed: its id while the rule holds
    /// it, else the rule matches it.
    fn is_window(&self, window: &WindowInfo, inventory: &ScreenCaptureInventory) -> bool {
        match &self.config.target {
            ScreenCaptureTarget::Window(rule) => inventory
                .resolve_window(rule)
                .is_some_and(|followed| followed.id == window.id),
            ScreenCaptureTarget::Screen { .. } => false,
        }
    }

    /// The configuration the daemon would accept, or why not (an English
    /// catalog reason).
    pub(crate) fn config(&self) -> Result<ScreenCaptureConfig, &'static str> {
        self.checked().map_err(|problem| problem.reason)
    }

    /// The configuration, or the field to fix and why.
    pub(crate) fn checked(&self) -> Result<ScreenCaptureConfig, FieldProblem<ScreenCaptureField>> {
        if let ScreenCaptureTarget::Window(rule) = &self.config.target
            && rule.title_match != TitleMatch::Any
            && rule.title.is_empty()
        {
            return Err(FieldProblem::prompt(
                ScreenCaptureField::Title,
                Msg::ScReasonTitle.en(),
            ));
        }
        self.config.validate().map_err(|error| match error {
            ScreenCaptureError::InvalidScreen => {
                FieldProblem::error(ScreenCaptureField::Target, Msg::ScReasonRule.en())
            }
            ScreenCaptureError::InvalidWindowRule => {
                FieldProblem::error(ScreenCaptureField::Title, Msg::ScReasonRule.en())
            }
            ScreenCaptureError::InvalidCrop => {
                FieldProblem::error(ScreenCaptureField::Crop, Msg::ScReasonCrop.en())
            }
            ScreenCaptureError::InvalidFps => {
                FieldProblem::error(ScreenCaptureField::Fps, Msg::ScReasonFps.en())
            }
            ScreenCaptureError::InvalidAudio => {
                FieldProblem::error(ScreenCaptureField::Audio, Msg::ScReasonAudio.en())
            }
        })?;
        Ok(self.config.clone())
    }

    /// A free input name for the picked target: the window's application,
    /// numbered when taken; `None` for a screen.
    pub(crate) fn suggested_name(&self, taken: &[&str]) -> Option<String> {
        let ScreenCaptureTarget::Window(rule) = &self.config.target else {
            return None;
        };
        let base = rule.app.trim();
        if base.is_empty() {
            return None;
        }
        (1..100_u32)
            .map(|number| {
                if number == 1 {
                    base.to_owned()
                } else {
                    format!("{base} {number}")
                }
            })
            .find(|name| !taken.iter().any(|taken| taken.eq_ignore_ascii_case(name)))
    }
}

fn size(width: u32, height: u32) -> String {
    format!("{width}x{height}")
}

/// A listed screen as the picker names it.
pub(crate) fn screen_label(l: Locale, screen: &ScreenInfo) -> String {
    let mut label = l.format(
        Msg::ScScreenOption,
        &[
            ("number", Arg::from(u64::from(screen.index) + 1)),
            ("name", Arg::Str(&screen.name)),
            ("size", Arg::Str(&size(screen.width, screen.height))),
        ],
    );
    if screen.primary {
        label.push_str(" · ");
        label.push_str(l.text(Msg::ScScreenPrimary));
    }
    label
}

/// A listed window as the picker names it: its application and title.
pub(crate) fn window_label(l: Locale, window: &WindowInfo) -> String {
    if window.title.is_empty() {
        l.format(Msg::ScWindowUntitled, &[("app", Arg::Str(&window.app))])
    } else {
        l.format(
            Msg::ScWindowOption,
            &[
                ("app", Arg::Str(&window.app)),
                ("title", Arg::Str(&window.title)),
            ],
        )
    }
}

/// What a configuration captures, in words.
pub(crate) fn target_text(l: Locale, config: &ScreenCaptureConfig) -> String {
    match &config.target {
        ScreenCaptureTarget::Screen { index } => l.format(
            Msg::ScTargetScreen,
            &[("number", Arg::from(u64::from(*index) + 1))],
        ),
        ScreenCaptureTarget::Window(rule) => {
            let app = if rule.app.is_empty() {
                "?"
            } else {
                rule.app.as_str()
            };
            let args = [("app", Arg::Str(app)), ("title", Arg::Str(&rule.title))];
            match rule.title_match {
                TitleMatch::Exact => l.format(Msg::ScTargetWindow, &args),
                TitleMatch::Contains => l.format(Msg::ScTargetContains, &args),
                TitleMatch::Any => l.format(Msg::ScTargetAny, &args[..1]),
            }
        }
    }
}

/// The line over the picker: the listing state, and whether it reads as a
/// problem (amber).
pub(crate) fn list_line(l: Locale, inventory: Option<&ScreenCaptureInventory>) -> (String, bool) {
    let Some(inventory) = inventory else {
        return (l.text(Msg::ScListUnknown).to_owned(), false);
    };
    if !inventory.supported {
        let reason = inventory.error.as_deref().unwrap_or_default();
        return (
            l.format(Msg::ScListUnsupported, &[("reason", Arg::Str(reason))]),
            true,
        );
    }
    if let Some(error) = &inventory.error {
        return (
            l.format(Msg::ScListError, &[("error", Arg::Str(error))]),
            true,
        );
    }
    let screens = l.format(
        Msg::ScListScreens,
        &[("count", Arg::from(inventory.screens.len() as u64))],
    );
    let windows = l.format(
        Msg::ScListWindows,
        &[("count", Arg::from(inventory.windows.len() as u64))],
    );
    (
        l.format(
            Msg::ScListCount,
            &[
                ("screens", Arg::Str(&screens)),
                ("windows", Arg::Str(&windows)),
                ("backend", Arg::Str(&inventory.backend)),
            ],
        ),
        inventory.screens.is_empty() && inventory.windows.is_empty(),
    )
}

/// The picker: the listed screens, then the listed windows, as rows; a
/// choice sets the target.
fn draw_target_picker(
    ui: &mut Ui,
    l: Locale,
    inventory: &ScreenCaptureInventory,
    id: &str,
    draft: &mut ScreenCaptureDraft,
) -> Option<WindowInfo> {
    let mut picked_screen = None;
    let mut picked_window = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical()
            .id_salt(format!("{id}-screen-capture-targets"))
            .max_height(150.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.label(section_label(l.text(Msg::ScGroupScreens)));
                for screen in &inventory.screens {
                    let size = size(screen.width, screen.height);
                    if ui
                        .selectable_label(draft.is_screen(screen), screen_label(l, screen))
                        .on_hover_text(l.format(
                            Msg::ScScreenOptionHover,
                            &[
                                ("number", Arg::from(u64::from(screen.index) + 1)),
                                ("size", Arg::Str(&size)),
                                ("x", Arg::Str(&screen.x.to_string())),
                                ("y", Arg::Str(&screen.y.to_string())),
                            ],
                        ))
                        .clicked()
                    {
                        picked_screen = Some(screen.clone());
                    }
                }
                ui.add_space(4.0);
                ui.label(section_label(l.text(Msg::ScGroupWindows)));
                if inventory.windows.is_empty() {
                    ui.label(
                        RichText::new(l.text(Msg::ScNoWindows))
                            .size(crate::theme::legible(10.5))
                            .color(muted_color()),
                    );
                }
                for window in &inventory.windows {
                    let size = size(window.width, window.height);
                    if ui
                        .selectable_label(
                            draft.is_window(window, inventory),
                            window_label(l, window),
                        )
                        .on_hover_text(l.format(
                            Msg::ScWindowOptionHover,
                            &[
                                ("app", Arg::Str(&window.app)),
                                ("title", Arg::Str(&window.title)),
                                ("size", Arg::Str(&size)),
                            ],
                        ))
                        .clicked()
                    {
                        picked_window = Some(window.clone());
                    }
                }
            });
    });
    if let Some(screen) = picked_screen {
        draft.choose_screen(&screen);
    }
    if let Some(window) = &picked_window {
        draft.choose_window(window);
    }
    picked_window
}

/// Whether the configured target is in the listing.
fn listed(draft: &ScreenCaptureDraft, inventory: &ScreenCaptureInventory) -> bool {
    match &draft.config.target {
        ScreenCaptureTarget::Screen { index } => inventory.screen(*index).is_some(),
        ScreenCaptureTarget::Window(rule) => inventory.resolve_window(rule).is_some(),
    }
}

/// What the form asks of its caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FormOutcome {
    /// The operator asked for the target list again.
    pub(crate) rescan: bool,
    /// The operator picked this window.
    pub(crate) picked: Option<WindowInfo>,
}

/// Draws the target picker with Refresh targets, how a window is followed,
/// the crop, the pointer, the rate cap, and the stall slate; `id` keeps the
/// dialog's widgets apart.
#[allow(clippy::too_many_lines)] // one form, top to bottom
pub(crate) fn draw_screen_capture_form(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    id: &str,
    draft: &mut ScreenCaptureDraft,
) -> FormOutcome {
    let mut outcome = FormOutcome::default();
    let inventory = state
        .view
        .as_ref()
        .and_then(|view| view.screen_capture_inventory.as_ref());
    section_heading_with(ui, l, Msg::ScTarget, |ui| {
        if ui
            .add_enabled(
                state.connection_status.controls_enabled(),
                egui::Button::new(
                    RichText::new(l.text(Msg::ScRefresh)).size(crate::theme::legible(11.0)),
                ),
            )
            .hint(
                l.text(Msg::ScRefreshHover),
                unavailable_reason(state, Capability::EditProject),
            )
            .clicked()
        {
            outcome.rescan = true;
        }
    });
    let (line, problem) = list_line(l, inventory);
    ui.label(
        RichText::new(line)
            .size(crate::theme::legible(10.5))
            .color(if problem { AMBER } else { muted_color() }),
    );
    if let Some(inventory) = inventory.filter(|inventory| inventory.supported) {
        outcome.picked = draw_target_picker(ui, l, inventory, id, draft);
        if !listed(draft, inventory) {
            ui.label(
                RichText::new(l.format(
                    Msg::ScChosenMissing,
                    &[("target", Arg::Str(&target_text(l, &draft.config)))],
                ))
                .size(crate::theme::legible(10.5))
                .color(AMBER),
            );
        }
    }

    if let Some(rule) = draft.rule_mut() {
        ui.add_space(4.0);
        ui.label(
            RichText::new(l.format(Msg::ScApp, &[("app", Arg::Str(&rule.app))]))
                .size(crate::theme::legible(11.0)),
        );
        ui.label(section_label(l.text(Msg::ScFollow)));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (title_match, label, hover) in [
                (TitleMatch::Exact, Msg::ScMatchExact, Msg::ScMatchExactHover),
                (
                    TitleMatch::Contains,
                    Msg::ScMatchContains,
                    Msg::ScMatchContainsHover,
                ),
                (TitleMatch::Any, Msg::ScMatchAny, Msg::ScMatchAnyHover),
            ] {
                let selected = rule.title_match == title_match;
                if ui
                    .add(kind_key(l.text(label), selected))
                    .on_hover_text(l.text(hover))
                    .clicked()
                    && !selected
                {
                    rule.title_match = title_match;
                }
            }
        });
        if rule.title_match != TitleMatch::Any {
            ui.label(section_label(l.text(Msg::ScTitle)));
            let edit = ui
                .add(
                    egui::TextEdit::singleline(&mut rule.title)
                        .id(egui::Id::new(format!("{id}-screen-capture-title")))
                        .char_limit(MAX_SCREEN_CAPTURE_TITLE_BYTES)
                        .desired_width(ui.available_width()),
                )
                .on_hover_text(l.text(Msg::ScTitleHover));
            if edit.changed() {
                // The typed title, not the picked window, is followed now.
                rule.window_id = None;
            }
            let problem = draft
                .checked()
                .err()
                .filter(|problem| problem.field == ScreenCaptureField::Title);
            if problem.is_some_and(|problem| !problem.prompt) {
                mark_invalid(ui, edit.rect);
            }
            hint_slot(
                ui,
                problem.map(|problem| {
                    if problem.prompt {
                        crate::theme::caption(l.tr(problem.reason))
                    } else {
                        field_error(l, problem.reason)
                    }
                }),
            );
        }
    }

    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::ScCrop)));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let crop = &mut draft.config.crop;
        for (label, value) in [
            (Msg::ScCropLeft, &mut crop.left),
            (Msg::ScCropTop, &mut crop.top),
            (Msg::ScCropRight, &mut crop.right),
            (Msg::ScCropBottom, &mut crop.bottom),
        ] {
            ui.label(RichText::new(l.text(label)).size(crate::theme::legible(10.5)));
            ui.add(
                egui::DragValue::new(value)
                    .range(0..=MAX_SCREEN_CAPTURE_CROP)
                    .speed(1.0),
            )
            .on_hover_text(l.text(Msg::ScCropHover));
        }
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.checkbox(&mut draft.config.cursor, l.text(Msg::ScCursor))
            .on_hover_text(l.text(Msg::ScCursorHover));
        ui.label(RichText::new(l.text(Msg::ScFps)).size(crate::theme::legible(10.5)));
        ui.add(
            egui::DragValue::new(&mut draft.config.fps)
                .range(MIN_SCREEN_CAPTURE_FPS..=MAX_SCREEN_CAPTURE_FPS)
                .speed(0.25),
        )
        .on_hover_text(l.text(Msg::ScFpsHover));
    });
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::ScStall)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (slate, label, hover) in [
            (
                StallSlate::Freeze,
                Msg::ScStallFreeze,
                Msg::ScStallFreezeHover,
            ),
            (StallSlate::Black, Msg::ScStallBlack, Msg::ScStallBlackHover),
        ] {
            let selected = draft.config.slate == slate;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.config.slate = slate;
            }
        }
    });
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::ScAudio)));
    let window = matches!(draft.config.target, ScreenCaptureTarget::Window(_));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (audio, label, hover) in [
            (
                ScreenCaptureAudio::None,
                Msg::ScAudioNone,
                Msg::ScAudioNoneHover,
            ),
            (
                ScreenCaptureAudio::System,
                Msg::ScAudioSystem,
                Msg::ScAudioSystemHover,
            ),
            (
                ScreenCaptureAudio::Application,
                Msg::ScAudioApp,
                Msg::ScAudioAppHover,
            ),
        ] {
            let selected = draft.config.audio == audio;
            let enabled = audio != ScreenCaptureAudio::Application || window;
            if ui
                .add_enabled(enabled, kind_key(l.text(label), selected))
                .hint(l.text(hover), Msg::ScAudioAppScreenHover.en())
                .clicked()
            {
                draft.config.audio = audio;
            }
        }
    });
    let problem = draft
        .checked()
        .err()
        .filter(|problem| problem.field == ScreenCaptureField::Audio);
    hint_slot(ui, problem.map(|problem| field_error(l, problem.reason)));
    outcome
}

/// The tooltip line of a screen capture input's audio, or `None` while
/// its audio is off.
pub(crate) fn audio_line(
    l: Locale,
    audio: ScreenCaptureAudio,
    status: &ScreenCaptureStatus,
) -> Option<String> {
    let source = l.text(match audio {
        ScreenCaptureAudio::Application => Msg::ScAudioApp,
        ScreenCaptureAudio::None | ScreenCaptureAudio::System => Msg::ScAudioSystem,
    });
    let reason = status
        .audio_error
        .as_deref()
        .unwrap_or_else(|| l.text(Msg::ScAudioNoReason));
    let args = [("source", Arg::Str(source)), ("reason", Arg::Str(reason))];
    let line = match status.audio {
        ScreenCaptureAudioState::Off => return None,
        ScreenCaptureAudioState::Opening => l.format(Msg::ScAudioLineOpening, &args[..1]),
        ScreenCaptureAudioState::Live => l.format(Msg::ScAudioLineLive, &args[..1]),
        ScreenCaptureAudioState::NoStream => l.format(Msg::ScAudioLineNoStream, &args[..1]),
        ScreenCaptureAudioState::Unsupported => l.format(Msg::ScAudioLineUnsupported, &args),
        ScreenCaptureAudioState::Failed => l.format(Msg::ScAudioLineFailed, &args),
    };
    Some(line)
}

/// The tile badge for a screen capture status: its text and colour, or
/// `None` while nothing captures (idle).
pub(crate) fn screen_capture_badge(
    l: Locale,
    status: &ScreenCaptureStatus,
) -> Option<(String, Color32)> {
    Some(match status.state {
        ScreenCaptureState::Idle => return None,
        ScreenCaptureState::Opening => (l.text(Msg::SaBadgeOpening).to_owned(), muted_color()),
        ScreenCaptureState::Live => (l.text(Msg::Live).to_owned(), Color32::from_rgb(30, 150, 70)),
        ScreenCaptureState::TargetGone => (l.text(Msg::ScBadgeGone).to_owned(), AMBER),
        ScreenCaptureState::Reopening { attempt, .. } => (
            l.format(
                Msg::SaBadgeReopening,
                &[("attempt", Arg::from(u64::from(attempt)))],
            ),
            AMBER,
        ),
        ScreenCaptureState::Unsupported => (l.text(Msg::ScBadgeUnsupported).to_owned(), ERROR),
        ScreenCaptureState::Failed => (l.text(Msg::ConnFailed).to_owned(), ERROR),
    })
}

/// Paints the screen capture badge in the picture's bottom trailing corner.
pub(crate) fn paint_screen_capture_badge(
    painter: &egui::Painter,
    thumb: egui::Rect,
    l: Locale,
    status: &ScreenCaptureStatus,
) {
    if let Some((text, color)) = screen_capture_badge(l, status) {
        crate::network_stream_ui::paint_corner_badge(painter, thumb, text, color);
    }
}

/// The tile tooltip line for a screen capture input: the target, the
/// state, the captured size, the reopens, and the last error.
pub(crate) fn screen_capture_details(l: Locale, entry: &ScreenCaptureEntry) -> String {
    let status = &entry.status;
    let state = screen_capture_badge(l, status)
        .map_or_else(|| l.text(Msg::NsBadgeIdle).to_owned(), |(text, _)| text);
    let mut lines = vec![l.format(
        Msg::ScDetail,
        &[
            ("target", Arg::Str(&target_text(l, &entry.config))),
            ("state", Arg::Str(&state)),
        ],
    )];
    if status.width > 0 && status.height > 0 {
        lines.push(l.format(
            Msg::ScDetailSize,
            &[("size", Arg::Str(&size(status.width, status.height)))],
        ));
    }
    if status.reopens > 0 {
        lines.push(l.format(
            Msg::SaDetailReopens,
            &[("count", Arg::from(u64::from(status.reopens)))],
        ));
    }
    if let ScreenCaptureState::Reopening { next_in_ms, .. } = status.state {
        lines.push(l.format(
            Msg::NsDetailNextAttempt,
            &[("seconds", Arg::from(u64::from(next_in_ms.div_ceil(1_000))))],
        ));
    }
    if let Some(error) = &status.last_error {
        lines.push(l.format(Msg::CardLastError, &[("error", Arg::Str(error))]));
    }
    lines.extend(audio_line(l, entry.config.audio, status));
    lines.join(" · ")
}

#[cfg(test)]
mod tests {
    use fm_ui_model::i18n::Language;

    use super::*;

    fn window(id: u64, app: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            app: app.into(),
            title: title.into(),
            pid: None,
            x: 0,
            y: 0,
            width: 1_280,
            height: 720,
        }
    }

    fn inventory() -> ScreenCaptureInventory {
        ScreenCaptureInventory {
            supported: true,
            backend: "x11".into(),
            screens: vec![ScreenInfo {
                index: 0,
                name: "DP-1".into(),
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
                primary: true,
            }],
            windows: vec![
                window(7, "Keynote", "Quarterly deck"),
                window(9, "Firefox", ""),
            ],
            error: None,
        }
    }

    #[test]
    fn the_form_follows_a_picked_window_and_validates_like_the_daemon() {
        let inventory = inventory();
        let mut draft = ScreenCaptureDraft::default();
        assert_eq!(
            draft.config(),
            Ok(ScreenCaptureConfig::new(ScreenCaptureTarget::Screen {
                index: 0
            }))
        );
        assert!(draft.is_screen(&inventory.screens[0]));
        assert_eq!(draft.suggested_name(&[]), None);
        draft.choose_window(&inventory.windows[0]);
        assert!(draft.is_window(&inventory.windows[0], &inventory));
        assert!(!draft.is_window(&inventory.windows[1], &inventory));
        assert_eq!(
            draft.config().unwrap().target,
            ScreenCaptureTarget::Window(WindowRule::exact("Keynote", "Quarterly deck", Some(7)))
        );
        assert_eq!(
            draft.suggested_name(&["keynote"]).as_deref(),
            Some("Keynote 2")
        );
        // Following by a title that contains a text, then an empty title.
        let rule = draft.rule_mut().unwrap();
        rule.title_match = TitleMatch::Contains;
        rule.title.clear();
        let problem = draft.checked().unwrap_err();
        assert!(problem.prompt);
        assert_eq!(problem.field, ScreenCaptureField::Title);
        draft.rule_mut().unwrap().title = " padded".into();
        let problem = draft.checked().unwrap_err();
        assert!(!problem.prompt);
        assert_eq!(problem.reason, Msg::ScReasonRule.en());
        // An untitled window is followed by any window of its application.
        draft.choose_window(&inventory.windows[1]);
        assert_eq!(
            draft.config().unwrap().target,
            ScreenCaptureTarget::Window(WindowRule {
                app: "Firefox".into(),
                title: String::new(),
                title_match: TitleMatch::Any,
                window_id: Some(9),
            })
        );
        draft.config.crop.left = MAX_SCREEN_CAPTURE_CROP + 1;
        assert_eq!(draft.checked().unwrap_err().field, ScreenCaptureField::Crop);
        draft.config.crop.left = 0;
        draft.config.fps = 0;
        assert_eq!(draft.checked().unwrap_err().field, ScreenCaptureField::Fps);
        draft.config.fps = 30;
        // Application audio for a window; a screen falls back to system.
        draft.config.audio = ScreenCaptureAudio::Application;
        assert_eq!(
            draft.config().unwrap().audio,
            ScreenCaptureAudio::Application
        );
        draft.choose_screen(&inventory.screens[0]);
        assert!(draft.rule_mut().is_none());
        assert_eq!(draft.config.audio, ScreenCaptureAudio::System);
        draft.config.audio = ScreenCaptureAudio::Application;
        let problem = draft.checked().unwrap_err();
        assert_eq!(problem.field, ScreenCaptureField::Audio);
        assert_eq!(problem.reason, Msg::ScReasonAudio.en());
    }

    #[test]
    fn labels_and_the_list_line_read_the_server_listing() {
        let l = Locale::new(Language::English);
        let inventory = inventory();
        assert_eq!(
            screen_label(l, &inventory.screens[0]),
            "Screen 1 · DP-1 · 1920x1080 · main screen"
        );
        assert_eq!(
            window_label(l, &inventory.windows[0]),
            "Keynote · Quarterly deck"
        );
        assert_eq!(window_label(l, &inventory.windows[1]), "Firefox (no title)");
        assert_eq!(
            list_line(l, Some(&inventory)),
            (
                "The server lists 1 screen and 2 windows (x11)".to_owned(),
                false
            )
        );
        assert_eq!(
            list_line(l, None),
            (l.text(Msg::ScListUnknown).to_owned(), false)
        );
        let (line, problem) = list_line(
            l,
            Some(&ScreenCaptureInventory::unsupported("Wayland session")),
        );
        assert!(problem);
        assert!(line.ends_with("Wayland session"), "{line}");
        let es = Locale::new(Language::Spanish);
        assert_ne!(
            screen_label(es, &inventory.screens[0]),
            screen_label(l, &inventory.screens[0])
        );
    }

    #[test]
    fn badges_and_details_follow_the_status() {
        let l = Locale::new(Language::English);
        assert_eq!(
            screen_capture_badge(l, &ScreenCaptureStatus::default()),
            None
        );
        let entry = ScreenCaptureEntry {
            input: fm_protocol::WireInputId::from_domain(fm_types::InputId::new(
                core::num::NonZeroU128::new(5).unwrap(),
            )),
            config: ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
                "Keynote", "Deck", None,
            ))),
            status: ScreenCaptureStatus {
                state: ScreenCaptureState::TargetGone,
                reopens: 2,
                width: 0,
                height: 0,
                window_id: None,
                last_error: Some("no Keynote window titled \"Deck\" is open".into()),
                ..ScreenCaptureStatus::default()
            },
        };
        assert_eq!(
            screen_capture_badge(l, &entry.status).map(|(text, _)| text),
            Some("TARGET GONE".to_owned())
        );
        let details = screen_capture_details(l, &entry);
        assert!(
            details.starts_with("Screen capture of the Keynote window \"Deck\": TARGET GONE"),
            "{details}"
        );
        assert!(details.contains("reopened 2 times"), "{details}");
        let mut live = entry;
        live.status = ScreenCaptureStatus {
            state: ScreenCaptureState::Live,
            width: 1_280,
            height: 720,
            ..ScreenCaptureStatus::default()
        };
        assert!(screen_capture_details(l, &live).contains("capturing 1280x720"));
        assert!(!screen_capture_details(l, &live).contains("audio"));
    }

    #[test]
    fn audio_states_read_clearly_in_both_languages() {
        let l = Locale::new(Language::English);
        let es = Locale::new(Language::Spanish);
        let status = |audio, error: Option<&str>| ScreenCaptureStatus {
            state: ScreenCaptureState::Live,
            audio,
            audio_error: error.map(str::to_owned),
            ..ScreenCaptureStatus::default()
        };
        let app = ScreenCaptureAudio::Application;
        assert_eq!(
            audio_line(l, app, &status(ScreenCaptureAudioState::Off, None)),
            None
        );
        assert_eq!(
            audio_line(l, app, &status(ScreenCaptureAudioState::Live, None)).as_deref(),
            Some("Application audio: live")
        );
        assert_eq!(
            audio_line(
                l,
                ScreenCaptureAudio::System,
                &status(ScreenCaptureAudioState::Opening, None)
            )
            .as_deref(),
            Some("System audio: starting")
        );
        let waiting = audio_line(l, app, &status(ScreenCaptureAudioState::NoStream, None)).unwrap();
        assert!(waiting.contains("no audio stream yet"), "{waiting}");
        let unsupported = audio_line(
            l,
            app,
            &status(
                ScreenCaptureAudioState::Unsupported,
                Some("per-application audio needs Windows 10 2004 or later"),
            ),
        )
        .unwrap();
        assert_eq!(
            unsupported,
            "Application audio unsupported here: per-application audio needs Windows 10 2004 or later"
        );
        let failed = audio_line(l, app, &status(ScreenCaptureAudioState::Failed, None)).unwrap();
        assert_eq!(failed, "Application audio failed: no reason given");
        let spanish =
            audio_line(es, app, &status(ScreenCaptureAudioState::NoStream, None)).unwrap();
        assert_ne!(spanish, waiting);
        let entry = ScreenCaptureEntry {
            input: fm_protocol::WireInputId::from_domain(fm_types::InputId::new(
                core::num::NonZeroU128::new(5).unwrap(),
            )),
            config: ScreenCaptureConfig {
                audio: app,
                ..ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
                    "Keynote", "Deck", None,
                )))
            },
            status: status(ScreenCaptureAudioState::NoStream, None),
        };
        assert!(screen_capture_details(l, &entry).ends_with(&waiting));
    }
}
