//! Live input add and remove (Protocol 2.23).
//!
//! Pure view-model helpers pick the new input's identifier and suggested
//! name, gate both actions with an operator reason, word the refusal toasts,
//! and track each sent request until replicated state or a refusal resolves
//! it. The drawing half adds the Add input dialog and the Remove confirmation.
//!
//! Only the generators the simulated daemon realizes can be added live: a
//! solid colour and colour bars, both with silent audio. Every other input
//! kind is added offline.

use egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{IdKind, LiveInputSource, RefusalReason};
use fm_types::{InputId, MAX_INPUT_NAME_BYTES};
use fm_ui_model::{ClientView, ManualTransitionStatus};

use crate::edit_history::{INPUT_LOCKED_REASON, PROJECT_LOCKED_REASON};
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_bank::project_editing_available;
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::network_stream_ui::FieldProblem;
use crate::theme::{
    ACCENT_FILL, AMBER, MONITOR_BLACK, PROGRAM, PROGRAM_FILL, Tone, accent_color,
    border_strong_color, mono, muted_color, paint_lamp, section_label, text_color, text_dim_color,
    widget_color,
};
use crate::toasts::ToastKind;
use crate::{StudioIntent, StudioShell, StudioUiState};

/// Most inputs a show may hold; equals the protocol list bound, which the
/// engine also enforces (`too_many_inputs`).
pub const MAX_LIVE_INPUTS: usize = fm_protocol::MAX_LIST_ITEMS;

pub(crate) const INPUT_LIMIT_REASON: &str = Msg::ReasonInputLimit.en();
pub(crate) const UNKNOWN_INPUT_REASON: &str = Msg::ReasonUnknownInput.en();
pub(crate) const REMOVAL_PENDING_REASON: &str = Msg::ReasonRemovalPending.en();
pub(crate) const ON_PROGRAM_REASON: &str = Msg::ReasonOnProgram.en();
pub(crate) const ON_PREVIEW_REASON: &str = Msg::ReasonOnPreview.en();
pub(crate) const IN_TRANSITION_REASON: &str = Msg::ReasonInTransition.en();
pub(crate) const STINGER_MEDIA_REASON: &str = Msg::ReasonStingerMedia.en();
pub(crate) const OVERLAY_ON_AIR_REASON: &str = Msg::ReasonOverlayOnAir.en();
pub(crate) const OVERLAY_QUEUED_REASON: &str = Msg::ReasonOverlayQueued.en();

/// An outcome is assumed lost when a request was never seen pending after
/// this long (for example the queue was full or the payload was refused
/// locally before sending).
pub(crate) const CATALOGUE_TIMEOUT_MS: u32 = 15_000;

/// One live input add or remove the client has sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueRequest {
    Add { input: InputId, name: String },
    Remove { input: InputId },
}

impl CatalogueRequest {
    /// The input the request adds or removes.
    #[must_use]
    pub const fn input(&self) -> InputId {
        match self {
            Self::Add { input, .. } | Self::Remove { input } => *input,
        }
    }

    const fn is_add(&self) -> bool {
        matches!(self, Self::Add { .. })
    }
}

/// A refused live input add or remove with its stable reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueRefusal {
    /// The refused command's identifier; each refusal is announced once.
    pub command_id: String,
    pub request: CatalogueRequest,
    /// Top-level rejection code (`conflict`, `invalid_command`, ...).
    pub code: String,
    /// Stable reason code from the first field issue, when present
    /// (`input_in_use`, `duplicate_name`, `project_locked`, ...).
    pub reason: Option<String>,
    /// The field issue's message, else the rejection message.
    pub message: String,
    /// The typed refusal reason (Protocol 2.45), worded in the console
    /// language; `None` falls back to `reason` and `message`.
    pub typed_reason: Option<RefusalReason>,
}

/// The generator a new input is added with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NewInputKind {
    Solid,
    Bars,
    /// A network stream input (IN-015).
    Network,
    /// A local camera or capture card input (IN-001).
    Capture,
}

impl NewInputKind {
    /// Base of the suggested name (`Solid Colour 1`, `Colour Bars 1`).
    #[must_use]
    pub const fn base_name(self) -> &'static str {
        self.base_name_key().en()
    }

    const fn base_name_key(self) -> Msg {
        match self {
            Self::Solid => Msg::NameSolidColour,
            Self::Bars => Msg::NameColourBars,
            Self::Network => Msg::NameNetworkStream,
            Self::Capture => Msg::NameCapture,
        }
    }

    const fn key_label(self) -> Msg {
        match self {
            Self::Solid => Msg::KindSolidKey,
            Self::Bars => Msg::KindBarsKey,
            Self::Network => Msg::KindNetworkKey,
            Self::Capture => Msg::KindCaptureKey,
        }
    }

    const fn description(self) -> Msg {
        match self {
            Self::Solid => Msg::KindSolidHover,
            Self::Bars => Msg::KindBarsHover,
            Self::Network => Msg::KindNetworkHover,
            Self::Capture => Msg::KindCaptureHover,
        }
    }
}

/// Picks the next free input identifier: one past the largest identifier in
/// use (current inputs and adds still in flight), so a new input never reuses
/// an identifier another editor just saw. Falls back to the smallest free
/// identifier when the largest is exhausted.
#[must_use]
pub fn next_free_input_id(taken: &[InputId]) -> Option<InputId> {
    let largest = taken
        .iter()
        .map(|input| input.get().get())
        .max()
        .unwrap_or(0);
    largest
        .checked_add(1)
        .and_then(core::num::NonZeroU128::new)
        .or_else(|| {
            (1..=u128::MAX)
                .filter_map(core::num::NonZeroU128::new)
                .find(|candidate| !taken.iter().any(|input| input.get() == *candidate))
        })
        .map(InputId::new)
}

/// Suggests a unique name: the kind's base name and the lowest number not
/// already used by an input or an add in flight.
#[must_use]
pub fn suggested_input_name(kind: NewInputKind, taken: &[&str]) -> String {
    localized_input_name(Locale::ENGLISH, kind, taken)
}

/// [`suggested_input_name`] with the base name in the console language
/// (`Color sólido 1` in Spanish). The name is project data, so it is only a
/// suggestion the operator may edit.
pub(crate) fn localized_input_name(l: Locale, kind: NewInputKind, taken: &[&str]) -> String {
    let base = l.text(kind.base_name_key());
    // One of the first `taken.len() + 1` numbers is always free.
    (1..=taken.len() + 1)
        .map(|number| format!("{base} {number}"))
        .find(|name| !taken.contains(&name.as_str()))
        .unwrap_or_else(|| base.to_owned())
}

/// Why a new input's name would be refused, if it would be.
#[must_use]
pub fn input_name_problem(name: &str, taken: &[&str]) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some(Msg::ReasonNameEmpty.en())
    } else if name.len() > MAX_INPUT_NAME_BYTES {
        Some(Msg::ReasonNameTooLongBytes.en())
    } else if taken.contains(&name) {
        Some(Msg::ReasonNameTaken.en())
    } else {
        None
    }
}

/// Availability of Add input or of removing one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogueAvailability {
    pub enabled: bool,
    /// Why the action is unavailable; empty when enabled.
    pub reason: &'static str,
    /// Right-column menu marker (`LOCKED`, `PGM`, `PVW`, `IN USE`), if any.
    pub mark: Option<&'static str>,
}

impl CatalogueAvailability {
    const AVAILABLE: Self = Self {
        enabled: true,
        reason: "",
        mark: None,
    };

    const fn blocked(reason: &'static str, mark: Option<&'static str>) -> Self {
        Self {
            enabled: false,
            reason,
            mark,
        }
    }

    /// Whether a padlock is what blocks the action.
    #[must_use]
    pub fn locked(self) -> bool {
        self.mark == Some(Msg::Locked.en())
    }
}

/// Add input availability, in the order an operator would fix it:
/// connection, synchronized state, `edit_project`, the project padlock, then
/// the input limit (counting adds still in flight).
#[must_use]
pub fn add_input_availability(
    state: &StudioUiState,
    adds_in_flight: usize,
) -> CatalogueAvailability {
    if !project_editing_available(state) {
        return CatalogueAvailability::blocked(
            unavailable_reason(state, Capability::EditProject),
            None,
        );
    }
    let Some(view) = &state.view else {
        return CatalogueAvailability::blocked(
            unavailable_reason(state, Capability::EditProject),
            None,
        );
    };
    if view.edit_locks.project {
        return CatalogueAvailability::blocked(PROJECT_LOCKED_REASON, Some(Msg::Locked.en()));
    }
    if view.inputs.len() + adds_in_flight >= MAX_LIVE_INPUTS {
        return CatalogueAvailability::blocked(INPUT_LIMIT_REASON, None);
    }
    CatalogueAvailability::AVAILABLE
}

/// Why the replicated state keeps `input` from being removed, mirroring the
/// server's rule: desired or realized Program or Preview, a manual transition
/// endpoint, a Stinger slot's media, an overlay on air, or an overlay queue.
#[must_use]
pub fn input_usage_reason(view: &ClientView, input: InputId) -> Option<&'static str> {
    let switcher = view.switcher;
    let in_manual = |status: ManualTransitionStatus| match status {
        ManualTransitionStatus::Active(transition) => {
            transition.from == input || transition.to == input
        }
        ManualTransitionStatus::Inactive => false,
    };
    let overlays = || view.desired_overlays.iter().chain(&view.realized_overlays);
    if switcher.desired.program == input || switcher.realized.program == input {
        Some(ON_PROGRAM_REASON)
    } else if switcher.desired.preview == input || switcher.realized.preview == input {
        Some(ON_PREVIEW_REASON)
    } else if in_manual(switcher.desired_manual_transition)
        || in_manual(switcher.realized_manual_transition)
    {
        Some(IN_TRANSITION_REASON)
    } else if view
        .stingers
        .iter()
        .any(|stinger| stinger.media_input == input)
    {
        Some(STINGER_MEDIA_REASON)
    } else if overlays()
        .any(|overlay| overlay.source == Some(input) && (overlay.active || overlay.opacity > 0))
    {
        Some(OVERLAY_ON_AIR_REASON)
    } else if overlays().any(|overlay| overlay.queued_sources.contains(&input)) {
        Some(OVERLAY_QUEUED_REASON)
    } else {
        None
    }
}

/// Remove availability for one input: session gates, the padlocks, a removal
/// already in flight, then the live references the server refuses.
#[must_use]
pub fn remove_input_availability(
    state: &StudioUiState,
    input: InputId,
    removal_in_flight: bool,
) -> CatalogueAvailability {
    let Some(view) = state
        .view
        .as_ref()
        .filter(|_| project_editing_available(state))
    else {
        return CatalogueAvailability::blocked(
            unavailable_reason(state, Capability::EditProject),
            None,
        );
    };
    if view.edit_locks.project {
        return CatalogueAvailability::blocked(PROJECT_LOCKED_REASON, Some(Msg::Locked.en()));
    }
    if !view.inputs.contains(&input) {
        return CatalogueAvailability::blocked(UNKNOWN_INPUT_REASON, None);
    }
    if view.edit_locks.is_input_locked(input) {
        return CatalogueAvailability::blocked(INPUT_LOCKED_REASON, Some(Msg::Locked.en()));
    }
    if removal_in_flight {
        return CatalogueAvailability::blocked(REMOVAL_PENDING_REASON, Some(Msg::Pending.en()));
    }
    match input_usage_reason(view, input) {
        Some(reason) => CatalogueAvailability::blocked(
            reason,
            Some(match reason {
                ON_PROGRAM_REASON => "PGM",
                ON_PREVIEW_REASON => "PVW",
                _ => Msg::InUse.en(),
            }),
        ),
        None => CatalogueAvailability::AVAILABLE,
    }
}

/// The stable reason code of a refused add or remove (`input_in_use`): the
/// typed reason's kind, else the field issue's code, else the rejection's.
#[must_use]
pub fn catalogue_refusal_code(refusal: &CatalogueRefusal) -> &str {
    match &refusal.typed_reason {
        Some(reason) => reason.kind(),
        None => refusal.reason.as_deref().unwrap_or(refusal.code.as_str()),
    }
}

/// Words a refused add or remove for its toast in the console language: the
/// action, what it means for this input, and what to do next. The stable
/// reason code stays out of the sentence (it is the toast's hover detail,
/// [`catalogue_refusal_code`]). A typed reason (Protocol 2.45) is worded from its parameters; a
/// refusal without one is worded from its stable field-issue code, and a
/// daemon detail the console does not recognise passes through.
#[must_use]
pub fn catalogue_refusal_message(l: Locale, refusal: &CatalogueRefusal, name: &str) -> String {
    let lead = if refusal.request.is_add() {
        Msg::AddRefused
    } else {
        Msg::RemoveRefused
    };
    let code = catalogue_refusal_code(refusal);
    let named = |msg: Msg| l.format(msg, &[("name", Arg::Str(name))]);
    let input = refusal.request.input();
    // The refused input is named by the operator's name; others by number.
    let input_names = |id: u128| (id == input.get().get()).then(|| name.to_owned());
    let advice = match &refusal.typed_reason {
        Some(RefusalReason::DuplicateId {
            kind: IdKind::Input,
            id,
        }) => l.format(
            Msg::RefuseDuplicateInput,
            &[("input", Arg::Str(&id.to_string()))],
        ),
        Some(RefusalReason::BoundsExceeded { field, max }) if field == "inputs" => {
            l.format(Msg::RefuseTooManyInputs, &[("limit", Arg::Plain(*max))])
        }
        Some(RefusalReason::UnknownId {
            kind: IdKind::Input,
            id,
        }) if *id == input.get().get() => named(Msg::RefuseUnknownInput),
        Some(RefusalReason::DuplicateName) => named(Msg::RefuseDuplicateName),
        Some(RefusalReason::ProjectLocked) => l.text(Msg::RefuseProjectLocked).to_owned(),
        Some(RefusalReason::InputLocked { input: locked }) if *locked == input.get().get() => {
            named(Msg::RefuseInputLocked)
        }
        Some(RefusalReason::RuntimeUnsupported) => l.text(Msg::RefuseUnavailable).to_owned(),
        Some(reason) => fm_ui_model::i18n::refusal_reason_text(l, reason, &input_names),
        None => match code {
            "duplicate_input" => l.format(
                Msg::RefuseDuplicateInput,
                &[("input", Arg::Str(&input.to_string()))],
            ),
            "too_many_inputs" => l.format(
                Msg::RefuseTooManyInputs,
                &[("limit", Arg::Plain(MAX_LIVE_INPUTS as u64))],
            ),
            "unknown_input" => named(Msg::RefuseUnknownInput),
            "invalid_name" => l.text(Msg::RefuseInvalidName).to_owned(),
            "duplicate_name" => named(Msg::RefuseDuplicateName),
            "project_locked" => l.text(Msg::RefuseProjectLocked).to_owned(),
            "input_locked" => named(Msg::RefuseInputLocked),
            "unavailable" => l.text(Msg::RefuseUnavailable).to_owned(),
            "unsupported_scheme" => l.text(Msg::RsnUnsupportedScheme).to_owned(),
            _ => refusal.message.clone(),
        },
    };
    l.format(lead, &[("advice", Arg::Str(&advice))])
}

/// Builds the Add input intent from the dialog, or says why it is blocked.
pub(crate) fn add_input_intent(
    draft: &AddInputDraft,
    state: &StudioUiState,
    tracker: &CatalogueTracker,
) -> Result<StudioIntent, &'static str> {
    let availability = add_input_availability(state, tracker.adds_in_flight(state));
    if !availability.enabled {
        return Err(availability.reason);
    }
    let view = state.view.as_ref().ok_or(UNKNOWN_INPUT_REASON)?;
    let taken = tracker.taken_names(state);
    if let Some(problem) = input_name_problem(&draft.name, &taken) {
        return Err(problem);
    }
    let input =
        next_free_input_id(&tracker.taken_ids(view, state)).ok_or(Msg::ReasonNoFreeId.en())?;
    Ok(StudioIntent::AddInput {
        input,
        name: draft.name.trim().to_owned(),
        source: draft.source()?,
    })
}

/// Add input dialog state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AddInputDraft {
    pub(crate) kind: NewInputKind,
    pub(crate) name: String,
    /// The operator typed a name; kind changes no longer re-suggest one.
    pub(crate) name_edited: bool,
    pub(crate) color: Color32,
    /// The stream form of a network stream input (IN-015).
    pub(crate) stream: crate::network_stream_ui::NetworkStreamDraft,
    /// The capture form of a camera or capture card input (IN-001).
    pub(crate) capture: crate::capture_input_ui::CaptureDraft,
    /// The name field takes keyboard focus once, when the dialog opens.
    focus_pending: bool,
    /// Language of the suggested name.
    locale: Locale,
}

/// Colour presets offered beside the picker.
pub(crate) const COLOUR_PRESETS: [(Msg, Color32); 8] = [
    (Msg::ColourBlack, Color32::from_rgb(0, 0, 0)),
    (Msg::ColourWhite, Color32::from_rgb(235, 235, 235)),
    (Msg::ColourGrey, Color32::from_rgb(64, 68, 74)),
    (Msg::ColourRed, Color32::from_rgb(200, 32, 40)),
    (Msg::ColourGreen, Color32::from_rgb(30, 150, 70)),
    (Msg::ColourBlue, Color32::from_rgb(20, 60, 140)),
    (Msg::ColourAmber, Color32::from_rgb(240, 168, 40)),
    (Msg::ColourMagenta, Color32::from_rgb(170, 40, 150)),
];

impl AddInputDraft {
    pub(crate) fn new(state: &StudioUiState, tracker: &CatalogueTracker) -> Self {
        let kind = NewInputKind::Solid;
        Self {
            kind,
            name: localized_input_name(tracker.locale, kind, &tracker.taken_names(state)),
            name_edited: false,
            color: COLOUR_PRESETS[5].1,
            stream: crate::network_stream_ui::NetworkStreamDraft::default(),
            capture: crate::capture_input_ui::CaptureDraft::default(),
            focus_pending: true,
            locale: tracker.locale,
        }
    }

    /// Switches the kind, re-suggesting the name unless the operator typed one.
    pub(crate) fn set_kind(&mut self, kind: NewInputKind, taken: &[&str]) {
        self.kind = kind;
        if !self.name_edited {
            self.name = localized_input_name(self.locale, kind, taken);
        }
    }

    /// The new input's source, or why the stream or capture form is
    /// unusable.
    pub(crate) fn source(&self) -> Result<LiveInputSource, &'static str> {
        Ok(match self.kind {
            NewInputKind::Solid => LiveInputSource::Solid {
                red: self.color.r(),
                green: self.color.g(),
                blue: self.color.b(),
                alpha: 255,
            },
            NewInputKind::Bars => LiveInputSource::Bars,
            NewInputKind::Network => {
                LiveInputSource::NetworkStream(Box::new(self.stream.config()?))
            }
            NewInputKind::Capture => LiveInputSource::Capture(Box::new(self.capture.config()?)),
        })
    }

    /// The stream or capture form field the source fails on: its label,
    /// the reason, and whether it is only still to be filled in.
    pub(crate) fn source_problem(&self) -> Option<FieldProblem<Msg>> {
        let problem = match self.kind {
            NewInputKind::Solid | NewInputKind::Bars => return None,
            NewInputKind::Network => {
                let problem = self.stream.checked().err()?;
                (problem.field.label(), problem.reason, problem.prompt)
            }
            NewInputKind::Capture => {
                let problem = self.capture.checked().err()?;
                (problem.field.label(), problem.reason, problem.prompt)
            }
        };
        Some(FieldProblem {
            field: problem.0,
            reason: problem.1,
            prompt: problem.2,
        })
    }
}

/// The footer line of the Add input dialog for a blocked add: a short
/// pointer to the field to fix when the reason belongs to one (the reason
/// itself shows under that field), else the reason; and the full
/// `Unavailable: …` line for its hover.
pub(crate) fn add_input_summary(
    l: Locale,
    draft: &AddInputDraft,
    taken: &[&str],
    reason: &str,
) -> (String, String) {
    let full = l10n::unavailable(l, reason);
    let field = if input_name_problem(&draft.name, taken) == Some(reason) {
        Some(Msg::AddInputName)
    } else {
        draft
            .source_problem()
            .filter(|problem| problem.reason == reason && !problem.prompt)
            .map(|problem| problem.field)
    };
    let short = field.map_or_else(
        || full.clone(),
        |field| l.format(Msg::AddInputFixField, &[("field", Arg::Str(l.text(field)))]),
    );
    (short, full)
}

/// Where the Add input dialog's top edge sits: a fixed distance from the
/// top of the screen, so the dialog grows and shrinks downwards as hint
/// lines and sections come and go instead of re-centring.
fn dialog_top(screen_height: f32) -> f32 {
    (screen_height * 0.06).clamp(12.0, 64.0)
}

/// Room the dialog's title, footer, and margins take besides its body.
const DIALOG_CHROME: f32 = 160.0;

/// A modal like [`crate::menu::help_modal`], anchored at the top centre of
/// the screen; `add_contents` gets the height its scrolled body may take.
fn anchored_modal(
    context: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut Ui, f32),
) -> bool {
    let l = l10n::locale(context);
    let screen = context.content_rect();
    let top = dialog_top(screen.height());
    let body = (screen.height() - 2.0 * top - DIALOG_CHROME).max(120.0);
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new(id))
        .area(
            egui::Modal::default_area(egui::Id::new(id))
                .anchor(Align2::CENTER_TOP, Vec2::new(0.0, top)),
        )
        .backdrop_color(Color32::from_black_alpha(150))
        .frame(
            egui::Frame::new()
                .fill(crate::theme::raised_color())
                .stroke(Stroke::new(
                    crate::theme::line_width(),
                    border_strong_color(),
                ))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(context, |ui| {
            ui.set_width(width);
            // The area offers last frame's size; the body may grow to the
            // screen's height.
            ui.set_max_height(screen.height() - 2.0 * top);
            // As in help_modal, the close key's place is reserved in the
            // title row and the key created last, for the Tab order.
            let mut close_slot = Rect::NOTHING;
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).strong().color(text_color()));
                ui.with_layout(l10n::trailing_layout(l), |ui| {
                    close_slot = ui
                        .allocate_exact_size(Vec2::splat(crate::theme::MIN_TARGET), Sense::hover())
                        .0;
                });
            });
            ui.separator();
            add_contents(ui, body);
            if ui
                .put(close_slot, crate::theme::close_key(14.0))
                .on_hover_text(l.text(Msg::CloseEsc))
                .clicked()
            {
                close = true;
            }
        });
    close || response.should_close()
}

/// `#rrggbb` text for a colour.
pub(crate) fn hex_colour(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Remove confirmation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoveConfirm {
    pub(crate) input: InputId,
    pub(crate) name: String,
}

/// One sent add or remove awaiting its outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Outstanding {
    request: CatalogueRequest,
    /// Display name: the new name, or the removed input's name at send time.
    name: String,
    key: String,
    age_ms: u32,
    /// The request was seen among the client's pending commands.
    seen_pending: bool,
}

/// A toast the tracker raises or replaces, keyed per request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CatalogueToast {
    pub(crate) key: String,
    pub(crate) kind: ToastKind,
    pub(crate) message: String,
    /// Hover detail: a refusal's stable reason code.
    pub(crate) detail: Option<String>,
}

/// What one frame of tracking resolved.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CatalogueOutcomes {
    pub(crate) toasts: Vec<CatalogueToast>,
    /// A refusal was announced here, so the generic error toast for the same
    /// rejection is redundant.
    pub(crate) refusal_announced: bool,
}

/// Tracks sent adds and removes from pending to success or refusal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CatalogueTracker {
    outstanding: Vec<Outstanding>,
    seen_refusal: Option<String>,
    next_key: u64,
    /// Language of the pending and outcome toasts this tracker words.
    pub(crate) locale: Locale,
}

impl CatalogueTracker {
    /// Records a sent request and returns its pending toast.
    pub(crate) fn submit(&mut self, request: CatalogueRequest, name: String) -> CatalogueToast {
        let key = format!("input-catalogue-{}", self.next_key);
        self.next_key = self.next_key.wrapping_add(1);
        let l = self.locale;
        let message = match &request {
            CatalogueRequest::Add { input, .. } => l.format(
                Msg::ToastAdding,
                &[
                    ("name", Arg::Str(&name)),
                    ("input", Arg::Str(&input.to_string())),
                ],
            ),
            CatalogueRequest::Remove { input } => l.format(
                Msg::ToastRemoving,
                &[
                    ("name", Arg::Str(&name)),
                    ("input", Arg::Str(&input.to_string())),
                ],
            ),
        };
        self.outstanding.push(Outstanding {
            request,
            name,
            key: key.clone(),
            age_ms: 0,
            seen_pending: false,
        });
        CatalogueToast {
            key,
            kind: ToastKind::Pending,
            message,
            detail: None,
        }
    }

    /// Records every add or remove among this frame's intents, naming a
    /// removed input from the current view.
    pub(crate) fn submit_intents(
        &mut self,
        intents: &[StudioIntent],
        view: Option<&ClientView>,
    ) -> Vec<CatalogueToast> {
        intents
            .iter()
            .filter_map(|intent| match intent {
                StudioIntent::AddInput { input, name, .. } => Some(self.submit(
                    CatalogueRequest::Add {
                        input: *input,
                        name: name.clone(),
                    },
                    name.clone(),
                )),
                StudioIntent::RemoveInput { input } => {
                    let name = view.and_then(|view| input_name(view, *input)).map_or_else(
                        || crate::monitors::fallback_input_name(self.locale, *input),
                        str::to_owned,
                    );
                    Some(self.submit(CatalogueRequest::Remove { input: *input }, name))
                }
                _ => None,
            })
            .collect()
    }

    /// Resolves requests against this frame's state: the replicated catalogue
    /// shows success, a matching refusal or a request that left the pending
    /// set without effect shows a refusal, and a request never seen pending
    /// times out.
    pub(crate) fn observe(&mut self, state: &StudioUiState, dt_ms: u32) -> CatalogueOutcomes {
        let mut outcomes = CatalogueOutcomes::default();
        let fresh_refusal = state
            .catalogue_refusal
            .as_ref()
            .filter(|refusal| self.seen_refusal.as_deref() != Some(refusal.command_id.as_str()));
        if let Some(refusal) = fresh_refusal {
            self.seen_refusal = Some(refusal.command_id.clone());
        }
        let ready = state.connection_status.controls_enabled();
        let locale = self.locale;
        self.outstanding.retain_mut(|outstanding| {
            outstanding.age_ms = outstanding.age_ms.saturating_add(dt_ms);
            let pending = state.catalogue_pending.contains(&outstanding.request);
            outstanding.seen_pending |= pending;
            let resolved = resolve(locale, outstanding, state, pending, ready, fresh_refusal);
            if let Some((kind, message)) = resolved {
                let refused = fresh_refusal
                    .filter(|refusal| refusal.request == outstanding.request)
                    .filter(|_| kind == ToastKind::Error);
                if refused.is_some() {
                    outcomes.refusal_announced = true;
                }
                outcomes.toasts.push(CatalogueToast {
                    key: outstanding.key.clone(),
                    kind,
                    message,
                    detail: refused.map(|refusal| {
                        fm_ui_model::i18n::refusal_code_detail(
                            locale,
                            catalogue_refusal_code(refusal),
                        )
                    }),
                });
                return false;
            }
            true
        });
        // A refusal of a request this console did not track (sent before a
        // restart, say) is still worded for the operator.
        if let Some(refusal) = fresh_refusal
            && !outcomes.refusal_announced
        {
            let name = match &refusal.request {
                CatalogueRequest::Add { name, .. } => name.clone(),
                CatalogueRequest::Remove { input } => state
                    .view
                    .as_ref()
                    .and_then(|view| input_name(view, *input))
                    .map_or_else(
                        || crate::monitors::fallback_input_name(locale, *input),
                        str::to_owned,
                    ),
            };
            outcomes.toasts.push(CatalogueToast {
                key: format!("input-catalogue-refusal-{}", refusal.command_id),
                kind: ToastKind::Error,
                message: catalogue_refusal_message(locale, refusal, &name),
                detail: Some(fm_ui_model::i18n::refusal_code_detail(
                    locale,
                    catalogue_refusal_code(refusal),
                )),
            });
            outcomes.refusal_announced = true;
        }
        outcomes
    }

    /// Adds still in flight, by this console or seen pending in the client.
    pub(crate) fn adds_in_flight(&self, state: &StudioUiState) -> usize {
        self.add_requests(state).count()
    }

    /// Whether a removal of `input` is in flight.
    pub(crate) fn removal_in_flight(&self, state: &StudioUiState, input: InputId) -> bool {
        let removal = CatalogueRequest::Remove { input };
        self.outstanding
            .iter()
            .any(|outstanding| outstanding.request == removal)
            || state.catalogue_pending.contains(&removal)
    }

    /// Names in use by inputs and by adds in flight.
    pub(crate) fn taken_names<'a>(&'a self, state: &'a StudioUiState) -> Vec<&'a str> {
        let mut names: Vec<&str> = state
            .view
            .as_ref()
            .map(|view| view.input_names.iter().map(String::as_str).collect())
            .unwrap_or_default();
        names.extend(self.add_requests(state).map(|(_, name)| name));
        names
    }

    /// Identifiers in use by inputs and by adds in flight.
    pub(crate) fn taken_ids(&self, view: &ClientView, state: &StudioUiState) -> Vec<InputId> {
        let mut ids = view.inputs.clone();
        ids.extend(self.add_requests(state).map(|(input, _)| input));
        ids
    }

    /// Display name of the first add in flight, for the bank's add tile.
    pub(crate) fn first_add_name(&self) -> Option<&str> {
        self.outstanding
            .iter()
            .find(|outstanding| outstanding.request.is_add())
            .map(|outstanding| outstanding.name.as_str())
    }

    fn add_requests<'a>(
        &'a self,
        state: &'a StudioUiState,
    ) -> impl Iterator<Item = (InputId, &'a str)> + 'a {
        let mut seen: Vec<InputId> = Vec::new();
        self.outstanding
            .iter()
            .map(|outstanding| &outstanding.request)
            .chain(&state.catalogue_pending)
            .filter_map(move |request| match request {
                CatalogueRequest::Add { input, name } if !seen.contains(input) => {
                    seen.push(*input);
                    Some((*input, name.as_str()))
                }
                _ => None,
            })
    }
}

fn input_name(view: &ClientView, input: InputId) -> Option<&str> {
    view.inputs
        .iter()
        .position(|candidate| *candidate == input)
        .map(|index| view.input_names[index].as_str())
}

/// The toast that resolves one request this frame, if it resolves.
fn resolve(
    l: Locale,
    outstanding: &Outstanding,
    state: &StudioUiState,
    pending: bool,
    ready: bool,
    fresh_refusal: Option<&CatalogueRefusal>,
) -> Option<(ToastKind, String)> {
    let name = outstanding.name.as_str();
    let add = outstanding.request.is_add();
    if let Some(view) = state.view.as_ref().filter(|_| ready) {
        match &outstanding.request {
            CatalogueRequest::Add { input, .. } => {
                if let Some(index) = view.inputs.iter().position(|candidate| candidate == input) {
                    return Some((
                        ToastKind::Success,
                        l.format(
                            Msg::ToastAdded,
                            &[
                                ("name", Arg::Str(&view.input_names[index])),
                                ("input", Arg::Str(&input.to_string())),
                                ("position", Arg::from(index + 1)),
                            ],
                        ),
                    ));
                }
            }
            CatalogueRequest::Remove { input } => {
                if !pending && !view.inputs.contains(input) {
                    return Some((
                        ToastKind::Success,
                        l.format(
                            Msg::ToastRemoved,
                            &[
                                ("name", Arg::Str(name)),
                                ("input", Arg::Str(&input.to_string())),
                            ],
                        ),
                    ));
                }
            }
        }
    }
    if let Some(refusal) = fresh_refusal.filter(|refusal| refusal.request == outstanding.request) {
        return Some((
            ToastKind::Error,
            catalogue_refusal_message(l, refusal, name),
        ));
    }
    if !ready || pending {
        return None;
    }
    let detail = state
        .error
        .as_deref()
        .unwrap_or_else(|| l.text(Msg::NoDaemonResult));
    let args = [("name", Arg::Str(name)), ("detail", Arg::Str(detail))];
    if outstanding.seen_pending {
        return Some((
            ToastKind::Error,
            l.format(
                if add {
                    Msg::ToastAddNotApplied
                } else {
                    Msg::ToastRemovalNotApplied
                },
                &args,
            ),
        ));
    }
    (outstanding.age_ms >= CATALOGUE_TIMEOUT_MS).then(|| {
        (
            ToastKind::Error,
            l.format(
                if add {
                    Msg::ToastAddNotConfirmed
                } else {
                    Msg::ToastRemovalNotConfirmed
                },
                &[
                    args[0],
                    args[1],
                    ("seconds", Arg::from(CATALOGUE_TIMEOUT_MS / 1_000)),
                ],
            ),
        )
    })
}

impl StudioShell {
    /// Opens the Add input dialog when the action is available.
    pub(crate) fn open_add_input(&mut self, state: &StudioUiState) {
        if add_input_availability(state, self.catalogue.adds_in_flight(state)).enabled {
            self.show_panel(crate::dock::PanelId::InputBank);
            self.add_input = Some(AddInputDraft::new(state, &self.catalogue));
        }
    }

    /// Opens the Remove confirmation for one input.
    pub(crate) fn open_remove_input(&mut self, input: InputId, name: &str) {
        self.remove_input = Some(RemoveConfirm {
            input,
            name: name.to_owned(),
        });
    }

    /// Add input availability for this frame.
    pub(crate) fn add_input_gate(&self, state: &StudioUiState) -> CatalogueAvailability {
        add_input_availability(state, self.catalogue.adds_in_flight(state))
    }

    /// Remove availability of one input for this frame.
    pub(crate) fn remove_input_gate(
        &self,
        state: &StudioUiState,
        input: InputId,
    ) -> CatalogueAvailability {
        remove_input_availability(state, input, self.catalogue.removal_in_flight(state, input))
    }

    /// Drops a Remove confirmation whose input is gone.
    pub(crate) fn reconcile_catalogue_dialogs(&mut self, state: &StudioUiState) {
        if let Some(confirm) = &self.remove_input
            && state
                .view
                .as_ref()
                .is_some_and(|view| !view.inputs.contains(&confirm.input))
        {
            self.remove_input = None;
        }
    }

    /// Draws the Add input dialog and the Remove confirmation when open.
    pub(crate) fn draw_catalogue_dialogs(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.draw_add_input_dialog(context, state, intents);
        self.draw_remove_input_dialog(context, state, intents);
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_add_input_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(mut draft) = self.add_input.take() else {
            return;
        };
        let next_id = state
            .view
            .as_ref()
            .and_then(|view| next_free_input_id(&self.catalogue.taken_ids(view, state)));
        let position = state.view.as_ref().map_or(0, |view| view.inputs.len())
            + self.catalogue.adds_in_flight(state)
            + 1;
        let taken: Vec<String> = self
            .catalogue
            .taken_names(state)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut submit = false;
        let mut cancel = false;
        let l = self.locale;
        let catalogue = &self.catalogue;
        let closed = anchored_modal(
            context,
            "studio-add-input-window",
            l.text(Msg::AddInput),
            460.0,
            |ui, body_height| {
                // The body scrolls on a short screen; the footer stays.
                egui::ScrollArea::vertical()
                    .id_salt("studio-add-input-body")
                    .max_height(body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(l.text(Msg::AddInputIntro))
                                .size(crate::theme::legible(11.0))
                                .color(text_dim_color()),
                        );
                        ui.add_space(8.0);
                        ui.label(section_label(l.text(Msg::AddInputKind)));
                        // Four kinds wrap rather than widen the dialog.
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                            for kind in [
                                NewInputKind::Solid,
                                NewInputKind::Bars,
                                NewInputKind::Network,
                                NewInputKind::Capture,
                            ] {
                                let selected = draft.kind == kind;
                                let response = ui
                                    .add(kind_key(l.text(kind.key_label()), selected))
                                    .on_hover_text(l.text(kind.description()));
                                if response.clicked() && !selected {
                                    let taken: Vec<&str> =
                                        taken.iter().map(String::as_str).collect();
                                    draft.set_kind(kind, &taken);
                                    // The capture form lists the server's devices.
                                    if kind == NewInputKind::Capture {
                                        intents.push(StudioIntent::RequestCaptureDevices);
                                    }
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(section_label(l.text(Msg::AddInputName)));
                        let edit = ui
                            .add(
                                egui::TextEdit::singleline(&mut draft.name)
                                    .id(egui::Id::new("studio-add-input-name"))
                                    .char_limit(MAX_INPUT_NAME_BYTES)
                                    .desired_width(ui.available_width()),
                            )
                            .on_hover_text(l.text(Msg::AddInputNameHover));
                        if draft.focus_pending {
                            draft.focus_pending = false;
                            edit.request_focus();
                        }
                        if edit.changed() {
                            draft.name_edited = true;
                        }
                        if edit.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            submit = true;
                        }
                        let taken_refs: Vec<&str> = taken.iter().map(String::as_str).collect();
                        let name_problem = input_name_problem(&draft.name, &taken_refs);
                        if name_problem.is_some() {
                            crate::network_stream_ui::mark_invalid(ui, edit.rect);
                        }
                        crate::network_stream_ui::hint_slot(
                            ui,
                            Some(match name_problem {
                                Some(problem) => crate::network_stream_ui::field_error(l, problem),
                                None if !draft.name_edited => {
                                    crate::theme::caption(l.text(Msg::NameSuggested))
                                }
                                None => crate::theme::caption(l.text(Msg::NameFree)),
                            }),
                        );
                        if draft.kind == NewInputKind::Network {
                            ui.add_space(6.0);
                            crate::network_stream_ui::draw_stream_form(ui, l, &mut draft.stream);
                        }
                        if draft.kind == NewInputKind::Capture {
                            ui.add_space(6.0);
                            let outcome = crate::capture_input_ui::draw_capture_form(
                                ui,
                                l,
                                state,
                                &mut draft.capture,
                            );
                            if outcome.rescan {
                                intents.push(StudioIntent::RequestCaptureDevices);
                            }
                            // The device name is the suggested input name.
                            if let Some(device) = outcome.picked
                                && !draft.name_edited
                            {
                                let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
                                if let Some(name) =
                                    crate::capture_input_ui::suggested_name(&device, &taken)
                                {
                                    draft.name = name;
                                }
                            }
                        }
                        ui.add_space(6.0);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            ui.vertical(|ui| {
                                ui.label(section_label(l.text(Msg::AddInputPreview)));
                                paint_source_preview(ui, &draft, next_id);
                                let position = ("position", Arg::from(position));
                                ui.label(
                                    mono(next_id.map_or_else(
                                        || l.format(Msg::AddInputNoIdPosition, &[position]),
                                        |input| {
                                            l.format(
                                                Msg::AddInputIdPosition,
                                                &[
                                                    position,
                                                    ("input", Arg::Str(&input.to_string())),
                                                ],
                                            )
                                        },
                                    ))
                                    .size(crate::theme::legible(10.5))
                                    .color(text_dim_color()),
                                );
                            });
                            if draft.kind == NewInputKind::Solid {
                                ui.vertical(|ui| {
                                    ui.label(section_label(l.text(Msg::AddInputColour)));
                                    ui.horizontal_wrapped(|ui| {
                                        ui.set_max_width(220.0);
                                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                                        for (label, color) in COLOUR_PRESETS {
                                            if colour_swatch(ui, color, draft.color == color)
                                                .on_hover_text(l.format(
                                                    Msg::ColourSwatchHover,
                                                    &[
                                                        ("colour", Arg::Str(l.text(label))),
                                                        ("hex", Arg::Str(&hex_colour(color))),
                                                    ],
                                                ))
                                                .clicked()
                                            {
                                                draft.color = color;
                                            }
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        egui::color_picker::color_edit_button_srgba(
                                            ui,
                                            &mut draft.color,
                                            egui::color_picker::Alpha::Opaque,
                                        )
                                        .on_hover_text(l.text(Msg::ColourPickHover));
                                        ui.label(
                                            mono(hex_colour(draft.color))
                                                .size(crate::theme::legible(11.5))
                                                .color(text_color()),
                                        );
                                    });
                                });
                            }
                        });
                    });
                // The rebuilt intent, so the footer agrees with this
                // frame's edits.
                let submission = add_input_intent(&draft, state, catalogue);
                ui.add_space(6.0);
                ui.separator();
                // The footer's message has its own wrapping row above the
                // keys, so a long reason never runs under them.
                ui.horizontal_top(|ui| {
                    if let Err(reason) = submission {
                        let names: Vec<&str> = taken.iter().map(String::as_str).collect();
                        let (short, full) = add_input_summary(l, &draft, &names, reason);
                        let (lamp, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        paint_lamp(ui.painter(), lamp.center(), 4.0, Tone::Pending);
                        ui.add(
                            egui::Label::new(
                                RichText::new(short)
                                    .size(crate::theme::legible(10.5))
                                    .color(AMBER),
                            )
                            .wrap(),
                        )
                        .on_hover_text(full);
                    } else {
                        ui.add(
                            egui::Label::new(crate::theme::caption(l.text(Msg::AddInputUndoNote)))
                                .wrap(),
                        );
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Add sits right of Cancel but is created after it,
                        // so Tab runs left to right: Cancel, then Add.
                        let add_slot = ui
                            .allocate_exact_size(Vec2::new(104.0, 26.0), Sense::hover())
                            .0;
                        if ui
                            .add(
                                Button::new(
                                    RichText::new(l.text(Msg::CancelKey))
                                        .size(12.0)
                                        .color(text_dim_color()),
                                )
                                .min_size(Vec2::new(84.0, 26.0)),
                            )
                            .on_hover_text(with_key(l, l.text(Msg::AddInputCancelHover), "Esc"))
                            .clicked()
                        {
                            cancel = true;
                        }
                        let add = ui
                            .add_enabled_ui(submission.is_ok(), |ui| {
                                ui.put(
                                    add_slot,
                                    Button::new(
                                        RichText::new(l.text(Msg::AddInputKey))
                                            .size(12.0)
                                            .strong()
                                            .color(text_color()),
                                    )
                                    .fill(ACCENT_FILL)
                                    .stroke(Stroke::new(1.0, accent_color()))
                                    .min_size(Vec2::new(104.0, 26.0)),
                                )
                            })
                            .inner
                            .hint(
                                &with_key(l, l.text(Msg::AddInputKeyHover), l.text(Msg::KeyEnter)),
                                submission.as_ref().err().copied().unwrap_or(""),
                            );
                        if add.clicked() {
                            submit = true;
                        }
                    });
                });
            },
        );
        // Rebuild after this frame's edits so Enter submits what was typed.
        let submission = add_input_intent(&draft, state, &self.catalogue);
        if submit && let Ok(intent) = submission {
            intents.push(intent);
            return;
        }
        if !(closed || cancel) {
            self.add_input = Some(draft);
        }
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_remove_input_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(confirm) = self.remove_input.clone() else {
            return;
        };
        let availability = self.remove_input_gate(state, confirm.input);
        let position = state.view.as_ref().and_then(|view| {
            view.inputs
                .iter()
                .position(|input| *input == confirm.input)
                .map(|index| index + 1)
        });
        let mut remove = false;
        let mut cancel = false;
        let l = self.locale;
        let input_text = confirm.input.to_string();
        let question = match position {
            Some(position) => l.format(
                Msg::RemoveInputQuestion,
                &[
                    ("name", Arg::Str(&confirm.name)),
                    ("input", Arg::Str(&input_text)),
                    ("position", Arg::from(position)),
                ],
            ),
            None => l.format(
                Msg::RemoveInputQuestionShort,
                &[
                    ("name", Arg::Str(&confirm.name)),
                    ("input", Arg::Str(&input_text)),
                ],
            ),
        };
        let closed = crate::menu::help_modal(
            context,
            "studio-remove-input-window",
            l.text(Msg::RemoveInput),
            420.0,
            |ui| {
                ui.add(
                    egui::Label::new(RichText::new(question).size(13.0).color(text_color())).wrap(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(l.text(Msg::RemoveInputWarning))
                        .size(crate::theme::legible(11.0))
                        .color(text_dim_color()),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if !availability.enabled {
                        let (lamp, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        paint_lamp(ui.painter(), lamp.center(), 4.0, Tone::Pending);
                        ui.add(
                            egui::Label::new(
                                RichText::new(l10n::unavailable(l, availability.reason))
                                    .size(crate::theme::legible(10.5))
                                    .color(AMBER),
                            )
                            .truncate(),
                        )
                        .on_hover_text(l10n::unavailable(l, availability.reason));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove sits right of Cancel but is created after
                        // it, so Tab reaches the safe Cancel key first; the
                        // dialog opens with Cancel focused, so a stray
                        // Enter or Space never removes the input.
                        let remove_slot = ui
                            .allocate_exact_size(Vec2::new(96.0, 26.0), Sense::hover())
                            .0;
                        let cancel_key = ui
                            .add(
                                Button::new(
                                    RichText::new(l.text(Msg::CancelKey))
                                        .size(12.0)
                                        .color(text_dim_color()),
                                )
                                .min_size(Vec2::new(84.0, 26.0)),
                            )
                            .on_hover_text(with_key(l, l.text(Msg::RemoveCancelHover), "Esc"));
                        if cancel_key.clicked() {
                            cancel = true;
                        }
                        if ui.ctx().memory(|memory| memory.focused().is_none()) {
                            cancel_key.request_focus();
                        }
                        if ui
                            .add_enabled_ui(availability.enabled, |ui| {
                                ui.put(
                                    remove_slot,
                                    Button::new(
                                        RichText::new(l.text(Msg::RemoveKey))
                                            .size(12.0)
                                            .strong()
                                            .color(text_color()),
                                    )
                                    .fill(PROGRAM_FILL)
                                    .stroke(Stroke::new(1.0, PROGRAM))
                                    .min_size(Vec2::new(96.0, 26.0)),
                                )
                            })
                            .inner
                            .hint(l.text(Msg::RemoveKeyHover), availability.reason)
                            .clicked()
                        {
                            remove = true;
                        }
                    });
                });
            },
        );
        if remove && availability.enabled {
            intents.push(StudioIntent::RemoveInput {
                input: confirm.input,
            });
            self.remove_input = None;
        } else if closed || cancel {
            self.remove_input = None;
        }
    }
}

pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A segmented kind key: lit accent outline and fill when selected.
pub(crate) fn kind_key(label: &str, selected: bool) -> Button<'static> {
    Button::new(
        RichText::new(label.to_owned())
            .size(crate::theme::legible(11.5))
            .strong()
            .color(if selected {
                text_color()
            } else {
                text_dim_color()
            }),
    )
    .fill(if selected {
        ACCENT_FILL
    } else {
        widget_color()
    })
    .stroke(Stroke::new(
        crate::theme::line_width(),
        if selected {
            accent_color()
        } else {
            border_strong_color()
        },
    ))
    .min_size(Vec2::new(150.0, 28.0))
}

/// One clickable colour preset; the current colour has an accent ring.
pub(crate) fn colour_swatch(ui: &mut Ui, color: Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(3), color);
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(
                if selected { 2.0 } else { 1.0 },
                if selected {
                    accent_color()
                } else if response.hovered() {
                    text_dim_color()
                } else {
                    border_strong_color()
                },
            ),
            StrokeKind::Outside,
        );
    }
    response
}

/// Standard 75 % colour bars, left to right: the eight-bar 75/0/75/0 signal
/// simulated `Bars` sources emit (75 % of 255, rounded, is 191).
///
/// Mirrors `BARS_75` in `fm-video`'s `test_signal` module; the UI layer may
/// not depend on media crates (`xtask deps check`), so keep the two in sync.
const BARS: [Color32; 8] = [
    Color32::from_rgb(191, 191, 191),
    Color32::from_rgb(191, 191, 0),
    Color32::from_rgb(0, 191, 191),
    Color32::from_rgb(0, 191, 0),
    Color32::from_rgb(191, 0, 191),
    Color32::from_rgb(191, 0, 0),
    Color32::from_rgb(0, 0, 191),
    Color32::from_rgb(0, 0, 0),
];

/// A 16:9 picture of the new source, tagged with its identifier.
fn paint_source_preview(ui: &mut Ui, draft: &AddInputDraft, next_id: Option<InputId>) {
    let l = l10n::locale(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(176.0, 99.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), MONITOR_BLACK);
    match draft.kind {
        NewInputKind::Solid => {
            painter.rect_filled(rect, CornerRadius::same(3), draft.color);
        }
        NewInputKind::Bars => {
            #[allow(clippy::cast_precision_loss)] // eight bars
            let width = rect.width() / BARS.len() as f32;
            for (index, color) in BARS.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let x = rect.min.x + index as f32 * width;
                painter.rect_filled(
                    Rect::from_min_max(pos2(x, rect.min.y), pos2(x + width + 0.5, rect.max.y)),
                    CornerRadius::ZERO,
                    *color,
                );
            }
        }
        // A stream shows nothing until it connects.
        // A capture input shows nothing until its device starts.
        NewInputKind::Network | NewInputKind::Capture => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                l.text(if draft.kind == NewInputKind::Capture {
                    Msg::StreamStarting
                } else {
                    Msg::ConnConnecting
                }),
                FontId::new(10.0, FontFamily::Proportional),
                muted_color(),
            );
        }
    }
    if let Some(input) = next_id {
        let tag = painter.layout_no_wrap(
            l.format(
                Msg::PreviewInputTag,
                &[("input", Arg::Str(&input.to_string()))],
            ),
            FontId::new(9.5, FontFamily::Monospace),
            text_color(),
        );
        let tag_rect = Rect::from_min_size(
            rect.min + Vec2::new(6.0, 6.0),
            tag.size() + Vec2::new(8.0, 4.0),
        );
        painter.rect_filled(
            tag_rect,
            CornerRadius::same(3),
            Color32::from_black_alpha(190),
        );
        painter.galley(tag_rect.min + Vec2::new(4.0, 2.0), tag, text_color());
    }
    painter.text(
        pos2(rect.max.x - 6.0, rect.max.y - 6.0),
        Align2::RIGHT_BOTTOM,
        l.text(Msg::PreviewSilent),
        FontId::new(9.0, FontFamily::Proportional),
        Color32::from_white_alpha(170),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(crate::theme::line_width(), border_strong_color()),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{EditHistoryStatus, InputUse};
    use fm_ui_model::{ClientModel, ProjectSnapshot};

    use super::*;
    use crate::StudioConnectionStatus;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn wire(value: u128) -> fm_protocol::WireInputId {
        fm_protocol::WireInputId::from_domain(input(value))
    }

    /// Inputs 1 (Program), 2 (Preview), 3, and 4 named `Cam`, `Slides`,
    /// `Solid Colour 1`, and `Guest`.
    fn snapshot() -> fm_protocol::SnapshotMessage {
        fm_protocol::SnapshotMessage {
            audio_mixer: fm_protocol::AudioMixerStatus::default(),
            fade_to_black_excluded_outputs: Vec::new(),
            fade_to_black_output_durations: Vec::new(),
            fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
            display_outputs: Vec::new(),
            display_output_states: Vec::new(),
            network_streams: Vec::new(),
            capture_inputs: Vec::new(),
            engine: fm_protocol::EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 9,
            show_name: "Show".into(),
            inputs: [
                (1, "Cam"),
                (2, "Slides"),
                (3, "Solid Colour 1"),
                (4, "Guest"),
            ]
            .map(|(id, name)| fm_protocol::InputStatus {
                input: wire(id),
                name: name.into(),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            })
            .to_vec(),
            outputs: Vec::new(),
            input_audio_strips: [1, 2, 3, 4]
                .map(|id| fm_protocol::InputAudioStripStatus {
                    pan_basis_points: 0,
                    group: None,
                    input: wire(id),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .to_vec(),
            desired_program: wire(1),
            desired_preview: wire(2),
            realized_program: wire(1),
            realized_preview: wire(2),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| fm_protocol::RecordingStatus {
                    recorder: fm_protocol::WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: fm_protocol::OverlayStatus::empty_channels(),
            realized_overlays: fm_protocol::OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            cue_list_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        }
    }

    fn view_of(snapshot: fm_protocol::SnapshotMessage) -> ClientView {
        let project = fm_types::ProjectId::new(NonZeroU128::new(1).unwrap());
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        model.view().unwrap()
    }

    fn editor(view: ClientView) -> StudioUiState {
        StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view)
            .with_edit_project_permission(true)
    }

    fn state() -> StudioUiState {
        editor(view_of(snapshot()))
    }

    fn add(id: u128, name: &str) -> CatalogueRequest {
        CatalogueRequest::Add {
            input: input(id),
            name: name.to_owned(),
        }
    }

    #[test]
    fn next_id_follows_the_largest_taken_identifier() {
        assert_eq!(next_free_input_id(&[]), Some(input(1)));
        assert_eq!(
            next_free_input_id(&[input(3), input(1), input(9)]),
            Some(input(10))
        );
        // The largest identifier is exhausted: the smallest gap is used.
        assert_eq!(
            next_free_input_id(&[input(u128::MAX), input(1), input(2)]),
            Some(input(3))
        );
    }

    #[test]
    fn bars_thumbnail_matches_simulated_75_percent_bars() {
        // White, yellow, cyan, green, magenta, red, blue at 75 %, then black,
        // as `fm-video`'s `BarsPattern::Bars75` renders them.
        let expected = [
            (191, 191, 191),
            (191, 191, 0),
            (0, 191, 191),
            (0, 191, 0),
            (191, 0, 191),
            (191, 0, 0),
            (0, 0, 191),
            (0, 0, 0),
        ];
        let actual: Vec<_> = BARS
            .iter()
            .map(|color| (color.r(), color.g(), color.b()))
            .collect();
        assert_eq!(actual, expected);
        assert!(BARS.iter().all(|color| color.a() == 255));
    }

    #[test]
    fn suggested_names_are_unique_per_kind() {
        assert_eq!(
            suggested_input_name(NewInputKind::Solid, &[]),
            "Solid Colour 1"
        );
        assert_eq!(
            suggested_input_name(NewInputKind::Solid, &["Solid Colour 1", "Solid Colour 3"]),
            "Solid Colour 2"
        );
        assert_eq!(
            suggested_input_name(NewInputKind::Bars, &["Solid Colour 1"]),
            "Colour Bars 1"
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            localized_input_name(spanish, NewInputKind::Solid, &["Color sólido 1"]),
            "Color sólido 2"
        );
        assert_eq!(
            localized_input_name(spanish, NewInputKind::Bars, &[]),
            "Barras de color 1"
        );
    }

    #[test]
    fn name_problems_mirror_the_server_rules() {
        let taken = ["Cam", "Slides"];
        assert_eq!(input_name_problem("Bars 1", &taken), None);
        assert_eq!(input_name_problem("   ", &taken), Some("the name is empty"));
        assert_eq!(
            input_name_problem(" Cam ", &taken),
            Some("another input already uses this name")
        );
        assert_eq!(
            input_name_problem(&"x".repeat(MAX_INPUT_NAME_BYTES + 1), &taken),
            Some("the name is longer than 128 bytes")
        );
        assert_eq!(
            input_name_problem(&"x".repeat(MAX_INPUT_NAME_BYTES), &taken),
            None
        );
    }

    #[test]
    fn add_is_gated_by_session_padlock_and_limit() {
        assert_eq!(MAX_LIVE_INPUTS, 256, "INPUT_LIMIT_REASON names 256");
        assert!(add_input_availability(&state(), 0).enabled);

        let mut viewer = state();
        viewer.can_edit_project = false;
        let denied = add_input_availability(&viewer, 0);
        assert!(!denied.enabled);
        assert_eq!(
            denied.reason,
            "this session lacks the edit_project permission"
        );

        let offline = StudioUiState::new(StudioConnectionStatus::Backoff);
        assert_eq!(
            add_input_availability(&offline, 0).reason,
            "the control connection is not ready yet"
        );

        let mut locked = snapshot();
        locked.edit_locks.project = true;
        let locked = add_input_availability(&editor(view_of(locked)), 0);
        assert_eq!(locked.reason, PROJECT_LOCKED_REASON);
        assert!(locked.locked());

        // Four inputs plus adds in flight reach the limit.
        let full = add_input_availability(&state(), MAX_LIVE_INPUTS - 4);
        assert_eq!(full.reason, INPUT_LIMIT_REASON);
        assert!(add_input_availability(&state(), MAX_LIVE_INPUTS - 5).enabled);
    }

    #[test]
    fn removal_is_blocked_by_live_references_and_padlocks() {
        let state = state();
        let gate = |state: &StudioUiState, id| remove_input_availability(state, input(id), false);
        assert_eq!(gate(&state, 1).reason, ON_PROGRAM_REASON);
        assert_eq!(gate(&state, 1).mark, Some("PGM"));
        assert_eq!(gate(&state, 2).reason, ON_PREVIEW_REASON);
        assert_eq!(gate(&state, 2).mark, Some("PVW"));
        assert!(gate(&state, 3).enabled);
        assert_eq!(gate(&state, 9).reason, UNKNOWN_INPUT_REASON);
        assert_eq!(
            remove_input_availability(&state, input(3), true).reason,
            REMOVAL_PENDING_REASON
        );

        let mut locked = snapshot();
        locked.edit_locks.inputs = vec![wire(3)];
        let locked = editor(view_of(locked));
        assert_eq!(gate(&locked, 3).reason, INPUT_LOCKED_REASON);
        assert!(gate(&locked, 3).locked());
        assert!(gate(&locked, 4).enabled);

        let mut project = snapshot();
        project.edit_locks.project = true;
        assert_eq!(
            gate(&editor(view_of(project)), 3).reason,
            PROJECT_LOCKED_REASON
        );

        // Realized Program still on the old source blocks it too.
        let mut moving = snapshot();
        moving.desired_program = wire(4);
        moving.desired_preview = wire(1);
        let moving = editor(view_of(moving));
        assert_eq!(gate(&moving, 1).reason, ON_PROGRAM_REASON);
        assert_eq!(gate(&moving, 4).reason, ON_PROGRAM_REASON);
        assert_eq!(gate(&moving, 2).reason, ON_PREVIEW_REASON);

        let mut viewer = state.clone();
        viewer.can_edit_project = false;
        assert_eq!(
            gate(&viewer, 3).reason,
            "this session lacks the edit_project permission"
        );
    }

    #[test]
    fn overlays_and_stingers_keep_their_inputs() {
        let mut overlay = snapshot();
        overlay.desired_overlays[0].source = Some(wire(3));
        overlay.desired_overlays[0].active = true;
        overlay.desired_overlays[1].queued_sources = vec![wire(4)];
        let overlay = editor(view_of(overlay));
        assert_eq!(
            remove_input_availability(&overlay, input(3), false).reason,
            OVERLAY_ON_AIR_REASON
        );
        assert_eq!(
            remove_input_availability(&overlay, input(4), false).reason,
            OVERLAY_QUEUED_REASON
        );
        assert_eq!(
            remove_input_availability(&overlay, input(4), false).mark,
            Some("IN USE")
        );

        let mut stinger = view_of(snapshot());
        stinger.stingers.push(fm_ui_model::StingerStatus {
            slot: 1,
            media_input: input(4),
            preload: true,
            cut_point_frames: 15,
            audio_policy: fm_protocol::StingerAudioPolicy::MixWithProgram,
            missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Cut,
            readiness: fm_protocol::StingerReadiness::Ready,
        });
        assert_eq!(
            input_usage_reason(&stinger, input(4)),
            Some(STINGER_MEDIA_REASON)
        );
        assert_eq!(input_usage_reason(&stinger, input(3)), None);
    }

    #[test]
    fn add_intent_uses_the_next_free_id_and_a_trimmed_unique_name() {
        let state = state();
        let mut tracker = CatalogueTracker::default();
        let mut draft = AddInputDraft::new(&state, &tracker);
        // `Solid Colour 1` is taken by input 3.
        assert_eq!(draft.name, "Solid Colour 2");
        draft.color = Color32::from_rgb(1, 2, 3);
        assert_eq!(
            add_input_intent(&draft, &state, &tracker),
            Ok(StudioIntent::AddInput {
                input: input(5),
                name: "Solid Colour 2".to_owned(),
                source: LiveInputSource::Solid {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
            })
        );
        // An add in flight reserves its id and name.
        tracker.submit(add(5, "Solid Colour 2"), "Solid Colour 2".to_owned());
        let next = AddInputDraft::new(&state, &tracker);
        assert_eq!(next.name, "Solid Colour 3");
        assert!(matches!(
            add_input_intent(&next, &state, &tracker),
            Ok(StudioIntent::AddInput { input: id, .. }) if id == input(6)
        ));
        assert_eq!(
            add_input_intent(&draft, &state, &tracker),
            Err("another input already uses this name")
        );

        draft.set_kind(NewInputKind::Bars, &tracker.taken_names(&state));
        assert_eq!(draft.name, "Colour Bars 1");
        assert!(matches!(
            add_input_intent(&draft, &state, &tracker),
            Ok(StudioIntent::AddInput {
                source: LiveInputSource::Bars,
                ..
            })
        ));
        // A typed name survives a kind change.
        draft.name = "  Test Card ".to_owned();
        draft.name_edited = true;
        draft.set_kind(NewInputKind::Solid, &[]);
        assert!(matches!(
            add_input_intent(&draft, &state, &tracker),
            Ok(StudioIntent::AddInput { name, .. }) if name == "Test Card"
        ));

        let mut viewer = state;
        viewer.can_edit_project = false;
        assert_eq!(
            add_input_intent(&draft, &viewer, &tracker),
            Err("this session lacks the edit_project permission")
        );
    }

    #[test]
    fn tracked_adds_resolve_to_success_when_the_input_appears() {
        let mut state = state();
        let mut tracker = CatalogueTracker::default();
        let pending = tracker.submit_intents(
            &[
                StudioIntent::Cut,
                StudioIntent::AddInput {
                    input: input(5),
                    name: "Colour Bars 1".to_owned(),
                    source: LiveInputSource::Bars,
                },
            ],
            state.view.as_ref(),
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, ToastKind::Pending);
        assert_eq!(pending[0].message, "Adding \"Colour Bars 1\" as input 5…");

        // Sent and awaiting its result: nothing resolves.
        state.catalogue_pending = vec![add(5, "Colour Bars 1")];
        assert_eq!(tracker.observe(&state, 16), CatalogueOutcomes::default());
        assert_eq!(tracker.adds_in_flight(&state), 1);

        // Accepted and applied: the input is in the replicated catalogue.
        let mut added = snapshot();
        added.inputs.push(fm_protocol::InputStatus {
            input: wire(5),
            name: "Colour Bars 1".into(),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        });
        let mut strip = added.input_audio_strips[0];
        strip.input = wire(5);
        added.input_audio_strips.push(strip);
        state.view = Some(view_of(added));
        state.catalogue_pending.clear();
        let outcomes = tracker.observe(&state, 16);
        assert_eq!(
            outcomes.toasts,
            [CatalogueToast {
                key: pending[0].key.clone(),
                kind: ToastKind::Success,
                message: "Added \"Colour Bars 1\" as input 5 at position 5. Undo removes it again."
                    .to_owned(),
                detail: None,
            }]
        );
        assert!(!outcomes.refusal_announced);
        assert_eq!(tracker.adds_in_flight(&state), 0);
    }

    #[test]
    fn tracked_removals_map_the_in_use_refusal_into_the_toast() {
        let mut state = state();
        let mut tracker = CatalogueTracker::default();
        let pending = tracker.submit_intents(
            &[StudioIntent::RemoveInput { input: input(3) }],
            state.view.as_ref(),
        );
        assert_eq!(pending[0].message, "Removing \"Solid Colour 1\" (input 3)…");
        assert!(tracker.removal_in_flight(&state, input(3)));
        state.catalogue_refusal = Some(CatalogueRefusal {
            command_id: "c-1".to_owned(),
            request: CatalogueRequest::Remove { input: input(3) },
            code: "conflict".to_owned(),
            reason: Some("input_in_use".to_owned()),
            message: "input 3 cannot be removed: it is the source of overlay 2".to_owned(),
            typed_reason: Some(RefusalReason::InputInUse {
                input: 3,
                usage: InputUse::Overlay { channel: 2 },
            }),
        });
        let outcomes = tracker.observe(&state, 16);
        assert!(outcomes.refusal_announced);
        assert_eq!(outcomes.toasts.len(), 1);
        assert_eq!(outcomes.toasts[0].kind, ToastKind::Error);
        assert_eq!(
            outcomes.toasts[0].message,
            "Remove refused: \"Solid Colour 1\" is the source of overlay 2. \
             Take the overlay off or clear its queue first."
        );
        // The stable code moves to the toast's hover detail.
        assert_eq!(
            outcomes.toasts[0].detail.as_deref(),
            Some("Reason code: input_in_use")
        );
        assert!(!tracker.removal_in_flight(&state, input(3)));
        // The same refusal is announced once.
        assert_eq!(tracker.observe(&state, 16), CatalogueOutcomes::default());
    }

    #[test]
    fn tracked_removals_resolve_when_the_input_leaves() {
        let mut state = state();
        let mut tracker = CatalogueTracker::default();
        tracker.submit(
            CatalogueRequest::Remove { input: input(4) },
            "Guest".to_owned(),
        );
        state.catalogue_pending = vec![CatalogueRequest::Remove { input: input(4) }];
        assert_eq!(tracker.observe(&state, 16).toasts, Vec::new());
        let mut removed = snapshot();
        removed.inputs.pop();
        removed.input_audio_strips.pop();
        state.view = Some(view_of(removed));
        state.catalogue_pending.clear();
        let outcomes = tracker.observe(&state, 16);
        assert_eq!(outcomes.toasts[0].kind, ToastKind::Success);
        assert_eq!(
            outcomes.toasts[0].message,
            "Removed \"Guest\" (input 4) with its mixer strip."
        );
    }

    #[test]
    fn unresolved_requests_fail_after_leaving_pending_or_timing_out() {
        let mut state = state();
        let mut tracker = CatalogueTracker::default();
        tracker.submit(add(5, "Bars"), "Bars".to_owned());
        state.catalogue_pending = vec![add(5, "Bars")];
        tracker.observe(&state, 16);
        // Left the pending set without effect and the refusal was missed.
        state.catalogue_pending.clear();
        state.error = Some("Command rejected (conflict): busy".to_owned());
        let outcomes = tracker.observe(&state, 16);
        assert_eq!(outcomes.toasts[0].kind, ToastKind::Error);
        assert_eq!(
            outcomes.toasts[0].message,
            "Add of \"Bars\" was not applied: Command rejected (conflict): busy"
        );

        // Never seen pending: waits, then times out.
        state.error = None;
        tracker.submit(
            CatalogueRequest::Remove { input: input(3) },
            "Solid".to_owned(),
        );
        assert_eq!(
            tracker.observe(&state, CATALOGUE_TIMEOUT_MS - 1).toasts,
            Vec::new()
        );
        // No timeout progress while the console is not ready.
        state.connection_status = StudioConnectionStatus::Backoff;
        assert_eq!(tracker.observe(&state, 10).toasts, Vec::new());
        state.connection_status = StudioConnectionStatus::Ready;
        let outcomes = tracker.observe(&state, 1);
        assert!(outcomes.toasts[0].message.starts_with(
            "Removal of \"Solid\" was not confirmed after 15 s: no result from the daemon"
        ));
    }

    #[test]
    fn untracked_refusals_are_still_worded() {
        let mut state = state();
        let mut tracker = CatalogueTracker::default();
        state.catalogue_refusal = Some(CatalogueRefusal {
            command_id: "c-9".to_owned(),
            request: add(5, "Cam"),
            code: "conflict".to_owned(),
            reason: Some("duplicate_name".to_owned()),
            message: "input name is already in use".to_owned(),
            typed_reason: None,
        });
        let outcomes = tracker.observe(&state, 16);
        assert!(outcomes.refusal_announced);
        assert_eq!(
            outcomes.toasts[0].message,
            "Add refused: Another input is already named \"Cam\"; pick another."
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one table of refusal cases per language
    fn refusal_messages_name_the_reason_code_and_next_step() {
        let refusal = |request, reason: Option<&str>, code: &str, message: &str| CatalogueRefusal {
            command_id: "c".to_owned(),
            request,
            code: code.to_owned(),
            reason: reason.map(str::to_owned),
            message: message.to_owned(),
            typed_reason: None,
        };
        // A typed refusal of removing input 2; its English text is never shown.
        let typed = |reason: RefusalReason| CatalogueRefusal {
            command_id: "c".to_owned(),
            request: CatalogueRequest::Remove { input: input(2) },
            code: "conflict".to_owned(),
            reason: None,
            message: reason.to_string(),
            typed_reason: Some(reason),
        };
        let remove = || CatalogueRequest::Remove { input: input(2) };
        let cases = [
            (
                refusal(add(7, "X"), Some("duplicate_input"), "conflict", "m"),
                "Add refused: Input 7 already exists; open Add input again for a fresh id.",
            ),
            (
                refusal(add(7, "X"), Some("too_many_inputs"), "conflict", "m"),
                "Add refused: The show is at its 256-input limit; remove an input first.",
            ),
            (
                refusal(remove(), Some("unknown_input"), "not_found", "m"),
                "Remove refused: \"X\" no longer exists; another editor removed it.",
            ),
            (
                refusal(add(7, "X"), Some("invalid_name"), "invalid_command", "m"),
                "Add refused: Use a non-blank name of at most 128 bytes.",
            ),
            (
                refusal(add(7, "X"), Some("project_locked"), "conflict", "m"),
                "Add refused: The project is locked; unlock the padlock in the top bar to change inputs.",
            ),
            (
                refusal(remove(), Some("input_locked"), "conflict", "m"),
                "Remove refused: \"X\" has its own padlock; unlock it to remove it.",
            ),
            (
                typed(RefusalReason::InputInUse {
                    input: 2,
                    usage: InputUse::Preview,
                }),
                "Remove refused: \"X\" is on Preview. Route another input to that bus first.",
            ),
            (
                typed(RefusalReason::InputInUse {
                    input: 2,
                    usage: InputUse::SceneLayer { scene: 100 },
                }),
                "Remove refused: \"X\" is a layer source of scene 100. Remove that layer from the scene first.",
            ),
            (
                typed(RefusalReason::ProjectLocked),
                "Remove refused: The project is locked; unlock the padlock in the top bar to change inputs.",
            ),
            (
                typed(RefusalReason::RuntimeUnsupported),
                "Remove refused: This daemon cannot change its inputs while running (native media); add or remove inputs offline.",
            ),
            (
                // A refusal without a typed reason or a known code keeps the
                // daemon's text.
                refusal(
                    remove(),
                    None,
                    "conflict",
                    "input 2 cannot be removed: it is a layer source of scene 100",
                ),
                "Remove refused: input 2 cannot be removed: it is a layer source of scene 100",
            ),
            (
                refusal(remove(), None, "unavailable", "native"),
                "Remove refused: This daemon cannot change its inputs while running (native media); add or remove inputs offline.",
            ),
            (
                refusal(remove(), None, "stale_revision", "revision moved"),
                "Remove refused: revision moved",
            ),
        ];
        for (refusal, expected) in cases {
            assert_eq!(
                catalogue_refusal_message(Locale::ENGLISH, &refusal, "X"),
                expected
            );
        }
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let stinger = typed(RefusalReason::InputInUse {
            input: 2,
            usage: InputUse::Stinger { slot: 3 },
        });
        assert_eq!(
            catalogue_refusal_message(spanish, &stinger, "X"),
            "Baja rechazada: «X» es el medio del Stinger 3. \
             Vacíe o reasigne esa ranura de Stinger primero."
        );
        // The review's screenshot: a Spanish console no longer shows the
        // daemon's English "a layer source of scene 100".
        let scene = typed(RefusalReason::InputInUse {
            input: 2,
            usage: InputUse::SceneLayer { scene: 100 },
        });
        assert_eq!(
            catalogue_refusal_message(spanish, &scene, "Camera 1 Wide"),
            "Baja rechazada: «Camera 1 Wide» es una fuente de capa de la \
             escena 100. Quite esa capa de la escena primero."
        );
        let playlist = typed(RefusalReason::InputInUse {
            input: 2,
            usage: InputUse::PlaylistEntry { entry: 4 },
        });
        assert_eq!(
            catalogue_refusal_message(spanish, &playlist, "X"),
            "Baja rechazada: «X» se reproduce en la entrada 4 de la lista. \
             Quítela de la lista primero."
        );
        let typed_full = CatalogueRefusal {
            request: add(5, "Cam"),
            ..typed(RefusalReason::bounds("inputs", 256))
        };
        assert_eq!(
            catalogue_refusal_message(spanish, &typed_full, "Cam"),
            "Alta rechazada: El show ya tiene el máximo de 256 entradas; \
             quite una entrada primero."
        );
        let scheme = refusal(
            add(5, "Cam"),
            Some("unsupported_scheme"),
            "invalid_command",
            "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls, rtmp, rtmps, srt)",
        );
        assert_eq!(
            catalogue_refusal_message(spanish, &scheme, "Cam"),
            "Alta rechazada: esquema no admitido (admitidos: rtsp, rtp, \
             udp, tcp, http, https, hls, rtmp, rtmps, srt)"
        );
        let full = refusal(add(5, "Cam"), None, "too_many_inputs", "m");
        assert!(
            catalogue_refusal_message(spanish, &full, "X")
                .starts_with("Alta rechazada: El show ya tiene el máximo de 256")
        );
        assert_eq!(
            crate::monitors::fallback_input_name(spanish, input(9)),
            "Entrada 9"
        );
    }

    #[test]
    fn solid_sources_are_opaque_and_hex_is_lowercase() {
        let state = state();
        let mut draft = AddInputDraft::new(&state, &CatalogueTracker::default());
        draft.color = Color32::from_rgb(0xAB, 0x0C, 0xFF);
        assert_eq!(
            draft.source().unwrap(),
            LiveInputSource::Solid {
                red: 0xab,
                green: 0x0c,
                blue: 0xff,
                alpha: 255,
            }
        );
        assert_eq!(hex_colour(draft.color), "#ab0cff");
        assert_eq!(capitalized("the name is empty"), "The name is empty");
    }

    #[test]
    fn capture_drafts_add_the_chosen_device_or_say_why_not() {
        let state = state();
        let tracker = CatalogueTracker::default();
        let mut draft = AddInputDraft::new(&state, &tracker);
        let taken = tracker.taken_names(&state);
        draft.set_kind(NewInputKind::Capture, &taken);
        assert_eq!(draft.name, "Camera 1");
        assert_eq!(draft.source(), Err(Msg::CapReasonNoDevice.en()));
        assert_eq!(
            add_input_intent(&draft, &state, &tracker),
            Err(Msg::CapReasonNoDevice.en())
        );
        draft.capture.device = Some("v4l2:usb-1:C920".into());
        let Ok(StudioIntent::AddInput { source, .. }) = add_input_intent(&draft, &state, &tracker)
        else {
            panic!("a chosen device adds a capture input");
        };
        assert_eq!(
            source,
            LiveInputSource::Capture(Box::new(fm_protocol::CaptureInputConfig::new(
                "v4l2:usb-1:C920"
            )))
        );
    }

    /// The Add input dialog drafts the layout test draws: every kind, the
    /// stream form's simple and Advanced views with problems in a hidden
    /// and a shown field, and the capture form with its filters and a bad
    /// ALSA name.
    fn dialog_drafts(state: &StudioUiState) -> Vec<(&'static str, AddInputDraft)> {
        use crate::capture_input_ui::AudioChoice;
        use crate::network_stream_ui::StreamAdvanced;
        let mut shell = StudioShell::default();
        shell.open_add_input(state);
        let base = shell.add_input.clone().unwrap();
        let with = |kind, configure: &dyn Fn(&mut AddInputDraft)| {
            let mut draft = base.clone();
            draft.kind = kind;
            draft.focus_pending = false;
            configure(&mut draft);
            draft
        };
        let all_open = StreamAdvanced {
            source: true,
            connection: true,
            recovery: true,
        };
        let webcam = crate::hover_audit::capture_inventory().devices[0].clone();
        vec![
            ("solid", with(NewInputKind::Solid, &|_| {})),
            ("network-empty", with(NewInputKind::Network, &|_| {})),
            (
                "network-rtsp-backoff",
                with(NewInputKind::Network, &|draft| {
                    draft.stream.url = "rtsp://operator:s3cret@10.0.0.42:554/stream1".into();
                    draft.stream.settings.transport = fm_protocol::RtspTransport::Tcp;
                    draft.stream.settings.reconnect.backoff_min_ms = 500;
                    draft.stream.settings.reconnect.backoff_max_ms = 400;
                }),
            ),
            (
                "network-rtmp-key",
                with(NewInputKind::Network, &|draft| {
                    draft.stream.url = "rtmp://0.0.0.0:1935/live".into();
                    draft.stream.settings.mode = fm_protocol::StreamMode::Listener;
                    draft.stream.stream_key = "obs key".into();
                }),
            ),
            (
                "network-srt-advanced",
                with(NewInputKind::Network, &|draft| {
                    draft.stream.url = "srt://0.0.0.0:9000".into();
                    draft.stream.settings.mode = fm_protocol::StreamMode::Listener;
                    draft.stream.passphrase = "correct-horse-battery".into();
                    draft.stream.reveal.passphrase = true;
                    draft.stream.stream_id = "cam1".into();
                    draft.stream.advanced = all_open;
                }),
            ),
            ("capture-none", with(NewInputKind::Capture, &|_| {})),
            (
                "capture-advanced",
                with(NewInputKind::Capture, &|draft| {
                    draft.capture.select_device(&webcam);
                    draft.capture.advanced = true;
                    draft.capture.format = Some(webcam.formats[0]);
                    draft.capture.audio = AudioChoice::Alsa;
                    draft.capture.alsa_device = "bad name".into();
                }),
            ),
        ]
    }

    /// Every text a pass painted, each with its rectangle cut to its clip
    /// rectangle.
    fn painted_texts(output: &egui::FullOutput) -> Vec<(String, Rect)> {
        fn collect(shape: &egui::Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(text) => {
                    let rect = text
                        .galley
                        .rect
                        .translate(text.pos.to_vec2())
                        .intersect(clip);
                    if rect.is_positive() && !text.galley.text().trim().is_empty() {
                        out.push((text.galley.text().to_owned(), rect));
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, clip, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, clipped.clip_rect, &mut out);
        }
        out
    }

    fn draw_dialog(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
        size: Vec2,
    ) -> egui::FullOutput {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-add-input-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let _ = shell.draw(&mut ui, state);
        drop(ui);
        context.end_pass()
    }

    /// At 1280x720 and 1920x1080, in English and Spanish, the Add input
    /// dialog keeps its top edge wherever its contents change, fits the
    /// screen, paints no text over other text, and keeps the footer's
    /// message on its own row above the keys. With
    /// `FM_ADD_INPUT_RENDER_DIR` set, each draft is also written there as a
    /// PNG for review.
    #[test]
    fn the_add_input_dialog_is_anchored_and_nothing_overlaps() {
        use crate::offscreen::{Canvas, Textures};
        use fm_ui_model::i18n::Language;
        let render_dir = std::env::var("FM_ADD_INPUT_RENDER_DIR").ok();
        let mut state = crate::hover_audit::bank_fixture();
        if let Some(view) = state.view.as_mut() {
            view.capture_inventory = Some(crate::hover_audit::capture_inventory());
        }
        for size in [Vec2::new(1_280.0, 720.0), Vec2::new(1_920.0, 1_080.0)] {
            for language in [Language::English, Language::Spanish] {
                let locale = Locale::new(language);
                // The console behind the dialog, to tell its text apart.
                let console = {
                    let context = egui::Context::default();
                    let mut shell = StudioShell::default();
                    shell.set_locale(locale);
                    let mut output = None;
                    for _ in 0..3 {
                        output = Some(draw_dialog(&context, &mut shell, &state, size));
                    }
                    painted_texts(&output.unwrap())
                };
                for (name, draft) in dialog_drafts(&state) {
                    let context = egui::Context::default();
                    let mut textures = Textures::default();
                    let mut shell = StudioShell::default();
                    shell.set_locale(locale);
                    // The console is up before the dialog opens over it, as
                    // in the app, so the dialog's layer is on top.
                    textures.apply(&draw_dialog(&context, &mut shell, &state, size));
                    shell.add_input = Some(AddInputDraft { locale, ..draft });
                    let mut output = None;
                    // Enough passes for the dialog to finish fading in.
                    for _ in 0..6 {
                        let full = draw_dialog(&context, &mut shell, &state, size);
                        textures.apply(&full);
                        output = Some(full);
                    }
                    let output = output.unwrap();
                    let scene = format!("{name} {language:?} {}x{}", size.x, size.y);
                    let dialog = context
                        .memory(|memory| memory.area_rect(egui::Id::new("studio-add-input-window")))
                        .expect("the dialog is open");
                    assert!(
                        (dialog.top() - dialog_top(size.y)).abs() < 0.5,
                        "{scene}: the dialog's top is {}",
                        dialog.top()
                    );
                    assert!(
                        dialog.bottom() <= size.y && dialog.left() >= 0.0,
                        "{scene}: the dialog {dialog:?} leaves the screen"
                    );
                    let mut texts = painted_texts(&output);
                    texts.retain(|text| dialog.contains_rect(text.1) && !console.contains(text));
                    for (index, (a, a_rect)) in texts.iter().enumerate() {
                        for (b, b_rect) in &texts[index + 1..] {
                            let overlap = a_rect.intersect(*b_rect);
                            assert!(
                                !(overlap.width() > 1.0 && overlap.height() > 1.0),
                                "{scene}: {a:?} {a_rect:?} overlaps {b:?} {b_rect:?}"
                            );
                        }
                    }
                    let keys = texts
                        .iter()
                        .filter(|(text, _)| {
                            text == locale.text(Msg::CancelKey)
                                || text == locale.text(Msg::AddInputKey)
                        })
                        .map(|(_, rect)| *rect)
                        .collect::<Vec<_>>();
                    assert_eq!(keys.len(), 2, "{scene}: both footer keys are painted");
                    let keys_top = keys.iter().map(Rect::top).fold(f32::MAX, f32::min);
                    for (text, rect) in &texts {
                        if rect.top() < keys_top && rect.bottom() > keys_top - 30.0 {
                            assert!(
                                rect.bottom() <= keys_top - 4.0,
                                "{scene}: footer text {text:?} {rect:?} runs into the keys"
                            );
                        }
                    }
                    if let Some(dir) = &render_dir {
                        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let mut canvas = Canvas::new(
                            size.x as usize,
                            size.y as usize,
                            crate::theme::graphite_color(),
                        );
                        canvas.paint(&primitives, &textures, output.pixels_per_point);
                        let (width, height, rgba) =
                            canvas.crop_rgba8(Rect::from_min_size(egui::Pos2::ZERO, size));
                        std::fs::write(
                            format!("{dir}/add-input-{name}-{language:?}-{width}x{height}.png"),
                            crate::offscreen::encode_png(width, height, &rgba),
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
}
