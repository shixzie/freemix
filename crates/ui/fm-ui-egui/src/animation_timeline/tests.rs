//! Headless `egui` tests: synthetic pointer and key events drive the widget
//! through real passes.

use egui::{Event, Key, PointerButton, Pos2, RawInput, Rect, Ui, UiBuilder, Vec2, vec2};
use fm_animation::{Clip, Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value};
use fm_types::FrameRate;
use fm_ui_model::animation_editor::{
    AnimDocument, EditorMode, TargetBounds, TargetKind, TargetOutline,
};
use fm_ui_model::i18n::{Language, Locale};

use super::*;

const TARGET: u32 = 1;
const FRAME_SECONDS: f64 = 1.0 / 60.0;

fn track(property: Property) -> TrackKey<u32> {
    TrackKey::new(TARGET, Phase::In, property)
}

/// 50 frames of IN at 25 fps: opacity 0 -> 255 and x -100 -> 0 over 0..20.
fn document() -> AnimDocument<u32> {
    let keys = |from: i64, to: i64| {
        vec![
            Keyframe::new(0, Value::Scalar(from), Easing::Linear),
            Keyframe::new(20, Value::Scalar(to), Easing::CubicOut),
        ]
    };
    let clip = Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        50,
        25,
        vec![
            Track::new(track(Property::Opacity), keys(0, 255)),
            Track::new(track(Property::PositionX), keys(-100, 0)),
        ],
    )
    .unwrap();
    let mut document = AnimDocument::new(clip, 1, 1_920, 1_080);
    document.set_targets([TargetOutline::new(
        TARGET,
        "Lower third",
        TargetKind::Shape,
        TargetBounds::new(100, 800, 900, 160),
    )]);
    document
}

struct Harness {
    context: egui::Context,
    document: AnimDocument<u32>,
    state: AnimationTimelineState<u32>,
    time: f64,
    modifiers: egui::Modifiers,
    locale: Option<Locale>,
}

impl Harness {
    fn new(mode: EditorMode) -> Self {
        let mut document = document();
        document.set_mode(mode);
        Self {
            context: egui::Context::default(),
            document,
            state: AnimationTimelineState::default(),
            time: 0.0,
            modifiers: egui::Modifiers::NONE,
            locale: None,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> (TimelineOutput, egui::FullOutput) {
        self.context.begin_pass(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 720.0))),
            time: Some(self.time),
            modifiers: self.modifiers,
            events,
            ..RawInput::default()
        });
        self.time += FRAME_SECONDS;
        let mut ui = Ui::new(
            self.context.clone(),
            egui::Id::new("fm-ui-egui-animation-timeline-test"),
            UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.context.viewport_rect()),
        );
        let mut widget = AnimationTimelineWidget::new(&mut self.document, &mut self.state);
        if let Some(locale) = self.locale {
            widget = widget.locale(locale);
        }
        let output = widget.show(&mut ui);
        let full = self.context.end_pass();
        (output, full)
    }

    fn idle(&mut self) -> TimelineOutput {
        self.frame(Vec::new()).0
    }

    fn press(&mut self, at: Pos2) -> TimelineOutput {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: self.modifiers,
            },
        ])
        .0
    }

    fn move_to(&mut self, at: Pos2) -> TimelineOutput {
        self.frame(vec![Event::PointerMoved(at)]).0
    }

    fn release(&mut self, at: Pos2) -> TimelineOutput {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: self.modifiers,
            },
        ])
        .0
    }

    fn click(&mut self, at: Pos2) -> TimelineOutput {
        self.press(at);
        self.release(at)
    }

    fn key(&mut self, key: Key, modifiers: egui::Modifiers) -> TimelineOutput {
        self.modifiers = modifiers;
        let output = self
            .frame(vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }])
            .0;
        self.modifiers = egui::Modifiers::NONE;
        output
    }

    fn key_position(&self, output: &TimelineOutput, property: Property, frame: u32) -> Pos2 {
        let row = self
            .document
            .rows()
            .iter()
            .position(|row| *row == track(property))
            .unwrap();
        Pos2::new(
            output.tracks.x_of(self.document.view(), frame),
            output.tracks.row_center(row),
        )
    }

    fn rect_of(output: &TimelineOutput, action: TimelineAction) -> Rect {
        output
            .transport
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .map(|(_, rect)| *rect)
            .unwrap()
    }

    fn frames(&self, property: Property) -> Vec<u32> {
        self.document
            .clip()
            .track(track(property))
            .map(|track| track.keys.iter().map(|key| key.frame).collect())
            .unwrap_or_default()
    }
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

#[test]
fn dragging_a_key_moves_it_as_exactly_one_undo_step() {
    let mut harness = Harness::new(EditorMode::Advanced);
    let output = harness.idle();
    let start = harness.key_position(&output, Property::Opacity, 20);
    // Default zoom is four points per frame: +40 points is +10 frames.
    harness.press(start);
    for step in [1.0_f32, 2.0, 3.0, 4.0] {
        let output = harness.move_to(start + vec2(10.0 * step, 0.0));
        assert!(output.changed || harness.document.drag().is_some());
    }
    assert!(harness.document.drag().is_some(), "the drag is live");
    harness.release(start + vec2(40.0, 0.0));
    assert!(harness.document.drag().is_none());
    assert_eq!(harness.frames(Property::Opacity), vec![0, 30]);
    assert_eq!(
        harness.frames(Property::PositionX),
        vec![0, 20],
        "only the pressed key was selected and moved"
    );
    assert_eq!(
        harness.document.undo_depth(),
        1,
        "the whole drag is one step"
    );

    // The press gave the timeline the keyboard: Ctrl+Z undoes the drag.
    assert!(harness.state.keyboard_active);
    harness.key(Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(harness.frames(Property::Opacity), vec![0, 20]);
    assert_eq!(harness.document.undo_depth(), 0);
    harness.key(Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
    assert_eq!(harness.frames(Property::Opacity), vec![0, 30]);
}

#[test]
fn the_mode_toggle_switches_between_simple_and_advanced() {
    let mut harness = Harness::new(EditorMode::Simple);
    let output = harness.idle();
    assert_eq!(output.tracks.rows, 1, "Simple mode: one bar per element");
    let toggle = Harness::rect_of(&output, TimelineAction::ToggleMode);
    let output = harness.click(toggle.center());
    assert_eq!(harness.document.mode(), EditorMode::Advanced);
    assert!(output.actions.contains(&TimelineAction::ToggleMode));
    let output = harness.idle();
    assert_eq!(output.tracks.rows, 2, "Advanced mode: one row per property");
    // M toggles back while the timeline owns the keyboard.
    assert!(harness.state.keyboard_active);
    harness.key(Key::M, egui::Modifiers::NONE);
    assert_eq!(harness.document.mode(), EditorMode::Simple);
}

#[test]
fn space_toggles_playback_once_the_timeline_owns_the_keyboard() {
    let mut harness = Harness::new(EditorMode::Advanced);
    let output = harness.idle();
    harness.key(Key::Space, egui::Modifiers::NONE);
    assert!(
        !harness.document.transport().is_playing(),
        "space belongs to the rest of the console until the timeline is clicked"
    );
    // Clicking the ruler parks the playhead at frame 0 and takes the keyboard.
    let ruler = Pos2::new(
        output.tracks.x_of(harness.document.view(), 0) + 1.0,
        output.tracks.ruler.center().y,
    );
    harness.click(ruler);
    assert!(harness.state.keyboard_active);
    let output = harness.key(Key::Space, egui::Modifiers::NONE);
    assert!(harness.document.transport().is_playing());
    assert!(output.actions.contains(&TimelineAction::PlayPause));
    for _ in 0..30 {
        harness.idle();
    }
    assert!(
        harness.document.playhead() > 0,
        "playback advances with the clock"
    );
    harness.key(Key::Space, egui::Modifiers::NONE);
    assert!(!harness.document.transport().is_playing());
}

#[test]
fn every_transport_button_has_hover_text_naming_its_shortcut() {
    for language in Language::ALL {
        transport_hover_names_shortcuts(Locale::new(language));
    }
}

fn transport_hover_names_shortcuts(locale: Locale) {
    let mut harness = Harness::new(EditorMode::Advanced);
    harness.locale = Some(locale);
    harness.idle();
    // Show every tooltip at once so one pass paints them all.
    harness
        .context
        .memory_mut(|memory| memory.set_everything_is_visible(true));
    // Tooltip areas size themselves invisibly on their first pass.
    harness.frame(Vec::new());
    let (output, full) = harness.frame(Vec::new());
    let painted = painted_text(&full);
    assert_eq!(output.transport.len(), TimelineAction::ALL.len());
    for action in TimelineAction::ALL {
        let spec = action_spec(action, &harness.document, &harness.state, locale);
        let hover = spec.hover(action);
        assert!(
            hover.contains(&format!("[{}]", action.shortcut(locale))),
            "{action:?}: {hover}"
        );
        assert!(
            painted.iter().any(|text| text.contains(&hover)),
            "{action:?} hover {hover:?} was not shown"
        );
        if let Some(reason) = spec.disabled_reason {
            assert!(
                painted
                    .iter()
                    .any(|text| text.contains(&hover) && text.contains(reason)),
                "{action:?} disabled reason {reason:?} was not shown"
            );
        }
    }
    // Nothing is selected or copied yet, so these explain why they are off.
    for action in [
        TimelineAction::Delete,
        TimelineAction::Paste,
        TimelineAction::Undo,
        TimelineAction::KeyAtPlayhead,
    ] {
        assert!(
            action_spec(action, &harness.document, &harness.state, locale)
                .disabled_reason
                .is_some(),
            "{action:?}"
        );
    }
}

#[test]
fn stretching_an_element_bar_retimes_it_in_one_step() {
    let mut harness = Harness::new(EditorMode::Simple);
    let output = harness.idle();
    let view = *harness.document.view();
    let end = Pos2::new(output.tracks.x_of(&view, 20), output.tracks.row_center(0));
    harness.press(end);
    for step in [1.0_f32, 2.0, 3.0, 4.0] {
        harness.move_to(end + vec2(20.0 * step, 0.0));
    }
    harness.release(end + vec2(80.0, 0.0));
    // 0..20 stretched to 0..40 on both properties.
    assert_eq!(harness.frames(Property::Opacity), vec![0, 40]);
    assert_eq!(harness.frames(Property::PositionX), vec![0, 40]);
    assert_eq!(harness.document.undo_depth(), 1);
}

#[test]
fn refused_actions_show_an_inline_message_from_the_core_error_id() {
    let mut harness = Harness::new(EditorMode::Advanced);
    let error = perform(
        TimelineAction::Paste,
        &mut harness.document,
        &mut harness.state,
        false,
    )
    .unwrap_err();
    harness.state.show_error(&error);
    let (_, full) = harness.frame(Vec::new());
    let notice = harness.state.notice().unwrap();
    assert_eq!(notice.msg_key, Some("anim-editor-empty-clipboard"));
    assert!(painted_text(&full).contains(&notice.text));

    // A shortcut for a disabled control explains itself instead.
    harness.state.keyboard_active = true;
    harness.key(Key::Delete, egui::Modifiers::NONE);
    let notice = harness.state.notice().unwrap();
    assert_eq!(notice.msg_key, None);
    assert!(
        notice.text.starts_with("No keys are selected"),
        "{}",
        notice.text
    );
}

#[test]
fn clicking_a_row_makes_it_active_and_k_keys_it_at_the_playhead() {
    let mut harness = Harness::new(EditorMode::Advanced);
    let output = harness.idle();
    let row = harness.key_position(&output, Property::PositionX, 10);
    harness.click(row);
    assert_eq!(harness.state.active_row, Some(track(Property::PositionX)));
    harness.document.set_playhead(10);
    harness.key(Key::K, egui::Modifiers::NONE);
    assert_eq!(harness.frames(Property::PositionX), vec![0, 10, 20]);
    assert_eq!(harness.document.undo_depth(), 1);
    // Arrows nudge the new (selected) key; Delete removes it.
    harness.key(Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(harness.frames(Property::PositionX), vec![0, 11, 20]);
    harness.key(Key::Delete, egui::Modifiers::NONE);
    assert_eq!(harness.frames(Property::PositionX), vec![0, 20]);
}

#[test]
fn ctrl_wheel_zooms_about_the_pointer() {
    let mut harness = Harness::new(EditorMode::Advanced);
    let output = harness.idle();
    let before = harness.document.view().frames_per_pixel();
    let at = Pos2::new(
        output.tracks.track_left + 200.0,
        output.tracks.row_center(0),
    );
    harness.move_to(at);
    harness.frame(vec![Event::Zoom(2.0)]);
    let after = harness.document.view().frames_per_pixel();
    assert!(after < before, "zoomed in: {before} -> {after}");
}

#[test]
fn a_pinned_locale_words_the_whole_timeline_and_its_notices() {
    let spanish = Locale::new(Language::Spanish);
    let mut harness = Harness::new(EditorMode::Advanced);
    harness.locale = Some(spanish);
    let (output, full) = harness.frame(Vec::new());
    let painted = painted_text(&full);
    assert!(
        painted.iter().any(|text| text == "SUAVIZADO"),
        "{painted:?}"
    );
    assert!(painted.iter().any(|text| text.starts_with("ENTRADA")));
    assert!(!painted.iter().any(|text| text == "EASING"));
    assert_eq!(harness.state.locale(), spanish);

    // Refusals and shortcut hints read in the pinned language too.
    let error = perform(
        TimelineAction::Paste,
        &mut harness.document,
        &mut harness.state,
        false,
    )
    .unwrap_err();
    harness.state.show_error(&error);
    assert_eq!(
        harness.state.notice().unwrap().text,
        "Aún no hay nada copiado: copie claves primero."
    );
    harness.state.keyboard_active = true;
    harness.key(Key::Delete, egui::Modifiers::NONE);
    assert!(
        harness
            .state
            .notice()
            .unwrap()
            .text
            .starts_with("No hay claves seleccionadas"),
        "{:?}",
        harness.state.notice()
    );
    assert_eq!(output.transport.len(), TimelineAction::ALL.len());
}

#[test]
fn elements_without_an_outline_are_numbered_and_keep_their_active_row() {
    let mut harness = Harness::new(EditorMode::Advanced);
    harness.document.set_targets([]);
    let output = harness.idle();
    let row = harness.key_position(&output, Property::PositionX, 10);
    harness.click(row);
    let (_, full) = harness.frame(Vec::new());
    assert_eq!(harness.state.active_row, Some(track(Property::PositionX)));
    assert!(
        painted_text(&full).iter().any(|text| text == "Element 1"),
        "{:?}",
        painted_text(&full)
    );
}

#[test]
fn resting_on_a_preset_previews_it_and_a_click_applies_it_as_one_step() {
    use fm_animation::AnimationPreset;

    let mut harness = Harness::new(EditorMode::Simple);
    // An unanimated logo next to the animated lower third.
    harness.document.set_targets([
        TargetOutline::new(
            TARGET,
            "Lower third",
            TargetKind::Shape,
            TargetBounds::new(100, 800, 900, 160),
        ),
        TargetOutline::new(
            2,
            "Logo",
            TargetKind::Image,
            TargetBounds::new(1_700, 60, 160, 160),
        ),
    ]);
    let output = harness.idle();
    assert_eq!(harness.state.preset_hover, None);
    // Open the logo's preset picker.
    let label = output.tracks.label_rect(1);
    let picker = Pos2::new(label.right() - 50.0, label.center().y);
    harness.click(picker);
    harness.idle();
    // Move down the open list until the pointer rests on a preset.
    let mut hovered = None;
    for step in 1..120_u16 {
        let at = Pos2::new(picker.x, picker.y + 4.0 * f32::from(step));
        harness.move_to(at);
        harness.idle();
        if let Some(hover) = harness.state.preset_hover {
            hovered = Some((at, hover));
            break;
        }
    }
    let (at, hover) = hovered.expect("a preset item is hovered");
    assert_eq!(hover.target, 2);
    assert_eq!(hover.phase, Phase::In);
    assert_eq!(hover.frames, 50, "the phase length");
    // Hovering changes nothing.
    assert_eq!(harness.document.undo_depth(), 0);
    assert!(!harness.document.is_dirty());
    // A click applies the hovered preset as one undo step, and ends the
    // hover.
    harness.click(at);
    harness.idle();
    assert_eq!(harness.document.undo_depth(), 1);
    assert!(
        harness
            .document
            .clip()
            .tracks()
            .iter()
            .any(|track| track.key.target == 2),
        "{:?} applied to the logo",
        hover.preset
    );
    assert!(AnimationPreset::ALL.contains(&hover.preset));
    assert_eq!(harness.state.preset_hover, None);
    harness.document.undo().unwrap();
    assert!(
        harness
            .document
            .clip()
            .tracks()
            .iter()
            .all(|track| track.key.target != 2)
    );
}

// ----- custom curve editor --------------------------------------------------

/// Advanced mode with opacity's first key (linear, starting the 0..20
/// segment) selected and opacity the active row.
fn curve_harness() -> Harness {
    let mut harness = Harness::new(EditorMode::Advanced);
    harness
        .document
        .set_selection(&fm_animation::KeySelection::from_iter([
            fm_animation::KeyRef {
                track: track(Property::Opacity),
                frame: 0,
            },
        ]));
    harness.state.active_row = Some(track(Property::Opacity));
    harness.idle();
    harness
}

fn opacity_easing(harness: &Harness, frame: u32) -> Easing {
    harness
        .document
        .clip()
        .key(track(Property::Opacity), frame)
        .unwrap()
        .easing
}

fn controls(easing: Easing) -> [f64; 4] {
    match easing {
        Easing::CubicBezier(curve) => fm_ui_model::animation_editor::curve::unit_controls(curve),
        other => panic!("not a custom curve: {other:?}"),
    }
}

/// Drags from `from` to `to` in a few steps, holding `modifiers`.
fn drag(harness: &mut Harness, from: Pos2, to: Pos2, modifiers: egui::Modifiers) {
    harness.modifiers = modifiers;
    harness.press(from);
    for step in 1..=4 {
        #[allow(clippy::cast_precision_loss)]
        let t = step as f32 / 4.0;
        harness.move_to(from + (to - from) * t);
    }
    harness.release(to);
    harness.modifiers = egui::Modifiers::NONE;
    harness.idle();
}

/// Turns the selection's easing into a custom curve through the picker's
/// "Custom curve…" item.
fn open_custom_curve(harness: &mut Harness) {
    let picker = harness.state.curve_layout().easing_picker.unwrap();
    harness.click(picker.center());
    harness.idle();
    let item = harness
        .state
        .curve_layout()
        .custom_item
        .expect("the picker lists Custom curve…");
    harness.click(item.center());
    harness.idle();
}

#[test]
fn custom_curve_opens_the_editor_and_a_handle_drag_is_one_undo_step() {
    let mut harness = curve_harness();
    assert!(
        harness.state.curve_layout().editor.is_none(),
        "linear: no editor"
    );
    open_custom_curve(&mut harness);
    assert_eq!(harness.document.undo_depth(), 1);
    // Linear converts to its bezier form: handles on the diagonal at thirds.
    let seeded = controls(opacity_easing(&harness, 0));
    assert!((seeded[0] - 1.0 / 3.0).abs() < 1e-4 && (seeded[3] - 2.0 / 3.0).abs() < 1e-4);
    let frame = harness
        .state
        .curve_layout()
        .editor
        .expect("the editor shows for a shared custom curve");

    // Drag the outgoing handle to (0.5, 1.5): snapped onto the overshoot
    // mark even from slightly off.
    let from = frame.to_screen([seeded[0], seeded[1]]);
    let to = frame.to_screen([0.51, 1.49]);
    drag(&mut harness, from, to, egui::Modifiers::NONE);
    assert!(harness.document.drag().is_none());
    let dragged = controls(opacity_easing(&harness, 0));
    assert!((dragged[0] - 0.5).abs() < 1e-4, "{dragged:?}");
    assert!((dragged[1] - 1.5).abs() < 1e-4, "{dragged:?}");
    assert_eq!(harness.document.undo_depth(), 2, "one step per drag");

    // Shift disables snapping.
    let frame = harness.state.curve_layout().editor.unwrap();
    let from = frame.to_screen([dragged[2], dragged[3]]);
    let to = frame.to_screen([0.81, 0.98]);
    drag(&mut harness, from, to, egui::Modifiers::SHIFT);
    let free = controls(opacity_easing(&harness, 0));
    assert!(
        (free[2] - 0.81).abs() < 0.02 && (free[2] - 0.75).abs() > 0.03,
        "{free:?}"
    );
    assert_eq!(harness.document.undo_depth(), 3);
    harness.document.undo().unwrap();
    assert_eq!(controls(opacity_easing(&harness, 0)), dragged);
}

#[test]
fn a_start_shape_replaces_the_curve_in_one_step() {
    let mut harness = curve_harness();
    open_custom_curve(&mut harness);
    let back = harness
        .state
        .curve_layout()
        .presets
        .iter()
        .find(|(preset, _)| *preset == fm_ui_model::animation_editor::CurvePreset::Back)
        .map(|(_, rect)| *rect)
        .unwrap();
    harness.click(back.center());
    harness.idle();
    let shaped = controls(opacity_easing(&harness, 0));
    assert!((shaped[1] - 1.56).abs() < 1e-4, "{shaped:?}");
    assert_eq!(harness.document.undo_depth(), 2);
}

#[test]
fn typing_a_curve_number_commits_one_step() {
    let mut harness = curve_harness();
    open_custom_curve(&mut harness);
    let field = harness
        .state
        .curve_layout()
        .fields
        .iter()
        .find(|(control, _)| *control == fm_ui_model::animation_editor::CurveControl::Y1)
        .map(|(_, rect)| *rect)
        .unwrap();
    harness.click(field.center());
    harness.frame(vec![
        Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        },
        Event::Text("1.25".to_owned()),
    ]);
    assert_eq!(
        harness.document.undo_depth(),
        1,
        "nothing is applied while typing"
    );
    harness.key(Key::Enter, egui::Modifiers::NONE);
    harness.idle();
    let typed = controls(opacity_easing(&harness, 0));
    assert!((typed[1] - 1.25).abs() < 1e-4, "{typed:?}");
    assert_eq!(harness.document.undo_depth(), 2);
}

#[test]
fn dragging_a_graph_handle_shapes_the_outgoing_key_in_one_step() {
    let mut harness = curve_harness();
    let handles = harness.state.curve_layout().graph_handles.clone();
    assert_eq!(handles.len(), 2, "both handles of the selected segment");
    let (frame, _, out) = handles
        .iter()
        .copied()
        .find(|(_, handle, _)| *handle == fm_ui_model::animation_editor::CurveHandle::Out)
        .unwrap();
    assert_eq!(frame, 0);
    // Further right (more influence) and lower (slower start).
    drag(
        &mut harness,
        out,
        out + vec2(15.0, 12.0),
        egui::Modifiers::SHIFT,
    );
    let shaped = controls(opacity_easing(&harness, 0));
    assert!(shaped[0] > 0.4, "{shaped:?}");
    assert!(shaped[1] < 1.0 / 3.0, "{shaped:?}");
    assert_eq!(harness.document.undo_depth(), 1, "one step per drag");
    assert_eq!(
        opacity_easing(&harness, 20),
        Easing::CubicOut,
        "only the outgoing key changes"
    );
    // A custom curve opens the inspector's editor too.
    assert!(harness.state.curve_layout().editor.is_some());
    harness.document.undo().unwrap();
    assert_eq!(opacity_easing(&harness, 0), Easing::Linear);
}

#[test]
fn clicking_a_graph_segment_selects_its_first_key() {
    let mut harness = Harness::new(EditorMode::Advanced);
    harness.state.active_row = Some(track(Property::PositionX));
    let output = harness.idle();
    assert_eq!(harness.state.curve_layout().graph_handles.len(), 0);
    let x = output.tracks.x_of(harness.document.view(), 10);
    let graph_y = harness.state.curve_layout().easing_picker.unwrap().bottom()
        + inspector::CURVE_HEIGHT / 2.0
        + 8.0;
    harness.click(Pos2::new(x, graph_y));
    harness.idle();
    assert!(
        harness
            .document
            .selection()
            .contains(track(Property::PositionX), 0)
    );
    assert_eq!(harness.state.curve_layout().graph_handles.len(), 2);
}

/// Writes `docs/progress/animation-curve-editor.png`: Advanced mode with
/// the lower third's first Position X key shaped by a custom overshooting
/// curve, its editor in the inspector and its handles on the value graph,
/// rendered offscreen from the real widget.
#[test]
#[ignore = "writes docs/progress/animation-curve-editor.png; run with --ignored"]
fn write_animation_curve_editor_screenshot() {
    use crate::offscreen::{Canvas, Textures};
    use fm_ui_model::animation_editor::{CurveTarget, curve::curve_from_unit};

    let keys = |keys: &[(u32, i64)]| {
        keys.iter()
            .map(|&(frame, value)| Keyframe::new(frame, Value::Scalar(value), Easing::Linear))
            .collect::<Vec<_>>()
    };
    let clip = Clip::from_tracks(
        Timebase::new(FrameRate::new(25, 1).unwrap()),
        60,
        25,
        vec![
            Track::new(track(Property::Opacity), keys(&[(0, 0), (15, 255)])),
            Track::new(track(Property::PositionX), keys(&[(0, -400), (36, 0)])),
            Track::new(track(Property::ScaleX), keys(&[(10, 8_000), (40, 10_000)])),
        ],
    )
    .unwrap();
    let mut document = AnimDocument::new(clip, 1, 1_920, 1_080);
    document.set_targets([TargetOutline::new(
        TARGET,
        "Lower third",
        TargetKind::Shape,
        TargetBounds::new(100, 800, 900, 160),
    )]);
    document.set_mode(EditorMode::Advanced);
    let position = fm_animation::KeyRef {
        track: track(Property::PositionX),
        frame: 0,
    };
    document.set_selection(&fm_animation::KeySelection::from_iter([position]));
    document
        .set_curve(
            CurveTarget::Segment(position),
            curve_from_unit([0.3, 1.45, 0.5, 1.0]),
        )
        .unwrap();
    document.set_playhead(14);
    let mut state = AnimationTimelineState {
        active_row: Some(track(Property::PositionX)),
        ..AnimationTimelineState::default()
    };

    let context = egui::Context::default();
    crate::theme::apply_console_visuals(&context, false);
    let size = Vec2::new(1_000.0, 640.0);
    let mut textures = Textures::default();
    let mut last = None;
    for pass in 0..5 {
        context.begin_pass(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            // The preview dot mid-travel on the last pass.
            time: Some(0.4 + f64::from(pass) * 1.6),
            ..RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-animation-curve-screenshot"),
            UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(Rect::from_min_size(
                    Pos2::new(12.0, 12.0),
                    size - Vec2::splat(24.0),
                )),
        );
        let output = AnimationTimelineWidget::new(&mut document, &mut state)
            .locale(Locale::ENGLISH)
            .show(&mut ui);
        if pass == 0 {
            fit_view(&mut document, output.tracks.track_width());
        }
        drop(ui);
        let full = context.end_pass();
        textures.apply(&full);
        last = Some((output.response.rect, full));
    }
    let (rect, full) = last.unwrap();
    let primitives = context.tessellate(full.shapes, full.pixels_per_point);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut canvas = Canvas::new(
        size.x as usize,
        size.y as usize,
        crate::theme::graphite_color(),
    );
    canvas.paint(&primitives, &textures, full.pixels_per_point);
    let (width, height, rgba) = canvas.crop_rgba8(rect.expand(8.0));
    let png = crate::offscreen::encode_png(width, height, &rgba);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/progress/animation-curve-editor.png");
    std::fs::write(&path, png).unwrap();
    println!("wrote {} ({width}x{height})", path.display());
}
