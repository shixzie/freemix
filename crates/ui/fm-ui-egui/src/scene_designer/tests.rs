use core::num::NonZeroU32;

use egui::{Modifiers, PointerButton};
use fm_protocol::{SceneLayerOperation, TitlePreviewImage, WireInputId};
use fm_ui_model::scene_designer::{EditLabel, LIVE_LAYER_EDITS};

use super::*;
use crate::hover_audit::designer_fixture;

/// The fixture's Scene input showing scene 30.
const SCENE_INPUT: u128 = 3;

fn layer(value: u32) -> WireSceneLayerId {
    WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
}

/// Draws one console frame at `time` seconds with `events` and `modifiers`.
fn frame_with(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    time: f64,
    events: Vec<Event>,
    modifiers: Modifiers,
) -> Vec<StudioIntent> {
    context.begin_pass(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_920.0, 1_080.0))),
        time: Some(time),
        events,
        modifiers,
        ..egui::RawInput::default()
    });
    let mut ui = Ui::new(
        context.clone(),
        egui::Id::new("fm-ui-egui-designer-test"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let intents = shell.draw(&mut ui, state);
    let _ = context.end_pass();
    intents
}

/// A console clock and frame driver.
struct Driver {
    context: egui::Context,
    time: f64,
}

impl Driver {
    fn new() -> Self {
        Self {
            context: egui::Context::default(),
            time: 0.0,
        }
    }

    fn frame(
        &mut self,
        shell: &mut StudioShell,
        state: &StudioUiState,
        events: Vec<Event>,
    ) -> Vec<StudioIntent> {
        self.frame_held(shell, state, events, Modifiers::NONE)
    }

    fn frame_held(
        &mut self,
        shell: &mut StudioShell,
        state: &StudioUiState,
        events: Vec<Event>,
        modifiers: Modifiers,
    ) -> Vec<StudioIntent> {
        self.time += 0.05;
        frame_with(&self.context, shell, state, self.time, events, modifiers)
    }

    /// Presses at `from`, moves to `to` in steps, and releases there.
    fn drag(
        &mut self,
        shell: &mut StudioShell,
        state: &StudioUiState,
        from: Pos2,
        to: Pos2,
        modifiers: Modifiers,
    ) -> Vec<StudioIntent> {
        let mut intents = Vec::new();
        intents.extend(self.frame_held(shell, state, vec![Event::PointerMoved(from)], modifiers));
        intents.extend(self.frame_held(
            shell,
            state,
            vec![press(from, true, modifiers)],
            modifiers,
        ));
        for step in 1..=8_u8 {
            let at = from + (to - from) * (f32::from(step) / 8.0);
            intents.extend(self.frame_held(shell, state, vec![Event::PointerMoved(at)], modifiers));
        }
        intents.extend(self.frame_held(shell, state, vec![press(to, false, modifiers)], modifiers));
        intents.extend(self.frame_held(shell, state, Vec::new(), modifiers));
        intents
    }

    fn click(&mut self, shell: &mut StudioShell, state: &StudioUiState, at: Pos2) {
        self.frame(shell, state, vec![Event::PointerMoved(at)]);
        self.frame(shell, state, vec![press(at, true, Modifiers::NONE)]);
        self.frame(shell, state, vec![press(at, false, Modifiers::NONE)]);
        self.frame(shell, state, Vec::new());
    }
}

fn press(at: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    }
}

fn key(key: Key, modifiers: Modifiers) -> Vec<Event> {
    [true, false]
        .into_iter()
        .map(|pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
        .collect()
}

/// The screen position of canvas pixel `(x, y)` last frame.
fn on_canvas(shell: &StudioShell, x: f64, y: f64) -> Pos2 {
    let rect = shell.designer.canvas_rect.expect("the canvas was drawn");
    let canvas = shell.designer.document().unwrap().canvas();
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let scale = f64::from(rect.width()) / f64::from(canvas.0);
    #[allow(clippy::cast_possible_truncation)]
    pos2(
        rect.min.x + (x * scale) as f32,
        rect.min.y + (y * scale) as f32,
    )
}

fn placement(
    shell: &StudioShell,
    id: u32,
) -> fm_ui_model::scene_designer::geometry::LayerPlacement {
    shell
        .designer
        .document()
        .unwrap()
        .layer(layer(id))
        .unwrap()
        .placement
}

fn scene_previews(intents: &[StudioIntent]) -> Vec<fm_protocol::ScenePreviewRequest> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            StudioIntent::ScenePreview(request) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

/// An open designer on the fixture's scene, drawn twice.
fn opened(operator: bool, locked: bool) -> (Driver, StudioShell, StudioUiState, SceneId) {
    let (state, scene) = designer_fixture(operator, locked);
    let mut shell = StudioShell::default();
    shell.open_scene_designer(scene);
    shell.designer.snapping.enabled = false;
    let mut driver = Driver::new();
    driver.frame(&mut shell, &state, Vec::new());
    driver.frame(&mut shell, &state, Vec::new());
    (driver, shell, state, scene)
}

#[test]
fn the_shortcut_opens_the_designer_on_the_first_scene_and_closes_it() {
    let (state, scene) = designer_fixture(true, false);
    let mut shell = StudioShell::default();
    let mut driver = Driver::new();
    driver.frame(&mut shell, &state, Vec::new());
    assert!(!shell.scene_designer_open());
    let chord = Modifiers::COMMAND | Modifiers::SHIFT;
    driver.frame_held(&mut shell, &state, key(Key::L, chord), chord);
    assert!(shell.scene_designer_open());
    driver.frame(&mut shell, &state, Vec::new());
    assert_eq!(shell.scene_designer_scene(), Some(scene));
    let draft = shell.scene_layout_draft(scene).unwrap();
    assert_eq!(draft.layers().len(), 2);
    assert_eq!(draft.canvas(), (1_920, 1_080));
    driver.frame_held(&mut shell, &state, key(Key::L, chord), chord);
    assert!(!shell.scene_designer_open());
    // The draft outlives the window.
    assert!(shell.scene_layout_draft(scene).is_some());
}

#[test]
fn the_scene_input_opens_its_scene() {
    let (state, scene) = designer_fixture(true, false);
    let mut shell = StudioShell::default();
    shell.open_scene_designer_for_input(
        &state,
        InputId::new(core::num::NonZeroU128::new(SCENE_INPUT).unwrap()),
    );
    assert!(shell.scene_designer_open());
    assert_eq!(shell.scene_designer_scene(), Some(scene));
}

#[test]
fn dragging_a_layer_on_the_canvas_moves_it_as_one_draft_step() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    // Host covers the left half; drag its middle 200 px right and 100 down.
    let from = on_canvas(&shell, 480.0, 540.0);
    let to = on_canvas(&shell, 680.0, 640.0);
    driver.drag(&mut shell, &state, from, to, Modifiers::NONE);
    let moved = placement(&shell, 1);
    assert!((moved.x - 200).abs() <= 2, "{moved:?}");
    assert!((moved.y - 370).abs() <= 2, "{moved:?}");
    assert_eq!((moved.width, moved.height), (960, 540));
    let document = shell.designer.document().unwrap();
    assert_eq!(document.selection(), &[layer(1)]);
    assert_eq!(document.undo_label(), Some(EditLabel::Move));
    // Sent live; the draft leads until the server's layout follows.
    assert!(document.is_dirty());
    assert!(!document.has_unsent());
    // The guides and readout clear on release.
    assert!(shell.designer.feedback.readout.is_none());
}

#[test]
fn a_move_snaps_and_draws_guides_until_release() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    shell.designer.snapping.enabled = true;
    driver.frame(&mut shell, &state, Vec::new());
    // Drag Guest's middle (1440, 540) to just off the canvas centre line
    // with its left edge: its left edge lands 3 px right of x 960 - 960.
    let from = on_canvas(&shell, 1_440.0, 540.0);
    let near = on_canvas(&shell, 1_443.0, 540.0);
    driver.frame(&mut shell, &state, vec![Event::PointerMoved(from)]);
    driver.frame(&mut shell, &state, vec![press(from, true, Modifiers::NONE)]);
    for step in 1..=4_u8 {
        let at = from + (near - from) * (f32::from(step) / 4.0) + vec2(0.0, 20.0 * f32::from(step));
        driver.frame(&mut shell, &state, vec![Event::PointerMoved(at)]);
    }
    assert_ne!(shell.designer.feedback.guides.len(), 0);
    assert!(shell.designer.feedback.readout.is_some());
    let last = near + vec2(0.0, 80.0);
    driver.frame(
        &mut shell,
        &state,
        vec![press(last, false, Modifiers::NONE)],
    );
    driver.frame(&mut shell, &state, Vec::new());
    // Its left edge stays on the canvas centre line.
    assert_eq!(placement(&shell, 2).x, 960);
    assert_eq!(shell.designer.feedback.guides.len(), 0);
}

#[test]
fn a_corner_handle_scales_and_shift_keeps_the_aspect() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    let at = on_canvas(&shell, 1_440.0, 540.0);
    driver.click(&mut shell, &state, at);
    assert_eq!(shell.designer.document().unwrap().selection(), &[layer(2)]);
    // Guest's bottom-right corner is at (1920, 810); pull it up and left.
    let corner = on_canvas(&shell, 1_920.0, 810.0);
    let to = on_canvas(&shell, 1_680.0, 700.0);
    driver.drag(&mut shell, &state, corner, to, Modifiers::SHIFT);
    let scaled = placement(&shell, 2);
    assert_eq!((scaled.x, scaled.y), (960, 270));
    // The width ratio (720/960) wins over the height ratio (430/540).
    let ratio = f64::from(scaled.width) / f64::from(scaled.height);
    assert!((ratio - 16.0 / 9.0).abs() < 0.01, "{scaled:?}");
    assert_eq!(
        shell.designer.document().unwrap().undo_label(),
        Some(EditLabel::Scale)
    );
}

#[test]
fn arrow_keys_nudge_the_focused_selection_and_esc_cancels_a_drag() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    let at = on_canvas(&shell, 480.0, 540.0);
    driver.click(&mut shell, &state, at);
    assert!(shell.designer.canvas_focused);
    driver.frame_held(
        &mut shell,
        &state,
        key(Key::ArrowRight, Modifiers::SHIFT),
        Modifiers::SHIFT,
    );
    driver.frame(&mut shell, &state, key(Key::ArrowUp, Modifiers::NONE));
    assert_eq!((placement(&shell, 1).x, placement(&shell, 1).y), (10, 269));
    // Esc during a drag puts the layer back.
    let from = on_canvas(&shell, 480.0, 540.0);
    driver.frame(&mut shell, &state, vec![Event::PointerMoved(from)]);
    driver.frame(&mut shell, &state, vec![press(from, true, Modifiers::NONE)]);
    for step in 1..=3_u8 {
        let at = from + vec2(30.0 * f32::from(step), 0.0);
        driver.frame(&mut shell, &state, vec![Event::PointerMoved(at)]);
    }
    assert!(placement(&shell, 1).x > 10);
    driver.frame(&mut shell, &state, key(Key::Escape, Modifiers::NONE));
    assert_eq!(placement(&shell, 1).x, 10);
    driver.frame(
        &mut shell,
        &state,
        vec![press(from + vec2(90.0, 0.0), false, Modifiers::NONE)],
    );
    assert_eq!(placement(&shell, 1).x, 10);
}

#[test]
fn a_padlock_keeps_selection_but_refuses_every_edit() {
    let (mut driver, mut shell, state, scene) = opened(true, true);
    assert_eq!(
        design_lock_reason(&state, scene),
        Some(PROJECT_LOCKED_REASON)
    );
    let at = on_canvas(&shell, 480.0, 540.0);
    driver.click(&mut shell, &state, at);
    assert_eq!(shell.designer.document().unwrap().selection(), &[layer(1)]);
    let from = on_canvas(&shell, 480.0, 540.0);
    driver.drag(
        &mut shell,
        &state,
        from,
        from + vec2(100.0, 0.0),
        Modifiers::NONE,
    );
    driver.frame(&mut shell, &state, key(Key::ArrowLeft, Modifiers::NONE));
    assert_eq!(placement(&shell, 1).x, 0);
    assert!(!shell.designer.document().unwrap().is_dirty());
}

#[test]
fn a_viewer_is_told_the_permission_it_lacks() {
    let (state, scene) = designer_fixture(false, false);
    assert_eq!(
        design_lock_reason(&state, scene),
        Some(unavailable_reason(&state, Capability::EditProject))
    );
    let (operator, _) = designer_fixture(true, false);
    assert_eq!(design_lock_reason(&operator, scene), None);
}

#[test]
fn dragging_a_list_row_restacks_the_layer() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    // The list shows Guest (top) first, then Host.
    let rows = shell.designer.list_rows.clone();
    assert_eq!(
        rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [layer(2), layer(1)]
    );
    let host = rows[1].1.center();
    let above = rows[0].1.center() - vec2(0.0, 8.0);
    driver.drag(&mut shell, &state, host, above, Modifiers::NONE);
    let document = shell.designer.document().unwrap();
    assert_eq!(document.index_of(layer(1)), Some(1));
    assert_eq!(document.undo_label(), Some(EditLabel::Reorder));
    assert_eq!(
        document.edits(),
        vec![fm_ui_model::scene_designer::SceneLayerEdit::Move { from: 1, to: 0 }]
    );
}

#[test]
fn the_canvas_shows_the_scenes_exact_picture_paced_and_held_during_drags() {
    let (mut driver, mut shell, mut state, _) = opened(true, false);
    // The first frames asked for one picture of Scene input 3.
    let (state_first, _) = designer_fixture(true, false);
    let mut fresh = StudioShell::default();
    fresh.open_scene_designer(shell.scene_designer_scene().unwrap());
    let mut other = Driver::new();
    let intents = other.frame(&mut fresh, &state_first, Vec::new());
    let requests = scene_previews(&intents);
    assert_eq!(requests.len(), 1, "{intents:?}");
    let request = &requests[0];
    assert_eq!(
        request.input,
        WireInputId::from_domain(InputId::new(
            core::num::NonZeroU128::new(SCENE_INPUT).unwrap()
        ))
    );
    assert!(request.request_id.starts_with("design-preview-"));
    assert_eq!((request.width, request.height), (640, 360));
    // Nothing more while that one is unanswered.
    assert_eq!(
        scene_previews(&other.frame(&mut fresh, &state_first, Vec::new())).len(),
        0
    );
    // The answer becomes the canvas picture.
    let mut answered = state_first.clone();
    answered.title_preview = Some(TitlePreviewImage {
        request_id: request.request_id.clone(),
        input: request.input,
        animation_revision: 2,
        draft: false,
        width: 4,
        height: 2,
        rgba: vec![200; 4 * 2 * 4].into(),
    });
    other.frame(&mut fresh, &answered, Vec::new());
    assert!(
        fresh
            .designer
            .texture_for(fresh.scene_designer_scene().unwrap())
            .is_some()
    );
    // A refresh follows a second later, but never during a drag.
    let from = on_canvas(&fresh, 480.0, 540.0);
    other.frame(&mut fresh, &answered, vec![Event::PointerMoved(from)]);
    other.frame(
        &mut fresh,
        &answered,
        vec![press(from, true, Modifiers::NONE)],
    );
    let mut during = Vec::new();
    for step in 1..=30_u8 {
        let at = from + vec2(f32::from(step), 0.0);
        during.extend(other.frame(&mut fresh, &answered, vec![Event::PointerMoved(at)]));
    }
    assert_eq!(scene_previews(&during).len(), 0);
    let released = other.frame(
        &mut fresh,
        &answered,
        vec![press(from + vec2(30.0, 0.0), false, Modifiers::NONE)],
    );
    let mut after = scene_previews(&released);
    after.extend(scene_previews(&other.frame(
        &mut fresh,
        &answered,
        Vec::new(),
    )));
    assert_eq!(after.len(), 1);
    // Previews need a direct TCP session: none without one.
    state.title_previews = false;
    let mut none = Vec::new();
    for _ in 0..40 {
        none.extend(driver.frame(&mut shell, &state, Vec::new()));
    }
    assert_eq!(scene_previews(&none).len(), 0);
}

/// The live scene layer edits among `intents`.
fn layer_edits(intents: &[StudioIntent]) -> Vec<fm_protocol::SceneLayerEdit> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            StudioIntent::SceneLayer(edit) => Some(edit.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_drag_sends_paced_live_edits_that_continue_one_server_undo_step() {
    const { assert!(LIVE_LAYER_EDITS) };
    let (mut driver, mut shell, state, scene) = opened(true, false);
    let from = on_canvas(&shell, 480.0, 540.0);
    let intents = driver.drag(
        &mut shell,
        &state,
        from,
        from + vec2(60.0, 0.0),
        Modifiers::NONE,
    );
    let edits = layer_edits(&intents);
    // The drag's frames are 50 ms apart and sends at most every 80 ms:
    // fewer sends than moves, the last one at the release.
    assert!(edits.len() >= 2 && edits.len() < 9, "{edits:?}");
    assert!(!edits[0].continues);
    assert!(edits[1..].iter().all(|edit| edit.continues));
    for edit in &edits {
        assert_eq!(edit.scene, fm_protocol::WireSceneId::from_domain(scene));
        assert_eq!(edit.layer, layer(1));
        assert!(matches!(
            edit.operation,
            fm_protocol::SceneLayerOperation::SetGeometry(_)
        ));
    }
    let moved = placement(&shell, 1);
    assert_eq!(
        edits.last().unwrap().operation,
        fm_protocol::SceneLayerOperation::SetGeometry(fm_ui_model::scene_designer::wire_geometry(
            moved
        ))
    );
    // Nothing more is sent once the drag is over.
    let idle = driver.frame(&mut shell, &state, Vec::new());
    assert_eq!(layer_edits(&idle).len(), 0);
    assert!(!shell.designer.document().unwrap().has_unsent());

    // A typed value is sent at once, as a new step.
    let document = shell.designer.document_mut().unwrap();
    document.select(layer(2), false);
    document.set_field(fm_ui_model::scene_designer::Field::Opacity, 128.0);
    let typed = layer_edits(&driver.frame(&mut shell, &state, Vec::new()));
    assert_eq!(typed.len(), 1);
    assert!(!typed[0].continues);
    assert_eq!(
        typed[0].operation,
        fm_protocol::SceneLayerOperation::SetAppearance {
            enabled: true,
            opacity: 128,
        }
    );

    // The secondary export copies the whole layout as offline commands.
    let script = shell
        .designer
        .document()
        .unwrap()
        .layout_script(PROJECT_PLACEHOLDER);
    assert!(script.starts_with("freemix-cli scene-layer-geometry '<show.freemix>' 30 0 "));
    shell.set_project_path(Some("/shows/evening.freemix".into()));
    assert_eq!(
        shell.designer.project_path.as_deref(),
        Some("/shows/evening.freemix")
    );
}

#[test]
fn a_padlocked_draft_sends_nothing() {
    let (mut driver, mut shell, state, _) = opened(true, true);
    let document = shell.designer.document_mut().unwrap();
    document.select(layer(1), false);
    document.set_field(fm_ui_model::scene_designer::Field::X, 40.0);
    let intents = driver.frame(&mut shell, &state, Vec::new());
    assert_eq!(layer_edits(&intents).len(), 0);
    assert!(!shell.designer.document().unwrap().is_dirty());
}

#[test]
fn closing_mid_drag_sends_the_layer_back() {
    let (mut driver, mut shell, state, _) = opened(true, false);
    let from = on_canvas(&shell, 480.0, 540.0);
    driver.frame(&mut shell, &state, vec![Event::PointerMoved(from)]);
    driver.frame(&mut shell, &state, vec![press(from, true, Modifiers::NONE)]);
    let mut sent = Vec::new();
    for step in 1..=4_u8 {
        let at = from + vec2(20.0 * f32::from(step), 0.0);
        sent.extend(layer_edits(&driver.frame(
            &mut shell,
            &state,
            vec![Event::PointerMoved(at)],
        )));
    }
    assert_ne!(sent.len(), 0);
    let chord = Modifiers::COMMAND | Modifiers::SHIFT;
    let closed = layer_edits(&driver.frame_held(&mut shell, &state, key(Key::L, chord), chord));
    // The cancel continues the gesture back to where it started.
    let last = closed.last().expect("the cancelled drag is put back");
    assert!(last.continues);
    assert_eq!(
        last.operation,
        fm_protocol::SceneLayerOperation::SetGeometry(fm_ui_model::scene_designer::wire_geometry(
            fm_ui_model::scene_designer::geometry::LayerPlacement::new(0, 270, 960, 540, 0)
        ))
    );
}

#[test]
fn closing_cancels_a_running_gesture_and_a_new_layout_rebases_a_clean_draft() {
    let (mut driver, mut shell, state, scene) = opened(true, false);
    let from = on_canvas(&shell, 480.0, 540.0);
    driver.frame(&mut shell, &state, vec![Event::PointerMoved(from)]);
    driver.frame(&mut shell, &state, vec![press(from, true, Modifiers::NONE)]);
    driver.frame(
        &mut shell,
        &state,
        vec![Event::PointerMoved(from + vec2(40.0, 0.0))],
    );
    assert!(shell.designer.document().unwrap().gesture_kind().is_some());
    let chord = Modifiers::COMMAND | Modifiers::SHIFT;
    driver.frame_held(&mut shell, &state, key(Key::L, chord), chord);
    driver.frame(
        &mut shell,
        &state,
        vec![press(from, false, Modifiers::NONE)],
    );
    assert!(!shell.scene_designer_open());
    let draft = shell.scene_layout_draft(scene).unwrap();
    assert!(draft.gesture_kind().is_none());
    assert!(!draft.is_dirty());
}

/// Clicks at `at` over four frames and returns every intent they sent.
fn click_intents(
    driver: &mut Driver,
    shell: &mut StudioShell,
    state: &StudioUiState,
    at: Pos2,
) -> Vec<StudioIntent> {
    let mut intents = driver.frame(shell, state, vec![Event::PointerMoved(at)]);
    intents.extend(driver.frame(shell, state, vec![press(at, true, Modifiers::NONE)]));
    intents.extend(driver.frame(shell, state, vec![press(at, false, Modifiers::NONE)]));
    intents.extend(driver.frame(shell, state, Vec::new()));
    intents
}

/// SW-008: with the host layer's input picture in, Auto key sends one
/// `scene_layer_key_set` with the green screen's exact colour and a green
/// despill; the eyedropper keys the colour clicked on the picture (a red
/// shirt: keyed, not despilled) and turns itself off.
#[test]
fn auto_key_and_the_eyedropper_send_one_key_edit_each() {
    use fm_ui_model::auto_key::{AutoKeyPicture, AutoKeySample, auto_key};
    let (mut state, scene) = designer_fixture(true, false);
    state.input_thumbnails = crate::hover_audit::green_screen_thumbnail(1);
    let mut shell = StudioShell::default();
    shell.open_scene_designer(scene);
    let mut driver = Driver::new();
    driver.frame(&mut shell, &state, Vec::new());
    shell
        .scene_layout_draft_mut(scene)
        .unwrap()
        .select(layer(1), false);
    driver.frame(&mut shell, &state, Vec::new());
    driver.frame(&mut shell, &state, Vec::new());
    let (button, picture) = shell.designer.auto_key_rects;
    let button = button.expect("the Auto key button was drawn");
    assert!(picture.is_some(), "the input picture was drawn");

    let (image, _) = &state.input_thumbnails
        [&WireInputId::from_domain(InputId::new(core::num::NonZeroU128::new(1).unwrap()))];
    let expected = auto_key(
        AutoKeyPicture::new(16, 9, &image.rgba).unwrap(),
        AutoKeySample::Dominant,
    )
    .unwrap();
    let intents = click_intents(&mut driver, &mut shell, &state, button.center());
    let edits = layer_edits(&intents);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].layer, layer(1));
    let SceneLayerOperation::SetKey(Some(WireLayerKey::Chroma(chroma))) = edits[0].operation else {
        panic!("auto key sets a chroma key: {edits:?}");
    };
    assert_eq!((chroma.red, chroma.green, chroma.blue), (30, 190, 60));
    assert_eq!(chroma.similarity, expected.similarity);
    assert_eq!(chroma.refinement.despill, expected.despill);
    assert!(matches!(expected.despill, WireKeyDespill::Green { .. }));
    let document = shell.scene_layout_draft(scene).unwrap();
    assert_eq!(document.undo_label(), Some(EditLabel::AutoKey));

    // The eyedropper: a click on the subject's red shirt keys that colour.
    // The picture sits low in the inspector: scroll it into view first.
    shell.designer.eyedropper = true;
    let over = button.center();
    driver.frame(&mut shell, &state, vec![Event::PointerMoved(over)]);
    driver.frame(
        &mut shell,
        &state,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }],
    );
    for _ in 0..4 {
        driver.frame(&mut shell, &state, Vec::new());
    }
    let picture = shell
        .designer
        .auto_key_rects
        .1
        .expect("the picture is drawn");
    let shirt = pos2(
        picture.min.x + picture.width() * 7.5 / 16.0,
        picture.min.y + picture.height() * 7.5 / 9.0,
    );
    let intents = click_intents(&mut driver, &mut shell, &state, shirt);
    let edits = layer_edits(&intents);
    assert_eq!(edits.len(), 1, "{edits:?}");
    let SceneLayerOperation::SetKey(Some(WireLayerKey::Chroma(chroma))) = edits[0].operation else {
        panic!("the eyedropper sets a chroma key: {edits:?}");
    };
    assert_eq!((chroma.red, chroma.green, chroma.blue), (210, 60, 50));
    assert_eq!(chroma.refinement.despill, WireKeyDespill::Off);
    assert!(
        !shell.designer.eyedropper,
        "a pick turns the eyedropper off"
    );
}

/// A stand-in for the server's composition of the fixture scene as stored:
/// Host (a cool studio wash) on the left half and Guest (a warm wash) on
/// the right, letterboxed in black.
fn stored_picture(width: u16, height: u16) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(usize::from(width) * usize::from(height) * 4);
    for y in 0..height {
        for x in 0..width {
            let inside =
                u32::from(y) * 4 >= u32::from(height) && u32::from(y) * 4 < u32::from(height) * 3;
            #[allow(clippy::cast_possible_truncation)]
            let shade = (u32::from(y) * 90 / u32::from(height)) as u8;
            let pixel = if !inside {
                [8, 9, 11, 255]
            } else if x < width / 2 {
                [30 + shade / 3, 70 + shade / 2, 120 + shade, 255]
            } else {
                [150 + shade, 90 + shade / 2, 40, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

/// Writes `docs/progress/studio-scene-designer.png`: the console with the
/// layer designer over it, rendered offscreen from the real shell.
#[test]
#[ignore = "writes docs/progress/studio-scene-designer.png; run with --ignored"]
fn write_scene_designer_screenshot() {
    use crate::offscreen::{Canvas, Textures};
    use fm_ui_model::scene_designer::Field;
    use fm_ui_model::scene_designer::geometry::Point;
    use fm_ui_model::scene_designer::gesture::{CanvasMetrics, GestureModifiers};

    let (state, scene) = designer_fixture(true, false);
    let mut shell = StudioShell::default();
    shell.set_project_path(Some("demo.freemix".into()));
    shell.open_scene_designer(scene);
    let context = egui::Context::default();
    let size = Vec2::new(1_920.0, 1_080.0);
    let mut textures = Textures::default();
    let mut pass = |shell: &mut StudioShell, state: &StudioUiState, time: f64| {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-designer-screenshot"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        drop(ui);
        let full = context.end_pass();
        textures.apply(&full);
        (intents, full)
    };
    let (intents, _) = pass(&mut shell, &state, 0.1);
    let request = scene_previews(&intents)
        .pop()
        .expect("a picture is asked for");
    let mut answered = state.clone();
    answered.title_preview = Some(TitlePreviewImage {
        request_id: request.request_id.clone(),
        input: request.input,
        animation_revision: 2,
        draft: false,
        width: request.width,
        height: request.height,
        rgba: stored_picture(request.width, request.height).into(),
    });
    pass(&mut shell, &answered, 0.2);
    // The draft: Guest becomes a picture-in-picture in the lower right with
    // its left edge cropped and an inverted mask, Host fills the frame.
    let snapping = shell.designer.snapping;
    let document = shell.scene_layout_draft_mut(scene).unwrap();
    document.select(layer(1), false);
    document.set_field(Field::Y, 0.0);
    document.set_field(Field::Width, 1_920.0);
    document.set_field(Field::Height, 1_080.0);
    document.select(layer(2), false);
    document.set_field(Field::Width, 640.0);
    document.set_field(Field::Height, 360.0);
    document.set_field(Field::X, 1_150.0);
    document.set_field(Field::Y, 660.0);
    document.set_field(Field::CropLeft, 240.0);
    document.set_field(Field::Masked, 1.0);
    // Mid-drag towards the title-safe corner: snap guides showing.
    let metrics = CanvasMetrics {
        tolerance: 20.0,
        rotate_offset: 60.0,
        snap_distance: 16.0,
    };
    let modifiers = GestureModifiers::default();
    document.press(Point::new(1_470.0, 840.0), modifiers, metrics, snapping);
    let document = shell.scene_layout_draft_mut(scene).unwrap();
    let feedback = document.drag(Point::new(1_501.0, 845.0), modifiers);
    assert_ne!(feedback.guides.len(), 0);
    shell.designer.feedback = feedback;
    let mut last = None;
    for step in 0..3 {
        last = Some(pass(&mut shell, &answered, 0.3 + f64::from(step) * 0.1).1);
    }
    let full = last.unwrap();
    let primitives = context.tessellate(full.shapes, full.pixels_per_point);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut canvas = Canvas::new(
        size.x as usize,
        size.y as usize,
        crate::theme::graphite_color(),
    );
    canvas.paint(&primitives, &textures, full.pixels_per_point);
    let (width, height, rgba) = canvas.crop_rgba8(Rect::from_min_size(Pos2::ZERO, size));
    let png = crate::offscreen::encode_png(width, height, &rgba);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/progress/studio-scene-designer.png");
    std::fs::write(&path, png).unwrap();
    println!("wrote {} ({width}x{height})", path.display());
}

#[test]
fn dragging_a_garbage_matte_point_moves_only_that_point_as_one_step() {
    use fm_ui_model::scene_designer::Field;
    let (mut driver, mut shell, state, scene) = opened(true, false);
    let document = shell.scene_layout_draft_mut(scene).unwrap();
    document.select(layer(1), false);
    let _ = document.set_key_kind(Some(true));
    assert!(document.set_field(Field::Matte, 1.0));
    driver.frame(&mut shell, &state, Vec::new());
    driver.frame(&mut shell, &state, Vec::new());
    let before = placement(&shell, 1);
    // Host fills (0, 270)-(960, 810); the default matte's first point sits
    // a tenth in from its top-left corner. Drag it to the layer's centre.
    let from = on_canvas(&shell, 96.0, 324.0);
    let to = on_canvas(&shell, 480.0, 540.0);
    driver.drag(&mut shell, &state, from, to, Modifiers::NONE);
    let document = shell.designer.document().unwrap();
    let matte = document
        .layer(layer(1))
        .and_then(|layer| layer.key)
        .and_then(|key| key.refinement().garbage_matte)
        .expect("the matte stays on");
    let moved = matte.points()[0];
    assert!(moved.x.abs_diff(500) <= 3, "{moved:?}");
    assert!(moved.y.abs_diff(500) <= 3, "{moved:?}");
    assert_eq!(matte.points()[1], fm_types::MattePoint::new(900, 100));
    assert_eq!(placement(&shell, 1), before);
    assert_eq!(
        document.undo_label(),
        Some(EditLabel::Field(Field::MattePointX { point: 0 }))
    );
}

/// SW-008, Protocol 2.97: with a layer selected, the designer asks the
/// server for its input's own picture (never while its canvas preview
/// flies), and Auto key samples that larger picture instead of the
/// thumbnail.
#[test]
fn auto_key_samples_the_larger_input_picture_once_it_arrives() {
    let (mut state, scene) = designer_fixture(true, false);
    state.input_thumbnails = crate::hover_audit::green_screen_thumbnail(1);
    let mut shell = StudioShell::default();
    shell.open_scene_designer(scene);
    let mut driver = Driver::new();
    let mut first = scene_previews(&driver.frame(&mut shell, &state, Vec::new()));
    shell
        .scene_layout_draft_mut(scene)
        .unwrap()
        .select(layer(1), false);
    first.extend(scene_previews(&driver.frame(
        &mut shell,
        &state,
        Vec::new(),
    )));
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].request_id.starts_with("design-preview-"));
    // Nothing else while the canvas preview flies.
    assert_eq!(
        scene_previews(&driver.frame(&mut shell, &state, Vec::new())).len(),
        0
    );
    let mut answered = state.clone();
    answered.title_preview = Some(TitlePreviewImage {
        request_id: first[0].request_id.clone(),
        input: first[0].input,
        animation_revision: 0,
        draft: false,
        width: 4,
        height: 2,
        rgba: vec![200; 4 * 2 * 4].into(),
    });
    let mut requests = Vec::new();
    for _ in 0..4 {
        requests.extend(scene_previews(&driver.frame(
            &mut shell,
            &answered,
            Vec::new(),
        )));
    }
    let host = WireInputId::from_domain(InputId::new(core::num::NonZeroU128::new(1).unwrap()));
    assert_eq!(requests.len(), 1, "{requests:?}");
    let key_request = &requests[0];
    assert!(key_request.request_id.starts_with("design-key-picture-"));
    assert_eq!(key_request.input, host);
    assert_eq!((key_request.width, key_request.height), (640, 360));
    assert!(key_request.clip.is_none());

    // The larger picture: an evenly lit green of another shade.
    let mut picture = answered.clone();
    picture.title_preview = Some(TitlePreviewImage {
        request_id: key_request.request_id.clone(),
        input: host,
        animation_revision: 0,
        draft: false,
        width: 64,
        height: 36,
        rgba: [20_u8, 160, 90, 255].repeat(64 * 36).into(),
    });
    driver.frame(&mut shell, &picture, Vec::new());
    driver.frame(&mut shell, &picture, Vec::new());
    let button = shell
        .designer
        .auto_key_rects
        .0
        .expect("the Auto key button");
    let edits = layer_edits(&click_intents(
        &mut driver,
        &mut shell,
        &picture,
        button.center(),
    ));
    assert_eq!(edits.len(), 1, "{edits:?}");
    let SceneLayerOperation::SetKey(Some(WireLayerKey::Chroma(chroma))) = edits[0].operation else {
        panic!("auto key sets a chroma key: {edits:?}");
    };
    assert_eq!((chroma.red, chroma.green, chroma.blue), (20, 160, 90));
}
