//! The Studio layer designer (SW-006): one scene's layers on its canvas,
//! moved, scaled, turned, cropped, masked, keyed, given effects, and
//! restacked by hand or by exact values.
//!
//! The window edits a [`SceneLayoutDocument`] per project scene, built from
//! the replicated layout (`scene_layout_state`, followed by every
//! `scene_layout_changed`, Protocol 2.39): geometry with free rotation and
//! anchor, crop, mask, chroma or luma key, effect list (Protocol 2.43), and
//! z-order. The canvas shows the server's exact
//! composition of the scene ([`StudioIntent::ScenePreview`], paced by a
//! [`PreviewPacer`] and held back while a drag runs) under the layer
//! outlines, handles, the anchor mark, safe-area marks, and snap guides.
//!
//! Edits are live ([`fm_ui_model::scene_designer::LIVE_LAYER_EDITS`]):
//! while a drag or field scrub runs the window sends the changed layer
//! properties as `scene_layer_*` commands ([`StudioIntent::SceneLayer`])
//! at most every [`LIVE_SEND_INTERVAL`], and the rest on release; the
//! later sends of one gesture continue its first, so the show's undo
//! (the window's Undo and Redo, the engine's shared history) takes back a
//! whole gesture of one layer at once. "Copy as commands" stays as a
//! secondary export: the scene's whole layout as `freemix-cli
//! scene-layer-*` commands. Padlocks and the `edit_project` permission
//! gate the edits exactly as the server does, with the reason on every
//! disabled control.

use core::time::Duration;
use std::collections::BTreeMap;

use egui::{
    Align, Align2, Color32, ColorImage, ComboBox, CornerRadius, CursorIcon, DragValue, Event,
    FontId, Key, Layout, Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2, pos2, vec2,
};
use fm_protocol::{
    TitlePreviewImage, WireInputId, WireKeyDespill, WireLayerKey, WireSceneLayerId,
    WireSceneLayerSource,
};
use fm_types::{InputId, SceneId};
use fm_ui_model::auto_key::{AutoKeyError, AutoKeyPicture, AutoKeySample, auto_key};
use fm_ui_model::scene_designer::geometry::{Corner, Edge, Handle, HitTarget, Point};
use fm_ui_model::scene_designer::gesture::{
    CanvasMetrics, GestureFeedback, GestureModifiers, Readout,
};
use fm_ui_model::scene_designer::preview::{KeyPicturePacer, PreviewPacer, PreviewSubject};
use fm_ui_model::scene_designer::snapping::{
    ACTION_SAFE, Axis, SNAP_DISTANCE_POINTS, SnapSettings, TITLE_SAFE, safe_area,
};
use fm_ui_model::scene_designer::{
    DesignLayer, EditLabel, Field, MAX_SCENE_LAYER_EFFECTS, SCENE_LAYER_EFFECT_KINDS,
    SceneLayoutDocument, effect_ranges, layout_stamp,
};
use fm_ui_model::title_animation::preview_size;

use crate::dock::ToolWindow;
use crate::edit_history::{INPUT_LOCKED_REASON, PROJECT_LOCKED_REASON, edit_history_availability};
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_thumbnail_ui::ThumbnailTextures;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, KeyIcon, MONITOR_BLACK, Tone, accent_color, add_icon_button, border_strong_color,
    caption, field_label, muted_color, section_label, small_button, status_chip, text_color,
    text_dim_color,
};
use crate::toasts::ToastKind;
use crate::{StudioIntent, StudioShell, StudioUiState, workspace};

/// The shortcut that opens and closes the layer designer.
pub const SCENE_DESIGNER_SHORTCUT: &str = "Ctrl+Shift+L";
/// The window's content size until the operator resizes it.
const DEFAULT_WINDOW_SIZE: Vec2 = Vec2::new(1_440.0, 760.0);
/// Smallest window content size.
const MIN_WINDOW_SIZE: Vec2 = Vec2::new(760.0, 420.0);
/// Width of the layer list column.
const LIST_WIDTH: f32 = 200.0;
/// Width of the inspector column.
const INSPECTOR_WIDTH: f32 = 262.0;
/// Height of one layer list row.
const ROW_HEIGHT: f32 = 26.0;
/// Drawn handle half-size, in points.
const HANDLE_SIZE: f32 = 4.5;
/// Handle grab distance, in points.
const HANDLE_GRAB: f32 = 9.0;
/// How far above a layer's top edge its rotate handle stands, in points.
const ROTATE_OFFSET: f32 = 26.0;
/// The project placeholder in copied commands when the show's bundle path
/// is not known (Studio connected to an existing daemon).
const PROJECT_PLACEHOLDER: &str = "<show.freemix>";
/// Snap guide colour.
const GUIDE: Color32 = Color32::from_rgb(236, 72, 200);
/// Dark translucent plate under canvas text (layer names, the picture badge).
const LABEL_BACKING: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 190);
/// Mask outline colour.
const MASK: Color32 = Color32::from_rgb(90, 210, 230);
/// Garbage matte outline and point colour.
const MATTE: Color32 = Color32::from_rgb(130, 230, 120);
/// Anchor mark colour.
const ANCHOR: Color32 = Color32::from_rgb(255, 196, 64);
/// Live edits of a running drag or scrub are sent at most this often.
pub const LIVE_SEND_INTERVAL: Duration = Duration::from_millis(80);
/// How long after its last send an idle draft takes the server's layout
/// again even without a replicated change (a refused edit leaves none).
const SETTLE_AFTER: Duration = Duration::from_millis(1_500);

/// The exact picture shown on the canvas and the scene it shows.
struct DesignTexture {
    texture: TextureHandle,
    scene: SceneId,
}

/// The layer designer's state, next to the console.
#[derive(Default)]
pub(crate) struct DesignerWindow {
    pub(crate) open: bool,
    /// The scene shown.
    pub(crate) scene: Option<SceneId>,
    /// One draft per project scene that has been shown.
    pub(crate) documents: BTreeMap<SceneId, SceneLayoutDocument>,
    pub(crate) snapping: SnapSettings,
    pacer: PreviewPacer,
    /// The selected layer's input picture auto key samples (SW-008).
    key_picture: KeyPicturePacer,
    texture: Option<DesignTexture>,
    seen_image: Option<String>,
    seen_failure: Option<String>,
    /// Guides and readout of the running gesture.
    pub(crate) feedback: GestureFeedback,
    /// A layer list row being dragged to restack.
    list_drag: Option<WireSceneLayerId>,
    /// The project bundle the copied commands name.
    pub(crate) project_path: Option<String>,
    /// Whether the canvas has keyboard focus (arrow keys nudge).
    pub(crate) canvas_focused: bool,
    /// Where the canvas and the layer list rows were laid out last frame.
    pub(crate) canvas_rect: Option<Rect>,
    pub(crate) list_rows: Vec<(WireSceneLayerId, Rect)>,
    /// When live edits were last sent.
    last_send: Option<Duration>,
    /// Whether a click on the KEY section's input picture picks the key
    /// colour (SW-008).
    pub(crate) eyedropper: bool,
    /// What the last auto key did, for the console's toasts.
    notice: Option<(ToastKind, String)>,
    /// Where the Auto key button and the input picture were laid out last
    /// frame.
    pub(crate) auto_key_rects: (Option<Rect>, Option<Rect>),
    /// The garbage matte point being dragged on the canvas (SW-008).
    matte_drag: Option<u8>,
}

/// Clones what the console compares and the drafts; the picture and the
/// preview pacing start over.
impl Clone for DesignerWindow {
    fn clone(&self) -> Self {
        Self {
            open: self.open,
            scene: self.scene,
            documents: self.documents.clone(),
            snapping: self.snapping,
            project_path: self.project_path.clone(),
            ..Self::default()
        }
    }
}

impl core::fmt::Debug for DesignerWindow {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DesignerWindow")
            .field("open", &self.open)
            .field("scene", &self.scene)
            .field("drafts", &self.documents.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for DesignerWindow {
    fn eq(&self, other: &Self) -> bool {
        self.open == other.open && self.scene == other.scene && self.snapping == other.snapping
    }
}

impl Eq for DesignerWindow {}

/// Why `scene`'s layout cannot be edited now (catalog English): the
/// session gates, then the project padlock, then the padlock of any Scene
/// input showing the scene. These are the gates the scene layer edit
/// commands would have.
pub(crate) fn design_lock_reason(state: &StudioUiState, scene: SceneId) -> Option<&'static str> {
    if !state.connection_status.controls_enabled()
        || state.view.is_none()
        || !state.can_edit_project
    {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    let locks = &state.view.as_ref()?.edit_locks;
    if locks.project {
        return Some(PROJECT_LOCKED_REASON);
    }
    let shown_by_locked = state.project_state.as_ref().is_some_and(|project| {
        project
            .scenes()
            .iter()
            .any(|shown| shown.scene == Some(scene) && locks.is_input_locked(shown.input))
    });
    shown_by_locked.then_some(INPUT_LOCKED_REASON)
}

/// Why no exact picture can be asked for (catalog English).
fn picture_block_reason(state: &StudioUiState, scene: SceneId) -> Option<&'static str> {
    if let Some(reason) = crate::studio_animation::preview_block_reason(state) {
        return Some(reason);
    }
    scene_input(state, scene)
        .is_none()
        .then_some(Msg::DesignReasonNoSceneInput.en())
}

/// The first Scene input showing `scene`.
fn scene_input(state: &StudioUiState, scene: SceneId) -> Option<InputId> {
    state.project_state.as_ref().and_then(|project| {
        project
            .scenes()
            .iter()
            .find(|shown| shown.scene == Some(scene))
            .map(|shown| shown.input)
    })
}

/// Every input that can be a key/fill key's key input (SW-008), with its
/// display name, in show order: the show's inputs except Scene inputs.
fn key_input_choices(state: &StudioUiState) -> Vec<(InputId, String)> {
    let Some(view) = state.view.as_ref() else {
        return Vec::new();
    };
    let scene_inputs: Vec<InputId> = state
        .project_state
        .as_ref()
        .map_or_else(Vec::new, |project| {
            project.scenes().iter().map(|shown| shown.input).collect()
        });
    view.inputs
        .iter()
        .zip(&view.input_names)
        .filter(|(input, _)| !scene_inputs.contains(input))
        .map(|(input, name)| (*input, name.clone()))
        .collect()
}

/// Every project scene with its display name, in project order.
fn scene_choices(l: Locale, state: &StudioUiState) -> Vec<(SceneId, String)> {
    state
        .project_state
        .as_ref()
        .map_or_else(Vec::new, |project| {
            project
                .scene_layouts()
                .iter()
                .map(|entry| (entry.scene, scene_name(l, entry.scene, &entry.name)))
                .collect()
        })
}

fn scene_name(l: Locale, scene: SceneId, name: &str) -> String {
    if name.trim().is_empty() {
        l.format(
            Msg::DesignSceneFallback,
            &[("id", Arg::Str(&scene.to_string()))],
        )
    } else {
        name.to_owned()
    }
}

fn layer_name(l: Locale, layer: &DesignLayer) -> String {
    if layer.name.trim().is_empty() {
        l.format(
            Msg::DesignLayerUnnamed,
            &[("id", Arg::Str(&layer.id.to_string()))],
        )
    } else {
        layer.name.clone()
    }
}

/// What a layer draws, in words.
fn source_text(l: Locale, state: &StudioUiState, source: WireSceneLayerSource) -> String {
    match source {
        WireSceneLayerSource::Input(input) => {
            let input = input.to_domain();
            let name = state
                .view
                .as_ref()
                .and_then(|view| {
                    let index = view.inputs.iter().position(|id| *id == input)?;
                    Some(view.input_names[index].clone())
                })
                .unwrap_or_else(|| input.to_string());
            l.format(Msg::DesignSourceInput, &[("name", Arg::Str(&name))])
        }
        WireSceneLayerSource::Scene(scene) => {
            let scene = scene.to_domain();
            let name = state
                .project_state
                .as_ref()
                .and_then(|project| project.scene_layout(scene))
                .map_or_else(
                    || scene.to_string(),
                    |entry| scene_name(l, scene, &entry.name),
                );
            l.format(Msg::DesignSourceScene, &[("name", Arg::Str(&name))])
        }
    }
}

const fn edge_text(edge: Edge) -> Msg {
    match edge {
        Edge::Left => Msg::DesignEdgeLeft,
        Edge::Top => Msg::DesignEdgeTop,
        Edge::Right => Msg::DesignEdgeRight,
        Edge::Bottom => Msg::DesignEdgeBottom,
    }
}

/// The gesture modifiers held now.
fn modifiers(ui: &Ui) -> GestureModifiers {
    let held = ui.input(|input| input.modifiers);
    GestureModifiers {
        shift: held.shift,
        alt: held.alt,
        command: held.command || held.ctrl,
    }
}

impl DesignerWindow {
    /// Follows the replicated layouts and takes this window's pictures.
    pub(crate) fn sync(&mut self, context: &egui::Context, state: &StudioUiState, now: Duration) {
        let Some(project) = state.project_state.as_ref() else {
            return;
        };
        let scenes: Vec<SceneId> = project
            .scene_layouts()
            .iter()
            .map(|entry| entry.scene)
            .collect();
        if project.scene_layouts_complete() {
            self.documents.retain(|scene, _| scenes.contains(scene));
        }
        for entry in project.scene_layouts() {
            if let Some(layout) = entry.layout.as_deref()
                && let Some(document) = self.documents.get_mut(&entry.scene)
            {
                document.rebase(layout);
            }
        }
        if self.open {
            if !self.scene.is_some_and(|scene| scenes.contains(&scene)) {
                self.scene = scenes.first().copied();
            }
            if let Some(scene) = self.scene
                && let Some(layout) = project
                    .scene_layout(scene)
                    .and_then(|entry| entry.layout.as_deref())
            {
                self.documents.entry(scene).or_insert_with(|| {
                    SceneLayoutDocument::new(fm_protocol::WireSceneId::from_domain(scene), layout)
                });
            }
        }
        if let Some(failed) = &state.title_preview_failed
            && self.seen_failure.as_ref() != Some(failed)
        {
            self.seen_failure = Some(failed.clone());
            self.pacer.failed(failed, now);
            self.key_picture.failed(failed, now);
        }
        if let Some(image) = &state.title_preview
            && self.seen_image.as_ref() != Some(&image.request_id)
        {
            self.seen_image = Some(image.request_id.clone());
            if !self.key_picture.received(image) {
                self.receive_image(context, state, image);
            }
        }
    }

    fn receive_image(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        image: &TitlePreviewImage,
    ) {
        if !self.pacer.received(&image.request_id) {
            return;
        }
        let input = image.input.to_domain();
        let Some(scene) = state
            .project_state
            .as_ref()
            .and_then(|project| project.scene(input))
            .and_then(|shown| shown.scene)
        else {
            return;
        };
        let size = [usize::from(image.width), usize::from(image.height)];
        let picture = ColorImage::from_rgba_unmultiplied(size, &image.rgba);
        match &mut self.texture {
            Some(shown) => {
                shown.texture.set(picture, TextureOptions::LINEAR);
                shown.scene = scene;
            }
            None => {
                self.texture = Some(DesignTexture {
                    texture: context.load_texture(
                        "studio-designer-preview",
                        picture,
                        TextureOptions::LINEAR,
                    ),
                    scene,
                });
            }
        }
    }

    /// What the canvas wants pictured now.
    fn preview_subject(&self, state: &StudioUiState) -> Option<PreviewSubject> {
        let scene = self.scene?;
        let layout = state
            .project_state
            .as_ref()?
            .scene_layout(scene)?
            .layout
            .as_deref()?;
        Some(PreviewSubject {
            input: scene_input(state, scene)?,
            layout_stamp: layout_stamp(layout),
            size: preview_size((layout.width, layout.height)),
        })
    }

    /// The input the single selected layer draws, whose picture auto key
    /// samples.
    fn key_picture_input(&self) -> Option<InputId> {
        match self.document()?.single_selection()?.source {
            WireSceneLayerSource::Input(input) => Some(input.to_domain()),
            WireSceneLayerSource::Scene(_) => None,
        }
    }

    /// The draft shown.
    pub(crate) fn document(&self) -> Option<&SceneLayoutDocument> {
        self.documents.get(&self.scene?)
    }

    /// The draft shown, for editing.
    pub(crate) fn document_mut(&mut self) -> Option<&mut SceneLayoutDocument> {
        self.documents.get_mut(&self.scene?)
    }

    /// Sends what the drafts changed as live scene layer edits: at once
    /// when no drag or scrub is open, else at most every
    /// [`LIVE_SEND_INTERVAL`]. A draft the session may not edit is put
    /// back to the server's layout instead. An idle draft with nothing to
    /// send settles on the server's layout [`SETTLE_AFTER`] its last send.
    pub(crate) fn send_live_edits(
        &mut self,
        state: &StudioUiState,
        now: Duration,
        intents: &mut Vec<StudioIntent>,
    ) {
        let due = self
            .last_send
            .is_none_or(|last| now.saturating_sub(last) >= LIVE_SEND_INTERVAL);
        let settle = self
            .last_send
            .is_none_or(|last| now.saturating_sub(last) >= SETTLE_AFTER);
        let mut sent = false;
        for (scene, document) in &mut self.documents {
            let open = document.gesture_kind().is_some() || document.editing();
            if !document.has_unsent() {
                if !open && settle {
                    document.settle();
                }
                continue;
            }
            if design_lock_reason(state, *scene).is_some() {
                if !open {
                    document.revert();
                }
                continue;
            }
            if open && !due {
                continue;
            }
            for edit in document.take_live_edits() {
                intents.push(StudioIntent::SceneLayer(edit));
                sent = true;
            }
        }
        if sent {
            self.last_send = Some(now);
        }
    }

    /// The picture's texture when it shows `scene`.
    fn texture_for(&self, scene: SceneId) -> Option<egui::TextureId> {
        self.texture
            .as_ref()
            .filter(|shown| shown.scene == scene)
            .map(|shown| shown.texture.id())
    }
}

impl StudioShell {
    /// Opens the layer designer on `scene`, or on its last scene.
    pub(crate) fn open_designer(&mut self, scene: Option<SceneId>) {
        self.designer.open = true;
        if scene.is_some() {
            self.designer.scene = scene;
        }
    }

    /// Opens the layer designer on `scene` (for hosts and tests).
    pub fn open_scene_designer(&mut self, scene: SceneId) {
        self.open_designer(Some(scene));
    }

    /// Opens the layer designer on the scene Scene input `input` shows.
    pub fn open_scene_designer_for_input(&mut self, state: &StudioUiState, input: InputId) {
        let scene = state
            .project_state
            .as_ref()
            .and_then(|project| project.scene(input))
            .and_then(|shown| shown.scene);
        self.open_designer(scene);
    }

    /// Whether the layer designer is open.
    #[must_use]
    pub const fn scene_designer_open(&self) -> bool {
        self.designer.open
    }

    /// The scene the layer designer shows.
    #[must_use]
    pub const fn scene_designer_scene(&self) -> Option<SceneId> {
        self.designer.scene
    }

    /// The layer designer's draft of `scene`, once it has been shown.
    #[must_use]
    pub fn scene_layout_draft(&self, scene: SceneId) -> Option<&SceneLayoutDocument> {
        self.designer.documents.get(&scene)
    }

    /// The layer designer's draft of `scene`, for hosts and tests.
    pub fn scene_layout_draft_mut(&mut self, scene: SceneId) -> Option<&mut SceneLayoutDocument> {
        self.designer.documents.get_mut(&scene)
    }

    /// Names the project bundle the designer's copied commands edit (the
    /// supervised show); without one they name a placeholder.
    pub fn set_project_path(&mut self, path: Option<String>) {
        self.designer.project_path = path;
    }

    /// `Ctrl+Shift+L` opens or closes the layer designer unless a text
    /// field or a dialog owns the keyboard (the focused canvas does not).
    pub(crate) fn toggle_designer_on_key(&mut self, ui: &mut Ui) {
        let context = ui.ctx().clone();
        if (context.egui_wants_keyboard_input() && !self.designer.canvas_focused)
            || self.window_keys_blocked()
        {
            return;
        }
        if ui.input_mut(|input| input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::L)) {
            self.designer.open = !self.designer.open;
            self.designer.canvas_focused = false;
        }
    }

    /// Whether the designer canvas owns the keyboard (arrow keys nudge,
    /// Esc cancels a drag).
    pub(crate) fn designer_owns_keyboard(&self) -> bool {
        self.designer.open && self.designer.canvas_focused
    }

    /// Records that the designer's preview `request_id` never reached the
    /// worker.
    pub(crate) fn designer_preview_unsent(&mut self, request_id: &str, now: Duration) {
        self.designer.pacer.failed(request_id, now);
        self.designer.key_picture.failed(request_id, now);
    }

    /// Forgets the designer's preview in flight (the worker stopped).
    pub(crate) fn abandon_designer_requests(&mut self) {
        self.designer.pacer.abandon();
        self.designer.key_picture.abandon();
    }

    /// Draws the layer designer when open and asks for its pictures.
    #[allow(clippy::too_many_lines)] // one window: popped out or floating, then its sends
    pub(crate) fn draw_designer_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let now = Duration::from_secs_f64(context.input(|input| input.time).max(0.0));
        self.designer.sync(context, state, now);
        if !self.designer.open {
            self.designer.canvas_focused = false;
            // A drag or scrub cut short by closing is abandoned (and what
            // it already sent is put back).
            for document in self.designer.documents.values_mut() {
                if document.gesture_kind().is_some() || document.editing() {
                    document.cancel();
                }
            }
            self.designer.feedback = GestureFeedback::default();
            self.designer.send_live_edits(state, now, intents);
            return;
        }
        let l = self.locale;
        let mut open = true;
        // Inside the work area, never over the top bar.
        let area = workspace::tool_area(self, context);
        let max = Vec2::new(area.width() - 32.0, area.height() - 24.0).max(MIN_WINDOW_SIZE);
        let close_hover = l10n::with_key(l, l.text(Msg::DesignClose), SCENE_DESIGNER_SHORTCUT);
        let mut copied = None;
        let mut history = Vec::new();
        if let Some(geometry) = self.layout.tool_popped(ToolWindow::Designer) {
            let (id, builder) = self.tool_viewport(ToolWindow::Designer, geometry);
            let designer = &mut self.designer;
            let thumbnails = &self.thumbnail_textures;
            let (outcome, close) = context.show_viewport_immediate(id, builder, |ui, class| {
                workspace::tool_popout_frame(
                    ui,
                    l,
                    ToolWindow::Designer,
                    class,
                    &close_hover,
                    |ui| {
                        ui.set_min_size(ui.available_size());
                        copied = draw_contents(ui, l, designer, state, thumbnails, &mut history);
                    },
                )
            });
            open &= !close;
            self.finish_tool_popout(ToolWindow::Designer, outcome);
        } else {
            let shown = egui::Window::new(l.text(Msg::DesignTitle))
                .id(egui::Id::new("studio-designer-window"))
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .default_size(DEFAULT_WINDOW_SIZE.min(max))
                .min_size(MIN_WINDOW_SIZE)
                .max_size(max)
                .constrain_to(area)
                .pivot(Align2::CENTER_TOP)
                .default_pos(pos2(area.center().x, area.top() + 4.0))
                .frame(workspace::tool_window_frame())
                .show(context, |ui| {
                    ui.set_min_size(ui.available_size());
                    workspace::floating_tool_frame(
                        ui,
                        l,
                        ToolWindow::Designer,
                        &close_hover,
                        |ui| {
                            copied = draw_contents(
                                ui,
                                l,
                                &mut self.designer,
                                state,
                                &self.thumbnail_textures,
                                &mut history,
                            );
                        },
                    )
                    .0
                });
            if let Some(header) = shown.and_then(|shown| shown.inner) {
                open &= !header.close;
                if header.pop_out {
                    self.layout.set_tool_popped(ToolWindow::Designer, true);
                }
            }
        }
        intents.extend(history);
        if let Some((kind, text)) = self.designer.notice.take() {
            self.toasts.push(kind, text);
        }
        if let Some(count) = copied {
            self.toasts.push(
                ToastKind::Success,
                l.format(Msg::DesignCopied, &[("count", Arg::from(count))]),
            );
        }
        if !open {
            self.designer.open = false;
            self.designer.canvas_focused = false;
            if let Some(document) = self.designer.document_mut() {
                document.cancel();
            }
        }
        self.designer.send_live_edits(state, now, intents);
        let window = &mut self.designer;
        if window
            .document()
            .is_some_and(|document| document.gesture_kind().is_some() || document.editing())
        {
            context.request_repaint_after(LIVE_SEND_INTERVAL);
        }
        let held = window
            .document()
            .is_some_and(|document| document.gesture_kind().is_some() || document.editing());
        let subject = window.preview_subject(state);
        let allowed = window
            .scene
            .is_some_and(|scene| picture_block_reason(state, scene).is_none());
        // The server keeps one preview per session: the canvas picture and
        // the key picture never fly together.
        if !window.key_picture.in_flight()
            && let Some(request) = window.pacer.poll(subject, now, held, allowed)
        {
            intents.push(StudioIntent::ScenePreview(request));
        }
        let key_input = window.key_picture_input();
        if let Some(size) = subject.map(|subject| subject.size)
            && let Some(request) = window.key_picture.poll(
                key_input,
                size,
                now,
                allowed && window.open && !held && !window.pacer.in_flight(),
            )
        {
            intents.push(StudioIntent::ScenePreview(request));
        }
        if allowed && window.open {
            context
                .request_repaint_after(window.pacer.next_due(now).max(Duration::from_millis(20)));
        }
    }
}

/// The window's contents; returns how many commands were copied, if the
/// operator copied the layout. Undo and Redo land in `history`.
#[allow(clippy::too_many_lines)] // one window: header, toolbar, banners, columns
fn draw_contents(
    ui: &mut Ui,
    l: Locale,
    window: &mut DesignerWindow,
    state: &StudioUiState,
    thumbnails: &ThumbnailTextures,
    history: &mut Vec<StudioIntent>,
) -> Option<usize> {
    let scenes = scene_choices(l, state);
    let mut copied = None;
    let lock = window
        .scene
        .and_then(|scene| design_lock_reason(state, scene));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(section_label(l.text(Msg::DesignTitle)))
            .on_hover_text(l.text(Msg::DesignTitleHover));
        if !scenes.is_empty() {
            scene_picker(ui, l, window, &scenes);
        }
        if let Some(document) = window.document() {
            status_and_history(ui, l, document, state, history);
        }
    });
    let Some(scene) = window.scene.filter(|_| !scenes.is_empty()) else {
        crate::theme::empty_state(
            ui,
            l.text(Msg::DesignNoScenes),
            l.text(Msg::DesignNoScenesDetail),
        );
        return None;
    };
    if !window.documents.contains_key(&scene) {
        ui.label(caption(l.text(Msg::DesignWaiting)).color(muted_color()));
        return None;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.toggle_value(&mut window.snapping.enabled, l.text(Msg::DesignSnap))
            .on_hover_text(l.text(Msg::DesignSnapHover));
        ui.toggle_value(
            &mut window.snapping.safe_areas,
            l.text(Msg::DesignSafeAreas),
        )
        .on_hover_text(l.text(Msg::DesignSafeAreasHover));
        ui.toggle_value(&mut window.snapping.layers, l.text(Msg::DesignSnapLayers))
            .on_hover_text(l.text(Msg::DesignSnapLayersHover));
        ui.separator();
        let project = window
            .project_path
            .clone()
            .unwrap_or_else(|| PROJECT_PLACEHOLDER.to_owned());
        let Some(document) = window.documents.get(&scene) else {
            return;
        };
        // The secondary export: the whole layout as offline commands.
        let empty = document.layers().is_empty();
        if ui
            .add_enabled(!empty, small_button(l.text(Msg::DesignCopyCli)))
            .hint(
                &l.format(Msg::DesignCopyCliHover, &[("project", Arg::Str(&project))]),
                Msg::DesignNoScenesDetail.en(),
            )
            .clicked()
        {
            ui.ctx().copy_text(document.layout_script(&project));
            copied = Some(document.layout_edits().len());
        }
    });
    if window
        .documents
        .get(&scene)
        .is_some_and(SceneLayoutDocument::base_changed)
    {
        ui.add(egui::Label::new(caption(l.text(Msg::DesignBaseChanged)).color(AMBER)).wrap());
    }
    if let Some(reason) = lock {
        ui.label(caption(l10n::unavailable(l, reason)).color(AMBER));
    }
    ui.add_space(4.0);
    let texture = window.texture_for(scene);
    let picture_reason = picture_block_reason(state, scene).or_else(|| {
        texture
            .is_none()
            .then_some(Msg::DesignReasonWaitingPicture.en())
    });
    let height = ui.available_height().max(240.0);
    let key_inputs = key_input_choices(state);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(LIST_WIDTH, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(LIST_WIDTH);
                layer_list(ui, l, window, state, scene, lock);
            },
        );
        let canvas_width = (ui.available_width() - INSPECTOR_WIDTH - 12.0).max(240.0);
        ui.allocate_ui_with_layout(
            Vec2::new(canvas_width, height),
            Layout::top_down(Align::Center),
            |ui| {
                draw_canvas(
                    ui,
                    l,
                    window,
                    scene,
                    CanvasLook {
                        texture,
                        picture_reason,
                        lock,
                        max: Vec2::new(canvas_width, height),
                    },
                );
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(INSPECTOR_WIDTH, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(INSPECTOR_WIDTH);
                egui::ScrollArea::vertical()
                    .id_salt("studio-designer-inspector")
                    .show(ui, |ui| {
                        if let Some(document) = window.documents.get_mut(&scene) {
                            let mut sampler = KeySampler {
                                state,
                                thumbnails,
                                key_picture: &window.key_picture,
                                eyedropper: &mut window.eyedropper,
                                notice: &mut window.notice,
                                rects: &mut window.auto_key_rects,
                            };
                            inspector(ui, l, document, &key_inputs, &mut sampler, lock);
                        }
                    });
            },
        );
    });
    copied
}

fn scene_picker(ui: &mut Ui, l: Locale, window: &mut DesignerWindow, scenes: &[(SceneId, String)]) {
    let shown = window
        .scene
        .and_then(|scene| scenes.iter().find(|(id, _)| *id == scene))
        .map_or_else(String::new, |(_, name)| name.clone());
    let mut picked = window.scene;
    ComboBox::from_id_salt("studio-designer-scene")
        .selected_text(crate::theme::ellipsize(&shown, 28))
        .width(200.0)
        .show_ui(ui, |ui| {
            for (scene, name) in scenes {
                ui.selectable_value(&mut picked, Some(*scene), name)
                    .on_hover_text(
                        l.format(Msg::DesignSceneOptionHover, &[("name", Arg::Str(name))]),
                    );
            }
        })
        .response
        .on_hover_text(l.text(Msg::DesignScenePickHover));
    if picked != window.scene {
        if let Some(document) = window.document_mut() {
            document.cancel();
        }
        window.scene = picked;
        window.feedback = GestureFeedback::default();
    }
}

/// The live status chip and the show's Undo and Redo (the engine's shared
/// history: a whole gesture of one layer is one step).
fn status_and_history(
    ui: &mut Ui,
    l: Locale,
    document: &SceneLayoutDocument,
    state: &StudioUiState,
    history: &mut Vec<StudioIntent>,
) {
    let sending = document.has_unsent() || document.gesture_kind().is_some() || document.editing();
    if sending {
        status_chip(ui, l.text(Msg::DesignStatusSending), Tone::Pending, 140.0)
            .on_hover_text(l.text(Msg::DesignStatusSendingHover));
    } else {
        status_chip(ui, l.text(Msg::DesignStatusMatches), Tone::Ready, 140.0)
            .on_hover_text(l.text(Msg::DesignStatusMatchesHover));
    }
    let availability = edit_history_availability(state);
    if ui
        .add_enabled(
            availability.undo.expected_edit.is_some(),
            small_button(l.text(Msg::Undo)),
        )
        .hint(l.text(Msg::DesignUndoHover), availability.undo.reason)
        .clicked()
        && let Some(intent) = availability.undo_intent()
    {
        history.push(intent);
    }
    if ui
        .add_enabled(
            availability.redo.expected_edit.is_some(),
            small_button(l.text(Msg::Redo)),
        )
        .hint(l.text(Msg::DesignRedoHover), availability.redo.reason)
        .clicked()
        && let Some(intent) = availability.redo_intent()
    {
        history.push(intent);
    }
}

/// The layer list: top of the stack first, click to select, drag to
/// restack; the restack buttons below.
#[allow(clippy::too_many_lines)] // one list with its drag and buttons
fn layer_list(
    ui: &mut Ui,
    l: Locale,
    window: &mut DesignerWindow,
    state: &StudioUiState,
    scene: SceneId,
    lock: Option<&'static str>,
) {
    ui.label(section_label(l.text(Msg::DesignLayers)))
        .on_hover_text(l.text(Msg::DesignLayersHover));
    let Some(document) = window.documents.get_mut(&scene) else {
        return;
    };
    let count = document.layers().len();
    let held = modifiers(ui);
    let mut rows: Vec<(WireSceneLayerId, Rect)> = Vec::new();
    let mut clicked = None;
    let mut released = None;
    let rows_height = (ui.available_height() - 76.0).max(ROW_HEIGHT * 2.0);
    egui::ScrollArea::vertical()
        .id_salt("studio-designer-layers")
        .max_height(rows_height)
        .show(ui, |ui| {
            for layer in document.layers().iter().rev() {
                let position = document.index_of(layer.id).unwrap_or(0) + 1;
                let sense = if lock.is_none() {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                };
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), sense);
                let selected = document.is_selected(layer.id);
                let painter = ui.painter();
                if selected {
                    painter.rect_filled(rect, CornerRadius::same(3), crate::theme::ACCENT_FILL);
                } else if response.hovered() {
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(3),
                        crate::theme::widget_hover_color(),
                    );
                }
                let name = layer_name(l, layer);
                let color = if layer.enabled {
                    text_color()
                } else {
                    muted_color()
                };
                painter.text(
                    rect.left_center() + vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    crate::theme::ellipsize(&name, 22),
                    FontId::proportional(crate::theme::legible(12.0)),
                    color,
                );
                painter.text(
                    rect.right_center() - vec2(6.0, 0.0),
                    Align2::RIGHT_CENTER,
                    position.to_string(),
                    FontId::monospace(crate::theme::legible(10.0)),
                    text_dim_color(),
                );
                let hover = l.format(
                    Msg::DesignLayerRowHover,
                    &[
                        ("name", Arg::Str(&name)),
                        ("source", Arg::Str(&source_text(l, state, layer.source))),
                        ("position", Arg::from(position)),
                        ("count", Arg::from(count)),
                    ],
                );
                let response = match lock {
                    None => response.on_hover_text(hover),
                    Some(reason) => response.on_hover_text(l10n::disabled_text(l, &hover, reason)),
                };
                if response.clicked() {
                    clicked = Some(layer.id);
                }
                if response.drag_started() {
                    window.list_drag = Some(layer.id);
                }
                if response.drag_stopped() {
                    released = Some(layer.id);
                }
                rows.push((layer.id, rect));
            }
        });
    window.list_rows.clone_from(&rows);
    if let Some(id) = clicked {
        document.select(id, held.shift || held.command);
    }
    // The drop slot under the pointer, as a stacking index (0 = bottom).
    let pointer = ui.input(|input| input.pointer.interact_pos());
    let slot = |pointer: Pos2| -> Option<usize> {
        let first = rows.first()?.1;
        let last = rows.last()?.1;
        let shown = if pointer.y <= first.top() {
            0
        } else if pointer.y >= last.bottom() {
            rows.len() - 1
        } else {
            rows.iter()
                .position(|(_, rect)| pointer.y < rect.bottom())
                .unwrap_or(rows.len() - 1)
        };
        Some(rows.len() - 1 - shown)
    };
    if let (Some(dragged), Some(pointer)) = (window.list_drag, pointer)
        && let Some(target) = slot(pointer)
    {
        let shown = rows.len() - 1 - target;
        let rect = rows[shown].1;
        let from = document.index_of(dragged).unwrap_or(target);
        let y = if target >= from {
            rect.top()
        } else {
            rect.bottom()
        };
        ui.painter().line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(2.0, accent_color()),
        );
    }
    if let Some(id) = released {
        if lock.is_none()
            && let Some(target) = pointer.and_then(slot)
        {
            document.move_layer(id, target);
        }
        window.list_drag = None;
    }
    if !ui.input(|input| input.pointer.any_down()) {
        window.list_drag = None;
    }
    ui.add_space(4.0);
    let none = document.selection().is_empty();
    let reason = |up: bool| {
        lock.or_else(|| none.then_some(Msg::DesignReasonNoSelection.en()))
            .or_else(|| {
                (!document.can_shift_selection(up)).then_some(if up {
                    Msg::DesignReasonTop.en()
                } else {
                    Msg::DesignReasonBottom.en()
                })
            })
    };
    let (up_reason, down_reason) = (reason(true), reason(false));
    egui::Grid::new("studio-designer-stack")
        .num_columns(2)
        .spacing(Vec2::new(4.0, 4.0))
        .show(ui, |ui| {
            let buttons = [
                (Msg::DesignForward, Msg::DesignForwardHover, up_reason, 0),
                (
                    Msg::DesignBackward,
                    Msg::DesignBackwardHover,
                    down_reason,
                    1,
                ),
                (Msg::DesignToFront, Msg::DesignToFrontHover, up_reason, 2),
                (Msg::DesignToBack, Msg::DesignToBackHover, down_reason, 3),
            ];
            for (text, hover, blocked, action) in buttons {
                let response = ui
                    .add_enabled(
                        blocked.is_none(),
                        small_button(l.text(text)).min_size(Vec2::new(94.0, 22.0)),
                    )
                    .hint(l.text(hover), blocked.unwrap_or(""));
                if response.clicked() {
                    match action {
                        0 => document.shift_selection(true),
                        1 => document.shift_selection(false),
                        2 => document.send_selection_to_end(true),
                        _ => document.send_selection_to_end(false),
                    };
                }
                if action % 2 == 1 {
                    ui.end_row();
                }
            }
        });
}

/// How the canvas looks this frame.
#[derive(Clone, Copy)]
struct CanvasLook {
    texture: Option<egui::TextureId>,
    /// Why no exact picture is shown, if none is.
    picture_reason: Option<&'static str>,
    lock: Option<&'static str>,
    max: Vec2,
}

/// The canvas size for `canvas` within `max` points.
#[allow(clippy::cast_precision_loss)] // Canvas sizes are far below 2^24.
fn fit(canvas: (u32, u32), max: Vec2) -> Vec2 {
    let aspect = canvas.1.max(1) as f32 / canvas.0.max(1) as f32;
    let width = max.x.max(160.0);
    let height = width * aspect;
    if height > max.y {
        Vec2::new(max.y / aspect, max.y)
    } else {
        Vec2::new(width, height)
    }
}

/// Maps project pixels to screen points on a canvas `rect`.
#[derive(Clone, Copy)]
struct CanvasMap {
    rect: Rect,
    scale: f32,
}

impl CanvasMap {
    #[allow(clippy::cast_precision_loss)]
    fn new(rect: Rect, canvas: (u32, u32)) -> Self {
        Self {
            rect,
            scale: rect.width() / canvas.0.max(1) as f32,
        }
    }

    #[allow(clippy::cast_possible_truncation)] // Screen positions are small.
    fn screen(self, point: Point) -> Pos2 {
        self.rect.min + vec2(point.x as f32 * self.scale, point.y as f32 * self.scale)
    }

    fn canvas(self, pos: Pos2) -> Point {
        let local = (pos - self.rect.min) / self.scale;
        Point::new(f64::from(local.x), f64::from(local.y))
    }

    /// Screen points as canvas pixels.
    fn pixels(self, points: f32) -> f64 {
        f64::from(points / self.scale)
    }
}

/// The cursor a press at a hit target would show.
fn cursor_for(target: HitTarget, alt: bool) -> CursorIcon {
    match target {
        HitTarget::Handle(_, Handle::Rotate) => CursorIcon::Alias,
        HitTarget::Handle(_, Handle::Anchor | Handle::Edge(_)) if alt => CursorIcon::Crosshair,
        HitTarget::Body(_) | HitTarget::Handle(_, Handle::Anchor) => CursorIcon::Move,
        HitTarget::Handle(_, Handle::Edge(Edge::Left | Edge::Right)) => {
            CursorIcon::ResizeHorizontal
        }
        HitTarget::Handle(_, Handle::Edge(Edge::Top | Edge::Bottom)) => CursorIcon::ResizeVertical,
        HitTarget::Handle(_, Handle::Corner(Corner::TopLeft | Corner::BottomRight)) => {
            CursorIcon::ResizeNwSe
        }
        HitTarget::Handle(_, Handle::Corner(_)) => CursorIcon::ResizeNeSw,
    }
}

/// The canvas: the exact picture under the draft's outlines, handles,
/// safe areas, and guides, and the direct manipulation on it.
#[allow(clippy::too_many_lines)] // one canvas: input, then paint
fn draw_canvas(
    ui: &mut Ui,
    l: Locale,
    window: &mut DesignerWindow,
    scene: SceneId,
    look: CanvasLook,
) {
    let Some(document) = window.documents.get_mut(&scene) else {
        return;
    };
    let canvas = document.canvas();
    let size = fit(canvas, look.max);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    window.canvas_rect = Some(rect);
    let map = CanvasMap::new(rect, canvas);
    let metrics = CanvasMetrics {
        tolerance: map.pixels(HANDLE_GRAB),
        rotate_offset: map.pixels(ROTATE_OFFSET),
        snap_distance: map.pixels(
            #[allow(clippy::cast_possible_truncation)] // A small constant.
            {
                SNAP_DISTANCE_POINTS as f32
            },
        ),
    };
    let held = modifiers(ui);
    let at_pointer = response.interact_pointer_pos().or(response.hover_pos());
    // Esc abandons a running drag wherever the keyboard focus is (egui
    // ends the drag on Esc too, so this comes before the release).
    if document.gesture_kind().is_some()
        && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
    {
        document.cancel();
        window.feedback = GestureFeedback::default();
    }
    if look.lock.is_none() {
        if response.drag_started() {
            let origin = ui
                .input(|input| input.pointer.press_origin())
                .or(at_pointer)
                .unwrap_or(rect.center());
            window.matte_drag = matte_point_at(document, map.canvas(origin), metrics.tolerance);
            if let Some(point) = window.matte_drag {
                document.begin(EditLabel::Field(Field::MattePointX { point }));
            } else {
                document.press(map.canvas(origin), held, metrics, window.snapping);
            }
            response.request_focus();
        }
        if response.dragged()
            && let Some(point) = window.matte_drag
            && let Some(at) = at_pointer
        {
            drag_matte_point(document, point, map.canvas(at));
        }
        if response.dragged()
            && document.gesture_kind().is_some()
            && let Some(at) = at_pointer
        {
            window.feedback = document.drag(map.canvas(at), held);
        }
        if response.drag_stopped() {
            if window.matte_drag.take().is_some() {
                document.commit();
            } else {
                document.release();
            }
            window.feedback = GestureFeedback::default();
        }
        if response.clicked()
            && let Some(at) = at_pointer
        {
            document.press(map.canvas(at), held, metrics, window.snapping);
            document.release();
            response.request_focus();
        }
        if let Some(at) = response.hover_pos()
            && document.gesture_kind().is_none()
        {
            if window.matte_drag.is_some()
                || matte_point_at(document, map.canvas(at), metrics.tolerance).is_some()
            {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            } else if let Some(target) = document.hit(map.canvas(at), metrics) {
                ui.ctx().set_cursor_icon(cursor_for(target, held.alt));
            }
        }
    } else if response.clicked()
        && let Some(at) = at_pointer
    {
        // Locked: selection still works so values can be read.
        match document.hit(map.canvas(at), metrics) {
            Some(HitTarget::Body(id) | HitTarget::Handle(id, _)) => document.select(id, held.shift),
            None => document.clear_selection(),
        }
    }
    window.canvas_focused = response.has_focus();
    if response.has_focus() {
        // Arrow keys nudge and Esc cancels here instead of moving focus.
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
        canvas_keys(ui, document, look.lock.is_none(), &mut window.feedback);
    }
    let mut hover = l.text(Msg::DesignCanvasHover).to_owned();
    match look.picture_reason {
        None => {
            hover.push('\n');
            hover.push_str(l.text(Msg::DesignBadgeExactHover));
        }
        Some(reason) => {
            hover.push('\n');
            hover.push_str(&l.format(
                Msg::DesignBadgeNoneHover,
                &[("reason", Arg::Str(l.tr(reason)))],
            ));
        }
    }
    let hover = match look.lock {
        None => hover,
        Some(reason) => l10n::disabled_text(l, &hover, reason),
    };
    response.on_hover_text(hover);

    // Paint.
    let painter = ui.painter_at(rect.expand(2.0));
    painter.rect_filled(rect, CornerRadius::ZERO, MONITOR_BLACK);
    if let Some(texture) = look.texture {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    if window.snapping.safe_areas {
        for (share, alpha) in [(ACTION_SAFE, 90), (TITLE_SAFE, 60)] {
            let area = safe_area(canvas, share);
            let corners = [
                map.screen(Point::new(area.left, area.top)),
                map.screen(Point::new(area.right, area.top)),
                map.screen(Point::new(area.right, area.bottom)),
                map.screen(Point::new(area.left, area.bottom)),
                map.screen(Point::new(area.left, area.top)),
            ];
            painter.extend(Shape::dashed_line(
                &corners,
                Stroke::new(1.0, Color32::from_white_alpha(alpha)),
                6.0,
                4.0,
            ));
        }
    }
    // Where changed layers sit on the server.
    for base in document.base() {
        if document
            .layer(base.id)
            .is_some_and(|draft| draft.placement != base.placement)
        {
            let corners: Vec<Pos2> = base
                .placement
                .frame()
                .corners()
                .iter()
                .chain(core::iter::once(&base.placement.frame().corners()[0]))
                .map(|corner| map.screen(*corner))
                .collect();
            painter.extend(Shape::dashed_line(
                &corners,
                Stroke::new(1.0, Color32::from_white_alpha(70)),
                3.0,
                3.0,
            ));
        }
    }
    let single = document.single_selection().map(|layer| layer.id);
    for layer in document.draw_order() {
        paint_layer(
            &painter,
            map,
            canvas,
            layer,
            document.is_selected(layer.id),
            look.texture.is_none(),
            l,
        );
    }
    if let Some(id) = single
        && let Some(layer) = document.layer(id)
    {
        paint_handles(&painter, map, layer, look.lock.is_none());
        paint_garbage_matte(&painter, map, layer, look.lock.is_none());
    } else if document.selection().len() > 1 {
        let mut group: Option<fm_ui_model::scene_designer::geometry::Bounds> = None;
        for id in document.selection() {
            if let Some(layer) = document.layer(*id) {
                let bounds = layer.placement.frame().bounds();
                group = Some(group.map_or(bounds, |group| group.union(bounds)));
            }
        }
        if let Some(group) = group {
            let min = map.screen(Point::new(group.left, group.top));
            let max = map.screen(Point::new(group.right, group.bottom));
            let corners = [min, pos2(max.x, min.y), max, pos2(min.x, max.y), min];
            painter.extend(Shape::dashed_line(
                &corners,
                Stroke::new(1.0, accent_color()),
                5.0,
                3.0,
            ));
        }
    }
    for guide in &window.feedback.guides {
        let (from, to) = match guide.axis {
            Axis::Vertical => (
                map.screen(Point::new(guide.position, 0.0)),
                map.screen(Point::new(guide.position, f64::from(canvas.1))),
            ),
            Axis::Horizontal => (
                map.screen(Point::new(0.0, guide.position)),
                map.screen(Point::new(f64::from(canvas.0), guide.position)),
            ),
        };
        painter.line_segment([from, to], Stroke::new(1.0, GUIDE));
    }
    if let Some(readout) = window.feedback.readout
        && let Some(at) = at_pointer
    {
        let text = readout_text(l, readout);
        let galley = painter.layout_no_wrap(
            text,
            FontId::monospace(crate::theme::legible(11.0)),
            Color32::WHITE,
        );
        let origin = (at + vec2(14.0, 14.0)).min(rect.max - galley.size() - vec2(6.0, 6.0));
        painter.rect_filled(
            Rect::from_min_size(origin - vec2(4.0, 2.0), galley.size() + vec2(8.0, 4.0)),
            CornerRadius::same(3),
            Color32::from_black_alpha(200),
        );
        painter.galley(origin, galley, Color32::WHITE);
    }
    let (badge, tone) = if look.picture_reason.is_none() {
        (Msg::DesignBadgeExact, Tone::Ready)
    } else {
        (Msg::DesignBadgeNone, Tone::Idle)
    };
    let badge_rect = Rect::from_min_size(
        pos2(rect.right() - 158.0, rect.top() + 8.0),
        Vec2::new(150.0, 18.0),
    );
    // The chip's own fill is a faint tint; a dark plate under it keeps the
    // badge legible over a light canvas.
    painter.rect_filled(badge_rect, CornerRadius::same(3), LABEL_BACKING);
    crate::theme::status_chip_painted(&painter, badge_rect, l.text(badge), tone);
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, border_strong_color()),
        egui::StrokeKind::Outside,
    );
}

/// Arrow keys nudge the selection (Shift: 10 px); Esc cancels a drag.
fn canvas_keys(
    ui: &mut Ui,
    document: &mut SceneLayoutDocument,
    editable: bool,
    feedback: &mut GestureFeedback,
) {
    let mut nudge = (0, 0);
    let mut cancel = false;
    ui.input_mut(|input| {
        input.events.retain(|event| match event {
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let step = if modifiers.shift { 10 } else { 1 };
                match key {
                    Key::ArrowLeft => nudge.0 -= step,
                    Key::ArrowRight => nudge.0 += step,
                    Key::ArrowUp => nudge.1 -= step,
                    Key::ArrowDown => nudge.1 += step,
                    Key::Escape if document.gesture_kind().is_some() => cancel = true,
                    _ => return true,
                }
                false
            }
            _ => true,
        });
    });
    if cancel {
        document.cancel();
        *feedback = GestureFeedback::default();
    }
    if editable && nudge != (0, 0) && document.gesture_kind().is_none() {
        document.nudge(nudge.0, nudge.1);
    }
}

fn readout_text(l: Locale, readout: Readout) -> String {
    match readout {
        Readout::Position { x, y } => format!("X {x}  Y {y}"),
        Readout::Size { width, height } => format!("W {width}  H {height}"),
        Readout::Angle { stored, .. } => l.format(
            Msg::DesignReadoutAngle,
            &[("free", Arg::Str(&format!("{stored:.1}")))],
        ),
        Readout::Anchor { x, y } => l.format(
            Msg::DesignReadoutAnchor,
            &[
                ("x", Arg::Str(&format!("{x:.1}"))),
                ("y", Arg::Str(&format!("{y:.1}"))),
            ],
        ),
        Readout::Crop { edge, pixels } => l.format(
            Msg::DesignReadoutCrop,
            &[
                ("edge", Arg::Str(l.text(edge_text(edge)))),
                ("pixels", Arg::from(pixels)),
            ],
        ),
    }
}

/// One layer's outline, name, cropped edges, and mask.
fn paint_layer(
    painter: &egui::Painter,
    map: CanvasMap,
    canvas: (u32, u32),
    layer: &DesignLayer,
    selected: bool,
    fill: bool,
    l: Locale,
) {
    let frame = layer.placement.frame();
    let corners: Vec<Pos2> = frame.corners().iter().map(|c| map.screen(*c)).collect();
    let color = if selected {
        accent_color()
    } else if layer.enabled {
        Color32::from_white_alpha(150)
    } else {
        Color32::from_white_alpha(60)
    };
    let fill_color = if fill {
        Color32::from_rgba_unmultiplied(70, 110, 160, if layer.enabled { 46 } else { 14 })
    } else if selected {
        Color32::from_rgba_unmultiplied(60, 120, 220, 24)
    } else {
        Color32::TRANSPARENT
    };
    painter.add(Shape::convex_polygon(
        corners.clone(),
        fill_color,
        Stroke::new(if selected { 2.0 } else { 1.0 }, color),
    ));
    // Cropped edges in amber.
    let edges = [
        (Edge::Top, 0, 1),
        (Edge::Right, 1, 2),
        (Edge::Bottom, 2, 3),
        (Edge::Left, 3, 0),
    ];
    for (edge, from, to) in edges {
        if layer.crop.get(edge) > 0 {
            painter.line_segment([corners[from], corners[to]], Stroke::new(2.0, AMBER));
        }
    }
    if let Some(mask) = layer.mask {
        let cropped = layer.crop.cropped(canvas);
        let (w, h) = (frame.half_width * 2.0, frame.half_height * 2.0);
        let sx = w / f64::from(cropped.0.max(1));
        let sy = h / f64::from(cropped.1.max(1));
        let local = |x: u32, y: u32| {
            after_flips(
                layer,
                Point::new(
                    f64::from(x).mul_add(sx, -frame.half_width),
                    f64::from(y).mul_add(sy, -frame.half_height),
                ),
            )
        };
        let points: Vec<Pos2> = [
            local(mask.x, mask.y),
            local(mask.x + mask.width, mask.y),
            local(mask.x + mask.width, mask.y + mask.height),
            local(mask.x, mask.y + mask.height),
            local(mask.x, mask.y),
        ]
        .iter()
        .map(|point| map.screen(frame.to_world(*point)))
        .collect();
        painter.extend(Shape::dashed_line(
            &points,
            Stroke::new(1.0, MASK),
            4.0,
            3.0,
        ));
    }
    let top_left = corners
        .iter()
        .copied()
        .fold(pos2(f32::MAX, f32::MAX), Pos2::min);
    // A dark translucent plate under the name keeps it legible over any
    // canvas colour, light or dark.
    let galley = painter.layout_no_wrap(
        crate::theme::ellipsize(&layer_name(l, layer), 24),
        FontId::proportional(crate::theme::legible(10.5)),
        color,
    );
    let origin = top_left + vec2(5.0, 4.0);
    painter.rect_filled(
        Rect::from_min_size(origin - vec2(3.0, 1.0), galley.size() + vec2(6.0, 2.0)),
        CornerRadius::same(2),
        LABEL_BACKING,
    );
    painter.galley(origin, galley, color);
}

/// The garbage matte of `layer`'s key, if any (SW-008).
fn garbage_matte(layer: &DesignLayer) -> Option<fm_types::GarbageMatte> {
    layer.key.and_then(|key| key.refinement().garbage_matte)
}

/// Where `local`, a point over `layer`'s (cropped) box as its key and
/// mask see it, shows once its effects ran: the compositors key and mask a
/// layer before its effects, whose `flip`s mirror the picture about the
/// box's centre. A mirror is its own inverse, so this maps back too.
fn after_flips(layer: &DesignLayer, local: Point) -> Point {
    let (horizontal, vertical) = layer
        .effects
        .iter()
        .filter(|effect| effect.name == "flip")
        .fold((false, false), |(horizontal, vertical), effect| {
            let on = |index: usize| {
                effect
                    .parameters
                    .get(index)
                    .is_some_and(|value| *value != 0)
            };
            (horizontal != on(0), vertical != on(1))
        });
    Point::new(
        if horizontal { -local.x } else { local.x },
        if vertical { -local.y } else { local.y },
    )
}

/// Garbage matte point `point` of `layer` on the canvas: its normalized
/// position over the layer's (cropped) box, mirrored with the layer.
fn matte_point_world(layer: &DesignLayer, point: fm_types::MattePoint) -> Point {
    let frame = layer.placement.frame();
    let unit = f64::from(fm_types::GARBAGE_MATTE_UNIT);
    frame.to_world(after_flips(
        layer,
        Point::new(
            (f64::from(point.x) / unit).mul_add(2.0 * frame.half_width, -frame.half_width),
            (f64::from(point.y) / unit).mul_add(2.0 * frame.half_height, -frame.half_height),
        ),
    ))
}

/// The garbage matte point of the single selected layer within
/// `tolerance` canvas pixels of `at`, if any.
fn matte_point_at(document: &SceneLayoutDocument, at: Point, tolerance: f64) -> Option<u8> {
    let layer = document.single_selection()?;
    let matte = garbage_matte(layer)?;
    matte
        .points()
        .iter()
        .enumerate()
        .map(|(index, point)| (index, matte_point_world(layer, *point).distance(at)))
        .filter(|(_, distance)| *distance <= tolerance)
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .and_then(|(index, _)| u8::try_from(index).ok())
}

/// Moves garbage matte point `point` of the single selected layer under
/// `at` (clamped to the layer's box), within the open step.
fn drag_matte_point(document: &mut SceneLayoutDocument, point: u8, at: Point) {
    let Some(layer) = document.single_selection() else {
        return;
    };
    let frame = layer.placement.frame();
    let local = after_flips(layer, frame.to_local(at));
    let unit = f64::from(fm_types::GARBAGE_MATTE_UNIT);
    let along = |offset: f64, half: f64| {
        if half > 0.0 {
            ((offset + half) / (2.0 * half) * unit).round()
        } else {
            0.0
        }
    };
    let x = along(local.x, frame.half_width);
    let y = along(local.y, frame.half_height);
    document.set_field(Field::MattePointX { point }, x);
    document.set_field(Field::MattePointY { point }, y);
}

/// The selected layer's garbage matte: its outline and numbered points.
fn paint_garbage_matte(
    painter: &egui::Painter,
    map: CanvasMap,
    layer: &DesignLayer,
    editable: bool,
) {
    let Some(matte) = garbage_matte(layer) else {
        return;
    };
    let mut outline: Vec<Pos2> = matte
        .points()
        .iter()
        .map(|point| map.screen(matte_point_world(layer, *point)))
        .collect();
    let first = outline[0];
    outline.push(first);
    painter.extend(Shape::dashed_line(
        &outline,
        Stroke::new(1.5, MATTE),
        5.0,
        3.0,
    ));
    let fill = if editable {
        MATTE
    } else {
        Color32::from_gray(120)
    };
    for (index, at) in outline.iter().take(matte.points().len()).enumerate() {
        painter.circle(*at, HANDLE_SIZE, fill, Stroke::new(1.0, Color32::BLACK));
        painter.text(
            *at + vec2(HANDLE_SIZE + 2.0, -HANDLE_SIZE - 2.0),
            egui::Align2::LEFT_BOTTOM,
            (index + 1).to_string(),
            FontId::proportional(crate::theme::legible(10.0)),
            MATTE,
        );
    }
}

/// The selected layer's scale, crop, and rotate handles and its centre.
fn paint_handles(painter: &egui::Painter, map: CanvasMap, layer: &DesignLayer, editable: bool) {
    let frame = layer.placement.frame();
    let fill = if editable {
        Color32::WHITE
    } else {
        Color32::from_gray(120)
    };
    let stroke = Stroke::new(1.0, accent_color());
    let rotate_offset = map.pixels(ROTATE_OFFSET);
    let top = map.screen(frame.handle_position(Handle::Edge(Edge::Top), rotate_offset));
    let rotate = map.screen(frame.handle_position(Handle::Rotate, rotate_offset));
    painter.line_segment([top, rotate], stroke);
    painter.circle(rotate, HANDLE_SIZE + 0.5, fill, stroke);
    for handle in Handle::ALL {
        if matches!(handle, Handle::Rotate | Handle::Anchor) {
            continue;
        }
        let at = map.screen(frame.handle_position(handle, rotate_offset));
        let size = if matches!(handle, Handle::Corner(_)) {
            HANDLE_SIZE
        } else {
            HANDLE_SIZE - 1.0
        };
        let square = Rect::from_center_size(at, Vec2::splat(size * 2.0));
        painter.rect_filled(square, CornerRadius::same(1), fill);
        painter.rect_stroke(
            square,
            CornerRadius::same(1),
            stroke,
            egui::StrokeKind::Middle,
        );
    }
    // The anchor: the layer turns and zooms about it (Alt-drag moves it).
    let anchor = map.screen(frame.handle_position(Handle::Anchor, rotate_offset));
    let colour = if editable {
        ANCHOR
    } else {
        Color32::from_gray(140)
    };
    painter.circle_stroke(anchor, HANDLE_SIZE + 1.5, Stroke::new(1.5, colour));
    for direction in [vec2(8.0, 0.0), vec2(0.0, 8.0)] {
        painter.line_segment(
            [anchor - direction, anchor + direction],
            Stroke::new(1.5, colour),
        );
    }
}

/// One numeric field: drag to scrub (one undo step per drag), click to
/// type (one step on Enter).
struct Scrub {
    field: Field,
    label: &'static str,
    hover: String,
    range: core::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &'static str,
    /// Shown value per stored value (opacity shows percent).
    shown_per_stored: f64,
}

fn scrub(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    scrub: Scrub,
    blocked: Option<&'static str>,
) {
    ui.label(field_label(scrub.label));
    let stored = document.field_value(id, scrub.field).unwrap_or(0.0);
    let mut value = stored * scrub.shown_per_stored;
    let decimals = match scrub.field {
        Field::Rotation => 3,
        Field::AnchorX | Field::AnchorY => 2,
        field => {
            usize::from((scrub.shown_per_stored - 1.0).abs() > f64::EPSILON || field == Field::Zoom)
        }
    };
    let response = ui
        .add_enabled(
            blocked.is_none(),
            DragValue::new(&mut value)
                .range(scrub.range)
                .speed(scrub.speed)
                .suffix(scrub.suffix)
                .max_decimals(decimals)
                .custom_formatter(move |value, range| {
                    // One-decimal fields read whole numbers without a
                    // trailing `.0` (`30 %`, not `30.0 %`).
                    if decimals == 1 && (value - value.round()).abs() < 0.05 {
                        format!("{value:.0}")
                    } else {
                        egui::emath::format_with_decimals_in_range(value, range)
                    }
                })
                .update_while_editing(false),
        )
        .hint(&scrub.hover, blocked.unwrap_or(""));
    let _ = l;
    if response.drag_started() {
        document.begin(EditLabel::Field(scrub.field));
    }
    if response.changed() {
        document.set_field(scrub.field, value / scrub.shown_per_stored);
    }
    if response.drag_stopped() {
        document.commit();
    }
}

/// The inspector: exact values of the selection.
#[allow(clippy::too_many_lines)] // one inspector: every group of fields
fn inspector(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    key_inputs: &[(InputId, String)],
    sampler: &mut KeySampler<'_>,
    lock: Option<&'static str>,
) {
    ui.label(section_label(l.text(Msg::DesignInspector)))
        .on_hover_text(l.text(Msg::DesignInspectorHover));
    let Some(&id) = document.selection().first() else {
        ui.add(
            egui::Label::new(caption(l.text(Msg::DesignNothingSelected)).color(muted_color()))
                .wrap(),
        );
        return;
    };
    let selected = document.selection().len();
    if selected == 1 {
        if let Some(layer) = document.layer(id) {
            ui.label(
                RichText::new(layer_name(l, layer))
                    .strong()
                    .color(text_color()),
            );
        }
    } else {
        ui.label(
            RichText::new(l.format(Msg::DesignMultiSelected, &[("count", Arg::from(selected))]))
                .strong()
                .color(text_color()),
        );
    }
    let canvas = document.canvas();
    let side = f64::from(fm_ui_model::scene_designer::geometry::MAX_LAYER_SIDE);
    let wide = f64::from(canvas.0.max(canvas.1)) * 4.0;
    let group = |ui: &mut Ui, title: Msg| {
        ui.add_space(6.0);
        ui.label(section_label(l.text(title)));
    };
    group(ui, Msg::DesignGroupPosition);
    egui::Grid::new("studio-designer-position")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::X,
                    label: "X",
                    hover: l.text(Msg::DesignXHover).to_owned(),
                    range: -wide..=wide,
                    speed: 1.0,
                    suffix: " px",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Y,
                    label: "Y",
                    hover: l.text(Msg::DesignYHover).to_owned(),
                    range: -wide..=wide,
                    speed: 1.0,
                    suffix: " px",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            ui.end_row();
        });
    group(ui, Msg::DesignGroupSize);
    egui::Grid::new("studio-designer-size")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Width,
                    label: "W",
                    hover: l.text(Msg::DesignWidthHover).to_owned(),
                    range: 1.0..=side,
                    speed: 1.0,
                    suffix: " px",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Height,
                    label: "H",
                    hover: l.text(Msg::DesignHeightHover).to_owned(),
                    range: 1.0..=side,
                    speed: 1.0,
                    suffix: " px",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            ui.end_row();
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Zoom,
                    label: l.text(Msg::TransitionZoom),
                    hover: l.text(Msg::DesignZoomHover).to_owned(),
                    range: 0.1..=400.0,
                    speed: 0.25,
                    suffix: " %",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.label(section_label(l.text(Msg::AnimPropRotation)))
        .on_hover_text(l.text(Msg::DesignRotationHover));
    egui::Grid::new("studio-designer-rotation")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Rotation,
                    label: l.text(Msg::DesignRotationField),
                    hover: l.text(Msg::DesignRotationHover).to_owned(),
                    range: -360.0..=360.0,
                    speed: 0.25,
                    suffix: " °",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            ui.end_row();
        });
    ui.horizontal(|ui| {
        let current = document.field_value(id, Field::Rotation).unwrap_or(0.0);
        for angle in [0_u16, 90, 180, 270] {
            let on = (current - f64::from(angle)).abs() < 0.0005;
            let response = ui
                .add_enabled(
                    lock.is_none(),
                    egui::Button::selectable(on, format!("{angle}°")),
                )
                .hint(
                    &l.format(
                        Msg::DesignRotationOptionHover,
                        &[("angle", Arg::Plain(u64::from(angle)))],
                    ),
                    lock.unwrap_or(""),
                );
            if response.clicked() {
                document.set_field(Field::Rotation, f64::from(angle));
            }
        }
    });
    group(ui, Msg::DesignGroupLook);
    egui::Grid::new("studio-designer-look")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::Opacity,
                    label: l.text(Msg::AnimPropOpacity),
                    hover: l.text(Msg::DesignOpacityHover).to_owned(),
                    range: 0.0..=100.0,
                    speed: 0.5,
                    suffix: " %",
                    shown_per_stored: 100.0 / 255.0,
                },
                lock,
            );
            let mut visible = document.field_value(id, Field::Enabled).unwrap_or(1.0) >= 0.5;
            // The compositor draws at most 64 layers of a scene: a hidden
            // layer of a full scene cannot be shown.
            let visible_reason = lock.or_else(|| {
                (!document.can_show(id)).then_some(Msg::DesignReasonTooManyVisible.en())
            });
            if ui
                .add_enabled(
                    visible_reason.is_none(),
                    egui::Checkbox::new(&mut visible, l.text(Msg::DesignVisible)),
                )
                .hint(
                    l.text(Msg::DesignVisibleHover),
                    visible_reason.unwrap_or(""),
                )
                .changed()
            {
                document.set_field(Field::Enabled, f64::from(u8::from(visible)));
            }
            ui.end_row();
        });
    group(ui, Msg::DesignGroupCrop);
    egui::Grid::new("studio-designer-crop")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            let edges = [
                (Field::CropLeft, "L", Edge::Left, canvas.0),
                (Field::CropRight, "R", Edge::Right, canvas.0),
                (Field::CropTop, "T", Edge::Top, canvas.1),
                (Field::CropBottom, "B", Edge::Bottom, canvas.1),
            ];
            for (index, (field, label, edge, limit)) in edges.into_iter().enumerate() {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    Scrub {
                        field,
                        label,
                        hover: l.format(
                            Msg::DesignCropHover,
                            &[("edge", Arg::Str(l.text(edge_text(edge))))],
                        ),
                        range: 0.0..=f64::from(limit.saturating_sub(1)),
                        speed: 1.0,
                        suffix: " px",
                        shown_per_stored: 1.0,
                    },
                    lock,
                );
                if index % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    group(ui, Msg::DesignGroupMask);
    let masked = document.field_value(id, Field::Masked).unwrap_or(0.0) >= 0.5;
    ui.horizontal(|ui| {
        let mut on = masked;
        if ui
            .add_enabled(
                lock.is_none(),
                egui::Checkbox::new(&mut on, l.text(Msg::DesignGroupMask)),
            )
            .hint(l.text(Msg::DesignMaskHover), lock.unwrap_or(""))
            .changed()
        {
            document.set_field(Field::Masked, f64::from(u8::from(on)));
        }
        let mut inverted = document.field_value(id, Field::MaskInverted).unwrap_or(0.0) >= 0.5;
        let invert_reason = lock.or_else(|| (!masked).then_some(Msg::DesignReasonNoMask.en()));
        if ui
            .add_enabled(
                invert_reason.is_none(),
                egui::Checkbox::new(&mut inverted, l.text(Msg::DesignMaskInvert)),
            )
            .hint(
                l.text(Msg::DesignMaskInvertHover),
                invert_reason.unwrap_or(""),
            )
            .changed()
        {
            document.set_field(Field::MaskInverted, f64::from(u8::from(inverted)));
        }
    });
    let mask_reason = lock.or_else(|| (!masked).then_some(Msg::DesignReasonNoMask.en()));
    egui::Grid::new("studio-designer-mask")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            let fields = [
                (Field::MaskX, "X", canvas.0),
                (Field::MaskY, "Y", canvas.1),
                (Field::MaskWidth, "W", canvas.0),
                (Field::MaskHeight, "H", canvas.1),
            ];
            for (index, (field, label, limit)) in fields.into_iter().enumerate() {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    Scrub {
                        field,
                        label,
                        hover: l.format(Msg::DesignMaskFieldHover, &[("field", Arg::Str(label))]),
                        range: 0.0..=f64::from(limit),
                        speed: 1.0,
                        suffix: " px",
                        shown_per_stored: 1.0,
                    },
                    mask_reason,
                );
                if index % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    key_section(ui, l, document, id, key_inputs, sampler, lock);
    effects_section(ui, l, document, id, lock);
    group(ui, Msg::DesignGroupStack);
    let position = document.index_of(id).map_or(0, |index| index + 1);
    ui.label(
        RichText::new(l.format(
            Msg::DesignStackPosition,
            &[
                ("position", Arg::from(position)),
                ("count", Arg::from(document.layers().len())),
            ],
        ))
        .color(text_color()),
    )
    .on_hover_text(l.text(Msg::DesignStackHover));
    egui::Grid::new("studio-designer-z-order")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                Scrub {
                    field: Field::ZOrder,
                    label: l.text(Msg::DesignZOrder),
                    hover: l.text(Msg::DesignZOrderHover).to_owned(),
                    range: f64::from(i32::MIN)..=f64::from(i32::MAX),
                    speed: 0.1,
                    suffix: "",
                    shown_per_stored: 1.0,
                },
                lock,
            );
            ui.end_row();
        });
    group(ui, Msg::DesignGroupAnchor);
    ui.label(caption(l.text(Msg::DesignAnchorHover)).color(muted_color()))
        .on_hover_text(l.text(Msg::DesignAnchorHover));
    egui::Grid::new("studio-designer-anchor")
        .num_columns(4)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            for (field, label, hover) in [
                (Field::AnchorX, "X", Msg::DesignAnchorXHover),
                (Field::AnchorY, "Y", Msg::DesignAnchorYHover),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    Scrub {
                        field,
                        label,
                        hover: l.text(hover).to_owned(),
                        range: 0.0..=100.0,
                        speed: 0.25,
                        suffix: " %",
                        shown_per_stored: 0.01,
                    },
                    lock,
                );
            }
            ui.end_row();
        });
    let centred = document.field_value(id, Field::AnchorX) == Some(5_000.0)
        && document.field_value(id, Field::AnchorY) == Some(5_000.0);
    if ui
        .add_enabled(
            lock.is_none() && !centred,
            small_button(l.text(Msg::DesignAnchorCentre)),
        )
        .hint(
            l.text(Msg::DesignAnchorCentreHover),
            lock.unwrap_or(Msg::DesignReasonCentred.en()),
        )
        .clicked()
    {
        document.begin(EditLabel::Anchor);
        document.set_field(Field::AnchorX, 5_000.0);
        document.set_field(Field::AnchorY, 5_000.0);
        document.commit();
    }
}

/// A byte-valued key field (`0..=255`).
fn byte_scrub(field: Field, label: &'static str, hover: String) -> Scrub {
    Scrub {
        field,
        label,
        hover,
        range: 0.0..=255.0,
        speed: 0.5,
        suffix: "",
        shown_per_stored: 1.0,
    }
}

/// The KEY section: no key, a chroma key (colour, similarity, smoothness,
/// spill), a luma key (low, high, softness, invert), or a key/fill key
/// (key input, low, high, softness, invert, key alpha; SW-008), then the
/// matte refinements every key shares.
fn key_section(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    key_inputs: &[(InputId, String)],
    sampler: &mut KeySampler<'_>,
    lock: Option<&'static str>,
) {
    ui.add_space(6.0);
    ui.label(section_label(l.text(Msg::DesignGroupKey)))
        .on_hover_text(l.text(Msg::DesignKeyHover));
    key_kind_picker(ui, l, document, id, key_inputs, lock);
    auto_key_fields(ui, l, document, id, sampler, lock);
    match document.layer(id).and_then(|layer| layer.key) {
        None => return,
        Some(WireLayerKey::Chroma(chroma)) => {
            chroma_fields(
                ui,
                l,
                document,
                id,
                [chroma.red, chroma.green, chroma.blue],
                lock,
            );
        }
        Some(WireLayerKey::Luma(luma)) => {
            luma_fields(ui, l, document, id, luma.invert, lock);
        }
        Some(WireLayerKey::Fill(fill)) => {
            key_input_picker(ui, l, document, fill.key_input, key_inputs, lock);
            luma_fields(ui, l, document, id, fill.invert, lock);
            let mut alpha = fill.alpha_channel;
            if ui
                .add_enabled(
                    lock.is_none(),
                    egui::Checkbox::new(&mut alpha, l.text(Msg::DesignKeyFillAlpha)),
                )
                .hint(l.text(Msg::DesignKeyFillAlphaHover), lock.unwrap_or(""))
                .changed()
            {
                document.set_field(Field::FillAlpha, f64::from(u8::from(alpha)));
            }
        }
    }
    refinement_fields(ui, l, document, id, lock);
}

/// What the KEY section's auto key reads (SW-008): the selected layer's
/// input picture, the latest input thumbnail, shown and picked on; auto
/// key samples the larger input picture the designer asked the server for
/// when one has arrived.
struct KeySampler<'a> {
    state: &'a StudioUiState,
    thumbnails: &'a ThumbnailTextures,
    key_picture: &'a KeyPicturePacer,
    eyedropper: &'a mut bool,
    notice: &'a mut Option<(ToastKind, String)>,
    rects: &'a mut (Option<Rect>, Option<Rect>),
}

impl<'a> KeySampler<'a> {
    /// The larger picture of `source`'s input, once it arrived.
    fn large_picture(&self, source: Option<WireSceneLayerSource>) -> Option<&'a TitlePreviewImage> {
        let pacer: &'a KeyPicturePacer = self.key_picture;
        match source? {
            WireSceneLayerSource::Input(input) => pacer.picture(input.to_domain()),
            WireSceneLayerSource::Scene(_) => None,
        }
    }
}

/// The width the KEY section shows the input picture at.
const KEY_PICTURE_WIDTH: f32 = INSPECTOR_WIDTH - 24.0;

/// Auto key (SW-008): the Auto key button, the Pick colour eyedropper,
/// and the layer input's picture to pick on. Either applies one chroma key
/// derived from the picture as one step ([`EditLabel::AutoKey`]), sent as
/// one `scene_layer_key_set` like any key edit: padlocked, undoable, and
/// journaled on the server.
fn auto_key_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    sampler: &mut KeySampler<'_>,
    lock: Option<&'static str>,
) {
    let source = document.layer(id).map(|layer| layer.source);
    let picture = match source {
        Some(WireSceneLayerSource::Input(input)) => sampler
            .state
            .input_thumbnails
            .get(&input)
            .map(|(image, _)| image)
            .zip(
                sampler
                    .thumbnails
                    .texture(input, &sampler.state.input_thumbnails),
            )
            .ok_or(Msg::DesignReasonNoInputPicture.en()),
        _ => Err(Msg::DesignReasonSceneSource.en()),
    };
    let reason = lock
        .or_else(|| {
            (document.selection().len() > 1).then_some(Msg::DesignReasonAutoKeyOneLayer.en())
        })
        .or_else(|| picture.as_ref().err().copied());
    if reason.is_some() {
        *sampler.eyedropper = false;
    }
    let mut request = None;
    *sampler.rects = (None, None);
    ui.horizontal(|ui| {
        let button = ui
            .add_enabled(reason.is_none(), small_button(l.text(Msg::DesignAutoKey)))
            .hint(l.text(Msg::DesignAutoKeyHover), reason.unwrap_or(""));
        sampler.rects.0 = Some(button.rect);
        if button.clicked() {
            request = Some(None);
        }
        // A key like Auto key beside it, lit in the accent while picking.
        let mut pick = small_button(l.text(Msg::DesignEyedropper));
        if *sampler.eyedropper {
            pick = pick
                .fill(crate::theme::ACCENT_FILL)
                .stroke(Stroke::new(1.0, accent_color()));
        }
        if ui
            .add_enabled(reason.is_none(), pick)
            .hint(l.text(Msg::DesignEyedropperHover), reason.unwrap_or(""))
            .clicked()
        {
            *sampler.eyedropper = !*sampler.eyedropper;
        }
    });
    let Ok((image, texture)) = picture else {
        return;
    };
    let width = KEY_PICTURE_WIDTH;
    let height = width * f32::from(image.height) / f32::from(image.width);
    let picking = *sampler.eyedropper && reason.is_none();
    let sense = if picking {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    sampler.rects.1 = Some(rect);
    ui.painter().image(
        texture,
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    let hover = if picking {
        Msg::DesignKeyPicturePickHover
    } else {
        Msg::DesignKeyPictureHover
    };
    let response = response.on_hover_text(l.text(hover));
    if picking {
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(1.5, accent_color()),
            egui::StrokeKind::Inside,
        );
        let response = response.on_hover_cursor(CursorIcon::Crosshair);
        if response.clicked()
            && let Some(position) = response.interact_pointer_pos()
        {
            request = Some(Some(position));
        }
    }
    // Sample the larger picture of the same input when one arrived; a pick
    // on the shown thumbnail maps onto it by position.
    if let Some(pick) = request {
        let large = sampler.large_picture(source);
        let picture = large.map_or((image.width, image.height, &*image.rgba), |large| {
            (large.width, large.height, &*large.rgba)
        });
        let sample = pick.map_or(AutoKeySample::Dominant, |position| {
            picture_point(rect, position, picture.0, picture.1)
        });
        apply_auto_key(l, document, id, picture, sample, sampler);
    }
}

/// The picture pixel under `position` in `rect`, clamped to the picture.
fn picture_point(rect: Rect, position: Pos2, width: u16, height: u16) -> AutoKeySample {
    let along = |offset: f32, extent: f32, pixels: u16| {
        let fraction = if extent > 0.0 { offset / extent } else { 0.0 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped below
        let pixel = (fraction * f32::from(pixels)).floor().max(0.0) as u32;
        pixel.min(u32::from(pixels).saturating_sub(1))
    };
    AutoKeySample::Point {
        x: along(position.x - rect.min.x, rect.width(), width),
        y: along(position.y - rect.min.y, rect.height(), height),
    }
}

/// Runs the auto key on `picture` (width, height, straight RGBA8) and
/// applies it to layer `id`, leaving a toast that says what it set or why
/// it could not.
fn apply_auto_key(
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    (width, height, rgba): (u16, u16, &[u8]),
    sample: AutoKeySample,
    sampler: &mut KeySampler<'_>,
) {
    let result = AutoKeyPicture::new(u32::from(width), u32::from(height), rgba)
        .and_then(|picture| auto_key(picture, sample));
    *sampler.notice = Some(match result {
        Ok(auto) => {
            document.apply_auto_key(id, &auto);
            *sampler.eyedropper = false;
            let colour = format!("#{:02X}{:02X}{:02X}", auto.red, auto.green, auto.blue);
            (
                ToastKind::Success,
                l.format(
                    Msg::DesignAutoKeyApplied,
                    &[
                        ("colour", Arg::Str(&colour)),
                        ("similarity", Arg::from(u32::from(auto.similarity))),
                        ("smoothness", Arg::from(u32::from(auto.smoothness))),
                    ],
                ),
            )
        }
        Err(AutoKeyError::NoBacking { .. }) => (
            ToastKind::Error,
            l.text(Msg::DesignAutoKeyNoBacking).to_owned(),
        ),
        Err(AutoKeyError::Unsaturated { .. }) => {
            (ToastKind::Error, l.text(Msg::DesignAutoKeyGrey).to_owned())
        }
        Err(_) => (
            ToastKind::Error,
            l.text(Msg::DesignAutoKeyUnreadable).to_owned(),
        ),
    });
}

/// The key kind: none, chroma, luma, or key/fill (which needs an input
/// that can be the key).
fn key_kind_picker(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    key_inputs: &[(InputId, String)],
    lock: Option<&'static str>,
) {
    let kinds = [
        (0_u8, Msg::DesignKeyNone, Msg::DesignKeyNoneHover),
        (1, Msg::DesignKeyChroma, Msg::DesignKeyChromaHover),
        (2, Msg::DesignKeyLuma, Msg::DesignKeyLumaHover),
        (3, Msg::DesignKeyFill, Msg::DesignKeyFillHover),
    ];
    let current = match document.layer(id).and_then(|layer| layer.key) {
        None => 0,
        Some(WireLayerKey::Chroma(_)) => 1,
        Some(WireLayerKey::Luma(_)) => 2,
        Some(WireLayerKey::Fill(_)) => 3,
    };
    let shown = kinds
        .iter()
        .find(|(kind, _, _)| *kind == current)
        .map_or(Msg::DesignKeyNone, |(_, text, _)| *text);
    let mut chosen = None;
    ui.add_enabled_ui(lock.is_none(), |ui| {
        ComboBox::from_id_salt("studio-designer-key-kind")
            .selected_text(l.text(shown))
            .width(INSPECTOR_WIDTH - 24.0)
            .show_ui(ui, |ui| {
                for (kind, text, hover) in kinds {
                    let possible = kind != 3 || !key_inputs.is_empty();
                    if ui
                        .add_enabled(
                            possible,
                            egui::Button::selectable(kind == current, l.text(text)),
                        )
                        .hint(l.text(hover), Msg::DesignReasonNoKeyInput.en())
                        .clicked()
                    {
                        chosen = Some(kind);
                    }
                }
            })
            .response
            .hint(l.text(Msg::DesignKeyKindHover), lock.unwrap_or(""));
    });
    match chosen {
        Some(0) => {
            document.set_key_kind(None);
        }
        Some(1) => {
            document.set_key_kind(Some(true));
        }
        Some(2) => {
            document.set_key_kind(Some(false));
        }
        Some(_) if current != 3 => {
            if let Some((input, _)) = key_inputs.first() {
                document.set_key_fill(WireInputId::from_domain(*input));
            }
        }
        _ => {}
    }
}

/// The key/fill key's key input; an input no longer in the show reads as
/// its id.
fn key_input_picker(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    key_input: WireInputId,
    key_inputs: &[(InputId, String)],
    lock: Option<&'static str>,
) {
    let current = key_input.to_domain();
    let shown = key_inputs
        .iter()
        .find(|(input, _)| *input == current)
        .map_or_else(|| current.to_string(), |(_, name)| name.clone());
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::DesignKeyInput)));
        ui.add_enabled_ui(lock.is_none(), |ui| {
            ComboBox::from_id_salt("studio-designer-key-input")
                .selected_text(shown)
                .width(INSPECTOR_WIDTH - 110.0)
                .show_ui(ui, |ui| {
                    for (input, name) in key_inputs {
                        if ui
                            .selectable_label(*input == current, name)
                            .on_hover_text(l.text(Msg::DesignKeyInputChoiceHover))
                            .clicked()
                        {
                            chosen = Some(*input);
                        }
                    }
                })
                .response
                .hint(l.text(Msg::DesignKeyInputHover), lock.unwrap_or(""));
        });
    });
    if let Some(input) = chosen.filter(|input| *input != current) {
        document.set_key_fill(WireInputId::from_domain(input));
    }
}

/// A chroma key's colour, its presets and components, and its similarity,
/// smoothness, and spill.
#[allow(clippy::too_many_lines)] // one group: colour, components, thresholds
fn chroma_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    colour: [u8; 3],
    lock: Option<&'static str>,
) {
    ui.horizontal(|ui| {
        let mut colour = colour;
        let response = ui
            .add_enabled_ui(lock.is_none(), |ui| {
                egui::color_picker::color_edit_button_srgb(ui, &mut colour)
            })
            .inner
            .hint(l.text(Msg::DesignKeyColourHover), lock.unwrap_or(""));
        if response.changed() {
            // A picker drag is one step: it closes when the
            // pointer is released.
            document.begin(EditLabel::KeyColour);
            document.set_key_colour(colour);
        }
        for (rgb, text, hover) in [
            ([0, 255, 0], Msg::ColourGreen, Msg::DesignKeyGreenHover),
            ([0, 0, 255], Msg::ColourBlue, Msg::DesignKeyBlueHover),
        ] {
            // Keys like Auto key and Pick colour, lit while they match.
            let mut key = small_button(l.text(text));
            if colour == rgb {
                key = key
                    .fill(crate::theme::ACCENT_FILL)
                    .stroke(Stroke::new(1.0, accent_color()));
            }
            if ui
                .add_enabled(lock.is_none(), key)
                .hint(l.text(hover), lock.unwrap_or(""))
                .clicked()
            {
                document.set_key_colour(rgb);
            }
        }
    });
    if document.editing_label() == Some(EditLabel::KeyColour)
        && !ui.ctx().input(|input| input.pointer.any_down())
    {
        document.commit();
    }
    egui::Grid::new("studio-designer-chroma-colour")
        .num_columns(6)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            for (field, label, hover) in [
                (Field::KeyRed, "R", Msg::DesignKeyRedHover),
                (Field::KeyGreen, "G", Msg::DesignKeyGreenChannelHover),
                (Field::KeyBlue, "B", Msg::DesignKeyBlueChannelHover),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    byte_scrub(field, label, l.text(hover).to_owned()),
                    lock,
                );
            }
            ui.end_row();
        });
    egui::Grid::new("studio-designer-chroma")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            for (field, label, hover) in [
                (
                    Field::KeySimilarity,
                    Msg::DesignKeySimilarity,
                    Msg::DesignKeySimilarityHover,
                ),
                (
                    Field::KeySmoothness,
                    Msg::DesignKeySmoothness,
                    Msg::DesignKeySmoothnessHover,
                ),
                (
                    Field::KeySpill,
                    Msg::DesignKeySpill,
                    Msg::DesignKeySpillHover,
                ),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    byte_scrub(field, l.text(label), l.text(hover).to_owned()),
                    lock,
                );
                ui.end_row();
            }
        });
}

/// A luma or key/fill key's thresholds, softness, and invert.
fn luma_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    invert: bool,
    lock: Option<&'static str>,
) {
    egui::Grid::new("studio-designer-luma")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            for (field, label, hover) in [
                (Field::LumaLow, Msg::DesignLumaLow, Msg::DesignLumaLowHover),
                (
                    Field::LumaHigh,
                    Msg::DesignLumaHigh,
                    Msg::DesignLumaHighHover,
                ),
                (
                    Field::LumaSoftness,
                    Msg::DesignLumaSoftness,
                    Msg::DesignLumaSoftnessHover,
                ),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    byte_scrub(field, l.text(label), l.text(hover).to_owned()),
                    lock,
                );
                ui.end_row();
            }
        });
    let mut invert = invert;
    if ui
        .add_enabled(
            lock.is_none(),
            egui::Checkbox::new(&mut invert, l.text(Msg::DesignMaskInvert)),
        )
        .hint(l.text(Msg::DesignLumaInvertHover), lock.unwrap_or(""))
        .changed()
    {
        document.set_field(Field::LumaInvert, f64::from(u8::from(invert)));
    }
}

/// The matte refinements every key shares (SW-008): clip black and white,
/// choke, edge softness, despill with its strength, and light wrap with
/// its width.
fn refinement_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    lock: Option<&'static str>,
) {
    ui.label(field_label(l.text(Msg::DesignKeyMatte)))
        .on_hover_text(l.text(Msg::DesignKeyMatteHover));
    egui::Grid::new("studio-designer-key-matte")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            for (field, label, hover) in [
                (
                    Field::KeyClipBlack,
                    Msg::DesignKeyClipBlack,
                    Msg::DesignKeyClipBlackHover,
                ),
                (
                    Field::KeyClipWhite,
                    Msg::DesignKeyClipWhite,
                    Msg::DesignKeyClipWhiteHover,
                ),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    byte_scrub(field, l.text(label), l.text(hover).to_owned()),
                    lock,
                );
                ui.end_row();
            }
            let choke = f64::from(fm_protocol::MAX_KEY_CHOKE);
            for (field, label, hover, range) in [
                (
                    Field::KeyChoke,
                    Msg::DesignKeyChoke,
                    Msg::DesignKeyChokeHover,
                    -choke..=choke,
                ),
                (
                    Field::KeyEdgeSoftness,
                    Msg::DesignKeyEdgeSoftness,
                    Msg::DesignKeyEdgeSoftnessHover,
                    0.0..=f64::from(fm_protocol::MAX_KEY_EDGE_SOFTNESS),
                ),
            ] {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    Scrub {
                        field,
                        label: l.text(label),
                        hover: l.text(hover).to_owned(),
                        range,
                        speed: 0.05,
                        suffix: " px",
                        shown_per_stored: 1.0,
                    },
                    lock,
                );
                ui.end_row();
            }
        });
    despill_fields(ui, l, document, id, lock);
    light_wrap_fields(ui, l, document, id, lock);
    garbage_matte_fields(ui, l, document, id, lock);
}

/// A key's polygon garbage matte (SW-008): off, keeping its inside, or
/// keeping its outside, and while on its points, each editable here and
/// by dragging it on the canvas.
fn garbage_matte_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    lock: Option<&'static str>,
) {
    let matte = document.layer(id).and_then(garbage_matte);
    let current = match matte {
        None => 0_u8,
        Some(matte) if matte.invert() => 2,
        Some(_) => 1,
    };
    let choices = [
        (0_u8, Msg::DesignMatteOff, Msg::DesignMatteOffHover),
        (1, Msg::DesignMatteInside, Msg::DesignMatteInsideHover),
        (2, Msg::DesignMatteOutside, Msg::DesignMatteOutsideHover),
    ];
    let shown = choices
        .iter()
        .find(|(choice, _, _)| *choice == current)
        .map_or(Msg::DesignMatteOff, |(_, text, _)| *text);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::DesignMatte)));
        ui.add_enabled_ui(lock.is_none(), |ui| {
            ComboBox::from_id_salt("studio-designer-key-garbage-matte")
                .selected_text(l.text(shown))
                .width(INSPECTOR_WIDTH - 110.0)
                .show_ui(ui, |ui| {
                    for (choice, text, hover) in choices {
                        if ui
                            .selectable_label(choice == current, l.text(text))
                            .on_hover_text(l.text(hover))
                            .clicked()
                        {
                            chosen = Some(choice);
                        }
                    }
                })
                .response
                .hint(l.text(Msg::DesignMatteHover), lock.unwrap_or(""));
        });
    });
    if let Some(choice) = chosen.filter(|choice| *choice != current) {
        document.set_field(Field::Matte, f64::from(choice));
    }
    let Some(matte) = matte else {
        return;
    };
    let count = matte.points().len();
    let mut remove = None;
    egui::Grid::new("studio-designer-key-garbage-matte-points")
        .num_columns(4)
        .spacing(Vec2::new(4.0, 4.0))
        .show(ui, |ui| {
            for index in 0..count {
                let point = u8::try_from(index).unwrap_or(u8::MAX);
                let hover = l.format(
                    Msg::DesignMattePointHover,
                    &[("number", Arg::from(u32::from(point) + 1))],
                );
                ui.label(field_label(&(index + 1).to_string()))
                    .on_hover_text(&hover);
                for field in [Field::MattePointX { point }, Field::MattePointY { point }] {
                    matte_coordinate(ui, document, id, field, &hover, lock);
                }
                if ui
                    .add_enabled(
                        lock.is_none() && count > fm_types::MIN_GARBAGE_MATTE_POINTS,
                        small_button("×"),
                    )
                    .hint(l.text(Msg::DesignMatteRemovePointHover), lock.unwrap_or(""))
                    .clicked()
                {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        document.remove_matte_point(index);
    }
    if ui
        .add_enabled(
            lock.is_none() && count < fm_types::MAX_GARBAGE_MATTE_POINTS,
            small_button(l.text(Msg::DesignMatteAddPoint)),
        )
        .hint(l.text(Msg::DesignMatteAddPointHover), lock.unwrap_or(""))
        .clicked()
    {
        document.add_matte_point();
    }
}

/// One garbage matte coordinate as a percentage of the layer: drag to
/// scrub (one undo step per drag), click to type.
fn matte_coordinate(
    ui: &mut Ui,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    field: Field,
    hover: &str,
    lock: Option<&'static str>,
) {
    let per_cent = f64::from(fm_types::GARBAGE_MATTE_UNIT) / 100.0;
    let mut value = document.field_value(id, field).unwrap_or(0.0) / per_cent;
    let response = ui
        .add_enabled(
            lock.is_none(),
            DragValue::new(&mut value)
                .range(0.0..=100.0)
                .speed(0.2)
                .suffix(" %")
                .max_decimals(2)
                .update_while_editing(false),
        )
        .hint(hover, lock.unwrap_or(""));
    if response.drag_started() {
        document.begin(EditLabel::Field(field));
    }
    if response.changed() {
        document.set_field(field, value * per_cent);
    }
    if response.drag_stopped() {
        document.commit();
    }
}

/// A key's light wrap amount and, while on, its width.
fn light_wrap_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    lock: Option<&'static str>,
) {
    let on = document
        .layer(id)
        .and_then(|layer| layer.key)
        .is_some_and(|key| key.refinement().light_wrap != 0);
    egui::Grid::new("studio-designer-key-light-wrap")
        .num_columns(2)
        .spacing(Vec2::new(6.0, 4.0))
        .show(ui, |ui| {
            scrub(
                ui,
                l,
                document,
                id,
                byte_scrub(
                    Field::KeyLightWrap,
                    l.text(Msg::DesignKeyLightWrap),
                    l.text(Msg::DesignKeyLightWrapHover).to_owned(),
                ),
                lock,
            );
            ui.end_row();
            if on {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    Scrub {
                        field: Field::KeyLightWrapWidth,
                        label: l.text(Msg::DesignKeyLightWrapWidth),
                        hover: l.text(Msg::DesignKeyLightWrapWidthHover).to_owned(),
                        range: 1.0..=f64::from(fm_protocol::MAX_KEY_LIGHT_WRAP_WIDTH),
                        speed: 0.05,
                        suffix: " px",
                        shown_per_stored: 1.0,
                    },
                    lock,
                );
                ui.end_row();
            }
        });
}

/// A key's despill (off, green, or blue) and, while on, its strength.
fn despill_fields(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    lock: Option<&'static str>,
) {
    let despill = document
        .layer(id)
        .and_then(|layer| layer.key)
        .map_or(WireKeyDespill::Off, |key| key.refinement().despill);
    let current = match despill {
        WireKeyDespill::Off => 0_u8,
        WireKeyDespill::Green { .. } => 1,
        WireKeyDespill::Blue { .. } => 2,
    };
    let choices = [
        (
            0_u8,
            Msg::DesignKeyDespillOff,
            Msg::DesignKeyDespillOffHover,
        ),
        (1, Msg::ColourGreen, Msg::DesignKeyDespillGreenHover),
        (2, Msg::ColourBlue, Msg::DesignKeyDespillBlueHover),
    ];
    let shown = choices
        .iter()
        .find(|(choice, _, _)| *choice == current)
        .map_or(Msg::DesignKeyDespillOff, |(_, text, _)| *text);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::DesignKeyDespill)));
        ui.add_enabled_ui(lock.is_none(), |ui| {
            ComboBox::from_id_salt("studio-designer-key-despill")
                .selected_text(l.text(shown))
                .width(INSPECTOR_WIDTH - 110.0)
                .show_ui(ui, |ui| {
                    for (choice, text, hover) in choices {
                        if ui
                            .selectable_label(choice == current, l.text(text))
                            .on_hover_text(l.text(hover))
                            .clicked()
                        {
                            chosen = Some(choice);
                        }
                    }
                })
                .response
                .hint(l.text(Msg::DesignKeyDespillHover), lock.unwrap_or(""));
        });
    });
    if let Some(choice) = chosen.filter(|choice| *choice != current) {
        document.set_field(Field::KeyDespill, f64::from(choice));
    }
    if current != 0 {
        egui::Grid::new("studio-designer-key-despill-strength")
            .num_columns(2)
            .spacing(Vec2::new(6.0, 4.0))
            .show(ui, |ui| {
                scrub(
                    ui,
                    l,
                    document,
                    id,
                    byte_scrub(
                        Field::KeyDespillStrength,
                        l.text(Msg::DesignKeyDespillStrength),
                        l.text(Msg::DesignKeyDespillStrengthHover).to_owned(),
                    ),
                    lock,
                );
                ui.end_row();
            });
    }
}

/// A compact effect-row key with a painted `icon`.
fn icon_key(ui: &mut Ui, enabled: bool, icon: KeyIcon, label: &str) -> egui::Response {
    add_icon_button(
        ui,
        enabled,
        egui::Button::new("").min_size(Vec2::new(22.0, 20.0)),
        icon,
        label,
    )
}

/// An effect kind's name and what it does.
fn effect_text(name: &str) -> (Msg, Msg) {
    match name {
        "levels" => (Msg::DesignEffectLevels, Msg::DesignEffectLevelsHover),
        "brightness_contrast" => (
            Msg::DesignEffectBrightnessContrast,
            Msg::DesignEffectBrightnessContrastHover,
        ),
        "saturation" => (Msg::GradeSaturation, Msg::DesignEffectSaturationHover),
        "sharpen" => (Msg::DesignEffectSharpen, Msg::DesignEffectSharpenHover),
        _ => (Msg::DesignEffectFlip, Msg::DesignEffectFlipHover),
    }
}

/// An effect parameter's label, help, shown value per stored value
/// (sharpen's per mille shows as percent), and unit suffix. Every signed
/// strength (brightness, contrast, saturation) and sharpen read in percent;
/// levels read in 8-bit code values.
fn parameter_text(name: &str, parameter: usize) -> (Msg, Msg, f64, &'static str) {
    match (name, parameter) {
        ("levels", 0) => (Msg::ColourBlack, Msg::DesignEffectBlackHover, 1.0, ""),
        ("levels", _) => (Msg::ColourWhite, Msg::DesignEffectWhiteHover, 1.0, ""),
        ("brightness_contrast", 0) => (
            Msg::DesignEffectBrightness,
            Msg::DesignEffectBrightnessHover,
            1.0,
            " %",
        ),
        ("brightness_contrast", _) => (
            Msg::GradeContrast,
            Msg::DesignEffectContrastHover,
            1.0,
            " %",
        ),
        ("saturation", _) => (
            Msg::DesignEffectAmount,
            Msg::DesignEffectSaturationAmountHover,
            1.0,
            " %",
        ),
        ("sharpen", _) => (
            Msg::DesignEffectAmount,
            Msg::DesignEffectSharpenAmountHover,
            0.1,
            " %",
        ),
        (_, 0) => (
            Msg::DesignEffectHorizontal,
            Msg::DesignEffectHorizontalHover,
            1.0,
            "",
        ),
        _ => (
            Msg::DesignEffectVertical,
            Msg::DesignEffectVerticalHover,
            1.0,
            "",
        ),
    }
}

/// The EFFECTS section: the layer's effects in order, each with its
/// parameters, reorder and remove buttons, and an add menu.
#[allow(clippy::too_many_lines)] // one section: the list, then the add menu
fn effects_section(
    ui: &mut Ui,
    l: Locale,
    document: &mut SceneLayoutDocument,
    id: WireSceneLayerId,
    lock: Option<&'static str>,
) {
    ui.add_space(6.0);
    ui.label(section_label(l.text(Msg::DesignGroupEffects)))
        .on_hover_text(l.text(Msg::DesignEffectsHover));
    let reason = lock
        .or_else(|| (!document.effects_shared()).then_some(Msg::DesignReasonEffectsDiffer.en()));
    if lock.is_none()
        && let Some(reason) = reason
    {
        ui.add(egui::Label::new(caption(l.tr(reason)).color(AMBER)).wrap());
    }
    let effects = document
        .layer(id)
        .map(|layer| layer.effects.clone())
        .unwrap_or_default();
    if effects.is_empty() {
        ui.label(caption(l.text(Msg::DesignEffectsEmpty)).color(muted_color()));
    }
    let count = effects.len();
    let mut action: Option<(usize, Option<usize>)> = None;
    for (index, effect) in effects.iter().enumerate() {
        let (name, hover) = effect_text(&effect.name);
        ui.push_id(("studio-designer-effect", index), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}. {}", index + 1, l.text(name))).color(text_color()),
                )
                .on_hover_text(l.text(hover));
                ui.with_layout(l10n::trailing_layout(l), |ui| {
                    if icon_key(
                        ui,
                        reason.is_none(),
                        KeyIcon::Remove,
                        l.text(Msg::KeyRemove),
                    )
                    .hint(l.text(Msg::DesignEffectRemoveHover), reason.unwrap_or(""))
                    .clicked()
                    {
                        action = Some((index, None));
                    }
                    let last = index + 1 == count;
                    if icon_key(
                        ui,
                        reason.is_none() && !last,
                        KeyIcon::Down,
                        l.text(Msg::KeyMoveDown),
                    )
                    .hint(
                        l.text(Msg::DesignEffectDownHover),
                        reason.unwrap_or(Msg::DesignReasonEffectLast.en()),
                    )
                    .clicked()
                    {
                        action = Some((index, Some(index + 1)));
                    }
                    if icon_key(
                        ui,
                        reason.is_none() && index > 0,
                        KeyIcon::Up,
                        l.text(Msg::KeyMoveUp),
                    )
                    .hint(
                        l.text(Msg::DesignEffectUpHover),
                        reason.unwrap_or(Msg::DesignReasonEffectFirst.en()),
                    )
                    .clicked()
                    {
                        action = Some((index, Some(index.saturating_sub(1))));
                    }
                });
            });
            let Ok(effect_index) = u8::try_from(index) else {
                return;
            };
            if effect.name == "flip" {
                ui.horizontal(|ui| {
                    for parameter in 0..effect.parameters.len() {
                        let Ok(parameter_index) = u8::try_from(parameter) else {
                            continue;
                        };
                        let (label, help, _, _) = parameter_text(&effect.name, parameter);
                        let mut on = effect.parameters[parameter] != 0;
                        if ui
                            .add_enabled(
                                reason.is_none(),
                                egui::Checkbox::new(&mut on, l.text(label)),
                            )
                            .hint(l.text(help), reason.unwrap_or(""))
                            .changed()
                        {
                            document.set_field(
                                Field::EffectParameter {
                                    effect: effect_index,
                                    parameter: parameter_index,
                                },
                                f64::from(u8::from(on)),
                            );
                        }
                    }
                });
                return;
            }
            let ranges = effect_ranges(&effect.name).unwrap_or(&[]);
            egui::Grid::new("studio-designer-effect-parameters")
                .num_columns(2)
                .spacing(Vec2::new(6.0, 4.0))
                .show(ui, |ui| {
                    for (parameter, (minimum, maximum)) in ranges.iter().enumerate() {
                        let Ok(parameter_index) = u8::try_from(parameter) else {
                            continue;
                        };
                        let (label, help, shown_per_stored, suffix) =
                            parameter_text(&effect.name, parameter);
                        scrub(
                            ui,
                            l,
                            document,
                            id,
                            Scrub {
                                field: Field::EffectParameter {
                                    effect: effect_index,
                                    parameter: parameter_index,
                                },
                                label: l.text(label),
                                hover: l.text(help).to_owned(),
                                range: f64::from(*minimum) * shown_per_stored
                                    ..=f64::from(*maximum) * shown_per_stored,
                                speed: if shown_per_stored < 1.0 { 0.25 } else { 0.5 },
                                suffix,
                                shown_per_stored,
                            },
                            reason,
                        );
                        ui.end_row();
                    }
                });
        });
    }
    match action {
        Some((index, None)) => {
            document.remove_effect(index);
        }
        Some((from, Some(to))) => {
            document.move_effect(from, to);
        }
        None => {}
    }
    let full = document
        .layer(id)
        .is_some_and(|layer| layer.effects.len() >= MAX_SCENE_LAYER_EFFECTS);
    let add_reason = reason.or_else(|| {
        (full || !document.can_add_effect()).then_some(Msg::DesignReasonEffectsFull.en())
    });
    let mut added = None;
    ui.add_enabled_ui(add_reason.is_none(), |ui| {
        ui.menu_button(l.text(Msg::DesignEffectAdd), |ui| {
            for (name, _) in SCENE_LAYER_EFFECT_KINDS {
                let (text, hover) = effect_text(name);
                if ui
                    .button(l.text(text))
                    .hint(l.text(hover), add_reason.unwrap_or(""))
                    .clicked()
                {
                    added = Some(name);
                    ui.close();
                }
            }
        })
        .response
        .hint(l.text(Msg::DesignEffectAddHover), add_reason.unwrap_or(""));
    });
    if let Some(name) = added {
        document.add_effect(name);
    }
}

#[cfg(test)]
mod tests;
