//! The Studio Colour window (SW-019): the colour grading panel bound to the
//! replicated grade of one input or Program.
//!
//! The window keeps one [`GradeBinding`]: authority updates reset its draft
//! while it is clean, and edits leave as [`StudioIntent::Grade`] commands,
//! throttled while a control is dragged and sent at once on release. The
//! preview grades a picture of the target locally so drags are instant;
//! the monitors show what the server applied. Only an *ungraded* picture
//! is a valid preview source (grading an already graded frame would apply
//! the grade twice), so the window borrows a monitor tap frame only while
//! the target shows there without a grade on air, keeps the last such frame
//! per target, and otherwise previews a test still.
//!
//! A scene layer's grade applies after its source's input grade, so its
//! clean picture is its source as a monitor shows it: the layer's input on
//! Preview (or on Program while Program is ungraded), with that input's
//! grade; for a layer drawing a scene directly, a Scene input showing that
//! scene without a grade of its own. The window previews that live picture
//! while it is on a monitor and the last one it saw otherwise (a test still
//! until one arrives); it never requests an exact scene preview for it.
//!
//! The window also saves the draft to the project's grade library, applies
//! library grades (linked by reference), and imports `.cube` LUTs. Studio
//! reads LUT files in the host (this crate does no I/O): the window queues a
//! [`GradeLutRequest`] and the host answers with a [`GradeLutFile`].

use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use egui::{ComboBox, Frame, Key, Margin, Modifiers, RichText, ScrollArea, Stroke, TextEdit, Ui};
use fm_protocol::{MonitorImage, MonitorTap, WireInputId, WireSceneLayerSource};
use fm_ui_model::GradingStatus;
use fm_ui_model::grade_editor::{
    GradeBinding, GradeCommand, GradeNameProblem, GradeSyncStatus, GradeTarget, RgbaImage,
    library_name_problem, lut_id, lut_name_problem, lut_uri, next_library_id, next_lut_id,
    sample_still, target_grade,
};

use crate::color_panel::{ColorPanelState, ColorPanelWidget, PREVIEW_MAX};
use crate::dock::ToolWindow;
use crate::edit_history::{INPUT_LOCKED_REASON, PROJECT_LOCKED_REASON};
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::MONITOR_STALE_AFTER;
use crate::preset_files::{ExportLut, PendingLutReplace, PresetFileRequest};
use crate::theme::{
    AMBER, Tone, border_strong_color, caption, close_key, muted_color, raised_color, section_label,
    small_button, status_chip,
};
use crate::toasts::ToastKind;
use crate::{StudioIntent, StudioShell, StudioUiState, workspace};

/// The shortcut that opens and closes the Colour window.
pub const COLOR_SHORTCUT: &str = "Ctrl+G";
/// Shortest gap between two live preview frames while idle.
const SOURCE_REFRESH: Duration = Duration::from_millis(400);
/// Most targets whose last ungraded frame is kept.
const HELD_SOURCES: usize = 16;

/// The refusal of one grade command, published once with its rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradeRefusal {
    pub command_id: String,
    /// The target a refused grade set addressed; `None` for library saves
    /// and LUT imports.
    pub target: Option<GradeTarget>,
}

/// A `.cube` file the operator asked to import; the host reads it and
/// answers with [`StudioShell::receive_grade_lut_file`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradeLutRequest {
    pub path: PathBuf,
    pub lut: NonZeroU32,
    pub name: String,
}

/// What the host read for a [`GradeLutRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeLutFile {
    /// The whole file, at most the protocol limit.
    Text(String),
    /// The file is larger than one command carries.
    TooLarge { bytes: u64 },
    /// The file is not UTF-8 text.
    NotText,
    /// The file could not be read.
    Unreadable(String),
}

/// Where the preview's picture came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceKind {
    Live(MonitorTap),
    Held,
    Still,
}

/// The Colour window's state, next to the console.
pub(crate) struct ColorWindow {
    pub(crate) open: bool,
    target: GradeTarget,
    binding: Option<GradeBinding>,
    pub(crate) panel: ColorPanelState,
    pub(crate) library_pick: Option<NonZeroU32>,
    pub(crate) library_name: String,
    pub(crate) lut_path: String,
    lut_requests: Vec<GradeLutRequest>,
    /// The last LUT id requested this session; the next import goes past it.
    pub(crate) lut_issued: Option<NonZeroU32>,
    /// The `.fmgrade` path export and import use.
    pub(crate) grade_path: String,
    /// `.fmgrade` files to export or import, for the host.
    pub(crate) grade_file_requests: Vec<PresetFileRequest>,
    /// A grade export waiting to replace or keep an existing sibling LUT.
    pub(crate) lut_replace: Option<PendingLutReplace>,
    /// The picture shown and the frame it came from.
    source: Option<(GradeTarget, SourceKind)>,
    source_frame: Option<(MonitorTap, u64)>,
    source_at: Option<Instant>,
    held: BTreeMap<GradeTarget, RgbaImage>,
    /// The last grade refusal handled.
    refusal: Option<String>,
    /// The targets ticked in `Copy to…`.
    pub(crate) copy_ticked: BTreeSet<GradeTarget>,
}

impl Default for ColorWindow {
    fn default() -> Self {
        Self {
            open: false,
            target: GradeTarget::Program,
            binding: None,
            panel: ColorPanelState::default(),
            library_pick: None,
            library_name: String::new(),
            lut_path: String::new(),
            lut_requests: Vec::new(),
            lut_issued: None,
            grade_path: String::new(),
            grade_file_requests: Vec::new(),
            lut_replace: None,
            source: None,
            source_frame: None,
            source_at: None,
            held: BTreeMap::new(),
            refusal: None,
            copy_ticked: BTreeSet::new(),
        }
    }
}

/// Clones what the console compares (open and target); previews, textures,
/// and drafts start over.
impl Clone for ColorWindow {
    fn clone(&self) -> Self {
        Self {
            open: self.open,
            target: self.target,
            ..Self::default()
        }
    }
}

impl core::fmt::Debug for ColorWindow {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ColorWindow")
            .field("open", &self.open)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ColorWindow {
    fn eq(&self, other: &Self) -> bool {
        self.open == other.open && self.target == other.target
    }
}

impl Eq for ColorWindow {}

/// The input a layer's clean picture shows on a monitor: its own input, or
/// any Scene input showing the scene it draws directly without a grade of
/// its own (`None` while its scene's layout has not arrived).
fn layer_source_inputs(
    state: &StudioUiState,
    grading: &GradingStatus,
    scene: fm_types::SceneId,
    layer: fm_protocol::WireSceneLayerId,
) -> Vec<fm_types::InputId> {
    let Some(project) = state.project_state.as_ref() else {
        return Vec::new();
    };
    let source = project
        .scene_layout(scene)
        .and_then(|entry| entry.layout.as_ref())
        .and_then(|layout| layout.layers.iter().find(|outline| outline.id == layer))
        .map(|outline| outline.source);
    match source {
        Some(WireSceneLayerSource::Input(input)) => vec![input.to_domain()],
        Some(WireSceneLayerSource::Scene(nested)) => project
            .scenes()
            .iter()
            .filter(|shown| shown.scene == Some(nested.to_domain()))
            .map(|shown| shown.input)
            .filter(|input| target_grade(grading, GradeTarget::Input(*input)).is_identity())
            .collect(),
        None => Vec::new(),
    }
}

/// Every Scene input showing `scene`.
fn scene_inputs_showing(state: &StudioUiState, scene: fm_types::SceneId) -> Vec<fm_types::InputId> {
    state
        .project_state
        .as_ref()
        .map_or_else(Vec::new, |project| {
            project
                .scenes()
                .iter()
                .filter(|shown| shown.scene == Some(scene))
                .map(|shown| shown.input)
                .collect()
        })
}

/// The project grading once every announced record arrived.
fn grading(state: &StudioUiState) -> Option<&GradingStatus> {
    state
        .project_state
        .as_ref()
        .filter(|project| project.grading_complete())
        .map(fm_ui_model::ProjectState::grading)
}

/// Why `target`'s grade cannot be edited now (catalog English), if it
/// cannot: the session gates first, then padlocks (an input padlock also
/// covers that input's grade, and every Scene input's padlock its scene's
/// layer grades; a scene no Scene input shows only the project padlock).
pub(crate) fn grade_lock_reason(
    state: &StudioUiState,
    target: GradeTarget,
) -> Option<&'static str> {
    if !state.connection_status.controls_enabled()
        || state.view.is_none()
        || !state.can_edit_project
    {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    if grading(state).is_none() {
        return Some(Msg::ColorReasonGradingPending.en());
    }
    let locks = &state.view.as_ref()?.edit_locks;
    if locks.project {
        Some(PROJECT_LOCKED_REASON)
    } else if match target {
        GradeTarget::Input(input) => locks.is_input_locked(input),
        GradeTarget::Layer { scene, .. } => scene_inputs_showing(state, scene)
            .into_iter()
            .any(|input| locks.is_input_locked(input)),
        GradeTarget::Program => false,
    } {
        Some(INPUT_LOCKED_REASON)
    } else {
        None
    }
}

/// The library-wide edits (saves, LUT imports) are project edits: only the
/// session gates and the project padlock apply.
pub(crate) fn project_lock_reason(state: &StudioUiState) -> Option<&'static str> {
    grade_lock_reason(state, GradeTarget::Program)
}

/// The monitor tap frame that shows `target` without any grade on air, if
/// one is fresh: a valid source for the local preview.
pub(crate) fn ungraded_frame<'a>(
    state: &'a StudioUiState,
    grading: &GradingStatus,
    target: GradeTarget,
    now: Instant,
) -> Option<(MonitorTap, &'a MonitorImage)> {
    let view = state.view.as_ref()?;
    let fresh = |tap: MonitorTap| {
        state.monitor_images[tap.index()]
            .as_ref()
            .filter(|(_, at)| now.saturating_duration_since(*at) < MONITOR_STALE_AFTER)
            .map(|(image, _)| image)
    };
    let program_clean = target_grade(grading, GradeTarget::Program).is_identity();
    match target {
        GradeTarget::Program => fresh(MonitorTap::Program)
            .filter(|_| program_clean)
            .map(|image| (MonitorTap::Program, image)),
        // A layer's clean picture is its source as a monitor shows it
        // (with the source's input grade, before the layer's own grade).
        GradeTarget::Layer { scene, layer } => {
            let sources = layer_source_inputs(state, grading, scene, layer);
            [MonitorTap::Preview, MonitorTap::Program]
                .into_iter()
                .find_map(|tap| {
                    let realized = match tap {
                        MonitorTap::Preview => view.switcher.realized.preview,
                        MonitorTap::Program => view.switcher.realized.program,
                    };
                    let image = fresh(tap)?;
                    (sources.contains(&realized)
                        && image.input == WireInputId::from_domain(realized)
                        && (tap == MonitorTap::Preview || program_clean))
                        .then_some((tap, image))
                })
        }
        GradeTarget::Input(input) => {
            if !target_grade(grading, target).is_identity() {
                return None;
            }
            let wire = WireInputId::from_domain(input);
            [MonitorTap::Preview, MonitorTap::Program]
                .into_iter()
                .find_map(|tap| {
                    let realized = match tap {
                        MonitorTap::Preview => view.switcher.realized.preview,
                        MonitorTap::Program => view.switcher.realized.program,
                    };
                    let image = fresh(tap)?;
                    (realized == input
                        && image.input == wire
                        && (tap == MonitorTap::Preview || program_clean))
                        .then_some((tap, image))
                })
        }
    }
}

fn rgba(image: &MonitorImage) -> Option<RgbaImage> {
    RgbaImage::new(
        u32::from(image.width),
        u32::from(image.height),
        image.rgba.to_vec(),
    )
    .map(|image| image.fit_within(PREVIEW_MAX.0, PREVIEW_MAX.1))
}

fn name_reason(problem: GradeNameProblem, lut: bool) -> &'static str {
    match problem {
        GradeNameProblem::Blank if lut => Msg::ColorReasonNoPath.en(),
        GradeNameProblem::Blank => Msg::ColorReasonNameBlank.en(),
        GradeNameProblem::TooLong => Msg::ReasonNameTooLongBytes.en(),
        GradeNameProblem::ControlCharacter => Msg::ColorReasonNameControl.en(),
        GradeNameProblem::Taken if lut => Msg::ColorReasonLutNameTaken.en(),
        GradeNameProblem::Taken => Msg::ColorReasonNameTaken.en(),
    }
}

/// The LUT name a path imports as: its file name without `.cube`.
fn lut_name(path: &str) -> String {
    let path = path.trim();
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file.strip_suffix(".cube")
        .or_else(|| file.strip_suffix(".CUBE"))
        .unwrap_or(file)
        .to_owned()
}

/// `target`'s name in the target list.
fn target_label(l: Locale, state: &StudioUiState, target: GradeTarget) -> String {
    match target {
        GradeTarget::Program => l.text(Msg::ColorTargetProgram).to_owned(),
        GradeTarget::Input(input) => input_label(state, input),
        GradeTarget::Layer { scene, layer } => {
            let entry = state
                .project_state
                .as_ref()
                .and_then(|project| project.scene_layout(scene));
            let name = entry
                .and_then(|entry| entry.layout.as_ref())
                .and_then(|layout| {
                    layout
                        .layers
                        .iter()
                        .find(|outline| outline.id == layer)
                        .map(|outline| outline.name.clone())
                })
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("{} {layer}", l.text(Msg::ColorTargetLayerFallback)));
            let scene_name = entry
                .map(|entry| entry.name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("{} {scene}", l.text(Msg::ColorTargetSceneFallback)));
            if scene_inputs_showing(state, scene).is_empty() {
                format!(
                    "{scene_name} ({}) › {name}",
                    l.text(Msg::ColorTargetSceneUnshown)
                )
            } else {
                format!("{scene_name} › {name}")
            }
        }
    }
}

/// An input's name in the target list.
fn input_label(state: &StudioUiState, input: fm_types::InputId) -> String {
    state
        .view
        .as_ref()
        .and_then(|view| {
            let index = view
                .inputs
                .iter()
                .position(|candidate| *candidate == input)?;
            Some(format!("{:02} · {}", index + 1, view.input_names[index]))
        })
        .unwrap_or_else(|| format!("Input {input}"))
}

/// Every scene layer target: each replicated project scene's layers
/// (Protocol 2.35: whether or not a Scene input shows the scene), in scene
/// then scene (stacking list) order.
fn layer_targets(state: &StudioUiState) -> Vec<GradeTarget> {
    state
        .project_state
        .as_ref()
        .map_or_else(Vec::new, |project| {
            project
                .scene_layouts()
                .iter()
                .filter_map(|entry| Some((entry.scene, entry.layout.as_ref()?)))
                .flat_map(|(scene, layout)| {
                    layout.layers.iter().map(move |outline| GradeTarget::Layer {
                        scene,
                        layer: outline.id,
                    })
                })
                .collect()
        })
}

const fn status_look(status: GradeSyncStatus) -> (Msg, Msg, Tone) {
    match status {
        // The grade is what Program shows: on-air red, as every ON AIR mark.
        GradeSyncStatus::OnAir => (Msg::OverlayOnAir, Msg::ColorStatusOnAirHover, Tone::OnAir),
        GradeSyncStatus::Editing => (
            Msg::ColorStatusEditing,
            Msg::ColorStatusEditingHover,
            Tone::Pending,
        ),
        GradeSyncStatus::Sending => (
            Msg::ColorStatusSending,
            Msg::ColorStatusSendingHover,
            Tone::Pending,
        ),
        GradeSyncStatus::Refused => (
            Msg::ColorStatusRefused,
            Msg::ColorStatusRefusedHover,
            Tone::Fault,
        ),
        GradeSyncStatus::Diverged => (
            Msg::ColorStatusDiverged,
            Msg::ColorStatusDivergedHover,
            Tone::Pending,
        ),
    }
}

impl ColorWindow {
    /// Binds the draft to the authority and handles a published refusal.
    fn sync(&mut self, state: &StudioUiState) {
        let gone = match self.target {
            GradeTarget::Input(input) => state
                .view
                .as_ref()
                .is_some_and(|view| !view.inputs.contains(&input)),
            GradeTarget::Layer { scene, .. } => {
                state.project_state.as_ref().is_some_and(|project| {
                    project.scene_layouts_complete() && project.scene_layout(scene).is_none()
                })
            }
            GradeTarget::Program => false,
        };
        if gone {
            self.target = GradeTarget::Program;
        }
        let Some(grading) = grading(state) else {
            if let Some(binding) = &mut self.binding {
                binding.abandon_in_flight();
            }
            return;
        };
        let revision = state
            .view
            .as_ref()
            .map_or(0, |view| view.cursor.revision.get());
        let binding = self
            .binding
            .get_or_insert_with(|| GradeBinding::new(self.target, grading, revision));
        binding.retarget(self.target, grading, revision);
        if let Some(refusal) = &state.grade_refusal
            && self.refusal.as_ref() != Some(&refusal.command_id)
        {
            self.refusal = Some(refusal.command_id.clone());
            if refusal.target == Some(binding.target()) {
                binding.refused();
            }
        }
        binding.sync(grading, revision);
        self.panel.lut_choices = grading
            .luts
            .iter()
            .map(|entry| lut_uri(entry.lut))
            .collect();
        self.panel.lut_names = grading
            .luts
            .iter()
            .map(|entry| (lut_uri(entry.lut), entry.name.clone()))
            .collect();
        if self
            .library_pick
            .is_some_and(|pick| grading.library_grade(pick).is_none())
        {
            self.library_pick = None;
        }
    }

    /// Feeds the preview an ungraded picture of the target.
    fn refresh_source(&mut self, state: &StudioUiState, now: Instant) {
        let dragging = self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.document().gesture_active());
        if dragging {
            return;
        }
        let target = self.target;
        let live = grading(state).and_then(|grading| ungraded_frame(state, grading, target, now));
        if let Some((tap, image)) = live {
            let same_target = self.source.is_some_and(|(shown, _)| shown == target);
            let due = !same_target
                || self
                    .source_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= SOURCE_REFRESH);
            if due
                && self.source_frame != Some((tap, image.sequence))
                && let Some(picture) = rgba(image)
            {
                self.panel.set_source(&picture);
                if self.held.len() >= HELD_SOURCES && !self.held.contains_key(&target) {
                    self.held.pop_first();
                }
                self.held.insert(target, picture);
                self.source = Some((target, SourceKind::Live(tap)));
                self.source_frame = Some((tap, image.sequence));
                self.source_at = Some(now);
            }
            return;
        }
        let kind = if self.held.contains_key(&target) {
            SourceKind::Held
        } else {
            SourceKind::Still
        };
        if self.source == Some((target, kind)) {
            return;
        }
        let picture = self
            .held
            .get(&target)
            .cloned()
            .unwrap_or_else(|| sample_still(PREVIEW_MAX.0, PREVIEW_MAX.1));
        self.panel.set_source(&picture);
        self.source = Some((target, kind));
        self.source_frame = None;
        self.source_at = Some(now);
    }
}

impl StudioShell {
    /// Opens the Colour window on `target`, or on its last target.
    pub(crate) fn open_color(&mut self, target: Option<GradeTarget>) {
        self.color.open = true;
        if let Some(target) = target {
            self.color.target = target;
        }
    }

    /// Whether the Colour window is open.
    #[must_use]
    pub fn color_open(&self) -> bool {
        self.color.open
    }

    /// The Colour window's target.
    #[must_use]
    pub const fn color_target(&self) -> GradeTarget {
        self.color.target
    }

    /// Opens the Colour window on `target` (for hosts and tests).
    pub fn open_color_window(&mut self, target: GradeTarget) {
        self.open_color(Some(target));
    }

    /// Takes the LUT files the operator asked to import. The host reads
    /// each (at most [`fm_protocol::MAX_GRADE_LUT_IMPORT_BYTES`]) and
    /// answers with [`Self::receive_grade_lut_file`].
    pub fn take_grade_lut_requests(&mut self) -> Vec<GradeLutRequest> {
        std::mem::take(&mut self.color.lut_requests)
    }

    /// Answers one [`GradeLutRequest`]: returns the import command to send,
    /// or toasts why the file cannot be imported. A valid LUT also becomes
    /// available to the local preview.
    pub fn receive_grade_lut_file(
        &mut self,
        request: GradeLutRequest,
        file: GradeLutFile,
    ) -> Option<StudioIntent> {
        let l = self.locale;
        let path = request.path.display().to_string();
        let file_arg = ("file", Arg::Str(&path));
        let problem = match file {
            GradeLutFile::Text(text) => match fm_grade::parse_cube(&text) {
                Ok((lut, _warnings)) => {
                    self.color
                        .panel
                        .set_lut(&lut_uri(request.lut), Arc::new(lut));
                    self.toasts.upsert(
                        &format!("grade-lut-{}", request.lut),
                        ToastKind::Pending,
                        l.format(Msg::ColorLutImporting, &[("name", Arg::Str(&request.name))]),
                    );
                    return Some(StudioIntent::Grade(GradeCommand::ImportLut {
                        lut: request.lut,
                        name: request.name,
                        data: text,
                    }));
                }
                Err(error) => l.format(
                    Msg::ColorLutInvalid,
                    &[file_arg, ("error", Arg::Str(&error.to_string()))],
                ),
            },
            GradeLutFile::TooLarge { bytes } => l.format(
                Msg::ColorLutTooLarge,
                &[file_arg, ("size", Arg::Count(bytes.div_ceil(1024)))],
            ),
            GradeLutFile::NotText => l.format(Msg::ColorLutNotText, &[file_arg]),
            GradeLutFile::Unreadable(error) => l.format(
                Msg::ColorLutUnreadable,
                &[file_arg, ("error", Arg::Str(&error))],
            ),
        };
        self.toasts.push(ToastKind::Error, problem);
        None
    }

    /// Forgets grade commands whose answers will not arrive (the worker
    /// stopped or never got them); their drafts stay for the operator.
    pub fn abandon_grade_requests(&mut self) {
        if let Some(binding) = &mut self.color.binding {
            binding.abandon_in_flight();
        }
    }

    /// `Ctrl+G` opens or closes the Colour window unless a text field or a
    /// dialog owns the keyboard.
    pub(crate) fn toggle_color_on_key(&mut self, ui: &mut Ui) {
        let context = ui.ctx().clone();
        if context.egui_wants_keyboard_input()
            || self.show_shortcuts
            || self.show_about
            || self.show_preferences
            || self.add_input.is_some()
            || self.remove_input.is_some()
        {
            return;
        }
        if ui.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::G)) {
            self.color.open = !self.color.open;
        }
    }

    /// Whether the Colour panel owns the keyboard (its single-key shortcuts
    /// must not reach the console).
    pub(crate) fn color_owns_keyboard(&self) -> bool {
        self.color.open && self.color.panel.keyboard_active
    }

    /// Draws the Colour window when open and sends due grade commands.
    pub(crate) fn draw_color_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let now = Instant::now();
        let time = Duration::from_secs_f64(context.input(|input| input.time).max(0.0));
        if self.color.open {
            self.color.sync(state);
            self.color.refresh_source(state, now);
            let l = self.locale;
            let mut open = true;
            let max_height = (context.viewport_rect().height() - 60.0).max(240.0);
            if let Some(geometry) = self.layout.tool_popped(ToolWindow::Color) {
                let (id, builder) = self.tool_viewport(ToolWindow::Color, geometry);
                let outcome = context.show_viewport_immediate(id, builder, |ui, class| {
                    workspace::tool_popout_frame(ui, l, ToolWindow::Color, class, |ui| {
                        ScrollArea::vertical()
                            .id_salt("studio-color-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                self.draw_color_contents(ui, state, intents, &mut open);
                            });
                    })
                });
                self.finish_tool_popout(ToolWindow::Color, outcome);
            } else {
                egui::Window::new(l.text(Msg::GradeTitle))
                .id(egui::Id::new("studio-color-window"))
                .title_bar(false)
                .auto_sized()
                .collapsible(false)
                // Bottom left, over the input bank: the Program monitor stays
                // in view to show the grade the server applied.
                .pivot(egui::Align2::LEFT_BOTTOM)
                .default_pos(egui::pos2(12.0, context.viewport_rect().bottom() - 30.0))
                .frame(
                    Frame::new()
                        .fill(raised_color())
                        .stroke(Stroke::new(
                            crate::theme::line_width(),
                            border_strong_color(),
                        ))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(Margin::same(12)),
                )
                .show(context, |ui| {
                    ScrollArea::vertical()
                        .id_salt("studio-color-scroll")
                        .max_height(max_height)
                        .show(ui, |ui| {
                            self.draw_color_contents(ui, state, intents, &mut open);
                        });
                });
            }
            if !open {
                self.color.open = false;
                self.color.panel.keyboard_active = false;
            }
        }
        let lock = grade_lock_reason(state, self.color.target);
        if let Some(binding) = &mut self.color.binding {
            if let Some(command) = binding.poll(time, lock.is_none()) {
                intents.push(StudioIntent::Grade(command));
            }
            if let Some(wait) = binding.next_send_in(time) {
                context.request_repaint_after(wait.max(Duration::from_millis(10)));
            }
        }
    }

    #[allow(clippy::too_many_lines)] // one window: header, library, LUTs, panel
    fn draw_color_contents(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
        open: &mut bool,
    ) {
        let l = self.locale;
        let target = self.color.target;
        let lock = grade_lock_reason(state, target);
        let graded: Vec<GradeTarget> = grading(state).map_or_else(Vec::new, |grading| {
            grading
                .inputs
                .keys()
                .map(|input| GradeTarget::Input(*input))
                .chain(
                    grading
                        .layers
                        .keys()
                        .map(|(scene, layer)| GradeTarget::Layer {
                            scene: *scene,
                            layer: *layer,
                        }),
                )
                .chain(grading.program.as_ref().map(|_| GradeTarget::Program))
                .collect()
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::ColorTarget)))
                .on_hover_text(l.text(Msg::ColorTargetHover));
            let mut targets = vec![GradeTarget::Program];
            if let Some(view) = &state.view {
                targets.extend(view.inputs.iter().map(|input| GradeTarget::Input(*input)));
            }
            targets.extend(layer_targets(state));
            let label = |candidate: GradeTarget| {
                let name = target_label(l, state, candidate);
                if graded.contains(&candidate) {
                    format!("{name} · {}", l.text(Msg::ColorGraded))
                } else {
                    name
                }
            };
            let mut picked = None;
            ComboBox::from_id_salt("studio-color-target")
                .selected_text(crate::theme::ellipsize(&label(target), 30))
                .width(230.0)
                .show_ui(ui, |ui| {
                    for candidate in targets.iter().copied() {
                        let hover = match candidate {
                            GradeTarget::Program => l.text(Msg::ColorTargetProgramHover),
                            GradeTarget::Input(_) => l.text(Msg::ColorTargetInputHover),
                            GradeTarget::Layer { .. } => l.text(Msg::ColorTargetLayerHover),
                        };
                        if ui
                            .selectable_label(candidate == target, label(candidate))
                            .on_hover_text(hover)
                            .clicked()
                        {
                            picked = Some(candidate);
                        }
                    }
                })
                .response
                .on_hover_text(l.text(Msg::ColorTargetHover));
            if let Some(candidate) = picked {
                self.color.target = candidate;
            }
            if let Some(binding) = &self.color.binding {
                let (text, hover, tone) = status_look(binding.status());
                status_chip(ui, l.text(text), tone, 96.0).on_hover_text(l.text(hover));
                let differs = binding.document().grade() != binding.authority();
                let revert = ui
                    .add_enabled(differs, small_button(l.text(Msg::ColorRevert)))
                    .hint(l.text(Msg::ColorRevertHover), Msg::ColorReasonOnAir.en());
                if revert.clicked()
                    && let Some(binding) = &mut self.color.binding
                {
                    binding.revert();
                }
            }
            self.draw_copy_menu(ui, state, intents, &targets, &label);
            // In the row, not right-aligned: an auto-sized window would grow
            // to the whole screen to push a trailing key to its edge.
            ui.separator();
            if ui
                .add(close_key(14.0))
                .on_hover_text(l10n::with_key(l, l.text(Msg::ColorClose), COLOR_SHORTCUT))
                .clicked()
            {
                *open = false;
            }
        });
        if let Some(reason) = lock {
            ui.label(caption(l10n::unavailable(l, reason)).color(AMBER));
        }
        self.draw_color_library(ui, state, intents);
        let caption_text = match self.color.source.map(|(_, kind)| kind) {
            Some(SourceKind::Live(tap)) => l.format(
                Msg::ColorSourceLive,
                &[(
                    "tap",
                    Arg::Str(match tap {
                        MonitorTap::Preview => l.text(Msg::TallyPreview),
                        MonitorTap::Program => l.text(Msg::TallyProgram),
                    }),
                )],
            ),
            Some(SourceKind::Held) => l.text(Msg::ColorSourceHeld).to_owned(),
            Some(SourceKind::Still) | None if matches!(target, GradeTarget::Layer { .. }) => {
                l.text(Msg::ColorSourceLayerStill).to_owned()
            }
            Some(SourceKind::Still) | None => l.text(Msg::ColorSourceStill).to_owned(),
        };
        ui.label(caption(caption_text).color(muted_color()))
            .on_hover_text(l.text(Msg::ColorSourceHover));
        ui.add_space(4.0);
        if let Some(binding) = &mut self.color.binding {
            self.color.panel.lock_reason = lock;
            if lock.is_some() {
                self.color.panel.keyboard_active = false;
            }
            let _ = ColorPanelWidget::new(binding.document_mut(), &mut self.color.panel)
                .locale(l)
                .show(ui);
        } else {
            ui.label(caption(l10n::unavailable(
                l,
                lock.unwrap_or(Msg::ColorReasonGradingPending.en()),
            )));
        }
    }

    /// `Copy to…`: ticks other targets and gives each the draft, one
    /// standalone grade set (and undo step) per target.
    #[allow(clippy::too_many_lines)] // one menu: header keys, targets, Copy
    fn draw_copy_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
        targets: &[GradeTarget],
        label: &dyn Fn(GradeTarget) -> String,
    ) {
        let l = self.locale;
        let own = self.color.target;
        let others: Vec<(GradeTarget, Option<&'static str>)> = targets
            .iter()
            .copied()
            .filter(|candidate| *candidate != own)
            .map(|candidate| (candidate, grade_lock_reason(state, candidate)))
            .collect();
        // Ticks of gone, locked, or edited targets do not count.
        self.color.copy_ticked.retain(|ticked| {
            others
                .iter()
                .any(|(candidate, lock)| candidate == ticked && lock.is_none())
        });
        let menu_reason = project_lock_reason(state).or_else(|| {
            self.color
                .binding
                .is_none()
                .then_some(Msg::ColorReasonGradingPending.en())
        });
        let mut send = false;
        ui.add_enabled_ui(menu_reason.is_none(), |ui| {
            // Ticking keeps the menu open; Copy or a click outside closes it.
            let (button, _) = egui::containers::menu::MenuButton::from_button(small_button(
                l.text(Msg::ColorCopyTo),
            ))
            .config(
                egui::containers::menu::MenuConfig::new()
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
            )
            .ui(ui, |ui| {
                ui.set_min_width(260.0);
                ui.label(section_label(l.text(Msg::ColorCopyHeader)))
                    .on_hover_text(l.text(Msg::ColorCopyHeaderHover));
                let free_inputs: Vec<GradeTarget> = others
                    .iter()
                    .filter(|(candidate, lock)| {
                        matches!(candidate, GradeTarget::Input(_)) && lock.is_none()
                    })
                    .map(|(candidate, _)| *candidate)
                    .collect();
                ui.horizontal(|ui| {
                    let every_reason = free_inputs
                        .iter()
                        .all(|candidate| self.color.copy_ticked.contains(candidate))
                        .then_some(Msg::ColorReasonCopyNoOther.en());
                    if ui
                        .add_enabled(
                            every_reason.is_none(),
                            small_button(l.text(Msg::ColorCopyEveryInput)),
                        )
                        .hint(
                            l.text(Msg::ColorCopyEveryInputHover),
                            every_reason.unwrap_or(""),
                        )
                        .clicked()
                    {
                        self.color.copy_ticked.extend(free_inputs.iter().copied());
                    }
                    let untick_reason = self
                        .color
                        .copy_ticked
                        .is_empty()
                        .then_some(Msg::ColorReasonCopyNothingTicked.en());
                    if ui
                        .add_enabled(
                            untick_reason.is_none(),
                            small_button(l.text(Msg::ColorCopyUntick)),
                        )
                        .hint(
                            l.text(Msg::ColorCopyUntickHover),
                            untick_reason.unwrap_or(""),
                        )
                        .clicked()
                    {
                        self.color.copy_ticked.clear();
                    }
                });
                ui.separator();
                ScrollArea::vertical()
                    .id_salt("studio-color-copy-targets")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (candidate, lock) in &others {
                            let mut ticked = self.color.copy_ticked.contains(candidate);
                            let changed = ui
                                .add_enabled(
                                    lock.is_none(),
                                    egui::Checkbox::new(&mut ticked, label(*candidate)),
                                )
                                .hint(l.text(Msg::ColorCopyPickHover), lock.unwrap_or(""))
                                .changed();
                            if changed {
                                if ticked {
                                    self.color.copy_ticked.insert(*candidate);
                                } else {
                                    self.color.copy_ticked.remove(candidate);
                                }
                            }
                        }
                    });
                ui.separator();
                let ticked: Vec<GradeTarget> = others
                    .iter()
                    .map(|(candidate, _)| *candidate)
                    .filter(|candidate| self.color.copy_ticked.contains(candidate))
                    .collect();
                let pending = match (&self.color.binding, grading(state)) {
                    (Some(binding), Some(grading)) => binding.copy_commands(&ticked, grading).len(),
                    _ => 0,
                };
                let apply_reason = if ticked.is_empty() {
                    Some(Msg::ColorReasonCopyNone.en())
                } else if pending == 0 {
                    Some(Msg::ColorReasonCopySame.en())
                } else {
                    None
                };
                let count = u64::try_from(pending).unwrap_or(u64::MAX);
                if ui
                    .add_enabled(
                        apply_reason.is_none(),
                        small_button(
                            l.format(Msg::ColorCopyApply, &[("count", Arg::Count(count))]),
                        ),
                    )
                    .hint(l.text(Msg::ColorCopyApplyHover), apply_reason.unwrap_or(""))
                    .clicked()
                {
                    send = true;
                    ui.close();
                }
            });
            let _ = button.hint(l.text(Msg::ColorCopyToHover), menu_reason.unwrap_or(""));
        });
        if send {
            self.send_grade_copies(state, intents);
        }
    }

    /// Sends the draft to every ticked target (one standalone grade set
    /// each) and unticks them.
    fn send_grade_copies(&mut self, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
        let (Some(binding), Some(grading)) = (&self.color.binding, grading(state)) else {
            return;
        };
        let ticked: Vec<GradeTarget> = self.color.copy_ticked.iter().copied().collect();
        let commands = binding.copy_commands(&ticked, grading);
        self.color.copy_ticked.clear();
        if commands.is_empty() {
            return;
        }
        let count = u64::try_from(commands.len()).unwrap_or(u64::MAX);
        intents.extend(commands.into_iter().map(StudioIntent::Grade));
        let l = self.locale;
        self.toasts.push(
            ToastKind::Notice,
            l.format(Msg::ColorCopySent, &[("count", Arg::Count(count))]),
        );
    }

    /// The library and LUT import rows.
    #[allow(clippy::too_many_lines)] // two rows of gated controls
    fn draw_color_library(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = self.locale;
        let empty = GradingStatus::default();
        let grading = grading(state).unwrap_or(&empty);
        let target_lock = grade_lock_reason(state, self.color.target);
        let project_lock = project_lock_reason(state);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::ColorLibrary)))
                .on_hover_text(l.text(Msg::ColorLibraryHover));
            let pick = self.color.library_pick;
            let shown = pick
                .and_then(|pick| grading.library_grade(pick))
                .map_or_else(
                    || l.text(Msg::ColorLibraryNone).to_owned(),
                    |entry| entry.name.clone(),
                );
            let has_library = !grading.library.is_empty();
            let mut picked = None;
            let combo = ui
                .add_enabled_ui(has_library, |ui| {
                    ComboBox::from_id_salt("studio-color-library")
                        .selected_text(crate::theme::ellipsize(&shown, 22))
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for entry in &grading.library {
                                if ui
                                    .selectable_label(pick == Some(entry.grade), &entry.name)
                                    .on_hover_text(l.text(Msg::ColorLibraryPickHover))
                                    .clicked()
                                {
                                    picked = Some(entry.grade);
                                }
                            }
                        })
                        .response
                })
                .inner;
            let _ = combo.hint(
                l.text(Msg::ColorLibraryPickHover),
                Msg::ColorReasonNoLibrary.en(),
            );
            if picked.is_some() {
                self.color.library_pick = picked;
            }
            let chosen = self
                .color
                .library_pick
                .and_then(|pick| grading.library_grade(pick));
            let apply_reason = target_lock.or_else(|| {
                if !has_library {
                    Some(Msg::ColorReasonNoLibrary.en())
                } else if chosen.is_none() {
                    Some(Msg::ColorReasonPickLibrary.en())
                } else {
                    None
                }
            });
            let apply = ui
                .add_enabled(
                    apply_reason.is_none(),
                    small_button(l.text(Msg::ColorLibraryApply)),
                )
                .hint(
                    l.text(Msg::ColorLibraryApplyHover),
                    apply_reason.unwrap_or(""),
                );
            if apply.clicked()
                && let Some(entry) = chosen
                && let Some(binding) = &mut self.color.binding
                && let Err(error) = binding.apply_library(entry.grade, &entry.value)
            {
                self.color.panel.show_error(&error);
            }
            ui.separator();
            let name = ui
                .add_enabled(
                    project_lock.is_none(),
                    TextEdit::singleline(&mut self.color.library_name)
                        .hint_text(l.text(Msg::ColorLibraryNameHint))
                        .desired_width(150.0),
                )
                .hint(
                    l.text(Msg::ColorLibraryNameHover),
                    project_lock.unwrap_or(""),
                );
            let typed = self.color.library_name.trim().to_owned();
            let save_reason = project_lock
                .or_else(|| library_name_problem(&typed, grading).map(|p| name_reason(p, false)))
                .or_else(|| {
                    next_library_id(grading)
                        .is_none()
                        .then_some(Msg::ColorReasonLibraryFull.en())
                });
            let save = ui
                .add_enabled(
                    save_reason.is_none(),
                    small_button(l.text(Msg::ColorLibrarySave)),
                )
                .hint(
                    l.text(Msg::ColorLibrarySaveHover),
                    save_reason.unwrap_or(""),
                );
            let submitted = name.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if (save.clicked() || (submitted && save_reason.is_none()))
                && let Some(grade) = next_library_id(grading)
                && let Some(binding) = &self.color.binding
            {
                intents.push(StudioIntent::Grade(GradeCommand::AddLibrary {
                    grade,
                    name: typed,
                    value: Box::new(binding.document().grade().clone()),
                }));
                self.color.library_name.clear();
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::ColorLut)))
                .on_hover_text(l.text(Msg::ColorLutHover));
            ui.add_enabled(
                project_lock.is_none(),
                TextEdit::singleline(&mut self.color.lut_path)
                    .hint_text(l.text(Msg::ColorLutPathHint))
                    .desired_width(300.0),
            )
            .hint(l.text(Msg::ColorLutPathHover), project_lock.unwrap_or(""));
            let name = lut_name(&self.color.lut_path);
            let import_reason = project_lock
                .or_else(|| {
                    self.color
                        .lut_path
                        .trim()
                        .is_empty()
                        .then_some(Msg::ColorReasonNoPath.en())
                })
                .or_else(|| lut_name_problem(&name, grading).map(|p| name_reason(p, true)))
                .or_else(|| {
                    next_lut_id(grading, self.color.lut_issued)
                        .is_none()
                        .then_some(Msg::ColorReasonLutsFull.en())
                });
            let import = ui
                .add_enabled(
                    import_reason.is_none(),
                    small_button(l.text(Msg::ColorLutImport)),
                )
                .hint(
                    l.text(Msg::ColorLutImportHover),
                    import_reason.unwrap_or(""),
                );
            if import.clicked()
                && let Some(lut) = next_lut_id(grading, self.color.lut_issued)
            {
                self.color.lut_issued = Some(lut);
                self.color.lut_requests.push(GradeLutRequest {
                    path: PathBuf::from(self.color.lut_path.trim()),
                    lut,
                    name,
                });
                self.color.lut_path.clear();
            }
        });
        ui.label(
            RichText::new(l.text(Msg::ColorLutLimit))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
        self.grade_file_row(ui, grading, project_lock);
    }

    /// Sharing grades through `.fmgrade` files: export the draft, import
    /// into the library.
    fn grade_file_row(
        &mut self,
        ui: &mut Ui,
        grading: &GradingStatus,
        project_lock: Option<&'static str>,
    ) {
        let l = self.locale;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::ColorGradeFile)))
                .on_hover_text(l.text(Msg::ColorGradeFileHover));
            ui.add(
                TextEdit::singleline(&mut self.color.grade_path)
                    .hint_text(l.text(Msg::ColorGradePathHint))
                    .desired_width(300.0),
            )
            .on_hover_text(l.text(Msg::ColorGradePathHover));
            let path = self.color.grade_path.trim().to_owned();
            let no_path = path.is_empty().then_some(Msg::AnimReasonNoPath.en());
            let export_reason = no_path.or_else(|| {
                self.color
                    .binding
                    .is_none()
                    .then_some(Msg::ColorReasonGradingPending.en())
            });
            let export = ui
                .add_enabled(
                    export_reason.is_none(),
                    small_button(l.text(Msg::ColorGradeExport)),
                )
                .hint(
                    l.text(Msg::ColorGradeExportHover),
                    export_reason.unwrap_or(""),
                );
            if export.clicked()
                && let Some(binding) = &self.color.binding
            {
                let grade = binding.document().grade().clone();
                let lut = grade.lut.as_ref().and_then(|lut| {
                    let id = lut_id(&lut.asset_uri)?;
                    let entry = grading.luts.iter().find(|entry| entry.lut == id)?;
                    Some(ExportLut {
                        lut: id,
                        name: entry.name.clone(),
                        strength_permille: lut.strength_permille,
                    })
                });
                let path = PathBuf::from(&path);
                let typed = self.color.library_name.trim();
                let name = if typed.is_empty() {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .filter(|stem| !stem.trim().is_empty())
                        .unwrap_or("Grade")
                        .to_owned()
                } else {
                    typed.to_owned()
                };
                self.color.lut_replace = None;
                self.color
                    .grade_file_requests
                    .push(PresetFileRequest::ExportGrade {
                        path,
                        name,
                        grade: Box::new(grade),
                        lut,
                        replace_lut: false,
                    });
            }
            let import_reason = project_lock.or(no_path).or_else(|| {
                next_library_id(grading)
                    .is_none()
                    .then_some(Msg::ColorReasonLibraryFull.en())
            });
            let import = ui
                .add_enabled(
                    import_reason.is_none(),
                    small_button(l.text(Msg::ColorGradeImport)),
                )
                .hint(
                    l.text(Msg::ColorGradeImportHover),
                    import_reason.unwrap_or(""),
                );
            if import.clicked() {
                self.color
                    .grade_file_requests
                    .push(PresetFileRequest::ImportGrade {
                        path: PathBuf::from(path),
                    });
                self.color.grade_path.clear();
            }
        });
        self.lut_replace_row(ui);
    }

    /// Answers the pending sibling-LUT question: `replace` sends the export
    /// again with `replace_lut`; otherwise the export is dropped and the
    /// existing file stays.
    pub(crate) fn answer_lut_replace(&mut self, replace: bool) {
        let Some(pending) = self.color.lut_replace.take() else {
            return;
        };
        if replace
            && let PresetFileRequest::ExportGrade {
                path,
                name,
                grade,
                lut,
                ..
            } = pending.request
        {
            self.color
                .grade_file_requests
                .push(PresetFileRequest::ExportGrade {
                    path,
                    name,
                    grade,
                    lut,
                    replace_lut: true,
                });
        }
    }

    /// Asks whether a grade export may overwrite the existing `.cube` file
    /// beside its `.fmgrade`.
    fn lut_replace_row(&mut self, ui: &mut Ui) {
        let l = self.locale;
        let Some(pending) = &self.color.lut_replace else {
            return;
        };
        let name = match &pending.request {
            PresetFileRequest::ExportGrade { name, .. } => name.clone(),
            _ => String::new(),
        };
        let lut = pending.lut.display().to_string();
        let mut replace = false;
        let mut keep = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(l.format(
                    Msg::ColorGradeLutExists,
                    &[("name", Arg::Str(&name)), ("lut", Arg::Str(&lut))],
                ))
                .color(AMBER),
            )
            .on_hover_text(l.text(Msg::ColorGradeLutExistsHover));
            replace = ui
                .add(small_button(l.text(Msg::ColorGradeLutReplace)))
                .on_hover_text(l.text(Msg::ColorGradeLutReplaceHover))
                .clicked();
            keep = ui
                .add(small_button(l.text(Msg::ColorGradeLutKeep)))
                .on_hover_text(l.text(Msg::ColorGradeLutKeepHover))
                .clicked();
        });
        if replace || keep {
            self.answer_lut_replace(replace);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{
        EngineIdentity, FadeToBlackPosition, FadeToBlackState, GradeEntry, GradeStateMessage,
        InputStatus, ManualTransitionStatus, MonitorFrameOrigin, OverlayStatus, RecordingStatus,
        SnapshotMessage, WireGradeSource, WireRecorderSlot,
    };
    use fm_types::{InputId, ProjectId};
    use fm_ui_model::grade_editor::GradeParam;
    use fm_ui_model::{ClientModel, ProjectSnapshot};

    use super::*;
    use crate::StudioConnectionStatus;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn wire(value: u128) -> WireInputId {
        WireInputId::from_domain(input(value))
    }

    fn layer(value: u32) -> fm_protocol::WireSceneLayerId {
        fm_protocol::WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
    }

    fn scene(value: u128) -> fm_types::SceneId {
        fm_types::SceneId::new(NonZeroU128::new(value).unwrap())
    }

    fn wire_scene(value: u128) -> fm_protocol::WireSceneId {
        fm_protocol::WireSceneId::from_domain(scene(value))
    }

    /// The scene Scene input 3 shows, and a scene no input shows.
    const SHOWN: u128 = 30;
    const SPARE: u128 = 31;

    fn fm_model_timebase() -> fm_animation::Timebase {
        fm_animation::Timebase::new(fm_types::FrameRate::new(30, 1).unwrap())
    }

    fn warm() -> fm_grade::Grade {
        let mut grade = fm_grade::Grade::IDENTITY;
        grade.white_balance.temperature = 300;
        grade
    }

    /// Three inputs, input 1 on Program and 2 on Preview; `graded` inputs
    /// carry a warm grade and `locked` engages the project padlock.
    fn ready(graded: &[u128], locked: bool, edit: bool) -> StudioUiState {
        ready_with(graded, locked, edit, None)
    }

    /// [`ready`] with input 3 a Scene input showing scene 30 ("Two up")
    /// with layers 1 ("Host", drawing input 2) and 2 (unnamed, drawing input
    /// 1), plus scene 31 ("Spare") no input shows, with layer 5 ("Wall")
    /// drawing scene 30: `scene` is whether layer 1 carries the warm grade
    /// and whether input 3's padlock is engaged.
    #[allow(clippy::too_many_lines)]
    fn ready_with(
        graded: &[u128],
        locked: bool,
        edit: bool,
        scene: Option<(bool, bool)>,
    ) -> StudioUiState {
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let live = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        let snapshot = SnapshotMessage {
            audio_mixer: fm_protocol::AudioMixerStatus::default(),
            fade_to_black_excluded_outputs: Vec::new(),
            fade_to_black_output_durations: Vec::new(),
            fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
            display_outputs: Vec::new(),
            display_output_states: Vec::new(),
            network_streams: Vec::new(),
            system_audio_inputs: Vec::new(),
            capture_inputs: Vec::new(),
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 10,
            show_name: "Show".into(),
            inputs: (1..=3)
                .map(|value| InputStatus {
                    input: wire(value),
                    name: format!("Cam {value}"),
                    lifecycle: fm_protocol::InputLifecycleStatus::default(),
                })
                .collect(),
            outputs: Vec::new(),
            input_audio_strips: (1..=3)
                .map(|value| fm_protocol::InputAudioStripStatus {
                    pan_basis_points: 0,
                    group: None,
                    input: wire(value),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .collect(),
            desired_program: wire(1),
            desired_preview: wire(2),
            realized_program: wire(1),
            realized_preview: wire(2),
            desired_manual_transition: ManualTransitionStatus::Inactive,
            realized_manual_transition: ManualTransitionStatus::Inactive,
            desired_fade_to_black: live,
            realized_fade_to_black: live,
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| RecordingStatus {
                    recorder: WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus {
                project: locked,
                inputs: if scene.is_some_and(|(_, padlocked)| padlocked) {
                    vec![wire(3)]
                } else {
                    Vec::new()
                },
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: if scene.is_some() {
                vec![wire(3)]
            } else {
                Vec::new()
            },
            layout_scenes: if scene.is_some() {
                vec![wire_scene(SHOWN), wire_scene(SPARE)]
            } else {
                Vec::new()
            },
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: u16::try_from(graded.len()).unwrap()
                + u16::from(scene.is_some_and(|(graded, _)| graded)),
            animation_preset_states: 0,
            media_states: 0,
            cue_list_states: 0,
            script_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
            stream_targets: Vec::new(),
            live_lan: fm_protocol::LiveLanEntry::default(),
            live_lan_status: fm_protocol::LiveLanStatus::default(),
        };
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        for value in graded {
            model
                .apply_grade_state(GradeStateMessage {
                    revision: 10,
                    entry: GradeEntry::Input {
                        input: wire(*value),
                        source: WireGradeSource::Inline(Box::new(warm())),
                    },
                })
                .unwrap();
        }
        if let Some((graded, _)) = scene {
            if graded {
                model
                    .apply_grade_state(GradeStateMessage {
                        revision: 10,
                        entry: GradeEntry::Layer {
                            scene: wire_scene(SHOWN),
                            layer: layer(1),
                            source: WireGradeSource::Inline(Box::new(warm())),
                        },
                    })
                    .unwrap();
            }
            model
                .apply_scene_animation_state(fm_protocol::SceneAnimationStateMessage {
                    revision: 10,
                    input: wire(3),
                    scene: wire_scene(SHOWN),
                    animation_revision: 0,
                    clip: Box::new(
                        fm_protocol::WireSceneClip::new(fm_model_timebase(), 0, 0).unwrap(),
                    ),
                })
                .unwrap();
            let outline = |id: u32, name: &str, source| fm_protocol::WireSceneLayerOutline {
                id: layer(id),
                name: name.into(),
                x: 0,
                y: 0,
                width: 16,
                height: 9,
                quarter_turns: 0,
                opacity: 255,
                enabled: true,
                tilt_millidegrees: 0,
                anchor_x: 5_000,
                anchor_y: 5_000,
                z_order: 0,
                crop: None,
                key: None,
                effects: Vec::new(),
                mask: None,
                source,
            };
            let input_source = |value| fm_protocol::WireSceneLayerSource::Input(wire(value));
            for (id, name, layers) in [
                (
                    SHOWN,
                    "Two up",
                    vec![
                        outline(1, "Host", input_source(2)),
                        outline(2, "", input_source(1)),
                    ],
                ),
                (
                    SPARE,
                    "Spare",
                    vec![outline(
                        5,
                        "Wall",
                        fm_protocol::WireSceneLayerSource::Scene(wire_scene(SHOWN)),
                    )],
                ),
            ] {
                model
                    .apply_scene_layout_state(fm_protocol::SceneLayoutStateMessage {
                        revision: 10,
                        scene: wire_scene(id),
                        name: name.into(),
                        layout: Box::new(fm_protocol::WireSceneLayout {
                            width: 16,
                            height: 9,
                            layers,
                        }),
                    })
                    .unwrap();
            }
        }
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(model.view().unwrap())
            .with_switcher_permissions(true, true)
            .with_edit_project_permission(edit);
        state.project_state = model.state().cloned();
        state
    }

    fn frame(tap: MonitorTap, source: u128, sequence: u64) -> (MonitorImage, Instant) {
        (
            MonitorImage {
                tap,
                sequence,
                frame: sequence,
                input: wire(source),
                origin: MonitorFrameOrigin::Simulated,
                width: 4,
                height: 2,
                rgba: vec![90; 4 * 2 * 4].into(),
            },
            Instant::now(),
        )
    }

    fn draw(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
    ) -> Vec<StudioIntent> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(1600.0, 1000.0),
            )),
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-colour-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        let _ = context.end_pass();
        intents
    }

    #[test]
    fn only_ungraded_tap_frames_of_the_target_feed_the_preview() {
        let mut state = ready(&[], false, true);
        state.monitor_images = [
            Some(frame(MonitorTap::Preview, 2, 1)),
            Some(frame(MonitorTap::Program, 1, 1)),
        ];
        let grading = state.project_state.as_ref().unwrap().grading().clone();
        let now = Instant::now();
        let tap = |state: &StudioUiState, grading: &GradingStatus, target| {
            ungraded_frame(state, grading, target, now).map(|(tap, _)| tap)
        };
        assert_eq!(
            tap(&state, &grading, GradeTarget::Input(input(2))),
            Some(MonitorTap::Preview)
        );
        assert_eq!(
            tap(&state, &grading, GradeTarget::Input(input(1))),
            Some(MonitorTap::Program)
        );
        assert_eq!(tap(&state, &grading, GradeTarget::Input(input(3))), None);
        assert_eq!(
            tap(&state, &grading, GradeTarget::Program),
            Some(MonitorTap::Program)
        );
        // A graded input's frames already carry its grade.
        let graded = ready(&[2], false, true);
        let grading = graded.project_state.as_ref().unwrap().grading().clone();
        assert_eq!(tap(&state, &grading, GradeTarget::Input(input(2))), None);
        // A Program grade taints the Program tap for inputs and Program.
        let grading = GradingStatus {
            program: Some(WireGradeSource::Inline(Box::new(warm()))),
            ..GradingStatus::default()
        };
        assert_eq!(tap(&state, &grading, GradeTarget::Input(input(1))), None);
        assert_eq!(tap(&state, &grading, GradeTarget::Program), None);
        assert_eq!(
            tap(&state, &grading, GradeTarget::Input(input(2))),
            Some(MonitorTap::Preview)
        );
    }

    #[test]
    fn edits_in_the_colour_window_send_grade_commands_unless_locked() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = ready(&[2], false, true);
        shell.open_color_window(GradeTarget::Input(input(2)));
        assert_eq!(draw(&context, &mut shell, &state), Vec::new());
        let binding = shell.color.binding.as_mut().unwrap();
        assert_eq!(
            binding.document().grade(),
            &warm(),
            "starts from the grade on air"
        );
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 500)
            .unwrap();
        let intents = draw(&context, &mut shell, &state);
        let mut expected = warm();
        expected.exposure_millistops = 500;
        assert_eq!(
            intents,
            vec![StudioIntent::Grade(GradeCommand::Set {
                target: GradeTarget::Input(input(2)),
                source: Some(WireGradeSource::Inline(Box::new(expected))),
                gesture: None,
            })]
        );

        // The project padlock holds edits back and explains itself.
        let locked = ready(&[2], true, true);
        assert_eq!(
            grade_lock_reason(&locked, GradeTarget::Program),
            Some(PROJECT_LOCKED_REASON)
        );
        let binding = shell.color.binding.as_mut().unwrap();
        binding.revert();
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, -500)
            .unwrap();
        assert_eq!(draw(&context, &mut shell, &locked), Vec::new());
        assert_eq!(shell.color.panel.lock_reason, Some(PROJECT_LOCKED_REASON));
        let viewer = ready(&[], false, false);
        assert_eq!(
            grade_lock_reason(&viewer, GradeTarget::Input(input(1))),
            Some(Msg::ReasonNoEditProject.en())
        );
    }

    #[test]
    fn copy_to_sends_the_draft_to_each_ticked_unlocked_target() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = ready(&[2], false, true);
        shell.open_color_window(GradeTarget::Input(input(2)));
        let _ = draw(&context, &mut shell, &state);
        shell.color.copy_ticked.extend([
            GradeTarget::Input(input(1)),
            GradeTarget::Input(input(2)),
            GradeTarget::Program,
        ]);
        // Drawing drops the tick of the target being edited.
        assert_eq!(draw(&context, &mut shell, &state), Vec::new());
        assert_eq!(
            shell.color.copy_ticked.iter().copied().collect::<Vec<_>>(),
            vec![GradeTarget::Input(input(1)), GradeTarget::Program]
        );
        let mut intents = Vec::new();
        shell.send_grade_copies(&state, &mut intents);
        let set = |target| {
            StudioIntent::Grade(GradeCommand::Set {
                target,
                source: Some(WireGradeSource::Inline(Box::new(warm()))),
                gesture: None,
            })
        };
        assert_eq!(
            intents,
            vec![set(GradeTarget::Input(input(1))), set(GradeTarget::Program)]
        );
        assert!(shell.color.copy_ticked.is_empty(), "sent ticks clear");

        // The project padlock drops every tick.
        let locked = ready(&[2], true, true);
        shell.color.copy_ticked.insert(GradeTarget::Input(input(1)));
        let _ = draw(&context, &mut shell, &locked);
        assert!(shell.color.copy_ticked.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // every layer target addressing case
    fn scene_layers_are_grade_targets_addressed_by_their_scene() {
        let context = egui::Context::default();
        let mut state = ready_with(&[], false, true, Some((true, false)));
        let host = GradeTarget::Layer {
            scene: scene(SHOWN),
            layer: layer(1),
        };
        let unnamed = GradeTarget::Layer {
            scene: scene(SHOWN),
            layer: layer(2),
        };
        let wall = GradeTarget::Layer {
            scene: scene(SPARE),
            layer: layer(5),
        };
        // The picker lists every project scene's layers, shown or not,
        // named after their scene and layer (or numbered fallbacks).
        assert_eq!(layer_targets(&state), vec![host, unnamed, wall]);
        let l = Locale::new(fm_ui_model::i18n::Language::English);
        assert_eq!(target_label(l, &state, host), "Two up › Host");
        assert_eq!(target_label(l, &state, unnamed), "Two up › Layer 2");
        assert_eq!(
            target_label(l, &state, wall),
            "Spare (no Scene input) › Wall"
        );
        let es = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            target_label(es, &state, wall),
            "Spare (sin entrada de escena) › Wall"
        );

        // A layer previews on its source's live picture: Preview shows
        // input 2 (Host's source) and the clean Program input 1 (the
        // unnamed layer's). Wall draws scene 30, which no monitor shows.
        state.monitor_images = [
            Some(frame(MonitorTap::Preview, 2, 1)),
            Some(frame(MonitorTap::Program, 1, 1)),
        ];
        let grading = grading(&state).unwrap().clone();
        let now = Instant::now();
        let tap = |target| ungraded_frame(&state, &grading, target, now).map(|(tap, _)| tap);
        assert_eq!(tap(host), Some(MonitorTap::Preview));
        assert_eq!(tap(unnamed), Some(MonitorTap::Program));
        assert_eq!(tap(wall), None);
        // A Program grade taints the Program tap for layers too.
        let program_graded = GradingStatus {
            program: Some(WireGradeSource::Inline(Box::new(warm()))),
            ..grading.clone()
        };
        assert_eq!(
            ungraded_frame(&state, &program_graded, unnamed, now).map(|(tap, _)| tap),
            None
        );

        // The window starts from the layer's replicated grade and sends a
        // layer grade command.
        let mut shell = StudioShell::default();
        shell.open_color_window(host);
        assert_eq!(draw(&context, &mut shell, &state), Vec::new());
        let binding = shell.color.binding.as_mut().unwrap();
        assert_eq!(binding.document().grade(), &warm());
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 250)
            .unwrap();
        let mut expected = warm();
        expected.exposure_millistops = 250;
        let intents = draw(&context, &mut shell, &state);
        assert_eq!(
            intents,
            vec![StudioIntent::Grade(GradeCommand::Set {
                target: host,
                source: Some(WireGradeSource::Inline(Box::new(expected.clone()))),
                gesture: None,
            })]
        );
        let StudioIntent::Grade(command) = &intents[0] else {
            unreachable!()
        };
        assert_eq!(
            command.payload(),
            fm_protocol::CommandPayload::SetLayerGrade {
                scene: wire_scene(SHOWN),
                layer: layer(1),
                source: Some(WireGradeSource::Inline(Box::new(expected))),
                gesture: None,
            }
        );
        // A layer of an unshown scene is graded by its scene id.
        assert_eq!(
            GradeCommand::Set {
                target: wall,
                source: None,
                gesture: None,
            }
            .payload(),
            fm_protocol::CommandPayload::SetLayerGrade {
                scene: wire_scene(SPARE),
                layer: layer(5),
                source: None,
                gesture: None,
            }
        );

        // The Scene input's padlock covers its scene's layers' grades; an
        // unshown scene's layers only answer to the project padlock.
        let padlocked = ready_with(&[], false, true, Some((true, true)));
        assert_eq!(
            grade_lock_reason(&padlocked, host),
            Some(INPUT_LOCKED_REASON)
        );
        assert_eq!(grade_lock_reason(&padlocked, wall), None);
        assert_eq!(grade_lock_reason(&padlocked, GradeTarget::Program), None);
        let project_locked = ready_with(&[], true, true, Some((true, false)));
        assert_eq!(
            grade_lock_reason(&project_locked, wall),
            Some(PROJECT_LOCKED_REASON)
        );
    }

    #[test]
    fn a_refusal_marks_the_targets_draft_and_authority_updates_reset_clean_drafts() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        shell.open_color_window(GradeTarget::Input(input(2)));
        let mut state = ready(&[], false, true);
        draw(&context, &mut shell, &state);
        shell
            .color
            .binding
            .as_mut()
            .unwrap()
            .document_mut()
            .set_param(GradeParam::Saturation, 1500)
            .unwrap();
        assert_eq!(draw(&context, &mut shell, &state).len(), 1);
        state.grade_refusal = Some(GradeRefusal {
            command_id: "c1".into(),
            target: Some(GradeTarget::Input(input(2))),
        });
        draw(&context, &mut shell, &state);
        let binding = shell.color.binding.as_ref().unwrap();
        assert_eq!(binding.status(), GradeSyncStatus::Refused);
        // Revert drops the refused draft; the next authority grade is
        // adopted.
        shell.color.binding.as_mut().unwrap().revert();
        let graded = ready(&[2], false, true);
        draw(&context, &mut shell, &graded);
        assert_eq!(
            shell.color.binding.as_ref().unwrap().document().grade(),
            &warm()
        );
    }

    #[test]
    fn oversized_or_invalid_lut_files_are_explained_not_sent() {
        let mut shell = StudioShell::default();
        let request = GradeLutRequest {
            path: PathBuf::from("/luts/big.cube"),
            lut: NonZeroU32::new(3).unwrap(),
            name: "big".into(),
        };
        assert_eq!(
            shell.receive_grade_lut_file(request.clone(), GradeLutFile::TooLarge { bytes: 70_000 }),
            None
        );
        let toast = shell.toasts.iter().last().unwrap().message.clone();
        assert!(toast.contains("freemix-cli grade-import-lut"), "{toast}");
        assert!(toast.contains("69 KiB"), "{toast}");
        assert_eq!(
            shell.receive_grade_lut_file(request.clone(), GradeLutFile::Text("nonsense".into())),
            None
        );
        let text = "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n".to_owned();
        assert_eq!(
            shell.receive_grade_lut_file(request, GradeLutFile::Text(text.clone())),
            Some(StudioIntent::Grade(GradeCommand::ImportLut {
                lut: NonZeroU32::new(3).unwrap(),
                name: "big".into(),
                data: text,
            }))
        );
        assert!(shell.color.panel.preview().has_lut("asset://luts/3.cube"));
        assert_eq!(lut_name(" /a/b/Warm Film.cube "), "Warm Film");
        assert_eq!(lut_name("C:\\luts\\x.CUBE"), "x");
    }

    #[test]
    fn grade_exports_ask_before_replacing_an_existing_sibling_lut() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let state = ready(&[2], false, true);
        shell.open_color_window(GradeTarget::Input(input(2)));
        let request = PresetFileRequest::ExportGrade {
            path: PathBuf::from("/grades/big.fmgrade"),
            name: "Big".into(),
            grade: Box::new(warm()),
            lut: Some(ExportLut {
                lut: NonZeroU32::new(3).unwrap(),
                name: "Film".into(),
                strength_permille: 1_000,
            }),
            replace_lut: false,
        };
        let refused = crate::PresetFileAnswer::LutFileExists {
            path: PathBuf::from("/grades/big.cube"),
        };

        // The refusal toasts the question and keeps the export pending.
        assert_eq!(
            shell.receive_preset_file(&state, &request, refused.clone()),
            Vec::new()
        );
        let toast = shell.toasts.iter().last().unwrap();
        assert_eq!(toast.kind, ToastKind::Notice);
        assert!(
            toast.message.contains("/grades/big.cube"),
            "{}",
            toast.message
        );
        assert_eq!(
            shell.color.lut_replace.as_ref().map(|pending| &pending.lut),
            Some(&PathBuf::from("/grades/big.cube"))
        );
        // The question stays on screen until it is answered.
        draw(&context, &mut shell, &state);
        assert!(shell.color.lut_replace.is_some());
        assert_eq!(shell.take_preset_file_requests(), Vec::new());

        // Keeping the file drops the export.
        shell.answer_lut_replace(false);
        assert_eq!(shell.color.lut_replace, None);
        assert_eq!(shell.take_preset_file_requests(), Vec::new());

        // Replacing sends the same export with `replace_lut`.
        shell.receive_preset_file(&state, &request, refused);
        shell.answer_lut_replace(true);
        assert_eq!(shell.color.lut_replace, None);
        let PresetFileRequest::ExportGrade {
            path,
            name,
            replace_lut,
            ..
        } = shell.take_preset_file_requests().pop().unwrap()
        else {
            panic!("a grade export");
        };
        assert_eq!(
            (path, name.as_str(), replace_lut),
            (PathBuf::from("/grades/big.fmgrade"), "Big", true)
        );
    }
}
