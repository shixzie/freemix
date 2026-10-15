//! The Studio Animation window (GX-011): a "mini After Effects" for title
//! elements, bound to the replicated title animation.
//!
//! The window edits one title's template animation at a time through a
//! [`TitleAnimationBinding`] per title: a clean draft follows every
//! `title_animation_changed`, edits are saved as revision-checked
//! `title_animation_set` commands ([`StudioIntent::TitleAnimation`]) when a
//! drag is released (auto-save) or on Save, and a change by someone else
//! under unsaved edits raises a conflict notice with "Reload theirs" and
//! "Overwrite". Saves of drafts for titles not shown keep travelling.
//!
//! On top sits the canvas: the server's exact preview of the draft at the
//! playhead ([`StudioIntent::TitlePreview`], debounced and latest-wins under
//! the server's rate limit), with the wireframe proxy and onion skins drawn
//! over it while the exact frame is out of date, a drag runs, or playback
//! plays. Below sits the [`AnimationTimelineWidget`], in Simple mode (one
//! bar and preset picker per element) unless the operator switches to
//! Advanced (keys, easing, value graph).
//!
//! Element names and placement (the title's design) arrive with the
//! snapshot's `title_layout_state` (Protocol 2.30); the host hands each to
//! [`StudioShell::set_title_design`], which lists every element by name
//! with a kind icon, enables local presets for every element (animated or
//! not), and draws the proxy outlines and onion skins over the exact
//! preview. In Simple mode, resting on a preset in an element's picker
//! previews it before it is applied: its outlines play the motion at once
//! and an exact preview of the hovered draft follows once the pointer
//! rests; a click applies it as one undo step. A title without a layout
//! (its template did not load) lists the elements that already move,
//! applies their presets on the server (`title_animation_preset`, sent only
//! over a clean draft), and draws no outlines.
//!
//! The window floats over the console, resizable by its edges, or docks
//! along the bottom of the console with a draggable height; the choice and
//! sizes live in [`crate::ConsoleLayout`], which the host persists.

use core::time::Duration;
use std::collections::BTreeMap;

use egui::{
    Align2, Color32, ColorImage, ComboBox, CornerRadius, FontId, Frame, Key, Margin, Modifiers,
    Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2, pos2,
};
use fm_animation::Phase;
use fm_protocol::{CommandPayload, TitlePreviewImage, WireInputId, WireTitleElementId};
use fm_types::InputId;
use fm_ui_model::title_animation::{
    AnimationRefusal, AnimationSaveStatus, PresetHover, TitleAnimationBinding, TitleDesign,
};

use crate::animation_timeline::text::preset_name;
use crate::animation_timeline::{
    AnimationTimelineState, AnimationTimelineWidget, OverlayOptions, paint_document_overlay,
};
use crate::edit_history::{INPUT_LOCKED_REASON, PROJECT_LOCKED_REASON};
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, MONITOR_BLACK, Tone, border_strong_color, caption, close_key, muted_color, raised_color,
    section_label, small_button, status_chip, status_chip_painted, text_dim_color,
};
use crate::titles::title_display_name;
use crate::{ConsoleLayout, StudioIntent, StudioShell, StudioUiState};

/// The shortcut that opens and closes the Animation window.
pub const ANIMATION_SHORTCUT: &str = "Ctrl+Shift+A";
/// Smallest canvas drawn, in points.
const CANVAS_MIN_HEIGHT: f32 = 110.0;
/// Share of the window height the timeline keeps below the canvas.
const TIMELINE_SHARE: f32 = 0.45;
/// Lowest height the timeline keeps below the canvas, in points.
const TIMELINE_MIN_HEIGHT: f32 = 190.0;
/// The floating window's content size until the operator resizes it.
const DEFAULT_WINDOW_SIZE: Vec2 = Vec2::new(860.0, 640.0);
/// The docked panel's height until the operator drags it.
const DEFAULT_DOCK_HEIGHT: f32 = 430.0;
/// The docked panel's id.
const DOCK_ID: &str = "studio-animation-dock";

/// The refusal of one title animation save, kept by the worker until
/// another save is refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleAnimationRefusal {
    pub command_id: String,
    /// The title input the refused save named.
    pub input: InputId,
    /// Whether the save was refused as based on a stale animation
    /// revision (`revision_conflict`).
    pub conflict: bool,
}

/// The exact preview shown on the canvas.
struct PreviewTexture {
    texture: TextureHandle,
    input: InputId,
}

/// What an operator's drag is resizing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResizeDrag {
    Window,
    Dock,
}

/// The Animation window's state, next to the console.
pub(crate) struct AnimationWindow {
    pub(crate) open: bool,
    input: Option<InputId>,
    bindings: BTreeMap<InputId, TitleAnimationBinding>,
    pub(crate) timeline: AnimationTimelineState<WireTitleElementId>,
    designs: BTreeMap<InputId, TitleDesign>,
    /// A server-side preset in flight: its title and the animation
    /// revision it was based on.
    preset_pending: Option<(InputId, u64)>,
    texture: Option<PreviewTexture>,
    /// The last preview image, failure, and refusal acted on.
    seen_image: Option<String>,
    seen_failure: Option<String>,
    seen_refusal: Option<String>,
    pub(crate) onion: bool,
    /// The floating window's content size last frame.
    last_size: Option<Vec2>,
    /// The resize drag in progress, of the floating window's edges or the
    /// docked panel's edge.
    resize: Option<ResizeDrag>,
    /// Where the timeline laid out its ruler and rows last frame.
    pub(crate) tracks: Option<crate::animation_timeline::TrackGeometry>,
}

impl Default for AnimationWindow {
    fn default() -> Self {
        Self {
            open: false,
            input: None,
            bindings: BTreeMap::new(),
            timeline: AnimationTimelineState::default(),
            designs: BTreeMap::new(),
            preset_pending: None,
            texture: None,
            seen_image: None,
            seen_failure: None,
            seen_refusal: None,
            onion: true,
            last_size: None,
            resize: None,
            tracks: None,
        }
    }
}

/// Clones what the console compares (open and title); drafts, designs, and
/// textures start over.
impl Clone for AnimationWindow {
    fn clone(&self) -> Self {
        Self {
            open: self.open,
            input: self.input,
            ..Self::default()
        }
    }
}

impl core::fmt::Debug for AnimationWindow {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AnimationWindow")
            .field("open", &self.open)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl PartialEq for AnimationWindow {
    fn eq(&self, other: &Self) -> bool {
        self.open == other.open && self.input == other.input
    }
}

impl Eq for AnimationWindow {}

/// Why `input`'s animation cannot be saved now (catalog English): the
/// session gates first, then a missing animation, then padlocks.
pub(crate) fn animation_lock_reason(state: &StudioUiState, input: InputId) -> Option<&'static str> {
    if !state.connection_status.controls_enabled()
        || state.view.is_none()
        || !state.can_edit_project
    {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    let arrived = state
        .project_state
        .as_ref()
        .and_then(|project| project.title(input))
        .is_some_and(|title| title.animation.is_some());
    if !arrived {
        return Some(Msg::AnimReasonAnimationPending.en());
    }
    let locks = &state.view.as_ref()?.edit_locks;
    if locks.project {
        Some(PROJECT_LOCKED_REASON)
    } else if locks.is_input_locked(input) {
        Some(INPUT_LOCKED_REASON)
    } else {
        None
    }
}

/// Why exact previews cannot be asked for now (catalog English).
pub(crate) fn preview_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if !state.connection_status.controls_enabled()
        || state.view.is_none()
        || !state.can_edit_project
    {
        Some(unavailable_reason(state, Capability::EditProject))
    } else if !state.title_previews {
        Some(Msg::AnimReasonPreviewTransport.en())
    } else {
        None
    }
}

/// The titles whose animation has arrived, in snapshot order, with their
/// display names.
fn animatable_titles(l: Locale, state: &StudioUiState) -> Vec<(InputId, String)> {
    let Some(project) = &state.project_state else {
        return Vec::new();
    };
    project
        .titles()
        .iter()
        .filter(|title| title.animation.is_some())
        .map(|title| {
            (
                title.input,
                title_display_name(l, state.view.as_ref(), title.input, &title.name),
            )
        })
        .collect()
}

const fn status_look(status: AnimationSaveStatus) -> (Msg, Msg, Tone) {
    match status {
        AnimationSaveStatus::Saved => {
            (Msg::AnimStatusSaved, Msg::AnimStatusSavedHover, Tone::Ready)
        }
        AnimationSaveStatus::Unsaved => (
            Msg::AnimStatusUnsaved,
            Msg::AnimStatusUnsavedHover,
            Tone::Pending,
        ),
        AnimationSaveStatus::Saving => (
            Msg::AnimStatusSaving,
            Msg::AnimStatusSavingHover,
            Tone::Pending,
        ),
        AnimationSaveStatus::Conflict => (
            Msg::TitleConflict,
            Msg::AnimStatusConflictHover,
            Tone::Fault,
        ),
        AnimationSaveStatus::Refused => (
            Msg::ColorStatusRefused,
            Msg::AnimStatusRefusedHover,
            Tone::Fault,
        ),
    }
}

/// The canvas rect for a `canvas` shaped picture within `width` x
/// `max_height` points.
#[allow(clippy::cast_precision_loss)] // Canvas sizes are far below 2^24.
fn canvas_size(canvas: (u32, u32), width: f32, max_height: f32) -> Vec2 {
    let aspect = canvas.1.max(1) as f32 / canvas.0.max(1) as f32;
    let width = width.max(160.0);
    let max_height = max_height.max(CANVAS_MIN_HEIGHT);
    let height = width * aspect;
    if height > max_height {
        Vec2::new(max_height / aspect, max_height)
    } else {
        Vec2::new(width, height)
    }
}

/// The frame of a `frames` long phase the hover proxy shows at `time`
/// seconds: the motion plays at `fps` and rests half a second on the last
/// frame before it repeats.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)] // Times and frame counts are small and non-negative.
fn hover_frame(time: f64, fps: f64, frames: u32) -> u32 {
    let fps = fps.max(1.0);
    let cycle = u64::from(frames) + (fps / 2.0).ceil() as u64 + 1;
    let frame = ((time.max(0.0) * fps) as u64) % cycle;
    u32::try_from(frame.min(u64::from(frames))).unwrap_or(frames)
}

/// A quiet checkerboard that shows the title's transparent areas.
fn paint_checker(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, MONITOR_BLACK);
    let cell = 16.0;
    let light = Color32::from_rgb(22, 24, 27);
    let mut y = rect.top();
    let mut row = 0_u32;
    while y < rect.bottom() {
        let mut x = rect.left() + if row.is_multiple_of(2) { 0.0 } else { cell };
        while x < rect.right() {
            let tile = Rect::from_min_max(
                pos2(x, y),
                pos2((x + cell).min(rect.right()), (y + cell).min(rect.bottom())),
            );
            painter.rect_filled(tile, CornerRadius::ZERO, light);
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

impl AnimationWindow {
    /// Follows the authority for every draft, acts on a newly published
    /// refusal, preview failure, or preview image, and makes sure the shown
    /// title has a draft.
    pub(crate) fn sync(&mut self, context: &egui::Context, state: &StudioUiState) {
        let Some(project) = &state.project_state else {
            return;
        };
        self.bindings
            .retain(|input, _| project.title(*input).is_some());
        self.designs
            .retain(|input, _| project.title(*input).is_some());
        for (input, binding) in &mut self.bindings {
            if let Some(status) = project
                .title(*input)
                .and_then(|title| title.animation.as_ref())
            {
                binding.sync(status);
            }
        }
        let titles: Vec<InputId> = project
            .titles()
            .iter()
            .filter(|title| title.animation.is_some())
            .map(|title| title.input)
            .collect();
        if self.open && !self.input.is_some_and(|input| titles.contains(&input)) {
            self.input = titles.first().copied();
        }
        if self.open
            && let Some(input) = self.input
            && let Some(status) = project
                .title(input)
                .and_then(|title| title.animation.as_ref())
        {
            self.bindings
                .entry(input)
                .or_insert_with(|| TitleAnimationBinding::new(input, status));
        }
        for (input, binding) in &mut self.bindings {
            if !binding.has_design()
                && let Some(design) = self.designs.get(input)
            {
                binding.set_design(design);
            }
        }
        // A server-side preset is answered by the next animation revision.
        if let Some((input, revision)) = self.preset_pending
            && self
                .bindings
                .get(&input)
                .is_none_or(|binding| binding.authority().revision != revision)
        {
            self.preset_pending = None;
        }
        if let Some(refusal) = &state.title_animation_refusal
            && self.seen_refusal.as_ref() != Some(&refusal.command_id)
        {
            self.seen_refusal = Some(refusal.command_id.clone());
            if self
                .preset_pending
                .is_some_and(|(input, _)| input == refusal.input)
            {
                self.preset_pending = None;
            } else if let Some(binding) = self.bindings.get_mut(&refusal.input)
                && binding.saving()
            {
                binding.refused(if refusal.conflict {
                    AnimationRefusal::Conflict
                } else {
                    AnimationRefusal::Other
                });
            }
        }
        if let Some(failed) = &state.title_preview_failed
            && self.seen_failure.as_ref() != Some(failed)
        {
            self.seen_failure = Some(failed.clone());
            for binding in self.bindings.values_mut() {
                binding.preview_failed(failed);
            }
        }
        if let Some(image) = &state.title_preview
            && self.seen_image.as_ref() != Some(&image.request_id)
        {
            self.seen_image = Some(image.request_id.clone());
            self.receive_image(context, image);
        }
    }

    /// Shows `image` if a draft was waiting for it.
    fn receive_image(&mut self, context: &egui::Context, image: &TitlePreviewImage) {
        let input = image.input.to_domain();
        let Some(binding) = self.bindings.get_mut(&input) else {
            return;
        };
        if binding.receive_preview(&image.request_id).is_none() {
            return;
        }
        let size = [usize::from(image.width), usize::from(image.height)];
        let picture = ColorImage::from_rgba_unmultiplied(size, &image.rgba);
        match &mut self.texture {
            Some(shown) => {
                shown.texture.set(picture, TextureOptions::LINEAR);
                shown.input = input;
            }
            None => {
                self.texture = Some(PreviewTexture {
                    texture: context.load_texture(
                        "studio-animation-preview",
                        picture,
                        TextureOptions::LINEAR,
                    ),
                    input,
                });
            }
        }
    }
}

impl StudioShell {
    /// Opens the Animation window on title `input`, or on its last title.
    pub(crate) fn open_animation(&mut self, input: Option<InputId>) {
        self.animation.open = true;
        if input.is_some() {
            self.animation.input = input;
        }
    }

    /// Opens the Animation window on title `input` (for hosts and tests).
    pub fn open_animation_window(&mut self, input: InputId) {
        self.open_animation(Some(input));
    }

    /// Whether the Animation window is open.
    #[must_use]
    pub const fn animation_open(&self) -> bool {
        self.animation.open
    }

    /// The title the Animation window shows.
    #[must_use]
    pub const fn animation_title(&self) -> Option<InputId> {
        self.animation.input
    }

    /// The draft of `input`'s animation, if the window made one.
    #[must_use]
    pub fn animation_binding(&self, input: InputId) -> Option<&TitleAnimationBinding> {
        self.animation.bindings.get(&input)
    }

    /// The draft of `input`'s animation, for hosts and tests that drive
    /// the editor directly.
    pub fn animation_binding_mut(&mut self, input: InputId) -> Option<&mut TitleAnimationBinding> {
        self.animation.bindings.get_mut(&input)
    }

    /// Whether the canvas shows an exact preview of `input`.
    #[must_use]
    pub fn animation_preview_shown(&self, input: InputId) -> bool {
        self.animation
            .texture
            .as_ref()
            .is_some_and(|shown| shown.input == input)
    }

    /// Gives the editor `input`'s design (canvas and element outlines):
    /// every element is listed with local presets, and the canvas draws the
    /// wireframe proxy and onion skins.
    pub fn set_title_design(&mut self, input: InputId, design: TitleDesign) {
        if let Some(binding) = self.animation.bindings.get_mut(&input) {
            binding.set_design(&design);
        }
        self.animation.designs.insert(input, design);
    }

    /// The design the editor has for `input`, if any.
    #[must_use]
    pub fn title_design(&self, input: InputId) -> Option<&TitleDesign> {
        self.animation.designs.get(&input)
    }

    /// Whether the Animation window is docked along the bottom of the
    /// console.
    #[must_use]
    pub const fn animation_docked(&self) -> bool {
        self.layout.animation_docked
    }

    /// Forgets animation saves whose answers will not arrive (the worker
    /// stopped or never got them); their drafts stay and are sent again.
    pub fn abandon_animation_requests(&mut self) {
        for binding in self.animation.bindings.values_mut() {
            binding.abandon_in_flight();
        }
        self.animation.preset_pending = None;
    }

    /// Records that the exact preview `request_id` never reached the
    /// worker.
    pub fn title_preview_unsent(&mut self, request_id: &str) {
        for binding in self.animation.bindings.values_mut() {
            binding.preview_failed(request_id);
        }
    }

    /// `Ctrl+Shift+A` opens or closes the Animation window (on the title
    /// selected in the Titles tab) unless a text field or a dialog owns the
    /// keyboard.
    pub(crate) fn toggle_animation_on_key(&mut self, ui: &mut Ui) {
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
        if ui.input_mut(|input| input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::A)) {
            if self.animation.open {
                self.animation.open = false;
                self.animation.timeline.keyboard_active = false;
            } else {
                let selected = self.titles.selected();
                self.open_animation(selected);
            }
        }
    }

    /// Whether the animation timeline owns the keyboard (its single-key
    /// shortcuts must not reach the console).
    pub(crate) fn animation_owns_keyboard(&self) -> bool {
        self.animation.open && self.animation.timeline.keyboard_active
    }

    /// Draws the Colour and Animation windows.
    pub(crate) fn draw_tool_windows(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.draw_color_window(context, state, intents);
        self.draw_animation_window(context, state, intents);
    }

    /// Follows the authority for every draft and, when the window is open
    /// and docked, draws it as a panel along the bottom of the console at
    /// the layout's height (call before the central panel).
    pub(crate) fn draw_animation_dock(&mut self, ui: &mut Ui, state: &StudioUiState) {
        let context = ui.ctx().clone();
        self.animation.sync(&context, state);
        if !(self.animation.open && self.layout.animation_docked) {
            if self.animation.resize == Some(ResizeDrag::Dock) {
                self.animation.resize = None;
            }
            return;
        }
        let min = f32::from(ConsoleLayout::MIN_ANIMATION_DOCK_HEIGHT);
        let max = (ui.available_height() * 0.8).max(min);
        let height = self
            .layout
            .animation_dock_height
            .map_or(DEFAULT_DOCK_HEIGHT, f32::from)
            .clamp(min, max);
        let id = egui::Id::new(DOCK_ID);
        // egui keeps the panel's own last height; outside a drag the
        // layout's height wins (a restored preference or a smaller window).
        if self.animation.resize != Some(ResizeDrag::Dock)
            && egui::PanelState::load(&context, id)
                .is_some_and(|panel| (panel.size().y - height).abs() > 0.5)
        {
            context.data_mut(|data| data.remove::<egui::PanelState>(id));
        }
        let mut open = true;
        egui::Panel::bottom(DOCK_ID)
            .resizable(true)
            .default_size(height)
            .min_size(min)
            .max_size(max)
            .frame(
                Frame::new()
                    .fill(raised_color())
                    .stroke(Stroke::new(
                        crate::theme::line_width(),
                        border_strong_color(),
                    ))
                    .inner_margin(Margin::same(10)),
            )
            .show(ui, |ui| {
                // The panel keeps the height it was given, not its content's.
                ui.set_min_size(ui.available_size());
                self.draw_animation_contents(ui, state, &mut open);
            });
        let dragging = context
            .read_response(id.with("__resize"))
            .is_some_and(|response| response.dragged());
        if self.animation.resize == Some(ResizeDrag::Dock)
            && !dragging
            && let Some(panel) = egui::PanelState::load(&context, id)
        {
            self.layout.record_animation_dock_height(panel.size().y);
        }
        self.animation.resize = dragging.then_some(ResizeDrag::Dock);
        if !open {
            self.close_animation();
        }
    }

    /// Closes the Animation window, ending any preset preview.
    fn close_animation(&mut self) {
        self.animation.open = false;
        self.animation.timeline.keyboard_active = false;
        self.animation.timeline.preset_hover = None;
        for binding in self.animation.bindings.values_mut() {
            binding.set_preset_hover(None, Duration::ZERO);
        }
    }

    /// Draws the floating Animation window when open and not docked, sends
    /// due saves of every draft, and asks for the exact preview of the
    /// shown draft.
    pub(crate) fn draw_animation_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let now = Duration::from_secs_f64(context.input(|input| input.time).max(0.0));
        if self.animation.open && !self.layout.animation_docked {
            let l = self.locale;
            let mut open = true;
            let viewport = context.viewport_rect();
            let min = Vec2::splat(f32::from(ConsoleLayout::MIN_ANIMATION_SIDE));
            let max = Vec2::new(viewport.width() - 24.0, viewport.height() - 60.0).max(min);
            let size = self
                .layout
                .animation_size
                .map_or(DEFAULT_WINDOW_SIZE, |(width, height)| {
                    Vec2::new(f32::from(width), f32::from(height))
                })
                .clamp(min, max);
            let shown = egui::Window::new(l.text(Msg::AnimWinSection))
                .id(egui::Id::new("studio-animation-window"))
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .default_size(size)
                .min_size(min)
                .max_size(max)
                .pivot(Align2::CENTER_TOP)
                .default_pos(pos2(viewport.center().x, 40.0))
                .frame(
                    Frame::new()
                        .fill(raised_color())
                        .stroke(Stroke::new(
                            crate::theme::line_width(),
                            border_strong_color(),
                        ))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(12)),
                )
                .show(context, |ui| {
                    let inner = ui.available_size();
                    // Fill the size the operator gave the window.
                    ui.set_min_size(inner);
                    self.draw_animation_contents(ui, state, &mut open);
                    inner
                });
            if let Some(inner) = shown.and_then(|shown| shown.inner) {
                self.track_animation_size(context, inner);
            }
            if !open {
                self.close_animation();
            }
        } else {
            self.animation.last_size = None;
            if self.animation.resize == Some(ResizeDrag::Window) {
                self.animation.resize = None;
            }
        }
        let presets = std::mem::take(&mut self.animation.timeline.preset_requests);
        if let Some(input) = self.animation.input
            && let Some(binding) = self.animation.bindings.get(&input)
            && animation_lock_reason(state, input).is_none()
            && self.animation.preset_pending.is_none()
            && !binding.document().is_dirty()
            && let Some(preset) = presets.last()
        {
            let base_revision = binding.authority().revision;
            self.animation.preset_pending = Some((input, base_revision));
            intents.push(StudioIntent::TitleAnimation(
                CommandPayload::ApplyTitleAnimationPreset {
                    input: WireInputId::from_domain(input),
                    base_revision,
                    element: preset.target,
                    phase: preset.phase,
                    preset: preset.preset,
                    frames: preset.frames,
                },
            ));
        }
        for (input, binding) in &mut self.animation.bindings {
            let lock = animation_lock_reason(state, *input);
            if let Some(payload) = binding.poll_save(lock.is_none()) {
                intents.push(StudioIntent::TitleAnimation(payload));
            }
        }
        if self.animation.open
            && let Some(input) = self.animation.input
            && let Some(binding) = self.animation.bindings.get_mut(&input)
        {
            if !binding.document().transport().is_playing() {
                binding.want_preview(now);
            }
            let allowed = preview_block_reason(state).is_none();
            if let Some(request) = binding.poll_preview(now, allowed) {
                intents.push(StudioIntent::TitlePreview(request));
            }
            if allowed && let Some(deadline) = binding.next_preview_deadline() {
                context.request_repaint_after(
                    deadline.saturating_sub(now).max(Duration::from_millis(10)),
                );
            }
        }
    }

    /// Records the floating window's size once a drag of its edges ends,
    /// so the host can remember it.
    fn track_animation_size(&mut self, context: &egui::Context, inner: Vec2) {
        let pointer_down = context.input(|input| input.pointer.any_down());
        if pointer_down
            && self
                .animation
                .last_size
                .is_some_and(|last| (last - inner).length() > 0.5)
        {
            self.animation.resize = Some(ResizeDrag::Window);
        }
        if self.animation.resize == Some(ResizeDrag::Window) && !pointer_down {
            self.layout.record_animation_size(inner);
            self.animation.resize = None;
        }
        self.animation.last_size = Some(inner);
    }

    #[allow(clippy::too_many_lines)] // one window: header, notices, canvas, timeline
    fn draw_animation_contents(&mut self, ui: &mut Ui, state: &StudioUiState, open: &mut bool) {
        let l = self.locale;
        let now = Duration::from_secs_f64(ui.input(|input| input.time).max(0.0));
        let titles = animatable_titles(l, state);
        let docked = &mut self.layout.animation_docked;
        let window = &mut self.animation;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(section_label(l.text(Msg::AnimWinSection)))
                .on_hover_text(l.text(Msg::AnimWinSectionHover));
            if !titles.is_empty() {
                let shown = window
                    .input
                    .and_then(|input| titles.iter().find(|(id, _)| *id == input))
                    .map_or_else(String::new, |(_, name)| name.clone());
                let mut picked = None;
                ComboBox::from_id_salt("studio-animation-title")
                    .selected_text(crate::theme::ellipsize(&shown, 28))
                    .width(210.0)
                    .show_ui(ui, |ui| {
                        for (input, name) in &titles {
                            if ui
                                .selectable_label(window.input == Some(*input), name)
                                .on_hover_text(l.format(
                                    Msg::AnimWinTitleOptionHover,
                                    &[("name", Arg::Str(name))],
                                ))
                                .clicked()
                            {
                                picked = Some(*input);
                            }
                        }
                    })
                    .response
                    .on_hover_text(l.text(Msg::AnimWinTitleHover));
                if let Some(input) = picked {
                    if let Some(previous) = window.input
                        && let Some(binding) = window.bindings.get_mut(&previous)
                    {
                        binding.set_preset_hover(None, now);
                    }
                    window.input = Some(input);
                    window.timeline.active_row = None;
                    window.timeline.clear_notice();
                }
            }
            if let Some(input) = window.input
                && let Some(binding) = window.bindings.get_mut(&input)
            {
                let lock = animation_lock_reason(state, input);
                let status = binding.status();
                let (text, hover, tone) = status_look(status);
                status_chip(ui, l.text(text), tone, 92.0).on_hover_text(l.text(hover));
                let mut auto = binding.auto_save();
                if ui
                    .checkbox(&mut auto, l.text(Msg::AnimAutoSave))
                    .on_hover_text(l.text(Msg::AnimAutoSaveHover))
                    .changed()
                {
                    binding.set_auto_save(auto);
                }
                let save_reason = lock.or(match status {
                    AnimationSaveStatus::Saved => Some(Msg::AnimReasonSaved.en()),
                    AnimationSaveStatus::Saving => Some(Msg::AnimReasonSaving.en()),
                    AnimationSaveStatus::Conflict => Some(Msg::AnimReasonConflict.en()),
                    AnimationSaveStatus::Unsaved | AnimationSaveStatus::Refused => None,
                });
                if ui
                    .add_enabled(
                        save_reason.is_none(),
                        small_button(l.text(Msg::TitleOpSave)),
                    )
                    .hint(l.text(Msg::AnimSaveHover), save_reason.unwrap_or(""))
                    .clicked()
                {
                    binding.request_save();
                }
                let dirty = binding.document().is_dirty() || status != AnimationSaveStatus::Saved;
                let revert_reason = (!dirty).then_some(Msg::AnimReasonSaved.en()).or_else(|| {
                    binding
                        .document()
                        .drag()
                        .is_some()
                        .then_some(Msg::AnimReasonDrag.en())
                });
                if ui
                    .add_enabled(
                        revert_reason.is_none(),
                        small_button(l.text(Msg::ColorRevert)),
                    )
                    .hint(l.text(Msg::AnimRevertHover), revert_reason.unwrap_or(""))
                    .clicked()
                {
                    binding.reload_theirs();
                    window.timeline.clear_notice();
                }
                ui.checkbox(&mut window.onion, l.text(Msg::AnimOnion))
                    .on_hover_text(l.text(Msg::AnimOnionHover));
            }
            ui.separator();
            let (dock_text, dock_hover) = if *docked {
                (Msg::AnimFloat, Msg::AnimFloatHover)
            } else {
                (Msg::AnimDock, Msg::AnimDockHover)
            };
            if ui
                .add(small_button(l.text(dock_text)))
                .on_hover_text(l.text(dock_hover))
                .clicked()
            {
                *docked = !*docked;
            }
            if ui
                .add(close_key(14.0))
                .on_hover_text(l10n::with_key(
                    l,
                    l.text(Msg::AnimClose),
                    ANIMATION_SHORTCUT,
                ))
                .clicked()
            {
                *open = false;
            }
        });
        let Some(input) = window.input.filter(|_| !titles.is_empty()) else {
            crate::theme::empty_state(
                ui,
                l.text(Msg::AnimWinNoTitles),
                l.text(Msg::AnimWinNoTitlesDetail),
            );
            return;
        };
        let lock = animation_lock_reason(state, input);
        if let Some(reason) = lock {
            ui.label(caption(l10n::unavailable(l, reason)).color(AMBER));
        }
        let Some(binding) = window.bindings.get_mut(&input) else {
            return;
        };
        if binding.status() == AnimationSaveStatus::Conflict {
            Frame::new()
                .fill(AMBER.gamma_multiply(0.10))
                .stroke(Stroke::new(1.0, AMBER.gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(l.text(Msg::AnimConflictBanner)).color(AMBER));
                        let busy = binding
                            .document()
                            .drag()
                            .is_some()
                            .then_some(Msg::AnimReasonDrag.en());
                        if ui
                            .add_enabled(
                                busy.is_none(),
                                small_button(l.text(Msg::AnimReloadTheirs)),
                            )
                            .hint(l.text(Msg::AnimReloadTheirsHover), busy.unwrap_or(""))
                            .clicked()
                        {
                            binding.reload_theirs();
                        }
                        let overwrite = lock.or(busy);
                        if ui
                            .add_enabled(
                                overwrite.is_none(),
                                small_button(l.text(Msg::AnimOverwrite)),
                            )
                            .hint(l.text(Msg::AnimOverwriteHover), overwrite.unwrap_or(""))
                            .clicked()
                        {
                            binding.overwrite();
                        }
                    });
                });
        }
        window.timeline.outside_presets = if binding.has_design() {
            None
        } else {
            ui.add(
                egui::Label::new(caption(l.text(Msg::AnimDesignUnavailable)).color(muted_color()))
                    .wrap(),
            );
            Some(if window.preset_pending.is_some() {
                Err(Msg::AnimReasonSaving.en())
            } else if binding.document().is_dirty()
                || binding.status() != AnimationSaveStatus::Saved
            {
                Err(Msg::AnimReasonServerPreset.en())
            } else {
                Ok(())
            })
        };
        let preview_reason = preview_block_reason(state);
        let texture = window
            .texture
            .as_ref()
            .filter(|shown| shown.input == input)
            .map(|shown| shown.texture.id());
        // The hovered preset's outlines play its motion until its exact
        // frame arrives, then rest on the frame the exact preview shows.
        let hover_exact = binding.hover_preview_current();
        if let Some(document) = binding.preset_hover_document_mut() {
            let frames = document.phase_frames();
            if hover_exact {
                document.set_playhead(frames / 2);
            } else {
                let rate = document.clip().timebase().rate();
                let fps = f64::from(rate.numerator()) / f64::from(rate.denominator().max(1));
                document.set_playhead(hover_frame(ui.input(|input| input.time), fps, frames));
                ui.ctx().request_repaint();
            }
        }
        let canvas_height = (ui.available_height()
            - (ui.available_height() * TIMELINE_SHARE).max(TIMELINE_MIN_HEIGHT))
        .max(CANVAS_MIN_HEIGHT);
        draw_canvas(
            ui,
            l,
            binding,
            &CanvasLook {
                texture,
                onion: window.onion,
                preview_reason,
                max_height: canvas_height,
            },
        );
        ui.add_space(4.0);
        window.timeline.lock_reason = lock;
        egui::ScrollArea::vertical()
            .id_salt("studio-animation-timeline")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                window.tracks = Some(
                    AnimationTimelineWidget::new(binding.document_mut(), &mut window.timeline)
                        .locale(l)
                        .show(ui)
                        .tracks,
                );
            });
        // Resting on a preset previews it (only where it would apply
        // locally); leaving the picker ends the preview.
        let hover = window
            .timeline
            .preset_hover
            .filter(|hover| binding.document().target(hover.target).is_some())
            .map(|hover| PresetHover {
                target: hover.target,
                phase: hover.phase,
                preset: hover.preset,
                frames: hover.frames,
            });
        binding.set_preset_hover(hover, now);
    }
}

/// What the canvas shows besides the draft.
#[derive(Clone, Copy)]
struct CanvasLook {
    texture: Option<egui::TextureId>,
    onion: bool,
    preview_reason: Option<&'static str>,
    max_height: f32,
}

/// The canvas: the exact preview when one exists, the wireframe proxy and
/// onion skins over it (filled while the exact frame is out of date, a
/// drag runs, or playback plays; outlines only over an exact frame), a
/// hovered preset's outlines playing its motion, a badge saying which is
/// shown, and the phase and frame.
#[allow(clippy::too_many_lines)] // one canvas: picture, overlay, badge, caption
fn draw_canvas(ui: &mut Ui, l: Locale, binding: &TitleAnimationBinding, look: &CanvasLook) {
    let CanvasLook {
        texture,
        onion,
        preview_reason,
        max_height,
    } = *look;
    let document = binding.document();
    let hovered = binding.preset_hover_document();
    let shown_document = hovered.unwrap_or(document);
    let size = canvas_size(document.canvas(), ui.available_width(), max_height);
    let (rect, response) = ui
        .vertical_centered(|ui| ui.allocate_exact_size(size, Sense::hover()))
        .inner;
    response.on_hover_text(l.text(Msg::AnimCanvasHover));
    let painter = ui.painter_at(rect);
    paint_checker(&painter, rect);
    let exact = if hovered.is_some() {
        binding.hover_preview_current()
    } else {
        binding.exact_preview_current()
    } && texture.is_some();
    // An exact frame of a preset no longer hovered is not the draft.
    let picture = texture.filter(|_| exact || !binding.shown_preview_is_hover());
    if let Some(texture) = picture {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            if exact {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(150)
            },
        );
    }
    let moving = document.drag().is_some() || document.transport().is_playing();
    if binding.has_design() {
        let settled = exact && !moving;
        let frames = shown_document.phase_frames();
        let onion_count = match (onion, hovered.is_some(), settled) {
            (false, _, _) => 0,
            (true, true, _) | (true, false, false) => 3,
            (true, false, true) => 2,
        };
        paint_document_overlay(
            &painter,
            rect,
            shown_document,
            OverlayOptions {
                onion_count,
                onion_step: if hovered.is_some() {
                    (frames / 6).max(1)
                } else {
                    OverlayOptions::default().onion_step
                },
                fill: !settled,
            },
        );
    } else if picture.is_none() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            l.text(Msg::AnimCanvasEmpty),
            FontId::proportional(crate::theme::legible(12.0)),
            text_dim_color(),
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, border_strong_color()),
        egui::StrokeKind::Outside,
    );
    // Which picture this is, top right.
    let badge = Rect::from_min_size(
        pos2(rect.right() - 138.0, rect.top() + 8.0),
        Vec2::new(130.0, 18.0),
    );
    let (text, tone) = if hovered.is_some() {
        (Msg::AnimBadgePreset, Tone::Pending)
    } else if exact && !moving {
        (Msg::AnimBadgeExact, Tone::Ready)
    } else {
        (Msg::AnimBadgeFast, Tone::Idle)
    };
    status_chip_painted(&painter, badge, l.text(text), tone);
    let badge_hover = match (binding.preset_hover(), exact && !moving, preview_reason) {
        (Some(hover), _, _) => {
            let name = document
                .target(hover.target)
                .map(|outline| outline.name.clone())
                .unwrap_or_default();
            l.format(
                Msg::AnimBadgePresetHover,
                &[
                    ("preset", Arg::Str(preset_name(l, hover.preset))),
                    ("name", Arg::Str(&name)),
                ],
            )
        }
        (None, true, _) => l.text(Msg::AnimBadgeExactHover).to_owned(),
        (None, false, None) => l.text(Msg::AnimBadgeFastHover).to_owned(),
        (None, false, Some(reason)) => l.format(
            Msg::AnimBadgeProxyHover,
            &[(
                "reason",
                Arg::Str(&l.format(
                    Msg::AnimPreviewUnavailable,
                    &[("reason", Arg::Str(l.tr(reason)))],
                )),
            )],
        ),
    };
    ui.interact(badge, ui.id().with("animation-badge"), Sense::hover())
        .on_hover_text(badge_hover);
    let phase = match shown_document.phase() {
        Phase::In => Msg::AnimPhaseIn,
        Phase::Out => Msg::AnimPhaseOut,
    };
    painter.text(
        pos2(rect.left() + 8.0, rect.bottom() - 8.0),
        Align2::LEFT_BOTTOM,
        l.format(
            Msg::AnimCanvasCaption,
            &[
                ("phase", Arg::Str(l.text(phase))),
                ("frame", Arg::from(shown_document.playhead())),
                ("total", Arg::from(shown_document.phase_frames())),
            ],
        ),
        FontId::monospace(crate::theme::legible(10.5)),
        Color32::from_white_alpha(200),
    );
}

#[cfg(test)]
mod tests;
