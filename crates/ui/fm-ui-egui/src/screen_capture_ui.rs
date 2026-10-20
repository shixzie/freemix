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
//! target). Captured sound feeds the input's own mixer strip. On a Wayland
//! server (backend `portal`) nothing is listed: the form offers A screen or
//! A window, and the server's own sharing dialog picks which when the
//! capture starts. The tile badge reads the input's latest
//! `screen_capture_status` (OPENING, PICK ON SERVER, LIVE, TARGET GONE,
//! REOPENING n, DENIED, UNSUPPORTED, FAILED); the tile's tooltip carries
//! the target, the captured size, the reopen count, the last error, and the
//! audio state (starting, live, no audio stream yet, unsupported here,
//! failed) with its reason, and the tile menu's Pick target again asks the
//! server's desktop anew. The audio state also has a badge of its own on
//! the tile and on the input's mixer strip (AUDIO OPENING, AUDIO LIVE, NO
//! STREAM, AUDIO UNSUPPORTED, AUDIO FAILED; none while the audio is off),
//! whose hover gives the state, its reason, and what it means.

use egui::{Color32, RichText, Ui, Vec2};
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

    /// The window rule, when a window is captured by its rule (not picked
    /// on the server's desktop).
    pub(crate) fn rule_mut(&mut self) -> Option<&mut WindowRule> {
        match &mut self.config.target {
            ScreenCaptureTarget::Window(rule) if !rule.is_picked() => Some(rule),
            ScreenCaptureTarget::Window(_) | ScreenCaptureTarget::Screen { .. } => None,
        }
    }

    /// Asks the server's desktop for a screen (`window` false) or a window
    /// when the capture starts.
    pub(crate) fn pick_on_server(&mut self, window: bool) {
        self.config.target = if window {
            ScreenCaptureTarget::Window(WindowRule::picked())
        } else {
            ScreenCaptureTarget::Screen { index: 0 }
        };
    }

    /// Whether the server's desktop picks a window.
    fn picks_window(&self) -> bool {
        matches!(&self.config.target, ScreenCaptureTarget::Window(_))
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
        ScreenCaptureTarget::Window(rule) if rule.is_picked() => {
            l.text(Msg::ScTargetPickedWindow).to_owned()
        }
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

/// The choice on a Wayland server, whose own sharing dialog picks the
/// target: A screen or A window, and what the desktop cannot share.
fn draw_portal_choice(
    ui: &mut Ui,
    l: Locale,
    inventory: &ScreenCaptureInventory,
    draft: &mut ScreenCaptureDraft,
) {
    ui.label(
        RichText::new(l.text(Msg::ScPortalNote))
            .size(crate::theme::legible(10.5))
            .color(muted_color()),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (window, label, hover) in [
            (false, Msg::ScPortalScreen, Msg::ScPortalScreenHover),
            (true, Msg::ScPortalWindow, Msg::ScPortalWindowHover),
        ] {
            let selected = draft.picks_window() == window;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
                && !selected
            {
                draft.pick_on_server(window);
            }
        }
    });
    // A window the operator picked from an X11 listing earlier is asked
    // for as any window.
    if let ScreenCaptureTarget::Window(rule) = &draft.config.target
        && !rule.is_picked()
    {
        draft.pick_on_server(true);
    }
    // The sharing dialog gives no application audio, so it falls back to
    // system audio, as for a screen.
    if draft.config.audio == ScreenCaptureAudio::Application {
        draft.config.audio = ScreenCaptureAudio::System;
    }
    if let Some(reason) = &inventory.error {
        ui.label(
            RichText::new(l.format(Msg::ScPortalLimit, &[("reason", Arg::Str(reason))]))
                .size(crate::theme::legible(10.5))
                .color(AMBER),
        );
    }
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
    if let Some(inventory) = inventory.filter(|inventory| inventory.picks_on_desktop()) {
        draw_portal_choice(ui, l, inventory, draft);
    } else {
        let (line, problem) = list_line(l, inventory);
        ui.label(
            RichText::new(line)
                .size(crate::theme::legible(10.5))
                .color(if problem { AMBER } else { muted_color() }),
        );
    }
    if let Some(inventory) =
        inventory.filter(|inventory| inventory.supported && !inventory.picks_on_desktop())
    {
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
        ui.add_space(4.0);
        ui.label(section_label(l.text(Msg::ScFollow)));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = OPTION_KEY_SPACING;
            let size = option_key_size(
                ui,
                &[
                    l.text(Msg::ScMatchExact),
                    l.text(Msg::ScMatchContains),
                    l.text(Msg::ScMatchAny),
                ],
            );
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
                if add_option_key(ui, true, l.text(label), selected, size)
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
    // Four equal columns, each edge's name over its field, so the row
    // never wraps whatever the language.
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let width = ((ui.available_width() - 3.0 * spacing) / 4.0 - 0.5).floor();
        let height = ui.spacing().interact_size.y;
        let crop = &mut draft.config.crop;
        for (label, value) in [
            (Msg::ScCropLeft, &mut crop.left),
            (Msg::ScCropTop, &mut crop.top),
            (Msg::ScCropRight, &mut crop.right),
            (Msg::ScCropBottom, &mut crop.bottom),
        ] {
            ui.allocate_ui_with_layout(
                Vec2::new(width, 2.0 * height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::Label::new(
                            RichText::new(l.text(label)).size(crate::theme::legible(10.5)),
                        )
                        .truncate(),
                    );
                    ui.add_sized(
                        Vec2::new(width, height),
                        egui::DragValue::new(value)
                            .range(0..=MAX_SCREEN_CAPTURE_CROP)
                            .speed(1.0),
                    )
                    .on_hover_text(l.text(Msg::ScCropHover));
                },
            );
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
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = OPTION_KEY_SPACING;
        let size = option_key_size(ui, &[l.text(Msg::ScStallFreeze), l.text(Msg::ScStallBlack)]);
        for (slate, label, hover) in [
            (
                StallSlate::Freeze,
                Msg::ScStallFreeze,
                Msg::ScStallFreezeHover,
            ),
            (StallSlate::Black, Msg::ScStallBlack, Msg::ScStallBlackHover),
        ] {
            let selected = draft.config.slate == slate;
            if add_option_key(ui, true, l.text(label), selected, size)
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
    // A Wayland server's sharing dialog gives no application audio.
    let portal = inventory.is_some_and(ScreenCaptureInventory::picks_on_desktop);
    let app_reason = if portal {
        Msg::ScAudioAppPortalHover.en()
    } else {
        Msg::ScAudioAppScreenHover.en()
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = OPTION_KEY_SPACING;
        let size = option_key_size(
            ui,
            &[
                l.text(Msg::ScAudioNone),
                l.text(Msg::ScAudioSystem),
                l.text(Msg::ScAudioApp),
            ],
        );
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
            let enabled = audio != ScreenCaptureAudio::Application || (window && !portal);
            if add_option_key(ui, enabled, l.text(label), selected, size)
                .hint(l.text(hover), app_reason)
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

/// Space between the keys of one option row.
const OPTION_KEY_SPACING: f32 = 6.0;
/// Columns of the option rows.
const OPTION_COLUMNS: f32 = 3.0;
/// The height of a one-line option key.
const OPTION_KEY_HEIGHT: f32 = 28.0;

/// The font of an option key's label.
fn option_font() -> egui::FontId {
    egui::FontId::proportional(crate::theme::legible(11.5))
}

/// The key size that puts an option row's keys on one line in three
/// columns, with each label as [`option_label`] breaks it, every key of
/// the row as tall as the tallest. `None` when a label cannot fit a column
/// even on two lines (the row then wraps keys of their natural width).
fn option_key_size(ui: &Ui, labels: &[&str]) -> Option<Vec2> {
    // Every option row is laid out in three columns, so the keys of the
    // two- and three-key rows line up.
    let count = OPTION_COLUMNS;
    // A point to spare so rounding never pushes the last key to a new line.
    let width = ((ui.available_width() - OPTION_KEY_SPACING * (count - 1.0)) / count - 1.0).floor();
    let text_width = option_text_width(ui, width);
    let padding = ui.spacing().button_padding;
    let mut tallest = 0.0_f32;
    for label in labels {
        let label = option_label(ui, label, text_width)?;
        let size = ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(label, option_font(), egui::Color32::WHITE)
                .size()
        });
        tallest = tallest.max(size.y);
    }
    Some(Vec2::new(
        width,
        (tallest + 2.0 * padding.y + 6.0).max(OPTION_KEY_HEIGHT),
    ))
}

/// The width a label has inside an option key `width` wide.
fn option_text_width(ui: &Ui, width: f32) -> f32 {
    width - 2.0 * ui.spacing().button_padding.x - 1.0
}

/// `label` as it reads in an option key with `width` for text: on one line
/// when it fits, else on the two most even lines that fit; `None` when
/// neither does.
fn option_label(ui: &Ui, label: &str, width: f32) -> Option<String> {
    let measure = |text: &str| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), option_font(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    if measure(label) <= width {
        return Some(label.to_owned());
    }
    let words: Vec<&str> = label.split_whitespace().collect();
    (1..words.len())
        .map(|split| {
            let (first, second) = (words[..split].join(" "), words[split..].join(" "));
            (measure(&first).max(measure(&second)), first, second)
        })
        .filter(|(widest, ..)| *widest <= width)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, first, second)| format!("{first}\n{second}"))
}

/// Adds a kind key of `size` (its label broken to fit), or of its natural
/// size.
fn add_option_key(
    ui: &mut Ui,
    enabled: bool,
    label: &str,
    selected: bool,
    size: Option<Vec2>,
) -> egui::Response {
    let fitted = size.and_then(|size| {
        option_label(ui, label, option_text_width(ui, size.x)).map(|label| (size, label))
    });
    match fitted {
        Some((size, label)) => ui.add_enabled(enabled, kind_key(&label, selected).min_size(size)),
        None => ui.add_enabled(enabled, kind_key(label, selected)),
    }
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

/// The audio state badge of a screen capture input (its text, colour,
/// and hover help: the state's line with its reason, then what the state
/// means), or `None` while its audio is off.
pub(crate) fn audio_badge(
    l: Locale,
    audio: ScreenCaptureAudio,
    status: &ScreenCaptureStatus,
) -> Option<(String, Color32, String)> {
    let line = audio_line(l, audio, status)?;
    let (text, color, meaning) = match status.audio {
        ScreenCaptureAudioState::Off => return None,
        ScreenCaptureAudioState::Opening => (
            Msg::ScAudioBadgeOpening,
            muted_color(),
            Msg::ScAudioBadgeOpeningHover,
        ),
        ScreenCaptureAudioState::Live => (
            Msg::ScAudioBadgeLive,
            Color32::from_rgb(30, 150, 70),
            Msg::ScAudioBadgeLiveHover,
        ),
        ScreenCaptureAudioState::NoStream => (
            Msg::ScAudioBadgeNoStream,
            AMBER,
            Msg::ScAudioBadgeNoStreamHover,
        ),
        ScreenCaptureAudioState::Unsupported => (
            Msg::ScAudioBadgeUnsupported,
            ERROR,
            Msg::ScAudioBadgeUnsupportedHover,
        ),
        ScreenCaptureAudioState::Failed => {
            (Msg::ScAudioBadgeFailed, ERROR, Msg::ScAudioBadgeFailedHover)
        }
    };
    Some((
        l.text(text).to_owned(),
        color,
        format!("{line}\n{}", l.text(meaning)),
    ))
}

/// Whether `entry` shows an audio state badge (its audio is not off).
pub(crate) fn shows_audio_badge(entry: &ScreenCaptureEntry) -> bool {
    entry.status.audio != ScreenCaptureAudioState::Off
}

/// The audio state badge as a small key `width` wide (at least its text)
/// with its hover help; nothing while the audio is off.
pub(crate) fn audio_badge_key(
    ui: &mut Ui,
    l: Locale,
    entry: &ScreenCaptureEntry,
    size: egui::Vec2,
) -> Option<egui::Response> {
    let (text, color, hover) = audio_badge(l, entry.config.audio, &entry.status)?;
    Some(
        ui.add(
            egui::Button::new(
                RichText::new(text)
                    .size(crate::theme::legible(9.0))
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(color)
            .truncate()
            .min_size(size),
        )
        .on_hover_text(hover),
    )
}

/// The audio badge on an input tile: in the picture's bottom leading
/// corner, clear of the capture state badge in the trailing one.
pub(crate) fn tile_audio_badge(
    ui: &mut Ui,
    thumb: egui::Rect,
    l: Locale,
    entry: &ScreenCaptureEntry,
) -> Option<egui::Response> {
    if !shows_audio_badge(entry) {
        return None;
    }
    let rect = egui::Rect::from_min_size(
        egui::pos2(thumb.min.x + 6.0, thumb.max.y - 15.0 - 8.0),
        egui::vec2((thumb.width() / 2.0 - 10.0).max(24.0), 15.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().button_padding = egui::vec2(4.0, 0.0);
    audio_badge_key(&mut child, l, entry, egui::vec2(0.0, 15.0))
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
        ScreenCaptureState::AwaitingSelection => (l.text(Msg::ScBadgePick).to_owned(), AMBER),
        ScreenCaptureState::Live => (l.text(Msg::Live).to_owned(), Color32::from_rgb(30, 150, 70)),
        ScreenCaptureState::TargetGone => (l.text(Msg::ScBadgeGone).to_owned(), AMBER),
        ScreenCaptureState::Reopening { attempt, .. } => (
            l.format(
                Msg::SaBadgeReopening,
                &[("attempt", Arg::from(u64::from(attempt)))],
            ),
            AMBER,
        ),
        ScreenCaptureState::Denied => (l.text(Msg::ScBadgeDenied).to_owned(), ERROR),
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
    if status.state == ScreenCaptureState::AwaitingSelection {
        lines.push(l.text(Msg::ScDetailPick).to_owned());
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

    /// The form's width after a few passes at the Add input dialog's
    /// content width, in `language`, with a window followed by its rule.
    fn form_width(language: Language) -> f32 {
        const WIDTH: f32 = 454.0;
        let context = egui::Context::default();
        let state = StudioUiState::new(crate::StudioConnectionStatus::Ready);
        let mut draft = ScreenCaptureDraft::default();
        draft.choose_window(&window(7, "Slides", "Keynote deck"));
        draft.config.audio = ScreenCaptureAudio::Application;
        let mut width = 0.0;
        for _ in 0..4 {
            context.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    Vec2::new(1920.0, 1080.0),
                )),
                ..egui::RawInput::default()
            });
            let mut ui = Ui::new(
                context.clone(),
                egui::Id::new("screen-capture-form-width"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(egui::Rect::from_min_size(
                        egui::pos2(10.0, 10.0),
                        Vec2::new(WIDTH, 2000.0),
                    )),
            );
            let _ = draw_screen_capture_form(
                &mut ui,
                Locale::new(language),
                &state,
                "test",
                &mut draft,
            );
            width = ui.min_rect().width();
            let _ = context.end_pass();
        }
        width
    }

    #[test]
    fn the_form_stays_inside_the_dialog_in_every_language() {
        for language in [Language::English, Language::Spanish] {
            let width = form_width(language);
            assert!(width <= 454.0, "{language:?}: {width}");
        }
    }

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

    #[test]
    fn every_audio_state_but_off_has_its_own_badge_and_help() {
        let entries = crate::hover_audit::screen_capture_audio_entries();
        for language in Language::ALL {
            let l = Locale::new(language);
            let mut texts = Vec::new();
            for entry in &entries {
                assert!(shows_audio_badge(entry));
                let (text, _, hover) = audio_badge(l, entry.config.audio, &entry.status).unwrap();
                let line = audio_line(l, entry.config.audio, &entry.status).unwrap();
                assert!(hover.starts_with(&line), "{hover}");
                assert_eq!(hover.lines().count(), 2, "{hover}");
                texts.push(text);
            }
            texts.dedup();
            assert_eq!(texts.len(), entries.len(), "{texts:?}");
            let mut off = entries[1].clone();
            off.status.audio = ScreenCaptureAudioState::Off;
            assert!(!shows_audio_badge(&off));
            assert_eq!(audio_badge(l, off.config.audio, &off.status), None);
        }
        let l = Locale::new(Language::English);
        let (text, color, hover) =
            audio_badge(l, entries[3].config.audio, &entries[3].status).unwrap();
        assert_eq!(text, "AUDIO UNSUPPORTED");
        assert_eq!(color, ERROR);
        assert!(
            hover.contains("per-application audio needs Windows 10 2004 or later"),
            "{hover}"
        );
        let (text, _, hover) = audio_badge(
            Locale::new(Language::Spanish),
            entries[2].config.audio,
            &entries[2].status,
        )
        .unwrap();
        assert_eq!(text, "SIN FLUJO");
        assert!(hover.contains("todavía no reproduce sonido"), "{hover}");
    }

    /// The console draws the audio badge on input 6's tile and on its
    /// mixer strip (beside its SYNC key).
    #[test]
    fn the_tile_and_the_strip_show_the_audio_badge() {
        let state = crate::hover_audit::screen_capture_audio_fixture();
        let mut shell = crate::StudioShell::default();
        let context = egui::Context::default();
        context.all_styles_mut(|style| style.debug.show_interactive_widgets = true);
        for _ in 0..3 {
            context.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1920.0, 1080.0),
                )),
                ..egui::RawInput::default()
            });
            let mut ui = Ui::new(
                context.clone(),
                egui::Id::new("audio-badge"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let _ = shell.draw(&mut ui, &state);
            drop(ui);
            let _ = context.end_pass();
        }
        let badges = context.viewport(|viewport| {
            let widgets = &viewport.prev_pass.widgets;
            widgets
                .layers()
                .flat_map(|(_, rects)| rects.iter().copied())
                .filter(|widget| widget.sense.interactive())
                .filter(|widget| {
                    widgets
                        .info(widget.id)
                        .and_then(|info| info.label.clone())
                        .is_some_and(|label| label == "AUDIO UNSUPPORTED")
                })
                .count()
        });
        assert_eq!(badges, 2);
    }
}
